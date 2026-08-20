use super::*;

/// The magic bytes are the whole test: `File::compressed` reads the first two
/// and `records()` refuses before anything is parsed, so eight bytes reach it.
#[test]
fn a_gzip_wrapped_volume_is_refused_exactly_as_upstream_refuses_it() {
    let file = nexrad_data::volume::File::new(vec![0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0]);

    assert!(
        matches!(file.scan(), Err(nexrad_data::result::Error::CompressedFile)),
        "premise: upstream refuses a gzip-wrapped file"
    );
    assert!(
        matches!(
            decoded(&file),
            Err(ScanError::Decode(
                nexrad_data::result::Error::CompressedFile
            ))
        ),
        "the one-pass decode must refuse it the same way"
    );
}

/// Twenty-four zero bytes of volume header followed by one zeroed 2432-byte
/// frame is the cheapest thing that reaches the check: the all-zero prefix
/// picks the legacy CTM path, and a zeroed frame decodes to a message the walk
/// ignores.
#[test]
fn a_volume_with_no_coverage_pattern_fails_rather_than_inventing_one() {
    let file = nexrad_data::volume::File::new(vec![0u8; 24 + 2432]);

    assert!(
        matches!(
            file.scan(),
            Err(nexrad_data::result::Error::MissingCoveragePattern)
        ),
        "premise: upstream refuses a volume with no message 5"
    );
    assert!(
        matches!(
            decoded(&file),
            Err(ScanError::Decode(
                nexrad_data::result::Error::MissingCoveragePattern
            ))
        ),
        "the one-pass decode must refuse it the same way"
    );
}

/// The premise every claim below rests on: `crate::par`'s
/// `into_par_iter().map(…).collect::<Vec<_>>()` returns results in **input**
/// order, whatever order the workers finish in. Instrumenting the same closure
/// with a completion counter measured item 0 finishing at rank 1020 of 1024,
/// and the four error items at ranks 1020, 41, 4 and 219 — so a collect that
/// honoured completion order would report index 512 as its first error.
#[test]
fn the_map_collects_in_input_order_however_the_workers_finish() {
    use crate::par::*;

    const N: usize = 1024;

    let collected = (0..N)
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|i| {
            let iterations = if i == 0 { 2_000_000 } else { (N - i) * 16 };
            let mut spin = 0u64;
            for k in 0..iterations {
                spin = spin.wrapping_add(std::hint::black_box(k as u64));
            }
            std::hint::black_box(spin);
            if i % 256 == 0 { Err(i) } else { Ok(i) }
        })
        .collect::<Vec<_>>();

    assert_eq!(collected.len(), N, "every item comes back");
    assert!(
        collected
            .iter()
            .enumerate()
            .all(|(position, item)| item.unwrap_or_else(|i| i) == position),
        "the collected results are in input order, not completion order",
    );
    assert_eq!(
        collected.iter().find_map(|item| item.err()),
        Some(0),
        "the first Err walking the results is the first in input order — the \
         last item to finish, and the one a completion-ordered collect buries",
    );
}

// -- the record-order fold ----------------------------------------------

fn a_pattern_and_nothing_else(vcp_number: u16) -> RecordContribution {
    RecordContribution {
        declared_nyquist: crate::nyquist::DeclaredNyquist::empty(),
        radials: Vec::new(),
        coverage_pattern: Some(nexrad_model::data::VolumeCoveragePattern::new(
            vcp_number,
            1,
            0.5,
            nexrad_model::data::PulseWidth::Short,
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
        )),
        site_location: None,
    }
}

