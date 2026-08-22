//! CF-convention unpacking: what a variable's stored bytes actually mean.
//!
//! A NetCDF4 producer routinely stores a quantity as a *packed* integer, and
//! neither an HDF5 reader nor netCDF-C applies the CF attributes that say how to
//! read it back. The examples below are GOES GLM L2 LCFA, kept because they are
//! measured rather than illustrative: unpacked naively, its `event_lat` reads
//! `-13585.0` where the real latitude is `38.967°N`.
//!
//! Nothing here knows what product it is unpacking. Every constant comes out of
//! the file's own attributes — see rule 6.
//!
//! Order matters. [`crate::h5`] does steps 1 and 2 — only the reader can ask
//! its library for a specific width — and [`unpack`] does the rest:
//!
//! 1. **Read the raw storage values in the variable's declared type.** Once a
//!    `short` has been widened to `float` the sign bit is baked in.
//! 2. **Apply `_Unsigned`.** NOAA declares the variable `short` and attaches
//!    `_Unsigned = "true"`, so a raw negative `i16` must be *bit-reinterpreted*,
//!    not negated: `-13585_i16 as u16 == 51951`. On disk the datatype really is
//!    `H5T_STD_I16LE` — the attribute carries the meaning. A numeric `1` is also
//!    permitted.
//! 3. **Apply `_FillValue` and `valid_range` in the raw (post-`_Unsigned`,
//!    pre-scale) domain.** GLM writes `_FillValue = -1s` and
//!    `valid_range = 0s, -6s`, i.e. `65535` and `0..=65530`; such values are
//!    **missing** and must not be scaled and published.
//! 4. **Only then apply `raw * scale_factor + add_offset`.** Missing means 1 and
//!    0 respectively; the arithmetic is done in `f64`.
//! 5. **Variables genuinely stored as `float`/`double` with no packing
//!    attributes pass through untouched** — GLM's `group_lat`/`flash_lat` are
//!    real `float` degrees, which is why those levels looked plausible while
//!    the packed event level was garbage. A file mixes the two freely, so this
//!    is per-variable and never per-file.
//! 6. **Never hardcode the constants.** `event_lon:add_offset` tracks the
//!    satellite sub-point: -141.56 for GOES-East, -203.56 for GOES-West, which
//!    moves the valid interval past the antimeridian. Renormalising a
//!    longitude that ran off the end of the range is the *caller's* job — it
//!    is a question about a sphere, not about a file.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarType {
    /// Signed integer storage, width in bytes (1, 2, 4 or 8).
    SignedInt(u8),
    UnsignedInt,
    Float,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CfAttr {
    Nums(Vec<f64>),
    Str(String),
}

pub struct RawVar {
    pub raw: Vec<f64>,
    /// The declared storage type, needed to reinterpret `_FillValue` and
    /// `valid_range` into the same domain as `raw`.
    pub vartype: VarType,
    /// Whether `_Unsigned` applied. Carried separately because `raw` has
    /// already been reinterpreted but the *attributes* have not.
    pub unsigned: bool,
    pub attrs: BTreeMap<String, CfAttr>,
}

pub struct UnpackedVar {
    /// One entry per element. `None` means the file marked the element
    /// missing, via `_FillValue` or `valid_range`.
    pub values: Vec<Option<f64>>,
    /// The variable's `units` attribute, verbatim. Callers must consult this
    /// rather than assuming a unit — GLM areas are `m2`, not `km²`.
    pub units: Option<String>,
}

/// The same values as [`UnpackedVar`], one quarter the memory.
///
/// **Missing is `NaN`.** That is not an ambiguous encoding: [`Packing::apply`]
/// already reports a non-finite *unpacked* value as missing, so no present
/// value can be `NaN` — pinned by
/// [`tests::a_non_finite_unpacked_value_is_missing_in_both_representations`].
///
/// Why it exists: `Option<f64>` is 16 bytes, because `f64` has no spare bit
/// pattern for the discriminant to hide in. A 15,000,000-element raster is
/// therefore 240 MB as [`UnpackedVar`] and 60 MB here. For a column of a few
/// hundred records that difference does not matter and [`UnpackedVar`] reads
/// better; for a raster it is the difference between the payload and four times
/// the payload. Pinned by
/// [`tests::the_raster_form_costs_a_quarter_of_the_option_form`].
pub struct UnpackedF32 {
    /// One entry per element, `NaN` where the file marked the element missing.
    pub values: Vec<f32>,
    /// The variable's `units` attribute, verbatim. Callers must consult this
    /// rather than assuming a unit — GLM areas are `m2`, not `km²`.
    pub units: Option<String>,
}

