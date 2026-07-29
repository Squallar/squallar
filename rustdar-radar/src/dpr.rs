//! Digital Instantaneous Precipitation Rate (the RPG's product 176, AWIPS
//! `DPR`) computed locally: the dual-pol rate forms evaluated down the same
//! hybrid scan [`crate::hhc`] composites, including the specific-attenuation
//! rate R(A).
//!
//! # What is implemented, and from which documents
//!
//! Products 176 and 177 come out of **one task**: `cpc013/tsk009`
//! (`qperate`) fills `Rate_Buf_t::RateComb[360][920]` (inches/hour) and
//! `HHC_Buf_t::HybridHCA[360][920]` from the same `compute_IRRate` outcome
//! at the same bins, and `buildDPR.c` formats the rate half as a packet-28
//! radial component. Everything below is transcribed from the CODE Build
//! 21.0r1.7 public source with the fleet defaults of
//! `cpc104/lib006/dp_precip.alg`; the algorithm is NSSL's dual-pol QPE
//! version 2 (J. Krause), the R(A) subsystem is Ryzhkov et al.'s specific
//! attenuation method as fielded by CCR NA17-00156 (ORPG Build 19).
//!
//! **The ladder is shared, not re-derived**:
//! [`crate::hhc::composite_hybrid_scan`] is `build_RR_Polar_Grid` /
//! `Add_bin_to_RR_Polar_Grid` — lowest cut tries the whole grid, failures go
//! on a hybrid list retried at each higher elevation, SAILS/MRLE
//! supplemental cuts recompute exactly the bins their base cut filled, and
//! the run stops at `Grid_is_full = 99.9%` unless the VCP has supplemental
//! cuts. This module supplies the ladder's *answer*: the rate, where
//! [`crate::hhc`] supplies the class.
//!
//! ## The rate forms (`qperate_comp_Rate*.c`, `Precip_type = CONTINENTAL`)
//!
//! | form | expression | constants |
//! |---|---|---|
//! | R(Z) | `10^((Z − 10·log₁₀ Z_mult)/(10·Z_power))` | `Z_mult` 300, `Z_power` 1.4 |
//! | R(Z, ZDR) | `m·z_lin^a·zdr_lin^b` | 0.0142 / 0.770 / −1.67 |
//! | R(KDP) | `m·|KDP|^p·sgn KDP` | `Kdp_mult` 44, `Kdp_power` 0.822 |
//! | R(KDP) for wet hail | as above with `Kdp_mult_rh` | 27, when ρ < 0.97 and the class is RH |
//! | R(A) | `Γ·A(r)^Δ` | Γ 4120, Δ 1.03, β 0.62 |
//!
//! Z is clamped into [`Refl_min` −32, `Refl_max` 53] dBZ before either Z
//! form (below the floor is noise and sends the bin up the ladder; above the
//! ceiling saturates, 53 dBZ → 103.85 mm/h), and the class picks the form
//! (`qperate_comp_IRRate.c`, with `vprc_switch = OFF` at the fleet default,
//! so every VPR-corrected branch collapses to its uncorrected twin):
//!
//! * **BI, NE** → rate 0.0, filled — biology and no-echo are answers;
//! * **BD, RA** → R(A) when the radial qualifies and Z < 50 dBZ, else R(Z, ZDR);
//! * **GR** → 0.8·R(Z) (`Gr_mult`, `use_Gr_mult = YES`);
//! * **IC** → 2.8·R(Z) (`Ic_mult`), at every height;
//! * **WS** → 0.6·R(Z) (`Ws_mult`);
//! * **HR** → R(A) when it qualifies; else R(KDP) above `Hr_HighZThresh`
//!   45 dBZ; else R(Z, ZDR);
//! * **DS** → 2.8·R(Z) (`Ds_mult`) beyond `beam_edge_top`, else
//!   1.0·R(Z) (`Ds_BelowMLTop_mult`, a no-op at the fleet default);
//! * **RH** → R(KDP) at or before `beam_edge_top`, else 0.8·R(Z) (`Rh_mult`).
//!
//! R(KDP) falls back to R(Z) whenever KDP is undefined, the gate is not
//! meteorological, or the result comes out negative. On a high-attenuation
//! radial a failed rate re-tries R(Z, ZDR) → R(Z) → R(KDP) (`AEL 3.1.2.4`).
//! `Add_bin` then rejects a negative or missing rate (the bin climbs),
//! caps at `Max_precip_rate` 200 mm/h and stores `rate · MM_TO_IN`.
//!
//! ## The specific-attenuation subsystem (`qperate_comp_RateRA.c`)
//!
//! Required here — `RofA_switch = ON` at the fleet default — where
//! [`crate::hhc`] could skip it. Per **radial**, on every tilt:
//!
//! 1. `get_startend_LiquidPrecipBins` finds r1/r2, the first and last gate
//!    before `beam_edge_bottom` that is liquid (class RA/HR/BD), has met
//!    signal **strictly** above 80, a defined raw smoothed Z ≤ 50 dBZ and a
//!    defined `PhiRA`. Either missing disables R(A) on the radial.
//! 2. `calc_PathIntAtten` takes ΔΦ = `PhiRA[r2] − PhiRA[r1]`, then walks
//!    r1+1…r2 subtracting the phase accumulated across runs of non-liquid /
//!    high-Z / weak-signal gates. ΔΦ below `MIN_DELTA_PHI` (0°) disables the
//!    radial. PIA = α·ΔΦ.
//! 3. `refIntigrate` integrates I(r1,r2) = 0.46·β·Σ(z_lin^β)·0.25 over the
//!    qualifying gates; below `MIN_REF_INT_THRESH` 100 the radial is
//!    disabled.
//! 4. The 360 PIAs are median-smoothed **twice** over a circularly extended
//!    array — `Low_rate_sm_lvl` 9 and `High_rate_sm_lvl` 3 — and the gate
//!    picks `PIA_low` below `PIA_ref_thresh` 35 dBZ, `PIA_high` at or above.
//! 5. A(r) = `z_lin^β·C(PIA) / (I(r1,r2) + C(PIA)·I(r,r2))` with
//!    C(PIA) = `exp(0.23·β·PIA) − 1`, and the rate is `Γ·A(r)^Δ`.
//!
//! Note the fields R(A) reads are the *raw* smoothed reflectivity (5-gate
//! mean, **no** attenuation correction) and `PhiRA` (7-gate mean of the
//! unfolded φ, interpolated across the meteorological groups) — two
//! preprocessor outputs distinct from the ones the classification consumes,
//! now produced by [`crate::hca`]'s `radial_fields` for both.
//!
//! # Documented gaps against the RPG
//!
//! * **α is persisted operational state.** `Calculate_Alpha` estimates one α
//!   per volume from the 0.5° cut's median ZDR-versus-Z slope
//!   (α = −0.75·slope + 0.04875, clamped to [0.015, 0.040]; 0.035 flat when
//!   only the stratiform check passes), but the value R(A) actually uses is
//!   a **running mean of the last 12 volumes' α**, held in a `static` that
//!   only updates once all twelve slots carry a valid estimate and otherwise
//!   keeps its last value for three hours before reverting to
//!   `DEFAULT_ALPHA` 0.015. One archived volume can fill exactly one slot,
//!   so the cold start ([`DEFAULT_ALPHA`] everywhere) and the volume's own
//!   estimate (the warm-RPG proxy) are the two readings a single volume
//!   admits; the survey below chose the estimate, and it is what
//!   [`compute_dpr`] runs, falling back to `DEFAULT_ALPHA` when the sample
//!   checks fail exactly as the RPG's un-filled ring does. It remains an
//!   approximation of a quantity the archive does not carry. Note that even
//!   in the RPG α is 0.015 on **every tilt above the lowest**:
//!   `rofa_params` is a local re-initialized per elevation and
//!   `Calculate_Alpha` runs only for `elev_angle_tenths / 10 ≤ 0.5`.
//! * **`vprc_switch` is a per-site adaptable parameter.** `dp_precip.alg`
//!   defaults it OFF, but three independent signatures in the live twins say
//!   the fleet runs it ON with `vprc_version = B` — IC reads exactly
//!   `Ic_mult` (2.8×) high against the twin, WS exactly `Ws_mult` (0.6×)
//!   low, and DS loses its above-the-layer multiplier — and turning the
//!   branching on wins at 7 of 7 surveyed volumes. [`Vprc::OnVersionB`] is
//!   therefore the primary. It reproduces the **branching only**: the
//!   vertically corrected Z that the ON setting also substitutes comes from
//!   a separate upstream task (`VPRC_buf_t`) that the Level II archive does
//!   not carry, so every RA/BD/IC/WS/DS gate above the melting layer's
//!   bottom still rates off an uncorrected Z.
//! * **Beam blockage**: as in [`crate::hhc`], the per-site blockage store is
//!   operational state the archive does not carry, so `blocked_percent` is 0
//!   everywhere. The FShield Z augmentation, the `blocked ≥ Kdp_min_beam_blk`
//!   R(KDP) branch of HR, the blocked R(KDP) branch of RH and the
//!   `blocked ≥ Kdp_max_beam_blk` R(A)-or-nothing branch never fire.
//! * **`beam_edge_top`/`beam_edge_bottom` are never `QPE_NODATA`**: they come
//!   from [`crate::hca`]'s beam/melting-layer intersection, which always
//!   answers, so the `NODATA → send the bin up` branches of DS and RH are
//!   unreachable. A layer at ground gives `beam_edge_bottom = 0`, which
//!   disables R(A) on the radial exactly as a negative value would.
//! * The **exclusion zones** (`Num_zones = 0`) are at their fleet default,
//!   so `is_Excluded` is a no-op and is not transcribed.
//! * The melting layer, environmental heights, CAPPI and met-signal state
//!   carry [`crate::hca`]'s documented gaps into every tilt.
//! * The **Rain Rate Classification** companion (`RRC_Buf_t`, the `HybridRateClass`
//!   the switch also fills) is a separate product and is not built here.
//!
//! # Encoding (`buildDPR.c` / `buildDPR_Packet28.c`)
//!
//! `RateData = nint(RateComb · 1000)` — thousandths of an inch per hour — in
//! a packet 28 radial component of 360 radials (leading-edge azimuth, 1°
//! wide) × 920 bins, first range 125 m, bin size 250 m. **DPR has no
//! no-data flag**: `QPE_NODATA` is written as level 0, the same level a
//! genuine 0.0 rate takes, and a nonzero rate that would round to 0 is
//! lifted to level 1. Levels 0 and 1 both decode as undefined, so
//! [`DerivedDpr::values`] emits `NaN` wherever `nint(rate · 1000) ≤ 1` —
//! the product's own presence rule, and what makes a twin comparison
//! presence-aligned.
//!
//! Two geometry facts the live twins settled, both verified before any
//! number here was trusted:
//!
//! * the packet's XDR attribute string reads **scale 1000 / offset 0** (the
//!   harness asserts it), 360 radials × 920 gates at 0.25 km;
//! * `first_range` is 125 m — the **centre** of bin 0, half a bin — which
//!   `nexrad_level3`'s packet-28 decoder was rounding to the bin *index* 1
//!   and so placing every twin gate a quarter-gate too far out. Fixed
//!   there, worth +11 points of agreement, and the harness's
//!   `DPR_DEBUG_ALIGN` sub-gate sweep now peaks at zero shift.
//!
//! # Validation status — **surveyed, and the product did not convert**
//!
//! Surveyed 2026-07-29 against live `DPR` twins on ten precipitating
//! site-hours across six distinct sites and five climate regions, paired by
//! volume start with the volume's *latest* DPR object, scored on **wet
//! gates** (either side above 0.01 in/hr) under the campaign's two bars:
//! ≥ 95% within max(±0.05 in/hr, ±10%) on gates where the derived hybrid
//! class equals the RPG's own `HHC` twin, and ≥ 99% within max(±0.05 in/hr,
//! ±25%) unrestricted.
//!
//! | site-hour | region | wet gates | primary % | secondary % | bias in/hr | mae |
//! |---|---|---|---|---|---|---|
//! | KDDC 07-28 09:05 | Kansas high plains | 10 746 | **96.20** | 96.27 | −0.004 | 0.013 |
//! | KOAX 07-28 09:04 | Nebraska plains | 13 834 | 92.54 | 92.61 | −0.004 | 0.022 |
//! | KUEX 07-28 09:05 | Nebraska plains | 18 884 | 91.19 | 90.76 | −0.006 | 0.024 |
//! | KUEX 07-29 13:08 | Nebraska plains | 27 059 | 90.24 | 91.29 | −0.004 | 0.023 |
//! | KUEX 07-28 01:07 | Nebraska plains | 10 757 | 88.55 | 88.48 | +0.013 | 0.035 |
//! | KOAX 07-29 13:06 | Nebraska plains | 10 665 | 79.11 | 83.22 | −0.019 | 0.058 |
//! | KTLH 07-28 01:07 | Florida Gulf coast | 11 459 | 76.24 | 85.96 | +0.024 | 0.084 |
//! | KUEX 07-29 10:21 | Nebraska plains | 25 748 | 70.79 | 81.47 | +0.014 | 0.119 |
//! | KMLB 07-28 19:24 | Florida subtropical | 12 287 | 67.92 | 73.65 | +0.047 | 0.158 |
//! | KEAX 07-28 09:05 | Missouri | 15 178 | 58.71 | 60.40 | +0.047 | 0.111 |
//!
//! Figures move by a few tenths of a point between runs: the melting layer
//! comes from a live sounding fetch, and a refreshed sounding moves a
//! handful of gates across the class multipliers.
//!
//! **One site-hour of ten clears the primary bar and none clears the
//! secondary**, so the product stays on the Level III fetch: no conversion.
//! The rest of the roster (KMPX, KFSD, KBIS, KABR, KTLX, KMRX, KMOB, KSGF,
//! KPAH, KMTX, KSFX, KMVX, KLZK, KSHV, KAMA, KFWS) was measured at the same
//! hour and skipped by the wet-gate floor — 0 to 3 500 wet gates each — a
//! printed skip, never a silent pass.
//!
//! **What is and is not reproduced.** Split by rate form (per-class
//! agreement at KUEX 07-29 13:08, the fullest volume), the pure R(Z)
//! multiples land: DS 95%, GR 99%, IC 100%, BD 94%, RA 90% — the Z field,
//! the tilt the hybrid ladder picks, the resampling and R(Z) itself are
//! right. The forms that read the *derived polarimetric* fields do not:
//! HR 55%, RH 91%, and WS 20% before the `vprc` branch was adopted. The
//! `DPR_DEBUG_ALIGN` split says the residual is roughly half per-radial
//! (p10–p90 ratio 0.86–1.16, a PIA/α spread) and half within-radial
//! (0.72–1.23, a field spread).
//!
//! # Why this stops here (the campaign's early-stop rule)
//!
//! The residual carries an **operational-state fingerprint**, the same one
//! [`crate::eet`], [`crate::vil`] and [`crate::kdp`] record:
//!
//! 1. **α is cross-volume state.** The RPG averages twelve volumes' α; one
//!    archived volume yields one sample. Adopting that sample is worth up to
//!    +28 points and is unanimous — but it is a proxy, and the sign of the
//!    residual flips with it (we read 31% high at KEAX, 26% low at KUEX
//!    07-29 10:36 before it was adopted), which is exactly what a
//!    mis-estimated α does. There is no way to do better from one volume.
//! 2. **`vprc_switch` is per-site adaptable and its correction is upstream
//!    state.** The branching can be inferred (and is); the corrected Z
//!    cannot be reproduced at all.
//! 3. **Beam blockage** is a per-site store the archive does not carry.
//! 4. **The melting layer** is model-enhanced in the RPG. Where the class
//!    multiplier switches across `beam_edge_top` the DS rate reads 1.70× at
//!    KEAX — the same `crate::hca` gap that quarantines KMTX there.
//!
//! Underneath all four, a rain-rate product amplifies small field
//! differences the way its own relations are shaped: R(Z, ZDR) moves 7.7%
//! per 0.2 dB of ZDR, R(KDP) 16% per 20% of KDP, and R(A) ~15% per dB of
//! raw Z. [`crate::hca`]'s and [`crate::kdp`]'s documented field gaps are
//! small enough to classify at 97.8% and still too large to rate at 95%
//! within 10%. **No bar was lowered and no undocumented constant was
//! fitted.**

