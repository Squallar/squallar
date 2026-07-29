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
//! Product 134 therefore **stays a Level III fetch**, and this module ships
//! with that provenance documented: as the twin harness's derived side, as
//! the hail SHI column's liquid-water machinery, and as the retired VIL
//! density input recorded below.
//!
//! # VIL density — measured 2026-07-29, and the local derivation retired
//!
//! **Outcome first**: VIL density is no longer computed here. It is now
//! [`crate::vild`] — the RPG's own two published products divided,
//! `1000 · DVL / ((EET_published + 0.5) · 304.8)` — because the survey below
//! measured the local `compute_vil / compute_eet` quotient against exactly
//! that expression and found it **effectively mute at the thresholds the
//! product is read for**. The residual is [`compute_vil`]'s, and it is the
//! DQA gap recorded above, which is not reachable from raw Level II. Rather
//! than ship a hail discriminator that does not discriminate, the product was
//! rebuilt as the reference itself: both inputs are already fetched and drawn
//! by the app, so the change costs no new datasource and the shipped field is
//! now as accurate as the RPG's own products allow.
//!
//! Its remaining error is the reference's own **quantization noise floor**,
//! which is therefore now the product's stated accuracy limit: product 135
//! publishes whole kilofeet, so ±0.5 kft of echo top is ±0.057 g/m³ at a
//! 30 kft top and VILD 3.5 (±0.113 at 15 kft, ±0.035 at 50), and ~±0.1 g/m³
//! once product 134's log-region step is included. See
//! [`crate::vild::quantization_halfwidth_g_m3`]. That floor is why the bars
//! below are decision bars rather than value-agreement bars, and why nothing
//! anywhere claims this field to better than about a tenth of a g/m³.
//!
//! **The survey's construction is the shipped product.** The harness does not
//! rebuild the quotient: [`vild_validation_policy`] re-exports
//! [`crate::vild`]'s own constructors, and
//! `vild::tests::the_shipped_path_is_the_surveys_reference_construction` pins
//! that [`crate::vild::compute_vild`] composes them in exactly the order the
//! harness does. So the survey below now scores the field the app draws by
//! construction, and its PRIMARY row is expected to be perfect; what the run
//! still measures is the two **attribution** rows, which are the record of how
//! far the retired local inputs sat from the RPG's.
//!
//! **Survey**: 41 precipitating site-hours over 2026-07-28 21 UTC →
//! 2026-07-29 18 UTC, every site-hour the shared reconnaissance scan
//! (`live_hca_precip_site_scan`, lowest-cut gates ≥ 35 dBZ) reported at three
//! candidate hours — southern and central plains (KDDC 19,966 hot gates,
//! KAMA 10,858, KUEX, KEAX, KABR, KOAX, KFSD, KMVX, KBIS), Appalachian
//! (KMRX 15,896), Florida and gulf (KMLB 9,098, KTLH 7,538, KMOB, KPAH),
//! mid-south (KLZK, KSHV, KSGF, KTLX) and mountain west (KSFX 3,915, KMTX
//! 3,435). 310,215 domain cells.
//!
//! **Verdict on the local derivation: the survey does not conclude, and every
//! direction it does resolve is a miss.** This is what retired it.
//!
//! * **The pinned per-site legs all pass, and passing them means nothing
//!   here.** Threshold agreement runs 99.45–100% at both breaks (bar 95%) and
//!   the false-high rate 0.00–0.03% (bar 3%), at all 41 site-hours. They pass
//!   because only **154** of 310,215 cells cross 3.5 g/m³ on the reference at
//!   all: a metric whose denominator is the whole field cannot fail on a
//!   decision that is never taken. This is exactly the triviality
//!   [`vild_validation_policy::FLAG_FAR_MAX_PCT`]'s doc warns about, and why
//!   the run is gated on
//!   [`vild_validation_policy::flag_sample_is_conclusive`] as well.
//! * **The decision metric is INCONCLUSIVE and pointing the wrong way.**
//!   Pooled at 3.5 g/m³ the reference flags 154 cells, we flag 24, 21 of them
//!   shared: **POD 13.64%**, FAR 12.50%, CSI 13.38%. At 4.0 the reference
//!   flags 105 and we flag 2: **POD 1.90%**. Both reference populations sit
//!   under the 200-cell conclusiveness gate, so no site is credited with a
//!   pass — but the shortfall is not marginal, it is five to fifty times the
//!   bar. Widening the sample from 13 site-hours to 41 (28 of them playing no
//!   part in any choice) moved POD from 12.95% to 13.64% and FAR from 14.29%
//!   to 12.50%: the figure is stable, the sample is not the problem.
//! * **The predicted HIGH bias does not appear. A LOW bias does.** The signed
//!   mean (ours − reference) is negative at 36 of 41 site-hours (−0.2278 …
//!   +0.0080 g/m³; every positive one is a site with no decision-region cells
//!   at all, at ≤ +0.008). At all 14 site-hours carrying decision-region mass
//!   (reference ≥ 3.0 g/m³) it is negative, **−1.23 … −4.78 g/m³**. Our field
//!   simply cannot reach the reference's values: maxima 3.98 against 12.14
//!   (KABR), 2.49/5.98 (KMOB), 1.72/5.02 (KUEX), 2.37/4.11 (KAMA),
//!   4.60/6.21 (KMRX).
//! * **Distribution agreement**: MAE 0.008–0.243 g/m³, within ±0.5 g/m³
//!   84.2–100%, but within ±15% only **8.0–40.2%** (median ~26%). The field
//!   is close in absolute terms because it is mostly small; it is *not* close
//!   in relative terms anywhere.
//! * **Input attribution — VIL, not the echo top, and it is not close.**
//!   Swapping one input at a time against the same reference: our VIL over
//!   the RPG's published top scores **POD 8.44%** (worse than the primary —
//!   it reproduces the entire miss), while the RPG's DVL over *our* echo top
//!   scores **POD 88.31%, CSI 65.38%**. In the decision region the VIL term
//!   is −1.32 … −4.92 g/m³ and negative at every site; the echo-top term is
//!   −0.55 … +0.74 g/m³ with mixed sign. Whole-field ±15% is 55–92% for the
//!   echo-top-only row against 8–36% for the VIL-only row.
//!
//!   So the two documented errors **partially cancel** — [`crate::eet`]'s low
//!   storm-core bias does push VILD up, visibly (that row's FAR reaches
//!   28.4%, over the 25% ceiling, and its decision-region mean runs to +0.74
//!   g/m³ at KABR) — but it is a fifth to a twentieth of VIL's term and of
//!   the opposite sign. VILD inherits VIL's low bias, and VIL's residual is
//!   the DQA gap recorded above. **Fixing the echo top would not move this
//!   product.**
//! * **Reference-datum A/B**: the bin centre is pinned from the ICD's
//!   encoding (`level = ⌊kft⌋ + 2`) before any run, not tuned; dividing by
//!   the published floor instead moves pooled POD 13.64% → 12.00% and changes
//!   no verdict at any of the 41 site-hours. Roughly a fifth of the
//!   reference's own flagged cells (30 of 154 at 3.5, 24 of 105 at 4.0) sit
//!   inside their own quantization halfwidth of the break — enough to blur
//!   the edge of the count, nowhere near enough to explain a 13% POD.
//!
//! Nothing was tuned to this result and no bar was moved to accommodate it.
//! Two legs were *added* after the first 13-site-hour run, both making the
//! survey stricter and both because a FAR-only skill bar is passed by
//! silence: [`vild_validation_policy::FLAG_POD_MIN_PCT`] and the
//! reference-flagged conclusiveness gate. The 28 later site-hours played no
//! part in either and reproduced every figure.
//!
//! What this did **not** say: that the local quotient's arithmetic was wrong
//! (the offline pins hand-computed it), or its wiring. What it said is that the
//! one thing VIL density is *read* for — Amburn & Wolf's severe-hail break —
//! was where it failed, that it failed by under-warning rather than the
//! suspected over-warning, and that the cause was [`compute_vil`]'s residual,
//! not [`crate::eet::compute_eet`]'s: **fixing the echo top would not have
//! moved the product**. The campaign's decision was the third option the
//! attribution left open — take the reference as the product. See
//! [`crate::vild`].
//!
//! ## Re-run against the shipped Level III product, 2026-07-29
//!
//! Eight precipitating site-hours (KUEX, KOAX, KEAX, KSGF, KLZK, KDDC at 12
//! UTC, KTLH and KMLB at 18 UTC — 1,700 to 55,800 lowest-cut gates ≥ 35 dBZ
//! each), 83,098 domain cells pooled. The result is what the construction
//! forces, and confirming it is the point of the run:
//!
//! * **PRIMARY (the shipped [`crate::vild::compute_vild`], entry point and
//!   all)**: threshold agreement **100.00%** at both breaks at every
//!   site-hour, false-high 0, MAE **0.0000** g/m³, within ±0.5 and within ±15%
//!   both 100.00%, pooled **POD 100.00% / FAR 0.00% / CSI 100.00%** over the
//!   33 cells the reference flags at 3.5 and the 25 it flags at 4.0. Not
//!   "close": bit-identical, which is the anti-drift pin passing on live data.
//! * **The retired local quotient**, scored on the same volumes: POD **3.03%**
//!   at 3.5, **0.00%** at 4.0, FAR 66.67%, and −5.55 … −7.92 g/m³ of signed
//!   bias at the site-hours with decision-region mass. Attribution reproduces
//!   too — our-VIL/RPG-top POD 3.03%, RPG-DVL/our-top POD 96.97%.
//! * **The datum A/B** still costs a decision: dividing by the published floor
//!   instead of the bin centre moves POD 100% → 97.06% at 3.5 (one cell of 34).
//! * The pooled **conclusiveness gate still reads INCONCLUSIVE** — 33
//!   reference-flagged cells against
//!   [`vild_validation_policy::MIN_FLAGGED_CELLS`]'s 200 — for exactly the
//!   reason it did over 41 site-hours (154 cells): real volumes carry tens of
//!   cells above 3.5 g/m³, not hundreds. That gate is now a statement about
//!   the *weather sample*, and it has deliberately **not** been relaxed: the
//!   failure mode it guards against (a mute derivation shrinking its own
//!   sample out of judgement) is impossible for a product that is its own
//!   reference, but it will still catch the next derivation that is not.
//!
//! Rendered fields for the record. KUEX 12:00:54, a stratiform-dominant MCS:
//! 39,004 defined cells, mostly 0.3–1.5 g/m³ with discrete 2–3.5 g/m³ cores
//! sitting on the same NNE-through-SE arc the 0.5° reflectivity carries its
//! 50+ dBZ cells on, and nothing crossing 3.5 — correctly, for that storm
//! mode. KDDC 12:01:01, a hail day: 65 cells ≥ 1 g/m³, 21 ≥ 3.5, 19 ≥ 4.0,
//! maximum **12.06** g/m³ in one compact cluster. That maximum is well past
//! Amburn & Wolf's scale and it is the **RPG's own** — a large `DVL` over a low
//! published `EET`, reproduced cell for cell on both sides of the comparison —
//! so it is the reference's behaviour to read, not a defect here.

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
    pub(super) fn golden_scan() -> Scan {
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

/// The parts of [`live_vild_validation`] that decide **what counts as
/// passing** for VIL density.
///
/// Outside the ignored module for the reason [`validation_policy`] is: the
/// live harness never runs under `cargo test --workspace`, so a bar defined
/// inside it could be quietly weakened without a default-suite test noticing.
/// Out here `vild_policy_tests` reaches all of it offline, and does.
///
/// # The reference is [`crate::vild`]
///
/// VIL density has no Level III twin — but the RPG publishes **both of its
/// inputs** for the same volume: product 134 (`DVL`, kg/m²) and product 135
/// (`EET`, kft above MSL). Dividing one by the other, cell for cell on the
/// shared 1° × 1 km grid, is the RPG's own VIL density in everything but
/// name:
///
/// ```text
/// VILD_ref = 1000 · DVL / ((EET_published + 0.5) · 304.8)     g/m³
/// ```
///
/// That expression is [`crate::vild`], and the constructors below are
/// **re-exports of it** rather than a second implementation. Nothing here
/// rebuilds the quotient, the bin-centre datum
/// ([`crate::vild::EET_BIN_CENTRE_KFT`] — see that module for why `+ 0.5` is
/// the unbiased estimator, and what the residual ±0.5 kft costs) or the
/// resampling; a private copy of the arithmetic in here is exactly how a
/// survey comes to bless a field the app does not draw.
///
/// [`reference_vild_on_published_floor`] is the one thing that stays local: it
/// is the datum A/B row, a deliberately *wrong* denominator kept so the choice
/// stays measured rather than assumed.
///
/// No value-agreement bar is pinned, only decision bars: the reference cannot
/// resolve VIL density finer than roughly ±0.1 g/m³ at the threshold
/// ([`crate::vild::quantization_halfwidth_g_m3`]), so a "within ±0.05 g/m³"
/// bar would be measuring product 135's quantization rather than the
/// derivation. The ±0.5 g/m³ and ±15% figures are *reported* (both comfortably
/// above the floor).
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod vild_validation_policy {
    use crate::volumetric::RANGE_BINS;

    /// The reference's construction, re-exported from the module that owns it.
    /// See this module's doc.
    pub use crate::vild::{
        EET_BIN_CENTRE_KFT, EET_QUANTUM_KFT, density_field, published_top_field,
        quantization_halfwidth_g_m3 as reference_quantization_halfwidth_g_m3, resampled_field,
        vild_from_published as reference_vild,
    };

    /// Amburn & Wolf (1997)'s two operational breaks, g/m³: below 3.5 severe
    /// hail is rare, at 4.0 and above it is near-universal. Both are scored;
    /// both are asserted.
    pub const HAIL_RARE_BELOW_G_M3: f32 = 3.5;
    pub const HAIL_NEAR_CERTAIN_AT_G_M3: f32 = 4.0;

    /// The datum A/B row: the same reference on the **literal** decode,
    /// dividing by the published bin's lower edge. Always the larger of the
    /// two, and deliberately not what [`crate::vild`] uses.
    pub fn reference_vild_on_published_floor(dvl_kg_m2: f32, eet_published_kft: f32) -> f32 {
        if !eet_published_kft.is_finite() || eet_published_kft <= 0.0 {
            return f32::NAN;
        }
        crate::vild::vild_g_m3(dvl_kg_m2, eet_published_kft)
    }

    /// The decision the product is read for, at one threshold: which side of
    /// it each cell sits on, ours against the reference.
    ///
    /// A cell counts wherever **either** side is defined — an undefined cell
    /// is one nothing is painted in, which is operationally "not flagged", so
    /// a cell we define at 5 g/m³ where the reference defines nothing at all
    /// is a false high, not an excluded cell. Cells undefined on both sides
    /// are not in the domain.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub struct ThresholdConfusion {
        /// Both sides at or above the threshold.
        pub hits: usize,
        /// We flag severe-hail-likely, the reference does not — the
        /// over-warning failure mode a low echo top predicts.
        pub false_high: usize,
        /// The reference flags, we do not.
        pub false_low: usize,
        /// Both sides below the threshold (or one of them undefined).
        pub agreed_below: usize,
    }

    impl ThresholdConfusion {
        /// Cells where either side is defined.
        pub fn domain(&self) -> usize {
            self.hits + self.false_high + self.false_low + self.agreed_below
        }

        /// **The primary metric**: share of the domain the two sides put on
        /// the same side of the threshold.
        pub fn agreement_pct(&self) -> f64 {
            100.0 * (self.hits + self.agreed_below) as f64 / self.domain().max(1) as f64
        }

        pub fn false_high_pct(&self) -> f64 {
            100.0 * self.false_high as f64 / self.domain().max(1) as f64
        }

        pub fn false_low_pct(&self) -> f64 {
            100.0 * self.false_low as f64 / self.domain().max(1) as f64
        }

        /// Cells either side flags — the population the skill figures below
        /// are meaningful over.
        pub fn flagged_union(&self) -> usize {
            self.hits + self.false_high + self.false_low
        }

        /// Cells the **reference** flags. This, not the union, is what a
        /// run's conclusiveness is gated on: the union shrinks when *we* stay
        /// silent, so gating on it would let a mute derivation declare its
        /// own sample too small to judge it.
        pub fn reference_flagged(&self) -> usize {
            self.hits + self.false_low
        }

        /// False alarm ratio of our flag: the share of the area **we** flag
        /// that the reference does not. Zero when we flag nothing.
        pub fn far_pct(&self) -> f64 {
            100.0 * self.false_high as f64 / (self.hits + self.false_high).max(1) as f64
        }

        /// Probability of detection: the share of the reference's flagged
        /// area we also flag.
        pub fn pod_pct(&self) -> f64 {
            100.0 * self.hits as f64 / (self.hits + self.false_low).max(1) as f64
        }

        /// Critical success index over the flagged union.
        pub fn csi_pct(&self) -> f64 {
            100.0 * self.hits as f64 / self.flagged_union().max(1) as f64
        }

        /// Pool another site-hour's cells into this one. Real convective
        /// volumes carry only tens of cells above 3.5 g/m³, so the run's
        /// decision figures are only meaningful pooled.
        pub fn merge(&mut self, other: &Self) {
            self.hits += other.hits;
            self.false_high += other.false_high;
            self.false_low += other.false_low;
            self.agreed_below += other.agreed_below;
        }
    }

    /// Score `ours` against `reference` at one threshold. Both grids are
    /// `[az][range]`, `NaN` undefined.
    pub fn confuse(
        ours: &[Vec<f32>],
        reference: &[Vec<f32>],
        threshold: f32,
    ) -> ThresholdConfusion {
        let mut c = ThresholdConfusion::default();
        for az in 0..360 {
            for r in 0..RANGE_BINS {
                let o = ours.get(az).and_then(|row| row.get(r)).copied();
                let f = reference.get(az).and_then(|row| row.get(r)).copied();
                let o_flag = o.is_some_and(|v| v.is_finite() && v >= threshold);
                let f_flag = f.is_some_and(|v| v.is_finite() && v >= threshold);
                if !o.is_some_and(f32::is_finite) && !f.is_some_and(f32::is_finite) {
                    continue;
                }
                match (o_flag, f_flag) {
                    (true, true) => c.hits += 1,
                    (true, false) => c.false_high += 1,
                    (false, true) => c.false_low += 1,
                    (false, false) => c.agreed_below += 1,
                }
            }
        }
        c
    }

    /// The reported (not pinned) value tolerances — see the module doc for
    /// why neither is a bar: both sit well above the reference's own ~±0.1
    /// g/m³ floor, so they characterise the field without deciding anything.
    pub const VALUE_TOLERANCE_G_M3: f32 = 0.5;
    pub const VALUE_TOLERANCE_FRACTION: f32 = 0.15;

    /// The decision region's floor, g/m³: cells the reference puts within
    /// half a g/m³ of Amburn & Wolf's lower break, where a signed bias
    /// actually moves warnings.
    pub const DECISION_REGION_MIN_G_M3: f32 = 3.0;

    /// Signed agreement over the cells both sides define.
    #[derive(Debug, Default, Clone)]
    pub struct ValueStats {
        pub n: usize,
        /// Mean of (ours − reference), g/m³.
        pub mean_diff: f64,
        pub median_diff: f64,
        pub mae: f64,
        pub within_abs: usize,
        pub within_rel: usize,
    }

    impl ValueStats {
        pub fn within_abs_pct(&self) -> f64 {
            100.0 * self.within_abs as f64 / self.n.max(1) as f64
        }

        pub fn within_rel_pct(&self) -> f64 {
            100.0 * self.within_rel as f64 / self.n.max(1) as f64
        }
    }

    /// Signed statistics over the cells **both** sides define whose reference
    /// value is at least `reference_at_least` (0.0 for the overall figure,
    /// [`DECISION_REGION_MIN_G_M3`] for the decision region).
    pub fn value_stats(
        ours: &[Vec<f32>],
        reference: &[Vec<f32>],
        reference_at_least: f32,
    ) -> ValueStats {
        let mut diffs: Vec<f64> = Vec::new();
        let mut s = ValueStats::default();
        for az in 0..360 {
            for r in 0..RANGE_BINS {
                let (Some(&o), Some(&f)) = (
                    ours.get(az).and_then(|row| row.get(r)),
                    reference.get(az).and_then(|row| row.get(r)),
                ) else {
                    continue;
                };
                if !o.is_finite() || !f.is_finite() || f < reference_at_least {
                    continue;
                }
                let d = f64::from(o) - f64::from(f);
                diffs.push(d);
                s.mae += d.abs();
                s.within_abs += usize::from(d.abs() <= f64::from(VALUE_TOLERANCE_G_M3));
                s.within_rel +=
                    usize::from(d.abs() <= f64::from(VALUE_TOLERANCE_FRACTION) * f64::from(f));
            }
        }
        s.n = diffs.len();
        if s.n > 0 {
            s.mean_diff = diffs.iter().sum::<f64>() / s.n as f64;
            s.mae /= s.n as f64;
            diffs.sort_by(f64::total_cmp);
            s.median_diff = diffs[s.n / 2];
        }
        s
    }

    /// How many domain cells sit close enough to a threshold that the
    /// reference's own echo-top quantization could flip them — reported so
    /// the confusion counts can be read against the floor that produces them.
    pub fn quantization_ambiguous_cells(
        reference: &[Vec<f32>],
        eet_published_kft: &[Vec<f32>],
        threshold: f32,
    ) -> usize {
        let mut n = 0usize;
        for az in 0..360 {
            for r in 0..RANGE_BINS {
                let (Some(&f), Some(&kft)) = (
                    reference.get(az).and_then(|row| row.get(r)),
                    eet_published_kft.get(az).and_then(|row| row.get(r)),
                ) else {
                    continue;
                };
                if !f.is_finite() || !kft.is_finite() || kft <= 0.0 {
                    continue;
                }
                let half = reference_quantization_halfwidth_g_m3(f, kft);
                n += usize::from((f - threshold).abs() <= half);
            }
        }
        n
    }

    // ── The bars ────────────────────────────────────────────────────────────
    //
    // No prior bar exists for VIL density: it shipped "by construction". These
    // are pinned from what the product is *read* for — a hail-size
    // discriminator consulted at a threshold — and deliberately not from what
    // the first survey happened to score.

    /// **The bar.** Percent of the domain (cells either side defines) on
    /// which we and the reference agree which side of the threshold the cell
    /// sits. Asserted at both [`HAIL_RARE_BELOW_G_M3`] and
    /// [`HAIL_NEAR_CERTAIN_AT_G_M3`], per site.
    pub const THRESHOLD_AGREEMENT_MIN_PCT: f64 = 95.0;

    /// Ceiling on the domain share we flag severe-hail-likely where the
    /// reference does not. Over-warning is the specific failure mode a
    /// systematically low echo top predicts, so it gets its own leg rather
    /// than hiding inside the agreement figure.
    pub const FALSE_HIGH_MAX_PCT: f64 = 3.0;

    /// Ceiling on the false alarm ratio **of the flag itself** — the share of
    /// the area we flag that the reference does not.
    ///
    /// This leg exists because the two above are, on their own, weak: a
    /// convective volume's domain is mostly cells both sides put far below
    /// 3.5 g/m³, so agreement is ≥ 95% and false-high ≤ 3% almost by
    /// construction. FAR is scored over the flagged union only, where the
    /// decision actually happens, and 25% is chosen as a deliberately
    /// *generous* ceiling: a quarter of the flagged area spurious against a
    /// reference built from the same volume, on the same grid, by the same
    /// arithmetic, is already worse than any operational tolerance — so
    /// clearing it proves little and missing it is decisive.
    pub const FLAG_FAR_MAX_PCT: f64 = 25.0;

    /// Floor on the probability of detection of the flag — the share of the
    /// reference's flagged area we also flag. The symmetric partner of
    /// [`FLAG_FAR_MAX_PCT`], at the same 25% miss budget.
    ///
    /// **Added after the first run**, and the record says so plainly: FAR
    /// alone is gameable by silence. A derivation that flags *nothing*
    /// scores FAR 0%, false-high 0% and ≥ 99% threshold agreement — which is
    /// exactly what the 2026-07-29 run produced. A bar a mute product passes
    /// is not a bar. Adding this leg made the survey stricter, never looser,
    /// and the derivation is measured against it, not it against the
    /// derivation.
    pub const FLAG_POD_MIN_PCT: f64 = 75.0;

    /// Below this many cells in the flagged union, FAR/POD/CSI are noise: a
    /// handful of cells swings them tens of points. They are printed but not
    /// asserted **per site**; the run's pooled figure is asserted instead
    /// (see [`flag_sample_is_conclusive`]), because real convective volumes
    /// turn out to carry only tens of cells above 3.5 g/m³ each.
    pub const MIN_FLAGGED_CELLS: usize = 200;

    /// A run concludes nothing until this many site-hours were asserted…
    pub const MIN_SITES: usize = 4;

    /// …and this many domain cells were scored, pooled across them.
    pub const MIN_DEFINED_CELLS: usize = 10_000;

    /// Volumes whose twins define fewer bins than this are skipped, not
    /// scored — the same near-empty rule as every other harness. A skip is
    /// printed, never silent.
    pub const MIN_TWIN_DEFINED_BINS: usize = 500;

    pub fn meets_agreement_bar(agreement_pct: f64) -> bool {
        agreement_pct >= THRESHOLD_AGREEMENT_MIN_PCT
    }

    pub fn false_high_is_acceptable(false_high_pct: f64) -> bool {
        false_high_pct <= FALSE_HIGH_MAX_PCT
    }

    pub fn far_is_acceptable(far_pct: f64) -> bool {
        far_pct <= FLAG_FAR_MAX_PCT
    }

    pub fn pod_is_acceptable(pod_pct: f64) -> bool {
        pod_pct >= FLAG_POD_MIN_PCT
    }

    /// Whether a flagged population is big enough for [`far_is_acceptable`]
    /// and [`pod_is_acceptable`] to mean anything.
    pub fn far_is_asserted(flagged_union: usize) -> bool {
        flagged_union >= MIN_FLAGGED_CELLS
    }

    pub fn volume_is_scoreable(twin_defined_bins: usize) -> bool {
        twin_defined_bins >= MIN_TWIN_DEFINED_BINS
    }

    pub fn sample_is_conclusive(sites_asserted: usize, pooled_domain_cells: usize) -> bool {
        sites_asserted >= MIN_SITES && pooled_domain_cells >= MIN_DEFINED_CELLS
    }

    /// Whether a run says anything at all about the **decision** metric.
    ///
    /// [`sample_is_conclusive`] gates on the domain, which a quiet volume
    /// fills with tens of thousands of cells nobody would ever read the
    /// product for. The decision lives where the threshold is crossed, and a
    /// run whose reference never crossed it has measured the threshold
    /// agreement of a field that has no threshold to agree about. That is a
    /// *skip*, not a pass — this gate is what makes the harness say so.
    ///
    /// Gated on the cells the **reference** flags
    /// ([`ThresholdConfusion::reference_flagged`]), never the union: the
    /// union shrinks when we stay silent, so a mute derivation would
    /// otherwise shrink its own sample below the gate and be excused.
    pub fn flag_sample_is_conclusive(pooled_reference_flagged: usize) -> bool {
        pooled_reference_flagged >= MIN_FLAGGED_CELLS
    }

    /// The reference's own value distribution, for the record: how much of
    /// the field is anywhere near a decision at all. Counts of cells at or
    /// above each of `bands`, plus the field maximum.
    pub fn distribution(field: &[Vec<f32>], bands: &[f32]) -> (f32, Vec<usize>) {
        let mut max = f32::NEG_INFINITY;
        let mut counts = vec![0usize; bands.len()];
        for v in field.iter().flatten().filter(|v| v.is_finite()) {
            max = max.max(*v);
            for (i, &b) in bands.iter().enumerate() {
                counts[i] += usize::from(*v >= b);
            }
        }
        (if max.is_finite() { max } else { f32::NAN }, counts)
    }

    /// How much of a quarantined site stops being asserted on. VIL density is
    /// a volume product with no per-tilt figure, so the only scope is the
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
    /// run's miss is a lead, not a verdict. A quarantined site stays in the
    /// roster, stays measured and stays printed; only the assertion is
    /// withheld. Never widen the bar instead.
    pub const QUARANTINED: &[Quarantine] = &[];

    pub fn quarantine(site: &str) -> Option<&'static Quarantine> {
        QUARANTINED.iter().find(|q| q.site == site)
    }

    pub fn site_is_asserted(site: &str) -> bool {
        quarantine(site).is_none()
    }
}

