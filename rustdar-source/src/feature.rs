//! The overlay feature vocabulary: what a polygon layer is made of, below any
//! particular renderer.

use crate::geo::{GeoBounds, GeoPolygon};

/// A map label to be drawn at a geographic position.
#[derive(Debug, Clone)]
pub struct OverlayLabel {
    pub lat: f64,
    pub lon: f64,
    pub text: String,
    pub color: [u8; 4],
}

/// Hatching for SPC's significant-severe area.
///
/// The three levels are SPC's Conditional Intensity Groups, which NWS Service
/// Change Notice 26-11 introduced on 2026-03-02 to replace the single `SIGN`
/// area with three intensity distributions. Higher levels hatch over lower
/// ones; see `rustdar_overlays`'s `render::hatch::draw_hatch_pass`.
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

/// `PartialEq` because features ride inside
/// `rustdar_frontend::offload::JobRequest` (the described overlay jobs for the
/// polygon kinds), whose wire round-trip tests compare whole requests; it is
/// derived and carries [`GeoBounds`]'s own `f64` caveat that `NaN != NaN`.
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
        let geo_bounds = crate::geo::compute_geo_bounds(&polygons);
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
