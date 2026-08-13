use super::*;
use nexrad_level3::model::{DataLayer, MessageHeader, ProductDescriptionBlock, SymbologyBlock};

fn header(code: i16) -> MessageHeader {
    MessageHeader {
        message_code: code,
        date_of_message: 20661,
        time_of_message: 7108,
        message_length: 0,
        source_id: 0,
        destination_id: 0,
        number_of_blocks: 3,
    }
}

/// Halfwords 31–33 of a real `MPX_N1G`: -63.5 m/s minimum, 0.5 m/s
/// increment, 254 levels.
fn velocity_pdb(
    product_code: i16,
    elevation_tenths: i16,
    elevation_number: u16,
    volume: u32,
) -> ProductDescriptionBlock {
    let mut thresholds = [0u16; 16];
    thresholds[0] = -635i16 as u16;
    thresholds[1] = 5;
    thresholds[2] = 254;
    ProductDescriptionBlock {
        block_divider: -1,
        latitude: 44.849,
        longitude: -93.565,
        height: 1000,
        product_code,
        operational_mode: 2,
        vcp: 212,
        sequence_number: 0,
        volume_scan_number: 39,
        volume_scan_date: 20661,
        volume_scan_time: volume,
        generation_date: 20661,
        generation_time: volume,
        product_specific_1: 0,
        product_specific_2: 0,
        elevation_number,
        product_specific_3: elevation_tenths,
        thresholds,
        // Halfword 51 is the BZ2 compression flag on a digital product.
        product_specific_47_53: [-93, 74, 0, 8097, 1, 13, 16382],
        version: 0,
        spot_blank: 0,
        symbology_offset: 60,
        graphic_offset: 0,
        tabular_offset: 0,
    }
}

/// Gate 129 is 0 m/s; each step is 0.5 m/s.
fn gate_for_ms(ms: f32) -> u16 {
    (129.0 + ms / 0.5).round() as u16
}

fn message(pdb: ProductDescriptionBlock, radials: Vec<RadialRun>) -> Level3Message {
    let code = pdb.product_code;
    let num_range_bins = radials
        .iter()
        .map(|r| r.gate_values.len())
        .max()
        .unwrap_or(0) as u16;
    Level3Message {
        header: header(code),
        pdb,
        symbology: Some(SymbologyBlock {
            block_id: 1,
            block_length: 0,
            num_layers: 1,
            layers: vec![DataLayer {
                layer_length: 0,
                packets: vec![DataPacket::DigitalRadial(RadialPacket {
                    first_range_bin: 0,
                    num_range_bins,
                    i_center: 0,
                    j_center: 0,
                    // What the RPG really writes: 999/1000, for a product
                    // whose gates are 0.25 km.
                    scale_factor: 0.999,
                    is_legacy: false,
                    xdr_data_scale: None,
                    xdr_data_offset: None,
                    radials,
                })],
            }],
        }),
    }
}

/// One radial per listed azimuth, every gate at the same velocity, on the
/// 1.3° cut.
fn uniform(product_code: i16, azimuths: &[f32], width: f32, ms: f32) -> Level3Message {
    uniform_at(product_code, 13, 9, azimuths, width, ms)
}

/// [`uniform`] at a named cut, for the tests that care which tilt it is.
fn uniform_at(
    product_code: i16,
    elevation_tenths: i16,
    elevation_number: u16,
    azimuths: &[f32],
    width: f32,
    ms: f32,
) -> Level3Message {
    let radials = azimuths
        .iter()
        .map(|&a| RadialRun {
            start_angle: a,
            angle_delta: width,
            gate_values: vec![gate_for_ms(ms); 4],
        })
        .collect();
    message(
        velocity_pdb(product_code, elevation_tenths, elevation_number, 7108),
        radials,
    )
}

fn sample(speed_kt: f32, direction_deg: f32, volume: u32) -> StormMotionSample {
    StormMotionSample {
        motion: StormMotion {
            speed_kt,
            direction_deg,
            is_scit_average: true,
        },
        volume: Some((20661, volume)),
    }
}

fn knots_at(d: &DerivedSrm, radial: usize, gate: usize) -> f32 {
    (d.packet.radials[radial].gate_values[gate] as f32 - d.offset) / d.scale
}

