use super::*;

/// A volume that is still gzip-wrapped has no readable records, and the
/// one-pass decode must say so with the same error `volume::File::scan()`
/// raised — not a scan with no sweeps, and not a different variant.
///
/// The magic bytes are the whole test: `File::compressed` reads the first two,
/// and `records()` refuses before anything is parsed, so eight bytes are enough
/// to reach the branch.
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

/// A volume carrying no message 5 is an error, not a `Scan` with an invented
/// coverage pattern — every reader of a `Scan` assumes the pattern is the one
/// the radar flew.
///
/// Twenty-four zero bytes of volume header followed by one zeroed 2432-byte
/// frame is the cheapest thing that reaches the check: the all-zero prefix
/// picks the legacy CTM path, which hands the frame over whole, and a zeroed
/// frame decodes to a message the walk ignores. So the walk completes, finds no
/// pattern, and has to fail — exactly as `scan()` does on the same bytes.
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

/// **The premise every claim below rests on, and the one thing here that is
/// not this crate's own code**: `crate::par`'s
/// `into_par_iter().map(…).collect::<Vec<_>>()` returns results in **input**
/// order, whatever order the workers happen to finish in.
///
/// [`super::fold_contributions`] decides "first wins" by walking its `Vec`, so
/// if that `Vec` were ever in completion order every rule it states would
/// silently become "whichever worker won the race" — the exact failure the fold
/// exists to prevent, and one no fixture driving the fold directly can see.
///
/// So the map is given work shaped to scramble completion order and to punish
/// index 0 specifically: a decreasing gradient across the rest, and an item 0
/// slow enough to outlast every other worker's whole share. Instrumenting the
/// same closure with a completion counter measured item 0 finishing at rank
/// **1020 of 1024**, 680 of the 1024 items completing in the opposite half of
/// the ordering, and the four error items completing at ranks 1020, 41, 4 and
/// 219 — so a collect that honoured completion order would report index **512**
/// as its first error, on nearly every run rather than rarely. The assertion
/// below says 0.
///
/// On wasm32 the fallback is a plain `into_iter` and this is trivially true;
/// it compiles and runs there anyway, because "trivially true on one arm" is
/// how the two arms are kept saying the same thing.
#[test]
fn the_map_collects_in_input_order_however_the_workers_finish() {
    use crate::par::*;

    const N: usize = 1024;

    let collected = (0..N)
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|i| {
            // Item 0 outlasts any other worker's entire share, so it finishes
            // last; the gradient scrambles the rest.
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
//
// [`super::decoded`] decodes the LDM records apart and puts the answer back
// together with [`super::fold_contributions`]. Everything that used to be
// decided by "the walk got here first" is now decided by that fold, so these
// drive it directly: they hand it contributions in a known order and check that
// the order — not the arrival, not the value — is what picks the winner.

/// A contribution that carries nothing but a coverage pattern, so a fold can
/// reach its end without failing on `MissingCoveragePattern`.
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

/// A radial identified only by its azimuth number, all on one cut so that
/// `Sweep::from_radials` keeps them in one sweep and in the order given.
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

/// **The failure mode `rustdar-radar/Cargo.toml` warns about**, held shut: the
/// error a caller sees is the first one in *record order*, not whichever
/// worker tripped first.
///
/// Two different variants in two different orders is the whole test. rayon's
/// `Result` collect would keep either one depending on which thread lost, so a
/// volume with two malformed records would report a variant that changed run to
/// run — and the serial walk this replaced always reported the earlier record's.
/// Note that the later error is not merely deprioritised, it is discarded: a
/// fold that returned both, or the last, would fail this too.
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

/// `DeclaredNyquist::declare` is first-writer-wins *within* a table, and
/// replaying the per-record tables in record order is what extends that rule
/// across the whole volume — which is what makes the folded table the one the
/// single serial walk produced.
///
/// Both orders again, because a fold that simply kept the smaller number, or
/// the first table wholesale, would pass one of them.
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

/// The radials come back in record order, and the site and the coverage
/// pattern come from the earliest record that states one.
///
/// `Sweep::from_radials` walks its `Vec` in order and splits on a change of
/// elevation number, so radial order *is* the `Vec`'s order — the reason the
/// per-record radials are `extend`ed rather than merged.
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

/// The same "first record wins" through the real decode, records and all:
/// two records each carrying a message 5, decoded under [`crate::par`].
///
/// # What the bytes are
///
/// A 24-byte volume header followed by two size-prefixed LDM records. Real
/// records are bzip2-compressed, and `Record::compressed` decides that on two
/// magic bytes — so a record whose payload does not carry them is handed
/// straight to the message decoder, which is what lets a multi-record volume be
/// written out here without a compressor. Each record is one 2432-byte message
/// frame with its leading four bytes overwritten by the LDM size prefix, which
/// lands inside the message header's `rpg_unknown` padding and so changes
/// nothing the decoder reads.
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

/// **The claim [`super::decoded`] rests on**: folding the declared-Nyquist read
/// into the walk that builds the `Scan` changed neither half of the answer.
///
/// Both halves are checked against the code the fold replaced, on a real
/// archived volume:
///
/// * the `Scan` against `nexrad_data::volume::File::scan()`, which is what this
///   crate called until the fold and is still the definition of a correctly
///   decoded volume — radials, their order, the sweep split, the site and the
///   coverage pattern all compare;
/// * the table against [`crate::nyquist::DeclaredNyquist::from_archive`], whose
///   separate walk exists for exactly this. It reads the same field off the
///   same bytes without building anything else, so agreement is evidence rather
///   than a restatement.
///
/// A fixed historical volume rather than the latest one: the two comparisons
/// are exact equality, and the point is a decode this crate can rerun against
/// the same bytes years from now.
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

    // Not a tautology worth skipping: an empty table would make the second
    // assertion below pass on a volume that declared nothing.
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

/// A pre-2010 `AR2V0001` volume produces a `Scan` with **no site**, so
/// everything downstream of it falls back to the table.
///
/// # Why this is a distinct case and not the chunk feed again
///
/// Both arrive at `ScanInfo::from_scan` as a site-less `Scan`, but they get
/// there for unrelated reasons and only one of them is a *decode* property.
/// The chunk feed never had a site to lose — `chunks::VolumeAssembler` builds
/// through `Scan::new`. A legacy volume is Message 1 throughout, and Message 1
/// has no Volume Data Block at all: the latitude, longitude and heights the
/// modern format states on every Message 31 simply are not in the file. So the
/// walk below has to complete, find nothing to read, and take the `Scan::new`
/// arm — rather than, say, unwrapping a site it assumes is there.
///
/// # What the bytes are
///
/// The smallest legacy volume that decodes: a 24-byte `AR2V0001` header, then
/// one 2432-byte CTM frame carrying a Message 5 with zero elevation cuts. The
/// all-zero `rpg_unknown` prefix is what selects the CTM path in
/// `split_compressed_records`, and the coverage pattern is what stops the walk
/// failing with `MissingCoveragePattern` before it can reach the site at all —
/// which is exactly what `a_volume_with_no_coverage_pattern_fails_rather_than_
/// inventing_one` above uses the *absence* of a message 5 to check.
///
/// No Message 1 is synthesized. Adding one would change nothing this asserts:
/// `decoded` matches `DigitalRadarData` only, exactly as upstream's `scan()`
/// does, so a legacy radial message is skipped without being looked at.
#[test]
fn a_pre_2010_volume_states_no_site_and_falls_back_to_the_table() {
    // The radars this renders against; there are none until a test asks.
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

    // And the fallback the absence exists to reach.
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
// established **independently of this crate**, by two decoders that share no
// code with it: MetPy's `Level2File`, and a raw walk of the LDM framing and
// Message 31 headers written from the ICD. Both report the same 8280 radials,
// the same per-cut counts and the same gaps, radial for radial.
//
// So the table below is an observation, not an expectation borrowed from our
// own assembler: it says which 120-radial LDM records the *file* contains, and
// these tests ask whether our assembly reproduces that and whether the
// measurement names the holes. Asserting our decode against our decode would
// have passed on the day the wedge went missing.

/// Which 120-radial records each cut of `KCRP20260717_211257_V06` carries.
///
/// `(elevation number, azimuth spacing, records present)`, where record `k`
/// holds azimuth numbers `(k - 1) * 120 + 1 ..= k * 120`. A 0.5° cut has six,
/// a 1.0° cut three. Nine records are absent across the volume, which is the
/// 1080 radials between its 8280 and the 9360 a complete VCP 215 would hold.
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
/// measures in the real file. The synthetic radials below sit on exact
/// multiples of their spacing, so they measure the whole number the real
/// jittered antenna misses by a few hundredths — 60.480 against 60.5 on cut 4.
const KCRP_SHORT_CUTS: &[(u8, usize, f64)] = &[
    (1, 480, 120.5),
    (3, 480, 60.5),
    (4, 600, 60.5),
    (7, 600, 60.5),
    (8, 600, 60.5),
    (10, 600, 60.5),
    (13, 240, 121.0),
];

/// One radial of a cut, at the azimuth its azimuth number implies.
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

/// The volume as the file holds it, cut by cut, record by record.
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

/// **The wedge that went missing, and the silence around it.**
///
/// Assembly first: fed the file's own radial inventory, the fold must produce
/// each cut at exactly the count the independent decoders found — nothing
/// dropped by us, nothing invented. In particular cut 4, the one
/// `render::find_sweep` hands the 0.5° storm-relative velocity pane, holds 600
/// radials and not 720.
///
/// Then the part that was missing: each short cut must *say* it is short. The
/// gap is what carries it, because the count alone cannot — cut 13 holds a
/// dense 1..240 with no interior seam at all, and only the 121° hole where the
/// circle closes distinguishes it from a sector the antenna really swept.
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

/// The complement, and the half that stops the measurement being a rubber
/// stamp: the nine cuts the file carries whole are reported whole, arc 360°.
///
/// Without this a measurement that called *everything* sectored would pass the
/// test above.
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

/// The two timestamps built exactly as the two production routes build them:
/// `(from the first radial, from the archive key)`.
///
/// Neither side is hand-written as a literal, because the whole defect lived
/// in the difference between the two constructions — a test that spelled both
/// out by hand would be asserting against its own arithmetic rather than
/// against the code's.
fn the_two_routes(hms: &str, millis_into_the_second: i64) -> (NaiveDateTime, NaiveDateTime) {
    let date = chrono::NaiveDate::from_ymd_opt(2026, 3, 2).expect("a real date");
    let key_time = NaiveTime::parse_from_str(hms, "%H%M%S").expect("a real archive key tail");
    // The archive key's route: `list_scans_for_range`, verbatim.
    let from_key = date.and_time(key_time);
    // The volume's route: `types::ScanInfo::from_scan`, verbatim — a radial's
    // `collection_timestamp()` in epoch milliseconds, through
    // `from_timestamp_millis`.
    let collection_timestamp = from_key.and_utc().timestamp_millis() + millis_into_the_second;
    let from_radial = chrono::DateTime::from_timestamp_millis(collection_timestamp)
        .expect("a real collection timestamp")
        .naive_utc();
    (from_radial, from_key)
}

/// **The regression.** A loop frame is stamped from the S3 key and the object
/// cached for it is stamped from the volume's first radial, and those two
/// name one volume.
///
/// The measured spread is 1 ms to 993 ms with a median of 517 ms, over 108 of
/// 108 archive volumes, so the endpoints and the median are all exercised
/// here — 0 ms is included as the case that never occurs in the archive but is
/// the still frame's every time, since there both sides come off the radial.
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
        // Symmetric, because the two call sites read it in both orders.
        assert!(
            names_same_volume(from_key, from_radial),
            "the pairing must not depend on which side is named first",
        );
    }
}

/// Exact equality still pairs — the still-frame path, where both sides are
/// `ScanInfo::timestamp` and the milliseconds are identical.
///
/// Widening a comparison is only safe if it is a widening: this is the case
/// the old `==` got right, and it has to keep working.
#[test]
fn a_volume_start_stated_identically_twice_still_names_one_volume() {
    let (from_radial, _) = the_two_routes("120347", 517);
    assert!(names_same_volume(from_radial, from_radial));

    let (whole_second, from_key) = the_two_routes("120347", 0);
    assert!(names_same_volume(whole_second, from_key));
}

/// A neighbouring volume never pairs, however either start was stated.
///
/// One scan cycle is the collision this has to be impossible for. The shortest
/// WSR-88D cadence in the tree is a measured 198 s (`constants/tests.rs:468`),
/// so that is the gap tested rather than the 259 s nominal — and the second
/// either side of it, because "under a second apart" is the whole rule and the
/// boundary is where a rule of that shape fails.
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

    // And the boundary, 198 s inside the nearest real collision: one second is
    // one volume, and the next second is never the same volume however small
    // the step across the boundary is.
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
    // The boundary is a boundary and not a rounding: the last instant of a
    // volume's second still names that volume, and does not also name the one
    // after. A rule that rounded rather than truncated would answer the
    // opposite to both of these.
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
