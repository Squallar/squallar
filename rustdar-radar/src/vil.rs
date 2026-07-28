//! Digital VIL (the RPG's product 134, "HRVIL", AWIPS `DVL`) computed locally
//! from the Level II reflectivity volume.
//!
//! # What is implemented, and from which documents
//!
//! **Flow** — ORPG man pages `hrvil(1)` and `hrvil(4)` (task `cpc014/tsk010`,
//! High Resolution VIL), from the WSR-88D CODE distribution: per 1° × 1 km
//! polar gate, a *partial VIL* is computed for each elevation and summed as
//! the volume completes; the total is the product. The HRVIL task's own
//! source is not in any public CODE distribution (the same closed `cpc014`
//! family as HREET), so the arithmetic below is the legacy VIL/Echo Tops
//! task's (`cpc013/tsk001`), which the man page describes HRVIL as applying
//! per radial gate instead of per 4 km × 4 km box.
//!
//! **Liquid water** — Greene & Clark (1972), computed in floating point and
//! capped at 56 dBZ:
//!
//! ```text
//! LW = 3.44e-3 · Z^(4/7)   g/m³,   Z = 10^(dBZ/10),   dBZ ≤ 56
//! ```
//!
//! The legacy task reads the `A313B1` look-up table (`a313.inc`) instead —
//! verified here entry for entry to be this formula **floored at hundredths
//! of g/m³** and saturated at 5.40 from data level 178 (56.0 dBZ) up — but
//! the twin survey arbitrates decisively for the unfloored form: product
//! 134's linear region steps by 0.011 kg/m², the table's floor biases every
//! weak-echo column a level or more low, and switching to floating point
//! moved within-±1 by 30–40 points at half the surveyed sites. The floored
//! table stays as the harness's `TableFloor` A/B variant.
//!
//! **Threshold** — none in the primary. The legacy task gates every sample
//! on `min_refl` 18.3 dBZ (`vil_echo_tops.alg`; `IREFMIN = NINT(2·18.3 +
//! 66) = 103` in `a313a1.ftn`, i.e. ≥ 18.5 dBZ on half-dB data), but live
//! product-134 twins refuse that reading twice over: they carry tens of
//! thousands of bins at levels 2–19 (0 – 0.2 kg/m², much of it below
//! anything an 18.3 dBZ column could produce), and they define **every**
//! bin their input carries valid reflectivity for — level 2 is a defined
//! 0.0 kg/m², not background. The primary therefore integrates every valid
//! gate and defines every data-carrying column (0.0 where nothing
//! contributes); the legacy gate survives as the `18.3`/`echo-only` A/B
//! variants.
//!
//! **Layer depths** — `A313T1__COMPUTE_DEPTH` (`a313t1.ftn`), reproduced
//! case for case with `RH = RS·cos φ` and `RS` the bin's **outer edge** in
//! km (the routine evaluates bin `J` at `RS = J` while the height table
//! `a313e1.ftn` uses centres — both kept):
//!
//! * **lowest tilt**: ground up to the angular midpoint of tilts 1–2,
//!   `RH·tan(φ_avg) + RH²/(2·(4/3)·6371·cos²φ_avg)` — the one place the
//!   depth table has a curvature term, and it is the **4/3 earth model**,
//!   *not* the 1.21·Re the same task uses for echo-top heights; each
//!   constant is copied from its own routine;
//! * **middle tilts**: flat-earth midpoint boundaries,
//!   `½·RH·(tan φ_above − tan φ_below)`;
//! * **highest tilt**: `½·RH·(tan(φ_top + BW/2) − tan φ_below)` with the
//!   routine's hardcoded `BW = 0.017` **radians** — the beam's upper flank
//!   caps the column, no extrapolation past the volume ceiling.
//!
//! A volume with a single reflectivity tilt follows the routine's actual
//! control flow: the top-tilt case overwrites the lowest-tilt case with an
//! unfound angle below (`tan 0`), giving `½·RH·tan(φ + BW/2)`.
//!
//! **Cut participation** — every reflectivity cut of the volume's first
//! pass: `viletalg.ftn` sets `ALLOW_SUPPL_SCANS = 0`, so the RPG's VIL never
//! sees SAILS/MRLE revisits and the cube is deduplicated
//! [`DedupPolicy::FirstOfVolume`], with layer depths built from the
//! SAILS-free elevation ladder — the same ladder the routine builds from the
//! "local" VCP definition.
//!
//! **Units** — kg/m² (g/m³ · km), summed in f64 per column. The legacy
//! task's per-elevation `NINT` to whole kg/m² (`a313d1.ftn`) is a
//! 16-level-product encoding artifact and is *not* applied: HRVIL encodes
//! once, at the end, into product 134's hybrid linear/log levels
//! ([`crate::l3_values::build_vil_lut`] decodes them, and the harness below
//! compares in them). The site-adaptable `max_vil` display cap (80 kg/m²
//! fleet default, up to 200 at KICT) is likewise an encoding-side clamp —
//! the twin's own LUT top enforces it in comparison, and the derived field
//! keeps the physical value.
//!
//! # Documented gaps against the RPG
//!
//! * **Input** is raw Level II reflectivity, not the DQA-edited buffer
//!   (`dqa(1)`, product 297) HRVIL consumes — the same closed preprocessing
//!   WP1 identified as EET's residual. Because DQA *removes* clutter, AP
//!   and other artifacts, the raw derivation defines more bins than the
//!   twin, and carries returns in them the RPG deleted; the harness's
//!   presence-disagreement gate is what measures that.
//! * **Cell statistic**: the RPG feeds HRVIL recombined 1° × 1 km
//!   reflectivity, documented as a linear-Z average of the super-resolution
//!   gates, so [`CellStat::LinearZMean`] is the primary; the EET campaign's
//!   twin-arbitrated `Max` is kept as an A/B variant in the live harness
//!   (the 2026-07-28 survey scores the two within half a point of each
//!   other pooled, mixed per site).
//! * **Elevation angles**: the RPG builds its depth table from the VCP's
//!   nominal angles in tenths of degrees; here each sweep's measured median
//!   elevation is used (the antenna's real ladder, within a few hundredths
//!   of a degree of nominal).
//!
//! # Validation status — read before trusting the twin harness to pass
//!
//! Three surveys over live volumes on 2026-07-28 arbitrated the
//! conventions, and the final one (21 sites asserted, 815,797 bins pooled)
//! does **not** meet the campaign bar (99% within one data level and ≤ 2%
//! presence disagreement, per site): within-±1 runs 43.2–98.75% (median
//! ~93; best KMPX 98.75, KFSD 98.36, KOAX 98.08) and presence disagreement
//! 8.3–24.1% at *every* site. Zero sites pass. What the surveys
//! established:
//!
//! * **Presence** — the twin defines every data-carrying bin (level 2 is a
//!   defined 0.0; 1,400–17,300 such bins per volume), and the *derived*
//!   side defines 8–24% more bins than the twin: the DQA deletes clutter,
//!   AP and artifact bins that raw Level II carries. The excess is the
//!   DQA's editing, unreachable from raw data — `dqa(1)`'s task is in the
//!   same closed `cpc014` family WP1 hit for EET.
//! * **Level agreement** tracks storm-core (log-region) mass: quiet sites
//!   sit at 97–99% within-±1, while sites whose twins carry 10–20k
//!   log-region bins (KMRX 45.1%, KMLB 43.2%, KSGF 55.7%) fall off a
//!   cliff — the fine hybrid levels (0.011 kg/m² linear steps, ~2.6%
//!   relative in the log region) amplify whatever reflectivity editing and
//!   smoothing HRVIL inherits from the DQA.
//! * The bounded A/B matrix (cell statistic × LW mapping × participation
//!   gate × depth datum, eight rows) arbitrated three conventions, each on
//!   the **whole 21-site roster** — plains, southeast/coastal,
//!   mountain-west and Appalachian sites alike, never a single site — and
//!   each was then re-confirmed on a second run in which 16 of 20 shared
//!   sites had moved on to *fresh volumes* that played no part in the
//!   choice. Unfloored LW: wins 20/21 in the decision run (margins +2 to
//!   +46 points of within-±1, every region; sole exception KSHV, −0.75)
//!   and 20/21 on the confirmation run. No participation gate: 21/21 and
//!   21/21. Data-columns presence is arbitrated by the product itself —
//!   all 21 twins carry defined-zero (level 2) mass, 1,405–17,281 bins
//!   each. The rest is noise on both runs: `Max` against `LinearZMean`
//!   splits 15/21 *for* `Max` but with median margin under one point and
//!   mixed sign (its only big wins are the two deepest-convection sites,
//!   which sit at 43–52% either way), so the documented recombination
//!   average is retained; edge/centre moves under two points everywhere.
//!   No documented convention closes the residual, and per the campaign's
//!   early-stop rule nothing undocumented was chased.
//!
//! Product 134 therefore **stays a Level III fetch**; this module ships as
//! the local input to [`compute_vil_density`] (VIL density has no Level III
//! twin anywhere) with that provenance documented.

