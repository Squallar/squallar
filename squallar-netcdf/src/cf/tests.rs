use super::*;

/// A packed `u16` above 32767 reads back as a *negative* `i16`; recovering it
/// is a bit reinterpretation.
///
/// Values are the first real `event_lat` sample from `noaa-goes19` granule
/// `OR_GLM-L2-LCFA_G19_s20262051200000_...`.
#[test]
fn unsigned_short_above_32767_reinterprets_not_negates() {
    let ty = VarType::SignedInt(2);
    assert_eq!(reinterpret_unsigned(-13585.0, ty, true), 51951.0);
    assert_eq!(reinterpret_unsigned(-13585.0, ty, false), -13585.0);
    assert_eq!(reinterpret_unsigned(11048.0, ty, true), 11048.0);

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
    assert_eq!(
        reinterpret_unsigned(65535.0, VarType::UnsignedInt, true),
        65535.0
    );
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
    assert!(parse_time_units("fortnights since 2026-07-24").is_none());
    assert!(parse_time_units("degrees_north").is_none());
}

#[test]
fn cf_epoch_accepts_the_shapes_glm_writes() {
    let expect = chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    assert_eq!(parse_cf_epoch("2026-07-24T12:00:00.0Z"), Some(expect));
    assert_eq!(parse_cf_epoch("2026-07-24 12:00:00.000"), Some(expect));
    assert_eq!(
        parse_cf_epoch("2026-07-24"),
        expect.date().and_hms_opt(0, 0, 0)
    );
    assert!(parse_cf_epoch("not a date").is_none());
}

// File-backed tests: real NetCDF4 files through the same library that reads the
// GOES granules, so the whole read path is exercised.

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
/// `scale_factor`/`add_offset` are taken as `f32` — GLM's width — so the file
/// holds exactly what a `float` attribute decodes to.
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

/// Write a single unpacked `float` variable — GLM's `group_lat` shape.
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
    crate::h5::Granule::open(bytes)
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
    assert!((got[2] - (-44.11)).abs() < 1e-2, "got {}", got[2]);

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
/// "Inverted" is only decidable *after* `_Unsigned` reinterpretation: GLM's real
/// range `0s, -6s` is `0..=65530`, while the transposed `-6s, 0s` is
/// `65530..=0`, which matches nothing. A `lo > hi` check in the *signed* domain
/// gets this exactly backwards.
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

    let correct = read_v(&short_var_file(&ShortVar {
        values: &[100, 200, -3], // -3 == 65533, past the 65530 cap
        unsigned: true,
        valid_range: Some(vec![0, -6]),
        scale: Some(1.0),
        ..Default::default()
    }));
    assert_eq!(correct.values, vec![Some(100.0), Some(200.0), None]);

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

    let one = read_v(&short_var_file(&ShortVar {
        values: &[100, -3],
        unsigned: true,
        valid_range: Some(vec![0]),
        scale: Some(1.0),
        ..Default::default()
    }));
    assert_eq!(one.values, vec![Some(100.0), Some(65533.0)]);
}

/// A value that unpacks to NaN or ±inf is missing, not a measurement. Two routes
/// in: coefficients that overflow `raw * scale + offset`, and a genuine `float`
/// variable holding NaN on disk with no `_FillValue` declared.
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

// ── The two representations of the same values ───────────────────────────

/// How many bytes one element costs, read off a *real* returned value rather
/// than off a type spelled in the assertion.
///
/// The indirection is the point: a revert of [`UnpackedF32::values`] to the
/// `Option` form has to move this number, so the size floor below cannot pass
/// vacuously.
fn element_bytes<T>(_: &[T]) -> usize {
    std::mem::size_of::<T>()
}

fn raw_of(bytes: &[u8]) -> RawVar {
    crate::h5::Granule::open(bytes)
        .expect("open")
        .raw_var("v")
        .expect("read")
        .expect("variable present")
}

