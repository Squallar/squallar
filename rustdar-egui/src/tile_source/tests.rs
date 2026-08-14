//! Tests for [`super`].
//!
//! # What "it works" has to mean here
//!
//! A tile source that constructs without panicking proves nothing: the whole
//! job is bytes on the wire becoming pixels in a texture. So the tests below
//! stand up a real HTTP server on loopback, serve a real PNG whose every pixel
//! is known, and read the pixels back out of the [`egui::Context`]'s texture
//! manager at the point a renderer would collect them. See
//! [`a_fetched_tile_reaches_an_egui_texture_with_the_pixels_that_were_served`].
//!
//! # Why the loopback tests inject a client
//!
//! [`super::tile_client`] sets `https_only`, so it cannot talk to a cleartext
//! loopback server — and that refusal is a feature under test in its own right
//! (`the_tile_client_refuses_cleartext`). The two are tested separately: the
//! machinery over cleartext loopback, the client's TLS posture on its own, and
//! both together in the `#[ignore]`d live test, which uses
//! [`HttpsTiles::new`] and therefore the real client against the real CDN.

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::Context;
use walkers::sources::{Attribution, TileSource};
use walkers::{Tile, TileId, TilePiece, Tiles};

use super::{
    DESKTOP_TILE_CACHE_ENTRIES, HttpsTiles, MAX_PARALLEL_DOWNLOADS, MOBILE_TILE_CACHE_ENTRIES,
    TILE_CACHE_ENTRIES, WASM_TILE_CACHE_ENTRIES, interpolate_from_lower_zoom, tile_client,
    tile_id_is_valid,
};
use crate::tiles::CartoDb;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Side length of the PNG fixture, in pixels.
///
/// Deliberately not 256: a decoder that ignored the image header and assumed the
/// usual tile size would still produce a 256x256 texture, and the size assertions
/// would not notice.
const FIXTURE_SIDE: u32 = 4;

/// The colour of one fixture pixel.
///
/// Every pixel differs from every other, and the three channels move at
/// different rates in different directions, so a transposed, flipped or
/// channel-swapped decode cannot land on the same bytes.
///
/// Alpha is 255 throughout, and that is load-bearing rather than lazy: egui
/// stores premultiplied colour, and premultiplication is the identity only at
/// full alpha. At any other alpha the expected value would have to model egui's
/// rounding, which would make the assertion a restatement of the conversion it
/// is supposed to be checking.
fn fixture_pixel(x: u32, y: u32) -> [u8; 4] {
    [
        (17 + x * 40) as u8,
        (3 + y * 50) as u8,
        (200 - x * 7 - y * 29) as u8,
        255,
    ]
}

/// The fixture, encoded as a real PNG.
///
/// Encoded rather than checked in so the expected pixels and the served bytes
/// cannot drift apart, and so the decode under test is exercised on a genuine
/// PNG bitstream rather than on a blob nobody can read.
fn fixture_png() -> Vec<u8> {
    let mut bitmap = image::RgbaImage::new(FIXTURE_SIDE, FIXTURE_SIDE);
    for y in 0..FIXTURE_SIDE {
        for x in 0..FIXTURE_SIDE {
            bitmap.put_pixel(x, y, image::Rgba(fixture_pixel(x, y)));
        }
    }

    let mut encoded = std::io::Cursor::new(Vec::new());
    bitmap
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("the fixture should encode as a PNG");
    encoded.into_inner()
}

/// The `uv` of a tile drawn whole, rather than as a piece of an ancestor.
fn whole_tile_uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}

/// The tuning values, restated as literals.
///
/// Deliberately *not* read from [`super::TILE_CACHE_ENTRIES`] and
/// [`super::MAX_PARALLEL_DOWNLOADS`]. A test that took its expectation from the
/// constant it is checking would move in lockstep with any change to it and
/// could never fail — the first version of
/// [`no_more_than_the_concurrency_limit_is_downloaded_at_once`] did exactly
/// that, and a mutant raising the limit from 6 to 64 sailed through it.
/// [`the_tuning_constants_are_the_written_figures_on_every_tier`] is what ties
/// these back to the code.
const EXPECTED_CACHE_ENTRIES: usize = 256;
const EXPECTED_MOBILE_CACHE_ENTRIES: usize = 128;
const EXPECTED_WASM_CACHE_ENTRIES: usize = 64;
const EXPECTED_PARALLEL_DOWNLOADS: usize = 6;

// ---------------------------------------------------------------------------
// A loopback tile server
// ---------------------------------------------------------------------------

/// How the loopback server answers a request.
#[derive(Clone)]
enum Behaviour {
    /// `200` with the given body.
    Serve(Arc<Vec<u8>>),
    /// `200`, but the body is not an image.
    Garbage,
    /// `404`.
    NotFound,
    /// Record the request and never answer it.
    ///
    /// The connection is parked, not closed, so the client neither succeeds nor
    /// fails. This is what "a download is in flight" looks like from outside,
    /// and it is what makes the de-duplication test able to observe a *pending*
    /// request rather than a completed one.
    Hang,
    /// `200` with the body for one exact path; [`Behaviour::Hang`] for the rest.
    ///
    /// Lets a test put exactly one tile in the cache and leave a chosen
    /// descendant permanently unavailable.
    ServeOnly { path: String, body: Arc<Vec<u8>> },
}

