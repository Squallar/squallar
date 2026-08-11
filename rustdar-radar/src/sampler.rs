//! A native-geometry volume sampler: point queries against the sweeps a
//! `Scan` already holds, with no resampling grid in between.
//!
//! Everything this crate draws today rasterizes *from* the radials outward —
//! walk the gates, paint where each one lands. A cross-section and a voxel
//! grid need the opposite direction: given a place, what did the radar measure
//! there. [`VolumeSampler`] is that direction, and it borrows the [`Scan`]
//! rather than gridding it, because a 15-tilt volume is ~10 M gates and the
//! answer to any one query touches six of them.
//!
//! # What a query costs, and why [`VolumeSampler::column`] exists
//!
//! One rung costs four gate reads — a bilinear in azimuth × slant range — and
//! a whole column is one of those per rung, `4·N`, ~64 on a 16-rung VCP 212
//! ladder. Every height after the first is then free: it is a two-point lerp
//! between rungs already sampled, and reads no gates at all.
//!
//! [`VolumeSampler::sample`] answers one point by building the column and
//! asking it, so a `W × H` section evaluated per pixel is `W·H·4·N` gate reads
//! against `W·4·N` for one column per output column — an **`H`-fold** saving,
//! 1024× on a 1024-row section. (The plan's "~8×" compares against a
//! per-pixel path that computed only the bracketing pair; this one does not,
//! deliberately — see [`VolumeSampler::sample`].) Both consumers on the way,
//! the cross-section rasterizer and the voxel builder, are column-shaped, so
//! the primitive is here rather than duplicated in each.
//!
//! # The tilt ladder
//!
//! **Settled by measurement over 203 real volumes plus a 60-volume holdout;
//! do not re-derive it.** For each sweep take `n = elevation_number()`, then
//!
//! ```text
//! key = coverage_pattern().elevation_cuts()[n - 1].elevation_angle_degrees()
//! if key > 180.0 { key -= 360.0 }          // two's-complement negative cuts
//! ```
//!
//! Group by **exact** `key`; one rung per group, ascending. Within a rung, per
//! moment, newest-first in volume order: a non-Doppler moment prefers a sweep
//! whose radials carry **no** velocity (falling back to any), a Doppler moment
//! (velocity, spectrum width) takes any. The rung's *geometric* elevation is
//! [`crate::volumetric::sweep_elevation_deg`] of the chosen sweep — the
//! nominal cut angle is the **grouping key only**, since measured medians sit
//! up to 0.044° off it.
//!
//! Scored against the VCP's own cut table this is **0 violations on all 203
//! volumes**, on all 19 mid-flight-join and 19 abandoned-tail variants, and on
//! a frozen-rule holdout of 30 untouched sites on a different day.
//! `elevation_number()` indexes the cut table on 203/203 sweeps: the RDA
//! already says which cut a sweep belongs to, so no angular inference is
//! needed.
//!
//! **No angular threshold can work, and this is why it is not a matter of
//! taste.** KBMX (VCP 212, adaptive base tilt) declares genuine cuts at 0.40°
//! and 0.48° — 0.09° apart — while the spread of first-radial angles *within*
//! the 0.48° cut is 0.088° and the gap *to* the 0.40° cut is also 0.088°. The
//! windows touch exactly. At a 0.2° merge threshold the rule does not fuse two
//! rungs, it makes the whole 0.48° cut **vanish**, leaving a plausible
//! 14-rung monotone ladder with reflectivity on every rung and one genuine cut
//! silently deleted. Thresholds of 0.10/0.15/0.20/0.30 failed 1/2/2/3 of 19
//! detailed volumes and 12 of 124 in the survey, reproduced independently on
//! the holdout at KDGX. `the_ladder_separates_cuts_no_angular_threshold_can`
//! reproduces the KBMX geometry and pins both halves.
//!
//! **[`crate::volumetric::VolumeCube`]'s rule must not be copied.** It keys
//! rungs on the median radial angle rounded to 0.1°, which violates the cut
//! table on **all 203** volumes — 398 short-half reflectivity, 24 split-cut,
//! 20 rung-count. That is not a live bug for echo tops and VIL, whose grid
//! stops at 230 km while the Doppler half reaches 300 km, so the short half is
//! never the half that matters there. A sampler reaching past 300 km has no
//! such protection.
//!
//! # This module reads the VCP, and that is why it fails loudly
//!
//! An earlier draft of this work said the coverage pattern was *deliberately*
//! not read, so that the reconstructed scan a render worker rebuilds from
//! [`crate::render_input::RenderInput`] would sample identically to the main
//! thread's. The ladder measurement reversed that: the cut table is the only
//! thing that separates KBMX's two base tilts, so the VCP has to be read and
//! therefore has to cross the worker boundary.
//!
//! It does not cross it yet. `render_input`'s `placeholder_coverage_pattern`
//! builds an **empty** cut list, so a sampler that tolerated a placeholder
//! would build a *different ladder in the worker than on the main thread*,
//! with no error and no `NaN` — the exact silent-divergence class this whole
//! feature exists to avoid. So [`VolumeSampler::new`] **refuses** an empty cut
//! table and an `elevation_number` that does not index it, returns a
//! [`SamplerError`] saying which, and logs it. Until the wire carries the cut
//! angles the sampler is *unusable* in the worker rather than quietly wrong.
//! `a_reconstructed_render_input_scan_is_refused` pins that against the real
//! `RenderInput` round trip rather than against a hand-built placeholder.
//!
//! # Two more deliberate omissions
//!
//! [`crate::hca::merge_split_cut_doppler`] is **not** used to fill a
//! surveillance rung's missing velocity. It clones every radial it merges — a
//! second full copy of the volume — which is affordable for the HHC's one
//! 230 km composite and is not affordable per rung. A Doppler moment gets its
//! own rung from its own cut instead, which is what the ladder rule already
//! produces.
//!
//! [`nexrad_model`]'s `SweepField` is not used either. Its elevation key is
//! the *first* radial's angle, its `value_at_polar` is nearest-azimuth with a
//! **floor**ed gate index (a fixed 125 m inward bias), and building one eagerly
//! decodes ~100 MB.
//!
//! # Geometry
//!
//! All of it comes from [`crate::beam`] — 4/3 earth, quadratic height,
//! closed-form inverse. The one thing this module adds is that it **applies
//! the `cos e` slant→ground correction** that `render::render_gate` does not
//! (that function never even receives an elevation angle). The consequence is
//! that a section will not register against the plan view above ~2°:
//! `the_cos_e_correction_diverges_from_the_plan_view_by_a_measured_amount`
//! pins it at 0.2017 km and 4.0151 km at the two tilts, and converts both to
//! pixels for the target being built. The section is the correct one; the
//! divergence is shipped as a measurement, not as a comment.
//!
//! # Status, rather than `NaN`
//!
//! [`SampleStatus`] carries the six reasons a sample has no number, so a hover
//! readout can say "below the lowest beam" instead of nothing.
//! `MomentValue::RangeFolded` is matched **nowhere else** in this crate — six
//! consumers, every one of them `Value`-only — and closing that gap is half
//! the point of the type.
//!
//! There is **no downward or upward extrapolation**. Under the lowest rung's
//! beam the answer is [`SampleStatus::BelowLowestBeam`]; over the highest it is
//! [`SampleStatus::AboveVolume`], which is also how the cone of silence
//! reports itself (over the site every rung's beam is at zero height, so every
//! height above the ground is above the volume). Neither is filled in.
//!
//! Expect, and treat as ordinary, a bracketing rung with **no data**: every
//! volume has one at 230 km and 300 km, and 8 of 19 measured volumes have one
//! at 150 km, because the upper cuts stop short of the surveillance half.
//! That is beam geometry, not a defect in the ladder, and it surfaces as
//! [`SampleStatus::BeyondRange`] on that rung rather than as an error.

use nexrad_model::data::{DataMoment, ElevationCut, MomentData, Radial, Sweep};

use crate::beam;
use crate::nyquist::Volume;
use crate::types::{MomentSlot, RadarProduct};
use crate::volumetric::{CellStat, sweep_elevation_deg};

/// Why a sample has no number — or, for [`SampleStatus::Value`], that it has
/// one.
///
/// The first two mirror `nexrad_model::data::MomentValue`'s own non-numeric
/// arms; the rest are this module's, and describe where the *query* fell
/// rather than what a gate said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleStatus {
    /// A measured value.
    Value,
    /// The gate was below the moment's signal threshold (raw code 0). The
    /// radar looked and saw nothing above threshold — distinct from having no
    /// gate there at all.
    BelowThreshold,
    /// The gate was range folded (raw code 1): its true range is ambiguous
    /// past the unambiguous range of the cut's PRF.
    RangeFolded,
    /// The query height is under the lowest rung's beam centre over that
    /// ground range. The radar never illuminated it; nothing is filled in.
    BelowLowestBeam,
    /// The query height is over the highest rung's beam centre over that
    /// ground range. Over the site this is the whole cone of silence.
    AboveVolume,
    /// The bracketing rung's gates stop short of that ground range. Ordinary,
    /// not exceptional: upper cuts are range-truncated, so every volume has a
    /// bracketing rung with no data at 230 km and at 300 km.
    BeyondRange,
    /// Nothing serves the query: the ladder is empty for this moment, no
    /// radial of the rung is within half a beam of the azimuth, the radial
    /// does not carry the moment, or the point is inside the first gate's
    /// centre range.
    NoCoverage,
}

impl SampleStatus {
    /// A stable byte for the wire, so a section rendered in a worker arrives
    /// with its statuses intact rather than as a field of `NaN`.
    ///
    /// Deliberately **not** the Level II raw gate codes (where 0 is below
    /// threshold and 1 is range folded): four of these seven have no raw code
    /// at all, so borrowing two of them would suggest a correspondence that
    /// does not exist. New variants append; existing codes never move.
    pub fn wire_code(self) -> u8 {
        match self {
            SampleStatus::Value => 0,
            SampleStatus::BelowThreshold => 1,
            SampleStatus::RangeFolded => 2,
            SampleStatus::BelowLowestBeam => 3,
            SampleStatus::AboveVolume => 4,
            SampleStatus::BeyondRange => 5,
            SampleStatus::NoCoverage => 6,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code). `None` for a byte this
    /// build does not know, which is what a payload from a newer sender looks
    /// like.
    pub fn from_wire_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => SampleStatus::Value,
            1 => SampleStatus::BelowThreshold,
            2 => SampleStatus::RangeFolded,
            3 => SampleStatus::BelowLowestBeam,
            4 => SampleStatus::AboveVolume,
            5 => SampleStatus::BeyondRange,
            6 => SampleStatus::NoCoverage,
            _ => return None,
        })
    }
}

