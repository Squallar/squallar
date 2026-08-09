//! Rasterize overlay polygons to RGBA textures using tiny-skia.
//!
//! Runs on a background thread; the texture is then geo-positioned on the map
//! the same way a radar image is.

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use std::sync::Arc;

use crate::glm::GlmFlash;
use crate::nws::alert::{AlertCategory, NwsAlert};
use crate::render::overlay_state::OverlayItem;
use crate::spc::colors::{md_fill_color, md_stroke_color};
use crate::spc::discussion::SpcDiscussion;
use crate::spc::reports::{StormReport, StormReportKind};
use crate::types::{GeoBounds, OverlayFeature};

// ── Hit buffer types ─────────────────────────────────────────────────────

/// Click detection against the pixels the rasterizer actually drew. Stored at
/// 1/4 resolution per axis, and sparsely, to keep memory down.
#[derive(Clone)]
pub struct HitMap {
    /// Quarter-resolution, not texture width.
    pub width: u32,
    pub height: u32,
    /// `qy * width + qx` → covering item IDs. Occupied cells only.
    cells: HashMap<u32, Vec<u32>>,
    id_map: HashMap<u32, Arc<dyn OverlayItem>>,
}

impl HitMap {
    /// Takes *full*-resolution dimensions and quarters them.
    pub fn new(full_width: u32, full_height: u32) -> Self {
        Self {
            width: full_width.div_ceil(4),
            height: full_height.div_ceil(4),
            cells: HashMap::new(),
            id_map: HashMap::new(),
        }
    }

    /// `(px, py)` is in full-resolution pixels.
    pub fn record(&mut self, px: f32, py: f32, item_id: u32) {
        let qx = (px as u32) / 4;
        let qy = (py as u32) / 4;
        if qx >= self.width || qy >= self.height {
            return;
        }
        let idx = qy * self.width + qx;
        let ids = self
            .cells
            .entry(idx)
            .or_insert_with(|| Vec::with_capacity(1));
        if !ids.contains(&item_id) {
            ids.push(item_id);
        }
    }

    pub fn register_id(&mut self, item_id: u32, item: Arc<dyn OverlayItem>) {
        self.id_map.insert(item_id, item);
    }

    /// `(u, v)` are texture UVs in `[0, 1]`.
    pub fn hit_test(&self, u: f32, v: f32) -> Vec<Arc<dyn OverlayItem>> {
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return Vec::new();
        }
        let qx = ((u * self.width as f32) as u32).min(self.width.saturating_sub(1));
        let qy = ((v * self.height as f32) as u32).min(self.height.saturating_sub(1));
        let idx = qy * self.width + qx;
        self.cells
            .get(&idx)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.id_map.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub struct RasterizeOutput {
    /// `width × height × 4` bytes, straight (not premultiplied) alpha.
    pub rgba: Vec<u8>,
    pub hit_map: Option<HitMap>,
}

// ── Mercator projection helpers ──────────────────────────────────────────

#[inline]
fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

/// Web Mercator's own limit; the projection diverges past it.
const MAX_MERCATOR_LAT: f64 = 85.05;

/// Mercator Y for both edges is precomputed once per texture.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MercatorBounds {
    min_lon: f64,
    max_lon: f64,
    merc_y_min: f64, // south edge
    merc_y_max: f64, // north edge
}

impl MercatorBounds {
    pub(crate) fn from_geo(bounds: &GeoBounds) -> Self {
        let clamped_min = bounds.min_lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT);
        let clamped_max = bounds.max_lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT);
        Self {
            min_lon: bounds.min_lon,
            max_lon: bounds.max_lon,
            merc_y_min: lat_rad_to_mercator_y(clamped_min.to_radians()),
            merc_y_max: lat_rad_to_mercator_y(clamped_max.to_radians()),
        }
    }

    /// To texture pixel coordinates.
    #[inline]
    pub(crate) fn project(&self, lat: f64, lon: f64, w: f32, h: f32) -> (f32, f32) {
        let lon_frac = (lon - self.min_lon) / (self.max_lon - self.min_lon);
        let merc_y = lat_rad_to_mercator_y(lat.to_radians());
        let merc_frac = (merc_y - self.merc_y_min) / (self.merc_y_max - self.merc_y_min);
        let px = (lon_frac * w as f64) as f32;
        // Y is inverted: top of texture = north = max Mercator Y.
        let py = ((1.0 - merc_frac) * h as f64) as f32;
        (px, py)
    }
}

// ── Public API ───────────────────────────────────────────────────────────

/// `hatch_color` is theme-dependent, so it cannot live in the feature.
pub fn rasterize_spc_outlooks(
    features: &[OverlayFeature],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    hatch_color: [u8; 4],
) -> Vec<u8> {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_spc_outlooks ({}×{})",
            width,
            height
        );
        return vec![0u8; (width * height * 4) as usize];
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    // Two passes: hatching must go over every fill, including fills drawn by
    // features later in the list.
    for feature in features {
        draw_feature(&mut pixmap, feature, &mb, w, h);
    }
    crate::render::hatch::draw_hatch_pass(&mut pixmap, features, &mb, w, h, hatch_color);

    premultiplied_to_straight(pixmap.data_mut());
    pixmap.take()
}

pub fn rasterize_spc_discussions(
    discussions: &[SpcDiscussion],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_spc_discussions ({}×{})",
            width,
            height
        );
        return vec![0u8; (width * height * 4) as usize];
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    for md in discussions {
        let fill_rgba = md_fill_color(&md.md_type);
        let stroke_rgba = md_stroke_color(&md.md_type);

        for ring in &md.polygon {
            if ring.len() < 3 {
                continue;
            }
            let pts: Vec<(f32, f32)> = ring
                .iter()
                .map(|&(lat, lon)| mb.project(lat, lon, w, h))
                .collect();
            if let Some(path) = build_polygon_path(&pts) {
                fill_path(&mut pixmap, &path, fill_rgba);
                let sw = scaled_stroke_width(&path, 2.0);
                stroke_path(&mut pixmap, &path, stroke_rgba, sw);
            }
        }
    }

    premultiplied_to_straight(pixmap.data_mut());
    pixmap.take()
}

/// Renders only alerts in `enabled_categories` and not in `hidden_ids`.
pub fn rasterize_nws_alerts(
    alerts: &[NwsAlert],
    enabled_categories: &[AlertCategory],
    hidden_ids: &HashSet<String>,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_nws_alerts ({}×{})",
            width,
            height
        );
        return vec![0u8; (width * height * 4) as usize];
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    for alert in alerts {
        if !enabled_categories.contains(&alert.category) || hidden_ids.contains(&alert.id) {
            continue;
        }
        for feature in &alert.features {
            draw_feature(&mut pixmap, feature, &mb, w, h);
        }
    }

    premultiplied_to_straight(pixmap.data_mut());
    pixmap.take()
}

/// Deliberately not `rustdar_radar`'s site type: keeps this crate decoupled.
pub struct RadarSiteInfo {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub is_current: bool,
    pub is_loading: bool,
}

pub fn rasterize_radar_sites(
    sites: &[RadarSiteInfo],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    zoom: f64,
    is_dark: bool,
) -> Vec<u8> {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_radar_sites ({}×{})",
            width,
            height
        );
        return vec![0u8; (width * height * 4) as usize];
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    let zoom_f32 = zoom as f32;
    let radius = ((5.0 + zoom_f32).clamp(4.0, 12.0)).max(1.0);
    let stroke_w = (radius * 0.3).clamp(0.5, 2.0);

    let text_bg = if is_dark {
        Color::from_rgba8(0, 0, 0, 140)
    } else {
        Color::from_rgba8(255, 255, 255, 140)
    };

    for site in sites {
        let (px, py) = mb.project(site.lat, site.lon, w, h);
        // 50 px of slack so a site just off-texture still contributes its label.
        if px < -50.0 || px > w + 50.0 || py < -50.0 || py > h + 50.0 {
            continue;
        }

        let fill = if site.is_loading {
            Color::from_rgba8(160, 32, 240, 255) // purple
        } else if site.is_current {
            Color::from_rgba8(255, 100, 100, 255) // red
        } else {
            Color::from_rgba8(100, 150, 255, 255) // blue
        };

        let mut pb = PathBuilder::new();
        pb.push_circle(px, py, radius);
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(fill);
            paint.anti_alias = true;
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );

            paint.set_color(Color::from_rgba8(255, 255, 255, 255));
            let stroke = Stroke {
                width: stroke_w,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }

        // tiny-skia cannot render text: only the background pill is baked in,
        // and egui draws the label over it per frame.
        if zoom >= 5.0 {
            let label_w = site.name.len() as f32 * 5.5 + 4.0;
            let label_h = 10.0;
            let lx = px - label_w / 2.0;
            let ly = py + radius + 2.0;
            let mut pb = PathBuilder::new();
            if let Some(rect) = tiny_skia::Rect::from_xywh(lx, ly, label_w, label_h) {
                pb.push_rect(rect);
            }
            if let Some(path) = pb.finish() {
                let mut paint = Paint::default();
                paint.set_color(text_bg);
                paint.anti_alias = true;
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
    }

    premultiplied_to_straight(pixmap.data_mut());
    pixmap.take()
}

