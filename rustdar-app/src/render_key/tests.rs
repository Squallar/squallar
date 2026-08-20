//! What the typed key promises: no theme term on radar, an elevation part
//! exactly where the tilt selects the picture, and one bucket per tenth.

use super::*;
use rustdar_overlays::render::overlay_state::OverlayRegistry;
use std::collections::HashMap;

fn plan_key(site: &str, elevation: f32) -> RenderKey {
    render_cache_key(
        site,
        RadarProduct::Reflectivity,
        RenderView::PlanView,
        elevation,
    )
}

/// Radar's key is byte-identical under either theme, and the constant saying so
/// is checked against the live declaration rather than trusted.
///
/// m11: a `RenderCache` entry is 32 MiB at the base side and 128 MiB at the
/// long-range one. A theme term on the radar key means an OS theme flip misses
/// every resident entry and re-decodes and re-renders every visible product —
/// for a change that cannot alter one of their pixels, because a radar picture's
/// palette is the product's and not the interface's.
///
/// The sibling half is `app::theme_flip_tests::
/// a_theme_flip_never_touches_the_radar_render_cache`, which flips a real `App`
/// and watches the cache. This one pins the *key* that makes that survival
/// structural.
#[test]
fn the_radar_key_is_the_same_in_dark_and_light() {
    let overlays = OverlayRegistry::default();

    // Non-vacuity control FIRST: the reader must be able to answer `true`, or
    // every assertion below passes on a registry that lost its handlers.
    assert!(
        overlays.theme_sensitive(&known::RADAR_SITES),
        "control: the site-label layer bakes `is_dark` into its raster and must \
         declare itself theme-sensitive — a registry that answers `false` here \
         cannot distinguish radar's `false` from a dead lookup",
    );

    let declared = overlays.theme_sensitive(&known::RADAR);
    assert!(
        !declared,
        "the radar layer now declares itself theme-sensitive; the key still \
         leaves the theme part out, so a flip would keep serving the old \
         theme's pixels",
    );
    assert_eq!(
        RADAR_THEME_SENSITIVE, declared,
        "RADAR_THEME_SENSITIVE has drifted from `OverlayHandler::theme_sensitive` \
         for `known::RADAR` — the constant is a copy of the declaration and this \
         is the assertion that keeps it one",
    );

    // The formula itself, driven with the live declaration and both readings.
    assert_eq!(
        SelectKey::theme_part(declared, true),
        SelectKey::theme_part(declared, false),
        "the theme part moved with the theme for a layer that declares its \
         raster theme-independent",
    );
    assert_eq!(
        SelectKey::theme_part(declared, true),
        None,
        "a layer declaring `theme_sensitive() == false` acquired a theme term",
    );
    // ...and the control's declaration proves the formula is not simply
    // constant: a `true` declaration must carry the reading through.
    assert_eq!(
        SelectKey::theme_part(true, true),
        Some(true),
        "control: a theme-sensitive layer's part must carry the reading, or \
         `theme_part` returning `None` unconditionally would pass every \
         assertion above",
    );

    // And the key a render is actually filed under.
    let key = plan_key("KTLX", 0.5);
    assert_eq!(
        key.select.theme, None,
        "the radar key carries a theme part — up to 128 MiB of theme-independent \
         pixels are now evicted by an OS theme flip",
    );
    assert_eq!(key.kind, known::RADAR, "the radar key names another layer");
}

/// The elevation part is present exactly when the tilt is what picks the
/// picture, over the whole `(view, product)` grid.
///
/// A section cuts across every tilt and a voxel grid resamples all of them, so
/// keying those by tilt would store one picture once per tilt the pane's
/// selector visits and evict the plan views to do it. The arbiter is
/// [`RenderView::elevation_selects_picture`] and nothing here restates it.
#[test]
fn the_elevation_part_is_present_exactly_when_the_tilt_selects_the_picture() {
    let mut present = 0usize;
    let mut absent = 0usize;

    for &view in RenderView::all() {
        for &product in RadarProduct::all() {
            let key = render_cache_key("KTLX", product, view, 1.5);
            let expected = view.elevation_selects_picture(product);
            assert_eq!(
                key.select.elevation_tenths.is_some(),
                expected,
                "{view:?}/{product:?}: elevation part present={} but \
                 elevation_selects_picture={expected}",
                key.select.elevation_tenths.is_some(),
            );
            if expected {
                assert_eq!(
                    key.select.elevation_tenths,
                    Some(15),
                    "{view:?}/{product:?}: 1.5° must bucket to 15 tenths",
                );
                present += 1;
            } else {
                absent += 1;
            }
        }
    }

    // Non-triviality floor: a grid that had drifted to all-present or
    // all-absent would satisfy every assertion above while pinning nothing.
    assert!(
        present > 0 && absent > 0,
        "the (view, product) grid no longer contains both answers \
         ({present} present, {absent} absent) — this test would pass on a \
         constant",
    );
}

/// One key per tenth of a degree: two selections inside a bucket are the same
/// render, one bucket apart are two.
///
/// Exercised through a `HashMap`, because that is what the cache is — equality
/// and hashing have to agree, and a key that compared equal while hashing apart
/// would miss its own entry.
#[test]
fn selections_in_one_tenths_bucket_are_one_key_and_a_bucket_apart_are_two() {
    let inside = plan_key("KTLX", 0.51);
    let also_inside = plan_key("KTLX", 0.54);
    let next_bucket = plan_key("KTLX", 0.56);

    assert_eq!(
        inside.select.elevation_tenths,
        Some(5),
        "premise: 0.51° buckets to 5 tenths",
    );
    assert_eq!(
        next_bucket.select.elevation_tenths,
        Some(6),
        "premise: 0.56° buckets to 6 tenths",
    );

    assert_eq!(
        inside, also_inside,
        "0.51° and 0.54° are one tenths bucket and must be one key — a finer \
         quantum re-renders the same picture per jitter of the selector",
    );
    assert_ne!(
        inside, next_bucket,
        "0.51° and 0.56° are a bucket apart and must be two keys — a coarser \
         quantum serves one tilt's raster for its neighbour",
    );

    let mut cache: HashMap<RenderKey, &str> = HashMap::new();
    cache.insert(inside.clone(), "the 0.5 tilt");
    assert_eq!(
        cache.get(&also_inside).copied(),
        Some("the 0.5 tilt"),
        "a selection in the same bucket missed the entry it should share — \
         equality and hashing disagree",
    );
    assert_eq!(
        cache.get(&next_bucket).copied(),
        None,
        "a selection a bucket away hit its neighbour's entry",
    );

    // The site axis, to keep the two assertions above from passing on a key
    // that ignores everything but the angle.
    assert_ne!(
        inside,
        plan_key("KOUN", 0.51),
        "control: two sites at one tilt collapsed to one key",
    );
}
