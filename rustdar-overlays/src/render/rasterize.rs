//! Rasterize overlay polygons to RGBA textures using tiny-skia.
//!
//! This module renders overlay features (SPC outlooks, NWS alerts, mesoscale
//! discussions) into RGBA pixel buffers suitable for upload as egui textures.
//! Rendering runs on a background thread; the resulting texture is displayed
//! as a geo-positioned image on the map — identical to how radar images work.

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

use tiny_skia::{
    Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform,
};

use crate::nws::alert::{AlertCategory, NwsAlert};
use crate::render::overlay_state::SelectedOverlay;
use crate::spc::colors::{md_fill_color, md_stroke_color};
use crate::spc::discussion::SpcDiscussion;
use crate::spc::reports::{StormReport, StormReportKind};
use crate::types::{GeoBounds, OverlayFeature};

// ── Hit buffer types ─────────────────────────────────────────────────────

/// Quarter-resolution hit buffer produced during rasterization.
///
/// Maps pixel regions to overlay item IDs so that click detection is
/// pixel-perfect — the exact same pixels the rasterizer drew are clickable.
/// Resolution is 1/4 of the texture in each axis to keep memory low.
#[derive(Clone)]
pub struct HitMap {
    /// Quarter-resolution width.
    pub width: u32,
    /// Quarter-resolution height.
    pub height: u32,
    /// Sparse map: quarter-res pixel index (`qy * width + qx`) → list of
    /// item IDs that cover that cell. Only occupied cells are stored.
    cells: HashMap<u32, Vec<u32>>,
    /// Maps item IDs used in `cells` to their `SelectedOverlay` values.
    id_map: HashMap<u32, SelectedOverlay>,
}

impl HitMap {
    /// Create an empty hit map with the given quarter-resolution dimensions.
    pub fn new(full_width: u32, full_height: u32) -> Self {
        Self {
            width: (full_width + 3) / 4,
            height: (full_height + 3) / 4,
            cells: HashMap::new(),
            id_map: HashMap::new(),
        }
    }

    /// Register that `item_id` covers the full-resolution pixel `(px, py)`.
    ///
    /// Quantizes to quarter-res and records the ID in the sparse cell map.
    pub fn record(&mut self, px: f32, py: f32, item_id: u32) {
        let qx = (px as u32) / 4;
        let qy = (py as u32) / 4;
        if qx >= self.width || qy >= self.height {
            return;
        }
        let idx = qy * self.width + qx;
        let ids = self.cells.entry(idx).or_insert_with(|| Vec::with_capacity(1));
        if !ids.contains(&item_id) {
            ids.push(item_id);
        }
    }

    /// Register the `SelectedOverlay` for a given item ID.
    pub fn register_id(&mut self, item_id: u32, selected: SelectedOverlay) {
        self.id_map.insert(item_id, selected);
    }

    /// Look up all overlay items at texture UV coordinates `(u, v)` in `[0, 1]`.
    pub fn hit_test(&self, u: f32, v: f32) -> Vec<&SelectedOverlay> {
        if u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0 {
            return Vec::new();
        }
        let qx = ((u * self.width as f32) as u32).min(self.width.saturating_sub(1));
        let qy = ((v * self.height as f32) as u32).min(self.height.saturating_sub(1));
        let idx = qy * self.width + qx;
        self.cells
            .get(&idx)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.id_map.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Output from a rasterization function: RGBA pixels plus an optional hit buffer.
pub struct RasterizeOutput {
    /// RGBA pixel data (`width × height × 4` bytes).
    pub rgba: Vec<u8>,
    /// Optional hit buffer for pixel-perfect click detection.
    pub hit_map: Option<HitMap>,
}

// ── Mercator projection helpers ──────────────────────────────────────────

/// Convert latitude (radians) to Web Mercator Y.
#[inline]
fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

/// Maximum latitude for Web Mercator projection (≈85.05°).
const MAX_MERCATOR_LAT: f64 = 85.05;

/// Mercator bounds for the texture: lat/lon extent plus pre-computed Mercator Y.
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

    /// Project a (lat, lon) pair to pixel coordinates within the texture.
    #[inline]
    pub(crate) fn project(&self, lat: f64, lon: f64, w: f32, h: f32) -> (f32, f32) {
        let lon_frac = (lon - self.min_lon) / (self.max_lon - self.min_lon);
        let merc_y = lat_rad_to_mercator_y(lat.to_radians());
        let merc_frac = (merc_y - self.merc_y_min) / (self.merc_y_max - self.merc_y_min);
        let px = (lon_frac * w as f64) as f32;
        // Y axis is inverted (top = north = max mercator Y)
        let py = ((1.0 - merc_frac) * h as f64) as f32;
        (px, py)
    }
}

// ── Public API ───────────────────────────────────────────────────────────

/// Rasterize SPC outlook features to an RGBA texture.
///
/// `hatch_color` is the [R,G,B,A] used for CIG hatch lines (theme-dependent).
pub fn rasterize_spc_outlooks(
    features: &[OverlayFeature],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    hatch_color: [u8; 4],
) -> Vec<u8> {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        return vec![0u8; (width * height * 4) as usize];
    };
    let mb = MercatorBounds::from_geo(bounds);
    let w = width as f32;
    let h = height as f32;

