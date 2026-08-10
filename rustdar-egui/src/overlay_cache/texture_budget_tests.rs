use super::*;

/// `max_texture_dimension_2d` on a desktop adapter. wgpu's `Limits::default()`
/// promises 8192; real GPUs report 16384 or more. Either way, nothing here is
/// allowed to shrink at that size.
const DESKTOP_LIMIT: u32 = 8192;
/// WebGL2's guaranteed floor, and the whole reason clamping exists.
const WEBGL2_LIMIT: u32 = 2048;

fn pane(w: f32, h: f32) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h))
}

/// A plan with the given overdraw. Dimensions are irrelevant to `coverage`, so
/// they stand in at 1x1 rather than pretending to a size.
fn plan_with_overdraw(overdraw: f32) -> OverlayTexturePlan {
    OverlayTexturePlan {
        width: 1,
        height: 1,
        overdraw,
    }
}

/// Right after a render: the texture covers `viewport ± overdraw` on all four
/// sides, and the viewport has not moved.
///
/// Deliberately routed through the production [`OverlayTexturePlan::coverage`]
/// rather than repeating its arithmetic. A fixture that computed the expansion
/// itself would agree with a broken `coverage` and hide it — the same shadowing
/// that let a wrong fraction at the call site go unnoticed.
fn freshly_rendered(viewport: &GeoBounds, overdraw: f32) -> GeoBounds {
    plan_with_overdraw(overdraw).coverage(viewport)
}

/// The reference viewport: **10° of latitude by 16° of longitude**.
///
/// Non-square on purpose. With a square viewport the latitude and longitude
/// bands are equal, and every per-axis mistake in `pan_exceeds_coverage` — most
/// obviously computing one axis's band from the other's ranges — produces
/// identical answers and survives every assertion here.
fn viewport() -> GeoBounds {
    GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -100.0,
        max_lon: -84.0,
    }
}

/// The viewport's own extent per axis, which is also the overdraw band at
/// `overdraw == 1.0`. Derived from [`viewport`] so the two cannot drift apart.
fn viewport_ranges() -> (f64, f64) {
    let vp = viewport();
    (vp.max_lat - vp.min_lat, vp.max_lon - vp.min_lon)
}

/// Slide a viewport south (negative) or north (positive) by `d` degrees.
fn panned_lat(viewport: &GeoBounds, d: f64) -> GeoBounds {
    GeoBounds {
        min_lat: viewport.min_lat + d,
        max_lat: viewport.max_lat + d,
        ..*viewport
    }
}

fn panned_lon(viewport: &GeoBounds, d: f64) -> GeoBounds {
    GeoBounds {
        min_lon: viewport.min_lon + d,
        max_lon: viewport.max_lon + d,
        ..*viewport
    }
}

// ── plan_overlay_texture ─────────────────────────────────────────────

/// The constraint being ported for: a pane only 683 points wide already asks for
/// 2049 px at the full overdraw, one past WebGL2's guarantee. egui only
/// `debug_assert!`s that bound, so a release wasm build would sail into
/// `Device::create_texture` and fail there instead.
#[test]
fn a_pane_that_would_overflow_the_limit_gives_up_overdraw_instead() {
    let unclamped = 683.0 * (1.0 + 2.0 * OVERDRAW_FRACTION);
    assert!(
        unclamped as u32 > WEBGL2_LIMIT,
        "fixture must actually cross the limit: {unclamped} vs {WEBGL2_LIMIT}"
    );

    let plan = plan_overlay_texture(pane(683.0, 400.0), WEBGL2_LIMIT);
    assert!(
        plan.width <= WEBGL2_LIMIT,
        "width {} exceeds the limit",
        plan.width
    );
    assert!(
        plan.height <= WEBGL2_LIMIT,
        "height {} exceeds the limit",
        plan.height
    );
    assert!(
        plan.overdraw < OVERDRAW_FRACTION,
        "overdraw {} should have been cut back from {OVERDRAW_FRACTION}",
        plan.overdraw
    );
    // The dimensions and the overdraw describe the same rectangle.
    assert_eq!(plan.width, (683.0 * (1.0 + 2.0 * plan.overdraw)) as u32);
    assert_eq!(plan.height, (400.0 * (1.0 + 2.0 * plan.overdraw)) as u32);
}

