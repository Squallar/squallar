//! The offline-area download engine and its segmented writer.
//!
//! Turns an area — a bbox, a detail zoom, an opaque id — into ~[`SEGMENT_BYTES`]
//! standalone `.pmtiles` files, each carrying its own header, directories and
//! metadata, so no segment references the parent archive's offsets and a new
//! archive generation cannot invalidate a byte of what was downloaded.
//!
//! # The seams it rides, and the one it must not
//!
//! Tile enumeration is `squallar_geo`'s slippy math; what a tile *costs* and
//! where its bytes sit come from [`crate::pmt_index`] — distinct
//! `(offset, length)` pairs, never `tile_count × average`; the bytes travel the
//! same [`RangeSource`] the live reader uses. **Never through
//! [`crate::tile_source::HttpsTiles`]**: its request channel is bounded at 6
//! and its LRU is the live map's working set, so a bulk pull through it would
//! evict what the user is looking at to fetch what they are not.
//!
//! # Segments, because mid-file resume does not exist
//!
//! `PmTilesStreamWriter` holds its state in memory and cannot be reopened, so
//! a half-written archive cannot be continued. Each segment is therefore a
//! complete archive on its own: cancellation keeps the finished ones, resume
//! is a **set difference over segments** ([`DownloadPlan`] minus
//! `existing_segments`), never a byte offset — and the engine never resumes on
//! its own; a fresh [`BasemapDownload::start`] on a half-done area completes
//! the difference because finished segments are simply not re-fetched. The cap
//! also bounds peak memory: a segment is built in a `Cursor<Vec<u8>>` and must
//! fit the heap, which is what fixes the web ceiling.
//!
//! Tile bytes go in via `add_raw_tile`, so the local bytes are the remote
//! bytes verbatim — no decode, no recompress, and the writer reproduces both
//! of the format's dedup mechanisms itself.
//!
//! # Completeness is recomputed, never stored
//!
//! There is no persisted "done" flag anywhere in this module. An area is
//! complete iff every planned segment is present in the store, recomputed from
//! the store's own listing; the native store publishes by rename
//! (`.part` → `.pmtiles`, the `write_blob` discipline) and a `.part` is never
//! listed, so a crash at any instant leaves either a listed complete segment
//! or nothing. A finished segment is additionally reopened through
//! [`crate::pmt_index`] and checked to address every tile it was asked to
//! hold, *before* it is published.
//!
//! # Failure is typed, not boolean
//!
//! [`DownloadOutcome`] is `Complete` / `Partial` / `Failed`, and a `Partial`
//! carries its counts so a UI cannot render it as done. Segments fail
//! independently: one bad range does not abandon the segments after it.
//!
//! # Cancellation is drop
//!
//! The engine owns the same per-target IO runtime the tile fetch task uses
//! (`tile_source::runtime`), and for the same reason dropping it is the whole
//! cancellation story: the runtime parks its thread on a quit channel so a
//! drop cancels an in-flight request outright rather than waiting it out.
//! There is no cancel protocol to hold wrong.
//!
//! # Every figure names its denominator
//!
//! Every byte figure this module reports counts **tile bytes, each distinct
//! `(offset, length)` pair once** — the same figure [`crate::pmt_index`]
//! quotes, which is what makes "the download transferred what was quoted" an
//! equality a test can assert. Bytes read through a coalescing gap
//! ([`COALESCE_GAP_BYTES`]) are discarded and appear in no figure.
//! [`DownloadProgress`] states per field whether its denominator is the whole
//! area or this run's remaining work.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

use egui::Context;
use pmtiles::{Compression, PmTilesWriter, TileCoord, TileType};
use squallar_geo::{lat_to_tile_y, lon_to_tile_x};
use squallar_units::DataSize;

use crate::basemap_archive::{ArchiveRangeSource, RangeError, RangeSource};
use crate::pmt_index::{IndexError, IndexHeader, PmtIndex, TileSpan, decompress, zxy_to_tile_id};
use crate::tile_source::runtime;

/// The soft cap on one segment's tile bytes.
///
/// Decimal, like every user-facing size in this workspace ([`DataSize`]'s
/// denominator). A cap on **tile bytes**: the finished artifact adds its
/// header, directories and metadata copy on top. Soft in exactly one case — a
/// single tile larger than the cap becomes a segment on its own, because a
/// tile cannot be split. The figure bounds what a segment build holds in
/// memory, which is what makes the download viable on the wasm heap.
///
/// **Measured, native**: one Monaco segment of 384,371 tile bytes peaked at
/// 908,659 engine-held bytes — fetched spans plus the finalized artifact's
/// buffer, the engine's own ledger, not process heap
/// (`one_segment_peak_held_is_measured_and_reported`). **The wasm figure is
/// OWED**: the engine is not reachable from the web build until the read-back
/// and store steps land, so the browser rig cannot drive it yet, and per
/// "always measure, never scale" the native figure is not extrapolated to a
/// wasm ceiling here.
pub const SEGMENT_BYTES: u64 = 16_000_000;

/// Ranges closer than this are fetched as one request and the gap discarded.
///
/// A skipped gap this small is cheaper to read through than to pay another
/// HTTP round trip for. Gap bytes appear in **no** reported figure — see the
/// module doc's denominator rule.
pub const COALESCE_GAP_BYTES: u64 = 4096;

/// The synthetic same-origin path the web store lives under.
///
/// **A fixed contract with the service worker**, which routes every request
/// under this path (`squallar-web/sw.js`): `PUT {base}/{area_id}/{seg}.pmtiles`
/// stores a segment, `GET` with a `Range` header answers `206` with an exact
/// `Content-Range`, `DELETE {base}/{area_id}/` removes an area, and
/// `GET {base}/__list__` answers a JSON array of `{url, bytes}` for every
/// stored segment — the listing launch-time completeness is recomputed from.
pub const OFFLINE_BASE_PATH: &str = "__squallar_offline__";

// ---------------------------------------------------------------------------
// The area and its tiles
// ---------------------------------------------------------------------------

/// What the user asked to make available offline.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaSpec {
    /// The caller's opaque identity for the area. It names files and URLs, so
    /// it must be a filename- and URL-safe token — ASCII alphanumerics, `-`,
    /// `_`, `.`, not starting with `.` — and the stores refuse anything else
    /// rather than let an id traverse a path.
    pub area_id: String,
    /// Western edge, degrees longitude. The bbox is not wrapped across the
    /// antimeridian — `lon_to_tile_x` clamps, matching the map's own behaviour.
    pub west: f64,
    /// Southern edge, degrees latitude.
    pub south: f64,
    /// Eastern edge, degrees longitude.
    pub east: f64,
    /// Northern edge, degrees latitude.
    pub north: f64,
    /// The deepest zoom to store, `z0..=this`. Callers read it off the
    /// archive's own header rather than hardcoding 14, so the offline ceiling
    /// and the render ceiling stay one number.
    pub max_zoom: u8,
}

/// Every tile id the area covers, `z0..=max_zoom`, via the same slippy math
/// the renderer enumerates with — including its high-latitude `asinh` form, so
/// a polar area stores the tiles the map will ask for rather than a set off by
/// one at the edges.
pub fn area_tiles(area: &AreaSpec) -> Vec<(u8, u32, u32)> {
    area_tiles_to(area, area.max_zoom)
}

/// [`area_tiles`] against an archive whose **own** header stops shallower than
/// the area asks for.
///
/// The area's depth is chosen against the base map, and the two archives an
/// area holds do not share a ceiling — the terrain build stops above the
/// basemap's. Enumerating past an archive's own `max_zoom` would ask its
/// directories for tiles it declares it does not have: every one absent, every
/// one a lookup, and the exact figure and the download would still agree only
/// by both wasting the same walk.
///
/// **The ceiling is always read off the archive's header** — `plan_area` and
/// the size probe both pass `index.header().max_zoom` — so nothing here or
/// above it names a depth. Over-zoom past the shallower ceiling is unchanged:
/// the composition publishes the live archive's maximum
/// (`BasemapArchives::max_zoom`) and the renderer stretches the deepest stored
/// tile exactly as it does online, which is why there is no copy about this.
pub fn area_tiles_to(area: &AreaSpec, archive_max_zoom: u8) -> Vec<(u8, u32, u32)> {
    let mut tiles = Vec::new();
    for z in 0..=area.max_zoom.min(archive_max_zoom) {
        let x_range = lon_to_tile_x(area.west, z)..=lon_to_tile_x(area.east, z);
        let y_range = lat_to_tile_y(area.north, z)..=lat_to_tile_y(area.south, z);
        for x in x_range {
            for y in y_range.clone() {
                tiles.push((z, x, y));
            }
        }
    }
    tiles
}

// ---------------------------------------------------------------------------
// The two archives an area holds
// ---------------------------------------------------------------------------

/// Which archive a set of an area's segments was cut from.
///
/// An area is one rectangle over **two** archives — the vector base map and
/// the terrain hillshade — and they are separate PMTiles files with separate
/// generations, separate ceilings and separate body types. So a downloaded
/// area holds one segment set per archive, namespaced apart in the store by
/// [`Self::artifact_infix`], and either may be absent without the other
/// noticing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AreaArchive {
    /// The vector base map. Every area has this half.
    Basemap,
    /// The terrain hillshade. Opt-in per download, so an area may have none.
    Terrain,
}

