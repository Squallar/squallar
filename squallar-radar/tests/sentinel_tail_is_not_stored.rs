//! **A ray's trailing run of below-threshold gates is not stored, and nothing
//! downstream can tell.**
//!
//! `MomentData::from_fixed_point_dropping_sentinel_tail` keeps the gates up to
//! the last one that measured something and records how many sentinel gates it
//! dropped. `raw_gate_values` restores them, so `iter`, `values` and every
//! consumer below them read the sequence the decoder read.
//!
//! MEASURED over 100 archived volumes, 8 sites, 5 VCPs: the trailing run is
//! 59.8 % of gate bytes on the precipitation VCP, 73.7 % on clear air, and no
//! volume in the corpus was under 41.8 %. Gate buffers are 95.9 % of a decoded
//! volume.
//!
//! **What each test here is for**, because "it is lossless" is the whole claim
//! and one identity assertion would not carry it:
//!
//! * the decoded sequence is identical — the claim itself, on both a scaled
//!   moment and a `scale == 0.0` one, where raw `0` is an ordinary value and
//!   not a status code;
//! * the counter reports the exact bytes dropped — a fires-counter, since a
//!   byte *level* cannot tell a small volume from a truncated one;
//! * a 16-bit gate of `0x0001` is a MEASUREMENT and stops the truncation — the
//!   trap a byte-wise scan for zeroes falls into;
//! * `moment_value_at`, the one caller that indexes the bytes rather than
//!   iterating them, answers the sentinel inside the restored tail and `None`
//!   past it — so a block that merely declares more gates than it carries goes
//!   on reading `BeyondRange`;
//! * a gate-RELEASED block still yields nothing, which is what
//!   `squallar_radar::skeleton`'s four silent readers are written against;
//! * and the cut fires on a real archived message, not just on constructions.

use nexrad_model::data::{
    ChannelConfiguration, DataMoment, ElevationCut, MomentData, MomentValue, PulseWidth, Radial,
    RadialStatus, Scan, Sweep, VolumeCoveragePattern, WaveformType,
};
use squallar_radar::render_input::RenderInput;
use squallar_radar::types::RadarProduct;

/// Gates 0..8 measure, then a sentinel tail out to 64 declared gates.
fn scaled_case() -> (Vec<u8>, u16) {
    let mut v = vec![0u8; 64];
    for (i, b) in v.iter_mut().enumerate().take(8) {
        *b = (i as u8) + 2;
    }
    // A range-folded gate inside the data: raw 1 is a measurement, not a tail.
    v[8] = 1;
    (v, 64)
}

fn build(values: Vec<u8>, gates: u16, word: u8, scale: f32) -> (MomentData, MomentData) {
    let whole = MomentData::from_fixed_point(gates, 0, 250, word, scale, 66.0, values.clone());
    let cut = MomentData::from_fixed_point_dropping_sentinel_tail(
        gates, 0, 250, word, scale, 66.0, values,
    );
    (whole, cut)
}

#[test]
fn the_decoded_gate_sequence_is_identical_with_the_tail_dropped() {
    for scale in [2.0f32, 0.0f32] {
        let (values, gates) = scaled_case();
        let (whole, cut) = build(values, gates, 8, scale);

        assert!(
            cut.raw_values().len() < whole.raw_values().len(),
            "scale {scale}: nothing was dropped, so this test proves nothing"
        );
        assert_eq!(
            cut.raw_values().len(),
            9,
            "scale {scale}: kept the wrong prefix"
        );
        assert_eq!(cut.trailing_sentinel_gates(), 64 - 9);
        assert_eq!(cut.gates_present(), 64, "scale {scale}");

        // The claim.
        assert_eq!(
            whole.values(),
            cut.values(),
            "scale {scale}: the decoded sequence moved"
        );
        assert_eq!(whole.values().len(), 64, "scale {scale}");
        // And the tail decodes to what the raw zero means at this scale, which
        // differs between the two scales — so this is not a vacuous compare.
        let expected_tail = if scale == 0.0 {
            MomentValue::Value(0.0)
        } else {
            MomentValue::BelowThreshold
        };
        assert_eq!(cut.values()[63], expected_tail, "scale {scale}");
    }
}

