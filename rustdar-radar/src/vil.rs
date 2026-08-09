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
//!
//! ## What the live harness asserts, after the 2026-07-29 restructure
//!
//! That re-run measured perfect figures it could not certify: every leg it
//! asserted passed, and the one verdict that would have meant something was
//! withheld, because 33 reference-flagged cells is not a sample and no amount of
//! re-running fixes weather. So [`live_vild_validation`] is now built around
//! what a live pair actually proves:
//!
//! * **Identity, asserted on every run** — the shipped
//!   [`crate::vild::compute_vild`] against the reference built from the same two
//!   messages, bit for bit over every defined cell, with a coverage floor
//!   ([`vild_validation_policy::MIN_IDENTITY_CELLS`]) so an empty grid cannot
//!   pass quietly. The constructors are shared, so this is *not* the
//!   arithmetic: it is codec selection off the real PDBs (134's hybrid LUT,
//!   135's mask/scale/offset and topped flag), resampling across both products'
//!   real gate spacings and range extents, the composition order and the `+0.5`
//!   bin-centre datum — the whole pipeline, on a sample any volume supplies
//!   (tens of thousands of cells; KUEX alone carried 39,004) with no hail
//!   needed. This is the primary evidence now, and a drifted shipped path fails
//!   here on the first volume rather than waiting for a hail day.
//! * **The pipeline's invariants, asserted on every run** — both PDBs naming one
//!   volume scan inside [`crate::vild::VOLUME_PAIRING_TOLERANCE_SECS`], defined
//!   cells finite and non-negative, nothing past
//!   [`vild_validation_policy::PLAUSIBLE_MAX_G_M3`], which is set at roughly
//!   four times the largest value the RPG has been recorded producing (KDDC's
//!   12.06 g/m³, KEAX's 12.80) rather than at what we expect of it.
//! * **The threshold and skill figures, reported on every run and asserted only
//!   where the sample is conclusive** — at the breaks whose reference-flagged
//!   population clears [`vild_validation_policy::MIN_FLAGGED_CELLS`]. Elsewhere
//!   the run prints them under an explicit "INCONCLUSIVE SAMPLE — SKILL NOT
//!   ASSERTED" line and stands on the two legs above. The gate was not lowered
//!   and the POD/FAR legs were not touched: they are the trap for the next
//!   implementation that only *approximates* the reference, and lowering a
//!   sample floor is how a mute product gets certified by silence.
//!
//! The conclusive skill measurement therefore lives on an archived
//! severe-weather day instead of on today's sky:
//! `live_vild_validation::live_derived_vild_on_the_2022_05_04_outbreak`, the
//! 2022-05-04 Oklahoma outbreak at 21:30Z and 23:30Z over KTLX, KINX, KVNX and
//! KSRX — the same site-hours the hail campaign reached 102 hail cells on. That
//! run **asserts** the 200-cell gate rather than reporting it.
//!
//! ## The conclusive run: 2022-05-04 outbreak, measured 2026-07-30
//!
//! Eight archived site-hours, 163,301 domain cells pooled, **306**
//! reference-flagged cells at 3.5 g/m³ and **211** at 4.0. The first VILD run to
//! clear [`vild_validation_policy::MIN_FLAGGED_CELLS`] at either break, and it
//! clears both:
//!
//! * **PRIMARY (the shipped [`crate::vild::compute_vild`])**: pooled POD
//!   **100.00%**, FAR **0.00%**, CSI **100.00%** at both breaks; threshold
//!   agreement 100.00%, MAE **0.0000** g/m³ and ±15% 100.00% at every site-hour;
//!   identity **163,301 of 163,301** cells bit-identical with no presence
//!   disagreement; no invariant violation anywhere (per-site maxima 1.95–7.36
//!   g/m³, all non-negative and finite, PDB volume skew 0 s at all eight).
//! * The decision mass is almost entirely in the 23:30Z hour — KTLX 120 cells
//!   ≥ 3.5 g/m³ (field maximum 6.61), KINX 97 (7.36), KSRX 48 (5.98), KVNX 39
//!   (5.07) — against **2** across all four sites at 21:30Z, two hours earlier
//!   in the same outbreak, where the storms were already precipitating heavily
//!   (2,968–32,217 lowest-cut gates ≥ 35 dBZ) but topped out at 1.95–3.68 g/m³.
//!   "Precipitating site-hour" and "hail site-hour" are not the same sample, and
//!   this is the measurement that says so.
//! * **The retired local quotient on these same volumes**: POD **4.90%** at 3.5
//!   and **1.90%** at 4.0, over a sample that is now conclusive — the muteness
//!   the July survey could only measure inconclusively, reproduced on an
//!   outbreak day. Attribution reproduces too: our-VIL/RPG-top POD 3.59%,
//!   RPG-DVL/our-top POD 95.75% at FAR 15.32%. The residual is still
//!   [`compute_vil`]'s.
//! * **The datum A/B** costs decisions at this scale: dividing by the published
//!   floor instead of the bin centre moves pooled POD 100% → **95.92%** at 3.5
//!   (13 cells of 319) and → 94.62% at 4.0.

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
