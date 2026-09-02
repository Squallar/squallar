//! Tests for [`super`].

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::Context;
use walkers::sources::{Attribution, TileSource};
use walkers::{Style, Tile, TileId, TilePiece, Tiles};

use super::byte_lru::MARKER_BYTES;
use super::{
    HttpsTiles, MAX_IN_FLIGHT, MAX_PARALLEL_DOWNLOADS, PumpBudget, ReadFailureRun,
    SUSTAINED_READ_FAILURES, TileCache, WASM_TILE_DECODES_PER_PUMP, cache_ledger, drain_up_to,
    interpolate_from_lower_zoom, slot_for, tile_client, tile_id_is_valid,
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

/// The tuning values, restated as literals. The cache's bounds are bytes now
/// and live in `squallar-device-profile`'s brackets, pinned there
/// (`the_tile_allowances_are_the_written_figures_on_every_bracket`); what is
/// left here is the concurrency, the pump, and what one fixture slot costs.
const EXPECTED_PARALLEL_DOWNLOADS: usize = 6;
/// Re-pointed at WO-10, when the time budget became the governor and the count
/// became a backstop: the value is the channel's own capacity plus one, so it
/// means "whatever is queued" rather than a throttle. It was 2.
const EXPECTED_WASM_DECODES_PER_PUMP: usize = 7;
/// What one fixture tile costs the cache once it has landed: the marker's node
/// plus a 4x4 RGBA texture. A pending or failed marker costs [`MARKER_BYTES`]
/// alone. Every byte budget below is a multiple of one or the other, so the
/// counts the old cap tests spoke in read the same.
const FIXTURE_TILE_BYTES: u64 = MARKER_BYTES + (FIXTURE_SIDE * FIXTURE_SIDE * 4) as u64;

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

/// [`loopback_tiles`] with the styled cache's byte budget handed in, so a
/// test can size the cache to a working set — or short of one — rather than
/// to this build's device bracket.
fn loopback_tiles_with_budget(server: &TileServer, ctx: &Context, budget_bytes: u64) -> HttpsTiles {
    HttpsTiles::with_client_and_budget(
        LoopbackSource::new(&server.base_url),
        ctx.clone(),
        loopback_client(),
        budget_bytes,
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

/// **A queued completion is taken by the very next pump, and nothing is lost
/// doing it.**
///
/// The quantity is a count of pumps, never a duration. WO-9e made the drain
/// refuse every pump while a map gesture was live and resume only 500 ms after
/// the last input, so the number of pumps a queued tile waited through was
/// bounded by a wall clock and nothing else — at 175 Hz, 87 of them. The pump
/// now consults no gesture state at all, so the bound is one: from the pump
/// that finds the completion queued, the tile is drawable, and the ledger
/// records exactly the one take that moved it.
///
/// WO-9e's conservation property is the second half and is unchanged: takes are
/// conserved across the whole exercise — one completion arrived, one take is
/// recorded, and the tile is on the glass. Fewer is a lost tile, more is a
/// double count.
#[test]
fn a_queued_tile_is_taken_by_the_next_pump_and_the_takes_ledger_conserves() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);
    let tile_id = TileId {
        x: 1,
        y: 2,
        zoom: 3,
    };

    assert!(tiles.at(tile_id).is_none(), "fixture: nothing cached yet");
    assert_eq!(tiles.takes(), 0, "fixture: nothing taken before the ask");

    // Frames, not seconds: pump exactly as `draw_tile_layer` does, once per
    // frame, and count them. Every one of these is a frame a real gesture
    // would have been moving through.
    let mut pumps_waited = 0_u64;
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    while tiles.at(tile_id).is_none() {
        assert!(
            Instant::now() < deadline,
            "the tile never arrived across {pumps_waited} pumps",
        );
        // The take is recorded by the pump, and `at` is what puts it on the
        // glass. They must happen on the *same* frame: a take that needs a
        // later frame to become drawable is the latch in another spelling.
        assert_eq!(
            tiles.takes(),
            0,
            "a take was recorded on pump {pumps_waited} and the tile was still \
             not drawable on that same frame",
        );
        tiles.pump();
        pumps_waited += 1;
        std::thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(
        tiles.takes(),
        1,
        "one completion arrived and the ledger must say exactly one take: \
         fewer is a lost tile, more is a double count",
    );
    assert_eq!(
        tiles.pumps(),
        pumps_waited,
        "the pump ledger must count every frame the loop drove, or the count \
         above is not the quantity it claims to be",
    );
}

/// **The pump takes what is queued however busy the frame already is.**
///
/// [`super::PUMP_TIME_BUDGET`] governs how much one pump spends, and a budget
/// that can round down to zero would stall the map exactly when the machine is
/// busiest — the WO-9e failure in a different spelling. The first take of a
/// pump is therefore unconditional. Driven with the deadline already in the
/// past, which is the strongest form of "over budget" the loop can be shown.
#[test]
fn a_pump_already_over_its_time_budget_still_takes_one_completion() {
    let mut reported = false;
    let (mut tx, mut rx) = futures::channel::mpsc::channel::<u32>(MAX_PARALLEL_DOWNLOADS);
    for value in 0..4 {
        tx.try_send(value).expect("the fixture queue has room");
    }

    let mut taken = Vec::new();
    let count = super::drain_up_to(
        &mut rx,
        MAX_PARALLEL_DOWNLOADS + 1,
        // Already elapsed: every deadline test after the first take fails.
        Instant::now() - Duration::from_millis(1),
        // The pass has taken nothing yet, so it still owes its one free take.
        true,
        &mut reported,
        |value| {
            taken.push(value);
            // A governor fixture, not a real tile: it stands in for the native
            // arm's drain, whose every take is a `Put`. Deliberately not
            // `Restyle` -- `take_ledger`'s own tests rely on that family being
            // untouched by anything else in this binary.
            super::take_ledger::TakeKind::Put
        },
    );

    assert_eq!(
        count, 1,
        "a pump whose budget was already spent took {count} completions: the \
         first take is unconditional and every later one is the deadline's",
    );
    assert_eq!(taken, vec![0], "the take was not the queue's head");
}

/// The whole queue moves in one pump when the takes are cheap.
///
/// The counterpart of the test above, and what makes that one non-vacuous: the
/// bound really is the deadline rather than a hard-coded one-per-pump. With
/// budget left, a pump empties what the channel holds — which is at most
/// [`MAX_PARALLEL_DOWNLOADS`], the channel's own capacity.
#[test]
fn a_pump_inside_its_time_budget_empties_the_queue() {
    let mut reported = false;
    let (mut tx, mut rx) = futures::channel::mpsc::channel::<u32>(MAX_PARALLEL_DOWNLOADS);
    for value in 0..4 {
        tx.try_send(value).expect("the fixture queue has room");
    }

    let mut taken = Vec::new();
    let count = super::drain_up_to(
        &mut rx,
        MAX_PARALLEL_DOWNLOADS + 1,
        Instant::now() + Duration::from_secs(60),
        true,
        &mut reported,
        |value| {
            taken.push(value);
            // A governor fixture, not a real tile: it stands in for the native
            // arm's drain, whose every take is a `Put`. Deliberately not
            // `Restyle` -- `take_ledger`'s own tests rely on that family being
            // untouched by anything else in this binary.
            super::take_ledger::TakeKind::Put
        },
    );

    assert_eq!(count, 4, "the pump did not empty a queue it had budget for");
    assert_eq!(taken, vec![0, 1, 2, 3], "the queue moved out of order");
}

// ---------------------------------------------------------------------------

/// **The zoom-out black screen, and the net that fills it.**
///
/// `cached_or_interpolated` walks towards zoom 0 and never towards the leaves,
/// so the tiles a zoom-out was just looking at — its *descendants* — can never
/// answer for it. Nothing asked for a shallower level either, so a session that
/// had only ever been deep drew a hole for every cell of a shallow viewport.
///
/// This drives the sequence the user reported: sit deep, then zoom out. The
/// first half is the mechanism, asserted directly — a deep tile resident, its
/// own shallow parent still a hole. The second half is the fix: once the net
/// has been warmed, as `draw_tile_layer` warms it every frame, the same shallow
/// ask is answered without a single new byte over the network.
#[test]
fn a_zoom_out_past_the_cached_level_has_an_ancestor_to_draw() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles = loopback_tiles(&server, &ctx);

    // Where the user has been sitting: one deep tile, on the glass.
    let deep = TileId {
        x: 1 << 10,
        y: 1 << 10,
        zoom: 14,
    };
    tile_eventually(&mut tiles, deep);

    // Every question below goes through `cached_or_interpolated` and never
    // through `at`. `at` is `ground_at`, which *requests* the tile it is asked
    // for — so polling with it would fetch the very tiles the net is supposed
    // to be the reason for, and every assertion here would hold with the net
    // ripped out. What is being asked is only ever "what could this frame
    // draw", which is what the user sees.
    let net_zoom = deep.zoom - crate::tiles::WARM_ANCESTOR_STEPS;
    let ancestor_of = |steps: u8| TileId {
        x: deep.x >> steps,
        y: deep.y >> steps,
        zoom: deep.zoom - steps,
    };

    // The mechanism. Every step of a zoom-out from where the user was sitting
    // is a hole: the walk runs from the asked-for zoom towards 0 and the only
    // thing cached is *below* all of them.
    for steps in 1..=crate::tiles::WARM_ANCESTOR_STEPS {
        assert!(
            tiles.cached_or_interpolated(ancestor_of(steps)).is_none(),
            "zooming out {steps} step(s) could already draw something before \
             the net existed, so the hole this test is about is not the hole \
             being reproduced",
        );
    }

    // The fix: warm the net, as `draw_tile_layer` does after every draw.
    let net = ancestor_of(crate::tiles::WARM_ANCESTOR_STEPS);
    tiles.warm(net);
    assert!(
        pump_until(DEFAULT_TIMEOUT, || {
            tiles.pump();
            tiles.cached_or_interpolated(net)
        })
        .is_some(),
        "the warmed net tile never arrived",
    );

    // ...and now every one of those steps draws, stretched, off the net alone —
    // no fetch of its own was ever started for any of them.
    for steps in 1..=crate::tiles::WARM_ANCESTOR_STEPS {
        assert!(
            tiles.cached_or_interpolated(ancestor_of(steps)).is_some(),
            "zooming out {steps} step(s) still had no ancestor to stretch with \
             the net resident: the zoom-out is a black screen for every cell",
        );
    }
    assert_eq!(
        net.zoom, net_zoom,
        "the net is asked for at the depth WARM_ANCESTOR_STEPS names",
    );
}

/// The net is requested at the depth it claims, and only where the source can
/// serve it. A net deeper than the source, or below zoom 0, is not a net.
#[test]
fn the_ancestor_net_is_never_asked_for_past_what_the_source_serves() {
    let server = TileServer::start(Behaviour::Hang);
    let ctx = Context::default();
    let shallow = HttpsTiles::with_client(
        LoopbackSource::new(&server.base_url).with_max_zoom(3),
        ctx.clone(),
        loopback_client(),
    );
    let mut shallow = shallow;

    let too_deep = TileId {
        x: 0,
        y: 0,
        zoom: 9,
    };
    shallow.warm(too_deep);
    assert!(
        !shallow.tile_is_cached(too_deep),
        "the net asked for a level the source cannot serve, which is the \
         `0/0/0` seeding MAX_ZOOM_UNKNOWN exists to prevent in another guise",
    );

    let servable = TileId {
        x: 1,
        y: 1,
        zoom: 2,
    };
    shallow.warm(servable);
    assert!(
        shallow.tile_is_cached(servable),
        "control: a net tile the source can serve must be asked for, or the \
         refusal above is vacuous",
    );
}

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

/// The concurrency limit and the pump's count backstop are the written
/// figures; the cache bounds are not here to pin, because there are none —
/// see [`the_two_slots_price_against_the_brackets_they_are_handed`].
#[test]
fn the_tuning_constants_are_the_written_figures() {
    assert_eq!(
        MAX_PARALLEL_DOWNLOADS, EXPECTED_PARALLEL_DOWNLOADS,
        "the parallel-download limit is a provider term of use, not a dial"
    );
    assert_eq!(
        WASM_TILE_DECODES_PER_PUMP, EXPECTED_WASM_DECODES_PER_PUMP,
        "the decode allowance is the channel's capacity plus one — 'whatever \
         is queued', with PUMP_TIME_BUDGET as the actual governor"
    );
    assert_eq!(
        WASM_TILE_DECODES_PER_PUMP,
        MAX_PARALLEL_DOWNLOADS + 1,
        "the count backstop must stay derived from the channel's own capacity: \
         a smaller number is a throttle nothing measured, and a larger one \
         cannot be reached because the queue cannot hold it"
    );
}

/// The styled tail is the whole slot and not the shapes alone: the plan's
/// 1.03 MB left the strokes out, and the strokes are more than half the
/// flattened half. If this ever reads under the shapes' own 652,112 plus a
/// flattened half, the band test has stopped measuring what an arrival puts.
#[allow(
    dead_code,
    reason = "a compile-time assertion; the name is its message"
)]
const STYLED_TAIL_CARRIES_THE_FLATTENED_HALF: () = assert!(
    super::MEASURED_STYLED_ENTRY_BYTES > 652_112 + 400_000,
    "the measured styled tail no longer carries the flattened buffers beside the shapes"
);

