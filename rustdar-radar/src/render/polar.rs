//! What a plan-view render painted, in the polar frame it painted *from*, and
//! the one way to ask it what lies under a point.
//!
//! # Why this is not a picture
//!
//! Everything else a render hands back is raster-shaped: `side_px²` of RGBA,
//! and — until this module existed — `side_px²` of `f32` beside it, so that a
//! hover could index one number out of it. That grid was a resampled copy of
//! data the readout cannot resolve past. A full-ring reflectivity sweep is
//! 1832 gates × 720 radials, about 5 MiB of measurements;
//! [`crate::types::raster_side_px`] draws it at 7362 px, and the matching value
//! grid is `7362² × 4` = **206.75 MiB** — forty times the data, oversampled
//! four-fold by area on purpose, because [`crate::types::TEXELS_PER_SAMPLE`] is
//! 2.0 in each axis.
//!
//! This is the measurements themselves, at the resolution the radar took them,
//! arranged the way [`super::MercatorProjection::render_gate`] read them.
//!
//! # The rule is `render_gate`'s rule, backwards
//!
//! A readout that picked its gate by any rule of its own would name a different
//! number from the one the colour under the cursor was painted with, and
//! nothing would say so. So [`PolarGeometry::pick`] is written as the exact
//! inverse of the forward paint, term for term:
//!
//!   * **Range.** `render_gate` paints a gate over
//!     `[range_km − gate_interval/2, range_km + gate_interval/2)` — half-open,
//!     because its `t` runs over `[0, 1)`, so the `+2` on the sample counts
//!     raises sample density and never extent — with `range_km` the gate's
//!     centre, `first_gate_km + gate · gate_interval_km`. So the gate holding a
//!     ground range is `floor((ground_km − first_gate_km) / gate_interval_km +
//!     ½)`, and a range outside `[first − ½gi, first + (gates − ½)gi)` is in no
//!     gate at all.
//!   * **Azimuth.** A radial is painted over `[centre − half, centre + half)`,
//!     half-open for the same reason, at the width
//!     [`super::l2_wedge_width_deg`] (or [`super::derived_grid_wedge_deg`])
//!     gave it — *not* at the spacing to its neighbour, which is the whole
//!     point of those two functions. Those wedges normally tile, but they
//!     overlap wherever a sweep ran tighter than it declared, so a point can be
//!     inside two.
//!   * **The tie.** When it is, [`super::write_key`] decides, and it ranks
//!     claims radial-major: the greatest claim wins a `fetch_max`, so the
//!     winner is the **highest radial index** whose wedge holds the point.
//!     [`PolarGeometry::pick`] takes exactly that one. Within a radial there is
//!     no tie to break — one ground range falls in one gate.
//!
//! # Where it disagrees with the raster, and why the gate is right
//!
//! The raster quantizes and a point does not. `render_gate` walks sample points
//! and truncates them onto a pixel grid nothing aligns them to, so inside the
//! range where a radial's arc is narrower than one pixel — about 14 km at
//! 8 px/km and 0.5° — many radials land in the same pixel and `fetch_max` hands
//! it to the highest-indexed of *all* of them, not to the one the cursor is
//! inside. Out there the two answers are the same gate; in there the raster's
//! answer is an arbitrary survivor of a quantization the reader cannot see, and
//! this module's is the gate the pointer is actually on.
//!
//! `super::tests::the_polar_field_answers_what_the_value_grid_holds` measures
//! how far in that reaches on a real sweep and pins that everything beyond it
//! agrees exactly.

use std::sync::atomic::{AtomicU32, Ordering};

/// The sky one radial stood for, as the render painted it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wedge {
    /// The radial's own azimuth, degrees clockwise from true north.
    pub azimuth_deg: f32,
    /// Half the width it was painted at, degrees — half of
    /// [`super::l2_wedge_width_deg`]'s answer, or of
    /// [`super::derived_grid_wedge_deg`]'s.
    pub half_width_deg: f32,
}

impl Wedge {
    /// The wedge of a radial that never reached
    /// [`super::MercatorProjection::render_gate`] — every gate on it was below
    /// threshold, or past the extent, or the sweep carried no moment for it.
    ///
    /// Not the same as a zero width, which is a wedge the renderer *clamped*
    /// and painted as a spoke. [`Self::contains`] declines both, so the
    /// distinction costs nothing at the call site; it is kept because a blank
    /// radial answering for its neighbours is exactly the failure
    /// [`super::l2_wedge_width_deg`] exists to prevent.
    pub const UNPAINTED: Self = Self {
        azimuth_deg: f32::NAN,
        half_width_deg: f32::NAN,
    };

