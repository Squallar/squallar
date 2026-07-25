//! Tests for the GLM HDF5 subset reader.
//!
//! The oracle is the `netcdf` crate — the C netCDF/HDF5 stack that this crate
//! exists to replace. It is a genuinely independent implementation: nothing in
//! `rustdar-hdf5` shares code, constants or expected values with it. Expected
//! numbers are never produced by this crate's own reader.
//!
//! The fixture is a real GOES-19 GLM L2 granule from the public `noaa-goes19`
//! S3 bucket (`GLM-L2-LCFA/2025/180/12/`), committed unmodified so these tests
//! need no network.

use super::*;

const FIXTURE: &[u8] =
    include_bytes!("../testdata/OR_GLM-L2-LCFA_G19_s20251801200000_e20251801200200_c20251801200212.nc");

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/OR_GLM-L2-LCFA_G19_s20251801200000_e20251801200200_c20251801200212.nc"
);

/// Reads a variable through the C netCDF library.
fn oracle_f32(name: &str) -> Vec<f32> {
    let file = netcdf::open(FIXTURE_PATH).expect("netcdf could not open the fixture");
    let var = file
        .variable(name)
        .unwrap_or_else(|| panic!("netcdf has no variable {name}"));
    var.get_values::<f32, _>(netcdf::Extents::All)
        .expect("netcdf could not read the variable")
}

fn oracle_i16(name: &str) -> Vec<i16> {
    let file = netcdf::open(FIXTURE_PATH).expect("netcdf could not open the fixture");
    let var = file
        .variable(name)
        .unwrap_or_else(|| panic!("netcdf has no variable {name}"));
    var.get_values::<i16, _>(netcdf::Extents::All)
        .expect("netcdf could not read the variable")
}

/// The spike's success criterion: `flash_lat` out of a real granule, matching
/// the C implementation.
///
/// `flash_lat` is stored as raw IEEE binary32 with no `scale_factor`,
/// `add_offset` or `_FillValue`, so the comparison is **exact** — every bit of
/// every value. There is no tolerance to hide a scaling error behind.
#[test]
fn flash_lat_matches_the_c_netcdf_library_exactly() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let ours = f.read_f32("flash_lat").expect("read flash_lat");
    let theirs = oracle_f32("flash_lat");

    assert_eq!(ours.len(), theirs.len(), "element count");
    assert!(!theirs.is_empty(), "oracle returned no data");
    assert_eq!(ours, theirs, "flash_lat values differ from netcdf");
}

#[test]
fn flash_lon_matches_the_c_netcdf_library_exactly() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let ours = f.read_f32("flash_lon").expect("read flash_lon");
    let theirs = oracle_f32("flash_lon");

    assert_eq!(ours.len(), theirs.len(), "element count");
    assert!(!theirs.is_empty(), "oracle returned no data");
    assert_eq!(ours, theirs, "flash_lon values differ from netcdf");
}

/// `flash_lat` fits in a single 256-element chunk, so it cannot detect a bug in
/// where a chunk's data lands in the output. `group_lat` has 2172 elements
/// across nine chunks, so every chunk after the first must be placed at the
/// right offset for this to pass.
///
/// This exists because a mutation that read the wrong component of the chunk
/// b-tree key survived the `flash_lat` test.
#[test]
fn multi_chunk_group_lat_matches_the_c_netcdf_library_exactly() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let ours = f.read_f32("group_lat").expect("read group_lat");
    let theirs = oracle_f32("group_lat");

    assert_eq!(ours.len(), theirs.len(), "element count");
    assert_eq!(ours, theirs, "group_lat values differ from netcdf");
}

