use super::*;
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, PulseWidth, VolumeCoveragePattern, WaveformType,
};

const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;

/// The site handed to `prepare` for its memo key.
const SITE: (f64, f64) = (35.33, -97.28);

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

fn gate_slant_km(j: usize) -> f64 {
    f64::from(FIRST_GATE_M) / 1000.0 + j as f64 * f64::from(GATE_M) / 1000.0
}

/// Per-gate fields, m/s / dBZ / degrees / unitless, `None` = no data.
type Fields<'f> = &'f dyn Fn(f64, f64) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>);

fn sweep_with(
    elevation_number: u8,
    elevation_deg: f32,
    n_radials: usize,
    n_gates: usize,
    fields: Fields<'_>,
) -> Sweep {
    sweep_with_clock(
        0,
        elevation_number,
        elevation_deg,
        n_radials,
        n_gates,
        fields,
    )
}

/// [`sweep_with`] with a clock on every radial, milliseconds since the Unix epoch.
fn sweep_with_clock(
    collected_ms: i64,
    elevation_number: u8,
    elevation_deg: f32,
    n_radials: usize,
    n_gates: usize,
    fields: Fields<'_>,
) -> Sweep {
    let encode = |v: Option<f64>, scale: f64, offset: f64| -> u8 {
        match v {
            None => 0,
            Some(v) => ((v * scale + offset).round() as i64).clamp(2, 255) as u8,
        }
    };
    let spacing = 360.0 / n_radials as f32;
    let radials = (0..n_radials)
        .map(|i| {
            let az = i as f64 * f64::from(spacing);
            let mut refl = Vec::with_capacity(n_gates);
            let mut vel = Vec::with_capacity(n_gates);
            let mut phi = Vec::with_capacity(n_gates);
            let mut rho = Vec::with_capacity(n_gates);
            for j in 0..n_gates {
                let (r, v, p, c) = fields(az, gate_slant_km(j));
                refl.push(encode(r, 2.0, 66.0));
                vel.push(encode(v, 2.0, 129.0));
                phi.push(encode(p, 2.8361, 2.0));
                rho.push(encode(c, 300.0, -60.5));
            }
            let moment = |bytes: Vec<u8>, scale: f32, offset: f32| {
                Some(MomentData::from_fixed_point(
                    bytes.len() as u16,
                    FIRST_GATE_M,
                    GATE_M,
                    8,
                    scale,
                    offset,
                    bytes,
                ))
            };
            Radial::new(
                collected_ms,
                i as u16,
                az as f32,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                moment(refl, 2.0, 66.0),
                moment(vel, 2.0, 129.0),
                None,
                None,
                moment(phi, 2.8361, 2.0),
                moment(rho, 300.0, -60.5),
                None,
            )
        })
        .collect();
    Sweep::new(elevation_number, radials)
}

/// Two tilts over `fields`, under a matching two-cut pattern.
fn scan_with(fields: Fields<'_>) -> Scan {
    Scan::new(
        vcp(&[0.5, 1.5]),
        vec![
            sweep_with(1, 0.53, 360, 240, fields),
            sweep_with(2, 1.47, 360, 240, fields),
        ],
    )
}

fn decoded(scan: &Scan, sweep: usize, radial: usize, gate: usize, slot: MomentSlot) -> Option<f64> {
    use nexrad_model::data::MomentValue;
    let radial = &scan.sweeps()[sweep].radials()[radial];
    let moment = slot.read(radial)?;
    match moment.iter().nth(gate)? {
        MomentValue::Value(v) => Some(f64::from(v)),
        _ => None,
    }
}

