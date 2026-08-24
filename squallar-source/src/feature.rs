//! The overlay feature vocabulary: what a polygon layer is made of, below any
//! particular renderer.

use squallar_geo::{GeoBounds, GeoPolygon};

#[derive(Debug, Clone)]
pub struct OverlayLabel {
    pub lat: f64,
    pub lon: f64,
    pub text: String,
    pub color: [u8; 4],
}

/// Hatching for SPC's significant-severe area. The three levels are SPC's
/// Conditional Intensity Groups (NWS Service Change Notice 26-11, 2026-03-02).
/// Higher levels hatch over lower ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HatchPattern {
    None,
    Cig1,
    Cig2,
    Cig3,
}

/// `PartialEq` because features ride inside a `JobRequest` whose wire
/// round-trip tests compare whole requests; it carries [`GeoBounds`]'s own
/// `f64` caveat that `NaN != NaN`.
#[derive(Debug, Clone, PartialEq)]
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
    /// Geo-coordinates, so they survive projection: the viewport cull compares
    /// them against a projected viewport's own lat/lon box.
    pub fn new(
        polygons: Vec<GeoPolygon>,
        fill_rgba: [u8; 4],
        stroke_rgba: [u8; 4],
        label: String,
        label2: String,
        hatch: HatchPattern,
    ) -> Self {
        let geo_bounds = squallar_geo::compute_geo_bounds(&polygons);
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
