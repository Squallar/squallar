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
//! The live harness scores the derivation against the RPG's own N0H for the
//! same volume and cut (paired like the KDP twin, elevation-angle fallback
//! included), as classes: exact agreement plus a compatible-pair band
//! (WS↔GR, BD↔RA, HR↔RA — see [`validation_policy`]) and the full confusion
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
//! the asserted eight, conclusive under [`validation_policy`]. The
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
//! (see [`validation_policy::QUARANTINED`]): KFSD (biology-dominated
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

const MIN_V_GC: f64 = 1.0;
const MAX_Z_RA: f64 = 50.0;
const MIN_RHO_RA: f64 = 0.94;
const MIN_PHIDP_RA: f64 = 100.0;
const MIN_Z_RH: f64 = 30.0;
const MIN_Z_HR: f64 = 30.0;
const MIN_ZDR_HR: f64 = 1.0;
const MAX_Z_IC: f64 = 40.0;
const MIN_Z_GR: f64 = 10.0;
const MAX_Z_GR: f64 = 60.0;
const MAX_ZDR_GR: f64 = 2.0;
const MIN_Z_BD: f64 = 15.0;
const MIN_ZDR_BD: f64 = 0.5;
// B21: `min_Z_WS` is "no longer used per CCR NA15-00181" — the Z leg of the
// WS kill is commented out of `hca_allowedHydroClass.c`; only ZDR remains.
const MIN_ZDR_WS: f64 = 0.0;
const MAX_RHOHV_BI: f64 = 0.97;
const MAX_Z_BI: f64 = 35.0;
const MAX_ZDR_DS: f64 = 2.0;
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
/// `HailSize.cpp`'s hard bounds.
const HSDA_MAX_ZDR: f64 = 2.0;
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

    let mut values = Vec::with_capacity(combined.len());
    let mut azimuths = Vec::with_capacity(combined.len());
    for c in &combined {
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
        values.push(
            classes
                .iter()
                .zip(sub.iter())
                .map(|(&cl, &s)| external_code(cl, s))
                .collect(),
        );
        azimuths.push(c.base.az);
    }

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

        for c in &combined {
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
                let range_index =
                    ((ml_range_from_height(c.elev, temp_height) / f.dg + 0.5) as usize).min(f.n);
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
                    weight[az_index][height_index] += 1.0 + elev_weight;
                }
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

