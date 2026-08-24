//! What the readout under the pointer reads, and where it reads it from.

use crate::render::polar::{GateAt, PolarField, PolarGeometry};
use crate::types::RadarProduct;
use nexrad_model::data::Scan;
use std::sync::Arc;

/// What the readout can be told about a point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reading {
    /// A gate was painted there, and this is its value.
    Value(f32),
    /// The render painted nothing there — off the end of a radial, in the sky
    /// of a radial that painted nothing, below threshold, or range-folded.
    /// The picture is blank under the cursor and so is the readout.
    Unpainted,
    /// A gate *was* painted there and nothing is holding its value.
    NotResident,
}

/// The volume behind a picture, and the sweep of it that was drawn.
#[derive(Clone)]
pub struct SweepGates {
    scan: Arc<Scan>,
    /// Index into `scan.sweeps()`, resolved once by
    /// [`crate::render::sweep_index_for`] — the render's own sweep selection,
    /// not a second one.
    sweep: usize,
    product: RadarProduct,
}

impl SweepGates {
    /// The gates of the sweep `product` at `elevation_deg` was drawn from, or
    /// `None` where this volume cannot answer for that picture.
    pub fn new(scan: Arc<Scan>, product: RadarProduct, elevation_deg: f32) -> Option<Self> {
        if !product.is_wire_moment() {
            return None;
        }
        let sweep = crate::render::sweep_index_for(&scan, product, elevation_deg)?;
        Some(Self {
            scan,
            sweep,
            product,
        })
    }

    /// The value at a gate, decoded on demand.
    fn at(&self, at: GateAt) -> Option<f32> {
        let sweep = self.scan.sweeps().get(self.sweep)?;
        let radial = sweep.radials().get(at.radial)?;
        let moment = self.product.get_moment(radial)?;
        let raw = crate::render::moment_value_at(moment, at.gate)?;
        crate::render::painted_moment_value(raw).filter(|v| !v.is_nan())
    }
}

/// Where a pane's readout gets its number: the geometry of the picture on the
/// glass, and whatever is holding the values behind it.
pub struct HoverSource {
    /// The picture's polar geometry, always — 5.8 KiB for a full ring — and its
    /// values when the render kept them.
    field: PolarField,
    /// The volume behind it, for a frame whose values were not kept.
    sweep: Option<SweepGates>,
}

impl HoverSource {
    /// A source over a render that kept its numbers — a still pane's.
    pub fn resident(field: PolarField) -> Self {
        Self { field, sweep: None }
    }

    /// A source over a render whose numbers were dropped, reading them back out
    /// of the volume it was drawn from — a loop frame's.
    pub fn from_volume(field: PolarField, sweep: Option<SweepGates>) -> Self {
        Self { field, sweep }
    }

    /// A source over nothing, for a pane with no picture yet.
    pub fn empty() -> Self {
        Self {
            field: PolarField::default(),
            sweep: None,
        }
    }

    /// What was painted at this point.
    pub fn read(&self, azimuth_deg: f64, ground_km: f64) -> Reading {
        let Some(at) = self.field.geometry().pick(azimuth_deg, ground_km) else {
            return Reading::Unpainted;
        };
        if self.field.has_values() {
            return match self.field.at(at) {
                Some(v) => Reading::Value(v),
                None => Reading::Unpainted,
            };
        }
        match self.sweep.as_ref().and_then(|s| s.at(at)) {
            Some(v) => Reading::Value(v),
            // A gate the geometry found, that nothing is holding.
            None if self.sweep.is_none() => Reading::NotResident,
            None => Reading::Unpainted,
        }
    }

    /// The picture's geometry, for callers that need to describe it rather than
    /// sample it.
    pub fn geometry(&self) -> &PolarGeometry {
        self.field.geometry()
    }

    /// What holding this costs, bytes — what the render cache bounds itself by.
    pub fn resident_bytes(&self) -> usize {
        self.field.resident_bytes()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod hover_tests;
