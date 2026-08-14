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
    refl_sweep_lifted(
        elevation_number,
        elevation_deg,
        n_radials,
        az_offset,
        sails_shift,
        0.0,
    )
}

/// [`refl_sweep`] with every gate's reflectivity raised by `lift_dbz` before
/// encoding — the second volume
/// [`lifting_every_gate_never_lowers_an_echo_top`] needs, identical to the
/// first but for a uniform shift of the field.
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
/// # What this test is, and what it is not
///
/// It is a **change detector**. It hashes this function's own output, so it
/// can report that a number moved; it cannot report that a number is right,
/// and it says nothing about whether the conventions above it are the ones
/// this product should have. For a long time it was the whole of
/// `compute_echo_tops`'s test coverage, which is how a product with an unnamed
/// reference came to look validated. The property tests at the foot of this
/// file carry the "is it right" half now, and the function's own doc carries
/// the measurement against [`crate::eet`].
///
/// Its blind spot is not the one you would guess. This fixture's background is
/// a **reported** 15 dBZ rather than an absence, so `z_up` is never `NaN` at a
/// cell that has data — and the echo-absent-above **clamp branch is therefore
/// unreachable from [`golden_scan`]**. A mutation that changed the clamped
/// height outright leaves this digest untouched, and is caught only by
/// [`a_crossing_with_nothing_above_it_clamps_to_its_own_tilt`]. That branch is
/// no corner case: in the RPG twin it governs 2 % of columns below 15 kft and
/// 52.7 % above 45 kft, and [`crate::eet`] names it the dominant term in its
/// own depth-dependent bias.
///
/// # The implementation itself is independently confirmed
///
/// Separately from this digest, an implementation written from
/// `compute_echo_tops`'s *documented* rule alone — Py-ART decoding the same
/// nine archives, no sight of this code — reproduces it with **footprint IoU
/// 1.000 at all nine sites** and **100 % of cells inside 1 kft**, RMS
/// 0.03–0.18 kft. So the arithmetic here does what the doc says it does; what
/// remains open is whether the conventions are the right ones, which no test
/// in this file can settle.
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
///
/// # Re-pinned again when the ground range became the arc
///
/// [`crate::beam::ground_range_km`] and its inverse stopped being the
/// tangent-plane `r·cos e` and became the spherical arc, so
/// [`crate::beam::height_at_ground_km`] — which this grid's heights come
/// through — is now `height_km` composed with the *arc's* inverse. The height
/// model itself did not change; what changed is which slant range a cell's
/// ground range names.
///
/// * digest `0x5385ddeb1814353b` → `0x7718c8e4c1f550ef`;
/// * **defined cells 4689 → 4689.** Unchanged, and that is the load-bearing
///   figure: no cell gained or lost coverage, so the grid's shape is identical
///   and only the heights inside it moved. A change that had disturbed the
///   binning would show here first.
/// * every spot height rose, all four by ~0.004 kft (1.2 m): 10.309390 →
///   10.313282 kft, 10.572002 → 10.576097, 12.872785 → 12.879181, 7.560002 →
///   7.562729. Rising is the direction the change predicts and it is the same
///   direction as the re-pin above, for the same reason carried one step
///   further — reaching a given distance *over the ground* takes a longer beam
///   when that distance is measured as an arc than when it is measured on the
///   tangent plane, and a longer beam stands higher. The interpolated cell
///   (150, 80) rises with the rest this time rather than falling, because both
///   tilts of its bracketing pair moved the same way by nearly the same amount.
///
/// The twins are again untouched: `eet` and `vil` are on
/// [`RangeBinning::Slant`], which never sees a ground range.
#[test]
fn golden_echo_tops_grid_is_pinned() {
    let grid = compute_echo_tops(&golden_scan());
    assert_eq!(grid.range_bins, 230);
    assert_eq!(grid.values.len(), 360);

    let defined: usize = grid.values.iter().flatten().filter(|v| !v.is_nan()).count();
    assert_eq!(defined, 4689, "defined-cell count moved");
    assert_eq!(fnv1a64(&grid), 0x7718c8e4c1f550ef, "grid digest moved");

    // Spot pins, exact to the bit, each hand-checked against the beam
    // height formula. Chosen to cover every code path:
    //
    // * (45, 40): core A crosses at the top tilt, so its top is *clamped*
    //   to that tilt's centre height over the cell. 40.5 km of ground arc
    //   at 4.3° is 40.62920 km of slant range (`slant_range_for_ground_km`),
    //   and 40.62920·sin 4.3° + 40.62920²/(2·8494.667) = 3.14349 km =
    //   10.31328 kft. Under the tangent plane the same cell was 40.50378 km
    //   of slant range and 3.14230 km = 10.30939 kft.
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
/// Cell 100 (centre 100.5 km of ground) on a 0.5° tilt. The arm is
/// `height_km(slant_range_for_ground_km(s, e), e)`, and standing over 100.5 km
/// of *arc* takes 100.5189 km of beam where the tangent plane asked for
/// 100.5038:
///   centre (0.500°) = 1.4719106511 km
///   bottom (0.025°) = 0.6384207809 km
///   top    (0.975°) = 2.3057665207 km
///
/// # Re-pinned when the ground range became the arc
///
/// The three were 1.4716008654, 0.6383568893 and 2.3050471627 km, the folded
/// `s·tan e + s²/(2·Re′·cos²e)` this stopped being. All three rose — by 0.31 m,
/// 0.06 m and 0.72 m — and rising is the direction the change predicts, for the
/// same reason the move to [`RangeBinning::Ground`] raised them before: the arc
/// is shorter than the tangent-plane projection of the same beam, so reaching a
/// *given* ground distance takes a longer beam than it used to, and a longer
/// beam stands higher.
///
/// At 0.5° the two arms are 38.8 cm apart at 100 km, where they were 7.9 cm —
/// still nothing, and the tolerance below moves 1e-4 → 1e-3 km to keep saying
/// so with a figure rather than by being wide. At 19.5° over 69.5 km of ground
/// they are **1.521 km** apart, where they were 1.447, because the slant arm is
/// answering about a beam 69.5 km long, which stands over only 65.3 km of
/// ground. A hail layer or an echo top taking the wrong one would be
/// integrating a column that is not the column it was asked about.
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

/// A cell's status names which of the decoder's three answers its gates gave,
/// and a cell no gate reached says so.
///
/// Validated against **the decoder's own raw-code convention**, not against
/// any table of ours: `nexrad_model`'s `MomentData::from_fixed_point` maps raw
/// 0 to `BelowThreshold` and raw 1 to `RangeFolded`, so the bytes written here
/// are the input side and `GateReport` is the output side. Nothing in this
/// test consults the value grid to decide what the status ought to be.
///
/// The four 1-km cells are built from 0.25 km gates, four to a cell, so the
/// aggregation rule is exercised rather than assumed:
///
/// * cell 0 — all four below threshold. The radar looked and saw nothing.
/// * cell 1 — three below threshold and one range folded. Signal beats no
///   signal, so the cell is folded, not empty.
/// * cell 2 — one real value among a fold and two blanks. A number beats
///   everything.
/// * cell 3 — all four range folded.
///
/// Everything past the moment's last gate is `NotReported`, which is the arm
/// a bare `NaN` could never distinguish from the two above it.
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
        1.0,
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
    // The azimuths no radial served are absences too.
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

/// The status plane keeps that invariant over a whole built cube, not only
/// over one hand-made radial — every moment, every tilt.
///
/// It also pins what this fixture *is*, which is why a synthetic scan cannot
/// stand in for a measurement of the populations: `refl_sweep` writes a raw
/// code for every gate of every radial out to 250 km, past the 230 km domain,
/// so every blank in it is a below-threshold **measurement** and the cube it
/// builds holds no `NotReported` cell at all. Real volumes are not like that.
/// [`crate::velocity::VelocityGrid`]'s census records the same limitation of
/// the synthetic corpus — its patcher repaints every gate, so it reads zero
/// folded everywhere. The absence arm is pinned on the hand-made radial
/// above, where the gate range can actually stop short.
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
    // A vacuous pass would satisfy the assertions above; both populations have
    // to be non-empty for the invariant to have been tested at all.
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
// Property tests for `compute_echo_tops`
//
// `golden_echo_tops_grid_is_pinned` above is a **change detector**: it hashes
// this function's own output, so it can tell you a number moved and can never
// tell you a number is right. Everything below asserts a property that is true
// of an echo top by construction — checked against a height derived in the
// test from `crate::beam`, or against an analytic interpolation, or against
// the same volume scanned twice — so each one fails on a real regression
// rather than on any change at all.
//
// They are deliberately *not* an oracle for the algorithm's conventions. What
// this product's cell statistic, binning and datum ought to be is a question
// only an external reference can settle; these bound the arithmetic given
// those conventions.
// ---------------------------------------------------------------------------

/// kft above the radar of a tilt's beam centre over ground range cell `r`,
/// derived in the test from the crate's own beam geometry rather than copied
/// from a pinned literal. `compute_echo_tops` bins on the ground, so this is
/// [`crate::beam::height_at_ground_km`] and not the slant-range formula.
fn expected_centre_kft(r: usize, elev_deg: f64) -> f64 {
    crate::beam::height_at_ground_km(r as f64 + 0.5, elev_deg) * 3.28084
}

/// A sweep whose every gate carries the same reflectivity — or, for `None`,
/// no return at all, which the grid must leave undefined.
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

/// **No column reports above the volume's ceiling, and none below its floor.**
///
/// The ceiling half is the load-bearing one: `compute_echo_tops` deliberately
/// does *not* extrapolate upward — a column still above threshold at the
/// highest tilt is clamped to that tilt's own centre height. A regression that
/// reintroduced upward extrapolation (the fixture's core A is exactly that
/// case) would leave the digest looking merely "moved" and would fail here
/// with the cell that broke the rule.
///
/// The bracket is derived per range cell from the tilt ladder the cube
/// actually built, so it follows the fixture rather than restating it.
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

/// **Lifting every gate by a constant never lowers an echo top, and never
/// takes one away.**
///
/// True by construction and independent of every convention: raising the whole
/// field either leaves the crossing tilt where it was and moves the
/// interpolation fraction up — the denominator `z − z_up` is unchanged by a
/// uniform shift while the numerator `z − t` grows — or promotes the crossing
/// to a higher tilt. Neither can lower the answer.
///
/// This is the one test here that would catch an inverted interpolation
/// fraction, swapped `h`/`h_up`, or a `<`/`>` flip in the top-down scan, none
/// of which a digest can distinguish from a legitimate re-pin.
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

/// **A volume with nothing above the threshold reports nothing at all.**
///
/// Three tilts uniformly at 15 dBZ — reported everywhere, below threshold
/// everywhere. The distinction matters: these gates are *not* absent, so a
/// regression that treated "reported but weak" as a crossing would fill the
/// whole grid, and one that lowered the threshold below 15 dBZ would too.
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

/// **The threshold is the boundary it says it is.**
///
/// 18.0 dBZ everywhere defines nothing; 18.5 dBZ everywhere defines the grid.
/// Both are exactly representable through scale 2 / offset 66, so this brackets
/// [`ET_THRESHOLD_DBZ`] without depending on the encoding's rounding — and it
/// fails if the constant moves in either direction by more than 0.2 dBZ.
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

/// **A single-tilt volume reports that tilt's own centre height, everywhere.**
///
/// With no tilt above there is nothing to interpolate toward, so every defined
/// cell must land exactly on the beam centre over its own ground range — the
/// degenerate case the interpolation branch must not touch. Checked against
/// [`crate::beam::height_at_ground_km`] evaluated in the test, so it also pins
/// that `compute_echo_tops` reads its heights on the **ground** binning: the
/// slant-range formula gives a visibly different number at every cell.
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

/// **A crossing with no data above clamps to its own tilt's centre height.**
///
/// The lower tilt is above threshold everywhere and the upper carries no
/// return at all. This is the documented "echo absent above" rule, and it is
/// the same clamp [`crate::eet`] applies — asserted here against a derived
/// height rather than a pinned one, so it survives a change to the beam model
/// and fails on a change to the rule.
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

/// **An interpolated top lands where the two tilts say it does.**
///
/// 25 dBZ below, 10 dBZ above: the crossing sits a fraction
/// `(25 − 18.3)/(25 − 10)` of the way from the lower tilt's centre to the
/// upper's, at every range cell. The expected value is computed here from the
/// arithmetic the module documents — not read back from the module — so this
/// is an independent check of the interpolation rather than a restatement of
/// it, and it is the test that would catch the fraction being inverted.
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

/// **A sector the volume never scanned stays undefined.**
///
/// The fixture reports nothing at all between 200° and 240°. An echo top there
/// would mean the column scan had read a neighbouring azimuth's data, which no
/// amount of re-pinning would reveal.
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

/// **Along one tilt, the top rises with range.** Beam centre height is strictly
/// increasing in ground range at any fixed elevation, so a uniformly filled
/// single-tilt volume must report a monotonically rising column of numbers.
/// A binning or height-lookup that read the wrong cell's range would break the
/// ordering long before it moved any single value far enough to notice.
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

/// How a run of 500 m reflectivity samples is written onto the wire by
/// [`replicated_sweep`].
#[derive(Clone, Copy)]
enum SampleForm {
    /// As they are: 500 m gates whose first is centred at 2250 m — the true
    /// geometry of a long-pulse sweep's content, declared honestly. Nothing
    /// on the wire looks like this; it is the oracle the wire form is scored
    /// against.
    Honest500m,
    /// The wire form a long-pulse volume actually uses: each sample duplicated
    /// onto two 250 m gates, the first centred at 2125 m.
    Replicated250m,
    /// The oracle with the registration error this whole exercise is about —
    /// 500 m samples filed from 2125 m, the first *sub*-gate's centre, rather
    /// than 2250 m, the pair's own.
    Honest500mMisregistered,
}

/// Declared first-gate range, metres, of a real long-pulse reflectivity
/// moment. The ICD's range to the **centre** of gate 0, so the declared grid's
/// leading edge is 2000 m.
const LONG_PULSE_FIRST_GATE_M: u16 = 2125;

/// One sweep carrying `samples` — 500 m-resolution gate bytes — written in
/// `form`, over `n_radials` evenly spaced azimuths, every radial identical, so
/// the grid this produces is a pure function of the range arithmetic.
///
/// `n_radials` is a parameter because [`replicated_pairs`] pools its evidence
/// over [`REPLICATION_SAMPLE_RADIALS`] radials rather than one, so a sweep's
/// radial count is part of how much evidence it can offer.
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

/// [`replicated_sweep_n`] over a full 360-radial sweep.
fn replicated_sweep(elevation_deg: f32, samples: &[u8], form: SampleForm) -> Sweep {
    replicated_sweep_n(360, elevation_deg, samples, form)
}

/// A run of 500 m samples with structure at every scale a bin can straddle:
/// a slow ramp so neighbours differ, and below-threshold gaps so the status
/// plane and the numeric-pair count are both exercised.
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

/// The elevation ceiling of the only VCPs that use a long pulse: every
/// long-pulse volume in the 158-volume corpus tops out between 4.44° and
/// 4.53°, VCP 34 included. `cos 4.5°` is 0.99692, so a 1 km **ground** bin
/// holds 4.0124 declared 250 m gates — which is the whole reason a replicated
/// pair ever straddles a bin edge, and the reason it so rarely does.
const LONG_PULSE_TOP_TILT_DEG: f32 = 4.5;

/// One sweep on the ground grid, as [`compute_echo_tops`]'s cube would build
/// it: the production statistic for reflectivity, and the filing the sweep's
/// own content asks for.
fn ground_grid(sweep: &Sweep) -> Vec<Vec<f32>> {
    let radials = sweep.radials();
    let binning = RangeBinning::Ground;
    sweep_to_grid(
        radials,
        RadarProduct::Reflectivity,
        CellStat::LinearZMean,
        binning.range_scale(radials),
        binning.gate_filing(radials, RadarProduct::Reflectivity),
    )
    .0
}

/// **The wire form and the honest form must grid to the same numbers.**
///
/// The oracle here is not a stored digest of this function's own output — it is
/// the *same physical content* declared a second, independent way: 500 m gates
/// at 500 m spacing, first centred at 2250 m, which is what a long-pulse
/// sweep's content actually is. If the pair walk reads the right gates and
/// files them at the right range, the two must agree bit for bit, and there is
/// nothing to pin because the reference is constructed rather than recorded.
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
    // And the comparison is not vacuous: the grids carry data.
    let defined = a.iter().flatten().filter(|v| !v.is_nan()).count();
    assert!(defined > 200, "only {defined} cells defined");
}

