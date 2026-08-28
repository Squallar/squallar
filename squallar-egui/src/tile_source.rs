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
//! [`walkers::Tile::new`], a bounded LRU of [`TILE_CACHE_ENTRIES`] tiles,
//! in-flight de-duplication, lower-zoom interpolation, `max_zoom` clamping,
//! grid-bounds checking, repaint-on-arrival and attribution.
//!
//! Deliberate differences: our client with a [`REQUEST_TIMEOUT`] (walkers sets
//! none); squallar's `User-Agent`; a closed request channel logs instead of
//! panicking (walkers' `TilesIo::make_sure_is_fetched` calls `panic!`); no HTTP
//! disk cache, which neither has.
//!
//! The IO runtime splits per target as walkers splits it: a thread with a
//! current-thread tokio runtime on native, `spawn_local` on wasm. On wasm the
//! IO task shares the page thread, so a completed fetch hands over **compressed
//! bytes** and the decode + upload runs in [`HttpsTiles::pump`] under
//! [`WASM_TILE_DECODES_PER_PUMP`] — see [`FetchPayload`].
//!
//! The pump is called **once per layer**, by `ui_map_overlays::draw_tile_layer`
//! before its grid loop, and never from [`Tiles::at`] — see
//! [`HttpsTiles::pump`] for why, and for the one thing that would break.
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use egui::Context;
use futures::channel::mpsc::{Receiver, Sender, TryRecvError, TrySendError, channel};
use futures::stream::FuturesUnordered;
use futures::{SinkExt, StreamExt, future::Either, future::select};
use lru::LruCache;
use walkers::sources::{Attribution, TileSource};
use walkers::{Style, Tile, TileId, TilePiece, Tiles};

/// Maximum number of tile downloads in flight at once — walkers' default.
/// Tile providers throttle or ban clients that exceed their limits, so this is
/// a term of use rather than a performance dial.
pub const MAX_PARALLEL_DOWNLOADS: usize = 6;

