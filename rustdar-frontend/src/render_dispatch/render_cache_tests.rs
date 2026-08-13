use super::*;
use crate::constants::MAX_RENDER_CACHE_ENTRIES;

fn key(site: &str, elevation_tenths: i32) -> RenderCacheKey {
    (
        site.to_string(),
        RadarProduct::Reflectivity,
        RenderView::PlanView,
        elevation_tenths,
    )
}

/// A distinguishable entry — `max_range_km` doubles as the identity so a test
/// can tell which render it got back.
///
/// Empty buffers, so it costs nothing: the tests that use it are about the
/// *count* bound, and they are handed a byte budget that cannot bind so that
/// each bound is exercised on its own.
fn output(range: f64) -> CachedRenderOutput {
    CachedRenderOutput {
        image: Arc::new(egui::ColorImage::default()),
        max_range_km: range,
        hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
        nyquist_ms: None,
        melting_layer_source: None,
    }
}

/// The gates behind a raster of `side` — a full ring of 720 radials at the gate
/// count that side implies.
///
/// `side` is `2 · extent / sample · TEXELS_PER_SAMPLE` across the *diameter*
/// (see `rustdar_radar::types::data_limited_side_px`), so a radius holds
/// `side / 4` gates: 1840 at the 7362 px a surveillance cut asks for, against
/// the 1832 such a cut really carries.
fn hover_field(side: usize) -> rustdar_radar::render::polar::PolarField {
    use rustdar_radar::render::polar::{PolarField, PolarGeometry, Wedge};
    const RADIALS: usize = 720;
    let gates = side / 4;
    let wedges = (0..RADIALS)
        .map(|i| Wedge {
            azimuth_deg: i as f32 * 0.5,
            half_width_deg: 0.25,
        })
        .collect();
    PolarField::from_parts(
        PolarGeometry::from_parts(wedges, 0.125, 0.25, gates),
        vec![0.0; RADIALS * gates],
    )
}

/// An entry that costs what a real raster of `side` costs: the texture,
/// `side² × 4`, and the gates behind it.
///
/// Those two used to be the same size, because the second was a `side²` `f32`
/// grid of the first's values resampled up. It is the measurements now, so a
/// long-range entry costs a little over half what it did and the same budget
/// buys nearly twice as many of them — see
/// [`a_cache_of_long_range_rasters_is_bounded_by_bytes_not_by_entries`].
fn output_of_side(range: f64, side: usize) -> CachedRenderOutput {
    CachedRenderOutput {
        image: Arc::new(egui::ColorImage::new(
            [side, side],
            vec![egui::Color32::BLACK; side * side],
        )),
        max_range_km: range,
        hover: Arc::new(rustdar_radar::hover::HoverSource::resident(hover_field(
            side,
        ))),
        nyquist_ms: None,
        melting_layer_source: None,
    }
}

/// The bound the cache exists for. Before this it was a bare `HashMap` that only
/// `reset_panes*` ever shrank, so cycling products grew it without limit at
/// ~32 MiB per entry.
#[test]
fn inserting_past_capacity_evicts_instead_of_growing() {
    let mut cache = RenderCache::new(3, usize::MAX);
    for i in 0..10 {
        cache.insert(key("KTLX", i), output(i as f64));
    }
    assert_eq!(cache.entry_count(), 3, "capacity must bound the cache");
    // The three newest survived; everything older is gone.
    assert!(cache.get(&key("KTLX", 9)).is_some());
    assert!(cache.get(&key("KTLX", 8)).is_some());
    assert!(cache.get(&key("KTLX", 7)).is_some());
    assert!(cache.get(&key("KTLX", 6)).is_none());
    assert!(cache.get(&key("KTLX", 0)).is_none());
}

