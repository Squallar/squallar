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
use crate::types::{GeoBounds, GeoPolygonRing, OverlayFeature};

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

/// Which of the two RGBA conventions a rasterizer's bytes are written in.
///
/// egui has a constructor for each — `ColorImage::from_rgba_premultiplied` is a
/// copy, `from_rgba_unmultiplied` is a copy through a 64 KiB lookup table — and
/// picking the wrong one does not fail, it shifts every translucent colour. So
/// the convention is carried on [`RasterizeOutput`] rather than known by the
/// consumer: `app_fetch::overlay_color_image` reads it off the value it was
/// handed and cannot be written to assume one.
///
/// It has to be per-rasterizer, not per-crate, because this module genuinely
/// produces both. Everything drawn through tiny-skia is [`Self::Premultiplied`]
/// — that is what a `Pixmap` holds, by definition. [`rasterize_model_data`]
/// bypasses tiny-skia and writes palette bytes into a buffer itself, so its
/// output is [`Self::Straight`]. A single global choice corrupts whichever half
/// it is wrong about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    /// RGB already scaled by alpha — a `tiny_skia::Pixmap`'s own layout, and
    /// egui's `Color32` layout, which is why the pair needs no conversion.
    Premultiplied,
    /// RGB independent of alpha, as a colour table or a picker states it.
    Straight,
}

pub struct RasterizeOutput {
    /// `width × height × 4` bytes, in the convention [`Self::alpha`] names.
    pub rgba: Vec<u8>,
    pub hit_map: Option<HitMap>,
    /// How to read [`Self::rgba`]. See [`AlphaMode`].
    pub alpha: AlphaMode,
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
//
// Every entry point below draws through tiny-skia, so every `Vec<u8>` they
// return is a `Pixmap`'s own buffer: **premultiplied** alpha. They used to
// un-premultiply it on the way out, because egui's overlay upload called
// `ColorImage::from_rgba_unmultiplied`, which immediately multiplied it back.
// The pair cancelled — at 15 ms of division plus 7 ms of extra table lookup per
// 18.7 Mpx texture, and one lossy `u8` round trip, for a picture that was
// already in the layout `Color32` wants. The buffer is now handed over as it
// was drawn, and [`AlphaMode`] on [`RasterizeOutput`] is what tells the
// uploader so.
//
// The exception is [`rasterize_model_data`], which never went through
// tiny-skia and never called the conversion: it writes palette bytes with
// straight alpha, and says so.

/// `hatch_color` is theme-dependent, so it cannot live in the feature.
///
/// Premultiplied RGBA — see the module note above.
pub fn rasterize_spc_outlooks(
    features: &[OverlayFeature],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    hatch_color: [u8; 4],
    device_scale: f32,
) -> Vec<u8> {
    let scale = sane_device_scale(device_scale);
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
        draw_feature(&mut pixmap, feature, &mb, w, h, scale);
    }
    crate::render::hatch::draw_hatch_pass(&mut pixmap, features, &mb, w, h, hatch_color);

    pixmap.take()
}

/// Premultiplied RGBA — see the module note above.
pub fn rasterize_spc_discussions(
    discussions: &[SpcDiscussion],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    device_scale: f32,
) -> Vec<u8> {
    let scale = sane_device_scale(device_scale);
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
                // Every ring of an MD is drawn as its own filled polygon, so
                // there are no holes to honour here.
                fill_path(&mut pixmap, &path, fill_rgba, FillRule::Winding);
                let sw = scaled_stroke_width(&path, 2.0, scale);
                stroke_path(&mut pixmap, &path, stroke_rgba, sw);
            }
        }
    }

    pixmap.take()
}

/// Renders only alerts in `enabled_categories` and not in `hidden_ids`.
///
/// Premultiplied RGBA — see the module note above.
pub fn rasterize_nws_alerts(
    alerts: &[NwsAlert],
    enabled_categories: &[AlertCategory],
    hidden_ids: &HashSet<String>,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    device_scale: f32,
) -> Vec<u8> {
    let scale = sane_device_scale(device_scale);
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
            draw_feature(&mut pixmap, feature, &mb, w, h, scale);
        }
    }

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

