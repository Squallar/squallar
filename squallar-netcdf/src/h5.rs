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
use hdf5_pure::{AttrValue, DType, DatasetAccessProperties, Datatype, DatatypeByteOrder};

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
        let Some(var) = self.describe(name, DatasetAccessProperties::new())? else {
            return Ok(None);
        };
        var.raw_var(name, span).map(Some)
    }

    /// Open a variable **once**, for reads that will come back to it.
    ///
    /// Every other read on this type opens a fresh `hdf5_pure` handle, and a
    /// handle owns its chunk cache — so a walk that reads a variable in row
    /// windows through [`read_unpacked_rows`](Self::read_unpacked_rows) pays
    /// the chunk pipeline once **per window**, whatever the cache holds. For a
    /// variable stored as one chunk that is the whole variable inflated per
    /// window: GMGSI's 3000 x 5000 coordinate arrays cost 44 inflations of
    /// 60 MB per granule that way, measured, when the walk only ever wanted
    /// one column and a few probe rows.
    ///
    /// A [`Variable`] keeps one handle whose chunk cache is sized to hold
    /// **one of its chunks** — `hdf5_pure`'s default is 1 MiB, which admits
    /// nothing a mosaic is stored in — so the windows share an inflation.
    /// Pinned by
    /// [`crate::cf::tests::a_variable_handle_serves_its_windows_off_one_inflation`].
    /// `Ok(None)` when the variable is absent.
    pub fn variable(&self, name: &str) -> Result<Option<Variable>, String> {
        // The chunk shape has to be read off the dataset before the handle
        // that will keep it can be sized, so the first open is only a look.
        let Some(probe) = self.describe(name, DatasetAccessProperties::new())? else {
            return Ok(None);
        };
        let chunk_bytes = probe
            .ds
            .chunk_shape()
            .map_err(|e| format!("Failed to read the chunk shape of {name}: {e}"))?
            .map(|dims| {
                let elems: u64 = dims.iter().product();
                let width = probe
                    .ds
                    .datatype()
                    .map(|dt| u64::from(dt.type_size()))
                    .unwrap_or(0);
                usize::try_from(elems.saturating_mul(width)).unwrap_or(usize::MAX)
            });
        let options = match chunk_bytes {
            // One chunk, no more: a many-chunk variable read whole through
            // this handle must not quietly retain a second copy of itself.
            Some(bytes) => DatasetAccessProperties::new().with_chunk_cache(
                hdf5_pure::ChunkCacheConfig::new()
                    .with_max_bytes(bytes)
                    .with_max_slots(1),
            ),
            // Compact and contiguous layouts have no chunks to cache.
            None => DatasetAccessProperties::new(),
        };
        drop(probe);
        let Some(inner) = self.describe(name, options)? else {
            return Ok(None);
        };
        Ok(Some(Variable {
            name: name.to_string(),
            inner,
        }))
    }

    /// [`read_unpacked_f32`](Self::read_unpacked_f32), **appended to a buffer
    /// the caller owns** instead of into a fresh one.
    ///
    /// Same values, same CF rules, `NaN` for missing — pinned bit-for-bit
    /// against the owning form by
    /// [`crate::cf::tests::the_appending_read_is_the_owning_read_bit_for_bit`].
    /// `Ok(None)` when the variable is absent; otherwise how many values were
    /// appended. `units` is not returned: a caller that wants it reads the
    /// attribute through the owning form, and the one caller this exists for
    /// reads counts.
    ///
    /// Why it exists: a 3000 x 5000 raster is 60,000,000 B, and the owning read
    /// materialises it **three times** over — `hdf5_pure` assembles the stored
    /// bytes, decodes them into a storage-width `Vec`, and [`cf::unpack_f32`]
    /// collects the unpacked copy — with two of the three live at once. On a
    /// wasm32 heap that only grows, a fresh 60 MB block per granule is what
    /// fragments it to death (`squallar_overlays::staging`), so the decode has
    /// to land in a retained buffer rather than in a new one.
    ///
    /// **Storage declared as a standard 4-byte IEEE float goes straight from
    /// the stored bytes into `out`**, one element at a time, with no
    /// storage-width copy between: the arithmetic is exactly
    /// [`cf::RawValues::F32`]'s arm of `unpack_f32` — `f64::from` the stored
    /// value, [`cf::Packing`], narrow — so the bits are the same. The
    /// standard-layout test mirrors `hdf5_pure`'s own bulk-decode gate (byte
    /// order little or big, bit offset 0, precision the full width); anything
    /// else, and every other storage width, takes the owning read and is
    /// appended from it, so no width is refused and no CF rule is spelled
    /// twice.
    ///
    /// What this cannot remove: `hdf5_pure` 0.44 offers a whole-dataset read
    /// and a first-dimension row window and nothing finer, so the assembled
    /// stored bytes — 60,000,000 B for that raster — are still one block, held
    /// for the length of this call. Per granule that is one transient block
    /// where there were three.
    pub fn read_unpacked_f32_into(
        &self,
        name: &str,
        out: &mut Vec<f32>,
    ) -> Result<Option<usize>, String> {
        let Some(var) = self.describe(name, DatasetAccessProperties::new())? else {
            return Ok(None);
        };
        let count = Span::All.elements(&var.shape);
        if count == 0 {
            return Ok(Some(0));
        }
        let count = usize::try_from(count)
            .map_err(|_| format!("Variable {name} declares {count} elements, more than fit"))?;

        let datatype = var
            .ds
            .datatype()
            .map_err(|e| format!("Failed to read the type of {name}: {e}"))?;
        let order = match (&var.dtype, &datatype) {
            (
                DType::F32,
                Datatype::FloatingPoint {
                    size: 4,
                    byte_order,
                    bit_offset: 0,
                    bit_precision: 32,
                    ..
                },
            ) => match byte_order {
                DatatypeByteOrder::LittleEndian => Some(true),
                DatatypeByteOrder::BigEndian => Some(false),
                _ => None,
            },
            _ => None,
        };
        let Some(little) = order else {
            let raw = read_raw(&var.ds, name, &var.dtype, Span::All)?;
            if raw.len() != count {
                return Err(format!(
                    "Variable {name} declares {count} elements but {} were read",
                    raw.len()
                ));
            }
            let raw = RawVar {
                raw,
                vartype: var.vartype,
                unsigned: var.unsigned,
                attrs: var.attrs,
            };
            let unpacked = cf::unpack_f32(&raw, name);
            out.try_reserve(unpacked.values.len())
                .map_err(|_| format!("Variable {name}: cannot hold {count} values"))?;
            out.extend_from_slice(&unpacked.values);
            return Ok(Some(unpacked.values.len()));
        };

        let (packing, _units) = cf::Packing::resolve(var.vartype, var.unsigned, &var.attrs, name);
        let bytes = var
            .ds
            .read_raw()
            .map_err(|e| format!("Failed to read {name}: {e}"))?;
        if bytes.len() != count * size_of::<f32>() {
            return Err(format!(
                "Variable {name} declares {count} elements but {} bytes were read",
                bytes.len()
            ));
        }
        out.try_reserve(count)
            .map_err(|_| format!("Variable {name}: cannot hold {count} values"))?;
        for word in bytes.chunks_exact(size_of::<f32>()) {
            let word: [u8; 4] = word.try_into().expect("chunks_exact yields four bytes");
            let stored = if little {
                f32::from_le_bytes(word)
            } else {
                f32::from_be_bytes(word)
            };
            out.push(
                packing
                    .apply(f64::from(stored))
                    .map_or(f32::NAN, |v| v as f32),
            );
        }
        Ok(Some(count))
    }

    /// Open a variable and resolve everything about it that is not its values.
    fn describe(
        &self,
        name: &str,
        options: DatasetAccessProperties,
    ) -> Result<Option<Described>, String> {
        if !self.datasets.contains(name) {
            return Ok(None);
        }
        let ds = self
            .file
            .dataset_with_options(name, options)
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
        Ok(Some(Described {
            ds,
            dtype,
            vartype,
            unsigned,
            attrs,
            shape,
        }))
    }
}

