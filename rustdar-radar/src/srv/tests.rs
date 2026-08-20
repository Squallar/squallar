use super::*;
use crate::types::GateReport;
use nexrad_model::data::{MomentData, Radial, RadialStatus};

/// Levels at the profile's 0.3 km layer centres, so `WindProfile::from_levels`
/// maps level `l` onto layer `l` exactly.
fn profile_at_centres(uv: impl Fn(usize) -> (f64, f64), count: usize) -> WindProfile {
    let levels: Vec<(f64, f64, f64)> = (0..count)
        .map(|l| {
            let (u, v) = uv(l);
            ((l as f64 + 0.5) * WindProfile::LAYER_KM, u, v)
        })
        .collect();
    WindProfile::from_levels(&levels).expect("levels are non-empty")
}

/// Unidirectional westerly shear, hand-computed: u = 10 + 2·l over the
/// twenty 0–6 km layers, v = 0.
///
/// mean u = 10 + 2·(19/2) = 29; head (l = 0, 1) = 11; tail (l = 18, 19)
/// = 47; S = (36, 0); (S_v, −S_u)/|S| = (0, −1); V_rm = (29, −7.5).
#[test]
fn bunkers_on_unidirectional_shear_deviates_straight_right() {
    let p = profile_at_centres(|l| (10.0 + 2.0 * l as f64, 0.0), 20);
    let (u, v) = bunkers_right_mover_uv(&p).expect("a full profile supports Bunkers");
    assert!((u - 29.0).abs() < 1e-9, "u = {u}");
    assert!((v + 7.5).abs() < 1e-9, "v = {v}");

    let m = bunkers_right_mover(&p).expect("same profile");
    assert_eq!(m.source, StormMotionSource::BunkersRightMover);
    let want_kt = (29.0f64.powi(2) + 7.5f64.powi(2)).sqrt() / KT_TO_MS;
    assert!((m.speed_kt as f64 - want_kt).abs() < 1e-3, "{}", m.speed_kt);
    // Motion toward (29, −7.5): compass atan2(29, −7.5) = 104.5° toward,
    // so 284.5° from.
    let want_dir = (29.0f64.atan2(-7.5).to_degrees() + 180.0).rem_euclid(360.0);
    assert!(
        (m.direction_deg as f64 - want_dir).abs() < 1e-3,
        "{} vs {want_dir}",
        m.direction_deg,
    );
}

/// A curved (two-segment) hodograph, hand-computed: the bottom ten
/// layers at (0, 10), the top ten at (10, 0).
///
/// mean = (5, 5); head = (0, 10); tail = (10, 0); S = (10, −10),
/// |S| = 14.142…; deviation = 7.5·(−10, −10)/|S| = (−5.303, −5.303);
/// V_rm = (−0.303, −0.303).
#[test]
fn bunkers_on_a_curved_hodograph_deviates_right_of_the_shear() {
    let p = profile_at_centres(|l| if l < 10 { (0.0, 10.0) } else { (10.0, 0.0) }, 20);
    let (u, v) = bunkers_right_mover_uv(&p).expect("a full profile supports Bunkers");
    let want = 5.0 - 7.5 * 10.0 / (200.0f64).sqrt();
    assert!((u - want).abs() < 1e-9, "u = {u}, want {want}");
    assert!((v - want).abs() < 1e-9, "v = {v}, want {want}");
    assert!(u < 0.0, "the deviation outweighs the mean here");
}

#[test]
fn bunkers_falls_back_to_the_mean_wind_without_shear_direction() {
    let uniform = profile_at_centres(|_| (15.0, 5.0), 20);
    assert_eq!(bunkers_right_mover_uv(&uniform), Some((15.0, 5.0)));

    // Only three levels fit near the surface: `from_levels` clamp-fills three
    // layers past the last one, leaving 6 of 20 — under the floor.
    let hollow = profile_at_centres(|l| (2.0 * l as f64, 0.0), 3);
    assert_eq!(bunkers_right_mover_uv(&hollow), None);
}

