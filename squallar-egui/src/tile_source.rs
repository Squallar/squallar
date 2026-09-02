//! squallar's own basemap tile source.
//!
//! Every other HTTPS call squallar makes goes through [`squallar_radar::tls::client`]
//! (`rustls-platform-verifier` + *ring*), so the OS decides trust at handshake
//! time and nothing in the binary carries an expiry date. Basemap tiles were the
//! one path that bypassed it: [`walkers::HttpTiles`] builds its own `reqwest`
//! client internally (`walkers-0.56.0/src/io/http.rs`, `bare_client`) with
//! `features = ["rustls-tls"]`, which in reqwest 0.12 resolves to `webpki-roots`
//! — the Mozilla root store compiled into the binary.
//!
//! [`HttpsTiles`] replaces `HttpTiles`, implementing [`walkers::Tiles`], so the
//! rest of the walkers integration is untouched. It reproduces async fetching,
//! bounded concurrency at [`MAX_PARALLEL_DOWNLOADS`], decode and upload via
//! [`walkers::Tile::new`], a byte-bounded LRU ([`byte_lru::ByteLru`]) floored
//! at the pass's measured working set, in-flight de-duplication, lower-zoom interpolation, `max_zoom` clamping,
//! grid-bounds checking, repaint-on-arrival and attribution.
//!
//! Deliberate differences: our client with a
//! [`crate::basemap_archive::REQUEST_TIMEOUT`] (walkers sets
//! none); squallar's `User-Agent`; a closed request channel logs instead of
//! panicking (walkers' `TilesIo::make_sure_is_fetched` calls `panic!`); no HTTP
//! disk cache, which neither has.
//!
//! The IO runtime splits per target as walkers splits it: a thread with a
//! current-thread tokio runtime on native, `spawn_local` on wasm. On wasm the
//! IO task shares the page thread, so a completed fetch hands over **compressed
//! bytes** — or, for a restyle served out of the parsed cache, the parse
//! itself — and the decode/styling + upload runs in [`HttpsTiles::pump`] under
//! [`WASM_TILE_DECODES_PER_PUMP`] — see [`FetchPayload`].
//!
//! The pump is called **once per layer**, by `ui_map_overlays::draw_tile_layer`
//! before its grid loop, and never from [`Tiles::at`] — see
//! [`HttpsTiles::pump`] for why, and for the one thing that would break.
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use egui::Context;
use futures::channel::mpsc::{Receiver, Sender, TryRecvError, channel};
// The batch-reply channel is the wasm arm's alone: native decodes on the IO
// thread and has nothing to hand back.
#[cfg(target_arch = "wasm32")]
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::stream::FuturesUnordered;
use futures::{SinkExt, StreamExt, future::Either, future::select};
use lru::LruCache;
use walkers::sources::{Attribution, TileSource};
use walkers::{Style, Tile, TileId, TilePiece, Tiles};

/// Maximum number of tile downloads in flight at once — walkers' default.
/// Tile providers throttle or ban clients that exceed their limits, so this is
/// a term of use rather than a performance dial.
pub const MAX_PARALLEL_DOWNLOADS: usize = 6;

/// Consecutive tile reads that must fail, with none answering in between,
/// before an archive source reports that it is not drawing the map.
///
/// [`MAX_PARALLEL_DOWNLOADS`], because that is how many reads
/// [`serve_archive_continuously`] can have outstanding at once: a run this
/// long means a whole cohort of concurrently-issued reads failed with not one
/// of them answering, which is the smallest observation this loop can make
/// that is about the *source* rather than about the particular tiles it was
/// asked for. One dead tile among live neighbours ends the run at 1, because
/// the cohort completes in arbitrary order and any answer resets it.
///
/// A read *answers* with a body or with the archive's authoritative "no tile
/// at this coordinate", so a viewport of open ocean is not a fault.
const SUSTAINED_READ_FAILURES: usize = MAX_PARALLEL_DOWNLOADS;

/// Said once per run, when [`ReadFailureRun::answered`] ends one. Named
/// because both arms of the serve loop's success path say it.
const RECOVERED: &str = "the archive is answering tile reads again, so it draws and is credited \
                         again";

/// The measured worst-case cost of one **styled** cache entry, in bytes: a
/// vector tile's shapes plus the flattened [`crate::tile_mesh::TileMeshes`]
/// built beside them, as [`slot_for`] prices the slot it makes.
///
/// # How the tile caches are bounded
///
/// One source owns one [`TileCache`]: a [`byte_lru::ByteLru`] whose budget
/// is the device's allowance for that source's population
/// (`squallar_device_profile::budget::TileCacheBudget`, pushed in every frame
/// through `MapTileState::set_budget`) and whose floor in entries is the
/// working set the last pass measured ([`HttpsTiles::note_wanted`]). There is
/// no count constant and no per-target cascade here any more. An LRU below
/// the working set is not a slower cache, it is a broken one — it evicts a
/// tile still on the glass and refetches it next frame, for something the
/// user never stopped looking at — and a count cannot say "48 MiB" when an
/// entry is anywhere from 456 bytes to a megabyte. The user's own 2878x1651
/// browser window at zoom 13.5 was the measurement: ~106 distinct tiles seen
/// against a cap of 100 — the working set the floor later measured there is
/// 174 — 93 % of asks refetches of tiles just evicted, 414 MB uploaded to
/// hold 18.8 MB, with nothing moving.
///
/// # Two slots, three populations
///
/// `MapTileState` holds one basemap source and one terrain source; a theme
/// flip restyles the basemap in place, so there is never a second copy of
/// either. (The four-source arithmetic this comment once carried — base and
/// labels, light and dark — priced a layout that no longer exists.) What the
/// two slots hold, and what one entry of each costs:
///
/// | slot    | population      | one entry                                | figure |
/// |---------|-----------------|------------------------------------------|--------|
/// | basemap | styled entries  | shapes + flattened buffers               | this constant (tail), [`TYPICAL_STYLED_ENTRY_BYTES`] (typical) |
/// | basemap | parsed geometry | the style-independent decode a restyle re-runs from | [`MEASURED_PARSED_TILE_BYTES`] (tail) |
/// | terrain | rasters         | one 256x256 RGBA texture                 | [`RASTER_TILE_BYTES`], no tail |
///
/// Every entry is charged at least [`byte_lru::MARKER_BYTES`], so a cache of
/// pending markers is bounded in bytes like everything else. Styled shapes and
/// parsed geometry are host allocations; only the terrain rasters (and a
/// styled tile's flattened buffers once uploaded) touch the GPU, and the
/// terrain population is omitted from `Budgets::app_texture_bytes` by name
/// (`the_terrain_rasters_are_omitted_from_the_gpu_sum_by_name`). The relation
/// this comment used to state in prose against
/// `WASM_APP_TEXTURE_BUDGET_BYTES` is an imported assertion now:
/// `the_two_slots_price_against_the_brackets_they_are_handed`.
///
/// # What the brackets hold
///
/// `squallar_device_profile::constants::WASM_TILE_STYLED_BYTES` carries the
/// whole argument. The user's window measured 86 tiles on the glass at a
/// whole zoom (32,104,551 B) and 174 at the half step that rounds onto the
/// archive's top level (60,080,378 B; Firefox, 2026-09-02, the floor in
/// place); at this tail 174 entries would be 254 MB, which the 48 MiB wasm
/// floor **cannot hold** (34 entries) and is not asked to — the working-set
/// floor keeps them resident as overrun, and whole-zoom snapping (the
/// tile-sharpness rung, [`snap`]) is what brings such a scene back under
/// budget. At the typical cost the same floor holds 1,600
/// entries. [`worst_case_entries`] is the arithmetic, for the tests and for
/// `squallar_gpu`'s mirror rung cap.
///
/// # The figure, and how it is re-derived
///
/// The committed Monaco fixture's z14 city-core tile (185,182 MVT bytes)
/// styled against the committed dark style renders to ~740 shapes; the shapes
/// counted at **capacity** measured 652,112 bytes (2026-08-28), and the
/// flattened buffers beside them — fills **and strokes**, at a feathering of
/// 1.0 point — measured 810,468 (2026-09-02), which with the marker's node is
/// the figure below. It is the slot's own [`CachedTile::bytes`], so what is
/// priced is what is resident. The plan this landed under quoted ~1.03 MB;
/// that figure had the fills and not the strokes, and the strokes are more
/// than half the flattened half. The brackets were argued from 1.03 MB and
/// are re-argued from this on `WASM_TILE_STYLED_BYTES`; nothing in them was
/// raised to meet it.
///
/// `tile_source::tests::the_vector_entry_cost_is_what_the_fixture_actually_renders`
/// holds a **band** (`heap <= CONST <= 2 * heap`) and deliberately not an
/// equality, because `size_of` and allocator rounding are toolchain
/// properties. So it cannot catch this constant drifting upward into a safe
/// over-estimate: re-derive it by forcing that assertion to fail, never by
/// inference from a type's field list. An earlier version of this comment
/// predicted a figure from a struct's fields and was wrong by exactly the
/// amount it computed; the lesson is kept, the arithmetic is not.
pub const MEASURED_STYLED_ENTRY_BYTES: usize = 1_462_708;

/// What a typical dense-city styled entry costs — an estimate, carried with
/// its direction. The committed Monaco fixture's mean over all 246 tiles it
/// holds measured 13,461 bytes of styled shapes (2026-08-28), and the whole
/// slot ran 2.24x the shapes on the measured tail
/// ([`MEASURED_STYLED_ENTRY_BYTES`] against 652,112), so a typical whole
/// entry is ~30.2 KB, rounded down to 30,000. An estimate from a measured
/// mean and a measured ratio, not a measured mean of the whole entry; if it
/// is wrong it is wrong low, and the bracket arguments that lean on it
/// (1,600 entries in the wasm floor) hold fewer tiles than they say.
pub const TYPICAL_STYLED_ENTRY_BYTES: usize = 30_000;

/// How many worst-case styled entries `budget_bytes` holds: the figure a
/// count-shaped question about the cache gets now that the cache is in bytes.
pub fn worst_case_entries(budget_bytes: u64) -> usize {
    (budget_bytes / MEASURED_STYLED_ENTRY_BYTES as u64) as usize
}

/// The host heap behind one styled vector tile's shapes, counted at
/// **capacity**, because capacity is what is resident while the tile is
/// cached: the shape spine, mesh vertices and indices, path points and label
/// strings. Exact on `Vec` capacities; the `size_of` terms are the
/// toolchain's. Run once per tile where the styling ran, never per frame.
pub fn styled_heap_bytes(shapes: &[walkers::ShapeOrText]) -> usize {
    std::mem::size_of_val(shapes)
        + shapes
            .iter()
            .map(|shape| match shape {
                walkers::ShapeOrText::Shape(egui::Shape::Mesh(mesh)) => {
                    std::mem::size_of::<egui::Mesh>()
                        + mesh.vertices.capacity() * std::mem::size_of::<egui::epaint::Vertex>()
                        + mesh.indices.capacity() * std::mem::size_of::<u32>()
                }
                walkers::ShapeOrText::Shape(egui::Shape::Path(path)) => {
                    std::mem::size_of::<egui::epaint::PathShape>()
                        + path.points.capacity() * std::mem::size_of::<egui::Pos2>()
                }
                walkers::ShapeOrText::Text(text) => text.text.capacity(),
                walkers::ShapeOrText::Shape(_) => 0,
            })
            .sum::<usize>()
}

/// What one cached raster tile costs: 256x256 RGBA.
pub const RASTER_TILE_BYTES: usize = 256 * 256 * 4;

/// The measured worst-case heap of one **parsed** vector tile, in bytes —
/// the second resident population the styled-entry figure above does not
/// cover.
///
/// Same tile, same method as [`MEASURED_STYLED_ENTRY_BYTES`]: the committed
/// Monaco fixture's z14 city-core tile (185,182 MVT bytes), counted at
/// **capacity** by `walkers::mvt::ParsedTile::heap_bytes` — decoded geometry,
/// per-feature property bags, key and value strings. Measured 2026-08-29 by
/// forcing
/// `tile_source::tests::the_parsed_entry_cost_is_what_the_fixture_actually_parses`
/// to fail; the band there is the derivation, this line is only its record.
/// **Twice the styled entry**, which is why the parsed cache has its own byte
/// allowance beside the styled one's rather than sharing it.
///
/// The composition matters, because the first measurement was **29,903,162 B**
/// and the difference is a fixed defect, not a re-count: `mvt-reader` grows
/// every ring at `Vec::with_capacity(<whole feature's command count>)`, so
/// geometry capacity was 28.1 MB for 318 KB of shrunk content.
/// `walkers::mvt::parse` now shrinks per feature (see `shrink_geometry`
/// there); what remains is 1,400,495 B of per-feature property bags — 2,913
/// features, 14,303 properties, a `HashMap` apiece — 317,736 B of geometry,
/// and the spine.
///
/// Like its styled sibling: re-derive it by forcing the test's band to fail,
/// never by inference from a type's field list — the band cannot catch this
/// constant drifting upward into a safe over-estimate.
pub const MEASURED_PARSED_TILE_BYTES: usize = 2_092_002;

/// A basemap styling: the built style, and the key that built it.
///
/// **Together, because a source that held one without the other could offload
/// a batch styled differently from the tiles beside it.** The frame's inline
/// path renders against the `Style`; the worker is handed only the `key` and
/// builds its own `Style` from the same compiled-in JSON
/// ([`squallar_basemap::style`]). The two are derived in one place so they
/// cannot drift.
#[derive(Clone)]
pub struct BasemapStyling {
    pub style: Arc<Style>,
    /// The key `style` was built from, or `None` for a source that has no
    /// style to speak of.
    ///
    /// A source without a key **never offloads**: the worker rebuilds the
    /// style from the key, so a batch posted without one could not be
    /// guaranteed to be styled the way the tiles already on the glass were.
    /// That is not a limitation in practice — the sources without a key are
    /// the terrain archive and the raster HTTP providers, whose bodies are
    /// pictures and belong on the frame thread's texture upload anyway.
    pub key: Option<squallar_basemap::jobs::StyleKey>,
}

impl BasemapStyling {
    /// The committed style for a theme and a disabled-source-layer set, with
    /// the key that names it.
    pub fn committed(is_dark: bool, disabled: &std::collections::BTreeSet<String>) -> Self {
        Self {
            style: crate::basemap_style::committed_filtered(is_dark, disabled),
            key: Some(squallar_basemap::jobs::StyleKey {
                is_dark,
                disabled: disabled.clone(),
            }),
        }
    }

    /// A specific style with **no key**, so the source that holds it never
    /// offloads.
    ///
    /// Tests only, and named rather than defaulted: several of this module's
    /// fixtures build a style directly to assert on what it draws, and are not
    /// about the worker at all. Production code reaches a keyless styling
    /// through [`Self::raster`], which says *why* there is no key; this one
    /// would be a way to lose the offload silently.
    // Gated exactly as its callers are (`tile_source::tests` is
    // `all(test, not(wasm32))`), so the wasm arm does not compile a function
    // nothing there can reach.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn keyless(style: Arc<Style>) -> Self {
        Self { style, key: None }
    }

    /// A source with no style to restyle — the terrain archive and the raster
    /// HTTP providers. See [`Self::key`].
    pub fn raster() -> Self {
        Self {
            style: Arc::new(Style::default()),
            key: None,
        }
    }
}

/// The **undecoded** MVT bodies a wasm archive source keeps, keyed by
/// [`TileId`] — what makes a theme flip cost no archive read once the pump is
/// offloading.
///
/// It exists because [`SharedParsedTiles`] stops being filled on the offload
/// path: the worker holds the parse and the page never sees one, so the
/// restyle route that cache was built for
/// ([`read_one`]'s `remembered` arm) would find nothing and re-read the
/// archive for every visible tile. On wasm32 that is a range request each,
/// over a network, for a theme flip that used to touch nothing.
///
/// **Bodies rather than parses, and it is the cheaper cache of the two.**
/// Measured over the committed Monaco archive: a body is 2,411 bytes at the
/// median and 185,182 at the tail, against a parse's 15,389 and 2,092,002 —
/// the same tile costs roughly an eighth as much held this way. It is also
/// page-side, where this workspace has measured its ceiling, rather than on
/// the worker, whose headroom nothing has read.
///
/// `Arc` because the same buffer is the one the pump staged and the one the
/// job carried.
///
/// Charged at the body's own length: bodies are the cheaper population to
/// hold — median 2,411 B against 15,389 B for a parse over 52 tiles of the
/// committed archive — and they share the parsed allowance, because at most
/// one of the two grows on a given arm: with the offloader installed the
/// worker holds the parse and only bodies are remembered; without it the pump
/// remembers both, which is the degraded no-worker configuration.
type SharedTileBodies = Arc<Mutex<byte_lru::ByteLru<TileId, Arc<Vec<u8>>>>>;

/// What the archive IO task carries bodies in — the cache on wasm32 (`None`
/// for a source with no restyle to serve, the terrain hillshade), nothing on
/// native, where a tile is decoded on the blocking pool and there is no
/// second styling to serve. A type alias so [`read_one`] keeps one body.
#[cfg(target_arch = "wasm32")]
type IoTileBodies = Option<SharedTileBodies>;
/// See the wasm32 arm above.
#[cfg(not(target_arch = "wasm32"))]
type IoTileBodies = ();

/// The parsed-geometry cache one archive source's IO task and frame side
/// share: the style-independent half of every vector tile the source has
/// decoded, keyed by [`TileId`], each charged at
/// `walkers::mvt::ParsedTile::heap_bytes`.
///
/// **Economy, with no floor.** It exists so a style change — a theme flip, a
/// map-detail toggle — re-styles from the cached parse with zero fetches and
/// zero re-parses ([`HttpsTiles::set_style`]); an entry evicted before the
/// restyle costs one refetch for that tile, exactly the pre-split behaviour,
/// and never a frame. So unlike the styled cache its working set is a target
/// and not a floor, and its budget (the device's parsed allowance) is the
/// whole of what bounds it.
///
/// **On wasm32 the restyle is served from the undecoded bodies instead once
/// the pump offloads**, and here is the price, per visible tile of the
/// committed Monaco z14 city-core fixture (185,182 B body, native release,
/// n=30, all terms timed in the same interleaved rounds, on a box held
/// quiet), in the configuration that ships:
///
/// | | frame thread | worker |
/// | --- | --- | --- |
/// | from the parsed cache | style **2,930 us** | — |
/// | from the body cache (wasm32) | wire decode 37 + flatten 231 = **268 us** | parse 1,744 + style 2,930 |
///
/// The re-parse is real — 1,744 us, 59.5 % of a styling — and it buys the
/// removal of 2,930 us from the frame thread, a 10.9x cheaper theme flip
/// there. `flatten` is the dominant term left on the frame thread; shipping
/// the flattened [`crate::tile_mesh::TileMeshes`] over the wire is a named
/// follow-on, not this row.
///
/// A `Mutex` because on native the IO runtime's blocking pool writes it while
/// the frame side holds a handle to budget and trim it; on wasm every party is
/// the page thread and the lock is never contended.
type SharedParsedTiles = Arc<Mutex<byte_lru::ByteLru<TileId, Arc<walkers::mvt::ParsedTile>>>>;

/// What the archive IO task styles a tile against, and which restyle
/// generation that styling belongs to. Written by [`HttpsTiles::set_style`],
/// read by [`read_one`] once per tile — together under one lock, because a
/// style observed with another generation's number would let a stale styling
/// be cached as current. Native only: on wasm32 the frame pump is the styling
/// side and owns the live style directly.
#[cfg(not(target_arch = "wasm32"))]
struct StyleSlot {
    style: Arc<Style>,
    epoch: u64,
    /// The tessellator feathering the IO task must flatten strokes at, in
    /// points. Travels with the style because it is the other flatten input
    /// and the two must be read under one lock; see
    /// [`HttpsTiles::set_feathering`].
    feathering: f32,
}

/// What the archive IO task carries to style against — the slot on native,
/// nothing on wasm32, where styling happens in the frame pump. A type alias
/// so [`serve_archive_continuously`] and [`read_one`] keep one body each.
#[cfg(not(target_arch = "wasm32"))]
type IoStyleSlot = Arc<std::sync::RwLock<StyleSlot>>;
/// See the native arm above.
#[cfg(target_arch = "wasm32")]
type IoStyleSlot = ();

/// The style generation raster payloads carry: raster sources never restyle,
/// so their frame-side epoch stays at this value and every payload matches.
const RASTER_STYLE_EPOCH: u64 = 0;

/// What a source flattens strokes at, given what the frame side has told it.
///
/// Zero until it has said anything, and zero is the value
/// `tile_mesh::stroke::is_open_stroke` refuses every path at — so a
/// source that was never told keeps its strokes on the CPU rather than baking
/// them at a guessed `pixels_per_point`.
fn flatten_feathering(feathering: Option<f32>) -> f32 {
    feathering.unwrap_or(0.0)
}

/// One slot of [`HttpsTiles`]' tile cache: the styled tile — or the
/// pending/failed `None` marker — and the style generation it belongs to.
///
/// The epoch is what makes a restyle seamless: [`HttpsTiles::set_style`] bumps
/// the source's generation without clearing anything, a slot from an older
/// generation keeps **drawing** (a stale-styled tile beats a blank one) while
/// [`HttpsTiles::request_once`] re-asks for it, and the restyled arrival
/// overwrites it. A `None` marker from an older generation is re-asked too —
/// "failed, do not ask again" is a per-style verdict only as far as the next
/// restyle, which for an archive source re-asks the parsed cache, not the
/// network.
struct CachedTile {
    epoch: u64,
    tile: Option<Tile>,
    /// The tile's tessellated fills, flattened for the renderer — `None` for
    /// a raster tile, a pending marker, or a vector tile whose style produced
    /// no fills at this zoom. Built beside the styling, on the thread that
    /// did the styling, so it never costs the frame more than the styling
    /// already does; see [`crate::tile_mesh`].
    meshes: Option<Arc<crate::tile_mesh::TileMeshes>>,
    /// Whether this slot's tile is drawing under a generation
    /// [`HttpsTiles::request_once`] has since re-asked for. Set by the
    /// stale-generation re-stamp, cleared by the arrival that replaces the
    /// slot — so the arrival can be told from a body that landed on a tile
    /// nobody re-asked for. Read by [`TileCache::put`] and nothing else.
    restyle_pending: bool,
    /// What the slot is charged in the cache: [`byte_lru::MARKER_BYTES`] for
    /// a marker; for a tile, the marker plus [`Self::priced`]'s figure,
    /// computed once where the styling ran ([`slot_for`]) and never per frame.
    bytes: u64,
}

impl CachedTile {
    /// A pending or failed marker under `epoch`: no tile, no buffers, the
    /// node's own charge.
    fn marker(epoch: u64) -> Self {
        Self {
            epoch,
            tile: None,
            meshes: None,
            restyle_pending: false,
            bytes: byte_lru::MARKER_BYTES,
        }
    }

    /// What a tile and its flattened buffers occupy, in bytes: the texture's
    /// own size for a raster ([`RASTER_TILE_BYTES`] for the 256x256 RGBA every
    /// archive here serves), the styled shapes' heap plus the buffers for a
    /// vector tile. See [`MEASURED_STYLED_ENTRY_BYTES`] for what the vector
    /// figure measures on the fixture.
    fn priced(tile: &Tile, meshes: Option<&Arc<crate::tile_mesh::TileMeshes>>) -> u64 {
        match tile {
            Tile::Raster(texture) => {
                let [width, height] = texture.size();
                (width * height * 4) as u64
            }
            Tile::Vector(shapes) => {
                styled_heap_bytes(shapes) as u64 + meshes.map_or(0, |meshes| meshes.bytes())
            }
        }
    }

