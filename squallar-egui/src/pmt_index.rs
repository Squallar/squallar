//! The exact-size PMTiles v3 directory reader.
//!
//! Answers one question the stock `pmtiles` crate declines to: **what would a
//! set of tiles cost to download, to the byte?** `DirEntry`'s `offset` and
//! `length` are `pub(crate)` there, so the sum is unreachable through its
//! public API. This module parses the fixed 127-byte v3 header and the varint
//! directories itself — sharing no code with the crate — while the stock
//! reader keeps serving the render path untouched
//! ([`crate::basemap_archive`]).
//!
//! # The sum is over distinct `(offset, length)` pairs, not entries
//!
//! PMTiles dedups twice. `run_length` collapses *consecutive* tile ids sharing
//! a blob into one entry; the writer's content-hash map additionally emits a
//! **fresh entry** pointing at an existing `(offset, length)` for a repeated
//! blob that is *not* adjacent. Two non-adjacent identical tiles are therefore
//! two entries sharing bytes, and summing over entries double-counts them. The
//! format concedes the point itself: `n_addressed_tiles`, `n_tile_entries` and
//! `n_tile_contents` are three separate header fields — 246, 157 and 108 on
//! the committed Monaco fixture. [`PmtIndex::download_bytes`] counts each
//! distinct pair once, which is what a range-coalescing downloader actually
//! transfers.
//!
//! # Correctness is a differential oracle, not review
//!
//! For every tile in the committed fixture archives, the tests assert this
//! index's `length` equals `reader.get_tile(coord)`'s byte count from the
//! stock crate — real bytes, shared code zero. See `pmt_index/tests.rs`.
//!
//! Portable half only, like the archive reader: everything here reaches bytes
//! through [`RangeSource`], so it compiles on wasm32 unchanged.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Read as _;
use std::sync::{Arc, Mutex, OnceLock};

use futures::StreamExt as _;

use crate::basemap_archive::{ArchiveRangeSource, RangeError, RangeSource};

/// Bytes in the fixed v3 header. The layout is frozen by the spec; nothing
/// here is configurable.
pub const HEADER_BYTES: usize = 127;

/// The seven magic bytes every v3 archive opens with.
const MAGIC: &[u8; 7] = b"PMTiles";

/// The one spec version this reader speaks.
const SPEC_VERSION: u8 = 3;

/// Directories nested past this depth are a corrupt or adversarial file, not
/// an archive: the format's own writers emit root → leaf, one hop. The walk
/// in [`PmtIndex::span_for_id`] allows a little slack over that rather than
/// trusting the file to terminate.
const MAX_DIRECTORY_DEPTH: usize = 4;

/// How many leaf-directory reads [`PmtIndex::warm_leaves`] keeps in flight.
///
/// **The planning phase's cost is round trips, not bytes.** Measured against
/// the published planet archive over HTTPS on 2026-08-29: enumerating a 568 km
/// box to z14 (110,026 tiles) touches 20 distinct leaves, each read averaging
/// 33 KB and 28 ms, and one at a time that is 20 sequential round trips
/// carrying 660 KB. The count is what the wait is made of, so the fix is to
/// overlap the round trips rather than to read fewer bytes.
///
/// Sixteen rather than the six [`crate::tile_source`] bounds tile fetches at:
/// that channel is bounded to keep a bulk pull from starving the live map's
/// working set, and these reads are a *bounded, one-off* burst of a few tens
/// of small ranges that the user is actively waiting on. The widest area this
/// download arm will accept (2000 km half-width) is still a few tens of
/// leaves, so this bounds the burst to two or three waves rather than making
/// it unbounded.
pub const MAX_INFLIGHT_LEAF_READS: usize = 16;