/// Least *recently used*, not least recently inserted. A pane that keeps reading
/// its entry must not lose it to one nobody has touched since it was written.
#[test]
fn a_read_protects_an_entry_from_eviction() {
    let mut cache = RenderCache::new(3, usize::MAX);
    cache.insert(key("KTLX", 0), output(0.0));
    cache.insert(key("KTLX", 1), output(1.0));
    cache.insert(key("KTLX", 2), output(2.0));

    // Touch the oldest, making the *second* oldest the eviction candidate.
    assert!(cache.get(&key("KTLX", 0)).is_some());
    cache.insert(key("KTLX", 3), output(3.0));

    assert!(
        cache.get(&key("KTLX", 0)).is_some(),
        "the read should have saved it"
    );
    assert!(
        cache.get(&key("KTLX", 1)).is_none(),
        "untouched since insert, so it goes"
    );
    assert_eq!(cache.entry_count(), 3);
}

/// Re-inserting an existing key replaces the value and refreshes its position,
/// rather than queueing the key a second time and corrupting the eviction order.
#[test]
fn reinserting_a_key_replaces_it_without_duplicating_it() {
    let mut cache = RenderCache::new(2, usize::MAX);
    cache.insert(key("KTLX", 0), output(0.0));
    cache.insert(key("KTLX", 1), output(1.0));
    cache.insert(key("KTLX", 0), output(99.0));

    assert_eq!(cache.entry_count(), 2, "a replacement is not a new entry");
    assert_eq!(cache.recency_order(), vec![key("KTLX", 1), key("KTLX", 0)]);
    assert_eq!(cache.get(&key("KTLX", 0)).unwrap().max_range_km, 99.0);

    // With `0` refreshed, `1` is now the oldest and is what a third insert evicts.
    cache.insert(key("KTLX", 2), output(2.0));
    assert!(cache.get(&key("KTLX", 1)).is_none());
    assert!(cache.get(&key("KTLX", 0)).is_some());
}

/// `reset_panes_for_site` drops one site's entries. The recency queue has to lose
/// them too, or it later evicts a key that is no longer in the map while the real
/// oldest entry survives.
#[test]
fn retain_drops_keys_from_the_recency_queue_as_well() {
    let mut cache = RenderCache::new(4, usize::MAX);
    cache.insert(key("KTLX", 0), output(0.0));
    cache.insert(key("KOUN", 1), output(1.0));
    cache.insert(key("KTLX", 2), output(2.0));

    cache.retain(|(site, _, _, _)| site != "KTLX");

    assert_eq!(cache.entry_count(), 1);
    assert_eq!(cache.recency_order(), vec![key("KOUN", 1)]);

    // Fill past capacity; KOUN is the oldest real entry and must be the one to go.
    for i in 10..14 {
        cache.insert(key("KDDC", i), output(i as f64));
    }
    assert_eq!(cache.entry_count(), 4);
    assert!(cache.get(&key("KOUN", 1)).is_none());
    assert!(cache.get(&key("KDDC", 13)).is_some());
}

#[test]
fn clear_empties_both_halves() {
    let mut cache = RenderCache::new(4, usize::MAX);
    cache.insert(key("KTLX", 0), output(0.0));
    cache.insert(key("KTLX", 1), output(1.0));
    cache.clear();
    assert_eq!(cache.entry_count(), 0);
    assert!(cache.recency_order().is_empty());
}

/// A zero capacity would evict every entry on the way in, silently disabling the
/// cross-pane sharing the cache exists for.
#[test]
fn capacity_is_floored_at_one() {
    let mut cache = RenderCache::new(0, usize::MAX);
    cache.insert(key("KTLX", 0), output(0.0));
    assert_eq!(cache.entry_count(), 1);
    assert!(cache.get(&key("KTLX", 0)).is_some());
}