#[test]
fn the_volume_slot_admits_the_natives_and_the_three_derivations() {
    use RadarProduct::*;
    let mut admitted = Vec::new();
    let mut refused = Vec::new();
    for product in RadarProduct::all() {
        match volume_slot(*product) {
            Some(slot) => admitted.push((*product, slot)),
            None => refused.push(*product),
        }
    }
    assert_eq!(
        admitted,
        vec![
            (Reflectivity, MomentSlot::Reflectivity),
            (Velocity, MomentSlot::Velocity),
            (SpectrumWidth, MomentSlot::SpectrumWidth),
            (DifferentialPhase, MomentSlot::DifferentialPhase),
            (CorrelationCoefficient, MomentSlot::CorrelationCoefficient),
            (
                DifferentialReflectivity,
                MomentSlot::DifferentialReflectivity
            ),
            (StormRelativeVelocity, MomentSlot::Velocity),
            (SpecificDifferentialPhase, MomentSlot::DifferentialPhase),
            (NormalizedRotation, MomentSlot::Velocity),
        ],
        "the vertical views' product set changed",
    );
    assert_eq!(
        refused,
        vec![
            EchoTops,
            EchoTopsInterpolated,
            VerticallyIntegratedLiquid,
            VilDensity,
            ProbabilityOfSevereHail,
            MaxExpectedHailSize,
            HydrometeorClassification,
            PrecipitationRate,
        ],
        "a product with no per-tilt field must stay refused",
    );
    for product in [
        StormRelativeVelocity,
        SpecificDifferentialPhase,
        NormalizedRotation,
    ] {
        assert!(crate::sampler::samplable(product).is_none());
    }
}

#[test]
fn srv_subtracts_the_override_motion_radial_by_radial() {
    let flow = |az: f64| 25.0 * (az - 180.0).to_radians().cos();
    let scan = scan_with(&move |az, _| (Some(40.0), Some(flow(az)), None, Some(0.99)));

    let speed_kt: f32 = 20.0;
    let speed_ms = f64::from(speed_kt) * 0.514444;
    let direction: f32 = 240.0;
    let prepared = prepare(
        (&scan).into(),
        RadarProduct::StormRelativeVelocity,
        crate::srv::MotionInputs {
            user_override: Some((speed_kt, direction)),
            ..Default::default()
        },
        SITE.0,
        SITE.1,
    )
    .expect("a velocity volume with an override derives");
    let Prepared::Derived(derived) = prepared else {
        panic!("SRV must be derived, never served from the raw scan");
    };

    let mut checked = 0;
    for radial in (0..360usize).step_by(45) {
        let az = radial as f64;
        let raw = decoded(&scan, 0, radial, 40, MomentSlot::Velocity).unwrap();
        let srv = decoded(&derived, 0, radial, 40, MomentSlot::Velocity)
            .expect("the derived field covers the raw field");
        let component = speed_ms * (f64::from(direction) - az).to_radians().cos();
        assert!(
            (srv - (raw + component)).abs() < 0.6,
            "az {az}: srv {srv} != raw {raw} + component {component:.2} \
                 (within one codec step + dealias identity)",
        );
        if component.abs() > 1.0 {
            checked += 1;
            assert!(
                (srv - raw).abs() > 0.5,
                "az {az}: the derived field equals the raw field — the \
                     motion was never subtracted",
            );
        }
    }
    assert!(
        checked >= 4,
        "precondition: the sweep sampled moving azimuths"
    );
}

#[test]
fn each_derived_codec_spans_exactly_the_range_its_product_declares() {
    for (product, lo, hi, why) in [
        (
            RadarProduct::StormRelativeVelocity,
            -63.5,
            63.0,
            "velocity's own span: same units, same resolution",
        ),
        (
            RadarProduct::NormalizedRotation,
            -5.0,
            5.0,
            "unitless, one number with the field's own NROT_LIMIT clamp",
        ),
        (
            RadarProduct::SpecificDifferentialPhase,
            f64::from(kdp::KDP_MIN_DISPLAY),
            f64::from(kdp::KDP_MAX_DISPLAY),
            "the estimator's own display clamp",
        ),
    ] {
        let (scale, offset) = codec(product);
        let decode = |raw: f64| (raw - f64::from(offset)) / f64::from(scale);
        assert!(
            (decode(2.0) - lo).abs() < 1e-3,
            "{}: raw code 2 means {}, not the declared {lo} ({why})",
            product.code(),
            decode(2.0),
        );
        assert!(
            (decode(255.0) - hi).abs() < 1e-3,
            "{}: raw code 255 means {}, not the declared {hi} ({why})",
            product.code(),
            decode(255.0),
        );
        assert!(
            decode(0.0) < lo,
            "{}: the no-data code 0 decodes to {}, inside the span",
            product.code(),
            decode(0.0),
        );
    }
}

