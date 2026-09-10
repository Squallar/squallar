//! **The storm report layer's `paints_in` against the rasterizer it mirrors.**
//!
//! `StormReportsHandler::paints_in` refuses a raster whose ground holds no
//! report the depicted instant has reached. A wrong `false` is not a wasted
//! raster, it is a **cleared pane**: the refusal is delivered as a blank and a
//! blank is a clear. So the property that matters is one-directional and is
//! what every case here asserts —
//!
//! > a refusal implies the rasterizer would have painted no ink.
//!
//! Nothing here states where the boundary is. The comparison is between the
//! real predicate and the real `rasterize_storm_reports` over the same bounds,
//! and the bounds come from the real `plan_overlay_texture` over a viewport
//! laid out at the same zoom the predicate is handed — because the predicate's
//! texel→ground conversion goes through that zoom, and a fixture free to make
//! the zoom disagree with the pane would be testing arithmetic nobody runs.
//!
//! `has_ink` reads the returned bytes rather than any `blank` field: `blank` is
//! written by the job funnel's output stage, not by a rasterizer, so a picture
//! examined here has never been judged. A byte that is not zero is ink.

use squallar_egui::overlay_cache::{ZOOM_QUANTIZATION_FACTOR, plan_overlay_texture};
use squallar_geo::GeoBounds;
use squallar_overlays::render::rasterize::{self, ReportPaint, ReportPlace, ReportsInput};
use squallar_overlays::spc::reports::StormReportKind;
use std::sync::Arc;

/// `walkers::mercator::project_at_scale`, verbatim — the projection the pane's
/// own viewport bounds are two unprojected corners of.
fn project(lat: f64, lon: f64, total_pixels: f64) -> (f64, f64) {
    let x = (1.0 + (lon.to_radians() / std::f64::consts::PI)) / 2.0;
    let y = (1.0 - (lat.to_radians().tan().asinh() / std::f64::consts::PI)) / 2.0;
    (x * total_pixels, y * total_pixels)
}

/// Its inverse, verbatim.
fn unproject(px: f64, py: f64, total_pixels: f64) -> (f64, f64) {
    let lon = ((px / total_pixels) * 2.0 - 1.0) * std::f64::consts::PI;
    let lat = (-(py / total_pixels) * 2.0 + 1.0) * std::f64::consts::PI;
    (lat.sinh().atan().to_degrees(), lon.to_degrees())
}

/// `squallar_egui::overlay_cache::viewport_geo_bounds` over
/// `walkers::Projector`: the pane's NW and SE corners, unprojected. `pane` is
/// in **logical points**, the frame walkers lays the map out in.
fn viewport(centre: (f64, f64), zoom: f64, pane: (f32, f32)) -> GeoBounds {
    let total = 2f64.powf(zoom) * 256.0;
    let (cx, cy) = project(centre.0, centre.1, total);
    let (nw_lat, nw_lon) = unproject(
        cx - f64::from(pane.0) / 2.0,
        cy - f64::from(pane.1) / 2.0,
        total,
    );
    let (se_lat, se_lon) = unproject(
        cx + f64::from(pane.0) / 2.0,
        cy + f64::from(pane.1) / 2.0,
        total,
    );
    GeoBounds {
        min_lat: nw_lat.min(se_lat),
        max_lat: nw_lat.max(se_lat),
        min_lon: nw_lon.min(se_lon),
        max_lon: nw_lon.max(se_lon),
    }
}

/// KTLX, so the fixtures sit where the feed's reports do.
const CENTRE: (f64, f64) = (35.33, -97.28);

fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 6, 12)
        .expect("a real date")
        .and_hms_opt(h, m, 0)
        .expect("a real clock reading")
}

fn has_ink(out: &rasterize::RasterizeOutput) -> bool {
    out.rgba.iter().any(|byte| *byte != 0)
}

/// One report at `(lat, lon)`, drawn through the real rasterizer over
/// `bounds` at the plan's own size and density.
fn paints_ink(
    lat: f64,
    lon: f64,
    valid: Option<chrono::NaiveDateTime>,
    bounds: &GeoBounds,
    plan: &squallar_egui::overlay_cache::OverlayTexturePlan,
    zoom: f64,
    as_of: chrono::NaiveDateTime,
) -> bool {
    let input = ReportsInput {
        reports: Arc::new(vec![ReportPaint {
            kind: StormReportKind::Hail,
            lat,
            lon,
            valid,
        }]),
        zoom,
        is_dark: false,
        device_scale: plan.pixels_per_point,
        as_of,
    };
    has_ink(&rasterize::rasterize_storm_reports(
        &input,
        bounds,
        plan.width,
        plan.height,
    ))
}

/// The predicate over exactly one report.
fn paints_in(
    lat: f64,
    lon: f64,
    valid: Option<chrono::NaiveDateTime>,
    bounds: &GeoBounds,
    zoom: f64,
    device_scale: f32,
    as_of: chrono::NaiveDateTime,
) -> bool {
    rasterize::any_report_paints_in(
        std::iter::once(ReportPlace { lat, lon, valid }),
        bounds,
        zoom,
        device_scale,
        as_of,
    )
}

