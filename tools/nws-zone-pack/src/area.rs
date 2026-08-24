//! Exact polygon-**area** intersection and union, by vertical slab decomposition.
//!
//! `--compare-cache` used to compare bounding boxes. A bounding box is not a
//! shape: one offshore cay two tenths of a degree off the coast moves the box
//! by the whole two tenths while moving the drawn pixels by almost nothing, so
//! a box IoU reports a disagreement that the map does not have. This module
//! measures the thing the map actually draws — filled area — and the thing that
//! makes a swap of origins visible — the two one-sided differences.
//!
//! # The decomposition
//!
//! Both operands are reduced to a bag of directed edges in `(x, y) = (lon,
//! lat)`. Take every vertex `x` of both operands, plus the `x` of every
//! crossing between any two edges (including an operand with itself — real
//! boundaries do self-intersect). Sort them. Between two consecutive values no
//! edge starts, ends or crosses another, so inside that vertical slab the
//! bottom-to-top ordering of the edges is constant and each region between two
//! neighbouring edges is a **trapezoid**, whose area
//!
//! ```text
//! (xr - xl) * ((y_top(xl) - y_bot(xl)) + (y_top(xr) - y_bot(xr))) / 2
//! ```
//!
//! is exact, because the edges are straight. Summing the trapezoids whose
//! winding number is non-zero for operand A gives `area(A)`; for B, `area(B)`;
//! for both at once, `area(A ∩ B)`. All three come out of one sweep, so they
//! cannot disagree with each other, and `union = a + b - inter` needs no second
//! algorithm.
//!
//! # Winding, and why it is imposed rather than read
//!
//! `GeoPolygon` declares ring 0 the exterior and the rest holes. The two
//! origins under comparison — an NWS shapefile and `api.weather.gov` GeoJSON —
//! have no reason to agree on the *winding* they store, and the app does not
//! read it: it reads the position. So this module re-winds ring 0
//! counter-clockwise and every later ring clockwise before it starts, and then
//! uses the non-zero rule. That makes the measurement a function of the
//! declared structure, which is what the renderer honours, rather than of a
//! convention neither source promises.
//!
//! With that fixed, the winding number of a point is the signed count of edges
//! *below* it, `+1` for an edge running in `+x` and `-1` for one running in
//! `-x`. A counter-clockwise square puts its `+1` bottom edge below its
//! interior and its `-1` top edge above it; a clockwise hole inside it
//! contributes `-1` below, cancelling to zero. Holes, islands, several
//! exteriors that overlap: all fall out of the same sum.
//!
//! # Units
//!
//! Areas are in **square degrees**, on the plain `(lon, lat)` plane. That is
//! not an area on the Earth, but IoU is a *ratio* and the near-uniform
//! `cos(lat)` shrink of a single zone's longitude divides out of numerator and
//! denominator; the residual is the variation of `cos(lat)` across one zone's
//! own latitude span. [`sq_deg_to_sq_km`] exists only to print a number a human
//! can size, and says in its own name that it is approximate.
//!
//! # The antimeridian
//!
//! A ring that straddles 180° arrives as longitudes near `+180` and near
//! `-180`, and a planar sweep would draw it as a band across the whole globe.
//! [`Operands::new`] detects it — a longitude span over 180° cannot be a
//! weather zone — and lifts every negative longitude by 360° for *both*
//! operands together, so the comparison stays like-for-like. It reports
//! whether it fired, so the caller can say so rather than assume.

use squallar_geo::GeoPolygon;

/// One directed edge, `x1 < x2` never assumed. Vertical edges are dropped
/// before an [`Edge`] is ever built: they span no slab and so bound no
/// trapezoid.
#[derive(Clone, Copy)]
struct Edge {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    /// `+1` when the edge runs towards `+x`, `-1` otherwise.
    dir: i32,
    /// `0` for the first operand, `1` for the second.
    set: usize,
}