/// [`RasterizeOutput`] and not a bare buffer, unlike its neighbours, because
/// this is the one rasterizer whose caller is not an
/// [`OverlayHandler`](crate::render::overlay_state::OverlayHandler): `app_fetch`
/// invokes it directly, so there is no handler in between to state the alpha
/// convention on its behalf. Returning the mode with the bytes is what keeps
/// that call site from having to know it.
pub fn rasterize_radar_sites(
    sites: &[RadarSiteInfo],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    zoom: f64,
    is_dark: bool,
    device_scale: f32,
) -> RasterizeOutput {
    let scale = sane_device_scale(device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_radar_sites ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_map: None,
            alpha: AlphaMode::Premultiplied,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    let zoom_f32 = zoom as f32;
    // Chosen in points and converted to texels, rather than chosen in texels:
    // the clamps are what a dot should look like on a screen, not how many
    // samples of one it takes.
    let radius = ((5.0 + zoom_f32).clamp(4.0, 12.0)).max(1.0) * scale;
    let stroke_w = (radius * 0.3).clamp(0.5 * scale, 2.0 * scale);

    let text_bg = if is_dark {
        Color::from_rgba8(0, 0, 0, 140)
    } else {
        Color::from_rgba8(255, 255, 255, 140)
    };

    for site in sites {
        let (px, py) = mb.project(site.lat, site.lon, w, h);
        // 50 points of slack so a site just off-texture still contributes its
        // label. In texels here, so the ground it stands for does not shrink
        // with density.
        let slack = 50.0 * scale;
        if px < -slack || px > w + slack || py < -slack || py > h + slack {
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
            // egui draws the glyphs over this pill at a fixed *point* size, so
            // the pill has to be that many points wide in texels or the text
            // overflows the background it is drawn on.
            let label_w = (site.name.len() as f32 * 5.5 + 4.0) * scale;
            let label_h = 10.0 * scale;
            let lx = px - label_w / 2.0;
            let ly = py + radius + 2.0 * scale;
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

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_map: None,
        alpha: AlphaMode::Premultiplied,
    }
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
    ctx: &crate::render::overlay_state::RasterizeContext,
) -> RasterizeOutput {
    let (zoom, is_dark) = (ctx.zoom, ctx.is_dark);
    let scale = sane_device_scale(ctx.device_scale);
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        log::error!(
            "Pixmap allocation failed in rasterize_storm_reports ({}×{})",
            width,
            height
        );
        return RasterizeOutput {
            rgba: vec![0u8; (width * height * 4) as usize],
            hit_map: None,
            alpha: AlphaMode::Premultiplied,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    let zoom_f32 = zoom as f32;
    let radius = (3.0 + zoom_f32 * 0.5).clamp(3.0, 10.0) * scale;
    let stroke_w = (radius * 0.3).clamp(0.5 * scale, 2.0 * scale);
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
        let slack = 20.0 * scale;
        if px < -slack || px > w + slack || py < -slack || py > h + slack {
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

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_map: Some(hit_map),
        alpha: AlphaMode::Premultiplied,
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
    /// Texels per logical point — see
    /// `crate::render::overlay_state::RasterizeContext::device_scale`.
    pub device_scale: f32,
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
            alpha: AlphaMode::Premultiplied,
        };
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;
    let mut hit_map = HitMap::new(width, height);

    // ~12 points at zoom 6, clamped to 6-20 points, then taken into texels.
    let zoom_f32 = params.zoom as f32;
    let base_size = (zoom_f32 * 2.0).clamp(6.0, 20.0) * sane_device_scale(params.device_scale);

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

    RasterizeOutput {
        rgba: pixmap.take(),
        hit_map: Some(hit_map),
        alpha: AlphaMode::Premultiplied,
    }
}

// ── Feature rendering ────────────────────────────────────────────────────

fn draw_feature(
    pixmap: &mut Pixmap,
    feature: &OverlayFeature,
    mb: &MercatorBounds,
    w: f32,
    h: f32,
    scale: f32,
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
        let Some(projected) = project_polygon(polygon, mb, w, h) else {
            continue;
        };
        if let Some((path, rule)) = build_filled_polygon_path(&projected.exterior, &projected.holes)
        {
            fill_path(pixmap, &path, feature.fill_rgba, rule);
            if feature.stroke_rgba[3] > 0 {
                // The path carries the holes as subpaths, so this outlines
                // them too: a hole's edge is as much a boundary of the feature
                // as the exterior is.
                let sw = scaled_stroke_width(&path, 1.5, scale);
                stroke_path(pixmap, &path, feature.stroke_rgba, sw);
            }
        }
    }
}

// ── Path building helpers ────────────────────────────────────────────────

/// Thins the stroke below a 40-point minimum dimension, so a small polygon is
/// not swallowed by its own outline. `base` is the width at close zoom, in
/// points; `scale` is texels per point.
///
/// Both the threshold and the clamps are in points and converted here, and the
/// middle term needs no conversion at all: `min_dim` grows with density and the
/// threshold grows with it, so the ratio is already right and only the bounds
/// it is held between have to move. Left unscaled, a polygon large enough to
/// take the full `base` would be outlined at `base` texels — half a point's
/// width on a 2x display — which is the case this whole parameter exists for,
/// since most polygons on screen are the large ones.
fn scaled_stroke_width(path: &tiny_skia::Path, base: f32, scale: f32) -> f32 {
    let b = path.bounds();
    let min_dim = b.width().min(b.height());
    (min_dim / 40.0 * base).clamp(0.5 * scale, base * scale)
}

/// A device scale that can be multiplied by, from one that may not be.
///
/// The value crosses a public API and reaches a marker radius and a
/// `tiny_skia::Rect`, where a zero draws nothing at all and a `NaN` makes
/// `Rect::from_xywh` return `None` — both of which are an overlay that silently
/// stops painting rather than an error anyone can read. Below one texel per
/// point there is nothing to gain and a sub-point marker to lose, so that is
/// the floor.
fn sane_device_scale(device_scale: f32) -> f32 {
    if device_scale.is_finite() {
        device_scale.max(1.0)
    } else {
        1.0
    }
}

/// Interior rings below this projected area (square pixels) are dropped.
///
/// They are not rare and they are not hypothetical: RDP simplification
/// (`SIMPLIFY_EPSILON`) collapses a small closed ring to a retracing
/// out-and-back, and 2,515 of the 4,579 interior rings in a full 7,015-zone
/// cache have exactly zero area — every one of them a three-point ring whose
/// shoelace terms cancel pairwise. Such a ring encloses nothing, so even-odd
/// already ignores it for the fill — but it is still a subpath, and the stroke
/// would draw every one of them as a hairline scratch across the zone.
const MIN_HOLE_AREA_PX: f32 = 0.25;

/// Interior rings thinner than this *on average* — twice the area over the
/// perimeter — are dropped as well.
///
/// Area alone does not catch what it was written to catch. A ring can clear
/// [`MIN_HOLE_AREA_PX`] and still be a scratch: the worst in that same cache
/// (`forecast_PKZ785`) spreads 0.39 px² over 31.1 px of rim, so it is a fortieth
/// of a pixel wide and 15 px long. Even-odd removes a fortieth of a pixel of
/// alpha, which nobody can see; the stroke then draws its rim at full opacity,
/// which is a visible line across a zone that has no visible hole in it. The
/// two tests together are the claim: a ring is kept only if it is both big
/// enough and thick enough to be *seen* as a hole, not merely outlined as one.
///
/// A quarter of a pixel, measured: it drops 78 of the 2,040 rings that clear
/// the area floor and keeps 1,962. The largest ring it drops is 4.6 px² spread
/// over 41 px of rim; the largest it keeps anywhere near the boundary is
/// 20 px² over 145 px (`forecast_GMZ237`, 0.276 px wide), a cut-out big enough
/// to see. Every ring it drops was invisible before this change too, which
/// never drew an interior ring at all — so this can only decline to add a
/// scratch, never remove a hole the map has been showing.
const MIN_HOLE_WIDTH_PX: f32 = 0.25;

/// Unsigned shoelace area, so it says nothing about which way the ring winds.
fn ring_area_px(pts: &[(f32, f32)]) -> f32 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut twice = 0.0;
    for i in 0..n {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % n];
        twice += x1 * y2 - x2 * y1;
    }
    (twice * 0.5).abs()
}