/// One query's answer: a status, and a number when the status is
/// [`SampleStatus::Value`].
///
/// The fields are private so the pairing cannot come apart — a `Value` with no
/// number and a `BelowThreshold` carrying one are both nonsense, and both are
/// easy to construct by hand.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    value: f32,
    status: SampleStatus,
}

/// Equality that ignores the number when there is no number to compare.
///
/// **A derived `PartialEq` makes every non-`Value` sample unequal to itself**,
/// because `missing` stores `f32::NAN` as its placeholder and `NaN != NaN`.
/// That is not a theoretical nuisance: WP-D's worker reply asserts
/// `assert_eq!(execute(&…), None)` on a `JobOutput` that transitively contains
/// these, and a whole cross-section of "below the lowest beam" would compare
/// unequal to a byte-identical copy of itself with nothing in the failure
/// message saying why. Values still compare as `f32`, so `found(NAN)` remains
/// unequal to itself — which is what a caller who put a `NaN` in a `Value`
/// asked for.
impl PartialEq for Sample {
    fn eq(&self, other: &Self) -> bool {
        self.status == other.status
            && (self.status != SampleStatus::Value || self.value == other.value)
    }
}

impl Sample {
    /// A measured value.
    pub fn found(value: f32) -> Self {
        Self {
            value,
            status: SampleStatus::Value,
        }
    }

    /// No value, for the stated reason.
    pub fn missing(status: SampleStatus) -> Self {
        debug_assert!(
            status != SampleStatus::Value,
            "Sample::missing(Value) has no number to report; use Sample::found",
        );
        Self {
            value: f32::NAN,
            status,
        }
    }

    /// Why this sample does or does not have a number.
    pub fn status(&self) -> SampleStatus {
        self.status
    }

    /// The measured value, or `None` for any of the six reasons there is not
    /// one.
    pub fn value(&self) -> Option<f32> {
        (self.status == SampleStatus::Value).then_some(self.value)
    }

    /// The measured value or `f32::NAN` — for a raster that keeps its statuses
    /// in a parallel array and wants the value plane unbranched.
    pub fn value_or_nan(&self) -> f32 {
        self.value
    }
}

/// Why a volume cannot be sampled at all. Every arm is a refusal to build a
/// ladder that would be wrong, never a degraded one.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SamplerError {
    /// The product has no native Level II moment to section — see
    /// [`samplable`].
    #[error("{} is not a samplable moment: {reason}", product.name())]
    NotSamplable {
        product: RadarProduct,
        reason: &'static str,
    },

    /// The scan's coverage pattern carries no elevation cuts, so no sweep can
    /// be keyed. This is what a scan reconstructed from a
    /// [`crate::render_input::RenderInput`] looks like, and refusing it is the
    /// whole reason this error exists.
    #[error(
        "the scan's coverage pattern (VCP {vcp}) has no elevation cuts, so no \
         tilt ladder can be built; a scan reconstructed from a RenderInput \
         looks exactly like this, and sampling it would build a different \
         ladder from the one the main thread built"
    )]
    EmptyCoveragePattern { vcp: u16 },

    /// A sweep's elevation number does not index the cut table. Measured to
    /// happen on 0 of 203 real volumes, so it means the pairing of sweeps to
    /// the VCP has broken rather than that the data is unusual.
    #[error(
        "sweep {sweep_index} reports elevation number {elevation_number}, \
         which does not index the coverage pattern's {cut_count} elevation cuts"
    )]
    ElevationNumberOutOfCutTable {
        sweep_index: usize,
        elevation_number: u8,
        cut_count: usize,
    },

    /// A cut angle that is not a number. Cut angles are decoded from a fixed
    /// point field and cannot be non-finite in valid data; a `NaN` key would
    /// silently fail every grouping comparison and scatter one cut across
    /// several rungs.
    #[error("elevation cut {cut_index} has a non-finite angle ({angle})")]
    NonFiniteCutAngle { cut_index: usize, angle: f64 },

    /// The ladder came out empty: no sweep in the volume carries this moment.
    #[error("no sweep in the volume carries {}", product.name())]
    NoSweepsWithMoment { product: RadarProduct },
}

/// The moment a product samples, or `None` if a section of it is meaningless.
///
/// The six native Level II moments are the whole list. Two families are
/// refused on purpose:
///
/// * **The hybrid hydrometeor classification is not a moment.** It is a
///   360 × 920 × 0.25 km hybrid-*scan* composite ([`crate::hhc`]) — one
///   surface, assembled from whichever tilt clears the terrain at each range.
///   It has no vertical extent to cut through, and the UI must not offer it.
/// * **The column integrals already collapsed the vertical axis.** Echo tops,
///   VIL, VIL density, POSH and MEHS are functions of a whole column; a
///   vertical section of one would draw the same number at every height and
///   look like a measurement.
///
/// The *derivations* (NROT, SRV, KDP) are refused **here** for a third
/// reason: they are computed per sweep, so sampling them means deriving them
/// first — refused rather than quietly served from raw velocity, which would
/// look right and be a different field. That derivation now exists:
/// [`crate::derive::prepare`] computes them into synthetic scans, and
/// [`crate::derive::volume_slot`] is the predicate the vertical views gate
/// on. This function stays the **raw-scan** gate, which is exactly why the
/// derived products stay out of it.
pub fn samplable(product: RadarProduct) -> Option<MomentSlot> {
    match product {
        RadarProduct::Reflectivity => Some(MomentSlot::Reflectivity),
        RadarProduct::Velocity => Some(MomentSlot::Velocity),
        RadarProduct::SpectrumWidth => Some(MomentSlot::SpectrumWidth),
        RadarProduct::DifferentialReflectivity => Some(MomentSlot::DifferentialReflectivity),
        RadarProduct::DifferentialPhase => Some(MomentSlot::DifferentialPhase),
        RadarProduct::CorrelationCoefficient => Some(MomentSlot::CorrelationCoefficient),
        _ => None,
    }
}

/// Why [`samplable`] said no, in the words a refusal should carry.
fn refusal_reason(product: RadarProduct) -> &'static str {
    match product {
        RadarProduct::HydrometeorClassification => {
            "it is a hybrid-scan composite, one surface assembled across tilts, \
             not a moment with vertical extent"
        }
        RadarProduct::EchoTops
        | RadarProduct::EchoTopsInterpolated
        | RadarProduct::VerticallyIntegratedLiquid
        | RadarProduct::VilDensity
        | RadarProduct::ProbabilityOfSevereHail
        | RadarProduct::MaxExpectedHailSize => {
            "it is a column integral, so a vertical section of it would draw \
             one number at every height"
        }
        RadarProduct::NormalizedRotation | RadarProduct::StormRelativeVelocity => {
            "it is derived per sweep from a volume wind fit, so it has to be \
             computed before it can be sampled — crate::derive::prepare is \
             that computation, and the door the vertical views go through"
        }
        RadarProduct::SpecificDifferentialPhase => {
            "it is derived per sweep from differential phase, so it has to be \
             computed before it can be sampled — crate::derive::prepare is \
             that computation, and the door the vertical views go through"
        }
        RadarProduct::PrecipitationRate => {
            "it is derived rather than measured, and no Level II moment carries it"
        }
        _ => "no Level II moment stands behind it",
    }
}

/// How two measurements of a moment average.
///
/// `Default` is the plain mean, which is what an empty [`Column`] carries.
/// Nothing reads it there — an empty column has no corners to combine — but a
/// `Default` that silently meant "reflectivity" would be a trap for whoever
/// adds the next constructor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Blend {
    /// Mean in linear `Z = 10^(dBZ/10)`, read back in dBZ. Averaging
    /// reflectivity in dB understates every mixed cell: 10 and 50 dBZ average
    /// to 46.99, not 30.
    LinearZ,
    /// Plain weighted mean.
    #[default]
    Arithmetic,
    /// Weighted mean of the unit vectors, so the 360°→0° seam does not average
    /// to 180°. Differential phase folds at 360° and this crate's own
    /// unfolder ([`crate::kdp`]) exists because of it; a sampler that lerped
    /// across the seam would invent a half-turn of phase.
    Angular360,
}

impl Blend {
    /// The blend a moment's physics wants.
    ///
    /// Reads [`CellStat::for_moment`] for the linear-Z question rather than
    /// restating it, so the sampler and the echo-tops cube cannot come to
    /// disagree about which moments average in dB. The angular arm is this
    /// module's own — `CellStat` has no need of one, because nothing that
    /// consumes it grids differential phase.
    fn for_moment(product: RadarProduct) -> Self {
        match product {
            RadarProduct::DifferentialPhase => Blend::Angular360,
            p if CellStat::for_moment(p) == CellStat::LinearZMean => Blend::LinearZ,
            _ => Blend::Arithmetic,
        }
    }

    /// Whether this moment wraps at a limit that is a property of the sweep
    /// rather than of the quantity — so the limit has to be carried per rung
    /// instead of living in a [`Blend`] variant.
    ///
    /// **Two moments in Level II wrap, and only one of them wraps at a
    /// constant.** Differential phase wraps at 360°, which is a property of
    /// the quantity itself, so [`Blend::Angular360`] can be a blend *arm*:
    /// everything it needs is in the variant. Doppler velocity wraps at the
    /// Nyquist velocity, which is a property of the *sweep's* PRF and differs
    /// from tilt to tilt inside one volume, so it cannot be an arm.
    ///
    /// **The archive carries that number, and it now reaches here.** Message
    /// 31's Radial Data Block has `nyquist_velocity`, `nexrad-decode` decodes
    /// it, and [`crate::scan`] reads it out of a raw
    /// `nexrad_data::volume::File` on the same walk that builds the `Scan` —
    /// the way [`crate::kdp::KdpParams::from_archive`] reads the other
    /// radial-header parameters, except folded in rather than paid for twice.
    /// What used to drop it was the boundary this sampler sits
    /// behind — `nexrad_model::data::Radial` does not carry it, and the
    /// worker's reconstructed `RenderInput` is built from model types alone —
    /// so the number was *measured off the data* instead. It is now carried:
    /// [`crate::nyquist::Volume`] pairs the declared table with the scan, the
    /// payload carries it per sweep, and [`estimate_fold_limit`] is the
    /// fallback for a volume that declared nothing (all Message 1, or a scan
    /// that reached here without a table). Both land in
    /// [`Rung::fold_limit_ms`], and which one did is in
    /// [`Rung::fold_limit_declared`].
    ///
    /// Every other moment this sampler serves is monotone over its encoding:
    /// reflectivity, spectrum width, differential reflectivity and correlation
    /// coefficient have no wraparound topology at all — spectrum width in
    /// particular is a non-negative spread, so no two of its gates can sit on
    /// opposite sides of a seam — and their blends are unaffected.
    fn folds_at_measured_limit(product: RadarProduct) -> bool {
        matches!(product, RadarProduct::Velocity)
    }
}

