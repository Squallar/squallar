//! Volume-derived products computed from the Level II volume.
//!
//! The heart is [`VolumeCube`]: the whole volume collapsed once per scan onto
//! a 360° × 230 km polar grid per tilt, for whatever moments a product needs,
//! with beam geometry and sweep provenance alongside. Products
//! ([`compute_echo_tops`], and the EET/DVL/KDP/HCA family to come) are then
//! column scans over the cube rather than owners of their own gridding.
//!
//! The interpolated echo tops here interpolate between tilt centres. What they
//! were calibrated against, and how far that goes, is written out at
//! [`compute_echo_tops`] — the short of it is **GR2Analyst's** own Echo Tops
//! and not the RPG's product 135, which [`crate::eet`] reproduces separately.
//!
//! # Two kinds of consumer, and [`RangeBinning`] is the seam
//!
//! A gate is measured out along the beam. Which 1-km bin it belongs in depends
//! on what the product asking is *for*, and the two answers are not
//! reconcilable:
//!
//! * [`compute_echo_tops`] and [`crate::hail`] are this display's own
//!   products. They stack tilts into a column and ask what is above a place,
//!   so a gate belongs over the ground it sits over — [`RangeBinning::Ground`].
//!   At 19.5° that moves it 5.7 % inward, four bins at 70 km.
//! * [`crate::eet`] and [`crate::vil`] exist to **reproduce** the RPG's own
//!   EET and DVL products bin for bin, and the RPG bins slant. They take
//!   [`RangeBinning::Slant`], and correcting them would not make them righter
//!   — it would make the twin comparison that validates them measure
//!   registration instead of physics.
//!
//! The heights travel with the bins ([`BeamHeights::at_elevation`] takes the
//! same parameter), because a cell filed under a ground range whose altitude
//! came from a slant range describes two different points in the air.
//!
//! The seam has a second edge that is easy to miss: some volumes deliver
//! content **coarser than the grid they declare**, and whether that costs
//! anything depends on which side of the seam you are on. See [`GateFiling`].

use crate::par::*;
use crate::types::RadarProduct;
use nexrad_model::data::{DataMoment, MomentValue, Radial, Scan};

/// Half-power beamwidth of the WSR-88D antenna, degrees. Beam bottom and top
/// heights sit half of this below and above the tilt centre.
///
/// Re-exported from [`crate::beam`], which owns the crate's beam geometry.
/// The WSR-88D's specifically, and named so: a TDWR's antenna is 0.55°
/// ([`crate::beam::TDWR_HALF_POWER_BEAMWIDTH_DEG`]), and a caller whose
/// answer depends on which network it is looking at wants
/// [`crate::beam::half_power_beamwidth_deg_near`] rather than this.
pub const WSR88D_HALF_POWER_BEAMWIDTH_DEG: f64 = crate::beam::WSR88D_HALF_POWER_BEAMWIDTH_DEG;

/// Reflectivity threshold for echo tops, dBZ.
const ET_THRESHOLD_DBZ: f32 = 18.3;

/// Range cells of the cube and of every volumetric product: 1 km each, 230 km
/// total — the domain the RPG specifies its derived products over.
pub const RANGE_BINS: usize = 230;

/// How wide one of those cells is, km.
///
/// The `1` in [`RangeBinning`]'s `[r, r+1)`, given a name because the raster
/// that draws these grids has to be told how far apart their samples are:
/// [`crate::types::data_limited_side_px`] will not paint more texels across a
/// field than its samples can fill, and a grid whose spacing it could not read
/// would be sized as though it had said nothing about its own sampling.
pub const RANGE_BIN_KM: f64 = 1.0;

/// Polar grid of a volume-derived product: 360 azimuth degrees × 1-km range
/// bins, value `NaN` where undefined.
pub struct VolumetricGrid {
    pub values: Vec<Vec<f32>>, // [az_deg][range_km]
    pub range_bins: usize,
}

/// Beam-center height above the radar, km, for a slant range and elevation.
/// `pub(crate)` for [`crate::hail`], whose column geometry has to sit in the
/// same 4/3-model vertical coordinate the cube's [`BeamHeights`] use.
///
/// The arithmetic moved to [`crate::beam::height_km`] — the crate's one home
/// for beam geometry — bit for bit; this name stays so the cube's own call
/// sites read in the cube's vocabulary. Every pinned echo-tops digest is a
/// test of that identity, and `beam::tests::
/// the_lifted_beam_height_is_bit_identical_to_the_one_volumetric_shipped`
/// is the local one.
pub(crate) fn beam_height_km(range_km: f64, elev_deg: f64) -> f64 {
    crate::beam::height_km(range_km, elev_deg)
}

/// A sweep's elevation angle: the **median** of its radials' instantaneous
/// angles. `None` for an empty sweep.
///
/// Not the first radial's: the antenna can still be settling onto the cut
/// when the sweep starts, and the error is not small — a live KMRX volume's
/// 0.5° cut opened at 0.283° and its 19.5° cut at 19.297°. Keying tilts on
/// the first radial split SAILS revisits into phantom tilts (and collided
/// them with neighbouring cuts), and any height ladder built from it sat a
/// fifth of a degree low.
pub fn sweep_elevation_deg(radials: &[Radial]) -> Option<f64> {
    if radials.is_empty() {
        return None;
    }
    let mut els: Vec<f32> = radials
        .iter()
        .map(|r| r.elevation_angle_degrees())
        .collect();
    els.sort_by(f32::total_cmp);
    Some(f64::from(els[els.len() / 2]))
}

/// The statistic collapsing a radial's gates into a 1-km cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellStat {
    /// Mean in linear Z (`10^(dBZ/10)`), read back in dBZ. Averaging
    /// reflectivity in dB space would understate every mixed cell.
    LinearZMean,
    /// Arithmetic mean of the physical values.
    Mean,
    /// Largest value in the cell.
    Max,
}

impl CellStat {
    /// The statistic a moment's physics wants: linear-Z mean for reflectivity
    /// (and the products that read it), arithmetic mean for everything else.
    pub fn for_moment(moment: RadarProduct) -> Self {
        match moment {
            RadarProduct::Reflectivity | RadarProduct::EchoTopsInterpolated => Self::LinearZMean,
            _ => Self::Mean,
        }
    }
}

/// How a repeated elevation (a SAILS/MRLE revisit of the lowest cuts) is
/// resolved to one sweep per tilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupPolicy {
    /// The latest sweep at an elevation wins — the freshest look, what the
    /// shipped interpolated echo tops have always done.
    NewestWins,
    /// The first sweep of the volume wins — the coherent snapshot the RPG's
    /// own volume products are computed from, which the validation harnesses
    /// need when comparing against an EET/DVL twin.
    FirstOfVolume,
}