impl AreaArchive {
    /// What a stored artifact carries between its segment number and
    /// [`SEGMENT_SUFFIX`].
    ///
    /// **Empty for the base map, and that is the whole of the migration**: an
    /// artifact published before terrain existed is named exactly as this
    /// still names one, so a device's stored areas read back unchanged.
    ///
    /// The two namespaces cannot be confused in either direction, by
    /// construction rather than by care. A terrain artifact
    /// (`{id}.{seg}.terrain.pmtiles`) read as a basemap one leaves
    /// `"{seg}.terrain"` where a segment number goes, which does not parse; and
    /// a basemap artifact can never end in the terrain infix, because what sits
    /// before `.pmtiles` is digits.
    fn artifact_infix(self) -> &'static str {
        match self {
            Self::Basemap => "",
            Self::Terrain => ".terrain",
        }
    }

    /// Which archive the artifact named `name` belongs to — the one reading of
    /// the naming rule above, used by both stores' read-back.
    fn of_artifact(name: &str) -> Self {
        let stem = name.strip_suffix(SEGMENT_SUFFIX).unwrap_or(name);
        if stem.ends_with(Self::Terrain.artifact_infix()) {
            Self::Terrain
        } else {
            Self::Basemap
        }
    }
}

// ---------------------------------------------------------------------------
// The persisted record
// ---------------------------------------------------------------------------

/// One area this device has made available offline, as the app carries it
/// between the store and the persisted list.
///
/// **The record says what was REQUESTED.** Nothing in it asserts that the
/// bytes are still there: `segments_expected` is the cut the plan made, and
/// whether the store still holds that many is [`DownloadedArea::reconcile`]'s
/// answer, recomputed from the store's own listing at every launch. There is
/// no completeness flag here to go stale — which is what makes the named
/// silent-partial-success defect structurally impossible rather than guarded
/// against.
///
/// It carries an [`AreaSpec`] whole rather than re-spelling its fields, so a
/// persisted area is startable: handing the spec back to
/// [`BasemapDownload::start`] resumes it as the set difference over segments.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadedArea {
    /// What was asked for: the id, the bbox and the detail ceiling.
    pub spec: AreaSpec,
    /// Segments the plan cut the area into. **Requested, not present** — see
    /// the type doc.
    pub segments_expected: u32,
    /// Tile bytes the finished area holds: distinct `(offset, length)` pairs
    /// once each, the module's one byte denominator.
    pub bytes: DataSize,
    /// The archive generation the area was cut from, in
    /// `basemap_archive::block_cache::generation_for_url`'s spelling — the one
    /// derivation of a generation from an archive URL, so this and the block
    /// cache cannot come to two answers about which archive a byte came from.
    ///
    /// A sub-archive carries its own header and directories and stays valid
    /// forever, so this never expires anything; it is what lets a later step
    /// state "Downloaded September 2026 · update available" as a fact the user
    /// may act on, never as a warning and never as an automatic re-download.
    pub generation: String,
    /// The terrain half, or `None` for an area that holds only the base map.
    ///
    /// **`None` is the honest reading of a record written before terrain was
    /// downloadable**: such an area holds no hillshade, which is exactly what
    /// this says. Nothing migrates and nothing is inferred.
    pub terrain: Option<TerrainHold>,
}

/// What an area's terrain half holds — its own cut, its own bytes, its own
/// generation.
///
/// Separate from the basemap figures rather than folded into them because the
/// two archives are reconciled against the store independently: a terrain
/// segment set that went missing must not read as basemap segments missing,
/// and the terrain build has its own release cadence, so one generation string
/// could never date both.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainHold {
    /// Segments the terrain plan cut this area into. **Requested, not
    /// present** — [`DownloadedArea`]'s rule, for its reason.
    pub segments_expected: u32,
    /// Terrain tile bytes, the module's one byte denominator.
    pub bytes: DataSize,
    /// The terrain archive generation these segments were cut from, in
    /// `generation_for_url`'s spelling.
    pub generation: String,
}

impl DownloadedArea {
    /// The record for a finished download, or `None` for one that did not
    /// finish.
    ///
    /// **This is the whole of layer 1 against silent partial success**: a
    /// `Partial` or `Failed` run yields no record, so the area never reaches
    /// the persisted list and never draws as one the device has. The counts
    /// come off the outcome itself rather than from a caller, so a record
    /// cannot disagree with the run that produced it.
    pub fn from_outcome(
        spec: AreaSpec,
        generation: String,
        outcome: &DownloadOutcome,
    ) -> Option<Self> {
        let DownloadOutcome::Complete { bytes, segments } = outcome else {
            return None;
        };
        Some(Self {
            spec,
            segments_expected: *segments,
            bytes: DataSize::from_bytes(*bytes),
            generation,
            terrain: None,
        })
    }

    /// The record for a finished run over **both** archives, or `None` for one
    /// that did not finish.
    ///
    /// [`Self::from_outcome`]'s rule — only `Complete` yields a record — with
    /// the run's per-archive cut supplied, because the two halves are
    /// reconciled independently and the outcome carries only the totals.
    /// `terrain_generation` dates the terrain half; it is `None` exactly when
    /// the run had no terrain source, which is what makes a basemap-only area
    /// say so rather than claim a hillshade it never fetched.
    pub fn from_run(
        spec: AreaSpec,
        generation: String,
        terrain_generation: Option<String>,
        outcome: &DownloadOutcome,
        holdings: &AreaHoldings,
    ) -> Option<Self> {
        if !matches!(outcome, DownloadOutcome::Complete { .. }) {
            return None;
        }
        Some(Self {
            spec,
            segments_expected: holdings.basemap.segments,
            bytes: DataSize::from_bytes(holdings.bytes()),
            generation,
            terrain: holdings
                .terrain
                .as_ref()
                .zip(terrain_generation)
                .map(|(hold, generation)| TerrainHold {
                    segments_expected: hold.segments,
                    bytes: DataSize::from_bytes(hold.bytes),
                    generation,
                }),
        })
    }

    /// This area's **basemap** segments against what `present` holds — the
    /// store's own listing, from [`SegmentStore::existing_segments`].
    ///
    /// Segments at or past `segments_expected` are **not** counted: a store
    /// left holding a longer cut from an earlier plan would otherwise read as
    /// more complete than the area it is being reconciled against.
    pub fn reconcile(&self, present: &BTreeSet<u32>) -> AreaStatus {
        Self::against(self.segments_expected, present)
    }

    /// This area's **terrain** segments against what `present` holds, or
    /// `None` for an area that holds no terrain — which is not the same as one
    /// whose terrain is missing, and must never draw as it.
    pub fn reconcile_terrain(&self, present: &BTreeSet<u32>) -> Option<AreaStatus> {
        let hold = self.terrain.as_ref()?;
        Some(Self::against(hold.segments_expected, present))
    }

    /// Both halves as one status: **every** segment this area asked for,
    /// against every one the store holds of it.
    ///
    /// One pair rather than two, because the manage screen has one slot for
    /// how much of an area is here and a second row per archive would be
    /// spending the user's attention on which file a byte came out of. A
    /// half-held terrain therefore reads as a half-held area, which is what it
    /// is: the rectangle does not draw as it was downloaded.
    pub fn reconcile_all(&self, basemap: &BTreeSet<u32>, terrain: &BTreeSet<u32>) -> AreaStatus {
        let basemap = self.reconcile(basemap);
        match self.reconcile_terrain(terrain) {
            Some(terrain) => AreaStatus {
                present: basemap.present.saturating_add(terrain.present),
                expected: basemap.expected.saturating_add(terrain.expected),
            },
            None => basemap,
        }
    }

    /// `present` counted against a cut of `expected` segments — the one
    /// reading both halves are reconciled by.
    fn against(expected: u32, present: &BTreeSet<u32>) -> AreaStatus {
        let present = present.iter().filter(|&&seg| seg < expected).count();
        AreaStatus {
            present: u32::try_from(present).unwrap_or(u32::MAX),
            expected,
        }
    }
}

/// How much of a persisted area the store actually holds, both figures
/// against the same denominator: the area's own segment cut.
///
/// Two numbers rather than a boolean, for the reason [`DownloadOutcome`]'s
/// `Partial` carries its counts: a screen that can only ask "done?" has
/// nowhere to put a half-held area but beside a finished one.
///
/// **These two never reach the glass.** Segments are an implementation fact
/// the user has no way to picture, so the manage screen spends them as a
/// held-of-asked byte pair instead — where "held" is the stored artifacts'
/// size (see [`SegmentStore`]'s denominator note) and "asked" is the tile-byte
/// total the download quoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AreaStatus {
    /// Segments of this area's cut the store holds complete.
    pub present: u32,
    /// Segments the area was cut into.
    pub expected: u32,
}

impl AreaStatus {
    /// Whether every segment the area asked for is in the store.
    ///
    /// An area expecting nothing is **not** complete: a zero denominator would
    /// otherwise make the emptiest possible record the most confident one.
    pub fn is_complete(self) -> bool {
        self.expected > 0 && self.present >= self.expected
    }
}

/// [`DownloadedArea::reconcile_all`] against a live store — the launch-time
/// question, asked of the store rather than of the record.
///
/// The terrain listing is asked for only when the record says the area has a
/// terrain half: a basemap-only area must cost no listing for one, and a store
/// that answered about a namespace nothing wrote would answer empty anyway.
pub async fn area_status<St: SegmentStore>(
    store: &St,
    area: &DownloadedArea,
) -> Result<AreaStatus, StoreError> {
    let basemap = store
        .existing_segments(&area.spec.area_id, AreaArchive::Basemap)
        .await?;
    let terrain = match area.terrain {
        Some(_) => {
            store
                .existing_segments(&area.spec.area_id, AreaArchive::Terrain)
                .await?
        }
        None => BTreeSet::new(),
    };
    Ok(area.reconcile_all(&basemap, &terrain))
}

// ---------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------

