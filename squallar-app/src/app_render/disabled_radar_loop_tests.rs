//! **A radar loop on a pane that needs no radar data buys nothing, renders
//! nothing and holds nothing — and comes back whole when the pane needs it
//! again.**
//!
//! The still picture's half of this landed on 2026-09-07 and the overlay
//! loops' half on 2026-09-08 (`super::disabled_layer_loop_tests`). Radar's own
//! loop was the piece neither reached, and it is the expensive one: a volume
//! per frame on the wire, a full-size raster per frame onto the funnel and a
//! texture per frame held for the life of the session, for a layer the pane
//! does not draw.
//!
//! # Why it could not be closed before, and what had to be built first
//!
//! `PaneState::refresh_transport` keeps a running timeline's transport on
//! purpose, so switching the layer off leaves the frame list, the listing and
//! the playhead exactly where they were. The tree's existing stop for a loop
//! that is serving nobody — `retire_queues` into
//! `LoopDownloadManager::remove_pending` — **had no way back**: the plan a
//! queue is derived from was re-set only where `resample_frames` reported the
//! frame list had *changed*, and switching a layer off changes no list. A loop
//! retired that way would sit for ever holding frames it never re-asked for,
//! so nothing could afford to retire one.
//!
//! [`squallar_radar::loop_downloads::LoopDownloadManager::plan_describes`] is
//! the way back, and it is deliberately a **state** and not an event: the plan
//! is a derivation of the loop's frame list, so the walk re-derives whenever
//! the derivation no longer holds, whatever made it stop holding. A re-sample
//! is now one reason among others rather than the only one.
//!
//! # Vacuity, and the arrangement each claim needs
//!
//! A frame holding a picture is not described again, so a loop whose frames
//! are all textured asks for nothing on a fixed tree *and* on a broken one.
//! The **stopping** claim is therefore made on a loop still owed its pictures
//! and the **freeing** claim on a loop that had them, and neither is written
//! as the other.
//!
//! And a byte figure is not asked of a store counter. One arrival is one
//! `Arc` held in several places at once, so a holder that goes quiet frees
//! nothing on its own: [`hover_of`] counts the `Arc` behind the picture and
//! the claim is that it falls to the test's own single handle.

use super::loop_dispatch_tests::volume_with_sweeps;
use super::radar_timeline_addressing_tests::{active_loop, at, point_at_site, textured};
use crate::loop_frame_store::LoopFrameKey;
use squallar_radar::loop_downloads::LoopDownloadManager;
use squallar_source::id::known;
use std::sync::Arc;
use std::sync::atomic::Ordering;

const SITE: &str = "KTLX";
const TILT: f32 = 0.5;

/// The three stamps every loop in this module runs over.
fn stamps() -> Vec<chrono::NaiveDateTime> {
    vec![at(0), at(5), at(10)]
}

/// How many passes a claim about the steady state is walked for.
///
/// **More than one on purpose**, for the reason
/// `super::disabled_layer_render_tests::FRAMES` gives: a supply that fires
/// once per *change* and one that fires once per *frame* are different
/// defects, and one pass cannot tell them apart.
const PASSES: usize = 4;

/// One map pane on [`SITE`] running a plan-view loop over `stamps`, with every
/// frame's volume resident, built through the app's own config load.
///
/// **Nothing here switches radar on**, and that is the point of building it
/// this way. `n_pane_app`'s config names a site and a pane count and no layer
/// stack at all — a first launch — so the slot comes from the app's own
/// seeding, and the premise below is the front guard for every gate in this
/// file: `is_overlay_enabled` answers `false` for a slot that was never
/// minted, so a seeding that broke would make a first-run user's loop dead
/// rather than making these tests red.
fn app_with_a_loop(frames: &[chrono::NaiveDateTime]) -> crate::app::App {
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    point_at_site(&mut app, 0);
    app.loop_mgr = LoopDownloadManager::new();
    for &stamp in frames {
        app.loop_mgr
            .cache_scan(SITE, stamp, volume_with_sweeps(&[TILT]));
    }
    *app.gui
        .pane_mut(0)
        .expect("the fixture built one pane")
        .time_state_mut(&known::RADAR) = active_loop(frames);
    assert!(
        draws_radar(&app, 0),
        "premise: a pane built from a config with no layer stack came up NOT \
         needing its radar data, so every gate below would fire on a first-run \
         user and this whole file would be green over a dead loop",
    );
    app
}