use crate::types::RadarProduct;
use crate::volumetric::{
    CellStat, DedupPolicy, RANGE_BINS, VolumeCube, VolumetricGrid, sweep_elevation_deg,
};
use nexrad_model::data::Scan;

/// The legacy VIL task's reflectivity gate, dBZ: the `alg.vil_echo_tops
/// min_refl` fleet default, applied by `a313f1.ftn` as `IREFMIN`. The
/// primary derivation does **not** apply it — live product-134 twins carry
/// mass at levels the gate would forbid (see the module doc) — but the
/// legacy reading stays in the harness's A/B matrix under this constant.
pub const VIL_MIN_REFL_DBZ: f32 = 18.3;

/// Greene & Clark's coefficient: `LW = 3.44e-3 · Z^(4/7)` g/m³.
const GREENE_CLARK_COEFF: f64 = 3.44e-3;

/// The `A313B1` table's saturation value, hundredths of g/m³: every data
/// level from 178 (56.0 dBZ) up maps to 540 — the product's 56 dBZ hail cap.
const LW_CAP_HUNDREDTHS: f64 = 540.0;

/// `A313T1__COMPUTE_DEPTH`'s hardcoded beamwidth, **radians** (`BW = .017`):
/// the top layer extends half of this above the highest tilt's centre.
const A313T1_BEAMWIDTH_RAD: f64 = 0.017;

/// Earth radius (km) and the 4/3 refraction factor of the depth table's one
/// curvature term (`RE = 6371.0`, `FOURP/THREEP` in `a313t1.ftn`). The echo
/// top height table uses 1.21·Re instead; each is faithful to its source.
const RE_KM: f64 = 6371.0;
const FOUR_THIRDS: f64 = 4.0 / 3.0;

/// How a gate's dBZ becomes liquid water — an A/B knob of the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LwMapping {
    /// The `A313B1` table's exact semantics: floored at hundredths of g/m³
    /// (zero below 8.5 dBZ), saturated at 5.40. The legacy 16-level
    /// product's quantization, kept as an A/B variant — only the harness
    /// and the offline tests construct it, so the lib build sees it dead.
    #[cfg_attr(not(test), allow(dead_code))]
    TableFloor,
    /// Greene–Clark in floating point, capped at 56 dBZ's own 5.452 g/m³.
    /// The primary: against live product-134 twins the floored table reads
    /// a systematic level low across the LUT's 0.011 kg/m² linear region —
    /// 30–40 points of within-±1 at some sites — so HRVIL's C code
    /// (`hrvil_compute_vil`, not public) evidently computes unfloored.
    Analytic,
}

/// The conventions [`compute_vil`] pins; the harness varies them.
#[derive(Debug, Clone, Copy)]
struct VilOptions {
    /// How super-resolution gates collapse into a 1° × 1 km cell.
    stat: CellStat,
    lw: LwMapping,
    /// `false`: depth evaluated at the bin's outer edge (`RS = J`, the
    /// `a313t1.ftn` literal). `true`: at the bin centre, the `a313e1.ftn`
    /// height-table reading of the same bin.
    depth_at_centre: bool,
    /// A participation gate on each dBZ sample: `Some(18.3)` is the legacy
    /// task's `IREFMIN` (`a313f1.ftn`); `None` lets every valid gate through
    /// to the LWC table, whose own floor zeroes everything below 8.5 dBZ —
    /// what a live DVL twin's value distribution shows HRVIL doing.
    min_refl: Option<f32>,
    /// `true`: a column with no participating gate is undefined — the
    /// legacy 4×4 km product's background. `false`: any column carrying
    /// valid reflectivity is defined, at 0.0 if nothing contributes — the
    /// convention product 134 encodes (its level 2 is a defined 0.0 kg/m²,
    /// and live twins carry tens of thousands of bins at it).
    echo_only: bool,
}

impl VilOptions {
    /// The primary: linear-Z recombination, floating-point Greene–Clark,
    /// depths at the outer bin edge, no participation gate, every
    /// data-carrying column defined — see the module doc's validation
    /// section for how each choice was arbitrated against live twins.
    const fn primary() -> Self {
        Self {
            stat: CellStat::LinearZMean,
            lw: LwMapping::Analytic,
            depth_at_centre: false,
            min_refl: None,
            echo_only: false,
        }
    }

    /// The legacy `cpc013` reading: the floored LWC table, `IREFMIN`-gated,
    /// background undefined. Constructed only by the harness's A/B matrix
    /// and the offline tests.
    #[cfg_attr(not(test), allow(dead_code))]
    const fn legacy_threshold() -> Self {
        Self {
            lw: LwMapping::TableFloor,
            min_refl: Some(VIL_MIN_REFL_DBZ),
            echo_only: true,
            ..Self::primary()
        }
    }
}

