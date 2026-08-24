use super::*;
use nexrad_level3::model::{
    DataLayer, DataPacket, MessageHeader, ProductDescriptionBlock, RadialRun, SymbologyBlock,
};

/// NEXRAD float16 for 1.0 and 2.0 (sign 0, exponent 16/17, fraction 0) —
/// the encoding [`crate::l3_values::nexrad_float16`] decodes.
const F16_ONE: u16 = 16 << 10;
const F16_TWO: u16 = 17 << 10;

fn dvl_thresholds() -> [u16; 16] {
    let mut t = [0u16; 16];
    t[0] = F16_ONE; // lin_scale
    t[1] = F16_TWO; // lin_offset
    t[2] = 255; // log_start — past the table, so the whole LUT is linear
    t[3] = F16_ONE; // log_scale  (unreached)
    t[4] = F16_ONE; // log_offset (unreached)
    t
}

fn eet_thresholds() -> [u16; 16] {
    let mut t = [0u16; 16];
    t[0] = 127;
    t[1] = 1;
    t[2] = 2;
    t[3] = 128;
    t
}

/// The level product 134 encodes `kg_m2` as, under [`dvl_thresholds`].
fn dvl_level(kg_m2: u16) -> u16 {
    kg_m2 + 2
}

/// The level product 135 encodes a published `kft` as, under
/// [`eet_thresholds`]: `⌊kft⌋ + 2`, with bit 7 set when `topped`.
fn eet_level(kft: u16, topped: bool) -> u16 {
    kft + 2 + if topped { 128 } else { 0 }
}

fn message(
    product_code: i16,
    thresholds: [u16; 16],
    volume_scan_date: u16,
    volume_scan_time: u32,
    scale_factor: f32,
    gates_at: impl Fn(usize) -> Vec<u16>,
) -> Level3Message {
    let pdb = ProductDescriptionBlock {
        block_divider: -1,
        latitude: 35.3333,
        longitude: -97.2778,
        height: 1200,
        product_code,
        operational_mode: 2,
        vcp: 212,
        sequence_number: 0,
        volume_scan_number: 39,
        volume_scan_date,
        volume_scan_time,
        generation_date: volume_scan_date,
        generation_time: volume_scan_time,
        product_specific_1: 0,
        product_specific_2: 0,
        elevation_number: 0,
        product_specific_3: 0,
        thresholds,
        product_specific_47_53: [0; 7],
        version: 0,
        spot_blank: 0,
        symbology_offset: 60,
        graphic_offset: 0,
        tabular_offset: 0,
    };
    let radials: Vec<RadialRun> = (0..360)
        .map(|az| RadialRun {
            start_angle: az as f32,
            angle_delta: 1.0,
            gate_values: gates_at(az),
        })
        .collect();
    let num_range_bins = radials
        .iter()
        .map(|r| r.gate_values.len())
        .max()
        .unwrap_or(0) as u16;
    Level3Message {
        header: MessageHeader {
            message_code: product_code,
            date_of_message: volume_scan_date,
            time_of_message: volume_scan_time,
            message_length: 0,
            source_id: 0,
            destination_id: 0,
            number_of_blocks: 3,
        },
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
                    scale_factor,
                    is_legacy: false,
                    xdr_data_scale: None,
                    xdr_data_offset: None,
                    radials,
                })],
            }],
        }),
    }
}

/// A volume start both fixtures share, MJD day 20661 at 01:58:28Z.
const VOL_DATE: u16 = 20661;
const VOL_TIME: u32 = 7108;

/// Digital VIL, 1 km gates, `kg_m2_at` kg/m² per azimuth (`None` =
/// below-threshold level 0), over `bins` gates.
fn dvl(
    volume_scan_time: u32,
    bins: usize,
    kg_m2_at: impl Fn(usize) -> Option<u16>,
) -> Level3Message {
    message(
        DVL_PRODUCT_CODE,
        dvl_thresholds(),
        VOL_DATE,
        volume_scan_time,
        1.0,
        move |az| vec![kg_m2_at(az).map_or(0, dvl_level); bins],
    )
}

