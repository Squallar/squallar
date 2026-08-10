use super::*;
use crate::render::{render_from, render_radar_to_image_full};

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
///
/// `refl` and `vel` are per-radial byte generators; `None` leaves that
/// moment absent, which is how a surveillance cut is told from a Doppler
/// one.
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

/// Strong, uniform reflectivity — well past the echo-tops threshold so the
/// interpolated path paints.
fn strong_refl(_: usize) -> u8 {
    200
}

fn weaker_refl(_: usize) -> u8 {
    150
}

/// Eight cycles of ±35 m/s: enough azimuthal shear to survive the NROT fit,
/// the range normalization and the display threshold.
fn shear(i: usize) -> u8 {
    let theta = i as f64 / RADIALS as f64 * std::f64::consts::TAU;
    (129.0 + 35.0 * (8.0 * theta).sin() * 2.0)
        .round()
        .clamp(2.0, 254.0) as u8
}

/// A volume shaped like a real SAILS one: a 0.5° surveillance cut carrying
/// only reflectivity, a 0.5° Doppler cut carrying both, and a merged 1.5°
/// tilt carrying both.
///
/// The two 0.5° cuts are what make `find_sweep`'s surveillance preference
/// observable, and the cuts carrying *both* moments are what would catch a
/// payload that guessed at which moment to carry.
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
///
/// [`volume`] is shaped like a real SAILS volume and so carries only
/// reflectivity and velocity, which is all the products behind those two
/// moments need. `extract` refuses a product whose moment no sweep carries,
/// so a claim made about *every* product needs a volume where every field
/// is present — the gate values do not matter, only that they are there.
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
/// `f32::NAN != f32::NAN`, and the grid is NaN wherever no gate claimed the
/// pixel — which is most of it — so a naive compare would pass on two
/// entirely blank renders.
fn assert_same_frame(
    left: &(Vec<u8>, f64, Vec<f32>),
    right: &(Vec<u8>, f64, Vec<f32>),
    what: &str,
) {
    assert_eq!(left.0, right.0, "{what}: RGBA differs");
    assert_eq!(left.1, right.1, "{what}: max range differs");
    assert_eq!(
        left.2.len(),
        right.2.len(),
        "{what}: value grid length differs"
    );
    for (i, (a, b)) in left.2.iter().zip(&right.2).enumerate() {
        assert!(
            a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
            "{what}: value {i} differs: {a} vs {b}"
        );
    }
}

fn painted(frame: &(Vec<u8>, f64, Vec<f32>)) -> usize {
    frame.0.chunks_exact(4).filter(|px| px[3] != 0).count()
}

/// The storm motion override a storm-relative render carries, for the
/// products whose parity is asserted below. `None` for everything else:
/// only SRV reads it, and without one SRV would need the fixture volume
/// to support a Bunkers fit, which its two shallow tilts cannot.
fn override_for(product: RadarProduct) -> Option<(f32, f32)> {
    (product == RadarProduct::StormRelativeVelocity).then_some((30.0, 240.0))
}

/// The environmental heights a hail render carries — only the hail pair
/// reads them, and without a pair those products render nothing at all.
/// 2 / 4 km MSL sits the fixture's strong low tilt across the ramp.
fn env_for(product: RadarProduct) -> Option<(f64, f64)> {
    reads_env_heights(product).then_some((2.0, 4.0))
}

/// The acceptance criterion for moving rasterization into a worker: the
/// payload path and the whole-volume path produce the same frame, for every
/// product shape — one sweep, velocity-derived, and whole-volume.
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
        let direct =
            crate::render::render_radar_to_image_full(&scan, 0.5, product, LAT, LON, over, env)
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

/// A sweep that opens off its own tilt while the antenna settles: the first
/// thirty radials ramp from `first` to `flown`, the rest sit on `flown`, so
/// the median is `flown` and the first radial is not.
///
/// [`volume`] gives every sweep one constant elevation, which makes the two
/// readings the same number — so it cannot see the hazard the next test is
/// about, and neither could any fixture in this module before it.
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