/// Liquid water for one gate, g/m³. The caller applies the reflectivity
/// threshold; this is only the mapping.
fn liquid_water_g_m3(dbz: f32, mapping: LwMapping) -> f64 {
    let lw = GREENE_CLARK_COEFF * 10f64.powf(f64::from(dbz) * 4.0 / 70.0);
    match mapping {
        LwMapping::TableFloor => (lw * 100.0).floor().min(LW_CAP_HUNDREDTHS) / 100.0,
        LwMapping::Analytic => lw.min(GREENE_CLARK_COEFF * 10f64.powf(56.0 * 4.0 / 70.0)),
    }
}

/// The `A313T1__COMPUTE_DEPTH` table: per tilt of the ascending elevation
/// ladder, the layer depth in km at every range cell. Empty for an empty
/// ladder.
fn layer_depths_km(elevs_deg: &[f64], depth_at_centre: bool) -> Vec<Vec<f64>> {
    let n = elevs_deg.len();
    let phi: Vec<f64> = elevs_deg.iter().map(|e| e.to_radians()).collect();
    (0..n)
        .map(|i| {
            (0..RANGE_BINS)
                .map(|r| {
                    let rs = r as f64 + if depth_at_centre { 0.5 } else { 1.0 };
                    if n == 1 {
                        // The routine's real control flow for one tilt: the
                        // top case overwrites the lowest case, with the
                        // unfound angle below reading tan 0.
                        let rh = rs * phi[0].cos();
                        return 0.5 * rh * (phi[0] + A313T1_BEAMWIDTH_RAD / 2.0).tan();
                    }
                    if i == 0 {
                        // Ground up to the midpoint of the two lowest tilts,
                        // through the 4/3 earth: the table's one curvature
                        // term.
                        let phi_avg = (phi[0] + phi[1]) / 2.0;
                        let rh = rs * phi[0].cos();
                        rh * phi_avg.tan()
                            + rh * rh / (2.0 * FOUR_THIRDS * RE_KM * phi_avg.cos().powi(2))
                    } else if i + 1 == n {
                        // The beam's upper flank caps the column.
                        let rh = rs * phi[i].cos();
                        0.5 * rh * ((phi[i] + A313T1_BEAMWIDTH_RAD / 2.0).tan() - phi[i - 1].tan())
                    } else {
                        // Flat-earth midpoint boundaries.
                        let rh = rs * phi[i].cos();
                        0.5 * rh * (phi[i + 1].tan() - phi[i - 1].tan())
                    }
                })
                .collect()
        })
        .collect()
}

/// Compute Digital VIL from a Level II volume, per the rules in the module
/// doc: per 1° × 1 km cell, `Σ LW(dBZ) · Δh` over the tilts whose gate meets
/// the threshold, in kg/m². `NaN` where no tilt in the column does.
pub fn compute_vil(scan: &Scan) -> VolumetricGrid {
    compute_vil_impl(scan, VilOptions::primary())
}

fn compute_vil_impl(scan: &Scan, opts: VilOptions) -> VolumetricGrid {
    let cube = VolumeCube::build_with_stats(
        scan,
        &[(RadarProduct::Reflectivity, opts.stat)],
        DedupPolicy::FirstOfVolume,
    );

    // The tilts carrying reflectivity, ascending, each at its *actual*
    // elevation — the sweep's median radial angle, as in `crate::eet` (the
    // cube key is rounded to 0.1°).
    let tilts: Vec<(f64, &Vec<Vec<f32>>)> = cube
        .tilts
        .iter()
        .enumerate()
        .filter_map(|(ti, tilt)| {
            let grid = cube.grid(ti, RadarProduct::Reflectivity)?;
            let elev = scan
                .sweeps()
                .get(grid.sweep_index)
                .and_then(|s| sweep_elevation_deg(s.radials()))
                .unwrap_or(tilt.elevation_deg);
            Some((elev, &grid.values))
        })
        .collect();
    let elevs: Vec<f64> = tilts.iter().map(|&(e, _)| e).collect();
    let depths = layer_depths_km(&elevs, opts.depth_at_centre);

    let mut values = vec![vec![f32::NAN; RANGE_BINS]; 360];
    for (az, row) in values.iter_mut().enumerate() {
        for (r, cell) in row.iter_mut().enumerate() {
            let mut sum = 0.0f64;
            let mut any_valid = false;
            let mut any_participating = false;
            for (ti, &(_, dbz)) in tilts.iter().enumerate() {
                let z = dbz[az][r];
                if z.is_nan() {
                    continue;
                }
                any_valid = true;
                if opts.min_refl.is_some_and(|t| z < t) {
                    continue;
                }
                sum += liquid_water_g_m3(z, opts.lw) * depths[ti][r];
                any_participating = true;
            }
            let defined = if opts.echo_only {
                any_participating
            } else {
                any_valid
            };
            if defined {
                *cell = sum as f32;
            }
        }
    }
    VolumetricGrid {
        values,
        range_bins: RANGE_BINS,
    }
}

/// One kilofoot in metres, exactly: 1000 ft · 0.3048 m/ft.
const KFT_TO_M: f32 = 304.8;

/// VIL density for one cell, g/m³, per Amburn & Wolf (1997, *Weather and
/// Forecasting* 12, 473–478): `VILD = 1000 · VIL / ET` with VIL in kg/m²
/// and the echo top in **metres** — the paper divides the WSR-88D VIL
/// product by the WSR-88D echo-top product's height as published, so the
/// height here is the ET product's own kft-above-MSL convention, converted
/// to metres. `NaN` when either input is undefined (or a non-positive top,
/// which MSL heights at real sites never produce).
pub fn vil_density_g_m3(vil_kg_m2: f32, echo_top_kft_msl: f32) -> f32 {
    if !vil_kg_m2.is_finite() || !echo_top_kft_msl.is_finite() || echo_top_kft_msl <= 0.0 {
        return f32::NAN;
    }
    1000.0 * vil_kg_m2 / (echo_top_kft_msl * KFT_TO_M)
}

/// VIL density over the volume: [`compute_vil`]'s field divided by
/// [`crate::eet::compute_eet`]'s, cell for cell, per [`vil_density_g_m3`].
///
/// Both inputs are the local Level II derivations — there is no Level III
/// VIL-density product anywhere to fetch or to validate against, so this
/// product is **validated by construction** from its two inputs: VIL's twin
/// survey verdict (misses the campaign bar, DQA residual — see the module
/// doc) and EET's (the same, recorded in [`crate::eet`]) are the product's
/// provenance. Amburn & Wolf's thresholds still apply to it operationally:
/// below 3.5 g/m³ severe hail is rare, at 4.0 and above nearly universal.
///
/// `radar_height_ft` is the site height above MSL in feet
/// ([`crate::eet::radar_height_ft_near`] for a render); it enters only
/// through the echo top's MSL datum, exactly as the paper's use of the ET
/// product implies. At high-elevation sites the MSL top overstates the
/// column's physical depth by the site elevation and VILD reads
/// correspondingly low — the paper's own convention, kept rather than
/// corrected, so the numbers stay comparable to the operational literature.
///
/// A cell is defined only where **both** inputs are: a weak-echo column
/// carries VIL 0.0 but no 18.3 dBZ echo top, so it is `NaN` here, not 0.
pub fn compute_vil_density(scan: &Scan, radar_height_ft: f64) -> VolumetricGrid {
    let vil = compute_vil(scan);
    let eet = crate::eet::compute_eet(scan, radar_height_ft);
    let values = vil
        .values
        .iter()
        .zip(&eet.values)
        .map(|(vil_row, et_row)| {
            vil_row
                .iter()
                .zip(et_row)
                .map(|(&v, &et)| vil_density_g_m3(v, et))
                .collect()
        })
        .collect();
    VolumetricGrid {
        values,
        range_bins: RANGE_BINS,
    }
}