/// Which range a cube's 1-km bins are indexed by: the slant range a gate was
/// measured at, or the ground range under it.
///
/// The two answer different questions, and both are needed here. A product
/// that says *where something is* wants the ground — a 19.5° gate at 60 km
/// slant sits over 56.5 km of ground, and an echo-top column that binned it at
/// 60 would be stacking it over a different column's reflectivity. A product
/// built to **reproduce the RPG's** wants whatever the RPG did, which is
/// slant: its derived products index by the gate's own range bin, and a twin
/// scored bin-for-bin against one has to make the same choice or the score
/// measures the registration rather than the physics.
///
/// So this is not a correctness switch with a right setting. It is a statement
/// about which grid the caller is trying to be, and the call sites divide
/// exactly on that line — [`compute_echo_tops`] and [`crate::hail`] take
/// [`Ground`](Self::Ground), [`crate::eet`] and [`crate::vil`] take
/// [`Slant`](Self::Slant).
///
/// The heights go with the bins. [`BeamHeights::at_elevation`] takes this too,
/// because a cell indexed by ground range whose height was computed from a
/// slant range is a cell whose two coordinates describe two different points
/// in the air.
///
/// # The two coordinates are the same ground range again
///
/// They briefly were not. When [`crate::beam::ground_range_km`] became the
/// spherical arc, this cube went on indexing with a per-sweep tangent-plane
/// `r·cos e` scalar while [`crate::beam::height_at_ground_km`] inverted the
/// arc — so a [`Ground`](Self::Ground) cell's height was the height at the
/// arc-inverse of a tangent-plane pseudo-range, which is not the height of the
/// gate that landed in it. Over this cube's own domain (230 bins × the VCP 212
/// ladder) that was at most 50.3 m below 20 km of altitude, a fifth of a gate.
///
/// [`range_of`](Self::range_of) now files each gate through the same arc, so
/// the two coordinates describe one point again and the statement above is
/// exact rather than bounded. The scalar could not simply be replaced by a
/// factor — an arc is not one — so it became a per-gate call, and the cost of
/// that was measured before it was accepted: the same substitution in the far
/// larger raster loop is +3.1 % in Chrome and nothing measurable in Firefox, on
/// a path that runs once per sweep and never per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeBinning {
    /// Bin `r` holds the gates whose **slant** range falls in `[r, r+1)` km.
    Slant,
    /// Bin `r` holds the gates whose **ground** range — the spherical arc
    /// beneath the beam, at the sweep's median elevation — falls in
    /// `[r, r+1)` km.
    Ground,
}

impl RangeBinning {
    /// The factor a sweep's slant ranges are multiplied by to index this
    /// binning: 1 for [`Slant`](Self::Slant), `cos e` of the sweep's median
    /// elevation for [`Ground`](Self::Ground).
    ///
    /// The median for the same reason [`sweep_elevation_deg`] exists: a
    /// settling first radial would rebin a whole sweep by a fifth of a degree.
    /// An empty or non-finite sweep falls back to 1, which bins it where it
    /// was measured rather than collapsing it onto the site.
    fn binning_elevation_deg(self, radials: &[Radial]) -> Option<f64> {
        match (self, sweep_elevation_deg(radials)) {
            (Self::Ground, Some(e)) if e.is_finite() => Some(e),
            _ => None,
        }
    }

    /// The range this binning files a gate measured at `slant_km` under.
    ///
    /// [`Slant`](Self::Slant) files it where it was measured.
    /// [`Ground`](Self::Ground) files it under the ground arc beneath it,
    /// through [`crate::beam::ground_range_km`] — the same conversion
    /// [`crate::beam::height_at_ground_km`] inverts, so a cell's range and its
    /// height finally describe the same point in the air.
    ///
    /// This was a per-sweep `cos e` scalar hoisted out of the gate loop. It had
    /// to become a per-gate call because the arc is not a scale factor, and the
    /// cost of that was measured before it was accepted rather than assumed:
    /// see the branch's render measurements, where the same substitution in the
    /// far larger raster loop cost +3.1 % in Chrome and nothing measurable in
    /// Firefox, on a path that runs once per sweep and never per frame.
    fn range_of(elevation_deg: Option<f64>, slant_km: f64) -> f64 {
        match elevation_deg {
            Some(e) => crate::beam::ground_range_km(slant_km, e),
            None => slant_km,
        }
    }

    /// Beam-centre height, km, over a cell of this binning at `range_km`.
    ///
    /// The pair to [`range_of`](Self::range_of), and the reason both
    /// live on the enum: a cube whose bins moved but whose heights did not
    /// would put a gate's reflectivity at one point and its altitude at
    /// another, which is a silent error in every column scan above it.
    fn height_km(self, range_km: f64, elev_deg: f64) -> f64 {
        match self {
            Self::Slant => beam_height_km(range_km, elev_deg),
            Self::Ground => crate::beam::height_at_ground_km(range_km, elev_deg),
        }
    }

    /// How this binning reads a sweep whose content may be coarser than the
    /// grid it declares — see [`GateFiling`] for the property and
    /// [`replicated_pairs`] for how it is recognised.
    ///
    /// **[`Slant`](Self::Slant) never decimates, and that is deliberate.**
    /// Under slant binning the replication costs nothing: a 1 km bin holds the
    /// declared gates `4r-8 ..= 4r-5`, the start index is always even, and two
    /// whole replicated pairs land in every bin — so each 500 m sample is
    /// weighted exactly once whichever way it is read. What decimating *would*
    /// do there is reassociate the sums by which [`crate::eet`] and
    /// [`crate::vil`] reproduce the RPG's own products bin for bin, moving a
    /// twin comparison that exists to measure physics for no gain in the
    /// physics. [`Ground`](Self::Ground) is where the alignment breaks, so
    /// [`Ground`](Self::Ground) is where this looks.
    fn gate_filing(self, radials: &[Radial], moment: RadarProduct) -> GateFiling {
        match self {
            Self::Slant => GateFiling::AsDeclared,
            Self::Ground if replicated_pairs(radials, moment) => GateFiling::ReplicatedPairs,
            Self::Ground => GateFiling::AsDeclared,
        }
    }
}

/// What one declared gate is, when a sweep's content is coarser than the grid
/// it declares.
///
/// # The property
///
/// A **long-pulse** volume (`pulse_width = 4`; VCP 31 and VCP 34 here, 38 of
/// the 158-volume corpus) declares 250 m reflectivity gates and delivers 500 m
/// content, **replicated exactly twice**: declared gate `2k` and declared gate
/// `2k+1` carry the same encoded value, always.
///
/// That is a property of the data and not of this decoder. MetPy, reading the
/// raw 8-bit gate integers rather than anything decoded here, finds the even
/// pairs identical over **134,570,160 gate pairs** across all 38 long-pulse
/// volumes, 25 sites, both VCPs and the holdout, with **zero exceptions** —
/// and the minimum over each of the 200,880 individual reflectivity radials is
/// 1.0, not merely the mean. The replication is exactly 2×: `raw[4k]` against
/// `raw[4k+2]` agrees 0.0794 of the time, which is the null rate, so there is
/// no 4× structure underneath it. Sentinels replicate too — the 37.6 M
/// below-threshold gates and the 336 range-folded ones pair up like the rest.
///
/// # Why it only matters under [`RangeBinning::Ground`]
///
/// Under [`RangeBinning::Slant`] a 1 km bin holds four declared gates starting
/// at an even index, so it holds two whole pairs and each 500 m sample is
/// weighted once. The `cos e` scaling of [`RangeBinning::Ground`] stretches
/// that to **four or five** declared gates per bin, so a pair straddles a bin
/// edge: one 500 m sample is counted twice in one bin and once in the next, and
/// a cell's mean is a weighted average that nothing intended.
///
/// # The registration, which is the part easy to get wrong
///
/// Reading every other gate is only half the fix. The declared first gate is at
/// **2125 m** — the ICD's range to the *centre* of gate 0, whose leading edge is
/// therefore 2000 m — and the declared gate count is even, so the `N/2` 500 m
/// samples tile `[2000 m, 2000 + 250·N)` exactly. The first sample spans
/// `[2000, 2500)` and its centre is **2250 m**, half a declared gate beyond
/// gate 0's own. Filing the pair at 2125 m instead — the first sub-gate's
/// centre — puts every sample 125 m too near the radar, and under the `cos e`
/// scaling that 125 m is most of the difference the whole change is about. It
/// is the same half-gate class of error as an index read as a centre, and this
/// crate has had one of those already.
///
/// So a pair's sample is filed at `first_gate + k·interval + interval/2`, which
/// for even `k` is just the declared gate's own range plus half a gate.
///
/// The oracle confirms the inputs to that argument and not its conclusion, and
/// the difference is worth keeping straight: MetPy reads `first_gate = 2125 m`
/// and `gate_width = 250 m` off the wire, and the gate count is even on every
/// long-pulse sweep (800…1832), but MetPy has no docstring, comment or
/// attribute anywhere saying whether `first_gate` is a centre or a leading
/// edge. **The centre reading is the ICD's, not MetPy's.** What the bits do
/// settle is the tiling: the pairing starts at gate 0 — `raw[0] == raw[1]` on
/// 32,400 of 32,400 radials checked, against a 0.079 null — so there is no
/// unpaired leading gate and sample `j` is exactly declared gates `2j, 2j+1`.
///
/// # What this is worth, measured
///
/// Not much, and the honest number belongs next to the mechanism. Over the
/// whole long-pulse corpus — 38 volumes, 25 sites, 3 in `holdout/` — filing
/// pairs correctly moves **79 of 9353 defined cells (0.84 %)**, in **3 of 38
/// volumes**, and only **11 cells in the entire corpus (0.12 %)** change the
/// 1 kft bin the ICD quantises echo tops to. The pooled mean absolute change is
/// **0.0014 kft**, about 0.4 m. **The holdout arm does not move a single
/// cell.** The largest single-cell change is 3.06 kft, in the one volume
/// (KGRB, 2024-12-16) that carries 68 of the 79.
///
/// The reason is structural rather than lucky, and it bounds the defect for
/// good: **a long pulse is only ever used in a clear-air VCP**, and every
/// long-pulse volume in the corpus — VCP 34 included — tops out between 4.44°
/// and 4.53°. `cos 4.53°` is 0.99688, so a 1 km ground bin holds **4.0125**
/// declared gates and a pair straddles a bin edge about once in every 80 bins.
/// The steep cuts where ground binning bites hardest (19.5°, 4.24 gates to a
/// bin) are short-pulse cuts, and short-pulse content is not replicated.
/// Replication and steep elevation never co-occur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateFiling {
    /// Every declared gate is its own sample, filed at its declared range.
    AsDeclared,
    /// Declared gates come in replicated pairs. Each pair is one sample, read
    /// from its even member and filed at the pair's centre — half a declared
    /// gate beyond that member's own range.
    ReplicatedPairs,
}