/// The payload has to survive the port for a sweep that opened off its
/// tilt, and this is the tightest constraint on what `SweepData` may carry.
///
/// `to_scan` stamps one elevation onto every reconstructed radial, so
/// whatever `sweep_data` stored *is* the reconstructed sweep's median.
/// `find_sweep` matches on the median within a tenth of a degree, so
/// storing the first radial's angle — 0.3° from the tilt the request names —
/// would leave the worker unable to find the one sweep its own payload
/// contains, and the web path would render nothing at all. Constant-elevation
/// fixtures cannot fail this; this one can.
#[test]
fn a_sweep_that_opened_off_its_tilt_still_renders_after_the_port() {
    let scan = Scan::new(
        placeholder_coverage_pattern(0),
        vec![settling_sweep(1, 0.68, 0.44)],
    );
    let product = RadarProduct::Reflectivity;

    let direct =
        crate::render::render_radar_to_image_full(&scan, 0.4, product, LAT, LON, None, None)
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

/// The same, across the wire format the worker actually receives.
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

/// Storm-relative velocity is a Level II product now: it extracts, it
/// carries every velocity tilt (the profile is both its dealias seed and
/// its Bunkers input), and the override moves the field.
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

    // A different vector must change pixels: the override reaches the
    // arithmetic, not just the payload.
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
    assert_ne!(a.0, b.0, "the vector was carried but never applied");
}

/// A KDP payload's primary moment — ΦDP, the derivation's *source* slot —
/// survives extraction and reconstruction, with the estimator's gate
/// moments (Z, ρHV) riding the extras.
///
/// The reconstruction's slot resolution must mirror the extraction's:
/// `moment_slot()` alone is `None` for KDP, and the measured failure mode
/// was exactly this pair disagreeing — the extraction wrote ΦDP as the
/// primary payload, `to_scan` dropped it, and a live KDP volume
/// reconstructed with reflectivity and ρHV and no phase, refusing every
/// 3D build with nothing in the log.
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
                // ΦDP through its own codec, a mid-scale phase.
                Some(moment(2.8361, 2.0, 120, 600)),
                // ρHV near 1: the estimator's meteorological gate.
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

    // Through the wire too: the worker decodes bytes, not the struct.
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

/// `to_bytes` reserves exactly what it writes. Wrong by a little is only a
/// realloc; wrong by a lot means the layout and the estimate have drifted.
///
/// Both branches of the per-sweep cut angle have to be measured: the
/// `volume()` fixture has no cut table so every sweep writes the one-byte
/// absent form, and [`cut_table_volume`] has one so every sweep writes the
/// nine-byte present form. An estimate that had forgotten the angle
/// entirely would still match the first.
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
}

/// One elevation cut, angle only — the reconstruction's own
/// [`elevation_cut`] under a name the fixtures read.
fn cut(angle_deg: f64) -> ElevationCut {
    elevation_cut(angle_deg)
}

/// [`volume`], but flown under a VCP that declares its cuts — three
/// entries, and sweeps that name them 1, 2 and 3.
///
/// The declared angles are deliberately **not** the medians: 0.48 against a
/// 0.5 median, 0.51 against a 0.5, 1.47 against a 1.5. That is what real
/// data looks like (measured medians sit up to 0.044° off the declared cut)
/// and it is what tells a reconstruction that carried the cut table apart
/// from one that re-derived it from the sweeps.
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

/// The parts entry is the `Scan` entry, not a sibling of it: handing a
/// scan's own pattern and sweep list to [`RenderInput::extract_volume_parts`]
/// produces the byte-identical payload. `extract_volume` delegates, so a
/// drift between them means the delegation was undone and the merged path
/// has grown a second extraction that can disagree with the first.
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

/// The reconstruction carries the ladder key, and carries it *raw*.
///
/// Two fields, and both used to be wrong in ways nothing reported: the cut
/// table was empty, and `elevation_number` was the sweep's index in the
/// payload, so the first sweep claimed to be cut 0 — a number that cannot
/// index a 1-based table at all. `crate::sampler::VolumeSampler` reads both,
/// and the ladder it builds from them is not checkable against anything
/// once the sampler is gone.
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
    // And the angles are the *declared* ones, not the sweeps' medians —
    // which is the difference between carrying the table and re-deriving it.
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

