//! A theme flip must move the overlay cache token — the live bug this pins.

use crate::ui::Gui;
use squallar_overlays::render::handlers::outlook::SpcOutlookFetchResult;
use squallar_overlays::render::overlay_state::OverlayFetchResult;
use squallar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use squallar_overlays::types::{HatchPattern, OverlayFeature};
use squallar_source::handler::{PaneMut, PaneRef};

/// The theme-dependent layer under test — see the module note for why this
/// one.
const OUTLOOK: squallar_source::id::LayerId = squallar_source::id::known::SPC_OUTLOOK;

/// The pane-keyed arm, for the uniformity test.
const SITES: squallar_source::id::LayerId = squallar_source::id::known::RADAR_SITES;

/// A `Gui` whose registry holds a real SPC outlook, fed through
/// `apply_fetch_result` — the same door a live fetch uses, the
/// `ui::overlay_retry_tests` way — so the content signature under test is the
/// one a live session would carry.
fn gui_with_an_outlook() -> Gui {
    let mut gui = Gui::new();
    gui.overlays
        .set_enabled(&OUTLOOK, true, &mut PaneMut::bare(0));

    let polygon = vec![vec![(35.0, -97.0), (36.0, -97.0), (36.0, -96.0)]];
    let feature = OverlayFeature::new(
        vec![polygon],
        [255, 0, 0, 128],
        [0, 0, 0, 255],
        "SLGT".into(),
        String::new(),
        HatchPattern::None,
    );
    gui.overlays.apply_fetch_result(
        OverlayFetchResult {
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
        },
        &PaneRef::bare(0),
    );
    assert!(
        gui.overlays.has_data(&OUTLOOK, &PaneRef::bare(0)),
        "premise: the outlook landed, so the signature under test is a live one",
    );
    gui
}

/// **The regression test.** The same outlook content keys different tokens in
/// different themes — the move that buys the re-rasterize a flip is owed —
/// and the token is a pure function of its inputs, so nothing *else* moves
/// it: equal inputs answer equal tokens, in both themes.
#[test]
fn a_theme_flip_changes_the_overlay_cache_token() {
    let gui = gui_with_an_outlook();
    let pane = gui.pane(0).expect("a fresh Gui has one pane");

    let dark = super::overlay_cache_token(&gui.overlays, 0, pane, &OUTLOOK, true);
    let light = super::overlay_cache_token(&gui.overlays, 0, pane, &OUTLOOK, false);
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
        super::overlay_cache_token(&gui.overlays, 0, pane, &OUTLOOK, true),
        "the dark-theme token must be stable across frames with unchanged content",
    );
    assert_eq!(
        light,
        super::overlay_cache_token(&gui.overlays, 0, pane, &OUTLOOK, false),
        "the light-theme token must be stable across frames with unchanged content",
    );
}

/// **The registry walk.** A theme flip moves the cache token of exactly the
/// layers that declared themselves theme-sensitive, and of no others.
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
        let dark = super::overlay_cache_token(&gui.overlays, 0, pane, &id, true);
        let light = super::overlay_cache_token(&gui.overlays, 0, pane, &id, false);
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
        super::overlay_cache_token(&gui.overlays, 0, pane, &SITES, true),
        super::overlay_cache_token(&gui.overlays, 0, pane, &SITES, false),
        "with the generation unchanged, the declaration alone must still move \
         the radar-sites token",
    );
    // The other half is still there: the generation moves the token too.
    let mut bumped = Gui::new();
    bumped.bump_all_radar_sites_gen();
    let after = bumped.pane(0).expect("a fresh Gui has one pane");
    assert_ne!(
        super::overlay_cache_token(&gui.overlays, 0, pane, &SITES, true),
        super::overlay_cache_token(&bumped.overlays, 0, after, &SITES, true),
        "the generation bump must still move the token on its own",
    );
}
