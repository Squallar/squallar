use super::*;

/// A packed `u16` above 32767 reads back as a *negative* `i16`; recovering
/// it is a bit reinterpretation.
///
/// Values are the first real `event_lat` sample from `noaa-goes19` granule
/// `OR_GLM-L2-LCFA_G19_s20262051200000_...`.
#[test]
fn unsigned_short_above_32767_reinterprets_not_negates() {
    let ty = VarType::SignedInt(2);
    assert_eq!(reinterpret_unsigned(-13585.0, ty, true), 51951.0);
    // Without `_Unsigned` the same bits stay negative.
    assert_eq!(reinterpret_unsigned(-13585.0, ty, false), -13585.0);
    // Values below 32768 are unaffected either way.
    assert_eq!(reinterpret_unsigned(11048.0, ty, true), 11048.0);

    // ...and it must unpack to a real latitude in Ohio.
    let lat: f64 = 51951.0 * 0.00203128 + -66.56;
    assert!((lat - 38.967).abs() < 1e-3, "got {lat}");
    let wrong: f64 = -13585.0 * 0.00203128 + -66.56;
    assert!(
        wrong < -90.0,
        "the unfixed path is not even a valid latitude"
    );
}

/// `_FillValue = -1s` under `_Unsigned` means 65535, and `valid_range =
/// 0s, -6s` means `0..=65530`. A signed-domain comparison never matches, so
/// the fill gets scaled and published as a real number.
#[test]
fn fill_and_valid_range_reinterpret_through_unsigned() {
    let ty = VarType::SignedInt(2);
    assert_eq!(reinterpret_unsigned(-1.0, ty, true), 65535.0);
    assert_eq!(reinterpret_unsigned(-6.0, ty, true), 65530.0);
    assert_eq!(reinterpret_unsigned(0.0, ty, true), 0.0);
}

#[test]
fn unsigned_reinterpretation_covers_the_other_int_widths() {
    assert_eq!(
        reinterpret_unsigned(-1.0, VarType::SignedInt(1), true),
        255.0
    );
    assert_eq!(
        reinterpret_unsigned(-1.0, VarType::SignedInt(4), true),
        4_294_967_295.0
    );
    assert_eq!(
        reinterpret_unsigned(-1.0, VarType::SignedInt(8), true),
        18_446_744_073_709_551_615.0
    );
    // Native unsigned storage is already correct and must be left alone.
    assert_eq!(
        reinterpret_unsigned(65535.0, VarType::UnsignedInt, true),
        65535.0
    );
    // Floats are never reinterpreted.
    assert_eq!(
        reinterpret_unsigned(-13585.0, VarType::Float, true),
        -13585.0
    );
}

#[test]
fn unsigned_attribute_accepts_string_and_numeric_spellings() {
    assert!(attr_is_true(&CfAttr::Str("true".into())));
    assert!(attr_is_true(&CfAttr::Str("True".into())));
    assert!(attr_is_true(&CfAttr::Str("1".into())));
    assert!(attr_is_true(&CfAttr::Nums(vec![1.0])));
    assert!(!attr_is_true(&CfAttr::Str("false".into())));
    assert!(!attr_is_true(&CfAttr::Nums(vec![0.0])));
}

#[test]
fn scalar_attributes_written_as_length_one_vectors_still_read() {
    assert_eq!(attr_as_f64(&CfAttr::Nums(vec![0.5])), Some(0.5));
    assert_eq!(attr_as_f64(&CfAttr::Nums(vec![0.5, 9.0])), Some(0.5));
    assert_eq!(attr_as_f64(&CfAttr::Nums(vec![-1.0])), Some(-1.0));
    assert_eq!(attr_as_f64(&CfAttr::Str("nope".into())), None);
}

#[test]
fn cf_time_units_parse_unit_and_epoch() {
    let t = parse_time_units("seconds since 2026-07-24 12:00:00.000").expect("parse");
    assert_eq!(t.seconds_per_unit, 1.0);
    assert_eq!(
        t.epoch,
        chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    );

    assert_eq!(
        parse_time_units("milliseconds since 2026-07-24T12:00:00Z")
            .expect("parse")
            .seconds_per_unit,
        1e-3
    );
    // A units string we do not understand must not be guessed at.
    assert!(parse_time_units("fortnights since 2026-07-24").is_none());
    assert!(parse_time_units("degrees_north").is_none());
}

