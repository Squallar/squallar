//! Dataspace, datatype and data layout messages, plus the version-1 b-tree
//! that indexes chunked data.
//!
//! Note which b-tree this is. Group *links* are indexed by a version-2 b-tree,
//! which this crate sidesteps by walking the fractal heap. Chunked *data* in a
//! version-3 layout message is indexed by a version-1 b-tree, which is the old,
//! simple, self-describing node format and is implemented here in full.

use crate::bytes::{addr_to_index, Cursor, UNDEFINED_ADDRESS};
use crate::Error;

/// The element type of a dataset. Only what GLM's coordinate variables use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    /// IEEE 754 binary32, little-endian.
    F32,
    /// IEEE 754 binary64, little-endian.
    F64,
    /// Two's-complement integer, little-endian, with its size in bytes and
    /// whether it is signed. GLM stores packed variables such as
    /// `flash_energy` as 16-bit integers that CF unpacking later scales.
    Int { bytes: u8, signed: bool },
}

impl DataType {
    pub fn size(self) -> usize {
        match self {
            DataType::F32 => 4,
            DataType::F64 => 8,
            DataType::Int { bytes, .. } => usize::from(bytes),
        }
    }
}

/// Where a dataset's raw bytes live.
#[derive(Debug)]
pub enum Layout {
    Contiguous { address: u64, size: u64 },
    Chunked { btree_address: u64, chunk_dims: Vec<u32> },
}

pub struct Dataspace {
    pub dims: Vec<u64>,
}

/// Parses a Dataspace message (version 1 or 2) and returns the current dims.
///
/// The maximum dimensions are deliberately ignored: GLM declares its flash
/// dimensions unlimited, but only the current size describes the data present.
pub fn parse_dataspace(body: &[u8]) -> Result<Dataspace, Error> {
    let mut c = Cursor::new(body, 0);
    let version = c.u8()?;
    let rank = usize::from(c.u8()?);
    let _flags = c.u8()?;
    match version {
        1 => c.skip(5)?,  // reserved byte + 4 reserved bytes
        2 => c.skip(1)?,  // dataspace type
        _ => return Err(Error::Unsupported("dataspace message version")),
    }
    let mut dims = Vec::with_capacity(rank);
    for _ in 0..rank {
        dims.push(c.length()?);
    }
    Ok(Dataspace { dims })
}

/// Parses a version-1 Datatype message.
///
/// This validates the full numeric description rather than trusting the class
/// and size alone: a 4-byte float message that said big-endian, or that placed
/// the exponent somewhere other than bit 23, would not be an `f32` and must not
/// be read as one.
pub fn parse_datatype(body: &[u8]) -> Result<DataType, Error> {
    let mut c = Cursor::new(body, 0);
    let class_and_version = c.u8()?;
    let version = class_and_version >> 4;
    let class = class_and_version & 0x0f;
    if version != 1 && version != 3 {
        return Err(Error::Unsupported("datatype message version"));
    }
    let bitfield = c.take(3)?;
    let size = c.u32()?;

    let little_endian = bitfield[0] & 0x01 == 0;
    if !little_endian {
        return Err(Error::Unsupported("big-endian datatype"));
    }

    match class {
        0 => {
            // Fixed-point (integer). Bit 3 of the bitfield is the sign flag.
            let signed = bitfield[0] & 0x08 != 0;
            let bit_offset = c.u16()?;
            let precision = c.u16()?;
            if bit_offset != 0 || u32::from(precision) != size * 8 {
                return Err(Error::Unsupported("integer with padding or partial precision"));
            }
            let bytes = u8::try_from(size).map_err(|_| Error::Unsupported("integer size"))?;
            if !matches!(bytes, 1 | 2 | 4 | 8) {
                return Err(Error::Unsupported("integer size"));
            }
            Ok(DataType::Int { bytes, signed })
        }
        1 => {
            // Floating point. Check the layout matches IEEE 754 exactly.
            let bit_offset = c.u16()?;
            let precision = c.u16()?;
            let exponent_location = c.u8()?;
            let exponent_size = c.u8()?;
            let mantissa_location = c.u8()?;
            let mantissa_size = c.u8()?;
            let exponent_bias = c.u32()?;
            let sign_location = bitfield[1];

            let ieee32 = size == 4
                && precision == 32
                && exponent_location == 23
                && exponent_size == 8
                && mantissa_location == 0
                && mantissa_size == 23
                && exponent_bias == 127
                && sign_location == 31;
            let ieee64 = size == 8
                && precision == 64
                && exponent_location == 52
                && exponent_size == 11
                && mantissa_location == 0
                && mantissa_size == 52
                && exponent_bias == 1023
                && sign_location == 63;

            if bit_offset != 0 {
                return Err(Error::Unsupported("float with a bit offset"));
            }
            if ieee32 {
                Ok(DataType::F32)
            } else if ieee64 {
                Ok(DataType::F64)
            } else {
                Err(Error::Unsupported("float that is not IEEE 754 binary32/64"))
            }
        }
        _ => Err(Error::Unsupported("datatype class")),
    }
}

