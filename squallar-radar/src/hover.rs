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
    /// **Takes the volume and does not keep the `Scan`.** The sweep's moments
    /// are cloned out and `scan` is released at the end of this call, so the
    /// loop download cache's eviction is free to drop the volume.
    ///
    /// It does **not** follow that the eviction frees the volume's bytes, and
    /// this doc said it did until 2026-09-09 — contradicting
    /// [`Self::bytes`]'s own note eighty lines above, which has been right
    /// since `7db617aa6`. `get_moment(radial).cloned()` clones a `MomentData`
    /// whose `values` is a `GateBuffer` = `Arc<Vec<u8>>`, so what is cloned
    /// out is a refcount per gate array. Until every other holder lets go,
    /// these gates stay resident and this frame is one of the things keeping
    /// them so.
    ///
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

/// **The drawn picture's own code plane, borrowed rather than copied** — the
/// readout for a frame whose surface is a fan.
///
/// This is the cheap half of the pair [`SweepGates`] is the expensive half of,
/// and the difference is whose allocation it is. A `SweepGates` clones one
/// moment per radial out of a decoded volume and so keeps that volume's gate
/// arrays alive; this holds the **same `Arc<Vec<u8>>` the `FanSweep` on the
/// glass is drawn from**, so it adds a refcount and no bytes.
///
/// `codes` is level 0 followed by the mip chain, exactly as the picture's
/// payload lays it out, and level 0 is `radials * gates` bytes at offset 0.
/// The readout samples level 0 only: the chain is what the *picture* is
/// reduced through and a number under the pointer must never be a reduction
/// of the gate the user is looking at.
///
/// **Reads identically to a still pane's field**, and that is a property of
/// this code rather than a hope: [`Self::at`] is [`PolarField::at`]'s body
/// over the same table — bounds, index, table lookup, and the same NaN filter
/// that makes an unpainted gate read `Unpainted` instead of a number.
#[derive(Clone)]
pub struct CodedGates {
    /// The picture's payload, shared with the `FanSweep` that draws it.
    codes: Arc<Vec<u8>>,
    /// What each code decodes to — `crate::render::codes::CodePlane::value_table`,
    /// 256 entries, shared with the same payload.
    table: Arc<Vec<f32>>,
    radials: usize,
    gates: usize,
}

impl CodedGates {
    /// A readout over a fan's own payload, or `None` where the four parts do
    /// not describe one picture.
    ///
    /// **Refused rather than repaired**, for the reason
    /// [`PolarField::from_code_table`] gives at the same door: a short buffer
    /// or a foreign table answers a plausible number for a gate nobody
    /// measured. The buffer may be *longer* than level 0 — it carries the mip
    /// chain — so the check is `>=` where that one's is `==`.
    pub fn new(
        codes: Arc<Vec<u8>>,
        table: Arc<Vec<f32>>,
        radials: usize,
        gates: usize,
    ) -> Option<Self> {
        if table.len() != crate::render::codes::LUT_ENTRIES {
            return None;
        }
        let level0 = radials.checked_mul(gates)?;
        if level0 == 0 || codes.len() < level0 {
            return None;
        }
        Some(Self {
            codes,
            table,
            radials,
            gates,
        })
    }

    /// The value at a gate, decoded through the table.
    fn at(&self, at: GateAt) -> Option<f32> {
        if at.gate >= self.gates || at.radial >= self.radials {
            return None;
        }
        let code = *self.codes.get(at.radial * self.gates + at.gate)?;
        let v = *self.table.get(usize::from(code))?;
        (!v.is_nan()).then_some(v)
    }

    /// **Zero, and deliberately** — both allocations are the picture's, priced
    /// where the picture is priced (`squallar_egui::radar_fan::FanSweep::resident_bytes`).
    /// Charging them here as well would name one buffer in two census families
    /// and report a saving that is a second spelling of a cost.
    pub fn resident_bytes(&self) -> usize {
        0
    }

    /// The payload this readout shares, for asserting **by identity** that it
    /// is the picture's own allocation and not a copy of it.
    pub fn codes_arc(&self) -> &Arc<Vec<u8>> {
        &self.codes
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
    /// The picture's own code plane, for a frame drawn as a fan — the source
    /// that costs nothing because the picture is already holding it.
    coded: Option<CodedGates>,
}

impl HoverSource {
    /// A source over a render that kept its numbers — a still pane's.
    pub fn resident(field: PolarField) -> Self {
        Self {
            field,
            sweep: None,
            coded: None,
        }
    }

    /// A source over a render whose numbers were dropped, reading them back out
    /// of the volume it was drawn from — a loop frame's.
    pub fn from_volume(field: PolarField, sweep: Option<SweepGates>) -> Self {
        Self {
            field,
            sweep,
            coded: None,
        }
    }

    /// **A source over a loop frame drawn as a fan**, reading its numbers out
    /// of the payload the picture is already holding.
    ///
    /// This is the arm that lets a loop frame answer a hover without a
    /// `SweepGates`, which is the whole of the saving: no moment is cloned out
    /// of a volume, no volume is pinned, and no walk of a sweep's radials
    /// happens on the frame thread to build one.
    pub fn from_coded_plane(field: PolarField, coded: CodedGates) -> Self {
        Self {
            field,
            sweep: None,
            coded: Some(coded),
        }
    }

    /// A source over nothing, for a pane with no picture yet.
    pub fn empty() -> Self {
        Self {
            field: PolarField::default(),
            sweep: None,
            coded: None,
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
        // The picture's own plane before the volume behind it: it is already
        // resident, it needs no volume to still be cached, and it decodes the
        // same number. A gate it finds unpainted is unpainted — the same
        // reading the field arm above gives.
        if let Some(coded) = self.coded.as_ref() {
            return match coded.at(at) {
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
    /// decode a gate on demand.
    ///
    /// **This is an upper bound and not what dropping the source frees.** This
    /// paragraph claimed the opposite — "shared with nothing", "what dropping
    /// the source frees, exactly" — until 2026-09-10, contradicting
    /// [`SweepGates::bytes`] eighty lines above, which has said since
    /// `7db617aa6` that cloning a moment bumps a `GateBuffer` refcount rather
    /// than copying gates. While the volume the sweep came out of is still
    /// cached these are that volume's bytes and this names them a second time;
    /// `LoopFrameStore::sole_pinned_volume_bytes` is the lower bound beside
    /// it.
    ///
    /// **Zero for a fan-drawn loop frame**, whose readout is
    /// [`CodedGates`] over the picture's own payload — see
    /// [`Self::from_coded_plane`]. That is the saving this figure is the
    /// scoreboard for.
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

    /// The picture's code plane this source reads through, where it has one.
    ///
    /// Exposed so a test can assert **by identity** — `Arc::ptr_eq` against
    /// the `FanSweep`'s own payload — that the readout borrows the picture
    /// rather than copying it. The saving is exactly that identity, and a
    /// call graph is not evidence of it.
    pub fn coded_plane(&self) -> Option<&CodedGates> {
        self.coded.as_ref()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod hover_tests;