use crate::dpprep::{
    ART_CORR, CORR_THRESH, MET_SIG_THRESHOLD, ReflCappi, median_filter, resample_to_polar_grid,
};
use crate::hca::{
    BD, BI, DS, GC, GR, HR, HcaOptions, HsdaHeights, IC, MeltingLayer, NE, NO_DATA, RA, RH, UK, WS,
};
use crate::hhc::{HHC_AZ, HHC_BINS, Tilt, TiltRow, composite_hybrid_scan, prepare_tilt};
use crate::kdp::KdpParams;
use nexrad_model::data::Radial;

// ── dp_precip.alg fleet defaults ────────────────────────────────────────────

/// `Z_mult` / `Z_power`: the R(Z) relation, `Z = 300·R^1.4`.
pub(crate) const Z_MULT: f64 = 300.0;
pub(crate) const Z_POWER: f64 = 1.4;
/// `Refl_min` / `Refl_max`: the reflectivity window both Z forms honour.
pub(crate) const REFL_MIN: f64 = -32.0;
pub(crate) const REFL_MAX: f64 = 53.0;
/// `Zdr_z_mult_cont` / `Zdr_z_power_cont` / `Zdr_zdr_power_cont`: the
/// continental R(Z, ZDR) (`Precip_type = CONTINENTAL`). The tropical triple
/// is 0.0067 / 0.927 / −3.43 and is not the fleet default.
pub(crate) const ZDR_Z_MULT: f64 = 0.0142;
pub(crate) const ZDR_Z_POWER: f64 = 0.770;
pub(crate) const ZDR_ZDR_POWER: f64 = -1.67;
/// `Kdp_mult` / `Kdp_power`, and `Kdp_mult_rh` for wet hail below ρ 0.97.
pub(crate) const KDP_MULT: f64 = 44.0;
pub(crate) const KDP_MULT_RH: f64 = 27.0;
pub(crate) const KDP_POWER: f64 = 0.822;
/// The ρ below which an RH gate takes `Kdp_mult_rh` (CCR NA17-00192).
pub(crate) const KDP_RH_RHO: f64 = 0.97;
/// `Kdp_min_usage_rate`: the legacy (met-signal OFF) minimum R(Z) for
/// R(KDP) to be considered reliable.
pub(crate) const KDP_MIN_USAGE_RATE: f64 = 10.0;
/// The class multipliers on R(Z).
pub(crate) const GR_MULT: f64 = 0.8;
pub(crate) const WS_MULT: f64 = 0.6;
pub(crate) const IC_MULT: f64 = 2.8;
pub(crate) const DS_MULT: f64 = 2.8;
pub(crate) const DS_BELOW_ML_TOP_MULT: f64 = 1.0;
pub(crate) const RH_MULT: f64 = 0.8;
/// `Hr_HighZThresh`: above this, HR rates by R(KDP) instead of R(Z, ZDR).
pub(crate) const HR_HIGH_Z_THRESH: f64 = 45.0;
/// `Max_precip_rate`, mm/h — `Add_bin` caps every stored rate here.
pub(crate) const MAX_PRECIP_RATE: f64 = 200.0;
/// `PIA_ref_thresh`, dBZ: which of the two PIA smoothings a gate reads.
pub(crate) const PIA_REF_THRESH: f64 = 35.0;
/// `Low_rate_sm_lvl` / `High_rate_sm_lvl`: the two PIA median windows.
pub(crate) const PIA_SMOOTH_LOW: usize = 9;
pub(crate) const PIA_SMOOTH_HIGH: usize = 3;

// ── qperate_RA.h literals ───────────────────────────────────────────────────

pub(crate) const BETA: f64 = 0.62;
pub(crate) const GAMMA: f64 = 4120.0;
pub(crate) const BIGDELTA: f64 = 1.03;
/// `MIN_CORREL_COEF`: the ρ floor for a Z/ZDR pair to feed α.
pub(crate) const MIN_CORREL_COEF: f64 = 0.98;
/// `MAX_ROFA_REF_THRESH`: the Z ceiling for R(A), dBZ.
pub(crate) const MAX_ROFA_REF_THRESH: f64 = 50.0;
/// `MIN_DELTA_PHI`: the ΔΦ floor across the radial, degrees.
pub(crate) const MIN_DELTA_PHI: f64 = 0.0;
/// `MIN_REF_INT_THRESH`: below this I(r1,r2) the radial gives up on R(A).
pub(crate) const MIN_REF_INT_THRESH: f64 = 100.0;
/// `DEFAULT_ALPHA`: the cold-start running-average α, and the value every
/// tilt above 0.5° uses regardless.
pub(crate) const DEFAULT_ALPHA: f64 = 0.015;
/// `STRATIFORM_ALPHA`, and the bounds `Calculate_Alpha` clamps into.
pub(crate) const STRATIFORM_ALPHA: f64 = 0.035;
pub(crate) const MIN_ALLOWABLE_ALPHA: f64 = 0.015;
pub(crate) const MAX_ALLOWABLE_ALPHA: f64 = 0.040;
/// `NUM_GROUPS`: 2-dBZ reflectivity groups centred 11, 13 … 49 dBZ.
pub(crate) const NUM_GROUPS: usize = 20;
/// `MIN_NUM_PAIRS_20TO40` / `MIN_NUM_PAIRS_10TO30`: per-group sample floors.
pub(crate) const MIN_NUM_PAIRS_20TO40: usize = 200;
pub(crate) const MIN_NUM_PAIRS_10TO30: usize = 400;
/// The elevation at or below which `Calculate_Alpha` runs, degrees —
/// `build_RR_Polar_Grid`'s `elev_angle_deg <= 0.5`, where `elev_angle_deg`
/// is the VCP's **target** angle (`elev_angle_tenths / 10.0`).
pub(crate) const ALPHA_MAX_ELEV_DEG: f64 = 0.5;
/// How far a sweep's reported angle may sit from the target and still be
/// the same cut. The archive carries the *measured* elevation of a sweep's
/// first radial, which trails the target while the antenna settles — KUEX's
/// 0.5° cut reads 0.66° — so `target ≤ 0.5` cannot be tested directly. The
/// same set of cuts is "the volume's lowest base angle, when that angle is
/// itself near 0.5°": every operational VCP puts its second cut at 0.9° or
/// above, four times this tolerance away.
pub(crate) const ALPHA_ELEV_TOL_DEG: f64 = 0.15;

/// Whether a tilt at `elev_deg` is one `Calculate_Alpha` runs on, given the
/// volume's lowest base elevation. Both angles are sweep **medians** — see
/// [`sweep_elevation_deg`].
pub(crate) fn is_alpha_cut(elev_deg: f64, lowest_base_deg: f64) -> bool {
    lowest_base_deg <= ALPHA_MAX_ELEV_DEG + ALPHA_ELEV_TOL_DEG
        && elev_deg <= lowest_base_deg + ALPHA_ELEV_TOL_DEG
}

/// A sweep's elevation, robustly: the median of its radials' reported
/// angles. The *first* radial's angle — what [`crate::hhc::volume_tilts`]
/// reports, and all the hybrid ladder needs — is still slewing (KUEX's 0.5°
/// cut opens at 0.66° and its 0.9° cut at 0.75°, only 0.09° apart), which
/// cannot separate the two cuts the way the VCP target angle the RPG reads
/// does. The median is within a few hundredths of the target.
pub(crate) fn sweep_elevation_deg(radials: &[Radial]) -> f64 {
    let mut angles: Vec<f64> = radials
        .iter()
        .map(|r| f64::from(r.elevation_angle_degrees()))
        .collect();
    if angles.is_empty() {
        return f64::MAX;
    }
    angles.sort_by(f64::total_cmp);
    angles[angles.len() / 2]
}

/// `dp_Consts.h`'s `MM_TO_IN` — the exact literal, so a hand-computed rate
/// reproduces the stored value bit for bit.
pub(crate) const MM_TO_IN: f64 = 0.03937008;

/// `buildDPR.c`'s `INC_SCAL`: thousandths of an inch per hour.
pub const DPR_SCALE: f64 = 1000.0;

/// Where α comes from — the one documented ambiguity a single archived
/// volume forces (see the module doc).
// The variant `primary()` does not pick exists for the bounded A/B in the
// live harness (and the offline tests that pin its behaviour), which is
// `cfg(test)` — so the library build sees it constructed nowhere.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RofaAlpha {
    /// `DEFAULT_ALPHA` on every tilt: the RPG's cold start, and its steady
    /// state whenever the twelve-volume ring is not full. The primary.
    Default,
    /// The volume's own `Calculate_Alpha` estimate on the ≤ 0.5° cuts (and
    /// `DEFAULT_ALPHA` above, as the RPG does): the warm-RPG proxy.
    Volume,
}

/// `vprc_switch` / `vprc_version`, the VPR-correction adaptable pair.
///
/// `OFF` is the `dp_precip.alg` fleet default and the primary. `ON` moves
/// four classes onto different branches of `compute_IRRate`'s switch — IC
/// and WS lose their multipliers, DS loses its above-the-melting-layer
/// multiplier, and RA/BD beyond `beam_edge_bottom` rate by R(Z) instead of
/// R(Z, ZDR) — and *also* substitutes a vertically corrected Z produced by
/// a separate upstream task (`VPRC_buf_t`) that the Level II archive does
/// not carry. [`Vprc::OnVersionB`] therefore reproduces the **branching
/// only**, exactly as the released code behaves when `vprc_inbuf == NULL`
/// (`Zvpr = bin_moments->Z`, no correction): enough to test which branch a
/// site runs, never enough to reproduce the site running it.
#[allow(dead_code)] // as `RofaAlpha`: the A/B alternative is test-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vprc {
    /// `vprc_switch = OFF`: the fixed HCA multipliers. The fleet default.
    Off,
    /// `vprc_switch = ON`, `vprc_version = B` — the branching only.
    OnVersionB,
}

/// The conventions [`compute_dpr`] pins; the harness varies them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DprOptions {
    pub(crate) hca: HcaOptions,
    /// `RofA_switch`, ON at the fleet default.
    pub(crate) rofa: bool,
    pub(crate) alpha: RofaAlpha,
    pub(crate) vprc: Vprc,
}

impl DprOptions {
    pub(crate) const fn primary() -> Self {
        Self {
            hca: HcaOptions::primary(),
            rofa: true,
            alpha: RofaAlpha::Volume,
            vprc: Vprc::OnVersionB,
        }
    }
}

/// The derived rate field, at the product's own geometry.
pub struct DerivedDpr {
    /// `[azimuth 0..360][bin 0..920]`, inches per hour; `NaN` where the
    /// ladder never filled **or** where the product's own encoding would
    /// land on level 0/1 (see the module doc's encoding note).
    pub values: Vec<Vec<f32>>,
    /// `[azimuth][bin]` of the hydrometeor class that produced each rate,
    /// in the product-177 external codes; `NaN` where unfilled or where the
    /// class encodes level 0 (NE/U0). The RPG fills this grid from the same
    /// outcome, which is what makes a class-matched rate comparison
    /// meaningful.
    pub classes: Vec<Vec<f32>>,
    /// Range to the centre of bin 0, km, and the bin size.
    pub first_gate_km: f64,
    pub gate_interval_km: f64,
    /// `pct_hybrate_filled`: percent of the grid that computed a rate.
    pub pct_filled: f64,
    /// `highest_elang`: the highest base elevation processed, degrees.
    pub highest_elev_deg: f64,
}

impl DerivedDpr {
    /// Resample the rate onto the 360° × 230 km comparison grid, cell for
    /// cell the way the twin comparator resamples the Level III product.
    pub fn to_polar_grid(&self) -> Vec<Vec<f32>> {
        self.resample(&self.values)
    }

    /// The same resampling of the companion class grid.
    pub fn classes_to_polar_grid(&self) -> Vec<Vec<f32>> {
        self.resample(&self.classes)
    }

    fn resample(&self, field: &[Vec<f32>]) -> Vec<Vec<f32>> {
        let azimuths: Vec<f64> = (0..HHC_AZ).map(|az| az as f64 + 0.5).collect();
        resample_to_polar_grid(
            field,
            &azimuths,
            self.first_gate_km,
            self.gate_interval_km,
            1.0,
        )
    }
}

// ── The rate forms ──────────────────────────────────────────────────────────

/// `compute_RateZ`. `None` is `QPE_NODATA` — send the bin up the ladder.
/// The two branches of the released source (`ptype == CV || Z > 40` and
/// otherwise) carry the *same* expression, so there is one here.
pub(crate) fn rate_z(z: f64) -> Option<f64> {
    if z == NO_DATA || z < REFL_MIN {
        // Too low to be anything but noise: try a higher elevation.
        return None;
    }
    let z = if z > REFL_MAX { REFL_MAX } else { z };
    let r = 10f64.powf((z - 10.0 * Z_MULT.log10()) / (10.0 * Z_POWER));
    (r >= 0.0).then_some(r)
}

/// `compute_RateZ_Zdr`, continental coefficients. ZDR enters linearly
/// (`10^(ZDR/10)`), Z enters as linear power clamped at `Refl_max`.
pub(crate) fn rate_z_zdr(z: f64, zdr: f64) -> Option<f64> {
    if z == NO_DATA || zdr == NO_DATA || z < REFL_MIN {
        return None;
    }
    let z_lin = 10f64.powf(0.1 * if z > REFL_MAX { REFL_MAX } else { z });
    let zdr_lin = 10f64.powf(0.1 * zdr);
    let r = ZDR_Z_MULT * z_lin.powf(ZDR_Z_POWER) * zdr_lin.powf(ZDR_ZDR_POWER);
    (r >= 0.0).then_some(r)
}

/// `compute_RateKdp`. Every rejection returns `rate_z` — which may itself be
/// `None` — rather than failing outright, exactly as the C returns the
/// caller's `Rate_Z`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rate_kdp(
    kdp: f64,
    rho: f64,
    class: usize,
    rate_z: Option<f64>,
    hatt: bool,
    met: f64,
    metsignal_on: bool,
) -> Option<f64> {
    if kdp == NO_DATA {
        return rate_z;
    }
    if !hatt {
        if metsignal_on {
            if met.is_nan() || met < MET_SIG_THRESHOLD {
                return rate_z;
            }
        } else {
            if rho < CORR_THRESH {
                return rate_z;
            }
            // A `QPE_NODATA` Rate_Z is far below the floor, so it too
            // falls through to the R(Z) answer.
            if rate_z.is_none_or(|r| r < KDP_MIN_USAGE_RATE) {
                return rate_z;
            }
        }
    }
    let mult = if rho < KDP_RH_RHO && class == RH {
        KDP_MULT_RH
    } else {
        KDP_MULT
    };
    let sign = if kdp < 0.0 { -1.0 } else { 1.0 };
    let r = mult * kdp.abs().powf(KDP_POWER) * sign;
    if r < 0.0 { rate_z } else { Some(r) }
}