/// The parts of [`live_validation`] that decide **what counts as passing**.
///
/// Outside the ignored module for the reason `srm::validation_policy` and
/// `eet::validation_policy` are: the live harness never runs under
/// `cargo test --workspace`, so anything defined inside it could be quietly
/// weakened without a default-suite test noticing. Out here `policy_tests`
/// reaches all of it offline, and does.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod validation_policy {
    use crate::twin::compare::ValueCodec;
    use nexrad_level3::model::RadialPacket;

    /// The acceptance bar: percent of compared bins within one data level of
    /// the twin, in the twin's own hybrid LUT levels. Lowering this is how a
    /// derivation that got worse ships anyway; it is pinned by
    /// `the_acceptance_bar_is_what_the_campaign_set`.
    pub const ACCEPTANCE_BAR_WITHIN_ONE_PCT: f64 = 99.0;

    /// Ceiling on cells defined on exactly one side, as a share of the union
    /// of defined cells — the DQA gap's budget.
    pub const PRESENCE_DISAGREEMENT_MAX_PCT: f64 = 2.0;

    /// A run concludes nothing until this many sites were actually asserted…
    pub const MIN_SITES: usize = 4;

    /// …and this many bins were compared, pooled across the asserted sites.
    pub const MIN_DEFINED_BINS: usize = 10_000;

    /// Volumes whose twin defines fewer bins than this are skipped, not
    /// scored: a clear-air volume's handful of bins measures nothing and a
    /// single speck disagreeing would swing whole percentage points. A skip
    /// is printed, never silent.
    pub const MIN_TWIN_DEFINED_BINS: usize = 500;

    pub fn meets_acceptance_bar(within_one_pct: f64) -> bool {
        within_one_pct >= ACCEPTANCE_BAR_WITHIN_ONE_PCT
    }

    pub fn presence_is_acceptable(presence_disagreement_pct: f64) -> bool {
        presence_disagreement_pct <= PRESENCE_DISAGREEMENT_MAX_PCT
    }

    pub fn volume_is_scoreable(twin_defined_bins: usize) -> bool {
        twin_defined_bins >= MIN_TWIN_DEFINED_BINS
    }

    pub fn sample_is_conclusive(sites_asserted: usize, pooled_compared_bins: usize) -> bool {
        sites_asserted >= MIN_SITES && pooled_compared_bins >= MIN_DEFINED_BINS
    }

    /// How much of a quarantined site stops being asserted on. VIL is a
    /// volume product with no per-tilt figure, so the only scope is the
    /// whole site.
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    pub enum Scope {
        Whole,
    }

    pub struct Quarantine {
        pub site: &'static str,
        pub scope: Scope,
        pub why: &'static str,
    }

    /// Sites measured to miss the bar, with what has been ruled out.
    ///
    /// Empty until the survey earns an entry: quarantining requires recorded
    /// evidence from **at least two volumes across at least two runs** — one
    /// run's miss is a lead, not a verdict. A quarantined site stays in
    /// [`crate::twin::live::SITES`] and stays measured and printed; only the
    /// assertion is withheld. Never widen the bar instead.
    pub const QUARANTINED: &[Quarantine] = &[];

    pub fn quarantine(site: &str) -> Option<&'static Quarantine> {
        QUARANTINED.iter().find(|q| q.site == site)
    }

    /// Whether a site's tally may enter the run's assertions and its pooled
    /// conclusiveness figure.
    pub fn site_is_asserted(site: &str) -> bool {
        quarantine(site).is_none()
    }

    /// The number of bins the twin defines — gates whose level decodes to a
    /// finite value through the product's own codec (for 134 the hybrid LUT,
    /// whose levels 0, 1 and 255 are undefined). What
    /// [`volume_is_scoreable`] gates on.
    pub fn twin_defined_bins(packet: &RadialPacket, codec: &ValueCodec) -> usize {
        packet
            .radials
            .iter()
            .flat_map(|r| r.gate_values.iter())
            .filter(|&&g| codec.decode(g).is_finite())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Sweep, VolumeCoveragePattern,
    };

    const SCALE: f32 = 2.0;
    const OFFSET: f32 = 66.0;
    const GATES: usize = 40;
    /// 1 km gates: cube cell `r` reads gate `r` exactly, so every expected
    /// value is hand-computable without the resampling entering into it.
    const GATE_INTERVAL_M: u16 = 1000;

    fn vcp() -> VolumeCoveragePattern {
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
            Vec::new(),
        )
    }

    /// One reflectivity sweep of 360 radials on cell centres, with dBZ per
    /// azimuth cell from `dbz_at` (`None` = censored, gate byte 0).
    fn refl_sweep(
        elevation_number: u8,
        elevation_deg: f32,
        dbz_at: impl Fn(usize) -> Option<f64>,
    ) -> Sweep {
        let radials = (0..360)
            .map(|i| {
                let byte = match dbz_at(i) {
                    None => 0u8,
                    Some(dbz) => ((dbz * f64::from(SCALE) + f64::from(OFFSET)).round() as i64)
                        .clamp(2, 255) as u8,
                };
                Radial::new(
                    0,
                    i as u16,
                    i as f32 + 0.5,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    Some(MomentData::from_fixed_point(
                        GATES as u16,
                        0,
                        GATE_INTERVAL_M,
                        8,
                        SCALE,
                        OFFSET,
                        vec![byte; GATES],
                    )),
                    None,
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

    /// Four tilts at 0.5°/1.5°/2.5°/3.5° exercising every column rule:
    ///
    /// * az 10: 50 dBZ on all four tilts — a **multi-layer** column summing
    ///   every depth, topped off at the volume ceiling (the top layer stops
    ///   at the beam's upper flank, nothing is extrapolated);
    /// * az 20: 40/30/20/10 dBZ — a graded column down to weak echo;
    /// * az 30: 25/25/censored/10 — a censored gate mid-column, which simply
    ///   drops out of the sum;
    /// * az 40: 15 dBZ everywhere — weak echo, in the sum under the primary
    ///   (0.0248 g/m³) and background under the legacy `IREFMIN` gate;
    /// * az 45: 5 dBZ everywhere — a tiny liquid water under the primary's
    ///   unfloored mapping, a **defined 0.0** under the table's 8.5 dBZ zero
    ///   floor, `NaN` under echo-only;
    /// * az 50: censored everywhere — **no VIL** under every convention;
    /// * az 60/61/62: 60/56/55.5 dBZ on the lowest tilt only — the 56 dBZ
    ///   **cap**: 60 and 56 dBZ both map to 5.40 g/m³, 55.5 to its own 5.10.
    fn golden_scan() -> Scan {
        let profile = |tilt: usize| {
            move |az: usize| -> Option<f64> {
                match az {
                    10 => Some(50.0),
                    20 => Some([40.0, 30.0, 20.0, 10.0][tilt]),
                    30 => [Some(25.0), Some(25.0), None, Some(10.0)][tilt],
                    40 => Some(15.0),
                    45 => Some(5.0),
                    60 => (tilt == 0).then_some(60.0),
                    61 => (tilt == 0).then_some(56.0),
                    62 => (tilt == 0).then_some(55.5),
                    _ => None,
                }
            }
        };
        Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 0.5, profile(0)),
                refl_sweep(2, 1.5, profile(1)),
                refl_sweep(3, 2.5, profile(2)),
                refl_sweep(4, 3.5, profile(3)),
            ],
        )
    }

    /// The documented rules against hand-computed VIL.
    ///
    /// All expectations use the constants pinned in the module: depths at
    /// bin 30's outer edge (RS = 31 km) for the 0.5/1.5/2.5/3.5° ladder are
    /// 0.597664238 / 0.541292613 / 0.541622832 / 0.402838053 km (Σd =
    /// 2.083418), and the unfloored LWs are 2.4757187 (50 dBZ), 0.66416
    /// (40), 0.1781739 (30), 0.0922847 (25), 0.0477986 (20), 0.0247572
    /// (15), 0.0128229 (10), 0.0066416 (5), 5.4520326 (56 and above),
    /// 5.1048974 (55.5) g/m³:
    ///
    /// * az 10: LW(50) · Σd = **5.157956** kg/m²;
    /// * az 20: Σ LWᵢ·dᵢ over 40/30/20/10 = **0.524443**;
    /// * az 30: LW(25)·(d₀ + d₁) + LW(10)·d₃ = **0.110274**;
    /// * az 40: LW(15) · Σd = **0.051580**;
    /// * az 45: LW(5) · Σd = **0.013837** — no zero floor in the primary
    ///   mapping;
    /// * az 60 and 61: 5.4520326·d₀ = **3.258485** — the 56 dBZ cap makes
    ///   60 dBZ and 56 dBZ indistinguishable;
    /// * az 62: 5.1048974·d₀ = **3.051015** — half a dB under the cap is
    ///   its own value.
    #[test]
    fn the_documented_rpg_rules_produce_hand_computed_vil() {
        let grid = compute_vil(&golden_scan());
        assert_eq!(grid.range_bins, RANGE_BINS);
        assert_eq!(grid.values.len(), 360);

        let r = 30;
        assert!(
            (grid.values[10][r] - 5.157_956).abs() < 1e-4,
            "multi-layer column: got {}",
            grid.values[10][r],
        );
        assert!(
            (grid.values[20][r] - 0.524_443).abs() < 1e-5,
            "graded column: got {}",
            grid.values[20][r],
        );
        assert!(
            (grid.values[30][r] - 0.110_274).abs() < 1e-5,
            "censored mid-column gate must drop out: got {}",
            grid.values[30][r],
        );
        assert!(
            (grid.values[40][r] - 0.051_580).abs() < 1e-6,
            "15 dBZ weak echo integrates under the primary: got {}",
            grid.values[40][r],
        );
        assert!(
            (grid.values[45][r] - 0.013_837).abs() < 1e-6,
            "5 dBZ carries its tiny unfloored liquid water: got {}",
            grid.values[45][r],
        );

        assert!(grid.values[50][r].is_nan(), "a censored column made VIL");
        assert!(grid.values[10][GATES].is_nan(), "beyond the data extent");

        assert_eq!(
            grid.values[60][r], grid.values[61][r],
            "60 dBZ and 56 dBZ must both read the capped 5.452 g/m³",
        );
        assert!((grid.values[60][r] - 3.258_485).abs() < 1e-4);
        assert!(
            (grid.values[62][r] - 3.051_015).abs() < 1e-4,
            "55.5 dBZ sits under the cap at its own value: got {}",
            grid.values[62][r],
        );
    }

    /// The legacy `cpc013` reading, kept as an A/B variant: the floored LWC
    /// table (2.47 / 0.17 / 0.09 / 0.66 / 0.04 hundredth-floored g/m³), the
    /// `IREFMIN` gate dropping sub-18.3 dBZ gates from the sum, and
    /// echo-free columns undefined.
    #[test]
    fn the_legacy_threshold_variant_gates_at_18_3_dbz() {
        let grid = compute_vil_impl(&golden_scan(), VilOptions::legacy_threshold());
        let r = 30;
        // az 20's 20 dBZ layer stays, its 10 dBZ ceiling goes:
        // 0.66·d₀ + 0.17·d₁ + 0.04·d₂ = 0.508143.
        assert!(
            (grid.values[20][r] - 0.508_143).abs() < 1e-5,
            "got {}",
            grid.values[20][r],
        );
        // az 30 loses its 10 dBZ ceiling too: 0.09·(d₀ + d₁) = 0.102506.
        assert!((grid.values[30][r] - 0.102_506).abs() < 1e-5);
        assert!(
            grid.values[40][r].is_nan(),
            "15 dBZ background is undefined under the legacy gate",
        );
        assert!(grid.values[45][r].is_nan());
        assert!(grid.values[50][r].is_nan());
        // The table's 56 dBZ saturation: 5.40·d₀ = 3.227387, under the
        // primary's unfloored 3.258485.
        assert!((grid.values[60][r] - 3.227_387).abs() < 1e-4);
        // And 50 dBZ floors to 2.47: 2.47·Σd = 5.146042.
        assert!((grid.values[10][r] - 5.146_042).abs() < 1e-4);
    }

    /// The `A313B1` table literals, straight from `a313.inc` — not from the
    /// formula the implementation uses, so a drifted coefficient, a round
    /// where the table floors, or a mis-set cap each break a different pin.
    /// Data level b encodes (b − 66)/2 dBZ; the table value is hundredths of
    /// g/m³.
    #[test]
    fn liquid_water_reproduces_the_rpgs_lwc_table() {
        for (byte, hundredths) in [
            (82u16, 0.0), // 8.0 dBZ: first zero of the ramp
            (83, 1.0),    // 8.5 dBZ: first nonzero
            (106, 4.0),   // 20 dBZ
            (122, 13.0),  // 28 dBZ
            (130, 23.0),  // 32 dBZ
            (161, 178.0), // 47.5 dBZ — ⌊178.24⌋, a round would say 178 too,
            (160, 166.0), // 47 dBZ — ⌊166.88⌋, but a round would say 167
            (170, 322.0), // 52 dBZ
            (176, 477.0), // 55 dBZ
            (177, 510.0), // 55.5 dBZ: the last unfloored level
            (178, 540.0), // 56 dBZ: the cap (analytic would floor 545)
            (254, 540.0), // 94 dBZ: still the cap
        ] {
            let dbz = (f32::from(byte) - 66.0) / 2.0;
            let got = liquid_water_g_m3(dbz, LwMapping::TableFloor) * 100.0;
            assert!(
                (got - hundredths).abs() < 1e-9,
                "LWC({byte}) = {got}, table says {hundredths}",
            );
        }
        // The analytic variant is unfloored and caps at 56 dBZ's own value.
        let analytic_cap = liquid_water_g_m3(94.0, LwMapping::Analytic);
        assert!((analytic_cap - 5.452_032_582).abs() < 1e-6);
        assert!(analytic_cap > liquid_water_g_m3(94.0, LwMapping::TableFloor));
    }

    /// `A313T1__COMPUTE_DEPTH`'s three cases against hand-computed depths at
    /// bin 100 (outer edge RS = 101 km), ladder 0.5/1.5/2.5/3.5°:
    ///
    /// * lowest: RH·tan 1° + RH²/(2·(4/3)·6371·cos²1°) = **2.363467** km;
    /// * middle: ½·101·cos 1.5°·(tan 2.5° − tan 0.5°) = **1.763566**, and
    ///   ½·101·cos 2.5°·(tan 3.5° − tan 1.5°) = **1.764642**;
    /// * top: ½·101·cos 3.5°·(tan(3.5° + 0.0085 rad) − tan 2.5°) =
    ///   **1.312472** — the 0.017 **radian** beamwidth, not degrees.
    #[test]
    fn layer_depths_follow_a313t1s_three_cases() {
        let d = layer_depths_km(&[0.5, 1.5, 2.5, 3.5], false);
        assert_eq!(d.len(), 4);
        assert_eq!(d[0].len(), RANGE_BINS);
        assert!((d[0][100] - 2.363_467_199).abs() < 1e-8);
        assert!((d[1][100] - 1.763_566_256).abs() < 1e-8);
        assert!((d[2][100] - 1.764_642_130).abs() < 1e-8);
        assert!((d[3][100] - 1.312_472_366).abs() < 1e-8);

        // Two tilts: the lowest and top cases meet with no middle.
        let two = layer_depths_km(&[0.5, 1.5], false);
        assert!((two[0][100] - 2.363_467_199).abs() < 1e-8);
        assert!((two[1][100] - 1.310_883_172).abs() < 1e-8);

        // One tilt: the routine's top case overwrites the lowest, reading
        // tan 0 below — ½·101·cos 0.5°·tan(0.5° + 0.0085 rad).
        let one = layer_depths_km(&[0.5], false);
        assert!((one[0][100] - 0.869_998_572).abs() < 1e-8);

        // The A/B centre datum shifts the whole table half a kilometre of
        // range: strictly smaller depths at every cell.
        let centre = layer_depths_km(&[0.5, 1.5, 2.5, 3.5], true);
        for (edge_row, centre_row) in d.iter().zip(&centre) {
            for (e, c) in edge_row.iter().zip(centre_row) {
                assert!(c < e);
            }
        }

        assert!(layer_depths_km(&[], false).is_empty());
    }

    /// A SAILS repeat late in the volume must not displace the first look:
    /// `viletalg.ftn` sets `ALLOW_SUPPL_SCANS = 0`, so the RPG's VIL is
    /// computed from the volume's first pass only.
    ///
    /// The repeat carries 50 dBZ where the first 0.5° look has 30 dBZ —
    /// LW 2.47 against 0.17 g/m³ — so a newest-wins dedup would move the
    /// answer by an order of magnitude, not just the provenance.
    #[test]
    fn a_sails_repeat_does_not_displace_the_first_look() {
        let first = |az: usize| (az == 61).then_some(30.0);
        let upper = |az: usize| (az == 61).then_some(10.0);
        let repeat = |az: usize| match az {
            60 => Some(50.0),
            61 => Some(50.0),
            _ => None,
        };
        let scan = Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 0.5, first),
                refl_sweep(2, 1.5, upper),
                refl_sweep(3, 0.5, repeat), // SAILS revisit, late
            ],
        );
        let grid = compute_vil(&scan);

        // az 60 exists only on the repeat: first-of-volume leaves it empty.
        assert!(
            grid.values[60][30].is_nan(),
            "the SAILS repeat displaced the first look",
        );

        // az 61 uses the FIRST look's 30 dBZ on the two-tilt ladder's lowest
        // depth at RS = 31, plus the 10 dBZ ceiling:
        // 0.1781739 · 0.597664238 + 0.0128229 · 0.402350280 = 0.111647; the
        // repeat's 50 dBZ would put the first term at 1.479676.
        assert!(
            (grid.values[61][30] - 0.111_647).abs() < 1e-5,
            "got {} — the repeat's reflectivity leaked into the sum",
            grid.values[61][30],
        );
    }

    /// Amburn & Wolf's formula against hand-computed pairs: 20 kg/m² over a
    /// 10 km top (32.8084 kft = 10,000 m) is exactly 2.0 g/m³, and 35 kg/m²
    /// over the same top is their 3.5 g/m³ severe-hail break. `NaN` in
    /// either slot, or a non-positive top, is `NaN` out.
    #[test]
    fn vil_density_reproduces_the_amburn_wolf_pairs() {
        assert!((vil_density_g_m3(20.0, 32.8084) - 2.0).abs() < 1e-5);
        assert!((vil_density_g_m3(35.0, 32.8084) - 3.5).abs() < 1e-5);
        // One kilofoot of top: 1000·VIL/304.8.
        assert!((vil_density_g_m3(1.0, 1.0) - 3.280_84).abs() < 1e-4);

        assert!(vil_density_g_m3(f32::NAN, 32.8).is_nan(), "undefined VIL");
        assert!(vil_density_g_m3(20.0, f32::NAN).is_nan(), "undefined top");
        assert!(vil_density_g_m3(f32::NAN, f32::NAN).is_nan());
        assert!(vil_density_g_m3(20.0, 0.0).is_nan(), "a zero top divides");
        assert!(vil_density_g_m3(20.0, -1.0).is_nan());
        assert!(vil_density_g_m3(f32::INFINITY, 32.8).is_nan());
    }

    /// The volume product wires the two derivations together: on the golden
    /// scan's topped 50 dBZ column (az 10, r 30) VIL is 5.157956 kg/m² and
    /// the echo top (radar at 0 ft MSL) is the 3.5° beam centre at 30.5 km,
    /// 6.306813 kft — VILD = 1000·5.157956/(6.306813·304.8) =
    /// **2.683198 g/m³**. Columns where either input is undefined are `NaN`:
    /// az 45 carries VIL (a defined 0.0-ish weak column) but no echo top,
    /// az 50 carries neither.
    #[test]
    fn vil_density_composes_the_two_derivations() {
        let grid = compute_vil_density(&golden_scan(), 0.0);
        assert_eq!(grid.range_bins, RANGE_BINS);
        let r = 30;
        assert!(
            (grid.values[10][r] - 2.683_198).abs() < 1e-4,
            "topped core column: got {}",
            grid.values[10][r],
        );
        assert!(
            grid.values[45][r].is_nan(),
            "VIL defined but no echo top: VILD must be NaN, not 0",
        );
        assert!(grid.values[50][r].is_nan(), "a censored column");
        assert!(grid.values[10][GATES].is_nan(), "beyond the data extent");
    }

    /// The A/B knobs really vary the conventions they name: on a uniform
    /// 56 dBZ column the floored table reads its saturated 5.40 against the
    /// primary's unfloored cap, and the centre depth datum shrinks every
    /// layer.
    #[test]
    fn the_ab_variants_move_in_the_documented_directions() {
        let scan = Scan::new(
            vcp(),
            vec![
                refl_sweep(1, 0.5, |az| (az == 10).then_some(56.0)),
                refl_sweep(2, 1.5, |az| (az == 10).then_some(56.0)),
            ],
        );
        let primary = compute_vil_impl(&scan, VilOptions::primary());
        let table = compute_vil_impl(
            &scan,
            VilOptions {
                lw: LwMapping::TableFloor,
                ..VilOptions::primary()
            },
        );
        let centred = compute_vil_impl(
            &scan,
            VilOptions {
                depth_at_centre: true,
                ..VilOptions::primary()
            },
        );
        let r = 30;
        // 5.40/5.452 exactly, since LW factors out of the whole column.
        let ratio = f64::from(table.values[10][r]) / f64::from(primary.values[10][r]);
        assert!((ratio - 5.40 / 5.452_032_582).abs() < 1e-4, "ratio {ratio}");
        assert!(centred.values[10][r] < primary.values[10][r]);
        // Uninvolved cells agree bit for bit: NaN everywhere else.
        assert!(primary.values[11][r].is_nan());
        assert!(table.values[11][r].is_nan());
    }
}