    /// What this slot is charged. See [`Self::bytes`].
    fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// The cache slot a decoded tile becomes.
///
/// **Built where the tile was decoded**, which is the IO runtime's blocking
/// pool on native and the frame pump's decode budget on wasm32 — the same two
/// places the styling itself runs, so flattening the fills and the strokes is
/// billed to the side that already pays for tessellating them.
///
/// `feathering` is the stroke half's other input, in points. A tile flattened
/// at one feathering and drawn at another would paint wrong-width roads, so
/// the ground phase compares the two and declines the run; see
/// [`HttpsTiles::set_feathering`] for what re-flattens it.
fn slot_for(tile: Tile, epoch: u64, feathering: f32) -> CachedTile {
    let meshes = match &tile {
        Tile::Vector(shapes) => {
            let flat = crate::tile_mesh::flatten(shapes, feathering);
            (!flat.is_empty()).then(|| Arc::new(flat))
        }
        // A raster tile has no geometry to flatten, so `feathering` is not
        // consulted on this arm.
        Tile::Raster(_) => None,
    };
    // Priced here, on the thread that styled and flattened, so the frame
    // never walks a tile's shapes to learn what it holds.
    let bytes = byte_lru::MARKER_BYTES + CachedTile::priced(&tile, meshes.as_ref());
    CachedTile {
        epoch,
        tile: Some(tile),
        meshes,
        restyle_pending: false,
        bytes,
    }
}

/// How many evictions per entry of the working-set floor [`TileCache`]
/// remembers, so a refetch can be told from a first sight.
///
/// Four, because a refetch is a tile the viewport still wants and the
/// viewport is what the floor holds: the id being re-asked was evicted while
/// the floor was breached, which the floor forbids, so on a static viewport
/// this memory should never be consulted at all and four working sets of
/// history is room to spare for the cases (a floor one pass behind a resize,
/// a pan) where it is. Beyond that the memory forgets and the refetch is
/// miscounted as a first put — under, never over. Never smaller than four
/// times [`MAX_IN_FLIGHT`], so a source that has drawn nothing yet still
/// remembers what it let go.
const EVICTED_MEMORY_PER_FLOOR_ENTRY: usize = 4;

/// The most requests one source can have open at once — asked for, and not
/// yet answered on the frame side — and so the most ids
/// [`TileCache::in_flight`] holds on the native arm. Three bounded queues
/// stand between an ask and its answer: the request channel, the IO task's
/// own concurrency, and the completion channel. Both channels are
/// `channel(MAX_PARALLEL_DOWNLOADS)` with a single sender, and a futures
/// channel holds `buffer + senders`, so `MAX_PARALLEL_DOWNLOADS + 1` apiece;
/// the IO task holds at most [`MAX_PARALLEL_DOWNLOADS`] fetches, the one
/// whose result it is handing over included. Twenty at the shipped six. A
/// full request channel makes [`HttpsTiles::request_once`]'s `try_send` fail,
/// and a failed send opens nothing.
///
/// On wasm32 the offload path holds bodies past the completion channel —
/// staged for, or riding in, a worker batch — and those requests stay open
/// until the reply is installed or dropped; each id is there at most once,
/// because an open request is never re-asked, so that arm's set is bounded by
/// this plus the batch's contents.
const MAX_IN_FLIGHT: usize = 3 * MAX_PARALLEL_DOWNLOADS + 2;

/// One source's tile cache, with what [`cache_ledger`] needs read **at the
/// cache**: which kind of slot a landing body replaced, and whether an ask is
/// for an id this cache recently let go of.
///
/// A wrapper rather than the bare `LruCache` because the four sites that put
/// (the native drain, the wasm32 inline decode, the wasm32 batch reply and
/// the test fixture) and the one that asks must all classify the same way;
/// the classification lives here once and the callers keep their `put`.
struct TileCache {
    role: cache_ledger::CacheRole,
    /// The slots, bounded in bytes by the device's allowance for this
    /// source's population and floored in entries at the working set the
    /// last pass measured — see [`byte_lru`] for the two conditions an
    /// eviction needs.
    slots: byte_lru::ByteLru<TileId, CachedTile>,
    /// Ids this cache evicted recently, most recent last — bounded at
    /// [`EVICTED_MEMORY_PER_FLOOR_ENTRY`] times the floor, so it follows the
    /// working set it remembers for. Probed and popped by [`Self::ask`] and by
    /// a [`Self::put`] that finds no slot.
    evicted: LruCache<TileId, ()>,
    /// Ids with a request out — asked for, and not yet answered by a body
    /// landing, a body dropped for its generation, or the IO side's word
    /// that none is coming. **The authority for "do not ask again".**
    ///
    /// The `None` marker in [`Self::slots`] used to be that authority, and
    /// it is an LRU citizen: under pressure the LRU evicts it before the body
    /// lands, [`HttpsTiles::request_once`] finds no slot and asks again, and
    /// one tile is fetched, decoded and uploaded twice — 16 duplicate puts
    /// over 607 asks at cap 100 on a 12x12 grid, on the loopback pin. This
    /// set is what the LRU cannot touch. The marker stays: it still reserves
    /// the slot and still says "failed, do not ask again" once the request
    /// is over, and its eviction is still counted.
    ///
    /// Bounded by construction at [`MAX_IN_FLIGHT`] on the native arm; see
    /// the constant for the wasm32 offload path's extra term.
    in_flight: HashSet<TileId>,
    /// This source's own reading of every event it recorded, moved by the
    /// same [`cache_ledger::Totals::apply`] the statics mirror — so a test
    /// can read one source without another source's events in the number.
    stats: cache_ledger::Totals,
}

/// How many evicted ids a cache with `floor` entries of working set remembers.
fn evicted_memory(floor: usize) -> NonZeroUsize {
    NonZeroUsize::new((floor.max(MAX_IN_FLIGHT)).saturating_mul(EVICTED_MEMORY_PER_FLOOR_ENTRY))
        .expect("the floor is raised to MAX_IN_FLIGHT, which is not zero")
}

impl TileCache {
    /// An empty cache allowed `budget_bytes` of residency, with no working
    /// set yet.
    fn new(budget_bytes: u64, role: cache_ledger::CacheRole) -> Self {
        Self {
            role,
            slots: byte_lru::ByteLru::new(budget_bytes),
            evicted: LruCache::new(evicted_memory(0)),
            in_flight: HashSet::with_capacity(MAX_IN_FLIGHT),
            stats: cache_ledger::Totals::default(),
        }
    }

    /// Allow `bytes` of residency. Evicts nothing here — see
    /// [`byte_lru::ByteLru::set_budget`]; the debt is paid by puts and by
    /// [`Self::trim_one`] from the pump.
    fn set_budget(&mut self, bytes: u64) {
        self.slots.set_budget(bytes);
        self.publish_levels();
    }

    /// Hold at least `entries` slots whatever the budget says: the working
    /// set the pass measured plus what may be in flight for it. The evicted
    /// memory follows it.
    fn set_floor_entries(&mut self, entries: usize) {
        self.slots.set_floor_entries(entries);
        let remembered = evicted_memory(entries);
        if self.evicted.cap() != remembered {
            self.evicted.resize(remembered);
        }
        self.publish_levels();
    }

    /// Pay one entry of shrink debt, if there is any: the least recent slot
    /// above the floor while the cache is over budget. Called once per pump.
    fn trim_one(&mut self) -> bool {
        match self.slots.trim_one() {
            Some(gone) => {
                self.let_go(gone);
                self.publish_levels();
                true
            }
            None => false,
        }
    }

    /// The mean charge of a resident slot, floored at the marker — what the
    /// zoom-bias gate multiplies a projected working set by.
    fn mean_entry_bytes(&self) -> u64 {
        self.slots.mean_entry_bytes()
    }

    /// The byte allowance in force. See [`byte_lru::ByteLru::budget`].
    fn budget(&self) -> u64 {
        self.slots.budget()
    }

    /// What the working set alone holds past the allowance. See
    /// [`byte_lru::ByteLru::floor_overrun_bytes`].
    fn floor_overrun_bytes(&self) -> u64 {
        self.slots.floor_overrun_bytes()
    }

    /// The slot under `tile_id`, refreshing its recency — a use, as
    /// `LruCache::get` is.
    fn get(&mut self, tile_id: &TileId) -> Option<&CachedTile> {
        self.slots.get(tile_id)
    }

    /// Whether `tile_id` holds a slot, without touching recency. Gated as
    /// its one caller, [`HttpsTiles::tile_is_cached`], is.
    #[cfg(test)]
    fn contains(&self, tile_id: &TileId) -> bool {
        self.slots.contains(tile_id)
    }

    /// Slots held, markers included. Gated as its one caller,
    /// [`HttpsTiles::cached_entries`], is.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn len(&self) -> usize {
        self.slots.len()
    }

    /// Requests open right now. Gated as its one caller,
    /// [`HttpsTiles::in_flight_len`], is.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    /// Whether a request for `tile_id` is out — asked for, and not yet put,
    /// dropped, or answered with nothing. What [`HttpsTiles::request_once`]
    /// consults before it sends, whatever the LRU did to the marker; see
    /// [`Self::in_flight`].
    fn is_in_flight(&self, tile_id: &TileId) -> bool {
        self.in_flight.contains(tile_id)
    }

    /// A request over with nothing to put: the IO side's word that no body
    /// is coming (a failed fetch, an archive with no tile there), a body
    /// dropped for a generation a restyle replaced, or a body that would not
    /// decode. Closes the request, so [`HttpsTiles::request_once`] may ask
    /// again when the marker is gone and the tile still wanted. A body that
    /// lands closes its request through [`Self::put`] instead.
    fn answered(&mut self, tile_id: &TileId) {
        self.in_flight.remove(tile_id);
    }

    /// This source's own counters. See [`Self::stats`]. Gated as its one
    /// caller, [`HttpsTiles::cache_stats`], is.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn stats(&self) -> cache_ledger::Totals {
        self.stats
    }

    /// [`HttpsTiles::request_once`]'s fresh ask: the request opened in
    /// [`Self::in_flight`], a `None` marker under `tile_id` at `epoch`, and
    /// one request counted — a refetch if this cache remembers evicting the
    /// id.
    fn ask(&mut self, tile_id: TileId, epoch: u64) {
        if self.evicted.pop(&tile_id).is_some() {
            self.note(cache_ledger::CacheEvent::RefetchAfterEviction);
        }
        self.note(cache_ledger::CacheEvent::Request);
        self.in_flight.insert(tile_id);
        self.insert(tile_id, CachedTile::marker(epoch));
    }

    /// [`HttpsTiles::request_once`]'s stale-generation arm: the request
    /// opened in [`Self::in_flight`], and the slot keeps its tile, re-stamped
    /// as "a request is out" under `epoch`.
    fn re_ask(&mut self, tile_id: TileId, epoch: u64) {
        if let Some(slot) = self.slots.get_mut(&tile_id) {
            slot.epoch = epoch;
            slot.restyle_pending = true;
        }
        self.note(cache_ledger::CacheEvent::RestyleAsk);
        self.in_flight.insert(tile_id);
    }

    /// A tile landing, classified by what it found under its id. See
    /// [`cache_ledger`] for the four kinds. Closes the id's request in
    /// [`Self::in_flight`]: this is the arrival the entry was open for.
    fn put(&mut self, tile_id: TileId, slot: CachedTile) {
        let asked = self.in_flight.remove(&tile_id);
        let kind = match self.slots.peek(&tile_id) {
            // A pending marker, of any generation: nothing drew yet, so this
            // is the tile's first sight whatever the marker was stamped.
            Some(CachedTile { tile: None, .. }) => cache_ledger::PutKind::First,
            Some(CachedTile {
                restyle_pending: true,
                ..
            }) => cache_ledger::PutKind::Restyle,
            Some(_) => cache_ledger::PutKind::Duplicate,
            None => {
                // No slot. With a request open, the LRU let the marker go
                // while the body was on its way — the eviction is already
                // counted, and the id is popped here so the memory holds
                // only ids that are gone — and the body is the tile's first
                // sight all the same, and its only one. With no request
                // open, nothing asked for this body.
                self.evicted.pop(&tile_id);
                if asked {
                    cache_ledger::PutKind::First
                } else {
                    cache_ledger::PutKind::Orphan
                }
            }
        };
        self.note(cache_ledger::CacheEvent::Put(kind));
        self.insert(tile_id, slot);
    }

    /// The put, with its evictions accounted: every slot the byte bound let
    /// go of is an eviction, remembered and classified by what it held, and
    /// handed off the frame thread; a slot replaced under the same id is not
    /// an eviction, and leaves the same way.
    fn insert(&mut self, tile_id: TileId, slot: CachedTile) {
        let bytes = slot.bytes();
        let mut evicted = Vec::new();
        if let Some(replaced) = self.slots.put(tile_id, slot, bytes, &mut evicted) {
            discard_slot("tile-cache-replace", replaced);
        }
        for gone in evicted {
            self.let_go(gone);
        }
        self.publish_levels();
    }

    /// One eviction's bookkeeping: the id remembered, the kind and the bytes
    /// counted, the payload handed to the discard sink so a styled tile's
    /// shapes are never freed on the frame thread. See [`discard_slot`].
    fn let_go(&mut self, gone: byte_lru::Evicted<TileId, CachedTile>) {
        self.evicted.push(gone.key, ());
        let kind = if gone.value.tile.is_none() {
            cache_ledger::EvictedKind::Pending
        } else {
            cache_ledger::EvictedKind::Resident
        };
        self.note(cache_ledger::CacheEvent::Evicted {
            kind,
            bytes: gone.bytes,
        });
        discard_slot("tile-cache-evict", gone.value);
    }

    /// The levels, stored where they move: on this source's own reading and
    /// on the role's statics.
    fn publish_levels(&mut self) {
        let levels = cache_ledger::Levels {
            resident_entries: self.slots.len() as u64,
            resident_bytes: self.slots.resident_bytes(),
            overrun_bytes: self.slots.overrun_bytes(),
            floor_entries: self.slots.floor_entries() as u64,
        };
        self.stats.resident_entries = levels.resident_entries;
        self.stats.resident_bytes = levels.resident_bytes;
        self.stats.overrun_bytes = levels.overrun_bytes;
        self.stats.floor_entries = levels.floor_entries;
        cache_ledger::set_resident(self.role, levels);
    }

    fn note(&mut self, event: cache_ledger::CacheEvent) {
        self.stats.apply(event);
        cache_ledger::note(self.role, event);
    }
}

/// One grid cell's answer: the piece [`Tiles::at`] would have given, plus the
/// flattened fills of the tile it came from.
///
/// The two travel together because they must describe the same cache slot: a
/// stretched ancestor's fills are the *ancestor's*, and a piece paired with
/// another tile's buffers would draw the wrong geography under the right
/// clip.
pub(crate) struct GroundPiece {
    pub(crate) tile: Tile,
    pub(crate) uv: egui::Rect,
    pub(crate) meshes: Option<Arc<crate::tile_mesh::TileMeshes>>,
}

/// How an archive source's tile bodies become [`Tile`]s — decided **once per
/// archive from its header's `tile_type`**, never by sniffing bodies.
///
/// The seam exists because two archives now flow through the same fetch loop
/// with different pixels in them: the basemap (`tile_type = 1`, MVT) and the
/// terrain hillshade (`tile_type = 4`, WebP). Before it, every body went
/// through `walkers::Tile::new`, which *guesses* — image decode if any decoder
/// recognises the bytes, MVT otherwise — so a raster archive would have
/// painted raw gdaldem grey over the map and a corrupt MVT body would be
/// sniffed rather than reported. The header names what the archive holds;
/// [`decode_archive_tile`] obeys it.
///
/// `pub(crate)` rather than `pub`: [`Self::TerrainRgb`] is a claim a *caller*
/// makes about an archive whose header cannot carry the distinction, but the
/// only thing that consumes such a claim is
/// [`HttpsTiles::from_range_source`], which is itself `pub(crate)` — so no
/// caller outside this crate could pass one even in principle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArchiveTileKind {
    /// `tile_type = 1` (MVT): tessellated against the committed style via
    /// [`Tile::from_mvt`] — an image body in a vector archive is an error,
    /// not a fallback.
    Vector,
    /// A raster `tile_type` (2 PNG, 3 JPEG, 4 WebP, 5 AVIF): decoded as an
    /// image and remapped by [`crate::terrain::decode_hillshade_tile`]. The
    /// remap is unconditional because the app's only raster archive is the
    /// terrain hillshade; a future plain-raster archive must add its own arm
    /// here rather than inherit the remap.
    Hillshade,
    /// Packed elevation in a PNG body: the terrain-RGB archives
    /// ([`crate::tiles::HEIGHT_ARCHIVE_URL`]). **Never reachable from
    /// [`Self::from_tile_type`]** — `tile_type = 2` is PNG for both a
    /// hillshade and an elevation grid, and no header field separates them —
    /// so it is only ever a *declaration*, cross-checked against the header
    /// at open (see [`serve_archive_continuously`]).
    ///
    /// It exists so that an elevation archive reaching a picture path fails
    /// loudly. There is no image in these bodies: each pixel is a base-256
    /// height triple, and a compositor handed one paints noise that looks
    /// like terrain.
    ///
    /// **Nothing in the shipped build constructs it, and that is the design,
    /// not an oversight.** The only consumer of a declaration is
    /// [`HttpsTiles::from_range_source`], and the height reader deliberately
    /// does not build an `HttpsTiles` at all — heights are read as bytes
    /// through [`crate::tiles::height_range_source`]. So the arms that receive
    /// this variant are the guard rail for the day something *does* route an
    /// elevation archive at the picture path, and only the tests reach them
    /// today. `expect` rather than `allow`, and only off the test cfg, so the
    /// first production construction makes this attribute unfulfilled and asks
    /// to be deleted.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "declared by callers of from_range_source; the height \
                      reader deliberately builds no HttpsTiles, so only the \
                      tests construct it until something does"
        )
    )]
    TerrainRgb,
    /// `tile_type = 0`: the header declares nothing, so the body is sniffed
    /// exactly as a plain HTTP tile is. No archive this app opens says this;
    /// the arm keeps "unknown" behaving as the pre-seam code did instead of
    /// picking a guess of its own.
    Undeclared,
}

/// Turn one archive tile body into a [`Tile`], the way `kind` — the archive
/// header's word — says to. See [`ArchiveTileKind`] for why the header and
/// never the bytes.
///
/// Costs where it runs: on native this is called on the IO runtime's blocking
/// pool ([`read_one`]), on wasm32 under the frame pump's
/// [`WASM_TILE_DECODES_PER_PUMP`] budget — never freely on the frame thread.
pub(crate) fn decode_archive_tile(
    bytes: &[u8],
    kind: ArchiveTileKind,
    style: &Style,
    zoom: u8,
    ctx: &Context,
) -> Result<Tile, String> {
    let tile = match kind {
        // `Tile::from_mvt` fuses the two halves (`render` IS `styled(parse())`),
        // and this arm spells them separately for one reason: so that the
        // phase ledger covers BOTH decode paths. A tile source with no parsed
        // cache reaches here, and a split that only the remembering path
        // recorded would report a figure whose denominator silently depended
        // on which source served the tile.
        ArchiveTileKind::Vector => {
            let parsed = timed_parse(bytes)?;
            Ok(timed_styled(&parsed, style, zoom))
        }
        ArchiveTileKind::Hillshade => {
            let remapped = crate::terrain::decode_hillshade_tile(bytes)?;
            Ok(Tile::Raster(ctx.load_texture(
                "terrain-hillshade",
                remapped,
                Default::default(),
            )))
        }
        ArchiveTileKind::TerrainRgb => Err(
            "this archive carries packed elevation and has no picture in it: a \
             terrain-RGB body is a base-256 height triple per pixel, so it is \
             read as data and never decoded into a tile"
                .to_owned(),
        ),
        ArchiveTileKind::Undeclared => {
            Tile::new(bytes, style, zoom, ctx).map_err(|error| error.to_string())
        }
    }?;
    note_archive_decode(kind);
    Ok(tile)
}

/// Tell [`crate::basemap_ledger`] that one archive body of `kind` decoded.
///
/// **Called only where a decode returned `Ok`**, and by every decoder, so the
/// ledger's denominator is exactly "bodies that became a [`Tile`]". That now
/// includes bodies decoded on the WORKER: `note_archive_decode` runs on the
/// thread the tile lands on, not the thread it was parsed on, because the
/// worker's statics are not the page's and this counter is read on the page
/// (`squallar_egui::basemap_ledger`, scraped by the browser rig as its
/// basemap positive control). A tile that drew but was not counted here would
/// read as an archive serving nothing. The kind
/// is the archive header's word and never the bytes', which is the same rule
/// [`decode_archive_tile`] itself obeys — a ledger that sniffed would be
/// answering a different question from the decoder it is measuring.
///
/// [`ArchiveTileKind::TerrainRgb`] has no arm because it has no `Ok`: an
/// elevation body is refused, so nothing to count ever reaches here.
fn note_archive_decode(kind: ArchiveTileKind) {
    match kind {
        ArchiveTileKind::Vector => crate::basemap_ledger::note_vector_tile(),
        ArchiveTileKind::Hillshade => crate::basemap_ledger::note_raster_tile(),
        ArchiveTileKind::Undeclared => crate::basemap_ledger::note_sniffed_tile(),
        ArchiveTileKind::TerrainRgb => {}
    }
}

/// Style a parsed tile into the value the tile cache holds.
fn styled_tile(parsed: &walkers::mvt::ParsedTile, style: &Style, zoom: u8) -> Tile {
    timed_styled(parsed, style, zoom)
}

/// Whole microseconds a closure took, saturating into the ledger's `u32`.
fn micros_of<T>(work: impl FnOnce() -> T) -> (T, u32) {
    let began = web_time::Instant::now();
    let out = work();
    let cost = web_time::Instant::now().duration_since(began);
    (out, cost.as_micros().min(u128::from(u32::MAX)) as u32)
}

/// [`walkers::mvt::parse`], charged to [`take_ledger::VectorPhase::Parse`].
///
/// Two clock reads per vector **body**, not per layer and not per feature: a
/// per-feature clock would be thousands of reads on the hot path and is a
/// harness's job, not a ledger's.
fn timed_parse(bytes: &[u8]) -> Result<walkers::mvt::ParsedTile, String> {
    let (parsed, micros) = micros_of(|| walkers::mvt::parse(bytes));
    // Charged whether or not it succeeded: a body that failed to parse cost
    // the frame the same time, and excluding it would make a broken archive
    // read as a fast one -- the same rule the take families follow.
    take_ledger::note_vector_phase(take_ledger::VectorPhase::Parse, micros);
    parsed.map_err(|error| error.to_string())
}

/// [`walkers::mvt::styled`], charged to [`take_ledger::VectorPhase::Style`].
fn timed_styled(parsed: &walkers::mvt::ParsedTile, style: &Style, zoom: u8) -> Tile {
    let (shapes, micros) = micros_of(|| walkers::mvt::styled(parsed, style, zoom));
    take_ledger::note_vector_phase(take_ledger::VectorPhase::Style, micros);
    Tile::Vector(Arc::new(shapes))
}

/// [`decode_archive_tile`], with the parse half **remembered**: a vector
/// body's zoom- and style-independent decode lands in `parsed_tiles` before
/// the styling, so a later restyle of this tile ([`HttpsTiles::set_style`])
/// touches neither the network nor the bytes. The raster arms delegate
/// unchanged — there is nothing style-independent to keep for a pixel body.
///
/// Runs where [`decode_archive_tile`] runs: the IO runtime's blocking pool on
/// native, the frame pump's [`WASM_TILE_DECODES_PER_PUMP`] budget on wasm32.
fn decode_archive_tile_remembering(
    bytes: &[u8],
    kind: ArchiveTileKind,
    style: &Style,
    tile_id: TileId,
    ctx: &Context,
    parsed_tiles: &SharedParsedTiles,
    role: cache_ledger::CacheRole,
) -> Result<Tile, String> {
    match kind {
        ArchiveTileKind::Vector => {
            let parsed = Arc::new(timed_parse(bytes)?);
            let charge = parsed.heap_bytes() as u64;
            let (held, held_bytes) = {
                let mut parsed_tiles = parsed_tiles
                    .lock()
                    .expect("the parsed-tile cache is not poisoned");
                // What the byte bound lets go of is freed here, on the thread
                // that just paid for a parse of the same order: the IO
                // runtime's blocking pool on native, the pump's decode budget
                // on wasm32. The frame-paced trim is `HttpsTiles::pump`'s.
                let mut evicted = Vec::new();
                parsed_tiles.put(tile_id, Arc::clone(&parsed), charge, &mut evicted);
                (parsed_tiles.len() as u64, parsed_tiles.resident_bytes())
            };
            // A level, stored where the parse lands: the only site that
            // grows this cache, on either target.
            cache_ledger::set_parsed(role, held, held_bytes);
            // The vector arm does not delegate, so it counts for itself; the
            // raster arms are counted inside `decode_archive_tile`.
            note_archive_decode(ArchiveTileKind::Vector);
            Ok(timed_styled(&parsed, style, tile_id.zoom))
        }
        raster => decode_archive_tile(bytes, raster, style, tile_id.zoom, ctx),
    }
}