impl Edge {
    fn xmin(&self) -> f64 {
        self.x1.min(self.x2)
    }
    fn xmax(&self) -> f64 {
        self.x1.max(self.x2)
    }
    /// `y` where the edge crosses the vertical line at `x`. Only ever called
    /// with an `x` inside the edge's own span, so the division is safe: a
    /// vertical edge never becomes an `Edge`.
    fn y_at(&self, x: f64) -> f64 {
        self.y1 + (x - self.x1) * (self.y2 - self.y1) / (self.x2 - self.x1)
    }
}

/// What one sweep measured. Every area field is square degrees.
#[derive(Debug, Clone, Copy, Default)]
pub struct Areas {
    pub a: f64,
    pub b: f64,
    pub inter: f64,
    /// Whether the antimeridian lift fired for this pair.
    pub wrapped: bool,
}

impl Areas {
    pub fn union(&self) -> f64 {
        self.a + self.b - self.inter
    }

    /// `inter / union`, or `None` when both operands enclose nothing at all —
    /// which is a degenerate input, not an agreement, and must not be silently
    /// counted as `1.0`.
    pub fn iou(&self) -> Option<f64> {
        let u = self.union();
        (u > 0.0).then(|| (self.inter / u).clamp(0.0, 1.0))
    }

    /// Area the first operand covers and the second does not.
    pub fn a_only(&self) -> f64 {
        (self.a - self.inter).max(0.0)
    }

    /// Area the second operand covers and the first does not.
    pub fn b_only(&self) -> f64 {
        (self.b - self.inter).max(0.0)
    }
}

/// Square degrees to an order-of-magnitude square kilometres, at a latitude.
/// A display convenience and nothing else — the name carries the caveat so a
/// reader cannot mistake the figure for a measured area.
pub fn sq_deg_to_sq_km(sq_deg: f64, lat: f64) -> f64 {
    use squallar_geo::KM_PER_DEGREE_LAT as DEG_KM;
    sq_deg * DEG_KM * DEG_KM * lat.to_radians().cos()
}

/// The two operands as WKT `MULTIPOLYGON`s, in exactly the plane the sweep
/// measured them in — the antimeridian lift included, or neither of them.
///
/// This exists for one purpose: handing the identical input to GDAL/GEOS, an
/// implementation that shares no line of code and no author with this one, so
/// that "the sweep is right" stops being a claim this file makes about itself.
/// Ring order is `GeoPolygon`'s own, which is WKT's own: exterior first.
pub fn wkt_pair(a: &[GeoPolygon], b: &[GeoPolygon]) -> (String, String, bool) {
    let lift = Operands::lift(a, b);
    let one = |polys: &[GeoPolygon]| {
        let bodies: Vec<String> = polys
            .iter()
            .filter(|p| !p.is_empty())
            .map(|poly| {
                let rings: Vec<String> = poly
                    .iter()
                    .map(|ring| {
                        let mut pts: Vec<(f64, f64)> =
                            ring.iter().map(|&(lat, lon)| (lift(lon), lat)).collect();
                        if pts.first() != pts.last()
                            && let Some(&first) = pts.first()
                        {
                            pts.push(first);
                        }
                        let s: Vec<String> = pts.iter().map(|&(x, y)| format!("{x} {y}")).collect();
                        format!("({})", s.join(","))
                    })
                    .collect();
                format!("({})", rings.join(","))
            })
            .collect();
        if bodies.is_empty() {
            "MULTIPOLYGON EMPTY".to_string()
        } else {
            format!("MULTIPOLYGON ({})", bodies.join(","))
        }
    };
    let wrapped = Operands::wraps(a, b);
    (one(a), one(b), wrapped)
}

/// Both operands' edges in one bag, after re-winding and the antimeridian lift.
struct Operands {
    edges: Vec<Edge>,
    wrapped: bool,
}

impl Operands {
    /// Whether this *pair* straddles the antimeridian. Decided over both
    /// operands at once: deciding it per operand could lift one and not the
    /// other and manufacture a 360-degree disagreement out of nothing.
    fn wraps(a: &[GeoPolygon], b: &[GeoPolygon]) -> bool {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for &(_, lon) in a.iter().chain(b).flatten().flatten() {
            lo = lo.min(lon);
            hi = hi.max(lon);
        }
        hi - lo > 180.0
    }