fn radial_numbered(azimuth_number: u16) -> nexrad_model::data::Radial {
    nexrad_model::data::Radial::new(
        0,
        azimuth_number,
        f32::from(azimuth_number),
        0.5,
        nexrad_model::data::RadialStatus::IntermediateRadialData,
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
}

#[test]
fn the_reported_error_is_the_first_in_record_order() {
    let file = nexrad_data::volume::File::new(Vec::new());

    let earlier_is_uncompressed = fold_contributions(
        &file,
        vec![
            Ok(a_pattern_and_nothing_else(21)),
            Err(nexrad_data::result::Error::UncompressedData),
            Err(nexrad_data::result::Error::CompressedData),
        ],
    );
    assert!(
        matches!(
            earlier_is_uncompressed,
            Err(ScanError::Decode(
                nexrad_data::result::Error::UncompressedData
            ))
        ),
        "the first malformed record in the file is the one reported",
    );

    let earlier_is_compressed = fold_contributions(
        &file,
        vec![
            Ok(a_pattern_and_nothing_else(21)),
            Err(nexrad_data::result::Error::CompressedData),
            Err(nexrad_data::result::Error::UncompressedData),
        ],
    );
    assert!(
        matches!(
            earlier_is_compressed,
            Err(ScanError::Decode(
                nexrad_data::result::Error::CompressedData
            ))
        ),
        "swapping the two records swaps the answer, so it is the order deciding",
    );
}

#[test]
fn an_earlier_records_declared_nyquist_survives_a_later_records() {
    let file = nexrad_data::volume::File::new(Vec::new());

    let low_cut_first = |first: f64, second: f64| {
        let mut earlier = a_pattern_and_nothing_else(21);
        earlier.declared_nyquist = [(1u8, first)].into_iter().collect();
        let mut later = a_pattern_and_nothing_else(21);
        later.declared_nyquist = [(1u8, second), (2u8, 31.0)].into_iter().collect();
        fold_contributions(&file, vec![Ok(earlier), Ok(later)])
            .expect("two well-formed records fold")
            .declared_nyquist
    };

    let ascending = low_cut_first(22.5, 35.5);
    assert_eq!(
        ascending.get(1),
        Some(22.5),
        "the earlier record's statement of cut 1 stands",
    );
    assert_eq!(
        ascending.get(2),
        Some(31.0),
        "a cut only the later record names is still picked up",
    );

    assert_eq!(
        low_cut_first(35.5, 22.5).get(1),
        Some(35.5),
        "swapping them swaps the answer, so it is the order deciding and not the value",
    );
}

#[test]
fn radials_the_site_and_the_pattern_all_come_from_the_earliest_record_that_has_them() {
    // An ICAO for the site to be named with; the location itself comes off the
    // radials, which is the half the fold decides.
    let mut header = Vec::new();
    header.extend_from_slice(b"AR2V0006.");
    header.extend_from_slice(b"001");
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(&0u32.to_be_bytes());
    header.extend_from_slice(b"KTLX");
    let file = nexrad_data::volume::File::new(header);

    let mut earlier = a_pattern_and_nothing_else(21);
    earlier.radials = vec![radial_numbered(1), radial_numbered(2)];
    earlier.site_location = Some(SiteLocation {
        latitude: 35.25,
        longitude: -97.5,
        site_height: 370,
        tower_height: 20,
    });

    let mut later = a_pattern_and_nothing_else(12);
    later.radials = vec![radial_numbered(3), radial_numbered(4)];
    later.site_location = Some(SiteLocation {
        latitude: 0.0,
        longitude: 0.0,
        site_height: 0,
        tower_height: 0,
    });

    let folded = fold_contributions(&file, vec![Ok(earlier), Ok(later)])
        .expect("two well-formed records fold");

    let sweeps = folded.scan.sweeps();
    assert_eq!(sweeps.len(), 1, "one elevation number is one sweep");
    assert_eq!(
        sweeps[0]
            .radials()
            .iter()
            .map(|r| r.azimuth_number())
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4],
        "the second record's radials follow the first record's, in record order",
    );

    let site = folded.scan.site().expect("the first record states one");
    assert_eq!(site.latitude(), 35.25);
    assert_eq!(site.longitude(), -97.5);
    assert_eq!(site.height_meters(), 370);
    assert_eq!(site.identifier_string(), "KTLX");
    assert_eq!(
        folded.scan.coverage_pattern_number().number(),
        21,
        "the first record's message 5 wins, as it does in `scan()`",
    );
}

