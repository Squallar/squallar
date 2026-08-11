//! What the event loop is allowed to sleep through.
//!
//! The bug these exist for: the frontend used to re-arm an unconditional
//! redraw at the end of every frame while `is_auto_poll_active()` answered
//! yes, and that answer was `enabled && initial_fetch_done` — true from frame
//! one of the default configuration and never false again. So the app drew at
//! the display's refresh rate, forever, with no user input and nothing in
//! flight, to service a poll that fires once a minute.
//!
//! Every test here is about the replacement being a *duration*, and about that
//! duration being long. The two failure directions are opposite and both
//! serious: too short is the busy loop back again, and too long is a poll that
//! never fires or a countdown that freezes on screen.

use super::*;

/// A whole second's worth of tolerance for the tests that name a deadline.
/// These read the real clock — `AutoPollState` is written against
/// `web_time::Instant` and has no injectable now — so the assertions are
/// bounds rather than equalities.
const SLACK: std::time::Duration = std::time::Duration::from_millis(500);

/// A poller that last fetched `ago` ago, as a frame that polled would leave
/// it.
fn polled(gui: &mut Gui, ago: std::time::Duration) {
    gui.auto_poll.last_fetch_time = Some(web_time::Instant::now() - ago);
}

/// The default poll interval `AutoPollState::on_success` resets to.
const INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Every layer off, so the radar poll is the only term left in the answer.
///
/// A fresh `Gui` opens with layers that refresh on timers of their own, and
/// one that has never been fetched is due *now* — a true answer, and the one
/// the overlay test below is about, but not the one these are.
fn only_the_radar_poll(gui: &mut Gui) {
    for &kind in OverlayKind::all() {
        gui.pane_mut(0)
            .expect("a fresh Gui has one pane")
            .enabled_overlays
            .insert(kind, false);
    }
}

/// The replacement's whole point: an idle app with auto-poll on is left to
/// sleep for the rest of the interval, rather than asked to draw again now.
#[test]
fn an_idle_poller_sleeps_out_the_rest_of_its_interval() {
    let mut gui = Gui::new();
    only_the_radar_poll(&mut gui);
    polled(&mut gui, std::time::Duration::from_secs(5));

    let delay = gui
        .auto_poll_delay()
        .expect("a live pane with auto-poll on is owed a poll");
    assert!(
        delay > INTERVAL - std::time::Duration::from_secs(5) - SLACK
            && delay <= INTERVAL - std::time::Duration::from_secs(5) + SLACK,
        "the wake is not the remainder of the interval: {delay:?}"
    );
}

/// The scheduling half and the firing half must agree, or a wake is spent on
/// a frame that polls nothing — which is the busy loop with extra steps.
///
/// Checked either side of the boundary rather than at one point, because the
/// two are written in different units: `should_poll` compares whole seconds
/// and this compares durations.
#[test]
fn the_wake_lands_exactly_when_the_poll_would_fire() {
    for (ago, due) in [
        (std::time::Duration::from_millis(59_500), false),
        (INTERVAL, true),
        (INTERVAL + std::time::Duration::from_secs(30), true),
    ] {
        let mut gui = Gui::new();
        polled(&mut gui, ago);
        assert_eq!(
            gui.auto_poll.should_poll(),
            due,
            "the premise moved: {ago:?} into a {INTERVAL:?} interval"
        );
        assert_eq!(
            gui.auto_poll
                .poll_delay()
                .expect("a timer is running")
                .is_zero(),
            due,
            "at {ago:?} the schedule and the poll disagree about whether a \
             round is due, so a wake will be spent on a frame that polls \
             nothing"
        );
    }
}

/// A poll that cannot fire must not be scheduled for, however overdue its
/// timer reads.
///
/// `time_until_next` saturates at zero, so a pane taken off live leaves the
/// timer permanently expired. Scheduling on it would be a zero-length sleep
/// re-armed on every iteration — the worst version of the bug, not a fix for
/// it.
#[test]
fn a_poll_no_pane_can_use_is_not_scheduled_for() {
    let mut gui = Gui::new();
    only_the_radar_poll(&mut gui);
    polled(&mut gui, INTERVAL * 4);
    gui.set_viewing_live_for_pane(0, false);

    assert!(
        !gui.is_any_pane_live(),
        "precondition: nothing on screen wants a live scan"
    );
    assert_eq!(
        gui.auto_poll_delay(),
        None,
        "an app whose panes are all historic is being woken for a poll that \
         `check_auto_polls` will refuse"
    );
}

/// Turning auto-poll off has to stop the wake as well as the poll.
#[test]
fn auto_poll_switched_off_asks_for_nothing() {
    let mut gui = Gui::new();
    only_the_radar_poll(&mut gui);
    polled(&mut gui, std::time::Duration::from_secs(5));
    gui.auto_poll.enabled = false;

    assert_eq!(gui.auto_poll_delay(), None);
    assert_eq!(
        gui.status_tick_delay(),
        None,
        "there is no countdown on screen to advance either"
    );
}

