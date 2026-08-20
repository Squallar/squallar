//! A native-geometry volume sampler: point queries against the sweeps a
//! `Scan` already holds, with no resampling grid in between.
//!
//! It borrows the [`Scan`] rather than gridding it, because a 15-tilt volume
//! is ~10 M gates and the answer to any one query touches six of them.
//!
//! One rung costs four gate reads — a bilinear in azimuth × slant range — and
//! a whole column is `4·N`, ~64 on a 16-rung VCP 212 ladder. Every height
//! after the first is a two-point lerp between rungs already sampled and
//! reads no gates, so a `W × H` section costs `W·4·N` rather than `W·H·4·N`.
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
//! takes any. The rung's *geometric* elevation is
//! [`crate::volumetric::sweep_elevation_deg`] of the chosen sweep — the
//! nominal cut angle is the **grouping key only**, since measured medians sit
//! up to 0.044° off it. Scored against the VCP's own cut table this is 0
//! violations on all 203 volumes and on a frozen-rule 30-site holdout.
//!
//! **No angular threshold can work.** KBMX (VCP 212, adaptive base tilt)
//! declares genuine cuts at 0.40° and 0.48° — 0.09° apart — while the spread
//! of first-radial angles *within* the 0.48° cut is 0.088° and the gap *to*
//! the 0.40° cut is also 0.088°. At a 0.2° merge threshold the whole 0.48°
//! cut **vanishes**. Thresholds of 0.10/0.15/0.20/0.30 failed 1/2/2/3 of 19
//! detailed volumes and 12 of 124 in the survey.
//!
//! **[`crate::volumetric::VolumeCube`]'s rule must not be copied.** Keying
//! rungs on the median radial angle rounded to 0.1° violates the cut table on
//! all 203 volumes; it is harmless there only because that grid stops at
//! 230 km, and a sampler reaching past 300 km has no such protection.
//!
//! [`crate::hca::merge_split_cut_doppler`] is **not** used to fill a
//! surveillance rung's missing velocity: it clones every radial it merges.
//! [`nexrad_model`]'s `SweepField` is not used either — its elevation key is
//! the *first* radial's angle, its `value_at_polar` is nearest-azimuth with a
//! **floor**ed gate index (a fixed 125 m inward bias), and building one
//! eagerly decodes ~100 MB.
//!
//! Geometry all comes from [`crate::beam`] — 4/3 earth, quadratic height, and
//! for the horizontal the spherical arc and its closed-form inverse. A query
//! names a **ground** range and [`beam::slant_range_for_ground_km`] converts
//! it to the slant range a gate was measured at, the same conversion the plan
//! view uses.
//!
//! [`SampleStatus`] carries the six reasons a sample has no number. There is
//! **no downward or upward extrapolation**: under the lowest rung's beam the
//! answer is [`SampleStatus::BelowLowestBeam`], over the highest
//! [`SampleStatus::AboveVolume`], which is also how the cone of silence
//! reports itself. Expect a bracketing rung with **no data** — every volume
//! has one at 230 km and 300 km, and 8 of 19 measured volumes at 150 km,
//! because the upper cuts stop short of the surveillance half.

use nexrad_model::data::{DataMoment, ElevationCut, MomentData, MomentValue, Radial, Sweep};

use crate::azimuth::{MAX_ADJACENT_GAP_STEPS, median_azimuth_step_deg};
use crate::beam;
use crate::nyquist::Volume;
use crate::types::{MomentSlot, RadarProduct};
use crate::volumetric::{CellStat, sweep_elevation_deg};

/// Why a sample has no number — or, for [`SampleStatus::Value`], that it has
/// one.
///
/// The first two mirror `nexrad_model::data::MomentValue`'s own non-numeric
/// arms; the rest describe where the *query* fell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleStatus {
    /// A measured value.
    Value,
    /// The gate was below the moment's signal threshold (raw code 0) —
    /// distinct from having no gate there at all.
    BelowThreshold,
    /// The gate was range folded (raw code 1): its true range is ambiguous
    /// past the unambiguous range of the cut's PRF.
    RangeFolded,
    /// The query height is under the lowest rung's beam centre over that
    /// ground range. Nothing is filled in.
    BelowLowestBeam,
    /// The query height is over the highest rung's beam centre over that
    /// ground range. Over the site this is the whole cone of silence.
    AboveVolume,
    /// The bracketing rung's gates stop short of that ground range. Ordinary:
    /// upper cuts are range-truncated.
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
    /// Deliberately **not** the Level II raw gate codes: four of these seven
    /// have no raw code at all. New variants append; existing codes never move.
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
    /// build does not know.
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
/// [`SampleStatus::Value`]. The fields are private so the pairing cannot come
/// apart.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    value: f32,
    status: SampleStatus,
}