// ── Storm report symbol helpers ───────────────────────────────────────

/// Tornado: funnel, i.e. an inverted triangle.
fn draw_tornado_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let s = r * 0.6; // symbol half-size
    let mut pb = PathBuilder::new();
    pb.move_to(px - s, py - s * 0.7);
    pb.line_to(px + s, py - s * 0.7);
    pb.line_to(px, py + s * 0.9);
    pb.close();
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Hail: filled core with four radiating ticks.
fn draw_hail_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let core = r * 0.3;
    let tick_inner = r * 0.35;
    let tick_outer = r * 0.65;
    let stroke_w = (r * 0.18).clamp(0.5, 1.5);

    let mut pb = PathBuilder::new();
    pb.push_circle(px, py, core);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: stroke_w,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    let diag = std::f32::consts::FRAC_1_SQRT_2;
    let offsets: [(f32, f32); 4] = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)];
    for (dx, dy) in offsets {
        let (adx, ady) = if dx.abs() > 0.5 {
            (dx, diag * dy.signum().max(0.3))
        } else {
            (diag * dx.signum().max(0.3), dy)
        };
        let _ = ady;
        let _ = adx;
        let mut pb = PathBuilder::new();
        pb.move_to(px + dx * tick_inner, py + dy * tick_inner);
        pb.line_to(px + dx * tick_outer, py + dy * tick_outer);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
}

/// Wind: right-pointing chevron.
fn draw_wind_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let s = r * 0.55;
    let stroke_w = (r * 0.22).clamp(0.5, 2.0);

    let mut pb = PathBuilder::new();
    pb.move_to(px - s * 0.5, py - s);
    pb.line_to(px + s * 0.5, py);
    pb.line_to(px - s * 0.5, py + s);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        let stroke = Stroke {
            width: stroke_w,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Tornado = red, hail = green, wind = blue. Below a 5 px radius the symbols
/// are unreadable, so it falls back to filled dots.
pub fn rasterize_storm_reports(
    reports: &[StormReport],
    items: &[Arc<dyn OverlayItem>],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    zoom: f64,
    is_dark: bool,
) -> RasterizeOutput {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_storm_reports ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_map: None,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    let zoom_f32 = zoom as f32;
    let radius = (3.0 + zoom_f32 * 0.5).clamp(3.0, 10.0);
    let stroke_w = (radius * 0.3).clamp(0.5, 2.0);
    // Includes the stroke, so the outline itself is clickable.
    let hit_radius = radius + stroke_w;

    let outline = if is_dark {
        Color::from_rgba8(255, 255, 255, 220)
    } else {
        Color::from_rgba8(40, 40, 40, 220)
    };

    let mut hit_map = HitMap::new(width, height);

    for (idx, report) in reports.iter().enumerate() {
        let (px, py) = mb.project(report.lat, report.lon, w, h);
        if px < -20.0 || px > w + 20.0 || py < -20.0 || py > h + 20.0 {
            continue;
        }

        let fill = match report.kind {
            StormReportKind::Tornado => Color::from_rgba8(220, 40, 40, 220),
            StormReportKind::Hail => Color::from_rgba8(40, 180, 40, 220),
            StormReportKind::Wind => Color::from_rgba8(40, 80, 220, 220),
        };

        let use_symbol = radius >= 5.0;

        let mut pb = PathBuilder::new();
        pb.push_circle(px, py, radius);
        if let Some(path) = pb.finish() {
            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };

            if use_symbol {
                paint.set_color(fill);
                let stroke = Stroke {
                    width: stroke_w,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);

                match report.kind {
                    StormReportKind::Tornado => {
                        draw_tornado_symbol(&mut pixmap, px, py, radius, fill)
                    }
                    StormReportKind::Hail => draw_hail_symbol(&mut pixmap, px, py, radius, fill),
                    StormReportKind::Wind => draw_wind_symbol(&mut pixmap, px, py, radius, fill),
                }
            } else {
                paint.set_color(fill);
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );

                paint.set_color(outline);
                let stroke = Stroke {
                    width: stroke_w,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }

        let item_id = idx as u32;
        if let Some(item) = items.get(idx) {
            hit_map.register_id(item_id, item.clone());
        }
        let min_x = (px - hit_radius).max(0.0) as i32;
        let max_x = ((px + hit_radius) as i32).min(width as i32 - 1);
        let min_y = (py - hit_radius).max(0.0) as i32;
        let max_y = ((py + hit_radius) as i32).min(height as i32 - 1);
        let r2 = hit_radius * hit_radius;
        // Step 4: one sample per quarter-res cell.
        let mut sy = min_y;
        while sy <= max_y {
            let mut sx = min_x;
            while sx <= max_x {
                let dx = sx as f32 - px;
                let dy = sy as f32 - py;
                if dx * dx + dy * dy <= r2 {
                    hit_map.record(sx as f32, sy as f32, item_id);
                }
                sx += 4;
            }
            sy += 4;
        }
    }

    premultiplied_to_straight(pixmap.data_mut());
    RasterizeOutput {
        rgba: pixmap.take(),
        hit_map: Some(hit_map),
    }
}

// ── GLM Lightning rasterization ──────────────────────────────────────────

fn draw_lightning_bolt(pixmap: &mut Pixmap, cx: f32, cy: f32, size: f32, rgba: [u8; 4]) {
    let s = size * 0.5;
    let mut pb = PathBuilder::new();
    pb.move_to(cx - s * 0.1, cy - s);
    pb.line_to(cx + s * 0.35, cy - s);
    pb.line_to(cx + s * 0.05, cy - s * 0.15);
    pb.line_to(cx + s * 0.35, cy - s * 0.15);
    pb.line_to(cx - s * 0.15, cy + s);
    pb.line_to(cx + s * 0.05, cy + s * 0.15);
    pb.line_to(cx - s * 0.25, cy + s * 0.15);
    pb.close();

    let Some(path) = pb.finish() else { return };
    let paint = tiny_skia::Paint {
        shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(
            rgba[0], rgba[1], rgba[2], rgba[3],
        )),
        anti_alias: true,
        ..Default::default()
    };
    pixmap.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        tiny_skia::Transform::identity(),
        None,
    );
}

/// Age ramp: white → yellow → orange → red, in thirds of `window_secs`.
fn time_decay_color(age_secs: f64, window_secs: f64, is_dark: bool) -> [u8; 4] {
    let t = (age_secs / window_secs).clamp(0.0, 1.0) as f32;
    let (r, g, b) = if t < 0.33 {
        let f = t / 0.33;
        (255, (255.0 - f * 30.0) as u8, (255.0 - f * 200.0) as u8)
    } else if t < 0.66 {
        let f = (t - 0.33) / 0.33;
        (255, (225.0 - f * 90.0) as u8, (55.0 - f * 55.0) as u8)
    } else {
        let f = (t - 0.66) / 0.34;
        ((255.0 - f * 55.0) as u8, (135.0 - f * 85.0) as u8, 0)
    };
    let alpha = if is_dark { 230 } else { 200 };
    [r, g, b, alpha]
}

/// GLM radiant energy → 0…1 bolt-size channel.
///
/// The 1e-16…1e-12 J clamp window covers event and group energies but not all
/// flash energies: about a sixth of flashes in a sampled GOES-West granule
/// exceed 1e-12 J, so the largest share a size. Input must be CF-unpacked —
/// raw packed counts (tens to thousands) clamp to the top for every strike.
///
/// `None` draws at the midpoint and must not collapse to either end: 0.0
/// renders "unknown" as "weakest", 1.0 as "strongest".
fn energy_size_scale(energy: Option<f32>) -> f32 {
    match energy {
        Some(e) => (e.log10().clamp(-16.0, -12.0) + 16.0) / 4.0,
        None => 0.5,
    }
}

