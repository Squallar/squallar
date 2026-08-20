//! The substrate's one bounds-checked cursor.
//!
//! The payloads read back off a message port were written by the other end,
//! which can be a different build, so every accessor answers `None` rather
//! than panicking: a browser tab that panicked here would take the page down.
//! One cursor rather than one per payload, because a `u32` that meant a
//! different width in one codec would be a silent misparse.

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
    /// with, so no length prefix can lie about it. Infallible because an empty
    /// tail is a legitimate value and the cursor never sits past the end.
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
    /// many items of `min_size` bytes each: it keeps a corrupt length from
    /// reserving gigabytes. A decoded volume nests three counted lists —
    /// sweeps, radials, gates — so the outer count must be checked.
    pub fn bounded(&self, count: u32, min_size: usize) -> Option<usize> {
        let count = count as usize;
        (count.checked_mul(min_size)? <= self.bytes.len() - self.at).then_some(count)
    }

    pub fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}

/// A digest of an encoded payload, for the
/// `the_wire_layout_is_the_one_this_version_ships` tests. FNV-1a 64, because
/// the adversary is a developer who moved a field, not one hunting collisions.
///
/// An `assert_eq!(FORMAT_VERSION, N)` fails only for the person who *raises*
/// the number, and is silent for the one who changes a shape and does not; a
/// digest over the bytes an encoder produced fails for the second.
///
/// Its fixtures are built from literals so it never fires for a reason other
/// than the layout: anything through beam geometry would be a digest of
/// whichever libm ran `sin`/`cos`/`atan2`. Not `#[cfg(test)]`, because
/// `cfg(test)` is per-crate and gating it would erase it from dependents.
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