/// Equality that ignores the number when there is no number to compare.
///
/// **A derived `PartialEq` makes every non-`Value` sample unequal to itself**,
/// because `missing` stores `f32::NAN` and `NaN != NaN`. Values still compare
/// as `f32`, so `found(NAN)` remains unequal to itself.
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
    /// [`crate::render_input::RenderInput`] looks like.
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

    /// A cut angle that is not a number. A `NaN` key would silently fail every
    /// grouping comparison and scatter one cut across several rungs.
    #[error("elevation cut {cut_index} has a non-finite angle ({angle})")]
    NonFiniteCutAngle { cut_index: usize, angle: f64 },

    /// The ladder came out empty: no sweep in the volume carries this moment.
    #[error("no sweep in the volume carries {}", product.name())]
    NoSweepsWithMoment { product: RadarProduct },
}

/// The moment a product samples, or `None` if a section of it is meaningless.
///
/// The six native Level II moments are the whole list. The hybrid hydrometeor
/// classification is a hybrid-*scan* composite with no vertical extent to cut
/// through, and echo tops, VIL, VIL density, POSH and MEHS are column
/// integrals that already collapsed the vertical axis.
///
/// The derivations (NROT, SRV, KDP) are refused here because they are computed
/// per sweep; [`crate::derive::prepare`] computes them into synthetic scans
/// and [`crate::derive::volume_slot`] is the predicate the vertical views
/// gate on.
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
    /// to 180°. Differential phase folds at 360°.
    Angular360,
}

impl Blend {
    /// The blend a moment's physics wants.
    ///
    /// Reads [`CellStat::for_moment`] for the linear-Z question rather than
    /// restating it. The angular arm is this module's own.
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
    /// **Two moments in Level II wrap, and only one at a constant.**
    /// Differential phase wraps at 360°, a property of the quantity, so
    /// [`Blend::Angular360`] can be a blend *arm*. Doppler velocity wraps at
    /// the Nyquist velocity, a property of the *sweep's* PRF that differs from
    /// tilt to tilt inside one volume, so it cannot be an arm.
    ///
    /// Message 31's Radial Data Block carries that number and [`crate::scan`]
    /// reads it on the same walk that builds the `Scan`.
    /// [`crate::nyquist::Volume`] pairs the declared table with the scan, and
    /// [`estimate_fold_limit`] is the fallback for a volume that declared
    /// nothing. Both land in [`Rung::fold_limit_ms`], and which one did is in
    /// [`Rung::fold_limit_declared`].
    ///
    /// Every other moment this sampler serves is monotone over its encoding —
    /// spectrum width in particular is a non-negative spread, so no two of its
    /// gates can sit on opposite sides of a seam.
    fn folds_at_measured_limit(product: RadarProduct) -> bool {
        matches!(product, RadarProduct::Velocity)
    }
}

/// The smallest estimated fold limit that is believed, m/s.
///
/// [`estimate_fold_limit`] reads the largest speed a sweep observed, which is
/// the Nyquist velocity *when the sweep folded at all* and an underestimate
/// when it did not. A sweep that saw nothing faster than a few m/s gives a
/// limit so small that ordinary noise clears it, so below this the guard is
/// switched off. It bounds the **declared** limit too: a velocity-bearing cut
/// declaring under 8 m/s is a mis-decoded field.
///
/// **The margin is thinner than the number looks.** The surveillance half of
/// a WSR-88D split cut declares 8.27–9.68 m/s, because the long PRT that buys
/// it 460 km of unambiguous range costs it exactly that — and over a
/// 208-volume, 17-site corpus *every one* of the 712 sweeps carrying no
/// velocity moment declares inside `[8.32, 9.68]`, against a minimum of
/// 12.37 m/s on every velocity-bearing sweep. Route one of those to a rung
/// and a 16.64 m/s fold interval is laid over a field that never wraps.
///
/// The whole defence is [`Blend::folds_at_measured_limit`]'s restriction to
/// velocity and the per-sweep keying in [`Rung::fold_limit_ms`].
/// `pub(crate)` because `crate::nrot::fold_limit_ms` inherits the same
/// exposure through the same shared floor.
pub(crate) const FOLD_LIMIT_FLOOR_MS: f64 = 8.0;