pub struct GlmRenderParams {
    pub zoom: f64,
    pub is_dark: bool,
    pub time_window_secs: f64,
    pub now: chrono::NaiveDateTime,
}

pub fn rasterize_glm_strikes(
    flashes: &[GlmFlash],
    items: &[Arc<dyn OverlayItem>],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    params: &GlmRenderParams,
) -> RasterizeOutput {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_glm_strikes ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_map: None,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;
    let mut hit_map = HitMap::new(width, height);

    // ~12 px at zoom 6, clamped to 6-20 px.
    let zoom_f32 = params.zoom as f32;
    let base_size = (zoom_f32 * 2.0).clamp(6.0, 20.0);

    for (i, flash) in flashes.iter().enumerate() {
        if flash.lat < bounds.min_lat
            || flash.lat > bounds.max_lat
            || flash.lon < bounds.min_lon
            || flash.lon > bounds.max_lon
        {
            continue;
        }

        let age_secs = (params.now - flash.time).num_milliseconds().max(0) as f64 / 1000.0;
        if age_secs > params.time_window_secs {
            continue;
        }

        let (px, py) = mb.project(flash.lat, flash.lon, w, h);
        if px < -base_size || px > w + base_size || py < -base_size || py > h + base_size {
            continue;
        }

        let bolt_size = base_size * (0.8 + energy_size_scale(flash.energy) * 0.4);

        let rgba = time_decay_color(age_secs, params.time_window_secs, params.is_dark);
        draw_lightning_bolt(&mut pixmap, px, py, bolt_size, rgba);

        if let Some(item) = items.get(i) {
            let item_id = i as u32;
            hit_map.register_id(item_id, Arc::clone(item));
            let r = bolt_size * 0.6;
            let r2 = r * r;
            let mut sy = (py - r) as i32;
            let sy_end = (py + r) as i32;
            while sy <= sy_end {
                let mut sx = (px - r) as i32;
                let sx_end = (px + r) as i32;
                while sx <= sx_end {
                    let dx = sx as f32 - px;
                    let dy = sy as f32 - py;
                    if dx * dx + dy * dy <= r2 {
                        hit_map.record(sx as f32, sy as f32, item_id);
                    }
                    sx += 4;
                }
                sy += 4;
            }
        }
    }

    premultiplied_to_straight(pixmap.data_mut());
    RasterizeOutput {
        rgba: pixmap.take(),
        hit_map: Some(hit_map),
    }
}

// ── Feature rendering ────────────────────────────────────────────────────

fn draw_feature(
    pixmap: &mut Pixmap,
    feature: &OverlayFeature,
    mb: &MercatorBounds,
    w: f32,
    h: f32,
) {
    // Geo-AABB cull before any projection work.
    if let Some(ref fb) = feature.geo_bounds {
        let tb = GeoBounds {
            min_lat: merc_y_to_lat(mb.merc_y_min),
            max_lat: merc_y_to_lat(mb.merc_y_max),
            min_lon: mb.min_lon,
            max_lon: mb.max_lon,
        };
        if !fb.intersects(&tb) {
            return;
        }
    }

    for polygon in &feature.polygons {
        let Some(exterior) = polygon.first() else {
            continue;
        };
        if exterior.len() < 3 {
            continue;
        }
        let ring = strip_closing_dup(exterior);
        let pts: Vec<(f32, f32)> = ring
            .iter()
            .map(|&(lat, lon)| mb.project(lat, lon, w, h))
            .collect();
        if let Some(path) = build_polygon_path(&pts) {
            fill_path(pixmap, &path, feature.fill_rgba);
            if feature.stroke_rgba[3] > 0 {
                let sw = scaled_stroke_width(&path, 1.5);
                stroke_path(pixmap, &path, feature.stroke_rgba, sw);
            }
        }
    }
}

// ── Path building helpers ────────────────────────────────────────────────

/// Thins the stroke below 40 px minimum dimension, so a small polygon is not
/// swallowed by its own outline. `base` is the width at close zoom.
fn scaled_stroke_width(path: &tiny_skia::Path, base: f32) -> f32 {
    let b = path.bounds();
    let min_dim = b.width().min(b.height());
    (min_dim / 40.0 * base).clamp(0.5, base)
}

/// `None` for degenerate paths, which tiny-skia cannot fill.
pub(crate) fn build_polygon_path(pts: &[(f32, f32)]) -> Option<tiny_skia::Path> {
    if pts.len() < 3 {
        return None;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0].0, pts[0].1);
    for &(x, y) in &pts[1..] {
        pb.line_to(x, y);
    }
    pb.close();
    let path = pb.finish()?;
    let b = path.bounds();
    if b.width() < 0.1 || b.height() < 0.1 {
        return None;
    }
    Some(path)
}

fn fill_path(pixmap: &mut Pixmap, path: &tiny_skia::Path, rgba: [u8; 4]) {
    if rgba[3] == 0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    paint.anti_alias = true;
    pixmap.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn stroke_path(pixmap: &mut Pixmap, path: &tiny_skia::Path, rgba: [u8; 4], width: f32) {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    paint.anti_alias = true;
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(path, &paint, &stroke, Transform::identity(), None);
}

// ── Utilities ────────────────────────────────────────────────────────────

/// Drops GeoJSON's closing duplicate vertex (last == first).
pub(crate) fn strip_closing_dup(ring: &[(f64, f64)]) -> &[(f64, f64)] {
    if ring.len() > 3 && ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    }
}

use crate::hrrr::HrrrGridData;

/// Returns degrees.
fn merc_y_to_lat(merc_y: f64) -> f64 {
    (2.0 * merc_y.exp().atan() - PI / 2.0).to_degrees()
}

/// tiny-skia writes premultiplied alpha; egui expects straight. Every
/// rasterizer here must call this before returning the buffer.
fn premultiplied_to_straight(data: &mut [u8]) {
    for pixel in data.chunks_exact_mut(4) {
        let a = pixel[3] as f32;
        if a > 0.0 && a < 255.0 {
            let inv = 255.0 / a;
            pixel[0] = (pixel[0] as f32 * inv).min(255.0) as u8;
            pixel[1] = (pixel[1] as f32 * inv).min(255.0) as u8;
            pixel[2] = (pixel[2] as f32 * inv).min(255.0) as u8;
        }
    }
}

// ── Model data (HRRR) rasterization ──────────────────────────────────────

/// Half-open `(i, j)` ranges of the grid the rasterizer touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IndexWindow {
    i0: usize,
    i1: usize,
    j0: usize,
    j1: usize,
}

impl IndexWindow {
    fn is_empty(&self) -> bool {
        self.i0 >= self.i1 || self.j0 >= self.j1
    }

    /// The cells this window can *draw*: one ring in from the edge, because
    /// sizing a cell reads its four neighbours, and only a neighbour inside the
    /// window has been projected. The grid's own edges need no ring — the
    /// sizing code already has a one-sided branch for them.
    fn interior(&self, ni: usize, nj: usize) -> Self {
        Self {
            i0: self.i0 + usize::from(self.i0 > 0),
            i1: self.i1 - usize::from(self.i1 < ni),
            j0: self.j0 + usize::from(self.j0 > 0),
            j1: self.j1 - usize::from(self.j1 < nj),
        }
    }
}

/// How far outside the texture a grid point can be and still paint into it.
///
/// `0.55` cells is the deliberate overlap the cell loop applies; the rest is
/// headroom for that spacing varying with latitude across the box.
const CELL_REACH: f64 = 0.75;

/// The same reach, in pixels: the half-extent has a `0.5` px floor, and `as i32`
/// truncates toward zero, so a rect ending just left of `x = 0` still paints
/// column 0. Rounded up from 1.5.
const PIXEL_REACH: f64 = 2.0;

/// `bounds` widened by `lon_pad` degrees and `merc_pad` of Mercator `y`.
///
/// Latitude is padded in Mercator, not in degrees, because that is the axis the
/// texture's pixels are linear in.
fn grow_bounds(bounds: &GeoBounds, lon_pad: f64, merc_pad: f64) -> GeoBounds {
    let mb = MercatorBounds::from_geo(bounds);
    GeoBounds {
        min_lon: bounds.min_lon - lon_pad,
        max_lon: bounds.max_lon + lon_pad,
        min_lat: merc_y_to_lat(mb.merc_y_min - merc_pad),
        max_lat: merc_y_to_lat(mb.merc_y_max + merc_pad),
    }
}

