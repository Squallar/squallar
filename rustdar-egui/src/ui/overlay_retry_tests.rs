//! What a *failing* overlay fetch costs, measured in attempts.
//!
//! The bug these exist for: "is this layer due a fetch?" was
//! `fetch_time.is_none_or(|t| t.elapsed() >= interval)`, and `fetch_time` is
//! stamped only on success. A failed fetch cleared `fetching` and left the
//! stamp alone, so the next frame found the layer due, and the frame after
//! that. Measured in headless Chromium against a failing SPC Mesoscale
//! Discussion feed: **3089 `SPC MD fetch failed` lines in 105 s** — 29.4
//! requests a second, one per animation frame, from every open tab, for as long
//! as the app stayed open.
//!
//! The sibling file `wake_schedule_tests.rs` covers the *healthy* schedule —
//! what an idle app is allowed to sleep through. These cover the failing one,
//! and the two failure directions are the same pair: too eager is the storm
//! back again, and too slow is a layer that never recovers.
//!
//! Everything here drives `Gui::check_auto_polls` — the real gate, on real
//! frames, feeding failures through the real ingest path — rather than reading
//! the policy arithmetic, which `rustdar_overlays::fetch_policy` pins on its
//! own.

use super::*;
use rustdar_overlays::fetch_policy::{
    BROKEN_RETRY_SECS, FetchError, FetchHealth, REFUSALS_BEFORE_BROKEN,
};
use rustdar_overlays::render::overlay_state::OverlayFetchResult;
use std::time::Duration;

/// The layer the storm was found on.
const KIND: rustdar_source::id::LayerId = rustdar_source::id::known::SPC_DISCUSSIONS;

/// A `Gui` with exactly one auto-polling layer on screen, so the attempt count
/// is unambiguous.
fn gui_with_only_discussions() -> Gui {
    let mut gui = Gui::new();
    for kind in crate::sources::default_draw_order() {
        let on = kind == KIND;
        gui.pane_mut(0)
            .expect("a fresh Gui has one pane")
            .enabled_overlays
            .insert(kind, on);
    }
    gui
}

/// One frame of the real auto-poll gate, with every fetch it starts failing
/// transiently. Returns how many fetches that frame began.
///
/// This is the production path end to end: `check_auto_polls` decides,
/// `set_fetching` marks it in flight exactly as `App::fetch_overlay` does, and
/// the error arrives through `apply_fetch_result` in the payload the network
/// would have delivered.
fn failing_frame(gui: &mut Gui) -> usize {
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    let mut started = 0;
    for action in actions {
        if let GuiAction::FetchOverlay { kind, .. } = action
            && kind == KIND
        {
            started += 1;
            gui.overlays.set_fetching(&kind, true);
            gui.overlays.apply_fetch_result(OverlayFetchResult {
                kind,
                data: OverlayRegistry::spc_discussions_failure_payload(FetchError::transient(
                    "SPC MD RSS request failed: connection refused",
                )),
            });
        }
    }
    started
}

/// Drive `frames` frames and total the attempts.
fn drive(gui: &mut Gui, frames: usize) -> usize {
    (0..frames).map(|_| failing_frame(gui)).sum()
}

/// **The storm test.** Thousands of frames inside the first backoff window buy
/// exactly one attempt.
///
/// The frame count is the measured one: 3089 frames is what the browser drew in
/// the 105 s that produced 3089 requests. Remove the backoff and this asserts
/// 3089 against 1.
#[test]
fn a_failing_layer_is_not_refetched_on_the_next_frame() {
    let mut gui = gui_with_only_discussions();
    let start = std::time::Instant::now();
    let attempts = drive(&mut gui, 3089);
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "premise: 3089 gate evaluations must fit inside the first backoff rung \
         for the count below to mean what it says, but took {elapsed:?}",
    );
    assert_eq!(
        attempts, 1,
        "a failing SPC MD fetch is being retried per frame — this is the 3089 \
         requests in 105 s that `rustdar_overlays::fetch_policy` exists to stop",
    );
}