/// The correction is `+speed·cos(direction − azimuth)`, in knots, on top of
/// a velocity the source stores in metres per second.
///
/// The fixture is a *uniform* 10 m/s field, so every number below is the
/// storm-motion term plus a constant — a dropped conversion, a dropped
/// cosine or a flipped sign each move a different one.
#[test]
fn the_storm_motion_term_is_added_along_the_radial() {
    // Radials at 0/90/180/270, each 1° wide, so their centres are at 0.5°,
    // 90.5°, … — near enough to read the cardinal cosines off.
    let msg = uniform(154, &[89.5, 179.5, 269.5, 359.5], 1.0, 10.0);
    let d = derive(&msg, &sample(30.0, 90.0, 7108)).expect("154 is a velocity source");
    let base: f32 = 10.0 * (1.0 / 0.514_444);
    assert!((base - 19.438).abs() < 0.01, "10 m/s is 19.4 kt");

    // Azimuth 90 points at the direction the storm comes from: full +30 kt.
    assert!((knots_at(&d, 0, 0) - (base + 30.0)).abs() < 0.5, "az 090");
    // Azimuth 270 is the reciprocal: full -30 kt.
    assert!((knots_at(&d, 2, 0) - (base - 30.0)).abs() < 0.5, "az 270");
    // Orthogonal radials keep the base velocity.
    assert!((knots_at(&d, 1, 0) - base).abs() < 0.5, "az 180");
    assert!((knots_at(&d, 3, 0) - base).abs() < 0.5, "az 000");
}

/// The base field must arrive in knots. A missing conversion leaves 10
/// where 19.4 belongs — a 48% error that no sign or index test sees,
/// because the storm-motion term is unaffected.
#[test]
fn the_source_velocity_is_converted_from_metres_per_second() {
    let msg = uniform(99, &[0.0], 1.0, 25.0);
    let d = derive(&msg, &sample(0.0, 0.0, 7108)).unwrap();
    assert!(
        (knots_at(&d, 0, 0) - 48.60).abs() < 0.3,
        "25 m/s is 48.6 kt"
    );
    assert!(knots_at(&d, 0, 0) > 30.0, "not left in metres per second");
}

/// A zero vector must leave the field alone: with no storm motion,
/// storm-relative velocity *is* base velocity. This is the control that
/// separates the conversion from the correction.
#[test]
fn a_zero_vector_reproduces_the_base_velocity() {
    for ms in [-40.0f32, -12.5, 0.0, 7.5, 33.0] {
        let msg = uniform(154, &[0.0, 137.0, 300.0], 0.5, ms);
        let d = derive(&msg, &sample(0.0, 285.7, 7108)).unwrap();
        for r in 0..3 {
            let want = ms as f64 * MS_TO_KT;
            assert!(
                (knots_at(&d, r, 0) as f64 - want).abs() < 0.3,
                "{ms} m/s radial {r}: got {}",
                knots_at(&d, r, 0),
            );
        }
    }
}

/// The correction uses the radial's **centre**, matching where
/// `render_level3_radial_to_image` places the gate.
///
/// Deliberately exaggerated geometry: at the 0.5° and 1° widths real
/// products carry, centre and leading edge differ by under 0.02 kt, so no
/// realistic fixture can tell them apart and one that tried would be
/// asserting on rounding. A 60°-wide radial makes the *convention*
/// observable, which is the thing that has to match the renderer.
#[test]
fn the_correction_uses_the_centre_of_the_radial_not_its_leading_edge() {
    // Leading edge 60°, width 60° → centre 90°, which is the peak.
    let msg = uniform(154, &[60.0], 60.0, 0.0);
    let d = derive(&msg, &sample(40.0, 90.0, 7108)).unwrap();
    assert!(
        (knots_at(&d, 0, 0) - 40.0).abs() < 0.3,
        "the centre is 090, the peak: got {}",
        knots_at(&d, 0, 0),
    );
    // The leading edge would give cos(30°) = 0.866 → 34.6 kt.
    assert!(
        (knots_at(&d, 0, 0) - 34.64).abs() > 1.0,
        "the correction was taken at the leading edge",
    );

    // And the reverse pairing: a radial whose centre is the zero crossing
    // but whose leading edge is not, so neither case passes by symmetry.
    let msg = uniform(154, &[150.0], 60.0, 0.0);
    let d2 = derive(&msg, &sample(40.0, 90.0, 7108)).unwrap();
    assert!(
        knots_at(&d2, 0, 0).abs() < 0.3,
        "the centre is 180, the zero crossing: got {}",
        knots_at(&d2, 0, 0),
    );
}

