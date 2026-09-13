//! **The two widgets that clear a pane's live flag without moving its clock**
//! — Back, before the navigation it emits lands, and the Set Time dialog's OK,
//! which fetches a volume and moves nothing — **leave a clock the rule declared
//! dead where it was: dead.**
//!
//! Before the flag had one writer, each cleared the field directly, and a pane
//! depicting now over a stored clock no loop owned — what an older config or a
//! stopped loop leaves — went straight back to that instant, with every layer
//! with a past asked about it (review of the 2026-09-12 fix). Both are driven
//! through their real widgets here.

use super::*;
use crate::actions::GuiAction;
use crate::pane::TimeMode;
use squallar_source::id::known;

fn dead_instant() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
        .and_then(|d| d.and_hms_opt(18, 0, 0))
        .expect("a real instant")
}

/// Pane 0 depicting now over a stored clock no loop owns.
fn over_a_dead_clock(h: &mut InputHarness) {
    let pane = h.gui_mut().pane_mut(0).expect("pane 0");
    pane.set_time_mode(TimeMode::AsOf(dead_instant()));
    assert!(pane.viewing_live(), "premise: the pane follows live");
    assert_eq!(
        pane.time_mode(),
        TimeMode::Live,
        "premise: and it depicts now"
    );
}

fn assert_still_dead(h: &mut InputHarness, widget: &str) {
    let pane = h.gui_mut().pane(0).expect("pane 0");
    assert!(
        !pane.viewing_live(),
        "premise: {widget} took the pane off live"
    );
    assert_eq!(
        pane.time_mode(),
        TimeMode::Live,
        "{widget} put the pane back on {} — a clock the rule had declared dead",
        dead_instant(),
    );
    assert_eq!(
        pane.view(0).layer(&known::NWS_ALERTS).as_of,
        None,
        "{widget}: and every layer with a past was asked about it again",
    );
}

#[test]
fn back_clears_the_live_flag_without_bringing_back_a_dead_clock() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    over_a_dead_clock(&mut h);
    h.warm_up();

    h.mouse_click(h.timeline().back.center());
    assert!(
        h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::NavigateTime { pane_idx: 0, .. })),
        "premise: Back asked to step pane 0"
    );
    assert_still_dead(&mut h, "Back");
}

#[test]
fn the_set_time_dialogs_ok_clears_the_live_flag_without_bringing_back_a_dead_clock() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
    h.load_scan("KTLX");
    over_a_dead_clock(&mut h);
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.warm_up();
    assert!(
        h.text_painted_in(h.screen_rect(), "Select Time"),
        "premise: the dialog is on screen"
    );

    let ok = h
        .painted_text_rects()
        .into_iter()
        .find(|(_, text)| text == "OK")
        .expect("the time dialog draws an OK button")
        .0;
    h.mouse_click(ok.center());
    assert!(
        h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::FetchRadarScan(_))),
        "premise: OK asked for a volume"
    );
    assert_still_dead(&mut h, "the Set Time dialog's OK");
}

/// **The archive rail's release clears the flag and names its own instant —
/// never the dead clock under it.**
///
/// The third widget that writes the flag. It clears it and then writes the
/// released instant in the same gesture, so the setter's write-down is
/// overwritten at once; this pins that the pane ends on the instant the hand
/// let go at, and that a layer handler is told that instant, whatever dead
/// clock the pane was carrying when the drag began.
#[test]
fn the_archive_rails_release_names_its_own_instant_not_a_dead_clock() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    over_a_dead_clock(&mut h);
    h.warm_up();

    let scrub = h.timeline().scrubber;
    assert!(scrub.is_positive(), "premise: the archive rail is drawn");
    let from = scrub.center();
    let to = from + egui::vec2(-30.0, 0.0);
    h.mouse_move(from);
    h.frame();
    h.mouse_press(from);
    h.frame();
    h.mouse_move(to);
    h.frame();
    h.mouse_release(to);
    h.frame();

    let now = chrono::Utc::now().naive_utc();
    let pane = h.gui_mut().pane(0).expect("pane 0");
    assert!(
        !pane.viewing_live(),
        "premise: releasing mid-rail took the pane off live"
    );
    let Some(released) = pane.time_mode().as_of() else {
        panic!("premise: releasing mid-rail parks the pane on an instant");
    };
    assert!(
        released > dead_instant() && released <= now,
        "the rail's release left the pane on {released}; the dead clock under it \
         was {} and the rail names instants inside its own window before now",
        dead_instant(),
    );
    assert_eq!(
        pane.view(0).layer(&known::NWS_ALERTS).as_of,
        Some(released),
        "and a layer handler must be told the instant the rail named",
    );
}
