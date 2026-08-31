//! The pin on archive offsets above 4 GiB.
//!
//! This suite exists for one shipped defect: the self-hosted vector basemap
//! never rendered a single tile in a browser, for as long as it shipped, and
//! nothing went red. `pmtiles-0.23.0` declared its backend seam as
//! `fn read(&self, offset: usize, length: usize)`, and `usize` is **32 bits on
//! wasm32**. Every archive offset above 4 GiB was therefore truncated mod
//! 2^32 on the one target the basemap was for. Native was never affected,
//! which is why every native test in this tree stayed green.
//!
//! # What each half of this file proves, and on which target
//!
//! **Read this before trusting a green run**, because the obvious version of
//! this test proves nothing. `usize` is 64 bits on the machine that runs
//! `cargo test`, so a test that merely records the offset the reader asks for
//! and compares it to the right number passes *whether or not the bug is
//! present*. It is exactly that shape of check which let the defect ship.
//!
//! So the proof is split, and only the first half is host-independent:
//!
//! * [`the_seam_cannot_narrow`] is a **compile-time** proof, and it is the
//!   regression gate. `u64` and `usize` are distinct types in Rust even where
//!   they are the same width, so a `u64` cannot be passed to a parameter
//!   declared `usize` on *any* host — a 64-bit one included. Against the
//!   unpatched crate this file does not compile, and neither does
//!   [`super::RangeBackend`]'s own `impl AsyncBackend`, which likewise
//!   declares `offset: u64`. That is a real regression gate on a 64-bit
//!   builder: it cannot silently pass.
//!
//! * [`a_leaf_directory_above_four_gibibytes_is_read_at_its_true_address`] is
//!   a **behavioural** test on the real numbers from the outage. On its own it
//!   would be vacuous on a 64-bit host, for the reason above. What it adds is
//!   that the offset the reader *computes* is the true 64-bit sum and that the
//!   whole leaf hop still works when it is — arithmetic and plumbing, not
//!   width. It is what would catch a future `saturating_`/`wrapping_` or a
//!   re-narrowing one layer down.
//!
//! Neither half runs on wasm32. `cargo check --workspace --all-targets
//! --target wasm32-unknown-unknown` compiles this crate for that target and so
//! type-checks the `u64` seam there, but nothing here *executes* a 32-bit read
//! — this crate pulls in wgpu and winit and does not build for a 32-bit
//! target.
//!
//! **The 32-bit execution lives one crate down**, in
//! `vendor/pmtiles/src/wide_offset_tests.rs`, which builds and runs on
//! `i686-unknown-linux-gnu`. Its module doc carries the measured control: the
//! same source, green on x86_64 and red on i686, asking for `2181506005`. Read
//! that before concluding this file is the whole of the evidence — it is not,
//! and on its own it would not be enough.
//!
//! # How a 4 GiB archive is tested without a 4 GiB file
//!
//! The committed fixtures are 419 KB and 803 B, and no fixture can be 4 GiB.
//! So the archive here is **real but relocated**: a genuine multi-level
//! archive is written in memory by `PmTilesWriter` — header, gzip-deflated
//! root directory, gzip-deflated leaf directories, tile data — and then its
//! 127-byte header is rewritten so that `leaf_offset` claims the exact value
//! the published archive carries, **83,785,884,629**. The source underneath
//! translates the offsets back, and records every one it was asked for.
//!
//! The reader is therefore doing real work on a real archive and genuinely
//! asking for a byte 83.7 GB in, which is the only thing that mattered.

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use pmtiles::{AsyncPmTilesReader, Compression, HashMapCache, PmTilesWriter, TileId, TileType};

use super::{ArchiveRangeSource, RangeBackend, RangeError, RangeSource};

/// `leaf_offset` of `basemap/omt-20260828.pmtiles`, the published archive.
///
/// Measured, not chosen: the archive is 83.8 GB and this is where its leaf
/// directories begin.
const TRUE_LEAF_OFFSET: u64 = 83_785_884_629;

/// What the shipped build asked for instead.
const TRUNCATED_LEAF_OFFSET: u64 = 2_181_506_005;

/// The defect in one line, checked by the compiler rather than asserted in
/// prose: the address the browser fetched was the true one mod 2^32.
///
/// This says nothing about the fix. It pins the *diagnosis*, so that the two
/// constants above cannot drift apart and quietly turn the test below into a
/// comparison between two numbers that no longer mean anything.
const _: () = assert!(TRUE_LEAF_OFFSET % (1u64 << 32) == TRUNCATED_LEAF_OFFSET);