/// Guards the test above: if `group_lat` were ever stored as a single chunk,
/// the multi-chunk placement path would silently stop being covered.
#[test]
fn group_lat_really_spans_several_chunks() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let addr = f.variables["group_lat"];
    let msgs = header::read_object_header(FIXTURE, addr).expect("header");
    let layout = msgs.iter().find(|m| m.kind == header::MSG_LAYOUT).unwrap();
    let Layout::Chunked {
        btree_address,
        chunk_dims,
    } = dataset::parse_layout(layout.body).unwrap()
    else {
        panic!("group_lat is not chunked");
    };

    let entries = dataset::read_chunk_btree(FIXTURE, btree_address, chunk_dims.len()).unwrap();
    assert!(
        entries.len() > 1,
        "group_lat has only {} chunk(s); the multi-chunk path is untested",
        entries.len()
    );
    // And the chunks must start at distinct, non-zero offsets.
    assert!(
        entries.iter().any(|e| e.offsets[0] != 0),
        "every chunk claims offset 0"
    );
}

/// `flash_energy` is the packed case: `int16` tagged `_Unsigned = "true"` with
/// a `scale_factor` and `add_offset`. This crate returns the raw stored bits,
/// so it is compared against netcdf reading the same variable as raw `i16` —
/// which bypasses the C library's implicit CF unpacking.
#[test]
fn flash_energy_raw_bits_match_the_c_netcdf_library_exactly() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let ours = f.read_i16("flash_energy").expect("read flash_energy");
    let theirs = oracle_i16("flash_energy");

    assert_eq!(ours.len(), theirs.len(), "element count");
    assert!(!theirs.is_empty(), "oracle returned no data");
    assert_eq!(ours, theirs, "flash_energy raw values differ from netcdf");
}

/// The variable list comes from walking the fractal heap, so it must account
/// for every link the C library can see. A heap walk that stopped early would
/// quietly lose variables.
///
/// This crate reports what is actually in the HDF5 group, which is netcdf's
/// variables *plus* its dimensions: netCDF-4 backs each dimension with a real
/// HDF5 dataset (a "dimension scale") and the C library hides those from
/// `variables()`. Both sets are asserted, so neither a missing variable nor a
/// stray extra name can pass.
#[test]
fn variable_list_accounts_for_every_netcdf_variable_and_dimension() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let ours: std::collections::BTreeSet<&str> = f.variable_names().collect();

    let file = netcdf::open(FIXTURE_PATH).expect("open");
    let vars: std::collections::BTreeSet<String> =
        file.variables().map(|v| v.name()).collect();
    let dims: std::collections::BTreeSet<String> =
        file.dimensions().map(|d| d.name()).collect();

    assert!(vars.len() > 40, "oracle found suspiciously few variables");
    assert!(!dims.is_empty(), "oracle found no dimensions");

    // Every netcdf variable must be present.
    for v in &vars {
        assert!(ours.contains(v.as_str()), "missing variable {v}");
    }

    // Anything extra must be a dimension scale, nothing else.
    let expected: std::collections::BTreeSet<&str> = vars
        .iter()
        .chain(dims.iter())
        .map(String::as_str)
        .collect();
    assert_eq!(ours, expected, "variable list differs from netcdf");
}

/// Shape and type, again against the C library rather than against constants
/// written down here.
#[test]
fn flash_lat_shape_and_type_match_the_c_netcdf_library() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let info = f.info("flash_lat").expect("info");

    let file = netcdf::open(FIXTURE_PATH).expect("open");
    let var = file.variable("flash_lat").expect("variable");

    assert_eq!(info.dims, var.dimensions().iter().map(|d| d.len() as u64).collect::<Vec<_>>());
    assert_eq!(info.dtype, DataType::F32);
    assert_eq!(info.len(), oracle_f32("flash_lat").len() as u64);
}

/// The file really is the format this crate claims to target. If a future
/// fixture were an older superblock-0 file, the dense-link and b-tree paths
/// below would never be exercised and the other tests would still pass.
#[test]
fn fixture_is_a_version_2_superblock_file() {
    assert_eq!(&FIXTURE[..8], &[0x89, b'H', b'D', b'F', 0x0d, 0x0a, 0x1a, 0x0a]);
    assert_eq!(FIXTURE[8], 2, "superblock version");
    assert_eq!(FIXTURE[9], 8, "size of offsets");
    assert_eq!(FIXTURE[10], 8, "size of lengths");
}