/// `Option<f64>` is 16 bytes per element, because `f64` has no spare bit
/// pattern for a discriminant to hide in; `f32` is 4. For a column of a few
/// hundred records that is nothing. For a raster it is the whole cost.
///
/// **Denominator**: one variable of 15,000,000 elements, *values only*. Neither
/// figure counts the `units` string or the `Vec` header — those are per
/// variable, not per element, and identical in both forms.
#[test]
fn the_raster_form_costs_a_quarter_of_the_option_form() {
    const N: usize = 15_000_000;

    let raw = raw_of(&short_var_file(&ShortVar {
        values: &[1, 2, 3],
        ..Default::default()
    }));
    let option_bytes = element_bytes(&unpack(&raw, "v").values);
    let raster_bytes = element_bytes(&unpack_f32(&raw, "v").values);

    assert_eq!(option_bytes, 16, "Option<f64> is 16 bytes per element");
    assert_eq!(raster_bytes, 4, "f32 is 4 bytes per element");

    let option_total = option_bytes * N;
    let raster_total = raster_bytes * N;
    assert_eq!(option_total, 240_000_000, "the Option form at 15,000,000");
    assert_eq!(raster_total, 60_000_000, "the raster form at 15,000,000");
    assert!(
        raster_total * 4 <= option_total,
        "the raster form must stay at least 4x cheaper: {raster_total} vs {option_total}"
    );
}

/// The cheap form is the same arithmetic, not a second implementation of it.
///
/// One `Packing` serves both, and this is what says so from the outside: every
/// element agrees, missing for missing and value for value, across packed
/// unsigned shorts, a `_FillValue`, a `valid_range` and plain floats.
#[test]
fn the_two_representations_agree_element_for_element() {
    let cases: Vec<Vec<u8>> = vec![
        short_var_file(&ShortVar {
            values: &[-13585, -13546, 11048],
            unsigned: true,
            scale: Some(0.00203128),
            offset: Some(-66.56),
            ..Default::default()
        }),
        short_var_file(&ShortVar {
            values: &[5, -1, 7],
            unsigned: true,
            fill: Some(-1),
            ..Default::default()
        }),
        short_var_file(&ShortVar {
            values: &[0, -3, 10],
            unsigned: true,
            valid_range: Some(vec![0, -6]),
            ..Default::default()
        }),
        float_var_file(&[1.5, -2.25, 0.0], Some("degrees_north")),
    ];

    for (n, bytes) in cases.iter().enumerate() {
        let raw = raw_of(bytes);
        let opt = unpack(&raw, "v");
        let f32s = unpack_f32(&raw, "v");

        assert_eq!(opt.units, f32s.units, "case {n}: units");
        assert_eq!(opt.values.len(), f32s.values.len(), "case {n}: length");
        for (i, (o, f)) in opt.values.iter().zip(f32s.values.iter()).enumerate() {
            match o {
                None => assert!(f.is_nan(), "case {n} element {i}: missing must be NaN"),
                Some(v) => assert_eq!(
                    *f, *v as f32,
                    "case {n} element {i}: present values must match"
                ),
            }
        }
    }
}

/// `NaN` is an unambiguous "missing" marker in [`UnpackedF32`] only because no
/// *present* value can be `NaN`.
///
/// [`unpack`] already reports a non-finite unpacked value as missing — see
/// [`non_finite_unpacked_values_are_missing_not_published`] — so the two facts
/// have to be checked together, on the same inputs, in both representations.
#[test]
fn a_non_finite_unpacked_value_is_missing_in_both_representations() {
    // Arithmetic that overflows: 100 * inf == inf, 0 * inf == NaN.
    let overflowed = raw_of(&short_var_file(&ShortVar {
        values: &[100, 0],
        unsigned: true,
        scale: Some(f32::INFINITY),
        ..Default::default()
    }));
    assert_eq!(unpack(&overflowed, "v").values, vec![None, None]);
    assert!(
        unpack_f32(&overflowed, "v")
            .values
            .iter()
            .all(|v| v.is_nan()),
        "an overflowed value must be NaN in the raster form, not inf"
    );

    // `float` storage holding non-finite bytes, with no arithmetic to blame.
    let stored = raw_of(&float_var_file(
        &[1.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY],
        Some("degrees_north"),
    ));
    let values = unpack_f32(&stored, "v").values;
    assert_eq!(values[0], 1.5, "the one finite value survives");
    assert!(
        values[1..].iter().all(|v| v.is_nan()),
        "stored inf must arrive as NaN, never as inf: {values:?}"
    );
}

