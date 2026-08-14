//! Geometry utilities. GUI-framework-agnostic: `rustdar-egui` bridges
//! `egui::Pos2` ↔ [`ScreenPoint`].

use crate::types::{GeoBounds, GeoPolygon, GeoPolygonRing, ScreenPoint};

/// The whole multiple of 360° that carries the datum spanning
/// `[datum_min, datum_max]` to its representation nearest the target spanning
/// `[target_min, target_max]`.
///
/// One spelling, because the two callers have to agree or the map draws a
/// shape where it cannot be clicked. The rasterizer moves a *polygon* toward
/// the *viewport* (`MercatorBounds::lon_shift`); the hit test moves a *click*
/// toward a *ring* (`geo_point_in_feature`). Same question, opposite ends.
///
/// A datum wider than a half-turn has no unambiguous nearest representation —
/// translating it would be a guess about which side of the seam it meant — so
/// it gets no shift. A single point always has a span of zero and so is always
/// answerable, which is why moving the point is the safe end to move.
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

// ── Shared geometry utilities ────────────────────────────────────────────

/// How far the tolerance may be tightened before a ring is kept unsimplified.
///
/// Each step halves `epsilon`, so 16 takes 0.005° down to 7.6e-8° — about 8 mm.
/// The measured worst case over all 11,651 published NWS zones is **13**, and
/// no ring in that corpus is undrawable at any tolerance; the bound exists so
/// that a ring which really is degenerate — every vertex collinear, or all of
/// them the same point — terminates instead of halving forever.
const MAX_TOLERANCE_HALVINGS: u32 = 16;

/// Whether a simplified ring is still a ring: at least three points, and some
/// area between them.
///
/// The area test is not belt-and-braces. RDP's terminal case returns
/// `[first, last]`, and on a *closed* ring `first == last`, so a ring whose
/// halves are both flat against the chord comes back as `[v0, vfar, v0]` — a
/// three-point figure that walks out and comes straight back. It clears any
/// `len() >= 3` check and encloses nothing. Its shoelace terms cancel pairwise
/// and sum to exactly `0.0`, which is why this is an exact comparison and not
/// a threshold: over the 58,196 rings of the full NWS zone corpus the test
/// splits 8,640 out-and-backs from every real ring with no cases in between.
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
/// # A ring in is a ring out
///
/// Simplification is a fidelity operation with a tolerance. It is **not** a
/// filter, and it must never be the thing that decides a shape is too small to
/// exist — that decision belongs downstream, in projected pixels, where it
/// knows the zoom (`rasterize::hole_is_drawable`). Plain RDP does not honour it:
/// anything smaller than `epsilon` collapses to two coincident points or to a
/// zero-area out-and-back, and the caller is left holding a shape it cannot
/// draw. So when the tolerance would destroy the ring, the tolerance gives way
/// — `epsilon` is halved until the ring survives, and only a ring that is
/// degenerate at *every* tolerance is returned as it came in.
///
/// Measured on the full NWS zone corpus — every one of the 11,651 published
/// zones — this is not a corner: 38,351 of 58,196 rings need a tighter
/// tolerance, 26,963 of 44,579 polygon parts had an exterior ring that drew
/// nothing at all, and six zones (the Yap outer-island atolls, ~700 m across)
/// vanished whole and reported themselves as having no boundary. Honouring the
/// ring costs 21% more vertices than discarding it, and still keeps 90% of the
/// reduction against the raw geometry.
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
