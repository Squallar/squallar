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

/// One steep sweep whose only return is a band a known **slant** range out.
///
/// Deliberately not [`refl_sweep`]'s field: a filled volume answers every bin
/// something, and the question the binning tests ask is *which bin* a single
/// return lands in. The band is ±2 gates so neither convention can miss it by
/// rounding.
fn banded_sweep(elevation_number: u8, elevation_deg: f32, band_slant_km: f64) -> Sweep {
    let band_gate = (band_slant_km / 0.25).round() as usize;
    let bytes: Vec<u8> = (0..GATES)
        .map(|j| {
            if j.abs_diff(band_gate) <= 2 {
                // 45 dBZ: well above the 18.3 echo-tops threshold and the
                // legacy VIL gate, so a bin holding it is unambiguous.
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

/// **The two binnings file the same gate in different bins**, which is the
/// whole of [`RangeBinning`] and the thing that silently corrupts a twin if
/// it is set wrong.
///
/// A 60° gate 40 km out along the beam stands over 20 km of ground —
/// `cos 60°` is exactly 0.5, so there is no rounding to argue about. The
/// slant cube must hold it at bin 40 and nothing at 20; the ground cube the
/// other way round.
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

    // And the heights moved with it: the ground cube's bin 20 is the height
    // over 20.5 km of ground, which is where that gate actually is.
    let h_ground = ground.tilts[0].heights.centre_km[20];
    let expect = crate::beam::height_at_ground_km(20.5, ground.tilts[0].elevation_deg);
    assert!(
        (h_ground - expect).abs() < 1e-12,
        "the ground cube's bin 20 sits at {h_ground:.4} km, not the \
         {expect:.4} km over that ground",
    );
}

/// **The RPG twins keep slant binning**, which is not a detail: `eet` and
/// `vil` exist to reproduce products 135 and 134 bin for bin, and the RPG
/// files a gate under the range it was measured at.
///
/// Two steep tilts carrying the same band at 40 km slant. Under slant binning
/// they stack in one column at bin 40 and the twins answer there. Under
/// ground binning they would scatter to bins 28 and 25 and bin 40 would be
/// empty — so this fails loudly the moment either module is "corrected".
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
/// defined-cell count, and spot values. Any change to the gridding, dedup,
/// beam-height or interpolation arithmetic moves at least the digest.
///
/// # Re-pinned when the cube learned [`RangeBinning`]
///
/// Echo tops moved to [`RangeBinning::Ground`], so every figure below is a
/// column over a **place** rather than over a beam length, and each one moved
/// the way that predicts:
///
/// * digest `0x4559ce366731e030` → `0x5385ddeb1814353b`;
/// * defined cells 4680 → 4689. Nine more, and upward is the expected
///   direction: a gate binning `cos e` inward packs the outer edge of each
///   core into fewer, fuller cells and the columns that were one gate short
///   of a defined cell now reach one.
/// * the spot heights all rise slightly, because a cell now asks "how high is
///   the beam **over** 40.5 km" rather than "how high is a beam 40.5 km
///   long", and standing over more ground means standing higher: 10.279476 →
///   10.309390 kft, 10.541305 → 10.572002, 12.955594 → 12.872785 (this one
///   falls — it is an *interpolated* crossing between two tilts, so it
///   follows the pair's spacing rather than either height), 7.6337643 →
///   7.560002.
///
/// The two RPG twins are **not** re-pinned, and that is the load-bearing half:
/// `eet` and `vil` stayed on [`RangeBinning::Slant`], and their suites are
/// byte-unchanged.
#[test]
fn golden_echo_tops_grid_is_pinned() {
    let grid = compute_echo_tops(&golden_scan());
    assert_eq!(grid.range_bins, 230);
    assert_eq!(grid.values.len(), 360);

    let defined: usize = grid.values.iter().flatten().filter(|v| !v.is_nan()).count();
    assert_eq!(defined, 4689, "defined-cell count moved");
    assert_eq!(fnv1a64(&grid), 0x5385ddeb1814353b, "grid digest moved");

    // Spot pins, exact to the bit, each hand-checked against the beam
    // height formula. Chosen to cover every code path:
    //
    // * (45, 40): core A crosses at the top tilt, so its top is *clamped*
    //   to that tilt's centre height over the cell — 40.5·tan 4.3° +
    //   40.5²/(2·8494.7·cos² 4.3°) = 3.143 km = 10.309 kft.
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
        (45usize, 40usize, 0x4124f343u32), // core A: 10.30939 kft
        (45, 41, 0x412926ec),              // core A, next cell: 10.572002
        (150, 80, 0x414df6ed),             // core B interpolated: 12.872785
        (308, 120, 0x40f1eb89),            // core C via SAILS repeat: 7.560002
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

/// The [`RangeBinning::Ground`] arm answers over the ground the cell covers,
/// which is a **different point in the air** from the slant arm's — and the
/// gap grows with the tilt, which is exactly why the two cannot be mixed.
///
/// Cell 100 (centre 100.5 km of ground) on a 0.5° tilt, `s·tan e +
/// s²/(2·Re′·cos²e)`:
///   centre = 100.5·tan 0.500° + 100.5²/(2·8494.667·cos² 0.500°)
///          = 1.4716008654 km
///   bottom = 100.5·tan 0.025° + 100.5²/(2·8494.667·cos² 0.025°)
///          = 0.6383568893 km
///   top    = 100.5·tan 0.975° + 100.5²/(2·8494.667·cos² 0.975°)
///          = 2.3050471627 km
///
/// At 0.5° the two arms are 7.9 cm apart at 100 km — nothing. At 19.5° over
/// 69.5 km of ground they are **1.447 km** apart, because the slant arm is
/// answering about a beam 69.5 km long, which stands over only 65.5 km of
/// ground. A hail layer or an echo top taking the wrong one would be
/// integrating a column that is not the column it was asked about.
#[test]
fn the_ground_arm_answers_over_the_ground_and_the_two_diverge_with_tilt() {
    let g = BeamHeights::at_elevation(0.5, RangeBinning::Ground);
    assert!((g.centre_km[100] - 1.4716008653654709).abs() < 1e-9);
    assert!((g.bottom_km[100] - 0.6383568893468486).abs() < 1e-9);
    assert!((g.top_km[100] - 2.3050471627204763).abs() < 1e-9);

    let slant = BeamHeights::at_elevation(0.5, RangeBinning::Slant);
    assert!(
        (g.centre_km[100] - slant.centre_km[100]).abs() < 1e-4,
        "at 0.5° the two arms must be within a metre of each other",
    );

    let steep_g = BeamHeights::at_elevation(19.5, RangeBinning::Ground);
    let steep_s = BeamHeights::at_elevation(19.5, RangeBinning::Slant);
    let gap = steep_g.centre_km[69] - steep_s.centre_km[69];
    assert!(
        (gap - 1.447316631030965).abs() < 1e-9,
        "at 19.5° over 69.5 km the arms must stand 1.447 km apart; they are \
         {gap:.6} km",
    );
    assert_eq!(g.centre_km.len(), RANGE_BINS);
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

/// **The property the parallel tilt walk has to keep**: the cube the pool
/// builds is the cube a serial walk over the same selection builds — same
/// tilts, same order, cell for cell.
///
/// [`VolumeCube::build_with_stats`] hands rayon one task per elevation. Two
/// things can go wrong there and nothing else in this module would see either.
/// `collect` could return the tilts out of order — every product over the cube
/// is a top-down or bottom-up column scan, and two of them sum along the tilt
/// index, so a swapped pair reads its reflectivity off one tilt and its beam
/// heights off another and the answer stays plausible. And a tilt could come
/// out differently for having run beside its neighbours rather than after
/// them.
///
/// **Read the second one narrowly**: what is pinned is *observational*
/// equivalence to a serial walk, not [`LinearZMemo`]'s locality contract.
/// Hoisting that table to a `static` shared unsynchronised across the pool, or
/// carrying it between calls in a `thread_local!`, both survive this test —
/// and would survive any test over this cube — because the table is keyed on
/// the gate's exact `to_bits()` and every racing writer stores the same `f64`
/// for the same key, so no reader can observe which writer won or whether the
/// entry was left over from an earlier call. Those rewrites are ruled out by
/// the argument at [`LinearZMemo`] and by review, and this test cannot be the
/// thing that catches them. What it does catch is any restructure that changes
/// what a caller sees: order, geometry, provenance, or a single cell's bits.
///
/// The reference is therefore a **serial `map` over [`sweep_selection`]'s own
/// output** — exactly the walk this function ran before it was parallelised,
/// over exactly the same selection, so the oracle cannot drift from the dedup
/// rules by restating them. A 1-thread rayon pool would not do on its own: it
/// runs this same par-map-collect code, so it can observe a data race and
/// nothing about the restructure. It is checked as well, with a repeat run,
/// because settling on the right answer once can still be a race that usually
/// lands the right way.
///
/// Every [`CellStat`] and both [`DedupPolicy`] arms: the statistic decides
/// which accumulator — and whether the memo is reached at all — inside each
/// task, and the policy decides which sweep a tilt is built from.
// Named rather than gating the module, as in `voxel` and `hca`: this is the
// only test here that reaches for `rayon` by name, and a module-wide gate
// would stop the rest being type-checked for wasm32 — the arm that compiles
// `par.rs`'s sequential stand-ins.
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

    // A split cut (velocity on its own sweep at 0.5°) and a SAILS repeat, so
    // the two policies genuinely disagree and one moment is absent on most
    // tilts.
    let scan = golden_scan();

    // Both [`RangeBinning`] arms, because the binning reaches inside each task
    // twice — the range scale a grid is filed at and the heights the tilt
    // carries — and a determinism claim proved for one filing rule says nothing
    // about the other.
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

            // A one-tilt cube cannot be out of order, and a cube of nothing
            // agrees with itself trivially.
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
                    // The ordering claim, stated where a swap would show:
                    // tilt `ti` is the same elevation the serial walk put
                    // there, and carries that elevation's own geometry.
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
                        // Cell by cell rather than slice against slice: a
                        // whole-grid `assert_eq!` prints 82 800 numbers twice
                        // and says nothing about which one moved.
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