/// Below-threshold and range-folded gates stay below-threshold. Mapping
/// them through the arithmetic would paint the storm-motion field itself
/// across every gate the radar saw nothing in.
#[test]
fn gates_with_no_data_stay_empty() {
    let radials = vec![RadialRun {
        start_angle: 90.0,
        angle_delta: 1.0,
        gate_values: vec![0, 1, gate_for_ms(5.0), 0],
    }];
    let msg = message(velocity_pdb(99, 24, 5, 7108), radials);
    let d = derive(&msg, &sample(35.0, 90.0, 7108)).unwrap();
    let g = &d.packet.radials[0].gate_values;
    assert_eq!(g[0], 0, "below threshold");
    assert_eq!(g[1], 0, "range folded");
    assert_eq!(g[3], 0);
    assert!(g[2] > 1, "the gate that had data still does");
}

/// The gate spacing must come from the product code. The packet says 999,
/// which reads as ~1 km — four times too coarse for a 0.25 km product, and
/// the field would be drawn out to 1200 km.
#[test]
fn the_derived_packet_carries_quarter_kilometre_gates() {
    for code in VELOCITY_PRODUCT_CODES {
        let msg = uniform(code, &[0.0], 1.0, 0.0);
        assert!(
            (radial_packet(&msg).unwrap().gate_interval_km() - 1.001).abs() < 0.01,
            "the fixture really does carry the RPG's misleading 999",
        );
        let d = derive(&msg, &sample(0.0, 0.0, 7108)).unwrap();
        assert!(
            (d.packet.gate_interval_km() - 0.25).abs() < 1e-9,
            "product {code} gates are 0.25 km",
        );
    }
}

fn set_first_range_bin(msg: &mut Level3Message, bin: i16) {
    let sym = msg.symbology.as_mut().expect("the fixture has symbology");
    for layer in &mut sym.layers {
        for p in &mut layer.packets {
            if let DataPacket::DigitalRadial(rp) = p {
                rp.first_range_bin = bin;
            }
        }
    }
}

/// The first bin is an *index*, counted in gates
/// ([`nexrad_level3::model::RadialPacket::gate_range_km`]), so re-spacing the
/// derived packet from the source's declared ~1 km to the product's real
/// 0.25 km has to re-index it too. Carried over unchanged, the same number 4
/// would move the field's start from 4 km out to 1 km out — the whole field
/// pulled four times closer to the radar.
///
/// Every live velocity product declares 0 here, so nothing on the wire shows
/// this; the fixture has to push the index off zero to hold it at all.
#[test]
fn re_spacing_the_derived_packet_keeps_the_first_gate_where_the_source_put_it() {
    for code in VELOCITY_PRODUCT_CODES {
        let mut msg = uniform(code, &[0.0], 1.0, 0.0);
        set_first_range_bin(&mut msg, 4);

        let src = radial_packet(&msg).unwrap();
        let src_first_edge = f64::from(src.first_range_bin) * src.gate_interval_km();

        let d = derive(&msg, &sample(0.0, 0.0, 7108)).unwrap();
        let out_gate = d.packet.gate_interval_km();
        let out_first_edge = f64::from(d.packet.first_range_bin) * out_gate;

        assert!(
            (out_first_edge - src_first_edge).abs() <= out_gate / 2.0,
            "product {code}: the first gate moved from {src_first_edge} km to \
             {out_first_edge} km across the re-spacing",
        );
        // 4 bins of the declared 1.001 km, re-indexed onto 0.25 km gates.
        assert_eq!(
            d.packet.first_range_bin, 16,
            "product {code}: 4 x 1.001 km re-indexes to 16 x 0.25 km",
        );
    }
}

/// Elevation comes from the Product Description Block. `N1G` is 1.3° in
/// VCP 212, not the 1.5° its mnemonic suggests, and the two adjacent cuts
/// at one angle are told apart only by elevation number.
#[test]
fn elevation_comes_from_the_product_description_block() {
    let msg = message(
        velocity_pdb(154, 13, 9, 7108),
        vec![RadialRun {
            start_angle: 0.0,
            angle_delta: 0.5,
            gate_values: vec![gate_for_ms(0.0)],
        }],
    );
    let d = derive(&msg, &sample(0.0, 0.0, 7108)).unwrap();
    assert_eq!(d.elevation_angle, 1.3, "not the mnemonic's nominal 1.5");
    assert_eq!(d.elevation_number, 9, "the MRLE repeat, not cut 3");
}