/// A 24-byte volume header followed by two size-prefixed LDM records. Each is
/// one 2432-byte message frame with its leading four bytes overwritten by the
/// LDM size prefix, which lands inside the message header's `rpg_unknown`
/// padding and so changes nothing the decoder reads. `Record::compressed`
/// decides on two magic bytes, so a payload without them needs no compressor.
#[test]
fn the_first_records_coverage_pattern_wins_through_the_parallel_decode() {
    const FRAME: usize = 2432;
    const MESSAGE_HEADER: usize = 28;

    fn message_5_record(vcp_number: u16) -> Vec<u8> {
        let mut frame = vec![0u8; FRAME];
        // The LDM record size prefix, over the first four bytes of the message
        // header's `rpg_unknown`. Non-zero, which is also what keeps
        // `split_compressed_records` on the LDM path rather than the legacy one.
        frame[0..4].copy_from_slice(&((FRAME - 4) as u32).to_be_bytes());
        frame[12..14].copy_from_slice(&11u16.to_be_bytes()); // segment size, halfwords
        frame[15] = 5; // RDA volume coverage pattern
        frame[16..18].copy_from_slice(&1u16.to_be_bytes()); // sequence number
        frame[24..26].copy_from_slice(&1u16.to_be_bytes()); // segment count
        frame[26..28].copy_from_slice(&1u16.to_be_bytes()); // segment number
        let vcp = MESSAGE_HEADER;
        frame[vcp..vcp + 2].copy_from_slice(&11u16.to_be_bytes()); // size, halfwords
        frame[vcp + 2..vcp + 4].copy_from_slice(&2u16.to_be_bytes()); // pattern type
        frame[vcp + 4..vcp + 6].copy_from_slice(&vcp_number.to_be_bytes());
        frame[vcp + 6..vcp + 8].copy_from_slice(&0u16.to_be_bytes()); // elevation cuts
        frame[vcp + 8] = 1; // version
        frame[vcp + 10] = 2; // doppler velocity resolution, 0.5
        frame[vcp + 11] = 2; // pulse width, short
        frame
    }

    let volume = |first: u16, second: u16| {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"AR2V0006.");
        bytes.extend_from_slice(b"001");
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"KTLX");
        assert_eq!(bytes.len(), 24, "the Archive II header is 24 bytes");
        bytes.extend_from_slice(&message_5_record(first));
        bytes.extend_from_slice(&message_5_record(second));
        nexrad_data::volume::File::new(bytes)
    };

    let file = volume(21, 12);
    assert_eq!(
        file.records().expect("the records split").len(),
        2,
        "premise: this really is a two-record volume",
    );
    assert!(
        !file.records().expect("the records split")[0].compressed(),
        "premise: the records go to the decoder without a decompress",
    );

    assert_eq!(
        decoded(&file)
            .expect("the volume decodes")
            .scan
            .coverage_pattern_number()
            .number(),
        21,
    );
    assert_eq!(
        decoded(&volume(12, 21))
            .expect("the volume decodes")
            .scan
            .coverage_pattern_number()
            .number(),
        12,
        "swapping the records swaps the answer, so record order survives the map",
    );
}

// -- live ---------------------------------------------------------------
//
// Run with:
//   cargo test -p rustdar-radar --release --lib -- --ignored --nocapture scan::tests::live_

#[cfg(not(target_arch = "wasm32"))]
#[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
#[tokio::test]
async fn live_one_pass_decode_matches_the_two_pass_decode() {
    let day = chrono::NaiveDate::from_ymd_opt(2024, 5, 20).expect("a real date");
    let metas = list_files("KTLX", &day).await.expect("a listing");
    let meta = metas.first().expect("the day is not empty").clone();
    println!("volume: {}", meta.name());

    let file =
        nexrad_data::volume::File::new(download_file(meta).await.expect("a downloaded volume"));

    let upstream = file.scan().expect("upstream decodes it");
    let independent = crate::nyquist::DeclaredNyquist::from_archive(&file);
    let one_pass = decoded(&file).expect("the one-pass decode");

    println!(
        "{} sweeps, {} cuts declared a Nyquist velocity: {:?}",
        upstream.sweeps().len(),
        independent.len(),
        independent
    );

    assert!(
        !independent.is_empty(),
        "a Message 31 volume must declare a Nyquist velocity somewhere"
    );
    assert_eq!(
        one_pass.declared_nyquist, independent,
        "the folded Nyquist table diverged from the separate walk's"
    );
    assert_eq!(
        one_pass.scan, upstream,
        "the one-pass decode produced a different Scan from `File::scan()`"
    );
}