/// The zoom the dispatch hands the predicate is the pane's, **quantized** —
/// `App::spawn_overlay_render` divides an `i32` by
/// [`ZOOM_QUANTIZATION_FACTOR`] — so the fixture quantizes it too. This is the
/// one term the predicate is allowed to be handed inexactly, and the pad
/// carries headroom for exactly this.
fn dispatch_zoom(zoom: f64) -> f64 {
    ((zoom * ZOOM_QUANTIZATION_FACTOR).round() as i32) as f64 / ZOOM_QUANTIZATION_FACTOR
}

/// The scenes: a pane, a display density, an oversampling and a zoom. The pane
/// sizes bracket a phone pane in a six-pane grid and a pane wider than a
/// WebGL2 texture limit, which is the case where `plan_overlay_texture` gives
/// up resolution and `pixels_per_point` stops being the display's.
const PANES: [(f32, f32); 4] = [
    (120.0, 90.0),
    (320.0, 200.0),
    (960.0, 540.0),
    (3000.0, 1600.0),
];
const DENSITIES: [f32; 3] = [1.0, 2.0, 3.0];
const OVERSAMPLES: [f32; 2] = [0.0, 0.25];
const ZOOMS: [f64; 3] = [3.4, 6.53, 10.7];

/// How far outside the box, in spans, a refusal is **rasterized** to prove it.
///
/// Every refusal is counted, and the ones inside this ring are the ones the
/// rasterizer is asked about. A report a third of a span outside the ground the
/// picture covers is tens to thousands of texels off the pixmap at every size
/// planned here, and the raster that proves it drew nothing costs an 8.9 MB
/// pixmap to allocate and scan. The boundary — the only ground where the
/// predicate and the rasterizer can disagree — is inside the ring.
const PROVEN_RING_SPANS: f64 = 0.4;
/// The renderer limit the primary WebGL2 arm reports, and a limit no arm
/// reports at all.
///
/// **512 is deliberate and is the harder case.** The pad the predicate spends
/// is the rasterizer's slack in *texels* carried into ground, so the smaller a
/// picture's texel count the larger the share of its own box that slack is: a
/// limit that clamps a 3000-point pane to 512 texels is the most a real plan
/// could ever ask of the conversion. The desktop arms' 8192 is not here for a
/// reason that is about this test rather than about coverage — a scene refused
/// at 8192 texels rasterizes a 143 MB pixmap to prove it drew nothing, and
/// the property under test does not depend on the count.
const MAX_SIDES: [u32; 2] = [2048, 512];