/// Only dealiased velocity may be derived from. Handed the RPG's own
/// product 56 — which is already storm-relative — this must decline rather
/// than apply the correction a second time.
#[test]
fn an_already_storm_relative_product_is_not_a_source() {
    for code in [56i16, 55, 94, 134, 135, 163, 176, 177] {
        let msg = uniform(code, &[0.0], 1.0, 10.0);
        assert!(
            derive(&msg, &sample(30.0, 90.0, 7108)).is_none(),
            "product {code}"
        );
    }
    for code in VELOCITY_PRODUCT_CODES {
        assert!(derive(&uniform(code, &[0.0], 1.0, 10.0), &sample(30.0, 90.0, 7108)).is_some());
    }
}

/// A vector from another volume still produces a field — the alternative is
/// no storm-relative velocity at all — but says so.
#[test]
fn a_vector_from_another_volume_is_used_and_flagged() {
    let msg = uniform(99, &[0.0], 1.0, 10.0);
    let matched = derive(&msg, &sample(20.0, 270.0, 7108)).unwrap();
    let stale = derive(&msg, &sample(20.0, 270.0, 6952)).unwrap();
    assert_eq!(matched.motion_provenance, MotionProvenance::SameVolume);
    assert_eq!(stale.motion_provenance, MotionProvenance::PreviousVolume);
    // The accuracy signal itself, not just the provenance it reads. Its
    // only other assertion is negative, so a body of `false` — or one
    // inverted to `PreviousVolume`, which flips the validation harness's
    // "vector one volume stale" annotation — would otherwise go unnoticed;
    // that harness is `#[ignore]`d and cannot catch it.
    assert!(matched.motion_volume_matches());
    assert!(!stale.motion_volume_matches());
    // Same arithmetic either way: the flag is provenance, not a switch.
    assert_eq!(
        matched.packet.radials[0].gate_values,
        stale.packet.radials[0].gate_values,
    );
}

/// Both halves of the conclusiveness predicate, which cannot be falsified
/// where it is used: inside the live test the site count is never zero when
/// the gate count is large, so a mutant on that conjunct would survive by
/// construction.
#[test]
fn a_sample_is_conclusive_only_with_both_sites_and_gates() {
    assert!(sample_is_conclusive(1, MIN_NONZERO_GATES + 1));
    assert!(sample_is_conclusive(9, 500_000));
    // No site asserted on, however many gates were seen elsewhere — the
    // case where every site was quiet or quarantined.
    assert!(!sample_is_conclusive(0, 500_000));
    // Too few gates for a percentage to mean anything.
    assert!(!sample_is_conclusive(3, MIN_NONZERO_GATES));
    assert!(!sample_is_conclusive(3, 0));
    // Absolute, not relative to the constant: a floor expressed only in
    // terms of `MIN_NONZERO_GATES` moves with it, so lowering the constant
    // to 1 would leave every assertion above still passing.
    assert!(
        !sample_is_conclusive(3, 5_000),
        "5,000 gates is not a sample"
    );
    assert!(!sample_is_conclusive(3, 9_999));
    assert!(sample_is_conclusive(3, 200_000));
}

/// A non-finite vector must not become a sample at all. NaN makes every
/// equality test on the sample false, so a change detector comparing two
/// identical overrides fires on every frame.
#[test]
fn a_non_finite_override_is_not_constructible() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(
            StormMotionSample::user_override(bad, 240.0).is_none(),
            "speed {bad}"
        );
        assert!(
            StormMotionSample::user_override(30.0, bad).is_none(),
            "direction {bad}"
        );
    }
    assert!(
        StormMotionSample::user_override(0.0, 0.0).is_some(),
        "zero is legitimate"
    );
}

/// A hand-entered vector belongs to no volume and is never a SCIT average.
///
/// Not the same claim as "a different volume": a sentinel key made the two
/// indistinguishable, so every override rendered under a `(previous
/// volume)` annotation that named provenance it never had. It must also
/// not read as *this* volume — that would claim the RPG had fitted it.
#[test]
fn a_user_override_claims_no_provenance() {
    let s = StormMotionSample::user_override(45.0, 210.0).expect("finite");
    assert!(!s.motion.is_scit_average);
    assert_eq!(
        s.volume, None,
        "an override must carry no volume key at all"
    );
    let d = derive(&uniform(154, &[0.0], 0.5, 0.0), &s).unwrap();
    assert_eq!(d.motion_provenance, MotionProvenance::UserOverride);
    assert!(
        !d.motion_volume_matches(),
        "an override agrees with no volume, so it is not this one either"
    );
    assert_eq!(d.motion.speed_kt, 45.0);
    assert_eq!(d.motion.direction_deg, 210.0);
}