// ── The raw domain is the storage width, and loses nothing ───────────────

/// Write one variable at a chosen storage width, optionally with an
/// `add_offset`.
///
/// A macro rather than a generic function because `FileBuilder` names the
/// width in the method (`with_u16_data`, `with_i64_data`, …) and there is no
/// trait over them.
macro_rules! typed_var_file {
    ($writer:ident, $values:expr) => {
        typed_var_file!($writer, $values, None)
    };
    ($writer:ident, $values:expr, $offset:expr) => {{
        let mut w = hdf5_pure::FileBuilder::new();
        let b = w.create_dataset("v");
        b.$writer($values);
        let offset: Option<f64> = $offset;
        if let Some(o) = offset {
            b.set_attr("add_offset", hdf5_pure::AttrValue::F64(o));
        }
        w.finish().expect("write the typed fixture")
    }};
}

/// Both representations of a variable, against expectations written by hand.
///
/// `expected` is the raw domain: the values the CF rules say the file means,
/// derived from the on-disk integers and the packing attributes rather than
/// read back off this crate's own output. `to_bits` rather than `==`, so a
/// value that is merely *close* is a failure and `-0.0` is not `0.0`.
fn expect_exact(bytes: &[u8], expected: &[f64], what: &str) {
    let raw = raw_of(bytes);
    assert_eq!(
        raw.raw.len(),
        expected.len(),
        "{what}: stored element count"
    );

    let wide = unpack(&raw, what).values;
    for (i, (got, want)) in wide.iter().zip(expected).enumerate() {
        let got = got.unwrap_or_else(|| panic!("{what}[{i}] came back missing"));
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "{what}[{i}]: unpack gave {got:e}, the file means {want:e}"
        );
    }

    let narrow = unpack_f32(&raw, what).values;
    for (i, (got, want)) in narrow.iter().zip(expected).enumerate() {
        assert_eq!(
            got.to_bits(),
            (*want as f32).to_bits(),
            "{what}[{i}]: unpack_f32 gave {got:e}, the raw domain rounds to {:e}",
            *want as f32
        );
    }
}