/// **The half-gate is not optional**, and this is what forgetting it costs.
///
/// Same content, same pair walk, filed from 2125 m — the first *sub*-gate's
/// centre — instead of 2250 m, the pair's own. Under the `cos e` scaling the
/// 125 m moves samples across bin edges, and this asserts the two conventions
/// are distinguishable so that picking the wrong one cannot pass unnoticed.
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

/// **Under [`RangeBinning::Slant`] the replication costs nothing**, which is
/// why [`crate::eet`] and [`crate::vil`] are left alone.
///
/// A 1 km slant bin holds declared gates `4r-8 ..= 4r-5`; the start index is
/// even, so it holds two whole replicated pairs and each 500 m sample is
/// weighted exactly once. Read as [`CellStat::Max`] — the statistic
/// [`crate::eet`] uses, and the one that cannot be confounded by summation
/// order — the wire form and the honest form must already agree without any
/// decimation at all.
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
            RangeBinning::Slant.range_scale(radials),
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

/// The detector recognises replication and declines a field that is merely
/// smooth.
///
/// The negative is deliberately far smoother than any measured sweep: gate `j`
/// carries `j / 7`, so 6 adjacent gates in 7 are equal — against a measured
/// short-pulse ceiling of 0.25 on numeric pairs. The parity structure is what
/// gives it away, not the agreement rate, and 7 is odd so a block boundary
/// eventually lands inside an even pair.
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

/// A sweep cannot qualify on too little evidence, and cannot qualify on
/// emptiness at all.
///
/// [`REPLICATION_MIN_PAIRS`] is a floor on gate pairs carrying **numbers**
/// precisely so that a cut whose every pair is two below-threshold sentinels —
/// which does satisfy "every pair is identical" — is not read as 500 m content.
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

/// The floor is a floor and not a wall: one pair over it is enough.
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

/// **Nothing that is not replicated is ever decimated**, which is the safety
/// property the short-pulse arm rests on: [`RangeBinning::gate_filing`] answers
/// [`GateFiling::AsDeclared`] for ordinary content under *both* binnings, and
/// for replicated content under [`RangeBinning::Slant`].
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
