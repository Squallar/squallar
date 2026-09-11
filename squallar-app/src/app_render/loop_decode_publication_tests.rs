//! **The publication reaches the pump, driven through the REAL `App` sweep.**
//!
//! The repair in `frames_needing_decode` is worthless unless
//! `App::evict_unneeded_loop_scans` publishes under the keys the plan is
//! filed at. Nothing else in this tree would notice if it did not: the pump
//! would offer every frame exactly as it did before, every gate in
//! `squallar-radar` would stay green because they publish for themselves, and
//! the only symptom would be `suppressed` sitting at 0 for ever — which is
//! also what a healthy idle loop prints.
//!
//! That is the shape `app.rs` warns about at the ceiling's own call site: a
//! wrong argument there reads green everywhere. So this drives the scene
//! rather than the predicate — a real pane, a real active loop, the real
//! sweep — and asks the manager afterwards.

use super::radar_timeline_addressing_tests::{active_loop, at, point_at_site, textured};
use crate::app::tests::two_pane_app;
use squallar_egui::pane::TimeMode;
use squallar_radar::loop_downloads::{FramePlan, LoopDownloadManager};
use squallar_source::id::known;

const SITE: &str = "KTLX";
/// Frames, and the playhead's index among them. Deliberately NOT zero: with
/// the playhead at the oldest frame, plan order and distance order agree and
/// the ordering half of the repair is invisible.
const FRAMES: usize = 8;
const PLAYHEAD: usize = 5;

/// One pane, an active loop over `FRAMES` stamps, a plan naming the same
/// stamps, an archive behind each, and the playhead parked mid-loop.
fn app_with_a_playing_loop() -> crate::app::App {
    let stamps: Vec<chrono::NaiveDateTime> = (0..FRAMES).map(|i| at(i as i64)).collect();
    let mut app = two_pane_app(SITE, SITE);
    point_at_site(&mut app, 0);
    app.loop_mgr = LoopDownloadManager::new();
    let ls = app
        .gui
        .pane_mut(0)
        .expect("the fixture built a pane")
        .time_state_mut(&known::RADAR);
    *ls = active_loop(&stamps);
    // The playhead is a derivation of the pane's clock, and `settle_playhead`
    // is its only writer, so it is placed the way the application places it.
    ls.settle_playhead(TimeMode::AsOf(at(PLAYHEAD as i64)));
    assert_eq!(
        ls.current_frame(),
        PLAYHEAD,
        "fixture: the playhead is not where this test needs it",
    );
    app.loop_mgr
        .set_plan(0, FramePlan::new(SITE.to_string(), stamps.clone()));
    for stamp in &stamps {
        app.loop_mgr
            .cache_archive(SITE, *stamp, std::sync::Arc::new(vec![0u8; 4096]));
    }
    app
}

/// **The pump's first errand is the frame on the glass, and the keys that
/// makes true are the application's own.**
///
/// The ordering is the assertion that cannot pass by accident. A rank the
/// pump could not resolve is `u64::MAX`, and the sort is stable, so a
/// publication filed under a site string or a timestamp the plan does not use
/// leaves EVERY frame at `u64::MAX` and the offers come back in plan order —
/// oldest first, playhead fifth. Getting `at(PLAYHEAD)` first therefore says
/// the site key matched and the stamps matched.
///
/// TAMPER: change the published key to `format!("{site}!")` in
/// `App::evict_unneeded_loop_scans` and this fails with the plan-order list.
#[test]
fn the_real_sweep_publishes_under_the_keys_the_plan_is_filed_at() {
    let mut app = app_with_a_playing_loop();
    // The sweep, unmodified, as the frame loop calls it.
    app.evict_unneeded_loop_scans();

    let offers: Vec<chrono::NaiveDateTime> = app
        .loop_mgr
        .frames_needing_decode(0)
        .into_iter()
        .map(|(site, stamp)| {
            assert_eq!(site, SITE, "an offer came back for another site");
            stamp
        })
        .collect();
    assert_eq!(
        offers.first().copied(),
        Some(at(PLAYHEAD as i64)),
        "the pump's first errand is not the frame on the glass, so the \
         sweep's publication did not reach it under matching keys: {offers:?}",
    );
    // And the whole order is the loop's own travel, not the plan's.
    let wanted: Vec<chrono::NaiveDateTime> = (0..FRAMES)
        .map(|i| at(((PLAYHEAD + i) % FRAMES) as i64))
        .collect();
    assert_eq!(
        offers, wanted,
        "the offers are not ordered forward from the playhead",
    );
}

/// **And a frame nothing will read is declined, with the fires-counter saying
/// so.**
///
/// `suppressed` reading 0 is the precondition failing, and it is also what an
/// idle loop prints — so it is asserted here against a scene built to make it
/// fire, rather than watched for in the field and hoped about.
///
/// TAMPER: delete the `set_decode_wants` call from
/// `App::evict_unneeded_loop_scans` and `suppressed` returns to 0.
#[test]
fn a_textured_frame_outside_the_lookahead_is_not_offered_to_the_pump() {
    let ctx = egui::Context::default();
    let mut app = app_with_a_playing_loop();
    // Every frame carries a picture, so the residency sweep wants the moments
    // of only the playhead and its lookahead — and the pump, before this
    // repair, offered all eight regardless and re-decoded the six it could
    // never keep.
    let ls = app
        .gui
        .pane_mut(0)
        .expect("the fixture built a pane")
        .time_state_mut(&known::RADAR);
    for frame in &mut ls.frames {
        frame.image = Some(textured(&ctx));
    }

    app.evict_unneeded_loop_scans();
    let offers = app.loop_mgr.frames_needing_decode(0);
    let (_, suppressed, ..) = app.loop_mgr.decode_churn();

    assert!(
        suppressed > 0,
        "nothing was suppressed on a scene where every frame is textured, so \
         the publication never reached the pump",
    );
    assert!(
        offers.len() < FRAMES,
        "the pump was offered all {FRAMES} frames though the sweep keeps only \
         the playhead and its lookahead: {offers:?}",
    );
    // What it DOES still offer is bounded by what the sweep keeps, and the
    // playhead is always in it: the repair may never refuse the frame the
    // user is looking at.
    assert!(
        offers
            .iter()
            .any(|(_, stamp)| *stamp == at(PLAYHEAD as i64)),
        "the frame on the glass was refused a decode: {offers:?}",
    );
}