// ── The specific-attenuation subsystem ──────────────────────────────────────

/// One radial's R(A) state (`Specific_atten_t`, per azimuth).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RofaRadial {
    /// `RofAFlag`: whether R(A) may be used on this radial at all.
    pub(crate) usable: bool,
    /// `endbin`: r2, the last liquid gate. (`startbin`, r1, is consumed
    /// while the radial state is built and never read again at bin time.)
    pub(crate) end: usize,
    /// `rad_ref_integral`: I(r1, r2).
    pub(crate) ref_integral: f64,
    /// `PIA` before smoothing, `NaN` when the radial never computed one.
    pub(crate) pia: f64,
    /// `PIA_low` / `PIA_high` after the two median smoothings.
    pub(crate) pia_low: f64,
    pub(crate) pia_high: f64,
}

impl Default for RofaRadial {
    fn default() -> Self {
        Self {
            usable: false,
            end: 0,
            ref_integral: 0.0,
            pia: f64::NAN,
            pia_low: f64::NAN,
            pia_high: f64::NAN,
        }
    }
}

/// Whether a gate qualifies as liquid precipitation for the R(A) machinery:
/// met signal **strictly** above the threshold (note the rate gate itself is
/// `≥`), an HCA liquid class, and a defined raw smoothed Z at or below
/// `MAX_ROFA_REF_THRESH`.
fn ra_liquid(row: &TiltRow, rng: usize) -> bool {
    let Some(&class) = row.class.get(rng) else {
        return false;
    };
    let met = row.met.get(rng).copied().unwrap_or(f64::NAN);
    let z = row.raw_smz.get(rng).copied().unwrap_or(NO_DATA);
    met > MET_SIG_THRESHOLD
        && matches!(class, RA | HR | BD)
        && z != NO_DATA
        && z <= MAX_ROFA_REF_THRESH
}

/// `get_startend_LiquidPrecipBins`: r1 scans `[0, beam_edge_bottom)` upward,
/// r2 scans `(r1, beam_edge_bottom]` downward — the asymmetric bounds are
/// the source's.
fn ra_start_end(row: &TiltRow) -> Option<(usize, usize)> {
    if row.bin_bb < 0 {
        return None;
    }
    let bb = row.bin_bb as usize;
    let qualifies = |rng: usize| {
        ra_liquid(row, rng) && row.phi_ra.get(rng).copied().unwrap_or(NO_DATA) != NO_DATA
    };
    let r1 = (0..bb).find(|&rng| qualifies(rng))?;
    let r2 = (r1 + 1..=bb).rev().find(|&rng| qualifies(rng))?;
    Some((r1, r2))
}

/// `calc_PathIntAtten`: ΔΦ over [r1, r2] with the phase accumulated across
/// runs of unusable gates removed, times α. `None` disables the radial.
fn ra_pia(row: &TiltRow, r1: usize, r2: usize, alpha: f64) -> Option<f64> {
    let phi = |i: usize| row.phi_ra.get(i).copied().unwrap_or(NO_DATA);
    let mut delta_phi = phi(r2) - phi(r1);
    if delta_phi < MIN_DELTA_PHI {
        return None;
    }
    let mut bad = false;
    let mut phi1 = 0.0;
    for rng in r1 + 1..=r2 {
        let z = row.raw_smz.get(rng).copied().unwrap_or(NO_DATA);
        if z == NO_DATA || phi(rng) == NO_DATA {
            continue; // no data: nothing to subtract
        }
        if !ra_liquid(row, rng) {
            if !bad {
                phi1 = phi(rng - 1);
            }
            bad = true;
        } else {
            if bad {
                delta_phi -= phi(rng) - phi1;
            }
            bad = false;
        }
    }
    if delta_phi < MIN_DELTA_PHI {
        return None;
    }
    Some(alpha * delta_phi)
}

/// `refIntigrate`: I(start, end) = 0.46·β·Σ z_lin^β·0.25 over the
/// qualifying gates, per kilometre at 0.25 km gates.
fn ra_ref_integral(row: &TiltRow, start: usize, end: usize) -> f64 {
    let mut sum = 0.0;
    for rng in start..=end {
        if ra_liquid(row, rng) {
            let z = row.raw_smz[rng];
            sum += 10f64.powf(0.1 * z).powf(BETA);
        }
    }
    0.46 * BETA * sum * 0.25
}

/// `smooth_PIA`: median-filter the 360 PIAs over a circularly extended
/// array so no radial sees a truncated window. Radials whose window holds
/// no PIA at all keep `NaN`, which only reaches a gate whose own radial has
/// no PIA — and such a radial is not `usable`.
pub(crate) fn smooth_pia(pia: &[f64], window: usize) -> Vec<f64> {
    let n = pia.len();
    if n == 0 || window == 0 {
        return pia.to_vec();
    }
    let w = window.min(n);
    let mut extended = Vec::with_capacity(n + 2 * w);
    extended.extend_from_slice(&pia[n - w..]);
    extended.extend_from_slice(pia);
    extended.extend_from_slice(&pia[..w]);
    let smoothed = median_filter(&extended, window);
    (0..n).map(|az| smoothed[w + az]).collect()
}

/// `compute_RateRA`: A(r) from the bin's own forward integral and the
/// radial's, then `Γ·A^Δ`. mm/h.
pub(crate) fn rate_ra(row: &TiltRow, ra: &RofaRadial, rng: usize) -> f64 {
    let bin_integral = ra_ref_integral(row, rng, ra.end);
    let z = row.raw_smz.get(rng).copied().unwrap_or(NO_DATA);
    // A missing Z underflows to zero linear power, exactly as the C's
    // `DPP_NO_DATA` does, and the gate rates 0.
    let z_lin = 10f64.powf(0.1 * z);
    let pia = if z < PIA_REF_THRESH {
        ra.pia_low
    } else {
        ra.pia_high
    };
    let c_of_pia = (0.23 * BETA * pia).exp() - 1.0;
    let atten = (z_lin.powf(BETA) * c_of_pia) / (ra.ref_integral + c_of_pia * bin_integral);
    GAMMA * atten.powf(BIGDELTA)
}

/// `Calculate_Alpha` over one tilt: the median-ZDR-versus-Z slope of the
/// liquid gates below the melting layer, turned into α. `None` is
/// `NO_ALPHA` — neither the convective nor the stratiform sample check
/// passed, and the RPG's ring keeps its previous contents.
///
/// The `blocked_percent < 20` filter is always true here (no blockage
/// store), and `quick_select`'s **lower** median is used, as the source's is.
pub(crate) fn calculate_alpha(tilt: &Tilt) -> Option<f64> {
    // smz_array[z] = 11 + 2z: group centres 11, 13 … 49 dBZ.
    let centre = |i: usize| 11.0 + 2.0 * i as f64;
    let mut groups: Vec<Vec<f64>> = vec![Vec::new(); NUM_GROUPS];
    for row in tilt.iter().flatten() {
        let bb = row.bin_bb.max(0) as usize;
        for rng in 81..bb.min(row.class.len()) {
            let z = row.smz[rng];
            let zdr = row.zdr[rng];
            let rho = row.rho[rng];
            let met = row.met.get(rng).copied().unwrap_or(f64::NAN);
            if zdr == NO_DATA
                || z == NO_DATA
                || rho <= MIN_CORREL_COEF
                || !matches!(row.class[rng], BD | RA | HR)
                || met.is_nan()
                || met <= MET_SIG_THRESHOLD
            {
                continue;
            }
            for (i, bucket) in groups.iter_mut().enumerate() {
                if z >= centre(i) - 1.0 && z < centre(i) + 1.0 {
                    bucket.push(zdr);
                    break;
                }
            }
        }
    }

    // The three windows the source indexes by their own group edges.
    const LOW_STRAT: usize = 0; // centre 11 → 10–12
    const UP_STRAT: usize = 9; // centre 29 → 28–30
    const LOW_CHCK: usize = 5; // centre 21 → 20–22
    const UP_CHCK: usize = 14; // centre 39 → 38–40
    const UP_CONV: usize = 19; // centre 49 → 48–50

    let num_pass_conv = (LOW_CHCK..=UP_CONV)
        .filter(|&i| groups[i].len() >= MIN_NUM_PAIRS_20TO40)
        .count();
    let num_pass_strat = (LOW_STRAT..=UP_STRAT)
        .filter(|&i| groups[i].len() >= MIN_NUM_PAIRS_10TO30)
        .count();

    if num_pass_conv >= 12 {
        let mut xs: Vec<f64> = Vec::new();
        let mut ys: Vec<f64> = Vec::new();
        for (i, bucket) in groups
            .iter_mut()
            .enumerate()
            .take(UP_CHCK + 1)
            .skip(LOW_CHCK)
        {
            if bucket.len() >= MIN_NUM_PAIRS_20TO40 {
                xs.push(centre(i));
                ys.push(lower_median(bucket));
            }
        }
        let slope = least_squares_slope(&xs, &ys)?;
        Some((-0.75 * slope + 0.04875).clamp(MIN_ALLOWABLE_ALPHA, MAX_ALLOWABLE_ALPHA))
    } else if num_pass_strat >= 9 {
        Some(STRATIFORM_ALPHA)
    } else {
        None
    }
}

/// `quick_select`'s median: index `(n − 1) / 2` of the sorted values — the
/// **lower** median for an even count, unlike the preprocessor's filters.
pub(crate) fn lower_median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[(values.len() - 1) / 2]
}

/// `linear_leastsquares`, returning the slope alone.
pub(crate) fn least_squares_slope(x: &[f64], y: &[f64]) -> Option<f64> {
    let n = x.len();
    if n < 2 || n != y.len() {
        return None;
    }
    let sx: f64 = x.iter().sum();
    let sxoss = sx / n as f64;
    let mut st2 = 0.0;
    let mut b = 0.0;
    for i in 0..n {
        let t = x[i] - sxoss;
        st2 += t * t;
        b += t * y[i];
    }
    (st2 != 0.0).then(|| b / st2)
}

// ── Per-tilt preparation and the per-bin rate ───────────────────────────────

/// One prepared tilt: the classification rows plus the R(A) radial state.
pub(crate) struct DprTilt {
    rows: Tilt,
    ra: Vec<RofaRadial>,
}

/// `build_RR_Polar_Grid`'s per-elevation R(A) preamble: α (lowest cut only),
/// the per-radial r1/r2, PIA and reflectivity integral, then the two PIA
/// smoothings. `alpha_cut` says whether this tilt is one `Calculate_Alpha`
/// runs on — see [`is_alpha_cut`].
fn prepare_rofa(
    rows: &Tilt,
    alpha_cut: bool,
    opts: DprOptions,
) -> (Vec<RofaRadial>, f64, Option<f64>) {
    // `rofa_params.alpha` is re-initialized to DEFAULT_ALPHA on every
    // elevation; only a ≤ 0.5° cut overwrites it, and then with the
    // *running average* — DEFAULT_ALPHA cold, this volume's own estimate
    // under the A/B alternative.
    let mut alpha = DEFAULT_ALPHA;
    let mut estimate = None;
    if alpha_cut {
        estimate = calculate_alpha(rows);
        if opts.alpha == RofaAlpha::Volume
            && let Some(a) = estimate
        {
            alpha = a;
        }
    }

    let mut ra: Vec<RofaRadial> = vec![RofaRadial::default(); HHC_AZ];
    // `qperate_RofA_buffer_control` bails out with the radial disabled when
    // the switch is off or the met signal is not available.
    if !opts.rofa || !opts.hca.metsignal {
        return (ra, alpha, estimate);
    }
    for (az, slot) in ra.iter_mut().enumerate() {
        let Some(row) = rows[az].as_ref() else {
            continue;
        };
        let Some((r1, r2)) = ra_start_end(row) else {
            continue;
        };
        let Some(pia) = ra_pia(row, r1, r2, alpha) else {
            continue;
        };
        let integral = ra_ref_integral(row, r1, r2);
        if integral < MIN_REF_INT_THRESH {
            continue;
        }
        *slot = RofaRadial {
            usable: true,
            end: r2,
            ref_integral: integral,
            pia,
            pia_low: f64::NAN,
            pia_high: f64::NAN,
        };
    }

    let pias: Vec<f64> = ra.iter().map(|r| r.pia).collect();
    let low = smooth_pia(&pias, PIA_SMOOTH_LOW);
    let high = smooth_pia(&pias, PIA_SMOOTH_HIGH);
    for (az, slot) in ra.iter_mut().enumerate() {
        if low[az].is_finite() {
            slot.pia_low = low[az];
        }
        if high[az].is_finite() {
            slot.pia_high = high[az];
        }
    }
    (ra, alpha, estimate)
}

/// `compute_IRRate` with `blocked_percent = 0` and `vprc_switch = OFF`.
/// Returns mm/h; `None` is `QPE_NODATA` — try a higher elevation.
pub(crate) fn compute_ir_rate(
    row: &TiltRow,
    ra: &RofaRadial,
    rng: usize,
    metsignal_on: bool,
    vprc: Vprc,
) -> Option<f64> {
    let class = *row.class.get(rng)?;

    // Biota and no echo are answers, not gaps (AEL 3.1.2.1) — checked
    // before ρ / met signal, because the HCA already weighed them.
    if class == BI || class == NE {
        return Some(0.0);
    }

    let met = row.met.get(rng).copied().unwrap_or(f64::NAN);
    let rho = row.rho.get(rng).copied().unwrap_or(NO_DATA);
    let hatt = row.hatt;
    if metsignal_on {
        if met.is_nan() || met < MET_SIG_THRESHOLD {
            return None;
        }
    } else if !hatt && rho < ART_CORR {
        // The legacy gate: attenuated radials skip the ρ check entirely.
        return None;
    }

    let z = row.smz.get(rng).copied().unwrap_or(NO_DATA);
    let zdr = row.zdr.get(rng).copied().unwrap_or(NO_DATA);
    let kdp = row.kdp.get(rng).copied().unwrap_or(NO_DATA);
    let base_z = rate_z(z);
    let kdp_rate = || rate_kdp(kdp, rho, class, base_z, hatt, met, metsignal_on);
    // The R(A) precondition reads the *processed* Z, so a gate with no Z
    // still qualifies — the source's test is `Z < MAX_ROFA_REF_THRESH`.
    let ra_ok = ra.usable && rng <= ra.end && z < MAX_ROFA_REF_THRESH;

    let vprc_on = vprc == Vprc::OnVersionB;
    let mut rate = match class {
        BD | RA => {
            if ra_ok {
                Some(rate_ra(row, ra, rng))
            } else if vprc_on && rng as i64 >= row.bin_bb {
                // Beyond the melting layer's bottom the VPR branch rates by
                // R(Z) of the corrected Z — the plain Z with no VPR buffer.
                base_z
            } else {
                rate_z_zdr(z, zdr)
            }
        }
        // `vprZ_in_GR = NO` with `use_Gr_mult = YES` lands on the same
        // expression either way.
        GR => base_z.map(|r| r * GR_MULT),
        IC => base_z.map(|r| if vprc_on { r } else { r * IC_MULT }),
        WS => base_z.map(|r| if vprc_on { r } else { r * WS_MULT }),
        HR => {
            if ra_ok {
                Some(rate_ra(row, ra, rng))
            } else if z > HR_HIGH_Z_THRESH {
                kdp_rate()
            } else {
                rate_z_zdr(z, zdr)
            }
        }
        DS => {
            if vprc_on {
                base_z
            } else if rng as i64 >= row.bin_tt {
                base_z.map(|r| r * DS_MULT)
            } else {
                base_z.map(|r| r * DS_BELOW_ML_TOP_MULT)
            }
        }
        RH => {
            if (rng as i64) <= row.bin_tt {
                kdp_rate()
            } else {
                base_z.map(|r| r * RH_MULT)
            }
        }
        // GC, UK and anything unclassified never reach here (`Add_bin`
        // filters them), but the source's default arm is NODATA.
        _ => return None,
    };

    // The attenuated fail-safe, AEL 3.1.2.4.
    if hatt && rate.is_none() {
        rate = rate_z_zdr(z, zdr).or(base_z).or_else(kdp_rate);
    }
    rate
}