    /// Whether `azimuth_deg` is inside the sky this radial was painted over.
    ///
    /// Half-open on the far side, matching `render_gate`'s `t ∈ [0, 1)`: a
    /// point exactly on the seam between two tiling wedges belongs to the one
    /// that starts there, which is the one whose samples reach it.
    fn contains(&self, azimuth_deg: f64) -> bool {
        let half = f64::from(self.half_width_deg);
        if !half.is_finite() || !self.azimuth_deg.is_finite() {
            return false;
        }
        let delta = wrap_deg(azimuth_deg - f64::from(self.azimuth_deg));
        delta >= -half && delta < half
    }
}

/// `a` folded onto `(-180, 180]`, so a wedge spanning north is one interval and
/// not two.
fn wrap_deg(a: f64) -> f64 {
    let mut a = a % 360.0;
    if a > 180.0 {
        a -= 360.0;
    } else if a <= -180.0 {
        a += 360.0;
    }
    a
}

/// One gate of one radial, in the order the render walked them.
///
/// For a Level II sweep that order is `Sweep::radials`', so a consumer holding
/// the same scan indexes the two alike — which is what lets a loop frame keep
/// this module's geometry and read its numbers straight out of the volume it
/// was rendered from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateAt {
    /// Index into [`PolarGeometry::wedges`].
    pub radial: usize,
    /// Index along that radial, from gate 0 at
    /// [`PolarGeometry::first_gate_km`].
    pub gate: usize,
}

/// Where a render's gates are — everything needed to turn a point into a
/// `(radial, gate)`, and nothing else.
///
/// Split from the numbers themselves because the two have wildly different
/// costs and wildly different lifetimes. This is `radials × 8` bytes — 5.8 KiB
/// for a full ring — and it is the half a loop frame keeps, because a loop
/// frame's numbers are already resident in the volume it was rendered from and
/// copying them per frame would cost 14 × 5.03 MiB on a browser's loop.
#[derive(Clone, Debug, Default)]
pub struct PolarGeometry {
    wedges: Vec<Wedge>,
    first_gate_km: f64,
    gate_interval_km: f64,
    gates: usize,
}

impl PolarGeometry {
    /// The gate `render_gate` painted the point at (`azimuth_deg`,
    /// `ground_km`) from, or `None` where it painted no gate there.
    ///
    /// `azimuth_deg` and `ground_km` are what
    /// [`crate::beam::site_bearing_range_km`] answers for a position — this
    /// crate's one spelling of "where is this point, from the radar" — so a
    /// caller does not have to know that a gate is measured along a slanted
    /// beam and drawn on the ground beneath it. The foreshortening is already
    /// in [`Self::first_gate_km`] and [`Self::gate_interval_km`], because it
    /// was already in the numbers `render_gate` was called with.
    ///
    /// **This is the module's whole subject; see the module docs for why each
    /// term is what it is.** `None` here means "no gate", not "no value" — a
    /// gate that was painted with nothing to say is a question for whatever
    /// holds the numbers.
    pub fn pick(&self, azimuth_deg: f64, ground_km: f64) -> Option<GateAt> {
        let gate = self.gate_at(ground_km)?;
        // Radial-major, greatest wins — `write_key`'s ordering, which is the
        // order a single-threaded render would have written in. Walked
        // downwards so the common case, a point inside exactly one wedge,
        // stops at the first hit rather than scanning to the end to take a
        // maximum it already had.
        let radial = (0..self.wedges.len())
            .rev()
            .find(|&i| self.wedges[i].contains(azimuth_deg))?;
        Some(GateAt { radial, gate })
    }

    /// The gate whose footprint holds `ground_km`, or `None` past either end of
    /// a radial.
    fn gate_at(&self, ground_km: f64) -> Option<usize> {
        if self.gate_interval_km <= 0.0 || self.gates == 0 {
            return None;
        }
        let g = ((ground_km - self.first_gate_km) / self.gate_interval_km + 0.5).floor();
        (g >= 0.0 && g < self.gates as f64).then_some(g as usize)
    }

    /// The wedge each radial was painted over, in the render's radial order.
    pub fn wedges(&self) -> &[Wedge] {
        &self.wedges
    }

    /// How many radials the render walked.
    pub fn radials(&self) -> usize {
        self.wedges.len()
    }

    /// How many gates each of them carries.
    pub fn gates(&self) -> usize {
        self.gates
    }

