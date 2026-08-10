//! rustdar's own basemap tile source.
//!
//! # Why this exists
//!
//! Every other HTTPS call rustdar makes goes through [`rustdar_radar::tls::client`],
//! which is `rustls-platform-verifier` + *ring*: the operating system decides
//! trust at handshake time, so nothing in the binary carries an expiry date and
//! OS distrust lists, enterprise CAs and user CAs all apply.
//!
//! Basemap tiles were the one path that bypassed it. [`walkers::HttpTiles`]
//! builds its own `reqwest` client internally (`walkers-0.56.0/src/io/http.rs`,
//! `bare_client`) and exposes no way to supply one, and walkers' manifest
//! declares
//!
//! ```toml
//! reqwest = { version = "0.12", features = ["rustls-tls"], default-features = false }
//! ```
//!
//! unconditionally — not `optional`, gated behind no feature. In reqwest 0.12
//! `rustls-tls` resolves to `webpki-roots`: a copy of the Mozilla root store
//! compiled into the binary, frozen at whatever was current when that crate
//! version was published. That is an expiration date on the shipped artifact,
//! which is exactly what the platform-verifier migration existed to remove.
//!
//! [`HttpsTiles`] replaces `HttpTiles`. It implements [`walkers::Tiles`] — the
//! only thing [`walkers::Map`] actually requires of a tile source — so the rest
//! of the walkers integration (projection, `MapMemory`, the flood-fill tile
//! draw, `TilePiece`/`Tile`) is untouched and unduplicated.
//!
//! # Behaviour parity with `HttpTiles`
//!
//! This is not a shim; `HttpTiles` does real work and all of it is reproduced:
//!
//! * **Async fetching** off the UI thread, on a dedicated IO runtime.
//! * **Bounded concurrency** at [`MAX_PARALLEL_DOWNLOADS`], matching walkers'
//!   `MaxParallelDownloads::default()`. Tile providers rate-limit; this is a
//!   politeness obligation, not a tuning knob.
//! * **Decode and texture upload** via [`walkers::Tile::new`], which is the same
//!   public entry point walkers' own `EguiTileFactory` calls. Reusing it rather
//!   than re-implementing the `image` → [`egui::ColorImage`] → texture path means
//!   the pixels cannot drift from what `HttpTiles` produced.
//! * **A bounded in-memory cache** of [`TILE_CACHE_ENTRIES`] tiles with LRU
//!   eviction, sized as walkers sizes it.
//! * **In-flight de-duplication**: a tile already requested is not requested
//!   again while it is pending. See [`HttpsTiles::request_once`] — as in walkers,
//!   the cache's `None` entry *is* the in-flight marker.
//! * **Lower-zoom interpolation**: a missing tile is drawn as a stretched piece
//!   of the nearest cached ancestor, which is what stops the map flashing blank
//!   while panning or zooming.
//! * **`max_zoom` clamping**, **tile-grid bounds checking**, and
//!   **[`egui::Context::request_repaint`] on arrival** (without which a fetched
//!   tile would not appear until some unrelated input woke the UI).
//! * **Attribution**, carried through unchanged from the source.
//!
//! # Deliberate differences
//!
//! * **The client.** Ours comes from [`rustdar_radar::tls::client`]: platform
//!   verifier, *ring*, `https_only(true)`, and a [`REQUEST_TIMEOUT`]. walkers
//!   sets no timeout, so a black-holed connection there occupies a concurrency
//!   slot indefinitely.
//! * **The `User-Agent`** is rustdar's rather than `walkers/0.56.0`. Tile
//!   providers require a UA that identifies the client; ours does, and it is the
//!   same one the rest of the application sends.
//! * **A closed request channel logs instead of panicking.** walkers'
//!   `TilesIo::make_sure_is_fetched` calls `panic!` there; taking down the UI
//!   thread because the IO task exited is a worse outcome than a map that stops
//!   fetching.
//! * **No HTTP disk cache.** Neither has one: `HttpTiles::new` uses
//!   `HttpOptions::default()`, whose `cache` field is `None`, and rustdar never
//!   called `with_options`. walkers' `http-cache-reqwest` middleware is only
//!   installed when that field is `Some`, so there is nothing to preserve.
//!
//! # wasm32
//!
//! The IO runtime is split per target exactly as walkers splits it: a thread
//! with a current-thread tokio runtime on native, `spawn_local` on wasm, where
//! `reqwest` becomes a `fetch()` call and the browser owns the trust store.

