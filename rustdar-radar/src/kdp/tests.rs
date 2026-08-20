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
/// interior gates read exactly half the slope, 2.0 °/km, on both windows.
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

/// `Interpolate`'s tail rule: past the last valid group's `end − w/2` the
/// smoothed φ holds constant, so the last gate's KDP is exactly 0.
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

/// The 40 dBZ window switch, over a φ step across a missing-φ gap (gates
/// 150–179; φ 100 before, 200 after, ρ solid):
///
/// * 9-gate chain bridges `[145, 184]`, slope 100/39 °/gate; at gate 182
///   `kdp9 = (Σ j·φ)/30` over `[178, 186]` with the last two flat at 200:
///   `Σ j·c = 49`, `kdp9 = (100/39)·49/30 =` **4.188034** °/km;
/// * 25-gate chain bridges `[137, 192]`, slope 100/55; over `[170, 194]` the
///   ramp part cancels exactly (`Σ j(j+45) = 0` for j = −12..10), leaving the
///   two flat gates: `kdp25 = (20/11)·(11 + 12)·55/650 =` **3.538462** °/km.
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

/// RhoHV censoring runs on the 5-gate smoothed ρ: a ρ = 0.3 stretch at gates
/// 100–119 censors 98–121 (every gate whose window average dips under 0.9).
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

/// A ramp that crosses 360° past the unfold start (gate 260 = 65 km).
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

/// The coherent pair combination, hand-computed: a 20 dB power imbalance pulls
/// the average toward the strong radial (atan2 of the summed vectors: 10.0985°
/// for 10°/20° at 50/30 dBZ), and the fold seam averages circularly.
#[test]
fn coherent_recombination_is_circular_and_power_weighted() {
    let p5 = 10f64.powf(5.0); // 50 dBZ linear
    let p3 = 10f64.powf(3.0); // 30 dBZ linear
    let (phi, rho) = coherent_phi_rho((15.0, 15.0), (0.99, 0.99), (p3, p3), (p3, p3));
    assert!((phi - 15.0).abs() < 1e-9, "got {phi}");
    assert!(
        (rho - 0.99).abs() < 1e-9,
        "identical inputs keep rho: {rho}"
    );
    // Equal powers: the 10° phase spread shortens the mean vector, so ρ
    // contracts by cos(5°).
    let (phi, rho) = coherent_phi_rho((10.0, 20.0), (0.99, 0.99), (p3, p3), (p3, p3));
    assert!((phi - 15.0).abs() < 1e-9, "got {phi}");
    assert!(
        (rho - 0.99 * 5f64.to_radians().cos()).abs() < 1e-9,
        "a phase spread must decorrelate: {rho}",
    );
    let (phi, _) = coherent_phi_rho((10.0, 20.0), (0.99, 0.99), (p5, p3), (p5, p3));
    assert!((phi - 10.0985).abs() < 1e-3, "got {phi}");
    let (phi, _) = coherent_phi_rho((359.0, 1.0), (0.99, 0.99), (p3, p3), (p3, p3));
    assert!(phi.min(360.0 - phi) < 1e-9, "got {phi}");
    let (phi, _) = coherent_phi_rho((10.0, 20.0), (0.99, 0.99), (f64::NAN, p3), (f64::NAN, p3));
    assert!((phi - 20.0).abs() < 1e-9, "got {phi}");
    let (phi, rho) = coherent_phi_rho(
        (10.0, 20.0),
        (f64::NAN, 0.99),
        (p3, f64::NAN),
        (p3, f64::NAN),
    );
    assert!(phi.is_nan() && rho.is_nan());
}

/// Pair members 6° apart straddle the 360° seam for five gates (258–262): the
/// coherent primary averages circularly, the arithmetic mean manufactures a
/// ~180° plateau.
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

/// The documented ISDP estimator (`calc_system_PhiDP.c`): per radial the
/// 360°-aware median of the first 11-gate high-quality run past 25 km, and
/// across the sweep the `round(n/20)`-th entry of the sorted queue. Runs
/// starting inside 25 km or touching a ≥ 40 dBZ gate contribute nothing.
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

    // 60 radials with phases 10..69: sorted queue index round(60/20) = 3 → 13.
    let sweep: Vec<CombinedRadial> = (0..60)
        .map(|k| combined(10.0 + k as f64, 100, 20.0))
        .collect();
    assert_eq!(estimate_isdp(&sweep), Some(13.0));

    let close = combined(10.0, 60, 20.0);
    assert_eq!(radial_system_phi(&close.phi, &close.rho, &close.z), None);

    let hot = combined(10.0, 100, 45.0);
    assert_eq!(radial_system_phi(&hot.phi, &hot.rho, &hot.z), None);

    let thin: Vec<CombinedRadial> = (0..39)
        .map(|k| combined(10.0 + k as f64, 100, 20.0))
        .collect();
    assert_eq!(estimate_isdp(&thin), None);

    // Phases straddling 360 sort fold-aware: 350..359.5 and 0..9.5 read 351.
    let wrapped: Vec<CombinedRadial> = (0..40)
        .map(|k| combined((350.0 + 0.5 * k as f64) % 360.0, 100, 20.0))
        .collect();
    assert_eq!(estimate_isdp(&wrapped), Some(351.0));

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
    assert_eq!(without.init_fdp_deg, 205.0);
    // Falls back to the RDA value exactly as the source's `isdp_est != -99`
    // guard does.
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

/// A 24 °/km ramp clamps to `KDP_MAX_DISPLAY`, a −10 °/km one to
/// `KDP_MIN_DISPLAY` — the caps `dualpol8bit.c` applies (10.0) and the 16-bit
/// moment's minimum level preserves (−2.05).
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

/// `Is_high_atten_radial`'s documented thresholds: more than 10 qualifying
/// gates past bin 180 flag the radial.
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

/// Mirrors the twin comparator's resampling: the radial covering the cell
/// centre, the gate whose centre falls nearest it, earlier gate wins the tie.
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
    // Cell (0, 5): gate centres 5.375 (j 21) and 5.625 (j 22) tie at 0.125 km
    // from the cell centre — the earlier gate wins, as in `tally_packet`.
    assert_eq!(grid[0][5], 21.0);
    assert_eq!(grid[0][0], 1.0, "bin 0 reads gate 1 (centre 0.375)");
    assert_eq!(grid[1][0], 1.0);
    assert!(grid[1][5].is_nan(), "gate 21 is past the short radial");
    assert!(grid[5].iter().all(|v| v.is_nan()));
}

#[test]
fn a_radial_with_no_meteo_group_is_fully_censored() {
    let phi = |i: usize| G::V(100.0 + i as f64);
    let rho = |_: usize| G::V(0.5);
    let z30 = |_: usize| G::V(30.0);
    let radials = vec![dp_radial(0.5, 1.0, D_GATES, &phi, &rho, Some(&z30))];
    let derived = compute_kdp(&radials, &params_with_isdp(100.0)).expect("computes");
    assert!(derived.values[0].iter().all(|v| v.is_nan()));
}