/// The `max_zoom` value that means "not read yet", not "zero".
///
/// An archive source does not learn its own depth until the IO task has read
/// the PMTiles header, and the two states have to be *different* values because
/// they mean opposite things to [`Tiles::at`]. Zero was used for both, and what
/// zero draws is why that was a defect rather than a nicety:
///
/// `ui_map_overlays::draw_tile_layer` clamps the tile zoom to
/// [`HttpsTiles::source_max_zoom`], so every pane's first frames asked for
/// `0/0/0` — and that tile then stayed in the LRU, where
/// [`HttpsTiles::cached_or_interpolated`] walked to it as the fallback ancestor
/// for any deep tile still in flight, for the rest of the session.
/// `full_rect_of_clipped_tile` places an ancestor against a rect of
/// `256 · 2^zoom` points, so the fixture's z0 tile — 7,889 bytes, six shapes —
/// was drawn 16,384 points across at app zoom 6 and 4,194,304 at zoom 14. Its
/// widest stroke measured 16 extent units, and `Shape::transform` scaled
/// `stroke.width` with the geometry, so a "blurry ancestor" painted as a
/// 64-point, 1,024-point and 16,384-point slab of solid colour.
///
/// 255 is safe as the sentinel because it is not a zoom: `tile_id_is_valid`
/// rejects every zoom at or past 32, since [`walkers::mercator::total_tiles`]
/// cannot count the grid there.
const MAX_ZOOM_UNKNOWN: u8 = u8::MAX;

// The per-tile request timeout used to be declared here as a second
// `Duration::from_secs(20)` beside `basemap_archive::REQUEST_TIMEOUT`, and the
// two could drift silently because nothing compared them. `tile_client` is now
// `archive_client`, so there is one client, one pool and one figure.

/// Tile decodes the frame side performs per **source** per **pass**, wasm32
/// only.
///
/// On native the IO thread decodes and uploads off the frame thread. On wasm
/// `spawn_local` runs the fetch loop on the page itself, so a pan exposing a
/// fresh row of tiles pays the decode+upload on the very thread the gesture
/// runs on.
///
/// **The count is a backstop; [`PUMP_TIME_BUDGET`] is the governor.** `tile_rx`
/// is `channel(MAX_PARALLEL_DOWNLOADS)` with a single sender, so the queue
/// holds at most [`MAX_PARALLEL_DOWNLOADS`] plus that sender's one guaranteed
/// slot — this exact figure — and this value means "whatever is queued"
/// rather than a throttle — the same figure and the same
/// meaning as [`NATIVE_TILE_UPLOADS_PER_PUMP`], because after the time bound
/// went in the two arms want the same rule.
///
/// It was 2, unconditionally, and that was the wrong shape in both directions.
/// A frame whose takes are cheap was held to 2 when the queue held 6, so a
/// zoom filled at a quarter of the rate the network was delivering; a frame
/// whose takes are expensive still paid *two* multi-millisecond tessellations,
/// because a count cannot see what a take costs. The time bound cuts the
/// expensive case to one take and lets the cheap case empty the queue.
///
/// Deliberately **capped inline, not offloaded**: tile PNGs behind the overlay
/// worker's job round-trip would trade a bounded per-pass cost for seconds of
/// blank basemap. Bounding a *single* take below the frame budget is what that
/// offload (C5p2) would buy and this cannot.
pub const WASM_TILE_DECODES_PER_PUMP: usize = MAX_PARALLEL_DOWNLOADS + 1;

/// The wall time one [`HttpsTiles::pump`] may spend taking completions off the
/// channel.
///
/// **This is the governor the gesture latch used to be.** Until WO-10 the pump
/// took *nothing* while a map gesture was live and resumed only after a 500 ms
/// wall clock, so a zoom that ended left the map unchanged for half a second
/// before the first tile could even be put. The cost that latch was dodging is
/// real — on wasm32 a take is a PNG decode and a texture upload, on native a
/// cache put — but it is a cost per take, and a count bound cannot see it.
/// A time bound can, and it needs no notion of a gesture at all: the frame
/// budget governs whether the map moves or not.
///
/// Checked *before* each take and never mid-take, so the true worst case is
/// this plus one take. Bounding one take's own cost is not something this
/// constant can do — that is the deferred worker-side decode (C5p2).
///
/// **A pass always takes at least one completion per source**, however far past
/// the deadline the frame already is. A budget that can round down to zero work
/// stalls the map exactly when the machine is busiest, which is the failure
/// this replaces rather than a different spelling of it. The guarantee is the
/// *pass's* and not each call's: see [`PumpBudget`], where spelling it per call
/// is the defect that shape had.
///
/// **1 ms, and the value is chosen for wasm32, because that is the only
/// target it binds on.** A frame's worst case is one of these plus one take
/// **per source** — two of each in a standard configuration, which draws the
/// basemap and the terrain. It is not per *layer-draw*: a source drawn in six
/// panes is pumped six times, and [`PumpBudget`] is what holds those six to
/// the one budget, in time as well as in count. Before it did, the same
/// sentence was true only of the single-pane layout it was written against.
///
/// A native take is an `LruCache::put` costing microseconds, so neither 1 ms
/// nor 2 ms is ever reached there and the count is what bounds a native pump.
/// A wasm32 take is a PNG decode and a texture upload costing milliseconds, so
/// this is the bound that actually fires — and halving it halves the worst
/// case on the target that needs it most.
///
/// **What the value governs is bursts, not steady state**, because the first
/// take is unconditional. Every frame therefore moves at least one tile per
/// source — two in a standard configuration — however small this is, which is
/// of the order of what [`MAX_PARALLEL_DOWNLOADS`] concurrent fetches deliver:
/// in steady state the floor is already keeping up and this bound is not what
/// the map is waiting on. It binds only against a backlog, and `tile_rx` holds
/// at most [`MAX_PARALLEL_DOWNLOADS`] of those: at one take per pass — which
/// is what 1 ms means when a take is a multi-millisecond tessellation — a full
/// queue clears in six frames, 100 ms at 60 Hz. That is the whole cost of the
/// smaller number, and against it the frame carries 1 ms per source.
///
/// **Measured, and it is not the whole story.** Native scene A interact p50
/// sits one histogram bin above the pre-WO-10 build: 26,909 us against
/// 32,000 us, base 3/3 and this build 2/2 across legs under loadavg 10,
/// counterbalanced base/head/head/base, viewport 1920x1080 verified by pid at
/// 17.97 MB/picture on every row. That gap did **not** move when this constant
/// went 2 ms -> 1 ms, which is what proves it is not this value's: on native
/// the bound never binds. What the gap is, is the pump draining on every frame
/// at all where WO-9e had it draining on none — up to
/// [`NATIVE_TILE_UPLOADS_PER_PUMP`] puts per layer per frame, each bumping
/// `put_generation` and so repainting the floor strip. The `pump` frame
/// segment carries it: p99 median 6,728 us base against 9,514 us here, the
/// same one bin.
///
/// **This is the receipt for worker-side tile decode (C5p2), which was
/// deferred as measurement-conditional and now has its measurement.** The bin
/// is buyable back, and only one way: by making a *take* cheaper. It cannot be
/// tuned away from here, because the bound this constant sets is never the one
/// that binds on the arm the regression was measured on — a smaller number
/// changes nothing, which is the experiment above. Off-thread decode is the
/// change that would, and a future reader should find this note rather than
/// re-derive it.
const PUMP_TIME_BUDGET: Duration = Duration::from_millis(1);

/// Completed fetches one [`HttpsTiles::pump`] moves into the cache, native
/// only.
///
/// Not a throttle, and not the counterpart of [`WASM_TILE_DECODES_PER_PUMP`]:
/// on native [`fetch_one`] already decoded and uploaded on the IO thread, so a
/// take here is a `LruCache::put` and nothing more. `tile_tx`/`tile_rx` is
/// `channel(MAX_PARALLEL_DOWNLOADS)` with a single sender, so the queue cannot
/// hold more than this, and the bound is only ever reached with an empty queue
/// behind it.
///
/// It is spelled rather than left as "drain until empty" so the loop is bounded
/// by a named figure and cannot spin against an IO thread refilling as fast as
/// it is drained. The figure is what the old shape already moved per pass:
/// [`Tiles::at`] took one tile per call and ran once per grid cell, and a layer
/// has tens of cells — 54 over a 1920x1080-**point** canvas at a whole zoom and
/// bias 0, by [`crate::tiles::tiles_resident_at_whole_zoom`] — so a pass already
/// emptied the queue. Pumping once per layer keeps that only by taking the whole
/// queue rather than one tile.
#[cfg(not(target_arch = "wasm32"))]
const NATIVE_TILE_UPLOADS_PER_PUMP: usize = MAX_PARALLEL_DOWNLOADS + 1;

// ---------------------------------------------------------------------------
// Target-dependent bounds
// ---------------------------------------------------------------------------

/// A [`TileSource`] that can be moved into the IO task. The bound differs by
/// target and this alias is where that difference lives: on native the task runs
/// on another thread and everything it captures must be `Send + Sync`; on wasm
/// it runs on the UI thread, where `reqwest`'s types are neither.
#[cfg(not(target_arch = "wasm32"))]
pub trait AsyncTileSource: TileSource + Send + Sync + 'static {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: TileSource + Send + Sync + 'static> AsyncTileSource for T {}

#[cfg(target_arch = "wasm32")]
pub trait AsyncTileSource: TileSource + 'static {}
#[cfg(target_arch = "wasm32")]
impl<T: TileSource + 'static> AsyncTileSource for T {}

/// What one completed fetch hands the frame side.
///
/// Native: the finished cache slot — [`fetch_one`] and [`read_one`] decode,
/// upload and flatten on the IO thread — carrying the style generation it was
/// styled under, so the frame side can drop a tile styled by a style a restyle
/// has already replaced ([`CachedTile`]). wasm32: the work still owed, under
/// the frame pump's [`WASM_TILE_DECODES_PER_PUMP`] budget — see
/// [`HttpsTiles::pump`]; no generation travels with it, because the pump
/// styles against the frame's current style by construction.
#[cfg(not(target_arch = "wasm32"))]
type FetchPayload = CachedTile;
#[cfg(target_arch = "wasm32")]
enum FetchPayload {
    /// A tile body the pump must decode — a compressed PNG from a raster
    /// source, an MVT body from the archive (parsed, remembered, styled).
    Bytes(Arc<Vec<u8>>),
    /// A parse the IO task found in [`SharedParsedTiles`]: the fetch was
    /// skipped entirely; only the styling remains, and it still bills the
    /// pump budget — tessellation is the heavy half.
    Parsed(Arc<walkers::mvt::ParsedTile>),
}

/// One request's answer, as the IO side hands it over: the payload, or
/// `None` for a request that gets no body — a fetch that failed, an archive
/// that positively holds no tile at that coordinate.
///
/// **The empty answer is delivered, not dropped.** The frame side keeps every
/// request open in [`TileCache::in_flight`] from the ask until something
/// arrives under its id, and a request that never arrived would be a tile
/// that is never asked for again once the LRU has let its marker go. The
/// failure itself is logged where it happened; the cache's `None` marker
/// under the id stays as it is — "asked, and nothing came" — which is what
/// "do not ask again" has always meant for a failed tile. An empty answer
/// changes no pixel, so it asks for no repaint, and it is no family's take in
/// [`take_ledger`]: nothing was decoded, uploaded or put.
type Fetched = (TileId, Option<FetchPayload>);

/// Managed IO runtime for the tile fetch task — and for
/// [`crate::basemap_download`], which owns the same kind of task for the same
/// reason and must not grow a second copy of this split.
pub(crate) mod runtime {
    #[cfg(not(target_arch = "wasm32"))]
    mod native {
        /// Owns the IO thread. Dropping it stops the fetch task.
        pub(crate) struct Runtime {
            join_handle: Option<std::thread::JoinHandle<()>>,
            quit_tx: tokio::sync::mpsc::UnboundedSender<()>,
        }

