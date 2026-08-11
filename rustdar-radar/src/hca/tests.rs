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
    let derived =
        compute_hca(&radials, &p, &MeltingLayer::flat(4.0), &hsda_far(), None).expect("computes");
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

/// One 360-radial sweep with a wet-snow ring (Z 33, ZDR 1.5, ρ 0.93) painted
/// where the beam sits between 2.5 and 2.95 km, rain below it and dry snow
/// above it.
fn wet_snow_ring_sweep(elev: f64) -> Vec<Radial> {
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
}

/// The full MLDA on synthetic 4°–10° sweeps: a wet-snow ring, rain below it,
/// dry snow above it. Three tilts accumulate past the 1500 floor and the
/// detected layer lands on the ring.
#[test]
fn detect_melting_layer_finds_the_wet_snow_ring() {
    let sweeps: Vec<Vec<Radial>> = [4.5, 5.5, 6.5]
        .iter()
        .map(|&e| wet_snow_ring_sweep(e))
        .collect();
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

/// **Both of this module's radial fan-outs land on the answer one thread
/// lands on, bit for bit.**
///
/// [`compute_hca`] maps each radial to a row and keeps `combined`'s order, so
/// there is nothing to reassociate; [`detect_melting_layer`] maps each radial to
/// the heights it votes for and then *adds the votes serially*, because `weight`
/// is a float accumulator several radials of a sweep write to.
///
/// Be precise about how much that last one currently buys: `elev_weight` is
/// bound once per sweep, outside the radial loop, so every addend into a given
/// `weight[az][h]` within a sweep is the identical `1.0 + elev_weight`, and
/// summing identical values is permutation-invariant. Order cannot move a bit
/// today. The serial replay is kept because it costs nothing and becomes
/// load-bearing the moment `elev_weight` varies per radial — not because a
/// reassociation hazard exists right now.
///
/// Turning either into a parallel reduction would still pass every other test
/// in this module — they assert ranges and classes, and a last-bit difference in
/// an accumulator is invisible to all of them — so this compares the exact bits
/// against a one-thread pool and against repeat runs.
///
/// Know what that catches and what it does not. A one-thread pool runs this
/// same map-collect-replay code, so it can observe a genuine race and nothing
/// about the restructure: reversing the replay order passes here. What pins the
/// order is the pre-existing behavioural suite — `a_rain_field_below_the_layer_`
/// `classifies_ra_end_to_end` and its neighbours. `voxel/tests.rs` needed a
/// restated serial loop for exactly this reason; the difference is that there
/// the serial loop is *gone*, whereas here `combined`'s order is still the
/// thing the surrounding tests assert against.
// See the note in `voxel/tests.rs`: named rather than module-gated, so the rest
// of this module keeps being type-checked for wasm32.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn the_pool_classifies_a_volume_the_way_one_thread_does() {
    assert!(
        rayon::current_num_threads() > 1,
        "single-threaded pool: this test cannot observe a race"
    );
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("a one-thread pool");

    // ── One tilt through the per-radial classification ──────────────────
    let tilt = wet_snow_ring_sweep(4.5);
    let ml = MeltingLayer::flat(2.75);
    let classify = || compute_hca(&tilt, &params(), &ml, &hsda_far(), None).expect("computes");
    let parallel = classify();
    assert!(
        parallel.values.iter().flatten().any(|v| !v.is_nan()),
        "every gate is undefined; the fixture proves nothing"
    );
    for (label, other) in [
        ("one thread", one.install(classify)),
        ("a repeat", classify()),
    ] {
        assert_eq!(
            parallel.azimuths_deg, other.azimuths_deg,
            "{label} put the radials in a different order",
        );
        for (r, (a, b)) in parallel.values.iter().zip(&other.values).enumerate() {
            for (g, (&x, &y)) in a.iter().zip(b).enumerate() {
                assert_eq!(
                    x.to_bits(),
                    y.to_bits(),
                    "{label}: radial {r} gate {g} is {y}, not {x}",
                );
            }
        }
    }

    // ── Three tilts through the melting-layer accumulator ───────────────
    let sweeps: Vec<Vec<Radial>> = [4.5, 5.5, 6.5]
        .iter()
        .map(|&e| wet_snow_ring_sweep(e))
        .collect();
    let refs: Vec<&[Radial]> = sweeps.iter().map(|s| s.as_slice()).collect();
    let detect = || detect_melting_layer(&refs, &params(), 2.75, &hsda_far(), None);
    let parallel = detect();
    assert!(
        parallel.top_km_arl[0] != 2.75,
        "the layer is still the default; nothing accumulated and this proves nothing"
    );
    for (label, other) in [("one thread", one.install(detect)), ("a repeat", detect())] {
        for az in 0..360 {
            assert_eq!(
                parallel.top_km_arl[az].to_bits(),
                other.top_km_arl[az].to_bits(),
                "{label}: the layer top at az {az} is {}, not {}",
                other.top_km_arl[az],
                parallel.top_km_arl[az],
            );
            assert_eq!(
                parallel.bottom_km_arl[az].to_bits(),
                other.bottom_km_arl[az].to_bits(),
                "{label}: the layer bottom at az {az} is {}, not {}",
                other.bottom_km_arl[az],
                parallel.bottom_km_arl[az],
            );
        }
    }
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