use std::num::NonZeroUsize;
use std::time::Duration;

use egui::Context;
use futures::channel::mpsc::{Receiver, Sender, TryRecvError, TrySendError, channel};
use futures::stream::FuturesUnordered;
use futures::{SinkExt, StreamExt, future::Either, future::select};
use lru::LruCache;
use walkers::sources::{Attribution, TileSource};
use walkers::{Style, Tile, TileId, TilePiece, Tiles};

/// Maximum number of tile downloads in flight at once.
///
/// walkers' default, which follows what browsers allow per host. Tile providers
/// throttle or ban clients that exceed their limits, so this is a term of use
/// rather than a performance dial.
pub const MAX_PARALLEL_DOWNLOADS: usize = 6;

/// Tiles retained in the in-memory cache before LRU eviction starts.
///
/// walkers' figure. A 256-entry cache at 256x256 RGBA is ~64 MiB of texture in
/// the worst case, which is why it is bounded at all.
pub const TILE_CACHE_ENTRIES: NonZeroUsize = NonZeroUsize::new(256).expect("256 is not zero");

/// Per-tile request timeout.
///
/// walkers sets none. A tile that never answers would otherwise hold one of the
/// [`MAX_PARALLEL_DOWNLOADS`] slots for as long as the process lives.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Target-dependent bounds
// ---------------------------------------------------------------------------

/// A [`TileSource`] that can be moved into the IO task.
///
/// The bound differs by target and this alias is where that difference lives, so
/// [`HttpsTiles::new`] has one signature and one body. On native the task runs on
/// another thread and everything it captures must be `Send + Sync`; on wasm it
/// runs on the same thread as the UI, where `reqwest`'s types are neither.
#[cfg(not(target_arch = "wasm32"))]
pub trait AsyncTileSource: TileSource + Send + Sync + 'static {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: TileSource + Send + Sync + 'static> AsyncTileSource for T {}

#[cfg(target_arch = "wasm32")]
pub trait AsyncTileSource: TileSource + 'static {}
#[cfg(target_arch = "wasm32")]
impl<T: TileSource + 'static> AsyncTileSource for T {}

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
        /// The future is `spawn`ed rather than `block_on`'d, and the thread parks
        /// on a quit channel instead. That is what makes shutdown immediate:
        /// dropping the runtime cancels an in-flight request outright, whereas
        /// blocking on the future itself would make [`Runtime::drop`] wait out
        /// whatever HTTP request happened to be in progress — up to
        /// [`super::super::REQUEST_TIMEOUT`], on the UI thread, every time the map
        /// tiles are torn down.
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
        ///
        /// It still stops when [`super::super::HttpsTiles`] is dropped — the fetch
        /// loop exits as soon as its request channel reports that the sender is
        /// gone.
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
/// which overflows for `zoom >= 32` — a panic in debug, a silent `0` in release.
/// `TileId::zoom` is a `u8`, so nothing in the type system rules that out. The
/// checked form is a strict improvement: no reachable zoom level behaves
/// differently, and the unreachable ones stop being undefined.
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
/// texture (`uv`) coordinates. Drawing that ancestor through the returned `uv` is
/// how a zoomed-in view stays populated with blurry tiles instead of holes while
/// the sharp ones download.
///
/// Reproduces walkers' `tiles::interpolate_from_lower_zoom`, also `pub(crate)`.
///
/// # Panics
///
/// Debug-asserts `available_zoom <= tile_id.zoom`. Every call site here either
/// walks `zoom` downwards or clamps to `max_zoom`, and all of them run after
/// [`tile_id_is_valid`] has rejected `zoom >= 32`, so the shift below cannot
/// overflow.
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

/// The HTTP client every basemap tile is fetched with.
///
/// The whole point of this module is that this function is
/// [`rustdar_radar::tls::client`] and nothing else: platform verifier, *ring*,
/// `https_only`. There is deliberately no way to inject a different client from
/// outside the crate.
fn tile_client() -> reqwest::Client {
    rustdar_radar::tls::client(rustdar_radar::tls::USER_AGENT, REQUEST_TIMEOUT)
        .build()
        .expect("the tile HTTP client should build")
}

