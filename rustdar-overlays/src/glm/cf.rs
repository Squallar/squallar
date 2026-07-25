//! CF-convention unpacking for NetCDF variables.
//!
//! # Why this module exists
//!
//! The `netcdf` crate is a thin wrapper over the netCDF-C library and it
//! **does not apply CF packing attributes**. `Variable::get_values::<f32>()`
//! performs a *numeric type conversion* of the bytes on disk and nothing more
//! — see the crate's own note on [`netcdf::Variable`]: "`scale_factor` and
//! `offset_factor` and other attributes are not [considered]".
//!
//! The GOES GLM L2 LCFA product stores nearly every quantity we care about as
//! a *packed* 16-bit integer. Reading it without unpacking yields numbers that
//! are not merely imprecise, they are meaningless. `event_lat` reads back as
//! `-13585.0`; the real latitude is `38.967°N`. Every event-level strike was
//! being plotted at a garbage coordinate.
//!
//! # The unpacking rules (this module is the specification)
//!
//! This crate is slated to be replaced by a pure-Rust HDF5 reader for the wasm
//! build. That reader **must** implement the following, in this order, or it
//! will silently reintroduce the same class of bug:
//!
//! 1. **Read the raw storage values in the variable's declared type.** Do not
//!    let the library widen a `short` to `float` for you before you have
//!    looked at `_Unsigned` — once it is a float the sign bit is baked in.
//!
//! 2. **Apply `_Unsigned`.** NetCDF-3 has no unsigned types, so the convention
//!    (inherited by NetCDF-4 files written through the classic model, which is
//!    what NOAA ships) is to declare the variable `short` and attach
//!    `_Unsigned = "true"`. The stored bits are then a `u16`, and a raw value
//!    that reads as a negative `i16` must be *bit-reinterpreted*, not negated:
//!    `-13585_i16 as u16 == 51951`. This is the subtle step. On disk the HDF5
//!    datatype really is `H5T_STD_I16LE`, so a reader that trusts the datatype
//!    alone gets it wrong.
//!
//!    `_Unsigned` is spelled as the *string* `"true"` in GLM files, but the
//!    convention also permits a numeric `1`, so both are accepted.
//!
//! 3. **Apply `_FillValue` and `valid_range` in the raw (post-`_Unsigned`,
//!    pre-scale) domain.** These attributes are stored in the variable's
//!    declared type, so they carry the same signed/unsigned trap: GLM writes
//!    `_FillValue = -1s` and `valid_range = 0s, -6s`, which under `_Unsigned`
//!    mean `65535` and `0..=65530`. Comparing against `-1` would never match.
//!    A value that is fill or out of range is **missing** — it must not be
//!    scaled and published as a number.
//!
//! 4. **Only then apply `raw * scale_factor + add_offset`.** Either attribute
//!    may be absent; a missing `scale_factor` means 1 and a missing
//!    `add_offset` means 0. Both are commonly stored as `float` even on a
//!    `short` variable, so the arithmetic is done in `f64` to avoid
//!    accumulating error.
//!
//! 5. **Variables genuinely stored as `float`/`double` with no packing
//!    attributes pass through untouched.** In GLM, `group_lat`/`group_lon` and
//!    `flash_lat`/`flash_lon` are real `float` degrees — which is exactly why
//!    the group and flash levels looked plausible on screen and masked the
//!    fact that the event level was broken.
//!
//! 6. **Never hardcode the constants.** `scale_factor`, `add_offset` and the
//!    time epoch legitimately differ between product versions and between
//!    spacecraft. `event_lon:add_offset` tracks the satellite sub-point, so it
//!    is `-141.56` for the GOES-East slot (both G16 and G19 — it follows the
//!    orbital position, not the spacecraft) but `-203.56` for GOES-West (G18).
//!    That is a 62° difference, and it moves the *valid interval* too: East
//!    unpacks to -141.56…-8.44 while West unpacks to -203.56…-70.44, running
//!    past the antimeridian. Read the constants from the file, every time, and
//!    see [`super::fetch::normalize_longitude`] for what the West range means
//!    for consumers.

use netcdf::AttributeValue;
use netcdf::types::{IntType, NcVariableType};