        /// Run `future` on a private current-thread runtime on its own thread.
        ///
        /// `spawn`ed rather than `block_on`'d, with the thread parked on a quit
        /// channel: dropping the runtime cancels an in-flight request outright,
        /// whereas blocking on the future would make [`Runtime::drop`] wait out
        /// the request on the UI thread.
        pub(crate) fn spawn<F>(future: F) -> Runtime
        where
            F: Future<Output = ()> + Send + 'static,
        {
            let (quit_tx, mut quit_rx) = tokio::sync::mpsc::unbounded_channel();

            let join_handle = std::thread::spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        log::error!("could not start the tile IO runtime: {error}");
                        return;
                    }
                };
                runtime.spawn(future);
                runtime.block_on(quit_rx.recv());
            });

            Runtime {
                join_handle: Some(join_handle),
                quit_tx,
            }
        }

        impl Drop for Runtime {
            fn drop(&mut self) {
                // Both of these fail only if the thread is already gone, which is
                // not a condition there is anything to do about.
                let _ = self.quit_tx.send(());
                if let Some(join_handle) = self.join_handle.take() {
                    let _ = join_handle.join();
                }
            }
        }

        /// A runtime that owns no thread and never ran a task — the inert
        /// source's slot. Dropping it sends the quit onto a channel nobody
        /// holds, which is already a condition [`Runtime::drop`] shrugs at.
        pub(crate) fn inert() -> Runtime {
            let (quit_tx, _quit_rx) = tokio::sync::mpsc::unbounded_channel();
            Runtime {
                join_handle: None,
                quit_tx,
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    mod web {
        /// There is no thread to own: the task runs on the browser's event loop.
        /// It still stops when [`super::super::HttpsTiles`] is dropped.
        pub(crate) struct Runtime;

        pub(crate) fn spawn<F>(future: F) -> Runtime
        where
            F: Future<Output = ()> + 'static,
        {
            wasm_bindgen_futures::spawn_local(future);
            Runtime
        }

        /// The inert slot: on this arm there was never a thread to not own.
        pub(crate) fn inert() -> Runtime {
            Runtime
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) use native::{Runtime, inert, spawn};
    #[cfg(target_arch = "wasm32")]
    pub(crate) use web::{Runtime, inert, spawn};
}

// ---------------------------------------------------------------------------
// Slippy-map helpers walkers keeps private
// ---------------------------------------------------------------------------

/// Number of tiles along one axis at `zoom`, or `None` if `zoom` is absurd.
///
/// walkers' `mercator::total_tiles` is `pub(crate)` and computes `2u32.pow(zoom)`,
/// which overflows for `zoom >= 32`. `TileId::zoom` is a `u8`.
fn total_tiles(zoom: u8) -> Option<u32> {
    2u32.checked_pow(zoom as u32)
}

/// Is this tile inside the grid for its own zoom level?
///
/// Reproduces `TileId::valid`, which walkers also keeps `pub(crate)`. Requesting
/// an out-of-range tile would 404 forever and burn a cache slot doing it.
fn tile_id_is_valid(tile_id: TileId) -> bool {
    match total_tiles(tile_id.zoom) {
        Some(side) => tile_id.x < side && tile_id.y < side,
        None => false,
    }
}

/// Locate `tile_id` inside its ancestor at `available_zoom`.
///
/// Returns the ancestor and the sub-rectangle of it that `tile_id` covers, in
/// texture (`uv`) coordinates — how a zoomed-in view stays populated with blurry
/// tiles instead of holes. Reproduces walkers'
/// `tiles::interpolate_from_lower_zoom`, which is `pub(crate)`.
///
/// # Panics
///
/// Debug-asserts `available_zoom <= tile_id.zoom`. Every call site walks `zoom`
/// downwards or clamps to `max_zoom`, after [`tile_id_is_valid`] has rejected
/// `zoom >= 32`, so the shift cannot overflow.
fn interpolate_from_lower_zoom(tile_id: TileId, available_zoom: u8) -> (TileId, egui::Rect) {
    debug_assert!(
        tile_id.zoom >= available_zoom,
        "cannot interpolate {tile_id:?} from the higher zoom {available_zoom}"
    );

    let dzoom = 2u32.pow((tile_id.zoom.saturating_sub(available_zoom)) as u32);

    let x = (tile_id.x / dzoom, tile_id.x % dzoom);
    let y = (tile_id.y / dzoom, tile_id.y % dzoom);

    let ancestor = TileId {
        x: x.0,
        y: y.0,
        zoom: available_zoom,
    };

    let z = (dzoom as f32).recip();
    let uv = egui::Rect::from_min_max(
        egui::pos2(x.1 as f32 * z, y.1 as f32 * z),
        egui::pos2(x.1 as f32 * z + z, y.1 as f32 * z + z),
    );

    (ancestor, uv)
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

/// The HTTP client every basemap tile is fetched with:
/// [`squallar_radar::tls::client`] and nothing else — platform verifier, *ring*,
/// `https_only`. There is deliberately no way to inject a different client from
/// outside the crate.
///
/// This is [`crate::basemap_archive::archive_client`], not a second client
/// built the same way: the two constructions were character-for-character
/// identical, down to the 20 s [`crate::basemap_archive::REQUEST_TIMEOUT`]
/// this module used to restate. Spelling it once means one process-wide client and one
/// connection pool rather than a fresh 4–5 ms platform-verifier build per tile
/// source — see that function for the measurement.
pub(crate) fn tile_client() -> reqwest::Client {
    crate::basemap_archive::archive_client()
}

// ---------------------------------------------------------------------------
// HttpsTiles
// ---------------------------------------------------------------------------

/// Drop-in replacement for [`walkers::HttpTiles`]. See the module docs for what
/// is reproduced and what deliberately differs.
///
/// Must persist between frames: the cache and the IO task live in it.
pub struct HttpsTiles {
    attribution: Attribution,
    tile_size: u32,

    /// The source's deepest zoom.
    ///
    /// Shared and atomic rather than a plain `u8` because an archive source
    /// does not know it at construction: the number is in the PMTiles header,
    /// which is read by the IO task. See [`HttpsTiles::from_archive_url`] for
    /// what it holds before the header arrives, and [`MAX_ZOOM_UNKNOWN`].
    max_zoom: Arc<AtomicU8>,

    /// Why this source will never serve a tile, once that is known.
    ///
    /// Written once by the IO task and read every frame, so a
    /// [`std::sync::OnceLock`] rather than a lock the frame has to take. Only
    /// the archive path ever sets it: a raster source's failures are per-tile
    /// and per-request, and a 404 on one tile says nothing about the next.
    fault: Arc<OnceLock<String>>,

    /// Whether this source's tile reads have been failing in an unbroken run
    /// of [`SUSTAINED_READ_FAILURES`] — set and cleared by the archive IO
    /// task, read every frame.
    ///
    /// **Not [`Self::fault`], and the difference is recovery.** A fault is
    /// permanent by construction — a `OnceLock`, and the slot that acts on it
    /// drops the source — which is right for an archive that will not open and
    /// wrong for reads that fail. This is the state a *per-tile* failure mode
    /// leaves the map in: a network that went, a corrupt downloaded part, an
    /// expired generation, an offset a 32-bit target cannot address. Each of
    /// them blanks the map while the archive itself logs as healthily open,
    /// because the header and the root directory fit inside the opening read.
    /// The frame side reads it so the credit corner can stop naming a provider
    /// whose bytes are not on the glass; a read that answers clears it.
    reads_failing: Arc<AtomicBool>,

    /// Set when the request channel has been found disconnected.
    ///
    /// **This is a latch, and it exists to bound a log flood.** A disconnected
    /// `Sender` reports `TrySendError::is_full() == false`, so the disconnect
    /// falls past the retry arm into an `error!`; and because the closure
    /// failed, `LruCache::try_get_or_insert` inserts nothing, so the very same
    /// tile is asked for again on the next frame. Measured against a viewport
    /// of 54 tiles that is 54 error lines per frame for as long as the app is
    /// open. The IO task being gone is a permanent condition, so it is worth
    /// exactly one line.
    requests_closed: bool,

    /// Tiles by id. A slot with `tile: None` means "asked for, not here yet"
    /// *and* "asked for, and it failed" — the two are deliberately
    /// indistinguishable, because both mean "do not ask again" — for as long
    /// as the slot's style generation is current. See [`Self::request_once`]
    /// and [`CachedTile`] for what a stale generation re-opens. The slot is
    /// an LRU citizen and can be evicted with its request still out;
    /// [`TileCache::in_flight`] is the record the LRU cannot evict, and the
    /// one [`Self::request_once`] consults before it sends. Wrapped by
    /// [`TileCache`], which classifies every put and eviction for
    /// [`cache_ledger`].
    cache: TileCache,

    /// The style generation the frame side currently wants — bumped by
    /// [`Self::set_style`], stamped on every slot, compared against arriving
    /// payloads on native. Stays [`RASTER_STYLE_EPOCH`] for a source that
    /// never restyles.
    style_epoch: u64,

    /// The tessellator feathering the frame side draws at, in points —
    /// `feathering_size_in_pixels / pixels_per_point`, and so a function of
    /// the display the window is on.
    ///
    /// `None` until the frame side has said, which is what keeps a source
    /// that was never told off the GPU stroke path rather than guessing at
    /// it: [`Self::flatten_feathering`] answers 0, which
    /// `tile_mesh::stroke::is_open_stroke` refuses.
    feathering: Option<f32>,

    /// Where a restyle lands for the IO task — the slot [`read_one`] reads a
    /// (style, generation) pair from, once per tile. `None` for a raster HTTP
    /// source, which has no style to swap. On wasm32 the frame pump is the
    /// styling side, so the slot exists only inside the task's ignored
    /// parameter and the live style is [`Self::style`].
    #[cfg(not(target_arch = "wasm32"))]
    style_slot: Option<Arc<std::sync::RwLock<StyleSlot>>>,

    /// The parsed-geometry cache shared with this source's IO task — `Some`
    /// for a vector archive source, `None` for a raster one (the terrain
    /// hillshade, a plain HTTP source, an inert source). On wasm32 the pump is
    /// the side that parses and remembers; on native the IO task is, and the
    /// frame side holds this clone to budget and trim it ([`Self::set_budget`],
    /// [`Self::pump`]).
    parsed: Option<SharedParsedTiles>,

    /// The undecoded bodies the wasm32 restyle path serves from, held here to
    /// budget and trim them for the reason [`Self::parsed`] is. `Some` only
    /// where the IO task remembers bodies — a vector archive on wasm32; see
    /// [`remember_body`] — and `None` everywhere else, so no arm of this
    /// struct's behaviour forks on the target.
    bodies: Option<SharedTileBodies>,

    /// What the passes drawing this source have said they want — the working
    /// set the cache's floor follows. See [`Self::note_wanted`].
    wanted: WantedTally,

    /// The asks the request channel refused, in the order it refused them, so
    /// the next pass asks for them first. See [`AskQueue`].
    asks: AskQueue,

    /// Where this source stands on the tile-sharpness rung, stepped once per
    /// pass by [`Self::snap_for_pass`]. See [`snap`].
    snap: snap::SnapState,

    /// The ladder rung `fit` took, as the frame's budget delivered it
    /// (`TileCacheBudget::whole_zoom`) — the scene-level input to the snap.
    whole_zoom_rung: bool,

    /// What the passes drawing this source would want **unsnapped** — the
    /// cells at the level `round` picks, and their net — tallied per pass as
    /// [`Self::wanted`] is. Equal to `wanted` while the source draws sharp;
    /// while snapped it is the counterfactual the release gate prices, since
    /// the set the source would return to is not on the glass to measure.
    unsnapped: WantedTally,

    /// Tiles the IO task should fetch.
    request_tx: Sender<TileId>,
    /// Tiles the IO task has fetched: decoded on native, compressed bytes on
    /// wasm32, where this channel doubles as the decode queue the frame pump
    /// drains under [`WASM_TILE_DECODES_PER_PUMP`] — or the word that a
    /// request gets no body; see [`Fetched`].
    tile_rx: Receiver<Fetched>,

    /// wasm32 only: the context the frame pump decodes and uploads through,
    /// and asks for the queue-draining repaint on. On native the IO thread
    /// owns the context's clone instead.
    #[cfg(target_arch = "wasm32")]
    egui_ctx: Context,
    /// wasm32 only: what is left of [`WASM_TILE_DECODES_PER_PUMP`] and of
    /// [`PUMP_TIME_BUDGET`] this pass — both bounds, both per pass.
    #[cfg(target_arch = "wasm32")]
    pump_budget: PumpBudget,

    /// wasm32 only: the style [`Tile::new`] renders a vector tile against.
    /// Empty for a raster source, which never reads it; the committed style for
    /// an archive source, where it is the whole appearance of the map. On
    /// native the IO task owns its own clone -- see [`fetch_one`], [`read_one`].
    #[cfg(target_arch = "wasm32")]
    style: Arc<Style>,

    /// wasm32 only: how this source's archive bodies decode — the archive
    /// header's word, written once by the IO task at open. `None` for a
    /// raster HTTP source, whose bodies keep going through [`Tile::new`]'s
    /// sniff. On native the IO task reads the header off the archive it holds
    /// instead — see [`read_one`].
    #[cfg(target_arch = "wasm32")]
    archive_kind: Option<Arc<OnceLock<ArchiveTileKind>>>,

    /// wasm32 only: the vector bodies on their way to the worker, and the
    /// batch already with it. See [`TileBatch`].
    #[cfg(target_arch = "wasm32")]
    batch: TileBatch,

    /// wasm32 only: where a finished batch lands. A channel and not a shared
    /// cell because the delivery closure runs outside this source's borrow —
    /// it cannot touch the cache, so it hands the reply to the next pump,
    /// which owns it.
    ///
    /// Unbounded, and it costs nothing to be: at most one batch is
    /// outstanding, so the queue holds at most one reply. A bounded channel
    /// would introduce a full-queue arm whose only correct behaviour would be
    /// to drop a reply this source is holding tiles for.
    #[cfg(target_arch = "wasm32")]
    batch_tx: UnboundedSender<Option<squallar_basemap::jobs::BasemapTiles>>,
    /// The read end of [`Self::batch_tx`].
    #[cfg(target_arch = "wasm32")]
    batch_rx: UnboundedReceiver<Option<squallar_basemap::jobs::BasemapTiles>>,

    /// wasm32 only: the key [`Self::style`] was built from, and the only part
    /// of a style that crosses to the worker. `None` for a source with no
    /// style to restyle, which therefore never offloads — see
    /// [`BasemapStyling::key`].
    #[cfg(target_arch = "wasm32")]
    style_key: Option<squallar_basemap::jobs::StyleKey>,

    /// Whether "the tile IO task is gone" has already been said for this
    /// source. See [`drain_up_to`]: the condition is permanent, so the line is
    /// not.
    io_task_gone_reported: bool,

    /// [`Self::pump`] calls since this source was built — see [`Self::pumps`].
    /// Always on, like the app's other ledgers: one `u64` add per layer per
    /// pass, against the tens of [`Tiles::at`] calls the same layer makes.
    pumps: u64,

    /// Completions the drain has taken off the channel — see [`Self::takes`].
    takes: u64,

    /// Cache puts that changed what this source can draw — one per **tile**
    /// landing in the cache, never per pending or failed marker. Per source
    /// because the basemap and the terrain are separate [`HttpsTiles`], each
    /// with its own arrivals; see [`Self::put_generation`], which the
    /// floor-strip content key reads.
    put_generation: u64,

    /// Declared last so it drops last: the channels above must close first, which
    /// is what tells the fetch loop to exit.
    #[expect(dead_code, reason = "owned for its Drop; shuts the IO task down")]
    runtime: runtime::Runtime,
}

/// One source's per-pass pump allowance, wasm32 only: the decodes it may still
/// spend, and the wall-clock deadline those decodes share.
///
/// **Both halves are per pass, and the time half is the one that was not.**
/// [`HttpsTiles::pump`] runs once per layer, and one source is drawn as a layer
/// in every pane that shows it plus the volume floor strip, so a per-*call*
/// bound lets a multi-pane layout bill one pass several times over. The count
/// half has been per-pass since it was written. The time half was computed as
/// `Instant::now() + PUMP_TIME_BUDGET` inside each `drain_completed_fetches`
/// call, so every pump of a pass opened a *fresh* budget and re-armed the
/// unconditional first take with it: a frame's tile cost was
/// `panes x (PUMP_TIME_BUDGET + one take)` per source, not `PUMP_TIME_BUDGET +
/// one take`. With six panes and a take that is a multi-millisecond
/// tessellation that is a twelvefold miss, and the constant's own note — "a
/// frame's worst case is twice this plus two takes" — held only for the
/// single-pane layout it was measured on.
///
/// The pass number is what turns both halves per-pass: the first call of a new
/// pass restores the count and stamps one deadline that every later call in
/// that pass shares. Per *source* because each [`HttpsTiles`] owns one, and the
/// caches they fill are per source too.
///
/// **Fairness needs no arbitration**, because a source is app-level and its
/// cache is shared by every pane that draws it: a take that lands for pane one
/// is resident for panes two through six in the same pass. Bounding the pass
/// rather than the call spends the budget on the *first* asker and serves all
/// of them, which is why this cannot starve a later pane.
///
/// Compiled on native for the tests (`any(test, target_arch = "wasm32")`), as
/// its predecessor was.
#[cfg(any(test, target_arch = "wasm32"))]
struct PumpBudget {
    /// The pass [`Self::spent`] and [`Self::deadline`] belong to.
    pass_nr: u64,
    /// Takes already performed in [`Self::pass_nr`].
    spent: usize,
    /// When [`Self::pass_nr`]'s takes stop being free. Stamped once, by the
    /// pass's first call.
    deadline: web_time::Instant,
}

/// What one pump call may still spend of its pass — see [`PumpBudget::open`].
#[cfg(any(test, target_arch = "wasm32"))]
struct PassAllowance {
    /// Takes still allowed in the pass.
    budget: usize,
    /// The deadline every take of the pass shares.
    deadline: web_time::Instant,
    /// Whether the pass has yet to take anything, and so still owes the
    /// unconditional first take. See [`PUMP_TIME_BUDGET`].
    first_take_free: bool,
}

#[cfg(any(test, target_arch = "wasm32"))]
impl PumpBudget {
    fn new() -> Self {
        Self {
            pass_nr: 0,
            spent: 0,
            // Replaced before it is read: the first `open` of pass 0 is a
            // fresh pass by `spent == 0`, and every later pass by `pass_nr`.
            deadline: web_time::Instant::now(),
        }
    }

    /// Open (or continue) `pass_nr`, returning what is left of it.
    ///
    /// A new pass restores the count and stamps the deadline; a later call in
    /// the same pass gets what is left of both. `first_take_free` is the
    /// pass's, not the call's — the anti-stall guarantee is "a frame always
    /// moves the map forward by one tile per source", and a per-call spelling
    /// of it is the per-call budget wearing the count's clothes.
    fn open(&mut self, pass_nr: u64) -> PassAllowance {
        if pass_nr != self.pass_nr {
            self.pass_nr = pass_nr;
            self.spent = 0;
        }
        if self.spent == 0 {
            // The budget starts when the spending does. A pass whose early
            // layers found an empty queue must not have burnt its deadline on
            // nothing before the layer with work reaches it — that would put
            // the stall back, one pane further along.
            self.deadline = web_time::Instant::now() + PUMP_TIME_BUDGET;
        }
        PassAllowance {
            budget: WASM_TILE_DECODES_PER_PUMP.saturating_sub(self.spent),
            deadline: self.deadline,
            first_take_free: self.spent == 0,
        }
    }

    /// Record `taken` takes performed against the allowance.
    fn record(&mut self, taken: usize) {
        self.spent += taken;
    }
}

// ── Offloading a pass's vector tiles ──────────────────────────────────────

/// Where a pass's vector tile bodies go instead of onto the frame thread.
///
/// **A seam and not a dependency.** The funnel lives in `squallar-worker`,
/// which sits *below* this crate: `squallar-worker` composes
/// `squallar-basemap`'s codec row, and a `squallar-egui -> squallar-worker`
/// edge would invert that and drag `squallar-elevation` and
/// `squallar-buildings` into the UI layer's graph for the life of the tree.
/// So the app installs an implementation here, the way
/// `squallar_worker::offload::set_worker` is itself installed from
/// `squallar-web`. Native and every test get "no offloader installed" for
/// free, which is exactly today's path.
///
/// There is no `cancel`, deliberately. A batch this source has stopped caring
/// about costs nothing to let finish: the reply is keyed back by `TileId`, a
/// superseded style generation is dropped by [`TileBatch`]'s epoch check, and
/// a dropped source's delivery finds a closed channel and discards. What
/// `cancel_job` would buy is the page-side pending slot released a few
/// hundred milliseconds sooner, against a surface every caller would have to
/// reason about.
#[cfg(any(test, target_arch = "wasm32"))]
pub trait TileOffloader {
    /// How many jobs the funnel currently owes, or `None` when it has no
    /// worker to run them on.
    ///
    /// **`None` is not "busy", it is "posting would run it HERE"** — with no
    /// sink attached `offload_job` executes the job inline on the calling
    /// thread, unbudgeted, which is strictly worse than the pump decoding it
    /// under [`PUMP_TIME_BUDGET`]. A funnel still inside its handshake window
    /// answers `None` too: a job posted then is *held* until a worker
    /// attaches, and a held tile is a tile that appears later.
    fn queued(&self) -> Option<usize>;

    /// Hand `job` over, answering whether it was accepted. `deliver` runs
    /// where the answer can be used — the page's own thread in the browser.
    /// `deliver` is `Send` because `squallar_worker::offload::offload_job`
    /// requires it on every target — the native funnel runs a job on a pool
    /// thread and hands the answer back across it. The browser never uses
    /// that freedom (page and pump are one thread), but the bound is the
    /// funnel's and this seam does not get to relax it.
    fn post(
        &self,
        job: squallar_basemap::jobs::BasemapTilesJob,
        deliver: Box<dyn FnOnce(Option<squallar_basemap::jobs::BasemapTiles>) + Send>,
    ) -> bool;
}

#[cfg(any(test, target_arch = "wasm32"))]
thread_local! {
    /// The installed offloader, or `None` — which every native build and
    /// every test has unless it says otherwise, and which means "decode on
    /// this thread, exactly as before".
    static TILE_OFFLOADER: std::cell::RefCell<Option<Box<dyn TileOffloader>>> =
        const { std::cell::RefCell::new(None) };
}

/// Install the offloader every [`HttpsTiles`] on this thread will hand its
/// vector batches to. Replaces any previous one.
#[cfg(any(test, target_arch = "wasm32"))]
pub fn set_tile_offloader(offloader: Box<dyn TileOffloader>) {
    TILE_OFFLOADER.with(|slot| *slot.borrow_mut() = Some(offloader));
}

/// Remove the installed offloader, putting every source back on the inline
/// path. The tests' undo for [`set_tile_offloader`]; nothing in the app calls
/// it.
#[cfg(any(test, target_arch = "wasm32"))]
pub fn clear_tile_offloader() {
    TILE_OFFLOADER.with(|slot| *slot.borrow_mut() = None);
}

/// Run `f` over the installed offloader.
///
/// The `RefCell` borrow is held across `f`, which is sound because nothing an
/// offloader does re-enters this module: `post` hands bytes to a message port
/// and returns, and its `deliver` sends into a channel the pump drains on a
/// later frame rather than calling back into the source.
#[cfg(any(test, target_arch = "wasm32"))]
fn with_offloader<T>(f: impl FnOnce(Option<&dyn TileOffloader>) -> T) -> T {
    TILE_OFFLOADER.with(|slot| f(slot.borrow().as_deref()))
}

/// One source's batch state: the vector bodies waiting to be handed over, and
/// the batch already with the worker.
///
/// **A struct rather than three fields on [`HttpsTiles`]**, because the rule
/// this holds is the whole of what makes the change safe — the pump either
/// offloads or does *exactly* what shipped before it — and a rule spelled
/// inline in a `cfg(target_arch = "wasm32")` function body can only ever be
/// exercised in a browser. Compiled on native for the tests, as
/// [`PumpBudget`] is.
/// Tile bodies waiting for, or riding in, one batch.
#[cfg(any(test, target_arch = "wasm32"))]
type StagedBodies = Vec<(TileId, Arc<Vec<u8>>)>;

#[cfg(any(test, target_arch = "wasm32"))]
#[derive(Default)]
struct TileBatch {
    /// Bodies taken off the channel and not yet handed over. `Arc` because
    /// the same buffer goes into the job and stays here: a tile the reply
    /// omits or refuses is decoded inline from this copy, so a refusal costs
    /// a slower tile and never a missing one.
    staging: StagedBodies,
    /// The batch with the worker, if any. At most one, which is what keeps a
    /// backlog from queueing several deep behind a radar rasterization.
    outstanding: Option<OutstandingBatch>,
}

/// The batch currently with the worker.
#[cfg(any(test, target_arch = "wasm32"))]
struct OutstandingBatch {
    /// The style generation it was posted under. A reply that arrives after a
    /// restyle is dropped rather than drawn — the rule the native arm has
    /// always had in `drain_completed_fetches` and the wasm arm did not need
    /// until now, because until now the pump styled against the frame's
    /// current style by construction.
    epoch: u64,
    /// What was asked for, with the bodies retained, so a tile the reply does
    /// not carry can be decoded here instead of refetched.
    asked: StagedBodies,
}

#[cfg(any(test, target_arch = "wasm32"))]
impl TileBatch {
    /// Hold one body for the next batch.
    fn stage(&mut self, tile_id: TileId, bytes: Arc<Vec<u8>>) {
        self.staging.push((tile_id, bytes));
    }

    /// Whether this pump should stage its vector bodies rather than decode
    /// them, given what the funnel owes.
    ///
    /// **Every `false` here is today's code path**, which is the property the
    /// whole change rests on: the offload is a win when it happens and a
    /// no-op when it does not, never a trade.
    ///
    /// `queued` is [`TileOffloader::queued`]. The comparison is against what
    /// *this source* already has outstanding, so "the funnel holds exactly my
    /// batch and nothing else" is the same answer as "the funnel is idle" —
    /// and anything else in the queue (a 160-190 ms radar rasterization, an
    /// unbounded Level II decode) means a batch posted now would appear
    /// later than a tile decoded here.
    fn should_stage(&self, queued: Option<usize>) -> bool {
        let ours = usize::from(self.outstanding.is_some());
        queued == Some(ours)
    }

    /// Whether there is a batch to post right now.
    fn ready_to_post(&self) -> bool {
        self.outstanding.is_none() && !self.staging.is_empty()
    }

    /// Take the staged bodies for a post under `epoch`, recording them as
    /// outstanding.
    fn open(&mut self, epoch: u64) -> StagedBodies {
        let asked = std::mem::take(&mut self.staging);
        self.outstanding = Some(OutstandingBatch {
            epoch,
            asked: asked.clone(),
        });
        asked
    }

    /// Retire the outstanding batch, answering what it asked for and the
    /// generation it was posted under.
    fn close(&mut self) -> Option<(u64, StagedBodies)> {
        self.outstanding
            .take()
            .map(|batch| (batch.epoch, batch.asked))
    }

    /// Take one staged body back, oldest first, for the caller to decode on
    /// this thread.
    ///
    /// **The gate can shut between a body being staged and the batch being
    /// posted** — the funnel takes a radar job, or the worker dies. Without
    /// this those bodies wait for a flight that may never happen, and their
    /// tiles never draw: the pass that follows sees `staging == false`,
    /// decodes the CHANNEL's arrivals inline, and steps straight over the
    /// list. One per pass rather than the whole list, because the whole list
    /// is up to thirteen multi-millisecond tessellations and one per pass is
    /// the rate a dense take clears the channel at anyway.
    fn take_one_staged(&mut self) -> Option<(TileId, Arc<Vec<u8>>)> {
        (!self.staging.is_empty()).then(|| self.staging.remove(0))
    }
}

/// One take decoded **on this thread**, which is what the wasm arm did for
/// every tile before the offload existed and still does for every tile the
/// dispatch gate declines.
///
/// A struct rather than eight arguments, and lifted out of the pump's closure
/// rather than rewritten, so that the fallback path is literally the code that
/// shipped: the gate's whole claim is that declining costs nothing, and that
/// claim is only as good as the two paths being one body.
#[cfg(target_arch = "wasm32")]
struct InlineDecode<'a> {
    style: &'a Style,
    archive_kind: Option<&'a Arc<OnceLock<ArchiveTileKind>>>,
    parsed: Option<&'a SharedParsedTiles>,
    egui_ctx: &'a Context,
    /// The generation the pump is styling against — current by construction
    /// on this path, because the styling happens inside this call.
    epoch: u64,
    /// The tessellator feathering the fills are flattened at, in points.
    ///
    /// Carried rather than read at draw time because a tile flattened at one
    /// feathering and drawn at another paints wrong-width roads — see
    /// [`HttpsTiles::set_feathering`], which bumps the style generation for
    /// exactly that reason, so a change to it re-asks every tile rather than
    /// leaving a mismatched flatten in the cache.
    feathering: f32,
    /// Which cache the parses this decode remembers belong to — the level
    /// [`cache_ledger::set_parsed`] is stored under.
    role: cache_ledger::CacheRole,
}

#[cfg(target_arch = "wasm32")]
impl InlineDecode<'_> {
    fn take(
        &self,
        cache: &mut TileCache,
        put_generation: &mut u64,
        tile_id: TileId,
        payload: FetchPayload,
    ) -> take_ledger::TakeKind {
        // The family this take belongs to, decided before the work rather
        // than after it, so a decode that FAILS is still charged to what it
        // attempted. Classified by the archive header's declared kind and
        // never by the bytes — the rule [`decode_archive_tile`] itself obeys.
        let kind = match (&payload, self.archive_kind) {
            (FetchPayload::Parsed(_), _) => take_ledger::TakeKind::Restyle,
            // A plain HTTP source has no header to declare anything; its body
            // goes through `Tile::new`'s sniff.
            (FetchPayload::Bytes(_), None) => take_ledger::TakeKind::Sniffed,
            (FetchPayload::Bytes(_), Some(declared)) => {
                match declared
                    .get()
                    .copied()
                    .unwrap_or(ArchiveTileKind::Undeclared)
                {
                    ArchiveTileKind::Vector => take_ledger::TakeKind::Vector,
                    ArchiveTileKind::Hillshade | ArchiveTileKind::TerrainRgb => {
                        take_ledger::TakeKind::Raster
                    }
                    ArchiveTileKind::Undeclared => take_ledger::TakeKind::Sniffed,
                }
            }
        };
        let decoded = match payload {
            // A restyle served from the parsed cache: no bytes were fetched,
            // only the styling is owed — still under this budget, because it
            // is the tessellation half.
            FetchPayload::Parsed(parse) => Ok(styled_tile(&parse, self.style, tile_id.zoom)),
            FetchPayload::Bytes(bytes) => match self.archive_kind {
                // A raster HTTP source: the body is whatever image the
                // provider serves, sniffed as it always was.
                None => Tile::new(&bytes, self.style, tile_id.zoom, self.egui_ctx)
                    .map_err(|error| error.to_string()),
                // An archive source: the header's word decides. The IO task
                // writes the slot at open, before any tile is served, so an
                // empty slot cannot be reached through the normal order of
                // events; if it ever is, sniffing is the pre-seam behaviour
                // rather than a guess of this code's.
                Some(kind) => {
                    let kind = kind.get().copied().unwrap_or(ArchiveTileKind::Undeclared);
                    match self.parsed {
                        Some(parsed) => decode_archive_tile_remembering(
                            &bytes,
                            kind,
                            self.style,
                            tile_id,
                            self.egui_ctx,
                            parsed,
                            self.role,
                        ),
                        None => decode_archive_tile(
                            &bytes,
                            kind,
                            self.style,
                            tile_id.zoom,
                            self.egui_ctx,
                        ),
                    }
                }
            },
        };
        match decoded {
            Ok(tile) => {
                *put_generation += 1;
                cache.put(tile_id, slot_for(tile, self.epoch, self.feathering));
            }
            Err(error) => {
                // The request is over with nothing to put; the marker stays
                // as "failed, do not ask again" for as long as the LRU keeps
                // it.
                cache.answered(&tile_id);
                log::warn!("decoding tile {tile_id:?}: {error}");
            }
        }
        // Counted here and nowhere else, against the same population
        // `note_tiles_offloaded` counts: one **vector body** disposed of. A
        // restyle carries no body and a raster is not vector, so neither is in
        // this denominator. See [`take_ledger::Disposition`].
        if kind == take_ledger::TakeKind::Vector {
            take_ledger::note_tiles_decoded_inline(1);
        }
        kind
    }
}

/// Move at most `budget` completed fetches out of `rx`, handing each to `take`.
/// Returns how many were taken. Stops early when the channel is empty; a closed
/// channel is the IO task gone, and is logged rather than panicked on, as
/// everywhere else in this module.
///
/// `take` answers the family the item was, or `None` for an item that was no
/// take at all — the IO side's word that a request gets no body, see
/// [`Fetched`] — which is moved and counted against `budget` but never priced:
/// a zero-cost sample in a decode family's histogram would read as a fast
/// decode. A bare [`take_ledger::TakeKind`] is accepted as always-`Some`, so a
/// caller that never sees an empty answer need not say so.
///
/// Both arms of [`HttpsTiles::drain_completed_fetches`] are this loop over a
/// different `take` and a different budget — which is the whole platform
/// difference behind [`HttpsTiles::pump`].
///
/// `reported` is the source's own latch and the reason it exists is measured,
/// not defensive. The IO task ends for good when the archive will not open —
/// [`serve_archive_continuously`] logs why and returns — and after that the
/// channel is closed on every subsequent drain. [`HttpsTiles::pump`] runs once
/// per layer per frame, so the unlatched version emitted this line at frame
/// rate: **120 `console.error`s in one 40 s Firefox run against a host that
/// answers `200` to a range request**, measured 2026-08-28. The condition is
/// permanent and the user can do nothing about it, so it is said once.
fn drain_up_to<T, K>(
    rx: &mut Receiver<T>,
    budget: usize,
    deadline: web_time::Instant,
    first_take_free: bool,
    reported: &mut bool,
    mut take: impl FnMut(T) -> K,
) -> usize
where
    K: Into<Option<take_ledger::TakeKind>>,
{
    let mut taken = 0;
    while taken < budget {
        // The first take of the **pass** is unconditional, so a frame already
        // over budget still moves the map forward by one tile per source
        // rather than stalling it. `first_take_free` is the pass's answer and
        // not this call's: spelling it `taken > 0` re-armed the exemption once
        // per layer, which is how a six-pane frame paid six of them. See
        // [`PumpBudget`] and [`PUMP_TIME_BUDGET`].
        if !(first_take_free && taken == 0) && web_time::Instant::now() >= deadline {
            break;
        }
        match rx.try_recv() {
            Ok(item) => {
                // The take, bracketed. This span is exactly what
                // [`PUMP_TIME_BUDGET`] cannot see inside — the deadline above
                // is checked between takes and never during one, so a frame
                // pays the budget plus one whole unbounded pass through here.
                // See [`take_ledger`] for the denominator and the families.
                let began = web_time::Instant::now();
                let kind: Option<take_ledger::TakeKind> = take(item).into();
                let cost = web_time::Instant::now().duration_since(began);
                if let Some(kind) = kind {
                    take_ledger::note_take(kind, cost.as_micros().min(u128::from(u32::MAX)) as u32);
                }
                taken += 1;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Closed) => {
                if !*reported {
                    *reported = true;
                    log::error!("the tile IO task is gone; this source will serve no more tiles");
                }
                break;
            }
        }
    }
    taken
}

impl HttpsTiles {
    /// Fetch `source`'s tiles through squallar's platform-verified HTTPS client.
    pub fn new<S: AsyncTileSource>(source: S, egui_ctx: Context) -> Self {
        Self::with_client(source, egui_ctx, tile_client())
    }

    /// [`Self::new`], with the HTTP client supplied. Crate-private, for the
    /// tests, which need to talk cleartext to a loopback server — [`tile_client`]
    /// refuses `http://` by design.
    pub(crate) fn with_client<S: AsyncTileSource>(
        source: S,
        egui_ctx: Context,
        client: reqwest::Client,
    ) -> Self {
        Self::with_client_and_budget(source, egui_ctx, client, default_tile_budget().styled_bytes)
    }

    /// [`Self::with_client`], with the styled cache's byte budget supplied.
    /// Crate-private, for the tests, which size a cache to a working set
    /// rather than to a device.
    fn with_client_and_budget<S: AsyncTileSource>(
        source: S,
        egui_ctx: Context,
        client: reqwest::Client,
        styled_bytes: u64,
    ) -> Self {
        let attribution = source.attribution();
        let tile_size = source.tile_size();
        let max_zoom = Arc::new(AtomicU8::new(source.max_zoom()));

        // Sized to the concurrency limit, as walkers sizes them: a full request
        // channel is the backpressure that makes `request_once` retry later.
        let (request_tx, request_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let (tile_tx, tile_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        // Where a finished batch lands. See `HttpsTiles::batch_tx`.
        #[cfg(target_arch = "wasm32")]
        let (batch_tx, batch_rx) = unbounded();

        // The frame pump needs the context too on wasm: it is the decoding
        // side there. See `FetchPayload`.
        #[cfg(target_arch = "wasm32")]
        let frame_ctx = egui_ctx.clone();

        let runtime = runtime::spawn(fetch_continuously(
            source, client, request_rx, tile_tx, egui_ctx,
        ));

        Self {
            attribution,
            tile_size,
            max_zoom,
            // A raster source has no single failure that ends it: each tile is
            // its own request, and a 404 on one says nothing about the next.
            // The run of failed reads is the archive loop's own count, and
            // `fetch_continuously` does not keep one, so this stays down for a
            // raster source rather than reading half of a rule.
            fault: Arc::new(OnceLock::new()),
            reads_failing: Arc::new(AtomicBool::new(false)),
            requests_closed: false,
            // A plain HTTP raster source draws the ground: the base role.
            cache: TileCache::new(styled_bytes, cache_ledger::CacheRole::Base),
            style_epoch: RASTER_STYLE_EPOCH,
            feathering: None,
            // A raster source has no style to swap and no parse to keep.
            #[cfg(not(target_arch = "wasm32"))]
            style_slot: None,
            parsed: None,
            bodies: None,
            wanted: WantedTally::default(),
            asks: AskQueue::default(),
            snap: snap::SnapState::default(),
            whole_zoom_rung: false,
            unsnapped: WantedTally::default(),
            request_tx,
            tile_rx,
            #[cfg(target_arch = "wasm32")]
            egui_ctx: frame_ctx,
            #[cfg(target_arch = "wasm32")]
            pump_budget: PumpBudget::new(),
            // A raster source. The wasm decode path passes this to `Tile::new`,
            // which only reads a style for a tile that is not an image.
            #[cfg(target_arch = "wasm32")]
            style: Arc::new(Style::default()),
            // Not an archive: no header to obey, so the sniff stands.
            #[cfg(target_arch = "wasm32")]
            archive_kind: None,
            #[cfg(target_arch = "wasm32")]
            batch: TileBatch::default(),
            #[cfg(target_arch = "wasm32")]
            batch_tx,
            #[cfg(target_arch = "wasm32")]
            batch_rx,
            #[cfg(target_arch = "wasm32")]
            style_key: None,
            io_task_gone_reported: false,
            put_generation: 0,
            pumps: 0,
            takes: 0,
            runtime,
        }
    }

    /// Adopt `style` for every tile this source serves from now on — the
    /// restyle seam a theme flip and a map-detail toggle ride
    /// (`MapTileState::ensure_base_tiles`), replacing the source rebuild that
    /// refetched every visible tile.
    ///
    /// **Nothing is cleared and nothing is blanked.** The generation
    /// ([`CachedTile`]) is bumped instead: every cached tile keeps drawing in
    /// its old style while [`Self::request_once`] re-asks for it, the IO side
    /// answers out of the parsed cache with **zero fetches and zero
    /// re-parses**, and arrivals replace the stale slots one by one. The heavy
    /// half — styling is mostly lyon tessellation — runs where a decode runs:
    /// the IO runtime's blocking pool on native, the frame pump's
    /// [`WASM_TILE_DECODES_PER_PUMP`] budget on wasm32. The frame thread pays
    /// for nothing here but the lock write.
    ///
    /// Meaningful only for an archive source; a raster source has no style
    /// slot and ignores the swap beyond the (harmless) generation bump.
    pub(crate) fn set_style(&mut self, styling: BasemapStyling) {
        self.style_epoch += 1;
        self.install_style(styling);
    }

    /// Adopt `feathering` — the tessellator's, in points — for every tile
    /// this source flattens from now on.
    ///
    /// **A flatten input, not a paint parameter.** Stroke offsets are
    /// pre-computed in extent units and a feathering sets both radii, the end
    /// extrude and, at hairline widths, which topology branch epaint takes. It
    /// changes when `pixels_per_point` does — the window crossing to a
    /// different-DPI display, or the user moving the UI scale — and the tiles
    /// already flattened are then wrong for the frame.
    ///
    /// So this rides the **same seam a restyle rides**: the generation is
    /// bumped, every cached tile keeps drawing while
    /// [`Self::request_once`] re-asks for it, and the IO side answers out of
    /// the parsed cache with zero fetches and zero re-parses. Until the
    /// re-flattened tile lands the ground phase draws that tile's strokes on
    /// the CPU, which is where they were before any of this.
    ///
    /// The **first** call installs without bumping: it happens before this
    /// source has been asked for a tile, so there is nothing to re-ask for.
    pub(crate) fn set_feathering(&mut self, feathering: f32) {
        if self.feathering == Some(feathering) {
            return;
        }
        let first = self.feathering.is_none();
        self.feathering = Some(feathering);
        if !first {
            self.style_epoch += 1;
        }
        self.publish_feathering();
    }

    /// The native arm of [`Self::set_feathering`]: republish the pair the IO
    /// task reads, keeping the style it already has.
    #[cfg(not(target_arch = "wasm32"))]
    fn publish_feathering(&mut self) {
        if let Some(slot) = &self.style_slot {
            let mut slot = slot.write().expect("the style slot is not poisoned");
            slot.epoch = self.style_epoch;
            slot.feathering = flatten_feathering(self.feathering);
        }
    }

    /// The wasm32 arm: the frame pump is the flattening side and reads the
    /// field directly.
    #[cfg(target_arch = "wasm32")]
    fn publish_feathering(&mut self) {}

    /// The native arm of the [`Self::set_style`] split: publish the pair to
    /// the IO task's slot.
    #[cfg(not(target_arch = "wasm32"))]
    fn install_style(&mut self, styling: BasemapStyling) {
        if let Some(slot) = &self.style_slot {
            *slot.write().expect("the style slot is not poisoned") = StyleSlot {
                style: styling.style,
                epoch: self.style_epoch,
                feathering: flatten_feathering(self.feathering),
            };
        }
    }

    /// The wasm32 arm: the frame pump is the styling side, so the live style
    /// is this field.
    #[cfg(target_arch = "wasm32")]
    fn install_style(&mut self, styling: BasemapStyling) {
        self.style = styling.style;
        // The key moves with the style, so a batch posted after this is
        // styled by the worker exactly as the pump would style it here.
        self.style_key = styling.key;
    }

    /// Move what the IO task has finished into the cache.
    ///
    /// Call this **once per layer, before the layer's tile loop**;
    /// `ui_map_overlays::draw_tile_layer` is the only caller. It is deliberately
    /// not called from [`Tiles::at`], which runs once per grid cell: over a
    /// 1920x1080-**point** canvas at a whole zoom and bias 0 that billed one
    /// drain per cell — 45 measured at zoom 6 over Oklahoma, and up to 54 as the
    /// grid's phase moves — for the one that had anything to move. On wasm32
    /// each of them opened by reading `Context::cumulative_pass_nr`, two
    /// `RwLock` acquisitions over the whole egui `Context`, only to learn the
    /// pass had not changed.
    ///
    /// The platform difference lives in [`Self::drain_completed_fetches`], not
    /// at the call site: on native the IO thread has already decoded and
    /// uploaded, so a take is a cache put; on wasm32 the IO task shares the page
    /// thread, so the PNG decode and the texture upload happen here, under
    /// [`WASM_TILE_DECODES_PER_PUMP`].
    ///
    /// **The one thing this shape forbids:** handing an [`HttpsTiles`] to
    /// `walkers::Map` as `dyn Tiles`. walkers' `draw_tiles`/`flood_fill_tiles`
    /// call `at` and know nothing about this method, so tiles would be requested
    /// and never arrive. Nothing does today — both `Map::new` sites in `ui_map`
    /// pass `None` for tiles and the app draws its own tile layers — and nothing
    /// plans to.
    /// **The pump does not know whether a gesture is running, and must not.**
    /// WO-9e gated this on a gesture latch that read
    /// [`crate::overlay_cache::SETTLE_REPAINT_DELAY`]: the drain took nothing
    /// while map input was live and resumed only 500 ms after it stopped. That
    /// bought a frame budget with half a second of a map that does not change
    /// — the black screen a zoom-out settles into, and the stretched ancestor a
    /// zoom-in sits on, both waited on that clock before a single tile could be
    /// put. What the latch was really dodging is the per-take cost, and
    /// [`PUMP_TIME_BUDGET`] bounds that directly, so the drain now runs on
    /// every frame whether the map is moving or not. Filling the map while the
    /// user moves *is* minimizing data latency.
    pub fn pump(&mut self) {
        self.pumps += 1;
        self.trim_economy();
        self.drain_completed_fetches();
    }

    /// Pay one entry of shrink debt on each of this source's caches, if any
    /// is over its budget — the lazy half of [`Self::set_budget`]. One entry
    /// per cache per pump, so a budget that fell by a hundred tiles costs no
    /// frame more than three evictions, each handed to the discard sink; a
    /// resize or a pane split never stalls input on a drop.
    fn trim_economy(&mut self) {
        self.cache.trim_one();
        if let Some(parsed) = &self.parsed {
            let mut parsed = parsed
                .lock()
                .expect("the parsed-tile cache is not poisoned");
            if let Some(gone) = parsed.trim_one() {
                cache_ledger::set_parsed(
                    self.cache.role,
                    parsed.len() as u64,
                    parsed.resident_bytes(),
                );
                drop(parsed);
                discard_slot("tile-parsed-trim", gone.value);
            }
        }
        if let Some(bodies) = &self.bodies {
            let gone = bodies
                .lock()
                .expect("the tile-body cache is not poisoned")
                .trim_one();
            if let Some(gone) = gone {
                discard_slot("tile-body-trim", gone.value);
            }
        }
    }

    /// Allow this source `styled_bytes` for its styled entries and
    /// `parsed_bytes` for its parsed geometry (and, on wasm32, the bodies
    /// that stand in for it) — the device's allowance, pushed in every frame
    /// by `MapTileState::set_budget`. A rise takes effect at once; a fall is
    /// paid down by [`Self::pump`] one entry at a time.
    pub(crate) fn set_budget(&mut self, styled_bytes: u64, parsed_bytes: u64) {
        self.cache.set_budget(styled_bytes);
        if let Some(parsed) = &self.parsed {
            parsed
                .lock()
                .expect("the parsed-tile cache is not poisoned")
                .set_budget(parsed_bytes);
        }
        if let Some(bodies) = &self.bodies {
            bodies
                .lock()
                .expect("the tile-body cache is not poisoned")
                .set_budget(parsed_bytes);
        }
    }

    /// **The working set, as the pass drawing this source measured it** —
    /// `on_glass` cells at the drawn level and `net` cells of the ancestor
    /// net — accumulated across every pane that draws this source in pass
    /// `pass_nr`. Called once per layer draw by
    /// `ui_map_overlays::draw_tile_layer`, after the net is asked for and
    /// before the grid is walked.
    ///
    /// Two things happen here, because this is the one per-pass hook a source
    /// has that knows where in the pass it is. **The floor**: the cache is
    /// told to hold the larger of the last completed pass's total and this
    /// pass's running total, plus [`MAX_PARALLEL_DOWNLOADS`] for the markers
    /// of requests in flight for it — so a window that grows is held from
    /// the pass that grows it, and one that shrinks lets go a pass later.
    /// **The refused asks**: the cells the request channel refused last pass
    /// are asked for now, in the order they were refused, before this pass's
    /// walk reaches its own head — the fix for a tail that was never asked.
    /// See [`AskQueue`] for why the order and not the depth is what moves.
    ///
    /// The floor counts entries and the budget counts bytes: while the
    /// working set costs more than the budget the difference is the
    /// cache's overrun, reported as a level and never hidden by evicting a
    /// tile that is on the glass.
    pub(crate) fn note_wanted(&mut self, pass_nr: u64, on_glass: usize, net: usize) {
        if self.wanted.note(pass_nr, on_glass, net) {
            self.asks.new_pass();
        }
        let floor = self.wanted.floor().saturating_add(MAX_PARALLEL_DOWNLOADS);
        self.cache.set_floor_entries(floor);
        let (completed_on_glass, completed_net) = self.wanted.completed();
        cache_ledger::set_wanted(
            self.cache.role,
            completed_on_glass as u64,
            completed_net as u64,
        );
        self.retry_refused_asks();
    }

    /// This source stopped drawing — the layer was switched off and the
    /// source parked. Nothing is on the glass, so the floor is zero and every
    /// slot is economy the budget may reclaim; the pass tally and the refused
    /// asks start over when it draws again.
    pub(crate) fn release_working_set(&mut self) {
        self.wanted = WantedTally::default();
        self.unsnapped = WantedTally::default();
        self.asks.clear();
        self.cache.set_floor_entries(0);
        cache_ledger::set_wanted(self.cache.role, 0, 0);
    }

    /// The mean charge of a resident slot, floored at
    /// [`byte_lru::MARKER_BYTES`] — what a consumer projecting a working
    /// set's cost multiplies by (`ui.rs`'s zoom-bias gate).
    pub(crate) fn mean_entry_bytes(&self) -> u64 {
        self.cache.mean_entry_bytes()
    }

    /// Tell this source where the ladder's tile-sharpness rung stands —
    /// `Budgets::tile_whole_zoom`, pushed with the allowances by
    /// `MapTileState::set_budget`. One of the two inputs to
    /// [`Self::snap_for_pass`]; it moves nothing until the dwell has counted.
    pub(crate) fn set_whole_zoom_rung(&mut self, rung: bool) {
        self.whole_zoom_rung = rung;
    }

    /// **Whether this source draws at the whole zoom this pass** — the
    /// tile-sharpness rung, decided once per pass ([`snap::snap_decision`]
    /// steps nothing for a pass it has seen) from the levels the last pass
    /// left: the rung as delivered, the styled cache's working-set overrun,
    /// and the set the source would draw unsnapped priced at the cache's mean
    /// entry. Called by `ui_map_overlays::draw_tile_layer` before the level
    /// is chosen, so every pane drawing the source in a pass draws it the
    /// same way. A flip is published to the ledger as the `snap` level.
    pub(crate) fn snap_for_pass(&mut self, pass_nr: u64) -> bool {
        let (on_glass, net) = self.unsnapped.completed();
        let reading = snap::SnapReading {
            whole_zoom_rung: self.whole_zoom_rung,
            working_set_overrun_bytes: self.cache.floor_overrun_bytes(),
            unsnapped_bytes: (on_glass.saturating_add(net) as u64)
                .saturating_mul(self.cache.mean_entry_bytes()),
            budget_bytes: self.cache.budget(),
        };
        let next = snap::snap_decision(self.snap, reading, pass_nr);
        if next.snapped() != self.snap.snapped() {
            cache_ledger::set_snapped(self.cache.role, next.snapped());
        }
        self.snap = next;
        next.snapped()
    }

    /// What the pass drawing this source **would** have wanted unsnapped —
    /// `on_glass` cells at the level `round` picks and `net` of its ancestor
    /// net — accumulated across panes in `pass_nr` as [`Self::note_wanted`]
    /// accumulates what was drawn. Priced, never asked for: the release gate
    /// of [`Self::snap_for_pass`] reads the last whole pass's total.
    pub(crate) fn note_unsnapped(&mut self, pass_nr: u64, on_glass: usize, net: usize) {
        self.unsnapped.note(pass_nr, on_glass, net);
    }

    /// Whether the tile-sharpness rung holds this source at the whole zoom.
    pub fn snapped(&self) -> bool {
        self.snap.snapped()
    }

    /// The last whole pass's `(on_glass, net)` this source drew. See
    /// [`Self::note_wanted`]; `cfg(test)` alone for [`Self::tile_is_cached`]'s
    /// reason.
    #[cfg(test)]
    pub(crate) fn wanted_for_test(&self) -> (usize, usize) {
        self.wanted.completed()
    }

    /// The last whole pass's `(on_glass, net)` this source would have drawn
    /// unsnapped. See [`Self::note_unsnapped`].
    #[cfg(test)]
    pub(crate) fn unsnapped_for_test(&self) -> (usize, usize) {
        self.unsnapped.completed()
    }

    /// Whether a pass asked for `tile_id`: a marker was put, or the channel
    /// refused it and it is queued to be asked first next pass. The property
    /// a draw pass leaves behind whatever the IO task has drained, where
    /// [`Self::tile_is_cached`] alone depends on the channel having had room.
    #[cfg(test)]
    pub(crate) fn asked_or_queued_for_test(&self, tile_id: TileId) -> bool {
        self.cache.contains(&tile_id) || self.asks.queued.contains(&tile_id)
    }

    /// Ask for the cells the channel refused, oldest first, until it refuses
    /// again. A refused cell goes back to the front so the order holds; a cell
    /// that has since arrived or been asked for by another route is simply
    /// dropped. Runs at the pass boundary, after the net and before the walk.
    fn retry_refused_asks(&mut self) {
        let mut budget = self.asks.len();
        while budget > 0 {
            budget -= 1;
            let Some(tile_id) = self.asks.pop_front() else {
                break;
            };
            if self.try_request(tile_id) == Ask::Refused {
                self.asks.push_front(tile_id);
                break;
            }
        }
    }

    /// [`Self::pump`] calls since this source was built.
    ///
    /// An always-on ledger, for the same reason the overlay and upload ledgers
    /// are: the cost this method's granularity controls is invisible from the
    /// outside otherwise, and `ui_map_overlays`' tests read it to hold the drain
    /// at one per layer rather than one per tile.
    pub fn pumps(&self) -> u64 {
        self.pumps
    }

    /// Completions the drain has taken off the channel since this source was
    /// built — an always-on ledger like [`Self::pumps`]. Each take is the
    /// per-tile cost the gesture latch defers: decode + upload on wasm, the
    /// cache put and its repaint-the-floor generation bump on native.
    pub fn takes(&self) -> u64 {
        self.takes
    }

    /// How many tiles have landed in this source's cache since it was built —
    /// the "did the ground change" input of the floor-strip content key.
    ///
    /// Moves on every put that carries a **tile**: an arrival on either
    /// target's drain, and the seam tests' direct puts. It deliberately does
    /// not move for a pending or failed marker ([`Self::request_once`]'s
    /// `tile: None` slots) or a restyle re-stamp — neither changes any pixel
    /// this source can currently serve, and a key input that moved on an ask
    /// would repaint every floor strip once per requested tile.
    pub(crate) fn put_generation(&self) -> u64 {
        self.put_generation
    }

    /// Native: move at most [`NATIVE_TILE_UPLOADS_PER_PUMP`] already-decoded
    /// tiles into the cache. [`fetch_one`] did the decode and the upload on the
    /// IO thread, so this is a queue move and nothing more.
    #[cfg(not(target_arch = "wasm32"))]
    fn drain_completed_fetches(&mut self) {
        // Split borrow: the closure needs the cache while the receiver is
        // borrowed.
        let Self {
            cache,
            tile_rx,
            io_task_gone_reported,
            style_epoch,
            put_generation,
            takes,
            ..
        } = self;
        let current = *style_epoch;
        let taken = drain_up_to(
            tile_rx,
            NATIVE_TILE_UPLOADS_PER_PUMP,
            web_time::Instant::now() + PUMP_TIME_BUDGET,
            // Per call, and deliberately still per call: the pass-wide budget
            // is [`PumpBudget`], and reaching it here would mean keeping a
            // frame-side `Context` clone this arm does not otherwise have —
            // the IO thread owns the only one — to read `cumulative_pass_nr`
            // under two `RwLock`s once per layer. A native take is an
            // `LruCache::put` costing microseconds, so `PUMP_TIME_BUDGET` is
            // never reached on this arm at all and there is nothing here for
            // a pass-wide spelling of it to bound. See [`PUMP_TIME_BUDGET`].
            true,
            io_task_gone_reported,
            |(tile_id, answer): Fetched| {
                let Some(slot) = answer else {
                    // The IO side's word that no body is coming: the request
                    // is over, and nothing moved but the word. See
                    // [`Fetched`] for why it is no family's take.
                    cache.answered(&tile_id);
                    return None;
                };
                // A tile styled under a generation a restyle has replaced is
                // dropped, not drawn: its slot keeps showing the old style and
                // [`Self::request_once`] re-asks under the current one, which
                // the IO side answers from the parsed cache. Dropped is still
                // answered: the request is over either way.
                if slot.epoch == current {
                    *put_generation += 1;
                    cache.put(tile_id, slot);
                } else {
                    cache.answered(&tile_id);
                }
                // The whole native take, whether or not the slot survived its
                // epoch check: a dropped restyle still cost the frame the
                // move off the channel. See [`take_ledger::TakeKind::Put`] —
                // on this arm the decode already happened on the IO thread,
                // which is why one family covers the arm.
                Some(take_ledger::TakeKind::Put)
            },
        );
        *takes += taken as u64;
    }

    /// wasm32: hand this pass's vector bodies to the worker, or decode them
    /// here exactly as this arm always has.
    ///
    /// [`PumpBudget`] keeps a burst of completed fetches from billing one pass
    /// for this source's whole backlog. The put is unconditional, exactly as a
    /// native tile arriving is: a `None` marker still means failed, do not ask
    /// again.
    #[cfg(target_arch = "wasm32")]
    fn drain_completed_fetches(&mut self) {
        // Anything the worker has already answered goes in FIRST, so a reply
        // that landed between frames is drawn this frame rather than next.
        self.install_batch_reply();

        // **One decision per pass, taken before the drain and never per
        // take.** A pass either stages its vector bodies or decodes them, and
        // deciding per take would put two populations — a pointer move and a
        // multi-millisecond tessellation — into one `take:vector` reading,
        // whose whole purpose is to say what a vector take costs this thread.
        //
        // Every `false` below lands on the code this arm shipped before the
        // offload existed. That is the property the change rests on: a win
        // when the funnel is free, a no-op when it is not, and never a trade.
        let queued = with_offloader(|offloader| offloader.and_then(TileOffloader::queued));
        let staging =
            self.archive_is_vector() && self.style_key.is_some() && self.batch.should_stage(queued);

        let Self {
            cache,
            tile_rx,
            egui_ctx,
            pump_budget,
            style,
            archive_kind,
            parsed,
            io_task_gone_reported,
            style_epoch,
            feathering,
            put_generation,
            takes,
            batch,
            ..
        } = self;
        let inline = InlineDecode {
            style,
            archive_kind: archive_kind.as_ref(),
            parsed: parsed.as_ref(),
            egui_ctx,
            // The pump styles against the frame's current style, so what it
            // puts is current by construction.
            epoch: *style_epoch,
            // And flattens at the frame's current feathering: a tile flattened
            // at one and drawn at another paints wrong-width roads. See
            // `HttpsTiles::set_feathering`, which bumps the style generation
            // for that reason, so a change re-asks rather than leaving a
            // mismatched flatten in the cache.
            feathering: flatten_feathering(*feathering),
            role: cache.role,
        };

        let allowance = pump_budget.open(egui_ctx.cumulative_pass_nr());
        let budget = allowance.budget;
        // A body staged for a flight that will not happen — see
        // [`TileBatch::take_one_staged`]. Taken before the channel so the
        // oldest body is served first, and charged to this pass's allowance
        // like any other take.
        let mut reclaimed = 0usize;
        if !staging
            && budget > 0
            && let Some((tile_id, bytes)) = batch.take_one_staged()
        {
            inline.take(cache, put_generation, tile_id, FetchPayload::Bytes(bytes));
            reclaimed = 1;
        }
        let taken = reclaimed
            + drain_up_to(
                tile_rx,
                budget - reclaimed,
                allowance.deadline,
                // The pass's unconditional first take is spent if the reclaim
                // above took it.
                allowance.first_take_free && reclaimed == 0,
                io_task_gone_reported,
                |(tile_id, answer): Fetched| match (answer, staging) {
                    // The IO side's word that no body is coming: the request
                    // is over, there is nothing to decode, and — see
                    // [`Fetched`] — it is no family's take.
                    (None, _) => {
                        cache.answered(&tile_id);
                        None
                    }
                    // The offload path's take, and the whole of it: a pointer
                    // move onto the staging list. Charged to the same family as
                    // the decode it replaces, because it IS this pass's vector
                    // take — which is what makes `take:vector` going to ~0 the
                    // direct expression of the defect rather than a family that
                    // quietly stopped being recorded. The request stays open:
                    // the body is on its way to the worker, and its reply is
                    // what closes it, in `install_batch_reply`.
                    (Some(FetchPayload::Bytes(bytes)), true) => {
                        batch.stage(tile_id, bytes);
                        Some(take_ledger::TakeKind::Vector)
                    }
                    // Everything else, unchanged: a restyle from the parsed
                    // cache, a raster body, a sniffed body, and every vector body
                    // on a pass that is not offloading.
                    (Some(payload), _) => {
                        Some(inline.take(cache, put_generation, tile_id, payload))
                    }
                },
            );
        pump_budget.record(taken);
        *takes += taken as u64;

        if budget > 0 && taken == budget {
            // The whole allowance went, so more completions may be waiting.
            // Ask for a frame so a backlog drains while the user is idle.
            egui_ctx.request_repaint();
        }

        // The borrow above ends here, which is why the post is its own step.
        self.post_staged_batch();
    }

    /// Whether this source's archive serves vector bodies — the header's
    /// word, read once by the IO task at open. Anything else (a hillshade, a
    /// sniffed body, a source with no archive at all) keeps the inline path:
    /// a raster take needs `Context::load_texture`, which is the frame
    /// thread's to call.
    #[cfg(target_arch = "wasm32")]
    fn archive_is_vector(&self) -> bool {
        self.archive_kind
            .as_ref()
            .and_then(|kind| kind.get())
            .is_some_and(|kind| *kind == ArchiveTileKind::Vector)
    }

    /// Hand the staged bodies to the worker, if there are any and nothing of
    /// this source's is already out there.
    #[cfg(target_arch = "wasm32")]
    fn post_staged_batch(&mut self) {
        if !self.batch.ready_to_post() {
            return;
        }
        let Some(style_key) = self.style_key.clone() else {
            // Unreachable: `drain_completed_fetches` will not stage without a
            // key, so nothing is ever waiting here without one. Stated rather
            // than left to a `expect`, because the failure it would guard
            // against is a source that stages forever and draws nothing.
            log::error!("a basemap batch was staged with no style key; decoding here instead");
            return;
        };
        let epoch = self.style_epoch;
        let tiles = self.batch.open(epoch);
        let job = squallar_basemap::jobs::BasemapTilesJob {
            style: style_key,
            tiles: tiles
                .iter()
                .map(|(tile_id, mvt)| squallar_basemap::jobs::TileBody {
                    z: tile_id.zoom,
                    x: tile_id.x,
                    y: tile_id.y,
                    mvt: Arc::clone(mvt),
                })
                .collect(),
        };
        let count = job.tiles.len();

        let reply_tx = self.batch_tx.clone();
        let ctx = self.egui_ctx.clone();
        let posted = with_offloader(|offloader| {
            offloader.is_some_and(|offloader| {
                offloader.post(
                    job,
                    Box::new(move |tiles| {
                        // Nothing is installed here: the source owns the
                        // cache and this runs outside its borrow. The pump
                        // drains this on its next pass, which the repaint
                        // asks for.
                        let _ = reply_tx.unbounded_send(tiles);
                        ctx.request_repaint();
                    }),
                )
            })
        });

        if posted {
            take_ledger::note_tiles_offloaded(count as u64);
            return;
        }
        // Refused between the gate and the post — a worker lost in that
        // window. Put the bodies back; the next pump finds `queued() == None`
        // and decodes them here.
        if let Some((_, asked)) = self.batch.close() {
            self.batch.staging.extend(asked);
        }
    }

    /// Install whatever the worker has answered, and decode here anything it
    /// did not answer for.
    ///
    /// **A reply from a superseded style generation is dropped, not drawn**,
    /// which is the rule the native arm has always had and this arm did not
    /// need until a batch could outlive the style it was posted under. The
    /// slot keeps showing the old styling while `request_once` re-asks under
    /// the current one — a stale-styled tile beats a blank one.
    #[cfg(target_arch = "wasm32")]
    fn install_batch_reply(&mut self) {
        // `try_recv` answers `Result<T, _>` where `T` is itself the reply's
        // `Option` — so `Ok(None)` is **a batch that answered nothing**, not an
        // empty channel, and it still owes `close()`. Spelled as a match and
        // not as a `let ... else` on `Ok(Some(..))`, which would drop that
        // arm on the floor and leave the batch outstanding forever, staging
        // every later body behind a flight that already landed.
        let reply = match self.batch_rx.try_recv() {
            Ok(reply) => reply,
            // Empty this pass, or the sender is gone with the source.
            Err(_) => return,
        };
        let Some((epoch, asked)) = self.batch.close() else {
            // A reply with no outstanding batch: the source was reset under
            // it. Nothing to install and nothing owed.
            return;
        };

        // What the reply actually carried, in the order it carried it.
        let mut styled: Vec<(TileId, Vec<walkers::ShapeOrText>)> = Vec::new();
        if let Some(tiles) = reply {
            for tile in tiles.tiles {
                let tile_id = TileId {
                    zoom: tile.z,
                    x: tile.x,
                    y: tile.y,
                };
                if let Some(shapes) = tile.shapes {
                    styled.push((tile_id, shapes));
                }
            }
        }

        if epoch == self.style_epoch {
            for (tile_id, shapes) in &styled {
                // Counted where the tile lands, for the reason
                // `note_archive_decode` records: the worker parsed this body,
                // but the worker's counters are not the ones anything reads.
                note_archive_decode(ArchiveTileKind::Vector);
                self.put_generation += 1;
                self.cache.put(
                    *tile_id,
                    slot_for(
                        Tile::Vector(Arc::new(shapes.clone())),
                        epoch,
                        flatten_feathering(self.feathering),
                    ),
                );
            }
        } else {
            log::debug!(
                "a batch of {} basemap tiles came back styled under generation \
                 {epoch}, which generation {} has replaced; re-asking",
                styled.len(),
                self.style_epoch,
            );
            // Dropped is still answered: each request is over, which is what
            // lets the re-ask go out.
            for (tile_id, _) in &styled {
                self.cache.answered(tile_id);
            }
        }

        // Anything asked for and not answered — a body that would not parse,
        // a worker that died, a reply this build could not read — goes back
        // to the staging list rather than being refetched. The next pump
        // decodes it here or offloads it again, and either way the tile
        // arrives.
        let answered: Vec<TileId> = styled.iter().map(|(tile_id, _)| *tile_id).collect();
        for (tile_id, bytes) in asked {
            if !answered.contains(&tile_id) {
                self.batch.stage(tile_id, bytes);
            }
        }
    }

    /// Ask for `tile_id` unless a request for it is out, or it has already
    /// been asked for **under the current style generation**.
    ///
    /// Two records say "do not ask". A slot under the tile's id stamped with
    /// the current generation records "it is here" (or "it failed", or "a
    /// request is out") and reserves the slot. The id's entry in
    /// [`TileCache::in_flight`] records "a request is out" and nothing else.
    /// The slot used to be the only record, and it is an LRU citizen: under
    /// pressure the cache evicts a pending marker before its body lands, and
    /// with the marker gone this asked again — one tile, two fetches, two
    /// decodes, two uploads, and a second body landing as a duplicate put (16
    /// of them over 607 asks at cap 100 on a 12x12 grid, measured on the
    /// loopback pin). The in-flight entry is what the LRU cannot evict, and
    /// it is consulted **before** the send. When the request channel is full
    /// nothing is recorded and the tile is retried on a later frame —
    /// recording the ask while dropping the request would strand it forever.
    ///
    /// A slot from an **older** generation is a tile styled by a style
    /// [`Self::set_style`] has replaced: it keeps drawing (see
    /// [`Self::cached_or_interpolated`]) while this re-asks for it, and the
    /// re-stamp to the current generation is the same "a request is out"
    /// record as a fresh insert. A restyle does not close the in-flight set:
    /// a tile whose request is out is not re-asked under the new generation
    /// until that request is answered. On native its body arrives stamped
    /// with the stale generation, the drain drops it and closes the request,
    /// and the next frame re-asks; on wasm32 the pump styles the body against
    /// the frame's current style, so it lands current and no re-ask is owed.
    fn request_once(&mut self, tile_id: TileId) {
        // Recorded here and not in `try_request`, so a retry from the queue
        // does not count as the pass wanting the cell: only the walk says
        // that, and only what the walk still wants survives the next pass's
        // purge — see `AskQueue::new_pass`.
        self.asks.wanted(tile_id);
        if self.try_request(tile_id) == Ask::Refused {
            self.asks.refuse(tile_id);
        }
    }

    /// [`Self::request_once`]'s decision and send, with the outcome named so
    /// the refused-ask queue can act on it. Records nothing in the queue.
    fn try_request(&mut self, tile_id: TileId) -> Ask {
        if self.requests_closed {
            return Ask::Unneeded;
        }
        let epoch = self.style_epoch;

        // Split borrow: the sender is needed while the cache is borrowed.
        let Self {
            cache, request_tx, ..
        } = self;

        // `get`, not `peek`: a hit refreshes recency exactly as the old
        // `try_get_or_insert` did.
        let known = cache.get(&tile_id).map(|slot| slot.epoch);
        if known == Some(epoch) {
            return Ask::Unneeded;
        }
        // After the `get`, so a wanted tile whose request is out still has
        // its recency refreshed; before the send, so the LRU's treatment of
        // the marker decides nothing.
        if cache.is_in_flight(&tile_id) {
            return Ask::Unneeded;
        }

        let outcome = request_tx.try_send(tile_id).map(|()| {
            if known.is_some() {
                // The stale slot keeps its tile; the re-stamp is the "a
                // request is out" record.
                cache.re_ask(tile_id, epoch);
            } else {
                cache.ask(tile_id, epoch);
            }
        });

        match outcome {
            Ok(()) => {
                log::trace!("requested tile {tile_id:?}");
                Ask::Sent
            }
            Err(error) if error.is_full() => {
                log::trace!("tile request queue is full, retrying {tile_id:?} next frame");
                Ask::Refused
            }
            // walkers panics here. The IO task being gone is not worth taking the
            // UI thread down for; the map simply stops fetching -- once, and
            // saying so once. See [`Self::requests_closed`] for why the latch is
            // not optional.
            Err(error) => {
                log::error!(
                    "the tile IO task is gone, so this source stops fetching \
                     (asking for {tile_id:?}: {error})"
                );
                self.requests_closed = true;
                Ask::Unneeded
            }
        }
    }

    /// The tile itself, or the nearest cached ancestor stretched to fit.
    ///
    /// Starts at the requested zoom and walks outwards until something is cached,
    /// so a zoomed-in view shows a blurry ancestor rather than a hole. Starts no
    /// download of its own.
    ///
    /// **The style generation is deliberately not consulted**: mid-restyle, a
    /// tile styled by the outgoing style is what stands between the user and a
    /// blank beat, and its slot is already re-stamped for replacement by
    /// [`Self::request_once`].
    fn cached_or_interpolated(&mut self, tile_id: TileId) -> Option<GroundPiece> {
        let mut zoom_candidate = tile_id.zoom;

        loop {
            let (ancestor, uv) = interpolate_from_lower_zoom(tile_id, zoom_candidate);

            if let Some(CachedTile {
                tile: Some(cached),
                meshes,
                ..
            }) = self.cache.get(&ancestor)
            {
                break Some(GroundPiece {
                    tile: cached.clone(),
                    uv,
                    meshes: meshes.clone(),
                });
            }

            // Out of ancestors: nothing to draw for this tile yet.
            zoom_candidate = zoom_candidate.checked_sub(1)?;
        }
    }

    /// [`Tiles::at`]'s answer with the tile's flattened fills beside it — what
    /// the map's own tile pass draws through, since it is the only caller and
    /// walkers' trait has no room for the second half.
    pub(crate) fn ground_at(&mut self, tile_id: TileId) -> Option<GroundPiece> {
        if !tile_id_is_valid(tile_id) {
            return None;
        }
        let max_zoom = self.source_max_zoom()?;
        let to_fetch = if tile_id.zoom > max_zoom {
            interpolate_from_lower_zoom(tile_id, max_zoom).0
        } else {
            tile_id
        };
        self.request_once(to_fetch);
        self.cached_or_interpolated(tile_id)
    }

    /// Ask for `tile_id` without drawing it — the ancestor net.
    ///
    /// [`Self::ground_at`] asks only for the level it is drawing, and
    /// [`Self::cached_or_interpolated`] can only answer a miss with something
    /// *shallower* than the tile asked for. Together those two mean a zoom-out
    /// has nothing to draw: the tiles it was just looking at are descendants of
    /// what it now wants, no shallower level was ever requested, and the walk
    /// runs to zoom 0 finding nothing. The pane draws a hole for every cell —
    /// the black screen — until the network answers.
    ///
    /// So the shallow level is requested too, every frame, by
    /// `ui_map_overlays::draw_tile_layer`. It is prediction rather than repair:
    /// the fetch is off-thread on both targets, and by the time a zoom-out
    /// arrives the net is already resident and every cell has an ancestor to
    /// stretch. See [`crate::tiles::WARM_ANCESTOR_STEPS`] for why the net sits
    /// four steps out rather than one.
    ///
    /// Cheap to call every frame by construction: [`Self::request_once`]
    /// returns on an LRU hit at the current style generation, so a warm tile
    /// costs one probe after its first fetch — and the probe is what keeps the
    /// net's recency fresh, which is what stops the LRU evicting the one thing
    /// standing between the user and a hole.
    pub(crate) fn warm(&mut self, tile_id: TileId) {
        if !tile_id_is_valid(tile_id) {
            return;
        }
        // A net deeper than the source goes is not a net; the drawn level is
        // already clamped to this and the net is shallower still, so this only
        // ever fires for a source whose header has not landed.
        if self
            .source_max_zoom()
            .is_none_or(|deepest| tile_id.zoom > deepest)
        {
            return;
        }
        self.request_once(tile_id);
    }

    /// The source's deepest zoom, so a consumer picking its own zoom level
    /// (`ui_map_overlays`' tile pass, when a zoom bias asks for a level deeper
    /// than the pane's own) can clamp to what this source can serve.
    ///
    /// `None` while an archive source is still waiting on its header — see
    /// [`MAX_ZOOM_UNKNOWN`]. A caller that clamps to this must draw and request
    /// **nothing** in that state rather than substituting a number, which is
    /// what makes the difference between waiting a frame and seeding a z0 tile
    /// that becomes the session's fallback ancestor.
    pub fn source_max_zoom(&self) -> Option<u8> {
        match self.max_zoom.load(Ordering::Relaxed) {
            MAX_ZOOM_UNKNOWN => None,
            zoom => Some(zoom),
        }
    }

    /// Why this source will never serve a tile, if that is settled.
    ///
    /// `Some` only after the IO task has failed in a way no later frame can
    /// undo — today, an archive that will not open. A source that is merely
    /// slow, or that answered `404` for one tile, is `None`.
    pub fn fault(&self) -> Option<&str> {
        self.fault.get().map(String::as_str)
    }

    /// Whether this source's tile reads have been failing long enough that it
    /// is not putting the map on the glass — [`SUSTAINED_READ_FAILURES`] of
    /// them in a row with none answering.
    ///
    /// **Read it every frame; never latch it.** Unlike [`Self::fault`] it
    /// clears itself the moment a read answers, which is the whole point: the
    /// conditions that raise it are the ones a later frame can undo.
    pub fn reads_are_failing(&self) -> bool {
        self.reads_failing.load(Ordering::Relaxed)
    }

    /// Report this source's reads as failing, with no transport to fail them.
    ///
    /// For the credit-composition tests, which need the *live-source* degraded
    /// state — the one [`crate::tiles::MapTileState::latch_base_unreachable_for_test`]
    /// cannot produce, because it empties the slot. What those tests prove is
    /// therefore the composition given the state, not the state's own raising;
    /// that is `tile_source::tests::archive`'s.
    #[cfg(test)]
    pub(crate) fn fail_reads_for_test(&self) {
        self.reads_failing.store(true, Ordering::Relaxed);
    }

    /// Tiles currently held, including pending and failed markers. Exposed for
    /// the eviction test; gated off wasm32 with `mod tests`, its only caller.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn cached_entries(&self) -> usize {
        self.cache.len()
    }

    /// Requests open right now — see [`TileCache::in_flight`]. Exposed for
    /// the bound test; gated as [`Self::cached_entries`] is.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn in_flight_len(&self) -> usize {
        self.cache.in_flight_len()
    }

    /// This source's own cache counters — every event its cache recorded,
    /// and no other source's. The statics [`cache_ledger::totals`] reads
    /// are shared by every source of a role in the process, which in a test
    /// binary is every test's; a pin on one source reads this instead.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn cache_stats(&self) -> cache_ledger::Totals {
        self.cache.stats()
    }

    /// Whether `tile_id` currently occupies a slot, pending and failed markers
    /// included. A peek, not a use: `LruCache::contains` leaves the recency
    /// order alone.
    ///
    /// `cfg(test)` alone, NOT the `all(test, not(wasm32))` its neighbour
    /// [`Self::cached_entries`] carries, and for the reason
    /// [`Self::put_for_test`] gives: `ui_map_overlays`' seam tests read it to
    /// hold the ancestor net's request at the call site, and that module's
    /// tests compile on the wasm32 test target too. The body is portable.
    #[cfg(test)]
    pub(crate) fn tile_is_cached(&self, tile_id: TileId) -> bool {
        self.cache.contains(&tile_id)
    }

    /// Put `tile` in the cache under `tile_id`, as an arrived fetch would.
    ///
    /// Test-only, and the reason it exists is isolation rather than
    /// convenience: the seam's dispatch — `Tile::Vector` reaching the painter
    /// — stays testable without an archive open or an IO task running.
    /// Nothing asked for the tile, so the ledger classes the landing as an
    /// orphan (or a duplicate, over a resident slot) — the honest reading of
    /// a fixture, and one reason no test reads the statics for an absolute.
    ///
    /// `cfg(test)` alone, NOT the `all(test, not(wasm32))` its neighbours
    /// carry: its caller (`ui_map_overlays`' seam tests) compiles on the
    /// wasm32 test target too, and the body is portable. The narrower gate
    /// was an E0599 on `cargo check -p squallar-egui --all-targets --target
    /// wasm32-unknown-unknown`, found when the flip made that a default row.
    #[cfg(test)]
    pub(crate) fn put_for_test(&mut self, tile_id: TileId, tile: Tile) {
        let epoch = self.style_epoch;
        // The generation moves exactly as it does for a real arrival, so a
        // fixture's put is a tile-arrival staleness event for the strip key.
        self.put_generation += 1;
        // Through the same constructor an arrival goes through, so a fixture
        // carries the flattened fills a real tile carries.
        let feathering = flatten_feathering(self.feathering);
        self.cache.put(tile_id, slot_for(tile, epoch, feathering));
    }
}

