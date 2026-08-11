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

    let file = download_file(meta).await.expect("a downloaded volume");

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
