//! Fractal heap traversal, used to enumerate the links of a group.
//!
//! A NetCDF-4 root group with more than a handful of members uses *dense* link
//! storage: the links live as objects in a fractal heap, and two version-2
//! b-trees index them by name hash and by creation order. Looking a name up
//! through the name b-tree means implementing b-tree v2 nodes *and* the Jenkins
//! lookup3 hash.
//!
//! This crate does not do that. Because the caller wants the whole variable
//! list anyway, it walks the heap's direct blocks and parses the link messages
//! out of them in storage order. That yields exactly the same set of links
//! while skipping both b-trees entirely — the single biggest simplification in
//! this crate.

use crate::bytes::{addr_to_index, Cursor, UNDEFINED_ADDRESS};
use crate::Error;

/// A hard link from a group to an object: a name and that object's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub name: String,
    pub object_address: u64,
}

struct HeapHeader {
    /// Bit 1 of the heap flags: direct blocks carry a checksum after their
    /// prefix, which shifts where object data starts.
    checksummed_blocks: bool,
    table_width: u32,
    starting_block_size: u64,
    max_direct_block_size: u64,
    /// Width in bytes of a block-offset field, derived from the maximum heap
    /// size (which the format states in *bits*).
    block_offset_bytes: usize,
    root_address: u64,
    current_rows: u32,
    managed_objects: u64,
}

fn read_heap_header(file: &[u8], addr: u64) -> Result<HeapHeader, Error> {
    let start = addr_to_index(addr, file.len())?;
    let mut c = Cursor::new(file, start);
    c.signature(b"FRHP")?;
    let version = c.u8()?;
    if version != 0 {
        return Err(Error::Unsupported("fractal heap version other than 0"));
    }
    let _heap_id_length = c.u16()?;
    let io_filter_length = c.u16()?;
    if io_filter_length != 0 {
        // A filtered heap would need the filter pipeline applied to each direct
        // block before any link could be read. GLM files do not use one.
        return Err(Error::Unsupported("fractal heap with I/O filters"));
    }
    let flags = c.u8()?;
    let _max_managed_size = c.u32()?;

    c.skip(8)?; // next huge object id
    c.skip(8)?; // huge object b-tree address
    c.skip(8)?; // free space in managed blocks
    c.skip(8)?; // free space manager address
    c.skip(8)?; // managed space in heap
    c.skip(8)?; // allocated managed space
    c.skip(8)?; // direct block iterator offset
    let managed_objects = c.length()?;
    c.skip(8)?; // size of huge objects
    let huge_objects = c.length()?;
    c.skip(8)?; // size of tiny objects
    let tiny_objects = c.length()?;

    if huge_objects != 0 || tiny_objects != 0 {
        // Huge objects live outside the heap blocks and tiny ones are packed
        // into heap IDs; either would make a straight block walk incomplete, so
        // refuse rather than silently return a short link list.
        return Err(Error::Unsupported("fractal heap with huge or tiny objects"));
    }

    let table_width = u32::from(c.u16()?);
    let starting_block_size = c.length()?;
    let max_direct_block_size = c.length()?;
    let max_heap_size_bits = c.u16()?;
    let _starting_rows = c.u16()?;
    let root_address = c.offset()?;
    let current_rows = u32::from(c.u16()?);

    if table_width == 0 || starting_block_size == 0 {
        return Err(Error::Malformed("fractal heap with zero table width or block size"));
    }

    let block_offset_bytes = usize::from(max_heap_size_bits).div_ceil(8);

    Ok(HeapHeader {
        checksummed_blocks: flags & 0x02 != 0,
        table_width,
        starting_block_size,
        max_direct_block_size,
        block_offset_bytes,
        root_address,
        current_rows,
        managed_objects,
    })
}

/// The size of a direct block in row `row` of the doubling table.
///
/// Rows 0 and 1 both hold blocks of the starting size; each row after that
/// doubles. Getting this wrong reads past the end of one block and into the
/// next, so it is the arithmetic worth being careful about.
fn row_block_size(starting: u64, row: u32) -> u64 {
    if row < 2 {
        starting
    } else {
        starting.saturating_mul(1u64 << (row - 1))
    }
}

/// Number of doubling-table rows that hold direct (rather than indirect)
/// blocks: `log2(max_direct) - log2(starting) + 2`.
fn max_direct_rows(starting: u64, max_direct: u64) -> u32 {
    let s = starting.trailing_zeros();
    let m = max_direct.trailing_zeros();
    m.saturating_sub(s) + 2
}

/// Reads every hard link stored in the group whose Link Info message points at
/// the fractal heap at `heap_address`.
pub fn read_links(file: &[u8], heap_address: u64) -> Result<Vec<Link>, Error> {
    let h = read_heap_header(file, heap_address)?;
    if h.root_address == UNDEFINED_ADDRESS {
        return Ok(Vec::new());
    }

    let mut links = Vec::new();
    if h.current_rows == 0 {
        // The root block is a lone direct block; the heap never grew enough to
        // need an indirect block above it.
        read_direct_block(file, &h, h.root_address, h.starting_block_size, &mut links)?;
    } else {
        read_indirect_block(file, &h, h.root_address, h.current_rows, &mut links)?;
    }

    // The heap header states how many objects it holds. Every managed object in
    // a group's link heap is one link, so a mismatch means the block walk
    // dropped something (a free-space gap, an unhandled block type) and the
    // caller would otherwise get a silently short variable list.
    let found = u64::try_from(links.len()).unwrap_or(u64::MAX);
    if found != h.managed_objects {
        return Err(Error::LinkCountMismatch {
            expected: h.managed_objects,
            found,
        });
    }
    Ok(links)
}