/// One rung of the tilt ladder: the sweep that won its cut, indexed for random
/// access.
struct Rung<'a> {
    /// The VCP cut angle this rung was grouped by, wrap-corrected. A key, and
    /// never geometry — measured medians sit up to 0.044° off it.
    nominal_deg: f64,
    /// The chosen sweep's median radial elevation: the angle every height in
    /// this rung is computed from.
    elevation_deg: f64,
    radials: &'a [Radial],
    /// `(azimuth, index into radials)`, ascending by azimuth. Built rather
    /// than assumed: a sweep's radials are in *collection* order.
    by_azimuth: Vec<(f32, u32)>,
    /// Median gap between adjacent azimuths, degrees — the scale
    /// [`MAX_ADJACENT_GAP_STEPS`] is measured in.
    az_step_deg: f64,
    /// The speed this rung's sweep folds at, m/s, or `None` when this moment
    /// has no fold seam, the volume declared nothing *and* the sweep never got
    /// near one, or the number found sits under [`FOLD_LIMIT_FLOOR_MS`].
    ///
    /// Per rung, not per volume: the Nyquist velocity follows the cut's PRF —
    /// over ten WSR-88D volumes the Doppler cuts declare 23.84–62.94 m/s,
    /// KFFC's low cuts at 25.65 against its cut 12 at 62.94.
    fold_limit_ms: Option<f64>,
    /// Where [`Self::fold_limit_ms`] came from: `true` for the archive's own
    /// declaration, `false` for [`estimate_fold_limit`]'s reading of the data.
    /// **Nothing in the guard reads this**; it exists so
    /// [`VolumeSampler::describe`] can print the provenance.
    fold_limit_declared: bool,
    /// When the chosen sweep was flown — the earliest collection timestamp on
    /// its radials, milliseconds since the Unix epoch, `0` when it carries
    /// none. A volume is flown one tilt at a time over four to ten minutes, so
    /// the bottom and top of one cross-section are minutes apart, and a SAILS
    /// repeat can leave one rung newer than both its neighbours.
    collected_ms: i64,
}

/// The tilt ladder over one ground point: every rung's beam height there and
/// what it measured, ascending by height.
///
/// A rung with no data at this ground range stays in the ladder carrying its
/// status — dropping it would silently widen the bracket and interpolate
/// straight across a tilt that measured nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Column {
    azimuth_deg: f64,
    ground_range_km: f64,
    blend: Blend,
    rungs: Vec<ColumnRung>,
}

/// One rung's contribution to a [`Column`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnRung {
    /// Beam-centre height above the antenna, km, over this column's ground
    /// range.
    pub height_km: f64,
    pub elevation_deg: f64,
    pub sample: Sample,
    /// This rung's fold limit, carried from [`Rung::fold_limit_ms`] so
    /// [`Column::at_height_km`] can refuse to lerp across a Nyquist seam.
    /// Private, unlike its neighbours, because publishing it would invite a
    /// caller to compare two rungs' limits and conclude something about the air.
    fold_limit_ms: Option<f64>,
}

impl Column {
    /// An empty column, which answers [`SampleStatus::NoCoverage`] at every
    /// height.
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
    pub fn height_span_km(&self) -> Option<(f64, f64)> {
        Some((self.rungs.first()?.height_km, self.rungs.last()?.height_km))
    }

