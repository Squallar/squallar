//! The crate's one bounds-checked cursor.
//!
//! Three payload types are read back off a message port here — a
//! [`crate::render_input::RenderInput`], the [`crate::nyquist::DeclaredNyquist`]
//! that rides beside every volume, and the [`crate::scan::DecodedScan`] a
//! decode job hands back — and all three are reading bytes they did not write.
//! The other end of the port can be a different build, so every accessor
//! answers `None` rather than panicking: a browser tab that panicked here would
//! take the whole page down where nobody would see it.
//!
//! One cursor rather than one per payload, because the three codecs are read
//! together and a `u32` that meant a different width in one of them would be a
//! silent misparse rather than a compile error.

/// A bounds-checked cursor over untrusted bytes.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    pub(crate) fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    pub(crate) fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    pub(crate) fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// `count` as a capacity, refused if the buffer cannot possibly hold that
    /// many items of `min_size` bytes each. Keeps a corrupt length from
    /// reserving gigabytes before the read fails.
    ///
    /// It matters more here than it reads: a decoded volume's wire form nests
    /// three counted lists — sweeps, then radials, then gates — so an
    /// unchecked count at the outer level would reserve against a length the
    /// inner levels have not been measured against yet.
    pub(crate) fn bounded(&self, count: u32, min_size: usize) -> Option<usize> {
        let count = count as usize;
        (count.checked_mul(min_size)? <= self.bytes.len() - self.at).then_some(count)
    }

    pub(crate) fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}
