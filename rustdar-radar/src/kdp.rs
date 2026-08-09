//! Specific Differential Phase (the RPG's product 163, AWIPS `N0K`) computed
//! locally from the Level II dual-pol moments of one tilt.
//!
//! # What is implemented, and from which documents
//!
//! Unlike the reflectivity-derived volume products (EET, DVL — whose RPG
//! tasks sit behind the closed `cpc014` DQA family, see [`crate::eet`] and
//! [`crate::vil`]), the WSR-88D **Dual-Polarization Preprocessor** is fully
//! public: task `cpc004/tsk011` (`dpprep`) ships complete C source in the
//! CODE distribution (mirrored at github `likev/CodeOrpgPub`). Everything
//! below is transcribed from that source, function for function, with the
//! fleet-default adaptation values from `cpc104/lib006/dpprep.alg`. The
//! algorithm lineage is Ryzhkov/Zrnić (NSSL) via Istok et al. 2009, "WSR-88D
//! dual polarization initial operational capabilities" (AMS 25th IIPS).
//!
//! **Chain** (`cpc104/lib003/task_attr_table`): super-res base data →
//! `a_recomb` (azimuth recombination, `cpc004/tsk009`) → `dpprep` → HCA →
//! `dualpol8bit` (`cpc024/tsk001`, products 159/161/163). So the RPG's KDP is
//! computed at **1° × 0.25 km**, after the two half-degree radials of each
//! degree are recombined.
//!
//! **Azimuth recombination** — `recomb_dp_fields.c` (`RCDP_dp_recomb` /
//! `Recomb_dp_data`): consecutive half-degree radials pair when the first
//! sits in the first half of the degree and the two are within 0.75°. Per
//! gate the DP fields are combined **coherently**: with `Ph = 10^(Z/10)`
//! (the site/range calibration factors cancel between the two radials —
//! they share the range gate) and `Pv = Ph / 10^(ZDR/10)`, each radial forms
//! the correlation vector `t·e^{-iφ}` with `t = ρ·√(Ph·Pv)`, the vectors and
//! powers are averaged (single-radial fallback per field when one side is
//! missing), and
//!
//! ```text
//! φc = −atan2(Im, Re)  (+360 if negative)      ρc = √((Re²+Im²)/(Ph·Pv))
//! ```
//!
//! A gate whose reflectivity is missing on both radials has no vector, so
//! the recombined PhiDP there is missing — the RDA's own SNR censoring of Z
//! carries into the DP fields, exactly as the source does it. Reflectivity
//! itself recombines as the linear-power mean (`Combine_azi`), velocity and
//! spectrum width feed only the high-attenuation test and are pair-averaged
//! here (the source's `Combine_dops` is power-weighted; the difference is
//! confined to that yes/no radial test, and the split-cut surveillance
//! radials `N0K` is built from carry no Doppler moments at all).
//!
//! **PhiDP unfolding** — `Unfold_PhiDP` (`dpp_process.c`), constants from
//! the source: fold 360°, historical median over the previous 30 gates whose
//! RhoHV ≥ 0.85 (needs > 25 of them, standard deviation < 120°), unfolding
//! allowed only past gate 240 (60 km) with > 15 valid gates accumulated;
//! when `|φ − median| ≥ 180°` the closer of `φ+360` / `φ+720` wins. The seed
//! median is the **initial system differential phase** (`init_fdp`).
//!
//! **Initial system PhiDP** — the RPG reads it from the radial header
//! (`bh->sys_diff_phase`, the RDA volume data block's "Initial System
//! Differential Phase"); the `isdp_apply` adjustment defaults to `NO`
//! (`dpprep.alg`) and is not applied. [`KdpParams::from_archive`] extracts
//! the RDA value from the Level II file. When it is unavailable (the render
//! path holds a decoded `Scan`, which does not carry it), the documented
//! estimator (`calc_system_PhiDP.c`, Krause/Klein) stands in: per radial the
//! first run of 11 consecutive gates past 25 km with RhoHV ≥ 0.986, Z ≥ 0
//! and none ≥ 40 dBZ yields a 360°-aware median; with ≥ 40 such radials the
//! estimate is the 5th-percentile entry (`NINT(n/20)`) of the sorted queue.
//!
//! **Gate quality / censoring** — `DPPP_process_data`: a gate is
//! *meteorological* when its 5-gate-averaged RhoHV ≥ 0.9 (`corr_thresh`)
//! and its PhiDP is present; on a **high-attenuation radial** (≥ 10 gates
//! past bin 180 with Z in [30, 50] dBZ, |V| ≥ 1 m/s, RhoHV ≤ 0.8, SW >
//! 2 m/s — `Is_high_atten_radial` with `dpprep.alg` values) the flag runs on
//! SNR ≥ 5 dB instead, from the 3-gate-smoothed Z and the radial header's
//! `dBZ0`/atmos. The final KDP is censored wherever that same smoothed
//! RhoHV is below 0.9, and the product marks a gate wherever the
//! (recombined) PhiDP input itself is missing (`Add_moment` keys the output
//! level on the *input* φ).
//!
//! **Smoothing and interpolation** — the unfolded φ is 5-gate median
//! filtered, censored to the meteorological gates, then run through **two**
//! chains: a 9-gate running average (`short_gate`) and a 25-gate one
//! (`long_gate`), each followed by `Interpolate`: gaps between valid
//! meteorological groups (size ≥ the window) are bridged linearly between
//! the smoothed values at `end−w/2` and `start+w/2`, the stretch before the
//! first group ramps from `init_fdp`, and the stretch after the last group
//! holds constant — which flattens the last `w/2` gates of the final group.
//!
//! **KDP** — `Calculate_kdp`/`Calculate_lls_kdp`: per gate, the
//! least-squares slope of the interpolated φ over the window (9 or 25
//! gates, shrunk at the radial ends), **halved** for the two-way phase
//! path: `factor = 6/(g·m(m²−1))` is exactly `½ · 12/(g·m(m²−1))`. The
//! short-gate estimate is kept where the attenuation-corrected smoothed
//! reflectivity exceeds 40 dBZ (`dbz_thresh`; `z_prcd = Z̄₃ +
//! 0.04·(φ_long − init_fdp)` per `Create_corrected_fields_and_adjust_kdp`),
//! the long-gate estimate everywhere else — including gates with no
//! reflectivity, whose `z_prcd` is NO-DATA and compares low. The RPG's
//! noise correction of ρ (`RPG_NOISE_CORRECTION`) is compiled out in the
//! released build (`LOCAL_DEFINES` empty in `dpprep.mak`), so the censor
//! runs on the plain smoothed ρ, as here.
//!
//! **Encoding** — `dualpol8bit.c`/`.h`: KDP is capped at 10.0 °/km
//! (`MAX_KDP_DISPLAY`), floored at −2.05 (the 16-bit intermediate's minimum
//! data level 2 through `Get_new_scale(20, 43, …)` preserves that physical
//! value), and product 163 encodes `level = round(kdp·20 + 43)` —
//! `KDP_ICD_SCALE` 20, `KDP_ICD_OFFSET` 43, maximum level 243, levels 0/1
//! below-threshold/range-folded. One data level is 0.05 °/km. The live
//! harness verifies the scale/offset a real N0K PDB declares instead of
//! trusting this transcription.
//!
//! # Documented gaps against the RPG
//!
//! * **Doppler recombination** for the high-attenuation test is a plain
//!   pair mean, not `Combine_dops`' power-weighted average. The test is a
//!   per-radial yes/no with a 10-gate margin, and the surveillance cuts the
//!   low-tilt products come from carry no velocity, making it moot there.
//! * **`dBZ0`/atmos** for the SNR path come from the volume/elevation data
//!   blocks when [`KdpParams::from_archive`] is used; the plain render path
//!   lacks them and falls back to the RhoHV flag on high-attenuation
//!   radials.
//! * The RPG computes in `float`; this module computes in `f64`. The
//!   difference is orders of magnitude below the 0.05 °/km data level.
//!
//! # Validation status — read before trusting the twin harness to pass
//!
//! **The live twin harness and its `validation_policy` now live on
//! branch `campaign-harness`.** The figures below are the last measured
//! before the move; re-measuring means that branch.
//!
//! The live harness scores the derivation against the RPG's own N0K for
//! the **same volume and cut** (paired by PDB volume start plus elevation
//! number, angle-matched where a site's product cut numbering differs from
//! the RDA's), in the twin's own data levels — the PDB's declared scale 20
//! / offset 43 was verified on every live twin, so ±1 level is ±0.05 °/km
//! and the double bar (≥ 90% within ±1 **and** ≥ 98% within ±2, per site,
//! never pooled) is ±0.10 °/km at the ±2 leg.
//!
//! Three full-roster surveys on 2026-07-28 (fresh volumes in each) do
//! **not** meet that bar: 7 of 22 sites pass (weak/stratiform fields —
//! KSHV read 100.00% exact on 3,383 compared gates, KABR 98.05%, KMVX
//! 97.63%: where the field is quiet the transcription is gate-exact), and
//! 15 miss, worst where the weather is (KMRX 63.7/80.5, KSGF 76.3/86.3,
//! KSFX 72.9/86.9). Presence disagreement is 1.5–15% everywhere — RhoHV
//! censoring, unlike the reflectivity products' DQA wall, is reproducible.
//! What the surveys established, each A/B scored on the tuning sites
//! (KTLX, KMLB, KMTX, KMPX, KSHV) and confirmed on holdouts (KFSD, KMRX,
//! KTLH, KDDC, KAMA) that played no part in the choice:
//!
//! * **Coherent recombination** wins everywhere, both sets, every survey:
//!   against the plain pair mean it is 1–7 points better on levels and
//!   3–15× better on presence (2–7% against 14–35%); against no
//!   recombination (super-res passthrough) the gap is wider still. The
//!   documented `Recomb_dp_data` average is the primary, uncontradicted.
//! * **The attenuation term in the window switch** (`delta_z`) is inert:
//!   identical scores to two decimals at every site. Kept, per the source.
//! * **Initial system phase** is the residual's first component. Every
//!   RDA header on the roster declares the default 60.0°, but the twins
//!   behave like the `isdp_apply` branch is live in the fleet: the misses
//!   concentrate in a **one-sided +1-level shoulder** (KEAX +1: 18.2%
//!   against −1: 0.7%; KTLH 16.8/0.8) — the exact signature of our
//!   leading-edge ramps climbing from 60° to the data's true system phase
//!   while the twin's sit flat. Where the single-volume estimator
//!   concludes, applying it (`isdp-applied`, the source's `isdp_est !=
//!   -99` semantics) recovers 10–14 points of within-±1 (KMRX 63.7 →
//!   74.9, KSFX 72.9 → 83.5) and never loses; but it concludes only in
//!   broad rain (the documented gates: 11 consecutive ρ ≥ 0.986 gates
//!   past 25 km with nothing ≥ 40 dBZ, ≥ 40 radials), while the RPG
//!   **persists** its estimate across volumes in `DP_ISDP_EST` — state a
//!   single archived volume cannot reproduce. On the tuning set the two
//!   variants tie (the estimator concludes at none of the five), so the
//!   documented `isdp_apply = NO` default stays primary and the finding
//!   is recorded here instead of tuned around.
//! * The rest of the residual is weak-band jitter around gradients
//!   (symmetric ±1–3-level spread at KSGF/KMTX/KLZK, near-zero mean
//!   bias): the censor and the meteorological grouping both hinge on
//!   `rho_smd ≥ 0.9` at gates where smoothed ρ sits within rounding of
//!   the threshold, and one flipped gate moves a whole interpolation
//!   bridge. `corr_thresh` itself is URC-adaptable per site ([0.5, 1.0]),
//!   like the ISDP store — operational state the archive stream does not
//!   carry. Per the campaign's early-stop rule nothing undocumented was
//!   chased.
//!
//! Product 163 therefore **stays a Level III fetch**; this module ships as
//! the documented local derivation with the render path wired
//! ([`crate::render::render_derived_kdp_to_image`], the `kdp` arm of the
//! `render_product.rs` example on branch `campaign-harness`) and that
//! provenance recorded.
//!
//! # Build 21 note (2026-07 cross-check)
//!
//! The CODE Build 21.0r1.7 source confirms every constant above unchanged,
//! but B21's `dpprep.alg` defaults `metsignal_processing = ON` (CCR
//! NA14-00100): the fleet's meteorological flag and unfold filter come from
//! the fuzzy met signal, not `rho_smd ≥ 0.9` — see [`crate::dpprep`]'s
//! module doc. That machinery is implemented and is the HCA chain's
//! primary; **this module's pipeline keeps the legacy flag its survey
//! record above was measured with** (three full-roster surveys; the product
//! ships as a fetch either way), so the figures and the code stay one
//! thing. The B21-new `ra_gate` φ chain (`DPRA`, window 7) and `DPIN` feed
//! DP QPE/CDA, not KDP.