impl Tiles for HttpsTiles {
    fn attribution(&self) -> Attribution {
        self.attribution.clone()
    }

    /// Return the tile if it is available, and start a download if it is not.
    ///
    /// Called once per grid cell of the layer being drawn, by
    /// `ui_map_overlays::draw_tile_layer` — **not** by walkers' flood fill,
    /// which is unreachable here because both `Map::new` sites in `ui_map` pass
    /// `None` for tiles. Everything here is cheap and non-blocking, and what the
    /// IO task has finished arrives through [`Self::pump`], once for the whole
    /// layer.
    fn at(&mut self, tile_id: TileId) -> Option<TilePiece> {
        // An archive source that has not read its header yet does not know what
        // it can serve. Asking anyway means asking for `0/0/0`, which is a real
        // tile that then never leaves the cache; see [`MAX_ZOOM_UNKNOWN`]. The
        // IO task repaints when the header lands, so this waits one frame.
        // Above the source's deepest zoom there is nothing to download; the
        // ancestor at `max_zoom` is what gets stretched over the gap. Both
        // rules live in [`Self::ground_at`], which this narrows.
        let piece = self.ground_at(tile_id)?;
        Some(TilePiece::new(piece.tile, piece.uv))
    }

    fn tile_size(&self) -> u32 {
        self.tile_size
    }
}

