use super::*;
use crate::render::{SweepRender, render_from, render_radar_to_image_full};

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
/// The standard Level II reflectivity encoding: `dBZ = (raw - 66) / 2`.
const REFL_SCALE: f32 = 2.0;
const REFL_OFFSET: f32 = 66.0;
/// Velocity at 0.5 m/s resolution: `m/s = (raw - 129) / 2`.
const VEL_SCALE: f32 = 2.0;
const VEL_OFFSET: f32 = 129.0;
const RADIALS: usize = 360;

fn moment(scale: f32, offset: f32, byte: u8, gates: usize) -> MomentData {
    MomentData::from_fixed_point(gates as u16, 0, 250, 8, scale, offset, vec![byte; gates])
}

/// One sweep at `elevation`, `RADIALS` radials spaced evenly from 0°.
fn sweep(
    elevation: f32,
    refl: Option<&dyn Fn(usize) -> u8>,
    vel: Option<&dyn Fn(usize) -> u8>,
) -> Sweep {
    let radials = (0..RADIALS)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                i as f32 * (360.0 / RADIALS as f32),
                360.0 / RADIALS as f32,
                RadialStatus::IntermediateRadialData,
                1,
                elevation,
                refl.map(|f| moment(REFL_SCALE, REFL_OFFSET, f(i), 600)),
                vel.map(|f| moment(VEL_SCALE, VEL_OFFSET, f(i), 400)),
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

fn strong_refl(_: usize) -> u8 {
    200
}

fn weaker_refl(_: usize) -> u8 {
    150
}

fn shear(i: usize) -> u8 {
    let theta = i as f64 / RADIALS as f64 * std::f64::consts::TAU;
    (129.0 + 35.0 * (8.0 * theta).sin() * 2.0)
        .round()
        .clamp(2.0, 254.0) as u8
}

fn volume() -> Scan {
    Scan::new(
        placeholder_coverage_pattern(0),
        vec![
            sweep(0.5, Some(&strong_refl), None),
            sweep(0.5, Some(&weaker_refl), Some(&shear)),
            sweep(1.5, Some(&weaker_refl), Some(&shear)),
        ],
    )
}

/// One tilt at `elevation` carrying every moment a radial can hold.
fn every_moment_tilt(elevation: f32, number: u8) -> Sweep {
    let radials = (0..RADIALS)
        .map(|i| {
            let other = || Some(moment(1.0, 0.0, shear(i), 400));
            Radial::new(
                0,
                i as u16,
                i as f32 * (360.0 / RADIALS as f32),
                360.0 / RADIALS as f32,
                RadialStatus::IntermediateRadialData,
                number,
                elevation,
                Some(moment(REFL_SCALE, REFL_OFFSET, strong_refl(i), 600)),
                Some(moment(VEL_SCALE, VEL_OFFSET, shear(i), 400)),
                other(),
                other(),
                other(),
                other(),
                None,
            )
        })
        .collect();
    Sweep::new(number, radials)
}

/// Byte-for-byte on the image, element-for-element on the value grid.
fn assert_same_frame(left: &SweepRender, right: &SweepRender, what: &str) {
    assert_eq!(left.image, right.image, "{what}: RGBA differs");
    assert_eq!(
        left.max_range_km, right.max_range_km,
        "{what}: max range differs"
    );
    assert_eq!(
        left.nyquist_ms, right.nyquist_ms,
        "{what}: declared Nyquist differs"
    );
    assert_eq!(
        left.values.len(),
        right.values.len(),
        "{what}: value grid length differs"
    );
    for (i, (a, b)) in left.values.iter().zip(&right.values).enumerate() {
        assert!(
            a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
            "{what}: value {i} differs: {a} vs {b}"
        );
    }
}

fn painted(frame: &SweepRender) -> usize {
    frame.image.chunks_exact(4).filter(|px| px[3] != 0).count()
}

fn override_for(product: RadarProduct) -> Option<(f32, f32)> {
    (product == RadarProduct::StormRelativeVelocity).then_some((30.0, 240.0))
}

fn env_for(product: RadarProduct) -> Option<(f64, f64)> {
    product.reads_env_heights().then_some((2.0, 4.0))
}

#[test]
fn render_from_an_extracted_payload_matches_the_scan_path() {
    let scan = volume();
    for product in [
        RadarProduct::Reflectivity,
        RadarProduct::Velocity,
        RadarProduct::NormalizedRotation,
        RadarProduct::StormRelativeVelocity,
        RadarProduct::EchoTopsInterpolated,
        RadarProduct::ProbabilityOfSevereHail,
        RadarProduct::MaxExpectedHailSize,
    ] {
        let over = override_for(product);
        let env = env_for(product);
        let direct = crate::render::render_radar_to_image_full(
            &scan,
            0.5,
            product,
            LAT,
            LON,
            crate::srv::MotionInputs {
                user_override: over,
                ..Default::default()
            },
            env,
            None,
            &crate::nyquist::DeclaredNyquist::empty(),
        )
        .unwrap();
        let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, over, env).unwrap();
        let viaformat = render_from(&input).unwrap();

        assert!(
            painted(&direct) > 1_000,
            "{product:?} painted only {} pixels — the comparison would be vacuous",
            painted(&direct)
        );
        assert_same_frame(&direct, &viaformat, &format!("{product:?}"));
    }
}