/// **A volume that stopped part way up still knows how far up its pattern
/// goes.**
///
/// The reconstruction used to size the cut table to the largest elevation
/// number the payload carried, filling unnamed slots with a copy of the
/// nearest carried angle. That keys every carried sweep correctly, which is
/// all the ladder needs — and it silently makes the table's ceiling the
/// *volume's* ceiling. Every cross-section in the app is cut from a
/// reconstructed scan, so "did this volume reach the top of its pattern?"
/// answered yes for all of them, and a live section three rungs into VCP
/// 212 captioned itself as complete for the whole six minutes it was not.
///
/// Nothing about that failure is visible in the ladder: the rungs are
/// right, the heights are right, the raster is right. Only the sentence
/// underneath it is wrong, and it is wrong in the reassuring direction.
#[test]
fn a_part_flown_volume_still_carries_the_ceiling_its_pattern_declares() {
    // The first cut only, out of a three-cut table: a volume caught early.
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

    // And a complete volume through the same path still reports complete,
    // so the fix is not simply "always warn".
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

/// The carried-velocity bit survives the port, and materialises as a
/// **gateless** marker rather than as invented data.
///
/// The `volume()` fixture is a split cut: a surveillance 0.5° carrying only
/// reflectivity, a Doppler 0.5° carrying both, and a merged 1.5° carrying
/// both. A reflectivity payload ships none of the velocity, so the bit is
/// the only thing that can tell the sampler which sweep is which half.
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
    // And the marker is empty: a wind fit or a dealiaser reading it finds
    // nothing, rather than finding a number nobody measured.
    for sweep in rebuilt.sweeps().iter().skip(1) {
        let velocity = sweep.radials()[0].velocity().expect("marked");
        assert_eq!(velocity.raw_values().len(), 0, "the marker invented gates");
        assert_eq!(velocity.values().len(), 0);
    }
}

/// A cut below the horizon arrives from the decoder as ~359.7°, and the
/// sampler is what turns that into −0.3°.
///
/// So the payload must carry it **uncorrected**: correcting it here would
/// mean the correction ran once on the main thread and not at all in the
/// worker, and the two would key that cut differently — 359.7° sorts to the
/// top of the ladder, −0.3° to the bottom.
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

/// A payload from a volume whose cut table could not answer rebuilds an
/// **empty** table rather than inventing one.
///
/// That is what a volume joined mid-flight looks like — `crate::chunks`
/// stands in a pattern with no cuts until the start chunk lands — and the
/// sampler refuses it. Faithful includes faithfully unusable; the
/// alternative is a ladder in the worker the main thread would not have
/// built.
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

/// `extract_volume` carries every tilt carrying the moment, whatever
/// [`RadarProduct::reads_whole_volume`] says about the product.
///
/// Reflectivity is a one-sweep product — `a_plain_product_carries_one_sweep`
/// pins that for `extract` — so if the two constructors ever came to share
/// the tilt-scoped branch this would carry one sweep and a section would be
/// drawn from a single beam.
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
    // And the widening is by `||`: a product that already read the whole
    // volume still does.
    let nrot = RenderInput::extract_volume(&scan, RadarProduct::NormalizedRotation, LAT, LON)
        .expect("the fixture carries velocity");
    assert_eq!(nrot.sweeps.len(), 2, "both velocity tilts still travel");
}

/// A whole-volume payload handed to a *frame* consumer draws nothing — the
/// state every render path already handles — rather than silently drawing
/// whichever tilt happened to be nearest.
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