    /// The longitude-to-`x` map for this pair: identity, or a 360° lift of the
    /// western half onto the eastern one.
    fn lift(a: &[GeoPolygon], b: &[GeoPolygon]) -> impl Fn(f64) -> f64 {
        let wrapped = Self::wraps(a, b);
        move |lon: f64| {
            if wrapped && lon < 0.0 {
                lon + 360.0
            } else {
                lon
            }
        }
    }

    fn new(a: &[GeoPolygon], b: &[GeoPolygon]) -> Self {
        let wrapped = Self::wraps(a, b);
        let x = Self::lift(a, b);

        let mut edges = Vec::new();
        for (set, polys) in [(0usize, a), (1usize, b)] {
            for poly in polys {
                for (i, ring) in poly.iter().enumerate() {
                    // Ring 0 is the exterior and is made counter-clockwise;
                    // every later ring is a hole and is made clockwise. The
                    // stored winding is overwritten, not consulted.
                    push_ring(&mut edges, ring, set, i == 0, &x);
                }
            }
        }
        Self { edges, wrapped }
    }
}

/// Twice the signed shoelace area in `(x, y)`. Positive is counter-clockwise.
fn twice_signed_area(pts: &[(f64, f64)]) -> f64 {
    let n = pts.len();
    let mut t = 0.0;
    for i in 0..n {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % n];
        t += x1 * y2 - x2 * y1;
    }
    t
}

fn push_ring(
    edges: &mut Vec<Edge>,
    ring: &[(f64, f64)],
    set: usize,
    want_ccw: bool,
    x: &dyn Fn(f64) -> f64,
) {
    if ring.len() < 3 {
        return;
    }
    let mut pts: Vec<(f64, f64)> = ring.iter().map(|&(lat, lon)| (x(lon), lat)).collect();
    // A ring that repeats its first point at the end is the same ring; the
    // closing edge is added below either way, and the duplicate would only add
    // a zero-length segment for the crossing sweep to test.
    if pts.len() > 1 && pts[0] == pts[pts.len() - 1] {
        pts.pop();
    }
    if pts.len() < 3 {
        return;
    }
    if (twice_signed_area(&pts) > 0.0) != want_ccw {
        pts.reverse();
    }
    let n = pts.len();
    for i in 0..n {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % n];
        if x1 == x2 {
            continue;
        }
        edges.push(Edge {
            x1,
            y1,
            x2,
            y2,
            dir: if x2 > x1 { 1 } else { -1 },
            set,
        });
    }
}

/// `x` of the crossing of two segments, when they meet. A touch at a vertex is
/// already a critical `x` for free, so only the interior case has to be found
/// here; returning it anyway costs one redundant slab wall.
fn crossing_x(p: &Edge, q: &Edge) -> Option<f64> {
    let (rx, ry) = (p.x2 - p.x1, p.y2 - p.y1);
    let (sx, sy) = (q.x2 - q.x1, q.y2 - q.y1);
    let denom = rx * sy - ry * sx;
    if denom == 0.0 {
        return None;
    }
    let (qpx, qpy) = (q.x1 - p.x1, q.y1 - p.y1);
    let t = (qpx * sy - qpy * sx) / denom;
    let u = (qpx * ry - qpy * rx) / denom;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some(p.x1 + t * rx)
}