/// Two map panes on one site, each running the same plan-view loop, the second
/// in no group with its layer link off — so what one pane keeps for the other
/// is the store's sharing and not the link's.
fn two_panes_with_a_loop(frames: &[chrono::NaiveDateTime]) -> crate::app::App {
    let mut app = crate::app::tests::two_pane_app(SITE, SITE);
    point_at_site(&mut app, 0);
    point_at_site(&mut app, 1);
    let second = app.gui.pane_mut(1).expect("the fixture built two panes");
    second.layer_link = false;
    second.group = None;
    assert!(
        !app.gui.panes_layer_linked(0, 1),
        "premise: nothing links these two panes",
    );
    app.loop_mgr = LoopDownloadManager::new();
    for &stamp in frames {
        app.loop_mgr
            .cache_scan(SITE, stamp, volume_with_sweeps(&[TILT]));
    }
    for idx in 0..2 {
        *app.gui
            .pane_mut(idx)
            .expect("the fixture built two panes")
            .time_state_mut(&known::RADAR) = active_loop(frames);
    }
    app
}

/// Switch a pane's radar layer off — or on — through the pane's own door.
fn set_radar(app: &mut crate::app::App, pane: usize, on: bool) {
    app.gui
        .pane_mut(pane)
        .expect("the fixture built this pane")
        .set_overlay_enabled(known::RADAR, on);
}

/// Whether this pane needs its site's radar data at all — the accessor the
/// gate reads, asked here rather than re-spelled.
fn draws_radar(app: &crate::app::App, pane: usize) -> bool {
    app.gui
        .pane(pane)
        .expect("the fixture built this pane")
        .needs_radar_data()
}

fn frame_stamps(app: &crate::app::App, pane: usize) -> Vec<chrono::NaiveDateTime> {
    app.gui
        .pane(pane)
        .expect("the fixture built this pane")
        .time_state(&known::RADAR)
        .frames
        .iter()
        .map(|frame| frame.timestamp)
        .collect()
}

/// **Which picture each of this pane's frames holds**, `None` where it holds
/// none — a "did this frame get a picture" assertion about a picture rather
/// than about a flag.
///
/// A picture key rather than a texture id, because a plan view has two
/// representations and only one of them has a texture: a fan's pixels never
/// exist on the host. The key identifies either without the caller knowing
/// which it got, and its arm tag keeps the two from colliding — see
/// `RadarSurface::picture_key`.
fn frame_pictures(app: &crate::app::App, pane: usize) -> Vec<Option<(u8, u64)>> {
    app.gui
        .pane(pane)
        .expect("the fixture built this pane")
        .time_state(&known::RADAR)
        .frames
        .iter()
        .map(|frame| {
            frame
                .image
                .as_ref()
                .and_then(squallar_egui::pane::LoopFrameImage::plan_view)
                .map(|picture| picture.surface.picture_key())
        })
        .collect()
}

/// **The `Arc` behind one frame's picture**, cloned out so a release can be
/// counted rather than inferred.
///
/// A store counter reading zero is not proof an allocation went: one picture
/// is held by the frame, by the shared frame store and by whatever else names
/// it, and a holder that stops naming it frees nothing while another holds on.
/// `Arc::strong_count` over this handle is the whole claim.
fn hover_of(
    app: &crate::app::App,
    pane: usize,
    frame: usize,
) -> Arc<squallar_radar::hover::HoverSource> {
    app.gui
        .pane(pane)
        .expect("the fixture built this pane")
        .time_state(&known::RADAR)
        .frames[frame]
        .image
        .as_ref()
        .and_then(squallar_egui::pane::LoopFrameImage::plan_view)
        .map(|picture| Arc::clone(&picture.hover))
        .expect("the frame must be holding a picture to have a hover source")
}

fn in_flight(app: &crate::app::App, pane: usize) -> Vec<bool> {
    app.gui
        .pane(pane)
        .expect("the fixture built this pane")
        .time_state(&known::RADAR)
        .frames
        .iter()
        .map(|frame| frame.render_in_flight)
        .collect()
}

fn target_of(app: &crate::app::App, pane: usize) -> super::RenderTarget {
    app.gui
        .pane(pane)
        .expect("the fixture built this pane")
        .time_state(&known::RADAR)
        .rendered_for
        .clone()
        .expect("the fixture loop is keyed")
}

