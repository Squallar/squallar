//! The WSR-88D dual-polarization preprocessor chain, factored out of
//! [`crate::kdp`] for shared consumers (KDP, HCA).
//!
//! Everything here is a function-for-function transcription of the released
//! ORPG source: the azimuth recombination task `cpc004/tsk009` (`recomb`),
//! the dual-pol preprocessor `cpc004/tsk011` (`dpprep`) and its
//! `calc_system_PhiDP.c` estimator, with the fleet-default adaptation values
//! from `cpc104/lib006/dpprep.alg`. Originally transcribed from the Build 16
//! mirror (github likev/CodeOrpgPub) and cross-checked against the CODE
//! **Build 21.0r1.7** public source; the full provenance, validation history
//! and documented gaps live in [`crate::kdp`]'s module documentation — this
//! module only holds the shared machinery, moved here verbatim so the
//! hydrometeor classification chain can consume the same preprocessor
//! without duplicating it.
//!
//! # Build 21 delta: Met Signal processing (CCR NA14-00100, fleet ON)
//!
//! Build 17 added, and B21's `dpprep.alg` defaults to,
//! `metsignal_processing = ON`: the meteorological gate flag no longer comes
//! from `rho_smd ≥ corr_thresh` (or SNR on high-attenuation radials) but from
//! a fuzzy **met-signal** per gate (`findMetSignal.cpp`): six trapezoidal
//! votes — Z (weight 2, trap 10..30), ρ (1, trap 0.75..0.9), an inverted |V|
//! trap (1, −1.5..1.5), and the 9-gate textures of raw φ (2, 0/0/10/20), raw
//! ZDR (2, 0/0/1/2) and raw ρ (1, 0/0/0.02/0.04) — averaged over the weights
//! of the present inputs (≥ 4 required), scaled to 0–100, zeroed when
//! SNR < 0 dB or when ≥ threshold but ρ < 0.65 / ZDR outside ±4.5
//! ([`find_met_signal`]). `cleanMetSignal.cpp` then dilates the ≥ 80 signal
//! by one gate, trims it back at run edges, and despeckles runs spanning
//! < 2 bins to 78.5 ([`clean_met_signal`]). The flag is `met > 80`
//! (`dpp_process.c`'s strict `<=` → 0), and `Unfold_PhiDP` filters on
//! `met ≥ 80` instead of ρ ≥ 0.85 — [`unfold_phidp`] is parameterized
//! accordingly, with the legacy ρ/0.85 pair for `metsignal_processing = OFF`.
//!
//! The companion **reflectivity CAPPI** (`ReflCAPPI.cpp`): a persistent
//! 360° × 1 km grid of raw Z near 3 km ARL, updated from every radial with
//! elevation ≥ 1° outside 30 km, that rescues weak met-signal gates
//! (0 < met < 80 → 99.5) below 3 km where the stored CAPPI exceeds 11 dBZ.
//! Both `update_CAPPI` and `apply_CAPPI` return early for elevations under
//! 1.0°, so the CAPPI **never touches the lowest split cuts** — it matters
//! to the melting-layer tilts' inner ranges and to the hybrid-scan tiers.
//! Operationally the CAPPI persists across volumes (15-minute freshness);
//! [`ReflCappi`] rebuilds it from the current volume's own ≥ 1° sweeps —
//! the nearest state a single archived volume can reproduce, one volume
//! fresher than the operational grid, with the cold-start (no CAPPI) as the
//! bounded A/B alternative.
//!
//! `findBragg` (ZDR-calibration monitoring) and the B21-new `DPRA`/`DPIN`
//! output phases (DP QPE / CDA consumers) alter no field this chain reads
//! and are not transcribed.

use nexrad_model::data::{DataMoment, MomentValue, Radial};

// ── dpprep.alg fleet defaults ────────────────────────────────────────────────

/// `corr_thresh`: RhoHV (5-gate smoothed) below this censors KDP and marks a
/// gate non-meteorological.
pub(crate) const CORR_THRESH: f64 = 0.9;
/// `dbz_thresh`: minimum smoothed reflectivity for the short-gate KDP.
pub(crate) const DBZ_THRESH: f64 = 40.0;
/// `md_snr_thresh`: the meteo flag's SNR threshold on high-attenuation radials.
pub(crate) const MD_SNR_THRESH: f64 = 5.0;
/// `dbz_window` / `window`: reflectivity and general smoothing windows.
pub(crate) const DBZ_WINDOW: usize = 3;
pub(crate) const WINDOW: usize = 5;
/// `short_gate` / `long_gate`: the two KDP estimation windows.
pub(crate) const SHORT_GATE: usize = 9;
pub(crate) const LONG_GATE: usize = 25;
/// High-attenuation-radial test (`art_*`).
pub(crate) const ART_START_BIN: usize = 180;
pub(crate) const ART_COUNT: usize = 10;
pub(crate) const ART_MIN_Z: f64 = 30.0;
pub(crate) const ART_MAX_Z: f64 = 50.0;
pub(crate) const ART_V: f64 = 1.0;
pub(crate) const ART_CORR: f64 = 0.8;
pub(crate) const ART_MIN_SW: f64 = 2.0;

// ── Unfold_PhiDP literals (dpp_process.c) ────────────────────────────────────

pub(crate) const FOLD_DEG: f64 = 360.0;
pub(crate) const UNFOLD_MIN_RHO: f64 = 0.85;
pub(crate) const HIST_WINDOW: usize = 30;
/// The history median needs **more than** this many qualifying gates.
pub(crate) const HIST_COUNT_THRESH: usize = 25;
/// `max_stddev = fold/3`.
pub(crate) const HIST_MAX_STDDEV: f64 = FOLD_DEG / 3.0;
/// Unfolding needs **more than** this many valid gates accumulated…
pub(crate) const UNFOLD_MIN_VALID: usize = 15;
/// …and only engages **past** this gate (60 km at 0.25 km gates).
pub(crate) const UNFOLD_START_BIN: usize = 240;

// ── calc_system_PhiDP.c literals ─────────────────────────────────────────────

pub(crate) const ISDP_MIN_ECHO: usize = 11;
pub(crate) const ISDP_MAX_QUEUE: usize = 200;
pub(crate) const ISDP_MIN_SAMPLES: usize = 40;
pub(crate) const ISDP_MIN_RHO: f64 = 0.986;
pub(crate) const ISDP_Z_MIN: f64 = 0.0;
pub(crate) const ISDP_Z_REJECT: f64 = 40.0;
/// The 11-gate run must start at or past this gate (25 km).
pub(crate) const ISDP_TOO_CLOSE: usize = 100;
/// Sorted-queue crossover handling: spread > 200° means the values straddle
/// 360, and entries under 270° get lifted a fold before re-sorting.
pub(crate) const ISDP_CROSSOVER: f64 = 200.0;
pub(crate) const ISDP_ADJUST: f64 = 270.0;

// ── Input extraction ─────────────────────────────────────────────────────────

/// One input radial's fields in physical units, `NaN` for below-threshold
/// and range-folded gates (`Icd_to_intern` maps both outside the valid
/// range; the RF distinction only matters to the product's flag levels,
/// which decode as undefined either way).
pub(crate) struct DpInput {
    pub(crate) az: f64,
    pub(crate) phi: Vec<f64>,
    pub(crate) rho: Vec<f64>,
    pub(crate) zdr: Vec<f64>,
    pub(crate) z: Vec<f64>,
    pub(crate) vel: Vec<f64>,
    pub(crate) spw: Vec<f64>,
    /// Centre of DP gate 0 and DP gate size, km.
    pub(crate) dr0: f64,
    pub(crate) dg: f64,
    /// Centre of Z gate 0 and Z gate size, km.
    pub(crate) zr0: f64,
    pub(crate) zg: f64,
    /// The radial's own angular width, degrees.
    pub(crate) spacing: f64,
    /// Half-degree radial (super-res), the recombination precondition.
    pub(crate) half_degree: bool,
    /// The radial's elevation angle, degrees (`bh->elevation`) — the HCA
    /// chain's melting-layer beam intersection reads it; the KDP chain does
    /// not.
    pub(crate) elev: f64,
}

