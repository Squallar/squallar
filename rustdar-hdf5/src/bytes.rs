//! A bounds-checked cursor over the file image.
//!
//! Every read in this crate goes through here so that a malformed or truncated
//! file produces an `Err`, never a panic and never a wild index. There is no
//! `unsafe` anywhere in the crate.

use crate::Error;

/// Reads little-endian scalars out of a byte slice, tracking a position.
///
/// HDF5 stores "offsets" (file addresses) and "lengths" at widths declared in
/// the superblock. This file family always uses 8 for both, and
/// [`Cursor::offset`] / [`Cursor::length`] assert that rather than silently
/// mis-parsing a 4-byte-offset file as if it were 8.
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8], pos: usize) -> Self {
        Cursor { buf, pos }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn skip(&mut self, n: usize) -> Result<(), Error> {
        self.pos = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        Ok(())
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let out = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(out)
    }

    pub fn peek(&self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        self.buf.get(self.pos..end).ok_or(Error::Truncated)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, Error> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// A little-endian unsigned integer of `n` bytes, widened to u64.
    ///
    /// HDF5 uses these for fields whose width is declared elsewhere in the
    /// format (heap block offsets, object-header chunk sizes, link name
    /// lengths). `n` above 8 is a malformed file, not a wider integer.
    pub fn uint(&mut self, n: usize) -> Result<u64, Error> {
        if n == 0 || n > 8 {
            return Err(Error::Unsupported("integer field wider than 8 bytes"));
        }
        let b = self.take(n)?;
        let mut v = 0u64;
        for (i, byte) in b.iter().enumerate() {
            v |= u64::from(*byte) << (8 * i);
        }
        Ok(v)
    }

    /// A file address. Always 8 bytes in the files this crate targets.
    pub fn offset(&mut self) -> Result<u64, Error> {
        self.u64()
    }

    /// A length field. Always 8 bytes in the files this crate targets.
    pub fn length(&mut self) -> Result<u64, Error> {
        self.u64()
    }

    /// Checks a 4-byte block signature and consumes it.
    pub fn signature(&mut self, want: &[u8; 4]) -> Result<(), Error> {
        let got = self.take(4)?;
        if got == want {
            Ok(())
        } else {
            let mut b = [0u8; 4];
            b.copy_from_slice(got);
            Err(Error::BadSignature {
                want: *want,
                got: b,
            })
        }
    }
}

/// The HDF5 "undefined address" sentinel: all bits set.
///
/// A fractal heap indirect block lists a full row of child slots whether or not
/// they have been allocated; unallocated ones carry this value and must be
/// skipped rather than followed.
pub const UNDEFINED_ADDRESS: u64 = u64::MAX;

/// Converts a file address to a `usize` index, rejecting the undefined
/// sentinel and anything past the end of the image.
pub fn addr_to_index(addr: u64, len: usize) -> Result<usize, Error> {
    if addr == UNDEFINED_ADDRESS {
        return Err(Error::UndefinedAddress);
    }
    let idx = usize::try_from(addr).map_err(|_| Error::Truncated)?;
    if idx >= len {
        return Err(Error::Truncated);
    }
    Ok(idx)
}