/// Tiles retained in one source's in-memory cache before LRU eviction starts.
///
/// **Two kinds of entry, and they do not cost the same.** This bound is a
/// *count*, so what it is worth in bytes depends on what the arm can hold:
///
/// | entry            | cost               | which arms hold it |
/// |------------------|-------------------:|--------------------|
/// | raster texture   | 256 KiB (256x256 RGBA) | all |
/// | vector tile      | **646,264 B** worst case | all |
///
/// The vector figure is measured, not derived: the committed Monaco fixture's
/// z14 city-core tile (185,182 MVT bytes) renders to 738 shapes — 708 paths of
/// 4,390 points, two coalesced meshes of 18,018 vertices and 40,812 indices,
/// and 27 labels — held as host allocations, not GPU textures. Over all 246
/// tiles the fixture holds the *mean* is 13,461 bytes and the median 456, so
/// this is a tail figure and the tail is what a cache must be sized for: a
/// viewport over a city is a viewport of city-core tiles.
///
/// **wasm32 holds vector entries too**, since `feat(web): the vector basemap
/// draws on wasm32` ungated the archive reader; the arm used to be derived
/// against the raster cost alone because there was nothing else it could hold.
/// The 646,264 B figure carries to it unmeasured but not unfounded: `mvt::render`
/// is the same code over the same fixture, and a `Tile` is host allocations on
/// every target. It is a native measurement applied to wasm32, and it is
/// recorded that way rather than restated as a wasm one.
///
/// | tier    | entries | worst case, per source        |
/// |---------|--------:|------------------------------:|
/// | desktop |     256 | ~158 MiB vector, 64 MiB raster |
/// | mobile  |      96 | ~59 MiB vector, 24 MiB raster |
/// | wasm32  |      96 | ~59 MiB vector, 24 MiB raster |
///
/// The wasm32 arm is the one where that promotion has a budget to answer to:
/// `squallar-device-profile`'s `constants::WASM_APP_TEXTURE_BUDGET_BYTES` is
/// 288 MiB for the whole application, and `MapTileState::adopt_theme` keeps one
/// theme's sources live, so base plus labels is ~118 MiB of it worst case where
/// the raster derivation said 48. It fits, with less room than the old figure
/// implied. The count did not move, because 96 is also the working-set floor
/// below.
///
/// **Against the meshes this workspace shipped until 2026-08-28 those figures
/// were 8.0 GiB and 4.0 GiB.** `VertexBuffers::new()` is
/// `with_capacity(512, 1024)` and the tile held 2,257 of them, so one entry was
/// 32.77 MB rather than 0.65 MB. Neither number was ever reached, because a
/// machine would have died first; they are quoted because they are what this
/// comment's old derivation — "a tile texture costs the same 256 KiB" — was
/// silently claiming, 50x under the truth on a cost it was not measuring at all.
///
/// **The mobile arm fell from 128 to 96.** The desktop arm did *not* fall, and
/// what stopped it is worth naming rather than leaving as an unexplained round
/// number: it is pinned from below by
/// `squallar_gpu::egui_renderer::mirror`'s `MIRROR_SCALE_MAX`. That cap is only
/// legitimate if a floor strip could actually take the rung, and
/// `mirror::tests::the_rung_above_the_cap_could_never_fit_the_tile_cache`
/// holds it so: a 900-point pane at two layers keeps 242 tiles resident at
/// `tile_zoom_bias = 1`, and 72 at bias 0 against a "three times over" margin.
/// So this arm may not go below 242 without moving the mirror's deepest rung,
/// which is a different subsystem's decision. What actually fixed desktop
/// residency was the 50.7x per-*entry* reduction, not the count.
///
/// **The honest remaining gap**: this is still a count, and a count cannot
/// express "158 MiB" as a limit when entries range from 456 bytes to 646,264.
/// A byte-budgeted LRU is the answer that would; it is not this change.
///
/// "Per source" is the multiplier: each map source owns one of these caches —
/// base and labels, light and dark — so the old desktop figure on wasm32 could
/// put 256 MiB of basemap texture against the 288 MiB
/// `squallar-device-profile`'s `constants::WASM_APP_TEXTURE_BUDGET_BYTES`
/// allows the whole application. The tiers follow that crate's budget cascade,
/// spelled rather than imported.
///
/// **Every arm is still above its working set, which is the floor no budget may
/// cross.** See below, and `tiles::tests`'
/// `the_cache_holds_the_working_set_at_every_zoom`. Mobile's 96 covers the 63 a
/// 1024x1366-point tablet keeps resident — the largest handheld canvas in
/// common use — where 128 covered nothing extra that a handheld can present.
///
/// **Every arm is sized against the worst case over the whole zoom range**, not
/// against a whole zoom. A tile is drawn `256 · 2^(zoom − round(zoom))` points
/// across, down to 181 at the half step, so between two whole zooms more tiles
/// fit the same window than at either end of it: 84 per source over a
/// 1920x1080-point canvas at one layer, against 54 at a whole zoom.
/// [`crate::tiles::tiles_resident_for`] reports that larger figure and
/// `tiles::tests` holds it equal to what the `tile_span` sweep measures.
///
/// What each arm covers, at one layer, in **points** — a 4K panel at the 2x
/// scaling it is nearly always run at presents 1920x1080 points, not 3840x2160:
///
/// | canvas    | tiles | wasm 96 | mobile 96 | desktop 256 |
/// |-----------|------:|---------|-----------|-------------|
/// | 1024x1366 |    63 | fits    | fits      | fits        |
/// | 1920x1080 |    84 | fits    | fits      | fits        |
/// | 1920x1200 |    96 | fits    | fits      | fits        |
/// | 2560x1440 |   144 | overruns| overruns  | fits        |
/// | 3840x2160 |   299 | overruns| overruns  | overruns    |
///
/// The wasm arm is 96 because that is 1920x1200 — the tallest panel in common
/// use at 1920 wide — and it carries 1080p's 84 with room rather than sitting
/// exactly on it. It costs 24 MiB per source, and the app can hold four live
/// sources at once (base and labels, light and dark; a theme flip retains
/// both), so 96 MiB against the 288 MiB budget: a third of it.
///
/// An LRU below the working set is not a slower cache, it is a broken one. It
/// evicts a tile that is still on the glass, the next frame re-enters
/// `request_once` for it, and that tile is fetched over the network again and
/// re-decoded against [`WASM_TILE_DECODES_PER_PUMP`] — for something the user
/// never stopped looking at. `tiles::tests`'
/// `the_cache_holds_the_working_set_at_every_zoom` is the gate.
#[cfg(target_arch = "wasm32")]
pub const TILE_CACHE_ENTRIES: NonZeroUsize = WASM_TILE_CACHE_ENTRIES;
/// See the wasm32 arm above.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "android", target_os = "ios")
))]
pub const TILE_CACHE_ENTRIES: NonZeroUsize = MOBILE_TILE_CACHE_ENTRIES;
/// See the wasm32 arm above.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub const TILE_CACHE_ENTRIES: NonZeroUsize = DESKTOP_TILE_CACHE_ENTRIES;

/// The wasm32 arm of [`TILE_CACHE_ENTRIES`]. All three arms are named outside
/// the cascade because this workspace runs `cargo test` on one arm, so the other
/// two are only reachable from a test if they have names.
pub const WASM_TILE_CACHE_ENTRIES: NonZeroUsize = NonZeroUsize::new(96).expect("96 is not zero");
/// The mobile arm — the largest handheld working set, not a fraction of the
/// desktop figure. See [`WASM_TILE_CACHE_ENTRIES`].
pub const MOBILE_TILE_CACHE_ENTRIES: NonZeroUsize = NonZeroUsize::new(96).expect("96 is not zero");
/// The desktop arm — pinned from below at 242 by `squallar_gpu`'s
/// `MIRROR_SCALE_MAX`, not by the byte budget. See [`WASM_TILE_CACHE_ENTRIES`].
pub const DESKTOP_TILE_CACHE_ENTRIES: NonZeroUsize =
    NonZeroUsize::new(256).expect("256 is not zero");