/// The smallest estimated fold limit that is believed, m/s.
///
/// [`estimate_fold_limit`] reads the largest speed a sweep observed, which is
/// the Nyquist velocity *when the sweep folded at all* and an underestimate
/// when it did not. An underestimate is mostly harmless — see that function —
/// but a sweep that saw nothing faster than a few m/s gives a limit so small
/// that ordinary noise clears it, and then the straddle test fires on air that
/// is merely calm. No operational NEXRAD waveform has a Nyquist velocity this
/// low, so below it the guard is switched off rather than trusted.
///
/// `crate::nrot::dealias_with_knobs` abandons dealiasing under the same 8 m/s
/// on the same reasoning about the same estimator, and the two numbers mean
/// the same thing; they are deliberately equal.
const FOLD_LIMIT_FLOOR_MS: f64 = 8.0;

/// How many median azimuth steps two radials may be apart and still count as
/// adjacent — i.e. as a pair worth interpolating between.
///
/// One step is what consecutive radials are apart by construction, and a real
/// sweep's jitter is a few hundredths of a step, so 1.5 is bracketed from both
/// sides: it is wide enough that a jittered sweep stays one continuous ladder
/// (`azimuth_jitter_does_not_open_a_hole`), and narrow enough that one dropped
/// radial — a gap of **two** steps — falls outside it and is therefore *not*
/// bridged (`an_azimuth_hole_is_reported_rather_than_painted_across`). What
/// happens past it is not a fallback to nearest-across-the-hole, which is how a
/// sampler paints data where the radar never looked: past it a rung serves
/// only the azimuths inside a surviving radial's own half-step footprint —
/// the same footprint `render::render_gate` paints, via
/// `RadialContext::new(azimuth, avg_azimuth_spacing / 2.0)` — and reports
/// [`SampleStatus::NoCoverage`] between them. An abandoned tail therefore
/// leaves a hole, exactly as it does in the plan view.
const MAX_ADJACENT_GAP_STEPS: f64 = 1.5;

/// One rung of the tilt ladder: the sweep that won its cut, indexed for random
/// access.
struct Rung<'a> {
    /// The VCP cut angle this rung was grouped by, wrap-corrected. A key, and
    /// never geometry — measured medians sit up to 0.044° off it.
    nominal_deg: f64,
    /// The chosen sweep's median radial elevation: the angle every height in
    /// this rung is computed from.
    elevation_deg: f64,
    /// The chosen sweep's radials, borrowed from the `Scan`.
    radials: &'a [Radial],
    /// `(azimuth, index into radials)`, ascending by azimuth. Built rather
    /// than assumed: a sweep's radials are in *collection* order, which starts
    /// wherever the antenna was.
    by_azimuth: Vec<(f32, u32)>,
    /// Median gap between adjacent azimuths, degrees — the scale
    /// [`MAX_ADJACENT_GAP_STEPS`] is measured in.
    ///
    /// The median rather than `render::compute_azimuth_spacing`'s mean,
    /// because the sweeps this guard exists for are exactly the ones with one
    /// enormous gap in them: a 400-radial abandoned tail spanning 200° has a
    /// mean step of 0.5° only if you already ignore the 160° hole, while its
    /// median step is 0.5° whether you noticed the hole or not.
    az_step_deg: f64,
    /// The speed this rung's sweep folds at, m/s, or `None` when this moment
    /// has no fold seam, the volume declared nothing *and* the sweep never got
    /// near one, or the number that was found sits under
    /// [`FOLD_LIMIT_FLOOR_MS`]. Resolved once here rather than per sample
    /// because it is a property of the sweep, and a sample must not pay a pass
    /// over the whole sweep.
    ///
    /// Per rung, not per volume: the Nyquist velocity follows the cut's PRF,
    /// and it genuinely differs inside one volume — measured across the six
    /// probe volumes, the low cuts fold at 22.5–31 m/s while the high cuts of
    /// the same volume fold at up to 35.5.
    fold_limit_ms: Option<f64>,
    /// Where [`Self::fold_limit_ms`] came from: `true` for the archive's own
    /// declaration (Message 31's Radial Data Block, carried here by
    /// [`crate::nyquist::DeclaredNyquist`]), `false` for
    /// [`estimate_fold_limit`]'s reading of the data.
    ///
    /// **Nothing in the guard reads this** — the two numbers mean the same
    /// thing and are used identically. It exists so
    /// [`VolumeSampler::describe`] can print the provenance, which is what
    /// makes `a_reconstructed_render_input_scan_builds_the_identical_ladder`
    /// able to see the failure this whole path exists to prevent: the main
    /// thread holding the declared number, the worker's reconstructed scan not
    /// holding it, and the two guarding differently at limits that are close
    /// enough to leave no other symptom.
    ///
    /// `false` when there is no limit at all, which `describe` never prints.
    fold_limit_declared: bool,
}

/// The tilt ladder over one ground point: every rung's beam height there and
/// what it measured, ascending by height.
///
/// Built once per column by [`VolumeSampler::column`] and then asked for as
/// many heights as the caller wants. A rung with no data at this ground range
/// stays in the ladder carrying its status — dropping it would silently widen
/// the bracket and interpolate straight across a tilt that measured nothing,
/// which is the fabrication this type exists to make impossible.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Column {
    azimuth_deg: f64,
    ground_range_km: f64,
    /// Carried from the sampler that filled this column, so
    /// [`Column::at_height_km`] blends reflectivity in linear Z without
    /// needing the sampler back.
    blend: Blend,
    rungs: Vec<ColumnRung>,
}

/// One rung's contribution to a [`Column`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnRung {
    /// Beam-centre height above the antenna, km, over this column's ground
    /// range.
    pub height_km: f64,
    /// The rung's geometric elevation, degrees.
    pub elevation_deg: f64,
    /// What this rung measured at this column's azimuth and ground range.
    pub sample: Sample,
    /// This rung's fold limit, carried from [`Rung::fold_limit_ms`] so
    /// [`Column::at_height_km`] can refuse to lerp across a Nyquist seam
    /// without needing the sampler back — the same reason [`Column::blend`]
    /// is carried.
    ///
    /// Private, unlike its neighbours, because it is a property of the *sweep*
    /// that a reader of one rung's sample has no use for, and publishing it
    /// would invite a caller to compare two rungs' limits and conclude
    /// something about the air.
    fold_limit_ms: Option<f64>,
}

impl Column {
    /// An empty column, which answers [`SampleStatus::NoCoverage`] at every
    /// height. `Default` yields this, which is what
    /// [`VolumeSampler::column_into`] wants as a reusable buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The azimuth this column was taken at, degrees clockwise from north.
    pub fn azimuth_deg(&self) -> f64 {
        self.azimuth_deg
    }

    /// The ground range this column was taken at, km from the site.
    pub fn ground_range_km(&self) -> f64 {
        self.ground_range_km
    }

    /// Every rung, ascending by beam height.
    pub fn rungs(&self) -> &[ColumnRung] {
        &self.rungs
    }

    /// The heights of the lowest and highest rung's beam centres over this
    /// column, km above the antenna. `None` for an empty column.
    ///
    /// A caller drawing a height axis wants this to know where its rows stop
    /// being answerable — the lower bound is the cone of silence's floor at
    /// this range, the upper its ceiling.
    pub fn height_span_km(&self) -> Option<(f64, f64)> {
        Some((self.rungs.first()?.height_km, self.rungs.last()?.height_km))
    }