/// The parts of the live harness that decide **what counts as passing**.
///
/// Outside the ignored module for the reason `kdp::validation_policy`,
/// `eet::validation_policy` and `vil::validation_policy` are: the live
/// harness never runs under `cargo test --workspace`, so anything defined
/// inside it could be quietly weakened without a default-suite test
/// noticing. Out here `policy_tests` reaches all of it offline, and does.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod validation_policy {
    use crate::twin::compare::{Tally, ValueCodec};
    use nexrad_level3::model::RadialPacket;

    /// The campaign's class bar, per site per tilt: at least this share of
    /// compared gates carrying **exactly** the twin's class code…
    pub const EXACT_PCT: f64 = 85.0;

    /// …and at least this share carrying the twin's code **or** its
    /// compatible partner.
    pub const COMPATIBLE_PCT: f64 = 95.0;

    /// A run concludes nothing until this many sites were asserted…
    pub const MIN_SITES: usize = 3;

    /// …and this many gates were compared, pooled across them.
    pub const MIN_DEFINED_GATES: usize = 10_000;

    /// Twins whose packet defines fewer gates than this are skipped, not
    /// scored: clear-air speckle measures nothing. A skip is printed,
    /// never silent.
    pub const MIN_TWIN_DEFINED_GATES: usize = 500;

    /// Class pairs the bar treats as compatible, in the product's external
    /// codes, justified from Park et al. (2009)'s own confusion
    /// discussion rather than tuned here:
    ///
    /// * **WS↔GR** (50↔90): the paper adds a convective/stratiform
    ///   routine precisely "to better discriminate between wet snow and
    ///   melting graupel within the melting layer" — a routine the
    ///   released HCA task does not contain, so the two remain the
    ///   overlap the paper says they are;
    /// * **BD↔RA** (80↔60): big drops are by the paper's own definition
    ///   "rain with a DSD skewed toward large raindrops" — a rain
    ///   subcategory whose Z/ZDR memberships overlap RA's over most of
    ///   the Z range;
    /// * **HR↔RA** (70↔60): the same rain continuum split at a rate
    ///   boundary, with overlapping Z memberships (RA to 50, HR from 40)
    ///   and identical remaining rows.
    pub const COMPATIBLE_PAIRS: &[(u8, u8)] = &[(50, 90), (60, 80), (60, 70)];

    pub fn is_compatible(a: u8, b: u8) -> bool {
        a == b
            || COMPATIBLE_PAIRS
                .iter()
                .any(|&(x, y)| (a, b) == (x, y) || (a, b) == (y, x))
    }

    /// The compatible share of a class tally: exact plus the compatible
    /// pairs, over the compared gates.
    pub fn compatible_pct(tally: &Tally) -> f64 {
        let ok: usize = tally
            .confusion
            .iter()
            .filter(|&(&(a, b), _)| is_compatible(a, b))
            .map(|(_, &c)| c)
            .sum();
        100.0 * ok as f64 / tally.compared.max(1) as f64
    }

    /// Both legs of the class bar, inclusive.
    pub fn meets_class_bar(exact_pct: f64, compatible_pct: f64) -> bool {
        exact_pct >= EXACT_PCT && compatible_pct >= COMPATIBLE_PCT
    }

    pub fn volume_is_scoreable(twin_defined_gates: usize) -> bool {
        twin_defined_gates >= MIN_TWIN_DEFINED_GATES
    }

    pub fn sample_is_conclusive(sites_asserted: usize, pooled_compared_gates: usize) -> bool {
        sites_asserted >= MIN_SITES && pooled_compared_gates >= MIN_DEFINED_GATES
    }

    /// How much of a quarantined site stops being asserted on. HCA is
    /// scored on one paired tilt per site, so the only scope is the whole
    /// site.
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
    /// Quarantining requires recorded evidence from **at least two volumes
    /// across at least two runs** — one run's miss is a lead, not a
    /// verdict. A quarantined site stays in [`crate::twin::live::SITES`]
    /// and stays measured and printed; only the assertion is withheld.
    /// Never widen the bar instead.
    ///
    /// Every entry below missed only the **95% compatible** leg — each
    /// cleared the 85% exact bar on every volume measured (the
    /// 2026-07-29 precipitation survey; figures in the module doc).
    pub const QUARANTINED: &[Quarantine] = &[
        Quarantine {
            site: "KFSD",
            scope: Scope::Whole,
            why: "compatible 91.24/91.66 on volumes 2026-07-29 08:01 and 06:29 \
                  across two runs (exact 91.23/91.66). The compared field is \
                  >80% biology, where the compatible pairs add nothing; the \
                  whole residual is BI↔GC (blockage store + split-cut Doppler \
                  graft) and BI↔UK (min_Dif_Agg margins) — the clear-air \
                  survey's documented operational-state fingerprints.",
        },
        Quarantine {
            site: "KMRX",
            scope: Scope::Whole,
            why: "compatible 94.34/94.60 on volumes 2026-07-28 20:58 and 19:58 \
                  across two runs (exact 90.94/91.19, heavy convection). \
                  Residual: BI↔GC/UK in the ridge-and-valley terrain (blockage \
                  store) plus UK↔RA ~1% on aggregation margins; the rain-family \
                  cells themselves are compatible (BD→RA 2%).",
        },
        Quarantine {
            site: "KSFX",
            scope: Scope::Whole,
            why: "compatible 93.74/92.49 on volumes 2026-07-28 20:59 and 21:59 \
                  across two runs (exact 93.42/91.86). Mountain site: BI↔GC/UK \
                  (blockage store) plus DS→IC in the cold band — the documented \
                  DS↔IC state fingerprint.",
        },
        Quarantine {
            site: "KMTX",
            scope: Scope::Whole,
            why: "compatible 93.69/93.55/93.85 on volumes 2026-07-28 20:57, \
                  20:27 and 21:29 across two runs (exact 93.51/93.06/93.34), \
                  driven by RA→DS 0.8-1.3% — the twin's model-enhanced melting \
                  layer sat below our sounding flat layer through that episode \
                  (RUC/RAP grid state the archive does not carry) — plus the \
                  mountain BI↔GC band. The site PASSED both bars on \
                  2026-07-29 07:57 (96.04/96.16): the derivation is sound when \
                  the ML state aligns, so the quarantine records state, not \
                  transcription, error.",
        },
    ];

    pub fn quarantine(site: &str) -> Option<&'static Quarantine> {
        QUARANTINED.iter().find(|q| q.site == site)
    }

    pub fn site_is_asserted(site: &str) -> bool {
        quarantine(site).is_none()
    }

    /// The number of gates the twin defines — levels that decode to a
    /// finite value through the product's own codec (level 0, ND/NE, is
    /// undefined).
    pub fn twin_defined_gates(packet: &RadialPacket, codec: &ValueCodec) -> usize {
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
    use nexrad_model::data::{MomentData, Radial, RadialStatus};

    const D_GATES: usize = 400;
    const FIRST_M: u16 = 125; // gate-0 centre at 0.125 km
    const GATE_M: u16 = 250;

    const PHI_SCALE: f32 = 10.0;
    const PHI_OFFSET: f32 = 2.0;
    const RHO_SCALE: f32 = 500.0;
    const RHO_OFFSET: f32 = 2.0;
    const Z_SCALE: f32 = 2.0;
    const Z_OFFSET: f32 = 66.0;
    const ZDR_SCALE_FX: f32 = 16.0;
    const ZDR_OFFSET_FX: f32 = 128.0;
    const V_SCALE: f32 = 2.0;
    const V_OFFSET: f32 = 129.0;

    /// One gate of a fixture moment.
    #[derive(Clone, Copy)]
    enum G {
        V(f64),
        Nd,
    }

    fn raw_of(scale: f32, offset: f32, g: G) -> u16 {
        match g {
            G::Nd => 0,
            G::V(v) => {
                let raw = v * f64::from(scale) + f64::from(offset);
                let rounded = raw.round();
                assert!(
                    (raw - rounded).abs() < 1e-6,
                    "fixture value {v} does not encode exactly (raw {raw})"
                );
                rounded as u16
            }
        }
    }

    fn m16(scale: f32, offset: f32, vals: &[G]) -> MomentData {
        let mut bytes = Vec::with_capacity(vals.len() * 2);
        for &g in vals {
            bytes.extend_from_slice(&raw_of(scale, offset, g).to_be_bytes());
        }
        MomentData::from_fixed_point(vals.len() as u16, FIRST_M, GATE_M, 16, scale, offset, bytes)
    }

    fn m8(scale: f32, offset: f32, vals: &[G]) -> MomentData {
        let bytes: Vec<u8> = vals
            .iter()
            .map(|&g| {
                let raw = raw_of(scale, offset, g);
                assert!(raw <= 255, "8-bit fixture value overflows");
                raw as u8
            })
            .collect();
        MomentData::from_fixed_point(vals.len() as u16, FIRST_M, GATE_M, 8, scale, offset, bytes)
    }

    /// One dual-pol radial with everything the HCA chain reads.
    #[allow(clippy::too_many_arguments)]
    fn hca_radial(
        az: f64,
        spacing: f32,
        elev: f32,
        n: usize,
        z_at: &dyn Fn(usize) -> G,
        zdr_at: &dyn Fn(usize) -> G,
        rho_at: &dyn Fn(usize) -> G,
        phi_at: &dyn Fn(usize) -> G,
        vel_at: Option<&dyn Fn(usize) -> G>,
    ) -> Radial {
        let z: Vec<G> = (0..n).map(z_at).collect();
        let zdr: Vec<G> = (0..n).map(zdr_at).collect();
        let rho: Vec<G> = (0..n).map(rho_at).collect();
        let phi: Vec<G> = (0..n).map(phi_at).collect();
        let vel = vel_at.map(|f| {
            let v: Vec<G> = (0..n).map(f).collect();
            m8(V_SCALE, V_OFFSET, &v)
        });
        Radial::new(
            0,
            0,
            az as f32,
            spacing,
            RadialStatus::IntermediateRadialData,
            1,
            elev,
            Some(m8(Z_SCALE, Z_OFFSET, &z)),
            vel,
            None,
            Some(m8(ZDR_SCALE_FX, ZDR_OFFSET_FX, &zdr)),
            Some(m16(PHI_SCALE, PHI_OFFSET, &phi)),
            Some(m16(RHO_SCALE, RHO_OFFSET, &rho)),
            None,
        )
    }

    fn params() -> KdpParams {
        KdpParams {
            init_fdp_deg: Some(60.0),
            dbz0: Some(-40.0),
            atmos_db_per_km: Some(-0.012),
            isdp_est_deg: None,
        }
    }

    /// Wet-bulb heights far above every fixture beam: the HSDA regimes and
    /// the RH ZDR modification stay inert unless a test moves them.
    fn hsda_far() -> HsdaHeights {
        HsdaHeights {
            tw0_km_arl: 100.0,
            twm25_km_arl: 105.0,
        }
    }

    // ── Transcription pins: one test per class's membership table ─────────
    //
    // Each row is asserted separately so a wrong transcription localizes to
    // the class × variable table it sits in. The expected numbers are read
    // off `cpc104/lib006/hca.alg` (`mem*` / `memFlag*`), never off this
    // module's own constants.

    fn assert_table(class: &str, table: &MemTable, rows: [[f64; 4]; 6], flags: [[MemFlag; 4]; 6]) {
        const VARS: [&str; 6] = ["SMZ", "ZDR", "LKDP", "RHO", "SDZ", "SDP"];
        for (i, var) in VARS.iter().enumerate() {
            assert_eq!(
                table.points[i], rows[i],
                "{class}/{var} membership points diverge from hca.alg",
            );
            assert_eq!(
                table.flags[i], flags[i],
                "{class}/{var} membership flags diverge from hca.alg",
            );
        }
    }

    const NF: [MemFlag; 4] = [MF, MF, MF, MF];

    #[test]
    fn mem_table_ra_matches_hca_alg() {
        assert_table(
            "RA",
            &MEM_RA,
            [
                [5.0, 10.0, 45.0, 50.0],
                [-0.3, 0.0, 0.0, 0.5],
                [-1.0, 0.0, 0.0, 1.0],
                [0.95, 0.97, 1.0, 1.01],
                [0.0, 0.5, 3.0, 6.0],
                [0.0, 1.0, 15.0, 30.0],
            ],
            [NF, [F1, F1, F2, F2], [G1, G1, G2, G2], NF, NF, NF],
        );
    }

    #[test]
    fn mem_table_hr_matches_hca_alg() {
        assert_table(
            "HR",
            &MEM_HR,
            [
                [40.0, 45.0, 55.0, 60.0],
                [-0.3, 0.0, 0.0, 0.5],
                [-1.0, 0.0, 0.0, 1.0],
                [0.92, 0.95, 1.0, 1.01],
                [0.0, 0.5, 3.0, 6.0],
                [0.0, 1.0, 15.0, 30.0],
            ],
            [NF, [F1, F1, F2, F2], [G1, G1, G2, G2], NF, NF, NF],
        );
    }

    #[test]
    fn mem_table_rh_matches_hca_alg() {
        assert_table(
            "RH",
            &MEM_RH,
            [
                [45.0, 50.0, 75.0, 80.0],
                [-0.3, 0.0, 0.0, 0.5],
                [-10.0, -4.0, 0.0, 1.0],
                [0.85, 0.90, 1.0, 1.01],
                [0.0, 0.5, 3.0, 6.0],
                [0.0, 1.0, 15.0, 30.0],
            ],
            [NF, [MF, MF, F1, F1], [MF, MF, G1, G1], NF, NF, NF],
        );
    }

    /// BD's Z row is the source's (10, 15, 45, 50) — the paper prints
    /// (20, 25, 45, 50); the source wins.
    #[test]
    fn mem_table_bd_matches_hca_alg() {
        assert_table(
            "BD",
            &MEM_BD,
            [
                [10.0, 15.0, 45.0, 50.0],
                [-0.3, 0.0, 0.0, 1.0],
                [-1.0, 0.0, 0.0, 1.0],
                [0.92, 0.95, 1.0, 1.01],
                [0.0, 0.5, 3.0, 6.0],
                [0.0, 1.0, 15.0, 30.0],
            ],
            [NF, [F2, F2, F3, F3], [G1, G1, G2, G2], NF, NF, NF],
        );
    }

    /// BI's ZDR x2 is the source's 0 (paper 2) and its ρ row tops at
    /// 0.85/0.90 (paper 0.80/0.83); the source wins.
    #[test]
    fn mem_table_bi_matches_hca_alg() {
        assert_table(
            "BI",
            &MEM_BI,
            [
                [5.0, 10.0, 20.0, 30.0],
                [0.0, 0.0, 10.0, 12.0],
                [-30.0, -25.0, 10.0, 20.0],
                [0.30, 0.50, 0.85, 0.90],
                [1.0, 2.0, 4.0, 7.0],
                [8.0, 10.0, 40.0, 60.0],
            ],
            [NF, [MF, F3, MF, MF], NF, NF, NF, NF],
        );
    }

    #[test]
    fn mem_table_gc_matches_hca_alg() {
        assert_table(
            "GC",
            &MEM_GC,
            [
                [15.0, 20.0, 70.0, 80.0],
                [-4.0, -2.0, 1.0, 2.0],
                [-30.0, -25.0, 10.0, 20.0],
                [0.50, 0.60, 0.90, 0.95],
                [2.0, 4.0, 10.0, 15.0],
                [30.0, 40.0, 50.0, 60.0],
            ],
            [NF; 6],
        );
    }

    /// DS's B21 rows: ZDR (−0.3, 0, 0.9, 1.1) and ρ (0.98, 0.99, 1, 1.01)
    /// — read off B21's `hca.alg`, tighter than both the paper and B16.
    #[test]
    fn mem_table_ds_matches_hca_alg() {
        assert_table(
            "DS",
            &MEM_DS,
            [
                [5.0, 10.0, 35.0, 40.0],
                [-0.3, 0.0, 0.9, 1.1],
                [-30.0, -25.0, 10.0, 20.0],
                [0.98, 0.99, 1.0, 1.01],
                [0.0, 0.5, 3.0, 6.0],
                [0.0, 1.0, 15.0, 30.0],
            ],
            [NF; 6],
        );
    }

    /// WS's B21 rows: Z starts at 15, the ZDR row is two-dimensional
    /// ((0.5, 1.0) + f2-based upper points), ρ widened down to 0.84.
    #[test]
    fn mem_table_ws_matches_hca_alg() {
        assert_table(
            "WS",
            &MEM_WS,
            [
                [15.0, 25.0, 40.0, 50.0],
                [0.5, 1.0, 0.0, 0.3],
                [-30.0, -25.0, 10.0, 20.0],
                [0.84, 0.88, 0.97, 0.985],
                [0.0, 0.5, 3.0, 6.0],
                [0.0, 1.0, 15.0, 30.0],
            ],
            [NF, [MF, MF, F2, F2], NF, NF, NF, NF],
        );
    }

    #[test]
    fn mem_table_ic_matches_hca_alg() {
        assert_table(
            "IC",
            &MEM_IC,
            [
                [0.0, 5.0, 20.0, 25.0],
                [0.1, 0.4, 3.0, 3.3],
                [-5.0, 0.0, 10.0, 15.0],
                [0.95, 0.98, 1.0, 1.01],
                [0.0, 0.5, 3.0, 6.0],
                [0.0, 1.0, 15.0, 30.0],
            ],
            [NF; 6],
        );
    }

    #[test]
    fn mem_table_gr_matches_hca_alg() {
        assert_table(
            "GR",
            &MEM_GR,
            [
                [25.0, 35.0, 50.0, 55.0],
                [-0.3, 0.0, 0.0, 0.3],
                [-30.0, -25.0, 10.0, 20.0],
                [0.90, 0.97, 1.0, 1.01],
                [0.0, 0.5, 3.0, 6.0],
                [0.0, 1.0, 15.0, 30.0],
            ],
            [NF, [MF, MF, F1, F1], NF, NF, NF, NF],
        );
    }

    /// The weight matrix, against `hca.alg`'s `weight_*` arrays (columns
    /// RA…GR), and the f/g coefficients against the paper's Eqs. (4)–(5),
    /// which the .alg values reproduce exactly.
    #[test]
    fn weights_and_equation_coefficients_match_hca_alg() {
        let expect: [(&str, [f64; 6]); 10] = [
            ("RA", [1.0, 0.8, 0.0, 0.6, 0.2, 0.2]),
            ("HR", [1.0, 0.8, 1.0, 0.6, 0.2, 0.2]),
            ("RH", [1.0, 0.8, 1.0, 0.6, 0.2, 0.2]),
            ("BD", [0.8, 1.0, 0.0, 0.6, 0.2, 0.2]),
            ("BI", [0.4, 0.6, 0.0, 1.0, 0.8, 0.8]),
            ("GC", [0.2, 0.4, 0.0, 1.0, 0.6, 0.8]),
            ("DS", [1.0, 0.8, 0.0, 0.6, 0.2, 0.2]),
            ("WS", [0.6, 0.8, 0.0, 1.0, 0.2, 0.2]),
            ("IC", [1.0, 0.6, 0.5, 0.4, 0.2, 0.2]),
            ("GR", [0.8, 1.0, 0.0, 0.4, 0.2, 0.2]),
        ];
        for (i, (name, row)) in expect.iter().enumerate() {
            assert_eq!(&WEIGHT[i], row, "{name} weights diverge from hca.alg");
        }
        assert_eq!(F1_COEF, (0.000_750, 0.0025, -0.5));
        assert_eq!(F2_COEF, (0.002_92, -0.0481, 0.68));
        assert_eq!(F3_COEF, (0.000_485, 0.0667, 1.42));
        assert_eq!(G1_COEF, (0.8, -44.0));
        assert_eq!(G2_COEF, (0.5, -22.0));
    }

    /// The hard thresholds and selection gates, against `hca.alg`.
    #[test]
    #[allow(clippy::assertions_on_constants)] // the pin IS a constant assert
    fn hard_thresholds_match_hca_alg() {
        assert_eq!(MIN_V_GC, 1.0);
        assert_eq!(MAX_Z_RA, 50.0);
        assert_eq!(MIN_RHO_RA, 0.94);
        assert_eq!(MIN_PHIDP_RA, 100.0);
        assert_eq!(MIN_Z_RH, 30.0, "the source's 30, not the paper's 40");
        assert_eq!(MIN_Z_HR, 30.0);
        assert_eq!(MIN_ZDR_HR, 1.0);
        assert_eq!(MAX_Z_IC, 40.0);
        assert_eq!(MIN_Z_GR, 10.0);
        assert_eq!(MAX_Z_GR, 60.0);
        assert_eq!(MAX_ZDR_GR, 2.0);
        assert_eq!(MIN_Z_BD, 15.0);
        assert_eq!(MIN_ZDR_BD, 0.5);
        // B21 (CCR NA15-00181): no min_Z_WS constant — the Z leg of the WS
        // kill is commented out of the source.
        assert_eq!(MIN_ZDR_WS, 0.0);
        assert_eq!(MAX_RHOHV_BI, 0.97);
        assert_eq!(MAX_Z_BI, 35.0);
        assert_eq!(MAX_ZDR_DS, 2.0);
        assert_eq!(MIN_AGG, 0.4);
        assert_eq!(MIN_DIF_AGG, 0.001);
        assert_eq!(MIN_SNR, 5.0);
        assert!(!ATTEN_CONTROL, "atten_control = Off in hca.alg");
        assert_eq!(MINI_LKTP, -40.0, "the source's −40, not the paper's −30");
        // The B21 HSDA adaptation values (hca.alg / hail.alg).
        assert!(ENABLE_SIZE, "enable_size = Yes in hca.alg");
        assert_eq!(MIN_DATA_SIZE, 2);
        assert_eq!(EXT_LH, 110.0);
        assert_eq!(EXT_GH, 120.0);
        assert!((DEFAULT_HEIGHT_TW0_KM_MSL - 3.048).abs() < 1e-9, "10.0 kft");
        assert!(
            (DEFAULT_HEIGHT_TW_M25_KM_MSL - 6.7056).abs() < 1e-9,
            "22.0 kft",
        );
    }

    /// The output codes against `dualpol8bit.c`'s `Class_external`, and
    /// against the label arms `types.rs::format_value` already ships for
    /// HydrometeorClassification.
    #[test]
    fn class_codes_match_the_products_convention() {
        let expected: [(usize, f32, &str); 11] = [
            (RA, 60.0, "Rain"),
            (HR, 70.0, "Heavy Rain"),
            (RH, 100.0, "Hail+Rain"),
            (BD, 80.0, "Big Drops"),
            (BI, 10.0, "Biological"),
            (GC, 20.0, "Clutter/AP"),
            (DS, 40.0, "Dry Snow"),
            (WS, 50.0, "Wet Snow"),
            (IC, 30.0, "Ice Crystals"),
            (GR, 90.0, "Graupel"),
            (UK, 140.0, "Unknown"),
        ];
        let prefs = rustdar_units::UserPreferences::default();
        for (class, code, label) in expected {
            assert_eq!(CLASS_EXTERNAL[class], code);
            assert_eq!(
                crate::types::RadarProduct::HydrometeorClassification.format_value(code, &prefs),
                format!("HHC: {label}"),
                "code {code} must land in the existing HHC arm",
            );
        }
        assert_eq!(CLASS_EXTERNAL[U0], 0.0);
        assert_eq!(CLASS_EXTERNAL[U1], 0.0);
        assert_eq!(CLASS_EXTERNAL[NE], 0.0, "no echo encodes as undefined");
    }

    // ── Membership machinery ───────────────────────────────────────────────

    /// The trapezoid: plateau, both shoulders, both edges, and the
    /// non-monotonic guard.
    #[test]
    fn degree_membership_is_the_documented_trapezoid() {
        let p = [0.0, 1.0, 3.0, 5.0];
        assert_eq!(degree_membership(2.0, p), 1.0, "plateau");
        assert_eq!(degree_membership(1.0, p), 1.0, "x2 belongs to the plateau");
        assert_eq!(degree_membership(3.0, p), 1.0, "x3 belongs to the plateau");
        assert_eq!(degree_membership(0.5, p), 0.5, "rising shoulder");
        assert_eq!(degree_membership(4.0, p), 0.5, "falling shoulder");
        assert_eq!(degree_membership(0.0, p), 0.0, "x1 is outside");
        assert_eq!(degree_membership(5.0, p), 0.0, "x4 is outside");
        assert_eq!(degree_membership(-1.0, p), 0.0);
        assert_eq!(degree_membership(6.0, p), 0.0);
        assert_eq!(
            degree_membership(2.0, [3.0, 1.0, 4.0, 5.0]),
            0.0,
            "non-monotonic points return 0 outright",
        );
    }

    /// The 2-D rows at Z = 45 dBZ, hand-computed: f1 = 1.13125,
    /// f2 = 4.4285, f3 = 5.403625, g1 = −8, g2 = 0.5. Heights far below
    /// the wet-bulb zero regimes keep the HSDA modification inert.
    #[test]
    fn two_dimensional_membership_points_follow_the_equations() {
        let mp = |class, input, z| set_membership_points(class, input, z, 0.0, 100.0);
        let ra_zdr = mp(RA, ZDR, 45.0);
        for (got, want) in ra_zdr.iter().zip([0.83125, 1.13125, 4.4285, 4.9285]) {
            assert!(
                (got - want).abs() < 1e-9,
                "RA/ZDR at 45 dBZ: {got} vs {want}"
            );
        }
        let ra_lkdp = mp(RA, LKDP, 45.0);
        for (got, want) in ra_lkdp.iter().zip([-9.0, -8.0, 0.5, 1.5]) {
            assert!(
                (got - want).abs() < 1e-9,
                "RA/LKDP at 45 dBZ: {got} vs {want}"
            );
        }
        let bd_zdr = mp(BD, ZDR, 45.0);
        for (got, want) in bd_zdr.iter().zip([4.1285, 4.4285, 5.403625, 6.403625]) {
            assert!(
                (got - want).abs() < 1e-9,
                "BD/ZDR at 45 dBZ: {got} vs {want}"
            );
        }
        // The B21 WS ZDR row: x3/x4 ride f2 (4.4285 + 0 / + 0.3 at 45 dBZ).
        let ws_zdr = mp(WS, ZDR, 45.0);
        for (got, want) in ws_zdr.iter().zip([0.5, 1.0, 4.4285, 4.7285]) {
            assert!(
                (got - want).abs() < 1e-9,
                "WS/ZDR at 45 dBZ: {got} vs {want}"
            );
        }
        // 1-D rows pass through untouched.
        assert_eq!(mp(GC, RHO, 45.0), [0.5, 0.6, 0.9, 0.95]);
    }

    /// The HSDA modification of RH's ZDR row (hca_setMembershipPoints.c):
    /// only the F1-flagged points move, only in the two regimes below the
    /// wet-bulb zero, by the hardcoded polynomials. At Z = 55:
    /// g-regime (tw0−2 < h ≤ tw0−1): 5e-4·55² + 1.5e-2·55 − 0.9 = 1.4375;
    /// linear regime (tw0−1 < h < tw0): 0.02·55 − 0.6 = 0.5.
    #[test]
    fn hsda_reshapes_rh_zdr_membership_below_the_wet_bulb_zero() {
        let tw0 = 3.0;
        // Far below both regimes: the normal F1 applies
        // (f1(55) = −0.5 + 2.5e-3·55 + 7.5e-4·55² = 1.90625; the RH ZDR
        // base points are x3 = 0, x4 = 0.5).
        let normal = set_membership_points(RH, ZDR, 55.0, 0.5, tw0);
        assert!((normal[2] - 1.906_25).abs() < 1e-9, "got {}", normal[2]);
        assert!((normal[3] - 2.406_25).abs() < 1e-9);
        // (tw0−2, tw0−1]: the g-shaped polynomial replaces F1.
        let g = set_membership_points(RH, ZDR, 55.0, 1.5, tw0);
        assert!((g[2] - 1.4375).abs() < 1e-9, "got {}", g[2]);
        assert!((g[3] - 1.9375).abs() < 1e-9);
        // (tw0−1, tw0): the linear polynomial.
        let lin = set_membership_points(RH, ZDR, 55.0, 2.5, tw0);
        assert!((lin[2] - 0.5).abs() < 1e-9, "got {}", lin[2]);
        assert!((lin[3] - 1.0).abs() < 1e-9);
        // At/above the wet-bulb zero: normal F1 again.
        let above = set_membership_points(RH, ZDR, 55.0, 3.0, tw0);
        assert_eq!(above, normal);
        // The unflagged x1/x2 never move.
        assert_eq!(g[0], -0.3);
        assert_eq!(g[1], 0.0);
        // Other classes are untouched in the same regime.
        assert_eq!(
            set_membership_points(RA, ZDR, 55.0, 1.5, tw0),
            set_membership_points(RA, ZDR, 55.0, 0.5, tw0),
        );
    }

    /// The aggregation: `Σ WQF / (Σ WQ + 0.01)`, hand-computed.
    #[test]
    fn weighted_aggregation_carries_the_plus_p01_denominator() {
        let w = [1.0, 0.8, 0.0, 0.6, 0.2, 0.2];
        let q = [1.0; 6];
        let f = [1.0, 1.0, 0.0, 1.0, 0.0, 0.0];
        let s: f64 = 1.0 + 0.8 + 0.6 + 0.2 + 0.2;
        let want = (1.0 + 0.8 + 0.6) / (s + 0.01);
        assert!((weighted_aggregation(&w, &q, &f) - want).abs() < 1e-12);
        assert_eq!(
            weighted_aggregation(&[0.0; 6], &q, &[1.0; 6]),
            0.0,
            "all-zero weights aggregate to 0 through the +0.01 guard",
        );
    }

    /// The 8-bit moment transport: round half away from zero, clamp to
    /// [2, 255], decode back — hand-computed pins.
    #[test]
    fn transport8_reproduces_add_moment_rounding() {
        assert_eq!(transport8(30.26, (2.0, 66.0)), 30.5);
        assert_eq!(transport8(-3.9, (16.0, 128.0)), -3.875);
        assert_eq!(transport8(300.0, (2.0, 66.0)), 94.5, "clamps at level 255");
        assert_eq!(transport8(-40.0, (2.0, 26.0)), -12.0, "clamps at level 2");
        assert!(transport8(f64::NAN, (2.0, 66.0)).is_nan());
    }

    /// The QIA's six indices at φ = 90°, SNR = 20 dB, ρ = 0.99, Z = 40,
    /// hand-computed through the quantized transport: (0.98, 0.94, 0.57,
    /// 0.57, 1.00, 1.00) in fuzzy-logic input order.
    #[test]
    fn quality_indices_match_the_hand_computed_values() {
        let q = quality_indices(90.0, 0.99, 40.0, 20.0, true);
        let want = [0.98, 0.94, 0.57, 0.57, 1.0, 1.0];
        for (i, (got, want)) in q.iter().zip(want).enumerate() {
            assert!((got - want).abs() < 1e-9, "q[{i}]: {got} vs {want}");
        }
        // The attenuation exception: ρ < 0.8 with Z < 25 zeroes the Δρ
        // term, so q_zdr rises against the same inputs without it.
        let with = quality_indices(90.0, 0.5, 20.0, 20.0, false);
        let without = quality_indices(90.0, 0.5, 30.0, 20.0, false);
        assert!(
            with[ZDR] > without[ZDR],
            "Dc must be zeroed only when Z < 25"
        );
        // Missing φ (the C sentinel) zeroes the φ-driven indices exactly,
        // and leaves the texture indices standing.
        let q = quality_indices(NO_DATA, 0.99, 40.0, 20.0, false);
        assert_eq!(q[SMZ], 0.0);
        assert_eq!(q[ZDR], 0.0);
        assert_eq!(q[LKDP], 0.0);
        assert_eq!(q[RHO], 0.0);
        assert!(q[SDZ] > 0.99 && q[SDP] > 0.99);
        // Missing SNR kills everything.
        let q = quality_indices(90.0, 0.99, 40.0, NO_DATA, false);
        assert_eq!(q, [0.0; 6]);
    }

    /// The texture filter, hand-computed on [10,10,40,10,10,10,10] about
    /// its own 5-gate mean: SD(2) = 14.126217, SD(0) = 18.949494; with the
    /// exclusion threshold at 20 the outlier difference (+24) drops out
    /// and SD(2) = 1.887678.
    #[test]
    fn texture_std_filter_matches_the_hand_computation() {
        let input = [10.0, 10.0, 40.0, 10.0, 10.0, 10.0, 10.0];
        let smoothed = average_filter(&input, 5);
        assert_eq!(smoothed[0], 20.0, "truncated leading window");
        let sd = std_filter(&input, &smoothed, 5, MAX_DIFF_DBZ);
        assert!((sd[2] - 14.126_216_76).abs() < 1e-6, "got {}", sd[2]);
        assert!((sd[0] - 18.949_494_28).abs() < 1e-6, "got {}", sd[0]);
        let sd = std_filter(&input, &smoothed, 5, 20.0);
        assert!((sd[2] - 1.887_458_6).abs() < 1e-6, "got {}", sd[2]);
    }

    /// The beam/melting-layer intersection at 0.5° over a flat 2.5–3.0 km
    /// layer, hand-computed on the 7708.91-km effective Earth: bins 414,
    /// 561, 632, 860 at 0.25 km.
    #[test]
    fn beam_ml_intersection_matches_the_hand_computation() {
        let ml = MeltingLayer {
            top_km_arl: [3.0; 360],
            bottom_km_arl: [2.5; 360],
        };
        let bins = beam_ml_intersection(0.5, 0, 0.25, &ml);
        assert_eq!(bins.bb, 414);
        assert_eq!(bins.b, 561);
        assert_eq!(bins.t, 632);
        assert_eq!(bins.tt, 860);
    }

    /// The melting-layer zones gate the allowed classes exactly as
    /// `Hca_allowedHydroClass` lists them.
    #[test]
    fn allowed_classes_follow_the_melting_layer_zones() {
        let ml = MlBins {
            bb: 100,
            b: 200,
            t: 300,
            tt: 400,
        };
        let allowed = |bin: i64| -> Vec<usize> {
            let mut agg = [0.0f64; NUM_CLASSES];
            // Inputs that trip no hard threshold: Z 32, ZDR 1, ρ 0.96,
            // φ 120, V missing.
            allowed_hydro_class(bin, 32.0, 1.0, 0.96, 120.0, NO_DATA, false, &mut agg, ml);
            (0..NUM_CLASSES).filter(|&i| agg[i] == 0.0).collect()
        };
        assert_eq!(allowed(50), vec![RA, HR, RH, BD, BI, GC]);
        assert_eq!(allowed(150), vec![RA, HR, RH, BD, BI, GC, WS, GR]);
        assert_eq!(allowed(250), vec![RH, BD, BI, GC, DS, WS, GR]);
        // B21 widened the upper zones: BI back in the upper transition,
        // GC and BI back above the layer.
        assert_eq!(allowed(350), vec![RH, BD, BI, GC, DS, WS, IC, GR]);
        assert_eq!(allowed(450), vec![RH, BI, GC, DS, IC, GR]);
    }

    /// B21 (CCR NA15-00181): weak Z no longer kills WS — only negative ZDR
    /// does.
    #[test]
    fn the_ws_kill_lost_its_z_leg_in_b21() {
        let ml = MlBins {
            bb: 0,
            b: 0,
            t: 100,
            tt: 100,
        };
        let ws_alive = |z: f64, zdr: f64| -> bool {
            let mut agg = [0.0f64; NUM_CLASSES];
            allowed_hydro_class(50, z, zdr, 0.93, 120.0, NO_DATA, false, &mut agg, ml);
            agg[WS] == 0.0
        };
        assert!(ws_alive(18.0, 0.5), "Z 18 killed WS in B16, not in B21");
        assert!(!ws_alive(18.0, -0.5), "negative ZDR still kills WS");
    }

    /// `Break_tie` (CCR NA14-00181): the AEL Table 4 priority per zone,
    /// including the source's "tuned" upper lists.
    #[test]
    fn break_tie_follows_the_zone_priority_lists() {
        let ml = MlBins {
            bb: 100,
            b: 200,
            t: 300,
            tt: 400,
        };
        // Below the layer BD outranks RA.
        assert_eq!(break_tie(50, ml, RA, BD), BD);
        assert_eq!(break_tie(50, ml, BD, RA), BD);
        // Entering: WS outranks BD.
        assert_eq!(break_tie(150, ml, BD, WS), WS);
        // Within: DS outranks WS.
        assert_eq!(break_tie(250, ml, WS, DS), DS);
        // Upper transition (tuned list): BI outranks GC.
        assert_eq!(break_tie(350, ml, GC, BI), BI);
        // Above: GC outranks DS.
        assert_eq!(break_tie(450, ml, DS, GC), GC);
        // A runner-up absent from the list leaves the winner standing.
        assert_eq!(break_tie(450, ml, DS, RA), DS);
    }

    /// Each hard threshold kills exactly its class.
    #[test]
    fn hard_thresholds_invalidate_the_documented_classes() {
        let ml = MlBins {
            bb: 1000,
            b: 1000,
            t: 1000,
            tt: 1000,
        }; // everything below the layer
        let killed = |z: f64, zdr: f64, rho: f64, phi: f64, v: f64| -> Vec<usize> {
            let mut agg = [0.0f64; NUM_CLASSES];
            allowed_hydro_class(0, z, zdr, rho, phi, v, false, &mut agg, ml);
            // Below the layer only GC/BI/BD/RA/HR/RH are in play; report
            // which of those the thresholds removed.
            [RA, HR, RH, BD, BI, GC]
                .into_iter()
                .filter(|&c| agg[c] == -1.0)
                .collect()
        };
        // A benign rain gate kills HR (ZDR 0.6 < 1) only.
        assert_eq!(killed(35.0, 0.6, 0.99, 120.0, NO_DATA), vec![HR, BI]);
        assert_eq!(killed(55.0, 1.5, 0.99, 120.0, NO_DATA), vec![RA, BI]);
        assert_eq!(killed(25.0, 1.5, 0.99, 120.0, NO_DATA), vec![HR, RH, BI]);
        assert_eq!(
            killed(35.0, 0.3, 0.99, 120.0, NO_DATA),
            vec![HR, BD, BI],
            "ZDR under 0.5 kills BD too",
        );
        assert_eq!(
            killed(35.0, 1.5, 0.90, 60.0, NO_DATA),
            vec![RA],
            "low rho with low phi kills RA",
        );
        assert_eq!(
            killed(35.0, 1.5, 0.90, 120.0, NO_DATA),
            Vec::<usize>::new(),
            "phi at 120 keeps RA despite the low rho",
        );
        assert_eq!(
            killed(20.0, 1.5, 0.98, 120.0, 3.0),
            vec![HR, RH, BI, GC],
            "|V| over 1 kills GC; rho over 0.97 kills BI; Z under 30 kills HR and RH",
        );
        assert_eq!(
            killed(40.0, 1.5, 0.96, 120.0, NO_DATA),
            vec![BI],
            "Z over 35 kills BI everywhere with atten_control off",
        );
    }

    // ── Per-class synthetic classification ─────────────────────────────────
    //
    // Each class at its membership plateau must win, and pushing any one
    // variable past the trapezoid's edges must zero that variable's
    // membership (the edge behaviour is pinned through the class's own
    // table so a failure localizes).

    /// A `Fields` fixture for direct gate classification.
    #[allow(clippy::too_many_arguments)]
    fn fields_one_gate(
        smz: f64,
        zdr: f64,
        rho: f64,
        kdp: f64,
        phi: f64,
        sdz: f64,
        sdp: f64,
        smv: f64,
        snr: f64,
    ) -> Fields {
        Fields {
            az: 0.5,
            elev: 0.5,
            hatt: false,
            n: 1,
            dg: 0.25,
            smz: vec![smz],
            snr: vec![snr],
            sdz: vec![sdz],
            zdr: vec![zdr],
            rho: vec![rho],
            kdp: vec![kdp],
            phi: vec![phi],
            sdp: vec![sdp],
            smv: vec![smv],
            met: vec![f64::NAN],
            q: vec![quality_indices(phi, rho, smz, snr, true)],
        }
    }

    const BELOW: MlBins = MlBins {
        bb: 100,
        b: 100,
        t: 100,
        tt: 100,
    };
    const ABOVE: MlBins = MlBins {
        bb: 0,
        b: 0,
        t: 0,
        tt: 0,
    };
    const WITHIN: MlBins = MlBins {
        bb: 0,
        b: 0,
        t: 100,
        tt: 100,
    };

    #[test]
    fn plateau_inputs_classify_each_class() {
        // (name, class, inputs (smz, zdr, rho, kdp, phi, sdz, sdp, smv), zone)
        let cases: [(&str, usize, [f64; 8], MlBins); 10] = [
            (
                "RA",
                RA,
                [30.0, 1.0, 0.99, NO_DATA, 60.0, 1.0, 5.0, NO_DATA],
                BELOW,
            ),
            (
                "HR",
                HR,
                // 47 dBZ, ZDR on the HR plateau at that Z, KDP 1.6 °/km
                // (LKdp ≈ 2, on the g-plateau [−6.4, 1.5]).
                [47.0, 2.0, 0.98, 1.6, 60.0, 1.0, 5.0, NO_DATA],
                BELOW,
            ),
            (
                "RH",
                RH,
                // 55 dBZ hail mixed with rain: ZDR under the f1 plateau's
                // edge, huge KDP, depressed rho.
                [55.0, 0.5, 0.93, 4.0, 60.0, 1.0, 5.0, NO_DATA],
                BELOW,
            ),
            (
                "BD",
                BD,
                // Big drops: Z 35, ZDR on the (f2, f3) plateau at 35 dBZ
                // (2.57–4.35 dB).
                [35.0, 3.0, 0.98, NO_DATA, 60.0, 1.0, 5.0, NO_DATA],
                BELOW,
            ),
            (
                "BI",
                BI,
                // Biological: weak Z, big ZDR, low rho, rough textures.
                [15.0, 5.0, 0.7, NO_DATA, 60.0, 3.0, 20.0, NO_DATA],
                BELOW,
            ),
            (
                "GC",
                GC,
                // Clutter: strong Z, near-zero velocity, low rho, very
                // rough textures.
                [45.0, 0.0, 0.8, NO_DATA, 60.0, 12.0, 45.0, 0.5],
                BELOW,
            ),
            (
                "DS",
                DS,
                [25.0, 0.25, 0.99, NO_DATA, 60.0, 1.0, 5.0, NO_DATA],
                ABOVE,
            ),
            (
                "WS",
                WS,
                [33.0, 1.5, 0.93, NO_DATA, 60.0, 1.0, 5.0, NO_DATA],
                WITHIN,
            ),
            (
                "IC",
                IC,
                // Crystals: weak Z, enhanced ZDR (past DS's plateau so DS
                // cannot tie), LKdp on the (0, 10) plateau via KDP 2 °/km.
                [10.0, 1.5, 0.99, 2.0, 60.0, 1.0, 5.0, NO_DATA],
                ABOVE,
            ),
            (
                "GR",
                GR,
                [40.0, 0.0, 0.99, NO_DATA, 60.0, 1.0, 5.0, NO_DATA],
                WITHIN,
            ),
        ];
        for (name, class, [smz, zdr, rho, kdp, phi, sdz, sdp, smv], zone) in cases {
            let f = fields_one_gate(smz, zdr, rho, kdp, phi, sdz, sdp, smv, 30.0);
            assert_eq!(
                classify_gate(&f, 0, zone, 100.0),
                class,
                "{name} plateau inputs must classify {name}",
            );
        }
    }

    /// Every class × variable trapezoid reads 1 at its plateau centre and
    /// 0 at and beyond both edges — the edge sweep the plateau test above
    /// leans on, pinned per table so a wrong row localizes.
    #[test]
    fn each_membership_row_peaks_on_its_plateau_and_dies_at_the_edges() {
        // A mid-range Z keeps every 2-D row monotonic.
        let z_ref = 35.0;
        for class in RA..=GR {
            for input in 0..NUM_FL_INPUTS {
                let p = set_membership_points(class, input, z_ref, 0.0, 100.0);
                let name = format!("class {class} input {input}");
                if p[0] > p[1] || p[1] > p[2] || p[2] > p[3] {
                    continue; // degenerate at this Z; the guard returns 0
                }
                let mid = 0.5 * (p[1] + p[2]);
                assert_eq!(degree_membership(mid, p), 1.0, "{name} plateau");
                assert_eq!(degree_membership(p[0], p), 0.0, "{name} lower edge");
                assert_eq!(degree_membership(p[3], p), 0.0, "{name} upper edge");
                assert_eq!(degree_membership(p[0] - 1.0, p), 0.0, "{name} below");
                assert_eq!(degree_membership(p[3] + 1.0, p), 0.0, "{name} above");
            }
        }
    }

    /// Low SNR is no-echo; a hopeless gate (nothing scores) is unknown.
    #[test]
    fn low_snr_is_ne_and_hopeless_gates_are_unknown() {
        let f = fields_one_gate(30.0, 1.0, 0.99, NO_DATA, 60.0, 1.0, 5.0, NO_DATA, 3.0);
        assert_eq!(classify_gate(&f, 0, BELOW, 100.0), NE, "SNR 3 dB < 5");
        let f = fields_one_gate(30.0, 1.0, 0.99, NO_DATA, 60.0, 1.0, 5.0, NO_DATA, NO_DATA);
        assert_eq!(classify_gate(&f, 0, BELOW, 100.0), NE, "missing SNR");
        // φ missing zeroes the φ-driven qualities; textures far outside
        // every plateau zero the rest: nothing reaches min_Agg → UK.
        let f = fields_one_gate(30.0, 3.0, 0.5, NO_DATA, NO_DATA, 20.0, 80.0, NO_DATA, 30.0);
        assert_eq!(classify_gate(&f, 0, BELOW, 100.0), UK);
    }

    // ── End-to-end synthetics through compute_hca ──────────────────────────

    /// A clean rain field below the melting layer: interior gates read RA
    /// (code 60) end to end, on a super-res sweep whose half-degree pairs
    /// recombine to 1° first.
    #[test]
    fn a_rain_field_below_the_layer_classifies_ra_end_to_end() {
        let z = |_: usize| G::V(30.0);
        let zdr = |_: usize| G::V(1.0);
        let rho = |_: usize| G::V(0.99);
        let phi = |_: usize| G::V(60.0);
        let radials: Vec<Radial> = (0..720)
            .map(|k| {
                hca_radial(
                    0.25 + 0.5 * k as f64,
                    0.5,
                    0.5,
                    D_GATES,
                    &z,
                    &zdr,
                    &rho,
                    &phi,
                    None,
                )
            })
            .collect();
        let ml = MeltingLayer::flat(4.0);
        let derived = compute_hca(&radials, &params(), &ml, &hsda_far(), None).expect("computes");
        assert_eq!(derived.values.len(), 360, "720 half-degree radials pair");
        assert!((derived.azimuths_deg[0] - 0.5).abs() < 1e-6);
        assert!((derived.gate_interval_km - 0.25).abs() < 1e-9);
        let row = &derived.values[100];
        for (i, &v) in row.iter().enumerate().take(300).skip(20) {
            assert_eq!(v, 60.0, "gate {i}: rain must read RA, got {v}");
        }
    }

    /// The same field pushed above the melting layer reads dry snow — the
    /// height gating flips the class with identical moments.
    #[test]
    fn the_melting_layer_flips_rain_to_dry_snow_above_the_top() {
        let z = |_: usize| G::V(25.0);
        let zdr = |_: usize| G::V(0.25);
        let rho = |_: usize| G::V(0.99);
        let phi = |_: usize| G::V(60.0);
        let radials: Vec<Radial> = (0..360)
            .map(|k| {
                hca_radial(
                    0.5 + k as f64,
                    1.0,
                    0.5,
                    D_GATES,
                    &z,
                    &zdr,
                    &rho,
                    &phi,
                    None,
                )
            })
            .collect();
        let below = compute_hca(
            &radials,
            &params(),
            &MeltingLayer::flat(6.0),
            &hsda_far(),
            None,
        )
        .expect("computes");
        let above = compute_hca(
            &radials,
            &params(),
            &MeltingLayer::flat(0.0),
            &hsda_far(),
            None,
        )
        .expect("computes");
        let i = 200;
        assert_eq!(below.values[0][i], 60.0, "below the layer this is rain");
        assert_eq!(
            above.values[0][i], 40.0,
            "above the layer the same moments are dry snow",
        );
    }

    /// Gates with no reflectivity are no-echo and decode as undefined; the
    /// polar grid mirrors the twin comparator's resampling.
    #[test]
    fn missing_reflectivity_is_no_echo_and_the_grid_is_undefined_there() {
        let z = |i: usize| if i < 100 { G::V(30.0) } else { G::Nd };
        let zdr = |_: usize| G::V(1.0);
        let rho = |_: usize| G::V(0.99);
        let phi = |_: usize| G::V(60.0);
        let radials: Vec<Radial> = (0..360)
            .map(|k| {
                hca_radial(
                    0.5 + k as f64,
                    1.0,
                    0.5,
                    D_GATES,
                    &z,
                    &zdr,
                    &rho,
                    &phi,
                    None,
                )
            })
            .collect();
        let derived = compute_hca(
            &radials,
            &params(),
            &MeltingLayer::flat(4.0),
            &hsda_far(),
            None,
        )
        .expect("computes");
        assert!(derived.values[0][50].is_finite());
        assert!(
            derived.values[0][150].is_nan(),
            "no reflectivity → NE → undefined",
        );
        let grid = derived.to_polar_grid();
        assert_eq!(grid[0][5], 60.0);
        assert!(grid[0][50].is_nan(), "the NE stretch stays undefined");
    }

    /// Without the calibration constant the SNR gate cannot run and every
    /// gate is no-echo — the documented failure mode, not a panic.
    #[test]
    fn without_dbz0_everything_is_no_echo() {
        let z = |_: usize| G::V(30.0);
        let zdr = |_: usize| G::V(1.0);
        let rho = |_: usize| G::V(0.99);
        let phi = |_: usize| G::V(60.0);
        let radials = vec![hca_radial(
            0.5, 1.0, 0.5, D_GATES, &z, &zdr, &rho, &phi, None,
        )];
        let p = KdpParams {
            init_fdp_deg: Some(60.0),
            ..KdpParams::default()
        };
        let derived = compute_hca(&radials, &p, &MeltingLayer::flat(4.0), &hsda_far(), None)
            .expect("computes");
        assert!(derived.values[0].iter().all(|v| v.is_nan()));
    }

    /// The split-cut merge grafts the Doppler cut's velocity onto the
    /// surveillance radials by azimuth — the RPG's combined base data.
    #[test]
    fn merge_split_cut_doppler_grafts_velocity_by_azimuth() {
        let z = |_: usize| G::V(30.0);
        let zdr = |_: usize| G::V(1.0);
        let rho = |_: usize| G::V(0.99);
        let phi = |_: usize| G::V(60.0);
        let vel = |_: usize| G::V(3.0);
        let cs: Vec<Radial> = (0..8)
            .map(|k| hca_radial(0.5 + k as f64, 1.0, 0.5, 40, &z, &zdr, &rho, &phi, None))
            .collect();
        // The Doppler partner misses azimuth 3.5 entirely.
        let cd: Vec<Radial> = (0..8)
            .filter(|&k| k != 3)
            .map(|k| {
                hca_radial(
                    0.5 + k as f64,
                    1.0,
                    0.5,
                    40,
                    &z,
                    &zdr,
                    &rho,
                    &phi,
                    Some(&vel),
                )
            })
            .collect();
        let merged = merge_split_cut_doppler(&cs, &cd);
        assert_eq!(merged.len(), cs.len());
        for (k, r) in merged.iter().enumerate() {
            assert_eq!(r.azimuth_angle_degrees(), cs[k].azimuth_angle_degrees());
            if k == 3 {
                assert!(r.velocity().is_none(), "no partner within half a spacing");
            } else {
                assert!(r.velocity().is_some(), "radial {k} must gain velocity");
                assert!(r.spectrum_width().is_none(), "cd carried no SW here");
            }
            assert!(
                r.differential_phase().is_some(),
                "DP fields stay the CS cut's"
            );
        }
        // A surveillance radial that already carries velocity passes
        // through untouched.
        let already: Vec<Radial> = (0..2)
            .map(|k| {
                hca_radial(
                    0.5 + k as f64,
                    1.0,
                    0.5,
                    40,
                    &z,
                    &zdr,
                    &rho,
                    &phi,
                    Some(&vel),
                )
            })
            .collect();
        let merged = merge_split_cut_doppler(&already, &cd);
        assert!(merged.iter().all(|r| r.velocity().is_some()));
    }

    // ── Melting layer construction and detection ───────────────────────────

    /// The default layer from the environmental 0 °C height: km MSL in,
    /// km ARL out, 0.5 km deep, floored at ground.
    #[test]
    fn the_default_layer_comes_from_the_zero_c_height() {
        let ml = MeltingLayer::from_zero_c_height(4.2, 0.2);
        assert!((ml.top_km_arl[0] - 4.0).abs() < 1e-12);
        assert!((ml.bottom_km_arl[123] - 3.5).abs() < 1e-12);
        let winter = MeltingLayer::from_zero_c_height(0.1, 0.3);
        assert_eq!(winter.top_km_arl[0], 0.0, "below-ground tops floor at 0");
        assert_eq!(winter.bottom_km_arl[0], 0.0);
        assert!(
            (DEFAULT_HEIGHT_0_KM_MSL - 3.2004).abs() < 1e-9,
            "the source's hardcoded height_0 fallback is 10.5 kft",
        );
    }

    /// The percentile read-off of `Calculate_melting_layer`, on a
    /// hand-built histogram: uniform weight 100 over height indices
    /// 25..=32 at every azimuth gives, through the ±10° window (total
    /// 16800 per azimuth), bottom = 2.65 km (first crossing of 0.2) and
    /// top = 3.15 km (first crossing of 0.8).
    #[test]
    fn the_percentile_read_off_matches_the_hand_computation() {
        let mut weight = vec![[0.0f64; ML_MAX_HEIGHTS]; 360];
        for az in weight.iter_mut() {
            for cell in az[25..=32].iter_mut() {
                *cell = 100.0;
            }
        }
        let ml = calculate_melting_layer(&weight, 2.8, &MeltingLayer::flat(2.8));
        for az in 0..360 {
            assert!((ml.top_km_arl[az] - 3.15).abs() < 1e-9, "top at az {az}");
            assert!(
                (ml.bottom_km_arl[az] - 2.65).abs() < 1e-9,
                "bottom at az {az}",
            );
        }
        // Under the min_wet_snow_sum floor nothing detects and the
        // default flat layer comes back.
        let mut thin = vec![[0.0f64; ML_MAX_HEIGHTS]; 360];
        for az in thin.iter_mut() {
            az[28] = 1.0;
        }
        let ml = calculate_melting_layer(&thin, 2.8, &MeltingLayer::flat(2.8));
        assert_eq!(ml.top_km_arl[0], 2.8);
        assert_eq!(ml.bottom_km_arl[0], 2.3);
        // The ±1 km clip: weight piled far from the previous top is zeroed
        // before the percentiles.
        let mut far = vec![[0.0f64; ML_MAX_HEIGHTS]; 360];
        for az in far.iter_mut() {
            for cell in az[60..=70].iter_mut() {
                *cell = 1000.0;
            }
        }
        let ml = calculate_melting_layer(&far, 2.8, &MeltingLayer::flat(2.8));
        assert_eq!(
            ml.top_km_arl[0], 2.8,
            "weight outside ±2·depth of the previous top is clipped",
        );
    }

    /// Azimuth gaps interpolate between the valid neighbours around the
    /// circle.
    #[test]
    fn melting_layer_gaps_interpolate_between_valid_azimuths() {
        let mut weight = vec![[0.0f64; ML_MAX_HEIGHTS]; 360];
        // Valid detections at azimuths 0..=99 (top 3.15) and 200..=299
        // (top 2.75; indices 21..=28).
        for row in weight.iter_mut().take(100) {
            for cell in row[25..=32].iter_mut() {
                *cell = 100.0;
            }
        }
        for row in weight.iter_mut().take(300).skip(200) {
            for cell in row[21..=28].iter_mut() {
                *cell = 100.0;
            }
        }
        let ml = calculate_melting_layer(&weight, 2.8, &MeltingLayer::flat(2.8));
        // Deep inside each run the windowed sums are pure.
        assert!((ml.top_km_arl[50] - 3.15).abs() < 1e-9);
        assert!((ml.top_km_arl[250] - 2.75).abs() < 1e-9);
        // The gap between the runs interpolates monotonically.
        let a = ml.top_km_arl[120];
        let b = ml.top_km_arl[150];
        let c = ml.top_km_arl[180];
        assert!(
            a > b && b > c,
            "gap must slope from 3.15 toward 2.75: {a} {b} {c}"
        );
        assert!(ml.top_km_arl.iter().all(|t| t.is_finite()));
    }

    /// The full MLDA on synthetic 4°–10° sweeps: a wet-snow ring (Z 33,
    /// ZDR 1.5, ρ 0.93) painted where the beam sits between 2.5 and
    /// 2.95 km, rain below it, dry snow above it. Three tilts accumulate
    /// past the 1500 floor and the detected layer lands on the ring.
    #[test]
    fn detect_melting_layer_finds_the_wet_snow_ring() {
        let make_sweep = |elev: f64| -> Vec<Radial> {
            (0..360)
                .map(|k| {
                    let h = move |i: usize| ml_height_from_range(elev, i as f64 * 0.25);
                    let z = move |i: usize| {
                        let h = h(i);
                        if h < 2.5 {
                            G::V(30.0)
                        } else if h < 2.95 {
                            G::V(33.0)
                        } else if h < 5.0 {
                            G::V(25.0)
                        } else {
                            G::Nd
                        }
                    };
                    let zdr = move |i: usize| {
                        let h = h(i);
                        if h < 2.5 {
                            G::V(1.0)
                        } else if h < 2.95 {
                            G::V(1.5)
                        } else {
                            G::V(0.25)
                        }
                    };
                    let rho = move |i: usize| {
                        let h = h(i);
                        if (2.5..2.95).contains(&h) {
                            G::V(0.93)
                        } else {
                            G::V(0.99)
                        }
                    };
                    let phi = |_: usize| G::V(60.0);
                    hca_radial(
                        0.5 + k as f64,
                        1.0,
                        elev as f32,
                        D_GATES,
                        &z,
                        &zdr,
                        &rho,
                        &phi,
                        None,
                    )
                })
                .collect()
        };
        let sweeps: Vec<Vec<Radial>> = [4.5, 5.5, 6.5].iter().map(|&e| make_sweep(e)).collect();
        let sweep_refs: Vec<&[Radial]> = sweeps.iter().map(|s| s.as_slice()).collect();
        let ml = detect_melting_layer(&sweep_refs, &params(), 2.75, &hsda_far(), None);
        for az in [0usize, 90, 180, 270] {
            assert!(
                (2.6..=3.3).contains(&ml.top_km_arl[az]),
                "top at az {az}: {}",
                ml.top_km_arl[az],
            );
            assert!(
                (2.3..=2.9).contains(&ml.bottom_km_arl[az]),
                "bottom at az {az}: {}",
                ml.bottom_km_arl[az],
            );
            assert!(ml.top_km_arl[az] > ml.bottom_km_arl[az]);
        }
        // A quiet volume detects nothing and returns the default.
        let quiet = detect_melting_layer(&[], &params(), 2.75, &hsda_far(), None);
        assert_eq!(quiet.top_km_arl[0], 2.75);
        assert_eq!(quiet.bottom_km_arl[0], 2.25);
    }

    // ── Hail size discrimination (HailSize.cpp v3) ─────────────────────────

    /// A `Fields` fixture of `n` identical gates for the HSDA.
    fn fields_n(n: usize, smz: f64, zdr: f64, rho: f64) -> Fields {
        let q = quality_indices(60.0, rho, smz, 30.0, true);
        Fields {
            az: 0.5,
            elev: 0.5,
            hatt: false,
            n,
            dg: 0.25,
            smz: vec![smz; n],
            snr: vec![30.0; n],
            sdz: vec![1.0; n],
            zdr: vec![zdr; n],
            rho: vec![rho; n],
            kdp: vec![NO_DATA; n],
            phi: vec![60.0; n],
            sdp: vec![5.0; n],
            smv: vec![NO_DATA; n],
            met: vec![f64::NAN; n],
            q: vec![q; n],
        }
    }

    /// Deep below the wet-bulb zero (regime 5), a 65 dBZ / −1 dB / ρ 0.90
    /// core is giant hail on every trapezoid: PV = 1 across Z/ZDR/ρ, so
    /// the aggregation clears 0.6 and a run of 4 survives the despeckle.
    #[test]
    fn hsda_subclasses_a_giant_hail_core() {
        let f = fields_n(4, 65.0, -1.0, 0.90);
        let classes = vec![RH; 4];
        let sub = hail_size_radial(&f, &classes, &hsda_far());
        assert_eq!(sub, vec![HailSize::Giant; 4]);
        assert_eq!(external_code(RH, HailSize::Giant), 120.0, "GH code");
        assert_eq!(external_code(RH, HailSize::Large), 110.0, "LH code");
        assert_eq!(
            external_code(RH, HailSize::Small),
            100.0,
            "small hail keeps RH's code",
        );
        assert_eq!(external_code(RH, HailSize::Current), 100.0);
        assert_eq!(external_code(RA, HailSize::NotHail), 60.0);
    }

    /// ZDR at or above 2 dB is never large or giant hail — the hard limit
    /// forces small regardless of the aggregation.
    #[test]
    fn hsda_zdr_hard_limit_forces_small() {
        let f = fields_n(4, 65.0, 2.5, 0.90);
        let classes = vec![RH; 4];
        let sub = hail_size_radial(&f, &classes, &hsda_far());
        assert_eq!(sub, vec![HailSize::Small; 4]);
    }

    /// A weak aggregation (nothing reaches 0.6) leaves the gate at RH, and
    /// non-RH gates are never touched.
    #[test]
    fn hsda_leaves_weak_gates_and_other_classes_alone() {
        // 46 dBZ with ZDR 1.9: the small-hail ZDR trapezoid tops out below
        // 1.9 in regime 5, Z sits on the shoulder — no size concludes.
        let f = fields_n(3, 46.0, 1.9, 0.97);
        let classes = vec![RH, RA, RH];
        let sub = hail_size_radial(&f, &classes, &hsda_far());
        assert_eq!(
            sub,
            vec![HailSize::Current, HailSize::NotHail, HailSize::Current],
        );
    }

    /// A single giant gate inside a large-hail run despeckles down to
    /// large (`min_data_size = 2`).
    #[test]
    fn hsda_despeckles_single_gate_giant_runs() {
        // Large-hail pattern in regime 5: Z 57, ZDR 0, ρ 0.94 — the giant
        // trapezoids score lower than large there.
        let mut f = fields_n(3, 57.0, 0.0, 0.94);
        // The middle gate is unambiguous giant.
        f.smz[1] = 65.0;
        f.zdr[1] = -1.0;
        f.rho[1] = 0.90;
        f.q[1] = quality_indices(60.0, 0.90, 65.0, 30.0, true);
        let classes = vec![RH; 3];
        let sub = hail_size_radial(&f, &classes, &hsda_far());
        assert_eq!(sub[1], HailSize::Large, "giant run of 1 demotes to large");
        assert_eq!(
            sub,
            vec![HailSize::Large; 3],
            "then a large run of 3 stands"
        );
    }

    /// The height regimes move the verdict: the same moments that read
    /// giant near the surface read differently above the wet-bulb zero,
    /// where the dry-hail trapezoids apply.
    #[test]
    fn hsda_regimes_follow_the_wet_bulb_heights() {
        // ZDR 0.4 / ρ 0.97 at 60 dBZ: below tw0−3 the giant ZDR plateau
        // tops at f3 + 0.3 = 0.5 − 0.5 + 0.3... regime 5 f3(60) = −0.5, so
        // giant ZDR range is (−8.75, −7.75, −0.5, −0.2): 0.4 reads 0. The
        // large plateau [f3, f2] = [−0.5, 0.5] holds 0.4 → large wins low.
        let f = fields_n(2, 60.0, 0.4, 0.97);
        let classes = vec![RH; 2];
        let low = hail_size_radial(&f, &classes, &hsda_far());
        assert_eq!(low, vec![HailSize::Large; 2]);
        // Push the whole column above the wet-bulb −25 °C level (regime
        // 0): ZDR 0.4 sits on the small/large plateau edge (−0.5..0.5 with
        // x3 = 0.3, shoulder to 0.5) but ρ 0.97 → small/large ρ plateau
        // (0.96..0.99) → both score; small ties large through Z (60 on
        // both plateaus) and the strict `>` keeps small.
        let cold = HsdaHeights {
            tw0_km_arl: -2.0,
            twm25_km_arl: -1.0,
        };
        let high = hail_size_radial(&f, &classes, &cold);
        assert_eq!(high, vec![HailSize::Small; 2]);
    }
}

