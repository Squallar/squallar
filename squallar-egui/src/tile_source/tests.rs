//! Tests for [`super`].

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::Context;
use walkers::sources::{Attribution, TileSource};
use walkers::{Style, Tile, TileId, TilePiece, Tiles};

use super::{
    DESKTOP_PARSED_TILE_CACHE_ENTRIES, DESKTOP_TILE_CACHE_ENTRIES, DecodeBudget, HttpsTiles,
    MAX_PARALLEL_DOWNLOADS, MOBILE_PARSED_TILE_CACHE_ENTRIES, MOBILE_TILE_CACHE_ENTRIES,
    PARSED_TILE_CACHE_ENTRIES, TILE_CACHE_ENTRIES, WASM_PARSED_TILE_CACHE_ENTRIES,
    WASM_TILE_CACHE_ENTRIES, WASM_TILE_DECODES_PER_PUMP, drain_up_to, interpolate_from_lower_zoom,
    tile_client, tile_id_is_valid,
};

// ---------------------------------------------------------------------------

/// Side length of the PNG fixture, in pixels.
const FIXTURE_SIDE: u32 = 4;

/// The colour of one fixture pixel.
fn fixture_pixel(x: u32, y: u32) -> [u8; 4] {
    [
        (17 + x * 40) as u8,
        (3 + y * 50) as u8,
        (200 - x * 7 - y * 29) as u8,
        255,
    ]
}

/// The fixture, encoded as a real PNG.
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
const EXPECTED_CACHE_ENTRIES: usize = 256;
const EXPECTED_MOBILE_CACHE_ENTRIES: usize = 96;
/// Not a fraction of the desktop figure: the worst-case working set of a
/// 1920x1200-point canvas, which `tiles::tests` measures at exactly this.
const EXPECTED_WASM_CACHE_ENTRIES: usize = 96;
const EXPECTED_PARALLEL_DOWNLOADS: usize = 6;
const EXPECTED_WASM_DECODES_PER_PUMP: usize = 2;
/// The 1920x1200 worst-case working set, the same derivation as the wasm
/// styled arm's: the common canvas restyles wholly from the parsed cache.
const EXPECTED_DESKTOP_PARSED_ENTRIES: usize = 96;
/// A byte-budget answer, below the working set on purpose: an evicted parse
/// costs one refetch on the next restyle, never a frame.
const EXPECTED_HANDHELD_PARSED_ENTRIES: usize = 24;

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
    Hang,
    /// `200` with the body for one exact path; [`Behaviour::Hang`] for the rest.
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

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long "and it stays that way" observations watch for.
const SETTLE: Duration = Duration::from_millis(300);