/// **Two slots price against the brackets they are handed, and the prose that
/// used to claim it is an imported assertion.** The old sizing note priced
/// four live sources at 61.5 MiB apiece against
/// `WASM_APP_TEXTURE_BUDGET_BYTES`; the layout it described no longer exists,
/// and the two slots that do exist are host memory except for the terrain
/// rasters, which are the one tile population on the GPU and are omitted from
/// the GPU sum by name (the device-profile side holds that:
/// `the_terrain_rasters_are_omitted_from_the_gpu_sum_by_name`). What this
/// holds, importing the figures rather than restating them: the terrain floor
/// is a small fraction of the wasm GPU budget it is omitted from; the wasm
/// styled floor cannot hold the user's 106-tile worst case at the measured
/// tail and holds 1,600 typical entries; and the measured tail is the whole
/// slot — shapes and flattened buffers — and not the shapes alone.
#[test]
fn the_two_slots_price_against_the_brackets_they_are_handed() {
    use squallar_device_profile::budget::BudgetLimits;
    use squallar_device_profile::constants::WASM_APP_TEXTURE_BUDGET_BYTES;

    let wasm = BudgetLimits::WASM;
    assert!(
        wasm.tile_terrain_bytes.floor <= WASM_APP_TEXTURE_BUDGET_BYTES / 10,
        "the wasm terrain floor ({} MiB) is more than a tenth of the {} MiB GPU budget it is \
         omitted from; re-argue the omission",
        wasm.tile_terrain_bytes.floor >> 20,
        WASM_APP_TEXTURE_BUDGET_BYTES >> 20,
    );
    let styled_floor = wasm.tile_styled_bytes.floor as u64;
    assert!(
        super::worst_case_entries(styled_floor) < 106,
        "the wasm styled floor holds {} worst-case entries, so it now holds the user's \
         106-tile window outright and the working-set floor is no longer what carries it",
        super::worst_case_entries(styled_floor),
    );
    assert!(
        styled_floor as usize / super::TYPICAL_STYLED_ENTRY_BYTES >= 1_600,
        "the wasm styled floor holds {} typical entries, under the 1,600 its doc quotes",
        styled_floor as usize / super::TYPICAL_STYLED_ENTRY_BYTES,
    );
    // The tail is the whole slot and not the shapes alone — held at compile
    // time by `STYLED_TAIL_CARRIES_THE_FLATTENED_HALF` below.
    // The terrain population is priced at the raster, no tail: one 256x256
    // RGBA texture, and the marker beside it.
    assert_eq!(super::RASTER_TILE_BYTES, 256 * 256 * 4);
    // Every slot the loopback fixture puts costs what this file says it does.
    assert_eq!(
        FIXTURE_TILE_BYTES,
        MARKER_BYTES + 64,
        "the fixture is a 4x4 RGBA tile: 64 texture bytes over the marker"
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

    let tile = runtime.block_on(async {
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

        Tile::from_mvt(&bytes, &crate::basemap_style::committed(true), 14)
            .expect("the tile renders")
    });

    // A non-triviality floor: a tile that rendered nothing would sail through
    // the bound below and prove nothing about the cost of a real one.
    let shapes = match &tile {
        Tile::Vector(shapes) => shapes.len(),
        Tile::Raster(_) => panic!("an MVT body rendered as a raster"),
    };
    assert!(
        shapes > 500,
        "the fixture's densest tile rendered only {shapes} shapes, so this is not \
         measuring a city core any more"
    );

    // The slot exactly as an arrival makes it: shapes priced at capacity plus
    // the fills and strokes flattened at a feathering of one point — the
    // value `feathering_of` answers on a 1x display — plus the marker's node.
    // What is measured is what is resident.
    let slot = slot_for(tile, 1, 1.0);
    let heap = slot.bytes() as usize;
    let shapes_alone = match slot.tile.as_ref() {
        Some(Tile::Vector(shapes)) => super::styled_heap_bytes(shapes),
        _ => unreachable!("the slot holds the vector tile it was built from"),
    };
    let flattened = slot.meshes.as_ref().map_or(0, |meshes| meshes.bytes()) as usize;
    assert!(
        flattened > 0,
        "the fixture tile flattened to no buffers, so this is not measuring the entry \
         a real arrival makes"
    );
    assert_eq!(
        heap,
        MARKER_BYTES as usize + shapes_alone + flattened,
        "the slot's charge is not the sum of its parts"
    );

    // Not an equality: `size_of` and allocator rounding are toolchain
    // properties, and pinning them would red-gate a compiler bump. The claim
    // that matters is that the constant the brackets are argued from is not
    // an *under*-estimate, and is still the right order. Re-derive the
    // constant by reading the figure out of this message, never by inference.
    assert!(
        heap <= super::MEASURED_STYLED_ENTRY_BYTES,
        "one styled entry measures {heap} bytes ({shapes_alone} of shapes, {flattened} \
         flattened, {MARKER_BYTES} marker), over the {} the brackets are derived from",
        super::MEASURED_STYLED_ENTRY_BYTES
    );
    assert!(
        heap * 2 >= super::MEASURED_STYLED_ENTRY_BYTES,
        "one styled entry measures {heap} bytes ({shapes_alone} of shapes, {flattened} \
         flattened), less than half the {} the brackets are derived from: the derivation \
         has gone stale in the safe direction, which still means it is not measuring this",
        super::MEASURED_STYLED_ENTRY_BYTES
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
        heap > super::MEASURED_STYLED_ENTRY_BYTES / 2,
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

/// The cache stops growing at its byte bound. Against a server that answers
/// 404 every slot is a failed marker charged [`MARKER_BYTES`], so a budget of
/// 256 markers settles at 256 entries — the count the old cap read, in bytes.
#[test]
fn the_tile_cache_is_bounded() {
    let server = TileServer::start(Behaviour::NotFound);
    let ctx = Context::default();
    let capacity = 256usize;
    let mut tiles = loopback_tiles_with_budget(&server, &ctx, capacity as u64 * MARKER_BYTES);

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

/// Eviction is exercised at several byte budgets, by recency, not only
/// compiled in. The budgets are markers' worth, since a 404 leaves a marker;
/// the three counts were once the three tiers' caps and are kept as sizes.
#[test]
fn eviction_holds_at_every_byte_budget_and_takes_the_least_recent() {
    for capacity in [8usize, 100, 256] {
        let server = TileServer::start(Behaviour::NotFound);
        let ctx = Context::default();
        let mut tiles = loopback_tiles_with_budget(&server, &ctx, capacity as u64 * MARKER_BYTES);
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

/// The grid `ui_map_overlays::draw_tile_layer` walks, in its order: rows
/// north to south, columns west to east, one `ground_at` per cell — after the
/// layer's one pump. **The bare walk**: no working set is reported, so the
/// cache has no floor and the byte budget alone decides — the shape the
/// eviction-mechanism pins below want.
fn frame_over_grid(tiles: &mut HttpsTiles, side: u32, zoom: u8) {
    tiles.pump();
    for y in 0..side {
        for x in 0..side {
            tiles.ground_at(TileId { x, y, zoom });
        }
    }
}

/// [`frame_over_grid`] as `draw_tile_layer` really makes a pass: the pump,
/// the working set reported for `pass_nr` (no ancestor net at this depth is
/// asked for here, so the net term is zero), then the walk. The report is
/// what sets the cache's floor and asks for last pass's refused cells first.
fn pass_over_grid(tiles: &mut HttpsTiles, pass_nr: u64, side: u32, zoom: u8) {
    tiles.pump();
    tiles.note_wanted(pass_nr, (side * side) as usize, 0);
    for y in 0..side {
        for x in 0..side {
            tiles.ground_at(TileId { x, y, zoom });
        }
    }
}

/// A 12x12 grid: 144 cells, which is what a 2878x1651-point window draws at
/// a whole zoom (13x8 = 104) plus part of what it holds between zooms (17x11
/// = 187) — and more than the ~106 the user's window measured on the rig at
/// zoom 13.5 — against a budget that holds 100 of them.
const GRID_SIDE: u32 = 12;
const GRID_CELLS: u64 = (GRID_SIDE * GRID_SIDE) as u64;
const GRID_FRAMES: usize = 120;
const GRID_ZOOM: u8 = 10;
/// A budget that holds 100 fixture tiles: the old wasm cap, in bytes, and
/// short of the grid by 44.
const BUDGET_FOR_100: u64 = 100 * FIXTURE_TILE_BYTES;
/// A budget that holds the whole grid with room.
const BUDGET_FOR_200: u64 = 200 * FIXTURE_TILE_BYTES;

/// **A working set the budget cannot hold is held by the floor, and refetches
/// nothing.** The same 144 cells every pass, in the walk order, over a budget
/// that holds 100 of them — the cap the user's 2878x1651 window overran on
/// the rig (1563 asks, 1457 of them refetches of tiles just evicted, with
/// nothing moving). Two arms, both `== 0`, because the mechanism and not the
/// number is what is pinned: with the budget holding the grid the floor is
/// never consulted; with the budget short by 44 tiles the floor the pass
/// reports keeps every cell resident, the overrun reads as exactly the
/// shortfall, and nothing on the glass is ever evicted for history.
///
/// This test landed `#[ignore]`d at WO-1 pinning the defect (`refetch > 0`),
/// and flipped here. What it no longer carries is the double fetch either:
/// duplicate and orphan puts read zero, as
/// [`an_evicted_pending_marker_is_never_fetched_twice`] holds without a
/// floor.
#[test]
fn a_working_set_the_budget_cannot_hold_is_held_by_the_floor_and_refetches_nothing() {
    for (budget, holds) in [(BUDGET_FOR_200, true), (BUDGET_FOR_100, false)] {
        let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
        let ctx = Context::default();
        let mut tiles = loopback_tiles_with_budget(&server, &ctx, budget);

        // Every cell has landed before the steady state is read: the request
        // channel holds six, so a 144-cell grid takes passes to ask for.
        let mut pass = 0u64;
        let landed = pump_until(DEFAULT_TIMEOUT, || {
            pass += 1;
            pass_over_grid(&mut tiles, pass, GRID_SIDE, GRID_ZOOM);
            (tiles.cache_stats().puts_first >= GRID_CELLS).then_some(())
        });
        assert!(
            landed.is_some(),
            "budget {budget}: not every cell landed within the timeout: {:?}",
            tiles.cache_stats()
        );
        for _ in 0..GRID_FRAMES {
            pass += 1;
            pass_over_grid(&mut tiles, pass, GRID_SIDE, GRID_ZOOM);
            std::thread::sleep(Duration::from_millis(2));
        }

        let stats = tiles.cache_stats();
        eprintln!(
            "budget {budget} over a {GRID_SIDE}x{GRID_SIDE} grid for {GRID_FRAMES} passes: \
             {stats:?}; the server saw {} requests",
            server.request_count()
        );
        assert_eq!(
            stats.refetch_after_eviction, 0,
            "budget {budget}: a tile still on the glass was evicted and asked for again: {stats:?}"
        );
        assert_eq!(
            stats.evicted(),
            0,
            "budget {budget}: the floor let a cell of the working set go: {stats:?}"
        );
        assert_eq!(
            stats.requests, GRID_CELLS,
            "budget {budget}: each of {GRID_CELLS} cells is asked for exactly once: {stats:?}"
        );
        assert_eq!(
            (stats.puts_first, stats.puts_duplicate, stats.puts_orphan),
            (GRID_CELLS, 0, 0),
            "budget {budget}: every landing was a first sight: {stats:?}"
        );
        assert_eq!(
            stats.resident_entries, GRID_CELLS,
            "budget {budget}: the level is what the cache holds: {stats:?}"
        );
        assert_eq!(
            stats.resident_bytes,
            GRID_CELLS * FIXTURE_TILE_BYTES,
            "budget {budget}: the level is what the cache is charged: {stats:?}"
        );
        assert_eq!(
            stats.floor_entries,
            GRID_CELLS + MAX_PARALLEL_DOWNLOADS as u64,
            "budget {budget}: the floor is the reported working set plus the in-flight markers"
        );
        if holds {
            assert_eq!(
                stats.overrun_bytes, 0,
                "budget {budget} holds the grid: {stats:?}"
            );
        } else {
            assert_eq!(
                stats.overrun_bytes,
                GRID_CELLS * FIXTURE_TILE_BYTES - budget,
                "budget {budget}: the overrun is exactly the shortfall the floor carries: {stats:?}"
            );
        }
        assert_eq!(
            server.request_count() as u64,
            GRID_CELLS,
            "budget {budget}: the server saw a request the cache did not count, or the reverse"
        );
    }
}

/// **Every visible cell is asked for, and within the channel's worth of
/// passes.** The request channel holds seven; a 144-cell walk asks for seven
/// a pass and is refused the rest, and before the refused-ask queue the
/// walk's head took the seven every pass while cells at the tail were never
/// asked at all — 10-20 of 144 in 120 frames at the old cap, the second
/// "broken on web" mechanism beside the eviction churn. Now the refused cells
/// are asked for first next pass, in the order they were refused, so a cell
/// reaches the channel within `ceil(144 / 7)` = 21 passes of asking. The bound
/// asserted is looser than that arithmetic — the IO thread has to take the
/// channel between passes, and a loaded box takes longer — but it is the
/// property that counts: every cell asked, and long before the 120 passes
/// that used to leave a tenth of them unasked.
#[test]
fn every_visible_cell_is_asked_within_the_channels_worth_of_passes() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles = loopback_tiles_with_budget(&server, &ctx, BUDGET_FOR_200);
    let ideal = GRID_CELLS.div_ceil(MAX_PARALLEL_DOWNLOADS as u64 + 1);

    let mut pass = 0u64;
    let all_asked = pump_until(DEFAULT_TIMEOUT, || {
        pass += 1;
        pass_over_grid(&mut tiles, pass, GRID_SIDE, GRID_ZOOM);
        std::thread::sleep(Duration::from_millis(2));
        let distinct: std::collections::HashSet<String> = server.requests().into_iter().collect();
        (distinct.len() as u64 >= GRID_CELLS).then_some(pass)
    });
    let stats = tiles.cache_stats();
    let asked = all_asked.unwrap_or_else(|| {
        let distinct: std::collections::HashSet<String> = server.requests().into_iter().collect();
        panic!(
            "after {pass} passes only {} of {GRID_CELLS} cells were ever asked for: {stats:?}",
            distinct.len()
        )
    });
    eprintln!(
        "every cell of a {GRID_SIDE}x{GRID_SIDE} grid was asked for by pass {asked} (the channel \
         alone allows {ideal}): {stats:?}"
    );
    assert!(
        asked <= GRID_FRAMES as u64 / 2,
        "every cell was asked for only by pass {asked}, against {ideal} the channel allows and \
         the {GRID_FRAMES} that used to leave the tail unasked: the refused-ask queue is not \
         serving the tail first"
    );
    assert_eq!(
        stats.requests, GRID_CELLS,
        "each cell asked for once: {stats:?}"
    );
    assert_eq!(stats.refetch_after_eviction, 0, "{stats:?}");
}

/// **The control**: the same walk over a cap the working set fits in evicts
/// nothing, refetches nothing, and asks for each cell exactly once.
#[test]
fn a_working_set_under_the_budget_asks_for_every_tile_once_and_refetches_none() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles = loopback_tiles_with_budget(&server, &ctx, BUDGET_FOR_200);

    // The request channel holds six, so a 144-cell walk takes frames to ask
    // for everything; every body must also have landed before the put kinds
    // are read.
    let landed = pump_until(DEFAULT_TIMEOUT, || {
        frame_over_grid(&mut tiles, GRID_SIDE, GRID_ZOOM);
        (tiles.cache_stats().puts_first >= GRID_CELLS).then_some(())
    });
    assert!(
        landed.is_some(),
        "not every cell landed within the timeout: {:?}",
        tiles.cache_stats()
    );
    for _ in 0..GRID_FRAMES {
        frame_over_grid(&mut tiles, GRID_SIDE, GRID_ZOOM);
        std::thread::sleep(Duration::from_millis(2));
    }

    let stats = tiles.cache_stats();
    assert_eq!(
        stats.refetch_after_eviction, 0,
        "a working set under the cap refetched: {stats:?}"
    );
    assert_eq!(
        stats.evicted(),
        0,
        "a working set under the cap evicted: {stats:?}"
    );
    assert_eq!(
        stats.requests, GRID_CELLS,
        "each of {GRID_CELLS} cells is asked for exactly once: {stats:?}"
    );
    assert_eq!(
        (stats.puts_first, stats.puts_duplicate, stats.puts_orphan),
        (GRID_CELLS, 0, 0),
        "every landing was a first sight: {stats:?}"
    );
    assert_eq!(
        stats.resident_entries, GRID_CELLS,
        "the level is what the cache holds: {stats:?}"
    );
    assert_eq!(
        server.request_count() as u64,
        GRID_CELLS,
        "the server saw a request the cache did not count, or the reverse"
    );
}

/// **An evicted pending marker is not fetched twice.** The same 144 cells
/// over a budget that holds 100, walked **bare** — no working set reported,
/// so no floor, and the byte budget alone decides — which is the one way left
/// to make the cache evict tiles still on the glass. It still refetches, on
/// purpose: that is the mechanism under test. But every landing is a first
/// sight: no body lands twice for one ask (`duplicate`), no body lands that
/// nothing asked for (`orphan`), and every ask is either a cell's first or an
/// honest refetch of a cell the cache let go -- the cells counted off the
/// server's log, since without the pass report nothing serves the walk's
/// tail and it can go unasked (see the body). Before the in-flight set the
/// same run read 16 duplicate and 9 orphan over 607 asks.
#[test]
fn an_evicted_pending_marker_is_never_fetched_twice() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles = loopback_tiles_with_budget(&server, &ctx, BUDGET_FOR_100);

    for _ in 0..GRID_FRAMES {
        frame_over_grid(&mut tiles, GRID_SIDE, GRID_ZOOM);
        std::thread::sleep(Duration::from_millis(2));
    }
    // Let the tail land: a request still out at the last frame has not put
    // yet, and the put kinds are read once every request has been answered.
    let settled = pump_until(DEFAULT_TIMEOUT, || {
        tiles.pump();
        let s = tiles.cache_stats();
        (s.puts() == s.requests).then_some(())
    });

    let stats = tiles.cache_stats();
    assert!(
        settled.is_some(),
        "not every request was answered within the timeout: {stats:?}"
    );
    let paths = server.requests();
    // Which cells were ever asked for is read off the server's log rather
    // than assumed to be all 144. With the budget below the working set and
    // no pass report to serve the refused cells first, the request channel
    // (seven deep) is spent each frame on the cells the walk reaches first,
    // and cells at its tail can go unasked for the whole run: 9 of 144 on the
    // run that found this (2026-09-02). The pass report is what cures that
    // (`every_visible_cell_is_asked_within_the_channels_worth_of_passes`);
    // this walk leaves it out to keep the eviction mechanism under test.
    let distinct: std::collections::HashSet<&String> = paths.iter().collect();
    eprintln!(
        "cap 100 over a {GRID_SIDE}x{GRID_SIDE} grid for {GRID_FRAMES} frames, with the \
         in-flight set: {stats:?}; the server saw {} requests for {} distinct tiles",
        paths.len(),
        distinct.len()
    );
    assert_eq!(
        (stats.puts_duplicate, stats.puts_orphan),
        (0, 0),
        "a body landed twice for one ask, or landed unasked: {stats:?}"
    );
    assert!(
        distinct.len() as u64 <= GRID_CELLS,
        "the server saw a tile the grid does not have: {distinct:?}"
    );
    assert_eq!(
        stats.requests,
        distinct.len() as u64 + stats.refetch_after_eviction,
        "an ask that was neither a tile's first nor a refetch of an evicted one ({} distinct \
         tiles): {stats:?}",
        distinct.len()
    );
    assert_eq!(
        stats.puts_first, stats.requests,
        "every ask landed exactly once, as a first sight: {stats:?}"
    );
    assert_eq!(
        paths.len() as u64,
        stats.requests,
        "the server saw a request the cache did not count, or the reverse"
    );
    assert_eq!(
        tiles.in_flight_len(),
        0,
        "a request stayed open after every body had landed"
    );
}

/// **The marker is the LRU's to evict; the request is not.** Two tiles are
/// asked for and never answered; a third ask at a cap of two evicts the
/// first's pending marker. Before the in-flight set the next frame found no
/// slot and asked again. Now the server sees exactly one request per tile,
/// however long the marker has been gone.
#[test]
fn a_tile_whose_marker_was_evicted_while_its_request_was_out_is_not_asked_again() {
    let server = TileServer::start(Behaviour::Hang);
    let ctx = Context::default();
    let mut tiles = loopback_tiles_with_budget(&server, &ctx, 2 * MARKER_BYTES);
    let id = |x: u32| TileId { x, y: 0, zoom: 3 };

    assert!(
        server.wait_for_requests(2, &mut || {
            tiles.at(id(0));
            tiles.at(id(1));
        }),
        "the first two requests never reached the server"
    );
    // The third ask at the cap: id 0 is the least recently touched, and goes.
    assert!(
        server.wait_for_requests(3, &mut || {
            tiles.at(id(2));
        }),
        "the third request never reached the server"
    );
    assert!(
        !tiles.tile_is_cached(id(0)),
        "fixture: the third ask must evict the first's marker"
    );
    assert_eq!(tiles.cache_stats().evicted_pending, 1);

    // Keep wanting id 0, with its marker gone and its request out.
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        tiles.pump();
        assert!(
            tiles.at(id(0)).is_none(),
            "fixture: the server never answers"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        server.request_count(),
        3,
        "a tile whose request was out was asked for again: {:?}",
        server.requests()
    );
    assert_eq!(tiles.cache_stats().requests, 3);
    assert_eq!(
        tiles.in_flight_len(),
        3,
        "three requests out and none answered"
    );
}

/// **Open requests stop at what can be outstanding, and the first two terms
/// of [`MAX_IN_FLIGHT`] are exact.** Against a server that never answers, the
/// IO task holds [`MAX_PARALLEL_DOWNLOADS`] fetches and the request channel
/// fills behind it — [`MAX_PARALLEL_DOWNLOADS`] plus its single sender's
/// guaranteed slot — and the frame side, wanting far more, opens exactly that
/// many and not one more: a full channel refuses the send, and a refused send
/// opens nothing.
#[test]
fn open_requests_stop_at_the_fetch_limit_plus_the_request_channel() {
    let server = TileServer::start(Behaviour::Hang);
    let ctx = Context::default();
    let mut tiles =
        loopback_tiles_with_budget(&server, &ctx, 4 * MAX_IN_FLIGHT as u64 * FIXTURE_TILE_BYTES);
    let wanted: Vec<TileId> = (0..3 * MAX_IN_FLIGHT as u32)
        .map(|x| TileId { x, y: 0, zoom: 10 })
        .collect();
    let expected = 2 * MAX_PARALLEL_DOWNLOADS + 1;

    let reached = pump_until(DEFAULT_TIMEOUT, || {
        tiles.pump();
        for id in &wanted {
            tiles.at(*id);
        }
        (tiles.in_flight_len() == expected && server.request_count() == MAX_PARALLEL_DOWNLOADS)
            .then_some(())
    });
    assert!(
        reached.is_some(),
        "open requests settled at {} with the server holding {}, not at {expected} and \
         {MAX_PARALLEL_DOWNLOADS}",
        tiles.in_flight_len(),
        server.request_count()
    );
    // And stays there: more wanting cannot open more.
    for _ in 0..8 {
        tiles.pump();
        for id in &wanted {
            tiles.at(*id);
        }
        assert_eq!(tiles.in_flight_len(), expected);
    }
}

/// **The completion channel is [`MAX_IN_FLIGHT`]'s third term, and the bound
/// holds.** Against a server that answers, with the frame side never pumping,
/// finished fetches queue in the completion channel while the IO task blocks
/// with one more in hand and the rest of its fetches behind it: more open than
/// the no-answer figure, and never more than [`MAX_IN_FLIGHT`]. Then one pump
/// takes the queue, and the count falls by exactly what it took — one closed
/// request per take.
#[test]
fn open_requests_never_exceed_max_in_flight() {
    let server = TileServer::start(Behaviour::Serve(Arc::new(fixture_png())));
    let ctx = Context::default();
    let mut tiles =
        loopback_tiles_with_budget(&server, &ctx, 4 * MAX_IN_FLIGHT as u64 * FIXTURE_TILE_BYTES);
    let wanted: Vec<TileId> = (0..3 * MAX_IN_FLIGHT as u32)
        .map(|x| TileId { x, y: 0, zoom: 10 })
        .collect();
    let without_answers = 2 * MAX_PARALLEL_DOWNLOADS + 1;

    let mut most = 0;
    let reached = pump_until(DEFAULT_TIMEOUT, || {
        // No pump: nothing leaves the completion channel.
        for id in &wanted {
            tiles.at(*id);
        }
        let open = tiles.in_flight_len();
        assert!(
            open <= MAX_IN_FLIGHT,
            "{open} requests open, over the bound of {MAX_IN_FLIGHT}"
        );
        most = most.max(open);
        (open > without_answers).then_some(())
    });
    assert!(
        reached.is_some(),
        "open requests never exceeded the no-answer figure {without_answers} (most seen: \
         {most}); the completion channel's term is missing"
    );
    for _ in 0..8 {
        for id in &wanted {
            tiles.at(*id);
        }
        let open = tiles.in_flight_len();
        assert!(open <= MAX_IN_FLIGHT, "{open} open, over {MAX_IN_FLIGHT}");
        most = most.max(open);
    }
    eprintln!("open requests peaked at {most} of a bound of {MAX_IN_FLIGHT}");

    // Only the frame side moves the set, so nothing changes between this
    // reading and the pump.
    let before = tiles.in_flight_len();
    let takes_before = tiles.takes();
    tiles.pump();
    let taken = (tiles.takes() - takes_before) as usize;
    assert!(
        taken > 0,
        "the pump took nothing off a full completion channel"
    );
    assert_eq!(
        tiles.in_flight_len(),
        before - taken,
        "a take closed no request, or more than one"
    );
}

/// **The cache classifies what lands and what it lets go**, at the cache:
/// a body on its own marker is a first sight, and so is a body whose marker
/// was evicted while its request was out; an ask for an id it remembers
/// evicting is a refetch; a body on a re-stamped tile is a restyle; a body on
/// a current tile is a duplicate; and a body with no slot and no request open
/// is an orphan. Each event lands in the source's own reading and, by at
/// least as much, in the role's statics.
#[test]
fn the_cache_tells_a_first_sight_from_a_refetch_a_restyle_a_duplicate_and_an_orphan() {
    use cache_ledger::{CacheRole, totals};

    let role = CacheRole::Terrain;
    let before = totals(role);
    // Two markers' worth: every slot here — a marker, or a shapeless vector
    // tile that flattens to nothing — is charged exactly the marker, so the
    // byte budget reads as a cap of two and this stays about kinds.
    let mut cache = TileCache::new(2 * MARKER_BYTES, role);
    let id = |x: u32| TileId { x, y: 0, zoom: 3 };
    let body = |epoch: u64| slot_for(Tile::Vector(Arc::new(Vec::new())), epoch, 0.0);

    cache.ask(id(0), 1);
    cache.ask(id(1), 1);
    let s = cache.stats();
    assert_eq!(
        (s.requests, s.refetch_after_eviction, s.evicted()),
        (2, 0, 0)
    );
    assert_eq!(s.resident_entries, 2);

    // id 0's body lands on its own marker.
    cache.put(id(0), body(1));
    assert_eq!(cache.stats().puts_first, 1);

    // A third ask at the cap evicts the least recently touched slot — id 1's
    // pending marker, since the put just touched id 0. The marker was the
    // LRU's to evict; the request was not.
    cache.ask(id(2), 1);
    let s = cache.stats();
    assert_eq!((s.evicted_pending, s.evicted_resident), (1, 0));
    assert!(!cache.contains(&id(1)));
    assert!(
        cache.is_in_flight(&id(1)),
        "the eviction closed the request"
    );

    // id 1's body lands with its marker gone: the tile's first sight all the
    // same, its request having been open — and its arrival evicts id 0, a
    // resident tile, and closes the request.
    cache.put(id(1), body(1));
    let s = cache.stats();
    assert_eq!((s.puts_first, s.puts_orphan), (2, 0), "{s:?}");
    assert_eq!((s.evicted_pending, s.evicted_resident), (1, 1), "{s:?}");
    assert!(
        !cache.is_in_flight(&id(1)),
        "the landing left the request open"
    );

    // id 0 is wanted again: a refetch, and a request.
    cache.ask(id(0), 1);
    let s = cache.stats();
    assert_eq!((s.requests, s.refetch_after_eviction), (4, 1), "{s:?}");

    // A restyle: id 1 is re-asked under a new generation and its restyled
    // body replaces it.
    cache.re_ask(id(1), 2);
    assert_eq!(cache.stats().restyle_asks, 1);
    assert!(cache.is_in_flight(&id(1)), "a re-ask is a request out");
    cache.put(id(1), body(2));
    assert_eq!(cache.stats().puts_restyle, 1);

    // A second body for id 1 that nothing asked for.
    cache.put(id(1), body(2));
    let s = cache.stats();
    assert_eq!(s.puts_duplicate, 1, "{s:?}");

    // A body for an id nothing asked for and nothing holds.
    cache.put(id(3), body(2));
    let s = cache.stats();
    assert_eq!(s.puts_orphan, 1, "{s:?}");
    assert_eq!(s.puts(), 5, "five landings over the four kinds: {s:?}");

    // The statics saw at least this source's events. `>=`, because other
    // tests in this binary build terrain sources too.
    let window = totals(role).diff(&before);
    assert!(window.requests >= s.requests, "{window:?} < {s:?}");
    assert!(window.refetch_after_eviction >= s.refetch_after_eviction);
    assert!(window.restyle_asks >= s.restyle_asks);
    assert!(window.puts_first >= s.puts_first);
    assert!(window.puts_restyle >= s.puts_restyle);
    assert!(window.puts_duplicate >= s.puts_duplicate);
    assert!(window.puts_orphan >= s.puts_orphan);
    assert!(window.evicted_pending >= s.evicted_pending);
    assert!(window.evicted_resident >= s.evicted_resident);
}

/// **A raster slot is priced at its texture over the marker, and the level
/// follows it out.** A marker is charged its node and nothing else; a raster
/// tile the node plus its texture. The budget is one raster's worth, so the
/// marker that follows it is over budget and the raster goes — with exactly
/// its charge in the eviction figure — and the level falls to the marker.
#[test]
fn the_resident_level_prices_a_raster_at_its_texture_and_releases_it_on_eviction() {
    let ctx = Context::default();
    let texture_bytes = u64::from(FIXTURE_SIDE * FIXTURE_SIDE * 4);
    let mut cache = TileCache::new(
        MARKER_BYTES + texture_bytes,
        cache_ledger::CacheRole::Terrain,
    );
    let id = |x: u32| TileId { x, y: 0, zoom: 2 };
    let image = egui::ColorImage::filled(
        [FIXTURE_SIDE as usize, FIXTURE_SIDE as usize],
        egui::Color32::WHITE,
    );
    let raster = || Tile::Raster(ctx.load_texture("t", image.clone(), Default::default()));

    cache.ask(id(0), 0);
    assert_eq!(
        cache.stats().resident_bytes,
        MARKER_BYTES,
        "a marker is priced at its node"
    );
    cache.put(id(0), slot_for(raster(), 0, 0.0));
    let s = cache.stats();
    assert_eq!(
        (s.resident_entries, s.resident_bytes),
        (1, MARKER_BYTES + texture_bytes)
    );
    assert_eq!(
        s.overrun_bytes, 0,
        "one raster is the budget exactly: {s:?}"
    );

    // A second slot over a budget of one raster evicts the raster and its
    // bytes with it.
    cache.ask(id(1), 0);
    let s = cache.stats();
    assert_eq!(
        (s.evicted_resident, s.evicted_bytes),
        (1, MARKER_BYTES + texture_bytes),
        "{s:?}"
    );
    assert_eq!(
        (s.resident_entries, s.resident_bytes),
        (1, MARKER_BYTES),
        "{s:?}"
    );
}

// ---------------------------------------------------------------------------

/// One pump decodes at most the budget; the rest wait their turn, in order.
///
/// The count is handed in rather than read off [`WASM_TILE_DECODES_PER_PUMP`].
/// That constant became the channel's own capacity plus one at WO-10 — a
/// backstop the queue can never make binding — so reading it here would make
/// the cap unobservable and the ordering claims below vacuous. What the
/// production value is stays pinned by
/// [`the_tuning_constants_are_the_written_figures`]; what this
/// holds is that the drain caps and orders correctly *given* a binding count.
#[test]
fn a_pump_decodes_at_most_the_budget_and_the_rest_wait_their_turn() {
    let ctx = Context::default();
    let png = fixture_png();
    let id = |x: u32| TileId { x, y: 0, zoom: 3 };

    /// A count small enough to bind against the backlog below.
    const FIXTURE_BUDGET: usize = 2;

    // More completed fetches than one pump may take, or the cap is untested.
    let queued: u32 = 5;
    assert!(
        (queued as usize) > FIXTURE_BUDGET,
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
            FIXTURE_BUDGET,
            // Far enough out that the count is the only thing that can bind.
            Instant::now() + Duration::from_secs(60),
            true,
            reported,
            |(tile_id, bytes): (TileId, Vec<u8>)| {
                // `Style::default()` for the reason `fetch_one` gives.
                #[allow(
                    clippy::default_constructed_unit_structs,
                    reason = "keeps compiling if walkers/mvt is ever enabled"
                )]
                let tile = Tile::new(&bytes, &Style::default(), tile_id.zoom, &ctx)
                    .expect("the fixture PNG should decode");
                drop(tile);
                decoded.push(tile_id);
                // `Tile::new` sniffed the body: no archive header declared
                // anything here, which is exactly what `Sniffed` names.
                super::take_ledger::TakeKind::Sniffed
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
    let mut budget = PumpBudget::new();

    assert_eq!(
        budget.open(1).budget,
        WASM_TILE_DECODES_PER_PUMP,
        "a fresh pass starts with the whole allowance"
    );
    budget.record(WASM_TILE_DECODES_PER_PUMP);
    assert_eq!(
        budget.open(1).budget,
        0,
        "a later call in the same pass gets nothing once the allowance is spent"
    );

    assert_eq!(
        budget.open(2).budget,
        WASM_TILE_DECODES_PER_PUMP,
        "the next pass restores the full allowance"
    );
    budget.record(1);
    assert_eq!(
        budget.open(2).budget,
        WASM_TILE_DECODES_PER_PUMP - 1,
        "a partial spend leaves exactly the difference for the same pass"
    );
}

/// **The time budget is the pass's, and every pump of that pass shares it.**
///
/// The count half of the allowance has been per-pass since it was written; the
/// time half was not. Each `drain_completed_fetches` computed its own
/// `Instant::now() + PUMP_TIME_BUDGET`, so a source drawn as a layer in six
/// panes opened six budgets in one frame and the frame's tile cost was
/// `panes x (budget + one take)` rather than `budget + one take`.
///
/// Asserted as an **identity**, never as a duration: the deadline two pumps of
/// one pass are handed is the same `Instant`, and a new pass's is a different
/// one. A wall-clock threshold here would be the "assert the property, not the
/// clock" defect — the property is which budget the second call is spending,
/// and that is an equality between two instants.
#[test]
fn the_pump_time_budget_is_the_passs_and_not_each_calls() {
    let mut budget = PumpBudget::new();

    // Nothing spent yet: an early layer that found an empty queue must not
    // have burnt the pass's deadline before the layer with work reaches it.
    let dry = budget.open(1);
    assert!(
        dry.first_take_free,
        "a pass that has taken nothing still owes its one unconditional take"
    );
    budget.record(0);
    assert!(
        budget.open(1).first_take_free,
        "a pump that took nothing must leave the pass's free take unspent, or \
         a layout whose first layer has an empty queue stalls the one behind it"
    );

    // The pass spends its free take. From here every later call in the pass is
    // the deadline's business.
    let first = budget.open(1);
    budget.record(1);

    let second = budget.open(1);
    assert_eq!(
        second.deadline, first.deadline,
        "the second pump of a pass opened a NEW time budget: a six-pane frame \
         bills six of them, which is the defect this pins"
    );
    assert!(
        !second.first_take_free,
        "the second pump of a pass re-armed the unconditional first take, so \
         the deadline above can never bind however far past it the frame is"
    );

    let third = budget.open(1);
    assert_eq!(
        third.deadline, second.deadline,
        "the deadline must be stamped once per pass, not once per call after \
         the first"
    );

    // A new pass is a new frame, and a new frame owes a fresh budget.
    let next_pass = budget.open(2);
    assert_ne!(
        next_pass.deadline, first.deadline,
        "a new pass reused the previous pass's deadline, so a frame inherits a \
         budget another frame already spent"
    );
    assert!(
        next_pass.first_take_free,
        "a new pass must restore the unconditional take, or a busy frame can \
         stall the map outright"
    );
}

/// The count and the time halves bound the pass **together**: whichever runs
/// out first stops it, and neither can be spent by re-entering.
///
/// The counterpart that keeps the test above non-vacuous. That one would pass
/// on a budget that never lets anything through at all; this one pins that a
/// pass with its allowance intact still hands out the whole of it.
#[test]
fn a_pass_hands_out_its_whole_allowance_across_however_many_pumps() {
    let mut budget = PumpBudget::new();

    // Spend the pass one take at a time, as a six-pane layout's six pumps do.
    let mut handed = 0usize;
    for _ in 0..WASM_TILE_DECODES_PER_PUMP {
        let allowance = budget.open(7);
        assert_eq!(
            allowance.budget,
            WASM_TILE_DECODES_PER_PUMP - handed,
            "the count left to the pass must be the allowance minus what it \
             has spent, however many calls spent it"
        );
        budget.record(1);
        handed += 1;
    }

    assert_eq!(
        handed, WASM_TILE_DECODES_PER_PUMP,
        "the pass handed out {handed} takes where its whole allowance is \
         {WASM_TILE_DECODES_PER_PUMP}: a bound that never lets the queue move \
         is a worse bug than the one it replaced",
    );
    assert_eq!(
        budget.open(7).budget,
        0,
        "the pass kept handing out takes past its own allowance"
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
    use std::path::PathBuf;

    use super::*;
    use crate::basemap_archive::FileRangeSource;

    /// The depth the committed Monaco extract declares in its own header.
    /// Named once so the two assertions below cannot drift apart.
    const FIXTURE_MAX_ZOOM: u8 = 14;

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
            super::super::BasemapStyling::keyless(crate::basemap_style::committed(is_dark)),
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx.clone(),
            super::super::SourceBudget {
                styled_bytes: 64 << 20,
                parsed_bytes: Some(64 << 20),
            },
            cache_ledger::CacheRole::Base,
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

        // The header is read by the IO task, so the depth may not be known yet
        // -- and "not known" is `None`, not a number. See
        // `tile_source::MAX_ZOOM_UNKNOWN` for what a stand-in zero drew.
        //
        // **Not `assert_eq!(.., None)`, and the difference is a race.** The IO
        // task is `runtime::spawn`ed by the constructor, so whether it has
        // already read this 419 KB local file by the time the next line runs is
        // a scheduling question, not a behavioural one. Asserting `None` here
        // asserts that the task has NOT finished, which is true on a loaded
        // machine and false on an idle one: it passed locally and failed in CI
        // with `Some(14)`.
        //
        // What the source actually promises is that it never claims a depth it
        // cannot serve. So both honest answers are allowed -- nothing yet, or
        // the archive's own depth -- and the stand-in zero this guards against
        // is neither.
        let before = tiles.source_max_zoom();
        assert!(
            before.is_none_or(|zoom| zoom == FIXTURE_MAX_ZOOM),
            "the source claimed depth {before:?}, which is neither \"not yet \
             known\" nor the archive's own {FIXTURE_MAX_ZOOM}"
        );

        let max_zoom = pump_until(DEFAULT_TIMEOUT, || {
            tiles.pump();
            tiles.source_max_zoom()
        })
        .expect("the archive header never arrived");
        // The race above is only safe because the settled value is pinned here:
        // if the depth were ever a stand-in, this is what would catch it.
        assert_eq!(
            max_zoom, FIXTURE_MAX_ZOOM,
            "the committed Monaco extract is built to zoom {FIXTURE_MAX_ZOOM}"
        );

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
            super::super::BasemapStyling::keyless(style),
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx.clone(),
            super::super::SourceBudget {
                styled_bytes: 64 << 20,
                parsed_bytes: Some(64 << 20),
            },
            cache_ledger::CacheRole::Base,
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

        tiles.set_style(super::super::BasemapStyling::keyless(after_style.clone()));

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
            super::super::BasemapStyling::keyless(crate::basemap_style::committed(true)),
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx.clone(),
            super::super::SourceBudget {
                styled_bytes: 64 << 20,
                parsed_bytes: Some(64 << 20),
            },
            cache_ledger::CacheRole::Base,
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

    // -----------------------------------------------------------------------
    // An archive that opens and then answers nothing
    // -----------------------------------------------------------------------

    /// **The rule itself, with no archive and no clock: a whole cohort of
    /// failures raises the verdict, and any single answer ends it.**
    ///
    /// Driven directly because the end-to-end tests below cannot pin it. They
    /// sample the verdict once per frame, so a rule that raised on one failure
    /// would raise and clear between two samples and go unnoticed; here every
    /// transition is observed at the moment it happens.
    #[test]
    fn the_failure_run_needs_a_whole_cohort_and_any_answer_ends_it() {
        assert_eq!(
            SUSTAINED_READ_FAILURES, MAX_PARALLEL_DOWNLOADS,
            "the run is a cohort of concurrently-issued reads; if the two \
             numbers part, the const's own reasoning no longer holds"
        );

        let failing = Arc::new(AtomicBool::new(false));
        let mut run = ReadFailureRun::new(Arc::clone(&failing));

        // One short of the rule, over and over, each stretch ended by an
        // answer: the transient, and it never reaches the glass.
        for round in 0..20 {
            for step in 0..SUSTAINED_READ_FAILURES - 1 {
                assert!(!run.failed(), "round {round} step {step}: verdict moved");
                assert!(
                    !failing.load(Ordering::SeqCst),
                    "round {round} step {step}: {} consecutive failures were \
                     reported as an archive that is not drawing",
                    step + 1,
                );
            }
            assert!(
                !run.answered(),
                "round {round}: an answer after a run that never raised the \
                 verdict is not a recovery"
            );
        }

        // The rule's own step is the one that raises it, and it says so once.
        for step in 0..SUSTAINED_READ_FAILURES - 1 {
            assert!(!run.failed(), "step {step}: raised early");
        }
        assert!(
            run.failed(),
            "the {SUSTAINED_READ_FAILURES}th consecutive failure is the rule, \
             and it must be the step that raises the verdict"
        );
        assert!(
            failing.load(Ordering::SeqCst),
            "the verdict is not published"
        );
        assert!(
            !run.failed(),
            "a run already reported must not report itself again; the line and \
             the repaint are per run, not per failed tile"
        );

        // And one answer is the whole of the recovery.
        assert!(run.answered(), "the recovery is not reported");
        assert!(
            !failing.load(Ordering::SeqCst),
            "a read answered and the archive is still called unreachable"
        );
        assert!(!run.answered(), "a second answer is not a second recovery");
    }

    /// `count` tile ids at `zoom` the committed fixture **provably holds**,
    /// read off the archive rather than guessed from the extract's bbox.
    ///
    /// The distinction is load-bearing for every test below. A coordinate the
    /// archive does not hold is answered `Absent` out of the root directory
    /// the `HashMapCache` has held since `open`, with no range read at all —
    /// so a killed source answers it *successfully*, and a failure rule that
    /// counts answers would have its run reset by exactly the tiles it was
    /// supposed to notice. Ids the archive holds cost a data read every time,
    /// which is what a killed source can fail.
    fn held_fixture_tiles(count: usize, zoom: u8) -> Vec<TileId> {
        let west = squallar_geo::lon_to_tile_x(MONACO_LON, zoom);
        let north = squallar_geo::lat_to_tile_y(MONACO_LAT, zoom);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            let archive = crate::basemap_archive::BasemapArchive::open(
                FileRangeSource::open(&fixture_path()).expect("the fixture opens"),
            )
            .await
            .expect("the fixture is a PMTiles archive");

            let mut held = Vec::new();
            for dy in 0..16u32 {
                for dx in 0..16u32 {
                    if held.len() == count {
                        return held;
                    }
                    let (x, y) = (west + dx, north + dy);
                    let present = archive
                        .tile(zoom, x, y)
                        .await
                        .is_ok_and(|bytes| bytes.into_bytes().is_some());
                    if present {
                        held.push(TileId { x, y, zoom });
                    }
                }
            }
            held
        })
    }

    /// Enough held ids for a failure run and a recovery run with margin: the
    /// rule needs [`MAX_PARALLEL_DOWNLOADS`] failures to raise, and each half
    /// below asks for far more than that.
    const HELD_IDS: usize = 48;

    /// Drive `state` for one frame's worth of tile work: the pump the drawing
    /// code runs once per layer, the `at` it runs per grid cell, and the
    /// per-frame `ensure_base_tiles` the panel runs before both.
    fn drive_frame(state: &mut crate::tiles::MapTileState, ids: &[TileId], ctx: &Context) {
        if let Some(tiles) = state.tiles.as_mut() {
            tiles.pump();
            for id in ids {
                let _ = tiles.at(*id);
            }
        }
        state.ensure_base_tiles(true, &std::collections::BTreeSet::new(), ctx);
    }

    /// A source over the committed fixture, in a `MapTileState`, with its
    /// header read and one real tile drawn — the state every test below
    /// starts from, so that none of them is testing the open-failure path.
    ///
    /// Answers the kill switch and the held ids beside the state.
    fn a_serving_basemap(
        test: &str,
        ctx: &Context,
    ) -> Option<(crate::tiles::MapTileState, Arc<AtomicBool>, Vec<TileId>)> {
        let (tiles, _reads, dead) =
            counted_fixture_tiles(test, ctx, crate::basemap_style::committed(true))?;
        let mut state = crate::tiles::MapTileState::default();
        state.tiles = Some(tiles);

        let max_zoom = pump_until(DEFAULT_TIMEOUT, || {
            let tiles = state.tiles.as_mut().expect("the slot holds the source");
            tiles.pump();
            tiles.source_max_zoom()
        })
        .expect("the archive header never arrived");
        assert_eq!(
            max_zoom, FIXTURE_MAX_ZOOM,
            "the fixture's own depth, or the ids below are for another archive"
        );

        let ids = held_fixture_tiles(HELD_IDS, FIXTURE_MAX_ZOOM);
        assert_eq!(
            ids.len(),
            HELD_IDS,
            "non-vacuity: the fixture yielded only {} tiles the archive holds, \
             so the runs below would be shorter than they read",
            ids.len(),
        );

        pump_until(DEFAULT_TIMEOUT, || {
            draw_one(
                state.tiles.as_mut().expect("the slot holds the source"),
                ids[0],
            )
        })
        .expect("the archive never served a tile, so it never opened");
        state.ensure_base_tiles(true, &std::collections::BTreeSet::new(), ctx);

        assert!(
            !state.base_archive_is_unreachable(),
            "control: an archive that is serving tiles must not read unreachable"
        );
        assert!(
            state.tiles.as_ref().and_then(HttpsTiles::fault).is_none(),
            "control: the archive opened, so the open-failure latch is not what \
             any assertion below is about"
        );

        Some((state, dead, ids))
    }

    /// **The headline: an archive that opens and then fails every tile read
    /// says so, instead of leaving a blank map under an OpenStreetMap credit.**
    ///
    /// The open-failure path was the only one that ever raised
    /// `base_archive_is_unreachable`, and per-tile read errors took a
    /// `log::warn!` and set nothing — so the credit corner kept naming a
    /// provider whose bytes were not on the glass. That is the state the web
    /// build was actually in: the header and the root directory fit inside the
    /// opening read, so the archive logged as healthily open and then answered
    /// nothing.
    ///
    /// What the panel paints off this is gated in `input_harness`; this is the
    /// bit it reads.
    #[test]
    fn an_archive_that_answers_no_tile_read_reports_the_basemap_unreachable() {
        let ctx = Context::default();
        let Some((mut state, dead, ids)) = a_serving_basemap(
            "an_archive_that_answers_no_tile_read_reports_the_basemap_unreachable",
            &ctx,
        ) else {
            return;
        };

        dead.store(true, Ordering::SeqCst);

        let latched = pump_until(DEFAULT_TIMEOUT, || {
            drive_frame(&mut state, &ids[1..HELD_IDS / 2], &ctx);
            state.base_archive_is_unreachable().then_some(())
        });
        assert!(
            latched.is_some(),
            "every tile read failed and the session still claims a reachable \
             basemap, so the credit corner is naming a provider that is not \
             drawing"
        );
        assert!(
            state.tiles.as_ref().and_then(HttpsTiles::fault).is_none(),
            "non-vacuity: the open-failure latch raised this, not the read \
             failures it is supposed to be about"
        );
    }

    /// **Recovery is automatic: reads that answer again take the credit back.**
    ///
    /// The ids asked for after the revival are ones this source has never
    /// been asked for, because a failed tile keeps its cache slot and is never
    /// re-requested (see `HttpsTiles::cache`). That is not a hole in the
    /// recovery: while nothing re-asks, nothing draws either, so the notice
    /// stays true for exactly as long as the map stays blank. A pan, a zoom
    /// or an eviction is what supplies the new ids in the field.
    #[test]
    fn a_basemap_that_answers_again_takes_its_credit_back() {
        let ctx = Context::default();
        let Some((mut state, dead, ids)) =
            a_serving_basemap("a_basemap_that_answers_again_takes_its_credit_back", &ctx)
        else {
            return;
        };

        dead.store(true, Ordering::SeqCst);
        pump_until(DEFAULT_TIMEOUT, || {
            drive_frame(&mut state, &ids[1..HELD_IDS / 2], &ctx);
            state.base_archive_is_unreachable().then_some(())
        })
        .expect("the read failures never raised the notice, so there is nothing to recover from");

        dead.store(false, Ordering::SeqCst);
        let cleared = pump_until(DEFAULT_TIMEOUT, || {
            drive_frame(&mut state, &ids[HELD_IDS / 2..], &ctx);
            (!state.base_archive_is_unreachable()).then_some(())
        });
        assert!(
            cleared.is_some(),
            "the archive is answering again and the credit corner is still \
             calling it unreachable"
        );

        // And it is drawing, not merely quiet: the notice cleared because
        // tiles arrived.
        assert!(
            pump_until(DEFAULT_TIMEOUT, || draw_one(
                state.tiles.as_mut().expect("the slot holds the source"),
                ids[HELD_IDS / 2]
            ))
            .is_some(),
            "non-vacuity: the recovered source never drew a tile"
        );
    }

    /// A source over the committed fixture that fails a fixed window of range
    /// reads and serves every other one — a transient, not a dead host.
    ///
    /// The window is counted in **range** reads, so it fails *at most* that
    /// many tile reads: a tile costs one data read after its directories are
    /// cached, and the first failure aborts the tile rather than retrying
    /// inside it. At most is the direction the control needs.
    struct FlakyRangeSource {
        inner: FileRangeSource,
        reads: Arc<std::sync::atomic::AtomicUsize>,
        window: std::ops::Range<usize>,
        fired: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl crate::basemap_archive::RangeSource for FlakyRangeSource {
        async fn read_range(
            &self,
            offset: u64,
            length: usize,
        ) -> Result<Vec<u8>, crate::basemap_archive::RangeError> {
            let nth = self.reads.fetch_add(1, Ordering::SeqCst);
            if self.window.contains(&nth) {
                self.fired.fetch_add(1, Ordering::SeqCst);
                return Err(crate::basemap_archive::RangeError::Transport(
                    "one read went missing".to_owned(),
                ));
            }
            self.inner.read_range(offset, length).await
        }
    }

    /// **The cry-wolf control: a few failed tiles among neighbours that are
    /// drawing is not an unreachable archive.**
    ///
    /// Both windows are shorter than the run the rule asks for, and the window
    /// starts after the reads `open` costs, so the archive is open and serving
    /// throughout — this is the transient the product rule refuses to
    /// apologise for, not a dead host. It passes on the tree that had no rule
    /// at all, which is the point: it is the property the fix must not break.
    ///
    /// **It cannot pin the threshold, and it is not what does.** The verdict is
    /// sampled once per frame, so a rule that raised on one failure would raise
    /// and clear again between two samples and this would still read green —
    /// measured 2026-08-30 by setting `SUSTAINED_READ_FAILURES` to 1, which
    /// left this test passing. The threshold's own gate is
    /// [`the_failure_run_needs_a_whole_cohort_and_any_answer_ends_it`], which
    /// drives the rule directly and has no clock in it.
    #[test]
    fn a_few_failed_tiles_among_neighbours_that_draw_is_not_unreachable() {
        for width in [1, MAX_PARALLEL_DOWNLOADS - 1] {
            let ctx = Context::default();
            let path = fixture_path();
            let Ok(inner) = FileRangeSource::open(&path) else {
                eprintln!(
                    "SKIPPED a_few_failed_tiles_among_neighbours_that_draw_is_not_unreachable: \
                     {} would not open. It is committed; `git status` on it.",
                    path.display()
                );
                return;
            };

            // Far enough in that `open`'s own reads are behind it, so the
            // archive is open and the window falls on tile reads.
            let first_bad = 8;
            let fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let tiles = HttpsTiles::from_range_source(
                FlakyRangeSource {
                    inner,
                    reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    window: first_bad..first_bad + width,
                    fired: Arc::clone(&fired),
                },
                super::super::BasemapStyling::keyless(crate::basemap_style::committed(true)),
                Attribution {
                    text: "test",
                    url: "https://example.invalid/",
                    logo_light: None,
                    logo_dark: None,
                },
                ctx.clone(),
                super::super::SourceBudget {
                    styled_bytes: 64 << 20,
                    parsed_bytes: Some(64 << 20),
                },
                cache_ledger::CacheRole::Base,
                None,
                crate::tile_source::ArchiveStores {
                    seed: None,
                    offline: None::<crate::basemap_download::PlatformSegmentStore>,
                    offline_archive: crate::basemap_download::AreaArchive::Basemap,
                },
            );

            let mut state = crate::tiles::MapTileState::default();
            state.tiles = Some(tiles);

            pump_until(DEFAULT_TIMEOUT, || {
                let tiles = state.tiles.as_mut().expect("the slot holds the source");
                tiles.pump();
                tiles.source_max_zoom()
            })
            .expect("the archive header never arrived");

            let ids = held_fixture_tiles(HELD_IDS, FIXTURE_MAX_ZOOM);
            assert_eq!(ids.len(), HELD_IDS, "non-vacuity: too few held ids");

            // Ask for the whole set until the window has fired and the
            // neighbours are drawing, then keep watching for SETTLE.
            let drew = pump_until(DEFAULT_TIMEOUT, || {
                drive_frame(&mut state, &ids, &ctx);
                let drawn = ids
                    .iter()
                    .filter(|id| {
                        state
                            .tiles
                            .as_mut()
                            .expect("the slot holds the source")
                            .at(**id)
                            .is_some()
                    })
                    .count();
                (fired.load(Ordering::SeqCst) == width && drawn >= HELD_IDS / 2).then_some(drawn)
            });
            assert!(
                drew.is_some(),
                "non-vacuity for width {width}: the window fired {} times and \
                 the neighbours never drew, so nothing was under test",
                fired.load(Ordering::SeqCst),
            );

            let deadline = Instant::now() + SETTLE;
            while Instant::now() < deadline {
                drive_frame(&mut state, &ids, &ctx);
                assert!(
                    !state.base_archive_is_unreachable(),
                    "width {width}: {} failed reads among neighbours that are \
                     drawing was reported as an unreachable archive",
                    fired.load(Ordering::SeqCst),
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// The archive decode seam: the header's `tile_type`, never the bytes,
/// decides how a body becomes a [`Tile`]. Two committed fixtures carry the
/// two real archives' shapes — Monaco (`tile_type = 1`, MVT) and the
/// single-tile terrain wrap (`tile_type = 4`, the real Kansas WebP inside).
mod archive_decode {
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

    /// **A decode that succeeded reaches the basemap ledger, under the kind
    /// the header declared.**
    ///
    /// This is the counter the Tier-2 rig now gates on
    /// (`--expect-basemap-tiles`, `vector_tiles > 0`), and the whole point of
    /// it is that a basemap decoding nothing is otherwise invisible: it was
    /// invisible for the entire life of a shipped build. So the increment
    /// itself needs a reading, or the gate is downstream of an unpinned line.
    ///
    /// **Deltas, and strict-increase deltas at that, never absolute values.**
    /// The counters are process-global statics and these tests run in
    /// parallel, so an equality on a total would be asserting what the rest of
    /// the binary happened to be doing. `after > before` is monotone and
    /// cannot be flaked by a concurrent decode — while still failing outright
    /// if the increment were deleted, or wired to the other counter, which is
    /// the pair of mistakes it exists to catch. What is deliberately NOT
    /// asserted here is the other half — that a refused decode counts nothing
    /// — because that IS an equality on a global and would flake; it is
    /// structural instead: `decode_archive_tile` reaches `note_archive_decode`
    /// only past a `?`.
    #[test]
    fn a_decoded_body_lands_in_the_basemap_ledger_under_its_declared_kind() {
        let ctx = Context::default();

        if let Some((kind, bytes)) = kind_and_tile("monaco.pmtiles", 14, 8529, 5974) {
            assert_eq!(kind, ArchiveTileKind::Vector);
            let before = crate::basemap_ledger::totals();
            decode_archive_tile(
                &bytes,
                kind,
                &crate::basemap_style::committed(true),
                14,
                &ctx,
            )
            .expect("the MVT body tessellates");
            let after = crate::basemap_ledger::totals();
            assert!(
                after.vector_tiles > before.vector_tiles,
                "an MVT body decoded and the basemap ledger's vector counter \
                 did not move ({} -> {}); --expect-basemap-tiles then reads a \
                 live basemap as dead on every Tier-2 leg",
                before.vector_tiles,
                after.vector_tiles,
            );
        }

        if let Some((kind, bytes)) = kind_and_tile("terrain-hillshade-mini.pmtiles", 10, 224, 395) {
            assert_eq!(kind, ArchiveTileKind::Hillshade);
            let before = crate::basemap_ledger::totals();
            decode_archive_tile(&bytes, kind, &Style::default(), 10, &ctx)
                .expect("the WebP body decodes");
            let after = crate::basemap_ledger::totals();
            assert!(
                after.raster_tiles > before.raster_tiles,
                "a raster body decoded and the basemap ledger's raster \
                 counter did not move ({} -> {})",
                before.raster_tiles,
                after.raster_tiles,
            );
        }
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
            super::super::BasemapStyling::keyless(std::sync::Arc::new(Style::default())),
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx,
            super::super::SourceBudget {
                styled_bytes: 8 << 20,
                parsed_bytes: None,
            },
            cache_ledger::CacheRole::Base,
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
            super::super::BasemapStyling::keyless(std::sync::Arc::new(Style::default())),
            Attribution {
                text: "test",
                url: "https://example.invalid/",
                logo_light: None,
                logo_dark: None,
            },
            ctx,
            super::super::SourceBudget {
                styled_bytes: 8 << 20,
                parsed_bytes: None,
            },
            cache_ledger::CacheRole::Base,
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

/// **Both halves of a vector decode are charged to the phase ledger.**
///
/// The count gate on the real path, and the companion to
/// `take_ledger::tests::the_drain_charges_every_take_to_the_ledger`. Without
/// it, deleting either `note_vector_phase` call left the whole suite green
/// while the reported `tile phase (…)` line quietly became an empty figure
/// that still reads as one — which is exactly the failure the phase split
/// exists to make impossible.
///
/// **Asserted as `>= REPEATS`, not `== REPEATS`, and that is deliberate.** The
/// histograms are process-global and the harness runs cases in parallel, so a
/// concurrent vector decode can only ever ADD to the window. A lower bound is
/// therefore sound where an equality would be a race, and it still fails hard
/// when the recording is removed: the delta is then zero and `REPEATS` is not.
/// The repeat count is the non-triviality floor — a single call could be
/// masked by one concurrent decode, eight cannot be.
#[test]
fn both_halves_of_a_vector_decode_reach_the_phase_ledger() {
    use crate::tile_source::take_ledger::{VectorPhase, phase_totals};

    const REPEATS: u64 = 8;

    // An empty body is a valid empty protobuf, so this exercises the real
    // `walkers::mvt::parse` and `walkers::mvt::styled` rather than a stub,
    // without needing a fixture whose content could drift.
    let before = phase_totals();
    for _ in 0..REPEATS {
        let parsed = super::timed_parse(&[]).expect("an empty MVT body is an empty tile");
        let _ = super::timed_styled(&parsed, &Style::default(), 0);
    }
    let window = phase_totals().diff(&before);

    for phase in [VectorPhase::Parse, VectorPhase::Style] {
        assert!(
            window.phase(phase).total() >= REPEATS,
            "{} decodes ran and the `{}` phase recorded only {} — a cost the \
             frame paid that no figure can see",
            REPEATS,
            phase.label(),
            window.phase(phase).total(),
        );
    }
}

/// The dispatch gate, as a property rather than as a browser session.
///
/// [`TileBatch`] is compiled on native for exactly this reason. The rule it
/// holds — a pass either offloads its vector bodies or does *precisely* what
/// this arm did before the offload existed — is the whole of what makes the
/// change a win-or-no-op rather than a trade, and a rule spelled inline in a
/// `cfg(target_arch = "wasm32")` function body could only ever be exercised by
/// a human with a browser open.
mod dispatch_gate {
    use super::super::TileBatch;
    use super::*;

    fn body(n: u8) -> Arc<Vec<u8>> {
        Arc::new(vec![n; 4])
    }

    fn a_tile(x: u32) -> TileId {
        TileId {
            zoom: 14,
            x,
            y: 5974,
        }
    }

    /// **The property the whole change rests on.** Every reason not to
    /// offload has to land on the inline path, because that path is what
    /// ships today: an idle funnel is the only state that stages.
    #[test]
    fn only_an_idle_funnel_stages_and_every_other_answer_falls_through() {
        let mut batch = TileBatch::default();

        // No offloader installed, or no worker attached, or still inside the
        // handshake window: `queued` is `None` and posting would run the job
        // on this very thread, unbudgeted.
        assert!(
            !batch.should_stage(None),
            "a funnel with no worker must never be staged for: posting there \
             runs the batch inline and unbudgeted, which is worse than the \
             pump decoding it under PUMP_TIME_BUDGET",
        );

        // Somebody else's job is in the worker — a 160-190 ms radar
        // rasterization, or an unbounded Level II decode. A batch posted now
        // appears later than a tile decoded here.
        for busy in [1usize, 2, 7] {
            assert!(
                !batch.should_stage(Some(busy)),
                "{busy} queued job(s) was staged for; a batch behind one \
                 makes tiles appear LATER, which is the objection the gate \
                 exists to answer",
            );
        }

        // Nothing owed at all: the one state that offloads.
        assert!(
            batch.should_stage(Some(0)),
            "an idle funnel must stage, or the offload never happens",
        );

        // With a batch already out, "the funnel holds exactly mine" is the
        // same answer as idle — so a second pass keeps staging instead of
        // falling back to a tessellation it does not need to pay.
        batch.stage(a_tile(1), body(1));
        let _ = batch.open(0);
        assert!(
            batch.should_stage(Some(1)),
            "a funnel holding only this source's own batch must keep staging",
        );
        assert!(
            !batch.should_stage(Some(2)),
            "a funnel holding this source's batch AND something else must not",
        );
        assert!(
            !batch.should_stage(None),
            "and a dead funnel still must not"
        );
    }

    /// At most one batch is outstanding. That is what keeps a backlog from
    /// queueing several deep behind a radar rasterization.
    #[test]
    fn only_one_batch_is_ever_outstanding() {
        let mut batch = TileBatch::default();
        batch.stage(a_tile(1), body(1));
        assert!(batch.ready_to_post());

        let opened = batch.open(0);
        assert_eq!(opened.len(), 1);
        assert!(
            !batch.ready_to_post(),
            "a second batch was offered while the first was still out",
        );

        // More arrive while it is in flight; they wait rather than posting.
        batch.stage(a_tile(2), body(2));
        assert!(
            !batch.ready_to_post(),
            "bodies staged during a flight must wait for it to land",
        );

        batch.close();
        assert!(
            batch.ready_to_post(),
            "the batch retired and its successor is still not offered",
        );
        assert_eq!(batch.open(0).len(), 1, "the successor carries what waited");
    }

    /// An empty staging list is not a batch. A post with no tiles would spend
    /// a round trip to be told nothing.
    #[test]
    fn nothing_staged_is_never_posted() {
        let mut batch = TileBatch::default();
        assert!(!batch.ready_to_post());
        batch.stage(a_tile(1), body(1));
        assert!(batch.ready_to_post());
        assert!(batch.take_one_staged().is_some());
        assert!(
            !batch.ready_to_post(),
            "reclaiming for the inline path left a phantom batch behind",
        );
        assert!(
            batch.take_one_staged().is_none(),
            "an empty staging list must reclaim nothing",
        );
    }

    /// **The gate can shut between a body being staged and a batch going
    /// out**, and those bodies must not be stranded: the pass that follows
    /// decodes the CHANNEL's arrivals inline and would step straight over the
    /// staging list. `take_one_staged` is what the pump reclaims them with,
    /// oldest first, one per pass.
    ///
    /// This was a live hole until a dead-code warning on the unused reclaim
    /// method exposed it — the method existed and nothing called it, so a
    /// worker that died after a stage left those tiles undrawn forever.
    #[test]
    fn a_body_staged_before_the_gate_shut_is_reclaimable_oldest_first() {
        let mut batch = TileBatch::default();
        batch.stage(a_tile(1), body(1));
        batch.stage(a_tile(2), body(2));

        let (first, bytes) = batch.take_one_staged().expect("one was staged");
        assert_eq!(first, a_tile(1), "the reclaim is not oldest-first");
        assert_eq!(*bytes, vec![1u8; 4], "the wrong body came back");
        assert_eq!(
            batch.take_one_staged().map(|(id, _)| id),
            Some(a_tile(2)),
            "the second body was not reclaimable",
        );
        assert!(batch.take_one_staged().is_none());
    }

    /// The bodies are **retained** while the batch is out, so a tile the
    /// reply does not carry is decoded from the copy here rather than
    /// refetched from the archive.
    ///
    /// This is what makes the row's `None` arm cost a slower tile and never a
    /// missing one.
    #[test]
    fn an_outstanding_batch_keeps_the_bodies_it_sent() {
        let mut batch = TileBatch::default();
        batch.stage(a_tile(1), body(11));
        batch.stage(a_tile(2), body(22));
        let sent = batch.open(3);
        assert_eq!(sent.len(), 2);

        let (epoch, asked) = batch.close().expect("a batch was outstanding");
        assert_eq!(epoch, 3, "the generation it was posted under comes back");
        assert_eq!(asked.len(), 2, "both bodies came back with it");
        assert_eq!(
            *asked[0].1,
            vec![11u8; 4],
            "the body that came back is not the body that went out",
        );
        // And they are the same allocations, not copies: the retention is a
        // pointer each, which is what makes it affordable.
        assert!(Arc::ptr_eq(&sent[0].1, &asked[0].1));
    }

    /// Closing with nothing outstanding answers `None` rather than
    /// fabricating a batch — the state a reply arriving after a source reset
    /// lands in.
    #[test]
    fn closing_nothing_answers_nothing() {
        let mut batch = TileBatch::default();
        assert!(batch.close().is_none());
        batch.stage(a_tile(1), body(1));
        batch.open(0);
        assert!(batch.close().is_some());
        assert!(batch.close().is_none(), "a batch was retired twice");
    }
}

/// The offloader seam itself: installing one, seeing it, and taking it away.
///
/// **`squallar-egui` cannot name the funnel**, so this is the only place the
/// install path is exercised at all — the real implementation lives in
/// `squallar-app` and only compiles for wasm32. Without this the seam would be
/// dead code on every native build, which is exactly what clippy said.
mod offloader_seam {
    use super::super::{TileOffloader, clear_tile_offloader, set_tile_offloader, with_offloader};

    /// An offloader that reports a fixed queue depth and accepts nothing.
    struct Fixed(Option<usize>);

    impl TileOffloader for Fixed {
        fn queued(&self) -> Option<usize> {
            self.0
        }

        fn post(
            &self,
            _job: squallar_basemap::jobs::BasemapTilesJob,
            _deliver: Box<dyn FnOnce(Option<squallar_basemap::jobs::BasemapTiles>) + Send>,
        ) -> bool {
            false
        }
    }

    /// With nothing installed the pump sees `None`, which is its instruction
    /// to decode on this thread — the state every native build is in for the
    /// life of the process, and the reason the offload is opt-in rather than
    /// something a target has to switch off.
    #[test]
    fn nothing_installed_reads_as_no_funnel() {
        clear_tile_offloader();
        assert_eq!(
            with_offloader(|o| o.and_then(TileOffloader::queued)),
            None,
            "an uninstalled seam must read as `no worker`, not as an idle one",
        );
    }

    /// An installed offloader is what the pump reads, and clearing puts it
    /// back.
    #[test]
    fn an_installed_offloader_is_what_the_pump_reads() {
        clear_tile_offloader();
        set_tile_offloader(Box::new(Fixed(Some(0))));
        assert_eq!(
            with_offloader(|o| o.and_then(TileOffloader::queued)),
            Some(0)
        );

        // Replacing rather than stacking: the second install is what answers.
        set_tile_offloader(Box::new(Fixed(Some(3))));
        assert_eq!(
            with_offloader(|o| o.and_then(TileOffloader::queued)),
            Some(3)
        );

        // And an installed offloader may still report no funnel — a worker
        // that died between frames.
        set_tile_offloader(Box::new(Fixed(None)));
        assert_eq!(with_offloader(|o| o.and_then(TileOffloader::queued)), None);

        clear_tile_offloader();
        assert_eq!(with_offloader(|o| o.and_then(TileOffloader::queued)), None);
    }
}

/// **A tile installed from a worker batch must count on the page's basemap
/// ledger.**
///
/// Held as a source scrape because nothing else can hold it: the installer is
/// `cfg(target_arch = "wasm32")`, so no native test executes it, and the
/// counter it feeds is a process static the browser rig reads as its basemap
/// positive control (`basemap tiles: N vector`). Miss it and an offloading leg
/// reports zero decoded tiles while the map draws perfectly — which is
/// indistinguishable from a leg where the basemap genuinely never decoded, and
/// that is the exact reading a measurement of this change must not produce for
/// the wrong reason.
///
/// The worker parses the body, but the worker's statics are not the page's.
#[test]
fn a_batch_installed_tile_is_counted_on_the_basemap_ledger() {
    const SOURCE: &str = include_str!("../tile_source.rs");

    let (_, after) = SOURCE
        .split_once("fn install_batch_reply(&mut self)")
        .expect("`install_batch_reply` is no longer written here");
    let body = after
        .split_once("\n    }")
        .map(|(body, _)| body)
        .expect("`install_batch_reply` has no recognisable body");

    // Control: the scrape is over the right function.
    assert!(
        body.contains("self.cache.put("),
        "control: `install_batch_reply` no longer puts into the cache, so the \
         check below is reading the wrong function",
    );
    assert!(
        body.contains("note_archive_decode("),
        "`install_batch_reply` installs tiles without calling \
         `note_archive_decode`. The page's `basemap tiles:` counter is what \
         the browser rig asserts the basemap decoded at all; a worker-decoded \
         tile that skips it makes a working map read as a dead archive.",
    );
}
