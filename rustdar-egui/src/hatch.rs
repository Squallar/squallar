use egui::{Color32, Pos2, Rect, Stroke};
use rustdar_overlays::types::HatchPattern;

/// Spacing between hatch lines in screen pixels.
const HATCH_SPACING: f32 = 10.0;
/// Dash length for dotted hatching (CIG1) in screen pixels.
const DASH_LENGTH: f32 = 4.0;
/// Gap length for dotted hatching (CIG1) in screen pixels.
const DASH_GAP: f32 = 4.0;
/// Stroke width for hatch lines.
const HATCH_STROKE_WIDTH: f32 = 1.5;

/// Draw a hatching pattern over a polygon region.
///
/// The polygon is specified as screen-space points (already projected from geo coords).
/// The painter will draw parallel lines (solid or dotted) clipped to the polygon interior
/// using scanline-polygon intersection.
pub fn draw_hatch(
    painter: &egui::Painter,
    polygon_pts: &[Pos2],
    pattern: HatchPattern,
    clip_rect: Rect,
    dark_theme: bool,
) {
    if polygon_pts.len() < 3 || pattern == HatchPattern::None {
        return;
    }

    let hatch_color = if dark_theme {
        Color32::from_rgba_unmultiplied(255, 255, 255, 200)
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 200)
    };

    match pattern {
        HatchPattern::Cig1 => {
            // Dotted lines NE→SW (forward-slash / direction, 45°)
            draw_directional_hatch(painter, polygon_pts, clip_rect, 45.0_f32, true, hatch_color);
        }
        HatchPattern::Cig2 => {
            // Solid lines NW→SE (backslash \ direction, 135°)
            draw_directional_hatch(painter, polygon_pts, clip_rect, 135.0_f32, false, hatch_color);
        }
        HatchPattern::Cig3 => {
            // Solid cross-hatch: both 45° and 135°
            draw_directional_hatch(painter, polygon_pts, clip_rect, 45.0_f32, false, hatch_color);
            draw_directional_hatch(
                painter,
                polygon_pts,
                clip_rect,
                135.0_f32,
                false,
                hatch_color,
            );
        }
        HatchPattern::None => {}
    }
}

/// Draw parallel hatch lines at a given angle, clipped to the polygon interior.
fn draw_directional_hatch(
    painter: &egui::Painter,
    polygon_pts: &[Pos2],
    _clip_rect: Rect,
    angle_degrees: f32,
    dotted: bool,
    color: Color32,
) {
    // Compute AABB of polygon (NOT clamped to viewport — clamping shifts
    // the line grid and causes hatch rows to flicker on zoom/pan).
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for pt in polygon_pts {
        min_x = min_x.min(pt.x);
        min_y = min_y.min(pt.y);
        max_x = max_x.max(pt.x);
        max_y = max_y.max(pt.y);
    }

    if min_x >= max_x || min_y >= max_y {
        return;
    }

    let angle_rad = angle_degrees.to_radians();
    // Direction along the hatch line
    let dir_x = angle_rad.cos();
    let dir_y = -angle_rad.sin(); // screen Y is inverted
    // Normal to the hatch line (perpendicular direction for stepping)
    let norm_x = -dir_y;
    let norm_y = dir_x;

    // Project polygon AABB corners onto the normal axis to find sweep range
    let corners = [
        Pos2::new(min_x, min_y),
        Pos2::new(max_x, min_y),
        Pos2::new(min_x, max_y),
        Pos2::new(max_x, max_y),
    ];
    let mut min_proj = f32::MAX;
    let mut max_proj = f32::MIN;
    for c in &corners {
        let proj = c.x * norm_x + c.y * norm_y;
        min_proj = min_proj.min(proj);
        max_proj = max_proj.max(proj);
    }

    // Compute line extent along the hatch direction
    let mut min_dir_proj = f32::MAX;
    let mut max_dir_proj = f32::MIN;
    for c in &corners {
        let proj = c.x * dir_x + c.y * dir_y;
        min_dir_proj = min_dir_proj.min(proj);
        max_dir_proj = max_dir_proj.max(proj);
    }
    let line_half_len = (max_dir_proj - min_dir_proj) * 0.5 + 10.0;
    let dir_center = (min_dir_proj + max_dir_proj) * 0.5;

    let stroke = Stroke::new(HATCH_STROKE_WIDTH, color);

    // Sweep from the polygon's own AABB projection — since the AABB is
    // unclamped, min_proj moves smoothly with the polygon during pan/zoom,
    // keeping the hatch pattern locked to the geometry.
    let mut t = min_proj;
    while t <= max_proj {
        // Center of this hatch line in screen space
        let cx = norm_x * t + dir_x * dir_center;
        let cy = norm_y * t + dir_y * dir_center;

        // Line endpoints (extend well beyond the polygon)
        let p1 = Pos2::new(cx - dir_x * line_half_len, cy - dir_y * line_half_len);
        let p2 = Pos2::new(cx + dir_x * line_half_len, cy + dir_y * line_half_len);

        // Clip this line to the polygon
        let segments = clip_line_to_polygon(p1, p2, polygon_pts);

        for (s1, s2) in segments {
            if dotted {
                draw_dashed_line(painter, s1, s2, stroke);
            } else {
                painter.line_segment([s1, s2], stroke);
            }
        }

        t += HATCH_SPACING;
    }
}