/// The smallest legacy volume that decodes: a 24-byte `AR2V0001` header, then
/// one 2432-byte CTM frame carrying a Message 5 with zero elevation cuts. The
/// all-zero `rpg_unknown` prefix selects the CTM path, and the coverage
/// pattern stops the walk failing with `MissingCoveragePattern` first.
///
/// Message 1 has no Volume Data Block at all, so the site simply is not in the
/// file and the walk takes the `Scan::new` arm.
#[test]
fn a_pre_2010_volume_states_no_site_and_falls_back_to_the_table() {
    crate::sites::fixture::install();
    const FRAME: usize = 2432;
    const MESSAGE_HEADER: usize = 28;

    let mut bytes = Vec::new();
    // Volume header: tape filename, extension, date, time, ICAO.
    bytes.extend_from_slice(b"AR2V0001.");
    bytes.extend_from_slice(b"001");
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(b"KTLX");
    assert_eq!(bytes.len(), 24, "the Archive II header is 24 bytes");

    let mut frame = vec![0u8; FRAME];
    // Message header. `rpg_unknown` stays zero, which is both what a real CTM
    // frame carries and what selects the legacy split.
    frame[12..14].copy_from_slice(&11u16.to_be_bytes()); // segment size, halfwords
    frame[15] = 5; // RDA volume coverage pattern
    frame[16..18].copy_from_slice(&1u16.to_be_bytes()); // sequence number
    frame[24..26].copy_from_slice(&1u16.to_be_bytes()); // segment count
    frame[26..28].copy_from_slice(&1u16.to_be_bytes()); // segment number
    // Message 5 itself: 22 bytes of header and no elevation blocks.
    let vcp = MESSAGE_HEADER;
    frame[vcp..vcp + 2].copy_from_slice(&11u16.to_be_bytes()); // size, halfwords
    frame[vcp + 2..vcp + 4].copy_from_slice(&2u16.to_be_bytes()); // pattern type
    frame[vcp + 4..vcp + 6].copy_from_slice(&21u16.to_be_bytes()); // VCP 21
    frame[vcp + 6..vcp + 8].copy_from_slice(&0u16.to_be_bytes()); // elevation cuts
    frame[vcp + 8] = 1; // version
    frame[vcp + 10] = 2; // doppler velocity resolution, 0.5
    frame[vcp + 11] = 2; // pulse width, short
    bytes.extend_from_slice(&frame);

    let file = nexrad_data::volume::File::new(bytes);
    assert_eq!(
        file.header().and_then(|h| h.tape_filename()).as_deref(),
        Some("AR2V0001."),
        "premise: this is a pre-2010 tape filename",
    );

    let upstream = file.scan().expect("upstream decodes the same bytes");
    assert!(
        upstream.site().is_none(),
        "premise: upstream reads no site out of a legacy volume either",
    );

    let decoded = decoded(&file).expect("the one-pass walk decodes it");
    assert_eq!(decoded.scan.coverage_pattern_number().number(), 21);
    assert!(
        decoded.scan.site().is_none(),
        "a legacy volume has no Volume Data Block, so there is no site to read",
    );

    let info = crate::types::ScanInfo::from_scan(
        &decoded.scan,
        "KTLX",
        chrono::NaiveDate::from_ymd_opt(2009, 5, 8)
            .unwrap()
            .and_hms_opt(1, 48, 0)
            .unwrap(),
        None,
    );
    let table = crate::sites::get_radar_site("KTLX").expect("in the table");
    assert_eq!(
        info.site_source,
        crate::site_position::SitePositionSource::Table,
    );
    assert_eq!(info.site.lat, table.lat);
    assert_eq!(info.site.lon, table.lon);
    assert_eq!(info.site_position, None);
}

// -- sweep coverage -----------------------------------------------------
//
// The volume behind these numbers is `KCRP20260717_211257_V06`, whose 0.5°
// Doppler cut renders with a 60° wedge missing. Its radial inventory was
// established independently of this crate, by MetPy's `Level2File` and a raw
// walk of the LDM framing and Message 31 headers written from the ICD. Both
// report the same 8280 radials, the same per-cut counts and the same gaps.