/// Which grid points [`rasterize_model_data`] must project.
///
/// The whole grid unless the coordinates can name a narrower window — see
/// [`crate::hrrr::GridCoords::index_bounds`].
///
/// A point *outside* the texture still paints into it, so the box is grown
/// before the index range is taken. Two things about that growth are
/// load-bearing:
///
///  * it grows the **box**, not the index range. A Lambert row is not a
///    parallel, so the box's latitude edge bounds `i` as well as `j`: a point a
///    hair south of the box can sit at an `i` well outside the unwidened range
///    while its longitude — and hence its column of pixels — is squarely inside
///    the texture.
///  * the pad is **absolute** — [`CELL_REACH`] grid cells and [`PIXEL_REACH`]
///    texture pixels — not a fraction of the box. The rasterizer's reach is
///    0.55 of a *cell*, which does not shrink with the viewport. Scaling the pad
///    to the box instead blanks the overlay below ~0.3 cells per texture, i.e.
///    from about map zoom 19 up, where a single cell covers the whole texture.
fn projection_window(
    grid: &HrrrGridData,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> IndexWindow {
    let full = IndexWindow {
        i0: 0,
        i1: grid.ni,
        j0: 0,
        j1: grid.nj,
    };

    // A grid with a longitude discontinuity in it — across the anti-meridian or
    // across the cone's own seam — has an `i` neighbour most of a turn away, so
    // its cell is stretched right across the texture and the "0.55 of a cell"
    // reach this window is built on stops describing it. The *box* crossing the
    // seam is handled inside `index_bounds`; this is the same hazard from the
    // grid's side, which nothing there can see.
    if grid.coords.wraps_longitude() {
        return full;
    }

    // `cos` is taken at the box's own extreme latitude: the only cells that can
    // reach the texture sit within a cell of the box, where the spacing is the
    // same to well inside the headroom in `CELL_REACH`.
    let edge_lat = bounds.min_lat.abs().max(bounds.max_lat.abs());
    let Some(cell_deg) = grid.coords.cell_span_degrees(edge_lat) else {
        return full;
    };
    let mb = MercatorBounds::from_geo(bounds);
    let lon_pad = CELL_REACH * cell_deg
        + PIXEL_REACH * (bounds.max_lon - bounds.min_lon) / width.max(1) as f64;
    let merc_pad = CELL_REACH * cell_deg.to_radians()
        + PIXEL_REACH * (mb.merc_y_max - mb.merc_y_min) / height.max(1) as f64;

    let grown = grow_bounds(bounds, lon_pad, merc_pad);
    let Some((fi0, fi1, fj0, fj1)) = grid.coords.index_bounds(&grown, grid.ni, grid.nj) else {
        return full;
    };

    // One more cell each way so every drawn cell's four neighbours are
    // projected too — an unprojected neighbour resizes the cell.
    let low = |f: f64, n: usize| (f.floor() - 1.0).max(0.0).min(n as f64) as usize;
    let high = |f: f64, n: usize| (f.ceil() + 1.0).max(0.0).min(n as f64) as usize;

    IndexWindow {
        i0: low(fi0, grid.ni),
        i1: high(fi1, grid.ni),
        j0: low(fj0, grid.nj),
        j1: high(fj1, grid.nj),
    }
}

/// Writes pixels directly rather than going through tiny-skia: one filled
/// rectangle per grid point, sized from its neighbour spacing.
pub fn rasterize_model_data(
    grid: &HrrrGridData,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> RasterizeOutput {
    let size = (width * height * 4) as usize;
    let mut rgba = vec![0u8; size];

    if grid.values.is_empty() || width == 0 || height == 0 || grid.ni == 0 || grid.nj == 0 {
        return RasterizeOutput {
            rgba,
            hit_map: None,
        };
    }

    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;
    let ni = grid.ni;
    let nj = grid.nj;

    // `coords.at` is the Lambert inverse for HRRR, and this loop used to run it
    // over all 1.9 M points — two thirds of this function's cost, on the
    // background render thread, re-paid on every zoom step and every third of a
    // viewport of pan (`OverlayTextureCache::needs_rerender`). Only points that
    // can influence a pixel of *this* texture are projected now.
    let win = projection_window(grid, bounds, width, height);
    if win.is_empty() {
        return RasterizeOutput {
            rgba,
            hit_map: None,
        };
    }
    let win_w = win.i1 - win.i0;

    // Pre-project once: the cell loop reads each neighbour several times.
    let mut px_coords: Vec<(f32, f32)> = Vec::with_capacity(win_w * (win.j1 - win.j0));
    for j in win.j0..win.j1 {
        for i in win.i0..win.i1 {
            match grid.coords.at(j * ni + i) {
                Some((lat, lon)) => px_coords.push(mb.project(lat, lon, w, h)),
                None => px_coords.push((f32::NAN, f32::NAN)),
            }
        }
    }
    // Every read below is inside `win`; `interior` is what guarantees it.
    let at = |i: usize, j: usize| px_coords[(j - win.j0) * win_w + (i - win.i0)];

    let draw = win.interior(ni, nj);
    for j in draw.j0..draw.j1 {
        for i in draw.i0..draw.i1 {
            let idx = j * ni + i;
            if idx >= grid.values.len() {
                continue;
            }
            let value = grid.values[idx];
            let color = grid.parameter.color_for_value(value);
            if color[3] == 0 {
                continue;
            }

            let (cx, cy) = at(i, j);
            if cx.is_nan() || cy.is_nan() {
                continue;
            }

            // Half-extents from neighbour spacing. 0.55, not 0.50: a slight
            // overlap hides seams between adjacent cells.
            let dx_left = if i > 0 {
                let (nx, _) = at(i - 1, j);
                ((cx - nx).abs() * 0.55).max(0.5)
            } else if i + 1 < ni {
                let (nx, _) = at(i + 1, j);
                ((nx - cx).abs() * 0.55).max(0.5)
            } else {
                1.0
            };
            let dx_right = if i + 1 < ni {
                let (nx, _) = at(i + 1, j);
                ((nx - cx).abs() * 0.55).max(0.5)
            } else {
                dx_left
            };
            let dy_up = if j > 0 {
                let (_, ny) = at(i, j - 1);
                ((cy - ny).abs() * 0.55).max(0.5)
            } else if j + 1 < nj {
                let (_, ny) = at(i, j + 1);
                ((ny - cy).abs() * 0.55).max(0.5)
            } else {
                1.0
            };
            let dy_down = if j + 1 < nj {
                let (_, ny) = at(i, j + 1);
                ((ny - cy).abs() * 0.55).max(0.5)
            } else {
                dy_up
            };

            let x0 = ((cx - dx_left) as i32).max(0);
            let y0 = ((cy - dy_up) as i32).max(0);
            let x1 = ((cx + dx_right) as i32).min(width as i32 - 1);
            let y1 = ((cy + dy_down) as i32).min(height as i32 - 1);

            for y in y0..=y1 {
                let row_offset = (y as u32 * width * 4) as usize;
                for x in x0..=x1 {
                    let offset = row_offset + (x as u32 * 4) as usize;
                    // Overwrite — no blending between adjacent grid cells.
                    rgba[offset] = color[0];
                    rgba[offset + 1] = color[1];
                    rgba[offset + 2] = color[2];
                    rgba[offset + 3] = color[3];
                }
            }
        }
    }

    RasterizeOutput {
        rgba,
        hit_map: None,
    }
}

#[cfg(test)]
mod glm_energy_tests {
    use super::*;
    use crate::glm::{GlmDataLevel, GlmFlash, GlmSatellite};

    /// The ends of the clamp window.
    const WEAKEST: f32 = 1e-16;
    const STRONGEST: f32 = 1e-12;

    /// Fails if an unreported energy renders as an extreme. A `0.0` sentinel
    /// does: `0.0f32.log10()` is `-inf`, which clamps to the window floor.
    #[test]
    fn unknown_energy_draws_between_the_extremes() {
        let unknown = energy_size_scale(None);
        let weakest = energy_size_scale(Some(WEAKEST));
        let strongest = energy_size_scale(Some(STRONGEST));

        assert_eq!(weakest, 0.0, "the window floor should be the channel floor");
        assert_eq!(
            strongest, 1.0,
            "the window ceiling should be the channel ceiling"
        );
        assert!(
            unknown > weakest,
            "an unreported energy must not render as the weakest strike (got {unknown})"
        );
        assert!(
            unknown < strongest,
            "an unreported energy must not render as the strongest strike (got {unknown})"
        );
    }

    /// A reinstated `0.0` sentinel lands on the floor, indistinguishable from
    /// the weakest real strike.
    #[test]
    fn zero_energy_would_clamp_to_the_floor() {
        assert_eq!(energy_size_scale(Some(0.0)), 0.0);
        assert_eq!(
            energy_size_scale(Some(0.0)),
            energy_size_scale(Some(WEAKEST))
        );
    }

    #[test]
    fn energy_scale_is_monotonic_and_clamped() {
        assert!(energy_size_scale(Some(1e-14)) > energy_size_scale(Some(1e-15)));
        assert_eq!(energy_size_scale(Some(1e-20)), 0.0);
        assert_eq!(energy_size_scale(Some(1e-9)), 1.0);
    }

    fn render_one(energy: Option<f32>) -> usize {
        let flash = GlmFlash {
            lat: 35.0,
            lon: -97.0,
            energy,
            area: None,
            time: chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            satellite: GlmSatellite::GoesEast,
            level: GlmDataLevel::Flash,
        };
        let bounds = GeoBounds {
            min_lat: 34.0,
            max_lat: 36.0,
            min_lon: -98.0,
            max_lon: -96.0,
        };
        let out = rasterize_glm_strikes(
            std::slice::from_ref(&flash),
            &[],
            &bounds,
            128,
            128,
            &GlmRenderParams {
                zoom: 8.0,
                is_dark: true,
                time_window_secs: 300.0,
                now: flash.time,
            },
        );
        // Bolt size is the only thing varying, so painted-pixel count is a
        // proxy for the size the strike was drawn at.
        out.rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
    }

    /// Pins the *wiring*, not just the mapping: an unreported energy has to
    /// reach the canvas as a mid-size bolt.
    #[test]
    fn unknown_energy_renders_larger_than_the_weakest_strike() {
        let unknown = render_one(None);
        let weakest = render_one(Some(WEAKEST));
        let strongest = render_one(Some(STRONGEST));

        assert!(weakest > 0, "the fixture must actually draw something");
        assert!(
            unknown > weakest,
            "unknown energy drew {unknown} px, the weakest strike drew {weakest} px"
        );
        assert!(
            unknown < strongest,
            "unknown energy drew {unknown} px, the strongest strike drew {strongest} px"
        );
    }
}

#[cfg(test)]
pub(crate) mod lambert_fixture {
    use super::*;
    use crate::hrrr::{GridCoords, ModelParameter, lambert::LambertGrid, summarize_values};

    /// An `ni` x `nj` grid on HRRR's own Lambert projection and 3 km step.
    /// `scanning_mode` is HRRR's `0b0100_0000` unless a test wants another.
    ///
    /// Values vary along both axes, so the painted output depends on *which*
    /// points were projected, not merely how many.
    pub(crate) fn lambert_grid(ni: usize, nj: usize, scanning_mode: u8) -> HrrrGridData {
        lambert_grid_stepped(ni, nj, scanning_mode, 3_000_000, 262_500_000)
    }

    /// `step` is the grid spacing in micro-metres and `lov` the central meridian
    /// in microdegrees. `lov` matters because it places the cone's seam at
    /// `lov + 180`: HRRR's 262.5 puts it at 82.5 E, well away from the
    /// anti-meridian, while `lov = 0` puts the two on top of each other.
    pub(crate) fn lambert_grid_stepped(
        ni: usize,
        nj: usize,
        scanning_mode: u8,
        step: u32,
        lov: u32,
    ) -> HrrrGridData {
        use grib::def::grib2::template::param_set::ScanningMode;
        let mut template = crate::hrrr::lambert::hrrr_conus_grid();
        template.ni = ni as u32;
        template.nj = nj as u32;
        template.scanning_mode = ScanningMode(scanning_mode);
        template.lov = lov;
        template.dx = step;
        template.dy = step;
        let geometry = LambertGrid::from_template(&template).unwrap();

        let parameter = ModelParameter::SurfaceBasedCape;
        let values: Vec<f32> = (0..ni * nj)
            .map(|k| ((k % 4001) + (k / ni.max(1)) % 997) as f32)
            .collect();
        let (visible_points, value_range) = summarize_values(&values, parameter);

        let mut bounds = GeoBounds {
            min_lat: f64::MAX,
            max_lat: f64::MIN,
            min_lon: f64::MAX,
            max_lon: f64::MIN,
        };
        for k in 0..ni * nj {
            let (lat, lon) = geometry.latlon_at(k).unwrap();
            bounds.min_lat = bounds.min_lat.min(lat);
            bounds.max_lat = bounds.max_lat.max(lat);
            bounds.min_lon = bounds.min_lon.min(lon);
            bounds.max_lon = bounds.max_lon.max(lon);
        }

        HrrrGridData {
            parameter,
            values,
            coords: GridCoords::Lambert(geometry),
            ni,
            nj,
            bounds,
            ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            forecast_hour: 0,
            visible_points,
            value_range,
        }
    }

    /// The same grid with its coordinates materialised — which is both what the
    /// rasterizer did before the grid went lazy and the one `GridCoords` arm
    /// [`projection_window`] declines to narrow. So rasterizing this is the
    /// project-every-point reference, on bit-identical coordinates.
    pub(crate) fn materialised(grid: &HrrrGridData) -> HrrrGridData {
        let n = grid.ni * grid.nj;
        let (mut lats, mut lons) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for k in 0..n {
            let (lat, lon) = grid.coords.at(k).expect("a full grid");
            lats.push(lat);
            lons.push(lon);
        }
        HrrrGridData {
            coords: GridCoords::Explicit { lats, lons },
            ..grid.clone()
        }
    }

    /// A box `cells` grid cells across, centred on grid point `(i, j)`.
    ///
    /// Sized in *cells*, not in a fraction of the grid: the rasterizer's reach
    /// is 0.55 of a cell, so that is the unit the window has to be correct in,
    /// and a sweep stated as a fraction of the grid never reaches the regime
    /// where one cell covers the whole texture.
    pub(crate) fn box_of_cells(
        grid: &HrrrGridData,
        i: usize,
        j: usize,
        cells: f64,
        offset: (f64, f64),
    ) -> GeoBounds {
        let at = |i: usize, j: usize| grid.coords.at(j * grid.ni + i).expect("in range");
        let (lat, lon) = at(i, j);
        let (nlat, nlon) = at(i + 1, j + 1);
        let (dlat, dlon) = ((nlat - lat).abs(), (nlon - lon).abs());
        // `offset` shifts the centre off the lattice point, in cells. A box
        // centred exactly on a grid point is the one case the window's
        // `floor - 1` / `ceil + 1` rounding covers for free, so an aligned-only
        // sweep cannot see a margin that is too small.
        let (clat, clon) = (lat + dlat * offset.1, lon + dlon * offset.0);
        GeoBounds {
            min_lat: clat - dlat * cells / 2.0,
            max_lat: clat + dlat * cells / 2.0,
            min_lon: clon - dlon * cells / 2.0,
            max_lon: clon + dlon * cells / 2.0,
        }
    }

    /// Sub-cell offsets a viewport centre can land on, in cells. The aligned
    /// case is first and is the weakest; the rest are what actually exercise the
    /// margin.
    pub(crate) const CELL_OFFSETS: &[(f64, f64)] = &[
        (0.0, 0.0),
        (0.5, 0.0),
        (0.0, 0.5),
        (0.5, 0.5),
        (0.37, -0.29),
        (-0.44, 0.18),
    ];

    /// The 97x61 grid moved to `first_point_lon`, keeping HRRR's projection.
    /// Used to park a grid on the cone seam at `lon0 + 180 = 82.5 E`.
    pub(crate) fn grid_anchored_at(first_point_lon: u32) -> HrrrGridData {
        let mut grid = lambert_grid(97, 61, 0b0100_0000);
        let mut template = crate::hrrr::lambert::hrrr_conus_grid();
        template.ni = 97;
        template.nj = 61;
        template.first_point_lat = 30_000_000;
        template.first_point_lon = first_point_lon;
        let geometry = LambertGrid::from_template(&template).unwrap();
        grid.bounds = GeoBounds {
            min_lat: f64::MAX,
            max_lat: f64::MIN,
            min_lon: f64::MAX,
            max_lon: f64::MIN,
        };
        for k in 0..grid.ni * grid.nj {
            let (lat, lon) = geometry.latlon_at(k).unwrap();
            grid.bounds.min_lat = grid.bounds.min_lat.min(lat);
            grid.bounds.max_lat = grid.bounds.max_lat.max(lat);
            grid.bounds.min_lon = grid.bounds.min_lon.min(lon);
            grid.bounds.max_lon = grid.bounds.max_lon.max(lon);
        }
        grid.coords = GridCoords::Lambert(geometry);
        grid
    }

    /// A texture's coverage: a lat/lon box centred on `(lat, lon)`.
    pub(crate) fn coverage(lat: f64, lon: f64, span: f64) -> GeoBounds {
        GeoBounds {
            min_lat: lat - span / 2.0,
            max_lat: lat + span / 2.0,
            min_lon: lon - span / 2.0,
            max_lon: lon + span / 2.0,
        }
    }
}

#[cfg(test)]
mod projection_window_tests {
    use super::lambert_fixture::{
        CELL_OFFSETS, box_of_cells, coverage, grid_anchored_at, lambert_grid, lambert_grid_stepped,
        materialised,
    };
    use super::*;

    /// Texture sides chosen to straddle the interesting regime change: at 1-8 px
    /// a grid cell is far smaller than a pixel, at 256 px far larger.
    const SIDES: &[u32] = &[1, 2, 7, 64, 256];

    /// Boxes stated as a fraction of the grid's own extent, so they mean the
    /// same thing whatever grid they are applied to: centred, off each corner,
    /// straddling each edge, enclosing everything, and missing entirely.
    const BOXES: &[(&str, f64, f64, f64, f64)] = &[
        // (label, centre-x fraction, centre-y fraction, width, height) of bounds
        ("tiny centre", 0.5, 0.5, 0.05, 0.05),
        ("half", 0.5, 0.5, 0.5, 0.5),
        ("all of it", 0.5, 0.5, 1.0, 1.0),
        ("twice over", 0.5, 0.5, 2.0, 2.0),
        ("SW corner", 0.0, 0.0, 0.3, 0.3),
        ("NE corner", 1.0, 1.0, 0.3, 0.3),
        ("NW corner", 0.0, 1.0, 0.3, 0.3),
        ("SE corner", 1.0, 0.0, 0.3, 0.3),
        ("west edge", 0.0, 0.5, 0.4, 1.4),
        ("north edge", 0.5, 1.0, 1.4, 0.4),
        ("wide and thin", 0.5, 0.5, 3.0, 0.04),
        ("tall and thin", 0.5, 0.5, 0.04, 3.0),
        ("far east", 3.0, 0.5, 0.5, 0.5),
        ("far north", 0.5, 4.0, 0.5, 0.5),
    ];

    fn box_over(g: &GeoBounds, fx: f64, fy: f64, w: f64, h: f64) -> GeoBounds {
        let (lon_span, lat_span) = (g.max_lon - g.min_lon, g.max_lat - g.min_lat);
        let (cx, cy) = (g.min_lon + lon_span * fx, g.min_lat + lat_span * fy);
        GeoBounds {
            min_lon: cx - lon_span * w / 2.0,
            max_lon: cx + lon_span * w / 2.0,
            min_lat: cy - lat_span * h / 2.0,
            max_lat: cy + lat_span * h / 2.0,
        }
    }

    /// **The window must be invisible in the output.** Not "close": the same
    /// bytes, because the coordinates are the same coordinates and skipping a
    /// point that could paint — or whose spacing sizes a point that paints — is
    /// a defect, not a trade-off.
    ///
    /// The reference grid is the *materialised* twin, which is both the shape
    /// the rasterizer had before the grid went lazy and the arm
    /// [`projection_window`] declines to narrow, so it really does project all
    /// of them.
    #[test]
    fn the_window_paints_exactly_what_projecting_every_point_paints() {
        let lambert = lambert_grid(97, 61, 0b0100_0000);
        let every_point = materialised(&lambert);

        for &(label, fx, fy, bw, bh) in BOXES {
            let bounds = box_over(&lambert.bounds, fx, fy, bw, bh);
            for &side in SIDES {
                let windowed = rasterize_model_data(&lambert, &bounds, side, side);
                let reference = rasterize_model_data(&every_point, &bounds, side, side);
                assert_eq!(
                    windowed.rgba, reference.rgba,
                    "{label} at {side}x{side}: the window changed the picture",
                );
            }
        }
    }

    /// Non-square textures cannot be caught above: an `i`/`j` margin swapped
    /// between the axes is invisible while width == height.
    #[test]
    fn the_window_survives_a_non_square_texture() {
        let lambert = lambert_grid(97, 61, 0b0100_0000);
        let every_point = materialised(&lambert);

        for &(label, fx, fy, bw, bh) in BOXES {
            let bounds = box_over(&lambert.bounds, fx, fy, bw, bh);
            for &(w, h) in &[(320u32, 24u32), (24, 320), (200, 3)] {
                assert_eq!(
                    rasterize_model_data(&lambert, &bounds, w, h).rgba,
                    rasterize_model_data(&every_point, &bounds, w, h).rgba,
                    "{label} at {w}x{h}",
                );
            }
        }
    }

    /// A scan order that does not lay the flat index out as `j * ni + i` — the
    /// only order the neighbour walk understands — must make the window decline
    /// to narrow, rather than narrow the wrong axis.
    ///
    /// GRIB2 Table 3.4: bit 3 (`0b0010_0000`) makes `j` the consecutive axis and
    /// bit 4 (`0b0001_0000`) makes rows alternate; either breaks the layout. The
    /// `i`/`j` *directions* (bits 1 and 2) do not — they only flip the step
    /// signs — so those four modes must still narrow, and are the control here.
    #[test]
    fn a_scan_order_the_neighbour_walk_does_not_match_is_not_narrowed() {
        let whole = |g: &HrrrGridData| IndexWindow {
            i0: 0,
            i1: g.ni,
            j0: 0,
            j1: g.nj,
        };
        for mode in [
            0b0000_0000u8,
            0b0100_0000,
            0b1000_0000,
            0b1100_0000,
            0b0010_0000,
            0b0110_0000,
            0b0101_0000,
            0b0001_0000,
        ] {
            let grid = lambert_grid(41, 29, mode);
            let bounds = box_over(&grid.bounds, 0.5, 0.5, 0.4, 0.4);
            let row_major = mode & 0b0011_0000 == 0;
            let window = projection_window(&grid, &bounds, 128, 128);
            assert_eq!(
                window == whole(&grid),
                !row_major,
                "scanning mode {mode:#010b}: got {window:?}",
            );
            assert_eq!(
                rasterize_model_data(&grid, &bounds, 128, 128).rgba,
                rasterize_model_data(&materialised(&grid), &bounds, 128, 128).rgba,
                "scanning mode {mode:#010b}",
            );
        }
    }

    /// A window that never narrows would pass every test above. This is the
    /// control: the point of the change is that a small viewport projects a
    /// small fraction of the grid.
    #[test]
    fn a_small_viewport_narrows_the_window_sharply() {
        let grid = lambert_grid(1799, 1059, 0b0100_0000);
        let full = (grid.ni * grid.nj) as f64;

        let tight = projection_window(&grid, &coverage(35.5, -97.5, 3.0), 1024, 1024);
        let tight_points = ((tight.i1 - tight.i0) * (tight.j1 - tight.j0)) as f64;
        assert!(
            tight_points / full < 0.02,
            "a 3° viewport still projects {:.1}% of the grid",
            100.0 * tight_points / full,
        );

        let typical = projection_window(&grid, &coverage(35.5, -97.5, 12.0), 1024, 1024);
        let typical_points = ((typical.i1 - typical.i0) * (typical.j1 - typical.j0)) as f64;
        assert!(
            typical_points / full < 0.2,
            "a 12° viewport still projects {:.1}% of the grid",
            100.0 * typical_points / full,
        );

        // Off the grid entirely: nothing at all.
        assert!(
            projection_window(&grid, &coverage(35.5, -40.0, 12.0), 1024, 1024).is_empty(),
            "an Atlantic viewport must project nothing",
        );

        // And the whole domain must still be the whole domain.
        assert_eq!(
            projection_window(&grid, &coverage(37.0, -97.5, 75.0), 1024, 1024),
            IndexWindow {
                i0: 0,
                i1: grid.ni,
                j0: 0,
                j1: grid.nj
            },
        );
    }

    /// The fixed boxes above are the cases someone thought of. This is the
    /// sweep: 400 boxes and texture shapes drawn from a fixed seed, ranging from
    /// a box a fiftieth of the grid to one eight times its size, and from a
    /// 1 px texture to a 300 px one. Any margin that is merely *usually* enough
    /// fails here.
    #[test]
    fn the_window_survives_a_randomised_sweep_of_viewports() {
        let lambert = lambert_grid(73, 47, 0b0100_0000);
        let every_point = materialised(&lambert);

        // xorshift64*, so the cases are the same on every machine and run.
        let mut state = 0x2026_0725_1200_0001u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };

        let mut narrowed = 0;
        let whole = IndexWindow {
            i0: 0,
            i1: lambert.ni,
            j0: 0,
            j1: lambert.nj,
        };
        for case in 0..400 {
            let (fx, fy) = (next() * 3.0 - 1.0, next() * 3.0 - 1.0);
            // Log-uniform, so small boxes — the ones that narrow — are not
            // crowded out by large ones.
            let (bw, bh) = (0.02 * 400f64.powf(next()), 0.02 * 400f64.powf(next()));
            let w = 1 + (next() * 300.0) as u32;
            let h = 1 + (next() * 300.0) as u32;
            let bounds = box_over(&lambert.bounds, fx, fy, bw, bh);
            if projection_window(&lambert, &bounds, w, h) != whole {
                narrowed += 1;
            }
            assert_eq!(
                rasterize_model_data(&lambert, &bounds, w, h).rgba,
                rasterize_model_data(&every_point, &bounds, w, h).rgba,
                "case {case}: box ({fx:.3}, {fy:.3}) x ({bw:.3}, {bh:.3}) at {w}x{h}",
            );
        }
        // Without this the sweep could pass on 400 cases that all fell back to
        // the whole grid, which proves nothing about the window.
        assert!(
            narrowed > 200,
            "only {narrowed} of 400 cases narrowed at all"
        );
    }

    /// **Street-level zoom.** `walkers` allows zoom 0-26 and nothing here
    /// narrows that, so a texture routinely covers a fraction of one 3 km cell.
    /// A margin stated as a fraction of the box cannot reach 0.55 of a cell once
    /// the box is smaller than a cell, and the overlay goes mostly blank —
    /// 5.5 M of 6.3 M pixels wrong at zoom 19, worsening upward.
    ///
    /// Sized in cells so the regime, not the grid, is what varies. Corners are
    /// included because the one-sided neighbour branches live there.
    #[test]
    fn the_window_survives_a_viewport_smaller_than_one_grid_cell() {
        let lambert = lambert_grid(97, 61, 0b0100_0000);
        let every_point = materialised(&lambert);

        for &cells in &[0.005, 0.05, 0.3, 0.7, 1.0, 2.0, 6.0] {
            for &(i, j) in &[(48, 30), (0, 0), (95, 59), (1, 1), (10, 55), (80, 2)] {
                for &offset in CELL_OFFSETS {
                    let bounds = box_of_cells(&lambert, i, j, cells, offset);
                    for &(w, h) in &[(64u32, 64u32), (512, 512), (385, 3), (3, 385), (1, 1)] {
                        assert_eq!(
                            rasterize_model_data(&lambert, &bounds, w, h).rgba,
                            rasterize_model_data(&every_point, &bounds, w, h).rgba,
                            "{cells} cells around ({i}, {j}) offset {offset:?} at {w}x{h}",
                        );
                    }
                }
            }
        }
    }

    /// **Past the anti-meridian.** `expand_for_overdraw` clamps latitude to the
    /// Mercator limit but leaves longitude alone, and `walkers::unproject`
    /// neither wraps nor clamps it, so panning west at low zoom produces a
    /// texture running past -180. Grid longitudes are normalised to -180..180,
    /// so such a box is not the interval it looks like.
    ///
    /// The numbers are the reviewer's: -277 is where `lon0 - 180 = -277.5` plus
    /// the growth crosses the cone's seam, and -290..-110 is a viewport of
    /// -230..-170 expanded by `OVERDRAW_FRACTION = 1.0`.
    #[test]
    fn the_window_survives_a_texture_running_past_the_antimeridian() {
        let lambert = lambert_grid(97, 61, 0b0100_0000);
        let every_point = materialised(&lambert);

        for &(min_lon, max_lon) in &[
            (-170.0, -30.0),  // control: inside the fold, must still narrow
            (-277.0, -20.0),  // just past the seam
            (-290.0, -110.0), // the overdraw-expanded viewport
            (-310.0, -10.0),
            (-360.0, 0.0),
            (10.0, 300.0), // and the eastern side
        ] {
            let bounds = GeoBounds {
                min_lat: 15.0,
                max_lat: 55.0,
                min_lon,
                max_lon,
            };
            for &side in &[64u32, 512] {
                assert_eq!(
                    rasterize_model_data(&lambert, &bounds, side, side).rgba,
                    rasterize_model_data(&every_point, &bounds, side, side).rgba,
                    "longitude {min_lon}..{max_lon} at {side}x{side}",
                );
            }
        }
    }

    /// A grid that itself straddles the anti-meridian: its `i` neighbour is a
    /// whole turn away in longitude, so the cell's rect is stretched across the
    /// texture and the "0.55 of a cell" reach this window is built on stops
    /// describing it. The window has to decline rather than model that.
    ///
    /// Not hypothetical arithmetic: with the guard removed this sweep fails 372
    /// of 600 cases on the 400 km grid and 5 of 600 on the 200 km one, while the
    /// HRRR-shaped control stays at 0 either way.
    ///
    /// **`LoV` is swept, and that is the point.** The guard this replaced asked
    /// whether the grid's `min_lon..max_lon` contained the seam, which for a
    /// wrapping grid is `-180..180` — so the seam only falls *inside* it when
    /// `LoV` puts it somewhere other than the anti-meridian. HRRR's 262.5 does;
    /// `LoV = 0` does not, and that case failed 398 of 600 while the test
    /// claiming to cover it passed. `LoV = 0` is the likeliest value there is
    /// for a global or European Lambert model.
    #[test]
    fn a_grid_that_wraps_the_globe_is_not_narrowed() {
        for &(ni, nj, step, lov, label) in &[
            (
                300usize,
                40usize,
                200_000_000u32,
                262_500_000u32,
                "200 km, LoV 262.5",
            ),
            (120, 60, 400_000_000, 262_500_000, "400 km, LoV 262.5"),
            (
                120,
                60,
                400_000_000,
                0,
                "400 km, LoV 0 (seam on the anti-meridian)",
            ),
            (300, 40, 200_000_000, 0, "200 km, LoV 0"),
            (120, 60, 400_000_000, 5_000_000, "400 km, LoV 5"),
            (120, 60, 400_000_000, 355_000_000, "400 km, LoV 355"),
        ] {
            let grid = lambert_grid_stepped(ni, nj, 0b0100_0000, step, lov);
            assert!(
                grid.bounds.max_lon - grid.bounds.min_lon > 180.0,
                "{label}: fixture must actually wrap, spans {}",
                grid.bounds.max_lon - grid.bounds.min_lon,
            );
            assert!(
                grid.coords.wraps_longitude(),
                "{label}: the guard must see the wrap whatever LoV is",
            );
            let every_point = materialised(&grid);

            let mut state = 0x1234_5678_9abc_def1u64;
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f64 / (1u64 << 53) as f64
            };

            for case in 0..300 {
                let (clat, clon) = (next() * 160.0 - 80.0, next() * 360.0 - 180.0);
                let dlat = 0.001 * 20000f64.powf(next());
                let dlon = 0.001 * 20000f64.powf(next());
                let (w, h) = (1 + (next() * 160.0) as u32, 1 + (next() * 160.0) as u32);
                let bounds = GeoBounds {
                    min_lat: (clat - dlat / 2.0).max(-85.0),
                    max_lat: (clat + dlat / 2.0).min(85.0),
                    min_lon: clon - dlon / 2.0,
                    max_lon: clon + dlon / 2.0,
                };
                if bounds.min_lat >= bounds.max_lat {
                    continue;
                }
                assert_eq!(
                    rasterize_model_data(&grid, &bounds, w, h).rgba,
                    rasterize_model_data(&every_point, &bounds, w, h).rgba,
                    "{label} case {case}: {bounds:?} at {w}x{h}",
                );
            }
        }
    }

    /// A grid anchored across the projection's own **seam** — the meridian
    /// opposite the central one, here 82.5 E. `theta` folds there and only then
    /// multiplies by the cone constant, so two `i`-adjacent cells either side of
    /// it land a third of a turn apart in the plane: the cell is stretched
    /// across the whole texture and no cell-sized reach describes it.
    ///
    /// Distinct from both the wrapping grid above and a seam-crossing *box* —
    /// these boxes are small, sit nowhere near the seam, and the grid spans
    /// under 3 degrees. With the guard removed this fails 102-138 of 800 cases
    /// per anchor; the seam check inside `index_bounds` does not see it, because
    /// nothing is wrong with the box.
    #[test]
    fn a_grid_sitting_across_the_projection_seam_is_not_narrowed() {
        for anchor in [81_000_000u32, 82_400_000, 82_500_000, 83_000_000] {
            let grid = grid_anchored_at(anchor);
            let every_point = materialised(&grid);
            assert!(
                grid.bounds.max_lon - grid.bounds.min_lon < 5.0,
                "fixture must be a small grid, not a wrapping one",
            );

            let mut state = 0x0bad_c0de_1234_5678u64;
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f64 / (1u64 << 53) as f64
            };

            for case in 0..250 {
                let (lon_span, lat_span) = (
                    grid.bounds.max_lon - grid.bounds.min_lon,
                    grid.bounds.max_lat - grid.bounds.min_lat,
                );
                let clon = grid.bounds.min_lon + lon_span * (next() * 2.0 - 0.5);
                let clat = grid.bounds.min_lat + lat_span * (next() * 2.0 - 0.5);
                let dlon = 0.001 * 3000f64.powf(next());
                let dlat = 0.001 * 3000f64.powf(next());
                let bounds = GeoBounds {
                    min_lon: clon - dlon / 2.0,
                    max_lon: clon + dlon / 2.0,
                    min_lat: (clat - dlat / 2.0).max(-85.0),
                    max_lat: (clat + dlat / 2.0).min(85.0),
                };
                if bounds.min_lat >= bounds.max_lat {
                    continue;
                }
                let (w, h) = (1 + (next() * 180.0) as u32, 1 + (next() * 180.0) as u32);
                assert_eq!(
                    rasterize_model_data(&grid, &bounds, w, h).rgba,
                    rasterize_model_data(&every_point, &bounds, w, h).rgba,
                    "anchor {anchor} case {case}: {bounds:?} at {w}x{h}",
                );
            }
        }
    }

    /// At real HRRR scale, once per regime. The small grids above vary the
    /// geometry; this pins that 1,905,141 points behave like 5,917.
    #[test]
    fn the_window_is_invisible_at_full_hrrr_scale() {
        let lambert = lambert_grid(1799, 1059, 0b0100_0000);
        let every_point = materialised(&lambert);

        let cases = [
            (coverage(35.5, -97.5, 12.0), "a typical pane"),
            (
                box_of_cells(&lambert, 900, 530, 0.02, (0.37, -0.29)),
                "well inside one cell, off-lattice",
            ),
            (
                GeoBounds {
                    min_lat: 15.0,
                    max_lat: 55.0,
                    min_lon: -290.0,
                    max_lon: -110.0,
                },
                "past the anti-meridian",
            ),
        ];
        for (bounds, what) in cases {
            assert_eq!(
                rasterize_model_data(&lambert, &bounds, 512, 512).rgba,
                rasterize_model_data(&every_point, &bounds, 512, 512).rgba,
                "{what}",
            );
        }
    }
}

