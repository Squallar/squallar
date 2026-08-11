use super::*;
use nexrad_model::data::{
    MomentData, PulseWidth, Radial, RadialStatus, Sweep, VolumeCoveragePattern,
};

const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
/// 0.25 km gates out to 250 km — past the 230 km grid, so the tail is
/// exercised as "outside the domain" rather than never generated.
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

/// The synthetic volume's reflectivity, dBZ, at a polar position and beam
/// height. `None` is "no return" (encoded as below-threshold, gate byte 0).
///
/// Three storm cores of different intensities decay with height at
/// 3.5 dBZ/km, so their columns cross the 18.3 dBZ echo-top threshold at
/// different tilts:
///
/// * core A (60 dBZ, az ~45°, r ~40 km) tops out above the highest tilt;
/// * core B (32 dBZ, az ~150°, r ~80 km) crosses between mid tilts;
/// * core C (27 dBZ, az ~300°, r ~120 km) crosses above the lowest tilt
///   only, so its top interpolates between the two lowest tilt centres.
///
/// The sector 200°–240° carries no data at all — a hole the grid must
/// leave NaN — and everything else is a 15 dBZ background that sits below
/// the threshold without being absent.
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

/// One reflectivity sweep: `n_radials` evenly spaced, first azimuth at
/// `az_offset`°, gate bytes encoding [`dbz_at`] through scale 2 offset 66.
fn refl_sweep(
    elevation_number: u8,
    elevation_deg: f32,
    n_radials: usize,
    az_offset: f32,
    sails_shift: bool,
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
                        Some(dbz) => ((dbz * SCALE as f64 + OFFSET as f64).round() as i64)
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

/// A velocity-only sweep — the Doppler half of a split cut. It carries no
/// reflectivity, so the reflectivity tilt selection must skip it entirely.
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

/// A five-tilt volume with the shapes a real SAILS volume throws at the
/// echo-top scan:
///
/// * 0.5° at half-degree super-resolution, radials **off** the whole-degree
///   cell centres (0.1° and 0.6°), so the nearest-radial choice is a real
///   choice;
/// * a velocity-only split cut at the same elevation, to be skipped;
/// * three upper tilts at 1° spacing;
/// * a SAILS repeat of 0.5° **late in the scan** whose cores are shifted 8°
///   in azimuth — under newest-wins it must displace the first 0.5° sweep,
///   which the pinned digest can tell because the two sweeps disagree.
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

/// FNV-1a over every cell's bit pattern, azimuth-major. Implemented here
/// rather than through `DefaultHasher` so the pinned literal does not
/// depend on the standard library's unspecified hash algorithm.
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

/// The golden pin for `compute_echo_tops`: the full grid digest, the
/// defined-cell count, and spot values, captured from the shipped
/// implementation before the volume cube refactor. Any change to the
/// gridding, dedup, beam-height or interpolation arithmetic moves at least
/// the digest; the refactor must reproduce all of it bit for bit.
#[test]
fn golden_echo_tops_grid_is_pinned() {
    let grid = compute_echo_tops(&golden_scan());
    assert_eq!(grid.range_bins, 230);
    assert_eq!(grid.values.len(), 360);

    let defined: usize = grid.values.iter().flatten().filter(|v| !v.is_nan()).count();
    assert_eq!(defined, 4680, "defined-cell count moved");
    assert_eq!(fnv1a64(&grid), 0x4559ce366731e030, "grid digest moved");

    // Spot pins, exact to the bit, each hand-checked against the beam
    // height formula. Chosen to cover every code path:
    //
    // * (45, 40): core A crosses at the top tilt, so its top is *clamped*
    //   to that tilt's centre height — 40.5·sin 4.3° + 40.5²/(2·8494.7)
    //   = 3.134 km = 10.28 kft.
    // * (45, 41): one cell further out, the same clamp moves with range.
    // * (150, 80): core B is topmost at 2.4° (18.9 dBZ at 3.75 km) and
    //   interpolates toward 3.4° (14.0 dBZ at 5.16 km) — ~3.95 km.
    // * (308, 120): core C crosses only at 0.5°, interpolating toward
    //   1.5° — ~2.3 km — and sits at 308° only because the SAILS repeat
    //   (cores shifted +8°) displaced the first 0.5° sweep.
    // * (300, 120): the first sweep's core C centre, NaN for the same
    //   reason. A first-of-volume dedup defines this cell and empties the
    //   one above, so these two pin newest-wins, not merely "some sweep".
    let spots = [
        (45usize, 40usize, 0x412478bcu32), // core A: 10.279476 kft
        (45, 41, 0x4128a92f),              // core A, next cell: 10.541305
        (150, 80, 0x414f4a1d),             // core B interpolated: 12.955594
        (308, 120, 0x40f447cc),            // core C via SAILS repeat: 7.6337643
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

    // The hole must stay a hole, and the sub-threshold background must not
    // produce tops.
    assert!(
        grid.values[220][100].is_nan(),
        "the no-data sector filled in"
    );
    assert!(grid.values[10][200].is_nan(), "15 dBZ background topped");
}

// ── VolumeCube ──────────────────────────────────────────────────────────

/// A one-radial sweep whose moment is handed in directly, for tests that
/// need full control of the encoding.
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

/// 1-km gates encoding the given bytes at scale/offset.
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
    let h = BeamHeights::at_elevation(0.5);
    assert!((h.centre_km[100] - 1.4715221935087277).abs() < 1e-9);
    assert!((h.bottom_km[100] - 0.638356771987057).abs() < 1e-9);
    assert!((h.top_km[100] - 2.3046273386189857).abs() < 1e-9);
    // Cell 0 (centre 0.5 km) on a 19.5° tilt: 0.5·sin 19.5° + 0.5²/(2·Re′).
    let steep = BeamHeights::at_elevation(19.5);
    assert!((steep.centre_km[0] - 0.16691814473225194).abs() < 1e-9);
    assert_eq!(h.centre_km.len(), RANGE_BINS);
    assert_eq!(h.bottom_km.len(), RANGE_BINS);
    assert_eq!(h.top_km.len(), RANGE_BINS);
}

/// Both dedup policies, on the same volume, disagree exactly where they
/// must: sweep identity, the displaced flag, and the values themselves —
/// the SAILS repeat's cores are shifted, so cell (308°, 120 km) is hotter
/// on the repeat than on the first look.
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
    );
    let first = VolumeCube::build(
        &scan,
        &[RadarProduct::Reflectivity],
        DedupPolicy::FirstOfVolume,
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

    // The two policies must yield *different fields*, not just different
    // indices: the repeat's core C sits at 308°, the first look's at 300°.
    assert!(n.values[308][120] > f.values[308][120]);
    assert!(n.values[300][120] < f.values[300][120]);

    // The unrepeated tilt is identical under both policies.
    let nu = newest.grid(1, RadarProduct::Reflectivity).unwrap();
    let fu = first.grid(1, RadarProduct::Reflectivity).unwrap();
    assert_eq!(nu.sweep_index, 1);
    assert_eq!(fu.sweep_index, 1);
    assert!(!nu.displaced_repeat);
}