/// Bytes of decoded leaf directories carried from one reader of an archive to
/// the next.
///
/// **A cross-reader accelerator, never the working cache.** Every
/// [`PmtIndex`] keeps every leaf it reads for its own lifetime regardless
/// ([`PmtIndex::leaves`]), so this budget can never make one walk re-read a
/// leaf that walk already has; all it bounds is how much of a *finished*
/// reader's work the next reader inherits. That is the whole point: the size
/// probe and the download engine open the archive separately over the same
/// area, and without this the engine repeats every directory round trip the
/// probe just made.
///
/// **Measured, not assumed**: one leaf of the published planet archive
/// decodes to 17,653 entries — 564,896 bytes of [`DirEntry`] — from 34,529
/// compressed bytes (2026-08-29, leaf at `leaf_offset`). The widest single
/// area measured across ten world cities touched 36 leaves, so 24 MB carries
/// any one of them whole. The doc this replaces said "a leaf is a few KB
/// decoded", which was true of the Monaco fixture and wrong by 100× about the
/// archive that ships.
pub const SHARED_LEAF_BYTES: usize = 24_000_000;

/// How a directory's bytes are compressed on disk.
///
/// Our own two-variant vocabulary rather than the stock crate's enum, because
/// this module shares no code with it. Brotli and zstd are legal in the spec
/// but nothing we publish emits them; an archive using one fails
/// [`PmtIndex::open`] with the value named rather than misreading garbage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryCompression {
    /// Stored plain.
    None,
    /// A gzip stream.
    Gzip,
}

/// The v3 header's fields, read from the fixed 127-byte layout.
///
/// Every figure is the file's own claim; nothing is derived. The three dedup
/// counters are carried because the tests pin the directory walk against them
/// — see the module doc for why they are three different numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHeader {
    /// Where the root directory's bytes start, from the file's first byte.
    pub root_offset: u64,
    /// The root directory's byte length.
    pub root_length: u64,
    /// Where the metadata JSON's bytes start. Compressed like the
    /// directories; the download engine copies it into every segment it
    /// writes, so a sub-archive carries the same `vector_layers` the parent
    /// does.
    pub metadata_offset: u64,
    /// The metadata's byte length.
    pub metadata_length: u64,
    /// Where the leaf-directory section starts. Leaf entries' offsets are
    /// relative to this.
    pub leaf_offset: u64,
    /// The leaf-directory section's byte length. Zero means every tile is
    /// addressed from the root.
    pub leaf_length: u64,
    /// Where the tile-data section starts. Tile entries' offsets are relative
    /// to this.
    pub tile_data_offset: u64,
    /// The tile-data section's byte length.
    pub tile_data_length: u64,
    /// Tiles addressable through the directories, runs expanded.
    pub n_addressed_tiles: u64,
    /// Tile entries across every directory, runs *not* expanded.
    pub n_tile_entries: u64,
    /// Distinct tile blobs stored — the count of distinct `(offset, length)`
    /// pairs the entries point at.
    pub n_tile_contents: u64,
    /// Whether tile data is ordered by tile id.
    pub clustered: bool,
    /// How the directories themselves are compressed.
    pub internal_compression: DirectoryCompression,
    /// How tile bodies are compressed — the raw spec value, carried but never
    /// decoded. This index measures bytes on the wire, and those are the
    /// compressed bytes whatever they hold.
    pub tile_compression: u8,
    /// What the tiles are — the raw spec value, for the same reason.
    pub tile_type: u8,
    /// The shallowest zoom the archive stores.
    pub min_zoom: u8,
    /// The deepest zoom the archive stores.
    pub max_zoom: u8,
}

/// One decoded directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    /// First tile id this entry speaks for.
    pub tile_id: u64,
    /// How many consecutive tile ids share this entry's bytes. **Zero means
    /// this entry points at a leaf directory**, not at a tile.
    pub run_length: u64,
    /// Byte offset — relative to the tile-data section for a tile, to the
    /// leaf section for a leaf pointer.
    pub offset: u64,
    /// Byte length of the blob or leaf directory.
    pub length: u64,
}

/// One tile blob's place in the tile-data section: the unit the download
/// total is deduplicated over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileSpan {
    /// Offset relative to [`IndexHeader::tile_data_offset`].
    pub offset: u64,
    /// Length in bytes.
    pub length: u64,
}

/// What a set of tiles costs, exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DownloadBytes {
    /// Total bytes to transfer: each distinct `(offset, length)` pair counted
    /// once, however many requested tiles share it.
    pub bytes: u64,
    /// Requested tiles the archive holds.
    pub present: u64,
    /// Requested tiles the archive does not hold. Absence costs nothing and
    /// is not an error — an area's corner tiles falling off a regional
    /// archive's edge is ordinary.
    pub absent: u64,
}