    /// What the volume holds at `height_km` above the antenna in this column.
    ///
    /// Interpolates between the two rungs that bracket the height. Outside the
    /// ladder nothing is filled in.
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
        // **Unreachable given finite rung heights, and kept anyway.** A `NaN`
        // rung height sorts last under `total_cmp`, so it *can* become the
        // upper bracket; a panic would turn a benign degradation into a dead
        // frame.
        let t = if span > 0.0 {
            ((height_km - lo.height_km) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // **The seam between two rungs is where a fold does the most damage.**
        // A two-corner lerp at `t = 0.5` of `+v` and `−v` is identically zero,
        // where the four-corner bilinear at least needs its other two corners
        // to agree. Measured over fourteen volumes: of 12,918 rung pairs an
        // independent continuity oracle confirms as folds, 12,903 — 99.9% —
        // average to less than a quarter of the sweep's Nyquist velocity.
        //
        // This pair spans *tilts*, so the guard's line sits at
        // `SEAM_PROXIMITY_ACROSS_TILTS` — lower than the bilinear's, because
        // across hundreds of metres of depth a real fold's ends stray further
        // from the seam.
        let fold_limit = match (lo.fold_limit_ms, hi.fold_limit_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            // This one-sided arm can fire only for armed limits in
            // [8.0, 16.0): the pair must clear `SEAM_PROXIMITY_ACROSS_TILTS`
            // of the measured limit on both ends while the unarmed rung's
            // speeds stay under the 8.0 m/s floor.
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
/// One resolved rung: the wrap-corrected cut key and the index — into the sweep
/// list handed to [`resolve_ladder`] — of the sweep this moment takes for it.
pub(crate) struct LadderChoice {
    pub(crate) key: f64,
    pub(crate) chosen: usize,
}

/// Steps 1–3 of the tilt ladder: key every sweep on its VCP cut, group by exact
/// key, choose one sweep per group for `slot`.
///
/// Factored out of [`VolumeSampler::build`] so that [`ladder_fingerprint`] runs
/// the *same* choice the sampler will make rather than a restatement of it.
///
/// Takes `&[&Sweep]` rather than `&Scan` because the current merged volume
/// ([`crate::current`]) composes sweeps from two volumes. Group order is
/// discovery order; member order inside a group is input order, which is what
/// "newest" means below.
pub(crate) fn resolve_ladder(
    cuts: &[ElevationCut],
    sweeps: &[&Sweep],
    slot: MomentSlot,
) -> Result<Vec<LadderChoice>, SamplerError> {
    // Step 1 and 2: key every sweep on its cut, then group by exact key,
    // preserving input order inside each group.
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
        // Newest-first: the last cut of a SAILS repeat is the current one.
        let chosen = if doppler {
            members.iter().rev().find(|i| carries(i))
        } else {
            // A split cut's Doppler half repeats a short-range copy of the
            // surveillance moments; reflectivity belongs to the surveillance
            // half, which reaches 460 km against the Doppler half's 300.
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

        // `partition_point` counts the rungs at or below the query, so 0 means
        // the query is under the lowest beam and `len` means it is at or over
        // the highest.
        //
        // The `span > 0.0` arm below is unreachable given finite rung heights
        // and kept anyway: a `NaN` rung height sorts last under `total_cmp`,
        // so it *can* become the upper bracket, and a panic would turn a
        // benign degradation into a dead frame.
        //
        // **The seam between two rungs is where a fold does the most damage.**
        // A two-corner lerp at `t = 0.5` of `+v` and `−v` is identically zero,
        // where the four-corner bilinear at least needs its other two corners
        // to agree. Measured over fourteen volumes: of 12,918 rung pairs an
        // independent continuity oracle confirms as folds, 12,903 average to
        // less than a quarter of the sweep's Nyquist velocity.
        //
        // The smaller of the two limits governs, and the guard's line sits at
        // `SEAM_PROXIMITY_ACROSS_TILTS` — lower than the bilinear's, because
        // across hundreds of metres of depth a real fold's ends stray further
        // from the seam.
pub fn ladder_fingerprint(
    pattern: &nexrad_model::data::VolumeCoveragePattern,
    sweeps: &[&Sweep],
    product: RadarProduct,
) -> Option<u64> {
    use std::hash::{Hash, Hasher};

    let slot = crate::derive::volume_slot(product)?;
    let cuts = pattern.elevation_cuts();
    if cuts.is_empty() {
        return None;
    }
    let mut choices = resolve_ladder(cuts, sweeps, slot).ok()?;
    if choices.is_empty() {
        return None;
    }
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
    /// which is not the highest rung the ladder *has*.
    top_declared_cut_deg: f64,
}

impl<'a> VolumeSampler<'a> {
    /// Resolve `volume`'s tilt ladder for `product`.
    ///
    /// Fails rather than degrades. Every error is also logged, so a caller
    /// that discards the `Result` with `.ok()` still leaves the reason
    /// somewhere.
    ///
    /// `volume` is a [`crate::nyquist::Volume`], which a bare `&Scan` converts
    /// into: that conversion declares no Nyquist velocities, so a velocity
    /// ladder built from one estimates every rung's fold limit off the data.
    pub fn new(volume: impl Into<Volume<'a>>, product: RadarProduct) -> Result<Self, SamplerError> {
        Self::build(volume.into(), product).inspect_err(|e| {
            log::warn!("volume sampler unavailable for {}: {e}", product.code());
        })
    }

    /// Resolve a **derived** scan's ladder for `product`, reading `slot`.
    ///
    /// The bypass [`crate::derive`] needs and nothing else may use:
    /// [`samplable`] refuses the derived products precisely so a raw volume
    /// can never be sampled under a derived label.
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
        // same way. Non-finite entries are skipped rather than refused: they
        // may never be referenced by a sweep.
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
            // Declared wins outright: it is the RDA's statement of the waveform
            // it flew, while the estimate is exact only for a sweep that
            // actually folded and an **under**estimate for one that did not.
            // `FOLD_LIMIT_FLOOR_MS` applies to whichever answer is used: a
            // declared value under the floor is a corrupt field, so it is
            // dropped and the rung falls through to the estimator.
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
                // The same reading of the same radials `RenderInput` makes
                // when it flattens a sweep for the port, shared rather than
                // restated.
                collected_ms: crate::render_input::sweep_collected_ms(radials),
            });
        }

        if rungs.is_empty() {
            return Err(SamplerError::NoSweepsWithMoment { product });
        }
        rungs.sort_by(|a, b| a.nominal_deg.total_cmp(&b.nominal_deg));

        // Every rung came from a cut whose angle was checked finite above, so a
        // ladder with rungs always has a finite top; the fold's seed survives
        // only a table of nothing but non-finite angles.
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

/// The identity of the sweeps the ladder would choose for `product` — the
/// re-cut key for anything that draws from a whole volume.
///
/// Two volumes fingerprint equal exactly when, for this moment, every rung
/// would be cut from the same measured data under the same declared pattern.
/// Hashed: the **declared cut table**, and per chosen sweep the **rung key**,
/// **elevation number**, **radial count**, first and last radials'
/// **collection timestamps**, and the first radial's **gate count**. A sealed
/// sweep is immutable, so this tuple names one sweep's data uniquely.
/// [`std::hash::DefaultHasher`] is stable within a process, which is the only
/// place the key is compared; it must never be persisted.
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
    pub fn tilt_count(&self) -> usize {
        self.rungs.len()
    }

    /// Each rung's geometric elevation, **in cut order** — which is ascending
    /// by the nominal key, not by this number. A chosen sweep's median can in
    /// principle sit outside its cut's place in that order; measured never to,
    /// in 4 756 ordered pairs. A caller who wants heights sorted wants
    /// [`Column::rungs`].
    pub fn elevations_deg(&self) -> impl Iterator<Item = f64> + '_ {
        self.rungs.iter().map(|r| r.elevation_deg)
    }

    /// Each rung's VCP cut angle, ascending — the grouping key, not geometry.
    /// Exposed so a caller can show which declared cuts a volume delivered.
    pub fn nominal_elevations_deg(&self) -> impl Iterator<Item = f64> + '_ {
        self.rungs.iter().map(|r| r.nominal_deg)
    }

    /// When each rung's chosen sweep was flown, milliseconds since the Unix
    /// epoch, **in the same cut order** [`elevations_deg`](Self::elevations_deg)
    /// reports. `0` for a rung whose sweep carried no clock at all.
    pub fn collection_times_ms(&self) -> impl Iterator<Item = i64> + '_ {
        self.rungs.iter().map(|r| r.collected_ms)
    }