#[cfg(test)]
use crate::dpprep::coherent_phi_rho;
use crate::dpprep::{
    CORR_THRESH, CombinedRadial, DBZ_THRESH, DBZ_WINDOW, DpInput, ISDP_MAX_QUEUE, LONG_GATE,
    MD_SNR_THRESH, SHORT_GATE, UNFOLD_MIN_RHO, WINDOW, average_filter, combine_sweep,
    estimate_isdp, index_into, interpolate, is_high_attenuation_radial, isdp_from_queue,
    kdp_from_phi, median_filter, meteo_groups, radial_system_phi, unfold_phidp,
};
use crate::volumetric::RANGE_BINS;
use nexrad_model::data::Radial;

/// ICD product-163 encoding, `dualpol8bit.h`: `level = kdp·20 + 43`.
pub const KDP_ICD_SCALE: f32 = 20.0;
pub const KDP_ICD_OFFSET: f32 = 43.0;

/// `dualpol8bit.c`'s `MAX_KDP_DISPLAY`: the product caps KDP at 10 °/km.
pub const KDP_MAX_DISPLAY: f32 = 10.0;

/// The product's floor: the 16-bit intermediate moment's minimum data level
/// 2 decodes to exactly `(2 − 43)/20` (`Get_new_scale` preserves it), so no
/// encoded KDP sits below −2.05 °/km.
pub const KDP_MIN_DISPLAY: f32 = -2.05;

