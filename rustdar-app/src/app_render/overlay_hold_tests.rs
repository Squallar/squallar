//! A pane keeps the *overlay* picture it has until the next one is whole.
//!
//! `raster_hold_tests` is the radar half of this rule; these are the layer
//! textures — alerts, outlooks, county lines — whose results arrive through
//! `poll_overlay_render_results`. That poller used to `show` unconditionally,
//! on the claim that overlay rasters cross the GPU inside their own frame; on
//! any phone-class viewport they do not (8.9–22.8 MB against the 8 MiB upload
//! band), and the swap put an id still bound to a transparent 1×1 stand-in on
//! screen for the frames the bands took. Alert overlays vanished mid-zoom and
//! popped back at settle. The rule these pin is the binding one: held data is
//! always drawn — stretched, stale, or soft — until its replacement is whole.
//!
//! As in `raster_hold_tests`, the rasters here are small because nothing here
//! is timing anything: how many frames a raster takes to cross is
//! `texture_upload`'s question. What is counted is textures, swaps, and what
//! is on screen between them.

use super::*;
use rustdar_geo::GeoBounds;
use rustdar_overlays::render::overlay_state::OverlayKind;

const KIND: OverlayKind = OverlayKind::NwsAlerts;
const W: usize = 8;
const H: usize = 5;

fn n_pane_app(n: usize) -> crate::app::App {
    crate::app::tests::n_pane_app(n, "KTLX")
}

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

/// Pixels whose bytes depend on `seed`, so two results are never one buffer.
fn raster(seed: u8) -> Arc<egui::ColorImage> {
    let rgba: Vec<u8> = (0..(W * H) as u8)
        .flat_map(|i| [seed, i.wrapping_mul(29), seed ^ i, 255])
        .collect();
    Arc::new(egui::ColorImage::from_rgba_unmultiplied([W, H], &rgba))
}

/// Post one finished overlay render for `pane_indices` and drain the poller —
/// the same leg `overlay_upload_tests::deliver` drives, with the generation a
/// parameter because these tests land several results on one pane.
fn deliver(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    generation: u64,
    seed: u8,
    pane_indices: Vec<usize>,
) {
    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            image: Some(raster(seed)),
            geo_bounds: bounds(),
            overlay_kind: KIND,
            generation,
            pane_indices,
            zoom: 32,
            hit_map: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
}

/// The id of the overlay texture pane `idx` is drawing, if it is drawing one.
fn on_screen(app: &crate::app::App, idx: usize) -> Option<egui::TextureId> {
    Some(
        app.gui
            .pane(idx)?
            .overlay_cache(KIND)?
            .current()?
            .texture
            .id(),
    )
}

/// The generation of the overlay picture pane `idx` is drawing.
fn generation_on_screen(app: &crate::app::App, idx: usize) -> u64 {
    app.gui
        .pane(idx)
        .expect("pane exists")
        .overlay_cache(KIND)
        .expect("cache exists")
        .current()
        .expect("a picture is on screen")
        .data_generation
}

/// Whether pane `idx`'s alert cache is waiting on pixels.
fn holding(app: &crate::app::App, idx: usize) -> bool {
    app.gui
        .pane(idx)
        .expect("pane exists")
        .overlay_cache(KIND)
        .is_some_and(rustdar_egui::overlay_cache::OverlayTextureCache::is_holding)
}

/// A pane's first overlay raster goes up as it arrives; every one after it
/// waits, with the first still on screen, until delivery promotes it.
///
/// The whole rule in the order a session meets it, and the middle assertion is
/// the defect: before the hold, the second result replaced the first
/// *immediately*, and what the user saw for the next several frames was the
/// replacement's transparent stand-in — the alert layer gone mid-zoom.
#[test]
fn the_first_overlay_arrives_and_the_second_waits() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);

    deliver(&mut app, &ctx, 1, 1, vec![0]);
    let first = on_screen(&app, 0).expect("a pane with no picture shows the one arriving");
    assert!(
        !holding(&app, 0),
        "the first raster has nothing to hold for"
    );

    deliver(&mut app, &ctx, 2, 2, vec![0]);
    assert_eq!(
        on_screen(&app, 0),
        Some(first),
        "the pane swapped onto an overlay whose pixels had not all arrived: \
         this is the layer vanishing mid-zoom",
    );
    assert!(holding(&app, 0));

    app.deliver_held_rasters();
    assert_ne!(
        on_screen(&app, 0),
        Some(first),
        "the pane never swapped onto the overlay it was holding",
    );
    assert_eq!(generation_on_screen(&app, 0), 2);
    assert!(!holding(&app, 0));
}

