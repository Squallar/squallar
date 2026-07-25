//! Differential tests: the pure-Rust reader against the netCDF-C library.
//!
//! # Why this file is temporary
//!
//! This module exists to make one migration safe, and is expected to be
//! deleted with the `netcdf` dependency once it has done its job. While both
//! readers are present there are two genuinely independent implementations of
//! the same format in the tree — netCDF-C (decades old, C, the reference) and
//! [`hdf5_pure`] (pure Rust) — and comparing them is far stronger evidence
//! than any expectation written by hand. Once `netcdf` is gone that oracle is
//! gone forever, so the comparison is made *now* and the values it agrees on
//! are frozen into [`super::tests`] as constants.
//!
//! # What is actually at risk
//!
//! netCDF-C applies `scale_factor`/`add_offset` **implicitly** in some paths;
//! `hdf5_pure` never does. A reader that ignores CF packing produces plausible
//! numbers of the wrong magnitude — no error, no panic, just wrong lightning.
//! That bites the packed `int16` variables (`flash_energy`, `flash_area`,
//! `event_lat`, `event_lon`, every `*_time_offset`), **not** `flash_lat`,
//! which is plain `H5T_IEEE_F32LE` with no packing attributes at all.
//!
//! So comparing only `flash_lat` would prove nothing about the code path that
//! can actually be silently wrong. Every test here therefore compares *after*
//! CF unpacking, and the packed variables are named explicitly so a future
//! edit cannot quietly drop them.

use std::collections::BTreeMap;

use super::cf::{self, CfAttr, RawVar, VarType};
use super::h5::Granule;

/// A real GOES-19 GLM L2 LCFA granule, committed to the tree.
///
/// 148 flashes, 2172 groups, 5941 events. Values in it are what NOAA actually
/// ships, including the `_Unsigned` packed shorts and a multi-chunk variable.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../rustdar-hdf5/testdata/",
    "OR_GLM-L2-LCFA_G19_s20251801200000_e20251801200200_c20251801200212.nc"
);

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(FIXTURE).expect("read the committed GLM granule")
}

// ---------------------------------------------------------------------------
// The netCDF-C side, expressed as the same `RawVar` the pure-Rust side builds.
//
// Both readers feed one implementation of the CF rules, so a difference in the
// unpacked output is a difference in the *bytes and attributes* the readers
// produced, never a difference in how they were interpreted. That is the point:
// it isolates the thing being migrated.
// ---------------------------------------------------------------------------