    /// The highest cut angle **this ladder has**, degrees — the top rung's
    /// grouping key, or `0.0` for a ladder with no rungs.
    pub fn top_tilt_deg(&self) -> f64 {
        self.rungs.last().map_or(0.0, |rung| rung.nominal_deg)
    }

    /// The highest cut angle the coverage pattern **declares**, degrees.
    ///
    /// Read against [`top_tilt_deg`](Self::top_tilt_deg) it answers the one
    /// question a consumer of a short ladder cannot otherwise ask: *did the
    /// volume stop early, or is this all there is?* The count is deliberately
    /// *not* the comparison — a pattern declares more cut-table entries than
    /// distinct angles, and the surveillance-only entries carry no Doppler
    /// moment — but every operational pattern's highest cut carries every one.
    pub fn top_declared_cut_deg(&self) -> f64 {
        self.top_declared_cut_deg
    }

    /// The largest angular step between adjacent rungs, degrees. `0.0` for a
    /// single-rung ladder. Measured over the elevations **sorted**: folding
    /// signed differences down cut order would report `0.0` for a ladder whose
    /// medians invert.
    pub fn widest_tilt_gap_deg(&self) -> f64 {
        let mut sorted: Vec<f64> = self.elevations_deg().collect();
        sorted.sort_by(f64::total_cmp);
        sorted
            .windows(2)
            .map(|w| w[1] - w[0])
            .fold(0.0f64, f64::max)
    }

