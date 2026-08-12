use nexrad_data::result;

#[test]
fn test_error_display_formatting() {
    use std::error::Error;

    let io_error = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "test error");
    let file_error = result::Error::Io(io_error);
    let display_output = format!("{}", file_error);
    assert!(display_output.contains("data file IO error"));
    assert!(file_error.source().is_some());
}

#[test]
fn test_uncompressed_data_error() {
    let uncompressed_data = vec![0, 1, 2, 3, 4, 5, 6, 7];
    let record = nexrad_data::volume::Record::new(uncompressed_data);
    assert!(!record.compressed());

    let decompress_result = record.decompress();
    assert!(
        decompress_result.is_err(),
        "Should fail to decompress uncompressed data"
    );

    let error = decompress_result.unwrap_err();
    match error {
        result::Error::UncompressedData => {
            let display = format!("{}", error);
            assert!(display.contains("error decompressing uncompressed data"));
        }
        _ => panic!("Expected UncompressedData, got: {:?}", error),
    }
}

#[test]
fn test_compressed_data_decode_error() {
    // Upstream reads records[0] out of
    // `include_bytes!("../../downloads/KDMX20220305_232324_V06")`, a file that
    // exists only in upstream's checkout after a download step and is not in
    // the published tarball, so the test could not compile here at all. The
    // assertion is kept and the fixture replaced: `messages()` refuses a
    // compressed record on the strength of `compressed()` alone, which is the
    // `BZ` marker at bytes 4..6, so a synthetic record exercises the same two
    // lines. This is upstream's own idiom -- `tests/aws_realtime_types.rs`
    // spells the identical constant `MINIMAL_BZ_RECORD`. See VENDORED.md.
    const MINIMAL_BZ_RECORD: &[u8] = &[0, 0, 0, 0, b'B', b'Z', 0, 0, 0, 0];

    let compressed_record = nexrad_data::volume::Record::new(MINIMAL_BZ_RECORD.to_vec());
    let compressed_record = &compressed_record;
    assert!(
        compressed_record.compressed(),
        "Test record should be compressed"
    );

    let messages_result = compressed_record.messages();
    assert!(
        messages_result.is_err(),
        "Should fail to decode compressed data"
    );

    let error = messages_result.unwrap_err();
    match error {
        result::Error::CompressedData => {
            let display = format!("{}", error);
            assert!(display.contains("compressed data cannot be decoded"));
        }
        _ => panic!("Expected CompressedData, got: {:?}", error),
    }
}