/// Generate hatch line segments for a polygon without drawing them.
///
/// Returns `(start, end, is_dotted)` tuples suitable for caching.
/// This avoids per-frame recomputation of scanline-polygon intersections.
pub fn generate_hatch_lines(
    polygon_pts: &[Pos2],
    pattern: HatchPattern,
    clip_rect: Rect,
    dark_theme: bool,
) -> Vec<(Pos2, Pos2, bool)> {
    let _ = (clip_rect, dark_theme); // used by draw_hatch but not needed here
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
    lines
}

/// Collect hatch line segments at a given angle, clipped to the polygon.
fn collect_directional_hatch(
    out: &mut Vec<(Pos2, Pos2, bool)>,
    polygon_pts: &[Pos2],
    angle_degrees: f32,
    dotted: bool,
) {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for pt in polygon_pts {
        min_x = min_x.min(pt.x);
        min_y = min_y.min(pt.y);
        max_x = max_x.max(pt.x);
        max_y = max_y.max(pt.y);
    }
    if min_x >= max_x || min_y >= max_y {
        return;
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
    let mut min_proj = f32::MAX;
    let mut max_proj = f32::MIN;
    for c in &corners {
        let proj = c.x * norm_x + c.y * norm_y;
        min_proj = min_proj.min(proj);
        max_proj = max_proj.max(proj);
    }

    let mut min_dir_proj = f32::MAX;
    let mut max_dir_proj = f32::MIN;
    for c in &corners {
        let proj = c.x * dir_x + c.y * dir_y;
        min_dir_proj = min_dir_proj.min(proj);
        max_dir_proj = max_dir_proj.max(proj);
    }
    let line_half_len = (max_dir_proj - min_dir_proj) * 0.5 + 10.0;
    let dir_center = (min_dir_proj + max_dir_proj) * 0.5;

    let mut t = min_proj;
    while t <= max_proj {
        let cx = norm_x * t + dir_x * dir_center;
        let cy = norm_y * t + dir_y * dir_center;
        let p1 = Pos2::new(cx - dir_x * line_half_len, cy - dir_y * line_half_len);
        let p2 = Pos2::new(cx + dir_x * line_half_len, cy + dir_y * line_half_len);
        let segments = clip_line_to_polygon(p1, p2, polygon_pts);
        for (s1, s2) in segments {
            out.push((s1, s2, dotted));
        }
        t += HATCH_SPACING;
    }
}

/// Clip a line segment to a polygon using even-odd intersection.
/// Returns a list of interior segments.
fn clip_line_to_polygon(p1: Pos2, p2: Pos2, polygon: &[Pos2]) -> Vec<(Pos2, Pos2)> {
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let line_len_sq = dx * dx + dy * dy;
    if line_len_sq < 1e-6 {
        return vec![];
    }

    let mut ts = Vec::new();

    let n = polygon.len();
    for i in 0..n {
        let a = polygon[i];
        let b = polygon[(i + 1) % n];

        // Solve: p1 + t*(p2-p1) = a + s*(b-a)
        let ex = b.x - a.x;
        let ey = b.y - a.y;
        let denom = dx * ey - dy * ex;
        if denom.abs() < 1e-10 {
            continue; // parallel
        }
        let t = ((a.x - p1.x) * ey - (a.y - p1.y) * ex) / denom;
        let s = ((a.x - p1.x) * dy - (a.y - p1.y) * dx) / denom;

        if s >= 0.0 && s <= 1.0 && t >= 0.0 && t <= 1.0 {
            ts.push(t);
        }
    }

    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Pair consecutive intersections (even-odd fill rule)
    let mut segments = Vec::new();
    let mut i = 0;
    while i + 1 < ts.len() {
        let t_start = ts[i];
        let t_end = ts[i + 1];
        // Only draw if the segment has nonzero length
        if (t_end - t_start).abs() > 1e-6 {
            let s1 = Pos2::new(p1.x + dx * t_start, p1.y + dy * t_start);
            let s2 = Pos2::new(p1.x + dx * t_end, p1.y + dy * t_end);
            segments.push((s1, s2));
        }
        i += 2;
    }

    segments
}

/// Draw a dashed line segment between two points.
fn draw_dashed_line(painter: &egui::Painter, p1: Pos2, p2: Pos2, stroke: Stroke) {
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let total_len = (dx * dx + dy * dy).sqrt();
    if total_len < 1.0 {
        return;
    }

    let ux = dx / total_len;
    let uy = dy / total_len;

    let mut dist = 0.0;
    let mut drawing = true;

    while dist < total_len {
        let seg_len = if drawing { DASH_LENGTH } else { DASH_GAP };
        let end_dist = (dist + seg_len).min(total_len);

        if drawing {
            let s = Pos2::new(p1.x + ux * dist, p1.y + uy * dist);
            let e = Pos2::new(p1.x + ux * end_dist, p1.y + uy * end_dist);
            painter.line_segment([s, e], stroke);
        }

        dist = end_dist;
        drawing = !drawing;
    }
}