/// Build a [`RawVar`] through the netCDF-C library.
pub(crate) fn nc_raw_var(file: &netcdf::File, name: &str) -> Option<RawVar> {
    use netcdf::AttributeValue as A;
    use netcdf::types::{IntType, NcVariableType};

    let var = file.variable(name)?;

    let attrs: BTreeMap<String, CfAttr> = var
        .attributes()
        .filter_map(|a| {
            let v = a.value().ok()?;
            let cf = match v {
                A::Str(s) => CfAttr::Str(s),
                A::Uchar(x) => CfAttr::Nums(vec![f64::from(x)]),
                A::Schar(x) => CfAttr::Nums(vec![f64::from(x)]),
                A::Ushort(x) => CfAttr::Nums(vec![f64::from(x)]),
                A::Short(x) => CfAttr::Nums(vec![f64::from(x)]),
                A::Uint(x) => CfAttr::Nums(vec![f64::from(x)]),
                A::Int(x) => CfAttr::Nums(vec![f64::from(x)]),
                A::Ulonglong(x) => CfAttr::Nums(vec![x as f64]),
                A::Longlong(x) => CfAttr::Nums(vec![x as f64]),
                A::Float(x) => CfAttr::Nums(vec![f64::from(x)]),
                A::Double(x) => CfAttr::Nums(vec![x]),
                A::Uchars(x) => CfAttr::Nums(x.iter().copied().map(f64::from).collect()),
                A::Schars(x) => CfAttr::Nums(x.iter().copied().map(f64::from).collect()),
                A::Ushorts(x) => CfAttr::Nums(x.iter().copied().map(f64::from).collect()),
                A::Shorts(x) => CfAttr::Nums(x.iter().copied().map(f64::from).collect()),
                A::Uints(x) => CfAttr::Nums(x.iter().copied().map(f64::from).collect()),
                A::Ints(x) => CfAttr::Nums(x.iter().copied().map(f64::from).collect()),
                A::Ulonglongs(x) => CfAttr::Nums(x.iter().map(|&v| v as f64).collect()),
                A::Longlongs(x) => CfAttr::Nums(x.iter().map(|&v| v as f64).collect()),
                A::Floats(x) => CfAttr::Nums(x.iter().copied().map(f64::from).collect()),
                A::Doubles(x) => CfAttr::Nums(x.to_vec()),
                A::Strs(_) => return None,
            };
            Some((a.name().to_string(), cf))
        })
        .collect();

    let unsigned = attrs.get("_Unsigned").is_some_and(cf::attr_is_true);
    let nctype = var.vartype();

    let vartype = match &nctype {
        NcVariableType::Int(IntType::I8) => VarType::SignedInt(1),
        NcVariableType::Int(IntType::I16) => VarType::SignedInt(2),
        NcVariableType::Int(IntType::I32) => VarType::SignedInt(4),
        NcVariableType::Int(IntType::I64) => VarType::SignedInt(8),
        NcVariableType::Int(_) => VarType::UnsignedInt,
        NcVariableType::Float(_) => VarType::Float,
        other => panic!("{name}: unexpected netcdf type {other:?}"),
    };

    // Read in the declared width, exactly as the production reader must.
    let raw: Vec<f64> = match (&nctype, unsigned) {
        (NcVariableType::Int(IntType::I8), true) => var
            .get_values::<i8, _>(..)
            .unwrap()
            .into_iter()
            .map(|v| f64::from(v as u8))
            .collect(),
        (NcVariableType::Int(IntType::I16), true) => var
            .get_values::<i16, _>(..)
            .unwrap()
            .into_iter()
            .map(|v| f64::from(v as u16))
            .collect(),
        (NcVariableType::Int(IntType::I32), true) => var
            .get_values::<i32, _>(..)
            .unwrap()
            .into_iter()
            .map(|v| f64::from(v as u32))
            .collect(),
        (NcVariableType::Int(IntType::I64), true) => var
            .get_values::<i64, _>(..)
            .unwrap()
            .into_iter()
            .map(|v| v as u64 as f64)
            .collect(),
        _ => var.get_values::<f64, _>(..).unwrap(),
    };

    Some(RawVar { raw, vartype, unsigned, attrs })
}

/// Every variable the GLM overlay actually reads, at all three levels.
const OVERLAY_VARS: &[&str] = &[
    "flash_lat",
    "flash_lon",
    "flash_energy",
    "flash_area",
    "flash_time_offset_of_first_event",
    "group_lat",
    "group_lon",
    "group_energy",
    "group_area",
    "group_time_offset",
    "event_lat",
    "event_lon",
    "event_energy",
    "event_time_offset",
];

/// The packed `_Unsigned` `int16` variables — the ones CF unpacking can get
/// silently wrong. Named separately so a future edit that trims `OVERLAY_VARS`
/// cannot accidentally reduce this file to testing only plain floats.
const PACKED_VARS: &[&str] = &[
    "flash_energy",
    "flash_area",
    "flash_time_offset_of_first_event",
    "group_energy",
    "group_area",
    "group_time_offset",
    "event_lat",
    "event_lon",
    "event_energy",
    "event_time_offset",
];