/// Radials the replication detector reads, spread at a fixed stride over the
/// sweep.
///
/// Sixteen rather than one because a *single* radial proves nothing: on the
/// short-pulse corpus individual radials reach 1.0000 even-parity agreement
/// where their sweeps reach 0.25, so a per-radial test would false-positive on
/// a data-starved cut. Sixteen rather than all of them because the detector
/// then costs about 2 % of the grid build it guards, and because the property
/// it looks for is not a tendency — it is exact, and a counterexample anywhere
/// in the sample ends the question.
const REPLICATION_SAMPLE_RADIALS: usize = 16;

/// Gate pairs carrying **numbers** that the detector must clear before it will
/// believe a sweep is replicated.
///
/// Measured, not guessed, over 158 archived volumes (120 short-pulse, 38
/// long-pulse; 5 VCPs including two TDWR volumes whose 150 m gates are the
/// finest — and so the smoothest-sampled — in the corpus):
///
/// | | short pulse (null) | long pulse (signal) |
/// |---|---|---|
/// | reflectivity sweeps | 1623 | 318 |
/// | sweeps where **every** sampled pair matches | **0** | **318** |
/// | most numeric pairs cleared before a mismatch | **3** | (no mismatch) |
/// | numeric pairs available in the sample | — | **≥ 46**, median 829 |
///
/// The two arms do not overlap and do not come close to it. A genuinely smooth
/// field agrees on adjacent gates about 7 % of the time — the short-pulse
/// even-parity rate is 0.067 at the median and 0.25 at its worst sweep, and the
/// long-pulse *odd*-parity control, which is the same null measured inside the
/// signal arm, reads 0.076 at the median and 0.273 at its worst. Replication
/// reads 1.0000, exactly, everywhere.
///
/// 32 sits an order of magnitude above the null's ceiling of 3 and 14 pairs
/// below the signal's floor of 46, so it costs nothing on either arm: every one
/// of the 318 long-pulse sweeps still detects, and none of the 1623 short-pulse
/// sweeps comes within a factor of ten of qualifying. Under the worst null rate
/// measured (0.25) clearing 32 pairs has probability 5 × 10⁻²⁰; even at a
/// coin-flip 0.5 it is 2 × 10⁻¹⁰.
///
/// **A sweep that cannot clear it is simply filed as declared** — the defect
/// this guards against is a mis-weighted mean, and declining to decimate leaves
/// exactly the behaviour that shipped. False positives are the expensive
/// direction: decimating a sweep that is *not* replicated would throw away half
/// its gates.
const REPLICATION_MIN_PAIRS: u32 = 32;

/// Whether a sweep's `moment` is 500 m content replicated onto a declared 250 m
/// grid — see [`GateFiling`] for the property and [`REPLICATION_MIN_PAIRS`] for
/// the measurement behind the constants.
///
/// The predicate is **exactness over a sample**, not a ratio: every declared
/// pair `(2k, 2k+1)` in [`REPLICATION_SAMPLE_RADIALS`] radials must be
/// identical — sentinels included, which they are, so a `BelowThreshold` beside
/// a number is itself a counterexample — and at least
/// [`REPLICATION_MIN_PAIRS`] of them must carry numbers, so that an empty sweep
/// whose every pair is two `BelowThreshold`s cannot qualify on vacuity.
///
/// Counting only numeric pairs toward the minimum is load-bearing and not
/// fastidiousness: over **all** gates a short-pulse sweep's runs of
/// below-threshold gates clear 2234 pairs before the first mismatch, against 3
/// once the count is restricted to gates carrying numbers.
///
/// Cheap because it is exact. The first mismatch ends the walk, and on
/// short-pulse data that arrives after one or two pairs — so the ~76 % of
/// volumes this changes nothing for pay a handful of comparisons per sweep,
/// not a scan. Measured end to end over the 120 short-pulse volumes,
/// best-of-five user CPU: **29.63 s without this, 29.69 s with it**, which is
/// 0.2 % and inside the run-to-run spread.
///
/// # Why a false positive cannot destroy data
///
/// Decimating a sweep that is not replicated would be the expensive mistake —
/// an unconditional stride-2 walk drops 1.4–19.5 % of a short-pulse sweep's
/// gates. This one cannot make it, because the gate it drops is one it has
/// **checked is bit-identical** to the gate it keeps. A false positive is
/// therefore not a loss of content; it is only a 125 m shift in where that
/// content is filed. The check is over a sample rather than every radial, so
/// that is an inference and not a proof — but it is an inference the oracle
/// measured on all 200,880 radials of the long-pulse arm and found exact.
///
/// The limit worth naming: content that happens to be **constant over
/// even-aligned pairs** is indistinguishable from replicated content, and no
/// local test can separate them. Real fields are nowhere near it — 1623
/// short-pulse sweeps clear at most 3 numeric pairs — and the failure would in
/// any case be the benign one above.
///
/// A pure function of `(radials, moment)`, like everything else
/// [`VolumeCube::build_with_stats`] runs under rayon, and deterministic: the
/// stride is fixed, so the same sweep is read the same way on every thread and
/// every run.
fn replicated_pairs(radials: &[Radial], moment: RadarProduct) -> bool {
    let stride = (radials.len() / REPLICATION_SAMPLE_RADIALS).max(1);
    let mut numeric_pairs = 0u32;
    for radial in radials.iter().step_by(stride) {
        let Some(md) = moment.get_moment(radial) else {
            continue;
        };
        let mut gates = md.iter();
        while let (Some(a), Some(b)) = (gates.next(), gates.next()) {
            if a != b {
                return false;
            }
            if matches!(a, MomentValue::Value(_)) {
                numeric_pairs += 1;
            }
        }
    }
    numeric_pairs >= REPLICATION_MIN_PAIRS
}