/// A variable read out of a NetCDF file with CF packing applied.
pub(crate) struct UnpackedVar {
    /// One entry per element. `None` means the file marked the element
    /// missing, via `_FillValue` or `valid_range`.
    pub values: Vec<Option<f64>>,
    /// The variable's `units` attribute, verbatim. Callers must consult this
    /// rather than assuming a unit — GLM areas are `m2`, not `km²`.
    pub units: Option<String>,
}

/// Read a 1-D variable and apply CF packing conventions.
///
/// Returns `Ok(None)` when the variable is absent from the file. That is a
/// different condition from "present but all-missing" and callers must not
/// conflate them: the L2 LCFA product has no `event_area` variable at all,
/// which is a property of the product, not of the granule.
pub(crate) fn read_unpacked(
    file: &netcdf::File,
    name: &str,
) -> Result<Option<UnpackedVar>, String> {
    let Some(var) = file.variable(name) else {
        return Ok(None);
    };

    // Step 2: does this variable use the signed-storage/unsigned-meaning
    // convention? Decided before any values are read, because the answer
    // changes how they must be read.
    let unsigned = attr(&var, "_Unsigned").is_some_and(|v| attr_is_true(&v));
    let vartype = var.vartype();

    // Step 1 + 2: raw storage values, bit-reinterpreted through `_Unsigned`.
    let raw = read_raw(&var, name, vartype.clone(), unsigned)?;

    // Step 3: missing-value markers, normalized into the same raw domain.
    let fill = attr(&var, "_FillValue")
        .and_then(|v| attr_as_f64(&v))
        .map(|v| reinterpret_unsigned(v, &vartype, unsigned));
    // CF also defines `valid_min`/`valid_max` as an alternative spelling. GLM
    // uses neither, so they are deliberately unsupported rather than
    // speculatively implemented; a reader porting this must add them if the
    // product it targets uses them.
    let valid_range = attr(&var, "valid_range").and_then(|v| {
        let bounds = attr_as_f64_vec(&v);
        if bounds.len() != 2 {
            // Ignoring a malformed range is the safe direction — it only means
            // fewer values are marked missing — but it is never expected, so
            // say so rather than dropping it silently.
            log::warn!(
                "GLM {name}: valid_range has {} element(s), expected 2; ignoring it",
                bounds.len()
            );
            return None;
        }
        let lo = reinterpret_unsigned(bounds[0], &vartype, unsigned);
        let hi = reinterpret_unsigned(bounds[1], &vartype, unsigned);
        if lo > hi {
            // An inverted range would mark *every* value missing, silently
            // emptying the variable. Refuse it instead.
            log::warn!(
                "GLM {name}: valid_range is inverted ({lo}..{hi}) after _Unsigned \
                 reinterpretation; ignoring it rather than discarding every value"
            );
            return None;
        }
        Some((lo, hi))
    });

    // Step 4: packing coefficients. Absent means identity, never zero.
    let scale = attr(&var, "scale_factor").and_then(|v| attr_as_f64(&v));
    let offset = attr(&var, "add_offset").and_then(|v| attr_as_f64(&v));

    let units = attr(&var, "units").and_then(|v| match v {
        AttributeValue::Str(s) => Some(s),
        _ => None,
    });

    let values = raw
        .into_iter()
        .map(|r| {
            if fill.is_some_and(|f| r == f) {
                return None;
            }
            if valid_range.is_some_and(|(lo, hi)| r < lo || r > hi) {
                return None;
            }
            let mut v = r;
            if let Some(s) = scale {
                v *= s;
            }
            if let Some(o) = offset {
                v += o;
            }
            // A packed integer can still unpack to a non-finite value if the
            // file's coefficients are nonsense; treat that as missing too.
            v.is_finite().then_some(v)
        })
        .collect();

    Ok(Some(UnpackedVar { values, units }))
}