fn settling_sweep(number: u8, first: f32, flown: f32) -> Sweep {
    const SETTLING: usize = 30;
    let radials = (0..RADIALS)
        .map(|i| {
            let elevation = if i < SETTLING {
                first + (flown - first) * (i as f32 / SETTLING as f32)
            } else {
                flown
            };
            Radial::new(
                0,
                i as u16,
                i as f32 * (360.0 / RADIALS as f32),
                360.0 / RADIALS as f32,
                RadialStatus::IntermediateRadialData,
                number,
                elevation,
                Some(moment(REFL_SCALE, REFL_OFFSET, strong_refl(i), 600)),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    Sweep::new(number, radials)
}

#[test]
fn a_sweep_that_opened_off_its_tilt_still_renders_after_the_port() {
    let scan = Scan::new(
        placeholder_coverage_pattern(0),
        vec![settling_sweep(1, 0.68, 0.44)],
    );
    let product = RadarProduct::Reflectivity;

    let direct = crate::render::render_radar_to_image_full(
        &scan,
        0.4,
        product,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &crate::nyquist::DeclaredNyquist::empty(),
    )
    .expect("the scan path draws the cut this volume flew");
    let input = RenderInput::extract(&scan, 0.4, product, LAT, LON, None, None)
        .expect("the payload extracts that same cut");
    assert!(
        (input.sweeps[0].elevation_angle - 0.44).abs() < 1e-4,
        "the payload must carry the tilt the sweep flew, not the one it opened on — got {}",
        input.sweeps[0].elevation_angle,
    );
    let reconstructed = input.to_scan();
    assert!(
        crate::render::find_sweep(&reconstructed, product, 0.4).is_some(),
        "the worker must find the one sweep its payload carries",
    );
    let via = render_from(&input).expect("the payload renders");
    assert!(
        painted(&direct) > 1_000,
        "the comparison would be vacuous — only {} pixels painted",
        painted(&direct),
    );
    assert_same_frame(&direct, &via, "a sweep that opened off its tilt");
}

#[test]
fn a_payload_renders_the_same_frame_after_a_round_trip() {
    let scan = volume();
    for product in [
        RadarProduct::Reflectivity,
        RadarProduct::Velocity,
        RadarProduct::NormalizedRotation,
        RadarProduct::StormRelativeVelocity,
        RadarProduct::EchoTopsInterpolated,
        RadarProduct::ProbabilityOfSevereHail,
        RadarProduct::MaxExpectedHailSize,
    ] {
        let input = RenderInput::extract(
            &scan,
            0.5,
            product,
            LAT,
            LON,
            override_for(product),
            env_for(product),
        )
        .unwrap();
        let decoded = RenderInput::from_bytes(&input.to_bytes())
            .unwrap_or_else(|| panic!("{product:?} payload did not decode"));
        assert_eq!(input, decoded, "{product:?} payload changed in transit");
        assert_eq!(
            decoded.storm_motion_override(),
            override_for(product),
            "{product:?}: the override must survive the wire",
        );
        assert_eq!(
            decoded.env_heights_km_msl(),
            env_for(product),
            "{product:?}: the environment must survive the wire",
        );
        assert_same_frame(
            &render_from(&input).unwrap(),
            &render_from(&decoded).unwrap(),
            &format!("{product:?} round trip"),
        );
    }
}

#[test]
fn srv_extracts_the_velocity_volume_and_honours_the_override() {
    let scan = volume();
    let input = RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::StormRelativeVelocity,
        LAT,
        LON,
        Some((30.0, 240.0)),
        None,
    )
    .unwrap();
    assert_eq!(input.sweeps.len(), 2, "both velocity tilts travel");
    assert_eq!(input.storm_motion_override(), Some((30.0, 240.0)));

    let other = RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::StormRelativeVelocity,
        LAT,
        LON,
        Some((30.0, 60.0)),
        None,
    )
    .unwrap();
    let a = render_from(&input).unwrap();
    let b = render_from(&other).unwrap();
    assert!(painted(&a) > 1_000);
    assert_ne!(a.image, b.image, "the vector was carried but never applied");
}

#[test]
fn the_kdp_payload_round_trips_its_phase() {
    let radials: Vec<Radial> = (0..RADIALS)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                i as f32 * (360.0 / RADIALS as f32),
                360.0 / RADIALS as f32,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                Some(moment(REFL_SCALE, REFL_OFFSET, 150, 600)),
                None,
                None,
                None,
                Some(moment(2.8361, 2.0, 120, 600)),
                Some(moment(300.0, -60.5, 237, 600)),
                None,
            )
        })
        .collect();
    let scan = Scan::new(
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
            vec![cut(0.5)],
        ),
        vec![Sweep::new(1, radials)],
    );
    let sweeps: Vec<&Sweep> = scan.sweeps().iter().collect();
    let input = RenderInput::extract_volume_parts(
        scan.coverage_pattern(),
        &sweeps,
        RadarProduct::SpecificDifferentialPhase,
        LAT,
        LON,
        None,
    )
    .expect("a \u{3a6}DP-carrying volume extracts for KDP");

    let decoded = RenderInput::from_bytes(&input.to_bytes()).expect("the payload round-trips");
    let back = decoded.to_scan();
    let first = &back.sweeps()[0].radials()[0];
    assert!(
        first.differential_phase().is_some(),
        "the primary source moment was dropped on reconstruction",
    );
    assert!(
        first.correlation_coefficient().is_some(),
        "the estimator's \u{3c1}HV gate must ride the extras",
    );
    assert!(
        first.reflectivity().is_some(),
        "the estimator's Z gate must ride the extras",
    );
}