/// A variable's handle and its resolved CF facts, before any value is read.
struct Described {
    ds: hdf5_pure::Dataset,
    dtype: DType,
    vartype: VarType,
    unsigned: bool,
    attrs: BTreeMap<String, CfAttr>,
    shape: Vec<u64>,
}

impl Described {
    /// Steps 1 and 2 over `span`: raw storage values in the declared width,
    /// bit-reinterpreted through `_Unsigned`, plus the attributes CF needs.
    fn raw_var(&self, name: &str, span: Span) -> Result<RawVar, String> {
        let count: u64 = span.elements(&self.shape);

        // An empty variable has no storage allocated and `hdf5_pure` errors, but
        // a granule that caught no lightning declares `number_of_flashes = 0`.
        // Short-circuit on the *declared* count only: a variable claiming 148
        // elements it cannot produce is still an error.
        if count == 0 {
            return Ok(RawVar {
                raw: empty_raw(&self.dtype, name)?,
                vartype: self.vartype,
                unsigned: self.unsigned,
                attrs: self.attrs.clone(),
            });
        }

        let raw = read_raw(&self.ds, name, &self.dtype, span)?;
        if raw.len() as u64 != count {
            return Err(format!(
                "Variable {name} declares {count} elements but {} were read",
                raw.len()
            ));
        }
        Ok(RawVar {
            raw,
            vartype: self.vartype,
            unsigned: self.unsigned,
            attrs: self.attrs.clone(),
        })
    }
}

