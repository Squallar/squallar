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
    /// **Host bytes the pinned `scan` is holding**, by
    /// [`crate::scan_size::scan_bytes`] — carried rather than computed on
    /// demand, because that function is a walk of every radial and the
    /// readers of this figure are a telemetry tick and a cache's byte
    /// budget, both of which ride the frame thread.
    ///
    /// Supplied by the caller because the caller already has it for free:
    /// the loop download cache priced the volume once at arrival
    /// (`LoopDownloadManager::cached_scan_price`), so pinning it here is a
    /// map lookup rather than a second walk.
    scan_bytes: usize,
}

impl SweepGates {
    /// The gates of the sweep `product` at `elevation_deg` was drawn from, or
    /// `None` where this volume cannot answer for that picture.
    pub fn new(
        scan: Arc<Scan>,
        product: RadarProduct,
        elevation_deg: f32,
        scan_bytes: usize,
    ) -> Option<Self> {
        if !product.is_wire_moment() {
            return None;
        }
        let sweep = crate::render::sweep_index_for(&scan, product, elevation_deg)?;
        Some(Self {
            scan,
            sweep,
            product,
            scan_bytes,
        })
    }

    /// What the volume this pins is holding, bytes. O(1) — see the field.
    pub fn scan_bytes(&self) -> usize {
        self.scan_bytes
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

    /// **The polar field alone** — the geometry and, where the render kept
    /// them, the values. Not the volume a loop frame's source pins; that is
    /// [`Self::pinned_volume_bytes`], and [`Self::resident_bytes`] is the
    /// two together.
    ///
    /// Spelled separately because the two land in different census families:
    /// a field is this source's own allocation, a pinned volume is a
    /// decoded `Scan` three other caches may hold `Arc`s of, and summing
    /// them into one family would smuggle radar bytes into a family whose
    /// name says they are not there.
    pub fn field_bytes(&self) -> usize {
        self.field.resident_bytes()
    }

    /// **Host bytes the decoded volume this source keeps alive is holding**,
    /// zero for a source over a render that kept its own numbers.
    ///
    /// A loop frame's source is built by [`Self::from_volume`] and holds an
    /// `Arc<Scan>` so the readout can decode a gate on demand. That volume
    /// is tens of MB and this source is one of its owners: while the loop
    /// download cache still holds the same `Arc` the bytes are priced there
    /// too, and **once that cache evicts the entry this is the only figure
    /// that names them.** Reporting it is not a claim to sole ownership —
    /// see the shared-ownership note on `squallar_egui::heap_census`.
    ///
    /// O(1): the figure was priced once where the volume arrived.
    pub fn pinned_volume_bytes(&self) -> usize {
        self.sweep.as_ref().map_or(0, SweepGates::scan_bytes)
    }

    /// What holding this costs, bytes — the field **and** the volume it pins.
    ///
    /// The volume used to be missing from this figure, which priced a loop
    /// frame pinning a whole decoded scan at the 5.8 KiB of its geometry.
    /// O(1).
    pub fn resident_bytes(&self) -> usize {
        self.field_bytes()
            .saturating_add(self.pinned_volume_bytes())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod hover_tests;
