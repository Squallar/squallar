//! **A saved preset's overlay list is sized by the layers the user picked, not
//! by the registry it was filtered out of.**
//!
//! [`Gui::capture_preset`] builds that list as
//! `default_draw_order().into_iter().filter(..).collect()`. That takes the
//! standard library's in-place specialisation — `Filter` is `InPlaceIterable`,
//! the source is an owned `Vec<LayerId>` and the element type does not change,
//! so alignment and size both permit it — which means the surviving ids are
//! written over the front of the registry's own buffer and **that buffer
//! becomes the preset's**. Nothing shrinks it, so the capacity stays at one
//! slot per *registered* layer however few the user ticked.
//!
//! The list is retained: `PresetConfig.overlays` goes into `Gui::presets` and
//! into the config file, so the slack is resident for as long as the preset is.

use super::super::Gui;
use squallar_source::id::{LayerId, known};

/// Read off the pinned toolchain, not assumed.
const LAYER_ID_BYTES: usize = size_of::<LayerId>();

/// **Red on `b91cfa238`**: capacity 18 — one slot per registered layer — for
/// the 8 ids the pane had on, 432 B of buffer holding 192 B of ids. The
/// fixture ticks two on top of the layers a fresh `Gui` starts with, so what
/// it asserts about is the gap and not a hand-kept count of either side.
///
/// The source here is `OverlayRegistry::default_draw_order`, which is itself
/// exactly sized (`handlers.iter().map(..).collect()` over a slice), so what
/// this pins is the `filter` conversion and not the registry's own
/// reservation.
#[test]
fn a_captured_presets_overlay_list_is_sized_by_what_it_kept() {
    let mut gui = Gui::new();
    let picked = [known::NWS_ALERTS, known::METAR];
    for id in &picked {
        gui.pane_mut(0)
            .expect("a fresh Gui has one pane")
            .set_overlay_enabled(id.clone(), true);
    }

    let registered = gui.overlays.default_draw_order();
    assert_eq!(
        registered.capacity(),
        registered.len(),
        "the source list must be exactly sized, or this fixture is trying the \
         registry's reservation instead of the conversion",
    );
    let registered = registered.len();

    let preset = gui.capture_preset("one".to_string());
    let kept = &preset.overlays.known;

    assert!(
        kept.len() < registered,
        "the fixture must leave some of the {registered} registered layers out, \
         or a capacity equal to the registry is also the exact answer and the \
         assertion below cannot fail",
    );
    assert_eq!(
        kept.capacity(),
        kept.len(),
        "the preset parks {} slots for {} ids -- {} B of buffer for {} B of \
         content, the registry's own Vec<LayerId> allocation carried forward \
         by an in-place filter over {registered} registered layers",
        kept.capacity(),
        kept.len(),
        kept.capacity() * LAYER_ID_BYTES,
        kept.len() * LAYER_ID_BYTES,
    );
}
