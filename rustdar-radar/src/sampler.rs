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
//! A point query is four gate reads (bilinear in azimuth × slant range) on
//! each of two bracketing tilts, then one vertical interpolation — eight
//! reads. A cross-section evaluating that per output pixel would re-derive the
//! same eight-read tilt bracket for every row of a column that never moves in
//! azimuth or ground range. [`VolumeSampler::column`] computes the whole tilt
//! ladder over one ground point *once* — ~16 bilinears — and then answers any
//! number of heights with a two-point lerp each. Both consumers on the way
//! (the cross-section rasterizer and the voxel builder) are column-shaped, so
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
//! [`crate::hhc::merge_split_cut_doppler`] is **not** used to fill a
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

use nexrad_model::data::{DataMoment, MomentData, Radial, Scan};

use crate::beam;
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
/// The two velocity *derivations* are refused for a third reason: NROT and SRM
/// /SRV are computed per sweep from a whole volume's wind fit, so sampling
/// them means deriving them first. That is a later work package's problem, not
/// a silent one — they are refused rather than quietly served from raw
/// velocity, which would look right and be a different field.
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
             computed before it can be sampled"
        }
        RadarProduct::SpecificDifferentialPhase | RadarProduct::PrecipitationRate => {
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
}

/// How many median azimuth steps two radials may be apart and still count as
/// adjacent — i.e. as a pair worth interpolating between.
///
/// One step is what consecutive radials are apart by construction, and a real
/// sweep's jitter is a few hundredths of a step, so 1.5 clears jitter by an
/// order and still catches one dropped radial (gap 2.0 steps). What happens
/// past it is not a fallback to nearest-across-the-hole, which is how a
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
        debug_assert!(above >= 1, "partition_point returned 0 after the guard");
        let lo = &self.rungs[above - 1];
        let hi = &self.rungs[above];
        let span = hi.height_km - lo.height_km;
        // Two rungs at the same height is only reachable at zero ground range,
        // where every beam centre is at zero. Weighting the lower one fully is
        // the only defined choice and matches the `above == len` arm above.
        let t = if span > 0.0 {
            ((height_km - lo.height_km) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        blend(self.blend, &[lo.sample, hi.sample], &[1.0 - t, t])
    }
}

impl std::fmt::Debug for VolumeSampler<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Point queries against a borrowed volume, for one moment.
///
/// Construction resolves the tilt ladder (see the module doc) and indexes each
/// rung's radials by azimuth; it decodes no gates. Gates are decoded on demand
/// out of `raw_values()`.
pub struct VolumeSampler<'a> {
    product: RadarProduct,
    slot: MomentSlot,
    blend: Blend,
    rungs: Vec<Rung<'a>>,
}

impl<'a> VolumeSampler<'a> {
    /// Resolve `scan`'s tilt ladder for `product`.
    ///
    /// Fails rather than degrades — see the module doc's section on the VCP.
    /// Every error is also logged, so a caller that discards the `Result` with
    /// `.ok()` still leaves the reason somewhere.
    pub fn new(scan: &'a Scan, product: RadarProduct) -> Result<Self, SamplerError> {
        Self::build(scan, product).inspect_err(|e| {
            log::warn!("volume sampler unavailable for {}: {e}", product.code());
        })
    }

    fn build(scan: &'a Scan, product: RadarProduct) -> Result<Self, SamplerError> {
        let Some(slot) = samplable(product) else {
            return Err(SamplerError::NotSamplable {
                product,
                reason: refusal_reason(product),
            });
        };

        let cuts = scan.coverage_pattern().elevation_cuts();
        if cuts.is_empty() {
            return Err(SamplerError::EmptyCoveragePattern {
                vcp: scan.coverage_pattern().pattern_number().number(),
            });
        }

        // Step 1 and 2: key every sweep on its cut, then group by exact key,
        // preserving volume order inside each group so "newest" below means
        // what it says.
        let mut groups: Vec<(f64, Vec<usize>)> = Vec::new();
        for (sweep_index, sweep) in scan.sweeps().iter().enumerate() {
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
        let mut rungs: Vec<Rung<'a>> = Vec::with_capacity(groups.len());
        for (key, members) in groups {
            let carries = |&i: &usize| -> bool {
                scan.sweeps()[i]
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
                            && scan.sweeps()[i]
                                .radials()
                                .first()
                                .is_some_and(|r| r.velocity().is_none())
                    })
                    .or_else(|| members.iter().rev().find(|i| carries(i)))
            };
            let Some(&chosen) = chosen else { continue };

            let radials = scan.sweeps()[chosen].radials();
            // Step 4: the geometry is the chosen sweep's median, never the key.
            let Some(elevation_deg) = sweep_elevation_deg(radials) else {
                continue;
            };
            let (by_azimuth, az_step_deg) = index_azimuths(radials);
            rungs.push(Rung {
                nominal_deg: key,
                elevation_deg,
                radials,
                by_azimuth,
                az_step_deg,
            });
        }

        if rungs.is_empty() {
            return Err(SamplerError::NoSweepsWithMoment { product });
        }
        rungs.sort_by(|a, b| a.nominal_deg.total_cmp(&b.nominal_deg));