/// One tile a segment will hold: its coordinate and where its bytes sit.
#[derive(Debug, Clone, Copy)]
struct PlannedTile {
    z: u8,
    x: u32,
    y: u32,
    span: TileSpan,
}

/// One segment of the plan: which tiles, and what their distinct spans cost.
#[derive(Debug, Clone)]
pub struct PlannedSegment {
    /// The segment's number, dense from zero. It names the artifact
    /// (`{area_id}.{seg}.pmtiles`), so the plan being a pure function of the
    /// area and the archive is what lets two engine runs agree on which
    /// segments exist.
    pub seg: u32,
    /// Distinct span bytes within this segment — what building it transfers
    /// when nothing is carried over from an earlier segment.
    pub tile_bytes: u64,
    /// The tiles, in ascending tile-id order — the order the writer wants and
    /// the order that makes ranges coalesce on a clustered archive.
    tiles: Vec<PlannedTile>,
}

impl PlannedSegment {
    /// How many tiles this segment holds.
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// The tiles this segment holds, as coordinates.
    pub fn tile_coords(&self) -> impl Iterator<Item = (u8, u32, u32)> + '_ {
        self.tiles.iter().map(|tile| (tile.z, tile.x, tile.y))
    }
}

/// The whole area, cut into segments, with its exact cost.
#[derive(Debug, Clone)]
pub struct DownloadPlan {
    /// The segments, in build order.
    pub segments: Vec<PlannedSegment>,
    /// Exact bytes a fresh download of the whole plan transfers: each distinct
    /// `(offset, length)` pair once, across all segments — the same figure
    /// [`PmtIndex::download_bytes`] answers for the same tiles.
    pub fetch_bytes: u64,
    /// Tiles the archive holds.
    pub present_tiles: u64,
    /// Tiles the archive does not hold. Costs nothing and is not an error —
    /// an area's corners falling off a regional archive's edge is ordinary.
    pub absent_tiles: u64,
}

/// Cut `area` into segments of at most `segment_bytes` distinct tile bytes.
///
/// Deterministic given the area and the archive: tiles are packed in tile-id
/// order, greedily, deduplicating spans within each segment as it is packed.
///
/// The enumeration stops at the **archive's own** `max_zoom`
/// ([`area_tiles_to`]), never at the area's alone: the two archives an area
/// holds do not share a ceiling, and the terrain build's is the shallower.
///
/// # Errors
///
/// As [`PmtIndex::tile_span`], on the first tile that fails.
pub async fn plan_area<S: ArchiveRangeSource>(
    index: &PmtIndex<S>,
    area: &AreaSpec,
    segment_bytes: u64,
) -> Result<DownloadPlan, IndexError> {
    let mut present: Vec<(u64, PlannedTile)> = Vec::new();
    let mut absent_tiles = 0u64;
    let mut distinct = HashSet::new();
    let mut fetch_bytes = 0u64;

    let tiles = area_tiles_to(area, index.header().max_zoom);
    // The area's directory round trips, overlapped, before the walk below
    // asks for them one at a time. A prefetch only: it changes when the
    // bytes arrive, never which spans the walk resolves, so the plan stays
    // the pure function of the area and the archive that resume depends on.
    index
        .warm_leaves(
            tiles
                .iter()
                .filter_map(|&(z, x, y)| zxy_to_tile_id(z, x, y)),
        )
        .await;

    for (z, x, y) in tiles {
        match index.tile_span(z, x, y).await? {
            Some(span) => {
                let id = zxy_to_tile_id(z, x, y).expect("tile_span validated the coordinate");
                present.push((id, PlannedTile { z, x, y, span }));
                if distinct.insert(span) {
                    fetch_bytes += span.length;
                }
            }
            None => absent_tiles += 1,
        }
    }
    present.sort_unstable_by_key(|&(id, _)| id);

    let mut segments: Vec<PlannedSegment> = Vec::new();
    let mut tiles: Vec<PlannedTile> = Vec::new();
    let mut spans = HashSet::new();
    let mut tile_bytes = 0u64;
    for (_, tile) in present {
        let addition = if spans.contains(&tile.span) {
            0
        } else {
            tile.span.length
        };
        if !tiles.is_empty() && tile_bytes + addition > segment_bytes {
            segments.push(PlannedSegment {
                seg: segments.len() as u32,
                tile_bytes,
                tiles: std::mem::take(&mut tiles),
            });
            spans.clear();
            tile_bytes = 0;
        }
        if spans.insert(tile.span) {
            tile_bytes += tile.span.length;
        }
        tiles.push(tile);
    }
    if !tiles.is_empty() {
        segments.push(PlannedSegment {
            seg: segments.len() as u32,
            tile_bytes,
            tiles,
        });
    }

    Ok(DownloadPlan {
        present_tiles: segments.iter().map(|s| s.tiles.len() as u64).sum(),
        segments,
        fetch_bytes,
        absent_tiles,
    })
}

// ---------------------------------------------------------------------------
// Range coalescing
// ---------------------------------------------------------------------------

/// One request's worth of the tile-data section: a contiguous read covering
/// every span in `spans`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FetchRange {
    /// Offset relative to the tile-data section.
    start: u64,
    /// Bytes to read.
    length: u64,
    /// The spans this read carves its bytes into.
    spans: Vec<TileSpan>,
}

/// Merge spans (sorted by offset) into reads, joining across gaps up to
/// [`COALESCE_GAP_BYTES`].
fn coalesce(sorted: &[TileSpan]) -> Vec<FetchRange> {
    let mut ranges: Vec<FetchRange> = Vec::new();
    for &span in sorted {
        if let Some(last) = ranges.last_mut()
            && span.offset <= last.start + last.length + COALESCE_GAP_BYTES
        {
            let end = (last.start + last.length).max(span.offset + span.length);
            last.length = end - last.start;
            last.spans.push(span);
            continue;
        }
        ranges.push(FetchRange {
            start: span.offset,
            length: span.length,
            spans: vec![span],
        });
    }
    ranges
}

// ---------------------------------------------------------------------------
// The stores
// ---------------------------------------------------------------------------