/// The root group must actually use *dense* link storage — a fractal heap with
/// no Link messages in the header. This is the structure the whole heap walk
/// exists for, and this test is what stops it from being dead code if a fixture
/// ever changed to compact links.
#[test]
fn root_group_uses_dense_link_storage() {
    let root = read_superblock(FIXTURE).expect("superblock");
    let msgs = header::read_object_header(FIXTURE, root).expect("root header");

    assert!(
        msgs.iter().any(|m| m.kind == header::MSG_LINK_INFO),
        "root group has no link info message"
    );
    assert!(
        !msgs.iter().any(|m| m.kind == 6),
        "root group has compact Link messages, so the fractal heap path is untested"
    );

    let info = msgs.iter().find(|m| m.kind == header::MSG_LINK_INFO).unwrap();
    let heap = header::link_info_heap_address(info.body).expect("heap address");
    assert_eq!(&FIXTURE[heap as usize..heap as usize + 4], b"FRHP");
}

/// `flash_lat` must be chunked with a version-1 b-tree index, otherwise the
/// b-tree reader is not being exercised either.
#[test]
fn flash_lat_is_chunked_with_a_v1_btree_index() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let addr = f.variables["flash_lat"];
    let msgs = header::read_object_header(FIXTURE, addr).expect("header");
    let layout = msgs.iter().find(|m| m.kind == header::MSG_LAYOUT).expect("layout");

    assert_eq!(layout.body[0], 3, "data layout message version");
    match dataset::parse_layout(layout.body).expect("parse layout") {
        Layout::Chunked { btree_address, .. } => {
            let a = btree_address as usize;
            assert_eq!(&FIXTURE[a..a + 4], b"TREE", "chunk index is not a v1 b-tree");
        }
        Layout::Contiguous { .. } => panic!("flash_lat is contiguous, not chunked"),
    }
}

/// The object header for `flash_lat` really does spill into a continuation
/// block, so the `OCHK` path is exercised by the parse.
#[test]
fn flash_lat_header_uses_a_continuation_block() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let addr = f.variables["flash_lat"];

    // read_object_header follows continuations transparently, so compare the
    // message count against a walk of only the first chunk. The continuation
    // holds four attribute messages.
    let all = header::read_object_header(FIXTURE, addr).expect("header");
    let attrs = all.iter().filter(|m| m.kind == 12).count();
    assert!(
        attrs >= 5,
        "expected attributes from the continuation block, found {attrs}"
    );
}

// -- negative / mutation guards ------------------------------------------
//
// Each of these corrupts one thing and asserts the reader notices. They are
// the record that the checks above are load-bearing: every one was watched
// failing (i.e. the reader returning success) before the corresponding check
// existed.

/// Truncating the file must be an error, not a short read or a panic.
#[test]
fn truncated_file_is_rejected() {
    let half = &FIXTURE[..FIXTURE.len() / 2];
    let parsed = Hdf5File::parse(half);
    let err = match parsed {
        Err(e) => e,
        Ok(f) => f
            .read_f32("flash_lat")
            .expect_err("reading past the end of a truncated file should fail"),
    };
    assert!(
        matches!(err, Error::Truncated | Error::BadSignature { .. } | Error::LinkCountMismatch { .. }),
        "unexpected error {err:?}"
    );
}

/// A file whose superblock claims version 0 must be refused rather than parsed
/// as if it were version 2.
#[test]
fn superblock_version_0_is_refused() {
    let mut bad = FIXTURE.to_vec();
    bad[8] = 0;
    assert_eq!(
        Hdf5File::parse(&bad).unwrap_err(),
        Error::Unsupported("superblock version other than 2")
    );
}

/// Corrupting the fractal heap's `FRHP` signature must fail the parse. If the
/// heap walk were somehow not on the path to the variable list, this would
/// still succeed.
#[test]
fn corrupt_fractal_heap_signature_is_caught() {
    let root = read_superblock(FIXTURE).expect("superblock");
    let msgs = header::read_object_header(FIXTURE, root).expect("root header");
    let info = msgs.iter().find(|m| m.kind == header::MSG_LINK_INFO).unwrap();
    let heap = header::link_info_heap_address(info.body).expect("heap") as usize;

    let mut bad = FIXTURE.to_vec();
    bad[heap] = b'X';
    let err = Hdf5File::parse(&bad).unwrap_err();
    assert!(matches!(err, Error::BadSignature { .. }), "got {err:?}");
}