/// Enhanced Echo Tops, 1 km gates, published `kft_at` kft per azimuth
/// (`None` = below-threshold level 0), over `bins` gates.
fn eet(volume_scan_time: u32, bins: usize, kft_at: impl Fn(usize) -> Option<u16>) -> Level3Message {
    message(
        EET_PRODUCT_CODE,
        eet_thresholds(),
        VOL_DATE,
        volume_scan_time,
        1.0,
        move |az| vec![kft_at(az).map_or(0, |kft| eet_level(kft, false)); bins],
    )
}

fn golden_pair() -> (Level3Message, Level3Message) {
    let dvl_at = |az: usize| match az {
        10 => Some(35),
        11 => Some(34),
        12 => Some(0),
        13 => Some(20),
        15 => Some(35),
        _ => None,
    };
    let eet_at = |az: usize| match az {
        10 | 11 => Some(32),
        12 => Some(32),
        14 => Some(40),
        15 => Some(0),
        _ => None,
    };
    (dvl(VOL_TIME, 60, dvl_at), eet(VOL_TIME, 60, eet_at))
}

#[test]
fn the_two_published_products_divide_to_hand_computed_vil_density() {
    let (num, den) = golden_pair();
    let grid = compute_vild(&num, &den).expect("a paired 134/135 pair renders");
    assert_eq!(grid.range_bins, RANGE_BINS);
    assert_eq!(grid.values.len(), 360);
    assert_eq!(grid.values[0].len(), RANGE_BINS);

    let r = 30;
    assert!(
        (grid.values[10][r] - 3.533_212).abs() < 1e-5,
        "got {}",
        grid.values[10][r],
    );
    assert!(grid.values[10][r] >= 3.5, "the 3.5 break must be crossed");
    assert!(
        (grid.values[11][r] - 3.432_263).abs() < 1e-5,
        "got {}",
        grid.values[11][r],
    );
    assert!(grid.values[11][r] < 3.5);
    assert_eq!(grid.values[12][r], 0.0, "a defined zero, not undefined");
}

/// Amburn & Wolf's own formula against hand-computed pairs: 20 kg/m² over
/// a 10 km top (32.8084 kft = 10,000 m) is exactly 2.0 g/m³, and 35 kg/m²
/// over the same top is their 3.5 g/m³ severe-hail break.
#[test]
fn the_arithmetic_reproduces_the_amburn_wolf_pairs() {
    assert!((vild_g_m3(20.0, 32.8084) - 2.0).abs() < 1e-5);
    assert!((vild_g_m3(35.0, 32.8084) - 3.5).abs() < 1e-5);
    assert!((vild_g_m3(1.0, 1.0) - 3.280_84).abs() < 1e-4);
    assert!((vild_from_published(20.0, 32.3084) - 2.0).abs() < 1e-5);
    // A defined 0.0 kg/m² column is a defined 0.0 g/m³, not undefined.
    assert_eq!(vild_from_published(0.0, 32.0), 0.0);
}

#[test]
fn the_bin_centre_datum_is_what_the_quotient_divides_by() {
    let centre = vild_from_published(35.0, 32.0);
    assert!((centre - 3.533_212).abs() < 1e-5, "got {centre}");
    let floor = vild_g_m3(35.0, 32.0);
    assert!((floor - 3.588_419).abs() < 1e-5, "got {floor}");
    assert!(
        (f64::from(floor / centre) - 32.5 / 32.0).abs() < 1e-6,
        "the floor datum's bias is exactly half a bin",
    );

    let straddle_centre = vild_from_published(34.4, 32.0);
    let straddle_floor = vild_g_m3(34.4, 32.0);
    assert!((straddle_centre - 3.472_643).abs() < 1e-5);
    assert!((straddle_floor - 3.526_903).abs() < 1e-5);
    assert!(
        straddle_centre < 3.5 && straddle_floor >= 3.5,
        "the datum decides the break: {straddle_centre} vs {straddle_floor}",
    );

    let (num, den) = golden_pair();
    let grid = compute_vild(&num, &den).expect("renders");
    assert!(
        (grid.values[10][30] - vild_from_published(35.0, 32.0)).abs() < 1e-6,
        "the grid divides by something other than the bin centre",
    );
    assert!(
        (grid.values[10][30] - vild_g_m3(35.0, 32.0)).abs() > 0.05,
        "a floor-datum grid would be indistinguishable — the pin is vacuous",
    );

    let tops = published_top_field(&[vec![0.0, 32.0, f32::NAN, -1.0, 69.0]]);
    assert!(tops[0][0].is_nan(), "a 0 kft top has no usable quotient");
    assert_eq!(tops[0][1], 32.5);
    assert!(tops[0][2].is_nan());
    assert!(tops[0][3].is_nan());
    assert_eq!(tops[0][4], 69.5);
}

