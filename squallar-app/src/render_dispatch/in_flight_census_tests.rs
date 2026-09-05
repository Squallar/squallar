//! The `renders in flight` census family: a reply's raster is priced from the
//! moment the reply is sent until the frame thread's receipt marks the render
//! finished, and at nothing else.
//!
//! On the measured scene a 206.8 MiB `ColorImage` sat in the reply channel at
//! the heap's peak and no family named it — `render cache` prices an image
//! only once receipt installs it, `upload pending` only once the renderer
//! bands it. These tests pin the gap between those two: the figure rises by
//! exactly the image's pixels when the reply is sent, holds until the receipt,
//! and is zero for a reply that drew nothing or a render nobody wanted.

use super::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// **Serialises every test here that touches process-global census state.**
///
/// The census families are statics and radar's pools are process-wide, so two
/// of these tests on two harness threads would each read the other's writes.
/// Nothing else in this crate's test binary parks a radar buffer or publishes
/// `renders in flight` (the gated renders in `render_invalidation_tests`
/// answer an empty raster, which prices nothing), so this lock is the whole
/// of the exclusion. A poisoned lock is read as a live one: a panicking test
/// has already failed and must not cascade into every sibling.
static CENSUS_STATIC: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn census_guard() -> std::sync::MutexGuard<'static, ()> {
    CENSUS_STATIC
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A side `plan_view_image` accepts on this build — the loop-frame side is the
/// floor of the accepted bracket, and the cheapest.
const SIDE: usize = squallar_device_profile::constants::LOOP_IMAGE_SIZE;

/// What that raster costs as a `ColorImage`: one `Color32` a pixel.
const IMAGE_BYTES: u64 = (SIDE * SIDE * std::mem::size_of::<egui::Color32>()) as u64;

/// A render that does not finish until the test releases it, and then answers
/// a raster of [`SIDE`]².
fn gated_render() -> (mpsc::Sender<()>, squallar_worker::offload::Job) {
    let (release, held) = mpsc::channel::<()>();
    (
        release,
        squallar_worker::offload::Job::Opaque(Box::new(move || {
            held.recv().expect("every gated render is released");
            Some(squallar_source::job::DescribedOut(Box::new(
                squallar_radar::frame::RenderedFrame {
                    image: squallar_radar::frame::RasterImage::Bytes(vec![0u8; SIDE * SIDE * 4]),
                    max_range_km: 230.0,
                    polar: Default::default(),
                    nyquist_ms: None,
                    melting_layer_source: None,
                    storm_motion: None,
                },
            )))
        })),
    )
}

/// [`gated_render`] for a render that finds no sweep to draw.
fn gated_nothing() -> (mpsc::Sender<()>, squallar_worker::offload::Job) {
    let (release, held) = mpsc::channel::<()>();
    (
        release,
        squallar_worker::offload::Job::Opaque(Box::new(move || {
            held.recv().expect("every gated render is released");
            None
        })),
    )
}

fn dispatch(
    d: &mut RenderDispatcher,
    results: &mpsc::Sender<RenderResponse>,
    job: squallar_worker::offload::Job,
) {
    d.spawn_render(
        0,
        "KOUN",
        RadarProduct::Reflectivity,
        0.5,
        results.clone(),
        None,
        job,
    );
}