/// Byte position of `leaf_offset` in a PMTiles v3 header.
///
/// 7 bytes of magic, 1 of version, then eight `u64le` in the order
/// `root_offset`, `root_length`, `metadata_offset`, `metadata_length`,
/// `leaf_offset`, ... (`vendor/pmtiles/src/header.rs`, `try_from_bytes`).
const LEAF_OFFSET_AT: usize = 8 + 8 * 4;

/// Byte position of `leaf_length` in the same header.
const LEAF_LENGTH_AT: usize = 8 + 8 * 5;

/// Byte position of `data_offset` in the same header.
const DATA_OFFSET_AT: usize = 8 + 8 * 6;

/// Tiles written into the fixture archive.
///
/// Enough that the root directory cannot hold them and the writer falls back
/// to "root directory is leaf pointers only" — which is the shape the
/// published archive has, and the shape that made the outage total rather than
/// partial. [`a_leaf_directory_above_four_gibibytes_is_read_at_its_true_address`]
/// asserts the fallback actually happened rather than assuming it.
const TILES: u64 = 20_000;

fn read_u64_le(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

fn write_u64_le(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// A real archive presented as though its leaf and data sections were 83.7 GB
/// into a much larger file, recording every offset it is asked for.
struct RelocatedArchive {
    /// The written archive, with its header's `leaf_offset` and `data_offset`
    /// increased by [`Self::base`].
    bytes: Vec<u8>,
    /// What was added to those two header fields.
    base: u64,
    /// Every `offset` [`RangeSource::read_range`] was called with, in order.
    asked: Arc<Mutex<Vec<u64>>>,
}

impl RangeSource for RelocatedArchive {
    async fn read_range(&self, offset: u64, length: usize) -> Result<Vec<u8>, RangeError> {
        self.asked.lock().expect("not poisoned").push(offset);

        // Everything the header itself points at (root directory, metadata)
        // is still where it was; only the two relocated sections are shifted.
        // `base` is ~83.7e9 and the archive is a few hundred KB, so the two
        // ranges cannot overlap and the branch is unambiguous.
        let local = offset.checked_sub(self.base).unwrap_or(offset);

        let start = usize::try_from(local)
            .unwrap_or(usize::MAX)
            .min(self.bytes.len());
        let end = start.saturating_add(length).min(self.bytes.len());
        Ok(self.bytes[start..end].to_vec())
    }
}

/// Write a real archive deep enough to have leaf directories.
///
/// `internal_compression` is left at the writer's default, which is gzip —
/// deliberately, and not because the test needs compression. The shipped
/// symptom was 48 × `Invalid gzip header` in the browser console: the reader
/// fetched the wrong bytes and then tried to inflate them. Writing the
/// directories gzipped is what makes this fixture able to produce that same
/// symptom if the offset is ever wrong again, instead of silently returning a
/// directory full of nonsense.
fn write_archive_with_leaves() -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = PmTilesWriter::new(TileType::Mvt)
            .create(&mut cursor)
            .expect("the writer starts");
        for tile_id in 0..TILES {
            let data = tile_id.to_le_bytes().to_vec();
            writer
                .add_tile(TileId::new(tile_id).expect("a valid tile id").into(), &data)
                .expect("the tile is written");
        }
        writer.finalize().expect("the archive is finalized");
    }
    cursor.into_inner()
}

/// The compile-time half, and the actual regression gate.
///
/// If [`pmtiles::AsyncBackend::read`] took `offset: usize` again, this body
/// would not compile — on a 32-bit host *or* a 64-bit one, because `u64` and
/// `usize` are different types regardless of their widths. There is no host on
/// which this reads green while the defect is present.
///
/// It is written as a real call rather than a `const` assertion because the
/// property being pinned is a *parameter type*, and the only thing that checks
/// a parameter type is passing an argument to it.
#[test]
fn the_seam_cannot_narrow() {
    fn offset_argument_is_u64<S: ArchiveRangeSource>(backend: &RangeBackend<S>, offset: u64) {
        // Not awaited: constructing the future is what type-checks the
        // argument, and running it would need a source.
        let _unpolled = pmtiles::AsyncBackend::read(backend, offset, 0);
    }

    let backend = RangeBackend::new(RelocatedArchive {
        bytes: Vec::new(),
        base: 0,
        asked: Arc::new(Mutex::new(Vec::new())),
    });
    offset_argument_is_u64(&backend, TRUE_LEAF_OFFSET);
}