#[test]
fn the_encoded_length_estimate_is_exact() {
    let scan = volume();
    for product in [RadarProduct::Reflectivity, RadarProduct::NormalizedRotation] {
        let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, None).unwrap();
        assert!(
            input.sweeps.iter().all(|s| s.cut_angle_deg.is_none()),
            "precondition: this fixture is supposed to have no cut table",
        );
        assert_eq!(input.encoded_len(), input.to_bytes().len(), "{product:?}");
    }

    let scan = cut_table_volume();
    for input in [
        RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None).unwrap(),
        RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON).unwrap(),
    ] {
        assert!(
            input.sweeps.iter().all(|s| s.cut_angle_deg.is_some()),
            "precondition: this fixture is supposed to have a cut table",
        );
        assert_eq!(input.encoded_len(), input.to_bytes().len());
    }

    // And both branches of the declared Nyquist velocity, for the same
    // reason: every payload above writes the one-byte absent form, so an
    // estimate that had forgotten the field's value entirely would match all
    // of them.
    let stamped = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
        .unwrap()
        .with_declared_nyquist(&[(1, 26.42)].into_iter().collect());
    assert!(
        stamped
            .sweeps
            .iter()
            .any(|s| s.declared_nyquist_ms.is_some()),
        "precondition: no sweep took the declaration, so the present form is \
         not being measured",
    );
    assert_eq!(stamped.encoded_len(), stamped.to_bytes().len());
}

#[test]
fn the_declared_nyquist_survives_the_wire_per_sweep() {
    let scan = cut_table_volume();
    let declared: crate::nyquist::DeclaredNyquist = [(1, 26.42), (2, 31.05)].into_iter().collect();
    let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
        .unwrap()
        .with_declared_nyquist(&declared);
    let carried = input.declared_nyquist();
    assert!(
        !carried.is_empty(),
        "precondition: the fixture's sweeps take none of the declarations, \
         so this test would pass on a codec that dropped the field",
    );

    let decoded = RenderInput::from_bytes(&input.to_bytes()).expect("the payload round-trips");
    assert_eq!(
        decoded.declared_nyquist().iter().collect::<Vec<_>>(),
        carried.iter().collect::<Vec<_>>(),
        "the declared Nyquist velocities did not cross the wire",
    );
    // Per sweep, not merely present somewhere: a codec that wrote one
    // sweep's value against every sweep would pass a whole-table
    // comparison on a volume whose cuts all fold at the same speed.
    assert_eq!(
        decoded
            .sweeps
            .iter()
            .map(|s| (s.elevation_number, s.declared_nyquist_ms))
            .collect::<Vec<_>>(),
        input
            .sweeps
            .iter()
            .map(|s| (s.elevation_number, s.declared_nyquist_ms))
            .collect::<Vec<_>>(),
    );
}

fn cut(angle_deg: f64) -> ElevationCut {
    elevation_cut(angle_deg)
}

fn cut_table_volume() -> Scan {
    let mut sweeps = vec![
        sweep(0.5, Some(&strong_refl), None),
        sweep(0.5, Some(&weaker_refl), Some(&shear)),
        sweep(1.5, Some(&weaker_refl), Some(&shear)),
    ];
    for (i, s) in sweeps.iter_mut().enumerate() {
        *s = Sweep::new(i as u8 + 1, s.radials().to_vec());
    }
    Scan::new(
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
            vec![cut(0.48), cut(0.51), cut(1.47)],
        ),
        sweeps,
    )
}

#[test]
fn extract_volume_parts_matches_extract_volume_byte_for_byte() {
    let scan = cut_table_volume();
    for product in [RadarProduct::Reflectivity, RadarProduct::Velocity] {
        let whole = RenderInput::extract_volume(&scan, product, LAT, LON)
            .expect("the fixture carries the moment");
        let sweeps: Vec<&Sweep> = scan.sweeps().iter().collect();
        let parts = RenderInput::extract_volume_parts(
            scan.coverage_pattern(),
            &sweeps,
            product,
            LAT,
            LON,
            None,
        )
        .expect("the same volume, as parts");
        assert_eq!(
            whole.to_bytes(),
            parts.to_bytes(),
            "{product:?}: the parts payload is not the scan payload",
        );
    }
}

#[test]
fn the_reconstruction_carries_the_cut_table_and_the_real_elevation_numbers() {
    let scan = cut_table_volume();
    let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
        .expect("the fixture carries reflectivity");
    let rebuilt = RenderInput::from_bytes(&input.to_bytes())
        .expect("the payload round-trips")
        .to_scan();

    assert_eq!(
        rebuilt
            .sweeps()
            .iter()
            .map(Sweep::elevation_number)
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the reconstructed sweeps do not name the cuts the originals named",
    );
    assert_eq!(
        rebuilt
            .coverage_pattern()
            .elevation_cuts()
            .iter()
            .map(ElevationCut::elevation_angle_degrees)
            .collect::<Vec<_>>(),
        vec![0.48, 0.51, 1.47],
        "the reconstructed cut table is not the original's",
    );
    assert_eq!(
        rebuilt.coverage_pattern().pattern_number().number(),
        212,
        "a rebuilt cut table under a VCP number nobody flew is worse than \
             no table at all",
    );
    assert!(
        rebuilt
            .coverage_pattern()
            .elevation_cuts()
            .iter()
            .zip(rebuilt.sweeps())
            .all(|(cut, sweep)| {
                let median =
                    crate::volumetric::sweep_elevation_deg(sweep.radials()).unwrap_or_default();
                (cut.elevation_angle_degrees() - median).abs() > 1e-6
            }),
        "every reconstructed cut angle equals its sweep's median, so this \
             test cannot tell a carried table from a re-derived one",
    );
}

