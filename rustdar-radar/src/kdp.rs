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
//! **The live twin harness, its `validation_policy`, and the full survey
//! record live on branch `campaign-harness`**; re-measuring means that
//! branch.
//!
//! The harness scores the derivation against the RPG's own N0K for the
//! **same volume and cut** (paired by PDB volume start plus elevation
//! number, angle-matched where a site's product cut numbering differs from
//! the RDA's), in the twin's own data levels — the PDB's declared scale 20
//! / offset 43 was verified on every live twin, so one data level is
//! 0.05 °/km. As last measured (three full-roster surveys) the derivation
//! does **not** meet the campaign's double bar: quiet/stratiform sites
//! read gate-exact, convective sites miss. What the surveys established,
//! each A/B scored on tuning sites and confirmed on holdouts that played
//! no part in the choice:
//!
//! * **Coherent recombination** wins everywhere, both sets, every survey,
//!   on levels and on presence. The documented `Recomb_dp_data` average
//!   is the primary, uncontradicted.
//! * **The attenuation term in the window switch** (`delta_z`) is inert:
//!   identical scores to two decimals at every site. Kept, per the source.
//! * **Initial system phase** is the residual's first component. Every
//!   RDA header on the roster declares the default 60.0°, but the twins
//!   behave like the `isdp_apply` branch is live in the fleet: the misses
//!   concentrate in a one-sided +1-level shoulder — the signature of
//!   leading-edge ramps climbing from 60° to the data's true system phase
//!   while the twin's sit flat. Where the single-volume estimator
//!   concludes, applying it recovers within-±1 and never loses; but it
//!   concludes only in broad rain, while the RPG **persists** its
//!   estimate across volumes in `DP_ISDP_EST` — state a single archived
//!   volume cannot reproduce. The documented `isdp_apply = NO` default
//!   stays primary and the finding is recorded instead of tuned around.
//! * The rest of the residual is weak-band jitter around gradients: the
//!   censor and the meteorological grouping both hinge on `rho_smd ≥ 0.9`
//!   at gates where smoothed ρ sits within rounding of the threshold, and
//!   one flipped gate moves a whole interpolation bridge. `corr_thresh`
//!   itself is URC-adaptable per site ([0.5, 1.0]), like the ISDP store —
//!   operational state the archive stream does not carry. Nothing
//!   undocumented was chased.
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
//! record was measured with** (the record lives on branch
//! `campaign-harness`; the product ships as a fetch either way), so the
//! record and the code stay one thing. The B21-new `ra_gate` φ chain
//! (`DPRA`, window 7) and `DPIN` feed DP QPE/CDA, not KDP.

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
    /// `dbz0` −43.5 dB sits mid-range of the RDA calibration constants
    /// read from live archives; `atmos` −0.012 dB/km was the value at
    /// every site surveyed (measured provenance: branch
    /// `campaign-harness`). The
    /// initial phase stays `None` — the documented estimator resolves it
    /// from the data. A ±2 dB `dbz0` error moves only the no-echo boundary
    /// at the SNR-5 dB fringe; the twin-validated paths always read the
    /// real values via [`from_archive`](Self::from_archive).
    ///
    /// **Provenance, stated because this pair is original and it ships.**
    /// Neither number is transcribed from anywhere: the ORPG reads both out of
    /// the RDA headers and publishes no fleet-typical stand-in, so there is no
    /// authority to check these two against. They are also not test-only —
    /// [`crate::render`] and [`crate::derive`] both build a `KdpParams` from
    /// this, and the same values reach the rendered HHC composite through
    /// [`crate::hhc`], where a wrong `dbz0` is invisible rather than loud.
    ///
    /// Three separate honesty notes, because the three claims above have
    /// three different standings:
    ///
    /// * **−43.5 dB "mid-range"** — no count of archives, no site list and no
    ///   spread is recorded with it, here or on `campaign-harness`. It is a
    ///   remembered reading, not a survey result.
    /// * **−0.012 dB/km "every site surveyed"** — a real survey, whose record
    ///   lives only on `campaign-harness`. It is a historical measurement this
    ///   tree cannot reproduce; re-running it means checking out that branch
    ///   and re-reading `atmos_atten` across the roster's volume headers.
    /// * **"±2 dB moves only the SNR-5 dB fringe"** — an argument from where
    ///   `dbz0` enters the SNR test, not a sweep. Nothing measured what a ±2 dB
    ///   shift does to the classification, and no test bounds it.
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
mod tests;
