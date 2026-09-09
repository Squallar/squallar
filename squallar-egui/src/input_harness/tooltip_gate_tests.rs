//! **A tooltip is asked for only by the widget the pointer is on.**
//!
//! `Response::on_hover_text` and its two siblings do not check first: each
//! builds a `Tooltip` — a `Popup` carrying the response's layer, id, anchor,
//! gap and width — and then runs `should_show_tooltip`, which takes the
//! context's memory lock, its previous-pass state twice, the global style and
//! five reads off the input state before reaching the line that decides it,
//! which for nearly every widget on the glass is `!response.hovered()`. The
//! gate in `crate::ui_hover` moves that decision in front of the machinery.
//!
//! Both directions, for the reason `eager_tooltip_tests` gives: a gate that
//! never admits anything passes the closed half trivially and breaks every
//! tooltip in the app, so the open half is the one that has to be there.

use super::InputHarness;
use crate::ui_hover::admitted;
use squallar_source::id::known;

/// Somewhere no tooltip-bearing widget is, for the closed-frame arm.
fn nowhere(h: &InputHarness) -> egui::Pos2 {
    h.screen_rect().left_top() + egui::vec2(2.0, 2.0)
}

/// A harness whose layer stack carries rows — the panel this gate is worth
/// the most on, because every row draws three tooltip-bearing controls.
fn stacked() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    for id in [&known::RADAR, &known::NWS_ALERTS, &known::METAR] {
        h.gui_mut().enable_overlay_for_test(id);
    }
    h.warm_up();
    h
}

/// **A frame hovering nothing enters egui's tooltip machinery zero times.**
///
/// The premise is the row count: a panel with no rows carries no tooltips and
/// would read zero however this were written.
#[test]
fn a_frame_hovering_nothing_asks_for_no_tooltip() {
    let mut h = stacked();
    let rows = h.stack().rows.len();
    assert!(
        rows >= 3,
        "premise: the layer panel must draw rows for their controls to carry \
         tooltips; it drew {rows}"
    );

    h.mouse_move(nowhere(&h));
    h.frames_for(4, 0.1);
    admitted::reset();
    h.frame();
    let closed = admitted::read();
    assert_eq!(
        closed, 0,
        "a frame hovering nothing asked egui for {closed} tooltip(s) — each \
         one a Popup built out of the response and a should_show_tooltip that \
         takes four context locks to answer no."
    );
}

/// **The widget under the pointer still asks, and still gets its tooltip.**
///
/// The painted-text assertion first: a count of one over a frame that drew no
/// tooltip would be a gate that admits and a tooltip that never shows.
#[test]
fn the_row_control_under_the_pointer_still_raises_its_tooltip() {
    let mut h = stacked();
    let row = h.stack().rows.first().cloned().expect("a stack row");
    assert_ne!(
        row.eye,
        egui::Rect::NOTHING,
        "premise: the row must draw an eye for there to be a hover on it"
    );

    h.mouse_move(nowhere(&h));
    h.frames_for(4, 0.1);
    h.mouse_move(row.eye.center());
    h.frames_for(12, 0.1);

    let painted = h.painted_text_strings();
    assert!(
        painted
            .iter()
            .any(|t| t.starts_with("Hide ") || t.starts_with("Show ")),
        "premise: hovering a row's eye must raise its tooltip; painted: {painted:?}"
    );

    admitted::reset();
    h.frame();
    let open = admitted::read();
    assert_eq!(
        open, 1,
        "the frame with the eye's tooltip up admitted {open} ask(s). One is \
         the widget under the pointer; zero is a gate that shut its own \
         tooltip; more than one is a gate that is not reading the pointer."
    );
}
