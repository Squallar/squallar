//! Rasterize overlay polygons to RGBA textures using tiny-skia.
//!
//! This module renders overlay features (SPC outlooks, NWS alerts, mesoscale
//! discussions) into RGBA pixel buffers suitable for upload as egui textures.
//! Rendering runs on a background thread; the resulting texture is displayed
//! as a geo-positioned image on the map — identical to how radar images work.

use std::collections::HashSet;
use std::f64::consts::PI;

use tiny_skia::{
    Color, FillRule, LineCap, Mask, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform,
};

use crate::nws::alert::{AlertCategory, NwsAlert};
use crate::spc::colors::{md_fill_color, md_stroke_color};
use crate::spc::discussion::SpcDiscussion;
use crate::types::{GeoBounds, HatchPattern, OverlayFeature};

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
struct MercatorBounds {
    min_lon: f64,
    max_lon: f64,
    merc_y_min: f64, // south edge
    merc_y_max: f64, // north edge
}

impl MercatorBounds {
    fn from_geo(bounds: &GeoBounds) -> Self {
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
    fn project(&self, lat: f64, lon: f64, w: f32, h: f32) -> (f32, f32) {
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

/// Expand viewport geo bounds by an overdraw fraction in each direction.
pub fn compute_render_bounds(viewport: &GeoBounds, overdraw: f32) -> GeoBounds {
    let lat_range = viewport.max_lat - viewport.min_lat;
    let lon_range = viewport.max_lon - viewport.min_lon;
    let lat_margin = lat_range * overdraw as f64;
    let lon_margin = lon_range * overdraw as f64;
    GeoBounds {
        min_lat: viewport.min_lat - lat_margin,
        max_lat: viewport.max_lat + lat_margin,
        min_lon: viewport.min_lon - lon_margin,
        max_lon: viewport.max_lon + lon_margin,
    }
}

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
    draw_hatch_pass(&mut pixmap, features, &mb, w, h, hatch_color);

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

// ── Hatch rendering ──────────────────────────────────────────────────────

/// Draw CIG hatch lines across all features, respecting exclusion zones.
fn draw_hatch_pass(
    pixmap: &mut Pixmap,
    features: &[OverlayFeature],
    mb: &MercatorBounds,
    w: f32,
    h: f32,
    hatch_color: [u8; 4],
) {
    // Collect projected polygon points per CIG level for exclusion masks
    let mut cig2_pts: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cig3_pts: Vec<Vec<(f32, f32)>> = Vec::new();
    // (feature_idx, path, projected_pts) for each hatched polygon
    let mut all_hatched: Vec<(usize, tiny_skia::Path, Vec<(f32, f32)>)> = Vec::new();

    for (idx, feature) in features.iter().enumerate() {
        if feature.hatch == HatchPattern::None {
            continue;
        }
        for polygon in &feature.polygons {
            let Some(exterior) = polygon.first() else { continue };
            let ring = strip_closing_dup(exterior);
            let pts: Vec<(f32, f32)> = ring.iter().map(|&(lat, lon)| mb.project(lat, lon, w, h)).collect();
            if let Some(path) = build_polygon_path(&pts) {
                match feature.hatch {
                    HatchPattern::Cig2 => cig2_pts.push(pts.clone()),
                    HatchPattern::Cig3 => cig3_pts.push(pts.clone()),
                    _ => {}
                }
                all_hatched.push((idx, path, pts));
            }
        }
    }

    let pw = pixmap.width();
    let ph = pixmap.height();

    for (idx, poly_path, pts) in &all_hatched {
        let hatch = features[*idx].hatch;

        let Some(mut mask) = Mask::new(pw, ph) else { continue };

        let exclusion_pts: Vec<&[(f32, f32)]> = match hatch {
            HatchPattern::Cig1 => cig2_pts.iter().chain(cig3_pts.iter()).map(|v| v.as_slice()).collect(),
            HatchPattern::Cig2 => cig3_pts.iter().map(|v| v.as_slice()).collect(),
            _ => Vec::new(),
        };

        if exclusion_pts.is_empty() {
            mask.fill_path(poly_path, FillRule::EvenOdd, false, Transform::identity());
        } else {
            let Some(combined) = build_polygon_with_exclusions(pts, &exclusion_pts) else {
                continue;
            };
            mask.fill_path(&combined, FillRule::EvenOdd, false, Transform::identity());
        }

        draw_hatch_lines_clipped(pixmap, &mask, hatch, hatch_color, poly_path);
    }
}

/// Build a combined path: the polygon ring plus all exclusion rings.
/// Using EvenOdd fill rule, overlapping regions cancel out — exclusions become holes.
fn build_polygon_with_exclusions(
    polygon_pts: &[(f32, f32)],
    exclusions: &[&[(f32, f32)]],
) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();

    // Add the main polygon contour
    if polygon_pts.len() >= 3 {
        pb.move_to(polygon_pts[0].0, polygon_pts[0].1);
        for &(x, y) in &polygon_pts[1..] {
            pb.line_to(x, y);
        }
        pb.close();
    }

    // Add exclusion contours (EvenOdd rule makes them subtract)
    for pts in exclusions {
        if pts.len() < 3 {
            continue;
        }
        pb.move_to(pts[0].0, pts[0].1);
        for &(x, y) in &pts[1..] {
            pb.line_to(x, y);
        }
        pb.close();
    }

    pb.finish()
}

/// Generate and draw hatch lines within a clip mask.
fn draw_hatch_lines_clipped(
    pixmap: &mut Pixmap,
    clip: &Mask,
    pattern: HatchPattern,
    hatch_color: [u8; 4],
    poly_path: &tiny_skia::Path,
) {
    let bounds = poly_path.bounds();
    let min_x = bounds.left();
    let min_y = bounds.top();
    let max_x = bounds.right();
    let max_y = bounds.bottom();

    match pattern {
        HatchPattern::Cig1 => {
            draw_directional_hatch(pixmap, clip, 45.0, true, hatch_color, min_x, min_y, max_x, max_y);
        }
        HatchPattern::Cig2 => {
            draw_directional_hatch(pixmap, clip, 135.0, false, hatch_color, min_x, min_y, max_x, max_y);
        }
        HatchPattern::Cig3 => {
            draw_directional_hatch(pixmap, clip, 45.0, false, hatch_color, min_x, min_y, max_x, max_y);
            draw_directional_hatch(pixmap, clip, 135.0, false, hatch_color, min_x, min_y, max_x, max_y);
        }
        HatchPattern::None => {}
    }
}

/// Spacing between hatch lines in pixels (matches existing HATCH_SPACING).
const HATCH_SPACING: f32 = 10.0;

/// Draw parallel hatch lines at a given angle within a clip mask.
fn draw_directional_hatch(
    pixmap: &mut Pixmap,
    clip: &Mask,
    angle_degrees: f32,
    dotted: bool,
    color: [u8; 4],
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
) {
    let angle_rad = angle_degrees.to_radians();
    let dir_x = angle_rad.cos();
    let dir_y = -angle_rad.sin();
    let norm_x = -dir_y;
    let norm_y = dir_x;

    let corners = [
        (min_x, min_y), (max_x, min_y),
        (min_x, max_y), (max_x, max_y),
    ];

    let (mut min_proj, mut max_proj) = (f32::MAX, f32::MIN);
    let (mut min_dir, mut max_dir) = (f32::MAX, f32::MIN);
    for &(cx, cy) in &corners {
        let np = cx * norm_x + cy * norm_y;
        min_proj = min_proj.min(np);
        max_proj = max_proj.max(np);
        let dp = cx * dir_x + cy * dir_y;
        min_dir = min_dir.min(dp);
        max_dir = max_dir.max(dp);
    }
    let half_len = (max_dir - min_dir) * 0.5 + 10.0;
    let center = (min_dir + max_dir) * 0.5;

    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
    paint.anti_alias = true;

    let mut stroke = Stroke {
        width: if dotted { 1.5 } else { 1.0 },
        line_cap: LineCap::Butt,
        ..Stroke::default()
    };
    if dotted {
        stroke.dash = StrokeDash::new(vec![4.0, 4.0], 0.0);
    }

    let mut t = min_proj;
    while t <= max_proj {
        let cx = norm_x * t + dir_x * center;
        let cy = norm_y * t + dir_y * center;
        let x1 = cx - dir_x * half_len;
        let y1 = cy - dir_y * half_len;
        let x2 = cx + dir_x * half_len;
        let y2 = cy + dir_y * half_len;

        let mut pb = PathBuilder::new();
        pb.move_to(x1, y1);
        pb.line_to(x2, y2);
        if let Some(line_path) = pb.finish() {
            pixmap.stroke_path(&line_path, &paint, &stroke, Transform::identity(), Some(clip));
        }

        t += HATCH_SPACING;
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
fn build_polygon_path(pts: &[(f32, f32)]) -> Option<tiny_skia::Path> {
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
fn strip_closing_dup(ring: &[(f64, f64)]) -> &[(f64, f64)] {
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