/// Give every frame of `pane` its own picture, through the channel the worker
/// replies over — one raster per frame, so a frame holding another frame's
/// texture is visible as a repeat.
fn deliver_every_frame(app: &mut crate::app::App, ctx: &egui::Context, pane: usize) {
    for stamp in frame_stamps(app, pane) {
        app.channels
            .loop_render_sender
            .send(crate::channels::LoopRenderResponse {
                pane_idx: pane,
                timestamp: stamp,
                target: target_of(app, pane),
                snapped: TILT,
                site_lat: 35.33,
                site_lon: -97.27,
                image: Some(egui::ColorImage::from_rgba_unmultiplied([2, 2], &[7u8; 16])),
                max_range_km: 230.0,
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
                polar: Default::default(),
                codes: None,
            })
            .expect("the receiver lives on the App");
    }
    app.poll_loop_render_results(ctx);
}

/// Every frame of `pane` given the same fixture picture directly, for the
/// claims that are about what a loop **already holds** rather than about how
/// it got it.
fn texture_every_frame(app: &mut crate::app::App, ctx: &egui::Context, pane: usize) {
    let count = frame_stamps(app, pane).len();
    let ls = app
        .gui
        .pane_mut(pane)
        .expect("the fixture built this pane")
        .time_state_mut(&known::RADAR);
    for idx in 0..count {
        ls.frames[idx].image = Some(textured(ctx));
    }
}

fn walk(app: &mut crate::app::App, passes: usize) {
    for _ in 0..passes {
        app.dispatch_loop_renders();
    }
}

// ---------------------------------------------------------------------------
// 1. The way back, on its own.

/// **A queue retired while its frame list stood is re-derived from that list.**
///
/// This is the piece that had to exist before anything could retire a live
/// loop's queue, and it is asserted here with no layer switched off at all —
/// the retirement is performed directly, so what is on trial is the way back
/// and not the thing that uses it.
///
/// `plan_frame_count` and not `pending_queue_count`, for the reason
/// `app_fetch::loop_restore_race_tests` gives in as many words: the pending
/// queue is *queued and undispatched*, and the dispatch drains it in the same
/// pass — so it reads zero on the re-deriving path too, and an assertion
/// against it would pass whatever this walk did. The PLAN is what separates
/// "the manager was given this loop" from "it never heard of it".
#[test]
fn a_retired_queue_is_re_derived_from_the_frame_list_that_outlived_it() {
    let mut app = app_with_a_loop(&stamps());

    app.dispatch_loop_renders();
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        stamps().len(),
        "premise: an ordinary pass must give this loop a download plan, or the \
         retirement below removes nothing",
    );

    // The tree's own stop, the one that had no way back.
    app.loop_mgr.remove_pending(0);
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        0,
        "control: the manager must really have forgotten this loop, or the \
         assertion below is about a plan that never left",
    );
    assert_eq!(
        frame_stamps(&app, 0),
        stamps(),
        "control: the frame list must be untouched by the retirement — a way \
         back that needs a re-listing is not a way back",
    );

    app.dispatch_loop_renders();

    assert!(
        app.loop_mgr
            .plan_describes(0, SITE, frame_stamps(&app, 0).into_iter()),
        "a retired queue was never re-derived: the plan is re-set only where \
         `resample_frames` reports a CHANGED list, and switching a loop off \
         changes no list, so this loop would hold frames it never re-asks for",
    );
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        stamps().len(),
        "the plan came back short: a loop that comes back must come back whole",
    );
}

/// **And it settles.** The way back is a state and this walk runs every frame
/// of the UI, so it must be idempotent: a plan that describes its list is left
/// exactly alone, or every frame re-sets the plan and re-derives the queue
/// behind it for as long as the loop runs.
///
/// **Asserted on a planted queue, because the obvious observables cannot tell
/// the two apart.** `planned_for` reads `Some` either way — a re-derive sets
/// it again on the same pass it cleared it — and the pending queue reads empty
/// either way, since the dispatch pops every frame whose volume is already
/// cached. What only a re-derive does is `set_plan`, and `set_plan` *removes*
/// the pane's pending queue. So one is planted here that belongs to no frame
/// of the plan: it survives a pass that left the plan alone, and cannot
/// survive one that re-set it.
#[test]
fn a_plan_that_still_describes_its_list_is_left_alone() {
    let mut app = app_with_a_loop(&stamps());
    walk(&mut app, PASSES);

    assert!(
        app.loop_mgr
            .plan_describes(0, SITE, frame_stamps(&app, 0).into_iter()),
        "premise: the plan must describe the list after a settled walk",
    );

    app.loop_mgr.insert_pending(
        0,
        squallar_radar::loop_downloads::PendingDownloads {
            site: SITE.to_string(),
            queue: std::collections::VecDeque::from(vec![at(99)]),
        },
    );
    app.dispatch_loop_renders();

    assert_eq!(
        app.loop_mgr.pending_queue_count(0),
        1,
        "the plan was re-set on a pass that changed nothing, so this loop \
         re-derives its download queue on every frame of the UI",
    );
}

