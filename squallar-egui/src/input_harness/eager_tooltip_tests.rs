//! **`Response::on_hover_text` takes its text ALREADY built.**
//!
//! It is `on_hover_ui` with the argument evaluated first (`egui/src/response.rs`
//! — the closure is lazy, the argument is not), so a String argument was
//! allocated on every frame whether or not anything was hovering. The three
//! The sites gated here now build inside the `on_hover_ui` body instead.
//!
//! Every gate runs **in both directions**: zero strings on a frame hovering
//! nothing, and one on the frame whose tooltip is up. The open half is what a
//! "never build it at all" implementation fails — the closed half it passes
//! trivially. The open half also asserts the tooltip is really *painted*
//! first: without that the count below it proves nothing.

use super::InputHarness;
use crate::ui::hover_text_count;
use squallar_source::id::known;

/// Somewhere no counted tooltip can be raised, for the closed-frame arm.
fn nowhere(h: &InputHarness) -> egui::Pos2 {
    h.screen_rect().left_top() + egui::vec2(2.0, 2.0)
}

/// **The transport's play button does not build its hover text on a frame
/// nobody is hovering.**
///
/// Two of the three branches are `"Pause"`/`"Play"` `.to_owned()` and the
/// third a `format!` — a String a frame, for as long as the second transport
/// row is open, which is the whole time a user is watching a loop.
///
/// **What this gate does NOT reach**, said plainly rather than left to read as
/// full coverage: it drives the `"Pause"` branch only. The `format!` branch
/// cannot be driven by any hover gate, because the state that selects it is
/// the state the caller disables the button in and `on_hover_ui` is
/// `Tooltip::for_enabled`. All three branches moved inside the closure
/// together, so the cut covers them; the *gate* covers one.
#[test]
fn a_frame_hovering_nothing_builds_no_play_button_tooltip_string() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    {
        let pane = h.gui_mut().pane_mut(0).expect("pane 0");
        *pane.time_state_mut(&known::RADAR) = crate::radar_layer::begin_loop(
            3600,
            squallar_radar::sites::get_radar_site("KTLX").expect("KTLX"),
            squallar_radar::types::RenderView::PlanView,
        );
        pane.time_state_mut(&known::RADAR).phase = crate::pane::LoopPhase::Playing;
        let base = chrono::Utc::now().naive_utc() - chrono::Duration::seconds(3600);
        pane.time_state_mut(&known::RADAR).frames = (0..8)
            .map(|i| crate::pane::LoopFrame {
                timestamp: base + chrono::Duration::seconds(i * 300),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        pane.park_on_frame(&known::RADAR, 7);
    }
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();

    let play = h
        .timeline()
        .row2
        .expect("premise: the expander must open the transport's second row")
        .play;
    assert_ne!(
        play,
        egui::Rect::NOTHING,
        "premise: row 2 must draw a play button for there to be a hover on it"
    );

    h.mouse_move(nowhere(&h));
    h.frames_for(4, 0.1);
    hover_text_count::reset();
    h.frame();
    let closed = hover_text_count::read();
    assert_eq!(
        closed, 0,
        "a frame hovering nothing built {closed} play-button tooltip string(s), \
         each allocated and dropped without being drawn."
    );

    h.mouse_move(play.center());
    h.frames_for(12, 0.1);
    assert!(
        h.painted_text_strings().iter().any(|t| t == "Pause"),
        "premise: hovering the play button of a playing loop must raise its \
         tooltip — without that the count below proves nothing; painted: {:?}",
        h.painted_text_strings()
    );
    hover_text_count::reset();
    h.frame();
    let open = hover_text_count::read();
    assert_eq!(
        open, 1,
        "a frame with the play button's tooltip OPEN built {open} string(s). \
         One is the word it draws; zero is a tooltip with nothing in it."
    );
}

/// **A layer body's opacity slider does not build its hover sentence on a
/// frame nobody is hovering.**
///
/// Two allocations, not one: the layer's display name was owned so the
/// `format!` could interpolate it, and neither was read on a frame with no
/// tooltip up. The inspector is a standing panel, so an expanded body pays
/// this on every frame it is open.
#[test]
fn a_frame_hovering_nothing_builds_no_layer_opacity_tooltip_string() {
    let mut h = InputHarness::new();
    h.open_layer_in_inspector(&known::RADAR);
    h.warm_up();

    let slider = h
        .control_items()
        .into_iter()
        .find(|item| {
            item.handler.as_ref() == Some(&known::RADAR) && item.label == crate::ui::OPACITY_LABEL
        })
        .expect("premise: an open layer body must draw its opacity slider")
        .rect;

    h.mouse_move(nowhere(&h));
    h.frames_for(4, 0.1);
    hover_text_count::reset();
    h.frame();
    let closed = hover_text_count::read();
    assert_eq!(
        closed, 0,
        "a frame hovering nothing built {closed} opacity tooltip string(s), \
         each a `format!` over an owned layer name nobody read."
    );

    h.mouse_move(slider.center());
    h.frames_for(12, 0.1);
    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| t.contains("paints over the layers beneath it")),
        "premise: hovering the opacity slider must raise its tooltip; \
         painted: {:?}",
        h.painted_text_strings()
    );
    hover_text_count::reset();
    h.frame();
    let open = hover_text_count::read();
    assert_eq!(
        open, 1,
        "a frame with the opacity tooltip OPEN built {open} string(s). One is \
         the sentence it draws; zero is an empty tooltip."
    );
}

/// **An open catalogue does not build every tile's hover text every frame.**
///
/// This one is per *tile*, not per frame: the inventory is deliberately the
/// complete registered set, so the cost was one `format!` per registered layer
/// plus one per preset plus one per delete control, on every frame the
/// catalogue was up — for the single tooltip a pointer can be over. Measured
/// on the open catalogue rather than an ordinary frame, which is the scene it
/// is absent from.
#[test]
fn a_frame_hovering_no_catalog_tile_builds_no_tile_tooltip_strings() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();
    h.warm_up();

    let tiles = h.catalog().tiles.len();
    assert!(
        tiles > 1,
        "premise: the cost is per tile, so it is only visible on a catalogue \
         drawing more than one; drew {tiles}"
    );
    let name = h.overlay_display_name(&known::NWS_ALERTS).to_owned();
    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Layers, &name)
        .expect("premise: the catalogue must draw a tile to hover")
        .rect;

    h.mouse_move(nowhere(&h));
    h.frames_for(4, 0.1);
    hover_text_count::reset();
    h.frame();
    let closed = hover_text_count::read();
    assert_eq!(
        closed, 0,
        "a frame hovering no tile built {closed} catalogue tooltip string(s) \
         across {tiles} tile(s), every one of them dropped undrawn."
    );

    h.mouse_move(tile.center());
    h.frames_for(12, 0.1);
    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| *t == format!("{name} is already in this pane - show it")),
        "premise: hovering a catalogue tile must raise its tooltip; \
         painted: {:?}",
        h.painted_text_strings()
    );
    hover_text_count::reset();
    h.frame();
    let open = hover_text_count::read();
    assert_eq!(
        open, 1,
        "a frame with one tile's tooltip OPEN built {open} string(s) across \
         {tiles} tile(s). One is the hovered tile's; more would mean the tiles \
         nobody is over built theirs as well."
    );
}