/// What went wrong publishing to or listing a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// A filesystem operation failed.
    Io(String),
    /// The store could not be reached.
    Transport(String),
    /// The store answered, with a status that is not success.
    Status {
        /// What was being done.
        action: &'static str,
        /// The status that came back.
        status: u16,
    },
    /// The store's listing would not parse.
    Listing(String),
    /// The area id is not a safe token — see [`AreaSpec::area_id`].
    AreaId(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "the segment store failed: {message}"),
            Self::Transport(message) => write!(f, "the segment store is unreachable: {message}"),
            Self::Status { action, status } => {
                write!(f, "the segment store answered {status} to {action}")
            }
            Self::Listing(message) => {
                write!(f, "the segment listing would not parse: {message}")
            }
            Self::AreaId(id) => write!(
                f,
                "{id:?} is not a safe area id (ASCII alphanumerics, '-', '_', '.', \
                 not starting with '.')"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

/// Refuse an id that could traverse a path or hide as a dotfile. Both stores
/// call this before building any name from the id, and a persisted record is
/// validated against it on the way back in — the one spelling of the rule.
pub fn valid_area_id(area_id: &str) -> Result<(), StoreError> {
    let safe = !area_id.is_empty()
        && !area_id.starts_with('.')
        && area_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if safe {
        Ok(())
    } else {
        Err(StoreError::AreaId(area_id.to_owned()))
    }
}

/// Where finished segments are published and existing ones are listed.
///
/// The bound splits per target exactly as [`crate::tile_source::AsyncTileSource`]
/// does, and for the same reason: on native the engine's task runs on its own
/// thread and everything it holds must be `Send + Sync`; on wasm it runs on
/// the page, where `reqwest`'s types are neither. Nothing external demands
/// `Send` of the wasm arm — the engine is the only caller — so no channel hop
/// is needed here, unlike [`crate::basemap_archive`]'s backend.
///
/// `publish` must be atomic: after it, `existing_segments` lists the segment
/// complete; before or during it, the segment is absent. That property is what
/// makes launch-time completeness a recomputable fact rather than a flag.
///
/// # The listing's byte denominator is the artifact's, not the plan's
///
/// [`Self::existing_segment_bytes`] reports what each stored segment
/// **occupies** — its tile data plus its own header, directories and metadata
/// copy — which is not the module's tile-byte denominator and is a little
/// larger than it per segment. It is the only held figure a store can answer
/// without re-planning the area against the live archive, and re-planning is a
/// network walk of every tile in the area. See
/// [`AreaStatus`] for where the difference lands on the glass.
/// # One area, two namespaces
///
/// Every method but [`Self::remove_area`] names an [`AreaArchive`] as well as
/// an area, because an area's two archives are two independent segment sets
/// that must not be able to collide, be counted together or be read as one
/// another. `remove_area` names no archive on purpose: deleting an area
/// deletes the rectangle, which is both halves and any artifact of them.
#[cfg(not(target_arch = "wasm32"))]
pub trait SegmentStore: Send + Sync + 'static {
    /// Store one finished segment's bytes, atomically.
    fn publish(
        &self,
        area_id: &str,
        archive: AreaArchive,
        seg: u32,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;
    /// The segments the store holds *complete* for `area_id`'s `archive` half,
    /// each with the bytes its artifact occupies. See the trait doc for the
    /// denominator.
    fn existing_segment_bytes(
        &self,
        area_id: &str,
        archive: AreaArchive,
    ) -> impl Future<Output = Result<BTreeMap<u32, u64>, StoreError>> + Send;
    /// The segments the store holds *complete* for `area_id`'s `archive` half.
    ///
    /// Derived from [`Self::existing_segment_bytes`] and never separately
    /// implemented, so the count and the sizes cannot come to two answers
    /// about what is here.
    fn existing_segments(
        &self,
        area_id: &str,
        archive: AreaArchive,
    ) -> impl Future<Output = Result<BTreeSet<u32>, StoreError>> + Send {
        async move {
            Ok(self
                .existing_segment_bytes(area_id, archive)
                .await?
                .into_keys()
                .collect())
        }
    }
    /// Remove every artifact of `area_id`, finished or not, **both archives**.
    fn remove_area(&self, area_id: &str) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// See the native arm above; this one drops `Send + Sync` because the wasm
/// task runs on the page thread.
#[cfg(target_arch = "wasm32")]
pub trait SegmentStore: 'static {
    /// Store one finished segment's bytes, atomically.
    fn publish(
        &self,
        area_id: &str,
        archive: AreaArchive,
        seg: u32,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// See the native arm above.
    fn existing_segment_bytes(
        &self,
        area_id: &str,
        archive: AreaArchive,
    ) -> impl Future<Output = Result<BTreeMap<u32, u64>, StoreError>>;
    /// See the native arm above.
    fn existing_segments(
        &self,
        area_id: &str,
        archive: AreaArchive,
    ) -> impl Future<Output = Result<BTreeSet<u32>, StoreError>> {
        async move {
            Ok(self
                .existing_segment_bytes(area_id, archive)
                .await?
                .into_keys()
                .collect())
        }
    }
    /// Remove every artifact of `area_id`, finished or not, **both archives**.
    fn remove_area(&self, area_id: &str) -> impl Future<Output = Result<(), StoreError>>;
}

/// The store this target publishes through — the `cfg` selecting a type
/// alias, never a branch: plain files on native, the service-worker route on
/// web.
#[cfg(not(target_arch = "wasm32"))]
pub type PlatformSegmentStore = FsSegmentStore;
/// See the native arm above.
#[cfg(target_arch = "wasm32")]
pub type PlatformSegmentStore = HttpSegmentStore;

/// Instantiating this proves the per-target alias satisfies the bound on
/// whichever target is being built — the [`crate::basemap_archive`]
/// `assert_source_bounds` construction, for the same wasm32 reason.
fn assert_store_bounds<St: SegmentStore>() {}
const _: fn() = assert_store_bounds::<PlatformSegmentStore>;

/// The read-back half of a store: what it holds, opened as range sources.
///
/// Separate from [`SegmentStore`] because the two halves have separate
/// consumers — the download engine only ever publishes, the map only ever
/// reads — and a store that could do one but not the other is a thing this
/// crate should be able to express. Both real stores do both.
///
/// The `Send` split per target is [`SegmentStore`]'s, for its reason.
///
/// **Completeness stays a store fact.** What this enumerates is what the
/// store holds *published*, by the same rule `existing_segments` answers by;
/// there is no flag anywhere saying a segment is finished, so there is none
/// to go stale.
#[cfg(not(target_arch = "wasm32"))]
pub trait OfflineSegments: Send + Sync + 'static {
    /// The range source this store's artifacts are read through.
    type Source: ArchiveRangeSource;

    /// Every complete segment the store holds **of `archive`**, labelled and
    /// opened.
    ///
    /// Filtered rather than returned whole because each archive's reader
    /// composes only its own segments: handing a hillshade segment to the
    /// vector reader would be refused at `attach_offline` on the tile type,
    /// but only after opening it, which is a read per segment per launch to
    /// learn what the name already says.
    ///
    /// **Returns no error, by design.** A store that cannot be listed, or an
    /// artifact that will not open, contributes nothing and is logged; the
    /// tiles it would have served come from the network, which is what the
    /// caller does with an empty answer anyway. There is nothing for a user
    /// to act on and so nothing to put on the glass.
    fn open_all(
        &self,
        archive: AreaArchive,
    ) -> impl Future<Output = Vec<(String, Self::Source)>> + Send;
}

/// See the native arm above.
#[cfg(target_arch = "wasm32")]
pub trait OfflineSegments: 'static {
    /// The range source this store's artifacts are read through.
    type Source: ArchiveRangeSource;
    /// See the native arm above.
    fn open_all(&self, archive: AreaArchive) -> impl Future<Output = Vec<(String, Self::Source)>>;
}

/// The suffix a published segment carries. The store's naming contract, in
/// one place, read by the listing as well as written by the publish.
const SEGMENT_SUFFIX: &str = ".pmtiles";

/// Instantiating this proves the per-target alias reads back on whichever
/// target is being built — [`assert_store_bounds`]'s construction.
fn assert_readback_bounds<St: OfflineSegments>() {}
const _: fn() = assert_readback_bounds::<PlatformSegmentStore>;

/// Segments as plain files: `{area_id}.{seg}.pmtiles` in one directory.
///
/// Native only — a `cfg` selecting a *dependency* (`std::fs`), which wasm32
/// does not have. Every operation blocks, which is correct where this runs
/// and wrong anywhere else: the engine owns an IO thread that exists to
/// block, exactly why tokio's `fs` feature is not wanted here.
///
/// Publish is `.part`-then-rename — the `write_blob` discipline (`squallar`'s
/// `kv.rs`): the rename is what publishes the segment, so a death at any
/// instant leaves either a complete listed segment or an unlisted `.part`
/// that the next run overwrites.
#[cfg(not(target_arch = "wasm32"))]
pub struct FsSegmentStore {
    dir: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FsSegmentStore {
    /// A store writing into `dir`. The directory is created on first publish,
    /// not here — an unused store leaves no empty directory.
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }

    /// `{area_id}.{seg}{infix}{suffix}` under the store's directory, where the
    /// infix is the archive's — empty for the base map, so an artifact this
    /// writes is named exactly as one written before terrain existed.
    fn artifact(
        &self,
        area_id: &str,
        archive: AreaArchive,
        seg: u32,
        suffix: &str,
    ) -> std::path::PathBuf {
        self.dir.join(format!(
            "{area_id}.{seg}{}{suffix}",
            archive.artifact_infix()
        ))
    }

    /// The segment number of `name`, if it is `archive`'s
    /// `{area_id}.{seg}{infix}.pmtiles`.
    ///
    /// A name of the *other* archive falls out here rather than being filtered
    /// beforehand: reading `{id}.{seg}.terrain.pmtiles` as a basemap artifact
    /// leaves `"{seg}.terrain"` where the number goes, which does not parse.
    fn segment_of(name: &str, area_id: &str, archive: AreaArchive) -> Option<u32> {
        name.strip_prefix(area_id)?
            .strip_prefix('.')?
            .strip_suffix(SEGMENT_SUFFIX)?
            .strip_suffix(archive.artifact_infix())?
            .parse()
            .ok()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl OfflineSegments for FsSegmentStore {
    type Source = crate::basemap_archive::FileRangeSource;

    /// Every `*.pmtiles` in the directory, in name order.
    ///
    /// **Not filtered to areas this device recorded**, on purpose: a segment
    /// hand-placed in the basemap directory is a segment, and the persisted
    /// area list is a later step's concern. It *is* filtered to `archive`,
    /// which is a property of the name rather than of any list. The directory
    /// listing is the one authority on what is here, so nothing can be listed
    /// and missing or missing and listed. A `.part` is not matched, which is
    /// the whole of what keeps a half-written artifact out of the map.
    async fn open_all(&self, archive: AreaArchive) -> Vec<(String, Self::Source)> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            // A directory nothing has published into holds nothing; absence
            // of it is that, not a fault.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                log::warn!(
                    "the downloaded basemap directory {} will not list, so the map reads \
                     everything from the network: {error}",
                    self.dir.display()
                );
                return Vec::new();
            }
        };

        let mut named: Vec<(String, std::path::PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_owned();
                (name.ends_with(SEGMENT_SUFFIX) && AreaArchive::of_artifact(&name) == archive)
                    .then(|| (name, entry.path()))
            })
            .collect();
        named.sort();

        let mut opened = Vec::with_capacity(named.len());
        for (name, path) in named {
            match Self::Source::open(&path) {
                Ok(source) => opened.push((name, source)),
                Err(error) => log::warn!(
                    "the downloaded basemap segment {} will not open, so its tiles come from \
                     the network instead: {error}",
                    path.display()
                ),
            }
        }
        opened
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SegmentStore for FsSegmentStore {
    async fn publish(
        &self,
        area_id: &str,
        archive: AreaArchive,
        seg: u32,
        bytes: Vec<u8>,
    ) -> Result<(), StoreError> {
        valid_area_id(area_id)?;
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| StoreError::Io(format!("create {}: {error}", self.dir.display())))?;

        let part = self.artifact(area_id, archive, seg, ".part");
        let published = self.artifact(area_id, archive, seg, SEGMENT_SUFFIX);
        std::fs::write(&part, &bytes)
            .map_err(|error| StoreError::Io(format!("write {}: {error}", part.display())))?;
        std::fs::rename(&part, &published).map_err(|error| {
            // A failed rename would otherwise leave an orphan per failure.
            let _ = std::fs::remove_file(&part);
            StoreError::Io(format!("publish {}: {error}", published.display()))
        })
    }

    async fn existing_segment_bytes(
        &self,
        area_id: &str,
        archive: AreaArchive,
    ) -> Result<BTreeMap<u32, u64>, StoreError> {
        valid_area_id(area_id)?;
        // A store nothing has published into holds no segments; absence of
        // the directory is that, not a fault.
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => {
                return Err(StoreError::Io(format!(
                    "list {}: {error}",
                    self.dir.display()
                )));
            }
        };

        let mut segments = BTreeMap::new();
        for entry in entries.flatten() {
            let Some(seg) = entry
                .file_name()
                .to_str()
                .and_then(|name| Self::segment_of(name, area_id, archive))
            else {
                continue;
            };
            // The segment is listed whatever its size reads as: it is on disk
            // and the completeness count is what listing answers. A size that
            // will not stat contributes zero, which understates the held
            // figure rather than dropping a segment from the count.
            let bytes = match entry.metadata() {
                Ok(metadata) => metadata.len(),
                Err(error) => {
                    log::warn!(
                        "{area_id}.{seg}: the offline store would not size the segment, so the \
                         held figure is short by it: {error}"
                    );
                    0
                }
            };
            segments.insert(seg, bytes);
        }
        Ok(segments)
    }

    /// Both archives and every `.part`: the sweep is `{area_id}.` followed by
    /// anything ending in an artifact suffix, which is what every name either
    /// half can carry.
    async fn remove_area(&self, area_id: &str) -> Result<(), StoreError> {
        valid_area_id(area_id)?;
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let ours = name
                .strip_prefix(area_id)
                .and_then(|rest| rest.strip_prefix('.'))
                .is_some_and(|rest| rest.ends_with(SEGMENT_SUFFIX) || rest.ends_with(".part"));
            if ours {
                std::fs::remove_file(entry.path()).map_err(|error| {
                    StoreError::Io(format!("remove {}: {error}", entry.path().display()))
                })?;
            }
        }
        Ok(())
    }
}