/// Radial-header parameters the RPG reads that a decoded
/// [`Scan`](nexrad_model::data::Scan) does not carry.
///
/// [`from_archive`](Self::from_archive) extracts them from the Level II
/// file's message 31 blocks — the same fields `dpp_format.c` reads off the
/// base data header. All optional: a missing initial phase falls back to the
/// documented data estimator, and missing `dBZ0`/atmos disable only the
/// high-attenuation SNR flag (see the module doc's gap list).
#[derive(Debug, Clone, Copy, Default)]
pub struct KdpParams {
    /// The RDA's initial system differential phase, degrees
    /// (`bh->sys_diff_phase`).
    pub init_fdp_deg: Option<f32>,
    /// `bh->calib_const`: the system reflectivity calibration `dBZ0`, dB.
    pub dbz0: Option<f32>,
    /// `bh->atmos_atten` scaled: atmospheric attenuation, dB/km (negative).
    pub atmos_db_per_km: Option<f32>,
    /// The volume-scope system-phase estimate ([`estimate_volume_isdp`]),
    /// the analog of the RPG's `DP_ISDP_EST` store that the `isdp_apply`
    /// branch consumes. Optional: without it the applied variant estimates
    /// from the sweep alone before falling back to the RDA value.
    pub isdp_est_deg: Option<f32>,
}

impl KdpParams {
    /// The render path's stand-in when only a decoded `Scan` is in hand
    /// (the model drops the radial-header blocks): fleet-typical values
    /// for the two parameters the classification cannot run without.
    /// `dbz0` −43.5 dB sits mid-range of the RDA calibration constants the
    /// 2026-07 precipitation survey read from archives (−41.0…−45.9);
    /// `atmos` −0.012 dB/km was the value at every site surveyed. The
    /// initial phase stays `None` — the documented estimator resolves it
    /// from the data. A ±2 dB `dbz0` error moves only the no-echo boundary
    /// at the SNR-5 dB fringe; the twin-validated paths always read the
    /// real values via [`from_archive`](Self::from_archive).
    pub fn render_fallback() -> Self {
        Self {
            init_fdp_deg: None,
            dbz0: Some(-43.5),
            atmos_db_per_km: Some(-0.012),
            isdp_est_deg: None,
        }
    }

    /// Read the RDA parameters from a raw Level II archive file: the first
    /// digital-radar-data message's volume block (initial system PhiDP,
    /// calibration constant) and elevation block (atmos).
    pub fn from_archive(file: &nexrad_data::volume::File) -> Self {
        use nexrad_decode::messages::MessageContents;
        let mut p = Self::default();
        let Ok(records) = file.records() else {
            return p;
        };
        for record in records {
            let record = if record.compressed() {
                match record.decompress() {
                    Ok(r) => r,
                    Err(_) => continue,
                }
            } else {
                record
            };
            let Ok(messages) = record.messages() else {
                continue;
            };
            for message in messages {
                if let MessageContents::DigitalRadarData(m) = message.contents() {
                    if p.init_fdp_deg.is_none()
                        && let Some(vol) = m.volume_data_block()
                    {
                        p.init_fdp_deg = Some(vol.initial_system_differential_phase_raw());
                        p.dbz0 = Some(vol.calibration_constant());
                    }
                    if p.atmos_db_per_km.is_none()
                        && let Some(el) = m.elevation_data_block()
                    {
                        p.atmos_db_per_km = Some(el.atmos());
                    }
                }
                if p.init_fdp_deg.is_some() && p.atmos_db_per_km.is_some() {
                    return p;
                }
            }
        }
        p
    }
}

/// How the two half-degree radials of a degree become one — an A/B knob of
/// the harness. The primary is the source's coherent average.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recomb {
    /// `Recomb_dp_data`'s power-weighted complex average (the primary).
    Coherent,
    /// Plain per-gate arithmetic mean of φ and ρ — the naive reading, kept
    /// to measure what the coherent average is worth.
    #[cfg_attr(not(test), allow(dead_code))]
    PlainMean,
    /// No recombination: every super-res radial processed on its own, the
    /// grid sampling whichever covers the cell. What `recomb` does with
    /// non-half-degree input ("recombination not needed").
    #[cfg_attr(not(test), allow(dead_code))]
    SuperRes,
}

/// Where `init_fdp` comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IsdpSource {
    /// [`KdpParams::init_fdp_deg`] when present (the RDA header value,
    /// `dpprep.alg`'s `isdp_apply = NO` default), else the documented
    /// estimator.
    Provided,
    /// The `isdp_apply = YES` reading: the documented estimator when it
    /// concludes (adjusting φ by `est − init` is equivalent to seeding the
    /// pipeline with the estimate), falling back to the RDA value exactly
    /// as the source's `isdp_est != -99` guard does.
    #[cfg_attr(not(test), allow(dead_code))]
    Estimated,
}

/// The conventions [`compute_kdp`] pins; the harness varies them.
#[derive(Debug, Clone, Copy)]
struct KdpOptions {
    recomb: Recomb,
    isdp: IsdpSource,
    /// Apply the `0.04·(φ_long − init_fdp)` attenuation correction inside
    /// the 40 dBZ window switch (`z_prcd`), per the source. Off is the A/B
    /// variant that removes `init_fdp` from the switch entirely.
    attenuation_in_switch: bool,
}

impl KdpOptions {
    const fn primary() -> Self {
        Self {
            recomb: Recomb::Coherent,
            isdp: IsdpSource::Provided,
            attenuation_in_switch: true,
        }
    }
}

/// The derived KDP field for one tilt, at the recombined radials' native
/// geometry (1° × 0.25 km from super-res input).
pub struct DerivedKdp {
    /// `[radial][gate]`, °/km in `[KDP_MIN_DISPLAY, KDP_MAX_DISPLAY]`,
    /// `NaN` censored/undefined.
    pub values: Vec<Vec<f32>>,
    /// Centre azimuth per radial, degrees.
    pub azimuths_deg: Vec<f64>,
    /// Range to the **centre** of gate 0, km.
    pub first_gate_km: f64,
    pub gate_interval_km: f64,
    /// Angular width of one radial, degrees: 1.0 after recombination, the
    /// input spacing for a passthrough sweep. What bounds a radial's claim
    /// on the comparison grid.
    pub radial_width_deg: f64,
    /// The initial system phase actually used, for the record.
    pub init_fdp_deg: f64,
}