/// What went wrong reading the index.
#[derive(Debug)]
pub enum IndexError {
    /// The source would not hand back bytes.
    Range(RangeError),
    /// The source answered, but with fewer bytes than the structure needs.
    Truncated {
        /// What was being read.
        what: &'static str,
        /// Bytes the structure needs.
        wanted: u64,
        /// Bytes that arrived.
        got: u64,
    },
    /// The file does not open with the v3 magic and version.
    NotPmtilesV3,
    /// A header field holds a value this reader does not speak.
    Unsupported {
        /// Which field.
        what: &'static str,
        /// The raw value found.
        value: u8,
    },
    /// A directory's bytes disagreed with themselves.
    Directory(&'static str),
    /// The coordinate is not a tile: a zoom past 31, or an `x`/`y` outside
    /// the grid at its zoom.
    Coordinate {
        /// Zoom.
        z: u8,
        /// Column.
        x: u32,
        /// Row.
        y: u32,
    },
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Range(error) => write!(f, "the index could not read a range: {error}"),
            Self::Truncated { what, wanted, got } => {
                write!(f, "{what} is truncated: wanted {wanted} bytes, got {got}")
            }
            Self::NotPmtilesV3 => write!(f, "not a PMTiles v3 archive"),
            Self::Unsupported { what, value } => {
                write!(f, "{what} value {value} is not supported by this reader")
            }
            Self::Directory(what) => write!(f, "a directory would not decode: {what}"),
            Self::Coordinate { z, x, y } => write!(f, "{z}/{x}/{y} is not a tile coordinate"),
        }
    }
}

impl std::error::Error for IndexError {}

impl From<RangeError> for IndexError {
    fn from(error: RangeError) -> Self {
        Self::Range(error)
    }
}

// ---------------------------------------------------------------------------
// Tile ids
// ---------------------------------------------------------------------------

/// Tiles at every zoom shallower than `z` — the id the first tile of zoom `z`
/// carries. `(4^z − 1) / 3`, the closed form of `Σ 4^i, i < z`.
fn zoom_base(z: u8) -> u64 {
    ((1u64 << (2 * u32::from(z))) - 1) / 3
}

/// `z/x/y` → PMTiles tile id, or `None` off the grid (`z > 31`, or a
/// coordinate outside `2^z` on either axis).
///
/// Tile ids order each zoom level on a Hilbert curve. This is the standard
/// `xy2d` walk over a `2^z`-cell grid, offset by every shallower zoom's tile
/// count; the differential oracle in the tests is what pins it to the curve
/// the format actually uses, by round-tripping every fixture tile through the
/// stock reader's own coordinate handling.
pub fn zxy_to_tile_id(z: u8, x: u32, y: u32) -> Option<u64> {
    if z > 31 {
        return None;
    }
    let n = 1u64 << u32::from(z);
    let (mut x, mut y) = (u64::from(x), u64::from(y));
    if x >= n || y >= n {
        return None;
    }

    let mut d = 0u64;
    let mut s = n >> 1;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        // Bits at and above `s` are consumed; keeping only the low bits lets
        // the reflection below stay inside the sub-square.
        x &= s - 1;
        y &= s - 1;
        if ry == 0 {
            if rx == 1 {
                x = s - 1 - x;
                y = s - 1 - y;
            }
            core::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }

    Some(zoom_base(z) + d)
}

/// PMTiles tile id → `z/x/y`, or `None` past the id space's end (zoom 31's
/// last tile).
pub fn tile_id_to_zxy(id: u64) -> Option<(u8, u32, u32)> {
    let mut base = 0u64;
    for z in 0u8..=31 {
        let count = 1u64 << (2 * u32::from(z));
        if id - base < count {
            let (x, y) = d2xy(z, id - base);
            return Some((z, x as u32, y as u32));
        }
        base += count;
    }
    None
}

