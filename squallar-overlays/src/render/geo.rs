//! Geometry utilities. GUI-framework-agnostic: `squallar-egui` bridges
//! `egui::Pos2` ↔ [`ScreenPoint`].

use crate::types::ScreenPoint;
use squallar_geo::{GeoBounds, GeoPolygon, GeoPolygonRing};

/// How far over a whole turn a datum may measure and still be carried by
/// [`lon_shift`].
///
/// An extent is read off its own coordinates, and one that spans the world can
/// come back a rounding error over 360: at the zoom floor a pane's two edges
/// unproject to `-4e-14` and `360.00000000000006`, because `2f64.powf(zoom)` is
/// not the exact inverse of the `log2` the floor was solved with (walkers,
/// `bc25271f`), and the same arithmetic one ulp of zoom away lands the other
/// side of 360. A bare `> 360.0` would carry one arm and refuse the other.
/// `1e-12` is 17.6 ulps of 360 (`2^-44`, 5.7e-14 each): six times the largest
/// excess measured, 3 ulps, over two pane widths at the floor and one ulp of
/// zoom either side of it and three centres — and `1e-10` of the finest column
/// any source here draws (MRMS, 0.01 deg), so no width that means anything
/// fits inside it. `lon_shift_tests` is that measurement.
pub const TURN_SLACK: f64 = 1e-12;

/// The whole multiple of 360° that carries the datum spanning
/// `[datum_min, datum_max]` to its representation nearest the target spanning
/// `[target_min, target_max]`.
///
/// One spelling, because the callers have to agree or the map draws a shape
/// where it cannot be clicked: the rasterizer moves a *polygon* toward the
/// *viewport*, the hit test moves a *click* toward a *ring*, the hover cull
/// moves a *pointer* toward a *grid* ([`lon_into_bounds`]).
///
/// **A datum up to a whole turn wide is carried; wider is left where it is.**
/// Nearest is decided between centres, which are defined for any finite span,
/// so the width never makes the choice ambiguous — it decides what the choice
/// is *for*. A point, or a ring inside a half-turn, has one copy that can be in
/// frame at all, and this picks it. A pooled extent has no single placement —
/// a zone the source cut at the seam, one piece at `+179.5..180` and one at
/// `-180..-179.5`, measures `-180..180` — but the caller reading a pooled
/// extent is a cull, and a cull wants the copy nearest the box: left in the
/// ±180 frame, that extent meets no viewport panned past the seam to
/// `181..200`, and the piece that belongs at `180..180.5` is never drawn
/// (`dateline_tests`). The half-turn ceiling this replaced refused exactly
/// those, and a datum spanning 180 degrees with them. Nothing written in ±180
/// can measure more than a turn, so past `360 + TURN_SLACK` the span is not a
/// width and no shift is right.
///
/// What this does not do: a ring wider than a half-turn can be in frame
/// through *two* copies at once, and a rigid shift draws one. No source here
/// carries such a ring.
pub fn lon_shift(datum_min: f64, datum_max: f64, target_min: f64, target_max: f64) -> f64 {
    let span = datum_max - datum_min;
    if !span.is_finite() || !(0.0..=360.0 + TURN_SLACK).contains(&span) {
        return 0.0;
    }
    let datum_centre = (datum_min + datum_max) / 2.0;
    let target_centre = (target_min + target_max) / 2.0;
    if !target_centre.is_finite() {
        return 0.0;
    }
    whole_turns(datum_centre, target_centre)
}

/// The whole multiple of 360° from `from` to the copy of it nearest `to`.
///
/// One expression, so the two shifts below cannot round two ways.
fn whole_turns(from: f64, to: f64) -> f64 {
    360.0 * ((to - from) / 360.0).round()
}

/// The whole multiple of 360° that carries a **box** spanning
/// `[box_min, box_max]` to the copy whose centre is nearest the centre of
/// `[target_min, target_max]` — [`lon_shift`] with the whole-turn ceiling
/// lifted.
///
/// The ceiling is right for what `lon_shift` carries. A point, a ring, a
/// pooled extent are read off ±180 coordinates, nothing written there can
/// measure more than a turn, and a span past one is not a width. A box is the
/// other kind of datum: stated in the one continuous frame the map works in,
/// its span *is* its width, and at the zoom floor the viewport alone is a turn
/// wide before `OverlayTexturePlan::coverage` grows it. Left where it is, a
/// box a turn and a half wide written `90..630` meets a grid at `-130..-60`
/// nowhere — `GridCoords::index_bounds` reads the box's *edges*, not the
/// ground it shows. Carried to within a half-turn of the target's centre, a
/// box wider than a turn covers the whole target, which is the one answer
/// those edges can give. Zero for any non-finite input.
pub fn box_lon_shift(box_min: f64, box_max: f64, target_min: f64, target_max: f64) -> f64 {
    let box_centre = (box_min + box_max) / 2.0;
    let target_centre = (target_min + target_max) / 2.0;
    if !(box_centre.is_finite() && target_centre.is_finite()) {
        return 0.0;
    }
    whole_turns(box_centre, target_centre)
}

/// A pointer's longitude carried into the frame `bounds` is written in.
///
/// The pointer arrives from `walkers::Projector::unproject`, which is linear in
/// pixel x and folds nothing, so past the antimeridian it reads 190 where the
/// grid under it is written at -170 and an unfolded
/// [`GeoBounds::contains_point`] refuses it. The pointer is the end that moves —
/// a point has no shape to deform — and it moves by the one whole turn that
/// brings it nearest the box, unconditionally: never behind a "does this grid
/// wrap" gate, which would have the same geometry answer two ways depending on
/// which arm holds it. [`lon_shift`] with a datum of zero span.
pub fn lon_into_bounds(lon: f64, bounds: &GeoBounds) -> f64 {
    lon + lon_shift(lon, lon, bounds.min_lon, bounds.max_lon)
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

#[cfg(test)]
mod lon_shift_tests;