/// Closed length of the ring, so the last vertex joins back to the first.
fn ring_perimeter_px(pts: &[(f32, f32)]) -> f32 {
    let n = pts.len();
    if n < 2 {
        return 0.0;
    }
    let mut total = 0.0;
    for i in 0..n {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % n];
        total += ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
    }
    total
}

/// Whether an interior ring is worth cutting *and outlining* at the size it is
/// drawn. See [`MIN_HOLE_AREA_PX`] and [`MIN_HOLE_WIDTH_PX`].
pub(crate) fn hole_is_drawable(pts: &[(f32, f32)]) -> bool {
    let area = ring_area_px(pts);
    let perimeter = ring_perimeter_px(pts);
    // `2 * area >= width * perimeter` rather than a division, so a ring of zero
    // perimeter cannot produce a NaN. It cannot reach here anyway: zero
    // perimeter means zero area, which the first test already rejects.
    area >= MIN_HOLE_AREA_PX && 2.0 * area >= MIN_HOLE_WIDTH_PX * perimeter
}

/// A GeoJSON polygon projected to texture pixels: the exterior ring, and the
/// interior rings that [`hole_is_drawable`] keeps.
///
/// One definition of "where this polygon's interior is", so the fill
/// ([`draw_feature`]) and the hatch mask ([`crate::render::hatch`]) cannot
/// answer differently — which they did while only the fill honoured holes, the
/// hatch painting straight across a hole the fill had just cut.
pub(crate) struct ProjectedPolygon {
    pub(crate) exterior: Vec<(f32, f32)>,
    pub(crate) holes: Vec<Vec<(f32, f32)>>,
}