/// A held overlay keeps the event loop awake, exactly as a held radar raster
/// does — `any_raster_held` is the frame loop's re-arm term, and a hold it
/// cannot see is a swap waiting for unrelated input.
#[test]
fn a_held_overlay_keeps_the_frame_loop_awake() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);

    deliver(&mut app, &ctx, 1, 1, vec![0]);
    assert!(
        !app.gui.any_raster_held(),
        "fixture: nothing is held after a first raster",
    );

    deliver(&mut app, &ctx, 2, 2, vec![0]);
    assert!(
        app.gui.any_raster_held(),
        "a pane is holding an overlay and the frame loop's re-arm term cannot \
         see it: the swap waits for unrelated input",
    );

    app.deliver_held_rasters();
    assert!(!app.gui.any_raster_held());
}

/// A newer overlay result supersedes one still arriving; the picture on screen
/// is untouched throughout, and the swap lands on the newest.
#[test]
fn a_newer_overlay_result_supersedes_the_one_still_arriving() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);

    deliver(&mut app, &ctx, 1, 1, vec![0]);
    let first = on_screen(&app, 0).expect("placed");
    deliver(&mut app, &ctx, 2, 2, vec![0]);
    deliver(&mut app, &ctx, 3, 3, vec![0]);
    assert_eq!(
        on_screen(&app, 0),
        Some(first),
        "a mid-gesture burst of results touched the picture on screen",
    );

    app.deliver_held_rasters();
    assert_eq!(
        generation_on_screen(&app, 0),
        3,
        "the swap landed on a superseded result rather than the newest",
    );
}

/// The overlay swap does not restamp the pane's `data_time` — that caption
/// dates the *radar* picture, and only `promote_held_raster` may write it.
///
/// An alert overlay landing must not redate the sweep on screen: on a site
/// that stopped scanning yesterday, the two differ by most of a day and the
/// pane would caption old radar as fresh.
#[test]
fn an_overlay_swap_does_not_restamp_the_panes_data_time() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);
    let sweep_time = chrono::NaiveDate::from_ymd_opt(2026, 8, 13)
        .unwrap()
        .and_hms_opt(1, 48, 0)
        .unwrap();
    app.gui.pane_mut(0).expect("pane exists").data_time = Some(sweep_time);

    deliver(&mut app, &ctx, 1, 1, vec![0]);
    deliver(&mut app, &ctx, 2, 2, vec![0]);
    app.deliver_held_rasters();

    assert_eq!(
        app.gui.pane(0).expect("pane exists").data_time,
        Some(sweep_time),
        "promoting an overlay rewrote the radar caption: the pane now dates \
         its sweep by when an alert raster landed",
    );
}

/// A renderer rebuild releases held overlays along with held radar rasters.
///
/// Same argument as `raster_hold_tests::a_renderer_rebuild_releases_every_hold`:
/// the held ids belong to the dead context, `is_delivered` answers `false`
/// about them forever, and any one left standing keeps `any_raster_held` true
/// — the event loop at refresh rate — for the rest of the session.
#[test]
fn a_renderer_rebuild_releases_held_overlays() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1);

    deliver(&mut app, &ctx, 1, 1, vec![0]);
    deliver(&mut app, &ctx, 2, 2, vec![0]);
    assert!(
        app.gui.any_raster_held(),
        "precondition: a hold is standing"
    );

    app.restore_cached_render(&egui::Context::default());
    assert!(
        !app.gui.any_raster_held(),
        "a held overlay survived the renderer that was going to deliver it, \
         so nothing will ever end it and the loop never parks again",
    );
}

/// Panes sharing one overlay result hold clones of one handle, and one
/// delivery answer swaps them all — the poller's one-upload rule, carried
/// through the hold.
#[test]
fn panes_sharing_an_overlay_result_swap_on_one_answer() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(4);

    deliver(&mut app, &ctx, 1, 1, vec![0, 1, 2, 3]);
    let first = on_screen(&app, 0).expect("placed");
    assert!((1..4).all(|idx| on_screen(&app, idx) == Some(first)));

    deliver(&mut app, &ctx, 2, 2, vec![0, 1, 2, 3]);
    assert!(
        (0..4).all(|idx| on_screen(&app, idx) == Some(first)),
        "a pane swapped onto pixels that had not arrived",
    );
    assert!((0..4).all(|idx| holding(&app, idx)));

    app.deliver_held_rasters();
    assert!(
        (0..4).all(|idx| !holding(&app, idx) && generation_on_screen(&app, idx) == 2),
        "one delivery did not swap every pane served from that result",
    );
}
