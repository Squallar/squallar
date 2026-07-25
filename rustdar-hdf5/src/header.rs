//! Object headers (version 2) and the handful of header messages this crate
//! needs.
//!
//! Every object header in a GOES GLM L2 file is version 2 — they all carry the
//! `OHDR` signature, which version 1 headers do not have at all. So only v2 is
//! implemented, and a v1 header is reported as unsupported rather than
//! misparsed.

use crate::bytes::{addr_to_index, Cursor};
use crate::Error;

pub const MSG_DATASPACE: u8 = 1;
pub const MSG_LINK_INFO: u8 = 2;
pub const MSG_DATATYPE: u8 = 3;
pub const MSG_LAYOUT: u8 = 8;
pub const MSG_FILTER_PIPELINE: u8 = 11;
pub const MSG_CONTINUATION: u8 = 16;

/// One header message: its type and its raw body.
pub struct Message<'a> {
    pub kind: u8,
    pub body: &'a [u8],
}

/// Parses an object header at `addr`, following continuation blocks, and
/// returns every message in it.
///
/// Continuation blocks (`OCHK`) hold the messages that did not fit in the
/// header's first chunk. GLM puts most dataset attributes there, and — more
/// importantly for us — a continuation is threaded into the *middle* of the
/// message list, so it has to be followed to see the whole object.
pub fn read_object_header(file: &[u8], addr: u64) -> Result<Vec<Message<'_>>, Error> {
    let start = addr_to_index(addr, file.len())?;
    let mut c = Cursor::new(file, start);

    // A version-1 object header has no signature; its first byte is the
    // version number. Detect that and say so rather than reporting a confusing
    // signature mismatch.
    if c.peek(4)? != b"OHDR" {
        if c.peek(1)?[0] == 1 {
            return Err(Error::Unsupported("object header version 1"));
        }
        c.signature(b"OHDR")?;
        unreachable!("signature() returns Err when the bytes do not match");
    }
    c.signature(b"OHDR")?;

    let version = c.u8()?;
    if version != 2 {
        return Err(Error::Unsupported("object header version other than 2"));
    }
    let flags = c.u8()?;

    if flags & 0x20 != 0 {
        c.skip(16)?; // access / modification / change / birth times
    }
    if flags & 0x10 != 0 {
        c.skip(4)?; // max compact + min dense attribute counts
    }

    let size_width = 1usize << (flags & 0x03);
    let chunk0_size = c.uint(size_width)?;
    let chunk0_size = usize::try_from(chunk0_size).map_err(|_| Error::Truncated)?;

    // Bit 2 of the header flags means every message in this object — including
    // ones in continuation blocks — carries a 2-byte creation order after its
    // flags byte. Getting this wrong shifts every subsequent message by two
    // bytes, so it is threaded through to the continuation reader.
    let track_order = flags & 0x04 != 0;

    let msgs_start = c.pos();
    let msgs_end = msgs_start.checked_add(chunk0_size).ok_or(Error::Truncated)?;

    let mut out = Vec::new();
    read_messages(file, msgs_start, msgs_end, track_order, 0, &mut out)?;
    Ok(out)
}

/// Maximum continuation-block depth. Real files nest one level; a cycle in a
/// corrupt file would otherwise recurse forever.
const MAX_CONTINUATION_DEPTH: u32 = 16;

fn read_messages<'a>(
    file: &'a [u8],
    start: usize,
    end: usize,
    track_order: bool,
    depth: u32,
    out: &mut Vec<Message<'a>>,
) -> Result<(), Error> {
    if depth > MAX_CONTINUATION_DEPTH {
        return Err(Error::Unsupported("continuation blocks nested too deeply"));
    }
    if end > file.len() {
        return Err(Error::Truncated);
    }
    let mut c = Cursor::new(file, start);

    // A message header is 4 bytes (+2 when creation order is tracked), so once
    // fewer than that remain the rest of the chunk is padding or the checksum.
    let stride = if track_order { 6 } else { 4 };
    while c.pos() + stride <= end {
        let kind = c.u8()?;
        let size = usize::from(c.u16()?);
        let _msg_flags = c.u8()?;
        if track_order {
            c.skip(2)?;
        }
        if c.pos() + size > end {
            break;
        }
        let body = c.take(size)?;

        if kind == MSG_CONTINUATION {
            let mut cc = Cursor::new(body, 0);
            let caddr = cc.offset()?;
            let clen = cc.length()?;
            let cstart = addr_to_index(caddr, file.len())?;
            let clen = usize::try_from(clen).map_err(|_| Error::Truncated)?;
            let cend = cstart.checked_add(clen).ok_or(Error::Truncated)?;
            if cend > file.len() {
                return Err(Error::Truncated);
            }
            let mut sig = Cursor::new(file, cstart);
            sig.signature(b"OCHK")?;
            // The block ends with a 4-byte checksum, which is not a message.
            let body_end = cend.checked_sub(4).ok_or(Error::Truncated)?;
            read_messages(file, sig.pos(), body_end, track_order, depth + 1, out)?;
        } else {
            out.push(Message { kind, body });
        }
    }
    Ok(())
}

/// The address of a group's fractal heap of links, from a Link Info message.
///
/// Version 0 layout: version, flags, then optionally the maximum creation
/// index (flags bit 0), then the fractal heap address, then the name-index
/// b-tree address, then optionally the creation-order index (flags bit 1).
///
/// Only the fractal heap address is returned: this crate enumerates links by
/// walking the heap directly, so neither b-tree is needed.
pub fn link_info_heap_address(body: &[u8]) -> Result<u64, Error> {
    let mut c = Cursor::new(body, 0);
    let version = c.u8()?;
    if version != 0 {
        return Err(Error::Unsupported("link info message version other than 0"));
    }
    let flags = c.u8()?;
    if flags & 0x01 != 0 {
        c.skip(8)?; // maximum creation index
    }
    let heap = c.offset()?;
    Ok(heap)
}
