//! Screen-space geometry utilities.
//!
//! Pure geometry algorithms operating on [`ScreenPoint`] coordinates.
//! These are GUI-framework-agnostic — the `rustdar-egui` crate provides
//! thin conversion wrappers to bridge `egui::Pos2` ↔ `ScreenPoint`.

use crate::types::{GeoBounds, GeoPolygon, GeoPolygonRing, PrecomputedTriangulation, ScreenPoint};

/// Ray-casting (even-odd rule) point-in-polygon test.
/// Returns `true` if `point` lies inside the polygon defined by `vertices`.
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
pub fn aabb(pts: &[ScreenPoint]) -> (f32, f32, f32, f32) {
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

/// Clip a line segment to a polygon using even-odd intersection.
/// Returns a list of interior segments.
pub fn clip_line_to_polygon(
    p1: ScreenPoint,
    p2: ScreenPoint,
    polygon: &[ScreenPoint],
) -> Vec<(ScreenPoint, ScreenPoint)> {
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
            let s1 = ScreenPoint::new(p1.x + dx * t_start, p1.y + dy * t_start);
            let s2 = ScreenPoint::new(p1.x + dx * t_end, p1.y + dy * t_end);
            segments.push((s1, s2));
        }
        i += 2;
    }

    segments
}


// ── Shared geometry utilities ────────────────────────────────────────────

/// Ramer-Douglas-Peucker polygon ring simplification.
///
/// Reduces vertex count by removing points within `epsilon` degrees of the
/// line between their neighbours. An epsilon of ~0.005 (~500 m) keeps shapes
/// visually accurate at typical map zoom levels while cutting vertex counts
/// significantly.
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
        left.pop(); // Remove duplicate junction point
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}

fn perpendicular_distance(
    point: (f64, f64),
    line_start: (f64, f64),
    line_end: (f64, f64),
) -> f64 {
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

/// Simplify all rings in all polygons of a feature's polygon set.
pub fn simplify_polygons(polygons: &mut Vec<GeoPolygon>, epsilon: f64) {
    for polygon in polygons.iter_mut() {
        for ring in polygon.iter_mut() {
            if ring.len() > 3 {
                *ring = simplify_ring(ring, epsilon);
            }
        }
        // Remove degenerate rings
        polygon.retain(|r| r.len() >= 3);
    }
    // Remove empty polygons
    polygons.retain(|p| !p.is_empty());
}


/// Strip the GeoJSON closing duplicate (last == first) from a ring.
fn strip_closing_dup(ring: &[(f64, f64)]) -> &[(f64, f64)] {
    if ring.len() > 3 && ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    }
}

/// Pre-compute ear-clip triangulation for each polygon's exterior ring.
pub fn precompute_triangulations(polygons: &[GeoPolygon]) -> Vec<Option<PrecomputedTriangulation>> {
    polygons
        .iter()
        .map(|polygon| {
            let exterior = polygon.first()?;
            let ring = strip_closing_dup(exterior);
            if ring.len() < 3 {
                return None;
            }
            // Flatten to [lat0, lon0, lat1, lon1, ...] for earcutr
            let coords: Vec<f64> = ring.iter().flat_map(|&(lat, lon)| [lat, lon]).collect();
            let indices = earcutr::earcut(&coords, &[], 2).ok()?;
            if indices.is_empty() {
                return None;
            }
            Some(PrecomputedTriangulation {
                indices: indices.into_iter().map(|i| i as u32).collect(),
            })
        })
        .collect()
}

/// Compute the overall geographic bounding box for all polygons in a feature.
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
