use egui::{Pos2};
use rustdar_overlays::types::HatchPattern;

use crate::geo;

/// Spacing between hatch lines in screen pixels.
const HATCH_SPACING: f32 = 10.0;

/// Generate hatch line segments for a polygon without drawing them.
///
/// Returns `(start, end, is_dotted)` tuples suitable for caching.
/// This avoids per-frame recomputation of scanline-polygon intersections.
///
/// `exclusion_polygons` are screen-space polygons from higher-severity CIG
/// areas. Any hatch segment portions that fall inside an exclusion polygon
/// are removed so that e.g. CIG1 hatching doesn't show through a CIG2 region.
pub fn generate_hatch_lines(
    polygon_pts: &[Pos2],
    pattern: HatchPattern,
    exclusion_polygons: &[&[Pos2]],
) -> Vec<(Pos2, Pos2, bool)> {
    if polygon_pts.len() < 3 || pattern == HatchPattern::None {
        return Vec::new();
    }

    let mut lines = Vec::new();
    match pattern {
        HatchPattern::Cig1 => {
            collect_directional_hatch(&mut lines, polygon_pts, 45.0, true);
        }
        HatchPattern::Cig2 => {
            collect_directional_hatch(&mut lines, polygon_pts, 135.0, false);
        }
        HatchPattern::Cig3 => {
            collect_directional_hatch(&mut lines, polygon_pts, 45.0, false);
            collect_directional_hatch(&mut lines, polygon_pts, 135.0, false);
        }
        HatchPattern::None => {}
    }

    if !exclusion_polygons.is_empty() {
        subtract_exclusion_polygons(&mut lines, exclusion_polygons);
    }

    lines
}

/// Remove portions of hatch segments that fall inside any exclusion polygon.
fn subtract_exclusion_polygons(
    lines: &mut Vec<(Pos2, Pos2, bool)>,
    exclusions: &[&[Pos2]],
) {
    let mut result = Vec::with_capacity(lines.len());
    for &(p1, p2, dotted) in lines.iter() {
        let mut segments = vec![(p1, p2)];
        for &excl_poly in exclusions {
            if excl_poly.len() < 3 {
                continue;
            }
            // Quick AABB reject per exclusion polygon
            let seg_min_x = segments.iter().map(|(a, b)| a.x.min(b.x)).fold(f32::MAX, f32::min);
            let seg_max_x = segments.iter().map(|(a, b)| a.x.max(b.x)).fold(f32::MIN, f32::max);
            let seg_min_y = segments.iter().map(|(a, b)| a.y.min(b.y)).fold(f32::MAX, f32::min);
            let seg_max_y = segments.iter().map(|(a, b)| a.y.max(b.y)).fold(f32::MIN, f32::max);
            let (ex_min_x, ex_min_y, ex_max_x, ex_max_y) = geo::aabb(excl_poly);
            if seg_max_x < ex_min_x || seg_min_x > ex_max_x
                || seg_max_y < ex_min_y || seg_min_y > ex_max_y
            {
                continue;
            }

            let mut next_segments = Vec::with_capacity(segments.len());
            for (s1, s2) in segments {
                subtract_polygon_from_segment(s1, s2, excl_poly, &mut next_segments);
            }
            segments = next_segments;
            if segments.is_empty() {
                break;
            }
        }
        for (s1, s2) in segments {
            result.push((s1, s2, dotted));
        }
    }
    *lines = result;
}

