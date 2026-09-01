//! **A BasemapTiles toggle must not rebuild the base source — asserted through
//! `Gui::ui`, where the stall is.**
//!
//! `MapTileState`'s own suite (`tiles::tests`) already pins the park, but it
//! pins it by calling `release_base_tiles` and `ensure_base_tiles` itself. What
//! routes a *frame* to those two methods is the `basemap_on` branch in this
//! module, and nothing asserted that branch. Point the else-arm somewhere else
//! — `self.map_tiles.tiles = None`, a `clear()`, a `tiles_owned` dropped rather
//! than restored — and every pin in `tiles::tests` stays green while the frame
//! thread pays for a source rebuild again.
//!
//! **A count, not a clock.** `base_builds` moves once per constructed source,
//! so a rebuilt source is `2` and a parked one is `1`. The count is not on its
//! own sufficient: a release that emptied the slot and an `ensure_` that then
//! built *nothing* — a latched `base_unreachable` — also reads `1`. That is why
//! each leg asserts the slot's occupancy beside the count; the pair is what
//! discriminates, and neither half alone does. The wall-clock cost this stands
//! in for is deliberately not timed here — a ratio assertion red-gates on a
//! loaded box, and this workspace counts the operation instead.
//!
//! The harness is **offline**, so these are `HttpsTiles::inert` sources with no
//! IO thread. That costs this fixture the *drop-join* half of the defect, which
//! only a live source can show and which `tiles::tests`'
//! `a_layer_toggle_parks_the_base_source_rather_than_joining_its_io_thread`
//! holds. It keeps the half this one is for: `base_builds` counts an inert
//! build exactly as it counts a live one, because `ensure_base_tiles` bumps it
//! above the branch that chooses between them.

use crate::input_harness::InputHarness;
use squallar_source::id::known;

const FRAME_DT: f64 = 1.0 / 60.0;

fn base_builds(h: &InputHarness) -> usize {
    h.gui().map_tiles.base_builds
}

/// Switch the BasemapTiles layer off and back on through real frames: the
/// source that comes back is the one that went away.
#[test]
fn a_basemap_toggle_through_the_frame_path_does_not_rebuild_the_source() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        base_builds(&h),
        1,
        "fixture: the first frames did not build a base source, so the layer \
         was never on and the toggle below proves nothing"
    );

    h.set_overlay_on_pane(0, &known::BASEMAP_TILES, false);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.gui().map_tiles.tiles.is_none(),
        "a switched-off layer must draw nothing: a source left in the slot is \
         a source the pane loop still hands to walkers"
    );
    assert_eq!(
        base_builds(&h),
        1,
        "the frames the layer spent switched OFF built a source"
    );

    h.set_overlay_on_pane(0, &known::BASEMAP_TILES, true);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.gui().map_tiles.tiles.is_some(),
        "the layer came back with an empty slot, so it draws nothing"
    );
    assert_eq!(
        base_builds(&h),
        1,
        "the frame that brought the layer back constructed a second source, \
         so the release let the first one go instead of parking it — the \
         parsed-geometry cache went with it and this frame re-tessellates \
         every visible tile on the frame thread"
    );
}