/// A realistic browser window: 1440 x 900 points against WebGL2's floor.
#[test]
fn a_full_size_browser_pane_stays_within_the_limit() {
    let plan = plan_overlay_texture(pane(1440.0, 900.0), WEBGL2_LIMIT);
    assert!(plan.width <= WEBGL2_LIMIT && plan.height <= WEBGL2_LIMIT);
    // Width is the binding axis, so it lands on the limit exactly.
    assert_eq!(plan.width, WEBGL2_LIMIT);
    // ...and the *same* fraction sizes the other axis, so the texture stays
    // proportional to the pane rather than stretching.
    assert_eq!(plan.height, (900.0 * (1.0 + 2.0 * plan.overdraw)) as u32);
    assert!(plan.overdraw > 0.0 && plan.overdraw < OVERDRAW_FRACTION);
}

/// The binding axis is whichever needs the most texels, not always width.
#[test]
fn the_taller_axis_can_be_the_binding_one() {
    let plan = plan_overlay_texture(pane(400.0, 1400.0), WEBGL2_LIMIT);
    assert!(plan.width <= WEBGL2_LIMIT && plan.height <= WEBGL2_LIMIT);
    assert_eq!(plan.height, WEBGL2_LIMIT);
    assert!(plan.overdraw < OVERDRAW_FRACTION);
}