impl DerivedKdp {
    /// Resample onto the 360° × 230 km comparison grid, cell for cell the
    /// way [`crate::twin::compare::tally_packet`] resamples the Level III
    /// twin: the radial nearest the cell centre `az + 0.5°`, and per 1-km
    /// cell the gate whose centre falls nearest the cell centre, earlier
    /// gate winning ties.
    pub fn to_polar_grid(&self) -> Vec<Vec<f32>> {
        let mut grid = vec![vec![f32::NAN; RANGE_BINS]; 360];
        if self.values.is_empty() {
            return grid;
        }

        // Which gate represents each 1-km cell (geometry is shared by every
        // radial of the sweep).
        let n_gates = self.values.iter().map(Vec::len).max().unwrap_or(0);
        let mut gate_for_bin: Vec<Option<usize>> = vec![None; RANGE_BINS];
        let mut best = vec![f64::INFINITY; RANGE_BINS];
        for j in 0..n_gates {
            let centre = self.first_gate_km + j as f64 * self.gate_interval_km;
            let bin = centre.floor() as i64;
            if !(0..RANGE_BINS as i64).contains(&bin) {
                continue;
            }
            let d = (centre - (bin as f64 + 0.5)).abs();
            if d < best[bin as usize] {
                best[bin as usize] = d;
                gate_for_bin[bin as usize] = Some(j);
            }
        }

        // A radial only claims cells its own span covers (plus tenth-degree
        // slack), so an incomplete sweep leaves the uncovered sector
        // undefined instead of smearing the nearest radial across it.
        let cover = 0.5 * self.radial_width_deg + 0.05;
        for (az, row) in grid.iter_mut().enumerate() {
            let centre = az as f64 + 0.5;
            let ri = self
                .azimuths_deg
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    circular_distance(**a, centre).total_cmp(&circular_distance(**b, centre))
                })
                .filter(|(_, a)| circular_distance(**a, centre) <= cover)
                .map(|(i, _)| i);
            let Some(ri) = ri else { continue };
            let values = &self.values[ri];
            for (r, cell) in row.iter_mut().enumerate() {
                if let Some(j) = gate_for_bin[r]
                    && let Some(&v) = values.get(j)
                {
                    *cell = v;
                }
            }
        }
        grid
    }
}

fn circular_distance(a: f64, b: f64) -> f64 {
    let mut d = (a - b).rem_euclid(360.0);
    if d > 180.0 {
        d = 360.0 - d;
    }
    d
}

/// Compute the tilt's KDP per the rules in the module doc: recombine the
/// sweep's radials to 1°, unfold and smooth PhiDP, take the half least-squares
/// slope over the 9/25-gate window selected by the 40 dBZ rule, censor on
/// smoothed RhoHV, clamp to the product's display range. `None` when no
/// radial carries the differential phase moment.
pub fn compute_kdp(radials: &[Radial], params: &KdpParams) -> Option<DerivedKdp> {
    compute_kdp_impl(radials, params, KdpOptions::primary())
}

fn compute_kdp_impl(
    radials: &[Radial],
    params: &KdpParams,
    opts: KdpOptions,
) -> Option<DerivedKdp> {
    let inputs: Vec<DpInput> = radials.iter().filter_map(DpInput::from_radial).collect();
    if inputs.is_empty() {
        return None;
    }
    let combined = match opts.recomb {
        Recomb::Coherent => combine_sweep(&inputs, true),
        Recomb::PlainMean => combine_sweep(&inputs, false),
        Recomb::SuperRes => inputs.iter().map(CombinedRadial::passthrough).collect(),
    };

    let init_fdp = match opts.isdp {
        IsdpSource::Provided => params
            .init_fdp_deg
            .map(f64::from)
            .or_else(|| estimate_isdp(&combined))
            .unwrap_or(0.0),
        IsdpSource::Estimated => params
            .isdp_est_deg
            .map(f64::from)
            .or_else(|| estimate_isdp(&combined))
            .or(params.init_fdp_deg.map(f64::from))
            .unwrap_or(0.0),
    };

    let geometry = combined.iter().find(|c| !c.phi.is_empty())?;
    let first_gate_km = geometry.dr0;
    let gate_interval_km = geometry.dg;
    let radial_width_deg = if !inputs[0].half_degree || opts.recomb == Recomb::SuperRes {
        inputs[0].spacing
    } else {
        1.0
    };

    let dbz0 = params.dbz0.map(f64::from);
    let atmos = params.atmos_db_per_km.map(f64::from);

    let mut values = Vec::with_capacity(combined.len());
    let mut azimuths = Vec::with_capacity(combined.len());
    for radial in &combined {
        values.push(process_radial(radial, init_fdp, dbz0, atmos, &opts));
        azimuths.push(radial.az);
    }

    Some(DerivedKdp {
        values,
        azimuths_deg: azimuths,
        first_gate_km,
        gate_interval_km,
        radial_width_deg,
        init_fdp_deg: init_fdp,
    })
}