/// A cell is defined only where **both** inputs are: a DVL column with no
/// echo top, an echo top with no DVL, and a cell with neither are all
/// `NaN` — never 0, which the palette would paint.
#[test]
fn a_cell_is_undefined_wherever_either_input_is() {
    let (num, den) = golden_pair();
    let grid = compute_vild(&num, &den).expect("renders");
    let r = 30;
    assert!(
        grid.values[13][r].is_nan(),
        "DVL defined but no echo top: got {}",
        grid.values[13][r],
    );
    assert!(
        grid.values[14][r].is_nan(),
        "an echo top with no DVL: got {}",
        grid.values[14][r],
    );
    assert!(grid.values[16][r].is_nan(), "neither input");

    // The scalar agrees, in every combination.
    assert!(vild_from_published(f32::NAN, 32.0).is_nan());
    assert!(vild_from_published(35.0, f32::NAN).is_nan());
    assert!(vild_from_published(f32::NAN, f32::NAN).is_nan());
    assert!(vild_from_published(f32::INFINITY, 32.0).is_nan());
    assert!(vild_g_m3(20.0, f32::NAN).is_nan());
}

#[test]
fn a_zero_published_echo_top_leaves_the_cell_undefined() {
    let (num, den) = golden_pair();
    let grid = compute_vild(&num, &den).expect("renders");
    assert!(
        grid.values[15][30].is_nan(),
        "35 kg/m² over a 0 kft top: got {}",
        grid.values[15][30],
    );

    let topped = message(
        EET_PRODUCT_CODE,
        eet_thresholds(),
        VOL_DATE,
        VOL_TIME,
        1.0,
        |az| {
            let level = match az {
                10 => eet_level(0, true),
                11 => eet_level(32, true),
                _ => 0,
            };
            vec![level; 60]
        },
    );
    let grid = compute_vild(&num, &topped).expect("renders");
    assert!(grid.values[10][30].is_nan(), "a topped 0 kft top");
    assert!(
        (grid.values[11][30] - vild_from_published(34.0, 32.0)).abs() < 1e-6,
        "a topped 32 kft top is an ordinary cell: got {}",
        grid.values[11][30],
    );

    assert!(vild_from_published(35.0, 0.0).is_nan());
    assert!(vild_from_published(35.0, -1.0).is_nan());
    assert!(vild_g_m3(20.0, 0.0).is_nan(), "a zero top divides");
}

