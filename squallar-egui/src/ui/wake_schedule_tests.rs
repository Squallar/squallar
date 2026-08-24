//! What the event loop is allowed to sleep through.

use super::*;
use squallar_source::handler::PaneRef;

/// A whole second's tolerance: these read the real clock, so the assertions
/// are bounds rather than equalities.
const SLACK: std::time::Duration = std::time::Duration::from_millis(500);

/// Put the radar layer's poll clock `ago` in the past, through the real
/// doors: the round is asked for (which is what stamps the clock — see
/// `RadarSource::set_fetching`), ends in the same breath because nothing
/// tracks an archive check, and the clock is then aged.
fn polled(gui: &mut Gui, ago: std::time::Duration) {
    gui.set_radar_round_in_flight(true);
    gui.set_radar_round_in_flight(false);
    gui.overlays
        .rewind_fetch_time(&crate::radar_layer::POLL_LAYER, ago);
}

/// What the status bar's chip reads off the radar layer.
fn chip(gui: &Gui) -> super::statusbar::ArchivePoll {
    super::statusbar::ArchivePoll::of(&gui.overlays)
}

const INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Silence every layer's refresh but the archive poll's — and **leave the
/// radar layer itself shown**, because that is what makes these tests about
/// the gate they name.
///
/// The radar poll is gated on a pane VIEWING LIVE, not on the layer being
/// enabled. Turning radar off here as well would make the two predicates
/// agree on every fixture, and a test that cannot tell them apart proves
/// nothing about which one the code consults.
fn only_the_radar_poll(gui: &mut Gui) {
    for kind in crate::sources::default_draw_order() {
        if kind == crate::radar_layer::POLL_LAYER {
            continue;
        }
        gui.pane_mut(0)
            .expect("a fresh Gui has one pane")
            .set_overlay_enabled(kind.clone(), false);
    }
    assert!(
        gui.any_pane_has_overlay_enabled(&crate::radar_layer::POLL_LAYER),
        "precondition: the radar layer stays shown, so `viewing_live` is the \
         only thing these fixtures vary"
    );
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
/// The two halves are read from opposite ends on purpose: the *firing* half is
/// what `check_auto_polls` actually emits, not a second reading of the same
/// delay the wake is computed from. A pin that asked `auto_fetch_delay`
/// whether `auto_fetch_delay` was zero could not fail.
#[test]
fn the_wake_lands_exactly_when_the_poll_would_fire() {
    for (ago, due) in [
        (std::time::Duration::from_millis(59_500), false),
        (INTERVAL, true),
        (INTERVAL + std::time::Duration::from_secs(30), true),
    ] {
        let mut gui = Gui::new();
        only_the_radar_poll(&mut gui);
        polled(&mut gui, ago);

        let mut actions = Vec::new();
        gui.check_auto_polls(&mut actions);
        let fired = actions
            .iter()
            .any(|a| matches!(a, crate::actions::GuiAction::CheckForNewScans(_)));
        assert_eq!(
            fired, due,
            "the premise moved: {ago:?} into a {INTERVAL:?} interval"
        );

        let mut gui = Gui::new();
        only_the_radar_poll(&mut gui);
        polled(&mut gui, ago);
        assert_eq!(
            gui.auto_poll_delay().expect("a timer is running").is_zero(),
            due,
            "at {ago:?} the schedule and the poll disagree about whether a \
             round is due, so a wake will be spent on a frame that polls \
             nothing"
        );
    }
}

/// A poll that cannot fire must not be scheduled for, however overdue its
/// timer reads.
#[test]
fn a_poll_no_pane_can_use_is_not_scheduled_for() {
    let mut gui = Gui::new();
    only_the_radar_poll(&mut gui);
    polled(&mut gui, INTERVAL * 4);
    gui.apply(crate::shell_api::GuiEvent::ViewingLiveForPane {
        pane_idx: 0,
        live: false,
    });

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

/// **An archive check is answered only when there is something newer**
/// (`fetch_latest_if_newer` sends nothing back otherwise), so the clock this
/// layer counts from is stamped by the ASK. A clock that waited for a
/// delivery would leave the layer reading "never fetched" on the next frame,
/// and the archive would be asked once per frame for ever.
#[test]
fn a_check_that_is_never_answered_still_spends_its_interval() {
    let mut gui = Gui::new();
    only_the_radar_poll(&mut gui);
    polled(&mut gui, INTERVAL);

    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, crate::actions::GuiAction::CheckForNewScans(_))),
        "precondition: the round was due and the check was asked for"
    );

    // Nothing arrives. No `ScanInfoForSite`, no `Error`, no `Fetching(false)`.
    let mut again = Vec::new();
    gui.check_auto_polls(&mut again);
    assert!(
        !again
            .iter()
            .any(|a| matches!(a, crate::actions::GuiAction::CheckForNewScans(_))),
        "an unanswered check left the layer due on the very next frame, so \
         the archive is being asked once per frame"
    );
    let delay = gui
        .auto_poll_delay()
        .expect("the poll is still owed its next round");
    assert!(
        delay > INTERVAL - SLACK,
        "the unanswered check did not spend its interval: {delay:?}"
    );
}

