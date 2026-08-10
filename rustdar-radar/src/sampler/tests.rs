use super::*;
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, PulseWidth, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
};

// ── Fixtures ────────────────────────────────────────────────────────────
//
// Every fixture uses a **nonzero** first gate (2.125 km, the operational
// super-resolution value) rather than 0, because `first_gate_range_km` is
// a gate *centre* and a sampler that forgot it would be ~2 km — eight
// gates — inward on every read while still passing any test that started
// its gates at the origin.

const REFL_SCALE: f32 = 2.0;
const REFL_OFFSET: f32 = 66.0;
const VEL_SCALE: f32 = 2.0;
const VEL_OFFSET: f32 = 129.0;
const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;

/// dBZ through the reflectivity encoding. Clamped at 2 because 0 and 1 are
/// the below-threshold and range-folded status codes.
fn encode_refl(dbz: f64) -> u8 {
    ((dbz * f64::from(REFL_SCALE) + f64::from(REFL_OFFSET)).round() as i64).clamp(2, 255) as u8
}

/// What `encode_refl` round-trips to. Assertions compare against this
/// rather than against the dBZ that went in, so a 0.5 dB quantisation step
/// is not mistaken for a sampler error.
fn round_trip_refl(dbz: f64) -> f64 {
    f64::from((f32::from(encode_refl(dbz)) - REFL_OFFSET) / REFL_SCALE)
}

fn encode_vel(ms: f64) -> u8 {
    ((ms * f64::from(VEL_SCALE) + f64::from(VEL_OFFSET)).round() as i64).clamp(2, 255) as u8
}

fn round_trip_vel(ms: f64) -> f64 {
    f64::from((f32::from(encode_vel(ms)) - VEL_OFFSET) / VEL_SCALE)
}

/// The slant range, km, of gate `j` in every fixture below.
fn gate_slant_km(j: usize) -> f64 {
    f64::from(FIRST_GATE_M) / 1000.0 + j as f64 * f64::from(GATE_M) / 1000.0
}

fn moment_from(bytes: Vec<u8>, scale: f32, offset: f32) -> MomentData {
    MomentData::from_fixed_point(
        bytes.len() as u16,
        FIRST_GATE_M,
        GATE_M,
        8,
        scale,
        offset,
        bytes,
    )
}

/// A field to plant: dBZ (or m/s) at an azimuth and slant range, or `None`
/// for below threshold.
type Field<'f> = &'f dyn Fn(f64, f64) -> Option<f64>;

