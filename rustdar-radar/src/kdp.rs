//! Specific Differential Phase (the RPG's product 163, AWIPS `N0K`) computed
//! locally from the Level II dual-pol moments of one tilt.

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
/// smoothed RhoHV, clamp to the product's display range.
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
/// often falls short of the 40-radial floor.
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