/// The ladder, climbed through the real gate one rung at a time: 2 s doubling
/// to the layer's own poll interval, and never faster than that ceiling.
///
/// Each rung is checked from both sides. Only-below would pass for a backoff
/// that never fires again; only-at would pass for one that fires early.
#[test]
fn the_ladder_is_climbed_one_attempt_per_rung() {
    let mut gui = gui_with_only_discussions();
    let interval = gui
        .overlays
        .auto_poll_interval(&KIND)
        .expect("SPC discussions auto-poll; this test needs a layer that does");

    // The first attempt, and the failure that starts the ladder.
    assert_eq!(drive(&mut gui, 1), 1, "the first fetch must go out at once");

    for (rung, secs) in [2u64, 4, 8, 16, 32, 64, 120, 120, 120].iter().enumerate() {
        assert!(
            *secs <= interval,
            "rung {rung} of {secs}s exceeds the {interval}s ceiling the layer declares",
        );

        // A second short of the rung: still waiting.
        gui.overlays
            .rewind_retry(&KIND, Duration::from_secs(secs - 1));
        assert_eq!(
            drive(&mut gui, 200),
            0,
            "rung {rung}: a fetch went out {}s into a {secs}s backoff",
            secs - 1,
        );

        // The last second of it: exactly one attempt, however many frames run.
        gui.overlays.rewind_retry(&KIND, Duration::from_secs(1));
        assert_eq!(
            drive(&mut gui, 200),
            1,
            "rung {rung}: {secs}s elapsed and the retry did not fire exactly once",
        );
    }
}

/// The measured window, through the real gate: the 105 s that cost 3089
/// requests now costs 6.
///
/// Walked a simulated second at a time, so it counts what the gate does rather
/// than what the arithmetic says it should.
#[test]
fn the_measured_storm_window_costs_six_attempts() {
    let mut gui = gui_with_only_discussions();
    let mut attempts = drive(&mut gui, 1);
    for _ in 0..105 {
        gui.overlays.rewind_retry(&KIND, Duration::from_secs(1));
        attempts += drive(&mut gui, 1);
    }
    assert_eq!(
        attempts, 6,
        "the 105 s window that produced 3089 requests must now produce 6",
    );
}

/// A user is never made to wait out a backoff — not even at the ceiling.
#[test]
fn a_user_fetch_is_answered_immediately_however_deep_the_backoff() {
    let mut gui = gui_with_only_discussions();
    drive(&mut gui, 1);
    // Climb to the ceiling.
    for secs in [2u64, 4, 8, 16, 32, 64] {
        gui.overlays.rewind_retry(&KIND, Duration::from_secs(secs));
        drive(&mut gui, 1);
    }
    let delay = gui
        .overlay_poll_delay(&KIND)
        .expect("a failing layer is still owed an eventual poll");
    assert!(
        delay > Duration::from_secs(60),
        "premise: the layer should be deep in backoff, but is due in {delay:?}",
    );
    assert_eq!(drive(&mut gui, 500), 0, "premise: nothing automatic is due");

    let mut actions = Vec::new();
    push_user_overlay_fetch(&mut gui.overlays, &mut actions, KIND, 0);

    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GuiAction::FetchOverlay { kind, .. } if *kind == KIND)),
        "pressing Refresh queued nothing",
    );
    assert_eq!(
        gui.overlays.auto_fetch_delay(&KIND),
        Some(Duration::ZERO),
        "a user action left the layer waiting out its backoff",
    );
}

/// Feed one refusal through the real ingest path.
fn refuse(gui: &mut Gui) {
    gui.overlays.set_fetching(&KIND, true);
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: KIND,
        data: OverlayRegistry::spc_discussions_failure_payload(FetchError::permanent(
            "SPC returned HTTP 400 for MD RSS feed",
        )),
    });
}

/// **One 4xx must not take a layer off the poll.** The frame-level half of
/// `fetch_policy`'s `one_refusal_does_not_condemn_a_layer`.
///
/// This is the bug in its original shape: a single 403 wrote `Broken`,
/// `auto_fetch_delay` returned `None`, and the layer never fetched again for
/// the life of the session — while going on drawing whatever it last held.
#[test]
fn one_refusal_leaves_the_layer_on_the_ordinary_ladder() {
    let mut gui = gui_with_only_discussions();
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    refuse(&mut gui);

    let delay = gui
        .overlay_poll_delay(&KIND)
        .expect("one refusal must not end the schedule");
    assert!(
        delay <= Duration::from_secs(2),
        "a first refusal must sit on the first rung, not a long one: {delay:?}",
    );
    assert_eq!(drive(&mut gui, 200), 0, "premise: the rung has not elapsed");
    gui.overlays.rewind_retry(&KIND, Duration::from_secs(2));
    assert_eq!(
        drive(&mut gui, 200),
        1,
        "the layer stopped polling after a single 4xx",
    );
}

/// A *run* of refusals is believed: the layer drops to the broken heartbeat
/// rather than the ceiling. Costly to be wrong about, so it takes evidence.
#[test]
fn a_run_of_refusals_drops_the_layer_to_a_heartbeat() {
    let mut gui = gui_with_only_discussions();
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    for _ in 0..REFUSALS_BEFORE_BROKEN {
        refuse(&mut gui);
        gui.overlays.rewind_retry(&KIND, Duration::from_secs(64));
    }

    let interval = Duration::from_secs(gui.overlays.auto_poll_interval(&KIND).unwrap());
    let delay = gui
        .overlay_poll_delay(&KIND)
        .expect("broken is a slower poll, never a stopped one");
    assert!(
        delay > interval,
        "a refused layer must cost less than a healthy one, not the same: {delay:?}",
    );
    assert_eq!(
        drive(&mut gui, 500),
        0,
        "the heartbeat fired immediately — that is the ceiling, not a heartbeat",
    );
}