/// Two radials contend for one azimuth cell; the one nearer the cell
/// centre must supply it.
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
    );
    let g = cube.grid(0, RadarProduct::Reflectivity).unwrap();
    assert!(
        (g.values[10][2] - 50.0).abs() < 1e-4,
        "cell 10 read {} — the farther radial won",
        g.values[10][2],
    );
    assert!(g.values[11][2].is_nan(), "no radial points at cell 11");
}

/// Below-threshold gates, ≥999 sentinels and empty cells all come out NaN;
/// a legitimate value in the same radial survives.
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

/// The statistic really is per moment: identical gate bytes read through
/// reflectivity average in linear Z, through ZDR arithmetically, and a
/// [`CellStat::Max`] override keeps the peak.
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
    );
    let m = peaked.grid(0, RadarProduct::Reflectivity).unwrap().values[7][0];
    assert_eq!(m, 40.0, "Max must keep the peak");
}

/// The free-bucket key must be a NaN, because that — and only that — is what
/// puts it behind [`sweep_to_grid`]'s `z.is_nan()` filter and so out of
/// [`LinearZMemo::linear_z`]'s reach. Any NaN would do; the pattern itself
/// buys nothing (see [`a_nan_gate_never_reaches_the_memo`]).
#[test]
fn the_linear_z_memo_free_bucket_is_a_nan() {
    assert!(f32::from_bits(LinearZMemo::FREE).is_nan());
}

