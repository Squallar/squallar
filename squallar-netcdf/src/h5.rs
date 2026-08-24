//! Pure-Rust NetCDF4 access.
//!
//! NetCDF4 *is* HDF5: every netCDF variable is an HDF5 dataset in the root
//! group and every attribute an HDF5 attribute. [`hdf5_pure`] replaces
//! `netcdf`/`netcdf-sys`/`hdf5-metno-sys` and their bundled C sources, which is
//! what lets every consumer build for wasm32 and iOS.
//!
//! Does steps 1 and 2 of the CF rules in [`crate::cf`] — read in the declared
//! width, bit-reinterpret through `_Unsigned` — and hands off to [`cf::unpack`].

use std::collections::{BTreeMap, BTreeSet};

use crate::cf::{self, CfAttr, RawVar, VarType};
use hdf5_pure::{AttrValue, DType};

pub struct Granule {
    file: hdf5_pure::File,
    /// Root-group dataset names, captured once so that "is this variable
    /// present?" is a lookup rather than an error to be pattern-matched.
    datasets: BTreeSet<String>,
}

impl Granule {
    /// Open a file from borrowed bytes, **copying them**.
    ///
    /// `hdf5_pure::File::from_bytes` takes ownership and offers no borrowing
    /// form, so the copy is the price of not already owning the buffer. At a
    /// few hundred KB — a GLM granule — that is noise. At 7.5 MB — a GMGSI
    /// granule — it is not, and a caller that *does* own its bytes should hand
    /// them over through [`from_vec`](Self::from_vec) and pay nothing.
    pub fn open(data: &[u8]) -> Result<Self, String> {
        Self::from_vec(data.to_vec())
    }

    /// Open a file from bytes this call takes ownership of.
    ///
    /// The no-copy form of [`open`](Self::open).
    pub fn from_vec(data: Vec<u8>) -> Result<Self, String> {
        let file = hdf5_pure::File::from_bytes(data)
            .map_err(|e| format!("Failed to open the HDF5 file: {e}"))?;
        let datasets = file
            .root()
            .datasets()
            .map_err(|e| format!("Failed to list the file's variables: {e}"))?
            .into_iter()
            .collect();
        Ok(Granule { file, datasets })
    }