/// A real HTTP/1.1 server on loopback, recording every path it is asked for.
struct TileServer {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    accept: Option<std::thread::JoinHandle<()>>,
    /// Connections parked by [`Behaviour::Hang`], held open until shutdown.
    parked: Arc<Mutex<Vec<TcpStream>>>,
}

impl TileServer {
    fn start(behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
        let addr = listener.local_addr().expect("read back the bound address");
        // Non-blocking accept so the thread can notice `stop` instead of parking
        // in `accept` until the process exits.
        listener
            .set_nonblocking(true)
            .expect("put the listener in non-blocking mode");

        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let parked = Arc::new(Mutex::new(Vec::new()));

        let accept = std::thread::spawn({
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            let parked = Arc::clone(&parked);
            move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let requests = Arc::clone(&requests);
                            let parked = Arc::clone(&parked);
                            let behaviour = behaviour.clone();
                            std::thread::spawn(move || {
                                serve_one(stream, behaviour, &requests, &parked);
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            requests,
            stop,
            accept: Some(accept),
            parked,
        }
    }

    /// Every path requested so far, in arrival order.
    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("the request log should not be poisoned")
            .clone()
    }

    fn request_count(&self) -> usize {
        self.requests
            .lock()
            .expect("the request log should not be poisoned")
            .len()
    }

    /// Drive `pump` until at least `n` requests have arrived. `false` on timeout.
    ///
    /// Takes the pump because nothing reaches the network on its own: `at` is
    /// what hands a tile id to the IO task, so a test that merely slept would be
    /// waiting for something it never asked for.
    fn wait_for_requests(&self, n: usize, pump: &mut dyn FnMut()) -> bool {
        pump_until(DEFAULT_TIMEOUT, || {
            pump();
            (self.request_count() >= n).then_some(())
        })
        .is_some()
    }
}

impl Drop for TileServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Closing the parked connections releases any client still waiting.
        if let Ok(mut parked) = self.parked.lock() {
            parked.clear();
        }
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve_one(
    mut stream: TcpStream,
    behaviour: Behaviour,
    requests: &Mutex<Vec<String>>,
    parked: &Mutex<Vec<TcpStream>>,
) {
    let _ = stream.set_nonblocking(false);

    // Tile requests are GETs with no body, and the request line is in the first
    // segment on loopback, so one read is always enough to learn the path.
    let mut buffer = [0u8; 2048];
    let Ok(read) = stream.read(&mut buffer) else {
        return;
    };
    if read == 0 {
        return;
    }
    let request = String::from_utf8_lossy(&buffer[..read]);
    let Some(path) = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
    else {
        return;
    };
    let path = path.to_owned();

    if let Ok(mut requests) = requests.lock() {
        requests.push(path.clone());
    }

    match behaviour {
        Behaviour::Serve(body) => respond(&mut stream, 200, "OK", &body),
        Behaviour::Garbage => respond(&mut stream, 200, "OK", b"definitely not an image"),
        Behaviour::NotFound => respond(&mut stream, 404, "Not Found", b"no such tile"),
        Behaviour::Hang => {
            if let Ok(mut parked) = parked.lock() {
                parked.push(stream);
            }
        }
        Behaviour::ServeOnly {
            path: served, body, ..
        } => {
            if path == served {
                respond(&mut stream, 200, "OK", &body);
            } else if let Ok(mut parked) = parked.lock() {
                parked.push(stream);
            }
        }
    }
}

fn respond(stream: &mut TcpStream, status: u16, reason: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: image/png\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

// ---------------------------------------------------------------------------
// A tile source pointed at the loopback server
// ---------------------------------------------------------------------------

struct LoopbackSource {
    base_url: String,
    tile_size: u32,
    max_zoom: u8,
}

impl LoopbackSource {
    fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_owned(),
            tile_size: 256,
            max_zoom: 19,
        }
    }

    fn with_tile_size(mut self, tile_size: u32) -> Self {
        self.tile_size = tile_size;
        self
    }

    fn with_max_zoom(mut self, max_zoom: u8) -> Self {
        self.max_zoom = max_zoom;
        self
    }
}

impl TileSource for LoopbackSource {
    fn tile_url(&self, tile_id: TileId) -> String {
        format!(
            "{}/{}/{}/{}.png",
            self.base_url, tile_id.zoom, tile_id.x, tile_id.y
        )
    }

    fn attribution(&self) -> Attribution {
        Attribution {
            text: "loopback",
            url: "http://127.0.0.1/",
            logo_light: None,
            logo_dark: None,
        }
    }

    fn tile_size(&self) -> u32 {
        self.tile_size
    }