#[test]
fn nrot_decodes_onto_the_reference_lattice() {
    /// GR2Analyst's quantum: 253 gaps across its own ±5.
    const STEP: f64 = 10.0 / 253.0;
    /// The ends its Product Details panel reports.
    const CLAMP: f64 = 5.0;

    let (scale, offset) = codec(RadarProduct::NormalizedRotation);
    let decode = |raw: u8| (f64::from(raw) - f64::from(offset)) / f64::from(scale);
    let levels: Vec<f64> = (2..=255u8).map(decode).collect();
    assert_eq!(
        levels.len(),
        254,
        "254 codes carry a value; 0 is no-data and 1 is reserved",
    );

    let measured = (levels[253] - levels[0]) / 253.0;
    assert!(
        (measured - STEP).abs() < 1e-6,
        "NROT decodes on a {measured} lattice; the reference reports on {STEP}",
    );
    for pair in levels.windows(2) {
        assert!(
            (pair[1] - pair[0] - STEP).abs() < 1e-6,
            "the lattice is not uniform: {} to {}",
            pair[0],
            pair[1],
        );
    }

    let nearest = levels
        .iter()
        .copied()
        .fold(f64::INFINITY, |acc, v| acc.min(v.abs()));
    assert!(
        (nearest - STEP / 2.0).abs() < 1e-6,
        "the level nearest zero is {nearest}, not the reference's half-step {}",
        STEP / 2.0,
    );

    // And the ends are the reference's ends, reached with no clamp engaged:
    // `synth_sweep`'s `clamp(2, 255)` cannot fire on a value the field can
    // produce, because the field's own clamp is where this span stops.
    assert!(
        (levels[0] + CLAMP).abs() < 1e-3,
        "raw code 2 means {}, not the reference's −5",
        levels[0],
    );
    assert!(
        (levels[253] - CLAMP).abs() < 1e-3,
        "raw code 255 means {}, not the reference's +5",
        levels[253],
    );
    let encode = |v: f64| (v * f64::from(scale) + f64::from(offset)).round() as i64;
    assert_eq!(encode(-CLAMP), 2, "−5 must reach raw 2 without saturating");
    assert_eq!(
        encode(CLAMP),
        255,
        "+5 must reach raw 255 without saturating"
    );
}

#[test]
fn srv_with_no_motion_vector_refuses() {
    let scan = scan_with(&|_, _| (Some(40.0), None, None, Some(0.99)));
    assert!(
        prepare(
            (&scan).into(),
            RadarProduct::StormRelativeVelocity,
            crate::srv::MotionInputs::default(),
            SITE.0,
            SITE.1,
        )
        .is_none()
    );
}

#[test]
fn nrot_is_rotation_not_relabelled_velocity() {
    let scan = scan_with(&|_, _| (Some(40.0), Some(15.0), None, Some(0.99)));
    let prepared = prepare(
        (&scan).into(),
        RadarProduct::NormalizedRotation,
        crate::srv::MotionInputs::default(),
        SITE.0,
        SITE.1,
    )
    .expect("a velocity volume derives");
    let Prepared::Derived(derived) = prepared else {
        panic!("NROT must be derived, never served from the raw scan");
    };
    let mut seen = 0;
    for radial in (0..360).step_by(20) {
        for gate in (20..200).step_by(30) {
            if let Some(v) = decoded(&derived, 0, radial, gate, MomentSlot::Velocity) {
                seen += 1;
                assert!(
                    v.abs() < 1.0,
                    "({radial},{gate}): a shear-free field read NROT {v} — \
                         the raw velocity leaked through",
                );
            }
        }
    }
    // `seen` may honestly be 0: the pipeline censors a zero-information field to no-
    // data, which is also not-velocity.
    let _ = seen;
}

