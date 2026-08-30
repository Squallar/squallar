//! Tests for [`super`].
//!
//! The load-bearing one is the **differential oracle**: for every tile the
//! fixture archive addresses, this index's `length` must equal the byte count
//! `pmtiles::AsyncPmTilesReader::get_tile` hands back — real bytes, shared
//! code zero. It runs against the committed Monaco build (gzip directories,
//! both dedup mechanisms live: 246 addressed / 157 entries / 108 contents)
//! and the hand-built terrain mini archive (plain directories), and it
//! follows the same [`ARCHIVE_ENV`] override the archive reader's suite does,
//! so pointing it at a regional build — the 151 MB Oklahoma archive, 65,953 /
//! 65,913 / 65,768 — re-runs the same oracle over real leaf directories.
//!
//! What Monaco cannot exercise — the leaf walk — is pinned by an archive
//! built by hand *in* the test, the way the terrain mini fixture was built,
//! with ground-truth byte totals no fixture generator touched.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use pmtiles::{AsyncPmTilesReader, HashMapCache, TileCoord};

use super::{
    DirectoryCompression, DownloadBytes, HEADER_BYTES, IndexError, PmtIndex, TileSpan,
    tile_id_to_zxy, zoom_base, zxy_to_tile_id,
};
use crate::basemap_archive::{FileRangeSource, RangeBackend, RangeError, RangeSource};

// ---------------------------------------------------------------------------
// Fixtures, and the loud skip
// ---------------------------------------------------------------------------

/// The committed Monaco fixture. `testdata/README.md` records how it was
/// built and why it never needs rebuilding.
const DEFAULT_ARCHIVE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/monaco.pmtiles");

/// The hand-built raster mini archive: one tile, plain (uncompressed)
/// directories — the `internal_compression = 1` arm Monaco cannot reach.
const TERRAIN_MINI: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/terrain-hillshade-mini.pmtiles"
);

/// The same override the archive reader's suite honours, so one spelling
/// points both suites at a regional build.
const ARCHIVE_ENV: &str = "SQUALLAR_PMTILES_ARCHIVE";

/// Monaco's three dedup counters. Three *different* numbers: runs collapse
/// 246 addressed tiles into 157 entries, content hashing collapses those onto
/// 108 stored blobs — which is what makes this fixture able to distinguish
/// "sum over entries" from "sum over distinct `(offset, length)`".
const MONACO_ADDRESSED_TILES: u64 = 246;
/// See [`MONACO_ADDRESSED_TILES`].
const MONACO_TILE_ENTRIES: u64 = 157;
/// See [`MONACO_ADDRESSED_TILES`].
const MONACO_TILE_CONTENTS: u64 = 108;

/// The archive the oracle runs against: the override if set, Monaco otherwise.
fn archive_path() -> PathBuf {
    std::env::var_os(ARCHIVE_ENV).map_or_else(|| PathBuf::from(DEFAULT_ARCHIVE), PathBuf::from)
}

/// Shout that a test tested nothing — same shape, and same reason, as
/// `basemap_archive::tests::skip_banner`: straight at the stderr handle,
/// because libtest swallows `eprintln!` on a passing test and a skip notice
/// nobody sees is not a notice.
fn skip_banner(test: &str, reason: &str, remedy: &str) {
    use std::io::Write as _;
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "\n\
         ###########################################################################\n\
         ## SKIPPED, NOT PASSED: {test}\n\
         ##   {reason}\n\
         ##   this test asserted NOTHING. {remedy}\n\
         ##   before reading this suite as covering the index.\n\
         ###########################################################################"
    );
}

/// Run `future` on a current-thread runtime.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime should build")
        .block_on(future)
}

/// The oracle's other half: the stock crate's reader, over the same file
/// through the same range source, sharing none of the directory parsing.
type StockReader = AsyncPmTilesReader<RangeBackend<FileRangeSource>, HashMapCache>;