/// The inverse Hilbert walk: position `d` along zoom `z`'s curve → `(x, y)`.
fn d2xy(z: u8, d: u64) -> (u64, u64) {
    let n = 1u64 << u32::from(z);
    let (mut x, mut y) = (0u64, 0u64);
    let mut t = d;
    let mut s = 1u64;
    while s < n {
        let rx = 1 & (t / 2);
        let ry = 1 & (t ^ rx);
        if ry == 0 {
            if rx == 1 {
                x = s - 1 - x;
                y = s - 1 - y;
            }
            core::mem::swap(&mut x, &mut y);
        }
        x += s * rx;
        y += s * ry;
        t /= 4;
        s <<= 1;
    }
    (x, y)
}

// ---------------------------------------------------------------------------
// Varints and directories
// ---------------------------------------------------------------------------

/// Decode one LEB128 `u64` from `bytes` at `*at`, advancing it.
fn read_varint(bytes: &[u8], at: &mut usize) -> Result<u64, IndexError> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let &byte = bytes.get(*at).ok_or(IndexError::Directory(
            "a varint ran off the directory's end",
        ))?;
        *at += 1;
        if shift >= 64 {
            return Err(IndexError::Directory("a varint overflowed 64 bits"));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
}

/// Decode one serialized directory: entry count, then four columns of varints
/// — delta-coded tile ids, run lengths, lengths, offsets — where an offset of
/// `0` means "the previous entry's offset plus its length" and any other
/// value is the offset plus one.
fn decode_directory(bytes: &[u8]) -> Result<Vec<DirEntry>, IndexError> {
    let mut at = 0usize;
    let count = read_varint(bytes, &mut at)?;
    // Each entry is at least four varint bytes, so a count past the byte
    // length is corrupt — checked before the allocation it would size.
    if count > bytes.len() as u64 {
        return Err(IndexError::Directory("more entries than bytes"));
    }
    let count = count as usize;

    let mut entries = vec![
        DirEntry {
            tile_id: 0,
            run_length: 0,
            offset: 0,
            length: 0,
        };
        count
    ];

    let mut tile_id = 0u64;
    for (i, entry) in entries.iter_mut().enumerate() {
        let delta = read_varint(bytes, &mut at)?;
        // The first id is its own delta from zero; later ids must grow, or
        // the binary search below has nothing to stand on.
        if i > 0 && delta == 0 {
            return Err(IndexError::Directory("tile ids did not strictly grow"));
        }
        tile_id = tile_id
            .checked_add(delta)
            .ok_or(IndexError::Directory("a tile id overflowed"))?;
        entry.tile_id = tile_id;
    }
    for entry in &mut entries {
        entry.run_length = read_varint(bytes, &mut at)?;
    }
    for entry in &mut entries {
        entry.length = read_varint(bytes, &mut at)?;
    }
    for i in 0..count {
        let value = read_varint(bytes, &mut at)?;
        entries[i].offset = if value == 0 {
            let previous = i
                .checked_sub(1)
                .map(|p| entries[p])
                .ok_or(IndexError::Directory("the first entry chained its offset"))?;
            previous
                .offset
                .checked_add(previous.length)
                .ok_or(IndexError::Directory("a chained offset overflowed"))?
        } else {
            value - 1
        };
    }

    if at != bytes.len() {
        // A directory is exactly its serialization; trailing bytes mean the
        // slice handed in was not one directory, and a length this module
        // computed from them would be a guess.
        return Err(IndexError::Directory("bytes trailing the last entry"));
    }

    Ok(entries)
}

/// Find the entry speaking for `tile_id`, per the spec's search: the last
/// entry at or before the id — a hit if the id falls inside its run, a leaf
/// pointer to follow if its run length is zero (a leaf covers every id from
/// its own up to the next entry's).
fn find_entry(entries: &[DirEntry], tile_id: u64) -> Option<DirEntry> {
    let after = entries.partition_point(|entry| entry.tile_id <= tile_id);
    let entry = entries[..after].last()?;
    if entry.run_length == 0 || tile_id - entry.tile_id < entry.run_length {
        Some(*entry)
    } else {
        None
    }
}