/// Segments over HTTP to the synthetic same-origin path the service worker
/// routes — see [`OFFLINE_BASE_PATH`] for the contract.
///
/// Deliberately ordinary `reqwest`, which is the point of the design: a store
/// operation is a request with a response and a real error, the same shape as
/// native, rather than a `postMessage` fire-and-forget needing correlation ids
/// — which is exactly where silent partial success hides. Compiled on every
/// target so the native test suite can drive the contract against a loopback
/// server; only the alias above makes it *the* store anywhere.
pub struct HttpSegmentStore {
    client: reqwest::Client,
    base: reqwest::Url,
}

impl HttpSegmentStore {
    /// A store on `origin` — the app's own origin; the service worker routes
    /// everything under [`OFFLINE_BASE_PATH`] on it.
    pub fn new(client: reqwest::Client, origin: reqwest::Url) -> Self {
        Self {
            client,
            base: origin,
        }
    }

    /// `{origin}/{OFFLINE_BASE_PATH}/{tail}`.
    fn url(&self, tail: &str) -> Result<reqwest::Url, StoreError> {
        self.base
            .join(&format!("{OFFLINE_BASE_PATH}/{tail}"))
            .map_err(|error| StoreError::Transport(format!("building a store URL: {error}")))
    }

    /// The `__list__` route's whole answer.
    ///
    /// One request, shared by the two readers — the per-area completeness
    /// count and the read-back enumeration — so they can never come to two
    /// answers about what the store holds.
    async fn list_rows(&self) -> Result<Vec<ListedSegment>, StoreError> {
        let response = self
            .client
            .get(self.url("__list__")?)
            .send()
            .await
            .map_err(|error| StoreError::Transport(error.to_string()))?;
        if !response.status().is_success() {
            return Err(StoreError::Status {
                action: "listing segments",
                status: response.status().as_u16(),
            });
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| StoreError::Transport(error.to_string()))?;
        serde_json::from_slice(&body).map_err(|error| StoreError::Listing(error.to_string()))
    }

    /// The `__quota__` route's answer: what the origin has, so "does this fit"
    /// is answerable *before* the download starts rather than discovered at
    /// byte 280 M.
    ///
    /// # Errors
    ///
    /// [`StoreError::Transport`] if the route will not answer,
    /// [`StoreError::Status`] on a non-2xx, [`StoreError::Listing`] if the body
    /// is not the documented JSON.
    pub async fn quota(&self) -> Result<OfflineQuota, StoreError> {
        let response = self
            .client
            .get(self.url("__quota__")?)
            .send()
            .await
            .map_err(|error| StoreError::Transport(error.to_string()))?;
        if !response.status().is_success() {
            return Err(StoreError::Status {
                action: "reading the storage quota",
                status: response.status().as_u16(),
            });
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| StoreError::Transport(error.to_string()))?;
        serde_json::from_slice(&body).map_err(|error| StoreError::Listing(error.to_string()))
    }
}

/// One row of the service worker's `__list__` answer.
///
/// `bytes` is what the segment occupies in the cache, as the worker wrote it
/// atomically with the body. A row from a worker too old to send one counts as
/// zero rather than dropping the segment: the segment is *there*, and a held
/// figure short by it is a smaller wrong than a completeness count short by
/// it.
#[derive(serde::Deserialize)]
struct ListedSegment {
    url: String,
    #[serde(default)]
    bytes: u64,
}

/// What the origin's storage has, as `navigator.storage.estimate()` reports it
/// through the worker's `__quota__` route.
///
/// **Either figure may be unknown, and unknown is not zero.** The route answers
/// `null` rather than `0` for exactly this reason: a zero quota reads as
/// "nothing fits", which is a fabrication a size figure would be gated on.
/// [`Self::free`] therefore answers `None` rather than a number whenever it
/// cannot subtract two real figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
pub struct OfflineQuota {
    /// Bytes the origin is already using, or `None` if the browser will not
    /// say.
    pub usage: Option<u64>,
    /// Bytes the origin may use in total, or `None` if the browser will not
    /// say.
    pub quota: Option<u64>,
}

impl OfflineQuota {
    /// What is left, or `None` when either figure is unknown — never a guess.
    ///
    /// Saturating, because a usage over quota is a state browsers do report
    /// and "nothing free" is the honest reading of it.
    pub fn free(self) -> Option<DataSize> {
        Some(DataSize::from_bytes(
            self.quota?.saturating_sub(self.usage?),
        ))
    }
}

impl OfflineSegments for HttpSegmentStore {
    type Source = crate::basemap_archive::HttpRangeSource;

    /// The `__list__` route's whole answer, every row that names a segment,
    /// opened as a monolith range source.
    ///
    /// Monolith without probing: the service worker holds one response per
    /// segment and a segment is [`SEGMENT_BYTES`], so the `.part000` probe
    /// could only ever spend one request per segment being told what the
    /// publish side already guarantees — see
    /// [`crate::basemap_archive::HttpRangeSource::monolith`].
    async fn open_all(&self, archive: AreaArchive) -> Vec<(String, Self::Source)> {
        let listed = match self.list_rows().await {
            Ok(listed) => listed,
            Err(error) => {
                log::warn!(
                    "the downloaded basemap store will not list, so the map reads everything \
                     from the network: {error}"
                );
                return Vec::new();
            }
        };

        let needle = format!("{OFFLINE_BASE_PATH}/");
        let mut tails: Vec<String> = listed
            .into_iter()
            .filter_map(|row| {
                let at = row.url.rfind(&needle)?;
                let tail = &row.url[at + needle.len()..];
                (tail.ends_with(SEGMENT_SUFFIX) && AreaArchive::of_artifact(tail) == archive)
                    .then(|| tail.to_owned())
            })
            .collect();
        tails.sort();

        let mut opened = Vec::with_capacity(tails.len());
        for tail in tails {
            let built = self.url(&tail).and_then(|url| {
                Self::Source::monolith(self.client.clone(), url.as_str())
                    .map_err(|error| StoreError::Transport(error.to_string()))
            });
            match built {
                Ok(source) => opened.push((tail, source)),
                Err(error) => log::warn!(
                    "the downloaded basemap segment {tail} will not open, so its tiles come \
                     from the network instead: {error}"
                ),
            }
        }
        opened
    }
}

impl SegmentStore for HttpSegmentStore {
    async fn publish(
        &self,
        area_id: &str,
        archive: AreaArchive,
        seg: u32,
        bytes: Vec<u8>,
    ) -> Result<(), StoreError> {
        valid_area_id(area_id)?;
        // Both halves live under the SAME area directory, so the worker's
        // `DELETE {area_id}/` prefix sweep removes the rectangle whole — the
        // native store's own rule, arrived at the same way.
        let url = self.url(&format!(
            "{area_id}/{seg}{}{SEGMENT_SUFFIX}",
            archive.artifact_infix()
        ))?;
        let response = self
            .client
            .put(url)
            .body(bytes)
            .send()
            .await
            .map_err(|error| StoreError::Transport(error.to_string()))?;
        if !response.status().is_success() {
            return Err(StoreError::Status {
                action: "storing a segment",
                status: response.status().as_u16(),
            });
        }
        Ok(())
    }

    async fn existing_segment_bytes(
        &self,
        area_id: &str,
        archive: AreaArchive,
    ) -> Result<BTreeMap<u32, u64>, StoreError> {
        valid_area_id(area_id)?;
        let listed = self.list_rows().await?;

        let needle = format!("{OFFLINE_BASE_PATH}/{area_id}/");
        let mut segments = BTreeMap::new();
        for row in listed {
            if let Some(at) = row.url.rfind(&needle)
                && let Some(seg) = row.url[at + needle.len()..]
                    .strip_suffix(SEGMENT_SUFFIX)
                    .and_then(|stem| stem.strip_suffix(archive.artifact_infix()))
                    .and_then(|stem| stem.parse().ok())
            {
                segments.insert(seg, row.bytes);
            }
        }
        Ok(segments)
    }

