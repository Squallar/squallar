//! VENDORED — this file is not upstream's. It is the pin on the one behaviour
//! this copy of the crate exists to change.
//!
//! Upstream declared its backend seam as
//! `fn read(&self, offset: usize, length: usize)`. `usize` is **32 bits on
//! wasm32**, so every archive offset above 4 GiB was silently truncated mod
//! 2^32 and the reader fetched bytes from the wrong place. Upstream's own
//! `// FIXME` at the top of `async_reader.rs` said as much; what it did not
//! say is that there is no crash, just wrong bytes. See VENDORED.md for the
//! measured outage.
//!
//! # Why this file is in the vendored crate rather than beside its caller
//!
//! Because of what it can be run on. `usize` is 64 bits on the machine that
//! runs `cargo test`, so a test of this asserted on the host proves nothing:
//! it passes whether or not the truncation is present. That is exactly the
//! shape of check that let the defect ship in the first place.
//!
//! This crate, unlike the workspace crate that consumes it, builds and runs on
//! **`i686-unknown-linux-gnu`** — a real 32-bit target, where `usize` is 32
//! bits and `as usize` really does truncate. So:
//!
//! ```text
//! cargo test -p pmtiles --target i686-unknown-linux-gnu
//! ```
//!
//! is a genuine execution of the failure mode, and
//! [`a_leaf_directory_above_four_gibibytes_is_not_truncated`] fails on that
//! target against the unpatched code for the actual reason rather than by
//! construction. Run it on both targets; a green x86_64 alone means nothing
//! here.
//!
//! # The control, measured
//!
//! Restoring upstream's narrowing in `find_entry_rec` — the one line
//! `let offset = (self.header.leaf_offset + entry.offset) as usize as u64;` —
//! and running the SAME source on both targets, 2026-08-31:
//!
//! ```text
//! cargo test -p pmtiles --lib wide_offset                                    ok
//! cargo test -p pmtiles --lib wide_offset --target i686-unknown-linux-gnu    FAILED
//!     the reader never asked for the leaf directory's true address
//!     (83785884629); it asked for [0, 2181506005]
//! ```
//!
//! That split is the whole defect: identical code, green on the 64-bit builder
//! that gates every CI job, red on a 32-bit target. `2181506005` there is not
//! a number this file computed — it is what the reader asked the backend for.
//!
//! # How a 4 GiB archive is tested without a 4 GiB file
//!
//! `fixtures/leaf.pmtiles` is 4 KB and has real leaf directories. It is read
//! into memory and its 127-byte header rewritten so `leaf_offset` claims the
//! exact value the published 83.8 GB basemap archive carries. The backend
//! underneath translates the offsets back and records every one it was asked
//! for, so the reader does real work on a real archive while genuinely asking
//! for a byte 83.7 GB in.

use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::{AsyncBackend, AsyncPmTilesReader, BackendResponse, PmtResult, TileId};

/// `leaf_offset` of `basemap/omt-20260828.pmtiles`, the archive whose tiles
/// never once reached a browser. Measured, not chosen.
const TRUE_LEAF_OFFSET: u64 = 83_785_884_629;

/// What the shipped build asked for instead: the same number mod 2^32.
const TRUNCATED_LEAF_OFFSET: u64 = 2_181_506_005;

/// The defect in one line, checked by the compiler so the two constants above
/// cannot drift apart and turn the test below into a comparison of two numbers
/// that no longer mean anything.
const _: () = assert!(TRUE_LEAF_OFFSET % (1u64 << 32) == TRUNCATED_LEAF_OFFSET);

/// Byte position of `leaf_offset` in a v3 header: 7 of magic, 1 of version,
/// then `root_offset`, `root_length`, `metadata_offset`, `metadata_length`.
const LEAF_OFFSET_AT: usize = 8 + 8 * 4;
/// Byte position of `leaf_length`.
const LEAF_LENGTH_AT: usize = 8 + 8 * 5;
/// Byte position of `data_offset`.
const DATA_OFFSET_AT: usize = 8 + 8 * 6;