#[test]
fn a_part_flown_volume_still_carries_the_ceiling_its_pattern_declares() {
    let whole = cut_table_volume();
    let part_flown = Scan::new(
        whole.coverage_pattern().clone(),
        vec![whole.sweeps()[0].clone()],
    );

    let input = RenderInput::extract_volume(&part_flown, RadarProduct::Reflectivity, LAT, LON)
        .expect("the fixture carries reflectivity");
    let rebuilt = RenderInput::from_bytes(&input.to_bytes())
        .expect("the payload round-trips")
        .to_scan();

    let angles: Vec<f64> = rebuilt
        .coverage_pattern()
        .elevation_cuts()
        .iter()
        .map(ElevationCut::elevation_angle_degrees)
        .collect();
    assert_eq!(
        angles,
        vec![0.48, 0.51, 1.47],
        "the reconstructed table stops where the volume stopped, so nothing \
             downstream can tell a truncated volume from a complete one",
    );
    assert_eq!(
        rebuilt.sweeps().len(),
        1,
        "precondition: only one cut was flown, so the table is longer than \
             anything that could have been derived from the sweeps",
    );

    // Which is the fact the sampler hands a section, and the one a caption
    // reads to decide whether the blank above the top rung is the cone of
    // silence or air nobody has looked at yet.
    let sampler = crate::sampler::VolumeSampler::new(&rebuilt, RadarProduct::Reflectivity)
        .expect("one cut is a ladder");
    assert_eq!(sampler.top_tilt_deg(), 0.48);
    assert_eq!(sampler.top_declared_cut_deg(), 1.47);
    assert!(
        sampler.top_tilt_deg() < sampler.top_declared_cut_deg(),
        "a one-rung volume out of a three-cut pattern reported a complete \
             ladder",
    );

    let complete = RenderInput::from_bytes(
        &RenderInput::extract_volume(&whole, RadarProduct::Reflectivity, LAT, LON)
            .expect("the fixture carries reflectivity")
            .to_bytes(),
    )
    .expect("the payload round-trips")
    .to_scan();
    let sampler = crate::sampler::VolumeSampler::new(&complete, RadarProduct::Reflectivity)
        .expect("three cuts are a ladder");
    assert_eq!(sampler.top_tilt_deg(), sampler.top_declared_cut_deg());
}

#[test]
fn the_doppler_half_is_still_recognisable_after_the_port() {
    let scan = volume();
    let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
        .expect("the fixture carries reflectivity");
    assert_eq!(
        input
            .sweeps
            .iter()
            .map(|s| s.carried_velocity)
            .collect::<Vec<_>>(),
        vec![false, true, true],
        "the bit does not match the fixture's split cut",
    );
    // precondition: none of the velocity itself travelled, so the bit is
    // doing the work rather than the data.
    assert!(
        input
            .sweeps
            .iter()
            .flat_map(|s| &s.radials)
            .all(|r| r.extras.is_empty()),
        "a reflectivity payload started carrying other moments, so this \
             test no longer measures what the bit is for",
    );

    let rebuilt = RenderInput::from_bytes(&input.to_bytes())
        .expect("round trips")
        .to_scan();
    let velocities: Vec<bool> = rebuilt
        .sweeps()
        .iter()
        .map(|s| s.radials()[0].velocity().is_some())
        .collect();
    assert_eq!(
        velocities,
        vec![false, true, true],
        "the reconstructed sweeps do not report the halves they were",
    );
    for sweep in rebuilt.sweeps().iter().skip(1) {
        let velocity = sweep.radials()[0].velocity().expect("marked");
        assert_eq!(velocity.raw_values().len(), 0, "the marker invented gates");
        assert_eq!(velocity.values().len(), 0);
    }
}

#[test]
fn a_below_horizon_cut_travels_uncorrected() {
    let scan = Scan::new(
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
            vec![cut(359.7)],
        ),
        vec![Sweep::new(
            1,
            sweep(-0.3, Some(&strong_refl), None).radials().to_vec(),
        )],
    );
    let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
        .expect("the fixture carries reflectivity");
    assert_eq!(input.sweeps[0].cut_angle_deg, Some(359.7));
    assert_eq!(
        input.to_scan().coverage_pattern().elevation_cuts()[0].elevation_angle_degrees(),
        359.7,
    );
}

#[test]
fn a_payload_with_no_cut_angles_rebuilds_an_empty_table() {
    let scan = volume();
    let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
        .expect("the fixture carries reflectivity");
    assert!(input.sweeps.iter().all(|s| s.cut_angle_deg.is_none()));
    assert!(
        input
            .to_scan()
            .coverage_pattern()
            .elevation_cuts()
            .is_empty(),
    );
}

#[test]
fn extract_volume_carries_every_tilt_whatever_the_product_says() {
    let scan = volume();
    assert!(
        !RadarProduct::Reflectivity.reads_whole_volume(),
        "precondition: reflectivity became a whole-volume product, so this \
             says nothing about the scope argument",
    );
    let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
        .expect("the fixture carries reflectivity");
    assert_eq!(input.sweeps.len(), scan.sweeps().len());
    let nrot = RenderInput::extract_volume(&scan, RadarProduct::NormalizedRotation, LAT, LON)
        .expect("the fixture carries velocity");
    assert_eq!(nrot.sweeps.len(), 2, "both velocity tilts still travel");
}

#[test]
fn a_whole_volume_payload_renders_no_frame() {
    let scan = cut_table_volume();
    let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
        .expect("the fixture carries reflectivity");
    assert_eq!(input.elevation(), NO_ELEVATION_DEG);
    assert!(
        render_from(&input).is_none(),
        "a section payload drew a plan-view frame",
    );
    // precondition: the payload is not empty, so what refuses above is the
    // elevation and not a missing sweep.
    assert_eq!(input.sweeps.len(), 3);
}

