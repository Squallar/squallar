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
//! Measured on this box, `--release`, holding one render per pane the way a
//! pane, the render cache and the suspend copy all did — a synthetic 720 × 1832
//! surveillance cut at a 8192 ceiling, which lands on a 7328 px raster, each
//! arm in its own process so the renderer's pools cannot confound them:
//!
//! | resident host bytes | grid | this |
//! |---------------------|-----:|-----:|
//! | marginal, per pane  | 214,806,528 B (204.86 MiB) | 5,288,000 B (5.04 MiB) |
//! | six panes           | 1034.31 MiB | **35.22 MiB** |
//!
//! Forty-one times smaller, and the six-pane figure is what a display showing
//! six distinct rasters was actually holding.
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
//!     half-open for the same reason, at the half-width
//!     [`super::l2_wedge_half_widths_deg`] (or
//!     [`super::derived_grid_half_widths_deg`]) gave it — its declared width,
//!     widened to meet a neighbour that is close enough to have measured the
//!     sky between them and *not* to one further off than that, which is the
//!     whole point of those two functions. Those wedges tile, and they overlap
//!     wherever a sweep ran tighter than it declared, so a point can be
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
    /// Half the width it was painted at, degrees —
    /// [`super::l2_wedge_half_widths_deg`]'s answer for this radial, or
    /// [`super::derived_grid_half_widths_deg`]'s for a derived grid's row.
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

#[cfg(test)]
thread_local! {
    /// Gate values read out of a picture on this thread since
    /// [`take_gate_reads`] last took the tally.
    ///
    /// `#[cfg(test)]` and nothing else, so no build that ships pays a `Cell`
    /// bump for a readout. It exists so that [`crate::hover`]'s
    /// `the_hover_lookup_does_not_walk_the_gates` can state its property as a
    /// count of gates read — the same integer on every machine under every
    /// load — rather than as a ratio of two `Instant`s, which on a contended
    /// box is a reading of the rest of the machine.
    ///
    /// Thread-local rather than a `static`: the suite runs its tests in
    /// parallel threads of one process, and a readout runs start to finish on
    /// the thread that asked for it.
    static GATE_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Note the gates an access actually read.
///
/// **`n` is computed from the span the caller took. It is never written as a
/// literal.** A literal is a claim about the code rather than a measurement of
/// it, and the first version of this counter was exactly that: it said `1` at a
/// call site that decoded every gate up to the one asked for, so the guard read
/// 64 while the readout read 6,477, and the test went green over the defect it
/// was written to catch. The `.len()` at each site is the whole difference
/// between a count and an assertion.
///
/// The two accessors the **readout** reaches a gate through are the callers:
/// [`PolarField::at`] for a render that kept its numbers, and
/// [`crate::render::moment_value_at`] for one reading back out of the volume.
/// This is not every gate access in the crate — the fills walk whole radials on
/// purpose and are no business of this counter.
///
/// `moment_value_at` is also the sampler's primitive, so this tallies
/// `gate_sample` too, and in a test build the sampler pays a `Cell` bump per
/// gate. That does not reach the hover test: the tally is thread-local, the
/// suite gives each test its own thread, and the readout runs start to finish
/// on the thread that drained it. A future test that sampled and hovered on one
/// thread would have to drain between the two.
///
/// A reader that bypasses both accessors counts nothing, and the hover test's
/// equality fails on the shortfall rather than passing on a zero. What no
/// counter can catch is an accessor that walks the row and then reports one
/// gate anyway; the guard against that one is that these two are the only gate
/// accessors a readout has, and both are short enough to read.
#[cfg(test)]
pub(crate) fn note_gate_reads(n: u64) {
    GATE_READS.with(|reads| reads.set(reads.get() + n));
}

/// The gate reads since this was last called, and the tally back to zero.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn take_gate_reads() -> u64 {
    GATE_READS.with(|reads| reads.replace(0))
}