/// Why the sentinel is `-1000.0` and not either of the two obvious
/// alternatives.
///
/// The bar is not "no sweep in some fixture matches it". `find_sweep`
/// accepts any sweep whose median is within
/// [`crate::render::ELEVATION_WINDOW`], so a sentinel is only safe if it
/// sits that far outside **every angle an antenna can point at** — the
/// payload can be built from any volume, and a sentinel that is merely
/// unusual is one a volume eventually walks onto.
///
/// * `0.0` fails that outright: it is a legal elevation and a below-horizon
///   cut is a real thing (the wrap correction in
///   [`crate::sampler::VolumeSampler`] exists for exactly those). This test
///   builds a sweep 0.05° above the horizon and shows `0.0` claims it.
/// * `NaN` fails differently: `RenderInput` derives `PartialEq`, so a
///   whole-volume payload carrying one would be unequal to itself and every
///   round-trip assertion in this module would fail on it — the failure
///   `CrossSection` and `VoxelGrid` hand-write their `PartialEq` to avoid.
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
    // The general bar, rather than one fixture's worth of it: outside the
    // window of every angle an antenna can point at.
    assert!(
        f64::from(NO_ELEVATION_DEG).abs() > 90.0 + crate::render::ELEVATION_WINDOW,
        "{NO_ELEVATION_DEG} is inside the window of an angle a real \
             antenna can reach",
    );
    assert!(
        NO_ELEVATION_DEG.is_finite(),
        "a NaN sentinel breaks the derived PartialEq",
    );
    // Finite and exactly representable, so it survives the f32 wire field.
    let input =
        RenderInput::extract_volume(&cut_table_volume(), RadarProduct::Reflectivity, LAT, LON)
            .unwrap();
    assert_eq!(
        RenderInput::from_bytes(&input.to_bytes()).unwrap(),
        input,
        "a whole-volume payload is not equal to itself after the wire",
    );
}

/// The version is a *number on the wire*, not merely a check that exists.
///
/// Every other test here round-trips a payload through this build's own
/// codec, so all of them pass whatever the constant says — a version that
/// silently failed to bump when the layout changed would be invisible to
/// the entire module, and the two ends of a worker port are exactly where
/// that costs something. The literal below is the whole assertion: changing
/// the layout without changing it fails here.
///
/// The magic is written as a literal for the same reason. Asserting it
/// against `MAGIC` is self-consistency — the encoder writes that constant,
/// so any unused four bytes stayed green — and the relabel loop in
/// `a_malformed_payload_is_refused_rather_than_misread` only pins `RDRI`
/// against its two port-mates, which has nothing to say about a third
/// value. The far end of the port has no constant that moves with this
/// one. Mirrors `xsect`'s and `voxel`'s tests of the same name.
#[test]
fn the_format_version_is_the_one_this_layout_ships() {
    assert_eq!(FORMAT_VERSION, 8);
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
        8,
        "the version is not where a decoder from another build looks for it",
    );
}

/// A merged tilt carries reflectivity *and* velocity. Reading "whichever
/// moment this radial has" off it would hand a reflectivity render the
/// velocity gates — a frame that renders, looks like weather, and is wrong.
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

/// What travels is what [`RadarProduct::reads_whole_volume`] says travels,
/// for every product there is.
///
/// That predicate is also what the live chunk feed narrows a site's download
/// by, and the two used to be separate hand-maintained matches: the feed's
/// copy omitted storm-relative velocity, so a live SRV pane fit its dealias
/// seed and its default Bunkers vector from a volume the feed had
/// deliberately skipped cuts of — no error, no NaN, and archived volumes are
/// whole, so nothing under test saw it.
///
/// This asserts the half that lives here: that `extract` *reads* the
/// predicate for every product rather than deciding again, so a second copy
/// cannot grow back inside it. Whether the predicate's own answer is right
/// is a claim about the algorithms, and each whole-volume product's
/// individual test below is what pins that — every one of them fails if its
/// product is downgraded to a single sweep.
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

/// The sizing decision the whole design rests on: a normal product ships
/// one sweep, not the volume.
#[test]
fn a_plain_product_carries_one_sweep() {
    let scan = volume();
    let input =
        RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None).unwrap();
    assert_eq!(input.sweeps.len(), 1);
    assert_eq!(input.sweeps[0].radials.len(), RADIALS);
}