// ---------------------------------------------------------------------------
// The IO task
// ---------------------------------------------------------------------------

/// [`fetch_body`], with the id kept beside the outcome, so that a failure is
/// delivered under the id it was for — see [`Fetched`].
async fn fetch_one<S: TileSource>(
    source: &S,
    client: &reqwest::Client,
    egui_ctx: &Context,
    tile_id: TileId,
) -> (TileId, Result<FetchPayload, String>) {
    (tile_id, fetch_body(source, client, egui_ctx, tile_id).await)
}

/// Download one tile — and on native, decode and upload it too.
///
/// On native, decoding happens here on the IO runtime, as walkers does it:
/// [`Tile::new`] performs the PNG decode and the
/// [`egui::Context::load_texture`] call, and `Context` is `Send + Sync` and
/// locks internally. On wasm32 the IO "runtime" *is* the UI thread, so the bytes
/// are handed over undecoded — see [`FetchPayload`].
///
/// The error is a `String` because walkers' `TileError` is not exported.
async fn fetch_body<S: TileSource>(
    source: &S,
    client: &reqwest::Client,
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            unused_variables,
            reason = "on wasm the frame pump decodes; see FetchPayload"
        )
    )]
    egui_ctx: &Context,
    tile_id: TileId,
) -> Result<FetchPayload, String> {
    let url = source.tile_url(tile_id);
    log::trace!("downloading '{url}'");

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|error| format!("requesting '{url}': {error}"))?
        .error_for_status()
        .map_err(|error| format!("requesting '{url}': {error}"))?;

    let body = response
        .bytes()
        .await
        .map_err(|error| format!("reading '{url}': {error}"))?;

    // An empty style. This function serves a raster source, and `Tile::new`
    // only consults a style for a body that is not a recognised image. The
    // epoch is likewise the raster constant: this source never restyles.
    #[cfg(not(target_arch = "wasm32"))]
    let payload = slot_for(
        Tile::new(&body, &Style::default(), tile_id.zoom, egui_ctx)
            .map_err(|error| format!("decoding '{url}': {error}"))?,
        RASTER_STYLE_EPOCH,
        // A raster body flattens to nothing, so no feathering is consulted.
        0.0,
    );

    // On wasm the decode belongs to the frame pump, under its budget.
    #[cfg(target_arch = "wasm32")]
    let payload = FetchPayload::Bytes(Arc::new(body.to_vec()));

    Ok(payload)
}

/// Serve tile requests until [`HttpsTiles`] is dropped.
///
/// Three states, mirroring walkers' `fetch_continuously_impl`:
///
/// * nothing in flight — only a new request can make progress;
/// * under the concurrency limit — a new request *or* a completion;
/// * at the limit — only a completion, which is what enforces the limit.
///
/// Either channel closing ends the loop, which is how the task learns that
/// [`HttpsTiles`] is gone.
async fn fetch_continuously<S: TileSource>(
    source: S,
    client: reqwest::Client,
    mut request_rx: Receiver<TileId>,
    mut tile_tx: Sender<Fetched>,
    egui_ctx: Context,
) {
    let mut outstanding = FuturesUnordered::new();

    loop {
        let completed = if outstanding.is_empty() {
            match request_rx.next().await {
                Some(tile_id) => {
                    outstanding.push(fetch_one(&source, &client, &egui_ctx, tile_id));
                    continue;
                }
                None => break,
            }
        } else if outstanding.len() < MAX_PARALLEL_DOWNLOADS {
            match select(request_rx.next(), outstanding.next()).await {
                Either::Left((Some(tile_id), pending)) => {
                    // Release the borrow of `outstanding` before pushing. Dropping
                    // a `Next` does not cancel the futures inside it.
                    drop(pending);
                    outstanding.push(fetch_one(&source, &client, &egui_ctx, tile_id));
                    continue;
                }
                Either::Left((None, _)) => break,
                Either::Right((completed, _)) => completed,
            }
        } else {
            outstanding.next().await
        };

        // `outstanding` was non-empty on both paths that reach here, so this is
        // always `Some`; treating it otherwise would just spin.
        let Some((tile_id, result)) = completed else {
            continue;
        };

        // A failure is delivered too, as the empty answer — see [`Fetched`].
        let answer = match result {
            Ok(payload) => Some(payload),
            Err(error) => {
                log::warn!("{error}");
                None
            }
        };
        let arrived = answer.is_some();
        if tile_tx.send((tile_id, answer)).await.is_err() {
            break;
        }
        if arrived {
            // Without this the tile sits in the channel until some unrelated
            // input wakes the UI, and the map appears to stop loading. An
            // empty answer changes no pixel and asks for no frame.
            egui_ctx.request_repaint();
        }
    }

    log::debug!("tile fetch loop finished");
}

// ---------------------------------------------------------------------------
// The archive source
// ---------------------------------------------------------------------------

