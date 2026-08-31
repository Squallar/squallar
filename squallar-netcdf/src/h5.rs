//! Pure-Rust NetCDF4 access.
//!
//! NetCDF4 *is* HDF5: every netCDF variable is an HDF5 dataset in the root
//! group and every attribute an HDF5 attribute. [`hdf5_pure`] replaces
//! `netcdf`/`netcdf-sys`/`hdf5-metno-sys` and their bundled C sources, which is
//! what lets every consumer build for wasm32 and iOS.
//!
//! Does step 1 of the CF rules in [`crate::cf`] — read in the declared width,
//! and *keep* it — and hands off to [`cf::unpack`], which widens one element at
//! a time. See [`cf::RawValues`] for what a whole-array widening costs.

use std::collections::{BTreeMap, BTreeSet};

use crate::cf::{self, CfAttr, RawValues, RawVar, VarType};
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
                raw: empty_raw(&dtype, name)?,
                vartype,
                unsigned,
                attrs,
            }));
        }

        let raw = read_raw(&ds, name, &dtype, span)?;
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

/// The one table pairing an HDF5 storage type with its [`RawValues`] variant
/// and its two `hdf5_pure` readers.
///
/// One table rather than three parallel matches: a width added to the reader
/// but not to the empty-variable arm, or bound to the wrong variant, is the
/// kind of mistake that shows up as a wrong number on one code path only.
macro_rules! raw_storage {
    ($($dtype:ident / $variant:ident : $whole:ident / $windowed:ident -> $t:ty),* $(,)?) => {
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

        /// Read a variable's storage values **in the width the file declares**.
        ///
        /// The width must be the variable's *declared* one: reading a packed
        /// `short` as `f32` and reinterpreting afterwards bakes the sign in.
        /// It is also the width they stay in — `_Unsigned` and the widening to
        /// the `f64` raw domain both happen per element in
        /// [`cf::RawValues::map`], so the array is never materialised twice.
        fn read_raw(
            ds: &hdf5_pure::Dataset,
            name: &str,
            dtype: &DType,
            span: Span,
        ) -> Result<RawValues, String> {
            match dtype {
                $(
                    DType::$dtype => span
                        .$whole(ds)
                        .map(RawValues::$variant)
                        .map_err(|e| format!("Failed to read {name}: {e}")),
                )*
                other => Err(format!("Variable {name} has unsupported type {other:?}")),
            }
        }

        /// An empty variable, in its declared width.
        ///
        /// Separate from [`read_raw`] because `hdf5_pure` errors on storage
        /// that was never allocated, which is what a granule declaring
        /// `number_of_flashes = 0` has.
        fn empty_raw(dtype: &DType, name: &str) -> Result<RawValues, String> {
            match dtype {
                $(DType::$dtype => Ok(RawValues::$variant(Vec::new())),)*
                other => Err(format!("Variable {name} has unsupported type {other:?}")),
            }
        }
    };
}

raw_storage! {
    F32 / F32: read_f32 / read_f32_rows -> f32,
    F64 / F64: read_f64 / read_f64_rows -> f64,
    I8 / I8: read_i8 / read_i8_rows -> i8,
    I16 / I16: read_i16 / read_i16_rows -> i16,
    I32 / I32: read_i32 / read_i32_rows -> i32,
    I64 / I64: read_i64 / read_i64_rows -> i64,
    U8 / U8: read_u8 / read_u8_rows -> u8,
    U16 / U16: read_u16 / read_u16_rows -> u16,
    U32 / U32: read_u32 / read_u32_rows -> u32,
    U64 / U64: read_u64 / read_u64_rows -> u64,
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
///
/// **Every numeric width is spelled out, and that is the point.** `AttrValue`
/// is `#[non_exhaustive]`, so the wildcard below cannot be removed and a
/// variant added upstream arrives here as an attribute that silently is not
/// there. hdf5-pure 0.42 did exactly that: it split the numeric variants by
/// width and stopped promoting a stored `float` to `F64`, so GLM's
/// `scale_factor` — a float32 — became `F32`, fell through the wildcard, and
/// every flash value unpacked against an implied scale of 1. Nothing failed to
/// compile. The three GLM golden tests are what caught it, and they are the
/// gate that keeps catching it.
///
/// So: match a width because it exists, not because a file in hand uses it.
fn convert_attr(v: &AttrValue) -> Option<CfAttr> {
    /// The widths where `f64` is lossless, so the widening needs no comment.
    fn widen<T: Copy + Into<f64>>(xs: &[T]) -> CfAttr {
        CfAttr::Nums(xs.iter().map(|&x| x.into()).collect())
    }

    Some(match v {
        AttrValue::F32(x) => CfAttr::Nums(vec![f64::from(*x)]),
        AttrValue::F64(x) => CfAttr::Nums(vec![*x]),
        AttrValue::I8(x) => CfAttr::Nums(vec![f64::from(*x)]),
        AttrValue::I16(x) => CfAttr::Nums(vec![f64::from(*x)]),
        AttrValue::I32(x) => CfAttr::Nums(vec![f64::from(*x)]),
        AttrValue::U8(x) => CfAttr::Nums(vec![f64::from(*x)]),
        AttrValue::U16(x) => CfAttr::Nums(vec![f64::from(*x)]),
        AttrValue::U32(x) => CfAttr::Nums(vec![f64::from(*x)]),
        AttrValue::F32Array(x) => widen(x),
        AttrValue::F64Array(x) => CfAttr::Nums(x.clone()),
        AttrValue::I8Array(x) => widen(x),
        AttrValue::I16Array(x) => widen(x),
        AttrValue::I32Array(x) => widen(x),
        AttrValue::U8Array(x) => widen(x),
        AttrValue::U16Array(x) => widen(x),
        AttrValue::U32Array(x) => widen(x),

        // 64-bit integers are the one width `f64` cannot hold exactly. Rounding
        // them is what this reader has always done, and CF's own numeric
        // attributes — scale_factor, add_offset, _FillValue, valid_range — are
        // float or narrow integer, never a count near `i64::MAX`.
        AttrValue::I64(x) => CfAttr::Nums(vec![*x as f64]),
        AttrValue::U64(x) => CfAttr::Nums(vec![*x as f64]),
        AttrValue::I64Array(x) => CfAttr::Nums(x.iter().map(|&v| v as f64).collect()),
        AttrValue::U64Array(x) => CfAttr::Nums(x.iter().map(|&v| v as f64).collect()),

        // Charset, storage width and whether the value lives in a global heap
        // are the file's business; CF reads a string. `..` on the sized forms
        // because those variants are sealed upstream.
        AttrValue::String(s)
        | AttrValue::AsciiString(s)
        | AttrValue::VarLenString(s)
        | AttrValue::VarLenAsciiString(s) => CfAttr::Str(s.clone()),
        AttrValue::StringSized { value, .. } | AttrValue::AsciiStringSized { value, .. } => {
            CfAttr::Str(value.clone())
        }

        // Arrays of strings have no `CfAttr` shape to land in, and no CF
        // attribute this reader consults is one.
        _ => return None,
    })
}