#[test]
fn the_counter_reports_exactly_the_bytes_that_were_not_stored() {
    let (values, gates) = scaled_case();
    let (whole, cut) = build(values, gates, 8, 2.0);
    let dropped = squallar_radar::scan_size::gate_bytes_dropped(&cut);
    assert_eq!(dropped, 55, "64 declared gates, 9 kept, one byte each");
    assert_eq!(
        squallar_radar::scan_size::gate_bytes_dropped(&whole),
        0,
        "an untruncated block must report no saving, or the counter reads as a \
         saving on every volume including the ones the cut never touched"
    );
    // The saving is real bytes, not just a smaller declared number.
    assert!(
        squallar_radar::scan_size::gate_bytes(&cut) < squallar_radar::scan_size::gate_bytes(&whole),
        "the byte census did not see the drop"
    );
}

#[test]
fn a_sixteen_bit_gate_whose_low_byte_is_set_is_a_measurement_and_stops_the_drop() {
    // **Two gates, each defeating a different wrong scan**, because a scan that
    // walks bytes instead of 16-bit words agrees with the right answer on most
    // inputs and only diverges on these:
    //
    // * gate 3 = `0x0001` — range folded, nonzero LOW byte only. A byte-wise
    //   scan that rounded its answer UP would keep an odd prefix and split a
    //   gate in half.
    // * gate 5 = `0x0200` — the LAST data gate, nonzero HIGH byte only. Its low
    //   byte is zero, so a byte-wise scan finds byte 10 and computes
    //   `(10 + 1) / 2 = 5` gates, dropping gate 5 itself and turning a real
    //   measurement of 512 into a sentinel. This is the case that makes the
    //   losslessness assertion below bite.
    let mut v = vec![0u8; 20]; // 10 gates at 16 bits
    v[6] = 0x00;
    v[7] = 0x01;
    v[10] = 0x02;
    v[11] = 0x00;
    let (whole, cut) = build(v, 10, 16, 2.0);
    assert_eq!(
        cut.raw_values().len(),
        12,
        "kept a partial gate or the wrong prefix"
    );
    assert_eq!(cut.raw_values().len() % 2, 0, "kept half of a 16-bit gate");
    assert_eq!(cut.trailing_sentinel_gates(), 4);
    assert_eq!(squallar_radar::scan_size::gate_bytes_dropped(&cut), 8);
    assert_eq!(whole.values(), cut.values(), "the decoded sequence moved");
    assert_eq!(cut.values()[3], MomentValue::RangeFolded, "lost the fold");
    // The gate a byte-wise scan would have eaten.
    assert_eq!(
        cut.values()[5],
        MomentValue::Value((512.0 - 66.0) / 2.0),
        "the last data gate was dropped as though it were sentinel"
    );
}

#[test]
fn an_all_sentinel_ray_stores_nothing_and_is_charged_nothing() {
    let (whole, cut) = build(vec![0u8; 32], 32, 8, 2.0);
    assert!(
        cut.raw_values().is_empty(),
        "kept bytes for a ray with no echo"
    );
    assert_eq!(cut.trailing_sentinel_gates(), 32);
    assert_eq!(whole.values(), cut.values(), "the decoded sequence moved");
    // Routed through the one process-wide empty buffer, so the census's
    // `len == 0` arm (0 bytes, 0 blocks) is telling the truth about it.
    assert_eq!(
        squallar_radar::scan_size::gate_bytes_and_blocks(&cut),
        (0, 0),
        "a fresh zero-length Arc was allocated and charged as nothing"
    );
}