    fn max_zoom(&self) -> u8 {
        self.max_zoom
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// The live CDN gets longer than loopback does.
const LIVE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long "and it stays that way" observations watch for.
const SETTLE: Duration = Duration::from_millis(300);

/// A client that can reach the cleartext loopback server.
///
/// [`super::tile_client`] deliberately cannot — see the module docs. `tls::init`
/// still has to run: with the workspace's `rustls-no-provider` pin,
/// `ClientBuilder::build` panics outright when no crypto provider is installed,
/// whatever scheme the request later uses.
fn loopback_client() -> reqwest::Client {
    rustdar_radar::tls::init();
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("the loopback client should build")
}

fn loopback_tiles(server: &TileServer, ctx: &Context) -> HttpsTiles {
    HttpsTiles::with_client(
        LoopbackSource::new(&server.base_url),
        ctx.clone(),
        loopback_client(),
    )
}

/// [`loopback_tiles`] with the cache bound handed in, for the per-tier
/// eviction test — `cargo test` only ever compiles one arm of
/// [`TILE_CACHE_ENTRIES`], so the other tiers' bounds have to arrive through
/// the same seam the client does.
fn loopback_tiles_with_capacity(server: &TileServer, ctx: &Context, capacity: usize) -> HttpsTiles {
    HttpsTiles::with_client_and_cache(
        LoopbackSource::new(&server.base_url),
        ctx.clone(),
        loopback_client(),
        std::num::NonZeroUsize::new(capacity).expect("a test capacity is not zero"),
    )
}

/// Poll `step` until it yields, or the deadline passes.
///
/// `Tiles::at` is the frame-driven API: it moves at most one fetched tile into
/// the cache per call, so polling it is exactly what the map does, not a test
/// affordance.
fn pump_until<T>(timeout: Duration, mut step: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = step() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Drive `tiles` until `tile_id` is drawable.
fn tile_eventually(tiles: &mut HttpsTiles, tile_id: TileId) -> TilePiece {
    pump_until(DEFAULT_TIMEOUT, || tiles.at(tile_id))
        .unwrap_or_else(|| panic!("{tile_id:?} never became available"))
}

/// Keep calling `at` for [`SETTLE`], asserting it never yields.
fn stays_unavailable(tiles: &mut HttpsTiles, tile_id: TileId) {
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        assert!(
            tiles.at(tile_id).is_none(),
            "{tile_id:?} became available when it should not have"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Run empty egui passes until the context stops asking for repaints by itself.
///
/// A fresh [`Context`] requests one while it builds its font atlas, and egui
/// answers every request with *two* passes (`ContextImpl::request_repaint_after`
/// sets `outstanding = 1`), so the flag is still set some passes later. Settling
/// that out is what makes a subsequent `true` attributable to the tile arriving
/// rather than to egui's own start-up.
///
/// `begin_pass` / `end_pass` rather than `run_ui`, matching `input_harness`.
fn settle_repaints(ctx: &Context) {
    for _ in 0..32 {
        ctx.begin_pass(egui::RawInput::default());
        let _ = ctx.end_pass();
        if !ctx.has_requested_repaint() {
            return;
        }
    }
    panic!("egui kept requesting repaints on its own; the flag cannot be used as a signal");
}

/// The pixels egui actually holds for `texture`.
///
/// `load_texture` queues an upload in the context's texture manager, and
/// `take_delta` is what a render backend calls to collect it. Reading it here
/// observes the image at the exact point it would be handed to the GPU, which is
/// as close to "on screen" as a headless test reaches.
///
/// Drains the queue, so call it once per texture.
fn uploaded_pixels(ctx: &Context, texture: &egui::TextureHandle) -> egui::ColorImage {
    let delta = ctx.tex_manager().write().take_delta();
    for (id, image_delta) in delta.set {
        if id == texture.id() {
            let egui::epaint::ImageData::Color(image) = image_delta.image;
            return (*image).clone();
        }
    }
    panic!("no texture upload was queued for the tile");
}

// ---------------------------------------------------------------------------
// The CartoDB source
// ---------------------------------------------------------------------------

/// The URL scheme is the contract with the provider; a typo here is a map that
/// silently shows nothing.
///
/// `x`, `y` and `zoom` are all different so a transposition fails, and the four
/// styles are checked together because they differ only in one path segment.
#[test]
fn cartodb_urls_match_the_published_tile_scheme() {
    // x % 4 == 1, so the subdomain is `b`.
    let tile_id = TileId {
        x: 5,
        y: 12,
        zoom: 4,
    };

    assert_eq!(
        CartoDb::light().tile_url(tile_id),
        "https://cartodb-basemaps-b.global.ssl.fastly.net/light_nolabels/4/5/12.png"
    );
    assert_eq!(
        CartoDb::dark().tile_url(tile_id),
        "https://cartodb-basemaps-b.global.ssl.fastly.net/dark_nolabels/4/5/12.png"
    );
    assert_eq!(
        CartoDb::light_labels().tile_url(tile_id),
        "https://cartodb-basemaps-b.global.ssl.fastly.net/light_only_labels/4/5/12.png"
    );
    assert_eq!(
        CartoDb::dark_labels().tile_url(tile_id),
        "https://cartodb-basemaps-b.global.ssl.fastly.net/dark_only_labels/4/5/12.png"
    );
}

/// Requests are spread over the provider's four subdomains, cycling on `x`.
#[test]
fn cartodb_spreads_tiles_over_all_four_subdomains() {
    let hosts: Vec<String> = (0..8)
        .map(|x| {
            let url = CartoDb::light().tile_url(TileId { x, y: 0, zoom: 3 });
            url.split('/')
                .nth(2)
                .expect("the URL should have a host")
                .to_owned()
        })
        .collect();

    let expected: Vec<String> = ["a", "b", "c", "d", "a", "b", "c", "d"]
        .iter()
        .map(|s| format!("cartodb-basemaps-{s}.global.ssl.fastly.net"))
        .collect();

    assert_eq!(hosts, expected);
}

/// Attribution is a licensing obligation, not decoration.
#[test]
fn cartodb_attribution_names_openstreetmap_and_cartodb() {
    for source in [
        CartoDb::light(),
        CartoDb::dark(),
        CartoDb::light_labels(),
        CartoDb::dark_labels(),
    ] {
        let attribution = source.attribution();
        assert_eq!(attribution.text, "\u{a9} OpenStreetMap \u{a9} CartoDB");
        assert_eq!(attribution.url, "https://www.openstreetmap.org/copyright");
    }
}

/// The attribution the map displays is the source's, not a placeholder.
///
/// The test above pins the source. This one pins the wrapper, which is a
/// different function and the one the widget actually calls.
#[test]
fn attribution_reaches_the_tiles_trait() {
    let tiles = HttpsTiles::new(CartoDb::dark(), Context::default());

    let attribution = Tiles::attribution(&tiles);
    assert_eq!(attribution.text, "\u{a9} OpenStreetMap \u{a9} CartoDB");
    assert_eq!(attribution.url, "https://www.openstreetmap.org/copyright");
}

// ---------------------------------------------------------------------------
// Tile-grid arithmetic
// ---------------------------------------------------------------------------

/// A tile is its own ancestor at its own zoom, and covers all of it.
#[test]
fn a_tile_interpolated_from_its_own_zoom_is_itself() {
    let tile_id = TileId {
        x: 6,
        y: 3,
        zoom: 4,
    };

    let (ancestor, uv) = interpolate_from_lower_zoom(tile_id, tile_id.zoom);

    assert_eq!(ancestor, tile_id);
    assert_eq!(uv, whole_tile_uv());
}

/// One zoom level out: the tile is one quadrant of its parent.
#[test]
fn a_tile_interpolated_one_level_out_is_a_quadrant_of_its_parent() {
    // x = 3 -> parent x 1, odd column; y = 1 -> parent y 0, odd row.
    // So the south-east quadrant.
    let (ancestor, uv) = interpolate_from_lower_zoom(
        TileId {
            x: 3,
            y: 1,
            zoom: 2,
        },
        1,
    );

    assert_eq!(
        ancestor,
        TileId {
            x: 1,
            y: 0,
            zoom: 1
        }
    );
    assert_eq!(
        uv,
        egui::Rect::from_min_max(egui::pos2(0.5, 0.5), egui::pos2(1.0, 1.0))
    );
}

/// Two levels out: a sixteenth, and the offsets have to scale with it.
///
/// One level cannot distinguish `dzoom` from a hard-coded 2, nor `z` from a
/// hard-coded 0.5. This one can.
#[test]
fn a_tile_interpolated_two_levels_out_is_a_sixteenth_of_its_grandparent() {
    // x = 7 -> grandparent x 1, offset 3; y = 5 -> grandparent y 1, offset 1.
    let (ancestor, uv) = interpolate_from_lower_zoom(
        TileId {
            x: 7,
            y: 5,
            zoom: 3,
        },
        1,
    );

    assert_eq!(
        ancestor,
        TileId {
            x: 1,
            y: 1,
            zoom: 1
        }
    );
    assert_eq!(
        uv,
        egui::Rect::from_min_max(egui::pos2(0.75, 0.25), egui::pos2(1.0, 0.5))
    );
}

/// Tiles outside their own zoom's grid are rejected, including at zoom levels
/// where computing the grid size would overflow.
#[test]
fn tile_ids_outside_their_zoom_grid_are_invalid() {
    // Zoom 0 is a single tile.
    assert!(tile_id_is_valid(TileId {
        x: 0,
        y: 0,
        zoom: 0
    }));
    assert!(!tile_id_is_valid(TileId {
        x: 1,
        y: 0,
        zoom: 0
    }));
    assert!(!tile_id_is_valid(TileId {
        x: 0,
        y: 1,
        zoom: 0
    }));

    // Zoom 2 is 4x4, so 3 is the last index on each axis.
    assert!(tile_id_is_valid(TileId {
        x: 3,
        y: 3,
        zoom: 2
    }));
    assert!(!tile_id_is_valid(TileId {
        x: 4,
        y: 3,
        zoom: 2
    }));
    assert!(!tile_id_is_valid(TileId {
        x: 3,
        y: 4,
        zoom: 2
    }));

    // `2u32.pow(32)` overflows. walkers' unchecked version panics here in debug
    // and silently reports a zero-tile grid in release.
    assert!(!tile_id_is_valid(TileId {
        x: 0,
        y: 0,
        zoom: 32
    }));
}

// ---------------------------------------------------------------------------
// Fetch, decode, upload
// ---------------------------------------------------------------------------

/// **The proof that tiles actually render.**
///
/// Bytes are served over a real socket, decoded from a real PNG, uploaded to a
/// real [`egui::Context`], and read back from the texture-manager delta that a
/// render backend would consume — and every one of the sixteen pixels is
/// compared against what was served.
///
/// Nothing here can pass on a type that merely exists.
#[test]
fn a_fetched_tile_reaches_an_egui_texture_with_the_pixels_that_were_served() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    let tile_id = TileId {
        x: 2,
        y: 3,
        zoom: 4,
    };
    let piece = tile_eventually(&mut tiles, tile_id);

    assert_eq!(
        piece.uv,
        whole_tile_uv(),
        "a tile fetched at its own zoom is drawn whole, not as a piece of an ancestor"
    );

    let Tile::Raster(texture) = piece.tile;
    assert_eq!(
        texture.size(),
        [FIXTURE_SIDE as usize; 2],
        "the texture is not the size of the image that was served"
    );

    let image = uploaded_pixels(&ctx, &texture);
    assert_eq!(image.size, [FIXTURE_SIDE as usize; 2]);

    for y in 0..FIXTURE_SIDE {
        for x in 0..FIXTURE_SIDE {
            let [r, g, b, _] = fixture_pixel(x, y);
            assert_eq!(
                image[(x as usize, y as usize)],
                egui::Color32::from_rgb(r, g, b),
                "pixel ({x}, {y}) did not survive fetch -> decode -> texture upload"
            );
        }
    }
}

/// The fetcher asks for the URL the source produced, byte for byte.
#[test]
fn the_fetcher_requests_the_url_the_source_built() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    // All three coordinates differ, so a transposed path fails.
    let tile_id = TileId {
        x: 6,
        y: 9,
        zoom: 5,
    };
    tile_eventually(&mut tiles, tile_id);

    assert_eq!(
        server.requests(),
        vec!["/5/6/9.png".to_owned()],
        "the fetcher did not request the source's URL"
    );
}

/// A tile already being downloaded is not downloaded again.
///
/// The server parks the connection, so the request stays pending for the whole
/// test and every later `at` call sees a tile that is neither cached nor failed
/// — the exact state the de-duplication exists for.
///
/// Without it each frame would issue a fresh request; the fetch loop would
/// accept up to `MAX_PARALLEL_DOWNLOADS` of them and the server would record
/// several.
#[test]
fn a_pending_tile_is_requested_only_once() {
    let server = TileServer::start(Behaviour::Hang);
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    let tile_id = TileId {
        x: 1,
        y: 1,
        zoom: 3,
    };

    // Drive until the request has demonstrably reached the server, so the count
    // below is measured after the interesting moment rather than before it.
    assert!(
        server.wait_for_requests(1, &mut || {
            tiles.at(tile_id);
        }),
        "the first request never reached the server"
    );

    stays_unavailable(&mut tiles, tile_id);

    assert_eq!(
        server.request_count(),
        1,
        "a pending tile was requested more than once: {:?}",
        server.requests()
    );
}

/// A tile that failed is not retried, and never becomes drawable.
///
/// Both failure shapes land in the same place: the cache keeps the `None` that
/// [`super::HttpsTiles::request_once`] inserted, which is simultaneously the
/// "already asked" marker. That is walkers' behaviour too — a 404 tile stays
/// blank rather than hammering the provider once per frame.
#[test]
fn a_tile_that_failed_is_not_requested_again() {
    for behaviour in [Behaviour::NotFound, Behaviour::Garbage] {
        let server = TileServer::start(behaviour);
        let ctx = Context::default();
        let mut tiles = loopback_tiles(&server, &ctx);

        let tile_id = TileId {
            x: 1,
            y: 2,
            zoom: 3,
        };

        assert!(
            server.wait_for_requests(1, &mut || {
                tiles.at(tile_id);
            }),
            "the first request never reached the server"
        );

        stays_unavailable(&mut tiles, tile_id);

        assert_eq!(
            server.request_count(),
            1,
            "a failed tile was re-requested: {:?}",
            server.requests()
        );
    }
}

/// A tile outside its zoom's grid is never requested.
///
/// The second half is what stops this being a test of a server that never sees
/// anything: the *same* source and the *same* server do record a valid tile.
#[test]
fn an_invalid_tile_is_never_requested_but_a_valid_one_is() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    // Zoom 0 has exactly one tile, at (0, 0).
    let invalid = TileId {
        x: 2,
        y: 2,
        zoom: 0,
    };
    stays_unavailable(&mut tiles, invalid);
    assert_eq!(
        server.request_count(),
        0,
        "an out-of-grid tile was requested: {:?}",
        server.requests()
    );

    let valid = TileId {
        x: 0,
        y: 0,
        zoom: 0,
    };
    tile_eventually(&mut tiles, valid);
    assert_eq!(
        server.requests(),
        vec!["/0/0/0.png".to_owned()],
        "the valid tile was not the only thing requested"
    );
}

/// Below the source's deepest zoom, the ancestor at `max_zoom` is fetched and
/// stretched.
///
/// Pins the clamp *and* the interpolation together: the requested path proves
/// the clamp happened, the `uv` proves the tile was located inside the ancestor
/// correctly.
#[test]
fn a_tile_deeper_than_the_source_supports_is_fetched_from_its_deepest_ancestor() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles = HttpsTiles::with_client(
        LoopbackSource::new(&server.base_url).with_max_zoom(3),
        ctx.clone(),
        loopback_client(),
    );

    // Zoom 5, four levels below max_zoom 3 -> a 4x4 subdivision.
    // x = 13 -> ancestor x 3, offset 1; y = 9 -> ancestor y 2, offset 1.
    let tile_id = TileId {
        x: 13,
        y: 9,
        zoom: 5,
    };
    let piece = tile_eventually(&mut tiles, tile_id);

    assert_eq!(
        server.requests(),
        vec!["/3/3/2.png".to_owned()],
        "the request was not clamped to the source's max zoom"
    );
    assert_eq!(
        piece.uv,
        egui::Rect::from_min_max(egui::pos2(0.25, 0.25), egui::pos2(0.5, 0.5)),
        "the tile was not located correctly inside its ancestor"
    );
}

/// A missing tile is drawn as a stretched piece of the nearest cached ancestor.
///
/// This is what keeps the map populated while panning and zooming instead of
/// flashing holes. The server answers exactly one path, so the descendant's own
/// download stays pending forever and the only way to return anything is to walk
/// outwards through the cache.
#[test]
fn a_cached_ancestor_is_stretched_over_a_tile_that_has_not_arrived() {
    let ancestor_id = TileId {
        x: 0,
        y: 0,
        zoom: 2,
    };
    let server = TileServer::start(Behaviour::ServeOnly {
        path: "/2/0/0.png".to_owned(),
        body: Arc::new(fixture_png()),
    });
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    let ancestor = tile_eventually(&mut tiles, ancestor_id);
    let Tile::Raster(ancestor_texture) = ancestor.tile;

    // Two levels deeper, inside that ancestor: x = 1 -> offset 1 of 4,
    // y = 1 -> offset 1 of 4.
    let descendant_id = TileId {
        x: 1,
        y: 1,
        zoom: 4,
    };
    let piece = pump_until(DEFAULT_TIMEOUT, || tiles.at(descendant_id))
        .expect("the ancestor should have stood in for the missing tile");

    let Tile::Raster(texture) = piece.tile;
    assert_eq!(
        texture.id(),
        ancestor_texture.id(),
        "the tile drawn was not the cached ancestor's texture"
    );
    assert_eq!(
        piece.uv,
        egui::Rect::from_min_max(egui::pos2(0.25, 0.25), egui::pos2(0.5, 0.5)),
        "the wrong part of the ancestor was selected"
    );
}

/// `tile_size` comes from the source rather than a constant.
///
/// walkers' flood fill multiplies by this to lay tiles out, so a wrong value is
/// a misaligned map rather than a missing one. The 512 case is what rules out a
/// hard-coded 256; the CartoDB case pins the value actually shipped.
#[test]
fn tile_size_comes_from_the_source() {
    let server = TileServer::start(Behaviour::Hang);
    let ctx = Context::default();

    let unusual = HttpsTiles::with_client(
        LoopbackSource::new(&server.base_url).with_tile_size(512),
        ctx.clone(),
        loopback_client(),
    );
    assert_eq!(Tiles::tile_size(&unusual), 512);

    let cartodb = HttpsTiles::new(CartoDb::light(), ctx);
    assert_eq!(
        Tiles::tile_size(&cartodb),
        256,
        "CartoDB serves 256px tiles"
    );
}

/// An arriving tile wakes the UI.
///
/// Without the `request_repaint`, a fetched tile sits in the channel until some
/// unrelated input causes a frame, and the map appears to stop loading.
///
/// The first half establishes that the flag is genuinely `false` beforehand and
/// that calling `at` does not set it on its own — otherwise the second half
/// would pass against a context that always reports `true`.
#[test]
fn an_arriving_tile_requests_a_repaint() {
    // No tile can arrive, so nothing should ask for a repaint.
    let stalled = TileServer::start(Behaviour::Hang);
    let quiet_ctx = Context::default();
    settle_repaints(&quiet_ctx);
    let mut stalled_tiles = loopback_tiles(&stalled, &quiet_ctx);

    // "No repaint" has to mean "no tile arrived", not "nothing happened at all",
    // so prove the fetch genuinely started first.
    assert!(
        stalled.wait_for_requests(1, &mut || {
            stalled_tiles.at(TileId {
                x: 1,
                y: 1,
                zoom: 3,
            });
        }),
        "the stalled fetch never reached the server"
    );
    stays_unavailable(
        &mut stalled_tiles,
        TileId {
            x: 1,
            y: 1,
            zoom: 3,
        },
    );
    assert!(
        !quiet_ctx.has_requested_repaint(),
        "a repaint was requested although no tile ever arrived"
    );

    // A tile does arrive, so one should.
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    settle_repaints(&ctx);
    let mut tiles = loopback_tiles(&server, &ctx);

    tile_eventually(
        &mut tiles,
        TileId {
            x: 1,
            y: 1,
            zoom: 3,
        },
    );
    assert!(
        pump_until(DEFAULT_TIMEOUT, || ctx
            .has_requested_repaint()
            .then_some(()))
        .is_some(),
        "a tile arrived but no repaint was requested"
    );
}

/// No more than [`super::MAX_PARALLEL_DOWNLOADS`] downloads are in flight.
///
/// Tile providers throttle or ban clients that exceed their limits, so this is a
/// term of use rather than a tuning choice — and it is invisible to every other
/// test here, all of which stay well under the cap.
///
/// The server parks every connection, so nothing ever completes and the number
/// of requests it has seen *is* the number in flight. Far more distinct tiles are
/// asked for than the limit allows, so a raised limit shows up as a higher count
/// rather than as no difference at all.
#[test]
fn no_more_than_the_concurrency_limit_is_downloaded_at_once() {
    let server = TileServer::start(Behaviour::Hang);
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    let wanted = (EXPECTED_PARALLEL_DOWNLOADS * 12) as u32;
    let mut ask = || {
        for x in 0..wanted {
            tiles.at(TileId { x, y: 0, zoom: 8 });
        }
    };

    assert!(
        server.wait_for_requests(EXPECTED_PARALLEL_DOWNLOADS, &mut ask),
        "the downloads never ramped up to the limit"
    );

    // Keep asking for all of them; the count must not climb past the limit.
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        ask();
        std::thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(
        server.request_count(),
        EXPECTED_PARALLEL_DOWNLOADS,
        "more downloads were started at once than the limit allows"
    );
}

/// The cache tiers and the concurrency limit are the written figures.
///
/// This is the one place the constants are compared to a literal, which is what
/// keeps every other test that spends them from being self-fulfilling. All
/// three cache arms are checked, not just the one this target compiles into
/// [`TILE_CACHE_ENTRIES`] — the arms have names precisely so the tiers a
/// desktop test run never activates cannot drift unobserved.
///
/// The cfg-gated assertion at the end restates the cascade's selection rule in
/// independent text: it cannot catch a wrong *value* (the literals above do
/// that), but it does catch the cascade wiring an arm to the wrong target —
/// swapped arms fail here on whichever target the tests run on.
#[test]
fn the_tuning_constants_are_the_written_figures_on_every_tier() {
    assert_eq!(
        MAX_PARALLEL_DOWNLOADS, EXPECTED_PARALLEL_DOWNLOADS,
        "the parallel-download limit is a provider term of use, not a dial"
    );
    assert_eq!(
        DESKTOP_TILE_CACHE_ENTRIES.get(),
        EXPECTED_CACHE_ENTRIES,
        "the desktop arm is walkers' own figure"
    );
    assert_eq!(
        MOBILE_TILE_CACHE_ENTRIES.get(),
        EXPECTED_MOBILE_CACHE_ENTRIES,
        "the mobile arm is half the desktop figure"
    );
    assert_eq!(
        WASM_TILE_CACHE_ENTRIES.get(),
        EXPECTED_WASM_CACHE_ENTRIES,
        "the wasm arm is a quarter of the desktop figure"
    );

    #[cfg(all(
        not(target_arch = "wasm32"),
        not(any(target_os = "android", target_os = "ios"))
    ))]
    assert_eq!(
        TILE_CACHE_ENTRIES, DESKTOP_TILE_CACHE_ENTRIES,
        "a desktop target must carry the desktop arm"
    );
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(target_os = "android", target_os = "ios")
    ))]
    assert_eq!(
        TILE_CACHE_ENTRIES, MOBILE_TILE_CACHE_ENTRIES,
        "a handheld target must carry the mobile arm"
    );
    #[cfg(target_arch = "wasm32")]
    assert_eq!(
        TILE_CACHE_ENTRIES, WASM_TILE_CACHE_ENTRIES,
        "wasm32 must carry the wasm arm"
    );
}