    // Pass 1: fill + stroke all polygons
    for feature in features {
        draw_feature(&mut pixmap, feature, &mb, w, h);
    }

    // Pass 2: CIG hatch lines with exclusion
    crate::render::hatch::draw_hatch_pass(&mut pixmap, features, &mb, w, h, hatch_color);

    premultiplied_to_straight(pixmap.data_mut());
    pixmap.take()
}

/// Rasterize SPC Mesoscale Discussion polygons to an RGBA texture.
pub fn rasterize_spc_discussions(
    discussions: &[SpcDiscussion],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
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
            let pts: Vec<(f32, f32)> = ring.iter().map(|&(lat, lon)| mb.project(lat, lon, w, h)).collect();
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

/// Rasterize NWS alert polygons to an RGBA texture.
///
/// Only alerts whose category is in `enabled_categories` and whose ID is not
/// in `hidden_ids` are rendered.
pub fn rasterize_nws_alerts(
    alerts: &[NwsAlert],
    enabled_categories: &[AlertCategory],
    hidden_ids: &HashSet<String>,
    bounds: &GeoBounds,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
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

/// Radar site descriptor for texture rasterization (decoupled from `rustdar-radar`).
pub struct RadarSiteInfo {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub is_current: bool,
    pub is_loading: bool,
}

/// Rasterize NEXRAD radar site markers to an RGBA texture.
///
/// Draws filled circles with white outlines and site-name labels for each site
/// visible within the given geo bounds.  Current/loading sites are colour-coded.
pub fn rasterize_radar_sites(
    sites: &[RadarSiteInfo],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    zoom: f64,
    is_dark: bool,
) -> Vec<u8> {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
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
        // Skip sites outside the texture (with margin for label)
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

        // Filled circle
        let mut pb = PathBuilder::new();
        pb.push_circle(px, py, radius);
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(fill);
            paint.anti_alias = true;
            pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);

            // White outline
            paint.set_color(Color::from_rgba8(255, 255, 255, 255));
            let stroke = Stroke { width: stroke_w, ..Stroke::default() };
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }

        // Label below the circle — tiny-skia cannot render text, so draw a
        // small background pill and the label is handled per-frame by egui.
        // Only draw the background pill at higher zoom levels where labels show.
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
                pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
    }

    premultiplied_to_straight(pixmap.data_mut());
    pixmap.take()
}

// ── Storm report symbol helpers ───────────────────────────────────────

/// Draw a tornado funnel symbol (inverted triangle) inside the circle.
fn draw_tornado_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let s = r * 0.6; // symbol half-size
    let mut pb = PathBuilder::new();
    // Inverted triangle: wide at top, narrow at bottom
    pb.move_to(px - s, py - s * 0.7);
    pb.line_to(px + s, py - s * 0.7);
    pb.line_to(px, py + s * 0.9);
    pb.close();
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Draw a hail symbol (small filled circle with 4 radiating ticks).
fn draw_hail_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let core = r * 0.3;
    let tick_inner = r * 0.35;
    let tick_outer = r * 0.65;
    let stroke_w = (r * 0.18).clamp(0.5, 1.5);

    // Small filled circle in center
    let mut pb = PathBuilder::new();
    pb.push_circle(px, py, core);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    // 4 radiating ticks at 45° angles
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = Stroke { width: stroke_w, line_cap: LineCap::Round, ..Stroke::default() };
    let diag = std::f32::consts::FRAC_1_SQRT_2;
    let offsets: [(f32, f32); 4] = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)];
    for (dx, dy) in offsets {
        // Use diagonal offsets for a spiky look
        let (adx, ady) = if dx.abs() > 0.5 { (dx, diag * dy.signum().max(0.3)) } else { (diag * dx.signum().max(0.3), dy) };
        let _ = ady; let _ = adx;
        let mut pb = PathBuilder::new();
        pb.move_to(px + dx * tick_inner, py + dy * tick_inner);
        pb.line_to(px + dx * tick_outer, py + dy * tick_outer);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
}

