//! Normalized Rotation (NROT): the azimuthal derivative of radial velocity,
//! normalized by a range-dependent divisor so one number reads the same at
//! every distance from the radar. The pipeline is reverse-engineered against
//! a reference implementation: kernel taps, divisor curve, median geometry,
//! and gating are all empirical rather than derived. Where it paints, it
//! correlates 0.996 with the reference's cursor readouts. It does not yet
//! paint as widely: over the 7.05–20 nm annulus of KTLX 2025-02-19's lowest
//! velocity cut the reference paints 5.13% of the bins where this module
//! paints 2.37%, and the two fields are **aligned, not displaced** — their
//! overlap peaks at exactly zero shift in both radials and gates, and their
//! features are the same shape (a median cluster of 3 radials × 4 gates
//! against 3 × 5). The shortfall is coverage upstream of the derivative
//! rather than gating; a hole in the stencil's window carries much of what is
//! left of it, and [`COH_MAX_STRADDLE`] the rest, now that
//! [`COH_FOLD_VNY_FRAC`] has taken the wedge out of the second.
//! The measurement apparatus lives on branch
//! `campaign-harness`, and so does the calibration record for every constant
//! whose readings survived — twelve of them kept only the apparatus, and say
//! so where they are declared.
//!
//! 1. Dealias the base velocity with the validity-marking multi-pass
//!    ([`dealias`]): environmental-wind and zero-isodop seeds, then
//!    radial/azimuthal bridges, flood fills and head-and-shoulders until
//!    nothing changes; unreached data keeps raw in bulk, and residual fold
//!    walls are censored. Folded velocity reads as a ±2·Vny jump, which the
//!    derivative stage would misread as extreme shear.
//! 2. Median-filter the dealiased field (3–5 radials by physical width × 5
//!    gates); centres whose window is mostly missing raw data read ND.
//! 3. At each bin, the azimuthal derivative is **one** antisymmetric tap
//!    stencil, at every range: the per-radial super-res operator
//!    ([`SPLIT_TAPS`]) on a 0.5° grid and [`LEGACY_TAPS`] on a sweep whose rows
//!    are already whole degrees, applied to 3-gate range means and divided by
//!    the local arc per radial. The sign-reversed outer tap produces the small
//!    negative side lobes flanking every strong gradient. Every tap cell must
//!    be intact and the profile must correlate with the stencil (r² ≥ 0.01);
//!    constant or incoherent profiles read ND.
//! 4. Divide ROT by the divisor curve — knot ranges in KILOMETRES, linearly
//!    interpolated, measured off the reference at 60 ranges (22.4 at 13 km
//!    rising to 24.0 at 22 km, then falling to 8.6 at 80 km and flat beyond)
//!    — and clamp to ±5.
//! 5. Blank painted clusters under 4 bins and one-gate-deep slivers.
//!
//! Values above 1.0 are significant rotation; above 2.5, extreme. The
//! reference quantizes NROT on a lattice of **0.0395** — 253 levels across
//! its own ±5 clamp, reported at bin centres — so differences below that are
//! not observable in its output at all. That number is not cosmetic: it is
//! what turns each of its readouts into an interval of half-width 0.0198, and
//! so what lets a candidate curve be tested for consistency rather than
//! scored for closeness.

use crate::beam::RE_EFF_KM;
// rayon on every target that has threads, the sequential stand-ins on wasm32.
use crate::par::*;

const KM_PER_NM: f64 = 1.852;

/// NROT is defined on -5..+5; the divisor curve guarantees nothing, so clamp.
const NROT_LIMIT: f64 = 5.0;

/// Skip bins closer than this. Residual ground clutter close to the radar
/// produces clamp-level fake shear (adjacent ±30 m/s bins over tens of meters
/// of arc). It is where the reference's own painting starts, walked at 0.05 nm
/// on a step edge painted continuously from 8 km outward: ND at every sample
/// to 7.00 nm and a value at 7.05 nm, identically at KHNX, KLOT and KMSX,
/// whose lowest cuts sit at +0.48°, +0.53° and −0.18° — so it is a range and
/// not a height. The previous campaign's own coarse ladder agrees (its first
/// value was at 7.03 nm); 6.75 nm was two gates short of it and painted 320
/// bins the reference does not, 3.1% of the standard set's inside-80 km
/// total. Measured provenance: branch `campaign-harness`.
const MIN_RANGE_NM: f64 = 7.05;

/// The magnitude at which this algorithm considers a bin **painted** — the
/// significance floor of the whole product, and the one number every consumer
/// of an NROT field has to agree with.
///
/// It is the threshold [`despeckle_nrot`] runs its 8-connected components
/// over: a bin under it is not part of any cluster, is never counted toward
/// [`DESPECKLE_MIN_BINS`], and survives only as a value nothing downstream is
/// meant to draw attention to. The 2D palette starts its first colour class
/// here too (`palette::NROT_CYCLONIC`'s "weak: slate…" stop), and the 3D
/// transparency profile takes its clear point from this constant by reference
/// (`voxel::volume_alpha_profile::NROT_CLEAR`) — so the value the algorithm
/// calls significant, the value the plan view first colours and the value the
/// volume first makes visible are one number and cannot drift apart.
///
/// # What this module paints that the reference does not
///
/// Ten campaigns measured coverage — which of the reference's bins this
/// module reaches. [`COH_MAX_STRADDLE`] carries that accounting. This is the
/// other direction, on the same decoded capture (KTLX 2025-02-19 15:05:14,
/// lowest cut, 3546 reference bins over the 7.05–20 nm annulus):
///
/// ```text
///     annulus bins                                  69 120
///     this module carries a value at                16 438   23.8%
///     this module paints                             1 559
///       the reference paints them too                  661   mean |0.486|
///       the reference paints nothing there             898   mean |0.592|
/// ```
///
/// **The bins this module is most confident about are the ones the reference
/// refuses.** They are not a rim: 412 of the 898 sit in 34 of this module's
/// 89 painted components that touch no reference paint at all, and the other
/// 486 sit *inside* components that do overlap — 435 of those 486 carrying
/// the same sign as the nearest agreeing bin, so they are this module's own
/// version of a shared feature, placed a bin or four off, and not a separate
/// detection. The two fields' clusters are the same shape (median 4 radials ×
/// 5 gates here, 3 × 4 there).
///
/// The one statistic that states the defect cleanly is the exceedance on the
/// ground both fields cover — this module's 16 438 carried bins:
///
/// ```text
///     |NROT| at least             0.25   0.35   0.50   0.75   1.00
///     this module paints          1559   1111    684    323    124
///     the reference paints        1021    464    239     98     40
///     ratio                       1.53   2.39   2.86   3.30   3.10
/// ```
///
/// This module's distribution has the heavier tail, and the gap widens with
/// the value. It is not a gain error and not a registration error: where the
/// reference reads over 0.6 this module paints 85–92% of the time, and over
/// the agreeing bins the two agree to a mean |Δ| of 0.168.
///
/// # Seven axes, and none of them separates
///
/// Each was measured against the decoded field end to end, despeckle
/// included, and each is quoted as the agreeing bins it costs against the
/// over-painted bins it removes, from 661 and 898:
///
/// ```text
///                                                    agreeing  over-painted
///     azimuthal smoothing ([`MEDIAN_AZ_HALF_MAX`] 3)     −168          −206
///     rms range texture ceiling 0.44 → 0.40               −64          −135
///     profile cells on common range offsets only          −44          −173
///     local data completeness, best of six windows        −30          −150
///     local along-beam straddle count, 420 variants        −0           −19
///     deeper range window ([`STENCIL_RNG_HALF`] 2..6)  precision falls 0.424 → 0.357
///     spatially disconnected echo islands         the sweep is one island of 80 972 bins
/// ```
///
/// Every one buys precision by giving up agreement, at five over-painted bins
/// per agreeing bin lost at the very best, and none of them lowers the mean
/// magnitude of what is left by more than a few hundredths — the rule that
/// removes 173 of them leaves the rest at |0.607| against |0.592|. Precision
/// here is purchasable and was not purchased.
///
/// # The data boundary is a marker and not the arithmetic
///
/// The over-painted bins do sit nearer the edge of valid data, and the
/// separation widens monotonically with how hard this module is painting —
/// mean distance in bins to the nearest missing gate, by this module's own
/// magnitude:
///
/// ```text
///     |NROT|        0.25–0.35  0.35–0.50  0.50–0.75  0.75–1.00   ≥1.00
///     agreeing           4.35       4.06       4.50       4.66    5.00
///     over-painted       3.99       3.81       3.93       3.39    3.20
///     (counts)        211/237    216/211    145/216     71/128  18/106
/// ```
///
/// That is the signature a derivative straddling a data edge would leave, and
/// it is the reason [`az_profile`]'s partial range windows were the leading
/// candidate: where the three-gate window is *empty* the cell is NaN and the
/// bin is refused — the stencil-window holes — and where it is *partly* empty
/// the cell is a mean over different gates than its neighbours, which is a
/// step the operator would differentiate. Those would be one rule failing
/// two ways.
///
/// **They are not.** Remove that step exactly — detrend each cell by the
/// window's own along-beam gradient before averaging, which costs no coverage
/// — and the painted values move by 0.9% at the agreeing bins and 1.8% at the
/// over-painted ones. The occupancy correlation is real and the arithmetic
/// behind it is worth about a hundredth of an NROT. Missing gates mark ground
/// where the velocity is poor, and poor velocity is what this module paints
/// loudly; the partial window is not what makes the value. Nor is the
/// dealiaser: it changed the bin at 18.3% of the agreeing bins (mean 4.21
/// m/s) against 7.9% of the over-painted ones (1.82 m/s), so the over-paint
/// sits on velocity it left alone.
///
/// So the discriminator is on an axis this module has not found — the same
/// answer [`COH_MAX_STRADDLE`] reaches from the coverage side, and the two
/// are separate defects: every one of the 898 lies outside the coherence
/// mask by construction. Measured provenance: branch `campaign-harness`.
///
/// # That over-paint is one volume's, and eight more say so
///
/// Everything above is KTLX 2025-02-19, because that was the only capture of
/// the reference's whole field there was. There are now nine, decoded the same
/// way each against its own calibration, one cut each, every pair registered at
/// exactly zero shift in radials and in gates. Over the same annulus:
///
/// ```text
///                Vny   this module   agreeing   reference   precision   mean |Δ|
///     KTLX     11.49          1559        661        3546      0.4240      0.168
///     KHNX     11.66             0          0         252           —          —
///     KCRP     31.52          1098        858        4357      0.7814      0.088
///     KDMX     27.93          1447       1104        2564      0.7630      0.062
///     KFTG     24.01           903        758        1668      0.8394      0.058
///     KATX     25.32           469        432         840      0.9211      0.027
///     KLOT     23.96            14          3          18      0.2143      0.197
///     KMSX     24.21            16          8          53      0.5000      0.054
///     KDDC     25.84          2731       2161        5823      0.7913      0.066
/// ```
///
/// **KTLX is the outlier and the accounting above is its portrait.** Where this
/// module paints at all, the five other storm volumes agree with the reference
/// 76% to 92% of the time and to a mean |Δ| of 0.03 to 0.09 — better than
/// KTLX's 0.168 — with the sign right at 96% to 100%. What KTLX had that they
/// did not was the coherence mask's two wedges, which refused 1092 of its
/// reference bins and none of KDMX's, KFTG's, KATX's or KDDC's.
/// [`COH_FOLD_VNY_FRAC`] is why: the wedges were the fold crossings of a wind
/// sitting near the Nyquist limit, counted as incoherence. KTLX now reads 1640
/// painted, 730 agreeing, precision 0.4451.
///
/// One number moved the other way and has been re-derived here. At KCRP the
/// 240 bins the reference refused averaged **|1.990|** against |0.338| at the
/// bins the two agree on: 137 over |1.0|, 110 over |2.0| and 8 at the
/// [`NROT_LIMIT`] clamp. [`COH_FOLD_VNY_FRAC`] takes 213 of them away, because
/// they were computed on velocity this rule had told the dealiaser to hand
/// back unresolved. [`CENSOR_VNY_FRAC`] carries what that velocity looked like
/// and why the censor standing over it did not object.
///
/// What is left, on the tree that rule landed on:
///
/// ```text
///                Vny   this module   agreeing   reference   precision   refused
///     KTLX     11.49          1640        730        3546      0.4451       910
///     KHNX     11.66             0          0         252           —         0
///     KCRP     31.52           935        905        4357      0.9679        30
///     KDMX     27.93          1453       1107        2564      0.7619       346
///     KFTG     24.01           903        758        1668      0.8394       145
///     KATX     25.32           469        432         840      0.9211        37
///     KLOT     23.96            14          3          18      0.2143        11
///     KMSX     24.21            16          8          53      0.5000         8
///     KDDC     25.84          2731       2161        5823      0.7913       570
/// ```
///
/// KCRP's 30 average |0.281|, and **nothing this module reports anywhere on
/// any of the nine cuts exceeds |2.2|** — the largest is 2.1445. **Nothing
/// reaches the [`NROT_LIMIT`] clamp on any of them**, so the question of what
/// the reference does at its own ceiling is now about a branch this corpus does
/// not exercise. It does the same thing: hovered on the KCRP cut, GR2Analyst's
/// Product Details panel reports a minimum of −5.00 and a maximum of +5.00.
///
/// # The cluster that used to sit at the top of that column
///
/// The row above read 941/905/0.9617 with 36 refused until
/// [`MEDIAN_MIN_DEALIASED_OCC`] landed, and four of those 36 were the only bins
/// over |2.0| the reference refused at any of the nine sites: one cluster, az
/// 72.72° to 74.21°, 9.52 to 9.65 nm, reaching **4.776**. That figure is the
/// pre-[`MEDIAN_MIN_DEALIASED_OCC`] state and survives here only as the
/// motivating defect — its own record carries the move, 4.7759 → 2.1445.
///
/// The four were never the mechanism [`CENSOR_VNY_FRAC`] describes — that one
/// is gone from this cut entirely, along with the last refused bin. They were
/// [`median_filter`]'s: the wind there sits at the fold, the raw sweep is a
/// ±30 checkerboard against a declared 31.52, the fold censor cuts it to a
/// filament and the median then returned **0.00** at a bin whose dealiased
/// value is **+30.00**, from a window holding {31, 30, −31, 30, 30, 0, −30,
/// −30, 0}. The operator differentiates that. GR2Analyst hovered at that bin
/// reads **+0.18**. [`MEDIAN_MIN_DEALIASED_OCC`] is the floor that refuses such
/// a window a median at all, and it takes all four.
///
/// Three guards against it were measured — refuse a median window spanning
/// more than k·Vny, one whose sorted values leave a k·Vny gap, one that moves
/// its own centre by more than k·Vny — over k from 0.5 to 1.8. Each removes
/// the four; each costs KTLX 2025-02-19 between 24 and 380 agreeing bins,
/// because at a declared 11.49 one fold is 23 m/s and ordinary shear reaches
/// it. Four bins on one volume do not buy a law that a site at a third of the
/// Nyquist has to obey, and none is taken.
///
/// # The decoded reference clips, and one column here is read through it
///
/// GR2Analyst's NROT colour bar runs **−2.00 to +3.00**; its product runs to
/// ±5.00. So every decoded bin at a rail is a lower bound, and 646 of KCRP's
/// are — 236 at the top and 410 at the bottom. Hovered, six of the top rail
/// read +3.06 to +3.30 and four of the bottom −2.19 to −2.47.
///
/// Where the reference paints nothing it is black, and black is exact, so
/// every count above stands. The **mean |Δ|** column of the first table does
/// not: it is computed against a clipped field and is a lower bound wherever
/// the reference exceeds its own bar, which at KCRP is 15% of what it paints.
/// It is left as it was measured, and nothing in this module is decided on it.
///
/// # The eighth axis: magnitude and completeness together
///
/// Seven axes are refused above for buying precision with agreement. The eighth
/// is the pair of them — *refuse |NROT| ≥ M where the footprint the operator
/// read is not fully populated* — which at KTLX alone is worth ten over-painted
/// bins per agreeing bin lost, twice the best of the seven. Both thresholds
/// were swept, over three definitions of the footprint (this module's median
/// field, the raw sweep, the dealiased grid), with M absolute, as a fraction of
/// the sweep's Nyquist, and as a percentile of the site's own painted
/// distribution. Over-painted bins removed per agreeing bin lost, footprint =
/// the stencil's own ±5 rows × 3 gates of the median field, nothing missing:
///
/// ```text
///        M    KTLX    KCRP    KDMX    KFTG    KATX  |  KDDC (holdout)
///     0.75    3.16    2.64    1.39    1.20    1.60  |  0.63
///     0.90    5.23    2.62    2.38    1.06    1.36  |  0.49
///     1.00    7.07    2.67    2.80    1.07    1.40  |  0.37
///     1.25    9.00    3.33    1.25    1.15    1.00  |  0.53
/// ```
///
/// **The rate does not transfer.** It is 7:1 at KTLX, 1.1:1 at KFTG and below
/// break-even at the holdout, where the rule spends 38 agreeing bins to remove
/// 14 and leaves precision at 0.7925 against 0.7913. Neither normalization
/// repairs it: at every operating point of every footprint definition that
/// moves more than a handful of bins, some live site's ratio is under one. The
/// points the holdout does survive sit so deep in the footprint — under 28 of
/// 33 cells — that the whole nine-site effect is 25 over-painted bins at KTLX
/// and 11 at the holdout for one agreeing bin, which is inside the instrument's
/// own resolution. KHNX, KLOT and KMSX are untouched at every point.
///
/// So the ten-to-one is KTLX's and not a law, and nothing is taken on its
/// account. [`median_filter`] carries what happens when this rule is asked to
/// stand as the guard on the coverage relaxation instead.
///
/// # Two precision conventions, and which one is quoted
///
/// The reference's colour bar has no class for |v| < 0.25, so a bin where it
/// reads 0.24 and this module reads 0.26 is a precision failure no hover-free
/// instrument can tell from a fabrication. 560 of KTLX's 898 read at least 0.42
/// — the floor plus the mean |Δ| the two fields agree to — and 338 read under
/// it. Every precision figure in this module's documentation is the strict one,
/// agreeing over painted, counting all 898. The other convention, agreeing over
/// agreeing-plus-those-560, reads **0.5414** where the strict one reads 0.4240,
/// and on every candidate measured the two moved the same way. Measured
/// provenance: branch `campaign-harness`.
pub const SIGNIFICANT: f64 = 0.25;

/// Blank painted clusters (8-connected runs of |NROT| ≥ [`SIGNIFICANT`])
/// smaller than this many bins. Empirical, chosen to match the reference's
/// painted density.
///
/// The density comparison it was chosen on did not outlive the scratchpad it
/// ran in. Branch `campaign-harness` carries that campaign's apparatus
/// (`campaigns/nrot/nrot-lab`) and none of its readings, so this is a number
/// to re-measure rather than one to cite.
const DESPECKLE_MIN_BINS: usize = 4;

/// A velocity sweep as a dense azimuth × range grid. NaN marks missing data.
/// Rows are in sweep order, so row `i` borders rows `i±1` and the first and
/// last rows border each other.
pub struct VelocitySweep<'a> {
    pub vel_grid: &'a [Vec<f64>],
    pub azimuths_deg: &'a [f64],
    pub gate_count: usize,
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
    /// Where this sweep's cut **declared** its velocity folds, m/s, or `None`
    /// when the volume declared nothing for it.
    ///
    /// Read by [`dealias_with_knobs`] and by nothing else in this module: it is
    /// the interval every fold decision is a multiple of, and
    /// [`estimate_nyquist`] is what stands in when it is absent. A
    /// [`WindProfileBuilder`] sweep leaves it `None` on purpose — the VAD fit
    /// trims folded samples statistically and has no use for a limit.
    ///
    /// The number comes off Message 31's Radial Data Block by way of
    /// [`crate::nyquist::DeclaredNyquist`], which is also what
    /// [`crate::sampler::VolumeSampler`] guards its velocity interpolation on.
    /// The two reading one table is the point: a section and a plan view that
    /// disagree about where a sweep folds disagree about which of its gates are
    /// one datum.
    pub declared_nyquist_ms: Option<f64>,
    /// Why each cell of [`vel_grid`](Self::vel_grid) is `NaN`, at the same
    /// `(radial, gate)` indices — [`crate::velocity::VelocityGrid::status`],
    /// borrowed.
    ///
    /// `None` is not "unknown". It says **this sweep has no report plane**,
    /// which is true of exactly one kind of sweep: a grid built in a test or a
    /// probe out of numbers rather than decoded out of a volume. Such a grid
    /// has no below-threshold gates and no range-folded ones to have flattened,
    /// so the finiteness of a cell *is* the whole of what it reports, and the
    /// one reader below falls back to finiteness. Every decoded path carries
    /// `Some`; `velocity_grid_sweeps_carry_the_report_plane` pins that.
    ///
    /// Read by [`median_filter`]'s raw-occupancy cliff and by nothing else yet.
    /// The other candidates [`crate::velocity::VelocityGrid::status`] names —
    /// the dealiaser's coverage rules, `tap_stencil`'s intact-tap demand — each
    /// move painted pixels and want their own measurement.
    pub status: Option<&'a [Vec<crate::types::GateReport>]>,
}

/// How this sweep's rows sit in azimuth: the step every stencil's
/// `arc_per_radial` is built from — and so the scale of every NROT value this
/// module reports — together with whether row `n−1` borders row 0.
///
/// [`crate::azimuth::Rows`] holds both and says why they are one question.
/// What the answers cost *here*, when a sector takes the complete cut's pair
/// (`360 / n`, and "the last row borders the first"), is two separate wrong
/// numbers:
///
/// * **Scale.** A 36° sector of 72 radials is 0.5° apart and reads 5°, so every
///   arc is ten times too long and every rotation over it ten times too small —
///   a tornadic couplet at 1.8 comes back 0.18, under the 0.25 [`SIGNIFICANT`]
///   floor, and the product paints nothing where the strongest rotation in the
///   sector is.
/// * **Seam.** Both stencils need every cell of a ±5 span, so the outermost
///   five rows at each end of that sector are read partly from the other end of
///   it — 324° away, across ground the antenna never pointed at. A field of 6
///   (m/s)/km of honest shear stands 187 m/s apart across those two ends at 50
///   km, and dividing that by the 0.44 km of arc half a degree spans saturates:
///   rows 0, 1, 70 and 71 come back at the ±5 clamp and rows 3 and 68 at 2.49,
///   against the 0.43 the field carries there and with nothing rotating
///   anywhere in it.
fn sweep_rows(sweep: &VelocitySweep, num_radials: usize) -> crate::azimuth::Rows {
    crate::azimuth::Rows::of(sweep.azimuths_deg, num_radials)
}

/// Run the full pipeline without a wind profile (elevation assumed 0.5°).
/// Output is indexed like the input grid; NaN where NROT is undefined (no
/// velocity, or too few neighbours to fit).
pub fn compute_nrot_grid(sweep: &VelocitySweep) -> Vec<Vec<f64>> {
    compute_nrot_grid_with_profile(sweep, 0.5, None)
}

/// Run the full pipeline with a volume wind profile guiding fold-branch
/// decisions. The profile is fitted from every velocity tilt in the volume
/// via [`WindProfileBuilder`], so its predictions stay well-conditioned at
/// long range where the sweep's own echo fills only a narrow azimuth sector.
pub fn compute_nrot_grid_with_profile(
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
) -> Vec<Vec<f64>> {
    let pre = preprocess_velocity_with(sweep, elevation_deg, profile);
    let mut grid = llsd_nrot(sweep, &pre.dealiased, &pre.median, pre.refused.as_deref());
    despeckle_nrot(
        &mut grid,
        DESPECKLE_MIN_BINS,
        sweep_rows(sweep, sweep.vel_grid.len()),
    );
    grid
}

/// Blank painted clusters smaller than `min_bins`: 8-connected components of
/// |NROT| ≥ [`SIGNIFICANT`] (either sign — a tiny dipole is still speckle).
/// Azimuth wraps where the sweep closes the circle; range never does.
fn despeckle_nrot(grid: &mut [Vec<f64>], min_bins: usize, rows: crate::azimuth::Rows) {
    let num_radials = grid.len();
    if num_radials == 0 {
        return;
    }
    let gate_count = grid[0].len();
    let painted = |g: &[Vec<f64>], i: usize, j: usize| {
        let v = g[i][j];
        !v.is_nan() && v.abs() >= SIGNIFICANT
    };
    let mut seen = vec![false; num_radials * gate_count];
    let mut stack = Vec::new();
    let mut comp = Vec::new();
    for i0 in 0..num_radials {
        for j0 in 0..gate_count {
            if seen[i0 * gate_count + j0] || !painted(grid, i0, j0) {
                continue;
            }
            comp.clear();
            stack.push((i0, j0));
            seen[i0 * gate_count + j0] = true;
            while let Some((i, j)) = stack.pop() {
                comp.push((i, j));
                for di in -1i32..=1 {
                    // Two clusters at the two ends of a sector are two
                    // clusters, each counted against `min_bins` on its own.
                    let Some(ii) = rows.neighbour(i, di) else {
                        continue;
                    };
                    for dj in -1i32..=1 {
                        let jj = j as i32 + dj;
                        if jj < 0 || jj >= gate_count as i32 {
                            continue;
                        }
                        let jj = jj as usize;
                        if !seen[ii * gate_count + jj] && painted(grid, ii, jj) {
                            seen[ii * gate_count + jj] = true;
                            stack.push((ii, jj));
                        }
                    }
                }
            }
            let (jmin, jmax) = comp
                .iter()
                .fold((usize::MAX, 0), |(lo, hi), &(_, j)| (lo.min(j), hi.max(j)));
            // A cluster one gate deep in range is a tangential sliver — an
            // artifact of hole-filling along a thin velocity arc, not a
            // rotation signature.
            if comp.len() < min_bins || jmax == jmin {
                for &(i, j) in &comp {
                    grid[i][j] = f64::NAN;
                }
            }
        }
    }
}

/// Everything step 1 and step 2 produce that [`llsd_nrot`] then reads.
struct Preprocessed {
    /// The dealiased field, which the continuity ceiling is measured on.
    dealiased: Vec<Vec<f64>>,
    /// The median-filtered field, which the stencils differentiate.
    median: Vec<Vec<f64>>,
    /// The incoherence mask the dealiasing set aside, or `None` from a
    /// dealiasing that produced none — see [`dealias_with_knobs`] for which is
    /// which, and why the difference matters. It travels with the grids
    /// because [`llsd_nrot`] refuses exactly the ground the dealiaser refused,
    /// and asking [`incoherent_velocity`] a second time cannot change it.
    refused: Option<Vec<bool>>,
}

fn preprocess_velocity_with(
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
) -> Preprocessed {
    let mut vel: Vec<Vec<f64>> = sweep.vel_grid.to_vec();
    let refused = dealias(
        &mut vel,
        sweep,
        elevation_deg,
        profile,
        DealiasProfile::NoFalseShear,
    );
    let med = median_filter(
        &vel,
        sweep.vel_grid,
        sweep.status,
        sweep.gate_count,
        sweep.first_gate_range_km,
        sweep.gate_interval_km,
        sweep_rows(sweep, sweep.vel_grid.len()),
    );
    Preprocessed {
        dealiased: vel,
        median: med,
        refused,
    }
}

/// Wind-profile layer thickness, km.
const PROFILE_LAYER_KM: f64 = 0.3;
/// Layers span 0..12 km AGL.
const PROFILE_LAYERS: usize = 40;

/// How far an unfitted layer may be filled from the nearest fitted one, in
/// layers — 3, so **0.9 km** at [`PROFILE_LAYER_KM`].
///
/// # The two fills disagreed, and the unbounded one was the shipped one
///
/// [`WindProfile::from_levels`] has always bounded this fill to ±3 layers.
/// [`WindProfileBuilder::finish`], right beside it and the constructor the
/// render path actually uses, bounded it not at all: every unfitted layer took
/// the nearest fitted layer's wind however far away that was, so the profile
/// **always returned 40 of 40 layers** whenever a single layer anywhere fit.
///
/// Two things followed, both of them the code claiming more than it could
/// support:
///
/// - [`crate::srv::BUNKERS_MIN_MEAN_LAYERS`] asks for 12 of the twenty 0–6 km
///   layers to carry a fit before it will call their mean "the 0–6 km mean
///   wind". Against a profile that always answers, that count was always 20
///   and the refusal **could never fire** — dead code guarding a threshold
///   nothing could cross.
/// - Bunkers reads its shear from the 5.5–6.0 km band. At 7 of the 13 sites
///   held, no layer near 6 km fit anything, and that band was a clamp-copy of
///   a much lower layer — so the shear vector was computed between two winds
///   that were, in part, the same wind, and reported as though it were not.
///
/// # Why 3 and not another number
///
/// It is not a new number: it is the bound `from_levels` already applied, now
/// applied by both. Making the two agree is most of the point — one profile
/// type with one filling rule, rather than a rule that depends on which
/// constructor a caller happened to reach for.
///
/// 0.9 km is also the distance over which "winds vary slowly with height" —
/// the justification the unbounded fill was written under — is actually true.
/// It stays true across a layer or three and stops being true across the eight
/// kilometres the unbounded fill was willing to reach.
const PROFILE_FILL_MAX_LAYERS: i64 = 3;
/// Sample cap per layer keeps memory bounded on wasm. A volume offers far
/// more than this: KCRP 2017-08-26 04:41:14 has 326 657 gates to give the
/// twenty layers under 6 km, and its lowest layer alone is offered more than
/// the cap within the first two of its fifteen cuts.
///
/// So *which* samples the cap keeps is a question about the fit, not only
/// about memory, and [`WindProfileBuilder::offer`] answers it by thinning
/// rather than by stopping. See there for what stopping cost.
const PROFILE_MAX_SAMPLES: usize = 16384;

/// Largest RMS fit residual, m/s, a layer may carry and still be published as
/// a wind — the RPG's own goodness-of-fit ceiling, converted.
///
/// # This number is the RPG's, not ours
///
/// The NVW product carries the adaptable parameters the RPG's own VAD was run
/// under, and a fit-quality ceiling is one of them: **RMS ≤ 9.7 kt**, alongside
/// ≥25 data points per level, 2 fit passes, and a fixed 16.2 nmi ring. The RPG
/// honours it rather than merely declaring it — across the thirteen volumes
/// whose NVW we hold, **not one published a level whose RMS exceeded 9.7**.
///
/// We had the other two gates and not this one. [`WindProfileBuilder::finish`]
/// refuses a layer with under 200 samples and trims residuals over 12 m/s
/// before its second pass, but nothing then asked whether what came back
/// *fit*: a layer whose samples are a folded mess solves its normal equations
/// just as willingly as a clean one, returns a wind that is biased low, and is
/// published with no mark on it. The dealiaser then seeds from that wind.
///
/// # Why the same number applies to a differently-shaped fit
///
/// The RPG fits one elevation's ring; this fits every tilt pooled into a
/// height layer, so its residual carries real wind variation across the pooling
/// volume on top of measurement noise, and is structurally the larger of the
/// two. Applying the RPG's ceiling to it is therefore **conservative in the
/// direction that matters** — it refuses some layers the RPG would have kept,
/// and refuses every layer the RPG would have refused. A gate that errs toward
/// silence is the right error for a seed: [`WindProfile::predict`] is consulted
/// to *propose* a branch, and a layer that proposes nothing costs coverage,
/// while a layer that proposes a wrong wind costs correctness.
const PROFILE_MAX_RMS_MS: f64 = 9.7 * 0.514_444;

/// Residual, m/s, past which a sample is dropped from the second of
/// [`WindProfileBuilder::finish`]'s two fit passes — the robust-regression trim
/// that keeps folded bins in a raw sweep from dragging the layer's wind.
///
/// Named because [`PROFILE_MAX_RMS_MS`] is measured over the population this
/// admits, and the two numbers have to be read together: an RMS taken over the
/// untrimmed samples would be a statement about the outliers the trim exists to
/// discard, not about how well the published wind describes the gates it was
/// fitted from.
const PROFILE_TRIM_MS: f64 = 12.0;

/// Horizontal wind fitted per height layer from every velocity tilt of a
/// volume: vr ≈ u·sin(az)·cos(el) + v·cos(az)·cos(el) + c.
pub struct WindProfile {
    /// (u, v, c) per layer; NaN-filled layers had too little data.
    layers: Vec<Option<(f64, f64, f64)>>,
}

impl WindProfile {
    /// Build from explicit (height km, u, v) levels. The render path fits
    /// its profile from the volume ([`WindProfileBuilder`]); this constructor
    /// exists for callers that already hold levels — tests, mostly. Levels
    /// map to the internal layers; gaps between adjacent levels are filled
    /// by the nearer level.
    pub fn from_levels(levels: &[(f64, f64, f64)]) -> Option<Self> {
        if levels.is_empty() {
            return None;
        }
        let mut layers: Vec<Option<(f64, f64, f64)>> = vec![None; PROFILE_LAYERS];
        for &(h, u, v) in levels {
            let l = (h / PROFILE_LAYER_KM) as usize;
            if l < PROFILE_LAYERS {
                layers[l] = Some((u, v, 0.0));
            }
        }
        // Fill interior gaps from the nearest filled layer below/above.
        let filled: Vec<usize> = (0..PROFILE_LAYERS)
            .filter(|&l| layers[l].is_some())
            .collect();
        for l in 0..PROFILE_LAYERS {
            if layers[l].is_none() {
                let nearest = filled
                    .iter()
                    .min_by_key(|&&f| (f as i64 - l as i64).unsigned_abs());
                if let Some(&f) = nearest
                    && (f as i64 - l as i64).abs() <= PROFILE_FILL_MAX_LAYERS
                {
                    layers[l] = layers[f];
                }
            }
        }
        Some(WindProfile { layers })
    }

    /// Layer thickness the profile is discretised at, km. Public so a
    /// consumer integrating over height bands (Bunkers storm motion in
    /// [`crate::srv`]) can sample every layer exactly once via
    /// [`wind_at_km`](Self::wind_at_km) at the layer centres.
    pub const LAYER_KM: f64 = PROFILE_LAYER_KM;

    /// The fitted horizontal wind `(u, v)` in m/s at `height_km` AGL, or
    /// `None` below zero, above the profile, or in a layer nothing fit.
    /// Resolved at layer granularity ([`Self::LAYER_KM`]), no interpolation.
    pub fn wind_at_km(&self, height_km: f64) -> Option<(f64, f64)> {
        if !height_km.is_finite() || height_km < 0.0 {
            return None;
        }
        let l = (height_km / PROFILE_LAYER_KM) as usize;
        self.layers.get(l)?.map(|(u, v, _)| (u, v))
    }

    /// Predicted radial velocity at the given azimuth (radians), range (km)
    /// and elevation (degrees), or None where no layer was fit.
    fn predict(&self, az_rad: f64, range_km: f64, elevation_deg: f64) -> Option<f64> {
        let el = elevation_deg.to_radians();
        let h = crate::beam::height_km(range_km, elevation_deg);
        let l = (h / PROFILE_LAYER_KM) as usize;
        let layer = *self
            .layers
            .get(l)?
            .as_ref()
            .or_else(|| self.layers.get(l + 1)?.as_ref())
            .or_else(|| self.layers.get(l.wrapping_sub(1))?.as_ref())?;
        let (u, v, c) = layer;
        Some(u * az_rad.sin() * el.cos() + v * az_rad.cos() * el.cos() + c)
    }
}

/// [`crate::beam::height_km`] with `sin(elevation)` already computed.
///
/// The one place this crate still writes the beam-height expression out, and it
/// earns it: [`WindProfileBuilder::add_sweep`] hoists `sin` and `cos` once per
/// sweep and then runs this over every third gate of every radial — tens of
/// thousands of evaluations where the shared function would recompute
/// `to_radians().sin()` each time. (Its sibling in [`WindProfile::predict`]
/// hoisted nothing, so that one simply calls `beam::height_km`.)
///
/// Being a named function rather than an inline expression is the point: it is
/// what lets `the_hoisted_beam_height_is_bit_identical_to_the_shared_one` pin
/// the copy against `beam::height_km` **directly**, rather than against a
/// transcription of it in a test. That matters here more than elsewhere,
/// because NROT is the one calibrated path the echo-tops golden digests do not
/// cover, and `(h / PROFILE_LAYER_KM) as usize` **floors** — so a one-ulp drift
/// at a layer boundary silently moves a sample into the neighbouring wind layer.
#[inline]
fn height_km_with_sin_el(range_km: f64, sin_el: f64) -> f64 {
    range_km * sin_el + range_km * range_km / (2.0 * RE_EFF_KM)
}

/// Accumulates VAD samples per height layer across the volume's velocity
/// tilts, then fits each layer with one trimmed re-fit so folded bins in the
/// raw (not-yet-dealiased) sweeps cannot drag the wind estimate.
///
/// # The fit is in the ground frame
///
/// Each sample carries the azimuth **the antenna was pointing at**, off the
/// radial. Nothing in [`VelocitySweep`] bins rows into a north-referenced
/// grid: `azimuths_deg` is filled from `Radial::azimuth_angle_degrees` in
/// sweep order, and a WSR-88D starts each cut wherever the previous one
/// ended — the fifteen velocity cuts of KCRP volume 2017-08-26 04:41:14
/// begin at 11.3°, 47.2°, 85.2°, 107.6°, … 104.5°, marching most of the way
/// round twice.
///
/// Row *index* would be a different angle in every cut. `u sin(θ) + v cos(θ)`
/// fitted against `θ = 2πi/n` returns the true wind turned by that cut's own
/// start azimuth, so pooling the volume's cuts into one layer — which is the
/// whole point of pooling, the reason a layer at 1 km holds samples from four
/// tilts at four ranges — averages winds that disagree by tens to hundreds of
/// degrees. Measured on that KCRP volume: the 0–6 km sample-weighted trimmed
/// RMS residual of the pooled fit is 4.85 m/s against azimuth and 5.48 m/s
/// against index, and on KDMX 2022-03-05 23:23:24 the 1.05 km layer alone
/// reads 3.55 m/s against 5.81. The model that explains the gates is the one
/// whose angle is the one the gates were measured at.
///
/// Within a single cut the error was invisible, and that is worth stating
/// because it is why nothing looked wrong: [`WindProfile::predict`] is
/// queried by the dealiaser at the same azimuth the fit was given, so a fit
/// turned by `az0` and a query turned by `az0` cancel and the predicted
/// radial velocity is right. What does not cancel is any reader of the
/// profile's `(u, v)` as a wind — the Bunkers storm motion under SRV
/// ([`crate::srv::bunkers_right_mover`]) is one, and it is a vector the user
/// reads off the pane.
#[derive(Default)]
pub struct WindProfileBuilder {
    samples: Vec<Layer>,
}

/// One height layer's accumulated VAD samples, thinned to
/// [`PROFILE_MAX_SAMPLES`] as they arrive.
struct Layer {
    /// (sin·cosθ, cos·cosθ, vr).
    pts: Vec<(f64, f64, f64)>,
    /// Samples offered so far, counted so `stride` can be applied to them.
    offered: usize,
    /// One offer in this many is kept. Doubles each time the layer fills.
    stride: usize,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            pts: Vec::new(),
            offered: 0,
            stride: 1,
        }
    }
}

impl WindProfileBuilder {
    pub fn new() -> Self {
        Self {
            samples: (0..PROFILE_LAYERS).map(|_| Layer::default()).collect(),
        }
    }

    /// Hand one sample to a layer, keeping one offer in `stride` and halving
    /// the layer whenever it fills.
    ///
    /// # Why not simply stop at the cap
    ///
    /// A layer fills in the order the volume is walked — cut by cut, row by
    /// row — so stopping keeps a *prefix*, and a prefix of a sweep is an arc.
    /// KCRP 2017-08-26 04:41:14 filled its 0.15 km layer partway through the
    /// second of fifteen cuts: the samples that layer was fitted from spanned
    /// 237.5° of azimuth with a 122.5° hole in them, 0.45 km spanned 282.5°
    /// and 0.75 km 315.5°. Those three are the layers the Bunkers 0–0.5 km
    /// head band and a sixth of its 0–6 km mean wind are read from.
    ///
    /// An arc still determines a VAD fit, so this was a conditioning cost
    /// rather than a wrong answer, and it is worth the size it is and no more.
    /// Against the same fit with the cap lifted altogether, the shipped
    /// right-mover moved 5.4 kt and 6.4° on KMSX 2022-06-04 20:05:58, 3.9 kt
    /// and 7.0° on that KCRP volume, and 2.1 kt and 0.9° on KDMX
    /// 2022-03-05 23:23:24 — a tenth of the rotation error the same three
    /// layers carried while the fit read row index, and still a vector a user
    /// reads.
    ///
    /// Thinning costs one halving per doubling of the offer count — four or
    /// five per layer for a super-res volume — and leaves the layer holding a
    /// uniform one-in-`stride` sample of the *whole* volume, so its azimuth
    /// coverage is the volume's. `offered` deliberately keeps counting across
    /// a halving: the kept samples sit at offers 0, 2·stride, 4·stride…, which
    /// is exactly the progression the doubled stride continues.
    fn offer(&mut self, l: usize, sample: (f64, f64, f64)) {
        let layer = &mut self.samples[l];
        let keep = layer.offered.is_multiple_of(layer.stride);
        layer.offered += 1;
        if !keep {
            return;
        }
        layer.pts.push(sample);
        if layer.pts.len() == PROFILE_MAX_SAMPLES {
            layer.pts = layer.pts.iter().step_by(2).copied().collect();
            layer.stride *= 2;
        }
    }

    pub fn add_sweep(&mut self, sweep: &VelocitySweep, elevation_deg: f64) {
        let el = elevation_deg.to_radians();
        let (sin_el, cos_el) = (el.sin(), el.cos());
        // `zip`, not an index: the azimuth slice is the caller's and this
        // module already declines to assume it is `vel_grid.len()` long
        // (`sweep_rows` takes the row count separately, for the same reason).
        // A row the sweep named no azimuth for contributes no sample, which is
        // the same answer a row of all-NaN gates gives.
        for (row, &az_deg) in sweep.vel_grid.iter().zip(sweep.azimuths_deg) {
            let az = az_deg.to_radians();
            let (s, c) = (az.sin() * cos_el, az.cos() * cos_el);
            // Every 3rd gate is plenty for a 3-parameter fit per layer.
            for (j, v) in row.iter().enumerate().step_by(3) {
                if v.is_nan() {
                    continue;
                }
                let r = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
                let h = height_km_with_sin_el(r, sin_el);
                let l = (h / PROFILE_LAYER_KM) as usize;
                if l < PROFILE_LAYERS {
                    self.offer(l, (s, c, *v));
                }
            }
        }
    }

    pub fn finish(self) -> Option<WindProfile> {
        let mut any = false;
        let layers = self
            .samples
            .iter()
            .map(|layer| {
                let pts = &layer.pts;
                let mut fit: Option<(f64, f64, f64)> = None;
                for _ in 0..2 {
                    let mut m = [[0.0f64; 3]; 3];
                    let mut b = [0.0f64; 3];
                    let mut n = 0u32;
                    for &(s, c, v) in pts {
                        if let Some((u, w, cc)) = fit
                            && (u * s + w * c + cc - v).abs() > PROFILE_TRIM_MS
                        {
                            continue;
                        }
                        let x = [s, c, 1.0];
                        for r in 0..3 {
                            for q in 0..3 {
                                m[r][q] += x[r] * x[q];
                            }
                            b[r] += x[r] * v;
                        }
                        n += 1;
                    }
                    if n < 200 {
                        fit = None;
                        break;
                    }
                    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
                    if det.abs() < 1e-9 {
                        fit = None;
                        break;
                    }
                    let solve = |col: usize| {
                        let mut mm = m;
                        for (row, mm_row) in mm.iter_mut().enumerate() {
                            mm_row[col] = b[row];
                        }
                        (mm[0][0] * (mm[1][1] * mm[2][2] - mm[1][2] * mm[2][1])
                            - mm[0][1] * (mm[1][0] * mm[2][2] - mm[1][2] * mm[2][0])
                            + mm[0][2] * (mm[1][0] * mm[2][1] - mm[1][1] * mm[2][0]))
                            / det
                    };
                    fit = Some((solve(0), solve(1), solve(2)));
                }
                // The gate the RPG publishes and this did not have. A layer
                // whose samples are a folded mess solves as willingly as a
                // clean one; only the residual distinguishes them, and until
                // now nothing looked at it. See [`PROFILE_MAX_RMS_MS`].
                if let Some((u, w, cc)) = fit {
                    let (mut sq, mut n) = (0.0f64, 0u32);
                    for &(s, c, v) in pts {
                        let r = u * s + w * c + cc - v;
                        if r.abs() > PROFILE_TRIM_MS {
                            continue;
                        }
                        sq += r * r;
                        n += 1;
                    }
                    if n == 0 || (sq / f64::from(n)).sqrt() > PROFILE_MAX_RMS_MS {
                        fit = None;
                    }
                }
                if fit.is_some() {
                    any = true;
                }
                fit
            })
            .collect();
        let mut layers: Vec<Option<(f64, f64, f64)>> = layers;
        // Clamp-extrapolate every unfitted layer from the nearest fitted one,
        // out to [`PROFILE_FILL_MAX_LAYERS`]: winds vary slowly with height,
        // and a None prediction is worse than a near neighbour's — it vetoes
        // every wind seed tile whose beam reaches that height. Measured once:
        // without the extension most of the reference's far band is lost. The
        // far-band counts behind that sentence are gone — branch
        // `campaign-harness` has the probe that would count them again, not
        // the count.
        //
        // The bound is what `from_levels` has always applied and this did not.
        // Unbounded, "the nearest fitted layer" could be eight kilometres away
        // and the profile answered all 40 layers whenever any one of them fit
        // — which is why `srv::BUNKERS_MIN_MEAN_LAYERS` could never refuse.
        let filled: Vec<usize> = (0..layers.len()).filter(|&l| layers[l].is_some()).collect();
        for l in 0..layers.len() {
            if layers[l].is_none()
                && let Some(&f) = filled
                    .iter()
                    .min_by_key(|&&f| (f as i64 - l as i64).unsigned_abs())
                && (f as i64 - l as i64).abs() <= PROFILE_FILL_MAX_LAYERS
            {
                layers[l] = layers[f];
            }
        }
        any.then_some(WindProfile { layers })
    }
}

/// The fold limit read **off the data**: the largest speed the sweep observed.
///
/// The fallback, not the answer. `nexrad_model::data::Radial` drops the RDA's
/// declared Nyquist velocity, so for a long time this was the only number
/// available; [`crate::nyquist::DeclaredNyquist`] now carries the declaration
/// past the model boundary and [`fold_limit_ms`] prefers it wherever a volume
/// made one. This still stands in for a volume that declared nothing — every
/// Message 1 volume (the legacy message has no such field), every fixture, and
/// any caller holding only model types.
///
/// It is exact when the sweep folded at all, because folded data reaches the
/// limit by construction, and an **under**estimate when it did not. The
/// underestimate is what makes it a fallback rather than a peer: on a calm
/// sector whose fastest gate is 6 m/s it returns 6, and 2·6 m/s then becomes
/// the interval every fold decision below is a multiple of — so ordinary shear
/// across a 12 m/s step reads as a fold and comes back unfolded by a step that
/// was never there. [`crate::sampler::FOLD_LIMIT_FLOOR_MS`] is the floor that
/// stops the worst of it; the declaration removes the failure mode outright.
fn estimate_nyquist(vel_grid: &[Vec<f64>]) -> f64 {
    vel_grid
        .iter()
        .flatten()
        .filter(|v| v.is_finite())
        .fold(0.0_f64, |a, &v| a.max(v.abs()))
}

// ————————————————————————————————————————————————————————————————————
// Step 2: range-dependent median filter
// ————————————————————————————————————————————————————————————————————

/// Half-width of the median kernel's azimuthal footprint, in km. The window
/// narrows from 5×5 toward 3×3 with range, so instead of switching at one
/// fixed range the radial count follows a constant physical footprint, capped
/// at 5 radials and floored at 3.
///
/// This is the one azimuthal scale in the module that is a **distance** and
/// not a count of rows, and so the one that divides the other way: the
/// stencils' taps sit at row offsets and their divisors count rows
/// ([`split_stencil_rot`]), while this counts rows *from* an arc, so a
/// coarser sweep gets fewer of them. Measured, at 20 km: five rows spanning
/// 2.5° on a 0.5° sweep, three spanning 3.0° on a 1.0° one — the same half
/// kilometre of sky either way, until [`MEDIAN_AZ_HALF_MAX`] and the floor of
/// one bound it.
const MEDIAN_HALF_WIDTH_KM: f64 = 0.4;

/// Cap on the median filter's azimuthal half-count. It is what stops
/// [`MEDIAN_HALF_WIDTH_KM`] from scaling: four-tenths of a kilometre of arc
/// is three rows at 9 nm and four at 7 nm, and this holds both at two.
///
/// # The cap is a couplet guard, and a couplet now says so
///
/// It was set against the reference median's couplet erasure and near-radar
/// couplet amplitudes — a 5×5 window counted in legacy 1° radials ≈ 9
/// super-res — none of which was kept; branch `campaign-harness` has the
/// generator that paints those couplets and nothing the reference read off
/// them. So the number stood on a method and not a result.
///
/// It now stands on the KFTG 2023-06-22 03:46:11 mesocyclone, which is the
/// thing the guard is for. Raising the cap erases it:
///
/// ```text
///     cap   KFTG core                     KTLX   KCRP   KDMX   KFTG  KMSX
///      2    +1.68 +1.50 +1.59 +1.75       1656   5616   2938   1571    34
///      3    +0.27 +0.40    ND    ND       1282   5355   2875   1460    23
///      4    +0.27 +0.40    ND    ND       1275   5353   2875   1456    23
/// ```
///
/// against the reference's +1.64 +1.52 +1.56 +1.76. Two of the four core bins
/// go ND outright and the other two fall to a sixth of what the reference
/// reads, because a mesocyclone's poles are a few rows apart and a median
/// seven rows wide outvotes them. KHNX stays at 0 either way and KLOT at 72.
///
/// The core alone refuses it, but the precision case for raising it fails
/// too, and that is worth recording because azimuthal smoothing is the axis
/// the over-paint measured at [`SIGNIFICANT`] responds to most: against the
/// reference's own decoded field at KTLX, cap 3 leaves 493 agreeing bins
/// where cap 2 leaves 661, at a precision of 0.416 against 0.424. It costs
/// agreement faster than it buys precision. What it does buy is quiet — bins
/// over |1.0| fall from 3.1× the reference's count on the same ground to
/// 1.9× — which is the clearest evidence there is both that the over-paint is
/// a smoothing question and that a median is the wrong instrument for it.
const MEDIAN_AZ_HALF_MAX: i32 = 2;

/// Half-depth of the median kernel in range gates — deliberately deeper than it
/// is wide. Range is the axis this module does *not* differentiate, so smoothing
/// along it removes noise without touching the azimuthal shear being measured.
/// The depth is empirical: 2 gates agreed with reference readouts better than
/// 1 on amplitude, correlation and painted density. Which readouts, and by how
/// much, is no longer recoverable — branch `campaign-harness` preserved this
/// campaign's harness and not the hovering it did.
const MEDIAN_RNG_HALF: i32 = 2;

/// Minimum fraction of the median window that must carry **echo** — a gate the
/// radar returned a number for — for a valid centre to survive: the reference
/// NDs under-populated windows, cleaning sparse fold soup the raw-default
/// dealias rule re-admits. The fraction is empirical, and like the rest of the
/// median geometry it was fitted against readings the campaign scratchpad did
/// not outlive: branch `campaign-harness` says how this was measured, never
/// what came back.
///
/// # It is not a coverage rule, and the census is why
///
/// This constant used to be documented as asking whether the radar *sampled*
/// that sky — "censored fold walls carry raw data and must not deplete the
/// window, only genuinely missing samples do". It never asked that. Its key
/// was the finiteness of a raw cell, and until [`crate::types::GateReport`]
/// existed a `NaN` there meant three different things, so a below-threshold
/// gate — the radar looking at that sky and finding no scatterers — counted as
/// *not sampled*, which is the opposite of what the sentence claims.
///
/// The sentence is not merely wrong, it is **unachievable**. Over the
/// 7.05–20 nm annulus the reference comparison scores, on the lowest Doppler
/// cut of all nine of that comparison's volumes,
/// [`GateReport::NotReported`](crate::types::GateReport::NotReported) is
/// **0.0000%** of the annulus — every cell, at every site, was sampled — while
/// [`BelowThreshold`](crate::types::GateReport::BelowThreshold) runs 0.57%
/// (KCRP) to 85.72% (KLOT). So a rule keyed on "did the radar look" is keyed
/// on a constant: re-keyed on
/// [`is_measured`](crate::types::GateReport::is_measured), the window's
/// occupancy is `slots`/`slots` at every centre of every one of those nine
/// volumes, and the cliff is not a cliff but a deletion. That was measured, not
/// argued: NROT grids taken with the re-keyed fraction at 0.0, at 0.6 and at
/// 1.00 are byte-identical to each other at all nine sites, and differ from
/// this tree at seven of them.
///
/// What the deletion costs is why the key stayed on echo, and the number that
/// decides it is the **marginal** precision — of the bins an arm *adds*, how
/// many the reference also paints. Overall precision can rise on a change whose
/// every new bin is wrong, so the aggregate is not the test. Against the
/// decoded reference, over the eight deciding sites, the re-key adds 70 bins
/// and loses none, and 19 of the 70 agree: **0.271**, against the **0.726** the
/// field already stands at. Per site it is 0.125 at KTLX, 0.152 at KDMX, 0.684
/// at KFTG and **0.000** at KATX, where all ten added bins are ones the
/// reference reads ND at. The holdout says the same and is the only site that
/// also loses a bin: 109 added, 1 lost, marginal 0.303. So overall precision
/// falls everywhere anything moves — KTLX 0.4451→0.4436, KDMX 0.7619→0.7483,
/// KFTG 0.8394→0.8362, KATX 0.9211→0.9019, KDDC 0.7913→0.7728 — and mean |Δ|
/// on agreeing bins improves nowhere. The one middle key that is neither
/// vacuous nor the shipped one — counting
/// [`RangeFolded`](crate::types::GateReport::RangeFolded) as echo, on the
/// argument that ambiguous range is still signal — is the same trade smaller
/// and no better shaped: +24 bins, marginal 0.500, still 0.000 at KATX, and
/// KFTG's mean |Δ| rises 0.058→0.060.
///
/// # KHNX is not a control for this rule
///
/// The reference's KHNX paint is adjudicated spurious and this module's zero
/// there is correct, which makes "does an arm start painting at KHNX" a
/// tempting red flag to watch. It cannot fire here. The cliff *is* live at
/// KHNX — it refuses 1098 of that cut's 62 272 dealiased annulus cells, and
/// re-keying releases 973 of them — but **100% of KHNX's median cells lie on
/// ground [`incoherent_velocity`] has already set aside**, at both keyings, so
/// [`llsd_nrot`] carries 0 cells there whatever this gate decides. Its zero is
/// held by the incoherence mask, upstream of anything this constant can move. A
/// control that cannot fail is not a control; the holdout and the per-site
/// marginal precisions above are what decided this.
///
/// So the rule is an **echo-coverage** rule and always was: a window three
/// fifths empty sky is a filament of weather, and a median along a filament is
/// a median of whatever the filament threads between. The key now says so, on
/// the plane, by naming [`Value`](crate::types::GateReport::Value) — which is
/// the arm the finiteness test always selected, so this is the same number at
/// the same operating point, and the grids above pin that at all nine sites.
const MEDIAN_MIN_RAW_OCC: f64 = 0.6;

/// Minimum fraction of the median window that must still **carry a dealiased
/// value** for a median to be reported at all.
///
/// [`MEDIAN_MIN_RAW_OCC`] asks whether the radar sampled this sky, and counts
/// raw cells on purpose, so that a censored fold wall — a thin line
/// [`CENSOR_VNY_FRAC`] removes from an otherwise complete neighbourhood — does
/// not deplete a window the radar did fill. That reasoning is right about a
/// wall and says nothing about a *region*. Where the censor has taken most of
/// the window, what is left is not a sample of the sky: it is the censor's
/// leftovers, and they are selected for standing on opposite sides of the jump
/// the censor found. A median over that set is a median of two fold branches,
/// and its value need lie in neither of them.
///
/// # The bin that measured it
///
/// KCRP 2017-08-26, az 73.26°, 9.52 nm. The wind sits at the fold, the raw
/// sweep is a ±30 checkerboard against a declared 31.52, and the censor cuts it
/// to a filament: the dealiased window holds **9 of its 25 cells**, reading
///
/// ```text
///     31   30   -31   30   30   0   -30   -30   0
/// ```
///
/// Sorted, that has median **0.00** — at a bin whose own dealiased value is
/// **+30.00**, and with no cell of the window within 29 of the answer. The
/// stencil then differentiates the ramp that leaves, and reads −4.776, the
/// largest magnitude this module produced anywhere on the nine decoded cuts
/// where the second largest is 2.14. GR2Analyst hovered there reads **+0.18**.
///
/// # Where 0.37 comes from
///
/// Two-sided, measured through the pipeline at all nine decoded sites, painted
/// and agreeing bins over the 7.05–20 nm annulus:
///
/// ```text
///     floor       0.32    0.33 .. 0.40    0.41    0.45
///     KCRP     941/905         935/905  935/905  935/905
///     KDMX    1453/1107       1453/1107 1452/1107 1450/1106
///     KTLX     1640/730        1640/730  1640/730  1639/730
///     KDDC    2731/2161       2731/2161 2731/2161 2730/2161
/// ```
///
/// Below 0.33 the rule does not fire at all; from 0.41 it starts taking bins
/// off sites it has no business touching. Across the whole plateau the six
/// remaining sites are **bit-identical**, so 0.37 is the middle of it with
/// about a tenth of its own value in hand on each side — the two-sided form
/// [`COH_MAX_STRADDLE`] is chosen by.
///
/// # What it moves
///
/// Six bins at KCRP, and nothing else anywhere. All six are bins the reference
/// reads ND at, so **no agreeing bin is lost at any site** — the failure mode
/// that refused the three magnitude guards a sibling swept, each of which cost
/// KTLX between 24 and 380. KCRP's spurious count falls 36 → 30 and its
/// precision rises 0.9617 → 0.9679, with the mean magnitude of what it still
/// over-paints falling 0.729 → 0.281 and the count of those at or above 0.42
/// falling 7 → 1. The four bins above are gone, and the largest magnitude this
/// module reports anywhere on the nine cuts falls **4.7759 → 2.1445**: nothing
/// on any of them now exceeds |2.2| at all. KTLX holds 1640 painted, 730
/// agreeing and 0.4451 precision to the digit, KHNX holds 0, the KFTG
/// 2023-06-22 core holds +1.68 +1.50 +1.59 +1.75, and the 216-rung scorecard
/// holds 195 rungs and 155 cells exactly right.
///
/// # A circular median is the estimator this asks for, and it is worse
///
/// Velocity near the limit is circular — at 31.52 the +31 and the −31 above are
/// 1.04 m/s apart, not 62 — so the linear median is formally the wrong
/// estimator and a circular one is the right one. It was built and measured
/// four ways: anchored on the centre's own branch, anchored only where the
/// dealiaser had claimed no branch, restricted to the cells it had not claimed,
/// and unanchored (the rotation of the sorted phases minimising the sum of
/// circular distances). At KTLX, whose field the dealiaser leaves largely
/// unclaimed, every one of them re-branches ordinary resolved shear — one fold
/// is 22.98 m/s there and a 5 × 5 window reaches it:
///
/// ```text
///                              KTLX painted / agreeing / precision   KCRP worst
///     as shipped                      1640 /  730 / 0.4451              4.7759
///     circular, centre-anchored       2402 /  813 / 0.3385              5.0000
///     circular, unclaimed cells only  1834 /  756 / 0.4122              5.0000
///     circular, unanchored            2289 /  800 / 0.3495              5.0000
///     this floor                      1640 /  730 / 0.4451              2.1445
/// ```
///
/// Each costs KTLX a fifth of its precision, and each drives KCRP's worst bin
/// **to the ±[`NROT_LIMIT`] clamp** rather than away from it: fixing the
/// estimator at az 73.26° moves that bin's median to the +30.00 it should be,
/// but its neighbours at 73.77° and 74.21° are centred on genuine 0.00 gates
/// and anchor there, so the ramp the operator reads gets steeper, not flatter.
/// The branch is a property of the neighbourhood and a per-bin estimator cannot
/// choose it. Refusing the window is what is left.
///
/// # Its key is loose, and the loose part is not a missing channel
///
/// [`MEDIAN_MIN_RAW_OCC`] records a rule whose key did not match its sentence.
/// This one has the same shape of gap and a different cause, so it is worth
/// stating exactly. The sentence above is about the **censor**: what is left
/// when the censor has taken most of the window is the censor's leftovers. The
/// key is `window.len()` — cells carrying a dealiased value — and that counts
/// two removals as one. A cell can be absent from the dealiased field because
/// the censor cut it, or because the radar found no echo there and it was never
/// in the raw sweep either.
///
/// Over the same annulus and the same nine cuts, of the window cells this rule
/// does not count, the fraction that never carried echo — nothing to do with
/// the censor — is 45.7% at KCRP, 75.2% at KTLX, 76.6% at KDMX, 85.0% at KDDC,
/// 89.7% at KLOT, 90.8% at KATX, 94.1% at KFTG, 98.2% at KMSX and **100%** at
/// KHNX, where the censor removes nothing at all. Keyed on the censor alone —
/// refuse where the censor has taken more than 1 − 0.37 of the window — the
/// rule would fire on **none** of the centres it currently fires on at seven of
/// the nine sites, on 2 of 107 at KDMX and 2 of 10 at KATX, and on 71 of 76 at
/// KCRP. Which is to say: it does the work its sentence describes at the one
/// site the sentence was written from, and everywhere else it is a second, much
/// stricter echo-coverage rule standing behind [`MEDIAN_MIN_RAW_OCC`].
///
/// **It is not a claim rule.** It does not mean "a pass placed this gate on a
/// branch", and it must not be re-keyed to mean that: `dealias` never writes a
/// value into a gate the raw sweep left empty — 0 such cells in the annulus at
/// all nine sites — so the cells it counts are exactly the raw cells the censor
/// spared, and placement is a different fact about them. A key on placement
/// would be vacuous where it matters most: over that annulus the dealiaser
/// moves 0 gates at KHNX and 0 at KMSX, 7 at KDDC, 8 at KLOT, 41 at KATX and
/// 57 at KDMX, against 18 559 at KCRP — the same kind of near-constant
/// statistic that made re-keying [`MEDIAN_MIN_RAW_OCC`] a deletion. (And a
/// *moved value* is itself only a proxy for a placement: a pass that puts a
/// gate on branch 0 leaves the number where it found it.)
///
/// So a `DealiasClaims` channel is **not** justified by this rule, and the
/// separation the rule actually wants needs no channel at all: a cell the
/// censor took is a cell whose raw value is finite and whose dealiased value is
/// not, which both grids in [`median_filter`]'s hand already say. Splitting the
/// key that way moves painted pixels — it would stop the rule firing on 284 of
/// the 359 centres it fires on across the nine cuts — so it is a measurement of
/// its own and not a comment change.
const MEDIAN_MIN_DEALIASED_OCC: f64 = 0.37;

/// The coverage rule this filter is often blamed for costs nothing where it is
/// blamed, and the rule it does **not** have is what costs.
///
/// At the thirteen ring points the reference paints at KTLX 2025-02-19 and
/// this module does not, the occupancy cliff above refuses **none**; relaxing
/// it to zero moves KTLX from 1227 painted bins to 1246 and the 7.05–20 nm
/// annulus from 1.63% to 1.65% against the reference's 3.27%.
///
/// What separates the two fields is the line below it. The reference **fills
/// a missing centre from its window**, and the hole ladder on [`tap_stencil`]
/// measures it over seven sites. Re-run against this tree, over the holed
/// sectors only, points where both fields carry a value / points the reference
/// paints and this module reads ND / points only this module paints:
///
/// ```text
///                              both   ref only   ours only   mean |Δ|
///     as shipped                826       292          18      0.075
///     fill the median centre   1102        16          21      0.080
/// ```
///
/// The fill closes the ladder outright — 292 refusals become 16 — and the
/// values it produces are the reference's, to five thousandths of the departure
/// the shipped rule already has. On real volumes it is the largest single
/// coverage move this module has: at KTLX the completeness refusals on
/// [`tap_stencil`] fall from 664 of the reference's bins to **7** and the bins
/// with no median value at all from 387 to 3, and 701 more of the reference's
/// bins come back agreeing (661 → 1362 of 3546). Six of the nine decoded sites
/// gain agreement; the holdout gains most of all, 2161 → 3184 of 5823.
///
/// # Why it is still not taken
///
/// Precision falls at every site — KTLX 0.4240 → 0.3604, the holdout 0.7913 →
/// 0.6350 — and KHNX 2024-12-16 goes from 0 painted bins to 49 in the annulus,
/// of which **none** is one of the 252 the reference paints there. They are not
/// near misses: all 49 stand in clusters the reference never touches, a median
/// **33 bins** from its nearest paint. The null control fails on placement, not
/// on amplitude.
///
/// Three guards return KHNX to zero and the ladder measures all three:
///
/// ```text
///                                       KHNX   ladder both/ref-only
///     raw window occupancy ≥ 0.88          0       826 / 292
///     hole isolated in azimuth and range   0       826 / 292
///     hole isolated in azimuth only       35      1102 /  16
/// ```
///
/// The first two are **bit-identical to the shipped rule on all 70 rungs**:
/// they buy their zero by not filling at all, so they are exactly as wrong
/// about the reference as the rule they replace, and each is a constant KHNX
/// chose. The third is the reference's own behaviour and leaves KHNX painted.
///
/// Nor does the completeness rule at [`SIGNIFICANT`] serve as the missing
/// guard. Asked to refuse the loud paint the fill admits, it needs to reach
/// down to |NROT| ≥ 0.35 before KHNX is empty, and at that setting KTLX ends
/// below where it started; counted against the resolved footprint instead, the
/// pair reaches KTLX 661 → 1062 agreeing at precision 0.4296, KHNX 0, the KFTG
/// core intact and the holdout 2161 → 2388 at flat precision — but only on top
/// of the 0.88 occupancy floor above, which the ladder has just refused. A pair
/// standing on a falsified half is not a pair.
///
/// So the fill is right, admitting it needs a discriminator this module does
/// not have, and the candidate that looked most like one is a number one volume
/// picked. [`COH_MAX_STRADDLE`] was held to lack the same one; it did not —
/// [`COH_FOLD_VNY_FRAC`] found it in the statistic rather than beside it, which
/// is worth trying here too before another axis is looked for.
fn median_filter(
    vel_grid: &[Vec<f64>],
    raw_grid: &[Vec<f64>],
    raw_status: Option<&[Vec<crate::types::GateReport>]>,
    gate_count: usize,
    first_gate_range_km: f64,
    gate_interval_km: f64,
    rows: crate::azimuth::Rows,
) -> Vec<Vec<f64>> {
    let num_radials = vel_grid.len();
    let spacing_rad = rows.step_deg.to_radians();

    (0..num_radials)
        .into_par_iter()
        .map(|i| {
            let mut window: Vec<f64> = Vec::with_capacity(25);
            (0..gate_count)
                .map(|j| {
                    // No NaN fill: a missing centre stays missing.
                    if vel_grid[i][j].is_nan() {
                        return f64::NAN;
                    }
                    let range_km = first_gate_range_km + j as f64 * gate_interval_km;
                    let arc_per_radial = range_km * spacing_rad;
                    let az_half = ((MEDIAN_HALF_WIDTH_KM / arc_per_radial).round() as i32)
                        .clamp(1, MEDIAN_AZ_HALF_MAX);

                    window.clear();
                    let mut slots = 0u32;
                    let mut raw_occ = 0u32;
                    for da in -az_half..=az_half {
                        // A row past the end of a sector is skipped rather
                        // than counted empty, which is what the range axis
                        // does three lines down at the ends of the grid: the
                        // occupancy cliff below asks what fraction of the
                        // cells that exist carry raw data.
                        let Some(ai) = rows.neighbour(i, da) else {
                            continue;
                        };
                        for dr in -MEDIAN_RNG_HALF..=MEDIAN_RNG_HALF {
                            let rj = j as i32 + dr;
                            if rj < 0 || rj >= gate_count as i32 {
                                continue;
                            }
                            slots += 1;
                            // **Echo**, asked of the report plane by name.
                            // `GateReport::Value` — not `is_measured()`, which
                            // is the same question the comment below used to
                            // claim to ask and which [`MEDIAN_MIN_RAW_OCC`]
                            // records as measurably vacuous here. The `None`
                            // arm is a sweep with no plane, where finiteness is
                            // the same predicate rather than a weaker one.
                            let carries_echo = match raw_status {
                                Some(st) => st[ai][rj as usize] == crate::types::GateReport::Value,
                                None => !raw_grid[ai][rj as usize].is_nan(),
                            };
                            if carries_echo {
                                raw_occ += 1;
                            }
                            let v = vel_grid[ai][rj as usize];
                            if !v.is_nan() {
                                window.push(v);
                            }
                        }
                    }
                    // The sparsity cliff tests how much of the window carries
                    // echo: a censored fold wall still returned a number and
                    // must not deplete a window the weather filled. It does
                    // *not* test whether the radar sampled here — nothing in
                    // this annulus goes unsampled, and [`MEDIAN_MIN_RAW_OCC`]
                    // carries the census and what asking that instead costs.
                    if (raw_occ as f64) < MEDIAN_MIN_RAW_OCC * slots as f64 {
                        return f64::NAN;
                    }
                    // And the window has to be a neighbourhood, not what the
                    // fold censor left of one: the survivors of a censored
                    // region stand on both sides of the jump it found, so their
                    // median can land on neither.
                    if (window.len() as f64) < MEDIAN_MIN_DEALIASED_OCC * slots as f64 {
                        return f64::NAN;
                    }
                    window.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
                    let mid = window.len() / 2;
                    if window.len() % 2 == 1 {
                        window[mid]
                    } else {
                        (window[mid - 1] + window[mid]) / 2.0
                    }
                })
                .collect()
        })
        .collect()
}

// ————————————————————————————————————————————————————————————————————
// Steps 3 and 4: azimuthal derivative stencils, range-normalized
// ————————————————————————————————————————————————————————————————————

/// Divisor for the range normalization, range in nautical miles (the unit the
/// callers already carry); converts to the kilometre knot curve.
fn rot_divisor(range_nm: f64) -> f64 {
    rot_divisor_km(range_nm * KM_PER_NM)
}

/// The divisor curve: knot ranges in KILOMETRES, linearly interpolated, flat
/// outside the knots. These are the reference's own values, read off it
/// directly rather than fitted to it.
///
/// # It is a table because the reference's is a table
///
/// The curve was chased for a closed form first, because a divisor that is
/// only a table is a divisor nobody can extend. There is none. Against 808
/// step-edge readings over 60 ranges and six sites, the best two-parameter
/// physical form — the response of this module's own operator to a reference
/// circulation of fixed size, which is what "normalized rotation" means —
/// leaves 272 of them outside the interval the reference's quantisation pins
/// them to, missing by up to 3.7 lattice steps; an exhaustive three-parameter
/// search over `K/(1 + (r/a)^m)^n` does no better. Three
/// features defeat every smooth form: the curve **rises** 6.1% from 13.9 to
/// 20.4 km, steepens again over 55–67 km, and then **flattens** at 8.2 beyond
/// 81 km. The last of those is a table's flat extension, and the shipped
/// 4-knot curve already carried it at 8.0 from 80 km.
///
/// Normalizing to a physical span with a floor and a ceiling — the obvious
/// reading of the rise-then-fall — is refuted rather than unfitted. That span
/// is measurable directly as the velocity jump that reads 1.0: it rises to
/// 22.3 m/s at 48 km and then *falls* 18% to 18.3 m/s by 85 km, all of it
/// inside the range band where this module's operator does not change. A
/// ceiling cannot fall.
///
/// # How these numbers were taken
///
/// Six step edges painted across 8–95 km at three jump sizes, so no site is
/// pushed into the reference's ±5 clamp at short range — which is what broke
/// the previous campaign's 13 km point, where only the two ~11.5 m/s-Nyquist
/// sites were still on scale. A step is invariant under a median filter, so
/// the reading is the operator and this curve and nothing else. Deciding
/// sites KHNX, KLWX, KLOT, KATX, KMSX, KDMX (declared Nyquist 11.3–27.9);
/// holdout KTWX at 35.6 agrees over 60 ranges to a mean of +0.07% and a
/// maximum of 2.09%. Site spread at a range is ≤3.5%.
///
/// The shape is the reference's alone: it comes from its readings and the
/// geometry, and this pipeline enters only as one scale constant — the step
/// gain Σtₖ of [`SPLIT_TAPS`]. That is worth stating as an identity, because
/// it is what decides when this curve has to be re-measured and when it does
/// not. The campaign recorded each reading as `D_ours(r)·(ours/GR)`, and for a
/// step edge `ours = J·(Σtₖ/2)/(arc(r)·D_ours(r))`, so `D_ours` cancels and
///
/// ```text
///   D_GR(r) = J · (Σtₖ/2) / (arc(r) · NROT_GR(r))
/// ```
///
/// — the painted jump, this operator's step gain, the arc, and what the
/// reference read. Evaluated over the campaign's own 808 deciding readings it
/// reproduces all 60 published knots to a mean of +0.12% and an rms of 0.31%.
///
/// So the curve is anchored to Σtₖ and is blind to how the operator
/// distributes it. When the operator's *shape* was corrected, Σtₖ was held at
/// 0.667000 and these knots were re-derived against the new taps and came back
/// unchanged — not approximately, identically. A shape error is one factor at
/// every range, since the operator does not change with range, so none of this
/// curve's structure was ever absorbing one. What pins it is that the
/// reference reports NROT on a 0.0395 lattice
/// (see the module header), so each readout is an interval of half-width
/// 0.0198 and a curve is either consistent with it or is not. These knots
/// leave 38 of those 808 outside, none by as much as one lattice step (worst
/// 0.67); the 4-knot curve they replace left 631 outside and missed by up
/// to 5.6.
///
/// Those counts are a reconstruction, and what one is worth here has a number
/// on it. The campaign took each reading as `D_ours·(ours/GR)` against a
/// dumped field that was not preserved, so they are recomputed from the
/// readings that were, recovering the jump the archive actually painted —
/// velocity is quantised, so it is not exactly k·Vny — as one constant per
/// site and edge. That jump is the whole reconstruction: it scales one site's
/// readings bodily, and the count is steep in it, moving some 20 readings per
/// 0.1%. So a count is quotable only against a stated recovery. This one
/// solves the jumps as a two-way layout, one unknown per site and edge and
/// one per gate, which lands all sixteen within 0.3% of an exact m/s and then
/// snaps them there — that snap is what fixes the single scale least squares
/// leaves free, and it is a fact about the archive rather than a fit to this
/// curve. Recover the jumps instead by matching them to this curve unsnapped,
/// and the 4-knot table, the 17-knot one it replaced and this one score 636,
/// 69 and 57 rather than 631, 50 and 38: the corner below is worth 12
/// readings either way, but the absolute counts do not carry across
/// recoveries. The 71 and 644 that stood here were an unsnapped pass's, which
/// this reduction reproduces as 69 and 636. The population is the 808 the
/// reduction reports; an earlier
/// hand-written count of 776, and a third of 880, appear in no generated
/// artifact and are not reproducible from the logs.
///
/// # Where the flat begins
///
/// The knee is at 81.5 km, and until this knot the table did not say so. The
/// pooled readings fall 2.6%/km over 79.6 → 81.5 km and then 0.27%/km over
/// 81.5 → 85.1: a corner, not a curve. The prose above has said this curve
/// flattens beyond 81 km since the campaign, but the knots ran straight from
/// (80.0, 8.62) to (85.0, 8.23) and chorded across it, riding above every
/// reading in between — +2.4% at the 81.4 and 81.6 km gates and +1.2% at
/// 83.4, against a standard error of 0.3% on each of those gate means. So the
/// band was the table's worst by a factor of two while both knots bounding it
/// were right: at 85.1 km the readings give 8.229 against the table's 8.230,
/// and at 79.6 km 8.701 against 8.698.
///
/// The correction is therefore an added knot, not a moved one and not a
/// restated value. Its range is where the hover actually was, 44.0 nm, and
/// its value is what the six deciding sites pool to there. Over 81–86 km that
/// takes the residual from a mean of +0.86% and an rms of 1.70% to +0.05% and
/// 1.05%, and the readings outside their intervals from 12 to none. Nothing
/// inside 80 km moves, because no knot below the corner changed. KTWX, still
/// the holdout, agrees: over the same band its rms falls from 1.00% to 0.66%.
///
/// The curve now serves one operator at every range. It used to hand over to a
/// second, wider stencil past 80 km whose step gain was 5.4% under the split
/// operator's, so the band beyond read 5.4% under the reference — the curve
/// being right made that visible rather than causing it, and the handover has
/// since been measured not to exist ([`SPLIT_TAPS`]).
///
/// Measured provenance: branch `campaign-harness`.
fn rot_divisor_km(range_km: f64) -> f64 {
    const KNOTS: [(f64, f64); 18] = [
        (13.1, 22.43),
        (16.0, 22.97),
        (19.0, 23.60),
        (22.0, 23.97),
        (26.0, 23.40),
        (30.0, 22.69),
        (35.0, 21.69),
        (40.0, 20.57),
        (45.0, 19.06),
        (50.0, 17.16),
        (55.0, 15.03),
        (60.0, 12.93),
        (65.0, 11.64),
        (70.0, 10.67),
        (75.0, 9.65),
        (80.0, 8.62),
        (81.5, 8.31),
        (85.0, 8.23),
    ];
    if range_km <= KNOTS[0].0 {
        return KNOTS[0].1;
    }
    for w in KNOTS.windows(2) {
        let ((r0, d0), (r1, d1)) = (w[0], w[1]);
        if range_km <= r1 {
            return d0 + (d1 - d0) * (range_km - r0) / (r1 - r0);
        }
    }
    KNOTS[KNOTS.len() - 1].1
}

/// The per-radial operator on a 0.5°-spaced sweep: four taps t₁..t₄
/// at row offsets 1/2/3/4, applied **antisymmetrically** — the same list on
/// both sides of the bin, positive toward increasing azimuth. Zero-sum by
/// construction; normalization is two rows of the grid, which is the legacy
/// 1.0° arc on the super-res grid it was fitted on (see
/// [`split_stencil_rot`], where the difference between those two readings is
/// what makes the operator a derivative rather than a reading of one
/// particular spacing).
///
/// # The reference has no pairing asymmetry to assign here
///
/// This used to be *two* tap lists — `SPLIT_CLEAN` = ĉ at offsets 2/3/4 on
/// the side facing a radial's whole-degree pair partner, these taps on the
/// side away from it — with the pairing phase deciding which side each radial
/// faced. A step landing between pairs then read a flat four-radial core
/// (0.78 ×4 at 21.0 nm on a ±8 m/s step) and one landing inside a pair read a
/// two-radial core with 0.50 shoulders, on the same weather, alternating with
/// absolute azimuth.
///
/// The reference does neither: it reads the shouldered profile at **every**
/// step, whatever the parity. Measured by patching six ±8 m/s step edges into
/// the 30–47 km band of a real volume's 0.5° cut — three at whole-degree
/// azimuths (40.0, 160.0, 280.0), three at half-degree ones (100.5, 220.5,
/// 340.5), which are opposite radial-index parities because super-res radial
/// centres sit at x.21/x.71 — and hovering GR2Analyst's status bar along the
/// 21.0 nm arc at 0.25° steps. All 36 profiles (KLOT VCP 212, KATX 215, KMSX,
/// KHNX, KLWX, and a KTLX holdout; declared Nyquist 8.3–24.0 m/s) read
///
/// ```text
/// −0.18  +0.10  +0.49  +0.77  +0.77  +0.49  +0.10  −0.18
/// ```
///
/// symmetric about the edge, which is these taps on both sides (predicted
/// −0.179/+0.116/+0.518/+0.780) and is *not* any assignment of the old
/// asymmetry: applying the clean side uniformly reads three radials at 0.78
/// and one at 0.50, an unsymmetric profile the reference never shows. A step
/// response determines a zero-sum operator uniquely — its successive
/// differences *are* the taps — so this is a measurement of the operator, not
/// a fit to it. Eighteen ±10 m/s six-radial couplet profiles over the same
/// azimuth classes agree: identical at both parities, flanks +0.30 against
/// this operator's +0.31.
///
/// Nor is the anchor merely off by one: a companion set of volumes with every
/// super-res azimuth shifted +0.5° — which moves the sweep's first radial
/// from the low member of its degree to the high one, and so flips
/// index-parity against floor(az) — reads the same profile again at both
/// KLOT and KTLX. The reference's response is invariant to the pairing, so
/// there is no phase to anchor.
///
/// # This operator is the whole of it, couplets included
///
/// A couplet is two steps, so a linear operator's response to one is fixed by
/// its step response with nothing left to choose. The reference obeys that:
/// 60 hovered couplets — pole widths 2, 3, 4, 5 and 6 radials at both boundary
/// parities, over the same six sites — read these taps' own prediction at
/// every radial of every profile, worst departure 0.04, which is the quantum
/// the reference reports in. 36 asymmetric ones (a width-3 pair with the weak
/// pole at 0.67 and at 0.33 of the strong, both parities, same six sites) read
/// it too, weak flank and all. The readings and what they replaced are at
/// `a_couplet_reads_the_operator_its_own_step_response_fixes`.
///
/// # This is the operator at every range
///
/// It used to stop at 80 km, where a wider 11-radial stencil took over. That
/// handover was ours and not the reference's. Six step edges painted from 8 to
/// 200 km into a real volume's velocity moment, hovered per radial at 0.25°
/// steps along five arcs — 55.6, 85.2, 111.1, 144.5 and 175.9 km — over seven
/// sites (KHNX, KLWX, KLOT, KATX, KMSX, KDMX, and KTWX held out; declared
/// Nyquist 11.3 to 35.6), at two jump sizes and both boundary parities.
/// Eighty profiles, and every one of them paints **four radials each side of
/// the edge and reads ND at the fifth**:
///
/// ```text
///  ND  −0.22  +0.14  +0.69  +1.05  +1.05  +0.69  +0.14  −0.22   ND
/// ```
///
/// (KHNX at 85.2 km; the same shape at every range, scaled by the jump.) The
/// composite stencil spanned ±5 rows, so it painted six radials a side and put
/// −0.20 of the peak where the reference reads nothing — a cell whose
/// magnitude is five lattice steps clear of zero on the 1.60·Vny profiles. It
/// is refused by all 80, at every range and every site. The reference's
/// response never widens, so there is no range at which the operator changes,
/// and the range branch is gone.
///
/// # The shape is a Savitzky–Golay derivative, and the profiles pick it alone
///
/// The same 80 profiles pin the shape and not only the support, because a
/// step response fixes a zero-sum operator uniquely. Normalized to the peak,
/// the reference reads (1, 0.663, 0.150, −0.228). Sweeping candidate shapes on
/// a 0.002 grid and keeping only those inside **every** profile's lattice
/// interval admits one, and it has a closed form: the Savitzky–Golay cubic
/// first derivative over nine points, (126, 193, 142, −86)/1188, ratios
/// (1, 0.6640, 0.1493, −0.2293). Of the 76 profiles whose peak is large enough
/// to resolve a shape, **73 admit it**.
///
/// What this list replaced — 0.238/0.342/0.238/−0.151, ratios
/// (1, 0.643, 0.130, −0.226) — was admitted by **21 of those 76**. It had
/// t₁ = t₃, and that is the whole of the error: the 21.0 nm arc it was solved
/// from has a peak of 19 lattice steps and cannot separate t₁ from t₃, where
/// these profiles reach 175.9 km and run to 78 steps. The deleted
/// `COMPOSITE_TAPS` was a 5-tap fit to this same operator with its outer tap
/// smeared, which is how an 80 km handover that does not exist came to be
/// believed in the first place.
///
/// # Why [`rot_divisor_km`] did not have to move with it
///
/// A hover reports `taps / divisor` and nothing else, so the pair is only
/// fixed up to a common scale and the shape cannot be changed without saying
/// what happens to the divisor. It is written here at the step gain the old
/// list carried — Σtₖ = 0.667000, the same to the last digit — and that is
/// what leaves the divisor exactly where its own campaign put it.
///
/// The reason is that the divisor's readings are **step edges**, and a
/// step edge's peak is Σtₖ and nothing else about the shape: the radial
/// flanking the edge reads Σ_{k} tₖ·(+A −(−A)) over two arcs. Re-deriving
/// that campaign's curve from its own logs makes the cancellation explicit —
/// its `D_GR = D_ours·(ours/GR)` reduces to `J·(Σtₖ/2)/(arc(r)·NROT_GR(r))`,
/// in which our own divisor and grid drop out entirely, and evaluating that
/// closed form over the 808 deciding readings reproduces all 60 published
/// knots to a mean of +0.12% and an rms of 0.31%. So the knots are the
/// reference's readings and the geometry, this operator enters them only
/// through Σtₖ, and holding Σtₖ fixed re-anchors the divisor to itself. None
/// of the curve's three awkward features — the 6.1% rise to 20.4 km, the
/// steepening over 55–67 km, the flat 8.2 past 81 km — was ever absorbing
/// operator shape error; a shape error is one factor at every range, since
/// this operator does not change with range, and could not have produced any
/// of them.
///
/// # What did move
///
/// The ramp gain, Σk·tₖ, goes 1.032 → 1.0565, which is the operator reading
/// constant shear 2.4% higher than it did. That is not free — it is what the
/// shape correction *is* — and three measurements say it is the right
/// direction:
///
/// * The KFTG 2023-06-22 mesocyclone core, the one real-weather anchor there
///   is, reads a mean 1.0062 of the reference's four hovered values against
///   the old list's 1.0094.
/// * The 60 hovered couplet profiles and the 36 asymmetric ones this operator
///   is also gated on (`a_couplet_reads_the_operator_its_own_step_response_fixes`)
///   admit it unchanged, at both parities.
/// * [`LEGACY_TAPS`] is a *separate* measurement on 1.0° cuts, and the two
///   operators must read one physical shear consistently. They agreed to 3.5%
///   before and agree to 1.1% now.
///
/// Painted density inside 80 km over the seven-site set rises 10634 → 11044
/// bins, 3.9%, and it rises in the skirts rather than the cores: the peak is
/// unchanged and the cell two radials out goes 0.130 → 0.149 of it, toward
/// the 0.150 the reference reads. The reference paints all four cells on all
/// 76 profiles; this operator was under-painting the outer two.
const SPLIT_TAPS: [(i32, f64); 4] = [(1, 0.2241), (2, 0.3433), (3, 0.2526), (4, -0.1530)];

/// The operator for a sweep that is *already* legacy resolution — a TDWR cut,
/// or a WSR-88D tilt above the super-res ones. Antisymmetric, at row offsets
/// ±1 and ±2 only, normalized by **one** row: ROT = Σ tₖ(v(i+k) − v(i−k)) /
/// arc_per_radial.
///
/// [`SPLIT_TAPS`] cannot serve here: it spans ±4 rows of a 0.5° grid, which
/// is ±4.0° of sky on a 1.0° one, and it carries a different gain
/// (`one_shear_reads_the_gain_its_own_operator_carries`). These taps are what
/// the reference does instead, hovered per radial off a synthetic 1.0° cut
/// carrying a ±8 m/s step and a ±10 m/s six-radial couplet (measured
/// provenance: branch `campaign-harness`):
///
/// * Its response is the **same at both index parities** — 8 step boundaries
///   and 8 couplets of alternating parity, over KLOT (VCP 212), KATX (215),
///   KMSX (35), KHNX (31), KLWX (32) and a KTLX holdout, Nyquist 11.3 to 24.2
///   m/s, every one reading alike. So there is no asymmetry to assign, and
///   this operator has none.
/// * Its support is **exactly ±2 rows**. A step reads full value on the two
///   radials flanking the discontinuity, tail/core = −0.14 on the next, and
///   nothing beyond; a couplet paints two radials past its poles and nothing
///   past that. Both edges are where a ±2 operator's response is identically
///   zero, so the ND boundary measures the span rather than any gate.
/// * It is **linear** there: the couplet's pole-edge/core ratio is −0.5 with
///   no free parameter under this support, and the reference reads −0.45/0.89
///   = −0.506. A couplet on a 1.0° cut is read by the same operator its step
///   response fixes and by nothing else, which is what a super-res couplet
///   turned out to be as well ([`SPLIT_TAPS`]).
///
/// Twelve hovered readings — a three-range step ladder at 32.2/39.1/45.9 km
/// and the couplet's four distinct classes — fit these two taps with a worst
/// residual of 0.026, under the lattice the reference quantizes its own output
/// on. Their ramp gain, Σ2k·tₖ = 1.0686, is the one number that is *not* the
/// split operator's (Σk·tₖ over two rows, 1.0565): one shear reads 1.1% higher
/// on a sweep collected at 1.0° than on one collected at 0.5°, because the
/// reference's coarse-grid operator is a narrower one and not the same taps in
/// row units.
///
/// That 1.1% is a gap between two independent measurements — these taps off
/// 1.0° cuts, [`SPLIT_TAPS`] off 0.5° ones — and it used to be 3.5%. Most of
/// it was the super-res operator's shape error, and correcting that shape
/// closed it without either measurement being touched. What remains is small
/// enough that a single hovered ramp cannot resolve it, so it is left as read
/// rather than reconciled.
///
/// # The family, and why these are not moved onto it
///
/// [`SPLIT_TAPS`] is the 9-point member of the Savitzky–Golay cubic
/// first-derivative family, so the obvious next claim is that the reference
/// evaluates one closed form at whatever support the radial spacing gives —
/// which would make these its 5-point member, (8, −1)/12, ratio
/// t₂/t₁ = −0.125. That is testable against this ladder, and the ladder does
/// not refuse it: the readings pin A = t₁+t₂ to [0.61489, 0.61633] and
/// B = t₂ to [−0.08967, −0.07287], and the line B = −A/7 the closed form
/// demands runs inside that box, clearing the nearest edge by 9.7% of its
/// width. Adopting it would also close the gap above from 1.1% to 0.1%.
///
/// It is not adopted, because this ladder cannot tell the two apart. The
/// closed form and the value shipped here each leave **0 of the 120 deciding
/// readings** outside their intervals, with the same worst residual — 0.97 of
/// a half-lattice — on the same reading, and likewise for the 20 holdout
/// readings. What the ladder pins is t₂/t₁ ∈ [−0.127, −0.106], an 18% span:
/// wide enough for the closed form, the measured centre, and a great deal
/// else. It is not a vacuous test — it refuses the plain two-point central
/// difference (t₂ = 0), the 5-point *quadratic* derivative and the 7-point
/// cubic one — but between these two candidates it is silent, and a measured
/// constant is not moved onto a closed form that fits no better. Separating
/// them needs tails read at larger amplitude than ±10 m/s at 78.0 nm gives.
///
/// # Scale and shape are one measurement, and it was re-taken
///
/// The ND boundary is the support and the pole-edge ratio the linearity, but
/// the absolute scale is only ever fixed through the product `taps / divisor`,
/// because a hover reports that product and nothing else. So when the divisor
/// curve was re-measured against the reference at 60 ranges these had to
/// follow it, or a reading measured to be right would have moved. They were
/// scaled by one constant — 0.6812/−0.0838 → 0.6996/−0.0861, ×1.0270, the
/// divisor's own change at 39.1 km — and that was a restatement rather than a
/// re-solve: the divisor's change across the ladder's 32.2–45.9 km span runs
/// 1.4% to 6.1%, so no single constant can restate all three rungs.
///
/// They are now solved against the divisor **each rung has**. The ladder was
/// re-taken rather than re-read, because it had to reach past the range where
/// a second operator used to take these bins over: a ±8 m/s step and a ±10 m/s
/// three-radial-pole couplet painted from 8 to 200 km into a real volume's
/// velocity moment, on its first legacy-resolution velocity cut, hovered per
/// radial at 0.35° steps at 32.2, 39.1, 45.9, 85.2 and 144.5 km — two steps
/// and two couplets a volume, at opposite radial-index parity. Deciding sites
/// KLOT, KATX, KMSX, KHNX and KLWX (elevations 1.79–3.53°, declared Nyquist
/// 11.3 to 25.9); KTLX held out.
///
/// Every reading constrains exactly one of two numbers — t₁+t₂, which a step's
/// core and a couplet's core and pole-edge carry, and t₂, which the tails carry
/// — each over its own rung's arc and divisor, so the answer is an
/// intersection of intervals and not a fit. Over 120 deciding readings it is
/// t₁+t₂ ∈ [0.61489, 0.61633] and t₂ ∈ [−0.08967, −0.07287]; these taps are
/// its centre, which is also where least squares over the same readings puts
/// t₂ (−0.0806). Nothing is outside its interval, and the holdout's 20
/// readings are not either — worst 0.97 of a half-lattice. The scaled taps
/// they replace put t₁+t₂ at 0.61350, just under the deciding intersection,
/// and left 3 of the 120 outside; the couplet core was one of them, reading
/// 0.870 against the reference's 0.890 where the interval allows 0.0198. It
/// now reads 0.873.
///
/// The support is the same past 80 km as inside it: at 85.2 and 144.5 km the
/// reference still paints two radials of core and one of tail each side and
/// reads ND beyond, at all six sites. That is what lets this operator serve a
/// 1.0° sweep at every range, now that [`SPLIT_TAPS`] serves a 0.5° one at
/// every range and there is no third stencil past 80 km to take either over.
const LEGACY_TAPS: [(i32, f64); 2] = [(1, 0.6969), (2, -0.0813)];

/// Range half-depth in gates for the stencils' 3-gate range means, per
/// Smith/Elmore's "3 range gates deep" — deeper smooths small features in
/// range and reads them low.
const STENCIL_RNG_HALF: i32 = 1;

/// Coherence floor for both stencils: squared correlation between the
/// velocity profile and the stencil's ramp response; constant or incoherent
/// profiles read ND, matching the reference's ND bins over good velocity.
///
/// # This is not the axis the clear-air over-paint is on
///
/// That was the standing hypothesis here, and the noise ladder refuted it. The
/// bin it was recorded at — KHNX 2024-12-16 08:01:56, az 269.75°, 14.6 km,
/// where this module read −1.49 and the reference reads ND — fits the stencil
/// at r² = 0.315, and the KFTG 2023-06-22 mesocyclone core the reference
/// paints +1.64 at fits it at r² = 0.197. The bin to drop correlates with the
/// operator *better* than the bin to keep, so no floor on this axis separates
/// them, and raising this one only costs real rotation.
///
/// What does separate them is [`GK_MAX_TEXTURE_VNY_FRAC`], which carries the
/// ladder: the reference refuses velocity that is discontinuous along the
/// beam, and reads azimuthal incoherence — which is all r² measures — quite
/// happily. Nor was it an input gate: reflectivity (40 down to −10 dBZ) and
/// spectrum-width (1 to 12 m/s) ladders painted into that same cut read the
/// identical +0.49 couplet in all twelve sectors.
///
/// # Where it sits, measured on the step the operator was measured on
///
/// 0.05 was never read off anything. The six-site step ladder that fixed
/// [`SPLIT_TAPS`] says what it costs: a ±8 m/s edge at six azimuths, hovered
/// along the 21.0 nm arc at 0.25°, over KHNX, KLWX, KTLX, KLOT, KATX and KMSX,
/// declared Nyquist 11.34 to 25.91. The reference carries a value at 575 of
/// those hovers. At 0.05 this module reads ND at **152** of them:
///
/// ```text
///     reference reads   hovers ND at 0.05   at 0.01
///          +0.49                12             0
///          +0.10               124             0
///          −0.18                16            11
/// ```
///
/// Twelve of them are over [`SIGNIFICANT`], so the floor costs painted bins
/// and not only carried ones. The shoulders it refuses are the step response's
/// own — the profile `SPLIT_TAPS` was fitted to — and they fit the stencil at
/// an r² near 0.012: all six sites transfer together between 0.011 and 0.013,
/// which is a step's shoulder having one shape rather than six.
///
/// Recovering them costs no accuracy. Over the whole ladder the mean departure
/// from the reference moves 0.0475 → 0.0485 on 141 more points, and the worst
/// case is 0.400 either way. The 216-rung scorecard stays at 195, with 26 of
/// its cells closer to the reference's hover count and 17 further — 138 cells
/// exactly right before, 155 after. Painted bins move KTLX 1227 → 1230,
/// KCRP 5228 → 5288, KDMX 2944 → 2947, KFTG 1539 → 1571, and KHNX stays 0.
///
/// **This is an upper bound with no measured floor.** Nothing in the corpus
/// shows the gate refusing something the reference also refuses: at KTLX the
/// three bins it admits are three the reference does not paint (+0.38, +0.29,
/// −0.25, all just over the floor), and that is the entire evidence for
/// keeping it positive. It sits at the top of what the ladder admits rather
/// than at zero for that reason and for no stronger one. Exactly constant
/// profiles are refused by the `svv <= 0.0` test beside it and do not need a
/// floor at all; near-constant ones are all this number still speaks for.
const GK_MIN_R2: f64 = 0.01;

/// Extra valid radials required beyond the split stencil's ±4 span on each
/// side. A completeness rule that stops at the stencil's own span doubles as a
/// data-edge noise gate — bins whose support just barely fits sit on echo
/// boundaries where the profile is half real, half edge — and the margin is
/// what keeps those out. Both readers here demand it, so which bins get a
/// value does not depend on which operator reads them.
const GK_DATA_MARGIN: i32 = 1;

/// Range-continuity ceiling. Rotation is reported only over velocity the radar
/// measured continuously along the beam: [`range_texture`] must stay under
/// this multiple of the cut's own fold limit.
///
/// # What the reference refuses
///
/// A noise ladder against GR2Analyst, six sites, hovered along the 21.0 nm arc
/// of the lowest super-res velocity cut at 0.25° steps. Every volume carries six sectors 60° apart,
/// each a width-3 couplet of 0.20·Vny poles — a couplet the reference paints
/// at every rung, so an ND there is a refusal and not a value that rounded
/// under its 0.04 quantum — plus one pseudorandom perturbation at six
/// amplitudes.
///
/// **The axis is range, not incoherence.** A perturbation that varies only
/// across azimuth the reference reads straight through: pure ±10 m/s azimuthal
/// noise at KHNX, no couplet under it at all, reads up to +0.53 and carries a
/// value at 15 of 21 hover points. The same amplitude varying along range as
/// well reads ND at all 21.
///
/// **Nor is it the painted patch.** A perturbation that varies along range and
/// is *common-mode across azimuth* — so the rotation computed from it is
/// exactly the clean couplet's, over exactly the clean couplet's patch, since
/// an antisymmetric operator cancels it identically — is refused at the same
/// amplitude as the two-dimensional one: 0/9 at 8.0 m/s at KHNX and KLWX, 9/9
/// at KMSX, whose limit is 25.91. So what the reference reads is the
/// velocity's own continuity along the beam, whether or not the discontinuity
/// ever reaches the derivative.
///
/// **The ceiling is a fraction of that cut's limit, not a velocity.** Rung 6
/// of the absolute ladder is 8.0 m/s at every site:
///
/// ```text
///                KHNX    KLWX    KLOT    KATX    KMSX
///     Vny       11.66   11.34   23.96   25.32   25.91
///     8.0 m/s    0/9     0/9     8/9     9/9     8/9    painted
/// ```
///
/// while the relative ladder's top rung, 0.45·Vny, paints 8/9 or better at all
/// six — including 16.0 m/s at the KTWX holdout, Vny 35.55, twice the absolute
/// noise that silences KHNX.
///
/// **Where it sits.** A fine ladder, six rungs from 0.45 to 0.75·Vny, read as
/// this module's own [`range_texture`] over the arc actually hovered:
///
/// ```text
///     site   Vny      last kept              first rejected
///     KHNX  11.66  0.397..0.427  7/9      0.462..0.493  0/9
///     KLWX  11.34  0.411..0.424  8/9      0.459..0.487  0/9
///     KLOT  23.96  0.409..0.425  9/9      0.455..0.471  0/9
///     KATX  25.32  0.406..0.421  9/9      0.456..0.473  2/9
///     KMSX  25.91  0.408..0.423  9/9      0.456..0.473  0/9
///     KTWX  35.55  0.408..0.422  9/9      0.456..0.473  0/9   holdout
/// ```
///
/// One rung boundary at every site across a 3.1× span of Nyquist, leaving the
/// gap 0.427..0.455, and this is its middle. The holdout decides nothing and
/// says the same thing louder: 18.13 m/s of range noise is kept there, and
/// 8.0 m/s silences KHNX.
///
/// # Asked of the dealiased field
///
/// Not the median-filtered field the stencils differentiate: that filter
/// exists to remove exactly this, and on it the two real cases are one number.
/// At the bin the reference NDs — KHNX 2024-12-16 08:01:56, az 269.75°,
/// 14.6 km — the median-filtered rms range difference is 1.65–1.81 m/s, and at
/// the KFTG 2023-06-22 mesocyclone core the reference paints +1.64 at, it is
/// 1.58–2.01. Raw they are 6.82–7.88 against 4.93–5.44, which no threshold in
/// m/s separates either.
///
/// It read the raw sweep until [`incoherent_velocity`] landed, because on the
/// raw sweep a fold wall reads as a discontinuity: KHNX's clear air and KCRP's
/// hurricane walls were one number, and holding KHNX cost KCRP its walls. That
/// objection is gone. With incoherent ground handed back as reported, the
/// dealiased field differs from the raw one only where a fold was actually
/// resolved, and a resolved fold is not a discontinuity in the velocity — only
/// in its encoding. Reading it there frees KCRP: 4987 painted bins against
/// 3670, and fourteen of the nineteen hovered bins against four, which is all
/// eleven of the wall bins the reference paints and the raw sweep dropped. The
/// KFTG core stays bit-identical, KHNX stays at 0, and no rung of any
/// synthetic ladder moves — 194 of 216, the same 22.
///
/// # Where the 0.44 comes from on this field
///
/// Not from the ladder, which cannot see it: the ladder's sectors carry no
/// folds, so [`dealias`] leaves them alone and the straddling refusal in
/// [`incoherent_velocity`] already decides every rung the ceiling used to. The
/// scorecard reads 194 of 216 unchanged for every ceiling from 0.12 to 0.80,
/// so the ladder pins nothing here and the number is measured on real weather
/// instead, bracketed from both sides.
///
/// From above by KHNX, whose clear air this ceiling must not readmit. Painted
/// bins inside 80 km on its lowest velocity cut, walked a hundredth at a time:
///
/// ```text
///     ceiling  0.42  0.44  0.45  0.46  0.47  0.48  0.50
///     KHNX        0     0     0     0     4     8    10
/// ```
///
/// From below by KTLX 2025-02-19 15:05:14, where the reference paints far more
/// than this module does and a lower ceiling only widens the gap. Painted
/// fraction of the 7.05–20 nm annulus, the reference read off its own screen at
/// 20 px/nm against this module's own grid over the same annulus:
///
/// ```text
///     ceiling  0.30  0.36  0.40  0.44   reference
///     KTLX    0.56% 1.04% 1.41% 1.62%       3.27%
/// ```
///
/// So 0.44 is inside the bracket with two hundredths of margin under the KHNX
/// knee, which is where the raw sweep's ladder gap (0.427..0.455) also put it.
/// The constant does not move; only the field it is asked of.
///
/// # What this costs, and the sample that hid it
///
/// On the fourteen strongest bins this module paints at KTLX, hovered by the
/// campaign that first tried this field, the trade looks purely bad:
///
/// ```text
///     az           312.7 142.3 252.8 278.3 112.3 244.7  33.8 317.3 …
///     nm            9.65  8.17 10.87  8.17  9.79 16.94  8.30 10.46 …
///     GR           −0.14    ND    ND −1.56 +0.18    ND    ND +0.61 …
///     raw sweep       ND +1.67    ND    ND    ND    ND    ND −1.10 …
///     dealiased    +1.72 +1.67 −1.44 −1.41 +1.31 −1.19 +1.18 −1.10 …
/// ```
///
/// Eleven of the fourteen read ND in the reference, and this field paints all
/// fourteen where the raw sweep painted three. That reading is what kept the
/// ceiling on the raw sweep, and it is a biased sample: it asks whether the
/// reference agrees where *this module* is most confident, which at a site
/// whose field is displaced rather than merely excessive is exactly where it
/// will not be. It cannot answer whether the module paints too much or too
/// little, and the answer is too little. Over the whole annulus the reference
/// paints 3.27% of KTLX against 0.28% of KHNX — the two clear-air volumes at
/// nearly the same Nyquist are twelve times apart in the reference, and 1500
/// times apart in the innermost band, 15.5% against 0.01% at 7–8 nm. An
/// unbiased ring of 240 hovers at 8/11/14/17/20 nm finds the reference painting
/// 13 of them at KTLX and none at KHNX. This module paints 0.88% of that
/// annulus on the raw sweep and 1.62% on this field, so the move is toward the
/// reference by half the remaining distance, not away from it.
///
/// # The difference is read as it stands, not the short way round
///
/// Reading the dealiased field removes most of this question but not all of
/// it: where no pass reached a region, [`dealias`] hands it back as the radar
/// reported it and a wall inside it still reads as ~2·Vny — an artefact of the
/// encoding, while the beam measured something continuous. So the objection
/// survives in the small, that the difference ought to be wrapped into ±Vny
/// before it is squared. It ought not to be, and the
/// instrument that says so is a range **square wave** rather than the wrapping
/// ramp that was tried before. A ramp cannot settle it: adding a couplet ahead
/// of the wrap displaces the wrap boundary in azimuth, so the sector carries a
/// genuine 2·Vny azimuthal step and what the reference reads over it is that
/// step, not the couplet.
///
/// Each of twelve sectors instead carries a couplet of 0.15·Vny poles over a
/// square wave of jump `J` whose phase depends on range alone. Nothing is ever
/// wrapped — |core| + J/2 ≤ 0.99·Vny by construction — so the walls sit at
/// identical ranges at every azimuth, the sector holds no azimuthal step but
/// the couplet's own, and the wave is common-mode across azimuth, so the
/// antisymmetric operator cancels it and the rotation over the couplet is the
/// clean couplet's at every rung. Rung 1 of the first six carries J = 0 and
/// certifies the couplet is painted at all, so an ND elsewhere is a refusal.
/// Painted, of nine points hovered along the 21.0 nm arc:
///
/// ```text
///     walls every 1.5 km    J/Vny  0.00  0.60  1.00  1.35  1.55  1.68
///       raw texture at KHNX        0.00  0.27  0.47  0.64  0.74  0.80
///       wrapped at KHNX            0.00  0.27  0.46  0.31  0.20  0.15
///       KLOT KATX KMSX KTWX         9/9   9/9   0/9   0/9   0/9   0/9
///       KCRP                        9/9   9/9   0/9   0/9   0/9   9/9
///       KHNX                        4/9   9/9   0/9   0/9   0/9   0/9
///       KLWX                        4/9   9/9   0/9   7/9   7/9   5/9
///
///     walls every 0.5 km    J/Vny  0.20  0.35  0.50  0.80  1.20  1.68
///       raw texture at KHNX        0.20  0.37  0.49  0.80  1.18  1.69
///       all seven sites             9/9 6-9/9 8-9/9 5-9/9   0/9   0/9
/// ```
///
/// The wrapped statistic reads the top three sparse rungs at 0.31, 0.20 and
/// 0.15, well inside this ceiling, so it predicts all three painted; five of
/// the seven sites refuse all three and KCRP refuses two. The rungs are the
/// same fraction of a 3.1× span of Nyquist and read the same to a percent at
/// every site — KTWX at 35.55 gives 0.28, 0.47, 0.64, 0.73, 0.79 where KHNX at
/// 11.66 gives 0.27, 0.47, 0.64, 0.74, 0.80.
///
/// Wrapping is refused a second time by arithmetic already on this page: on
/// the noise ladder above no difference comes near 2·Vny, so wrapped and raw
/// agree there to the third decimal and the ceiling stays 0.44 — and at 0.44
/// the wrapped statistic returns KHNX's clear air to 17670 painted bins of the
/// 20878 this ceiling exists to remove. To hold KHNX it would want 0.25..0.30,
/// and the square wave above says 0.44; nothing satisfies both.
///
/// # Where this ceiling and the reference still part company
///
/// The KCRP half is closed. The eleven bins the raw sweep dropped, spread over
/// az 41–140° and 8–39 nm, are all painted here, and their values track the
/// reference's: +0.77 against +0.91, +0.53 against +0.53, +0.49 against +0.48.
/// Their texture falls from 0.55–0.97 of the limit on the raw sweep to
/// 0.09–0.16 on this field, which is the measurement saying what the walls
/// were: aliasing, not shear.
///
/// What remains is that this module paints too little, not too much, and
/// nowhere more than in the near-in clear air. Painted fraction of the
/// 7.05–20 nm annulus against the reference read off its own screen:
///
/// ```text
///                  reference   here
///     KTLX            3.27%   1.62%
///     KCRP            5.08%   1.07%
///     KHNX            0.28%   0.00%
/// ```
///
/// The deficit is not this ceiling's doing and raising it does not close it:
/// at the thirteen ring points the reference paints at KTLX, this module reads
/// ND at nine of them with *every* gate on this page disabled. Those nine were
/// once attributed here to the post-dealias fold censor, the median filter's
/// coverage rule and the stencils' completeness rule. **Two of those three
/// cost nothing.** Instrumented bin by bin at the thirteen:
///
/// ```text
///     already painted                                       1
///     refused by incoherent_velocity                        6
///     velocity dropped by the dealiaser's unreached rule     2   (a third
///                                                                has no raw
///                                                                velocity)
///     refused by the stencils' completeness rule            3   (2 tap
///                                                                cells,
///                                                                1 margin)
///     refused by the post-dealias fold censor               0
///     refused by the median filter's coverage rule          0
/// ```
///
/// The censor costs nothing near here at all: disabling it outright *lowers*
/// KTLX from 1227 painted bins to 1039, because what it removes are walls the
/// derivative would otherwise read as shear. And every hole the stencils
/// refuse at those points is one **this module made** — the raw sweep carries
/// velocity at every one of them and the dealiaser's unreached-region rule
/// dropped it. One dropped cell then costs eleven radials of NROT, because
/// the completeness rule spans ±(4 + [`GK_DATA_MARGIN`]).
///
/// What the reference does with such a hole is measured, and is neither of the
/// two repairs it suggests: [`tap_stencil`] and [`median_filter`] carry that
/// ladder, and why what it says is not yet taken.
///
/// # What it does on real volumes
///
/// Lowest super-res velocity cut, painted bins (|NROT| ≥ [`SIGNIFICANT`])
/// inside 80 km. Ungated → this ceiling, with [`incoherent_velocity`]
/// upstream of both:
///
/// ```text
///     KHNX 2024-12-16  clear air      11 →    0
///     KTLX 2025-02-19              1502 → 1216
///     KCRP 2017-08-26  Harvey      5384 → 4987
///     KDMX 2022-03-05              3287 → 2863
///     KFTG 2023-06-22              1482 → 1467
///     KLOT 2024-12-16                78 →   69
///     KMSX 2022-06-04                32 →   32
/// ```
const GK_MAX_TEXTURE_VNY_FRAC: f64 = 0.44;

/// One antisymmetric tap list applied to an azimuthal profile: Σ tₖ·(v(i+k) −
/// v(i−k)), returned **unnormalized** — the caller divides by the arc its own
/// taps are anchored on, which is the only thing that separates the two
/// operators here.
///
/// `None` when the data margin is not met, when a cell some tap reads is
/// missing, or when the profile does not correlate with the stencil. Those
/// three rules are written once on purpose: which bins get a value is a
/// property of the profile, not of which tap list read it, and a sweep must
/// not gain or lose painted bins by being read at one resolution or the other.
///
/// # The reference has no completeness rule, and this is not how to drop one
///
/// A hole ladder settles the first half. Twelve sectors of a real volume's
/// lowest cut carry the same ±0.80·Vny step edge in the 30–47 km band — six on
/// whole-degree edges, six on half-degree ones, so each holed sector has a
/// clean control at its own sub-radial phase — and five sectors per phase
/// punch **one whole radial** to ND at offsets 5, 6, 7, 8 and 10 from the edge.
/// Hovered along the 21.0 nm arc at 0.25°, over KHNX, KLWX, KTLX, KLOT, KATX,
/// KMSX deciding and KTWX held out; declared Nyquist **11.34 to 35.55**.
///
/// A rule needing every cell of ±(4 + [`GK_DATA_MARGIN`]) blanks offsets
/// o−5 … o+5, so the ND boundary must march one radial per rung. It does here
/// — the hole costs 6, 4, 2, 2 painted radials at o = 5, 6, 7, 8, identically
/// at every site. **The reference loses nothing at any rung**: over 70 rungs
/// its profile through the hole is the clean sector's, radial for radial and
/// value for value, and the one radial that ever differs also differs at
/// o = 10, where no candidate rule reaches. So the reference paints straight
/// through a hole in its own footprint.
///
/// Dropping the tap **pair** the hole breaks and renormalizing by the
/// derivative's own gain — the obvious repair, and the one that keeps the
/// operator antisymmetric — recovers much of that coverage and gets the values
/// wrong. Scored point for point against the reference over the holed sectors:
///
/// ```text
///                        agree with GR   GR paints, we ND   mean |Δ|   max
///     as shipped              633              485           0.074    1.37
///     drop the broken pair    773              345           0.201    2.85
///     fill the median centre  827              291           0.077    1.37
/// ```
///
/// The middle row is 140 more painted bins whose mean error is 0.78 — twenty
/// half-lattices — and a worst case of 2.85, which is a fabricated tornadic
/// couplet. It is refused for that reason and not for its coverage.
/// [`median_filter`] carries the bottom row, which is the reference's own
/// answer and is blocked elsewhere; re-run against this tree the two rows that
/// survive read 826 / 292 and 1102 / 16, at 0.075 and 0.080.
///
/// # And nine real volumes say the same thing without a synthetic
///
/// The ladder is twelve patched sectors of one cut. The reference's whole
/// decoded field at nine sites ([`SIGNIFICANT`]) asks the same question of real
/// weather: for every bin of the annulus, how many of the 33 cells this
/// operator would read carry raw velocity, and does the reference paint there.
/// Its paint rate, full footprint against a footprint missing one to three
/// cells:
///
/// ```text
///                KTLX   KCRP   KDMX   KFTG   KATX   KDDC   KHNX
///     33 cells   11.3%   5.3%   5.2%   2.7%   2.0%  25.4%   0.8%
///     30–32       9.0%  16.1%   7.9%   3.0%   2.4%  27.8%   0.2%
/// ```
///
/// At six of the nine it paints **more** often where this operator is missing
/// cells than where it is not, three times as often at KCRP. The rate only
/// falls away below about 24 of 33 and vanishes below 12. And at the bins where
/// this module has no velocity at all to read, the reference still carries a
/// value at 1.2–14.5% of them — 387 bins at KTLX, 467 at KDDC, 191 at KCRP.
/// The reference is not tolerating an incomplete footprint; it is computing
/// from one.
///
/// # Computing from one was implemented, and its bins are not the reference's
///
/// Smith & Elmore (2004) P5.6 — the LLSD azimuthal shear MRMS reports — fits
/// over the samples that are there and renormalizes the weights for the ones
/// that are not. Translated to this tap list that is the least-squares
/// projection of the profile onto the surviving weights, both mean-centred on
/// the cells actually read, rescaled by the whole stencil's weight energy
/// Σtₖ²: numerator and denominator over the same support, which is what
/// renormalizing means and is the only form that never puts a number where the
/// radar reported none. Substituting zero is not available — a below-threshold
/// gate is the radar measuring *no scatterers*, not scatterers at rest, and a
/// zero under a derivative is fabricated shear.
///
/// It is an extension and not a change. The mirrored weights sum to zero, so
/// on a whole footprint the centred fit **is** Σtₖ·(v(i+k) − v(i−k)) and the
/// arithmetic below is left to compute it; the nine dumps come back
/// byte-identical to this tree with the relaxation compiled in and switched
/// off, and no configuration of it loses a painted bin at any site. So what
/// follows is only ever about bins that are ND today.
///
/// What each relaxation adds over the nine decoded fields — a bin the
/// reference confirms, or a bin where it paints nothing at all:
///
/// ```text
///                                    added  confirmed  refused   prec  ≥0.75  orphan
///     drop the ±5 margin only         1011        494      517  0.489     78     247
///     renormalize, 95% of Σtₖ² left    435        210      225  0.483     68     126
///     renormalize, one cell short     1599        707      892  0.442    201     433
///     renormalize, half the taps      2859        985     1874  0.345    435     954
///     both of the last two            7926       1782     6144  0.225   1158    2995
///     the published count rescale     5876       1060     4816  0.180   1231    3479
///
///     the field as it already is      8161       6104     2057  0.748
/// ```
///
/// `orphan` is a refused bin standing in a painted cluster the reference never
/// touches; `≥0.75` counts the refused bins loud enough to draw a couplet.
/// Every row buys bins worse than the ones already here, the ordering is
/// monotone in how much footprint is allowed to be missing, and there is no
/// knee to operate at: even the most conservative rule that renormalizes
/// anything — 95% of the weight energy surviving, so the rescale is barely a
/// rescale — is right 210 times in 435 and leaves 68 loud bins on ground the
/// reference reads as still.
///
/// The last row is the oracle's own formula as written: the numerator over the
/// survivors against a full-support denominator, scaled by the count. It is the
/// worst of the six, and structurally so — over an asymmetric footprint Σtₖ of
/// the survivors is no longer zero, so a *constant* profile reads as shear. The
/// Rankine-vortex harness that recovers 91–106% of a known truth never caught
/// it, because its synthetic carries no holes and the renormalization it
/// validates is multiplication by one.
///
/// The margin and the taps were asked **separately**, because the ±5 cells are
/// read by no tap: they are an echo-edge sentinel, not part of the fit, and the
/// published method has no such rule. Dropping them alone is the top row and
/// the best-behaved thing on this page — and still 517 bins the reference does
/// not have, 78 at |NROT| ≥ 0.75 and 247 in clusters it never touches. Refused
/// on its own evidence, not by association.
///
/// The **coherence gate** below keeps its meaning rather than its arithmetic.
/// It is the squared correlation between the profile and the weights, so over a
/// partial footprint it is that same correlation over the cells actually read,
/// with both centred on that support — recentring the weights is what keeps
/// `Σ w` out of the numerator, and without it the statistic answers a different
/// question at every hole. Wherever the footprint is whole the two definitions
/// coincide, which is why nothing here changed.
///
/// KHNX decides nothing either way: all 62 272 of its annulus bins carrying
/// velocity are set aside by [`incoherent_velocity`] before this function is
/// reached, so its zero is held upstream and no rule on this page can move it.
/// The control is passed, and passed vacuously.
fn tap_stencil(prof: &[f64], taps: &[(i32, f64)]) -> Option<f64> {
    const C: usize = PROFILE_MAX_HALF;
    // Data-margin completeness: the outermost cells must be populated too, so
    // bins do not appear at echo edges where the profile is half real.
    for m in 0..GK_DATA_MARGIN {
        let o = C - m as usize;
        if prof[C + o].is_nan() || prof[C - o].is_nan() {
            return None;
        }
    }
    // Signed weight per profile cell: one tap list, mirrored — positive
    // toward increasing azimuth, negative away from it.
    let mut w = [0.0f64; 2 * PROFILE_MAX_HALF + 1];
    for &(o, t) in taps {
        w[(C as i32 + o) as usize] += t;
        w[(C as i32 - o) as usize] -= t;
    }
    let (mut acc, mut mean, mut nv) = (0.0, 0.0, 0i32);
    for (k, &wk) in w.iter().enumerate() {
        if wk == 0.0 {
            continue;
        }
        let v = prof[k];
        if v.is_nan() {
            return None;
        }
        acc += wk * v;
        mean += v;
        nv += 1;
    }
    // Coherence gate: squared correlation between the velocity profile and the
    // stencil weights. Constant profiles have zero variance — ND.
    mean /= f64::from(nv);
    let (mut svv, mut scc) = (0.0, 0.0);
    for (k, &wk) in w.iter().enumerate() {
        if wk == 0.0 {
            continue;
        }
        svv += (prof[k] - mean).powi(2);
        scc += wk * wk;
    }
    if svv <= 0.0 || acc * acc / (scc * svv) < GK_MIN_R2 {
        return None;
    }
    Some(acc)
}

/// The super-res operator ([`SPLIT_TAPS`]) at one bin, at **every** range.
fn split_stencil_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
    rows: crate::azimuth::Rows,
) -> Option<f64> {
    let mut buf = EMPTY_PROFILE;
    // Off the end of a sector's arc a cell comes back NaN, and the
    // completeness rules read that as the data edge it is.
    let prof = az_profile(
        &mut buf,
        vel_grid,
        i,
        j,
        gate_count,
        PROFILE_MAX_HALF as i32,
        rows,
    );
    let acc = tap_stencil(prof, &SPLIT_TAPS)?;
    // Normalize by two radials of *this* grid — the legacy 1.0° arc on the
    // 0.5° grid the taps were fitted on, and the arc of two rows on any
    // other. The 2 counts rows, not degrees, and that is what makes this a
    // derivative rather than a reading of one particular grid: the taps sit
    // at row offsets, so on a finer grid the numerator spans proportionally
    // less sky and the divisor shrinks by the same factor, and the quotient
    // is the shear either way — measured across 0.5° and 0.25° samplings of
    // one field in `one_shear_reads_the_gain_its_own_operator_carries`.
    // Pinning the divisor to a physical degree instead would double the
    // reading on every grid whose rows are not half degrees.
    //
    // Which grids reach here is the other half of the answer, and it is not
    // "any": a sweep whose rows are already whole degrees is read by
    // [`legacy_stencil_rot`], the operator measured on such a sweep
    // ([`rows_are_half_degree_pairs`]). So the coarse sampling of one field
    // does *not* read what the fine one reads — at any range — and that is a
    // difference of operators, measured against the reference, rather than a
    // divisor. The same test pins it.
    Some(acc / (2.0 * arc_per_radial))
}

/// [`LEGACY_TAPS`] at one bin, for a sweep whose rows are already whole
/// degrees. Same profile, same completeness rule and same coherence floor as
/// [`split_stencil_rot`] — so which bins get a value is unchanged and only the
/// value changes — but symmetric, and normalized by one row rather than two.
///
/// It reads the same [`crate::azimuth::Rows`] the split operator does, for the
/// same reason: a 1.0° sweep is exactly where sectors live — every TDWR cut is
/// one — so past the end of an arc the profile cell stays NaN and the
/// completeness rule below reads the data edge it is.
fn legacy_stencil_rot(
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    arc_per_radial: f64,
    gate_count: usize,
    rows: crate::azimuth::Rows,
) -> Option<f64> {
    let mut buf = EMPTY_PROFILE;
    // The same span the split operator reads, so the data-margin rule tests the
    // same cells and neither operator paints an echo edge the other would not.
    let prof = az_profile(
        &mut buf,
        vel_grid,
        i,
        j,
        gate_count,
        PROFILE_MAX_HALF as i32,
        rows,
    );
    let acc = tap_stencil(prof, &LEGACY_TAPS)?;
    // One row, not two: these taps sit at whole-degree offsets of a sweep whose
    // rows are whole degrees, and the reference's step response is 0.69 at
    // 39 km where two rows would make it 0.35.
    Some(acc / arc_per_radial)
}

/// Whether this sweep's rows pair into whole-degree legacy bins: radials
/// (2k, 2k+1) — or (2k+1, 2k+2) — sharing a degree sector. The pairing is
/// anchored to ABSOLUTE azimuth, not to collection order: a super-res cut's
/// radial centres sit at x.21/x.71 and the two sharing a floor are the pair.
///
/// The answer is one bit, and it chooses an operator rather than a form of
/// one: a 0.5° grid takes [`SPLIT_TAPS`], rows that are already whole degrees
/// take [`LEGACY_TAPS`]. *Which* alignment cohabits — the phase — is computed
/// here because it is how the question is answered, and it is not returned,
/// because nothing downstream may branch on it.
///
/// That is a measurement, not a preference. Hovered across 36 synthetic step
/// edges at both parities over six sites, the reference's super-res step
/// response is the same symmetric profile every time ([`SPLIT_TAPS`] carries
/// the readings), and across 60 couplets — five pole widths at both parities
/// over the same six sites — its couplet response is the same profile too,
/// and is the response [`SPLIT_TAPS`] alone predicts (the width ladder is at
/// `a_couplet_reads_the_operator_its_own_step_response_fixes`). A
/// shift-invariant reference cannot have a parity-dependent implementation,
/// so no reader here takes a parity.
///
/// # False on a sweep with no pairing to find
///
/// The question only has an answer on a 0.5° grid. Two radials 1.0° apart can
/// never share a whole degree — their floors differ by one by construction —
/// so on a 1.0°-spaced sweep both counts come back zero, for whole-degree
/// azimuths, for a sweep offset by 0.37° or 0.5°, and for one jittered ±0.06°
/// (all four measured, in `a_one_degree_sweep_has_no_pair_phase_to_measure`).
/// That is not a bad reading of a real pairing; there is no pairing. Each
/// radial of such a sweep *is* a legacy bin, and the caller reaches for
/// [`LEGACY_TAPS`].
///
/// Answering "paired" there — which this did until the reference was hovered
/// on a 1.0° cut — handed the primary operator's then clean/away asymmetry to
/// `i % 2`, off collection index rather than off azimuth. Two sites make the
/// cost concrete: the same synthetic step at az 100.1° and the same couplet at
/// az 140.1° landed on even indices at KLOT and odd ones at KATX, purely
/// because the antennas began their cuts at different azimuths, and the
/// pipeline read 0.388 against 0.249 across the step and 0.388 against 0.180
/// across the couplet — a factor of 2.2 on the same sky. The reference read
/// 0.69 and 0.89 at both.
///
/// A ragged sweep is deliberately still paired. Only *no* cohabiting pair at
/// either alignment says the rows are whole degrees; a sector or a jittered
/// super-res cut still finds most of its pairs and keeps [`SPLIT_TAPS`],
/// which is the path validated against the reference.
fn rows_are_half_degree_pairs(azimuths_deg: &[f64]) -> bool {
    let n = azimuths_deg.len();
    if n < 4 {
        return false;
    }
    let cohabit = |phase: usize| {
        (0..n / 2)
            .filter(|&k| {
                let (a, b) = (
                    azimuths_deg[(2 * k + phase) % n],
                    azimuths_deg[(2 * k + 1 + phase) % n],
                );
                a.floor() == b.floor()
            })
            .count()
    };
    // A real pairing accounts for *most* of the sweep: on a 0.5° grid one
    // alignment puts every pair inside a degree and the other puts none. A
    // 1.0° grid can still show a few, because an antenna that wanders a few
    // hundredths backwards leaves two consecutive radials on the same side of
    // a degree boundary — 8 such pairs in 180 on a ±0.06° jitter. Requiring a
    // majority separates the two without asking the caller for the spacing.
    2 * cohabit(0).max(cohabit(1)) > n / 2
}

/// The azimuthal half-span every profile reader asks for: the widest stencil's
/// own ±4 plus [`GK_DATA_MARGIN`]. Both operators here demand exactly this and
/// [`az_profile`] is asked for exactly this, which is what makes which bins get
/// a value independent of which tap list reads them.
const PROFILE_MAX_HALF: usize = 4 + GK_DATA_MARGIN as usize;

/// Backing store for one [`az_profile`], sized for [`PROFILE_MAX_HALF`].
///
/// A plain stack array rather than a `Vec`: this is read once per non-`NaN`
/// bin, over ~230 k bins of a sweep, for a fixed-length list of eleven.
type ProfileBuf = [f64; 2 * PROFILE_MAX_HALF + 1];

/// An empty [`ProfileBuf`], for a caller about to hand it to [`az_profile`].
const EMPTY_PROFILE: ProfileBuf = [f64::NAN; 2 * PROFILE_MAX_HALF + 1];

/// Range-averaged azimuthal velocity profile around (i, j): the 3-gate range
/// mean per radial offset −half..=half — the same per-radial samples the tap
/// stencils consume. NaN where a radial has no data in the range window.
///
/// Fills the leading `2·half + 1` entries of `out` and returns them; `half`
/// above [`PROFILE_MAX_HALF`] would be a caller reaching past the widest span
/// any operator here has, and panics rather than silently truncating.
fn az_profile<'p>(
    out: &'p mut ProfileBuf,
    vel_grid: &[Vec<f64>],
    i: usize,
    j: usize,
    gate_count: usize,
    half: i32,
    rows: crate::azimuth::Rows,
) -> &'p [f64] {
    let len = 2 * half as usize + 1;
    let slot = &mut out[..len];
    for (idx, cell) in slot.iter_mut().enumerate() {
        let da = idx as i32 - half;
        let Some(ai) = rows.neighbour(i, da) else {
            *cell = f64::NAN;
            continue;
        };
        let (mut sum, mut n) = (0.0, 0);
        for dr in -STENCIL_RNG_HALF..=STENCIL_RNG_HALF {
            let rj = j as i32 + dr;
            if rj < 0 || rj >= gate_count as i32 {
                continue;
            }
            let v = vel_grid[ai][rj as usize];
            if !v.is_nan() {
                sum += v;
                n += 1;
            }
        }
        *cell = if n > 0 { sum / n as f64 } else { f64::NAN };
    }
    slot
}

/// Half-depth in km of the range window [`range_texture`] reads. Wide enough
/// that a bin's own gate does not decide the question and narrow enough that
/// the window is still one feature.
const TEXTURE_RANGE_HALF_KM: f64 = 1.0;

/// Separation in km at which [`range_texture`] differences velocity along the
/// beam. A physical distance rather than a gate count, because a super-res
/// velocity cut repeats one estimate over two 0.25 km gates and adjacent-gate
/// differences there are structurally zero half the time.
///
/// # How much of the corpus reports that way
///
/// Not an edge case. Of 158 volumes surveyed, 38 — 24% — are long pulse, and
/// every one of them declares 250 m gate spacing while delivering 500 m
/// content replicated exactly 2x: adjacent gates are bit-identical at every
/// even-indexed pair, on all seven moments. `pulse_width == 4` is the
/// discriminator, and it is exact — no disagreement in either direction
/// anywhere in the survey. The VCP number is **not** one, and nothing may key
/// on it: VCP 34 replicates as readily as VCP 31. Py-ART and MetPy decode the
/// same replication at 100.000% agreement, so this is a property of the data
/// and not of a decoder.
///
/// Both volumes this module is tuned against are in that class. KHNX
/// 2024-12-16 08:01:56 — the null control that must paint nothing — and KTLX
/// 2025-02-19 — the reference field [`COH_MAX_STRADDLE`] is measured against —
/// are both VCP 31 `pulse_width = 4`. The KDDC holdout is short pulse.
///
/// Reading the declared grid rather than the 500 m samples under it costs
/// nothing measurable. Across the corpus the straddling fraction moves by a
/// mean of 0.013% and a max of 0.036%, and the refused fraction by at most
/// 0.00066 percentage points: KHNX 92.98% against 93.00%, KTLX 14.26% against
/// 14.26%. No threshold flips, and [`COH_MAX_STRADDLE`]'s own measured margins
/// are three orders of magnitude wider. Collapsing the replication before the
/// rules read it would therefore buy nothing on the 24% and wreck the other
/// 76%, where the same operation moves the field by up to 97% — and it would
/// fork the baseline the reference comparison is stated in.
///
/// # The floor under the conversion
///
/// Both derivations of the separation floor the rounded quotient at one gate.
/// At a gate interval of 0.75 km or wider the quotient rounds to zero and the
/// floor substitutes one gate, which is a shorter span than the thresholds
/// above are calibrated for — and silently, since nothing about the result
/// says the conversion did not survive. It is unreachable on this corpus: all
/// 157 velocity sweeps land at two gates (155 of them) or three (2 TDWR cuts).
/// Recorded, not handled.
const TEXTURE_STEP_KM: f64 = 0.5;

/// Azimuthal half-span of the texture window, in rows: the widest span any
/// stencil here reads, so the question asked is about the whole neighbourhood
/// the estimator differentiates and not about one row of it.
const TEXTURE_AZ_HALF: i32 = 5;

/// Pairs required before the window has an answer.
const TEXTURE_MIN_PAIRS: usize = 8;

/// Rms velocity difference along the beam, over the neighbourhood each stencil
/// reads. Root-mean-square of `v(r + `[`TEXTURE_STEP_KM`]`) − v(r)` across
/// ±[`TEXTURE_RANGE_HALF_KM`] in range and ±[`TEXTURE_AZ_HALF`] rows, NaN
/// where the window holds fewer than [`TEXTURE_MIN_PAIRS`] pairs.
///
/// `grid` is [`dealias`]'s output rather than the median-filtered field the
/// stencils differentiate: the question is whether the radar measured a
/// continuous velocity along the beam, and a filter of ours must not be able
/// to answer it yes. Undoing the encoding's own wrap is not answering it —
/// see [`GK_MAX_TEXTURE_VNY_FRAC`] for which field, and why it changed.
fn range_texture(
    grid: &[Vec<f64>],
    sweep: &VelocitySweep,
    rows: crate::azimuth::Rows,
) -> Vec<Vec<f64>> {
    let n = grid.len();
    let gc = sweep.gate_count;
    if gc == 0 {
        return vec![Vec::new(); n];
    }
    let dk = ((TEXTURE_STEP_KM / sweep.gate_interval_km).round() as usize).max(1);
    let gh = ((TEXTURE_RANGE_HALF_KM / sweep.gate_interval_km).round() as i32).max(1);
    // Per row, the squared difference at `dk` and its running window sum, so
    // the azimuthal pass below adds rows rather than rewalking gates.
    // `f32`/`u16` per cell rather than `f64`/`u32`: this is one number per bin
    // held for the length of the azimuthal pass, and a 1832-gate super-res cut
    // has 1.3 M of them. The count never exceeds the window and the sum is a
    // mean of squares, so neither wants the width.
    //
    // Two flat buffers written through `par_chunks_mut` rather than a `Vec` of
    // per-row pairs collected out of the map: the azimuthal pass holds all of
    // this at once either way, so the row vectors bought nothing and cost one
    // allocation each. The prefix sums are scratch — nothing outside the row
    // reads them — so they are held per *job* through `for_each_init` instead
    // of per row. `pre[0]`/`pcn[0]` are zero at construction and no row writes
    // index 0, so a reused pair needs no clearing.
    let mut sum = vec![0.0f32; n * gc];
    let mut cnt = vec![0u16; n * gc];
    sum.par_chunks_mut(gc)
        .zip(cnt.par_chunks_mut(gc))
        .enumerate()
        .for_each_init(
            || (vec![0.0f64; gc + 1], vec![0u32; gc + 1]),
            |(pre, pcn), (i, (sum_row, cnt_row))| {
                // The difference is folded into the prefix sum rather than
                // laid down in a pass of its own: it is read exactly once,
                // immediately, in the same order. `d2` and `ok` were two
                // whole-row allocations to carry a scalar one step.
                for j in 0..gc {
                    let (mut d2, mut ok) = (0.0f64, 0u32);
                    if j + dk < gc {
                        let (a, b) = (grid[i][j], grid[i][j + dk]);
                        if !a.is_nan() && !b.is_nan() {
                            d2 = (b - a).powi(2);
                            ok = 1;
                        }
                    }
                    pre[j + 1] = pre[j] + d2;
                    pcn[j + 1] = pcn[j] + ok;
                }
                for j in 0..gc {
                    let lo = (j as i32 - gh).max(0) as usize;
                    let hi = ((j as i32 + gh) as usize).min(gc - 1);
                    sum_row[j] = (pre[hi + 1] - pre[lo]) as f32;
                    cnt_row[j] = (pcn[hi + 1] - pcn[lo]) as u16;
                }
            },
        );
    (0..n)
        .into_par_iter()
        .map(|i| {
            (0..gc)
                .map(|j| {
                    let (mut s, mut c) = (0.0f64, 0u32);
                    for da in -TEXTURE_AZ_HALF..=TEXTURE_AZ_HALF {
                        if let Some(ai) = rows.neighbour(i, da) {
                            s += f64::from(sum[ai * gc + j]);
                            c += u32::from(cnt[ai * gc + j]);
                        }
                    }
                    if (c as usize) < TEXTURE_MIN_PAIRS {
                        f64::NAN
                    } else {
                        (s / c as f64).sqrt()
                    }
                })
                .collect()
        })
        .collect()
}

/// `refused` is the incoherence mask [`dealias_with_knobs`] already built for
/// this sweep, or `None` from a dealiasing that built none. See
/// [`incoherent_velocity`] for why the two are the same mask, and
/// [`preprocess_velocity_with`] for how it gets here.
fn llsd_nrot(
    sweep: &VelocitySweep,
    dealiased: &[Vec<f64>],
    vel_grid: &[Vec<f64>],
    refused: Option<&[bool]>,
) -> Vec<Vec<f64>> {
    let num_radials = vel_grid.len();
    let gc = sweep.gate_count;
    let rows = sweep_rows(sweep, num_radials);
    let spacing_rad = rows.step_deg.to_radians();
    let half_degree_rows = rows_are_half_degree_pairs(sweep.azimuths_deg);
    // The cut's own limit, read off the raw sweep: after the dealiaser has
    // run, a grid no longer folds at it.
    let limit = fold_limit_ms(sweep, sweep.vel_grid);
    // The ceiling and the field it is applied to are one value, so the stage
    // cannot run where nothing reads it. Without a limit there is no ceiling,
    // and [`range_texture`] over a whole sweep would be answering a question
    // no gate below asks.
    let texture = limit.map(|v| {
        (
            GK_MAX_TEXTURE_VNY_FRAC * v,
            range_texture(dealiased, sweep, rows),
        )
    });
    // The dealiaser asked this exact question first, of this exact grid: it
    // set the same ground aside before its first pass ran, from
    // `sweep.vel_grid`, at this `rows`, this `gate_interval_km` and this
    // limit. Its answer is reused rather than recomputed.
    //
    // `None` is a dealiasing that produced no mask — it returned before
    // reaching one (no fold limit, or too few rows to propagate across), or
    // it ran under a profile that does not refuse incoherent velocity at all.
    // Only the first two can reach here, and in the first the `limit` below is
    // `None` too, so the fallback is a real computation exactly when the mask
    // this rule needs does not exist yet.
    let fallback: Option<Vec<bool>> = match (refused, limit) {
        (None, Some(v)) => Some(incoherent_velocity(
            sweep.vel_grid,
            rows,
            gc,
            sweep.gate_interval_km,
            v,
        )),
        _ => None,
    };
    let incoherent: Option<&[bool]> = refused.or(fallback.as_deref());

    (0..num_radials)
        .into_par_iter()
        .map(|i| {
            (0..sweep.gate_count)
                .map(|j| {
                    if vel_grid[i][j].is_nan() {
                        return f64::NAN;
                    }
                    let range_km = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
                    if range_km <= MIN_RANGE_NM * KM_PER_NM {
                        return f64::NAN;
                    }
                    // Rotation is not reported over velocity with no coherent
                    // solution: the dealiaser handed that ground back exactly
                    // as the radar reported it, and a rotation computed from
                    // it would be a rotation of the noise.
                    if incoherent.is_some_and(|m| m[i * gc + j]) {
                        return f64::NAN;
                    }
                    // Rotation is only reported over velocity the radar
                    // measured continuously along the beam.
                    if let Some((max, tex)) = &texture
                        && tex[i][j] > *max
                    {
                        return f64::NAN;
                    }

                    let arc_per_radial = range_km * spacing_rad;
                    // The operator is chosen by the sweep's own row spacing and
                    // by nothing else — not by range. Rows already at whole
                    // degrees are read by the operator measured on such a
                    // sweep, not by the super-res one over twice the sky. Same
                    // rows either way — a sector's arc ends where the antenna
                    // stopped whichever operator reads it.
                    let op = if half_degree_rows {
                        split_stencil_rot
                    } else {
                        legacy_stencil_rot
                    };
                    let rot = op(vel_grid, i, j, arc_per_radial, sweep.gate_count, rows);
                    match rot {
                        Some(rot) => {
                            let divisor = rot_divisor(range_km / KM_PER_NM);
                            (rot / divisor).clamp(-NROT_LIMIT, NROT_LIMIT)
                        }
                        None => f64::NAN,
                    }
                })
                .collect()
        })
        .collect()
}

// ————————————————————————————————————————————————————————————————————
// Step 1: dealiaser — a validity-marking multi-pass. Gates start invalid;
// environmental-wind and zero-isodop seeds mark the first valid gates;
// bridge and flood-fill passes propagate validity; unreached data keeps raw
// in bulk (measured), the rest is converted to ND, and residual fold walls
// are censored.
// ————————————————————————————————————————————————————————————————————

/// Environmental-wind seed tolerance in m/s — deliberately tight; empirical,
/// tuned against the reference's kept fraction on folded volumes. Those kept
/// fractions are gone; branch `campaign-harness` preserved the instrument
/// this campaign tuned with and no reading it took.
const DA_SEED_TOL: f64 = 5.0;

/// Agreeing 4-neighbors required for a gate-level wind seed. A wind-matching
/// pocket inside storm-perturbed flow can never seed a 5×10 all-gates tile;
/// gate seeds anchor it at raw before any bridge can unfold it to the wrong
/// branch. The runs that showed it were not kept — nothing on branch
/// `campaign-harness` audits the 3, so treat it as unaudited.
const DA_SEEDGATE_NEIGHBORS: i32 = 3;

/// Scale on every bridge/fill threshold — the pass ordering is fixed but the
/// base thresholds are nominal; the scale is empirical, set where dealias
/// coverage matches the reference. The coverage sweep that located 1.4 is not
/// on branch `campaign-harness`; the probe that would re-run it is.
const DA_THRESH_SCALE: f64 = 1.4;

/// Iteration cap for the pass loop; propagation converges within ten on
/// every volume measured.
const DA_PASSES: i32 = 10;

/// Raw-continuity flood-fill threshold as a Vny fraction. The aliased flood
/// runs at a much lower threshold than the raw flood — raw acceptance does
/// no interval testing, so a high value cannot cause wrong-branch unfolds;
/// it only admits raw-continuous texture the bridges' agreement rules
/// refuse.
const DA_FLOOD_RAW_FRAC: f64 = 0.4;

/// Aliased flood-fill threshold as a Vny fraction.
const DA_FLOOD_ALIASED_FRAC: f64 = 0.25;

/// Gap length (gates) above which the skip-ND radial bridge waives the
/// continuity test on re-entry. This pass exists to connect distant regions
/// — comparing a gate's velocity to one 50 km back is meaningless; across
/// long gaps the two-direction identity requirement is the only sane check.
const DA_GAPJUMP_GATES: i32 = 10;

/// Zero-isodop seed tightness in m/s.
const DA_ZISO_TOL: f64 = 1.5;

/// Minimum connected-component size (bins, 4-adjacency) for a never-reached
/// data region to be kept at raw, and for a gate-seed cluster to count.
/// Empirical: the reference's raw-default keeps only regions above a
/// measured size gate this value sits inside. Where that gate actually sits
/// was read off the reference once and written down nowhere that lasted, so
/// branch `campaign-harness` cannot say how much room 16 has either side.
///
/// # The ladder below it, and why none of it is taken
///
/// This rule punches two of the thirteen holes at KTLX that the unbiased ring
/// hover found, so it was worth walking down. Painted bins inside 80 km on the
/// lowest super-res cut, with KTLX scored against the reference's own field —
/// GR2Analyst's render decoded bin for bin, 3546 painted bins over the
/// 7.05–20 nm annulus, not thirteen hover points:
///
/// ```text
///   value  KHNX  KTLX   annulus  agreeing  disagreeing  KFTG core
///     16      0  1227     1.63%       434          696  intact
///      8      0  1362     1.97%       499          863  intact
///      4      4  1540     2.23%       553          987  intact
///      2      6  1581     2.29%       565         1016  intact
///      1      6  1585     2.29%       571         1014  moved
/// ```
///
/// Four and below take KHNX off zero, where the reference reads ND at all
/// twenty bins hovered, so they are refused before anything else is asked.
/// They also cost a scorecard rung, and it is a real disagreement rather than
/// an artifact: `CA_KHNX` at 6.0 m/s of range noise, where the reference
/// paints six of nine hovered points and this module falls from six to one.
///
/// Eight survives every hard test — KHNX 0, the KFTG core unmoved, 195 of 216
/// rungs with not one cell changed — and is refused on its values. It adds 301
/// painted bins of which **101** are ones the reference paints and 200 are
/// not, a 34% hit rate against the 38% precision the field already has; it
/// blanks 69 bins of which 36 were agreeing; the ones it gets right carry a
/// mean departure of 0.233 and a worst case of 2.09; and the 200 it gets wrong
/// average |0.471| with eight over 1.0, in twenty free-standing clusters that
/// touch no reference paint at all. That is noise at a third the honest rate,
/// not a repair, and 16 stands.
const DA_RAWMIN_BINS: usize = 16;

/// Censor threshold in units of Vny: the jump between 4-neighbours above
/// which the pair is a residual fold wall rather than shear, and both bins go.
///
/// # The reference's transfer point, at six sites
///
/// Step edges were patched into the 30–47 km band of a real volume's 0.5° cut
/// and GR2Analyst hovered along the 21.0 nm arc, six sites, jumps sized in
/// units of **each cut's own declared Nyquist**: a coarse ladder at 0.70,
/// 1.00, 1.24, 1.50, 1.70 and 1.90·Vny, then a fine one stepping the painted
/// amplitude by the 0.5 m/s the archive quantizes velocity in, which lands six
/// rungs between 1.675 and 2.176·Vny depending on how low the site's limit is.
///
/// Every rung at or under 1.795·Vny paints at full honest value — peak linear
/// in the jump, 0.047 per m/s of it at 21.0 nm, the same slope at all six
/// sites — and no rung from 1.801·Vny up paints anything:
///
/// ```text
/// site   Vny     last kept        first censored
/// KHNX  11.66   1.715  (0.93)     1.801
/// KLWX  11.34   1.764  (0.93)     1.852
/// KLOT  23.96   1.795  (2.04)     1.836
/// KATX  25.32   1.777  (2.11)     1.817
/// KMSX  25.91   1.775  (2.15)     1.814
/// KTLX  11.49   1.741  (0.93)     1.828   (holdout)
/// ```
///
/// Twenty of the 23 rungs above the transfer read ND outright. The other three
/// read ±0.06 to ±0.10 — which is |jump − 2·Vny| of shear at that same 0.047
/// per m/s, the reference resolving the wall rather than painting it, and
/// under the palette's first colour either way. Nothing above the transfer
/// survives to be seen.
///
/// Two things are pinned by that table. The transfer is at the same *multiple*
/// at every site while the absolute jump under it runs from 20.0 m/s (KLWX) to
/// 46.0 m/s (KMSX), so the censor scales with the cut's limit and is not a
/// speed. And it is a plain threshold, not a test of nearness to the 2·Vny a
/// fold displaces by: the rungs **above** 2·Vny, which only the low-limit
/// sites can express (2.00, 2.03, 2.06, 2.09, 2.12, 2.14, 2.18), are censored
/// too.
///
/// # What 1.24 was
///
/// It was this multiple at one site: 1.24 × 23.96 = 29.7 m/s, the KLOT wall
/// ladder it was fitted on. Carried to a cut that declares 11.5 m/s it becomes
/// a 14.3 m/s censor, and a ±8 m/s step — 16 m/s, ordinary strong shear, which
/// the reference paints at +0.38 — was erased outright at KHNX and KTLX. It
/// was too low at KLOT as well: the reference paints 1.24, 1.50 and 1.70·Vny
/// there and we erased all three.
///
/// # It is not where the clear-air coverage goes
///
/// This censor has been named as a suspect for the near-in shortfall
/// ([`GK_MAX_TEXTURE_VNY_FRAC`] carries that accounting) on the reasoning that
/// a cut whose velocity is comparable to its own Nyquist nearly everywhere
/// must be full of pairs 1.80·Vny apart. Measured, it refuses **none** of the
/// thirteen ring points the reference paints at KTLX 2025-02-19, and disabling
/// it entirely moves that cut from 1227 painted bins to **1039** — down, not
/// up, because a kept fold wall is 2·Vny of fake shear the derivative then
/// reads. 1.80 stands, and on this cut it is buying coverage rather than
/// costing it.
///
/// # The one wall it was told to ignore, and why it still is
///
/// The censor skips a bin [`incoherent_velocity`] refused, and does not count
/// a refused neighbour as evidence against the bin beside it — a bin no pass
/// made a claim about is not a wall a pass failed to place. That exemption is
/// what let KCRP 2017-08-26's tail through, and it is still the right rule.
///
/// The write-back hands a refused bin back exactly as reported, folded, while
/// its neighbours come back unfolded, so the two meet across a step that is
/// pure convention. Before [`COH_FOLD_VNY_FRAC`], with 7.5% of that cut
/// refused: **191 of the 240 bins the reference refused stood within the
/// operator's own 11-radial span of such a seam, against 0 of the 858 the two
/// fields agree on.** Each of the 191 had exactly one refused side; across
/// them the dealiased pair differed by a median 63.04 m/s, 2·Vny to the
/// centimetre, and the *reported* pair by a median **1.00**. GR2Analyst,
/// hovered at the fourteen loudest with the status bar's az/range readback
/// verified on each, read ND at five and −0.18 to +0.18 at the other nine,
/// where this module read −3.35 to −5.00. They were an artefact, and they were
/// this module's own.
///
/// Withdrawing the exemption removes them. Adding the test that makes it
/// principled — count a refused neighbour only where the *reported* field is
/// continuous across the pair, so the gap is one the passes opened rather than
/// one the antenna measured — removed 292 values at KCRP of which the
/// reference painted none, 203 of them painted here at a mean |2.109|, and
/// left every other decoded site's field alone to the bin.
///
/// It is not taken, because the seam was a symptom. On the tree
/// [`COH_FOLD_VNY_FRAC`] landed on, KCRP has **no refused bins at all** and no
/// spurious bin within span of such a seam, so the rule buys nothing there —
/// and at KTLX 2025-02-19, where the mask still fires, it costs **24 agreeing
/// bins for none removed**, on seams the passes opened correctly and the
/// reference paints across. The exemption's reasoning survives its own
/// counter-example: what was wrong was which bins were being refused.
const CENSOR_VNY_FRAC: f64 = 1.80;

/// The posture [`dealias`] takes towards data its passes could not settle.
///
/// The passes themselves — seeds, bridges, flood fills, head-and-shoulders —
/// are identical under every profile; what differs is only what happens
/// around them. NROT differentiates the field, so a residual fold wall becomes
/// clamp-level fake shear and is worth censoring aggressively. A velocity
/// *display* consumer (storm-relative velocity) shows the field itself, where
/// a censored gate is a hole in a couplet and the harm runs the other way — so
/// it keeps everything the passes did not prove wrong.
///
/// The profile reaches the two post-pass censoring/ND knobs and one pre-pass
/// one: whether velocity with no coherent solution is handed back as the radar
/// reported it ([`incoherent_velocity`]) rather than run through passes that
/// can only invent an answer. That is on for NROT and off for display, where
/// the field is measured against the RPG's own dealiased velocity and the
/// RPG resolves everything present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DealiasProfile {
    /// NROT's tuned posture: velocity with no coherent solution comes back
    /// exactly as reported, unreached-data regions under [`DA_RAWMIN_BINS`]
    /// bins go ND, and any bin more than [`CENSOR_VNY_FRAC`]·Vny from a
    /// 4-neighbour is censored as a residual fold wall.
    NoFalseShear,
    /// Maximum retained coverage for velocity display consumers: every
    /// unreached data gate keeps its raw value regardless of region size
    /// ([`COVERAGE_RAWMIN_BINS`]), and the fold-wall censor runs at the same
    /// measured [`CENSOR_VNY_FRAC`] threshold — dropping the censor entirely
    /// was measured worse against the RPG's own dealiased velocity (a kept
    /// fold wall is a 2·Vny error on every gate it touches, which costs
    /// more level agreement than the censored hole costs coverage). That A/B
    /// ran against live N0G/N1G twins and its record did not survive; branch
    /// `campaign-harness` carries no reading of it.
    Coverage,
}

/// [`DealiasProfile::Coverage`]'s kept-raw region floor: keep every unreached
/// data gate, however small the region. The RPG's dealiaser resolves all
/// present data, so for a field that is *displayed* rather than
/// differentiated, matching its coverage matters more than suppressing
/// isolated pockets.
///
/// The A/B against live N0G/N1G twins that settled this went with the same
/// scratchpad as [`DealiasProfile::Coverage`]'s. Branch `campaign-harness` has
/// the harness the twins were fetched into and not a number either of them
/// gave back, so the reasoning above is the whole of the surviving case.
const COVERAGE_RAWMIN_BINS: usize = 1;

/// The two post-pass knobs a [`DealiasProfile`] resolves to. `pub(crate)` so
/// the srv harness can measure candidate postures without shipping a variant
/// per experiment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DealiasKnobs {
    /// Minimum connected-component size (bins, 4-adjacency) for a
    /// never-reached data region to keep raw rather than go ND.
    pub rawmin_bins: usize,
    /// Post-dealias fold-wall censor threshold, in units of Vny;
    /// `f64::INFINITY` disables the censor.
    pub censor_vny_frac: f64,
    /// Hand back velocity with no coherent solution
    /// ([`incoherent_velocity`]) exactly as the radar reported it, rather
    /// than unfolding, censoring or dropping any of it.
    pub refuse_incoherent: bool,
}

impl DealiasProfile {
    pub(crate) fn knobs(self) -> DealiasKnobs {
        match self {
            DealiasProfile::NoFalseShear => DealiasKnobs {
                rawmin_bins: DA_RAWMIN_BINS,
                censor_vny_frac: CENSOR_VNY_FRAC,
                refuse_incoherent: true,
            },
            DealiasProfile::Coverage => DealiasKnobs {
                rawmin_bins: COVERAGE_RAWMIN_BINS,
                censor_vny_frac: CENSOR_VNY_FRAC,
                refuse_incoherent: false,
            },
        }
    }
}

/// Half a circle, counted in rows of this grid — the offset from a radial to
/// the one facing it, which the zero-isodop seed pairs a near-zero gate
/// against.
///
/// On a grid that closes the circle it is half the grid, exactly and without
/// measuring anything: n rows spanning 360° put n/2 of them in 180°. An odd n
/// has no row at 180° and takes the one just inside, off by 360/2n° — a
/// quarter of a degree on the 719 rows a rotation that dropped a radial
/// leaves, and the ±3-row window below is three times that wide.
///
/// On an arc it is 180° at the arc's own spacing, and **not** half the arc's
/// rows: the 36 rows that are half of a 72-row, 36° sector sit 18° around, and
/// a radial 18° away is on the same side of the isodop as the one it would be
/// confirming — the near-zero band is tens of degrees wide, so such a pair
/// agrees with itself and seeds the whole band.
fn half_turn_rows(rows: crate::azimuth::Rows) -> i32 {
    if rows.closed {
        (rows.count / 2) as i32
    } else {
        (180.0 / rows.step_deg).round() as i32
    }
}

/// Returns what [`dealias_with_knobs`] returns: the incoherence mask this
/// dealiasing set aside, for the one caller that refuses the same ground
/// again. A caller that only wants the grid drops it — [`crate::srv`] does,
/// and under [`DealiasProfile::Coverage`] there is nothing to drop.
///
/// **The grid this hands back does not say which gates it examined**, and the
/// obvious way to recover that is measured to be useless. A pass either places
/// a gate on a fold branch, refuses it under
/// [`DealiasKnobs::refuse_incoherent`], or never reaches it — three different
/// facts. The mask names the middle one wherever a mask exists at all; the grid
/// that comes out carries none of them.
///
/// The cheap recovery is to call a gate resolved when its value moved. Counted
/// on real volumes, 2026-08-12, lowest Doppler cut, against the passes' own
/// account:
///
/// | volume | placed on a branch | of those, value unchanged |
/// |---|---|---|
/// | KTLX 2024-12-16 | 91896 | **99.996%** |
/// | KFTG 2023-06-22 | 344171 | **99.92%** |
/// | KHNX 2024-12-16 | 14950 | **99.87%** |
/// | KDMX 2022-03-05 | 167967 | **98.31%** |
/// | KCRP 2017-08-26 | 396077 | **85.43%** |
///
/// So that test finds between 0.004% and 15% of what it is looking for, and it
/// is wrong in the other direction too (at KDMX, 12069 of the 14901 gates whose
/// value moved were not placed by any pass). The reason is ordinary: most gates
/// are already on the right branch, so a pass *confirms* rather than moves
/// them, and a confirmation leaves the value identical.
///
/// That is exactly the distinction worth having — "a pass examined this and
/// confirmed it" against "nothing ever looked here" — and it is most of the
/// population. It is also the whole of what is still unnamed: the refusal is
/// reported, so what no return value here separates is the gate a pass reached
/// and left alone from the gate no pass reached. Nothing consumes that yet, so
/// nothing is plumbed; recorded so the next reader reaches for the measurement
/// instead of the comparison.
pub(crate) fn dealias(
    vel_grid: &mut [Vec<f64>],
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    dealias_profile: DealiasProfile,
) -> Option<Vec<bool>> {
    dealias_with_knobs(
        vel_grid,
        sweep,
        elevation_deg,
        profile,
        dealias_profile.knobs(),
    )
}

/// The fold limit this sweep is dealiased against, m/s: what the RDA declared,
/// or what the data shows when it declared nothing. `None` abandons the pass.
///
/// **Declared wins, above the floor.** The declaration is a property of the
/// waveform — the PRF the cut was flown at — and it is right whether or not the
/// sweep happened to fold, which is exactly where [`estimate_nyquist`] is
/// wrong. A calm sector estimates a limit far below the real one and then
/// unfolds honest gradients into shear that was never in the air; a
/// declaration cannot do that.
///
/// # Which radar this reaches, measured
///
/// **The WSR-88D, and only it.** Its Doppler cuts declare 23.84–62.94 m/s
/// across ten volumes, and the number moves inside one volume as much as
/// between sites — KFFC's low Doppler cuts declare 25.65 and its cut 12
/// declares 62.94 — so a per-sweep declaration is worth having and this arm
/// takes it.
///
/// The TDWR does **not** reach it, and that is the correction to what stood
/// here before. Its short PRT does buy 150 m gates at the cost of unambiguous
/// velocity, so its Doppler cuts really do fold on ordinary storm motion — but
/// it never says where. Across 22 volumes from 10 TDWR sites over three days,
/// every cut declares `nyquist_velocity = 0`, which
/// [`crate::nyquist::DeclaredNyquist::declare`] refuses as the absence it is.
/// So a TDWR arrives here with `declared_nyquist_ms == None` and takes the
/// estimator arm — the radar this argument was originally made about is the one
/// still being estimated for.
///
/// [`crate::sampler::FOLD_LIMIT_FLOOR_MS`] bounds both, and it is the sampler's
/// own constant rather than a second copy of the number: the guard that refuses
/// to interpolate across a fold and the pass that removes one are answering the
/// same question about the same sweep, and two floors that could drift apart
/// would let a section and a plan view take different views of the same gate. A
/// declaration under the floor is refused rather than trusted — no operational
/// waveform folds that low, so such a value is a mis-decode, and the estimate
/// is the better of two poor answers.
fn fold_limit_ms(sweep: &VelocitySweep, vel_grid: &[Vec<f64>]) -> Option<f64> {
    let floor = crate::sampler::FOLD_LIMIT_FLOOR_MS;
    match sweep.declared_nyquist_ms {
        Some(declared) if declared >= floor => Some(declared),
        _ => {
            let estimated = estimate_nyquist(vel_grid);
            (estimated >= floor).then_some(estimated)
        }
    }
}

/// Along-beam difference above which two gates [`TEXTURE_STEP_KM`] apart
/// **straddle** — cannot be read as one continuous velocity — as a fraction of
/// the cut's own limit.
///
/// It is a fraction rather than a speed for the reason every other threshold
/// on this page is: the reference's answer holds at the same multiple across
/// a 3.1× span of Nyquist. It sits just under 1 because that is what the two
/// ladders that bracket it say. On the square wave, whose every wall is a jump
/// of exactly J and nothing else, the reference paints J = 0.60·Vny at all
/// seven sites and refuses J = 1.00 at five of them; on the dense wave it
/// paints 0.80 and refuses 1.20. On the range-noise ladder, whose differences
/// are triangular on ±2q·Vny, it paints q = 0.51 and refuses q = 0.57, which
/// puts a per-difference boundary between 1.02 and 1.14·Vny.
const COH_STRADDLE_VNY_FRAC: f64 = 0.90;

/// Along-beam difference **above** which the pair is a fold rather than a
/// straddle, as a fraction of the cut's own limit.
///
/// [`COH_STRADDLE_VNY_FRAC`] bounds the straddling band below and this bounds
/// it above, because for twelve campaigns it had no upper bound at all and that
/// is what made the rule unable to tell one clear-air volume from another.
///
/// # The rule was answering its own question with the wrong statistic
///
/// [`incoherent_velocity`] asks whether *any* assignment of fold branches makes
/// the velocity continuous. It asked it of `|v[j+dk] − v[j]|` on the velocity
/// the radar reports, which is already wrapped onto ±Vny. A fold wall is a jump
/// of very nearly 2·Vny — the true change is small and the wrap supplies the
/// rest — so every fold wall cleared 0.90·Vny and was counted as evidence that
/// no fold assignment exists. The doc below conceded it ("aliasing straddles
/// only along the line the wall runs on") and defended it on the grounds that a
/// wall is one curve crossing the neighbourhood. That defence holds where the
/// wind is far from the limit. It fails where the wind approaches it, and that
/// is exactly the one volume where this rule fired and the reference disagreed.
///
/// # Why it fails at KTLX and at no other site
///
/// KTLX 2025-02-19 is clear air at Vny 11.49. In its 45°–112.5° quadrant the
/// radial wind sits at 0.73 of that limit on average with 46–52% of its gates
/// above 0.85 of it; in the quadrants this rule leaves alone it sits at
/// 0.21–0.39 with 2.6–4.8% above. Velocity that close to the limit crosses it
/// on estimator noise, and every crossing is a 2·Vny difference. Split the
/// straddling pairs by whether they *can* be a fold:
///
/// ```text
///                            |d| in (0.90, 1.30)·Vny    |d| >= 1.30·Vny
///     KTLX quiet sectors              3.33%                  2.56%
///     KTLX wedge sectors              3.71%                 14.03%
///     KHNX quiet sectors              7.36%                  7.62%
///     KHNX wedge sectors              5.92%                  9.54%
/// ```
///
/// The wedge is a 5.5× excess of fold-shaped differences over an unchanged rate
/// of everything else. KHNX is high on both everywhere — it really is
/// incoherent and this rule is right about it. KDMX, KFTG, KATX, KMSX and KDDC
/// never come near their limit, which is why the rule never fired there.
///
/// # Where 1.30 comes from
///
/// Bounded below by the reference's own ladders and above by its field, and the
/// two brackets do not overlap.
///
/// **Below.** [`COH_STRADDLE_VNY_FRAC`]'s dense-wave ladder has the reference
/// refusing a wall of J = 1.20·Vny. A bound at or under 1.20 would excuse that
/// wall as a fold, so the bound must exceed 1.20. Pure fold-invariance —
/// reducing the difference modulo 2·Vny, which is the bound 1.10 — is therefore
/// not available, and it measures worse anyway.
///
/// **Above.** KHNX is the null control and reads ND across its whole cut. Swept
/// through the pipeline with [`COH_MAX_STRADDLE`] held at 0.039, painted bins
/// inside 80 km:
///
/// ```text
///     upper bound        1.20   1.25   1.30   1.35   1.40   none (as shipped)
///     KHNX                402     10      0      0      0          0
///     KTLX               2508   2024   1709   1488   1260       1656
/// ```
///
/// 1.30 is the smallest bound at which KHNX still paints nothing, and therefore
/// the one that releases the most of KTLX's ground while it does. The screen
/// this was chosen on rated 1.25 equal; the pipeline does not, because the
/// dealiaser reads this mask too. Above 1.35 the band stops excusing the folds
/// that made the wedge and KTLX falls below its shipped coverage.
const COH_FOLD_VNY_FRAC: f64 = 1.30;

/// Fraction of a neighbourhood's along-beam pairs allowed to straddle before
/// the sweep is held to carry no coherent velocity there.
///
/// A fold wall is a curve: it crosses the neighbourhood along one line and
/// straddles a few percent of its pairs. Velocity with no coherent solution
/// straddles everywhere at once. Measured at the bins the reference decides,
/// over the neighbourhood below:
///
/// ```text
///                                    straddling fraction
///     KFTG 2023-06-22 mesocyclone core, 4 bins        0.00        painted
///     KCRP 2017-08-26 Harvey, 19 bins            0.01..0.08   17 painted
///     KHNX 2024-12-16 clear air, 20 bins         0.11..0.21    0 painted
/// ```
///
/// The gap 0.08..0.11 is wider than it looks: the KCRP maximum is one bin at
/// az 73.26°, 9.52 nm, where the reference reads −4.42, and the next is 0.07.
///
/// # The gap is closed by a site those three did not include
///
/// KTLX 2025-02-19 is clear air at 11.49, within 0.2 m/s of KHNX's limit, and
/// an unbiased 240-point ring hover at 8/11/14/17/20 nm finds the reference
/// painting thirteen of its bins. Their straddling fractions are
///
/// ```text
///     0.031 0.044 0.065 0.076 0.080 0.085 0.087 0.091 0.094 0.108 0.123
///     0.148 0.189
/// ```
///
/// and KHNX's twenty ND bins read 0.119..0.211. **Four bins the reference
/// paints sit inside the band of twenty it refuses**, and six sit above this
/// threshold. No threshold on this statistic admits the one set and refuses
/// the other. Nor does adding [`GK_MAX_TEXTURE_VNY_FRAC`]'s range texture as a
/// second axis: KTLX's painted bin at az 188.75°, 8.00 nm reads (0.148, 0.661)
/// and KHNX's refused bin at az 330.25°, 8.17 nm reads (0.145, 0.581), which is
/// smaller in **both** coordinates — so every downward-closed region admitting
/// the first admits the second, whatever its shape.
///
/// # None of that was true of the statistic, only of the unbanded one
///
/// Every figure above was read off `|d| > 0.90·Vny` with no upper bound, which
/// counts a fold crossing as evidence that no fold assignment exists — see
/// [`COH_FOLD_VNY_FRAC`], which is what closed this. Bounded above at 1.30·Vny
/// the same two populations separate, over the whole decoded field rather than
/// thirteen hovers and twenty:
///
/// ```text
///                        KTLX reference-painted        KHNX field
///     unbanded              0.0254 .. 0.1962        0.1129 .. 0.2039
///     banded                0.0158 .. 0.0479        0.0420 .. 0.0852
/// ```
///
/// Admitting 99% of KTLX's reference-painted bins releases 78.6% of KHNX's
/// field unbanded and 1.7% banded.
///
/// # Where 0.039 comes from
///
/// Two-sided, and measured through the pipeline rather than off the screen.
/// Painted bins inside 80 km with [`COH_FOLD_VNY_FRAC`] at 1.30:
///
/// ```text
///     threshold          0.030   0.035   0.039   0.042   0.045
///     KHNX (must be 0)       0       0       0       0      30
///     KDDC (holdout)      2705    2772    2772    2772    2772
///     KTLX                1090    1623    1709    1968    2336
/// ```
///
/// Above 0.042 the null control starts painting; below 0.035 the holdout starts
/// losing ground it keeps everywhere else. 0.039 is the middle of what is left,
/// with about a tenth of its own value in hand on each side — where 0.09 sat
/// 2.2% under KHNX's minimum and had no second side at all.
///
/// # What this rule costs, measured against the reference's whole field
///
/// Thirteen hover points said this was one refuser among several. The
/// reference's own field says it is *the* refuser. GR2Analyst's NROT render at
/// KTLX 2025-02-19 decodes exactly — its colour bar carries no blending, so a
/// nearest-colour lookup inverts it, and the result reproduces the same 13 of
/// 13 OCR hovers to a mean 0.0076 and GR's own reported min and max (−1.96,
/// +2.15) to the digit. That is 3546 painted bins over the 7.05–20 nm annulus
/// instead of thirteen. Against them, the first rule that refuses each one, in
/// the order [`llsd_nrot`] applies them, as it stood under the two window
/// shapes and before this rule's difference was bounded above:
///
/// ```text
///                                            ±40 × ±32    ±16 × ±192
///     no raw velocity at the bin                   147           147
///     the dealiaser dropped it                     182           238
///     the median filter dropped it                   2             2
///     flagged incoherent — here                   1783          1092
///     range texture or r²                           98           143
///     a hole in the stencil's window               590           845
///     carried, under SIGNIFICANT                   276           360
///     despeckled                                    34            58
///     painted                                      434           661
/// ```
///
/// The 691 bins the re-shaped window stopped refusing did not all become paint.
/// 227 did; 255 of them walked straight into a **hole in the stencil's window**,
/// which is now the second largest refuser and within reach of the largest.
/// That number is worth reading twice, because a sibling campaign established
/// that every one of those holes is one this module made: the raw sweep carries
/// velocity at all of them and the dealiaser dropped it, and one missing cell
/// costs eleven radials. Bounding the difference above moves the bottom line to
/// 730 and does not re-derive that attribution, which is a per-rule count no
/// probe in the tree produces.
///
/// # The wedge
///
/// Reshaping the support left one component holding 81% of KTLX's mask, 173 of
/// 720 radials wide, with 984 of the reference's bins inside it, and it was not
/// an artefact of the window: the ground under it really did straddle
/// everywhere by the unbanded statistic. It was an artefact of the statistic.
/// Banding the difference above breaks it up — 45 radials and 123 reference
/// bins — because what filled it was the fold crossings of a wind sitting at
/// 0.73 of the Nyquist limit, not incoherence. [`COH_FOLD_VNY_FRAC`] carries
/// that measurement.
///
/// Distance from a censored bin to the nearest straddling pair separates
/// nothing and was not what was wrong: the bins the reference paints and this
/// rule refused at KTLX sit a median 2.0 bins from straddle evidence, and
/// KHNX's twenty ND bins sit the same 2.0.
///
/// So the clear-air difference is not sparseness spread over the sweep and it
/// is not a registration error — the two fields' overlap peaks at exactly zero
/// shift in radials and in gates, falling off symmetrically either way, and
/// where both carry a value they agree: over the unbiased 240-point ring,
/// mean |Δ| 0.038 with the sign right 9 times in 9.
const COH_MAX_STRADDLE: f64 = 0.039;

/// The neighbourhood the straddling fraction is counted over: half-spans in
/// rows and in gates.
///
/// Long along the beam and narrow across it, because the statistic is an
/// along-beam one. It counts pairs of gates [`TEXTURE_STEP_KM`] apart on a
/// single radial; range is the axis it is measured on and azimuth is only
/// where more of it can be found. The window that reads it should have the
/// same shape, and the one this rule shipped with had the opposite one —
/// ±40 rows × ±32 gates, 81 radials wide and 16 km deep.
///
/// # Why a wide window at all
///
/// Over the ±5 rows × ±4 gates [`range_texture`] reads, the same statistic
/// puts KCRP's worst bin at 0.29 and KHNX's best at 0.04 — the two overlap
/// completely, because a fold wall fills a small window as thoroughly as
/// noise does. The wall only dilutes once the neighbourhood holds ground the
/// wall does not cross, and incoherent velocity does not dilute at all.
///
/// So the window is a variance argument, and its half-spans are counts of
/// samples rather than an angle and a distance — which is why they are in
/// rows and gates and not in degrees and kilometres. A cut with twice the
/// rows gets twice the rows here, and that is the intent.
///
/// # Where these two numbers come from
///
/// Not from a preference for narrowness. Against the reference's decoded
/// field at KTLX (see [`COH_MAX_STRADDLE`]) both half-spans were swept, with
/// the threshold held at its measured 0.09 throughout and KHNX's twenty ND
/// bins — the null control — deciding admissibility. Shrinking the window
/// fails at once: at ±20 × ±16 KHNX paints 180 bins that must read ND, at
/// ±10 × ±8 it paints 681. Shrinking azimuth *while lengthening range* does
/// not, and it is the only direction that does not:
///
/// ```text
///     half-spans     KHNX painted   KTLX bins agreeing   precision
///     ±40 × ±32  (shipped)     0            434            0.383
///     ±16 × ±96                8            646            0.437
///     ±16 × ±128               0            654            0.419
///     ±16 × ±160               0            670            0.426
///     ±16 × ±192               0            661            0.424
///     ±16 × ±256               0            661            0.426
/// ```
///
/// ±192 is the smallest depth on that plateau at which KHNX also holds a
/// margin: raise the threshold to 0.10 and the shipped window leaks 14 bins
/// at KHNX where this one leaks none, and at 0.11 the shipped window leaks 64
/// against this one's 15. The neighbourhood is therefore both **narrower
/// where it was doing harm** — 81 radials to 33 — and better separated from
/// the volume that has to stay dark.
const COH_AZ_HALF: i32 = 16;
const COH_RANGE_HALF: i32 = 192;

/// Where the sweep carries velocity that has no coherent solution.
///
/// The question a dealiaser can actually answer is not "is this smooth" but
/// "could any assignment of fold branches make it continuous". Shear is not
/// incoherence — a mesocyclone's own gradient is far under the limit and
/// straddles nothing — and neither is aliasing, which is a fold and is what a
/// dealiaser is for. What straddles is velocity that is not a measurement of
/// one air motion at all: a difference too large to be one continuous reading
/// ([`COH_STRADDLE_VNY_FRAC`]) and too small to be a fold
/// ([`COH_FOLD_VNY_FRAC`]). For twelve campaigns only the first half of that
/// was tested, and the missing half is what cut a wedge out of KTLX.
///
/// # What is done with it, and why that is a refusal
///
/// [`dealias`] hands it back exactly as the radar reported it, under
/// [`DealiasKnobs::refuse_incoherent`]: not unfolded, not censored, not
/// dropped. A dealiaser's job is to undo a known encoding wrap, and where no
/// wrap explains the field there is no claim it can honestly make.
/// [`llsd_nrot`] then refuses the same ground outright, because a rotation
/// computed there is a rotation of the noise.
///
/// The passes were not merely idle on that ground before — they were
/// *productive* on it, which is worse. On KHNX 2024-12-16 08:01:56 inside
/// 80 km, 60.5% of data gates were resolved by a pass, 33.3% kept raw because
/// no pass reached them, and 5.1% censored as fold walls by
/// [`CENSOR_VNY_FRAC`]. The censored 5.1% sit against the largest differences
/// in the sweep, so removing them alone drops the rms range difference from
/// 0.585–0.676 of the limit to 0.352–0.398 — the dealiaser manufacturing the
/// very continuity a continuity ceiling then reads. At az 198.25°, 8.57 nm it
/// went further and unfolded a raw −8.0 m/s to +15.3, a full 2·Vny, out of
/// nine radials reading −10.0 −5.5 −5.5 −5.0 −8.0 −0.5 −2.0 −4.5 −2.0.
///
/// # What it moves
///
/// Lowest super-res velocity cut, painted bins (|NROT| ≥ [`SIGNIFICANT`])
/// inside 80 km. "off" is this rule removed entirely, "±40 × ±32" the window
/// it shipped with, "unbanded" the ±16 × ±192 window at 0.09 with no upper
/// bound on the difference, "banded" this rule as it now stands:
///
/// ```text
///                                              off   ±40 × ±32   unbanded   banded
///     KHNX 2024-12-16  clear air, Vny 11.66    9927           0          0        0
///     KTLX 2025-02-19  clear air, Vny 11.49    2539        1230       1656     1709
///     KBLX 2025-02-19  clear air, Vny 11.17       0           0          0        0
///     KBLX 2024-12-16  clear air, Vny 11.17       —           —          8       53
///     KVNX 2025-02-19  clear air, Vny 11.55       —           —       3383     3394
///     KAMA 2025-02-19             Vny 11.55       —        1952       2082     2082
///     KCRP 2017-08-26  Harvey,    Vny 31.52       —        5288       5634     5328
///     KDMX 2022-03-05             Vny 27.93       —        2947       2948     2957
///     KFTG 2023-06-22             Vny 24.01       —        1571       1571     1571
///     KATX 2025-02-19             Vny 25.32       —           —       2001     1997
///     KLOT 2024-12-16             Vny 23.96       —          72         72       72
///     KMSX 2022-06-04             Vny 24.21       —          34         34       34
///     KABR 2025-02-19             Vny 33.33       —          51         51       51
///     KTWX 2025-01-15             Vny 35.55       —           —          0        0
///     KDDC 2024-12-16  holdout,   Vny 25.84       —           —       2772     2772
/// ```
///
/// Only the low-Nyquist clear-air volumes move, which is where the fold
/// crossings this rule used to count as incoherence are. Against the
/// reference's nine decoded fields over the 7.05–20 nm annulus, with both sides
/// read at |NROT| ≥ [`SIGNIFICANT`] so the reference's own |v| < 0.25 gap
/// counts as unpainted:
///
/// ```text
///                     ours          agreeing        precision
///     KTLX      1559 -> 1640     661 ->  730    0.4240 -> 0.4451
///     KCRP      1098 ->  941     858 ->  905    0.7814 -> 0.9617
///     KDMX      1447 -> 1453    1104 -> 1107    0.7630 -> 0.7619
///     KHNX / KFTG / KATX / KLOT / KMSX / KDDC   unchanged
/// ```
///
/// KCRP is the one to read twice. The rule's mask reaches the dealiaser as well
/// as [`llsd_nrot`], so releasing ground there does not only add paint: 213 bins
/// stop being painted, 112 of them at |NROT| ≥ 1.5 and eight at the ±5.00 clamp,
/// and the reference paints **none** of the 213. Bins handed back unresolved
/// under [`DealiasKnobs::refuse_incoherent`] were the source of those extremes.
///
/// The "off" column is why the window's shape was worth a campaign: at KTLX
/// removing this rule outright doubles what the module paints **at unchanged
/// precision against the reference** (0.383 → 0.381), so nothing it censors
/// there is worse than what it lets through. At KHNX it is the only thing
/// standing between the module and 9927 painted bins the reference reads ND
/// at. Those two facts are what every candidate support had to satisfy at
/// once.
///
/// Six holdout volumes that decided nothing — KCBW, KESX, KICT, KLRX, KGJX,
/// KFSX, Vny 24.26 to 33.50 — read 0, 0, 14, 10, 22, 26 under both windows.
///
/// KHNX's twenty hovered bins, where the reference reads ND at all twenty, go
/// from six painted to none, and so does the whole cut inside 80 km — which is
/// what the reference does there: 290 points on the 8/16/24/32/40 nm rings,
/// none painted. The KFTG 2023-06-22 mesocyclone core is bit-identical under
/// either window — +1.68, +1.50, +1.59, +1.75 at az 90.8/91.3° and 7.5/8.0 nm,
/// against the reference's +1.64, +1.52, +1.56, +1.76 — because the ground
/// this refuses is nowhere the stencil reads there — and it is bit-identical
/// again with the difference bounded above, +1.6839 +1.4955 +1.5884 +1.7520
/// re-measured.
///
/// And neither window nor the band moves a rung of any synthetic ladder: 195 of
/// the 216 rungs across the noise, common-mode, fine, azimuthal and square-wave
/// families at seven sites agree with the reference, disagreeing on the same 21,
/// with the same 155 cells exactly right. The square-wave family was the one to
/// check, since its walls at J = 1.20, 1.35, 1.55 and 1.68·Vny all sit above
/// [`COH_FOLD_VNY_FRAC`] and are now excused as folds here: every one of those
/// cells is unchanged, because [`CENSOR_VNY_FRAC`] and not this rule is what
/// decides them.
fn incoherent_velocity(
    raw: &[Vec<f64>],
    rows: crate::azimuth::Rows,
    gc: usize,
    gate_interval_km: f64,
    nyquist: f64,
) -> Vec<bool> {
    let n = raw.len();
    if gc == 0 {
        return Vec::new();
    }
    let tol = COH_STRADDLE_VNY_FRAC * nyquist;
    let fold = COH_FOLD_VNY_FRAC * nyquist;
    // The same separation [`range_texture`] differences at, and for the same
    // reason: a super-res velocity cut repeats one estimate over two 0.25 km
    // gates, so adjacent-gate differences are structurally zero half the time.
    // `TEXTURE_STEP_KM` carries how much of the corpus reports that way, and
    // what the floor below would cost on a cut that declared wider gates than
    // any of it does.
    let dk = ((TEXTURE_STEP_KM / gate_interval_km).round() as usize).max(1);
    // Per row, the straddling and present pair counts and their running window
    // sums, so the azimuthal pass adds rows rather than rewalking gates —
    // ±COH_AZ_HALF rows is 33 of them per bin on a rotation, and the range
    // half-span costs nothing beyond the prefix sum however deep it is, which
    // is why ±COH_RANGE_HALF may be 385 gates without the rule getting slower.
    // A window that deep still fits u16: it can hold at most 385 pairs.
    //
    // Flat, and written in place: the azimuthal pass reads every row of both
    // counts, so they were always going to be live together, and a row vector
    // each only added an allocation per radial to say so. The prefix sums are
    // scratch, so `for_each_init` holds one pair per job the pool splits off
    // rather than one per row. Index 0 of each is zero at construction and no
    // row writes it, so a reused pair needs no clearing.
    let mut straddling = vec![0u16; n * gc];
    let mut present = vec![0u16; n * gc];
    straddling
        .par_chunks_mut(gc)
        .zip(present.par_chunks_mut(gc))
        .enumerate()
        .for_each_init(
            || (vec![0u32; gc + 1], vec![0u32; gc + 1]),
            |(ps, pp), (i, (straddling_row, present_row))| {
                for j in 0..gc {
                    let (mut s, mut p) = (0u32, 0u32);
                    if j + dk < gc {
                        let (a, b) = (raw[i][j], raw[i][j + dk]);
                        if !a.is_nan() && !b.is_nan() {
                            p = 1;
                            // Above `tol` the pair cannot be read as one
                            // continuous velocity; above `fold` it cannot be
                            // read as anything *but* a fold, and a fold is
                            // what the dealiaser is for.
                            let dv = (b - a).abs();
                            s = u32::from(dv > tol && dv < fold);
                        }
                    }
                    ps[j + 1] = ps[j] + s;
                    pp[j + 1] = pp[j] + p;
                }
                for j in 0..gc {
                    let lo = (j as i32 - COH_RANGE_HALF).max(0) as usize;
                    let hi = ((j as i32 + COH_RANGE_HALF) as usize).min(gc - 1);
                    straddling_row[j] = (ps[hi + 1] - ps[lo]) as u16;
                    present_row[j] = (pp[hi + 1] - pp[lo]) as u16;
                }
            },
        );
    // The mask is one buffer the rows are written into, rather than a row of
    // `bool` per radial gathered by `flat_map`: the caller wants it flat, and
    // building it flat is one allocation instead of `n` plus whatever the
    // gather costs to concatenate.
    let mut refused = vec![false; n * gc];
    refused
        .par_chunks_mut(gc)
        .enumerate()
        .for_each(|(i, refused_row)| {
            for (j, out) in refused_row.iter_mut().enumerate() {
                if raw[i][j].is_nan() {
                    continue;
                }
                let (mut s, mut p) = (0u32, 0u32);
                for da in -COH_AZ_HALF..=COH_AZ_HALF {
                    // Past the end of a sector's arc there is no row to count,
                    // exactly as there is no gate past the last.
                    if let Some(ai) = rows.neighbour(i, da) {
                        s += u32::from(straddling[ai * gc + j]);
                        p += u32::from(present[ai * gc + j]);
                    }
                }
                *out = p > 0 && (s as f64) > COH_MAX_STRADDLE * p as f64;
            }
        });
    refused
}

/// Returns the `n · gc` incoherence mask this dealiasing set aside, so that
/// [`llsd_nrot`] — which refuses exactly the same ground — can read it instead
/// of asking [`incoherent_velocity`] the same question about the same grid a
/// second time.
///
/// `None` means **no mask was produced**, never "no ground was refused": the
/// two early returns below reach it before [`incoherent_velocity`] runs, and
/// [`DealiasKnobs::refuse_incoherent`] off is a posture that refuses nothing
/// by construction. A caller that needs the mask must compute it itself on
/// `None`; it must not read the absence as an empty mask.
pub(crate) fn dealias_with_knobs(
    vel_grid: &mut [Vec<f64>],
    sweep: &VelocitySweep,
    elevation_deg: f64,
    profile: Option<&WindProfile>,
    knobs: DealiasKnobs,
) -> Option<Vec<bool>> {
    let nyquist = fold_limit_ms(sweep, vel_grid)?;
    let interval = 2.0 * nyquist;
    let n = vel_grid.len();
    let gc = sweep.gate_count;
    if n < 8 {
        return None;
    }
    // Every pass below propagates a fold decision from one gate to a
    // neighbouring one, so every one of them needs to know where the sweep's
    // rows end. On a rotation they do not end, and this is the wrap it always
    // was; on a sector the two ends are the two edges of the arc, and a
    // decision carried across them is carried across ground the antenna never
    // pointed at.
    let rows = sweep_rows(sweep, n);
    // Where each row points, in radians, for the two wind seeds below. Both
    // ask [`WindProfile::predict`] what the environment does along one line of
    // sight, and a line of sight is an angle in the sky, not a position in the
    // sweep — the same azimuth [`WindProfileBuilder::add_sweep`] fitted the
    // profile at, which is what makes prediction and fit the same wind.
    //
    // Hoisted out of the tile loop as well as the row loop: seed 1 visits
    // every row once per 10-gate tile column, so a 1832-gate super-res cut
    // re-derived each row's angle 184 times.
    //
    // `Option` per row, because the azimuth slice's length is the caller's to
    // decide (see `sweep_rows`, which takes the row count separately). A row
    // with no azimuth gets no prediction, and both seeds already treat "the
    // profile predicts nothing here" as "do not seed".
    let az_rad: Vec<Option<f64>> = (0..n)
        .map(|i| sweep.azimuths_deg.get(i).map(|a| a.to_radians()))
        .collect();
    let reported: Vec<Vec<f64>> = vel_grid.to_vec();
    // Velocity with no coherent solution is set aside before any pass runs:
    // the passes see it as the absence it is, and the write-back below returns
    // it exactly as the radar reported it. Neither unfolded, nor censored, nor
    // dropped — a dealiaser that cannot resolve a region makes no claim about
    // it, and leaves the incoherence there for whoever reads the field next.
    let refused = if knobs.refuse_incoherent {
        incoherent_velocity(&reported, rows, gc, sweep.gate_interval_km, nyquist)
    } else {
        vec![false; n * gc]
    };
    let mut raw = reported.clone();
    for (i, row) in raw.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            if refused[i * gc + j] {
                *v = f64::NAN;
            }
        }
    }
    let raw = raw;
    // value[i][j] holds the dealiased velocity once valid[i][j].
    let mut valid = vec![false; n * gc];
    let mut value = vec![f64::NAN; n * gc];
    let idx = |i: usize, j: usize| i * gc + j;
    let has = |i: usize, j: usize| !raw[i][j].is_nan();

    // Seed 1: environmental winds. 5-radial × 10-gate tiles where every
    // data gate sits within the tight threshold of the wind component are
    // valid at their raw values (raw acceptance — no interval testing).
    if let Some(wp) = profile {
        for ti in (0..n).step_by(5) {
            for tj in (0..gc).step_by(10) {
                let mut ok = true;
                let mut any = false;
                'tile: for (i, row) in raw.iter().enumerate().take((ti + 5).min(n)).skip(ti) {
                    let az = az_rad[i];
                    for (j, &v) in row.iter().enumerate().take((tj + 10).min(gc)).skip(tj) {
                        if v.is_nan() {
                            continue;
                        }
                        any = true;
                        let r = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
                        match az.and_then(|az| wp.predict(az, r, elevation_deg)) {
                            Some(pred) if (v - pred).abs() < DA_SEED_TOL => {}
                            _ => {
                                ok = false;
                                break 'tile;
                            }
                        }
                    }
                }
                if ok && any {
                    for i in ti..(ti + 5).min(n) {
                        for j in tj..(tj + 10).min(gc) {
                            if has(i, j) {
                                valid[idx(i, j)] = true;
                                value[idx(i, j)] = raw[i][j];
                            }
                        }
                    }
                }
            }
        }
    }
    // Seed 1b: gate-level wind seeds — a gate whose raw value matches the
    // wind component, with at least DA_SEEDGATE_NEIGHBORS of its 4 neighbors
    // also matching, is valid at raw.
    if let Some(wp) = profile {
        let close = |i: usize, j: usize| -> Option<bool> {
            if !has(i, j) {
                return None;
            }
            let az = az_rad[i]?;
            let r = sweep.first_gate_range_km + j as f64 * sweep.gate_interval_km;
            wp.predict(az, r, elevation_deg)
                .map(|pred| (raw[i][j] - pred).abs() < DA_SEED_TOL)
        };
        let mut cand = vec![false; n * gc];
        for i in 0..n {
            for j in 0..gc {
                if valid[idx(i, j)] || close(i, j) != Some(true) {
                    continue;
                }
                let mut agree = 0;
                // A row past the end of an arc does not agree, in the same
                // way the gate before the first does not: `nj < gc` is that
                // same absence on the range axis. A gate on either edge of a
                // sector therefore needs all three neighbours it has, which
                // is what a gate at the first range gate has always needed.
                for (ni, nj) in [
                    (rows.neighbour(i, -1), j),
                    (rows.neighbour(i, 1), j),
                    (Some(i), j.wrapping_sub(1)),
                    (Some(i), j + 1),
                ] {
                    if let Some(ni) = ni
                        && nj < gc
                        && close(ni, nj) == Some(true)
                    {
                        agree += 1;
                    }
                }
                if agree >= DA_SEEDGATE_NEIGHBORS {
                    cand[idx(i, j)] = true;
                }
            }
        }
        // The reference has no gate-level seeding — this pass approximates
        // the tile seeds at finer granularity, so hold it to the same
        // measured region-size gate as kept-raw data: candidate components
        // smaller than DA_RAWMIN_BINS are not seeds.
        let mut seen = vec![false; n * gc];
        for si in 0..n {
            for sj in 0..gc {
                let s0 = idx(si, sj);
                if !cand[s0] || seen[s0] {
                    continue;
                }
                let mut comp = vec![(si, sj)];
                seen[s0] = true;
                let mut q = vec![(si, sj)];
                while let Some((ci, cj)) = q.pop() {
                    // Candidate pockets at the two ends of a sector are two
                    // pockets, each held to `DA_RAWMIN_BINS` on its own.
                    let neigh = [
                        (rows.neighbour(ci, -1), cj),
                        (rows.neighbour(ci, 1), cj),
                        (Some(ci), cj.wrapping_sub(1)),
                        (Some(ci), cj + 1),
                    ];
                    for (ni, nj) in neigh {
                        let Some(ni) = ni else {
                            continue;
                        };
                        if nj >= gc {
                            continue;
                        }
                        let ix = idx(ni, nj);
                        if cand[ix] && !seen[ix] {
                            seen[ix] = true;
                            comp.push((ni, nj));
                            q.push((ni, nj));
                        }
                    }
                }
                if comp.len() >= DA_RAWMIN_BINS {
                    for (ci, cj) in comp {
                        valid[idx(ci, cj)] = true;
                        value[idx(ci, cj)] = raw[ci][cj];
                    }
                }
            }
        }
    }

    // Seed 2: zero isodop near the radar, with a counterpart ~180° away.
    //
    // On an arc the counterpart is often not there to look at, and that is a
    // different answer from a radial 324° round the other way: a sector
    // narrower than 180° has no opposite radial for any of its rows, so this
    // seed finds nothing there rather than confirming a near-zero gate
    // against one of its own. Where the arc is wide enough to hold both ends
    // of a diameter, the counterpart lies forward of some rows and behind
    // others — the pairing is symmetric, so both are tried, and on a rotation
    // the forward lookup always answers and the second never runs.
    let half_turn = half_turn_rows(rows);
    let opposite = |i: usize| {
        rows.neighbour(i, half_turn)
            .or_else(|| rows.neighbour(i, -half_turn))
    };
    let near_gates = ((40.0 - sweep.first_gate_range_km) / sweep.gate_interval_km) as usize;
    for i in 0..n {
        let Some(opp) = opposite(i) else {
            continue;
        };
        for j in 0..near_gates.min(gc) {
            if has(i, j)
                && raw[i][j].abs() < DA_ZISO_TOL
                && (0..3).any(|d| {
                    rows.neighbour(opp, d)
                        .is_some_and(|o| has(o, j) && raw[o][j].abs() < DA_ZISO_TOL)
                })
            {
                valid[idx(i, j)] = true;
                value[idx(i, j)] = raw[i][j];
            }
        }
    }

    let unfold =
        |v: f64, reference: f64| -> f64 { v + ((reference - v) / interval).round() * interval };

    // Robust directional unfold chain over a gap (NaN = missing gate):
    // references the running mean of the last ≤3 accepted values and skips
    // isolated outliers (left uncommitted), aborting only when over a third
    // of the gap's data gates fail. Measured: a fragile strict chain reaches
    // reference coverage only with its thresholds widened far enough that
    // the radial bridge mis-unfolds pockets the reference keeps. The coverage
    // figures behind that were not preserved — branch `campaign-harness` has
    // the apparatus that took them and no trace of what it read.
    let chain = |seed: f64, raws: &[f64], t: f64, gap_free: i32| -> Option<Vec<f64>> {
        let mut out = Vec::with_capacity(raws.len());
        let mut acc: Vec<f64> = vec![seed];
        let mut fails = 0usize;
        let mut datag = 0usize;
        let mut gap = 0i32;
        for &r in raws {
            if r.is_nan() {
                out.push(f64::NAN);
                gap += 1;
                continue;
            }
            let jumped = gap_free > 0 && gap >= gap_free;
            gap = 0;
            if jumped {
                // Re-entry after a long gap: unfold to the nearest branch of
                // the carried reference without a continuity test; the
                // two-direction identity check is the acceptance criterion.
                let refm = *acc.last().unwrap();
                let u = unfold(r, refm);
                out.push(u);
                acc.clear();
                acc.push(u);
                datag += 1;
                continue;
            }
            datag += 1;
            let take = acc.len().min(3);
            let refm: f64 = acc[acc.len() - take..].iter().sum::<f64>() / take as f64;
            let u = unfold(r, refm);
            if (u - refm).abs() > t {
                fails += 1;
                out.push(f64::NAN);
                if fails * 3 > datag {
                    return None;
                }
                continue;
            }
            out.push(u);
            acc.push(u);
        }
        Some(out)
    };

    // Two-direction agreement: one-sided gates are skipped; a two-sided
    // disagreement rejects the whole bridge.
    let bridge_reject = |fwd: &[f64], bwd: &[f64]| {
        fwd.iter()
            .zip(bwd)
            .any(|(a, b)| !a.is_nan() && !b.is_nan() && (a - b).abs() >= 0.01)
    };

    for _pass in 0..DA_PASSES {
        let mut changed = false;

        // (a) radial bridge (and (e): the skip variant tolerates ND inside).
        for skip_nd in [false, true] {
            let t = if skip_nd { 0.45 } else { 0.6 } * nyquist * DA_THRESH_SCALE;
            for i in 0..n {
                let mut j = 0;
                while j < gc {
                    if !valid[idx(i, j)] {
                        j += 1;
                        continue;
                    }
                    // find next valid gate beyond a run of invalid data gates
                    let mut k = j + 1;
                    let mut any_gap = false;
                    while k < gc && !valid[idx(i, k)] {
                        if has(i, k) {
                            any_gap = true;
                        } else if !skip_nd {
                            break;
                        }
                        k += 1;
                    }
                    if k >= gc || !valid[idx(i, k)] || !any_gap {
                        j = k.max(j + 1);
                        continue;
                    }
                    // outward from j, inward from k; commit where they agree
                    let raws_f: Vec<f64> = ((j + 1)..k)
                        .map(|m| if has(i, m) { raw[i][m] } else { f64::NAN })
                        .collect();
                    let raws_b: Vec<f64> = raws_f.iter().rev().copied().collect();
                    let gf = if skip_nd { DA_GAPJUMP_GATES } else { 0 };
                    let fwd = chain(value[idx(i, j)], &raws_f, t, gf);
                    let bwd = chain(value[idx(i, k)], &raws_b, t, gf).map(|mut v| {
                        v.reverse();
                        v
                    });
                    if let (Some(fwd), Some(bwd)) = (fwd, bwd)
                        && !bridge_reject(&fwd, &bwd)
                    {
                        for (off, (a, b)) in fwd.iter().zip(&bwd).enumerate() {
                            if !a.is_nan() && !b.is_nan() {
                                valid[idx(i, j + 1 + off)] = true;
                                value[idx(i, j + 1 + off)] = *a;
                                changed = true;
                            }
                        }
                    }
                    j = k;
                }
            }
        }

        // (b) azimuthal bridge, tighter threshold; azimuth wraps where the
        // sweep closes the circle.
        let t_b = 0.35 * nyquist * DA_THRESH_SCALE;
        for j in 0..gc {
            for start in 0..n {
                if !valid[idx(start, j)] {
                    continue;
                }
                // The rows the walk crosses on its way to `end`, in order.
                // Recorded as it goes rather than recomputed from `start + m`,
                // because past the end of an arc there is no such row: the
                // walk stops there with no `end` to bridge to, which is what
                // it already does at a gate the radar saw nothing in.
                let mut gap = [0usize; 39];
                let mut k = 1;
                let mut end = None;
                while k < 40 {
                    let Some(ii) = rows.neighbour(start, k as i32) else {
                        break;
                    };
                    if valid[idx(ii, j)] {
                        end = Some(ii);
                        break;
                    }
                    if !has(ii, j) {
                        break;
                    }
                    gap[k - 1] = ii;
                    k += 1;
                }
                // `k == 1` is `end` sitting in the next row along, with no gap
                // between the two to bridge.
                let Some(end) = end.filter(|_| k > 1) else {
                    continue;
                };
                let raws_f: Vec<f64> = gap[..k - 1].iter().map(|&ii| raw[ii][j]).collect();
                let raws_b: Vec<f64> = raws_f.iter().rev().copied().collect();
                let fwd = chain(value[idx(start, j)], &raws_f, t_b, 0);
                let bwd = chain(value[idx(end, j)], &raws_b, t_b, 0).map(|mut v| {
                    v.reverse();
                    v
                });
                if let (Some(fwd), Some(bwd)) = (fwd, bwd)
                    && !bridge_reject(&fwd, &bwd)
                {
                    for (off, (a, b)) in fwd.iter().zip(&bwd).enumerate() {
                        if !a.is_nan() && !b.is_nan() {
                            let ii = gap[off];
                            valid[idx(ii, j)] = true;
                            value[idx(ii, j)] = *a;
                            changed = true;
                        }
                    }
                }
            }
        }

        // (c)+(d) flood fills: runs of ≥10 unvalidated data gates alongside a
        // valid neighbour radial; raw acceptance at the (c) threshold,
        // unfolding acceptance at the tighter (d) threshold. Run-mean
        // decisions, not per-gate: the run's mean deviation from the neighbor
        // radial decides; individual gates only need to stay within 2t.
        for aliased in [false, true] {
            let t = if aliased {
                DA_FLOOD_ALIASED_FRAC
            } else {
                DA_FLOOD_RAW_FRAC
            } * nyquist
                * DA_THRESH_SCALE;
            for i in 0..n {
                // A row on the edge of a sector is flooded from the one side
                // it has a neighbour on. The two directions run in order and
                // the second sees the first's writes, so which side a row is
                // missing is the side it is not filled from.
                for di in [-1i32, 1] {
                    let Some(ni) = rows.neighbour(i, di) else {
                        continue;
                    };
                    let mut run = 0usize;
                    for j in 0..gc {
                        let cand = has(i, j) && !valid[idx(i, j)] && valid[idx(ni, j)];
                        if cand {
                            run += 1;
                        } else {
                            run = 0;
                            continue;
                        }
                        if run >= 10 {
                            let lo = j + 1 - run;
                            let devs: Vec<(usize, f64, f64)> = (lo..=j)
                                .map(|m| {
                                    let neigh = value[idx(ni, m)];
                                    let u = if aliased {
                                        unfold(raw[i][m], neigh)
                                    } else {
                                        raw[i][m]
                                    };
                                    (m, u, u - neigh)
                                })
                                .collect();
                            let mean = devs.iter().map(|d| d.2).sum::<f64>() / run as f64;
                            if mean.abs() < t {
                                for &(m, u, d) in &devs {
                                    if d.abs() < 2.0 * t {
                                        valid[idx(i, m)] = true;
                                        value[idx(i, m)] = u;
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // (f) head and shoulders: single invalid gate matching the average of
        // three valid gates on each side along the radial.
        for i in 0..n {
            for j in 3..gc.saturating_sub(3) {
                if !has(i, j) || valid[idx(i, j)] {
                    continue;
                }
                let before: Vec<f64> = (j - 3..j)
                    .filter(|&m| valid[idx(i, m)])
                    .map(|m| value[idx(i, m)])
                    .collect();
                let after: Vec<f64> = (j + 1..j + 4)
                    .filter(|&m| valid[idx(i, m)])
                    .map(|m| value[idx(i, m)])
                    .collect();
                if before.len() == 3 && after.len() == 3 {
                    let avg = (before.iter().sum::<f64>() + after.iter().sum::<f64>()) / 6.0;
                    let u = unfold(raw[i][j], avg);
                    if (u - avg).abs() < 0.3 * nyquist * DA_THRESH_SCALE {
                        valid[idx(i, j)] = true;
                        value[idx(i, j)] = u;
                        changed = true;
                    }
                }
            }
        }

        if !changed {
            break;
        }
    }

    // Convert unresolved to ND; write dealiased values back. Never-reached
    // data gates keep their raw values in bulk — measured: the reference
    // dealiaser resolves ALL present data,
    // including isolated gates no propagation pass can reach —
    // unresolved-to-ND conversion evidently applies to contradictory
    // bridging, not unreached data. Size-gate the kept-raw regions: connected
    // components (4-adjacency, azimuth wrapping where the sweep closes the
    // circle) of unreached data gates below the measured minimum are dropped
    // to ND.
    let mut keep_raw = vec![false; n * gc];
    let mut seen = vec![false; n * gc];
    for si in 0..n {
        for sj in 0..gc {
            let s0 = idx(si, sj);
            if seen[s0] || valid[s0] || !has(si, sj) {
                continue;
            }
            let mut comp = vec![(si, sj)];
            seen[s0] = true;
            let mut q = vec![(si, sj)];
            while let Some((ci, cj)) = q.pop() {
                let neigh = [
                    (rows.neighbour(ci, -1), cj),
                    (rows.neighbour(ci, 1), cj),
                    (Some(ci), cj.wrapping_sub(1)),
                    (Some(ci), cj + 1),
                ];
                for (ni, nj) in neigh {
                    let Some(ni) = ni else {
                        continue;
                    };
                    if nj >= gc {
                        continue;
                    }
                    let ix = idx(ni, nj);
                    if !seen[ix] && !valid[ix] && has(ni, nj) {
                        seen[ix] = true;
                        comp.push((ni, nj));
                        q.push((ni, nj));
                    }
                }
            }
            if comp.len() >= knobs.rawmin_bins {
                for (ci, cj) in comp {
                    keep_raw[idx(ci, cj)] = true;
                }
            }
        }
    }
    for i in 0..n {
        for j in 0..gc {
            vel_grid[i][j] = if refused[idx(i, j)] {
                reported[i][j]
            } else if valid[idx(i, j)] {
                value[idx(i, j)]
            } else if keep_raw[idx(i, j)] {
                raw[i][j]
            } else {
                f64::NAN
            };
        }
    }
    // Post-dealias fold censor: a bin more than CENSOR_VNY_FRAC·Vny from any
    // 4-neighbor marks a fold wall no pass could place — kept-raw folded
    // regions meet correctly unfolded ones exactly there.
    if knobs.censor_vny_frac.is_infinite() {
        return knobs.refuse_incoherent.then_some(refused);
    }
    let snapshot: Vec<Vec<f64>> = vel_grid.to_vec();
    let censor_at = knobs.censor_vny_frac * nyquist;
    for i in 0..n {
        for j in 0..gc {
            // A refused bin is not a fold wall this pass failed to place; it
            // is a bin no pass made a claim about, and the censor has no
            // claim to withdraw. It is not evidence against its neighbours
            // either, for the same reason.
            if refused[idx(i, j)] {
                continue;
            }
            let v = snapshot[i][j];
            if v.is_nan() {
                continue;
            }
            let nb_of = |i: usize, j: usize| {
                if refused[idx(i, j)] {
                    f64::NAN
                } else {
                    snapshot[i][j]
                }
            };
            // A row at the edge of an arc has no neighbour on that side, in
            // exactly the sense the first and last gate of a radial have
            // none: there is no jump to measure, so nothing to censor for.
            let up = rows.neighbour(i, 1).map_or(f64::NAN, |k| nb_of(k, j));
            let down = rows.neighbour(i, -1).map_or(f64::NAN, |k| nb_of(k, j));
            let right = if j + 1 < gc {
                nb_of(i, j + 1)
            } else {
                f64::NAN
            };
            let left = if j > 0 { nb_of(i, j - 1) } else { f64::NAN };
            for nb in [up, down, left, right] {
                if !nb.is_nan() && (nb - v).abs() > censor_at {
                    vel_grid[i][j] = f64::NAN;
                    break;
                }
            }
        }
    }
    // `refuse_incoherent` off did not compute a mask — it stood in an
    // all-`false` one so the passes above could read it unconditionally — and
    // handing that back would tell a caller "nothing is incoherent here" about
    // a question this dealiasing never asked.
    knobs.refuse_incoherent.then_some(refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the stencil fixtures below sweep a full turn of synthetic
    // azimuths; nothing outside the tests reaches for π any more, now that
    // the wind fit reads the angle each radial declares.
    use std::f64::consts::PI;

    /// The hoisted beam height is the shared one, bit for bit.
    ///
    /// This module shares [`crate::beam::RE_EFF_KM`] but still writes the height
    /// arithmetic out once, in [`height_km_with_sin_el`], because
    /// [`WindProfileBuilder::add_sweep`] hoists `sin(elevation)` across tens of
    /// thousands of gates. Sharing a constant does not stop an expression from
    /// drifting, and NROT is the calibrated path the five pinned echo-tops
    /// digests never touch — so the copy is pinned here instead.
    ///
    /// Bit-exact rather than approximate on purpose: `add_sweep` bins samples
    /// with `(h / PROFILE_LAYER_KM) as usize`, a **floor**, so a single-ulp
    /// difference at a layer boundary moves a sample into the adjacent wind
    /// layer with no error, no NaN and no visible symptom.
    #[test]
    fn the_hoisted_beam_height_is_bit_identical_to_the_shared_one() {
        // The VCP 212 ladder, and gate centres out past the velocity extent.
        const ELEVS: [f64; 16] = [
            0.2, 0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0, 10.0, 12.0, 14.0, 16.7, 19.5,
        ];
        let mut checked = 0usize;
        for &e in &ELEVS {
            let sin_el = e.to_radians().sin();
            // 0.125 km first-gate centre, 0.25 km gates, 1200 gates -> 300 km.
            for j in 0..1200 {
                let r = 0.125 + j as f64 * 0.25;
                let hoisted = height_km_with_sin_el(r, sin_el);
                let shared = crate::beam::height_km(r, e);
                assert_eq!(
                    hoisted.to_bits(),
                    shared.to_bits(),
                    "the hoisted height drifted from `beam::height_km` at \
                     {r} km / {e}°: {hoisted} vs {shared}",
                );
                // The consequence that makes bit-identity load-bearing.
                assert_eq!(
                    (hoisted / PROFILE_LAYER_KM) as usize,
                    (shared / PROFILE_LAYER_KM) as usize,
                    "the two heights bin to different wind layers at \
                     {r} km / {e}°",
                );
                checked += 1;
            }
        }
        assert_eq!(
            checked,
            ELEVS.len() * 1200,
            "precondition: the grid did not cover every tilt × gate",
        );
    }

    fn sweep_for<'a>(
        vel_grid: &'a [Vec<f64>],
        azimuths_deg: &'a [f64],
        gate_count: usize,
    ) -> VelocitySweep<'a> {
        VelocitySweep {
            vel_grid,
            azimuths_deg,
            gate_count,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.25,
            declared_nyquist_ms: None,
            status: None,
        }
    }

    /// The fixtures declare nothing, so every existing expectation below is
    /// measured against [`estimate_nyquist`] exactly as it always was;
    /// `declaring` is how the two tests that are *about* the declaration state
    /// one.
    fn sweep<'a>(grid: &'a [Vec<f64>], azimuths: &'a [f64], gates: usize) -> VelocitySweep<'a> {
        VelocitySweep {
            vel_grid: grid,
            azimuths_deg: azimuths,
            gate_count: gates,
            first_gate_range_km: 0.25,
            gate_interval_km: 0.25,
            declared_nyquist_ms: None,
            status: None,
        }
    }

    fn ring_azimuths(n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64 * 360.0 / n as f64).collect()
    }

    // ---- the wind fit is in the ground frame ------------------------------
    //
    // A VAD cut carrying one known horizontal wind, laid down from whatever
    // azimuth the antenna happened to be at when the cut began. Real cuts
    // begin all over the circle — KCRP volume 2017-08-26 04:41:14 starts its
    // fifteen velocity cuts at 11.3°, 47.2°, 85.2°, … 104.5° — and the wind
    // over the radar is the same wind for every one of them.

    /// Range geometry for the synthetic cuts below: 200 gates a kilometre
    /// apart, which at 0.5° reaches 4.1 km and gives every layer it crosses
    /// thousands of samples, well clear of both the 200-sample floor and the
    /// [`PROFILE_MAX_SAMPLES`] ceiling.
    fn vad_sweep<'a>(grid: &'a [Vec<f64>], azimuths: &'a [f64]) -> VelocitySweep<'a> {
        VelocitySweep {
            vel_grid: grid,
            azimuths_deg: azimuths,
            gate_count: grid.first().map_or(0, Vec::len),
            first_gate_range_km: 1.0,
            gate_interval_km: 1.0,
            declared_nyquist_ms: None,
            status: None,
        }
    }

    /// `rows` radials `step_deg` apart from `az0`, every gate holding the
    /// radial component of `(u, v)`: `vr = u·sin(az)·cos(el) + v·cos(az)·cos(el)`.
    /// One wind at every height, so every layer must fit the same one.
    fn vad_cut(
        rows: usize,
        az0: f64,
        step_deg: f64,
        (u, v): (f64, f64),
        el_deg: f64,
    ) -> (Vec<Vec<f64>>, Vec<f64>) {
        let azimuths: Vec<f64> = (0..rows)
            .map(|i| (az0 + i as f64 * step_deg).rem_euclid(360.0))
            .collect();
        let cos_el = el_deg.to_radians().cos();
        let grid = azimuths
            .iter()
            .map(|a| {
                let r = a.to_radians();
                vec![(u * r.sin() + v * r.cos()) * cos_el; 200]
            })
            .collect();
        (grid, azimuths)
    }

    /// Heights every synthetic cut below reaches with room to spare.
    const VAD_PROBES: [f64; 4] = [0.15, 0.75, 1.65, 2.85];

    fn assert_wind(profile: &WindProfile, (u, v): (f64, f64), tol: f64, what: &str) {
        for h in VAD_PROBES {
            let (fu, fv) = profile
                .wind_at_km(h)
                .unwrap_or_else(|| panic!("{what}: no fit at {h} km"));
            assert!(
                (fu - u).abs() < tol && (fv - v).abs() < tol,
                "{what}: {h} km fitted ({fu:.4}, {fv:.4}), wind is ({u}, {v})",
            );
        }
    }

    /// The wind a cut measures does not depend on where the cut began.
    ///
    /// Both cuts carry the same 12 m/s westerly with a 5 m/s northerly
    /// component; one starts at north and one at 137.5°. Read against row
    /// index instead of azimuth the second returns that wind turned by 137.5°
    /// — (12, −5) arrives as (−5.5, 11.7), a 20° speed at 65° instead of a
    /// 13 m/s wind from 293°.
    #[test]
    fn a_cut_that_starts_off_north_fits_the_wind_a_cut_starting_at_north_does() {
        let wind = (12.0, -5.0);
        let mut fits = Vec::new();
        for az0 in [0.0, 137.5] {
            let (grid, azimuths) = vad_cut(360, az0, 1.0, wind, 0.5);
            let mut builder = WindProfileBuilder::new();
            builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
            fits.push(builder.finish().expect("a noiseless cut fits"));
        }
        assert_wind(&fits[0], wind, 1e-6, "cut starting at north");
        assert_wind(&fits[1], wind, 1e-6, "cut starting at 137.5°");
        for h in VAD_PROBES {
            let (a, b) = (fits[0].wind_at_km(h), fits[1].wind_at_km(h));
            let ((au, av), (bu, bv)) = (a.unwrap(), b.unwrap());
            // Not bit equality: the two cuts solve normal equations built from
            // the same angles in a different order, so they land within an ulp
            // or two of each other and of the wind. Everything this test is
            // about is orders of magnitude coarser than that.
            assert!(
                (au - bu).abs() < 1e-9 && (av - bv).abs() < 1e-9,
                "the two cuts disagree at {h} km: {a:?} against {b:?}",
            );
        }
    }

    /// The whole point of pooling a volume: four cuts, four start azimuths,
    /// four elevations, one atmosphere. Every sample in a layer has to be
    /// referred to the same north before the layer is solved, or the layer
    /// averages winds that disagree by whatever the cuts' starts disagree by
    /// — here 97.3°, 211.8° and 318.4°, which spans the compass.
    #[test]
    fn cuts_that_start_at_four_azimuths_pool_to_the_one_wind() {
        let wind = (-9.0, 14.0);
        let cuts: Vec<(Vec<Vec<f64>>, Vec<f64>, f64)> =
            [(0.0, 0.5), (97.3, 0.9), (211.8, 1.5), (318.4, 2.4)]
                .into_iter()
                .map(|(az0, el)| {
                    let (grid, azimuths) = vad_cut(360, az0, 1.0, wind, el);
                    (grid, azimuths, el)
                })
                .collect();
        let mut builder = WindProfileBuilder::new();
        for (grid, azimuths, el) in &cuts {
            builder.add_sweep(&vad_sweep(grid, azimuths), *el);
        }
        assert_wind(
            &builder.finish().expect("four noiseless cuts fit"),
            wind,
            1e-6,
            "four cuts pooled",
        );
    }

    /// A layer offered more than it can hold is fitted from all of it, thinned
    /// — not from the first of it.
    ///
    /// Two cuts of identical geometry, one carrying a 6 m/s westerly and one
    /// a 6 m/s southerly. Least squares over two stacked copies of one design
    /// is the mean of what each copy asks for, so the layer's answer is
    /// (3, 3) — a 4.2 m/s wind from 225° — and each cut on its own would say
    /// (6, 0) or (0, 6). Each cut offers this layer 69 120 samples against a
    /// [`PROFILE_MAX_SAMPLES`] of 16 384 — the pair thin to 8640 at a stride
    /// of 16 — so a layer that stopped at the cap would hold nothing but the
    /// first cut's opening rows and answer (6, 0).
    ///
    /// # Why 6 m/s and not 10
    ///
    /// Two contradictory winds are, by construction, a layer that does not fit
    /// a single VAD, and [`PROFILE_MAX_RMS_MS`] now measures exactly that. The
    /// residual against the pooled answer is `(v/2)·(cos − sin)` — amplitude
    /// `v·√2/2`, and over a uniform circle an RMS of exactly **`v/2`**. At the
    /// original 10 m/s that is 5.000 m/s against a ceiling of 4.990: the
    /// fixture missed it by two parts in a thousand, and the gate was right to
    /// refuse it. At 6 m/s the RMS is 3.0 and the fixture is clear of the
    /// ceiling by the same factor it was over it.
    ///
    /// The amplitude was never what this test is about. Halving both winds
    /// halves the answer and changes nothing about the thinning: a layer that
    /// kept a prefix still reads the first cut alone, and still answers
    /// `(6, 0)` instead of `(3, 3)`. What the scaling buys is that the two
    /// properties stay separable — this test fails for a broken cap, and
    /// `a_layer_whose_residual_clears_the_rpgs_ceiling_is_not_published` fails
    /// for a broken gate, neither standing in for the other.
    #[test]
    fn a_layer_offered_more_than_it_holds_is_fitted_from_the_whole_volume() {
        // 700 gates 50 m apart reach 35 km, which at 0.5° is still inside the
        // second layer: nearly every sample lands in the 0–0.3 km one.
        let cut = |(u, v): (f64, f64)| -> (Vec<Vec<f64>>, Vec<f64>) {
            let azimuths: Vec<f64> = (0..360).map(|i| i as f64).collect();
            let cos_el = 0.5f64.to_radians().cos();
            let grid = azimuths
                .iter()
                .map(|a| {
                    let r = a.to_radians();
                    vec![(u * r.sin() + v * r.cos()) * cos_el; 700]
                })
                .collect();
            (grid, azimuths)
        };
        let mut builder = WindProfileBuilder::new();
        for wind in [(6.0, 0.0), (0.0, 6.0)] {
            let (grid, azimuths) = cut(wind);
            builder.add_sweep(
                &VelocitySweep {
                    vel_grid: &grid,
                    azimuths_deg: &azimuths,
                    gate_count: 700,
                    first_gate_range_km: 0.05,
                    gate_interval_km: 0.05,
                    declared_nyquist_ms: None,
                    status: None,
                },
                0.5,
            );
        }
        let (u, v) = builder
            .finish()
            .expect("two oversubscribed cuts fit")
            .wind_at_km(0.15)
            .expect("the 0–0.3 km layer is the one they filled");
        // The thinned set is an arithmetic progression over both cuts' offers,
        // so the two are represented to within one sample of each other; the
        // slack here is that one sample and the round-off of nine thousand
        // normal-equation terms.
        assert!(
            (u - 3.0).abs() < 0.01 && (v - 3.0).abs() < 0.01,
            "the layer fitted ({u:.4}, {v:.4}), not the (3, 3) both cuts average to",
        );
    }

    /// A sector holds an arc, and an arc of a sinusoid still determines it.
    /// 90° of 0.5° radials — the narrowest the chunk feed hands over as a
    /// usable cut — recovers (7, 11) m/s to 7e-12 m/s. Read
    /// against row index the same 181 rows are stretched around a full
    /// circle, and the three-parameter fit then has nothing to do with the
    /// field it was given.
    #[test]
    fn a_ninety_degree_sector_fits_the_wind_over_the_arc_it_has() {
        let wind = (7.0, 11.0);
        let (grid, azimuths) = vad_cut(181, 42.0, 0.5, wind, 0.5);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        assert_wind(
            &builder
                .finish()
                .expect("a 90° arc still determines a VAD fit"),
            wind,
            1e-9,
            "90° sector",
        );
    }

    /// [`vad_cut`] with a residual of a **chosen size** laid over the wind:
    /// `+amp` on even gate indices, `−amp` on odd ones.
    ///
    /// The perturbation alternates along the beam, so within any one row — and
    /// every sample of a row shares that row's `(sin, cos)` — it sums to zero
    /// across the gates the fit reads. It is therefore very nearly orthogonal
    /// to the design `(sin·cos el, cos·cos el, 1)`: the solved wind stays the
    /// planted one and **every** residual is `±amp`, so the layer's RMS
    /// residual is `amp` by construction rather than by anything this module
    /// computes. That is what makes the gate test below non-circular — the
    /// number the test asserts against is one the test itself put there.
    fn vad_cut_noisy(
        rows: usize,
        (u, v): (f64, f64),
        el_deg: f64,
        amp: f64,
    ) -> (Vec<Vec<f64>>, Vec<f64>) {
        let azimuths: Vec<f64> = (0..rows).map(|i| i as f64).collect();
        let cos_el = el_deg.to_radians().cos();
        let grid = azimuths
            .iter()
            .map(|a| {
                let r = a.to_radians();
                let base = (u * r.sin() + v * r.cos()) * cos_el;
                (0..200)
                    .map(|j| if j % 2 == 0 { base + amp } else { base - amp })
                    .collect()
            })
            .collect();
        (grid, azimuths)
    }

    /// A layer that solves but does not *fit* is not a wind.
    ///
    /// The RPG publishes the goodness-of-fit ceiling its own VAD honours —
    /// RMS ≤ 9.7 kt — inside the NVW product, and across the thirteen volumes
    /// whose NVW we hold it never published a level above it. This module had
    /// the RPG's sample-count gate and its trim and not this one, so a layer
    /// fitted on a folded mess was published with no mark on it and the
    /// dealiaser seeded from it.
    ///
    /// 9.7 kt is 4.990 m/s. The two arms plant a residual of **4 m/s (7.8 kt)**
    /// and **6 m/s (11.7 kt)**, bracketing the ceiling from both sides with a
    /// margin far wider than the fit's own round-off. The planted wind is
    /// identical in both; only how well the gates agree with it changes.
    ///
    /// Every layer of the refused arm carries the same residual, so all of them
    /// fail together and the builder has no layer to publish — `finish` returns
    /// `None` rather than a profile of clamp-copies of nothing.
    #[test]
    fn a_layer_whose_residual_clears_the_rpgs_ceiling_is_not_published() {
        let wind = (9.0, -6.0);
        // 4 m/s of residual — 7.8 kt, inside the RPG's 9.7 — is a fit.
        let (grid, azimuths) = vad_cut_noisy(360, wind, 0.5, 4.0);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        let kept = builder
            .finish()
            .expect("a 7.8 kt residual is under the RPG's 9.7 kt ceiling");
        // The wind itself is untouched by the perturbation, which is what
        // "orthogonal to the design" buys and what makes the arms comparable.
        assert_wind(&kept, wind, 0.05, "4 m/s residual");

        // 6 m/s — 11.7 kt — is not, and no layer of it is.
        let (grid, azimuths) = vad_cut_noisy(360, wind, 0.5, 6.0);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        assert!(
            builder.finish().is_none(),
            "an 11.7 kt residual is over the RPG's 9.7 kt ceiling and must not \
             be published as a wind",
        );
    }

    /// A volume that never sampled 6 km does not get to answer at 6 km.
    ///
    /// [`vad_sweep`]'s geometry — 200 gates a kilometre apart from 1 km, at
    /// 0.5° — tops out at 199 km slant, which the beam puts at **4.068 km**.
    /// So layers 0–13 collect samples (720 at the thinnest, well over the
    /// 200-sample floor) and layers 14–39 collect none at all.
    ///
    /// [`PROFILE_FILL_MAX_LAYERS`] says the fill reaches three layers past the
    /// top fitted one and stops: 14, 15 and 16 are the nearest fitted layer's
    /// wind, and 17 upward are `None`. Unbounded — which is what `finish`
    /// shipped — **all** of 14–39 answered, so a profile fitted on a volume
    /// that saw nothing above 4 km reported a wind at 11.85 km, and
    /// `srv::BUNKERS_MIN_MEAN_LAYERS` counted twenty of twenty every time.
    ///
    /// The layer indices here are arithmetic on the fixture's own geometry,
    /// not readings taken from the profile under test.
    #[test]
    fn the_fill_reaches_three_layers_past_the_top_fitted_one_and_stops() {
        let wind = (13.0, 4.0);
        let (grid, azimuths) = vad_cut(360, 0.0, 1.0, wind, 0.5);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        let profile = builder.finish().expect("layers 0-13 fit");

        let centre = |l: usize| (l as f64 + 0.5) * PROFILE_LAYER_KM;
        // 13 is the top layer the beam reached, and it fit.
        assert!(
            profile.wind_at_km(centre(13)).is_some(),
            "layer 13 is the top layer this geometry samples and must fit",
        );
        // 14, 15, 16 are the fill, and carry layer 13's wind.
        for l in 14..=16 {
            let (u, v) = profile
                .wind_at_km(centre(l))
                .unwrap_or_else(|| panic!("layer {l} is within the fill's reach"));
            assert!(
                (u - wind.0).abs() < 1e-6 && (v - wind.1).abs() < 1e-6,
                "layer {l} is a clamp-copy and must carry the fitted wind",
            );
        }
        // 17 is one layer too far, and so is everything above it.
        for l in 17..PROFILE_LAYERS {
            assert!(
                profile.wind_at_km(centre(l)).is_none(),
                "layer {l} is {} layers past the top fitted one and must not \
                 answer; unbounded, this profile answered at every layer to 39",
                l - 13,
            );
        }
    }

    /// The two constructors fill by the same rule.
    ///
    /// `from_levels` has always bounded the fill to
    /// [`PROFILE_FILL_MAX_LAYERS`]; `finish` did not, and `finish` is the one
    /// the render path uses. This pins them together at the boundary — three
    /// layers out answers, four does not — so the rule cannot drift back apart
    /// in one of them.
    #[test]
    fn both_constructors_fill_to_the_same_reach() {
        let centre = |l: usize| (l as f64 + 0.5) * PROFILE_LAYER_KM;
        // One level, at the bottom: everything else is fill or nothing.
        let from_levels = WindProfile::from_levels(&[(0.0, 7.0, -3.0)]).expect("one level is a profile");
        let reach = PROFILE_FILL_MAX_LAYERS as usize;
        assert!(
            from_levels.wind_at_km(centre(reach)).is_some(),
            "from_levels must fill {reach} layers out",
        );
        assert!(
            from_levels.wind_at_km(centre(reach + 1)).is_none(),
            "from_levels must stop at {reach} layers out",
        );

        // And `finish`, whose top fitted layer is 13 by the geometry above.
        let (grid, azimuths) = vad_cut(360, 0.0, 1.0, (7.0, -3.0), 0.5);
        let mut builder = WindProfileBuilder::new();
        builder.add_sweep(&vad_sweep(&grid, &azimuths), 0.5);
        let finished = builder.finish().expect("layers 0-13 fit");
        assert!(
            finished.wind_at_km(centre(13 + reach)).is_some(),
            "finish must fill {reach} layers out, the same as from_levels",
        );
        assert!(
            finished.wind_at_km(centre(13 + reach + 1)).is_none(),
            "finish must stop at {reach} layers out, the same as from_levels",
        );
    }

    /// The ceiling is the RPG's published number, not a tuned one: 9.7 kt
    /// expressed in the m/s the fit works in.
    #[test]
    fn the_fit_quality_ceiling_is_the_rpgs_published_knots() {
        assert!(
            (PROFILE_MAX_RMS_MS - 9.7 * 0.514_444).abs() < 1e-12,
            "the ceiling must stay 9.7 kt converted, not a number chosen here",
        );
        // Bracketed by the two arms above, so neither is a boundary case.
        assert!(
            4.0 < PROFILE_MAX_RMS_MS && PROFILE_MAX_RMS_MS < 6.0,
            "the test's two residuals must straddle the ceiling",
        );
    }

    /// A field folded across the Nyquist limit must come back continuous:
    /// with an environmental wind profile seeding the dealiaser, the folded
    /// arcs unfold by one full 2·Vny interval instead of standing as phantom
    /// shear walls.
    #[test]
    fn dealias_unfolds_a_folded_patch() {
        let n = 72;
        let gates = 40;
        let nyquist = 25.0;
        // True field: a uniform 30 m/s southerly flow, vr = 30·cos(az).
        // |vr| > 25 folds in the arcs around az 0 and az 180.
        let azs: Vec<f64> = (0..n).map(|i| i as f64 * 360.0 / n as f64).collect();
        let true_v: Vec<f64> = azs.iter().map(|a| 30.0 * a.to_radians().cos()).collect();
        let mut grid: Vec<Vec<f64>> = true_v
            .iter()
            .map(|&v| {
                let folded = if v > nyquist {
                    v - 2.0 * nyquist
                } else if v < -nyquist {
                    v + 2.0 * nyquist
                } else {
                    v
                };
                vec![folded; gates]
            })
            .collect();
        // One bin pinned at the fold limit so the Nyquist estimate is exactly
        // 25 (az 90°, true vr ≈ 0 — an isolated spike the passes drop).
        grid[18][0] = 25.0;
        let wp = WindProfile::from_levels(&[(0.0, 0.0, 30.0)]).unwrap();

        let vg = grid.clone();
        let sw = sweep_for(&vg, &azs, gates);
        dealias(&mut grid, &sw, 0.5, Some(&wp), DealiasProfile::NoFalseShear);

        assert_eq!(grid[0][10], 30.0, "folded arc should unfold to +30");
        assert_eq!(grid[12][10], true_v[12], "unfolded flow must not move");

        // The Coverage profile shares every unfolding pass, so a field the
        // passes settle comes back identical under both postures.
        let mut coverage = vg.clone();
        dealias(&mut coverage, &sw, 0.5, Some(&wp), DealiasProfile::Coverage);
        assert_eq!(coverage[0][10], 30.0);
        assert_eq!(coverage[12][10], true_v[12]);
    }

    /// The wind seeds ask the profile about the sky, not about the sweep.
    ///
    /// The fixture above unfolds either way, because all forty of its gates
    /// sit inside 12 km and the zero-isodop seed reaches anything within 40:
    /// the near-zero rows anchor the field and the passes carry it round. This
    /// one puts its gates at 50–89 km, where that seed has nothing to work
    /// with (`near_gates` is zero), so the environmental wind is the only
    /// thing that can start the unfolding.
    ///
    /// A 30 m/s southerly is what `from_levels` states here and what
    /// [`crate::srv`] hands the SRV render: a wind over the radar, in the
    /// ground frame. Asked at row index instead, this cut's row 0 is "row 0 of
    /// 72" — due north, where the profile predicts +30 m/s — while the gate
    /// there holds the 21.2 m/s its true 45° azimuth carries. Nothing in the
    /// cut lands within the 4 m/s seed tolerance of its own row index: the two
    /// rows whose index prediction comes closest are the ones the fold has
    /// already moved 50 m/s. So no tile seeds, no gate seeds, no pass has
    /// anything to propagate, and the two folded arcs stay folded.
    #[test]
    fn the_wind_seeds_read_the_azimuth_the_antenna_pointed_at() {
        let n = 72;
        let gates = 40;
        let nyquist = 25.0;
        // 45° is a whole number of 5° rows, so this cut holds exactly the rows
        // a cut starting at north would, renumbered: its row 63 faces north.
        let azs: Vec<f64> = (0..n)
            .map(|i| (45.0 + i as f64 * 360.0 / n as f64).rem_euclid(360.0))
            .collect();
        let true_v: Vec<f64> = azs.iter().map(|a| 30.0 * a.to_radians().cos()).collect();
        let mut grid: Vec<Vec<f64>> = true_v
            .iter()
            .map(|&v| {
                let folded = if v > nyquist {
                    v - 2.0 * nyquist
                } else if v < -nyquist {
                    v + 2.0 * nyquist
                } else {
                    v
                };
                vec![folded; gates]
            })
            .collect();
        // One bin pinned at the fold limit so the Nyquist estimate is exactly
        // 25, as in the fixture above: the 5° rows straddle the ±25 crossing
        // rather than landing on it, so unaided the estimate would be the
        // 24.57 of the nearest row and every unfold would land 0.85 m/s short.
        // Row 9 faces 90°, where a southerly reads zero — an isolated spike
        // the passes drop.
        grid[9][0] = 25.0;
        let wp = WindProfile::from_levels(&[(0.0, 0.0, 30.0)]).unwrap();

        let vg = grid.clone();
        let sw = VelocitySweep {
            vel_grid: &vg,
            azimuths_deg: &azs,
            gate_count: gates,
            first_gate_range_km: 50.0,
            gate_interval_km: 1.0,
            // No declaration: this fixture's expectations were measured against
            // the estimator, which is what an undeclared sweep still reaches.
            declared_nyquist_ms: None,
            status: None,
        };
        dealias(&mut grid, &sw, 0.5, Some(&wp), DealiasProfile::NoFalseShear);

        assert_eq!(grid[63][10], 30.0, "the folded arc should unfold to +30");
        assert_eq!(grid[3][10], true_v[3], "unfolded flow must not move");
    }

    /// The profile parameter reaches only the post-pass censoring: an
    /// unreached data region smaller than `DA_RAWMIN_BINS` goes ND under
    /// `NoFalseShear` — today's tuned NROT behaviour, unchanged — and keeps
    /// its raw values under `Coverage`.
    #[test]
    fn a_small_unreached_region_is_nd_for_nrot_and_raw_for_coverage() {
        let n = 72;
        let gates = 40;
        // Nothing seeds: no wind profile, and no data near zero inside 40 km
        // (the zero-isodop band). A lone 2×3 patch of 20 m/s at long range is
        // unreachable by every propagation pass.
        let mut grid: Vec<Vec<f64>> = vec![vec![f64::NAN; gates]; n];
        for row in grid.iter_mut().take(32).skip(30) {
            for g in row.iter_mut().take(39).skip(36) {
                *g = 20.0;
            }
        }
        // One far bin pins the Nyquist estimate above the 8 m/s floor.
        grid[0][39] = 26.0;
        let azs = ring_azimuths(n);
        let vg = grid.clone();
        let sw = sweep_for(&vg, &azs, gates);

        let mut strict = grid.clone();
        dealias(&mut strict, &sw, 0.5, None, DealiasProfile::NoFalseShear);
        assert!(
            strict[30][37].is_nan(),
            "a 6-bin unreached region is under DA_RAWMIN_BINS and goes ND"
        );

        let mut coverage = grid.clone();
        dealias(&mut coverage, &sw, 0.5, None, DealiasProfile::Coverage);
        assert_eq!(
            coverage[30][37], 20.0,
            "Coverage keeps every unreached data gate at raw"
        );
    }

    /// A continuous field, even a sheared one, must pass through untouched:
    /// the zero-isodop seeds anchor it and every propagation pass then keeps
    /// raw values on the zero-fold branch.
    #[test]
    fn dealias_leaves_continuous_data_alone() {
        let n = 72;
        let gates = 40;
        let grid_orig: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                let v = 20.0 * (i as f64 / n as f64 * std::f64::consts::TAU).sin();
                vec![v; gates]
            })
            .collect();
        let mut grid = grid_orig.clone();
        let azs: Vec<f64> = (0..n).map(|i| i as f64 * 360.0 / n as f64).collect();
        let vg = grid.clone();
        let sw = sweep_for(&vg, &azs, gates);
        dealias(&mut grid, &sw, 0.5, None, DealiasProfile::NoFalseShear);
        assert_eq!(grid, grid_orig);
    }

    /// A sector's two edges are two edges, not a join. The same demand as
    /// [`dealias_leaves_continuous_data_alone`] — a continuous field passes
    /// through untouched — laid over 36° of 0.5° rows instead of around a
    /// rotation: 40 m/s of azimuthal shear from one end of the arc to the
    /// other, 0.56 m/s between adjacent rows, and nothing folded anywhere in
    /// it, since the Nyquist estimate is the 20 m/s the field itself reaches.
    ///
    /// What the two ends read *across* each other is the point. Rows 0 and 71
    /// stand 40 m/s apart because they are 35.5° apart in the sky, and the
    /// post-pass censor blanks any bin more than [`CENSOR_VNY_FRAC`]·Vny —
    /// 36 m/s here — from a 4-neighbour. Counted as neighbours, the two rows
    /// are a fold wall the passes could not place, and both go ND over all 40
    /// of their gates: 80 of the sector's 2880 bins erased out of a field with
    /// no fold in it.
    #[test]
    fn dealias_leaves_a_sectors_continuous_data_alone() {
        let n = 72;
        let gates = 40;
        let azimuths: Vec<f64> = (0..n).map(|i| i as f64 * 0.5).collect();
        let orig: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![-20.0 + 40.0 * i as f64 / (n - 1) as f64; gates])
            .collect();
        let mut grid = orig.clone();
        let vg = grid.clone();
        dealias(
            &mut grid,
            &sweep_for(&vg, &azimuths, gates),
            0.5,
            None,
            DealiasProfile::NoFalseShear,
        );
        assert_eq!(grid, orig);
    }

    /// A gust front is 11 m/s in and 11 m/s out across one line, and the
    /// declared limit is what tells that from a fold.
    ///
    /// The fixture is the shape a gust front makes: half the rotation inbound
    /// at 11 m/s, half outbound at 11 m/s, nothing else. Nothing in it is
    /// folded — the narrowest Doppler declaration measured across ten WSR-88D
    /// volumes is KTLX's 23.84 m/s, and the fastest gate here is under half of
    /// that.
    ///
    /// [`estimate_nyquist`] reads the fastest gate, so it answers **11**, and
    /// the post-pass censor then blanks any bin more than
    /// [`CENSOR_VNY_FRAC`]·Vny — 19.8 m/s — from a 4-neighbour. The two rows
    /// facing each other across the line stand 22 m/s apart, which under that
    /// limit is a fold wall no pass could have placed, so the censor erases
    /// them: 160 bins of the strongest convergence in the sweep, in a field
    /// with no fold anywhere in it. Told the 23.84 m/s the cut was flown at,
    /// the same wall sits inside a 42.9 m/s censor and stands.
    ///
    /// The two arms are 2.00·Vny and 0.92·Vny of jump, which is the same
    /// separation [`CENSOR_VNY_FRAC`]'s six-site ladder measures the reference
    /// making — an exact fold displacement goes, ordinary shear stays.
    ///
    /// The `None` arm is not scaffolding — it is what every reader of this
    /// module did before the declaration crossed the model boundary, and it is
    /// what a Message 1 volume still gets.
    #[test]
    fn a_declared_limit_keeps_a_shear_line_the_estimate_censors_as_a_fold() {
        /// KTLX's 0.5° Doppler cut, 2026-08-11 10:09 — the narrowest real
        /// declaration in the ten-volume WSR-88D control, so the fixture is
        /// tested against the tightest censor an archive has actually asked
        /// for.
        const DECLARED_MS: f64 = 23.84;
        let n = 72;
        let gates = 40;
        let azimuths = ring_azimuths(n);
        let orig: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![if i < n / 2 { 11.0 } else { -11.0 }; gates])
            .collect();

        let run = |declared: Option<f64>| {
            let mut grid = orig.clone();
            let vg = grid.clone();
            let mut sweep = sweep_for(&vg, &azimuths, gates);
            sweep.declared_nyquist_ms = declared;
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
            grid
        };

        let declared = run(Some(DECLARED_MS));
        assert_eq!(
            declared, orig,
            "the declared limit leaves a 22 m/s shear line exactly where it was",
        );

        let estimated = run(None);
        for row in [0usize, n / 2 - 1, n / 2, n - 1] {
            assert!(
                estimated[row].iter().all(|v| v.is_nan()),
                "row {row} faces the line and the 11 m/s estimate censors it",
            );
        }
        assert_eq!(
            estimated.iter().flatten().filter(|v| v.is_nan()).count(),
            4 * gates,
            "only the four rows either side of the two lines are erased",
        );
    }

    /// Both halves of the censor's job, on the two jumps the reference draws
    /// its line between.
    ///
    /// The limit is KHNX's 0.5° Doppler declaration, 11.66 m/s — one of the
    /// low-Nyquist cuts where the old 1.24 threshold erased ordinary shear.
    /// A ±0.85·Vny step is a 1.70·Vny jump, which the reference paints at
    /// +0.93 on the hovered ladder [`CENSOR_VNY_FRAC`] carries; a ±1.00·Vny
    /// step is a 2.00·Vny jump, exactly what one fold displaces a region by,
    /// and the reference paints nothing there. Nothing else differs between
    /// the two runs, so the pair is the censor's whole job in one fixture:
    /// keep the first field intact, erase the two rows either side of each of
    /// the second's two walls.
    #[test]
    fn the_censor_keeps_the_shear_the_reference_paints_and_drops_a_fold_displacement() {
        /// KHNX 2024-12-16 08:01:56, elevation 2, the cut the ladder was
        /// painted into.
        const VNY: f64 = 11.66;
        let n = 72;
        let gates = 40;
        let azimuths = ring_azimuths(n);
        let run = |amp: f64| {
            let orig: Vec<Vec<f64>> = (0..n)
                .map(|i| vec![if i < n / 2 { amp } else { -amp }; gates])
                .collect();
            let mut grid = orig.clone();
            let vg = grid.clone();
            let mut sweep = sweep_for(&vg, &azimuths, gates);
            sweep.declared_nyquist_ms = Some(VNY);
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
            (orig, grid)
        };

        let (orig, kept) = run(0.85 * VNY);
        assert_eq!(
            kept, orig,
            "a 1.70·Vny jump is shear, and the reference paints it",
        );

        let (_, walled) = run(VNY);
        for row in [0usize, n / 2 - 1, n / 2, n - 1] {
            assert!(
                walled[row].iter().all(|v| v.is_nan()),
                "row {row} faces a 2.00·Vny wall and must not survive it",
            );
        }
        assert_eq!(
            walled.iter().flatten().filter(|v| v.is_nan()).count(),
            4 * gates,
            "only the four rows either side of the two walls are erased",
        );
    }

    /// A declaration under [`crate::sampler::FOLD_LIMIT_FLOOR_MS`] is a
    /// mis-decoded field, not a very slow radar: no operational waveform folds
    /// at 3 m/s. It is refused and the estimate stands, which on
    /// [`a_declared_limit_keeps_a_shear_line_the_estimate_censors_as_a_fold`]'s
    /// fixture is the arm that erases the line.
    #[test]
    fn a_declaration_below_the_floor_falls_back_to_the_estimate() {
        let n = 72;
        let gates = 40;
        let azimuths = ring_azimuths(n);
        let orig: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![if i < n / 2 { 11.0 } else { -11.0 }; gates])
            .collect();
        let vg = orig.clone();
        let mut sweep = sweep_for(&vg, &azimuths, gates);

        sweep.declared_nyquist_ms = Some(3.0);
        assert_eq!(
            fold_limit_ms(&sweep, &orig),
            Some(11.0),
            "a sub-floor declaration is refused and the estimate answers",
        );
        sweep.declared_nyquist_ms = Some(crate::sampler::FOLD_LIMIT_FLOOR_MS);
        assert_eq!(
            fold_limit_ms(&sweep, &orig),
            Some(crate::sampler::FOLD_LIMIT_FLOOR_MS),
            "the floor itself is believed",
        );

        let mut grid = orig.clone();
        sweep.declared_nyquist_ms = Some(3.0);
        dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
        assert!(
            grid[0].iter().all(|v| v.is_nan()),
            "the sub-floor arm censors exactly as the estimate does",
        );
    }

    /// A sweep too slow for even the estimate abandons the pass outright,
    /// declaration or none: the field is returned untouched.
    #[test]
    fn a_sweep_under_the_floor_with_no_declaration_is_left_alone() {
        let n = 72;
        let gates = 40;
        let azimuths = ring_azimuths(n);
        let orig: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![if i < n / 2 { 3.0 } else { -3.0 }; gates])
            .collect();
        let vg = orig.clone();
        let sweep = sweep_for(&vg, &azimuths, gates);
        assert_eq!(fold_limit_ms(&sweep, &orig), None);
        let mut grid = orig.clone();
        dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
        assert_eq!(grid, orig);
    }

    // ---- velocity with no coherent solution --------------------------------

    /// A sweep of incoherent velocity beside a sweep that only folds, both
    /// built the same way and differing in one thing: whether the velocity
    /// under the wrap is a field or a coin toss.
    ///
    /// `wrapped` carries a true velocity that ramps across five full
    /// intervals along the beam, so its raw form jumps 2·Vny at every wrap
    /// and its texture is enormous — and every one of those jumps is the
    /// encoding, not the air. `noise` fills the same gates with independent
    /// draws over the whole interval. [`incoherent_velocity`] must refuse the
    /// second and not the first, since the first is exactly what a dealiaser
    /// is for.
    fn coherence_fixture(noise: bool) -> (Vec<Vec<f64>>, Vec<f64>, usize) {
        const VNY: f64 = 12.0;
        let (n, gates) = (360usize, 400usize);
        let azimuths = ring_azimuths(n);
        // A fixed multiplicative-congruential stream, so the fixture is the
        // same on every run and on every platform.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                (0..gates)
                    .map(|j| {
                        let v = if noise {
                            2.0 * VNY * next() - VNY
                        } else {
                            let truth = 10.0 * VNY * j as f64 / gates as f64
                                + 3.0 * (i as f64).to_radians().cos();
                            (truth + VNY).rem_euclid(2.0 * VNY) - VNY
                        };
                        (v / 0.5).round() * 0.5
                    })
                    .collect()
            })
            .collect();
        (grid, azimuths, gates)
    }

    /// Aliasing is not incoherence, and the statistic that separates them says
    /// so on a fixture where nothing else differs.
    #[test]
    fn a_fold_wall_is_coherent_and_a_coin_toss_is_not() {
        for (noise, want) in [(false, false), (true, true)] {
            let (grid, azimuths, gates) = coherence_fixture(noise);
            let sweep = sweep_for(&grid, &azimuths, gates);
            let rows = sweep_rows(&sweep, grid.len());
            let nyq = fold_limit_ms(&sweep, &grid).expect("a limit");
            let mask = incoherent_velocity(&grid, rows, gates, sweep.gate_interval_km, nyq);
            let refused = mask.iter().filter(|m| **m).count();
            let all = grid.len() * gates;
            if want {
                assert_eq!(refused, all, "every bin of a coin toss is refused");
            } else {
                assert_eq!(refused, 0, "a wrapping ramp is refused nowhere");
            }
        }
    }

    /// One physical field twice: as the short-pulse cut whose gates really are
    /// 0.5 km apart, and as the long-pulse cut that declares 0.25 km gates and
    /// fills each pair of them with one estimate.
    ///
    /// Returns `(coarse, replicated, azimuths)`. `replicated` is `coarse` with
    /// every value duplicated into an adjacent gate pair — bit-identical
    /// neighbours, exactly as a `pulse_width = 4` volume reports them (see
    /// [`TEXTURE_STEP_KM`]).
    ///
    /// One cell in `SPARSE` carries a `+1.00·Vny` spike, so a spike puts its
    /// two neighbouring cell pairs inside the straddling band and the fraction
    /// of straddling pairs is 2/`SPARSE` ≈ 5.9%. That is over
    /// [`COH_MAX_STRADDLE`]'s 3.9% and *under twice* it, which is the whole
    /// point: differencing adjacent gates on the replicated grid reads half
    /// the pairs as structurally zero, halving the fraction to ≈ 2.9% — under
    /// the threshold. The band is narrow because the dilution is exactly 2x
    /// and nothing else about the field changes, so if [`COH_MAX_STRADDLE`]
    /// moves, `SPARSE` has to move with it to stay inside (3.9%, 7.8%).
    ///
    /// The base is a wind rather than a constant for the same reason
    /// [`straddle_fixture`]'s is, and both terms stay inside the declared
    /// limit so nothing in it folds.
    fn long_pulse_fixture(n: usize, cells: usize) -> (Vec<Vec<f64>>, Vec<Vec<f64>>, Vec<f64>) {
        const SPARSE: usize = 34;
        let azimuths = ring_azimuths(n);
        let coarse: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                let base = 5.0 + 8.0 * azimuths[i].to_radians().cos();
                (0..cells)
                    .map(|k| {
                        if (i * 7 + k * 13) % SPARSE == 0 {
                            base + STRADDLE_VNY
                        } else {
                            base
                        }
                    })
                    .collect()
            })
            .collect();
        let replicated = coarse
            .iter()
            .map(|row| row.iter().flat_map(|&v| [v, v]).collect())
            .collect();
        (coarse, replicated, azimuths)
    }

    /// [`incoherent_velocity`] separates its samples by a physical distance
    /// converted through the declared gate spacing, and **not** by gate
    /// adjacency — which is the only reason long-pulse volumes do not defeat
    /// it.
    ///
    /// A `pulse_width = 4` WSR-88D volume declares 250 m gates and delivers
    /// 500 m content replicated exactly 2x: adjacent gates are bit-identical
    /// at every even-indexed pair. `dk = round(`[`TEXTURE_STEP_KM`]` / gi)` is
    /// 2 gates there, so the comparison straddles the replication period at
    /// every parity and never differences a gate against its own copy. Written
    /// as a bare `1` — which is what `((0.5 / gi).round() as usize).max(1)`
    /// invites a reader to "simplify" it to — half of every comparison is a
    /// value against itself, the straddling fraction halves, and the rule stops
    /// refusing. On KHNX 2024-12-16 08:01:56, the null control that must paint
    /// nothing, that is a collapse from 92.98% of bins refused to 0.45%: a 19x
    /// error, and the failure is **silent** — no panic, no NaN, no warning,
    /// just a control volume that quietly stops controlling.
    ///
    /// Three legs, the same physical field throughout:
    ///
    /// - the honest 0.5 km-gate declaration refuses it — the field really is
    ///   incoherent at 500 m, so the other two legs are about the grid and not
    ///   about the field;
    /// - the replicated 0.25 km-gate declaration refuses it too, because the
    ///   0.5 km span is conserved as 2 gates;
    /// - handing the replicated grid a 0.5 km spacing forces the separation to
    ///   one gate and the refusal disappears. That leg is the counterfactual,
    ///   and it is what makes the second leg discriminate: with the separation
    ///   hardcoded to 1 the second leg reads like the third and fails.
    #[test]
    fn the_gate_separation_is_a_distance_and_not_a_gate_count() {
        let (n, cells) = (360usize, 400usize);
        let (coarse, replicated, azimuths) = long_pulse_fixture(n, cells);
        assert!(
            (0..cells).all(|k| replicated[0][2 * k] == replicated[0][2 * k + 1]),
            "the fixture must carry 500 m content on a declared 250 m grid",
        );

        let coarse_sweep = VelocitySweep {
            vel_grid: &coarse,
            azimuths_deg: &azimuths,
            gate_count: cells,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.5,
            declared_nyquist_ms: Some(STRADDLE_VNY),
            status: None,
        };
        let rows = sweep_rows(&coarse_sweep, n);
        let refused = |grid: &[Vec<f64>], gc: usize, gi: f64| {
            incoherent_velocity(grid, rows, gc, gi, STRADDLE_VNY)
                .iter()
                .filter(|m| **m)
                .count()
        };

        // The field is incoherent at 500 m, whichever grid reports it.
        assert_eq!(
            refused(&coarse, cells, 0.5),
            n * cells,
            "the 0.5 km-gate cut must refuse every bin of it",
        );
        assert_eq!(
            refused(&replicated, 2 * cells, 0.25),
            n * 2 * cells,
            "and so must the same field replicated onto a 0.25 km grid",
        );

        // The counterfactual: one gate apart on the replicated grid, half of
        // every comparison is a value against its own copy.
        assert_eq!(
            refused(&replicated, 2 * cells, 0.5),
            0,
            "differencing adjacent gates on replicated content refuses nothing",
        );
    }

    /// What the dealiaser does with it: nothing at all. Not a value unfolded,
    /// not a gate censored, not a region dropped — the field comes back bit
    /// for bit as the radar reported it, which is the only honest answer where
    /// no assignment of fold branches explains it.
    #[test]
    fn the_dealiaser_hands_incoherent_velocity_back_as_reported() {
        let (orig, azimuths, gates) = coherence_fixture(true);
        let vg = orig.clone();
        let mut grid = orig.clone();
        dealias(
            &mut grid,
            &sweep_for(&vg, &azimuths, gates),
            0.5,
            None,
            DealiasProfile::NoFalseShear,
        );
        assert_eq!(grid, orig);
    }

    /// And rotation is not reported over it. Ungated, the estimator finds
    /// plenty there — a coin toss differentiates to whatever it likes — which
    /// is the whole reason the refusal is worth having.
    #[test]
    fn no_rotation_is_reported_over_velocity_with_no_coherent_solution() {
        let (grid, azimuths, gates) = coherence_fixture(true);
        let sweep = sweep_for(&grid, &azimuths, gates);
        let nrot = compute_nrot_grid(&sweep);
        assert!(
            nrot.iter().flatten().all(|v| v.is_nan()),
            "a coin toss carries no rotation to report",
        );
    }

    /// The display profile is untouched by any of it. Storm-relative velocity
    /// is measured against the RPG's own dealiased field, which resolves
    /// everything present, so the refusal is NROT's posture and not the
    /// module's.
    #[test]
    fn the_display_profile_still_resolves_incoherent_velocity() {
        assert!(DealiasProfile::NoFalseShear.knobs().refuse_incoherent);
        assert!(!DealiasProfile::Coverage.knobs().refuse_incoherent);
        let (orig, azimuths, gates) = coherence_fixture(true);
        let vg = orig.clone();
        let mut grid = orig.clone();
        dealias(
            &mut grid,
            &sweep_for(&vg, &azimuths, gates),
            0.5,
            None,
            DealiasProfile::Coverage,
        );
        assert_ne!(grid, orig, "the coverage posture still works the field");
    }

    // ---- the mask is built once and read twice -----------------------------

    /// Every f64 of a grid as the bits it is, so a comparison is exact over
    /// NaN — which is most of an NROT grid, and exactly the part these tests
    /// are about.
    fn bits(grid: &[Vec<f64>]) -> Vec<Vec<u64>> {
        grid.iter()
            .map(|row| row.iter().map(|v| v.to_bits()).collect())
            .collect()
    }

    /// The fold limit [`STRADDLE_FIXTURE`] declares. Declared rather than
    /// estimated so the two thresholds the fixture has to sit between are
    /// fixed numbers and not a property of the noise.
    const STRADDLE_VNY: f64 = 25.0;

    /// A smooth wind sprinkled with lone gates a whole interval away from it —
    /// the one fixture on which [`incoherent_velocity`] is the *deciding*
    /// rule.
    ///
    /// [`coherence_fixture`]'s coin toss is refused by the continuity ceiling
    /// as well, so it cannot show which rule blanked a bin. This one is built
    /// to sit between the two: one gate in `SPARSE` carries a `+1.00·Vny`
    /// spike, which puts 2/`SPARSE` ≈ 8.3% of the along-beam pairs inside the
    /// straddling band — over [`COH_MAX_STRADDLE`]'s 3.9%, so every bin is
    /// refused — while the rms of the same differences is
    /// √(0.083)·Vny ≈ 0.29·Vny, under [`GK_MAX_TEXTURE_VNY_FRAC`]'s 0.44, so
    /// the ceiling admits all of it. Take the refusal away and the sweep
    /// paints; leave it and the sweep is empty.
    ///
    /// The base is a wind and not a constant, because [`tap_stencil`]'s
    /// coherence gate reads a profile of no azimuthal variance as ND: a flat
    /// field is blanked by the stencil before either rule is reached, which
    /// would make the comparison vacuous. Both terms stay well inside the
    /// declared limit, so nothing in it folds.
    fn straddle_fixture(n: usize, gates: usize) -> (Vec<Vec<f64>>, Vec<f64>) {
        const SPARSE: usize = 24;
        let azimuths = ring_azimuths(n);
        let grid = (0..n)
            .map(|i| {
                let base = 5.0 + 8.0 * azimuths[i].to_radians().cos();
                (0..gates)
                    .map(|j| {
                        if (i * 7 + j * 13) % SPARSE == 0 {
                            base + STRADDLE_VNY
                        } else {
                            base
                        }
                    })
                    .collect()
            })
            .collect();
        (grid, azimuths)
    }

    fn straddle_sweep<'a>(
        grid: &'a [Vec<f64>],
        azimuths: &'a [f64],
        gates: usize,
    ) -> VelocitySweep<'a> {
        VelocitySweep {
            vel_grid: grid,
            azimuths_deg: azimuths,
            gate_count: gates,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.25,
            declared_nyquist_ms: Some(STRADDLE_VNY),
            status: None,
        }
    }

    /// [`dealias_with_knobs`] reports the mask it built, and reports **none**
    /// wherever it built none.
    ///
    /// The distinction is the whole safety of reusing it. An absent mask is
    /// "this dealiasing never asked the question", which a caller that needs
    /// the answer must go and compute; an all-`false` mask would be "asked and
    /// answered nowhere", and reading the first as the second is how a sweep
    /// silently loses its refusal.
    #[test]
    fn the_dealiaser_reports_the_mask_it_built_and_nothing_it_did_not() {
        let (orig, azimuths, gates) = coherence_fixture(true);
        let vg = orig.clone();
        let sweep = sweep_for(&vg, &azimuths, gates);
        let rows = sweep_rows(&sweep, orig.len());
        let nyq = fold_limit_ms(&sweep, &orig).expect("a limit");
        let want = incoherent_velocity(&orig, rows, gates, sweep.gate_interval_km, nyq);
        assert!(want.iter().any(|m| *m), "the fixture must refuse something");

        let mut grid = orig.clone();
        let got = dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear);
        assert_eq!(
            got.as_deref(),
            Some(want.as_slice()),
            "the mask handed out is the mask the passes ran against",
        );

        // The posture that refuses nothing produced no mask to hand out.
        let mut grid = orig.clone();
        assert_eq!(
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::Coverage),
            None,
            "a dealiasing that never asks the question reports no answer",
        );
    }

    /// The two early returns report no mask either — and they are the two the
    /// fallback in [`llsd_nrot`] exists for.
    #[test]
    fn a_dealiasing_that_returns_early_reports_no_mask() {
        // No fold limit: every gate is under `FOLD_LIMIT_FLOOR_MS`, so
        // `fold_limit_ms` abandons the pass before a mask is reachable.
        let calm: Vec<Vec<f64>> = vec![vec![1.5; 40]; 360];
        let calm_az = ring_azimuths(360);
        let vg = calm.clone();
        let sweep = sweep_for(&vg, &calm_az, 40);
        assert_eq!(fold_limit_ms(&sweep, &calm), None);
        let mut grid = calm.clone();
        assert_eq!(
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear),
            None,
        );

        // Too few rows to propagate a fold decision across.
        let (small, small_az) = straddle_fixture(7, 40);
        let vg = small.clone();
        let sweep = straddle_sweep(&vg, &small_az, 40);
        assert!(
            fold_limit_ms(&sweep, &small).is_some(),
            "this arm must fail on the row count and not on the limit",
        );
        let mut grid = small.clone();
        assert_eq!(
            dealias(&mut grid, &sweep, 0.5, None, DealiasProfile::NoFalseShear),
            None,
        );
    }

    /// Handed the dealiaser's mask, [`llsd_nrot`] produces exactly what it
    /// produces when it computes one itself — and neither is what an empty
    /// mask produces.
    ///
    /// The first equality is the change: the second call was reading the same
    /// grid, the same rows, the same gate interval and the same limit, so it
    /// could not have produced a different answer. The inequality is what
    /// makes the first mean anything.
    #[test]
    fn the_reused_mask_and_the_recomputed_one_are_the_same_grid() {
        let (orig, azimuths) = straddle_fixture(360, 400);
        let vg = orig.clone();
        let sweep = straddle_sweep(&vg, &azimuths, 400);
        let pre = preprocess_velocity_with(&sweep, 0.5, None);
        let (dealiased, med) = (&pre.dealiased, &pre.median);
        let mask = pre
            .refused
            .clone()
            .expect("a full sweep with a limit produces a mask");
        assert!(mask.iter().any(|m| *m), "the fixture must refuse something");

        let reused = llsd_nrot(&sweep, dealiased, med, Some(&mask));
        let recomputed = llsd_nrot(&sweep, dealiased, med, None);
        assert_eq!(bits(&reused), bits(&recomputed));

        let empty = vec![false; orig.len() * 400];
        let unrefused = llsd_nrot(&sweep, dealiased, med, Some(&empty));
        assert!(
            unrefused.iter().flatten().any(|v| !v.is_nan()),
            "the fixture must paint once the refusal is taken away",
        );
        assert_ne!(
            bits(&reused),
            bits(&unrefused),
            "an empty mask must not be what an absent one means",
        );
    }

    /// End to end on a sweep the dealiaser will not run on: the refusal
    /// survives.
    ///
    /// Seven rows is under the eight [`dealias_with_knobs`] needs, so the
    /// dealiaser returns before it ever computes a mask and
    /// [`compute_nrot_grid_with_profile`] has to compute one for itself. Seven
    /// rows still *close the circle* — 51.4° apart, accounting for all of it —
    /// so the stencils have their neighbours and there is a grid to compare.
    #[test]
    fn a_sweep_too_small_to_dealias_still_refuses_incoherent_velocity() {
        let (grid, azimuths) = straddle_fixture(7, 400);
        let vg = grid.clone();
        let sweep = straddle_sweep(&vg, &azimuths, 400);
        let rows = sweep_rows(&sweep, 7);
        assert!(rows.closed, "the fixture must give the stencils neighbours");

        let pre = preprocess_velocity_with(&sweep, 0.5, None);
        let (dealiased, med) = (&pre.dealiased, &pre.median);
        assert_eq!(pre.refused, None, "seven rows is too few to dealias");

        let nyq = fold_limit_ms(&sweep, &grid).expect("a limit");
        let mask = incoherent_velocity(&grid, rows, 400, sweep.gate_interval_km, nyq);
        assert!(mask.iter().any(|m| *m), "the fixture must refuse something");

        let finish = |mut g: Vec<Vec<f64>>| {
            despeckle_nrot(&mut g, DESPECKLE_MIN_BINS, rows);
            g
        };
        let want = finish(llsd_nrot(&sweep, dealiased, med, Some(&mask)));
        let unrefused = finish(llsd_nrot(
            &sweep,
            dealiased,
            med,
            Some(&vec![false; 7 * 400]),
        ));
        let got = compute_nrot_grid_with_profile(&sweep, 0.5, None);

        assert!(
            unrefused.iter().flatten().any(|v| !v.is_nan()),
            "the fixture must paint once the refusal is taken away",
        );
        assert_eq!(bits(&got), bits(&want), "the mask was recomputed, not lost");
        assert_ne!(
            bits(&got),
            bits(&unrefused),
            "and it refused ground an empty mask would have painted",
        );
    }

    /// A sweep with no fold limit has no continuity ceiling either, and that
    /// is why [`range_texture`] does not run on one.
    ///
    /// [`GK_MAX_TEXTURE_VNY_FRAC`] is a fraction *of the cut's own limit*, so
    /// where [`fold_limit_ms`] abandons the pass there is no number to compare
    /// a texture against and every gate passes the rule however rough it is.
    /// The fixture is rough on purpose — ±2.5 m/s square wave in range, four
    /// gates to the period, so the 0.5 km difference [`range_texture`]
    /// measures is 5 m/s everywhere — and every gate of it still paints,
    /// because 5.5 m/s peak is under [`crate::sampler::FOLD_LIMIT_FLOOR_MS`]
    /// and the sweep therefore declares nothing this rule could be scaled to.
    ///
    /// So the stage is dead work there rather than cheap work, which is what
    /// makes computing it beside its ceiling rather than before it a guard and
    /// not a reordering.
    #[test]
    fn a_sweep_with_no_fold_limit_has_no_continuity_ceiling() {
        let (n, gates) = (360usize, 400usize);
        let azimuths = ring_azimuths(n);
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                let base = 3.0 * azimuths[i].to_radians().cos();
                (0..gates)
                    .map(|j| base + if (j % 4) < 2 { 2.5 } else { -2.5 })
                    .collect()
            })
            .collect();
        let vg = grid.clone();
        let sweep = sweep_for(&vg, &azimuths, gates);
        assert_eq!(
            fold_limit_ms(&sweep, &grid),
            None,
            "the fixture must reach the arm this test is about",
        );

        let rows = sweep_rows(&sweep, n);
        let texture = range_texture(&grid, &sweep, rows);
        let coarsest = texture
            .iter()
            .flatten()
            .filter(|v| !v.is_nan())
            .fold(0.0f64, |a, &b| a.max(b));
        assert!(coarsest > 4.0, "the fixture must be rough: {coarsest}");

        let nrot = compute_nrot_grid_with_profile(&sweep, 0.5, None);
        assert!(
            nrot.iter().flatten().any(|v| !v.is_nan()),
            "with no limit there is no ceiling, so roughness refuses nothing",
        );
    }

    // ---- the row scratch is pooled, and no row can tell ---------------------

    /// A sweep of no gates is answered before either windowing stage splits
    /// its work.
    ///
    /// Both stages hold their per-row output flat and write it through
    /// `par_chunks_mut`, and **a zero chunk size is a panic there**, not an
    /// empty walk — `chunk_size must not be zero`. That makes the width an
    /// argument the stage has to check before it splits, where a plain
    /// `for j in 0..gc` needed no check because the body simply never ran.
    ///
    /// The limit is declared rather than estimated, so `fold_limit_ms` answers
    /// off the header and both guards are reached end to end: the dealiaser
    /// asks [`incoherent_velocity`] for a mask, and [`llsd_nrot`] has a ceiling
    /// and so asks [`range_texture`] for a field.
    #[test]
    fn a_sweep_with_no_gates_is_answered_before_the_work_is_split() {
        let n = 360usize;
        let grid: Vec<Vec<f64>> = vec![Vec::new(); n];
        let azimuths = ring_azimuths(n);
        let sweep = straddle_sweep(&grid, &azimuths, 0);
        let rows = sweep_rows(&sweep, n);
        assert_eq!(
            fold_limit_ms(&sweep, &grid),
            Some(STRADDLE_VNY),
            "the declaration must stand so both stages are reached",
        );

        let empty: Vec<Vec<f64>> = vec![Vec::new(); n];
        assert_eq!(range_texture(&grid, &sweep, rows), empty);
        assert!(
            incoherent_velocity(&grid, rows, 0, sweep.gate_interval_km, STRADDLE_VNY).is_empty(),
            "no gates is no mask, not a mask of nothing",
        );
        assert_eq!(compute_nrot_grid(&sweep), empty);
    }

    /// [`range_texture`]'s range pass with a fresh prefix-sum pair per row,
    /// and the squared difference laid down in a pass of its own — the shape
    /// the pooled version has to agree with, bit for bit.
    fn range_texture_fresh_scratch(
        grid: &[Vec<f64>],
        sweep: &VelocitySweep,
        rows: crate::azimuth::Rows,
    ) -> Vec<Vec<f64>> {
        let n = grid.len();
        let gc = sweep.gate_count;
        let dk = ((TEXTURE_STEP_KM / sweep.gate_interval_km).round() as usize).max(1);
        let gh = ((TEXTURE_RANGE_HALF_KM / sweep.gate_interval_km).round() as i32).max(1);
        let per_row: Vec<(Vec<f32>, Vec<u16>)> = (0..n)
            .map(|i| {
                let mut d2 = vec![0.0f64; gc];
                let mut ok = vec![0u32; gc];
                for j in 0..gc.saturating_sub(dk) {
                    let (a, b) = (grid[i][j], grid[i][j + dk]);
                    if !a.is_nan() && !b.is_nan() {
                        d2[j] = (b - a).powi(2);
                        ok[j] = 1;
                    }
                }
                let mut pre = vec![0.0f64; gc + 1];
                let mut pcn = vec![0u32; gc + 1];
                for j in 0..gc {
                    pre[j + 1] = pre[j] + d2[j];
                    pcn[j + 1] = pcn[j] + ok[j];
                }
                let (mut sum, mut cnt) = (vec![0.0f32; gc], vec![0u16; gc]);
                for j in 0..gc {
                    let lo = (j as i32 - gh).max(0) as usize;
                    let hi = ((j as i32 + gh) as usize).min(gc - 1);
                    sum[j] = (pre[hi + 1] - pre[lo]) as f32;
                    cnt[j] = (pcn[hi + 1] - pcn[lo]) as u16;
                }
                (sum, cnt)
            })
            .collect();
        (0..n)
            .map(|i| {
                (0..gc)
                    .map(|j| {
                        let (mut s, mut c) = (0.0f64, 0u32);
                        for da in -TEXTURE_AZ_HALF..=TEXTURE_AZ_HALF {
                            if let Some(ai) = rows.neighbour(i, da) {
                                s += f64::from(per_row[ai].0[j]);
                                c += u32::from(per_row[ai].1[j]);
                            }
                        }
                        if (c as usize) < TEXTURE_MIN_PAIRS {
                            f64::NAN
                        } else {
                            (s / c as f64).sqrt()
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// [`incoherent_velocity`]'s range pass with a fresh prefix-sum pair per
    /// row, and the mask gathered per row rather than written flat.
    fn incoherent_velocity_fresh_scratch(
        raw: &[Vec<f64>],
        rows: crate::azimuth::Rows,
        gc: usize,
        gate_interval_km: f64,
        nyquist: f64,
    ) -> Vec<bool> {
        let n = raw.len();
        let tol = COH_STRADDLE_VNY_FRAC * nyquist;
        let fold = COH_FOLD_VNY_FRAC * nyquist;
        let dk = ((TEXTURE_STEP_KM / gate_interval_km).round() as usize).max(1);
        let per_row: Vec<(Vec<u16>, Vec<u16>)> = (0..n)
            .map(|i| {
                let mut ps = vec![0u32; gc + 1];
                let mut pp = vec![0u32; gc + 1];
                for j in 0..gc {
                    let (mut s, mut p) = (0u32, 0u32);
                    if j + dk < gc {
                        let (a, b) = (raw[i][j], raw[i][j + dk]);
                        if !a.is_nan() && !b.is_nan() {
                            p = 1;
                            let dv = (b - a).abs();
                            s = u32::from(dv > tol && dv < fold);
                        }
                    }
                    ps[j + 1] = ps[j] + s;
                    pp[j + 1] = pp[j] + p;
                }
                let (mut s, mut p) = (vec![0u16; gc], vec![0u16; gc]);
                for j in 0..gc {
                    let lo = (j as i32 - COH_RANGE_HALF).max(0) as usize;
                    let hi = ((j as i32 + COH_RANGE_HALF) as usize).min(gc - 1);
                    s[j] = (ps[hi + 1] - ps[lo]) as u16;
                    p[j] = (pp[hi + 1] - pp[lo]) as u16;
                }
                (s, p)
            })
            .collect();
        (0..n)
            .flat_map(|i| {
                (0..gc)
                    .map(|j| {
                        if raw[i][j].is_nan() {
                            return false;
                        }
                        let (mut s, mut p) = (0u32, 0u32);
                        for da in -COH_AZ_HALF..=COH_AZ_HALF {
                            if let Some(ai) = rows.neighbour(i, da) {
                                s += u32::from(per_row[ai].0[j]);
                                p += u32::from(per_row[ai].1[j]);
                            }
                        }
                        p > 0 && (s as f64) > COH_MAX_STRADDLE * p as f64
                    })
                    .collect::<Vec<bool>>()
            })
            .collect()
    }

    /// [`straddle_fixture`] with deterministic holes punched through it, so the
    /// present-pair counts vary along every row and between rows rather than
    /// standing at the window width everywhere.
    ///
    /// A hole is what makes the count prefix sum carry anything: with no NaN a
    /// row's `pcn` is the identity and a stale carry could not be told from a
    /// correct one.
    fn holed_straddle_fixture(n: usize, gates: usize) -> (Vec<Vec<f64>>, Vec<f64>) {
        let (mut grid, azimuths) = straddle_fixture(n, gates);
        for (i, row) in grid.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                if (i * 11 + j * 5) % 37 < 4 {
                    *v = f64::NAN;
                }
            }
        }
        (grid, azimuths)
    }

    /// Both windowing stages hold one prefix-sum pair **per job the pool splits
    /// off** rather than one per row, and a row cannot tell.
    ///
    /// This is the property the pooling rests on, and it is not obvious: a
    /// reused pair arrives holding the row before it. It is safe because index
    /// 0 of each is zero at construction and no row ever writes it, and every
    /// other index a row reads it has already written this row — so nothing a
    /// previous row left is reachable. The oracle is the same arithmetic with a
    /// fresh pair per row, compared bit for bit, `f32` narrowing and all.
    ///
    /// 360 rows, so no pool this test can run under gives every row its own job
    /// and the reuse is really exercised; 400 gates, so the range window is
    /// clipped at both ends and reads the prefix sum rather than the whole row.
    #[test]
    fn a_pooled_prefix_sum_and_a_fresh_one_are_the_same_row() {
        let (n, gates) = (360usize, 400usize);
        let (grid, azimuths) = holed_straddle_fixture(n, gates);
        assert!(
            grid.iter().flatten().any(|v| v.is_nan()),
            "the fixture must have holes for the counts to carry",
        );
        let vg = grid.clone();
        let sweep = straddle_sweep(&vg, &azimuths, gates);
        let rows = sweep_rows(&sweep, n);

        assert_eq!(
            bits(&range_texture(&grid, &sweep, rows)),
            bits(&range_texture_fresh_scratch(&grid, &sweep, rows)),
            "the texture a pooled scratch produces is the texture a fresh one does",
        );

        let interval = sweep.gate_interval_km;
        let pooled = incoherent_velocity(&grid, rows, gates, interval, STRADDLE_VNY);
        let fresh = incoherent_velocity_fresh_scratch(&grid, rows, gates, interval, STRADDLE_VNY);
        assert!(
            pooled.iter().any(|m| *m),
            "the fixture must refuse something"
        );
        assert!(
            !pooled.iter().all(|m| *m),
            "and must not refuse everything, or the comparison is one value",
        );
        assert_eq!(pooled, fresh);
    }

    /// The radial a near-zero gate is confirmed against is the one facing it,
    /// and the seed asks for it at 180° rather than at half the rows.
    ///
    /// 241 rows of 1.0° covering az 60°..300°, carrying a 30 m/s wind folded
    /// at a 25 m/s Nyquist. The arc holds the whole isodop — the zeros at az
    /// 90° and 270° are both in it, and 180 rows apart — and one folded lobe,
    /// az 146°..214°, where |30·cos(az)| passes 25. The seeds fire along the
    /// isodop, the flood fills carry them over the arc, and every one of the
    /// 9640 bins comes back at the velocity the wind carries there.
    ///
    /// Half the *arc* is not half the circle. Counted in rows, 241/2 lands
    /// 120° around, where this wind reads 24.8 m/s and confirms nothing about
    /// a zero: paired that way no seed fires anywhere in the sector, no pass
    /// has anything to propagate from, and the lobe stays folded — 2601 bins a
    /// full 2·Vny from the velocity their own gate holds, with the fold walls
    /// at either edge of it censored to ND (160 bins more).
    #[test]
    fn a_sector_pairs_a_zero_across_the_diameter_it_scanned() {
        let n = 241;
        let gates = 40;
        let azimuths: Vec<f64> = (0..n).map(|i| 60.0 + i as f64).collect();
        let truth: Vec<f64> = azimuths
            .iter()
            .map(|a| 30.0 * a.to_radians().cos())
            .collect();
        let fold = |v: f64| (v + 25.0).rem_euclid(50.0) - 25.0;
        let mut grid: Vec<Vec<f64>> = truth.iter().map(|&v| vec![fold(v); gates]).collect();
        // One bin pinned at the fold limit so the Nyquist estimate is exactly
        // the 25 the field was folded at (az 90°, where the wind's radial
        // component is zero — an isolated spike the passes drop).
        grid[30][0] = 25.0;
        let vg = grid.clone();
        dealias(
            &mut grid,
            &sweep_for(&vg, &azimuths, gates),
            0.5,
            None,
            DealiasProfile::NoFalseShear,
        );

        for (i, row) in grid.iter().enumerate() {
            for (j, &v) in row.iter().enumerate() {
                if (i, j) == (30, 0) {
                    assert!(v.is_nan(), "the pinned spike survived as {v}");
                    continue;
                }
                assert!(
                    (v - truth[i]).abs() < 1e-9,
                    "az {} gate {j} read {v}, not the {} its wind carries",
                    azimuths[i],
                    truth[i],
                );
            }
        }
    }

    /// Which radial faces which, on a rotation and on an arc.
    ///
    /// A rotation always has an answer, and it is the one it always gave:
    /// half the rows around, wrapping. An arc narrower than a half circle
    /// never has one — a 36° sector's rows face 36° of sky it never looked at
    /// — and a wider arc has one for the rows near its two ends and not for
    /// the 119 in its middle, whose counterparts lie in the 119° the antenna
    /// skipped. Forward and backward are both tried because facing is
    /// symmetric: on the 241-row arc below, az 60° is answered by az 240°
    /// ahead of it and az 300° by az 120° behind.
    #[test]
    fn a_radial_faces_the_one_the_antenna_pointed_at_or_none() {
        for n in [360usize, 720] {
            let rows = rows_for(&ring_azimuths(n), n);
            assert_eq!(half_turn_rows(rows), (n / 2) as i32);
            for i in 0..n {
                assert_eq!(
                    rows.neighbour(i, half_turn_rows(rows)),
                    Some((i + n / 2) % n)
                );
            }
        }

        // 36° of 0.5° rows: 360 rows of *this* grid would be a half circle,
        // and it has 72.
        let sector: Vec<f64> = (0..72).map(|i| f64::from(i) * 0.5).collect();
        let rows = rows_for(&sector, 72);
        assert_eq!(half_turn_rows(rows), 360);
        for i in 0..72 {
            assert_eq!(rows.neighbour(i, 360), None);
            assert_eq!(rows.neighbour(i, -360), None);
        }

        // 241 rows of 1.0° covering az 60°..300°.
        let arc: Vec<f64> = (0..241).map(|i| 60.0 + f64::from(i)).collect();
        let rows = rows_for(&arc, 241);
        assert_eq!(half_turn_rows(rows), 180);
        assert_eq!(rows.neighbour(0, 180), Some(180));
        assert_eq!(rows.neighbour(240, 180), None);
        assert_eq!(rows.neighbour(240, -180), Some(60));
        let facing = |i: usize| rows.neighbour(i, 180).or_else(|| rows.neighbour(i, -180));
        assert_eq!((0..241).filter(|&i| facing(i).is_none()).count(), 119);
    }

    /// Every azimuth lookup the dealiaser makes on a complete cut is the wrap
    /// it always was, at every row and every offset any of its passes reaches:
    /// ±1 for the four-neighbour seed tests, the flood fills' neighbouring
    /// radial and the fold censor, out to 39 for the azimuthal bridge's walk,
    /// and the half turn plus the isodop's three-row window. Every one of them
    /// goes through this one lookup, so this identity is what says the tuned
    /// constants below still measure what they were measured against.
    #[test]
    fn a_closed_sweeps_dealias_neighbours_are_the_wrap_they_always_were() {
        for n in [360usize, 720] {
            let rows = rows_for(&ring_azimuths(n), n);
            assert!(rows.closed);
            let half = half_turn_rows(rows);
            for i in 0..n {
                for d in (-39..=39).chain(half..=half + 3).chain([-half]) {
                    assert_eq!(
                        rows.neighbour(i, d),
                        Some((i as i32 + d).rem_euclid(n as i32) as usize),
                        "row {i} offset {d}",
                    );
                }
            }
        }
    }

    /// The median filter's job in this pipeline: a single-bin velocity spike
    /// disappears; the surrounding field survives.
    #[test]
    fn median_filter_removes_an_isolated_spike() {
        let n = 40;
        let gates = 40;
        let mut grid: Vec<Vec<f64>> = vec![vec![10.0; gates]; n];
        grid[20][20] = 90.0;
        let azs = ring_azimuths(n);
        let filtered = median_filter(&grid, &grid, None, gates, 0.25, 0.25, rows_for(&azs, n));
        assert_eq!(filtered[20][20], 10.0);
        assert_eq!(filtered[10][10], 10.0);
    }

    /// A window the fold censor has emptied has no median, and the linear one
    /// it used to report stood on neither branch of what was left.
    ///
    /// This is the KCRP 2017-08-26 bin at az 73.26°, 9.52 nm reduced to its
    /// arithmetic: the raw sweep carries velocity everywhere, so
    /// [`MEDIAN_MIN_RAW_OCC`] is satisfied, while the dealiased grid holds 9 of
    /// the window's 25 cells and they read ±30 on both sides of the fold with
    /// two genuine zeroes between. The sorted median is 0.00 at a centre of
    /// +30.00 — the value that produced −4.776, the loudest bin the module had.
    #[test]
    fn a_median_window_the_censor_emptied_reports_nothing() {
        let n = 40;
        let gates = 40;
        // Raw is complete: the sky was sampled, so the raw cliff is not what
        // decides this.
        let raw: Vec<Vec<f64>> = vec![vec![30.0; gates]; n];
        let mut deal: Vec<Vec<f64>> = vec![vec![f64::NAN; gates]; n];
        let survivors = [
            (18, 19, 31.0),
            (18, 20, 30.0),
            (19, 18, -31.0),
            (19, 19, 30.0),
            (20, 20, 30.0),
            (21, 20, 0.0),
            (21, 21, -30.0),
            (22, 18, -30.0),
            (22, 20, 0.0),
        ];
        for (i, j, v) in survivors {
            deal[i][j] = v;
        }
        let azs = ring_azimuths(n);
        // 0.5 km first gate and 0.5 km gates put az_half at its cap of 2, so
        // the window is the 5 × 5 the reading above was taken over.
        let rows = rows_for(&azs, n);
        let filtered = median_filter(&deal, &raw, None, gates, 0.5, 0.5, rows);
        assert_eq!(deal[20][20], 30.0, "the centre carries a dealiased value");
        assert!(
            filtered[20][20].is_nan(),
            "9 of 25 cells is under MEDIAN_MIN_DEALIASED_OCC, so there is no \
             neighbourhood to take a median of; got {}",
            filtered[20][20]
        );
        // A window the censor left alone still reports, at the same occupancy
        // floor the raw cliff has always allowed.
        let full: Vec<Vec<f64>> = vec![vec![30.0; gates]; n];
        assert_eq!(
            median_filter(&full, &raw, None, gates, 0.5, 0.5, rows)[20][20],
            30.0
        );
    }

    /// [`MEDIAN_MIN_RAW_OCC`] counts **echo**, and a below-threshold gate is
    /// not echo however plainly the radar looked at it.
    ///
    /// Non-circular by construction: nothing here states what the window's
    /// occupancy is. The bytes are the input, `nexrad_model`'s decoder is what
    /// turns raw 0 into [`GateReport::BelowThreshold`], and the test asserts
    /// only (a) that every cell of the window is one the radar *measured* —
    /// so a rule keyed on [`GateReport::is_measured`] would pass all fifteen
    /// and refuse nothing — and (b) that the filter refuses anyway. That pair
    /// is the whole content of the constant's census, in miniature, and it is
    /// what fails if the key is ever moved back onto illumination.
    #[test]
    fn the_raw_occupancy_cliff_counts_echo_and_not_illumination() {
        use crate::types::GateReport;
        use nexrad_model::data::{MomentData, Radial, RadialStatus};

        // Velocity's own codec: raw 0 is below threshold, 1 is range folded,
        // and 2 upward is a number.
        const SCALE: f32 = 2.0;
        const OFFSET: f32 = 129.0;
        // 50 km out, 250 m gates and half a degree of spacing put az_half at
        // its floor of 1, so the centre's window is 3 rows × 5 gates = 15.
        const FIRST_GATE_M: u16 = 50_000;
        const GATE_M: u16 = 250;
        // Eight of those fifteen carry echo, seven are below threshold: 8/15 is
        // 0.533, under MEDIAN_MIN_RAW_OCC and over MEDIAN_MIN_DEALIASED_OCC, so
        // the raw cliff is the rule under test and the dealiased one is not.
        let rows_bytes: [[u8; 5]; 3] = [
            [200, 200, 200, 0, 0],
            [200, 200, 200, 0, 0],
            [200, 200, 0, 0, 0],
        ];
        let radials: Vec<Radial> = rows_bytes
            .iter()
            .enumerate()
            .map(|(i, bytes)| {
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * 0.5,
                    0.5,
                    RadialStatus::IntermediateRadialData,
                    1,
                    0.5,
                    None,
                    Some(MomentData::from_fixed_point(
                        bytes.len() as u16,
                        FIRST_GATE_M,
                        GATE_M,
                        8,
                        SCALE,
                        OFFSET,
                        bytes.to_vec(),
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        let grid = crate::velocity::grid(&radials).expect("the radials carry velocity");
        let sweep = grid.sweep(None);
        assert!(
            sweep.status.is_some(),
            "a decoded sweep's view carries the report plane",
        );

        // (a) Every cell the window reaches was measured — nothing here is
        //     unsampled sky, which is the census's finding on all nine of the
        //     reference volumes and the reason the intent-reading is vacuous.
        for row in &grid.status {
            for report in row {
                assert_ne!(
                    *report,
                    GateReport::NotReported,
                    "the radar reported every gate of this window",
                );
                assert!(report.is_measured());
            }
        }
        assert!(
            grid.status[0].contains(&GateReport::BelowThreshold),
            "and seven of them are the decoder's measurement of emptiness",
        );

        // (b) The filter refuses the centre anyway, because five sevenths of
        //     the window is empty sky and a median along the filament that is
        //     left is a median of whatever the filament threads between.
        let rows = sweep_rows(&sweep, radials.len());
        let filtered = median_filter(
            &grid.values,
            &grid.values,
            sweep.status,
            grid.gate_count,
            grid.first_gate_range_km,
            grid.gate_interval_km,
            rows,
        );
        assert!(
            grid.values[1][2].is_finite(),
            "precondition: the centre itself carries echo",
        );
        assert!(
            filtered[1][2].is_nan(),
            "8 of 15 cells carry echo, under MEDIAN_MIN_RAW_OCC; got {}",
            filtered[1][2]
        );
        // And the `None` arm is the same predicate, not a weaker one: a sweep
        // with no plane reads identically cell for cell.
        let planeless = median_filter(
            &grid.values,
            &grid.values,
            None,
            grid.gate_count,
            grid.first_gate_range_km,
            grid.gate_interval_km,
            rows,
        );
        assert!(
            planeless
                .iter()
                .flatten()
                .zip(filtered.iter().flatten())
                .all(|(a, b)| (a.is_nan() && b.is_nan()) || a == b),
            "finiteness selects the same cells GateReport::Value does",
        );
    }

    /// The divisor curve is the reference's own, read off it at 60 ranges on
    /// six sites. Check the knots, a mid-segment interpolation, and both flat
    /// extensions.
    ///
    /// Every figure here moved when the curve did, and each is a restatement
    /// of a new measurement rather than an assertion bent to pass: the old
    /// values were a 4-knot approximation that left 631 of 808 readings
    /// outside the interval the reference's quantisation pins them to, missing
    /// by up to 5.6 lattice steps, against 38 and under one step now.
    #[test]
    fn rot_divisor_matches_the_reference_curve() {
        // Flat below the first knot — unreachable in the pipeline, which skips
        // everything inside MIN_RANGE_NM (13.06 km), one gate under it.
        assert_eq!(rot_divisor_km(10.0), 22.43);
        assert_eq!(rot_divisor_km(13.1), 22.43);
        // The curve RISES to its peak near 22 km before it falls: 25.0 flat
        // below 20 km was the largest single error in the old table, 10% high
        // where the reference reads 22.7.
        assert_eq!(rot_divisor_km(22.0), 23.97);
        assert!((rot_divisor_km(17.5) - 23.285).abs() < 0.001); // 16→19 segment
        assert_eq!(rot_divisor_km(40.0), 20.57);
        assert_eq!(rot_divisor_km(60.0), 12.93);
        assert!((rot_divisor_km(72.5) - 10.16).abs() < 0.005); // 70→75 segment
        assert_eq!(rot_divisor_km(80.0), 8.62);
        // The corner is at 81.5 km, where the fall of 2.6%/km before it gives
        // way to 0.27%/km after. Without this knot the 80→85 chord cut it and
        // read 2.4% high across the gates between.
        assert_eq!(rot_divisor_km(81.5), 8.31);
        assert!((rot_divisor_km(83.25) - 8.27).abs() < 0.005); // 81.5→85 segment
        assert_eq!(rot_divisor_km(85.0), 8.23);
        assert_eq!(rot_divisor_km(250.0), 8.23); // flat beyond the last knot
        // The nm entry point converts and lands on the same curve.
        assert_eq!(rot_divisor(40.0 / KM_PER_NM), 20.57);
    }

    /// On v = k·(azimuthal arc), the recovered slope is k and NROT is k over
    /// the local divisor. Checked away from the grid edges at a known range.
    #[test]
    fn llsd_recovers_a_linear_azimuthal_gradient() {
        let n = 720;
        let gates = 400;
        let azimuths = ring_azimuths(n);
        // 6 (m/s)/km of azimuthal shear everywhere, small enough to not fold.
        let k = 6.0;
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                let theta = (azimuths[i]).to_radians();
                let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
                (0..gates)
                    .map(|j| {
                        let r = 0.25 + j as f64 * 0.25;
                        k * r * dtheta
                    })
                    .collect()
            })
            .collect();
        let s = sweep(&grid, &azimuths, gates);
        let nrot = llsd_nrot(&s, &grid, &grid, None);

        // Gate 200 → 50.25 km: inside the super-res operator's domain. Its
        // ramp gain is Σ t·o over the legacy arc, the same on both sides:
        // Σk·tₖ = 1.0565. The reference reads this gain directly — a
        // 6 m/s-per-degree synthetic ramp at 21.0 nm reads 0.45 there,
        // against this operator's 0.461, the shape it replaced's 0.450 and
        // the pre-pairlab operator's 0.502 (measured, KLOT/KMSX/KLWX; see
        // [`SPLIT_TAPS`]). One hovered ramp is one lattice interval —
        // 0.45 means [0.436, 0.475] — so it admits the first two and not the
        // third; it is the 76 step profiles that separate the first two.
        let range_nm = (0.25 + 200.0 * 0.25) / KM_PER_NM;
        let gain: f64 = SPLIT_TAPS.iter().map(|&(o, t)| o as f64 * t).sum();
        let expected = k * gain / rot_divisor(range_nm);
        let got = nrot[10][200];
        assert!(
            (got - expected).abs() < 0.03,
            "NROT {got} != expected {expected}"
        );
    }

    /// The rows `sweep_rows` reports for `azimuths`, without a grid to hang
    /// them on: only the azimuths and the count decide.
    fn rows_for(azimuths: &[f64], n: usize) -> crate::azimuth::Rows {
        let grid: Vec<Vec<f64>> = Vec::new();
        sweep_rows(&sweep(&grid, azimuths, 0), n)
    }

    /// Every complete cut is read exactly as it always was, in both halves of
    /// the answer. The step stays `360 / n` bit for bit — n radials around a
    /// circle leave n gaps summing to 360°, so that is their exact mean and a
    /// measured median is only a noisier reading of the same number — and the
    /// last row still borders the first, at every offset any stencil here
    /// reaches for. Every constant in this module was calibrated against full
    /// rotations, so this is the invariance that leaves them measuring what
    /// they were measured against.
    #[test]
    fn a_closed_sweep_is_read_exactly_as_it_always_was() {
        for n in [360usize, 720] {
            assert_eq!(rows_for(&ring_azimuths(n), n).step_deg, 360.0 / n as f64);
        }

        // Collection order starts wherever the antenna was.
        let rolled: Vec<f64> = (0..720).map(|i| f64::from((i + 431) % 720) * 0.5).collect();
        assert_eq!(rows_for(&rolled, 720).step_deg, 0.5);

        // Real azimuths jitter by a few hundredths of a step; ±0.02° is that,
        // and the median of 720 such gaps still reads within a thousandth of a
        // degree of the mean — ten times inside what the closed-sweep test
        // leaves, so the sweep is still read as closed.
        let jittered: Vec<f64> = (0..720)
            .map(|i| i as f64 * 0.5 + 0.02 * (i as f64 * 1.7).sin())
            .collect();
        assert_eq!(rows_for(&jittered, 720).step_deg, 0.5);

        // One radial dropped is a hole in a rotation, not a sector: 719 × 0.5°
        // still accounts for 359.5° of the 360.
        let dropped: Vec<f64> = ring_azimuths(720)
            .into_iter()
            .filter(|a| *a != 100.0)
            .collect();
        assert_eq!(rows_for(&dropped, 719).step_deg, 360.0 / 719.0);

        // The seam. `PROFILE_MAX_HALF` is the widest any reader here reaches,
        // and every one of them goes through this lookup, so a rotation that
        // wraps at ±11 wraps everywhere this module indexes a neighbour.
        let rows = rows_for(&jittered, 720);
        let half = PROFILE_MAX_HALF as i32;
        for i in 0..720 {
            for d in -half..=half {
                assert_eq!(
                    rows.neighbour(i, d),
                    Some((i as i32 + d).rem_euclid(720) as usize),
                    "row {i} offset {d}",
                );
            }
        }
    }

    /// The same physical shear presented twice — 0.5° radials all the way
    /// round, and the first 36° of them standing alone — reports the same
    /// rotation. The sector is differentiated over the arc its radials
    /// actually span, not over the 5° a row `360 / 72` would claim, which is
    /// ten times the arc and so a tenth of the shear.
    #[test]
    fn a_sector_reports_the_rotation_its_own_spacing_carries() {
        let gates = 400;
        // 6 (m/s)/km of azimuthal shear everywhere, as a function of azimuth
        // so both presentations see one field.
        let k = 6.0;
        let row = |az_deg: f64| -> Vec<f64> {
            let theta = az_deg.to_radians();
            let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
            (0..gates)
                .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                .collect()
        };
        let full_az = ring_azimuths(720);
        let sector_az = full_az[..72].to_vec();
        let full: Vec<Vec<f64>> = full_az.iter().map(|&a| row(a)).collect();
        let sector: Vec<Vec<f64>> = sector_az.iter().map(|&a| row(a)).collect();

        let full_nrot = llsd_nrot(&sweep(&full, &full_az, gates), &full, &full, None);
        let sector_nrot = llsd_nrot(&sweep(&sector, &sector_az, gates), &sector, &sector, None);

        // Rows 20..52 of the sector read only rows 3..69 of it, so their whole
        // support lies inside the arc and is the full rotation's data bin for
        // bin — the split operator reaches ±4 and demands ±5, and the window
        // keeps three rows of margin past that at each end.
        let mut carried = 0;
        for i in 20..52 {
            for j in 100..300 {
                let (s, f) = (sector_nrot[i][j], full_nrot[i][j]);
                assert!(
                    s == f || (s.is_nan() && f.is_nan()),
                    "row {i} gate {j}: the sector read {s}, the rotation {f}",
                );
                carried += usize::from(s.is_finite());
            }
        }
        assert_eq!(carried, 32 * 200, "the compared window read mostly ND");

        // And the value is the shear that is there: gate 200 is 50.25 km,
        // inside the super-res operator's domain, where the ramp gain is its
        // Σ t·o over the legacy arc.
        let range_nm = (0.25 + 200.0 * 0.25) / KM_PER_NM;
        let gain: f64 = SPLIT_TAPS.iter().map(|&(o, t)| o as f64 * t).sum();
        let expected = k * gain / rot_divisor(range_nm);
        let got = sector_nrot[30][200];
        assert!(
            (got - expected).abs() < 0.03,
            "the sector read NROT {got}, not the {expected} its shear carries \
             (over a 5° row it would read {:.3})",
            expected / 10.0,
        );
    }

    /// A sector has two edges, and neither of them is a place where anything
    /// rotates. The same 6 (m/s)/km of azimuthal shear as the full-rotation
    /// ramp test, laid over 36° of 0.5° radials: inside the arc every row reads
    /// the value that ramp analytically carries, and the five rows at each end
    /// — the ones whose ±5 stencil span reaches past a radial the antenna never
    /// collected — read ND, which is what this module reports at every other
    /// data edge.
    ///
    /// The number the two ends do *not* report is the point. Rows 0 and 71 sit
    /// 324° apart across ground the sweep never looked at, and this field
    /// stands 187 m/s apart across them at 50 km; over the 0.44 km of arc half
    /// a degree spans, that saturates. Nothing anywhere in this sector is
    /// allowed above 1.0, against a field whose own rotation runs from 0.29 at
    /// 25 km to 0.86 at the far gate.
    ///
    /// Five rows an end and not four: the widest stencil reads ±4 but both
    /// demand ±5,
    /// because a bin whose support only just fits sits on a data edge where
    /// half the profile is echo boundary ([`GK_DATA_MARGIN`]). That rule costs
    /// one computable row at each end of a sector, and it is the same rule
    /// spending the same row at every echo edge in every full rotation.
    #[test]
    fn a_sectors_edges_read_no_data_rather_than_its_far_end() {
        let gates = 400;
        let k = 6.0;
        let azimuths: Vec<f64> = (0..72).map(|i| f64::from(i) * 0.5).collect();
        let grid: Vec<Vec<f64>> = azimuths
            .iter()
            .map(|az| {
                let dtheta = az.to_radians();
                (0..gates)
                    .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                    .collect()
            })
            .collect();
        let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid, &grid, None);

        let gain: f64 = SPLIT_TAPS.iter().map(|&(o, t)| o as f64 * t).sum();
        for j in 100..300 {
            let range_km = 0.25 + j as f64 * 0.25;
            let expected = k * gain / rot_divisor(range_km / KM_PER_NM);
            for i in (0..5).chain(67..72) {
                assert!(
                    nrot[i][j].is_nan(),
                    "row {i} gate {j} read {} past the arc's edge",
                    nrot[i][j],
                );
            }
            for (i, row) in nrot.iter().enumerate().take(67).skip(5) {
                assert!(
                    (row[j] - expected).abs() < 1e-9,
                    "row {i} gate {j} read {}, not the {expected} its shear carries",
                    row[j],
                );
            }
        }
        assert!(
            nrot.iter().flatten().all(|v| v.is_nan() || v.abs() < 1.0),
            "a sector of pure shear painted a rotation",
        );
    }

    /// The sector rule and the legacy-resolution operator meet on the same
    /// sweep, and this is that sweep: 72 rows of 1.0°, which is every TDWR cut
    /// there is. Its rows are whole degrees, so
    /// [`rows_are_half_degree_pairs`] is false and [`legacy_stencil_rot`]
    /// reads it; its arc stops, so the rows past either end are not there to
    /// read.
    ///
    /// Both halves are asserted against the *rotation* rather than against a
    /// formula, which is what makes this an integration test rather than two
    /// restatements: rows 5..67 of the sector read, bit for bit, what the same
    /// field's complete 1.0° rotation reads at the same rows, because their
    /// whole support lies inside the arc, which for this operator is ±2 rows
    /// read and ±(4 + [`GK_DATA_MARGIN`]) demanded. The five rows at
    /// each end read ND, on the same margin the split operator
    /// spends there, and nothing in the sector is allowed near a rotation:
    /// rows 0 and 71 stand 71° and 372 m/s apart at 50 km, and stitched
    /// together over the 0.87 km of arc a whole degree spans they would
    /// saturate the clamp.
    #[test]
    fn a_legacy_resolution_sector_reads_its_arc_and_stops() {
        let gates = 400;
        let k = 6.0;
        let row = |az_deg: f64| -> Vec<f64> {
            let theta = az_deg.to_radians();
            let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
            (0..gates)
                .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                .collect()
        };
        let full_az = ring_azimuths(360);
        let sector_az = full_az[..72].to_vec();
        let full: Vec<Vec<f64>> = full_az.iter().map(|&a| row(a)).collect();
        let sector: Vec<Vec<f64>> = sector_az.iter().map(|&a| row(a)).collect();
        assert!(
            !rows_are_half_degree_pairs(&sector_az),
            "a 1.0° sector found a pairing"
        );

        let full_nrot = llsd_nrot(&sweep(&full, &full_az, gates), &full, &full, None);
        let sector_nrot = llsd_nrot(&sweep(&sector, &sector_az, gates), &sector, &sector, None);

        let legacy_gain: f64 = LEGACY_TAPS.iter().map(|&(o, t)| 2.0 * o as f64 * t).sum();
        let mut carried = 0;
        for j in 100..300 {
            let range_km = 0.25 + j as f64 * 0.25;
            let expected = k * legacy_gain / rot_divisor(range_km / KM_PER_NM);
            for i in (0..5).chain(67..72) {
                assert!(
                    sector_nrot[i][j].is_nan(),
                    "row {i} gate {j} read {} past the arc's edge",
                    sector_nrot[i][j],
                );
            }
            for i in 5..67 {
                let (s, f) = (sector_nrot[i][j], full_nrot[i][j]);
                assert!(
                    s == f,
                    "row {i} gate {j}: the sector read {s}, the rotation {f}",
                );
                assert!(
                    (s - expected).abs() < 1e-9,
                    "row {i} gate {j} read {s}, not the {expected} its own taps carry",
                );
                carried += 1;
            }
        }
        assert_eq!(carried, 62 * 200, "the compared window read mostly ND");
        assert!(
            sector_nrot
                .iter()
                .flatten()
                .all(|v| v.is_nan() || v.abs() < 1.0),
            "a 1.0° sector of pure shear painted a rotation",
        );
    }

    /// A sweep far too small for anything here to read runs the whole pipeline
    /// and reports nothing, rather than dividing an arc down to nothing or
    /// indexing off its own end. Three radials cannot fill either stencil's ±5
    /// span from inside a 1° arc, and there is nowhere else for the span to
    /// come from.
    #[test]
    fn a_sweep_too_small_for_a_stencil_reads_nothing() {
        let grid = vec![vec![10.0; 200]; 3];
        let azs = vec![0.0, 0.5, 1.0];
        let out = compute_nrot_grid(&sweep(&grid, &azs, 200));
        assert!(out.iter().flatten().all(|v| v.is_nan()));
    }

    /// Every stencil divisor in this module counts **rows of the grid** and
    /// not degrees of sky, and one shear sampled at 0.5° and at 1.0° is what
    /// shows it.
    ///
    /// The two samplings do not read the same number, and that is not the
    /// divisor: it is that the reference uses a *different operator* on a grid
    /// that is already legacy resolution. [`LEGACY_TAPS`] carries a ramp gain
    /// of 1.0686 against the split operator's 1.0565, so the same 6 (m/s)/km
    /// field reads 1.011 of itself on the coarser sweep — measured, not
    /// chosen: the taps are the ones the reference's own hovered step and
    /// couplet profiles solve to, against the divisor curve their scale is
    /// anchored to.
    ///
    /// They differ by that same ratio at **every** range, which is the other
    /// thing this test pins. A second, wider stencil used to take over past
    /// 80 km — one both grids reached, over a one-row divisor, so out there
    /// they read one number and the handover was invisible here. The reference
    /// has since been hovered across it, 55.6 to 175.9 km at seven sites, and
    /// reads the split operator's own 8-radial step response at every range
    /// ([`SPLIT_TAPS`]). So gate 380 below asserts the same gain ratio gates
    /// 100/200/300 do, and a range-dependent operator could not pass it.
    ///
    /// What the test still has to rule out is the reading a reader meets
    /// first — that `2.0 * arc_per_radial` in [`split_stencil_rot`] means
    /// "1.0° of arc" rather than "two rows". Every gain asserted below is
    /// checked against its own taps rather than against the other sampling,
    /// which pins each operator's divisor in row counts. That alone no longer
    /// separates the two readings for the *split* operator, though, and this
    /// change is what took the separation away: the 1.0° sweep used to supply
    /// it, and it now takes a different operator, while on the 0.5° grid two
    /// rows and one degree are the same number by construction. So a third
    /// sampling at 0.25° carries it — still paired, so still the split
    /// operator, and its rows are quarter degrees, where the two readings
    /// differ by a factor of two.
    #[test]
    fn one_shear_reads_the_gain_its_own_operator_carries() {
        let gates = 400;
        // 6 (m/s)/km of azimuthal shear, written as a function of azimuth so
        // both samplings see one field rather than two.
        let k = 6.0;
        let row = |az_deg: f64| -> Vec<f64> {
            let theta = az_deg.to_radians();
            let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
            (0..gates)
                .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                .collect()
        };
        let fine_az = ring_azimuths(720);
        let coarse_az = ring_azimuths(360);
        let fine: Vec<Vec<f64>> = fine_az.iter().map(|&a| row(a)).collect();
        let coarse: Vec<Vec<f64>> = coarse_az.iter().map(|&a| row(a)).collect();
        let fine_nrot = llsd_nrot(&sweep(&fine, &fine_az, gates), &fine, &fine, None);
        let coarse_nrot = llsd_nrot(&sweep(&coarse, &coarse_az, gates), &coarse, &coarse, None);

        // Each grid reads the gain its own operator carries: the super-res
        // operator's is its Σ t·o over two rows, the legacy grid's is Σ 2·o·t
        // over one.
        let split_gain: f64 = SPLIT_TAPS.iter().map(|&(o, t)| o as f64 * t).sum::<f64>();
        let legacy_gain: f64 = LEGACY_TAPS.iter().map(|&(o, t)| 2.0 * o as f64 * t).sum();
        // 25.25/50.25/75.25 km, and 95.25 km — the last one past where a second
        // stencil used to take both grids over.
        for j in [100usize, 200, 300, 380] {
            let range_km = 0.25 + j as f64 * 0.25;
            let divisor = rot_divisor(range_km / KM_PER_NM);
            for (label, got, gain) in [
                ("0.5°", fine_nrot[180][j], split_gain),
                ("1.0°", coarse_nrot[90][j], legacy_gain),
            ] {
                let expect = k * gain / divisor;
                assert!(
                    (got - expect).abs() < 1e-9,
                    "{label} gate {j}: read {got}, not the {expect} a {k} (m/s)/km \
                     ramp carries through its own taps",
                );
            }
        }

        // The whole of the difference is the ratio of those two gains —
        // 1.0115 — at every range, and none of it is the divisor.
        for j in [100usize, 200, 300, 380] {
            let ratio = coarse_nrot[90][j] / fine_nrot[180][j];
            assert!(
                (ratio - legacy_gain / split_gain).abs() < 1e-9,
                "gate {j}: coarse/fine {ratio}, not the gain ratio {}",
                legacy_gain / split_gain,
            );
        }

        // A third sampling, and the one that keeps the divisor honest. On the
        // 0.5° grid "two rows" and "1.0° of arc" are the same number, and the
        // 1.0° grid no longer reaches this operator at all, so neither of the
        // two above can tell them apart. A 0.25° sweep can: its radials still
        // pair — a quarter degree apart, they share a whole degree — so it
        // runs the same split operator, over half the arc per row. Counting
        // rows it reads the shear that is there; read as a physical degree the
        // divisor would be twice the grid's own two rows and every bin would
        // come back at half.
        assert!(
            rows_are_half_degree_pairs(&ring_azimuths(1440)),
            "a 0.25° sweep stopped pairing, and this probe stopped probing",
        );
        let probe_gates = 250; // to 62.75 km — inside the split band, whole
        let quarter_az = ring_azimuths(1440);
        let quarter: Vec<Vec<f64>> = quarter_az
            .iter()
            .map(|&az_deg| {
                let theta = az_deg.to_radians();
                let dtheta = if theta > PI { theta - 2.0 * PI } else { theta };
                (0..probe_gates)
                    .map(|j| k * (0.25 + j as f64 * 0.25) * dtheta)
                    .collect()
            })
            .collect();
        let quarter_nrot = llsd_nrot(
            &sweep(&quarter, &quarter_az, probe_gates),
            &quarter,
            &quarter,
            None,
        );
        for j in [100usize, 200] {
            let range_km = 0.25 + j as f64 * 0.25;
            let expect = k * split_gain / rot_divisor(range_km / KM_PER_NM);
            let got = quarter_nrot[360][j]; // az 90°, clear of the field's wrap
            assert!(
                (got - expect).abs() < 1e-9,
                "0.25° gate {j}: read {got}, not the {expect} the same taps carry \
                 over this grid's own two rows — the divisor read a degree",
            );
            // And it is the same number the 0.5° sampling reads, which is the
            // spacing identity itself, inside the split band, between the two
            // grids that share the operator.
            assert!(
                (got - fine_nrot[180][j]).abs() < 1e-12,
                "0.25° gate {j} read {got} where 0.5° read {}",
                fine_nrot[180][j],
            );
        }
    }

    /// A 1.0°-spaced sweep has no pairing, and the measurement says so.
    ///
    /// [`rows_are_half_degree_pairs`] asks whether either index alignment puts
    /// radials in the same whole degree. Two radials 1.0° apart never are —
    /// their floors differ by one by construction — so both counts are zero
    /// whatever the sweep's offset or jitter, and each of its radials *is* a
    /// whole-degree bin. Answering "paired" there is what handed the split
    /// operator's then asymmetry to the collection index; answering false is
    /// what sends such a sweep to [`legacy_stencil_rot`] instead.
    #[test]
    fn a_one_degree_sweep_has_no_pair_phase_to_measure() {
        let whole: Vec<f64> = (0..360).map(f64::from).collect();
        let offset: Vec<f64> = (0..360).map(|i| f64::from(i) + 0.37).collect();
        let half_offset: Vec<f64> = (0..360).map(|i| f64::from(i) + 0.5).collect();
        // A real antenna wanders a few hundredths of a degree off the grid.
        let jittered: Vec<f64> = (0..360)
            .map(|i| f64::from(i) + 0.06 * (f64::from(i) * 1.7).sin())
            .collect();
        for azs in [&whole, &offset, &half_offset, &jittered] {
            assert!(
                !rows_are_half_degree_pairs(azs),
                "a 1.0° sweep reported a pairing"
            );
        }

        // The super-res control: there the pairing is real and is found,
        // whichever alignment carries it — a sweep whose collection started
        // half a degree along pairs on the other one, and the answer is the
        // same bit, which is now all there is to be had.
        assert!(rows_are_half_degree_pairs(&ring_azimuths(720)));
        let rolled: Vec<f64> = (0..720).map(|i| f64::from(i) * 0.5 + 0.5).collect();
        assert!(rows_are_half_degree_pairs(&rolled));

        // And a super-res sweep that only covers a sector still finds its
        // pairs: false means "these rows are whole degrees", not "this sweep
        // is ragged", so a 60° sector keeps the validated split operator.
        let sector: Vec<f64> = (0..120).map(|i| 30.0 + f64::from(i) * 0.5).collect();
        assert!(rows_are_half_degree_pairs(&sector));
    }

    /// Where the antenna happened to start a cut is not a property of the
    /// weather, so it must not move the rotation. It does not: no reader in
    /// this module takes a radial's index parity any more, so a sweep rolled
    /// by one radial reads bit for bit what it read before.
    ///
    /// The 1.0° half used to assert only that *something* moved, because the
    /// asymmetry fell to `i % 2` and nothing had been measured to say what it
    /// should be instead: a roll moved 7 of 353 bins past 0.04, flipped 3
    /// between a value and ND, and read −0.198 where the unrolled sweep read
    /// −0.086. The reference has since been hovered on 1.0° cuts and reads the
    /// same at both parities ([`LEGACY_TAPS`]), so such a sweep now takes a
    /// symmetric operator and this half asserts the same invariance as the
    /// other one.
    #[test]
    fn a_sweep_reads_the_same_wherever_collection_began() {
        let gates = 224; // to 56.0 km — the split-tap band, whole
        let j = 199; // 50.0 km, through the vortex core
        // A Rankine vortex at az 90°, 50 km, 3 km core, on a 15 m/s flow: the
        // tangential wind projects onto the beam as the azimuthal couplet the
        // product exists to find.
        let field = |az_deg: f64| -> Vec<f64> {
            (0..gates)
                .map(|jj| {
                    let r = 0.25 + jj as f64 * 0.25;
                    let across = r * (az_deg - 90.0).to_radians();
                    let along = r - 50.0;
                    let rad = (across * across + along * along).sqrt();
                    let vt = if rad < 3.0 {
                        20.0 * rad / 3.0
                    } else {
                        20.0 * 3.0 / rad
                    };
                    let couplet = if rad > 1e-9 { vt * across / rad } else { 0.0 };
                    15.0 * (az_deg - 40.0).to_radians().cos() + couplet
                })
                .collect()
        };
        // One row of NROT at 50 km, indexed by *physical* azimuth, from a
        // sweep whose collection order starts `roll` radials along.
        let read = |n: usize, step: f64, roll: usize| -> Vec<f64> {
            let azs: Vec<f64> = (0..n).map(|i| ((i + roll) % n) as f64 * step).collect();
            let grid: Vec<Vec<f64>> = azs.iter().map(|&a| field(a)).collect();
            let nrot = llsd_nrot(&sweep(&grid, &azs, gates), &grid, &grid, None);
            (0..n).map(|k| nrot[(k + n - roll) % n][j]).collect()
        };

        let fine = (read(720, 0.5, 0), read(720, 0.5, 1));
        let mut compared = 0;
        for (k, (&a, &b)) in fine.0.iter().zip(fine.1.iter()).enumerate() {
            assert!(
                a == b || (a.is_nan() && b.is_nan()),
                "0.5° az {}°: unrolled read {a}, rolled read {b}",
                k as f64 * 0.5,
            );
            compared += usize::from(a.is_finite());
        }
        // 713 until the operator's shape was corrected, then 714; 715 since
        // [`GK_MIN_R2`] came down to the floor the six-site step ladder admits.
        // Each move is one more of the 720 bins on this row carrying a value —
        // this count is a witness that the row is not mostly ND, not a reading
        // of anything. The roll-invariance the test exists for is asserted
        // above, bin for bin, and no change here has touched it.
        assert_eq!(compared, 715, "the compared row read mostly ND");

        let coarse = (read(360, 1.0, 0), read(360, 1.0, 1));
        let mut compared = 0;
        for (k, (&a, &b)) in coarse.0.iter().zip(coarse.1.iter()).enumerate() {
            assert!(
                a == b || (a.is_nan() && b.is_nan()),
                "1.0° az {k}°: unrolled read {a}, rolled read {b}",
            );
            compared += usize::from(a.is_finite());
        }
        assert_eq!(compared, 358, "the compared row read mostly ND");
    }

    /// The reference's own per-radial profiles over a 1.0°-spaced cut, which
    /// are what [`LEGACY_TAPS`] was solved from.
    ///
    /// Procedure: a real volume's velocity moment was overwritten with this
    /// pattern — a ±8 m/s step and a ±10 m/s six-radial couplet in a 30–47 km
    /// band, twice each at opposite radial-index parity — and the reference
    /// read per radial by hovering its cursor along an arc at 0.25° steps and
    /// OCRing the azimuth/range/NROT triplet it reports. Six volumes: KLOT
    /// (VCP 212), KATX (215), KMSX (35), KHNX (31), KLWX (32), and KTLX held
    /// out of the fit; elevations 1.79–3.53°, Nyquist 11.3–24.2 m/s. All six
    /// read alike, and alike at both parities, at ~21.0 nm:
    ///
    /// ```text
    /// step    ND  ∓0.10  ±0.69  ±0.69  ∓0.10  ND
    /// couplet ND  +0.06  −0.45  −0.45  −0.06  +0.89  +0.89  −0.06  −0.45  −0.45  +0.06  ND
    /// ```
    ///
    /// Both ND boundaries are where a ±2-row operator's response is
    /// identically zero, which is what fixes the support; the couplet's
    /// −0.45/0.89 = −0.506 is the −0.5 that support forces with no free
    /// parameter, which is what says the reference does not compress couplets
    /// on this grid, which is what the super-res width ladder went on to say
    /// of that grid too.
    ///
    /// A three-range step ladder — 0.77/0.69/0.65 at 32.2/39.1/45.9 km — fixes
    /// the scale against the divisor curve as shipped. It has since been
    /// re-taken over five deciding sites and a holdout and carried out to
    /// 144.5 km, and it is what [`LEGACY_TAPS`] is solved from rung by rung.
    ///
    /// The tolerance is the 0.04 the reference quantizes its own output in.
    #[test]
    fn a_legacy_resolution_sweep_reads_the_reference_profiles() {
        let gates = 400;
        let n = 360;
        let azimuths = ring_azimuths(n); // whole degrees: no pairing to find
        // −8 below each boundary, +8 above; first +8 radial at 101 (odd index)
        // and at 122 (even), so the two steps sit at opposite parity.
        // Couplets: −10 on three radials then +10 on three, first +10 radial at
        // 140 (even) and at 161 (odd).
        let vel = |i: usize| -> f64 {
            match i {
                91..=100 | 122..=131 => -8.0,
                101..=121 => 8.0,
                137..=139 | 158..=160 => -10.0,
                140..=142 | 161..=163 => 10.0,
                _ => 0.0,
            }
        };
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                (0..gates)
                    .map(|j| {
                        let r = 0.25 + j as f64 * 0.25;
                        // A far uniform-wind band, so that max|v| — this
                        // module's Nyquist estimate — sits above the couplet's
                        // 20 m/s pole-to-pole jump and the fold censor leaves
                        // the couplet alone. Real cuts always carry one.
                        if r >= 60.0 {
                            return 23.0 * f64::from(i as u16).to_radians().cos();
                        }
                        if (30.0..47.0).contains(&r) {
                            vel(i)
                        } else {
                            0.0
                        }
                    })
                    .collect()
            })
            .collect();
        let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid, &grid, None);
        let j = 154; // 38.75 km, mid-band, where the reference was hovered
        let at = |i: usize| nrot[i][j];

        // A class the reference reports under [`SIGNIFICANT`] may read ND
        // here: this module's coherence floor drops profiles that correlate
        // weakly with the stencil, and those bins are below the palette's
        // first colour either way. The same allowance is made of the super-res
        // step tails in `split_stencil_matches_the_measured_step_profile`.
        let agrees = |got: f64, want: f64| {
            (got - want).abs() < 0.04 || (got.is_nan() && want.abs() < SIGNIFICANT)
        };

        // Both steps, both parities, against the hovered ∓0.10 / ±0.69.
        for (first, sign) in [(101usize, 1.0), (122usize, -1.0)] {
            for (offset, want) in [(-2i32, -0.10), (-1, 0.69), (0, 0.69), (1, -0.10)] {
                let i = (first as i32 + offset) as usize;
                let (got, want) = (at(i), sign * want);
                assert!(
                    agrees(got, want),
                    "step at {first}: radial {i} read {got:.4}, reference {want:.2}",
                );
            }
            for i in [first - 3, first + 2] {
                assert!(
                    at(i).is_nan(),
                    "radial {i} painted where the reference is ND"
                );
            }
        }

        // Both couplets, both parities, against the hovered ten-radial profile.
        const COUPLET: [f64; 10] = [
            0.06, -0.45, -0.45, -0.06, 0.89, 0.89, -0.06, -0.45, -0.45, 0.06,
        ];
        for first in [140usize, 161] {
            for (k, &want) in COUPLET.iter().enumerate() {
                let i = first - 5 + k;
                let got = at(i);
                assert!(
                    agrees(got, want),
                    "couplet at {first}: radial {i} read {got:.4}, reference {want:.2}",
                );
            }
            for i in [first - 6, first + 5] {
                assert!(
                    at(i).is_nan(),
                    "radial {i} painted where the reference is ND"
                );
            }
        }

        // The two parities are not merely each within a quantum of the
        // reference: they are the same field. This is the property the whole
        // change exists for.
        for k in 0..10 {
            let (a, b) = (at(135 + k), at(156 + k));
            assert!(
                a == b || (a.is_nan() && b.is_nan()),
                "couplet radial {k}: even parity read {a}, odd parity read {b}",
            );
        }
    }

    /// A patch whose velocity varies but carries no coherent azimuthal trend
    /// is noise, and the reference reports nothing at such bins even where it
    /// has good velocity. The coherence floor is what discards them, so a field
    /// that alternates sign radial-to-radial — zero net gradient, maximum
    /// variance — must produce no value rather than a large one.
    #[test]
    fn incoherent_patches_are_rejected_by_the_fit_quality_floor() {
        let n = 720;
        let gates = 200;
        let azimuths = ring_azimuths(n);
        // ±6 m/s alternating every radial: no linear trend at any scale.
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| vec![if i % 2 == 0 { 6.0 } else { -6.0 }; gates])
            .collect();
        let s = sweep(&grid, &azimuths, gates);
        let nrot = llsd_nrot(&s, &grid, &grid, None);
        let strong = nrot
            .iter()
            .flatten()
            .filter(|v| !v.is_nan() && v.abs() >= 0.25)
            .count();
        assert_eq!(
            strong, 0,
            "{strong} incoherent bins survived as NROT >= 0.25"
        );
    }

    /// Rotation is reported only over velocity that is continuous along the
    /// beam, and "continuous" is measured against the cut's own fold limit.
    ///
    /// The perturbation here is the reference's own discriminating case: it
    /// varies along range and is **common-mode across azimuth**, so an
    /// antisymmetric operator cancels it identically and the rotation computed
    /// from it is the clean couplet's, over the clean couplet's patch. The
    /// reference refuses it anyway (GR2Analyst, KHNX and KLWX, 0 of 9 hover
    /// points painted at 8.0 m/s where 6.0 m/s paints; KMSX, whose limit is
    /// 25.91 rather than 11.66, paints all nine) — so what is being tested is
    /// the velocity's continuity and not the rotation field's.
    ///
    /// The second half is the axis: one grid, two declarations. A cut that
    /// declares twice the Nyquist tolerates twice the discontinuity, which is
    /// what [`GK_MAX_TEXTURE_VNY_FRAC`]'s ladder measured across a 3.1× span of
    /// declared limits.
    #[test]
    fn rotation_is_reported_only_over_velocity_continuous_along_the_beam() {
        let n = 720;
        let gates = 400;
        let azimuths = ring_azimuths(n);
        let j = 155; // 38.75 km, where the ladder was hovered
        const VNY: f64 = 11.66; // KHNX's declared limit
        const AMP: f64 = 5.0;
        const CORE: usize = 100;

        // A width-3 couplet, plus a perturbation that alternates every
        // TEXTURE_STEP_KM along range and does not vary across azimuth: every
        // range pair in the window differs by exactly 2·`bump`, so the texture
        // this fixture carries is 2·`bump` and nothing else.
        let paint = |bump: f64| -> Vec<Vec<f64>> {
            (0..n)
                .map(|i| {
                    let d = i as i64 - CORE as i64;
                    let pole = if (0..3).contains(&d) {
                        AMP
                    } else if (-3..0).contains(&d) {
                        -AMP
                    } else {
                        0.0
                    };
                    (0..gates)
                        .map(|g| pole + if (g / 2) % 2 == 0 { bump } else { -bump })
                        .collect()
                })
                .collect()
        };
        let read = |grid: &[Vec<f64>], nyquist: f64| -> f64 {
            let mut s = sweep(grid, &azimuths, gates);
            s.declared_nyquist_ms = Some(nyquist);
            llsd_nrot(&s, grid, grid, None)[CORE][j]
        };

        let ceiling = GK_MAX_TEXTURE_VNY_FRAC * VNY;
        let (under, over) = (0.4 * ceiling, 0.7 * ceiling); // textures 0.8× and 1.4×
        let flat = paint(0.0);
        let quiet = paint(under);
        let broken = paint(over);

        let clean = read(&flat, VNY);
        assert!(
            clean.abs() >= SIGNIFICANT,
            "precondition: the couplet itself reads {clean:.3}",
        );
        let kept = read(&quiet, VNY);
        assert!(
            kept.abs() >= SIGNIFICANT,
            "a discontinuity at {:.2} of the limit blanked the couplet: read {kept:.3}",
            2.0 * under / VNY,
        );
        let dropped = read(&broken, VNY);
        assert!(
            dropped.is_nan(),
            "a discontinuity at {:.2} of the limit still read {dropped:.3}",
            2.0 * over / VNY,
        );

        // The same grid, twice the declared limit: the ceiling is a multiple of
        // what the cut says it can measure, not a velocity.
        let wider = read(&broken, 2.0 * VNY);
        assert!(
            !wider.is_nan() && wider.abs() >= SIGNIFICANT,
            "a cut declaring {:.2} refused what {:.2} of its own limit allows: read {wider:.3}",
            2.0 * VNY,
            2.0 * over / (2.0 * VNY),
        );
    }

    /// The full pipeline output is clamped to ±5 no matter how violent the
    /// input shear is.
    #[test]
    fn nrot_is_clamped_to_plus_minus_five() {
        let n = 720;
        let gates = 100;
        let azimuths = ring_azimuths(n);
        let grid: Vec<Vec<f64>> = (0..n)
            .map(|i| {
                // ±30 m/s alternating over 4-radial blocks: absurd shear.
                let v = if (i / 4) % 2 == 0 { 30.0 } else { -30.0 };
                vec![v; gates]
            })
            .collect();
        let s = sweep(&grid, &azimuths, gates);
        let nrot = compute_nrot_grid(&s);
        for row in &nrot {
            for v in row {
                assert!(v.is_nan() || v.abs() <= 5.0, "unclamped NROT {v}");
            }
        }
    }
    /// [`SPLIT_TAPS`] reproduces the reference's measured per-radial step
    /// profile, **and reads a step at a whole degree exactly as it reads one
    /// at a half degree**.
    ///
    /// The classes are the operator's tail sums from the outside in — the
    /// radial m out from the edge reads Σ_{k>m} tₖ, which is what a step
    /// response *is* for a zero-sum operator — and the reference's own
    /// readings at 21.0 nm on a ±8 m/s step are printed beside them:
    ///
    /// ```text
    ///   radials flanking the edge   t₁+t₂+t₃+t₄ = 0.667   0.780   GR 0.77
    ///   one further out             t₂+t₃+t₄              0.518   GR 0.49
    ///   two further out             t₃+t₄                 0.116   GR 0.10
    ///   three further out           t₄                   −0.179   GR −0.18
    /// ```
    ///
    /// This arc cannot separate the shape and is not what fixed it. Its peak
    /// is 19 lattice steps, so each cell is an interval of ±0.0198 and the
    /// intersection over the four cells admits both this operator and the
    /// one it replaced — the 0.518 above and the 0.49 the reference reports
    /// are one lattice step apart under a peak the profile leaves free. What
    /// pins the shape is the same profile read out to 175.9 km, where the
    /// peak runs to 78 steps ([`SPLIT_TAPS`]).
    ///
    /// Both boundaries carry the same profile because the reference does:
    /// 36 hovered profiles over six sites, three at whole-degree azimuths and
    /// three at half-degree ones — opposite radial-index parities, since
    /// super-res radial centres sit at x.21/x.71 — every one reading the
    /// shouldered shape above. This test asserted a flat *four*-radial core
    /// at the whole-degree boundary until those readings were taken; that
    /// shape is what the old clean/away asymmetry produced there and the
    /// reference never shows it. See [`SPLIT_TAPS`].
    ///
    /// The sub-threshold tails may read ND (the coherence gate drops them,
    /// and the display palette would not paint them either) but when present
    /// must carry the measured class values.
    ///
    /// **And the same four classes at 38.5, 95.25 and 175.25 km**, which is
    /// the half that pins the handover's absence. A second stencil used to
    /// take over past 80 km and paint six radials a side; the reference paints
    /// four at every range out to 175.9 km, over seven sites, and reads ND at
    /// the fifth ([`SPLIT_TAPS`]). The two long gates below are on either side
    /// of where that stencil used to start, and read exactly what the short one
    /// does once the arc and the divisor are taken out.
    #[test]
    fn split_stencil_matches_the_measured_step_profile() {
        let n = 720;
        let gates = 800; // to 200.25 km, so a gate past 80 km is a gate here
        let azimuths = ring_azimuths(n); // i·0.5°, pairs at whole degrees
        // The step response is the operator's tail sums, outermost first.
        // Taken from the taps rather than written out, because t₁ ≠ t₃ and an
        // earlier spelling of these classes quietly assumed they were equal —
        // which is the shape error the 175.9 km profiles found.
        let tail: Vec<f64> = (0..SPLIT_TAPS.len())
            .map(|m| SPLIT_TAPS[m..].iter().map(|&(_, t)| t).sum())
            .collect();

        // Radial 90 is the first +8 radial of a step at az 45.0, a boundary
        // *between* whole-degree pairs; radial 91 is the first of a step at
        // az 45.5, a boundary *inside* one.
        for (boundary_az, first_plus) in [(45.0, 90usize), (45.5, 91usize)] {
            let grid: Vec<Vec<f64>> = (0..n)
                .map(|i| vec![if azimuths[i] < boundary_az { -8.0 } else { 8.0 }; gates])
                .collect();
            let s = sweep(&grid, &azimuths, gates);
            let nrot = llsd_nrot(&s, &grid, &grid, None);
            // 38.5 km, and 95.25 and 175.25 km — both past the range a second
            // operator used to take the bin over at.
            for j in [153usize, 380, 700] {
                let range_km = 0.25 + j as f64 * 0.25;
                let arc_legacy = range_km * 1.0_f64.to_radians();
                let scale = 16.0 / arc_legacy / rot_divisor_km(range_km);
                let class = [
                    tail[0] * scale,
                    tail[1] * scale,
                    tail[2] * scale,
                    tail[3] * scale,
                ];
                for (radial, expect) in [
                    (first_plus - 1, class[0]),
                    (first_plus, class[0]),
                    (first_plus - 2, class[1]),
                    (first_plus + 1, class[1]),
                    (first_plus - 3, class[2]),
                    (first_plus + 2, class[2]),
                    (first_plus - 4, class[3]),
                    (first_plus + 3, class[3]),
                ] {
                    let got = nrot[radial][j];
                    let core = expect == class[0];
                    assert!(
                        (got - expect).abs() < 0.02 || (!core && got.is_nan()),
                        "az {boundary_az}, gate {j}, radial {radial}: got \
                         {got:.3}, expected {expect:.3}{}",
                        if core { "" } else { " or ND" },
                    );
                }
                // The fifth radial out and beyond: this operator's response is
                // identically zero there, and the reference reads ND. A wider
                // stencil would paint −0.20 of the peak at the first of them.
                for radial in [
                    first_plus - 6,
                    first_plus - 5,
                    first_plus + 4,
                    first_plus + 5,
                ] {
                    let got = nrot[radial][j];
                    assert!(
                        got.is_nan() || got.abs() < 0.02,
                        "az {boundary_az}, gate {j}, radial {radial}: got \
                         {got:.3}, expected ~0"
                    );
                }
            }
        }
    }

    /// A couplet reads the operator its own step response fixes — at every
    /// pole width, at both boundary parities, and with no compression stage
    /// between.
    ///
    /// A couplet is two steps, so a linear operator's response to one is
    /// determined by its step response with nothing left to choose. Sixty
    /// hovered couplets say the reference is that operator and no more: pole
    /// widths 2, 3, 4, 5 and 6 radials, each at a whole-degree and at a
    /// half-degree centre — opposite radial-index parities, since super-res
    /// radial centres sit at x.21/x.71 — patched into the 30–47 km band of a
    /// real volume's 0.5° cut at KHNX (Vny 11.66), KLWX (11.34), KLOT (23.96),
    /// KATX (25.32), KMSX (25.91) and a KTLX holdout (11.49), and hovered
    /// along the 21.0 nm arc at 0.25° steps. Poles are 0.45·Vny, so the three
    /// low-limit sites painted ±5.0 m/s; per radial outward from the couplet's
    /// centre they read
    ///
    /// ```text
    ///        core     +1     +2     +3     +4     +5
    /// w2    +0.30  +0.14  −0.18  −0.26     ND  +0.06
    /// w3    +0.49  +0.14  −0.18  −0.34  −0.14     ND
    /// w4    +0.53  +0.30  −0.10  −0.34  −0.22  −0.14
    /// w5    +0.45  +0.38     ND  −0.26  −0.22  −0.22
    /// w6    +0.45  +0.30  +0.14  −0.14  −0.14  −0.22
    /// ```
    ///
    /// symmetric about the centre, the same at both parities, and the same
    /// profile scaled by amplitude at the three high-limit sites (w3 core
    /// +1.09 on KLOT's ±10.5 poles and +1.13 on KATX's and KMSX's ±11.5,
    /// against +1.07 and +1.17 predicted). Every value above is
    /// [`SPLIT_TAPS`]' own prediction to within 0.04, the quantum the
    /// reference reports in — for all 60 profiles, worst departure 0.04.
    ///
    /// Thirty-six asymmetric couplets say the same: width-3 poles with the
    /// weak one at 0.67 and at 0.33 of the strong, both parities, same six
    /// sites. At ±5.0 m/s strong poles the reference reads, strong side then
    /// weak side outward,
    ///
    /// ```text
    /// 0.67  +0.42  +0.10  −0.18  −0.34  −0.14 | +0.42  +0.14  −0.10  −0.26  −0.10
    /// 0.33  +0.30     ND  −0.18  −0.30  −0.14 | +0.34  +0.14     ND  −0.14     ND
    /// ```
    ///
    /// which is again this operator applied to that pattern, weak flank and
    /// all.
    ///
    /// # What this replaced
    ///
    /// A matched-filter kernel bank — five fitted tap operators (widths 2/3/4
    /// and two asymmetric ones), a per-bin cap, a footprint layer, template
    /// and balance gates — used to cap couplet cores here, and did it by
    /// radial parity. On the ladder above it read a width-3 core at 0.37 and
    /// at 0.18 where the reference reads 0.49 at both, and a width-2 core at
    /// 0.03 and at 0.20 against 0.30. It was fitted against an operator that
    /// has since been corrected: the compression it existed to reproduce was
    /// the old parity-split operator's own shape error, and against the
    /// operator the reference's step response fixes there is nothing left to
    /// compress. On real weather it was crushing the thing NROT exists to
    /// show — the KFTG 2023-06-22 mesocyclone's core read +0.34 and +0.30
    /// under it at az 90.8°/7.5 nm and 91.3°/8.0 nm, where the reference reads
    /// +1.64 and +1.56 and this operator alone reads +1.52 and +1.46.
    #[test]
    fn a_couplet_reads_the_operator_its_own_step_response_fixes() {
        let n = 720;
        let gates = 400;
        let azimuths = ring_azimuths(n);
        let j = 155; // 38.75 km, mid-band, where the reference was hovered
        const AMP: f64 = 5.0;

        // Per radial outward from the centre, as hovered. `None` is a class
        // the reference reports under [`SIGNIFICANT`], where this module's
        // coherence floor may drop the bin instead — invisible either way.
        type Profile = [Option<f64>; 6];
        /// An asymmetric couplet's two sides are two profiles: the weak pole
        /// is a shorter step, and the reference reads each side accordingly.
        type Sides = (f64, [Option<f64>; 5], [Option<f64>; 5]);
        const SYMMETRIC: [(usize, Profile); 5] = [
            (
                2,
                [
                    Some(0.30),
                    Some(0.14),
                    Some(-0.18),
                    Some(-0.26),
                    None,
                    Some(0.06),
                ],
            ),
            (
                3,
                [
                    Some(0.49),
                    Some(0.14),
                    Some(-0.18),
                    Some(-0.34),
                    Some(-0.14),
                    None,
                ],
            ),
            (
                4,
                [
                    Some(0.53),
                    Some(0.30),
                    Some(-0.10),
                    Some(-0.34),
                    Some(-0.22),
                    Some(-0.14),
                ],
            ),
            (
                5,
                [
                    Some(0.45),
                    Some(0.38),
                    None,
                    Some(-0.26),
                    Some(-0.22),
                    Some(-0.22),
                ],
            ),
            (
                6,
                [
                    Some(0.45),
                    Some(0.30),
                    Some(0.14),
                    Some(-0.14),
                    Some(-0.14),
                    Some(-0.22),
                ],
            ),
        ];
        // (weak-pole ratio, strong side outward, weak side outward)
        const ASYMMETRIC: [Sides; 2] = [
            (
                2.0 / 3.0,
                [
                    Some(0.42),
                    Some(0.10),
                    Some(-0.18),
                    Some(-0.34),
                    Some(-0.14),
                ],
                [
                    Some(0.42),
                    Some(0.14),
                    Some(-0.10),
                    Some(-0.26),
                    Some(-0.10),
                ],
            ),
            (
                1.0 / 3.0,
                [Some(0.30), None, Some(-0.18), Some(-0.30), Some(-0.14)],
                [Some(0.34), Some(0.14), None, Some(-0.14), None],
            ),
        ];

        // A couplet of `w` radials a pole, its first positive radial at
        // `first_plus`; everything else is the zero background the reference
        // reads ND over.
        let paint = |w: usize, first_plus: usize, ratio: f64| -> Vec<Vec<f64>> {
            (0..n)
                .map(|i| {
                    let d = i as i64 - first_plus as i64;
                    let v = if (0..w as i64).contains(&d) {
                        AMP
                    } else if (-(w as i64)..0).contains(&d) {
                        -ratio * AMP
                    } else {
                        0.0
                    };
                    vec![v; gates]
                })
                .collect()
        };
        let agrees = |got: f64, want: Option<f64>| match want {
            Some(w) => (got - w).abs() < 0.04 || (got.is_nan() && w.abs() < SIGNIFICANT),
            None => got.is_nan() || got.abs() < SIGNIFICANT,
        };

        for (w, profile) in SYMMETRIC {
            // Even and odd `first_plus`: the couplet's centre falls between a
            // whole-degree pair and inside one.
            let mut both = Vec::new();
            for first_plus in [100usize, 141] {
                let grid = paint(w, first_plus, 1.0);
                let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid, &grid, None);
                let mut read = Vec::new();
                for (m, want) in profile.iter().enumerate() {
                    for radial in [first_plus + m, first_plus - 1 - m] {
                        let got = nrot[radial][j];
                        assert!(
                            agrees(got, *want),
                            "width {w} at {first_plus}: radial {radial} read \
                             {got:.3}, the reference {want:?}",
                        );
                        read.push(got);
                    }
                }
                both.push(read);
            }
            for (k, (a, b)) in both[0].iter().zip(&both[1]).enumerate() {
                assert!(
                    a == b || (a.is_nan() && b.is_nan()),
                    "width {w} radial {k}: one parity read {a}, the other {b}",
                );
            }
        }

        for (ratio, strong, weak) in ASYMMETRIC {
            for first_plus in [100usize, 141] {
                let grid = paint(3, first_plus, ratio);
                let nrot = llsd_nrot(&sweep(&grid, &azimuths, gates), &grid, &grid, None);
                for (m, want) in strong.iter().enumerate() {
                    let got = nrot[first_plus + m][j];
                    assert!(
                        agrees(got, *want),
                        "ratio {ratio:.2} at {first_plus}: strong side +{m} read \
                         {got:.3}, the reference {want:?}",
                    );
                }
                for (m, want) in weak.iter().enumerate() {
                    let got = nrot[first_plus - 1 - m][j];
                    assert!(
                        agrees(got, *want),
                        "ratio {ratio:.2} at {first_plus}: weak side {m} out read \
                         {got:.3}, the reference {want:?}",
                    );
                }
            }
        }
    }
}