/// Volume pairing is mandatory: a DVL from one volume beside an EET from
/// the next is **refused**, not painted. A ratio of two volumes is a
/// plausible field of a storm that never existed.
#[test]
fn a_volume_mismatch_refuses_to_render() {
    let (num, den) = golden_pair();
    assert!(compute_vild(&num, &den).is_ok(), "the paired case renders");

    // The next volume, four minutes later.
    let later = eet(VOL_TIME + 240, 60, |az| (az == 10).then_some(32));
    match compute_vild(&num, &later) {
        Err(Refusal::VolumeMismatch { dvl: a, eet: b }) => {
            assert_ne!(a, b, "the two starts must differ");
        }
        Err(other) => panic!("wrong refusal: {other:?}"),
        Ok(_) => panic!("a mismatched pair must be refused, not painted"),
    }

    let jittered = eet(VOL_TIME + 30, 60, |az| (az == 10).then_some(32));
    assert!(
        compute_vild(&num, &jittered).is_ok(),
        "{VOLUME_PAIRING_TOLERANCE_SECS} s of jitter is one volume",
    );
    let past = eet(VOL_TIME + 61, 60, |az| (az == 10).then_some(32));
    assert!(compute_vild(&num, &past).is_err(), "one second past it");

    let unreadable = message(EET_PRODUCT_CODE, eet_thresholds(), 0, VOL_TIME, 1.0, |_| {
        vec![eet_level(32, false); 60]
    });
    assert_eq!(volume_scan_started(&unreadable.pdb), None);
    assert!(matches!(
        compute_vild(&num, &unreadable),
        Err(Refusal::VolumeMismatch { eet: None, .. }),
    ));

    assert!(volumes_pair(
        volume_scan_started(&num.pdb),
        volume_scan_started(&den.pdb),
    ));
    assert!(!volumes_pair(None, None), "two unknowns are not a pair");
    assert!(!volumes_pair(volume_scan_started(&num.pdb), None));
}

/// The two products are not interchangeable: swapping them would divide
/// kilofeet by kilograms and the palette would paint the result.
#[test]
fn only_a_134_over_135_pair_renders() {
    let (num, den) = golden_pair();
    assert_eq!(
        compute_vild(&den, &num).err(),
        Some(Refusal::WrongProduct {
            dvl: EET_PRODUCT_CODE,
            eet: DVL_PRODUCT_CODE,
        }),
        "the two products are not interchangeable",
    );
    assert!(matches!(
        compute_vild(&num, &num).err(),
        Some(Refusal::WrongProduct { .. }),
    ));

    // A message with no symbology carries no radial packet.
    let mut empty = den.clone();
    empty.symbology = None;
    assert_eq!(
        compute_vild(&num, &empty).err(),
        Some(Refusal::NoRadialData),
    );
}

fn a_finer_gated_denominator_resamples_onto_the_1_km_cells() -> Level3Message {
    message(
        EET_PRODUCT_CODE,
        eet_thresholds(),
        VOL_DATE,
        VOL_TIME,
        4.0,
        |az| {
            let mut gates = vec![0u16; 12];
            if az == 10 {
                gates[1] = eet_level(32, false); // cell 0
                gates[5] = eet_level(16, false); // cell 1
            }
            gates
        },
    )
}

/// The grid is defined only where both packets reach, and never past the
/// 230 km display cap — and the two products' gate spacings and range
/// extents do not have to match.
#[test]
fn both_products_resample_onto_the_common_capped_grid() {
    let short_eet = eet(VOL_TIME, 40, |az| (az == 10).then_some(32));
    let grid = compute_vild(
        &dvl(VOL_TIME, 60, |az| (az == 10).then_some(35)),
        &short_eet,
    )
    .expect("renders");
    assert!(grid.values[10][39].is_finite(), "inside both extents");
    assert!(
        grid.values[10][40].is_nan(),
        "past the EET's extent: got {}",
        grid.values[10][40],
    );
    assert!(grid.values[10][RANGE_BINS - 1].is_nan(), "past both");

    // Differing spacings: a 0.25 km-gated EET against a 1 km DVL.
    let grid = compute_vild(
        &dvl(VOL_TIME, 60, |az| (az == 10).then_some(35)),
        &a_finer_gated_denominator_resamples_onto_the_1_km_cells(),
    )
    .expect("renders");
    assert!(
        (grid.values[10][0] - vild_from_published(35.0, 32.0)).abs() < 1e-6,
        "cell 0 must read gate 1's 32 kft: got {}",
        grid.values[10][0],
    );
    assert!(
        (grid.values[10][1] - vild_from_published(35.0, 16.0)).abs() < 1e-6,
        "cell 1 must read gate 5's 16 kft: got {}",
        grid.values[10][1],
    );
    assert!(
        grid.values[10][2].is_nan(),
        "cell 2 reads gate 9, which is below threshold: got {}",
        grid.values[10][2],
    );
    assert!(grid.values[10][3].is_nan(), "past the finer packet's 3 km");
}