/// The correction is `+speed·cos(direction − azimuth)` in m/s at the radial
/// centre — [`crate::srm`]'s pinned conventions.
#[test]
fn the_storm_motion_term_is_added_along_the_radial() {
    let mut grid = VelocityGrid {
        values: vec![vec![10.0; 4]; 4],
        status: vec![vec![GateReport::Value; 4]; 4],
        azimuths_deg: vec![90.0, 180.0, 270.0, 0.0],
        gate_count: 4,
        first_gate_range_km: 2.125,
        gate_interval_km: 0.25,
    };
    let motion = SrvMotion {
        speed_kt: 30.0,
        direction_deg: 90.0,
        source: StormMotionSource::UserOverride,
    };
    apply_storm_motion(&mut grid, &motion);
    let full = 30.0 * KT_TO_MS;
    assert!((grid.values[0][0] - (10.0 + full)).abs() < 1e-9, "az 090");
    assert!((grid.values[2][0] - (10.0 - full)).abs() < 1e-9, "az 270");
    assert!((grid.values[1][0] - 10.0).abs() < 1e-9, "az 180");
    assert!((grid.values[3][0] - 10.0).abs() < 1e-9, "az 000");
}

#[test]
fn a_zero_vector_is_identity_and_no_data_stays_empty() {
    let mut grid = VelocityGrid {
        values: vec![vec![-12.5, f64::NAN, 33.0]],
        status: vec![vec![
            GateReport::Value,
            GateReport::NotReported,
            GateReport::Value,
        ]],
        azimuths_deg: vec![137.0],
        gate_count: 3,
        first_gate_range_km: 2.125,
        gate_interval_km: 0.25,
    };
    let zero = SrvMotion {
        speed_kt: 0.0,
        direction_deg: 285.7,
        source: StormMotionSource::UserOverride,
    };
    apply_storm_motion(&mut grid, &zero);
    assert_eq!(grid.values[0][0], -12.5);
    assert!(grid.values[0][1].is_nan());
    assert_eq!(grid.values[0][2], 33.0);

    let moving = SrvMotion {
        speed_kt: 45.0,
        direction_deg: 137.0,
        source: StormMotionSource::UserOverride,
    };
    apply_storm_motion(&mut grid, &moving);
    assert!(
        grid.values[0][1].is_nan(),
        "a gate with no data must not paint the storm-motion field"
    );
    assert!((grid.values[0][0] - (-12.5 + 45.0 * KT_TO_MS)).abs() < 1e-9);
}

#[test]
fn the_msus_arithmetic_matches_the_level3_derivation_gate_for_gate() {
    use nexrad_level3::model::{
        DataLayer, DataPacket, Level3Message, MessageHeader, ProductDescriptionBlock, RadialPacket,
        RadialRun, SymbologyBlock,
    };

    // A 154-shaped message: 0.5°-wide radials, thresholds -63.5/0.5/254.
    let mut thresholds = [0u16; 16];
    thresholds[0] = -635i16 as u16;
    thresholds[1] = 5;
    thresholds[2] = 254;
    let radials: Vec<RadialRun> = (0..720)
        .map(|i| RadialRun {
            start_angle: i as f32 * 0.5,
            angle_delta: 0.5,
            gate_values: (0..40)
                .map(|j| (2 + (i * 7 + j * 11) % 254) as u16)
                .collect(),
        })
        .collect();
    let msg = Level3Message {
        header: MessageHeader {
            message_code: 154,
            date_of_message: 20661,
            time_of_message: 7108,
            message_length: 0,
            source_id: 0,
            destination_id: 0,
            number_of_blocks: 3,
        },
        pdb: ProductDescriptionBlock {
            block_divider: -1,
            latitude: 44.849,
            longitude: -93.565,
            height: 1000,
            product_code: 154,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 39,
            volume_scan_date: 20661,
            volume_scan_time: 7108,
            generation_date: 20661,
            generation_time: 7108,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number: 1,
            product_specific_3: 5,
            thresholds,
            product_specific_47_53: [-93, 74, 0, 8097, 1, 13, 16382],
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        },
        symbology: Some(SymbologyBlock {
            block_id: 1,
            block_length: 0,
            num_layers: 1,
            layers: vec![DataLayer {
                layer_length: 0,
                packets: vec![DataPacket::DigitalRadial(RadialPacket {
                    first_range_bin: 0,
                    num_range_bins: 40,
                    i_center: 0,
                    j_center: 0,
                    scale_factor: 0.999,
                    is_legacy: false,
                    xdr_data_scale: None,
                    xdr_data_offset: None,
                    radials,
                })],
            }],
        }),
    };

    let sample = crate::srm::StormMotionSample {
        motion: nexrad_level3::model::StormMotion {
            speed_kt: 33.7,
            direction_deg: 213.4,
            is_scit_average: true,
        },
        volume: Some((20661, 7108)),
    };
    let theirs = crate::srm::derive(&msg, &sample).expect("154 derives");

    let motion = SrvMotion {
        speed_kt: 33.7,
        direction_deg: 213.4,
        source: StormMotionSource::UserOverride,
    };
    let packet = crate::srm::radial_packet(&msg).expect("the fixture carries radials");
    let mut ours = grid_from_packet(packet, &msg.pdb);
    apply_storm_motion(&mut ours, &motion);

    let (mut n, mut within_one, mut exact) = (0usize, 0usize, 0usize);
    for (our_row, their_run) in ours.values.iter().zip(&theirs.packet.radials) {
        for (our_ms, &their_gate) in our_row.iter().zip(&their_run.gate_values) {
            if their_gate < 2 {
                continue;
            }
            let our_level =
                (our_ms / KT_TO_MS * theirs.scale as f64).round() + theirs.offset as f64;
            let diff = (our_level - their_gate as f64).abs();
            n += 1;
            exact += usize::from(diff == 0.0);
            within_one += usize::from(diff <= 1.0);
        }
    }
    assert_eq!(n, 720 * 40, "every gate compared");
    assert_eq!(
        within_one, n,
        "the two arithmetics may differ by float rounding alone"
    );
    assert!(
        exact as f64 / n as f64 > 0.999,
        "more than rounding separates the ports: {exact}/{n} exact"
    );
}