/// Open `path` twice — once for our index, once for the stock reader — or
/// `None` with a shouted skip.
fn open_pair(test: &str, path: &Path) -> Option<(PmtIndex<FileRangeSource>, StockReader)> {
    if !path.is_file() {
        skip_banner(
            test,
            &format!("no PMTiles archive at {}", path.display()),
            &format!("Restore the committed fixture, or point {ARCHIVE_ENV} at an archive,"),
        );
        return None;
    }

    block_on(async {
        let index = PmtIndex::open(FileRangeSource::open(path).expect("the file opens"))
            .await
            .expect("our index should open the archive");
        let reader = AsyncPmTilesReader::try_from_cached_source(
            RangeBackend::new(FileRangeSource::open(path).expect("the file opens")),
            HashMapCache::default(),
        )
        .await
        .expect("the stock reader should open the archive");
        Some((index, reader))
    })
}

/// Every addressed tile in the archive as `(tile_id, its span)`, runs
/// expanded — the enumeration the oracle and the totals both walk.
fn addressed_tiles(index: &PmtIndex<FileRangeSource>) -> Vec<(u64, TileSpan)> {
    block_on(async {
        let mut tiles = Vec::new();
        for entry in index.tile_entries().await.expect("the directories read") {
            for id in entry.tile_id..entry.tile_id + entry.run_length {
                tiles.push((
                    id,
                    TileSpan {
                        offset: entry.offset,
                        length: entry.length,
                    },
                ));
            }
        }
        tiles
    })
}

// ---------------------------------------------------------------------------
// The differential oracle
// ---------------------------------------------------------------------------

/// The oracle's body: every addressed tile's `length`, ours, must equal the
/// stock reader's actual bytes for that coordinate. Answers how many tiles it
/// checked, so callers can refuse a vacuous run.
fn oracle_against(test: &str, path: &Path) -> Option<u64> {
    let (index, reader) = open_pair(test, path)?;
    let tiles = addressed_tiles(&index);

    let checked = block_on(async {
        let mut checked = 0u64;
        for &(id, span) in &tiles {
            let (z, x, y) = tile_id_to_zxy(id).expect("an addressed id is on the grid");
            let coord = TileCoord::new(z, x, y).expect("a grid coordinate is a TileCoord");
            let bytes = reader
                .get_tile(coord)
                .await
                .expect("the stock reader should read the tile")
                .unwrap_or_else(|| {
                    panic!(
                        "our index addresses {z}/{x}/{y} (tile id {id}) but the stock \
                         reader holds nothing there - the directory walk or the \
                         Hilbert conversion is wrong"
                    )
                });
            assert_eq!(
                bytes.len() as u64,
                span.length,
                "tile {z}/{x}/{y} (id {id}): our index says {} bytes, the stock \
                 reader handed back {}",
                span.length,
                bytes.len(),
            );
            checked += 1;
        }
        checked
    });

    // The spec lets a writer leave the counters at 0 for "unknown" — the
    // hand-built terrain mini does — so coverage is pinned only where the
    // header makes a claim to pin it against.
    if index.header().n_addressed_tiles != 0 {
        assert_eq!(
            checked,
            index.header().n_addressed_tiles,
            "the enumeration walked {checked} tiles but the header claims {} - \
             the oracle did not cover the archive it reports on",
            index.header().n_addressed_tiles,
        );
    }
    Some(checked)
}

/// **The differential oracle.** Monaco by default; the [`ARCHIVE_ENV`]
/// override re-runs it over a regional build's real leaf directories.
#[test]
fn every_tiles_length_matches_the_stock_readers_bytes() {
    let Some(checked) = oracle_against(
        "every_tiles_length_matches_the_stock_readers_bytes",
        &archive_path(),
    ) else {
        return;
    };
    assert!(
        checked > 0,
        "an oracle that checked zero tiles proved nothing"
    );
}