#[test]
fn the_sentinel_elevation_is_one_no_sweep_can_carry() {
    let near_horizon = Scan::new(
        placeholder_coverage_pattern(0),
        vec![Sweep::new(
            1,
            sweep(0.05, Some(&strong_refl), None).radials().to_vec(),
        )],
    );
    assert!(
        crate::render::find_sweep(&near_horizon, RadarProduct::Reflectivity, 0.0).is_some(),
        "0.0 is disqualified as a sentinel because a cut just above the \
             horizon claims it — if this stops being true, say so here rather \
             than quietly reverting the constant",
    );
    assert!(
        crate::render::find_sweep(&near_horizon, RadarProduct::Reflectivity, NO_ELEVATION_DEG)
            .is_none(),
    );
    assert!(
        f64::from(NO_ELEVATION_DEG).abs() > 90.0 + crate::render::ELEVATION_WINDOW,
        "{NO_ELEVATION_DEG} is inside the window of an angle a real \
             antenna can reach",
    );
    assert!(
        NO_ELEVATION_DEG.is_finite(),
        "a NaN sentinel breaks the derived PartialEq",
    );
    let input =
        RenderInput::extract_volume(&cut_table_volume(), RadarProduct::Reflectivity, LAT, LON)
            .unwrap();
    assert_eq!(
        RenderInput::from_bytes(&input.to_bytes()).unwrap(),
        input,
        "a whole-volume payload is not equal to itself after the wire",
    );
}

#[test]
fn the_format_version_is_the_one_this_layout_ships() {
    assert_eq!(FORMAT_VERSION, 12);
    let bytes = RenderInput::extract(
        &volume(),
        0.5,
        RadarProduct::Reflectivity,
        LAT,
        LON,
        None,
        None,
    )
    .unwrap()
    .to_bytes();
    assert_eq!(&bytes[..4], b"RDRI", "the magic moved");
    assert_eq!(
        u16::from_le_bytes([bytes[4], bytes[5]]),
        12,
        "the version is not where a decoder from another build looks for it",
    );
}

fn layout_fixture() -> RenderInput {
    let moment = |first: u8| MomentPayload {
        gate_count: 3,
        first_gate_range_m: 2125,
        gate_interval_m: 250,
        word_size: 8,
        scale: 2.0,
        offset: 66.0,
        gates: vec![first, 128, 255],
    };
    RenderInput {
        product: RadarProduct::Reflectivity,
        elevation: 0.5,
        radar_lat: 35.25,
        radar_lon: -97.5,
        storm_motion_override: Some((25.5, 220.0)),
        env_heights_km_msl: Some((3.5, 7.25)),
        melting_layer_product: Some(std::sync::Arc::new(vec![1, 2, 3, 4, 5])),
        rpg_storm_motion: Some((18.25, 195.5)),
        srv_fallback: crate::srv::SrvFallback::BunkersRightMover,
        vcp: 212,
        declared_cut_angles_deg: vec![0.5, 1.5, 359.75],
        sweeps: vec![
            SweepData {
                elevation_angle: 0.5,
                elevation_number: 1,
                cut_angle_deg: Some(0.5),
                carried_velocity: true,
                declared_nyquist_ms: Some(32.5),
                collected_ms: 1_600_000_000_000,
                radials: vec![
                    RadialData {
                        azimuth: 0.0,
                        azimuth_spacing: 0.5,
                        moment: Some(moment(7)),
                        extras: vec![(2, moment(9))],
                    },
                    RadialData {
                        azimuth: 90.25,
                        azimuth_spacing: 1.0,
                        moment: Some(moment(11)),
                        extras: vec![],
                    },
                ],
            },
            SweepData {
                elevation_angle: 1.5,
                elevation_number: 2,
                cut_angle_deg: None,
                carried_velocity: false,
                declared_nyquist_ms: None,
                collected_ms: -1,
                radials: vec![RadialData {
                    azimuth: 180.5,
                    azimuth_spacing: 0.25,
                    moment: None,
                    extras: vec![],
                }],
            },
        ],
    }
}

#[test]
fn the_wire_layout_is_the_one_this_version_ships() {
    let bytes = layout_fixture().to_bytes();
    assert_eq!(
        (
            FORMAT_VERSION,
            bytes.len(),
            crate::wire::layout_digest(&bytes)
        ),
        (12, 260, 0xaa29_1c4f_2a6e_feb5),
        "the bytes `to_bytes` writes are not the bytes version 12 shipped. \
         Something about this payload's layout moved — a field added, \
         removed, reordered, retyped, or written at a different width. That \
         is the change `FORMAT_VERSION` exists to announce, and a stale \
         worker that shares a build token with a fresh page (locally it \
         always does: `GITHUB_SHA` is absent outside CI, so the token \
         degrades to `…/dev`) will decode the new bytes into the old field \
         order — a site at the wrong coordinates, a ladder keyed to the \
         wrong cuts — with no error anywhere. Bump `FORMAT_VERSION`, then \
         write the new length and digest here — in that order, and never \
         the numbers alone.",
    );
}

#[test]
fn a_tilt_carrying_both_moments_still_yields_the_requested_one() {
    let scan = Scan::new(
        placeholder_coverage_pattern(0),
        vec![sweep(0.5, Some(&strong_refl), Some(&shear))],
    );
    let input =
        RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None).unwrap();
    let moment = input.sweeps[0].radials[0].moment.as_ref().unwrap();
    assert_eq!(moment.scale, REFL_SCALE);
    assert_eq!(moment.offset, REFL_OFFSET);
    assert_eq!(
        moment.gates[0],
        strong_refl(0),
        "carried the velocity gates under the reflectivity request"
    );
}