/// Draw a wind symbol (right-pointing chevron/arrow).
fn draw_wind_symbol(pixmap: &mut Pixmap, px: f32, py: f32, r: f32, color: Color) {
    let s = r * 0.55;
    let stroke_w = (r * 0.22).clamp(0.5, 2.0);

    let mut pb = PathBuilder::new();
    // Chevron: top-left to center-right to bottom-left
    pb.move_to(px - s * 0.5, py - s);
    pb.line_to(px + s * 0.5, py);
    pb.line_to(px - s * 0.5, py + s);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        let stroke = Stroke { width: stroke_w, line_cap: LineCap::Round, ..Stroke::default() };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Rasterize SPC storm report markers to an RGBA texture.
///
/// Draws circles with weather-type symbols at each report location,
/// colour-coded by type: tornado = red, hail = green, wind = blue.
/// At low zoom (small radius), falls back to filled dots.
pub fn rasterize_storm_reports(
    reports: &[StormReport],
    bounds: &GeoBounds,
    width: u32,
    height: u32,
    zoom: f64,
    is_dark: bool,
) -> RasterizeOutput {
    let Some(mut pixmap) = Pixmap::new(width, height) else {
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
    // Hit radius includes the stroke outline for generous click targets.
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
            let mut paint = Paint::default();
            paint.anti_alias = true;

            if use_symbol {
                // Outline-only circle with symbol inside
                paint.set_color(fill);
                let stroke = Stroke { width: stroke_w, ..Stroke::default() };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);

                match report.kind {
                    StormReportKind::Tornado => draw_tornado_symbol(&mut pixmap, px, py, radius, fill),
                    StormReportKind::Hail => draw_hail_symbol(&mut pixmap, px, py, radius, fill),
                    StormReportKind::Wind => draw_wind_symbol(&mut pixmap, px, py, radius, fill),
                }
            } else {
                // Filled dot at low zoom (symbol too small to read)
                paint.set_color(fill);
                pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);

                paint.set_color(outline);
                let stroke = Stroke { width: stroke_w, ..Stroke::default() };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }

        // Record hit cells for all quarter-res pixels within the hit radius.
        let item_id = idx as u32;
        hit_map.register_id(item_id, SelectedOverlay::StormReport { index: idx });
        let min_x = (px - hit_radius).max(0.0) as i32;
        let max_x = ((px + hit_radius) as i32).min(width as i32 - 1);
        let min_y = (py - hit_radius).max(0.0) as i32;
        let max_y = ((py + hit_radius) as i32).min(height as i32 - 1);
        let r2 = hit_radius * hit_radius;
        // Step by 4 since we only need one sample per quarter-res cell.
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

// ── Feature rendering ────────────────────────────────────────────────────

/// Draw a single overlay feature (fill + stroke for each polygon).
fn draw_feature(pixmap: &mut Pixmap, feature: &OverlayFeature, mb: &MercatorBounds, w: f32, h: f32) {
    // Geo-AABB cull: skip features entirely outside the texture bounds
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
        let Some(exterior) = polygon.first() else { continue };
        if exterior.len() < 3 {
            continue;
        }
        let ring = strip_closing_dup(exterior);
        let pts: Vec<(f32, f32)> = ring.iter().map(|&(lat, lon)| mb.project(lat, lon, w, h)).collect();
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

/// Compute a stroke width that scales down when the polygon is small on screen.
/// `base` is the desired width at close zoom; the result is clamped to [0.5, base].
fn scaled_stroke_width(path: &tiny_skia::Path, base: f32) -> f32 {
    let b = path.bounds();
    let min_dim = b.width().min(b.height());
    // Below 40px diameter, start thinning the stroke proportionally
    (min_dim / 40.0 * base).clamp(0.5, base)
}

/// Build a closed polygon path from projected screen points.
/// Returns `None` if the path is degenerate (too few points or zero area).
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
    // Skip degenerate paths that tiny-skia cannot fill
    let b = path.bounds();
    if b.width() < 0.1 || b.height() < 0.1 {
        return None;
    }
    Some(path)
}

/// Fill a path with the given RGBA color.
fn fill_path(pixmap: &mut Pixmap, path: &tiny_skia::Path, rgba: [u8; 4]) {
    if rgba[3] == 0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    paint.anti_alias = true;
    pixmap.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

/// Stroke a path with the given RGBA color and width.
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

/// Strip GeoJSON closing duplicate (last == first) from a ring.
pub(crate) fn strip_closing_dup(ring: &[(f64, f64)]) -> &[(f64, f64)] {
    if ring.len() > 3 && ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    }
}

/// Convert Mercator Y back to latitude (degrees).
fn merc_y_to_lat(merc_y: f64) -> f64 {
    (2.0 * merc_y.exp().atan() - PI / 2.0).to_degrees()
}

/// Convert premultiplied RGBA (tiny-skia's format) to straight alpha (egui's format).
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
