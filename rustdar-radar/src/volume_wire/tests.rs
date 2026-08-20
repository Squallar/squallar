use super::*;
use nexrad_model::data::{
    ChannelConfiguration, DataMoment, ElevationCut, MomentData, PulseWidth, VolumeCoveragePattern,
    WaveformType,
};
use nexrad_model::meta::Site;

const LAT: f32 = 39.7866;
const LON: f32 = -104.5458;

fn moment(scale: f32, offset: f32, gates: &[u8]) -> MomentData {
    MomentData::from_fixed_point(
        gates.len() as u16,
        125,
        250,
        8,
        scale,
        offset,
        gates.to_vec(),
    )
}

fn opinionated_pattern(angles: &[f64]) -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        212,
        13,
        0.25,
        PulseWidth::Short,
        true,
        3,
        true,
        2,
        true,
        true,
        4,
        true,
        true,
        angles
            .iter()
            .map(|angle| {
                ElevationCut::new(
                    *angle,
                    ChannelConfiguration::ConstantPhase,
                    WaveformType::CS,
                    21.2,
                    true,
                    true,
                    true,
                    true,
                    7,
                    15,
                    1.5,
                    2.5,
                    3.5,
                    4.5,
                    5.5,
                    6.5,
                    true,
                    1,
                    true,
                    2,
                    true,
                    true,
                )
            })
            .collect(),
    )
}

fn a_volume() -> DecodedScan {
    let mut sweeps = Vec::new();
    for (sweep_index, elevation_number) in [1u8, 2u8].into_iter().enumerate() {
        let radials = (0..4u16)
            .map(|i| {
                let has_dualpol = i % 2 == 0;
                Radial::new(
                    1_600_000_000_000 + i64::from(i) + (sweep_index as i64) * 1000,
                    i * 7,
                    f32::from(i) * 0.5,
                    0.5,
                    match i {
                        0 => RadialStatus::ElevationStart,
                        1 => RadialStatus::IntermediateRadialData,
                        2 => RadialStatus::ElevationEnd,
                        _ => RadialStatus::Unknown(77),
                    },
                    elevation_number,
                    0.5 + elevation_number as f32,
                    Some(moment(2.0, 66.0, &[10, 20, 30, i as u8])),
                    (i != 3).then(|| moment(2.0, 129.0, &[1, 2, 3])),
                    has_dualpol.then(|| moment(1.0, 0.0, &[4, 5])),
                    has_dualpol.then(|| moment(16.0, 128.0, &[6])),
                    has_dualpol.then(|| moment(2.8, 2.0, &[7, 8, 9])),
                    has_dualpol.then(|| moment(300.0, 0.0, &[11])),
                    // The seventh slot. Real WSR-88D volumes populate it on
                    // every radial.
                    (i != 1).then(|| {
                        nexrad_model::data::CFPMomentData::from_fixed_point(
                            2,
                            125,
                            250,
                            8,
                            1.0,
                            0.0,
                            vec![12, 13],
                        )
                    }),
                )
            })
            .collect();
        sweeps.push(Sweep::new(elevation_number, radials));
    }
    let scan = Scan::with_site(
        Site::new(*b"KFTG", LAT, LON, 1675, 20),
        opinionated_pattern(&[0.5, 0.9, 1.3]),
        sweeps,
    );
    let mut declared_nyquist = DeclaredNyquist::empty();
    declared_nyquist.declare(1, 32.0);
    declared_nyquist.declare(2, 27.5);
    // A second, different declaration for cut 1 — the one thing the far end
    // could not recompute.
    declared_nyquist.declare(1, 8.0);
    DecodedScan {
        scan,
        declared_nyquist,
    }
}

fn assert_same_moment(a: Option<&MomentData>, b: Option<&MomentData>, what: &str) {
    match (a, b) {
        (None, None) => {}
        (Some(a), Some(b)) => {
            assert_eq!(a.gate_count(), b.gate_count(), "{what} gate count");
            assert_eq!(a.raw_values(), b.raw_values(), "{what} gates");
            assert_eq!(a.scale(), b.scale(), "{what} scale");
            assert_eq!(a.offset(), b.offset(), "{what} offset");
            assert_eq!(a.data_word_size(), b.data_word_size(), "{what} word size");
            assert_eq!(
                a.gate_interval_km(),
                b.gate_interval_km(),
                "{what} gate interval"
            );
            assert_eq!(
                a.first_gate_range_km(),
                b.first_gate_range_km(),
                "{what} first gate"
            );
        }
        _ => panic!("{what}: one side had the moment and the other did not"),
    }
}