/// **Every storage width reaches the raw domain as the exact value on disk.**
///
/// The byte-exactness net under [`RawValues`]. Each row carries a value the
/// width can hold and a *narrower* float cannot — `i32::MAX` and `u32::MAX`
/// round up in `f32`, `2^24 + 1` rounds down, `1.0 + f64::EPSILON` collapses to
/// `1.0` — so an implementation that widened through anything narrower than
/// `f64` reddens here rather than on a granule.
///
/// Identity packing throughout: with no `scale_factor` or `add_offset` the
/// expected value *is* the storage value, so nothing here is a re-recording of
/// the unpacker's arithmetic.
#[test]
fn every_storage_width_unpacks_to_the_same_bits_it_always_did() {
    expect_exact(
        &typed_var_file!(with_f32_data, &[1.5f32, -2.25, -0.0, f32::MAX]),
        &[1.5, -2.25, -0.0, f64::from(f32::MAX)],
        "f32",
    );
    expect_exact(
        &typed_var_file!(
            with_f64_data,
            &[
                0.1f64,
                1.0 + f64::EPSILON,
                -1.234_567_890_123_456_7e300,
                f64::MAX
            ]
        ),
        &[
            0.1,
            1.0 + f64::EPSILON,
            -1.234_567_890_123_456_7e300,
            f64::MAX,
        ],
        "f64",
    );
    expect_exact(
        &typed_var_file!(with_i8_data, &[i8::MIN, -1, 0, i8::MAX]),
        &[-128.0, -1.0, 0.0, 127.0],
        "i8",
    );
    expect_exact(
        &typed_var_file!(with_i16_data, &[i16::MIN, -1, 0, i16::MAX]),
        &[-32768.0, -1.0, 0.0, 32767.0],
        "i16",
    );
    expect_exact(
        &typed_var_file!(with_i32_data, &[i32::MIN, -1, 0, 16_777_217, i32::MAX]),
        &[-2_147_483_648.0, -1.0, 0.0, 16_777_217.0, 2_147_483_647.0],
        "i32",
    );
    // `2^53` is the last integer `f64` holds with its neighbour; above it the
    // `as` cast rounds, exactly as it always has.
    expect_exact(
        &typed_var_file!(
            with_i64_data,
            &[-9_007_199_254_740_992i64, -1, 0, 9_007_199_254_740_992]
        ),
        &[-9_007_199_254_740_992.0, -1.0, 0.0, 9_007_199_254_740_992.0],
        "i64",
    );
    expect_exact(
        &typed_var_file!(with_u8_data, &[0u8, 1, 128, 255]),
        &[0.0, 1.0, 128.0, 255.0],
        "u8",
    );
    expect_exact(
        &typed_var_file!(with_u16_data, &[0u16, 1, 32768, 65535]),
        &[0.0, 1.0, 32768.0, 65535.0],
        "u16",
    );
    expect_exact(
        &typed_var_file!(with_u32_data, &[0u32, 16_777_217, u32::MAX]),
        &[0.0, 16_777_217.0, 4_294_967_295.0],
        "u32",
    );
    expect_exact(
        &typed_var_file!(with_u64_data, &[0u64, 16_777_217, 9_007_199_254_740_992]),
        &[0.0, 16_777_217.0, 9_007_199_254_740_992.0],
        "u64",
    );
}

/// **A source whose precision exceeds `f32` survives into the raster too.**
///
/// The gate that stops [`RawValues`] becoming a silent correctness regression.
/// [`every_storage_width_unpacks_to_the_same_bits_it_always_did`] catches a
/// narrowed raw domain in the `Option<f64>` form; this catches it in the `f32`
/// form, which is the one a raster reads and the one where "the output is
/// `f32` anyway" is the tempting wrong answer.
///
/// The construction is what makes it bite: the storage value needs more than
/// `f32`'s 24-bit significand, and `add_offset` then subtracts the part that
/// does not fit, so **the surviving difference is a small number `f32` holds
/// perfectly**. Do the subtraction in the raw domain at full width and the
/// raster reads it; narrow the raw domain first and the difference is gone
/// before the subtraction can rescue it, and the raster reads `0`.
#[test]
fn a_wide_precision_source_survives_into_the_raster() {
    // 2^24 + 1, the first integer `f32` cannot represent.
    expect_exact(
        &typed_var_file!(
            with_i32_data,
            &[16_777_217i32, 16_777_216],
            Some(-16_777_216.0)
        ),
        &[1.0, 0.0],
        "an i32 one above f32's last exact integer",
    );

    // 2^53 + 2, the same trap two widths up: exact in `f64`, and in `f32` not
    // merely rounded but 2 away.
    expect_exact(
        &typed_var_file!(
            with_f64_data,
            &[9_007_199_254_740_994.0f64, 9_007_199_254_740_992.0],
            Some(-9_007_199_254_740_992.0)
        ),
        &[2.0, 0.0],
        "an f64 above f32's reach",
    );

    // And a fraction rather than an integer: `0.1` is not representable in
    // either float, but `f64`'s error is 2^29 times smaller, so the difference
    // from `f32`'s nearest value is a number `f32` can state exactly.
    let narrowed = f64::from(0.1f32);
    expect_exact(
        &typed_var_file!(with_f64_data, &[0.1f64], Some(-narrowed)),
        &[0.1 - narrowed],
        "an f64 fraction against its f32 neighbour",
    );
    assert_ne!(
        (0.1 - narrowed) as f32,
        0.0,
        "the fixture is vacuous unless the surviving difference is non-zero in f32"
    );
}