/// `Add_bin_to_RR_Polar_Grid` for one bin: the class gate, the rate, the
/// negative/missing rejection, the `Max_precip_rate` cap and the mm → inch
/// conversion. `Some((rate_in_per_hr, class))` fills the bin.
fn bin_answer(
    tilt: &DprTilt,
    az: usize,
    rng: usize,
    opts: DprOptions,
) -> Option<(f32, usize, bool)> {
    let row = tilt.rows[az].as_ref()?;
    let class = *row.class.get(rng)?;
    // Ground clutter and unknown go to a higher elevation (AEL 1.1.2.4).
    if class == GC || class == UK {
        return None;
    }
    let ra = &tilt.ra[az];
    let rate = compute_ir_rate(row, ra, rng, opts.hca.metsignal, opts.vprc)?;
    if rate < 0.0 {
        // R(KDP) can produce a negative rate; that bin climbs.
        return None;
    }
    let capped = if rate > MAX_PRECIP_RATE {
        MAX_PRECIP_RATE
    } else {
        rate
    };
    let ra_used = ra.usable && rng <= ra.end && matches!(class, BD | RA | HR);
    Some(((capped * MM_TO_IN) as f32, class, ra_used))
}

// ── Public entry points ─────────────────────────────────────────────────────

/// Composite one volume's tilts into the instantaneous precipitation rate.
///
/// `tilts` come from [`crate::hhc::volume_tilts`] (scan order); `ml`, `hsda`
/// and `cappi` are the volume state [`crate::hca::compute_hca`] takes.
/// `None` when no tilt carries differential phase.
pub fn compute_dpr(
    tilts: &[(f32, Vec<Radial>)],
    params: &KdpParams,
    ml: &MeltingLayer,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
) -> Option<DerivedDpr> {
    compute_dpr_impl(tilts, params, ml, hsda, cappi, DprOptions::primary()).map(|(d, _)| d)
}

/// Diagnostics the harness prints alongside the field.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DprDiagnostics {
    /// The α of the lowest base cut.
    pub(crate) alpha: f64,
    /// What `Calculate_Alpha` estimated on the lowest base cut, whatever α
    /// the run actually used — the warm-RPG value a single volume can see.
    pub(crate) alpha_volume: Option<f64>,
    /// Filled bins whose rate came from R(A).
    pub(crate) ra_bins: usize,
    /// Radials over all tilts where R(A) was usable, and radials tried.
    pub(crate) ra_radials: usize,
    pub(crate) radials: usize,
}

pub(crate) fn compute_dpr_impl(
    tilts: &[(f32, Vec<Radial>)],
    params: &KdpParams,
    ml: &MeltingLayer,
    hsda: &HsdaHeights,
    cappi: Option<&ReflCappi>,
    opts: DprOptions,
) -> Option<(DerivedDpr, DprDiagnostics)> {
    let mut diag = DprDiagnostics::default();
    let mut first_alpha: Option<f64> = None;
    let elevations: Vec<f64> = tilts.iter().map(|(_, r)| sweep_elevation_deg(r)).collect();
    let lowest_base = elevations
        .iter()
        .copied()
        .fold(f64::MAX, |a: f64, b: f64| a.min(b));
    let mut prepared = 0usize;

    let composite = composite_hybrid_scan(
        tilts,
        |radials| {
            let rows = prepare_tilt(radials, params, ml, hsda, cappi, opts.hca)?;
            // The tilts arrive in scan order and `prepare` is called once
            // per tilt in that order, so this index tracks the elevation.
            let elev = elevations.get(prepared).copied().unwrap_or(f64::MAX);
            prepared += 1;
            let (ra, alpha, estimate) = prepare_rofa(&rows, is_alpha_cut(elev, lowest_base), opts);
            if first_alpha.is_none() {
                diag.alpha_volume = estimate;
            }
            first_alpha.get_or_insert(alpha);
            diag.radials += rows.iter().filter(|r| r.is_some()).count();
            diag.ra_radials += ra.iter().filter(|r| r.usable).count();
            Some(DprTilt { rows, ra })
        },
        |tilt: &DprTilt, az, rng| bin_answer(tilt, az, rng, opts),
    )?;
    diag.alpha = first_alpha.unwrap_or(DEFAULT_ALPHA);

    let mut values = vec![vec![f32::NAN; HHC_BINS]; HHC_AZ];
    let mut classes = vec![vec![f32::NAN; HHC_BINS]; HHC_AZ];
    for (az, row) in composite.cells.iter().enumerate() {
        for (rng, cell) in row.iter().enumerate() {
            let Some(&(rate, class, ra_used)) = cell.as_ref() else {
                continue;
            };
            if ra_used {
                diag.ra_bins += 1;
            }
            let code = crate::hca::CLASS_EXTERNAL[class];
            if code != 0.0 {
                classes[az][rng] = code;
            }
            // `buildDPR.c`: level 0 for a zero or missing rate, level 1 for
            // a nonzero rate that rounds to zero — both undefined on
            // display, so both are NaN here.
            if (f64::from(rate) * DPR_SCALE).round() > 1.0 {
                values[az][rng] = rate;
            }
        }
    }

    Some((
        DerivedDpr {
            values,
            classes,
            first_gate_km: composite.first_gate_km,
            gate_interval_km: composite.gate_interval_km,
            pct_filled: composite.pct_filled,
            highest_elev_deg: composite.highest_elev_deg,
        },
        diag,
    ))
}

/// The bars, floors and tolerances the live survey asserts, pinned outside
/// the network-bound module so the offline tests can hold them steady.
///
/// Two bars, over the volume's **wet gates** — cells where either side
/// reads more than [`WET_GATE_IN_PER_HR`]. A rain-rate field is mostly
/// zeros; scoring the zeros would measure the hybrid scan's presence rule,
/// which product 177's own survey already measured, and would drown the
/// rate arithmetic this product exists to test.
///
/// * **Primary**, on the wet gates where the derived hybrid class equals the
///   RPG's (product 177's twin for the same volume): at least
///   [`PRIMARY_PCT`] within [`primary_tolerance`]. Restricting to matched
///   classes isolates the rate arithmetic from classification residue — a
///   gate the two sides class differently takes a different *form*, and its
///   disagreement belongs to product 177's ledger, not this one. The
///   unrestricted figure is printed at every site regardless.
/// * **Secondary**, on every wet gate: at least [`SECONDARY_PCT`] within
///   [`secondary_tolerance`].
pub mod validation_policy {
    /// A gate counts as wet when either side exceeds this, inches per hour
    /// — the display threshold `palette.rs` already draws nothing below.
    pub const WET_GATE_IN_PER_HR: f64 = 0.01;

    /// The primary bar: share of class-matched wet gates inside the tight
    /// tolerance.
    pub const PRIMARY_PCT: f64 = 95.0;

    /// The secondary bar: share of all wet gates inside the loose tolerance.
    pub const SECONDARY_PCT: f64 = 99.0;

    /// The tight tolerance: `max(±0.05 in/hr, ±10%)` of the twin's rate.
    /// The absolute floor exists because the product quantizes to 0.001
    /// in/hr and the rate forms are exponential — at 0.02 in/hr a tenth of
    /// a dBZ moves the answer by more than 10%.
    pub fn primary_tolerance(twin_in_per_hr: f64) -> f64 {
        ABS_FLOOR_IN_PER_HR.max(0.10 * twin_in_per_hr.abs())
    }

    /// The loose tolerance: the same floor with ±25%. The floor is shared
    /// deliberately — a secondary bar that were *stricter* than the primary
    /// at low rates would not be a secondary bar.
    pub fn secondary_tolerance(twin_in_per_hr: f64) -> f64 {
        ABS_FLOOR_IN_PER_HR.max(0.25 * twin_in_per_hr.abs())
    }

    /// The absolute floor both tolerances share, inches per hour.
    pub const ABS_FLOOR_IN_PER_HR: f64 = 0.05;

    /// A volume with fewer wet gates than this is **skipped, not scored**:
    /// a dry or barely-wet volume measures nothing about a rain-rate field.
    /// A skip is printed, never silent.
    pub const MIN_WET_GATES: usize = 10_000;

    /// A run concludes nothing until this many sites were asserted…
    pub const MIN_SITES: usize = 3;

    /// …and this many wet gates were compared, pooled across them.
    pub const MIN_POOLED_WET_GATES: usize = 10_000;

    /// Whether a volume carries enough wet gates to be worth scoring.
    pub fn volume_is_scoreable(wet_gates: usize) -> bool {
        wet_gates >= MIN_WET_GATES
    }

    /// Whether a gate is wet: either side above the floor.
    pub fn is_wet(derived: Option<f64>, twin: Option<f64>) -> bool {
        derived.is_some_and(|v| v > WET_GATE_IN_PER_HR)
            || twin.is_some_and(|v| v > WET_GATE_IN_PER_HR)
    }

    pub fn meets_primary(pct: f64) -> bool {
        pct >= PRIMARY_PCT
    }

    pub fn meets_secondary(pct: f64) -> bool {
        pct >= SECONDARY_PCT
    }

    pub fn sample_is_conclusive(sites_asserted: usize, pooled_wet_gates: usize) -> bool {
        sites_asserted >= MIN_SITES && pooled_wet_gates >= MIN_POOLED_WET_GATES
    }

