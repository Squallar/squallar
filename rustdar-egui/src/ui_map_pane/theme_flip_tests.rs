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
//! The token mixes in the theme (a fixed odd 64-bit XOR) for the layers that
//! **declare** themselves theme-sensitive, and the caller reads the live
//! `dark_mode` off the egui context every frame. The SPC outlook is the layer
//! the first test uses because its raster is theme-dependent in this exact way
//! — the hatch-colour branch is the pixels that go stale.
//!
//! The term was uniform when the fix first landed, which was the right shape
//! for a repair that had to be sure it missed nothing. It is
//! `OverlayHandler::theme_sensitive` now, so which layers a flip invalidates is
//! a set rather than "all of them", and
//! `a_theme_flip_invalidates_exactly_the_declared_layers` is the walk that
//! holds the set to the declaration in both directions. Note the module note
//! above names METAR as a theme-reading layer and it still is — per frame, with
//! no cached raster and no token, which is why it is deliberately **not** in
//! the declared set.
//!
//! The frontend's `app::theme_flip_tests` holds the other half: a flip must
//! *not* touch the radar's theme-independent `RenderCache`.

use crate::ui::Gui;
use rustdar_overlays::render::handlers::outlook::SpcOutlookFetchResult;
use rustdar_overlays::render::overlay_state::OverlayFetchResult;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use rustdar_overlays::types::{HatchPattern, OverlayFeature};

/// The theme-dependent layer under test — see the module note for why this
/// one.
const OUTLOOK: rustdar_source::id::LayerId = rustdar_source::id::known::SPC_OUTLOOK;

/// The pane-keyed arm, for the uniformity test.
const SITES: rustdar_source::id::LayerId = rustdar_source::id::known::RADAR_SITES;

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
    gui.overlays.set_enabled(&OUTLOOK, true);

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
        kind: OUTLOOK,
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
        gui.overlays.has_data(&OUTLOOK),
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

    let dark = super::overlay_cache_token(&gui.overlays, pane, &OUTLOOK, true);
    let light = super::overlay_cache_token(&gui.overlays, pane, &OUTLOOK, false);
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
        super::overlay_cache_token(&gui.overlays, pane, &OUTLOOK, true),
        "the dark-theme token must be stable across frames with unchanged content",
    );
    assert_eq!(
        light,
        super::overlay_cache_token(&gui.overlays, pane, &OUTLOOK, false),
        "the light-theme token must be stable across frames with unchanged content",
    );
}

/// **The registry walk.** A theme flip moves the cache token of exactly the
/// layers that declared themselves theme-sensitive, and of no others.
///
/// # Why a walk and not four assertions
///
/// The term used to be uniform, so "which layers does a flip invalidate" had
/// one answer and needed no test: all of them. Making it a handler declaration
/// makes it a *set*, and a set has two ways to be wrong — a handler that bakes
/// the theme into its pixels and forgot to declare it (stale colours after a
/// flip, indefinitely) and a handler that declares it and does not need it (a
/// full re-rasterize of every enabled overlay on every flip). Naming four ids
/// in a test would catch the first and not the second, because a fifth
/// handler's `true` would sit outside the assertion entirely.
///
/// So the walk asks every registered handler both questions and requires them
/// to agree: token moves ⟺ `theme_sensitive()`. The expected set is written
/// down as well, because the walk alone would still pass if every handler
/// silently flipped to `false`.
#[test]
fn a_theme_flip_invalidates_exactly_the_declared_layers() {
    let gui = gui_with_an_outlook();
    let pane = gui.pane(0).expect("a fresh Gui has one pane");

    let mut moved: Vec<String> = Vec::new();
    let mut declared: Vec<String> = Vec::new();
    let mut walked = 0usize;

    for handler in gui.overlays.handlers() {
        let id = handler.id();
        walked += 1;
        let dark = super::overlay_cache_token(&gui.overlays, pane, &id, true);
        let light = super::overlay_cache_token(&gui.overlays, pane, &id, false);
        if dark != light {
            moved.push(id.as_str().to_owned());
        }
        if handler.theme_sensitive() {
            declared.push(id.as_str().to_owned());
        }
    }
    moved.sort();
    declared.sort();

    assert!(
        walked >= 11,
        "only {walked} handlers walked — the registry is not the live one",
    );
    assert_eq!(
        moved, declared,
        "a theme flip must move the token of exactly the handlers that declare \
         `theme_sensitive()`; it moved {moved:?} against a declared set of \
         {declared:?}",
    );

    // And the declared set is the one that was reasoned about. `Metar` is
    // absent on purpose — it is `PerFramePoint`, holds no cached raster and
    // never reaches this function; see `OverlayHandler::theme_sensitive`.
    assert_eq!(
        declared,
        vec![
            "Lightning".to_owned(),
            "RadarSites".to_owned(),
            "SpcOutlook".to_owned(),
            "StormReports".to_owned(),
        ],
        "the theme-sensitive set moved without the reasoning moving with it",
    );

    // Non-vacuity on the walk itself: some handler had to answer `false`, or
    // the two lists agreeing would only mean "everything, as before".
    assert!(
        declared.len() < walked,
        "every one of the {walked} handlers declared itself theme-sensitive, so \
         the declaration is not distinguishing anything",
    );
}

/// The radar-sites arm keeps **both** of its invalidations.
///
/// Its base is the pane's own `radar_sites_render_gen`, which `adopt_theme`
/// bumps on a flip, and it also declares `theme_sensitive`. Either alone would
/// invalidate the raster today. The pin is that the *declaration* is one of
/// them: the gen bump is scheduled to move, and a token that had quietly become
/// single-sourced on it would lose its theme invalidation at that moment with
/// nothing failing to say so.
#[test]
fn the_radar_sites_arm_is_invalidated_by_the_declaration_as_well_as_the_gen() {
    let gui = Gui::new();
    let pane = gui.pane(0).expect("a fresh Gui has one pane");

    // The declaration, read from the handler.
    assert!(
        gui.overlays.theme_sensitive(&SITES),
        "RadarSites bakes `is_dark` into its label plates and must declare it",
    );
    // The token, at one fixed generation — i.e. with the gen bump held still,
    // which is exactly the world the pane-ref work leaves behind.
    assert_ne!(
        super::overlay_cache_token(&gui.overlays, pane, &SITES, true),
        super::overlay_cache_token(&gui.overlays, pane, &SITES, false),
        "with the generation unchanged, the declaration alone must still move \
         the radar-sites token",
    );
    // The other half is still there: the generation moves the token too.
    let mut bumped = Gui::new();
    bumped.bump_all_radar_sites_gen();
    let after = bumped.pane(0).expect("a fresh Gui has one pane");
    assert_ne!(
        super::overlay_cache_token(&gui.overlays, pane, &SITES, true),
        super::overlay_cache_token(&bumped.overlays, after, &SITES, true),
        "the generation bump must still move the token on its own",
    );
}
