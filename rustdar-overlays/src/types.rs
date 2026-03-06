/// A single polygon ring: a sequence of (latitude, longitude) points.
/// The first ring is the exterior, subsequent rings are holes.
pub type GeoPolygonRing = Vec<(f64, f64)>;

/// A polygon with an exterior ring and optional holes.
pub type GeoPolygon = Vec<GeoPolygonRing>;

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
}