/// The measured worst-case heap of one cached vector tile, in bytes.
///
/// The committed Monaco fixture's z14 city-core tile, 2026-08-28: shape spine,
/// path points, mesh vertices and indices and label strings, counted at
/// **capacity** rather than length because capacity is what is resident. The
/// derivation of [`TILE_CACHE_ENTRIES`] is against this figure; it is named so
/// a test can hold the two together instead of the number living only in prose.
///
/// **Re-measured the same day, 646_264 to 652_112 — +5,848 bytes, +0.9%**, when
/// `walkers::Text` grew the fields carrying a label's wrapping. It is a
/// re-measurement and not a relaxation:
/// `tile_source::tests::the_vector_entry_cost_is_what_the_fixture_actually_renders`
/// re-derives it rather than trusting this line. The budget it feeds is
/// unchanged in shape — 96 entries is 62.6 MB where it was 62.0 MB, and the
/// desktop arm is pinned from below by `MIRROR_SCALE_MAX` rather than by bytes,
/// so no arm's entry count moves.
///
/// **Removing the halo later the same day did NOT move it back, and an earlier
/// version of this comment predicted that it would.** That prediction came from
/// attributing the growth to `ShapeOrText` widening by 8 bytes per shape over
/// 731 shapes, which is arithmetic that lands on 5,848 exactly and is still
/// wrong: measured, `size_of::<ShapeOrText>()` is **72 both with and without the
/// halo fields**, because `Text` is 72 with them and 64 without, and the other
/// variant (`egui::Shape`) is 64 either way. The enum never changed size, so the
/// halo was never costing the spine anything. The figure below was re-derived
/// after the removal, with a forced rebuild, and is unchanged.
///
/// The lesson worth keeping is about the test rather than the number: it asserts
/// a *band* (`heap <= CONST <= 2 * heap`) and deliberately not equality, because
/// `size_of` and allocator rounding are toolchain properties. So it cannot catch
/// this constant drifting upward into a safe over-estimate. Re-derive it by
/// forcing the assertion to fail; do not infer it from a type's field list.
pub const MEASURED_VECTOR_TILE_BYTES: usize = 652_112;

/// What one cached raster tile costs: 256x256 RGBA.
pub const RASTER_TILE_BYTES: usize = 256 * 256 * 4;

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

/// Per-tile request timeout.
///
/// walkers sets none. A tile that never answers would otherwise hold one of the
/// [`MAX_PARALLEL_DOWNLOADS`] slots for as long as the process lives.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Tile decodes the frame side performs per **source** per **pass**, wasm32
/// only.
///
/// On native the IO thread decodes and uploads off the frame thread. On wasm
/// `spawn_local` runs the fetch loop on the page itself, so before this bound a
/// pan exposing a fresh row of tiles paid up to [`MAX_PARALLEL_DOWNLOADS`]
/// decode+upload rounds *per live source* on the very thread the gesture runs on.
///
/// Every [`HttpsTiles`] owns its own [`DecodeBudget`] and the standard
/// configuration drives two live sources a pass, so a typical frame gets **4**
/// rounds, not 2; a second pass doubles it again, since the budget keys on
/// `Context::cumulative_pass_nr`. Against the pre-fix 12 across base + labels
/// that is still a 3× cut, and it keeps each source filling at ~120 tiles a
/// second at 60 fps. [`HttpsTiles::pump`] requests a repaint whenever it uses
/// its whole allowance, so a backlog drains over idle frames.
///
/// Deliberately **capped inline, not offloaded**: tile PNGs behind the overlay
/// worker's job round-trip would trade a bounded per-pass cost for seconds of
/// blank basemap.
pub const WASM_TILE_DECODES_PER_PUMP: usize = 2;

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
/// Native: the decoded [`Tile`] — [`fetch_one`] and [`read_one`] decode and
/// upload on the IO thread. wasm32: the tile body undecoded — a compressed PNG
/// from a raster source, an MVT body from the archive — turned into a [`Tile`]
/// by the frame pump at most [`WASM_TILE_DECODES_PER_PUMP`] per source per pass
/// — see [`HttpsTiles::pump`].
#[cfg(not(target_arch = "wasm32"))]
type FetchPayload = Tile;
#[cfg(target_arch = "wasm32")]
type FetchPayload = Vec<u8>;