/// Tiles served out of a self-hosted PMTiles v3 archive rather than one HTTP
/// GET per tile.
///
/// Everything downstream of the fetch is the raster path unchanged: the same
/// LRU, the same de-duplication, the same interpolation from a shallower
/// ancestor, the same [`HttpsTiles::pump`] contract. Only where the bytes come
/// from, and what they decode into, differ.
impl HttpsTiles {
    /// Serve tiles from the PMTiles archive at `url`, rendered against `style`.
    ///
    /// **The archive is opened by the IO task, not here.** Reading the header
    /// and the root directory is two range requests over the network, and this
    /// is called from a frame.
    ///
    /// So [`Self::source_max_zoom`] answers `None` until the header lands, and
    /// [`Tiles::at`] draws and requests nothing while it does — see
    /// [`MAX_ZOOM_UNKNOWN`]. **It used to answer `0`, and that was a defect
    /// dressed as caution**: `at` clamps a deeper request down to the maximum,
    /// so every pane's first frames asked for `0/0/0`, that tile stayed in the
    /// LRU, and from then on it was the fallback ancestor
    /// [`Self::cached_or_interpolated`] walked to for any deep tile in flight —
    /// drawn stretched over the whole viewport. The doc that argued for it
    /// reasoned about rasters ("a real tile drawn stretched"), and a stretched
    /// raster is blurry while a stretched vector tile has its stroke widths
    /// stretched too. `MAX_ZOOM_UNKNOWN` records the header's absence for what
    /// it is; the IO task repaints the moment it lands, so this costs a frame.
    ///
    /// # Errors
    ///
    /// [`crate::basemap_archive::RangeError`] if `url` will not parse. A URL
    /// that parses but does not answer is a *runtime* failure of the IO task;
    /// it cannot be reported here, because nothing has been asked for yet, so
    /// it is recorded on [`Self::fault`] instead of only in the log.
    /// `cache` is the persistent block cache's configuration, decided by the
    /// caller because deriving it takes *every* archive URL the build reads
    /// (the GC's live-generation set, from `tiles::live_archive_urls`) —
    /// `None` reads uncached, exactly as before the cache existed. The basemap source is also the one that seeds: the z0–z5
    /// warm-up runs through this source's cache, once per generation.
    pub fn from_archive_url(
        url: &str,
        styling: BasemapStyling,
        attribution: Attribution,
        egui_ctx: Context,
        cache: Option<crate::basemap_archive::block_cache::BlockCacheConfig>,
        offline: Option<crate::basemap_download::PlatformSegmentStore>,
    ) -> Result<Self, crate::basemap_archive::RangeError> {
        use crate::basemap_archive::{HttpRangeSource, archive_client, block_cache};

        let source = block_cache::BlockCachedSource::new(
            HttpRangeSource::new(archive_client(), url)?,
            cache.clone(),
        );
        // The budgets this build starts on; `MapTileState::set_budget` hands
        // the resolved ones over the moment the source is in its slot.
        let budget = default_tile_budget();
        Ok(Self::from_range_source(
            source,
            styling,
            attribution,
            egui_ctx,
            SourceBudget {
                styled_bytes: budget.styled_bytes,
                parsed_bytes: Some(budget.parsed_bytes),
            },
            cache_ledger::CacheRole::Base,
            // The header says `tile_type = 1` and that is the whole truth
            // about a vector archive; nothing here knows better.
            None,
            ArchiveStores {
                seed: cache,
                offline,
                offline_archive: crate::basemap_download::AreaArchive::Basemap,
            },
        ))
    }

    /// Serve the terrain hillshade archive at `url`.
    ///
    /// [`Self::from_archive_url`]'s twin with terrain's facts: an empty style
    /// (the bodies are raster; nothing tessellates, and the raster decode
    /// never reads a style), the terrain allowance as the bound, and **no
    /// parsed cache**: a raster archive has no style-independent decode to
    /// keep, so a parsed population here would be residency with no consumer.
    /// The hillshade remap is **not** chosen here: the archive's own header
    /// (`tile_type = 4`) routes its bodies through it — see
    /// [`ArchiveTileKind`].
    ///
    /// # Errors
    ///
    /// As [`Self::from_archive_url`]: only a URL that will not parse fails at
    /// construction; everything later lands on [`Self::fault`].
    /// `cache` as on [`Self::from_archive_url`]. The terrain source shares
    /// the cache root (its generation directory sits beside the basemap's)
    /// but never seeds: the seed is the basemap's z0–z5, by design.
    pub fn from_terrain_archive_url(
        url: &str,
        attribution: Attribution,
        egui_ctx: Context,
        cache: Option<crate::basemap_archive::block_cache::BlockCacheConfig>,
        offline: Option<crate::basemap_download::PlatformSegmentStore>,
    ) -> Result<Self, crate::basemap_archive::RangeError> {
        use crate::basemap_archive::{HttpRangeSource, archive_client, block_cache};

        let source = block_cache::BlockCachedSource::new(
            HttpRangeSource::new(archive_client(), url)?,
            cache,
        );
        Ok(Self::from_range_source(
            source,
            // Pictures, not geometry: nothing here restyles, and nothing here
            // offloads. See `BasemapStyling::raster`.
            BasemapStyling::raster(),
            attribution,
            egui_ctx,
            SourceBudget {
                styled_bytes: default_tile_budget().terrain_bytes,
                parsed_bytes: None,
            },
            cache_ledger::CacheRole::Terrain,
            // The hillshade is the app's only *pictorial* raster archive, so
            // the header's `tile_type = 4` already routes it correctly. A
            // declaration here would claim a fact the header carries.
            None,
            ArchiveStores {
                // No seed: the warm-up is the basemap's shallow zooms, not
                // the hillshade's.
                seed: None,
                offline,
                offline_archive: crate::basemap_download::AreaArchive::Terrain,
            },
        ))
    }

    /// [`Self::from_archive_url`] with the range source and the byte budgets
    /// supplied. Crate-private, for the tests, which read the committed Monaco
    /// fixture off disk rather than over a network. `budget.parsed_bytes`
    /// `None` builds no parsed cache at all — the raster archives' shape.
    ///
    /// The two stores travel as one [`ArchiveStores`] rather than as three
    /// arguments, because they mean one thing — what this source consults
    /// before the network — and the archive kind is meaningless apart from the
    /// store it filters.
    ///
    /// `declared_kind` is what the caller claims the bodies are, for the one
    /// case the header cannot say: `Some(ArchiveTileKind::TerrainRgb)` over a
    /// PNG archive. It is verified against the header at open and is not a
    /// pre-set answer — see [`ArchiveSlots::declared`]. `None` leaves
    /// the header's word standing, which is what both published archives use.
    ///
    /// `role` is which cache [`cache_ledger`] files this source's events
    /// under; the two public constructors each name their own.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_range_source<S, St>(
        source: S,
        styling: BasemapStyling,
        attribution: Attribution,
        egui_ctx: Context,
        budget: SourceBudget,
        role: cache_ledger::CacheRole,
        declared_kind: Option<ArchiveTileKind>,
        stores: ArchiveStores<St>,
    ) -> Self
    where
        S: crate::basemap_archive::ArchiveRangeSource + 'static,
        St: crate::basemap_download::OfflineSegments,
    {
        let (request_tx, request_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let (tile_tx, tile_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        // Where a finished batch lands. See `HttpsTiles::batch_tx`.
        #[cfg(target_arch = "wasm32")]
        let (batch_tx, batch_rx) = unbounded();

        let style = styling.style;
        // The key is the worker's half of a styling and never the frame's, so
        // it exists only where a worker does.
        #[cfg(target_arch = "wasm32")]
        let style_key = styling.key;
        let max_zoom = Arc::new(AtomicU8::new(MAX_ZOOM_UNKNOWN));
        let fault: Arc<OnceLock<String>> = Arc::new(OnceLock::new());
        let reads_failing = Arc::new(AtomicBool::new(false));
        // How this archive's bodies decode -- filled in by the IO task from
        // the header, at open, before it serves a tile. See [`ArchiveTileKind`].
        let archive_kind: Arc<OnceLock<ArchiveTileKind>> = Arc::new(OnceLock::new());

        // The parsed-geometry cache the IO task and the frame side share, and
        // the slot a restyle publishes the new style through. See
        // [`SharedParsedTiles`] and [`HttpsTiles::set_style`]. `None` for a
        // source with nothing style-independent to keep.
        let parsed: Option<SharedParsedTiles> = budget
            .parsed_bytes
            .map(|bytes| Arc::new(Mutex::new(byte_lru::ByteLru::new(bytes))));
        // The undecoded bodies, for the restyle the parsed cache stops
        // serving once the pump offloads, under the same allowance. See
        // [`SharedTileBodies`]; `()` on native, where the parsed cache still
        // serves it.
        #[cfg(target_arch = "wasm32")]
        let tile_bodies: IoTileBodies = budget
            .parsed_bytes
            .map(|bytes| Arc::new(Mutex::new(byte_lru::ByteLru::new(bytes))));
        #[cfg(not(target_arch = "wasm32"))]
        let tile_bodies: IoTileBodies = ();
        // The frame side's handle to the bodies, to budget and trim them:
        // a clone of the IO task's on wasm32, nothing where none is kept.
        #[cfg(target_arch = "wasm32")]
        let frame_bodies: Option<SharedTileBodies> = tile_bodies.clone();
        #[cfg(not(target_arch = "wasm32"))]
        let frame_bodies: Option<SharedTileBodies> = None;
        #[cfg(not(target_arch = "wasm32"))]
        let style_slot: IoStyleSlot = Arc::new(std::sync::RwLock::new(StyleSlot {
            style: Arc::clone(&style),
            epoch: RASTER_STYLE_EPOCH,
            // Refuses every stroke until the frame side says what it draws
            // at; see `HttpsTiles::set_feathering`.
            feathering: 0.0,
        }));
        #[cfg(target_arch = "wasm32")]
        let style_slot: IoStyleSlot = ();

        // Both clones exist for the reason `with_client_and_budget` clones the
        // context: on wasm32 the frame pump is the tessellating side, so it
        // needs the context to upload through and the style to render against.
        // The IO task is handed its own pair either way -- `read_one` takes
        // both and ignores them on the target that does not decode there.
        #[cfg(target_arch = "wasm32")]
        let frame_ctx = egui_ctx.clone();
        #[cfg(target_arch = "wasm32")]
        let frame_style = Arc::clone(&style);
        #[cfg(target_arch = "wasm32")]
        let frame_archive_kind = Arc::clone(&archive_kind);
        let frame_parsed = parsed.clone();
        #[cfg(not(target_arch = "wasm32"))]
        let frame_style_slot = Arc::clone(&style_slot);

        let runtime = runtime::spawn(serve_archive_continuously(
            source,
            ArchiveStyling {
                style: style_slot,
                tile_bodies,
                parsed_tiles: parsed,
                role,
            },
            ArchiveSlots {
                max_zoom: Arc::clone(&max_zoom),
                fault: Arc::clone(&fault),
                reads_failing: Arc::clone(&reads_failing),
                kind: archive_kind,
                declared: declared_kind,
            },
            request_rx,
            tile_tx,
            egui_ctx,
            stores,
        ));

        Self {
            attribution,
            // The side a vector tile's extent is mapped onto. 256 is what
            // `crate::tiles::TILE_SIDE_POINTS` places a tile at, and the two
            // must agree: `walkers::mercator::tile_id` folds a larger declared
            // size into the zoom, which would ask the archive for a level
            // shallower than the one being drawn.
            tile_size: crate::tiles::TILE_SIDE_POINTS as u32,
            max_zoom,
            fault,
            reads_failing,
            requests_closed: false,
            cache: TileCache::new(budget.styled_bytes, role),
            style_epoch: RASTER_STYLE_EPOCH,
            feathering: None,
            #[cfg(not(target_arch = "wasm32"))]
            style_slot: Some(frame_style_slot),
            parsed: frame_parsed,
            bodies: frame_bodies,
            wanted: WantedTally::default(),
            asks: AskQueue::default(),
            snap: snap::SnapState::default(),
            whole_zoom_rung: false,
            unsnapped: WantedTally::default(),
            request_tx,
            tile_rx,
            #[cfg(target_arch = "wasm32")]
            egui_ctx: frame_ctx,
            #[cfg(target_arch = "wasm32")]
            pump_budget: PumpBudget::new(),
            // NOT the raster path's `Style::default()`: this is the committed
            // style, and it is what `Tile::new` renders the MVT body against.
            // An empty one would hand back a blank tile for every road and
            // every label.
            #[cfg(target_arch = "wasm32")]
            style: frame_style,
            #[cfg(target_arch = "wasm32")]
            archive_kind: Some(frame_archive_kind),
            #[cfg(target_arch = "wasm32")]
            batch: TileBatch::default(),
            #[cfg(target_arch = "wasm32")]
            batch_tx,
            #[cfg(target_arch = "wasm32")]
            batch_rx,
            #[cfg(target_arch = "wasm32")]
            style_key,
            io_task_gone_reported: false,
            put_generation: 0,
            pumps: 0,
            takes: 0,
            runtime,
        }
    }

    /// A source that will never fetch: the shape of a just-built source —
    /// its credit line carried, its header unread, its cache empty — held
    /// forever, because no IO task exists behind it.
    ///
    /// **For [`crate::tiles::MapTileState::go_offline_for_tests`], and
    /// nothing else.** A live source races the frames of whatever built it:
    /// tiles that arrive paint label `TextShape`s at arbitrary positions,
    /// and a transport fault latches the unreachable credit — either way a
    /// unit test's glass changes with how much wall-clock time the test
    /// took. Inert, the glass is the fast-test glass on every run: ground
    /// pending, credit the provider's own, no thread, no socket.
    ///
    /// The far channel ends are dropped, so the first tile request trips the
    /// once-only "tile IO task is gone" latch instead of queueing — the same
    /// quiet the frame side already keeps when a real IO task has exited.
    pub(crate) fn inert(attribution: Attribution, egui_ctx: Context) -> Self {
        let (request_tx, _request_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let (_tile_tx, tile_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        // Where a finished batch lands. See `HttpsTiles::batch_tx`.
        #[cfg(target_arch = "wasm32")]
        let (batch_tx, batch_rx) = unbounded();
        let _ = &egui_ctx;

        Self {
            attribution,
            tile_size: crate::tiles::TILE_SIDE_POINTS as u32,
            max_zoom: Arc::new(AtomicU8::new(MAX_ZOOM_UNKNOWN)),
            fault: Arc::new(OnceLock::new()),
            reads_failing: Arc::new(AtomicBool::new(false)),
            requests_closed: false,
            cache: TileCache::new(
                default_tile_budget().styled_bytes,
                cache_ledger::CacheRole::Base,
            ),
            // An inert source never restyles and never parses -- the same
            // never-restyles spelling as a raster source, which is what the
            // epoch constant's doc names for this case.
            style_epoch: RASTER_STYLE_EPOCH,
            feathering: None,
            #[cfg(not(target_arch = "wasm32"))]
            style_slot: None,
            parsed: None,
            bodies: None,
            wanted: WantedTally::default(),
            asks: AskQueue::default(),
            snap: snap::SnapState::default(),
            whole_zoom_rung: false,
            unsnapped: WantedTally::default(),
            request_tx,
            tile_rx,
            #[cfg(target_arch = "wasm32")]
            egui_ctx,
            #[cfg(target_arch = "wasm32")]
            pump_budget: PumpBudget::new(),
            #[cfg(target_arch = "wasm32")]
            style: Arc::new(Style::default()),
            #[cfg(target_arch = "wasm32")]
            archive_kind: None,
            #[cfg(target_arch = "wasm32")]
            batch: TileBatch::default(),
            #[cfg(target_arch = "wasm32")]
            batch_tx,
            #[cfg(target_arch = "wasm32")]
            batch_rx,
            #[cfg(target_arch = "wasm32")]
            style_key: None,
            io_task_gone_reported: false,
            put_generation: 0,
            pumps: 0,
            takes: 0,
            runtime: runtime::inert(),
        }
    }
}

/// The two halves of the restyle seam, as the archive IO task carries them:
/// what a tile is styled against ([`IoStyleSlot`]; nothing on wasm32) and
/// where its parse is remembered. One parameter because every [`read_one`]
/// needs both together.
struct ArchiveStyling {
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            dead_code,
            reason = "the wasm arm of IoStyleSlot is (); the frame pump owns the style"
        )
    )]
    style: IoStyleSlot,
    /// The undecoded bodies this source keeps for restyling — see
    /// [`SharedTileBodies`]. `()` on native, whose restyle is served from the
    /// parsed cache instead.
    tile_bodies: IoTileBodies,
    /// `None` for a raster archive, which has no style-independent decode to
    /// keep and so builds no cache to keep it in.
    parsed_tiles: Option<SharedParsedTiles>,
    /// Which cache the parses land for — the level
    /// [`cache_ledger::set_parsed`] is stored under. Read where the parse is
    /// remembered, which on wasm32 is the frame pump and not here.
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            dead_code,
            reason = "on wasm the frame pump remembers the parse and carries its own role"
        )
    )]
    role: cache_ledger::CacheRole,
}

/// What this device already holds of the archive, and where.
///
/// One parameter rather than two because they are the same kind of fact —
/// bytes on the device rather than on the wire — and because the archive loop
/// does the same thing with both: consults them before the network, once, at
/// open. The block cache is a *tier* under the reads; a downloaded area is a
/// *source* in front of them.
pub(crate) struct ArchiveStores<St> {
    /// The persistent block cache to seed with the shallow zooms, or `None`
    /// for a source that must not seed (terrain) or a target with no
    /// filesystem.
    pub(crate) seed: Option<crate::basemap_archive::block_cache::BlockCacheConfig>,
    /// The downloaded offline areas to put in front of the live archive, or
    /// `None` when this device holds none.
    pub(crate) offline: Option<St>,
    /// Which archive's segments this source composes. A store holds an area's
    /// base map and its hillshade side by side, and each reader takes only its
    /// own: a segment of the other kind would be refused on its tile type
    /// anyway, but only after an open, which is a read per segment per launch
    /// to learn what its name already says.
    pub(crate) offline_archive: crate::basemap_download::AreaArchive,
}

/// What the IO task publishes to the frame side — created by
/// [`HttpsTiles::from_range_source`], filled in by
/// [`serve_archive_continuously`]. One parameter rather than four, because
/// they are the same kind of fact: what the frame may ask this source for, and
/// whether it is answering.
///
/// All but one are written exactly once, at open. [`Self::reads_failing`] is
/// the exception and says so on itself: it is a *condition*, raised and
/// dropped by the serve loop for as long as the source lives.
struct ArchiveSlots {
    max_zoom: Arc<AtomicU8>,
    fault: Arc<OnceLock<String>>,
    /// [`HttpsTiles::reads_failing`]'s far end — the only slot here the serve
    /// loop writes more than once, and the only one that can go back down.
    reads_failing: Arc<AtomicBool>,
    kind: Arc<OnceLock<ArchiveTileKind>>,
    /// What the *caller* says this archive holds, when the header cannot say
    /// it — the only way [`ArchiveTileKind::TerrainRgb`] is ever reached.
    ///
    /// **A declaration, not a pre-set `OnceLock`.** `kind` is still written
    /// once by the IO task at open, and writing it is what
    /// [`serve_archive_continuously`] does *after* cross-checking this against
    /// the header. Pre-setting `kind` from the constructor would make the
    /// declaration unfalsifiable: a `TerrainRgb` sitting over an MVT archive
    /// would decode nothing and report no fault.
    declared: Option<ArchiveTileKind>,
}

impl ArchiveTileKind {
    /// The header's `tile_type`, as a decode decision. See the enum's own
    /// docs for the arms; the mapping is total so a new pmtiles `TileType`
    /// variant is a compile error here rather than a silent sniff.
    fn from_tile_type(tile_type: pmtiles::TileType) -> Self {
        use pmtiles::TileType;
        match tile_type {
            TileType::Mvt | TileType::Mlt => Self::Vector,
            TileType::Png | TileType::Jpeg | TileType::Webp | TileType::Avif => Self::Hillshade,
            TileType::Unknown => Self::Undeclared,
        }
    }

    /// Whether a caller may declare an archive whose header says `tile_type`
    /// to be this kind.
    ///
    /// [`Self::TerrainRgb`] is the whole reason declarations exist, and it is
    /// the only kind the header cannot confirm: `tile_type = 2` is PNG for a
    /// hillshade and for an elevation grid alike, so all this can check is
    /// that the bodies are PNG at all. Every other kind is checkable exactly,
    /// so it is checked exactly — a declaration nobody could falsify would be
    /// a claim rather than a mechanism.
    fn accepts_tile_type(self, tile_type: pmtiles::TileType) -> bool {
        match self {
            Self::TerrainRgb => tile_type == pmtiles::TileType::Png,
            exact => exact == Self::from_tile_type(tile_type),
        }
    }
}