        Ok(Self {
            product,
            slot,
            blend: Blend::for_moment(product),
            rungs,
        })
    }

    /// The moment this sampler was built for.
    pub fn product(&self) -> RadarProduct {
        self.product
    }

    /// The ladder, as one line.
    ///
    /// Hand-written rather than derived because a derived `Debug` would walk
    /// the borrowed radials and print the whole ~10 M-gate volume — which is
    /// what `assert_eq!` and `unwrap` reach for on failure, so the derive
    /// would turn a one-line test failure into an unreadable one.
    fn describe(&self) -> String {
        format!(
            "{} on {} rungs {:?} (nominal {:?})",
            self.product.code(),
            self.rungs.len(),
            self.elevations_deg().collect::<Vec<_>>(),
            self.nominal_elevations_deg().collect::<Vec<_>>(),
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

    /// Each rung's geometric elevation, ascending.
    pub fn elevations_deg(&self) -> impl Iterator<Item = f64> + '_ {
        self.rungs.iter().map(|r| r.elevation_deg)
    }

    /// Each rung's VCP cut angle, ascending — the grouping key, not geometry.
    /// Exposed so a caller can show which declared cuts a volume actually
    /// delivered.
    pub fn nominal_elevations_deg(&self) -> impl Iterator<Item = f64> + '_ {
        self.rungs.iter().map(|r| r.nominal_deg)
    }

    /// The largest angular step between consecutive rungs, degrees. `0.0` for
    /// a single-rung ladder.
    pub fn widest_tilt_gap_deg(&self) -> f64 {
        self.rungs
            .windows(2)
            .map(|w| w[1].elevation_deg - w[0].elevation_deg)
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
    /// Equivalent to `self.column(az, ground).at_height_km(h)` and pinned as
    /// such; prefer [`column`](Self::column) whenever more than one height of
    /// the same column is wanted, which is about eight times cheaper per pixel
    /// for a cross-section.
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
        blend(self.blend, &corners, &weights)
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
    let interval = moment.gate_interval_km();
    if !interval.is_finite() || interval <= 0.0 {
        let s = Sample::missing(SampleStatus::NoCoverage);
        return (s, s, 0.0);
    }
    let x = (slant_km - moment.first_gate_range_km()) / interval;
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
/// Ties go to the earliest corner, so the result does not depend on iteration
/// order.
fn blend(kind: Blend, corners: &[Sample], weights: &[f64]) -> Sample {
    debug_assert_eq!(
        corners.len(),
        weights.len(),
        "every corner needs exactly one weight",
    );
    if corners.iter().all(|c| c.status == SampleStatus::Value) {
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
mod tests {
    use super::*;
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern,
        WaveformType,
    };

    // ── Fixtures ────────────────────────────────────────────────────────────
    //
    // Every fixture uses a **nonzero** first gate (2.125 km, the operational
    // super-resolution value) rather than 0, because `first_gate_range_km` is
    // a gate *centre* and a sampler that forgot it would be ~2 km — eight
    // gates — inward on every read while still passing any test that started
    // its gates at the origin.

    const REFL_SCALE: f32 = 2.0;
    const REFL_OFFSET: f32 = 66.0;
    const VEL_SCALE: f32 = 2.0;
    const VEL_OFFSET: f32 = 129.0;
    const FIRST_GATE_M: u16 = 2125;
    const GATE_M: u16 = 250;

    /// dBZ through the reflectivity encoding. Clamped at 2 because 0 and 1 are
    /// the below-threshold and range-folded status codes.
    fn encode_refl(dbz: f64) -> u8 {
        ((dbz * f64::from(REFL_SCALE) + f64::from(REFL_OFFSET)).round() as i64).clamp(2, 255) as u8
    }

    /// What `encode_refl` round-trips to. Assertions compare against this
    /// rather than against the dBZ that went in, so a 0.5 dB quantisation step
    /// is not mistaken for a sampler error.
    fn round_trip_refl(dbz: f64) -> f64 {
        f64::from((f32::from(encode_refl(dbz)) - REFL_OFFSET) / REFL_SCALE)
    }

    fn encode_vel(ms: f64) -> u8 {
        ((ms * f64::from(VEL_SCALE) + f64::from(VEL_OFFSET)).round() as i64).clamp(2, 255) as u8
    }

    fn round_trip_vel(ms: f64) -> f64 {
        f64::from((f32::from(encode_vel(ms)) - VEL_OFFSET) / VEL_SCALE)
    }

    /// The slant range, km, of gate `j` in every fixture below.
    fn gate_slant_km(j: usize) -> f64 {
        f64::from(FIRST_GATE_M) / 1000.0 + j as f64 * f64::from(GATE_M) / 1000.0
    }

    fn moment_from(bytes: Vec<u8>, scale: f32, offset: f32) -> MomentData {
        MomentData::from_fixed_point(
            bytes.len() as u16,
            FIRST_GATE_M,
            GATE_M,
            8,
            scale,
            offset,
            bytes,
        )
    }

    /// A field to plant: dBZ (or m/s) at an azimuth and slant range, or `None`
    /// for below threshold.
    type Field<'f> = &'f dyn Fn(f64, f64) -> Option<f64>;

    /// One sweep, carrying whichever of the two moments is asked for.
    ///
    /// Azimuths are `i · 360/n`, so a 720-radial sweep sits on exact halves of
    /// a degree and a query at a radial centre lands on it bit-exactly.
    fn make_sweep(
        elevation_number: u8,
        elevation_deg: f32,
        n_radials: usize,
        n_gates: usize,
        refl: Option<Field<'_>>,
        vel: Option<Field<'_>>,
    ) -> Sweep {
        let spacing = 360.0 / n_radials as f32;
        let radials = (0..n_radials)
            .map(|i| {
                let az = i as f32 * spacing;
                let build = |f: Field<'_>, scale: f32, offset: f32, enc: &dyn Fn(f64) -> u8| {
                    let bytes: Vec<u8> = (0..n_gates)
                        .map(|j| match f(f64::from(az), gate_slant_km(j)) {
                            None => 0,
                            Some(v) => enc(v),
                        })
                        .collect();
                    moment_from(bytes, scale, offset)
                };
                Radial::new(
                    0,
                    i as u16,
                    az,
                    spacing,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    refl.map(|f| build(f, REFL_SCALE, REFL_OFFSET, &encode_refl)),
                    vel.map(|f| build(f, VEL_SCALE, VEL_OFFSET, &encode_vel)),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    }

    /// A reflectivity-only sweep of a constant field.
    fn flat_refl_sweep(
        elevation_number: u8,
        elevation_deg: f32,
        n_radials: usize,
        n_gates: usize,
        dbz: f64,
    ) -> Sweep {
        make_sweep(
            elevation_number,
            elevation_deg,
            n_radials,
            n_gates,
            Some(&move |_, _| Some(dbz)),
            None,
        )
    }

    fn cut(angle_deg: f64) -> ElevationCut {
        ElevationCut::new(
            angle_deg,
            ChannelConfiguration::ConstantPhase,
            WaveformType::CS,
            20.0,
            true,
            true,
            false,
            false,
            1,
            20,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0,
            false,
            0,
            false,
            false,
        )
    }

    fn vcp(cut_angles: &[f64]) -> VolumeCoveragePattern {
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            cut_angles.iter().copied().map(cut).collect(),
        )
    }

    // ── The tilt ladder ─────────────────────────────────────────────────────

    /// The rule the whole campaign settled on, in the one geometry that proves
    /// no angular threshold can substitute for it.
    ///
    /// KBMX under VCP 212 with the adaptive base tilt declares genuine cuts at
    /// **0.40° and 0.48° — 0.09° apart** — while the spread of first-radial
    /// angles *within* the 0.48° cut is 0.088° and the gap to the 0.40° cut is
    /// also 0.088°. The windows touch exactly. Reproduced here with medians
    /// 0.09° apart, which is what the fixture asserts as a precondition: any
    /// threshold wide enough to close a cut's own spread also swallows a whole
    /// genuine cut, and at 0.2° the failure is not a merged pair but a
    /// *vanished* rung inside a plausible monotone ladder.
    #[test]
    fn the_ladder_separates_cuts_no_angular_threshold_can() {
        let scan = Scan::new(
            vcp(&[0.40, 0.48, 0.90]),
            vec![
                flat_refl_sweep(1, 0.44, 360, 40, 20.0),
                flat_refl_sweep(2, 0.53, 360, 40, 40.0),
                flat_refl_sweep(3, 0.91, 360, 40, 30.0),
            ],
        );

        let separation = 0.53 - 0.44;
        // precondition: the two cuts really are inside every threshold the
        // campaign measured (0.10 / 0.15 / 0.20 / 0.30), so a rule that split
        // them cannot have done it by angle.
        assert!(
            separation < 0.10,
            "precondition: the fixture's cuts are {separation:.3}° apart, \
             which the 0.10° threshold would already separate — the test no \
             longer proves anything about thresholds",
        );

        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        assert_eq!(
            sampler.tilt_count(),
            3,
            "the cut table declares three cuts and three sweeps arrived; the \
             ladder found {} rungs at elevations {:?}",
            sampler.tilt_count(),
            sampler.elevations_deg().collect::<Vec<_>>(),
        );
        let nominal: Vec<f64> = sampler.nominal_elevations_deg().collect();
        assert_eq!(nominal, vec![0.40, 0.48, 0.90]);

        // And each rung really carries its own sweep's data, which is what
        // "the 0.48° cut vanished" would have destroyed.
        let column = sampler.column(45.0, 8.0);
        let values: Vec<f64> = column
            .rungs()
            .iter()
            .map(|r| f64::from(r.sample.value().expect("every rung has data at 8 km")))
            .collect();
        assert_eq!(
            values,
            vec![
                round_trip_refl(20.0),
                round_trip_refl(40.0),
                round_trip_refl(30.0)
            ],
        );
    }

    /// The nominal cut angle is the grouping key and nothing else: the
    /// geometry is the chosen sweep's median radial elevation, which measured
    /// volumes put up to 0.044° off nominal.
    #[test]
    fn a_rungs_geometry_is_its_sweeps_median_not_the_nominal_cut() {
        let scan = Scan::new(
            vcp(&[0.5, 4.0]),
            vec![
                flat_refl_sweep(1, 0.544, 360, 40, 20.0),
                flat_refl_sweep(2, 3.968, 360, 40, 20.0),
            ],
        );
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

        let geometry: Vec<f64> = sampler.elevations_deg().collect();
        let nominal: Vec<f64> = sampler.nominal_elevations_deg().collect();
        assert_eq!(nominal, vec![0.5, 4.0]);
        for (g, n) in geometry.iter().zip(&nominal) {
            let off = (g - n).abs();
            assert!(
                (0.03..0.05).contains(&off),
                "the fixture planted a ~0.044° offset but the rung reports \
                 {g}° against a nominal {n}° ({off}° apart) — the ladder is \
                 using the key as geometry",
            );
        }

        // The consequence, in metres: at 100 km the 0.032° offset on the 4.0°
        // cut moves the beam centre far enough to matter, and exactly the kind
        // of error that reads as plausible.
        let with_median = beam::height_at_ground_km(100.0, geometry[1]);
        let with_nominal = beam::height_at_ground_km(100.0, nominal[1]);
        assert!(
            (with_median - with_nominal).abs() * 1000.0 > 40.0,
            "the median/nominal height gap at 100 km is only {:.1} m, so this \
             distinction stopped mattering",
            (with_median - with_nominal).abs() * 1000.0,
        );
    }

    /// A split cut is two VCP cuts at one angle: a surveillance half reaching
    /// 460 km with no velocity, and a Doppler half reaching 300 km with it.
    /// Reflectivity belongs to the surveillance half; velocity has no choice
    /// but the Doppler one.
    #[test]
    fn a_non_doppler_moment_takes_the_surveillance_half_of_a_split_cut() {
        // 1832 gates from 2.125 km at 250 m reaches 460 km; 1200 reaches 302.
        let scan = Scan::new(
            vcp(&[0.5, 0.5, 0.9]),
            vec![
                make_sweep(1, 0.5, 360, 1832, Some(&|_, _| Some(20.0)), None),
                make_sweep(
                    2,
                    0.5,
                    360,
                    1200,
                    Some(&|_, _| Some(45.0)),
                    Some(&|_, _| Some(10.0)),
                ),
                make_sweep(3, 0.9, 360, 1200, Some(&|_, _| Some(30.0)), None),
            ],
        );

        let refl = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        assert_eq!(refl.tilt_count(), 2, "the two 0.5° cuts are one rung");
        let low = refl.column(45.0, 100.0).rungs()[0].sample;
        assert_eq!(
            f64::from(low.value().expect("the surveillance half has 100 km")),
            round_trip_refl(20.0),
            "reflectivity came from the Doppler half of the split cut",
        );

        // The reason it matters: only the surveillance half reaches past
        // 300 km, and the Doppler half would have reported nothing there.
        let far = refl.column(45.0, 400.0).rungs()[0].sample;
        assert_eq!(
            f64::from(far.value().expect("460 km of surveillance gates")),
            round_trip_refl(20.0),
        );

        // Velocity has one candidate and takes it.
        let vel = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
        assert_eq!(
            vel.tilt_count(),
            1,
            "only the Doppler half of the 0.5° cut carries velocity, and the \
             0.9° cut carries none",
        );
        let v = vel.column(45.0, 100.0).rungs()[0].sample;
        assert_eq!(
            f64::from(v.value().expect("the Doppler half has 100 km")),
            round_trip_vel(10.0),
        );
    }

    /// SAILS repeats the low cuts minutes apart. The newest is what the
    /// reference display shows and what a section must show.
    #[test]
    fn the_newest_sweep_of_a_repeated_cut_wins_its_rung() {
        let scan = Scan::new(
            vcp(&[0.5, 0.9, 0.5]),
            vec![
                flat_refl_sweep(1, 0.5, 360, 40, 20.0),
                flat_refl_sweep(2, 0.9, 360, 40, 30.0),
                flat_refl_sweep(3, 0.5, 360, 40, 55.0), // the SAILS repeat
            ],
        );
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        assert_eq!(sampler.tilt_count(), 2);
        assert_eq!(
            f64::from(sampler.column(45.0, 8.0).rungs()[0].sample.value().unwrap()),
            round_trip_refl(55.0),
            "the rung kept the first 0.5° cut rather than the SAILS repeat",
        );
    }

    /// The cut table stores a below-horizon angle as a two's-complement value
    /// this decoder hands back unsigned, so −0.3° arrives as 359.7°. Left
    /// uncorrected it sorts above 19.5° and inverts the whole ladder.
    #[test]
    fn a_cut_angle_past_180_degrees_wraps_to_a_negative_elevation() {
        let scan = Scan::new(
            vcp(&[359.7, 0.5, 4.0]),
            vec![
                flat_refl_sweep(1, -0.28, 360, 40, 20.0),
                flat_refl_sweep(2, 0.52, 360, 40, 30.0),
                flat_refl_sweep(3, 4.02, 360, 40, 40.0),
            ],
        );
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        let nominal: Vec<f64> = sampler.nominal_elevations_deg().collect();
        assert!(
            (nominal[0] - -0.3).abs() < 1e-9,
            "359.7° did not wrap to −0.3°: the ladder reads {nominal:?}",
        );
        assert_eq!(nominal.len(), 3);
        assert!(
            nominal.windows(2).all(|w| w[0] < w[1]),
            "the ladder is not ascending: {nominal:?}",
        );
        // Without the correction the 359.7° cut sorts to the top, so the
        // highest rung would be 359.7° rather than 4.0°.
        assert!(
            nominal[2] < 180.0,
            "an unwrapped cut angle is still in the ladder: {nominal:?}",
        );
    }

    /// The refusal that keeps the worker from building a different ladder from
    /// the main thread's — pinned against the **real** `RenderInput` round
    /// trip rather than a hand-built placeholder, because the hand-built one
    /// would stop matching the day `render_input` changed.
    #[test]
    fn a_reconstructed_render_input_scan_is_refused() {
        let scan = Scan::new(
            vcp(&[0.5, 0.9]),
            vec![
                flat_refl_sweep(1, 0.5, 360, 40, 20.0),
                flat_refl_sweep(2, 0.9, 360, 40, 30.0),
            ],
        );
        // precondition: the original scan samples fine, so the refusal below
        // is about the reconstruction and not about the fixture.
        assert!(VolumeSampler::new(&scan, RadarProduct::Reflectivity).is_ok());

        let input = crate::render_input::RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::Reflectivity,
            35.33,
            -97.27,
            None,
            None,
        )
        .expect("the fixture carries reflectivity at 0.5°");
        let reconstructed = input.to_scan();

        // precondition: the reconstruction really did keep a renderable sweep,
        // so what fails below is the ladder and not the payload.
        assert!(
            !reconstructed.sweeps().is_empty(),
            "precondition: the round trip dropped every sweep",
        );
        assert!(
            crate::render::render_from(&input).is_some(),
            "precondition: the reconstructed input no longer renders, so this \
             test is measuring a broken fixture rather than the sampler",
        );

        let err = VolumeSampler::new(&reconstructed, RadarProduct::Reflectivity).expect_err(
            "the sampler accepted a scan whose coverage pattern is a \
             placeholder — it has just built a ladder in the worker that does \
             not match the main thread's, silently",
        );
        assert!(
            matches!(err, SamplerError::EmptyCoveragePattern { .. }),
            "expected the empty-cut-table refusal, got {err:?}",
        );
        // The message has to say enough for whoever hits it to know why.
        let text = err.to_string();
        assert!(
            text.contains("elevation cuts") && text.contains("RenderInput"),
            "the refusal does not explain itself: {text}",
        );
    }

    /// The second half of the same guard: a cut table that exists but does not
    /// cover a sweep's elevation number. Measured to happen on 0 of 203 real
    /// volumes, so it means the sweep-to-VCP pairing has broken.
    #[test]
    fn an_elevation_number_outside_the_cut_table_is_refused() {
        for elevation_number in [0u8, 3, 255] {
            let scan = Scan::new(
                vcp(&[0.5, 0.9]),
                vec![flat_refl_sweep(elevation_number, 0.5, 360, 40, 20.0)],
            );
            let err = VolumeSampler::new(&scan, RadarProduct::Reflectivity)
                .expect_err("an elevation number outside the table indexed a two-cut VCP");
            assert!(
                matches!(
                    err,
                    SamplerError::ElevationNumberOutOfCutTable { cut_count: 2, .. }
                ),
                "expected the cut-table index refusal for elevation number \
                 {elevation_number}, got {err:?}",
            );
        }
        // And the in-range numbers still work, so the guard is a boundary
        // rather than a refusal of everything.
        for elevation_number in [1u8, 2] {
            let scan = Scan::new(
                vcp(&[0.5, 0.9]),
                vec![flat_refl_sweep(elevation_number, 0.5, 360, 40, 20.0)],
            );
            assert!(VolumeSampler::new(&scan, RadarProduct::Reflectivity).is_ok());
        }
    }

    /// A volume with no sweep carrying the moment is a refusal too, rather
    /// than an empty ladder that answers `NoCoverage` at every point and looks
    /// like a blank section.
    #[test]
    fn a_volume_with_no_sweep_carrying_the_moment_is_refused() {
        let scan = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 360, 40, 20.0)]);
        let err = VolumeSampler::new(&scan, RadarProduct::CorrelationCoefficient)
            .expect_err("the fixture carries reflectivity only");
        assert!(matches!(err, SamplerError::NoSweepsWithMoment { .. }));
    }

    // ── Geometry ────────────────────────────────────────────────────────────

    /// A field that depends only on beam height reads back at the height it
    /// was planted at.
    ///
    /// The slab is 4–5 km; at 30 km ground range the fixture's half-degree
    /// ladder puts rungs ~0.26 km apart, so the slab is four rungs thick and
    /// its edges are resolvable.
    #[test]
    fn a_planted_horizontal_slab_reads_at_its_planted_height() {
        let angles: Vec<f64> = (1..=40).map(|i| f64::from(i) * 0.5).collect();
        let slab = |h: f64| if (4.0..5.0).contains(&h) { 50.0 } else { 20.0 };
        let sweeps: Vec<Sweep> = angles
            .iter()
            .enumerate()
            .map(|(i, &e)| {
                make_sweep(
                    i as u8 + 1,
                    e as f32,
                    360,
                    600,
                    Some(&move |_, slant| Some(slab(beam::height_km(slant, e)))),
                    None,
                )
            })
            .collect();
        let scan = Scan::new(vcp(&angles), sweeps);
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        assert_eq!(sampler.tilt_count(), 40);

        let column = sampler.column(37.0, 30.0);
        let (lowest, highest) = column.height_span_km().unwrap();
        // precondition: the ladder brackets the slab at this ground range, so
        // every assertion below is an interpolation rather than a refusal.
        assert!(
            lowest < 3.0 && highest > 6.0,
            "precondition: at 30 km the ladder spans {lowest:.2}–{highest:.2} \
             km and does not bracket the 4–5 km slab",
        );

        let at = |h: f64| f64::from(column.at_height_km(h).value().unwrap());
        assert!(
            (at(4.5) - round_trip_refl(50.0)).abs() < 1e-6,
            "{}",
            at(4.5)
        );
        assert!(
            (at(2.0) - round_trip_refl(20.0)).abs() < 1e-6,
            "{}",
            at(2.0)
        );
        assert!(
            (at(6.5) - round_trip_refl(20.0)).abs() < 1e-6,
            "{}",
            at(6.5)
        );

        // The edges land where they were planted, to within the rung spacing
        // that resolves them — ~0.26 km at this ground range.
        let midpoint = 0.5 * (round_trip_refl(20.0) + round_trip_refl(50.0));
        let crossing = |from: f64, to: f64| {
            let steps = 4000;
            (0..=steps)
                .map(|i| from + (to - from) * f64::from(i) / f64::from(steps))
                .find(|&h| f64::from(column.at_height_km(h).value().unwrap()) > midpoint)
                .unwrap()
        };
        let bottom = crossing(3.0, 4.5);
        let top = crossing(6.0, 4.5);
        assert!(
            (bottom - 4.0).abs() < 0.3,
            "the slab's floor read at {bottom:.3} km, planted at 4.0",
        );
        assert!(
            (top - 5.0).abs() < 0.3,
            "the slab's ceiling read at {top:.3} km, planted at 5.0",
        );
    }

    /// The `cos e` test. A wall planted at a **ground** range reads at that
    /// ground range on every tilt; without the correction the 10° tilt puts it
    /// 1.5 km out.
    #[test]
    fn a_planted_vertical_wall_reads_at_its_planted_ground_range() {
        const WALL_KM: f64 = 100.0;
        let angles = [0.5f64, 4.0, 10.0];
        let sweeps: Vec<Sweep> = angles
            .iter()
            .enumerate()
            .map(|(i, &e)| {
                make_sweep(
                    i as u8 + 1,
                    e as f32,
                    360,
                    600,
                    Some(&move |_, slant| {
                        let ground = beam::ground_range_km(slant, e);
                        Some(if (ground - WALL_KM).abs() <= 0.5 {
                            55.0
                        } else {
                            10.0
                        })
                    }),
                    None,
                )
            })
            .collect();
        let scan = Scan::new(vcp(&angles), sweeps);
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

        let on_wall = sampler.column(120.0, WALL_KM);
        for (k, rung) in on_wall.rungs().iter().enumerate() {
            assert_eq!(
                f64::from(rung.sample.value().unwrap()),
                round_trip_refl(55.0),
                "rung {k} at {}° missed the wall at {WALL_KM} km ground",
                rung.elevation_deg,
            );
        }

        // The discriminating half. A sampler that fed the ground range to the
        // gate index as if it were a slant range reads the 10° tilt's wall at
        // `100 · cos 10° = 98.48` km — 1.52 km, six gates, inward. That
        // position must be clear air.
        let uncorrected = beam::ground_range_km(WALL_KM, 10.0);
        let error_km = WALL_KM - uncorrected;
        assert!(
            (error_km - 1.5192).abs() < 1e-3,
            "the 10° cos e error moved: {error_km:.4} km, documented as 1.5192",
        );
        let off_wall = sampler.column(120.0, uncorrected);
        let steep = off_wall
            .rungs()
            .iter()
            .find(|r| r.elevation_deg > 9.0)
            .expect("a 10° rung");
        assert_eq!(
            f64::from(steep.sample.value().unwrap()),
            round_trip_refl(10.0),
            "the 10° rung found the wall at {uncorrected:.3} km ground, which \
             is where an uncorrected slant range would have put it",
        );
        // At 0.5° the same mistake is 0.004 km — a sixtieth of a gate — which
        // is why the low tilts cannot be the test.
        let shallow_error = WALL_KM - beam::ground_range_km(WALL_KM, 0.5);
        assert!(
            shallow_error < 0.01,
            "precondition: the 0.5° cos e error is {shallow_error:.4} km, so \
             the low tilts would have discriminated too and the steep tilt is \
             not doing the work",
        );

        // And the point query agrees with the column, at a height inside the
        // ladder.
        let h = on_wall.rungs()[1].height_km;
        assert_eq!(
            sampler.sample(120.0, WALL_KM, h),
            on_wall.at_height_km(h),
            "the point query and the column disagree",
        );
    }

    /// The divergence this module ships as a measurement rather than as a
    /// comment: `render::render_gate` applies **no** `cos e` at all (it never
    /// receives an elevation angle), so a section and the plan view will not
    /// register above ~2°.
    ///
    /// Both figures name their target, because `IMAGE_SIZE` is 2048 natively
    /// and 1024 on wasm32 and the pixel counts halve with it.
    #[test]
    fn the_cos_e_correction_diverges_from_the_plan_view_by_a_measured_amount() {
        let cases = [(230.0f64, 2.4f64, 0.2017f64), (70.0, 19.5, 4.0151)];
        for (slant, elev, expected_km) in cases {
            let gap_km = slant - beam::ground_range_km(slant, elev);
            assert!(
                (gap_km - expected_km).abs() < 1e-3,
                "the {elev}° / {slant} km slant-to-ground gap moved: \
                 {gap_km:.4} km, documented as {expected_km}",
            );
        }

        let px = |slant: f64, elev: f64| {
            (slant - beam::ground_range_km(slant, elev)) * crate::types::PIXELS_PER_KM
        };
        // 2048 px over 460 km is 4.4522 px/km; wasm32 halves both.
        #[cfg(not(target_arch = "wasm32"))]
        let (expected_low, expected_high) = (0.898, 17.876);
        #[cfg(target_arch = "wasm32")]
        let (expected_low, expected_high) = (0.449, 8.938);
        assert_eq!(
            crate::types::IMAGE_SIZE,
            if cfg!(target_arch = "wasm32") {
                1024
            } else {
                2048
            },
            "IMAGE_SIZE moved, so the pixel figures below name the wrong target",
        );
        let low = px(230.0, 2.4);
        let high = px(70.0, 19.5);
        assert!(
            (low - expected_low).abs() < 0.01,
            "at 2.4° / 230 km the section sits {low:.3} px off the plan view \
             on a {}-pixel image, documented as {expected_low}",
            crate::types::IMAGE_SIZE,
        );
        assert!(
            (high - expected_high).abs() < 0.01,
            "at 19.5° / 70 km the section sits {high:.3} px off the plan view \
             on a {}-pixel image, documented as {expected_high}",
            crate::types::IMAGE_SIZE,
        );
        // precondition: the disagreement is invisible at the low tilts, which
        // is why "above ~2°" is the way it is stated.
        assert!(
            px(230.0, 0.5) < 0.2,
            "the 0.5° divergence is now {:.3} px, so the ~2° threshold in the \
             module doc is wrong",
            px(230.0, 0.5),
        );
    }

    // ── Interpolation ───────────────────────────────────────────────────────

    /// Reflectivity averages in linear Z. 10 and 50 dBZ meet at **46.99**, not
    /// at 30 — a 17 dB error, which is four palette bands.
    ///
    /// Also the super-resolution half of the acceptance: 720 alternating
    /// radials each return their own value. **This covers the low tilts
    /// only** — azimuth resolution drops 720 → 360 partway up every real
    /// ladder, which the fixture reproduces and the assertion below states.
    #[test]
    fn reflectivity_blends_in_linear_z_and_every_super_res_radial_survives() {
        let alternating = |az: f64, _slant: f64| {
            Some(if (az / 0.5).round() as i64 % 2 == 0 {
                10.0
            } else {
                50.0
            })
        };
        let scan = Scan::new(
            vcp(&[0.5, 4.0]),
            vec![
                make_sweep(1, 0.5, 720, 200, Some(&alternating), None),
                flat_refl_sweep(2, 4.0, 360, 200, 30.0),
            ],
        );
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

        // Every one of the 720 radials returns its own planted value.
        let mut seen_low = 0usize;
        let mut seen_high = 0usize;
        for i in 0..720u32 {
            let az = f64::from(i as f32 * 0.5);
            let got = f64::from(sampler.column(az, 20.0).rungs()[0].sample.value().unwrap());
            let want = round_trip_refl(if i % 2 == 0 { 10.0 } else { 50.0 });
            assert!(
                (got - want).abs() < 1e-4,
                "radial {i} at {az}° read {got} dBZ, planted {want}",
            );
            if i % 2 == 0 {
                seen_low += 1;
            } else {
                seen_high += 1;
            }
        }
        assert_eq!((seen_low, seen_high), (360, 360));

        // Halfway between two radials: linear Z, not dB.
        let mid = f64::from(
            sampler.column(0.25, 20.0).rungs()[0]
                .sample
                .value()
                .unwrap(),
        );
        let linear_z = 10.0 * (0.5 * (10f64.powf(1.0) + 10f64.powf(5.0))).log10();
        assert!(
            (mid - linear_z).abs() < 0.01,
            "halfway between 10 and 50 dBZ read {mid:.3}, expected \
             {linear_z:.3} (the arithmetic mean, 30.0, is the wrong answer)",
        );
        assert!(
            (mid - 46.9897).abs() < 0.01,
            "the documented 46.99 moved: {mid:.4}",
        );
        assert!((mid - 30.0).abs() > 16.0);

        // The coverage caveat, asserted rather than left in prose: the upper
        // rung of this ladder has 360 radials, so an "all 720" test says
        // nothing about it.
        assert_eq!(sampler.column(0.25, 20.0).rungs().len(), 2);
        assert_eq!(scan.sweeps()[0].radials().len(), 720);
        assert_eq!(
            scan.sweeps()[1].radials().len(),
            360,
            "precondition: the fixture no longer drops to 360 radials on the \
             upper tilt, so this test's super-resolution claim covers more \
             than it should",
        );
    }

    /// Everything that is not reflectivity averages arithmetically, and
    /// differential phase averages on the circle so the 360°→0° fold does not
    /// become a half turn.
    #[test]
    fn velocity_averages_arithmetically_and_phase_averages_on_the_circle() {
        let alternating = |az: f64, _: f64| {
            Some(if az.round() as i64 % 2 == 0 {
                -20.0
            } else {
                20.0
            })
        };
        let scan = Scan::new(
            vcp(&[0.5]),
            vec![make_sweep(1, 0.5, 360, 200, None, Some(&alternating))],
        );
        let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
        let mid = f64::from(sampler.column(0.5, 20.0).rungs()[0].sample.value().unwrap());
        let arithmetic = 0.5 * (round_trip_vel(-20.0) + round_trip_vel(20.0));
        assert!(
            (mid - arithmetic).abs() < 1e-4,
            "velocity halfway between {} and {} read {mid}, expected \
             {arithmetic}",
            round_trip_vel(-20.0),
            round_trip_vel(20.0),
        );

        // Differential phase: 359° and 1° meet at 0°, not at 180°. Encoded
        // 16-bit at 1/100°, which keeps both ends clear of the 0/1 status
        // codes.
        let radials: Vec<Radial> = (0..360)
            .map(|i| {
                let v = if i % 2 == 0 { 359.0f64 } else { 1.0 };
                let raw = (v * 100.0).round() as u16;
                let bytes: Vec<u8> = (0..40).flat_map(|_| raw.to_be_bytes()).collect();
                Radial::new(
                    0,
                    i,
                    f32::from(i),
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    1,
                    0.5,
                    None,
                    None,
                    None,
                    None,
                    Some(MomentData::from_fixed_point(
                        40,
                        FIRST_GATE_M,
                        GATE_M,
                        16,
                        100.0,
                        0.0,
                        bytes,
                    )),
                    None,
                    None,
                )
            })
            .collect();
        let scan = Scan::new(vcp(&[0.5]), vec![Sweep::new(1, radials)]);
        let sampler = VolumeSampler::new(&scan, RadarProduct::DifferentialPhase).unwrap();
        // 8 km ground: gate 24 of 40, comfortably inside this moment's span.
        let seam = f64::from(sampler.column(0.5, 8.0).rungs()[0].sample.value().unwrap());
        let off_zero = seam.min(360.0 - seam).abs();
        assert!(
            off_zero < 0.01,
            "359° and 1° averaged to {seam}°, which is {off_zero}° off the 0° \
             they straddle — a linear lerp would say 180°",
        );
        assert!(
            (seam - 180.0).abs() > 100.0,
            "differential phase is being lerped across the 360° fold",
        );
    }

    /// The gap `MomentValue::RangeFolded` never crossed: five different
    /// reasons for having no number, all distinguishable, none of them `NaN`
    /// alone.
    #[test]
    fn a_range_folded_gate_is_distinguishable_from_a_missing_one() {
        // Gate 0 below threshold, gate 1 range folded, gates 2.. ordinary.
        let mut bytes = vec![0u8, 1];
        bytes.extend((2..40).map(|_| encode_vel(15.0)));
        let radials: Vec<Radial> = (0..360)
            .map(|i| {
                Radial::new(
                    0,
                    i,
                    f32::from(i),
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    1,
                    0.5,
                    None,
                    // Radial 200 carries no velocity at all.
                    (i != 200).then(|| moment_from(bytes.clone(), VEL_SCALE, VEL_OFFSET)),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        let scan = Scan::new(vcp(&[0.5]), vec![Sweep::new(1, radials)]);
        let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();

        // Gate centres at a radial centre, so the heaviest corner is
        // unambiguous. Ground range and slant range agree to 4 cm at 0.5°.
        let status_at =
            |az: f64, ground: f64| sampler.column(az, ground).rungs()[0].sample.status();
        let statuses = [
            status_at(10.0, gate_slant_km(0)),
            status_at(10.0, gate_slant_km(1)),
            status_at(10.0, gate_slant_km(5)),
            status_at(10.0, gate_slant_km(200)),
            status_at(0.5, 1.0),
        ];
        assert_eq!(statuses[0], SampleStatus::BelowThreshold);
        assert_eq!(statuses[1], SampleStatus::RangeFolded);
        assert_eq!(statuses[2], SampleStatus::Value);
        assert_eq!(statuses[3], SampleStatus::BeyondRange);
        assert_eq!(
            statuses[4],
            SampleStatus::NoCoverage,
            "inside the first gate"
        );
        assert_eq!(
            status_at(200.0, gate_slant_km(5)),
            SampleStatus::NoCoverage,
            "a radial with no moment",
        );

        // All five are distinct, which is the property that makes a hover
        // readout worth writing.
        for (i, a) in statuses.iter().enumerate() {
            for b in &statuses[i + 1..] {
                assert_ne!(a, b, "two conditions collapsed to {a:?}");
            }
        }
        // And a value is a value: the range-folded gate has none.
        assert!(
            sampler.column(10.0, gate_slant_km(1)).rungs()[0]
                .sample
                .value()
                .is_none()
        );
        assert!(
            sampler.column(10.0, gate_slant_km(5)).rungs()[0]
                .sample
                .value()
                .is_some()
        );
    }

    /// The duplication guard on `gate_sample`, against the model's own
    /// decoder, element for element.
    ///
    /// **Includes a `scale == 0.0` moment**, because that case disables the
    /// 0/1 status codes entirely and is the one a reimplementation gets wrong.
    ///
    /// A 16-bit moment with an *odd* byte count would exercise `gate_sample`'s
    /// `get(k..k + 2)` against the model's `chunks_exact`, and it is not
    /// tested here because it cannot be built: `MomentDataBlock::from_fixed_point`
    /// carries a `debug_assert!` refusing it, so the fixture would pass in
    /// release and panic in debug. The bounds are covered by the last-gate
    /// assertions below instead.
    #[test]
    fn raw_gate_decoding_matches_the_model_element_for_element() {
        let eight_bit: Vec<u8> = (0..=255u8).collect();
        let sixteen_bit: Vec<u8> = (0..600u16).flat_map(u16::to_be_bytes).collect();
        // A declared gate count that overruns the bytes, which is what makes
        // `raw_values().len()` rather than `gate_count()` authoritative.
        let short_bytes: Vec<u8> = (0..50u8).collect();

        let cases: Vec<(&str, MomentData)> = vec![
            (
                "8-bit, scaled",
                MomentData::from_fixed_point(256, FIRST_GATE_M, GATE_M, 8, 2.0, 66.0, eight_bit),
            ),
            (
                "8-bit, scale 0 (status codes disabled)",
                MomentData::from_fixed_point(
                    256,
                    FIRST_GATE_M,
                    GATE_M,
                    8,
                    0.0,
                    0.0,
                    (0..=255u8).collect(),
                ),
            ),
            (
                "16-bit, scaled",
                MomentData::from_fixed_point(
                    600,
                    FIRST_GATE_M,
                    GATE_M,
                    16,
                    100.0,
                    0.0,
                    sixteen_bit,
                ),
            ),
            (
                "8-bit, gate_count overruns the bytes",
                MomentData::from_fixed_point(400, FIRST_GATE_M, GATE_M, 8, 2.0, 66.0, short_bytes),
            ),
        ];

        let mut checked = 0usize;
        let mut saw_below = false;
        let mut saw_folded = false;
        let mut saw_unscaled_zero = false;
        for (label, moment) in &cases {
            let model = moment.values();
            for gate in 0..model.len() + 3 {
                let ours = gate_sample(moment, gate);
                match model.get(gate) {
                    None => assert_eq!(
                        ours.status(),
                        SampleStatus::BeyondRange,
                        "{label}: gate {gate} is past the model's {} values \
                         but decoded as {ours:?}",
                        model.len(),
                    ),
                    Some(nexrad_model::data::MomentValue::Value(v)) => {
                        assert_eq!(ours.status(), SampleStatus::Value, "{label} gate {gate}");
                        assert_eq!(
                            ours.value().unwrap().to_bits(),
                            v.to_bits(),
                            "{label}: gate {gate} decoded to {} where the \
                             model says {v}",
                            ours.value().unwrap(),
                        );
                        if *v == 0.0 && moment.scale() == 0.0 {
                            saw_unscaled_zero = true;
                        }
                    }
                    Some(nexrad_model::data::MomentValue::BelowThreshold) => {
                        assert_eq!(
                            ours.status(),
                            SampleStatus::BelowThreshold,
                            "{label} gate {gate}",
                        );
                        saw_below = true;
                    }
                    Some(nexrad_model::data::MomentValue::RangeFolded) => {
                        assert_eq!(
                            ours.status(),
                            SampleStatus::RangeFolded,
                            "{label} gate {gate}",
                        );
                        saw_folded = true;
                    }
                }
                checked += 1;
            }
        }
        // preconditions: the sweep actually reached each of the three decode
        // paths, so an implementation that got one of them wrong could not
        // have passed by never being asked.
        // 256 + 256 + 600 + 50 gates, plus three past the end of each.
        assert_eq!(checked, 1174, "the comparison grid changed size");
        assert!(saw_below, "no below-threshold gate was exercised");
        assert!(saw_folded, "no range-folded gate was exercised");
        assert!(
            saw_unscaled_zero,
            "the scale == 0.0 moment never returned raw 0 as a value, which \
             is the case this test exists for",
        );
        // `raw_values().len()` and not `gate_count()` decides where the gates
        // stop: this moment declares 400 and has 50.
        let short = &cases[3].1;
        assert_eq!(short.gate_count(), 400);
        assert_eq!(short.raw_values().len(), 50);
        assert_eq!(short.values().len(), 50, "the model trusts the bytes");
        assert_eq!(gate_sample(short, 49).status(), SampleStatus::Value);
        assert_eq!(
            gate_sample(short, 50).status(),
            SampleStatus::BeyondRange,
            "the declared gate count was trusted over the bytes",
        );

        // And the 16-bit moment's own last gate, so the two-byte stride's
        // bound is pinned too.
        let wide = &cases[2].1;
        assert_eq!(gate_sample(wide, 599).status(), SampleStatus::Value);
        assert_eq!(gate_sample(wide, 600).status(), SampleStatus::BeyondRange);
    }

    // ── The edges of the volume ─────────────────────────────────────────────

    /// Nothing is filled in outside the ladder, in either direction, and the
    /// cone of silence reports itself.
    #[test]
    fn nothing_is_extrapolated_above_or_below_the_ladder() {
        let angles = [0.5f64, 4.0, 10.0];
        let sweeps: Vec<Sweep> = angles
            .iter()
            .enumerate()
            .map(|(i, &e)| flat_refl_sweep(i as u8 + 1, e as f32, 360, 600, 35.0))
            .collect();
        let scan = Scan::new(vcp(&angles), sweeps);
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

        let column = sampler.column(90.0, 50.0);
        let (low, high) = column.height_span_km().unwrap();
        assert_eq!(
            column.at_height_km(low - 0.001).status(),
            SampleStatus::BelowLowestBeam,
        );
        assert_eq!(
            column.at_height_km(high + 0.001).status(),
            SampleStatus::AboveVolume,
        );
        // The boundaries themselves are inside.
        assert_eq!(column.at_height_km(low).status(), SampleStatus::Value);
        assert_eq!(column.at_height_km(high).status(), SampleStatus::Value);
        // Ground level under a 0.5° beam at 50 km is 0.6 km down and is not
        // invented.
        assert_eq!(
            column.at_height_km(0.0).status(),
            SampleStatus::BelowLowestBeam,
        );

        // The cone of silence: over the site every beam centre is at zero
        // height, so anything above the antenna is above the volume.
        let overhead = sampler.column(90.0, 0.0);
        assert_eq!(
            overhead.height_span_km(),
            Some((0.0, 0.0)),
            "the beams do not all meet at the antenna",
        );
        assert_eq!(
            overhead.at_height_km(3.0).status(),
            SampleStatus::AboveVolume,
            "the cone of silence was filled in rather than reported",
        );

        // An empty column answers the same way everywhere.
        let empty = Column::new();
        assert_eq!(empty.at_height_km(3.0).status(), SampleStatus::NoCoverage);
        assert_eq!(empty.height_span_km(), None);
        assert_eq!(
            column.at_height_km(f64::NAN).status(),
            SampleStatus::NoCoverage,
        );
    }

    /// The ordinary case the plan calls out: **every** volume has a bracketing
    /// rung with no data at 230 km and 300 km, because the upper cuts stop
    /// short. It is beam geometry, not a ladder defect, and it must surface as
    /// a status rather than be filled from the rung below.
    #[test]
    fn a_bracketing_rung_that_stops_short_reports_rather_than_being_filled() {
        // The 0.5° surveillance cut reaches 460 km; the 4.0° cut stops at
        // 150 km, which 8 of 19 measured volumes do.
        let scan = Scan::new(
            vcp(&[0.5, 4.0]),
            vec![
                flat_refl_sweep(1, 0.5, 360, 1832, 35.0),
                flat_refl_sweep(2, 4.0, 360, 592, 45.0),
            ],
        );
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

        let short_of = sampler.column(45.0, 200.0);
        assert_eq!(short_of.rungs().len(), 2, "the rung was dropped, not kept");
        assert_eq!(short_of.rungs()[0].sample.status(), SampleStatus::Value);
        assert_eq!(
            short_of.rungs()[1].sample.status(),
            SampleStatus::BeyondRange,
            "the 4° cut stops at 150 km and this column is at 200",
        );

        let (low, high) = short_of.height_span_km().unwrap();
        // Under halfway the surveillance rung carries it; over halfway the
        // absent rung does, and nothing is invented in between.
        let just_above = low + 0.1 * (high - low);
        let just_below = low + 0.9 * (high - low);
        assert_eq!(
            f64::from(short_of.at_height_km(just_above).value().unwrap()),
            round_trip_refl(35.0),
        );
        assert_eq!(
            short_of.at_height_km(just_below).status(),
            SampleStatus::BeyondRange,
            "the missing rung's half of the bracket was filled from the rung \
             below",
        );

        // precondition: the same column inside 150 km has both rungs, so the
        // status above is about range and not about the fixture.
        let inside = sampler.column(45.0, 100.0);
        assert!(inside.rungs().iter().all(|r| r.sample.value().is_some()));
    }

    /// An abandoned tail leaves a hole in azimuth. Painting the nearest
    /// surviving radial across it would draw data where the radar never
    /// looked; the plan view leaves the same hole, and so does this.
    #[test]
    fn an_azimuth_hole_is_reported_rather_than_painted_across() {
        let full_gate = |_: usize| encode_refl(35.0);
        let radial_at = |i: u16, az: f32, spacing: f32| {
            Radial::new(
                0,
                i,
                az,
                spacing,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                Some(moment_from(
                    (0..40).map(full_gate).collect(),
                    REFL_SCALE,
                    REFL_OFFSET,
                )),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        };

        // Radials 0.0 … 199.5 at half-degree spacing: a 160° hole.
        let radials: Vec<Radial> = (0..400)
            .map(|i| radial_at(i, f32::from(i) * 0.5, 0.5))
            .collect();
        let scan = Scan::new(vcp(&[0.5]), vec![Sweep::new(1, radials)]);
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

        let status_at = |az: f64| sampler.column(az, 8.0).rungs()[0].sample.status();
        assert_eq!(status_at(100.0), SampleStatus::Value, "inside the sweep");
        assert_eq!(status_at(199.5), SampleStatus::Value, "the last radial");
        assert_eq!(
            status_at(199.7),
            SampleStatus::Value,
            "inside the last radial's own quarter-degree footprint",
        );
        assert_eq!(
            status_at(200.5),
            SampleStatus::NoCoverage,
            "past the last radial's footprint, in the hole",
        );
        assert_eq!(status_at(280.0), SampleStatus::NoCoverage, "mid hole");
        assert_eq!(
            status_at(359.5),
            SampleStatus::NoCoverage,
            "half a degree short of the first radial, still in the hole",
        );
        // The footprint reaches backwards across 0° as well as forwards, which
        // is the wrap case: 359.9° is 0.1° from the 0.0° radial's centre.
        assert_eq!(
            status_at(359.9),
            SampleStatus::Value,
            "inside the first radial's footprint, reached across the 0° seam",
        );
        assert_eq!(
            status_at(0.1),
            SampleStatus::Value,
            "the first radial's footprint",
        );

        // One dropped radial leaves a gap of the same shape, one step wide.
        let mut radials: Vec<Radial> = (0..720)
            .map(|i| radial_at(i, f32::from(i) * 0.5, 0.5))
            .collect();
        radials.remove(180); // 90.0°
        let scan = Scan::new(vcp(&[0.5]), vec![Sweep::new(1, radials)]);
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        let status_at = |az: f64| sampler.column(az, 8.0).rungs()[0].sample.status();
        assert_eq!(status_at(89.5), SampleStatus::Value);
        assert_eq!(
            status_at(90.0),
            SampleStatus::NoCoverage,
            "the dropped radial",
        );
        assert_eq!(status_at(90.5), SampleStatus::Value);
        assert_eq!(
            status_at(89.7),
            SampleStatus::Value,
            "inside the surviving 89.5° radial's footprint",
        );

        // A full sweep interpolates across every seam, including 359.5 → 0.
        let full = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 720, 40, 35.0)]);
        let full = VolumeSampler::new(&full, RadarProduct::Reflectivity).unwrap();
        for az in [0.0, 0.25, 90.0, 180.3, 359.75, 359.99] {
            assert_eq!(
                full.column(az, 8.0).rungs()[0].sample.status(),
                SampleStatus::Value,
                "a complete sweep reported no coverage at {az}°",
            );
        }
    }

    // ── The product gate and the wire ───────────────────────────────────────

    /// Only the six native moments. The hybrid classification is not a moment
    /// and the integrals have no vertical axis left to cut.
    #[test]
    fn samplable_admits_the_six_native_moments_and_nothing_else() {
        let native = [
            (RadarProduct::Reflectivity, MomentSlot::Reflectivity),
            (RadarProduct::Velocity, MomentSlot::Velocity),
            (RadarProduct::SpectrumWidth, MomentSlot::SpectrumWidth),
            (
                RadarProduct::DifferentialReflectivity,
                MomentSlot::DifferentialReflectivity,
            ),
            (
                RadarProduct::DifferentialPhase,
                MomentSlot::DifferentialPhase,
            ),
            (
                RadarProduct::CorrelationCoefficient,
                MomentSlot::CorrelationCoefficient,
            ),
        ];
        for (product, slot) in native {
            assert_eq!(samplable(product), Some(slot), "{product:?}");
        }

        let refused = [
            RadarProduct::HydrometeorClassification,
            RadarProduct::EchoTops,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::VilDensity,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
            RadarProduct::NormalizedRotation,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::SpecificDifferentialPhase,
            RadarProduct::PrecipitationRate,
        ];
        let scan = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 360, 40, 20.0)]);
        for product in refused {
            assert_eq!(samplable(product), None, "{product:?} was admitted");
            let err =
                VolumeSampler::new(&scan, product).expect_err("a refused product built a sampler");
            let text = err.to_string();
            assert!(
                matches!(err, SamplerError::NotSamplable { .. }) && text.len() > 40,
                "{product:?} was refused without a reason: {text}",
            );
        }
        // precondition: every variant is covered, so a new product cannot be
        // added without a decision about it.
        assert_eq!(
            native.len() + refused.len(),
            17,
            "RadarProduct has gained or lost a variant; decide whether it is \
             samplable rather than letting it fall through",
        );

        // The HHC refusal in particular names what it is, because "not a
        // moment" is the part that surprises people.
        let hhc = VolumeSampler::new(&scan, RadarProduct::HydrometeorClassification)
            .unwrap_err()
            .to_string();
        assert!(hhc.contains("hybrid-scan"), "{hhc}");
    }

    /// The wire codes are stable, total and injective — a section crossing a
    /// message port keeps its statuses instead of arriving as a field of
    /// `NaN`.
    #[test]
    fn every_sample_status_survives_the_wire() {
        let all = [
            SampleStatus::Value,
            SampleStatus::BelowThreshold,
            SampleStatus::RangeFolded,
            SampleStatus::BelowLowestBeam,
            SampleStatus::AboveVolume,
            SampleStatus::BeyondRange,
            SampleStatus::NoCoverage,
        ];
        for (i, s) in all.iter().enumerate() {
            assert_eq!(s.wire_code(), i as u8, "{s:?} moved on the wire");
            assert_eq!(SampleStatus::from_wire_code(s.wire_code()), Some(*s));
        }
        assert_eq!(SampleStatus::from_wire_code(7), None);
        assert_eq!(SampleStatus::from_wire_code(255), None);
        // precondition: the list above is the whole enum, so a new variant
        // fails here rather than travelling as an unknown byte.
        assert_eq!(
            all.len(),
            7,
            "SampleStatus gained a variant without a wire code",
        );
    }

    /// A `Sample` cannot carry a number it does not have, or hide one it does.
    #[test]
    fn a_sample_pairs_its_number_with_its_reason() {
        let found = Sample::found(35.5);
        assert_eq!(found.status(), SampleStatus::Value);
        assert_eq!(found.value(), Some(35.5));
        assert_eq!(found.value_or_nan(), 35.5);

        let missing = Sample::missing(SampleStatus::RangeFolded);
        assert_eq!(missing.status(), SampleStatus::RangeFolded);
        assert_eq!(missing.value(), None);
        assert!(missing.value_or_nan().is_nan());
    }

    /// `sample` is `column().at_height_km()`, over a grid that crosses every
    /// boundary the two share.
    #[test]
    fn the_point_query_is_exactly_the_column_query() {
        let angles = [0.5f64, 1.5, 4.0, 10.0];
        let sweeps: Vec<Sweep> = angles
            .iter()
            .enumerate()
            .map(|(i, &e)| {
                make_sweep(
                    i as u8 + 1,
                    e as f32,
                    360,
                    400,
                    Some(&move |az, slant| (slant < 80.0).then_some(20.0 + 0.1 * az + e * 2.0)),
                    None,
                )
            })
            .collect();
        let scan = Scan::new(vcp(&angles), sweeps);
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

        let mut checked = 0usize;
        let mut statuses = std::collections::HashSet::new();
        for az in [0.0, 37.5, 180.0, 359.9] {
            for ground in [0.0, 5.0, 40.0, 90.0, 250.0] {
                let column = sampler.column(az, ground);
                for h in [-1.0, 0.0, 0.5, 2.0, 6.0, 20.0] {
                    let a = sampler.sample(az, ground, h);
                    let b = column.at_height_km(h);
                    assert_eq!(a, b, "az {az}, ground {ground}, height {h}");
                    statuses.insert(a.status());
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 4 * 5 * 6);
        // precondition: the grid really did cross the boundaries, rather than
        // agreeing trivially on one status everywhere.
        assert!(
            statuses.len() >= 4,
            "the grid only produced {statuses:?}, so the agreement is not \
             saying much",
        );
    }

    /// A negative or non-finite query is answered, not panicked on: a UI can
    /// hand this whatever the pointer was over.
    #[test]
    fn a_nonsensical_query_answers_no_coverage() {
        let scan = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 360, 40, 20.0)]);
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        for (az, ground) in [
            (f64::NAN, 10.0),
            (10.0, f64::NAN),
            (10.0, -5.0),
            (f64::INFINITY, 10.0),
            (10.0, f64::INFINITY),
        ] {
            let column = sampler.column(az, ground);
            assert!(column.rungs().is_empty(), "az {az}, ground {ground}");
            assert_eq!(column.at_height_km(1.0).status(), SampleStatus::NoCoverage);
        }
        // An azimuth outside 0..360 is wrapped rather than refused, because a
        // bearing arrives from arithmetic that can overshoot either way.
        assert_eq!(
            sampler.column(365.0, 8.0).rungs()[0].sample,
            sampler.column(5.0, 8.0).rungs()[0].sample,
        );
        assert_eq!(
            sampler.column(-5.0, 8.0).rungs()[0].sample,
            sampler.column(355.0, 8.0).rungs()[0].sample,
        );
    }

    /// The ladder's shape accessors, which the section's axes are built from.
    #[test]
    fn the_ladder_reports_its_own_shape() {
        let angles = [0.5f64, 0.9, 7.0, 19.5];
        let sweeps: Vec<Sweep> = angles
            .iter()
            .enumerate()
            .map(|(i, &e)| flat_refl_sweep(i as u8 + 1, e as f32, 360, 40, 20.0))
            .collect();
        let scan = Scan::new(vcp(&angles), sweeps);
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        assert_eq!(sampler.product(), RadarProduct::Reflectivity);
        assert_eq!(sampler.tilt_count(), 4);
        let gap = sampler.widest_tilt_gap_deg();
        assert!(
            (gap - 12.5).abs() < 1e-4,
            "the 7.0 → 19.5 gap read {gap}° — this is the number that warns a \
             section is interpolating a smooth layer across nothing",
        );
        let single = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 360, 40, 20.0)]);
        let single = VolumeSampler::new(&single, RadarProduct::Reflectivity).unwrap();
        assert_eq!(single.widest_tilt_gap_deg(), 0.0);
        assert_eq!(single.tilt_count(), 1);
    }

    /// Both interpolation stages refuse to invent a number when a corner did
    /// not measure one, and the heaviest corner decides instead.
    #[test]
    fn a_corner_with_no_value_takes_the_cell_rather_than_being_averaged_in() {
        let v = Sample::found;
        let folded = Sample::missing(SampleStatus::RangeFolded);
        let below = Sample::missing(SampleStatus::BelowThreshold);

        // All values: a true weighted mean.
        assert_eq!(
            blend(Blend::Arithmetic, &[v(10.0), v(20.0)], &[0.25, 0.75]).value(),
            Some(17.5),
        );
        // One corner missing: the heavier corner wins outright, with its own
        // value or its own status.
        assert_eq!(
            blend(Blend::Arithmetic, &[v(10.0), folded], &[0.75, 0.25]),
            v(10.0),
        );
        assert_eq!(
            blend(Blend::Arithmetic, &[v(10.0), folded], &[0.25, 0.75]),
            folded,
        );
        // Ties go to the earliest corner, so the answer does not depend on
        // iteration order.
        assert_eq!(
            blend(Blend::Arithmetic, &[v(10.0), folded], &[0.5, 0.5]),
            v(10.0),
        );
        assert_eq!(
            blend(Blend::Arithmetic, &[folded, v(10.0)], &[0.5, 0.5]),
            folded,
        );
        // Two different reasons stay different rather than merging.
        assert_eq!(
            blend(Blend::Arithmetic, &[below, folded], &[0.4, 0.6]),
            folded,
        );
        assert_eq!(
            blend(Blend::Arithmetic, &[below, folded], &[0.6, 0.4]),
            below,
        );
        // Zero total weight cannot divide, and falls through to the same rule.
        assert_eq!(
            blend(Blend::Arithmetic, &[v(10.0), v(20.0)], &[0.0, 0.0]),
            v(10.0),
        );
        // Degenerate input answers rather than panicking.
        assert_eq!(
            blend(Blend::Arithmetic, &[], &[]).status(),
            SampleStatus::NoCoverage,
        );
    }

    /// The linear-Z question has one answer in this crate, not two.
    #[test]
    fn the_blend_table_agrees_with_the_echo_top_cubes() {
        for product in [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::CorrelationCoefficient,
        ] {
            let wants_linear_z = CellStat::for_moment(product) == CellStat::LinearZMean;
            assert_eq!(
                Blend::for_moment(product) == Blend::LinearZ,
                wants_linear_z,
                "{product:?}: the sampler and CellStat disagree about whether \
                 it averages in linear Z",
            );
        }
        // Differential phase is the one arm `CellStat` does not have, and it
        // must not fall through to the arithmetic mean.
        assert_eq!(
            Blend::for_moment(RadarProduct::DifferentialPhase),
            Blend::Angular360,
        );
        assert_eq!(
            CellStat::for_moment(RadarProduct::DifferentialPhase),
            CellStat::Mean,
            "precondition: CellStat grew an angular arm, so this module should \
             read it instead of overriding it",
        );
    }
}