/// Guard the guard: every name in `PACKED_VARS` must really be a packed,
/// `_Unsigned`, 16-bit variable in the fixture. If the product ever stopped
/// packing one, this file would go on "comparing packed variables" while
/// comparing plain floats, and the differential would lose its teeth silently.
#[test]
fn the_packed_variable_list_is_actually_packed() {
    let bytes = fixture_bytes();
    let g = Granule::open(&bytes).expect("open with hdf5-pure");
    for name in PACKED_VARS {
        let v = g
            .raw_var(name)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .unwrap_or_else(|| panic!("{name} missing from the fixture"));
        assert_eq!(
            v.vartype,
            VarType::SignedInt(2),
            "{name} is no longer 16-bit signed storage"
        );
        assert!(v.unsigned, "{name} no longer carries _Unsigned");
        assert!(
            v.attrs.contains_key("scale_factor"),
            "{name} no longer carries scale_factor, so nothing here exercises unpacking"
        );
        assert!(!v.raw.is_empty(), "{name} is empty in the fixture");
    }
}

/// The headline: both readers, through identical CF logic, on every variable
/// the overlay reads — compared **after** unpacking, which is the only place
/// the packed variables can be wrong.
#[test]
fn unpacked_values_match_the_c_netcdf_library_for_every_overlay_variable() {
    let bytes = fixture_bytes();
    let nc = netcdf::open_mem(None, &bytes).expect("netcdf open_mem");
    let g = Granule::open(&bytes).expect("open with hdf5-pure");

    for name in OVERLAY_VARS {
        let theirs = nc_raw_var(&nc, name).unwrap_or_else(|| panic!("{name} missing from netcdf"));
        let ours = g
            .raw_var(name)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .unwrap_or_else(|| panic!("{name} missing from hdf5-pure"));

        // The raw domain first: this is the bytes-off-disk step, and comparing
        // it separately says whether a mismatch came from reading or from
        // interpreting.
        assert_eq!(ours.vartype, theirs.vartype, "{name}: storage type differs");
        assert_eq!(ours.unsigned, theirs.unsigned, "{name}: _Unsigned differs");
        assert_eq!(ours.raw, theirs.raw, "{name}: raw stored values differ");

        // Then the unpacked domain, which is what reaches the map.
        let a = cf::unpack(&ours, name);
        let b = cf::unpack(&theirs, name);
        assert_eq!(a.units, b.units, "{name}: units differ");
        assert_eq!(
            a.values, b.values,
            "{name}: CF-unpacked values differ between hdf5-pure and netcdf-c"
        );
        assert!(!a.values.is_empty(), "{name} unexpectedly empty");
    }
}

/// The attribute tables must agree too, not just the values they happen to
/// produce. A `scale_factor` that one reader cannot see is a silent identity
/// multiply, and on `flash_area` that is the difference between 278 km² and a
/// raw count of 1826.
#[test]
fn cf_attributes_match_the_c_netcdf_library() {
    let bytes = fixture_bytes();
    let nc = netcdf::open_mem(None, &bytes).expect("netcdf open_mem");
    let g = Granule::open(&bytes).expect("open with hdf5-pure");

    for name in OVERLAY_VARS {
        let theirs = nc_raw_var(&nc, name).unwrap();
        let ours = g.raw_var(name).unwrap().unwrap();
        for key in ["scale_factor", "add_offset", "_FillValue", "valid_range", "units", "_Unsigned"]
        {
            assert_eq!(
                ours.attrs.get(key),
                theirs.attrs.get(key),
                "{name}: attribute {key} differs"
            );
        }
    }
}

/// `group_lat` is stored in **9 chunks** of 256 (2172 elements). `flash_lat`
/// is a single chunk at offset 0, so chunk *placement* is unobservable through
/// it: a reader that ignored the chunk index entirely would still return
/// `flash_lat` correctly. This variable is the one that can tell.
#[test]
fn a_multi_chunk_variable_matches_the_c_netcdf_library() {
    let bytes = fixture_bytes();
    let nc = netcdf::open_mem(None, &bytes).expect("netcdf open_mem");
    let g = Granule::open(&bytes).expect("open with hdf5-pure");

    let theirs = nc_raw_var(&nc, "group_lat").unwrap();
    let ours = g.raw_var("group_lat").unwrap().unwrap();
    assert_eq!(ours.raw.len(), 2172);
    assert_eq!(ours.raw, theirs.raw, "group_lat differs across chunk boundaries");

    // Pin that it really is multi-chunk, so the test cannot quietly degrade
    // into a second single-chunk case if the product's chunking changes.
    let f = hdf5_pure::File::from_bytes(bytes.clone()).unwrap();
    let ds = f.dataset("group_lat").unwrap();
    assert!(ds.is_chunked(), "group_lat is no longer chunked");
    let chunk = ds.chunk_shape().unwrap().expect("chunk shape");
    assert!(
        2172u64.div_ceil(chunk[0]) >= 2,
        "group_lat is now a single chunk ({chunk:?}); chunk placement is untested"
    );
}