    /// The ground range of gate 0's centre, km.
    pub fn first_gate_km(&self) -> f64 {
        self.first_gate_km
    }

    /// One gate's ground depth, km.
    pub fn gate_interval_km(&self) -> f64 {
        self.gate_interval_km
    }

    /// Whether this describes no gates at all, which is what a render that
    /// painted nothing produces.
    pub fn is_empty(&self) -> bool {
        self.wedges.is_empty() || self.gates == 0
    }

    /// What holding this costs, bytes.
    pub fn resident_bytes(&self) -> usize {
        self.wedges.len() * std::mem::size_of::<Wedge>()
    }

    /// Build one directly, for callers that hold a polar layout already and are
    /// not going through a render.
    pub fn from_parts(
        wedges: Vec<Wedge>,
        first_gate_km: f64,
        gate_interval_km: f64,
        gates: usize,
    ) -> Self {
        Self {
            wedges,
            first_gate_km,
            gate_interval_km,
            gates,
        }
    }
}

/// A render's geometry with the numbers it painted, row-major `radials ×
/// gates`.
///
/// The values are `f32::NAN` where the render painted nothing and
/// [`super::RANGE_FOLDED_BITS`] where it painted a range-folded gate — the same
/// two states the raster value grid carried, kept so that a readout derived
/// from this prints exactly what one derived from that printed.
#[derive(Clone, Debug, Default)]
pub struct PolarField {
    geometry: PolarGeometry,
    values: Vec<f32>,
}

impl PolarField {
    /// The geometry alone — the half a loop frame keeps.
    pub fn geometry(&self) -> &PolarGeometry {
        &self.geometry
    }

    /// Give up the numbers and keep the geometry, for a render whose caller
    /// asked for the picture and not the values.
    ///
    /// The counterpart of `rustdar_frontend`'s `values_wanted`, and the reason
    /// it can stay a request rather than becoming two render paths: a loop
    /// frame drops 5.03 MiB of numbers it can read back out of the volume it
    /// was rendered from and keeps 5.8 KiB of geometry it cannot.
    pub fn strip_values(&mut self) {
        self.values = Vec::new();
    }

    /// Whether the numbers are resident. `false` after [`Self::strip_values`],
    /// and for a render that painted nothing at all.
    pub fn has_values(&self) -> bool {
        !self.values.is_empty()
    }

    /// The value at a gate this render walked, or `None` where it painted
    /// nothing there.
    ///
    /// A range-folded gate answers `None`, matching the grid this replaced:
    /// it is a reading, and it claims its pixel and takes the folded colour,
    /// but it has no number to print — `RenderBuffers::into_output` erases the
    /// sentinel from the values in the same pass that paints the colour.
    pub fn at(&self, at: GateAt) -> Option<f32> {
        if at.gate >= self.geometry.gates {
            return None;
        }
        let v = *self.values.get(at.radial * self.geometry.gates + at.gate)?;
        (!v.is_nan()).then_some(v)
    }

    /// What holding this costs, bytes — what the render cache bounds itself by.
    pub fn resident_bytes(&self) -> usize {
        self.geometry.resident_bytes() + self.values.len() * std::mem::size_of::<f32>()
    }

    /// Build one directly, for tests and for callers holding a polar grid.
    ///
    /// `values` is `geometry.radials() × geometry.gates()`, row-major.
    pub fn from_parts(geometry: PolarGeometry, values: Vec<f32>) -> Self {
        debug_assert!(
            values.is_empty() || values.len() == geometry.radials() * geometry.gates(),
            "a polar field is exactly radials × gates, or nothing"
        );
        Self { geometry, values }
    }
}

/// The shape a render declares its polar source to have, so the buffer that
/// records it can be sized before the first gate is painted.
///
/// Every rasterization path in [`super`] knows all three up front — a sweep's
/// radials and its moment's gate count, or a derived grid's rows and columns —
/// and none of them can be recovered from the raster afterwards, which is the
/// whole reason the field exists.
#[derive(Clone, Copy, Debug)]
pub(super) struct PolarShape {
    /// How many radials (or grid rows) the fill will walk.
    pub radials: usize,
    /// The most gates any one of them carries.
    pub gates: usize,
    /// Gate 0's ground range, km — foreshortened exactly as the fill will
    /// foreshorten it.
    pub first_gate_km: f64,
}