/// Managed IO runtime for the tile fetch task.
mod runtime {
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
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) use native::{Runtime, spawn};
    #[cfg(target_arch = "wasm32")]
    pub(super) use web::{Runtime, spawn};
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
fn tile_client() -> reqwest::Client {
    squallar_radar::tls::client(squallar_radar::tls::USER_AGENT, REQUEST_TIMEOUT)
        .build()
        .expect("the tile HTTP client should build")
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

    /// Tiles by id. A `None` value means "asked for, not here yet" *and*
    /// "asked for, and it failed" — the two are deliberately indistinguishable,
    /// because both mean "do not ask again". See [`Self::request_once`].
    cache: LruCache<TileId, Option<Tile>>,

    /// Tiles the IO task should fetch.
    request_tx: Sender<TileId>,
    /// Tiles the IO task has fetched: decoded on native, compressed bytes on
    /// wasm32, where this channel doubles as the decode queue the frame pump
    /// drains under [`WASM_TILE_DECODES_PER_PUMP`].
    tile_rx: Receiver<(TileId, FetchPayload)>,

    /// wasm32 only: the context the frame pump decodes and uploads through,
    /// and asks for the queue-draining repaint on. On native the IO thread
    /// owns the context's clone instead.
    #[cfg(target_arch = "wasm32")]
    egui_ctx: Context,
    /// wasm32 only: what is left of [`WASM_TILE_DECODES_PER_PUMP`] this pass.
    #[cfg(target_arch = "wasm32")]
    decode_budget: DecodeBudget,

    /// wasm32 only: the style [`Tile::new`] renders a vector tile against.
    /// Empty for a raster source, which never reads it; the committed style for
    /// an archive source, where it is the whole appearance of the map. On
    /// native the IO task owns its own clone -- see [`fetch_one`], [`read_one`].
    #[cfg(target_arch = "wasm32")]
    style: Arc<Style>,

    /// Whether "the tile IO task is gone" has already been said for this
    /// source. See [`drain_up_to`]: the condition is permanent, so the line is
    /// not.
    io_task_gone_reported: bool,

    /// [`Self::pump`] calls since this source was built — see [`Self::pumps`].
    /// Always on, like the app's other ledgers: one `u64` add per layer per
    /// pass, against the tens of [`Tiles::at`] calls the same layer makes.
    pumps: u64,

    /// Declared last so it drops last: the channels above must close first, which
    /// is what tells the fetch loop to exit.
    #[expect(dead_code, reason = "owned for its Drop; shuts the IO task down")]
    runtime: runtime::Runtime,
}

/// One source's per-pass decode allowance, wasm32 only.
///
/// [`HttpsTiles::pump`] runs once per layer, and one source is drawn as a layer
/// in every pane that shows it plus the volume floor strip, so a per-*call*
/// bound would still let a multi-pane layout bill one pass several times over.
/// The pass number is what turns it per-pass: the first call of a new pass
/// restores the full allowance. Per *source* because each [`HttpsTiles`] owns
/// one.
///
/// Compiled on native for the tests (`any(test, target_arch = "wasm32")`).
#[cfg(any(test, target_arch = "wasm32"))]
struct DecodeBudget {
    /// The pass [`Self::spent`] counts within.
    pass_nr: u64,
    /// Decodes already performed in [`Self::pass_nr`].
    spent: usize,
}

#[cfg(any(test, target_arch = "wasm32"))]
impl DecodeBudget {
    fn new() -> Self {
        Self {
            pass_nr: 0,
            spent: 0,
        }
    }

    /// Decodes still allowed in `pass_nr`, resetting the count when the pass
    /// has moved on since the last call.
    fn remaining(&mut self, pass_nr: u64) -> usize {
        if pass_nr != self.pass_nr {
            self.pass_nr = pass_nr;
            self.spent = 0;
        }
        WASM_TILE_DECODES_PER_PUMP.saturating_sub(self.spent)
    }

    /// Record `taken` decodes performed against the allowance.
    fn record(&mut self, taken: usize) {
        self.spent += taken;
    }
}