/// A Level III velocity packet as this module's grid: m/s through the PDB's
/// scale/offset, radial-centre azimuths, gate centres at
/// `(first_range_bin + j + 0.5) · 0.25 km`.
fn grid_from_packet(
    packet: &nexrad_level3::model::RadialPacket,
    pdb: &nexrad_level3::model::ProductDescriptionBlock,
) -> VelocityGrid {
    let scale = pdb.data_scale() as f64;
    let offset = pdb.data_offset() as f64;
    let gate_km = pdb.range_gate_km().unwrap_or(0.25);
    VelocityGrid {
        values: packet
            .radials
            .iter()
            .map(|run| {
                run.gate_values
                    .iter()
                    .map(|&g| {
                        if g < 2 {
                            f64::NAN
                        } else {
                            (g as f64 - offset) / scale
                        }
                    })
                    .collect()
            })
            .collect(),
        // Levels 0 and 1 are below-threshold and range-folded across the whole
        // Level III family — the same convention `twin::compare::ValueCodec`
        // decodes by.
        status: packet
            .radials
            .iter()
            .map(|run| {
                run.gate_values
                    .iter()
                    .map(|&g| match g {
                        0 => GateReport::BelowThreshold,
                        1 => GateReport::RangeFolded,
                        _ => GateReport::Value,
                    })
                    .collect()
            })
            .collect(),
        azimuths_deg: packet
            .radials
            .iter()
            .map(|run| run.start_angle as f64 + run.angle_delta as f64 / 2.0)
            .collect(),
        gate_count: packet
            .radials
            .iter()
            .map(|r| r.gate_values.len())
            .max()
            .unwrap_or(0),
        first_gate_range_km: (packet.first_range_bin as f64 + 0.5) * gate_km,
        gate_interval_km: gate_km,
    }
}

fn radial(azimuth: f32, elevation: f32, gates: Vec<u8>) -> Radial {
    Radial::new(
        0,
        0,
        azimuth,
        0.5,
        RadialStatus::IntermediateRadialData,
        1,
        elevation,
        None,
        Some(MomentData::from_fixed_point(
            gates.len() as u16,
            2125,
            250,
            8,
            2.0,
            129.0,
            gates,
        )),
        None,
        None,
        None,
        None,
        None,
    )
}

