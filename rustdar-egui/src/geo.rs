//! Shared screen-space geometry utilities.
//!
//! Consolidates duplicated helpers (point-in-polygon, dashed lines, AABB)
//! that were previously defined independently in `hatch.rs`, `overlay_cache.rs`,
//! and `ui.rs`.

use egui::{Pos2, Shape, Stroke};

/// Ray-casting (even-odd rule) point-in-polygon test.
/// Returns `true` if `point` lies inside the polygon defined by `vertices`.
pub fn point_in_polygon(point: Pos2, vertices: &[Pos2]) -> bool {
    let n = vertices.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let px = point.x;
    let py = point.y;
    let mut j = n - 1;
    for i in 0..n {
        let vi = vertices[i];
        let vj = vertices[j];
        if (vi.y > py) != (vj.y > py)
            && px < (vj.x - vi.x) * (py - vi.y) / (vj.y - vi.y) + vi.x
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Compute axis-aligned bounding box of a set of screen-space points.
/// Returns `(min_x, min_y, max_x, max_y)`.
pub fn aabb(pts: &[Pos2]) -> (f32, f32, f32, f32) {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for pt in pts {
        min_x = min_x.min(pt.x);
        min_y = min_y.min(pt.y);
        max_x = max_x.max(pt.x);
        max_y = max_y.max(pt.y);
    }
    (min_x, min_y, max_x, max_y)
}

/// Generate dashed line shapes from `p1` to `p2`.
pub fn dashed_line_shapes(p1: Pos2, p2: Pos2, stroke: Stroke) -> Vec<Shape> {
    const DASH: f32 = 4.0;
    const GAP: f32 = 4.0;
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return Vec::new();
    }
    let nx = dx / len;
    let ny = dy / len;
    let mut shapes = Vec::new();
    let mut t = 0.0_f32;
    while t < len {
        let end = (t + DASH).min(len);
        shapes.push(Shape::line_segment(
            [
                Pos2::new(p1.x + nx * t, p1.y + ny * t),
                Pos2::new(p1.x + nx * end, p1.y + ny * end),
            ],
            stroke,
        ));
        t = end + GAP;
    }
    shapes
}

/// Clip a line segment to a polygon using even-odd intersection.
/// Returns a list of interior segments.
pub fn clip_line_to_polygon(p1: Pos2, p2: Pos2, polygon: &[Pos2]) -> Vec<(Pos2, Pos2)> {
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
        if (t_end - t_start).abs() > 1e-6 {
            let s1 = Pos2::new(p1.x + dx * t_start, p1.y + dy * t_start);
            let s2 = Pos2::new(p1.x + dx * t_end, p1.y + dy * t_end);
            segments.push((s1, s2));
        }
        i += 2;
    }

    segments
}