fn read_indirect_block(
    file: &[u8],
    h: &HeapHeader,
    addr: u64,
    rows: u32,
    out: &mut Vec<Link>,
) -> Result<(), Error> {
    let start = addr_to_index(addr, file.len())?;
    let mut c = Cursor::new(file, start);
    c.signature(b"FHIB")?;
    let version = c.u8()?;
    if version != 0 {
        return Err(Error::Unsupported("fractal heap indirect block version"));
    }
    c.skip(8)?; // heap header address
    c.skip(h.block_offset_bytes)?; // block offset

    let direct_rows = rows.min(max_direct_rows(h.starting_block_size, h.max_direct_block_size));

    for slot in 0..direct_rows.saturating_mul(h.table_width) {
        let child = c.offset()?;
        if child == UNDEFINED_ADDRESS {
            // Unallocated slot: the doubling table always lists a full row.
            continue;
        }
        let row = slot / h.table_width;
        let size = row_block_size(h.starting_block_size, row);
        read_direct_block(file, h, child, size, out)?;
    }

    // Rows beyond the direct rows hold child indirect blocks. GLM heaps are far
    // too small to reach one, so this is refused rather than guessed at.
    if rows > direct_rows {
        return Err(Error::Unsupported("nested fractal heap indirect blocks"));
    }
    Ok(())
}

fn read_direct_block(
    file: &[u8],
    h: &HeapHeader,
    addr: u64,
    size: u64,
    out: &mut Vec<Link>,
) -> Result<(), Error> {
    let start = addr_to_index(addr, file.len())?;
    let size = usize::try_from(size).map_err(|_| Error::Truncated)?;
    let end = start.checked_add(size).ok_or(Error::Truncated)?;
    if end > file.len() {
        return Err(Error::Truncated);
    }

    let mut c = Cursor::new(file, start);
    c.signature(b"FHDB")?;
    let version = c.u8()?;
    if version != 0 {
        return Err(Error::Unsupported("fractal heap direct block version"));
    }
    c.skip(8)?; // heap header address
    c.skip(h.block_offset_bytes)?;
    if h.checksummed_blocks {
        c.skip(4)?;
    }

    // Objects are packed from the start of the data area; free space is at the
    // end and reads as zeroes, which fails the version check below and ends the
    // loop cleanly.
    while c.pos() < end {
        match parse_link(&mut c, end) {
            Ok(link) => out.push(link),
            Err(_) => break,
        }
    }
    Ok(())
}

/// Parses one Link message out of the heap's object area.
///
/// Field order is fixed by the format: version, flags, then the optional link
/// type, creation order and character set in that order, then the name length,
/// the name, and finally the link information.
fn parse_link(c: &mut Cursor<'_>, end: usize) -> Result<Link, Error> {
    let version = c.u8()?;
    if version != 1 {
        return Err(Error::Malformed("link message version"));
    }
    let flags = c.u8()?;

    let mut link_type = 0u8;
    if flags & 0x08 != 0 {
        link_type = c.u8()?;
    }
    if flags & 0x04 != 0 {
        c.skip(8)?; // creation order
    }
    if flags & 0x10 != 0 {
        c.skip(1)?; // name character set
    }

    let name_len_width = 1usize << (flags & 0x03);
    let name_len = c.uint(name_len_width)?;
    let name_len = usize::try_from(name_len).map_err(|_| Error::Truncated)?;
    if name_len == 0 || c.pos() + name_len > end {
        return Err(Error::Malformed("link name length"));
    }
    let raw = c.take(name_len)?;
    let name = core::str::from_utf8(raw)
        .map_err(|_| Error::Malformed("link name is not valid UTF-8"))?
        .to_owned();

    if link_type != 0 {
        // Soft and external links carry a target path, not an address.
        return Err(Error::Unsupported("soft or external link"));
    }
    let object_address = c.offset()?;
    Ok(Link {
        name,
        object_address,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Row sizing is the arithmetic that decides how far a direct block
    /// extends. Rows 0 and 1 share the starting size and every later row
    /// doubles; an off-by-one here reads one block's trailer as the next
    /// block's links.
    ///
    /// The expected values come from the HDF5 format specification's
    /// description of the doubling table, not from this crate's own output.
    #[test]
    fn doubling_table_rows_0_and_1_share_the_starting_size() {
        assert_eq!(row_block_size(512, 0), 512);
        assert_eq!(row_block_size(512, 1), 512);
        assert_eq!(row_block_size(512, 2), 1024);
        assert_eq!(row_block_size(512, 3), 2048);
        assert_eq!(row_block_size(512, 4), 4096);
    }

    /// `log2(65536) - log2(512) + 2 == 9`: with a 512-byte starting block and a
    /// 64 KiB maximum direct block, rows 0..8 hold direct blocks.
    #[test]
    fn direct_row_count_matches_the_specified_formula() {
        assert_eq!(max_direct_rows(512, 65536), 9);
        assert_eq!(max_direct_rows(512, 512), 2);
        assert_eq!(max_direct_rows(1024, 65536), 8);
    }
}
