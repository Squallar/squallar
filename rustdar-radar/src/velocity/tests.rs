use super::*;
use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, VolumeCoveragePattern};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const N_RADIALS: usize = 360;
const N_GATES: usize = 240;

/// [`profile_bits`] of [`vad_volume`]'s `(12, -5)` volume, hashed.
const VAD_DIGEST: u64 = 0x85c2_e75c_62bf_87d8;

/// One velocity sweep carrying an exact VAD signature.
fn vad_sweep(
    elevation_number: u8,
    elevation_deg: f32,
    az0: f64,
    (u, v): (f64, f64),
    blank_first: bool,
) -> Sweep {
    let cos_el = f64::from(elevation_deg).to_radians().cos();
    let spacing = 360.0 / N_RADIALS as f32;
    let radials = (0..N_RADIALS)
        .map(|i| {
            let az = (az0 + i as f64 * f64::from(spacing)).rem_euclid(360.0);
            let (s, c) = az.to_radians().sin_cos();
            let vr = (u * s + v * c) * cos_el;
            let byte = ((vr * 2.0 + 129.0).round() as i64).clamp(2, 255) as u8;
            let velocity = (!(blank_first && i == 0)).then(|| {
                MomentData::from_fixed_point(
                    N_GATES as u16,
                    2125,
                    250,
                    8,
                    2.0,
                    129.0,
                    vec![byte; N_GATES],
                )
            });
            Radial::new(
                0,
                i as u16,
                az as f32,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                None,
                velocity,
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

/// [`vad_sweep`]'s cut with the radial velocity **wrapped** into
/// `[-nyquist, nyquist)` before it reaches the codec — a real folded sweep.
fn vad_sweep_folded(
    elevation_number: u8,
    elevation_deg: f32,
    az0: f64,
    (u, v): (f64, f64),
    nyquist: f64,
) -> Sweep {
    let cos_el = f64::from(elevation_deg).to_radians().cos();
    let spacing = 360.0 / N_RADIALS as f32;
    let radials = (0..N_RADIALS)
        .map(|i| {
            let az = (az0 + i as f64 * f64::from(spacing)).rem_euclid(360.0);
            let (s, c) = az.to_radians().sin_cos();
            let vr = (u * s + v * c) * cos_el;
            let folded = (vr + nyquist).rem_euclid(2.0 * nyquist) - nyquist;
            let byte = ((folded * 2.0 + 129.0).round() as i64).clamp(2, 255) as u8;
            Radial::new(
                0,
                i as u16,
                az as f32,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                None,
                Some(MomentData::from_fixed_point(
                    N_GATES as u16,
                    2125,
                    250,
                    8,
                    2.0,
                    129.0,
                    vec![byte; N_GATES],
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

/// [`vad_volume`]'s three cuts, folded at `nyquist`.
fn vad_volume_folded(wind: (f64, f64), nyquist: f64) -> Scan {
    Scan::new(
        vcp(),
        [(1u8, 0.53f32, 0.0f64), (2, 1.47, 97.3), (3, 2.42, 211.8)]
            .into_iter()
            .map(|(n, el, az0)| vad_sweep_folded(n, el, az0, wind, nyquist))
            .collect(),
    )
}

/// The one-pass fit, for arms that need the thing the second pass improves on.
fn one_pass_of(scan: &Scan) -> Option<WindProfile> {
    let mut builder = WindProfileBuilder::new();
    for tilt in tilts(scan) {
        builder.add_sweep(&tilt.grid.sweep(None), tilt.elevation_deg);
    }
    builder.finish()
}

/// Refitting on the dealiased field lands nearer the planted wind than
/// fitting on the folded one.
#[test]
fn the_refit_on_the_dealiased_field_lands_nearer_than_the_fit_on_the_folded_one() {
    const NYQUIST: f64 = 25.0;
    for wind in [(26.0, 0.0), (28.0, 0.0)] {
        let folded = vad_volume_folded(wind, NYQUIST);
        let at = |p: &WindProfile| p.wind_at_km(1.05).expect("the 1.05 km layer fits");

        let one = at(&one_pass_of(&folded).expect("the folded volume fits one pass"));
        let two = at(&volume_wind_profile(&folded).expect("and two"));

        let err = |(u, v): (f64, f64)| (u - wind.0).hypot(v - wind.1);
        // Both are close — the trim is most of the way there on a clean field.
        assert!(
            err(one) < 0.1 && err(two) < 0.1,
            "{wind:?}: {one:?} {two:?}"
        );
        // And the refit is closer, which is the property under test.
        assert!(
            err(two) < err(one),
            "{wind:?}: refitting on the dealiased field left {:.4} m/s of \
             error against the folded field's {:.4} — the second pass must \
             not be the worse of the two",
            err(two),
            err(one),
        );
    }
}

/// A volume with nothing to unfold is not moved by unfolding it.
#[test]
fn a_volume_with_nothing_to_unfold_fits_the_same_wind_twice() {
    let scan = vad_volume((12.0, -5.0));
    let one = one_pass_of(&scan).expect("three cuts fit");
    let two = volume_wind_profile(&scan).expect("three cuts fit twice");
    assert_eq!(
        profile_bits(&one),
        profile_bits(&two),
        "an unfolded volume must fit the same profile in one pass and two",
    );
}

fn vcp() -> VolumeCoveragePattern {
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

/// Three [`vad_sweep`] cuts of one wind, each opening at its own azimuth and
/// each reaching its own height, so the layers are pooled from three tilts
/// rather than one — the arrangement the module docs say pooling is for.
fn vad_volume(wind: (f64, f64)) -> Scan {
    Scan::new(
        vcp(),
        [(1u8, 0.53f32, 0.0f64), (2, 1.47, 97.3), (3, 2.42, 211.8)]
            .into_iter()
            .map(|(n, el, az0)| vad_sweep(n, el, az0, wind, false))
            .collect(),
    )
}

/// The wind at every one of the profile's forty layer centres, as raw bits.
fn profile_bits(profile: &WindProfile) -> Vec<Option<(u64, u64)>> {
    (0..40)
        .map(|l| {
            let h = (l as f64 + 0.5) * WindProfile::LAYER_KM;
            profile
                .wind_at_km(h)
                .map(|(u, v)| (u.to_bits(), v.to_bits()))
        })
        .collect()
}

fn digest(profile: &WindProfile) -> u64 {
    let mut h = DefaultHasher::new();
    profile_bits(profile).hash(&mut h);
    h.finish()
}

/// Unifying the three drivers moved nobody's numbers.
#[test]
fn the_wind_fit_still_returns_the_number_the_three_drivers_did() {
    let profile = volume_wind_profile(&vad_volume((12.0, -5.0))).expect("three cuts fit");
    assert_eq!(digest(&profile), VAD_DIGEST, "the fitted profile moved");
    for (u, v) in profile_bits(&profile)
        .iter()
        .filter_map(|b| b.map(|(u, v)| (f64::from_bits(u), f64::from_bits(v))))
    {
        assert!(
            (u - 12.0).abs() < 0.05 && (v + 5.0).abs() < 0.05,
            "fitted ({u:.4}, {v:.4}), planted (12, -5)",
        );
    }
}

/// Streaming the tilts and holding them are the same fit.
#[test]
fn the_streamed_fit_and_the_held_fit_are_one_fit() {
    let scan = vad_volume((12.0, -5.0));
    let streamed = volume_wind_profile(&scan).expect("three cuts fit");
    let held: Vec<VelocityTilt<'_>> = tilts(&scan).collect();
    let lent = wind_profile_of(&held).expect("three cuts fit");
    assert_eq!(profile_bits(&streamed), profile_bits(&lent));
}

/// A tilt whose first radial lost its velocity moment is still a velocity
/// tilt, and is still fitted.
#[test]
fn a_tilt_whose_first_radial_lost_velocity_still_fits() {
    let wind = (12.0, -5.0);
    let maimed = Scan::new(
        vcp(),
        [(1u8, 0.53f32, 0.0f64), (2, 1.47, 97.3), (3, 2.42, 211.8)]
            .into_iter()
            .map(|(n, el, az0)| vad_sweep(n, el, az0, wind, true))
            .collect(),
    );
    assert_eq!(
        tilts(&maimed).count(),
        3,
        "every cut still carries velocity"
    );
    let profile = volume_wind_profile(&maimed).expect("359 of 360 radials is a VAD cut");
    for (u, v) in profile_bits(&profile)
        .iter()
        .filter_map(|b| b.map(|(u, v)| (f64::from_bits(u), f64::from_bits(v))))
    {
        assert!(
            (u - 12.0).abs() < 0.05 && (v + 5.0).abs() < 0.05,
            "fitted ({u:.4}, {v:.4}), planted (12, -5)",
        );
    }

    let intact = volume_wind_profile(&vad_volume(wind)).expect("three cuts fit");
    assert_eq!(digest(&intact), VAD_DIGEST);
}

/// A sweep with no velocity anywhere is not a velocity tilt, and neither is a
/// sweep too short to be a cut.
#[test]
fn the_walk_refuses_a_moment_less_sweep_and_a_two_radial_one() {
    let long_but_dry = Sweep::new(
        1,
        (0..N_RADIALS)
            .map(|i| {
                Radial::new(
                    0,
                    i as u16,
                    i as f32,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    1,
                    0.5,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect(),
    );
    let wet_but_short = Sweep::new(
        2,
        vad_sweep(2, 1.47, 0.0, (12.0, -5.0), false).radials()[..2].to_vec(),
    );
    let scan = Scan::new(vcp(), vec![long_but_dry, wet_but_short]);
    assert_eq!(tilts(&scan).count(), 0);
    assert!(volume_wind_profile(&scan).is_none());
}

/// Each gate's status is the decoder's own answer, verbatim — no aggregation,
/// because one gate is one cell here — and a radial carrying no velocity is a
/// row of absences rather than a row that merely looks empty.
#[test]
fn a_gates_status_is_the_decoders_answer_and_a_dry_radial_is_absent() {
    use crate::types::GateReport;

    const SCALE: f32 = 2.0;
    const OFFSET: f32 = 129.0;
    // raw 0 -> below threshold, 1 -> range folded, >= 2 -> a number.
    let bytes: Vec<u8> = vec![0, 1, 200, 2, 0];
    let with = Radial::new(
        0,
        0,
        10.0,
        1.0,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        None,
        Some(MomentData::from_fixed_point(
            bytes.len() as u16,
            0,
            250,
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
    );
    // Same sweep, one radial that never reported the moment at all.
    let without = Radial::new(
        0,
        1,
        11.0,
        1.0,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );

    let g = grid(&[with, without]).expect("the first radial carries velocity");
    assert_eq!(
        g.status[0],
        vec![
            GateReport::BelowThreshold,
            GateReport::RangeFolded,
            GateReport::Value,
            GateReport::Value,
            GateReport::BelowThreshold,
        ],
        "the decoder's four answers arrive apart",
    );
    // The row that carries nothing: absence, and distinguishable from the
    // below-threshold gates of the row above it, which is the whole point.
    assert_eq!(
        g.status[1],
        vec![GateReport::NotReported; g.gate_count],
        "a radial with no velocity moment reports nothing, it does not measure emptiness",
    );

    // And the invariant, on both rows.
    for (row_v, row_s) in g.values.iter().zip(g.status.iter()) {
        for (v, s) in row_v.iter().zip(row_s.iter()) {
            assert_eq!(v.is_finite(), *s == GateReport::Value);
        }
    }
}

/// The borrowed view carries the plane, so a consumer in `nrot` never has to
/// re-derive from `values` the distinction `values` cannot hold.
#[test]
fn velocity_grid_sweeps_carry_the_report_plane() {
    use crate::types::GateReport;

    let bytes: Vec<u8> = vec![0, 1, 200, 2, 0];
    let radial = Radial::new(
        0,
        0,
        10.0,
        1.0,
        RadialStatus::IntermediateRadialData,
        1,
        0.5,
        None,
        Some(MomentData::from_fixed_point(
            bytes.len() as u16,
            0,
            250,
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
    );
    let g = grid(&[radial]).expect("the radial carries velocity");
    let view = g.sweep(None);
    let plane = view
        .status
        .expect("a decoded sweep's view carries the plane");
    assert_eq!(plane, g.status.as_slice(), "and it is the grid's own");
    assert_eq!(plane[0][0], GateReport::BelowThreshold);
    assert!(
        view.vel_grid[0][0].is_nan(),
        "which the values it sits beside cannot say",
    );
}
