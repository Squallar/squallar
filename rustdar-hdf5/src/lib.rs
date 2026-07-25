//! A pure-Rust reader for the narrow slice of HDF5 that GOES GLM L2 files use.
//!
//! This exists so that rustdar can read GLM lightning data on wasm32 and iOS,
//! where the `netcdf` crate's C dependencies (`netcdf-sys`, `hdf5-metno-sys`
//! and their bundled sources) cannot go. It is **not** a general HDF5 library
//! and does not try to be: it decodes exactly the structures a GLM granule
//! contains and returns a clear `Unsupported` error for everything else.
//!
//! What it handles, all of it verified against a real granule:
//!
//! * superblock version 2
//! * object header version 2, including continuation (`OCHK`) blocks
//! * dense group link storage, by walking the fractal heap directly
//! * dataspace versions 1 and 2, datatype version 1 (IEEE floats and integers)
//! * data layout version 3, contiguous and chunked
//! * version-1 b-tree chunk indexes
//!
//! What it deliberately refuses: filtered data, big-endian types, compact
//! layout, layout version 4, huge/tiny fractal heap objects, and object header
//! version 1. Each is a distinct [`Error::Unsupported`] message so a file that
//! needs one fails loudly instead of returning plausible wrong numbers.
//!
//! CF conventions — `scale_factor`, `add_offset`, `_FillValue`, `_Unsigned` —
//! are *not* applied here. This crate returns the raw stored values; unpacking
//! them is the caller's job and rustdar already implements it.

mod bytes;
mod dataset;
mod header;
mod heap;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use bytes::{addr_to_index, Cursor};
pub use dataset::DataType;
use dataset::Layout;

/// Everything that can go wrong reading a file.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("not an HDF5 file: bad magic number")]
    BadMagic,
    #[error("expected block signature {want:?}, found {got:?}")]
    BadSignature { want: [u8; 4], got: [u8; 4] },
    #[error("file ended in the middle of a structure")]
    Truncated,
    #[error("malformed file: {0}")]
    Malformed(&'static str),
    #[error("unsupported HDF5 feature: {0}")]
    Unsupported(&'static str),
    #[error("followed an undefined file address")]
    UndefinedAddress,
    #[error("no variable named {0:?}")]
    NoSuchVariable(String),
    #[error("variable is {actual:?}, not {wanted}")]
    TypeMismatch {
        actual: DataType,
        wanted: &'static str,
    },
    #[error("group heap holds {expected} links but only {found} could be read")]
    LinkCountMismatch { expected: u64, found: u64 },
    #[error("chunked dataset does not cover all {missing} of its trailing elements")]
    IncompleteChunkCoverage { missing: u64 },
}

/// A parsed file: the root group's variables, ready to read by name.
#[derive(Debug)]
pub struct Hdf5File<'a> {
    data: &'a [u8],
    variables: BTreeMap<String, u64>,
}

/// A dataset's shape and element type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableInfo {
    pub dims: Vec<u64>,
    pub dtype: DataType,
}

impl VariableInfo {
    /// Total number of elements.
    pub fn len(&self) -> u64 {
        self.dims.iter().copied().product::<u64>().max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.dims.contains(&0)
    }
}

impl<'a> Hdf5File<'a> {
    /// Parses the superblock and the root group's link table.
    ///
    /// Only the group structure is read here; dataset bytes are read on demand
    /// by [`Hdf5File::read_f32`] and friends.
    pub fn parse(data: &'a [u8]) -> Result<Self, Error> {
        let root = read_superblock(data)?;
        let msgs = header::read_object_header(data, root)?;

        let heap_address = msgs
            .iter()
            .find(|m| m.kind == header::MSG_LINK_INFO)
            .map(|m| header::link_info_heap_address(m.body))
            .transpose()?
            .ok_or(Error::Unsupported(
                "root group without a link info message (old-style symbol table)",
            ))?;

        let links = heap::read_links(data, heap_address)?;
        let variables = links
            .into_iter()
            .map(|l| (l.name, l.object_address))
            .collect();

        Ok(Hdf5File { data, variables })
    }