/// One moment's gates as `f64`, with every non-value gate a `NaN`.
///
/// `iter`, not `values`: the latter is `iter().collect()`, so going through it
/// would build an intermediate `Vec<MomentValue>` — eight bytes per gate — only
/// to walk it once and drop it. This runs six times per radial.
pub(crate) fn decode_moment(moment: &nexrad_model::data::MomentData) -> Vec<f64> {
    moment
        .iter()
        .map(|v| match v {
            MomentValue::Value(x) => f64::from(x),
            _ => f64::NAN,
        })
        .collect()
}

impl DpInput {
    pub(crate) fn from_radial(radial: &Radial) -> Option<Self> {
        let phi_m = radial.differential_phase()?;
        let rho_m = radial.correlation_coefficient()?;
        let (zdr, z, zr0, zg) = {
            let zdr = radial
                .differential_reflectivity()
                .map(decode_moment)
                .unwrap_or_default();
            match radial.reflectivity() {
                Some(m) => (
                    zdr,
                    decode_moment(m),
                    m.first_gate_range_km(),
                    m.gate_interval_km(),
                ),
                None => (zdr, Vec::new(), 0.0, 0.25),
            }
        };
        Some(Self {
            az: f64::from(radial.azimuth_angle_degrees()),
            phi: decode_moment(phi_m),
            rho: decode_moment(rho_m),
            zdr,
            z,
            vel: radial.velocity().map(decode_moment).unwrap_or_default(),
            spw: radial
                .spectrum_width()
                .map(decode_moment)
                .unwrap_or_default(),
            dr0: phi_m.first_gate_range_km(),
            dg: phi_m.gate_interval_km(),
            zr0,
            zg,
            spacing: f64::from(radial.azimuth_spacing_degrees()),
            half_degree: radial.azimuth_spacing_degrees() < 0.75,
            elev: f64::from(radial.elevation_angle_degrees()),
        })
    }
}

// ── Azimuth recombination (cpc004/tsk009) ────────────────────────────────────

/// One recombined (or passed-through) radial, ready for the preprocessor.
pub(crate) struct CombinedRadial {
    pub(crate) az: f64,
    pub(crate) phi: Vec<f64>,
    pub(crate) rho: Vec<f64>,
    pub(crate) z: Vec<f64>,
    pub(crate) vel: Vec<f64>,
    pub(crate) spw: Vec<f64>,
    pub(crate) dr0: f64,
    pub(crate) dg: f64,
    pub(crate) zr0: f64,
    pub(crate) zg: f64,
}

impl CombinedRadial {
    pub(crate) fn passthrough(input: &DpInput) -> Self {
        Self {
            az: input.az,
            phi: input.phi.clone(),
            rho: input.rho.clone(),
            z: input.z.clone(),
            vel: input.vel.clone(),
            spw: input.spw.clone(),
            dr0: input.dr0,
            dg: input.dg,
            zr0: input.zr0,
            zg: input.zg,
        }
    }

    /// The single-radial recombination: fields pass through, the azimuth
    /// snaps to the half-degree index (`Get_recombined_azi`).
    pub(crate) fn single(input: &DpInput) -> Self {
        let mut out = Self::passthrough(input);
        if input.half_degree {
            out.az = input.az.trunc() + 0.5;
        }
        out
    }
}

/// Pair consecutive half-degree radials per `combine_radials.c`: the saved
/// radial must sit in the first half of its degree (`Index_angle` 0.5), the
/// next radial within (0, 0.75]° of it. Unpairable radials go through the
/// single-radial path.
pub(crate) fn combine_sweep(inputs: &[DpInput], coherent: bool) -> Vec<CombinedRadial> {
    let mut out = Vec::with_capacity(inputs.len() / 2 + 1);
    let mut saved: Option<&DpInput> = None;
    for input in inputs {
        if !input.half_degree {
            out.push(CombinedRadial::passthrough(input));
            continue;
        }
        match saved.take() {
            None => saved = Some(input),
            Some(first) => {
                let mut diff = input.az - first.az;
                if diff < -180.0 {
                    diff += 360.0;
                }
                let fazi = first.az - first.az.trunc();
                let pairable = (0.0..=0.75).contains(&diff)
                    && fazi <= 0.5
                    && first.phi.len() == input.phi.len()
                    && (first.dr0 - input.dr0).abs() < 1e-6;
                if pairable {
                    out.push(combine_pair(first, input, coherent));
                } else {
                    out.push(CombinedRadial::single(first));
                    saved = Some(input);
                }
            }
        }
    }
    if let Some(first) = saved {
        out.push(CombinedRadial::single(first));
    }
    out
}

/// Linear reflectivity power at the Z gate covering DP gate `i`, or `NaN`.
/// The calibration and range-correction factors of the source's `Get_p`
/// cancel between the two radials (same gate), so plain `10^(Z/10)` carries
/// exactly the relative weight that survives into the average.
pub(crate) fn dp_gate_power(input: &DpInput, i: usize) -> f64 {
    let d = input.dr0 + i as f64 * input.dg;
    match index_into(d, input.zr0, input.zg, input.z.len()) {
        Some(zi) => {
            let z = input.z[zi];
            if z.is_nan() {
                f64::NAN
            } else {
                10f64.powf(0.1 * z)
            }
        }
        None => f64::NAN,
    }
}

/// `Create_a_index`'s gate mapping: the output gate whose span covers range
/// `d`, `None` out of range.
pub(crate) fn index_into(d: f64, or0: f64, og: f64, n: usize) -> Option<usize> {
    if og <= 0.0 {
        return None;
    }
    let mut oi = ((d - or0) / og).trunc() as i64;
    if d - (or0 + oi as f64 * og) > 0.5 * og {
        oi += 1;
    }
    if oi < 0 || oi >= n as i64 {
        None
    } else {
        Some(oi as usize)
    }
}

/// φ and ρ of one gate combined coherently per `Recomb_dp_data`. Inputs are
/// per-radial (φ, ρ, horizontal power, vertical power); the fallbacks for a
/// missing side are the source's.
pub(crate) fn coherent_phi_rho(
    phi: (f64, f64),
    rho: (f64, f64),
    ph: (f64, f64),
    pv: (f64, f64),
) -> (f64, f64) {
    let vector = |phi: f64, rho: f64, ph: f64, pv: f64| -> Option<(f64, f64)> {
        if phi.is_nan() || rho.is_nan() || ph.is_nan() || pv.is_nan() {
            return None;
        }
        let t = rho * (ph * pv).sqrt();
        let f = -phi.to_radians();
        Some((t * f.cos(), t * f.sin()))
    };
    let avg = |a: f64, b: f64| -> f64 {
        match (a.is_nan(), b.is_nan()) {
            (false, false) => 0.5 * (a + b),
            (false, true) => a,
            (true, false) => b,
            (true, true) => f64::NAN,
        }
    };
    let v1 = vector(phi.0, rho.0, ph.0, pv.0);
    let v2 = vector(phi.1, rho.1, ph.1, pv.1);
    let (re, im) = match (v1, v2) {
        (Some((r1, i1)), Some((r2, i2))) => (0.5 * (r1 + r2), 0.5 * (i1 + i2)),
        (Some(v), None) | (None, Some(v)) => v,
        (None, None) => (f64::NAN, f64::NAN),
    };
    let phc = avg(ph.0, ph.1);
    let pvc = avg(pv.0, pv.1);

    let phi_c = if re.is_nan() || im.is_nan() {
        f64::NAN
    } else {
        let mut p = -im.atan2(re).to_degrees();
        if p < 0.0 {
            p += FOLD_DEG;
        }
        p
    };
    let rho_c = if re.is_nan() || im.is_nan() || phc.is_nan() || pvc.is_nan() {
        f64::NAN
    } else {
        ((re * re + im * im) / (phc * pvc)).sqrt()
    };
    (phi_c, rho_c)
}