    /// Both archives: the worker's directory `DELETE` is a prefix sweep, and
    /// both halves are published under the one area directory.
    async fn remove_area(&self, area_id: &str) -> Result<(), StoreError> {
        valid_area_id(area_id)?;
        let response = self
            .client
            .delete(self.url(&format!("{area_id}/"))?)
            .send()
            .await
            .map_err(|error| StoreError::Transport(error.to_string()))?;
        if !response.status().is_success() {
            return Err(StoreError::Status {
                action: "removing an area",
                status: response.status().as_u16(),
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Errors and the outcome
// ---------------------------------------------------------------------------

/// What went wrong inside the engine. Internal — what crosses to the caller
/// is its rendering inside [`DownloadOutcome`], where the *variant* carries
/// the meaning.
#[derive(Debug)]
enum DownloadError {
    /// The archive's directories would not read or decode.
    Index(IndexError),
    /// A tile-data range would not arrive.
    Range(RangeError),
    /// A tile-data range arrived short — the archive ended before the span it
    /// declared.
    Truncated {
        /// Bytes asked for.
        wanted: u64,
        /// Bytes that arrived.
        got: u64,
    },
    /// The store refused a publish or a listing.
    Store(StoreError),
    /// The segment writer failed.
    Write(String),
    /// The finished segment did not verify — it would not reopen, or a tile
    /// it was built to hold is not addressed in it.
    Verify(String),
    /// The archive's metadata would not read or decode.
    Metadata(String),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(error) => write!(f, "{error}"),
            Self::Range(error) => write!(f, "{error}"),
            Self::Truncated { wanted, got } => {
                write!(
                    f,
                    "a tile range arrived short: wanted {wanted} bytes, got {got}"
                )
            }
            Self::Store(error) => write!(f, "{error}"),
            Self::Write(message) => write!(f, "writing a segment failed: {message}"),
            Self::Verify(message) => write!(f, "a finished segment failed verification: {message}"),
            Self::Metadata(message) => {
                write!(f, "the archive metadata would not read: {message}")
            }
        }
    }
}

impl From<IndexError> for DownloadError {
    fn from(error: IndexError) -> Self {
        Self::Index(error)
    }
}

/// How a download ended. **No bool and no `Result<(), E>` on this path**: a
/// `Partial` carries its counts, so a UI cannot render a half-download as done
/// — "3 of 7 parts" draws *in place of* a size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// Every planned segment is in the store.
    Complete {
        /// Tile bytes this run transferred — distinct `(offset, length)`
        /// pairs once each; on a fresh area, exactly
        /// [`DownloadPlan::fetch_bytes`]. Zero when everything already
        /// existed, or the area addresses no tiles at all.
        bytes: u64,
        /// Segments the area has in total.
        segments: u32,
    },
    /// Some segments made it, some did not. The finished ones are standalone
    /// archives and stay useful; a fresh start completes the difference.
    Partial {
        /// Segments now in the store, pre-existing ones included.
        done: u32,
        /// Segments the plan has in total.
        of: u32,
        /// Tile bytes this run transferred — same denominator as `Complete`.
        bytes: u64,
        /// The first error, rendered. Later segments' failures are usually
        /// the same fault repeating.
        first_error: String,
    },
    /// Nothing is in the store: planning failed, or every segment did and
    /// none pre-existed.
    Failed {
        /// The error, rendered.
        error: String,
    },
}

/// What each archive's half of a run came to.
///
/// **Beside [`DownloadOutcome`] rather than inside it**, because the two
/// answer different questions and only one of them is the run's verdict.
/// `DownloadOutcome` states whether the area is now whole, over both archives
/// summed; this is the bookkeeping a *record* needs, because the two halves
/// are reconciled against the store independently and a combined segment count
/// could not be split back apart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AreaHoldings {
    /// The base map's half. Every run has one.
    pub basemap: ArchiveHold,
    /// The terrain half, or `None` for a run with no terrain source — which is
    /// what a download the user did not ask terrain of is.
    pub terrain: Option<ArchiveHold>,
}

impl AreaHoldings {
    /// Tile bytes the run transferred across both archives — the same figure
    /// [`DownloadOutcome::Complete`]'s `bytes` carries, summed the same way.
    pub fn bytes(&self) -> u64 {
        self.basemap.bytes + self.terrain.as_ref().map_or(0, |hold| hold.bytes)
    }
}

/// One archive's half of a run: what it was cut into and what it cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArchiveHold {
    /// Segments this archive's plan cut the area into — requested, not
    /// present, [`DownloadedArea`]'s rule.
    pub segments: u32,
    /// Tile bytes transferred for this archive **this run**, distinct
    /// `(offset, length)` pairs once each. Zero on a resume that found every
    /// segment already stored, exactly as `Complete`'s figure is.
    pub bytes: u64,
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// The engine's always-on counters — relaxed atomics, one write per event,
/// summarised by a single [`BasemapDownload::progress`], on the
/// `overlay_cache::ledger` model.
#[derive(Default)]
struct Ledger {
    /// Tiles whose segment has been published, this run.
    tiles_done: AtomicU64,
    /// Tiles this run set out to fetch (missing segments only).
    tiles_total: AtomicU64,
    /// Tile bytes fetched so far, this run.
    bytes_done: AtomicU64,
    /// Tile bytes this run set out to fetch.
    bytes_total: AtomicU64,
    /// Segments in the store, pre-existing included.
    segments_done: AtomicU32,
    /// Segments the whole area has.
    segments_total: AtomicU32,
    /// High-water mark of bytes the engine held at once.
    peak_held_bytes: AtomicU64,
}

impl Ledger {
    /// Record what the engine is holding right now, keeping the high-water
    /// mark.
    fn note_held(&self, bytes: u64) {
        self.peak_held_bytes.fetch_max(bytes, Relaxed);
    }
}

/// A reading of a download's counters, taken together.
///
/// Two denominators, each named: the **segment** figures cover the whole
/// area (done includes segments that already existed); the **tile and byte**
/// figures cover this run's remaining work only — which is what a progress
/// bar should fill against on a resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Tiles landed, of this run's work.
    pub tiles_done: u64,
    /// Tiles this run set out to fetch.
    pub tiles_total: u64,
    /// Tile bytes fetched, of this run's work. Distinct spans once each; gap
    /// bytes are in no figure.
    pub bytes_done: DataSize,
    /// Tile bytes this run set out to fetch.
    pub bytes_total: DataSize,
    /// Segments in the store, of the whole area — pre-existing included.
    pub segments_done: u32,
    /// Segments the whole area has.
    pub segments_total: u32,
    /// The most bytes the engine held at once: fetched spans awaiting their
    /// segment, plus the segment artifact being built. **The engine's own
    /// buffers**, not process heap — allocator slack, HTTP internals and the
    /// directory cache are not in it.
    pub peak_held: DataSize,
}

impl DownloadProgress {
    /// Whether this run has a byte denominator yet.
    ///
    /// The plan is cut before a byte moves — every tile of the area looked up
    /// through the archive's directories — and until it lands there is no
    /// total for a bar to fill against. A screen must say it is preparing
    /// rather than draw a bar pinned at zero: over a large area the cut runs
    /// for minutes, and a stationary bar is indistinguishable from a hang.
    ///
    /// Read off the **segment** cut rather than the byte total, because a
    /// resume with nothing left to fetch has a real plan and a zero byte
    /// total; asking the byte total would report that run as still preparing
    /// forever.
    pub fn denominator_known(self) -> bool {
        self.segments_total > 0
    }

    /// How full a bar over this run's bytes stands, `0.0..=1.0`, or `None`
    /// while [`Self::denominator_known`] is false — never a fabricated
    /// fraction over an unknown denominator.
    ///
    /// A planned run with nothing to fetch is full: every byte it set out to
    /// transfer is transferred, which is what zero of zero means here.
    pub fn byte_fraction(self) -> Option<f32> {
        if !self.denominator_known() {
            return None;
        }
        let total = self.bytes_total.bytes();
        if total == 0 {
            return Some(1.0);
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a bar's fill is a fraction of its width; the exact \
                      figures are the labels beside it"
        )]
        Some((self.bytes_done.bytes() as f32 / total as f32).clamp(0.0, 1.0))
    }
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// One area's download, running on its own IO runtime from the moment
/// [`BasemapDownload::start`] returns. Dropping it is cancellation.
pub struct BasemapDownload {
    ledger: Arc<Ledger>,
    ended: Arc<OnceLock<RunEnd>>,
    /// Owns the task. Dropped last, but named first here for the reader: the
    /// drop is the whole cancel protocol.
    _runtime: runtime::Runtime,
}

/// How a run finished: the verdict and each archive's own cut, set together in
/// one `OnceLock` write so a reader can never see one without the other.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunEnd {
    outcome: DownloadOutcome,
    holdings: AreaHoldings,
}

impl BasemapDownload {
    /// Download `area`'s base map from `source` into `store`, in
    /// [`SEGMENT_BYTES`] segments. Returns immediately; the work runs on the
    /// engine's own runtime, repainting `egui_ctx` as counters move and once
    /// more when the outcome lands.
    pub fn start<S: ArchiveRangeSource, St: SegmentStore>(
        source: S,
        store: St,
        area: AreaSpec,
        egui_ctx: Context,
    ) -> Self {
        Self::with_segment_bytes(source, store, area, egui_ctx, SEGMENT_BYTES)
    }

    /// [`Self::start`] with the terrain archive as well, when the user asked
    /// for the hillshade.
    ///
    /// **One run over two archives, not two runs.** The two are planned before
    /// either fetches, so the byte denominator the bar fills against is the
    /// whole download's from the first byte — a total that grew halfway would
    /// send the bar backwards. `None` is exactly a basemap-only download and
    /// takes the same path this call did before terrain existed.
    pub fn start_with_terrain<S, T, St>(
        source: S,
        terrain: Option<T>,
        store: St,
        area: AreaSpec,
        egui_ctx: Context,
    ) -> Self
    where
        S: ArchiveRangeSource,
        T: ArchiveRangeSource,
        St: SegmentStore,
    {
        Self::with_terrain_and_segment_bytes(source, terrain, store, area, egui_ctx, SEGMENT_BYTES)
    }

