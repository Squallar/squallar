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
//! bytes** and the decode + upload runs in the frame pump under
//! [`WASM_TILE_DECODES_PER_PUMP`] — see [`FetchPayload`].
use std::num::NonZeroUsize;
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
/// Per device tier, because a tile texture costs the same 256 KiB (256x256
/// RGBA) everywhere while the memory it comes out of does not:
///
/// | tier    | entries | texture worst case, per source |
/// |---------|--------:|-------------------------------:|
/// | desktop |     256 |                        ~64 MiB |
/// | mobile  |     128 |                        ~32 MiB |
/// | wasm32  |      64 |                        ~16 MiB |
///
/// The desktop arm is walkers' own figure. "Per source" is the multiplier: each
/// map source owns one of these caches — base and labels, light and dark — so
/// the desktop figure on wasm32 could put 256 MiB of basemap texture against the
/// 288 MiB `squallar-device-profile`'s `constants::WASM_APP_TEXTURE_BUDGET_BYTES`
/// allows the whole application. The tiers follow that crate's budget cascade,
/// spelled rather than imported.
///
/// What the wasm arm accepts: the working set at native zoom is the window's own
/// tile count (`tiles::tiles_resident_for`), so a 1920x1080-point canvas keeps
/// ~54 tiles per source and fits, while 2560x1440 keeps ~77 and overruns.
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
pub const WASM_TILE_CACHE_ENTRIES: NonZeroUsize = NonZeroUsize::new(64).expect("64 is not zero");
/// The mobile arm. See [`WASM_TILE_CACHE_ENTRIES`].
pub const MOBILE_TILE_CACHE_ENTRIES: NonZeroUsize =
    NonZeroUsize::new(128).expect("128 is not zero");
/// The desktop arm — walkers' own figure. See [`WASM_TILE_CACHE_ENTRIES`].
pub const DESKTOP_TILE_CACHE_ENTRIES: NonZeroUsize =
    NonZeroUsize::new(256).expect("256 is not zero");

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
/// second at 60 fps. The pump requests a repaint whenever it uses its whole
/// allowance, so a backlog drains over idle frames.
///
/// Deliberately **capped inline, not offloaded**: tile PNGs behind the overlay
/// worker's job round-trip would trade a bounded per-pass cost for seconds of
/// blank basemap.
pub const WASM_TILE_DECODES_PER_PUMP: usize = 2;

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
/// Native: the decoded [`Tile`] — [`fetch_one`] decodes and uploads on the IO
/// thread. wasm32: the compressed PNG bytes, decoded by the frame pump at most
/// [`WASM_TILE_DECODES_PER_PUMP`] per source per pass — see
/// [`HttpsTiles::receive_fetched_tiles`].
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
    max_zoom: u8,

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

    /// Declared last so it drops last: the channels above must close first, which
    /// is what tells the fetch loop to exit.
    #[expect(dead_code, reason = "owned for its Drop; shuts the IO task down")]
    runtime: runtime::Runtime,
}