#[test]
fn cf_epoch_accepts_the_shapes_glm_writes() {
    let expect = chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    // `time_coverage_start` global attribute form.
    assert_eq!(parse_cf_epoch("2026-07-24T12:00:00.0Z"), Some(expect));
    // `units` attribute form.
    assert_eq!(parse_cf_epoch("2026-07-24 12:00:00.000"), Some(expect));
    assert_eq!(
        parse_cf_epoch("2026-07-24"),
        expect.date().and_hms_opt(0, 0, 0)
    );
    assert!(parse_cf_epoch("not a date").is_none());
}

// ---------------------------------------------------------------------
// File-backed tests: real NetCDF4 files through the same library that
// reads the GOES granules, so the whole read path is exercised. A unit
// test on `reinterpret_unsigned` alone cannot catch a reader that widens a
// packed `short` before anyone looks at `_Unsigned`.
// ---------------------------------------------------------------------

/// Description of a packed 16-bit variable, mirroring how GLM writes one.
struct ShortVar<'a> {
    values: &'a [i16],
    unsigned: bool,
    scale: Option<f32>,
    offset: Option<f32>,
    fill: Option<i16>,
    /// Written verbatim, element count and all — a `Vec` rather than a pair
    /// because a malformed (not-exactly-two) `valid_range` is under test.
    valid_range: Option<Vec<i16>>,
    units: Option<&'a str>,
}

impl Default for ShortVar<'_> {
    fn default() -> Self {
        Self {
            values: &[],
            unsigned: true,
            scale: None,
            offset: None,
            fill: None,
            valid_range: None,
            units: None,
        }
    }
}

/// Write a single packed variable to an HDF5 file and hand back its bytes.
///
/// `scale_factor`/`add_offset` are taken as `f32` — GLM's width — and
/// widened before writing, so the file holds exactly what a `float`
/// attribute decodes to. `152601.9_f64` is a different constant from the
/// `152601.859375` the real product yields.
fn short_var_file(spec: &ShortVar<'_>) -> Vec<u8> {
    let mut w = hdf5_pure::FileBuilder::new();
    let b = w.create_dataset("v");
    b.with_i16_data(spec.values);
    if spec.unsigned {
        b.set_attr("_Unsigned", hdf5_pure::AttrValue::String("true".into()));
    }
    if let Some(f) = spec.fill {
        b.set_attr("_FillValue", hdf5_pure::AttrValue::I64(i64::from(f)));
    }
    if let Some(range) = &spec.valid_range {
        b.set_attr(
            "valid_range",
            hdf5_pure::AttrValue::I64Array(range.iter().map(|&v| i64::from(v)).collect()),
        );
    }
    if let Some(s) = spec.scale {
        b.set_attr("scale_factor", hdf5_pure::AttrValue::F64(f64::from(s)));
    }
    if let Some(o) = spec.offset {
        b.set_attr("add_offset", hdf5_pure::AttrValue::F64(f64::from(o)));
    }
    if let Some(u) = spec.units {
        b.set_attr("units", hdf5_pure::AttrValue::String(u.into()));
    }
    w.finish().expect("write packed fixture")
}

/// Write a single unpacked `float` variable — GLM's `group_lat`/`flash_lat`
/// shape — and hand back its bytes.
fn float_var_file(values: &[f32], units: Option<&str>) -> Vec<u8> {
    let mut w = hdf5_pure::FileBuilder::new();
    let b = w.create_dataset("v");
    b.with_f32_data(values);
    if let Some(u) = units {
        b.set_attr("units", hdf5_pure::AttrValue::String(u.into()));
    }
    w.finish().expect("write float fixture")
}

fn read_v(bytes: &[u8]) -> UnpackedVar {
    super::super::h5::Granule::open(bytes)
        .expect("open")
        .read_unpacked("v")
        .expect("read")
        .expect("variable present")
}

/// A `u16` above 32767 arrives as a negative `i16` and must come out of the
/// unpacker as the right latitude. `51951` is stored as `-13585`, which is
/// the real granule's first `event_lat`.
#[test]
fn packed_unsigned_short_above_32767_unpacks_correctly_from_a_file() {
    let bytes = short_var_file(&ShortVar {
        values: &[-13585, -13546, 11048],
        unsigned: true,
        scale: Some(0.00203128),
        offset: Some(-66.56),
        units: Some("degrees_north"),
        ..Default::default()
    });
    let v = read_v(&bytes);
    assert_eq!(v.units.as_deref(), Some("degrees_north"));

    let got: Vec<f64> = v.values.iter().map(|x| x.expect("no fill")).collect();
    assert!((got[0] - 38.9670).abs() < 1e-3, "got {}", got[0]);
    assert!((got[1] - 39.0462).abs() < 1e-3, "got {}", got[1]);
    // Below 32767 the signed and unsigned readings agree.
    assert!((got[2] - (-44.11)).abs() < 1e-2, "got {}", got[2]);

    // The unreinterpreted reading is not a latitude at all.
    assert!(got[0] > 0.0 && got[0] < 90.0);
}