/// Where a render's gates are — everything needed to turn a point into a
/// `(radial, gate)`, and nothing else.
///
/// Split from the numbers themselves because the two have wildly different
/// costs and wildly different lifetimes. This is `radials × 8` bytes — 5.8 KiB
/// for a full ring — and it is the half a loop frame keeps, because a loop
/// frame's numbers are already resident in the volume it was rendered from and
/// copying them per frame would cost 14 × 5.03 MiB on a browser's loop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolarGeometry {
    wedges: Vec<Wedge>,
    first_gate_km: f64,
    gate_interval_km: f64,
    gates: usize,
    reach_gates: usize,
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
        if self.gate_interval_km <= 0.0 || self.reach_gates == 0 {
            return None;
        }
        let g = ((ground_km - self.first_gate_km) / self.gate_interval_km + 0.5).floor();
        (g >= 0.0 && g < self.reach_gates as f64).then_some(g as usize)
    }

    /// The wedge each radial was painted over, in the render's radial order.
    pub fn wedges(&self) -> &[Wedge] {
        &self.wedges
    }

    /// How many radials the render walked.
    pub fn radials(&self) -> usize {
        self.wedges.len()
    }

    /// How many gates each radial's row holds — the stride, and what the fill
    /// *declared*.
    pub fn gates(&self) -> usize {
        self.gates
    }

    /// How many of them the render actually reached, which is the bound
    /// [`Self::pick`] answers within.
    ///
    /// Not the same as [`Self::gates`], and the difference is the extent. Every
    /// fill stops at `proj.extent_km`, and [`crate::types::plan_view_extent_km`]
    /// caps that at [`crate::types::MAX_EXTENT_KM`] — so a radial declaring
    /// gates past 470 km has them, and the picture does not. Without this bound
    /// a readout over the corner of the square, outside the disc the data was
    /// drawn in, would name a gate nothing was painted from.
    ///
    /// No product this display draws reaches that far: a WSR-88D surveillance
    /// cut is 1832 × 0.25 km = 458 km and a TDWR long-range reflectivity 417.
    /// It is bounded here anyway, because the alternative is a rule that is
    /// correct only for the sweeps that exist today.
    pub fn reach_gates(&self) -> usize {
        self.reach_gates
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
        self.wedges.is_empty() || self.reach_gates == 0
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
            reach_gates: gates,
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
#[derive(Clone, Debug, Default, PartialEq)]
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
        let index = at.radial * self.geometry.gates + at.gate;
        // Taken as the span it is so that the count below is produced by the
        // access rather than declared about it: a reader that walked the row to
        // reach the gate would hand over the length of its walk. See
        // [`note_gate_reads`].
        let taken = self.values.get(index..=index)?;
        #[cfg(test)]
        note_gate_reads(taken.len() as u64);
        let v = *taken.last()?;
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

impl PolarField {
    /// The header this field's byte form opens with: three counts and two
    /// ranges.
    const HEADER: usize = 4 * 4 + 8 * 2;

    /// This field as bytes, little-endian, for the one boundary that can only
    /// carry buffers.
    ///
    /// The browser's page↔worker port is that boundary, and it is exactly the
    /// reason this exists rather than a `serde` derive: the wire is
    /// `postMessage`, the buffer is *transferred* rather than copied, and what
    /// it costs is one pass to build and one to read. A full ring of a
    /// surveillance cut is 5.03 MiB through it, where the raster grid it
    /// replaced was 16 MiB on that target and 206.75 MiB on desktop.
    ///
    /// Not a general-purpose format and not versioned: the only thing that
    /// writes it and the only thing that reads it ship in the same binary, and
    /// `rustdar_web`'s protocol token already refuses a worker built from
    /// different source.
    pub fn to_bytes(&self) -> Vec<u8> {
        let g = &self.geometry;
        let mut out = Vec::with_capacity(Self::HEADER + g.wedges.len() * 8 + self.values.len() * 4);
        out.extend_from_slice(&(g.wedges.len() as u32).to_le_bytes());
        out.extend_from_slice(&(g.gates as u32).to_le_bytes());
        out.extend_from_slice(&(g.reach_gates as u32).to_le_bytes());
        out.extend_from_slice(&(self.values.len() as u32).to_le_bytes());
        out.extend_from_slice(&g.first_gate_km.to_le_bytes());
        out.extend_from_slice(&g.gate_interval_km.to_le_bytes());
        for w in &g.wedges {
            out.extend_from_slice(&w.azimuth_deg.to_le_bytes());
            out.extend_from_slice(&w.half_width_deg.to_le_bytes());
        }
        for v in &self.values {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// The inverse of [`Self::to_bytes`], or `None` for anything this build did
    /// not write.
    ///
    /// Every length is checked against the buffer rather than trusted, so a
    /// truncated or foreign message answers `None` and the readout goes quiet,
    /// which is what a message from a worker this page cannot understand should
    /// do. The alternative is a panic on a slice index, in a browser, on a
    /// message a user cannot see.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::HEADER {
            return None;
        }
        let u32_at = |i: usize| -> usize {
            u32::from_le_bytes(bytes[i..i + 4].try_into().expect("bounds checked")) as usize
        };
        let f64_at = |i: usize| -> f64 {
            f64::from_le_bytes(bytes[i..i + 8].try_into().expect("bounds checked"))
        };
        let radials = u32_at(0);
        let gates = u32_at(4);
        let reach_gates = u32_at(8);
        let n_values = u32_at(12);
        let first_gate_km = f64_at(16);
        let gate_interval_km = f64_at(24);

        let wedge_bytes = radials.checked_mul(8)?;
        let value_bytes = n_values.checked_mul(4)?;
        if bytes.len()
            != Self::HEADER
                .checked_add(wedge_bytes)?
                .checked_add(value_bytes)?
        {
            return None;
        }
        // A values buffer that is neither empty nor exactly the shape says the
        // two halves disagree about the picture, which no reader can repair.
        if n_values != 0 && n_values != radials.checked_mul(gates)? {
            return None;
        }

        let mut at = Self::HEADER;
        let mut wedges = Vec::with_capacity(radials);
        for _ in 0..radials {
            let f = |i: usize| f32::from_le_bytes(bytes[i..i + 4].try_into().expect("checked"));
            wedges.push(Wedge {
                azimuth_deg: f(at),
                half_width_deg: f(at + 4),
            });
            at += 8;
        }
        let mut values = Vec::with_capacity(n_values);
        for _ in 0..n_values {
            values.push(f32::from_le_bytes(
                bytes[at..at + 4].try_into().expect("checked"),
            ));
            at += 4;
        }
        Some(Self {
            geometry: PolarGeometry {
                wedges,
                first_gate_km,
                gate_interval_km,
                gates,
                reach_gates,
            },
            values,
        })
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
        // The reach falls out of the same pass rather than costing one of its
        // own, and out of what was *painted* rather than what was declared —
        // see `PolarGeometry::reach_gates`. A `fetch_max` per gate in
        // `render_gate` would have answered it too, and would have put an
        // atomic read-modify-write in the loop `POOLED_CELLS` measures.
        let mut reach_gates = 0usize;
        let gates = self.gates;
        let values: Vec<f32> = self
            .values
            .iter_mut()
            .enumerate()
            .map(|(i, v)| {
                let bits = *v.get_mut();
                let v = f32::from_bits(bits);
                // On the bits and not on `is_nan`, because a range-folded gate
                // is painted and its sentinel *is* a NaN — see
                // `RANGE_FOLDED_BITS`. "Was this slot written" is the question.
                if bits != UNPAINTED_BITS && gates > 0 {
                    reach_gates = reach_gates.max(i % gates + 1);
                }
                v
            })
            .collect();
        PolarField {
            geometry: PolarGeometry {
                wedges,
                first_gate_km: self.first_gate_km,
                gate_interval_km: self.gate_interval_km,
                gates,
                reach_gates,
            },
            values,
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
