//! What the readout under the pointer reads, and where it reads it from.

use crate::render::polar::{GateAt, PolarField, PolarGeometry};
use crate::types::RadarProduct;
use nexrad_model::data::Scan;

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

/// The gates of the one sweep a picture was drawn from.
///
/// **This holds the drawn sweep's moments and no reference to the volume they
/// came out of.** That is the invariant, and it is what makes a decoded
/// volume freeable by whoever else holds it: a stored loop frame owning an
/// `Arc<Scan>` would keep the whole volume resident for as long as the
/// picture was on the glass, however the loop download cache evicted, and one
/// of these exists per textured frame — so the volume term would be
/// multiplied by the render budget (36 frames desktop, 18 mobile, 14 wasm)
/// rather than bounded by the cache.
///
/// `at` reaches one moment of one radial of one sweep and nothing else, so
/// that is what is kept. The readout is therefore unchanged **by
/// construction**: the moments are cloned out and read by the same
/// [`crate::render::moment_value_at`] over the same bytes.
#[derive(Clone)]
pub struct SweepGates {
    /// One entry per radial of the drawn sweep, in radial order, holding what
    /// [`RadarProduct::get_moment`] answered for that radial — `None` where it
    /// answered `None`, so a radial that never carried this moment still reads
    /// as unpainted rather than as the next radial's gates.
    radials: Vec<Option<nexrad_model::data::MomentData>>,
    /// **Host bytes the moments above are holding**, by the same convention
    /// [`crate::scan_size`] prices a volume with — the gate buffers, the
    /// vector's own slots, and one allocator block apiece.
    ///
    /// Carried rather than computed on demand, because the readers of this
    /// figure are a telemetry tick and a cache's byte budget and both ride
    /// the frame thread.
    ///
    /// **SHARED with the volume, since `nexrad_model::data::GateBuffer`.**
    /// Cloning a moment out of a sweep now bumps a refcount rather than
    /// copying its gates, so while the volume this was taken from is still
    /// alive these bytes are its bytes and this figure names them a second
    /// time. It is the same convention `scan_size` prices a volume with,
    /// deliberately, and it is the right figure for the question a cache's
    /// budget asks — what this entry is keeping alive once the volume goes —
    /// but it is NOT additive with a live volume's own price. The trade is
    /// strictly in the heap's favour either way: the pair used to be two
    /// copies of these bytes and is now one.
    bytes: usize,
}

impl SweepGates {
    /// The gates of the sweep `product` at `elevation_deg` was drawn from, or
    /// `None` where this volume cannot answer for that picture.
    ///
    /// **Takes the volume and does not keep it.** The sweep's moments are
    /// cloned out and `scan` is released at the end of this call, which is
    /// what lets the loop download cache's eviction actually free a volume.
    /// One walk of one sweep's radials, once per landed loop frame.
    pub fn new(scan: &Scan, product: RadarProduct, elevation_deg: f32) -> Option<Self> {
        if !product.is_wire_moment() {
            return None;
        }
        // The render's own sweep selection, not a second one.
        let index = crate::render::sweep_index_for(scan, product, elevation_deg)?;
        let sweep = scan.sweeps().get(index)?;
        let radials: Vec<Option<nexrad_model::data::MomentData>> = sweep
            .radials()
            .iter()
            .map(|radial| product.get_moment(radial).cloned())
            .collect();
        let bytes = radials
            .iter()
            .flatten()
            .fold(container_bytes(radials.len()), |sum, moment| {
                sum.saturating_add(crate::scan_size::gate_bytes(moment))
            });
        Some(Self { radials, bytes })
    }

    /// What this is holding, bytes. O(1) — see the field.
    pub fn scan_bytes(&self) -> usize {
        self.bytes
    }

    /// The value at a gate, decoded on demand.
    fn at(&self, at: GateAt) -> Option<f32> {
        let moment = self.radials.get(at.radial)?.as_ref()?;
        let raw = crate::render::moment_value_at(moment, at.gate)?;
        crate::render::painted_moment_value(raw).filter(|v| !v.is_nan())
    }
}

/// The `Vec` of moments' own slots and the one block holding them.
///
/// `collect` into a `Vec` from a sized iterator asks for exactly `len` slots,
/// so capacity is length here and is not read back off the vector.
fn container_bytes(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    len.saturating_mul(size_of::<Option<nexrad_model::data::MomentData>>())
        .saturating_add(crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD)
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
    /// them, the values. Not the sweep a loop frame's source holds beside it;
    /// that is [`Self::pinned_volume_bytes`], and [`Self::resident_bytes`] is
    /// the two together.
    ///
    /// Spelled separately because the two land in different census families:
    /// a field is the picture's own grid and a sweep's moments are radar
    /// data, and summing them into one family would smuggle radar bytes into
    /// a family whose name says they are not there.
    pub fn field_bytes(&self) -> usize {
        self.field.resident_bytes()
    }

    /// **Host bytes the drawn sweep's moments this source holds**, zero for a
    /// source over a render that kept its own numbers.
    ///
    /// A loop frame's source is built by [`Self::from_volume`] and holds the
    /// gates of the one sweep its picture was drawn from, so the readout can
    /// decode a gate on demand. Those gates are this source's own allocation
    /// and are shared with nothing — see [`SweepGates`] — so this figure is
    /// what dropping the source frees, exactly, and no other holder names
    /// these bytes.
    ///
    /// **The name says "volume" and the thing is a sweep** because the name
    /// is the census family's, and a family renamed is a row a reader cannot
    /// follow across the campaign's own measurements.
    ///
    /// O(1): the figure was priced once where the sweep was extracted.
    pub fn pinned_volume_bytes(&self) -> usize {
        self.sweep.as_ref().map_or(0, SweepGates::scan_bytes)
    }

    /// What holding this costs, bytes — the field **and** the sweep beside it.
    ///
    /// O(1).
    pub fn resident_bytes(&self) -> usize {
        self.field_bytes()
            .saturating_add(self.pinned_volume_bytes())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod hover_tests;