/// A gate whose decoded value is *exactly* the free-bucket key must never
/// reach the memo, or it would match a bucket nothing ever wrote and read the
/// initial `0.0` back as a real answer.
///
/// Such a gate is constructible, which is the whole point: a NaN `offset`
/// propagates its payload through `(raw - offset) / scale` unchanged, so a
/// block declaring `offset = f32::from_bits(0xFFFF_FFFF)` decodes gate after
/// gate to `u32::MAX`. What keeps them out is `sweep_to_grid`'s `z.is_nan()`
/// filter, standing in front of the only call site.
///
/// Deleting `|| z.is_nan()` passes every other test in this crate. It does
/// not pass this one: the cell reads `-inf` instead of `NaN`, because
/// `LinearZMean` finishes with `10·log10(sum / n)` and the sum is the free
/// bucket's zero.
#[test]
fn a_nan_gate_never_reaches_the_memo() {
    let nan_offset = f32::from_bits(LinearZMemo::FREE);
    let refl = MomentData::from_fixed_point(8, 0, 1000, 8, SCALE, nan_offset, vec![100u8; 8]);

    // The premise: these gates decode to values, and they wear the free key.
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
    );
    let g = cube.grid(0, RadarProduct::Reflectivity).unwrap();
    for (r, v) in g.values[42].iter().enumerate() {
        assert!(
            v.is_nan(),
            "range cell {r} took a value from a NaN gate: {v}"
        );
    }
}

/// The memo's contract is bit-identity over the domain it actually sees.
/// Raw 0 and 1 are the below-threshold and range-folded sentinels and never
/// decode to a value, so an 8-bit reflectivity block reaches the conversion
/// with 254 distinct values — and every one of them, on the miss that
/// computes it and on the hits that follow, must be exactly the `f64`
/// `10f64.powf(z / 10.0)` returns.
#[test]
fn the_linear_z_memo_answers_every_reachable_gate_exactly_as_powf() {
    let domain: Vec<f32> = (2u16..=255)
        .map(|raw| (f32::from(raw) - OFFSET) / SCALE)
        .collect();
    assert_eq!(domain.len(), 254, "the reachable 8-bit gate domain");

    let mut memo = LinearZMemo::for_stat(CellStat::LinearZMean);
    // Three passes: the first misses everywhere, the rest hit.
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

/// The table is direct-mapped and a collision overwrites, so correctness
/// must not rest on an entry staying resident. A 16-bit moment block
/// reaches the conversion with 65 534 values — thirty-two per bucket — and
/// every answer is still `powf`'s, bit for bit, whether the bucket held this
/// key, someone else's, or nothing. The 8-bit domain, evicted wholesale by
/// that flood, then reads back identically too.
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

/// The gates that are not ordinary reflectivity but still reach the
/// conversion: zero — whose `-0.0` twin is a *different* key for the same
/// answer — the infinity a degenerate scale could decode to, which passes
/// `sweep_to_grid`'s `>= 999.0` and NaN filters unharmed, and a pair of
/// adjacent `f32`s.
///
/// That last pair is the one that keeps the key honest. Two values one ULP
/// apart are distinct gates with distinct answers, so any key that quantises
/// away low mantissa bits — `to_bits() >> 1`, `to_bits() & !0xFF` — hands the
/// second gate the first one's number. Nothing else here would notice: every
/// other domain in these tests is spaced half a dBZ apart.
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
        // Twice: the miss that computes, then the hit that recalls.
        for _ in 0..2 {
            assert_eq!(
                memo.linear_z(z).to_bits(),
                10f64.powf(z as f64 / 10.0).to_bits(),
                "gate {z}",
            );
        }
    }
}

/// A memo built for a statistic that never converts carries no buckets, and
/// a bucketless memo is exactly the expression the call site used to run
/// inline — every call straight to `powf`, nothing remembered between them.
/// That is what makes the empty case safe to carry rather than guard.
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

/// A split cut: reflectivity and velocity at the same elevation on
/// different sweeps. Each moment must come from its own sweep, on one
/// shared tilt.
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

    // The upper tilt has reflectivity but no velocity.
    assert!(cube.grid(1, RadarProduct::Reflectivity).is_some());
    assert!(cube.grid(1, RadarProduct::Velocity).is_none());

    // A moment the cube was not built for is None everywhere.
    assert!(cube.grid(0, RadarProduct::SpectrumWidth).is_none());
    assert_eq!(
        cube.moments(),
        &[RadarProduct::Reflectivity, RadarProduct::Velocity],
    );
}
