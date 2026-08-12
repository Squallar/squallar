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
use rustdar_overlays::fetch_policy::FetchError;
use rustdar_overlays::render::overlay_state::OverlayFetchResult;
use std::time::Duration;

/// The layer the storm was found on.
const KIND: OverlayKind = OverlayKind::SpcDiscussions;

/// A `Gui` with exactly one auto-polling layer on screen, so the attempt count
/// is unambiguous.
fn gui_with_only_discussions() -> Gui {
    let mut gui = Gui::new();
    for &kind in OverlayKind::all() {
        gui.pane_mut(0)
            .expect("a fresh Gui has one pane")
            .enabled_overlays
            .insert(kind, kind == KIND);
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
            gui.overlays.set_fetching(kind, true);
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
        .auto_poll_interval(KIND)
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
            .rewind_retry(KIND, Duration::from_secs(secs - 1));
        assert_eq!(
            drive(&mut gui, 200),
            0,
            "rung {rung}: a fetch went out {}s into a {secs}s backoff",
            secs - 1,
        );

        // The last second of it: exactly one attempt, however many frames run.
        gui.overlays.rewind_retry(KIND, Duration::from_secs(1));
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
        gui.overlays.rewind_retry(KIND, Duration::from_secs(1));
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
        gui.overlays.rewind_retry(KIND, Duration::from_secs(secs));
        drive(&mut gui, 1);
    }
    let delay = gui
        .overlay_poll_delay(KIND)
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
        gui.overlays.auto_fetch_delay(KIND),
        Some(Duration::ZERO),
        "a user action left the layer waiting out its backoff",
    );
}

/// A permanent failure is said in the state rather than retried forever at a
/// slow cadence — and a user can still revive it.
#[test]
fn a_permanent_failure_stops_the_automatic_poll_entirely() {
    let mut gui = gui_with_only_discussions();
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    gui.overlays.set_fetching(KIND, true);
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: KIND,
        data: OverlayRegistry::spc_discussions_failure_payload(FetchError::permanent(
            "SPC returned HTTP 403 for MD RSS feed",
        )),
    });

    assert_eq!(
        gui.overlay_poll_delay(KIND),
        None,
        "a layer that cannot succeed is still being scheduled for",
    );
    // Far past any ceiling: a slow retry would have fired many times over.
    gui.overlays.rewind_retry(KIND, Duration::from_secs(86_400));
    assert_eq!(
        drive(&mut gui, 500),
        0,
        "a permanent failure is being retried anyway, just more slowly",
    );

    let mut actions = Vec::new();
    push_user_overlay_fetch(&mut gui.overlays, &mut actions, KIND, 0);
    assert_eq!(
        gui.overlays.auto_fetch_delay(KIND),
        Some(Duration::ZERO),
        "Refresh must revive a layer that was given up on",
    );
}

/// "Not published right now" is an answer, not a fault: it puts the layer back
/// on its ordinary interval instead of onto the ladder, and says so.
#[test]
fn an_absent_product_polls_at_the_ordinary_interval() {
    let mut gui = gui_with_only_discussions();
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    gui.overlays.set_fetching(KIND, true);
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: KIND,
        data: OverlayRegistry::spc_discussions_failure_payload(FetchError::absent(
            "SPC returned HTTP 404",
        )),
    });

    let interval = Duration::from_secs(gui.overlays.auto_poll_interval(KIND).unwrap());
    let delay = gui.overlay_poll_delay(KIND).expect("still polling");
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
        gui.overlays.rewind_retry(KIND, Duration::from_secs(secs));
        drive(&mut gui, 1);
    }

    gui.overlays.set_fetching(KIND, true);
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: KIND,
        data: OverlayRegistry::spc_discussions_payload(Vec::new()),
    });

    let interval = Duration::from_secs(gui.overlays.auto_poll_interval(KIND).unwrap());
    let delay = gui.overlay_poll_delay(KIND).expect("still polling");
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
        let due = gui.overlay_poll_delay(KIND).expect("owed a poll");
        assert_eq!(
            due.is_zero(),
            failing_frame(&mut gui) == 1,
            "the schedule and the gate disagree about whether a fetch is due",
        );
        gui.overlays.rewind_retry(KIND, Duration::from_secs(secs));
    }
}
