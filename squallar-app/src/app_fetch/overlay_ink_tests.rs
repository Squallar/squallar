//! **A picture that paints nothing is separable from one that paints.**
//!
//! The hole, measured 2026-08-31 and found while deliberately emptying a
//! raster: nothing anywhere discarded or marked a blank picture.
//! `ledger::note_picture` took the RGBA buffer's length and counted it whatever
//! was in it, so a texture layer emitting a fully transparent pixmap satisfied
//! every conjunct `--expect-overlay-rasters` had — `dispatched > 0`,
//! `arrived > 0`, `pictures > 0`, `picture_bytes > 0`, `shown + promoted > 0`,
//! and the arrival balance. The rig read 6 dispatched → 6 arrived → 6 pictures
//! over a layer drawing nothing, and anyone reasoning "the rig would catch it
//! if the layer stopped drawing" was wrong.
//!
//! **The reading is taken here, in the offload closure, and never at the
//! arrival.** `poll_overlay_render_results` runs inside `setup_egui_frame`;
//! the question costs a pass over a buffer that is 17.8 MiB on a 1080p pane.
//! That is the same rule `no_poller_unmultiplies_on_the_frame_thread` states
//! for the premultiply, and `OverlayRenderResponse::ink` is what carries the
//! answer across.
//!
//! The two tests below are the two sides that make the counter non-vacuous, and
//! each fails on its own: a `has_ink` stuck at `false` red-gates every honest
//! run and is caught by the first; one stuck at `true` passes the exact case
//! the conjunct was added for and is caught by the second. Neither can be
//! satisfied by the fixture stating its own answer — both drive the real
//! `App::overlay_job_deliver` over real rasterizer bytes and read `ink` off
//! what it built.

use squallar_geo::GeoBounds;
use squallar_source::id::known;

const W: u32 = 64;
const H: u32 = 48;

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

/// Run the real deliver over `rgba` and hand back the response it built.
///
/// `ink` goes in `false` deliberately: a fixture that seeded the answer it then
/// asserts would pass with the predicate deleted. Every `true` below is one the
/// production closure wrote.
fn delivered(app: &crate::app::App, rgba: Vec<u8>) -> crate::channels::OverlayRenderResponse {
    let response = crate::channels::OverlayRenderResponse {
        image: None,
        ink: false,
        geo_bounds: bounds(),
        overlay_kind: known::NWS_ALERTS,
        generation: 5,
        pane_indices: vec![0],
        zoom: 32,
        hit_map: None,
        frame: None,
    };
    crate::app::App::overlay_job_deliver(
        "test-ink",
        W,
        H,
        None,
        response,
        app.channels.overlay_render_sender.clone(),
        None,
    )(Some(squallar_source::job::DescribedOut(Box::new(
        squallar_overlays::render::rasterize::RasterizeOutput {
            rgba,
            hit_cells: None,
            alpha: squallar_overlays::render::rasterize::AlphaMode::Premultiplied,
        },
    ))));
    app.channels
        .overlay_render_receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("every deliver answers, failure included")
}

/// A buffer of the dispatch's own size, entirely transparent.
fn blank() -> Vec<u8> {
    vec![0u8; (W * H * 4) as usize]
}

/// **The floor: a picture that paints is marked as one.**
///
/// Without this the conjunct could be satisfied by a `has_ink` that always
/// answers `false`, which reads red on every leg and would be diagnosed as a
/// broken overlay pipeline rather than as a broken gate.
///
/// The ink is in the **last** pixel, which is where the cheap wrong answers —
/// peek at the head, sample a stride — get it wrong. An alerts raster whose
/// only content is one polygon has exactly this shape.
#[test]
fn a_picture_with_one_pixel_of_ink_is_marked_as_painting() {
    let app = crate::app::tests::n_pane_app(1, "KTLX");

    let mut rgba = blank();
    *rgba.last_mut().expect("the buffer is not empty") = 1;
    let painted = delivered(&app, rgba);

    assert!(
        painted.image.is_some(),
        "the deliver refused a well-shaped buffer, so this is not exercising \
         the picture path at all",
    );
    assert!(
        painted.ink,
        "a picture with a non-transparent pixel was marked as painting \
         nothing. `--expect-overlay-rasters` now reads `inked == 0` and goes \
         red on a working overlay pipeline",
    );
}

/// **The acceptance: a fully transparent picture is marked as painting
/// nothing, and every other figure on the path is unchanged by that.**
///
/// The second half is what makes this the closure of the hole rather than a
/// restatement of it. The blank picture is delivered, it is a picture, it has
/// the same byte count as the painted one — so the five conjuncts that existed
/// before cannot separate the two readings, and `ink` is the only thing on the
/// response that does.
#[test]
fn a_fully_transparent_picture_is_marked_as_painting_nothing() {
    let app = crate::app::tests::n_pane_app(1, "KTLX");

    let empty = delivered(&app, blank());
    let mut inked_rgba = blank();
    *inked_rgba.last_mut().expect("the buffer is not empty") = 1;
    let painted = delivered(&app, inked_rgba);

    let empty_image = empty.image.as_ref().expect(
        "a blank raster is still a delivered picture — if the deliver started \
         refusing it, the ledger's drop counter sees it and this test is \
         asserting a path that no longer exists",
    );
    let painted_image = painted.image.as_ref().expect("the painted picture");

    // Everything the old five conjuncts could see, over both readings.
    assert_eq!(
        empty_image.as_raw().len(),
        painted_image.as_raw().len(),
        "the two pictures differ in size, so `picture_bytes` would separate \
         them by itself and this test is not about what it says it is about",
    );
    assert_eq!(
        empty_image.as_raw().len(),
        (W * H * 4) as usize,
        "the delivered buffer is not the dispatch's own size",
    );
    assert_eq!(
        empty.geo_bounds, painted.geo_bounds,
        "the fixtures differ somewhere other than their pixels",
    );

    assert!(
        !empty.ink,
        "a picture of {} fully transparent bytes was marked as painting. That \
         is the measured hole verbatim: six dispatched, six arrived, six \
         pictures, and a map drawing nothing",
        empty_image.as_raw().len(),
    );
    assert_ne!(
        empty.ink, painted.ink,
        "the blank picture and the painted one agree on every field of the \
         response, so nothing downstream can tell a layer that stopped drawing \
         from one that never did",
    );
}