#[test]
fn a_derived_section_slices_the_derived_field() {
    let flow = |az: f64| 25.0 * (az - 180.0).to_radians().cos();
    let scan = scan_with(&move |az, _| (Some(40.0), Some(flow(az)), None, Some(0.99)));
    let site = (35.33306, -97.2775);
    let req = |product| crate::xsect::SectionRequest {
        start: (site.0, site.1 - 0.4),
        end: (site.0, site.1 + 0.4),
        top_km_msl: None,
        product,
    };
    let bv = crate::xsect::render_section(
        &scan,
        &req(RadarProduct::Velocity),
        site.0,
        site.1,
        crate::srv::MotionInputs::default(),
    )
    .expect("the velocity section renders");
    let srv = crate::xsect::render_section(
        &scan,
        &req(RadarProduct::StormRelativeVelocity),
        site.0,
        site.1,
        crate::srv::MotionInputs {
            user_override: Some((20.0, 240.0)),
            ..Default::default()
        },
    )
    .expect("the SRV section renders");

    let differing = bv
        .values()
        .iter()
        .zip(srv.values())
        .filter(|(b, s)| b.is_finite() && s.is_finite() && (**b - **s).abs() > 1.0)
        .count();
    assert!(
        differing > 100,
        "only {differing} samples differ between the BV and SRV sections \
             — the section sampled the raw field under the SRV label",
    );
}

#[test]
fn a_derived_voxel_grid_resamples_the_derived_field() {
    use crate::voxel::{HalfExtentKm, VoxelRequest, VoxelShape, build_voxels_with_motion};
    let flow = |az: f64| 25.0 * (az - 180.0).to_radians().cos();
    let scan = scan_with(&move |az, _| (Some(40.0), Some(flow(az)), None, Some(0.99)));
    let site = (35.33306, -97.2775);
    let req = |product| VoxelRequest {
        centre: site,
        half_extent_km: Some(HalfExtentKm::square(30.0)),
        base_km_msl: 0.0,
        top_km_msl: 4.0,
        product,
        shape: VoxelShape {
            nx: 32,
            ny: 32,
            nz: 8,
        },
        values_wanted: true,
    };
    let bv = build_voxels_with_motion(
        &scan,
        &req(RadarProduct::Velocity),
        site.0,
        site.1,
        crate::srv::MotionInputs::default(),
    )
    .expect("the velocity grid builds");
    let srv = build_voxels_with_motion(
        &scan,
        &req(RadarProduct::StormRelativeVelocity),
        site.0,
        site.1,
        crate::srv::MotionInputs {
            user_override: Some((20.0, 240.0)),
            ..Default::default()
        },
    )
    .expect("the SRV grid builds");

    let mut differing = 0;
    for z in 0..8 {
        for y in 0..32 {
            for x in 0..32 {
                let (Some(b), Some(s)) = (bv.value_at(x, y, z), srv.value_at(x, y, z)) else {
                    continue;
                };
                if b.is_finite() && s.is_finite() && (b - s).abs() > 1.0 {
                    differing += 1;
                }
            }
        }
    }
    assert!(
        differing > 100,
        "only {differing} cells differ between the BV and SRV grids — the \
             grid resampled the raw field under the SRV label",
    );

    let nrot = build_voxels_with_motion(
        &scan,
        &req(RadarProduct::NormalizedRotation),
        site.0,
        site.1,
        crate::srv::MotionInputs::default(),
    )
    .expect("the NROT grid builds");
    assert_eq!(srv.value_range().1, 63.5, "SRV rides velocity's ramp");
    assert_eq!(
        nrot.value_range().1,
        5.0,
        "NROT carries its own ±5 unitless ramp",
    );
    assert!(
        (f64::from(nrot.value_range().0) - (-5.0 - 10.0 / 254.0)).abs() < 1e-3,
        "NROT's index 0 sits one step under −5",
    );

    let restored = crate::voxel::VoxelGrid::from_bytes(&srv.to_bytes())
        .expect("a derived grid round-trips the wire");
    assert_eq!(restored.value_range(), srv.value_range());
    assert_eq!(restored.lut(), srv.lut());
}