#[test]
fn every_product_carries_the_volume_exactly_when_it_says_it_reads_one() {
    let scan = Scan::new(
        placeholder_coverage_pattern(0),
        vec![
            every_moment_tilt(0.5, 1),
            every_moment_tilt(1.5, 2),
            every_moment_tilt(2.5, 3),
        ],
    );
    let tilts = scan.sweeps().len();
    assert!(
        tilts > 1,
        "precondition: with one tilt in the volume, carrying the volume and \
             carrying one sweep are the same payload and this says nothing"
    );

    for &product in RadarProduct::all() {
        let Some(input) = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, None) else {
            assert!(
                product.is_level3(),
                "{product:?} extracted nothing from a volume carrying every \
                     moment on every tilt"
            );
            continue;
        };
        let expected = if product.reads_whole_volume() {
            tilts
        } else {
            1
        };
        assert_eq!(
            input.sweeps.len(),
            expected,
            "{product:?}: reads_whole_volume() is {}, so {expected} of the \
                 volume's {tilts} tilts should have travelled",
            product.reads_whole_volume(),
        );
    }
}

#[test]
fn a_plain_product_carries_one_sweep() {
    let scan = volume();
    let input =
        RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None).unwrap();
    assert_eq!(input.sweeps.len(), 1);
    assert_eq!(input.sweeps[0].radials.len(), RADIALS);
}

#[test]
fn nrot_carries_every_velocity_tilt() {
    let scan = volume();
    let input = RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::NormalizedRotation,
        LAT,
        LON,
        None,
        None,
    )
    .unwrap();
    assert_eq!(input.sweeps.len(), 2, "both velocity tilts travel");
}

#[test]
fn interpolated_echo_tops_carries_every_reflectivity_tilt() {
    let scan = volume();
    let input = RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::EchoTopsInterpolated,
        LAT,
        LON,
        None,
        None,
    )
    .unwrap();
    assert_eq!(input.sweeps.len(), 3);
    assert_eq!(
        input
            .sweeps
            .iter()
            .map(|s| s.elevation_angle)
            .collect::<Vec<_>>(),
        vec![0.5, 0.5, 1.5],
        "scan order decides which same-elevation cut wins",
    );
}

#[test]
fn a_product_with_no_level_two_moment_extracts_nothing() {
    let scan = volume();
    assert!(
        RenderInput::extract(&scan, 0.5, RadarProduct::EchoTops, LAT, LON, None, None).is_none()
    );
    assert!(
        render_radar_to_image_full(
            &scan,
            0.5,
            RadarProduct::EchoTops,
            LAT,
            LON,
            crate::srv::MotionInputs::default(),
            None,
            None,
            &crate::nyquist::DeclaredNyquist::empty()
        )
        .is_none(),
        "the payload and the renderer must refuse the same requests"
    );
}

#[test]
fn hail_without_an_environment_renders_nothing_on_both_paths() {
    let scan = volume();
    for product in [
        RadarProduct::ProbabilityOfSevereHail,
        RadarProduct::MaxExpectedHailSize,
    ] {
        let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, None).unwrap();
        assert_eq!(input.env_heights_km_msl(), None);
        assert!(
            render_from(&input).is_none(),
            "{product:?} rendered without an environment"
        );
        assert!(
            crate::render::render_radar_to_image_full(
                &scan,
                0.5,
                product,
                LAT,
                LON,
                crate::srv::MotionInputs::default(),
                None,
                None,
                &crate::nyquist::DeclaredNyquist::empty()
            )
            .is_none(),
            "{product:?}: the payload and the renderer must refuse alike"
        );

        let with =
            RenderInput::extract(&scan, 0.5, product, LAT, LON, None, Some((2.0, 4.0))).unwrap();
        let frame = render_from(&with).unwrap();
        assert!(
            painted(&frame) > 1_000,
            "{product:?} with an environment must paint"
        );
    }
}

#[test]
fn every_extras_slot_is_pinned_to_its_wire_index() {
    let table: [(u8, MomentSlot); 6] = [
        (0, MomentSlot::Reflectivity),
        (1, MomentSlot::Velocity),
        (2, MomentSlot::SpectrumWidth),
        (3, MomentSlot::DifferentialReflectivity),
        (4, MomentSlot::DifferentialPhase),
        (5, MomentSlot::CorrelationCoefficient),
    ];
    for (code, slot) in table {
        assert_eq!(
            ALL_SLOTS[code as usize], slot,
            "wire index {code} is {:?} now, not {slot:?} — a stale worker's \
                 {:?} would land on {slot:?}'s field",
            ALL_SLOTS[code as usize], ALL_SLOTS[code as usize],
        );
        assert_eq!(
            ALL_SLOTS.get(code as usize),
            Some(&slot),
            "wire index {code} no longer decodes to {slot:?}",
        );
    }
    assert_eq!(
        table.len(),
        ALL_SLOTS.len(),
        "a moment slot joined `ALL_SLOTS` without being given a literal \
             wire index in the table above",
    );
    // The N+1 guard, and it is the decoder's own bound: `to_scan` drops a tag this
    // answers `None` for, and `from_bytes` refuses the frame.
    assert_eq!(ALL_SLOTS.get(table.len()), None);
    assert_eq!(ALL_SLOTS.get(u8::MAX as usize), None);
}