/// **Everything that decides what a chunked variable decodes to, as stored**
/// — its shape, type, chunking, filter pipeline, CF attributes, and every
/// chunk's own stored (still-compressed) bytes.
///
/// Two granules whose variable fingerprints are equal decode that variable
/// to the same values: decoding is a pure function of these inputs and
/// nothing else. That is a stronger fact than "same shape" and a cheaper one
/// to establish than "same values" — GMGSI's 3000 x 5000 coordinate arrays
/// are 60,000,000 B decoded and ~446 KB stored, and the same on every
/// granule, so a caller can keep the axes it derived and prove the next
/// granule's are identical without inflating a byte.
///
/// Compared by `==`, not hashed: the stored bytes are carried whole, so
/// equality is a memcmp and there is no collision to argue about.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFingerprint {
    shape: Vec<u64>,
    datatype: Datatype,
    chunk_shape: Vec<u64>,
    filters: Vec<hdf5_pure::Filter>,
    attrs: BTreeMap<String, CfAttr>,
    /// Per chunk: logical offset, filter mask, stored bytes.
    chunks: Vec<(Vec<u64>, u32, Vec<u8>)>,
}

impl StoredFingerprint {
    /// How many stored bytes this fingerprint carries — what keeping one
    /// costs.
    pub fn stored_bytes(&self) -> usize {
        self.chunks.iter().map(|(_, _, b)| b.len()).sum()
    }

    /// Each chunk's stored bytes, in index order — what a test compares
    /// against the file to show the address arithmetic is right.
    pub fn chunk_bytes(&self) -> impl Iterator<Item = &[u8]> {
        self.chunks.iter().map(|(_, _, b)| b.as_slice())
    }
}

impl Granule {
    /// The [`StoredFingerprint`] of `name`, or `Ok(None)` when the variable
    /// is absent or cannot be fingerprinted: it is not chunked, its chunk
    /// index has no enumerator, or some of its chunks were never written
    /// (those decode to the fill value, which this key does not carry).
    ///
    /// Pinned by
    /// [`crate::cf::tests::a_stored_fingerprint_is_the_stored_bytes_and_moves_with_them`].
    pub fn stored_fingerprint(&self, name: &str) -> Result<Option<StoredFingerprint>, String> {
        let Some(var) = self.describe(name, DatasetAccessProperties::new())? else {
            return Ok(None);
        };
        let Some(chunk_shape) = var
            .ds
            .chunk_shape()
            .map_err(|e| format!("Failed to read the chunk shape of {name}: {e}"))?
        else {
            return Ok(None);
        };
        let Ok(chunks) = var.ds.chunks() else {
            return Ok(None);
        };
        let expected: u64 = var
            .shape
            .iter()
            .zip(&chunk_shape)
            .map(|(d, c)| if *c == 0 { 0 } else { d.div_ceil(*c) })
            .product();
        if chunks.len() as u64 != expected {
            return Ok(None);
        }
        let file = self.file.as_bytes();
        let mut stored = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let (Ok(start), Ok(len)) = (
                usize::try_from(chunk.address),
                usize::try_from(chunk.storage_size),
            ) else {
                return Ok(None);
            };
            let Some(bytes) = start.checked_add(len).and_then(|end| file.get(start..end)) else {
                return Ok(None);
            };
            stored.push((chunk.offset, chunk.filter_mask, bytes.to_vec()));
        }
        let datatype = var
            .ds
            .datatype()
            .map_err(|e| format!("Failed to read the type of {name}: {e}"))?;
        Ok(Some(StoredFingerprint {
            shape: var.shape,
            datatype,
            chunk_shape,
            filters: var.ds.filter_pipeline(),
            attrs: var.attrs,
            chunks: stored,
        }))
    }
}

/// A variable held open across reads — see [`Granule::variable`].
pub struct Variable {
    name: String,
    inner: Described,
}

impl Variable {
    /// The variable's declared dimensions.
    pub fn shape(&self) -> &[u64] {
        &self.inner.shape
    }

    /// [`Granule::read_unpacked_rows_f32`] through this handle: rows
    /// `[start_row, start_row + num_rows)`, clamped, into the raster form.
    pub fn read_unpacked_rows_f32(
        &self,
        start_row: u64,
        num_rows: u64,
    ) -> Result<cf::UnpackedF32, String> {
        let raw = self.inner.raw_var(
            &self.name,
            Span::Rows {
                start: start_row,
                count: num_rows,
            },
        )?;
        Ok(cf::unpack_f32(&raw, &self.name))
    }

    /// What this handle's chunk cache has done — the figure that says whether
    /// the windows shared an inflation or each paid for one.
    pub fn chunk_cache_stats(&self) -> hdf5_pure::ChunkCacheStats {
        self.inner.ds.chunk_cache_stats()
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