    /// What the volume holds at `height_km` above the antenna in this column.
    ///
    /// Interpolates between the two rungs that bracket the height. Outside the
    /// ladder nothing is filled in: under the lowest beam the answer is
    /// [`SampleStatus::BelowLowestBeam`], over the highest
    /// [`SampleStatus::AboveVolume`].
    pub fn at_height_km(&self, height_km: f64) -> Sample {
        if !height_km.is_finite() {
            return Sample::missing(SampleStatus::NoCoverage);
        }
        let Some(last) = self.rungs.last() else {
            return Sample::missing(SampleStatus::NoCoverage);
        };

        // `partition_point` counts the rungs at or below the query, so 0 means
        // the query is under the lowest beam and `len` means it is at or over
        // the highest.
        let above = self.rungs.partition_point(|r| r.height_km <= height_km);
        if above == 0 {
            return Sample::missing(SampleStatus::BelowLowestBeam);
        }
        if above == self.rungs.len() {
            return if height_km > last.height_km {
                Sample::missing(SampleStatus::AboveVolume)
            } else {
                last.sample
            };
        }
        let lo = &self.rungs[above - 1];
        let hi = &self.rungs[above];
        let span = hi.height_km - lo.height_km;
        // **This branch is unreachable given finite rung heights, and is kept
        // anyway.** `partition_point` over the ascending sort guarantees
        // `lo.height ≤ h < hi.height`, so the span is strictly positive; two
        // rungs *can* share a height (every beam centre is at zero over the
        // site, and two cuts can share a median), but then they are both at or
        // below `h` or both above it and neither becomes a bracket.
        //
        // The qualifier is load-bearing. A `NaN` rung height sorts last under
        // `total_cmp` and leaves the partition intact, so it *can* become the
        // upper bracket — and then the span is `NaN`, `span > 0.0` is false,
        // and this arm degrades to weighting the lower rung fully. Reaching it
        // takes a `NaN` radial elevation, which fixed-point decoding cannot
        // produce, which is why no test pins it. It stays a branch rather than
        // an `unreachable!()` precisely because that path exists: a panic
        // would turn a benign degradation into a dead frame.
        let t = if span > 0.0 {
            ((height_km - lo.height_km) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // **The seam between two rungs is where a fold does the most damage,
        // not the least.** A two-corner lerp at `t = 0.5` of `+v` and `−v` is
        // identically zero, so *every* fold-straddling rung pair halfway up
        // fabricates, where the four-corner bilinear at least needs its other
        // two corners to agree. Measured over fourteen volumes: of 12,918 rung
        // pairs an independent continuity oracle confirms as folds, 12,903 —
        // 99.9% — average to less than a quarter of the sweep's Nyquist
        // velocity, which is the display's word for near-calm air. The
        // corresponding figure for four-corner quads is 28,814 of 28,981, so
        // the two are close here; what makes the vertical case worse is not a
        // higher rate but that `t` is so often near 0.5.
        //
        // (An earlier note here claimed 94.9% of straddling *rung pairs at
        // KLWX* fabricated, against 97% of straddling gate pairs. Neither
        // number reproduces under any population this module can name, and
        // both are withdrawn; the figures above replace them. The residual those
        // numbers left unexplained was not rounding: it was the straddles
        // whose smaller corner sits well inside the range, which is exactly
        // the false-positive population [`straddles_fold`] now refuses.)
        //
        // The smaller of the two limits governs. The pair is folded if
        // *either* end folded, each end folds at its own cut's Nyquist, and a
        // reading wrapped at the lower limit is the one whose seam is easier
        // to cross unnoticed — so testing against the larger limit would miss
        // exactly the straddles the mixed-PRF ladder creates.
        //
        // This pair spans *tilts*, and the guard's line sits at
        // `SEAM_PROXIMITY_ACROSS_TILTS` — higher than the bilinear's,
        // because across hundreds of metres of depth a real fold's ends
        // stray further from the seam and a real shear's ends reach nearer
        // it, so this path demands more before refusing to interpolate.
        // The constant's doc carries the corpus that set it, including what
        // the guard here still misses: even at the old `0.5` it never kept
        // every real fold, so its fraction is a measured break-even, not a
        // construction.
        let fold_limit = match (lo.fold_limit_ms, hi.fold_limit_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            // This one-sided arm can fire only for armed limits in
            // [8.0, ≈11.94): the pair must clear `SEAM_PROXIMITY_ACROSS_TILTS`
            // of the measured limit on both ends while the unarmed rung's
            // speeds stay under the 8.0 m/s floor, which caps the armed limit
            // below `8.0 / 0.67` — dead at a 12.5 m/s cut, alive at the
            // ~11 m/s cuts VCP 31 actually flies.
            (a, b) => a.or(b),
        };
        blend(
            self.blend,
            &[lo.sample, hi.sample],
            &[1.0 - t, t],
            fold_limit.map(Seam::AcrossTilts),
        )
    }
}

impl std::fmt::Debug for VolumeSampler<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Point queries against a borrowed volume, for one moment.
///
/// One resolved rung: the wrap-corrected cut key and the index — into the sweep
/// list handed to [`resolve_ladder`] — of the sweep this moment takes for it.
pub(crate) struct LadderChoice {
    pub(crate) key: f64,
    pub(crate) chosen: usize,
}

/// Steps 1–3 of the tilt ladder: key every sweep on its VCP cut, group by exact
/// key, choose one sweep per group for `slot`.
///
/// Factored out of [`VolumeSampler::build`] so that [`ladder_fingerprint`] — the
/// re-cut key a live pane compares frame to frame — runs the *same* choice the
/// sampler will make, rather than a restatement of it. This campaign has paid
/// twice for a second copy of a sampler rule drifting from the first; the
/// factoring is the fix that cannot drift.
///
/// Takes `&[&Sweep]` rather than `&Scan` because the sweep list is no longer
/// always a scan's own: the current merged volume ([`crate::current`]) composes
/// sweeps from two volumes, and this function must key them identically either
/// way. Group order is discovery order (the caller sorts); member order inside a
/// group is input order, which is what "newest" means below.
pub(crate) fn resolve_ladder(
    cuts: &[ElevationCut],
    sweeps: &[&Sweep],
    slot: MomentSlot,
) -> Result<Vec<LadderChoice>, SamplerError> {
    // Step 1 and 2: key every sweep on its cut, then group by exact key,
    // preserving input order inside each group so "newest" below means what it
    // says.
    let mut groups: Vec<(f64, Vec<usize>)> = Vec::new();
    for (sweep_index, sweep) in sweeps.iter().enumerate() {
        let elevation_number = sweep.elevation_number();
        let cut_index = match usize::from(elevation_number).checked_sub(1) {
            Some(i) if i < cuts.len() => i,
            _ => {
                return Err(SamplerError::ElevationNumberOutOfCutTable {
                    sweep_index,
                    elevation_number,
                    cut_count: cuts.len(),
                });
            }
        };
        let mut key = cuts[cut_index].elevation_angle_degrees();
        if !key.is_finite() {
            return Err(SamplerError::NonFiniteCutAngle {
                cut_index,
                angle: key,
            });
        }
        // The cut table stores a signed angle in a field this decoder
        // hands back unsigned, so a below-horizon cut arrives as ~359.7°.
        if key > 180.0 {
            key -= 360.0;
        }
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(sweep_index),
            None => groups.push((key, vec![sweep_index])),
        }
    }

    // Step 3: one sweep per group, per this moment.
    let doppler = matches!(slot, MomentSlot::Velocity | MomentSlot::SpectrumWidth);
    let mut choices: Vec<LadderChoice> = Vec::with_capacity(groups.len());
    for (key, members) in groups {
        let carries = |&i: &usize| -> bool {
            sweeps[i]
                .radials()
                .first()
                .is_some_and(|r| slot.read(r).is_some())
        };
        // Newest-first: the last cut of a SAILS repeat is the current one,
        // and the reference display shows it too.
        let chosen = if doppler {
            members.iter().rev().find(|i| carries(i))
        } else {
            // A split cut's Doppler half repeats a short-range copy of the
            // surveillance moments; reflectivity belongs to the
            // surveillance half, which reaches 460 km against the Doppler
            // half's 300. Load-bearing past ~300 km, and the same
            // preference `render::find_sweep` already applies.
            members
                .iter()
                .rev()
                .find(|&&i| {
                    carries(&i)
                        && sweeps[i]
                            .radials()
                            .first()
                            .is_some_and(|r| r.velocity().is_none())
                })
                .or_else(|| members.iter().rev().find(|i| carries(i)))
        };
        let Some(&chosen) = chosen else { continue };
        choices.push(LadderChoice { key, chosen });
    }
    Ok(choices)
}

/// The identity of the sweeps the ladder would choose for `product` — the
/// re-cut key for anything that draws from a whole volume.
///
/// Two volumes fingerprint equal exactly when, for this moment, every rung
/// would be cut from the same measured data under the same declared pattern —
/// in which case the picture is byte-identical and a re-cut is pure waste.
/// The previous key was a count of sweeps *carrying* the moment, and it moved
/// on seals that change nothing: a split cut's Doppler half carries a
/// short-range reflectivity copy, so its seal incremented the reflectivity
/// count while the surveillance preference kept the chosen rung exactly where
/// it was — measured at ~6 of the 18–23 re-cuts per VCP-212 volume.
///
/// What is hashed, and why each part:
/// * the **declared cut table** — the pattern's angles set the rung keys and
///   the ladder's declared ceiling, which the section caption draws;
/// * per chosen sweep: the **rung key**, the sweep's **elevation number**, its
///   **radial count**, its first and last radials' **collection timestamps**,
///   and the first radial's **gate count** for this moment. A sealed sweep is
///   immutable, so this tuple names one sweep's data uniquely: two sweeps of
///   the same cut collected at different times differ in their timestamps,
///   and the same sweep re-delivered through a new snapshot hashes the same.
///
/// The hash is [`std::hash::DefaultHasher`]: stable within a process, which is
/// the only place the key is ever compared. It must never be persisted.
///
/// `None` when no ladder can be built at all — the moment is not samplable,
/// the pattern declares no cuts, a sweep cannot be keyed, or no rung carries
/// the moment. The caller treats `None` as its own value of the key: "nothing
/// to cut" is a state a pane can be aimed at.
pub fn ladder_fingerprint(
    pattern: &nexrad_model::data::VolumeCoveragePattern,
    sweeps: &[&Sweep],
    product: RadarProduct,
) -> Option<u64> {
    use std::hash::{Hash, Hasher};

    // `volume_slot`, not `samplable`: a derived product's ladder is its
    // source moment's ladder (SRV and NROT climb the velocity cuts, KDP the
    // ΦDP cuts), and the re-cut key has to see the same ladder the worker's
    // derived sampler resolves or a section would never re-cut — or always
    // re-cut — on a derived product.
    let slot = crate::derive::volume_slot(product)?;
    let cuts = pattern.elevation_cuts();
    if cuts.is_empty() {
        return None;
    }
    let mut choices = resolve_ladder(cuts, sweeps, slot).ok()?;
    if choices.is_empty() {
        return None;
    }
    // The ladder, not the discovery: `resolve_ladder` returns rungs in the
    // order their first member appeared, and that order shifts when a
    // superseded base sweep leaves the merged list even though every rung's
    // *choice* stands. The sampler sorts its rungs by key; the fingerprint
    // hashes the same sorted ladder, or an unchanged picture would re-cut.
    choices.sort_by(|a, b| a.key.total_cmp(&b.key));

    let mut hasher = std::hash::DefaultHasher::new();
    cuts.len().hash(&mut hasher);
    for cut in cuts {
        cut.elevation_angle_degrees().to_bits().hash(&mut hasher);
    }
    for LadderChoice { key, chosen } in choices {
        let sweep = sweeps[chosen];
        let radials = sweep.radials();
        key.to_bits().hash(&mut hasher);
        sweep.elevation_number().hash(&mut hasher);
        radials.len().hash(&mut hasher);
        if let Some(first) = radials.first() {
            first.collection_timestamp().hash(&mut hasher);
            slot.read(first)
                .map(|moment| moment.gate_count())
                .hash(&mut hasher);
        }
        if let Some(last) = radials.last() {
            last.collection_timestamp().hash(&mut hasher);
        }
    }
    Some(hasher.finish())
}

/// Construction resolves the tilt ladder (see the module doc) and indexes each
/// rung's radials by azimuth; it decodes no gates. Gates are decoded on demand
/// out of `raw_values()`.
pub struct VolumeSampler<'a> {
    product: RadarProduct,
    slot: MomentSlot,
    blend: Blend,
    rungs: Vec<Rung<'a>>,
    /// The highest cut angle the coverage pattern *declares*, wrap-corrected —
    /// which is not the highest rung the ladder *has*. See
    /// [`top_declared_cut_deg`](Self::top_declared_cut_deg).
    top_declared_cut_deg: f64,
}

impl<'a> VolumeSampler<'a> {
    /// Resolve `volume`'s tilt ladder for `product`.
    ///
    /// Fails rather than degrades — see the module doc's section on the VCP.
    /// Every error is also logged, so a caller that discards the `Result` with
    /// `.ok()` still leaves the reason somewhere.
    ///
    /// `volume` is a [`crate::nyquist::Volume`], which a bare `&Scan` converts
    /// into: the conversion declares no Nyquist velocities, so a velocity
    /// ladder built from one estimates every rung's fold limit off the data.
    /// A caller that holds the archive's own declarations — the whole
    /// production path does — passes [`crate::nyquist::Volume::new`] and gets
    /// the declared numbers instead. See [`Rung::fold_limit_ms`].
    pub fn new(volume: impl Into<Volume<'a>>, product: RadarProduct) -> Result<Self, SamplerError> {
        Self::build(volume.into(), product).inspect_err(|e| {
            log::warn!("volume sampler unavailable for {}: {e}", product.code());
        })
    }