/// An empty variable still knows how wide its storage is.
///
/// `hdf5_pure` errors on storage that was never allocated, so this arm cannot
/// read the width off the data and takes it from the declared type instead —
/// a second place the type table has to be right, and the only one no real
/// granule exercises.
#[test]
fn an_empty_variable_keeps_its_declared_storage_width() {
    let bytes = grid_var_file();
    let file = crate::h5::Granule::open(&bytes).expect("open");
    let past = file
        .raw_var_rows("v", WIN_ROWS as u64 + 5, 2)
        .expect("a window past the end is not an error")
        .expect("variable present");
    assert!(past.raw.is_empty());
    assert!(
        matches!(past.raw, RawValues::F32(_)),
        "the 2-D fixture is `float` on disk, so its empty read must be too",
    );

    let bytes = typed_var_file!(with_u16_data, &[7u16, 8, 9]);
    let file = crate::h5::Granule::open(&bytes).expect("open");
    let past = file
        .raw_var_rows("v", 99, 2)
        .expect("a window past the end is not an error")
        .expect("variable present");
    assert!(past.raw.is_empty());
    assert!(
        matches!(past.raw, RawValues::U16(_)),
        "a `short` variable's empty read must stay `short`",
    );
}

// ── Row-windowed reads ───────────────────────────────────────────────────

/// Rows and columns of the 2-D fixture below.
const WIN_ROWS: usize = 6;
const WIN_COLS: usize = 4;

/// A 2-D `f32` variable whose value encodes its own position: `j * 100 + i`.
///
/// Position-encoded so that a window landing on the wrong rows is a wrong
/// *value*, not merely a wrong length — a length check alone passes for any
/// window of the right size.
fn grid_var_file() -> Vec<u8> {
    let mut values = vec![0f32; WIN_ROWS * WIN_COLS];
    for j in 0..WIN_ROWS {
        for i in 0..WIN_COLS {
            values[j * WIN_COLS + i] = (j * 100 + i) as f32;
        }
    }
    let mut w = hdf5_pure::FileBuilder::new();
    let b = w.create_dataset("v");
    b.with_f32_data(&values)
        .with_shape(&[WIN_ROWS as u64, WIN_COLS as u64]);
    w.finish().expect("write the 2-D fixture")
}

/// A window reads its own rows and only its own rows.
///
/// This is the whole point of the windowed read: extracting one row of a 2-D
/// coordinate variable must not cost the entire variable.
#[test]
fn a_row_window_reads_only_the_rows_it_names() {
    let bytes = grid_var_file();
    let file = crate::h5::Granule::open(&bytes).expect("open");

    let whole = file
        .read_unpacked("v")
        .expect("read")
        .expect("variable present");
    assert_eq!(whole.values.len(), WIN_ROWS * WIN_COLS);

    // Row 0 alone — what a "constant down each column" axis needs.
    let first = file
        .read_unpacked_rows("v", 0, 1)
        .expect("read")
        .expect("variable present");
    assert_eq!(
        first.values,
        (0..WIN_COLS).map(|i| Some(i as f64)).collect::<Vec<_>>(),
        "row 0 must be exactly row 0"
    );

    // A window in the middle, where an off-by-one would still be the right size.
    let middle = file
        .read_unpacked_rows("v", 2, 3)
        .expect("read")
        .expect("variable present");
    let expected: Vec<Option<f64>> = (2..5)
        .flat_map(|j| (0..WIN_COLS).map(move |i| Some((j * 100 + i) as f64)))
        .collect();
    assert_eq!(middle.values, expected, "rows 2..5, in order");

    // And the windowed reads agree with the corresponding slice of the whole.
    assert_eq!(middle.values, whole.values[2 * WIN_COLS..5 * WIN_COLS]);
}