/// **Auto-poll off means stop checking for newer volumes; it has never meant
/// show nothing at all.** The session's first fetch is not on the poll's gate,
/// and folding it onto one would leave a user who turned the switch off with
/// an empty map for the whole session.
#[test]
fn the_first_fetch_of_a_session_happens_with_the_poll_switched_off() {
    let mut gui = Gui::new();
    only_the_radar_poll(&mut gui);
    gui.set_auto_poll_enabled(false);
    assert_eq!(
        gui.auto_poll_delay(),
        None,
        "precondition: with the switch off nothing is scheduled for"
    );

    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, crate::actions::GuiAction::FetchRadarScan(_))),
        "switching the archive poll off left the session with no radar at all"
    );
}

/// Turning auto-poll off has to stop the wake as well as the poll.
#[test]
fn auto_poll_switched_off_asks_for_nothing() {
    let mut gui = Gui::new();
    only_the_radar_poll(&mut gui);
    polled(&mut gui, std::time::Duration::from_secs(5));
    gui.set_auto_poll_enabled(false);

    assert_eq!(gui.auto_poll_delay(), None);
    assert_eq!(
        gui.status_tick_delay(),
        None,
        "there is no countdown on screen to advance either"
    );
}

/// A fetch in flight suppresses the poll (`check_auto_polls` refuses while
/// the layer's own in-flight flag), so it must suppress the wake too.
#[test]
fn a_fetch_in_flight_yields_the_wake_to_whatever_ends_it() {
    let mut gui = Gui::new();
    only_the_radar_poll(&mut gui);
    polled(&mut gui, INTERVAL * 2);
    gui.apply(crate::shell_api::GuiEvent::Fetching(true));

    assert_eq!(gui.auto_poll_delay(), None);
}

/// An overlay's refresh is on the same terms as the radar poll: scheduled
/// while some pane on screen can draw it, and not otherwise.
#[test]
fn an_overlay_is_scheduled_for_only_while_a_pane_can_draw_it() {
    let kind = squallar_source::id::known::NWS_ALERTS;
    let mut gui = Gui::new();
    let interval = gui
        .overlays
        .auto_poll_interval(&kind)
        .expect("NWS alerts auto-poll; this test needs a layer that does");

    gui.pane_mut(0)
        .unwrap()
        .set_overlay_enabled(kind.clone(), false);
    assert_eq!(gui.overlay_poll_delay(&kind), None);

    gui.pane_mut(0)
        .unwrap()
        .set_overlay_enabled(kind.clone(), true);
    assert_eq!(
        gui.overlay_poll_delay(&kind),
        Some(std::time::Duration::ZERO),
        "a layer that has never been fetched is due now"
    );

    gui.overlays.apply_fetch_result(
        squallar_overlays::render::overlay_state::OverlayFetchResult {
            kind: kind.clone(),
            data: OverlayRegistry::nws_alerts_payload(Vec::new()),
        },
        &PaneRef::bare(0),
    );
    let delay = gui.overlay_poll_delay(&kind).expect("still owed");
    let interval = std::time::Duration::from_secs(interval);
    assert!(
        delay > interval - SLACK && delay <= interval,
        "a layer fetched just now must be scheduled a whole interval out, \
         not {delay:?}"
    );

    gui.overlays.set_fetching(&kind, true, &PaneRef::bare(0));
    assert_eq!(
        gui.overlay_poll_delay(&kind),
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
        chip(&gui).secs(),
        Some(50),
        "precondition: the bar is printing `archive 50s`"
    );

    let tick = chip(&gui)
        .countdown_tick()
        .expect("the count is still moving");
    assert!(
        tick > std::time::Duration::from_millis(500)
            && tick <= std::time::Duration::from_millis(700),
        "the tick is not the remainder of this second: {tick:?}"
    );
}

/// …and stops asking once the number has stopped moving. `time_until_next`
/// saturates at zero, so a poll that cannot fire leaves a string that would
/// otherwise be repainted once a second for ever.
#[test]
fn a_countdown_that_has_bottomed_out_asks_for_no_more_frames() {
    let mut gui = Gui::new();
    polled(&mut gui, INTERVAL * 3);
    assert_eq!(
        chip(&gui).secs(),
        Some(0),
        "precondition: the count has bottomed out"
    );

    assert_eq!(chip(&gui).countdown_tick(), None);
}

/// The tick is never zero, whatever the phase of the clock. A zero-length
/// sleep re-armed every iteration is the spin this path exists to avoid.
#[test]
fn the_countdown_tick_is_never_a_zero_length_sleep() {
    for millis in [0, 1, 999, 1_000, 1_001, 30_000, 59_999] {
        let mut gui = Gui::new();
        polled(&mut gui, std::time::Duration::from_millis(millis));
        let tick = chip(&gui)
            .countdown_tick()
            .expect("the count is still moving");
        assert!(
            !tick.is_zero() && tick <= std::time::Duration::from_secs(1),
            "at {millis}ms in, the countdown asked for a {tick:?} sleep"
        );
    }
}

/// A status bar nobody is looking at costs nothing. The tick is what the bar
/// itself recorded while drawing, so an app that has drawn no bar is owed none.
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