/// Offline pins on the validation policy — everything the ignored live test
/// decides with.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod policy_tests {
    use super::validation_policy as policy;
    use crate::twin::compare::ValueCodec;
    use nexrad_level3::model::{RadialPacket, RadialRun};

    fn packet(gate_values: Vec<u16>) -> RadialPacket {
        RadialPacket {
            first_range_bin: 0,
            num_range_bins: gate_values.len() as u16,
            i_center: 0,
            j_center: 0,
            scale_factor: 1.0,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials: vec![RadialRun {
                start_angle: 0.0,
                angle_delta: 1.0,
                gate_values,
            }],
        }
    }

    /// The campaign's bars, pinned so the ignored harness cannot drift them.
    #[test]
    fn the_acceptance_bar_is_what_the_campaign_set() {
        assert_eq!(policy::ACCEPTANCE_BAR_WITHIN_ONE_PCT, 99.0);
        assert_eq!(policy::PRESENCE_DISAGREEMENT_MAX_PCT, 2.0);
        assert_eq!(policy::MIN_SITES, 4);
        assert_eq!(policy::MIN_DEFINED_BINS, 10_000);
        assert_eq!(policy::MIN_TWIN_DEFINED_BINS, 500);

        assert!(policy::meets_acceptance_bar(99.0), "the bar is inclusive");
        assert!(!policy::meets_acceptance_bar(98.999_999));
        assert!(policy::presence_is_acceptable(2.0));
        assert!(!policy::presence_is_acceptable(2.000_001));
    }

    /// The conclusiveness fold: both legs required, boundaries inclusive.
    #[test]
    fn a_run_is_conclusive_only_with_enough_sites_and_bins() {
        assert!(policy::sample_is_conclusive(4, 10_000));
        assert!(!policy::sample_is_conclusive(3, 1_000_000), "sites gate");
        assert!(!policy::sample_is_conclusive(20, 9_999), "bins gate");
        assert!(!policy::sample_is_conclusive(0, 0));
    }

    /// The near-empty-volume skip rule.
    #[test]
    fn near_empty_volumes_are_skipped_not_scored() {
        assert!(!policy::volume_is_scoreable(0));
        assert!(!policy::volume_is_scoreable(499));
        assert!(policy::volume_is_scoreable(500));
    }

    /// The quarantine table starts empty, and any entry ever added must name
    /// a site the harness still measures — a quarantined site silently
    /// dropped from `SITES` is a site nobody would notice had got worse.
    #[test]
    fn the_quarantine_table_is_empty_and_would_stay_measured() {
        assert!(
            policy::QUARANTINED.is_empty(),
            "an entry appeared: it needs evidence from ≥2 volumes across ≥2 \
             runs recorded in its `why`, per the table's doc",
        );
        for q in policy::QUARANTINED {
            assert!(
                crate::twin::live::SITES.contains(&q.site),
                "{} ({:?}) is quarantined but no longer measured: {}",
                q.site,
                q.scope,
                q.why,
            );
        }
        assert!(policy::quarantine("KMPX").is_none());
        assert!(policy::site_is_asserted("KMPX"));
        // The only scope an entry could carry: whole-site. Constructed here
        // so the empty table does not leave the enum dead.
        assert_eq!(format!("{:?}", policy::Scope::Whole), "Whole");
    }

    /// The scoreability count decodes through the twin's own codec: levels
    /// 0 and 1 are undefined, a `NaN` LUT level (255, the reserved slot) is
    /// undefined, defined levels count.
    #[test]
    fn twin_defined_bins_counts_codec_defined_gates_only() {
        let mut lut = vec![f32::NAN; 256];
        for (i, slot) in lut.iter_mut().enumerate().take(255).skip(2) {
            *slot = i as f32;
        }
        let codec = ValueCodec::Lut(lut);
        assert_eq!(
            policy::twin_defined_bins(&packet(vec![0, 1, 2, 130, 254, 255]), &codec),
            3,
        );
    }
}

