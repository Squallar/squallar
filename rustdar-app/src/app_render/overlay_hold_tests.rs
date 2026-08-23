use super::*;
use rustdar_geo::GeoBounds;
use rustdar_source::id::{LayerId, known};

const KIND: LayerId = known::NWS_ALERTS;
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

fn raster(seed: u8) -> Arc<egui::ColorImage> {
    let rgba: Vec<u8> = (0..(W * H) as u8)
        .flat_map(|i| [seed, i.wrapping_mul(29), seed ^ i, 255])
        .collect();
    Arc::new(egui::ColorImage::from_rgba_unmultiplied([W, H], &rgba))
}

fn deliver(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    generation: u64,
    seed: u8,
    pane_indices: Vec<usize>,
) {
    // **The mark a real dispatch leaves**, and these fixtures have to leave it
    // too. `poll_overlay_render_results` accepts a raster only while the cache
    // is still waiting for that very dispatch — the stale-result policy on
    // `RendersInFlight::retire` — so a reply posted against no mark at all
    // exercises that path instead of the one under test here. Every reply below
    // is the answer to a dispatch, so every one gets its ticket.
    for &idx in &pane_indices {
        if let Some(pane) = app.gui.pane_mut(idx) {
            pane.overlay_cache_mut(&KIND).renders.record(
                rustdar_egui::overlay_cache::RenderTicket::whole(generation, bounds()),
            );
        }
    }
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
            // The pane's live raster, not a loop frame's.
            frame: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
}

fn on_screen(app: &crate::app::App, idx: usize) -> Option<egui::TextureId> {
    Some(
        app.gui
            .pane(idx)?
            .overlay_cache(&KIND)?
            .current()?
            .texture
            .id(),
    )
}

fn generation_on_screen(app: &crate::app::App, idx: usize) -> u64 {
    app.gui
        .pane(idx)
        .expect("pane exists")
        .overlay_cache(&KIND)
        .expect("cache exists")
        .current()
        .expect("a picture is on screen")
        .data_generation
}

fn holding(app: &crate::app::App, idx: usize) -> bool {
    app.gui
        .pane(idx)
        .expect("pane exists")
        .overlay_cache(&KIND)
        .is_some_and(rustdar_egui::overlay_cache::OverlayTextureCache::is_holding)
}

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
