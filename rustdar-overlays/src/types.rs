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

/// Deliberately low so the hatch lines stay visible through the fill.
pub const CIG_FILL_ALPHA: u8 = 40;
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

/// CIG (Conditional Intensity Group) hatching.
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
#[derive(Debug, Clone, Copy)]
pub struct GeoBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl GeoBounds {
    /// Points are `(lat, lon)`.
    pub fn from_points(pts: &[(f64, f64)]) -> Option<Self> {
        if pts.is_empty() {
            return None;
        }
        let mut min_lat = f64::MAX;
        let mut max_lat = f64::MIN;
        let mut min_lon = f64::MAX;
        let mut max_lon = f64::MIN;
        for &(lat, lon) in pts {
            min_lat = min_lat.min(lat);
            max_lat = max_lat.max(lat);
            min_lon = min_lon.min(lon);
            max_lon = max_lon.max(lon);
        }
        Some(Self {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        })
    }

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

    /// Call after mutating `polygons` (e.g. simplification).
    pub fn recompute_cache(&mut self) {
        self.geo_bounds = crate::render::geo::compute_geo_bounds(&self.polygons);
    }
}