// ---------------------------------------------------------------------------
// 2. The stop, on a loop still owed its pictures.

/// **A pane that needs no radar data asks for nothing.**
///
/// Arranged on a loop whose frames are owed their pictures — the state a
/// listing that lands after the switch-off leaves, and the state every frame
/// outside the byte share is in while a loop plays. On a loop whose frames all
/// hold pictures this claim would be vacuous: a textured frame is not
/// described again on a fixed tree *or* on a broken one.
#[test]
fn a_loop_on_a_pane_that_needs_no_radar_data_asks_for_nothing() {
    let mut app = app_with_a_loop(&stamps());
    assert!(
        frame_pictures(&app, 0).iter().all(Option::is_none),
        "premise: every frame is owed its picture, or this claim is vacuous",
    );

    set_radar(&mut app, 0, false);
    walk(&mut app, PASSES);

    assert!(
        in_flight(&app, 0).iter().all(|flight| !flight),
        "a full-size raster was described for a frame no pane can paint",
    );
    assert_eq!(
        app.render.renders_in_flight.load(Ordering::Relaxed),
        0,
        "the render counter moved for a layer the pane does not draw",
    );
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        0,
        "the download plan outlived the pane's need for the data, so the queue \
         derived from it goes on buying volumes nobody can draw",
    );
    assert!(
        !app.loop_mgr.pending_pane_indices().contains(&0),
        "the pane still owns a volume download queue",
    );
}

/// The control the claim above stands on: the identical arrangement with the
/// layer left alone spends everything the disabled one does not, so the zeros
/// above are the gate's doing and not the fixture's.
#[test]
fn the_same_loop_with_its_layer_on_asks_for_all_of_it() {
    let mut app = app_with_a_loop(&stamps());
    assert!(draws_radar(&app, 0), "premise: this is the control");

    app.dispatch_loop_renders();

    assert!(
        in_flight(&app, 0).iter().any(|flight| *flight),
        "the control asked for no raster at all: the fixture cannot dispatch, \
         so every zero in this file is the fixture's and not the gate's",
    );
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        stamps().len(),
        "the control derived no download plan",
    );
}

// ---------------------------------------------------------------------------
// 3. The release, on a loop that had its pictures.

/// **What it was holding goes**, counted on the `Arc` behind the picture
/// rather than on a store's own tally — one arrival is one allocation held in
/// several places, and a holder that stops naming it frees nothing while
/// another holds on.
#[test]
fn a_loop_on_a_pane_that_needs_no_radar_data_gives_its_pictures_back() {
    let ctx = egui::Context::default();
    let mut app = app_with_a_loop(&stamps());
    texture_every_frame(&mut app, &ctx, 0);
    app.dispatch_loop_renders();

    assert!(
        frame_pictures(&app, 0).iter().all(Option::is_some),
        "premise: every frame holds a picture, or there is nothing to release",
    );
    let hover = hover_of(&app, 0, 0);
    assert!(
        Arc::strong_count(&hover) > 1,
        "premise: the picture must be held by something other than this test's \
         own handle, or a fall to 1 below says nothing",
    );
    let key = LoopFrameKey::plan_view(target_of(&app, 0), at(0));
    assert_eq!(
        app.loop_frames.holders(&key),
        1,
        "premise: the shared store holds this pane's picture",
    );

    set_radar(&mut app, 0, false);
    walk(&mut app, PASSES);

    assert!(
        frame_pictures(&app, 0).iter().all(Option::is_none),
        "the frames kept their pictures for a layer the pane does not draw",
    );
    assert_eq!(
        app.loop_frames.holders(&key),
        0,
        "the shared frame store still names the picture, so the frame letting \
         go of it freed nothing",
    );
    // **Two, and each one named.** This test's own handle, and the store's
    // copy on its way to the discard lane — `loop_frame_store::discard` hands
    // the hover source to `squallar_worker::offload` rather than dropping it
    // on the frame thread, because heavy work never lands there. That payload
    // is a transfer *for freeing*, so the lane may already have retired it by
    // the time this line runs and the count may read 1: the bound, not the
    // equality, is what makes the claim race-free in both directions.
    //
    // It is still exactly discriminating. A tree that released neither holder
    // reads 3, and the two structural assertions above name which of the two
    // it was — this one adds what neither of them can say, that no holder
    // nobody counts is keeping the allocation alive.
    assert!(
        Arc::strong_count(&hover) <= 2,
        "the picture is alive behind {} references: this test's handle and the \
         one payload already handed to the discard lane are the only two the \
         release leaves, so something else is still holding it",
        Arc::strong_count(&hover),
    );
}