#[test]
fn indexing_past_the_stored_bytes_reads_the_sentinel_inside_the_tail_and_nothing_past_it() {
    let (values, gates) = scaled_case();
    let (_whole, cut) = build(values, gates, 8, 2.0);
    // Inside the restored tail.
    assert_eq!(
        squallar_radar::render::moment_value_at(&cut, 40),
        Some(MomentValue::BelowThreshold),
        "a dropped gate read as absent rather than as below threshold"
    );
    assert_eq!(
        squallar_radar::render::moment_value_at(&cut, 63),
        Some(MomentValue::BelowThreshold)
    );
    // Past every gate the moment has.
    assert_eq!(squallar_radar::render::moment_value_at(&cut, 64), None);

    // **The distinction the new field exists for.** A block that merely
    // declares more gates than it carries has UNKNOWN gates past its bytes,
    // not sentinel ones, and must still read as beyond range.
    let malformed = MomentData::from_fixed_point(400, 0, 250, 8, 2.0, 66.0, vec![9u8; 50]);
    assert_eq!(malformed.trailing_sentinel_gates(), 0);
    assert_eq!(malformed.gates_present(), 50);
    assert_eq!(
        squallar_radar::render::moment_value_at(&malformed, 50),
        None,
        "an unstored-and-unknown gate was invented as below threshold"
    );
}

#[test]
fn a_gate_released_block_still_yields_nothing() {
    let (values, gates) = scaled_case();
    let (_whole, cut) = build(values, gates, 8, 2.0);
    let released = cut.without_values();
    assert!(released.raw_values().is_empty());
    assert_eq!(
        released.trailing_sentinel_gates(),
        0,
        "a released block claimed a restorable tail, which would turn a \
         gate-released volume into a fully below-threshold one"
    );
    assert_eq!(
        released.values().len(),
        0,
        "a released block yielded gates, which is the blank-picture-reported-\
         as-success hazard skeleton exists to prevent"
    );
    assert_eq!(released.gate_count(), 64, "the scalar was not preserved");
}

/// The cut fires on a real archived message. The two committed first-messages
/// are a real archive cut to its first Message 31, header verbatim.
#[test]
fn the_cut_fires_on_a_real_archived_message() {
    const FIXTURES: [(&str, &[u8]); 2] = [
        (
            "KTLX20260811_000049_V06",
            include_bytes!("../testdata/KTLX20260811_000049_V06.first-message"),
        ),
        (
            "KAMX20200810_000424_V06",
            include_bytes!("../testdata/KAMX20200810_000424_V06.first-message"),
        ),
    ];
    for (name, bytes) in FIXTURES {
        let contents = squallar_radar::chunks::decode_chunk(name, bytes)
            .unwrap_or_else(|e| panic!("decoding {name}: {e}"));
        assert!(!contents.radials.is_empty(), "{name} decoded no radials");
        let mut dropped = 0usize;
        let mut stored = 0usize;
        for radial in &contents.radials {
            for moment in [
                radial.reflectivity(),
                radial.velocity(),
                radial.spectrum_width(),
                radial.differential_reflectivity(),
                radial.differential_phase(),
                radial.correlation_coefficient(),
            ]
            .into_iter()
            .flatten()
            {
                dropped += squallar_radar::scan_size::gate_bytes_dropped(moment);
                stored += moment.raw_values().len();
            }
        }
        // A zero here is the scar this counter exists for: a cut whose
        // precondition never held on the arm it shipped to, reading 0 on every
        // tick while nobody looked.
        assert!(
            dropped > 0,
            "{name}: the cut did not fire on a real message"
        );
        // **The corpus band does not apply here, and the denominator is why.**
        // A first-message fixture is ONE radial of the LOWEST elevation cut,
        // and both of those push the fraction down: the 0.5 degree cut is the
        // one where echo reaches furthest (55.9 % trailing, byte-weighted over
        // 280 such sweeps, against 83.7 % at 2.1-4.0 degrees), and a single
        // azimuth pointed into a storm core is one draw rather than a
        // distribution. KTLX reads 0.160 here for exactly that reason and
        // nothing is wrong with it.
        //
        // So what this asserts is the FIRES claim plus a plausibility floor,
        // not the corpus figure. The corpus figure is asserted where it can be:
        // over whole volumes, by the tests above on constructed moments whose
        // tails are known exactly.
        let frac = dropped as f64 / (dropped + stored) as f64;
        assert!(
            (0.02..0.98).contains(&frac),
            "{name}: dropped fraction {frac:.3} is not a plausible reading for \
             one lowest-tilt radial"
        );
    }
}