/// The same oracle over the hand-built terrain mini archive — the plain
/// (`internal_compression = 1`) directory arm, which Monaco's gzip
/// directories never reach.
#[test]
fn the_terrain_mini_archive_reads_the_same_way() {
    let Some(checked) = oracle_against(
        "the_terrain_mini_archive_reads_the_same_way",
        Path::new(TERRAIN_MINI),
    ) else {
        return;
    };
    assert_eq!(checked, 1, "the mini archive holds exactly one tile");
}

// ---------------------------------------------------------------------------
// The exact total, and the estimate it must never regress to
// ---------------------------------------------------------------------------

/// The exact figure is a **measurement, not an estimate** — the required
/// negative assertion. On a fixture where both dedup mechanisms fired, the
/// distinct-`(offset, length)` total must differ from the per-entry sum
/// (which double-counts content-hash duplicates) *and* from
/// `tile_count × average tile size` (the estimate the figure could silently
/// regress to, which as an integer identity is the per-addressed-tile sum).
/// A test asserting only "the number is right" would pass equally against
/// either wrong sum on a fixture where dedup happened not to fire; requiring
/// them to differ keeps this measuring what it claims to.
#[test]
fn the_exact_total_is_not_an_estimate() {
    const TEST: &str = "the_exact_total_is_not_an_estimate";
    let Some((index, _reader)) = open_pair(TEST, &archive_path()) else {
        return;
    };
    let header = *index.header();
    if header.n_addressed_tiles == header.n_tile_entries
        || header.n_tile_entries == header.n_tile_contents
    {
        skip_banner(
            TEST,
            "this archive's dedup counters do not all differ, so the three sums coincide",
            &format!("Point {ARCHIVE_ENV} at an archive whose three counters differ,"),
        );
        return;
    }

    let tiles = addressed_tiles(&index);

    // The three candidate totals, spelled independently of `download_bytes`.
    let per_tile_sum: u64 = tiles.iter().map(|&(_, span)| span.length).sum();
    let entry_sum: u64 = block_on(index.tile_entries())
        .expect("the directories read")
        .iter()
        .map(|entry| entry.length)
        .sum();
    let distinct_sum: u64 = tiles
        .iter()
        .map(|&(_, span)| span)
        .collect::<HashSet<_>>()
        .iter()
        .map(|span| span.length)
        .sum();

    let coords: Vec<(u8, u32, u32)> = tiles
        .iter()
        .map(|&(id, _)| tile_id_to_zxy(id).expect("an addressed id is on the grid"))
        .collect();
    let total = block_on(index.download_bytes(coords)).expect("the whole archive measures");

    assert_eq!(
        total,
        DownloadBytes {
            bytes: distinct_sum,
            present: header.n_addressed_tiles,
            absent: 0,
        },
        "downloading every addressed tile must cost exactly the distinct spans",
    );
    assert_ne!(
        total.bytes, entry_sum,
        "the total equals the per-entry sum - content-hash duplicates are being \
         double-counted, or this fixture stopped distinguishing them",
    );
    assert_ne!(
        total.bytes, per_tile_sum,
        "the total equals tile_count x average (the per-addressed-tile sum) - \
         the exact figure has regressed to an estimate",
    );
    assert!(
        total.bytes < entry_sum && entry_sum < per_tile_sum,
        "with both dedup mechanisms live the sums must strictly order: \
         distinct {} < entries {entry_sum} < per-tile {per_tile_sum}",
        total.bytes,
    );
}