    /// Resolve a **derived** scan's ladder for `product`, reading `slot`.
    ///
    /// The bypass [`crate::derive`] needs and nothing else may use:
    /// [`samplable`] refuses the derived products precisely so a raw volume
    /// can never be sampled under a derived label — this constructor exists
    /// for a scan whose `slot` moment [`crate::derive::prepare`] has already
    /// rewritten with the derived field. `pub(crate)` because the derivation
    /// layer is the only legitimate caller; going through [`Self::new`] with
    /// a derived product is a refusal by design, not a missing feature.
    pub(crate) fn for_derived(
        volume: impl Into<Volume<'a>>,
        product: RadarProduct,
        slot: MomentSlot,
    ) -> Result<Self, SamplerError> {
        Self::build_for_slot(volume.into(), product, slot).inspect_err(|e| {
            log::warn!(
                "volume sampler unavailable for derived {}: {e}",
                product.code()
            );
        })
    }

    fn build(volume: Volume<'a>, product: RadarProduct) -> Result<Self, SamplerError> {
        let Some(slot) = samplable(product) else {
            return Err(SamplerError::NotSamplable {
                product,
                reason: refusal_reason(product),
            });
        };
        Self::build_for_slot(volume, product, slot)
    }

    fn build_for_slot(
        volume: Volume<'a>,
        product: RadarProduct,
        slot: MomentSlot,
    ) -> Result<Self, SamplerError> {
        let scan = volume.scan();
        let declared = volume.declared_nyquist();
        let cuts = scan.coverage_pattern().elevation_cuts();
        if cuts.is_empty() {
            return Err(SamplerError::EmptyCoveragePattern {
                vcp: scan.coverage_pattern().pattern_number().number(),
            });
        }

        // The ceiling the *pattern* declares, before a word about what flew.
        // Read off the same table the rungs are keyed through and corrected the
        // same way, so a comparison against a rung's own key is exact rather
        // than a tolerance. Non-finite entries are skipped rather than refused:
        // this is a summary of cuts that may never be referenced by a sweep,
        // and a garbage angle in one of those is not a reason to refuse a
        // volume the ladder can be built from perfectly well.
        let top_declared_cut_deg = cuts
            .iter()
            .map(|cut| {
                let angle = cut.elevation_angle_degrees();
                if angle > 180.0 { angle - 360.0 } else { angle }
            })
            .filter(|angle| angle.is_finite())
            .fold(f64::NEG_INFINITY, f64::max);

        // Steps 1–3, shared with [`ladder_fingerprint`] so the re-cut key can
        // never disagree with the ladder about which sweep a rung took.
        let sweeps: Vec<&Sweep> = scan.sweeps().iter().collect();
        let choices = resolve_ladder(cuts, &sweeps, slot)?;

        let mut rungs: Vec<Rung<'a>> = Vec::with_capacity(choices.len());
        for LadderChoice { key, chosen } in choices {
            let sweep = &scan.sweeps()[chosen];
            let radials = sweep.radials();
            // Step 4: the geometry is the chosen sweep's median, never the key.
            let Some(elevation_deg) = sweep_elevation_deg(radials) else {
                continue;
            };
            let (by_azimuth, az_step_deg) = index_azimuths(radials);
            // The declared number first, the reading of the data second.
            //
            // Declared wins outright rather than being reconciled: it is the
            // RDA's statement of the waveform it flew, while the estimate is
            // exact only for a sweep that actually folded and an
            // **under**estimate for one that did not — and this sampler uses
            // the number as a classification boundary, where an underestimate
            // widens the fold hypothesis and manufactures false positives.
            //
            // `FOLD_LIMIT_FLOOR_MS` applies to whichever answer is used. It
            // was written for the estimator's failure mode — a calm sweep
            // giving a limit that ordinary noise clears — which a declaration
            // cannot have; but no operational NEXRAD waveform has a Nyquist
            // velocity that low either, so a declared value under the floor is
            // a corrupt field rather than a slow one. It is dropped, and the
            // rung falls through to the estimator, which applies the same
            // floor and may well switch the guard off entirely. A declaration
            // this module cannot believe must not be *more* trusted than a
            // measurement it cannot believe.
            //
            // `folds_at_measured_limit` gates the estimator's pass over the
            // sweep so no moment without a seam pays for it; the lookup is
            // free either way, and is still behind the gate so a reflectivity
            // rung cannot pick up a velocity cut's limit.
            let (fold_limit_ms, fold_limit_declared) = if Blend::folds_at_measured_limit(product) {
                match declared
                    .get(sweep.elevation_number())
                    .filter(|ms| *ms >= FOLD_LIMIT_FLOOR_MS)
                {
                    Some(ms) => (Some(ms), true),
                    None => (estimate_fold_limit(radials, slot), false),
                }
            } else {
                (None, false)
            };
            rungs.push(Rung {
                nominal_deg: key,
                elevation_deg,
                radials,
                by_azimuth,
                az_step_deg,
                fold_limit_ms,
                fold_limit_declared,
            });
        }

        if rungs.is_empty() {
            return Err(SamplerError::NoSweepsWithMoment { product });
        }
        rungs.sort_by(|a, b| a.nominal_deg.total_cmp(&b.nominal_deg));

        // Every rung came from a cut whose angle was checked finite above, so a
        // ladder with rungs always has a finite top. The fold's seed only
        // survives a table of nothing but non-finite angles, and then the
        // ladder's own top is the honest answer: it says "as far as anything
        // here knows, the volume delivered its whole pattern", which
        // under-warns rather than crying wolf about a table nobody can read.
        let top_declared_cut_deg = if top_declared_cut_deg.is_finite() {
            top_declared_cut_deg
        } else {
            rungs.last().map_or(0.0, |rung: &Rung<'a>| rung.nominal_deg)
        };

        Ok(Self {
            product,
            slot,
            blend: Blend::for_moment(product),
            rungs,
            top_declared_cut_deg,
        })
    }

    /// The moment this sampler was built for.
    pub fn product(&self) -> RadarProduct {
        self.product
    }

    /// The ladder, as one line: per rung, `nominal->median radials×gates`.
    ///
    /// Hand-written rather than derived because a derived `Debug` would walk
    /// the borrowed radials and print the whole ~10 M-gate volume — which is
    /// what `assert_eq!` and `unwrap` reach for on failure, so the derive
    /// would turn a one-line test failure into an unreadable one.
    ///
    /// # The radial and gate counts are the load-bearing part
    ///
    /// They say **which sweep won each rung**, and nothing else here does. An
    /// earlier version printed only the angles, and that made this line
    /// structurally incapable of seeing the failure it is most often reached
    /// for: on a real split cut the two halves share a cut angle *and* a
    /// median — 0.4834° for both on a measured KMPX VCP 212 volume — so a
    /// ladder that took the Doppler half where it should have taken the
    /// surveillance half printed byte-identically to a correct one. What
    /// separates them is range: 1832 reflectivity gates on the surveillance
    /// half against 1192 on the Doppler half, which is 460 km against 300.
    ///
    /// So a comparison over this string is a comparison of the ladder, not of
    /// its labels. `a_reconstructed_render_input_scan_builds_the_identical_ladder`
    /// and the live harness both rest on that.
    ///
    /// # The fold limit and its provenance are printed for the same reason
    ///
    /// A rung that guards velocity appends `±<limit>d` or `±<limit>e` — `d`
    /// for the archive's declaration, `e` for [`estimate_fold_limit`]'s
    /// reading of the data. Both halves are load-bearing, and neither is
    /// cosmetic:
    ///
    /// * the **limit** because two ladders that chose the same sweeps can
    ///   still blend differently, and a fold limit is the only per-rung input
    ///   to that decision the rest of this line does not name;
    /// * the **letter** because a declared limit and an estimated one are
    ///   usually within a few m/s of each other — the estimate *is* the
    ///   Nyquist velocity whenever the sweep folded — so a worker that lost
    ///   the declared table would print a nearly identical number and a string
    ///   comparison would pass. The provenance is what makes the divergence
    ///   visible before the numbers happen to differ.
    ///
    /// Nothing is appended for a rung with no limit, so every moment without a
    /// fold seam prints exactly what it printed before.
    fn describe(&self) -> String {
        let rungs: Vec<String> = self
            .rungs
            .iter()
            .map(|r| {
                let fold = match r.fold_limit_ms {
                    Some(ms) => {
                        format!(" ±{ms:.2}{}", if r.fold_limit_declared { 'd' } else { 'e' },)
                    }
                    None => String::new(),
                };
                format!(
                    "{:.4}->{:.4} {}x{}{fold}",
                    r.nominal_deg,
                    r.elevation_deg,
                    r.radials.len(),
                    self.slot
                        .read(&r.radials[0])
                        .map_or(0, |m| m.raw_values().len()),
                )
            })
            .collect();
        format!(
            "{} on {} rungs [{}]",
            self.product.code(),
            self.rungs.len(),
            rungs.join(", "),
        )
    }

    /// How many rungs the ladder has for this moment.
    ///
    /// A section drawn on a short ladder interpolates across whatever gap the
    /// ladder leaves and draws a smooth layer that is not there, so a caller
    /// that means to warn about it needs this and
    /// [`widest_tilt_gap_deg`](Self::widest_tilt_gap_deg).
    pub fn tilt_count(&self) -> usize {
        self.rungs.len()
    }

    /// Each rung's geometric elevation, **in cut order** — which is ascending
    /// by the nominal key, not by this number.
    ///
    /// The distinction is not pedantry: the ladder is ordered by the VCP's cut
    /// angles, and a chosen sweep's median can in principle sit outside its
    /// cut's place in that order. Measured never to, in 4 756 ordered pairs,
    /// but `a_ladder_whose_medians_invert_still_brackets_by_height` builds one
    /// that does and this iterator reports `[1.05, 0.55]` for it. A caller who
    /// wants heights sorted wants [`Column::rungs`], which is.
    pub fn elevations_deg(&self) -> impl Iterator<Item = f64> + '_ {
        self.rungs.iter().map(|r| r.elevation_deg)
    }

