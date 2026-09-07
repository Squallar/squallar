//! [`has_ink`](super::has_ink) against the byte scan it replaced.
//!
//! A faster scan and a scan that returns the WRONG ANSWER both get quicker, and
//! the two are not distinguishable by a timing. The answer decides whether a
//! picture-sized payload crosses the wire at all
//! (`RasterizeOutput::settle_blank`), so a blank wrongly called inked spends
//! 40 MiB on nothing and an inked picture wrongly called blank clears a pane
//! that should be painted. Every test here is therefore differential: the same
//! bytes through both spellings, asserted equal.
//!
//! The word scan reads a `u64` at a time off whatever alignment the buffer
//! landed on, so the cases that can separate the two are **alignment** (the
//! head bytes before the first aligned word), **length** (the tail bytes after
//! the last), and **where the ink sits** relative to both. All three are
//! crossed here rather than sampled.

use super::has_ink;

/// The spelling `has_ink` replaced, kept as the oracle. Byte at a time, no
/// alignment to reason about, obviously correct.
fn has_ink_bytewise(rgba: &[u8]) -> bool {
    rgba.iter().any(|&b| b != 0)
}

/// Every offset into a `u64` — one of these makes any buffer's first aligned
/// word start at each possible distance in.
const OFFSETS: [usize; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];

/// Lengths either side of the word boundary, including the ones with no whole
/// word in them at all and the ones that are all head or all tail.
const LENS: [usize; 20] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 15, 16, 17, 23, 24, 25, 31, 32, 33, 64,
];

/// A buffer big enough to carve every (offset, len) pair out of, so the
/// subslices genuinely differ in alignment rather than all being fresh
/// allocations that happen to land the same way.
fn arena(fill: u8) -> Vec<u8> {
    vec![fill; OFFSETS.len() + LENS[LENS.len() - 1] + 8]
}

#[test]
fn agrees_with_the_byte_scan_on_every_alignment_and_length() {
    for &off in &OFFSETS {
        for &len in &LENS {
            // Blank, and then the same window with ink at each position in it.
            // `None` is the no-ink case; `Some(i)` puts the single non-zero
            // byte at `i`, which covers the first byte, the last byte, and
            // every interior one — so head, whole-word and tail ink are all
            // reached without naming which is which.
            for ink_at in std::iter::once(None).chain((0..len).map(Some)) {
                let mut buf = arena(0);
                if let Some(i) = ink_at {
                    buf[off + i] = 1;
                }
                let window = &buf[off..off + len];
                assert_eq!(
                    has_ink(window),
                    has_ink_bytewise(window),
                    "the word scan and the byte scan disagreed at offset {off}, \
                     length {len}, ink at {ink_at:?}",
                );
            }
        }
    }
}

#[test]
fn a_fully_blank_buffer_of_every_shape_reports_no_ink() {
    // The population the scan exists to serve: `pictures - inked`. If this
    // regressed to `true` every blank would keep its buffer and the wire
    // elision would be dead, silently.
    for &off in &OFFSETS {
        for &len in &LENS {
            let buf = arena(0);
            let window = &buf[off..off + len];
            assert!(
                !has_ink(window),
                "an all-zero buffer at offset {off}, length {len} reported ink",
            );
        }
    }
}

#[test]
fn ink_in_the_last_byte_is_found_at_every_alignment() {
    // The tail is the half a word scan cannot cover, and the last byte is the
    // one a scan that rounded the length down would drop. A stuck-`false`
    // `has_ink` would clear panes that should be painted.
    for &off in &OFFSETS {
        for &len in &LENS {
            if len == 0 {
                continue;
            }
            let mut buf = arena(0);
            buf[off + len - 1] = 255;
            let window = &buf[off..off + len];
            assert!(
                has_ink(window),
                "ink in the last byte was missed at offset {off}, length {len}",
            );
        }
    }
}

#[test]
fn every_single_nonzero_byte_value_is_ink() {
    // "Non-zero" is the whole definition; a word scan that compared against
    // some mask rather than zero would pass the 1-and-255 cases above and fail
    // in between.
    for value in 1..=u8::MAX {
        for &off in &OFFSETS {
            let mut buf = arena(0);
            buf[off + 9] = value;
            let window = &buf[off..off + 24];
            assert!(
                has_ink(window),
                "byte value {value} at offset {off} was not counted as ink",
            );
        }
    }
}

#[test]
fn a_picture_sized_blank_agrees_with_the_byte_scan() {
    // The real shape: one 4317x2477 overlay picture's worth of premultiplied
    // zeroes, which is the case that pays the whole pass and the case the
    // change was made for. Held to the oracle at full size, not just on the
    // small windows above.
    let blank = vec![0u8; 4317 * 2477 * 4];
    assert_eq!(has_ink(&blank), has_ink_bytewise(&blank));
    assert!(!has_ink(&blank), "a picture-sized blank reported ink");

    // And the same buffer with its very last byte set, which is the worst case
    // for a scan that gets the tail wrong.
    let mut inked = blank;
    *inked.last_mut().expect("the buffer is not empty") = 1;
    assert_eq!(has_ink(&inked), has_ink_bytewise(&inked));
    assert!(
        has_ink(&inked),
        "a picture with one inked byte reported blank"
    );
}