/// The live twin harness: score the derivation against the RPG's own Digital
/// VIL for the **same volume**, across [`crate::twin::live::SITES`], in the
/// twin's own hybrid LUT data levels.
///
/// ```text
/// cargo test -p rustdar-radar --release --lib -- --ignored --nocapture live_derived_vil
/// ```
#[cfg(all(test, not(target_arch = "wasm32")))]
mod live_validation {
    use super::validation_policy as policy;
    use super::{LwMapping, VilOptions};
    use crate::sources::DataSources;
    use crate::twin::{compare, live};
    use crate::volumetric::CellStat;

    /// The bounded A/B matrix: the documented conventions only, primary
    /// first. Nothing outside this list is ever tried — the campaign's
    /// early-stop rule. One row carries the depth-datum knob (measured at
    /// under two points everywhere, never better than the edge datum); the
    /// table and threshold rows record what the legacy `cpc013` readings
    /// cost.
    const AB_MATRIX: &[(&str, VilOptions)] = &[
        ("zmean/nothresh/analytic/edge", VilOptions::primary()),
        (
            "zmean/nothresh/analytic/centre",
            VilOptions {
                depth_at_centre: true,
                ..VilOptions::primary()
            },
        ),
        (
            "zmean/nothresh/table/edge",
            VilOptions {
                lw: LwMapping::TableFloor,
                ..VilOptions::primary()
            },
        ),
        (
            "zmean/18.3/analytic/edge",
            VilOptions {
                min_refl: Some(super::VIL_MIN_REFL_DBZ),
                ..VilOptions::primary()
            },
        ),
        (
            "zmean/18.3/table/edge/echo-only",
            VilOptions::legacy_threshold(),
        ),
        (
            "max/nothresh/analytic/edge",
            VilOptions {
                stat: CellStat::Max,
                ..VilOptions::primary()
            },
        ),
        (
            "max/nothresh/table/edge",
            VilOptions {
                stat: CellStat::Max,
                lw: LwMapping::TableFloor,
                ..VilOptions::primary()
            },
        ),
        (
            "max/18.3/table/edge",
            VilOptions {
                stat: CellStat::Max,
                lw: LwMapping::TableFloor,
                min_refl: Some(super::VIL_MIN_REFL_DBZ),
                ..VilOptions::primary()
            },
        ),
    ];