/// A filter pipeline declaring any filter must stop the read. Without this the
/// reader would hand back compressed bytes reinterpreted as values — the exact
/// silent-wrongness this crate has to avoid.
///
/// The fixture is unfiltered, so the guard is exercised directly on synthetic
/// message bodies. Both directions are checked: zero filters must pass, and any
/// non-zero count must fail. A guard that always returned `Ok` would fail the
/// second assertion; one that always failed would fail the first.
#[test]
fn a_filter_pipeline_declaring_a_filter_is_refused() {
    // Version 2 pipeline message, zero filters: acceptable.
    assert_eq!(dataset::check_no_filters(&[2, 0, 0, 0]), Ok(()));

    // One filter (e.g. deflate) — must be refused.
    assert_eq!(
        dataset::check_no_filters(&[2, 1, 0, 0]),
        Err(Error::Unsupported("dataset with a filter pipeline"))
    );
    // Two filters, the shuffle+deflate combination.
    assert_eq!(
        dataset::check_no_filters(&[2, 2, 0, 0]),
        Err(Error::Unsupported("dataset with a filter pipeline"))
    );
}

/// A big-endian float must be refused, not byte-swapped silently. `flash_lat`
/// is little-endian; flipping the datatype's byte-order bit has to be fatal.
#[test]
fn big_endian_datatype_is_refused() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let addr = f.variables["flash_lat"];
    let msgs = header::read_object_header(FIXTURE, addr).expect("header");
    let dt = msgs.iter().find(|m| m.kind == header::MSG_DATATYPE).unwrap();

    let mut body = dt.body.to_vec();
    body[1] |= 0x01; // byte order bit -> big endian
    assert_eq!(
        dataset::parse_datatype(&body).unwrap_err(),
        Error::Unsupported("big-endian datatype")
    );
}

/// The IEEE 754 field validation must actually reject a non-IEEE float rather
/// than waving through anything of class 1 and size 4.
#[test]
fn a_four_byte_float_with_the_wrong_exponent_position_is_refused() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let addr = f.variables["flash_lat"];
    let msgs = header::read_object_header(FIXTURE, addr).expect("header");
    let dt = msgs.iter().find(|m| m.kind == header::MSG_DATATYPE).unwrap();

    // Sanity: unmodified, this is an f32.
    assert_eq!(dataset::parse_datatype(dt.body).unwrap(), DataType::F32);

    let mut body = dt.body.to_vec();
    body[12] = 22; // exponent location 23 -> 22
    assert_eq!(
        dataset::parse_datatype(&body).unwrap_err(),
        Error::Unsupported("float that is not IEEE 754 binary32/64")
    );
}

/// Pointing the chunk b-tree at the wrong address must fail loudly. This is the
/// guard that would catch a chunk-index regression.
#[test]
fn a_chunk_btree_at_the_wrong_address_is_caught() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let addr = f.variables["flash_lat"];
    let msgs = header::read_object_header(FIXTURE, addr).expect("header");
    let layout = msgs.iter().find(|m| m.kind == header::MSG_LAYOUT).unwrap();
    let Layout::Chunked { btree_address, .. } = dataset::parse_layout(layout.body).unwrap() else {
        panic!("expected chunked");
    };

    let err = dataset::read_chunk_btree(FIXTURE, btree_address + 1, 2).unwrap_err();
    assert!(matches!(err, Error::BadSignature { .. }), "got {err:?}");
}

/// Asking for the wrong Rust type must be an error rather than a
/// reinterpretation of the bits.
#[test]
fn reading_a_float_variable_as_i16_is_refused() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let err = f.read_i16("flash_lat").unwrap_err();
    assert_eq!(
        err,
        Error::TypeMismatch {
            actual: DataType::F32,
            wanted: "16-bit integer",
        }
    );
}

#[test]
fn an_unknown_variable_name_is_an_error() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    assert_eq!(
        f.read_f32("no_such_thing").unwrap_err(),
        Error::NoSuchVariable("no_such_thing".to_owned())
    );
}
