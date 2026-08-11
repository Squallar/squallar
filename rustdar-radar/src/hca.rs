//! Hydrometeor Classification (the RPG's per-tilt product 165, AWIPS `N0H`)
//! computed locally from the Level II dual-pol moments of one tilt.
//!
//! # What is implemented, and from which documents
//!
//! The WSR-88D **Hydrometeor Classification Algorithm** is fully public:
//! task `cpc023/tsk001` (`hca`) ships complete C source in the CODE
//! distribution, together with its two feeder tasks — the dual-pol
//! preprocessor `cpc004/tsk011` (`dpprep`, already transcribed for
//! [`crate::kdp`] and shared through [`crate::dpprep`]) and the **Quality
//! Index Algorithm** `cpc023/tsk002` (`qia`) — and the **Melting Layer
//! Detection Algorithm** `cpc023/tsk003` (`mlda`). Everything below was
//! first transcribed from the Build 16 mirror (github `likev/CodeOrpgPub`),
//! then cross-checked against and **updated to the CODE Build 21.0r1.7
//! public source** (the fleet runs ≥ B21 semantics; the delta list is
//! below), with the fleet-default adaptation values from
//! `cpc104/lib006/{hca,qia,mlda,dpprep,hail}.alg`. The algorithm lineage is
//! Park, Ryzhkov, Zrnić, Kim 2009, "The Hydrometeor Classification
//! Algorithm for the Polarimetric WSR-88D" (Weather and Forecasting 24,
//! 730–748) for HCA and Giangrande, Krause, Ryzhkov 2008 (JAMC 47,
//! 1354–1364) for the MLDA; **where the released source and the paper
//! differ, the source wins** (the divergence list below).
//!
//! # Build 21 deltas applied over the Build 16 transcription
//!
//! Diffed file-for-file against `rpg_b21_0r1_7_pub_src` (all fleet
//! defaults; each item names its CCR where the source records one):
//!
//! * **memDS** (`hca.alg`): ZDR row (−0.3, 0, **0.9, 1.1**) — B16 had
//!   (−0.3, 0, 1.3, 1.6) — and ρ row (**0.98, 0.99**, 1.0, 1.01) — B16
//!   (0.95, 0.98, 1.0, 1.01).
//! * **memWS** (`hca.alg`): Z row (**15, 25**, 40, 50) — B16 (25, 30, 40,
//!   50); the ZDR row became **two-dimensional**, (0.5, 1.0, f2, f2+0.3)
//!   via `memFlagWS` = (none, none, f2, f2); ρ row (**0.84, 0.88, 0.97**,
//!   0.985) — B16 (0.88, 0.92, 0.95, 0.985).
//! * **WS hard threshold** (CCR NA15-00181): the `Z < min_Z_WS` leg is
//!   commented out of `hca_allowedHydroClass.c`; only `ZDR < 0` kills WS.
//! * **Melting-layer zones** (`hca_allowedHydroClass.c`): the upper
//!   transition regained **BI** and the above-layer zone regained **GC and
//!   BI** (B16: `GC DS WS IC GR BD RH` / `DS IC GR RH`).
//! * **Tie-break** (CCR NA14-00181): an aggregation margin under
//!   `min_Dif_Agg` no longer reads UK — `Break_tie` picks between winner
//!   and runner-up by the AEL Table 4 priority of the gate's zone, with the
//!   source's "tuned" upper lists (BI/GC prepended).
//! * **Hail Size Discrimination** (CCR NA14-00275, `enable_size = Yes`):
//!   `HailSize_v3` subclasses RH gates into small/large/giant against six
//!   height regimes around the wet-bulb 0 °C/−25 °C heights (`hail.alg`
//!   operator values; here [`HsdaHeights`], sounding-fed), with
//!   `min_data_size = 2` despeckling and a ZDR ≥ 2 hard stop; product 165
//!   emits **LH 110 / GH 120** for the large/giant subclasses
//!   (`dualpol8bit.c`'s `EXT_LH`/`EXT_GH`). `hca_setMembershipPoints.c`
//!   additionally re-derives RH's F1-flagged ZDR points from the gate
//!   height in the two regimes below the wet-bulb zero (hardcoded
//!   polynomials — the `.alg`'s unused `h1` coefficients are *not* what
//!   the code evaluates).
//! * **Met-signal preprocessing** (CCR NA14-00100, `metsignal_processing =
//!   ON`): the dpprep meteorological flag, unfold filter and the CAPPI
//!   rescue — see [`crate::dpprep`]'s module doc; the QIA is unchanged
//!   except for a blockage term (`Nc`) that is zero without the blockage
//!   store, and `melting_layer.c`'s constants are unchanged (its B21
//!   model-merge refactor is operational state, same gap as before).
//! * `dpprep`'s new `DPRA`/`DPIN` output phases and `findBragg` feed DP QPE
//!   / CDA / monitoring, not this chain.
//!
//! **Chain** (`cpc104/lib003/task_attr_table`): super-res base data →
//! `recomb` → `dpprep` → `qia` → `hca` → `dualpol8bit` (product 165). Per
//! recombined 1° × 0.25 km radial, dpprep hands HCA these fields, all of
//! which [`compute_hca`] reproduces through [`crate::dpprep`]:
//!
//! * `DSMZ` — 3-gate smoothed, attenuation-corrected Z (`z_prcd`);
//! * `DZDR` — 5-gate smoothed, attenuation-corrected ZDR (`zdr_prcd`, the
//!   recombined ZDR being `10·log10(phc/pvc)` of the pair's averaged
//!   powers);
//! * `DRHO` — 5-gate smoothed ρhv (`rho_prcd`; the noise correction is
//!   compiled out of the released build);
//! * `DKDP` — the 9/25-gate merged KDP, censored on smoothed ρ < 0.9;
//! * `DPHI` — the **25-gate smoothed, interpolated** ΦDP (`phi_long_gate`),
//!   not the raw phase: it feeds the quality indices and the RA hard
//!   threshold;
//! * `DSNR` — SNR from the 3-gate smoothed Z and the radial header's
//!   `dBZ0`/atmos;
//! * `DSMV` — 5-gate smoothed velocity;
//! * `DSDZ` — texture SD(Z): the 5-gate non-biased std of `Z − Z̄₅`,
//!   differences beyond ±50 dB excluded (`DPPT_std_filter`);
//! * `DSDP` — texture SD(ΦDP): the 9-gate std of `φ_unfolded − φ̄₉`,
//!   differences beyond ±100° excluded.
//!
//! Each field crosses task boundaries as a quantized moment
//! (`Add_moment`/`RPGCS_radar_data_conversion`), so the primary pipeline
//! rounds the 8-bit fields to their transport resolution — Z and SNR to
//! 0.5 dB, ZDR to 1/16 dB, SD(Z) to 1/8.33, SD(ΦDP) to 0.4°, velocity to
//! 0.5 m/s, the quality indices to 0.01 — and a moment gate whose **raw**
//! input was missing is missing downstream regardless of what the smoothing
//! window filled in (`Add_moment` keys the level on `inp`). The 16-bit
//! fields (ρ, φ, KDP) travel at sub-physical resolution and are not
//! re-quantized here.
//!
//! **QIA** (`qia_process.c`, the released "simple" version): per gate, six
//! quality indices `q = exp(−0.69·Σ c²)` with components `φ/600` (Z),
//! `φ/300` (ZDR), `φ/100` (ρ, KDP), `(1−ρ)/0.5` (zeroed when ρ < 0.8 and
//! Z < 25 dBZ — attenuation, `z_atten_thresh`), `snr_thresh/snr` in linear
//! power (0 dB for Z/KDP/SDZ/SDP, 5 dB for ZDR), and the beam-blockage term
//! (zero here — see the gap list). Non-finite indices become 0.
//!
//! **HCA proper** (`hca_process_radial.c` and friends): per gate,
//! * SNR < 5 dB (`min_snr`) → no echo (NE);
//! * range-folded ZDR/ρ/φ → unknown (UK) — unreachable from Archive II
//!   dual-pol moments, whose decode maps RF to missing;
//! * hard thresholds (`hca_allowedHydroClass.c`, `hca.alg` values)
//!   invalidate classes: |V| > 1 kills GC; Z > 50 kills RA (plus ρ < 0.94
//!   with φ < 100°); Z < 30 kills RH and HR (HR also ZDR < 1); Z > 40 kills
//!   IC; Z outside [10, 60] or ZDR > 2 kills GR; Z < 15 or ZDR < 0.5 kills
//!   BD; ZDR < 0 kills WS (the Z leg is gone in B21); ZDR > 2 kills DS;
//!   ρ > 0.97 or Z > 35 kills BI (`atten_control = Off` applies both
//!   everywhere);
//! * the melting layer gates the allowed set by the gate's position against
//!   the four beam/ML intersection ranges (`hca_beamMLIntersection.c`,
//!   effective radius 7708.91 km, 1° beam): below — GC BI BD RA HR RH;
//!   entering — + WS GR; within — GC BI DS WS GR BD RH; upper — GC BI DS
//!   WS IC GR BD RH; above — GC BI DS IC GR RH;
//! * for each surviving class, six trapezoidal memberships
//!   (`hca_setMembershipPoints.c` + `hca_degreeMembership.c`; the ZDR and
//!   LKDP breakpoints of the rain family shift with Z through
//!   `f1/f2/f3/g1/g2`, and RH's ZDR points additionally with gate height
//!   below the wet-bulb zero — the HSDA modification), each weighted by
//!   the class×variable weight **and** the gate's quality index, aggregate
//!   `Σ WQF/(Σ WQ + 0.01)`;
//! * the largest aggregation wins; a maximum under 0.4 (`min_Agg`) yields
//!   UK, and a margin under 0.001 (`min_Dif_Agg`) goes to the zone's AEL
//!   Table 4 priority (`Break_tie`). LKdp is `10·log10(KDP)`, floored at
//!   −40 for KDP < 0.001 (`MINI_LKTP`);
//! * RH gates then pass through `HailSize_v3` (see the B21 delta list).
//!
//! The output uses the product's external codes (`dualpol8bit.c`'s
//! `Class_external`, class × 10): RA 60, HR 70, RH 100 (LH 110 and GH 120
//! for the large/giant-hail subclasses), BD 80, BI 10, GC 20, DS 40, WS 50,
//! IC 30, GR 90, UK 140; NE encodes level 0 and decodes as undefined,
//! exactly as the Level III twin's codec treats it.
//!
//! # Melting layer and environmental data
//!
//! What the operational chain actually does with the model 0 °C height
//! (`hca_buffer_control.c`, `melting_layer.c`):
//!
//! * On the first volume, and whenever the MLDA produces nothing, HCA uses
//!   a **flat** layer: top = the `height_0` adaptation value (the
//!   operator/model 0 °C height, kft MSL) converted to km above radar
//!   level, bottom = top − 0.5 km, both floored at ground.
//!   [`MeltingLayer::from_zero_c_height`] mirrors this with the WP-S
//!   sounding's [`crate::sounding::EnvHeights::h0c_km_msl`] standing in for
//!   `height_0`.
//! * The radar-based MLDA ([`detect_melting_layer`], Giangrande 2008 per
//!   `melting_layer.c`) accumulates "wet snow" detections from the 4°–10°
//!   tilts — gates whose HCA class is not GC/BI/UK/NE, SNR > 5, Z in
//!   (15, 47), ρ in (0.90, 0.97), whose 0.5-km-above window's Z maximum is
//!   in (30, 47) and ZDR maximum in (0.8, 2.2), both at ρ > 0.85 — into an
//!   azimuth × 100-m-height histogram weighted by elevation
//!   (`(0.36·e − 0.56)·(e/10)` above 1), sums it over ±10° of azimuth,
//!   clips to ±1 km of the previous top, and reads the top and bottom as
//!   the 80th and 20th percentiles (+0.05 km). An azimuth needs a summed
//!   weight above 1500 (`min_wet_snow_sum`); gaps interpolate between the
//!   valid neighbours around the circle, and no valid azimuth at all falls
//!   back to the flat default.
//! * Operationally the RPG accumulates those histograms across **3 volumes**
//!   (6 in clear air), applies the previous volume's result, and — with the
//!   fleet default `Melting_Layer_Source = Model_Enhanced` — merges in the
//!   RUC/RAP **freezing-height grid**, per-azimuth, when fewer than 320
//!   azimuths are radar-valid. Both are operational state a single archived
//!   volume cannot reproduce (the model grid is not in the archive at all),
//!   so this module's primary is the volume's own radar detection with the
//!   sounding 0 °C fallback — the documented `Radar_Based` source, one
//!   volume fresher than the operational value, with `RPG_0C_Hgt` as the
//!   bounded A/B alternative.
//!
//! # Where the released source diverges from Park et al. (2009)
//!
//! The source's constant tables win throughout; the paper values are noted
//! so nobody "fixes" them back:
//!
//! * BD's Z membership is (10, 15, 45, 50) in `hca.alg`, (20, 25, 45, 50)
//!   in the paper — with the BD hard threshold rewritten from
//!   `ZDR < f2(Z) − 0.3` to fixed `Z < 15 || ZDR < 0.5`;
//! * BI's ZDR x2 is 0 (paper 2) and its ρ row is (0.30, 0.50, 0.85, 0.90)
//!   (paper x3/x4 0.80/0.83); the source adds the `max_Z_BI = 35` kill;
//! * DS's ZDR row is (−0.3, 0, 0.9, 1.1) in B21 (paper (−0.3, 0, 0.3,
//!   0.6); B16 shipped (−0.3, 0, 1.3, 1.6));
//! * RH's minimum-Z hard threshold is 30 dBZ (paper 40);
//! * LKdp floors at −40 for KDP < 0.001 (paper −30 for ≤ 0.001);
//! * the aggregation denominator carries `+ 0.01`;
//! * the quality indices are the QIA's released "simple" version, not the
//!   paper's confidence vector (Eqs. 14–19: no NBF gradients, no ΔZDR, no
//!   blockage estimate);
//! * the paper's convective/stratiform separation and despeckling do not
//!   exist in the released HCA task (the only despeckle is the HSDA's
//!   hail-size one);
//! * MLDA's ZDR-maximum profile ceiling is 2.2 dB (`mlda.alg`; the paper
//!   text says 2.5).
//!
//! # Documented gaps against the RPG
//!
//! * **Beam blockage** (`read_Blockage`, the FShield Z adjustment and the
//!   QIA blockage term) needs the per-site blockage store, which the
//!   archive stream does not carry; this derivation runs unblocked
//!   (blockage 0 ≤ `Min_blockage` 5%), so terrain-blocked sectors at
//!   mountain sites will diverge.
//! * **Velocity** on split-cut surveillance tilts: the RPG's HCA input has
//!   the Doppler cut's velocity recombined in; the archive's surveillance
//!   sweep carries none, so the GC velocity kill is inert there (a missing
//!   V skips the test, per the source's own NO_DATA guard).
//! * The RPG computes in `float`; this module computes in `f64` — orders of
//!   magnitude below the transport quantization it reproduces.
//! * The RF → UK branch is unreachable (see above).
//!
//! # Validation status — read before trusting the twin harness to pass
//!
//! **The live twin harness, its `validation_policy` (compatible pairs,
//! quarantine table) and the offline policy pins now live on branch
//! `campaign-harness`.** The figures below are the last measured before
//! the move; re-measuring means that branch.
//!
//! The live harness scores the derivation against the RPG's own N0H for the
//! same volume and cut (paired like the KDP twin, elevation-angle fallback
//! included), as classes: exact agreement plus a compatible-pair band
//! (WS↔GR, BD↔RA, HR↔RA — see `validation_policy`) and the full confusion
//! matrix per site. Verifying the encoding against live PDBs found product
//! 165's packet scale factor carrying the projection constant, like its
//! sibling 163 — every roster site declared PDB scale 1 / offset 0 (levels
//! ARE the class codes) and ~1.0 km/gate for a 0.25 km product, fixed in
//! `ProductDescriptionBlock::range_gate_km`.
//!
//! A full-roster survey on 2026-07-29 (~00:50 UTC volumes, every site in
//! nocturnal clear-air biology — no precipitation anywhere on the roster,
//! so the melting-layer machinery and the WS/GR/BD/HR compatible band went
//! unexercised): all 22 sites were measured, **exact agreement 88.7–98.5%,
//! every site over the 85% exact bar** (KTLX 98.53, KMVX 98.38, KMLB 97.80,
//! …, KSFX 88.74, KMTX 88.76); presence disagreement 5.8–19.3%, the
//! derivation defining slightly *more* than the twin, with the cells only
//! the twin defines sitting at the `min_snr` margin (the diagnostic's
//! `low-SNR` cause; `no-Z` and `uncovered` were 0 everywhere). Twelve sites
//! also met the 95% compatible bar; ten missed it (88.8–94.4%) because in
//! a biology field the compatible pairs add almost nothing — the residual
//! confusion is BI↔GC (0.5–2% each way), BI↔UK, and at the cold high-plains
//! sites DS↔WS/IC (KMTX, KBIS, KUEX), deliberately outside the pair list.
//!
//! The bounded A/B (documented conventions only) was flat on this survey:
//! radar-MLDA vs the flat 0 °C layer tied at every site (no wet snow
//! anywhere, so the detection correctly fell back to the sounding default);
//! `isdp-applied` tied everywhere but KMRX/KSFX, where the primary RDA
//! value won by 0.03–0.07; the physical-units variant lost to the
//! documented 8-bit transport on the tuning set (KTLX −0.46 its largest
//! move) and on the holdout (4 of 5 sites), so the transport stays primary.
//! The remaining residual carries operational-state fingerprints, per the
//! campaign's early-stop rule: BI↔GC hinges on the per-site **blockage
//! store** (FShield and the QIA blockage term run unblocked here) and on
//! the Doppler cut's velocity being sampled ~30 s apart from the
//! surveillance cut it is grafted onto; BI↔UK flips on `min_Dif_Agg`
//! margins of 0.001 in the aggregation, inside one 8-bit transport step of
//! the inputs; and the DS↔WS/IC band at the cold sites is where the twin's
//! melting layer is the previous volume's **model-enhanced MLDA** (the
//! RUC/RAP freezing grid plus 3-volume accumulation — state the archive
//! does not carry). None of these is reachable from a single archived
//! volume; nothing undocumented was chased.
//!
//! # Precipitation re-survey — 2026-07-29, after the B21 upgrade
//!
//! The clear-air survey never exercised the rain/hail classes, the
//! melting-layer ring or the compatible band, so the campaign re-surveyed
//! on **precipitating site-hours** picked by protocol: the roster scanned
//! at candidate hours over the previous day (`live_hca_precip_site_scan`,
//! lowest-cut gates ≥ 35 dBZ as the cheap check), twelve site-hours
//! selected for climatology — the 2026-07-29 06–08 UTC plains nocturnal
//! MCS (KUEX 15.8k hot gates, KDDC 7.0k, KAMA 5.3k, KOAX 5.2k, KEAX 2.2k,
//! KFSD 0.8k, KSGF 0.8k), the 2026-07-28 20–22 UTC afternoon convection
//! southeast and gulf (KMRX 15.9k, KMLB 9.1k Florida, KMOB 2.3k) and
//! mountain west (KSFX 3.9k, KMTX 3.4k). No cold-sector stratiform exists
//! anywhere in late July; that regime remains unexercised.
//!
//! **Verdict: pass.** Eighteen measurements (twelve site-hours plus
//! second/third volumes at the leads): every one cleared the 85% exact
//! bar (90.9–98.8%); eight of twelve sites cleared the 95% compatible bar
//! too — KUEX 96.36/96.43, KOAX 95.45/95.56, KDDC 97.76/97.82, KEAX
//! 95.74/95.75, KSGF 97.80/97.80, KAMA 97.53/97.53, KMLB 96.02/96.22,
//! KMOB 98.77/98.84 (exact/compatible) — 330k compared gates pooled over
//! the asserted eight, conclusive under `validation_policy`. The
//! confusion matrix finally carries the precipitation classes, with
//! per-site producer accuracies at the asserted sites: RA 74–99% over
//! ~27k twin gates, HR 97–100% (KUEX n=448, KMLB n=143), BD 82–95%
//! (~5.9k), GR 72–93% (~1.6k), DS 56–98%, WS 39–75% (user 56–93% — the
//! shortfall lands in GR/DS, the paper's own overlap), RH 40–100% on
//! small populations. **HSDA validated live**: the twins do emit LH
//! 110/GH 120, and the single-gate LH/GH cells matched exactly at
//! KDDC/KAMA/KSGF (7 of 8 across the survey) — wrong before this upgrade,
//! when those cells could only read RH.
//!
//! Four sites are **quarantined** with two-run, multi-volume evidence
//! (see `validation_policy::QUARANTINED`): KFSD (biology-dominated
//! field, compatible adds nothing, residual = the documented BI↔GC/UK
//! state fingerprints), KMRX and KSFX (terrain blockage-store residual),
//! and KMTX (the 07-28 episode's twin ran a model-enhanced melting layer
//! below our sounding flat — RA→DS 0.8–1.3% — while the same site
//! **passed both bars** on 2026-07-29 07:57, 96.04/96.16, pinning the
//! miss on ML state, not transcription). Every quarantined site still
//! clears the exact bar on every volume.
//!
//! **Melting-layer ring**: [`detect_melting_layer`] concluded from wet
//! snow at none of the eighteen measurements (0/360 azimuths everywhere) —
//! a single volume's 4°–10° histogram never reaches `min_wet_snow_sum`
//! = 1500 in July convection, where the operational MLDA accumulates
//! three volumes and merges the model grid. Every survey ran on the
//! sounding flat layer, and the radar-vs-flat A/B rows were identical at
//! all eighteen; the only place the twin's transition band disagreed with
//! the sounding was the quarantined KMTX episode above. WS populations at
//! the asserted plains sites (n=36–325 per site, producer 39–75%,
//! compatible with GR) sat inside the sounding band.
//!
//! **A/B in precipitation** (decided on the precipitating tuning set
//! KUEX/KMLB/KMTX/KMRX/KDDC, confirmed on the holdout
//! KOAX/KAMA/KMOB/KSFX/KEAX/KSGF/KFSD, which played no part):
//!
//! * **B21 met-signal flag vs the legacy ρ/SNR flag**: met signal won 4
//!   of 5 tuning sites (+0.07…+0.24 exact, one KMTX tie at −0.01) and 7
//!   of 7 holdouts (+0.06…+0.89, KFSD tie) — the fleet-default
//!   `metsignal_processing = ON` stays primary, now with survey evidence.
//! * **Volume-built CAPPI vs cold start**: identical on every measurement
//!   — every paired N0H tilt sits under 1.0°, where `apply_CAPPI` never
//!   fires. The warm build stays primary as the closer operational
//!   approximation for the ≥ 1° consumers.
//! * **radar-MLDA vs flat**: tied everywhere (no detection).
//! * **isdp-applied** and **physical-units**: ties to small losses; the
//!   documented defaults stay.