#[cfg(test)]
mod model_nan_tests {
    use super::*;
    use crate::hrrr::ModelParameter;

    const BOUNDS: GeoBounds = GeoBounds {
        min_lat: 34.9,
        max_lat: 35.2,
        min_lon: -97.2,
        max_lon: -96.9,
    };

    /// 2x2 over `BOUNDS`, summarised the way the fetch path does.
    fn grid(parameter: ModelParameter, values: Vec<f32>) -> HrrrGridData {
        let (visible_points, value_range) = crate::hrrr::summarize_values(&values, parameter);
        HrrrGridData {
            parameter,
            values,
            coords: crate::hrrr::GridCoords::Explicit {
                lats: vec![35.1, 35.1, 35.0, 35.0],
                lons: vec![-97.1, -97.0, -97.1, -97.0],
            },
            ni: 2,
            nj: 2,
            bounds: BOUNDS,
            ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
                .unwrap()
                .and_hms_opt(3, 0, 0)
                .unwrap(),
            forecast_hour: parameter.forecast_hour(),
            visible_points,
            value_range,
        }
    }

    fn painted_pixels(grid: &HrrrGridData) -> usize {
        let out = rasterize_model_data(grid, &BOUNDS, 64, 64);
        out.rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
    }

    /// Fails if a NaN grid point paints. The ramps in `hrrr/mod.rs` are
    /// descending `if` chains ending in an unguarded `else`: NaN fails every
    /// comparison and lands there, where `f32::min` returns the non-NaN
    /// operand — so an unguarded missing point renders as the *most extreme*
    /// value, under a tooltip that `format_value` leaves blank.
    ///
    /// Unreachable while every HRRR field ships Section 6
    /// `bitmap_indicator = 255`; live the moment NOMADS ships a bitmapped one.
    #[test]
    fn a_missing_grid_point_paints_nothing() {
        for parameter in ModelParameter::all() {
            let all_nan = grid(*parameter, vec![f32::NAN; 4]);
            assert_eq!(
                painted_pixels(&all_nan),
                0,
                "{} painted a missing grid point",
                parameter.display_name(),
            );
        }
    }