/// One recombined radial through the whole documented pipeline. Returns the
/// censored, display-clamped KDP per DP gate.
fn process_radial(
    radial: &CombinedRadial,
    init_fdp: f64,
    dbz0: Option<f64>,
    atmos: Option<f64>,
    opts: &KdpOptions,
) -> Vec<f32> {
    let n = radial.phi.len();
    if n == 0 {
        return Vec::new();
    }

    let mut phi = radial.phi.clone();
    // The legacy (metsignal-OFF) filter pair — the configuration this
    // module's survey record was measured with; see the module doc's B21
    // note.
    unfold_phidp(&mut phi, &radial.rho, UNFOLD_MIN_RHO, init_fdp);

    let rho_smd = average_filter(&radial.rho, WINDOW);
    let ref_smd = average_filter(&radial.z, DBZ_WINDOW);

    // The meteorological flag: SNR on a high-attenuation radial (when the
    // radial header parameters are available), smoothed RhoHV otherwise.
    let hatt = is_high_attenuation_radial(&radial.z, &radial.vel, &radial.spw, &radial.rho);
    let mut flag = vec![false; n];
    if hatt && let Some(dbz0) = dbz0 {
        let atmos = atmos.unwrap_or(0.0);
        let ngs = n.min(ref_smd.len());
        for (i, f) in flag.iter_mut().enumerate().take(ngs) {
            let r = (radial.zr0 + i as f64 * radial.zg).max(1e-9);
            let snr = ref_smd[i] - 20.0 * r.log10() + atmos * r - dbz0;
            *f = snr >= MD_SNR_THRESH && !phi[i].is_nan();
        }
    } else {
        for (i, f) in flag.iter_mut().enumerate() {
            *f = rho_smd[i] >= CORR_THRESH && !phi[i].is_nan();
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

    let kdp9 = kdp_from_phi(&phi_short, SHORT_GATE, radial.dg);
    let kdp25 = kdp_from_phi(&phi_long, LONG_GATE, radial.dg);

    // z_prcd per Z gate: smoothed Z plus the attenuation correction driven
    // by the long-gate φ (`Create_corrected_fields_and_adjust_kdp`).
    let z_prcd: Vec<f64> = (0..radial.z.len())
        .map(|iz| {
            if ref_smd[iz].is_nan() {
                return f64::NAN;
            }
            let delta = if opts.attenuation_in_switch {
                let zr = radial.zr0 + iz as f64 * radial.zg;
                match index_into(zr, radial.dr0, radial.dg, n) {
                    Some(id) if phi_long[id].is_finite() && phi_long[id] >= init_fdp => {
                        0.04 * (phi_long[id] - init_fdp)
                    }
                    _ => 0.0,
                }
            } else {
                0.0
            };
            ref_smd[iz] + delta
        })
        .collect();

    (0..n)
        .map(|i| {
            // Censor on the smoothed RhoHV (rho_prcd — the noise correction
            // is compiled out of the released RPG), and on the recombined
            // input φ, which is what the output moment's level keys on.
            if rho_smd[i].is_nan() || rho_smd[i] < CORR_THRESH || radial.phi[i].is_nan() {
                return f32::NAN;
            }
            let d = radial.dr0 + i as f64 * radial.dg;
            let z = index_into(d, radial.zr0, radial.zg, z_prcd.len())
                .map(|iz| z_prcd[iz])
                .unwrap_or(f64::NAN);
            let use_short = z.is_finite() && z > DBZ_THRESH;
            let v = if use_short { kdp9[i] } else { kdp25[i] };
            if v.is_nan() {
                f32::NAN
            } else {
                (v as f32).clamp(KDP_MIN_DISPLAY, KDP_MAX_DISPLAY)
            }
        })
        .collect()
}

/// The documented estimator at its own scope: `calc_system_PhiDP` queues
/// radial phases across **every cut below the fourth** (`max_elev_num`),
/// 200 deep, not per sweep — three cuts of samples where a single sweep
/// often falls short of the 40-radial floor. The value feeds
/// [`KdpParams::isdp_est_deg`]; the RPG persists its own across volumes
/// (`DP_ISDP_EST`), so using the current volume's estimate is one volume
/// fresher than the operational value, not a different algorithm.
pub fn estimate_volume_isdp(scan: &nexrad_model::data::Scan) -> Option<f32> {
    let mut queue: Vec<f64> = Vec::new();
    for sweep in scan.sweeps() {
        if queue.len() >= ISDP_MAX_QUEUE {
            break;
        }
        let radials = sweep.radials();
        let Some(first) = radials
            .iter()
            .take(5)
            .find(|r| r.differential_phase().is_some())
        else {
            continue;
        };
        if first.elevation_number() >= 4 {
            continue;
        }
        let inputs: Vec<DpInput> = radials.iter().filter_map(DpInput::from_radial).collect();
        if inputs.is_empty() {
            continue;
        }
        for radial in combine_sweep(&inputs, true) {
            if queue.len() >= ISDP_MAX_QUEUE {
                break;
            }
            if let Some(p) = radial_system_phi(&radial.phi, &radial.rho, &radial.z) {
                queue.push(p);
            }
        }
    }
    isdp_from_queue(queue).map(|v| v as f32)
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
    const ZDR_SCALE: f32 = 16.0;
    const ZDR_OFFSET: f32 = 128.0;

    /// One gate of a fixture moment.
    #[derive(Clone, Copy)]
    enum G {
        V(f64),
        Nd,
        Rf,
    }

    fn raw_of(scale: f32, offset: f32, g: G) -> u16 {
        match g {
            G::Nd => 0,
            G::Rf => 1,
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

    /// One dual-pol radial: φ and ρ per `phi_at`/`rho_at`, reflectivity per
    /// `z_at` (`None` leaves the moment off entirely), ZDR a flat 0 dB
    /// whenever reflectivity is present (the coherent recombination's
    /// vertical-power input).
    fn dp_radial(
        az: f64,
        spacing: f32,
        n: usize,
        phi_at: &dyn Fn(usize) -> G,
        rho_at: &dyn Fn(usize) -> G,
        z_at: Option<&dyn Fn(usize) -> G>,
    ) -> Radial {
        let phi: Vec<G> = (0..n).map(phi_at).collect();
        let rho: Vec<G> = (0..n).map(rho_at).collect();
        let (refl, zdr) = match z_at {
            Some(f) => {
                let z: Vec<G> = (0..n).map(f).collect();
                (
                    Some(m8(Z_SCALE, Z_OFFSET, &z)),
                    Some(m8(ZDR_SCALE, ZDR_OFFSET, &vec![G::V(0.0); n])),
                )
            }
            None => (None, None),
        };
        Radial::new(
            0,
            0,
            az as f32,
            spacing,
            RadialStatus::IntermediateRadialData,
            1,
            0.5,
            refl,
            None,
            None,
            zdr,
            Some(m16(PHI_SCALE, PHI_OFFSET, &phi)),
            Some(m16(RHO_SCALE, RHO_OFFSET, &rho)),
            None,
        )
    }

    fn params_with_isdp(isdp: f32) -> KdpParams {
        KdpParams {
            init_fdp_deg: Some(isdp),
            ..KdpParams::default()
        }
    }

    /// A clean φ ramp of 4 °/km (1° per 0.25 km gate) over solid ρ = 0.99:
    /// interior gates read exactly half the slope, 2.0 °/km, on both the
    /// short-gate radial (45 dBZ) and the long-gate one (30 dBZ) — with no
    /// curvature the two windows agree.
    #[test]
    fn a_clean_phidp_ramp_reads_half_the_slope() {
        let phi = |i: usize| G::V(100.0 + i as f64);
        let rho = |_: usize| G::V(0.99);
        let z30 = |_: usize| G::V(30.0);
        let z45 = |_: usize| G::V(45.0);
        let radials = vec![
            dp_radial(0.5, 1.0, D_GATES, &phi, &rho, Some(&z30)),
            dp_radial(1.5, 1.0, D_GATES, &phi, &rho, Some(&z45)),
        ];
        let derived = compute_kdp(&radials, &params_with_isdp(100.0)).expect("computes");
        assert_eq!(derived.values.len(), 2, "1° radials pass through");
        assert!((derived.gate_interval_km - 0.25).abs() < 1e-9);
        assert!((derived.first_gate_km - 0.125).abs() < 1e-9);
        for (which, row) in derived.values.iter().enumerate() {
            for (i, &v) in row.iter().enumerate().take(D_GATES - 30).skip(30) {
                assert!(
                    (f64::from(v) - 2.0).abs() < 1e-5,
                    "radial {which} gate {i}: got {v}, want 2.0",
                );
            }
        }
    }

    /// `Interpolate`'s tail rule: past the last valid group's `end − w/2`
    /// the smoothed φ holds constant, so the last gate's KDP is exactly 0
    /// on both chains even in the middle of a ramp.
    #[test]
    fn the_interpolation_tail_flattens_the_last_half_window() {
        let phi = |i: usize| G::V(100.0 + i as f64);
        let rho = |_: usize| G::V(0.99);
        let z30 = |_: usize| G::V(30.0);
        let z45 = |_: usize| G::V(45.0);
        let radials = vec![
            dp_radial(0.5, 1.0, D_GATES, &phi, &rho, Some(&z30)),
            dp_radial(1.5, 1.0, D_GATES, &phi, &rho, Some(&z45)),
        ];
        let derived = compute_kdp(&radials, &params_with_isdp(100.0)).expect("computes");
        for row in &derived.values {
            assert!(
                f64::from(row[D_GATES - 1]).abs() < 1e-9,
                "the flattened tail must read 0, got {}",
                row[D_GATES - 1],
            );
        }
    }

    /// The 40 dBZ window switch, observed through a φ step across a
    /// missing-φ gap (gates 150–179; φ 100 before, 200 after, ρ solid):
    ///
    /// * the 9-gate chain bridges `[145, 184]` — slope 100/39 °/gate — so
    ///   at gate 182 `kdp9 = (Σ j·φ)/30` over `[178, 186]` with the last
    ///   two gates flat 200: `Σ j·c = 49`, `kdp9 = (100/39)·49/30 =`
    ///   **4.188034** °/km;
    /// * the 25-gate chain bridges `[137, 192]` — slope 100/55 — and over
    ///   `[170, 194]` the ramp part cancels exactly (`Σ j(j+45) = 0` for
    ///   j = −12..10), leaving the two flat gates: `kdp25 = (20/11)·(11 +
    ///   12)·55/650 =` **3.538462** °/km;
    /// * 45 dBZ selects the short gate, 30 dBZ the long one, and a radial
    ///   with **no reflectivity at all** compares low against the
    ///   threshold and gets the long gate too;
    /// * the gap gates themselves stay undefined — the product keys the
    ///   output level on the input φ.
    #[test]
    fn the_40_dbz_rule_switches_between_short_and_long_windows() {
        let phi = |i: usize| match i {
            0..=149 => G::V(100.0),
            150..=179 => G::Nd,
            _ => G::V(200.0),
        };
        let rho = |i: usize| {
            if (150..=179).contains(&i) {
                G::V(0.95)
            } else {
                G::V(0.99)
            }
        };
        let z45 = |_: usize| G::V(45.0);
        let z30 = |_: usize| G::V(30.0);
        let radials = vec![
            dp_radial(0.5, 1.0, D_GATES, &phi, &rho, Some(&z45)),
            dp_radial(1.5, 1.0, D_GATES, &phi, &rho, Some(&z30)),
            dp_radial(2.5, 1.0, D_GATES, &phi, &rho, None),
        ];
        let derived = compute_kdp(&radials, &params_with_isdp(100.0)).expect("computes");
        let short = f64::from(derived.values[0][182]);
        let long = f64::from(derived.values[1][182]);
        let no_z = f64::from(derived.values[2][182]);
        assert!(
            (short - 4.188_034).abs() < 1e-4,
            "45 dBZ gate must use the 9-gate window: got {short}",
        );
        assert!(
            (long - 3.538_462).abs() < 1e-4,
            "30 dBZ gate must use the 25-gate window: got {long}",
        );
        assert_eq!(
            long, no_z,
            "missing reflectivity compares low and selects the long gate",
        );
        assert!(
            derived.values[0][165].is_nan(),
            "a missing-φ gate stays undefined even though the bridge crosses it",
        );
    }

    /// RhoHV censoring runs on the 5-gate smoothed ρ: a ρ = 0.3 stretch at
    /// gates 100–119 censors 98–121 (every gate whose window average dips
    /// under 0.9), while φ itself stays a clean ramp — so every *defined*
    /// gate still reads 2.0 °/km, the interpolation bridge being collinear
    /// with the ramp.
    #[test]
    fn low_correlation_censors_kdp_through_the_smoothed_rho() {
        let phi = |i: usize| G::V(100.0 + i as f64);
        let rho = |i: usize| {
            if (100..=119).contains(&i) {
                G::V(0.3)
            } else {
                G::V(0.99)
            }
        };
        let z30 = |_: usize| G::V(30.0);
        let radials = vec![dp_radial(0.5, 1.0, D_GATES, &phi, &rho, Some(&z30))];
        let derived = compute_kdp(&radials, &params_with_isdp(100.0)).expect("computes");
        let row = &derived.values[0];
        assert!(!row[97].is_nan(), "gate 97's smoothed rho is clean");
        for (i, &v) in row.iter().enumerate().take(121 + 1).skip(98) {
            assert!(v.is_nan(), "gate {i} must be censored");
        }
        assert!(!row[122].is_nan(), "gate 122's smoothed rho is clean");
        for i in (30..=97).chain(122..=373) {
            let v = row[i];
            if v.is_nan() {
                continue;
            }
            assert!(
                (f64::from(v) - 2.0).abs() < 1e-5,
                "gate {i}: got {v}, want 2.0",
            );
        }
    }

    /// A ramp that crosses 360° past the unfold start (gate 260 = 65 km):
    /// the documented unfolder lifts the wrapped stretch a full fold, and
    /// KDP stays the ramp's half-slope straight across — on a super-res
    /// sweep whose half-degree pairs recombine to 1° first.
    #[test]
    fn phidp_unfolds_across_the_fold_point() {
        let phi = |i: usize| G::V((100.0 + i as f64) % 360.0);
        let rho = |_: usize| G::V(0.99);
        let z30 = |_: usize| G::V(30.0);
        let radials: Vec<Radial> = (0..720)
            .map(|k| dp_radial(0.25 + 0.5 * k as f64, 0.5, D_GATES, &phi, &rho, Some(&z30)))
            .collect();
        let derived = compute_kdp(&radials, &params_with_isdp(100.0)).expect("computes");
        assert_eq!(derived.values.len(), 360, "720 half-degree radials pair");
        assert!(
            (derived.azimuths_deg[0] - 0.5).abs() < 1e-6,
            "the pair's azimuth is the degree centre, got {}",
            derived.azimuths_deg[0],
        );
        let row = &derived.values[100];
        for (i, &v) in row.iter().enumerate().take(330).skip(230) {
            assert!(
                (f64::from(v) - 2.0).abs() < 1e-5,
                "gate {i} (across the fold at 260): got {v}, want 2.0",
            );
        }
    }

    /// The coherent pair combination, against hand-computed cases: equal
    /// powers average the angle, a 20 dB power imbalance pulls the average
    /// toward the strong radial (atan2 of the summed vectors: 10.0985° for
    /// 10°/20° at 50/30 dBZ), the fold seam averages circularly, and a
    /// radial whose reflectivity is missing drops out of the vector sum
    /// entirely.
    #[test]
    fn coherent_recombination_is_circular_and_power_weighted() {
        let p5 = 10f64.powf(5.0); // 50 dBZ linear
        let p3 = 10f64.powf(3.0); // 30 dBZ linear
        // Identical inputs pass through exactly.
        let (phi, rho) = coherent_phi_rho((15.0, 15.0), (0.99, 0.99), (p3, p3), (p3, p3));
        assert!((phi - 15.0).abs() < 1e-9, "got {phi}");
        assert!(
            (rho - 0.99).abs() < 1e-9,
            "identical inputs keep rho: {rho}"
        );
        // Equal powers: plain angular mean — and the 10° phase spread
        // shortens the mean vector, so ρ contracts by cos(5°): the
        // decorrelation the coherent average is supposed to encode.
        let (phi, rho) = coherent_phi_rho((10.0, 20.0), (0.99, 0.99), (p3, p3), (p3, p3));
        assert!((phi - 15.0).abs() < 1e-9, "got {phi}");
        assert!(
            (rho - 0.99 * 5f64.to_radians().cos()).abs() < 1e-9,
            "a phase spread must decorrelate: {rho}",
        );
        // 20 dB imbalance: the strong radial dominates.
        let (phi, _) = coherent_phi_rho((10.0, 20.0), (0.99, 0.99), (p5, p3), (p5, p3));
        assert!((phi - 10.0985).abs() < 1e-3, "got {phi}");
        // The fold seam: 359° and 1° average to 0, not 180.
        let (phi, _) = coherent_phi_rho((359.0, 1.0), (0.99, 0.99), (p3, p3), (p3, p3));
        assert!(phi.min(360.0 - phi) < 1e-9, "got {phi}");
        // One side without reflectivity: the other's phase, unchanged.
        let (phi, _) = coherent_phi_rho((10.0, 20.0), (0.99, 0.99), (f64::NAN, p3), (f64::NAN, p3));
        assert!((phi - 20.0).abs() < 1e-9, "got {phi}");
        // No usable vector on either side: undefined.
        let (phi, rho) = coherent_phi_rho(
            (10.0, 20.0),
            (f64::NAN, 0.99),
            (p3, f64::NAN),
            (p3, f64::NAN),
        );
        assert!(phi.is_nan() && rho.is_nan());
    }

    /// The A/B knob moves the documented direction: on a super-res ramp
    /// whose pair members sit 6° apart (a plausible azimuthal gradient),
    /// the members straddle the 360° seam for five consecutive gates
    /// (258–262). The coherent primary averages circularly and reads the
    /// clean half-slope straight through; the plain arithmetic mean
    /// manufactures a ~180° plateau there — too wide for the 5-gate median
    /// to heal, and below the unfolder's 180° threshold — and the slope
    /// blows up around it.
    #[test]
    fn a_plain_mean_recombination_breaks_at_the_fold_seam() {
        let phi_a = |i: usize| G::V((97.0 + i as f64) % 360.0);
        let phi_b = |i: usize| G::V((103.0 + i as f64) % 360.0);
        let rho = |_: usize| G::V(0.99);
        let z30 = |_: usize| G::V(30.0);
        let radials: Vec<Radial> = (0..720)
            .map(|k| {
                let az = 0.25 + 0.5 * k as f64;
                if k % 2 == 0 {
                    dp_radial(az, 0.5, D_GATES, &phi_a, &rho, Some(&z30))
                } else {
                    dp_radial(az, 0.5, D_GATES, &phi_b, &rho, Some(&z30))
                }
            })
            .collect();
        let params = params_with_isdp(100.0);
        let coherent = compute_kdp(&radials, &params).expect("computes");
        let plain = compute_kdp_impl(
            &radials,
            &params,
            KdpOptions {
                recomb: Recomb::PlainMean,
                ..KdpOptions::primary()
            },
        )
        .expect("computes");
        // The members straddle 360 across gates 258–262.
        let row_c = &coherent.values[50];
        let row_p = &plain.values[50];
        for (i, &v) in row_c.iter().enumerate().take(270 + 1).skip(250) {
            assert!(
                (f64::from(v) - 2.0).abs() < 1e-4,
                "coherent gate {i}: got {v}",
            );
        }
        let worst = (250..=270)
            .map(|i| (f64::from(row_p[i]) - 2.0).abs())
            .fold(0.0, f64::max);
        assert!(
            worst > 1.0,
            "the plain mean's fold plateau should wreck the slope, worst delta {worst}",
        );
    }

    /// The documented ISDP estimator (`calc_system_PhiDP.c`): per radial
    /// the 360°-aware median of the first 11-gate high-quality run past
    /// 25 km, and across the sweep the `round(n/20)`-th entry of the
    /// sorted queue. Radials whose run starts inside 25 km or touches a
    /// ≥ 40 dBZ gate contribute nothing, and fewer than 40 samples
    /// conclude nothing.
    #[test]
    fn the_isdp_estimator_returns_the_documented_percentile() {
        let combined = |phi_val: f64, from: usize, z_val: f64| -> CombinedRadial {
            let n = 200;
            CombinedRadial {
                az: 0.0,
                phi: (0..n).map(|_| phi_val).collect(),
                rho: (0..n).map(|i| if i >= from { 0.99 } else { 0.5 }).collect(),
                z: vec![z_val; n],
                vel: Vec::new(),
                spw: Vec::new(),
                dr0: 0.125,
                dg: 0.25,
                zr0: 0.125,
                zg: 0.25,
            }
        };

        // 60 radials with phases 10..69: sorted queue index round(60/20) = 3
        // reads 13.
        let sweep: Vec<CombinedRadial> = (0..60)
            .map(|k| combined(10.0 + k as f64, 100, 20.0))
            .collect();
        assert_eq!(estimate_isdp(&sweep), Some(13.0));

        // A run starting inside 25 km (gate 60) is rejected outright.
        let close = combined(10.0, 60, 20.0);
        assert_eq!(radial_system_phi(&close.phi, &close.rho, &close.z), None);

        // A ≥ 40 dBZ gate inside the run rejects the radial.
        let hot = combined(10.0, 100, 45.0);
        assert_eq!(radial_system_phi(&hot.phi, &hot.rho, &hot.z), None);

        // 39 qualifying radials conclude nothing.
        let thin: Vec<CombinedRadial> = (0..39)
            .map(|k| combined(10.0 + k as f64, 100, 20.0))
            .collect();
        assert_eq!(estimate_isdp(&thin), None);

        // Phases straddling 360 sort fold-aware: 350..359.5 and 0..9.5 read
        // percentile 351, not a seam artifact.
        let wrapped: Vec<CombinedRadial> = (0..40)
            .map(|k| combined((350.0 + 0.5 * k as f64) % 360.0, 100, 20.0))
            .collect();
        assert_eq!(estimate_isdp(&wrapped), Some(351.0));

        // And the wiring: a provided RDA value wins; absent one, the
        // estimator's value is what compute reports using. Low ρ inside
        // 25 km keeps the qualifying run from starting too close.
        let phi = |i: usize| G::V(100.0 + i as f64);
        let rho = |i: usize| {
            if i < 100 { G::V(0.5) } else { G::V(0.99) }
        };
        let z20 = |_: usize| G::V(20.0);
        let radials: Vec<Radial> = (0..45)
            .map(|k| dp_radial(0.5 + k as f64, 1.0, D_GATES, &phi, &rho, Some(&z20)))
            .collect();
        let with = compute_kdp(&radials, &params_with_isdp(77.0)).expect("computes");
        assert_eq!(with.init_fdp_deg, 77.0);
        let without = compute_kdp(&radials, &KdpParams::default()).expect("computes");
        // Every radial's first qualifying run is gates 100–110 with
        // φ = 200..210, median 205; the percentile of identical values is
        // 205.
        assert_eq!(without.init_fdp_deg, 205.0);
        // The isdp-applied variant prefers the estimate and falls back to
        // the RDA value exactly as the source's `isdp_est != -99` guard
        // does: a 44-radial sweep concludes 205; a too-thin one keeps 77.
        let applied = KdpOptions {
            isdp: IsdpSource::Estimated,
            ..KdpOptions::primary()
        };
        let est = compute_kdp_impl(&radials, &params_with_isdp(77.0), applied).expect("computes");
        assert_eq!(est.init_fdp_deg, 205.0);
        let thin_est =
            compute_kdp_impl(&radials[..30], &params_with_isdp(77.0), applied).expect("computes");
        assert_eq!(
            thin_est.init_fdp_deg, 77.0,
            "the -99 fallback is the RDA value"
        );
    }

    /// Range-folded and missing φ gates stay undefined in the output —
    /// the product keys the gate level on the input φ — while their
    /// neighbours survive.
    #[test]
    fn rf_and_missing_phi_gates_stay_undefined() {
        let phi = |i: usize| match i {
            200 => G::Rf,
            201 => G::Nd,
            _ => G::V(100.0 + i as f64),
        };
        let rho = |_: usize| G::V(0.99);
        let z30 = |_: usize| G::V(30.0);
        let radials = vec![dp_radial(0.5, 1.0, D_GATES, &phi, &rho, Some(&z30))];
        let derived = compute_kdp(&radials, &params_with_isdp(100.0)).expect("computes");
        let row = &derived.values[0];
        assert!(row[200].is_nan(), "an RF gate is undefined");
        assert!(row[201].is_nan(), "a missing gate is undefined");
        assert!(!row[199].is_nan() && !row[202].is_nan());
    }

    /// The product's display range: a 24 °/km ramp clamps to exactly
    /// `KDP_MAX_DISPLAY`, a −10 °/km one to exactly `KDP_MIN_DISPLAY` —
    /// the caps `dualpol8bit.c` applies (10.0) and the 16-bit moment's
    /// minimum level preserves (−2.05). The steep ramp runs at 45 dBZ so
    /// the clean 9-gate window carries it (a 25-gate window on a 40-gate
    /// radial never escapes the edge-truncation bias).
    #[test]
    fn steep_ramps_clamp_to_the_products_display_range() {
        let up = |i: usize| G::V(100.0 + 6.0 * i as f64);
        let down = |i: usize| G::V(350.0 - 2.5 * i as f64);
        let rho = |_: usize| G::V(0.99);
        let z30 = |_: usize| G::V(30.0);
        let z45 = |_: usize| G::V(45.0);
        let steep = compute_kdp(
            &[dp_radial(0.5, 1.0, 40, &up, &rho, Some(&z45))],
            &params_with_isdp(100.0),
        )
        .expect("computes");
        assert_eq!(
            steep.values[0][15], KDP_MAX_DISPLAY,
            "24 °/km must cap at 10",
        );
        let neg = compute_kdp(
            &[dp_radial(0.5, 1.0, 100, &down, &rho, Some(&z30))],
            &params_with_isdp(350.0),
        )
        .expect("computes");
        assert_eq!(
            neg.values[0][30], KDP_MIN_DISPLAY,
            "−5 °/km must floor at −2.05",
        );
    }

    /// `Is_high_atten_radial`'s documented thresholds, gate for gate: more
    /// than 10 qualifying gates past bin 180 flag the radial; each
    /// threshold edge disqualifies.
    #[test]
    fn high_attenuation_radials_are_detected_by_the_documented_test() {
        let n = 250;
        let qualify = |count: usize, start: usize| -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
            let (mut z, mut v, mut w, mut r) =
                (vec![20.0; n], vec![0.0; n], vec![0.0; n], vec![0.99; n]);
            for i in start..start + count {
                z[i] = 40.0;
                v[i] = 5.0;
                w[i] = 3.0;
                r[i] = 0.7;
            }
            (z, v, w, r)
        };

        let (z, v, w, r) = qualify(11, 200);
        assert!(is_high_attenuation_radial(&z, &v, &w, &r), "11 > 10 gates");
        let (z, v, w, r) = qualify(10, 200);
        assert!(
            !is_high_attenuation_radial(&z, &v, &w, &r),
            "10 is not > 10"
        );
        let (z, v, w, r) = qualify(11, 100);
        assert!(
            !is_high_attenuation_radial(&z, &v, &w, &r),
            "gates before bin 180 do not count",
        );

        // Each threshold edge in turn.
        let (mut z, v, w, r) = qualify(11, 200);
        z[205] = 29.9;
        assert!(!is_high_attenuation_radial(&z, &v, &w, &r), "z floor");
        let (z, mut v, w, r) = qualify(11, 200);
        v[205] = 0.9;
        assert!(!is_high_attenuation_radial(&z, &v, &w, &r), "|v| floor");
        let (z, v, mut w, r) = qualify(11, 200);
        w[205] = 2.0;
        assert!(
            !is_high_attenuation_radial(&z, &v, &w, &r),
            "sw must exceed 2",
        );
        let (z, v, w, mut r) = qualify(11, 200);
        r[205] = 0.81;
        assert!(!is_high_attenuation_radial(&z, &v, &w, &r), "rho ceiling");
    }

    /// [`DerivedKdp::to_polar_grid`] mirrors the twin comparator's
    /// resampling: the radial covering the cell centre, the gate whose
    /// centre falls nearest the cell centre with the earlier gate winning
    /// the exact tie, and nothing claimed outside a radial's own span.
    #[test]
    fn to_polar_grid_resamples_like_the_twin_comparator() {
        let derived = DerivedKdp {
            values: vec![
                (0..40).map(|j| j as f32).collect(),
                (0..10).map(|j| j as f32).collect(),
            ],
            azimuths_deg: vec![0.5, 1.5],
            first_gate_km: 0.125,
            gate_interval_km: 0.25,
            radial_width_deg: 1.0,
            init_fdp_deg: 0.0,
        };
        let grid = derived.to_polar_grid();
        // Cell (0, 5): gate centres 5.375 (j 21) and 5.625 (j 22) tie at
        // 0.125 km from the cell centre — the earlier gate wins, as in
        // `tally_packet`.
        assert_eq!(grid[0][5], 21.0);
        assert_eq!(grid[0][0], 1.0, "bin 0 reads gate 1 (centre 0.375)");
        // The second radial covers cell 1 but carries only 10 gates.
        assert_eq!(grid[1][0], 1.0);
        assert!(grid[1][5].is_nan(), "gate 21 is past the short radial");
        // No radial spans azimuth 5°: nothing may claim it.
        assert!(grid[5].iter().all(|v| v.is_nan()));
    }

    /// A radial with no meteorological group at all (ρ everywhere below
    /// 0.9) censors everything and panics nowhere.
    #[test]
    fn a_radial_with_no_meteo_group_is_fully_censored() {
        let phi = |i: usize| G::V(100.0 + i as f64);
        let rho = |_: usize| G::V(0.5);
        let z30 = |_: usize| G::V(30.0);
        let radials = vec![dp_radial(0.5, 1.0, D_GATES, &phi, &rho, Some(&z30))];
        let derived = compute_kdp(&radials, &params_with_isdp(100.0)).expect("computes");
        assert!(derived.values[0].iter().all(|v| v.is_nan()));
    }
}