/// NROT fits its wind profile from every velocity tilt — that fit is the
/// only wind source since the NVW fetch left — so every velocity tilt has
/// to travel with the payload.
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

/// Interpolated echo tops integrate the volume; every reflectivity tilt has
/// to be there, in scan order, because `VolumeCube::build` dedups
/// same-elevation cuts by encounter.
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

/// A product with no Level II moment behind it renders nothing today, and
/// must not produce a payload that pretends otherwise.
#[test]
fn a_product_with_no_level_two_moment_extracts_nothing() {
    let scan = volume();
    assert!(
        RenderInput::extract(&scan, 0.5, RadarProduct::EchoTops, LAT, LON, None, None).is_none()
    );
    assert!(
        render_radar_to_image_full(&scan, 0.5, RadarProduct::EchoTops, LAT, LON, None, None)
            .is_none(),
        "the payload and the renderer must refuse the same requests"
    );
}

/// The hail pair without an environment: the payload still extracts —
/// the sweeps and the request are valid — but both render paths answer
/// nothing, the explicit "undefined field" seam (`crate::hail`), never a
/// zero-filled grid. The same payload with an environment paints.
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
            crate::render::render_radar_to_image_full(&scan, 0.5, product, LAT, LON, None, None,)
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

/// Every extras tag is pinned to a literal index, because the index *is*
/// the wire code.
///
/// The same discipline as [`RadarProduct::wire_code`] and
/// [`RenderView::wire_code`], for the one table in this module whose wire
/// codes are never written down: an extra's tag byte is its position in
/// [`ALL_SLOTS`], produced by `.enumerate()` in `sweep_data` and consumed
/// by `ALL_SLOTS.get(code as usize)` in `to_scan`. Both ends read the same
/// array, so **reordering it renumbers the wire consistently on both
/// sides** and no round-trip test can see it — `input == from_bytes(to_bytes(input))`
/// holds for any order, and so does every distinctness claim.
///
/// What a reorder costs is a misparse, not a refusal. Swap indices 2 and 3
/// and a stale worker's differential reflectivity arrives on the φDP field
/// of the HHC's reconstructed radial; the classifier reads a plausible
/// number off the wrong moment and produces a category field with no
/// `NaN`, no blank frame, and nothing to refuse. The literals below are
/// the only thing standing between that and a green suite.
#[test]
fn every_extras_slot_is_pinned_to_its_wire_index() {
    // Written out, not derived: deriving these from `ALL_SLOTS` would
    // re-import exactly the self-consistency this test exists to remove.
    let table: [(u8, MomentSlot); 6] = [
        (0, MomentSlot::Reflectivity),
        (1, MomentSlot::Velocity),
        (2, MomentSlot::SpectrumWidth),
        (3, MomentSlot::DifferentialReflectivity),
        (4, MomentSlot::DifferentialPhase),
        (5, MomentSlot::CorrelationCoefficient),
    ];
    for (code, slot) in table {
        // The encoder's side: `.enumerate()` hands out this position.
        assert_eq!(
            ALL_SLOTS[code as usize], slot,
            "wire index {code} is {:?} now, not {slot:?} — a stale worker's \
                 {:?} would land on {slot:?}'s field",
            ALL_SLOTS[code as usize], ALL_SLOTS[code as usize],
        );
        // The decoder's side, spelled the way `to_scan` spells it.
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
    // The N+1 guard, and it is the decoder's own bound: `to_scan` drops a
    // tag this answers `None` for, and `from_bytes` refuses the frame. If
    // this ever decodes, a slot was appended and the table above has
    // stopped being the whole wire.
    assert_eq!(ALL_SLOTS.get(table.len()), None);
    assert_eq!(ALL_SLOTS.get(u8::MAX as usize), None);
}

/// The hybrid classification's payload carries every sweep with every
/// moment (the extras), plus the environmental heights — and the whole
/// bundle survives the byte round trip. The fixture volume carries only
/// reflectivity and velocity, which is not enough to classify, so the
/// pin here is structural: the extras and heights are the parts version
/// 5 added.
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
    // The slot moment is reflectivity; velocity rides in the extras.
    let with_velocity = input.sweeps[1]
        .radials
        .iter()
        .filter(|r| r.extras.iter().any(|(code, _)| *code == 1))
        .count();
    assert!(with_velocity > 0, "the Doppler moment travels as an extra");
    let back = RenderInput::from_bytes(&input.to_bytes()).expect("round trips");
    assert_eq!(back, input);
    // And the reconstruction puts the extras back on their fields.
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

/// The bytes arrive off a message port. Every malformed shape has to be a
/// clean `None` — the two ends of that port can be different builds.
#[test]
fn a_malformed_payload_is_refused_rather_than_misread() {
    let scan = volume();
    let good = RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
        .unwrap()
        .to_bytes();

    assert!(RenderInput::from_bytes(&[]).is_none(), "empty");
    assert!(RenderInput::from_bytes(b"nope").is_none(), "wrong magic");

    // A **whole** payload relabelled, including with the two magics that
    // share this port. Mutation testing is why: the four-byte buffer above
    // cannot pin the magic test, because it runs out on the *version* read
    // whether or not the comparison exists, and the truncation loop below
    // never cuts inside the magic. Deleting `if r.take(4)? != MAGIC` left
    // the entire workspace green — so an `RDVX` grid or an `RDXS` section
    // arriving on the shared worker port would have been read as a render
    // input rather than refused. `render_input` was the last of the three
    // legs of that handshake without this loop; `voxel` and `xsect` both
    // caught the same mutation in themselves with it.
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

/// A corrupt length must not be believed far enough to reserve on it.
#[test]
fn an_absurd_length_does_not_reach_an_allocation() {
    let scan = volume();
    let mut bytes =
        RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
            .unwrap()
            .to_bytes();
    // The sweep count sits directly after the header and the
    // absent-override and absent-environment flag bytes.
    let at = 4 + 2 + 2 + 4 + 8 + 8 + 1 + 1;
    bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(RenderInput::from_bytes(&bytes).is_none());
}

/// Round-tripping through kilometres and back is exact for every value the
/// field can hold, which is what makes the reconstructed moment identical.
#[test]
fn gate_ranges_survive_the_kilometre_round_trip() {
    for raw in [0u16, 1, 250, 999, 2125, 32768, u16::MAX] {
        assert_eq!(km_to_metres(raw as f64 * 0.001), raw);
    }
}

/// The wire codes are a fixed table of *literals*, not the enum's
/// declaration order: reordering the variants must not silently change what
/// an already-encoded payload means.
///
/// The literals are the point. Distinctness and round-trip cannot see a
/// **renumbering** — swap two products' codes in both
/// [`RadarProduct::wire_code`] and [`RadarProduct::from_wire_code`] and both
/// properties still hold, which is exactly the four-line diff a
/// renumbering across a build boundary is. Nor can `from_wire_code`'s own
/// `debug_assert_eq!`, which compares the table against itself.
///
/// What a renumbering costs is a **misparse, not a refusal**. A stale
/// worker encodes reflectivity as `1`; a fresh page reads `1` as velocity;
/// the magic matches, the version matches, `moment_slot()` succeeds and
/// every length check passes, so the frame renders — reflectivity gates
/// under the velocity colour ramp, labelled "kt". This one `u16` sits in
/// the header of all three payloads that cross the worker port
/// ([`RenderInput`], `CrossSection`, `VoxelGrid`) and in every arm of
/// `rustdar_frontend::offload`'s job framing, so it is pinned here by
/// value, the way `SampleStatus`'s codes are in [`crate::sampler`].
///
/// Read the table below as the wire contract it is: a number in it is not
/// an implementation detail, and changing one changes what bytes already
/// in flight mean.
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
    // Precondition: the table above is the whole enum. A new variant that
    // reached `all()` without reaching the table would otherwise travel
    // unpinned, and 18 is the next number it would take.
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