/// Subtract a single polygon from a line segment, outputting remaining portions.
fn subtract_polygon_from_segment(
    p1: Pos2,
    p2: Pos2,
    polygon: &[Pos2],
    out: &mut Vec<(Pos2, Pos2)>,
) {
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-6 {
        return;
    }

    // Find all intersection t-values with polygon edges
    let n = polygon.len();
    let mut ts = Vec::new();
    for i in 0..n {
        let a = polygon[i];
        let b = polygon[(i + 1) % n];
        let ex = b.x - a.x;
        let ey = b.y - a.y;
        let denom = dx * ey - dy * ex;
        if denom.abs() < 1e-10 {
            continue;
        }
        let t = ((a.x - p1.x) * ey - (a.y - p1.y) * ex) / denom;
        let s = ((a.x - p1.x) * dy - (a.y - p1.y) * dx) / denom;
        if s >= 0.0 && s <= 1.0 && t >= 0.0 && t <= 1.0 {
            ts.push(t);
        }
    }
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);

    // Build intervals. Check inside/outside at each sub-segment midpoint.
    let mut boundaries = vec![0.0_f32];
    boundaries.extend_from_slice(&ts);
    boundaries.push(1.0);

    for w in boundaries.windows(2) {
        let t0 = w[0];
        let t1 = w[1];
        if (t1 - t0) < 1e-6 {
            continue;
        }
        let mid_t = (t0 + t1) * 0.5;
        let mid = Pos2::new(p1.x + dx * mid_t, p1.y + dy * mid_t);
        if !geo::point_in_polygon(mid, polygon) {
            // This portion is outside the exclusion polygon — keep it
            out.push((
                Pos2::new(p1.x + dx * t0, p1.y + dy * t0),
                Pos2::new(p1.x + dx * t1, p1.y + dy * t1),
            ));
        }
    }
}



/// Pre-computed parameters for sweeping hatch scanlines across a polygon's AABB.
struct ScanlineParams {
    /// Direction vector along the hatch line.
    dir_x: f32,
    dir_y: f32,
    /// Normal vector (perpendicular to hatch direction).
    norm_x: f32,
    norm_y: f32,
    /// Projection range along the normal — sweep `t` from `min_proj` to `max_proj`.
    min_proj: f32,
    max_proj: f32,
    /// Half-length of each scanline in the direction axis.
    line_half_len: f32,
    /// Center projection along the direction axis.
    dir_center: f32,
}

impl ScanlineParams {
    /// Compute scanline sweep parameters from a polygon's AABB and hatch angle.
    fn new(polygon_pts: &[Pos2], angle_degrees: f32) -> Option<Self> {
        let (min_x, min_y, max_x, max_y) = geo::aabb(polygon_pts);
        if min_x >= max_x || min_y >= max_y {
            return None;
        }

        let angle_rad = angle_degrees.to_radians();
        let dir_x = angle_rad.cos();
        let dir_y = -angle_rad.sin();
        let norm_x = -dir_y;
        let norm_y = dir_x;

        let corners = [
            Pos2::new(min_x, min_y),
            Pos2::new(max_x, min_y),
            Pos2::new(min_x, max_y),
            Pos2::new(max_x, max_y),
        ];

        let (mut min_proj, mut max_proj) = (f32::MAX, f32::MIN);
        let (mut min_dir_proj, mut max_dir_proj) = (f32::MAX, f32::MIN);
        for c in &corners {
            let np = c.x * norm_x + c.y * norm_y;
            min_proj = min_proj.min(np);
            max_proj = max_proj.max(np);
            let dp = c.x * dir_x + c.y * dir_y;
            min_dir_proj = min_dir_proj.min(dp);
            max_dir_proj = max_dir_proj.max(dp);
        }

        Some(Self {
            dir_x,
            dir_y,
            norm_x,
            norm_y,
            min_proj,
            max_proj,
            line_half_len: (max_dir_proj - min_dir_proj) * 0.5 + 10.0,
            dir_center: (min_dir_proj + max_dir_proj) * 0.5,
        })
    }
}

/// Collect hatch line segments at a given angle, clipped to the polygon.
fn collect_directional_hatch(
    out: &mut Vec<(Pos2, Pos2, bool)>,
    polygon_pts: &[Pos2],
    angle_degrees: f32,
    dotted: bool,
) {
    let Some(params) = ScanlineParams::new(polygon_pts, angle_degrees) else {
        return;
    };

    let mut t = params.min_proj;
    while t <= params.max_proj {
        let cx = params.norm_x * t + params.dir_x * params.dir_center;
        let cy = params.norm_y * t + params.dir_y * params.dir_center;
        let p1 = Pos2::new(cx - params.dir_x * params.line_half_len, cy - params.dir_y * params.line_half_len);
        let p2 = Pos2::new(cx + params.dir_x * params.line_half_len, cy + params.dir_y * params.line_half_len);
        let segments = geo::clip_line_to_polygon(p1, p2, polygon_pts);
        for (s1, s2) in segments {
            out.push((s1, s2, dotted));
        }
        t += HATCH_SPACING;
    }
}