/// Without `_Unsigned` the very same bytes must stay negative — the
/// attribute is what changes the meaning, not the storage type.
#[test]
fn short_without_unsigned_attribute_keeps_its_sign() {
    let bytes = short_var_file(&ShortVar {
        values: &[-13585],
        unsigned: false,
        scale: Some(1.0),
        ..Default::default()
    });
    assert_eq!(read_v(&bytes).values[0], Some(-13585.0));
}

/// `_FillValue = -1s` under `_Unsigned` means 65535. A signed-domain
/// comparison misses it and publishes `65535 * scale + offset`.
#[test]
fn fill_value_becomes_missing_not_a_number() {
    let bytes = short_var_file(&ShortVar {
        values: &[1826, -1, 491],
        unsigned: true,
        scale: Some(152601.9),
        offset: Some(0.0),
        fill: Some(-1),
        units: Some("m2"),
        ..Default::default()
    });
    let v = read_v(&bytes);
    assert_eq!(v.values[1], None, "the _FillValue element must be missing");
    // The file stores `scale_factor` as f32, so the exact expectation has
    // to come through f32 too.
    let scale = f64::from(152_601.9_f32);
    assert!((v.values[0].unwrap() - 1826.0 * scale).abs() < 1e-6);
    assert!((v.values[2].unwrap() - 491.0 * scale).abs() < 1e-6);
    // Which is 278.65 km² — not the raw count of 1826.
    assert!((v.values[0].unwrap() / 1e6 - 278.65).abs() < 0.01);
}

/// GLM writes `valid_range = 0s, -6s`, which under `_Unsigned` is
/// `0..=65530` — the same signed/unsigned trap a second time.
#[test]
fn out_of_valid_range_becomes_missing() {
    let bytes = short_var_file(&ShortVar {
        values: &[100, -3, 200], // -3 == 65533 unsigned, above the 65530 cap
        unsigned: true,
        valid_range: Some(vec![0, -6]),
        scale: Some(1.0),
        ..Default::default()
    });
    let v = read_v(&bytes);
    assert_eq!(v.values[0], Some(100.0));
    assert_eq!(v.values[1], None);
    assert_eq!(v.values[2], Some(200.0));
}

/// A missing `scale_factor` means 1 and a missing `add_offset` means 0 —
/// never zero and never "skip the whole unpack".
#[test]
fn missing_packing_attributes_default_to_identity() {
    let scale_only = read_v(&short_var_file(&ShortVar {
        values: &[10],
        unsigned: true,
        scale: Some(2.5),
        offset: None,
        ..Default::default()
    }));
    assert_eq!(scale_only.values[0], Some(25.0));

    let offset_only = read_v(&short_var_file(&ShortVar {
        values: &[10],
        unsigned: true,
        scale: None,
        offset: Some(-5.0),
        ..Default::default()
    }));
    assert_eq!(offset_only.values[0], Some(5.0));

    let neither = read_v(&short_var_file(&ShortVar {
        values: &[10],
        unsigned: true,
        ..Default::default()
    }));
    assert_eq!(neither.values[0], Some(10.0));
}

/// Variables genuinely stored as `float` with no packing attributes — GLM's
/// `group_lat`/`flash_lat` — must come through bit-for-bit.
#[test]
fn float_variable_passes_through_unchanged() {
    let bytes = float_var_file(&[39.033424_f32, -22.65055, 55.2922], Some("degrees_north"));

    let v = read_v(&bytes);
    assert_eq!(v.values[0], Some(f64::from(39.033424_f32)));
    assert_eq!(v.values[1], Some(f64::from(-22.65055_f32)));
    assert_eq!(v.values[2], Some(f64::from(55.2922_f32)));
}