/// One moment's 360×230 grid on one tilt, with the sweep it came from.
pub struct MomentGrid {
    /// `[az_deg][range_km]`, `NaN` where no gate carried data.
    pub values: Vec<Vec<f32>>,
    /// Why each cell of [`values`](Self::values) is `NaN`, at the same
    /// `[az_deg][range_km]` indices — the three distinct facts the `NaN`
    /// cannot hold apart. `Value` exactly where `values` is finite.
    ///
    /// A **parallel plane** rather than a sentinel value or a `NaN` payload,
    /// and rather than replacing `values` with an enum grid. The reasoning is
    /// written out once at [`crate::types::GateReport`]'s call site in
    /// [`sweep_to_grid`]; the short of it is that this arm is additive — every
    /// existing reader of `values` keeps its type, its arithmetic and its
    /// results — while costing one byte per cell against four, and that it
    /// cannot be silently dropped by a later reader the way a sentinel can.
    ///
    /// A cell aggregates several gates, so this is their [`Ord`]-max: see
    /// [`crate::types::GateReport`] for why that ordering is the precedence.
    pub status: Vec<Vec<crate::types::GateReport>>,
    /// Index into [`Scan::sweeps`] of the sweep this grid was computed from.
    pub sweep_index: usize,
    /// Whether this sweep displaced an earlier sweep at the same elevation — a
    /// SAILS/MRLE repeat resolved by [`DedupPolicy::NewestWins`]. Always
    /// `false` under [`DedupPolicy::FirstOfVolume`], which keeps the sweep a
    /// repeat would have displaced.
    pub displaced_repeat: bool,
}

/// Beam bottom/centre/top heights above the radar, km, at every range cell
/// centre (`r + 0.5` km) of one tilt.
pub struct BeamHeights {
    pub bottom_km: Vec<f64>,
    pub centre_km: Vec<f64>,
    pub top_km: Vec<f64>,
}

impl BeamHeights {
    /// Heights for a tilt centred on `elev_deg`, the bottom and top at half
    /// the half-power beamwidth below and above it, over the cells of
    /// `binning`.
    ///
    /// `binning` is what makes a cell's two coordinates describe one point:
    /// under [`RangeBinning::Ground`] cell `r` holds the gates over `r + 0.5`
    /// km of *ground*, so its height is [`crate::beam::height_at_ground_km`]
    /// there and not the height of a beam `r + 0.5` km long.
    ///
    /// The flanks are the WSR-88D's. They are the one thing here a TDWR's
    /// narrower antenna would change, and nothing in production reads them —
    /// [`compute_echo_tops`] scans `centre_km` alone, and the one place a
    /// beamwidth reaches a rendered number is [`crate::hail`]'s top-layer cap,
    /// which takes the site's own. A product that starts reading `bottom_km`
    /// or `top_km` needs this to take a beamwidth too.
    fn at_elevation(elev_deg: f64, binning: RangeBinning) -> Self {
        let half = WSR88D_HALF_POWER_BEAMWIDTH_DEG / 2.0;
        let at = |e: f64| -> Vec<f64> {
            (0..RANGE_BINS)
                .map(|r| binning.height_km(r as f64 + 0.5, e))
                .collect()
        };
        Self {
            bottom_km: at(elev_deg - half),
            centre_km: at(elev_deg),
            top_km: at(elev_deg + half),
        }
    }
}

/// One distinct elevation of the volume.
pub struct Tilt {
    /// The elevation key, degrees, rounded to 0.1° — the resolution sweeps are
    /// deduplicated at.
    pub elevation_deg: f64,
    /// Beam geometry at every range cell centre.
    pub heights: BeamHeights,
    /// One entry per requested moment, in the cube's moment order. `None` when
    /// no sweep at this elevation carries the moment.
    grids: Vec<Option<MomentGrid>>,
}

/// The volume as a stack of polar grids: one 360° × 230 km grid per tilt per
/// requested moment, computed **once** per scan and shared by every product
/// derived from it.
///
/// Sweeps are chosen **per moment**: a split cut publishes reflectivity and
/// velocity at the same elevation on different sweeps, so a tilt's
/// reflectivity grid and its velocity grid may legitimately come from
/// different sweep indices. The tilt list is the union of every requested
/// moment's elevations, ascending.
pub struct VolumeCube {
    moments: Vec<RadarProduct>,
    pub tilts: Vec<Tilt>,
}

impl VolumeCube {
    /// Build the cube with each moment's default statistic
    /// ([`CellStat::for_moment`]).
    pub fn build(
        scan: &Scan,
        moments: &[RadarProduct],
        policy: DedupPolicy,
        binning: RangeBinning,
    ) -> Self {
        let with_stats: Vec<(RadarProduct, CellStat)> = moments
            .iter()
            .map(|&m| (m, CellStat::for_moment(m)))
            .collect();
        Self::build_with_stats(scan, &with_stats, policy, binning)
    }

    /// Build the cube with an explicit statistic per moment.
    ///
    /// `binning` decides which range a gate is filed under and, with it, the
    /// heights on every [`Tilt`] — see [`RangeBinning`] for why that is a
    /// caller's decision rather than this module's.
    ///
    /// The tilts are built **across rayon's pool**, one task per elevation.
    /// [`tilt_at`] is a pure function of `(scan, moments, chosen, key,
    /// binning)` — see
    /// its own note for why, and [`LinearZMemo`] for the table that would
    /// otherwise be shared — so the tasks touch nothing in common but
    /// immutable inputs, and no float crosses between them: **nothing in the
    /// parallel region is summed across tilts**, so a tilt's grid is the same
    /// arithmetic in the same order on the same inputs whichever thread runs
    /// it.
    ///
    /// `collect` into a `Vec` is order-preserving for an indexed parallel
    /// iterator, so `tilts` still comes out in ascending-elevation order.
    ///
    /// **That ordering is load-bearing, not incidental**, and the reason is
    /// downstream rather than here. Products *do* combine tilts, along the one
    /// axis this function must therefore keep stable: [`crate::hail`] and
    /// [`crate::vil`] accumulate a running `f64` over the tilt index per
    /// column, and [`crate::eet`] and [`compute_echo_tops`] scan
    /// `(0..tilts.len()).rev()` and stop at the first tilt that qualifies. All
    /// four are outside the parallel region and untouched by it — and all four
    /// are safe only because the tilts they walk are the same tilts, in the
    /// same order, as a serial walk would have produced. Reordering them would
    /// reassociate those sums and change which tilt "topmost" names, without
    /// making any single grid wrong.
    ///
    /// [`tests::the_tilts_the_pool_builds_are_the_tilts_a_serial_walk_builds`]
    /// pins order and contents against a serial walk over the same selection,
    /// on top of the pinned echo-tops digest this function is the direct
    /// producer of.
    pub fn build_with_stats(
        scan: &Scan,
        moments: &[(RadarProduct, CellStat)],
        policy: DedupPolicy,
        binning: RangeBinning,
    ) -> Self {
        let (chosen, keys) = sweep_selection(scan, moments, policy);

        let tilts = keys
            .into_par_iter()
            .map(|key| tilt_at(scan, moments, &chosen, key, binning))
            .collect();

        Self {
            moments: moments.iter().map(|&(m, _)| m).collect(),
            tilts,
        }
    }

    /// The moments this cube was built for, in grid order.
    pub fn moments(&self) -> &[RadarProduct] {
        &self.moments
    }