#[test]
fn hhc_payloads_carry_extras_and_env_heights() {
    let scan = volume();
    let input = RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::HydrometeorClassification,
        LAT,
        LON,
        None,
        Some((5.0, 8.6)),
    )
    .unwrap();
    assert_eq!(input.sweeps.len(), 3, "every sweep travels");
    assert_eq!(input.env_heights_km_msl(), Some((5.0, 8.6)));
    let with_velocity = input.sweeps[1]
        .radials
        .iter()
        .filter(|r| r.extras.iter().any(|(code, _)| *code == 1))
        .count();
    assert!(with_velocity > 0, "the Doppler moment travels as an extra");
    let back = RenderInput::from_bytes(&input.to_bytes()).expect("round trips");
    assert_eq!(back, input);
    let rebuilt = back.to_scan();
    let radial = &rebuilt.sweeps()[1].radials()[0];
    assert!(radial.reflectivity().is_some(), "slot moment placed");
    assert!(radial.velocity().is_some(), "extra placed on its field");
    // A non-HHC product never carries either, whatever the caller
    // passed — other products' payload bytes must not depend on an
    // unrelated cache.
    let refl = RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::Reflectivity,
        LAT,
        LON,
        None,
        Some((5.0, 8.6)),
    )
    .unwrap();
    assert_eq!(refl.env_heights_km_msl(), None);
    assert!(refl.sweeps[0].radials.iter().all(|r| r.extras.is_empty()));
}

#[test]
fn a_malformed_payload_is_refused_rather_than_misread() {
    let scan = volume();
    let good = RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
        .unwrap()
        .to_bytes();

    assert!(RenderInput::from_bytes(&[]).is_none(), "empty");
    assert!(RenderInput::from_bytes(b"nope").is_none(), "wrong magic");

    assert!(
        RenderInput::from_bytes(&good).is_some(),
        "precondition: the payload being relabelled has to decode as it \
             stands, or each refusal below could be for some other reason",
    );
    for wrong in [*b"nope", *b"RDVX", *b"RDXS"] {
        let mut relabelled = good.clone();
        relabelled[..4].copy_from_slice(&wrong);
        assert!(
            RenderInput::from_bytes(&relabelled).is_none(),
            "a whole payload labelled {} decoded as a render input",
            String::from_utf8_lossy(&wrong),
        );
    }

    let mut wrong_version = good.clone();
    wrong_version[4] = 0xFF;
    wrong_version[5] = 0xFF;
    assert!(RenderInput::from_bytes(&wrong_version).is_none(), "version");

    let mut wrong_product = good.clone();
    wrong_product[6] = 0xFE;
    wrong_product[7] = 0xFF;
    assert!(RenderInput::from_bytes(&wrong_product).is_none(), "product");

    for cut in [1, 8, 32, good.len() / 2, good.len() - 1] {
        assert!(
            RenderInput::from_bytes(&good[..cut]).is_none(),
            "truncated to {cut} bytes"
        );
    }

    let mut trailing = good.clone();
    trailing.push(0);
    assert!(
        RenderInput::from_bytes(&trailing).is_none(),
        "trailing bytes mean the layouts disagree"
    );
}

#[test]
fn an_absurd_length_does_not_reach_an_allocation() {
    let scan = volume();
    let mut bytes =
        RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
            .unwrap()
            .to_bytes();
    let at = 4 + 2 + 2 + 4 + 8 + 8 + 1 + 1;
    bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(RenderInput::from_bytes(&bytes).is_none());
}

#[test]
fn gate_ranges_survive_the_kilometre_round_trip() {
    for raw in [0u16, 1, 250, 999, 2125, 32768, u16::MAX] {
        assert_eq!(km_to_metres(raw as f64 * 0.001), raw);
    }
}

#[test]
fn every_product_has_a_stable_distinct_wire_code() {
    let table: [(RadarProduct, u16); 17] = [
        (RadarProduct::Reflectivity, 1),
        (RadarProduct::Velocity, 2),
        (RadarProduct::SpectrumWidth, 3),
        (RadarProduct::DifferentialPhase, 4),
        (RadarProduct::CorrelationCoefficient, 5),
        (RadarProduct::DifferentialReflectivity, 6),
        (RadarProduct::StormRelativeVelocity, 7),
        (RadarProduct::SpecificDifferentialPhase, 8),
        (RadarProduct::EchoTops, 9),
        (RadarProduct::EchoTopsInterpolated, 10),
        (RadarProduct::VerticallyIntegratedLiquid, 11),
        (RadarProduct::HydrometeorClassification, 12),
        (RadarProduct::PrecipitationRate, 13),
        (RadarProduct::NormalizedRotation, 14),
        (RadarProduct::VilDensity, 15),
        (RadarProduct::ProbabilityOfSevereHail, 16),
        (RadarProduct::MaxExpectedHailSize, 17),
    ];
    let mut seen = std::collections::HashSet::new();
    for (product, code) in table {
        assert_eq!(
            product.wire_code(),
            code,
            "{product:?} moved on the wire: it encodes as {} now, not {code}",
            product.wire_code(),
        );
        assert!(seen.insert(code), "{product:?} reuses wire code {code}");
        assert_eq!(
            RadarProduct::from_wire_code(code),
            Some(product),
            "wire code {code} no longer decodes to {product:?}",
        );
    }
    assert_eq!(RadarProduct::from_wire_code(0), None);
    assert_eq!(RadarProduct::from_wire_code(u16::MAX), None);
    assert_eq!(
        table.len(),
        RadarProduct::all().len(),
        "a product gained or lost a wire code without the table above moving",
    );
    assert_eq!(
        RadarProduct::from_wire_code(18),
        None,
        "18 decodes, so the table above has stopped being the whole wire",
    );
}