    /// Each rung's VCP cut angle, ascending — the grouping key, not geometry.
    /// Exposed so a caller can show which declared cuts a volume actually
    /// delivered.
    pub fn nominal_elevations_deg(&self) -> impl Iterator<Item = f64> + '_ {
        self.rungs.iter().map(|r| r.nominal_deg)
    }

    /// The highest cut angle **this ladder has**, degrees — the top rung's
    /// grouping key, or `0.0` for a ladder with no rungs (which
    /// [`new`](Self::new) refuses to build, so only a caller holding one by
    /// other means can see it).
    ///
    /// The key rather than the median, so it can be compared against
    /// [`top_declared_cut_deg`](Self::top_declared_cut_deg) exactly. The two
    /// come off the same cut table.
    pub fn top_tilt_deg(&self) -> f64 {
        self.rungs.last().map_or(0.0, |rung| rung.nominal_deg)
    }

    /// The highest cut angle the coverage pattern **declares**, degrees.
    ///
    /// # Why this travels with a section
    ///
    /// Read against [`top_tilt_deg`](Self::top_tilt_deg) it answers the one
    /// question a consumer of a short ladder cannot otherwise ask: *did the
    /// volume stop early, or is this all there is?* They are different pictures
    /// with the same pixels. A complete VCP 35 delivering five cuts to 4.5° has
    /// a ceiling because that is the pattern; a VCP 212 four rungs into its
    /// flight has a ceiling because the antenna has not got there yet, and
    /// everything above 1.8° in that picture is unscanned air rather than the
    /// cone of silence. Naming the second as the first hands the user a
    /// confident meteorological explanation for a blank region and it is the
    /// wrong one.
    ///
    /// The count is deliberately *not* the comparison. A pattern declares more
    /// cut-table entries than it has distinct angles — a split cut is two
    /// entries at one angle — and the surveillance-only entries at the bottom
    /// of a precipitation VCP carry no Doppler moment at all, so counting would
    /// report a complete volume's velocity ladder as short for ever. Every
    /// operational pattern's *highest* cut carries every moment, so the top is
    /// the comparison that holds across moments.
    pub fn top_declared_cut_deg(&self) -> f64 {
        self.top_declared_cut_deg
    }

    /// The largest angular step between adjacent rungs, degrees. `0.0` for a
    /// single-rung ladder.
    ///
    /// Measured over the elevations **sorted**, not over the ladder's cut
    /// order. Folding signed differences down the cut order instead would
    /// report `0.0` for a ladder whose medians invert — every difference
    /// negative, `f64::max` from `0.0` keeping the seed — so the one number
    /// that exists to warn "this section is interpolating across a gap" would
    /// read *no gap at all* in one of the few cases it is there for.
    pub fn widest_tilt_gap_deg(&self) -> f64 {
        let mut sorted: Vec<f64> = self.elevations_deg().collect();
        sorted.sort_by(f64::total_cmp);
        sorted
            .windows(2)
            .map(|w| w[1] - w[0])
            .fold(0.0f64, f64::max)
    }

    /// The tilt ladder over one ground point, allocating a fresh [`Column`].
    ///
    /// `azimuth_deg` is clockwise from true north; `ground_range_km` is a
    /// **ground** range, so a caller holding a slant range wants
    /// [`beam::ground_range_km`] first.
    pub fn column(&self, azimuth_deg: f64, ground_range_km: f64) -> Column {
        let mut out = Column::new();
        self.column_into(azimuth_deg, ground_range_km, &mut out);
        out
    }

    /// [`column`](Self::column) into a caller-owned buffer, so a raster
    /// sweeping thousands of columns allocates once.
    pub fn column_into(&self, azimuth_deg: f64, ground_range_km: f64, out: &mut Column) {
        out.rungs.clear();
        out.azimuth_deg = azimuth_deg;
        out.ground_range_km = ground_range_km;
        out.blend = self.blend;
        if !azimuth_deg.is_finite() || !ground_range_km.is_finite() || ground_range_km < 0.0 {
            return;
        }
        let azimuth = azimuth_deg.rem_euclid(360.0);
        for rung in &self.rungs {
            out.rungs.push(ColumnRung {
                height_km: beam::height_at_ground_km(ground_range_km, rung.elevation_deg),
                elevation_deg: rung.elevation_deg,
                sample: self.sample_rung(rung, azimuth, ground_range_km),
                fold_limit_ms: rung.fold_limit_ms,
            });
        }
        // Ascending by height. The rungs are already ascending by cut angle,
        // and `height_at_ground_km` is strictly increasing in elevation, so
        // this reorders nothing unless a chosen sweep's median inverted its
        // cut's order — measured never to happen in 4 756 ordered pairs, which
        // is a reason to sort defensively rather than to assume.
        out.rungs
            .sort_by(|a, b| a.height_km.total_cmp(&b.height_km));
    }

    /// What the volume holds at one point, in radar-relative coordinates.
    ///
    /// For a hover readout and anything else that asks once. It builds the
    /// whole column and asks it, so it costs the whole ladder — `4·N` gate
    /// reads — rather than the eight a bracketing pair would need. That is a
    /// deliberate trade of a cost nobody pays (a hover query happens once a
    /// frame) for **one** interpolation path: sampling only the bracketing
    /// pair means finding the bracket a second way, and two ways of choosing a
    /// bracket is precisely the split-key hazard this module's ladder rule
    /// exists to close. `the_point_query_is_exactly_the_column_query` pins the
    /// equivalence, and would keep pinning it if this were ever specialised.
    ///
    /// Anything asking for more than one height of the same column wants
    /// [`column`](Self::column), which is `H` times cheaper over `H` heights.
    pub fn sample(&self, azimuth_deg: f64, ground_range_km: f64, height_km: f64) -> Sample {
        self.column(azimuth_deg, ground_range_km)
            .at_height_km(height_km)
    }

    /// Bilinear in azimuth × slant range within one rung.
    fn sample_rung(&self, rung: &Rung<'a>, azimuth: f64, ground_range_km: f64) -> Sample {
        let Some((lo, hi, fa)) = azimuth_bracket(rung, azimuth) else {
            return Sample::missing(SampleStatus::NoCoverage);
        };
        // The `cos e` the plan view omits. See the module doc.
        let slant_km = beam::slant_range_for_ground_km(ground_range_km, rung.elevation_deg);

        let mut corners = [Sample::missing(SampleStatus::NoCoverage); 4];
        let mut weights = [0.0f64; 4];
        for (side, (radial_index, wa)) in [(lo, 1.0 - fa), (hi, fa)].into_iter().enumerate() {
            let radial = &rung.radials[radial_index];
            let (near, far, fr) = match self.slot.read(radial) {
                Some(moment) => gate_bracket(moment, slant_km),
                None => (
                    Sample::missing(SampleStatus::NoCoverage),
                    Sample::missing(SampleStatus::NoCoverage),
                    0.0,
                ),
            };
            corners[side * 2] = near;
            corners[side * 2 + 1] = far;
            weights[side * 2] = wa * (1.0 - fr);
            weights[side * 2 + 1] = wa * fr;
        }
        // These corners span *gates* (and radials) of one sweep, so the
        // guard's line sits at `SEAM_PROXIMITY_ACROSS_GATES` — see the
        // constant for the corpus that set it apart from the tilt path's.
        blend(
            self.blend,
            &corners,
            &weights,
            rung.fold_limit_ms.map(Seam::AcrossGates),
        )
    }
}

/// `(azimuth, radial index)` ascending by azimuth, and the sweep's median
/// azimuth step.
fn index_azimuths(radials: &[Radial]) -> (Vec<(f32, u32)>, f64) {
    let mut by_azimuth: Vec<(f32, u32)> = radials
        .iter()
        .enumerate()
        .map(|(i, r)| {
            (
                (r.azimuth_angle_degrees() as f64).rem_euclid(360.0) as f32,
                i as u32,
            )
        })
        .collect();
    by_azimuth.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Circular gaps, so the seam between the last and first azimuth counts as
    // one step in a complete sweep rather than as the sweep's one big hole.
    let mut gaps: Vec<f64> = Vec::with_capacity(by_azimuth.len());
    for i in 0..by_azimuth.len() {
        let a = f64::from(by_azimuth[i].0);
        let b = f64::from(by_azimuth[(i + 1) % by_azimuth.len()].0);
        let gap = (b - a).rem_euclid(360.0);
        if gap > 0.0 {
            gaps.push(gap);
        }
    }
    gaps.sort_by(f64::total_cmp);
    // A sweep with no two distinct azimuths has no observable step. One degree
    // is the coarsest spacing a WSR-88D produces, so it is the least
    // presumptuous stand-in: it makes the footprint rule serve half a degree
    // either side and no more.
    let az_step_deg = gaps.get(gaps.len() / 2).copied().unwrap_or(1.0);
    (by_azimuth, az_step_deg)
}

/// The two radials bracketing `azimuth` and the fraction between them, or
/// `None` where no radial's footprint covers it.
///
/// Returns indices into `rung.radials`, not into `rung.by_azimuth`.
fn azimuth_bracket(rung: &Rung<'_>, azimuth: f64) -> Option<(usize, usize, f64)> {
    let n = rung.by_azimuth.len();
    if n == 0 {
        return None;
    }
    // The number of azimuths at or below the query; 0 and `n` are both the
    // wrap case, where the bracket is the last azimuth and the first.
    let above = rung
        .by_azimuth
        .partition_point(|&(a, _)| f64::from(a) <= azimuth);
    let (lo_slot, hi_slot) = if above == 0 || above == n {
        (n - 1, 0)
    } else {
        (above - 1, above)
    };
    let (az_lo, i_lo) = rung.by_azimuth[lo_slot];
    let (az_hi, i_hi) = rung.by_azimuth[hi_slot];
    let (az_lo, az_hi) = (f64::from(az_lo), f64::from(az_hi));
    let (i_lo, i_hi) = (i_lo as usize, i_hi as usize);

    let half_footprint = rung.az_step_deg / 2.0;
    let gap = (az_hi - az_lo).rem_euclid(360.0);
    if gap <= 0.0 {
        // One radial, or a duplicated azimuth. Either way there is nothing to
        // interpolate with, so it serves its own footprint alone.
        let d = (azimuth - az_lo).rem_euclid(360.0);
        return (d.min(360.0 - d) <= half_footprint).then_some((i_lo, i_hi, 0.0));
    }
    let f = (azimuth - az_lo).rem_euclid(360.0) / gap;
    if gap <= MAX_ADJACENT_GAP_STEPS * rung.az_step_deg {
        return Some((i_lo, i_hi, f.clamp(0.0, 1.0)));
    }
    // The pair straddles a hole. Serve only from inside a surviving radial's
    // own footprint; between them the sweep measured nothing and says so.
    if (azimuth - az_lo).rem_euclid(360.0) <= half_footprint {
        Some((i_lo, i_hi, 0.0))
    } else if (az_hi - azimuth).rem_euclid(360.0) <= half_footprint {
        Some((i_lo, i_hi, 1.0))
    } else {
        None
    }
}

/// The two gates bracketing `slant_km` on one radial, and the fraction between
/// their centres.
///
/// `first_gate_range_km` is the **centre** of gate 0, so interpolating between
/// centres is `(slant − first) / interval` with no half-gate subtracted — the
/// half gate matters to "which gate contains this range", which is a different
/// question this function is not asking.
fn gate_bracket(moment: &MomentData, slant_km: f64) -> (Sample, Sample, f64) {
    // A zero gate interval is not guarded separately: `gate_interval_km` is a
    // `u16` of metres so it cannot be negative, and dividing by zero lands on
    // an infinity or a `NaN` that the finiteness test already refuses.
    // A second guard would be an unreachable branch, and an unreachable branch
    // is one nothing can pin.
    let x = (slant_km - moment.first_gate_range_km()) / moment.gate_interval_km();
    if !x.is_finite() || x < 0.0 {
        // Inside the first gate's centre: the radar has no gate there at all.
        // `BeyondRange` would be the wrong word — nothing has been exceeded.
        let s = Sample::missing(SampleStatus::NoCoverage);
        return (s, s, 0.0);
    }
    let near_index = x.floor();
    let frac = x - near_index;
    // `usize::MAX` for anything past the addressable range; `gate_sample`
    // answers `BeyondRange` for it, which is the same answer.
    let near_index = if near_index <= usize::MAX as f64 {
        near_index as usize
    } else {
        usize::MAX
    };
    (
        gate_sample(moment, near_index),
        gate_sample(moment, near_index.saturating_add(1)),
        frac,
    )
}