/// The product's own precision, hand-computed at VILD 3.5 g/m³ — the
/// table in the module doc, and the reason no tighter agreement than
/// ~±0.1 g/m³ is claimed anywhere.
#[test]
fn the_quantization_halfwidth_is_half_a_kilofoot_of_echo_top() {
    for (published, halfwidth) in [
        (15.0f32, 0.112_903f32),
        (20.0, 0.085_366),
        (30.0, 0.057_377),
        (40.0, 0.043_210),
        (50.0, 0.034_653),
    ] {
        let got = quantization_halfwidth_g_m3(3.5, published);
        assert!(
            (got - halfwidth).abs() < 1e-5,
            "at {published} kft: got {got}, hand-computed {halfwidth}",
        );
        // Relative, so it scales with the value itself.
        assert!((quantization_halfwidth_g_m3(7.0, published) - 2.0 * halfwidth).abs() < 1e-5);
    }
    assert_eq!(EET_QUANTUM_KFT, 1.0);
    assert_eq!(EET_BIN_CENTRE_KFT, 0.5);
}

#[test]
fn the_shipped_path_is_the_surveys_reference_construction() {
    let policy = |dvl: &Level3Message, eet: &Level3Message| {
        let dvl_packet = crate::srm::radial_packet(dvl).expect("packet");
        let eet_packet = crate::srm::radial_packet(eet).expect("packet");
        let dvl_codec = ValueCodec::for_message(dvl).expect("codec");
        let eet_codec = ValueCodec::for_message(eet).expect("codec");
        let dvl_field = resampled_field(
            dvl_packet,
            compare::gate_km(&dvl.pdb, dvl_packet),
            &dvl_codec,
        );
        let eet_field = resampled_field(
            eet_packet,
            compare::gate_km(&eet.pdb, eet_packet),
            &eet_codec,
        );
        density_field(&dvl_field, &published_top_field(&eet_field))
    };

    let (golden_dvl, golden_eet) = golden_pair();
    let mut total_finite = 0usize;
    for (label, dvl_msg, eet_msg) in [
        ("golden", golden_dvl, golden_eet),
        (
            "whole domain",
            dvl(VOL_TIME, 230, |az| Some((az % 60) as u16)),
            eet(VOL_TIME, 230, |az| Some((az % 50) as u16)),
        ),
        (
            "topped tops",
            dvl(VOL_TIME, 100, |az| (az % 3 == 0).then_some(45)),
            message(
                EET_PRODUCT_CODE,
                eet_thresholds(),
                VOL_DATE,
                VOL_TIME,
                1.0,
                |az| vec![eet_level((az % 40) as u16, az % 7 == 0); 100],
            ),
        ),
        (
            "finer denominator",
            dvl(VOL_TIME, 60, |az| Some((az % 45) as u16)),
            a_finer_gated_denominator_resamples_onto_the_1_km_cells(),
        ),
    ] {
        let shipped = compute_vild(&dvl_msg, &eet_msg)
            .unwrap_or_else(|e| panic!("{label}: shipped path refused: {e:?}"));
        let reference = policy(&dvl_msg, &eet_msg);
        assert_eq!(shipped.values.len(), reference.len(), "{label}");
        let mut finite = 0usize;
        for (az, (ours, theirs)) in shipped.values.iter().zip(&reference).enumerate() {
            assert_eq!(ours.len(), theirs.len(), "{label} az {az}");
            for (r, (&a, &b)) in ours.iter().zip(theirs).enumerate() {
                assert!(
                    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits(),
                    "{label} az {az} r {r}: shipped {a}, reference {b}",
                );
                finite += usize::from(a.is_finite());
            }
        }
        assert!(
            finite > 0,
            "{label}: no defined cells at all — the row compares nothing",
        );
        total_finite += finite;
    }
    assert!(
        total_finite > 50_000,
        "only {total_finite} defined cells pooled — the pin is too thin to catch \
             a re-datumed or re-resampled shipped path",
    );
}