/// The cache stops growing at its bound.
///
/// This is a wiring check rather than a test of LRU itself: what it pins is that
/// [`super::TILE_CACHE_ENTRIES`] is the bound actually in force, so an
/// unbounded map cannot be substituted for the cache. Failures answer quickly,
/// which is what keeps requests flowing while the cache fills.
#[test]
fn the_tile_cache_is_bounded() {
    let server = TileServer::start(Behaviour::NotFound);
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    let capacity = EXPECTED_CACHE_ENTRIES;
    let attempts = capacity as u32 + 64;

    let reached = pump_until(DEFAULT_TIMEOUT, || {
        for x in 0..attempts {
            tiles.at(TileId { x, y: 0, zoom: 10 });
        }
        (tiles.cached_entries() >= capacity).then(|| tiles.cached_entries())
    });

    assert_eq!(
        reached,
        Some(capacity),
        "the cache did not settle at its bound"
    );
}

/// Eviction is exercised at every tier's cap, by recency, not only compiled in.
///
/// `cargo test` runs on one arm of [`super::TILE_CACHE_ENTRIES`] — the desktop
/// one — so the mobile and wasm bounds would otherwise ship as numbers no test
/// had ever driven a cache to. The bound arrives through
/// [`loopback_tiles_with_capacity`]; what is under test is the wiring (the
/// handed-in bound is the one in force) and the direction (the victim is the
/// least recently touched id, so a touch is what protects an entry).
///
/// Every count is exact and read back, never merely "at most the cap": a fill
/// below the cap must evict *nothing* (the paired negative control — an empty
/// or pass-through cache fails the very first count), and the one insert at
/// the cap must remove exactly the LRU id while the just-touched oldest id
/// survives.
#[test]
fn eviction_holds_at_every_tier_cap_and_takes_the_least_recent() {
    for capacity in [
        EXPECTED_WASM_CACHE_ENTRIES,
        EXPECTED_MOBILE_CACHE_ENTRIES,
        EXPECTED_CACHE_ENTRIES,
    ] {
        let server = TileServer::start(Behaviour::NotFound);
        let ctx = Context::default();
        let mut tiles = loopback_tiles_with_capacity(&server, &ctx, capacity);
        let id = |x: u32| TileId { x, y: 0, zoom: 10 };

        // The counter moves before any bound is near: three ids are three
        // entries, exactly.
        let reached = pump_until(DEFAULT_TIMEOUT, || {
            for x in 0..3 {
                tiles.at(id(x));
            }
            (tiles.cached_entries() >= 3).then(|| tiles.cached_entries())
        });
        assert_eq!(
            reached,
            Some(3),
            "three requested ids must be three entries at cap {capacity}"
        );

        // Fill to exactly the cap. Below-or-at the bound nothing may be
        // evicted, so the exact count doubles as total membership.
        let reached = pump_until(DEFAULT_TIMEOUT, || {
            for x in 0..capacity as u32 {
                tiles.at(id(x));
            }
            (tiles.cached_entries() >= capacity).then(|| tiles.cached_entries())
        });
        assert_eq!(
            reached,
            Some(capacity),
            "a fill to the cap ({capacity}) must evict nothing"
        );
        assert!(
            tiles.tile_is_cached(id(0)) && tiles.tile_is_cached(id(capacity as u32 - 1)),
            "both ends of a to-the-cap fill must be resident at {capacity}"
        );

        // The last full fill pass touched 0..capacity in ascending order, so
        // the recency order is known. Touch the oldest id: the next victim
        // must now be x=1, not x=0.
        tiles.at(id(0));

        // One insert at the cap: admitted, and paid for by exactly the LRU id.
        let new = id(capacity as u32);
        let admitted = pump_until(DEFAULT_TIMEOUT, || {
            tiles.at(new);
            tiles.tile_is_cached(new).then_some(())
        });
        assert!(
            admitted.is_some(),
            "the at-cap insert was never admitted at {capacity}"
        );
        assert_eq!(
            tiles.cached_entries(),
            capacity,
            "an at-cap insert must not grow the cache past {capacity}"
        );
        assert!(
            !tiles.tile_is_cached(id(1)),
            "the least recently touched id must be the one evicted at {capacity}"
        );
        assert!(
            tiles.tile_is_cached(id(0)),
            "the touch must be what protected the oldest id at {capacity}"
        );
        assert!(
            tiles.tile_is_cached(id(capacity as u32 - 1)),
            "an id younger than the victim must survive at {capacity}"
        );
    }
}