/// Decode one gate, by index, without allocating and without walking the
/// radial.
///
/// **The reason this duplicates `MomentData::iter`'s six lines is O(1) random
/// access, not allocation.** `iter()` is already allocation-free, so a doc
/// blaming allocation would invite someone to "fix" this back to
/// `iter().nth(j)` — quadratic per radial — with every test still green. One
/// bilinear sample touches four radials at arbitrary gate indices, which an
/// iterator cannot serve at any price.
/// `raw_gate_decoding_matches_the_model_element_for_element` is the guard on
/// the duplication, and it includes a `scale == 0.0` moment because that case
/// disables the 0/1 status codes entirely and is the one a reimplementation
/// gets wrong.
///
/// `raw_values().len()` is authoritative for how many gates there are, not
/// `gate_count()`: the model's own `raw_gate_values` iterates
/// `chunks_exact(word)` over the bytes, so a moment whose declared count
/// overruns its bytes has the gates its bytes have.
fn gate_sample(moment: &MomentData, gate: usize) -> Sample {
    let bytes = moment.raw_values();
    // Anything other than 16 is one byte per gate, which is how the model's
    // own `raw_gate_values` reads it.
    let raw = if moment.data_word_size() == 16 {
        let Some(pair) = gate.checked_mul(2).and_then(|k| bytes.get(k..k + 2)) else {
            return Sample::missing(SampleStatus::BeyondRange);
        };
        u16::from_be_bytes([pair[0], pair[1]])
    } else {
        let Some(&b) = bytes.get(gate) else {
            return Sample::missing(SampleStatus::BeyondRange);
        };
        u16::from(b)
    };

    let scale = moment.scale();
    // An exact comparison, as in the model: the value comes from a binary
    // format where IEEE 754 zero is stored literally. A zero scale means the
    // raw words *are* the values, so 0 and 1 are ordinary numbers rather than
    // status codes.
    if scale == 0.0 {
        return Sample::found(raw as f32);
    }
    match raw {
        0 => Sample::missing(SampleStatus::BelowThreshold),
        1 => Sample::missing(SampleStatus::RangeFolded),
        _ => Sample::found((raw as f32 - moment.offset()) / scale),
    }
}

/// The speed one sweep folds at, m/s, read off the sweep itself.
///
/// # Why this is the Nyquist velocity, and why it is sound when it is not
///
/// A folded field always contains gates *at* the fold limit — that is what
/// folding does to the values it wraps — so when a sweep aliased at all, the
/// largest speed it reports **is** its Nyquist velocity.
///
/// **That is measured, not assumed.** Scored against the archive's own
/// `nyquist_velocity` (Message 31's Radial Data Block, which `nexrad-decode`
/// exposes and this crate's render path cannot reach — see
/// [`Blend::folds_at_measured_limit`]) over 140 rungs of fourteen volumes at
/// six sites, the ratio of this estimate to the declared number runs
/// 0.889–1.016, with 133 of the 140 inside 0.96–1.016 and a median of 0.992.
/// The predicted failure — a weak-flow sweep that never reaches Nyquist and
/// so reports a limit far below it — did not appear: every operational sweep
/// examined folded somewhere. Note the ratios above 1: the reported extreme
/// can exceed the declared Nyquist by one encoding step, and an over-estimate
/// makes the guard *less* eager, which is the harmless direction.
///
/// When a sweep did *not* alias, this is an underestimate — the true Nyquist
/// is higher than anything the sweep saw. [`FOLD_LIMIT_FLOOR_MS`] closes the
/// case where that stops meaning anything at all; between the floor and the
/// truth, an underestimate makes [`straddles_fold`] fire more readily than it
/// should, which is why the size of the error above is worth having measured.
///
/// `crate::nrot`'s `estimate_nyquist` is the same measurement on the same
/// reasoning, and this module could not call it: that one takes an
/// already-gridded, already-median-filtered `Vec<Vec<f64>>` built by the NROT
/// pipeline, which the sampler neither has nor wants to build. **The two uses
/// have opposite sensitivity to the same error, and the shared reasoning
/// should not be read as a shared safety argument.** `nrot` scales a threshold
/// by its estimate, so a low estimate lowers the threshold and is
/// conservative. This sampler uses it as an exact classification boundary, so
/// a low estimate widens the fold hypothesis and manufactures false
/// positives — the same number, the opposite direction of harm.
///
/// # The arithmetic deliberately mirrors [`gate_sample`]
///
/// Only the extreme raw words are converted, not every gate, because the
/// encoding is affine: `(raw − offset) / scale` is monotone in `raw`, so the
/// largest `|value|` is reached at the smallest or the largest raw word and
/// nowhere between. Both ends are converted because a negative `scale` swaps
/// which is which. The `raw >= 2` filter is [`gate_sample`]'s status codes,
/// and the `scale == 0.0` skip is its "the raw words *are* the values" arm:
/// those values are unsigned, so no two can straddle a seam and no limit is
/// wanted from them.
///
/// `None` when nothing here reached [`FOLD_LIMIT_FLOOR_MS`], which switches
/// the guard off for this sweep entirely.
fn estimate_fold_limit(radials: &[Radial], slot: MomentSlot) -> Option<f64> {
    let mut limit = 0.0f64;
    for radial in radials {
        let Some(moment) = slot.read(radial) else {
            continue;
        };
        let scale = moment.scale();
        if scale == 0.0 {
            continue;
        }
        let bytes = moment.raw_values();
        let (mut lo, mut hi) = (u16::MAX, 0u16);
        let mut fold = |raw: u16| {
            if raw >= 2 {
                lo = lo.min(raw);
                hi = hi.max(raw);
            }
        };
        if moment.data_word_size() == 16 {
            for pair in bytes.chunks_exact(2) {
                fold(u16::from_be_bytes([pair[0], pair[1]]));
            }
        } else {
            for &b in bytes {
                fold(u16::from(b));
            }
        }
        if lo > hi {
            // Every gate was a status code: this radial measured no speed.
            continue;
        }
        for raw in [lo, hi] {
            let value = f64::from((raw as f32 - moment.offset()) / scale);
            if value.is_finite() {
                limit = limit.max(value.abs());
            }
        }
    }
    (limit >= FOLD_LIMIT_FLOOR_MS).then_some(limit)
}

/// How near the seam both extremes must sit before a straddle between
/// adjacent *gates* — the corners of one rung's bilinear — is read as a fold,
/// as a fraction of the fold limit.
///
/// **This is a fraction, and saying so is the point.** An earlier rule tested
/// only that the extremes changed sign and spread by more than `limit`, and
/// argued that `1.0·limit` needed no shading because it is the break-even
/// point of the two explanations — below it the pair is closer together across
/// the middle, above it through the seam. That argument is sound about
/// *likelihood* and wrong about *posterior*: break-even likelihood is
/// break-even belief only if a fold and a non-fold were equally likely before
/// either was measured, and they are not. On every population measured the
/// sign-changing pairs near a spread ratio of 1.0 outnumber the folded ones
/// by one to two orders of magnitude, so a rule that splits the likelihood
/// evenly hands almost all of the disputed band to the wrong answer.
///
/// # Where the number comes from
///
/// The criterion is marginal, not global. Each step up in the fraction stops
/// the guard firing on one band of pairs, and that band holds both
/// oracle-confirmed folds (now averaged across the seam — given up) and
/// oracle-confirmed shear (no longer refused — won). The fraction stops
/// earning at the step where the band's confirmed shear stops outnumbering
/// its confirmed folds — where that ratio crosses 1. Swept by `seam_probe`
/// over its arbitration corpus — 56 VCP 31 volumes over 22 sites and eight
/// dates, VCP 31 being the only operational pattern that puts the seam at
/// 11–12.5 m/s — the quad bands cross between 0.55 and 0.65, so `0.60`. The
/// KILN holdout, a site the arbitration never saw, measured once and
/// afterwards, reproduces the crossing to within one band; the seven-volume
/// storm control and the VCP 32/35 mid-Nyquist control are recorded with the
/// corpus in `seam_probe`'s module doc.
///
/// # What the shipped point costs
///
/// A break-even keeps a trade, and the kept side is recorded here rather
/// than left to a re-run. Measured on the KILN holdout — clear-air VCP 31,
/// once: at the shipped `0.60`/`0.67` the guard still passes **2,925
/// fabricating quads and 2,199 fabricating rung pairs**, oracle-confirmed
/// folds it now declines that average to near-calm. (On KILN the quad rows
/// at `0.65` and `0.67` are identical: legacy-resolution velocity is
/// quantised at 0.5 m/s, and at that site's ~12.5 m/s estimated limits no
/// half-m/s reading falls between the two bounds.) The seven-volume storm
/// control fabricated nothing at the shipped fractions on its original
/// measuring run — a claim scoped to that run, whose corpus hours were
/// never recorded, not a property of storm volumes in general.
const SEAM_PROXIMITY_ACROSS_GATES: f64 = 0.60;