    /// The grid for one moment on one tilt. `None` when the tilt index is out
    /// of range, the moment was not requested, or no sweep at that elevation
    /// carries the moment.
    pub fn grid(&self, tilt: usize, moment: RadarProduct) -> Option<&MomentGrid> {
        let mi = self.moments.iter().position(|m| *m == moment)?;
        self.tilts.get(tilt)?.grids[mi].as_ref()
    }
}

/// One moment's chosen sweep at one elevation: the elevation key, the index
/// into [`Scan::sweeps`], and whether it displaced an earlier sweep at the
/// same elevation.
type SweepChoice = (f64, usize, bool);

/// Which sweep supplies each moment at each elevation, and the tilt list.
///
/// Returns the per-moment choices in encounter order, and the union of every
/// moment's elevations ascending — the keys [`VolumeCube::build_with_stats`]
/// then builds one tilt per.
///
/// Split out from the build so the tilt walk above it is the whole of what
/// runs under rayon, and so a test can drive that walk serially over exactly
/// this selection rather than restating the dedup rules and drifting from
/// them.
fn sweep_selection(
    scan: &Scan,
    moments: &[(RadarProduct, CellStat)],
    policy: DedupPolicy,
) -> (Vec<Vec<SweepChoice>>, Vec<f64>) {
    let mut chosen: Vec<Vec<SweepChoice>> = vec![Vec::new(); moments.len()];
    for (si, sweep) in scan.sweeps().iter().enumerate() {
        let Some(first) = sweep.radials().first() else {
            continue;
        };
        // Keyed on the sweep's median elevation, not the first radial's —
        // see [`sweep_elevation_deg`] for what settling does to the first.
        let key = (sweep_elevation_deg(sweep.radials()).unwrap_or_default() * 10.0).round() / 10.0;
        for (mi, (moment, _)) in moments.iter().enumerate() {
            if moment.get_moment(first).is_none() {
                continue;
            }
            match chosen[mi]
                .iter_mut()
                .find(|(k, ..)| (*k - key).abs() < 0.05)
            {
                Some(entry) => {
                    if policy == DedupPolicy::NewestWins {
                        *entry = (entry.0, si, true);
                    }
                }
                None => chosen[mi].push((key, si, false)),
            }
        }
    }

    // The union of every moment's elevations, ascending.
    let mut keys: Vec<f64> = Vec::new();
    for per_moment in &chosen {
        for &(k, ..) in per_moment {
            if !keys.iter().any(|e| (e - k).abs() < 0.05) {
                keys.push(k);
            }
        }
    }
    keys.sort_by(f64::total_cmp);

    (chosen, keys)
}

/// One tilt of the cube: the beam geometry at `key`, and each requested
/// moment's grid from whichever sweep [`sweep_selection`] gave that moment at
/// that elevation.
///
/// **A pure function of its arguments, and it has to stay one**: this is the
/// unit of work [`VolumeCube::build_with_stats`] hands to rayon, so the
/// signature is the claim — three shared references, a key and a [`Copy`]
/// binning in, one owned [`Tilt`] out, nothing read that another tilt writes
/// and nothing carried between calls. `binning` is the caller's for the whole
/// cube, so every tilt is filed and measured the same way whichever thread
/// builds it. [`sweep_to_grid`] is pure for the same reason and says so at
/// [`LinearZMemo`], whose table lives and dies inside a single call.
fn tilt_at(
    scan: &Scan,
    moments: &[(RadarProduct, CellStat)],
    chosen: &[Vec<SweepChoice>],
    key: f64,
    binning: RangeBinning,
) -> Tilt {
    let grids = moments
        .iter()
        .enumerate()
        .map(|(mi, &(moment, stat))| {
            chosen[mi]
                .iter()
                .find(|(k, ..)| (k - key).abs() < 0.05)
                .map(|&(_, si, displaced)| {
                    let radials = scan.sweeps()[si].radials();
                    let (values, status) = sweep_to_grid(
                        radials,
                        moment,
                        stat,
                        binning.binning_elevation_deg(radials),
                        binning.gate_filing(radials, moment),
                    );
                    MomentGrid {
                        values,
                        status,
                        sweep_index: si,
                        displaced_repeat: displaced,
                    }
                })
        })
        .collect();
    Tilt {
        elevation_deg: key,
        heights: BeamHeights::at_elevation(key, binning),
        grids,
    }
}

/// Buckets in a [`LinearZMemo`], as a power of two so the index is a shift.
///
/// Sized from the domain, not from a guess: an 8-bit moment block can express
/// at most 254 distinct values (raw 0 and 1 are the below-threshold and
/// range-folded sentinels, and never reach a value), and sixteen archived
/// volumes from six sites between 2010 and 2026 each offered 141–206, 214
/// between them. 2048 buckets leave even the 254-value ceiling nearly
/// collision-free — and a collision costs a recomputation, not an error.
const LINEAR_Z_MEMO_BITS: u32 = 11;

/// `10^(dBZ/10)` — [`CellStat::LinearZMean`]'s per-gate conversion — memoized
/// on the gate value's exact `f32` bit pattern.
///
/// Gate values are decoded from a fixed-point block, so this conversion's
/// input domain is tiny and discrete while a single sweep feeds it hundreds of
/// thousands of gates. Nearly every call is a repeat of one already answered,
/// and answering it again was about half the cube's build time on a volume
/// with real coverage.
///
/// **A hit returns the same bits, not merely the same value.** A miss runs
/// exactly the `10f64.powf(z as f64 / 10.0)` this conversion has always run
/// and stores *that* number; a hit hands the stored number back untouched.
/// Nothing here approximates the power, and nothing depends on which entries
/// happen to be resident: the table is direct-mapped, a collision simply
/// overwrites, and an evicted key recomputes to the identical number when it
/// next appears. That is why the pinned echo-tops digest
/// ([`tests::golden_echo_tops_grid_is_pinned`], mirrored four times in
/// `chunks::tests`) does not move. Rewriting the power itself — as `exp2`, say
/// — would have moved it, on most inputs.
///
/// One table per [`sweep_to_grid`] call, never leaving it. That is what lets
/// [`VolumeCube::build_with_stats`] fan its tilts across rayon: many tables are
/// live at once — one per sweep in flight, and a pane render is offloaded to
/// its own thread besides, so two whole cube builds can overlap — and a table
/// that cannot outlive a call frame cannot be shared between any of them.
/// `sweep_to_grid` therefore stays a pure function of its arguments whatever
/// thread it lands on, which is the property every digest pin rests on.
///
/// **Nothing tests this, and nothing can.** Hoisting the table to a shared
/// `static` or carrying it between calls in a `thread_local!` would leave
/// every output bit-identical — the key is the gate's exact `to_bits()` and
/// every writer stores the same `f64` for the same key, so a reader cannot
/// tell which writer won or whether the entry outlived the call that made it.
/// A `static` version would also be an unsynchronised data race that no
/// assertion over this cube can see. So the locality here is held by this
/// paragraph and by review, not by a pin, and it is not on offer: the
/// contract is that a hit returns the bits the miss it replaces would have
/// returned, and a table nobody else can reach is how that stays cheap to
/// believe.
struct LinearZMemo {
    /// `(gate value bits, 10^(value/10))` per bucket.
    slots: Vec<(u32, f64)>,
}

impl LinearZMemo {
    /// The free-bucket key: a pattern [`linear_z`](Self::linear_z) is never
    /// called with, because [`sweep_to_grid`]'s `z.is_nan()` filter stands in
    /// front of the only call site and this is a NaN.
    ///
    /// That filter is what makes this safe — **not** the bit pattern being
    /// exotic. A gate really can carry these exact bits: decoding is
    /// `(raw - offset) / scale`, a NaN `offset` propagates its payload
    /// unchanged through both operations on x86-64, and a block declaring
    /// `offset = f32::from_bits(0xFFFF_FFFF)` hands almost every gate a value
    /// whose `to_bits()` is precisely `u32::MAX`. Were the filter dropped,
    /// such a gate would match a bucket that had never been written and read
    /// the initial `0.0` as its own answer — the one way this table can return
    /// something `powf` would not. [`tests::a_nan_gate_never_reaches_the_memo`]
    /// pins the filter for that reason; it is load-bearing now in a way it was
    /// not before the memo existed.
    const FREE: u32 = u32::MAX;