/// `None` when the exterior ring is too short to enclose anything.
pub(crate) fn project_polygon(
    polygon: &[GeoPolygonRing],
    mb: &MercatorBounds,
    w: f32,
    h: f32,
) -> Option<ProjectedPolygon> {
    let exterior_ring = polygon.first()?;
    if exterior_ring.len() < 3 {
        return None;
    }
    let project = |ring: &[(f64, f64)]| -> Vec<(f32, f32)> {
        strip_closing_dup(ring)
            .iter()
            .map(|&(lat, lon)| mb.project(lat, lon, w, h))
            .collect()
    };
    // `polygon[1..]` are interior rings — holes. Dropping them painted a donut
    // as a solid blob while `geo_point_in_feature` counted the hole as outside,
    // so a click in the cut-out of a marine zone or a nested SPC outlook hit
    // nothing the eye could see was not there.
    let holes = polygon[1..]
        .iter()
        .filter(|ring| ring.len() >= 3)
        .map(|ring| project(ring))
        .filter(|pts| hole_is_drawable(pts))
        .collect();
    Some(ProjectedPolygon {
        exterior: project(exterior_ring),
        holes,
    })
}

/// One closed subpath. Repeated calls on the same builder produce the
/// multi-subpath path a polygon with holes needs.
fn push_ring(pb: &mut PathBuilder, pts: &[(f32, f32)]) {
    pb.move_to(pts[0].0, pts[0].1);
    for &(x, y) in &pts[1..] {
        pb.line_to(x, y);
    }
    pb.close();
}

/// `None` for degenerate paths, which tiny-skia cannot fill.
pub(crate) fn build_polygon_path(pts: &[(f32, f32)]) -> Option<tiny_skia::Path> {
    if pts.len() < 3 {
        return None;
    }
    let mut pb = PathBuilder::new();
    push_ring(&mut pb, pts);
    let path = pb.finish()?;
    let b = path.bounds();
    if b.width() < 0.1 || b.height() < 0.1 {
        return None;
    }
    Some(path)
}