/// One sweep, carrying whichever of the two moments is asked for.
///
/// Azimuths are `i · 360/n`, so a 720-radial sweep sits on exact halves of
/// a degree and a query at a radial centre lands on it bit-exactly.
fn make_sweep(
    elevation_number: u8,
    elevation_deg: f32,
    n_radials: usize,
    n_gates: usize,
    refl: Option<Field<'_>>,
    vel: Option<Field<'_>>,
) -> Sweep {
    let spacing = 360.0 / n_radials as f32;
    let radials = (0..n_radials)
        .map(|i| {
            let az = i as f32 * spacing;
            let build = |f: Field<'_>, scale: f32, offset: f32, enc: &dyn Fn(f64) -> u8| {
                let bytes: Vec<u8> = (0..n_gates)
                    .map(|j| match f(f64::from(az), gate_slant_km(j)) {
                        None => 0,
                        Some(v) => enc(v),
                    })
                    .collect();
                moment_from(bytes, scale, offset)
            };
            Radial::new(
                0,
                i as u16,
                az,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                refl.map(|f| build(f, REFL_SCALE, REFL_OFFSET, &encode_refl)),
                vel.map(|f| build(f, VEL_SCALE, VEL_OFFSET, &encode_vel)),
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

/// A reflectivity-only sweep of a constant field.
fn flat_refl_sweep(
    elevation_number: u8,
    elevation_deg: f32,
    n_radials: usize,
    n_gates: usize,
    dbz: f64,
) -> Sweep {
    make_sweep(
        elevation_number,
        elevation_deg,
        n_radials,
        n_gates,
        Some(&move |_, _| Some(dbz)),
        None,
    )
}

/// A reflectivity sweep with **explicit azimuths in collection order**.
///
/// Collection order is not azimuth order: a real sweep starts wherever the
/// antenna was and wraps through 0°, which is what makes the by-azimuth
/// index a real index rather than a copy of the radial list. Azimuths that
/// are evenly spaced and start at 0 hide every ordering bug there is.
fn refl_sweep_at(
    elevation_number: u8,
    elevation_deg: f32,
    azimuths: &[f32],
    n_gates: usize,
    dbz: impl Fn(f64) -> f64,
) -> Sweep {
    let spacing = 360.0 / azimuths.len() as f32;
    let radials = azimuths
        .iter()
        .enumerate()
        .map(|(i, &az)| {
            let bytes = vec![encode_refl(dbz(f64::from(az))); n_gates];
            Radial::new(
                0,
                i as u16,
                az,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                Some(moment_from(bytes, REFL_SCALE, REFL_OFFSET)),
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

/// A velocity-only sweep of a constant field — the Doppler half of a split
/// cut, and the shape a SAILS repeat of a Doppler cut takes.
fn flat_velocity_sweep(elevation_number: u8, elevation_deg: f32, ms: f64) -> Sweep {
    make_sweep(
        elevation_number,
        elevation_deg,
        360,
        200,
        None,
        Some(&move |_, _| Some(ms)),
    )
}

fn cut(angle_deg: f64) -> ElevationCut {
    ElevationCut::new(
        angle_deg,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        20.0,
        true,
        true,
        false,
        false,
        1,
        20,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        0,
        false,
        0,
        false,
        false,
    )
}

fn vcp(cut_angles: &[f64]) -> VolumeCoveragePattern {
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
        cut_angles.iter().copied().map(cut).collect(),
    )
}

// ── The tilt ladder ─────────────────────────────────────────────────────

/// The rule the whole campaign settled on, in the one geometry that proves
/// no angular threshold can substitute for it.
///
/// KBMX under VCP 212 with the adaptive base tilt declares genuine cuts at
/// **0.40° and 0.48° — 0.09° apart** — while the spread of first-radial
/// angles *within* the 0.48° cut is 0.088° and the gap to the 0.40° cut is
/// also 0.088°. The windows touch exactly. Reproduced here with medians
/// 0.09° apart, which is what the fixture asserts as a precondition: any
/// threshold wide enough to close a cut's own spread also swallows a whole
/// genuine cut, and at 0.2° the failure is not a merged pair but a
/// *vanished* rung inside a plausible monotone ladder.
#[test]
fn the_ladder_separates_cuts_no_angular_threshold_can() {
    let scan = Scan::new(
        vcp(&[0.40, 0.48, 0.90]),
        vec![
            flat_refl_sweep(1, 0.44, 360, 40, 20.0),
            flat_refl_sweep(2, 0.53, 360, 40, 40.0),
            flat_refl_sweep(3, 0.91, 360, 40, 30.0),
        ],
    );

    let separation = 0.53 - 0.44;
    // precondition: the two cuts really are inside every threshold the
    // campaign measured (0.10 / 0.15 / 0.20 / 0.30), so a rule that split
    // them cannot have done it by angle.
    assert!(
        separation < 0.10,
        "precondition: the fixture's cuts are {separation:.3}° apart, \
             which the 0.10° threshold would already separate — the test no \
             longer proves anything about thresholds",
    );

    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    assert_eq!(
        sampler.tilt_count(),
        3,
        "the cut table declares three cuts and three sweeps arrived; the \
             ladder found {} rungs at elevations {:?}",
        sampler.tilt_count(),
        sampler.elevations_deg().collect::<Vec<_>>(),
    );
    let nominal: Vec<f64> = sampler.nominal_elevations_deg().collect();
    assert_eq!(nominal, vec![0.40, 0.48, 0.90]);

    // And each rung really carries its own sweep's data, which is what
    // "the 0.48° cut vanished" would have destroyed.
    let column = sampler.column(45.0, 8.0);
    let values: Vec<f64> = column
        .rungs()
        .iter()
        .map(|r| f64::from(r.sample.value().expect("every rung has data at 8 km")))
        .collect();
    assert_eq!(
        values,
        vec![
            round_trip_refl(20.0),
            round_trip_refl(40.0),
            round_trip_refl(30.0)
        ],
    );
}

/// The nominal cut angle is the grouping key and nothing else: the
/// geometry is the chosen sweep's median radial elevation, which measured
/// volumes put up to 0.044° off nominal.
#[test]
fn a_rungs_geometry_is_its_sweeps_median_not_the_nominal_cut() {
    let scan = Scan::new(
        vcp(&[0.5, 4.0]),
        vec![
            flat_refl_sweep(1, 0.544, 360, 40, 20.0),
            flat_refl_sweep(2, 3.968, 360, 40, 20.0),
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

    let geometry: Vec<f64> = sampler.elevations_deg().collect();
    let nominal: Vec<f64> = sampler.nominal_elevations_deg().collect();
    assert_eq!(nominal, vec![0.5, 4.0]);
    for (g, n) in geometry.iter().zip(&nominal) {
        let off = (g - n).abs();
        assert!(
            (0.03..0.05).contains(&off),
            "the fixture planted a ~0.044° offset but the rung reports \
                 {g}° against a nominal {n}° ({off}° apart) — the ladder is \
                 using the key as geometry",
        );
    }

    // The consequence, in metres: at 100 km the 0.032° offset on the 4.0°
    // cut moves the beam centre far enough to matter, and exactly the kind
    // of error that reads as plausible.
    let with_median = beam::height_at_ground_km(100.0, geometry[1]);
    let with_nominal = beam::height_at_ground_km(100.0, nominal[1]);
    assert!(
        (with_median - with_nominal).abs() * 1000.0 > 40.0,
        "the median/nominal height gap at 100 km is only {:.1} m, so this \
             distinction stopped mattering",
        (with_median - with_nominal).abs() * 1000.0,
    );
}

/// A split cut is two VCP cuts at one angle: a surveillance half reaching
/// 460 km with no velocity, and a Doppler half reaching 300 km with it.
/// Reflectivity belongs to the surveillance half; velocity has no choice
/// but the Doppler one.
#[test]
fn a_non_doppler_moment_takes_the_surveillance_half_of_a_split_cut() {
    // 1832 gates from 2.125 km at 250 m reaches 460 km; 1200 reaches 302.
    let scan = Scan::new(
        vcp(&[0.5, 0.5, 0.9]),
        vec![
            make_sweep(1, 0.5, 360, 1832, Some(&|_, _| Some(20.0)), None),
            make_sweep(
                2,
                0.5,
                360,
                1200,
                Some(&|_, _| Some(45.0)),
                Some(&|_, _| Some(10.0)),
            ),
            make_sweep(3, 0.9, 360, 1200, Some(&|_, _| Some(30.0)), None),
        ],
    );

    let refl = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    assert_eq!(refl.tilt_count(), 2, "the two 0.5° cuts are one rung");
    let low = refl.column(45.0, 100.0).rungs()[0].sample;
    assert_eq!(
        f64::from(low.value().expect("the surveillance half has 100 km")),
        round_trip_refl(20.0),
        "reflectivity came from the Doppler half of the split cut",
    );

    // The reason it matters: only the surveillance half reaches past
    // 300 km, and the Doppler half would have reported nothing there.
    let far = refl.column(45.0, 400.0).rungs()[0].sample;
    assert_eq!(
        f64::from(far.value().expect("460 km of surveillance gates")),
        round_trip_refl(20.0),
    );

    // The preference is a *preference*: an upper cut is a single merged
    // sweep carrying everything, so there is no velocity-free half to
    // prefer and reflectivity falls back to the newest sweep that has it.
    // Two merged cuts at one angle is what MRLE produces.
    let merged = Scan::new(
        vcp(&[4.0, 4.0]),
        vec![
            make_sweep(
                1,
                4.0,
                360,
                1200,
                Some(&|_, _| Some(20.0)),
                Some(&|_, _| Some(3.0)),
            ),
            make_sweep(
                2,
                4.0,
                360,
                1200,
                Some(&|_, _| Some(45.0)),
                Some(&|_, _| Some(7.0)),
            ),
        ],
    );
    let merged_refl = VolumeSampler::new(&merged, RadarProduct::Reflectivity).unwrap();
    assert_eq!(merged_refl.tilt_count(), 1);
    assert_eq!(
        f64::from(
            merged_refl.column(45.0, 100.0).rungs()[0]
                .sample
                .value()
                .unwrap()
        ),
        round_trip_refl(45.0),
        "with no velocity-free half to prefer, reflectivity did not fall \
             back to the newest sweep",
    );

    // Velocity has one candidate and takes it.
    let vel = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    assert_eq!(
        vel.tilt_count(),
        1,
        "only the Doppler half of the 0.5° cut carries velocity, and the \
             0.9° cut carries none",
    );
    let v = vel.column(45.0, 100.0).rungs()[0].sample;
    assert_eq!(
        f64::from(v.value().expect("the Doppler half has 100 km")),
        round_trip_vel(10.0),
    );
}

/// SAILS repeats the low cuts minutes apart. The newest is what the
/// reference display shows and what a section must show.
#[test]
fn the_newest_sweep_of_a_repeated_cut_wins_its_rung() {
    let scan = Scan::new(
        vcp(&[0.5, 0.9, 0.5]),
        vec![
            flat_refl_sweep(1, 0.5, 360, 40, 20.0),
            flat_refl_sweep(2, 0.9, 360, 40, 30.0),
            flat_refl_sweep(3, 0.5, 360, 40, 55.0), // the SAILS repeat
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    assert_eq!(sampler.tilt_count(), 2);
    assert_eq!(
        f64::from(sampler.column(45.0, 8.0).rungs()[0].sample.value().unwrap()),
        round_trip_refl(55.0),
        "the rung kept the first 0.5° cut rather than the SAILS repeat",
    );

    // The Doppler arm takes the same preference and is reached by a
    // different branch, so it needs its own volume: SAILS repeats the
    // Doppler cuts too.
    let scan = Scan::new(
        vcp(&[0.5, 0.9, 0.5]),
        vec![
            flat_velocity_sweep(1, 0.5, 5.0),
            flat_refl_sweep(2, 0.9, 360, 200, 30.0),
            flat_velocity_sweep(3, 0.5, 25.0), // the SAILS repeat
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    assert_eq!(
        sampler.tilt_count(),
        1,
        "only the 0.5° cut carries velocity"
    );
    assert_eq!(
        f64::from(sampler.column(45.0, 8.0).rungs()[0].sample.value().unwrap()),
        round_trip_vel(25.0),
        "the Doppler rung kept the first 0.5° cut rather than the SAILS \
             repeat",
    );
}

/// A volume joined mid-flight starts partway up the ladder and wraps into
/// the next one, so its sweeps do not arrive in cut order. The ladder has
/// to be ascending anyway — a section reads its rows off it, and a
/// descending pair inverts every bracket in the column.
///
/// One of the 19 mid-flight-join variants the ladder rule was scored on.
#[test]
fn a_volume_joined_mid_flight_still_yields_an_ascending_ladder() {
    let scan = Scan::new(
        vcp(&[0.5, 0.9, 1.3]),
        vec![
            // Joined at the 0.9° cut, then 1.3°, then the next volume's
            // 0.5°.
            flat_refl_sweep(2, 0.9, 360, 200, 30.0),
            flat_refl_sweep(3, 1.3, 360, 200, 40.0),
            flat_refl_sweep(1, 0.5, 360, 200, 20.0),
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    let nominal: Vec<f64> = sampler.nominal_elevations_deg().collect();
    assert_eq!(
        nominal,
        vec![0.5, 0.9, 1.3],
        "the ladder came out in volume order rather than ascending",
    );
    let column = sampler.column(45.0, 20.0);
    let values: Vec<f64> = column
        .rungs()
        .iter()
        .map(|r| f64::from(r.sample.value().unwrap()))
        .collect();
    assert_eq!(
        values,
        vec![
            round_trip_refl(20.0),
            round_trip_refl(30.0),
            round_trip_refl(40.0)
        ],
        "a rung is carrying another cut's data",
    );
}

/// A cut angle that is not a number would fail every grouping comparison
/// and scatter one cut across as many rungs as it has sweeps, with a
/// ladder that still looks the right length.
#[test]
fn a_non_finite_cut_angle_is_refused() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let scan = Scan::new(
            vcp(&[bad, 0.9]),
            vec![
                flat_refl_sweep(1, 0.5, 360, 40, 20.0),
                flat_refl_sweep(2, 0.9, 360, 40, 30.0),
            ],
        );
        let err = VolumeSampler::new(&scan, RadarProduct::Reflectivity)
            .expect_err("a non-finite cut angle built a ladder");
        assert!(
            matches!(err, SamplerError::NonFiniteCutAngle { cut_index: 0, .. }),
            "expected the non-finite refusal for {bad}, got {err:?}",
        );
    }
}

/// The cut table stores a below-horizon angle as a two's-complement value
/// this decoder hands back unsigned, so −0.3° arrives as 359.7°. Left
/// uncorrected it sorts above 19.5° and inverts the whole ladder.
#[test]
fn a_cut_angle_past_180_degrees_wraps_to_a_negative_elevation() {
    let scan = Scan::new(
        vcp(&[359.7, 0.5, 4.0]),
        vec![
            flat_refl_sweep(1, -0.28, 360, 40, 20.0),
            flat_refl_sweep(2, 0.52, 360, 40, 30.0),
            flat_refl_sweep(3, 4.02, 360, 40, 40.0),
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    let nominal: Vec<f64> = sampler.nominal_elevations_deg().collect();
    assert!(
        (nominal[0] - -0.3).abs() < 1e-9,
        "359.7° did not wrap to −0.3°: the ladder reads {nominal:?}",
    );
    assert_eq!(nominal.len(), 3);
    assert!(
        nominal.windows(2).all(|w| w[0] < w[1]),
        "the ladder is not ascending: {nominal:?}",
    );
    // Without the correction the 359.7° cut sorts to the top, so the
    // highest rung would be 359.7° rather than 4.0°.
    assert!(
        nominal[2] < 180.0,
        "an unwrapped cut angle is still in the ladder: {nominal:?}",
    );
}

/// The *declared* ceiling needs the same wrap correction the ladder's keys
/// get, and nothing else in the suite notices when it is missing.
///
/// The cut table is read twice — once per sweep to key a rung, once over
/// the whole table for [`VolumeSampler::top_declared_cut_deg`] — and only
/// the first read is covered by
/// `a_cut_angle_past_180_degrees_wraps_to_a_negative_elevation`. Drop the
/// correction from the second and the ladder is still perfect; what breaks
/// is the comparison a caller makes against it.
///
/// The cuts here are KMSX's, which declares its base tilt at **359.82°** —
/// a real below-horizon cut at a real site, not a constructed one. Left
/// unwrapped it is the table's largest number, so `top_declared_cut_deg`
/// reports 359.8° for a volume that flew its pattern to the top, every
/// section caption reads "topping out at 19.5° of the 359.8°", and
/// `describe_missing` calls the cone of silence unflown air — for **every**
/// volume at every site whose base tilt is below the horizon.
#[test]
fn a_below_horizon_declared_cut_does_not_become_the_declared_ceiling() {
    let scan = Scan::new(
        vcp(&[359.82, 0.48, 0.88, 1.31, 19.5]),
        vec![
            flat_refl_sweep(1, -0.16, 360, 40, 10.0),
            flat_refl_sweep(2, 0.51, 360, 40, 20.0),
            flat_refl_sweep(3, 0.90, 360, 40, 30.0),
            flat_refl_sweep(4, 1.33, 360, 40, 40.0),
            flat_refl_sweep(5, 19.52, 360, 40, 50.0),
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    assert_eq!(
        sampler.top_declared_cut_deg(),
        19.5,
        "the pattern's declared ceiling is a below-horizon cut read \
             unsigned: the ladder is {sampler:?}",
    );
    // The two are compared for equality by every consumer of a short
    // ladder, so the point of the assertion above is that they agree here:
    // this volume flew its pattern to the top and must read as complete.
    assert_eq!(
        sampler.top_tilt_deg(),
        sampler.top_declared_cut_deg(),
        "a complete volume reads as short of its own pattern",
    );
}

/// A volume shaped like a real SAILS one, with the cut table that separates
/// its two base tilts.
///
/// Six sweeps over six declared cuts, carrying every hazard the ladder rule
/// exists for:
///
/// * a below-horizon 359.7° cut that only the wrap correction reads as
///   −0.3°;
/// * **two genuine base tilts declared 0.09° apart** (0.40° and 0.48°), the
///   KBMX adaptive-base-tilt geometry no angular threshold can separate;
/// * a **split 0.48° cut** — a long-range surveillance half carrying no
///   velocity, and a short-range Doppler half carrying it — plus a SAILS
///   Doppler repeat of the same cut that is *newer* than both.
///
/// The split cut is shaped the way a real one is, which is the part that
/// matters: all three 0.48° members share the cut angle **and the median**
/// (0.53°, exactly as KMPX's three 0.4834° members all measure 0.4834°), so
/// the only thing that distinguishes the surveillance half is its range —
/// [`LONG_GATES`] against [`SHORT_GATES`], standing in for 1832 gates
/// (460 km) against 1192 (300 km). A ladder that took the wrong half is
/// therefore invisible in the angles and visible only in the gate count,
/// which is why [`VolumeSampler::describe`] prints one.
const LONG_GATES: usize = 120;
const SHORT_GATES: usize = 40;

fn sails_volume() -> Scan {
    let refl = |dbz: f64| move |_: f64, _: f64| Some(dbz);
    Scan::new(
        vcp(&[359.7, 0.40, 0.48, 0.48, 1.5, 0.48]),
        vec![
            make_sweep(
                1,
                -0.28,
                360,
                SHORT_GATES,
                Some(&refl(15.0)),
                Some(&|_, _| Some(7.0)),
            ),
            make_sweep(2, 0.44, 360, LONG_GATES, Some(&refl(20.0)), None),
            // The surveillance half of the split cut: no velocity, and the
            // only member that reaches past 300 km. It must win the rung.
            make_sweep(3, 0.53, 360, LONG_GATES, Some(&refl(25.0)), None),
            // Its Doppler half: the same angle, the same median, a short
            // copy of the reflectivity, and velocity.
            make_sweep(
                4,
                0.53,
                360,
                SHORT_GATES,
                Some(&refl(26.0)),
                Some(&|_, _| Some(9.0)),
            ),
            make_sweep(
                5,
                1.51,
                360,
                SHORT_GATES,
                Some(&refl(30.0)),
                Some(&|_, _| Some(11.0)),
            ),
            // A SAILS Doppler repeat, newest of the three 0.48° members —
            // so "newest wins" and "surveillance wins" disagree here, and
            // the surveillance preference is what has to break the tie.
            make_sweep(
                6,
                0.53,
                360,
                SHORT_GATES,
                Some(&refl(35.0)),
                Some(&|_, _| Some(13.0)),
            ),
        ],
    )
}

/// The ladder a worker builds from a reconstructed payload is the ladder
/// the main thread built — **identically**, not approximately.
///
/// This is the property `render_input`'s version 6 exists for, and it used
/// to be impossible: the reconstruction carried an empty cut table and a
/// 0-based payload index where the elevation number belongs, so the sampler
/// refused the scan outright rather than silently keying it wrong. The
/// refusal was a placeholder for this test.
///
/// Compared over the sampler's own `Debug` line, which is the whole ladder
/// — product, rung count, each rung's geometric elevation *in cut order*,
/// and each rung's wrap-corrected nominal key. Comparing rung counts alone
/// would pass on a ladder that had kept the right number of rungs and
/// chosen the wrong sweep for every one of them, which is exactly the
/// silent failure the split cuts and the SAILS repeat above are here to
/// produce.
#[test]
fn a_reconstructed_render_input_scan_builds_the_identical_ladder() {
    let scan = sails_volume();
    for product in [RadarProduct::Reflectivity, RadarProduct::Velocity] {
        let original = VolumeSampler::new(&scan, product).expect("the fixture's own ladder builds");

        let input = crate::render_input::RenderInput::extract_volume(&scan, product, 35.33, -97.27)
            .expect("the fixture carries the moment");
        // Through the bytes, not just through `to_scan`: the cut angles and
        // the elevation numbers have to survive the wire, and a worker
        // holds bytes rather than a `RenderInput`.
        let decoded = crate::render_input::RenderInput::from_bytes(&input.to_bytes())
            .expect("the payload round-trips");
        let reconstructed = decoded.to_scan();

        let ported = VolumeSampler::new(&reconstructed, product)
            .expect("the reconstructed scan's ladder builds");

        assert_eq!(
            format!("{ported:?}"),
            format!("{original:?}"),
            "{product:?}: the worker's ladder is not the main thread's",
        );
        // precondition: the fixture is not so simple that any rule agrees.
        assert!(
            original.tilt_count() >= 3,
            "precondition: a {}-rung ladder is too short to distinguish \
                 the grouping rules this is about",
            original.tilt_count(),
        );
        assert!(
            original.nominal_elevations_deg().any(|k| k < 0.0),
            "precondition: the below-horizon cut left the fixture, so the \
                 wrap correction is no longer exercised across the port",
        );
    }
}

/// The two near-angle base tilts stay apart across the port — the thing no
/// angular threshold can do — and the SAILS repeat still fuses into its own
/// cut and still wins it on recency. Asserted on the *reconstructed* scan
/// rather than only on the original.
///
/// The fixture's cuts are declared 0.40° and 0.48° — 0.09° apart — while
/// its medians (0.44 and 0.53) sit inside every merge threshold the
/// campaign measured. A reconstruction that lost the cut table would have
/// to key by angle and would fuse the two into one rung, deleting a genuine
/// tilt; one that kept the table but wrote payload indices where the
/// elevation numbers go would key the sweeps 0..5 and read every one of
/// them off the wrong cut. Both produce a plausible monotone ladder and
/// neither errors.
///
/// The split cut's winner is the third thing, and the one the angles cannot
/// see: all three 0.48° members share a median, so which of them won is
/// legible only in the gate count.
#[test]
fn the_ported_ladder_still_separates_the_near_angle_cuts() {
    let scan = sails_volume();
    let input = crate::render_input::RenderInput::extract_volume(
        &scan,
        RadarProduct::Reflectivity,
        35.33,
        -97.27,
    )
    .expect("the fixture carries reflectivity");
    let reconstructed = input.to_scan();

    let medians: Vec<f64> = reconstructed
        .sweeps()
        .iter()
        .filter_map(|s| sweep_elevation_deg(s.radials()))
        .collect();
    let spread = medians
        .iter()
        .flat_map(|a| medians.iter().map(move |b| (a - b).abs()))
        .filter(|d| *d > 0.0)
        .fold(f64::INFINITY, f64::min);
    assert!(
        spread < 0.10,
        "precondition: the closest two medians are {spread:.3}° apart, \
             wider than the tightest threshold the campaign measured, so this \
             fixture no longer proves anything about angular merging",
    );

    let sampler = VolumeSampler::new(&reconstructed, RadarProduct::Reflectivity)
        .expect("the reconstructed ladder builds");
    let nominal: Vec<f64> = sampler.nominal_elevations_deg().collect();
    assert_eq!(
        nominal.len(),
        4,
        "the ported ladder fused or scattered cuts: {sampler:?}",
    );
    assert!(
        (nominal[1] - 0.40).abs() < 1e-9 && (nominal[2] - 0.48).abs() < 1e-9,
        "the two base tilts did not survive the port as declared: {nominal:?}",
    );
}

/// **The `Debug` line can tell two ladders apart when only the chosen
/// sweep differs.** Everything else here compares one ladder's string
/// against another's, and a comparison of two strings cannot pin what is
/// *in* them: drop a term from `describe` and both sides lose it together,
/// so every identity assertion in this module goes on passing while
/// becoming blind. That is precisely how the split-cut regression reached
/// review — the line printed only angles, and on a real split cut the
/// angles are identical whichever half won.
///
/// So this asserts the discriminating power directly: two volumes whose
/// ladders agree in every angle and differ only in which sweep took the
/// 0.48° rung must not describe themselves the same way.
#[test]
fn the_ladder_description_distinguishes_two_sweeps_of_one_cut() {
    let full = sails_volume();
    // The same volume with the surveillance half of the split cut removed,
    // so its Doppler half wins that rung instead. Nothing else moves.
    let without_surveillance = Scan::new(
        full.coverage_pattern().clone(),
        full.sweeps()
            .iter()
            .filter(|s| s.elevation_number() != 3)
            .cloned()
            .collect(),
    );

    let a = VolumeSampler::new(&full, RadarProduct::Reflectivity).expect("builds");
    let b = VolumeSampler::new(&without_surveillance, RadarProduct::Reflectivity).expect("builds");

    // precondition: the two ladders really are indistinguishable by angle,
    // which is what makes this test about the description rather than about
    // the ladders.
    assert_eq!(
        a.nominal_elevations_deg().collect::<Vec<_>>(),
        b.nominal_elevations_deg().collect::<Vec<_>>(),
    );
    assert_eq!(
        a.elevations_deg().collect::<Vec<_>>(),
        b.elevations_deg().collect::<Vec<_>>(),
        "the two ladders differ in a median, so the angles alone would \
             separate them and this says nothing about `describe`",
    );

    assert_ne!(
        format!("{a:?}"),
        format!("{b:?}"),
        "the ladder describes a 460 km surveillance rung and a 300 km \
             Doppler rung identically, so every `assert_eq!` on this string in \
             this module is blind to the difference that matters most",
    );

    // The other half of the same claim: a sweep with an **abandoned tail**
    // covers less azimuth at the same range, and that is equally invisible
    // in the angles. Same cut, same median, same gate count, fewer radials.
    let truncated = Scan::new(
        full.coverage_pattern().clone(),
        full.sweeps()
            .iter()
            .map(|s| {
                if s.elevation_number() == 3 {
                    Sweep::new(3, s.radials()[..300].to_vec())
                } else {
                    s.clone()
                }
            })
            .collect(),
    );
    let c = VolumeSampler::new(&truncated, RadarProduct::Reflectivity).expect("builds");
    assert_eq!(
        a.elevations_deg().collect::<Vec<_>>(),
        c.elevations_deg().collect::<Vec<_>>(),
        "truncating the tail moved a median, so this pair is separable by \
             angle and says nothing about the description",
    );
    assert_ne!(
        format!("{a:?}"),
        format!("{c:?}"),
        "the ladder describes a whole sweep and one missing a sixth of its \
             azimuths identically",
    );
}

/// **The surveillance half of a split cut still wins its rung after the
/// port** — which is a fact about *range*, and the one the angles cannot
/// express.
///
/// The rule is `sampler`'s, at the `carries(&i) && …velocity().is_none()`
/// in `build`: reflectivity belongs to the surveillance half, which reaches
/// 460 km against the Doppler half's 300. It discriminates on a field that
/// a **reflectivity** payload does not carry — `extract_volume` ships the
/// product's own moment and nothing else — so unless the payload says which
/// sweeps had velocity, every reconstructed sweep looks like a surveillance
/// half and `.rev().find(…)` takes the *newest* member instead: the Doppler
/// one.
///
/// Nothing about that fails. The section simply stops at ~300 km where the
/// main thread's own sampler reaches 460, and takes the low tilt's geometry
/// from the wrong antenna pass. On a real volume the two halves share a cut
/// angle *and* a median, so the ladder's angles are byte-identical either
/// way; only the gate count moves.
#[test]
fn the_ported_ladder_takes_the_surveillance_half_of_a_split_cut() {
    let scan = sails_volume();
    let input = crate::render_input::RenderInput::extract_volume(
        &scan,
        RadarProduct::Reflectivity,
        35.33,
        -97.27,
    )
    .expect("the fixture carries reflectivity");
    let reconstructed = input.to_scan();

    // precondition: the three members of the 0.48° cut are indistinguishable
    // by angle, so what is asserted below cannot be read off the medians.
    let split: Vec<f64> = reconstructed
        .sweeps()
        .iter()
        .filter(|s| matches!(s.elevation_number(), 3 | 4 | 6))
        .filter_map(|s| sweep_elevation_deg(s.radials()))
        .collect();
    assert_eq!(split.len(), 3, "the split cut lost a member in the port");
    assert!(
        split.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-9),
        "the split cut's members no longer share a median ({split:?}), so \
             this test could pass on the angles alone and would stop being \
             about the range",
    );

    for (label, scan) in [("original", &scan), ("reconstructed", &reconstructed)] {
        let sampler =
            VolumeSampler::new(scan, RadarProduct::Reflectivity).expect("the ladder builds");
        let rung = sampler
            .rungs
            .iter()
            .find(|r| (r.nominal_deg - 0.48).abs() < 1e-9)
            .expect("the 0.48 cut has a rung");
        let gates = rung.radials[0]
            .reflectivity()
            .map_or(0, |m| m.raw_values().len());
        assert_eq!(
            gates, LONG_GATES,
            "{label}: the 0.48° rung was won by the {gates}-gate Doppler \
                 half instead of the {LONG_GATES}-gate surveillance half — a \
                 section drawn from it stops short with no error and no NaN",
        );
    }
}

/// The refusal is still reachable, and still pinned against the **real**
/// `RenderInput` round trip: a volume joined mid-flight has no cut table
/// yet (`crate::chunks`' own placeholder), so there is nothing for the
/// payload to carry and the reconstruction rebuilds the same empty table.
///
/// Faithful includes faithfully unusable. The alternative — inventing cut
/// angles from the sweeps' own medians — would build a ladder in the worker
/// that the main thread would have refused to build, which is the silent
/// divergence this whole error exists to stop.
#[test]
fn a_payload_from_a_volume_with_no_cut_table_is_still_refused() {
    let scan = Scan::new(
        crate::render_input::placeholder_coverage_pattern(212),
        vec![
            flat_refl_sweep(1, 0.5, 360, 40, 20.0),
            flat_refl_sweep(2, 0.9, 360, 40, 30.0),
        ],
    );
    // precondition: the original is refused for exactly this reason, so
    // what is asserted below is that the port preserved it.
    assert!(matches!(
        VolumeSampler::new(&scan, RadarProduct::Reflectivity),
        Err(SamplerError::EmptyCoveragePattern { .. }),
    ));

    let input = crate::render_input::RenderInput::extract(
        &scan,
        0.5,
        RadarProduct::Reflectivity,
        35.33,
        -97.27,
        None,
        None,
    )
    .expect("the fixture carries reflectivity at 0.5°");
    // precondition: the reconstruction really did keep a renderable sweep,
    // so what fails below is the ladder and not the payload.
    assert!(
        crate::render::render_from(&input).is_some(),
        "precondition: the reconstructed input no longer renders, so this \
             test is measuring a broken fixture rather than the sampler",
    );

    let err = VolumeSampler::new(&input.to_scan(), RadarProduct::Reflectivity).expect_err(
        "the sampler accepted a scan rebuilt from a volume that had no cut \
             table — it has just built a ladder in the worker that the main \
             thread would have refused to build, silently",
    );
    assert!(
        matches!(err, SamplerError::EmptyCoveragePattern { vcp: 212 }),
        "expected the empty-cut-table refusal naming the real VCP, got {err:?}",
    );
    // The message has to say enough for whoever hits it to know why.
    let text = err.to_string();
    assert!(
        text.contains("elevation cuts") && text.contains("RenderInput"),
        "the refusal does not explain itself: {text}",
    );
}

/// The second half of the same guard: a cut table that exists but does not
/// cover a sweep's elevation number. Measured to happen on 0 of 203 real
/// volumes, so it means the sweep-to-VCP pairing has broken.
#[test]
fn an_elevation_number_outside_the_cut_table_is_refused() {
    for elevation_number in [0u8, 3, 255] {
        let scan = Scan::new(
            vcp(&[0.5, 0.9]),
            vec![flat_refl_sweep(elevation_number, 0.5, 360, 40, 20.0)],
        );
        let err = VolumeSampler::new(&scan, RadarProduct::Reflectivity)
            .expect_err("an elevation number outside the table indexed a two-cut VCP");
        assert!(
            matches!(
                err,
                SamplerError::ElevationNumberOutOfCutTable { cut_count: 2, .. }
            ),
            "expected the cut-table index refusal for elevation number \
                 {elevation_number}, got {err:?}",
        );
    }
    // And the in-range numbers still work, so the guard is a boundary
    // rather than a refusal of everything.
    for elevation_number in [1u8, 2] {
        let scan = Scan::new(
            vcp(&[0.5, 0.9]),
            vec![flat_refl_sweep(elevation_number, 0.5, 360, 40, 20.0)],
        );
        assert!(VolumeSampler::new(&scan, RadarProduct::Reflectivity).is_ok());
    }
}

/// A volume with no sweep carrying the moment is a refusal too, rather
/// than an empty ladder that answers `NoCoverage` at every point and looks
/// like a blank section.
#[test]
fn a_volume_with_no_sweep_carrying_the_moment_is_refused() {
    let scan = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 360, 40, 20.0)]);
    let err = VolumeSampler::new(&scan, RadarProduct::CorrelationCoefficient)
        .expect_err("the fixture carries reflectivity only");
    assert!(matches!(err, SamplerError::NoSweepsWithMoment { .. }));
}

// ── Geometry ────────────────────────────────────────────────────────────

/// A field that depends only on beam height reads back at the height it
/// was planted at.
///
/// The slab is 4–5 km; at 30 km ground range the fixture's half-degree
/// ladder puts rungs ~0.26 km apart, so the slab is four rungs thick and
/// its edges are resolvable.
#[test]
fn a_planted_horizontal_slab_reads_at_its_planted_height() {
    let angles: Vec<f64> = (1..=40).map(|i| f64::from(i) * 0.5).collect();
    let slab = |h: f64| if (4.0..5.0).contains(&h) { 50.0 } else { 20.0 };
    let sweeps: Vec<Sweep> = angles
        .iter()
        .enumerate()
        .map(|(i, &e)| {
            make_sweep(
                i as u8 + 1,
                e as f32,
                360,
                600,
                Some(&move |_, slant| Some(slab(beam::height_km(slant, e)))),
                None,
            )
        })
        .collect();
    let scan = Scan::new(vcp(&angles), sweeps);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    assert_eq!(sampler.tilt_count(), 40);

    let column = sampler.column(37.0, 30.0);
    let (lowest, highest) = column.height_span_km().unwrap();
    // precondition: the ladder brackets the slab at this ground range, so
    // every assertion below is an interpolation rather than a refusal.
    assert!(
        lowest < 3.0 && highest > 6.0,
        "precondition: at 30 km the ladder spans {lowest:.2}–{highest:.2} \
             km and does not bracket the 4–5 km slab",
    );

    let at = |h: f64| f64::from(column.at_height_km(h).value().unwrap());
    assert!(
        (at(4.5) - round_trip_refl(50.0)).abs() < 1e-6,
        "{}",
        at(4.5)
    );
    assert!(
        (at(2.0) - round_trip_refl(20.0)).abs() < 1e-6,
        "{}",
        at(2.0)
    );
    assert!(
        (at(6.5) - round_trip_refl(20.0)).abs() < 1e-6,
        "{}",
        at(6.5)
    );

    // The edges land where they were planted, to within the rung spacing
    // that resolves them — ~0.26 km at this ground range.
    let midpoint = 0.5 * (round_trip_refl(20.0) + round_trip_refl(50.0));
    let crossing = |from: f64, to: f64| {
        let steps = 4000;
        (0..=steps)
            .map(|i| from + (to - from) * f64::from(i) / f64::from(steps))
            .find(|&h| f64::from(column.at_height_km(h).value().unwrap()) > midpoint)
            .unwrap()
    };
    let bottom = crossing(3.0, 4.5);
    let top = crossing(6.0, 4.5);
    assert!(
        (bottom - 4.0).abs() < 0.3,
        "the slab's floor read at {bottom:.3} km, planted at 4.0",
    );
    assert!(
        (top - 5.0).abs() < 0.3,
        "the slab's ceiling read at {top:.3} km, planted at 5.0",
    );
}

/// The `cos e` test. A wall planted at a **ground** range reads at that
/// ground range on every tilt; without the correction the 10° tilt puts it
/// 1.5 km out.
#[test]
fn a_planted_vertical_wall_reads_at_its_planted_ground_range() {
    const WALL_KM: f64 = 100.0;
    let angles = [0.5f64, 4.0, 10.0];
    let sweeps: Vec<Sweep> = angles
        .iter()
        .enumerate()
        .map(|(i, &e)| {
            make_sweep(
                i as u8 + 1,
                e as f32,
                360,
                600,
                Some(&move |_, slant| {
                    let ground = beam::ground_range_km(slant, e);
                    Some(if (ground - WALL_KM).abs() <= 0.5 {
                        55.0
                    } else {
                        10.0
                    })
                }),
                None,
            )
        })
        .collect();
    let scan = Scan::new(vcp(&angles), sweeps);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

    let on_wall = sampler.column(120.0, WALL_KM);
    for (k, rung) in on_wall.rungs().iter().enumerate() {
        assert_eq!(
            f64::from(rung.sample.value().unwrap()),
            round_trip_refl(55.0),
            "rung {k} at {}° missed the wall at {WALL_KM} km ground",
            rung.elevation_deg,
        );
        // The rung's height is measured over the **ground** range too, not
        // along the slant range that shares the number.
        assert_eq!(
            rung.height_km,
            beam::height_at_ground_km(WALL_KM, rung.elevation_deg),
            "rung {k} at {}° took its height along the slant range",
            rung.elevation_deg,
        );
    }
    // precondition: the two height forms really do differ at the steep
    // tilt, so the assertion above discriminates. 286 m at 10° / 100 km.
    let height_gap =
        (beam::height_at_ground_km(WALL_KM, 10.0) - beam::height_km(WALL_KM, 10.0)).abs();
    assert!(
        (height_gap - 0.2862).abs() < 1e-3,
        "the 10° ground/slant height gap moved: {height_gap:.4} km, \
             documented as 0.2862",
    );

    // The discriminating half. A sampler that fed the ground range to the
    // gate index as if it were a slant range reads the 10° tilt's wall at
    // `100 · cos 10° = 98.48` km — 1.52 km, six gates, inward. That
    // position must be clear air.
    let uncorrected = beam::ground_range_km(WALL_KM, 10.0);
    let error_km = WALL_KM - uncorrected;
    assert!(
        (error_km - 1.5192).abs() < 1e-3,
        "the 10° cos e error moved: {error_km:.4} km, documented as 1.5192",
    );
    let off_wall = sampler.column(120.0, uncorrected);
    let steep = off_wall
        .rungs()
        .iter()
        .find(|r| r.elevation_deg > 9.0)
        .expect("a 10° rung");
    assert_eq!(
        f64::from(steep.sample.value().unwrap()),
        round_trip_refl(10.0),
        "the 10° rung found the wall at {uncorrected:.3} km ground, which \
             is where an uncorrected slant range would have put it",
    );
    // At 0.5° the same mistake is 0.004 km — a sixtieth of a gate — which
    // is why the low tilts cannot be the test.
    let shallow_error = WALL_KM - beam::ground_range_km(WALL_KM, 0.5);
    assert!(
        shallow_error < 0.01,
        "precondition: the 0.5° cos e error is {shallow_error:.4} km, so \
             the low tilts would have discriminated too and the steep tilt is \
             not doing the work",
    );

    // And the point query agrees with the column, at a height inside the
    // ladder.
    let h = on_wall.rungs()[1].height_km;
    assert_eq!(
        sampler.sample(120.0, WALL_KM, h),
        on_wall.at_height_km(h),
        "the point query and the column disagree",
    );
}

/// The divergence this module ships as a measurement rather than as a
/// comment: `render::render_gate` applies **no** `cos e` at all (it never
/// receives an elevation angle), so a section and the plan view will not
/// register above ~2°.
///
/// Both figures name their target, because `IMAGE_SIZE` is 2048 natively
/// and 1024 on wasm32 and the pixel counts halve with it.
#[test]
fn the_cos_e_correction_diverges_from_the_plan_view_by_a_measured_amount() {
    let cases = [(230.0f64, 2.4f64, 0.2017f64), (70.0, 19.5, 4.0151)];
    for (slant, elev, expected_km) in cases {
        let gap_km = slant - beam::ground_range_km(slant, elev);
        assert!(
            (gap_km - expected_km).abs() < 1e-3,
            "the {elev}° / {slant} km slant-to-ground gap moved: \
                 {gap_km:.4} km, documented as {expected_km}",
        );
    }

    let px = |slant: f64, elev: f64| {
        (slant - beam::ground_range_km(slant, elev)) * crate::types::PIXELS_PER_KM
    };
    // 2048 px over 460 km is 4.4522 px/km; wasm32 halves both.
    #[cfg(not(target_arch = "wasm32"))]
    let (expected_low, expected_high) = (0.898, 17.876);
    #[cfg(target_arch = "wasm32")]
    let (expected_low, expected_high) = (0.449, 8.938);
    assert_eq!(
        crate::types::IMAGE_SIZE,
        if cfg!(target_arch = "wasm32") {
            1024
        } else {
            2048
        },
        "IMAGE_SIZE moved, so the pixel figures below name the wrong target",
    );
    let low = px(230.0, 2.4);
    let high = px(70.0, 19.5);
    assert!(
        (low - expected_low).abs() < 0.01,
        "at 2.4° / 230 km the section sits {low:.3} px off the plan view \
             on a {}-pixel image, documented as {expected_low}",
        crate::types::IMAGE_SIZE,
    );
    assert!(
        (high - expected_high).abs() < 0.01,
        "at 19.5° / 70 km the section sits {high:.3} px off the plan view \
             on a {}-pixel image, documented as {expected_high}",
        crate::types::IMAGE_SIZE,
    );
    // precondition: the disagreement is invisible at the low tilts, which
    // is why "above ~2°" is the way it is stated.
    assert!(
        px(230.0, 0.5) < 0.2,
        "the 0.5° divergence is now {:.3} px, so the ~2° threshold in the \
             module doc is wrong",
        px(230.0, 0.5),
    );
}

// ── Interpolation ───────────────────────────────────────────────────────

/// Reflectivity averages in linear Z. 10 and 50 dBZ meet at **46.99**, not
/// at 30 — a 17 dB error, which is four palette bands.
///
/// Also the super-resolution half of the acceptance: 720 alternating
/// radials each return their own value. **This covers the low tilts
/// only** — azimuth resolution drops 720 → 360 partway up every real
/// ladder, which the fixture reproduces and the assertion below states.
#[test]
fn reflectivity_blends_in_linear_z_and_every_super_res_radial_survives() {
    let alternating = |az: f64, _slant: f64| {
        Some(if (az / 0.5).round() as i64 % 2 == 0 {
            10.0
        } else {
            50.0
        })
    };
    let scan = Scan::new(
        vcp(&[0.5, 4.0]),
        vec![
            make_sweep(1, 0.5, 720, 200, Some(&alternating), None),
            flat_refl_sweep(2, 4.0, 360, 200, 30.0),
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

    // Every one of the 720 radials returns its own planted value.
    let mut seen_low = 0usize;
    let mut seen_high = 0usize;
    for i in 0..720u32 {
        let az = f64::from(i as f32 * 0.5);
        let got = f64::from(sampler.column(az, 20.0).rungs()[0].sample.value().unwrap());
        let want = round_trip_refl(if i % 2 == 0 { 10.0 } else { 50.0 });
        assert!(
            (got - want).abs() < 1e-4,
            "radial {i} at {az}° read {got} dBZ, planted {want}",
        );
        if i % 2 == 0 {
            seen_low += 1;
        } else {
            seen_high += 1;
        }
    }
    assert_eq!((seen_low, seen_high), (360, 360));

    // Halfway between two radials: linear Z, not dB.
    let mid = f64::from(
        sampler.column(0.25, 20.0).rungs()[0]
            .sample
            .value()
            .unwrap(),
    );
    let linear_z = 10.0 * (0.5 * (10f64.powf(1.0) + 10f64.powf(5.0))).log10();
    assert!(
        (mid - linear_z).abs() < 0.01,
        "halfway between 10 and 50 dBZ read {mid:.3}, expected \
             {linear_z:.3} (the arithmetic mean, 30.0, is the wrong answer)",
    );
    assert!(
        (mid - 46.9897).abs() < 0.01,
        "the documented 46.99 moved: {mid:.4}",
    );
    assert!((mid - 30.0).abs() > 16.0);

    // The coverage caveat, asserted rather than left in prose: the upper
    // rung of this ladder has 360 radials, so an "all 720" test says
    // nothing about it.
    assert_eq!(sampler.column(0.25, 20.0).rungs().len(), 2);
    assert_eq!(scan.sweeps()[0].radials().len(), 720);
    assert_eq!(
        scan.sweeps()[1].radials().len(),
        360,
        "precondition: the fixture no longer drops to 360 radials on the \
             upper tilt, so this test's super-resolution claim covers more \
             than it should",
    );
}

/// The range axis interpolates between gate **centres**, so a point half a
/// gate along reads the mean of the two gates around it rather than the
/// nearer one repeated.
///
/// The azimuth test above cannot catch this — its field is constant along
/// range — and `round` in place of `floor` produces a *negative* far-corner
/// weight, which in linear Z is the logarithm of a negative number.
#[test]
fn gates_interpolate_between_their_centres_rather_than_snapping() {
    let alternating = |_az: f64, slant: f64| {
        let gate = ((slant - f64::from(FIRST_GATE_M) / 1000.0) / (f64::from(GATE_M) / 1000.0))
            .round() as i64;
        Some(if gate % 2 == 0 { 10.0 } else { 50.0 })
    };
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![make_sweep(1, 0.5, 360, 200, Some(&alternating), None)],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    // Sampling on a radial centre so azimuth contributes no blend of its
    // own; the ground range is the gate's slant range through `cos e`.
    let at = |slant: f64| {
        let ground = beam::ground_range_km(slant, 0.5);
        f64::from(
            sampler.column(30.0, ground).rungs()[0]
                .sample
                .value()
                .unwrap(),
        )
    };

    for gate in 40..50usize {
        let want = round_trip_refl(if gate % 2 == 0 { 10.0 } else { 50.0 });
        let got = at(gate_slant_km(gate));
        assert!(
            (got - want).abs() < 1e-4,
            "gate {gate}'s centre read {got} dBZ, planted {want}",
        );
    }

    // Half a gate along: the linear-Z mean of 10 and 50, the same 46.99 the
    // azimuth axis produces.
    let half = at(gate_slant_km(40) + f64::from(GATE_M) / 2000.0);
    assert!(
        (half - 46.9897).abs() < 0.01,
        "half a gate past gate 40 read {half:.4} dBZ, expected 46.9897",
    );
    // A quarter along leans towards the nearer gate but is still a blend,
    // which "snap to nearest" is not: 10log10(0.75·10 + 0.25·10⁵) = 43.98.
    let quarter = at(gate_slant_km(40) + f64::from(GATE_M) / 4000.0);
    assert!(
        (quarter - 43.9800).abs() < 0.01,
        "a quarter gate past gate 40 read {quarter:.4} dBZ, expected \
             43.9800",
    );
}

/// Everything that is not reflectivity averages arithmetically, and
/// differential phase averages on the circle so the 360°→0° fold does not
/// become a half turn.
#[test]
fn velocity_averages_arithmetically_and_phase_averages_on_the_circle() {
    // **±9 rather than ±20, and gate 0 held at 25.** A ±20 checkerboard in
    // a sweep whose fastest gate is 20 m/s is, to anything reading only
    // the data, a textbook Nyquist fold: adjacent radials spanning the
    // whole observed range and changing sign. `straddles_fold` fires on it
    // and is right to — no atmosphere produces that field — so the fixture
    // is given an amplitude a real sweep can hold instead.
    //
    // The 25 m/s at gate 0 is what keeps this test honest: it arms the
    // fold guard at 25 m/s, so the arithmetic mean asserted below is the
    // guard *declining* on an 18 m/s step rather than the guard being
    // switched off by `FOLD_LIMIT_FLOOR_MS`. The seam itself is pinned by
    // `a_velocity_pair_across_the_nyquist_seam_takes_the_nearer_gate`.
    let alternating = |az: f64, slant: f64| {
        Some(if slant < gate_slant_km(1) {
            25.0
        } else if az.round() as i64 % 2 == 0 {
            -9.0
        } else {
            9.0
        })
    };
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![make_sweep(1, 0.5, 360, 200, None, Some(&alternating))],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    let mid = f64::from(sampler.column(0.5, 20.0).rungs()[0].sample.value().unwrap());
    assert_eq!(
        sampler.rungs[0].fold_limit_ms,
        Some(25.0),
        "precondition: the fold guard is switched off for this sweep, so \
             the mean below would pass even if the guard were over-eager",
    );
    let arithmetic = 0.5 * (round_trip_vel(-9.0) + round_trip_vel(9.0));
    assert!(
        (mid - arithmetic).abs() < 1e-4,
        "velocity halfway between {} and {} read {mid}, expected \
             {arithmetic}",
        round_trip_vel(-9.0),
        round_trip_vel(9.0),
    );

    // Differential phase: 359° and 1° meet at 0°, not at 180°. Encoded
    // 16-bit at 1/100°, which keeps both ends clear of the 0/1 status
    // codes.
    let radials: Vec<Radial> = (0..360)
        .map(|i| {
            let v = if i % 2 == 0 { 359.0f64 } else { 1.0 };
            let raw = (v * 100.0).round() as u16;
            let bytes: Vec<u8> = (0..40).flat_map(|_| raw.to_be_bytes()).collect();
            Radial::new(
                0,
                i,
                f32::from(i),
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                None,
                None,
                None,
                None,
                Some(MomentData::from_fixed_point(
                    40,
                    FIRST_GATE_M,
                    GATE_M,
                    16,
                    100.0,
                    0.0,
                    bytes,
                )),
                None,
                None,
            )
        })
        .collect();
    let scan = Scan::new(vcp(&[0.5]), vec![Sweep::new(1, radials)]);
    let sampler = VolumeSampler::new(&scan, RadarProduct::DifferentialPhase).unwrap();
    // 8 km ground: gate 24 of 40, comfortably inside this moment's span.
    let seam = f64::from(sampler.column(0.5, 8.0).rungs()[0].sample.value().unwrap());
    let off_zero = seam.min(360.0 - seam).abs();
    assert!(
        off_zero < 0.01,
        "359° and 1° averaged to {seam}°, which is {off_zero}° off the 0° \
             they straddle — a linear lerp would say 180°",
    );
    assert!(
        (seam - 180.0).abs() > 100.0,
        "differential phase is being lerped across the 360° fold",
    );
}

/// The gap `MomentValue::RangeFolded` never crossed: five different
/// reasons for having no number, all distinguishable, none of them `NaN`
/// alone.
#[test]
fn a_range_folded_gate_is_distinguishable_from_a_missing_one() {
    // Gate 0 below threshold, gate 1 range folded, gates 2.. ordinary.
    let mut bytes = vec![0u8, 1];
    bytes.extend((2..40).map(|_| encode_vel(15.0)));
    let radials: Vec<Radial> = (0..360)
        .map(|i| {
            Radial::new(
                0,
                i,
                f32::from(i),
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                None,
                // Radial 200 carries no velocity at all.
                (i != 200).then(|| moment_from(bytes.clone(), VEL_SCALE, VEL_OFFSET)),
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    let scan = Scan::new(vcp(&[0.5]), vec![Sweep::new(1, radials)]);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();

    // Gate centres at a radial centre, so the heaviest corner is
    // unambiguous. Ground range and slant range agree to 4 cm at 0.5°.
    let status_at = |az: f64, ground: f64| sampler.column(az, ground).rungs()[0].sample.status();
    let statuses = [
        status_at(10.0, gate_slant_km(0)),
        status_at(10.0, gate_slant_km(1)),
        status_at(10.0, gate_slant_km(5)),
        status_at(10.0, gate_slant_km(200)),
        status_at(0.5, 1.0),
    ];
    assert_eq!(statuses[0], SampleStatus::BelowThreshold);
    assert_eq!(statuses[1], SampleStatus::RangeFolded);
    assert_eq!(statuses[2], SampleStatus::Value);
    assert_eq!(statuses[3], SampleStatus::BeyondRange);
    assert_eq!(
        statuses[4],
        SampleStatus::NoCoverage,
        "inside the first gate"
    );
    assert_eq!(
        status_at(200.0, gate_slant_km(5)),
        SampleStatus::NoCoverage,
        "a radial with no moment",
    );

    // All five are distinct, which is the property that makes a hover
    // readout worth writing.
    for (i, a) in statuses.iter().enumerate() {
        for b in &statuses[i + 1..] {
            assert_ne!(a, b, "two conditions collapsed to {a:?}");
        }
    }
    // And a value is a value: the range-folded gate has none.
    assert!(
        sampler.column(10.0, gate_slant_km(1)).rungs()[0]
            .sample
            .value()
            .is_none()
    );
    assert!(
        sampler.column(10.0, gate_slant_km(5)).rungs()[0]
            .sample
            .value()
            .is_some()
    );
}

/// The duplication guard on `gate_sample`, against the model's own
/// decoder, element for element.
///
/// **Includes a `scale == 0.0` moment**, because that case disables the
/// 0/1 status codes entirely and is the one a reimplementation gets wrong.
///
/// A 16-bit moment with an *odd* byte count would exercise `gate_sample`'s
/// `get(k..k + 2)` against the model's `chunks_exact`, and it is not
/// tested here because it cannot be built: `MomentDataBlock::from_fixed_point`
/// carries a `debug_assert!` refusing it, so the fixture would pass in
/// release and panic in debug. The bounds are covered by the last-gate
/// assertions below instead.
#[test]
fn raw_gate_decoding_matches_the_model_element_for_element() {
    let eight_bit: Vec<u8> = (0..=255u8).collect();
    let sixteen_bit: Vec<u8> = (0..600u16).flat_map(u16::to_be_bytes).collect();
    // A declared gate count that overruns the bytes, which is what makes
    // `raw_values().len()` rather than `gate_count()` authoritative.
    let short_bytes: Vec<u8> = (0..50u8).collect();

    let cases: Vec<(&str, MomentData)> = vec![
        (
            "8-bit, scaled",
            MomentData::from_fixed_point(256, FIRST_GATE_M, GATE_M, 8, 2.0, 66.0, eight_bit),
        ),
        (
            "8-bit, scale 0 (status codes disabled)",
            MomentData::from_fixed_point(
                256,
                FIRST_GATE_M,
                GATE_M,
                8,
                0.0,
                0.0,
                (0..=255u8).collect(),
            ),
        ),
        (
            "16-bit, scaled",
            MomentData::from_fixed_point(600, FIRST_GATE_M, GATE_M, 16, 100.0, 0.0, sixteen_bit),
        ),
        (
            "8-bit, gate_count overruns the bytes",
            MomentData::from_fixed_point(400, FIRST_GATE_M, GATE_M, 8, 2.0, 66.0, short_bytes),
        ),
    ];

    let mut checked = 0usize;
    let mut saw_below = false;
    let mut saw_folded = false;
    let mut saw_unscaled_zero = false;
    for (label, moment) in &cases {
        let model = moment.values();
        for gate in 0..model.len() + 3 {
            let ours = gate_sample(moment, gate);
            match model.get(gate) {
                None => assert_eq!(
                    ours.status(),
                    SampleStatus::BeyondRange,
                    "{label}: gate {gate} is past the model's {} values \
                         but decoded as {ours:?}",
                    model.len(),
                ),
                Some(nexrad_model::data::MomentValue::Value(v)) => {
                    assert_eq!(ours.status(), SampleStatus::Value, "{label} gate {gate}");
                    assert_eq!(
                        ours.value().unwrap().to_bits(),
                        v.to_bits(),
                        "{label}: gate {gate} decoded to {} where the \
                             model says {v}",
                        ours.value().unwrap(),
                    );
                    if *v == 0.0 && moment.scale() == 0.0 {
                        saw_unscaled_zero = true;
                    }
                }
                Some(nexrad_model::data::MomentValue::BelowThreshold) => {
                    assert_eq!(
                        ours.status(),
                        SampleStatus::BelowThreshold,
                        "{label} gate {gate}",
                    );
                    saw_below = true;
                }
                Some(nexrad_model::data::MomentValue::RangeFolded) => {
                    assert_eq!(
                        ours.status(),
                        SampleStatus::RangeFolded,
                        "{label} gate {gate}",
                    );
                    saw_folded = true;
                }
            }
            checked += 1;
        }
    }
    // preconditions: the sweep actually reached each of the three decode
    // paths, so an implementation that got one of them wrong could not
    // have passed by never being asked.
    // 256 + 256 + 600 + 50 gates, plus three past the end of each.
    assert_eq!(checked, 1174, "the comparison grid changed size");
    assert!(saw_below, "no below-threshold gate was exercised");
    assert!(saw_folded, "no range-folded gate was exercised");
    assert!(
        saw_unscaled_zero,
        "the scale == 0.0 moment never returned raw 0 as a value, which \
             is the case this test exists for",
    );
    // `raw_values().len()` and not `gate_count()` decides where the gates
    // stop: this moment declares 400 and has 50.
    let short = &cases[3].1;
    assert_eq!(short.gate_count(), 400);
    assert_eq!(short.raw_values().len(), 50);
    assert_eq!(short.values().len(), 50, "the model trusts the bytes");
    assert_eq!(gate_sample(short, 49).status(), SampleStatus::Value);
    assert_eq!(
        gate_sample(short, 50).status(),
        SampleStatus::BeyondRange,
        "the declared gate count was trusted over the bytes",
    );

    // And the 16-bit moment's own last gate, so the two-byte stride's
    // bound is pinned too.
    let wide = &cases[2].1;
    assert_eq!(gate_sample(wide, 599).status(), SampleStatus::Value);
    assert_eq!(gate_sample(wide, 600).status(), SampleStatus::BeyondRange);
}

// ── The edges of the volume ─────────────────────────────────────────────

/// Nothing is filled in outside the ladder, in either direction, and the
/// cone of silence reports itself.
#[test]
fn nothing_is_extrapolated_above_or_below_the_ladder() {
    let angles = [0.5f64, 4.0, 10.0];
    let sweeps: Vec<Sweep> = angles
        .iter()
        .enumerate()
        .map(|(i, &e)| flat_refl_sweep(i as u8 + 1, e as f32, 360, 600, 35.0))
        .collect();
    let scan = Scan::new(vcp(&angles), sweeps);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

    let column = sampler.column(90.0, 50.0);
    let (low, high) = column.height_span_km().unwrap();
    assert_eq!(
        column.at_height_km(low - 0.001).status(),
        SampleStatus::BelowLowestBeam,
    );
    assert_eq!(
        column.at_height_km(high + 0.001).status(),
        SampleStatus::AboveVolume,
    );
    // The boundaries themselves are inside.
    assert_eq!(column.at_height_km(low).status(), SampleStatus::Value);
    assert_eq!(column.at_height_km(high).status(), SampleStatus::Value);
    // Ground level under a 0.5° beam at 50 km is 0.6 km down and is not
    // invented.
    assert_eq!(
        column.at_height_km(0.0).status(),
        SampleStatus::BelowLowestBeam,
    );

    // The cone of silence: over the site every beam centre is at zero
    // height, so anything above the antenna is above the volume.
    let overhead = sampler.column(90.0, 0.0);
    assert_eq!(
        overhead.height_span_km(),
        Some((0.0, 0.0)),
        "the beams do not all meet at the antenna",
    );
    assert_eq!(
        overhead.at_height_km(3.0).status(),
        SampleStatus::AboveVolume,
        "the cone of silence was filled in rather than reported",
    );

    // An empty column answers the same way everywhere.
    let empty = Column::new();
    assert_eq!(empty.at_height_km(3.0).status(), SampleStatus::NoCoverage);
    assert_eq!(empty.height_span_km(), None);
    assert_eq!(
        column.at_height_km(f64::NAN).status(),
        SampleStatus::NoCoverage,
    );
}

/// The ordinary case the plan calls out: **every** volume has a bracketing
/// rung with no data at 230 km and 300 km, because the upper cuts stop
/// short. It is beam geometry, not a ladder defect, and it must surface as
/// a status rather than be filled from the rung below.
#[test]
fn a_bracketing_rung_that_stops_short_reports_rather_than_being_filled() {
    // The 0.5° surveillance cut reaches 460 km; the 4.0° cut stops at
    // 150 km, which 8 of 19 measured volumes do.
    let scan = Scan::new(
        vcp(&[0.5, 4.0]),
        vec![
            flat_refl_sweep(1, 0.5, 360, 1832, 35.0),
            flat_refl_sweep(2, 4.0, 360, 592, 45.0),
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

    let short_of = sampler.column(45.0, 200.0);
    assert_eq!(short_of.rungs().len(), 2, "the rung was dropped, not kept");
    assert_eq!(short_of.rungs()[0].sample.status(), SampleStatus::Value);
    assert_eq!(
        short_of.rungs()[1].sample.status(),
        SampleStatus::BeyondRange,
        "the 4° cut stops at 150 km and this column is at 200",
    );

    let (low, high) = short_of.height_span_km().unwrap();
    // Under halfway the surveillance rung carries it; over halfway the
    // absent rung does, and nothing is invented in between.
    let just_above = low + 0.1 * (high - low);
    let just_below = low + 0.9 * (high - low);
    assert_eq!(
        f64::from(short_of.at_height_km(just_above).value().unwrap()),
        round_trip_refl(35.0),
    );
    assert_eq!(
        short_of.at_height_km(just_below).status(),
        SampleStatus::BeyondRange,
        "the missing rung's half of the bracket was filled from the rung \
             below",
    );

    // precondition: the same column inside 150 km has both rungs, so the
    // status above is about range and not about the fixture.
    let inside = sampler.column(45.0, 100.0);
    assert!(inside.rungs().iter().all(|r| r.sample.value().is_some()));
}

/// An abandoned tail leaves a hole in azimuth. Painting the nearest
/// surviving radial across it would draw data where the radar never
/// looked; the plan view leaves the same hole, and so does this.
#[test]
fn an_azimuth_hole_is_reported_rather_than_painted_across() {
    let full_gate = |_: usize| encode_refl(35.0);
    let radial_at = |i: u16, az: f32, spacing: f32| {
        Radial::new(
            0,
            i,
            az,
            spacing,
            RadialStatus::IntermediateRadialData,
            1,
            0.5,
            Some(moment_from(
                (0..40).map(full_gate).collect(),
                REFL_SCALE,
                REFL_OFFSET,
            )),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    };

    // Radials 0.0 … 199.5 at half-degree spacing: a 160° hole.
    let radials: Vec<Radial> = (0..400)
        .map(|i| radial_at(i, f32::from(i) * 0.5, 0.5))
        .collect();
    let scan = Scan::new(vcp(&[0.5]), vec![Sweep::new(1, radials)]);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

    let status_at = |az: f64| sampler.column(az, 8.0).rungs()[0].sample.status();
    assert_eq!(status_at(100.0), SampleStatus::Value, "inside the sweep");
    assert_eq!(status_at(199.5), SampleStatus::Value, "the last radial");
    assert_eq!(
        status_at(199.7),
        SampleStatus::Value,
        "inside the last radial's own quarter-degree footprint",
    );
    assert_eq!(
        status_at(200.5),
        SampleStatus::NoCoverage,
        "past the last radial's footprint, in the hole",
    );
    assert_eq!(status_at(280.0), SampleStatus::NoCoverage, "mid hole");
    assert_eq!(
        status_at(359.5),
        SampleStatus::NoCoverage,
        "half a degree short of the first radial, still in the hole",
    );
    // The footprint reaches backwards across 0° as well as forwards, which
    // is the wrap case: 359.9° is 0.1° from the 0.0° radial's centre.
    assert_eq!(
        status_at(359.9),
        SampleStatus::Value,
        "inside the first radial's footprint, reached across the 0° seam",
    );
    assert_eq!(
        status_at(0.1),
        SampleStatus::Value,
        "the first radial's footprint",
    );

    // One dropped radial leaves a gap of the same shape, one step wide.
    let mut radials: Vec<Radial> = (0..720)
        .map(|i| radial_at(i, f32::from(i) * 0.5, 0.5))
        .collect();
    radials.remove(180); // 90.0°
    let scan = Scan::new(vcp(&[0.5]), vec![Sweep::new(1, radials)]);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    let status_at = |az: f64| sampler.column(az, 8.0).rungs()[0].sample.status();
    assert_eq!(status_at(89.5), SampleStatus::Value);
    assert_eq!(
        status_at(90.0),
        SampleStatus::NoCoverage,
        "the dropped radial",
    );
    assert_eq!(status_at(90.5), SampleStatus::Value);
    assert_eq!(
        status_at(89.7),
        SampleStatus::Value,
        "inside the surviving 89.5° radial's footprint",
    );

    // A full sweep interpolates across every seam, including 359.5 → 0.
    let full = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 720, 40, 35.0)]);
    let full = VolumeSampler::new(&full, RadarProduct::Reflectivity).unwrap();
    for az in [0.0, 0.25, 90.0, 180.3, 359.75, 359.99] {
        assert_eq!(
            full.column(az, 8.0).rungs()[0].sample.status(),
            SampleStatus::Value,
            "a complete sweep reported no coverage at {az}°",
        );
    }
}

/// A sweep arrives in **collection** order, starting wherever the antenna
/// was and wrapping through 0°, and its lowest azimuth is not 0.
///
/// Three things fail on such a sweep and on no other: an index that trusts
/// the radial order, a bracket that handles only the top of the wrap, and
/// a query below the sweep's lowest azimuth (which is the *lower* wrap
/// case, and reaches the last radial through 360°).
#[test]
fn a_sweep_that_starts_off_north_is_indexed_and_wraps_at_both_ends() {
    // 250.5°, 251.5° … 359.5°, 0.5° … 249.5°, in that order.
    let azimuths: Vec<f32> = (0..360).map(|i| ((250 + i) % 360) as f32 + 0.5).collect();
    // precondition: this really is out of order and really does miss 0°.
    assert!(
        azimuths.windows(2).any(|w| w[1] < w[0]),
        "precondition: the fixture's azimuths are already ascending",
    );
    assert!(azimuths.iter().all(|&a| a > 0.4));

    // Two hot radials: one in the middle of the sweep, one at the seam.
    let hot = |az: f64| {
        if (az - 100.5).abs() < 0.01 || (az - 359.5).abs() < 0.01 {
            55.0
        } else {
            10.0
        }
    };
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![refl_sweep_at(1, 0.5, &azimuths, 200, hot)],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    let at = |az: f64| {
        f64::from(
            sampler.column(az, 20.0).rungs()[0]
                .sample
                .value()
                .expect("a complete sweep covers every azimuth"),
        )
    };

    assert_eq!(at(100.5), round_trip_refl(55.0), "the hot radial");
    assert_eq!(at(103.5), round_trip_refl(10.0), "three radials away");
    assert_eq!(
        at(250.5),
        round_trip_refl(10.0),
        "the first radial collected"
    );

    // Below the sweep's lowest azimuth: the bracket is the *last* radial
    // (359.5°, hot) and the first (0.5°, cold), reached across 360°.
    // 0.2° sits 0.7 of the way from 359.5 to 0.5, so linear Z gives
    // 10log10(0.3·10^5.5 + 0.7·10) = 49.77 dBZ.
    let below = at(0.2);
    assert!(
        (below - 49.7715).abs() < 0.01,
        "0.2° read {below:.4} dBZ; expected 49.7715, the linear-Z blend of \
             the 359.5° and 0.5° radials across the seam",
    );
    // And just the other side of the seam, 0.4 of the way instead of 0.7.
    let above = at(359.9);
    assert!(
        (above - 52.7816).abs() < 0.01,
        "359.9° read {above:.4} dBZ; expected 52.7816",
    );
}

/// A real sweep's azimuths jitter a few hundredths of a degree, so the
/// adjacency threshold cannot be one step exactly.
///
/// This is the lower bracket on [`MAX_ADJACENT_GAP_STEPS`]; the dropped
/// radial in `an_azimuth_hole_is_reported_rather_than_painted_across` is
/// the upper one, because that gap is two steps and must *not* be bridged.
#[test]
fn azimuth_jitter_does_not_open_a_hole() {
    // ±0.04°, deterministic, well inside half a step so the order holds.
    let jitter = |i: usize| ((i * 7) % 17) as f32 * 0.005 - 0.04;
    let azimuths: Vec<f32> = (0..720).map(|i| i as f32 * 0.5 + jitter(i)).collect();

    // precondition: the jitter really does push a gap past one step, so a
    // 1.0-step threshold would open a hole here.
    let gap = |i: usize| {
        let a = f64::from(azimuths[i]);
        let b = f64::from(azimuths[(i + 1) % 720]);
        (b - a).rem_euclid(360.0)
    };
    let widest_at = (0..720).max_by(|&a, &b| gap(a).total_cmp(&gap(b))).unwrap();
    let widest = gap(widest_at);
    assert!(
        (0.5..0.75).contains(&widest),
        "precondition: the widest jittered gap is {widest:.4}°, which does \
             not sit between one and 1.5 median steps",
    );

    let scan = Scan::new(
        vcp(&[0.5]),
        vec![refl_sweep_at(1, 0.5, &azimuths, 200, |_| 35.0)],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

    // The middle of the widest gap, named rather than swept for: a
    // 1.0-step threshold refuses only the sliver of that one gap outside
    // the two radials' footprints, which a coarse sweep steps over.
    let mid = (f64::from(azimuths[widest_at]) + widest / 2.0).rem_euclid(360.0);
    assert_eq!(
        sampler.column(mid, 20.0).rungs()[0].sample.status(),
        SampleStatus::Value,
        "the middle of the widest jittered gap ({widest:.4}° at {mid}°) \
             read as a hole",
    );

    for step in 0..3600 {
        let az = f64::from(step) / 10.0;
        assert_eq!(
            sampler.column(az, 20.0).rungs()[0].sample.status(),
            SampleStatus::Value,
            "a jittered but complete sweep reported no coverage at {az}°",
        );
    }
}

/// A badly truncated sweep must not widen its own radials' footprints.
///
/// The azimuth step is the **median** gap and not the mean for exactly
/// this volume: 100 radials covering 50° have a mean gap of 3.6° and a
/// median of 0.5°. On the mean, each surviving radial would claim 1.8° of
/// ground either side and paint 3.6° of fabricated data around the edge of
/// the hole.
#[test]
fn a_badly_truncated_sweep_keeps_its_radials_half_step_footprint() {
    let azimuths: Vec<f32> = (0..100).map(|i| i as f32 * 0.5).collect();
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![refl_sweep_at(1, 0.5, &azimuths, 200, |_| 35.0)],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    let status_at = |az: f64| sampler.column(az, 20.0).rungs()[0].sample.status();

    assert_eq!(status_at(25.0), SampleStatus::Value, "inside the sweep");
    assert_eq!(
        status_at(49.7),
        SampleStatus::Value,
        "0.2° past the last radial, inside its quarter-degree footprint",
    );
    assert_eq!(
        status_at(49.8),
        SampleStatus::NoCoverage,
        "0.3° past the last radial: past a half-step footprint, and the \
             sweep's *mean* step would have claimed it",
    );
    assert_eq!(status_at(51.0), SampleStatus::NoCoverage);
    assert_eq!(status_at(180.0), SampleStatus::NoCoverage);
    assert_eq!(
        status_at(359.7),
        SampleStatus::NoCoverage,
        "0.3° short of the first radial, on the other side of the hole",
    );
    assert_eq!(
        status_at(359.9),
        SampleStatus::Value,
        "0.1° short of the first radial, inside the footprint it reaches \
             back across 0° with",
    );
}

/// A ladder whose chosen sweeps' medians invert its cut order still
/// brackets by height.
///
/// Measured never to happen — medians did not invert the VCP's cut order
/// in 4 756 ordered pairs — which is why the column sorts rather than
/// assumes. Without the sort `partition_point` is asking an unsorted
/// sequence a sorted question, and the answer is `BelowLowestBeam`
/// everywhere: silent, total, and shaped exactly like a volume with no
/// data in it.
#[test]
fn a_ladder_whose_medians_invert_still_brackets_by_height() {
    let scan = Scan::new(
        vcp(&[0.5, 0.9]),
        vec![
            // The 0.5° cut ran high and the 0.9° cut ran low.
            flat_refl_sweep(1, 1.05, 360, 200, 20.0),
            flat_refl_sweep(2, 0.55, 360, 200, 40.0),
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    // The ladder is in cut order, which is what the rule says — so the
    // geometric elevations come out descending, and `elevations_deg` says
    // "in cut order" rather than "ascending" for exactly this reason.
    assert_eq!(
        sampler.elevations_deg().collect::<Vec<_>>(),
        vec![f64::from(1.05f32), f64::from(0.55f32)],
    );
    // And the gap is still a gap. Folding signed steps down the cut order
    // would give `0.0` here — the number that exists to warn "this section
    // is interpolating across nothing" reading *no gap* in one of the few
    // cases it is there for.
    let gap = sampler.widest_tilt_gap_deg();
    assert!(
        (gap - (f64::from(1.05f32) - f64::from(0.55f32))).abs() < 1e-9,
        "an inverted ladder reports a widest gap of {gap}°, not the 0.5° \
             between its two rungs",
    );
    assert!(gap > 0.0);

    // 30 km: inside these sweeps' 51.9 km of gates on both rungs.
    let column = sampler.column(45.0, 30.0);
    let heights: Vec<f64> = column.rungs().iter().map(|r| r.height_km).collect();
    assert!(
        heights.windows(2).all(|w| w[0] < w[1]),
        "the column is not ascending by height: {heights:?}",
    );
    // The low rung is the 0.55° one, so it carries the 40 dBZ.
    assert_eq!(
        f64::from(column.rungs()[0].sample.value().unwrap()),
        round_trip_refl(40.0),
    );
    let (low, high) = column.height_span_km().unwrap();
    assert!(low < high);
    let mid = 0.5 * (low + high);
    assert_eq!(
        column.at_height_km(mid).status(),
        SampleStatus::Value,
        "a height between the two rungs was not bracketed",
    );
    assert_eq!(
        column.at_height_km(low - 0.01).status(),
        SampleStatus::BelowLowestBeam,
    );
    assert_eq!(
        column.at_height_km(high + 0.01).status(),
        SampleStatus::AboveVolume,
    );
}

// ── The product gate and the wire ───────────────────────────────────────

/// Only the six native moments. The hybrid classification is not a moment
/// and the integrals have no vertical axis left to cut.
#[test]
fn samplable_admits_the_six_native_moments_and_nothing_else() {
    let native = [
        (RadarProduct::Reflectivity, MomentSlot::Reflectivity),
        (RadarProduct::Velocity, MomentSlot::Velocity),
        (RadarProduct::SpectrumWidth, MomentSlot::SpectrumWidth),
        (
            RadarProduct::DifferentialReflectivity,
            MomentSlot::DifferentialReflectivity,
        ),
        (
            RadarProduct::DifferentialPhase,
            MomentSlot::DifferentialPhase,
        ),
        (
            RadarProduct::CorrelationCoefficient,
            MomentSlot::CorrelationCoefficient,
        ),
    ];
    for (product, slot) in native {
        assert_eq!(samplable(product), Some(slot), "{product:?}");
    }

    let refused = [
        RadarProduct::HydrometeorClassification,
        RadarProduct::EchoTops,
        RadarProduct::EchoTopsInterpolated,
        RadarProduct::VerticallyIntegratedLiquid,
        RadarProduct::VilDensity,
        RadarProduct::ProbabilityOfSevereHail,
        RadarProduct::MaxExpectedHailSize,
        RadarProduct::NormalizedRotation,
        RadarProduct::StormRelativeVelocity,
        RadarProduct::SpecificDifferentialPhase,
        RadarProduct::PrecipitationRate,
    ];
    let scan = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 360, 40, 20.0)]);
    for product in refused {
        assert_eq!(samplable(product), None, "{product:?} was admitted");
        let err =
            VolumeSampler::new(&scan, product).expect_err("a refused product built a sampler");
        let text = err.to_string();
        assert!(
            matches!(err, SamplerError::NotSamplable { .. }) && text.len() > 40,
            "{product:?} was refused without a reason: {text}",
        );
    }
    // precondition: every variant is covered, so a new product cannot be
    // added without a decision about it.
    assert_eq!(
        native.len() + refused.len(),
        17,
        "RadarProduct has gained or lost a variant; decide whether it is \
             samplable rather than letting it fall through",
    );

    // The HHC refusal in particular names what it is, because "not a
    // moment" is the part that surprises people.
    let hhc = VolumeSampler::new(&scan, RadarProduct::HydrometeorClassification)
        .unwrap_err()
        .to_string();
    assert!(hhc.contains("hybrid-scan"), "{hhc}");
}

/// The wire codes are stable, total and injective — a section crossing a
/// message port keeps its statuses instead of arriving as a field of
/// `NaN`.
#[test]
fn every_sample_status_survives_the_wire() {
    let all = [
        SampleStatus::Value,
        SampleStatus::BelowThreshold,
        SampleStatus::RangeFolded,
        SampleStatus::BelowLowestBeam,
        SampleStatus::AboveVolume,
        SampleStatus::BeyondRange,
        SampleStatus::NoCoverage,
    ];
    for (i, s) in all.iter().enumerate() {
        assert_eq!(s.wire_code(), i as u8, "{s:?} moved on the wire");
        assert_eq!(SampleStatus::from_wire_code(s.wire_code()), Some(*s));
    }
    assert_eq!(SampleStatus::from_wire_code(7), None);
    assert_eq!(SampleStatus::from_wire_code(255), None);
    // precondition: the list above is the whole enum, so a new variant
    // fails here rather than travelling as an unknown byte.
    assert_eq!(
        all.len(),
        7,
        "SampleStatus gained a variant without a wire code",
    );
}

/// A `Sample` cannot carry a number it does not have, or hide one it does.
#[test]
fn a_sample_pairs_its_number_with_its_reason() {
    let found = Sample::found(35.5);
    assert_eq!(found.status(), SampleStatus::Value);
    assert_eq!(found.value(), Some(35.5));
    assert_eq!(found.value_or_nan(), 35.5);

    let missing = Sample::missing(SampleStatus::RangeFolded);
    assert_eq!(missing.status(), SampleStatus::RangeFolded);
    assert_eq!(missing.value(), None);
    assert!(missing.value_or_nan().is_nan());
}

/// `sample` is `column().at_height_km()`, over a grid that crosses every
/// boundary the two share.
#[test]
fn the_point_query_is_exactly_the_column_query() {
    let angles = [0.5f64, 1.5, 4.0, 10.0];
    let sweeps: Vec<Sweep> = angles
        .iter()
        .enumerate()
        .map(|(i, &e)| {
            make_sweep(
                i as u8 + 1,
                e as f32,
                360,
                400,
                Some(&move |az, slant| (slant < 80.0).then_some(20.0 + 0.1 * az + e * 2.0)),
                None,
            )
        })
        .collect();
    let scan = Scan::new(vcp(&angles), sweeps);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();

    let mut checked = 0usize;
    let mut statuses = std::collections::HashSet::new();
    for az in [0.0, 37.5, 180.0, 359.9] {
        for ground in [0.0, 5.0, 40.0, 90.0, 250.0] {
            let column = sampler.column(az, ground);
            for h in [-1.0, 0.0, 0.5, 2.0, 6.0, 20.0] {
                let a = sampler.sample(az, ground, h);
                let b = column.at_height_km(h);
                assert_eq!(a, b, "az {az}, ground {ground}, height {h}");
                statuses.insert(a.status());
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 4 * 5 * 6);
    // precondition: the grid really did cross the boundaries, rather than
    // agreeing trivially on one status everywhere.
    assert!(
        statuses.len() >= 4,
        "the grid only produced {statuses:?}, so the agreement is not \
             saying much",
    );
}

/// A negative or non-finite query is answered, not panicked on: a UI can
/// hand this whatever the pointer was over.
#[test]
fn a_nonsensical_query_answers_no_coverage() {
    let scan = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 360, 40, 20.0)]);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    for (az, ground) in [
        (f64::NAN, 10.0),
        (10.0, f64::NAN),
        (10.0, -5.0),
        (f64::INFINITY, 10.0),
        (10.0, f64::INFINITY),
    ] {
        let column = sampler.column(az, ground);
        assert!(column.rungs().is_empty(), "az {az}, ground {ground}");
        assert_eq!(column.at_height_km(1.0).status(), SampleStatus::NoCoverage);
    }
    // An azimuth outside 0..360 is wrapped rather than refused, because a
    // bearing arrives from arithmetic that can overshoot either way. The
    // field has to *vary* with azimuth for this to say anything — on the
    // flat fixture above, every wrong answer is also the right one.
    let hot = |az: f64| {
        if (az - 5.0).abs() < 0.01 || (az - 355.0).abs() < 0.01 {
            55.0
        } else {
            10.0
        }
    };
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![refl_sweep_at(1, 0.5, &azimuths, 200, hot)],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    let at = |az: f64| sampler.column(az, 20.0).rungs()[0].sample;
    // precondition: the two azimuths under test are the hot ones, so a
    // wrap that landed anywhere else reads 10 rather than 55.
    assert_eq!(f64::from(at(5.0).value().unwrap()), round_trip_refl(55.0));
    assert_eq!(f64::from(at(355.0).value().unwrap()), round_trip_refl(55.0));
    assert_eq!(f64::from(at(9.0).value().unwrap()), round_trip_refl(10.0));

    assert_eq!(at(365.0), at(5.0), "an azimuth past 360° did not wrap");
    assert_eq!(at(-5.0), at(355.0), "a negative azimuth did not wrap");
    assert_eq!(at(725.0), at(5.0), "two turns past 360° did not wrap");
}

/// The ladder's shape accessors, which the section's axes are built from.
#[test]
fn the_ladder_reports_its_own_shape() {
    let angles = [0.5f64, 0.9, 7.0, 19.5];
    let sweeps: Vec<Sweep> = angles
        .iter()
        .enumerate()
        .map(|(i, &e)| flat_refl_sweep(i as u8 + 1, e as f32, 360, 40, 20.0))
        .collect();
    let scan = Scan::new(vcp(&angles), sweeps);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    assert_eq!(sampler.product(), RadarProduct::Reflectivity);
    assert_eq!(sampler.tilt_count(), 4);
    let gap = sampler.widest_tilt_gap_deg();
    assert!(
        (gap - 12.5).abs() < 1e-4,
        "the 7.0 → 19.5 gap read {gap}° — this is the number that warns a \
             section is interpolating a smooth layer across nothing",
    );
    let single = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 360, 40, 20.0)]);
    let single = VolumeSampler::new(&single, RadarProduct::Reflectivity).unwrap();
    assert_eq!(single.widest_tilt_gap_deg(), 0.0);
    assert_eq!(single.tilt_count(), 1);
}

/// Both interpolation stages refuse to invent a number when a corner did
/// not measure one, and the heaviest corner decides instead.
#[test]
fn a_corner_with_no_value_takes_the_cell_rather_than_being_averaged_in() {
    let v = Sample::found;
    let folded = Sample::missing(SampleStatus::RangeFolded);
    let below = Sample::missing(SampleStatus::BelowThreshold);

    // All values: a true weighted mean.
    assert_eq!(
        blend(Blend::Arithmetic, &[v(10.0), v(20.0)], &[0.25, 0.75], None).value(),
        Some(17.5),
    );
    // One corner missing: the heavier corner wins outright, with its own
    // value or its own status.
    assert_eq!(
        blend(Blend::Arithmetic, &[v(10.0), folded], &[0.75, 0.25], None),
        v(10.0),
    );
    assert_eq!(
        blend(Blend::Arithmetic, &[v(10.0), folded], &[0.25, 0.75], None),
        folded,
    );
    // Ties go to the earliest corner, so the answer does not depend on
    // iteration order.
    assert_eq!(
        blend(Blend::Arithmetic, &[v(10.0), folded], &[0.5, 0.5], None),
        v(10.0),
    );
    assert_eq!(
        blend(Blend::Arithmetic, &[folded, v(10.0)], &[0.5, 0.5], None),
        folded,
    );
    // Two different reasons stay different rather than merging.
    assert_eq!(
        blend(Blend::Arithmetic, &[below, folded], &[0.4, 0.6], None),
        folded,
    );
    assert_eq!(
        blend(Blend::Arithmetic, &[below, folded], &[0.6, 0.4], None),
        below,
    );
    // Zero total weight cannot divide, and falls through to the same rule.
    assert_eq!(
        blend(Blend::Arithmetic, &[v(10.0), v(20.0)], &[0.0, 0.0], None),
        v(10.0),
    );
    // Degenerate input answers rather than panicking.
    assert_eq!(
        blend(Blend::Arithmetic, &[], &[], None).status(),
        SampleStatus::NoCoverage,
    );
}

// ── The Nyquist seam ────────────────────────────────────────────────────

/// **The measurement that started this: +24.50 and −24.50 m/s averaged to
/// exactly 0.000.**
///
/// Nothing about that number is a rounding artefact to be waved away. It
/// is the display's word for calm air, written over the one place the
/// radar reported flow as fast as it can report — and it is stated here as
/// an exact equality on the old behaviour so that a future change which
/// merely makes the fabrication *small* cannot pass.
#[test]
fn a_velocity_pair_across_the_nyquist_seam_takes_the_nearer_gate() {
    let (out, back) = (Sample::found(24.5), Sample::found(-24.5));
    let limit = 24.5;

    // precondition: without a limit — which is every other moment, and
    // this moment before the fix — the answer is the fabricated calm.
    assert_eq!(
        blend(Blend::Arithmetic, &[out, back], &[0.5, 0.5], None).value(),
        Some(0.0),
        "precondition: the arithmetic mean of ±24.50 is not 0.000, so this \
             test is no longer standing on the defect it was written for",
    );

    // With the seam known, the nearer gate answers verbatim. These
    // corners are gate neighbours, so the guard is armed across gates.
    let seam = Some(Seam::AcrossGates(limit));
    assert_eq!(
        blend(Blend::Arithmetic, &[out, back], &[0.5, 0.5], seam).value(),
        Some(24.5),
    );
    assert_eq!(
        blend(Blend::Arithmetic, &[back, out], &[0.5, 0.5], seam).value(),
        Some(-24.5),
        "the tie goes to the earliest corner here too, so the seam rule \
             did not smuggle in an order dependence",
    );
    assert_eq!(
        blend(Blend::Arithmetic, &[out, back], &[0.3, 0.7], seam).value(),
        Some(-24.5),
    );

    // **Heaviest is the nearest sample, not the fastest one.** With the
    // near corner at +18 and the far one at −24.5, a "largest magnitude"
    // reading of the rule would answer −24.5 and turn every fold edge into
    // a peak-hold. Both corners sit outside `SEAM_PROXIMITY_ACROSS_GATES
    // · 24.5`, so the guard really does fire and the answer really is a
    // choice.
    let near_side = [Sample::found(18.0), Sample::found(-24.5)];
    assert!(
        straddles_fold(&near_side, Seam::AcrossGates(limit)),
        "precondition: the guard does not fire on this pair, so the answer \
             below is an ordinary mean and says nothing about heaviest",
    );
    assert_eq!(
        blend(Blend::Arithmetic, &near_side, &[0.7, 0.3], seam).value(),
        Some(18.0),
        "the heaviest corner is the nearest one, not the fastest one",
    );

    // The four-corner bilinear is the same rule: one straddling pair among
    // the corners is enough, wherever in the quad it sits.
    assert_eq!(
        blend(
            Blend::Arithmetic,
            &[out, out, out, back],
            &[0.4, 0.2, 0.2, 0.2],
            seam,
        )
        .value(),
        Some(24.5),
    );
}

/// The straddle test asks where each extreme sits, not how far apart the
/// two are — so it is a box around the seam and not a band on the spread,
/// and the difference between those two shapes is the whole point.
///
/// Every claim in the first half is a claim about the rule's *shape*, so
/// each is asserted across both adjacencies; the second half is where
/// each adjacency's own line sits, at the finest resolution a `Sample`
/// can carry. The lines are pinned by value in
/// [`each_guard_draws_its_line_at_its_own_fraction`] and through the
/// integrated paths by the two `holds_its_line` tests beside it.
#[test]
fn the_straddle_test_needs_both_extremes_near_the_seam() {
    for seam in [Seam::AcrossGates as fn(f64) -> Seam, Seam::AcrossTilts] {
        let s = |a: f64, b: f64, limit: f64| {
            straddles_fold(
                &[Sample::found(a as f32), Sample::found(b as f32)],
                seam(limit),
            )
        };

        // Same sign is a ramp, never a fold — however wide.
        assert!(!s(2.0, 40.0, 20.0), "a same-sign ramp is not a fold");
        assert!(!s(-2.0, -40.0, 20.0));

        // Opposite signs but nowhere near the seam: an ordinary zero
        // crossing, which is the zero isodop and must keep interpolating
        // smoothly.
        assert!(
            !s(2.0, -2.0, 20.0),
            "an ordinary zero crossing is not a fold"
        );
        assert!(!s(9.0, -9.0, 20.0));

        // **The pair that the old spread-only rule got wrong.** −5 and
        // +25 against a 20 m/s limit spread by 30, which clears a whole
        // fold period, and change sign — so the old rule called it a
        // fold. It cannot be one: a single wrap of a smooth field leaves
        // *both* sides near ±20, and −5 is a quarter of the way in.
        assert!(
            !s(25.0, -5.0, 20.0),
            "a wide straddle with one end deep inside the range is shear, \
                 not a fold — this is the case the spread test could not see",
        );
        assert!(!s(5.0, -25.0, 20.0), "and the same the other way round");

        // A real fold: piled against the ±limit seam.
        assert!(s(19.5, -19.5, 20.0));

        // A corner of exactly zero is on no side of the seam.
        assert!(!s(0.0, -24.5, 12.0), "zero is not the far side of a seam");

        // Strictly stronger than the rule it replaces: everything that
        // fires here would have fired under sign-change-plus-spread, and
        // the converse fails, which the `25.0, -5.0` case above is. This
        // holds for any fraction at or above ½, so it holds on both
        // paths.
        for a in -60..=60 {
            for b in -60..=60 {
                let (a, b) = (f64::from(a) * 0.5, f64::from(b) * 0.5);
                let (lo, hi) = (a.min(b), a.max(b));
                let old = lo < 0.0 && hi > 0.0 && hi - lo > 20.0;
                assert!(
                    !s(a, b, 20.0) || old,
                    "{a} and {b} fire under the seam rule but not under \
                         the spread rule, so the new rule is not strictly \
                         stronger",
                );
            }
        }
    }

    // Each extreme is tested on its own side of its own adjacency's
    // line, and one end past it is not enough. The gate bound survives
    // every conversion — `0.60 · 20` is exactly 12.0 in f64 and in a
    // `Sample`'s f32 alike — so the strictness of the rule's own `<` is
    // pinned exactly on the bound as well as either side of it. The tilt
    // bound `0.67 · 20 = 13.4` is representable in neither, so there the
    // nearest half-m/s readings either side — the finest step
    // legacy-resolution velocity takes — stand in. (An earlier note here
    // retired the exact-on-bound pin claiming neither shipped bound
    // survived the trip; the gate bound does, and the three on-bound
    // cases below restore the pin that claim removed.)
    let g = |a: f64, b: f64| {
        straddles_fold(
            &[Sample::found(a as f32), Sample::found(b as f32)],
            Seam::AcrossGates(20.0),
        )
    };
    assert!(!g(11.5, -11.5), "57.5% is inside the gate line (60%)");
    assert!(!g(12.5, -11.5), "one end past the gate line is not enough");
    assert!(!g(11.5, -12.5), "nor is the other end alone");
    assert!(g(12.5, -12.5), "62.5% is past the gate line on both ends");
    assert!(
        !g(12.0, -12.0),
        "exactly on the gate bound is not past it — the rule is strict",
    );
    assert!(!g(12.5, -12.0), "on the bound at the low end interpolates");
    assert!(!g(12.0, -12.5), "and on the bound at the high end too");

    let t = |a: f64, b: f64| {
        straddles_fold(
            &[Sample::found(a as f32), Sample::found(b as f32)],
            Seam::AcrossTilts(20.0),
        )
    };
    assert!(!t(13.0, -13.0), "65% is inside the tilt line (67%)");
    assert!(!t(13.5, -13.0), "one end past the tilt line is not enough");
    assert!(!t(13.0, -13.5), "nor is the other end alone");
    assert!(t(13.5, -13.5), "67.5% is past the tilt line on both ends");
}

/// The two fractions, by value, and that they are two.
///
/// Each number is the output of the corpus arbitration recorded on its
/// constant, so a drift in either is a re-decision and must read as one
/// here. The inequality is pinned separately because it is a separate
/// claim: an edit landing both paths on one number — either number —
/// undoes the split while leaving one of the value pins green. And the
/// *direction* is pinned because it is the physical content of the whole
/// argument: across tilts a real fold's ends stray further from the
/// seam, so the vertical guard must be the more reluctant one.
#[test]
fn each_guard_draws_its_line_at_its_own_fraction() {
    assert_eq!(
        SEAM_PROXIMITY_ACROSS_GATES, 0.60,
        "the gate fraction moved off its corpus break-even",
    );
    assert_eq!(
        SEAM_PROXIMITY_ACROSS_TILTS, 0.67,
        "the tilt fraction moved off its corpus break-even",
    );
    assert_ne!(
        SEAM_PROXIMITY_ACROSS_GATES, SEAM_PROXIMITY_ACROSS_TILTS,
        "the two adjacencies measured different break-evens; one number \
             serving both paths is the exact collapse the corpus ruled out",
    );
    // Compile-time on purpose — clippy points out the operands are
    // constants, and taking the hint makes this pin the hardest kind to
    // silence: reordering the two fractions does not fail a test run, it
    // refuses to build one.
    const {
        assert!(
            SEAM_PROXIMITY_ACROSS_TILTS > SEAM_PROXIMITY_ACROSS_GATES,
            "the vertical guard must demand more than the bilinear, not less",
        );
    }

    // One pair, read across each adjacency: what fires between two gates
    // need not fire between two tilts. This is the observable the two
    // constants exist to create, and no single fraction — whichever
    // value it took — could answer it both ways.
    let pair = [Sample::found(13.0), Sample::found(-13.0)];
    assert!(
        straddles_fold(&pair, Seam::AcrossGates(20.0)),
        "±13 against a limit of 20 is past the gate line",
    );
    assert!(
        !straddles_fold(&pair, Seam::AcrossTilts(20.0)),
        "±13 against a limit of 20 is short of the tilt line",
    );
}

/// **The gate guard's line, held from both sides through the real
/// bilinear.** A range seam between ±11 on a sweep whose limit is 20
/// puts both extremes at 55% of the limit — inside the 60% line — and
/// must interpolate; a seam between ±13 puts them at 65% — past it — and
/// must snap to a measured speed. The second fixture is half of the swap
/// detector: under the tilt fraction 65% is *inside* the line, so these
/// fixtures fail if the gate guard regresses to the old 0.5, moves off
/// its break-even, or trades fractions with the tilt guard.
#[test]
fn the_gate_guard_holds_its_line_from_both_sides() {
    // ── 55%: inside the line. Crossing the seam must visit speeds
    // between the sides, which a snapped read never produces. The
    // 20 m/s planted at the first gate arms the guard at 20, so the
    // ratio under test is set by the seam values, not by the pair.
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![make_sweep(
            1,
            0.5,
            360,
            200,
            None,
            Some(&|_, slant| {
                Some(if slant < gate_slant_km(1) {
                    20.0
                } else if slant < 10.0 {
                    11.0
                } else {
                    -11.0
                })
            }),
        )],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    assert_eq!(
        sampler.rungs[0].fold_limit_ms,
        Some(20.0),
        "precondition: the planted gate did not set the limit, so the \
             ratio below is not the one this test is about",
    );
    assert!(
        !straddles_fold(
            &[Sample::found(11.0), Sample::found(-11.0)],
            Seam::AcrossGates(20.0),
        ),
        "±11 against 20 is 55%, inside the 60% line, and must not fire",
    );
    let (mut between, mut saw_inner, mut saw_outer) = (0u32, false, false);
    for step in 0..4000 {
        let ground_km = 5.0 + f64::from(step) * 0.0025;
        let Some(value) = sampler.column(90.0, ground_km).rungs()[0].sample.value() else {
            continue;
        };
        assert!(
            (-11.0..=11.0).contains(&value),
            "{ground_km} km read {value} m/s, outside the two speeds \
                 either side of the seam",
        );
        if value == 11.0 {
            saw_inner = true;
        } else if value == -11.0 {
            saw_outer = true;
        } else {
            between += 1;
        }
    }
    assert!(
        saw_inner && saw_outer,
        "precondition: the swept range never crossed the seam",
    );
    assert!(
        between > 20,
        "only {between} samples fell between ±11; a 55% straddle is \
             being snapped rather than interpolated — the gate guard is \
             firing below its own line",
    );

    // ── 65%: past the line. Every read is a measured speed. ──
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![make_sweep(
            1,
            0.5,
            360,
            200,
            None,
            Some(&|_, slant| {
                Some(if slant < gate_slant_km(1) {
                    20.0
                } else if slant < 10.0 {
                    13.0
                } else {
                    -13.0
                })
            }),
        )],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    assert_eq!(sampler.rungs[0].fold_limit_ms, Some(20.0));
    assert!(
        straddles_fold(
            &[Sample::found(13.0), Sample::found(-13.0)],
            Seam::AcrossGates(20.0),
        ),
        "±13 against 20 is 65%, past the 60% line, and must fire",
    );
    let mut crossed = false;
    let mut previous = None;
    for step in 0..4000 {
        let ground_km = 5.0 + f64::from(step) * 0.0025;
        let Some(value) = sampler.column(90.0, ground_km).rungs()[0].sample.value() else {
            continue;
        };
        assert!(
            value == 13.0 || value == -13.0,
            "{ground_km} km read {value} m/s across a 65% straddle the \
                 gate guard must refuse to average — under a swapped tilt \
                 fraction this seam would interpolate",
        );
        if previous.is_some_and(|p| p != value) {
            crossed = true;
        }
        previous = Some(value);
    }
    assert!(
        crossed,
        "precondition: the swept range never crossed the seam, so this \
             test never exercised the blend it was written for",
    );
}

/// **The tilt guard's line, held from both sides through the real
/// lerp.** Two tilts at ±12.5 under a limit of 20 put the pair at 62.5%
/// — past the *gate* line at 60%, short of the *tilt* line at 67% — so
/// the lerp must keep interpolating: the midpoint is the plain mean,
/// 0.0. Two tilts at ±14 are at 70%, past the line, and must snap to a
/// measured speed. The 62.5% fixture is the other half of the swap
/// detector: under the gate fraction it fires, so it fails if the tilt
/// guard regresses to the old 0.5, moves off its break-even, or trades
/// fractions with the gate guard.
#[test]
fn the_tilt_guard_holds_its_line_from_both_sides() {
    // The midpoint of the lerp between tilts at `±speed`, with both
    // rungs' guards armed at 20 m/s by a planted first gate.
    let lerped_mid = |speed: f32| {
        let scan = Scan::new(
            vcp(&[0.5, 4.5]),
            vec![
                make_sweep(
                    1,
                    0.5,
                    360,
                    200,
                    None,
                    Some(&move |_, slant| {
                        Some(if slant < gate_slant_km(1) {
                            20.0
                        } else {
                            f64::from(speed)
                        })
                    }),
                ),
                make_sweep(
                    2,
                    4.5,
                    360,
                    200,
                    None,
                    Some(&move |_, slant| {
                        Some(if slant < gate_slant_km(1) {
                            -20.0
                        } else {
                            f64::from(-speed)
                        })
                    }),
                ),
            ],
        );
        let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
        for rung in 0..2 {
            assert_eq!(
                sampler.rungs[rung].fold_limit_ms,
                Some(20.0),
                "precondition: a planted gate did not set this rung's \
                     limit, so the ratio under test is not the stated one",
            );
        }
        let column = sampler.column(90.0, 30.0);
        let rungs = column.rungs();
        assert_eq!(rungs[0].sample.value(), Some(speed));
        assert_eq!(rungs[1].sample.value(), Some(-speed));
        let mid = 0.5 * (rungs[0].height_km + rungs[1].height_km);
        column.at_height_km(mid)
    };

    // ── 62.5%: short of the tilt line, past the gate line. ──
    let pair = [Sample::found(12.5), Sample::found(-12.5)];
    assert!(
        !straddles_fold(&pair, Seam::AcrossTilts(20.0)),
        "±12.5 against 20 is 62.5%, short of the 67% line, and must not \
             fire",
    );
    assert!(
        straddles_fold(&pair, Seam::AcrossGates(20.0)),
        "precondition: the same pair is past the gate line, so a swap of \
             the two fractions cannot leave the lerp below green",
    );
    let value = lerped_mid(12.5).value().expect("both rungs measured");
    assert!(
        value.abs() < 1e-6,
        "±12.5 under a 20 m/s limit is short of the tilt line and must \
             lerp to its plain mean at the midpoint; it read {value}, a \
             corner — the tilt guard is drawing the gate guard's line",
    );

    // ── 70%: past the tilt line. The midpoint is a measured speed. ──
    assert!(
        straddles_fold(
            &[Sample::found(14.0), Sample::found(-14.0)],
            Seam::AcrossTilts(20.0),
        ),
        "±14 against 20 is 70%, past the 67% line, and must fire",
    );
    let value = lerped_mid(14.0).value().expect("both rungs measured");
    assert!(
        value == 14.0 || value == -14.0,
        "the midpoint between a +14 tilt and a −14 tilt read {value}, \
             which is a speed neither tilt measured — the tilt guard did not \
             fire past its own line",
    );
}

/// **The tilt line's lower side, at quarter-quantum resolution.** Against
/// a 20 m/s limit the tilt fraction `0.67` draws its line at 13.4 and the
/// next band down, `0.65`, at 13.0 — and the 0.5 m/s legacy-resolution
/// grid steps from 13.0 straight to 13.5, over the whole interval where
/// the two disagree. So every half-quantum fixture in this file is blind
/// to the tilt fraction slipping `0.67 → 0.65`, and only the literal
/// value pin would notice. Super-res velocity is quantised at 0.25 m/s,
/// and ±13.25 — 66.25% of the limit — sits inside the disputed band:
/// short of the shipped line, past the band below it. This drives that
/// pair through the real vertical path at the real super-res encoding
/// and pins that it still lerps.
#[test]
fn the_tilt_line_holds_its_lower_side_at_quarter_quantum() {
    // The super-res encoding: 0.25 m/s per raw step. ±20 planted at the
    // first gate arms both rungs at 20; ±13.25 everywhere else is the
    // pair under test, and both survive the encoding exactly.
    const SUPER_RES_VEL_SCALE: f32 = 4.0;
    let sweep = |elevation_number: u8, elevation_deg: f32, sign: f64| {
        let radials = (0..360u16)
            .map(|i| {
                let bytes: Vec<u8> = (0..200usize)
                    .map(|j| {
                        let ms = sign * if j == 0 { 20.0 } else { 13.25 };
                        (ms * f64::from(SUPER_RES_VEL_SCALE) + f64::from(VEL_OFFSET)).round() as u8
                    })
                    .collect();
                Radial::new(
                    0,
                    i,
                    f32::from(i),
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    None,
                    Some(moment_from(bytes, SUPER_RES_VEL_SCALE, VEL_OFFSET)),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    };
    let scan = Scan::new(
        vcp(&[0.5, 4.5]),
        vec![sweep(1, 0.5, 1.0), sweep(2, 4.5, -1.0)],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    for rung in 0..2 {
        assert_eq!(
            sampler.rungs[rung].fold_limit_ms,
            Some(20.0),
            "precondition: the planted gate did not set this rung's \
                 limit, so the ratio under test is not the stated one",
        );
    }

    // The rule itself, on the pair the fixture carries.
    assert!(
        !straddles_fold(
            &[Sample::found(13.25), Sample::found(-13.25)],
            Seam::AcrossTilts(20.0),
        ),
        "±13.25 against 20 is 66.25%, short of the 67% line, and must \
             not fire — a tilt fraction of 0.65 would fire here",
    );

    // And through the real lerp: the midpoint is the plain mean, 0.0.
    let column = sampler.column(90.0, 30.0);
    let rungs = column.rungs();
    assert_eq!(
        rungs[0].sample.value(),
        Some(13.25),
        "precondition: the quarter-quantum speed did not survive the \
             super-res encoding, so the pair under test is not ±13.25",
    );
    assert_eq!(rungs[1].sample.value(), Some(-13.25));
    let mid = 0.5 * (rungs[0].height_km + rungs[1].height_km);
    let value = column.at_height_km(mid).value().expect("both measured");
    assert!(
        value.abs() < 1e-6,
        "±13.25 under a 20 m/s limit is short of the tilt line and must \
             lerp to its plain mean at the midpoint; it read {value}, a \
             corner — the tilt guard slipped to a lower band",
    );
}

/// **The vertical lerp is the total case: a two-corner blend at `t = 0.5`
/// of `±v` is identically zero, so every fold-straddling rung pair halfway
/// up fabricates.**
///
/// Measured over fourteen volumes, 12,903 of the 12,918 rung pairs an
/// independent continuity oracle confirms as folds — 99.9% — average to
/// less than a quarter of the sweep's Nyquist velocity. The horizontal fix
/// alone *raises* the vertical count, because unsmeared fold structure
/// then survives to reach this stage.
///
/// (An earlier version of this note claimed 94.9% at KLWX-2018 against 97%
/// of straddling gate pairs. Neither number reproduces; see
/// [`Column::at_height_km`] for the withdrawal.)
#[test]
fn the_vertical_lerp_does_not_average_two_tilts_across_the_seam() {
    let scan = Scan::new(
        vcp(&[0.5, 4.5]),
        vec![
            make_sweep(1, 0.5, 360, 200, None, Some(&|_, _| Some(24.5))),
            make_sweep(2, 4.5, 360, 200, None, Some(&|_, _| Some(-24.5))),
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    let column = sampler.column(90.0, 30.0);
    let rungs = column.rungs();
    assert_eq!(rungs.len(), 2, "precondition: two rungs to lerp between");
    assert_eq!(rungs[0].sample.value(), Some(24.5));
    assert_eq!(rungs[1].sample.value(), Some(-24.5));
    assert_eq!(
        rungs[0].fold_limit_ms,
        Some(24.5),
        "precondition: the seam was measured off the sweep, so the guard \
             is armed rather than silently absent",
    );

    // Exactly halfway: `t = 0.5`, where the mean of ±24.5 is zero.
    let mid = 0.5 * (rungs[0].height_km + rungs[1].height_km);
    let sample = column.at_height_km(mid);
    let value = sample.value().expect("both rungs measured");
    assert!(
        value == 24.5 || value == -24.5,
        "the midpoint between a +24.50 tilt and a −24.50 tilt read \
             {value}, which is a speed neither tilt measured",
    );

    // And the whole span between the rungs stays on measured speeds
    // rather than sweeping through calm air on its way over.
    for step in 0..=100 {
        let t = f64::from(step) / 100.0;
        let h = rungs[0].height_km + t * (rungs[1].height_km - rungs[0].height_km);
        let v = column.at_height_km(h).value().expect("inside the ladder");
        assert!(
            v == 24.5 || v == -24.5,
            "t={t}: the vertical lerp read {v} between two tilts that \
                 measured only ±24.50",
        );
    }
}

/// The end-to-end horizontal path: a fold seam in range, sampled through
/// the real bilinear rather than through [`blend`] alone.
///
/// This is the test that pins the *plumbing* — that velocity actually
/// reaches [`blend`] with a limit. A fix living entirely inside `blend`
/// with nothing wired to it would pass every unit test above and change
/// nothing a caller can see.
#[test]
fn a_fold_seam_in_range_never_reads_as_calm_air() {
    // +24.5 out to 10 km, −24.5 beyond: the seam sits between two gates.
    let seam_km = 10.0;
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![make_sweep(
            1,
            0.5,
            360,
            200,
            None,
            Some(&move |_, slant| Some(if slant < seam_km { 24.5 } else { -24.5 })),
        )],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();

    let mut crossed = false;
    let mut previous = None;
    for step in 0..4000 {
        // Fine enough to land inside the one gate interval the seam
        // occupies, from both sides.
        let ground_km = 5.0 + f64::from(step) * 0.0025;
        let sample = sampler.column(90.0, ground_km).rungs()[0].sample;
        let Some(value) = sample.value() else {
            continue;
        };
        assert!(
            value == 24.5 || value == -24.5,
            "{ground_km} km read {value} m/s across a seam whose two sides \
                 measured only ±24.50",
        );
        if previous.is_some_and(|p| p != value) {
            crossed = true;
        }
        previous = Some(value);
    }
    assert!(
        crossed,
        "precondition: the swept range never crossed the seam, so this \
             test never exercised the blend it was written for",
    );
}

/// **The band where the answer actually changed: a spread of 1.0–1.5 fold
/// limits.** Between 40% and 60% of the guard's real fires land here, and
/// before this test nothing exercised it through the integrated path at
/// all — the fixtures either straddled at `±limit` (ratio 2.0, unambiguous
/// fold) or crossed gently (ratio 0.04–0.72, unambiguous shear).
///
/// +25.0 m/s inbound of 10 km and −5.0 m/s beyond it, on a sweep that
/// folds at 25.0. The spread is 30.0 — **1.2 fold limits**, so it changes
/// sign and clears a whole period, and the rule this replaces called it a
/// fold and answered with one endpoint or the other. It cannot be one: a
/// single wrap of a smooth field leaves both sides within the pair's own
/// true change of ±25, and −5.0 is a fifth of the way in. It is an echo
/// boundary with strong shear across it, and it must interpolate.
#[test]
fn a_wide_zero_crossing_in_the_disputed_band_still_interpolates() {
    let (fast, slow, seam_km) = (25.0f32, -5.0f32, 10.0);
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![make_sweep(
            1,
            0.5,
            360,
            200,
            None,
            Some(&move |_, slant| {
                Some(if slant < seam_km {
                    f64::from(fast)
                } else {
                    f64::from(slow)
                })
            }),
        )],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    let limit = sampler.rungs[0]
        .fold_limit_ms
        .expect("precondition: the sweep claimed no seam, so nothing below could misfire");
    assert_eq!(limit, 25.0);

    // precondition: this pair really is in the disputed band, and the rule
    // being replaced really did fire on it. Without both, a pass below
    // says nothing — the test would be standing on a case no detector
    // ever disagreed about.
    let pair = [Sample::found(fast), Sample::found(slow)];
    let (lo, hi) = (f64::from(slow), f64::from(fast));
    let ratio = (hi - lo) / limit;
    assert!(
        (1.0..1.5).contains(&ratio),
        "precondition: the spread ratio is {ratio}, outside the 1.0–1.5 \
             band this test exists to cover",
    );
    assert!(
        lo < 0.0 && hi > 0.0 && hi - lo > limit,
        "precondition: the sign-change-and-spread rule does not fire here, \
             so this test cannot observe its removal",
    );
    // (The corners span gates, and the gate line has since moved outward
    // from the 0.5 this test was written against to its corpus
    // break-even at 0.60 — which *widens* the disputed band and leaves
    // this pair, at 20% of the limit, still deep inside the population
    // the two rules disagree about. The preconditions above are what
    // hold that claim in place.)
    assert!(
        !straddles_fold(&pair, Seam::AcrossGates(limit)),
        "a straddle with one end a fifth of the way into the range was \
             read as a fold",
    );

    // And through the real bilinear: crossing the boundary must visit
    // speeds between the two sides rather than stepping from one to the
    // other.
    let mut between = 0;
    let mut saw_fast = false;
    let mut saw_slow = false;
    for step in 0..4000 {
        let ground_km = 5.0 + f64::from(step) * 0.0025;
        let Some(value) = sampler.column(90.0, ground_km).rungs()[0].sample.value() else {
            continue;
        };
        assert!(
            (slow..=fast).contains(&value),
            "{ground_km} km read {value} m/s, outside the two speeds either \
                 side of the boundary",
        );
        if value == fast {
            saw_fast = true;
        } else if value == slow {
            saw_slow = true;
        } else {
            between += 1;
        }
    }
    assert!(
        saw_fast && saw_slow,
        "precondition: the swept range never crossed the boundary",
    );
    assert!(
        between > 20,
        "only {between} samples fell between {fast} and {slow}; the \
             boundary is being resampled rather than interpolated",
    );
}

/// **A gentle zero crossing is resampled by nobody: it is interpolated,
/// gate by gate, across the whole ramp.** The zero isodop is where the
/// flow is perpendicular to the beam and the field genuinely passes
/// through zero, and it is the most-read feature of a velocity display.
///
/// # What this test can and cannot catch — read before trusting the name
///
/// The ramp below steps 1 m/s per quad against a measured limit of
/// 24.5 m/s, a spread ratio of **0.041**. So it refuses a detector keyed
/// on sign alone, and it refuses any threshold below about 4% of the fold
/// limit, and that is the whole of its reach — it does not appear in the
/// kill list of any mutation that moves the threshold within the range a
/// threshold could plausibly take. An earlier version of this comment
/// claimed it refused "any threshold small enough to catch a gentle
/// crossing", which is not a claim its fixture supports.
///
/// The tests that do resolve the detector's boundary are
/// [`the_straddle_test_needs_both_extremes_near_the_seam`] on the rule
/// itself, [`each_guard_draws_its_line_at_its_own_fraction`] on the two
/// fractions by value, and the two `holds_its_line` tests beside them,
/// which hold each adjacency's own line from both sides through the
/// integrated paths. A smooth ramp cannot be one of them: to
/// put the pair bracketing zero at the rule's own bound the ramp would
/// have to step a whole fold limit per gate, which is not a ramp.
#[test]
fn a_gentle_zero_crossing_interpolates_gate_by_gate() {
    // −24.5 m/s inbound at the first gate ramping to +24.5 outbound at
    // gate 49: 4 m/s per km, which is **1 m/s per gate — two quantisation
    // steps** — so consecutive gates really do differ, and the pair either
    // side of zero reads −0.5 and +0.5 rather than 0 and 0. That slope is
    // 0.004 s⁻¹, an entirely ordinary shear, and the sweep's measured
    // limit is 24.5 m/s: the guard is armed, well clear of the floor, and
    // genuinely has the chance to misfire on every gate of the ramp.
    let ramp = |slant: f64| -24.5 + (slant - gate_slant_km(0)) * 4.0;
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![make_sweep(
            1,
            0.5,
            360,
            50,
            None,
            Some(&move |_, slant| Some(ramp(slant))),
        )],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();

    // precondition: the guard is armed, so a pass here is the detector
    // declining to fire rather than the detector being switched off.
    assert_eq!(
        sampler.rungs[0].fold_limit_ms,
        Some(24.5),
        "precondition: this sweep's seam was not measured, so nothing \
             below could have misfired anyway",
    );

    // Halfway between the two gates that bracket zero, the answer is their
    // arithmetic mean and *not* either endpoint — which is exactly what a
    // detector firing here would destroy.
    let decode = |j: usize| {
        f64::from((f32::from(encode_vel(ramp(gate_slant_km(j)))) - VEL_OFFSET) / VEL_SCALE)
    };
    let (near, far) = (decode(24), decode(25));
    assert!(
        near < 0.0 && far > 0.0,
        "precondition: gates 24 and 25 read {near} and {far}, which do \
             not bracket zero, so this test is not standing on a crossing",
    );
    assert_eq!(
        blend(
            Blend::Arithmetic,
            &[Sample::found(near as f32), Sample::found(far as f32)],
            &[0.5, 0.5],
            Some(Seam::AcrossGates(24.5)),
        )
        .value(),
        Some(((near + far) / 2.0) as f32),
        "a gentle zero crossing was taken for a fold and went blocky",
    );

    // And across the whole ramp the sampled profile stays monotone and
    // visits values between the gate centres, which a blocky read cannot.
    let mut seen_between = 0;
    let mut previous = f32::NEG_INFINITY;
    for step in 0..2000 {
        let ground_km = 2.5 + f64::from(step) * 0.0055;
        let Some(value) = sampler.column(90.0, ground_km).rungs()[0].sample.value() else {
            continue;
        };
        assert!(
            value >= previous - 1e-4,
            "the ramp reversed at {ground_km} km: {previous} then {value}",
        );
        // A value strictly between two encoded gate centres — the 0.5 m/s
        // quantum — can only come from interpolating.
        if (f64::from(value) * 2.0).fract().abs() > 1e-6 {
            seen_between += 1;
        }
        previous = value;
    }
    assert!(
        seen_between > 1000,
        "only {seen_between} of ~2000 samples fell between gate centres; \
             the ramp is being resampled rather than interpolated",
    );
}

/// Below [`FOLD_LIMIT_FLOOR_MS`] the guard is off, because a sweep that
/// saw nothing fast has no measured seam to trust — and the lerp over such
/// a sweep really does come back with the plain mean, which this computes
/// rather than implies.
#[test]
fn a_sweep_too_slow_to_have_folded_keeps_its_plain_mean() {
    let quiet = make_sweep(1, 0.5, 360, 200, None, Some(&|_, _| Some(3.0)));
    let scan = Scan::new(vcp(&[0.5]), vec![quiet]);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    assert_eq!(
        sampler.rungs[0].fold_limit_ms, None,
        "3 m/s is below the {FOLD_LIMIT_FLOOR_MS} m/s floor, so no seam \
             should have been claimed for this sweep",
    );

    // **The mean the name promises.** Two tilts at ±7.5 m/s — the fastest
    // pair that still leaves both sweeps under the floor — lerp to 0.0 at
    // the midpoint. That number is the one the whole change exists to
    // refuse *when there is a seam*, and here there is not, so it must
    // survive: with the floor at 7.0 instead of 8.0 both sweeps would arm
    // at 7.5, `±7.5` clears `SEAM_PROXIMITY_ACROSS_TILTS · 7.5` at both
    // ends, and this would read ±7.5 rather than nothing.
    let scan = Scan::new(
        vcp(&[0.5, 4.5]),
        vec![
            make_sweep(1, 0.5, 360, 200, None, Some(&|_, _| Some(7.5))),
            make_sweep(2, 4.5, 360, 200, None, Some(&|_, _| Some(-7.5))),
        ],
    );
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    assert_eq!(sampler.rungs[0].fold_limit_ms, None);
    assert_eq!(sampler.rungs[1].fold_limit_ms, None);
    let column = sampler.column(90.0, 30.0);
    let rungs = column.rungs();
    assert_eq!(rungs[0].sample.value(), Some(7.5));
    assert_eq!(rungs[1].sample.value(), Some(-7.5));
    let mid = 0.5 * (rungs[0].height_km + rungs[1].height_km);
    let value = column.at_height_km(mid).value().expect("both measured");
    assert!(
        value.abs() < 1e-6,
        "with no seam measured the vertical lerp is a plain mean, and \
             halfway between +7.5 and −7.5 that is 0.0 — this read {value}, \
             which is a corner rather than a mean",
    );

    // The same sweep one step over the floor does arm the guard, so the
    // floor is a threshold rather than a switch that is always off.
    let fast = make_sweep(1, 0.5, 360, 200, None, Some(&|_, _| Some(8.0)));
    let scan = Scan::new(vcp(&[0.5]), vec![fast]);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    assert_eq!(sampler.rungs[0].fold_limit_ms, Some(8.0));

    // And the two numbers mean the same thing in both modules.
    assert_eq!(
        FOLD_LIMIT_FLOOR_MS, 8.0,
        "the floor moved away from the one `nrot` abandons dealiasing at",
    );
}

/// **One rung with a measured seam and one without still guards the lerp.**
///
/// [`Column::at_height_km`] takes the tighter of the two limits when both
/// rungs have one and falls back to whichever exists when only one does.
/// That fallback arm — `(a, b) => a.or(b)` — is reachable and was
/// unpinned: replacing it with `None` left every other velocity test
/// passing, because every other fixture arms both rungs.
///
/// The shape is real rather than contrived. A clear-air coverage pattern
/// measures a fold limit around 11 m/s; a higher cut of the same volume
/// looking at slow air can report nothing over [`FOLD_LIMIT_FLOOR_MS`]
/// and so claims no seam at all, while the cut below it folds.
///
/// (The armed sweep read 12.5 when the vertical fraction was 0.5. This
/// pair must beat `SEAM_PROXIMITY_ACROSS_TILTS` of the one measured
/// limit on both ends while its slow side stays under the 8.0 floor
/// that keeps that sweep unarmed — which caps the armed limit below
/// `8.0 / 0.67 ≈ 11.9`. At 12.5 the old pair sat at 60% and the tilt
/// guard now rightly declines it, so the one-sided arm this test exists
/// to pin was never reached; the straddle precondition below is what
/// keeps this fixture from going silently dead the same way again.)
#[test]
fn a_lerp_between_one_measured_seam_and_one_unmeasured_still_guards() {
    for (low, high, expect_lo, expect_hi) in
        [(11.0, -7.5, 11.0f32, -7.5f32), (7.5, -11.0, 7.5, -11.0)]
    {
        let scan = Scan::new(
            vcp(&[0.5, 4.5]),
            vec![
                make_sweep(1, 0.5, 360, 200, None, Some(&move |_, _| Some(low))),
                make_sweep(2, 4.5, 360, 200, None, Some(&move |_, _| Some(high))),
            ],
        );
        let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
        // precondition: exactly one rung claims a seam, so this really is
        // the one-sided arm and not the `min` of two.
        let claimed: Vec<_> = sampler
            .rungs
            .iter()
            .map(|r| r.fold_limit_ms)
            .filter(|l| l.is_some())
            .collect();
        assert_eq!(
            claimed.len(),
            1,
            "precondition: {low}/{high} armed {} rungs, so this fixture \
                 does not exercise the one-sided arm",
            claimed.len(),
        );
        // precondition: the pair still straddles at the tilt fraction of
        // the one measured limit — without this, a fraction change can
        // park the fixture below the line and the assert at the bottom
        // stops observing the arm it names.
        let limit = claimed[0].expect("one rung claimed a seam");
        assert!(
            straddles_fold(
                &[Sample::found(low as f32), Sample::found(high as f32)],
                Seam::AcrossTilts(limit),
            ),
            "precondition: {low}/{high} does not straddle at the tilt \
                 fraction of {limit}, so the guard below never fires and the \
                 one-sided arm goes unobserved",
        );

        let column = sampler.column(90.0, 30.0);
        let rungs = column.rungs();
        assert_eq!(rungs[0].sample.value(), Some(expect_lo));
        assert_eq!(rungs[1].sample.value(), Some(expect_hi));
        let mid = 0.5 * (rungs[0].height_km + rungs[1].height_km);
        let value = column
            .at_height_km(mid)
            .value()
            .expect("both rungs measured");
        assert!(
            value == expect_lo || value == expect_hi,
            "the midpoint between {expect_lo} and {expect_hi} read {value}, \
                 so the one rung that measured a seam did not reach the lerp",
        );
    }
}

/// Which moments have a seam at all, stated once and exhaustively.
#[test]
fn velocity_is_the_only_moment_with_an_unstated_fold_limit() {
    for &product in RadarProduct::all() {
        let expected = product == RadarProduct::Velocity;
        assert_eq!(
            Blend::folds_at_measured_limit(product),
            expected,
            "{product:?}",
        );
    }
    // Spectrum width is the near miss: it is a Doppler moment off the same
    // sweep, so it looks like velocity — but it is a non-negative spread,
    // so no two of its gates can sit on opposite sides of a seam and
    // `straddles_fold` could never fire on it even if it were armed.
    assert!(!Blend::folds_at_measured_limit(RadarProduct::SpectrumWidth));
    // Differential phase does wrap, and is handled by a blend arm instead,
    // because 360° is a constant the format does not have to carry.
    assert!(!Blend::folds_at_measured_limit(
        RadarProduct::DifferentialPhase
    ));
    assert_eq!(
        Blend::for_moment(RadarProduct::DifferentialPhase),
        Blend::Angular360,
    );
    // Nothing but velocity pays for the measuring pass.
    let scan = Scan::new(vcp(&[0.5]), vec![flat_refl_sweep(1, 0.5, 360, 40, 20.0)]);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    assert_eq!(sampler.rungs[0].fold_limit_ms, None);
}

/// The limit is read per rung, because the Nyquist velocity follows the
/// cut's PRF and genuinely differs inside one volume.
#[test]
fn each_rung_measures_its_own_seam_and_the_lerp_takes_the_tighter_one() {
    // **The two sweeps must differ in their limits *and* read something
    // other than those limits where they are sampled**, or `min` and `max`
    // cannot be told apart. Two flat sweeps can never separate them: a
    // flat sweep's limit is the speed it reads, so an opposite-sign pair
    // always differs by `l0 + l1`, which clears the larger limit too. So
    // the high tilt reads −11 where it is sampled while folding at 30
    // somewhere else — which is exactly the real shape, a cut whose PRF
    // sets a seam far above anything in this particular column.
    let low = make_sweep(1, 0.5, 360, 200, None, Some(&|_, _| Some(11.5)));
    let high = make_sweep(
        2,
        4.5,
        360,
        200,
        None,
        Some(&|_, slant| {
            Some(if slant < gate_slant_km(1) {
                30.0
            } else {
                -11.0
            })
        }),
    );
    let scan = Scan::new(vcp(&[0.5, 4.5]), vec![low, high]);
    let sampler = VolumeSampler::new(&scan, RadarProduct::Velocity).unwrap();
    assert_eq!(sampler.rungs[0].fold_limit_ms, Some(11.5));
    assert_eq!(sampler.rungs[1].fold_limit_ms, Some(30.0));

    // +11.5 and −11.0 differ by 22.5, which clears the *smaller* limit and
    // not the larger. Testing against the larger would miss this straddle
    // entirely, so this pins the `min` and not merely that some limit was
    // reaching the lerp.
    let pair = [Sample::found(11.5), Sample::found(-11.0)];
    assert!(
        straddles_fold(&pair, Seam::AcrossTilts(11.5)),
        "the tighter seam is crossed",
    );
    assert!(
        !straddles_fold(&pair, Seam::AcrossTilts(30.0)),
        "precondition: the wider limit also fires here, so this fixture \
             cannot tell `min` from `max`",
    );

    let column = sampler.column(90.0, 30.0);
    let rungs = column.rungs();
    assert_eq!(rungs[0].sample.value(), Some(11.5));
    assert_eq!(rungs[1].sample.value(), Some(-11.0));
    let mid = 0.5 * (rungs[0].height_km + rungs[1].height_km);
    let value = column.at_height_km(mid).value().expect("both measured");
    assert!(
        value == 11.5 || value == -11.0,
        "the lerp read {value} between rungs measuring +11.5 and −11.0, so \
             it used the wider limit and averaged across the tighter seam",
    );
}

/// The estimator reads the same numbers [`gate_sample`] does, off the same
/// bytes, including the encodings that make a reimplementation wrong.
#[test]
fn the_fold_limit_is_the_fastest_speed_the_sweep_actually_reports() {
    // A field whose extreme is planted at one known gate, so a scan that
    // stopped early or skipped the status codes reads differently.
    let field = |_: f64, slant: f64| {
        Some(if (slant - gate_slant_km(150)).abs() < 1e-9 {
            -31.5
        } else if (slant - gate_slant_km(3)).abs() < 1e-9 {
            28.0
        } else {
            4.0
        })
    };
    let sweep = make_sweep(1, 0.5, 360, 200, None, Some(&field));
    let radials = sweep.radials();
    assert_eq!(
        estimate_fold_limit(radials, MomentSlot::Velocity),
        Some(31.5),
        "the estimate is the largest |speed| in the sweep, whichever sign \
             it has and wherever in the radial it sits",
    );

    // Brute force over every gate, as the definition rather than the
    // implementation: the affine shortcut must agree with it exactly.
    let mut brute = 0.0f64;
    for radial in radials {
        let moment = MomentSlot::Velocity.read(radial).unwrap();
        for gate in 0..moment.raw_values().len() {
            let sample = gate_sample(moment, gate);
            if sample.status == SampleStatus::Value {
                brute = brute.max(f64::from(sample.value).abs());
            }
        }
    }
    assert_eq!(brute, 31.5);

    // **The status codes, planted as raw bytes.** Every fixture above
    // encodes through `encode_vel`, which clamps to 2..=255 and so can
    // never produce a 0 or a 1 — meaning no fixture above can catch an
    // estimator that reads the status codes as speeds. Raw 0 decodes to
    // −64.5 m/s and raw 1 to −64.0 under this encoding, so an estimator
    // that admitted either would claim a seam nearly three times the real
    // one and switch the guard off in practice.
    let coded = Radial::new(
        0,
        0,
        0.0,
        1.0,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        None,
        Some(moment_from(
            vec![0, 1, 2, encode_vel(24.5), encode_vel(-20.0), 1, 0],
            VEL_SCALE,
            VEL_OFFSET,
        )),
        None,
        None,
        None,
        None,
        None,
    );
    assert_eq!(
        gate_sample(MomentSlot::Velocity.read(&coded).unwrap(), 0).status(),
        SampleStatus::BelowThreshold,
        "precondition: raw 0 is not the below-threshold code here",
    );
    assert_eq!(
        gate_sample(MomentSlot::Velocity.read(&coded).unwrap(), 1).status(),
        SampleStatus::RangeFolded,
        "precondition: raw 1 is not the range-folded code here",
    );
    // **Raw 2 is the boundary the filter is actually written at, and it is
    // an admitted value, not a code.** Planting 0 and 1 alone only shows
    // that the filter is at least as strict as `>= 2`; a filter one step
    // stricter still passes. Raw 2 decodes to −63.5 m/s here — larger than
    // anything else in the radial — so admitting it and skipping it give
    // different answers, and the estimate below distinguishes all three
    // neighbouring filters at once: `>= 3` reads 24.5, `>= 2` reads 63.5,
    // `>= 1` reads 64.0 and `>= 0` reads 64.5.
    assert_eq!(
        gate_sample(MomentSlot::Velocity.read(&coded).unwrap(), 2),
        Sample::found(-63.5),
        "precondition: raw 2 is not the first admitted code here, so it \
             cannot pin the filter's boundary",
    );
    assert_eq!(
        estimate_fold_limit(std::slice::from_ref(&coded), MomentSlot::Velocity),
        Some(63.5),
        "the estimate does not run from the first admitted code to the last",
    );

    // A sweep of nothing but below-threshold gates claims no seam at all
    // rather than a seam of zero.
    let empty = make_sweep(1, 0.5, 360, 200, None, Some(&|_, _| None));
    assert_eq!(
        estimate_fold_limit(empty.radials(), MomentSlot::Velocity),
        None,
    );
    // And a sweep with no velocity moment at all does the same.
    let refl = flat_refl_sweep(1, 0.5, 360, 40, 20.0);
    assert_eq!(
        estimate_fold_limit(refl.radials(), MomentSlot::Velocity),
        None,
    );
}

/// The linear-Z question has one answer in this crate, not two.
#[test]
fn the_blend_table_agrees_with_the_echo_top_cubes() {
    for product in [
        RadarProduct::Reflectivity,
        RadarProduct::Velocity,
        RadarProduct::SpectrumWidth,
        RadarProduct::DifferentialReflectivity,
        RadarProduct::CorrelationCoefficient,
    ] {
        let wants_linear_z = CellStat::for_moment(product) == CellStat::LinearZMean;
        assert_eq!(
            Blend::for_moment(product) == Blend::LinearZ,
            wants_linear_z,
            "{product:?}: the sampler and CellStat disagree about whether \
                 it averages in linear Z",
        );
    }
    // Differential phase is the one arm `CellStat` does not have, and it
    // must not fall through to the arithmetic mean.
    assert_eq!(
        Blend::for_moment(RadarProduct::DifferentialPhase),
        Blend::Angular360,
    );
    assert_eq!(
        CellStat::for_moment(RadarProduct::DifferentialPhase),
        CellStat::Mean,
        "precondition: CellStat grew an angular arm, so this module should \
             read it instead of overriding it",
    );
}