use crate::dpprep::{
    CORR_THRESH, DBZ_THRESH, DBZ_WINDOW, DpCombined, DpInput, LONG_GATE, MET_SIG_THRESHOLD,
    SHORT_GATE, UNFOLD_MIN_RHO, WINDOW, average_filter, clean_met_signal, combine_sweep_dp,
    find_met_signal, index_into, interpolate, is_high_attenuation_radial, isdp_from_queue,
    kdp_from_phi, median_filter, meteo_groups, radial_system_phi, resample_to_polar_grid,
    std_filter, unfold_phidp,
};
use crate::kdp::KdpParams;
use crate::par::*;
use nexrad_model::data::Radial;

pub use crate::dpprep::ReflCappi;

// ── Class indices (hca.h) and the product's external codes ──────────────────

pub(crate) const NUM_CLASSES: usize = 14;
const U0: usize = 0;
const U1: usize = 1;
pub(crate) const RA: usize = 2;
pub(crate) const HR: usize = 3;
pub(crate) const RH: usize = 4;
pub(crate) const BD: usize = 5;
pub(crate) const BI: usize = 6;
pub(crate) const GC: usize = 7;
pub(crate) const DS: usize = 8;
pub(crate) const WS: usize = 9;
pub(crate) const IC: usize = 10;
pub(crate) const GR: usize = 11;
pub(crate) const UK: usize = 12;
pub(crate) const NE: usize = 13;

/// `dualpol8bit.c`'s `Class_external`: internal class index → the product's
/// data level (class codes scaled by 10). U0/U1/NE map to 0, which the
/// Level III codec decodes as undefined.
pub const CLASS_EXTERNAL: [f32; NUM_CLASSES] = [
    0.0, 0.0, 60.0, 70.0, 100.0, 80.0, 10.0, 20.0, 40.0, 50.0, 30.0, 90.0, 140.0, 0.0,
];

/// The C sentinel for a missing value (`HCA_NO_DATA`). The classification
/// arithmetic runs in this sentinel domain, exactly as the source does —
/// a missing ZDR *is* −10⁵ dB against every threshold and membership edge.
pub(crate) const NO_DATA: f64 = -1.0e5;

/// `MINI_LKTP`: LKdp for KDP below 0.001 °/km.
const MINI_LKTP: f64 = -40.0;

