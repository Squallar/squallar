//! Turning one shapefile record's flat list of rings into `GeoPolygon`s.
//!
//! This is where the format's one genuine ambiguity lives. A shapefile Polygon
//! record is a bag of rings with no nesting and no `MultiPolygon` — NWS says so
//! itself — so which ring is an island and which is a lake in it is carried
//! *only* by winding order. ESRI's spec: an exterior ring runs clockwise, a
//! hole runs counter-clockwise, both in `(x, y)`.
//!
//! With `x = lon` and `y = lat` and the ordinary shoelace, clockwise is a
//! **negative** signed area. That sign convention is stated once, here, and
//! every decision below reads it from [`signed_area`].

use squallar_geo::{GeoPolygon, GeoPolygonRing};

/// Twice the signed shoelace area of a ring in `(x, y)` order. Positive is
/// counter-clockwise, so under the ESRI convention positive means *hole*.
pub fn signed_area(ring: &[(f64, f64)]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut twice = 0.0;
    for i in 0..n {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % n];
        twice += x1 * y2 - x2 * y1;
    }
    twice / 2.0
}

/// Ray casting in `(x, y)`; used only to decide which exterior a hole belongs
/// to, where a boundary case cannot change the drawn result.
fn contains(ring: &[(f64, f64)], (px, py): (f64, f64)) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RingStats {
    /// Rings whose winding said "hole" and that no exterior ring contained.
    /// Promoted to exteriors, because a hole with nothing to be a hole in is
    /// either a mis-wound island or an orphan, and dropping it deletes land.
    pub orphan_holes: usize,
    /// Records whose every ring wound counter-clockwise, so the record declared
    /// itself to be all holes and no land.
    pub all_holes_records: usize,
    /// Rings with fewer than four points (a closed triangle is four), which
    /// cannot bound anything.
    pub degenerate_rings: usize,
}

/// One record's rings as `GeoPolygon`s in this workspace's `(lat, lon)` order,
/// exterior first and its holes after it.
pub fn to_polygons(rings: &[Vec<(f64, f64)>], stats: &mut RingStats) -> Vec<GeoPolygon> {
    let mut exteriors: Vec<&Vec<(f64, f64)>> = Vec::new();
    let mut holes: Vec<&Vec<(f64, f64)>> = Vec::new();
    for ring in rings {
        if ring.len() < 4 {
            stats.degenerate_rings += 1;
            continue;
        }
        if signed_area(ring) < 0.0 {
            exteriors.push(ring);
        } else {
            holes.push(ring);
        }
    }

    if exteriors.is_empty() {
        if !holes.is_empty() {
            stats.all_holes_records += 1;
            exteriors = std::mem::take(&mut holes);
        } else {
            return Vec::new();
        }
    }

    let mut out: Vec<GeoPolygon> = exteriors
        .iter()
        .map(|e| vec![to_lat_lon(e)] as GeoPolygon)
        .collect();

    for hole in holes {
        let probe = hole[0];
        // Smallest containing exterior, so a lake on an island inside a lake
        // lands on the island and not on the mainland around it.
        let mut best: Option<(usize, f64)> = None;
        for (i, e) in exteriors.iter().enumerate() {
            if contains(e, probe) {
                let a = signed_area(e).abs();
                if best.is_none_or(|(_, ba)| a < ba) {
                    best = Some((i, a));
                }
            }
        }
        match best {
            Some((i, _)) => out[i].push(to_lat_lon(hole)),
            None => {
                stats.orphan_holes += 1;
                out.push(vec![to_lat_lon(hole)]);
            }
        }
    }
    out
}

/// `(x, y)` = `(lon, lat)` in the file, `(lat, lon)` in this workspace. The one
/// place the swap happens.
fn to_lat_lon(ring: &[(f64, f64)]) -> GeoPolygonRing {
    ring.iter().map(|&(x, y)| (y, x)).collect()
}