fn read_u64_le(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

fn write_u64_le(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// A real archive presented as though its leaves and data lived 83.7 GB into a
/// much larger file, recording every offset it is asked for.
struct RelocatedArchive {
    bytes: Bytes,
    base: u64,
    asked: Arc<Mutex<Vec<u64>>>,
}

impl AsyncBackend for RelocatedArchive {
    async fn read(&self, offset: u64, length: usize) -> PmtResult<BackendResponse> {
        self.asked.lock().expect("not poisoned").push(offset);

        // `base` is ~83.7e9 and the archive is 4 KB, so "below base" (header,
        // root directory, metadata — not relocated) and "at or above base"
        // (leaves, tile data — relocated) cannot overlap.
        let local = offset.checked_sub(self.base).unwrap_or(offset);

        let start = usize::try_from(local)
            .unwrap_or(usize::MAX)
            .min(self.bytes.len());
        let end = start.saturating_add(length).min(self.bytes.len());
        Ok(BackendResponse::new(self.bytes.slice(start..end)))
    }
}

#[tokio::test]
async fn a_leaf_directory_above_four_gibibytes_is_not_truncated() {
    let mut bytes = std::fs::read("fixtures/leaf.pmtiles").expect("the fixture is readable");

    let real_leaf_offset = read_u64_le(&bytes, LEAF_OFFSET_AT);
    let real_leaf_length = read_u64_le(&bytes, LEAF_LENGTH_AT);
    let real_data_offset = read_u64_le(&bytes, DATA_OFFSET_AT);

    // Non-triviality floor: with no leaf directories there is no leaf hop, the
    // reader answers every tile out of the root directory it already holds,
    // and every assertion below would pass while testing nothing.
    assert!(
        real_leaf_offset > 0 && real_leaf_length > 0 && real_data_offset > 0,
        "the fixture has no leaf directories, so this test would be vacuous: \
         leaf_offset={real_leaf_offset}, leaf_length={real_leaf_length}, \
         data_offset={real_data_offset}"
    );

    // Relocate so the FIRST leaf directory sits exactly where the published
    // archive puts its leaves. The first root entry has `offset == 0`, so the
    // read the reader issues for it is the constant itself with nothing else
    // mixed in.
    let base = TRUE_LEAF_OFFSET - real_leaf_offset;
    write_u64_le(&mut bytes, LEAF_OFFSET_AT, TRUE_LEAF_OFFSET);
    write_u64_le(&mut bytes, DATA_OFFSET_AT, real_data_offset + base);

    let asked = Arc::new(Mutex::new(Vec::new()));
    let reader = AsyncPmTilesReader::try_from_source(RelocatedArchive {
        bytes: Bytes::from(bytes),
        base,
        asked: Arc::clone(&asked),
    })
    .await
    .expect("the relocated archive opens");

    // Held rather than unwrapped: on a target where the truncation is real the
    // tile read fails first, and unwrapping here would report that downstream
    // symptom instead of the assertions below, which name the actual defect.
    let tile = reader
        .get_tile(TileId::new(0).expect("a valid tile id"))
        .await;

    let asked = asked.lock().expect("not poisoned").clone();

    assert!(
        asked.contains(&TRUE_LEAF_OFFSET),
        "the reader never asked for the leaf directory's true address \
         ({TRUE_LEAF_OFFSET}); it asked for {asked:?}"
    );
    assert!(
        !asked.contains(&TRUNCATED_LEAF_OFFSET),
        "the reader asked for {TRUNCATED_LEAF_OFFSET}, which is \
         {TRUE_LEAF_OFFSET} truncated to 32 bits — this is the shipped defect"
    );
    assert!(
        asked.iter().any(|&offset| offset > u64::from(u32::MAX)),
        "no read was above 4 GiB, so nothing here exercised a wide offset"
    );

    // And the hop completed. Without this the offset assertions could hold
    // while the leaf directory failed to inflate, which is what the browser
    // was actually doing.
    let tile = tile
        .expect("the tile read succeeds")
        .expect("the tile is present");
    assert_eq!(tile.as_ref(), b"0");
}