/// **The composition with the shared-buffer round trip.**
///
/// `render_input` takes a moment apart into scalars plus a `GateBuffer` and puts
/// it back together, and since `80ddbbbe8` the buffer is adopted by REFERENCE
/// COUNT in both directions rather than copied. So a truncated moment arrives at
/// the far end SHORT, through a path that never allocated it. The two cuts are
/// complementary only if the restore count travels with the buffer, and this is
/// the gate that says it does.
///
/// **Why this needed a gate rather than a reading of the diff.** If the count
/// did not travel, `to_moment_data` would rebuild the moment with no tail and it
/// would answer for `keep` gates instead of `gate_count` — and it would be
/// SILENT, because `render::plane` sizes the code plane at `gate_count()` and
/// prefills the same below-threshold code, so the picture would look correct
/// while `velocity::grid` and the volumetric status plane read gates that were
/// not there. Nothing about the rendered raster would move.
///
/// The round trip is the public one end to end: `extract_volume` (payload out,
/// buffer shared), `to_bytes`/`from_bytes` (the positional wire, whose
/// `FORMAT_VERSION` moved with the new field so a stale reader refuses instead
/// of reading the count as half of a length), and `to_scan` (payload back in
/// through `from_gate_buffer_with_sentinel_tail`).
#[test]
fn a_truncated_moment_survives_the_shared_buffer_round_trip_gate_for_gate() {
    const RADIALS: usize = 8;
    const GATES: usize = 1832;
    /// Gates that measure. The rest of the ray is the sentinel tail under test.
    const ECHO: usize = 300;

    let radial = |index: usize| -> Radial {
        let spacing = 360.0 / RADIALS as f32;
        let mut refl = vec![0u8; GATES];
        for (g, b) in refl.iter_mut().enumerate().take(ECHO) {
            *b = ((index + g) % 254 + 2) as u8;
        }
        Radial::new(
            0,
            index as u16,
            index as f32 * spacing,
            spacing,
            RadialStatus::IntermediateRadialData,
            1,
            0.5,
            Some(MomentData::from_fixed_point_dropping_sentinel_tail(
                GATES as u16,
                2125,
                250,
                8,
                2.0,
                66.0,
                refl,
            )),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    };
    let cut = ElevationCut::new(
        0.5,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        20.0,
        true,
        true,
        false,
        false,
        1,
        1,
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
    );
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
            vec![cut],
        ),
        vec![Sweep::new(1, (0..RADIALS).map(radial).collect())],
    );

    // The cut must be present in the INPUT, or the round trip is being checked
    // on data with no tail to lose and would pass for the wrong reason.
    let dropped_before = squallar_radar::scan_size::scan_gate_bytes_dropped(&scan);
    assert_eq!(
        dropped_before,
        RADIALS * (GATES - ECHO),
        "the fixture did not drop the tail this test exists to round-trip"
    );

    let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, 35.3333, -97.2778)
        .expect("reflectivity extracts");
    let round_tripped = RenderInput::from_bytes(&input.to_bytes())
        .expect("the payload survives its own wire")
        .to_scan();

    // Gate for gate, through the decoded sequence every consumer reads.
    let gates_of = |s: &Scan| -> Vec<Vec<MomentValue>> {
        s.sweeps()
            .iter()
            .flat_map(|sweep| sweep.radials())
            .filter_map(|r| r.reflectivity().map(|m| m.values()))
            .collect()
    };
    let before = gates_of(&scan);
    let after = gates_of(&round_tripped);
    assert_eq!(before.len(), RADIALS, "no reflectivity to compare");
    assert_eq!(after.len(), RADIALS, "the round trip lost or gained rays");
    assert!(
        before.iter().all(|r| r.len() == GATES),
        "the input did not reconstruct its own tail"
    );
    assert_eq!(
        before, after,
        "the shared-buffer round trip did not reconstruct the dropped tail"
    );

    // And the tail is still DROPPED on the far side, not rematerialised into
    // stored bytes — neither cut may undo the other.
    assert_eq!(
        squallar_radar::scan_size::scan_gate_bytes_dropped(&round_tripped),
        dropped_before,
        "the round trip changed how many gate bytes are unstored"
    );
}
