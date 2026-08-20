//! What a theme flip may and may not invalidate.

use crate::platform_double::TestBridge;
use crate::render_dispatch::CachedRenderOutput;
use crate::render_key::RenderKey;
use rustdar_radar::types::RenderView;
use std::sync::Arc;

use super::tests::headless;

/// One radar render-cache entry, the `render_cache_tests` way: empty buffers,
/// `max_range_km` as the identity.
fn a_radar_entry() -> (RenderKey, CachedRenderOutput) {
    (
        crate::render_key::render_cache_key(
            "KTLX",
            &rustdar_radar::fields::known::REFLECTIVITY,
            RenderView::PlanView,
            0.5,
        ),
        CachedRenderOutput {
            image: Arc::new(egui::ColorImage::default()),
            max_range_km: 230.0,
            hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        },
    )
}

/// The gens the flip is measured against, one per pane.
fn radar_sites_gens(app: &mut super::App) -> Vec<u64> {
    app.gui
        .panes_mut()
        .iter()
        .map(|p| p.radar_sites_render_gen)
        .collect()
}

/// A theme flip invalidates the site labels on every pane and nothing in the
/// radar's render cache.
#[test]
fn a_theme_flip_never_touches_the_radar_render_cache() {
    let mut app = headless(TestBridge::desktop());
    assert!(
        app.cached_dark_theme.is_none(),
        "premise: no theme reading has been adopted yet",
    );
    assert!(
        app.adopt_theme(true),
        "the first reading from a None start is a change",
    );

    let (key, entry) = a_radar_entry();
    app.render.render_cache.insert(key.clone(), entry);
    let entries_before = app.render.render_cache.entry_count();
    let gens_before = radar_sites_gens(&mut app);
    assert!(
        !gens_before.is_empty(),
        "premise: there are panes to measure the flip against",
    );

    assert!(app.adopt_theme(false), "a real flip is a change");

    assert_eq!(
        app.render.render_cache.entry_count(),
        entries_before,
        "a theme flip flushed the radar render cache — up to 128 MiB of \
         theme-independent pixels re-decoded for a change that cannot alter them",
    );
    assert!(
        app.render.render_cache.get(&key).is_some(),
        "the resident radar entry did not survive the flip",
    );
    for (idx, (before, after)) in gens_before
        .iter()
        .zip(radar_sites_gens(&mut app))
        .enumerate()
    {
        assert_eq!(
            after,
            before.wrapping_add(1),
            "pane {idx}: a flip must invalidate the site-label raster exactly once",
        );
    }

    let gens_after_flip = radar_sites_gens(&mut app);
    assert!(
        !app.adopt_theme(false),
        "a repeated reading reported itself as a change",
    );
    assert_eq!(
        radar_sites_gens(&mut app),
        gens_after_flip,
        "a repeated reading bumped the site-label gens — the Android poller \
         would re-rasterise every pane twice a second",
    );
}
