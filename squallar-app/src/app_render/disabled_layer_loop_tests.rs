//! **A loop on a layer the pane does not draw supplies nothing and holds
//! nothing — and comes back whole when the layer does.**
//!
//! The still-picture half of this landed on 2026-09-07: a pane with its radar
//! layer switched off stopped fetching, stopped rendering and gave its texture
//! back. Neither gate reached the *loop's* half of the same layers. A loop
//! survives its layer being switched off — `PaneState::refresh_transport`
//! keeps a running timeline's transport on purpose — so
//! `dispatch_overlay_loop_renders` went on describing a full-size raster per
//! frame and re-asking for the granules behind them, and went on holding one
//! texture per frame, for pictures no pane could paint, for the life of the
//! session.
//!
//! # Three claims, and the arrangement each one needs
//!
//! All of them drive the production pass
//! (`App::dispatch_overlay_loop_renders`) against the
//! [`super::loop_overlay_render_tests`] double, so what they read is what the
//! layer was actually asked for.
//!
//! **They cannot share one arrangement, and the reason is the point.** A frame
//! that is holding a picture is not described again (`frame.image.is_some()`
//! is the dispatch's own skip), so a loop whose frames are all textured asks
//! for nothing on a fixed tree *and on a broken one*. The stopping claim is
//! therefore made on a loop whose frames are owed their pictures — the state a
//! listing that lands after the switch-off leaves, and the state every frame
//! outside the byte share is in while a loop plays — and the freeing claim on
//! a loop that had them. Written as one test, the stopping half would have
//! been vacuous.
//!
//! # The trap held shut here
//!
//! Stopping a supply is easy to do in a way that cannot be undone. The third
//! test is the whole reason the skip evicts *textures* and touches nothing
//! else: no queue is retired, no list is re-sampled, no listing is dropped, so
//! the frame list a re-enabled layer comes back to is the list it left with —
//! same stamps, same order, same count — and every frame is refilled by the
//! same walk that emptied it.

use super::loop_overlay_render_tests::{
    Asked, app_with_frames, build_loop, deliver_raster, frame_stamps, frame_textures, run,
};
use squallar_source::id::known;
use squallar_source::time::FrameStamp;
use std::sync::{Arc, Mutex};

/// The three frames every loop in this module holds.
fn ts(hour: i64) -> chrono::NaiveDateTime {
    run() + chrono::Duration::hours(hour)
}

fn stamp(hour: i64) -> FrameStamp {
    FrameStamp {
        valid: ts(hour),
        run: Some(run()),
    }
}

/// A sink that takes every job — the funnel must not rasterize here.
struct TakeAll;

impl squallar_worker::offload::JobSink for TakeAll {
    fn send(
        &self,
        _id: u64,
        _request: squallar_worker::offload::JobRequest,
    ) -> Result<(), squallar_worker::offload::JobRequest> {
        Ok(())
    }
}

/// The pane's live dispatch, which is what writes the geometry record the loop
/// dispatch reuses — without one, `spawn_overlay_render` has nothing to size a
/// raster with and the loop asks for nothing whatever the enabled flag says.
fn a_render_request() -> crate::app::fetch::OverlayRenderRequest {
    crate::app::fetch::OverlayRenderRequest {
        geo_bounds: squallar_geo::GeoBounds {
            min_lat: 34.0,
            max_lat: 36.0,
            min_lon: -99.0,
            max_lon: -97.0,
        },
        texture: squallar_egui::overlay_cache::OverlayTexturePlan {
            width: 8,
            height: 5,
            overdraw: 0.0,
            pixels_per_point: 1.0,
            pane_px: [0, 0],
        },
        data_generation: 5,
        zoom: 32,
    }
}

/// A pane looping three frames, none of them drawn yet, with the geometry
/// record the loop dispatch needs already written.
///
/// The record is written by the pane's own live dispatch, and without one
/// `spawn_overlay_render` has nothing to size a raster with — so a loop on
/// such a pane asks for nothing whatever its enabled flag says, and every
/// claim below would read green on any tree.
fn app_with_a_listed_loop() -> (crate::app::App, Arc<Mutex<Asked>>) {
    let (mut app, asked) = app_with_frames(vec![ts(0), ts(1), ts(2)]);
    build_loop(&mut app, (ts(0), ts(2)));
    assert_eq!(
        frame_stamps(&app),
        vec![ts(0), ts(1), ts(2)],
        "premise: the listing became the layer's frame list",
    );
    app.spawn_overlay_render(vec![0], known::MODEL_DATA, a_render_request(), None);
    (app, asked)
}

/// [`app_with_a_listed_loop`], played until every frame holds its own picture:
/// three frames, three rasters, three textures.
fn app_with_a_drawn_loop(ctx: &egui::Context) -> (crate::app::App, Arc<Mutex<Asked>>) {
    let (mut app, asked) = app_with_a_listed_loop();
    clear(&asked);
    app.dispatch_overlay_loop_renders();
    assert_eq!(
        asks_since_clear(&asked).0,
        3,
        "premise: a drawn loop describes a raster per frame. Without this the \
         claims below are about a loop that never asked for anything.",
    );
    for (hour, shade) in [(0i64, 10u8), (1, 110), (2, 210)] {
        deliver_raster(&mut app, ctx, stamp(hour), shade);
    }
    assert!(
        frame_textures(&app).iter().all(Option::is_some),
        "premise: every frame of the running loop is holding a picture, or \
         there is nothing for the switch-off to give back",
    );
    (app, asked)
}

