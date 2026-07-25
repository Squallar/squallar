//! Pure-Rust HDF5 access to a GLM granule.
//!
//! GOES GLM L2 LCFA files are NetCDF4, which *is* HDF5: every netCDF variable
//! is an HDF5 dataset in the root group and every netCDF attribute is an HDF5
//! attribute on it. Nothing here needs the netCDF layer above that, so
//! [`hdf5_pure`] replaces `netcdf`/`netcdf-sys`/`hdf5-metno-sys` and their
//! bundled C sources — which is what lets the overlay build for wasm32 and iOS.
//!
//! Does steps 1 and 2 of the CF rules in [`super::cf`] — read in the declared
//! width, bit-reinterpret through `_Unsigned` — and hands off to [`cf::unpack`].

use std::collections::{BTreeMap, BTreeSet};

use super::cf::{self, CfAttr, RawVar, VarType};
use hdf5_pure::{AttrValue, DType};

/// An open GLM granule, read straight from the downloaded bytes.
pub(crate) struct Granule {
    file: hdf5_pure::File,
    /// Root-group dataset names, captured once so that "is this variable
    /// present?" is a lookup rather than an error to be pattern-matched.
    datasets: BTreeSet<String>,
}

impl Granule {
    /// Parse the superblock and root group of an in-memory granule.
    pub(crate) fn open(data: &[u8]) -> Result<Self, String> {
        // `hdf5_pure` wants ownership; a GLM granule is a few hundred KB.
        let file = hdf5_pure::File::from_bytes(data.to_vec())
            .map_err(|e| format!("Failed to open GLM HDF5 file: {e}"))?;
        let datasets = file
            .root()
            .datasets()
            .map_err(|e| format!("Failed to list GLM variables: {e}"))?
            .into_iter()
            .collect();
        Ok(Granule { file, datasets })
    }