/// The cache the dispatcher actually builds must hold every pane that can be on
/// screen at once, or the panes evict each other and every layout change
/// re-renders.
///
/// Asserted by filling a real `RenderDispatcher` rather than by comparing
/// `MAX_RENDER_CACHE_ENTRIES` against the pane limit. Those two constants can
/// both be right while the dispatcher hands `RenderCache::new` something else
/// entirely — a comparison of constants observes the *intent*, and this is the
/// one place the intent is wired up.
#[test]
fn the_dispatchers_own_cache_holds_every_pane_on_screen() {
    let max_panes = if cfg!(target_os = "android") {
        rustdar_egui::pane::MAX_PANES_MOBILE
    } else {
        rustdar_egui::pane::MAX_PANES_DESKTOP
    };
    let sites: Vec<String> = (0..max_panes).map(|i| format!("SITE{i}")).collect();
    assert!(
        MAX_RENDER_CACHE_ENTRIES >= sites.len(),
        "precondition: the bound itself is too small — {MAX_RENDER_CACHE_ENTRIES} \
             entries for {} panes",
        sites.len()
    );

    // A full screen of panes, each on its own site *and each a different
    // view*, cycling so a mixed screen is what is measured. The view axis
    // was the open question when it was added: a pane still wants exactly
    // one entry whatever it shows, so `capacity >= pane_count` is still the
    // whole invariant — what the axis removed is the *wrong* sharing
    // between a plan view and a section of the same product, not headroom.
    // Asserting it over mixed views is what says so.
    let views = RenderView::all();
    let view_of = |i: usize| views[i % views.len()];

    let mut dispatcher = RenderDispatcher::new();
    for (i, site) in sites.iter().enumerate() {
        dispatcher.cache_render(
            site,
            RadarProduct::Reflectivity,
            view_of(i),
            0.5,
            output(i as f64),
        );
    }

    // A full screen of panes, each on its own site: none may have evicted another.
    for (i, site) in sites.iter().enumerate() {
        let hit = dispatcher.get_cached_render(site, RadarProduct::Reflectivity, view_of(i), 0.5);
        let Some(hit) = hit else {
            panic!(
                "{site} was evicted with only {} panes' worth cached",
                sites.len()
            );
        };
        assert_eq!(
            hit.max_range_km, i as f64,
            "{site} came back as another pane's render"
        );
    }
}

/// The collision the view axis exists to stop: one site, one product, one
/// elevation, two views.
///
/// Without the axis the second `cache_render` overwrites the first and the
/// plan-view pane is handed the section's buffers — which is not a wrong
/// picture but `ColorImage::from_rgba_unmultiplied`'s `assert_eq!` on the
/// main thread, live in release, aborting the whole app under wasm.
#[test]
fn two_views_of_one_product_do_not_share_an_entry() {
    let mut d = RenderDispatcher::new();
    d.cache_render(
        "KTLX",
        RadarProduct::Reflectivity,
        RenderView::PlanView,
        0.5,
        output(1.0),
    );
    d.cache_render(
        "KTLX",
        RadarProduct::Reflectivity,
        RenderView::CrossSection,
        0.5,
        output(2.0),
    );
    assert_eq!(
        d.get_cached_render(
            "KTLX",
            RadarProduct::Reflectivity,
            RenderView::PlanView,
            0.5
        )
        .map(|c| c.max_range_km),
        Some(1.0),
        "the section overwrote the plan view",
    );
    assert_eq!(
        d.get_cached_render(
            "KTLX",
            RadarProduct::Reflectivity,
            RenderView::CrossSection,
            0.5,
        )
        .map(|c| c.max_range_km),
        Some(2.0),
    );
}

/// A vertical view has no elevation, so every tilt selection maps to one
/// entry — and that is only safe because the *view* is what keeps it apart
/// from a real 0.0° plan render, which no `i32` sentinel could have done.
#[test]
fn a_vertical_view_ignores_the_elevation_and_still_misses_the_plan_view() {
    let mut d = RenderDispatcher::new();
    d.cache_render(
        "KTLX",
        RadarProduct::Reflectivity,
        RenderView::CrossSection,
        3.4,
        output(7.0),
    );
    assert_eq!(
        d.get_cached_render(
            "KTLX",
            RadarProduct::Reflectivity,
            RenderView::CrossSection,
            19.5,
        )
        .map(|c| c.max_range_km),
        Some(7.0),
        "a section was keyed by a tilt it does not have",
    );
    assert!(
        d.get_cached_render(
            "KTLX",
            RadarProduct::Reflectivity,
            RenderView::PlanView,
            0.0,
        )
        .is_none(),
        "the section's viewless elevation slot collided with a real 0.0 plan render",
    );
}

