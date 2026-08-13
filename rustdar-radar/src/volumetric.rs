//! Volume-derived products computed from the Level II volume.
//!
//! The heart is [`VolumeCube`]: the whole volume collapsed once per scan onto
//! a 360° × 230 km polar grid per tilt, for whatever moments a product needs,
//! with beam geometry and sweep provenance alongside. Products
//! ([`compute_echo_tops`], and the EET/DVL/KDP/HCA family to come) are then
//! column scans over the cube rather than owners of their own gridding.
//!
//! The RPG's EET/DVL products use coarser grids and beam-top conventions; the
//! interpolated echo tops here interpolate between tilt centers, calibrated
//! against a reference implementation's readouts.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeBinning {
    /// Bin `r` holds the gates whose **slant** range falls in `[r, r+1)` km.
    Slant,
    /// Bin `r` holds the gates whose **ground** range — slant × `cos e` of the
    /// sweep's median elevation — falls in `[r, r+1)` km.
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
    fn range_scale(self, radials: &[Radial]) -> f64 {
        match (self, sweep_elevation_deg(radials)) {
            (Self::Ground, Some(e)) if e.is_finite() => e.to_radians().cos().clamp(0.0, 1.0),
            _ => 1.0,
        }
    }

    /// Beam-centre height, km, over a cell of this binning at `range_km`.
    ///
    /// The pair to [`range_scale`](Self::range_scale), and the reason both
    /// live on the enum: a cube whose bins moved but whose heights did not
    /// would put a gate's reflectivity at one point and its altitude at
    /// another, which is a silent error in every column scan above it.
    fn height_km(self, range_km: f64, elev_deg: f64) -> f64 {
        match self {
            Self::Slant => beam_height_km(range_km, elev_deg),
            Self::Ground => crate::beam::height_at_ground_km(range_km, elev_deg),
        }
    }
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
                    let (values, status) =
                        sweep_to_grid(radials, moment, stat, binning.range_scale(radials));
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
/// `range_scale` is [`RangeBinning::range_scale`] — 1 to file a gate under the
/// range it was measured at, `cos e` to file it under the ground it sits over.
/// A parameter rather than a re-derivation from `radials` because it is one
/// number per sweep and this runs once per moment per tilt.
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
    range_scale: f64,
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
        // (accumulator, gate count) per cell; what the accumulator holds
        // depends on `stat`.
        let mut acc = vec![(0.0f64, 0u32); RANGE_BINS];
        // `iter`, not `values`: this walk is sequential, so the `Vec` `values`
        // collects into would be eight bytes per gate allocated and dropped
        // for every azimuth cell of every sweep of the volume.
        for (j, v) in md.iter().enumerate() {
            // The bin is the gate's, whatever the gate said: a below-threshold
            // or range-folded gate is still a gate *here*, and filing it is
            // the whole point of the status plane.
            let r = ((fg + j as f64 * gi) * range_scale) as usize;
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
