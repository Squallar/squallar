//! **A time link carries the live flag and not the clock, and neither half may
//! put a pane on a clock the rule declared dead.**
//!
//! `Gui::propagate_time_posture` copies `viewing_live` from the active pane to
//! every time-linked pane in its group, and not the clock — each pane keeps its
//! own. So a pane the user scrubbed, whose neighbour then pressed Live, came out
//! flagged live over its old instant (report, 2026-09-12); and once that pane
//! depicted now, the neighbour's next step back carried a *cleared* flag to it
//! and put it straight back on the scrub (review of the fix). Both halves are
//! driven here through the real propagation, beside every other event that
//! writes the flag.
//!
//! **Non-vacuity**: a pane whose link is off keeps its instant; an event that
//! names an instant is depicted at that instant.

use crate::Gui;
use crate::pane::TimeMode;
use crate::shell_api::GuiEvent;
use squallar_source::id::known;

fn scrubbed_to() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
        .and_then(|d| d.and_hms_opt(18, 0, 0))
        .expect("a real instant")
}

/// Two panes, pane 0 scrubbed to [`scrubbed_to`] with its time link as given,
/// pane 1 active.
fn scrubbed_beside_active(linked: bool) -> Gui {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    {
        let scrubbed = gui.pane_mut(0).expect("two panes");
        scrubbed.time_link = linked;
        scrubbed.set_viewing_live(false);
        scrubbed.set_time_mode(TimeMode::AsOf(scrubbed_to()));
    }
    gui.set_active_pane_for_test(1);
    gui
}

/// Pane 1 presses Live, and the frame's propagation runs.
fn live_on_pane_1(gui: &mut Gui) {
    gui.apply(GuiEvent::PaneTimeSelected {
        pane_idx: 1,
        instant: chrono::Utc::now().naive_utc(),
        live: true,
    });
    gui.propagate_pane_sync();
}

#[test]
fn a_linked_neighbours_live_brings_a_scrubbed_pane_to_now() {
    for (linked, expected, why) in [
        (true, TimeMode::Live, "pane 0 is time-linked to pane 1"),
        (
            false,
            TimeMode::AsOf(scrubbed_to()),
            "pane 0's time link is off",
        ),
    ] {
        let mut gui = scrubbed_beside_active(linked);
        live_on_pane_1(&mut gui);

        let pane = gui.pane(0).expect("two panes");
        assert_eq!(
            pane.viewing_live(),
            linked,
            "premise: the link carries the live flag exactly when it is on ({why})"
        );
        assert_eq!(
            pane.time_mode(),
            expected,
            "{why}: once the flag says live the pane must depict now, not the \
             instant it was scrubbed to",
        );
        assert_eq!(
            pane.view(0).layer(&known::NWS_ALERTS).as_of,
            expected.as_of(),
            "{why}: and a layer handler must be told the same instant",
        );
    }
}

/// **The neighbour's next step back must not put pane 0 back on its scrub.**
///
/// Back clears the active pane's flag before its navigation moves that pane's
/// clock, and the link copies the cleared flag — not a clock — onto pane 0,
/// which had been depicting now over the scrub it still stored.
#[test]
fn a_linked_neighbours_step_back_does_not_resurrect_a_scrub_the_live_press_retired() {
    let mut gui = scrubbed_beside_active(true);
    live_on_pane_1(&mut gui);
    assert_eq!(
        gui.pane(0).expect("two panes").time_mode(),
        TimeMode::Live,
        "premise: pane 0 depicts now after its neighbour's Live"
    );

    // What the Back button and the navigation it emits do to pane 1.
    gui.pane_mut(1).expect("two panes").set_viewing_live(false);
    gui.apply(GuiEvent::PaneTimeSelected {
        pane_idx: 1,
        instant: chrono::Utc::now().naive_utc() - chrono::Duration::minutes(10),
        live: false,
    });
    gui.propagate_pane_sync();

    let pane = gui.pane(0).expect("two panes");
    assert!(
        !pane.viewing_live(),
        "premise: the link carried the cleared flag to pane 0"
    );
    assert_eq!(
        pane.time_mode(),
        TimeMode::Live,
        "pane 0 depicted now, and its neighbour's step back put it on the scrub \
         the Live press had retired",
    );
    assert_eq!(
        pane.view(0).layer(&known::NWS_ALERTS).as_of,
        None,
        "and every layer with a past was asked about that scrub again",
    );
}

/// **Every event that writes a pane's flag leaves a dead clock dead** — and one
/// that names an instant is depicted at that instant.
#[test]
fn every_event_that_clears_the_flag_leaves_a_dead_clock_dead() {
    let named = scrubbed_to() + chrono::Duration::hours(3);
    let rows: [(&str, GuiEvent, TimeMode); 2] = [
        (
            "ViewingLiveForPane { live: false }",
            GuiEvent::ViewingLiveForPane {
                pane_idx: 0,
                live: false,
            },
            TimeMode::Live,
        ),
        (
            "PaneTimeSelected { live: false } names its own instant",
            GuiEvent::PaneTimeSelected {
                pane_idx: 0,
                instant: named,
                live: false,
            },
            TimeMode::AsOf(named),
        ),
    ];

    for (why, event, expected) in rows {
        let mut gui = Gui::new();
        {
            let pane = gui.pane_mut(0).expect("pane 0");
            pane.set_time_mode(TimeMode::AsOf(scrubbed_to()));
            assert_eq!(
                pane.time_mode(),
                TimeMode::Live,
                "premise: a live pane over a clock no loop owns depicts now ({why})"
            );
        }
        gui.apply(event);
        let pane = gui.pane(0).expect("pane 0");
        assert!(
            !pane.viewing_live(),
            "premise: the event cleared the flag ({why})"
        );
        assert_eq!(
            pane.time_mode(),
            expected,
            "{why}: the pane must depict {expected:?}, not the dead clock",
        );
    }
}