/// The whole-volume predicate honours both of its halves — pinned as a
/// truth table over one concrete pair per quadrant, with the expected
/// answers written out as literals. This test used to restate production's
/// own `view.reads_whole_volume() || product.reads_whole_volume()` on the
/// expectation side, which could never disagree with the function it was
/// checking; a table of hard-coded booleans can.
#[test]
fn the_whole_volume_predicate_is_both_halves() {
    // The quadrant representatives: a cross-section is vertical structure
    // one sweep does not have (the view half), and interpolated echo tops
    // integrates the whole reflectivity volume (the product half), while a
    // reflectivity plan view is one sweep of one moment (neither half).
    let table = [
        // (view, product, needs the whole volume?)
        (RenderView::PlanView, RadarProduct::Reflectivity, false), // neither half
        (RenderView::CrossSection, RadarProduct::Reflectivity, true), // the view's sake alone
        (
            RenderView::PlanView,
            RadarProduct::EchoTopsInterpolated,
            true,
        ), // the product's alone
        (
            RenderView::CrossSection,
            RadarProduct::EchoTopsInterpolated,
            true,
        ), // both halves
    ];
    for (view, product, expected) in table {
        assert_eq!(
            needs_whole_volume(view, product),
            expected,
            "needs_whole_volume({view:?}, {product:?}) is no longer {expected}",
        );
    }

    // Non-vacuity: the view-only quadrant must really exist — a pair that
    // needs the volume for the view's sake alone is the case the product
    // half alone gets wrong, and every such pair must still answer true.
    let mut saw_view_only = false;
    for &view in RenderView::all() {
        for &product in RadarProduct::all() {
            if view.reads_whole_volume() && !product.reads_whole_volume() {
                assert!(needs_whole_volume(view, product));
                saw_view_only = true;
            }
        }
    }
    assert!(
        saw_view_only,
        "no (view, product) pair needs the volume for the view's sake alone, \
             so this says nothing about why both halves are asked",
    );
    assert!(
        !needs_whole_volume(RenderView::PlanView, RadarProduct::Reflectivity),
        "the predicate answers true for everything, which is safe and vacuous",
    );
}

/// The four products whose plan view is the same picture at every tilt. Named
/// here rather than derived, because this list *is* what the derived predicate
/// is being checked against: it is the set
/// `render::render_radar_to_image_full` dispatches before it calls
/// `find_sweep`, read off that function.
const TILT_INDEPENDENT: [RadarProduct; 4] = [
    RadarProduct::EchoTopsInterpolated,
    RadarProduct::ProbabilityOfSevereHail,
    RadarProduct::MaxExpectedHailSize,
    RadarProduct::HydrometeorClassification,
];

/// The predicate must name exactly the products the renderer dispatches
/// before `find_sweep` — no more (a tilt-dependent product collapsed into one
/// slot would hand a pane another tilt's picture) and no fewer (each one left
/// out is a whole-volume recompute per tilt click).
#[test]
fn the_tilt_independent_set_is_the_renderers_own_pre_sweep_dispatch() {
    for &product in RadarProduct::all() {
        assert_eq!(
            product.tilt_independent_plan_view(),
            TILT_INDEPENDENT.contains(&product),
            "{product:?} is on the wrong side of the tilt-independence line",
        );
    }
}

/// What the fix is for: clicking to another tilt on one of those panes now
/// finds the render already there instead of paying for the whole volume
/// again to redraw the identical picture.
#[test]
fn a_tilt_change_on_a_volume_product_is_a_cache_hit() {
    for product in TILT_INDEPENDENT {
        let mut d = RenderDispatcher::new();
        d.cache_render("KTLX", product, RenderView::PlanView, 0.5, output(11.0));
        assert_eq!(
            d.get_cached_render("KTLX", product, RenderView::PlanView, 19.5)
                .map(|c| c.max_range_km),
            Some(11.0),
            "{product:?} re-rendered the whole volume for a byte-identical picture",
        );
    }
}