/// Rejects a Filter Pipeline message that declares any filter.
///
/// If a dataset is filtered, its stored bytes are compressed and/or shuffled
/// and are not the data. Reading them as values would produce numbers that
/// look plausible and are entirely wrong, so this is a hard refusal.
pub fn check_no_filters(body: &[u8]) -> Result<(), Error> {
    let mut c = Cursor::new(body, 0);
    let _version = c.u8()?;
    let nfilters = c.u8()?;
    if nfilters != 0 {
        return Err(Error::Unsupported("dataset with a filter pipeline"));
    }
    Ok(())
}

/// Parses a version-3 or version-4 Data Layout message.
pub fn parse_layout(body: &[u8]) -> Result<Layout, Error> {
    let mut c = Cursor::new(body, 0);
    let version = c.u8()?;
    if version != 3 {
        // Version 4 replaces the b-tree with one of five index types (single
        // chunk, implicit, fixed array, extensible array, b-tree v2). GLM files
        // written by the netCDF-4 library use version 3.
        return Err(Error::Unsupported("data layout message version other than 3"));
    }
    let class = c.u8()?;
    match class {
        1 => {
            let address = c.offset()?;
            let size = c.length()?;
            Ok(Layout::Contiguous { address, size })
        }
        2 => {
            // `dimensionality` counts the dataset rank plus one: the trailing
            // "dimension" is the element size in bytes.
            let dimensionality = usize::from(c.u8()?);
            if dimensionality < 2 {
                return Err(Error::Malformed("chunked layout dimensionality"));
            }
            let btree_address = c.offset()?;
            let mut chunk_dims = Vec::with_capacity(dimensionality);
            for _ in 0..dimensionality {
                chunk_dims.push(c.u32()?);
            }
            Ok(Layout::Chunked {
                btree_address,
                chunk_dims,
            })
        }
        0 => Err(Error::Unsupported("compact data layout")),
        _ => Err(Error::Malformed("data layout class")),
    }
}

/// One leaf entry of the chunk b-tree: where a chunk lives and how big it is.
#[derive(Debug)]
pub struct ChunkEntry {
    pub address: u64,
    pub size: u32,
    /// Offset of this chunk's first element in each dataset dimension. The
    /// trailing element-size component of the stored key is dropped.
    pub offsets: Vec<u64>,
}

/// Walks a version-1 b-tree of raw data chunks and returns every leaf entry.
///
/// Node layout: signature, node type, level, entries used, left and right
/// sibling addresses, then alternating keys and child pointers with one extra
/// trailing key. At level 0 the children are chunk addresses; above that they
/// are child node addresses.
pub fn read_chunk_btree(
    file: &[u8],
    address: u64,
    dimensionality: usize,
) -> Result<Vec<ChunkEntry>, Error> {
    let mut out = Vec::new();
    read_chunk_btree_node(file, address, dimensionality, 0, &mut out)?;
    Ok(out)
}

const MAX_BTREE_DEPTH: u32 = 32;

fn read_chunk_btree_node(
    file: &[u8],
    address: u64,
    dimensionality: usize,
    depth: u32,
    out: &mut Vec<ChunkEntry>,
) -> Result<(), Error> {
    if depth > MAX_BTREE_DEPTH {
        return Err(Error::Malformed("chunk b-tree nested too deeply"));
    }
    let start = addr_to_index(address, file.len())?;
    let mut c = Cursor::new(file, start);
    c.signature(b"TREE")?;
    let node_type = c.u8()?;
    if node_type != 1 {
        return Err(Error::Unsupported("b-tree node type other than raw data chunks"));
    }
    let level = c.u8()?;
    let entries = usize::from(c.u16()?);
    c.skip(8)?; // left sibling
    c.skip(8)?; // right sibling

    for _ in 0..entries {
        let size = c.u32()?;
        // Filter mask: which pipeline filters were skipped for this chunk.
        // This crate rejects filtered datasets outright, so it carries no
        // information here.
        let _filter_mask = c.u32()?;
        let mut offsets = Vec::with_capacity(dimensionality);
        for _ in 0..dimensionality {
            offsets.push(c.u64()?);
        }
        // Drop the trailing element-size component so `offsets` indexes the
        // dataset's own dimensions.
        offsets.pop();
        let child = c.offset()?;
        if level == 0 {
            if child != UNDEFINED_ADDRESS {
                out.push(ChunkEntry {
                    address: child,
                    size,
                    offsets,
                });
            }
        } else {
            read_chunk_btree_node(file, child, dimensionality, depth + 1, out)?;
        }
    }
    Ok(())
}
