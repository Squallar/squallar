//! **The basemap detail set is read off the control surface only when the
//! layer says its own state moved — and it is still read when it did.**
//!
//! `render_panes`' setup arm has to know which OMT source layers the ground
//! excludes, and the sanctioned way to know is the layer's declared control
//! surface. `SourceHandler::controls` answers in owned `String`s — the
//! BasemapTiles layer declares fifteen source-layer toggles plus two more
//! items — so walking through that door costs the same allocations whether or
//! not the answer changed, and the answer changes only when a control is
//! applied. It was walked on **every frame**, and the set it built was
//! discarded unread by `ensure_base_tiles` on every frame but the ones that
//! followed a click.
//!
//! **A count, not a clock.** `base_detail_reads` moves once per walk, so a
//! settled frame must not move it at all and a toggle must move it by exactly
//! one. The count alone is not sufficient in either direction, which is why
//! each leg pairs it:
//!
//! * a *zero* would also be read off a frame path whose basemap arm never ran
//!   — a layer switched off, an empty pane list — so the still legs assert the
//!   slot is occupied beside the count;
//! * a *one* says the surface was walked, not that the answer landed, so the
//!   toggle legs assert the committed set itself and `base_restyles` beside
//!   it. A memo that read and then dropped the result reads green on the
//!   count and red on the set.
//!
//! The third leg is the one the memo can get wrong and the first two cannot
//! see: a detail toggled while the layer is **off**. No frame reads the
//! surface across that span, so the revision the memo holds is stale by the
//! time the layer comes back, and a memo keyed on anything the off-frames
//! updated would conclude "unchanged" and bring back a source styled for the
//! detail set the user has since left.
//!
//! The harness is **offline**, so the slot holds `HttpsTiles::inert`. That
//! costs these legs nothing they are for: `base_detail_reads` and
//! `base_restyles` are bumped above the arm that chooses between an inert
//! source and a live one.

use crate::input_harness::InputHarness;
use squallar_source::controls::{ControlUpdate, ControlValue};
use squallar_source::id::known;

const FRAME_DT: f64 = 1.0 / 60.0;

/// A source layer that ships ON, so switching it off is a real change against
/// the default set — see `basemap_layer::default_disabled_source_layers`.
const SHIPS_ON: &str = "sl:park";

fn reads(h: &InputHarness) -> usize {
    h.gui().map_tiles.base_detail_reads
}

fn restyles(h: &InputHarness) -> usize {
    h.gui().map_tiles.base_restyles
}

/// Switch one source-layer toggle through the same door the inspector's
/// checkbox uses.
fn toggle_detail(h: &mut InputHarness, on: bool) {
    h.gui_mut().apply_control_on_pane_for_test(
        0,
        &known::BASEMAP_TILES,
        &ControlUpdate {
            id: SHIPS_ON,
            value: ControlValue::Bool(on),
        },
    );
}

/// **Frames on which nobody touched a control walk no control surface.**
#[test]
fn a_settled_frame_does_not_re_read_the_basemap_control_surface() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.frames_for(4, FRAME_DT);
    assert!(
        h.gui().map_tiles.tiles.is_some(),
        "fixture: the base slot is empty, so the setup arm under test never \
         ran and a zero below would say nothing about the memo"
    );
    assert!(
        reads(&h) > 0,
        "fixture: the surface was never walked at all, so the memo cannot be \
         the reason a later count does not move"
    );

    let before = reads(&h);
    let styled_before = restyles(&h);
    h.frames_for(30, FRAME_DT);

    assert!(
        h.gui().map_tiles.tiles.is_some(),
        "the base slot emptied under the still frames, so the arm stopped \
         running and the count below is a false zero"
    );
    assert_eq!(
        reads(&h),
        before,
        "a still frame walked the layer's control surface: fifteen toggle \
         labels allocated, a set built out of them, and the whole answer \
         discarded because nothing had moved"
    );
    assert_eq!(
        restyles(&h),
        styled_before,
        "a still frame re-styled the live source, so the memo is handing over \
         a set that differs from the committed one"
    );
}

/// **A toggle is still read, exactly once, and it lands.**
#[test]
fn a_toggled_detail_is_read_once_and_lands_on_the_live_source() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.frames_for(4, FRAME_DT);
    assert!(
        !h.gui()
            .map_tiles
            .committed_disabled_source_layers()
            .contains("park"),
        "fixture: the source layer under test is already excluded, so \
         switching it off is not a change and the leg proves nothing"
    );
    let before = reads(&h);
    let styled_before = restyles(&h);

    toggle_detail(&mut h, false);
    h.frames_for(2, FRAME_DT);

    assert_eq!(
        reads(&h),
        before + 1,
        "the frames after a toggle either never walked the surface or walked \
         it more than once"
    );
    assert_eq!(
        restyles(&h),
        styled_before + 1,
        "the toggle did not re-style the live source exactly once"
    );
    assert!(
        h.gui()
            .map_tiles
            .committed_disabled_source_layers()
            .contains("park"),
        "the surface was walked and the answer was thrown away: the live \
         style still draws the source layer the user switched off"
    );

    let after = reads(&h);
    h.frames_for(20, FRAME_DT);
    assert_eq!(
        reads(&h),
        after,
        "the frames after the toggle settled kept walking the surface, so \
         the memo re-arms only for one frame"
    );
}

/// **A detail switched while the layer is OFF lands on the source that comes
/// back.** No frame reads the control surface across that span, so a memo
/// that concluded "unchanged" from its own untouched bookkeeping would restore
/// a source styled for a detail set the user has left.
#[test]
fn a_detail_toggled_while_the_layer_is_off_lands_when_it_comes_back() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.frames_for(4, FRAME_DT);

    h.set_overlay_on_pane(0, &known::BASEMAP_TILES, false);
    h.frames_for(4, FRAME_DT);
    let off = reads(&h);

    toggle_detail(&mut h, false);
    h.frames_for(4, FRAME_DT);
    assert_eq!(
        reads(&h),
        off,
        "fixture: a frame walked the surface while the layer was off, so this \
         leg is not exercising the stale-across-the-park case it exists for"
    );

    h.set_overlay_on_pane(0, &known::BASEMAP_TILES, true);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.gui().map_tiles.tiles.is_some(),
        "the layer came back with an empty slot, so there is no style to \
         assert on"
    );
    assert!(
        h.gui()
            .map_tiles
            .committed_disabled_source_layers()
            .contains("park"),
        "the layer came back styled for the detail set the user left while it \
         was off"
    );
}