    /// Without this, the NaN test passes on a fixture that draws nothing.
    #[test]
    fn the_fixture_paints_when_values_are_present() {
        let alarming = grid(ModelParameter::SurfaceBasedCin, vec![-400.0; 4]);
        assert!(
            painted_pixels(&alarming) > 0,
            "fixture must draw a real field, or the NaN test proves nothing",
        );
    }

    /// Fails if NaN is merely *some* colour rather than fully transparent.
    #[test]
    fn nan_does_not_take_the_extreme_branch_of_any_ramp() {
        for parameter in ModelParameter::all() {
            let nan = parameter.color_for_value(f32::NAN);
            assert_eq!(
                nan,
                [0, 0, 0, 0],
                "{} maps a missing point to a visible colour",
                parameter.display_name(),
            );

            // A value that legitimately saturates this ramp's top branch.
            let extreme = match parameter {
                ModelParameter::SurfaceBasedCin | ModelParameter::MixedLayerCin => -600.0,
                ModelParameter::LiftedIndex => -20.0,
                ModelParameter::Visibility => 0.0,
                ModelParameter::Temperature2m => 400.0,
                _ => 10_000.0,
            };
            assert_ne!(
                parameter.color_for_value(extreme),
                nan,
                "{}: a missing point is indistinguishable from a saturated one",
                parameter.display_name(),
            );
        }
    }