/// Read a variable's values into the raw (pre-scale) domain as `f64`,
/// reinterpreting the bits through `_Unsigned` where required.
///
/// `f64` holds every `u32`/`i32` and every 16-bit value exactly, so nothing is
/// lost for the integer types CF packing actually uses.
fn read_raw(
    var: &netcdf::Variable,
    name: &str,
    vartype: NcVariableType,
    unsigned: bool,
) -> Result<Vec<f64>, String> {
    let err = |e: netcdf::Error| format!("Failed to read {name}: {e}");

    // Only *signed* integer storage needs the reinterpretation. Natively
    // unsigned storage (NC_USHORT and friends) and floats are already correct,
    // and netCDF-C widens them to f64 without loss of meaning.
    if unsigned && let NcVariableType::Int(int_type) = vartype {
        return Ok(match int_type {
            IntType::I8 => var
                .get_values::<i8, _>(..)
                .map_err(err)?
                .into_iter()
                .map(|v| f64::from(v as u8))
                .collect(),
            IntType::I16 => var
                .get_values::<i16, _>(..)
                .map_err(err)?
                .into_iter()
                .map(|v| f64::from(v as u16))
                .collect(),
            IntType::I32 => var
                .get_values::<i32, _>(..)
                .map_err(err)?
                .into_iter()
                .map(|v| f64::from(v as u32))
                .collect(),
            IntType::I64 => var
                .get_values::<i64, _>(..)
                .map_err(err)?
                .into_iter()
                .map(|v| v as u64 as f64)
                .collect(),
            // Already unsigned on disk: nothing to reinterpret.
            _ => var.get_values::<f64, _>(..).map_err(err)?,
        });
    }

    var.get_values::<f64, _>(..).map_err(err)
}

/// Bit-reinterpret a raw value that netCDF reported through its *signed*
/// declared type but which the file means as unsigned.
///
/// This is the step that fixes `-13585 → 51951`. Note it is a bit
/// reinterpretation, not `abs()` and not a negation.
fn reinterpret_unsigned(raw: f64, vartype: &NcVariableType, unsigned: bool) -> f64 {
    if !unsigned {
        return raw;
    }
    let NcVariableType::Int(int_type) = vartype else {
        return raw;
    };
    let bits = raw as i64;
    match int_type {
        IntType::I8 => f64::from(bits as i8 as u8),
        IntType::I16 => f64::from(bits as i16 as u16),
        IntType::I32 => f64::from(bits as i32 as u32),
        IntType::I64 => bits as u64 as f64,
        // Already unsigned on disk.
        _ => raw,
    }
}

fn attr(var: &netcdf::Variable, name: &str) -> Option<AttributeValue> {
    var.attribute(name)?.value().ok()
}

/// `_Unsigned` is spelled `"true"` in GLM files. The convention also allows a
/// numeric `1`, and some producers write `"1"`, so accept all of them.
fn attr_is_true(v: &AttributeValue) -> bool {
    match v {
        AttributeValue::Str(s) => {
            let s = s.trim();
            s.eq_ignore_ascii_case("true") || s == "1"
        }
        other => attr_as_f64(other).is_some_and(|n| n != 0.0),
    }
}

/// Coerce a scalar numeric attribute to `f64`.
///
/// NetCDF4 writers frequently store a logically-scalar attribute as a
/// length-1 vector, so the vector variants are accepted as scalars too.
fn attr_as_f64(v: &AttributeValue) -> Option<f64> {
    attr_as_f64_vec(v).first().copied()
}

fn attr_as_f64_vec(v: &AttributeValue) -> Vec<f64> {
    use AttributeValue as A;
    match v {
        A::Uchar(x) => vec![f64::from(*x)],
        A::Schar(x) => vec![f64::from(*x)],
        A::Ushort(x) => vec![f64::from(*x)],
        A::Short(x) => vec![f64::from(*x)],
        A::Uint(x) => vec![f64::from(*x)],
        A::Int(x) => vec![f64::from(*x)],
        A::Ulonglong(x) => vec![*x as f64],
        A::Longlong(x) => vec![*x as f64],
        A::Float(x) => vec![f64::from(*x)],
        A::Double(x) => vec![*x],
        A::Uchars(x) => x.iter().copied().map(f64::from).collect(),
        A::Schars(x) => x.iter().copied().map(f64::from).collect(),
        A::Ushorts(x) => x.iter().copied().map(f64::from).collect(),
        A::Shorts(x) => x.iter().copied().map(f64::from).collect(),
        A::Uints(x) => x.iter().copied().map(f64::from).collect(),
        A::Ints(x) => x.iter().copied().map(f64::from).collect(),
        A::Ulonglongs(x) => x.iter().map(|&v| v as f64).collect(),
        A::Longlongs(x) => x.iter().map(|&v| v as f64).collect(),
        A::Floats(x) => x.iter().copied().map(f64::from).collect(),
        A::Doubles(x) => x.to_vec(),
        A::Str(_) | A::Strs(_) => Vec::new(),
    }
}