/// `area(a)`, `area(b)` and `area(a ∩ b)` from a single slab sweep.
pub fn areas(a: &[GeoPolygon], b: &[GeoPolygon]) -> Areas {
    let Operands { edges, wrapped } = Operands::new(a, b);
    if edges.is_empty() {
        return Areas {
            wrapped,
            ..Default::default()
        };
    }

    // ── the critical x values ────────────────────────────────────────────
    let mut xs: Vec<f64> = Vec::with_capacity(edges.len() * 2);
    for e in &edges {
        xs.push(e.x1);
        xs.push(e.x2);
    }
    // Crossings, found with a sweep on `xmin` rather than all pairs: two edges
    // that do not overlap in x cannot cross, and a county ring of a few
    // thousand points would otherwise cost millions of pair tests.
    let mut order: Vec<usize> = (0..edges.len()).collect();
    order.sort_by(|&i, &j| {
        edges[i]
            .xmin()
            .partial_cmp(&edges[j].xmin())
            .expect("no NaN coordinate")
    });
    for (pi, &i) in order.iter().enumerate() {
        let imax = edges[i].xmax();
        for &j in &order[pi + 1..] {
            if edges[j].xmin() > imax {
                break;
            }
            if let Some(x) = crossing_x(&edges[i], &edges[j]) {
                xs.push(x);
            }
        }
    }
    xs.sort_by(|p, q| p.partial_cmp(q).expect("no NaN coordinate"));
    xs.dedup();

    // ── the sweep ────────────────────────────────────────────────────────
    let mut out = Areas {
        wrapped,
        ..Default::default()
    };
    // Edges enter the active list in `xmin` order and leave when their `xmax`
    // falls behind the slab, so no slab rescans the whole bag.
    let mut next = 0usize;
    let mut active: Vec<usize> = Vec::new();
    // Reused across slabs: `(y at midpoint, y at xl, y at xr, dir, set)`.
    let mut fence: Vec<(f64, f64, f64, i32, usize)> = Vec::new();

    for w in xs.windows(2) {
        let (xl, xr) = (w[0], w[1]);
        if xr <= xl {
            continue;
        }
        let xm = xl + (xr - xl) / 2.0;
        // A slab so thin that its midpoint is one of its own walls carries no
        // area an f64 can represent; skipping it is exact, not a tolerance.
        if xm <= xl || xm >= xr {
            continue;
        }
        while next < order.len() && edges[order[next]].xmin() <= xl {
            active.push(order[next]);
            next += 1;
        }
        active.retain(|&i| edges[i].xmax() > xl);

        fence.clear();
        for &i in &active {
            let e = &edges[i];
            if e.xmin() > xl || e.xmax() < xr {
                continue;
            }
            fence.push((e.y_at(xm), e.y_at(xl), e.y_at(xr), e.dir, e.set));
        }
        if fence.len() < 2 {
            continue;
        }
        fence.sort_by(|p, q| p.0.partial_cmp(&q.0).expect("no NaN coordinate"));

        let width = xr - xl;
        let mut wind = [0i32; 2];
        for k in 0..fence.len() - 1 {
            let (_, yl, yr, dir, set) = fence[k];
            wind[set] += dir;
            let inside_a = wind[0] != 0;
            let inside_b = wind[1] != 0;
            if !inside_a && !inside_b {
                continue;
            }
            let (_, yl_up, yr_up, _, _) = fence[k + 1];
            let cell = width * ((yl_up - yl) + (yr_up - yr)) / 2.0;
            if inside_a {
                out.a += cell;
            }
            if inside_b {
                out.b += cell;
            }
            if inside_a && inside_b {
                out.inter += cell;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed axis-aligned box as a one-ring `GeoPolygon`, in `(lat, lon)`.
    fn rect(s: f64, w: f64, n: f64, e: f64) -> GeoPolygon {
        vec![vec![(s, w), (s, e), (n, e), (n, w), (s, w)]]
    }

    fn iou(a: &[GeoPolygon], b: &[GeoPolygon]) -> f64 {
        areas(a, b).iou().expect("non-degenerate")
    }

    #[test]
    fn identical_squares_agree_exactly() {
        let a = vec![rect(0.0, 0.0, 1.0, 1.0)];
        assert_eq!(iou(&a, &a), 1.0);
        let m = areas(&a, &a);
        assert!((m.a - 1.0).abs() < 1e-12, "{m:?}");
        assert!((m.inter - 1.0).abs() < 1e-12, "{m:?}");
    }

    #[test]
    fn half_overlap_is_a_third() {
        // Two unit squares sharing half their area: inter 0.5, union 1.5.
        let a = vec![rect(0.0, 0.0, 1.0, 1.0)];
        let b = vec![rect(0.0, 0.5, 1.0, 1.5)];
        assert!((iou(&a, &b) - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn disjoint_squares_score_zero() {
        let a = vec![rect(0.0, 0.0, 1.0, 1.0)];
        let b = vec![rect(0.0, 5.0, 1.0, 6.0)];
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn a_hole_is_subtracted_whatever_its_stored_winding() {
        // Ring 0 exterior, ring 1 hole, both handed over wound the same way;
        // the module must re-wind the hole rather than trust it.
        let mut with_hole = rect(0.0, 0.0, 10.0, 10.0);
        with_hole.push(vec![(4.0, 4.0), (4.0, 6.0), (6.0, 6.0), (6.0, 4.0)]);
        let m = areas(&[with_hole], &[rect(0.0, 0.0, 10.0, 10.0)]);
        assert!((m.a - 96.0).abs() < 1e-9, "{m:?}");
        assert!((m.b - 100.0).abs() < 1e-9, "{m:?}");
        assert!((m.inter - 96.0).abs() < 1e-9, "{m:?}");
        assert!((m.iou().unwrap() - 0.96).abs() < 1e-9, "{m:?}");
    }

    #[test]
    fn an_island_the_other_side_lacks_shows_as_one_sided() {
        let mainland = rect(0.0, 0.0, 1.0, 1.0);
        let cay = rect(0.4, 3.0, 0.5, 3.1);
        let m = areas(&[mainland.clone(), cay], &[mainland]);
        assert!((m.a_only() - 0.01).abs() < 1e-12, "{m:?}");
        assert_eq!(m.b_only(), 0.0);
    }

    #[test]
    fn a_bow_tie_counts_its_lobes_once_each() {
        // Self-intersecting: the case where the shoelace cancels to zero and a
        // sweep must not. Two triangles of area 1 meeting at (1, 1); their
        // windings are opposite, so a rule that summed signs would report 0 and
        // the non-zero rule reports 2.
        let bow: GeoPolygon = vec![vec![(0.0, 0.0), (0.0, 2.0), (2.0, 0.0), (2.0, 2.0)]];
        assert_eq!(
            twice_signed_area(&[(0.0, 0.0), (2.0, 0.0), (0.0, 2.0), (2.0, 2.0)]),
            0.0
        );
        let m = areas(std::slice::from_ref(&bow), std::slice::from_ref(&bow));
        assert!((m.a - 2.0).abs() < 1e-12, "bow-tie area {} is not 2", m.a);
        assert_eq!(m.iou(), Some(1.0));
    }

    #[test]
    fn the_antimeridian_does_not_wrap_the_globe() {
        // A zone from 179E to 179W: 2 degrees wide, not 358.
        let a = vec![vec![vec![
            (50.0, 179.0),
            (50.0, -179.0),
            (51.0, -179.0),
            (51.0, 179.0),
        ]]];
        let m = areas(&a, &a);
        assert!(m.wrapped, "the lift should have fired");
        assert!((m.a - 2.0).abs() < 1e-9, "{m:?}");
        assert_eq!(m.iou(), Some(1.0));
    }

    #[test]
    fn two_empties_are_degenerate_not_identical() {
        let empty: Vec<GeoPolygon> = Vec::new();
        assert_eq!(areas(&empty, &empty).iou(), None);
    }

    #[test]
    fn a_diagonal_overlap_matches_the_closed_form() {
        // A diamond against an axis-aligned square, where every crossing is
        // interior to both edges and none is at a vertex.
        let diamond: GeoPolygon = vec![vec![(0.0, 1.0), (1.0, 2.0), (2.0, 1.0), (1.0, 0.0)]];
        let square = rect(0.0, 0.0, 2.0, 2.0);
        let m = areas(&[diamond], &[square]);
        assert!((m.a - 2.0).abs() < 1e-9, "{m:?}");
        assert!((m.b - 4.0).abs() < 1e-9, "{m:?}");
        assert!((m.inter - 2.0).abs() < 1e-9, "{m:?}");
        assert!((m.iou().unwrap() - 0.5).abs() < 1e-9, "{m:?}");
    }
}