// ---------------------------------------------------------------------------
// 4. The two halves together: off, then on again.

/// **The loop that comes back is the loop that left** — same stamps, same
/// order, same count, same playhead — and every frame is refilled with a
/// picture of its own.
///
/// The whole reason part one had to be built first. A supply stopped in a way
/// that cannot be undone is worse than one that was never stopped: the layer
/// comes back on, the frame list is still there, and nothing ever asks for the
/// pictures again.
#[test]
fn a_loop_switched_off_and_on_again_comes_back_whole() {
    let ctx = egui::Context::default();
    let mut app = app_with_a_loop(&stamps());

    app.dispatch_loop_renders();
    deliver_every_frame(&mut app, &ctx, 0);
    let before = frame_pictures(&app, 0);
    let playhead_before = app
        .gui
        .pane(0)
        .expect("the fixture built one pane")
        .time_state(&known::RADAR)
        .current_frame();
    assert!(
        before.iter().all(Option::is_some),
        "premise: the loop is fully drawn before it is switched off",
    );

    set_radar(&mut app, 0, false);
    walk(&mut app, PASSES);
    assert!(
        frame_pictures(&app, 0).iter().all(Option::is_none),
        "premise: the switch-off really released the pictures, or coming back \
         whole is a claim about a loop that never left",
    );
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        0,
        "premise: the queue really was retired, which is the state that had no \
         way back",
    );

    set_radar(&mut app, 0, true);
    app.dispatch_loop_renders();

    assert_eq!(
        frame_stamps(&app, 0),
        stamps(),
        "the loop came back with a different frame list: same stamps, same \
         order, same count is the contract",
    );
    assert_eq!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .time_state(&known::RADAR)
            .current_frame(),
        playhead_before,
        "the playhead moved across a switch-off it never saw",
    );
    assert!(
        app.loop_mgr
            .plan_describes(0, SITE, frame_stamps(&app, 0).into_iter()),
        "the download plan did not come back, so any frame whose volume was \
         evicted while the layer was off is never re-fetched",
    );

    deliver_every_frame(&mut app, &ctx, 0);
    let after = frame_pictures(&app, 0);
    assert!(
        after.iter().all(Option::is_some),
        "a frame was never re-asked for its picture: {after:?}",
    );
    let mut distinct: Vec<(u8, u64)> = after.iter().flatten().copied().collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        stamps().len(),
        "the re-filled frames share a texture: {after:?} — a loop that comes \
         back must come back frame for frame, not with one picture repeated",
    );
}

// ---------------------------------------------------------------------------
// 5. The front: a sibling that still draws the same site.