/// Steps 3 to 5 of the rules at the top of this module, resolved once for a
/// variable and then applied per element.
///
/// One type, so that the two representations [`unpack`] and [`unpack_f32`]
/// produce cannot disagree about a CF rule. Splitting them into two hand-written
/// loops is exactly the change that would let `_Unsigned` be right in the form
/// the tests read and wrong in the form the raster reads.
struct Packing {
    /// `_FillValue`, already reinterpreted into the same domain as `raw`.
    fill: Option<f64>,
    /// `valid_range`, likewise, and only if it was well-formed.
    valid_range: Option<(f64, f64)>,
    scale: Option<f64>,
    offset: Option<f64>,
}

impl Packing {
    /// Resolve the packing attributes, and the `units` string beside them.
    fn read(var: &RawVar, name: &str) -> (Self, Option<String>) {
        let RawVar {
            raw: _,
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
        // CF's alternative `valid_min`/`valid_max` spelling is unsupported:
        // neither producer this crate reads uses it.
        let valid_range = attr("valid_range").and_then(|v| {
            let bounds = attr_as_f64_vec(&v);
            if bounds.len() != 2 {
                // Ignoring a malformed range is the safe direction: it can only
                // mark *fewer* values missing.
                log::warn!(
                    "{name}: valid_range has {} element(s), expected 2; ignoring it",
                    bounds.len()
                );
                return None;
            }
            let lo = reinterpret_unsigned(bounds[0], vartype, unsigned);
            let hi = reinterpret_unsigned(bounds[1], vartype, unsigned);
            if lo > hi {
                // An inverted range matches nothing and would silently empty
                // the variable.
                log::warn!(
                    "{name}: valid_range is inverted ({lo}..{hi}) after _Unsigned \
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

        (
            Packing {
                fill,
                valid_range,
                scale,
                offset,
            },
            units,
        )
    }

    /// One raw storage value to its unpacked meaning, or `None` for missing.
    #[inline]
    fn apply(&self, r: f64) -> Option<f64> {
        if self.fill.is_some_and(|f| r == f) {
            return None;
        }
        if self.valid_range.is_some_and(|(lo, hi)| r < lo || r > hi) {
            return None;
        }
        let mut v = r;
        if let Some(s) = self.scale {
            v *= s;
        }
        if let Some(o) = self.offset {
            v += o;
        }
        // A packed integer can still unpack to a non-finite value if the file's
        // coefficients are nonsense; treat that as missing too.
        v.is_finite().then_some(v)
    }
}

/// Apply CF packing conventions to a variable's raw storage values — steps 3 to
/// 5 of the rules at the top of this module.
///
/// Missing is `None`. For a raster, prefer [`unpack_f32`], which is the same
/// arithmetic at a quarter of the memory.
pub fn unpack(var: &RawVar, name: &str) -> UnpackedVar {
    let (packing, units) = Packing::read(var, name);
    let values = var.raw.iter().copied().map(|r| packing.apply(r)).collect();
    UnpackedVar { values, units }
}

/// [`unpack`] into `f32`, with `NaN` for missing.
///
/// The arithmetic is identical and happens in `f64`; only the *stored* result
/// narrows. A caller that will put the values in a texture or a raster wants
/// this one — see [`UnpackedF32`] for the size argument.
pub fn unpack_f32(var: &RawVar, name: &str) -> UnpackedF32 {
    let (packing, units) = Packing::read(var, name);
    let values = var
        .raw
        .iter()
        .copied()
        .map(|r| packing.apply(r).map_or(f32::NAN, |v| v as f32))
        .collect();
    UnpackedF32 { values, units }
}

/// Bit-reinterpret a raw value that the file stores in a *signed* type but
/// means as unsigned: `-13585 → 51951`. Not `abs()` and not a negation.
pub fn reinterpret_unsigned(raw: f64, vartype: VarType, unsigned: bool) -> f64 {
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
pub fn attr_is_true(v: &CfAttr) -> bool {
    match v {
        CfAttr::Str(s) => {
            let s = s.trim();
            s.eq_ignore_ascii_case("true") || s == "1"
        }
        other => attr_as_f64(other).is_some_and(|n| n != 0.0),
    }
}

/// Coerce a scalar numeric attribute to `f64`; a length-1 vector counts as one.
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeUnits {
    pub seconds_per_unit: f64,
    pub epoch: chrono::NaiveDateTime,
}

/// Parse a CF time `units` string such as
/// `"seconds since 2026-07-24 12:00:00.000"`.
///
/// GLM's `*_time_offset` variables carry exactly this, naming the granule's
/// `time_coverage_start`; the unit multiplier means a switch to `milliseconds
/// since` cannot quietly shift every strike by 1e3.
pub fn parse_time_units(units: &str) -> Option<TimeUnits> {
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

pub fn parse_cf_epoch(s: &str) -> Option<chrono::NaiveDateTime> {
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