/// And the other half: a product whose pixels really do come from the sweep
/// `find_sweep` picks must still miss, or a tilt click would show the tilt
/// before it. NROT and SRV are the pair that makes this more than a
/// restatement — both answer *true* to `reads_whole_volume`, because they fit
/// their dealias seed from every velocity tilt, and both still rasterize one
/// sweep.
#[test]
fn a_tilt_change_on_a_sweep_product_is_still_a_miss() {
    for &product in RadarProduct::all() {
        if TILT_INDEPENDENT.contains(&product) {
            continue;
        }
        let mut d = RenderDispatcher::new();
        d.cache_render("KTLX", product, RenderView::PlanView, 0.5, output(11.0));
        assert!(
            d.get_cached_render("KTLX", product, RenderView::PlanView, 19.5)
                .is_none(),
            "{product:?} answered a 19.5\u{b0} request with the 0.5\u{b0} render",
        );
    }
}

/// The collapse is per product, and the cache still tells the four apart from
/// each other and from the other views — a shared `NO_ELEVATION_SLOT` is not a
/// shared entry.
#[test]
fn the_collapsed_products_still_key_apart_from_each_other() {
    let mut d = RenderDispatcher::new();
    for (i, product) in TILT_INDEPENDENT.iter().enumerate() {
        d.cache_render(
            "KTLX",
            *product,
            RenderView::PlanView,
            0.5,
            output(i as f64),
        );
    }
    d.cache_render(
        "KTLX",
        RadarProduct::EchoTopsInterpolated,
        RenderView::CrossSection,
        0.5,
        output(99.0),
    );
    for (i, product) in TILT_INDEPENDENT.iter().enumerate() {
        assert_eq!(
            d.get_cached_render("KTLX", *product, RenderView::PlanView, 7.5)
                .map(|c| c.max_range_km),
            Some(i as f64),
            "{product:?} was handed another product's render",
        );
    }
    assert_eq!(
        d.get_cached_render(
            "KTLX",
            RadarProduct::EchoTopsInterpolated,
            RenderView::CrossSection,
            0.5,
        )
        .map(|c| c.max_range_km),
        Some(99.0),
        "the plan view and the section collided in the viewless slot",
    );
}

/// A completed cut still invalidates what it should. `reset_panes_for_tilts`
/// matches a plan view's entry by `elevation_key(angle)`, and the four
/// collapsed products no longer carry one — but they are also the products it
/// deliberately skips, because a volume still being assembled must not be read
/// as a complete short one. `reset_panes_for_site` is what refreshes them, and
/// it is elevation-blind, so the collapse cannot leave a stale entry behind.
#[test]
fn a_site_reset_still_clears_a_collapsed_entry() {
    let gui = rustdar_egui::Gui::new();
    let mut d = RenderDispatcher::new();
    for product in TILT_INDEPENDENT {
        d.cache_render("KTLX", product, RenderView::PlanView, 0.5, output(11.0));
    }
    d.reset_panes_for_site("KTLX", &gui);
    for product in TILT_INDEPENDENT {
        assert!(
            d.get_cached_render("KTLX", product, RenderView::PlanView, 0.5)
                .is_none(),
            "{product:?} survived a site reset in the viewless slot",
        );
    }
}

