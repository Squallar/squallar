use super::*;
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, PulseWidth, VolumeCoveragePattern, WaveformType,
};

const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;

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

/// One sweep carrying reflectivity, velocity, ΦDP and ρHV through their
/// real codecs, over `fields(az, slant_km) -> (refl, vel, phi, rho)`.
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

/// [`sweep_with`] with a clock on every radial, milliseconds since the Unix
/// epoch. `0` is the decoder's own "no timestamp" value, which is what every
/// fixture that does not care about time passes.
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

/// Decode one gate of the given slot from a scan's sweep, m/s (or the
/// slot's units), `None` for no data.
fn decoded(scan: &Scan, sweep: usize, radial: usize, gate: usize, slot: MomentSlot) -> Option<f64> {
    use nexrad_model::data::MomentValue;
    let radial = &scan.sweeps()[sweep].radials()[radial];
    let moment = slot.read(radial)?;
    match moment.iter().nth(gate)? {
        MomentValue::Value(v) => Some(f64::from(v)),
        _ => None,
    }
}

/// The volume-slot table: natives sample, the three derivations sample
/// through their source slot, everything else refuses.
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
    // And none of the derived three may pass the raw-scan gate: that is
    // the door `for_derived` exists to keep shut.
    for product in [
        StormRelativeVelocity,
        SpecificDifferentialPhase,
        NormalizedRotation,
    ] {
        assert!(crate::sampler::samplable(product).is_none());
    }
}

/// SRV honours the user's storm motion vector, radial by radial: the
/// derived field minus the raw field is exactly the subtracted motion
/// component, which is `apply_storm_motion`'s contract carried through
/// the whole prepare → synthesize → decode round trip.
///
/// The fixture wind is a smooth 25 m/s southerly flow (no folds, so the
/// dealias pass is the identity) and the override is deliberately not the
/// flow — a fixture whose override matched its wind would hide a sign
/// error in the correction.
#[test]
fn srv_subtracts_the_override_motion_radial_by_radial() {
    // Radial velocity of a uniform wind FROM 180° at 25 m/s.
    let flow = |az: f64| 25.0 * (az - 180.0).to_radians().cos();
    let scan = scan_with(&move |az, _| (Some(40.0), Some(flow(az)), None, Some(0.99)));

    let speed_kt: f32 = 20.0;
    let speed_ms = f64::from(speed_kt) * 0.514444;
    let direction: f32 = 240.0;
    let prepared = prepare(
        (&scan).into(),
        RadarProduct::StormRelativeVelocity,
        Some((speed_kt, direction)),
        None,
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

/// Each derived codec spans exactly the range its product declares —
/// asserted against the span, not against the encoder that wrote it.
///
/// Every other test of a derived field in this module decodes through the
/// same `codec()` that `synth_sweep` encoded with, so the pair is
/// self-inverting: swapping NROT's codec for velocity's (2, 129) — a
/// sixteen-fold coarsening, and a different zero — changes the raw bytes
/// and the decode together and the whole workspace stays green. The field
/// would then be quantised to 0.5 unitless per code, against a palette
/// whose entire weak class is 0.75 wide.
///
/// This pin has no encoder in it. The codec is a claim about what raw
/// codes 2 and 255 *mean*, and the claim is checked against the numbers
/// the module doc and `voxel::data_levels_for` state independently. The
/// second half of the loop — that the voxel ramp agrees with these spans
/// — is `a_derived_voxel_grid_resamples_the_derived_field`'s `value_range`
/// assertions.
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
        // And the no-data code is outside the span in the direction the
        // ramp puts it, so a zero byte can never decode to a value.
        assert!(
            decode(0.0) < lo,
            "{}: the no-data code 0 decodes to {}, inside the span",
            product.code(),
            decode(0.0),
        );
    }
}