    /// The tilt ladder over one ground point, allocating a fresh [`Column`].
    /// `ground_range_km` is a **ground** range, so a caller holding a slant
    /// range wants [`beam::ground_range_km`] first.
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
        // Ascending by height. The rungs are already ascending by cut angle and
        // `height_at_ground_km` is strictly increasing in elevation, so this
        // reorders nothing unless a chosen sweep's median inverted its cut's
        // order — measured never to happen in 4 756 ordered pairs.
        out.rungs
            .sort_by(|a, b| a.height_km.total_cmp(&b.height_km));
    }

    /// What the volume holds at one point, in radar-relative coordinates.
    ///
    /// Builds the whole column and asks it, so it costs `4·N` gate reads rather
    /// than the eight a bracketing pair would need. That buys **one**
    /// interpolation path: sampling only the bracketing pair means finding the
    /// bracket a second way, and two ways of choosing a bracket is the
    /// split-key hazard this module's ladder rule exists to close.
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
        // The same arc the plan view paints with, run backwards: a caller
        // names ground, a gate is indexed by beam.
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
        // These corners span *gates* (and radials) of one sweep, so the guard's
        // line sits at `SEAM_PROXIMITY_ACROSS_GATES`.
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

    // The declared angles again, not `by_azimuth`'s wrapped copies: the helper
    // performs the same wrap itself, and feeding it values already through the
    // `f32` round-trip would run the quantization twice.
    //
    // A sweep with no two distinct azimuths has no observable step. One degree
    // is the coarsest spacing a WSR-88D produces, so it serves half a degree
    // either side and no more.
    let az_step_deg =
        median_azimuth_step_deg(radials.iter().map(|r| f64::from(r.azimuth_angle_degrees())))
            .unwrap_or(1.0);
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
        // One radial, or a duplicated azimuth: nothing to interpolate with, so
        // it serves its own footprint alone.
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
/// centres is `(slant − first) / interval` with no half-gate subtracted.
fn gate_bracket(moment: &MomentData, slant_km: f64) -> (Sample, Sample, f64) {
    // A zero gate interval is not guarded separately: `gate_interval_km` is a
    // `u16` of metres so it cannot be negative, and dividing by zero lands on
    // an infinity or a `NaN` the finiteness test already refuses.
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

    /// The ladder, as one line: per rung, `nominal->median radials×gates`.
    ///
    /// Hand-written rather than derived because a derived `Debug` would walk
    /// the borrowed radials and print the whole ~10 M-gate volume.
    ///
    /// **The radial and gate counts are the load-bearing part**: on a real
    /// split cut the two halves share a cut angle *and* a median — 0.4834° for
    /// both on a measured KMPX VCP 212 volume — so only range separates them,
    /// 1832 reflectivity gates against 1192. A rung guarding velocity appends
    /// `±<limit>d` or `±<limit>e` for declared or estimated; the letter is
    /// what makes a lost declared table visible before the numbers differ.
fn gate_sample(moment: &MomentData, gate: usize) -> Sample {
    let Some(value) = crate::render::moment_value_at(moment, gate) else {
        return Sample::missing(SampleStatus::BeyondRange);
    };
    match value {
        MomentValue::Value(v) => Sample::found(v),
        MomentValue::BelowThreshold => Sample::missing(SampleStatus::BelowThreshold),
        MomentValue::RangeFolded => Sample::missing(SampleStatus::RangeFolded),
    }
}

/// The speed one sweep folds at, m/s, read off the sweep itself.
///
/// A folded field always contains gates *at* the fold limit, so when a sweep
/// aliased at all the largest speed it reports **is** its Nyquist velocity.
/// Scored against the archive's own `nyquist_velocity` over 140 rungs of
/// fourteen volumes at six sites, the ratio of this estimate to the declared
/// number runs 0.889–1.016, median 0.992. When a sweep did *not* alias this is
/// an underestimate, which makes [`straddles_fold`] fire more readily than it
/// should; [`FOLD_LIMIT_FLOOR_MS`] closes the case where it stops meaning
/// anything.
///
/// `crate::nrot`'s `estimate_nyquist` is the same measurement on the same
/// reasoning, but **the two uses have opposite sensitivity to the same
/// error**: `nrot` scales a threshold by its estimate, so a low estimate is
/// conservative there, while this sampler uses it as an exact classification
/// boundary, where a low estimate manufactures false positives.
///
/// Only the extreme raw words are converted, because the encoding is affine.
/// Both ends are converted because a negative `scale` swaps which is which.
/// The `raw >= 2` filter is [`gate_sample`]'s status codes, and the
/// `scale == 0.0` skip is its "the raw words *are* the values" arm.
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
/// The criterion is marginal: each step up stops the guard firing on one band
/// of pairs holding both confirmed folds (given up) and confirmed shear
/// (won), and the fraction stops earning where that ratio crosses 1. Over an
/// arbitration corpus of 56 VCP 31 volumes at 22 sites — VCP 31 being the
/// only operational pattern that puts the seam at 11–12.5 m/s — the quad
/// bands cross between 0.55 and 0.65, so `0.60`.
///
/// Against *labelled* truth the crossing moves with the fold base rate (64%
/// of quad candidates → 0.25–0.30; 26.7% → 0.45–0.50; 12% → 0.45–0.50), which
/// is arithmetic rather than weather and is pinned in this tree by
/// `the_break_even_moves_up_as_folds_get_rarer`. So `0.60` sits one to three
/// grid steps above the labelled evidence, costing 90.6% / 78.4% quad recall
/// against 93.5% / 85.1% at `0.50`, for a false-fire rate of 0.544% / 0.863%
/// against 1.085% / 1.811%. These percentages are corpus properties.
const SEAM_PROXIMITY_ACROSS_GATES: f64 = 0.60;

/// How near the seam both rungs must sit before a straddle between adjacent
/// *tilts* — the pair [`Column::at_height_km`] lerps between — is read as a
/// fold, as a fraction of the fold limit.
///
/// **Why this is below [`SEAM_PROXIMITY_ACROSS_GATES`].** [`straddles_fold`]'s
/// argument — one wrap of a smooth field leaves *both* sides near `±limit` —
/// assumes the pair's own true change is small next to the Nyquist interval.
/// Between adjacent gates of one sweep that holds. Between adjacent tilts the
/// rungs sit hundreds of metres apart, so a genuine fold often presents with
/// one end deep inside the range, which the rule reads as shear.
///
/// **`0.50` is a floor, not an argmin.** Mean error falls monotonically as the
/// fraction falls, which would argue for `0.25`, but the error curve moves
/// with the fold base rate and so cannot arbitrate; and below `½` the rule
/// stops being strictly stronger than the one it replaced —
/// [`straddles_fold`] fires on `hi − lo > 2f·limit`, which at `f ≥ ½` clears
/// half a Nyquist period, and the `const` assertion in
/// `each_guard_draws_its_line_at_its_own_fraction` refuses to *build* a
/// fraction under `½`.
///
/// Scored by `seam_truth` against labelled truth at a 11.50 m/s synthetic
/// seam over two site-disjoint corpora (47.9% and 63.8% fold base rates):
///
/// | f | arb recall | arb false-fire | holdout recall | holdout false-fire |
/// |---|---|---|---|---|
/// | 0.35 | 77.5% | 10.33% | 76.7% | 12.25% |
/// | **0.50** | **65.0%** | **5.26%** | **70.2%** | **7.70%** |
/// | 0.60 | 56.0% | 2.75% | 65.6% | 5.67% |
/// | 0.67 | 49.8% | 1.60% | 61.8% | 4.52% |
///
/// Missing a vertical fold costs roughly twice what firing on one costs.
/// **65.0–70.2% is a property of a corpus, not of this constant**: a third,
/// site- and date-disjoint corpus replicates substantially but not exactly,
/// and its 10.9%-base-rate half puts the break-even on the shipped fraction.
///
/// **Where this guard fails.** Recall by mid-beam height runs 37.2 / 62.9 /
/// 80.0 / 73.5 / 46.8 / 39.9 / 27.3% across 0–1, 1–2, 2–4, 4–6, 6–8, 8–11 and
/// 11+ km, while by slant range it is a 19-point spread with no trend:
/// **height is the organising axis; range is not**. At `f = 0.50` against an
/// 11.50 m/s seam the line sits at 5.75 m/s, and the mean *true* change
/// between adjacent rungs reaches 5.39 m/s in the 6–8 km stratum — the
/// premise failing, which
/// `recall_falls_away_as_the_true_change_reaches_the_guards_own_line` pins.
/// Bringing the line in catches the real shear with it.
///
/// **This guard and the dealiaser fail together without either feeding the
/// other**: `crate::nrot::dealias` takes a bare velocity grid and returns no
/// [`Scan`], the only scans carrying post-dealias content are
/// `crate::derive`'s synthetic ones and none is `RadarProduct::Velocity`, and
/// [`Blend::folds_at_measured_limit`] arms this guard for
/// `RadarProduct::Velocity` alone. Above 6–8 km the real velocity change
/// between adjacent measurement heights becomes comparable to the Nyquist
/// interval, and every method assuming vertical continuity loses its footing.
const SEAM_PROXIMITY_ACROSS_TILTS: f64 = 0.50;

/// Which adjacency a velocity blend spans, carrying the fold limit its guard
/// tests against.
///
/// This is how the two seam-proximity constants stay with their own paths: the
/// fraction cannot be passed at all. A bare fraction parameter would have
/// compiled with the two values swapped and said nothing.
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
/// **A fold wraps a continuous field across `±limit`, so both sides of the
/// discontinuity it leaves are *near* `±limit`.** For a field passing smoothly
/// through the seam the true speeds are `limit − a` and `limit + b` for small
/// `a`, `b`, and what gets reported is `limit − a` and `−(limit − b)`. So a
/// straddle whose *smaller* extreme sits well inside the range cannot be one
/// fold of a smooth field — it is real shear.
///
/// The rule is `lo < −f·limit && hi > f·limit`, where `seam` says what the
/// corners are adjacent across and `f` follows from that. Opposite signs and
/// `hi − lo > 2f·limit` both fall out of it, and at `f ≥ ½` that clears a
/// whole period — so this is strictly stronger than the
/// sign-change-and-spread rule it replaced.
///
/// Only the extreme pair is tested, which is exhaustive rather than a
/// shortcut: if any pair among the corners straddles, the widest pair's ends
/// are at least as far either side of zero.
///
/// The fractions do **not** empty the disputed population: on a low-Nyquist
/// clear-air volume ordinary boundary-layer shear is comparable to the seam
/// itself. The spread statistic is sharply bimodal only on volumes that fold
/// hard; on a clear-air volume the histogram is broad and flat with no valley
/// at all, so a threshold cannot be placed "in the valley".
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
/// did not, the answer is the corner carrying the most weight, verbatim —
/// value and status both. At an echo edge, blending a number towards "below
/// threshold" would require inventing a number for it; taking the heaviest
/// corner puts the boundary at the half-weight point. A range-folded gate
/// stays range folded over its own half of the interval instead of being
/// averaged out of existence.
///
/// `seam` extends that to corners that all *did* measure something: +24.50 and
/// −24.50 m/s average to exactly 0.000, the display's word for "no motion"
/// written over its word for "as fast as this radar can report". No weighted
/// mean of two points on opposite sides of a discontinuity lands near either,
/// so a straddle falls through to the same heaviest-corner answer.
///
/// **Heaviest means the largest bilinear weight — the *nearest* sample — not
/// the largest magnitude.** Picking the fastest corner would bias every fold
/// edge outward and turn this into a peak-hold. Ties go to the earliest
/// corner, so the result does not depend on iteration order.
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

#[cfg(test)]
mod seam_fixture;