pub(crate) fn nan_mean(a: f64, b: f64) -> f64 {
    match (a.is_nan(), b.is_nan()) {
        (false, false) => 0.5 * (a + b),
        (false, true) => a,
        (true, false) => b,
        (true, true) => f64::NAN,
    }
}

pub(crate) fn combine_pair(a: &DpInput, b: &DpInput, coherent: bool) -> CombinedRadial {
    let n = a.phi.len().min(b.phi.len());
    let mut phi = Vec::with_capacity(n);
    let mut rho = Vec::with_capacity(n);
    for i in 0..n {
        if coherent {
            let pha = dp_gate_power(a, i);
            let phb = dp_gate_power(b, i);
            let zdra = a.zdr.get(i).copied().unwrap_or(f64::NAN);
            let zdrb = b.zdr.get(i).copied().unwrap_or(f64::NAN);
            let pva = if pha.is_nan() || zdra.is_nan() {
                f64::NAN
            } else {
                pha / 10f64.powf(0.1 * zdra)
            };
            let pvb = if phb.is_nan() || zdrb.is_nan() {
                f64::NAN
            } else {
                phb / 10f64.powf(0.1 * zdrb)
            };
            let (p, r) = coherent_phi_rho(
                (a.phi[i], b.phi[i]),
                (a.rho[i], b.rho[i]),
                (pha, phb),
                (pva, pvb),
            );
            phi.push(p);
            rho.push(r);
        } else {
            phi.push(nan_mean(a.phi[i], b.phi[i]));
            rho.push(nan_mean(a.rho[i], b.rho[i]));
        }
    }

    // Reflectivity: linear-power mean, single-radial fallback (Combine_azi).
    let mut z = Vec::with_capacity(a.z.len().max(b.z.len()));
    for i in 0..a.z.len().max(b.z.len()) {
        let za = a.z.get(i).copied().unwrap_or(f64::NAN);
        let zb = b.z.get(i).copied().unwrap_or(f64::NAN);
        let v = match (za.is_nan(), zb.is_nan()) {
            (false, false) => 10.0 * (0.5 * (10f64.powf(0.1 * za) + 10f64.powf(0.1 * zb))).log10(),
            (false, true) => za,
            (true, false) => zb,
            (true, true) => f64::NAN,
        };
        z.push(v);
    }

    // Doppler fields: plain pair mean (see the module doc's gap list).
    let mean_vec = |x: &[f64], y: &[f64]| -> Vec<f64> {
        (0..x.len().max(y.len()))
            .map(|i| {
                nan_mean(
                    x.get(i).copied().unwrap_or(f64::NAN),
                    y.get(i).copied().unwrap_or(f64::NAN),
                )
            })
            .collect()
    };

    let mut az1 = a.az;
    let mut az2 = b.az;
    if az1 - az2 > 180.0 {
        az2 += 360.0;
    }
    if az2 - az1 > 180.0 {
        az1 += 360.0;
    }
    let mut az = 0.5 * (az1 + az2);
    if az >= 360.0 {
        az -= 360.0;
    }

    CombinedRadial {
        az,
        phi,
        rho,
        z,
        vel: mean_vec(&a.vel, &b.vel),
        spw: mean_vec(&a.spw, &b.spw),
        dr0: a.dr0,
        dg: a.dg,
        zr0: a.zr0,
        zg: a.zg,
    }
}

// ── The preprocessor proper (cpc004/tsk011) ──────────────────────────────────

/// `Unfold_PhiDP`, transcribed: the historical median of the previous 30
/// qualifying gates decides whether φ folded, past gate 240 with enough
/// valid data accumulated. B21 parameterizes the qualifying test
/// (`filtering_data`/`filtering_data_threshold`): the legacy pair is
/// (ρ, [`UNFOLD_MIN_RHO`]), the `metsignal_processing = ON` pair is the
/// cleaned met signal with [`MET_SIG_THRESHOLD`].
pub(crate) fn unfold_phidp(phi: &mut [f64], filter: &[f64], thresh: f64, init_fdp: f64) {
    let n = phi.len();
    let mut unfolded = vec![f64::NAN; n];
    let mut valid_data = 0usize;
    let mut hist_med = init_fdp;
    let mut hist = Vec::with_capacity(HIST_WINDOW);
    for i in 0..n {
        if phi[i].is_nan() {
            continue;
        }
        let f_i = filter.get(i).copied().unwrap_or(f64::NAN);
        if f_i >= thresh {
            valid_data += 1;
        }
        if i >= HIST_WINDOW {
            hist.clear();
            for j in 1..=HIST_WINDOW {
                let f = filter.get(i - j).copied().unwrap_or(f64::NAN);
                if f >= thresh && !unfolded[i - j].is_nan() {
                    hist.push(unfolded[i - j]);
                }
            }
            if hist.len() > HIST_COUNT_THRESH {
                let sd = sample_stddev(&hist);
                if sd < HIST_MAX_STDDEV {
                    hist_med = upper_median(&mut hist);
                }
            }
        }
        let phi_diff = (phi[i] - hist_med).abs();
        let single = phi[i] + FOLD_DEG;
        let double = phi[i] + 2.0 * FOLD_DEG;
        let mut flag = 0u8;
        if phi_diff >= FOLD_DEG / 2.0 && valid_data > UNFOLD_MIN_VALID && i > UNFOLD_START_BIN {
            let single_diff = (hist_med - single).abs();
            let double_diff = (hist_med - double).abs();
            if phi_diff > single_diff {
                flag = 1;
            }
            if single_diff > double_diff {
                flag = 2;
            }
        }
        unfolded[i] = match flag {
            1 => single,
            2 => double,
            _ => phi[i],
        };
    }
    phi.copy_from_slice(&unfolded);
}

/// Non-biased standard deviation, `Standard_deviation`'s formula.
pub(crate) fn sample_stddev(data: &[f64]) -> f64 {
    let n = data.len();
    if n <= 1 {
        return 0.0;
    }
    let (mut sum, mut sq) = (0.0, 0.0);
    for &d in data {
        sum += d;
        sq += d * d;
    }
    let mean = sum / n as f64;
    let var = (sq - mean * mean * n as f64) / (n - 1) as f64;
    var.max(0.0).sqrt()
}

/// The upper median (`(low + high + 1) / 2`), what `DPPT_med_filter`
/// selects. Sorts its scratch input.
pub(crate) fn upper_median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// `DPPT_average_filter`: centred running mean over `w` gates, missing
/// gates skipped, windows truncated at the radial ends.
pub(crate) fn average_filter(input: &[f64], w: usize) -> Vec<f64> {
    let n = input.len();
    let hw = (w / 2) as isize;
    (0..n as isize)
        .map(|i| {
            let mut sum = 0.0;
            let mut cnt = 0usize;
            for j in (i - hw).max(0)..=(i + hw).min(n as isize - 1) {
                let v = input[j as usize];
                if !v.is_nan() {
                    sum += v;
                    cnt += 1;
                }
            }
            if cnt > 0 { sum / cnt as f64 } else { f64::NAN }
        })
        .collect()
}

/// `DPPT_median_filter`: centred running median (upper for even counts).
pub(crate) fn median_filter(input: &[f64], w: usize) -> Vec<f64> {
    let n = input.len();
    let hw = (w / 2) as isize;
    let mut buf = Vec::with_capacity(w);
    (0..n as isize)
        .map(|i| {
            buf.clear();
            for j in (i - hw).max(0)..=(i + hw).min(n as isize - 1) {
                let v = input[j as usize];
                if !v.is_nan() {
                    buf.push(v);
                }
            }
            if buf.is_empty() {
                f64::NAN
            } else {
                upper_median(&mut buf)
            }
        })
        .collect()
}