/// A window running past the last row yields the rows that exist.
///
/// Clamping rather than erroring is `hdf5_pure`'s own behaviour, and the count
/// check in `raw_var_span` has to mirror it — otherwise a caller streaming
/// fixed-size windows to the end of a variable gets a spurious "declares N
/// elements but M were read".
#[test]
fn a_row_window_past_the_end_clamps_instead_of_failing() {
    let bytes = grid_var_file();
    let file = crate::h5::Granule::open(&bytes).expect("open");

    // Straddling the end: two rows exist of the four asked for.
    let straddling = file
        .read_unpacked_rows("v", (WIN_ROWS - 2) as u64, 4)
        .expect("a straddling window is not an error")
        .expect("variable present");
    assert_eq!(straddling.values.len(), 2 * WIN_COLS);

    // Entirely past the end: empty, still not an error.
    let past = file
        .read_unpacked_rows("v", WIN_ROWS as u64 + 5, 2)
        .expect("a window past the end is not an error")
        .expect("variable present");
    assert!(past.values.is_empty(), "got {:?}", past.values);
}

/// The two savings compose: a window, in the cheap representation.
#[test]
fn a_row_window_reads_into_the_raster_form_too() {
    let bytes = grid_var_file();
    let file = crate::h5::Granule::open(&bytes).expect("open");

    let row = file
        .read_unpacked_rows_f32("v", 3, 1)
        .expect("read")
        .expect("variable present");
    assert_eq!(
        row.values,
        (0..WIN_COLS).map(|i| (300 + i) as f32).collect::<Vec<_>>()
    );
    assert_eq!(element_bytes(&row.values), 4);
}

/// **The appending read is the owning read, bit for bit**, on both of its arms.
///
/// `read_unpacked_f32_into` decodes a standard 4-byte float straight from the
/// stored bytes and takes the owning read for every other width, and a bit
/// that differed between the two forms on either arm would be a raster reading
/// a different number depending on which buffer it was decoded into. So each
/// arm is driven with every CF rule live — a fill, an offset, a `valid_range`
/// on the packed one — and compared by `to_bits`, since a mapped fill is a
/// `NaN` and `==` would pass wherever the difference actually is.
///
/// The prefix check is the appending half: what was already in the buffer
/// stays, and the variable lands after it.
#[test]
fn the_appending_read_is_the_owning_read_bit_for_bit() {
    // Arm one: `float` storage with a fill and an offset — the fast path.
    let float_bytes = {
        let mut w = hdf5_pure::FileBuilder::new();
        let b = w.create_dataset("v");
        b.with_f32_data(&[1.5f32, -9999.0, -0.0, 0.1, f32::MAX, f32::MIN_POSITIVE]);
        b.set_attr("_FillValue", hdf5_pure::AttrValue::F32(-9999.0));
        b.set_attr("add_offset", hdf5_pure::AttrValue::F64(0.25));
        w.finish().expect("write the float fixture")
    };
    // Arm two: packed unsigned `short` with every rule live — the owning read.
    let short_bytes = short_var_file(&ShortVar {
        values: &[-13585, 100, -1, 0, 11048],
        unsigned: true,
        scale: Some(0.001),
        offset: Some(-90.0),
        fill: Some(-1),
        // `-5` reinterprets to 65531 through `_Unsigned`, so the range is live
        // and the real sample stays inside it.
        valid_range: Some(vec![0, -5]),
        units: Some("degrees_north"),
    });

    for (what, bytes) in [("float", float_bytes), ("packed short", short_bytes)] {
        let file = crate::h5::Granule::open(&bytes).expect("open");
        let owning = file
            .read_unpacked_f32("v")
            .expect("read")
            .expect("variable present");
        assert!(
            owning.values.iter().any(|v| v.is_nan()),
            "premise ({what}): a fill reached the values, so the missing rule is live",
        );

        let prefix = [7.0f32, f32::NAN];
        let mut out: Vec<f32> = prefix.to_vec();
        let appended = file
            .read_unpacked_f32_into("v", &mut out)
            .expect("read")
            .expect("variable present");

        assert_eq!(appended, owning.values.len(), "({what}) the count appended");
        assert_eq!(out.len(), prefix.len() + owning.values.len());
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(
            bits(&out[..prefix.len()]),
            bits(&prefix),
            "({what}) what the buffer already held stays where it was",
        );
        assert_eq!(
            bits(&out[prefix.len()..]),
            bits(&owning.values),
            "({what}) the appended values must be the owning read's, bit for bit",
        );
    }

    // An absent variable appends nothing and says so, on either form.
    let file = crate::h5::Granule::open(&float_var_file(&[1.0], None)).expect("open");
    let mut out = vec![1.0f32];
    assert_eq!(
        file.read_unpacked_f32_into("absent", &mut out)
            .expect("read"),
        None
    );
    assert_eq!(out, vec![1.0]);
}