/// The cache is bounded by bytes as well as by entries, because the count
/// stopped being a statement about memory.
///
/// Eight entries of `4096² × 8` is 1 GiB, and that is what
/// [`MAX_RENDER_CACHE_ENTRIES`] meant while a plan view was one of three sizes.
/// Once the side became the device's own answer — a 7362 px surveillance cut on
/// a machine that reports 32768 — the same eight entries are 3.3 GiB. Nothing
/// in the cache was counting, so that regression would have been silent, which
/// is why this is asserted against the resident total and not against a count.
///
/// The three sizes are the real ones: a browser loop frame, the base raster,
/// and the widest sweep a WSR-88D flies at two texels per gate.
#[test]
fn a_cache_of_long_range_rasters_is_bounded_by_bytes_not_by_entries() {
    // A quarter of a base-size raster, so the arithmetic below is exact and the
    // test is not sized to whatever the host class ships.
    const BUDGET: usize = 4 * 128 * 1024 * 1024;

    for (side, why) in [
        (1024usize, "a browser's loop frame"),
        (2048, "the base raster"),
        (7362, "a 1832-gate surveillance cut at two texels per gate"),
    ] {
        let mut cache = RenderCache::new(usize::MAX, BUDGET);
        for i in 0..12 {
            cache.insert(key("KTLX", i), output_of_side(i as f64, side));
        }
        assert!(
            cache.resident_bytes() <= BUDGET,
            "{why}: {} bytes resident against a {BUDGET} byte budget",
            cache.resident_bytes(),
        );
        // And the bound is on bytes, so a smaller raster genuinely buys more
        // entries — the thing a fixed count could not express.
        let held = cache.entry_count();
        assert!(
            held >= 1,
            "{why}: the cache evicted everything, so nothing is ever shared",
        );
        // An entry is the texture plus the gates, and the gates are the
        // measurements rather than a resampling of them — so the arithmetic is
        // read off the same two buffers the cache charges for rather than
        // written down as `side² × 8`, which is what it was when the second
        // buffer was a second raster.
        let entry = side * side * 4 + hover_field(side).resident_bytes();
        assert_eq!(
            held,
            (BUDGET / entry).clamp(1, 12),
            "{why}: {held} entries of {side} px is not what the budget pays for",
        );
        assert!(
            entry * 3 < side * side * 8 * 2,
            "{why}: an entry is {entry} B against the {} B two rasters cost, \
             which is not the saving this change was for",
            side * side * 8,
        );
    }
}

/// An entry larger than the whole budget is kept anyway, alone.
///
/// The alternative is worse than holding it: a cache that dropped the render
/// just handed to it would report a miss to the pane that asked, which would
/// dispatch the same render again, for as long as the pane stayed open. The
/// budget bounds what is *retained beside* an entry, and cannot be a reason to
/// refuse one.
#[test]
fn a_single_raster_over_the_whole_budget_is_still_cached() {
    let mut cache = RenderCache::new(usize::MAX, 1);
    cache.insert(key("KTLX", 5), output_of_side(460.0, 512));
    assert_eq!(cache.entry_count(), 1);
    assert!(cache.get(&key("KTLX", 5)).is_some());

    // A second one evicts the first rather than accumulating.
    cache.insert(key("KTLX", 6), output_of_side(300.0, 512));
    assert_eq!(cache.entry_count(), 1);
    assert!(cache.get(&key("KTLX", 6)).is_some());
}

/// The byte total tracks every way an entry can arrive or leave, not just
/// insertion.
///
/// A counter that only counted upward would drift above the truth and evict a
/// cache that was already empty — silently turning off pane sharing, which is
/// the failure this whole cache exists to prevent and the one nothing on screen
/// would show.
#[test]
fn the_resident_total_survives_replacement_retention_and_clearing() {
    let mut cache = RenderCache::new(usize::MAX, usize::MAX);
    // What one entry costs, off the same two buffers the cache charges for
    // rather than written down. It was `512² × 8` while the second buffer was a
    // second raster of the same side; the gates behind a raster are not that
    // shape and never were, so a literal here would be pinning arithmetic the
    // cache stopped doing.
    let one = 512 * 512 * 4 + hover_field(512).resident_bytes();

    cache.insert(key("KTLX", 5), output_of_side(1.0, 512));
    cache.insert(key("KOUN", 5), output_of_side(2.0, 512));
    assert_eq!(cache.resident_bytes(), 2 * one);

    // Replacing a key must not count the old entry twice.
    cache.insert(key("KTLX", 5), output_of_side(3.0, 512));
    assert_eq!(
        cache.resident_bytes(),
        2 * one,
        "a replacement double-counted"
    );

    cache.retain(|(site, ..)| site == "KTLX");
    assert_eq!(cache.resident_bytes(), one, "retain did not release bytes");

    cache.clear();
    assert_eq!(cache.resident_bytes(), 0, "clear did not release bytes");
}