/// Move at most `budget` completed fetches out of `rx`, handing each to `take`.
/// Returns how many were taken. Stops early when the channel is empty; a closed
/// channel is the IO task gone, and is logged rather than panicked on, as
/// everywhere else in this module.
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
fn drain_up_to<T>(
    rx: &mut Receiver<T>,
    budget: usize,
    reported: &mut bool,
    mut take: impl FnMut(T),
) -> usize {
    let mut taken = 0;
    while taken < budget {
        match rx.try_recv() {
            Ok(item) => {
                take(item);
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

    /// [`Self::new`], crediting `attribution` rather than what `source` claims.
    ///
    /// One caller: `tiles::MapTileState::ensure_base_tiles`, falling back to the
    /// rasters after the vector archive reported itself unusable. The provider
    /// is the same and its credit is still owed, but the panel's corner is the
    /// only place a user is told which basemap they are looking at, so a
    /// fallback that credited CartoDB *plainly* would be indistinguishable from
    /// a build configured for CartoDB on purpose.
    pub fn with_attribution<S: AsyncTileSource>(
        source: S,
        egui_ctx: Context,
        attribution: Attribution,
    ) -> Self {
        let mut tiles = Self::with_client(source, egui_ctx, tile_client());
        tiles.attribution = attribution;
        tiles
    }

    /// [`Self::new`], with the HTTP client supplied. Crate-private, for the
    /// tests, which need to talk cleartext to a loopback server — [`tile_client`]
    /// refuses `http://` by design.
    pub(crate) fn with_client<S: AsyncTileSource>(
        source: S,
        egui_ctx: Context,
        client: reqwest::Client,
    ) -> Self {
        Self::with_client_and_cache(source, egui_ctx, client, TILE_CACHE_ENTRIES)
    }

    /// [`Self::with_client`], with the cache bound supplied. Crate-private, for
    /// the tests: [`TILE_CACHE_ENTRIES`] is cfg-selected and this workspace runs
    /// `cargo test` on the desktop arm only.
    fn with_client_and_cache<S: AsyncTileSource>(
        source: S,
        egui_ctx: Context,
        client: reqwest::Client,
        cache_entries: NonZeroUsize,
    ) -> Self {
        let attribution = source.attribution();
        let tile_size = source.tile_size();
        let max_zoom = Arc::new(AtomicU8::new(source.max_zoom()));

        // Sized to the concurrency limit, as walkers sizes them: a full request
        // channel is the backpressure that makes `request_once` retry later.
        let (request_tx, request_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let (tile_tx, tile_rx) = channel(MAX_PARALLEL_DOWNLOADS);

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
            fault: Arc::new(OnceLock::new()),
            requests_closed: false,
            cache: LruCache::new(cache_entries),
            request_tx,
            tile_rx,
            #[cfg(target_arch = "wasm32")]
            egui_ctx: frame_ctx,
            #[cfg(target_arch = "wasm32")]
            decode_budget: DecodeBudget::new(),
            // A raster source. The wasm decode path passes this to `Tile::new`,
            // which only reads a style for a tile that is not an image.
            #[cfg(target_arch = "wasm32")]
            style: Arc::new(Style::default()),
            io_task_gone_reported: false,
            pumps: 0,
            runtime,
        }
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
    pub fn pump(&mut self) {
        self.pumps += 1;
        self.drain_completed_fetches();
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
            ..
        } = self;
        drain_up_to(
            tile_rx,
            NATIVE_TILE_UPLOADS_PER_PUMP,
            io_task_gone_reported,
            |(tile_id, tile): (TileId, Tile)| {
                cache.put(tile_id, Some(tile));
            },
        );
    }

    /// wasm32: decode, upload and cache at most this pass's remaining allowance
    /// of fetched tiles.
    ///
    /// [`DecodeBudget`] keeps a burst of completed fetches from billing one pass
    /// for this source's whole backlog. The put is unconditional, exactly as a
    /// native tile arriving is: a `None` marker still means failed, do not ask
    /// again.
    #[cfg(target_arch = "wasm32")]
    fn drain_completed_fetches(&mut self) {
        let Self {
            cache,
            tile_rx,
            egui_ctx,
            decode_budget,
            style,
            io_task_gone_reported,
            ..
        } = self;
        let style: &Style = style;

        let budget = decode_budget.remaining(egui_ctx.cumulative_pass_nr());
        let taken = drain_up_to(
            tile_rx,
            budget,
            io_task_gone_reported,
            |(tile_id, bytes): (TileId, Vec<u8>)| match Tile::new(
                &bytes,
                style,
                tile_id.zoom,
                egui_ctx,
            ) {
                Ok(tile) => {
                    cache.put(tile_id, Some(tile));
                }
                Err(error) => log::warn!("decoding tile {tile_id:?}: {error}"),
            },
        );
        decode_budget.record(taken);

        if budget > 0 && taken == budget {
            // The whole allowance went, so more completions may be waiting.
            // Ask for a frame so a backlog drains while the user is idle.
            egui_ctx.request_repaint();
        }
    }

    /// Ask for `tile_id` unless it has already been asked for.
    ///
    /// The de-duplication and the cache are the same structure: inserting `None`
    /// under the tile's id records "a request is out" and reserves the slot.
    /// When the request channel is full the closure fails, *nothing is inserted*,
    /// and the tile is retried on a later frame — dropping the request while
    /// marking the tile as requested would strand it forever.
    fn request_once(&mut self, tile_id: TileId) {
        if self.requests_closed {
            return;
        }

        // Split borrow: the closure needs the sender while the cache is borrowed.
        let Self {
            cache, request_tx, ..
        } = self;

        let outcome =
            cache.try_get_or_insert(tile_id, || -> Result<Option<Tile>, TrySendError<TileId>> {
                request_tx.try_send(tile_id)?;
                log::trace!("requested tile {tile_id:?}");
                Ok(None)
            });

        match outcome {
            Ok(_) => {}
            Err(error) if error.is_full() => {
                log::trace!("tile request queue is full, retrying {tile_id:?} next frame");
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
            }
        }
    }

    /// The tile itself, or the nearest cached ancestor stretched to fit.
    ///
    /// Starts at the requested zoom and walks outwards until something is cached,
    /// so a zoomed-in view shows a blurry ancestor rather than a hole. Starts no
    /// download of its own.
    fn cached_or_interpolated(&mut self, tile_id: TileId) -> Option<TilePiece> {
        let mut zoom_candidate = tile_id.zoom;

        loop {
            let (ancestor, uv) = interpolate_from_lower_zoom(tile_id, zoom_candidate);

            if let Some(Some(cached)) = self.cache.get(&ancestor) {
                break Some(TilePiece::new(cached.clone(), uv));
            }

            // Out of ancestors: nothing to draw for this tile yet.
            zoom_candidate = zoom_candidate.checked_sub(1)?;
        }
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

    /// Tiles currently held, including pending and failed markers. Exposed for
    /// the eviction test; gated off wasm32 with `mod tests`, its only caller.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn cached_entries(&self) -> usize {
        self.cache.len()
    }

    /// Whether `tile_id` currently occupies a slot, pending and failed markers
    /// included. A peek, not a use: `LruCache::contains` leaves the recency
    /// order alone. Test-gated like [`Self::cached_entries`].
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn tile_is_cached(&self, tile_id: TileId) -> bool {
        self.cache.contains(&tile_id)
    }

    /// Put `tile` in the cache under `tile_id`, as an arrived fetch would.
    ///
    /// Test-only, and the reason it exists is coverage rather than convenience:
    /// the vector draw seam has to be reachable from a test that runs in a
    /// **default** `cargo test --workspace`. `walkers/mvt` is on
    /// unconditionally, so `Tile::Vector` exists on every build, but the only
    /// thing that *produces* one is the archive, and the archive is behind
    /// `basemap-vector`. Without this the seam's dispatch would be tested only
    /// by a CI row that has to be remembered.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn put_for_test(&mut self, tile_id: TileId, tile: Tile) {
        self.cache.put(tile_id, Some(tile));
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
        if !tile_id_is_valid(tile_id) {
            return None;
        }

        // An archive source that has not read its header yet does not know what
        // it can serve. Asking anyway means asking for `0/0/0`, which is a real
        // tile that then never leaves the cache; see [`MAX_ZOOM_UNKNOWN`]. The
        // IO task repaints when the header lands, so this waits one frame.
        let max_zoom = self.source_max_zoom()?;

        // Above the source's deepest zoom there is nothing to download; the
        // ancestor at `max_zoom` is what gets stretched over the gap.
        let to_fetch = if tile_id.zoom > max_zoom {
            interpolate_from_lower_zoom(tile_id, max_zoom).0
        } else {
            tile_id
        };

        self.request_once(to_fetch);
        self.cached_or_interpolated(tile_id)
    }

    fn tile_size(&self) -> u32 {
        self.tile_size
    }
}

// ---------------------------------------------------------------------------
// The IO task
// ---------------------------------------------------------------------------

/// Download one tile — and on native, decode and upload it too.
///
/// On native, decoding happens here on the IO runtime, as walkers does it:
/// [`Tile::new`] performs the PNG decode and the
/// [`egui::Context::load_texture`] call, and `Context` is `Send + Sync` and
/// locks internally. On wasm32 the IO "runtime" *is* the UI thread, so the bytes
/// are handed over undecoded — see [`FetchPayload`].
///
/// The error is a `String` because walkers' `TileError` is not exported.
async fn fetch_one<S: TileSource>(
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
) -> Result<(TileId, FetchPayload), String> {
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
    // only consults a style for a body that is not a recognised image.
    #[cfg(not(target_arch = "wasm32"))]
    let payload = Tile::new(&body, &Style::default(), tile_id.zoom, egui_ctx)
        .map_err(|error| format!("decoding '{url}': {error}"))?;

    // On wasm the decode belongs to the frame pump, under its budget.
    #[cfg(target_arch = "wasm32")]
    let payload = body.to_vec();

    Ok((tile_id, payload))
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
    mut tile_tx: Sender<(TileId, FetchPayload)>,
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
        let Some(result) = completed else { continue };

        match result {
            Ok(fetched) => {
                if tile_tx.send(fetched).await.is_err() {
                    break;
                }
                // Without this the tile sits in the channel until some unrelated
                // input wakes the UI, and the map appears to stop loading.
                egui_ctx.request_repaint();
            }
            Err(error) => log::warn!("{error}"),
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
#[cfg(feature = "basemap-vector")]
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
    pub fn from_archive_url(
        url: &str,
        style: Arc<Style>,
        attribution: Attribution,
        egui_ctx: Context,
    ) -> Result<Self, crate::basemap_archive::RangeError> {
        use crate::basemap_archive::{HttpRangeSource, archive_client};

        let source = HttpRangeSource::new(archive_client(), url)?;
        Ok(Self::from_range_source(
            source,
            style,
            attribution,
            egui_ctx,
            TILE_CACHE_ENTRIES,
        ))
    }

    /// [`Self::from_archive_url`] with the range source and the cache bound
    /// supplied. Crate-private, for the tests, which read the committed Monaco
    /// fixture off disk rather than over a network.
    pub(crate) fn from_range_source<S>(
        source: S,
        style: Arc<Style>,
        attribution: Attribution,
        egui_ctx: Context,
        cache_entries: NonZeroUsize,
    ) -> Self
    where
        S: crate::basemap_archive::ArchiveRangeSource + 'static,
    {
        let (request_tx, request_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let (tile_tx, tile_rx) = channel(MAX_PARALLEL_DOWNLOADS);

        let max_zoom = Arc::new(AtomicU8::new(MAX_ZOOM_UNKNOWN));
        let fault: Arc<OnceLock<String>> = Arc::new(OnceLock::new());

        // Both clones exist for the reason `with_client_and_cache` clones the
        // context: on wasm32 the frame pump is the tessellating side, so it
        // needs the context to upload through and the style to render against.
        // The IO task is handed its own pair either way -- `read_one` takes
        // both and ignores them on the target that does not decode there.
        #[cfg(target_arch = "wasm32")]
        let frame_ctx = egui_ctx.clone();
        #[cfg(target_arch = "wasm32")]
        let frame_style = Arc::clone(&style);

        let runtime = runtime::spawn(serve_archive_continuously(
            source,
            style,
            Arc::clone(&max_zoom),
            Arc::clone(&fault),
            request_rx,
            tile_tx,
            egui_ctx,
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
            requests_closed: false,
            cache: LruCache::new(cache_entries),
            request_tx,
            tile_rx,
            #[cfg(target_arch = "wasm32")]
            egui_ctx: frame_ctx,
            #[cfg(target_arch = "wasm32")]
            decode_budget: DecodeBudget::new(),
            // NOT the raster path's `Style::default()`: this is the committed
            // style, and it is what `Tile::new` renders the MVT body against.
            // An empty one would hand back a blank tile for every road and
            // every label.
            #[cfg(target_arch = "wasm32")]
            style: frame_style,
            io_task_gone_reported: false,
            pumps: 0,
            runtime,
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
/// neither target is it this task. `Tile::new` falls through to `mvt::render`
/// for a body no image decoder recognises, and that is the whole per-tile cost
/// of a vector basemap. On native [`read_one`] hands it to the runtime's
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
#[cfg(feature = "basemap-vector")]
async fn serve_archive_continuously<S>(
    source: S,
    style: Arc<Style>,
    max_zoom: Arc<AtomicU8>,
    fault: Arc<OnceLock<String>>,
    mut request_rx: Receiver<TileId>,
    mut tile_tx: Sender<(TileId, FetchPayload)>,
    egui_ctx: Context,
) where
    S: crate::basemap_archive::ArchiveRangeSource,
{
    use crate::basemap_archive::BasemapArchive;

    let archive = match BasemapArchive::open(source).await {
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
            let _ = fault.set(reason);
            // Nothing else would wake the UI to notice.
            egui_ctx.request_repaint();
            return;
        }
    };

    max_zoom.store(archive.max_zoom(), Ordering::Relaxed);
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

    let mut outstanding = FuturesUnordered::new();

    loop {
        let completed = if outstanding.is_empty() {
            match request_rx.next().await {
                Some(tile_id) => {
                    outstanding.push(read_one(&archive, &style, &egui_ctx, tile_id));
                    continue;
                }
                None => break,
            }
        } else if outstanding.len() < MAX_PARALLEL_DOWNLOADS {
            match select(request_rx.next(), outstanding.next()).await {
                Either::Left((Some(tile_id), pending)) => {
                    // Release the borrow of `outstanding` before pushing.
                    drop(pending);
                    outstanding.push(read_one(&archive, &style, &egui_ctx, tile_id));
                    continue;
                }
                Either::Left((None, _)) => break,
                Either::Right((completed, _)) => completed,
            }
        } else {
            outstanding.next().await
        };

        let Some(result) = completed else { continue };

        match result {
            // The archive positively holds no tile there. Not an error and not
            // a retry: the `None` the cache already carries under this id is
            // the right answer forever.
            Ok(None) => {}
            Ok(Some(fetched)) => {
                if tile_tx.send(fetched).await.is_err() {
                    break;
                }
                egui_ctx.request_repaint();
            }
            Err(error) => log::warn!("{error}"),
        }
    }

    log::debug!("archive tile loop finished");
}

/// Read one tile out of the archive -- and on native, tessellate and upload it
/// too.
///
/// `Ok(None)` is the archive positively holding nothing at that coordinate --
/// an ocean tile at zoom 14 -- which is why
/// [`crate::basemap_archive::TileBytes`] is a type rather than an empty `Vec`.
///
/// The payload split is [`fetch_one`]'s, for [`FetchPayload`]'s reason and not
/// a second one: on native the tessellation happens here, on wasm32 the IO task
/// *is* the page thread, so the MVT body crosses the channel undecoded and
/// [`HttpsTiles::drain_completed_fetches`] renders it under
/// [`WASM_TILE_DECODES_PER_PUMP`]. Doing it here on wasm32 would put an
/// unbounded tessellation on the frame thread; doing it there puts a bounded
/// one.
///
/// **On native the render runs on the runtime's blocking pool, not on the
/// calling task**, measured at **24.8 ms** for the committed Monaco fixture's
/// z14 city-core tile against the 95-layer committed style, release build,
/// 2026-08-28. There is no await point across it, so before this
/// [`MAX_PARALLEL_DOWNLOADS`] bounded the range requests and bounded nothing
/// about the tessellations: they ran one after another on [`runtime::spawn`]'s
/// single current-thread runtime, and during each 24.8 ms slice no range
/// request progressed either. A fresh 54-tile viewport of tiles this dense was
/// ~1.34 s of CPU serialized behind itself. `spawn_blocking` puts each render
/// on its own pool thread; `outstanding` still holds the concurrency at
/// [`MAX_PARALLEL_DOWNLOADS`], so this is bounded parallelism rather than an
/// unbounded fan-out, and the reads and the renders stop starving each other.
///
/// This is **not** the shape the plan asks for. The plan splits the
/// zoom-independent MVT parse from the zoom-dependent tessellation and
/// dispatches the second keyed `(TileId, zoom)`; this moves the whole of
/// `Tile::new` off the task as one unit. What it fixes is the serialization.
///
/// **Unverified, and stated so it is not a surprise later**: `Runtime::drop`
/// joins the IO thread, whose `tokio::runtime::Runtime` drop is documented to
/// wait for blocking tasks that have already started. If that holds, dropping a
/// source mid-tessellation blocks the *frame* thread for up to one render --
/// ~25 ms -- and `tiles::MapTileState::adopt_theme` drops a source on every
/// theme flip. The inline spelling had a wait of the same order for the same
/// reason, so this is not believed to be a regression; neither figure has been
/// measured. The wasm32 arm never reaches `spawn_blocking` at all.
#[cfg(feature = "basemap-vector")]
async fn read_one<S>(
    archive: &crate::basemap_archive::BasemapArchive<S>,
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            unused_variables,
            reason = "on wasm the frame pump tessellates; see FetchPayload"
        )
    )]
    style: &Arc<Style>,
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            unused_variables,
            reason = "on wasm the frame pump tessellates; see FetchPayload"
        )
    )]
    egui_ctx: &Context,
    tile_id: TileId,
) -> Result<Option<(TileId, FetchPayload)>, String>
where
    S: crate::basemap_archive::ArchiveRangeSource,
{
    let bytes = archive
        .tile(tile_id.zoom, tile_id.x, tile_id.y)
        .await
        .map_err(|error| format!("reading {tile_id:?} from the basemap archive: {error}"))?;

    let Some(bytes) = bytes.into_bytes() else {
        log::trace!("the basemap archive holds no tile at {tile_id:?}");
        return Ok(None);
    };

    #[cfg(not(target_arch = "wasm32"))]
    let payload = {
        let style = Arc::clone(style);
        let egui_ctx = egui_ctx.clone();

        tokio::task::spawn_blocking(move || Tile::new(&bytes, &style, tile_id.zoom, &egui_ctx))
            .await
            .map_err(|error| format!("rendering {tile_id:?} from the basemap archive: {error}"))?
            .map_err(|error| format!("rendering {tile_id:?} from the basemap archive: {error}"))?
    };

    // On wasm the tessellation belongs to the frame pump, under its budget.
    #[cfg(target_arch = "wasm32")]
    let payload = bytes;

    Ok(Some((tile_id, payload)))
}

// Native-only: `#[tokio::test]` (the dev-dependency is target-gated),
// `ClientBuilder::timeout` and `Error::is_connect`, which reqwest's wasm arm
// does not have, and `squallar_radar::tls::default_is_ring`, itself
// `cfg(not(wasm32))`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