/// Rows and columns of the one-chunk fixture below: 600 x 600 `f32` is
/// 1,440,000 B, **over** `hdf5_pure`'s 1 MiB default chunk-cache budget, so a
/// default handle cannot retain it and a sized one can. That gap is the whole
/// reason [`crate::h5::Granule::variable`] exists.
const ONE_CHUNK_ROWS: usize = 600;
const ONE_CHUNK_COLS: usize = 600;
const ONE_CHUNK_WINDOW: usize = 75;

/// A 2-D `f32` variable stored as **one** shuffled, deflated chunk — the
/// shape GMGSI's `lat`/`lon` are stored in — with position-encoded values.
fn one_chunk_var_file() -> Vec<u8> {
    let mut values = vec![0f32; ONE_CHUNK_ROWS * ONE_CHUNK_COLS];
    for j in 0..ONE_CHUNK_ROWS {
        for i in 0..ONE_CHUNK_COLS {
            values[j * ONE_CHUNK_COLS + i] = (j * 1000 + i) as f32;
        }
    }
    let mut w = hdf5_pure::FileBuilder::new();
    let b = w.create_dataset("v");
    b.with_f32_data(&values)
        .with_shape(&[ONE_CHUNK_ROWS as u64, ONE_CHUNK_COLS as u64])
        .with_chunks(&[ONE_CHUNK_ROWS as u64, ONE_CHUNK_COLS as u64])
        .with_shuffle()
        .with_deflate(5);
    w.finish().expect("write the one-chunk fixture")
}

/// **A [`crate::h5::Variable`] serves every row window off one inflation of
/// its chunk**, and reads the same bits a fresh handle per window would.
///
/// The control is the same walk over a plain `hdf5_pure` handle with the
/// default cache: every window is an oversize rejection and a miss, which is
/// what every windowed read in this crate did before the handle existed. The
/// two are asserted side by side so the sized cache is shown to be the
/// difference rather than assumed to be.
#[test]
fn a_variable_handle_serves_its_windows_off_one_inflation() {
    let bytes = one_chunk_var_file();
    let file = crate::h5::Granule::open(&bytes).expect("open");
    let windows = ONE_CHUNK_ROWS / ONE_CHUNK_WINDOW;
    assert!(
        windows > 1,
        "premise: more than one window, or nothing is shared"
    );

    let var = file.variable("v").expect("open").expect("present");
    assert_eq!(var.shape(), [ONE_CHUNK_ROWS as u64, ONE_CHUNK_COLS as u64]);
    for w in 0..windows {
        let start = (w * ONE_CHUNK_WINDOW) as u64;
        let through_handle = var
            .read_unpacked_rows_f32(start, ONE_CHUNK_WINDOW as u64)
            .expect("read");
        let fresh = file
            .read_unpacked_rows_f32("v", start, ONE_CHUNK_WINDOW as u64)
            .expect("read")
            .expect("present");
        assert_eq!(
            through_handle
                .values
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>(),
            fresh.values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "window {w} through the held handle must be the window, bit for bit",
        );
        assert_eq!(
            through_handle.values[0],
            (w * ONE_CHUNK_WINDOW * 1000) as f32,
            "and it must be the window that was asked for",
        );
    }
    let stats = var.chunk_cache_stats();
    assert_eq!(
        (
            stats.misses(),
            stats.hits(),
            stats.oversize_chunks(),
            stats.cached_chunks()
        ),
        (1, windows as u64 - 1, 0, 1),
        "one inflation, every later window a hit, the chunk admitted and held",
    );

    // Control: the default handle, and the fixture is over its budget by
    // construction. Every window pays the pipeline again.
    let plain = hdf5_pure::File::from_bytes(bytes.clone()).expect("open");
    let ds = plain.dataset("v").expect("present");
    for w in 0..windows {
        ds.read_f32_rows((w * ONE_CHUNK_WINDOW) as u64, ONE_CHUNK_WINDOW as u64)
            .expect("read");
    }
    let control = ds.chunk_cache_stats();
    assert_eq!(
        (control.hits(), control.cached_chunks()),
        (0, 0),
        "premise: a default handle never retains this chunk, so the hits above \
         are the sized cache's doing and nothing else's",
    );
    assert!(
        ONE_CHUNK_ROWS * ONE_CHUNK_COLS * size_of::<f32>()
            > hdf5_pure::ChunkCacheConfig::new().max_bytes(),
        "premise: the chunk is over the default budget, or the control proves nothing",
    );
}

