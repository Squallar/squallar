//! **The continuous longitude frame `308f1592` stands on.**
//!
//! The GMGSI seam fix in [`rasterize_gridded`] has two halves — carry each
//! point to the representation nearest the box, and refuse a neighbour further
//! than a half-turn as a spacing — and the second reads [`half_turn_px`]:
//! `180 / (max_lon - min_lon) * w`. That denominator is the box's width *in the
//! frame the box arrives in*, and the app has exactly one frame:
//! `walkers::Projector::unproject` is linear in pixel x and folds nothing,
//! `squallar_egui::overlay_cache::viewport_geo_bounds` takes its two corners as
//! they come, and `OverlayTexturePlan::coverage` grows the result without
//! folding. A viewport straddling the seam therefore reaches the raster as
//! `178.88..181.12`, never as `-178.88..178.88`.
//!
//! Nothing in `gmgsi_seam_probe_tests` can see that precondition: it pins one
//! viewport's output, and a fold upstream leaves every fixture there green
//! while the denominator under the refusal moves from 2.25 degrees to 357.75.
//! This file asserts the precondition where it is consumed — on the box
//! `rasterize_gridded` is handed — and shows the folded spelling of the same
//! box failing with both denominators in the message.
//!
//! The transcriptions below are the seam probe's, written a second time rather
//! than shared: that file is a regression surface and stays byte-identical.

use super::*;

/// `walkers::mercator::unproject_at_scale`, verbatim. **Longitude is linear in
/// x with no wrap**, which is the whole of the precondition.
fn unproject(px: f64, py: f64, total_pixels: f64) -> (f64, f64) {
    let lon = ((px / total_pixels) * 2.0 - 1.0) * std::f64::consts::PI;
    let lat = (-(py / total_pixels) * 2.0 + 1.0) * std::f64::consts::PI;
    (lat.sinh().atan().to_degrees(), lon.to_degrees())
}

/// `walkers::mercator::project_at_scale`, verbatim.
fn project(lat: f64, lon: f64, total_pixels: f64) -> (f64, f64) {
    let x = (1.0 + (lon.to_radians() / std::f64::consts::PI)) / 2.0;
    let y = (1.0 - (lat.to_radians().tan().asinh() / std::f64::consts::PI)) / 2.0;
    (x * total_pixels, y * total_pixels)
}

/// `squallar_egui::overlay_cache::viewport_geo_bounds` over
/// `walkers::Projector`: unproject the pane's NW and SE corners.
fn viewport_bounds(zoom: f64) -> GeoBounds {
    let total = 2f64.powf(zoom) * 256.0;
    let (cx, cy) = project(CENTRE.0, CENTRE.1, total);
    let (nw_lat, nw_lon) = unproject(cx - PANE_W / 2.0, cy - PANE_H / 2.0, total);
    let (se_lat, se_lon) = unproject(cx + PANE_W / 2.0, cy + PANE_H / 2.0, total);
    GeoBounds {
        min_lat: nw_lat.min(se_lat),
        max_lat: nw_lat.max(se_lat),
        min_lon: nw_lon.min(se_lon),
        max_lon: nw_lon.max(se_lon),
    }
}

/// `squallar_egui::overlay_cache::OverlayTexturePlan::coverage`, verbatim —
/// latitude clamped to the Mercator limit, longitude not clamped and not
/// folded.
fn coverage(view: &GeoBounds, overdraw: f64) -> GeoBounds {
    const LIMIT: f64 = squallar_geo::MERCATOR_LAT_LIMIT_DEG;
    let lat_range = view.max_lat - view.min_lat;
    let lon_range = view.max_lon - view.min_lon;
    GeoBounds {
        min_lat: (view.min_lat - lat_range * overdraw).max(-LIMIT),
        max_lat: (view.max_lat + lat_range * overdraw).min(LIMIT),
        min_lon: view.min_lon - lon_range * overdraw,
        max_lon: view.max_lon + lon_range * overdraw,
    }
}

/// The fold this file refuses: each edge into `[-180, 180)`, then re-sorted,
/// which is what "fold the bounds" spells wherever it is spelled.
fn folded_into_pm180(b: &GeoBounds) -> GeoBounds {
    let fold = |lon: f64| (lon + 180.0).rem_euclid(360.0) - 180.0;
    let (a, c) = (fold(b.min_lon), fold(b.max_lon));
    GeoBounds {
        min_lon: a.min(c),
        max_lon: a.max(c),
        ..*b
    }
}

const PANE_W: f64 = 2878.0;
const PANE_H: f64 = 1651.0;
/// On the antimeridian, at the Aleutians' latitude: the view a user reaches by
/// panning west along the chain.
const CENTRE: (f64, f64) = (51.88, 180.0);
/// The world is 691 000 px wide and the pane 0.42 % of it: a 1.5-degree view.
const ZOOM: f64 = 11.4;
/// `OVERDRAW_FRACTION` — the shipped 150 % oversample, one quarter a side.
const OVERDRAW: f64 = 0.25;

