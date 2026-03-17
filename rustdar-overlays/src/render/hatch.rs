use tiny_skia::{
    Color, FillRule, LineCap, Mask, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform,
};

use crate::types::{HatchPattern, OverlayFeature};
use crate::render::rasterize::{MercatorBounds, build_polygon_path, strip_closing_dup};

/// Spacing between hatch lines in pixels (matches existing HATCH_SPACING).
const HATCH_SPACING: f32 = 10.0;

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

/// Draw CIG hatch lines across all features, respecting exclusion zones.
pub(crate) fn draw_hatch_pass(
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