/// An inverted `valid_range` is refused, not honoured.
///
/// "Inverted" is only decidable *after* `_Unsigned` reinterpretation: GLM's
/// real range `0s, -6s` is `0..=65530`, while the transposed `-6s, 0s` is
/// `65530..=0`, which matches nothing and empties the variable. A `lo > hi`
/// check in the *signed* domain gets this exactly backwards — it accepts
/// the inverted range and rejects the real one.
#[test]
fn an_inverted_valid_range_is_refused_rather_than_emptying_the_variable() {
    let inverted = read_v(&short_var_file(&ShortVar {
        // `-6` on disk, i.e. raw 65530 — the value that would sit exactly on
        // the cap if the transposed range were honoured.
        values: &[100, 200, -6],
        unsigned: true,
        valid_range: Some(vec![-6, 0]), // == 65530..=0 once reinterpreted
        scale: Some(1.0),
        ..Default::default()
    }));
    assert_eq!(
        inverted.values,
        vec![Some(100.0), Some(200.0), Some(65530.0)],
        "an inverted range must be dropped, not applied; applying it empties \
             the variable and renders identically to a quiet sky"
    );

    // The same two bounds the right way round are the real GLM attribute,
    // and must still be enforced.
    let correct = read_v(&short_var_file(&ShortVar {
        values: &[100, 200, -3], // -3 == 65533, past the 65530 cap
        unsigned: true,
        valid_range: Some(vec![0, -6]),
        scale: Some(1.0),
        ..Default::default()
    }));
    assert_eq!(correct.values, vec![Some(100.0), Some(200.0), None]);

    // Inversion is refused on a plainly signed variable too.
    let signed = read_v(&short_var_file(&ShortVar {
        values: &[-20, 0, 20],
        unsigned: false,
        valid_range: Some(vec![10, 5]),
        scale: Some(1.0),
        ..Default::default()
    }));
    assert_eq!(signed.values, vec![Some(-20.0), Some(0.0), Some(20.0)]);
}

/// `valid_range` is defined as exactly two elements; anything else is
/// malformed and ignored wholesale. Taking the first two of a longer
/// attribute applies a range the file never declared, and `bounds[1]` of a
/// shorter one panics on a granule the user cannot control.
#[test]
fn a_valid_range_that_is_not_two_elements_is_ignored() {
    // Three elements, whose first two are the real GLM range — honouring
    // them would drop 65533 and look plausible.
    let three = read_v(&short_var_file(&ShortVar {
        values: &[100, -3],
        unsigned: true,
        valid_range: Some(vec![0, -6, 99]),
        scale: Some(1.0),
        ..Default::default()
    }));
    assert_eq!(
        three.values,
        vec![Some(100.0), Some(65533.0)],
        "a 3-element valid_range is malformed; its first two elements are \
             not a range the file declared"
    );

    // One element: there is no upper bound to read at all.
    let one = read_v(&short_var_file(&ShortVar {
        values: &[100, -3],
        unsigned: true,
        valid_range: Some(vec![0]),
        scale: Some(1.0),
        ..Default::default()
    }));
    assert_eq!(one.values, vec![Some(100.0), Some(65533.0)]);
}

/// A value that unpacks to NaN or ±inf is missing, not a measurement. Two
/// routes in: coefficients that overflow `raw * scale + offset`, and a
/// genuine `float` variable (GLM's `flash_lat`) holding NaN on disk with no
/// `_FillValue` declared, where CF's fill machinery never sees it.
///
/// A published NaN propagates into the projection instead of announcing
/// itself — `rasterize` sizes bolts by `energy.log10()`.
#[test]
fn non_finite_unpacked_values_are_missing_not_published() {
    // 100 * inf == inf, 0 * inf == NaN.
    let overflowed = read_v(&short_var_file(&ShortVar {
        values: &[100, 0],
        unsigned: true,
        scale: Some(f32::INFINITY),
        ..Default::default()
    }));
    assert_eq!(
        overflowed.values,
        vec![None, None],
        "a value that unpacked to inf/NaN must not reach a caller as a number"
    );

    // The offset is the other half of the same expression.
    let offset_nan = read_v(&short_var_file(&ShortVar {
        values: &[100],
        unsigned: true,
        scale: Some(1.0),
        offset: Some(f32::NAN),
        ..Default::default()
    }));
    assert_eq!(offset_nan.values, vec![None]);

    // Unpacked `float` storage, where there is no arithmetic to blame.
    let raw = read_v(&float_var_file(
        &[1.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY],
        Some("degrees_north"),
    ));
    assert_eq!(raw.values, vec![Some(1.5), None, None, None]);
}