/// Which 120-radial records each cut of `KCRP20260717_211257_V06` carries:
/// `(elevation number, azimuth spacing, records present)`, where record `k`
/// holds azimuth numbers `(k - 1) * 120 + 1 ..= k * 120`. Nine records are
/// absent across the volume — the 1080 radials between its 8280 and the 9360
/// a complete VCP 215 would hold.
const KCRP_2026_07_17T21_12_57: &[(u8, f32, &[u8])] = &[
    (1, 0.5, &[1, 2, 5, 6]),
    (2, 0.5, &[1, 2, 3, 4, 5, 6]),
    (3, 0.5, &[1, 2, 3, 5]),
    (4, 0.5, &[1, 2, 4, 5, 6]),
    (5, 0.5, &[1, 2, 3, 4, 5, 6]),
    (6, 0.5, &[1, 2, 3, 4, 5, 6]),
    (7, 0.5, &[2, 3, 4, 5, 6]),
    (8, 0.5, &[1, 3, 4, 5, 6]),
    (9, 0.5, &[1, 2, 3, 4, 5, 6]),
    (10, 0.5, &[1, 2, 3, 4, 5]),
    (11, 1.0, &[1, 2, 3]),
    (12, 1.0, &[1, 2, 3]),
    (13, 1.0, &[1, 2]),
    (14, 1.0, &[1, 2, 3]),
    (15, 1.0, &[1, 2, 3]),
    (16, 1.0, &[1, 2, 3]),
];

/// The cuts the independent decode found short, and the widest gap each one
/// measures. The synthetic radials sit on exact multiples of their spacing, so
/// they measure the whole number the jittered antenna misses by a few
/// hundredths — 60.480 against 60.5 on cut 4.
const KCRP_SHORT_CUTS: &[(u8, usize, f64)] = &[
    (1, 480, 120.5),
    (3, 480, 60.5),
    (4, 600, 60.5),
    (7, 600, 60.5),
    (8, 600, 60.5),
    (10, 600, 60.5),
    (13, 240, 121.0),
];