/// NROT's decoded levels **are** GR2Analyst's lattice, not merely near it.
///
/// **Non-circular by construction.** Every expectation here is the
/// reference's own number, and the lattice under test is read back out of
/// the decode rather than recomputed from `codec`'s constants — so widening
/// or narrowing that span fails this on the reference's quantum. The sibling
/// above cannot catch that: it checks a span against the same span.
///
/// The reference's numbers come from the `campaign-harness` NROT record.
/// Its hovered readouts pool to **14 780** values, and a two-parameter fit
/// over them admits exactly one lattice — spacing **10/253 = 0.0395257**,
/// offset **half a step**, so zero is deliberately *not* a point on it —
/// with the denominators 252 and 254 both infeasible against those same
/// readings. Its ends are what GR2Analyst's own Product Details panel
/// reports hovered on a KCRP cut: minimum **−5.00**, maximum **+5.00**.
///
/// The half-step is the part worth pinning. The record's prose quotes the
/// lattice as `n·0.03950 + 0.0210`, and that offset breaks 6.6% of the
/// readings it was drawn from; half a step breaks none of them.
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

    // Spacing, measured off the decode rather than assumed of it.
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

    // The offset. Zero is not a level, and the two straddling it sit half a
    // step out — the half of the reference's lattice its own prose got wrong.
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

/// SRV with neither an override nor a usable wind fit refuses: base
/// velocity under a storm-relative label is the failure the refusal
/// exists to prevent.
#[test]
fn srv_with_no_motion_vector_refuses() {
    // No velocity anywhere: no wind fit, and (no override) no vector.
    let scan = scan_with(&|_, _| (Some(40.0), None, None, Some(0.99)));
    assert!(
        prepare(
            (&scan).into(),
            RadarProduct::StormRelativeVelocity,
            None,
            None
        )
        .is_none()
    );
}

/// NROT is the rotation pipeline's output, not relabelled velocity: on a
/// uniform 15 m/s field — every gate moving, zero shear — the derived
/// field must read no-data or near-zero everywhere, never 15.
#[test]
fn nrot_is_rotation_not_relabelled_velocity() {
    let scan = scan_with(&|_, _| (Some(40.0), Some(15.0), None, Some(0.99)));
    let prepared = prepare((&scan).into(), RadarProduct::NormalizedRotation, None, None)
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
    // `seen` may honestly be 0: the pipeline censors a zero-information
    // field to no-data, which is also not-velocity. The assertion above
    // is the pin; this is just the record of which way it went.
    let _ = seen;
}

