//! CF-convention unpacking for GLM variables.
//!
//! The GOES GLM L2 LCFA product stores nearly every quantity as a *packed*
//! 16-bit integer, and neither an HDF5 reader nor netCDF-C applies the CF
//! attributes that say what those bytes mean. Unpacked, `event_lat` reads back
//! as `-13585.0` where the real latitude is `38.967°N`.
//!
//! # The unpacking rules (this module is the specification)
//!
//! Order matters. [`super::h5`] does steps 1 and 2 — only the reader can ask
//! its library for a specific width — and [`unpack`] does the rest:
//!
//! 1. **Read the raw storage values in the variable's declared type.** Once a
//!    `short` has been widened to `float` the sign bit is baked in.
//!
//! 2. **Apply `_Unsigned`.** NetCDF-3 has no unsigned types, so NOAA declares
//!    the variable `short` and attaches `_Unsigned = "true"`. A raw value that
//!    reads as a negative `i16` must be *bit-reinterpreted*, not negated:
//!    `-13585_i16 as u16 == 51951`. On disk the HDF5 datatype really is
//!    `H5T_STD_I16LE` — the attribute, not the datatype, carries the meaning.
//!
//!    GLM spells it as the *string* `"true"`; the convention also permits a
//!    numeric `1`, so both are accepted.
//!
//! 3. **Apply `_FillValue` and `valid_range` in the raw (post-`_Unsigned`,
//!    pre-scale) domain.** They are stored in the declared type and carry the
//!    same trap: GLM writes `_FillValue = -1s` and `valid_range = 0s, -6s`,
//!    which under `_Unsigned` mean `65535` and `0..=65530`. Fill or
//!    out-of-range values are **missing** and must not be scaled and published.
//!
//! 4. **Only then apply `raw * scale_factor + add_offset`.** A missing
//!    `scale_factor` means 1 and a missing `add_offset` means 0. Both are
//!    commonly stored as `float` even on a `short` variable; the arithmetic is
//!    done in `f64`.
//!
//! 5. **Variables genuinely stored as `float`/`double` with no packing
//!    attributes pass through untouched.** `group_lat`/`group_lon` and
//!    `flash_lat`/`flash_lon` are real `float` degrees, which is why those
//!    levels looked plausible while the event level was garbage.
//!
//! 6. **Never hardcode the constants.** `event_lon:add_offset` tracks the
//!    satellite sub-point: `-141.56` for the GOES-East slot (G16 and G19 alike
//!    — it follows the orbital position) and `-203.56` for GOES-West (G18).
//!    That moves the valid interval too — East unpacks to -141.56…-8.44, West
//!    to -203.56…-70.44, past the antimeridian. See
//!    [`super::fetch::normalize_longitude`].

use std::collections::BTreeMap;

/// Backend-neutral storage type of a variable.
///
/// Only signed integers (and their width) matter: they are the sole case
/// `_Unsigned` reinterprets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VarType {
    /// Signed integer storage, width in bytes (1, 2, 4 or 8).
    SignedInt(u8),
    /// Natively unsigned integer storage — never reinterpreted.
    UnsignedInt,
    /// IEEE float storage — never reinterpreted.
    Float,
}

/// Backend-neutral attribute value.
///
/// Numeric attributes are widened to `f64` on the way in, scalar and vector
/// alike: CF only ever compares them numerically.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CfAttr {
    /// A numeric attribute, scalar or vector.
    Nums(Vec<f64>),
    /// A text attribute.
    Str(String),
}

/// A variable's raw storage values plus the metadata CF unpacking needs.
///
/// The seam between "get the bytes off disk" (the backend does steps 1 and 2)
/// and "decide what they mean" ([`unpack`] does the rest).
pub(crate) struct RawVar {
    /// Raw stored values, already bit-reinterpreted through `_Unsigned`.
    pub raw: Vec<f64>,
    /// The declared storage type, needed to reinterpret `_FillValue` and
    /// `valid_range` into the same domain as `raw`.
    pub vartype: VarType,
    /// Whether `_Unsigned` applied. Carried separately because `raw` has
    /// already been reinterpreted but the *attributes* have not.
    pub unsigned: bool,
    pub attrs: BTreeMap<String, CfAttr>,
}

/// A variable read out of a GLM file with CF packing applied.
pub(crate) struct UnpackedVar {
    /// One entry per element. `None` means the file marked the element
    /// missing, via `_FillValue` or `valid_range`.
    pub values: Vec<Option<f64>>,
    /// The variable's `units` attribute, verbatim. Callers must consult this
    /// rather than assuming a unit — GLM areas are `m2`, not `km²`.
    pub units: Option<String>,
}