/// The whole granule, end to end, through the real entry point: the records
/// the overlay would plot must be identical whichever reader produced them.
///
/// This is the test that would catch a mistake made *between* the reader and
/// the map — unit conversion, longitude normalization, time-axis handling —
/// rather than only inside the reader.
#[test]
fn parsed_records_match_the_c_netcdf_library_end_to_end() {
    use super::fetch::parse_glm_netcdf_via_netcdf;
    use super::{GlmDataLevel, GlmSatellite};

    let bytes = fixture_bytes();
    let levels = [GlmDataLevel::Flash, GlmDataLevel::Group, GlmDataLevel::Event];

    let ours = super::fetch::parse_glm_netcdf(&bytes, GlmSatellite::GoesEast, &levels)
        .expect("pure-Rust parse");
    let theirs = parse_glm_netcdf_via_netcdf(&bytes, GlmSatellite::GoesEast, &levels)
        .expect("netcdf-c parse");

    assert_eq!(ours.records.len(), theirs.records.len(), "record count differs");
    assert!(
        ours.records.len() > 8000,
        "expected the full 148 + 2172 + 5941 granule, got {}",
        ours.records.len()
    );
    for (i, (a, b)) in ours.records.iter().zip(&theirs.records).enumerate() {
        assert_eq!(a.lat, b.lat, "record {i}: lat");
        assert_eq!(a.lon, b.lon, "record {i}: lon");
        assert_eq!(a.energy, b.energy, "record {i}: energy");
        assert_eq!(a.area, b.area, "record {i}: area");
        assert_eq!(a.time, b.time, "record {i}: time");
        assert_eq!(a.level, b.level, "record {i}: level");
    }
}

/// The ten datasets in this product with no allocated storage.
///
/// Seven are metadata containers (`goes_lat_lon_projection` and friends) and
/// three are zero-length dimension placeholders. `h5py` synthesises fill values
/// for them; `hdf5_pure` returns a typed error. Neither matters to the overlay,
/// which never asks for any of them — and that is the property under test: one
/// unreadable metadata container must not be able to take a granule down.
#[test]
fn unallocated_metadata_datasets_do_not_break_a_granule_parse() {
    use super::{GlmDataLevel, GlmSatellite};

    let bytes = fixture_bytes();
    let g = Granule::open(&bytes).expect("open");

    // The zero-length placeholders read as empty, because that is what they
    // are — and an empty granule must not be an error.
    for name in ["number_of_flashes", "number_of_groups", "number_of_events"] {
        let v = g
            .raw_var(name)
            .unwrap_or_else(|e| panic!("{name} should read as empty, got {e}"))
            .unwrap_or_else(|| panic!("{name} missing"));
        assert!(v.raw.is_empty(), "{name} should be empty");
    }

    // The scalar metadata containers are a different case: they *claim*
    // elements they have no storage for. That is reported rather than
    // fabricated — but it is confined to the variable asked for.
    for name in ["goes_lat_lon_projection", "algorithm_product_version_container"] {
        assert!(
            g.raw_var(name).is_err(),
            "{name} claims elements it has no storage for; that must not read as data"
        );
    }

    // And the granule as a whole parses regardless.
    let parsed = super::fetch::parse_glm_netcdf(
        &bytes,
        GlmSatellite::GoesEast,
        &[GlmDataLevel::Flash, GlmDataLevel::Group, GlmDataLevel::Event],
    )
    .expect("granule must parse despite unreadable metadata containers");
    assert!(parsed.level_failures.is_empty(), "no level should have failed");
}