/// A CF time axis: `"<unit> since <epoch>"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TimeUnits {
    /// How many seconds one unpacked unit represents.
    pub seconds_per_unit: f64,
    /// The reference instant the offsets are measured from.
    pub epoch: chrono::NaiveDateTime,
}

/// Parse a CF time `units` string such as
/// `"seconds since 2026-07-24 12:00:00.000"`.
///
/// GLM's `*_time_offset` variables carry exactly this, and the epoch they
/// name is the granule's `time_coverage_start`. Reading the epoch from the
/// variable rather than assuming it keeps the two from silently diverging —
/// and the unit multiplier means a future switch to `milliseconds since`
/// cannot quietly shift every strike by three orders of magnitude.
pub(crate) fn parse_time_units(units: &str) -> Option<TimeUnits> {
    let (unit, epoch) = units.split_once(" since ")?;

    let seconds_per_unit = match unit.trim().to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
        "ms" | "msec" | "msecs" | "millisecond" | "milliseconds" => 1e-3,
        "us" | "usec" | "usecs" | "microsecond" | "microseconds" => 1e-6,
        "min" | "mins" | "minute" | "minutes" => 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
        "d" | "day" | "days" => 86400.0,
        _ => return None,
    };

    Some(TimeUnits {
        seconds_per_unit,
        epoch: parse_cf_epoch(epoch)?,
    })
}

