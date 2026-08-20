//! The window between dispatching an overlay render and switching that layer
//! off.
//!
//! `Gui::write_pane_overlay` releases a disabled layer's texture the moment the
//! decision is made, but a render already on a worker thread cannot be
//! recalled. It arrives at `poll_overlay_render_results` some frames later,
//! carrying a freshly uploaded full-size texture, for a layer the pane no
//! longer draws — and the poller used to store every result it received. The
//! release would therefore undo itself, and permanently for a pane the viewport
//! loop in `ui_map_pane` never repaints: hidden past the split's visible count,
//! or converted to a cross-section.
//!
//! Both directions are covered here off **one fixture and one delivery**, which
//! is what makes the pair mean anything. Dropping too little is the leak;
//! dropping too much is a live layer whose picture never lands, and a guard
//! written slightly too wide would show exactly that and nothing else.
//!
//! The in-flight mark is the third assertion and the one with the worst failure
//! mode. `ui_map_pane` dispatches on `stale && !cache.render_in_flight`, so a
//! mark left standing over a result that was dropped is a layer that can never
//! be re-rendered again — a re-enable that stays blank for the rest of the
//! session, which is worse than the residency this change is about.
//!
//! That assertion carries more weight than it looks, because this is the *only*
//! place an `offload`ed render's mark is cleared.
//! `PaneState::release_disabled_overlay_textures` deliberately leaves it alone
//! — clearing it there opens the dispatch gate and buys a second render of the
//! same content on a fast off/on, see that function — so the drop path here is
//! what has to undo it, and `the_release_leaves_a_render_already_in_flight_marked`
//! in `rustdar_egui`'s sibling module is the other half of that division.

use super::*;
use rustdar_egui::overlay_cache::OverlayTextureData;
use rustdar_geo::GeoBounds;
use rustdar_source::id::{LayerId, known};

/// The layer raced. A texture-mode overlay that `poll_overlay_render_results`
/// really serves — the radar raster never reaches this poller at all
/// (`ui_map_pane`'s viewport loop skips `Radar`; the picture comes from
/// `apply_render_to_pane`).
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
///
/// The parked texture is the test's instrument, not a claim about production:
/// after a real toggle the cache is empty, and an empty cache cannot tell
/// "the poller dropped the result" from "the poller stored it into a cache
/// nothing looked at". Something already in the slot makes both outcomes
/// visible in the same field — the id either changed or it did not.
///
/// Returns the app and the id of the parked texture.
fn app_awaiting_a_render(ctx: &egui::Context) -> (crate::app::App, egui::TextureId) {
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let parked = image(ctx, "parked");
    let id = parked.id();
    let pane = app.gui.pane_mut(0).expect("the fixture has one pane");
    pane.set_overlay_enabled(KIND, true);
    let cache = pane.overlay_cache_mut(&KIND);
    cache.show(OverlayTextureData {
        texture: parked,
        placed: rustdar_geo::PlacedRaster::of(bounds()),
        data_generation: 1,
        render_zoom: 32,
        width: W,
        height: H,
        radar_meta: None,
        hit_map: None,
    });
    // The state a dispatched render leaves behind, set exactly as
    // `ui_map_pane` sets it.
    cache.render_in_flight = true;
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
        .render_in_flight
}

/// **The race.** A render dispatched before the layer was switched off lands
/// after it, and is thrown away rather than parked.
///
/// The layer is switched off by writing the pane's map directly rather than
/// through `Gui::write_pane_overlay`, and that is the point: the toggle path
/// would clear the cache, and what is under test here is the *poller's* own
/// guard, in the one state that isolates it.
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
///
/// Without this, a guard that dropped *everything* would pass the test above
/// and take every overlay on the map with it.
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

    // Kept, not dropped — but *held*, because the pane already has a picture
    // and the new one's pixels are not on the GPU yet. The parked texture
    // staying on screen here is the hold doing its job, not the guard
    // over-filtering; the promotion below is what tells the two apart.
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

/// **The un-wedge.** A described render (`offload::JobRequest::Overlay`) can
/// fail — a worker lost mid-job, a lapsed wait for one, a reply the dispatch's
/// length check refused — and the failure arrives as a response carrying no
/// image at all. It must clear the in-flight mark for every named pane exactly
/// as a kept result does, and place nothing: `ui_map_pane` dispatches on
/// `stale && !render_in_flight`, so a failure that skipped the clear would
/// leave the layer un-dispatchable for the life of the session — wedged blank,
/// with no error anywhere.
///
/// Off the same fixture as its two siblings, and the parked texture is the
/// same instrument: whether the pane's picture survived is visible in the same
/// field as whether something replaced it.
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