/// The four request keys, and the reason they are not `N0S`..`N3S`.
#[test]
fn every_tilt_product_is_a_dealiased_velocity_key() {
    assert_eq!(SRM_TILT_PRODUCTS, ["N0G", "N1G", "N2U", "N3U"]);
    for dead in ["N1S", "N2S", "N3S"] {
        assert!(
            !SRM_TILT_PRODUCTS.contains(&dead),
            "{dead} has had no data written since 2020 (NWS SCN 22-96)",
        );
    }
    // `N2G`/`N3G` and `N0U`/`N1U` are not in the bucket; asserted by name
    // because swapping one in is the obvious thing to try.
    for absent in ["N2G", "N3G", "N0U", "N1U"] {
        assert!(
            !SRM_TILT_PRODUCTS.contains(&absent),
            "{absent} is not published"
        );
    }
}

/// `N0S` is fetched but is not a tilt. Rendering it was the 0.5° pane's
/// old behaviour and is the thing this module exists to have stopped
/// doing: 1 km against 0.25 km, 16 display levels against 254, and the
/// RPG's vector baked in where the user's override belongs.
#[test]
fn the_vector_source_is_fetched_but_never_rendered() {
    assert_eq!(STORM_MOTION_PRODUCT, "N0S");
    assert!(
        !SRM_TILT_PRODUCTS.contains(&STORM_MOTION_PRODUCT),
        "{STORM_MOTION_PRODUCT} is back as a tilt: the 0.5° pane would be \
             1 km where the other three are 0.25 km, and would ignore the \
             storm motion override",
    );
    // The fetch list is exactly the vector source followed by the tilts,
    // in order — a tilt dropped from the fetch list never arrives, and a
    // key fetched but absent from the tilt list is never drawn.
    assert_eq!(SRM_FETCH_PRODUCTS[0], STORM_MOTION_PRODUCT);
    assert_eq!(SRM_FETCH_PRODUCTS[1..], SRM_TILT_PRODUCTS);
}

/// The lowest tilt derives from the same product 154 as `N1G`, at the same
/// 0.25 km, and honours a vector the same way. Built from the real `N0G`
/// PDB halfwords, so a 0.5° special case anywhere in `derive` shows up as
/// a disagreement with 1.3° rather than as a silently coarser pane.
#[test]
fn the_lowest_tilt_derives_exactly_as_the_ones_above_it() {
    // 0.5° cut 1 and 1.3° cut 3, the elevation numbers `TLX` really
    // publishes, over the identical field and vector.
    let low = uniform_at(154, 5, 1, &[89.5], 1.0, 10.0);
    let high = uniform_at(154, 13, 3, &[89.5], 1.0, 10.0);
    let s = sample(30.0, 90.0, 7108);
    let d0 = derive(&low, &s).expect("N0G is product 154");
    let d1 = derive(&high, &s).expect("N1G is product 154");

    assert_eq!(d0.elevation_angle, 0.5);
    assert_eq!(d1.elevation_angle, 1.3);
    assert_eq!(
        d0.packet.radials[0].gate_values,
        d1.packet.radials[0].gate_values
    );
    assert!(
        (d0.packet.gate_interval_km() - 0.25).abs() < 1e-9,
        "0.5° is 0.25 km"
    );
    assert_eq!(d0.scale, d1.scale);
    assert_eq!(d0.offset, d1.offset);
    // 10 m/s is 19.4 kt, and azimuth 090 takes the full +30 kt.
    assert!(
        (knots_at(&d0, 0, 0) - (19.438 + 30.0)).abs() < 0.5,
        "got {}",
        knots_at(&d0, 0, 0)
    );
}

/// The vector cannot come off `N0G`: halfword 51 is the BZ2 compression
/// flag there, exactly as on `N1G`.
#[test]
fn the_lowest_tilts_source_carries_no_vector_of_its_own() {
    let low = uniform_at(154, 5, 1, &[0.0], 0.5, 0.0);
    assert!(
        StormMotionSample::from_message(&low).is_none(),
        "N0G reported a vector — halfword 51 is its compression flag, and \
             reading it yields 0.1 kt from 1.3°",
    );
}