    /// A root-group (global) attribute, if it is text.
    pub(crate) fn global_str(&self, name: &str) -> Option<String> {
        let attrs = self.file.root().attrs().ok()?;
        match attrs.get(name)? {
            AttrValue::String(s) | AttrValue::AsciiString(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Read a variable and apply CF packing conventions.
    ///
    /// Returns `Ok(None)` when the variable is absent from the file, which is
    /// not the same as "present but all-missing": the L2 LCFA product has no
    /// `event_area` variable at all.
    pub(crate) fn read_unpacked(&self, name: &str) -> Result<Option<cf::UnpackedVar>, String> {
        Ok(self.raw_var(name)?.map(|v| cf::unpack(&v, name)))
    }

    /// Steps 1 and 2: raw storage values in the declared width, bit-
    /// reinterpreted through `_Unsigned`, plus the attributes CF needs.
    pub(crate) fn raw_var(&self, name: &str) -> Result<Option<RawVar>, String> {
        if !self.datasets.contains(name) {
            return Ok(None);
        }
        let ds = self
            .file
            .dataset(name)
            .map_err(|e| format!("Failed to open {name}: {e}"))?;

        let attrs: BTreeMap<String, CfAttr> = ds
            .attrs()
            .map_err(|e| format!("Failed to read attributes of {name}: {e}"))?
            .into_iter()
            .filter_map(|(k, v)| convert_attr(&v).map(|v| (k, v)))
            .collect();

        // Step 2 first: it decides how the bits are read.
        let unsigned = attrs.get("_Unsigned").is_some_and(cf::attr_is_true);

        let dtype = ds
            .dtype()
            .map_err(|e| format!("Failed to read the type of {name}: {e}"))?;
        let vartype = var_type(&dtype)
            .ok_or_else(|| format!("GLM variable {name} has unsupported type {dtype:?}"))?;

        let shape = ds
            .shape()
            .map_err(|e| format!("Failed to read the shape of {name}: {e}"))?;
        // A scalar has an empty shape and one element, so `product()` of no
        // dimensions is 1; `product()` of `[0]` is already 0.
        let count: u64 = shape.iter().product();

        // An empty variable has no storage allocated and `hdf5_pure` errors
        // rather than returning nothing, but a granule that caught no lightning
        // declares `number_of_flashes = 0` and its `flash_*` variables are
        // legitimately empty. Short-circuit on the *declared* count only: a
        // variable claiming 148 elements it cannot produce is still an error.
        if count == 0 {
            return Ok(Some(RawVar { raw: Vec::new(), vartype, unsigned, attrs }));
        }

        let raw = read_raw(&ds, name, &dtype, unsigned)?;
        if raw.len() as u64 != count {
            return Err(format!(
                "GLM variable {name} declares {count} elements but {} were read",
                raw.len()
            ));
        }
        Ok(Some(RawVar { raw, vartype, unsigned, attrs }))
    }
}

/// Read a variable's values into the raw (pre-scale) domain as `f64`,
/// reinterpreting the bits through `_Unsigned` where required.
///
/// `f64` holds every `u32`/`i32` and every 16-bit value exactly.
///
/// The width must be the variable's *declared* one: reading a packed `short` as
/// `f32` and reinterpreting afterwards bakes the sign in.
fn read_raw(
    ds: &hdf5_pure::Dataset,
    name: &str,
    dtype: &DType,
    unsigned: bool,
) -> Result<Vec<f64>, String> {
    let err = |e: hdf5_pure::Error| format!("Failed to read {name}: {e}");
    Ok(match dtype {
        DType::F32 => ds.read_f32().map_err(err)?.into_iter().map(f64::from).collect(),
        DType::F64 => ds.read_f64().map_err(err)?,
        // Signed storage: `_Unsigned` reinterprets the bits, and only here.
        DType::I8 => ds
            .read_i8()
            .map_err(err)?
            .into_iter()
            .map(|v| if unsigned { f64::from(v as u8) } else { f64::from(v) })
            .collect(),
        DType::I16 => ds
            .read_i16()
            .map_err(err)?
            .into_iter()
            .map(|v| if unsigned { f64::from(v as u16) } else { f64::from(v) })
            .collect(),
        DType::I32 => ds
            .read_i32()
            .map_err(err)?
            .into_iter()
            .map(|v| if unsigned { f64::from(v as u32) } else { f64::from(v) })
            .collect(),
        DType::I64 => ds
            .read_i64()
            .map_err(err)?
            .into_iter()
            .map(|v| if unsigned { v as u64 as f64 } else { v as f64 })
            .collect(),
        // Already unsigned on disk: nothing to reinterpret.
        DType::U8 => ds.read_u8().map_err(err)?.into_iter().map(f64::from).collect(),
        DType::U16 => ds.read_u16().map_err(err)?.into_iter().map(f64::from).collect(),
        DType::U32 => ds.read_u32().map_err(err)?.into_iter().map(f64::from).collect(),
        DType::U64 => ds.read_u64().map_err(err)?.into_iter().map(|v| v as f64).collect(),
        other => return Err(format!("GLM variable {name} has unsupported type {other:?}")),
    })
}

/// Map an HDF5 datatype onto the distinction CF unpacking needs.
fn var_type(dtype: &DType) -> Option<VarType> {
    Some(match dtype {
        DType::F32 | DType::F64 => VarType::Float,
        DType::I8 => VarType::SignedInt(1),
        DType::I16 => VarType::SignedInt(2),
        DType::I32 => VarType::SignedInt(4),
        DType::I64 => VarType::SignedInt(8),
        DType::U8 | DType::U16 | DType::U32 | DType::U64 => VarType::UnsignedInt,
        _ => return None,
    })
}

/// Normalize an `hdf5_pure` attribute into the backend-neutral form.
///
/// `None` for attribute types CF never uses (string arrays, object references),
/// so they read as absent rather than as numbers.
fn convert_attr(v: &AttrValue) -> Option<CfAttr> {
    Some(match v {
        AttrValue::F64(x) => CfAttr::Nums(vec![*x]),
        AttrValue::F64Array(x) => CfAttr::Nums(x.clone()),
        AttrValue::I32(x) => CfAttr::Nums(vec![f64::from(*x)]),
        AttrValue::I64(x) => CfAttr::Nums(vec![*x as f64]),
        AttrValue::I64Array(x) => CfAttr::Nums(x.iter().map(|&v| v as f64).collect()),
        AttrValue::U32(x) => CfAttr::Nums(vec![f64::from(*x)]),
        AttrValue::U64(x) => CfAttr::Nums(vec![*x as f64]),
        AttrValue::String(s) | AttrValue::AsciiString(s) => CfAttr::Str(s.clone()),
        _ => return None,
    })
}