/// **One pane switching its layer off takes nothing from the pane beside it.**
///
/// The volumes are cached per site and the pictures are shared between panes,
/// so a release written as "drop what this loop names" would empty the site
/// out from under a sibling that is still drawing it. The store's holder count
/// is what says which of the two happened.
#[test]
fn a_sibling_still_drawing_the_site_keeps_the_shared_picture_and_the_volumes() {
    let ctx = egui::Context::default();
    let mut app = two_panes_with_a_loop(&stamps());
    texture_every_frame(&mut app, &ctx, 0);
    texture_every_frame(&mut app, &ctx, 1);
    app.dispatch_loop_renders();

    let key = LoopFrameKey::plan_view(target_of(&app, 0), at(0));
    assert_eq!(
        app.loop_frames.holders(&key),
        2,
        "premise: both panes are recorded as holders of the one picture",
    );

    set_radar(&mut app, 1, false);
    walk(&mut app, PASSES);

    assert!(
        frame_pictures(&app, 0).iter().all(Option::is_some),
        "the pane still drawing the site lost its pictures",
    );
    assert!(
        frame_pictures(&app, 1).iter().all(Option::is_none),
        "premise: the disabled pane really let go, or the sibling's pictures \
         survived a release that never happened",
    );
    assert_eq!(
        app.loop_frames.holders(&key),
        1,
        "the shared picture went with the disabled pane: the sibling draws it \
         and now has to render it again",
    );
    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        stamps().len(),
        "the site's volumes went with one pane's layer toggle, so the sibling \
         re-downloads every one of them",
    );
    assert!(
        app.loop_mgr
            .plan_describes(0, SITE, frame_stamps(&app, 0).into_iter()),
        "the drawing pane's own download plan was retired with its sibling's",
    );
}

/// The first-run pane, from the front. A pane that has saved nothing must get
/// its loop supplied — `is_overlay_enabled` reads `false` for a slot that was
/// never minted, so a gate on it that swallowed the fresh-install case would
/// leave a new user with a loop that lists its frames and never draws one.
#[test]
fn a_pane_that_has_saved_nothing_still_gets_its_loop_supplied() {
    let mut app = app_with_a_loop(&stamps());
    app.dispatch_loop_renders();

    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        stamps().len(),
        "a pane built from a config with no layer stack was refused its loop \
         supply: the gate swallowed the first-run case",
    );
    assert!(
        in_flight(&app, 0).iter().any(|flight| *flight),
        "and it was refused its rasters too",
    );
}

// ---------------------------------------------------------------------------
// 6. The readiness walk, which would otherwise finish the job.

/// The state a withdrawn supply leaves a loop in, arranged directly: every
/// frame owed its picture, no volume cached for any of them, and nothing on
/// the wire. `radar_on` decides only whether the pane still needs the data.
fn a_loop_stripped_of_its_supply(radar_on: bool) -> crate::app::App {
    let mut app = app_with_a_loop(&stamps());
    drop(app.loop_mgr.retain_scans(|_, _, _| false));
    app.loop_mgr.remove_pending(0);
    set_radar(&mut app, 0, radar_on);
    assert!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .time_state(&known::RADAR)
            .is_active(),
        "premise: the loop is running before the readiness walk judges it",
    );
    app
}

/// **A loop nothing is supplying is not a loop that failed.**
///
/// `settle_loop_phase`'s last arm answers "no frame of this loop could be
/// rendered" by destroying the timeline, and a withdrawn supply makes all
/// three of its readings true at once: every frame settled (an untextured
/// frame with no volume and nothing in flight *is* settled by
/// `render_set_settled`'s rule), no frame holding a picture, nothing still
/// arriving. So the walk that stops spending on a pane which needs no radar
/// data would, one pass later, destroy the frame list the way back is refilled
/// from — and the layer would come back on to an empty timeline. The stop and
/// the way back would each be correct and the pair of them would still lose
/// the loop.
#[test]
fn the_readiness_walk_does_not_destroy_a_loop_it_is_no_longer_supplying() {
    let mut app = a_loop_stripped_of_its_supply(false);

    app.update_loop_readiness();

    assert_eq!(
        frame_stamps(&app, 0),
        stamps(),
        "the readiness walk tore down a loop whose supply was withdrawn on \
         purpose, so switching the layer back on finds nothing to refill",
    );
    assert!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .time_state(&known::RADAR)
            .is_active(),
        "and it left loop mode entirely",
    );
}

/// The control, and the reason the test above is not vacuous: the identical
/// arrangement on a pane that *does* need its radar data is destroyed, so the
/// destroying arm is genuinely reachable from this state and the survival
/// above is the guard's doing.
#[test]
fn the_readiness_walk_still_destroys_a_loop_that_really_cannot_render() {
    let mut app = a_loop_stripped_of_its_supply(true);

    app.update_loop_readiness();

    assert!(
        frame_stamps(&app, 0).is_empty(),
        "the destroying arm was not reached at all from this arrangement, so \
         the survival asserted above says nothing about the guard",
    );
}