/// A fetch in flight suppresses the poll (`check_auto_polls` refuses while
/// `radar.fetching`), so it must suppress the wake too. What ends the wait is
/// the fetch landing, and that asks for its own frame.
#[test]
fn a_fetch_in_flight_yields_the_wake_to_whatever_ends_it() {
    let mut gui = Gui::new();
    only_the_radar_poll(&mut gui);
    polled(&mut gui, INTERVAL * 2);
    gui.set_fetching(true);

    assert_eq!(gui.auto_poll_delay(), None);
}

/// An overlay's refresh is on the same terms as the radar poll: scheduled
/// while some pane on screen can draw it, and not otherwise.
#[test]
fn an_overlay_is_scheduled_for_only_while_a_pane_can_draw_it() {
    let kind = OverlayKind::NwsAlerts;
    let mut gui = Gui::new();
    let interval = gui
        .overlays
        .auto_poll_interval(kind)
        .expect("NWS alerts auto-poll; this test needs a layer that does");

    // Nothing enables it, so nothing is owed — the old predicate answered
    // "yes, forever" for any layer with an interval, whether or not one was
    // on screen.
    gui.pane_mut(0)
        .unwrap()
        .enabled_overlays
        .insert(kind, false);
    assert_eq!(gui.overlay_poll_delay(kind), None);

    gui.pane_mut(0).unwrap().enabled_overlays.insert(kind, true);
    assert_eq!(
        gui.overlay_poll_delay(kind),
        Some(std::time::Duration::ZERO),
        "a layer that has never been fetched is due now"
    );

    // Fed through the production ingest path rather than a test setter, so
    // the timer this reads is the one a real fetch would leave behind.
    gui.overlays.apply_fetch_result(
        rustdar_overlays::render::overlay_state::OverlayFetchResult {
            kind,
            data: OverlayRegistry::nws_alerts_payload(Vec::new()),
        },
    );
    let delay = gui.overlay_poll_delay(kind).expect("still owed");
    let interval = std::time::Duration::from_secs(interval);
    assert!(
        delay > interval - SLACK && delay <= interval,
        "a layer fetched just now must be scheduled a whole interval out, \
         not {delay:?}"
    );

    gui.overlays.set_fetching(kind, true);
    assert_eq!(
        gui.overlay_poll_delay(kind),
        None,
        "a refresh already in flight is being scheduled for a second time"
    );
}

/// The countdown on the status bar is the one thing that changes with no
/// input, and it must land on the second it changes — not sooner, which is a
/// repaint for the same string, and not later, which drops a number.
#[test]
fn the_countdown_wake_lands_on_the_second_the_number_moves() {
    let mut gui = Gui::new();
    polled(&mut gui, std::time::Duration::from_millis(10_400));
    assert_eq!(
        gui.auto_poll.time_until_next(),
        Some(50),
        "precondition: the bar is printing `archive 50s`"
    );

    let tick = gui
        .auto_poll
        .countdown_tick_delay()
        .expect("the count is still moving");
    assert!(
        tick > std::time::Duration::from_millis(500)
            && tick <= std::time::Duration::from_millis(700),
        "the tick is not the remainder of this second: {tick:?}"
    );
}

/// …and stops asking once the number has stopped moving. `time_until_next`
/// saturates at zero, so a poll that cannot fire leaves `archive 0s` on
/// screen: a string that will read the same forever, which is exactly what
/// must not be repainted once a second for the life of the process.
#[test]
fn a_countdown_that_has_bottomed_out_asks_for_no_more_frames() {
    let mut gui = Gui::new();
    polled(&mut gui, INTERVAL * 3);
    assert_eq!(
        gui.auto_poll.time_until_next(),
        Some(0),
        "precondition: the count has bottomed out"
    );

    assert_eq!(gui.auto_poll.countdown_tick_delay(), None);
}

/// The tick is never zero, whatever the phase of the clock. A zero-length
/// sleep re-armed every iteration is the spin this whole path exists to
/// avoid, and this term is the one the caller's floor deliberately does not
/// cover.
#[test]
fn the_countdown_tick_is_never_a_zero_length_sleep() {
    for millis in [0, 1, 999, 1_000, 1_001, 30_000, 59_999] {
        let mut gui = Gui::new();
        polled(&mut gui, std::time::Duration::from_millis(millis));
        let tick = gui
            .auto_poll
            .countdown_tick_delay()
            .expect("the count is still moving");
        assert!(
            !tick.is_zero() && tick <= std::time::Duration::from_secs(1),
            "at {millis}ms in, the countdown asked for a {tick:?} sleep"
        );
    }
}

/// A status bar nobody is looking at costs nothing. The tick is what the bar
/// itself recorded while drawing, so an app that has drawn no bar — a phone
/// shell, a faded-out chrome, a collapsed bar — is owed no frame for it.
#[test]
fn a_status_bar_that_never_drew_asks_for_no_frames() {
    let mut gui = Gui::new();
    polled(&mut gui, std::time::Duration::from_secs(5));

    assert_eq!(
        gui.status_tick_delay(),
        None,
        "a countdown nobody has drawn is holding the event loop awake"
    );
}