/// Parse the datetime half of a CF `units` string, or a `time_coverage_start`
/// global attribute — they use the same set of shapes.
pub(crate) fn parse_cf_epoch(s: &str) -> Option<chrono::NaiveDateTime> {
    // Trim a trailing UTC designator; CF times without an offset are UTC and
    // GLM writes both `...12:00:00.000` and `...12:00:00.0Z`.
    let s = s.trim().trim_end_matches('Z').trim_end_matches(" UTC").trim();

    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt);
        }
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single most important line in this module: a packed `u16` above
    /// 32767 reads back from netCDF as a *negative* `i16`, and turning it into
    /// the right unsigned number is a bit reinterpretation.
    ///
    /// The values here are the first real `event_lat` sample from
    /// `noaa-goes19` granule `OR_GLM-L2-LCFA_G19_s20262051200000_...`.
    #[test]
    fn unsigned_short_above_32767_reinterprets_not_negates() {
        let ty = NcVariableType::Int(IntType::I16);
        assert_eq!(reinterpret_unsigned(-13585.0, &ty, true), 51951.0);
        // Without `_Unsigned` the same bits stay negative.
        assert_eq!(reinterpret_unsigned(-13585.0, &ty, false), -13585.0);
        // Values below 32768 are unaffected either way.
        assert_eq!(reinterpret_unsigned(11048.0, &ty, true), 11048.0);

        // ...and unpacking it must land on a real latitude in Ohio, not on
        // whatever `-13585 * scale + offset` would produce.
        let lat: f64 = 51951.0 * 0.00203128 + -66.56;
        assert!((lat - 38.967).abs() < 1e-3, "got {lat}");
        let wrong: f64 = -13585.0 * 0.00203128 + -66.56;
        assert!(wrong < -90.0, "the unfixed path is not even a valid latitude");
    }

    /// `_FillValue = -1s` under `_Unsigned` means 65535, and `valid_range =
    /// 0s, -6s` means `0..=65530`. Comparing in the signed domain would never
    /// match, so the fill would be scaled and published as a real number.
    #[test]
    fn fill_and_valid_range_reinterpret_through_unsigned() {
        let ty = NcVariableType::Int(IntType::I16);
        assert_eq!(reinterpret_unsigned(-1.0, &ty, true), 65535.0);
        assert_eq!(reinterpret_unsigned(-6.0, &ty, true), 65530.0);
        assert_eq!(reinterpret_unsigned(0.0, &ty, true), 0.0);
    }

    #[test]
    fn unsigned_reinterpretation_covers_the_other_int_widths() {
        assert_eq!(
            reinterpret_unsigned(-1.0, &NcVariableType::Int(IntType::I8), true),
            255.0
        );
        assert_eq!(
            reinterpret_unsigned(-1.0, &NcVariableType::Int(IntType::I32), true),
            4_294_967_295.0
        );
        // Native unsigned storage is already correct and must be left alone.
        assert_eq!(
            reinterpret_unsigned(65535.0, &NcVariableType::Int(IntType::U16), true),
            65535.0
        );
        // Floats are never reinterpreted.
        assert_eq!(
            reinterpret_unsigned(
                -13585.0,
                &NcVariableType::Float(netcdf::types::FloatType::F32),
                true
            ),
            -13585.0
        );
    }

    #[test]
    fn unsigned_attribute_accepts_string_and_numeric_spellings() {
        assert!(attr_is_true(&AttributeValue::Str("true".into())));
        assert!(attr_is_true(&AttributeValue::Str("True".into())));
        assert!(attr_is_true(&AttributeValue::Str("1".into())));
        assert!(attr_is_true(&AttributeValue::Uchar(1)));
        assert!(!attr_is_true(&AttributeValue::Str("false".into())));
        assert!(!attr_is_true(&AttributeValue::Uchar(0)));
    }

    #[test]
    fn scalar_attributes_written_as_length_one_vectors_still_read() {
        assert_eq!(attr_as_f64(&AttributeValue::Float(0.5)), Some(0.5));
        assert_eq!(attr_as_f64(&AttributeValue::Floats(vec![0.5])), Some(0.5));
        assert_eq!(attr_as_f64(&AttributeValue::Short(-1)), Some(-1.0));
        assert_eq!(attr_as_f64(&AttributeValue::Str("nope".into())), None);
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
    // File-backed tests.
    //
    // These build real NetCDF4 files through the same library that reads the
    // GOES granules, so they exercise the actual read path rather than a
    // reimplementation of it. A unit test on `reinterpret_unsigned` alone
    // could not catch the case that mattered: netCDF-C happily widening a
    // packed `short` to `f32` before anyone looked at `_Unsigned`.
    // ---------------------------------------------------------------------

    /// Description of a packed 16-bit variable, mirroring how GLM writes one.
    struct ShortVar<'a> {
        values: &'a [i16],
        unsigned: bool,
        scale: Option<f32>,
        offset: Option<f32>,
        fill: Option<i16>,
        /// Written verbatim, element count and all. A `Vec` rather than a pair
        /// because the *shape* of this attribute is itself under test: a
        /// `valid_range` that is not exactly two elements is malformed and must
        /// be ignored, and a pair could not express one.
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

    /// A unique scratch path, so tests can run in parallel.
    fn scratch_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "rustdar-glm-{tag}-{}-{:?}.nc",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    /// Write a single packed variable to a NetCDF4 file and hand back its bytes.
    fn short_var_file(spec: &ShortVar<'_>) -> Vec<u8> {
        let path = scratch_path("short");
        let _ = std::fs::remove_file(&path);
        {
            let mut file = netcdf::create(&path).expect("create netcdf");
            file.add_dimension("n", spec.values.len()).expect("dim");
            let mut var = file.add_variable::<i16>("v", &["n"]).expect("add var");
            // Attributes first: netCDF-C wants `_FillValue` set before data.
            if spec.unsigned {
                var.put_attribute("_Unsigned", "true").expect("_Unsigned");
            }
            if let Some(f) = spec.fill {
                var.put_attribute("_FillValue", f).expect("_FillValue");
            }
            if let Some(range) = &spec.valid_range {
                var.put_attribute("valid_range", range.clone()).expect("valid_range");
            }
            if let Some(s) = spec.scale {
                var.put_attribute("scale_factor", s).expect("scale_factor");
            }
            if let Some(o) = spec.offset {
                var.put_attribute("add_offset", o).expect("add_offset");
            }
            if let Some(u) = spec.units {
                var.put_attribute("units", u).expect("units");
            }
            var.put_values(spec.values, ..).expect("put values");
        }
        let bytes = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);
        bytes
    }

    /// Write a single unpacked `float` variable — GLM's `group_lat`/`flash_lat`
    /// shape — and hand back its bytes.
    fn float_var_file(values: &[f32], units: Option<&str>) -> Vec<u8> {
        let path = scratch_path("float");
        let _ = std::fs::remove_file(&path);
        {
            let mut file = netcdf::create(&path).expect("create");
            file.add_dimension("n", values.len()).expect("dim");
            let mut var = file.add_variable::<f32>("v", &["n"]).expect("add var");
            if let Some(u) = units {
                var.put_attribute("units", u).expect("units");
            }
            var.put_values(values, ..).expect("put");
        }
        let bytes = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);
        bytes
    }

    fn read_v(bytes: &[u8]) -> UnpackedVar {
        let file = netcdf::open_mem(None, bytes).expect("open_mem");
        read_unpacked(&file, "v").expect("read").expect("variable present")
    }

    /// The headline regression: a `u16` above 32767 arrives from netCDF-C as a
    /// negative `i16`, and must come out of the unpacker as the right
    /// latitude. `51951` is stored as `-13585`; the real granule's first
    /// `event_lat` is exactly this.
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

        // And the number the unfixed code produced is not a latitude at all.
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

    /// `_FillValue = -1s` under `_Unsigned` means 65535. Comparing in the
    /// signed domain would miss it and publish `65535 * scale + offset` as if
    /// it were a measurement.
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
    /// `group_lat`/`flash_lat` — must come through bit-for-bit. This is the
    /// half of the product that looked fine and hid the bug.
    #[test]
    fn float_variable_passes_through_unchanged() {
        let bytes =
            float_var_file(&[39.033424_f32, -22.65055, 55.2922], Some("degrees_north"));

        let v = read_v(&bytes);
        assert_eq!(v.values[0], Some(f64::from(39.033424_f32)));
        assert_eq!(v.values[1], Some(f64::from(-22.65055_f32)));
        assert_eq!(v.values[2], Some(f64::from(55.2922_f32)));
    }

    /// An inverted `valid_range` is refused, not honoured.
    ///
    /// The trap is that "inverted" is only decidable *after* `_Unsigned`
    /// reinterpretation, and the two orderings look the same in the signed
    /// domain the file is written in. GLM's real range is `0s, -6s`, which is
    /// `0..=65530` — fine. Transpose it to `-6s, 0s` and it becomes
    /// `65530..=0`, which matches nothing: every value in the variable would be
    /// marked missing and the layer would go dark with no error anywhere.
    ///
    /// A future reader that checks `lo > hi` in the *signed* domain gets this
    /// exactly backwards — it would accept the inverted range and reject the
    /// real one.
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
        // and they must still be enforced — refusing inverted ranges is not
        // licence to stop checking ranges.
        let correct = read_v(&short_var_file(&ShortVar {
            values: &[100, 200, -3], // -3 == 65533, past the 65530 cap
            unsigned: true,
            valid_range: Some(vec![0, -6]),
            scale: Some(1.0),
            ..Default::default()
        }));
        assert_eq!(correct.values, vec![Some(100.0), Some(200.0), None]);

        // Inversion is refused on a plainly signed variable too — nothing about
        // the check is specific to `_Unsigned`, it is just where it bites.
        let signed = read_v(&short_var_file(&ShortVar {
            values: &[-20, 0, 20],
            unsigned: false,
            valid_range: Some(vec![10, 5]),
            scale: Some(1.0),
            ..Default::default()
        }));
        assert_eq!(signed.values, vec![Some(-20.0), Some(0.0), Some(20.0)]);
    }

    /// `valid_range` is defined as exactly two elements. Anything else is
    /// malformed and is ignored wholesale.
    ///
    /// Ignoring is the safe direction — it can only mark *fewer* values missing,
    /// never invent an empty variable — and it is the direction a reader has to
    /// choose deliberately. Quietly taking the first two elements of a longer
    /// attribute would apply a range the file never declared; reaching for
    /// `bounds[1]` of a shorter one is an outright panic on a granule the user
    /// cannot control.
    #[test]
    fn a_valid_range_that_is_not_two_elements_is_ignored() {
        // Three elements. The first two happen to be the real GLM range, so
        // honouring them would drop 65533 and look entirely plausible.
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

    /// A value that unpacks to NaN or ±inf is missing, not a measurement.
    ///
    /// Two ways in, and both are real. A packed variable can carry coefficients
    /// that overflow the arithmetic — the values here are deliberately absurd,
    /// but nothing in the format prevents them and `raw * scale + offset` has no
    /// other guard. And a genuine `float` variable (GLM's `flash_lat`) can hold
    /// NaN on disk directly, where CF's fill machinery never sees it because no
    /// `_FillValue` was declared.
    ///
    /// Publishing either is worse than dropping it: `rasterize` sizes bolts by
    /// `energy.log10()` and positions them by lat/lon, so a NaN propagates into
    /// the projection instead of announcing itself.
    #[test]
    fn non_finite_unpacked_values_are_missing_not_published() {
        // 100 * inf == inf, and 0 * inf == NaN — both non-finite, by different
        // routes through the same expression.
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
}