/// A cross-section of a derived product slices the derived field — the
/// integrated pin over `xsect::render_section`'s derivation seam. An SRV
/// section and a BV section of the same volume must differ wherever the
/// motion component is non-zero; a seam that forgot to derive (sampled
/// the raw scan under the SRV label) makes them equal, which is the
/// "looks right, different field" failure the sampler's refusal exists
/// to prevent.
#[test]
fn a_derived_section_slices_the_derived_field() {
    let flow = |az: f64| 25.0 * (az - 180.0).to_radians().cos();
    let scan = scan_with(&move |az, _| (Some(40.0), Some(flow(az)), None, Some(0.99)));
    // KTLX; a west–east line through the site so azimuths span the flow.
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
        None,
        None,
    )
    .expect("the velocity section renders");
    let srv = crate::xsect::render_section(
        &scan,
        &req(RadarProduct::StormRelativeVelocity),
        site.0,
        site.1,
        Some((20.0, 240.0)),
        None,
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

/// A voxel grid of a derived product resamples the derived field — the
/// same pin as the section one, over `voxel::build_voxels_with_motion`'s
/// seam — and the derived grids carry their own value ranges, not their
/// source slot's.
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
        None,
        None,
    )
    .expect("the velocity grid builds");
    let srv = build_voxels_with_motion(
        &scan,
        &req(RadarProduct::StormRelativeVelocity),
        site.0,
        site.1,
        Some((20.0, 240.0)),
        None,
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

    // The derived ranges: SRV shares velocity's, NROT and KDP carry
    // their own — pinned here as the literals `derive`'s codecs match.
    let nrot = build_voxels_with_motion(
        &scan,
        &req(RadarProduct::NormalizedRotation),
        site.0,
        site.1,
        None,
        None,
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

    // And a derived grid survives its own wire form: the far end
    // re-derives the range and the table from the product and must agree.
    let restored = crate::voxel::VoxelGrid::from_bytes(&srv.to_bytes())
        .expect("a derived grid round-trips the wire");
    assert_eq!(restored.value_range(), srv.value_range());
    assert_eq!(restored.lut(), srv.lut());
}

/// KDP is the phase derivative, not relabelled ΦDP: a linear 1 °/km ramp
/// must derive to ~0.5 °/km (KDP = dΦ/2dr), never to the ~30–70° the raw
/// phase reads.
#[test]
fn kdp_is_the_phase_derivative_not_relabelled_phase() {
    let scan = scan_with(&|_, slant| (Some(45.0), None, Some(10.0 + 1.0 * slant), Some(0.99)));
    let prepared = prepare(
        (&scan).into(),
        RadarProduct::SpecificDifferentialPhase,
        None,
        None,
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

/// **A derivation keeps the tilt's clock.**
///
/// `synth_sweep` builds fresh radials for a tilt the radar already flew, and it
/// used to stamp `0` on their collection timestamps because nothing read one.
/// Now something does: a section pane reports how old the rung under the
/// pointer is, and that age is read off these radials.
///
/// Left unfixed the failure would have been invisible in exactly the way this
/// campaign keeps paying for — every *native* moment's section would date its
/// rungs correctly while storm-relative velocity, NROT and KDP silently could
/// not, with no error, no warning and nothing in the picture to point at.
///
/// The two tilts carry clocks a minute apart, so a derivation that stamped one
/// constant across the volume fails as well as one that stamped zero.
#[test]
fn a_derived_sweep_keeps_the_clock_of_the_tilt_it_was_computed_from() {
    const T0: i64 = 1_760_000_000_000;
    // A rotational couplet's worth of velocity, plus the phase and correlation
    // KDP needs, so all three derivations have something to run on.
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
        // A motion vector for SRV; the other two read none. The volume states
        // no Nyquist limit, so the dealias each derivation runs estimates one
        // — which is what this fixture had before the declaration crossed the
        // boundary, and the clocks it checks do not depend on it.
        let prepared = prepare((&scan).into(), product, Some((30.0, 240.0)), None)
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

/// One velocity-carrying sweep of `n` radials `step_deg` apart from due north
/// — a sector whenever `n · step_deg` stops short of the circle.
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

/// A synthetic radial declares the arc its own grid row covers. On a rotation
/// that is `360 / rows` to the bit — which is what every WSR-88D cut is — and
/// on a 36° sector of 0.5° rows it is 0.5°, not the 5° `360 / 72` claims.
#[test]
fn a_derived_radial_declares_the_step_its_own_grid_sits_at() {
    for (n, step, expected) in [(360usize, 1.0f32, 1.0f32), (72, 0.5, 0.5)] {
        let scan = Scan::new(vcp(&[0.5]), vec![arc_sweep(n, step)]);
        let prepared = prepare((&scan).into(), RadarProduct::NormalizedRotation, None, None)
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

/// A tilt whose leading radial lost its ΦDP is still derived.
///
/// The guard admitted a sweep on `radials.first().differential_phase()` while
/// [`crate::kdp::compute_kdp`] — the estimator it then called — reads every
/// radial. The two disagreed, so one blank leading radial refused a cut the
/// estimator was perfectly willing to derive, and the tilt vanished from the
/// KDP volume with no warning: the same guard-versus-extractor split
/// `crate::velocity` closed for the wind fit.
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
        None,
        None,
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