/// Undo a directory's on-disk compression. `pub(crate)` for
/// [`crate::basemap_download`], whose metadata copy is compressed the same
/// way — the header's `internal_compression` governs both.
pub(crate) fn decompress(
    bytes: Vec<u8>,
    compression: DirectoryCompression,
) -> Result<Vec<u8>, IndexError> {
    match compression {
        DirectoryCompression::None => Ok(bytes),
        DirectoryCompression::Gzip => {
            let mut plain = Vec::new();
            flate2::read::GzDecoder::new(bytes.as_slice())
                .read_to_end(&mut plain)
                .map_err(|_| IndexError::Directory("a gzip stream would not decode"))?;
            Ok(plain)
        }
    }
}

// ---------------------------------------------------------------------------
// The header
// ---------------------------------------------------------------------------

/// Parse the fixed 127-byte header off the front of an archive.
fn parse_header(bytes: &[u8]) -> Result<IndexHeader, IndexError> {
    if bytes.len() < HEADER_BYTES {
        return Err(IndexError::Truncated {
            what: "the header",
            wanted: HEADER_BYTES as u64,
            got: bytes.len() as u64,
        });
    }
    if &bytes[0..7] != MAGIC || bytes[7] != SPEC_VERSION {
        return Err(IndexError::NotPmtilesV3);
    }

    let u64_at = |offset: usize| {
        u64::from_le_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("eight bytes of a slice already length-checked"),
        )
    };

    Ok(IndexHeader {
        root_offset: u64_at(8),
        root_length: u64_at(16),
        metadata_offset: u64_at(24),
        metadata_length: u64_at(32),
        leaf_offset: u64_at(40),
        leaf_length: u64_at(48),
        tile_data_offset: u64_at(56),
        tile_data_length: u64_at(64),
        n_addressed_tiles: u64_at(72),
        n_tile_entries: u64_at(80),
        n_tile_contents: u64_at(88),
        clustered: bytes[96] == 1,
        internal_compression: match bytes[97] {
            1 => DirectoryCompression::None,
            2 => DirectoryCompression::Gzip,
            value => {
                return Err(IndexError::Unsupported {
                    what: "internal_compression",
                    value,
                });
            }
        },
        tile_compression: bytes[98],
        tile_type: bytes[99],
        min_zoom: bytes[100],
        max_zoom: bytes[101],
    })
}

// ---------------------------------------------------------------------------
// The leaves readers of one archive share
// ---------------------------------------------------------------------------

/// Decoded leaf directories carried between readers of one archive, bounded
/// at [`SHARED_LEAF_BYTES`] and least-recently-used out.
///
/// The entries are held as the same [`Arc`]s the readers hold, so a leaf in
/// both a live [`PmtIndex`] and this store is one allocation, and a leaf
/// evicted here is freed only once the last reader of it is gone.
struct SharedLeaves {
    store: Mutex<LeafStore>,
}

/// [`SharedLeaves`]' contents and what they cost, kept together so the byte
/// total can never drift from the map it counts.
struct LeafStore {
    by_offset: lru::LruCache<u64, Arc<Vec<DirEntry>>>,
    held_bytes: usize,
}

/// What one decoded leaf costs the budget: its entries, at their real size.
fn leaf_bytes(entries: &[DirEntry]) -> usize {
    core::mem::size_of_val(entries)
}

impl SharedLeaves {
    fn new() -> Self {
        Self {
            store: Mutex::new(LeafStore {
                by_offset: lru::LruCache::unbounded(),
                held_bytes: 0,
            }),
        }
    }

    /// The decoded leaf at `offset`, if it is still held.
    fn get(&self, offset: u64) -> Option<Arc<Vec<DirEntry>>> {
        self.store
            .lock()
            .expect("the shared leaf lock is never poisoned: no holder panics")
            .by_offset
            .get(&offset)
            .map(Arc::clone)
    }