// ---------------------------------------------------------------------------
// The client's TLS posture
// ---------------------------------------------------------------------------

/// Tile traffic cannot fall back to cleartext.
///
/// This is the `https_only(true)` that [`rustdar_radar::tls::client`] sets,
/// observed behaviourally because `reqwest::ClientBuilder` exposes no getter for
/// it. It is also the load-bearing check that [`super::tile_client`] really is
/// that function: a bare `reqwest::Client::builder()` would happily make this
/// request.
///
/// Nothing is listening on port 1, and the rejection happens before any socket
/// is opened, so this is offline-safe either way.
#[tokio::test]
async fn the_tile_client_refuses_cleartext() {
    let error = tile_client()
        .get("http://127.0.0.1:1/light_nolabels/0/0/0.png")
        .send()
        .await
        .expect_err("a cleartext tile URL must not be fetched");

    assert!(
        error.is_builder(),
        "the request failed, but not at the https_only scheme check: {error}"
    );
}

/// The same client does *not* reject `https://`.
///
/// Without this, a permanently broken client would satisfy the test above.
#[tokio::test]
async fn the_tile_client_accepts_https() {
    let error = tile_client()
        .get("https://127.0.0.1:1/light_nolabels/0/0/0.png")
        .send()
        .await
        .expect_err("nothing listens on port 1, so the connection must fail");

    assert!(
        !error.is_builder(),
        "an https:// tile URL was rejected before any connection was attempted: {error}"
    );
    assert!(
        error.is_connect(),
        "expected a connection failure, got: {error}"
    );
}