/// The quantiser's bins, checked against the boundaries a real `N0S`
/// declares. Each edge is exercised from both sides — a `<=` for a `<`
/// moves every boundary gate by one level.
#[test]
fn the_rpg_level_bins_run_from_below_minus_64_to_above_64() {
    assert_eq!(quantize_to_rpg_levels(-100.0), 1);
    assert_eq!(quantize_to_rpg_levels(-64.1), 1);
    assert_eq!(
        quantize_to_rpg_levels(-64.0),
        2,
        "the edge belongs to the bin above"
    );
    assert_eq!(quantize_to_rpg_levels(-50.1), 2);
    assert_eq!(quantize_to_rpg_levels(-50.0), 3);
    assert_eq!(quantize_to_rpg_levels(-0.1), 7, "just negative");
    assert_eq!(quantize_to_rpg_levels(0.0), 8, "zero reads positive");
    assert_eq!(quantize_to_rpg_levels(9.9), 8);
    assert_eq!(quantize_to_rpg_levels(10.0), 9);
    assert_eq!(quantize_to_rpg_levels(63.9), 13);
    assert_eq!(quantize_to_rpg_levels(64.0), 14);
    assert_eq!(quantize_to_rpg_levels(200.0), 14);
    // Monotone, and every one of the 14 levels reachable.
    let mut seen = std::collections::BTreeSet::new();
    let mut last = 0;
    for i in -2000..2000 {
        let l = quantize_to_rpg_levels(i as f32 / 10.0);
        assert!(l >= last, "not monotone at {}", i as f32 / 10.0);
        last = l;
        seen.insert(l);
    }
    assert_eq!(seen.len(), 14, "reached {seen:?}");
}

/// The worst case the settings dialog admits must survive the encoding.
///
/// A clamped gate is still ≥ 2, so saturation does not drop out — it paints
/// at the clamp, which reads as a real -199 kt inbound rather than as
/// missing data. The encoding therefore has to cover the input range, and
/// the input range is set by the widget, not by meteorology.
#[test]
fn the_largest_vector_the_ui_permits_cannot_saturate_the_encoding() {
    // The radial centre is 90.0°, so a vector from 270° subtracts its full
    // speed and one from 090° adds it. Gate 2 is the source's floor
    // (-63.5 m/s = -123.4 kt), gate 255 its ceiling (+63.0 m/s = +122.4 kt).
    for (gate, direction, want) in [
        (2u16, 270.0f32, -123.4 - MAX_OVERRIDE_SPEED_KT as f64),
        (255, 90.0, 122.4 + MAX_OVERRIDE_SPEED_KT as f64),
    ] {
        let radials = vec![RadialRun {
            start_angle: 89.5,
            angle_delta: 1.0,
            gate_values: vec![gate],
        }];
        let msg = message(velocity_pdb(154, 13, 9, 7108), radials);
        let s = StormMotionSample::user_override(MAX_OVERRIDE_SPEED_KT, direction)
            .expect("the UI maximum is finite");
        let d = derive(&msg, &s).expect("154 is a velocity source");
        let raw = d.packet.radials[0].gate_values[0];
        assert!(raw > FIRST_DATA_GATE, "gate {gate} clamped to the floor");
        assert!(raw < u16::MAX, "gate {gate} clamped to the ceiling");
        // The value must come back intact, not at the clamp.
        let got = knots_at(&d, 0, 0) as f64;
        assert!(
            (got - want).abs() < 1.0,
            "gate {gate} from {direction}°: got {got:.1} kt, want {want:.1} kt",
        );
    }
}

/// The derived scale must not be coarser than the source's, or the
/// requantisation adds error of its own. 0.5 kt per step against the
/// source's 0.5 m/s (0.97 kt).
#[test]
fn the_derived_scale_is_finer_than_the_source_step() {
    let source_step_kt = 0.5 * MS_TO_KT;
    assert!(1.0 / DERIVED_SCALE as f64 <= source_step_kt);
    // Round-tripping every source level must be exact to well under a step.
    let msg = uniform(154, &[0.0], 0.5, 0.0);
    for gate in 2u16..=255 {
        let want = (gate as f64 - 129.0) * 0.5 * MS_TO_KT;
        let radials = vec![RadialRun {
            start_angle: 0.0,
            angle_delta: 0.5,
            gate_values: vec![gate],
        }];
        let m = message(msg.pdb.clone(), radials);
        let d = derive(&m, &sample(0.0, 0.0, 7108)).unwrap();
        assert!(
            (knots_at(&d, 0, 0) as f64 - want).abs() <= 0.25,
            "gate {gate}: {} vs {want}",
            knots_at(&d, 0, 0),
        );
    }
}