    /// Per site: the archived Level II volume nearest now, the DVL object
    /// generated **from that volume** (paired by PDB volume start, never key
    /// freshness), our derivation, and a tally in the twin's own hybrid LUT
    /// levels via [`compare::ValueCodec::for_message`] — never a fixed
    /// physical tolerance, which the LUT's log region makes meaningless.
    /// Per-site assertion on the primary conventions; the A/B matrix is
    /// printed alongside for the record.
    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_derived_vil_matches_the_rpgs_own_product() {
        crate::tls::init();
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();

        let mut asserted_sites = 0usize;
        let mut pooled_compared = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for &site in live::SITES {
            let Some((scan, l2_start)) = live::l2_volume_near(site, now).await else {
                println!("{site}: SKIP — no archived Level II volume found");
                continue;
            };
            let Some(twin) = live::l3_twin(&sources, site, "DVL", l2_start, None).await else {
                println!("{site}: SKIP — no DVL twin names volume {l2_start}");
                continue;
            };
            if twin.message.pdb.product_code != 134 {
                println!(
                    "{site}: SKIP — twin {} decodes as product {}",
                    twin.stamp.key, twin.message.pdb.product_code,
                );
                continue;
            }
            let Some(packet) = crate::srm::radial_packet(&twin.message) else {
                println!(
                    "{site}: SKIP — twin {} has no radial packet",
                    twin.stamp.key
                );
                continue;
            };
            let Some(codec) = compare::ValueCodec::for_message(&twin.message) else {
                println!("{site}: SKIP — twin {} has no codec", twin.stamp.key);
                continue;
            };

            let defined = policy::twin_defined_bins(packet, &codec);
            if !policy::volume_is_scoreable(defined) {
                println!(
                    "{site}: SKIP — near-empty twin ({defined} defined bins < {})",
                    policy::MIN_TWIN_DEFINED_BINS,
                );
                continue;
            }

            let mut primary_tally = None;
            for (label, opts) in AB_MATRIX {
                let grid = super::compute_vil_impl(&scan, *opts);
                let Some(t) = compare::tally_against_l3(
                    &grid.values,
                    &twin.message,
                    compare::ProductKind::Numeric,
                ) else {
                    continue;
                };
                let tag = if primary_tally.is_none() {
                    "PRIMARY"
                } else {
                    "     ab"
                };
                println!(
                    "{site}: {tag} {label:22} | compared {} exact {:.2}% ±1 {:.2}% ±2 {:.2}% \
                     presence {:.2}% (derived {} / twin {})",
                    t.compared,
                    t.exact_pct(),
                    t.within_one_pct(),
                    t.within_two_pct(),
                    t.presence_disagreement_pct(),
                    t.derived_defined,
                    t.l3_defined,
                );
                if primary_tally.is_none() {
                    primary_tally = Some(t);
                }
            }
            let Some(heights) = primary_tally else {
                println!("{site}: SKIP — no tally produced");
                continue;
            };
            // The twin's own level mass, for the record: level 2 is a
            // defined 0.0 kg/m², 3–19 the 0.011 kg/m²-step linear region,
            // 20+ the log region (real DVL PDBs carry log_start = 20).
            let (mut l0, mut l2, mut lin, mut log_r) = (0usize, 0usize, 0usize, 0usize);
            for g in packet.radials.iter().flat_map(|r| r.gate_values.iter()) {
                match g {
                    0 | 1 => l0 += 1,
                    2 => l2 += 1,
                    3..=19 => lin += 1,
                    _ => log_r += 1,
                }
            }
            println!(
                "{site}: vol {l2_start} twin {} VCP {} | twin defined {defined} \
                 (levels: undef {l0}, zero {l2}, linear {lin}, log {log_r})",
                twin.stamp.key, twin.message.pdb.vcp,
            );

            if !policy::site_is_asserted(site) {
                println!("{site}: measured but quarantined — not asserted");
                continue;
            }

            let mut misses = Vec::new();
            if !policy::meets_acceptance_bar(heights.within_one_pct()) {
                misses.push(format!(
                    "within-one {:.2}% < {}%",
                    heights.within_one_pct(),
                    policy::ACCEPTANCE_BAR_WITHIN_ONE_PCT,
                ));
            }
            if !policy::presence_is_acceptable(heights.presence_disagreement_pct()) {
                misses.push(format!(
                    "presence disagreement {:.2}% > {}%",
                    heights.presence_disagreement_pct(),
                    policy::PRESENCE_DISAGREEMENT_MAX_PCT,
                ));
            }
            if !misses.is_empty() {
                failures.push(format!("{site} ({l2_start}): {}", misses.join("; ")));
            }
            asserted_sites += 1;
            pooled_compared += heights.compared;
        }

        println!(
            "asserted {asserted_sites} sites, {pooled_compared} bins pooled; failures: {}",
            failures.len(),
        );
        assert!(
            failures.is_empty(),
            "sites under the bar:\n  {}",
            failures.join("\n  "),
        );
        assert!(
            policy::sample_is_conclusive(asserted_sites, pooled_compared),
            "inconclusive run: {asserted_sites} sites / {pooled_compared} bins asserted, \
             need ≥{} sites and ≥{} bins — re-run when more sites carry echo",
            policy::MIN_SITES,
            policy::MIN_DEFINED_BINS,
        );
    }
}