    /// Infinities take the same path as NaN through the ramps.
    #[test]
    fn infinite_values_paint_nothing_either() {
        for parameter in ModelParameter::all() {
            for value in [f32::INFINITY, f32::NEG_INFINITY] {
                assert_eq!(
                    parameter.color_for_value(value),
                    [0, 0, 0, 0],
                    "{} painted {value}",
                    parameter.display_name(),
                );
            }
        }
    }

    /// `px_coords` is NaN-padded when `ni * nj` exceeds the coordinate arrays;
    /// those points must be skipped, not projected somewhere arbitrary.
    ///
    /// A padded point is a *neighbour* of a real one, and neighbour spacing is
    /// what sizes each cell. NaN survives that arithmetic and falls out at the
    /// 0.5 px floor via `f32::max`; any real coordinate — `(0.0, 0.0)` being
    /// the obvious wrong choice — stretches the cell across the texture
    /// instead. Hence the bottom-row assertion: the four real points all sit in
    /// the upper two thirds.
    #[test]
    fn a_grid_shape_mismatch_does_not_paint_padded_points() {
        let mut g = grid(ModelParameter::SurfaceBasedCin, vec![-400.0; 4]);
        // Claim 4x4 while supplying only 4 coordinates and 4 values.
        g.ni = 4;
        g.nj = 4;
        let out = rasterize_model_data(&g, &BOUNDS, 64, 64);
        assert_eq!(out.rgba.len(), 64 * 64 * 4, "must not overrun the buffer");

        let bottom_row = &out.rgba[(63 * 64 * 4)..];
        assert_eq!(
            bottom_row.chunks_exact(4).filter(|px| px[3] > 0).count(),
            0,
            "a padded neighbour stretched a cell to the bottom of the texture",
        );
        assert!(
            out.rgba.chunks_exact(4).any(|px| px[3] > 0),
            "control: the four real points must still paint",
        );
    }
}