    /// [`Self::start`] with the segment cap handed in — for the tests, which
    /// need multi-segment plans out of a 419 KB fixture. The cap is not a
    /// tuning dial: two runs over one area must agree on the plan, so
    /// production has exactly one cap.
    pub(crate) fn with_segment_bytes<S: ArchiveRangeSource, St: SegmentStore>(
        source: S,
        store: St,
        area: AreaSpec,
        egui_ctx: Context,
        segment_bytes: u64,
    ) -> Self {
        Self::with_terrain_and_segment_bytes(
            source,
            None::<S>,
            store,
            area,
            egui_ctx,
            segment_bytes,
        )
    }

    /// [`Self::start_with_terrain`] with the segment cap handed in — the one
    /// body the other three constructors are spellings of.
    pub(crate) fn with_terrain_and_segment_bytes<S, T, St>(
        source: S,
        terrain: Option<T>,
        store: St,
        area: AreaSpec,
        egui_ctx: Context,
        segment_bytes: u64,
    ) -> Self
    where
        S: ArchiveRangeSource,
        T: ArchiveRangeSource,
        St: SegmentStore,
    {
        let ledger = Arc::new(Ledger::default());
        let ended = Arc::new(OnceLock::new());

        let task = {
            let ledger = Arc::clone(&ledger);
            let ended = Arc::clone(&ended);
            async move {
                let finished = match drive(
                    source,
                    terrain,
                    store,
                    &area,
                    segment_bytes,
                    &ledger,
                    &egui_ctx,
                )
                .await
                {
                    Ok(finished) => finished,
                    Err(error) => RunEnd {
                        outcome: DownloadOutcome::Failed {
                            error: error.to_string(),
                        },
                        holdings: AreaHoldings::default(),
                    },
                };
                // Set only here, so a second set is impossible rather than
                // guarded against.
                let _ = ended.set(finished);
                egui_ctx.request_repaint();
            }
        };

        Self {
            ledger,
            ended,
            _runtime: runtime::spawn(task),
        }
    }

    /// Where the download stands. See [`DownloadProgress`] for which figure
    /// has which denominator.
    pub fn progress(&self) -> DownloadProgress {
        DownloadProgress {
            tiles_done: self.ledger.tiles_done.load(Relaxed),
            tiles_total: self.ledger.tiles_total.load(Relaxed),
            bytes_done: DataSize::from_bytes(self.ledger.bytes_done.load(Relaxed)),
            bytes_total: DataSize::from_bytes(self.ledger.bytes_total.load(Relaxed)),
            segments_done: self.ledger.segments_done.load(Relaxed),
            segments_total: self.ledger.segments_total.load(Relaxed),
            peak_held: DataSize::from_bytes(self.ledger.peak_held_bytes.load(Relaxed)),
        }
    }

    /// How the download ended, once it has. Both archives summed — see
    /// [`Self::holdings`] for the split.
    pub fn outcome(&self) -> Option<DownloadOutcome> {
        self.ended.get().map(|ended| ended.outcome.clone())
    }

    /// What each archive's half of the run came to, once it has ended.
    pub fn holdings(&self) -> Option<AreaHoldings> {
        self.ended.get().map(|ended| ended.holdings.clone())
    }
}

/// Everything one archive's segments share — bundled so the inner loop is
/// methods rather than a ten-argument call.
struct Run<'a, S, St> {
    index: &'a PmtIndex<S>,
    header: IndexHeader,
    metadata: String,
    area: &'a AreaSpec,
    archive: AreaArchive,
    store: &'a St,
    ledger: &'a Ledger,
    ctx: &'a Context,
}

/// One archive read, planned and diffed against the store, with nothing
/// fetched yet.
struct Planned<S> {
    index: PmtIndex<S>,
    header: IndexHeader,
    metadata: String,
    plan: DownloadPlan,
    /// Indices into `plan.segments` the store does not already hold.
    missing: Vec<usize>,
    /// Segments the store already holds, of this archive's cut.
    pre_existing: u32,
    /// Tiles the missing segments hold.
    run_tiles: u64,
    /// Distinct span bytes across the missing segments.
    run_bytes: u64,
}

impl<S> Planned<S> {
    /// This archive's cut, as the record carries it.
    fn segments(&self) -> u32 {
        self.plan.segments.len() as u32
    }
}

/// Open one archive, cut the area against it, and diff the cut against the
/// store — everything up to the first byte of tile data.
async fn prepare<S: ArchiveRangeSource, St: SegmentStore>(
    source: S,
    store: &St,
    area: &AreaSpec,
    archive: AreaArchive,
    segment_bytes: u64,
) -> Result<Planned<S>, DownloadError> {
    let index = PmtIndex::open(source).await?;
    let header = *index.header();
    let metadata = read_metadata(&index).await?;
    let plan = plan_area(&index, area, segment_bytes).await?;
    let existing = store
        .existing_segments(&area.area_id, archive)
        .await
        .map_err(DownloadError::Store)?;

    // Resume is this filter and nothing else: a set difference over
    // segments, never a byte offset. Per archive, so one half being whole
    // says nothing about the other.
    let missing: Vec<usize> = plan
        .segments
        .iter()
        .enumerate()
        .filter(|(_, segment)| !existing.contains(&segment.seg))
        .map(|(at, _)| at)
        .collect();
    let pre_existing = (plan.segments.len() - missing.len()) as u32;

    // This archive's share of the run's denominators. Bytes: distinct spans
    // across the missing segments, once each — the carry below is what makes
    // the engine actually transfer that figure and not more.
    let mut run_spans = HashSet::new();
    let mut run_bytes = 0u64;
    let mut run_tiles = 0u64;
    for &at in &missing {
        for tile in &plan.segments[at].tiles {
            run_tiles += 1;
            if run_spans.insert(tile.span) {
                run_bytes += tile.span.length;
            }
        }
    }

    Ok(Planned {
        index,
        header,
        metadata,
        plan,
        missing,
        pre_existing,
        run_tiles,
        run_bytes,
    })
}

