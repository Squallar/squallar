//! The window between dispatching an overlay render and switching that layer
//! off.

use super::*;
use squallar_egui::overlay_cache::OverlayTextureData;
use squallar_geo::GeoBounds;
use squallar_source::id::{LayerId, known};

/// The layer raced.
const KIND: LayerId = known::NWS_ALERTS;

const W: u32 = 8;
const H: u32 = 5;

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

fn image(ctx: &egui::Context, name: &str) -> egui::TextureHandle {
    ctx.load_texture(
        name,
        egui::ColorImage::from_rgba_unmultiplied(
            [W as usize, H as usize],
            &vec![128u8; (W * H) as usize * 4],
        ),
        egui::TextureOptions::NEAREST,
    )
}

/// An app whose single pane is showing an overlay texture and waiting on a
/// second one.
fn app_awaiting_a_render(ctx: &egui::Context) -> (crate::app::App, egui::TextureId) {
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let parked = image(ctx, "parked");
    let id = parked.id();
    let pane = app.gui.pane_mut(0).expect("the fixture has one pane");
    pane.set_overlay_enabled(KIND, true);
    let cache = pane.overlay_cache_mut(&KIND);
    cache.show(OverlayTextureData {
        texture: parked,
        placed: squallar_geo::PlacedRaster::of(bounds()),
        data_generation: 1,
        render_zoom: 32,
        width: W,
        height: H,
        radar_meta: None,
        hit_map: None,
    });
    // **The ticket the `deliver` below answers**, not an anonymous mark: the
    // arrival accepts a raster only while the cache is still waiting for that
    // very dispatch, so a fixture whose mark named something else would have
    // the result dropped for a reason neither test is about.
    cache
        .renders
        .record(squallar_egui::overlay_cache::RenderTicket::whole(
            9,
            bounds(),
        ));
    (app, id)
}

/// Post the finished render and drain the poller — the same call in both tests,
/// so the only difference between them is the pane's enabled map.
fn deliver(app: &mut crate::app::App, ctx: &egui::Context) {
    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            image: Some(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
                [W as usize, H as usize],
                &vec![255u8; (W * H) as usize * 4],
            ))),
            geo_bounds: bounds(),
            overlay_kind: KIND,
            generation: 9,
            pane_indices: vec![0],
            zoom: 32,
            hit_map: None,
            // The pane's live raster, not a loop frame's.
            frame: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
}

fn current_id(app: &mut crate::app::App) -> Option<egui::TextureId> {
    Some(
        app.gui
            .pane_mut(0)
            .expect("pane 0")
            .overlay_cache_mut(&KIND)
            .current()?
            .texture
            .id(),
    )
}

fn in_flight(app: &mut crate::app::App) -> bool {
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .overlay_cache_mut(&KIND)
        .renders
        .holds(squallar_egui::overlay_cache::RenderSlot::WHOLE)
}

/// **The race.** A render dispatched before the layer was switched off lands
/// after it, and is thrown away rather than parked.
#[test]
fn a_late_result_for_a_disabled_layer_is_dropped() {
    let ctx = egui::Context::default();
    let (mut app, parked) = app_awaiting_a_render(&ctx);
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .set_overlay_enabled(KIND, false);

    assert_eq!(
        current_id(&mut app),
        Some(parked),
        "premise: the pane must really be holding a texture here, or the \
         assertion below is satisfied by a cache that was never populated",
    );
    assert!(
        in_flight(&mut app),
        "premise: the render must really be marked in flight, or the mark \
         being clear afterwards says nothing",
    );

    deliver(&mut app, &ctx);

    assert_eq!(
        current_id(&mut app),
        Some(parked),
        "the poller parked a full-size texture for a layer this pane no longer \
         draws, undoing the release the toggle had just performed — and on a \
         hidden or non-map pane nothing would ever clear it again",
    );
    assert!(
        !in_flight(&mut app),
        "the dropped result left its in-flight mark standing: `ui_map_pane` \
         dispatches on `stale && !render_in_flight`, so this layer could never \
         be rendered again for the life of the process",
    );
}

/// The over-filtering control, off the same fixture and the same delivery: a
/// layer that is still on gets its picture.
#[test]
fn a_result_for_an_enabled_layer_still_lands() {
    let ctx = egui::Context::default();
    let (mut app, parked) = app_awaiting_a_render(&ctx);

    assert!(
        app.gui.pane(0).expect("pane 0").is_overlay_enabled(&KIND),
        "premise: this arm differs from its sibling in exactly one field, and \
         this is the field",
    );

    deliver(&mut app, &ctx);

    assert_eq!(
        current_id(&mut app),
        Some(parked),
        "the poller swapped a picture onto the screen before its pixels had \
         all arrived",
    );
    assert!(
        app.gui.pane(0).expect("pane 0").is_holding_raster(),
        "the poller dropped a result for a layer that is switched on — the \
         disable guard is filtering more than the disabled",
    );
    app.deliver_held_rasters();
    let landed = current_id(&mut app).expect("the promoted overlay is on this pane");
    assert_ne!(
        landed, parked,
        "the held result never made it to the screen once its pixels landed",
    );
    assert!(
        !in_flight(&mut app),
        "a stored result must clear its own in-flight mark, exactly as it did \
         before the guard existed",
    );
}

/// **The un-wedge.**
#[test]
fn a_failed_render_clears_the_in_flight_mark_and_touches_nothing() {
    let ctx = egui::Context::default();
    let (mut app, parked) = app_awaiting_a_render(&ctx);
    assert!(
        in_flight(&mut app),
        "premise: the render must really be marked in flight, or the mark \
         being clear afterwards says nothing",
    );

    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            image: None,
            geo_bounds: bounds(),
            overlay_kind: KIND,
            generation: 9,
            pane_indices: vec![0],
            zoom: 32,
            hit_map: None,
            // The pane's live raster, not a loop frame's.
            frame: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(&ctx);

    assert!(
        !in_flight(&mut app),
        "a failed render left its in-flight mark standing: the layer can \
         never be dispatched again — the exact wedge the empty response \
         exists to prevent",
    );
    assert_eq!(
        current_id(&mut app),
        Some(parked),
        "a failed render must leave the picture on screen alone",
    );
    assert!(
        !app.gui.pane(0).expect("pane 0").is_holding_raster(),
        "a failed render staged a hold, so an empty upload would swap in over \
         the picture the pane is rightly keeping",
    );
}