    /// The names of every variable in the root group, in sorted order.
    pub fn variable_names(&self) -> impl Iterator<Item = &str> {
        self.variables.keys().map(String::as_str)
    }

    /// A variable's shape and element type.
    pub fn info(&self, name: &str) -> Result<VariableInfo, Error> {
        let (info, _) = self.describe(name)?;
        Ok(info)
    }

    fn describe(&self, name: &str) -> Result<(VariableInfo, Vec<u8>), Error> {
        let addr = *self
            .variables
            .get(name)
            .ok_or_else(|| Error::NoSuchVariable(name.to_owned()))?;
        let msgs = header::read_object_header(self.data, addr)?;

        // A filter pipeline with any filter in it means the stored bytes are
        // not the data. Refusing here is what stops this crate from returning
        // compressed bytes reinterpreted as floats.
        if let Some(m) = msgs.iter().find(|m| m.kind == header::MSG_FILTER_PIPELINE) {
            dataset::check_no_filters(m.body)?;
        }

        let space = msgs
            .iter()
            .find(|m| m.kind == header::MSG_DATASPACE)
            .ok_or(Error::Malformed("dataset without a dataspace message"))?;
        let space = dataset::parse_dataspace(space.body)?;

        let dtype = msgs
            .iter()
            .find(|m| m.kind == header::MSG_DATATYPE)
            .ok_or(Error::Malformed("dataset without a datatype message"))?;
        let dtype = dataset::parse_datatype(dtype.body)?;

        let layout = msgs
            .iter()
            .find(|m| m.kind == header::MSG_LAYOUT)
            .ok_or(Error::Malformed("dataset without a layout message"))?;
        let layout = dataset::parse_layout(layout.body)?;

        let info = VariableInfo {
            dims: space.dims,
            dtype,
        };
        let raw = self.read_raw(&info, &layout)?;
        Ok((info, raw))
    }

    /// Assembles a dataset's raw bytes in element order.
    fn read_raw(&self, info: &VariableInfo, layout: &Layout) -> Result<Vec<u8>, Error> {
        let elem = info.dtype.size();
        let count = usize::try_from(info.len()).map_err(|_| Error::Truncated)?;
        let total = count.checked_mul(elem).ok_or(Error::Truncated)?;

        match layout {
            Layout::Contiguous { address, size } => {
                let start = addr_to_index(*address, self.data.len())?;
                let size = usize::try_from(*size).map_err(|_| Error::Truncated)?;
                let take = size.min(total);
                let end = start.checked_add(take).ok_or(Error::Truncated)?;
                let src = self.data.get(start..end).ok_or(Error::Truncated)?;
                let mut out = vec![0u8; total];
                out[..src.len()].copy_from_slice(src);
                Ok(out)
            }
            Layout::Chunked {
                btree_address,
                chunk_dims,
            } => {
                if info.dims.len() != 1 {
                    return Err(Error::Unsupported("chunked dataset with rank other than 1"));
                }
                let chunk_len = usize::try_from(chunk_dims[0]).map_err(|_| Error::Truncated)?;
                if chunk_len == 0 {
                    return Err(Error::Malformed("zero-length chunk"));
                }

                let entries = dataset::read_chunk_btree(
                    self.data,
                    *btree_address,
                    chunk_dims.len(),
                )?;

                let mut out = vec![0u8; total];
                let mut covered = 0usize;
                for e in &entries {
                    let first = usize::try_from(e.offsets[0]).map_err(|_| Error::Truncated)?;
                    if first >= count {
                        // A chunk entirely past the current dimension size: the
                        // dataset is extendible and this is stale space.
                        continue;
                    }
                    let n = chunk_len.min(count - first);
                    let src_start = addr_to_index(e.address, self.data.len())?;
                    let want = n.checked_mul(elem).ok_or(Error::Truncated)?;
                    let avail = usize::try_from(e.size).map_err(|_| Error::Truncated)?;
                    if avail < want {
                        return Err(Error::Malformed("chunk shorter than its element count"));
                    }
                    let src_end = src_start.checked_add(want).ok_or(Error::Truncated)?;
                    let src = self.data.get(src_start..src_end).ok_or(Error::Truncated)?;
                    let dst_start = first.checked_mul(elem).ok_or(Error::Truncated)?;
                    let dst_end = dst_start.checked_add(want).ok_or(Error::Truncated)?;
                    out.get_mut(dst_start..dst_end)
                        .ok_or(Error::Truncated)?
                        .copy_from_slice(src);
                    covered += n;
                }

                // Without this, a dataset whose b-tree is missing a chunk would
                // come back padded with zeroes — which for a latitude reads as
                // a perfectly plausible point off the coast of Africa.
                if covered < count {
                    return Err(Error::IncompleteChunkCoverage {
                        missing: (count - covered) as u64,
                    });
                }
                Ok(out)
            }
        }
    }