#[test]
fn the_display_dealias_keeps_isolated_pockets_the_nrot_posture_drops() {
    let n = 72;
    let gates = 40;
    // Mostly empty sweep: a 2×3 pocket of 20 m/s at long range, one far
    // bin pinning the Nyquist estimate at 26 m/s.
    let radials: Vec<Radial> = (0..n)
        .map(|i| {
            let mut bytes = vec![0u8; gates]; // 0 = below threshold
            if (30..32).contains(&i) {
                for b in bytes.iter_mut().take(39).skip(36) {
                    *b = 129 + 40; // 20 m/s
                }
            }
            if i == 0 {
                bytes[39] = 129 + 52; // 26 m/s
            }
            radial(i as f32 * 5.0, 0.5, bytes)
        })
        .collect();

    let grid = dealiased_grid(&radials, 0.5, None, None).expect("velocity present");
    assert!(
        (grid.values[30][37] - 20.0).abs() < 1e-6,
        "Coverage keeps the unreached pocket: {}",
        grid.values[30][37],
    );

    let raw = crate::velocity::grid(&radials).expect("velocity present");
    let mut strict = raw.values.clone();
    crate::nrot::dealias(
        &mut strict,
        &raw.sweep(None),
        0.5,
        None,
        DealiasProfile::NoFalseShear,
    );
    assert!(
        strict[30][37].is_nan(),
        "NoFalseShear censors the same pocket"
    );
}

#[test]
fn compute_srv_derives_a_full_sweep() {
    let n = 72;
    let radials: Vec<Radial> = (0..n)
        .map(|i| {
            let az = i as f64 * 360.0 / n as f64;
            let v_ms = 15.0 * az.to_radians().sin(); // zero-isodop north
            let byte = (129.0 + v_ms * 2.0).round() as u8;
            radial(az as f32, 0.5, vec![byte; 40])
        })
        .collect();
    let motion = SrvMotion {
        speed_kt: 20.0,
        direction_deg: 0.0,
        source: StormMotionSource::UserOverride,
    };
    let grid = compute_srv_grid(&radials, 0.5, None, &motion, None).expect("velocity present");
    assert_eq!(grid.gate_count, 40);
    assert!((grid.first_gate_range_km - 2.125).abs() < 1e-9);
    assert!((grid.gate_interval_km - 0.25).abs() < 1e-9);
    // At azimuth 0 the storm comes straight down the radial: +20 kt.
    assert!(
        (grid.values[0][0] - 20.0 * KT_TO_MS).abs() < 0.3,
        "az 0: {}",
        grid.values[0][0],
    );
    // At azimuth 90: base 15 m/s, correction zero.
    assert!(
        (grid.values[18][0] - 15.0).abs() < 0.3,
        "az 90: {}",
        grid.values[18][0],
    );
}

#[test]
fn the_user_override_dominates_a_derived_rung() {
    let p = profile_at_centres(|l| (10.0 + 2.0 * l as f64, 0.0), 20);
    let over = SrvMotion::user_override(45.0, 210.0).expect("finite");
    let picked = storm_motion(Some(&p), Some(over), None, SrvFallback::MeanWind)
        .expect("an override is a vector");
    assert_eq!(picked.speed_kt, 45.0);
    assert_eq!(picked.direction_deg, 210.0);
    assert_eq!(picked.source, StormMotionSource::UserOverride);

    let picked = storm_motion(Some(&p), Some(over), None, SrvFallback::BunkersRightMover)
        .expect("still the override");
    assert_eq!(picked.source, StormMotionSource::UserOverride);

    let default = storm_motion(Some(&p), None, None, SrvFallback::MeanWind)
        .expect("the mean wind from the profile");
    assert_eq!(default.source, StormMotionSource::MeanWind);

    let poser = SrvMotion {
        speed_kt: 1.0,
        direction_deg: 2.0,
        source: StormMotionSource::BunkersRightMover,
    };
    let picked = storm_motion(Some(&p), Some(poser), None, SrvFallback::MeanWind)
        .expect("falls through to the derived rung");
    assert_eq!(picked.source, StormMotionSource::MeanWind);
    assert_ne!(picked.speed_kt, 1.0);

    assert_eq!(
        storm_motion(None, None, None, SrvFallback::MeanWind),
        None,
        "no vector, no render",
    );
    assert!(SrvMotion::user_override(f32::NAN, 90.0).is_none());
    assert!(SrvMotion::user_override(30.0, f32::INFINITY).is_none());
}