    pub fn global_str(&self, name: &str) -> Option<String> {
        let attrs = self.file.root().attrs().ok()?;
        match attrs.get(name)? {
            AttrValue::String(s) | AttrValue::AsciiString(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// The variable's declared dimensions, or `Ok(None)` when it is absent.
    ///
    /// GLM never needed this — every L2 LCFA variable is a flat list whose only
    /// dimension is its own count. A gridded granule does: GMGSI declares
    /// `data(time, yc, xc)` and `lat(yc, xc)`, and the row length is the one
    /// fact that turns a flat read back into a raster.
    pub fn shape(&self, name: &str) -> Result<Option<Vec<u64>>, String> {
        if !self.datasets.contains(name) {
            return Ok(None);
        }
        self.file
            .dataset(name)
            .and_then(|d| d.shape())
            .map(Some)
            .map_err(|e| format!("Failed to read the shape of {name}: {e}"))
    }

    /// Returns `Ok(None)` when the variable is absent from the file, which is
    /// not the same as "present but all-missing": the L2 LCFA product has no
    /// `event_area` variable at all.
    pub fn read_unpacked(&self, name: &str) -> Result<Option<cf::UnpackedVar>, String> {
        Ok(self.raw_var(name)?.map(|v| cf::unpack(&v, name)))
    }

    /// [`read_unpacked`](Self::read_unpacked) into the cheap raster form.
    ///
    /// Same values, same CF rules, `NaN` for missing, a quarter of the memory.
    /// See [`cf::UnpackedF32`].
    pub fn read_unpacked_f32(&self, name: &str) -> Result<Option<cf::UnpackedF32>, String> {
        Ok(self.raw_var(name)?.map(|v| cf::unpack_f32(&v, name)))
    }

    /// [`read_unpacked`](Self::read_unpacked) over a window of the *first*
    /// dimension only — rows `[start_row, start_row + num_rows)`.
    ///
    /// Only the storage the window overlaps is read, so peak memory scales with
    /// the window rather than the variable. That is what makes pulling one row
    /// out of a 2-D coordinate variable cost a row instead of the whole array.
    ///
    /// The window is **clamped**: reading past the end yields the rows that
    /// exist, and an entirely out-of-range window yields an empty variable
    /// rather than an error. Pinned by
    /// [`crate::cf::tests::a_row_window_past_the_end_clamps_instead_of_failing`].
    pub fn read_unpacked_rows(
        &self,
        name: &str,
        start_row: u64,
        num_rows: u64,
    ) -> Result<Option<cf::UnpackedVar>, String> {
        Ok(self
            .raw_var_rows(name, start_row, num_rows)?
            .map(|v| cf::unpack(&v, name)))
    }

    /// [`read_unpacked_rows`](Self::read_unpacked_rows) into the cheap raster
    /// form — the two savings compose.
    pub fn read_unpacked_rows_f32(
        &self,
        name: &str,
        start_row: u64,
        num_rows: u64,
    ) -> Result<Option<cf::UnpackedF32>, String> {
        Ok(self
            .raw_var_rows(name, start_row, num_rows)?
            .map(|v| cf::unpack_f32(&v, name)))
    }

    /// Steps 1 and 2: raw storage values in the declared width, bit-
    /// reinterpreted through `_Unsigned`, plus the attributes CF needs.
    pub fn raw_var(&self, name: &str) -> Result<Option<RawVar>, String> {
        self.raw_var_span(name, Span::All)
    }

    /// [`raw_var`](Self::raw_var) over a clamped window of the first dimension.
    pub fn raw_var_rows(
        &self,
        name: &str,
        start_row: u64,
        num_rows: u64,
    ) -> Result<Option<RawVar>, String> {
        self.raw_var_span(
            name,
            Span::Rows {
                start: start_row,
                count: num_rows,
            },
        )
    }

    fn raw_var_span(&self, name: &str, span: Span) -> Result<Option<RawVar>, String> {
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
            .ok_or_else(|| format!("Variable {name} has unsupported type {dtype:?}"))?;

        let shape = ds
            .shape()
            .map_err(|e| format!("Failed to read the shape of {name}: {e}"))?;
        let count: u64 = span.elements(&shape);

        // An empty variable has no storage allocated and `hdf5_pure` errors, but
        // a granule that caught no lightning declares `number_of_flashes = 0`.
        // Short-circuit on the *declared* count only: a variable claiming 148
        // elements it cannot produce is still an error.
        if count == 0 {
            return Ok(Some(RawVar {
                raw: Vec::new(),
                vartype,
                unsigned,
                attrs,
            }));
        }

        let raw = read_raw(&ds, name, &dtype, unsigned, span)?;
        if raw.len() as u64 != count {
            return Err(format!(
                "Variable {name} declares {count} elements but {} were read",
                raw.len()
            ));
        }
        Ok(Some(RawVar {
            raw,
            vartype,
            unsigned,
            attrs,
        }))
    }
}

/// Which part of a variable a read covers.
///
/// The windowed arm exists because a consumer that wants one row of a 2-D
/// variable should not pay for the whole array: `hdf5_pure` reads only the
/// storage a window overlaps — the overlapping chunks, for a chunked layout —
/// so peak memory scales with the window.
#[derive(Debug, Clone, Copy)]
enum Span {
    All,
    /// Rows `[start, start + count)` of the first dimension, clamped to it.
    Rows {
        start: u64,
        count: u64,
    },
}

impl Span {
    /// How many elements this span selects from a variable of `shape`.
    ///
    /// Mirrors `hdf5_pure`'s own clamping, so the count checked against the
    /// read is the count the read will actually produce rather than the count
    /// the caller asked for.
    fn elements(self, shape: &[u64]) -> u64 {
        let inner: u64 = shape.iter().skip(1).product();
        match self {
            Span::All => shape.iter().product(),
            Span::Rows { start, count } => {
                // A 0-D scalar is one row, as it is for `read_raw_rows`.
                let rows = shape.first().copied().unwrap_or(1);
                let start = start.min(rows);
                count.min(rows - start) * inner
            }
        }
    }
}

/// One `Span::read_x` per width `read_raw` reads, each choosing between the
/// whole-variable call and its windowed companion.
macro_rules! span_readers {
    ($($whole:ident / $windowed:ident -> $t:ty),* $(,)?) => {
        impl Span {
            $(
                fn $whole(self, ds: &hdf5_pure::Dataset) -> Result<Vec<$t>, hdf5_pure::Error> {
                    match self {
                        Span::All => ds.$whole(),
                        Span::Rows { start, count } => ds.$windowed(start, count),
                    }
                }
            )*
        }
    };
}

span_readers! {
    read_f32 / read_f32_rows -> f32,
    read_f64 / read_f64_rows -> f64,
    read_i8 / read_i8_rows -> i8,
    read_i16 / read_i16_rows -> i16,
    read_i32 / read_i32_rows -> i32,
    read_i64 / read_i64_rows -> i64,
    read_u8 / read_u8_rows -> u8,
    read_u16 / read_u16_rows -> u16,
    read_u32 / read_u32_rows -> u32,
    read_u64 / read_u64_rows -> u64,
}

/// Read a variable's values into the raw (pre-scale) domain as `f64`,
/// reinterpreting the bits through `_Unsigned` where required.
///
/// The width must be the variable's *declared* one: reading a packed `short` as
/// `f32` and reinterpreting afterwards bakes the sign in.
fn read_raw(
    ds: &hdf5_pure::Dataset,
    name: &str,
    dtype: &DType,
    unsigned: bool,
    span: Span,
) -> Result<Vec<f64>, String> {
    let err = |e: hdf5_pure::Error| format!("Failed to read {name}: {e}");
    Ok(match dtype {
        DType::F32 => span
            .read_f32(ds)
            .map_err(err)?
            .into_iter()
            .map(f64::from)
            .collect(),
        DType::F64 => span.read_f64(ds).map_err(err)?,
        // Signed storage: `_Unsigned` reinterprets the bits, and only here.
        DType::I8 => span
            .read_i8(ds)
            .map_err(err)?
            .into_iter()
            .map(|v| {
                if unsigned {
                    f64::from(v as u8)
                } else {
                    f64::from(v)
                }
            })
            .collect(),
        DType::I16 => span
            .read_i16(ds)
            .map_err(err)?
            .into_iter()
            .map(|v| {
                if unsigned {
                    f64::from(v as u16)
                } else {
                    f64::from(v)
                }
            })
            .collect(),
        DType::I32 => span
            .read_i32(ds)
            .map_err(err)?
            .into_iter()
            .map(|v| {
                if unsigned {
                    f64::from(v as u32)
                } else {
                    f64::from(v)
                }
            })
            .collect(),
        DType::I64 => span
            .read_i64(ds)
            .map_err(err)?
            .into_iter()
            .map(|v| if unsigned { v as u64 as f64 } else { v as f64 })
            .collect(),
        // Already unsigned on disk: nothing to reinterpret.
        DType::U8 => span
            .read_u8(ds)
            .map_err(err)?
            .into_iter()
            .map(f64::from)
            .collect(),
        DType::U16 => span
            .read_u16(ds)
            .map_err(err)?
            .into_iter()
            .map(f64::from)
            .collect(),
        DType::U32 => span
            .read_u32(ds)
            .map_err(err)?
            .into_iter()
            .map(f64::from)
            .collect(),
        DType::U64 => span
            .read_u64(ds)
            .map_err(err)?
            .into_iter()
            .map(|v| v as f64)
            .collect(),
        other => {
            return Err(format!("Variable {name} has unsupported type {other:?}"));
        }
    })
}

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
/// `None` for attribute types CF never uses, so they read as absent.
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