    /// Offer a decoded leaf, evicting the least recently used until the
    /// budget holds. A leaf larger than the whole budget is simply not kept —
    /// storing it would evict everything to hold one entry.
    fn put(&self, offset: u64, entries: &Arc<Vec<DirEntry>>) {
        let cost = leaf_bytes(entries);
        if cost > SHARED_LEAF_BYTES {
            return;
        }
        let mut store = self
            .store
            .lock()
            .expect("the shared leaf lock is never poisoned: no holder panics");
        if let Some(replaced) = store.by_offset.put(offset, Arc::clone(entries)) {
            store.held_bytes -= leaf_bytes(&replaced);
        }
        store.held_bytes += cost;
        while store.held_bytes > SHARED_LEAF_BYTES {
            let Some((_, evicted)) = store.by_offset.pop_lru() else {
                break;
            };
            store.held_bytes -= leaf_bytes(&evicted);
        }
    }
}

/// The shared leaves for the archive `identity` names, created on first ask.
///
/// One store per archive name, on [`crate::basemap_archive::block_cache`]'s
/// registry model: a process-wide map, and the identity is the archive's own
/// (see [`RangeSource::archive_identity`] for why only a source that can
/// promise immutability answers with one).
fn shared_leaves(identity: &str) -> Arc<SharedLeaves> {
    static ARCHIVES: OnceLock<Mutex<HashMap<String, Arc<SharedLeaves>>>> = OnceLock::new();
    let archives = ARCHIVES.get_or_init(|| Mutex::new(HashMap::new()));
    Arc::clone(
        archives
            .lock()
            .expect("the shared archive registry lock is never poisoned: no holder panics")
            .entry(identity.to_owned())
            .or_insert_with(|| Arc::new(SharedLeaves::new())),
    )
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

/// A PMTiles v3 archive's directories, read a range at a time from whatever
/// source they were opened over, answering byte costs rather than tiles.
///
/// The figure is exact *after fetching the directories covering the asked-for
/// ids* — a small read, root plus the relevant leaves — and every leaf read
/// is kept, so re-measuring a moved box repays nothing it already paid.
pub struct PmtIndex<S> {
    source: S,
    header: IndexHeader,
    root: Arc<Vec<DirEntry>>,
    /// Decoded leaves by their offset in the leaf section. Never evicted for
    /// this reader's lifetime: the set one download area touches is bounded
    /// by the area itself, and a walk that has already paid for a leaf must
    /// never pay again part-way through.
    ///
    /// **Not "a few KB"** — measured on the published planet archive, one
    /// leaf decodes to 17,653 entries, 564,896 bytes; the widest area
    /// measured touched 36 of them. That is the figure [`SHARED_LEAF_BYTES`]
    /// is sized against.
    leaves: Mutex<HashMap<u64, Arc<Vec<DirEntry>>>>,
    /// Leaves this archive's *other* readers decoded, when the source can
    /// name the archive. `None` for an anonymous source — an in-memory
    /// segment being verified, or a test double — which is what keeps a
    /// reader of one archive from ever being handed another's directories.
    shared: Option<Arc<SharedLeaves>>,
}

impl<S: ArchiveRangeSource> PmtIndex<S> {
    /// Open the archive `source` addresses: read and validate the header,
    /// then read and decode the root directory.
    ///
    /// # Errors
    ///
    /// [`IndexError::Range`] if the source will not answer;
    /// [`IndexError::NotPmtilesV3`] / [`IndexError::Unsupported`] /
    /// [`IndexError::Truncated`] / [`IndexError::Directory`] if what it
    /// answers with is not an archive this reader speaks.
    pub async fn open(source: S) -> Result<Self, IndexError> {
        let header_bytes = read_exact(&source, 0, HEADER_BYTES as u64, "the header").await?;
        let header = parse_header(&header_bytes)?;

        let root_bytes = read_exact(
            &source,
            header.root_offset,
            header.root_length,
            "the root directory",
        )
        .await?;
        let root = decode_directory(&decompress(root_bytes, header.internal_compression)?)?;

        Ok(Self {
            shared: source.archive_identity().as_deref().map(shared_leaves),
            source,
            header,
            root: Arc::new(root),
            leaves: Mutex::new(HashMap::new()),
        })
    }

    /// The archive's header.
    pub fn header(&self) -> &IndexHeader {
        &self.header
    }

    /// The source the index was opened over, so a caller that has measured a
    /// set of tiles can fetch their bytes through the same seam — the
    /// download engine, which must never grow a second connection to the
    /// archive beside this one.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// Where `z/x/y`'s bytes sit in the tile-data section, or `None` for a
    /// tile the archive does not hold.
    ///
    /// # Errors
    ///
    /// [`IndexError::Coordinate`] for a coordinate off the grid; the open-time
    /// errors' directory subset for a leaf that will not read or decode.
    pub async fn tile_span(&self, z: u8, x: u32, y: u32) -> Result<Option<TileSpan>, IndexError> {
        let id = zxy_to_tile_id(z, x, y).ok_or(IndexError::Coordinate { z, x, y })?;
        self.span_for_id(id).await
    }

    /// [`Self::tile_span`] by tile id.
    pub async fn span_for_id(&self, tile_id: u64) -> Result<Option<TileSpan>, IndexError> {
        let mut directory = Arc::clone(&self.root);
        for _ in 0..MAX_DIRECTORY_DEPTH {
            let Some(entry) = find_entry(&directory, tile_id) else {
                return Ok(None);
            };
            if entry.run_length > 0 {
                return Ok(Some(TileSpan {
                    offset: entry.offset,
                    length: entry.length,
                }));
            }
            directory = self.leaf(entry.offset, entry.length).await?;
        }
        Err(IndexError::Directory(
            "directories nested past any real archive's depth",
        ))
    }

    /// The exact bytes downloading `tiles` would transfer.
    ///
    /// Distinct `(offset, length)` pairs, counted once each — see the module
    /// doc for why entries would double-count. Absent tiles are counted, not
    /// errors.
    ///
    /// # Errors
    ///
    /// As [`Self::tile_span`], on the first tile that fails.
    pub async fn download_bytes<I>(&self, tiles: I) -> Result<DownloadBytes, IndexError>
    where
        I: IntoIterator<Item = (u8, u32, u32)>,
    {
        let tiles: Vec<(u8, u32, u32)> = tiles.into_iter().collect();
        // The directory round trips, overlapped, before the walk asks for
        // them one at a time. A coordinate off the grid is skipped here and
        // still errors below.
        self.warm_leaves(
            tiles
                .iter()
                .filter_map(|&(z, x, y)| zxy_to_tile_id(z, x, y)),
        )
        .await;

        let mut spans = HashSet::new();
        let mut total = DownloadBytes::default();
        for (z, x, y) in tiles {
            match self.tile_span(z, x, y).await? {
                Some(span) => {
                    total.present += 1;
                    if spans.insert(span) {
                        total.bytes += span.length;
                    }
                }
                None => total.absent += 1,
            }
        }
        Ok(total)
    }

    /// Every tile entry in the archive — the root's and every leaf's, leaf
    /// pointers walked rather than returned. Runs are *not* expanded: the sum
    /// of `run_length` over the answer is the archive's addressed-tile count.
    ///
    /// # Errors
    ///
    /// As [`Self::span_for_id`], on the first leaf that fails.
    pub async fn tile_entries(&self) -> Result<Vec<DirEntry>, IndexError> {
        let mut tiles = Vec::new();
        let mut directories = vec![(Arc::clone(&self.root), 0usize)];
        while let Some((directory, depth)) = directories.pop() {
            for entry in directory.iter() {
                if entry.run_length > 0 {
                    tiles.push(*entry);
                } else if depth + 1 < MAX_DIRECTORY_DEPTH {
                    directories.push((self.leaf(entry.offset, entry.length).await?, depth + 1));
                } else {
                    return Err(IndexError::Directory(
                        "directories nested past any real archive's depth",
                    ));
                }
            }
        }
        Ok(tiles)
    }

    /// Read every leaf directory the walk over `tile_ids` will need, up to
    /// [`MAX_INFLIGHT_LEAF_READS`] reads at a time.
    ///
    /// **A prefetch and nothing else.** It resolves no span, returns no
    /// answer and reports no error: a leaf that will not read is left
    /// uncached, and the walk that needs it fails exactly where and with
    /// exactly what it would have failed with had this never been called. So
    /// the plan a caller builds afterwards is the same plan, tile for tile
    /// and error for error, whatever order the reads land in — which is what
    /// resume depends on, since a resume is a set difference over segments
    /// the *previous* run cut.
    ///
    /// Ids that resolve straight out of the root cost nothing here; an
    /// archive with no leaves at all reads nothing.
    pub async fn warm_leaves<I>(&self, tile_ids: I)
    where
        I: IntoIterator<Item = u64>,
    {
        // Ids still walking, each paired with the directory it is walking in.
        let mut walking: Vec<(u64, Arc<Vec<DirEntry>>)> = tile_ids
            .into_iter()
            .map(|id| (id, Arc::clone(&self.root)))
            .collect();

        for _ in 0..MAX_DIRECTORY_DEPTH {
            // The leaves this hop needs, deduplicated. The dedup is what
            // makes the burst the size of the *distinct* leaf set rather than
            // of the tile set.
            let mut seen = HashSet::new();
            let wanted: Vec<(u64, u64)> = walking
                .iter()
                .filter_map(|(id, directory)| find_entry(directory, *id))
                .filter(|entry| entry.run_length == 0 && seen.insert(entry.offset))
                .map(|entry| (entry.offset, entry.length))
                .collect();
            if wanted.is_empty() {
                return;
            }

            futures::stream::iter(wanted.into_iter().map(|(offset, length)| async move {
                // Dropped on purpose: see the "prefetch and nothing else"
                // paragraph above. The walk re-reads and reports it.
                let _ = self.leaf(offset, length).await;
            }))
            .buffer_unordered(MAX_INFLIGHT_LEAF_READS)
            .for_each(|()| async {})
            .await;

            // Advance the ids that landed in a leaf this hop; the rest are
            // answered, absent, or on a leaf that would not read.
            let cached = self
                .leaves
                .lock()
                .expect("the leaf cache lock is never poisoned: no holder panics");
            walking = walking
                .iter()
                .filter_map(|(id, directory)| {
                    let entry = find_entry(directory, *id)?;
                    (entry.run_length == 0)
                        .then(|| cached.get(&entry.offset))
                        .flatten()
                        .map(|leaf| (*id, Arc::clone(leaf)))
                })
                .collect();
            drop(cached);
        }
    }

    /// The decoded leaf at `offset` in the leaf section, from this reader's
    /// cache, from what another reader of the same archive already decoded,
    /// or from the source.
    async fn leaf(&self, offset: u64, length: u64) -> Result<Arc<Vec<DirEntry>>, IndexError> {
        if let Some(hit) = self
            .leaves
            .lock()
            .expect("the leaf cache lock is never poisoned: no holder panics")
            .get(&offset)
        {
            return Ok(Arc::clone(hit));
        }

        if let Some(hit) = self.shared.as_ref().and_then(|shared| shared.get(offset)) {
            self.leaves
                .lock()
                .expect("the leaf cache lock is never poisoned: no holder panics")
                .insert(offset, Arc::clone(&hit));
            return Ok(hit);
        }

        let bytes = read_exact(
            &self.source,
            self.header.leaf_offset + offset,
            length,
            "a leaf directory",
        )
        .await?;
        let entries = Arc::new(decode_directory(&decompress(
            bytes,
            self.header.internal_compression,
        )?)?);

        if let Some(shared) = &self.shared {
            shared.put(offset, &entries);
        }
        self.leaves
            .lock()
            .expect("the leaf cache lock is never poisoned: no holder panics")
            .insert(offset, Arc::clone(&entries));
        Ok(entries)
    }
}

/// Read exactly `length` bytes at `offset`, where the structure knows its own
/// size — which everything in a PMTiles archive does. [`RangeSource`] reads
/// *up to* the asked length, so short is [`IndexError::Truncated`] here and an
/// over-answering source is clamped.
async fn read_exact<S: RangeSource>(
    source: &S,
    offset: u64,
    length: u64,
    what: &'static str,
) -> Result<Vec<u8>, IndexError> {
    let wanted = usize::try_from(length).map_err(|_| IndexError::Truncated {
        what,
        wanted: length,
        got: 0,
    })?;
    let mut bytes = source.read_range(offset, wanted).await?;
    if bytes.len() < wanted {
        return Err(IndexError::Truncated {
            what,
            wanted: length,
            got: bytes.len() as u64,
        });
    }
    bytes.truncate(wanted);
    Ok(bytes)
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests;