/// Offline pins on the VIL-density validation policy: the reference
/// construction's arithmetic hand-computed, the echo-top quantization floor,
/// the threshold-confusion accounting, and the bars themselves.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod vild_policy_tests {
    use super::vild_validation_policy as policy;
    use crate::twin::compare::ValueCodec;
    use nexrad_level3::model::{RadialPacket, RadialRun};

    /// The reference is the RPG's two published products divided, on the
    /// echo-top bin **centre**, hand-computed:
    ///
    /// * DVL 35 kg/m² over a published 32 kft top → 32.5 kft = 9906.0 m, so
    ///   `35000/9906` = **3.533212 g/m³**;
    /// * the literal floor decode divides by 32.0 kft = 9753.6 m instead,
    ///   `35000/9753.6` = **3.588419**, high by 32.5/32 − 1 = **1.5625%**;
    /// * and the gap is decision-sized: 34.4 kg/m² over the same top reads
    ///   `34400/9906` = **3.472643** on the bin centre but `34400/9753.6` =
    ///   **3.526903** on the floor — opposite sides of Amburn & Wolf's 3.5
    ///   break, from the same two published numbers.
    #[test]
    fn the_reference_divides_the_rpgs_own_two_products() {
        let centre = policy::reference_vild(35.0, 32.0);
        assert!((centre - 3.533_212).abs() < 1e-5, "got {centre}");
        let floor = policy::reference_vild_on_published_floor(35.0, 32.0);
        assert!((floor - 3.588_419).abs() < 1e-5, "got {floor}");
        assert!(
            (f64::from(floor / centre) - 32.5 / 32.0).abs() < 1e-6,
            "the floor decode's bias is exactly half a bin",
        );

        let straddle_centre = policy::reference_vild(34.4, 32.0);
        let straddle_floor = policy::reference_vild_on_published_floor(34.4, 32.0);
        assert!(
            (straddle_centre - 3.472_643).abs() < 1e-5,
            "{straddle_centre}"
        );
        assert!(
            (straddle_floor - 3.526_903).abs() < 1e-5,
            "{straddle_floor}"
        );
        assert!(
            straddle_centre < 3.5 && straddle_floor >= 3.5,
            "the two data land both sides of the 3.5 break — the datum decides",
        );

        // The same arithmetic the shipped product applies to its own inputs.
        assert!((policy::reference_vild(20.0, 32.3084) - 2.0).abs() < 1e-5);

        // A defined 0.0 kg/m² column is a defined 0.0 g/m³, not undefined.
        assert_eq!(policy::reference_vild(0.0, 32.0), 0.0);

        // Undefined inputs, and the zero-top exclusion: product 135's level 2
        // (and the topped 130) decode to 0.0 kft, where no quotient exists.
        assert!(policy::reference_vild(35.0, 0.0).is_nan(), "a zero top");
        assert!(policy::reference_vild_on_published_floor(35.0, 0.0).is_nan());
        assert!(policy::reference_vild(35.0, f32::NAN).is_nan());
        assert!(policy::reference_vild(f32::NAN, 32.0).is_nan());
        assert!(policy::reference_vild(35.0, -1.0).is_nan());
    }

    /// The reference's own noise floor: ±0.5 kft of echo-top quantization is
    /// a relative VILD uncertainty of `0.5/(published + 0.5)`. The table in
    /// the policy module's doc, hand-computed at VILD 3.5 g/m³ — this is the
    /// reason no value-agreement bar is pinned.
    #[test]
    fn the_reference_noise_floor_is_half_a_kilofoot_of_echo_top() {
        for (published, halfwidth) in [
            (15.0f32, 0.112_903f32),
            (20.0, 0.085_366),
            (30.0, 0.057_377),
            (40.0, 0.043_210),
            (50.0, 0.034_653),
        ] {
            let got = policy::reference_quantization_halfwidth_g_m3(3.5, published);
            assert!(
                (got - halfwidth).abs() < 1e-5,
                "at {published} kft: got {got}, hand-computed {halfwidth}",
            );
            // Relative, so it scales with the value itself.
            let double = policy::reference_quantization_halfwidth_g_m3(7.0, published);
            assert!((double - 2.0 * halfwidth).abs() < 1e-5);
        }

        // The floor decode's bias is `value · 0.5/published` — a whole noise
        // floor wide (it exceeds the ±0.5 kft halfwidth by exactly the ratio
        // (published + 0.5)/published), so choosing the datum is not a
        // rounding detail: it is the difference between a reference centred
        // on the truth and one offset by its own uncertainty.
        let centre = policy::reference_vild(35.0, 30.0);
        let floor_bias = policy::reference_vild_on_published_floor(35.0, 30.0) - centre;
        let half = policy::reference_quantization_halfwidth_g_m3(centre, 30.0);
        assert!(
            (f64::from(floor_bias) - f64::from(centre) * 0.5 / 30.0).abs() < 1e-6,
            "floor bias {floor_bias}",
        );
        assert!(
            (f64::from(floor_bias / half) - 30.5 / 30.0).abs() < 1e-4,
            "{floor_bias} vs {half}",
        );
    }

    /// Cells the reference cannot resolve either side of a threshold: the
    /// count is over cells whose distance from the break is inside their own
    /// quantization halfwidth.
    #[test]
    fn quantization_ambiguous_cells_are_the_ones_inside_the_floor() {
        // halfwidths at a published 30 kft top: 0.05738 (3.5), 0.05656
        // (3.45), 0.04918 (3.0).
        let reference = vec![vec![3.5f32, 3.45, 3.0, f32::NAN]];
        let tops = vec![vec![30.0f32, 30.0, 30.0, 30.0]];
        assert_eq!(
            policy::quantization_ambiguous_cells(&reference, &tops, 3.5),
            2,
        );
        // A zero published top is not in the reference's domain at all.
        let zero_tops = vec![vec![0.0f32, 0.0, 0.0, 0.0]];
        assert_eq!(
            policy::quantization_ambiguous_cells(&reference, &zero_tops, 3.5),
            0,
        );
    }

    /// The published echo tops become the reference's denominator on bin
    /// centres, with zero and undefined tops dropped.
    #[test]
    fn published_tops_become_bin_centres_with_zero_dropped() {
        let field = policy::published_top_field(&[vec![0.0, 32.0, f32::NAN, -1.0, 69.0]]);
        assert!(field[0][0].is_nan(), "a 0 kft top has no usable quotient");
        assert_eq!(field[0][1], 32.5);
        assert!(field[0][2].is_nan());
        assert!(field[0][3].is_nan());
        assert_eq!(field[0][4], 69.5);
    }

    /// The threshold confusion accounts every cell of the domain exactly
    /// once, counts an undefined side as "not flagged", and takes the
    /// threshold **inclusively** (Amburn & Wolf's "at 4.0 and above").
    ///
    /// Ours `[5.0, 1.15, 4.0, NaN, 3.6]` against the reference
    /// `[6.0, 1.2, 2.0, 7.0, NaN]`:
    ///
    /// * at 3.5 — hit, agreed-below, false-high, false-low, false-high;
    /// * at 4.0 — the 4.0 cell still flags (inclusive) and the 3.6 cell stops
    ///   flagging, so one false-high becomes an agreed-below.
    #[test]
    fn the_threshold_confusion_accounts_every_domain_cell_once() {
        let ours = vec![vec![5.0f32, 1.15, 4.0, f32::NAN, 3.6]];
        let reference = vec![vec![6.0f32, 1.2, 2.0, 7.0, f32::NAN]];

        let low = policy::confuse(&ours, &reference, policy::HAIL_RARE_BELOW_G_M3);
        assert_eq!(low.hits, 1);
        assert_eq!(low.false_high, 2, "one over-value, one over-presence");
        assert_eq!(low.false_low, 1);
        assert_eq!(low.agreed_below, 1);
        assert_eq!(low.domain(), 5, "cells undefined on both sides are not in");
        assert!((low.agreement_pct() - 40.0).abs() < 1e-9);
        assert!((low.false_high_pct() - 40.0).abs() < 1e-9);
        assert!((low.false_low_pct() - 20.0).abs() < 1e-9);
        assert_eq!(low.flagged_union(), 4);
        assert!((low.far_pct() - 100.0 * 2.0 / 3.0).abs() < 1e-9);
        assert!((low.pod_pct() - 50.0).abs() < 1e-9);
        assert!((low.csi_pct() - 25.0).abs() < 1e-9);

        let high = policy::confuse(&ours, &reference, policy::HAIL_NEAR_CERTAIN_AT_G_M3);
        assert_eq!(high.hits, 1, "4.0 ≥ 4.0 — the break is inclusive");
        assert_eq!(high.false_high, 1);
        assert_eq!(high.false_low, 1);
        assert_eq!(high.agreed_below, 2);
        assert_eq!(high.domain(), 5, "the domain does not move with the break");

        // A cell undefined on both sides is out of the domain entirely, and
        // an empty comparison divides by nothing.
        let empty = policy::confuse(&[vec![f32::NAN]], &[vec![f32::NAN]], 3.5);
        assert_eq!(empty, policy::ThresholdConfusion::default());
        assert_eq!(empty.domain(), 0);
        assert_eq!(empty.agreement_pct(), 0.0);
        assert_eq!(empty.far_pct(), 0.0);

        // Pooling adds cell for cell — the run's decision figures are only
        // meaningful over the whole survey.
        let mut sum = low;
        sum.merge(&high);
        assert_eq!(sum.hits, low.hits + high.hits);
        assert_eq!(sum.false_high, low.false_high + high.false_high);
        assert_eq!(sum.false_low, low.false_low + high.false_low);
        assert_eq!(sum.agreed_below, low.agreed_below + high.agreed_below);
        assert_eq!(sum.domain(), low.domain() + high.domain());
    }

    /// A derivation that flags **nothing** is the one thing a FAR-only skill
    /// bar cannot catch: it scores perfect agreement, zero false-high and
    /// zero FAR while missing every core the reference flags. This is the
    /// pin that says why [`policy::FLAG_POD_MIN_PCT`] exists.
    #[test]
    fn a_mute_derivation_passes_every_bar_except_the_pod_leg() {
        let mute = vec![vec![0.1f32; 100]];
        let mut reference = vec![vec![0.1f32; 100]];
        for cell in reference[0].iter_mut().take(2) {
            *cell = 9.0;
        }
        let c = policy::confuse(&mute, &reference, policy::HAIL_RARE_BELOW_G_M3);
        assert_eq!(c.hits, 0);
        assert_eq!(c.false_high, 0);
        assert_eq!(c.false_low, 2);
        assert!(policy::meets_agreement_bar(c.agreement_pct()), "98% agrees");
        assert!(policy::false_high_is_acceptable(c.false_high_pct()));
        assert!(policy::far_is_acceptable(c.far_pct()), "FAR 0 by silence");
        assert!(
            !policy::pod_is_acceptable(c.pod_pct()),
            "POD 0% — the only leg a mute derivation cannot pass",
        );
    }

    /// The run says nothing about the decision unless enough cells actually
    /// crossed the threshold, and the distribution is what shows whether any
    /// did.
    #[test]
    fn a_run_that_never_crosses_the_threshold_is_inconclusive_not_passing() {
        assert!(!policy::flag_sample_is_conclusive(0));
        assert!(!policy::flag_sample_is_conclusive(199));
        assert!(policy::flag_sample_is_conclusive(200));

        // The gate counts the reference's flags, not the union: a mute
        // derivation must not be able to shrink its own sample out of
        // judgement. 250 reference-flagged cells stay conclusive however few
        // of them we hit.
        let mute = policy::ThresholdConfusion {
            hits: 0,
            false_high: 0,
            false_low: 250,
            agreed_below: 1_000,
        };
        assert_eq!(mute.reference_flagged(), 250);
        assert_eq!(mute.flagged_union(), 250);
        assert!(policy::flag_sample_is_conclusive(mute.reference_flagged()));
        let half_mute = policy::ThresholdConfusion {
            hits: 100,
            false_high: 0,
            false_low: 150,
            agreed_below: 1_000,
        };
        assert_eq!(half_mute.reference_flagged(), 250);
        assert!(!policy::pod_is_acceptable(half_mute.pod_pct()), "POD 40%");

        let (max, bands) = policy::distribution(
            &[vec![0.5f32, 2.5, 3.7, 4.2, f32::NAN]],
            &[1.0, 2.0, 3.0, 3.5, 4.0],
        );
        assert!((max - 4.2).abs() < 1e-6);
        assert_eq!(bands, vec![3, 3, 2, 2, 1]);

        let (empty_max, empty_bands) = policy::distribution(&[vec![f32::NAN]], &[1.0]);
        assert!(empty_max.is_nan(), "an all-undefined field has no maximum");
        assert_eq!(empty_bands, vec![0]);
    }

    /// The signed statistics run over the cells **both** sides define, and
    /// the reference floor restricts them to the decision region.
    ///
    /// Diffs (ours − reference) are −1.0, −0.05, +2.0: mean +0.316667, median
    /// −0.05 (the middle of three), MAE 1.016667, one cell within ±0.5 g/m³
    /// and one within ±15% (0.05 ≤ 0.18). Restricted to reference ≥ 3.0 only
    /// the 6.0 cell survives.
    #[test]
    fn value_stats_report_signed_bias_over_the_cells_both_sides_define() {
        let ours = vec![vec![5.0f32, 1.15, 4.0, f32::NAN, 3.6]];
        let reference = vec![vec![6.0f32, 1.2, 2.0, 7.0, f32::NAN]];

        let all = policy::value_stats(&ours, &reference, 0.0);
        assert_eq!(all.n, 3);
        assert!((all.mean_diff - 0.316_666_666).abs() < 1e-6, "{all:?}");
        assert!((all.median_diff - -0.05).abs() < 1e-6);
        assert!((all.mae - 1.016_666_666).abs() < 1e-6);
        assert_eq!(all.within_abs, 1);
        assert_eq!(all.within_rel, 1);
        assert!((all.within_abs_pct() - 100.0 / 3.0).abs() < 1e-9);
        assert!((all.within_rel_pct() - 100.0 / 3.0).abs() < 1e-9);

        let decision = policy::value_stats(&ours, &reference, policy::DECISION_REGION_MIN_G_M3);
        assert_eq!(decision.n, 1, "only the 6.0 g/m³ cell is in the region");
        assert!((decision.mean_diff - -1.0).abs() < 1e-9);
        assert!((decision.median_diff - -1.0).abs() < 1e-9);
        assert!((decision.mae - 1.0).abs() < 1e-9);

        let none = policy::value_stats(&[vec![f32::NAN]], &[vec![1.0]], 0.0);
        assert_eq!(none.n, 0);
        assert_eq!(none.mean_diff, 0.0);
        assert_eq!(none.within_abs_pct(), 0.0);
    }

    /// The harness scores the **shipped** product because its constructors
    /// *are* the shipped product's: this is the pin that they have not been
    /// forked back apart into a private copy, which is how a survey comes to
    /// bless a field the app does not draw.
    ///
    /// The composition — that [`crate::vild::compute_vild`] applies these in
    /// this order, at each product's own gate spacing — is pinned by
    /// `vild::tests::the_shipped_path_is_the_surveys_reference_construction`,
    /// which has the synthetic message pairs to do it with.
    #[test]
    fn the_harness_scores_the_shipped_vil_density_product() {
        for &dvl in &[0.0f32, 0.011, 3.7, 34.4, 35.0, 62.0, 200.0, f32::NAN] {
            for &kft in &[0.0f32, -1.0, 1.0, 15.0, 32.0, 69.0, f32::NAN] {
                let harness = policy::reference_vild(dvl, kft);
                let shipped = crate::vild::vild_from_published(dvl, kft);
                assert!(
                    (harness.is_nan() && shipped.is_nan())
                        || harness.to_bits() == shipped.to_bits(),
                    "DVL {dvl} over {kft} kft: harness {harness}, shipped {shipped} — the \
                     harness's reference has been forked from the shipped product",
                );
            }
        }

        // And the two field constructors, on a field carrying every category:
        // values either side of both breaks, a defined zero, one-sided cells,
        // a zero top and an undefined top.
        let dvl = vec![vec![35.0f32, 34.0, 0.0, 20.0, f32::NAN, 35.0, 35.0]];
        let tops = vec![vec![32.0f32, 32.0, 32.0, f32::NAN, 40.0, 0.0, -1.0]];
        let harness = policy::density_field(&dvl, &policy::published_top_field(&tops));
        let shipped = crate::vild::density_field(&dvl, &crate::vild::published_top_field(&tops));
        assert_eq!(harness.len(), shipped.len());
        for (az, (a, b)) in harness.iter().zip(&shipped).enumerate() {
            for (r, (&x, &y)) in a.iter().zip(b).enumerate() {
                assert!(
                    (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits(),
                    "az {az} r {r}: harness {x}, shipped {y}",
                );
            }
        }
        assert!(
            harness[0][..3].iter().all(|v| v.is_finite()),
            "the comparison must cover defined cells, not only NaNs",
        );
    }

    /// The resampler feeds the reference physical values through the
    /// product's own codec, on the 360 × 230 grid, `NaN` where the packet has
    /// no gate.
    #[test]
    fn the_reference_fields_decode_through_each_products_own_codec() {
        let packet = RadialPacket {
            first_range_bin: 0,
            num_range_bins: 3,
            i_center: 0,
            j_center: 0,
            scale_factor: 1.0,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials: (0..360)
                .map(|i| RadialRun {
                    start_angle: i as f32,
                    angle_delta: 1.0,
                    gate_values: vec![0, 2, 3],
                })
                .collect(),
        };
        // A product-135-shaped LUT: level 2 is 0 kft, level 3 is 1 kft.
        let mut lut = vec![f32::NAN; 256];
        lut[2] = 0.0;
        lut[3] = 1.0;
        let field = policy::resampled_field(&packet, 1.0, &ValueCodec::Lut(lut));
        assert_eq!(field.len(), 360);
        assert_eq!(field[0].len(), crate::volumetric::RANGE_BINS);
        assert!(field[0][0].is_nan(), "level 0 is below threshold");
        assert_eq!(field[0][1], 0.0, "a defined zero, not undefined");
        assert_eq!(field[0][2], 1.0);
        assert!(field[0][3].is_nan(), "past the packet's own range extent");
        assert!(field[0][229].is_nan(), "and the domain stops at 230 km");
    }

    /// The campaign's bars, pinned so the ignored harness cannot drift them.
    #[test]
    fn the_vild_acceptance_bars_are_what_the_campaign_set() {
        assert_eq!(policy::HAIL_RARE_BELOW_G_M3, 3.5);
        assert_eq!(policy::HAIL_NEAR_CERTAIN_AT_G_M3, 4.0);
        assert_eq!(policy::THRESHOLD_AGREEMENT_MIN_PCT, 95.0);
        assert_eq!(policy::FALSE_HIGH_MAX_PCT, 3.0);
        assert_eq!(policy::FLAG_FAR_MAX_PCT, 25.0);
        assert_eq!(policy::FLAG_POD_MIN_PCT, 75.0);
        assert_eq!(policy::MIN_FLAGGED_CELLS, 200);
        assert_eq!(policy::MIN_SITES, 4);
        assert_eq!(policy::MIN_DEFINED_CELLS, 10_000);
        assert_eq!(policy::MIN_TWIN_DEFINED_BINS, 500);
        assert_eq!(policy::EET_QUANTUM_KFT, 1.0);
        assert_eq!(policy::EET_BIN_CENTRE_KFT, 0.5);
        assert_eq!(policy::VALUE_TOLERANCE_G_M3, 0.5);
        assert_eq!(policy::VALUE_TOLERANCE_FRACTION, 0.15);
        assert_eq!(policy::DECISION_REGION_MIN_G_M3, 3.0);

        assert!(policy::meets_agreement_bar(95.0), "the bar is inclusive");
        assert!(!policy::meets_agreement_bar(94.999_999));
        assert!(policy::false_high_is_acceptable(3.0));
        assert!(!policy::false_high_is_acceptable(3.000_001));
        assert!(policy::far_is_acceptable(25.0));
        assert!(!policy::far_is_acceptable(25.000_001));
        assert!(policy::pod_is_acceptable(75.0), "the bar is inclusive");
        assert!(!policy::pod_is_acceptable(74.999_999));
        assert!(policy::far_is_asserted(200));
        assert!(
            !policy::far_is_asserted(199),
            "too few flagged cells to say"
        );
    }

    /// The conclusiveness fold and the near-empty-volume skip.
    #[test]
    fn a_vild_run_is_conclusive_only_with_enough_sites_and_cells() {
        assert!(policy::sample_is_conclusive(4, 10_000));
        assert!(!policy::sample_is_conclusive(3, 1_000_000), "sites gate");
        assert!(!policy::sample_is_conclusive(20, 9_999), "cells gate");
        assert!(!policy::sample_is_conclusive(0, 0));

        assert!(!policy::volume_is_scoreable(0));
        assert!(!policy::volume_is_scoreable(499));
        assert!(policy::volume_is_scoreable(500));
    }

    /// The quarantine table starts empty, and any entry ever added must name
    /// a site the harness still measures.
    #[test]
    fn the_vild_quarantine_table_is_empty_and_would_stay_measured() {
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
        assert_eq!(format!("{:?}", policy::Scope::Whole), "Whole");
    }
}