/// The derived default is the mean wind, and the right-mover is a choice: the
/// right-mover is a supercell motion *prediction*, 7.5 m/s off the mean wind
/// in whatever direction the shear points, and measured against the vector the
/// RPG publishes the mean wind sits an order of magnitude closer.
#[test]
fn the_derived_default_is_the_mean_wind_and_bunkers_is_a_choice() {
    let p = profile_at_centres(|l| (10.0 + 2.0 * l as f64, 0.0), 20);

    let default =
        storm_motion(Some(&p), None, None, SrvFallback::default()).expect("the default renders");
    assert_eq!(
        default.source,
        StormMotionSource::MeanWind,
        "the shipped default fell to something other than the mean wind",
    );

    let asked = storm_motion(Some(&p), None, None, SrvFallback::BunkersRightMover)
        .expect("the right-mover is still reachable");
    assert_eq!(asked.source, StormMotionSource::BunkersRightMover);

    assert_ne!(default.speed_kt, asked.speed_kt);
    assert_ne!(default.direction_deg, asked.direction_deg);

    let (u, v) = mean_wind_uv(&p).expect("a full profile has a mean wind");
    assert!((u - 29.0).abs() < 1e-9, "u = {u}");
    assert!(v.abs() < 1e-9, "v = {v}");

    // Twelve fitted layers is the mean's floor; with only the lowest twelve
    // present the 5.5–6 km shear band is empty, so Bunkers has no direction to
    // deviate in.
    let hollow = profile_at_centres(|l| (10.0 + 2.0 * l as f64, 0.0), 12);
    assert_eq!(
        bunkers_right_mover(&hollow),
        None,
        "the fixture must be one Bunkers cannot answer, or this proves nothing",
    );
    let picked = storm_motion(Some(&hollow), None, None, SrvFallback::MeanWind)
        .expect("the mean wind still renders where the right-mover cannot");
    assert_eq!(picked.source, StormMotionSource::MeanWind);

    assert_eq!(
        storm_motion(Some(&hollow), None, None, SrvFallback::BunkersRightMover),
        None,
    );
}

#[test]
fn the_rpg_vector_outranks_bunkers_and_yields_to_an_override() {
    let p = profile_at_centres(|l| (10.0 + 2.0 * l as f64, 0.0), 20);
    let rpg = SrvMotion::rpg_scit_average(31.0, 246.0).expect("finite");

    for fallback in [SrvFallback::MeanWind, SrvFallback::BunkersRightMover] {
        let picked = storm_motion(Some(&p), None, Some(rpg), fallback).expect("the RPG's vector");
        assert_eq!(
            picked.source,
            StormMotionSource::RpgScitAverage,
            "{fallback:?}"
        );
        assert_eq!(picked.speed_kt, 31.0);
        assert_eq!(picked.direction_deg, 246.0);
    }

    let over = SrvMotion::user_override(45.0, 210.0).expect("finite");
    let picked =
        storm_motion(Some(&p), Some(over), Some(rpg), SrvFallback::MeanWind).expect("the override");
    assert_eq!(picked.source, StormMotionSource::UserOverride);
    assert_eq!(picked.speed_kt, 45.0);

    let picked =
        storm_motion(None, None, Some(rpg), SrvFallback::MeanWind).expect("no profile needed");
    assert_eq!(picked.source, StormMotionSource::RpgScitAverage);

    let poser = SrvMotion {
        speed_kt: 1.0,
        direction_deg: 2.0,
        source: StormMotionSource::BunkersRightMover,
    };
    let picked =
        storm_motion(Some(&p), None, Some(poser), SrvFallback::MeanWind).expect("falls through");
    assert_eq!(picked.source, StormMotionSource::MeanWind);
    assert_ne!(picked.speed_kt, 1.0);

    assert!(SrvMotion::rpg_scit_average(f32::NAN, 90.0).is_none());
    assert!(SrvMotion::rpg_scit_average(30.0, f32::INFINITY).is_none());
}