/// **A refusal implies no ink, over every scene and a ring of positions
/// around each.** The positions walk from well outside the box to well inside
/// it in twentieths of a span, on both axes and both diagonals, so the
/// boundary of the predicate is crossed in every scene rather than assumed.
#[test]
fn a_refused_report_could_not_have_painted_ink() {
    let as_of = at(23, 0);
    let mut refusals = 0usize;
    let mut admissions = 0usize;
    let mut proven = 0usize;
    for &pane in &PANES {
        for &density in &DENSITIES {
            for &oversample in &OVERSAMPLES {
                for &zoom in &ZOOMS {
                    for &max_side in &MAX_SIDES {
                        let plan = plan_overlay_texture(
                            egui::Rect::from_min_size(
                                egui::pos2(0.0, 0.0),
                                egui::vec2(pane.0, pane.1),
                            ),
                            max_side,
                            density,
                            oversample,
                        );
                        let view = viewport(CENTRE, zoom, pane);
                        let bounds = plan.coverage(&view);
                        let lat_span = bounds.max_lat - bounds.min_lat;
                        let lon_span = bounds.max_lon - bounds.min_lon;
                        let dz = dispatch_zoom(zoom);
                        for step in -20i32..=40 {
                            let f = f64::from(step) / 20.0;
                            for (lat, lon) in [
                                (
                                    bounds.min_lat + lat_span * 0.5,
                                    bounds.min_lon + lon_span * f,
                                ),
                                (
                                    bounds.min_lat + lat_span * f,
                                    bounds.min_lon + lon_span * 0.5,
                                ),
                                (bounds.min_lat + lat_span * f, bounds.min_lon + lon_span * f),
                                (bounds.min_lat + lat_span * f, bounds.max_lon - lon_span * f),
                            ] {
                                let admitted = paints_in(
                                    lat,
                                    lon,
                                    None,
                                    &bounds,
                                    dz,
                                    plan.pixels_per_point,
                                    as_of,
                                );
                                if admitted {
                                    admissions += 1;
                                    continue;
                                }
                                refusals += 1;
                                let outside = ((bounds.min_lat - lat).max(lat - bounds.max_lat)
                                    / lat_span)
                                    .max(
                                        (bounds.min_lon - lon).max(lon - bounds.max_lon) / lon_span,
                                    );
                                if outside > PROVEN_RING_SPANS {
                                    continue;
                                }
                                proven += 1;
                                assert!(
                                    !paints_ink(lat, lon, None, &bounds, &plan, dz, as_of),
                                    "`paints_in` refused a report the rasterizer inked: \
                                     pane {pane:?} density {density} oversample {oversample} \
                                     zoom {zoom} max_side {max_side} plan {}x{} @{} \
                                     report ({lat}, {lon}) in {bounds:?}",
                                    plan.width,
                                    plan.height,
                                    plan.pixels_per_point,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    // Anti-vacuity on both halves: a predicate stuck at `true` refuses
    // nothing and this test would then assert nothing at all, and one stuck at
    // `false` would admit nothing and be the pane-clearing defect itself.
    assert!(refusals > 1_000, "only {refusals} refusals were exercised");
    assert!(
        admissions > 1_000,
        "only {admissions} admissions were exercised"
    );
    assert!(
        proven > 300,
        "only {proven} refusals were put to the rasterizer, out of {refusals}"
    );
}

/// **The layer still paints where its reports are** — the floor under the case
/// above, at the centre of every scene, read off the rasterizer's own bytes.
#[test]
fn a_report_under_the_camera_is_admitted_and_inks() {
    let as_of = at(23, 0);
    for &pane in &PANES {
        for &density in &DENSITIES {
            for &zoom in &ZOOMS {
                let plan = plan_overlay_texture(
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(pane.0, pane.1)),
                    2048,
                    density,
                    0.125,
                );
                let view = viewport(CENTRE, zoom, pane);
                let bounds = plan.coverage(&view);
                let dz = dispatch_zoom(zoom);
                assert!(
                    paints_in(
                        CENTRE.0,
                        CENTRE.1,
                        Some(at(22, 0)),
                        &bounds,
                        dz,
                        plan.pixels_per_point,
                        as_of
                    ),
                    "a report at the centre of the view was refused: pane {pane:?} \
                     density {density} zoom {zoom}"
                );
                assert!(
                    paints_ink(
                        CENTRE.0,
                        CENTRE.1,
                        Some(at(22, 0)),
                        &bounds,
                        &plan,
                        dz,
                        as_of
                    ),
                    "the rasterizer drew nothing for a report at the centre of the view: \
                     pane {pane:?} density {density} zoom {zoom}"
                );
            }
        }
    }
}

/// **The as-of term is the rasterizer's, including the `None`.** A report the
/// depicted instant has not reached is refused by both; a report with no
/// readable time is refused by neither, which is what keeps a report the feed
/// timestamped badly on the map.
#[test]
fn the_instant_a_picture_depicts_is_the_only_clock_either_reads() {
    let as_of = at(23, 0);
    let pane = (960.0, 540.0);
    let plan = plan_overlay_texture(
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(pane.0, pane.1)),
        2048,
        2.0,
        0.125,
    );
    let bounds = plan.coverage(&viewport(CENTRE, 7.0, pane));
    let dz = dispatch_zoom(7.0);

    for (valid, expected) in [
        (Some(at(22, 0)), true),
        (Some(at(23, 0)), true),
        (Some(at(23, 1)), false),
        (None, true),
    ] {
        assert_eq!(
            paints_in(
                CENTRE.0,
                CENTRE.1,
                valid,
                &bounds,
                dz,
                plan.pixels_per_point,
                as_of
            ),
            expected,
            "the door and the rasterizer disagree about a report valid at {valid:?}"
        );
        assert_eq!(
            paints_ink(CENTRE.0, CENTRE.1, valid, &bounds, &plan, dz, as_of),
            expected,
            "the rasterizer's own as-of cull is not what the door mirrors, at {valid:?}"
        );
    }
}

/// **A zoom or a density that does not describe a picture dispatches.** The
/// one direction the predicate may not be wrong in is `false`, so an input it
/// cannot convert to ground is an admission and not a refusal.
#[test]
fn a_context_the_pad_cannot_be_computed_from_admits_everything() {
    let as_of = at(23, 0);
    let bounds = GeoBounds {
        min_lat: 33.0,
        max_lat: 37.0,
        min_lon: -100.0,
        max_lon: -95.0,
    };
    // Maine, degrees outside the box: refused with a usable context, admitted
    // without one.
    let (lat, lon) = (44.5, -69.5);
    assert!(!paints_in(lat, lon, None, &bounds, 7.0, 2.0, as_of));
    for (zoom, scale) in [
        (f64::NAN, 2.0f32),
        (f64::INFINITY, 2.0),
        (7.0, f32::NAN),
        (7.0, 0.0),
        (7.0, -1.0),
    ] {
        assert!(
            paints_in(lat, lon, None, &bounds, zoom, scale, as_of),
            "a context with zoom {zoom} and density {scale} produced a refusal"
        );
    }
}