/// One source's per-pass decode allowance, wasm32 only.
///
/// [`Tiles::at`] runs once per visible tile per pass and the pump runs at the
/// top of every call, so a per-*call* bound would be no bound on the pass. The
/// pass number is what turns it per-pass: the first call of a new pass restores
/// the full allowance. Per *source* because each [`HttpsTiles`] owns one.
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
/// channel is the IO task gone, logged as
/// [`HttpsTiles::receive_one_fetched_tile`] logs it. Compiled on native for the
/// tests, like [`DecodeBudget`].
#[cfg(any(test, target_arch = "wasm32"))]
fn drain_up_to<T>(rx: &mut Receiver<T>, budget: usize, mut take: impl FnMut(T)) -> usize {
    let mut taken = 0;
    while taken < budget {
        match rx.try_recv() {
            Ok(item) => {
                take(item);
                taken += 1;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Closed) => {
                log::error!("the tile IO task is gone");
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
    fn with_client<S: AsyncTileSource>(
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
        let max_zoom = source.max_zoom();

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
            cache: LruCache::new(cache_entries),
            request_tx,
            tile_rx,
            #[cfg(target_arch = "wasm32")]
            egui_ctx: frame_ctx,
            #[cfg(target_arch = "wasm32")]
            decode_budget: DecodeBudget::new(),
            runtime,
        }
    }

    /// Move one fetched tile from the IO task into the cache.
    ///
    /// One per call, as walkers does: this runs every frame for every visible
    /// tile, and draining the whole channel here would put an unbounded number of
    /// texture uploads in one frame.
    #[cfg(not(target_arch = "wasm32"))]
    fn receive_one_fetched_tile(&mut self) {
        match self.tile_rx.try_recv() {
            Ok((tile_id, tile)) => {
                self.cache.put(tile_id, Some(tile));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Closed) => log::error!("the tile IO task is gone"),
        }
    }

    /// Decode, upload and cache at most this pass's remaining allowance of
    /// fetched tiles. The wasm32 counterpart of
    /// [`Self::receive_one_fetched_tile`].
    ///
    /// [`DecodeBudget`] keeps a burst of completed fetches from billing one pass
    /// for this source's whole backlog. The put is unconditional, exactly as a
    /// native tile arriving is: a `None` marker still means failed, do not ask
    /// again.
    #[cfg(target_arch = "wasm32")]
    fn receive_fetched_tiles(&mut self) {
        let Self {
            cache,
            tile_rx,
            egui_ctx,
            decode_budget,
            ..
        } = self;

        let budget = decode_budget.remaining(egui_ctx.cumulative_pass_nr());
        let taken = drain_up_to(tile_rx, budget, |(tile_id, bytes): (TileId, Vec<u8>)| {
            // `Style::default()` for the reason `fetch_one` gives on native.
            #[allow(
                clippy::default_constructed_unit_structs,
                reason = "keeps compiling if walkers/mvt is ever enabled"
            )]
            match Tile::new(&bytes, &Style::default(), tile_id.zoom, egui_ctx) {
                Ok(tile) => {
                    cache.put(tile_id, Some(tile));
                }
                Err(error) => log::warn!("decoding tile {tile_id:?}: {error}"),
            }
        });
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
            // UI thread down for; the map simply stops fetching.
            Err(error) => log::error!("cannot request tile {tile_id:?}: {error}"),
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
    pub fn source_max_zoom(&self) -> u8 {
        self.max_zoom
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
}

impl Tiles for HttpsTiles {
    fn attribution(&self) -> Attribution {
        self.attribution.clone()
    }

    /// Return the tile if it is available, and start a download if it is not.
    ///
    /// Called once per visible tile per frame by walkers' flood fill, so
    /// everything here is cheap and non-blocking.
    fn at(&mut self, tile_id: TileId) -> Option<TilePiece> {
        #[cfg(not(target_arch = "wasm32"))]
        self.receive_one_fetched_tile();
        #[cfg(target_arch = "wasm32")]
        self.receive_fetched_tiles();

        if !tile_id_is_valid(tile_id) {
            return None;
        }

        // Above the source's deepest zoom there is nothing to download; the
        // ancestor at `max_zoom` is what gets stretched over the gap.
        let to_fetch = if tile_id.zoom > self.max_zoom {
            interpolate_from_lower_zoom(tile_id, self.max_zoom).0
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

    // `Style::default()` rather than clippy's suggested `&Style`: walkers only
    // defines `Style` as a unit struct when its `mvt` feature is off, and with
    // `mvt` on `&Style` stops compiling.
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(
        clippy::default_constructed_unit_structs,
        reason = "keeps compiling if walkers/mvt is ever enabled"
    )]
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

// Native-only: `#[tokio::test]` (the dev-dependency is target-gated),
// `ClientBuilder::timeout` and `Error::is_connect`, which reqwest's wasm arm
// does not have, and `squallar_radar::tls::default_is_ring`, itself
// `cfg(not(wasm32))`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
