//! Geometry utilities. GUI-framework-agnostic: `rustdar-egui` bridges
//! `egui::Pos2` ↔ [`ScreenPoint`].

use crate::types::{GeoBounds, GeoPolygon, GeoPolygonRing, ScreenPoint};

/// Ray casting, even-odd rule. Behaviour on the boundary is unspecified.
pub fn point_in_polygon(point: ScreenPoint, vertices: &[ScreenPoint]) -> bool {
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
        if (vi.y > py) != (vj.y > py) && px < (vj.x - vi.x) * (py - vi.y) / (vj.y - vi.y) + vi.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

// ── Shared geometry utilities ────────────────────────────────────────────

/// Ramer-Douglas-Peucker. `epsilon` is in **degrees**, not metres or pixels;
/// 0.005 ≈ 500 m. See [`crate::types::SIMPLIFY_EPSILON`].
pub fn simplify_ring(ring: &GeoPolygonRing, epsilon: f64) -> GeoPolygonRing {
    if ring.len() <= 3 {
        return ring.clone();
    }
    rdp_simplify(ring, epsilon)
}

fn rdp_simplify(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let first = points[0];
    let last = points[points.len() - 1];
    let mut max_dist = 0.0_f64;
    let mut max_idx = 0;

    for (i, &pt) in points.iter().enumerate().skip(1).take(points.len() - 2) {
        let d = perpendicular_distance(pt, first, last);
        if d > max_dist {
            max_dist = d;
            max_idx = i;
        }
    }

    if max_dist > epsilon {
        let mut left = rdp_simplify(&points[..=max_idx], epsilon);
        let right = rdp_simplify(&points[max_idx..], epsilon);
        left.pop(); // The junction point appears in both halves.
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}

fn perpendicular_distance(point: (f64, f64), line_start: (f64, f64), line_end: (f64, f64)) -> f64 {
    let dx = line_end.0 - line_start.0;
    let dy = line_end.1 - line_start.1;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-12 {
        let px = point.0 - line_start.0;
        let py = point.1 - line_start.1;
        return (px * px + py * py).sqrt();
    }
    let num = ((point.0 - line_start.0) * dy - (point.1 - line_start.1) * dx).abs();
    num / len_sq.sqrt()
}

/// Also drops rings and polygons that simplification made degenerate.
pub fn simplify_polygons(polygons: &mut Vec<GeoPolygon>, epsilon: f64) {
    for polygon in polygons.iter_mut() {
        for ring in polygon.iter_mut() {
            if ring.len() > 3 {
                *ring = simplify_ring(ring, epsilon);
            }
        }
        polygon.retain(|r| r.len() >= 3);
    }
    polygons.retain(|p| !p.is_empty());
}

/// `None` when there is not a single vertex.
pub fn compute_geo_bounds(polygons: &[GeoPolygon]) -> Option<GeoBounds> {
    let mut min_lat = f64::MAX;
    let mut max_lat = f64::MIN;
    let mut min_lon = f64::MAX;
    let mut max_lon = f64::MIN;
    let mut any = false;

    for polygon in polygons {
        for ring in polygon {
            for &(lat, lon) in ring {
                min_lat = min_lat.min(lat);
                max_lat = max_lat.max(lat);
                min_lon = min_lon.min(lon);
                max_lon = max_lon.max(lon);
                any = true;
            }
        }
    }

    if any {
        Some(GeoBounds {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        })
    } else {
        None
    }
}