/// **The absorbing-state test.** A broken layer must come back on its own.
///
/// `auto_fetch_delay` returning `None` for a broken layer made the state
/// unreachable-from: no automatic fetch could run, so no success could ever be
/// recorded, so nothing could ever clear the verdict. A layer condemned by a
/// transient WAF rule stayed condemned until the process exited. Here the
/// heartbeat comes due, the fetch goes out, and it succeeds.
#[test]
fn a_broken_layer_recovers_on_its_own_once_the_heartbeat_comes_due() {
    let mut gui = gui_with_only_discussions();
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    for _ in 0..REFUSALS_BEFORE_BROKEN {
        refuse(&mut gui);
        gui.overlays.rewind_retry(&KIND, Duration::from_secs(64));
    }
    assert!(
        gui.overlays
            .fetch_health(&KIND)
            .is_some_and(FetchHealth::is_unhealthy),
        "premise: the layer is broken",
    );

    gui.overlays
        .rewind_retry(&KIND, Duration::from_secs(BROKEN_RETRY_SECS));
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GuiAction::FetchOverlay { kind, .. } if *kind == KIND)),
        "the heartbeat never came due; a broken layer is still a dead end",
    );

    // And the fetch it produced can clear the verdict, which is the property
    // the old `None` made unreachable.
    gui.overlays.set_fetching(&KIND, true);
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: KIND,
        data: OverlayRegistry::spc_discussions_payload(Vec::new()),
    });
    assert_eq!(
        gui.overlays.fetch_health(&KIND),
        Some(&FetchHealth::Ok),
        "a success on the heartbeat did not clear the verdict",
    );
}

/// A user can still revive a broken layer at once, without waiting out the
/// heartbeat.
#[test]
fn refresh_revives_a_broken_layer_immediately() {
    let mut gui = gui_with_only_discussions();
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    for _ in 0..REFUSALS_BEFORE_BROKEN {
        refuse(&mut gui);
        gui.overlays.rewind_retry(&KIND, Duration::from_secs(64));
    }
    assert_eq!(drive(&mut gui, 500), 0, "premise: nothing automatic is due");

    let mut actions = Vec::new();
    push_user_overlay_fetch(&mut gui.overlays, &mut actions, KIND, 0);
    assert_eq!(
        gui.overlays.auto_fetch_delay(&KIND),
        Some(Duration::ZERO),
        "Refresh must revive a layer that was given up on",
    );
}

/// **The recovery that did not recover.** Switching a stale layer off and on
/// again must re-ask the origin.
///
/// The guard on the enable-fetch rule was `!has_data(kind)`, and `has_data` is
/// `!data.is_empty()` — so a layer that had worked, then took a 4xx, was
/// *holding* data and did not re-ask. Toggling it off and on did nothing at
/// all, in exactly the dangerous case: an alerts layer painting a warning set
/// that stopped updating an hour ago looks identical to one that is current,
/// and "off and on again" is the first thing a user tries.
///
/// The original guard still holds where it should: the last block toggles a
/// *healthy* layer and asserts no request is spent.
#[test]
fn toggling_a_stale_layer_off_and_on_re_asks_the_origin() {
    let mut gui = gui_with_only_discussions();

    // A layer with data on screen — the case the old guard skipped.
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: KIND,
        data: OverlayRegistry::spc_discussions_payload(vec![a_discussion()]),
    });
    assert!(gui.overlays.has_data(&KIND), "premise: something is drawn");

    for _ in 0..REFUSALS_BEFORE_BROKEN {
        refuse(&mut gui);
        gui.overlays.rewind_retry(&KIND, Duration::from_secs(64));
    }
    assert!(
        gui.overlays.has_data(&KIND),
        "premise: the stale data is still on screen, which is the whole danger",
    );

    let mut actions = Vec::new();
    let mut pane = std::mem::take(gui.pane_mut(0).expect("a fresh Gui has one pane"));
    gui.set_pane_overlay_with_fetch(&mut pane, 0, &KIND, false, &mut actions);
    gui.set_pane_overlay_with_fetch(&mut pane, 0, &KIND, true, &mut actions);
    *gui.pane_mut(0).expect("one pane") = pane;

    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GuiAction::FetchOverlay { kind, .. } if *kind == KIND)),
        "toggling a stale layer off and on queued nothing — the frozen warning \
         set stays frozen and the user has no other lever",
    );
    assert_eq!(
        gui.overlays.fetch_health(&KIND),
        Some(&FetchHealth::Ok),
        "the toggle queued a fetch but left the ledger condemned, so the very \
         next automatic poll would still be on the heartbeat",
    );

    // The guard's own job, unchanged: a healthy layer with fresh data does not
    // spend a request on being switched on. This is what keeps a preset that
    // enables eight layers on four panes from being thirty-two requests.
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: KIND,
        data: OverlayRegistry::spc_discussions_payload(vec![a_discussion()]),
    });
    let mut actions = Vec::new();
    let mut pane = std::mem::take(gui.pane_mut(0).expect("one pane"));
    gui.set_pane_overlay_with_fetch(&mut pane, 0, &KIND, false, &mut actions);
    gui.set_pane_overlay_with_fetch(&mut pane, 0, &KIND, true, &mut actions);
    *gui.pane_mut(0).expect("one pane") = pane;
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, GuiAction::FetchOverlay { kind, .. } if *kind == KIND)),
        "a healthy layer spent a request on being toggled off and on",
    );
}

