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
