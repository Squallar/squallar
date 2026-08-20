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

use crate::par::*;
use crate::types::RadarProduct;
use nexrad_model::data::{DataMoment, MomentValue, Radial, Scan};

/// Half-power beamwidth of the WSR-88D antenna, degrees. Beam bottom and top
/// heights sit half of this below and above the tilt centre.
pub const WSR88D_HALF_POWER_BEAMWIDTH_DEG: f64 = crate::beam::WSR88D_HALF_POWER_BEAMWIDTH_DEG;

/// Reflectivity threshold for echo tops, dBZ.
const ET_THRESHOLD_DBZ: f32 = 18.3;

/// Range cells of the cube and of every volumetric product: 1 km each, 230 km
/// total — the domain the RPG specifies its derived products over.
pub const RANGE_BINS: usize = 230;

/// How wide one of those cells is, km.
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
pub(crate) fn beam_height_km(range_km: f64, elev_deg: f64) -> f64 {
    crate::beam::height_km(range_km, elev_deg)
}

/// A sweep's elevation angle: the **median** of its radials' instantaneous
/// angles. `None` for an empty sweep.
///
/// Not the first radial's: the antenna can still be settling onto the cut
/// when the sweep starts, and the error is not small — a live KMRX volume's
/// 0.5° cut opened at 0.283° and its 19.5° cut at 19.297°.
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
/// 60 would be stacking it over a different column's reflectivity.
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
    fn range_of(elevation_deg: Option<f64>, slant_km: f64) -> f64 {
        match elevation_deg {
            Some(e) => crate::beam::ground_range_km(slant_km, e),
            None => slant_km,
        }
    }

    /// Beam-centre height, km, over a cell of this binning at `range_km`.
    fn height_km(self, range_km: f64, elev_deg: f64) -> f64 {
        match self {
            Self::Slant => beam_height_km(range_km, elev_deg),
            Self::Ground => crate::beam::height_at_ground_km(range_km, elev_deg),
        }
    }

    /// How this binning reads a sweep whose content may be coarser than the
    /// grid it declares — see [`GateFiling`] for the property and
    /// [`replicated_pairs`] for how it is recognised.
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
/// A **long-pulse** volume (`pulse_width = 4`; VCP 31 and VCP 34 here, 38 of
/// the 158-volume corpus) declares 250 m reflectivity gates and delivers 500 m
/// content, **replicated exactly twice**: declared gate `2k` and declared gate
/// `2k+1` carry the same encoded value, always.
///
/// Under [`RangeBinning::Slant`] a 1 km bin holds four declared gates starting
/// at an even index, so it holds two whole pairs and each 500 m sample is
/// weighted once. The `cos e` scaling of [`RangeBinning::Ground`] stretches
/// that to **four or five** declared gates per bin, so a pair straddles a bin
/// edge: one 500 m sample is counted twice in one bin and once in the next, and
/// a cell's mean is a weighted average that nothing intended.
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
const REPLICATION_SAMPLE_RADIALS: usize = 16;

/// Gate pairs carrying **numbers** that the detector must clear before it will
/// believe a sweep is replicated.
///
/// **A sweep that cannot clear it is simply filed as declared** — the defect
/// this guards against is a mis-weighted mean, and declining to decimate leaves
/// exactly the behaviour that shipped.
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
/// at most 254 distinct values.
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
struct LinearZMemo {
    /// `(gate value bits, 10^(value/10))` per bucket.
    slots: Vec<(u32, f64)>,
}

impl LinearZMemo {
    /// The free-bucket key: a pattern [`linear_z`](Self::linear_z) is never
    /// called with, because [`sweep_to_grid`]'s `z.is_nan()` filter stands in
    /// front of the only call site and this is a NaN.
    const FREE: u32 = u32::MAX;

/// Buckets only for the statistic that converts.
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
// two meanings the other arms have, so it raises nothing.
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
// cells are defined.
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
/// **It is not the RPG's product 135.** [`crate::eet`] is; this is a separate,
/// adjacent product (`RadarProduct::EchoTopsInterpolated`, "Echo Tops
/// (Interp)"), added deliberately alongside it.
///
/// The measurement above says this product is doing its job: it reproduces the
/// reference it was built to reproduce. Moving it toward [`crate::eet`] — the
/// cell statistic above all — would improve the RPG comparison and *break* the
/// GR one, and there is no reading of "correct" under which both improve. The
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