/// The field under construction, written by
/// [`super::MercatorProjection::render_gate`] as it paints.
///
/// # Recorded where it is painted, not beside it
///
/// The alternative was for each of the nine rasterization paths to assemble its
/// own field from the same polar source it hands the gate loop. That is one
/// more thing per path to keep true, and the failure it invites is silent: a
/// field built from a source the loop reads differently — a `break` the field
/// does not take, a `continue` it does not make — describes a picture that was
/// never drawn, and the readout then disagrees with the colour under it in
/// exactly the cases the eye cannot check.
///
/// `render_gate` is the one place a `(radial, gate)` and the value painted from
/// it exist together, so it is where they are recorded. Every path gets the
/// field by construction, and a path that grows a new filter gets it too.
///
/// # Why atomics
///
/// The fills run [`crate::par`]'s `par_iter` over radials, so the writes are on
/// many threads. They do not contend: a `(radial, gate)` is painted once, by
/// the one thread that owns that radial, so every value slot has exactly one
/// writer, and the two wedge slots have one writer each storing the same
/// number once per gate. Relaxed stores are all either needs, and they are what
/// the cell buffer beside them already uses — see [`super::RenderBuffers`].
pub(super) struct PolarBuffers {
    values: Vec<AtomicU32>,
    azimuth: Vec<AtomicU32>,
    half_width: Vec<AtomicU32>,
    gates: usize,
    first_gate_km: f64,
    gate_interval_km: f64,
}

/// The bits [`PolarBuffers`] leaves where nothing was painted — `f32::NAN`.
const UNPAINTED_BITS: u32 = 0x7FC0_0000;

impl PolarBuffers {
    /// A field of `shape`, every gate unpainted and every wedge unrecorded.
    ///
    /// `gate_interval_km` is the fill's own ground sample spacing —
    /// [`super::FieldRadial::sample_km`], the same number it hands
    /// `render_gate` as the gate depth — rather than a fourth member of
    /// [`PolarShape`], because a field whose gate depth disagreed with the
    /// spacing the raster was *sized* from would be describing a different
    /// sweep from the one on the glass.
    pub(super) fn new(shape: PolarShape, gate_interval_km: f64) -> Self {
        let cells = shape.radials.saturating_mul(shape.gates);
        Self {
            values: (0..cells).map(|_| AtomicU32::new(UNPAINTED_BITS)).collect(),
            azimuth: (0..shape.radials)
                .map(|_| AtomicU32::new(UNPAINTED_BITS))
                .collect(),
            half_width: (0..shape.radials)
                .map(|_| AtomicU32::new(UNPAINTED_BITS))
                .collect(),
            gates: shape.gates,
            first_gate_km: shape.first_gate_km,
            gate_interval_km,
        }
    }

    /// Record one gate as `render_gate` paints it.
    ///
    /// An index outside the declared shape is dropped rather than panicking. A
    /// path that walks more than it declared has a bug and the `debug_assert`
    /// is where it is caught; in release, losing the tail of one radial's
    /// readout is a better failure than a rasterizer that panics mid-sweep on
    /// a volume nobody can re-download.
    #[inline]
    pub(super) fn paint(
        &self,
        at: super::GateId,
        azimuth_deg: f64,
        half_width_deg: f64,
        value: f32,
    ) {
        debug_assert!(
            at.radial < self.azimuth.len() && at.gate < self.gates,
            "gate ({}, {}) is outside the declared polar shape ({} radials × {} gates)",
            at.radial,
            at.gate,
            self.azimuth.len(),
            self.gates
        );
        if let Some(slot) = self.azimuth.get(at.radial) {
            slot.store((azimuth_deg as f32).to_bits(), Ordering::Relaxed);
        }
        if let Some(slot) = self.half_width.get(at.radial) {
            slot.store((half_width_deg as f32).to_bits(), Ordering::Relaxed);
        }
        if at.gate < self.gates
            && let Some(slot) = self.values.get(at.radial * self.gates + at.gate)
        {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    /// The finished field. Rasterization is over by the time this runs, and
    /// taking `self` by value is what proves it, so every load below is a plain
    /// one rather than an atomic read.
    pub(super) fn into_field(mut self) -> PolarField {
        let wedges = self
            .azimuth
            .iter_mut()
            .zip(self.half_width.iter_mut())
            .map(|(a, h)| Wedge {
                azimuth_deg: f32::from_bits(*a.get_mut()),
                half_width_deg: f32::from_bits(*h.get_mut()),
            })
            .collect();
        let values = self
            .values
            .iter_mut()
            .map(|v| f32::from_bits(*v.get_mut()))
            .collect();
        PolarField {
            geometry: PolarGeometry {
                wedges,
                first_gate_km: self.first_gate_km,
                gate_interval_km: self.gate_interval_km,
                gates: self.gates,
            },
            values,
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