#[test]
fn a_volume_round_trips_every_radial_field_the_workspace_reads() {
    let original = a_volume();
    let bytes = original.to_bytes();
    let back = DecodedScan::from_bytes(&bytes).expect("a payload this build wrote");

    let site = back.scan.site().expect("the site travelled");
    assert_eq!(site.identifier(), b"KFTG");
    assert_eq!(site.latitude(), LAT);
    assert_eq!(site.longitude(), LON);
    assert_eq!(site.height_meters(), 1675);
    assert_eq!(site.tower_height_meters(), 20);

    assert_eq!(back.scan.sweeps().len(), original.scan.sweeps().len());
    for (a, b) in original.scan.sweeps().iter().zip(back.scan.sweeps()) {
        assert_eq!(a.elevation_number(), b.elevation_number());
        assert_eq!(a.radials().len(), b.radials().len());
        for (a, b) in a.radials().iter().zip(b.radials()) {
            assert_eq!(
                a.collection_timestamp(),
                b.collection_timestamp(),
                "the per-radial clock `record_tilt_freshness` takes a max over"
            );
            assert_eq!(a.azimuth_number(), b.azimuth_number());
            assert_eq!(a.azimuth_angle_degrees(), b.azimuth_angle_degrees());
            assert_eq!(a.azimuth_spacing_degrees(), b.azimuth_spacing_degrees());
            assert_eq!(a.radial_status(), b.radial_status());
            assert_eq!(a.elevation_number(), b.elevation_number());
            assert_eq!(a.elevation_angle_degrees(), b.elevation_angle_degrees());
            assert_same_moment(a.reflectivity(), b.reflectivity(), "reflectivity");
            assert_same_moment(a.velocity(), b.velocity(), "velocity");
            assert_same_moment(a.spectrum_width(), b.spectrum_width(), "spectrum width");
            assert_same_moment(
                a.differential_reflectivity(),
                b.differential_reflectivity(),
                "ZDR",
            );
            assert_same_moment(a.differential_phase(), b.differential_phase(), "PHI");
            assert_same_moment(
                a.correlation_coefficient(),
                b.correlation_coefficient(),
                "RHO",
            );
            assert_eq!(
                a.clutter_filter_power().map(DataMoment::raw_values),
                b.clutter_filter_power().map(DataMoment::raw_values),
                "the clutter-filter power moment"
            );
        }
    }
}

/// The Nyquist table's contradiction set is the half a replay of `declare`
/// could not rebuild — see [`DeclaredNyquist::to_bytes`].
#[test]
fn the_declared_nyquist_table_travels_including_what_contradicted_it() {
    let original = a_volume();
    let back = DecodedScan::from_bytes(&original.to_bytes()).expect("round trip");
    assert_eq!(
        back.declared_nyquist.get(1),
        Some(32.0),
        "first writer wins"
    );
    assert_eq!(back.declared_nyquist.get(2), Some(27.5));
    assert_eq!(
        back.declared_nyquist.contradicted().collect::<Vec<_>>(),
        vec![1],
        "a cut two waveforms disagreed about must not arrive looking trustworthy"
    );
}

#[test]
fn the_coverage_pattern_crosses_whole() {
    let original = a_volume();
    let back = DecodedScan::from_bytes(&original.to_bytes()).expect("round trip");
    let pattern = back.scan.coverage_pattern();
    assert_eq!(pattern, original.scan.coverage_pattern());
    assert_eq!(pattern.pattern_number().number(), 212);
    assert_eq!(pattern.version(), 13);
    assert_eq!(pattern.doppler_velocity_resolution(), 0.25);
    assert_eq!(pattern.pulse_width(), PulseWidth::Short);
    assert!(pattern.sails_enabled());
    assert_eq!(pattern.sails_cuts(), 3);
    assert!(pattern.mrle_enabled());
    assert_eq!(pattern.mrle_cuts(), 2);
    assert!(pattern.mpda_enabled());
    assert!(pattern.base_tilt_enabled());
    assert_eq!(pattern.base_tilt_count(), 4);
    assert!(pattern.sequence_active());
    assert!(pattern.truncated());
    let cut = &pattern.elevation_cuts()[0];
    assert_eq!(cut.elevation_angle_degrees(), 0.5);
    assert_eq!(
        cut.channel_configuration(),
        ChannelConfiguration::ConstantPhase
    );
    assert_eq!(cut.waveform_type(), WaveformType::CS);
    assert_eq!(cut.azimuth_rate_degrees_per_second(), 21.2);
    assert!(cut.super_resolution_half_degree_azimuth());
    assert!(cut.super_resolution_dual_pol_to_300km());
    assert_eq!(cut.surveillance_prf_number(), 7);
    assert_eq!(cut.surveillance_prf_pulse_count(), 15);
    assert_eq!(cut.reflectivity_threshold_db(), 1.5);
    assert_eq!(cut.correlation_coefficient_threshold_db(), 6.5);
    assert!(cut.is_sails_cut());
    assert_eq!(cut.sails_sequence_number(), 1);
    assert!(cut.is_mrle_cut());
    assert_eq!(cut.mrle_sequence_number(), 2);
    assert!(cut.is_mpda_cut());
    assert!(cut.is_base_tilt_cut());
}

#[test]
fn a_volume_is_equal_to_itself_through_the_wire() {
    let original = a_volume();
    assert_eq!(
        DecodedScan::from_bytes(&original.to_bytes()).expect("round trip"),
        original
    );
}