// ---------------------------------------------------------------------------
// HttpsTiles
// ---------------------------------------------------------------------------

/// A [`walkers::Tiles`] that fetches over HTTPS trusted by the operating system.
///
/// One fetched tile: the texture walkers draws, and the compressed bytes it
/// was decoded from — kept so a consumer that needs pixels on the CPU (the 3D
/// floor's map composite) does not need a second download or a second decode
/// path diverging from this one.
#[derive(Clone)]
struct CachedTile {
    tile: Tile,
    bytes: std::sync::Arc<Vec<u8>>,
}

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
    ///
    /// Each hit carries the tile's **compressed source bytes** beside the
    /// decoded texture — ~30 KiB of PNG against the texture's 256 KiB — which
    /// is what un-blocks the 3D floor's map composite: the tile pipeline used
    /// to decode straight into an egui texture and keep no CPU-readable form,
    /// and rustdar owns this fetch path precisely so decisions like that are
    /// its own to make. See [`Self::raster_bytes_at`].
    cache: LruCache<TileId, Option<CachedTile>>,

    /// Tiles the IO task should fetch.
    request_tx: Sender<TileId>,
    /// Tiles the IO task has fetched and decoded.
    tile_rx: Receiver<(TileId, CachedTile)>,

    /// Declared last so it drops last: the channels above must close first, which
    /// is what tells the fetch loop to exit.
    #[expect(dead_code, reason = "owned for its Drop; shuts the IO task down")]
    runtime: runtime::Runtime,
}

impl HttpsTiles {
    /// Fetch `source`'s tiles through rustdar's platform-verified HTTPS client.
    pub fn new<S: AsyncTileSource>(source: S, egui_ctx: Context) -> Self {
        Self::with_client(source, egui_ctx, tile_client())
    }