/// Offline pins on the validation policy — everything the ignored live test
/// decides with.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod policy_tests {
    use super::validation_policy as policy;
    use crate::twin::compare::{Tally, ValueCodec};
    use nexrad_level3::model::{RadialPacket, RadialRun};

    #[test]
    fn the_class_bar_is_what_the_campaign_set() {
        assert_eq!(policy::EXACT_PCT, 85.0);
        assert_eq!(policy::COMPATIBLE_PCT, 95.0);
        assert_eq!(policy::MIN_SITES, 3);
        assert_eq!(policy::MIN_DEFINED_GATES, 10_000);
        assert_eq!(policy::MIN_TWIN_DEFINED_GATES, 500);
        assert!(policy::meets_class_bar(85.0, 95.0), "inclusive");
        assert!(!policy::meets_class_bar(84.999, 100.0), "both legs bind");
        assert!(!policy::meets_class_bar(100.0, 94.999));
    }

    /// The compatible pairs are exactly the documented three, symmetric,
    /// and nothing else.
    #[test]
    fn the_compatible_pairs_are_the_documented_three() {
        assert_eq!(policy::COMPATIBLE_PAIRS, &[(50, 90), (60, 80), (60, 70)]);
        assert!(policy::is_compatible(50, 90) && policy::is_compatible(90, 50));
        assert!(policy::is_compatible(60, 80) && policy::is_compatible(80, 60));
        assert!(policy::is_compatible(60, 70) && policy::is_compatible(70, 60));
        assert!(policy::is_compatible(40, 40), "equality is compatible");
        assert!(!policy::is_compatible(70, 80), "HR↔BD is not a pair");
        assert!(!policy::is_compatible(40, 50), "DS↔WS is not a pair");
        assert!(!policy::is_compatible(100, 60), "RH↔RA is not a pair");
    }

    #[test]
    fn compatible_pct_reads_the_confusion_matrix() {
        let mut t = Tally {
            compared: 100,
            exact: 70,
            ..Tally::default()
        };
        t.confusion.insert((60, 60), 70);
        t.confusion.insert((80, 60), 20);
        t.confusion.insert((90, 60), 10);
        assert_eq!(policy::compatible_pct(&t), 90.0, "70 exact + 20 BD↔RA");
    }

    #[test]
    fn a_run_is_conclusive_only_with_enough_sites_and_gates() {
        assert!(policy::sample_is_conclusive(3, 10_000));
        assert!(!policy::sample_is_conclusive(2, 1_000_000), "sites gate");
        assert!(!policy::sample_is_conclusive(20, 9_999), "gates gate");
    }

    #[test]
    fn near_empty_twins_are_skipped_not_scored() {
        assert!(!policy::volume_is_scoreable(499));
        assert!(policy::volume_is_scoreable(500));
    }

    /// The quarantine table carries exactly the 2026-07-29 precipitation
    /// survey's four evidence-backed entries, each still measured, each
    /// `why` naming at least two volumes (the ≥2-volumes/≥2-runs rule
    /// leaves its trace as multiple slash-separated figures).
    #[test]
    fn the_quarantine_table_matches_the_precipitation_survey_evidence() {
        let sites: Vec<&str> = policy::QUARANTINED.iter().map(|q| q.site).collect();
        assert_eq!(
            sites,
            ["KFSD", "KMRX", "KSFX", "KMTX"],
            "quarantine entries changed: they need evidence from ≥2 volumes \
             across ≥2 runs recorded in the `why`, per the table's doc",
        );
        for q in policy::QUARANTINED {
            assert!(
                crate::twin::live::SITES.contains(&q.site),
                "{} ({:?}) is quarantined but no longer measured: {}",
                q.site,
                q.scope,
                q.why,
            );
            assert!(
                q.why.contains("compatible") && q.why.contains('/'),
                "{}'s why must record the multi-volume compatible evidence",
                q.site,
            );
            assert!(!policy::site_is_asserted(q.site));
        }
        assert!(policy::site_is_asserted("KTLX"));
        assert!(policy::site_is_asserted("KUEX"));
        assert_eq!(format!("{:?}", policy::Scope::Whole), "Whole");
    }

    /// Scoreability decodes through the twin's own codec: the class codes
    /// count, level 0 (ND/NE) does not.
    #[test]
    fn twin_defined_gates_counts_codec_defined_gates_only() {
        let codec = ValueCodec::Scaled {
            scale: 1.0,
            offset: 0.0,
        };
        let packet = RadialPacket {
            first_range_bin: 0,
            num_range_bins: 6,
            i_center: 0,
            j_center: 0,
            scale_factor: 1.0,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials: vec![RadialRun {
                start_angle: 0.0,
                angle_delta: 1.0,
                gate_values: vec![0, 10, 60, 140, 150, 0],
            }],
        };
        assert_eq!(policy::twin_defined_gates(&packet, &codec), 4);
    }
}