    /// Reads a variable stored as IEEE 754 binary32.
    pub fn read_f32(&self, name: &str) -> Result<Vec<f32>, Error> {
        let (info, raw) = self.describe(name)?;
        if info.dtype != DataType::F32 {
            return Err(Error::TypeMismatch {
                actual: info.dtype,
                wanted: "f32",
            });
        }
        Ok(raw
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect())
    }

    /// Reads a variable stored as IEEE 754 binary64.
    pub fn read_f64(&self, name: &str) -> Result<Vec<f64>, Error> {
        let (info, raw) = self.describe(name)?;
        if info.dtype != DataType::F64 {
            return Err(Error::TypeMismatch {
                actual: info.dtype,
                wanted: "f64",
            });
        }
        Ok(raw
            .chunks_exact(8)
            .map(|b| f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            .collect())
    }

    /// Reads a variable stored as a 16-bit integer.
    ///
    /// GLM stores packed variables such as `flash_energy` and `flash_area` this
    /// way, tagged `_Unsigned = "true"`. The bits are returned as `i16` exactly
    /// as stored; reinterpreting them as unsigned and applying `scale_factor`
    /// and `add_offset` is CF unpacking, which belongs to the caller.
    pub fn read_i16(&self, name: &str) -> Result<Vec<i16>, Error> {
        let (info, raw) = self.describe(name)?;
        if !matches!(info.dtype, DataType::Int { bytes: 2, .. }) {
            return Err(Error::TypeMismatch {
                actual: info.dtype,
                wanted: "16-bit integer",
            });
        }
        Ok(raw
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect())
    }
}

/// Reads a version-2 superblock and returns the root group's header address.
fn read_superblock(data: &[u8]) -> Result<u64, Error> {
    const MAGIC: [u8; 8] = [0x89, b'H', b'D', b'F', 0x0d, 0x0a, 0x1a, 0x0a];
    if data.len() < 48 || data[..8] != MAGIC {
        return Err(Error::BadMagic);
    }
    let mut c = Cursor::new(data, 8);
    let version = c.u8()?;
    if version != 2 && version != 3 {
        return Err(Error::Unsupported("superblock version other than 2"));
    }
    let offset_size = c.u8()?;
    let length_size = c.u8()?;
    if offset_size != 8 || length_size != 8 {
        return Err(Error::Unsupported("offset or length size other than 8 bytes"));
    }
    let _flags = c.u8()?;
    let base = c.offset()?;
    if base != 0 {
        return Err(Error::Unsupported("non-zero base address"));
    }
    let _extension = c.offset()?;
    let eof = c.offset()?;
    if eof > data.len() as u64 {
        return Err(Error::Truncated);
    }
    let root = c.offset()?;
    Ok(root)
}