/// Clear-air and cell-free volumes publish exactly 0.0 kt from 0.0°: SCIT
/// tracked nothing, so the RPG paints an unshifted field.
#[test]
fn a_zero_rpg_vector_is_a_reading_and_not_a_gap() {
    let p = profile_at_centres(|l| (10.0 + 2.0 * l as f64, 0.0), 20);
    let zero = SrvMotion::rpg_scit_average(0.0, 0.0).expect("zero is finite");
    let picked = storm_motion(Some(&p), None, Some(zero), SrvFallback::MeanWind)
        .expect("zero still renders");
    assert_eq!(picked.source, StormMotionSource::RpgScitAverage);
    assert_eq!(picked.speed_kt, 0.0);
    assert_eq!(picked.direction_deg, 0.0);

    let mut grid = VelocityGrid {
        values: vec![vec![12.0; 4]; 4],
        status: vec![vec![GateReport::Value; 4]; 4],
        azimuths_deg: vec![90.0, 180.0, 270.0, 0.0],
        gate_count: 4,
        first_gate_range_km: 2.125,
        gate_interval_km: 0.25,
    };
    let before = grid.values.clone();
    apply_storm_motion(&mut grid, &picked);
    assert_eq!(grid.values, before, "a zero vector shifts nothing");
}

#[test]
fn a_mean_wind_fallback_does_not_claim_to_be_a_right_mover() {
    let uniform = profile_at_centres(|_| (15.0, 5.0), 20);
    let m = bunkers_right_mover(&uniform).expect("the mean wind still renders");
    assert_eq!(m.source, StormMotionSource::MeanWind);
    assert_eq!(bunkers_right_mover_uv(&uniform), Some((15.0, 5.0)));
    let want_kt = (15.0f64.powi(2) + 5.0f64.powi(2)).sqrt() / KT_TO_MS;
    assert!((m.speed_kt as f64 - want_kt).abs() < 1e-3, "{}", m.speed_kt);

    let sheared = profile_at_centres(|l| (10.0 + 2.0 * l as f64, 0.0), 20);
    let m = bunkers_right_mover(&sheared).expect("a full profile supports Bunkers");
    assert_eq!(m.source, StormMotionSource::BunkersRightMover);

    let picked = storm_motion(Some(&uniform), None, None, SrvFallback::BunkersRightMover)
        .expect("still a vector");
    assert_eq!(picked.source, StormMotionSource::MeanWind);
}

/// The declaration order is the fallback order, so `<` reads as "nearer the
/// top of the chain" — the convention `MeltingLayerSource` uses, and the
/// reason `Ord` is derived at all.
#[test]
fn the_storm_motion_chain_orders_itself_best_first() {
    use StormMotionSource::*;
    assert!(UserOverride < RpgScitAverage);
    assert!(RpgScitAverage < MeanWind);
    assert!(MeanWind < BunkersRightMover);

    assert!(RpgScitAverage.is_rpg());
    for other in [UserOverride, BunkersRightMover, MeanWind] {
        assert!(!other.is_rpg(), "{other:?}");
    }

    assert_eq!(SrvFallback::default(), SrvFallback::MeanWind);
    assert_eq!(SrvFallback::MeanWind.source(), MeanWind);
    assert_eq!(SrvFallback::BunkersRightMover.source(), BunkersRightMover);

    for naming in [StormMotionSource::label, StormMotionSource::tag] {
        let named = [UserOverride, RpgScitAverage, MeanWind, BunkersRightMover].map(naming);
        for (i, a) in named.iter().enumerate() {
            for b in &named[i + 1..] {
                assert_ne!(a, b, "two rungs share a name");
            }
        }
    }
}

#[test]
fn no_rung_names_itself_in_this_codebases_private_vocabulary() {
    use StormMotionSource::*;
    for rung in [UserOverride, RpgScitAverage, MeanWind, BunkersRightMover] {
        for text in [rung.label(), rung.tag()] {
            let lower = text.to_ascii_lowercase();
            for jargon in ["rpg", "scit", "cell average"] {
                assert!(
                    !lower.contains(jargon),
                    "{rung:?} says {text:?}, which names {jargon:?} at the reader",
                );
            }
        }
        assert!(
            rung.tag().len() <= 12 && !rung.tag().contains('.'),
            "{rung:?}'s tag {:?} is a sentence, not a label",
            rung.tag(),
        );
    }
}