/// The live twin harness: score the derivation against the RPG's own N0H
/// for the **same volume and cut**, across [`crate::twin::live::SITES`], as
/// class codes with the full confusion matrix.
///
/// ```text
/// cargo test -p rustdar-radar --release --lib -- --ignored --nocapture live_derived_hca
/// ```
///
/// # Site-diversity protocol (campaign directive)
///
/// A/B arbitration is **decided** on the climatologically diverse tuning
/// subset — KTLX (southern plains), KMLB (Florida coast), KMTX (mountain
/// west), KMPX (upper midwest), KSHV (gulf south) — and the winner is
/// **confirmed** on the holdout sites KFSD, KMRX, KTLH, KDDC, KAMA, which
/// play no part in the choice; both figures are reported separately. The
/// survey itself always runs the full roster with per-site bars, never
/// pooled.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod live_validation {
    use super::validation_policy as policy;
    use super::{HcaOptions, MeltingLayer};
    use crate::kdp::KdpParams;
    use crate::sources::DataSources;
    use crate::twin::{compare, live};
    use nexrad_model::data::Radial;

    /// A short name for a product class code, for the confusion print.
    fn class_name(code: u8) -> &'static str {
        match code {
            0 => "ND",
            10 => "BI",
            20 => "GC",
            30 => "IC",
            40 => "DS",
            50 => "WS",
            60 => "RA",
            70 => "HR",
            80 => "BD",
            90 => "GR",
            100 => "RH",
            110 => "LH",
            120 => "GH",
            140 => "UK",
            150 => "RF",
            _ => "??",
        }
    }

    /// The largest off-diagonal confusion cells, as `ours→twin`.
    fn top_confusions(tally: &compare::Tally, n: usize) -> String {
        let mut pairs: Vec<(usize, u8, u8)> = tally
            .confusion
            .iter()
            .filter(|&(&(a, b), _)| a != b)
            .map(|(&(a, b), &c)| (c, a, b))
            .collect();
        pairs.sort_unstable_by(|a, b| b.cmp(a));
        pairs
            .into_iter()
            .take(n)
            .map(|(c, a, b)| {
                format!(
                    "{}→{} {:.1}%",
                    class_name(a),
                    class_name(b),
                    100.0 * c as f64 / tally.compared.max(1) as f64,
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Diagnostic: where the presence disagreement lives, and both sides'
    /// class distributions. The twin-defined / derived-undefined cells are
    /// split by our censoring cause — no reflectivity at the gate, SNR
    /// under 5 dB with reflectivity present, or no covering radial — and
    /// the marginal class histograms say which classes carry the
    /// disagreement. Pure printing; no decision reads it.
    fn presence_diagnostic(
        radials: &[Radial],
        params: &KdpParams,
        grid: &[Vec<f32>],
        packet: &nexrad_level3::model::RadialPacket,
        gate_km: f64,
    ) -> String {
        use crate::dpprep::{DpInput, combine_sweep_dp, resample_to_polar_grid};
        let inputs: Vec<DpInput> = radials.iter().filter_map(DpInput::from_radial).collect();
        if inputs.is_empty() {
            return String::new();
        }
        let width = if inputs[0].half_degree {
            1.0
        } else {
            inputs[0].spacing
        };
        let combined = combine_sweep_dp(&inputs, true);
        let init_fdp = super::resolve_init_fdp(params, &combined, false);
        let dbz0 = params.dbz0.map(f64::from);
        let atmos = params.atmos_db_per_km.map(f64::from);
        let mut cause: Vec<Vec<f32>> = Vec::new();
        let mut azs = Vec::new();
        let (mut first_gate, mut gate_int) = (0.125, 0.25);
        for c in &combined {
            let f = super::radial_fields(c, init_fdp, dbz0, atmos, true, true, None);
            cause.push(
                (0..f.n)
                    .map(|i| {
                        if f.smz[i] == super::NO_DATA {
                            2.0
                        } else if f.snr[i] < super::MIN_SNR {
                            3.0
                        } else {
                            1.0
                        }
                    })
                    .collect(),
            );
            azs.push(c.base.az);
            first_gate = c.base.dr0;
            gate_int = c.base.dg;
        }
        let cause_grid = resample_to_polar_grid(&cause, &azs, first_gate, gate_int, width);

        // The twin's cells, resampled exactly as the tally resamples.
        let mut slots: Vec<Option<usize>> = vec![None; 3600];
        for (i, run) in packet.radials.iter().enumerate() {
            let start = (run.start_angle as f64 * 10.0).round() as i32;
            let w = (run.angle_delta as f64 * 10.0).round().max(1.0) as i32;
            for k in 0..w {
                slots[(start + k).rem_euclid(3600) as usize] = Some(i);
            }
        }
        let n_gates = packet
            .radials
            .iter()
            .map(|r| r.gate_values.len())
            .max()
            .unwrap_or(0);
        let mut gate_for_bin: Vec<Option<usize>> = vec![None; crate::volumetric::RANGE_BINS];
        let mut best = vec![f64::INFINITY; crate::volumetric::RANGE_BINS];
        for j in 0..n_gates {
            let centre = (packet.first_range_bin as f64 + j as f64 + 0.5) * gate_km;
            let bin = centre.floor() as i64;
            if !(0..crate::volumetric::RANGE_BINS as i64).contains(&bin) {
                continue;
            }
            let d = (centre - (bin as f64 + 0.5)).abs();
            if d < best[bin as usize] {
                best[bin as usize] = d;
                gate_for_bin[bin as usize] = Some(j);
            }
        }

        let (mut no_z, mut low_snr, mut uncovered) = (0usize, 0usize, 0usize);
        let mut twin_hist = [0usize; 16];
        let mut twin_missed_hist = [0usize; 16];
        let mut ours_hist = [0usize; 16];
        for az in 0..360usize {
            let run = slots[az * 10 + 5].map(|ri| &packet.radials[ri]);
            for r in 0..crate::volumetric::RANGE_BINS {
                let twin_code: Option<u16> = run
                    .and_then(|run| gate_for_bin[r].and_then(|j| run.gate_values.get(j).copied()))
                    .filter(|&g| g > 1);
                let ours = grid[az][r];
                if ours.is_finite() {
                    ours_hist[((ours as usize) / 10).min(15)] += 1;
                }
                let Some(code) = twin_code else { continue };
                twin_hist[(code as usize / 10).min(15)] += 1;
                if ours.is_finite() {
                    continue;
                }
                twin_missed_hist[(code as usize / 10).min(15)] += 1;
                let c = cause_grid[az][r];
                if c == 2.0 {
                    no_z += 1;
                } else if c == 3.0 {
                    low_snr += 1;
                } else if c != 1.0 {
                    uncovered += 1;
                }
            }
        }
        let hist = |h: &[usize; 16]| -> String {
            let total: usize = h.iter().sum();
            let mut items: Vec<(usize, usize)> = h
                .iter()
                .enumerate()
                .filter(|&(_, &c)| c > 0)
                .map(|(i, &c)| (c, i))
                .collect();
            items.sort_unstable_by(|a, b| b.cmp(a));
            items
                .into_iter()
                .take(6)
                .map(|(c, i)| {
                    format!(
                        "{} {:.0}%",
                        class_name((i * 10) as u8),
                        100.0 * c as f64 / total.max(1) as f64,
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!(
            "twin-only cells: no-Z {no_z} low-SNR {low_snr} uncovered {uncovered} | twin there: \
             [{}] | ours all: [{}] | twin all: [{}]",
            hist(&twin_missed_hist),
            hist(&ours_hist),
            hist(&twin_hist),
        )
    }

    /// Per-class producer accuracy (P(ours = c | twin = c)) and user
    /// accuracy (P(twin = c | ours = c)), from the confusion matrix.
    fn per_class_accuracy(tally: &compare::Tally) -> String {
        use std::collections::BTreeMap;
        let mut twin_totals: BTreeMap<u8, usize> = BTreeMap::new();
        let mut ours_totals: BTreeMap<u8, usize> = BTreeMap::new();
        for (&(a, b), &c) in &tally.confusion {
            *ours_totals.entry(a).or_insert(0) += c;
            *twin_totals.entry(b).or_insert(0) += c;
        }
        let mut parts = Vec::new();
        for (&code, &twin_n) in &twin_totals {
            let diag = tally.confusion.get(&(code, code)).copied().unwrap_or(0);
            let ours_n = ours_totals.get(&code).copied().unwrap_or(0);
            parts.push(format!(
                "{} n={} prod {:.0}% user {:.0}%",
                class_name(code),
                twin_n,
                100.0 * diag as f64 / twin_n.max(1) as f64,
                100.0 * diag as f64 / ours_n.max(1) as f64,
            ));
        }
        parts.join(" | ")
    }

    /// The survey's site-hours: `HCA_SITE_HOURS` as comma/semicolon
    /// separated `SITE=YYYY-MM-DDTHH:MM` pairs targets precipitating
    /// archive hours (a site may appear more than once); unset, the full
    /// roster at now — the clear-air fallback.
    fn site_hours() -> Vec<(String, chrono::NaiveDateTime)> {
        let now = chrono::Utc::now().naive_utc();
        match std::env::var("HCA_SITE_HOURS") {
            Ok(spec) if !spec.trim().is_empty() => spec
                .split([',', ';'])
                .filter_map(|pair| {
                    let (site, when) = pair.trim().split_once('=')?;
                    let when = chrono::NaiveDateTime::parse_from_str(when.trim(), "%Y-%m-%dT%H:%M")
                        .unwrap_or_else(|e| panic!("bad HCA_SITE_HOURS entry {pair}: {e}"));
                    Some((site.trim().to_uppercase(), when))
                })
                .collect(),
            _ => live::SITES.iter().map(|s| (s.to_string(), now)).collect(),
        }
    }

    /// The site-hour **selection protocol**'s cheap precipitation check:
    /// for every roster site × candidate hour (`HCA_SCAN_HOURS`, comma
    /// separated ISO minutes), fetch the nearest archived volume and count
    /// the lowest cut's gates at or above 35 dBZ. Pure reconnaissance — it
    /// asserts nothing; its table picks the survey's `HCA_SITE_HOURS`.
    ///
    /// ```text
    /// HCA_SCAN_HOURS=2026-07-29T08:00,2026-07-28T21:00 \
    ///   cargo test -p rustdar-radar --release --lib -- --ignored \
    ///   --nocapture live_hca_precip_site_scan
    /// ```
    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_hca_precip_site_scan() {
        crate::tls::init();
        let hours: Vec<chrono::NaiveDateTime> = std::env::var("HCA_SCAN_HOURS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%dT%H:%M")
                    .unwrap_or_else(|e| panic!("bad HCA_SCAN_HOURS entry {s}: {e}"))
            })
            .collect();
        let hours = if hours.is_empty() {
            vec![chrono::Utc::now().naive_utc()]
        } else {
            hours
        };
        for &when in &hours {
            for &site in live::SITES {
                let Some((file, l2_start)) = live::l2_archive_near(site, when).await else {
                    println!("{when} {site}: no volume");
                    continue;
                };
                let Ok(scan) = file.scan() else {
                    println!("{when} {site}: volume failed to decode");
                    continue;
                };
                let lowest = scan.sweeps().first();
                let hot: usize = lowest
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
                println!(
                    "{when} {site}: vol {l2_start} VCP {:?} — {hot} gates ≥ 35 dBZ in the lowest cut",
                    scan.coverage_pattern_number(),
                );
            }
        }
    }

    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_derived_hca_matches_the_rpgs_own_product() {
        crate::tls::init();
        let sources = DataSources::production();

        let mut asserted_sites = 0usize;
        let mut pooled_compared = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for (site, when) in site_hours() {
            let site = site.as_str();
            let Some((file, l2_start)) = live::l2_archive_near(site, when).await else {
                println!("{site}: SKIP — no archived Level II volume found near {when}");
                continue;
            };
            let mut params = KdpParams::from_archive(&file);
            let Ok(scan) = file.scan() else {
                println!("{site}: SKIP — volume failed to decode");
                continue;
            };
            params.isdp_est_deg = crate::kdp::estimate_volume_isdp(&scan);

            // The environmental heights (the operational chain's `height_0`
            // and the HSDA's wet-bulb pair) from the WP-S sounding, with
            // the sources' own fallbacks; radar height from the site table.
            let radar_km_msl = crate::sites::get_radar_site(site)
                .and_then(|s| s.elev)
                .map(|ft| f64::from(ft) * 0.0003048)
                .unwrap_or(0.0);
            let env = match crate::sites::get_radar_site(site) {
                Some(s) => crate::sounding::fetch_env_heights(&sources, s.lat, s.lon).await,
                None => None,
            };
            let h0c = env
                .as_ref()
                .map(|e| e.h0c_km_msl)
                .unwrap_or(super::DEFAULT_HEIGHT_0_KM_MSL);
            let hsda = match &env {
                Some(e) => {
                    super::HsdaHeights::from_env_heights(e.h0c_km_msl, e.hm20c_km_msl, radar_km_msl)
                }
                None => super::HsdaHeights::operational_defaults(radar_km_msl),
            };
            let default_top_arl = (h0c - radar_km_msl).max(0.0);
            let ml_flat = MeltingLayer::from_zero_c_height(h0c, radar_km_msl);

            // The volume's reflectivity CAPPI (the met-signal rescue state;
            // built from the ≥ 1° dual-pol sweeps in scan order).
            let all_dp_sweeps: Vec<&[Radial]> = scan
                .sweeps()
                .iter()
                .map(|s| s.radials())
                .filter(|radials| {
                    radials
                        .first()
                        .map(|r| r.differential_phase().is_some())
                        .unwrap_or(false)
                })
                .collect();
            let cappi = super::build_refl_cappi(&all_dp_sweeps);

            // The volume's 4°–10° tilts feed the radar MLDA (the
            // operational accumulation spans 3 volumes and merges the
            // model grid — see the module doc's ML notes).
            let ml_sweeps: Vec<&[Radial]> = scan
                .sweeps()
                .iter()
                .map(|s| s.radials())
                .filter(|radials| {
                    radials
                        .first()
                        .map(|r| {
                            let e = f64::from(r.elevation_angle_degrees());
                            (4.0..=10.0).contains(&e)
                        })
                        .unwrap_or(false)
                })
                .collect();
            let ml_radar = super::detect_melting_layer(
                &ml_sweeps,
                &params,
                default_top_arl,
                &hsda,
                Some(&cappi),
            );
            let detected_azs = ml_radar
                .top_km_arl
                .iter()
                .zip(ml_flat.top_km_arl.iter())
                .filter(|(a, b)| (*a - *b).abs() > 1e-9)
                .count();
            let mean = |v: &[f64; 360]| v.iter().sum::<f64>() / 360.0;
            println!(
                "{site}: h0c {h0c:.2} km MSL ({}), radar {radar_km_msl:.2} km, default ML top \
                 {default_top_arl:.2} km ARL | Tw0 {:.2} Tw-25 {:.2} km ARL | radar MLDA {} \
                 ({} ML sweeps, {detected_azs}/360 az differ from flat) mean top {:.2} \
                 bottom {:.2}",
                if env.is_some() {
                    "sounding"
                } else {
                    "fallback"
                },
                hsda.tw0_km_arl,
                hsda.twm25_km_arl,
                if detected_azs > 0 {
                    "detected"
                } else {
                    "default"
                },
                ml_sweeps.len(),
                mean(&ml_radar.top_km_arl),
                mean(&ml_radar.bottom_km_arl),
            );

            // The tilts that could have generated N0H: the lowest sweeps
            // carrying differential phase, by elevation number — each
            // rebuilt as the RPG's combined base data (the split-cut
            // Doppler partner's velocity grafted in, so the GC velocity
            // kill runs as it does operationally).
            let mut candidates: Vec<(u8, f32, Vec<Radial>)> = Vec::new();
            for sweep in scan.sweeps() {
                let radials = sweep.radials();
                let Some(first) = radials
                    .iter()
                    .take(5)
                    .find(|r| r.differential_phase().is_some())
                else {
                    continue;
                };
                let eln = first.elevation_number();
                if candidates.iter().any(|(e, ..)| *e == eln) {
                    continue;
                }
                let angle = first.elevation_angle_degrees();
                let merged = if radials.iter().take(5).any(|r| r.velocity().is_some()) {
                    radials.to_vec()
                } else {
                    // The Doppler half of the split cut: the next RDA cut
                    // at the same target elevation.
                    let partner = scan.sweeps().iter().find_map(|s| {
                        let r = s.radials();
                        let f = r.first()?;
                        (f.elevation_number() == eln + 1
                            && (f.elevation_angle_degrees() - angle).abs() < 0.3
                            && r.iter().take(5).any(|x| x.velocity().is_some()))
                        .then_some(r)
                    });
                    match partner {
                        Some(cd) => super::merge_split_cut_doppler(radials, cd),
                        None => radials.to_vec(),
                    }
                };
                candidates.push((eln, angle, merged));
                if candidates.len() == 3 {
                    break;
                }
            }
            if candidates.is_empty() {
                println!("{site}: SKIP — no sweep carries differential phase");
                continue;
            }

            let mut paired = None;
            for (eln, _, radials) in &candidates {
                if let Some(twin) = live::l3_twin(&sources, site, "N0H", l2_start, Some(*eln)).await
                {
                    paired = Some((*eln, radials.as_slice(), twin));
                    break;
                }
            }
            if paired.is_none() {
                // Some sites number the product's cut differently from the
                // RDA cut index our sweeps carry (the KDP survey's finding
                // at the MPDA-style sites KMOB/KSGF/KPAH/KMTX/KSHV). Fall
                // back to the volume-paired twin regardless of cut number,
                // accepted only when its PDB's *elevation angle* names the
                // same tilt as one of our dual-pol sweeps.
                if let Some(t) = live::l3_twin(&sources, site, "N0H", l2_start, None).await {
                    let angle = t.message.pdb.elevation_angle();
                    if let Some((eln, ours, radials)) = candidates
                        .iter()
                        .filter(|(_, a, _)| (a - angle).abs() < 0.3)
                        .min_by(|(_, a, _), (_, b, _)| {
                            (a - angle).abs().total_cmp(&(b - angle).abs())
                        })
                    {
                        println!(
                            "{site}: pairing by angle — twin {} declares eln {} at {angle}°, \
                             matched to our cut {eln} at {ours}°",
                            t.stamp.key, t.message.pdb.elevation_number,
                        );
                        paired = Some((*eln, radials.as_slice(), t));
                    } else {
                        println!(
                            "{site}: SKIP — N0H twin {} declares eln {} at {angle}°, no \
                             dual-pol sweep of ours matches (elns {:?})",
                            t.stamp.key,
                            t.message.pdb.elevation_number,
                            candidates.iter().map(|(e, ..)| *e).collect::<Vec<_>>(),
                        );
                    }
                } else {
                    println!("{site}: SKIP — no N0H twin names volume {l2_start}");
                }
            }
            let Some((eln, radials, twin)) = paired else {
                continue;
            };

            if twin.message.pdb.product_code != 165 {
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

            // The encoding a real product declares, checked, never
            // assumed: N0H's data levels must BE the class codes (scale 1,
            // offset 0) for the tally's level comparison to compare codes.
            let (scale, offset) = match &codec {
                compare::ValueCodec::Scaled { scale, offset } => (*scale, *offset),
                compare::ValueCodec::Lut(_) => {
                    println!("{site}: SKIP — twin selected a LUT codec, not scale/offset");
                    continue;
                }
            };
            println!(
                "{site}: twin {} | codec scale {scale} offset {offset} | pdb scale {} offset {} \
                 | xdr {:?}/{:?} | radials {} × {} gates at {:.3} km",
                twin.stamp.key,
                twin.message.pdb.data_scale(),
                twin.message.pdb.data_offset(),
                packet.xdr_data_scale,
                packet.xdr_data_offset,
                packet.radials.len(),
                packet.num_range_bins,
                compare::gate_km(&twin.message.pdb, packet),
            );
            if scale != 1.0 || offset != 0.0 {
                println!(
                    "{site}: SKIP — twin declares scale {scale}/offset {offset}, not the class \
                     codes' 1/0; levels are not class codes here",
                );
                continue;
            }

            let defined = policy::twin_defined_gates(packet, &codec);
            if !policy::volume_is_scoreable(defined) {
                println!(
                    "{site}: SKIP — near-empty twin ({defined} defined gates < {})",
                    policy::MIN_TWIN_DEFINED_GATES,
                );
                continue;
            }

            // How much precipitation this tilt actually carries, for the
            // site-hour selection table (raw Z ≥ 35 dBZ gate count).
            let hot_gates: usize = radials
                .iter()
                .filter_map(|r| r.reflectivity())
                .flat_map(|m| m.values())
                .filter(|v| matches!(v, nexrad_model::data::MomentValue::Value(x) if *x >= 35.0))
                .count();
            println!("{site}: precip check — {hot_gates} gates ≥ 35 dBZ on the paired tilt");

            // The bounded A/B matrix: documented conventions only, primary
            // first. Nothing outside this list is ever tried.
            let ab: [(&str, &MeltingLayer, Option<&super::ReflCappi>, HcaOptions); 6] = [
                (
                    "radar-mlda/metsig+cappi/rda-isdp/quant",
                    &ml_radar,
                    Some(&cappi),
                    HcaOptions::primary(),
                ),
                (
                    "flat-0c-ml /metsig+cappi/rda-isdp/quant",
                    &ml_flat,
                    Some(&cappi),
                    HcaOptions::primary(),
                ),
                (
                    "radar-mlda/metsig cold-cappi/rda-isdp",
                    &ml_radar,
                    None,
                    HcaOptions::primary(),
                ),
                (
                    "radar-mlda/legacy-rho-flag/rda-isdp",
                    &ml_radar,
                    None,
                    HcaOptions {
                        metsignal: false,
                        ..HcaOptions::primary()
                    },
                ),
                (
                    "radar-mlda/metsig+cappi/isdp-applied",
                    &ml_radar,
                    Some(&cappi),
                    HcaOptions {
                        isdp_estimated: true,
                        ..HcaOptions::primary()
                    },
                ),
                (
                    "radar-mlda/metsig+cappi/rda-isdp/phys",
                    &ml_radar,
                    Some(&cappi),
                    HcaOptions {
                        quantize_transport: false,
                        ..HcaOptions::primary()
                    },
                ),
            ];

            let mut primary_tally = None;
            for (label, ml, ab_cappi, opts) in ab {
                let Some(derived) =
                    super::compute_hca_impl(radials, &params, ml, &hsda, ab_cappi, opts)
                else {
                    continue;
                };
                let grid = derived.to_polar_grid();
                let Some(t) =
                    compare::tally_against_l3(&grid, &twin.message, compare::ProductKind::Class)
                else {
                    continue;
                };
                let tag = if primary_tally.is_none() {
                    "PRIMARY"
                } else {
                    "     ab"
                };
                println!(
                    "{site}: {tag} {label:40} | compared {} exact {:.2}% compatible {:.2}% \
                     presence {:.2}% (derived {} / twin {})",
                    t.compared,
                    t.exact_pct(),
                    policy::compatible_pct(&t),
                    t.presence_disagreement_pct(),
                    t.derived_defined,
                    t.l3_defined,
                );
                if primary_tally.is_none() {
                    println!("{site}: confusion {}", top_confusions(&t, 8));
                    println!("{site}: per-class {}", per_class_accuracy(&t));
                    println!(
                        "{site}: presence {}",
                        presence_diagnostic(
                            radials,
                            &params,
                            &grid,
                            packet,
                            compare::gate_km(&twin.message.pdb, packet),
                        ),
                    );
                    primary_tally = Some(t);
                }
            }
            let Some(tally) = primary_tally else {
                println!("{site}: SKIP — no tally produced");
                continue;
            };
            println!(
                "{site}: vol {l2_start} eln {eln} VCP {} | twin defined {defined} | rda isdp \
                 {:?} vol-est {:?} dbz0 {:?} atmos {:?}",
                twin.message.pdb.vcp,
                params.init_fdp_deg,
                params.isdp_est_deg,
                params.dbz0,
                params.atmos_db_per_km,
            );

            if !policy::site_is_asserted(site) {
                println!("{site}: measured but quarantined — not asserted");
                continue;
            }

            let compatible = policy::compatible_pct(&tally);
            if !policy::meets_class_bar(tally.exact_pct(), compatible) {
                failures.push(format!(
                    "{site} ({l2_start} eln {eln}): exact {:.2}% (bar {}), compatible {:.2}% \
                     (bar {})",
                    tally.exact_pct(),
                    policy::EXACT_PCT,
                    compatible,
                    policy::COMPATIBLE_PCT,
                ));
            }
            asserted_sites += 1;
            pooled_compared += tally.compared;
        }

        println!(
            "asserted {asserted_sites} sites, {pooled_compared} gates pooled; failures: {}",
            failures.len(),
        );
        assert!(
            failures.is_empty(),
            "sites under the class bar:\n  {}",
            failures.join("\n  "),
        );
        assert!(
            policy::sample_is_conclusive(asserted_sites, pooled_compared),
            "inconclusive run: {asserted_sites} sites / {pooled_compared} gates asserted, \
             need ≥{} sites and ≥{} gates — re-run when more sites carry precipitation",
            policy::MIN_SITES,
            policy::MIN_DEFINED_GATES,
        );
    }
}