/// A client that can reach the cleartext loopback server.
fn loopback_client() -> reqwest::Client {
    squallar_radar::tls::init();
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

/// One layer's worth of drawing, for a single-tile layer: the pump
/// `ui_map_overlays::draw_tile_layer` runs once before its grid loop, then the
/// one cell's `at`. [`HttpsTiles::at`] does not drain, so every loop below that
/// waits for a tile to *arrive* has to go through the pump the drawing code
/// goes through.
fn draw_one(tiles: &mut HttpsTiles, tile_id: TileId) -> Option<TilePiece> {
    tiles.pump();
    tiles.at(tile_id)
}

/// Drive `tiles` until `tile_id` is drawable.
fn tile_eventually(tiles: &mut HttpsTiles, tile_id: TileId) -> TilePiece {
    pump_until(DEFAULT_TIMEOUT, || draw_one(tiles, tile_id))
        .unwrap_or_else(|| panic!("{tile_id:?} never became available"))
}

/// Keep drawing for [`SETTLE`], asserting the tile never yields.
fn stays_unavailable(tiles: &mut HttpsTiles, tile_id: TileId) {
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        assert!(
            draw_one(tiles, tile_id).is_none(),
            "{tile_id:?} became available when it should not have"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Run empty egui passes until the context stops asking for repaints by itself.
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

/// The attribution the map displays is the source's, not a placeholder.
#[test]
fn attribution_reaches_the_tiles_trait() {
    let server = TileServer::start(Behaviour::Hang);
    let tiles = HttpsTiles::with_client(
        LoopbackSource::new(&server.base_url),
        Context::default(),
        loopback_client(),
    );

    let attribution = Tiles::attribution(&tiles);
    assert_eq!(attribution.text, "loopback");
    assert_eq!(attribution.url, "http://127.0.0.1/");
}

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

/// **The proof that tiles actually render.**
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

    let Tile::Raster(texture) = piece.tile else {
        panic!("the tile source fetches PNGs; it never produces a vector tile");
    };
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
    let Tile::Raster(ancestor_texture) = ancestor.tile else {
        panic!("the tile source fetches PNGs; it never produces a vector tile");
    };

    // Two levels deeper, inside that ancestor: x = 1 -> offset 1 of 4,
    // y = 1 -> offset 1 of 4.
    let descendant_id = TileId {
        x: 1,
        y: 1,
        zoom: 4,
    };
    let piece = pump_until(DEFAULT_TIMEOUT, || draw_one(&mut tiles, descendant_id))
        .expect("the ancestor should have stood in for the missing tile");

    let Tile::Raster(texture) = piece.tile else {
        panic!("the tile source fetches PNGs; it never produces a vector tile");
    };
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

    let ordinary = HttpsTiles::with_client(
        LoopbackSource::new(&server.base_url),
        ctx,
        loopback_client(),
    );
    assert_eq!(
        Tiles::tile_size(&ordinary),
        256,
        "the slippy-map default is 256px tiles"
    );
}

/// An arriving tile wakes the UI.
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
#[test]
fn no_more_than_the_concurrency_limit_is_downloaded_at_once() {
    let server = TileServer::start(Behaviour::Hang);
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    let wanted = (EXPECTED_PARALLEL_DOWNLOADS * 12) as u32;
    let mut ask = || {
        tiles.pump();
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
#[test]
fn the_tuning_constants_are_the_written_figures_on_every_tier() {
    assert_eq!(
        MAX_PARALLEL_DOWNLOADS, EXPECTED_PARALLEL_DOWNLOADS,
        "the parallel-download limit is a provider term of use, not a dial"
    );
    assert_eq!(
        DESKTOP_TILE_CACHE_ENTRIES.get(),
        EXPECTED_CACHE_ENTRIES,
        "the desktop arm is pinned from below by squallar_gpu's \
         MIRROR_SCALE_MAX, not by the byte budget"
    );
    assert_eq!(
        MOBILE_TILE_CACHE_ENTRIES.get(),
        EXPECTED_MOBILE_CACHE_ENTRIES,
        "the mobile arm is the largest handheld working set, not a fraction \
         of the desktop figure"
    );
    assert_eq!(
        WASM_TILE_CACHE_ENTRIES.get(),
        EXPECTED_WASM_CACHE_ENTRIES,
        "the wasm arm is the worst-case working set of a 1920x1200-point \
         canvas, not a fraction of the desktop figure"
    );
    assert_eq!(
        WASM_TILE_CACHE_ENTRIES.get(),
        crate::tiles::tiles_resident_for(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1200.0)),
            0,
            1,
        ),
        "the wasm arm must stay derived from what a viewport actually keeps \
         resident, worst case over the whole zoom range"
    );
    assert!(
        WASM_TILE_CACHE_ENTRIES.get() <= MOBILE_TILE_CACHE_ENTRIES.get()
            && MOBILE_TILE_CACHE_ENTRIES.get() <= DESKTOP_TILE_CACHE_ENTRIES.get(),
        "the tiers must stay ordered by how much memory the class has"
    );
    assert_eq!(
        WASM_TILE_DECODES_PER_PUMP, EXPECTED_WASM_DECODES_PER_PUMP,
        "the decode allowance is two tiles per source per pass"
    );

    // The two arms that can hold a vector entry must be derived against the
    // vector entry, which is what the doc comment claims and what stopped being
    // true when the entry stopped being a 256 KiB texture. A ceiling rather than
    // an equality: the number is also floored by the working set, so it is not a
    // pure function of the byte budget.
    for (entries, ceiling_mib, tier) in [
        (DESKTOP_TILE_CACHE_ENTRIES.get(), 160, "desktop"),
        (MOBILE_TILE_CACHE_ENTRIES.get(), 64, "mobile"),
        (WASM_TILE_CACHE_ENTRIES.get(), 64, "wasm32"),
    ] {
        let worst = entries * super::MEASURED_VECTOR_TILE_BYTES;
        assert!(
            worst <= ceiling_mib * 1024 * 1024,
            "the {tier} arm holds {entries} entries, which is {:.1} MiB of \
             vector tiles per source against a {ceiling_mib} MiB ceiling",
            worst as f64 / (1024.0 * 1024.0)
        );
    }

    // The parsed-geometry population: the second resident cost a style
    // change rides. The desktop arm is the same working-set derivation the
    // wasm styled arm carries; the handheld arms are deliberately below the
    // working set (an evicted parse is one refetch on the next restyle, not a
    // frame), so they are pinned as written figures plus a byte ceiling.
    assert_eq!(
        DESKTOP_PARSED_TILE_CACHE_ENTRIES.get(),
        EXPECTED_DESKTOP_PARSED_ENTRIES,
        "the desktop parsed arm is the 1920x1200 working set"
    );
    assert_eq!(
        DESKTOP_PARSED_TILE_CACHE_ENTRIES.get(),
        crate::tiles::tiles_resident_for(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1200.0)),
            0,
            1,
        ),
        "the desktop parsed arm must stay derived from what a viewport keeps \
         resident, worst case over the whole zoom range"
    );
    assert_eq!(
        MOBILE_PARSED_TILE_CACHE_ENTRIES.get(),
        EXPECTED_HANDHELD_PARSED_ENTRIES,
        "the mobile parsed arm is a byte-budget figure"
    );
    assert_eq!(
        WASM_PARSED_TILE_CACHE_ENTRIES.get(),
        EXPECTED_HANDHELD_PARSED_ENTRIES,
        "the wasm parsed arm is a byte-budget figure"
    );
    for (entries, ceiling_mib, tier) in [
        (DESKTOP_PARSED_TILE_CACHE_ENTRIES.get(), 192, "desktop"),
        (MOBILE_PARSED_TILE_CACHE_ENTRIES.get(), 48, "mobile"),
        (WASM_PARSED_TILE_CACHE_ENTRIES.get(), 48, "wasm32"),
    ] {
        let worst = entries * super::MEASURED_PARSED_TILE_BYTES;
        assert!(
            worst <= ceiling_mib * 1024 * 1024,
            "the {tier} parsed arm holds {entries} entries, which is {:.1} MiB \
             of parsed tiles per source against a {ceiling_mib} MiB ceiling",
            worst as f64 / (1024.0 * 1024.0)
        );
    }
    for (parsed, styled, tier) in [
        (
            DESKTOP_PARSED_TILE_CACHE_ENTRIES,
            DESKTOP_TILE_CACHE_ENTRIES,
            "desktop",
        ),
        (
            MOBILE_PARSED_TILE_CACHE_ENTRIES,
            MOBILE_TILE_CACHE_ENTRIES,
            "mobile",
        ),
        (
            WASM_PARSED_TILE_CACHE_ENTRIES,
            WASM_TILE_CACHE_ENTRIES,
            "wasm32",
        ),
    ] {
        assert!(
            parsed.get() <= styled.get(),
            "the {tier} parsed arm outgrew the styled arm: an economy cache \
             holding parses for tiles the styled cache cannot keep is residency \
             with no consumer"
        );
    }
    assert!(
        WASM_PARSED_TILE_CACHE_ENTRIES.get() <= MOBILE_PARSED_TILE_CACHE_ENTRIES.get()
            && MOBILE_PARSED_TILE_CACHE_ENTRIES.get() <= DESKTOP_PARSED_TILE_CACHE_ENTRIES.get(),
        "the parsed tiers must stay ordered by how much memory the class has"
    );
    #[cfg(all(
        not(target_arch = "wasm32"),
        not(any(target_os = "android", target_os = "ios"))
    ))]
    assert_eq!(
        PARSED_TILE_CACHE_ENTRIES, DESKTOP_PARSED_TILE_CACHE_ENTRIES,
        "a desktop target must carry the desktop parsed arm"
    );
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(target_os = "android", target_os = "ios")
    ))]
    assert_eq!(
        PARSED_TILE_CACHE_ENTRIES, MOBILE_PARSED_TILE_CACHE_ENTRIES,
        "a handheld target must carry the mobile parsed arm"
    );
    #[cfg(target_arch = "wasm32")]
    assert_eq!(
        PARSED_TILE_CACHE_ENTRIES, WASM_PARSED_TILE_CACHE_ENTRIES,
        "wasm32 must carry the wasm parsed arm"
    );

    // The wasm arm holds vector entries as of `feat(web): the vector basemap
    // draws on wasm32`, so it is in the loop above rather than exempt from it.
    // Its raster derivation is still pinned, because the terrain slot holds
    // raster entries and the arm has to be right for it too.
    assert_eq!(
        WASM_TILE_CACHE_ENTRIES.get() * super::RASTER_TILE_BYTES,
        24 * 1024 * 1024,
        "the wasm arm's derivation is against the raster entry and moved"
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

/// The measured vector-entry cost is a real measurement of a real tile, not a
/// number that drifted into a doc comment.
///
/// **Skips rather than reddens without the fixture**, like every other test
/// here that reads it. What it cannot do is pass vacuously: a rendered tile
/// that is trivially small would fail the floor below.
#[test]
fn the_vector_entry_cost_is_what_the_fixture_actually_renders() {
    use std::path::PathBuf;

    use crate::basemap_archive::FileRangeSource;

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/monaco.pmtiles");
    if FileRangeSource::open(&path).is_err() {
        eprintln!(
            "SKIPPED the_vector_entry_cost_is_what_the_fixture_actually_renders: \
             {} would not open. It is committed; `git status` on it.",
            path.display()
        );
        return;
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");

    let shapes = runtime.block_on(async {
        let archive = crate::basemap_archive::BasemapArchive::open(
            FileRangeSource::open(&path).expect("the fixture opens"),
        )
        .await
        .expect("the fixture is a PMTiles archive");

        // Monaco's own z14 tile: the densest the fixture holds, and the tail
        // the cache has to be sized for. Not the header's declared centre,
        // which is open Mediterranean -- see `archive::MONACO_LON`.
        let bytes = archive
            .tile(14, 8529, 5974)
            .await
            .expect("the tile reads")
            .into_bytes()
            .expect("the fixture holds Monaco's own z14 tile");

        match Tile::from_mvt(&bytes, &crate::basemap_style::committed(true), 14)
            .expect("the tile renders")
        {
            Tile::Vector(shapes) => shapes,
            Tile::Raster(_) => panic!("an MVT body rendered as a raster"),
        }
    });

    // A non-triviality floor: a tile that rendered nothing would sail through
    // the bound below and prove nothing about the cost of a real one.
    assert!(
        shapes.len() > 500,
        "the fixture's densest tile rendered only {} shapes, so this is not \
         measuring a city core any more",
        shapes.len()
    );

    let heap: usize = std::mem::size_of_val(&shapes[..])
        + shapes
            .iter()
            .map(|shape| match shape {
                walkers::ShapeOrText::Shape(egui::Shape::Mesh(mesh)) => {
                    std::mem::size_of::<egui::Mesh>()
                        + mesh.vertices.capacity() * std::mem::size_of::<egui::epaint::Vertex>()
                        + mesh.indices.capacity() * 4
                }
                walkers::ShapeOrText::Shape(egui::Shape::Path(path)) => {
                    std::mem::size_of::<egui::epaint::PathShape>()
                        + path.points.capacity() * std::mem::size_of::<egui::Pos2>()
                }
                walkers::ShapeOrText::Text(text) => text.text.capacity(),
                walkers::ShapeOrText::Shape(_) => 0,
            })
            .sum::<usize>();

    // Not an equality: `size_of` and allocator rounding are toolchain
    // properties, and pinning them would red-gate a compiler bump. The claim
    // that matters is that the constant the cache is sized against is not an
    // *under*-estimate, and is still the right order.
    assert!(
        heap <= super::MEASURED_VECTOR_TILE_BYTES,
        "one cached vector entry measures {heap} bytes, over the \
         {} the cache sizing is derived from",
        super::MEASURED_VECTOR_TILE_BYTES
    );
    assert!(
        heap * 2 >= super::MEASURED_VECTOR_TILE_BYTES,
        "one cached vector entry measures {heap} bytes, less than half the \
         {} the cache sizing is derived from: the derivation has gone stale in \
         the safe direction, which still means it is not measuring this",
        super::MEASURED_VECTOR_TILE_BYTES
    );
}

/// The measured parsed-entry cost is a real measurement of a real tile — the
/// styled test's twin for the second resident population.
///
/// Skips without the fixture, like its twin, and cannot pass vacuously: the
/// floor below rejects a parse that decoded nothing. The band is the
/// derivation of [`super::MEASURED_PARSED_TILE_BYTES`]; re-derive the constant
/// by forcing it to fail, never by inference — the band cannot catch the
/// constant drifting upward into a safe over-estimate.
#[test]
fn the_parsed_entry_cost_is_what_the_fixture_actually_parses() {
    use std::path::PathBuf;

    use crate::basemap_archive::FileRangeSource;

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/monaco.pmtiles");
    if FileRangeSource::open(&path).is_err() {
        eprintln!(
            "SKIPPED the_parsed_entry_cost_is_what_the_fixture_actually_parses: \
             {} would not open. It is committed; `git status` on it.",
            path.display()
        );
        return;
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");

    let bytes = runtime.block_on(async {
        let archive = crate::basemap_archive::BasemapArchive::open(
            FileRangeSource::open(&path).expect("the fixture opens"),
        )
        .await
        .expect("the fixture is a PMTiles archive");

        // The same tile the styled figure is measured on: Monaco's own z14
        // city core, the tail the cache is sized for.
        archive
            .tile(14, 8529, 5974)
            .await
            .expect("the tile reads")
            .into_bytes()
            .expect("the fixture holds Monaco's own z14 tile")
    });

    let parsed = walkers::mvt::parse(&bytes).expect("the tile parses");

    // A non-triviality floor, against the same fixture facts the styled test
    // uses: a city-core tile is thousands of features, and a parse whose heap
    // sits below the styled entry is not holding them.
    let heap = parsed.heap_bytes();
    assert!(
        heap > super::MEASURED_VECTOR_TILE_BYTES / 2,
        "the parse of the fixture's densest tile measures {heap} bytes, which \
         is too small to be the decode of a city core"
    );

    assert!(
        heap <= super::MEASURED_PARSED_TILE_BYTES,
        "one parsed entry measures {heap} bytes, over the \
         {} the parsed-cache sizing is derived from",
        super::MEASURED_PARSED_TILE_BYTES
    );
    assert!(
        heap * 2 >= super::MEASURED_PARSED_TILE_BYTES,
        "one parsed entry measures {heap} bytes, less than half the \
         {} the parsed-cache sizing is derived from: the derivation has gone \
         stale in the safe direction, which still means it is not measuring this",
        super::MEASURED_PARSED_TILE_BYTES
    );
}

/// The cache stops growing at its bound.
#[test]
fn the_tile_cache_is_bounded() {
    let server = TileServer::start(Behaviour::NotFound);
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    let capacity = EXPECTED_CACHE_ENTRIES;
    let attempts = capacity as u32 + 64;

    let reached = pump_until(DEFAULT_TIMEOUT, || {
        tiles.pump();
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
            tiles.pump();
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
            tiles.pump();
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
            tiles.pump();
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

/// One pump decodes at most the budget; the rest wait their turn, in order.
#[test]
fn a_pump_decodes_at_most_the_budget_and_the_rest_wait_their_turn() {
    let ctx = Context::default();
    let png = fixture_png();
    let id = |x: u32| TileId { x, y: 0, zoom: 3 };

    // More completed fetches than one pump may take, or the cap is untested.
    let queued: u32 = 5;
    assert!(
        (queued as usize) > WASM_TILE_DECODES_PER_PUMP,
        "the backlog must exceed the budget for the cap to be observable"
    );

    let (mut tx, mut rx) = futures::channel::mpsc::channel::<(TileId, Vec<u8>)>(queued as usize);
    for x in 0..queued {
        tx.try_send((id(x), png.clone()))
            .expect("the test channel should hold the whole backlog");
    }

    // The sink is the decode path: an item only counts once its bytes have
    // been decoded, so the counts cannot be satisfied by mere dequeueing.
    let mut decoded: Vec<TileId> = Vec::new();
    // The "IO task is gone" latch. This test never closes the channel, so it is
    // only here to satisfy the signature; `drain_up_to`'s own doc says why the
    // latch exists.
    let mut io_task_gone_reported = false;
    let pump = |rx: &mut futures::channel::mpsc::Receiver<(TileId, Vec<u8>)>,
                decoded: &mut Vec<TileId>,
                reported: &mut bool| {
        drain_up_to(
            rx,
            WASM_TILE_DECODES_PER_PUMP,
            reported,
            |(tile_id, bytes)| {
                // `Style::default()` for the reason `fetch_one` gives.
                #[allow(
                    clippy::default_constructed_unit_structs,
                    reason = "keeps compiling if walkers/mvt is ever enabled"
                )]
                let tile = Tile::new(&bytes, &Style::default(), tile_id.zoom, &ctx)
                    .expect("the fixture PNG should decode");
                drop(tile);
                decoded.push(tile_id);
            },
        )
    };

    let first = pump(&mut rx, &mut decoded, &mut io_task_gone_reported);
    assert_eq!(
        first,
        decoded.len(),
        "the reported take and the decode count are the same events"
    );
    assert_eq!(
        decoded,
        [id(0), id(1)],
        "one pump decodes exactly the budget, in arrival order"
    );

    let second = pump(&mut rx, &mut decoded, &mut io_task_gone_reported);
    assert_eq!(second, 2, "a second pump takes the next two");
    assert_eq!(
        decoded,
        [id(0), id(1), id(2), id(3)],
        "the first pump left N - 2 queued, and nothing was lost or reordered"
    );

    let third = pump(&mut rx, &mut decoded, &mut io_task_gone_reported);
    assert_eq!(third, 1, "the tail is one tile, not a refilled budget");
    let fourth = pump(&mut rx, &mut decoded, &mut io_task_gone_reported);
    assert_eq!(fourth, 0, "an empty queue yields nothing");
    assert_eq!(
        decoded,
        [id(0), id(1), id(2), id(3), id(4)],
        "every queued tile was decoded exactly once"
    );
}

/// The allowance is per pass, not per call, and a new pass restores it.
#[test]
fn the_decode_allowance_is_per_pass_and_a_new_pass_restores_it() {
    let mut budget = DecodeBudget::new();

    assert_eq!(
        budget.remaining(1),
        WASM_TILE_DECODES_PER_PUMP,
        "a fresh pass starts with the whole allowance"
    );
    budget.record(WASM_TILE_DECODES_PER_PUMP);
    assert_eq!(
        budget.remaining(1),
        0,
        "a later call in the same pass gets nothing once the allowance is spent"
    );

    assert_eq!(
        budget.remaining(2),
        WASM_TILE_DECODES_PER_PUMP,
        "the next pass restores the full allowance"
    );
    budget.record(1);
    assert_eq!(
        budget.remaining(2),
        WASM_TILE_DECODES_PER_PUMP - 1,
        "a partial spend leaves exactly the difference for the same pass"
    );
}

// ---------------------------------------------------------------------------

/// Tile traffic cannot fall back to cleartext.
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

// ---------------------------------------------------------------------------
// The archive source
//
// Unconditional since the flip: the archive is THE basemap, so these run in
// every `cargo test -p squallar-egui` and every workspace suite. The seam
// they exercise -- `Tile::Vector` reaching the painter -- is also covered
// from the other side in `ui_map_overlays::tests`, without an archive.
// ---------------------------------------------------------------------------

/// The committed fixture, 419,355 bytes of Monaco.
///
/// A test that reaches the network is not a test. The published planet archive
/// is 83.88 GB and lives behind a CDN; this is 419 KB and lives in the tree.
mod archive {
    use std::num::NonZeroUsize;
    use std::path::PathBuf;

    use super::*;
    use crate::basemap_archive::FileRangeSource;

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/monaco.pmtiles")
    }

    /// The archive-backed source, over the committed fixture.
    ///
    /// Returns `None`, with a banner, when the fixture is not there — the same
    /// shape `basemap_archive::tests` uses, so a checkout without the fixture
    /// skips rather than reddening on something that is not a defect.
    fn fixture_tiles(test: &str, ctx: &Context, is_dark: bool) -> Option<HttpsTiles> {
        let path = fixture_path();
        let source = match FileRangeSource::open(&path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!(
                    "SKIPPED {test}: {} would not open ({error}). It is committed; \
                     `git status` on it.",
                    path.display()
                );
                return None;
            }
        };

        Some(HttpsTiles::from_range_source(
            source,
            crate::basemap_style::committed(is_dark),
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx.clone(),
            NonZeroUsize::new(64).expect("64 is not zero"),
            // Nothing declared: the header's word stands, which is what the
            // archives under test are read by.
            None,
            // No seed and no downloaded areas: these read the committed
            // fixture over a bare source, and the composition's own suite is
            // what covers the local-first walk.
            crate::tile_source::ArchiveStores {
                seed: None,
                offline: None::<crate::basemap_download::PlatformSegmentStore>,
                offline_archive: crate::basemap_download::AreaArchive::Basemap,
            },
        ))
    }

    /// Monaco itself.
    ///
    /// **Not the centre the archive's header declares.** That is the centroid
    /// of the extract's bounding box (lon 7.408583..7.595671, lat
    /// 43.483817..43.752930), which is 7.502127/43.618374 -- open Mediterranean,
    /// 13 km south-east of the city. Measured 2026-08-28: at zoom 14 that
    /// coordinate's tile is 164 bytes and renders **3** shapes, all of them the
    /// style's background rectangle and two coastline strokes, while Monaco's
    /// own is 185,182 bytes and renders **2,992**. A test anchored on the
    /// header centre would pass every "a tile came back" assertion on an
    /// essentially empty tile.
    const MONACO_LON: f64 = 7.424_6;
    const MONACO_LAT: f64 = 43.738_4;

    /// **First light, as a test.** A real tile out of a real PMTiles archive,
    /// rendered against a real committed style, arriving as drawable geometry.
    ///
    /// This is the whole seam in one: range reads, gunzip, MVT parse, style
    /// evaluation, tessellation, the LRU, and `Tiles::at`. Before it, nothing
    /// in this workspace had ever turned an archive byte into a shape.
    #[test]
    fn a_tile_from_the_archive_arrives_as_drawable_geometry() {
        let ctx = Context::default();
        let Some(mut tiles) = fixture_tiles(
            "a_tile_from_the_archive_arrives_as_drawable_geometry",
            &ctx,
            true,
        ) else {
            return;
        };

        // The header is read by the IO task, so the depth is not known yet --
        // and "not known" is `None`, not a number. See
        // `tile_source::MAX_ZOOM_UNKNOWN` for what a stand-in zero drew.
        assert_eq!(
            tiles.source_max_zoom(),
            None,
            "before the header lands the source must claim nothing it cannot serve"
        );

        let max_zoom = pump_until(DEFAULT_TIMEOUT, || {
            tiles.pump();
            tiles.source_max_zoom()
        })
        .expect("the archive header never arrived");

        let tile_id = TileId {
            x: squallar_geo::lon_to_tile_x(MONACO_LON, max_zoom),
            y: squallar_geo::lat_to_tile_y(MONACO_LAT, max_zoom),
            zoom: max_zoom,
        };

        let piece = pump_until(DEFAULT_TIMEOUT, || {
            let piece = draw_one(&mut tiles, tile_id)?;
            // `at` answers with an ancestor while the real tile is in flight.
            // Only the tile itself proves the archive read worked.
            (piece.uv == whole_tile_uv()).then_some(piece)
        })
        .expect("the archive never yielded Monaco's own tile");

        let Tile::Vector(shapes) = piece.tile else {
            panic!(
                "the archive holds gzipped MVT; a raster here means `Tile::new` \
                 guessed an image format for it"
            );
        };

        // NON-VACUITY. `mvt::render` returns `Ok(vec![])` for a style whose
        // layers match nothing, and an empty tile draws a blank pane while
        // every assertion above still passes. 92 style layers over an OMT tile
        // of a city centre is hundreds of shapes; the floor is far below that
        // and far above zero.
        // 2,992 measured 2026-08-28 on this fixture at this coordinate. The
        // floor is far below that -- a style edit must not redden this -- and
        // far above the 3 that the archive's *declared* centre renders, which
        // is the number a wrongly-anchored version of this test would sit on.
        assert!(
            shapes.len() > 200,
            "the archive tile rendered {} shapes. A tile that decodes and draws \
             nothing is the failure this asserts against, not a pass.",
            shapes.len()
        );

        // And it is geometry, not only the style's background rectangle — which
        // `render` emits for *any* tile, including one whose every source layer
        // was missing.
        let non_background = shapes
            .iter()
            .filter(|s| !matches!(s, walkers::ShapeOrText::Shape(egui::Shape::Rect(_))))
            .count();
        assert!(
            non_background > 100,
            "{non_background} of {} shapes were not the style's background \
             rectangle; a tile whose source layers all missed renders exactly \
             the background and nothing else",
            shapes.len()
        );
    }

    /// The two themes render the same tile to different pictures.
    ///
    /// The control for "a style was applied at all": if the style were ignored,
    /// or if the same one were used for both, these would be identical.
    #[test]
    fn the_theme_chooses_the_style_the_tile_is_rendered_against() {
        let ctx = Context::default();

        let mut drawn = Vec::new();
        for is_dark in [true, false] {
            let Some(mut tiles) = fixture_tiles(
                "the_theme_chooses_the_style_the_tile_is_rendered_against",
                &ctx,
                is_dark,
            ) else {
                return;
            };

            let max_zoom = pump_until(DEFAULT_TIMEOUT, || {
                tiles.pump();
                tiles.source_max_zoom()
            })
            .expect("the archive header never arrived");

            let tile_id = TileId {
                x: squallar_geo::lon_to_tile_x(MONACO_LON, max_zoom),
                y: squallar_geo::lat_to_tile_y(MONACO_LAT, max_zoom),
                zoom: max_zoom,
            };

            let piece = pump_until(DEFAULT_TIMEOUT, || {
                let piece = draw_one(&mut tiles, tile_id)?;
                (piece.uv == whole_tile_uv()).then_some(piece)
            })
            .expect("the archive never yielded Monaco's own tile");

            let Tile::Vector(shapes) = piece.tile else {
                panic!("the archive yielded a raster");
            };
            drawn.push(format!("{shapes:?}"));
        }

        assert_ne!(
            drawn[0], drawn[1],
            "the dark and light styles rendered the same tile identically, so \
             the style is not reaching the renderer"
        );
    }

    /// A coordinate the archive does not hold is not a retry and not a hole in
    /// the log: the cache keeps its `None` and the source stays quiet.
    #[test]
    fn a_tile_the_archive_does_not_hold_settles_rather_than_retrying() {
        let ctx = Context::default();
        let Some(mut tiles) = fixture_tiles(
            "a_tile_the_archive_does_not_hold_settles_rather_than_retrying",
            &ctx,
            true,
        ) else {
            return;
        };

        let max_zoom = pump_until(DEFAULT_TIMEOUT, || {
            tiles.pump();
            tiles.source_max_zoom()
        })
        .expect("the archive header never arrived");

        // Mid-Atlantic, which a Monaco archive certainly does not hold.
        let tile_id = TileId {
            x: squallar_geo::lon_to_tile_x(-30.0, max_zoom),
            y: squallar_geo::lat_to_tile_y(25.0, max_zoom),
            zoom: max_zoom,
        };

        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            assert!(
                draw_one(&mut tiles, tile_id).is_none(),
                "{tile_id:?} is not in a Monaco archive and nothing may be drawn for it"
            );
            std::thread::sleep(Duration::from_millis(2));
        }

        assert!(
            tiles.tile_is_cached(tile_id),
            "an absent tile must keep its cache slot, or it is asked for again \
             on every frame for as long as it is on screen"
        );
    }

    /// The committed fixture behind a counted, killable seam — the archive
    /// path's loopback server. Every byte the source ever serves passes
    /// `read_range`, so `reads` is the complete fetch ledger, and `dead`
    /// turns the "it would have refetched" control into a hard failure: once
    /// stored, any fetch attempt errors and the tile it was for never
    /// arrives.
    struct CountingRangeSource {
        inner: FileRangeSource,
        reads: Arc<std::sync::atomic::AtomicUsize>,
        dead: Arc<AtomicBool>,
    }

    impl crate::basemap_archive::RangeSource for CountingRangeSource {
        async fn read_range(
            &self,
            offset: u64,
            length: usize,
        ) -> Result<Vec<u8>, crate::basemap_archive::RangeError> {
            if self.dead.load(Ordering::SeqCst) {
                return Err(crate::basemap_archive::RangeError::Transport(
                    "the loopback source was killed after the first fill".to_owned(),
                ));
            }
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.read_range(offset, length).await
        }
    }

    /// [`fixture_tiles`] over a [`CountingRangeSource`], answering the ledger
    /// and the kill switch beside the source.
    fn counted_fixture_tiles(
        test: &str,
        ctx: &Context,
        style: std::sync::Arc<walkers::Style>,
    ) -> Option<(
        HttpsTiles,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<AtomicBool>,
    )> {
        let path = fixture_path();
        let inner = match FileRangeSource::open(&path) {
            Ok(inner) => inner,
            Err(error) => {
                eprintln!(
                    "SKIPPED {test}: {} would not open ({error}). It is committed; \
                     `git status` on it.",
                    path.display()
                );
                return None;
            }
        };
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dead = Arc::new(AtomicBool::new(false));
        let tiles = HttpsTiles::from_range_source(
            CountingRangeSource {
                inner,
                reads: Arc::clone(&reads),
                dead: Arc::clone(&dead),
            },
            style,
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx.clone(),
            NonZeroUsize::new(64).expect("64 is not zero"),
            // Nothing declared: the header's word stands, which is what the
            // archives under test are read by.
            None,
            // No seed and no downloaded areas: these read the committed
            // fixture over a bare source, and the composition's own suite is
            // what covers the local-first walk.
            crate::tile_source::ArchiveStores {
                seed: None,
                offline: None::<crate::basemap_download::PlatformSegmentStore>,
                offline_archive: crate::basemap_download::AreaArchive::Basemap,
            },
        );
        Some((tiles, reads, dead))
    }

    /// Monaco's own z14 tile id.
    fn monaco_tile(max_zoom: u8) -> TileId {
        TileId {
            x: squallar_geo::lon_to_tile_x(MONACO_LON, max_zoom),
            y: squallar_geo::lat_to_tile_y(MONACO_LAT, max_zoom),
            zoom: max_zoom,
        }
    }

    /// Drive `tiles` until Monaco's own tile arrives whole, as a `Debug`
    /// rendering — exact for every `f32` in every vertex, as the walkers
    /// golden argues.
    fn monaco_drawn(tiles: &mut HttpsTiles, tile_id: TileId) -> Option<String> {
        let piece = draw_one(tiles, tile_id)?;
        (piece.uv == whole_tile_uv()).then(|| match piece.tile {
            Tile::Vector(shapes) => format!("{shapes:?}"),
            Tile::Raster(_) => panic!("the archive holds MVT; a raster cannot arrive"),
        })
    }

    /// What `mvt::parse` + `mvt::styled` produce for Monaco's z14 bytes under
    /// `style` — the from-scratch oracle the restyled output must equal.
    fn monaco_rendered_fresh(style: &walkers::Style) -> String {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let bytes = runtime.block_on(async {
            let archive = crate::basemap_archive::BasemapArchive::open(
                FileRangeSource::open(&fixture_path()).expect("the fixture opens"),
            )
            .await
            .expect("the fixture is a PMTiles archive");
            archive
                .tile(14, 8529, 5974)
                .await
                .expect("the tile reads")
                .into_bytes()
                .expect("the fixture holds Monaco's own z14 tile")
        });
        match Tile::from_mvt(&bytes, style, 14).expect("the tile renders") {
            Tile::Vector(shapes) => format!("{shapes:?}"),
            Tile::Raster(_) => panic!("an MVT body rendered as a raster"),
        }
    }

    /// Fill once, kill the source, change the style, and demand the correct
    /// new output with **zero fetches** — the shared body of the two restyle
    /// pins below.
    ///
    /// The control that the old path would have refetched is structural, not
    /// asserted: before `HttpsTiles::set_style`, a style change rebuilt the
    /// source (`MapTileState::ensure_base_tiles`'s v1 behaviour, recorded at
    /// its old rebuild site), and a rebuilt source re-opens the archive and
    /// re-reads every visible tile — reads which, with `dead` stored, error.
    /// So a regression to any refetching shape cannot produce the new output
    /// at all; it times out below instead of passing quietly. The ledger
    /// assertion is the belt to those braces.
    fn restyle_after_the_source_dies(
        test: &str,
        before_style: std::sync::Arc<walkers::Style>,
        after_style: std::sync::Arc<walkers::Style>,
    ) {
        let ctx = Context::default();
        let Some((mut tiles, reads, dead)) = counted_fixture_tiles(test, &ctx, before_style) else {
            return;
        };

        let max_zoom = pump_until(DEFAULT_TIMEOUT, || {
            tiles.pump();
            tiles.source_max_zoom()
        })
        .expect("the archive header never arrived");
        let tile_id = monaco_tile(max_zoom);

        let before = pump_until(DEFAULT_TIMEOUT, || monaco_drawn(&mut tiles, tile_id))
            .expect("the archive never yielded Monaco's own tile");

        // The kill: from here, a single fetch attempt is a hard error and the
        // tile it was for never arrives.
        dead.store(true, Ordering::SeqCst);
        let reads_at_kill = reads.load(Ordering::SeqCst);
        assert!(
            reads_at_kill > 0,
            "fixture: the first fill read the archive"
        );

        tiles.set_style(after_style.clone());

        // Immediately after the swap the stale tile still draws — that is the
        // no-blank-beat half of the contract.
        assert_eq!(
            monaco_drawn(&mut tiles, tile_id).as_ref(),
            Some(&before),
            "the outgoing style must keep drawing until the restyle lands"
        );

        let after = pump_until(DEFAULT_TIMEOUT, || {
            let drawn = monaco_drawn(&mut tiles, tile_id)?;
            (drawn != before).then_some(drawn)
        })
        .expect(
            "the restyled tile never arrived; with the source dead this means \
             the restyle tried to refetch instead of using the parsed cache",
        );

        assert_eq!(
            reads.load(Ordering::SeqCst),
            reads_at_kill,
            "the restyle read the archive: the parsed cache did not serve it"
        );

        // Correct, not merely different: byte-for-byte what a from-scratch
        // render of the new style produces.
        assert_eq!(
            after,
            monaco_rendered_fresh(&after_style),
            "the restyled output is not what the new style renders from scratch"
        );
    }

    /// **A map-detail toggle re-renders with zero fetches.** The style pair is
    /// the toggle's own: the committed dark style with and without the
    /// `water` source-layer.
    #[test]
    fn a_detail_toggle_restyles_with_zero_fetches_after_the_source_dies() {
        let disabled: std::collections::BTreeSet<String> = ["water".to_owned()].into();
        restyle_after_the_source_dies(
            "a_detail_toggle_restyles_with_zero_fetches_after_the_source_dies",
            crate::basemap_style::committed(true),
            crate::basemap_style::committed_filtered(true, &disabled),
        );
    }

    /// **A theme flip re-styles from the parsed cache.** Dark to light over
    /// one live source, zero fetches — the flip that used to drop the source
    /// and refetch the viewport.
    #[test]
    fn a_theme_flip_restyles_from_the_parsed_cache_without_refetching() {
        restyle_after_the_source_dies(
            "a_theme_flip_restyles_from_the_parsed_cache_without_refetching",
            crate::basemap_style::committed(true),
            crate::basemap_style::committed(false),
        );
    }

    /// A range source that cannot answer at all: the shape of DNS failure, a
    /// 404, a host that is down, or a stale `omt-YYYYMMDD` generation.
    struct DeadRangeSource;

    impl crate::basemap_archive::RangeSource for DeadRangeSource {
        async fn read_range(
            &self,
            _offset: u64,
            _length: usize,
        ) -> Result<Vec<u8>, crate::basemap_archive::RangeError> {
            Err(crate::basemap_archive::RangeError::Transport(
                "the host is not there".to_owned(),
            ))
        }
    }

    /// **An archive that will not open is reported, bounded, and not blank
    /// forever.**
    ///
    /// Three separate defects met here, and each assertion below is one of
    /// them:
    ///
    /// 1. The failure was logged inside the IO task and *nowhere else*, so the
    ///    frame side could not tell "still loading" from "will never load".
    /// 2. Returning from that task drops the `Receiver`, and a disconnected
    ///    `TrySendError` reports `is_full() == false` -- so `request_once` fell
    ///    past its retry arm into `log::error!`, and because the closure had
    ///    errored, `LruCache::try_get_or_insert` inserted nothing, so the same
    ///    tile was asked for again on the very next frame. ~54 error lines per
    ///    frame, forever.
    /// 3. Nothing was ever drawn.
    #[test]
    fn an_archive_that_will_not_open_reports_itself_and_stops_asking() {
        let ctx = Context::default();
        let mut tiles = HttpsTiles::from_range_source(
            DeadRangeSource,
            crate::basemap_style::committed(true),
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx.clone(),
            NonZeroUsize::new(64).expect("64 is not zero"),
            // Nothing declared: the header's word stands, which is what the
            // archives under test are read by.
            None,
            // No seed and no downloaded areas: these read the committed
            // fixture over a bare source, and the composition's own suite is
            // what covers the local-first walk.
            crate::tile_source::ArchiveStores {
                seed: None,
                offline: None::<crate::basemap_download::PlatformSegmentStore>,
                offline_archive: crate::basemap_download::AreaArchive::Basemap,
            },
        );

        let fault = pump_until(DEFAULT_TIMEOUT, || {
            tiles.pump();
            tiles.fault().map(str::to_owned)
        })
        .expect("the source never reported why it serves nothing");

        assert!(
            !fault.is_empty(),
            "the fault must carry the transport's own words, or it cannot be acted on"
        );

        // The depth is never learnt, so the source must keep claiming nothing
        // rather than falling back to a zero that seeds `0/0/0`.
        assert_eq!(
            tiles.source_max_zoom(),
            None,
            "a source that never read a header must not claim a depth"
        );

        // The flood: every one of these would have been an `error!` line, and
        // the run is what proves the latch holds across frames rather than
        // suppressing only a repeat within one.
        let before = tiles.cached_entries();
        for _ in 0..10 {
            tiles.pump();
            for x in 0..54u32 {
                assert!(
                    tiles.at(TileId { x, y: 0, zoom: 14 }).is_none(),
                    "a source with no archive behind it cannot answer with a tile"
                );
            }
        }

        assert!(
            tiles.requests_closed || tiles.source_max_zoom().is_none(),
            "540 requests against a dead source must have latched something"
        );
        assert_eq!(
            tiles.cached_entries(),
            before,
            "a dead source must not burn cache slots on tiles it will never fetch"
        );
    }
}

/// The archive decode seam: the header's `tile_type`, never the bytes,
/// decides how a body becomes a [`Tile`]. Two committed fixtures carry the
/// two real archives' shapes — Monaco (`tile_type = 1`, MVT) and the
/// single-tile terrain wrap (`tile_type = 4`, the real Kansas WebP inside).
mod archive_decode {
    use std::num::NonZeroUsize;
    use std::path::PathBuf;

    use super::super::{ArchiveTileKind, decode_archive_tile};
    use super::*;
    use crate::basemap_archive::{BasemapArchive, FileRangeSource};

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name)
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    /// Open a committed archive and read one tile out of it, answering the
    /// header-derived kind alongside the body.
    fn kind_and_tile(name: &str, z: u8, x: u32, y: u32) -> Option<(ArchiveTileKind, Vec<u8>)> {
        let path = fixture(name);
        let source = match FileRangeSource::open(&path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!(
                    "SKIPPED: {} would not open ({error}). It is committed; \
                     `git status` on it.",
                    path.display()
                );
                return None;
            }
        };
        Some(block_on(async {
            let archive = BasemapArchive::open(source)
                .await
                .expect("the committed fixture is a PMTiles archive");
            let kind = ArchiveTileKind::from_tile_type(archive.tile_type());
            let bytes = archive
                .tile(z, x, y)
                .await
                .expect("the tile reads")
                .into_bytes()
                .expect("the fixture holds this tile");
            (kind, bytes)
        }))
    }

    /// The terrain fixture's header says WebP, so its body routes to the
    /// hillshade decoder and comes out a raster — with the flat-transparent
    /// remap applied, not the raw grey.
    #[test]
    fn a_raster_archives_header_routes_its_body_to_the_hillshade_decoder() {
        let Some((kind, bytes)) = kind_and_tile("terrain-hillshade-mini.pmtiles", 10, 224, 395)
        else {
            return;
        };
        assert_eq!(
            kind,
            ArchiveTileKind::Hillshade,
            "tile_type 4 is a raster archive"
        );

        let ctx = Context::default();
        let tile = decode_archive_tile(&bytes, kind, &Style::default(), 10, &ctx)
            .expect("the WebP body decodes");
        match tile {
            Tile::Raster(_) => {}
            Tile::Vector(_) => panic!("a WebP body under a raster header became a vector tile"),
        }
    }

    /// The basemap fixture's header says MVT, so its body routes to
    /// `Tile::from_mvt` exactly as before the seam existed.
    #[test]
    fn an_mvt_archives_header_still_routes_its_body_to_from_mvt() {
        let Some((kind, bytes)) = kind_and_tile("monaco.pmtiles", 14, 8529, 5974) else {
            return;
        };
        assert_eq!(kind, ArchiveTileKind::Vector, "tile_type 1 is MVT");

        let ctx = Context::default();
        let tile = decode_archive_tile(
            &bytes,
            kind,
            &crate::basemap_style::committed(true),
            14,
            &ctx,
        )
        .expect("the MVT body tessellates");
        match tile {
            Tile::Vector(_) => {}
            Tile::Raster(_) => panic!("an MVT body under a vector header became a raster"),
        }
    }

    /// **The choice comes from the header, not the bytes.** Each fixture's
    /// body handed to the *other* archive's kind errors instead of being
    /// sniffed into whatever it happens to look like — which is exactly what
    /// `Tile::new`'s guess used to do, and the regression this seam exists
    /// to prevent.
    #[test]
    fn the_bytes_do_not_get_a_vote() {
        let Some((_, webp)) = kind_and_tile("terrain-hillshade-mini.pmtiles", 10, 224, 395) else {
            return;
        };
        let Some((_, mvt)) = kind_and_tile("monaco.pmtiles", 14, 8529, 5974) else {
            return;
        };
        let ctx = Context::default();

        // Control: `Tile::new` WOULD sniff the WebP body into a raster, so
        // the error below is the seam refusing, not the body being unreadable.
        assert!(
            Tile::new(&webp, &Style::default(), 10, &ctx).is_ok(),
            "non-vacuity: the sniffing path accepts this body",
        );

        assert!(
            decode_archive_tile(&webp, ArchiveTileKind::Vector, &Style::default(), 10, &ctx)
                .is_err(),
            "a WebP body under an MVT header is a broken archive, not a raster",
        );
        assert!(
            decode_archive_tile(
                &mvt,
                ArchiveTileKind::Hillshade,
                &Style::default(),
                14,
                &ctx
            )
            .is_err(),
            "an MVT body under a raster header is a broken archive, not a map",
        );
    }

    // -----------------------------------------------------------------------
    // Packed elevation
    // -----------------------------------------------------------------------

    /// A 2x2 PNG, valid enough that every image decoder in the tree accepts it.
    ///
    /// It stands in for a terrain-RGB body: the point of the tests below is
    /// that a body a decoder *would* take is refused anyway, so a body a
    /// decoder would reject could not tell them apart.
    fn a_real_png() -> Vec<u8> {
        let mut bitmap = image::RgbImage::new(2, 2);
        for y in 0..2 {
            for x in 0..2 {
                bitmap.put_pixel(x, y, image::Rgb([1 + x as u8, 2 + y as u8, 3]));
            }
        }
        let mut encoded = std::io::Cursor::new(Vec::new());
        bitmap
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("the fixture encodes as a PNG");
        encoded.into_inner()
    }

    /// A one-tile archive of `kind`, holding `body`, written to a temp file.
    ///
    /// A file rather than an in-memory source because `FileRangeSource` is
    /// already in scope here and the archive is a few hundred bytes.
    fn one_tile_archive_file(label: &str, kind: pmtiles::TileType, body: &[u8]) -> PathBuf {
        let mut sink = std::io::Cursor::new(Vec::new());
        let mut writer = pmtiles::PmTilesWriter::new(kind)
            .tile_compression(pmtiles::Compression::None)
            .min_zoom(0)
            .max_zoom(0)
            .bounds(-180.0, -85.0, 180.0, 85.0)
            .create(&mut sink)
            .expect("the writer opens");
        writer
            .add_tile(
                pmtiles::TileCoord::new(0, 0, 0).expect("0/0/0 is a tile"),
                body,
            )
            .expect("the tile writes");
        writer.finalize().expect("the archive finalizes");

        let path = std::env::temp_dir().join(format!(
            "squallar-declared-kind-{}-{label}.pmtiles",
            std::process::id()
        ));
        std::fs::write(&path, sink.into_inner()).expect("the archive writes");
        path
    }

    /// Build a source over a declared archive and let its IO task settle,
    /// answering the fault it recorded, if any.
    fn fault_of_declared(
        path: &std::path::Path,
        declared: Option<ArchiveTileKind>,
    ) -> Option<String> {
        let ctx = Context::default();
        let mut tiles = HttpsTiles::from_range_source(
            FileRangeSource::open(path).expect("the archive file opens"),
            std::sync::Arc::new(Style::default()),
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx,
            NonZeroUsize::new(8).expect("8 is not zero"),
            declared,
            crate::tile_source::ArchiveStores {
                seed: None,
                offline: None::<crate::basemap_download::PlatformSegmentStore>,
                offline_archive: crate::basemap_download::AreaArchive::Basemap,
            },
        );

        // Either outcome — a fault, or a depth published — means the IO task
        // finished with the header. Waiting on only one of the two would make
        // the timeout the assertion.
        pump_until(DEFAULT_TIMEOUT, || {
            tiles.pump();
            tiles
                .fault()
                .map(|fault| Some(fault.to_owned()))
                .or_else(|| tiles.source_max_zoom().map(|_| None))
        })
        .expect("the IO task never read the header")
    }

    /// **The declaration is checked against the header, not trusted.**
    ///
    /// `TerrainRgb` is the one kind the header cannot confirm — `tile_type = 2`
    /// is PNG for a hillshade and for an elevation grid alike — so all the open
    /// can check is that the bodies are PNG at all. It checks that much: a
    /// `TerrainRgb` declared over a WebP archive is recorded as a fault, the
    /// same shape as an archive that will not open.
    #[test]
    fn a_declared_terrain_rgb_over_a_non_png_archive_records_a_fault() {
        let png = one_tile_archive_file("png", pmtiles::TileType::Png, &a_real_png());
        let webp = one_tile_archive_file("webp", pmtiles::TileType::Webp, b"body");

        // The control, first: the same declaration over a PNG archive is
        // accepted, so the fault below is about the header and not about
        // declaring anything at all.
        assert_eq!(
            fault_of_declared(&png, Some(ArchiveTileKind::TerrainRgb)),
            None,
            "a TerrainRgb declared over a PNG archive must be accepted"
        );

        let fault = fault_of_declared(&webp, Some(ArchiveTileKind::TerrainRgb))
            .expect("a TerrainRgb declared over a WebP archive must be a fault");
        assert!(
            fault.contains("TerrainRgb") && fault.contains("Webp"),
            "the fault must name both what was declared and what the header \
             says, or it cannot be acted on: {fault}"
        );

        // And the undeclared control: the same WebP archive opened with no
        // declaration is fine, because the header's word is the whole truth.
        assert_eq!(
            fault_of_declared(&webp, None),
            None,
            "an undeclared archive must open on its header alone"
        );

        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_file(&webp);
    }

    /// How long a one-tile local archive gets to serve its only tile.
    ///
    /// Short because the *control* below has to fit inside it — the refusal is
    /// asserted as "nothing arrived in this long", and a window the positive
    /// case could not fill would make that vacuous.
    const LOCAL_ARCHIVE_TIMEOUT: Duration = Duration::from_secs(3);

    /// Whether a source over `path`, declared `declared`, ever serves the one
    /// tile its archive holds.
    fn serves_its_tile(path: &std::path::Path, declared: Option<ArchiveTileKind>) -> bool {
        let ctx = Context::default();
        let mut tiles = HttpsTiles::from_range_source(
            FileRangeSource::open(path).expect("the archive file opens"),
            std::sync::Arc::new(Style::default()),
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx,
            NonZeroUsize::new(8).expect("8 is not zero"),
            declared,
            crate::tile_source::ArchiveStores {
                seed: None,
                offline: None::<crate::basemap_download::PlatformSegmentStore>,
                offline_archive: crate::basemap_download::AreaArchive::Basemap,
            },
        );

        pump_until(LOCAL_ARCHIVE_TIMEOUT, || {
            tiles.pump();
            tiles.at(TileId {
                x: 0,
                y: 0,
                zoom: 0,
            })
        })
        .is_some()
    }

    /// **The declaration reaches the decode, not just the fault slot.**
    ///
    /// A declared `TerrainRgb` over a genuine PNG archive opens cleanly — the
    /// header cannot contradict it — and then serves **nothing**, because
    /// every body it holds is refused by the `TerrainRgb` arm. The control is
    /// the same archive with nothing declared, which paints, and which is what
    /// this would silently have done before the kind was carried from the open
    /// to `read_one` instead of being derived twice.
    #[test]
    fn a_declared_terrain_rgb_archive_paints_nothing() {
        let png = one_tile_archive_file("serves", pmtiles::TileType::Png, &a_real_png());

        assert!(
            serves_its_tile(&png, None),
            "non-vacuity: the same archive with nothing declared must paint its \
             tile inside {LOCAL_ARCHIVE_TIMEOUT:?}, or the refusal below is \
             just a short timeout"
        );
        assert!(
            !serves_its_tile(&png, Some(ArchiveTileKind::TerrainRgb)),
            "an archive declared to hold packed elevation painted a picture"
        );

        let _ = std::fs::remove_file(&png);
    }

    /// **A terrain-RGB body never becomes a picture.**
    ///
    /// The bodies are PNG and a decoder will happily take one — that is the
    /// hazard. The arm refuses instead, and says why in a sentence a reader of
    /// the log can act on.
    #[test]
    fn a_terrain_rgb_body_is_refused_rather_than_painted() {
        let png = a_real_png();
        let ctx = Context::default();

        // Non-vacuity: the sniffing path accepts this body, so the refusal
        // below is the arm's and not the bytes'.
        assert!(
            Tile::new(&png, &Style::default(), 0, &ctx).is_ok(),
            "non-vacuity: the sniffing path accepts this body"
        );
        // And so does the hillshade arm, which is the arm the header alone
        // would have chosen for it.
        assert!(
            decode_archive_tile(&png, ArchiveTileKind::Hillshade, &Style::default(), 0, &ctx)
                .is_ok(),
            "non-vacuity: the hillshade arm accepts this body"
        );

        // `Tile` is not `Debug`, so the error is taken by match rather than
        // by `expect_err`.
        let Err(error) = decode_archive_tile(
            &png,
            ArchiveTileKind::TerrainRgb,
            &Style::default(),
            0,
            &ctx,
        ) else {
            panic!("packed elevation must not decode into a tile");
        };
        assert!(
            error.contains("packed elevation") && error.contains("no picture"),
            "the refusal must say what the archive holds: {error}"
        );
    }

    /// The header can never produce [`ArchiveTileKind::TerrainRgb`]; only a
    /// caller can. Over every `tile_type` the format has.
    #[test]
    fn the_header_alone_never_answers_terrain_rgb() {
        use pmtiles::TileType;

        let every = [
            TileType::Unknown,
            TileType::Mvt,
            TileType::Png,
            TileType::Jpeg,
            TileType::Webp,
            TileType::Avif,
            TileType::Mlt,
        ];
        for tile_type in every {
            assert_ne!(
                ArchiveTileKind::from_tile_type(tile_type),
                ArchiveTileKind::TerrainRgb,
                "{tile_type:?} derived TerrainRgb from the header, which no \
                 header field can distinguish"
            );
        }

        // The declaration rule: TerrainRgb takes PNG and only PNG, and every
        // other kind is confirmed exactly.
        for tile_type in every {
            assert_eq!(
                ArchiveTileKind::TerrainRgb.accepts_tile_type(tile_type),
                tile_type == TileType::Png,
                "TerrainRgb accepted {tile_type:?}"
            );
            let exact = ArchiveTileKind::from_tile_type(tile_type);
            assert!(
                exact.accepts_tile_type(tile_type),
                "{exact:?} refused the very tile_type it was derived from"
            );
        }
    }
}