/// The width the viewport has in a continuous frame: the pane's share of the
/// world at `zoom`, `PANE_W / (2^zoom * 256)` of a turn. From the zoom and the
/// pane alone, so that a fold applied anywhere upstream — to the viewport, to
/// the box — is named against the true width and not against itself.
fn view_span_at(zoom: f64) -> f64 {
    PANE_W / (2f64.powf(zoom) * 256.0) * 360.0
}

/// The precondition, as a verdict with its figures. `Ok` when `cov` is the
/// viewport at `zoom` grown by the overdraw in one continuous frame — the box
/// is that and nothing else — and, for a view narrower than a half-turn, a
/// half-turn is then wider than the whole texture, so no two points the
/// texture holds can be a half-turn apart and the refusal only ever declines a
/// whole-turn jump. `Err` naming both denominators when the box has been
/// folded.
fn continuous_frame(zoom: f64, cov: &GeoBounds, w: u32) -> Result<(), String> {
    let view_span = view_span_at(zoom);
    let span = cov.max_lon - cov.min_lon;
    let expected = view_span * (1.0 + 2.0 * OVERDRAW);
    let htp = half_turn_px(cov, w);
    let expected_htp = half_turn_px(
        &GeoBounds {
            min_lon: 0.0,
            max_lon: expected,
            ..*cov
        },
        w,
    );
    if (span - expected).abs() <= 1e-9 * expected && htp > w as f32 {
        return Ok(());
    }
    Err(format!(
        "the box handed to rasterize_gridded spans {span:.4} deg where the viewport \
         spans {view_span:.4} deg (x{}) = {expected:.4} deg: it has been folded into \
         +/-180. half_turn_px = 180 / {span:.4} * {w} = {htp:.0} px against a {w} px \
         texture, where the continuous frame gives 180 / {expected:.4} * {w} = \
         {expected_htp:.0} px. 308f1592's seam refusal reads that denominator \
         (rasterize::half_turn_px); the frame is decided upstream, where \
         walkers::Projector::unproject folds nothing.",
        1.0 + 2.0 * OVERDRAW
    ))
}

/// **The precondition, on the box the app builds.** The view straddles the
/// seam, so its box holds a longitude past 180 and is continuous across it.
#[test]
fn a_seam_straddling_viewport_hands_the_raster_one_continuous_box() {
    let view = viewport_bounds(ZOOM);
    let view_span = view.max_lon - view.min_lon;
    let cov = coverage(&view, OVERDRAW);
    let w = (PANE_W * (1.0 + 2.0 * OVERDRAW)) as u32;
    println!(
        "view [{:.5}, {:.5}] span {view_span:.5}  box [{:.5}, {:.5}] span {:.5}  \
         half_turn_px {:.0} of {w} px",
        view.min_lon,
        view.max_lon,
        cov.min_lon,
        cov.max_lon,
        cov.max_lon - cov.min_lon,
        half_turn_px(&cov, w)
    );
    assert!(
        cov.min_lon < 180.0 && cov.max_lon > 180.0,
        "the fixture must straddle the seam, and a straddling box carries a longitude \
         past 180; got [{}, {}]",
        cov.min_lon,
        cov.max_lon
    );
    if let Err(why) = continuous_frame(ZOOM, &cov, w) {
        panic!("{why}");
    }
    assert!(
        (view_span - view_span_at(ZOOM)).abs() <= 1e-9 * view_span,
        "non-triviality: the transcribed unproject yields the pane's share of the world, \
         {view_span} against {}",
        view_span_at(ZOOM)
    );
}

/// **The same box folded into ±180 is refused, and the message names the
/// jump.** The 2.25-degree denominator becomes 357.75, and the half-turn falls
/// from 80 textures to half of one.
#[test]
fn the_same_box_folded_into_pm180_is_refused_with_both_denominators_named() {
    let view = viewport_bounds(ZOOM);
    let cov = coverage(&view, OVERDRAW);
    let w = (PANE_W * (1.0 + 2.0 * OVERDRAW)) as u32;

    let folded = folded_into_pm180(&cov);
    let folded_span = folded.max_lon - folded.min_lon;
    assert!(
        folded_span > 350.0,
        "the fold must turn a straddling box into a near-world one; got {folded_span}"
    );
    let why = continuous_frame(ZOOM, &folded, w).expect_err("the folded box must be refused");
    println!("{why}");
    let small = format!("{:.4}", view_span_at(ZOOM) * (1.0 + 2.0 * OVERDRAW));
    let big = format!("{folded_span:.4}");
    assert!(
        why.contains(&small) && why.contains(&big),
        "the refusal must name both denominators, {small} and {big}: {why}"
    );
    assert!(
        why.contains("walkers::Projector::unproject"),
        "the refusal must point at the upstream half: {why}"
    );
    assert!(
        half_turn_px(&folded, w) < w as f32,
        "in the folded frame a half-turn fits inside the texture — the regime the \
         refusal was never meant to run in"
    );
}
