//! A theme flip must move the overlay cache token — the live bug this pins.
//!
//! # The bug
//!
//! Overlay handlers rasterize *in* the theme: the SPC outlook branches on
//! `RasterizeContext::is_dark` for its hatch colour
//! (`rustdar-overlays/src/render/handlers/outlook.rs`), the GLM and
//! storm-report handlers carry `is_dark` into their described jobs, and the
//! METAR station model picks its text colour by it
//! (`rustdar-overlays/src/render/station_model.rs`). Those sites were always
//! correct — at raster time. What was broken was invalidation: on a theme
//! flip `App::adopt_theme` bumped only `radar_sites_render_gen`, and
//! `overlay_cache_token` answered the bare content signature for every other
//! layer, so `needs_rerender` compared equal and the pane kept compositing
//! rasters baked in the *old* theme's colours — dark-grey hatches on a dark
//! map — until the next content change happened to move the signature.
//!
//! # The fix these tests hold in place
//!
//! The token mixes in the theme (a fixed odd 64-bit XOR, uniform on both
//! arms), and the caller reads the live `dark_mode` off the egui context
//! every frame. The SPC outlook is the layer under test because its raster
//! is theme-dependent in this exact way — the hatch-colour branch is the
//! pixels that go stale.
//!
//! The frontend's `app::theme_flip_tests` holds the other half: a flip must
//! *not* touch the radar's theme-independent `RenderCache`.

use super::*;
use crate::ui::Gui;
use rustdar_overlays::render::handlers::outlook::SpcOutlookFetchResult;
use rustdar_overlays::render::overlay_state::OverlayFetchResult;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use rustdar_overlays::types::{HatchPattern, OverlayFeature};

/// The theme-dependent layer under test — see the module note for why this
/// one.
const OUTLOOK: OverlayKind = OverlayKind::SpcOutlook;

/// The pane-keyed arm, for the uniformity test.
const SITES: OverlayKind = OverlayKind::RadarSites;

/// A `Gui` whose registry holds a real SPC outlook, fed through
/// `apply_fetch_result` — the same door a live fetch uses, the
/// `ui::overlay_retry_tests` way — so the content signature under test is the
/// one a live session would carry.
///
/// The outlook's "enabled" *is* its product set and its data is keyed by
/// `(day, product)`, so the toggle has to precede the payload for the two to
/// meet.
fn gui_with_an_outlook() -> Gui {
    let mut gui = Gui::new();
    gui.overlays.set_enabled(OUTLOOK, true);

    let polygon = vec![vec![(35.0, -97.0), (36.0, -97.0), (36.0, -96.0)]];
    let feature = OverlayFeature::new(
        vec![polygon],
        [255, 0, 0, 128],
        [0, 0, 0, 255],
        "SLGT".into(),
        String::new(),
        HatchPattern::None,
    );
    gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: OUTLOOK.id(),
        data: Box::new(SpcOutlookFetchResult {
            day: OutlookDay::Day1,
            product: OutlookProduct::Categorical,
            result: Ok(SpcOutlook {
                day: OutlookDay::Day1,
                product: OutlookProduct::Categorical,
                valid: None,
                expire: None,
                features: vec![feature],
            }),
        }),
    });
    assert!(
        gui.overlays.has_data(OUTLOOK),
        "premise: the outlook landed, so the signature under test is a live one",
    );
    gui
}

/// **The regression test.** The same outlook content keys different tokens in
/// different themes — the move that buys the re-rasterize a flip is owed —
/// and the token is a pure function of its inputs, so nothing *else* moves
/// it: equal inputs answer equal tokens, in both themes.
///
/// Before the fix the two tokens were equal and `needs_rerender` kept the
/// stale-theme raster on screen indefinitely.
#[test]
fn a_theme_flip_changes_the_overlay_cache_token() {
    let gui = gui_with_an_outlook();
    let pane = gui.pane(0).expect("a fresh Gui has one pane");

    let dark = super::overlay_cache_token(&gui.overlays, pane, OUTLOOK, true);
    let light = super::overlay_cache_token(&gui.overlays, pane, OUTLOOK, false);
    assert_ne!(
        dark, light,
        "one outlook, two themes, one token — the cache would keep compositing \
         the old theme's hatch colours after a flip",
    );

    // Equal inputs answer equal tokens: the theme term is deterministic
    // arithmetic, not a counter — a token that drifted between frames would
    // re-rasterize every overlay every frame.
    assert_eq!(
        dark,
        super::overlay_cache_token(&gui.overlays, pane, OUTLOOK, true),
        "the dark-theme token must be stable across frames with unchanged content",
    );
    assert_eq!(
        light,
        super::overlay_cache_token(&gui.overlays, pane, OUTLOOK, false),
        "the light-theme token must be stable across frames with unchanged content",
    );
}

/// The theme term is uniform: the radar-sites arm — keyed on the pane's own
/// counter rather than a content signature — moves with the theme too.
///
/// Redundant with `adopt_theme`'s gen bump today, and deliberately so: one
/// rule for every arm is what E5's handler-declared form replaces, and until
/// then a special case is a place for the bug to grow back.
#[test]
fn the_theme_term_applies_to_the_radar_sites_arm_too() {
    let gui = Gui::new();
    let pane = gui.pane(0).expect("a fresh Gui has one pane");

    assert_ne!(
        super::overlay_cache_token(&gui.overlays, pane, SITES, true),
        super::overlay_cache_token(&gui.overlays, pane, SITES, false),
        "the radar-sites arm must carry the same theme term as every other layer",
    );
}