/// `Is_high_atten_radial`: strong-signal gates past bin 180 that look like
/// attenuation (moving, decorrelated, wide) — more than 10 of them flags
/// the radial.
pub(crate) fn is_high_attenuation_radial(z: &[f64], vel: &[f64], spw: &[f64], rho: &[f64]) -> bool {
    let n = z.len().min(vel.len()).min(spw.len()).min(rho.len());
    let mut count = 0usize;
    for i in ART_START_BIN..n {
        if !z[i].is_nan()
            && (ART_MIN_Z..=ART_MAX_Z).contains(&z[i])
            && !vel[i].is_nan()
            && vel[i].abs() >= ART_V
            && !rho[i].is_nan()
            && rho[i] <= ART_CORR
            && spw[i] > ART_MIN_SW
        {
            count += 1;
        }
    }
    count > ART_COUNT
}

/// Maximal runs of meteorological gates, inclusive bounds.
pub(crate) fn meteo_groups(flag: &[bool]) -> Vec<(usize, usize)> {
    let mut groups = Vec::new();
    let mut start: Option<usize> = None;
    for (i, &f) in flag.iter().enumerate() {
        match (f, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                groups.push((s, i - 1));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        groups.push((s, flag.len() - 1));
    }
    groups
}

/// `Interpolate`: bridge the gaps between valid meteorological groups
/// (size ≥ `w`) linearly, ramp the leading stretch from `init_fdp`, hold
/// the trailing stretch constant. The output has no missing gates unless
/// there is no valid group at all, in which case it is `init_fdp`
/// everywhere.
pub(crate) fn interpolate(
    input: &[f64],
    w: usize,
    groups: &[(usize, usize)],
    init_fdp: f64,
) -> Vec<f64> {
    let n = input.len();
    let hw = w / 2;
    let mut out = input.to_vec();
    let mut cnt = 0usize;
    let mut pre = 0usize;
    for j in 0..=groups.len() {
        let (begbin, endbin, begphi, endphi);
        if j < groups.len() {
            let (gb, ge) = groups[j];
            if ge - gb + 1 < w {
                continue;
            }
            if cnt == 0 {
                if gb == 0 {
                    (begbin, endbin, begphi, endphi) = (0, 0, 0.0, 0.0);
                } else {
                    begbin = 0;
                    endbin = gb + hw;
                    begphi = init_fdp;
                    endphi = input[endbin];
                }
            } else {
                begbin = groups[pre].1 - hw;
                endbin = gb + hw;
                begphi = input[begbin];
                endphi = input[endbin];
            }
        } else {
            if cnt == 0 {
                break;
            }
            begbin = groups[pre].1 - hw;
            endbin = n - 1;
            begphi = out[begbin];
            endphi = begphi;
        }
        if endbin > begbin {
            let slope = (endphi - begphi) / (endbin - begbin) as f64;
            for (k, cell) in out[begbin..=endbin].iter_mut().enumerate() {
                *cell = slope * k as f64 + begphi;
            }
        }
        pre = j;
        cnt += 1;
    }
    if cnt == 0 {
        out.fill(init_fdp);
    }
    out
}

/// Half the least-squares slope of φ over the `w`-gate window centred on
/// each gate (shrunk at the radial ends): `Calculate_kdp`'s
/// `6/(g·m(m²−1))·Σ jφ` closed form, which the general
/// `Calculate_lls_kdp` reduces to — the slope does not depend on where the
/// window is centred, only on the gate spacing.
pub(crate) fn kdp_from_phi(phi: &[f64], w: usize, g_km: f64) -> Vec<f64> {
    let n = phi.len();
    let hw = (w / 2) as isize;
    (0..n as isize)
        .map(|i| {
            let lo = (i - hw).max(0);
            let hi = (i + hw).min(n as isize - 1);
            let m = (hi - lo + 1) as f64;
            if m <= 1.0 {
                return f64::NAN;
            }
            let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
            for j in lo..=hi {
                let x = (j - lo) as f64 * g_km;
                let y = phi[j as usize];
                sx += x;
                sy += y;
                sxx += x * x;
                sxy += x * y;
            }
            0.5 * (m * sxy - sx * sy) / (m * sxx - sx * sx)
        })
        .collect()
}

// ── Initial system PhiDP estimator (calc_system_PhiDP.c) ─────────────────────

/// Sort φ values 360°-aware (`qsort_360`): if the sorted spread exceeds
/// 200°, values under 270° are lifted a fold and the array re-sorted.
pub(crate) fn sort_360(values: &mut [f64]) {
    values.sort_by(f64::total_cmp);
    if let (Some(first), Some(last)) = (values.first(), values.last())
        && last - first > ISDP_CROSSOVER
    {
        for v in values.iter_mut() {
            if *v < ISDP_ADJUST {
                *v += FOLD_DEG;
            }
        }
        values.sort_by(f64::total_cmp);
    }
}

/// One radial's system-phase sample: the 360°-aware median of the first run
/// of 11 consecutive high-quality gates, `None` when the run starts inside
/// 25 km, contains suspect gates, or never happens.
pub(crate) fn radial_system_phi(phi: &[f64], rho: &[f64], z: &[f64]) -> Option<f64> {
    let mut run: Vec<f64> = Vec::with_capacity(ISDP_MIN_ECHO);
    for (i, &phi_i) in phi.iter().enumerate() {
        let rho_i = rho.get(i).copied().unwrap_or(f64::NAN);
        let z_i = z.get(i).copied().unwrap_or(f64::NAN);
        if !phi_i.is_nan() && rho_i >= ISDP_MIN_RHO && z_i >= ISDP_Z_MIN {
            run.push(phi_i);
            if run.len() == ISDP_MIN_ECHO {
                if i + 1 - ISDP_MIN_ECHO < ISDP_TOO_CLOSE {
                    return None;
                }
                for j in 0..ISDP_MIN_ECHO {
                    let zz = z.get(i - j).copied().unwrap_or(f64::NAN);
                    let rr = rho.get(i - j).copied().unwrap_or(f64::NAN);
                    if zz >= ISDP_Z_REJECT || rr > 1.0 {
                        return None;
                    }
                }
                sort_360(&mut run);
                let mut med = run[ISDP_MIN_ECHO / 2];
                if med >= FOLD_DEG {
                    med -= FOLD_DEG;
                }
                return Some(med);
            }
        } else {
            run.clear();
        }
    }
    None
}

/// The estimator's closing step: with at least 40 queued radial phases,
/// the sorted (360°-aware) queue's `round(n/20)`-th entry — the
/// low-percentile reading that stands clear of precipitation-accumulated
/// phase.
pub(crate) fn isdp_from_queue(mut queue: Vec<f64>) -> Option<f64> {
    if queue.len() < ISDP_MIN_SAMPLES {
        return None;
    }
    let idx = (queue.len() as f64 / 20.0).round() as usize;
    sort_360(&mut queue);
    let mut v = queue[idx.min(queue.len() - 1)];
    if v >= FOLD_DEG {
        v -= FOLD_DEG;
    }
    Some(v)
}

/// The documented estimator over one sweep's recombined radials: queue
/// per-radial phases (capped at 200) and take the percentile.
pub(crate) fn estimate_isdp(radials: &[CombinedRadial]) -> Option<f64> {
    let mut queue: Vec<f64> = Vec::new();
    for radial in radials {
        if queue.len() >= ISDP_MAX_QUEUE {
            break;
        }
        if let Some(p) = radial_system_phi(&radial.phi, &radial.rho, &radial.z) {
            queue.push(p);
        }
    }
    isdp_from_queue(queue)
}

// ── Extensions for the HCA chain ─────────────────────────────────────────────
//
// Everything below is consumed by [`crate::hca`] only. The KDP chain above is
// untouched: `combine_sweep` keeps returning the lean [`CombinedRadial`] the
// KDP pipeline was validated with, and the HCA chain gets the same
// recombination plus the pieces dpprep computes that KDP never reads — the
// recombined ZDR (`Recomb_dp_data`'s `Zdrc = 10·log10(phc/pvc)`), the radial
// elevation, and the texture filter (`DPPT_std_filter`).

/// One recombined radial with the fields the HCA chain needs on top of the
/// KDP chain's [`CombinedRadial`].
pub(crate) struct DpCombined {
    pub(crate) base: CombinedRadial,
    /// Recombined differential reflectivity at the DP gates, dB.
    pub(crate) zdr: Vec<f64>,
    /// Elevation angle, degrees (the first pair member's, as the RPG keeps
    /// the saved radial's header).
    pub(crate) elev: f64,
}

/// The ZDR of one recombined pair per `Recomb_dp_data`: the power ratio of
/// the (fallback-aware) averaged horizontal and vertical powers,
/// `10·log10(phc/pvc)` — `ZDR_CAL` is 0 ("already applied at the RDA").
/// The plain-mean A/B variant averages the two ZDRs directly.
fn combine_pair_zdr(a: &DpInput, b: &DpInput, coherent: bool) -> Vec<f64> {
    let n = a.phi.len().min(b.phi.len());
    (0..n)
        .map(|i| {
            let zdra = a.zdr.get(i).copied().unwrap_or(f64::NAN);
            let zdrb = b.zdr.get(i).copied().unwrap_or(f64::NAN);
            if !coherent {
                return nan_mean(zdra, zdrb);
            }
            let pha = dp_gate_power(a, i);
            let phb = dp_gate_power(b, i);
            let pva = if pha.is_nan() || zdra.is_nan() {
                f64::NAN
            } else {
                pha / 10f64.powf(0.1 * zdra)
            };
            let pvb = if phb.is_nan() || zdrb.is_nan() {
                f64::NAN
            } else {
                phb / 10f64.powf(0.1 * zdrb)
            };
            let phc = nan_mean(pha, phb);
            let pvc = nan_mean(pva, pvb);
            if phc.is_nan() || pvc.is_nan() {
                f64::NAN
            } else {
                10.0 * (phc / pvc).log10()
            }
        })
        .collect()
}

/// [`combine_sweep`] with the HCA extras: the same pairing decisions
/// (`combine_radials.c`), the same recombination, plus the recombined ZDR
/// and the radial elevation.
pub(crate) fn combine_sweep_dp(inputs: &[DpInput], coherent: bool) -> Vec<DpCombined> {
    let single = |input: &DpInput| DpCombined {
        base: CombinedRadial::single(input),
        zdr: input.zdr.clone(),
        elev: input.elev,
    };
    let mut out = Vec::with_capacity(inputs.len() / 2 + 1);
    let mut saved: Option<&DpInput> = None;
    for input in inputs {
        if !input.half_degree {
            out.push(DpCombined {
                base: CombinedRadial::passthrough(input),
                zdr: input.zdr.clone(),
                elev: input.elev,
            });
            continue;
        }
        match saved.take() {
            None => saved = Some(input),
            Some(first) => {
                // The pairing conditions mirror `combine_sweep` exactly.
                let mut diff = input.az - first.az;
                if diff < -180.0 {
                    diff += 360.0;
                }
                let fazi = first.az - first.az.trunc();
                let pairable = (0.0..=0.75).contains(&diff)
                    && fazi <= 0.5
                    && first.phi.len() == input.phi.len()
                    && (first.dr0 - input.dr0).abs() < 1e-6;
                if pairable {
                    out.push(DpCombined {
                        base: combine_pair(first, input, coherent),
                        zdr: combine_pair_zdr(first, input, coherent),
                        elev: first.elev,
                    });
                } else {
                    out.push(single(first));
                    saved = Some(input);
                }
            }
        }
    }
    if let Some(first) = saved {
        out.push(single(first));
    }
    out
}

/// `DPPT_std_filter`: the windowed non-biased standard deviation of
/// `input − smoothed` — the texture fields SD(Z) (window 5, differences
/// beyond ±50 dB excluded) and SD(ΦDP) (window 9, ±100°). Gates whose
/// window collects fewer than `w/2` (or 2) qualifying pairs are undefined.
pub(crate) fn std_filter(input: &[f64], smoothed: &[f64], w: usize, max_diff: f64) -> Vec<f64> {
    let n = input.len();
    let hw = (w / 2) as isize;
    (0..n as isize)
        .map(|i| {
            let mut sum = 0.0;
            let mut sq = 0.0;
            let mut cnt = 0usize;
            for j in (i - hw).max(0)..=(i + hw).min(n as isize - 1) {
                let v1 = input[j as usize];
                let v2 = smoothed.get(j as usize).copied().unwrap_or(f64::NAN);
                if v1.is_nan() || v2.is_nan() {
                    continue;
                }
                let d = v1 - v2;
                if d <= max_diff && d >= -max_diff {
                    sum += d;
                    sq += d * d;
                    cnt += 1;
                }
            }
            if cnt < hw as usize || cnt < 2 {
                f64::NAN
            } else {
                let mean = sum / cnt as f64;
                let var = (sq - mean * mean * cnt as f64) / (cnt as f64 - 1.0);
                var.max(0.0).sqrt()
            }
        })
        .collect()
}

// ── Met Signal processing (findMetSignal.cpp / cleanMetSignal.cpp, B21) ─────

/// `dpprep.alg`'s `metsignal_threshold` fleet default.
pub(crate) const MET_SIG_THRESHOLD: f64 = 80.0;
/// `cleanMetSignal`'s despeckle span (`despeckle_seg_size` at the call site).
const MET_SIG_DESPECKLE_SPAN: f64 = 2.0;
/// `cleanMetSignal`'s dilate radius.
const MET_SIG_DILATE: usize = 1;

/// `trap4point`: the plateau-inclusive trapezoid with the degenerate guard.
/// Shared with the HCA chain's HSDA (`HailSize.cpp` links the same
/// `trapazoid.cpp`).
pub(crate) fn trap4(input: f64, x1: f64, x2: f64, x3: f64, x4: f64) -> f64 {
    if x2 - x1 < 0.0 || x3 - x2 < 0.0 || x4 - x3 < 0.0 {
        return 0.0;
    }
    if input >= x2 && input <= x3 {
        1.0
    } else if input <= x1 || input >= x4 {
        0.0
    } else if input > x1 && input < x2 {
        (input - x1) / (x2 - x1)
    } else if input > x3 && input < x4 {
        (x4 - input) / (x4 - x3)
    } else {
        0.0 // NaN input matches no branch in the C either
    }
}

/// `nint`: round half away from zero, the source's own helper.
fn nint(v: f64) -> i64 {
    if v >= 0.0 {
        (v + 0.5) as i64
    } else {
        (v - 0.5) as i64
    }
}

/// `findMetSignal`: the fuzzy met-signal per gate, 0–100. All inputs are at
/// the DP gates in the NaN domain — `z` and `snr` sampled there by the
/// caller (the C indexes its Z-gate arrays directly, the geometries being
/// identical) — with `phi` the **raw** phase (pre-unfold) and `zdr`/`rho`
/// raw, exactly as `Dpp_process_radial` calls it. `snr` is from the 3-gate
/// smoothed Z.
pub(crate) fn find_met_signal(
    z: &[f64],
    vel: &[f64],
    zdr: &[f64],
    rho: &[f64],
    phi: &[f64],
    snr: &[f64],
) -> Vec<f64> {
    let n = phi.len();
    let std_phi = std_filter(phi, &average_filter(phi, SHORT_GATE), SHORT_GATE, 100.0);
    let std_zdr = std_filter(zdr, &average_filter(zdr, SHORT_GATE), SHORT_GATE, 100.0);
    let std_rho = std_filter(rho, &average_filter(rho, SHORT_GATE), SHORT_GATE, 100.0);

    const Z_WEIGHT: f64 = 2.0;
    const RHV_WEIGHT: f64 = 1.0;
    const V_WEIGHT: f64 = 1.0;
    const STD_ZDR_WEIGHT: f64 = 2.0;
    const STD_PHI_WEIGHT: f64 = 2.0;
    const STD_RHO_WEIGHT: f64 = 1.0;

    let mut met = vec![0.0f64; n];
    for i in 0..n {
        let snr_i = snr.get(i).copied().unwrap_or(f64::NAN);
        // SNR below 0 dB (or missing — the sentinel is negative) is not met.
        if snr_i.is_nan() || snr_i < 0.0 {
            continue;
        }
        let mut sv = 0.0f64;
        let mut weight = 0.0f64;
        let mut num_s = 0usize;
        let z_i = z.get(i).copied().unwrap_or(f64::NAN);
        if z_i.is_finite() {
            sv += Z_WEIGHT * trap4(z_i, 10.0, 30.0, f64::INFINITY, f64::INFINITY);
            num_s += 1;
            weight += Z_WEIGHT;
        }
        let rho_i = rho.get(i).copied().unwrap_or(f64::NAN);
        if rho_i.is_finite() {
            sv += RHV_WEIGHT * trap4(rho_i, 0.75, 0.9, f64::INFINITY, f64::INFINITY);
            num_s += 1;
            weight += RHV_WEIGHT;
        }
        let v_i = vel.get(i).copied().unwrap_or(f64::NAN);
        if v_i.is_finite() {
            // The source spells the inverted trapezoid exactly this way.
            sv += 1.0 - V_WEIGHT * trap4(v_i, -1.5, -1.0, 1.0, 1.5);
            num_s += 1;
            weight += V_WEIGHT;
        }
        if std_phi[i].is_finite() {
            sv += STD_PHI_WEIGHT * trap4(std_phi[i], 0.0, 0.0, 10.0, 20.0);
            num_s += 1;
            weight += STD_PHI_WEIGHT;
        }
        if std_zdr[i].is_finite() {
            sv += STD_ZDR_WEIGHT * trap4(std_zdr[i], 0.0, 0.0, 1.0, 2.0);
            num_s += 1;
            weight += STD_ZDR_WEIGHT;
        }
        if std_rho[i].is_finite() {
            sv += STD_RHO_WEIGHT * trap4(std_rho[i], 0.0, 0.0, 0.02, 0.04);
            num_s += 1;
            weight += STD_RHO_WEIGHT;
        }
        if num_s < 4 {
            continue; // met stays 0
        }
        met[i] = nint(sv / weight * 100.0) as f64;
        // Hard limits: a qualifying signal with depressed ρ or extreme ZDR
        // (either missing — the C sentinel fails these tests too) is not met.
        let zdr_i = zdr.get(i).copied().unwrap_or(f64::NAN);
        let rho_fails = rho_i.is_nan() || rho_i < 0.65;
        let zdr_fails = zdr_i.is_nan() || !(-4.5..=4.5).contains(&zdr_i);
        if met[i] >= MET_SIG_THRESHOLD && (rho_fails || zdr_fails) {
            met[i] = 0.0;
        }
    }
    met
}

/// `cleanMetSignal`: dilate the ≥ threshold signal by one gate, trim it back
/// at run edges, then despeckle runs spanning fewer than 2 bins down to
/// `threshold − 1.5`.
pub(crate) fn clean_met_signal(met: &mut [f64], threshold: f64) {
    let n = met.len();
    if n <= 2 * MET_SIG_DILATE {
        return;
    }
    let mut temp = met.to_vec();

    // Dilate.
    for (b, &m) in met
        .iter()
        .enumerate()
        .take(n - MET_SIG_DILATE)
        .skip(MET_SIG_DILATE)
    {
        if m.is_finite() && m >= threshold {
            for cell in temp
                .iter_mut()
                .take(b + MET_SIG_DILATE + 1)
                .skip(b - MET_SIG_DILATE)
            {
                if *cell < threshold {
                    *cell = threshold;
                }
            }
        }
    }

    // Trim back to the original values at run transitions.
    let mut signal = temp[0] >= threshold;
    for b in MET_SIG_DILATE..(n - MET_SIG_DILATE) {
        if temp[b] >= threshold {
            if !signal {
                // New data found: trim [b, b + dilate) to the original.
                temp[b..b + MET_SIG_DILATE].copy_from_slice(&met[b..b + MET_SIG_DILATE]);
            }
            signal = true;
        } else {
            if signal {
                // Data stopped: trim [b − dilate, b) to the original.
                temp[b - MET_SIG_DILATE..b].copy_from_slice(&met[b - MET_SIG_DILATE..b]);
            }
            signal = false;
        }
    }
    met.copy_from_slice(&temp);

    // Despeckle isolated runs; the trailing run is never flushed, per the C.
    let (mut beg, mut end): (Option<usize>, Option<usize>) = (None, None);
    for b in 0..n {
        if met[b].is_finite() && met[b] >= threshold {
            if beg.is_none() {
                beg = Some(b);
            }
            end = Some(b);
        } else {
            if let (Some(bi), Some(ei)) = (beg, end)
                && ((ei as f64) - (bi as f64)).abs() < MET_SIG_DESPECKLE_SPAN
            {
                for cell in met.iter_mut().take(ei + 1).skip(bi) {
                    *cell = threshold - 1.5;
                }
            }
            beg = None;
            end = None;
        }
    }
}

// ── Reflectivity CAPPI (ReflCAPPI.cpp, B21) ──────────────────────────────────

/// `CAPPI_height` / `CAPPI_threshold` fleet defaults (`dpprep.alg`).
pub(crate) const CAPPI_HEIGHT_KM: f64 = 3.0;
pub(crate) const CAPPI_REFL_THRESH_DBZ: f64 = 11.0;
/// `MIN_CAPPI_RANGE`: no update inside this range.
const CAPPI_MIN_RANGE_KM: f64 = 30.0;
/// `METSIG_CAPPI_OVERRIDE`.
const CAPPI_OVERRIDE: f64 = 99.5;
/// `checkCAPPIHeight`'s height tolerance.
const CAPPI_TOLERANCE_KM: f64 = 0.2;
const CAPPI_NUM_GATES: usize = 300;
const CAPPI_AZ_OFFSET: f64 = 0.25;
const CAPPI_UNSET_HEIGHT: f64 = 9999.0;

/// `computeHeight_b5`: beam height above the radar, km, on the B5 spec's
/// `1.21·6371 km` effective Earth — the same model `RPGCS_height` and the
/// MLDA use.
pub(crate) fn height_b5_km(elev_deg: f64, range_km: f64) -> f64 {
    let s = elev_deg.to_radians().sin();
    range_km * s + range_km * range_km / (2.0 * 1.21 * 6371.0)
}

/// The persistent reflectivity CAPPI the met-signal chain consults
/// (`ReflCAPPI.cpp`): raw Z near 3 km ARL on a 360° × 1 km grid.
///
/// Operationally this survives across volumes with a 15-minute freshness
/// window; here it is rebuilt from one volume's own ≥ 1° sweeps (every
/// radial inside a volume is within that window), which stands one volume
/// fresher than the grid the RPG consulted for the same cut.
pub struct ReflCappi {
    /// `[az][1-km range]`, raw dBZ, `NaN` empty.
    z: Vec<[f64; CAPPI_NUM_GATES]>,
    /// The actual beam height stored per range column (`CAPPI_Heights`).
    heights: [f64; CAPPI_NUM_GATES],
}

impl Default for ReflCappi {
    fn default() -> Self {
        Self::new()
    }
}

impl ReflCappi {
    pub(crate) fn new() -> Self {
        Self {
            z: vec![[f64::NAN; CAPPI_NUM_GATES]; 360],
            heights: [CAPPI_UNSET_HEIGHT; CAPPI_NUM_GATES],
        }
    }

    /// `check_Az` + the double-`nint` azimuth indexing of the source.
    fn az_index(az_deg: f64) -> usize {
        let check = |a: f64| a.rem_euclid(360.0);
        let inner = nint(check(az_deg - CAPPI_AZ_OFFSET)) as f64;
        (nint(check(inner)) as usize) % 360
    }

    /// `update_CAPPI`: store this radial's raw Z into the columns whose beam
    /// height sits nearest the CAPPI height. Radials under 1.0° never update.
    pub(crate) fn update_radial(
        &mut self,
        elev_deg: f64,
        az_deg: f64,
        zr0: f64,
        zg: f64,
        z: &[f64],
    ) {
        if elev_deg < 1.0 {
            return;
        }
        let az = Self::az_index(az_deg);
        let cos_e = elev_deg.to_radians().cos();

        // checkCAPPIHeight: the UseFLAG pass, mutating the height store.
        let mut use_flag = vec![false; z.len()];
        for (b, flag) in use_flag.iter_mut().enumerate() {
            let range = zr0 + b as f64 * zg;
            let h = height_b5_km(elev_deg, range);
            let ri = nint((range - zr0) * cos_e);
            if ri >= CAPPI_NUM_GATES as i64 {
                break;
            }
            let ri = ri.max(0) as usize;
            let stored = self.heights[ri];
            if (h - CAPPI_HEIGHT_KM).abs() <= (stored - CAPPI_HEIGHT_KM).abs() + CAPPI_TOLERANCE_KM
                || stored == CAPPI_UNSET_HEIGHT
            {
                self.heights[ri] = h;
                *flag = true;
            }
            if range < CAPPI_MIN_RANGE_KM {
                *flag = false;
            }
        }

        for (b, &v) in z.iter().enumerate() {
            let range = zr0 + b as f64 * zg;
            let ri = nint((range - zr0) * cos_e);
            if ri >= CAPPI_NUM_GATES as i64 {
                break;
            }
            if use_flag[b] {
                self.z[az][ri.max(0) as usize] = v;
            }
        }
    }

    /// `apply_CAPPI`: rescue weak met-signal gates (0 < met < threshold)
    /// below the CAPPI height where the stored Z exceeds 11 dBZ. Radials
    /// under 1.0° are never touched — the lowest split cuts see no CAPPI.
    pub(crate) fn apply_radial(
        &self,
        elev_deg: f64,
        az_deg: f64,
        dr0: f64,
        dg: f64,
        met: &mut [f64],
    ) {
        if elev_deg < 1.0 {
            return;
        }
        let az = Self::az_index(az_deg);
        let cos_e = elev_deg.to_radians().cos();
        for (b, m) in met.iter_mut().enumerate() {
            let range = dr0 + b as f64 * dg;
            if height_b5_km(elev_deg, range) >= CAPPI_HEIGHT_KM {
                break;
            }
            let ri = nint((range - dr0) * cos_e);
            if ri >= CAPPI_NUM_GATES as i64 {
                break;
            }
            let stored = self.z[az][ri.max(0) as usize];
            if stored.is_finite()
                && stored > CAPPI_REFL_THRESH_DBZ
                && *m < MET_SIG_THRESHOLD
                && *m > 0.0
            {
                *m = CAPPI_OVERRIDE;
            }
        }
    }
}

/// Resample a derived field onto the 360° × 230 km comparison grid, cell for
/// cell the way [`crate::twin::compare::tally_packet`] resamples the Level
/// III twin: the radial nearest the cell centre `az + 0.5°` (bounded by the
/// radial's own angular claim), and per 1-km cell the gate whose centre falls
/// nearest the cell centre, earlier gate winning ties. Shared by the derived
/// products' `to_polar_grid` implementations.
pub(crate) fn resample_to_polar_grid(
    values: &[Vec<f32>],
    azimuths_deg: &[f64],
    first_gate_km: f64,
    gate_interval_km: f64,
    radial_width_deg: f64,
) -> Vec<Vec<f32>> {
    use crate::volumetric::RANGE_BINS;
    let mut grid = vec![vec![f32::NAN; RANGE_BINS]; 360];
    if values.is_empty() {
        return grid;
    }

    let n_gates = values.iter().map(Vec::len).max().unwrap_or(0);
    let mut gate_for_bin: Vec<Option<usize>> = vec![None; RANGE_BINS];
    let mut best = vec![f64::INFINITY; RANGE_BINS];
    for j in 0..n_gates {
        let centre = first_gate_km + j as f64 * gate_interval_km;
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

    let circular_distance = |a: f64, b: f64| -> f64 {
        let mut d = (a - b).rem_euclid(360.0);
        if d > 180.0 {
            d = 360.0 - d;
        }
        d
    };
    let cover = 0.5 * radial_width_deg + 0.05;
    for (az, row) in grid.iter_mut().enumerate() {
        let centre = az as f64 + 0.5;
        let ri = azimuths_deg
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                circular_distance(**a, centre).total_cmp(&circular_distance(**b, centre))
            })
            .filter(|(_, a)| circular_distance(**a, centre) <= cover)
            .map(|(i, _)| i);
        let Some(ri) = ri else { continue };
        let radial = &values[ri];
        for (r, cell) in row.iter_mut().enumerate() {
            if let Some(j) = gate_for_bin[r]
                && let Some(&v) = radial.get(j)
            {
                *cell = v;
            }
        }
    }
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── trap4point ───────────────────────────────────────────────────────────

    /// The trapezoid the met signal and the HSDA share: plateau inclusive,
    /// linear shoulders, zero at and beyond the outer points, degenerate
    /// rows and NaN inputs read 0.
    #[test]
    fn trap4_matches_trapazoid_cpp() {
        assert_eq!(trap4(2.0, 0.0, 1.0, 3.0, 5.0), 1.0);
        assert_eq!(trap4(1.0, 0.0, 1.0, 3.0, 5.0), 1.0, "x2 is plateau");
        assert_eq!(trap4(3.0, 0.0, 1.0, 3.0, 5.0), 1.0, "x3 is plateau");
        assert_eq!(trap4(0.5, 0.0, 1.0, 3.0, 5.0), 0.5);
        assert_eq!(trap4(4.0, 0.0, 1.0, 3.0, 5.0), 0.5);
        assert_eq!(trap4(0.0, 0.0, 1.0, 3.0, 5.0), 0.0);
        assert_eq!(trap4(5.0, 0.0, 1.0, 3.0, 5.0), 0.0);
        // The half-open trapezoids the met signal uses: x1 == x2 reads 1
        // at the shared point, and an infinite top never falls off.
        assert_eq!(trap4(0.0, 0.0, 0.0, 10.0, 20.0), 1.0);
        assert_eq!(trap4(1e12, 10.0, 30.0, f64::INFINITY, f64::INFINITY), 1.0);
        assert_eq!(trap4(1.0, 3.0, 1.0, 4.0, 5.0), 0.0, "degenerate row");
        assert_eq!(
            trap4(f64::NAN, 0.0, 1.0, 3.0, 5.0),
            0.0,
            "NaN matches no branch",
        );
    }

    // ── findMetSignal ────────────────────────────────────────────────────────

    fn uniform(n: usize, v: f64) -> Vec<f64> {
        vec![v; n]
    }

    /// A clean rain radial (strong Z, high ρ, smooth φ/ZDR/ρ, no velocity)
    /// scores 100; SNR under 0 dB scores 0 outright.
    #[test]
    fn find_met_signal_scores_clean_rain_at_100() {
        let n = 40;
        let met = find_met_signal(
            &uniform(n, 40.0),
            &[],
            &uniform(n, 0.5),
            &uniform(n, 0.99),
            &uniform(n, 60.0),
            &uniform(n, 30.0),
        );
        assert!(met.iter().all(|&m| m == 100.0), "got {:?}", &met[..5]);

        let met = find_met_signal(
            &uniform(n, 40.0),
            &[],
            &uniform(n, 0.5),
            &uniform(n, 0.99),
            &uniform(n, 60.0),
            &uniform(n, -3.0),
        );
        assert!(met.iter().all(|&m| m == 0.0), "SNR < 0 is never met");
    }

    /// The hard limits: a gate whose vote clears the threshold but whose ρ
    /// sits under 0.65 (or ZDR outside ±4.5) zeroes — biology cannot ride
    /// smooth textures in.
    #[test]
    fn find_met_signal_hard_limits_zero_qualifying_gates() {
        let n = 40;
        // ρ = 0.5: the ρ trapezoid contributes 0 but the five other votes
        // give 7/8 = 88 ≥ 80 — the hard limit must kill it.
        let met = find_met_signal(
            &uniform(n, 40.0),
            &[],
            &uniform(n, 0.5),
            &uniform(n, 0.50),
            &uniform(n, 60.0),
            &uniform(n, 30.0),
        );
        assert!(met.iter().all(|&m| m == 0.0), "got {:?}", &met[..5]);
        // Same construction with ρ = 0.70: above the hard floor, below the
        // trapezoid's 0.75 foot — the vote stands at 88.
        let met = find_met_signal(
            &uniform(n, 40.0),
            &[],
            &uniform(n, 0.5),
            &uniform(n, 0.70),
            &uniform(n, 60.0),
            &uniform(n, 30.0),
        );
        assert!(met.iter().all(|&m| m == 88.0), "got {:?}", &met[..5]);
        // Extreme ZDR trips the other limb.
        let met = find_met_signal(
            &uniform(n, 40.0),
            &[],
            &uniform(n, 5.0),
            &uniform(n, 0.99),
            &uniform(n, 60.0),
            &uniform(n, 30.0),
        );
        assert!(met.iter().all(|&m| m == 0.0), "ZDR 5 dB is out of bounds");
    }

    /// Fewer than four present inputs scores 0 (the divide-by-zero guard).
    #[test]
    fn find_met_signal_needs_four_inputs() {
        let n = 40;
        // Only Z and SNR present: the textures of missing fields are
        // missing, ρ/ZDR missing → 1 vote < 4.
        let met = find_met_signal(
            &uniform(n, 40.0),
            &[],
            &uniform(n, f64::NAN),
            &uniform(n, f64::NAN),
            &uniform(n, f64::NAN),
            &uniform(n, 30.0),
        );
        assert!(met.iter().all(|&m| m == 0.0));
    }

    // ── cleanMetSignal ───────────────────────────────────────────────────────

    /// Runs spanning fewer than 2 bins despeckle to threshold − 1.5; a
    /// trailing run survives (the C never flushes it).
    #[test]
    fn clean_met_signal_despeckles_isolated_runs() {
        let t = MET_SIG_THRESHOLD;
        // An isolated single-gate signal: dilate spreads it to the two
        // neighbours, trim pulls both back, despeckle removes the residue.
        let mut met = vec![0.0, 0.0, 0.0, 95.0, 0.0, 0.0, 0.0];
        clean_met_signal(&mut met, t);
        assert!(met.iter().all(|&m| m < t), "got {met:?}");

        // A broad run survives intact in its interior.
        let mut met = vec![0.0, 0.0, 90.0, 90.0, 90.0, 90.0, 90.0, 90.0, 0.0, 0.0];
        clean_met_signal(&mut met, t);
        assert!(met[3..8].iter().all(|&m| m >= t), "got {met:?}");

        // A trailing run at the array end is never despeckled.
        let mut met = vec![0.0; 8];
        met[7] = 95.0;
        clean_met_signal(&mut met, t);
        assert_eq!(met[7], 95.0, "the C never flushes the trailing run");
    }

    // ── ReflCAPPI ────────────────────────────────────────────────────────────

    /// `height_b5`: the B5 spec's 1.21·6371 km model, hand-computed at
    /// 3.0° / 50 km: 50·sin(3°) + 2500/15417.82 = 2.778948 km.
    #[test]
    fn height_b5_matches_the_hand_computation() {
        assert!((height_b5_km(3.0, 50.0) - 2.778_948).abs() < 1e-4);
        assert_eq!(height_b5_km(0.0, 0.0), 0.0);
    }

    /// update/apply honour the guards: nothing under 1.0° elevation,
    /// no update inside 30 km, apply stops at the CAPPI height, and the
    /// rescue only lifts weak-but-nonzero signals over stored Z > 11 dBZ.
    #[test]
    fn refl_cappi_updates_and_rescues_per_the_source() {
        let mut cappi = ReflCappi::new();
        let n = 1200usize; // 0.25 km gates to 300 km
        let z: Vec<f64> = vec![20.0; n];

        // A 0.5° radial never updates.
        cappi.update_radial(0.5, 100.25, 0.125, 0.25, &z);
        assert!(cappi.z[100].iter().all(|v| v.is_nan()));

        // A 3.0° radial fills its azimuth's columns past 30 km.
        cappi.update_radial(3.0, 100.25, 0.125, 0.25, &z);
        let az = ReflCappi::az_index(100.25);
        assert_eq!(az, 100);
        assert!(cappi.z[100][29].is_nan(), "no update inside 30 km");
        assert_eq!(cappi.z[100][50], 20.0);

        // Apply on a 1.5° radial: weak-but-nonzero gates over stored
        // Z > 11 dBZ lift to 99.5, zero-signal gates stay zero, gates
        // inside 30 km see empty columns, and the loop stops at 3 km.
        let mut met = vec![50.0; n];
        met[200] = 0.0;
        cappi.apply_radial(1.5, 100.25, 0.125, 0.25, &mut met);
        assert_eq!(met[100], 50.0, "range 25 km: column never filled");
        assert_eq!(met[240], 99.5, "range 60 km: rescued");
        assert_eq!(met[200], 0.0, "a zero signal is never rescued");
        // Beyond the height cut (h ≥ 3 km at 1.5° is ~103 km) nothing moves.
        assert_eq!(met[500], 50.0, "range 125 km: beam above the CAPPI");

        // A 0.5° radial is never rescued at all.
        let mut met_low = vec![50.0; n];
        cappi.apply_radial(0.5, 100.25, 0.125, 0.25, &mut met_low);
        assert!(met_low.iter().all(|&m| m == 50.0));

        // A strong signal is never overridden.
        let mut met_hi = vec![90.0; n];
        cappi.apply_radial(1.5, 100.25, 0.125, 0.25, &mut met_hi);
        assert!(met_hi.iter().all(|&m| m == 90.0));

        // The tolerance rule: a later radial whose beam sits farther from
        // 3 km than the stored height (plus 0.2 km) does not overwrite —
        // at 50 km the 3.0° beam (2.78 km) beats the 8.0° one (7.28 km).
        let z25: Vec<f64> = vec![25.0; n];
        cappi.update_radial(8.0, 100.25, 0.125, 0.25, &z25);
        assert_eq!(
            cappi.z[100][50], 20.0,
            "the closer-to-CAPPI height keeps the column",
        );
    }

    /// The double-nint azimuth indexing with the 0.25° offset.
    #[test]
    fn refl_cappi_azimuth_indexing_wraps_the_offset() {
        assert_eq!(
            ReflCappi::az_index(0.1),
            0,
            "0.1 − 0.25 wraps to 359.85 → 360 → 0",
        );
        assert_eq!(ReflCappi::az_index(0.75), 1);
        assert_eq!(ReflCappi::az_index(359.75), 0);
        assert_eq!(ReflCappi::az_index(180.0), 180);
    }
}