fn set_drawn(app: &mut crate::app::App, drawn: bool) {
    app.gui
        .pane_mut(0)
        .expect("the fixture built a pane")
        .set_overlay_enabled(known::MODEL_DATA, drawn);
}

/// Everything the double was asked for since it was last cleared, as
/// `(frame rasters described, granules fetched)`. The live raster — a
/// `prepare_job` naming no frame — is not the loop's supply and is not
/// counted.
fn asks_since_clear(asked: &Arc<Mutex<Asked>>) -> (usize, usize) {
    let record = asked.lock().expect("no poisoned lock");
    (
        record.prepared.iter().filter(|f| f.is_some()).count(),
        record.fetched.len(),
    )
}

fn clear(asked: &Arc<Mutex<Asked>>) {
    let mut record = asked.lock().expect("no poisoned lock");
    record.prepared.clear();
    record.fetched.clear();
}

/// **A loop on a layer the pane does not draw is asked for nothing.**
///
/// Six passes and not one: a supply that fires once per *change* and one that
/// fires once per *frame* are different defects, and a single pass cannot tell
/// them apart.
///
/// **Floor — delete the `is_overlay_enabled` arm** from the supply walk in
/// `dispatch_overlay_loop_renders`: the count is 3, one full-size raster
/// described per frame for a layer nobody can see.
#[test]
fn a_loop_on_a_layer_the_pane_does_not_draw_is_asked_for_nothing() {
    let _guard = squallar_worker::offload::install_test_worker(Box::new(TakeAll));
    let (mut app, asked) = app_with_a_listed_loop();

    set_drawn(&mut app, false);
    clear(&asked);
    for _ in 0..6 {
        app.dispatch_overlay_loop_renders();
    }

    assert_eq!(
        asks_since_clear(&asked),
        (0, 0),
        "the loop spent on a layer the pane does not draw: rasters described \
         and granules fetched for frames nobody can paint. Each one is a \
         full-size picture on the funnel and a granule on the wire.",
    );
}

/// **And a layer that stops being drawn is left holding nothing.**
///
/// The other half, and the one that frees rather than stops: a loop whose
/// frames were all textured before the switch-off. It is a separate test
/// because on this arrangement the ask counts are zero on any tree — a frame
/// holding a picture is not described again — so the claim above cannot be
/// made here and this one cannot be made there.
///
/// **Floor — replace the arm's `evict_textures_outside_render_set(0)` with a
/// bare `continue`:** the frames keep all three textures.
#[test]
fn a_loop_whose_layer_stops_being_drawn_gives_its_frame_textures_back() {
    let ctx = egui::Context::default();
    let _guard = squallar_worker::offload::install_test_worker(Box::new(TakeAll));
    let (mut app, _asked) = app_with_a_drawn_loop(&ctx);

    set_drawn(&mut app, false);
    for _ in 0..6 {
        app.dispatch_overlay_loop_renders();
    }

    assert_eq!(
        frame_textures(&app),
        vec![None, None, None],
        "the frames kept their textures after the layer went off. Stopping \
         the growth is only half of it — what the loop already holds is one \
         texture per frame, held for the life of the session.",
    );
    assert_eq!(
        frame_stamps(&app),
        vec![ts(0), ts(1), ts(2)],
        "the frame list itself was torn down. Only the pictures may go: the \
         stamps are what the way back is refilled from.",
    );
}

/// **And it comes back whole.**
///
/// The trap a "stop supplying when it is off" change is written to fall into.
/// The list is the same list, every frame is asked for again, and every frame
/// ends up holding its own distinct picture — not one shared handle, which is
/// what a presence check would read as green.
#[test]
fn a_layer_switched_back_on_gets_every_frame_of_its_loop_back() {
    let ctx = egui::Context::default();
    let _guard = squallar_worker::offload::install_test_worker(Box::new(TakeAll));
    let (mut app, asked) = app_with_a_drawn_loop(&ctx);

    set_drawn(&mut app, false);
    app.dispatch_overlay_loop_renders();
    set_drawn(&mut app, true);
    clear(&asked);
    app.dispatch_overlay_loop_renders();

    let mut described: Vec<FrameStamp> = asked
        .lock()
        .expect("no poisoned lock")
        .prepared
        .iter()
        .filter_map(|f| *f)
        .collect();
    described.sort_by_key(|f| f.valid);
    assert_eq!(
        described,
        vec![stamp(0), stamp(1), stamp(2)],
        "the layer came back on and its loop was not refilled. A frame the \
         skip emptied and the way back does not ask for again is blank for \
         the life of the loop.",
    );

    for (hour, shade) in [(0i64, 30u8), (1, 130), (2, 230)] {
        deliver_raster(&mut app, &ctx, stamp(hour), shade);
    }
    assert_eq!(
        frame_stamps(&app),
        vec![ts(0), ts(1), ts(2)],
        "the loop that came back is not the loop that left",
    );
    let textures = frame_textures(&app);
    let ids: Vec<egui::TextureId> = textures.iter().filter_map(|t| *t).collect();
    assert_eq!(
        ids.len(),
        3,
        "a frame came back without a picture: {textures:?}",
    );
    let distinct: std::collections::HashSet<egui::TextureId> = ids.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        3,
        "two frames came back holding the SAME texture, which a presence \
         check reads as a complete loop and a viewer reads as an animation \
         that does not move: {ids:?}",
    );
}