/// Fetch and publish one archive's missing segments, answering how many of its
/// cut the store now holds and the first thing that went wrong.
async fn build_archive<S: ArchiveRangeSource, St: SegmentStore>(
    planned: &Planned<S>,
    area: &AreaSpec,
    archive: AreaArchive,
    store: &St,
    ledger: &Ledger,
    ctx: &Context,
) -> (u32, Option<DownloadError>) {
    // A span two segments share is fetched at its first user and carried to
    // its last, so the run transfers each distinct span exactly once.
    let mut last_use: HashMap<TileSpan, usize> = HashMap::new();
    for (order, &at) in planned.missing.iter().enumerate() {
        for tile in &planned.plan.segments[at].tiles {
            last_use.insert(tile.span, order);
        }
    }

    let run = Run {
        index: &planned.index,
        header: planned.header,
        metadata: planned.metadata.clone(),
        area,
        archive,
        store,
        ledger,
        ctx,
    };
    let mut held: HashMap<TileSpan, Vec<u8>> = HashMap::new();
    let mut held_bytes = 0u64;
    let mut first_error: Option<DownloadError> = None;
    let mut done = planned.pre_existing;

    for (order, &at) in planned.missing.iter().enumerate() {
        let segment = &planned.plan.segments[at];
        match run
            .build_and_publish(segment, &mut held, &mut held_bytes)
            .await
        {
            Ok(()) => {
                done += 1;
                ledger.segments_done.fetch_add(1, Relaxed);
                ledger
                    .tiles_done
                    .fetch_add(segment.tiles.len() as u64, Relaxed);
            }
            Err(error) => {
                // Segments fail independently; the ones after this may still
                // land, and the finished ones already have.
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        // Spans whose last user has passed are dead weight.
        held.retain(|span, bytes| {
            let keep = last_use.get(span).is_some_and(|&last| last > order);
            if !keep {
                held_bytes -= bytes.len() as u64;
            }
            keep
        });
        ctx.request_repaint();
    }

    (done, first_error)
}

/// The whole download, start to outcome — **both archives, planned before
/// either fetches**.
///
/// The two phases are the reason this is not two calls of a one-archive
/// engine: the bar's denominators are set once, from both cuts, so a run's
/// total never grows under it. The archives then build in order, and a failure
/// in one does not abandon the other — the same independence a segment has
/// from the segment after it, one level up.
async fn drive<S, T, St>(
    source: S,
    terrain: Option<T>,
    store: St,
    area: &AreaSpec,
    segment_bytes: u64,
    ledger: &Ledger,
    ctx: &Context,
) -> Result<RunEnd, DownloadError>
where
    S: ArchiveRangeSource,
    T: ArchiveRangeSource,
    St: SegmentStore,
{
    let basemap = prepare(source, &store, area, AreaArchive::Basemap, segment_bytes).await?;
    // The terrain half planning is fallible on its own terms: a terrain
    // archive that will not open must not lose the base map the user also
    // asked for, so the error is carried into the verdict rather than
    // returned.
    let (terrain, mut first_error) = match terrain {
        Some(source) => {
            match prepare(source, &store, area, AreaArchive::Terrain, segment_bytes).await {
                Ok(planned) => (Some(planned), None),
                Err(error) => (None, Some(error)),
            }
        }
        None => (None, None),
    };

    let terrain_tiles = terrain.as_ref().map_or(0, |half| half.run_tiles);
    let terrain_bytes = terrain.as_ref().map_or(0, |half| half.run_bytes);
    let terrain_segments = terrain.as_ref().map_or(0, Planned::segments);
    let terrain_pre_existing = terrain.as_ref().map_or(0, |half| half.pre_existing);
    ledger
        .tiles_total
        .store(basemap.run_tiles + terrain_tiles, Relaxed);
    ledger
        .bytes_total
        .store(basemap.run_bytes + terrain_bytes, Relaxed);
    ledger
        .segments_total
        .store(basemap.segments() + terrain_segments, Relaxed);
    ledger
        .segments_done
        .store(basemap.pre_existing + terrain_pre_existing, Relaxed);
    ctx.request_repaint();

    let before = ledger.bytes_done.load(Relaxed);
    let (basemap_done, basemap_error) =
        build_archive(&basemap, area, AreaArchive::Basemap, &store, ledger, ctx).await;
    let basemap_hold = ArchiveHold {
        segments: basemap.segments(),
        bytes: ledger.bytes_done.load(Relaxed) - before,
    };
    first_error = first_error.or(basemap_error);

    let mut done = basemap_done;
    let mut terrain_hold = None;
    if let Some(half) = terrain.as_ref() {
        let before = ledger.bytes_done.load(Relaxed);
        let (terrain_done, terrain_error) =
            build_archive(half, area, AreaArchive::Terrain, &store, ledger, ctx).await;
        terrain_hold = Some(ArchiveHold {
            segments: half.segments(),
            bytes: ledger.bytes_done.load(Relaxed) - before,
        });
        done += terrain_done;
        first_error = first_error.or(terrain_error);
    }

    let of = basemap.segments() + terrain_segments;
    let bytes = ledger.bytes_done.load(Relaxed);
    let outcome = match first_error {
        None => DownloadOutcome::Complete {
            bytes,
            segments: of,
        },
        Some(error) if done == 0 => DownloadOutcome::Failed {
            error: error.to_string(),
        },
        Some(error) => DownloadOutcome::Partial {
            done,
            of,
            bytes,
            first_error: error.to_string(),
        },
    };
    Ok(RunEnd {
        outcome,
        holdings: AreaHoldings {
            basemap: basemap_hold,
            terrain: terrain_hold,
        },
    })
}

impl<S: ArchiveRangeSource, St: SegmentStore> Run<'_, S, St> {
    /// Fetch what one segment still needs, write it, verify it, publish it.
    ///
    /// `held` is the run's carry: spans fetched here that a later segment
    /// also needs stay in it, so the run transfers each distinct span once.
    async fn build_and_publish(
        &self,
        segment: &PlannedSegment,
        held: &mut HashMap<TileSpan, Vec<u8>>,
        held_bytes: &mut u64,
    ) -> Result<(), DownloadError> {
        // What is not already carried from an earlier segment.
        let mut needed: Vec<TileSpan> = segment
            .tiles
            .iter()
            .map(|tile| tile.span)
            .filter(|span| !held.contains_key(span))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        needed.sort_unstable_by_key(|span| span.offset);

        for range in coalesce(&needed) {
            let bytes = read_exactly(
                self.index.source(),
                self.header.tile_data_offset + range.start,
                range.length,
            )
            .await?;
            for span in range.spans {
                let from = (span.offset - range.start) as usize;
                let blob = bytes[from..from + span.length as usize].to_vec();
                *held_bytes += blob.len() as u64;
                held.insert(span, blob);
                self.ledger.bytes_done.fetch_add(span.length, Relaxed);
            }
            self.ledger.note_held(*held_bytes);
            self.ctx.request_repaint();
        }

        let artifact = Arc::new(write_segment(
            &self.header,
            &self.metadata,
            self.area,
            segment,
            held,
        )?);
        self.ledger
            .note_held(*held_bytes + artifact.capacity() as u64);

        verify_segment(Arc::clone(&artifact), segment).await?;
        let artifact =
            Arc::try_unwrap(artifact).expect("the verifier dropped its clone of the artifact");

        self.store
            .publish(&self.area.area_id, self.archive, segment.seg, artifact)
            .await
            .map_err(DownloadError::Store)
    }
}

/// Read exactly `length` bytes at `offset` — a short answer here is the
/// archive ending before a span its directories declared.
async fn read_exactly<S: RangeSource>(
    source: &S,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, DownloadError> {
    let mut bytes = source
        .read_range(offset, length as usize)
        .await
        .map_err(DownloadError::Range)?;
    if (bytes.len() as u64) < length {
        return Err(DownloadError::Truncated {
            wanted: length,
            got: bytes.len() as u64,
        });
    }
    bytes.truncate(length as usize);
    Ok(bytes)
}

/// Read and decompress the archive's metadata JSON, copied verbatim into
/// every segment so a sub-archive carries the same `vector_layers` its parent
/// does.
async fn read_metadata<S: ArchiveRangeSource>(
    index: &PmtIndex<S>,
) -> Result<String, DownloadError> {
    let header = index.header();
    if header.metadata_length == 0 {
        return Ok("{}".to_owned());
    }
    let bytes = read_exactly(
        index.source(),
        header.metadata_offset,
        header.metadata_length,
    )
    .await?;
    let plain = decompress(bytes, header.internal_compression)
        .map_err(|error| DownloadError::Metadata(error.to_string()))?;
    String::from_utf8(plain).map_err(|_| DownloadError::Metadata("not UTF-8".to_owned()))
}

/// A `Write + Seek` handle onto a buffer the caller keeps, because
/// `PmTilesStreamWriter::finalize` consumes the writer without returning the
/// sink. The lock is uncontended — one writer, no reader until it is done.
#[derive(Clone, Default)]
struct SegmentSink(Arc<Mutex<Cursor<Vec<u8>>>>);

impl SegmentSink {
    /// The finished bytes, once the writer is done with the sink.
    fn into_bytes(self) -> Vec<u8> {
        std::mem::take(
            self.0
                .lock()
                .expect("the segment sink lock is never poisoned: no holder panics")
                .get_mut(),
        )
    }
}

impl Write for SegmentSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("the segment sink lock is never poisoned: no holder panics")
            .write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for SegmentSink {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0
            .lock()
            .expect("the segment sink lock is never poisoned: no holder panics")
            .seek(pos)
    }
}

/// Write one segment as a complete standalone archive, in memory.
///
/// Synchronous on purpose: `PmTilesStreamWriter` holds `Box<dyn Compressor>`
/// without `Send`, so it must never live across an `await` — confining it to
/// this function is what keeps the engine's future spawnable on the native
/// runtime.
fn write_segment(
    header: &IndexHeader,
    metadata: &str,
    area: &AreaSpec,
    segment: &PlannedSegment,
    held: &HashMap<TileSpan, Vec<u8>>,
) -> Result<Vec<u8>, DownloadError> {
    let tile_type: TileType = header
        .tile_type
        .try_into()
        .map_err(|_| DownloadError::Write(format!("tile type {} unknown", header.tile_type)))?;
    let compression: Compression = header.tile_compression.try_into().map_err(|_| {
        DownloadError::Write(format!(
            "tile compression {} unknown",
            header.tile_compression
        ))
    })?;
    let (min_zoom, max_zoom) = segment.tiles.iter().fold((u8::MAX, 0u8), |(lo, hi), tile| {
        (lo.min(tile.z), hi.max(tile.z))
    });

    let sink = SegmentSink::default();
    // The declared compression matches the parent's because the bytes ARE the
    // parent's: `add_raw_tile` stores them verbatim, so nothing is decoded
    // and nothing is recompressed.
    let mut writer = PmTilesWriter::new(tile_type)
        .tile_compression(compression)
        .min_zoom(min_zoom)
        .max_zoom(max_zoom)
        .bounds(area.west, area.south, area.east, area.north)
        .metadata(metadata)
        .create(sink.clone())
        .map_err(|error| DownloadError::Write(error.to_string()))?;

    for tile in &segment.tiles {
        let bytes = held
            .get(&tile.span)
            .expect("every span of a segment was fetched or carried before the write");
        let coord = TileCoord::new(tile.z, tile.x, tile.y)
            .map_err(|error| DownloadError::Write(error.to_string()))?;
        writer
            .add_raw_tile(coord, bytes)
            .map_err(|error| DownloadError::Write(error.to_string()))?;
    }
    writer
        .finalize()
        .map_err(|error| DownloadError::Write(error.to_string()))?;

    Ok(sink.into_bytes())
}

/// A [`RangeSource`] over a finished segment's bytes, for verification.
#[derive(Clone)]
struct SegmentBytes(Arc<Vec<u8>>);

impl RangeSource for SegmentBytes {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let bytes = Arc::clone(&self.0);
        async move {
            let from = (offset as usize).min(bytes.len());
            let to = from.saturating_add(length).min(bytes.len());
            Ok(bytes[from..to].to_vec())
        }
    }
}

/// Reopen a finished segment and confirm it addresses every tile it was built
/// to hold — the artifact verifying itself *before* the publish, so "the loop
/// finished without error" is never the evidence.
async fn verify_segment(
    artifact: Arc<Vec<u8>>,
    segment: &PlannedSegment,
) -> Result<(), DownloadError> {
    let reopened = PmtIndex::open(SegmentBytes(artifact))
        .await
        .map_err(|error| DownloadError::Verify(format!("it would not reopen: {error}")))?;
    for tile in &segment.tiles {
        let span = reopened
            .tile_span(tile.z, tile.x, tile.y)
            .await
            .map_err(|error| DownloadError::Verify(error.to_string()))?;
        if span.is_none() {
            return Err(DownloadError::Verify(format!(
                "{}/{}/{} is not addressed in its finished segment",
                tile.z, tile.x, tile.y
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod tests;
