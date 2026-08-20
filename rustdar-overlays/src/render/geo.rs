//! Geometry utilities. GUI-framework-agnostic: `rustdar-egui` bridges
//! `egui::Pos2` ↔ [`ScreenPoint`].

use crate::types::ScreenPoint;
use rustdar_geo::{GeoPolygon, GeoPolygonRing};

/// The whole multiple of 360° that carries the datum spanning
/// `[datum_min, datum_max]` to its representation nearest the target spanning
/// `[target_min, target_max]`.
///
/// One spelling, because the two callers have to agree or the map draws a shape
/// where it cannot be clicked: the rasterizer moves a *polygon* toward the
/// *viewport*, the hit test moves a *click* toward a *ring*.
///
/// A datum wider than a half-turn has no unambiguous nearest representation, so
/// it gets no shift. A single point always has a span of zero, which is why
/// moving the point is the safe end to move.
pub fn lon_shift(datum_min: f64, datum_max: f64, target_min: f64, target_max: f64) -> f64 {
    let span = datum_max - datum_min;
    if !span.is_finite() || !(0.0..180.0).contains(&span) {
        return 0.0;
    }
    let datum_centre = (datum_min + datum_max) / 2.0;
    let target_centre = (target_min + target_max) / 2.0;
    if !target_centre.is_finite() {
        return 0.0;
    }
    360.0 * ((target_centre - datum_centre) / 360.0).round()
}

/// `ring`'s longitude extent, or `None` for a ring with no finite vertex.
pub fn ring_lon_extent(ring: &[(f64, f64)]) -> Option<(f64, f64)> {
    let mut min_lon = f64::INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    for &(_, lon) in ring {
        min_lon = min_lon.min(lon);
        max_lon = max_lon.max(lon);
    }
    (min_lon.is_finite() && max_lon.is_finite()).then_some((min_lon, max_lon))
}

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


/// How far the tolerance may be tightened before a ring is kept unsimplified.
///
/// Each step halves `epsilon`, so 16 takes 0.005° down to 7.6e-8° — about 8 mm.
/// The measured worst case over all 11,651 published NWS zones is **13**.
const MAX_TOLERANCE_HALVINGS: u32 = 16;

/// Whether a simplified ring is still a ring: at least three points, and some
/// area between them.
///
/// The area test is not belt-and-braces: RDP's terminal case returns
/// `[first, last]`, and on a *closed* ring `first == last`, so a ring whose
/// halves are both flat against the chord comes back as `[v0, vfar, v0]` — a
/// three-point figure that clears `len() >= 3` and encloses nothing. Its
/// shoelace terms cancel to exactly `0.0`, hence an exact comparison; over the
/// 58,196 rings of the full NWS zone corpus it splits 8,640 out-and-backs from
/// every real ring with no cases in between.
fn encloses_area(ring: &[(f64, f64)]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut twice = 0.0;
    for i in 0..n {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % n];
        twice += x1 * y2 - x2 * y1;
    }
    (twice * 0.5).abs() > 0.0
}

/// Ramer-Douglas-Peucker. `epsilon` is in **degrees**, not metres or pixels;
/// 0.005 ≈ 500 m. See [`crate::types::SIMPLIFY_EPSILON`].
///
/// Simplification is a fidelity operation, **not** a filter: it must never be
/// the thing that decides a shape is too small to exist — that belongs
/// downstream, in projected pixels (`rasterize::hole_is_drawable`). So when the
/// tolerance would destroy the ring, the tolerance gives way: `epsilon` is
/// halved until the ring survives, and only a ring degenerate at *every*
/// tolerance is returned as it came in.
///
/// Measured on all 11,651 published NWS zones: 38,351 of 58,196 rings need a
/// tighter tolerance, 26,963 of 44,579 polygon parts had an exterior ring that
/// drew nothing at all, and six zones vanished whole. Honouring the ring costs
/// 21% more vertices and still keeps 90% of the reduction.
pub fn simplify_ring(ring: &GeoPolygonRing, epsilon: f64) -> GeoPolygonRing {
    if ring.len() <= 3 {
        return ring.clone();
    }
    let mut epsilon = epsilon;
    for _ in 0..=MAX_TOLERANCE_HALVINGS {
        let candidate = rdp_simplify(ring, epsilon);
        if encloses_area(&candidate) {
            return candidate;
        }
        epsilon /= 2.0;
    }
    ring.clone()
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