/// A whole GeoJSON polygon — exterior ring plus its interior rings — as one
/// path, with the fill rule that path must be filled under.
///
/// **Even-odd, because that is what the hit test computes.**
/// `geo_point_in_feature` counts ray crossings per ring and never looks at
/// orientation, so even-odd is the rule that makes painted pixels and clicks
/// answer the same question. `Winding` would instead punch a hole only where
/// the interior ring runs against its exterior.
///
/// That distinction is not academic even though the producers mostly behave.
/// Measured over a full 7,015-zone NWS cache and eight archived SPC outlooks:
/// of 2,064 interior rings with any area at all, 2,062 wind against their
/// exterior as GeoJSON's right-hand rule asks — and two do not, so `Winding`
/// would leave those two solid. The other 2,515 interior rings in that cache
/// have *zero* area after simplification and so no orientation for a
/// winding rule to consult (see `MIN_HOLE_AREA_PX`). A rule that must ask
/// which way a ring turns has, for more than half of this data, nothing to
/// ask.
///
/// The exterior alone keeps `Winding`, the rule it has always been filled
/// under. The two rules agree on any simple ring, but RDP simplification
/// leaves 811 of the 17,306 rings in that cache (4.7%) self-intersecting, and
/// some of those enclose a doubly-wound region that even-odd would drop — 400
/// uniform samples of each such ring's bounding box find one in 46 of the 811.
/// Sampling can only under-count a lobe it never lands in, so 46 is a floor and
/// not a fraction. Holes are this change's business; re-cutting hole-free
/// coastlines is not.
///
/// **What that deferral costs.** 694 of the 12,727 exterior rings in that cache
/// (5.5%) self-intersect, and wherever such a ring carries no hole its fill
/// still reads `Winding` while `geo_point_in_feature` reads it even-odd: the
/// same paint/click disagreement this commit closes for holes, left open for
/// self-intersection. It is smaller — the two rules differ only on the
/// doubly-wound lobes an RDP crossing leaves behind, not on a whole enclave —
/// but it is the same bug, and it is still here.
///
/// **A second residual, of the same family.** RDP can also leave two interior
/// rings of one polygon overlapping: 99 such pairs in that cache, across 29
/// polygons, none nested, ~70 px² of overlap in total. Even-odd paints the
/// overlap (two hole crossings plus the exterior is odd) while the hit test's
/// `any(hole contains)` calls it outside. That is a strict improvement on what
/// this function did before — the region used to be painted along with both
/// holes, and is now painted alone — so it is recorded, not fixed.
pub(crate) fn build_filled_polygon_path(
    exterior: &[(f32, f32)],
    holes: &[Vec<(f32, f32)>],
) -> Option<(tiny_skia::Path, FillRule)> {
    // The degeneracy gate stays on the exterior, so a polygon that used to
    // draw still draws no matter what its holes look like — and so the
    // hole-free path below is bit-for-bit the one this function has always
    // returned.
    let exterior_path = build_polygon_path(exterior)?;
    if holes.is_empty() {
        return Some((exterior_path, FillRule::Winding));
    }
    let mut pb = PathBuilder::new();
    pb.push_path(&exterior_path);
    for hole in holes {
        if hole.len() >= 3 {
            push_ring(&mut pb, hole);
        }
    }
    let path = pb.finish()?;
    Some((path, FillRule::EvenOdd))
}

fn fill_path(pixmap: &mut Pixmap, path: &tiny_skia::Path, rgba: [u8; 4], rule: FillRule) {
    if rgba[3] == 0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    paint.anti_alias = true;
    pixmap.fill_path(path, &paint, rule, Transform::identity(), None);
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
            alpha: AlphaMode::Straight,
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
            alpha: AlphaMode::Straight,
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
        alpha: AlphaMode::Straight,
    }
}

#[cfg(test)]
mod alpha_tests;

#[cfg(test)]
mod glm_energy_tests;

#[cfg(test)]
mod hole_tests;

#[cfg(test)]
pub(crate) mod lambert_fixture;

#[cfg(test)]
mod model_nan_tests;

#[cfg(test)]
mod projection_window_tests;

#[cfg(test)]
mod device_scale_tests;