// ── hca.alg fleet defaults ───────────────────────────────────────────────────
//
// The five ZDR class kills below are `pub(crate)`: `voxel::volume_alpha_profile`
// takes the 3D transparency profile's quiet band for ZDR from them by
// reference, because the band where ZDR discriminates nothing is exactly the
// interval this algorithm leaves open for rain. `hca` is a `pub mod`, so
// `pub` would have published them as crate API for a consumer that lives one
// module over; `pub(crate)` is the reach they actually need. Everything else
// here stays private to the classifier.

const MIN_V_GC: f64 = 1.0;
const MAX_Z_RA: f64 = 50.0;
const MIN_RHO_RA: f64 = 0.94;
const MIN_PHIDP_RA: f64 = 100.0;
const MIN_Z_RH: f64 = 30.0;
const MIN_Z_HR: f64 = 30.0;
/// Heavy rain is refused under this ZDR.
pub(crate) const MIN_ZDR_HR: f64 = 1.0;
const MAX_Z_IC: f64 = 40.0;
const MIN_Z_GR: f64 = 10.0;
const MAX_Z_GR: f64 = 60.0;
/// Graupel is refused over this ZDR.
pub(crate) const MAX_ZDR_GR: f64 = 2.0;
const MIN_Z_BD: f64 = 15.0;
/// Big drops are refused under this ZDR.
pub(crate) const MIN_ZDR_BD: f64 = 0.5;
// B21: `min_Z_WS` is "no longer used per CCR NA15-00181" — the Z leg of the
// WS kill is commented out of `hca_allowedHydroClass.c`; only ZDR remains.
/// Wet snow is refused under this ZDR — the last liquid-bearing class to go
/// as ZDR falls through zero.
pub(crate) const MIN_ZDR_WS: f64 = 0.0;
const MAX_RHOHV_BI: f64 = 0.97;
const MAX_Z_BI: f64 = 35.0;
/// Dry snow is refused over this ZDR.
pub(crate) const MAX_ZDR_DS: f64 = 2.0;
const MIN_AGG: f64 = 0.4;
const MIN_DIF_AGG: f64 = 0.001;
const MIN_SNR: f64 = 5.0;
/// `atten_control = Off`: the BI kills apply on every radial.
const ATTEN_CONTROL: bool = false;

/// The two-dimensional membership equations (`hca.alg` f/g coefficients):
/// `f = a·Z² + b·Z + c`, `g = b·Z + c`.
const F1_COEF: (f64, f64, f64) = (0.000_750, 0.0025, -0.5);
const F2_COEF: (f64, f64, f64) = (0.002_92, -0.0481, 0.68);
const F3_COEF: (f64, f64, f64) = (0.000_485, 0.0667, 1.42);
const G1_COEF: (f64, f64) = (0.8, -44.0);
const G2_COEF: (f64, f64) = (0.5, -22.0);

// ── Fuzzy-logic input indices (hca_local.h) ──────────────────────────────────

const SMZ: usize = 0;
const ZDR: usize = 1;
const LKDP: usize = 2;
const RHO: usize = 3;
const SDZ: usize = 4;
const SDP: usize = 5;
const NUM_FL_INPUTS: usize = 6;

/// Which equation adjusts a membership point (`memFlag*` in `hca.alg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemFlag {
    None,
    F1,
    F2,
    F3,
    G1,
    G2,
}

use MemFlag::{F1, F2, F3, G1, G2, None as MF};

/// One class's six membership rows: `[input][x1..x4]` base points, plus the
/// 2-D flags added to them (`Hca_setMembershipPoints`).
pub(crate) struct MemTable {
    pub(crate) points: [[f64; 4]; NUM_FL_INPUTS],
    pub(crate) flags: [[MemFlag; 4]; NUM_FL_INPUTS],
}