/// The level once it reaches `want`, or whatever it reads when a generous
/// deadline passes — so a figure that never arrives fails an assertion with
/// the value it stuck at rather than hanging the run. A wait for a positive
/// event, not a timing claim.
fn level_once(d: &RenderDispatcher, want: u64) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let level = d.in_flight_image_bytes();
        if level == want || Instant::now() > deadline {
            return level;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The whole life of one in-flight raster: unpriced while the render runs,
/// priced at exactly its pixels from the send, still priced while the reply
/// sits received-but-unfinished on the frame thread, and gone the moment the
/// receipt marks the pane's render finished.
#[test]
fn a_reply_is_priced_from_its_send_to_its_receipt() {
    let _census = census_guard();
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    assert_eq!(d.in_flight_image_bytes(), 0, "premise: nothing in flight");

    let (release, job) = gated_render();
    dispatch(&mut d, &results, job);
    assert_eq!(
        d.in_flight_image_bytes(),
        0,
        "a render still running has no raster to price, yet one is on the level"
    );

    release.send(()).expect("the render is still running");
    let sent = level_once(&d, IMAGE_BYTES);
    assert_eq!(
        sent, IMAGE_BYTES,
        "the reply's raster is {IMAGE_BYTES} B of pixels and the level reads {sent} B"
    );

    // Received by the frame thread but not yet marked finished: the raster is
    // still nobody else's to price.
    let rr = rx.recv().expect("the reply arrives");
    assert_eq!(
        in_flight_bytes(rr.rendered.as_ref()),
        IMAGE_BYTES as usize,
        "the reply carries a raster of another size than the one priced"
    );
    assert_eq!(
        d.in_flight_image_bytes(),
        IMAGE_BYTES,
        "the level fell before the receipt marked the render finished"
    );

    d.pane_render[rr.pane_idx].render_finished();
    assert_eq!(
        d.in_flight_image_bytes(),
        0,
        "the raster stayed priced after the frame thread took it: {} B",
        d.in_flight_image_bytes()
    );
}

/// A reply with nothing drawn is still a reply — it clears the pane's
/// in-flight flag — but it carries no raster, and the level must say zero
/// throughout rather than pricing a `None`.
#[test]
fn a_reply_that_drew_nothing_prices_nothing() {
    let _census = census_guard();
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let (release, job) = gated_nothing();
    dispatch(&mut d, &results, job);
    release.send(()).expect("the render is still running");
    let rr = rx
        .recv()
        .expect("a render that finds nothing still reports back");
    assert!(rr.rendered.is_none(), "premise: this reply drew nothing");
    assert_eq!(
        d.in_flight_image_bytes(),
        0,
        "a reply with no raster put {} B on the level",
        d.in_flight_image_bytes()
    );
    d.pane_render[rr.pane_idx].render_finished();
    assert_eq!(d.in_flight_image_bytes(), 0);
}

/// An abandoned render sends nothing, so it has nothing in flight: the
/// raster it drew is dropped inside the closure and must never reach the
/// level. This is the arm that would leave a phantom if the price were taken
/// before the `wanted` check.
#[test]
fn an_abandoned_render_prices_nothing() {
    let _census = census_guard();
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let (release, job) = gated_render();
    dispatch(&mut d, &results, job);
    d.reset_panes();
    release.send(()).expect("the render is still running");
    drop(results);
    assert_eq!(
        rx.iter().count(),
        0,
        "premise: an abandoned render stayed silent"
    );
    assert_eq!(
        d.in_flight_image_bytes(),
        0,
        "an abandoned render's raster is priced at {} B with no reply to carry it",
        d.in_flight_image_bytes()
    );
}

/// The speculative arm has no pane; its cell is the dispatcher's and its
/// receipt is `speculative_finished`. Driven at the cell, since a speculative
/// dispatch needs a decoded volume: what is pinned is that the receipt zeroes
/// exactly this cell and that the fold counts it.
#[test]
fn the_speculative_receipt_zeroes_its_own_cell() {
    let _census = census_guard();
    let d = RenderDispatcher::new();
    d.speculative_reply_bytes
        .fetch_add(IMAGE_BYTES as usize, Ordering::Relaxed);
    assert_eq!(
        d.in_flight_image_bytes(),
        IMAGE_BYTES,
        "the fold does not count the speculative cell"
    );
    let mut d = d;
    d.speculative_finished();
    assert_eq!(
        d.in_flight_image_bytes(),
        0,
        "the speculative receipt left its raster priced"
    );
}

/// **The publish door itself**: what the allocation-error hook would read.
///
/// The four tests above read the dispatcher's own fold, which is the truth
/// the level is reconciled against — but a fold nobody publishes is a figure
/// nobody can see, and deleting the publish left all four green. This one
/// asserts the census STATIC, the thing `heap_census::census()` hands the
/// hook, and it never runs a telemetry tick: both families must be current
/// off the seams alone.
///
/// That is the whole defect. A render reply lives about one frame — the
/// closure sends and the next `poll_render_results` takes it, before the 2 s
/// tick ever runs — and on wasm the closure runs from the worker's own task,
/// so a tick-published level would read zero for a 206.8 MiB image sitting in
/// the channel at the exact moment the allocator refused.
#[test]
fn the_census_static_carries_the_reply_off_the_seam_with_no_tick() {
    let _census = census_guard();
    use squallar_egui::heap_census::census;

    // Premise. Only this module's guarded tests ever publish this family, and
    // the guard is held, so a clear level here is a level this test owns.
    squallar_egui::heap_census::set_render_in_flight_bytes(0);
    assert_eq!(census().render_in_flight_bytes, 0, "premise: nothing sent");

    // **Off the closure's own publish.** No tick, no `publish_heap_census`:
    // the send is what must put the figure on the static, and the receipt is
    // what must take it off.
    let mut d = RenderDispatcher::new();
    let (results, rx) = mpsc::channel();
    let (release, job) = gated_render();
    dispatch(&mut d, &results, job);
    release.send(()).expect("the render is still running");
    let level = level_once(&d, IMAGE_BYTES);
    assert_eq!(
        level, IMAGE_BYTES,
        "premise: the dispatcher's own fold never reached the reply's bytes"
    );
    assert_eq!(
        census().render_in_flight_bytes,
        IMAGE_BYTES,
        "the reply is in the channel and the census static reads {} B, not its \
         {IMAGE_BYTES} B — the seam does not publish, so the hook reads zero for a \
         raster that is resident",
        census().render_in_flight_bytes
    );

    let rr = rx.recv().expect("the reply arrives");
    d.pane_render[rr.pane_idx].render_finished();
    assert_eq!(
        census().render_in_flight_bytes,
        0,
        "the receipt left {} B on the census static after the frame thread took the \
         raster",
        census().render_in_flight_bytes
    );
}

/// **The loop producer's seam**, which the family named but did not count.
///
/// `App::spawn_loop_frame_render` is the third builder of in-flight rasters
/// and on a looping scene the largest population — one `ColorImage` per frame
/// per pane. Its ends live in `app_fetch` and `app_render`, so what is pinned
/// here is the pair they call: the ticket prices a raster onto the census
/// static before the send, and the receipt settles **that response's own
/// bytes**, not the pane's whole slate.
///
/// The per-response half is the one with teeth. A loop pane has many frames
/// in flight at once, so a receipt that cleared a cell — the shape the pane
/// renders use — would drop every sibling reply still in the channel and
/// report zero while megabytes were resident.
#[test]
fn a_loop_reply_is_priced_by_its_ticket_and_settled_per_response() {
    let _census = census_guard();
    use squallar_egui::heap_census::census;

    let d = RenderDispatcher::new();
    squallar_egui::heap_census::set_render_in_flight_bytes(0);
    assert_eq!(d.in_flight_image_bytes(), 0, "premise: nothing in flight");

    // Two frames of different sizes, so a settle that takes the wrong one is
    // visible rather than symmetric.
    let first = loop_image_bytes(Some(&egui::ColorImage::new(
        [SIDE, SIDE],
        vec![egui::Color32::BLACK; SIDE * SIDE],
    )));
    let second = loop_image_bytes(Some(&egui::ColorImage::new(
        [SIDE / 2, SIDE / 2],
        vec![egui::Color32::BLACK; (SIDE / 2) * (SIDE / 2)],
    )));
    assert!(
        first > second && second > 0,
        "the two frames must differ for this test to distinguish them"
    );

    let ticket = d.loop_reply_ticket();
    ticket.price(first);
    assert_eq!(
        census().render_in_flight_bytes,
        first as u64,
        "a loop reply's raster reached the channel and the census static reads {} B, \
         not its {first} B",
        census().render_in_flight_bytes
    );
    assert_eq!(
        d.in_flight_image_bytes(),
        first as u64,
        "the fold the tick reconciles against does not count the loop cell, so the next \
         tick would erase this raster from the level"
    );

    // A second frame in flight beside the first.
    ticket.price(second);
    assert_eq!(
        census().render_in_flight_bytes,
        (first + second) as u64,
        "two loop frames in flight and the level is {} B",
        census().render_in_flight_bytes
    );

    // One response taken: only its bytes leave.
    d.settle_loop_reply(first);
    assert_eq!(
        census().render_in_flight_bytes,
        second as u64,
        "settling one loop response left {} B where the other frame's {second} B are \
         still in the channel",
        census().render_in_flight_bytes
    );

    d.settle_loop_reply(second);
    assert_eq!(
        census().render_in_flight_bytes,
        0,
        "the last loop response settled and {} B stayed on the level",
        census().render_in_flight_bytes
    );
    assert_eq!(d.in_flight_image_bytes(), 0);
}

/// A loop reply that drew nothing prices nothing — the failure arm of
/// `spawn_loop_frame_render`, which sends a response with no image.
#[test]
fn a_loop_reply_that_drew_nothing_prices_nothing() {
    let _census = census_guard();
    assert_eq!(loop_image_bytes(None), 0);

    let d = RenderDispatcher::new();
    squallar_egui::heap_census::set_render_in_flight_bytes(0);
    d.loop_reply_ticket().price(loop_image_bytes(None));
    assert_eq!(
        squallar_egui::heap_census::census().render_in_flight_bytes,
        0,
        "a loop reply with no image put bytes on the level"
    );
    assert_eq!(d.in_flight_image_bytes(), 0);
}

/// **Both ends of the loop seam are actually called.**
///
/// The two tests above drive the ticket and the settle directly, so they pin
/// the pair's arithmetic but would stay green if nobody called them — the
/// exact blindness that let the first version of this commit publish a level
/// no site updated. The producers live in `app_fetch` (the send, inside an
/// `offload_job` closure) and `app_render` (the receipt, inside the frame
/// thread's poll), and driving either needs a whole `App` with its channel
/// hub; a source scan is what is affordable here.
///
/// It is a door guard and not a proof: it says the calls are present, not
/// that they are on every path or in the right order. `include_str!` reads
/// the sources at compile time, so it cannot go stale against the binary.
#[test]
fn the_loop_seam_is_called_at_both_ends() {
    let send = include_str!("../app_fetch.rs");
    assert!(
        send.contains("loop_reply.price("),
        "`spawn_loop_frame_render` no longer prices its raster before the send, so every \
         loop frame in flight — the largest population on a looping scene — is missing \
         from `renders in flight`"
    );
    let receipt = include_str!("../app_render.rs");
    assert!(
        receipt.contains("settle_loop_reply("),
        "`poll_loop_render_results` no longer settles the raster it took, so the level \
         climbs by every loop frame the session ever rendered and never falls"
    );
}