/// How near the seam both rungs must sit before a straddle between adjacent
/// *tilts* — the pair [`Column::at_height_km`] lerps between — is read as a
/// fold, as a fraction of the fold limit.
///
/// # Why this is not [`SEAM_PROXIMITY_ACROSS_GATES`]
///
/// [`straddles_fold`]'s argument — one wrap of a smooth field leaves *both*
/// sides of the discontinuity near `±limit` — assumes the pair's own true
/// change is small next to the Nyquist interval. Between adjacent gates of
/// one sweep that holds. Between adjacent tilts it fails: the two rungs sit
/// hundreds of metres apart, and against a 12.3 m/s Nyquist the real veer
/// across that depth moves a reading well away from the seam before it
/// wraps, so a genuine fold across tilts often presents with one end deep
/// inside the range — the shape the rule reads as shear. That asymmetry is
/// in the data, not just the argument: at every adequately-powered site of
/// the corpus, quad recall exceeds rung-pair recall at the same fraction, by
/// 2.4–13.0 points at `0.50` and 14.4–51.6 at `0.75`, and the gap widens
/// monotonically with the fraction. So the vertical guard buys each step of
/// its fraction with more real folds than the horizontal guard pays for the
/// same step, and its break-even lands higher: the rung-pair marginal bands
/// cross between 0.65 and 0.70 on the arbitration corpus, `0.67`, and the
/// KILN holdout reproduces that crossing to within one band too. One
/// constant serving both paths would put both numbers wrong, and whoever
/// tuned it would be trading quad false fires against rung-pair recall
/// without either trade being visible.
///
/// # What the pre-corpus text here claimed, corrected
///
/// The first version of this guard shipped `0.5` for both paths, argued from
/// one clear-air volume and a fourteen-volume mixed corpus, and its doc made
/// two claims the VCP 31 corpus overturned. It said neither the old rule nor
/// this one ever fires on a pair the oracle can confirm is smooth shear:
/// pooled over the arbitration corpus at `0.5`, the guard fires on 4,774
/// confirmed-shear quads and 9,034 confirmed-shear rung pairs. And it read
/// as if the box rule kept every real fold by construction, which the
/// vertical path never did: at `0.5` the rung-pair guard already misses 6.6%
/// of oracle-confirmed folds (93.4% recall pooled), and at `0.75` it would
/// miss 30.8% (69.2%). The by-construction argument is a good approximation
/// exactly where [`SEAM_PROXIMITY_ACROSS_GATES`] applies; here it is only a
/// tendency, which is why this number is a measured break-even and not a
/// theorem.
///
/// # What the shipped point costs
///
/// The recall quoted above is at `0.50` and `0.75`; the shipped point has
/// its own number: on the KILN holdout — clear-air VCP 31, measured once —
/// **rung recall at `0.67` is 63.06%**, so the vertical guard keeps under
/// two-thirds of oracle-confirmed rung folds at its own line. The
/// fabrications that survive both shipped fractions are recorded with the
/// quad figures on [`SEAM_PROXIMITY_ACROSS_GATES`].
const SEAM_PROXIMITY_ACROSS_TILTS: f64 = 0.67;

/// Which adjacency a velocity blend spans, carrying the fold limit its guard
/// tests against.
///
/// This is how the two seam-proximity constants stay with their own paths:
/// the fraction cannot be passed at all. A call site says which adjacency its
/// corners span, and the number follows from that inside [`straddles_fold`] —
/// so putting [`SEAM_PROXIMITY_ACROSS_TILTS`] on the bilinear takes a call
/// site claiming in words that gate neighbours are tilt neighbours, where a
/// bare fraction parameter would have compiled with the two values swapped
/// and said nothing.
#[derive(Clone, Copy)]
enum Seam {
    /// The corners of one rung's bilinear — adjacent gates and adjacent
    /// radials of one sweep — guarded at [`SEAM_PROXIMITY_ACROSS_GATES`].
    /// Carries the rung's own fold limit, m/s.
    AcrossGates(f64),
    /// The two rungs of the vertical lerp — adjacent tilts of the ladder —
    /// guarded at [`SEAM_PROXIMITY_ACROSS_TILTS`]. Carries the tighter of
    /// the pair's fold limits, m/s.
    AcrossTilts(f64),
}

/// Whether these corners sit on opposite sides of the fold seam they span.
///
/// **What a fold does is wrap a continuous field across `±limit`, so both
/// sides of the discontinuity it leaves behind are *near* `±limit`.** Take a
/// field passing smoothly through the seam between two samples: the true
/// speeds are `limit − a` and `limit + b` for small `a`, `b`, and what gets
/// reported is `limit − a` and `−(limit − b)`. Both readings are within the
/// pair's own true change of the seam. So a straddle whose *smaller* extreme
/// sits well inside the range cannot be one fold of a smooth field — it is
/// real shear, and this refuses it.
///
/// That is the whole rule: `lo < −f·limit && hi > f·limit`, where `seam`
/// says what the corners are adjacent across and `f` follows from that —
/// [`SEAM_PROXIMITY_ACROSS_GATES`] or [`SEAM_PROXIMITY_ACROSS_TILTS`], which
/// is where the two numbers and the corpus that set them are argued. Two
/// things the rule used to say separately fall out of it and are not tested
/// again. Opposite signs: `lo` is below a negative bound and `hi` above a
/// positive one. More than half a period of spread: `hi − lo > 2f·limit`,
/// which at any `f ≥ ½` — and both shipped fractions are — clears a whole
/// period, so on either path this is *strictly stronger* than the
/// sign-change-and-spread rule it replaced and can only fire where that
/// fired.
///
/// Only the extreme pair is tested. That is exhaustive rather than a shortcut:
/// if any pair among the corners straddles, the widest pair's ends are at
/// least as far either side of zero, so the extremes straddle too.
///
/// # What the fractions buy, and what they do not
///
/// The recall, false-fire and per-band numbers live with the constants, and
/// the instrument that measured them is `seam_probe`. An earlier version of
/// this comment carried a was→now fire table over a fourteen-volume mixed
/// corpus and two claims measured at the first shipped fraction — `0.5` on
/// both paths: that neither the old rule nor this one ever fires on a pair
/// the oracle can confirm is smooth shear, and, riding the construction
/// argument above, that every real fold is kept. The VCP 31 corpus
/// overturned both — the correction, with numbers, is on
/// [`SEAM_PROXIMITY_ACROSS_TILTS`] — and the table went with them because
/// its "now" column described `0.5`, which no longer ships on either path.
///
/// What the fractions do not do survives every corpus and is recorded so
/// nobody re-derives it: they do not empty the disputed population. On a
/// low-Nyquist clear-air volume ordinary boundary-layer shear is comparable
/// to the seam itself, no test on two numbers can separate the two, and
/// raising either fraction only moves through that population — which is why
/// both stop at their measured break-even rather than pressing on towards
/// certainty.
///
/// # The shape of the spread statistic, stated where it holds
///
/// On volumes that fold hard the spread is sharply bimodal — ordinary
/// zero-crossings below `0.4·limit`, folds piled against `2·limit`, a valley
/// around `1.2–1.6·limit` (KCRP 2017-08-26, quad spreads: 1961 counts in the
/// lowest tenth, 27 in the `1.5` bin, 7913 in the `2.0` bin). That is *not*
/// general. On the clear-air volume the histogram is broad and flat with no
/// valley at all (155/182/244/206/209/170/185/169/176/173 across the first ten
/// bins), and on the quiet KCRP 2021-08-01 volume it decays monotonically with
/// no second mode. A threshold cannot be placed "in the valley" on a
/// distribution that has none, which is the other half of why the argument
/// here is about what folding *does* rather than about where the counts fall.
fn straddles_fold(corners: &[Sample], seam: Seam) -> bool {
    let (fraction, limit) = match seam {
        Seam::AcrossGates(limit) => (SEAM_PROXIMITY_ACROSS_GATES, limit),
        Seam::AcrossTilts(limit) => (SEAM_PROXIMITY_ACROSS_TILTS, limit),
    };
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for corner in corners {
        let value = f64::from(corner.value);
        lo = lo.min(value);
        hi = hi.max(value);
    }
    lo < -fraction * limit && hi > fraction * limit
}

/// Combine weighted corner samples.
///
/// **Interpolation needs every corner to have measured something.** If any one
/// of them did not, the answer is the corner carrying the most weight,
/// verbatim — value and status both. That is deliberate and it is the whole
/// treatment of edges:
///
/// * Inside solid echo all corners are values, so the result is a true
///   bilinear (and a true vertical lerp), which is what makes a section look
///   like a section rather than like a stack of tiles.
/// * At an echo edge one corner is below threshold, and blending a number
///   towards "below threshold" would require inventing a number for it. Taking
///   the heaviest corner instead puts the boundary at the half-weight point —
///   the same place a linear ramp would have crossed the middle — and
///   fabricates nothing.
/// * A range-folded gate stays range folded over its own half of the interval
///   instead of being averaged out of existence, which is the reporting
///   `MomentValue::RangeFolded` never got from this crate before.
///
/// `seam` extends that last idea to corners that all *did* measure
/// something. **A velocity pair straddling the Nyquist seam averages to a
/// number neither gate saw, and the number it averages to reads as calm air:
/// +24.50 and −24.50 m/s average to exactly 0.000, which is the display's word
/// for "no motion" written over the display's word for "as fast as this radar
/// can report".** Averaging cannot be rescued there — the seam is a
/// discontinuity, and no weighted mean of two points on opposite sides of one
/// lands near either. So a straddle falls through to the same heaviest-corner
/// answer an echo edge gets, for the same stated reason: it fabricates
/// nothing. See [`straddles_fold`] for how a straddle is recognised, and the
/// [`Seam`] variants for why the recognition draws its line differently
/// across gates than across tilts; `None` means this moment has no seam to
/// straddle and restores the previous behaviour exactly.
///
/// **Heaviest means the largest bilinear weight — the *nearest* sample — not
/// the largest magnitude.** Picking the fastest corner would bias every fold
/// edge outward and turn this from an interpolation into a peak-hold; picking
/// the nearest is ordinary nearest-neighbour resampling, which is what
/// interpolation degrades to when interpolation is not defined.
///
/// Ties go to the earliest corner, so the result does not depend on iteration
/// order.
fn blend(kind: Blend, corners: &[Sample], weights: &[f64], seam: Option<Seam>) -> Sample {
    debug_assert_eq!(
        corners.len(),
        weights.len(),
        "every corner needs exactly one weight",
    );
    if corners.iter().all(|c| c.status == SampleStatus::Value)
        && !seam.is_some_and(|seam| straddles_fold(corners, seam))
    {
        let total: f64 = weights.iter().sum();
        if total > 0.0 {
            let mean = match kind {
                Blend::LinearZ => {
                    let z: f64 = corners
                        .iter()
                        .zip(weights)
                        .map(|(c, w)| w * 10f64.powf(f64::from(c.value) / 10.0))
                        .sum();
                    10.0 * (z / total).log10()
                }
                Blend::Arithmetic => {
                    let s: f64 = corners
                        .iter()
                        .zip(weights)
                        .map(|(c, w)| w * f64::from(c.value))
                        .sum();
                    s / total
                }
                Blend::Angular360 => {
                    let (mut sin, mut cos) = (0.0f64, 0.0f64);
                    for (c, w) in corners.iter().zip(weights) {
                        let r = f64::from(c.value).to_radians();
                        sin += w * r.sin();
                        cos += w * r.cos();
                    }
                    sin.atan2(cos).to_degrees().rem_euclid(360.0)
                }
            };
            return Sample::found(mean as f32);
        }
    }
    let mut best = 0usize;
    for (i, &w) in weights.iter().enumerate() {
        if w > weights[best] {
            best = i;
        }
    }
    corners
        .get(best)
        .copied()
        .unwrap_or_else(|| Sample::missing(SampleStatus::NoCoverage))
}

#[cfg(test)]
mod tests;
