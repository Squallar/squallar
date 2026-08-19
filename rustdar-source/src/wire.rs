//! The substrate's one bounds-checked cursor.
//!
//! Three payload types are read back off a message port with it — a
//! `RenderInput`, the `DeclaredNyquist` that rides beside every volume, and
//! the `DecodedScan` a decode job hands back, all defined in `rustdar-radar` —
//! and all three are reading bytes they did not write. The other end of the
//! port can be a different build, so every accessor answers `None` rather than
//! panicking: a browser tab that panicked here would take the whole page down
//! where nobody would see it.
//!
//! One cursor rather than one per payload, because the three codecs are read
//! together and a `u32` that meant a different width in one of them would be a
//! silent misparse rather than a compile error.

/// A bounds-checked cursor over untrusted bytes.
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    /// Everything not yet consumed — the variable-length tail a payload ends
    /// with, so no length prefix can lie about it. Infallible where every
    /// other accessor answers `Option`, because an empty tail is a legitimate
    /// value rather than a misread, and the cursor never sits past the end of
    /// the buffer, so the slice below cannot panic.
    ///
    /// Semantics match the duplicate cursor in `rustdar_frontend::offload`
    /// (which lives beside this one until WO-M7.2): its first consumer here
    /// is the overlay reply decoder's raw RGBA tail (WO-M6.2), then the
    /// radar decode arms (WO-M7.1/M7.2).
    pub fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    pub fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    pub fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    pub fn i64(&mut self) -> Option<i64> {
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
    pub fn bounded(&self, count: u32, min_size: usize) -> Option<usize> {
        let count = count as usize;
        (count.checked_mul(min_size)? <= self.bytes.len() - self.at).then_some(count)
    }

    pub fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}

/// A digest of an encoded payload, for the
/// `the_wire_layout_is_the_one_this_version_ships` tests.
///
/// FNV-1a 64, six lines and no dependency, because nothing here needs a
/// cryptographic hash — the adversary is a developer who moved a field, not
/// one searching for a collision.
///
/// **What this is for.** Each of the three codecs pins its version with
/// `assert_eq!(FORMAT_VERSION, N)`, which is our value compared against our
/// value: it fails for exactly one person, the one who *raises* the number,
/// and is silent for the one who changes a shape and does not. That is the
/// wrong way round — raising it is the safe act, and forgetting to is what
/// ships a page and a worker that misread each other. A digest over the bytes
/// an encoder actually produced fails for the second developer, which is the
/// one the number exists to catch.
///
/// **Why the fixtures it is used on are built from literals.** A digest is
/// only a guard if it never fires for a reason other than the layout. Every
/// grid, section and tilt `rustdar-radar` builds for its other tests goes
/// through beam geometry, and `sin`/`cos`/`atan2` are the platform's libm
/// rather than anything IEEE 754 pins — so a digest of one of those would be
/// a digest of whichever libm ran it, and would go red on a target nobody
/// changed. The `layout_fixture` beside each of these tests is therefore
/// assembled by hand from exactly-representable numbers, and carries no value
/// that anything computes.
///
/// **Why it is not `#[cfg(test)]`.** In `rustdar-radar` it was, because a
/// crate's own tests see its `cfg(test)` items. A dependent crate's tests do
/// not — `cfg(test)` is per-crate, so gating it here would erase it from
/// every digest suite that reaches it through a re-export. It is ordinary
/// code now: eight lines, no dependency, dead in any build that does not
/// call it.
pub fn layout_digest(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_is_the_unconsumed_tail_and_empty_at_the_end() {
        let bytes = [1u8, 2, 3, 4, 5];
        let mut r = Reader::new(&bytes);
        assert_eq!(
            r.take(2),
            Some(&[1u8, 2][..]),
            "the control: take consumes from the front",
        );
        assert_eq!(
            r.rest(),
            &[3u8, 4, 5][..],
            "rest is exactly what take has not consumed",
        );
        // `rest` peeks rather than consumes: the cursor has not moved, so a
        // second read answers the same tail and `take` still starts at 3.
        assert_eq!(r.rest(), &[3u8, 4, 5][..]);
        assert_eq!(r.take(3), Some(&[3u8, 4, 5][..]));
        assert_eq!(
            r.rest(),
            &[] as &[u8],
            "at the end the tail is empty, not a panic and not None — an \
             empty tail is a legitimate value on this wire",
        );
        assert!(r.at_end());
    }
}