fn radial_at(
    elevation_number: u8,
    azimuth_number: u16,
    spacing: f32,
) -> nexrad_model::data::Radial {
    nexrad_model::data::Radial::new(
        0,
        azimuth_number,
        f32::from(azimuth_number - 1) * spacing,
        spacing,
        nexrad_model::data::RadialStatus::IntermediateRadialData,
        elevation_number,
        0.5,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

fn kcrp_radials() -> Vec<nexrad_model::data::Radial> {
    let mut radials = Vec::new();
    for (elevation_number, spacing, records) in KCRP_2026_07_17T21_12_57 {
        for record in *records {
            for offset in 0..120u16 {
                let azimuth_number = (u16::from(*record) - 1) * 120 + offset + 1;
                radials.push(radial_at(*elevation_number, azimuth_number, *spacing));
            }
        }
    }
    radials
}

#[test]
fn a_volume_missing_whole_chunks_assembles_short_and_says_so() {
    let file = nexrad_data::volume::File::new(Vec::new());
    let mut contribution = a_pattern_and_nothing_else(215);
    contribution.radials = kcrp_radials();

    let decoded = fold_contributions(&file, vec![Ok(contribution)]).expect("the fold succeeds");
    let coverage = decoded.sweep_coverage();

    assert_eq!(
        coverage.len(),
        KCRP_2026_07_17T21_12_57.len(),
        "one sweep per cut: no cut fragmented, because no elevation number repeats",
    );
    assert_eq!(
        coverage.iter().map(|cut| cut.radials).sum::<usize>(),
        8280,
        "every radial the file holds survives assembly",
    );

    for (cut, (elevation_number, spacing, records)) in coverage.iter().zip(KCRP_2026_07_17T21_12_57)
    {
        assert_eq!(cut.elevation_number, *elevation_number);
        assert_eq!(
            cut.radials,
            records.len() * 120,
            "cut {elevation_number} holds the records the file carries",
        );
        assert_eq!(cut.azimuth_step_degrees, f64::from(*spacing));
    }

    let short: Vec<_> = coverage.iter().filter(|cut| !cut.is_whole).collect();
    assert_eq!(
        short
            .iter()
            .map(|cut| (cut.elevation_number, cut.radials, cut.largest_gap_degrees))
            .collect::<Vec<_>>(),
        KCRP_SHORT_CUTS.to_vec(),
        "exactly the cuts the independent decode found short are named short",
    );

    let doppler = coverage
        .iter()
        .find(|cut| cut.elevation_number == 4)
        .expect("cut 4");
    assert_eq!(doppler.radials, 600);
    assert!(!doppler.is_whole);
    assert_eq!(doppler.arc_degrees(), 299.5, "300° of azimuth, not 360°");
}

#[test]
fn the_cuts_the_file_carries_whole_are_not_called_short() {
    let file = nexrad_data::volume::File::new(Vec::new());
    let mut contribution = a_pattern_and_nothing_else(215);
    contribution.radials = kcrp_radials();

    let decoded = fold_contributions(&file, vec![Ok(contribution)]).expect("the fold succeeds");

    let whole: Vec<u8> = decoded
        .sweep_coverage()
        .iter()
        .filter(|cut| cut.is_whole)
        .map(|cut| cut.elevation_number)
        .collect();
    assert_eq!(whole, vec![2, 5, 6, 9, 11, 12, 14, 15, 16]);

    assert!(
        decoded
            .sweep_coverage()
            .iter()
            .filter(|cut| cut.is_whole)
            .all(|cut| cut.arc_degrees() == 360.0),
        "a whole cut covers the circle",
    );
}

// ── Volume identity across the two ways a start is stated ─────────────────

fn the_two_routes(hms: &str, millis_into_the_second: i64) -> (NaiveDateTime, NaiveDateTime) {
    let date = chrono::NaiveDate::from_ymd_opt(2026, 3, 2).expect("a real date");
    let key_time = NaiveTime::parse_from_str(hms, "%H%M%S").expect("a real archive key tail");
    let from_key = date.and_time(key_time);
    let collection_timestamp = from_key.and_utc().timestamp_millis() + millis_into_the_second;
    let from_radial = chrono::DateTime::from_timestamp_millis(collection_timestamp)
        .expect("a real collection timestamp")
        .naive_utc();
    (from_radial, from_key)
}

/// The measured spread is 1 ms to 993 ms with a median of 517 ms over 108 of
/// 108 archive volumes, so the endpoints and the median are all exercised —
/// 0 ms included as the still frame's case, where both sides come off the
/// radial.
#[test]
fn a_loop_frames_archive_key_names_the_volumes_first_radial() {
    for millis in [0, 1, 517, 993, 999] {
        let (from_radial, from_key) = the_two_routes("120347", millis);

        assert_eq!(
            from_key.and_utc().timestamp_subsec_millis(),
            0,
            "premise: an archive key has no sub-second field",
        );
        assert_eq!(
            from_radial.and_utc().timestamp_subsec_millis(),
            u32::try_from(millis).expect("a millisecond offset inside a second"),
            "premise: the first radial's time keeps its milliseconds",
        );

        assert!(
            names_same_volume(from_radial, from_key),
            "a frame keyed {from_key} was refused the object stamped {from_radial}",
        );
        assert!(
            names_same_volume(from_key, from_radial),
            "the pairing must not depend on which side is named first",
        );
    }
}

#[test]
fn a_volume_start_stated_identically_twice_still_names_one_volume() {
    let (from_radial, _) = the_two_routes("120347", 517);
    assert!(names_same_volume(from_radial, from_radial));

    let (whole_second, from_key) = the_two_routes("120347", 0);
    assert!(names_same_volume(whole_second, from_key));
}

/// The shortest WSR-88D cadence in the tree is a measured 198 s
/// (`constants/tests.rs:468`), so that is the gap tested rather than the 259 s
/// nominal — and the second either side of it.
#[test]
fn a_neighbouring_volume_never_pairs_however_its_start_was_stated() {
    let (from_radial, from_key) = the_two_routes("120347", 517);

    // The shortest measured WSR-88D volume interval, and the two longer ones.
    for gap_secs in [198, 259, 360, 517] {
        for shifted in [
            from_radial + Duration::seconds(gap_secs),
            from_radial - Duration::seconds(gap_secs),
            from_key + Duration::seconds(gap_secs),
            from_key - Duration::seconds(gap_secs),
        ] {
            assert!(
                !names_same_volume(from_radial, shifted),
                "a volume {gap_secs} s away paired with {from_radial}",
            );
            assert!(
                !names_same_volume(from_key, shifted),
                "a volume {gap_secs} s away paired with {from_key}",
            );
        }
    }

    let (next_second, _) = the_two_routes("120348", 0);
    assert!(
        !names_same_volume(from_radial, next_second),
        "the second after a volume start is a different second and so a \
         different volume",
    );
    assert!(
        !names_same_volume(from_key, next_second),
        "two archive keys one second apart are two volumes",
    );
    let last_instant = next_second - Duration::milliseconds(1);
    assert!(
        names_same_volume(from_key, last_instant),
        "the last millisecond of a volume's second still names that volume",
    );
    assert!(
        !names_same_volume(next_second, last_instant),
        "the millisecond before a second must not name the second after it",
    );
}