#[test]
fn to_bytes_reserves_exactly_what_it_writes() {
    let volume = a_volume();
    assert_eq!(volume.byte_len(), volume.to_bytes().len());
}

#[test]
fn trailing_bytes_are_refused() {
    let mut bytes = a_volume().to_bytes();
    bytes.push(0);
    assert!(DecodedScan::from_bytes(&bytes).is_none());
}

#[test]
fn a_foreign_magic_or_version_is_refused() {
    let good = a_volume().to_bytes();

    let mut wrong_magic = good.clone();
    wrong_magic[0] ^= 0xff;
    assert!(DecodedScan::from_bytes(&wrong_magic).is_none());

    let mut wrong_version = good.clone();
    wrong_version[2] = wrong_version[2].wrapping_add(1);
    assert!(DecodedScan::from_bytes(&wrong_version).is_none());
}

#[test]
fn every_truncation_is_refused() {
    let bytes = a_volume().to_bytes();
    for len in 0..bytes.len() {
        assert!(
            DecodedScan::from_bytes(&bytes[..len]).is_none(),
            "a {len}-byte prefix of a {}-byte payload decoded",
            bytes.len()
        );
    }
}

/// One sweep, one radial, no site, no cuts, no declared table and no moments —
/// so every byte before the moment mask is fixed-width and the mask's offset
/// can be stated rather than searched for.
fn a_minimal_volume() -> DecodedScan {
    let radial = Radial::new(
        0,
        0,
        0.0,
        0.5,
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
    DecodedScan {
        scan: Scan::new(
            VolumeCoveragePattern::new(
                212,
                0,
                0.5,
                PulseWidth::Unknown,
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
            ),
            vec![Sweep::new(1, vec![radial])],
        ),
        declared_nyquist: DeclaredNyquist::empty(),
    }
}

/// magic 2, version 1, "no site" 1; the pattern's number 2, version 1,
/// resolution 4, pulse width 1, flags 1, three counts 3 and its own cut count
/// 4 (with no cuts); the declared count 4 and the contradicted count 4; the
/// sweep count 4, the sweep's elevation number 1 and radial count 4; then the
/// radial: timestamp 8, azimuth number 2, azimuth 4, spacing 4, status 2,
/// elevation number 1, elevation angle 4.
const MINIMAL_MASK_OFFSET: usize =
    2 + 1 + 1 + (2 + 1 + 4 + 1 + 1 + 3 + 4) + 4 + 4 + 4 + 1 + 4 + 8 + 2 + 4 + 4 + 2 + 1 + 4;

#[test]
fn a_moment_mask_bit_this_build_does_not_have_is_refused() {
    let mut bytes = a_minimal_volume().to_bytes();
    assert_eq!(
        bytes.len(),
        MINIMAL_MASK_OFFSET + 1,
        "the mask is the last byte of a volume whose one radial carries nothing"
    );
    assert_eq!(bytes[MINIMAL_MASK_OFFSET], 0, "no moment, so no bit set");
    assert!(DecodedScan::from_bytes(&bytes).is_some());

    bytes[MINIMAL_MASK_OFFSET] |= 1 << 7;
    assert!(DecodedScan::from_bytes(&bytes).is_none());
}

/// The bytes this version ships are **these** bytes. Every round-trip test
/// here is written against `to_bytes` and `from_bytes` together, so a
/// same-width reorder made to both in step passes all of them and leaves a
/// page and a worker from opposite sides of a deploy reading fields shifted.
/// [`a_volume`] rather than a dedicated fixture: nothing in it is computed —
/// every field is a literal, and its two arithmetic expressions are exact in
/// binary at these magnitudes on every target — and it already exercises every
/// branch this encoder has. An edit to it for another test's sake moves this
/// digest; the message below says so.
/// [`crate::nyquist::DeclaredNyquist::to_bytes`] is written into the middle of
/// this payload and carries no version — it borrows this one.
#[test]
fn the_volume_wire_layout_is_the_one_this_version_ships() {
    let bytes = a_volume().to_bytes();
    assert_eq!(
        (VERSION, bytes.len(), crate::wire::layout_digest(&bytes)),
        (2, 1203, 0x4908_5ffd_c20a_bccc),
        "the bytes `DecodedScan::to_bytes` writes are not the bytes version 2 \
         shipped. Something about this payload's layout moved — a field added, \
         removed, reordered, retyped, or written at a different width, here or \
         in the `DeclaredNyquist` table nested inside it. That is the change \
         `VERSION` exists to announce, and a stale worker that shares a build \
         token with a fresh page (locally it always does: `GITHUB_SHA` is \
         absent outside CI, so the token degrades to `.../dev`) will decode a \
         60 MiB volume into the old field order and draw weather that is not \
         there, with no error anywhere. Bump `VERSION`, then write the new \
         length and digest here — in that order, and never the numbers alone. \
         The one exception is an edit to `a_volume` itself: that moves these \
         numbers without moving the layout, and then the re-pin is the whole \
         of the work.",
    );
}