/// `hca.alg`'s `memRA`/`memFlagRA`. Row order is the fuzzy-logic input
/// order: SMZ, ZDR, LKDP, RHO, SD(Z), SD(ΦDP).
pub(crate) const MEM_RA: MemTable = MemTable {
    points: [
        [5.00, 10.00, 45.00, 50.00],
        [-0.30, 0.00, 0.00, 0.50],
        [-1.00, 0.00, 0.00, 1.00],
        [0.95, 0.97, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [F1, F1, F2, F2],
        [G1, G1, G2, G2],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memHR`/`memFlagHR`.
pub(crate) const MEM_HR: MemTable = MemTable {
    points: [
        [40.00, 45.00, 55.00, 60.00],
        [-0.30, 0.00, 0.00, 0.50],
        [-1.00, 0.00, 0.00, 1.00],
        [0.92, 0.95, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [F1, F1, F2, F2],
        [G1, G1, G2, G2],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memRH`/`memFlagRH` (rain and hail).
pub(crate) const MEM_RH: MemTable = MemTable {
    points: [
        [45.00, 50.00, 75.00, 80.00],
        [-0.30, 0.00, 0.00, 0.50],
        [-10.00, -4.00, 0.00, 1.00],
        [0.85, 0.90, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [MF, MF, F1, F1],
        [MF, MF, G1, G1],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memBD`/`memFlagBD` (big drops). The Z row is the source's
/// (10, 15, 45, 50) — the paper prints (20, 25, 45, 50).
pub(crate) const MEM_BD: MemTable = MemTable {
    points: [
        [10.00, 15.00, 45.00, 50.00],
        [-0.30, 0.00, 0.00, 1.00],
        [-1.00, 0.00, 0.00, 1.00],
        [0.92, 0.95, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [F2, F2, F3, F3],
        [G1, G1, G2, G2],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memBI`/`memFlagBI` (biological). ZDR x2 is the source's 0
/// (paper 2); the ρ row tops at 0.85/0.90 (paper 0.80/0.83).
pub(crate) const MEM_BI: MemTable = MemTable {
    points: [
        [5.00, 10.00, 20.00, 30.00],
        [0.00, 0.00, 10.00, 12.00],
        [-30.00, -25.00, 10.00, 20.00],
        [0.30, 0.50, 0.85, 0.90],
        [1.00, 2.00, 4.00, 7.00],
        [8.00, 10.00, 40.00, 60.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [MF, F3, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memGC`/`memFlagGC` (ground clutter).
pub(crate) const MEM_GC: MemTable = MemTable {
    points: [
        [15.00, 20.00, 70.00, 80.00],
        [-4.00, -2.00, 1.00, 2.00],
        [-30.00, -25.00, 10.00, 20.00],
        [0.50, 0.60, 0.90, 0.95],
        [2.00, 4.00, 10.00, 15.00],
        [30.00, 40.00, 50.00, 60.00],
    ],
    flags: [[MF; 4]; 6],
};

/// `hca.alg`'s `memDS`/`memFlagDS` (dry snow). B21 tightened the row pair
/// B16 shipped: ZDR (−0.3, 0, **0.9, 1.1**) — B16 (−0.3, 0, 1.3, 1.6), the
/// paper (−0.3, 0, 0.3, 0.6) — and ρ (**0.98, 0.99**, 1.00, 1.01) — B16
/// (0.95, 0.98, 1.00, 1.01).
pub(crate) const MEM_DS: MemTable = MemTable {
    points: [
        [5.00, 10.00, 35.00, 40.00],
        [-0.30, 0.00, 0.90, 1.10],
        [-30.00, -25.00, 10.00, 20.00],
        [0.98, 0.99, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [[MF; 4]; 6],
};

/// `hca.alg`'s `memWS`/`memFlagWS` (wet snow), reworked wholesale in B21:
/// Z (**15, 25**, 40, 50) — B16 (25, 30, 40, 50); the ZDR row became
/// two-dimensional, (0.5, 1.0, f2+0, f2+0.3) via `memFlagWS`'s new
/// (none, none, f2, f2); ρ widened to (**0.84, 0.88, 0.97**, 0.985) — B16
/// (0.88, 0.92, 0.95, 0.985).
pub(crate) const MEM_WS: MemTable = MemTable {
    points: [
        [15.00, 25.00, 40.00, 50.00],
        [0.50, 1.00, 0.00, 0.30],
        [-30.00, -25.00, 10.00, 20.00],
        [0.84, 0.88, 0.97, 0.985],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [MF, MF, F2, F2],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// `hca.alg`'s `memIC`/`memFlagIC` (ice crystals).
pub(crate) const MEM_IC: MemTable = MemTable {
    points: [
        [0.00, 5.00, 20.00, 25.00],
        [0.10, 0.40, 3.00, 3.30],
        [-5.00, 0.00, 10.00, 15.00],
        [0.95, 0.98, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [[MF; 4]; 6],
};

/// `hca.alg`'s `memGR`/`memFlagGR` (graupel).
pub(crate) const MEM_GR: MemTable = MemTable {
    points: [
        [25.00, 35.00, 50.00, 55.00],
        [-0.30, 0.00, 0.00, 0.30],
        [-30.00, -25.00, 10.00, 20.00],
        [0.90, 0.97, 1.00, 1.01],
        [0.00, 0.50, 3.00, 6.00],
        [0.00, 1.00, 15.00, 30.00],
    ],
    flags: [
        [MF, MF, MF, MF],
        [MF, MF, F1, F1],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
        [MF, MF, MF, MF],
    ],
};

/// The fuzzy-logic classes' membership tables, indexed `class − RA`.
pub(crate) const MEM: [&MemTable; 10] = [
    &MEM_RA, &MEM_HR, &MEM_RH, &MEM_BD, &MEM_BI, &MEM_GC, &MEM_DS, &MEM_WS, &MEM_IC, &MEM_GR,
];

/// `hca.alg`'s weight arrays, transposed to `[class − RA][input]`. The
/// class columns of `weight_Z`…`weight_SDPHIdp` in order RA HR RH BD BI GC
/// DS WS IC GR (U0/U1/UK/NE all carry 0 and never score).
pub(crate) const WEIGHT: [[f64; NUM_FL_INPUTS]; 10] = [
    // SMZ  ZDR  LKDP RHO  SDZ  SDP
    [1.0, 0.8, 0.0, 0.6, 0.2, 0.2], // RA
    [1.0, 0.8, 1.0, 0.6, 0.2, 0.2], // HR
    [1.0, 0.8, 1.0, 0.6, 0.2, 0.2], // RH
    [0.8, 1.0, 0.0, 0.6, 0.2, 0.2], // BD
    [0.4, 0.6, 0.0, 1.0, 0.8, 0.8], // BI
    [0.2, 0.4, 0.0, 1.0, 0.6, 0.8], // GC
    [1.0, 0.8, 0.0, 0.6, 0.2, 0.2], // DS
    [0.6, 0.8, 0.0, 1.0, 0.2, 0.2], // WS
    [1.0, 0.6, 0.5, 0.4, 0.2, 0.2], // IC
    [0.8, 1.0, 0.0, 0.4, 0.2, 0.2], // GR
];

// ── qia.alg / qia_process.c constants ────────────────────────────────────────

const QIA_C: f64 = -0.69;
const PHI_DP_Z_THRESH: f64 = 600.0;
const PHI_DP_ZDR_THRESH: f64 = 300.0;
const PHI_DP_PHI_THRESH: f64 = 100.0;
const PHI_DP_KDP_THRESH: f64 = 100.0;
/// `pow(10, 0.1·5.0)` as the source spells it.
const LINEAR_SNR_ZDR_THRESH: f64 = 3.16228;
const DELTA_RHO_1_THRESHOLD: f64 = 0.5;
const RHO_MIN_THRESH: f64 = 0.8;
/// `qia.alg`'s `z_atten_thresh`.
const Z_ATTEN_THRESH: f64 = 25.0;
/// The quality indices' 8-bit transport (`Q_scale`/`Q_offset`).
const Q_SCALE: f64 = 100.0;
const Q_OFFSET: f64 = 2.0;

// ── mlda.alg fleet defaults / melting_layer.c constants ─────────────────────

const ML_DEPTH_KM: f64 = 0.5;
const ML_MAX_TOP_KM: f64 = 8.0;
const ML_HEIGHT_INTERVAL_KM: f64 = 0.1;
const ML_MAX_HEIGHTS: usize = 80;
const ML_UPPER_RHO: f64 = 0.97;
const ML_LOWER_RHO: f64 = 0.90;
const ML_LOW_RHO_PROFILE: f64 = 0.85;
const ML_UPPER_ZMAX: f64 = 47.0;
const ML_LOWER_ZMAX: f64 = 30.0;
const ML_UPPER_Z: f64 = 47.0;
const ML_LOWER_Z: f64 = 15.0;
const ML_UPPER_ZDRMAX: f64 = 2.2;
const ML_LOWER_ZDRMAX: f64 = 0.8;
const ML_HALF_WINDOW: usize = 10;
const ML_UPPER_ELEV: f64 = 10.0;
const ML_LOWER_ELEV: f64 = 4.0;
const ML_HIGH_PERCENTILE: f64 = 0.80;
const ML_LOW_PERCENTILE: f64 = 0.20;
const ML_MIN_WET_SNOW_SUM: f64 = 1500.0;
const ML_MIN_SNR: f64 = 5.0;
/// `melting_layer.c`'s beam-height model: 4/3-equivalent `IR·RE`.
const ML_IR: f64 = 1.21;
const ML_RE_KM: f64 = 6371.0;
/// `hca_beamMLIntersection.c`'s effective Earth radius ("per RPG
/// requirements" — not the 8498.67 km the 4/3 model would give).
const BEAM_ML_AE_KM: f64 = 7708.91;
const BEAM_WIDTH_DEG: f64 = 1.0;

/// The `height_0` fallback the source hardcodes when the adaptation store
/// is unreadable: 10.5 kft, in km MSL.
pub const DEFAULT_HEIGHT_0_KM_MSL: f64 = 10.5 * 0.3048;

// ── HSDA (Hail Size Discrimination, CCR NA14-00275; HailSize.cpp v3) ────────

/// `hca.alg`'s `enable_size` fleet default (Yes): product 165 subclasses RH
/// into small/large/giant hail, large and giant carrying their own codes.
const ENABLE_SIZE: bool = true;
/// `hca.alg`'s `min_data_size`: hail-size runs shorter than this despeckle
/// down one size.
const MIN_DATA_SIZE: usize = 2;
/// `dualpol8bit.c`'s `EXT_LH`/`EXT_GH`: the product codes of the RH
/// subclasses (small hail stays at RH's 100).
const EXT_LH: f32 = 110.0;
const EXT_GH: f32 = 120.0;
/// `hail.alg`'s operator-maintained wet-bulb heights, kft MSL → km: the
/// fleet defaults stand in when no environmental value is available.
pub const DEFAULT_HEIGHT_TW0_KM_MSL: f64 = 10.0 * 0.3048;
pub const DEFAULT_HEIGHT_TW_M25_KM_MSL: f64 = 22.0 * 0.3048;
/// `HailSize.cpp`'s hard bounds. `HSDA_MAX_ZDR` is `pub(crate)` for the same
/// reason the ZDR class kills above are: it is the hard ceiling that makes
/// "hail is a near-zero ZDR signature" a statement of this crate's own
/// algorithm rather than of received wisdom, and `voxel` is the only thing
/// outside this module that needs to say so.
pub(crate) const HSDA_MAX_ZDR: f64 = 2.0;
const HSDA_MIN_ZDR: f64 = -7.75;
const HSDA_MIN_RHO: f64 = 0.0;
const HSDA_MAX_Z: f64 = 100.0;
const HSDA_DELTA_ZDR: f64 = -0.50;
const HSDA_MIN_PV: f64 = 0.2;
const HSDA_MIN_AGG: f64 = 0.6;

/// The wet-bulb heights the HSDA regimes and the RH ZDR-membership
/// modification read, km **above radar level** — `Hca_process_radial`'s
/// `Hca_0_Tw_height`/`Hca_minus_25_Tw_height` after its MSL → ARL
/// conversion. Operationally these are the `hail.alg` operator values;
/// [`from_env_heights`](Self::from_env_heights) stands the WP-S sounding's
/// dry-bulb heights in for them (wet-bulb sits within a few hundred metres
/// below dry-bulb in moist columns — inside the operator values' own
/// update cadence), extrapolating −25 °C from the 0/−20 °C lapse.
#[derive(Debug, Clone, Copy)]
pub struct HsdaHeights {
    pub tw0_km_arl: f64,
    pub twm25_km_arl: f64,
}

impl HsdaHeights {
    /// From MSL heights, as `Hca_process_radial` converts them. The source
    /// does not floor these at ground.
    pub fn from_msl(tw0_km_msl: f64, twm25_km_msl: f64, radar_km_msl: f64) -> Self {
        Self {
            tw0_km_arl: tw0_km_msl - radar_km_msl,
            twm25_km_arl: twm25_km_msl - radar_km_msl,
        }
    }

    /// The `hail.alg` fleet defaults (10.0 / 22.0 kft MSL).
    pub fn operational_defaults(radar_km_msl: f64) -> Self {
        Self::from_msl(
            DEFAULT_HEIGHT_TW0_KM_MSL,
            DEFAULT_HEIGHT_TW_M25_KM_MSL,
            radar_km_msl,
        )
    }

    /// From the sounding's dry-bulb 0 °C / −20 °C heights (km MSL):
    /// −25 °C extrapolated by a quarter of the 0 → −20 °C depth.
    pub fn from_env_heights(h0c_km_msl: f64, hm20c_km_msl: f64, radar_km_msl: f64) -> Self {
        let hm25 = hm20c_km_msl + 0.25 * (hm20c_km_msl - h0c_km_msl);
        Self::from_msl(h0c_km_msl, hm25, radar_km_msl)
    }
}

// ── dpprep transport scales (dpp_format.c / qia_process.c Add_moment) ───────

const SMZ_SCALE: (f64, f64) = (2.0, 66.0);
const SNR_SCALE: (f64, f64) = (2.0, 26.0);
const SDZ_SCALE: (f64, f64) = (8.33, 2.0);
const SDP_SCALE: (f64, f64) = (2.5, 2.0);
const ZDR_SCALE: (f64, f64) = (16.0, 128.0);
const SMV_SCALE: (f64, f64) = (2.0, 129.0);

/// `dpprep.alg`'s texture exclusion thresholds.
const MAX_DIFF_DBZ: f64 = 50.0;
const MAX_DIFF_PHIDP: f64 = 100.0;

// ── Melting layer ────────────────────────────────────────────────────────────

/// Per-azimuth melting-layer top and bottom, km **above radar level** — the
/// exact form `Hca_buffer_control` holds (`ML_top`/`ML_bottom`).
#[derive(Debug, Clone)]
pub struct MeltingLayer {
    pub top_km_arl: [f64; 360],
    pub bottom_km_arl: [f64; 360],
}

impl MeltingLayer {
    /// A flat layer: top at `top_km_arl`, bottom 0.5 km below, both floored
    /// at ground — the source's default construction (`HALF_KM`).
    pub fn flat(top_km_arl: f64) -> Self {
        let top = top_km_arl.max(0.0);
        let bottom = (top - ML_DEPTH_KM).max(0.0);
        Self {
            top_km_arl: [top; 360],
            bottom_km_arl: [bottom; 360],
        }
    }

    /// The operational default: the environmental 0 °C height (km MSL —
    /// [`crate::sounding::EnvHeights::h0c_km_msl`] standing in for the
    /// `height_0` adaptation value) converted to above-radar-level, bottom
    /// 0.5 km below.
    pub fn from_zero_c_height(h0c_km_msl: f64, radar_km_msl: f64) -> Self {
        Self::flat(h0c_km_msl - radar_km_msl)
    }
}

/// The four beam/melting-layer intersection ranges of one radial, as DP bin
/// numbers (`Hca_beamMLintersection`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct MlBins {
    /// `BEAM_EDGE_BOTTOM`: the beam's *upper* edge crossing the layer
    /// bottom — the nearest of the four, the absolute bottom of the layer.
    bb: i64,
    b: i64,
    t: i64,
    /// `BEAM_EDGE_TOP`: the beam's *lower* edge crossing the layer top —
    /// the farthest of the four, the absolute top of the layer.
    pub(crate) tt: i64,
}

/// `Hca_beamMLintersection`: where the 1° beam's bottom edge, centre and
/// top edge cross the layer, on the 7708.91-km effective Earth.
pub(crate) fn beam_ml_intersection(
    elev_deg: f64,
    az: usize,
    bin_size_km: f64,
    ml: &MeltingLayer,
) -> MlBins {
    let half_bw = (BEAM_WIDTH_DEG / 2.0).to_radians();
    let e = elev_deg.to_radians();
    let ae = BEAM_ML_AE_KM;
    let range = |h: f64, s: f64| (2.0 * h * ae + ae * ae * s * s).sqrt() - ae * s;
    let r_bb = range(ml.bottom_km_arl[az], (e + half_bw).sin());
    let r_b = range(ml.bottom_km_arl[az], e.sin());
    let r_t = range(ml.top_km_arl[az], e.sin());
    let r_tt = range(ml.top_km_arl[az], (e - half_bw).sin());
    MlBins {
        bb: (r_bb / bin_size_km).round() as i64,
        b: (r_b / bin_size_km).round() as i64,
        t: (r_t / bin_size_km).round() as i64,
        tt: (r_tt / bin_size_km).round() as i64,
    }
}

// ── Membership machinery ─────────────────────────────────────────────────────

/// `Hca_setMembershipPoints`: the class×input row's four points, the 2-D
/// rows adjusted by `f1/f2/f3/g1/g2` of the (FShield-adjusted) reflectivity.
/// With HSDA enabled (B21, CCR NA14-00275), the RH class's F1-flagged ZDR
/// points are re-derived from the gate height against the wet-bulb 0 °C
/// height in the two regimes below it — the hardcoded polynomials of
/// `hca_setMembershipPoints.c`, not the `.alg`'s `h1` coefficients.
fn set_membership_points(
    class: usize,
    fl_input: usize,
    z_fshield: f64,
    height_km: f64,
    tw0_km_arl: f64,
) -> [f64; 4] {
    let table = MEM[class - RA];
    let mut points = [0.0; 4];
    for (x, point) in points.iter_mut().enumerate() {
        let flag = table.flags[fl_input][x];
        let mut eqn = match flag {
            MemFlag::None => 0.0,
            MemFlag::F1 => F1_COEF.0 * z_fshield * z_fshield + F1_COEF.1 * z_fshield + F1_COEF.2,
            MemFlag::F2 => F2_COEF.0 * z_fshield * z_fshield + F2_COEF.1 * z_fshield + F2_COEF.2,
            MemFlag::F3 => F3_COEF.0 * z_fshield * z_fshield + F3_COEF.1 * z_fshield + F3_COEF.2,
            MemFlag::G1 => G1_COEF.0 * z_fshield + G1_COEF.1,
            MemFlag::G2 => G2_COEF.0 * z_fshield + G2_COEF.1,
        };
        if ENABLE_SIZE && class == RH && fl_input == ZDR && flag == MemFlag::F1 {
            if tw0_km_arl - 2.0 < height_km && height_km <= tw0_km_arl - 1.0 {
                eqn = 5e-4 * z_fshield * z_fshield + 1.5e-2 * z_fshield - 0.9;
            } else if tw0_km_arl - 1.0 < height_km && height_km < tw0_km_arl {
                eqn = 0.02 * z_fshield - 0.6;
            }
        }
        *point = eqn + table.points[fl_input][x];
    }
    points
}

/// `Hca_degreeMembership`: the trapezoid, 0 outside (x1, x4), 1 on
/// [x2, x3], linear on the shoulders — and 0 outright when the points are
/// not monotonic (which the Z-dependent rows produce at extreme Z).
fn degree_membership(d: f64, points: [f64; 4]) -> f64 {
    let [x1, x2, x3, x4] = points;
    if x1 > x2 || x2 > x3 || x3 > x4 {
        return 0.0;
    }
    if d >= x2 && d <= x3 {
        1.0
    } else if d <= x1 || d >= x4 {
        0.0
    } else if d > x1 && d < x2 {
        (d - x1) / (x2 - x1)
    } else {
        (x4 - d) / (x4 - x3)
    }
}

/// `Hca_weightedMembershipAggregation`: `Σ WQF / (Σ WQ + 0.01)`.
fn weighted_aggregation(weight: &[f64; 6], quality: &[f64; 6], fd_mem: &[f64; 6]) -> f64 {
    let mut s = 0.0;
    for i in 0..NUM_FL_INPUTS {
        s += weight[i] * quality[i];
    }
    let mut sfd = 0.0;
    for i in 0..NUM_FL_INPUTS {
        sfd += weight[i] * quality[i] * fd_mem[i] / (s + 0.01);
    }
    sfd
}

/// `Hca_allowedHydroClass`: the hard thresholds and the melting-layer
/// zones, setting disallowed classes to `INVALID_CLASS`.
#[allow(clippy::too_many_arguments)]
fn allowed_hydro_class(
    bin: i64,
    z: f64,
    zdr: f64,
    rho: f64,
    phi: f64,
    v: f64,
    atten_rad: bool,
    agg: &mut [f64; NUM_CLASSES],
    ml: MlBins,
) {
    const INVALID: f64 = -1.0;
    agg[U0] = INVALID;
    agg[U1] = INVALID;

    // The RF sentinel (−2e5) never occurs here (see the module doc), so the
    // velocity guard reduces to the NO_DATA check.
    if v != NO_DATA && v.abs() > MIN_V_GC {
        agg[GC] = INVALID;
    }
    if z > MAX_Z_RA {
        agg[RA] = INVALID;
    }
    if z < MIN_Z_RH {
        agg[RH] = INVALID;
    }
    if z < MIN_Z_HR || zdr < MIN_ZDR_HR {
        agg[HR] = INVALID;
    }
    if z > MAX_Z_IC {
        agg[IC] = INVALID;
    }
    if !(MIN_Z_GR..=MAX_Z_GR).contains(&z) || zdr > MAX_ZDR_GR {
        agg[GR] = INVALID;
    }
    if z < MIN_Z_BD || zdr < MIN_ZDR_BD {
        agg[BD] = INVALID;
    }
    // B21 (CCR NA15-00181): the WS kill lost its Z leg.
    if zdr < MIN_ZDR_WS {
        agg[WS] = INVALID;
    }
    if zdr > MAX_ZDR_DS {
        agg[DS] = INVALID;
    }
    if ATTEN_CONTROL && atten_rad {
        if rho > MAX_RHOHV_BI {
            agg[BI] = INVALID;
        }
    } else if rho > MAX_RHOHV_BI || z > MAX_Z_BI {
        agg[BI] = INVALID;
    }
    if rho < MIN_RHO_RA && phi < MIN_PHIDP_RA {
        agg[RA] = INVALID;
    }

    // B21 widened the two upper zones: the upper transition regained BI and
    // the above-layer zone regained GC and BI (B16: GC DS WS IC GR BD RH and
    // DS IC GR RH respectively).
    let allowed: &[usize] = if bin < ml.bb {
        &[GC, BI, BD, RA, HR, RH]
    } else if bin < ml.b {
        &[GC, BI, WS, GR, BD, RA, HR, RH]
    } else if bin < ml.t {
        &[GC, BI, DS, WS, GR, BD, RH]
    } else if bin < ml.tt {
        &[GC, BI, DS, WS, IC, GR, BD, RH]
    } else {
        &[GC, BI, DS, IC, GR, RH]
    };
    for (i, a) in agg.iter_mut().enumerate() {
        if !allowed.contains(&i) {
            *a = INVALID;
        }
    }
}

/// `Break_tie` (CCR NA14-00181, B21's `hca_process_radial.c`): when the top
/// two aggregations sit within `min_Dif_Agg`, the class is chosen by the
/// AEL Table 4 priority order of the gate's melting-layer zone — B16 read
/// UK here. The upper-transition and above-layer lists carry the source's
/// "tuned" orders (BI/GC prepended to the original AEL lists).
fn break_tie(bin: i64, ml: MlBins, h_class: usize, runner_up: usize) -> usize {
    let priority: &[usize] = if bin < ml.bb {
        &[GC, BI, BD, RA, HR, RH]
    } else if bin < ml.b {
        &[GC, BI, WS, GR, BD, RA, HR, RH]
    } else if bin < ml.t {
        &[GC, BI, DS, WS, GR, BD, RH]
    } else if bin < ml.tt {
        &[BI, GC, DS, WS, IC, GR, BD, RH] // "tuned"
    } else {
        &[GC, BI, DS, IC, GR, RH]
    };
    for &c in priority {
        if c == h_class {
            return h_class;
        }
        if c == runner_up {
            return runner_up;
        }
    }
    h_class
}

// ── The preprocessed per-radial fields HCA and the MLDA consume ─────────────

/// One recombined radial's HCA inputs, in the C sentinel domain
/// ([`NO_DATA`] for missing) after the documented moment transport.
pub(crate) struct Fields {
    pub(crate) az: f64,
    pub(crate) elev: f64,
    pub(crate) hatt: bool,
    pub(crate) n: usize,
    pub(crate) dg: f64,
    /// `DSMZ` (z_prcd), `DSNR`, `DSDZ` — the z-gate fields sampled at each
    /// DP gate.
    pub(crate) smz: Vec<f64>,
    pub(crate) snr: Vec<f64>,
    pub(crate) sdz: Vec<f64>,
    pub(crate) zdr: Vec<f64>,
    pub(crate) rho: Vec<f64>,
    pub(crate) kdp: Vec<f64>,
    pub(crate) phi: Vec<f64>,
    pub(crate) sdp: Vec<f64>,
    pub(crate) smv: Vec<f64>,
    /// The cleaned met signal per gate (`DMET`), NaN when the legacy flag
    /// ran instead — the hybrid-scan compositor's usability check reads it.
    pub(crate) met: Vec<f64>,
    /// The six quality indices per gate, in fuzzy-logic input order.
    pub(crate) q: Vec<[f64; 6]>,
}

/// One value through an 8-bit moment (`Add_moment` then
/// `RPGCS_radar_data_conversion`): round half away from zero at
/// `v·scale + offset`, clamp to [2, 255], decode back.
fn transport8(v: f64, (scale, offset): (f64, f64)) -> f64 {
    if !v.is_finite() {
        return f64::NAN;
    }
    let f = v * scale + offset;
    let t = if f >= 0.0 {
        (f + 0.5) as i64
    } else {
        -((-f + 0.5) as i64)
    };
    let t = t.clamp(2, 255);
    (t as f64 - offset) / scale
}

/// NaN → the C sentinel.
fn sentinel(v: f64) -> f64 {
    if v.is_finite() { v } else { NO_DATA }
}

/// The full dpprep + QIA chain for one recombined radial. With
/// `metsignal` (the B21 fleet default) the meteorological flag and the
/// unfold filter come from the cleaned met signal — plus the CAPPI rescue
/// on ≥ 1° radials when a volume CAPPI is supplied; without it, the legacy
/// (metsignal-OFF) construction [`crate::kdp`] validated.
pub(crate) fn radial_fields(
    c: &DpCombined,
    init_fdp: f64,
    dbz0: Option<f64>,
    atmos: Option<f64>,
    quantize: bool,
    metsignal: bool,
    cappi: Option<&ReflCappi>,
) -> Fields {
    let r = &c.base;
    let n = r.phi.len();
    let nz = r.z.len();

    // SNR precedes the met signal (Compute_snr's first call, from the
    // 3-gate smoothed Z).
    let ref_smd3 = average_filter(&r.z, DBZ_WINDOW);
    let snr_z: Vec<f64> = (0..nz)
        .map(|iz| match dbz0 {
            Some(dbz0) if !ref_smd3[iz].is_nan() => {
                let rr = (r.zr0 + iz as f64 * r.zg).max(1e-9);
                ref_smd3[iz] - 20.0 * rr.log10() + atmos.unwrap_or(0.0) * rr - dbz0
            }
            _ => f64::NAN,
        })
        .collect();

    // The met signal reads the raw fields — φ before unfolding.
    let met = if metsignal {
        let pick_z = |field: &[f64], i: usize| -> f64 {
            let d = r.dr0 + i as f64 * r.dg;
            index_into(d, r.zr0, r.zg, field.len())
                .map(|iz| field[iz])
                .unwrap_or(f64::NAN)
        };
        let z_dp: Vec<f64> = (0..n).map(|i| pick_z(&r.z, i)).collect();
        let snr_dp: Vec<f64> = (0..n).map(|i| pick_z(&snr_z, i)).collect();
        let mut met = find_met_signal(&z_dp, &r.vel, &c.zdr, &r.rho, &r.phi, &snr_dp);
        clean_met_signal(&mut met, MET_SIG_THRESHOLD);
        if let Some(cappi) = cappi {
            cappi.apply_radial(c.elev, r.az, r.dr0, r.dg, &mut met);
        }
        Some(met)
    } else {
        None
    };

    let mut phi = r.phi.clone();
    match &met {
        Some(met) => unfold_phidp(&mut phi, met, MET_SIG_THRESHOLD, init_fdp),
        None => unfold_phidp(&mut phi, &r.rho, UNFOLD_MIN_RHO, init_fdp),
    }

    // Textures about their own smoothing windows (dpp_process.c order:
    // SD(Z) about the 5-gate mean, before ref_smd is overwritten by the
    // 3-gate one).
    let ref_smd5 = average_filter(&r.z, WINDOW);
    let sd_zh = std_filter(&r.z, &ref_smd5, WINDOW, MAX_DIFF_DBZ);
    let phi_smd9 = average_filter(&phi, SHORT_GATE);
    let sd_phi = std_filter(&phi, &phi_smd9, SHORT_GATE, MAX_DIFF_PHIDP);

    let rho_smd = average_filter(&r.rho, WINDOW);
    let zdr_smd = average_filter(&c.zdr, WINDOW);
    let vel_smd = average_filter(&r.vel, WINDOW);

    let hatt = is_high_attenuation_radial(&r.z, &r.vel, &r.spw, &r.rho);

    // Meteorological flag: the cleaned met signal above threshold (strictly
    // — dpp_process.c zeroes `<=`), or the legacy construction the KDP
    // chain pins.
    let mut flag = vec![false; n];
    match &met {
        Some(met) => {
            for (i, f) in flag.iter_mut().enumerate() {
                *f = met[i] > MET_SIG_THRESHOLD;
            }
        }
        None if hatt && dbz0.is_some() => {
            let ngs = n.min(snr_z.len());
            for (i, f) in flag.iter_mut().enumerate().take(ngs) {
                *f = snr_z[i] >= crate::dpprep::MD_SNR_THRESH && !phi[i].is_nan();
            }
        }
        None => {
            for (i, f) in flag.iter_mut().enumerate() {
                *f = rho_smd[i] >= CORR_THRESH && !phi[i].is_nan();
            }
        }
    }
    let groups = meteo_groups(&flag);

    let mut phi_med = median_filter(&phi, WINDOW);
    for (i, f) in flag.iter().enumerate() {
        if !f {
            phi_med[i] = f64::NAN;
        }
    }
    let phi_short = interpolate(
        &average_filter(&phi_med, SHORT_GATE),
        SHORT_GATE,
        &groups,
        init_fdp,
    );
    let phi_long = interpolate(
        &average_filter(&phi_med, LONG_GATE),
        LONG_GATE,
        &groups,
        init_fdp,
    );

    let kdp9 = kdp_from_phi(&phi_short, SHORT_GATE, r.dg);
    let kdp25 = kdp_from_phi(&phi_long, LONG_GATE, r.dg);

    // z_prcd / zdr_prcd with the ΦDP-driven attenuation corrections
    // (Create_corrected_fields_and_adjust_kdp; the syscals are 0).
    let z_prcd: Vec<f64> = (0..nz)
        .map(|iz| {
            if ref_smd3[iz].is_nan() {
                return f64::NAN;
            }
            let zr = r.zr0 + iz as f64 * r.zg;
            let delta = match index_into(zr, r.dr0, r.dg, n) {
                Some(id) if phi_long[id].is_finite() && phi_long[id] >= init_fdp => {
                    0.04 * (phi_long[id] - init_fdp)
                }
                _ => 0.0,
            };
            ref_smd3[iz] + delta
        })
        .collect();
    let zdr_prcd: Vec<f64> = (0..n)
        .map(|i| {
            if zdr_smd[i].is_nan() {
                return f64::NAN;
            }
            let delta = if phi_long[i].is_finite() && phi_long[i] >= init_fdp {
                0.004 * (phi_long[i] - init_fdp)
            } else {
                0.0
            };
            zdr_smd[i] + delta
        })
        .collect();

    // The merged, censored KDP (the DKDP moment).
    let kdp_merged: Vec<f64> = (0..n)
        .map(|i| {
            if rho_smd[i].is_nan() || rho_smd[i] < CORR_THRESH {
                return f64::NAN;
            }
            let d = r.dr0 + i as f64 * r.dg;
            let zp = index_into(d, r.zr0, r.zg, nz)
                .map(|iz| z_prcd[iz])
                .unwrap_or(f64::NAN);
            if zp.is_finite() && zp > DBZ_THRESH {
                kdp9[i]
            } else {
                kdp25[i]
            }
        })
        .collect();

    // Moment transport: sample the z-gate fields at each DP gate, key
    // presence on the raw input (Add_moment's `inp`), quantize the 8-bit
    // fields, and land in the sentinel domain.
    let q8 = |v: f64, s: (f64, f64)| if quantize { transport8(v, s) } else { v };
    let mut fields = Fields {
        az: r.az,
        elev: c.elev,
        hatt,
        n,
        dg: r.dg,
        smz: Vec::with_capacity(n),
        snr: Vec::with_capacity(n),
        sdz: Vec::with_capacity(n),
        zdr: Vec::with_capacity(n),
        rho: Vec::with_capacity(n),
        kdp: Vec::with_capacity(n),
        phi: Vec::with_capacity(n),
        sdp: Vec::with_capacity(n),
        smv: Vec::with_capacity(n),
        // The DMET moment (8-bit, scale 2 / offset 50) — what qperate's
        // usability check reads downstream; NaN when the legacy flag ran.
        met: match &met {
            Some(m) => m
                .iter()
                .map(|&v| {
                    if quantize {
                        transport8(v, (2.0, 50.0))
                    } else {
                        v
                    }
                })
                .collect(),
            None => vec![f64::NAN; n],
        },
        q: Vec::with_capacity(n),
    };
    for i in 0..n {
        let d = r.dr0 + i as f64 * r.dg;
        let zi = index_into(d, r.zr0, r.zg, nz);
        let z_present = zi.map(|iz| !r.z[iz].is_nan()).unwrap_or(false);
        // Quantize in the NaN domain (transport8 keeps NaN as NaN, i.e. an
        // undefined field value encodes level 0), sentinel afterwards.
        let pick_z = |field: &[f64]| -> f64 { zi.map(|iz| field[iz]).unwrap_or(f64::NAN) };
        fields.smz.push(if z_present {
            sentinel(q8(pick_z(&z_prcd), SMZ_SCALE))
        } else {
            NO_DATA
        });
        fields.snr.push(if z_present {
            sentinel(q8(pick_z(&snr_z), SNR_SCALE))
        } else {
            NO_DATA
        });
        fields.sdz.push(if z_present {
            sentinel(q8(pick_z(&sd_zh), SDZ_SCALE))
        } else {
            NO_DATA
        });

        let zdr_present = !c.zdr.get(i).copied().unwrap_or(f64::NAN).is_nan();
        fields.zdr.push(if zdr_present {
            sentinel(q8(zdr_prcd[i], ZDR_SCALE))
        } else {
            NO_DATA
        });

        let phi_present = !r.phi[i].is_nan();
        fields.rho.push(if !r.rho[i].is_nan() {
            sentinel(rho_smd[i])
        } else {
            NO_DATA
        });
        fields.kdp.push(if phi_present {
            sentinel(kdp_merged[i])
        } else {
            NO_DATA
        });
        fields.phi.push(if phi_present {
            sentinel(phi_long[i])
        } else {
            NO_DATA
        });
        fields.sdp.push(if phi_present {
            sentinel(q8(sd_phi[i], SDP_SCALE))
        } else {
            NO_DATA
        });

        let vel_raw = r.vel.get(i).copied().unwrap_or(f64::NAN);
        fields.smv.push(if !vel_raw.is_nan() {
            sentinel(q8(vel_smd.get(i).copied().unwrap_or(f64::NAN), SMV_SCALE))
        } else {
            NO_DATA
        });

        fields.q.push(quality_indices(
            fields.phi[i],
            fields.rho[i],
            fields.smz[i],
            fields.snr[i],
            quantize,
        ));
    }
    fields
}

/// `Qia_process_radial`'s six indices for one gate, in fuzzy-logic input
/// order (SMZ, ZDR, LKDP, RHO, SDZ, SDP). Inputs are the transported
/// fields, sentinel domain; the arithmetic runs exactly as the C does —
/// a `NO_DATA` φ of −10⁵ squares into an index of exactly 0.
fn quality_indices(phi: f64, rho: f64, smz: f64, snr: f64, quantize: bool) -> [f64; 6] {
    let linear_snr = 10f64.powf(0.1 * snr);
    let ac = phi / PHI_DP_Z_THRESH;
    let bc = 1.0 / linear_snr;
    let cc = phi / PHI_DP_ZDR_THRESH;
    let mut dc = (1.0 - rho) / DELTA_RHO_1_THRESHOLD;
    let ec = LINEAR_SNR_ZDR_THRESH / linear_snr;
    let fc = phi / PHI_DP_PHI_THRESH;
    let hc = 1.0 / linear_snr;
    let ic = phi / PHI_DP_KDP_THRESH;
    let lc = 1.0 / linear_snr;
    if rho < RHO_MIN_THRESH && smz < Z_ATTEN_THRESH {
        dc = 0.0;
    }
    let fix = |q: f64| if q.is_finite() { q } else { 0.0 };
    let mut q = [
        fix((QIA_C * (ac * ac + bc * bc)).exp()),
        fix((QIA_C * (cc * cc + dc * dc + ec * ec)).exp()),
        fix((QIA_C * (ic * ic + dc * dc + hc * hc)).exp()),
        fix((QIA_C * (fc * fc + dc * dc + hc * hc)).exp()),
        fix((QIA_C * (lc * lc)).exp()),
        fix((QIA_C * (hc * hc)).exp()),
    ];
    if quantize {
        for v in q.iter_mut() {
            *v = transport8(*v, (Q_SCALE, Q_OFFSET));
        }
    }
    q
}

/// One gate through `Hca_process_radial`'s classification: returns the
/// internal class index. `tw0_km_arl` feeds the HSDA modification of RH's
/// ZDR membership.
fn classify_gate(f: &Fields, bin: usize, ml: MlBins, tw0_km_arl: f64) -> usize {
    if f.snr[bin] < MIN_SNR {
        return NE;
    }
    // (The RF → UK branch is unreachable here; see the module doc.)

    let z_fshield = f.smz[bin]; // no blockage: FShield adjustment is 0
    // `RPGCS_height(bin·dg, elev)` — the bin height the HSDA membership
    // modification reads (the C measures range from bin 0, not `dr0`).
    let height_km = ml_height_from_range(f.elev, bin as f64 * f.dg);

    let mut agg = [0.0f64; NUM_CLASSES];
    allowed_hydro_class(
        bin as i64, f.smz[bin], f.zdr[bin], f.rho[bin], f.phi[bin], f.smv[bin], f.hatt, &mut agg,
        ml,
    );

    let lkdp = if f.kdp[bin] >= 0.001 {
        10.0 * f.kdp[bin].log10()
    } else {
        MINI_LKTP
    };
    let mut d = [0.0f64; NUM_FL_INPUTS];
    d[SMZ] = z_fshield;
    d[ZDR] = f.zdr[bin];
    d[LKDP] = lkdp;
    d[RHO] = f.rho[bin];
    d[SDZ] = f.sdz[bin];
    d[SDP] = f.sdp[bin];
    let quality = f.q[bin];

    for (h_class, a) in agg.iter_mut().enumerate() {
        if *a == -1.0 {
            *a = 0.0;
            continue;
        }
        // U0/U1/UK/NE carry all-zero weights in the adaptation data, so
        // their aggregations are identically 0 — skip the arithmetic.
        if !(RA..=GR).contains(&h_class) {
            continue;
        }
        let mut fd_mem = [0.0f64; 6];
        for (fl_input, fd) in fd_mem.iter_mut().enumerate() {
            let points = set_membership_points(h_class, fl_input, z_fshield, height_km, tw0_km_arl);
            *fd = degree_membership(d[fl_input], points);
        }
        *a = weighted_aggregation(&WEIGHT[h_class - RA], &quality, &fd_mem);
    }

    // The largest aggregation wins (first index on ties, as the C's strict
    // `<` keeps the earlier class), then the min_Agg gate; a margin under
    // min_Dif_Agg goes to the AEL Table 4 tie-break (B21; B16 read UK).
    let mut agg_max = -2.0;
    let mut max_cal = NE;
    for (h_class, &a) in agg.iter().enumerate() {
        if agg_max < a {
            agg_max = a;
            max_cal = h_class;
        }
    }
    let mut top_diff = 100.0;
    let mut runner_up = UK;
    for (h_class, &a) in agg.iter().enumerate() {
        if h_class != max_cal {
            let diff = agg_max - a;
            if diff < top_diff {
                top_diff = diff;
                runner_up = h_class;
            }
        }
    }
    if agg_max < MIN_AGG {
        return UK;
    }
    if top_diff < MIN_DIF_AGG {
        return break_tie(bin as i64, ml, max_cal, runner_up);
    }
    max_cal
}

/// One radial's classes.
pub(crate) fn classify_radial(f: &Fields, ml: &MeltingLayer, tw0_km_arl: f64) -> Vec<usize> {
    let az = (f.az.rem_euclid(360.0)) as usize % 360;
    let bins = beam_ml_intersection(f.elev, az, f.dg, ml);
    (0..f.n)
        .map(|bin| classify_gate(f, bin, bins, tw0_km_arl))
        .collect()
}

// ── Hail size discrimination (HailSize.cpp v3) ───────────────────────────────

/// The RH subclassification (`data.sub`): `Current` is an RH gate the HSDA
/// left at rain-and-hail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HailSize {
    NotHail,
    Current,
    Small,
    Large,
    Giant,
}

/// One height regime's three (Z, ZDR, ρ) trapezoids, small/large/giant.
type HsdaTraps = [[[f64; 4]; 3]; 3];

/// `HailSize_v3`'s inline trapezoids for one gate: the six height regimes
/// against the wet-bulb heights, the ZDR rows of the lower regimes built
/// from the hail-size `f`/`g` polynomials at the gate's Z (all carrying
/// `DeltaZdr = −0.5`). Returns the regime's (weights, trapezoids).
fn hsda_regime(height_km: f64, hs: &HsdaHeights, z: f64) -> ([f64; 3], HsdaTraps) {
    let dz = HSDA_DELTA_ZDR;
    let f1 = -0.5 + 2.5e-3 * z + 7.5e-4 * z * z + dz;
    let f2 = 0.1 * (z - 50.0) + dz;
    let f3 = 0.1 * (z - 60.0) + dz;
    let g1 = -0.9 + 1.5e-2 * z + 5.0e-4 * z * z + dz;
    let g2 = 0.075 * (z - 50.0) + dz;
    let g3 = 0.075 * (z - 60.0) + dz;
    let (zmin, rmin, zmax) = (HSDA_MIN_ZDR, HSDA_MIN_RHO, HSDA_MAX_Z);
    let (tw0, twm25) = (hs.tw0_km_arl, hs.twm25_km_arl);

    if height_km > twm25 {
        (
            [1.0, 0.3, 0.6],
            [
                [
                    [45.0, 50.0, 60.0, 65.0],
                    [-0.5, -0.3, 0.3, 0.5],
                    [0.92, 0.96, 0.99, 1.0],
                ],
                [
                    [48.0, 58.0, 63.0, 68.0],
                    [-0.5, -0.3, 0.3, 0.5],
                    [0.92, 0.96, 0.99, 1.0],
                ],
                [
                    [50.0, 60.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, 0.3, 0.5],
                    [rmin - 1.0, rmin, 0.99, 1.0],
                ],
            ],
        )
    } else if height_km > tw0 {
        (
            [1.0, 0.3, 0.6],
            [
                [
                    [45.0, 50.0, 60.0, 65.0],
                    [-0.5, -0.3, 0.3, 0.5],
                    [0.92, 0.96, 0.99, 1.0],
                ],
                [
                    [48.0, 58.0, 63.0, 68.0],
                    [-0.5, -0.3, 0.3, 0.5],
                    [0.86, 0.90, 0.96, 0.98],
                ],
                [
                    [50.0, 60.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, 0.2, 0.5],
                    [rmin - 1.0, rmin, 0.93, 0.98],
                ],
            ],
        )
    } else if height_km > tw0 - 1.0 {
        (
            [0.8, 0.5, 0.6],
            [
                [
                    [45.0, 50.0, 60.0, 65.0],
                    [-0.1, 0.3, 0.7, 1.2],
                    [0.93, 0.96, 0.99, 1.0],
                ],
                [
                    [48.0, 58.0, 63.0, 68.0],
                    [-0.3, 0.1, 0.5, 1.0],
                    [0.80, 0.91, 0.97, 0.98],
                ],
                [
                    [50.0, 60.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, 0.2, 0.7],
                    [rmin - 1.0, rmin, 0.94, 0.98],
                ],
            ],
        )
    } else if height_km > tw0 - 2.0 {
        (
            [0.7, 0.8, 0.6],
            [
                [
                    [45.0, 52.0, 62.0, 67.0],
                    [g2 - 0.3, g2, g1, g1 + 0.3],
                    [0.94, 0.96, 0.98, 1.0],
                ],
                [
                    [50.0, 60.0, 65.0, 70.0],
                    [g3 - 0.3, g3, g2, g2 + 0.3],
                    [0.80, 0.91, 0.97, 0.98],
                ],
                [
                    [52.0, 62.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, g3, g3 + 0.3],
                    [rmin - 1.0, rmin, 0.96, 0.98],
                ],
            ],
        )
    } else if height_km > tw0 - 3.0 {
        (
            [0.7, 1.0, 0.6],
            [
                [
                    [45.0, 49.0, 59.0, 64.0],
                    [f2 - 0.3, f2, f1, f1 + 0.3],
                    [0.91, 0.94, 0.96, 0.99],
                ],
                [
                    [50.0, 57.0, 62.0, 67.0],
                    [f3 - 0.3, f3, f2, f2 + 0.3],
                    [0.80, 0.93, 0.96, 0.99],
                ],
                [
                    [50.0, 59.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, f3, f3 + 0.3],
                    [rmin - 1.0, rmin, 0.93, 0.98],
                ],
            ],
        )
    } else {
        (
            [0.7, 1.0, 0.6],
            [
                [
                    [45.0, 47.0, 57.0, 62.0],
                    [f2 - 0.3, f2, f1, f1 + 0.3],
                    [0.91, 0.94, 0.96, 0.99],
                ],
                [
                    [50.0, 55.0, 60.0, 65.0],
                    [f3 - 0.3, f3, f2, f2 + 0.3],
                    [0.80, 0.93, 0.96, 0.99],
                ],
                [
                    [50.0, 57.0, zmax, zmax + 1.0],
                    [zmin - 1.0, zmin, f3, f3 + 0.3],
                    [rmin - 1.0, rmin, 0.93, 0.98],
                ],
            ],
        )
    }
}

/// `HailSize_v3` over one radial: subclassify the RH gates by hail size.
/// Inputs are the classified radial's fields (sentinel domain — a missing
/// ZDR at −10⁵ falls off every trapezoid, exactly as the C's `no_data`
/// does) and the QIA indices for Z, ZDR and ρ. The despeckle demotes
/// giant→large then large→small runs shorter than `min_data_size`.
fn hail_size_radial(f: &Fields, classes: &[usize], hs: &HsdaHeights) -> Vec<HailSize> {
    use crate::dpprep::trap4;
    let mut sub: Vec<HailSize> = classes
        .iter()
        .map(|&c| {
            if c == RH {
                HailSize::Current
            } else {
                HailSize::NotHail
            }
        })
        .collect();

    for (i, cell) in sub.iter_mut().enumerate().take(f.n) {
        if *cell != HailSize::Current {
            continue;
        }
        let z = f.smz[i];
        let zdr = f.zdr[i];
        let rho = f.rho[i];
        let height_km = ml_height_from_range(f.elev, i as f64 * f.dg);
        let (w, traps) = hsda_regime(height_km, hs, z);
        let q = [f.q[i][SMZ], f.q[i][ZDR], f.q[i][RHO]];

        let mut agg = [0.0f64; 3];
        for (s, a) in agg.iter_mut().enumerate() {
            let t = &traps[s];
            let pv = [
                trap4(z, t[0][0], t[0][1], t[0][2], t[0][3]),
                trap4(zdr, t[1][0], t[1][1], t[1][2], t[1][3]),
                trap4(rho, t[2][0], t[2][1], t[2][2], t[2][3]),
            ];
            let sum_weights = w[0] * q[0] + w[1] * q[1] + w[2] * q[2];
            *a = (w[0] * pv[0] * q[0] + w[1] * pv[1] * q[1] + w[2] * pv[2] * q[2]) / sum_weights;
            // The "handcuffs": large and giant need every input to carry
            // at least some membership.
            if s != 0 && (pv[0] < HSDA_MIN_PV || pv[1] < HSDA_MIN_PV || pv[2] < HSDA_MIN_PV) {
                *a = 0.0;
            }
        }

        // Strict `>` keeps the earlier (smaller) size on ties; a NaN
        // aggregation (all-zero qualities) selects nothing, as in the C.
        let mut max_value = -1.0f64;
        let mut max_index = 0usize;
        for (s, &a) in agg.iter().enumerate() {
            if a > max_value {
                max_value = a;
                max_index = s;
            }
        }
        if max_value >= HSDA_MIN_AGG {
            // max_hail_cat is pinned at giant in the released source, so
            // the category caps never bind.
            *cell = match max_index {
                0 => HailSize::Small,
                1 => HailSize::Large,
                _ => HailSize::Giant,
            };
        }
        // Hard limit: high ZDR is never large/giant hail.
        if zdr >= HSDA_MAX_ZDR {
            *cell = HailSize::Small;
        }
    }

    despeckle_hail(&mut sub, HailSize::Giant, HailSize::Large);
    despeckle_hail(&mut sub, HailSize::Large, HailSize::Small);
    sub
}

/// One gate's product code: `dualpol8bit.c`'s `Class_external` with the RH
/// subclass split (`EXT_LH`/`EXT_GH`; small hail and unsized RH keep RH's
/// 100). Codes of 0 (U0/U1/NE) are undefined.
fn external_code(class: usize, size: HailSize) -> f32 {
    let code = if class == RH {
        match size {
            HailSize::Large => EXT_LH,
            HailSize::Giant => EXT_GH,
            _ => CLASS_EXTERNAL[RH],
        }
    } else {
        CLASS_EXTERNAL[class]
    };
    if code == 0.0 { f32::NAN } else { code }
}

/// One despeckle pass: runs of `from` shorter than `min_data_size` become
/// `to`. The trailing run is flushed by the loop's else-arm never firing —
/// the C leaves it standing, and so does this.
fn despeckle_hail(sub: &mut [HailSize], from: HailSize, to: HailSize) {
    let mut short_runs: Vec<(usize, usize)> = Vec::new();
    let mut beg: Option<usize> = None;
    let mut count = 0usize;
    for (i, &cur) in sub.iter().enumerate() {
        if cur == from {
            if beg.is_none() {
                beg = Some(i);
            }
            count += 1;
        } else {
            if let Some(b) = beg
                && count < MIN_DATA_SIZE
            {
                short_runs.push((b, i));
            }
            beg = None;
            count = 0;
        }
    }
    for (b, e) in short_runs {
        for cell in sub[b..e].iter_mut() {
            *cell = to;
        }
    }
}

// ── Public entry points ──────────────────────────────────────────────────────

/// The conventions [`compute_hca`] pins; the harness varies them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HcaOptions {
    /// Seed the pipeline from the volume estimate before the RDA header
    /// value — the `isdp_apply = YES` reading (see [`crate::kdp`]'s ISDP
    /// finding). Off is the documented default.
    pub(crate) isdp_estimated: bool,
    /// Reproduce the 8-bit moment transport between tasks (the primary).
    /// Off is the naive physical-units reading.
    pub(crate) quantize_transport: bool,
    /// The B21 met-signal meteorological flag (`metsignal_processing = ON`,
    /// the fleet default and the primary). Off is the legacy (pre-B17)
    /// ρ/SNR flag the KDP chain's survey record was measured with.
    pub(crate) metsignal: bool,
}

impl HcaOptions {
    pub(crate) const fn primary() -> Self {
        Self {
            isdp_estimated: false,
            quantize_transport: true,
            metsignal: true,
        }
    }
}

/// The derived hydrometeor classification for one tilt, at the recombined
/// radials' native geometry.
pub struct DerivedHca {
    /// `[radial][gate]`, the product's external class codes (10–140);
    /// `NaN` where the gate is no-echo/undefined (external code 0).
    pub values: Vec<Vec<f32>>,
    /// Centre azimuth per radial, degrees.
    pub azimuths_deg: Vec<f64>,
    /// Range to the centre of gate 0, km.
    pub first_gate_km: f64,
    pub gate_interval_km: f64,
    /// Angular width of one radial, degrees.
    pub radial_width_deg: f64,
    /// The initial system phase actually used, for the record.
    pub init_fdp_deg: f64,
}

impl DerivedHca {
    /// Resample onto the 360° × 230 km comparison grid, cell for cell the
    /// way the twin comparator resamples the Level III product.
    pub fn to_polar_grid(&self) -> Vec<Vec<f32>> {
        resample_to_polar_grid(
            &self.values,
            &self.azimuths_deg,
            self.first_gate_km,
            self.gate_interval_km,
            self.radial_width_deg,
        )
    }
}

/// The `init_fdp` the pipeline seeds with — the same resolution the KDP
/// chain validated: the RDA header value, else the volume estimate; the
/// `isdp_apply = YES` variant prefers the estimate.
pub(crate) fn resolve_init_fdp(
    params: &KdpParams,
    combined: &[DpCombined],
    estimated: bool,
) -> f64 {
    let estimate = || {
        let mut queue: Vec<f64> = Vec::new();
        for c in combined {
            if queue.len() >= crate::dpprep::ISDP_MAX_QUEUE {
                break;
            }
            if let Some(p) = radial_system_phi(&c.base.phi, &c.base.rho, &c.base.z) {
                queue.push(p);
            }
        }
        isdp_from_queue(queue)
    };
    if estimated {
        params
            .isdp_est_deg
            .map(f64::from)
            .or_else(estimate)
            .or(params.init_fdp_deg.map(f64::from))
            .unwrap_or(0.0)
    } else {
        params
            .init_fdp_deg
            .map(f64::from)
            .or_else(estimate)
            .unwrap_or(0.0)
    }
}

/// Compute the tilt's hydrometeor classification per the rules in the
/// module doc: recombine the sweep to 1°, run the dpprep (met-signal) and
/// QIA chains, classify every gate against the melting layer, subclass RH
/// by hail size, and emit the product's external class codes. `None` when
/// no radial carries the differential phase moment.
///
/// `params` carries the radial-header values ([`KdpParams::from_archive`]);
/// without `dbz0` the SNR gate cannot run and every gate reads no-echo,
/// exactly as the operational chain would with no calibration constant.
/// `hsda` carries the wet-bulb heights; `cappi` the volume's reflectivity
/// CAPPI ([`build_refl_cappi`]) — `None` is the cold-start state, which
/// only differs on ≥ 1° tilts.
pub fn compute_hca(
    radials: &[Radial],
    params: &KdpParams,
    ml: &MeltingLayer,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
) -> Option<DerivedHca> {
    compute_hca_impl(radials, params, ml, hsda, cappi, HcaOptions::primary())
}

/// Build the volume's reflectivity CAPPI from its ≥ 1° dual-pol sweeps —
/// the state [`compute_hca`]'s met-signal chain consults (see the
/// [`crate::dpprep`] module doc's CAPPI notes). Sweeps must be given in
/// scan order, as the RPG fills the grid.
pub fn build_refl_cappi(sweeps: &[&[Radial]]) -> ReflCappi {
    let mut cappi = ReflCappi::new();
    for &radials in sweeps {
        let inputs: Vec<DpInput> = radials.iter().filter_map(DpInput::from_radial).collect();
        if inputs.is_empty() {
            continue;
        }
        let combined = combine_sweep_dp(&inputs, true);
        for c in &combined {
            cappi.update_radial(c.elev, c.base.az, c.base.zr0, c.base.zg, &c.base.z);
        }
    }
    cappi
}

fn compute_hca_impl(
    radials: &[Radial],
    params: &KdpParams,
    ml: &MeltingLayer,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
    opts: HcaOptions,
) -> Option<DerivedHca> {
    let inputs: Vec<DpInput> = radials.iter().filter_map(DpInput::from_radial).collect();
    if inputs.is_empty() {
        return None;
    }
    let radial_width_deg = if inputs[0].half_degree {
        1.0
    } else {
        inputs[0].spacing
    };
    let combined = combine_sweep_dp(&inputs, true);
    let init_fdp = resolve_init_fdp(params, &combined, opts.isdp_estimated);

    let geometry = combined.iter().find(|c| !c.base.phi.is_empty())?;
    let first_gate_km = geometry.base.dr0;
    let gate_interval_km = geometry.base.dg;

    let dbz0 = params.dbz0.map(f64::from);
    let atmos = params.atmos_db_per_km.map(f64::from);

    // One radial at a time, all at once. `radial_fields`, `classify_radial` and
    // `hail_size_radial` read this radial and the volume state around it and
    // write nothing else; the output is one row per radial, in `combined`'s
    // order, which rayon's `map`/`collect` keeps exactly as `into_iter` did.
    // Nothing is summed across radials, so no float is reassociated and the
    // product is the serial one gate for gate —
    // [`tests::the_pool_classifies_a_volume_the_way_one_thread_does`].
    let (values, azimuths): (Vec<Vec<f32>>, Vec<f64>) = combined
        .par_iter()
        .map(|c| {
            let fields = radial_fields(
                c,
                init_fdp,
                dbz0,
                atmos,
                opts.quantize_transport,
                opts.metsignal,
                cappi,
            );
            let classes = classify_radial(&fields, ml, hsda.tw0_km_arl);
            let sub = if ENABLE_SIZE {
                hail_size_radial(&fields, &classes, hsda)
            } else {
                vec![HailSize::NotHail; classes.len()]
            };
            let row: Vec<f32> = classes
                .iter()
                .zip(sub.iter())
                .map(|(&cl, &s)| external_code(cl, s))
                .collect();
            (row, c.base.az)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .unzip();

    Some(DerivedHca {
        values,
        azimuths_deg: azimuths,
        first_gate_km,
        gate_interval_km,
        radial_width_deg,
        init_fdp_deg: init_fdp,
    })
}

/// Rebuild a split cut the way the RPG's **combined base data** stream
/// feeds dpprep/HCA: the surveillance cut's Z and dual-pol moments with the
/// Doppler cut's velocity and spectrum width grafted in, radial by radial
/// (nearest azimuth). The archive keeps the two half-cuts as separate
/// sweeps; the operational chain classifies the combination — without it
/// the GC velocity kill (`min_V_GC`) is inert on the surveillance tilt.
///
/// Surveillance radials that already carry velocity pass through unchanged;
/// a Doppler radial farther than half a spacing away contributes nothing.
pub fn merge_split_cut_doppler(surveillance: &[Radial], doppler: &[Radial]) -> Vec<Radial> {
    let dop: Vec<(f64, &Radial)> = doppler
        .iter()
        .filter(|r| r.velocity().is_some())
        .map(|r| (f64::from(r.azimuth_angle_degrees()), r))
        .collect();
    let circ = |a: f64, b: f64| -> f64 {
        let mut d = (a - b).rem_euclid(360.0);
        if d > 180.0 {
            d = 360.0 - d;
        }
        d
    };
    surveillance
        .iter()
        .map(|cs| {
            if cs.velocity().is_some() || dop.is_empty() {
                return cs.clone();
            }
            let az = f64::from(cs.azimuth_angle_degrees());
            let partner = dop
                .iter()
                .min_by(|(a, _), (b, _)| circ(*a, az).total_cmp(&circ(*b, az)))
                .filter(|(a, _)| circ(*a, az) <= 0.5 * f64::from(cs.azimuth_spacing_degrees()))
                .map(|(_, r)| *r);
            let Some(cd) = partner else {
                return cs.clone();
            };
            Radial::new(
                cs.collection_timestamp(),
                cs.azimuth_number(),
                cs.azimuth_angle_degrees(),
                cs.azimuth_spacing_degrees(),
                cs.radial_status(),
                cs.elevation_number(),
                cs.elevation_angle_degrees(),
                cs.reflectivity().cloned(),
                cd.velocity().cloned(),
                cd.spectrum_width().cloned(),
                cs.differential_reflectivity().cloned(),
                cs.differential_phase().cloned(),
                cs.correlation_coefficient().cloned(),
                None,
            )
        })
        .collect()
}

// ── Melting layer detection (cpc023/tsk003, melting_layer.c) ─────────────────

/// `Compute_height_from_range`: beam height above the radar, km, on the
/// `IR·RE` model.
fn ml_height_from_range(elev_deg: f64, range_km: f64) -> f64 {
    let s = elev_deg.to_radians().sin();
    range_km * s + range_km * range_km / (2.0 * ML_IR * ML_RE_KM)
}

/// `Compute_range_from_height`, its inverse.
fn ml_range_from_height(elev_deg: f64, height_km: f64) -> f64 {
    let s = elev_deg.to_radians().sin();
    ML_IR * ML_RE_KM * ((s * s + 2.0 * height_km / (ML_IR * ML_RE_KM)).sqrt() - s)
}

/// `Compute_elev_weight`: the gate-count × reliability weighting of a
/// detection at `elev`.
fn ml_elev_weight(elev_deg: f64) -> f64 {
    let gate_ratio = 0.36 * elev_deg - 0.56;
    let acc_ratio = 1.0 - (ML_UPPER_ELEV - elev_deg) / ML_UPPER_ELEV;
    gate_ratio * acc_ratio
}

/// Detect the melting layer from one volume's 4°–10° tilts per
/// `melting_layer.c` (Giangrande, Krause, Ryzhkov 2008), classifying those
/// tilts with the flat default layer first — the operational chain's own
/// first-volume state. Azimuths whose accumulated wet-snow weight misses
/// `min_wet_snow_sum` interpolate between valid neighbours; with no valid
/// azimuth (or a single one) the default flat layer is returned.
///
/// The operational deltas — 3-volume accumulation and the RUC/RAP model
/// merge — are catalogued in the module doc.
pub fn detect_melting_layer(
    sweeps: &[&[Radial]],
    params: &KdpParams,
    default_top_km_arl: f64,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
) -> MeltingLayer {
    detect_melting_layer_impl(
        sweeps,
        params,
        default_top_km_arl,
        hsda,
        cappi,
        HcaOptions::primary(),
    )
}

fn detect_melting_layer_impl(
    sweeps: &[&[Radial]],
    params: &KdpParams,
    default_top_km_arl: f64,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
    opts: HcaOptions,
) -> MeltingLayer {
    let default = MeltingLayer::flat(default_top_km_arl);
    let dbz0 = params.dbz0.map(f64::from);
    let atmos = params.atmos_db_per_km.map(f64::from);

    let mut weight = vec![[0.0f64; ML_MAX_HEIGHTS]; 360];
    for &radials in sweeps {
        let inputs: Vec<DpInput> = radials.iter().filter_map(DpInput::from_radial).collect();
        if inputs.is_empty() {
            continue;
        }
        let sweep_elev = inputs[0].elev;
        if !(ML_LOWER_ELEV..=ML_UPPER_ELEV).contains(&sweep_elev) {
            continue;
        }
        let combined = combine_sweep_dp(&inputs, true);
        let init_fdp = resolve_init_fdp(params, &combined, opts.isdp_estimated);
        let elev_weight = ml_elev_weight(sweep_elev);

        // **Which heights each radial votes for, found in parallel; the votes
        // themselves cast in order.**
        //
        // Finding them is per-radial and pure — the classification, the wet-snow
        // window and the 0.5 km Z/ZDR search read one radial and the flat
        // default layer. Casting them is not: `weight` is a float accumulator,
        // several radials of a sweep round to the same whole degree, and `+=`
        // over floats is not associative, so a thread order would decide the
        // last bit of a sum. The map therefore hands back each radial's height
        // indices in gate order, and the serial loop below adds them in
        // `combined`'s order — the order the fused loop added them in — so the
        // accumulation is not merely equivalent but identical.
        let votes: Vec<(usize, Vec<usize>)> = combined
            .par_iter()
            .map(|c| {
                let f = radial_fields(
                    c,
                    init_fdp,
                    dbz0,
                    atmos,
                    opts.quantize_transport,
                    opts.metsignal,
                    cappi,
                );
                let classes = classify_radial(&f, &default, hsda.tw0_km_arl);
                let stop = (ml_range_from_height(c.elev, ML_MAX_TOP_KM) / f.dg + 0.5) as usize;
                let az_index = (f.az.rem_euclid(360.0)) as usize % 360;
                let mut heights = Vec::new();
                for (i, &class) in classes.iter().enumerate().take(f.n.min(stop)) {
                    if class == GC || class == BI || class == UK || class == NE {
                        continue;
                    }
                    if f.snr[i] <= ML_MIN_SNR {
                        continue;
                    }
                    if !(f.smz[i] > ML_LOWER_Z
                        && f.smz[i] < ML_UPPER_Z
                        && f.rho[i] > ML_LOWER_RHO
                        && f.rho[i] < ML_UPPER_RHO)
                    {
                        continue;
                    }
                    let height_index = (ml_height_from_range(c.elev, i as f64 * f.dg)
                        / ML_HEIGHT_INTERVAL_KM
                        + 0.5) as usize;
                    if height_index >= ML_MAX_HEIGHTS {
                        continue;
                    }
                    // Search up to 0.5 km above this gate for the Z and ZDR
                    // maxima that fingerprint wet snow.
                    let temp_height = ML_DEPTH_KM + ml_height_from_range(c.elev, i as f64 * f.dg);
                    let range_index = ((ml_range_from_height(c.elev, temp_height) / f.dg + 0.5)
                        as usize)
                        .min(f.n);
                    let (mut zmax, mut zdrmax) = (-1000.0f64, -1000.0f64);
                    let (mut zmax_i, mut zdrmax_i) = (i, i);
                    for j in i..range_index {
                        if f.snr[j] > ML_MIN_SNR {
                            if zmax < f.smz[j] {
                                zmax = f.smz[j];
                                zmax_i = j;
                            }
                            if zdrmax < f.zdr[j] {
                                zdrmax = f.zdr[j];
                                zdrmax_i = j;
                            }
                        }
                    }
                    if zmax > ML_LOWER_ZMAX
                        && zmax < ML_UPPER_ZMAX
                        && f.rho[zmax_i] > ML_LOW_RHO_PROFILE
                        && zdrmax > ML_LOWER_ZDRMAX
                        && zdrmax < ML_UPPER_ZDRMAX
                        && f.rho[zdrmax_i] > ML_LOW_RHO_PROFILE
                    {
                        heights.push(height_index);
                    }
                }
                (az_index, heights)
            })
            .collect();

        for (az_index, heights) in votes {
            for height_index in heights {
                weight[az_index][height_index] += 1.0 + elev_weight;
            }
        }
    }

    calculate_melting_layer(&weight, default_top_km_arl, &default)
}

/// `Calculate_melting_layer`'s radar-only path over one accumulation of
/// wet-snow weights: the ±10° azimuth sums, the ±(2·depth) clip around the
/// previous top (the default top here — first-volume state), the 20th/80th
/// percentile bottom/top, gap interpolation around the circle.
fn calculate_melting_layer(
    weight: &[[f64; ML_MAX_HEIGHTS]],
    last_avg_top: f64,
    default: &MeltingLayer,
) -> MeltingLayer {
    let mut top = [f64::NAN; 360];
    let mut bottom = [f64::NAN; 360];

    let clip_high = ((last_avg_top + 2.0 * ML_DEPTH_KM) / ML_HEIGHT_INTERVAL_KM + 0.5) as i64;
    let clip_low = ((last_avg_top - 2.0 * ML_DEPTH_KM) / ML_HEIGHT_INTERVAL_KM + 0.5) as i64;

    for az in 0..360usize {
        let mut sum_heights = [0.0f64; ML_MAX_HEIGHTS];
        for d in -(ML_HALF_WINDOW as i64)..=(ML_HALF_WINDOW as i64) {
            let j = (az as i64 + d).rem_euclid(360) as usize;
            for (k, s) in sum_heights.iter_mut().enumerate() {
                *s += weight[j][k];
            }
        }
        // Zero out heights more than 2·depth from the previous top.
        for (k, s) in sum_heights.iter_mut().enumerate() {
            if (k as i64) < clip_low || (k as i64) > clip_high {
                *s = 0.0;
            }
        }
        let total: f64 = sum_heights.iter().sum();
        if total <= ML_MIN_WET_SNOW_SUM {
            continue;
        }
        let mut running = 0.0;
        let (mut low_index, mut high_index) = (-1i64, -1i64);
        for (k, &s) in sum_heights.iter().enumerate() {
            running += s;
            let statistic = running / total;
            if statistic > ML_LOW_PERCENTILE && low_index == -1 {
                low_index = k as i64;
            }
            if statistic > ML_HIGH_PERCENTILE && high_index == -1 {
                high_index = k as i64;
            }
            if low_index > 0 && high_index > 0 {
                break;
            }
        }
        top[az] = high_index as f64 * ML_HEIGHT_INTERVAL_KM + 0.05;
        bottom[az] = low_index as f64 * ML_HEIGHT_INTERVAL_KM + 0.05;
    }

    let valid: Vec<usize> = (0..360).filter(|&i| !top[i].is_nan()).collect();
    if valid.len() < 2 {
        // No radar detection (or a degenerate single azimuth): the default
        // flat layer, as the source's `ML_not_found` path sends.
        return default.clone();
    }

    // Fill the gaps by linear interpolation between the bracketing valid
    // azimuths, around the circle — the source's Valid_radar_index walk.
    let mut out_top = top;
    let mut out_bottom = bottom;
    for w in 0..valid.len() {
        let a = valid[w];
        let b = valid[(w + 1) % valid.len()];
        let span = ((b as i64 - a as i64).rem_euclid(360)) as usize;
        if span <= 1 {
            continue;
        }
        for step in 1..span {
            let az = (a + step) % 360;
            let t = step as f64 / span as f64;
            out_top[az] = top[a] * (1.0 - t) + top[b] * t;
            out_bottom[az] = bottom[a] * (1.0 - t) + bottom[b] * t;
        }
    }

    MeltingLayer {
        top_km_arl: out_top,
        bottom_km_arl: out_bottom,
    }
}

#[cfg(test)]
mod tests;
