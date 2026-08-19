/// A 2D screen-space point. Not `egui::Pos2`: keeps this crate GUI-agnostic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenPoint {
    pub x: f32,
    pub y: f32,
}

impl ScreenPoint {
    #[inline]
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Ramer-Douglas-Peucker epsilon, degrees. 0.005° ≈ 500 m.
pub const SIMPLIFY_EPSILON: f64 = 0.005;

/// The fill for a layer drawn **over** another one: SPC's significant-severe
/// area, which sits on top of the probability contours it qualifies.
///
/// Deliberately low, for two reasons that point the same way — so the hatch
/// lines stay visible through the fill, and so the contours underneath stay
/// readable rather than being buried by the thing describing them.
pub const SIGNIFICANT_FILL_ALPHA: u8 = 40;
pub const REGULAR_FILL_ALPHA: u8 = 100;
pub const NWS_FILL_ALPHA: u8 = 80;
pub const STROKE_ALPHA: u8 = 255;

// The feature vocabulary is defined in `rustdar-source` — the shared floor
// under this crate and `rustdar-radar` — and re-exported here under the paths
// this crate always published it at. The geo vocabulary is `rustdar-geo`'s and
// is named at that one spelling since WO-G4 killed the re-export shim here.
pub use rustdar_source::feature::{HatchPattern, OverlayFeature, OverlayLabel};

use rustdar_geo::GeoPolygon;

/// GeoJSON is `[[[lon, lat], ...], ...]`; output is `(lat, lon)`. Order swaps.
/// Rings with fewer than 3 points are dropped.
pub fn parse_polygon_coords(coords: &serde_json::Value) -> Option<GeoPolygon> {
    let rings = coords.as_array()?;
    let mut geo_rings = Vec::with_capacity(rings.len());

    for ring in rings {
        let points = ring.as_array()?;
        let geo_ring: Vec<(f64, f64)> = points
            .iter()
            .filter_map(|pt| {
                let arr = pt.as_array()?;
                let lon = arr.first()?.as_f64()?;
                let lat = arr.get(1)?.as_f64()?;
                Some((lat, lon))
            })
            .collect();
        if geo_ring.len() >= 3 {
            geo_rings.push(geo_ring);
        }
    }

    if geo_rings.is_empty() {
        None
    } else {
        Some(geo_rings)
    }
}
