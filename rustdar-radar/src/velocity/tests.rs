use super::*;
use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, VolumeCoveragePattern};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const N_RADIALS: usize = 360;
const N_GATES: usize = 240;

/// [`profile_bits`] of [`vad_volume`]'s `(12, -5)` volume, hashed.
///
/// Recorded from `render::build_wind_profile` — one of the three drivers this
/// module replaced — while all three still existed, and asserted here against
/// their single successor. See `the_wind_fit_still_returns_the_number_the_three_drivers_did`.
const VAD_DIGEST: u64 = 0xc001_5958_cdc9_1e48;

/// One velocity sweep carrying an exact VAD signature.
///
/// Gate `(az, r)` reads the radial component of a single horizontal wind —
/// `u·sin(az)·cos(el) + v·cos(az)·cos(el)` — through the real 8-bit velocity
/// codec, so the samples the fit sees are quantized to 0.5 m/s like a radar's.
/// `blank_first` drops the velocity moment from radial 0 and leaves the other
/// 359 alone, which is the shape a first-radial admission test refuses.
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
///
/// [`VAD_DIGEST`] was recorded from the render path's own fit before this
/// module existed, on this volume, and the SRV path's fit was pinned equal to
/// it to the bit at the same time. This is that number, from the one fit that
/// replaced both.
///
/// The tolerance check underneath is what stops the digest from being a hash
/// of an arbitrary answer: the fit *is* the planted wind, to six thousandths
/// of a metre per second, which is the 0.5 m/s codec's rounding bias and not
/// noise that would average out.
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
///
/// [`volume_wind_profile`] drops each grid before decoding the next; the
/// derivations in [`crate::derive`] keep the whole volume's and lend them to
/// the same fit. Two callers, one answer — which is the property the three
/// separate drivers could not state.
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
///
/// All three drivers admitted a sweep on `radials.first().velocity()` alone
/// while [`grid`] found the sweep's geometry with a `find_map` over every
/// radial — so one blank leading radial made 359 good ones invisible to the
/// wind fit, and a volume of such cuts had no wind profile at all: no dealias
/// seed for NROT or SRV, and no Bunkers vector, so SRV refused to render.
/// That is the behaviour this changes, and it changes it in one direction —
/// the fit gains a tilt it should always have had.
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

    // And the ordinary volume is untouched by the wider guard: every radial
    // there carries velocity, so the two admission tests agree and the digest
    // above still holds. Stated here because "more correct" has to mean the
    // one case moved and nothing else did.
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
///
/// Non-circular by construction: the raw bytes are the input and
/// `nexrad_model`'s `MomentData::from_fixed_point` is what turns raw 0 into
/// `BelowThreshold` and raw 1 into `RangeFolded`. This asserts that
/// [`grid`] carries the decoder's answer through, not that it agrees with a
/// table of ours.
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