/// Monaco's counters, pinned against our own header parse — and the walk
/// against the header. Skipped under the override: these constants describe
/// the committed file specifically.
#[test]
fn the_committed_fixture_distinguishes_both_dedup_mechanisms() {
    const TEST: &str = "the_committed_fixture_distinguishes_both_dedup_mechanisms";
    if std::env::var_os(ARCHIVE_ENV).is_some() {
        skip_banner(
            TEST,
            "the archive override is set, and these counters describe monaco.pmtiles",
            &format!("Unset {ARCHIVE_ENV},"),
        );
        return;
    }
    let Some((index, _reader)) = open_pair(TEST, Path::new(DEFAULT_ARCHIVE)) else {
        return;
    };

    let header = *index.header();
    assert_eq!(header.n_addressed_tiles, MONACO_ADDRESSED_TILES);
    assert_eq!(header.n_tile_entries, MONACO_TILE_ENTRIES);
    assert_eq!(header.n_tile_contents, MONACO_TILE_CONTENTS);
    assert_eq!(header.internal_compression, DirectoryCompression::Gzip);
    assert_eq!((header.min_zoom, header.max_zoom), (0, 14));

    let entries = block_on(index.tile_entries()).expect("the directories read");
    assert_eq!(
        entries.len() as u64,
        MONACO_TILE_ENTRIES,
        "the decoded entry count must match the header's own claim",
    );
    assert_eq!(
        entries.iter().map(|entry| entry.run_length).sum::<u64>(),
        MONACO_ADDRESSED_TILES,
        "runs expanded must cover every addressed tile",
    );
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.offset, entry.length))
            .collect::<HashSet<_>>()
            .len() as u64,
        MONACO_TILE_CONTENTS,
        "distinct (offset, length) pairs are exactly the stored contents - the \
         set download_bytes sums over",
    );
}

/// A tile the archive does not hold costs nothing, is counted rather than
/// silently folded in, and never disturbs the present tiles' total.
#[test]
fn an_absent_tile_costs_nothing_and_is_counted() {
    const TEST: &str = "an_absent_tile_costs_nothing_and_is_counted";
    let Some((index, _reader)) = open_pair(TEST, &archive_path()) else {
        return;
    };

    let tiles = addressed_tiles(&index);
    let held: HashSet<u64> = tiles.iter().map(|&(id, _)| id).collect();
    let z = index.header().max_zoom;
    let absent = (0u32..)
        .map(|x| (z, x, 0u32))
        .find(|&(z, x, y)| zxy_to_tile_id(z, x, y).is_some_and(|id| !held.contains(&id)))
        .expect("a regional archive leaves most of the world's tiles absent");

    let alone = block_on(index.download_bytes([absent])).expect("absence is not an error");
    assert_eq!(
        alone,
        DownloadBytes {
            bytes: 0,
            present: 0,
            absent: 1,
        },
    );

    let (first_id, first_span) = tiles[0];
    let first = tile_id_to_zxy(first_id).expect("an addressed id is on the grid");
    let mixed = block_on(index.download_bytes([first, absent])).expect("a mixed set measures");
    assert_eq!(
        mixed,
        DownloadBytes {
            bytes: first_span.length,
            present: 1,
            absent: 1,
        },
        "an absent tile must not disturb the present tiles' bytes",
    );
}

// ---------------------------------------------------------------------------
// The leaf walk, on a hand-built archive
// ---------------------------------------------------------------------------

/// A byte-buffer [`RangeSource`], for archives built inside a test.
struct BytesSource(Vec<u8>);

impl RangeSource for BytesSource {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(self.0.len());
        let end = start.saturating_add(length).min(self.0.len());
        let bytes = self.0[start..end].to_vec();
        async move { Ok(bytes) }
    }
}

/// LEB128-encode `value` onto `out`.
fn push_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// How a hand-built entry spells its offset on disk.
enum Offset {
    /// The varint `offset + 1`.
    Absolute(u64),
    /// The varint `0`: "the previous entry's offset plus its length".
    Chained,
}

/// Serialize one directory: `(tile_id, run_length, length, offset)` rows in
/// the spec's four-column layout.
fn encode_directory(rows: &[(u64, u64, u64, Offset)]) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint(rows.len() as u64, &mut out);
    let mut previous_id = 0;
    for &(tile_id, ..) in rows {
        push_varint(tile_id - previous_id, &mut out);
        previous_id = tile_id;
    }
    for &(_, run_length, ..) in rows {
        push_varint(run_length, &mut out);
    }
    for &(_, _, length, _) in rows {
        push_varint(length, &mut out);
    }
    for (.., offset) in rows {
        match offset {
            Offset::Absolute(value) => push_varint(value + 1, &mut out),
            Offset::Chained => push_varint(0, &mut out),
        }
    }
    out
}