/// One MD with a polygon, so `has_data()` is true and something is drawn.
fn a_discussion() -> rustdar_overlays::spc::discussion::SpcDiscussion {
    use rustdar_overlays::spc::colors::{md_fill_color, md_stroke_color};
    use rustdar_overlays::spc::discussion::{MdType, SpcDiscussion};
    use rustdar_overlays::types::{HatchPattern, OverlayFeature};

    let md_type = MdType::Convective;
    let polygon = vec![vec![(35.0, -97.0), (36.0, -97.0), (36.0, -96.0)]];
    SpcDiscussion {
        number: 1234,
        title: "Mesoscale Discussion #1234".into(),
        text: String::new(),
        link: String::new(),
        md_type,
        polygon: polygon.clone(),
        feature: OverlayFeature::new(
            vec![polygon],
            md_fill_color(&md_type),
            md_stroke_color(&md_type),
            "MD 1234".into(),
            String::new(),
            HatchPattern::None,
        ),
        concerning: None,
    }
}

/// "Not published right now" is an answer, not a fault: it puts the layer back
/// on its ordinary interval instead of onto the ladder, and says so.
#[test]
fn an_absent_product_polls_at_the_ordinary_interval() {
    let mut gui = gui_with_only_discussions();
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    gui.overlays.set_fetching(&KIND, true);
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: KIND,
        data: OverlayRegistry::spc_discussions_failure_payload(FetchError::absent(
            "SPC returned HTTP 404",
        )),
    });

    let interval = Duration::from_secs(gui.overlays.auto_poll_interval(&KIND).unwrap());
    let delay = gui.overlay_poll_delay(&KIND).expect("still polling");
    assert!(
        delay > interval - Duration::from_secs(2) && delay <= interval,
        "an absent product must resume the ordinary interval, not a backoff: {delay:?}",
    );
    assert_eq!(
        drive(&mut gui, 500),
        0,
        "an absent product left the layer due on every frame",
    );
}

/// A success mid-ladder returns the layer to its ordinary interval.
#[test]
fn a_success_clears_the_backoff() {
    let mut gui = gui_with_only_discussions();
    drive(&mut gui, 1);
    for secs in [2u64, 4, 8] {
        gui.overlays.rewind_retry(&KIND, Duration::from_secs(secs));
        drive(&mut gui, 1);
    }

    gui.overlays.set_fetching(&KIND, true);
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: KIND,
        data: OverlayRegistry::spc_discussions_payload(Vec::new()),
    });

    let interval = Duration::from_secs(gui.overlays.auto_poll_interval(&KIND).unwrap());
    let delay = gui.overlay_poll_delay(&KIND).expect("still polling");
    assert!(
        delay > interval - Duration::from_secs(2) && delay <= interval,
        "a good answer must put the layer back on its interval: {delay:?}",
    );
}

/// The gate that fires and the wake that schedules for it must be one reading.
///
/// They were two — `check_auto_polls` compared whole seconds, `overlay_poll_delay`
/// subtracted durations — and a wake spent on a frame that polls nothing is the
/// busy loop with extra steps.
#[test]
fn the_wake_and_the_poll_agree_on_a_failing_layer() {
    let mut gui = gui_with_only_discussions();
    drive(&mut gui, 1);
    for secs in [2u64, 4, 8, 16] {
        let due = gui.overlay_poll_delay(&KIND).expect("owed a poll");
        assert_eq!(
            due.is_zero(),
            failing_frame(&mut gui) == 1,
            "the schedule and the gate disagree about whether a fetch is due",
        );
        gui.overlays.rewind_retry(&KIND, Duration::from_secs(secs));
    }
}
