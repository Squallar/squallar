//! **A shed rung reaches the render cache's byte capacity.**
//!
//! `adopt_budgets` is the one place `App::budgets` is written. It re-applied
//! the raster side ceiling and every tile cache's budget and left the render
//! cache filling to the capacity it was built with at startup — memory the
//! ladder had just decided the scene could not afford. The only correction
//! was `on_pressure`, which empties the cache outright.
//!
//! The budgets here are the desktop ceiling with the two render-cache fields
//! written down to what a test can afford to fill; nothing reads a bracket
//! constant's value, and nothing pins `render_cache_budget_bytes()`. The one
//! arithmetic precondition is on the pixels, which is what the cache prices an
//! entry at against its capacity (`RenderCache::entry_budget_bytes`), so the
//! eviction it demands is owed.

use squallar_device_profile::budget::{BudgetLimits, Budgets, Promotion, at_class_rung};
use squallar_device_profile::constants::PLAN_VIEW_TEXEL_BYTES;
use squallar_radar::types::{RadarProduct, RenderView};
use std::sync::Arc;

use super::tests::headless;
use crate::platform_double::TestBridge;
use crate::render_dispatch::CachedRenderOutput;

/// The fixture side: four rasters of it are a test's worth of heap.
const SIDE: usize = 256;

fn hover_field(side: usize) -> squallar_radar::render::polar::PolarField {
    use squallar_radar::render::polar::{PolarField, PolarGeometry, Wedge};
    const RADIALS: usize = 720;
    let gates = side / 4;
    let wedges = (0..RADIALS)
        .map(|i| Wedge {
            azimuth_deg: i as f32 * 0.5,
            half_width_deg: 0.25,
        })
        .collect();
    PolarField::from_parts(
        PolarGeometry::from_parts(wedges, 0.125, 0.25, None, gates),
        vec![0.0; RADIALS * gates],
    )
}

/// An entry that costs what a real raster of `SIDE` costs, with `range` as its
/// identity.
fn output(range: f64) -> CachedRenderOutput {
    CachedRenderOutput {
        image: Arc::new(egui::ColorImage::new(
            [SIDE, SIDE],
            vec![egui::Color32::BLACK; SIDE * SIDE],
        )),
        max_range_km: range,
        hover: Arc::new(squallar_radar::hover::HoverSource::resident(hover_field(
            SIDE,
        ))),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

/// The desktop ceiling with its render cache budget written down to `entries`
/// rasters of `SIDE` px — the two fields `render_cache_budget_bytes` is a
/// function of, set by hand on the `pub` struct.
fn budgets_holding(entries: usize) -> Budgets {
    let mut budgets = at_class_rung(&BudgetLimits::DESKTOP, Promotion::Ceiling);
    budgets.render_cache_entries = entries;
    budgets.long_range_image_side_px = SIDE;
    budgets
}

fn fill(app: &mut crate::app::App, first: usize, count: usize) {
    for i in first..first + count {
        app.render.cache_render(
            &format!("SITE{i}"),
            RadarProduct::Reflectivity,
            RenderView::PlanView,
            0.5,
            output(i as f64),
        );
    }
}

fn hit(app: &mut crate::app::App, i: usize) -> Option<f64> {
    app.render
        .get_cached_render(
            &format!("SITE{i}"),
            RadarProduct::Reflectivity,
            RenderView::PlanView,
            0.5,
        )
        .map(|c| c.max_range_km)
}

/// Adopting a narrower budget evicts what no longer fits, least-recently-used
/// first, and the cache refills to the narrow figure afterwards rather than
/// the one it was built with.
#[test]
fn a_shed_rung_re_applies_the_render_cache_byte_budget() {
    let mut app = headless(TestBridge::desktop());
    let wide = budgets_holding(12);
    let narrow = budgets_holding(2);
    assert!(
        narrow.render_cache_budget_bytes() < wide.render_cache_budget_bytes(),
        "precondition: the two budgets do not differ in the render cache term"
    );
    // Four rasters' pixels alone exceed the narrow budget, so a shed owes an
    // eviction whichever figure the cache compares against its capacity.
    assert!(
        4 * SIDE * SIDE * PLAN_VIEW_TEXEL_BYTES > narrow.render_cache_budget_bytes(),
        "precondition: four fixture rasters fit the narrow budget, so nothing is owed"
    );

    app.adopt_budgets(wide);
    fill(&mut app, 0, 4);
    assert_eq!(
        app.render.render_cache.entry_count(),
        4,
        "precondition: the wide budget did not hold the fixture, so the shrink is untested"
    );
    let resident_before = app.render.render_cache.resident_bytes();

    app.adopt_budgets(narrow);

    assert!(
        app.render.render_cache.entry_count() < 4,
        "a shed rung left the render cache holding what it held at the wide budget"
    );
    assert!(
        app.render.render_cache.resident_bytes() < resident_before,
        "a shed rung did not move the render cache's resident figure"
    );
    assert_eq!(
        hit(&mut app, 3),
        Some(3.0),
        "the most-recently-used render did not survive the shed"
    );
    assert!(
        hit(&mut app, 0).is_none(),
        "the least-recently-used render survived the shed"
    );

    // The capacity, not just the contents: refilling stops at the narrow
    // figure. This is the symptom — a cache that keeps refilling to the
    // startup budget after the ladder has shed a rung.
    fill(&mut app, 4, 4);
    assert!(
        app.render.render_cache.entry_count() < 4,
        "after the shed the render cache refilled past the narrow budget"
    );
}

/// The other direction: adopting a wider budget evicts nothing.
#[test]
fn a_regained_rung_evicts_nothing_from_the_render_cache() {
    let mut app = headless(TestBridge::desktop());
    app.adopt_budgets(budgets_holding(12));
    fill(&mut app, 0, 4);
    assert_eq!(app.render.render_cache.entry_count(), 4);
    let resident_before = app.render.render_cache.resident_bytes();

    app.adopt_budgets(budgets_holding(24));

    assert_eq!(
        app.render.render_cache.entry_count(),
        4,
        "a wider budget evicted from the render cache"
    );
    assert_eq!(app.render.render_cache.resident_bytes(), resident_before);
}