#[test]
fn the_derived_rung_choice_round_trips_and_only_for_srv() {
    use crate::srv::SrvFallback;
    let scan = volume();

    let bare = RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::StormRelativeVelocity,
        LAT,
        LON,
        None,
        None,
    )
    .expect("the fixture carries velocity");
    assert_eq!(bare.srv_fallback(), SrvFallback::MeanWind);

    for fallback in [SrvFallback::MeanWind, SrvFallback::BunkersRightMover] {
        let carried = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::StormRelativeVelocity,
            LAT,
            LON,
            None,
            None,
        )
        .expect("the fixture carries velocity")
        .with_srv_fallback(fallback);
        assert_eq!(carried.srv_fallback(), fallback);
        let after = RenderInput::from_bytes(&carried.to_bytes()).expect("round trips");
        assert_eq!(
            after.srv_fallback(),
            fallback,
            "{fallback:?} did not survive the port",
        );
        assert_eq!(after, carried);
        // And it reaches the chain the far end resolves through, not just the
        // accessor: a field that round-tripped but was never read would look
        // identical here and paint the wrong quantity.
        assert_eq!(after.storm_motion().fallback, fallback);
    }

    let reflectivity =
        RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
            .expect("the fixture carries reflectivity");
    assert_eq!(
        reflectivity
            .clone()
            .with_srv_fallback(SrvFallback::BunkersRightMover)
            .to_bytes(),
        reflectivity.to_bytes(),
    );
}

#[test]
fn the_melting_layer_object_round_trips_and_only_for_the_classification() {
    let scan = volume();
    let object = std::sync::Arc::new(vec![0xAAu8, 0xBB, 0xCC, 0xDD, 0xEE]);

    let carried = RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::HydrometeorClassification,
        LAT,
        LON,
        None,
        env_for(RadarProduct::HydrometeorClassification),
    )
    .expect("the fixture carries dual-pol")
    .with_melting_layer_product(Some(object.clone()));
    assert_eq!(
        carried.melting_layer_product().map(|o| o.as_slice()),
        Some(object.as_slice()),
    );
    let after = RenderInput::from_bytes(&carried.to_bytes()).expect("round trips");
    assert_eq!(
        after.melting_layer_product().map(|o| o.as_slice()),
        Some(object.as_slice()),
        "the object did not survive the port",
    );
    assert_eq!(after, carried);

    let bare = RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::HydrometeorClassification,
        LAT,
        LON,
        None,
        env_for(RadarProduct::HydrometeorClassification),
    )
    .expect("the fixture carries dual-pol");
    assert!(bare.melting_layer_product().is_none());
    assert!(
        RenderInput::from_bytes(&bare.to_bytes())
            .expect("round trips")
            .melting_layer_product()
            .is_none(),
    );

    for product in [
        RadarProduct::Reflectivity,
        RadarProduct::Velocity,
        RadarProduct::MaxExpectedHailSize,
    ] {
        let other = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, env_for(product))
            .expect("the fixture carries this moment")
            .with_melting_layer_product(Some(object.clone()));
        assert!(
            other.melting_layer_product().is_none(),
            "{product:?} must not carry a melting layer object",
        );
    }
}
fn leading_radial_blanked(sweep: &Sweep) -> Sweep {
    let radials = sweep
        .radials()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            Radial::new(
                r.collection_timestamp(),
                r.azimuth_number(),
                r.azimuth_angle_degrees(),
                r.azimuth_spacing_degrees(),
                r.radial_status(),
                r.elevation_number(),
                r.elevation_angle_degrees(),
                r.reflectivity().cloned(),
                if i == 0 { None } else { r.velocity().cloned() },
                r.spectrum_width().cloned(),
                r.differential_reflectivity().cloned(),
                r.differential_phase().cloned(),
                r.correlation_coefficient().cloned(),
                r.clutter_filter_power().cloned(),
            )
        })
        .collect();
    Sweep::new(sweep.elevation_number(), radials)
}

#[test]
fn a_sweep_whose_leading_radial_is_blank_still_reaches_the_worker() {
    let intact = volume();
    let maimed = Scan::new(
        intact.coverage_pattern().clone(),
        intact.sweeps().iter().map(leading_radial_blanked).collect(),
    );
    assert!(
        maimed.sweeps()[1].radials()[0].velocity().is_none(),
        "the fixture must be blank in front",
    );
    assert!(
        maimed.sweeps()[1].radials()[1].velocity().is_some(),
        "and only in front",
    );

    for product in [
        RadarProduct::StormRelativeVelocity,
        RadarProduct::NormalizedRotation,
        RadarProduct::Velocity,
    ] {
        let payload = RenderInput::extract(
            &maimed,
            0.5,
            product,
            LAT,
            LON,
            override_for(product),
            env_for(product),
        )
        .unwrap_or_else(|| {
            panic!("{product:?}: one blank leading radial hid the sweep from the port")
        });

        let expected = if product.reads_whole_volume() {
            crate::velocity::tilts(&maimed).count()
        } else {
            1
        };
        assert_eq!(
            crate::velocity::tilts(&payload.to_scan()).count(),
            expected,
            "{product:?}: the payload dropped a velocity tilt the scan carries",
        );
    }
}

#[test]
fn the_doppler_half_is_still_marked_when_its_leading_radial_is_blank() {
    let intact = volume();
    let maimed = Scan::new(
        intact.coverage_pattern().clone(),
        intact.sweeps().iter().map(leading_radial_blanked).collect(),
    );

    let payload = RenderInput::extract_volume(&maimed, RadarProduct::Reflectivity, LAT, LON)
        .expect("every sweep carries reflectivity");
    let rebuilt = payload.to_scan();

    let marked = rebuilt
        .sweeps()
        .iter()
        .filter(|s| s.radials().iter().any(|r| r.velocity().is_some()))
        .count();
    assert_eq!(
        marked, 2,
        "the two Doppler halves must still be recognisable after the port",
    );
}
