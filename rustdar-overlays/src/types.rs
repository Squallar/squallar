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

/// Ring of (latitude, longitude) points. First ring is exterior, rest are holes.
pub type GeoPolygonRing = Vec<(f64, f64)>;

pub type GeoPolygon = Vec<GeoPolygonRing>;

/// A map label to be drawn at a geographic position.
#[derive(Debug, Clone)]
pub struct OverlayLabel {
    pub lat: f64,
    pub lon: f64,
    pub text: String,
    pub color: [u8; 4],
}

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

/// Hatching for SPC's significant-severe area.
///
/// The three levels are SPC's Conditional Intensity Groups, which NWS Service
/// Change Notice 26-11 introduced on 2026-03-02 to replace the single `SIGN`
/// area with three intensity distributions. Higher levels hatch over lower
/// ones; see [`crate::render::hatch::draw_hatch_pass`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HatchPattern {
    None,
    /// Dotted, 45° (forward slash).
    Cig1,
    /// Solid, 135° (backslash).
    Cig2,
    /// Solid, both directions (cross-hatch).
    Cig3,
}

/// Geographic bounding box for viewport culling.
///
/// `PartialEq` because the box rides inside
/// `rustdar_frontend::offload::JobRequest`, whose wire round-trip tests compare
/// whole requests; it is derived — four `f64` comparisons — and carries the
/// usual `f64` caveat that `NaN != NaN`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl GeoBounds {
    pub fn intersects(&self, other: &GeoBounds) -> bool {
        self.min_lat <= other.max_lat
            && self.max_lat >= other.min_lat
            && self.min_lon <= other.max_lon
            && self.max_lon >= other.min_lon
    }
}

#[derive(Debug, Clone)]
pub struct OverlayFeature {
    /// One or more polygons (from GeoJSON MultiPolygon).
    pub polygons: Vec<GeoPolygon>,
    pub fill_rgba: [u8; 4],
    pub stroke_rgba: [u8; 4],
    /// Short label, e.g. "SLGT", "0.05", "CIG1".
    pub label: String,
    /// Long label, e.g. "Slight Risk", "5% Tornado Risk".
    pub label2: String,
    pub hatch: HatchPattern,
    pub geo_bounds: Option<GeoBounds>,
}

impl OverlayFeature {
    /// Bounds are taken in geo-coordinates, so they survive projection: the
    /// viewport cull compares them against a projected viewport's own
    /// lat/lon box.
    pub fn new(
        polygons: Vec<GeoPolygon>,
        fill_rgba: [u8; 4],
        stroke_rgba: [u8; 4],
        label: String,
        label2: String,
        hatch: HatchPattern,
    ) -> Self {
        let geo_bounds = crate::render::geo::compute_geo_bounds(&polygons);
        Self {
            polygons,
            fill_rgba,
            stroke_rgba,
            label,
            label2,
            hatch,
            geo_bounds,
        }
    }
}
