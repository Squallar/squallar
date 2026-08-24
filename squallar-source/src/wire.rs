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

    /// LEB128, read through [`Reader::u8`] so it inherits the bounds check and
    /// the `None`-on-short-buffer policy.
    ///
    /// Capped at ten groups because a `u64` cannot need more: an unterminated
    /// run of continuation bytes would otherwise walk to the end of the buffer
    /// and answer with whatever the low bits happened to be. The tenth group
    /// contributes only bit 63, so a value with junk in its upper bits is
    /// truncated rather than refused — every caller here is a length or a
    /// delta, and every length is bounded again through [`Reader::bounded`].
    pub fn uvarint(&mut self) -> Option<u64> {
        let mut value: u64 = 0;
        for group in 0..10 {
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7F) << (7 * group);
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    /// A zigzag-coded signed varint: `n` is stored as `(n << 1) ^ (n >> 63)`,
    /// so a small negative delta costs one byte rather than ten.
    pub fn zigzag(&mut self) -> Option<i64> {
        let raw = self.uvarint()?;
        Some(((raw >> 1) as i64) ^ -((raw & 1) as i64))
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

    /// LEB128 written out by hand, so the test does not check the reader
    /// against itself.
    fn leb128(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
        out
    }

    #[test]
    fn uvarint_reads_every_group_width_and_leaves_the_cursor_after_it() {
        // One value per group count, plus the boundaries either side of each.
        let mut cases: Vec<u64> = vec![0, 1, 0x7F, 0x80, 0x3FFF, 0x4000, u64::MAX];
        for shift in 1..64 {
            cases.push(1u64 << shift);
        }

        for want in cases {
            let bytes = leb128(want);
            // A sentinel behind the varint: a reader that consumed one byte too
            // many or too few would take the wrong value here.
            let mut buf = bytes.clone();
            buf.push(0xA5);
            let mut r = Reader::new(&buf);
            assert_eq!(r.uvarint(), Some(want), "{want} as {bytes:?}");
            assert_eq!(
                r.u8(),
                Some(0xA5),
                "{want}: the cursor did not stop at the end of the varint",
            );
            assert!(r.at_end());
        }
    }

    #[test]
    fn zigzag_round_trips_both_signs_and_costs_one_byte_near_zero() {
        let put = |v: i64| leb128(((v << 1) ^ (v >> 63)) as u64);

        for want in [0i64, -1, 1, -63, 63, -64, 64, i64::MIN, i64::MAX] {
            let mut buf = put(want);
            buf.push(0xA5);
            let mut r = Reader::new(&buf);
            assert_eq!(r.zigzag(), Some(want), "{want}");
            assert_eq!(r.u8(), Some(0xA5), "{want}: the cursor overran");
        }

        // The reason the coding exists at all: a delta of -1 must not cost the
        // ten bytes a two's-complement -1 would.
        assert_eq!(put(-1).len(), 1, "a small negative delta is one byte");
        assert_eq!(put(1).len(), 1);
    }

    #[test]
    fn an_unterminated_varint_stops_rather_than_walking_the_buffer() {
        // Every byte a continuation: there is no terminator to find.
        let runaway = vec![0xFFu8; 4096];
        let mut r = Reader::new(&runaway);
        assert_eq!(
            r.uvarint(),
            None,
            "a run of continuation bytes must be refused, not truncated into a value",
        );
        assert_eq!(
            r.rest().len(),
            4096 - 10,
            "and it must give up after ten groups, not at the end of the buffer",
        );

        // Truncated mid-varint: the continuation bit promises a byte the buffer
        // does not have.
        let mut r = Reader::new(&[0x80u8]);
        assert_eq!(r.uvarint(), None);
        let mut r = Reader::new(&[] as &[u8]);
        assert_eq!(r.uvarint(), None);
        assert_eq!(r.zigzag(), None);
    }

    /// A corrupt length is what `bounded` exists for, and a varint length is
    /// the cheapest possible lie: three bytes can ask for two million items.
    #[test]
    fn a_varint_length_still_has_to_pass_bounded() {
        let mut buf = leb128(2_000_000);
        buf.extend_from_slice(&[0u8; 8]);
        let mut r = Reader::new(&buf);
        let count = r.uvarint().expect("the length itself reads");
        assert_eq!(count, 2_000_000);
        assert_eq!(
            r.bounded(count as u32, 2),
            None,
            "eight bytes cannot hold two million two-byte items",
        );
        assert_eq!(r.bounded(4, 2), Some(4), "the control: four of them fit");
    }
}