/// Apply CF packing conventions to a variable's raw storage values.
///
/// Steps 3 to 5 of the rules at the top of this module; the backend has
/// already done steps 1 and 2 and handed over a [`RawVar`].
pub(crate) fn unpack(var: &RawVar, name: &str) -> UnpackedVar {
    let RawVar {
        raw,
        vartype,
        unsigned,
        attrs,
    } = var;
    let (vartype, unsigned) = (*vartype, *unsigned);
    let attr = |n: &str| attrs.get(n).cloned();

    // Step 3: missing-value markers, normalized into the same raw domain.
    let fill = attr("_FillValue")
        .and_then(|v| attr_as_f64(&v))
        .map(|v| reinterpret_unsigned(v, vartype, unsigned));
    // CF's alternative `valid_min`/`valid_max` spelling is unsupported: GLM
    // uses neither.
    let valid_range = attr("valid_range").and_then(|v| {
        let bounds = attr_as_f64_vec(&v);
        if bounds.len() != 2 {
            // Ignoring a malformed range is the safe direction: it can only
            // mark *fewer* values missing.
            log::warn!(
                "GLM {name}: valid_range has {} element(s), expected 2; ignoring it",
                bounds.len()
            );
            return None;
        }
        let lo = reinterpret_unsigned(bounds[0], vartype, unsigned);
        let hi = reinterpret_unsigned(bounds[1], vartype, unsigned);
        if lo > hi {
            // An inverted range matches nothing and would silently empty the
            // variable.
            log::warn!(
                "GLM {name}: valid_range is inverted ({lo}..{hi}) after _Unsigned \
                 reinterpretation; ignoring it rather than discarding every value"
            );
            return None;
        }
        Some((lo, hi))
    });

    // Step 4: packing coefficients. Absent means identity, never zero.
    let scale = attr("scale_factor").and_then(|v| attr_as_f64(&v));
    let offset = attr("add_offset").and_then(|v| attr_as_f64(&v));

    let units = attr("units").and_then(|v| match v {
        CfAttr::Str(s) => Some(s),
        CfAttr::Nums(_) => None,
    });

    let values = raw
        .iter()
        .copied()
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

    UnpackedVar { values, units }
}

/// Bit-reinterpret a raw value that the file stores in a *signed* type but
/// means as unsigned: `-13585 → 51951`. Not `abs()` and not a negation.
pub(crate) fn reinterpret_unsigned(raw: f64, vartype: VarType, unsigned: bool) -> f64 {
    if !unsigned {
        return raw;
    }
    let VarType::SignedInt(bytes) = vartype else {
        // Natively unsigned storage and floats are already correct.
        return raw;
    };
    let bits = raw as i64;
    match bytes {
        1 => f64::from(bits as i8 as u8),
        2 => f64::from(bits as i16 as u16),
        4 => f64::from(bits as i32 as u32),
        _ => bits as u64 as f64,
    }
}

/// `_Unsigned` is spelled `"true"` in GLM files. The convention also allows a
/// numeric `1`, and some producers write `"1"`, so accept all of them.
pub(crate) fn attr_is_true(v: &CfAttr) -> bool {
    match v {
        CfAttr::Str(s) => {
            let s = s.trim();
            s.eq_ignore_ascii_case("true") || s == "1"
        }
        other => attr_as_f64(other).is_some_and(|n| n != 0.0),
    }
}

/// Coerce a scalar numeric attribute to `f64`.
///
/// NetCDF4 writers frequently store a logically-scalar attribute as a length-1
/// vector, so vectors are accepted as scalars too.
fn attr_as_f64(v: &CfAttr) -> Option<f64> {
    attr_as_f64_vec(v).first().copied()
}

fn attr_as_f64_vec(v: &CfAttr) -> Vec<f64> {
    match v {
        CfAttr::Nums(x) => x.clone(),
        // A text attribute is never a number. Returning empty (rather than
        // parsing the string) keeps `_FillValue = "n/a"` from becoming 0.
        CfAttr::Str(_) => Vec::new(),
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
/// GLM's `*_time_offset` variables carry exactly this, naming the granule's
/// `time_coverage_start`. Reading the epoch from the variable keeps the two
/// from silently diverging, and the unit multiplier means a future switch to
/// `milliseconds since` cannot quietly shift every strike by 1e3.
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
    let s = s
        .trim()
        .trim_end_matches('Z')
        .trim_end_matches(" UTC")
        .trim();

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
mod tests;