/// The live VIL-density harness: score the shipped
/// [`crate::vild::compute_vild`] against a reference built from the RPG's
/// **own two inputs** for the same volume — product 134 (`DVL`) over product
/// 135 (`EET`) — at Amburn & Wolf's decision thresholds.
///
/// VIL density has no Level III twin, so this is the only oracle there is.
/// See [`vild_validation_policy`] for the reference's construction and its
/// quantization noise floor.
///
/// Since the product **is** that reference, the PRIMARY row is perfect by
/// construction and its job is to prove it (a miss means the shipped path and
/// the reference have drifted — start at
/// `vild::tests::the_shipped_path_is_the_surveys_reference_construction`). The
/// rows that still measure something are the two attribution mixes and the
/// retired local quotient, which record how far the local derivations sat from
/// the RPG's own products.
///
/// Site-hours come from `VILD_SITE_HOURS` (`SITE=YYYY-MM-DDTHH:MM`, comma or
/// semicolon separated, a site may appear more than once); unset, the roster
/// at now — the clear-air fallback, which proves nothing about a hail
/// product. Pick precipitating hours with the shared reconnaissance scan:
///
/// ```text
/// HCA_SCAN_HOURS=2026-07-29T06:00,2026-07-28T21:00 \
///   cargo test -p rustdar-radar --release --lib -- --ignored \
///   --nocapture live_hca_precip_site_scan
///
/// VILD_SITE_HOURS=KUEX=2026-07-29T06:00,KMLB=2026-07-28T21:00 \
///   cargo test -p rustdar-radar --release --lib -- --ignored \
///   --nocapture live_derived_vild
/// ```
#[cfg(all(test, not(target_arch = "wasm32")))]
mod live_vild_validation {
    use super::vild_validation_policy as policy;
    use crate::sources::DataSources;
    use crate::twin::{compare, live};