    /// Buckets only for the statistic that converts. [`sweep_to_grid`] builds
    /// one memo per call whatever statistic it was asked for, and two of the
    /// three never reach the conversion at all, so buying them 2048 buckets
    /// would be 32 KiB allocated and filled per sweep to be read zero times.
    ///
    /// Declining it did not show up in measurement — `compute_eet`, which
    /// grids on [`CellStat::Max`], timed the same either way — so this is
    /// tidiness, not a win. It is here because the empty case costs nothing to
    /// carry (see [`Self::linear_z`]), not because it bought anything.
    fn for_stat(stat: CellStat) -> Self {
        Self {
            slots: match stat {
                CellStat::LinearZMean => vec![(Self::FREE, 0.0); 1 << LINEAR_Z_MEMO_BITS],
                CellStat::Mean | CellStat::Max => Vec::new(),
            },
        }
    }

    /// `10^(z / 10)`: from the table when this exact `z` has been converted
    /// before, and from `powf` — the identical call — when it has not.
    ///
    /// A bucketless memo answers every call from `powf`, which is to say it is
    /// precisely the expression this replaced. Carrying that case is free: the
    /// bucket lookup is one bounds check either way, and an empty `slots`
    /// simply fails it.
    #[inline]
    fn linear_z(&mut self, z: f32) -> f64 {
        let key = z.to_bits();
        // Fibonacci hashing. The keys come off a fixed-point decode, so their
        // low mantissa bits are largely zero and their exponents span a narrow
        // band; multiplying and taking the *top* bits spreads them where any
        // slice of the raw pattern would pile up.
        let idx = (key.wrapping_mul(0x9E37_79B1) >> (32 - LINEAR_Z_MEMO_BITS)) as usize;
        let Some(slot) = self.slots.get_mut(idx) else {
            return 10f64.powf(z as f64 / 10.0);
        };
        if slot.0 != key {
            *slot = (key, 10f64.powf(z as f64 / 10.0));
        }
        slot.1
    }
}

/// One sweep collapsed onto the cube's grid for one moment: per whole-degree
/// azimuth cell the radial nearest the cell centre, per 1-km range cell `stat`
/// over the gates falling in it. `NaN` where no gate carried data; gate values
/// ≥ 999 are the decoder's sentinels and are dropped.
///
/// `binning_elevation_deg` is [`RangeBinning::binning_elevation_deg`] — `None`
/// to file a gate under the range it was measured at, the sweep's median
/// elevation to file it under the ground arc it sits over, through
/// [`RangeBinning::range_of`]. A parameter rather than a re-derivation from
/// `radials` because it is one number per sweep and this runs once per moment
/// per tilt; the *conversion* it feeds is per gate, because an arc is not a
/// scale factor.
///
/// `filing` is [`RangeBinning::gate_filing`], a parameter for the same reason
/// and answering a different question: whether a declared gate *is* a sample,
/// or is one of a replicated pair that together make one. Over a pair this walk
/// reads the even gate only and files it half a declared gate further out —
/// which is where the pair's sample actually sits. See [`GateFiling`].
///
/// # The second return value, and why it is a second value
///
/// The `NaN` above is three different facts wearing one bit pattern — the
/// radar looked and found nothing, the radar found signal it cannot place in
/// range, and no gate was ever reported here — and
/// [`crate::types::GateReport`] says why a consumer wants them apart. Four
/// ways to keep them were weighed:
///
/// * **A parallel status plane** (this). Additive: `grid` keeps its type, its
///   arithmetic and every value it ever produced, so no existing reader
///   changes and no pinned digest moves. Costs one byte per cell against the
///   value's four — 82.8 kB per tilt per moment, 25% on top of a `f32` grid.
/// * **A sentinel in the value.** Free, and the cheapest to lose: nothing in
///   the type says the number is not a number, so the first consumer to
///   compare, average or rescale it silently launders the sentinel back into
///   data. This crate has already had to write `z >= 999.0` guards for
///   exactly that pattern arriving from a decoder.
/// * **`NaN` payload bits.** Also free — an `f32` `NaN` has 22 spare — and
///   there is precedent for it two crates over in the rasterizer's
///   `RANGE_FOLDED_BITS`. Rejected here because payloads are preserved by
///   convention rather than by the language: `f32 as f64`, `min`/`max` and
///   any library call may or may not carry them, and this grid is summed,
///   averaged and interpolated. The rasterizer's use survives because it
///   *paints* the value immediately, and even there the render path
///   deliberately canonicalises the payload away rather than let it reach JS.
/// * **An enum grid** replacing `values` outright. The clearest statement and
///   the most invasive: every consumer's element type changes, and the
///   arithmetic ones would have to unwrap on every gate of every column.
///
/// The plane wins on the same argument twice: it is the only one of the four
/// that adds the fact without moving any existing number, and the only one a
/// later reader cannot drop by accident — reading `values` without `status`
/// leaves you exactly where this crate already was, rather than somewhere
/// subtly worse.
///
/// **It stays crate-internal.** Nothing here crosses the worker boundary, so
/// the worker protocol stays at version 4; the products that do cross are
/// rasterized downstream of this. That is a decision, not an oversight — see
/// [`MomentGrid::status`].
fn sweep_to_grid(
    radials: &[Radial],
    moment: RadarProduct,
    stat: CellStat,
    binning_elevation_deg: Option<f64>,
    filing: GateFiling,
) -> (Vec<Vec<f32>>, Vec<Vec<crate::types::GateReport>>) {
    use crate::types::GateReport;
    let mut grid = vec![vec![f32::NAN; RANGE_BINS]; 360];
    let mut status = vec![vec![GateReport::NotReported; RANGE_BINS]; 360];
    // nearest radial per whole-degree centre
    let mut nearest: Vec<Option<usize>> = vec![None; 360];
    for (ri, radial) in radials.iter().enumerate() {
        let az = (radial.azimuth_angle_degrees() as f64).rem_euclid(360.0);
        let cell = az as usize % 360;
        let centre = cell as f64 + 0.5;
        let d = (az - centre).abs();
        let better = match nearest[cell] {
            None => true,
            Some(prev) => {
                let paz = (radials[prev].azimuth_angle_degrees() as f64).rem_euclid(360.0);
                d < (paz - centre).abs()
            }
        };
        if better {
            nearest[cell] = Some(ri);
        }
    }
    // Shared by every azimuth cell of this sweep: one sweep's gates repeat the
    // same few hundred values a third of a million times over.
    let mut memo = LinearZMemo::for_stat(stat);
    for (cell, slot) in nearest.iter().enumerate() {
        let Some(ri) = slot else { continue };
        let radial = &radials[*ri];
        let Some(md) = moment.get_moment(radial) else {
            continue;
        };
        let fg = md.first_gate_range_km();
        let gi = md.gate_interval_km();
        // How many declared gates one sample spans, and where its centre sits
        // relative to the gate this walk reads it from. Both are 1 and 0 for
        // ordinary content; see [`GateFiling`] for the pair case and for why
        // the half-gate is not optional.
        let (step, centre_shift) = match filing {
            GateFiling::AsDeclared => (1, 0.0),
            GateFiling::ReplicatedPairs => (2, gi / 2.0),
        };
        // (accumulator, gate count) per cell; what the accumulator holds
        // depends on `stat`.
        let mut acc = vec![(0.0f64, 0u32); RANGE_BINS];
        // `iter`, not `values`: this walk is sequential, so the `Vec` `values`
        // collects into would be eight bytes per gate allocated and dropped
        // for every azimuth cell of every sweep of the volume.
        for (j, v) in md.iter().enumerate().step_by(step) {
            // The bin is the gate's, whatever the gate said: a below-threshold
            // or range-folded gate is still a gate *here*, and filing it is
            // the whole point of the status plane.
            let r = RangeBinning::range_of(binning_elevation_deg, fg + j as f64 * gi + centre_shift)
                as usize;
            if r >= RANGE_BINS {
                continue;
            }
            let MomentValue::Value(z) = v else {
                // `max`, so the cell keeps the strongest claim any of its
                // gates made — see `GateReport`'s ordering note.
                status[cell][r] = status[cell][r].max(GateReport::of(&v));
                continue;
            };
            if z >= 999.0 || z.is_nan() {
                // The decoder's own out-of-range sentinels. A gate was
                // reported, but it carries neither a number nor either of the
                // two meanings the other arms have, so it raises nothing: the
                // cell keeps whatever its other gates said, and stays
                // `NotReported` if they said nothing.
                continue;
            }
            match stat {
                CellStat::LinearZMean => acc[r].0 += memo.linear_z(z),
                CellStat::Mean => acc[r].0 += z as f64,
                CellStat::Max => {
                    acc[r].0 = if acc[r].1 == 0 {
                        z as f64
                    } else {
                        acc[r].0.max(z as f64)
                    }
                }
            }
            acc[r].1 += 1;
        }
        for (r, (sum, n)) in acc.into_iter().enumerate() {
            if n > 0 {
                grid[cell][r] = match stat {
                    CellStat::LinearZMean => (10.0 * (sum / n as f64).log10()) as f32,
                    CellStat::Mean => (sum / n as f64) as f32,
                    CellStat::Max => sum as f32,
                };
                // Written from the same `n > 0` the value is, in the same
                // pass, so the plane and the grid cannot disagree about which
                // cells are defined. `xsect.rs` keeps its own status plane
                // honest the same way.
                status[cell][r] = GateReport::Value;
            }
        }
    }
    (grid, status)
}