/// Desktop must not change. A desktop adapter's limit is far above anything a
/// window can demand, so the plan is bit-for-bit what the old constant produced.
#[test]
fn a_desktop_adapter_limit_changes_nothing() {
    for (w, h) in [
        (683.0, 400.0),
        (1920.0, 1080.0),
        (2560.0, 1440.0),
        (100.0, 100.0),
    ] {
        let plan = plan_overlay_texture(pane(w, h), DESKTOP_LIMIT);
        assert_eq!(
            plan.overdraw, OVERDRAW_FRACTION,
            "{w}x{h} should keep the full overdraw on a desktop adapter"
        );
        assert_eq!(plan.width, (w * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32);
        assert_eq!(plan.height, (h * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32);
    }
}

/// Past the point where the viewport alone fills the limit there is no overdraw
/// left to give up, and the dimensions must still not overflow. Both axes are
/// oversized here so the clamp on each is genuinely exercised.
#[test]
fn a_pane_wider_than_the_limit_falls_back_to_zero_overdraw() {
    let rect = pane(3000.0, 2500.0);
    assert!(
        rect.width().min(rect.height()) > WEBGL2_LIMIT as f32,
        "fixture must overflow on both axes for both clamps to be reached"
    );
    let plan = plan_overlay_texture(rect, WEBGL2_LIMIT);
    assert_eq!(plan.overdraw, 0.0, "nothing left to give up");
    assert_eq!(plan.width, WEBGL2_LIMIT);
    assert_eq!(plan.height, WEBGL2_LIMIT);
}

/// Only the axis that actually overflows is truncated; the other keeps its
/// natural size. Clamping both to the limit would stretch the overlay.
#[test]
fn only_the_overflowing_axis_is_truncated() {
    let plan = plan_overlay_texture(pane(3000.0, 900.0), WEBGL2_LIMIT);
    assert_eq!(plan.overdraw, 0.0);
    assert_eq!(
        plan.width, WEBGL2_LIMIT,
        "the overflowing axis is cut to the limit"
    );
    assert_eq!(plan.height, 900, "the axis that fits keeps its own size");
}

/// A zero-area pane must not leak the `inf` its own division produces into the
/// fraction or the dimensions. Nothing special-cases the zero — `min` discards
/// the `inf` and the cast floors the product — so this pins that the general
/// arithmetic really does stay finite rather than that a guard branch exists.
#[test]
fn a_degenerate_pane_produces_a_finite_plan() {
    let plan = plan_overlay_texture(pane(0.0, 0.0), WEBGL2_LIMIT);
    assert!(plan.overdraw.is_finite(), "got {}", plan.overdraw);
    assert_eq!(
        plan.overdraw, OVERDRAW_FRACTION,
        "a zero side constrains nothing"
    );
    assert_eq!((plan.width, plan.height), (0, 0));
}

/// One zero axis, one real one: the real axis still has to be sized and clamped
/// normally rather than being dragged to zero or to `inf` by its neighbour.
#[test]
fn a_pane_with_one_zero_axis_still_sizes_the_other() {
    let plan = plan_overlay_texture(pane(0.0, 3000.0), WEBGL2_LIMIT);
    assert!(plan.overdraw.is_finite());
    assert_eq!(
        plan.overdraw, 0.0,
        "the 3000pt axis alone exhausts the limit"
    );
    assert_eq!(plan.width, 0);
    assert_eq!(plan.height, WEBGL2_LIMIT);
}

// ── pan_exceeds_coverage ─────────────────────────────────────────────

/// The check the cache exists for. Before this was measured from the bounds it
/// compared `tex_range * OVERDRAW_FRACTION * PAN_REBUILD_THRESHOLD`, and with a
/// three-viewport texture that margin (2.1 viewports) swallowed the whole
/// overdraw band — so this returned `true` the instant the render landed and
/// every overlay re-rasterised on every frame.
#[test]
fn a_texture_that_just_rendered_covers_its_own_viewport() {
    let vp = viewport();
    let tex = freshly_rendered(&vp, OVERDRAW_FRACTION);
    assert!(
        !pan_exceeds_coverage(&tex, &vp),
        "a texture rendered for this very viewport cannot already be out of coverage"
    );
}

/// Past `PAN_REBUILD_THRESHOLD` of the band, on every edge.
///
/// Each axis is measured against **its own** band. The viewport is 10° tall and
/// 16° wide, so a pan that overruns the latitude band is comfortably inside the
/// longitude one — which is what makes this fail if the two are ever crossed.
#[test]
fn panning_most_of_the_way_across_the_band_triggers_a_rebuild() {
    let vp = viewport();
    let (band_lat, band_lon) = viewport_ranges(); // at overdraw 1.0 the band is the range
    let tex = freshly_rendered(&vp, OVERDRAW_FRACTION);
    let past = |band: f64| band * (PAN_REBUILD_THRESHOLD as f64 + 0.05);
    let short = |band: f64| band * (PAN_REBUILD_THRESHOLD as f64 - 0.05);

    for d in [past(band_lat), -past(band_lat)] {
        assert!(
            pan_exceeds_coverage(&tex, &panned_lat(&vp, d)),
            "lat pan {d} must rebuild"
        );
    }
    for d in [past(band_lon), -past(band_lon)] {
        assert!(
            pan_exceeds_coverage(&tex, &panned_lon(&vp, d)),
            "lon pan {d} must rebuild"
        );
    }
    for d in [short(band_lat), -short(band_lat)] {
        assert!(
            !pan_exceeds_coverage(&tex, &panned_lat(&vp, d)),
            "lat pan {d} is still covered"
        );
    }
    for d in [short(band_lon), -short(band_lon)] {
        assert!(
            !pan_exceeds_coverage(&tex, &panned_lon(&vp, d)),
            "lon pan {d} is still covered"
        );
    }
}

/// Each axis's margin comes from that axis's own ranges. The bands here differ by
/// 60% (10° of latitude against 16° of longitude), so a pan sized to sit just
/// inside the latitude band lands *outside* it if the longitude band is
/// substituted — a cross-axis mix-up no square fixture can see.
#[test]
fn each_axis_is_judged_against_its_own_band() {
    let vp = viewport();
    let (band_lat, band_lon) = viewport_ranges();
    assert!(
        band_lon > band_lat * 1.5,
        "fixture must be decisively non-square"
    );

    let tex = freshly_rendered(&vp, OVERDRAW_FRACTION);
    // Headroom is 30% of the band: 3° of latitude, 4.8° of longitude. A 6.5°
    // southward pan leaves 3.5° of the latitude band — still covered — but would
    // read as only 3.5° against a 4.8° longitude margin, and rebuild.
    let pan = panned_lat(&vp, -6.5);
    let headroom = 1.0 - PAN_REBUILD_THRESHOLD as f64;
    assert!(
        band_lat * headroom < 3.5 && 3.5 < band_lon * headroom,
        "fixture must straddle the two margins: {} < 3.5 < {}",
        band_lat * headroom,
        band_lon * headroom
    );
    assert!(
        !pan_exceeds_coverage(&tex, &pan),
        "the latitude band still covers this pan"
    );
}

/// The invariant clamping would otherwise break. A texture whose overdraw was cut
/// to 0.2 tolerates far less pan than one with the full 1.0 — and the check has to
/// notice, or the cache holds a stale image over ground it never rasterised.
#[test]
fn a_clamped_texture_runs_out_of_coverage_sooner_than_a_full_one() {
    let vp = viewport();
    let clamped = freshly_rendered(&vp, 0.2);
    let full = freshly_rendered(&vp, OVERDRAW_FRACTION);

    // 0.2 of a 10 degree viewport height is a 2 degree band; 0.7 of that is 1.4.
    let pan = panned_lat(&vp, -1.6);
    assert!(
        pan_exceeds_coverage(&clamped, &pan),
        "a 2-degree band cannot absorb a 1.6-degree pan"
    );
    assert!(
        !pan_exceeds_coverage(&full, &pan),
        "precondition: the same pan is comfortably inside a full-overdraw texture, \
             so only the coverage measurement distinguishes these"
    );
}

/// A texture with no overdraw at all — what a pane wider than the adapter's limit
/// gets — must rebuild on any pan whatsoever, and *not* before.
///
/// The unpanned case is the one that matters: with a zero band every comparison
/// sits exactly on its boundary, so a `<` relaxed to `<=` reports "panned off"
/// for a viewport that has not moved at all. That re-rasterises every frame on
/// precisely the wide-pane wasm configuration this whole change exists for, and
/// no non-degenerate fixture can see it.
#[test]
fn a_zero_overdraw_texture_rebuilds_on_the_slightest_pan() {
    let vp = viewport();
    let tex = freshly_rendered(&vp, 0.0);
    assert!(
        !pan_exceeds_coverage(&tex, &vp),
        "a zero-overdraw texture still covers the viewport it was rendered for"
    );
    assert!(pan_exceeds_coverage(&tex, &panned_lat(&vp, -0.001)));
    assert!(pan_exceeds_coverage(&tex, &panned_lat(&vp, 0.001)));
    assert!(pan_exceeds_coverage(&tex, &panned_lon(&vp, -0.001)));
    assert!(pan_exceeds_coverage(&tex, &panned_lon(&vp, 0.001)));
}

/// A pane that grew since its texture was rasterised is no longer covered, even
/// without panning. The measured band goes negative and trips the comparison.
#[test]
fn a_pane_that_outgrew_its_texture_rebuilds() {
    let vp = viewport();
    let tex = freshly_rendered(&vp, 0.1);
    let grown = GeoBounds {
        min_lat: 20.0,
        max_lat: 50.0,
        min_lon: -110.0,
        max_lon: -80.0,
    };
    assert!(
        grown.max_lat - grown.min_lat > tex.max_lat - tex.min_lat,
        "fixture must actually outgrow the texture"
    );
    assert!(pan_exceeds_coverage(&tex, &grown));
}

// ── the two together ─────────────────────────────────────────────────

/// End to end: plan a texture against a small limit, take the coverage the plan
/// itself reports (exactly as `spawn_overlay_render` does), and the coverage
/// check agrees it is fresh. Expanding by `OVERDRAW_FRACTION` instead — the bug
/// clamping would introduce — claims ground the pixels never covered.
#[test]
fn the_plans_overdraw_is_what_the_coverage_check_reads_back() {
    let vp = viewport();
    let (band_lat_at_full, _) = viewport_ranges();
    let plan = plan_overlay_texture(pane(1440.0, 900.0), WEBGL2_LIMIT);
    assert!(
        plan.overdraw < OVERDRAW_FRACTION,
        "fixture must be a clamped one"
    );

    // The production path: the plan is asked for its own coverage.
    let honest = plan.coverage(&vp);
    assert!(!pan_exceeds_coverage(&honest, &vp));

    // The band the honest texture really has, and a pan that overruns it.
    let overrun = panned_lat(&vp, -(band_lat_at_full * plan.overdraw as f64 * 0.95));
    assert!(pan_exceeds_coverage(&honest, &overrun));

    // Had the bounds been expanded by the unclamped constant, the same pan would
    // have looked comfortably covered — the stale-overlay failure mode.
    let overclaimed = plan_with_overdraw(OVERDRAW_FRACTION).coverage(&vp);
    assert!(!pan_exceeds_coverage(&overclaimed, &overrun));
}

// ── OverlayTexturePlan::coverage ─────────────────────────────────────

/// The plan's own fraction sizes the bounds, so pixels and coverage describe the
/// same rectangle. A clamped plan must produce visibly tighter bounds than the
/// unclamped constant would.
#[test]
fn the_bounds_grow_by_the_plans_overdraw_not_the_constant() {
    // A 1440pt-wide pane against WebGL2's floor: the plan has to give overdraw up.
    let plan = plan_overlay_texture(pane(1440.0, 900.0), WEBGL2_LIMIT);
    assert!(
        plan.overdraw < OVERDRAW_FRACTION,
        "fixture must be a clamped plan, else this test cannot tell the two apart"
    );

    let vp = viewport();
    let (lat_range, lon_range) = viewport_ranges();
    let honest = plan.coverage(&vp);
    let overclaimed = plan_with_overdraw(OVERDRAW_FRACTION).coverage(&vp);

    assert!((honest.min_lat - (vp.min_lat - lat_range * plan.overdraw as f64)).abs() < 1e-9);
    assert!((honest.max_lon - (vp.max_lon + lon_range * plan.overdraw as f64)).abs() < 1e-9);
    assert!(
        honest.min_lat > overclaimed.min_lat,
        "the clamped plan must claim strictly less ground than the constant would"
    );
    assert!(honest.max_lat < overclaimed.max_lat);
    assert!(honest.min_lon > overclaimed.min_lon);
    assert!(honest.max_lon < overclaimed.max_lon);
}

/// Zero overdraw — a pane wider than the adapter's whole texture limit — must
/// leave the viewport exactly as it is rather than defaulting to a margin.
#[test]
fn zero_overdraw_leaves_the_viewport_untouched() {
    let vp = viewport();
    let bounds = plan_with_overdraw(0.0).coverage(&vp);
    assert_eq!(
        (
            bounds.min_lat,
            bounds.max_lat,
            bounds.min_lon,
            bounds.max_lon
        ),
        (vp.min_lat, vp.max_lat, vp.min_lon, vp.max_lon)
    );
}

/// Latitude is clamped to the Mercator-valid range; longitude is not, because
/// the map wraps.
#[test]
fn latitude_is_clamped_to_the_mercator_range() {
    let polar = GeoBounds {
        min_lat: -80.0,
        max_lat: 80.0,
        min_lon: -10.0,
        max_lon: 10.0,
    };
    assert!(
        80.0 + 160.0 * OVERDRAW_FRACTION as f64 > MERCATOR_LAT_LIMIT,
        "fixture must actually overrun the clamp"
    );
    let bounds = plan_with_overdraw(OVERDRAW_FRACTION).coverage(&polar);
    assert_eq!(bounds.max_lat, MERCATOR_LAT_LIMIT);
    assert_eq!(bounds.min_lat, -MERCATOR_LAT_LIMIT);
    assert_eq!(bounds.min_lon, -10.0 - 20.0 * OVERDRAW_FRACTION as f64);
}