/// A 2-D chunked `f32` variable, **unfiltered**, so its stored bytes are its
/// values' little-endian bytes and the fingerprint can be checked against
/// the file rather than against itself.
fn chunked_plain_var_file(values: &[f32]) -> Vec<u8> {
    let mut w = hdf5_pure::FileBuilder::new();
    let b = w.create_dataset("v");
    b.with_f32_data(values)
        .with_shape(&[4, 4])
        .with_chunks(&[2, 4]);
    b.set_attr(
        "units",
        hdf5_pure::AttrValue::String("degrees_north".into()),
    );
    w.finish().expect("write the chunked fixture")
}

/// **A stored fingerprint is the stored bytes** — equal for two files that
/// store the same variable, different the moment one value differs, and
/// absent for a variable that is not chunked.
///
/// The address arithmetic is the part that could be quietly wrong — a
/// fingerprint over the wrong bytes is still self-consistent — so the
/// chunks' bytes are compared against the values written, through the
/// unfiltered fixture where stored bytes are values.
#[test]
fn a_stored_fingerprint_is_the_stored_bytes_and_moves_with_them() {
    let values: Vec<f32> = (0..16).map(|k| k as f32 * 1.5).collect();
    let one = crate::h5::Granule::open(&chunked_plain_var_file(&values)).expect("open");
    let two = crate::h5::Granule::open(&chunked_plain_var_file(&values)).expect("open");
    let fp_one = one
        .stored_fingerprint("v")
        .expect("read")
        .expect("chunked, so fingerprintable");
    let fp_two = two
        .stored_fingerprint("v")
        .expect("read")
        .expect("likewise");
    assert_eq!(
        fp_one, fp_two,
        "the same variable stored twice is one fingerprint"
    );

    let stored: Vec<u8> = fp_one.chunk_bytes().flatten().copied().collect();
    let written: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(
        stored, written,
        "the fingerprint's chunk bytes must be the file's stored bytes for \
         this variable: an unfiltered chunk stores its values verbatim",
    );
    assert_eq!(fp_one.stored_bytes(), 16 * size_of::<f32>());

    let mut moved = values.clone();
    moved[9] += 1.0;
    let other = crate::h5::Granule::open(&chunked_plain_var_file(&moved)).expect("open");
    assert_ne!(
        other
            .stored_fingerprint("v")
            .expect("read")
            .expect("chunked"),
        fp_one,
        "one changed value is a different fingerprint",
    );

    // Not chunked: nothing to fingerprint, and it says so rather than
    // inventing a key that would match every contiguous variable of a shape.
    let contiguous = crate::h5::Granule::open(&float_var_file(&values, None)).expect("open");
    assert_eq!(contiguous.stored_fingerprint("v").expect("read"), None);
    assert_eq!(contiguous.stored_fingerprint("absent").expect("read"), None);
}