/// Open the archive, then answer tile requests out of it until [`HttpsTiles`]
/// is dropped.
///
/// The same three-state loop as [`fetch_continuously`], for the same reason: a
/// bounded number of reads in flight is what keeps a pan from opening one range
/// request per visible tile at once.
///
/// **Where the tessellation happens is the target's, not this loop's**, and on
/// neither target is it this task. A vector body's per-tile cost is
/// `mvt::parse` plus `mvt::styled` ([`decode_archive_tile_remembering`]), the
/// parse remembered in [`SharedParsedTiles`] so a restyle re-runs only the
/// second half. On native [`read_one`] hands the work to the runtime's
/// blocking pool and the frame side only moves a finished `Tile` into the
/// cache; on wasm32 there is no other thread to pay it on, so the body crosses
/// undecoded and [`HttpsTiles::drain_completed_fetches`] pays it under
/// [`WASM_TILE_DECODES_PER_PUMP`]. See [`FetchPayload`].
///
/// The native render used to run inline here, and "on the IO thread" undersold
/// what that cost: this task is a *current-thread* runtime on one `std::thread`
/// ([`runtime::spawn`]), so a 24.8 ms `mvt::render` was 24.8 ms in which no
/// range request could progress either. `MAX_PARALLEL_DOWNLOADS` bounds the
/// reads and bounded nothing about the tessellations, so filling a fresh
/// 54-tile viewport was ~1.34 s of CPU serialized behind itself while the
/// fetches and the tessellations starved each other.
async fn serve_archive_continuously<S, St>(
    source: S,
    styling: ArchiveStyling,
    slots: ArchiveSlots,
    mut request_rx: Receiver<TileId>,
    mut tile_tx: Sender<Fetched>,
    egui_ctx: Context,
    stores: ArchiveStores<St>,
) where
    S: crate::basemap_archive::ArchiveRangeSource,
    St: crate::basemap_download::OfflineSegments,
{
    use crate::basemap_archive::BasemapArchives;

    let mut archive = match BasemapArchives::open(source).await {
        Ok(archive) => archive,
        Err(error) => {
            // **Recorded, not only logged.** Returning here drops `request_rx`,
            // and a dropped receiver is what the frame side sees as a
            // *disconnected* sender -- which reports `is_full() == false` and so
            // used to fall through to one `error!` per visible tile per frame,
            // for as long as the app was open, over a map that stayed blank with
            // nothing on the glass to say why. `HttpsTiles::requests_closed`
            // bounds the flood; this is what lets the UI act on it.
            let reason = error.to_string();
            log::error!("the basemap archive will not open, so it serves no tiles: {reason}");
            let _ = slots.fault.set(reason);
            // Nothing else would wake the UI to notice.
            egui_ctx.request_repaint();
            return;
        }
    };

    // The declaration, cross-checked against the header before anything is
    // served. A declared kind the header contradicts is recorded and returned
    // from exactly as an archive that would not open is: the composition is
    // wrong about what it is reading, and serving bodies to the wrong decoder
    // would put noise on the glass instead of saying so.
    let kind = match slots.declared {
        Some(declared) if !declared.accepts_tile_type(archive.tile_type()) => {
            let reason = format!(
                "the archive was opened as {declared:?}, but its header says \
                 tile_type {:?}",
                archive.tile_type()
            );
            log::error!("the basemap archive is not what it was declared to be: {reason}");
            let _ = slots.fault.set(reason);
            egui_ctx.request_repaint();
            return;
        }
        // The caller knows something the header cannot carry.
        Some(declared) => declared,
        // Nobody claimed anything, so the header's word stands.
        None => ArchiveTileKind::from_tile_type(archive.tile_type()),
    };

    slots.max_zoom.store(archive.max_zoom(), Ordering::Relaxed);
    // What the bodies are, published before the first tile is served: on
    // wasm32 the frame pump decodes, and this is how it knows which decoder
    // the archive calls for.
    let _ = slots.kind.set(kind);
    log::info!(
        "basemap archive open: zooms {}-{}, tiles {:?}, compression {:?}",
        archive.min_zoom(),
        archive.max_zoom(),
        archive.tile_type(),
        archive.tile_compression()
    );
    // The header changed what `at` may ask for, and nothing else would wake the
    // UI to ask for it.
    egui_ctx.request_repaint();

    // Downloaded areas go in front of the live archive **after** the header
    // is published and the repaint asked for, so opening them can never delay
    // first paint. Each is local storage, not the network; a store that will
    // not list, and a segment that will not open, are logged inside and leave
    // the composition serving from the live archive alone.
    if let Some(store) = stores.offline {
        for (label, source) in store.open_all(stores.offline_archive).await {
            archive.attach_offline(label, source).await;
        }
        if archive.offline_count() > 0 {
            // The ceiling can only have risen, and `Tiles::at` reads it.
            slots.max_zoom.store(archive.max_zoom(), Ordering::Relaxed);
            log::info!(
                "{} downloaded basemap segments serve before the network",
                archive.offline_count()
            );
        }
    }

    // In an `Arc` so the seed below can hold the archive while this loop
    // serves from it; costs nothing when there is no seed.
    let archive = Arc::new(archive);

    // Warm the block cache with the shallow zooms, in the background on this
    // same runtime — after the header is published and the repaint is asked
    // for, so the seed can never delay first paint. A no-op when `seed` is
    // `None` (terrain, no cache dir) and on wasm32 (a cfg-selected body).
    // **The live archive's**: the seed is what fills the persistent block
    // cache for the remote generation, and a downloaded segment is already
    // resident by definition.
    crate::basemap_archive::block_cache::maybe_seed(archive.live(), stores.seed);

    let mut outstanding = FuturesUnordered::new();
    let mut failures = ReadFailureRun::new(slots.reads_failing);

    loop {
        let completed = if outstanding.is_empty() {
            match request_rx.next().await {
                Some(tile_id) => {
                    outstanding.push(read_one(&archive, &styling, &egui_ctx, kind, tile_id));
                    continue;
                }
                None => break,
            }
        } else if outstanding.len() < MAX_PARALLEL_DOWNLOADS {
            match select(request_rx.next(), outstanding.next()).await {
                Either::Left((Some(tile_id), pending)) => {
                    // Release the borrow of `outstanding` before pushing.
                    drop(pending);
                    outstanding.push(read_one(&archive, &styling, &egui_ctx, kind, tile_id));
                    continue;
                }
                Either::Left((None, _)) => break,
                Either::Right((completed, _)) => completed,
            }
        } else {
            outstanding.next().await
        };

        let Some((tile_id, result)) = completed else {
            continue;
        };

        // Every outcome is delivered under its id — a body, or the empty
        // answer that closes the request; see [`Fetched`].
        let answer = match result {
            // The archive positively holds no tile there. Not an error and not
            // a retry: the `None` the cache already carries under this id is
            // the right answer for as long as the cache keeps it. It is also
            // the archive *answering*, which is why it ends a failure run — a
            // viewport of open ocean must not read as an archive that is not
            // there.
            Ok(None) => {
                if failures.answered() {
                    log::info!("{RECOVERED}");
                    // An absent tile has nothing to draw, so without this the
                    // recovered credit would wait for whatever asked next.
                    egui_ctx.request_repaint();
                }
                None
            }
            Ok(Some(payload)) => {
                if failures.answered() {
                    log::info!("{RECOVERED}");
                }
                Some(payload)
            }
            Err(error) => {
                log::warn!("{error}");
                if failures.failed() {
                    log::error!(
                        "{} tile reads in a row failed with none answering, so this archive is \
                         drawing nothing; the credit corner says so until one answers",
                        failures.len(),
                    );
                    // Nothing else would wake the UI to repaint the credit: a
                    // failed read has no tile to draw.
                    egui_ctx.request_repaint();
                }
                None
            }
        };
        let arrived = answer.is_some();
        if tile_tx.send((tile_id, answer)).await.is_err() {
            break;
        }
        if arrived {
            egui_ctx.request_repaint();
        }
    }

    log::debug!("archive tile loop finished");
}

/// The unbroken run of failed tile reads, and the verdict it publishes to the
/// frame side through [`HttpsTiles::reads_failing`].
///
/// **A type rather than three statements inside the serve loop**, because the
/// rule is the whole of what keeps a map drawing nothing from being credited
/// to a provider, and a rule spelled inline can only ever be exercised through
/// a real archive on a real clock — which is the one shape that cannot pin a
/// threshold: a run that raises the verdict and a read that clears it can both
/// land between two frames, so an end-to-end observation misses an over-eager
/// rule instead of reddening on it.
///
/// Recovery lives here too, and it is [`Self::answered`] and nothing else: no
/// session-lifetime latch stands between a read that answers and the credit
/// coming back. **What supplies that read is the frame side, not this loop.**
/// A tile whose read failed keeps its cache slot and is never asked for again
/// (see [`HttpsTiles::cache`]), so the reads that clear the verdict come from
/// ids the source has not been asked for — a pan, a zoom, an eviction. That is
/// not a gap: while nothing re-asks, nothing draws either, so the notice stays
/// true for exactly as long as the map stays blank.
struct ReadFailureRun {
    run: usize,
    failing: Arc<AtomicBool>,
}

impl ReadFailureRun {
    fn new(failing: Arc<AtomicBool>) -> Self {
        Self { run: 0, failing }
    }

    /// A read answered — with a body, or with the archive's authoritative "no
    /// tile at this coordinate". Answers whether that *changed* the verdict,
    /// so the caller says the recovery once per run rather than once per tile.
    fn answered(&mut self) -> bool {
        self.run = 0;
        self.failing.swap(false, Ordering::Relaxed)
    }

    /// A read failed. Answers whether that changed the verdict, which is at
    /// most once per run for the same reason.
    fn failed(&mut self) -> bool {
        self.run += 1;
        self.run >= SUSTAINED_READ_FAILURES && !self.failing.swap(true, Ordering::Relaxed)
    }

    /// How long the current run is, for the line that reports it.
    fn len(&self) -> usize {
        self.run
    }
}

/// Read one tile out of the archive -- and on native, tessellate and upload it
/// too. Or skip the archive entirely: a tile whose parse is already in
/// [`SharedParsedTiles`] is re-styled from it, which is the whole of what a
/// theme flip or a detail toggle costs since [`HttpsTiles::set_style`].
///
/// `kind` is decided once, at open, by [`serve_archive_continuously`] —
/// **not re-derived here**. On native this function is the decoding side, so a
/// second derivation from the header would silently overrule a declaration and
/// hand a terrain-RGB body to the hillshade decoder; see [`ArchiveTileKind`].
///
/// `Ok(None)` is the archive positively holding nothing at that coordinate --
/// an ocean tile at zoom 14 -- which is why
/// [`crate::basemap_archive::TileBytes`] is a type rather than an empty `Vec`.
///
/// The payload split is [`fetch_one`]'s, for [`FetchPayload`]'s reason and not
/// a second one: on native the tessellation happens here, on wasm32 the IO task
/// *is* the page thread, so the MVT body (or the remembered parse) crosses the
/// channel unstyled and [`HttpsTiles::drain_completed_fetches`] renders it
/// under [`WASM_TILE_DECODES_PER_PUMP`]. Doing it here on wasm32 would put an
/// unbounded tessellation on the frame thread; doing it there puts a bounded
/// one.
///
/// **On native the render runs on the runtime's blocking pool, not on the
/// calling task**, measured at **24.8 ms** for the committed Monaco fixture's
/// z14 city-core tile against the 95-layer committed style, release build,
/// 2026-08-28 (parse and styling fused; the split does the same work in two
/// named halves). There is no await point across it, so before this
/// [`MAX_PARALLEL_DOWNLOADS`] bounded the range requests and bounded nothing
/// about the tessellations: they ran one after another on [`runtime::spawn`]'s
/// single current-thread runtime, and during each 24.8 ms slice no range
/// request progressed either. A fresh 54-tile viewport of tiles this dense was
/// ~1.34 s of CPU serialized behind itself. `spawn_blocking` puts each render
/// on its own pool thread; `outstanding` still holds the concurrency at
/// [`MAX_PARALLEL_DOWNLOADS`], so this is bounded parallelism rather than an
/// unbounded fan-out, and the reads and the renders stop starving each other.
///
/// **Measured, 2026-08-31, and it was real.** `Runtime::drop` joins the IO
/// thread, whose `tokio::runtime::Runtime` drop waits for blocking tasks that
/// have already started, so dropping a source mid-tessellation blocks whatever
/// thread dropped it. Against the committed Monaco fixture, release build, a
/// nine-tile z14 request block: **up to 13.1 ms**, peaking when the drop lands
/// 1-3 ms after the requests (renders started, none finished) and falling to
/// ~1.9 ms once they have drained. A source with no IO thread
/// (`runtime::inert`) drops in 0.034 ms. The worst case reachable through the
/// restyle path, where six renders start at once with no read latency in
/// front of them, measured 9.2 ms.
///
/// This note used to name "a layer release" as one of the moments that could
/// still hit it. **It did, on the frame thread**: `MapTileState`'s release now
/// parks the source instead of dropping it, so the moments left are a suspend
/// and a graphics reset, both of which already stop drawing. The wasm32 arm
/// never reaches `spawn_blocking` at all.
/// This tile's remembered body, when the target keeps one.
///
/// The whole `cfg` split for the body cache lives in this function and
/// [`remember_body`], and is deliberately a **value** selection: the native
/// arm answers `None` unconditionally, so [`read_one`] below has one body on
/// both targets rather than a `cfg` forking its control flow.
#[cfg(target_arch = "wasm32")]
fn remembered_body(cache: &IoTileBodies, tile_id: TileId) -> Option<Arc<Vec<u8>>> {
    let cache = cache.as_ref()?;
    cache
        .lock()
        .expect("the tile-body cache is not poisoned")
        .get(&tile_id)
        .cloned()
}

/// See the wasm32 arm above. Native restyles from [`SharedParsedTiles`], so
/// there is no second population to keep.
#[cfg(not(target_arch = "wasm32"))]
fn remembered_body(_cache: &IoTileBodies, _tile_id: TileId) -> Option<Arc<Vec<u8>>> {
    None
}

/// Remember this tile's body for a later restyle. See [`remembered_body`].
#[cfg(target_arch = "wasm32")]
fn remember_body(cache: &IoTileBodies, tile_id: TileId, bytes: &Arc<Vec<u8>>) {
    let Some(cache) = cache else {
        return;
    };
    // Charged at the body's own length; what the bound lets go of is one
    // `Vec`, freed here on the page thread as an O(1) free. The frame-paced
    // trim is `HttpsTiles::pump`'s.
    let mut evicted = Vec::new();
    cache
        .lock()
        .expect("the tile-body cache is not poisoned")
        .put(tile_id, Arc::clone(bytes), bytes.len() as u64, &mut evicted);
}

/// See the wasm32 arm above.
#[cfg(not(target_arch = "wasm32"))]
fn remember_body(_cache: &IoTileBodies, _tile_id: TileId, _bytes: &Arc<Vec<u8>>) {}

/// This tile's undecoded body: from the body cache where the target keeps
/// one, and from the archive otherwise, the archive's answer remembered on
/// the way out.
///
/// `Ok(None)` is the archive positively holding nothing there, unchanged.
async fn body_of<S, O>(
    archive: &crate::basemap_archive::BasemapArchives<S, O>,
    cache: &IoTileBodies,
    tile_id: TileId,
) -> Result<Option<Arc<Vec<u8>>>, String>
where
    S: crate::basemap_archive::ArchiveRangeSource,
    O: crate::basemap_archive::ArchiveRangeSource,
{
    if let Some(bytes) = remembered_body(cache, tile_id) {
        return Ok(Some(bytes));
    }
    let bytes = archive
        .tile(tile_id.zoom, tile_id.x, tile_id.y)
        .await
        .map_err(|error| format!("reading {tile_id:?} from the basemap archive: {error}"))?;
    let Some(bytes) = bytes.into_bytes() else {
        log::trace!("the basemap archive holds no tile at {tile_id:?}");
        return Ok(None);
    };
    let bytes = Arc::new(bytes);
    remember_body(cache, tile_id, &bytes);
    Ok(Some(bytes))
}

/// [`read_body`], with the id kept beside the outcome, so that "no tile
/// there" and a failed read are delivered under the id they were for — see
/// [`Fetched`].
async fn read_one<S, O>(
    archive: &crate::basemap_archive::BasemapArchives<S, O>,
    styling: &ArchiveStyling,
    egui_ctx: &Context,
    kind: ArchiveTileKind,
    tile_id: TileId,
) -> (TileId, Result<Option<FetchPayload>, String>)
where
    S: crate::basemap_archive::ArchiveRangeSource,
    O: crate::basemap_archive::ArchiveRangeSource,
{
    (
        tile_id,
        read_body(archive, styling, egui_ctx, kind, tile_id).await,
    )
}

/// The body of [`read_one`], which keeps the id beside this outcome so that
/// "no tile there" and a failed read are delivered under the id they were
/// for — see [`Fetched`].
async fn read_body<S, O>(
    archive: &crate::basemap_archive::BasemapArchives<S, O>,
    styling: &ArchiveStyling,
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            unused_variables,
            reason = "on wasm the frame pump tessellates; see FetchPayload"
        )
    )]
    egui_ctx: &Context,
    kind: ArchiveTileKind,
    tile_id: TileId,
) -> Result<Option<FetchPayload>, String>
where
    S: crate::basemap_archive::ArchiveRangeSource,
    O: crate::basemap_archive::ArchiveRangeSource,
{
    // The restyle path: parsed geometry already held means the archive — and
    // the network and disk behind it — is not consulted at all. This is what
    // `HttpsTiles::set_style` turns a theme flip and a detail toggle into.
    let remembered = match (&styling.parsed_tiles, kind) {
        (Some(parsed_tiles), ArchiveTileKind::Vector) => parsed_tiles
            .lock()
            .expect("the parsed-tile cache is not poisoned")
            .get(&tile_id)
            .cloned(),
        _ => None,
    };
    if let Some(parsed) = remembered {
        #[cfg(not(target_arch = "wasm32"))]
        let payload = {
            let (style, epoch, feathering) = current_style(&styling.style);
            tokio::task::spawn_blocking(move || {
                slot_for(
                    styled_tile(&parsed, &style, tile_id.zoom),
                    epoch,
                    feathering,
                )
            })
            .await
            .map_err(|error| format!("re-styling {tile_id:?} from the parsed cache: {error}"))?
        };

        // On wasm the styling belongs to the frame pump, under its budget.
        #[cfg(target_arch = "wasm32")]
        let payload = FetchPayload::Parsed(parsed);

        return Ok(Some(payload));
    }

    let Some(bytes) = body_of(archive, &styling.tile_bodies, tile_id).await? else {
        return Ok(None);
    };

    #[cfg(not(target_arch = "wasm32"))]
    let payload = {
        let (style, epoch, feathering) = current_style(&styling.style);
        let egui_ctx = egui_ctx.clone();
        let parsed_tiles = styling.parsed_tiles.clone();
        let role = styling.role;

        tokio::task::spawn_blocking(move || {
            match &parsed_tiles {
                Some(parsed_tiles) => decode_archive_tile_remembering(
                    &bytes,
                    kind,
                    &style,
                    tile_id,
                    &egui_ctx,
                    parsed_tiles,
                    role,
                ),
                // Nothing to remember for: a raster archive decodes and
                // uploads, and keeps no parse.
                None => decode_archive_tile(&bytes, kind, &style, tile_id.zoom, &egui_ctx),
            }
            .map(|tile| slot_for(tile, epoch, feathering))
        })
        .await
        .map_err(|error| format!("rendering {tile_id:?} from the basemap archive: {error}"))?
        .map_err(|error| format!("rendering {tile_id:?} from the basemap archive: {error}"))?
    };

    // On wasm the tessellation belongs to the frame pump, under its budget.
    #[cfg(target_arch = "wasm32")]
    let payload = FetchPayload::Bytes(bytes);

    Ok(Some(payload))
}

/// The style the IO task must render against right now, with the restyle
/// generation it belongs to and the feathering its strokes flatten at — read
/// together under one lock so a styling can never be stamped with another
/// generation's number.
#[cfg(not(target_arch = "wasm32"))]
fn current_style(slot: &std::sync::RwLock<StyleSlot>) -> (Arc<Style>, u64, f32) {
    let slot = slot.read().expect("the style slot is not poisoned");
    (Arc::clone(&slot.style), slot.epoch, slot.feathering)
}

/// What one [`HttpsTiles::try_request`] did with a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    /// A request went out.
    Sent,
    /// The request channel was full; nothing was recorded and the cell is
    /// owed a retry.
    Refused,
    /// Nothing to send: cached at the current generation, a request already
    /// out, or a source that has stopped fetching.
    Unneeded,
}

/// The working set the passes drawing one source measure, pass by pass.
///
/// `note` accumulates one pass's cells across every pane drawing the source
/// — the same source is drawn once per pane that shows it — and rolls the
/// total over when the pass number moves. The floor the cache is told is the
/// larger of the completed pass and the pass under way, so a window that
/// grows is held from the pass that grows it and a window that shrinks lets
/// go one pass later; the levels the ledger reports are the completed pass's,
/// which are stable for the whole of the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WantedTally {
    /// The pass being accumulated, once any has been.
    pass_nr: Option<u64>,
    /// This pass so far: cells at the drawn level, cells of the ancestor net.
    running: (usize, usize),
    /// The last whole pass.
    completed: (usize, usize),
}

impl WantedTally {
    /// Add one layer draw's cells to `pass_nr`, answering whether this call
    /// began a new pass.
    fn note(&mut self, pass_nr: u64, on_glass: usize, net: usize) -> bool {
        let new_pass = self.pass_nr != Some(pass_nr);
        if new_pass {
            if self.pass_nr.is_some() {
                self.completed = self.running;
            }
            self.pass_nr = Some(pass_nr);
            self.running = (0, 0);
        }
        self.running.0 = self.running.0.saturating_add(on_glass);
        self.running.1 = self.running.1.saturating_add(net);
        new_pass
    }

    /// The most cells either the last whole pass or this one has wanted.
    fn floor(&self) -> usize {
        let running = self.running.0.saturating_add(self.running.1);
        let completed = self.completed.0.saturating_add(self.completed.1);
        running.max(completed)
    }

    /// The last whole pass's `(on_glass, net)`.
    fn completed(&self) -> (usize, usize) {
        self.completed
    }
}

/// The most refused asks one source remembers. A 3840x2160 canvas at one
/// zoom of bias keeps 1,196 tiles between zooms plus its net; a pass can
/// refuse at most that many, and a cell refused past this bound is simply
/// refused again next pass, exactly as every refused cell was before the
/// queue existed.
const MAX_REFUSED_ASKS: usize = 4096;

/// The cells the request channel refused, in the order it refused them.
///
/// **The tail-starvation fix, and why it is about order and not depth.** The
/// channel holds [`MAX_PARALLEL_DOWNLOADS`] plus its sender's slot, and a
/// pass walks a hundred and more cells in a fixed order; with the cache
/// evicting the walk's head each pass — which the working-set floor now
/// forbids, but which a floor one pass behind a resize, or a restyle re-asking
/// every cell, can still produce for a pass or two — the head took every slot
/// every pass and cells at the tail were never asked at all: 10-20 of 144
/// over 120 frames on the loopback pin (2026-09-02). Widening the channel
/// would move the tail, not remove it; changing the walk order would change
/// the order labels are collected in, and so which label wins a collision.
/// So the *ask* order rotates while the walk order stands: every cell the
/// channel refused is queued here once, in walk order, and the next pass asks
/// for the queue's head before its own walk begins. A cell reaches the head
/// within `ceil(working set / channel depth)` passes, whatever the walk does.
///
/// **What leaves the queue.** A cell arrives, is found in flight or cached, or
/// is refused again (and goes back to the front). A cell the walk stopped
/// asking for — it scrolled off — is purged at the next pass boundary rather
/// than fetched for nothing: `wanted_now` records what this pass's walk
/// asked, and `new_pass` keeps only the queued cells the pass before still
/// wanted.
#[derive(Debug, Default)]
struct AskQueue {
    refused: std::collections::VecDeque<TileId>,
    queued: HashSet<TileId>,
    wanted_now: HashSet<TileId>,
    wanted_before: HashSet<TileId>,
}

impl AskQueue {
    /// The walk asked for `tile_id` this pass.
    fn wanted(&mut self, tile_id: TileId) {
        self.wanted_now.insert(tile_id);
    }

    /// The channel refused `tile_id`; queue it once, at the back.
    fn refuse(&mut self, tile_id: TileId) {
        if self.refused.len() >= MAX_REFUSED_ASKS || !self.queued.insert(tile_id) {
            return;
        }
        self.refused.push_back(tile_id);
    }

    /// The oldest refused cell, taken.
    fn pop_front(&mut self) -> Option<TileId> {
        let tile_id = self.refused.pop_front()?;
        self.queued.remove(&tile_id);
        Some(tile_id)
    }

    /// Put a cell back at the front: refused again, and still the oldest.
    fn push_front(&mut self, tile_id: TileId) {
        if self.queued.insert(tile_id) {
            self.refused.push_front(tile_id);
        }
    }

    /// A pass began: what the last pass wanted becomes the filter, and the
    /// queue keeps only cells it still wanted.
    fn new_pass(&mut self) {
        self.wanted_before = std::mem::take(&mut self.wanted_now);
        let wanted = &self.wanted_before;
        self.refused.retain(|tile_id| wanted.contains(tile_id));
        self.queued.retain(|tile_id| wanted.contains(tile_id));
    }

    fn clear(&mut self) {
        self.refused.clear();
        self.queued.clear();
        self.wanted_now.clear();
        self.wanted_before.clear();
    }

    fn len(&self) -> usize {
        self.refused.len()
    }
}

/// The byte budgets one source is built with: its styled (or, for a raster
/// source, its texture) cache, and its parsed-geometry cache where it has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceBudget {
    /// The population the source's [`TileCache`] holds.
    pub(crate) styled_bytes: u64,
    /// `None` builds no parsed cache: a raster archive has nothing
    /// style-independent to keep.
    pub(crate) parsed_bytes: Option<u64>,
}

/// The tile allowances this build starts on before the application has
/// pushed the resolved ones: the compile-time bracket's floor, resolved by the
/// same function the application uses — the `concurrent_renders` precedent,
/// here without a `cfg` constant to name, because the tile allowances have no
/// cascade.
pub(crate) fn default_tile_budget() -> squallar_device_profile::budget::TileCacheBudget {
    squallar_device_profile::budget::resolve(
        &squallar_device_profile::budget::DeviceProfile::for_target(),
    )
    .tile_cache()
}

/// Where an evicted cache payload goes to be freed.
///
/// **Installed rather than depended on**, exactly as [`TileOffloader`] is:
/// this crate must not name `squallar-worker`, whose `offload::discard` is
/// the frame thread's route for a large drop (the pool's free lane on native,
/// the frame-paced deferred queue on wasm32), so the application installs the
/// function once at startup and every source on every thread reaches it here.
/// A `fn` pointer and a `OnceLock`: nothing to lock per eviction, nothing to
/// clear, and a build that installs none — every test, the harness — drops
/// inline, which is what the code did before the sink existed.
static TILE_DISCARD: OnceLock<TileDiscard> = OnceLock::new();

/// The shape of the discard sink: a name for the ledger the payload is filed
/// under, and the payload, boxed — `squallar_worker::offload::discard`'s own
/// signature with the generic payload already erased.
pub type TileDiscard = fn(&'static str, Box<dyn std::any::Any + Send>);

/// Install the sink evicted tile payloads are freed through. The first call
/// wins; later ones are ignored, so a second `App` in one process changes
/// nothing.
pub fn set_tile_discard(sink: TileDiscard) {
    let _ = TILE_DISCARD.set(sink);
}

/// Free `payload` through the installed sink, or here when none is.
fn discard_slot(name: &'static str, payload: impl Send + 'static) {
    match TILE_DISCARD.get() {
        Some(sink) => sink(name, Box::new(payload)),
        None => drop(payload),
    }
}

pub mod byte_lru;
pub mod cache_ledger;
pub mod snap;
pub mod take_ledger;

// Native-only: `#[tokio::test]` (the dev-dependency is target-gated),
// `ClientBuilder::timeout` and `Error::is_connect`, which reqwest's wasm arm
// does not have, and `squallar_radar::tls::default_is_ring`, itself
// `cfg(not(wasm32))`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