// ---------------------------------------------------------------------------
// Live
// ---------------------------------------------------------------------------

/// End-to-end against the real CDN over real TLS.
///
/// Uses [`HttpsTiles::new`], so the client is [`super::tile_client`] and nothing
/// is stubbed: rustls-platform-verifier validates CartoDB's chain against the
/// operating system's trust store, *ring* carries the handshake, and the PNG
/// that comes back is decoded and uploaded through the same path the map uses.
///
/// If the platform verifier were not wired up — or if some future change
/// reintroduced a bundled root store — this is the test that would notice.
///
/// Run with:
///   `cargo test -p rustdar-egui --lib -- --ignored --nocapture live_`
#[ignore = "hits the live CartoDB basemap CDN over HTTPS"]
#[test]
fn live_cartodb_tile_decodes_and_reaches_a_texture() {
    let ctx = Context::default();
    let mut tiles = HttpsTiles::new(CartoDb::light(), ctx.clone());

    // Pins down *which* provider carried the handshake. aws-lc-rs is still in the
    // graph via nexrad-data, so without this the test would pass just as happily
    // on the fallback provider.
    assert!(
        rustdar_radar::tls::default_is_ring(),
        "the handshake would not be carried by ring"
    );

    let tile_id = TileId {
        x: 0,
        y: 0,
        zoom: 0,
    };
    println!("fetching {}", CartoDb::light().tile_url(tile_id));

    let piece = pump_until(LIVE_TIMEOUT, || tiles.at(tile_id))
        .expect("the world tile should download from CartoDB");

    let Tile::Raster(texture) = piece.tile;
    assert_eq!(
        texture.size(),
        [256, 256],
        "CartoDB serves 256px tiles; got {:?}",
        texture.size()
    );

    let image = uploaded_pixels(&ctx, &texture);
    println!("decoded {:?} pixels", image.size);
    assert!(
        image.pixels.iter().any(|pixel| *pixel != image.pixels[0]),
        "the decoded tile is a single flat colour, which is not a basemap"
    );
}