    /// [`Self::new`], with the HTTP client supplied.
    ///
    /// Crate-private and exists for the tests, which need to talk cleartext to a
    /// loopback server — [`tile_client`] refuses `http://` by design, and that
    /// refusal is itself under test in `tile_client_refuses_cleartext_urls`.
    fn with_client<S: AsyncTileSource>(
        source: S,
        egui_ctx: Context,
        client: reqwest::Client,
    ) -> Self {
        let attribution = source.attribution();
        let tile_size = source.tile_size();
        let max_zoom = source.max_zoom();

        // Sized to the concurrency limit, as walkers sizes them: a full request
        // channel is the backpressure signal that makes `request_once` retry on a
        // later frame rather than queue without bound.
        let (request_tx, request_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let (tile_tx, tile_rx) = channel(MAX_PARALLEL_DOWNLOADS);

        let runtime = runtime::spawn(fetch_continuously(
            source, client, request_rx, tile_tx, egui_ctx,
        ));

        Self {
            attribution,
            tile_size,
            max_zoom,
            cache: LruCache::new(TILE_CACHE_ENTRIES),
            request_tx,
            tile_rx,
            runtime,
        }
    }

    /// Move one fetched tile from the IO task into the cache.
    ///
    /// One per call, as walkers does: this runs every frame for every visible
    /// tile, and draining the whole channel here would put an unbounded number of
    /// texture uploads in one frame.
    fn receive_one_fetched_tile(&mut self) {
        match self.tile_rx.try_recv() {
            Ok((tile_id, cached)) => {
                self.cache.put(tile_id, Some(cached));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Closed) => log::error!("the tile IO task is gone"),
        }
    }

    /// Ask for `tile_id` unless it has already been asked for.
    ///
    /// The de-duplication and the cache are the same structure: inserting `None`
    /// under the tile's id both records "a request is out for this" and reserves
    /// the slot the tile will land in. `try_get_or_insert` runs the closure only
    /// when the key is absent, so a tile that is pending, cached or permanently
    /// failed is never requested twice.
    ///
    /// When the request channel is full the closure fails, *nothing is inserted*,
    /// and the tile is retried on a later frame. That is deliberate: dropping the
    /// request while marking the tile as requested would strand it forever.
    fn request_once(&mut self, tile_id: TileId) {
        // Split borrow: the closure needs the sender while the cache is borrowed.
        let Self {
            cache, request_tx, ..
        } = self;

        let outcome = cache.try_get_or_insert(
            tile_id,
            || -> Result<Option<CachedTile>, TrySendError<TileId>> {
                request_tx.try_send(tile_id)?;
                log::trace!("requested tile {tile_id:?}");
                Ok(None)
            },
        );

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
                break Some(TilePiece::new(cached.tile.clone(), uv));
            }

            // Out of ancestors: nothing to draw for this tile yet.
            zoom_candidate = zoom_candidate.checked_sub(1)?;
        }
    }

    /// The source's deepest zoom, so a consumer picking its own zoom level
    /// (the 3D floor's composite) can clamp to what this source can serve.
    pub fn source_max_zoom(&self) -> u8 {
        self.max_zoom
    }

    /// The compressed bytes of exactly the slippy tile `(x, y)` at `zoom`,
    /// starting a download when the tile has never been asked for.
    ///
    /// The 3D floor's map composite calls this — once per needed tile per
    /// frame, the same cadence [`Tiles::at`] runs at for visible tiles — so a
    /// 3D pane drives tile fetching even when no 2D pane is looking at the
    /// same ground. No ancestor interpolation on purpose: the composite
    /// resamples pixels itself and stretching a lower zoom underneath it would
    /// bake a blur into the floor that quietly stopped refreshing; absence
    /// here means "not yet", and the caller re-composes when the bytes land.
    /// Primitive coordinates rather than [`TileId`] so the caller
    /// (`rustdar-frontend`) does not need walkers in its dependency set.
    pub fn raster_bytes_at(&mut self, x: u32, y: u32, zoom: u8) -> Option<std::sync::Arc<Vec<u8>>> {
        self.receive_one_fetched_tile();
        let tile_id = TileId { x, y, zoom };
        if !tile_id_is_valid(tile_id) || tile_id.zoom > self.max_zoom {
            return None;
        }
        self.request_once(tile_id);
        self.cache
            .get(&tile_id)
            .and_then(|hit| hit.as_ref())
            .map(|cached| std::sync::Arc::clone(&cached.bytes))
    }

    /// Tiles currently held, including pending and failed markers.
    ///
    /// Exposed for the eviction test; the map has no use for it. Gated off
    /// wasm32 with `mod tests`, its only caller, or it would be dead there.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn cached_entries(&self) -> usize {
        self.cache.len()
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
        self.receive_one_fetched_tile();

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

/// Download, decode and upload one tile.
///
/// Decoding happens here, on the IO runtime, rather than on the UI thread —
/// which is also where walkers does it. [`Tile::new`] performs the PNG decode and
/// the [`egui::Context::load_texture`] call; `Context` is `Send + Sync` and locks
/// internally, so uploading from this thread is sound.
///
/// The error is a `String` because the decode error type, walkers' `TileError`,
/// is not exported from walkers and so cannot be named here. Nothing acts on
/// these errors — walkers logs and drops them too — so flattening loses nothing.
async fn fetch_one<S: TileSource>(
    source: &S,
    client: &reqwest::Client,
    egui_ctx: &Context,
    tile_id: TileId,
) -> Result<(TileId, CachedTile), String> {
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
    // defines `Style` as a unit struct when its `mvt` feature is off. With `mvt`
    // on it is a real struct with fields, `&Style` stops compiling, and
    // `default()` keeps working. Writing the unit literal would make this file
    // silently depend on a feature of a dependency staying disabled.
    #[allow(
        clippy::default_constructed_unit_structs,
        reason = "keeps compiling if walkers/mvt is ever enabled"
    )]
    let tile = Tile::new(&body, &Style::default(), tile_id.zoom, egui_ctx)
        .map_err(|error| format!("decoding '{url}': {error}"))?;

    Ok((
        tile_id,
        CachedTile {
            tile,
            bytes: std::sync::Arc::new(body.to_vec()),
        },
    ))
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
    mut tile_tx: Sender<(TileId, CachedTile)>,
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
// does not have, and `rustdar_radar::tls::default_is_ring`, itself
// `cfg(not(wasm32))`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
