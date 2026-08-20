use super::*;
use nexrad_model::data::{
    MomentData, PulseWidth, Radial, RadialStatus, Sweep, VolumeCoveragePattern,
};

const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const GATES: usize = 1000;
const GATE_INTERVAL_M: u16 = 250;

pub(crate) fn vcp() -> VolumeCoveragePattern {
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

fn dbz_at(az_deg: f64, r_km: f64, height_km: f64, sails_shift: bool) -> Option<f64> {
    if (200.0..240.0).contains(&az_deg) {
        return None;
    }
    let shift = if sails_shift { 8.0 } else { 0.0 };
    let core = |c_az: f64, c_r: f64, w_az: f64, w_r: f64, amp: f64| {
        let mut daz = (az_deg - (c_az + shift)).abs();
        if daz > 180.0 {
            daz = 360.0 - daz;
        }
        let dr = r_km - c_r;
        amp * (-(daz * daz) / (2.0 * w_az * w_az) - (dr * dr) / (2.0 * w_r * w_r)).exp()
    };
    let surface = 15.0
        + core(45.0, 40.0, 12.0, 15.0, 45.0)
        + core(150.0, 80.0, 15.0, 20.0, 17.0)
        + core(300.0, 120.0, 10.0, 12.0, 12.0);
    Some(surface - 3.5 * height_km)
}

fn refl_sweep(
    elevation_number: u8,
    elevation_deg: f32,
    n_radials: usize,
    az_offset: f32,
    sails_shift: bool,
) -> Sweep {
    refl_sweep_lifted(
        elevation_number,
        elevation_deg,
        n_radials,
        az_offset,
        sails_shift,
        0.0,
    )
}

fn refl_sweep_lifted(
    elevation_number: u8,
    elevation_deg: f32,
    n_radials: usize,
    az_offset: f32,
    sails_shift: bool,
    lift_dbz: f64,
) -> Sweep {
    let spacing = 360.0 / n_radials as f32;
    let radials = (0..n_radials)
        .map(|i| {
            let az = az_offset + i as f32 * spacing;
            let bytes: Vec<u8> = (0..GATES)
                .map(|j| {
                    let r_km = j as f64 * 0.25;
                    let h_km = beam_height_km(r_km, elevation_deg as f64);
                    match dbz_at(az as f64, r_km, h_km, sails_shift) {
                        None => 0, // below threshold: skipped by the grid
                        Some(dbz) => (((dbz + lift_dbz) * SCALE as f64 + OFFSET as f64).round()
                            as i64)
                            .clamp(2, 255) as u8,
                    }
                })
                .collect();
            Radial::new(
                0,
                i as u16,
                az,
                spacing,
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
                    bytes,
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

fn banded_sweep(elevation_number: u8, elevation_deg: f32, band_slant_km: f64) -> Sweep {
    let band_gate = (band_slant_km / 0.25).round() as usize;
    let bytes: Vec<u8> = (0..GATES)
        .map(|j| {
            if j.abs_diff(band_gate) <= 2 {
                // 45 dBZ: above the 18.3 echo-tops threshold and the legacy
                // VIL gate, so a bin holding it is unambiguous.
                ((45.0 * SCALE + OFFSET).round() as i64).clamp(2, 255) as u8
            } else {
                0
            }
        })
        .collect();
    let radials = (0..360)
        .map(|i| {
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
                    bytes.clone(),
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

#[test]
fn a_high_tilt_gate_bins_at_its_ground_range() {
    let scan = Scan::new(vcp(), vec![banded_sweep(1, 60.0, 40.0)]);
    let cube = |binning| {
        VolumeCube::build(
            &scan,
            &[RadarProduct::Reflectivity],
            DedupPolicy::NewestWins,
            binning,
        )
    };
    let at =
        |c: &VolumeCube, r: usize| c.grid(0, RadarProduct::Reflectivity).unwrap().values[42][r];

    let slant = cube(RangeBinning::Slant);
    assert!(
        !at(&slant, 40).is_nan(),
        "the slant cube lost the gate at the range it was measured at",
    );
    assert!(
        at(&slant, 20).is_nan(),
        "the slant cube put the gate over the ground as well as at its range",
    );

    let ground = cube(RangeBinning::Ground);
    assert!(
        !at(&ground, 20).is_nan(),
        "the ground cube did not file the 40 km gate over the 20 km of ground \
         it stands on",
    );
    assert!(
        at(&ground, 40).is_nan(),
        "the ground cube still holds the gate at its slant range",
    );

    let h_ground = ground.tilts[0].heights.centre_km[20];
    let expect = crate::beam::height_at_ground_km(20.5, ground.tilts[0].elevation_deg);
    assert!(
        (h_ground - expect).abs() < 1e-12,
        "the ground cube's bin 20 sits at {h_ground:.4} km, not the \
         {expect:.4} km over that ground",
    );
}

#[test]
fn the_rpg_twins_keep_slant_binning() {
    let scan = Scan::new(
        vcp(),
        vec![banded_sweep(1, 45.0, 40.0), banded_sweep(2, 50.0, 40.0)],
    );

    let vil = crate::vil::compute_vil(&scan);
    assert!(
        !vil.values[42][40].is_nan(),
        "DVL lost its column at the slant bin the RPG would have used",
    );
    assert!(
        vil.values[42][28].is_nan(),
        "DVL answered at the 28 km of ground a 45° / 40 km gate stands over, \
         which is not where the twin puts it",
    );

    let eet = crate::eet::compute_eet(&scan, 0.0);
    assert!(
        !eet.values[42][40].is_nan(),
        "EET lost its column at the slant bin the RPG would have used",
    );
    assert!(
        eet.values[42][28].is_nan(),
        "EET answered over the ground rather than at the gate's own range",
    );
}

fn velocity_only_sweep(elevation_number: u8, elevation_deg: f32) -> Sweep {
    let radials = (0..360)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                i as f32 + 0.5,
                1.0,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                None,
                Some(MomentData::from_fixed_point(
                    400,
                    0,
                    250,
                    8,
                    2.0,
                    129.0,
                    vec![129; 400],
                )),
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

pub(crate) fn golden_scan() -> Scan {
    Scan::new(
        vcp(),
        vec![
            refl_sweep(1, 0.5, 720, 0.1, false),
            velocity_only_sweep(2, 0.5),
            refl_sweep(3, 1.5, 360, 0.5, false),
            refl_sweep(4, 2.4, 360, 0.5, false),
            refl_sweep(5, 3.4, 360, 0.5, false),
            refl_sweep(6, 0.5, 720, 0.1, true), // SAILS repeat: newest wins
            refl_sweep(7, 4.3, 360, 0.5, false),
        ],
    )
}

pub(crate) fn fnv1a64(grid: &VolumetricGrid) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for row in &grid.values {
        for v in row {
            for b in v.to_bits().to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
        }
    }
    h
}

#[test]
fn golden_echo_tops_grid_is_pinned() {
    let grid = compute_echo_tops(&golden_scan());
    assert_eq!(grid.range_bins, 230);
    assert_eq!(grid.values.len(), 360);

    let defined: usize = grid.values.iter().flatten().filter(|v| !v.is_nan()).count();
    assert_eq!(defined, 4689, "defined-cell count moved");
    assert_eq!(fnv1a64(&grid), 0x7718c8e4c1f550ef, "grid digest moved");

    // Spot pins, exact to the bit, each hand-checked against the beam height
    // formula:
    // * (45, 40): core A crosses at the top tilt, so its top is clamped to
    //   that tilt's centre height. 40.5 km of ground arc at 4.3° is
    //   40.62920 km of slant range (`slant_range_for_ground_km`), and
    //   40.62920·sin 4.3° + 40.62920²/(2·8494.667) = 3.14349 km = 10.31328 kft.
    // * (45, 41): one cell further out, the same clamp moves with range.
    // * (150, 80): core B is topmost at 2.4° (18.9 dBZ at 3.75 km) and
    //   interpolates toward 3.4° (14.0 dBZ at 5.16 km) — ~3.95 km.
    // * (308, 120): core C crosses only at 0.5°, interpolating toward 1.5°.
    // * (300, 120): the first sweep's core C centre, NaN — newest-wins.
    let spots = [
        (45usize, 40usize, 0x41250334u32), // core A: 10.313282 kft
        (45, 41, 0x412937b2),              // core A, next cell: 10.576097
        (150, 80, 0x414e1120),             // core B interpolated: 12.879181
        (308, 120, 0x40f201e0),            // core C via SAILS repeat: 7.562729
    ];
    for (az, r, bits) in spots {
        let got = grid.values[az][r];
        assert_eq!(
            got.to_bits(),
            bits,
            "cell az {az}° r {r} km: got {got} ({:#010x})",
            got.to_bits(),
        );
    }
    assert!(
        grid.values[300][120].is_nan(),
        "the SAILS repeat no longer displaces the first 0.5° sweep",
    );

    assert!(
        grid.values[220][100].is_nan(),
        "the no-data sector filled in"
    );
    assert!(grid.values[10][200].is_nan(), "15 dBZ background topped");
}

// ── VolumeCube ──────────────────────────────────────────────────────────

fn one_radial_sweep(
    elevation_number: u8,
    elevation_deg: f32,
    azimuth: f32,
    refl: Option<MomentData>,
    vel: Option<MomentData>,
    zdr: Option<MomentData>,
) -> Sweep {
    let radial = Radial::new(
        0,
        0,
        azimuth,
        1.0,
        RadialStatus::IntermediateRadialData,
        elevation_number,
        elevation_deg,
        refl,
        vel,
        None,
        zdr,
        None,
        None,
        None,
    );
    Sweep::new(elevation_number, radials_vec(radial))
}

fn radials_vec(r: Radial) -> Vec<Radial> {
    vec![r]
}

fn moment(bytes: &[u8], scale: f32, offset: f32) -> MomentData {
    MomentData::from_fixed_point(
        bytes.len() as u16,
        0,
        1000,
        8,
        scale,
        offset,
        bytes.to_vec(),
    )
}

#[test]
fn beam_heights_match_the_hand_computed_four_thirds_model() {
    // Range cell 100 (centre 100.5 km) on a 0.5° tilt, half-power
    // beamwidth 0.95°, effective radius 6371·4/3 km:
    //   centre = 100.5·sin 0.500° + 100.5²/(2·8494.667) = 1.4715221935 km
    //   bottom = 100.5·sin 0.025° + …                   = 0.6383567720 km
    //   top    = 100.5·sin 0.975° + …                   = 2.3046273386 km
    let h = BeamHeights::at_elevation(0.5, RangeBinning::Slant);
    assert!((h.centre_km[100] - 1.4715221935087277).abs() < 1e-9);
    assert!((h.bottom_km[100] - 0.638356771987057).abs() < 1e-9);
    assert!((h.top_km[100] - 2.3046273386189857).abs() < 1e-9);
    // Cell 0 (centre 0.5 km) on a 19.5° tilt: 0.5·sin 19.5° + 0.5²/(2·Re′).
    let steep = BeamHeights::at_elevation(19.5, RangeBinning::Slant);
    assert!((steep.centre_km[0] - 0.16691814473225194).abs() < 1e-9);
    assert_eq!(h.centre_km.len(), RANGE_BINS);
    assert_eq!(h.bottom_km.len(), RANGE_BINS);
    assert_eq!(h.top_km.len(), RANGE_BINS);
}

#[test]
fn the_ground_arm_answers_over_the_ground_and_the_two_diverge_with_tilt() {
    let g = BeamHeights::at_elevation(0.5, RangeBinning::Ground);
    assert!((g.centre_km[100] - 1.471910651079749).abs() < 1e-9);
    assert!((g.bottom_km[100] - 0.6384207809488871).abs() < 1e-9);
    assert!((g.top_km[100] - 2.3057665206657405).abs() < 1e-9);

    let slant = BeamHeights::at_elevation(0.5, RangeBinning::Slant);
    assert!(
        (g.centre_km[100] - slant.centre_km[100]).abs() < 1e-3,
        "at 0.5° the two arms must be within a metre of each other",
    );

    let steep_g = BeamHeights::at_elevation(19.5, RangeBinning::Ground);
    let steep_s = BeamHeights::at_elevation(19.5, RangeBinning::Slant);
    let gap = steep_g.centre_km[69] - steep_s.centre_km[69];
    assert!(
        (gap - 1.5212578179714455).abs() < 1e-9,
        "at 19.5° over 69.5 km the arms must stand 1.521 km apart; they are \
         {gap:.6} km",
    );
    assert_eq!(g.centre_km.len(), RANGE_BINS);
}

#[test]
fn dedup_policies_pick_opposite_ends_of_a_sails_pair() {
    let scan = Scan::new(
        vcp(),
        vec![
            refl_sweep(1, 0.5, 360, 0.5, false),
            refl_sweep(2, 1.5, 360, 0.5, false),
            refl_sweep(3, 0.5, 360, 0.5, true), // SAILS repeat, shifted
        ],
    );
    let newest = VolumeCube::build(
        &scan,
        &[RadarProduct::Reflectivity],
        DedupPolicy::NewestWins,
        RangeBinning::Slant,
    );
    let first = VolumeCube::build(
        &scan,
        &[RadarProduct::Reflectivity],
        DedupPolicy::FirstOfVolume,
        RangeBinning::Slant,
    );
    assert_eq!(newest.tilts.len(), 2);
    assert_eq!(first.tilts.len(), 2);
    assert!((newest.tilts[0].elevation_deg - 0.5).abs() < 1e-12);
    assert!((newest.tilts[1].elevation_deg - 1.5).abs() < 1e-12);

    let n = newest.grid(0, RadarProduct::Reflectivity).unwrap();
    assert_eq!(n.sweep_index, 2);
    assert!(n.displaced_repeat, "the repeat displaced the first look");

    let f = first.grid(0, RadarProduct::Reflectivity).unwrap();
    assert_eq!(f.sweep_index, 0);
    assert!(!f.displaced_repeat);

    assert!(n.values[308][120] > f.values[308][120]);
    assert!(n.values[300][120] < f.values[300][120]);

    let nu = newest.grid(1, RadarProduct::Reflectivity).unwrap();
    let fu = first.grid(1, RadarProduct::Reflectivity).unwrap();
    assert_eq!(nu.sweep_index, 1);
    assert_eq!(fu.sweep_index, 1);
    assert!(!nu.displaced_repeat);
}

#[test]
fn the_radial_nearest_the_cell_centre_wins() {
    // Cell 10's centre is 10.5°. 10.2° is 0.3 away, 10.4° is 0.1 away.
    let far = Radial::new(
        0,
        0,
        10.2,
        0.5,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        Some(moment(&[126; 5], SCALE, OFFSET)), // 30 dBZ
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let near = Radial::new(
        0,
        1,
        10.4,
        0.5,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        Some(moment(&[166; 5], SCALE, OFFSET)), // 50 dBZ
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let scan = Scan::new(vcp(), vec![Sweep::new(1, vec![far, near])]);
    let cube = VolumeCube::build(
        &scan,
        &[RadarProduct::Reflectivity],
        DedupPolicy::NewestWins,
        RangeBinning::Slant,
    );
    let g = cube.grid(0, RadarProduct::Reflectivity).unwrap();
    assert!(
        (g.values[10][2] - 50.0).abs() < 1e-4,
        "cell 10 read {} — the farther radial won",
        g.values[10][2],
    );
    assert!(g.values[11][2].is_nan(), "no radial points at cell 11");
}

#[test]
fn nan_propagation_keeps_holes_and_drops_sentinels() {
    // ZDR at scale 0.1, offset 0: byte 0 below threshold, byte 100 →
    // 1000 (a ≥999 sentinel, dropped), byte 50 → 500 (kept).
    let mut bytes = vec![0u8; 3];
    bytes.extend_from_slice(&[100, 100, 100]); // cell 3..6 → sentinel
    bytes.extend_from_slice(&[50, 50]); // cells 6, 7 → 500
    let zdr = MomentData::from_fixed_point(8, 0, 1000, 8, 0.1, 0.0, bytes);
    let scan = Scan::new(
        vcp(),
        vec![one_radial_sweep(1, 0.5, 42.5, None, None, Some(zdr))],
    );
    let cube = VolumeCube::build(
        &scan,
        &[RadarProduct::DifferentialReflectivity],
        DedupPolicy::NewestWins,
        RangeBinning::Slant,
    );
    let g = cube
        .grid(0, RadarProduct::DifferentialReflectivity)
        .unwrap();
    assert!(g.values[42][0].is_nan(), "below-threshold gates filled in");
    assert!(g.values[42][3].is_nan(), "a ≥999 sentinel was kept");
    assert!((g.values[42][6] - 500.0).abs() < 1e-3);
    assert!(g.values[42][7 + 1..].iter().all(|v| v.is_nan()));
    assert!(g.values[43][0].is_nan(), "another azimuth cell filled in");
}

#[test]
fn cell_statistics_dispatch_per_moment() {
    // Two 0.5-km gates per 1-km cell: 20 dBZ and 40 dBZ.
    let bytes = vec![106u8, 146]; // (b-66)/2 → 20, 40
    let make = || MomentData::from_fixed_point(2, 0, 500, 8, SCALE, OFFSET, bytes.clone());
    let scan = Scan::new(
        vcp(),
        vec![one_radial_sweep(
            1,
            0.5,
            7.5,
            Some(make()),
            None,
            Some(make()),
        )],
    );
    let cube = VolumeCube::build(
        &scan,
        &[
            RadarProduct::Reflectivity,
            RadarProduct::DifferentialReflectivity,
        ],
        DedupPolicy::NewestWins,
        RangeBinning::Slant,
    );
    let z = cube.grid(0, RadarProduct::Reflectivity).unwrap().values[7][0];
    let zdr = cube
        .grid(0, RadarProduct::DifferentialReflectivity)
        .unwrap()
        .values[7][0];
    // 10·log₁₀((10² + 10⁴)/2) = 37.0329…, not the 30.0 a dB-space mean
    // would give.
    assert!((z - 37.032_913).abs() < 1e-4, "got {z}");
    assert_eq!(zdr, 30.0, "ZDR must average arithmetically");

    let peaked = VolumeCube::build_with_stats(
        &scan,
        &[(RadarProduct::Reflectivity, CellStat::Max)],
        DedupPolicy::NewestWins,
        RangeBinning::Slant,
    );
    let m = peaked.grid(0, RadarProduct::Reflectivity).unwrap().values[7][0];
    assert_eq!(m, 40.0, "Max must keep the peak");
}

#[test]
fn the_linear_z_memo_free_bucket_is_a_nan() {
    assert!(f32::from_bits(LinearZMemo::FREE).is_nan());
}

#[test]
fn a_nan_gate_never_reaches_the_memo() {
    let nan_offset = f32::from_bits(LinearZMemo::FREE);
    let refl = MomentData::from_fixed_point(8, 0, 1000, 8, SCALE, nan_offset, vec![100u8; 8]);

    let wearing_free = refl
        .iter()
        .filter(|v| matches!(v, MomentValue::Value(z) if z.to_bits() == LinearZMemo::FREE))
        .count();
    assert_eq!(
        wearing_free, 8,
        "a NaN offset must hand every gate the free-bucket key, or this test \
         proves nothing",
    );

    let scan = Scan::new(
        vcp(),
        vec![one_radial_sweep(1, 0.5, 42.5, Some(refl), None, None)],
    );
    let cube = VolumeCube::build(
        &scan,
        &[RadarProduct::Reflectivity],
        DedupPolicy::NewestWins,
        RangeBinning::Slant,
    );
    let g = cube.grid(0, RadarProduct::Reflectivity).unwrap();
    for (r, v) in g.values[42].iter().enumerate() {
        assert!(
            v.is_nan(),
            "range cell {r} took a value from a NaN gate: {v}"
        );
    }
}

#[test]
fn the_linear_z_memo_answers_every_reachable_gate_exactly_as_powf() {
    let domain: Vec<f32> = (2u16..=255)
        .map(|raw| (f32::from(raw) - OFFSET) / SCALE)
        .collect();
    assert_eq!(domain.len(), 254, "the reachable 8-bit gate domain");

    let mut memo = LinearZMemo::for_stat(CellStat::LinearZMean);
    for pass in 0..3 {
        for &z in &domain {
            assert_eq!(
                memo.linear_z(z).to_bits(),
                10f64.powf(z as f64 / 10.0).to_bits(),
                "pass {pass}, gate {z} dBZ",
            );
        }
    }
}

#[test]
fn the_linear_z_memo_is_exact_through_eviction() {
    let mut memo = LinearZMemo::for_stat(CellStat::LinearZMean);
    for raw in 2u32..=65_535 {
        let z = (raw as f32 - OFFSET) / SCALE;
        assert_eq!(
            memo.linear_z(z).to_bits(),
            10f64.powf(z as f64 / 10.0).to_bits(),
            "16-bit code {raw}",
        );
    }
    for raw in 2u16..=255 {
        let z = (f32::from(raw) - OFFSET) / SCALE;
        assert_eq!(
            memo.linear_z(z).to_bits(),
            10f64.powf(z as f64 / 10.0).to_bits(),
            "8-bit code {raw} after eviction",
        );
    }
}

#[test]
fn the_linear_z_memo_is_exact_on_the_gates_that_are_not_reflectivity() {
    let mut memo = LinearZMemo::for_stat(CellStat::LinearZMean);
    let ulp = 20.0f32;
    let ulp_next = f32::from_bits(ulp.to_bits() + 1);
    assert_ne!(ulp, ulp_next, "the ULP pair must be two distinct gates");
    for z in [
        0.0f32,
        -0.0,
        f32::NEG_INFINITY,
        -3.4e38,
        998.9,
        ulp,
        ulp_next,
    ] {
        for _ in 0..2 {
            assert_eq!(
                memo.linear_z(z).to_bits(),
                10f64.powf(z as f64 / 10.0).to_bits(),
                "gate {z}",
            );
        }
    }
}

#[test]
fn a_memo_for_a_statistic_that_never_converts_is_powf_itself() {
    for stat in [CellStat::Mean, CellStat::Max] {
        let mut memo = LinearZMemo::for_stat(stat);
        assert!(
            memo.slots.is_empty(),
            "{stat:?} bought buckets it cannot use",
        );
        for raw in 2u16..=255 {
            let z = (f32::from(raw) - OFFSET) / SCALE;
            assert_eq!(
                memo.linear_z(z).to_bits(),
                10f64.powf(z as f64 / 10.0).to_bits(),
                "{stat:?}, 8-bit code {raw}",
            );
        }
        assert!(memo.slots.is_empty(), "a bucketless memo stays bucketless");
    }
}

#[test]
fn a_split_cut_supplies_each_moment_from_its_own_sweep() {
    let scan = Scan::new(
        vcp(),
        vec![
            refl_sweep(1, 0.5, 360, 0.5, false),
            velocity_only_sweep(2, 0.5),
            refl_sweep(3, 1.5, 360, 0.5, false),
        ],
    );
    let cube = VolumeCube::build(
        &scan,
        &[RadarProduct::Reflectivity, RadarProduct::Velocity],
        DedupPolicy::NewestWins,
        RangeBinning::Slant,
    );
    assert_eq!(cube.tilts.len(), 2);

    let z = cube.grid(0, RadarProduct::Reflectivity).unwrap();
    let v = cube.grid(0, RadarProduct::Velocity).unwrap();
    assert_eq!(z.sweep_index, 0, "reflectivity from the surveillance cut");
    assert_eq!(v.sweep_index, 1, "velocity from the Doppler cut");
    assert!(
        !z.displaced_repeat && !v.displaced_repeat,
        "a split cut is not a SAILS repeat: neither moment displaced anything",
    );
    assert_eq!(v.values[42][50], 0.0, "byte 129 at scale 2/offset 129");

    assert!(cube.grid(1, RadarProduct::Reflectivity).is_some());
    assert!(cube.grid(1, RadarProduct::Velocity).is_none());

    assert!(cube.grid(0, RadarProduct::SpectrumWidth).is_none());
    assert_eq!(
        cube.moments(),
        &[RadarProduct::Reflectivity, RadarProduct::Velocity],
    );
}

// Named rather than gating the module: a module-wide gate would stop the rest
// being type-checked for wasm32, the arm that compiles `par.rs`'s sequential
// stand-ins.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn the_tilts_the_pool_builds_are_the_tilts_a_serial_walk_builds() {
    assert!(
        rayon::current_num_threads() > 1,
        "single-threaded pool: this test cannot observe a race"
    );
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("a one-thread pool");

    let scan = golden_scan();

    for (stat, binning) in [CellStat::LinearZMean, CellStat::Mean, CellStat::Max]
        .into_iter()
        .flat_map(|s| [RangeBinning::Slant, RangeBinning::Ground].map(|b| (s, b)))
    {
        let moments: Vec<(RadarProduct, CellStat)> =
            [RadarProduct::Reflectivity, RadarProduct::Velocity]
                .into_iter()
                .map(|m| (m, stat))
                .collect();
        for policy in [DedupPolicy::NewestWins, DedupPolicy::FirstOfVolume] {
            let at = format!("{stat:?} under {policy:?}, {binning:?} bins");

            let (chosen, keys) = sweep_selection(&scan, &moments, policy);
            let serial: Vec<Tilt> = keys
                .into_iter()
                .map(|key| tilt_at(&scan, &moments, &chosen, key, binning))
                .collect();

            assert!(
                serial.len() > 1,
                "{at}: the fixture yielded {} tilt(s); this proves nothing",
                serial.len(),
            );
            let defined: usize = serial
                .iter()
                .flat_map(|t| t.grids.iter().flatten())
                .flat_map(|g| g.values.iter().flatten())
                .filter(|v| !v.is_nan())
                .count();
            assert!(
                defined > 1000,
                "{at}: only {defined} cells carry data; this proves nothing",
            );

            let build = || VolumeCube::build_with_stats(&scan, &moments, policy, binning);
            let pooled = build();
            let one_thread = one.install(build);
            let repeat = build();
            for (label, other) in [
                ("the pool", &pooled),
                ("one thread", &one_thread),
                ("a repeat", &repeat),
            ] {
                assert_eq!(
                    other.tilts.len(),
                    serial.len(),
                    "{at}: {label} built {} tilts, the serial walk {}",
                    other.tilts.len(),
                    serial.len(),
                );
                for (ti, (got, want)) in other.tilts.iter().zip(&serial).enumerate() {
                    assert_eq!(
                        got.elevation_deg.to_bits(),
                        want.elevation_deg.to_bits(),
                        "{at}: {label} put {}° at tilt {ti}, the serial walk {}°",
                        got.elevation_deg,
                        want.elevation_deg,
                    );
                    for (which, a, b) in [
                        ("bottom", &got.heights.bottom_km, &want.heights.bottom_km),
                        ("centre", &got.heights.centre_km, &want.heights.centre_km),
                        ("top", &got.heights.top_km, &want.heights.top_km),
                    ] {
                        for (r, (&x, &y)) in a.iter().zip(b).enumerate() {
                            assert_eq!(
                                x.to_bits(),
                                y.to_bits(),
                                "{at}: {label} tilt {ti} {which} height at {r} km is {x}, \
                                 the serial walk's is {y}",
                            );
                        }
                    }

                    assert_eq!(got.grids.len(), want.grids.len(), "{at}: tilt {ti}");
                    for (mi, (g, w)) in got.grids.iter().zip(&want.grids).enumerate() {
                        let (g, w) = match (g, w) {
                            (None, None) => continue,
                            (Some(g), Some(w)) => (g, w),
                            (g, _) => panic!(
                                "{at}: {label} tilt {ti} moment {mi} is {}, the serial \
                                 walk's is the other",
                                if g.is_some() { "present" } else { "absent" },
                            ),
                        };
                        assert_eq!(
                            g.sweep_index, w.sweep_index,
                            "{at}: {label} tilt {ti} moment {mi} came from sweep {}, \
                             the serial walk's from {}",
                            g.sweep_index, w.sweep_index,
                        );
                        assert_eq!(
                            g.displaced_repeat, w.displaced_repeat,
                            "{at}: {label} tilt {ti} moment {mi} disagrees on displacement",
                        );
                        for (az, (a, b)) in g.values.iter().zip(&w.values).enumerate() {
                            for (r, (&x, &y)) in a.iter().zip(b).enumerate() {
                                assert_eq!(
                                    x.to_bits(),
                                    y.to_bits(),
                                    "{at}: {label} tilt {ti} moment {mi} cell \
                                     [{az}][{r}] is {x}, the serial walk's is {y}",
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn a_cells_status_names_which_of_the_decoders_answers_its_gates_gave() {
    use crate::types::GateReport;

    // Raw codes, four 0.25 km gates per 1-km cell. 0 and 1 are the decoder's;
    // anything >= 2 decodes to a number through SCALE/OFFSET.
    let bytes: Vec<u8> = vec![
        0, 0, 0, 0, // cell 0: below threshold throughout
        0, 0, 1, 0, // cell 1: one fold among blanks
        0, 1, 200, 0, // cell 2: a real value, a fold, two blanks
        1, 1, 1, 1, // cell 3: folded throughout
    ];
    let radial = Radial::new(
        0,
        0,
        0.5,
        1.0,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        Some(MomentData::from_fixed_point(
            bytes.len() as u16,
            0,
            GATE_INTERVAL_M,
            8,
            SCALE,
            OFFSET,
            bytes,
        )),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let (values, status) = sweep_to_grid(
        &radials_vec(radial),
        RadarProduct::Reflectivity,
        CellStat::Max,
        None,
        GateFiling::AsDeclared,
    );

    assert_eq!(status[0][0], GateReport::BelowThreshold, "cell 0");
    assert_eq!(status[0][1], GateReport::RangeFolded, "cell 1: fold wins");
    assert_eq!(status[0][2], GateReport::Value, "cell 2: a number wins");
    assert_eq!(status[0][3], GateReport::RangeFolded, "cell 3");
    assert_eq!(
        status[0][4],
        GateReport::NotReported,
        "past the last gate is an absence, not a measurement",
    );
    assert_eq!(status[10][0], GateReport::NotReported, "unserved azimuth");

    // The invariant `xsect.rs` holds its own status plane to: the plane says
    // `Value` exactly where the grid has a number, on every cell of the grid.
    for (az, (vrow, srow)) in values.iter().zip(status.iter()).enumerate() {
        for (r, (v, s)) in vrow.iter().zip(srow.iter()).enumerate() {
            assert_eq!(
                v.is_finite(),
                *s == GateReport::Value,
                "az {az} bin {r}: value {v} against status {s:?}",
            );
        }
    }
}

#[test]
fn the_cubes_status_plane_says_value_exactly_where_its_grid_has_a_number() {
    use crate::types::GateReport;

    let scan = golden_scan();
    let cube = VolumeCube::build(
        &scan,
        &[RadarProduct::Reflectivity],
        DedupPolicy::NewestWins,
        RangeBinning::Slant,
    );
    let mut seen_value = 0u64;
    let mut seen_below = 0u64;
    let mut seen_absent = 0u64;
    for ti in 0..cube.tilts.len() {
        let Some(grid) = cube.grid(ti, RadarProduct::Reflectivity) else {
            continue;
        };
        assert_eq!(grid.status.len(), grid.values.len(), "row counts agree");
        for (vrow, srow) in grid.values.iter().zip(grid.status.iter()) {
            assert_eq!(vrow.len(), srow.len(), "column counts agree");
            for (v, s) in vrow.iter().zip(srow.iter()) {
                assert_eq!(v.is_finite(), *s == GateReport::Value);
                match s {
                    GateReport::Value => seen_value += 1,
                    GateReport::BelowThreshold => seen_below += 1,
                    GateReport::NotReported => seen_absent += 1,
                    GateReport::RangeFolded => {}
                }
            }
        }
    }
    assert!(seen_value > 0, "the fixture must define some cells");
    assert!(seen_below > 0, "and leave some measured-empty");
    assert_eq!(
        seen_absent, 0,
        "this fixture reports every gate of the domain: its blanks are all \
         measurements, which is exactly why the populations are measured on \
         real volumes and not here",
    );
}

// ---------------------------------------------------------------------------
// Property tests for `compute_echo_tops`. `golden_echo_tops_grid_is_pinned`
// above hashes this function's own output, so it can tell you a number moved
// and never that a number is right; each of these asserts a property checked
// against a height derived from `crate::beam`, an analytic interpolation, or
// the same volume scanned twice. They are not an oracle for the algorithm's
// conventions — only an external reference can settle those.
// ---------------------------------------------------------------------------

fn expected_centre_kft(r: usize, elev_deg: f64) -> f64 {
    crate::beam::height_at_ground_km(r as f64 + 0.5, elev_deg) * 3.28084
}

fn flat_sweep(elevation_number: u8, elevation_deg: f32, dbz: Option<f64>) -> Sweep {
    let n_radials = 360;
    let radials = (0..n_radials)
        .map(|i| {
            let az = 0.5 + i as f32;
            let bytes: Vec<u8> = (0..GATES)
                .map(|_| match dbz {
                    None => 0,
                    Some(z) => {
                        ((z * SCALE as f64 + OFFSET as f64).round() as i64).clamp(2, 255) as u8
                    }
                })
                .collect();
            Radial::new(
                0,
                i as u16,
                az,
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
                    bytes,
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

fn defined_cells(grid: &VolumetricGrid) -> usize {
    grid.values.iter().flatten().filter(|v| !v.is_nan()).count()
}

#[test]
fn every_echo_top_sits_inside_the_tilt_ladder_it_was_scanned_from() {
    let scan = golden_scan();
    let grid = compute_echo_tops(&scan);
    let cube = VolumeCube::build(
        &scan,
        &[RadarProduct::Reflectivity],
        DedupPolicy::NewestWins,
        RangeBinning::Ground,
    );
    let elevs: Vec<f64> = cube
        .tilts
        .iter()
        .enumerate()
        .filter(|(ti, _)| cube.grid(*ti, RadarProduct::Reflectivity).is_some())
        .map(|(_, t)| t.elevation_deg)
        .collect();
    let (lowest, highest) = (elevs[0], elevs[elevs.len() - 1]);

    let mut checked = 0usize;
    for (az, row) in grid.values.iter().enumerate() {
        for (r, v) in row.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            checked += 1;
            let (floor, ceil) = (
                expected_centre_kft(r, lowest),
                expected_centre_kft(r, highest),
            );
            let v = f64::from(*v);
            assert!(
                v >= floor - 1e-3,
                "az {az}° r {r} km: top {v} kft is below the lowest tilt's centre {floor} kft"
            );
            assert!(
                v <= ceil + 1e-3,
                "az {az}° r {r} km: top {v} kft is above the highest tilt's centre {ceil} kft — \
                 the ceiling clamp has been lost and the scan is extrapolating upward"
            );
        }
    }
    assert!(checked > 1000, "fixture defined only {checked} cells");
}

#[test]
fn lifting_every_gate_never_lowers_an_echo_top() {
    let lifted = Scan::new(
        vcp(),
        vec![
            refl_sweep_lifted(1, 0.5, 720, 0.1, false, 3.0),
            velocity_only_sweep(2, 0.5),
            refl_sweep_lifted(3, 1.5, 360, 0.5, false, 3.0),
            refl_sweep_lifted(4, 2.4, 360, 0.5, false, 3.0),
            refl_sweep_lifted(5, 3.4, 360, 0.5, false, 3.0),
            refl_sweep_lifted(6, 0.5, 720, 0.1, true, 3.0),
            refl_sweep_lifted(7, 4.3, 360, 0.5, false, 3.0),
        ],
    );
    let base = compute_echo_tops(&golden_scan());
    let up = compute_echo_tops(&lifted);

    let mut compared = 0usize;
    for az in 0..360 {
        for r in 0..RANGE_BINS {
            let (b, u) = (base.values[az][r], up.values[az][r]);
            if b.is_nan() {
                continue;
            }
            assert!(
                !u.is_nan(),
                "az {az}° r {r} km: a cell defined at {b} kft went undefined when every gate rose"
            );
            compared += 1;
            assert!(
                u >= b - 1e-3,
                "az {az}° r {r} km: top fell from {b} to {u} kft when every gate rose by 3 dBZ"
            );
        }
    }
    assert!(compared > 1000, "compared only {compared} cells");
    assert!(
        defined_cells(&up) >= defined_cells(&base),
        "the lifted volume defines fewer cells ({}) than the base one ({})",
        defined_cells(&up),
        defined_cells(&base),
    );
}

#[test]
fn a_volume_entirely_below_the_threshold_reports_no_echo_tops() {
    let scan = Scan::new(
        vcp(),
        vec![
            flat_sweep(1, 0.5, Some(15.0)),
            flat_sweep(2, 1.5, Some(15.0)),
            flat_sweep(3, 2.5, Some(15.0)),
        ],
    );
    assert_eq!(defined_cells(&compute_echo_tops(&scan)), 0);
}

#[test]
fn the_echo_top_threshold_brackets_between_18_0_and_18_5_dbz() {
    let at = |dbz: f64| {
        defined_cells(&compute_echo_tops(&Scan::new(
            vcp(),
            vec![flat_sweep(1, 0.5, Some(dbz)), flat_sweep(2, 1.5, Some(dbz))],
        )))
    };
    assert_eq!(
        at(18.0),
        0,
        "18.0 dBZ is below the threshold and must not cross"
    );
    assert!(
        at(18.5) > 0,
        "18.5 dBZ is above the threshold and must cross"
    );
}

#[test]
fn a_single_tilt_volume_reports_that_tilts_centre_height() {
    let elev = 2.0;
    let grid = compute_echo_tops(&Scan::new(
        vcp(),
        vec![flat_sweep(1, elev as f32, Some(40.0))],
    ));
    let mut checked = 0usize;
    for row in grid.values.iter() {
        for (r, v) in row.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            checked += 1;
            let want = expected_centre_kft(r, elev);
            assert!(
                (f64::from(*v) - want).abs() < 1e-2,
                "r {r} km: got {v} kft, beam centre over that ground range is {want} kft"
            );
        }
    }
    assert!(checked > 1000, "checked only {checked} cells");
}

#[test]
fn a_crossing_with_nothing_above_it_clamps_to_its_own_tilt() {
    let elev = 1.5;
    let grid = compute_echo_tops(&Scan::new(
        vcp(),
        vec![
            flat_sweep(1, elev as f32, Some(40.0)),
            flat_sweep(2, 2.5, None),
        ],
    ));
    let mut checked = 0usize;
    for row in grid.values.iter() {
        for (r, v) in row.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            checked += 1;
            let want = expected_centre_kft(r, elev);
            assert!(
                (f64::from(*v) - want).abs() < 1e-2,
                "r {r} km: clamped top {v} kft, its own tilt's centre is {want} kft"
            );
        }
    }
    assert!(checked > 1000, "checked only {checked} cells");
}

#[test]
fn an_interpolated_top_lands_between_its_two_tilts_by_reflectivity() {
    let (lo, up) = (1.5f64, 2.5f64);
    let grid = compute_echo_tops(&Scan::new(
        vcp(),
        vec![
            flat_sweep(1, lo as f32, Some(25.0)),
            flat_sweep(2, up as f32, Some(10.0)),
        ],
    ));
    let frac = (25.0 - 18.3) / (25.0 - 10.0);
    let mut checked = 0usize;
    for row in grid.values.iter() {
        for (r, v) in row.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            checked += 1;
            let (h_lo, h_up) = (expected_centre_kft(r, lo), expected_centre_kft(r, up));
            let want = h_lo + (h_up - h_lo) * frac;
            assert!(
                (f64::from(*v) - want).abs() < 2e-2,
                "r {r} km: got {v} kft, the two tilts ({h_lo}, {h_up}) put the 18.3 dBZ \
                 crossing at {want} kft"
            );
            assert!(
                f64::from(*v) > h_lo && f64::from(*v) < h_up,
                "r {r} km: interpolated top {v} kft escaped its bracket ({h_lo}, {h_up})"
            );
        }
    }
    assert!(checked > 1000, "checked only {checked} cells");
}

#[test]
fn an_unscanned_sector_reports_no_echo_tops() {
    let grid = compute_echo_tops(&golden_scan());
    for az in 200..240 {
        let defined = grid.values[az].iter().filter(|v| !v.is_nan()).count();
        assert_eq!(
            defined, 0,
            "azimuth {az}° has {defined} tops in an unscanned sector"
        );
    }
}

#[test]
fn along_one_tilt_the_echo_top_rises_with_range() {
    let grid = compute_echo_tops(&Scan::new(vcp(), vec![flat_sweep(1, 3.0, Some(40.0))]));
    let row = &grid.values[100];
    let mut last = f32::NEG_INFINITY;
    let mut seen = 0usize;
    for (r, v) in row.iter().enumerate() {
        if v.is_nan() {
            continue;
        }
        assert!(
            *v > last,
            "r {r} km: top {v} kft did not rise above the previous cell's {last}"
        );
        last = *v;
        seen += 1;
    }
    assert!(seen > 100, "saw only {seen} cells");
}

// ---------------------------------------------------------------------------
// Long-pulse 500 m replication: [`GateFiling`], [`replicated_pairs`].
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum SampleForm {
    Honest500m,
    Replicated250m,
    Honest500mMisregistered,
}

const LONG_PULSE_FIRST_GATE_M: u16 = 2125;

fn replicated_sweep_n(
    n_radials: usize,
    elevation_deg: f32,
    samples: &[u8],
    form: SampleForm,
) -> Sweep {
    let (bytes, first_gate_m, gate_m): (Vec<u8>, u16, u16) = match form {
        SampleForm::Honest500m => (samples.to_vec(), 2250, 500),
        SampleForm::Honest500mMisregistered => (samples.to_vec(), LONG_PULSE_FIRST_GATE_M, 500),
        SampleForm::Replicated250m => (
            samples.iter().flat_map(|&b| [b, b]).collect(),
            LONG_PULSE_FIRST_GATE_M,
            250,
        ),
    };
    let spacing = 360.0 / n_radials as f32;
    let radials = (0..n_radials)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                i as f32 * spacing + 0.5,
                spacing,
                RadialStatus::IntermediateRadialData,
                1,
                elevation_deg,
                Some(MomentData::from_fixed_point(
                    bytes.len() as u16,
                    first_gate_m,
                    gate_m,
                    8,
                    SCALE,
                    OFFSET,
                    bytes.clone(),
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
    Sweep::new(1, radials)
}

fn replicated_sweep(elevation_deg: f32, samples: &[u8], form: SampleForm) -> Sweep {
    replicated_sweep_n(360, elevation_deg, samples, form)
}

fn sample_run() -> Vec<u8> {
    (0..920)
        .map(|k: usize| {
            if k.is_multiple_of(97) {
                0 // below threshold
            } else {
                (2 + (k * 7) % 200) as u8
            }
        })
        .collect()
}

const LONG_PULSE_TOP_TILT_DEG: f32 = 4.5;

fn ground_grid(sweep: &Sweep) -> Vec<Vec<f32>> {
    let radials = sweep.radials();
    let binning = RangeBinning::Ground;
    sweep_to_grid(
        radials,
        RadarProduct::Reflectivity,
        CellStat::LinearZMean,
        binning.binning_elevation_deg(radials),
        binning.gate_filing(radials, RadarProduct::Reflectivity),
    )
    .0
}

#[test]
fn a_replicated_pair_grids_as_the_500_m_sample_it_encodes() {
    let samples = sample_run();
    let wire = replicated_sweep(
        LONG_PULSE_TOP_TILT_DEG,
        &samples,
        SampleForm::Replicated250m,
    );
    let honest = replicated_sweep(LONG_PULSE_TOP_TILT_DEG, &samples, SampleForm::Honest500m);
    let (a, b) = (ground_grid(&wire), ground_grid(&honest));
    let mut differing = 0usize;
    for (row_a, row_b) in a.iter().zip(&b) {
        for (x, y) in row_a.iter().zip(row_b) {
            if x.to_bits() != y.to_bits() {
                differing += 1;
            }
        }
    }
    assert_eq!(
        differing, 0,
        "the pair walk does not reproduce the 500 m content it encodes"
    );
    let defined = a.iter().flatten().filter(|v| !v.is_nan()).count();
    assert!(defined > 200, "only {defined} cells defined");
}

#[test]
fn the_first_sub_gates_centre_is_not_the_pairs_centre() {
    let samples = sample_run();
    let wire = replicated_sweep(
        LONG_PULSE_TOP_TILT_DEG,
        &samples,
        SampleForm::Replicated250m,
    );
    let trap = replicated_sweep(
        LONG_PULSE_TOP_TILT_DEG,
        &samples,
        SampleForm::Honest500mMisregistered,
    );
    let (a, b) = (ground_grid(&wire), ground_grid(&trap));
    let differing = a
        .iter()
        .flatten()
        .zip(b.iter().flatten())
        .filter(|(x, y)| x.to_bits() != y.to_bits())
        .count();
    assert!(
        differing > 0,
        "the 2125 m and 2250 m registrations produced identical grids, so this \
         test cannot tell a right answer from a wrong one"
    );
}

#[test]
fn slant_bins_hold_whole_pairs_so_replication_costs_them_nothing() {
    let samples = sample_run();
    let wire = replicated_sweep(
        LONG_PULSE_TOP_TILT_DEG,
        &samples,
        SampleForm::Replicated250m,
    );
    let honest = replicated_sweep(LONG_PULSE_TOP_TILT_DEG, &samples, SampleForm::Honest500m);
    let grid = |s: &Sweep| {
        let radials = s.radials();
        sweep_to_grid(
            radials,
            RadarProduct::Reflectivity,
            CellStat::Max,
            RangeBinning::Slant.binning_elevation_deg(radials),
            RangeBinning::Slant.gate_filing(radials, RadarProduct::Reflectivity),
        )
        .0
    };
    let (a, b) = (grid(&wire), grid(&honest));
    let differing = a
        .iter()
        .flatten()
        .zip(b.iter().flatten())
        .filter(|(x, y)| x.to_bits() != y.to_bits())
        .count();
    assert_eq!(
        differing, 0,
        "slant bins do not hold whole replicated pairs after all"
    );
}

#[test]
fn the_detector_reads_parity_and_not_smoothness() {
    let replicated = replicated_sweep(1.5, &sample_run(), SampleForm::Replicated250m);
    assert!(
        replicated_pairs(replicated.radials(), RadarProduct::Reflectivity),
        "replicated content was not recognised"
    );
    let smooth_bytes: Vec<u8> = (0..1840u32).map(|j| (2 + (j / 7) % 200) as u8).collect();
    let smooth = replicated_sweep(1.5, &smooth_bytes, SampleForm::Honest500m);
    assert!(
        !replicated_pairs(smooth.radials(), RadarProduct::Reflectivity),
        "a smooth ramp was mistaken for replicated content"
    );
}

#[test]
fn the_detector_needs_numbers_and_needs_enough_of_them() {
    let short: Vec<u8> = (0..(REPLICATION_MIN_PAIRS as usize - 1))
        .map(|k| (2 + k % 200) as u8)
        .collect();
    let sweep = replicated_sweep_n(1, 1.5, &short, SampleForm::Replicated250m);
    assert!(
        !replicated_pairs(sweep.radials(), RadarProduct::Reflectivity),
        "{} numeric pairs cleared a floor of {}",
        short.len(),
        REPLICATION_MIN_PAIRS
    );

    let empty = vec![0u8; 920];
    let sweep = replicated_sweep(1.5, &empty, SampleForm::Replicated250m);
    assert!(
        !replicated_pairs(sweep.radials(), RadarProduct::Reflectivity),
        "a sweep of nothing but below-threshold pairs qualified as replicated"
    );
}

#[test]
fn the_detector_clears_the_floor_at_exactly_the_floor() {
    let just_enough: Vec<u8> = (0..REPLICATION_MIN_PAIRS as usize)
        .map(|k| (2 + k % 200) as u8)
        .collect();
    let sweep = replicated_sweep_n(1, 1.5, &just_enough, SampleForm::Replicated250m);
    assert!(
        replicated_pairs(sweep.radials(), RadarProduct::Reflectivity),
        "exactly {} numeric pairs did not clear a floor of {}",
        REPLICATION_MIN_PAIRS,
        REPLICATION_MIN_PAIRS
    );
}

#[test]
fn only_ground_binning_of_replicated_content_ever_decimates() {
    let refl = RadarProduct::Reflectivity;
    let replicated = replicated_sweep(1.5, &sample_run(), SampleForm::Replicated250m);
    let ordinary = replicated_sweep(1.5, &sample_run(), SampleForm::Honest500m);
    for (sweep, binning, want, what) in [
        (
            &replicated,
            RangeBinning::Ground,
            GateFiling::ReplicatedPairs,
            "replicated content on the ground grid",
        ),
        (
            &replicated,
            RangeBinning::Slant,
            GateFiling::AsDeclared,
            "replicated content on the slant grid",
        ),
        (
            &ordinary,
            RangeBinning::Ground,
            GateFiling::AsDeclared,
            "ordinary content on the ground grid",
        ),
        (
            &ordinary,
            RangeBinning::Slant,
            GateFiling::AsDeclared,
            "ordinary content on the slant grid",
        ),
    ] {
        assert_eq!(
            binning.gate_filing(sweep.radials(), refl),
            want,
            "{what} was filed the wrong way"
        );
    }
}

#[test]
fn a_ground_binned_cell_is_the_arc_not_the_tangent_plane() {
    const ELEV: f32 = 0.5;
    const SLANT_KM: f64 = 200.06;

    // 250 m gates from 60 m, so gate 800 is centred at exactly
    // 0.06 + 800 × 0.25 = 200.06 km. Only that one carries a number.
    const FIRST_GATE_M: u16 = 60;
    const GATE_M: u16 = 250;
    const TARGET: usize = 800;
    let mut bytes = vec![0u8; TARGET + 1];
    bytes[TARGET] = (50.0f32 * SCALE + OFFSET) as u8;
    let md = MomentData::from_fixed_point(
        bytes.len() as u16,
        FIRST_GATE_M,
        GATE_M,
        8,
        SCALE,
        OFFSET,
        bytes,
    );
    assert!(
        (f64::from(FIRST_GATE_M) / 1000.0 + TARGET as f64 * f64::from(GATE_M) / 1000.0 - SLANT_KM)
            .abs()
            < 1e-9,
        "the target gate is not at {SLANT_KM} km",
    );
    let scan = Scan::new(
        vcp(),
        vec![one_radial_sweep(1, ELEV, 0.0, Some(md), None, None)],
    );

    let ground = crate::beam::ground_range_km(SLANT_KM, f64::from(ELEV));
    let flat = SLANT_KM * f64::from(ELEV).to_radians().cos();
    assert_eq!(ground as usize, 199, "the arc's bin moved: {ground}");
    assert_eq!(flat as usize, 200, "the tangent plane's bin moved: {flat}");

    let cube = VolumeCube::build(
        &scan,
        &[RadarProduct::Reflectivity],
        DedupPolicy::NewestWins,
        RangeBinning::Ground,
    );
    let values = &cube
        .grid(0, RadarProduct::Reflectivity)
        .expect("the cube kept the one reflectivity sweep")
        .values;
    let filed: Vec<usize> = (0..RANGE_BINS)
        .filter(|&r| !values[0][r].is_nan())
        .collect();
    assert_eq!(
        filed,
        vec![199],
        "the one gate should file in the arc's bin 199 and nowhere else",
    );
    assert!(
        values[0][200].is_nan(),
        "the gate filed under the tangent plane's bin 200",
    );
}