#[test]
fn kdp_is_the_phase_derivative_not_relabelled_phase() {
    let scan = scan_with(&|_, slant| (Some(45.0), None, Some(10.0 + 1.0 * slant), Some(0.99)));
    let prepared = prepare(
        (&scan).into(),
        RadarProduct::SpecificDifferentialPhase,
        crate::srv::MotionInputs::default(),
        SITE.0,
        SITE.1,
    )
    .expect("a ΦDP volume derives");
    let Prepared::Derived(derived) = prepared else {
        panic!("KDP must be derived, never served from the raw scan");
    };
    let mut values = Vec::new();
    let radial_count = derived.sweeps()[0].radials().len();
    for radial in (0..radial_count).step_by(radial_count / 12) {
        for gate in [60, 100, 140] {
            if let Some(v) = decoded(&derived, 0, radial, gate, MomentSlot::DifferentialPhase) {
                values.push(v);
            }
        }
    }
    assert!(
        !values.is_empty(),
        "precondition: the estimator produced values on a clean ramp",
    );
    for &v in &values {
        assert!(
            (0.1..1.2).contains(&v),
            "a 1 °/km ΦDP ramp must derive to ≈0.5 °/km, not {v} — raw \
                 phase would read tens of degrees",
        );
    }
}

#[test]
fn a_derived_sweep_keeps_the_clock_of_the_tilt_it_was_computed_from() {
    const T0: i64 = 1_760_000_000_000;
    let fields = &|az: f64, slant: f64| {
        (
            Some(30.0),
            Some(if az < 180.0 { 20.0 } else { -20.0 }),
            Some(slant * 2.0),
            Some(0.98),
        )
    };
    let scan = Scan::new(
        vcp(&[0.5, 1.5]),
        vec![
            sweep_with_clock(T0, 1, 0.53, 360, 240, fields),
            sweep_with_clock(T0 + 60_000, 2, 1.47, 360, 240, fields),
        ],
    );

    // precondition: the source really is clocked, and not all alike.
    let source_clocks: Vec<i64> = scan
        .sweeps()
        .iter()
        .map(|s| crate::render_input::sweep_collected_ms(s.radials()))
        .collect();
    assert_eq!(
        source_clocks,
        vec![T0, T0 + 60_000],
        "precondition: the fixture's tilts are not clocked a minute apart",
    );

    for product in [
        RadarProduct::StormRelativeVelocity,
        RadarProduct::NormalizedRotation,
        RadarProduct::SpecificDifferentialPhase,
    ] {
        let prepared = prepare(
            (&scan).into(),
            product,
            crate::srv::MotionInputs {
                user_override: Some((30.0, 240.0)),
                ..Default::default()
            },
            SITE.0,
            SITE.1,
        )
        .unwrap_or_else(|| panic!("{product:?} derives from this fixture"));
        let derived = match &prepared {
            Prepared::Derived(scan) => scan,
            Prepared::Native(_) => panic!("{product:?} took the native path"),
        };
        let clocks: Vec<i64> = derived
            .sweeps()
            .iter()
            .map(|s| crate::render_input::sweep_collected_ms(s.radials()))
            .collect();
        assert_eq!(
            clocks, source_clocks,
            "{product:?}: the derived tilts lost the clocks of the tilts they \
             were computed from, so every rung age a derived section reports \
             is missing or wrong",
        );
    }
}