/// The behavioural half: the reader asks for the true 64-bit address of a leaf
/// directory that lives 83.7 GB into an archive, and the tile comes back.
///
/// See the module doc for what this does and does not prove — on a 64-bit host
/// it is a test of the reader's arithmetic and plumbing, not of the width of
/// its seam. [`the_seam_cannot_narrow`] is the width.
#[tokio::test]
async fn a_leaf_directory_above_four_gibibytes_is_read_at_its_true_address() {
    let mut bytes = write_archive_with_leaves();

    let real_leaf_offset = read_u64_le(&bytes, LEAF_OFFSET_AT);
    let real_leaf_length = read_u64_le(&bytes, LEAF_LENGTH_AT);
    let real_data_offset = read_u64_le(&bytes, DATA_OFFSET_AT);

    // Non-triviality floor. If the writer ever stops producing leaf
    // directories for this many tiles, every assertion below would still pass
    // while testing nothing at all: with no leaves there is no leaf hop, and
    // the reader would find the tile in the root directory it already holds,
    // never asking the source for a wide offset.
    //
    // Note the layout this asserts against, which is not the obvious one: the
    // stream writer emits tile data as tiles arrive and both directories at
    // `finalize`, so the sections run header | root | metadata | **data** |
    // leaves, and `data_offset` is BELOW `leaf_offset`. Measured on this
    // fixture: root at 127, metadata at 16384, data at 16406, leaves at
    // 496150. Both relocated sections are shifted by the same `base`, so the
    // order between them does not matter — but an assertion that assumed the
    // other order stood here and was wrong.
    assert!(
        real_leaf_offset > 0 && real_leaf_length > 0 && real_data_offset > 0,
        "the fixture archive has no leaf directories, so this test would be \
         vacuous: leaf_offset={real_leaf_offset}, leaf_length={real_leaf_length}, \
         data_offset={real_data_offset}"
    );

    // Relocate so the FIRST leaf directory sits at exactly the address the
    // published archive puts its leaves at. The first root entry has
    // `offset == 0`, so the read the reader issues for it is
    // `leaf_offset + 0` — the constant itself, with nothing else mixed in.
    let base = TRUE_LEAF_OFFSET - real_leaf_offset;
    write_u64_le(&mut bytes, LEAF_OFFSET_AT, TRUE_LEAF_OFFSET);
    write_u64_le(&mut bytes, DATA_OFFSET_AT, real_data_offset + base);

    let asked = Arc::new(Mutex::new(Vec::new()));
    let reader = AsyncPmTilesReader::try_from_cached_source(
        RangeBackend::new(RelocatedArchive {
            bytes,
            base,
            asked: Arc::clone(&asked),
        }),
        HashMapCache::default(),
    )
    .await
    .expect("the relocated archive opens");

    // Gzipped directories, as the module doc explains: this is what lets a
    // wrong offset reproduce the browser's `Invalid gzip header` rather than
    // quietly yielding a directory of nonsense.
    assert_eq!(
        reader.get_header().internal_compression(),
        Compression::Gzip
    );

    // Tile 0 is behind the first leaf pointer, so its leaf read is at
    // `leaf_offset + 0`.
    //
    // The result is deliberately NOT unwrapped yet. Reintroducing the
    // truncation makes the leaf directory inflate to garbage and the tile read
    // fail first, so unwrapping here would report a downstream symptom
    // (`UnexpectedNumberOfBytesReturned`) instead of the offset assertions
    // below, which say what actually went wrong. Verified by tampering: with
    // `find_entry_rec` truncated to 32 bits, the message that fires is the
    // `TRUNCATED_LEAF_OFFSET` one.
    let raw = reader
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

    // The tile itself lives in the data section, which is relocated too, so
    // its read is above 4 GiB by a different arithmetic path
    // (`data_offset + entry.offset`, not `leaf_offset + entry.offset`).
    assert!(
        asked.iter().any(|&offset| offset > u64::from(u32::MAX)),
        "no read was above 4 GiB, so nothing here exercised a wide offset"
    );

    let raw = raw
        .expect("the tile read succeeds")
        .expect("the tile is present");

    // The same four bytes the diagnosis used to tell a right read from a wrong
    // one: a correctly addressed read of this archive starts a gzip member,
    // and the shipped build's truncated read started `55 29 c9 a4`, which is
    // not one. Asserted on the tile rather than left implicit, because "some
    // bytes came back" is exactly what the broken build also did.
    assert_eq!(
        &raw[..4],
        &[0x1f, 0x8b, 0x08, 0x00],
        "the bytes at the relocated address do not begin a gzip member"
    );

    let tile = reader
        .get_tile_decompressed(TileId::new(0).expect("a valid tile id"))
        .await
        .expect("the tile read succeeds")
        .expect("the tile is present");

    // The hop actually completed. Without this the offset assertions could all
    // hold while the leaf directory failed to inflate — which is precisely
    // what the browser was doing.
    assert_eq!(
        tile.as_ref(),
        0u64.to_le_bytes(),
        "the tile behind the 83.7 GB leaf directory did not come back intact"
    );
}
