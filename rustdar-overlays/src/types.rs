/// A 2D screen-space point.
///
/// Used in place of egui's `Pos2` so that geometry algorithms in this crate
/// remain GUI-framework-agnostic.  Conversion to/from `egui::Pos2` is
/// trivially implemented in the `rustdar-egui` crate.
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

/// Default epsilon (degrees) for Ramer-Douglas-Peucker polygon simplification.
/// ~0.005° ≈ 500 m — keeps shapes visually accurate at typical map zoom levels
/// while significantly reducing vertex counts.
pub const SIMPLIFY_EPSILON: f64 = 0.005;

/// Fill alpha for CIG-hatched outlook areas (low opacity so hatching is visible).
pub const CIG_FILL_ALPHA: u8 = 40;
/// Fill alpha for regular (non-hatched) outlook areas.
pub const REGULAR_FILL_ALPHA: u8 = 100;
/// Fill alpha for NWS alert polygons.
pub const NWS_FILL_ALPHA: u8 = 80;
/// Stroke alpha (fully opaque) shared by all overlay types.
pub const STROKE_ALPHA: u8 = 255;

/// A single polygon ring: a sequence of (latitude, longitude) points.
/// The first ring is the exterior, subsequent rings are holes.
pub type GeoPolygonRing = Vec<(f64, f64)>;

/// A polygon with an exterior ring and optional holes.
pub type GeoPolygon = Vec<GeoPolygonRing>;

/// A map label to be drawn at a geographic position.
#[derive(Debug, Clone)]
pub struct OverlayLabel {
    pub lat: f64,
    pub lon: f64,
    pub text: String,
    pub color: [u8; 4],
}

/// Parse GeoJSON polygon coordinate rings into a `GeoPolygon`.
///
/// GeoJSON format: `[ [ [lon, lat], ... ], ... ]`
/// Returns `(lat, lon)` pairs per ring. Skips degenerate rings with fewer than 3 points.
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

/// Hatching pattern for CIG (Conditional Intensity Group) areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HatchPattern {
    /// No hatching — standard filled polygon.
    None,
    /// CIG1: dotted hatch lines angled to the left (135° / backslash direction).
    Cig1,
    /// CIG2: solid hatch lines angled to the right (45° / forward-slash direction).
    Cig2,
    /// CIG3: solid hatch lines in both directions (cross-hatch).
    Cig3,
}

/// Geographic bounding box for quick viewport culling.
#[derive(Debug, Clone, Copy)]
pub struct GeoBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl GeoBounds {
    /// Compute bounds from a set of (lat, lon) points.
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

    /// Check whether this bounds intersects another.
    pub fn intersects(&self, other: &GeoBounds) -> bool {
        self.min_lat <= other.max_lat
            && self.max_lat >= other.min_lat
            && self.min_lon <= other.max_lon
            && self.max_lon >= other.min_lon
    }
}

/// Pre-computed triangulation for a single polygon ring.
/// Indices refer to the exterior ring vertices (with GeoJSON closing
/// duplicate stripped). Computed once at fetch time and reused across frames.
#[derive(Debug, Clone)]
pub struct PrecomputedTriangulation {
    /// Triangle indices into the ring's vertex list.
    pub indices: Vec<u32>,
}

/// A renderable overlay feature with geometry, styling, and metadata.
#[derive(Debug, Clone)]
pub struct OverlayFeature {
    /// One or more polygons (from GeoJSON MultiPolygon).
    pub polygons: Vec<GeoPolygon>,
    /// Fill color as RGBA (alpha controls transparency).
    pub fill_rgba: [u8; 4],
    /// Stroke/outline color as RGBA.
    pub stroke_rgba: [u8; 4],
    /// Short label (e.g. "SLGT", "0.05", "CIG1").
    pub label: String,
    /// Human-readable label (e.g. "Slight Risk", "5% Tornado Risk").
    pub label2: String,
    /// Hatching pattern for CIG areas.
    pub hatch: HatchPattern,
    /// Pre-computed triangulation for each polygon's exterior ring.
    /// Indexed parallel to `polygons` — `triangulations[i]` corresponds
    /// to `polygons[i][0]` (the exterior ring).
    pub triangulations: Vec<Option<PrecomputedTriangulation>>,
    /// Geographic bounding box encompassing all polygons in this feature.
    pub geo_bounds: Option<GeoBounds>,
    /// Optional geographic center point for screen-space hit-testing.
    /// When set, click detection projects this point to screen coordinates
    /// and checks pixel distance rather than doing polygon containment.
    /// Used for point-marker overlays (e.g. storm reports).
    pub click_center: Option<(f64, f64)>,
}

impl OverlayFeature {
    /// Build a new `OverlayFeature` and pre-compute triangulation + geo-bounds.
    ///
    /// Triangulation is computed once in geo-coordinates (the topology is
    /// projection-invariant, so the same index buffer works after any
    /// linear coordinate transform such as Mercator projection).
    pub fn new(
        polygons: Vec<GeoPolygon>,
        fill_rgba: [u8; 4],
        stroke_rgba: [u8; 4],
        label: String,
        label2: String,
        hatch: HatchPattern,
    ) -> Self {
        let triangulations = crate::render::geo::precompute_triangulations(&polygons);
        let geo_bounds = crate::render::geo::compute_geo_bounds(&polygons);
        Self {
            polygons,
            fill_rgba,
            stroke_rgba,
            label,
            label2,
            hatch,
            triangulations,
            geo_bounds,
            click_center: None,
        }
    }

    /// Build a point feature for screen-space click detection.
    ///
    /// Has no polygons — hit-testing is done by projecting `click_center`
    /// to screen space and checking pixel distance.
    pub fn point(lat: f64, lon: f64) -> Self {
        Self {
            polygons: Vec::new(),
            fill_rgba: [0; 4],
            stroke_rgba: [0; 4],
            label: String::new(),
            label2: String::new(),
            hatch: HatchPattern::None,
            triangulations: Vec::new(),
            geo_bounds: None,
            click_center: Some((lat, lon)),
        }
    }

    /// Recompute triangulation and geo-bounds from the current polygons.
    /// Call this after mutating `polygons` (e.g. after simplification).
    pub fn recompute_cache(&mut self) {
        self.triangulations = crate::render::geo::precompute_triangulations(&self.polygons);
        self.geo_bounds = crate::render::geo::compute_geo_bounds(&self.polygons);
    }
}