fn arc_sweep(n: usize, step_deg: f32) -> Sweep {
    let radials = (0..n)
        .map(|i| {
            let az = i as f32 * step_deg;
            let bytes: Vec<u8> = (0..240)
                .map(|_| ((20.0 * f64::from(az).to_radians().cos()) * 2.0 + 129.0) as u8)
                .collect();
            Radial::new(
                0,
                i as u16,
                az,
                // Deliberately 1.0° in both fixtures, and deliberately not what
                // the sector's rows are apart by: what a row of a *derived*
                // grid stands for is a property of that grid.
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                None,
                Some(MomentData::from_fixed_point(
                    240,
                    FIRST_GATE_M,
                    GATE_M,
                    8,
                    2.0,
                    129.0,
                    bytes,
                )),
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

#[test]
fn a_derived_radial_declares_the_step_its_own_grid_sits_at() {
    for (n, step, expected) in [(360usize, 1.0f32, 1.0f32), (72, 0.5, 0.5)] {
        let scan = Scan::new(vcp(&[0.5]), vec![arc_sweep(n, step)]);
        let prepared = prepare(
            (&scan).into(),
            RadarProduct::NormalizedRotation,
            crate::srv::MotionInputs::default(),
            SITE.0,
            SITE.1,
        )
        .expect("a velocity volume derives NROT");
        let Prepared::Derived(derived) = prepared else {
            panic!("NROT must be derived, never served from the raw scan");
        };
        let radials = derived.sweeps()[0].radials();
        assert_eq!(radials.len(), n);
        for radial in radials {
            assert_eq!(radial.azimuth_spacing_degrees(), expected, "{n} rows");
        }
    }
}

#[test]
fn a_tilt_whose_leading_radial_lost_its_phase_is_still_derived() {
    let clean = scan_with(&|_, slant| (Some(45.0), None, Some(10.0 + slant), Some(0.99)));
    let maimed = Scan::new(
        clean.coverage_pattern().clone(),
        clean
            .sweeps()
            .iter()
            .map(|sweep| {
                let radials: Vec<Radial> = sweep
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
                            r.velocity().cloned(),
                            r.spectrum_width().cloned(),
                            r.differential_reflectivity().cloned(),
                            (i > 0).then(|| r.differential_phase().cloned()).flatten(),
                            r.correlation_coefficient().cloned(),
                            r.clutter_filter_power().cloned(),
                        )
                    })
                    .collect();
                Sweep::new(sweep.elevation_number(), radials)
            })
            .collect(),
    );
    assert!(
        maimed.sweeps()[0].radials()[0]
            .differential_phase()
            .is_none(),
        "the fixture must be blank in front",
    );

    let prepared = prepare(
        (&maimed).into(),
        RadarProduct::SpecificDifferentialPhase,
        crate::srv::MotionInputs::default(),
        SITE.0,
        SITE.1,
    )
    .expect("a \u{3a6}DP volume derives");
    let Prepared::Derived(derived) = prepared else {
        panic!("KDP must be derived, never served from the raw scan");
    };
    assert_eq!(
        derived.sweeps().len(),
        clean.sweeps().len(),
        "one blank leading radial dropped a tilt from the KDP volume",
    );
}

// ---------------------------------------------------------------------------
// The derivation memo (WO-E4.8). These tests share one process-global memo
// with every other test in this binary, so each uses its own clock epoch and
// site, and they serialize against each other — a `memo_clear` in one must
// not race a hit-check in another. Tests elsewhere in the crate cannot be
// polluted BY the memo (a hit is byte-identical by the determinism gate
// below), and cannot pollute these keys (distinct epoch + site).
// ---------------------------------------------------------------------------

/// The memo tests' mutual exclusion. Only they take it.
static MEMO_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn clocked_scan_with(t0: i64, fields: Fields<'_>) -> Scan {
    Scan::new(
        vcp(&[0.5, 1.5]),
        vec![
            sweep_with_clock(t0, 1, 0.53, 360, 240, fields),
            sweep_with_clock(t0 + 60_000, 2, 1.47, 360, 240, fields),
        ],
    )
}

fn derived_fingerprint(scan: &Scan, slot: MomentSlot) -> Vec<(usize, usize, usize, Option<f64>)> {
    let mut out = Vec::new();
    for (s, sweep) in scan.sweeps().iter().enumerate() {
        for (r, _radial) in sweep.radials().iter().enumerate() {
            for g in 0..240 {
                out.push((s, r, g, decoded(scan, s, r, g, slot)));
            }
        }
    }
    out
}

#[test]
fn a_memoized_derivation_is_the_bytes_a_fresh_one_computes() {
    let _serial = MEMO_TEST_LOCK.lock().unwrap();
    let scan = clocked_scan_with(1_770_000_000_000, &|az, _| {
        (
            Some(30.0),
            Some(if az < 180.0 { 18.0 } else { -18.0 }),
            None,
            Some(0.98),
        )
    });
    let site = (36.0, -96.0);
    let run = || match prepare(
        (&scan).into(),
        RadarProduct::NormalizedRotation,
        crate::srv::MotionInputs::default(),
        site.0,
        site.1,
    )
    .expect("a clocked velocity volume derives NROT")
    {
        Prepared::Derived(derived) => derived,
        Prepared::Native(_) => panic!("NROT must be derived"),
    };

    memo_clear();
    let first = run();
    let second = run();
    assert!(
        Arc::ptr_eq(&first, &second),
        "the second same-key derivation was recomputed — the memo missed \
         (key built differently across two identical calls, or the insert \
         never happened)",
    );

    memo_clear();
    let fresh = run();
    assert!(
        !Arc::ptr_eq(&first, &fresh),
        "precondition: after memo_clear the derivation must actually rerun",
    );
    assert_eq!(
        derived_fingerprint(&first, MomentSlot::Velocity),
        derived_fingerprint(&fresh, MomentSlot::Velocity),
        "a memoized derivation served different bytes than a fresh compute",
    );
}

#[test]
fn evicting_a_volume_drops_its_memo_entry_and_keeps_the_live_ones() {
    let _serial = MEMO_TEST_LOCK.lock().unwrap();
    let scan = clocked_scan_with(1_771_000_000_000, &|az, _| {
        (
            Some(30.0),
            Some(if az < 180.0 { 12.0 } else { -12.0 }),
            None,
            Some(0.98),
        )
    });
    let site = (37.0, -95.0);
    let run = || match prepare(
        (&scan).into(),
        RadarProduct::NormalizedRotation,
        crate::srv::MotionInputs::default(),
        site.0,
        site.1,
    )
    .expect("a clocked velocity volume derives NROT")
    {
        Prepared::Derived(derived) => derived,
        Prepared::Native(_) => panic!("NROT must be derived"),
    };

    memo_clear();
    let first = run();
    retain_volumes([&scan]);
    let kept = run();
    assert!(
        Arc::ptr_eq(&first, &kept),
        "retain_volumes dropped an entry whose volume is still live",
    );
    retain_volumes([]);
    let recomputed = run();
    assert!(
        !Arc::ptr_eq(&first, &recomputed),
        "retain_volumes kept an entry whose volume was evicted",
    );
}

#[test]
fn an_unclocked_volume_is_never_memoized() {
    let _serial = MEMO_TEST_LOCK.lock().unwrap();
    let scan = scan_with(&|az, _| {
        (
            Some(30.0),
            Some(if az < 180.0 { 9.0 } else { -9.0 }),
            None,
            Some(0.98),
        )
    });
    let run = || match prepare(
        (&scan).into(),
        RadarProduct::NormalizedRotation,
        crate::srv::MotionInputs::default(),
        38.0,
        -94.0,
    )
    .expect("a velocity volume derives NROT")
    {
        Prepared::Derived(derived) => derived,
        Prepared::Native(_) => panic!("NROT must be derived"),
    };
    let first = run();
    let second = run();
    assert!(
        !Arc::ptr_eq(&first, &second),
        "an unclocked volume was served from the memo — it has no identity \
         to be cached under",
    );
}