/// A whole v3 archive in memory: header, root, leaf section, tile data —
/// plain directories, the way the terrain mini fixture was hand-built.
fn hand_built_archive() -> (Vec<u8>, u64) {
    // Tile data: two blobs. Four addressed tiles point at them - a run of
    // two (run_length dedup), one distinct, and one NON-adjacent duplicate of
    // blob 0 (content-hash dedup: a fresh entry, same bytes).
    let blob0 = vec![0xaa; 10];
    let blob1 = vec![0xbb; 20];
    let first_id = zoom_base(10) + 100;

    let leaf = encode_directory(&[
        (first_id, 2, 10, Offset::Absolute(0)),
        (first_id + 5, 1, 20, Offset::Chained),
        (first_id + 9, 1, 10, Offset::Absolute(0)),
    ]);
    let root = encode_directory(&[(first_id, 0, leaf.len() as u64, Offset::Absolute(0))]);

    let root_offset = HEADER_BYTES as u64;
    let leaf_offset = root_offset + root.len() as u64;
    let tile_data_offset = leaf_offset + leaf.len() as u64;

    let mut header = vec![0u8; HEADER_BYTES];
    header[0..7].copy_from_slice(b"PMTiles");
    header[7] = 3;
    header[8..16].copy_from_slice(&root_offset.to_le_bytes());
    header[16..24].copy_from_slice(&(root.len() as u64).to_le_bytes());
    header[40..48].copy_from_slice(&leaf_offset.to_le_bytes());
    header[48..56].copy_from_slice(&(leaf.len() as u64).to_le_bytes());
    header[56..64].copy_from_slice(&tile_data_offset.to_le_bytes());
    header[64..72].copy_from_slice(&30u64.to_le_bytes());
    header[72..80].copy_from_slice(&4u64.to_le_bytes()); // addressed
    header[80..88].copy_from_slice(&3u64.to_le_bytes()); // entries
    header[88..96].copy_from_slice(&2u64.to_le_bytes()); // contents
    header[96] = 1; // clustered
    header[97] = 1; // internal_compression: none
    header[98] = 1; // tile_compression: none
    header[99] = 4; // tile_type: webp, matching the terrain mini precedent
    header[100] = 10;
    header[101] = 10;

    let mut archive = header;
    archive.extend_from_slice(&root);
    archive.extend_from_slice(&leaf);
    archive.extend_from_slice(&blob0);
    archive.extend_from_slice(&blob1);
    (archive, first_id)
}

/// The leaf walk, which no committed fixture reaches (Monaco addresses all
/// 246 tiles from its root), pinned on ground truth no generator touched —
/// including the offset-chain decode and the dedup arithmetic.
#[test]
fn a_leaf_directory_resolves_through_the_walk() {
    let (archive, first_id) = hand_built_archive();
    let index = block_on(PmtIndex::open(BytesSource(archive))).expect("the archive opens");

    let span_of = |id: u64| block_on(index.span_for_id(id)).expect("the walk reads");
    let blob0 = TileSpan {
        offset: 0,
        length: 10,
    };
    let blob1 = TileSpan {
        offset: 10,
        length: 20,
    };
    assert_eq!(span_of(first_id), Some(blob0), "the run's first tile");
    assert_eq!(span_of(first_id + 1), Some(blob0), "the run's second tile");
    assert_eq!(
        span_of(first_id + 5),
        Some(blob1),
        "the chained offset must decode to the previous offset plus length",
    );
    assert_eq!(
        span_of(first_id + 9),
        Some(blob0),
        "the content-hash duplicate points back at blob 0's bytes",
    );
    assert_eq!(span_of(first_id + 2), None, "between entries is absent");
    assert_eq!(span_of(first_id - 1), None, "before the leaf is absent");

    let coords: Vec<_> = [first_id, first_id + 1, first_id + 5, first_id + 9]
        .into_iter()
        .map(|id| tile_id_to_zxy(id).expect("on the grid"))
        .collect();
    let total = block_on(index.download_bytes(coords)).expect("the set measures");
    assert_eq!(
        total,
        DownloadBytes {
            bytes: 30,
            present: 4,
            absent: 0,
        },
        "10 + 20, each blob once: not the 40 of summing entries, not the 50 of \
         summing per tile",
    );
}