    /// The survey's site-hours; unset, the full roster at now.
    fn site_hours() -> Vec<(String, chrono::NaiveDateTime)> {
        let now = chrono::Utc::now().naive_utc();
        match std::env::var("VILD_SITE_HOURS") {
            Ok(spec) if !spec.trim().is_empty() => spec
                .split([',', ';'])
                .filter_map(|pair| {
                    let (site, when) = pair.trim().split_once('=')?;
                    let when = chrono::NaiveDateTime::parse_from_str(when.trim(), "%Y-%m-%dT%H:%M")
                        .unwrap_or_else(|e| panic!("bad VILD_SITE_HOURS entry {pair}: {e}"));
                    Some((site.trim().to_uppercase(), when))
                })
                .collect(),
            _ => live::SITES.iter().map(|s| (s.to_string(), now)).collect(),
        }
    }

    /// The rows every site-hour is scored on, pooled in this order: the
    /// shipped product, the two attribution mixes (one input swapped for the
    /// retired local derivation at a time), the fully local quotient the
    /// product used to be, and the reference-datum A/B.
    const ROW_LABELS: [&str; 5] = [
        "PRIMARY shipped L3",
        "attr our-VIL/RPG-top",
        "attr RPG-DVL/our-top",
        "retired local/local",
        "ab  ref-on-floor-top",
    ];
    const ROWS: usize = ROW_LABELS.len();

