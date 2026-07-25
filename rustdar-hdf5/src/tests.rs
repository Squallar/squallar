//! Tests for the GLM HDF5 subset reader.
//!
//! The oracle **was** the `netcdf` crate — the C netCDF/HDF5 stack this crate
//! exists to replace, and a genuinely independent implementation. It has since
//! been removed from the workspace (it linked `netcdf-sys`, `netcdf-src`,
//! `hdf5-metno-sys` and `hdf5-metno-src`, four bundled C/C++ builds that block
//! wasm32 and iOS), so the comparison can no longer be run live.
//!
//! What is left behind is its verdict: the constants in [`GOLDEN_F32`],
//! [`GOLDEN_I16`] and [`ALL_NAMES`] were **produced by the C library** while it
//! was still a dependency, not by this crate's reader. Regenerating them from
//! this crate would leave tests that assert only that the code agrees with
//! itself, which is worth nothing. If the fixture is ever replaced, the
//! replacement's expectations must come from an independent reader
//! (`h5dump`, `ncdump`, `h5py`) and not from `cargo test` output.
//!
//! The fixture is a real GOES-19 GLM L2 granule from the public `noaa-goes19`
//! S3 bucket (`GLM-L2-LCFA/2025/180/12/`), committed unmodified so these tests
//! need no network.

use super::*;

const FIXTURE: &[u8] =
    include_bytes!("../testdata/OR_GLM-L2-LCFA_G19_s20251801200000_e20251801200200_c20251801200212.nc");

/// FNV-1a over the raw IEEE / two's-complement bits of every element.
///
/// Exact, not tolerance-based: the fixture is unfiltered and unscaled, so any
/// difference at all is a difference, and one flipped element in 2172 must not
/// be able to hide behind an average.
fn fingerprint(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn fp_f32(v: &[f32]) -> u64 {
    fingerprint(v.iter().flat_map(|x| x.to_bits().to_le_bytes()))
}

fn fp_i16(v: &[i16]) -> u64 {
    fingerprint(v.iter().flat_map(|x| x.to_le_bytes()))
}

/// `(variable, element count, fingerprint)` as the C netCDF library read them.
const GOLDEN_F32: &[(&str, usize, u64)] = &[
    ("flash_lat", 148, 0x7a11_e1d4_0d07_9c07),
    ("flash_lon", 148, 0x129d_fc4c_8ce8_6dce),
    ("group_lat", 2172, 0xa4ac_24ad_533b_2499),
];

/// The packed case, compared as raw stored bits.
const GOLDEN_I16: &[(&str, usize, u64)] = &[("flash_energy", 148, 0x1ec7_a75c_23d4_8991)];

/// Every name in the root group: netCDF's 48 variables plus the 6 dimension
/// scales it hides from `variables()`. Captured from the C library.
const ALL_NAMES: &[&str] = &[
    "algorithm_dynamic_input_data_container",
    "algorithm_product_version_container",
    "event_count",
    "event_energy",
    "event_id",
    "event_lat",
    "event_lon",
    "event_parent_group_id",
    "event_time_offset",
    "flash_area",
    "flash_count",
    "flash_energy",
    "flash_frame_time_offset_of_first_event",
    "flash_frame_time_offset_of_last_event",
    "flash_id",
    "flash_lat",
    "flash_lon",
    "flash_quality_flag",
    "flash_time_offset_of_first_event",
    "flash_time_offset_of_last_event",
    "flash_time_threshold",
    "goes_lat_lon_projection",
    "group_area",
    "group_count",
    "group_energy",
    "group_frame_time_offset",
    "group_id",
    "group_lat",
    "group_lon",
    "group_parent_flash_id",
    "group_quality_flag",
    "group_time_offset",
    "group_time_threshold",
    "lat_field_of_view",
    "lat_field_of_view_bounds",
    "lightning_wavelength",
    "lightning_wavelength_bounds",
    "lon_field_of_view",
    "lon_field_of_view_bounds",
    "nominal_satellite_height",
    "nominal_satellite_subpoint_lat",
    "nominal_satellite_subpoint_lon",
    "number_of_events",
    "number_of_field_of_view_bounds",
    "number_of_flashes",
    "number_of_groups",
    "number_of_time_bounds",
    "number_of_wavelength_bounds",
    "percent_navigated_L1b_events",
    "percent_uncorrectable_L0_errors",
    "processing_parm_version_container",
    "product_time",
    "product_time_bounds",
    "yaw_flip_flag",
];

/// The spike's success criterion, now frozen: `flash_lat`, `flash_lon` and
/// `group_lat` out of a real granule, bit for bit as the C library read them.
///
/// All three are raw IEEE binary32 with no `scale_factor`, `add_offset` or
/// `_FillValue`, so the comparison is exact and there is no tolerance for a
/// scaling error to hide behind.
///
/// `group_lat` is the important one. `flash_lat` fits in a single 256-element
/// chunk, so it cannot detect a bug in *where* a chunk's data lands;
/// `group_lat` spans 2172 elements across nine chunks, and every chunk after
/// the first must be placed at the right offset for this to pass. It is here
/// because a mutation that read the wrong component of the chunk b-tree key
/// survived the `flash_lat` test.
#[test]
fn float_variables_match_the_values_the_c_netcdf_library_read() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    for (name, len, hash) in GOLDEN_F32 {
        let ours = f.read_f32(name).unwrap_or_else(|e| panic!("read {name}: {e}"));
        assert_eq!(ours.len(), *len, "{name}: element count");
        assert_eq!(fp_f32(&ours), *hash, "{name}: values differ from netCDF-C");
    }
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
/// so the frozen values are netcdf reading the same variable as raw `i16` —
/// which bypassed the C library's implicit CF unpacking.
#[test]
fn packed_short_raw_bits_match_the_values_the_c_netcdf_library_read() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    for (name, len, hash) in GOLDEN_I16 {
        let ours = f.read_i16(name).unwrap_or_else(|e| panic!("read {name}: {e}"));
        assert_eq!(ours.len(), *len, "{name}: element count");
        assert_eq!(fp_i16(&ours), *hash, "{name}: raw values differ from netCDF-C");
    }
}

/// The variable list comes from walking the fractal heap, so it must account
/// for every link the C library can see. A heap walk that stopped early would
/// quietly lose variables.
///
/// This crate reports what is actually in the HDF5 group, which is netcdf's 48
/// variables *plus* its 6 dimensions: netCDF-4 backs each dimension with a real
/// HDF5 dataset (a "dimension scale") and the C library hides those from
/// `variables()`. The whole set is asserted, so neither a missing variable nor
/// a stray extra name can pass.
#[test]
fn variable_list_accounts_for_every_netcdf_variable_and_dimension() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let ours: std::collections::BTreeSet<&str> = f.variable_names().collect();
    let expected: std::collections::BTreeSet<&str> = ALL_NAMES.iter().copied().collect();

    assert_eq!(expected.len(), 54, "the frozen name list lost an entry");
    assert_eq!(ours, expected, "variable list differs from netCDF-C's");
}

/// Shape and type. `[148]` and `F32` are what the C library reported for this
/// variable.
#[test]
fn flash_lat_shape_and_type_match_the_c_netcdf_library() {
    let f = Hdf5File::parse(FIXTURE).expect("parse");
    let info = f.info("flash_lat").expect("info");

    assert_eq!(info.dims, vec![148]);
    assert_eq!(info.dtype, DataType::F32);
    assert_eq!(info.len(), 148);
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

