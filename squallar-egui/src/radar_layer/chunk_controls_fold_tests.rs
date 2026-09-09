//! **The chunk-feed path reads the radar layer's control surface once.**
//!
//! What is guarded is a *cost*, and the cost leaves no trace in the answer:
//! [`chunk_feed_controls`] and the four single readers it replaced return the
//! same three booleans and the same endpoint whether the surface was built
//! once or four times. So the count is the gate, and
//! [`crate::ui::control_walks`] is what reads it —
//! `#[cfg(test)]`, so nothing ships to carry it.
//!
//! **A count alone is insufficient in both directions**, so every leg here
//! pairs it. A *one* would also be read off a fold that walked once and then
//! answered off a default it never looked up, so each leg asserts the values
//! beside the count. And a *zero* — the surface never reached at all — is
//! held off by the four-walk control: the same counter, over the spelling this
//! replaced, must read four, or it is not counting.

use super::*;
use crate::Gui;
use crate::ui::control_walks;

/// The four reads `App::drive_chunk_feeds` made of this surface per frame,
/// spelled as they were before the fold: the live-chunk switch twice, the
/// notification switch, and the endpoint.
fn the_four_single_reads(gui: &Gui) -> (bool, bool, bool, String) {
    (
        live_chunks_enabled(gui),
        chunk_notifications_enabled(gui),
        live_chunks_enabled(gui),
        notifier_endpoint(gui),
    )
}

fn set(gui: &mut Gui, id: &'static str, on: bool) {
    gui.apply_layer_control(
        &POLL_LAYER,
        &squallar_source::controls::ControlUpdate {
            id,
            value: squallar_source::controls::ControlValue::Bool(on),
        },
    );
}

/// **One walk against the four the shipped path used to take**, with the
/// answers held to the old spelling's on the same `Gui`.
///
/// The four-walk reading is the discriminator, not decoration: it is the same
/// counter over the same door in the same test, so a counter that had stopped
/// counting — or a door that had stopped being walked at all — reads zero
/// there and reddens, and the `1` below cannot be the zero of an arm that
/// never ran.
#[test]
fn the_fold_walks_the_surface_once_where_the_single_reads_walked_four() {
    for (chunks, notify) in [(true, true), (true, false), (false, true), (false, false)] {
        let mut gui = Gui::new();
        set(
            &mut gui,
            squallar_radar::source::LIVE_CHUNKS_CONTROL,
            chunks,
        );
        set(
            &mut gui,
            squallar_radar::source::CHUNK_NOTIFICATIONS_CONTROL,
            notify,
        );

        let (folded, folded_walks) =
            control_walks::during(&POLL_LAYER, || chunk_feed_controls(&gui));
        let (singles, single_walks) =
            control_walks::during(&POLL_LAYER, || the_four_single_reads(&gui));

        assert_eq!(
            single_walks, 4,
            "the four-read spelling no longer walks the surface four times, so \
             the one below is measured against nothing — either the counter \
             stopped counting or the readers stopped reading the door"
        );
        assert_eq!(
            folded_walks, 1,
            "the chunk-feed path builds the radar layer's whole control surface \
             {folded_walks} times for three booleans and a string. \
             `SourceHandler::controls` answers in owned `String`s: each walk is \
             a seven-item `Vec` with nine `String`s and a nested `Vec` in it, \
             plus the slot list `Pane::view` collects to reach it, on a frame \
             pump row that runs every frame"
        );
        assert_eq!(
            (
                folded.live_chunks,
                folded.notifications,
                folded.notifier_endpoint.clone(),
            ),
            (singles.0, singles.1, singles.3),
            "the fold walks once and answers differently from the reads it \
             replaced: chunks={chunks} notifications={notify}"
        );
        assert_eq!(
            (folded.live_chunks, folded.notifications),
            (chunks, notify),
            "neither spelling reports what was written, so their agreeing \
             above is two readers agreeing on the wrong answer"
        );
    }
}

/// **The endpoint comes back resolved, not raw** — and an empty box is the
/// built-in rather than an off switch.
///
/// [`notifier_endpoint`] made two `String`s of the one `controls` had already
/// allocated inside itself; the fold moves that one out. A move that had
/// quietly become something else still passes a count, so what is asserted is
/// the value: the same rule [`notifier_endpoint`] applies, over the same
/// inputs.
#[test]
fn the_folded_endpoint_is_the_resolved_one_and_an_empty_box_is_the_built_in() {
    for raw in [
        "",
        "   ",
        "  wss://example.test/notify  ",
        "wss://example.test/n",
    ] {
        let mut gui = Gui::new();
        gui.apply_layer_control(&POLL_LAYER, &notifier_endpoint_update(raw));

        let (folded, walks) = control_walks::during(&POLL_LAYER, || chunk_feed_controls(&gui));
        assert_eq!(walks, 1, "the fold took {walks} walks for {raw:?}");
        assert_eq!(
            folded.notifier_endpoint,
            notifier_endpoint(&gui),
            "the folded endpoint is not the resolved one for {raw:?}"
        );
        assert!(
            !folded.notifier_endpoint.trim().is_empty(),
            "an empty box resolved to an empty endpoint for {raw:?}, which is \
             not an off switch — it is the built-in"
        );
    }
}

/// **The leg that says why this is a fold and not a memo.**
///
/// `live_chunks` is the *active pane's* answer:
/// `RadarSource::live_chunks_for` reads the pane's own slot config before
/// falling back to the layer's global. So two panes can answer differently
/// with no handler write between them, and
/// `SourceHandler::layer_state_revision` — which its own doc says carries the
/// layer-global state and says nothing about per-pane state — cannot key a
/// memo over this reading. A memo that tried would serve the pane it was
/// warmed on.
///
/// The pin is on the *property* such a memo would break, not on the memo's
/// absence: switching the active pane changes the answer with the layer's own
/// state untouched.
#[test]
fn the_folded_live_chunk_answer_is_the_active_panes_and_not_the_layers() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    // The layer-global answer, which is what a pane with no copy of its own
    // takes.
    set(&mut gui, squallar_radar::source::LIVE_CHUNKS_CONTROL, true);
    // One pane told otherwise, directly on its slot — the half
    // `fan_out_live_chunks` writes.
    gui.pane_mut(1)
        .expect("the second pane exists")
        .set_radar_live_chunks(false);

    gui.set_active_pane_for_test(0);
    let a = chunk_feed_controls(&gui);
    gui.set_active_pane_for_test(1);
    let b = chunk_feed_controls(&gui);

    assert!(
        a.live_chunks,
        "the pane with no copy of its own stopped taking the layer's global, \
         so this fixture no longer stands two different answers side by side"
    );
    assert!(
        !b.live_chunks,
        "the fold answered the layer's global on a pane that carries its own \
         copy. This is the reading a `layer_state_revision` memo cannot see: \
         nothing the handler counts moved between these two calls"
    );
}