/// Echo tops: height (kft above radar) of the interpolated crossing of
/// [`ET_THRESHOLD_DBZ`], scanning tilts top-down per column of a
/// newest-wins reflectivity [`VolumeCube`].
///
/// # What this is a twin of, and what it is not
///
/// **It is not the RPG's product 135.** [`crate::eet`] is; this is a separate,
/// adjacent product (`RadarProduct::EchoTopsInterpolated`, "Echo Tops
/// (Interp)"), added deliberately alongside it.
///
/// The reference it was calibrated against is **GR2Analyst's own Echo Tops** —
/// GR computes echo tops itself from the Level II volume rather than displaying
/// the RPG's. The module doc used to say only "a reference implementation's
/// readouts" without naming it, which is how it came to look self-validating.
/// The calibration was **twelve hover readouts on one frozen KOAX volume**
/// (2026-07-28): the cell statistic, the ground binning and the
/// echo-absent-above clamp were each chosen to move those twelve points, and
/// nothing since had re-measured them.
///
/// Two things followed that a reader should have in hand:
///
/// * That is single-site tuning, on one volume, at one site, in one regime.
/// * GR was **de-scoped as a target for this product family** by the user the
///   same day, on the ground that for products already displayed from RPG
///   Level III the RPG's version is the intended one and GR is a gut check.
///
/// # Measured against GR — the calibration generalised
///
/// GR2Analyst was run headless and its Echo Tops render decoded against its own
/// colour bar (step 0.164 kft, i.e. 50 m), registered **values-blind on the
/// footprint**. On cells GR paints:
///
/// | site | VCP | mean | rms | within 1 kft | footprint IoU |
/// |------|-----|------|-----|--------------|---------------|
/// | KDMX | 212 | −0.45 | 1.47 | 81.0 % | 0.846 |
/// | KFTG | 212 | −0.73 | 1.68 | 72.1 % | 0.908 |
/// | KCRP | 212 | −0.58 | 1.28 | 79.1 % | 0.934 |
/// | KMSX | 212 | −0.51 | 1.11 | 73.6 % | 0.647 |
/// | KATX | 215 | −0.08 | 0.34 | 98.0 % | 0.471 |
///
/// So it **does** reproduce its reference, at four sites that had no part in the
/// original KOAX tuning, with a small and consistent low bias of about half a
/// kft. Set that beside the RPG twin's 31.9–54.4 % within 1 kft in the table
/// above: this is a GR twin and not an RPG one, and the two tables are the
/// evidence for saying so rather than an assumption.
///
/// Three things bound that result, and none of them is a disagreement:
///
/// * **GR's colour bar floors at 10.17 kft.** Below that GR paints nothing, so
///   a decode cannot see it. At **KAMA, KICT, KESX and KGJX** every top is
///   under that floor and the oracle yielded no cells at all — "could not be
///   obtained", not "disagrees". That is **all three holdout sites**, so the
///   GR comparison is measured on five sites and *held out on none*. The
///   holdout carries the `eet` comparison and the independent-implementation
///   check above, not this one.
/// * **GR's own product header reports its threshold as `Top: 18.5dbz`**
///   (seen at KDMX, KICT and KESX) where [`ET_THRESHOLD_DBZ`] here is 18.3, the
///   RPG's fleet default. The product takes its threshold from one reference
///   and its target from the other. The sign does not explain the residual —
///   a lower threshold crosses *higher* — so the −0.5 kft is something else.
/// * The registration was checked against the known false-offset trap: shifting
///   range by ±1 and +2 bins degrades footprint IoU *and* rms *and* the
///   within-1-kft share at all four convective sites, all peaking at zero
///   shift. The mean alone drifts toward zero under a +1 shift and would have
///   sold that offset as real.
///
/// One convention this settles: **GR's Echo Tops is above the radar, not above
/// MSL**, which is what this function reports and is *not* what [`crate::eet`]
/// reports. KFTG sits 5.5 kft above sea level and KCRP at sea level; their
/// offsets against GR differ by 0.15 kft, not by 5.5.
///
/// # Measured against [`crate::eet`]
///
/// Nine volumes, nine sites, four VCPs (212/215/31/35), three of them a
/// holdout. Both grids taken in kft **above the radar** — `compute_eet` run
/// with its datum zeroed — so the comparison is of algorithm and convention
/// and not of site elevation. `eet` minus this, on cells both define:
///
/// | site | VCP | mean | rms | within 1 kft | footprint IoU |
/// |------|-----|------|-----|--------------|---------------|
/// | KDMX | 212 | +1.02 | 2.95 | 51.7 % | 0.808 |
/// | KFTG | 212 | +1.93 | 3.78 | 31.9 % | 0.855 |
/// | KCRP | 212 | +1.58 | 2.67 | 38.0 % | 0.933 |
/// | KMSX | 212 | +1.21 | 1.75 | 54.4 % | 0.715 |
/// | KATX | 215 | +0.69 | 1.13 | 79.1 % | 0.540 |
/// | KAMA | 31  | +0.09 | 0.27 | 100 %  | 0.477 |
/// | KICT | 35 (holdout) | +0.10 | 0.26 | 100 % | 0.194 |
/// | KESX | 35 (holdout) | +0.23 | 0.52 | 98.2 % | 0.404 |
/// | KGJX | 35 (holdout) | +0.29 | 0.56 | 89.3 % | 0.250 |
///
/// This reads **low** against the RPG twin wherever there is deep convection,
/// and only a third to a half of cells agree inside the ICD's own 1 kft
/// quantisation. The shallow clear-air sites agree on value because their tops
/// are shallow — but disagree most on *footprint*: at KICT the two products
/// share under a fifth of the cells either one defines, and `eet` defines more
/// cells than this at **all nine** sites.
///
/// # Where the difference comes from
///
/// Flipping one convention at a time from here toward [`crate::eet`], mean
/// change in kft: **[`CellStat::Max`] +0.54 … +1.87** and every other
/// convention within ±0.21 of zero — dedup policy, the 0.1°-rounded elevation
/// key, ground-versus-slant binning and the refraction model. Essentially the
/// whole *systematic* gap is the cell statistic.
///
/// "Negligible in the mean" is not "negligible", and the dedup policy is the
/// one to watch: switching it to [`DedupPolicy::FirstOfVolume`] moves the mean
/// by at most 0.07 kft while moving individual cells by up to **15.7 kft**
/// (p99 5.5 at KDMX), and drops footprint agreement to 0.21 at KICT. It is a
/// wash on average and a large per-cell disagreement — which is what picking a
/// different sweep looks like. [`DedupPolicy::NewestWins`] takes the *latest*
/// sweep at an elevation, and on a split cut both halves carry reflectivity,
/// so the freshest look is the short-PRT Doppler half rather than the
/// surveillance cut: measured independently, 30 of 31 split elevations across
/// these nine volumes. The policy's doc justifies itself by SAILS revisits and
/// does not mention split cuts.
///
/// That matters because the statistic is the one choice already settled
/// elsewhere: [`crate::eet`]'s [`CellStat::Max`] is correct per FMH-11 Part C
/// § 3.2.3 and beat linear-Z mean at all seven sites it was measured on. This
/// product uses [`CellStat::LinearZMean`], which is also what makes it define
/// fewer cells — averaging a 1-km cell's gates dilutes a strong gate below
/// 18.3 dBZ, so the column never crosses and reports nothing.
///
/// The residual also has the **opposite sign in height** to `eet`'s own. `eet`
/// grades +0.36 kft below 15 kft to −2.94 kft above 45 kft against the real
/// RPG; this grades the other way against `eet` — about −0.1 below 15 kft
/// rising to +1.8…+4.5 kft in the 25–45 kft bands. The two errors compound
/// rather than cancel, so against the actual RPG product this sits lower still
/// at height.
///
/// # A cube-level defect the measurement turned up
///
/// **One physical cut, visited twice, can become two tilts.** The cube keys
/// tilts on the sweep's median elevation rounded to 0.1°, and a SAILS revisit
/// of the same cut does not repeat that median exactly. When the two visits
/// straddle a rounding boundary they get two keys: KMSX's 0.879° cut keys at
/// 0.8 **and** 0.9 (medians 0.8350 / 0.8789), KICT's 0.483° cut at 0.4 **and**
/// 0.5 (medians 0.4395 / 0.4834) — 0.0439° apart, the same beam.
///
/// For a column scan that is not cosmetic. A column crossing at the lower
/// phantom interpolates toward a "next tilt up" that is the same beam 0.044°
/// higher — a rung of 0.003–1.3 kft where the real next cut is 2.9–5.2 kft up
/// — so its top is truncated to very near its own tilt. It reaches 5.2 % of
/// KMSX's defined cells and 34 % of KICT's.
///
/// This is [`VolumeCube`]'s keying, not this function's, so it reaches
/// [`crate::hail`], [`crate::eet`] and [`crate::vil`] equally. Keying on the
/// VCP's **target** elevation angle — bit-identical for both visits of a cut —
/// would remove the failure mode outright. Not done here: it re-pins every
/// cube consumer's fixtures at once and wants its own change.
///
/// Sized since, over the whole 158-volume corpus: **20 volumes split a physical
/// cut across two tilt keys**, and the split is the *same number every time* —
/// the two medians are **0.0439°** apart in 21 of 21 occurrences, which is the
/// Message 5 elevation quantisation the geometry audit measured independently.
/// It is a VCP-35-and-215 phenomenon (15 and 5 volumes) with one VCP 31 case,
/// and it splits at the 0.4/0.5 and 1.3/1.4 key boundaries. That is an order of
/// magnitude more common than the replication defect below and it moves cells
/// by kilofeet rather than by metres, so of the two it is the one worth a
/// change — but it is still a change to the tilt key that every cube consumer's
/// fixtures are pinned against, and two of those consumers are scored against
/// RPG twins.
///
/// # A second one: long-pulse volumes deliver 500 m content on a 250 m grid
///
/// [`GateFiling`] has the property, the mechanism and the registration; the
/// part that belongs here is what it is worth to *this* product. Under
/// [`RangeBinning::Ground`] a replicated pair can straddle a bin edge and be
/// counted twice on one side and once on the other. Measured over the whole
/// long-pulse corpus, correcting it moves **0.84 % of defined cells in 3 of 38
/// volumes**, changes the ICD's own 1 kft bin for **11 cells in the corpus**,
/// and moves **nothing at all in the holdout arm**. Its magnitude is capped by
/// the fact that a long pulse only ever flies in a clear-air VCP, which never
/// cuts above 4.53°.
///
/// [`crate::eet`] and [`crate::vil`] are untouched by it: they bin
/// [`RangeBinning::Slant`], where a 1 km bin holds two whole pairs and the
/// replication costs exactly nothing.
///
/// # None of this is a licence to "fix" this to match `eet`
///
/// The measurement above says this product is doing its job: it reproduces the
/// reference it was built to reproduce. Moving it toward [`crate::eet`] — the
/// cell statistic above all — would improve the RPG comparison and *break* the
/// GR one, and there is no reading of "correct" under which both improve. The
/// two products target two different things, and the tables are here so that
/// choice is made deliberately rather than by whoever reads one table first.
///
/// What is genuinely open, and cheap to close, is the 18.3-versus-18.5 dBZ
/// threshold and whether it accounts for any of the −0.5 kft.
pub fn compute_echo_tops(scan: &Scan) -> VolumetricGrid {
    // Ground: an echo top is the height of a column over a *place*, so the
    // tilts stacked into it have to be over the same place. See
    // [`RangeBinning`].
    let cube = VolumeCube::build(
        scan,
        &[RadarProduct::Reflectivity],
        DedupPolicy::NewestWins,
        RangeBinning::Ground,
    );
    // The tilts actually carrying reflectivity, bottom-up.
    let tilts: Vec<(&BeamHeights, &Vec<Vec<f32>>)> = cube
        .tilts
        .iter()
        .enumerate()
        .filter_map(|(ti, t)| {
            cube.grid(ti, RadarProduct::Reflectivity)
                .map(|g| (&t.heights, &g.values))
        })
        .collect();

    let mut out = vec![vec![f32::NAN; RANGE_BINS]; 360];
    for (az, row) in out.iter_mut().enumerate() {
        for (r, cell) in row.iter_mut().enumerate() {
            // topmost tilt meeting the threshold
            for ti in (0..tilts.len()).rev() {
                let z = tilts[ti].1[az][r];
                if !z.is_nan() && z >= ET_THRESHOLD_DBZ {
                    let h = tilts[ti].0.centre_km[r];
                    let ht = if ti + 1 < tilts.len() {
                        let z_up = tilts[ti + 1].1[az][r];
                        let h_up = tilts[ti + 1].0.centre_km[r];
                        if z_up.is_nan() {
                            // echo absent above: the tilt centre itself
                            h
                        } else {
                            // z_up < threshold (else ti wouldn't be topmost)
                            h + (h_up - h) * ((z - ET_THRESHOLD_DBZ) / (z - z_up)) as f64
                        }
                    } else {
                        h
                    };
                    *cell = (ht * 3.28084) as f32; // km -> kft
                    break;
                }
            }
        }
    }
    VolumetricGrid {
        values: out,
        range_bins: RANGE_BINS,
    }
}

#[cfg(test)]
pub(crate) mod tests;