// ---------------------------------------------------------------------------
// Tile ids
// ---------------------------------------------------------------------------

/// The spec's own first ids, then the round trip across whole zoom levels.
#[test]
fn tile_ids_speak_the_specs_hilbert_ordering() {
    // z0 is id 0; z1's four tiles wind 1..=4 in Hilbert order; z2 opens at 5.
    for ((z, x, y), want) in [
        ((0, 0, 0), 0),
        ((1, 0, 0), 1),
        ((1, 0, 1), 2),
        ((1, 1, 1), 3),
        ((1, 1, 0), 4),
        ((2, 0, 0), 5),
    ] {
        assert_eq!(
            zxy_to_tile_id(z, x, y),
            Some(want),
            "{z}/{x}/{y} is tile id {want} in the spec's own examples",
        );
    }

    for z in 0..=5u8 {
        let n = 1u32 << z;
        for x in 0..n {
            for y in 0..n {
                let id = zxy_to_tile_id(z, x, y).expect("on the grid");
                assert_eq!(
                    tile_id_to_zxy(id),
                    Some((z, x, y)),
                    "id {id} did not round-trip",
                );
            }
        }
    }
}

/// The grid's edges: the deepest legal zoom round-trips, and everything off
/// the grid is `None` rather than a wrong id.
#[test]
fn tile_ids_hold_at_the_grids_edges() {
    let corner = u32::MAX >> 1; // 2^31 - 1
    let id = zxy_to_tile_id(31, corner, corner).expect("the far corner of z31");
    assert_eq!(tile_id_to_zxy(id), Some((31, corner, corner)));

    assert_eq!(zxy_to_tile_id(32, 0, 0), None, "zoom past 31");
    assert_eq!(zxy_to_tile_id(3, 8, 0), None, "x past the grid");
    assert_eq!(zxy_to_tile_id(3, 0, 8), None, "y past the grid");
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Damage is an error with a name, never a panic and never a wrong number.
#[test]
fn a_damaged_archive_is_refused_with_its_defect_named() {
    let truncated = block_on(PmtIndex::open(BytesSource(vec![0u8; 10]))).err();
    assert!(
        matches!(truncated, Some(IndexError::Truncated { .. })),
        "10 bytes cannot hold a header: {truncated:?}",
    );

    let (mut alien, _) = hand_built_archive();
    alien[0] = b'X';
    let alien = block_on(PmtIndex::open(BytesSource(alien))).err();
    assert!(
        matches!(alien, Some(IndexError::NotPmtilesV3)),
        "a wrong magic must be refused: {alien:?}",
    );

    let (mut zstd, _) = hand_built_archive();
    zstd[97] = 4; // internal_compression: zstd, legal in the spec, not spoken here
    let zstd = block_on(PmtIndex::open(BytesSource(zstd))).err();
    assert!(
        matches!(
            zstd,
            Some(IndexError::Unsupported {
                what: "internal_compression",
                value: 4,
            })
        ),
        "an undecodable directory compression must be named, not misread: {zstd:?}",
    );

    let (mut short, _) = hand_built_archive();
    short.truncate(HEADER_BYTES + 4);
    let short = block_on(PmtIndex::open(BytesSource(short))).err();
    assert!(
        matches!(short, Some(IndexError::Truncated { .. })),
        "a root directory cut short must be truncation, not a decode: {short:?}",
    );
}