    /// One volume's rate agreement.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct RateTally {
        /// Wet gates defined on both sides.
        pub wet: usize,
        /// …of those, the ones both sides class alike.
        pub wet_class_matched: usize,
        /// Wet gates inside the tight tolerance, over all wet gates.
        pub tight: usize,
        /// …and over the class-matched ones.
        pub tight_matched: usize,
        /// Wet gates inside the loose tolerance.
        pub loose: usize,
        /// Wet cells defined on exactly one side.
        pub presence_disagreements: usize,
        /// Signed mean and mean-absolute difference, in/hr, over wet gates.
        pub sum_diff: f64,
        pub sum_abs_diff: f64,
    }

    impl RateTally {
        /// The primary figure: tight agreement on class-matched wet gates.
        pub fn primary_pct(&self) -> f64 {
            100.0 * self.tight_matched as f64 / self.wet_class_matched.max(1) as f64
        }

        /// The same tolerance without the class restriction — printed at
        /// every site, never asserted on.
        pub fn tight_unrestricted_pct(&self) -> f64 {
            100.0 * self.tight as f64 / self.wet.max(1) as f64
        }

        /// The secondary figure: loose agreement on every wet gate.
        pub fn secondary_pct(&self) -> f64 {
            100.0 * self.loose as f64 / self.wet.max(1) as f64
        }

        pub fn bias_in_per_hr(&self) -> f64 {
            self.sum_diff / self.wet.max(1) as f64
        }

        pub fn mean_abs_in_per_hr(&self) -> f64 {
            self.sum_abs_diff / self.wet.max(1) as f64
        }

        /// Score one cell. `derived`/`twin` are `None` where undefined;
        /// `class_matched` says the two sides agree on the hydrometeor
        /// class there.
        pub fn add(&mut self, derived: Option<f64>, twin: Option<f64>, class_matched: bool) {
            if !is_wet(derived, twin) {
                return;
            }
            let (Some(d), Some(t)) = (derived, twin) else {
                self.presence_disagreements += 1;
                return;
            };
            self.wet += 1;
            let diff = d - t;
            self.sum_diff += diff;
            self.sum_abs_diff += diff.abs();
            let tight = diff.abs() <= primary_tolerance(t);
            self.tight += usize::from(tight);
            self.loose += usize::from(diff.abs() <= secondary_tolerance(t));
            if class_matched {
                self.wet_class_matched += 1;
                self.tight_matched += usize::from(tight);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hca::{HsdaHeights, MeltingLayer};
    use nexrad_model::data::{MomentData, Radial, RadialStatus};

    const D_GATES: usize = 400;
    const FIRST_M: u16 = 125;
    const GATE_M: u16 = 250;

    /// A row of `n` identical gates, everything defined and meteorological.
    fn row(n: usize, class: usize, smz: f64, zdr: f64, kdp: f64, rho: f64) -> TiltRow {
        TiltRow {
            class: vec![class; n],
            smz: vec![smz; n],
            zdr: vec![zdr; n],
            kdp: vec![kdp; n],
            rho: vec![rho; n],
            met: vec![100.0; n],
            raw_smz: vec![smz; n],
            phi_ra: vec![60.0; n],
            hatt: false,
            bin_tt: n as i64,
            bin_bb: n as i64,
        }
    }

    /// Test-only deep copy (the production type never needs `Clone`).
    fn clone_row(r: &TiltRow) -> TiltRow {
        TiltRow {
            class: r.class.clone(),
            smz: r.smz.clone(),
            zdr: r.zdr.clone(),
            kdp: r.kdp.clone(),
            rho: r.rho.clone(),
            met: r.met.clone(),
            raw_smz: r.raw_smz.clone(),
            phi_ra: r.phi_ra.clone(),
            hatt: r.hatt,
            bin_tt: r.bin_tt,
            bin_bb: r.bin_bb,
        }
    }

    fn one_row_tilt(r: TiltRow) -> DprTilt {
        DprTilt {
            rows: vec![Some(r)],
            ra: vec![RofaRadial::default()],
        }
    }

    // ── The rate forms at hand-computed points ─────────────────────────────

    /// R(Z) is `Z = 300·R^1.4` inverted. At 40 dBZ:
    /// `10^((40 − 24.77121)/14) = 10^1.0877698 = 12.23969 mm/h`.
    #[test]
    fn r_of_z_at_forty_dbz() {
        let r = rate_z(40.0).expect("40 dBZ rates");
        let want = 10f64.powf((40.0 - 10.0 * 300f64.log10()) / 14.0);
        assert!((r - want).abs() < 1e-12);
        assert!(
            (r - 12.23969).abs() < 1e-5,
            "R(Z) at 40 dBZ is {r}, hand-computed 12.23969 mm/h",
        );
        // 20 dBZ, which the source itself quotes as "0.456 mm/hr under the
        // standard R(Z)".
        let light = rate_z(20.0).expect("20 dBZ rates");
        assert!(
            (light - 0.456).abs() < 5e-4,
            "R(Z) at 20 dBZ is {light}, the source's quoted 0.456 mm/h",
        );
    }

    /// `Refl_min` sends the bin up the ladder; `Refl_max` saturates — the
    /// source quotes ~100 mm/h at 52.77 dBZ, and 53 dBZ gives 103.85.
    #[test]
    fn r_of_z_honours_the_reflectivity_window() {
        assert!(rate_z(REFL_MIN - 0.1).is_none(), "below Refl_min: no rate");
        assert!(rate_z(NO_DATA).is_none(), "no Z: no rate");
        let cap = rate_z(REFL_MAX).expect("Refl_max rates");
        assert!(
            (cap - 103.83457).abs() < 1e-5,
            "the 53 dBZ cap is {cap} mm/h, hand-computed 103.83457",
        );
        assert_eq!(
            rate_z(80.0).map(|r| (r * 1e6).round()),
            Some((cap * 1e6).round()),
            "above Refl_max the rate is the cap, not more",
        );
        assert!(
            cap > rate_z(52.0).unwrap(),
            "the cap is monotone with Z below it",
        );
    }

    /// Continental R(Z, ZDR): `0.0142·z_lin^0.770·zdr_lin^-1.67`. At
    /// 40 dBZ / 1 dB, `z_lin = 10^4`, `zdr_lin = 10^0.1`:
    /// `0.0142·10^3.08·10^-0.167 = 11.62220 mm/h`.
    #[test]
    fn r_of_z_zdr_at_forty_dbz_one_db() {
        let r = rate_z_zdr(40.0, 1.0).expect("rates");
        let want = 0.0142 * 10f64.powf(4.0 * 0.770) * 10f64.powf(0.1 * -1.67);
        assert!((r - want).abs() < 1e-12);
        assert!(
            (r - 11.62220).abs() < 1e-5,
            "R(Z,ZDR) at 40 dBZ / 1 dB is {r}, hand-computed 11.62220 mm/h",
        );
        // A bigger ZDR (larger drops, fewer of them) means less rain.
        assert!(rate_z_zdr(40.0, 3.0).unwrap() < r);
        // Either input missing is no rate.
        assert!(rate_z_zdr(40.0, NO_DATA).is_none());
        assert!(rate_z_zdr(NO_DATA, 1.0).is_none());
        // Z saturates at Refl_max here too.
        assert_eq!(
            rate_z_zdr(80.0, 1.0).map(|r| (r * 1e9).round()),
            rate_z_zdr(REFL_MAX, 1.0).map(|r| (r * 1e9).round()),
        );
    }

    /// R(KDP) = `44·|KDP|^0.822·sgn(KDP)`. At 1°/km it is exactly 44; at
    /// 2°/km, `44·2^0.822 = 77.78562`.
    #[test]
    fn r_of_kdp_at_one_and_two_deg_per_km() {
        let one = rate_kdp(1.0, 0.99, RA, Some(50.0), false, 100.0, true).expect("rates");
        assert!((one - KDP_MULT).abs() < 1e-12, "R(KDP) at 1°/km is 44");
        let two = rate_kdp(2.0, 0.99, RA, Some(50.0), false, 100.0, true).expect("rates");
        assert!(
            (two - 77.78562).abs() < 1e-5,
            "R(KDP) at 2°/km is {two}, hand-computed 77.78562 mm/h",
        );
    }

    /// The wet-hail multiplier (CCR NA17-00192): 27 instead of 44, and only
    /// for an RH gate below ρ 0.97.
    #[test]
    fn r_of_kdp_uses_the_wet_hail_multiplier_only_for_rh_below_rho_097() {
        let rh_low = rate_kdp(1.0, 0.95, RH, Some(50.0), false, 100.0, true).expect("rates");
        assert!((rh_low - KDP_MULT_RH).abs() < 1e-12, "RH at ρ 0.95 → 27");
        let rh_high = rate_kdp(1.0, 0.98, RH, Some(50.0), false, 100.0, true).expect("rates");
        assert!((rh_high - KDP_MULT).abs() < 1e-12, "RH at ρ 0.98 → 44");
        let hr_low = rate_kdp(1.0, 0.95, HR, Some(50.0), false, 100.0, true).expect("rates");
        assert!((hr_low - KDP_MULT).abs() < 1e-12, "HR at ρ 0.95 → 44");
    }

    /// Every R(KDP) rejection returns the caller's R(Z) — including when
    /// that is itself absent — and a negative KDP does too.
    #[test]
    fn r_of_kdp_falls_back_to_r_of_z() {
        assert_eq!(
            rate_kdp(NO_DATA, 0.99, RA, Some(7.0), false, 100.0, true),
            Some(7.0),
            "no KDP keeps R(Z)",
        );
        assert_eq!(
            rate_kdp(NO_DATA, 0.99, RA, None, false, 100.0, true),
            None,
            "no KDP and no R(Z) is no rate",
        );
        assert_eq!(
            rate_kdp(-1.0, 0.99, RA, Some(7.0), false, 100.0, true),
            Some(7.0),
            "a negative signed rate keeps R(Z)",
        );
        assert_eq!(
            rate_kdp(1.0, 0.99, RA, Some(7.0), false, 12.0, true),
            Some(7.0),
            "a non-meteorological gate keeps R(Z)",
        );
        // The legacy (met-signal OFF) gate: ρ below corr_thresh, or R(Z)
        // below the usage floor, both fall back.
        assert_eq!(
            rate_kdp(1.0, 0.5, RA, Some(20.0), false, f64::NAN, false),
            Some(20.0),
        );
        assert_eq!(
            rate_kdp(1.0, 0.99, RA, Some(1.0), false, f64::NAN, false),
            Some(1.0),
            "R(Z) under Kdp_min_usage_rate keeps R(Z)",
        );
        // An attenuated radial skips every check.
        assert!(
            (rate_kdp(1.0, 0.5, RA, Some(1.0), true, f64::NAN, false).unwrap() - KDP_MULT).abs()
                < 1e-12,
        );
    }

    // ── The class multiplier table ─────────────────────────────────────────

    /// Each class's rate is its documented multiple of R(Z) — the table the
    /// module doc lists, evaluated at 40 dBZ where R(Z) = 12.2438 mm/h.
    #[test]
    fn the_class_multiplier_table() {
        let base = rate_z(40.0).expect("R(Z)");
        let ra = RofaRadial::default();
        let n = 20usize;
        let cases: [(usize, usize, f64, &str); 6] = [
            (GR, 5, GR_MULT, "graupel 0.8·R(Z)"),
            (IC, 5, IC_MULT, "crystals 2.8·R(Z)"),
            (WS, 5, WS_MULT, "wet snow 0.6·R(Z)"),
            (DS, 15, DS_MULT, "dry snow above the ML top 2.8·R(Z)"),
            (DS, 5, DS_BELOW_ML_TOP_MULT, "dry snow below it 1.0·R(Z)"),
            (RH, 15, RH_MULT, "wet hail above the ML top 0.8·R(Z)"),
        ];
        for (class, rng, mult, why) in cases {
            let mut r = row(n, class, 40.0, 1.0, NO_DATA, 0.99);
            r.bin_tt = 10;
            let got = compute_ir_rate(&r, &ra, rng, true, Vprc::Off).expect(why);
            assert!(
                (got - base * mult).abs() < 1e-9,
                "{why}: got {got}, want {}",
                base * mult,
            );
        }
        // RH at or before the ML top rates by KDP, not by a multiple.
        let mut r = row(n, RH, 40.0, 1.0, 1.0, 0.95);
        r.bin_tt = 10;
        let kdp_side =
            compute_ir_rate(&r, &ra, 5, true, Vprc::Off).expect("RH below the top rates");
        assert!(
            (kdp_side - KDP_MULT_RH).abs() < 1e-12,
            "RH at or before beam_edge_top is R(KDP), got {kdp_side}",
        );
        // RA and BD take R(Z, ZDR) when R(A) is unavailable.
        for class in [RA, BD] {
            let r = row(n, class, 40.0, 1.0, NO_DATA, 0.99);
            let got = compute_ir_rate(&r, &ra, 5, true, Vprc::Off).expect("rain rates");
            assert!((got - rate_z_zdr(40.0, 1.0).unwrap()).abs() < 1e-9);
        }
        // HR above Hr_HighZThresh switches to R(KDP); below it to R(Z,ZDR).
        let hot = row(n, HR, 50.0, 1.0, 1.0, 0.99);
        assert!((compute_ir_rate(&hot, &ra, 5, true, Vprc::Off).unwrap() - KDP_MULT).abs() < 1e-12);
        let mild = row(n, HR, 44.0, 1.0, 1.0, 0.99);
        assert!(
            (compute_ir_rate(&mild, &ra, 5, true, Vprc::Off).unwrap()
                - rate_z_zdr(44.0, 1.0).unwrap())
            .abs()
                < 1e-9,
        );
        // Biology and no echo are rate 0, filled, before any other check.
        for class in [BI, NE] {
            let mut r = row(n, class, NO_DATA, NO_DATA, NO_DATA, NO_DATA);
            r.met = vec![0.0; n];
            assert_eq!(compute_ir_rate(&r, &ra, 5, true, Vprc::Off), Some(0.0));
        }
        // A gate under the met-signal threshold climbs the ladder.
        let mut weak = row(n, RA, 40.0, 1.0, NO_DATA, 0.99);
        weak.met = vec![MET_SIG_THRESHOLD - 0.5; n];
        assert_eq!(compute_ir_rate(&weak, &ra, 5, true, Vprc::Off), None);
    }

    /// The attenuated fail-safe (AEL 3.1.2.4): on a high-attenuation radial
    /// a class whose own form fails re-tries R(Z, ZDR), then R(Z), then
    /// R(KDP) — and on a normal radial it does not.
    #[test]
    fn the_attenuated_fail_safe_retries_in_order() {
        let ra = RofaRadial::default();
        let n = 20usize;
        // RA with no ZDR: R(Z, ZDR) fails. Normal radial → no rate.
        let plain = row(n, RA, 40.0, NO_DATA, NO_DATA, 0.99);
        assert_eq!(compute_ir_rate(&plain, &ra, 5, true, Vprc::Off), None);
        // Attenuated: R(Z, ZDR) fails again, R(Z) answers.
        let mut att = row(n, RA, 40.0, NO_DATA, NO_DATA, 0.99);
        att.hatt = true;
        let got = compute_ir_rate(&att, &ra, 5, true, Vprc::Off).expect("the fail-safe answers");
        assert!((got - rate_z(40.0).unwrap()).abs() < 1e-9, "the R(Z) leg");
        // Attenuated with no Z either: R(KDP) is the last leg.
        let mut kdp_only = row(n, RA, NO_DATA, NO_DATA, 2.0, 0.99);
        kdp_only.hatt = true;
        let got =
            compute_ir_rate(&kdp_only, &ra, 5, true, Vprc::Off).expect("the R(KDP) leg answers");
        assert!((got - KDP_MULT * 2f64.powf(KDP_POWER)).abs() < 1e-9);
    }

    /// The `vprc_switch` A/B moves exactly four classes onto different
    /// branches: IC and WS lose their multipliers, DS loses the
    /// above-the-melting-layer one, and RA/BD beyond `beam_edge_bottom`
    /// rate by R(Z) instead of R(Z, ZDR). GR, HR and RH are untouched
    /// (`vprZ_in_GR = NO` with `use_Gr_mult = YES` lands on one expression).
    #[test]
    fn the_vprc_branch_drops_four_multipliers() {
        let base = rate_z(40.0).expect("R(Z)");
        let ra = RofaRadial::default();
        let n = 20usize;
        let at = |class: usize, rng: usize, vprc: Vprc, tt: i64, bb: i64| {
            let mut r = row(n, class, 40.0, 1.0, NO_DATA, 0.99);
            r.bin_tt = tt;
            r.bin_bb = bb;
            compute_ir_rate(&r, &ra, rng, true, vprc)
        };
        // IC and WS lose their multipliers.
        assert!((at(IC, 5, Vprc::Off, 10, 20).unwrap() - base * IC_MULT).abs() < 1e-9);
        assert!((at(IC, 5, Vprc::OnVersionB, 10, 20).unwrap() - base).abs() < 1e-9);
        assert!((at(WS, 5, Vprc::Off, 10, 20).unwrap() - base * WS_MULT).abs() < 1e-9);
        assert!((at(WS, 5, Vprc::OnVersionB, 10, 20).unwrap() - base).abs() < 1e-9);
        // DS keeps R(Z) on both sides of the melting-layer top.
        assert!((at(DS, 15, Vprc::Off, 10, 20).unwrap() - base * DS_MULT).abs() < 1e-9);
        assert!((at(DS, 15, Vprc::OnVersionB, 10, 20).unwrap() - base).abs() < 1e-9);
        assert!((at(DS, 5, Vprc::OnVersionB, 10, 20).unwrap() - base).abs() < 1e-9);
        // RA beyond beam_edge_bottom takes R(Z); inside it, R(Z, ZDR) still.
        let zzdr = rate_z_zdr(40.0, 1.0).unwrap();
        assert!((at(RA, 15, Vprc::OnVersionB, 20, 10).unwrap() - base).abs() < 1e-9);
        assert!((at(RA, 5, Vprc::OnVersionB, 20, 10).unwrap() - zzdr).abs() < 1e-9);
        assert!((at(RA, 15, Vprc::Off, 20, 10).unwrap() - zzdr).abs() < 1e-9);
        // GR and RH are the same either way.
        for vprc in [Vprc::Off, Vprc::OnVersionB] {
            assert!((at(GR, 5, vprc, 10, 20).unwrap() - base * GR_MULT).abs() < 1e-9);
            assert!((at(RH, 15, vprc, 10, 20).unwrap() - base * RH_MULT).abs() < 1e-9);
        }
    }

    /// `RofaAlpha` selects which α the lowest cut runs with, and only the
    /// lowest cut: `is_alpha_cut` reads the volume's own lowest base angle
    /// because the archive reports slewing first-radial angles.
    #[test]
    fn the_alpha_cut_is_the_volumes_lowest_base_elevation() {
        // A 0.5° VCP: the lowest cut and its SAILS repeat qualify, the 0.9°
        // cut does not — even though the reported angles are much closer
        // together than the targets.
        assert!(is_alpha_cut(0.53, 0.53));
        assert!(is_alpha_cut(0.60, 0.53));
        assert!(!is_alpha_cut(0.92, 0.53));
        assert!(!is_alpha_cut(1.27, 0.53));
        // A site whose lowest cut is 0.2° still runs α; a volume that opens
        // at 3.1° (a partial) never does.
        assert!(is_alpha_cut(0.2, 0.2));
        assert!(!is_alpha_cut(3.1, 3.1));
        // The sweep angle is the median, not the first radial's.
        let radials = [0.66f32, 0.53, 0.52, 0.53, 0.54];
        let sweep: Vec<Radial> = radials
            .iter()
            .enumerate()
            .map(|(k, &e)| {
                Radial::new(
                    0,
                    0,
                    k as f32,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    1,
                    e,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        assert!((sweep_elevation_deg(&sweep) - 0.53).abs() < 1e-5);
        assert_eq!(sweep_elevation_deg(&[]), f64::MAX);
    }

    // ── The 200 mm/h cap and the encoding ──────────────────────────────────

    /// `Add_bin` caps at `Max_precip_rate` and stores inches per hour:
    /// 200 mm/h · MM_TO_IN = 7.874016 in/hr.
    #[test]
    fn the_cap_is_two_hundred_millimetres_per_hour() {
        let n = 20usize;
        // R(KDP) at 8°/km is 44·8^0.822 = 239.6 mm/h — over the cap.
        let hot = row(n, HR, 50.0, 1.0, 8.0, 0.99);
        let raw = compute_ir_rate(&hot, &RofaRadial::default(), 5, true, Vprc::Off).expect("rates");
        assert!(raw > MAX_PRECIP_RATE, "the raw rate {raw} exceeds the cap");
        let tilt = one_row_tilt(hot);
        let (stored, class, _) = bin_answer(&tilt, 0, 5, DprOptions::primary()).expect("fills");
        assert_eq!(class, HR);
        let want = (MAX_PRECIP_RATE * MM_TO_IN) as f32;
        assert_eq!(stored, want, "stored {stored} in/hr, cap {want}");
        assert!(
            (f64::from(want) - 7.874016).abs() < 1e-6,
            "the cap is 7.874016 in/hr",
        );
        // Under the cap the rate passes through unit-converted.
        let tilt = one_row_tilt(row(n, GR, 40.0, 1.0, NO_DATA, 0.99));
        let (stored, _, _) = bin_answer(&tilt, 0, 5, DprOptions::primary()).expect("fills");
        let want = (rate_z(40.0).unwrap() * GR_MULT * MM_TO_IN) as f32;
        assert_eq!(stored, want);
    }

    /// Ground clutter and unknown never fill — they climb — and a bin past
    /// the moment's gates climbs too.
    #[test]
    fn clutter_and_unknown_climb_the_ladder() {
        for class in [GC, UK] {
            let tilt = one_row_tilt(row(20, class, 40.0, 1.0, 1.0, 0.99));
            assert!(bin_answer(&tilt, 0, 5, DprOptions::primary()).is_none());
        }
        let tilt = one_row_tilt(row(20, RA, 40.0, 1.0, 1.0, 0.99));
        assert!(
            bin_answer(&tilt, 0, 50, DprOptions::primary()).is_none(),
            "beyond the moment, the bin climbs",
        );
    }

    // ── R(A) on synthetic profiles ─────────────────────────────────────────

    /// A synthetic liquid radial: `n` gates of rain with φ rising linearly,
    /// the melting-layer bottom one gate short of the end.
    fn ra_row(n: usize, z: f64, dphi_per_gate: f64) -> TiltRow {
        let mut r = row(n, RA, z, 1.0, NO_DATA, 0.99);
        r.phi_ra = (0..n).map(|i| 60.0 + dphi_per_gate * i as f64).collect();
        r.bin_bb = n as i64 - 1;
        r.bin_tt = n as i64;
        r
    }

    /// r1/r2 bracket the liquid stretch, and a non-liquid or unsignalled
    /// gate is excluded from both ends.
    #[test]
    fn r_of_a_brackets_the_liquid_gates() {
        let mut r = ra_row(200, 35.0, 0.05);
        for m in r.met.iter_mut().take(20) {
            *m = 10.0;
        }
        for m in r.met.iter_mut().skip(150) {
            *m = 10.0;
        }
        let (r1, r2) = ra_start_end(&r).expect("brackets");
        assert_eq!(r1, 20, "r1 is the first signalled liquid gate");
        assert_eq!(r2, 149, "r2 is the last one before beam_edge_bottom");

        // No liquid at all: no R(A) on the radial.
        let dry = row(200, DS, 25.0, 1.0, NO_DATA, 0.99);
        assert!(ra_start_end(&dry).is_none());
        // A melting layer at the ground gives beam_edge_bottom 0 and the
        // r1 scan finds nothing.
        let mut ground = ra_row(200, 35.0, 0.05);
        ground.bin_bb = 0;
        assert!(ra_start_end(&ground).is_none());
        // Raw Z above MAX_ROFA_REF_THRESH disqualifies a gate.
        let hail = ra_row(200, 55.0, 0.05);
        assert!(ra_start_end(&hail).is_none());
    }

    /// PIA is α·ΔΦ over the bracket, and the phase accumulated across a run
    /// of non-liquid gates is removed from ΔΦ.
    #[test]
    fn r_of_a_path_integrated_attenuation() {
        let r = ra_row(101, 35.0, 0.1);
        let (r1, r2) = ra_start_end(&r).expect("brackets");
        assert_eq!(
            (r1, r2),
            (0, 100),
            "r1 scans [0, beam_edge_bottom) but r2's scan includes it",
        );
        let pia = ra_pia(&r, r1, r2, DEFAULT_ALPHA).expect("a PIA");
        let want = DEFAULT_ALPHA * 0.1 * (r2 - r1) as f64;
        assert!((pia - want).abs() < 1e-12, "PIA {pia}, want {want}");

        // Make gates 40…59 non-liquid: their 2.0° of phase is removed.
        let mut gapped = ra_row(101, 35.0, 0.1);
        for c in gapped.class[40..60].iter_mut() {
            *c = DS;
        }
        let (g1, g2) = ra_start_end(&gapped).expect("brackets");
        let gapped_pia = ra_pia(&gapped, g1, g2, DEFAULT_ALPHA).expect("a PIA");
        // φ1 is the last good gate before the run (39) and the resume
        // gate is the first good one after it (60): 21 gates of phase.
        let removed = 0.1 * 21.0;
        assert!(
            (gapped_pia - DEFAULT_ALPHA * (0.1 * (g2 - g1) as f64 - removed)).abs() < 1e-9,
            "the bad run's phase is removed: {gapped_pia}",
        );
        assert!(gapped_pia < pia, "and that leaves less attenuation");

        // A radial whose φ falls has ΔΦ < 0 and no R(A).
        let falling = ra_row(101, 35.0, -0.1);
        let (f1, f2) = ra_start_end(&falling).expect("brackets");
        assert!(ra_pia(&falling, f1, f2, DEFAULT_ALPHA).is_none());
    }

    /// I(r1,r2) is `0.46·β·Σ z_lin^β·0.25`, and the R(A) rate is `Γ·A^Δ` —
    /// hand-computed on a uniform 35 dBZ profile.
    #[test]
    fn r_of_a_integral_and_rate() {
        let r = ra_row(101, 35.0, 0.1);
        let (r1, r2) = ra_start_end(&r).expect("brackets");
        let integral = ra_ref_integral(&r, r1, r2);
        let z_lin = 10f64.powf(3.5);
        let per_gate = z_lin.powf(BETA);
        let want = 0.46 * BETA * per_gate * (r2 - r1 + 1) as f64 * 0.25;
        assert!(
            (integral - want).abs() < 1e-6,
            "I(r1,r2) {integral}, want {want}",
        );
        assert!(
            integral >= MIN_REF_INT_THRESH,
            "a 100-gate 35 dBZ rain shaft clears MIN_REF_INT_THRESH",
        );

        let pia = ra_pia(&r, r1, r2, DEFAULT_ALPHA).expect("a PIA");
        let radial = RofaRadial {
            usable: true,
            end: r2,
            ref_integral: integral,
            pia,
            pia_low: pia,
            pia_high: pia,
        };
        // At the first gate the forward integral is the whole radial's, so
        // A = z^β·C/(I + C·I) and R = Γ·A^Δ.
        let rate = rate_ra(&r, &radial, r1);
        let c = (0.23 * BETA * pia).exp() - 1.0;
        let want = GAMMA * ((per_gate * c) / (integral + c * integral)).powf(BIGDELTA);
        assert!((rate - want).abs() < 1e-9, "R(A) {rate}, want {want}");
        assert!(
            rate > 0.0 && rate < MAX_PRECIP_RATE,
            "a 35 dBZ shaft rates {rate} mm/h — physical",
        );
        // Deeper into the radial less attenuation remains ahead, so the
        // rate rises: A's denominator shrinks with I(r, r2).
        assert!(rate_ra(&r, &radial, r2 - 1) > rate);
        // A missing raw Z underflows to a zero rate, as the C's sentinel
        // does.
        let mut blank = clone_row(&r);
        blank.raw_smz[r1] = NO_DATA;
        assert_eq!(rate_ra(&blank, &radial, r1), 0.0);
    }

    /// `PIA_ref_thresh` selects between the two smoothings, and the
    /// smoothing itself is a circular median: an outlier radial is replaced
    /// by its neighbours and the window wraps at azimuth 0.
    #[test]
    fn r_of_a_pia_smoothing() {
        let mut pia = vec![1.0f64; HHC_AZ];
        pia[10] = 100.0;
        let low = smooth_pia(&pia, PIA_SMOOTH_LOW);
        assert!(
            (low[10] - 1.0).abs() < 1e-12,
            "the 9-median rejects a spike"
        );
        let high = smooth_pia(&pia, PIA_SMOOTH_HIGH);
        assert!((high[10] - 1.0).abs() < 1e-12, "so does the 3-median");
        // Circular: azimuth 0's window reaches 359.
        let mut wrap = vec![f64::NAN; HHC_AZ];
        wrap[HHC_AZ - 1] = 5.0;
        wrap[0] = 5.0;
        let sm = smooth_pia(&wrap, PIA_SMOOTH_HIGH);
        assert!(
            (sm[0] - 5.0).abs() < 1e-12 && (sm[HHC_AZ - 1] - 5.0).abs() < 1e-12,
            "the wrap-around window sees both",
        );
        assert!(sm[180].is_nan(), "a window with no PIA stays undefined");

        // The threshold picks PIA_low below 35 dBZ, PIA_high at or above.
        let n = 20usize;
        let mut r = ra_row(n, 30.0, 0.1);
        r.raw_smz = vec![PIA_REF_THRESH - 5.0; n];
        let radial = RofaRadial {
            usable: true,
            end: n - 1,
            ref_integral: 1000.0,
            pia: 1.0,
            pia_low: 1.0,
            pia_high: 4.0,
        };
        let below = rate_ra(&r, &radial, 0);
        r.raw_smz = vec![PIA_REF_THRESH; n];
        let at = rate_ra(&r, &radial, 0);
        assert!(
            at > below,
            "at PIA_ref_thresh the high-rate PIA takes over ({at} vs {below})",
        );
    }

    /// `Calculate_Alpha` needs its sample checks to pass, clamps into
    /// [0.015, 0.040], and falls to the stratiform value when only the
    /// 10–30 dBZ check passes. A thin sample yields nothing.
    #[test]
    fn alpha_from_the_zdr_versus_z_slope() {
        let empty: Tilt = (0..HHC_AZ).map(|_| None).collect();
        assert_eq!(calculate_alpha(&empty), None, "no data, no α");

        // ZDR rising 0.05 dB per dBZ: α = −0.75·0.05 + 0.04875 = 0.01125,
        // clamped up to the floor.
        let alpha = alpha_for_slope(0.05).expect("the convective check passes");
        assert!(
            (alpha - MIN_ALLOWABLE_ALPHA).abs() < 1e-12,
            "a steep slope clamps to the floor, got {alpha}",
        );
        // −0.75·(−0.01) + 0.04875 = 0.05625 → clamps to the ceiling.
        let alpha = alpha_for_slope(-0.01).expect("passes");
        assert!((alpha - MAX_ALLOWABLE_ALPHA).abs() < 1e-12);
        // slope 0.02 → −0.015 + 0.04875 = 0.03375, inside the range.
        let alpha = alpha_for_slope(0.02).expect("passes");
        assert!(
            (alpha - 0.03375).abs() < 1e-9,
            "α from slope 0.02 is {alpha}, hand-computed 0.03375",
        );

        // Only the 10–30 dBZ groups populated → the stratiform value.
        let strat = tilt_with_pairs(11.0, 29.0, MIN_NUM_PAIRS_10TO30, 0.0);
        assert_eq!(calculate_alpha(&strat), Some(STRATIFORM_ALPHA));
        // Populated but under the per-group floor → nothing.
        let thin = tilt_with_pairs(11.0, 49.0, 10, 0.0);
        assert_eq!(calculate_alpha(&thin), None);

        // The least-squares slope itself, on a clean line.
        let slope = least_squares_slope(&[21.0, 23.0, 25.0], &[1.0, 1.2, 1.4]).expect("a slope");
        assert!((slope - 0.1).abs() < 1e-12);
        assert!(least_squares_slope(&[1.0], &[1.0]).is_none());
        // `quick_select`'s lower median.
        assert_eq!(lower_median(&mut [1.0, 2.0, 3.0, 4.0]), 2.0);
        assert_eq!(lower_median(&mut [3.0, 1.0, 2.0]), 2.0);
    }

    fn alpha_for_slope(slope: f64) -> Option<f64> {
        calculate_alpha(&tilt_with_pairs(11.0, 49.0, MIN_NUM_PAIRS_20TO40, slope))
    }

    /// `per_group` gates in every 2-dBZ group from `lo` to `hi`, ZDR on the
    /// line `1.0 + slope·(z − 30)`, all past gate 80 and inside the melting
    /// layer's bottom.
    fn tilt_with_pairs(lo: f64, hi: f64, per_group: usize, slope: f64) -> Tilt {
        let centres: Vec<f64> = (0..NUM_GROUPS)
            .map(|i| 11.0 + 2.0 * i as f64)
            .filter(|c| *c >= lo && *c <= hi)
            .collect();
        let n = 81 + centres.len() * per_group;
        let mut smz = vec![NO_DATA; n];
        let mut zdr = vec![NO_DATA; n];
        let mut i = 81;
        for &c in &centres {
            for _ in 0..per_group {
                smz[i] = c;
                zdr[i] = 1.0 + slope * (c - 30.0);
                i += 1;
            }
        }
        let r = TiltRow {
            class: vec![RA; n],
            smz,
            zdr,
            kdp: vec![NO_DATA; n],
            rho: vec![0.99; n],
            met: vec![100.0; n],
            raw_smz: vec![NO_DATA; n],
            phi_ra: vec![60.0; n],
            hatt: false,
            bin_tt: n as i64,
            bin_bb: n as i64,
        };
        let mut tilt: Tilt = (0..HHC_AZ).map(|_| None).collect();
        tilt[0] = Some(r);
        tilt
    }

    // ── The validation policy's pins ───────────────────────────────────────

    /// The bars, tolerances and floors are what the campaign plan pinned,
    /// and the tolerance functions behave as documented at the floor and
    /// above it.
    #[test]
    fn validation_policy_pins() {
        use validation_policy as p;
        assert_eq!(p::PRIMARY_PCT, 95.0);
        assert_eq!(p::SECONDARY_PCT, 99.0);
        assert_eq!(p::ABS_FLOOR_IN_PER_HR, 0.05);
        assert_eq!(p::WET_GATE_IN_PER_HR, 0.01);
        assert_eq!(p::MIN_WET_GATES, 10_000);
        assert_eq!(p::MIN_SITES, 3);
        assert_eq!(p::MIN_POOLED_WET_GATES, 10_000);

        // Below 0.5 in/hr the ±10% leg is under the floor, so the floor
        // rules; above it the percentage does.
        assert_eq!(p::primary_tolerance(0.2), 0.05);
        assert!((p::primary_tolerance(2.0) - 0.2).abs() < 1e-12);
        // The secondary is never tighter than the primary.
        for t in [0.0, 0.01, 0.2, 0.5, 1.0, 5.0, 8.0] {
            assert!(
                p::secondary_tolerance(t) >= p::primary_tolerance(t),
                "the secondary must not be stricter than the primary at {t}",
            );
        }
        assert!((p::secondary_tolerance(4.0) - 1.0).abs() < 1e-12);

        assert!(p::meets_primary(95.0) && !p::meets_primary(94.99));
        assert!(p::meets_secondary(99.0) && !p::meets_secondary(98.99));
        assert!(p::volume_is_scoreable(10_000) && !p::volume_is_scoreable(9_999));
        assert!(p::sample_is_conclusive(3, 10_000));
        assert!(!p::sample_is_conclusive(2, 10_000));
        assert!(!p::sample_is_conclusive(3, 9_999));

        // The product's encoding, verified live before it is trusted.
        assert_eq!(DPR_SCALE, 1000.0);
        assert!((MM_TO_IN - 0.03937008).abs() < 1e-12);
    }

    /// The wet-gate rule: a cell counts only when a side exceeds the floor,
    /// a one-sided wet cell is a presence disagreement rather than a scored
    /// gate, and the class restriction narrows the primary population
    /// without touching the secondary.
    #[test]
    fn the_wet_gate_floor_rule() {
        use validation_policy as p;
        assert!(!p::is_wet(Some(0.005), Some(0.004)), "both dry");
        assert!(p::is_wet(Some(0.02), None), "derived wet alone");
        assert!(p::is_wet(None, Some(0.02)), "twin wet alone");
        assert!(!p::is_wet(None, None));

        let mut t = p::RateTally::default();
        // Dry on both sides: invisible.
        t.add(Some(0.001), Some(0.002), true);
        assert_eq!((t.wet, t.presence_disagreements), (0, 0));
        // Wet on one side only: a presence disagreement, not a gate.
        t.add(Some(0.5), None, true);
        assert_eq!((t.wet, t.presence_disagreements), (0, 1));
        // Wet, matched, inside the tight tolerance.
        t.add(Some(1.05), Some(1.0), true);
        // Wet, matched, outside tight but inside loose (±25% of 1.0).
        t.add(Some(1.2), Some(1.0), true);
        // Wet, unmatched class, well outside both.
        t.add(Some(4.0), Some(1.0), false);
        assert_eq!(t.wet, 3);
        assert_eq!(t.wet_class_matched, 2);
        assert_eq!(t.tight_matched, 1);
        assert_eq!(t.tight, 1);
        assert_eq!(t.loose, 2);
        assert!((t.primary_pct() - 50.0).abs() < 1e-9);
        assert!((t.tight_unrestricted_pct() - 100.0 / 3.0).abs() < 1e-9);
        assert!((t.secondary_pct() - 200.0 / 3.0).abs() < 1e-9);
        assert!(t.bias_in_per_hr() > 0.0, "the derived side reads high here");
        assert!(t.mean_abs_in_per_hr() >= t.bias_in_per_hr().abs());

        // An empty tally divides by the guarded maximum, not by zero.
        let empty = p::RateTally::default();
        assert_eq!(empty.primary_pct(), 0.0);
        assert_eq!(empty.secondary_pct(), 0.0);
    }

    // ── End to end on a synthetic volume ───────────────────────────────────

    #[derive(Clone, Copy)]
    enum G {
        V(f64),
    }

    fn raw_of(scale: f32, offset: f32, g: G) -> u16 {
        match g {
            G::V(v) => (v * f64::from(scale) + f64::from(offset)).round() as u16,
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
            .map(|&g| raw_of(scale, offset, g) as u8)
            .collect();
        MomentData::from_fixed_point(vals.len() as u16, FIRST_M, GATE_M, 8, scale, offset, bytes)
    }

    /// One tilt of 360 one-degree radials of uniform rain, φ rising
    /// 0.05°/gate so the R(A) subsystem has attenuation to work with.
    fn rain_tilt(elev: f32, z: f64) -> Vec<Radial> {
        let zv: Vec<G> = (0..D_GATES).map(|_| G::V(z)).collect();
        let zdr: Vec<G> = (0..D_GATES).map(|_| G::V(1.0)).collect();
        let rho: Vec<G> = (0..D_GATES).map(|_| G::V(0.99)).collect();
        let phi: Vec<G> = (0..D_GATES).map(|i| G::V(60.0 + 0.05 * i as f64)).collect();
        (0..360)
            .map(|k| {
                Radial::new(
                    0,
                    0,
                    0.5 + k as f32,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    1,
                    elev,
                    Some(m8(2.0, 66.0, &zv)),
                    None,
                    None,
                    Some(m8(16.0, 128.0, &zdr)),
                    Some(m16(10.0, 2.0, &phi)),
                    Some(m16(500.0, 2.0, &rho)),
                    None,
                )
            })
            .collect()
    }

    fn params() -> KdpParams {
        KdpParams {
            init_fdp_deg: Some(60.0),
            dbz0: Some(-40.0),
            atmos_db_per_km: Some(-0.012),
            isdp_est_deg: None,
        }
    }

    fn hsda_far() -> HsdaHeights {
        HsdaHeights {
            tw0_km_arl: 100.0,
            twm25_km_arl: 105.0,
        }
    }

    /// A uniform 40 dBZ rain volume rates everywhere the moment reaches, on
    /// the product's own geometry and in inches per hour, with the
    /// companion class grid filled at exactly the same bins.
    #[test]
    fn a_uniform_rain_volume_rates_end_to_end() {
        let tilts = vec![(0.5f32, rain_tilt(0.5, 40.0))];
        let ml = MeltingLayer::flat(4.0);
        let d = compute_dpr(&tilts, &params(), &ml, &hsda_far(), None).expect("computes");
        assert_eq!(d.values.len(), HHC_AZ);
        assert_eq!(d.values[0].len(), HHC_BINS);
        assert!((d.gate_interval_km - 0.25).abs() < 1e-9);
        assert!((d.first_gate_km - 0.125).abs() < 1e-9);

        for r in 40..300 {
            let v = d.values[100][r];
            assert!(v.is_finite(), "bin {r} rates");
            assert!(
                (0.01..8.0).contains(&v),
                "bin {r} rates {v} in/hr — inside the product's range",
            );
            assert!(
                d.classes[100][r].is_finite(),
                "bin {r} carries its class too",
            );
        }
        assert!(
            d.values[100][500].is_nan(),
            "beyond the moment: nothing filled",
        );
        assert!(d.classes[100][500].is_nan());
        assert!((d.highest_elev_deg - 0.5).abs() < 1e-6);
        assert!(d.pct_filled > 40.0, "filled {}%", d.pct_filled);

        // The comparison grid keeps the field.
        let grid = d.to_polar_grid();
        assert_eq!(grid.len(), 360);
        assert!(grid[100][20].is_finite());
        assert!(d.classes_to_polar_grid()[100][20].is_finite());
    }

    /// A zero rate encodes as level 0, indistinguishable from no rate, so
    /// the field reports it as undefined — the product's own presence rule.
    #[test]
    fn a_zero_rate_reads_undefined() {
        // Below the SNR floor everywhere: every bin classifies as no echo
        // and fills with rate 0.0, which the product cannot express.
        let tilts = vec![(0.5f32, rain_tilt(0.5, -20.0))];
        let ml = MeltingLayer::flat(4.0);
        let d = compute_dpr(&tilts, &params(), &ml, &hsda_far(), None).expect("computes");
        assert!(
            !d.values.iter().flatten().any(|v| v.is_finite()),
            "a clear volume rates nothing",
        );
        assert!(
            d.pct_filled > 40.0,
            "…but the ladder filled it: {}%",
            d.pct_filled,
        );
    }

    /// Turning R(A) off changes the rain rate — the subsystem is live in
    /// the primary, not dead code — and α is the cold-start default there.
    #[test]
    fn the_specific_attenuation_path_is_live() {
        let tilts = vec![(0.5f32, rain_tilt(0.5, 35.0))];
        let ml = MeltingLayer::flat(4.0);
        let (with, diag) = compute_dpr_impl(
            &tilts,
            &params(),
            &ml,
            &hsda_far(),
            None,
            DprOptions::primary(),
        )
        .expect("computes");
        assert!(
            (diag.alpha - DEFAULT_ALPHA).abs() < 1e-12,
            "the primary runs the cold-start α",
        );
        assert!(
            diag.ra_bins > 0,
            "R(A) rated some bins ({} of them)",
            diag.ra_bins,
        );
        assert!(diag.ra_radials > 0, "and some radials qualified");

        let (without, diag_off) = compute_dpr_impl(
            &tilts,
            &params(),
            &ml,
            &hsda_far(),
            None,
            DprOptions {
                rofa: false,
                ..DprOptions::primary()
            },
        )
        .expect("computes");
        assert_eq!(diag_off.ra_bins, 0);
        assert_eq!(diag_off.ra_radials, 0);
        let differing = (0..HHC_AZ)
            .flat_map(|az| (0..HHC_BINS).map(move |r| (az, r)))
            .filter(|&(az, r)| {
                let a = with.values[az][r];
                let b = without.values[az][r];
                a.is_finite() && b.is_finite() && (a - b).abs() > 1e-6
            })
            .count();
        assert!(
            differing > 1000,
            "R(A) moved {differing} bins — the switch is not a no-op",
        );
    }
}

/// The live twin harness: score the derived rate against the RPG's own
/// `DPR` (product 176) for the **same volume**, on the volume's wet gates.
///
/// ```text
/// cargo test -p rustdar-radar --release --lib -- --ignored --nocapture live_derived_dpr
/// ```
///
/// Pairing follows [`crate::hhc`]'s: by volume start, taking the volume's
/// *latest* object, because qperate emits one intermediate per SAILS/MRLE
/// scan before the end-of-volume product. The **class mask** for the
/// primary bar comes from the same volume's `HHC` (product 177) twin — the
/// grid the RPG fills from the very same `compute_IRRate` outcome — so a
/// gate the two sides class differently is excluded from the primary and
/// counted only in the printed unrestricted figure.
///
/// Site-hours come from `DPR_SITE_HOURS`
/// (`KUEX=2026-07-29T08:00,KOAX=...`); with no override every site in
/// [`crate::twin::live::SITES`] is tried at the current hour, which in a
/// dry spell measures nothing — a rain-rate product must be surveyed in
/// precipitation, and the wet-gate floor makes a dry volume a printed skip
/// rather than a silent pass.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod live_validation {
    use super::validation_policy as policy;
    use super::*;
    use crate::sources::DataSources;
    use crate::twin::{compare, live};
    use crate::volumetric::RANGE_BINS;

    fn site_hours() -> Vec<(String, chrono::NaiveDateTime)> {
        let now = chrono::Utc::now().naive_utc();
        match std::env::var("DPR_SITE_HOURS") {
            Ok(spec) if !spec.trim().is_empty() => spec
                .split([',', ';'])
                .filter_map(|pair| {
                    let (site, when) = pair.trim().split_once('=')?;
                    let when = chrono::NaiveDateTime::parse_from_str(when.trim(), "%Y-%m-%dT%H:%M")
                        .unwrap_or_else(|e| panic!("bad DPR_SITE_HOURS entry {pair}: {e}"));
                    Some((site.trim().to_uppercase(), when))
                })
                .collect(),
            _ => live::SITES.iter().map(|s| (s.to_string(), now)).collect(),
        }
    }

    /// A twin's radial packet resampled to physical values on the 360° ×
    /// 230 km grid, `None` where the product defines nothing.
    fn twin_values(msg: &nexrad_level3::model::Level3Message) -> Option<Vec<Vec<Option<f64>>>> {
        let packet = crate::srm::radial_packet(msg)?;
        let codec = compare::ValueCodec::for_message(msg)?;
        let levels = compare::resample_packet_levels(packet, compare::gate_km(&msg.pdb, packet));
        Some(
            levels
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|g| {
                            g.map(|g| f64::from(codec.decode(g)))
                                .filter(|v| v.is_finite())
                        })
                        .collect()
                })
                .collect(),
        )
    }

    /// Score one derived pair against the twin grids.
    fn score(
        rate: &[Vec<f32>],
        class: &[Vec<f32>],
        twin_rate: &[Vec<Option<f64>>],
        twin_class: &[Vec<Option<f64>>],
    ) -> policy::RateTally {
        let mut tally = policy::RateTally::default();
        for az in 0..360 {
            for r in 0..RANGE_BINS {
                let d = rate[az][r];
                let d = d.is_finite().then(|| f64::from(d));
                let t = twin_rate[az][r];
                let dc = class[az][r];
                let tc = twin_class[az][r];
                let matched = dc.is_finite() && tc.is_some_and(|c| (c - f64::from(dc)).abs() < 0.5);
                tally.add(d, t, matched);
            }
        }
        tally
    }

    /// Where a residual sits: per hydrometeor class, the wet gates and the
    /// two sides' mean rate, plus the overall ratio quartiles. Printed for
    /// the primary run only — it explains a miss, it never decides one.
    fn breakdown(
        rate: &[Vec<f32>],
        class: &[Vec<f32>],
        twin_rate: &[Vec<Option<f64>>],
        twin_class: &[Vec<Option<f64>>],
    ) -> String {
        use std::collections::BTreeMap;
        let mut per_class: BTreeMap<i32, (usize, f64, f64, usize)> = BTreeMap::new();
        let mut ratios: Vec<f64> = Vec::new();
        for az in 0..360 {
            for r in 0..RANGE_BINS {
                let d = rate[az][r];
                let (Some(t), true) = (twin_rate[az][r], d.is_finite()) else {
                    continue;
                };
                let d = f64::from(d);
                if !policy::is_wet(Some(d), Some(t)) {
                    continue;
                }
                let matched = class[az][r].is_finite()
                    && twin_class[az][r].is_some_and(|c| (c - f64::from(class[az][r])).abs() < 0.5);
                let key = if matched {
                    class[az][r] as i32
                } else {
                    -1 // classes disagree
                };
                let e = per_class.entry(key).or_insert((0, 0.0, 0.0, 0));
                e.0 += 1;
                e.1 += d;
                e.2 += t;
                e.3 += usize::from((d - t).abs() <= policy::primary_tolerance(t));
                if t > 0.0 {
                    ratios.push(d / t);
                }
            }
        }
        ratios.sort_by(f64::total_cmp);
        let q = |f: f64| -> f64 {
            if ratios.is_empty() {
                return f64::NAN;
            }
            ratios[((ratios.len() - 1) as f64 * f) as usize]
        };
        let per = per_class
            .into_iter()
            .map(|(c, (n, sd, st, ok))| {
                format!(
                    "{}:{n}({:.3}/{:.3};{:.0}%)",
                    if c < 0 {
                        "mixed".to_string()
                    } else {
                        c.to_string()
                    },
                    sd / n as f64,
                    st / n as f64,
                    100.0 * ok as f64 / n as f64,
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "class:n(ours/theirs) {per} | derived/twin ratio p10 {:.3} p25 {:.3} p50 {:.3} \
             p75 {:.3} p90 {:.3}",
            q(0.10),
            q(0.25),
            q(0.50),
            q(0.75),
            q(0.90),
        )
    }

    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_derived_dpr_matches_the_rpgs_own_product() {
        crate::tls::init();
        let sources = DataSources::production();

        let mut asserted_sites = 0usize;
        let mut pooled_wet = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for (site, when) in site_hours() {
            let site = site.as_str();
            // A throttled bucket listing drops a whole UTC day and leaves
            // the "nearest" volume hours away — a silently wrong survey.
            // A named site-hour must land on its own hour, so retry the
            // listing before giving up on one.
            let named = std::env::var("DPR_SITE_HOURS").is_ok();
            let mut found = None;
            for attempt in 0..4 {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
                let Some((file, l2_start)) = live::l2_archive_near(site, when).await else {
                    continue;
                };
                if !named || (l2_start - when).num_minutes().abs() <= 60 {
                    found = Some((file, l2_start));
                    break;
                }
                println!(
                    "{site}: retrying — nearest archived volume was {l2_start}, {} min from \
                     the requested {when} (throttled bucket listing?)",
                    (l2_start - when).num_minutes(),
                );
            }
            let Some((file, l2_start)) = found else {
                println!("{site}: SKIP — no archived Level II volume found near {when}");
                continue;
            };
            let mut params = crate::kdp::KdpParams::from_archive(&file);
            let Ok(scan) = file.scan() else {
                println!("{site}: SKIP — volume failed to decode");
                continue;
            };
            params.isdp_est_deg = crate::kdp::estimate_volume_isdp(&scan);

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
                .unwrap_or(crate::hca::DEFAULT_HEIGHT_0_KM_MSL);
            let hsda = match &env {
                Some(e) => {
                    HsdaHeights::from_env_heights(e.h0c_km_msl, e.hm20c_km_msl, radar_km_msl)
                }
                None => HsdaHeights::operational_defaults(radar_km_msl),
            };
            let default_top_arl = (h0c - radar_km_msl).max(0.0);
            let ml_flat = MeltingLayer::from_zero_c_height(h0c, radar_km_msl);

            let all_sweeps: Vec<&[Radial]> = scan.sweeps().iter().map(|s| s.radials()).collect();
            let dp_sweeps: Vec<&[Radial]> = all_sweeps
                .iter()
                .copied()
                .filter(|radials| {
                    radials
                        .first()
                        .map(|r| r.differential_phase().is_some())
                        .unwrap_or(false)
                })
                .collect();
            let cappi = crate::hca::build_refl_cappi(&dp_sweeps);
            let ml_sweeps: Vec<&[Radial]> = dp_sweeps
                .iter()
                .copied()
                .filter(|radials| {
                    radials
                        .first()
                        .map(|r| (4.0..=10.0).contains(&f64::from(r.elevation_angle_degrees())))
                        .unwrap_or(false)
                })
                .collect();
            let ml_radar = crate::hca::detect_melting_layer(
                &ml_sweeps,
                &params,
                default_top_arl,
                &hsda,
                Some(&cappi),
            );

            let tilts = crate::hhc::volume_tilts(&all_sweeps);
            if tilts.is_empty() {
                println!("{site}: SKIP — no sweep carries differential phase");
                continue;
            }

            let Some(twin) = live::latest_l3_twin(&sources, site, "DPR", l2_start).await else {
                println!("{site}: SKIP — no DPR twin names volume {l2_start}");
                continue;
            };
            if twin.message.pdb.product_code != 176 {
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
            // The encoding is verified against the live product before any
            // number is trusted: `buildDPR.c` writes thousandths of an inch
            // per hour, so the packet-28 XDR pair must read scale 1000 /
            // offset 0.
            let (scale, offset) = match &codec {
                compare::ValueCodec::Scaled { scale, offset } => (*scale, *offset),
                compare::ValueCodec::Lut(_) => {
                    println!("{site}: SKIP — twin selected a LUT codec");
                    continue;
                }
            };
            println!(
                "{site}: twin {} | XDR scale {scale} offset {offset} | packet scale_factor {} \
                 → gate {:.3} km | radials {} × {} gates | PDB scale {} offset {} | dep 30-32 {:?}",
                twin.stamp.key,
                packet.scale_factor,
                compare::gate_km(&twin.message.pdb, packet),
                packet.radials.len(),
                packet.num_range_bins,
                twin.message.pdb.data_scale(),
                twin.message.pdb.data_offset(),
                &twin.message.pdb.product_specific_47_53[..3],
            );
            assert!(
                (f64::from(scale) - DPR_SCALE).abs() < 1e-6 && offset == 0.0,
                "{site}: DPR encodes at scale {scale} / offset {offset}, not the \
                 buildDPR.c INC_SCAL {DPR_SCALE} / 0 this module assumes",
            );

            let Some(twin_rate) = twin_values(&twin.message) else {
                println!("{site}: SKIP — the DPR twin does not resample");
                continue;
            };

            // Geometry and residual structure, on demand. This is the
            // diagnostic that found the two alignment facts the module doc
            // records — that the derived grid starts at the dual-pol
            // moment's own first gate (2.125 km at most sites, not 0.125),
            // and that the packet-28 decoder used to place the twin a
            // quarter-gate out — and the split that identified α as the
            // dominant residual. Reproducers want it; nothing asserts on it.
            if std::env::var("DPR_DEBUG_ALIGN").is_ok() {
                let (d, _) = compute_dpr_impl(
                    &tilts,
                    &params,
                    &ml_radar,
                    &hsda,
                    Some(&cappi),
                    DprOptions::primary(),
                )
                .expect("computes");
                println!(
                    "{site}: geometry — derived first gate {:.4} km / {:.4} km bins; twin \
                     first_range_bin {} num_range_bins {} radial[0] start {} delta {}",
                    d.first_gate_km,
                    d.gate_interval_km,
                    packet.first_range_bin,
                    packet.num_range_bins,
                    packet.radials[0].start_angle,
                    packet.radials[0].angle_delta,
                );
                // Sub-gate range alignment: agreement must peak at zero.
                for shift in [-0.5f64, -0.25, 0.0, 0.25, 0.5] {
                    let g = crate::dpprep::resample_to_polar_grid(
                        &d.values,
                        &(0..360).map(|a| a as f64 + 0.5).collect::<Vec<_>>(),
                        d.first_gate_km + shift,
                        d.gate_interval_km,
                        1.0,
                    );
                    let (mut n, mut ok) = (0usize, 0usize);
                    for az in 0..360 {
                        for r in 0..RANGE_BINS {
                            let Some(t) = twin_rate[az][r] else { continue };
                            let o = g[az][r];
                            if !o.is_finite() || t <= policy::WET_GATE_IN_PER_HR {
                                continue;
                            }
                            n += 1;
                            ok += usize::from(
                                (f64::from(o) - t).abs() <= policy::primary_tolerance(t),
                            );
                        }
                    }
                    println!(
                        "{site}: SUBGATE shift {shift:+.2} km → {:.2}% of {n}",
                        100.0 * ok as f64 / n.max(1) as f64,
                    );
                }
                // Per-radial versus within-radial residual: a PIA/α error
                // moves a whole radial together, a field difference does not.
                let ours = d.to_polar_grid();
                let mut per_radial: Vec<f64> = Vec::new();
                let mut within: Vec<f64> = Vec::new();
                for az in 0..360usize {
                    let (mut so, mut st) = (0.0, 0.0);
                    let mut local: Vec<f64> = Vec::new();
                    for r in 0..RANGE_BINS {
                        let o = ours[az][r];
                        let Some(t) = twin_rate[az][r] else { continue };
                        if !o.is_finite() || t <= policy::WET_GATE_IN_PER_HR {
                            continue;
                        }
                        so += f64::from(o);
                        st += t;
                        local.push(f64::from(o) / t);
                    }
                    if local.len() >= 20 && st > 0.0 {
                        let m = so / st;
                        per_radial.push(m);
                        within.extend(local.iter().map(|v| v / m));
                    }
                }
                let pct = |v: &mut Vec<f64>, f: f64| {
                    v.sort_by(f64::total_cmp);
                    if v.is_empty() {
                        f64::NAN
                    } else {
                        v[((v.len() - 1) as f64 * f) as usize]
                    }
                };
                let (mut rr, mut wi) = (per_radial.clone(), within.clone());
                println!(
                    "{site}: residual — per-radial ratio n{} p10 {:.3} p50 {:.3} p90 {:.3} | \
                     within-radial p10 {:.3} p50 {:.3} p90 {:.3}",
                    per_radial.len(),
                    pct(&mut rr, 0.10),
                    pct(&mut rr, 0.50),
                    pct(&mut rr, 0.90),
                    pct(&mut wi, 0.10),
                    pct(&mut wi, 0.50),
                    pct(&mut wi, 0.90),
                );
            }

            // The class mask: the same volume's HHC. Without it the primary
            // bar cannot be formed, so the volume is measured unrestricted
            // and printed, never asserted.
            let hhc_twin = live::latest_l3_twin(&sources, site, "HHC", l2_start).await;
            let twin_class = match hhc_twin
                .as_ref()
                .filter(|p| p.message.pdb.product_code == 177)
            {
                Some(p) => twin_values(&p.message),
                None => None,
            };

            // The bounded A/B matrix: documented conventions only, plus one
            // labelled diagnostic.
            let ab: [(&str, &MeltingLayer, DprOptions); 5] = [
                (
                    "alpha per-volume/vprc ON-B/radar-mlda",
                    &ml_radar,
                    DprOptions::primary(),
                ),
                (
                    "alpha cold-start/vprc ON-B/radar-mlda",
                    &ml_radar,
                    DprOptions {
                        alpha: RofaAlpha::Default,
                        ..DprOptions::primary()
                    },
                ),
                (
                    "alpha per-volume/vprc off /radar-mlda",
                    &ml_radar,
                    DprOptions {
                        vprc: Vprc::Off,
                        ..DprOptions::primary()
                    },
                ),
                (
                    "alpha per-volume/vprc ON-B/flat-0c-ml",
                    &ml_flat,
                    DprOptions::primary(),
                ),
                (
                    "R(A) OFF  (diagnostic, not a candidate)",
                    &ml_radar,
                    DprOptions {
                        rofa: false,
                        ..DprOptions::primary()
                    },
                ),
            ];

            let mut primary: Option<policy::RateTally> = None;
            for (label, ml, opts) in ab {
                let Some((derived, diag)) =
                    compute_dpr_impl(&tilts, &params, ml, &hsda, Some(&cappi), opts)
                else {
                    continue;
                };
                let rate = derived.to_polar_grid();
                let class = derived.classes_to_polar_grid();
                let empty: Vec<Vec<Option<f64>>> = vec![vec![None; RANGE_BINS]; 360];
                let tally = score(
                    &rate,
                    &class,
                    &twin_rate,
                    twin_class.as_ref().unwrap_or(&empty),
                );

                if primary.is_none() && !policy::volume_is_scoreable(tally.wet) {
                    println!(
                        "{site}: SKIP — {} wet gates < {} (vol {l2_start}, VCP {})",
                        tally.wet,
                        policy::MIN_WET_GATES,
                        twin.message.pdb.vcp,
                    );
                    break;
                }

                let tag = if primary.is_none() {
                    "PRIMARY"
                } else {
                    "     ab"
                };
                println!(
                    "{site}: {tag} {label:38} | wet {} (matched {}) | primary {:.2}% \
                     unrestricted {:.2}% | secondary {:.2}% | bias {:+.3} mae {:.3} in/hr | \
                     presence {} | alpha {:.4} (volume est {:?}) R(A) bins {} radials {}/{} | \
                     filled {:.1}% top {:.1}°",
                    tally.wet,
                    tally.wet_class_matched,
                    tally.primary_pct(),
                    tally.tight_unrestricted_pct(),
                    tally.secondary_pct(),
                    tally.bias_in_per_hr(),
                    tally.mean_abs_in_per_hr(),
                    tally.presence_disagreements,
                    diag.alpha,
                    diag.alpha_volume,
                    diag.ra_bins,
                    diag.ra_radials,
                    diag.radials,
                    derived.pct_filled,
                    derived.highest_elev_deg,
                );
                {
                    println!(
                        "{site}:   {}",
                        breakdown(
                            &rate,
                            &class,
                            &twin_rate,
                            twin_class.as_ref().unwrap_or(&empty),
                        ),
                    );
                }
                if primary.is_none() {
                    primary = Some(tally);
                }
            }
            let Some(tally) = primary else {
                continue;
            };
            if !policy::volume_is_scoreable(tally.wet) {
                continue;
            }
            println!(
                "{site}: vol {l2_start} VCP {} | HHC mask {}",
                twin.message.pdb.vcp,
                if twin_class.is_some() {
                    "present"
                } else {
                    "ABSENT — primary not asserted"
                },
            );
            if twin_class.is_none() {
                println!("{site}: measured but the class mask is missing — not asserted");
                continue;
            }
            if tally.wet_class_matched == 0 {
                println!("{site}: measured but no class-matched wet gate — not asserted");
                continue;
            }

            let primary_pct = tally.primary_pct();
            let secondary_pct = tally.secondary_pct();
            if !policy::meets_primary(primary_pct) {
                failures.push(format!(
                    "{site} ({l2_start}): primary {primary_pct:.2}% (bar {}) on {} \
                     class-matched wet gates",
                    policy::PRIMARY_PCT,
                    tally.wet_class_matched,
                ));
            }
            if !policy::meets_secondary(secondary_pct) {
                failures.push(format!(
                    "{site} ({l2_start}): secondary {secondary_pct:.2}% (bar {}) on {} wet gates",
                    policy::SECONDARY_PCT,
                    tally.wet,
                ));
            }
            asserted_sites += 1;
            pooled_wet += tally.wet;
        }

        println!(
            "asserted {asserted_sites} sites, {pooled_wet} wet gates pooled; failures: {}",
            failures.len(),
        );
        assert!(
            failures.is_empty(),
            "sites under a rate bar:\n  {}",
            failures.join("\n  "),
        );
        assert!(
            policy::sample_is_conclusive(asserted_sites, pooled_wet),
            "inconclusive run: {asserted_sites} sites / {pooled_wet} wet gates asserted",
        );
    }

    /// Scan the recent archive for precipitating site-hours, the way the
    /// classification campaign's scan does: count gates at or above 35 dBZ
    /// in the lowest cut of the newest volume at each site. Prints a table
    /// to paste into `DPR_SITE_HOURS`; asserts nothing.
    ///
    /// ```text
    /// cargo test -p rustdar-radar --release --lib -- --ignored --nocapture live_dpr_precip_site_scan
    /// ```
    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_dpr_precip_site_scan() {
        crate::tls::init();
        let hours: Vec<i64> = std::env::var("DPR_SCAN_HOURS")
            .ok()
            .map(|s| {
                s.split(',')
                    .filter_map(|h| h.trim().parse().ok())
                    .collect::<Vec<i64>>()
            })
            .unwrap_or_else(|| vec![0]);
        let now = chrono::Utc::now().naive_utc();
        for back in hours {
            let when = now - chrono::Duration::hours(back);
            for &site in live::SITES {
                let Some((file, start)) = live::l2_archive_near(site, when).await else {
                    continue;
                };
                let Ok(scan) = file.scan() else { continue };
                let Some(sweep) = scan.sweeps().first() else {
                    continue;
                };
                let mut wet = 0usize;
                let mut gates = 0usize;
                for radial in sweep.radials() {
                    if let Some(m) = radial.reflectivity() {
                        for v in m.values() {
                            gates += 1;
                            if let nexrad_model::data::MomentValue::Value(z) = v
                                && z >= 35.0
                            {
                                wet += 1;
                            }
                        }
                    }
                }
                println!(
                    "{site}={} | {wet:6} gates >= 35 dBZ of {gates:7} | dual-pol tilts {}",
                    start.format("%Y-%m-%dT%H:%M"),
                    scan.sweeps()
                        .iter()
                        .filter(|s| s
                            .radials()
                            .first()
                            .map(|r| r.differential_phase().is_some())
                            .unwrap_or(false))
                        .count(),
                );
            }
        }
    }
}