    /// One scored row of the survey: a candidate VIL-density field against
    /// the reference, at both breaks. Returns the two confusions so the run
    /// can pool them — a single volume carries only tens of flagged cells.
    fn print_row(
        label: &str,
        field: &[Vec<f32>],
        reference: &[Vec<f32>],
    ) -> [policy::ThresholdConfusion; 2] {
        let mut out = [policy::ThresholdConfusion::default(); 2];
        for (i, threshold) in [
            policy::HAIL_RARE_BELOW_G_M3,
            policy::HAIL_NEAR_CERTAIN_AT_G_M3,
        ]
        .into_iter()
        .enumerate()
        {
            let c = policy::confuse(field, reference, threshold);
            out[i] = c;
            println!(
                "    {label:24} @{threshold:>3} | domain {:>7} agree {:>6.2}% \
                 false-high {:>5} ({:.2}%) false-low {:>5} ({:.2}%) \
                 | flagged {:>6} FAR {:>6.2}% POD {:>6.2}% CSI {:>6.2}%",
                c.domain(),
                c.agreement_pct(),
                c.false_high,
                c.false_high_pct(),
                c.false_low,
                c.false_low_pct(),
                c.flagged_union(),
                c.far_pct(),
                c.pod_pct(),
                c.csi_pct(),
            );
        }
        let all = policy::value_stats(field, reference, 0.0);
        let decision = policy::value_stats(field, reference, policy::DECISION_REGION_MIN_G_M3);
        println!(
            "    {label:24} value    | n {:>7} mean {:+.4} median {:+.4} MAE {:.4} \
             ±0.5 {:.2}% ±15% {:.2}% || decision-region n {:>6} mean {:+.4} median {:+.4} \
             MAE {:.4}",
            all.n,
            all.mean_diff,
            all.median_diff,
            all.mae,
            all.within_abs_pct(),
            all.within_rel_pct(),
            decision.n,
            decision.mean_diff,
            decision.median_diff,
            decision.mae,
        );
        out
    }

    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_derived_vild_matches_the_rpgs_own_inputs() {
        crate::tls::init();
        let sources = DataSources::production();

        let mut asserted_sites = 0usize;
        let mut pooled_domain = 0usize;
        let mut pooled = [policy::ThresholdConfusion::default(); 2];
        let mut pooled_rows = [[policy::ThresholdConfusion::default(); 2]; ROWS];
        let mut pooled_ambiguous = [0usize; 2];
        let mut failures: Vec<String> = Vec::new();

        for (site, when) in site_hours() {
            let site = site.as_str();
            let Some((scan, l2_start)) = live::l2_volume_near(site, when).await else {
                println!("{site}: SKIP — no archived Level II volume near {when}");
                continue;
            };
            let Some(dvl) = live::l3_twin(&sources, site, "DVL", l2_start, None).await else {
                println!("{site}: SKIP — no DVL twin names volume {l2_start}");
                continue;
            };
            let Some(eet) = live::l3_twin(&sources, site, "EET", l2_start, None).await else {
                println!("{site}: SKIP — no EET twin names volume {l2_start}");
                continue;
            };
            if dvl.message.pdb.product_code != 134 || eet.message.pdb.product_code != 135 {
                println!(
                    "{site}: SKIP — twins decode as products {} / {}",
                    dvl.message.pdb.product_code, eet.message.pdb.product_code,
                );
                continue;
            }
            let (Some(dvl_packet), Some(eet_packet)) = (
                crate::srm::radial_packet(&dvl.message),
                crate::srm::radial_packet(&eet.message),
            ) else {
                println!("{site}: SKIP — a twin carries no radial packet");
                continue;
            };
            let (Some(dvl_codec), Some(eet_codec)) = (
                compare::ValueCodec::for_message(&dvl.message),
                compare::ValueCodec::for_message(&eet.message),
            ) else {
                println!("{site}: SKIP — a twin has no codec");
                continue;
            };

            let dvl_defined = super::validation_policy::twin_defined_bins(dvl_packet, &dvl_codec);
            let eet_defined = super::validation_policy::twin_defined_bins(eet_packet, &eet_codec);
            if !policy::volume_is_scoreable(dvl_defined)
                || !policy::volume_is_scoreable(eet_defined)
            {
                println!(
                    "{site}: SKIP — near-empty twins (DVL {dvl_defined} / EET {eet_defined} \
                     defined bins < {})",
                    policy::MIN_TWIN_DEFINED_BINS,
                );
                continue;
            }

            // The two published fields on our own 360° × 230 km grid, and the
            // reference they make: DVL over the EET bin centre.
            let dvl_field = policy::resampled_field(
                dvl_packet,
                compare::gate_km(&dvl.message.pdb, dvl_packet),
                &dvl_codec,
            );
            let eet_field = policy::resampled_field(
                eet_packet,
                compare::gate_km(&eet.message.pdb, eet_packet),
                &eet_codec,
            );
            let rpg_tops = policy::published_top_field(&eet_field);
            let reference = policy::density_field(&dvl_field, &rpg_tops);
            let reference_floor: Vec<Vec<f32>> = (0..360)
                .map(|az| {
                    (0..crate::volumetric::RANGE_BINS)
                        .map(|r| {
                            policy::reference_vild_on_published_floor(
                                dvl_field[az][r],
                                eet_field[az][r],
                            )
                        })
                        .collect()
                })
                .collect();

            // The shipped product, straight through its own entry point —
            // *not* rebuilt from the fields above, so this row measures what
            // the app draws, product-code checks and volume pairing included.
            // It is the reference by construction, and the run's job is to
            // prove that it still is.
            let ours = match crate::vild::compute_vild(&dvl.message, &eet.message) {
                Ok(grid) => grid.values,
                Err(refusal) => {
                    println!("{site}: SKIP — the shipped path refused: {refusal:?}");
                    continue;
                }
            };

            // The two attribution mixes — one input swapped for the retired
            // local derivation at a time — and the retired quotient itself, so
            // the residual it had can still be pinned on VIL or on the echo
            // top.
            let radar_height_ft = f64::from(eet.message.pdb.height);
            let our_vil = super::compute_vil(&scan);
            let our_eet = crate::eet::compute_eet(&scan, radar_height_ft);
            let our_vil_rpg_top = policy::density_field(&our_vil.values, &rpg_tops);
            let rpg_vil_our_top = policy::density_field(&dvl_field, &our_eet.values);
            let local_only = policy::density_field(&our_vil.values, &our_eet.values);

            let hot = scan
                .sweeps()
                .first()
                .map(|s| {
                    s.radials()
                        .iter()
                        .filter_map(|r| r.reflectivity())
                        .flat_map(|m| m.values())
                        .filter(
                            |v| matches!(v, nexrad_model::data::MomentValue::Value(x) if *x >= 35.0),
                        )
                        .count()
                })
                .unwrap_or(0);
            let ref_defined = reference.iter().flatten().filter(|v| v.is_finite()).count();
            let our_defined = ours.iter().flatten().filter(|v| v.is_finite()).count();
            const BANDS: &[f32] = &[1.0, 2.0, 3.0, 3.5, 4.0];
            let (ref_max, ref_bands) = policy::distribution(&reference, BANDS);
            let (our_max, our_bands) = policy::distribution(&ours, BANDS);
            println!(
                "{site} {when}: vol {l2_start} VCP {} | DVL {} ({dvl_defined} bins) \
                 EET {} ({eet_defined} bins) | {hot} gates ≥ 35 dBZ lowest cut | \
                 reference cells {ref_defined}, ours {our_defined} | ambiguous within the \
                 quantization floor: {} @3.5, {} @4.0",
                eet.message.pdb.vcp,
                dvl.stamp.key,
                eet.stamp.key,
                policy::quantization_ambiguous_cells(
                    &reference,
                    &eet_field,
                    policy::HAIL_RARE_BELOW_G_M3
                ),
                policy::quantization_ambiguous_cells(
                    &reference,
                    &eet_field,
                    policy::HAIL_NEAR_CERTAIN_AT_G_M3
                ),
            );
            println!(
                "    distribution (cells ≥ 1/2/3/3.5/4 g/m³) | reference max {ref_max:.2} \
                 {ref_bands:?} | ours max {our_max:.2} {our_bands:?}",
            );

            for (i, row) in [
                print_row(ROW_LABELS[0], &ours, &reference),
                print_row(ROW_LABELS[1], &our_vil_rpg_top, &reference),
                print_row(ROW_LABELS[2], &rpg_vil_our_top, &reference),
                print_row(ROW_LABELS[3], &local_only, &reference),
                print_row(ROW_LABELS[4], &ours, &reference_floor),
            ]
            .into_iter()
            .enumerate()
            {
                pooled_rows[i][0].merge(&row[0]);
                pooled_rows[i][1].merge(&row[1]);
            }
            pooled_ambiguous[0] += policy::quantization_ambiguous_cells(
                &reference,
                &eet_field,
                policy::HAIL_RARE_BELOW_G_M3,
            );
            pooled_ambiguous[1] += policy::quantization_ambiguous_cells(
                &reference,
                &eet_field,
                policy::HAIL_NEAR_CERTAIN_AT_G_M3,
            );

            if !policy::site_is_asserted(site) {
                println!("{site}: measured but quarantined — not asserted");
                continue;
            }

            let mut misses = Vec::new();
            let mut domain = 0usize;
            for (i, threshold) in [
                policy::HAIL_RARE_BELOW_G_M3,
                policy::HAIL_NEAR_CERTAIN_AT_G_M3,
            ]
            .into_iter()
            .enumerate()
            {
                let c = policy::confuse(&ours, &reference, threshold);
                domain = domain.max(c.domain());
                pooled[i].merge(&c);
                if !policy::meets_agreement_bar(c.agreement_pct()) {
                    misses.push(format!(
                        "@{threshold} agreement {:.2}% < {}%",
                        c.agreement_pct(),
                        policy::THRESHOLD_AGREEMENT_MIN_PCT,
                    ));
                }
                if !policy::false_high_is_acceptable(c.false_high_pct()) {
                    misses.push(format!(
                        "@{threshold} false-high {:.2}% > {}%",
                        c.false_high_pct(),
                        policy::FALSE_HIGH_MAX_PCT,
                    ));
                }
                if policy::far_is_asserted(c.flagged_union()) {
                    if !policy::far_is_acceptable(c.far_pct()) {
                        misses.push(format!(
                            "@{threshold} flag FAR {:.2}% > {}% (over {} flagged cells)",
                            c.far_pct(),
                            policy::FLAG_FAR_MAX_PCT,
                            c.flagged_union(),
                        ));
                    }
                    if !policy::pod_is_acceptable(c.pod_pct()) {
                        misses.push(format!(
                            "@{threshold} flag POD {:.2}% < {}% (over {} flagged cells)",
                            c.pod_pct(),
                            policy::FLAG_POD_MIN_PCT,
                            c.flagged_union(),
                        ));
                    }
                }
            }
            if !misses.is_empty() {
                failures.push(format!("{site} ({l2_start}): {}", misses.join("; ")));
            }
            asserted_sites += 1;
            pooled_domain += domain;
        }

        // The decision figures are only meaningful pooled: a convective
        // volume carries tens of cells above 3.5 g/m³, not hundreds.
        for (i, threshold) in [
            policy::HAIL_RARE_BELOW_G_M3,
            policy::HAIL_NEAR_CERTAIN_AT_G_M3,
        ]
        .into_iter()
        .enumerate()
        {
            let c = pooled[i];
            println!(
                "POOLED @{threshold} | domain {} agree {:.2}% false-high {} ({:.2}%) \
                 false-low {} ({:.2}%) | flagged {} hits {} FAR {:.2}% POD {:.2}% CSI {:.2}%",
                c.domain(),
                c.agreement_pct(),
                c.false_high,
                c.false_high_pct(),
                c.false_low,
                c.false_low_pct(),
                c.flagged_union(),
                c.hits,
                c.far_pct(),
                c.pod_pct(),
                c.csi_pct(),
            );
        }
        // The input attribution, pooled: which of the two retired local inputs
        // the old product's residual belonged to. Row 1 swaps our echo top out
        // for the RPG's (so what is left is VIL's), row 2 swaps our VIL out for
        // the RPG's (so what is left is the echo top's), row 3 is the retired
        // product itself.
        for (label, row) in ROW_LABELS.into_iter().zip(pooled_rows) {
            for (i, threshold) in [
                policy::HAIL_RARE_BELOW_G_M3,
                policy::HAIL_NEAR_CERTAIN_AT_G_M3,
            ]
            .into_iter()
            .enumerate()
            {
                let c = row[i];
                println!(
                    "POOLED {label:24} @{threshold:>3} | flagged {:>5} hits {:>5} \
                     false-high {:>5} false-low {:>5} | FAR {:>6.2}% POD {:>6.2}% CSI {:>6.2}%",
                    c.flagged_union(),
                    c.hits,
                    c.false_high,
                    c.false_low,
                    c.far_pct(),
                    c.pod_pct(),
                    c.csi_pct(),
                );
            }
        }
        println!(
            "asserted {asserted_sites} site-hours, {pooled_domain} domain cells pooled; \
             reference cells inside its own quantization floor of a break: {} @3.5, {} @4.0; \
             failures: {}",
            pooled_ambiguous[0],
            pooled_ambiguous[1],
            failures.len(),
        );

        let mut pooled_misses = Vec::new();
        for (i, threshold) in [
            policy::HAIL_RARE_BELOW_G_M3,
            policy::HAIL_NEAR_CERTAIN_AT_G_M3,
        ]
        .into_iter()
        .enumerate()
        {
            let c = pooled[i];
            if !policy::flag_sample_is_conclusive(c.reference_flagged()) {
                pooled_misses.push(format!(
                    "@{threshold} INCONCLUSIVE: the reference flagged {} cells pooled < {} — the \
                     sample barely crossed the threshold, so its agreement measures nothing \
                     (we flagged {}, POD {:.2}%, FAR {:.2}%)",
                    c.reference_flagged(),
                    policy::MIN_FLAGGED_CELLS,
                    c.hits + c.false_high,
                    c.pod_pct(),
                    c.far_pct(),
                ));
                continue;
            }
            if !policy::far_is_acceptable(c.far_pct()) {
                pooled_misses.push(format!(
                    "@{threshold} pooled flag FAR {:.2}% > {}%",
                    c.far_pct(),
                    policy::FLAG_FAR_MAX_PCT,
                ));
            }
            if !policy::pod_is_acceptable(c.pod_pct()) {
                pooled_misses.push(format!(
                    "@{threshold} pooled flag POD {:.2}% < {}%",
                    c.pod_pct(),
                    policy::FLAG_POD_MIN_PCT,
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "site-hours under the bar:\n  {}",
            failures.join("\n  "),
        );
        assert!(
            policy::sample_is_conclusive(asserted_sites, pooled_domain),
            "inconclusive run: {asserted_sites} site-hours / {pooled_domain} cells asserted, \
             need ≥{} and ≥{} — re-run on precipitating site-hours",
            policy::MIN_SITES,
            policy::MIN_DEFINED_CELLS,
        );
        assert!(
            pooled_misses.is_empty(),
            "the run's decision metric:\n  {}",
            pooled_misses.join("\n  "),
        );
    }
}
