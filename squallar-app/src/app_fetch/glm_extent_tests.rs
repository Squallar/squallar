//! **The lightning layer's `paints_in` against the rasterizer it mirrors.**
//!
//! `GlmHandler::paints_in` refuses a raster whose ground holds no flash the
//! depicted instant's fade window has reached. A wrong `false` is not a wasted
//! raster, it is a **cleared pane**: the refusal is delivered as a blank and a
//! blank is a clear. So the property that matters is one-directional and is
//! what every case here asserts —
//!
//! > a refusal implies the rasterizer would have painted no ink.
//!
//! Nothing here states where the boundary is. The comparison is between the
//! real `FlashOccupancy` and the real `rasterize_glm_strikes` over the same
//! bounds, and the bounds come from the real `plan_overlay_texture` over a
//! viewport laid out at the same zoom the predicate is handed — because the
//! predicate's texel→ground conversion goes through that zoom, and a fixture
//! free to make the zoom disagree with the pane would be testing arithmetic
//! nobody runs.
//!
//! # Why this layer's door is an index and not a scan
//!
//! The storm reports' door walks its rows. This layer's population is the
//! 250,000-flash retention ceiling, so the same walk is over a millisecond on
//! the frame thread. What a dispatch reads instead is a 1°×1° occupancy bitmap
//! with the time extent of each of its words, built once per generation inside
//! the walk that was already building the paint rows. **That quantization is
//! itself the conservative term** — a cell is occupied when a flash is
//! anywhere inside it — and at every zoom this file plans, one cell of grid is
//! far more ground than the pad `glm_pad` spends. The pad is still computed and
//! still exact; it is subsumed, not unused, and `the_margins_are_margin`
//! records which of the three margins any case here actually needs.
//!
//! `has_ink` reads the returned bytes rather than any `blank` field: `blank` is
//! written by the job funnel's output stage, not by a rasterizer, so a picture
//! examined here has never been judged. A byte that is not zero is ink.

use squallar_egui::overlay_cache::{ZOOM_QUANTIZATION_FACTOR, plan_overlay_texture};
use squallar_geo::GeoBounds;
use squallar_overlays::render::rasterize::{self, FlashOccupancy, FlashPaint, GlmStrikesInput};
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

/// KTLX, so the fixtures sit where a plains lightning complex does.
const CENTRE: (f64, f64) = (35.33, -97.28);
/// The handler's shipped default, and the one term of the fade the door reads.
const WINDOW: f64 = 300.0;

fn at(h: u32, m: u32, s: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 6, 12)
        .expect("a real date")
        .and_hms_opt(h, m, s)
        .expect("a real clock reading")
}

fn has_ink(out: &rasterize::RasterizeOutput) -> bool {
    out.rgba.iter().any(|byte| *byte != 0)
}

/// One flash at `(lat, lon)`, drawn through the real rasterizer over `bounds`
/// at the plan's own size and density.
fn paints_ink(
    lat: f64,
    lon: f64,
    time: chrono::NaiveDateTime,
    bounds: &GeoBounds,
    plan: &squallar_egui::overlay_cache::OverlayTexturePlan,
    zoom: f64,
    as_of: chrono::NaiveDateTime,
) -> bool {
    let input = GlmStrikesInput {
        flashes: Arc::new(vec![FlashPaint {
            lat,
            lon,
            time,
            energy: None,
        }]),
        zoom,
        is_dark: false,
        time_window_secs: WINDOW,
        now: as_of,
        device_scale: plan.pixels_per_point,
    };
    has_ink(&rasterize::rasterize_glm_strikes(
        &input,
        bounds,
        plan.width,
        plan.height,
    ))
}

/// The predicate over an index holding exactly one flash — the same index the
/// handler builds, through the same two public calls.
fn paints_in(
    lat: f64,
    lon: f64,
    time: chrono::NaiveDateTime,
    bounds: &GeoBounds,
    zoom: f64,
    device_scale: f32,
    as_of: chrono::NaiveDateTime,
) -> bool {
    let mut occupancy = FlashOccupancy::default();
    occupancy.insert(lat, lon, time);
    occupancy.any_paints_in(bounds, zoom, device_scale, as_of, WINDOW)
}

/// The zoom the dispatch hands the predicate is the pane's, **quantized** —
/// `App::spawn_overlay_render` divides an `i32` by
/// [`ZOOM_QUANTIZATION_FACTOR`] — so the fixture quantizes it too.
fn dispatch_zoom(zoom: f64) -> f64 {
    ((zoom * ZOOM_QUANTIZATION_FACTOR).round() as i32) as f64 / ZOOM_QUANTIZATION_FACTOR
}

/// The scenes: a pane, a display density, an oversampling and a zoom. The pane
/// sizes bracket a phone pane in a six-pane grid and a pane wider than a WebGL2
/// texture limit, which is the case where `plan_overlay_texture` gives up
/// resolution and `pixels_per_point` stops being the display's.
const PANES: [(f32, f32); 4] = [
    (120.0, 90.0),
    (320.0, 200.0),
    (960.0, 540.0),
    (3000.0, 1600.0),
];
const DENSITIES: [f32; 3] = [1.0, 2.0, 3.0];
const OVERSAMPLES: [f32; 2] = [0.0, 0.25];
/// Low, middle and high: at 3.4 a box spans many grid cells, at 10.7 a single
/// cell is orders of magnitude more ground than the whole box, and the two ends
/// exercise opposite halves of the query's cell arithmetic.
const ZOOMS: [f64; 3] = [3.4, 6.53, 10.7];
/// The renderer limit the primary WebGL2 arm reports, and a limit no arm
/// reports at all. **512 is deliberate and is the harder case**: the pad is the
/// rasterizer's slack in *texels* carried into ground, so the smaller a
/// picture's texel count the larger the share of its own box that slack is.
const MAX_SIDES: [u32; 2] = [2048, 512];

/// How far outside the box, in spans, a refusal is **rasterized** to prove it.
/// The boundary — the only ground where the predicate and the rasterizer can
/// disagree — is inside the ring, and a flash further out is thousands of
/// texels off a pixmap that costs megabytes to allocate and scan.
const PROVEN_RING_SPANS: f64 = 0.4;

/// **A refusal implies no ink, over every scene and a ring of positions around
/// each.** The positions walk from well outside the box to well inside it in
/// twentieths of a span, on both axes and both diagonals, so the boundary of
/// the predicate is crossed in every scene rather than assumed.
#[test]
fn a_refused_flash_could_not_have_painted_ink() {
    let as_of = at(23, 0, 0);
    // Inside the window, and not on either of its edges: this case is about
    // the geographic term, and `the_fade_window_is_the_rasterizers_own` owns
    // the other one.
    let time = at(22, 58, 0);
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
                                    time,
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
                                    !paints_ink(lat, lon, time, &bounds, &plan, dz, as_of),
                                    "`paints_in` refused a flash the rasterizer inked: \
                                     pane {pane:?} density {density} oversample {oversample} \
                                     zoom {zoom} max_side {max_side} plan {}x{} @{} \
                                     flash ({lat}, {lon}) in {bounds:?}",
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
    // Anti-vacuity on both halves: an index stuck at `true` refuses nothing and
    // this test would then assert nothing at all, and one stuck at `false`
    // would admit nothing and be the pane-clearing defect itself.
    assert!(refusals > 1_000, "only {refusals} refusals were exercised");
    assert!(
        admissions > 1_000,
        "only {admissions} admissions were exercised"
    );
    assert!(
        proven > 100,
        "only {proven} refusals were put to the rasterizer, out of {refusals}"
    );
}

/// **The layer still paints where its lightning is** — the floor under the case
/// above, at the centre of every scene, read off the rasterizer's own bytes.
#[test]
fn a_flash_under_the_camera_is_admitted_and_inks() {
    let as_of = at(23, 0, 0);
    let time = at(22, 58, 0);
    for &pane in &PANES {
        for &density in &DENSITIES {
            for &zoom in &ZOOMS {
                let plan = plan_overlay_texture(
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(pane.0, pane.1)),
                    2048,
                    density,
                    0.125,
                );
                let bounds = plan.coverage(&viewport(CENTRE, zoom, pane));
                let dz = dispatch_zoom(zoom);
                assert!(
                    paints_in(
                        CENTRE.0,
                        CENTRE.1,
                        time,
                        &bounds,
                        dz,
                        plan.pixels_per_point,
                        as_of
                    ),
                    "a flash at the centre of the view was refused: pane {pane:?} \
                     density {density} zoom {zoom}"
                );
                assert!(
                    paints_ink(CENTRE.0, CENTRE.1, time, &bounds, &plan, dz, as_of),
                    "the rasterizer drew nothing for a flash at the centre of the view: \
                     pane {pane:?} density {density} zoom {zoom}"
                );
            }
        }
    }
}

/// **A wrapped viewport asks about the same representation of a flash either
/// way.** The rasterizer folds a flash into `[min_lon, min_lon + 360)` before
/// it tests anything; the index places it by `lon.rem_euclid(360)` and folds
/// the box instead. A dateline pane is where those two spellings could
/// disagree, so it is asserted against the rasterizer's bytes rather than
/// argued.
#[test]
fn a_dateline_viewport_agrees_with_the_rasterizer() {
    let as_of = at(23, 0, 0);
    let time = at(22, 58, 0);
    let pane = (960.0, 540.0);
    let plan = plan_overlay_texture(
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(pane.0, pane.1)),
        2048,
        2.0,
        0.125,
    );
    // A pane whose bounds straddle the anti-meridian, in the unfolded frame
    // `walkers` hands over: -195..-165 is 165°E..195°E.
    let bounds = plan.coverage(&viewport((20.0, -180.0), 5.0, pane));
    assert!(
        bounds.min_lon < -180.0,
        "the fixture's own bounds do not cross the anti-meridian: {bounds:?}"
    );
    let dz = dispatch_zoom(5.0);
    let mut agreed = 0usize;
    for step in -30i32..=30 {
        // Walk across the seam in the *folded* frame a granule reports in.
        let lon = 178.0 + f64::from(step) * 0.25;
        let lon = if lon > 180.0 { lon - 360.0 } else { lon };
        let inked = paints_ink(20.0, lon, time, &bounds, &plan, dz, as_of);
        if inked {
            assert!(
                paints_in(20.0, lon, time, &bounds, dz, plan.pixels_per_point, as_of),
                "a flash the rasterizer inked at lon {lon} was refused over {bounds:?}"
            );
            agreed += 1;
        }
    }
    assert!(agreed > 20, "only {agreed} inked positions were exercised");
}

/// **The fade window and the as-of cull are the rasterizer's own, and there is
/// no missing timestamp to handle.**
///
/// `FlashPaint::time` is a `NaiveDateTime` and not an `Option` — unlike the
/// storm reports' `valid`, a GLM flash without a readable time never reaches a
/// row, because the granule's time variable is what the parse keys every record
/// on. So the case the reports' door spends a branch on does not exist here,
/// and what is pinned instead is the interval: `[as_of - window, as_of]`, both
/// edges, against the bytes.
///
/// **The door admits a one-second band beyond each edge** and this asserts that
/// direction rather than equality. The rasterizer's window test is `f64`
/// seconds off an integer millisecond subtraction and the index's is integer
/// milliseconds; a second is more than that seam can be, and one extra second
/// of admission is one raster, while a second of refusal is a cleared pane.
#[test]
fn the_fade_window_is_the_rasterizers_own() {
    let as_of = at(23, 0, 0);
    let pane = (960.0, 540.0);
    let plan = plan_overlay_texture(
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(pane.0, pane.1)),
        2048,
        2.0,
        0.125,
    );
    let bounds = plan.coverage(&viewport(CENTRE, 7.0, pane));
    let dz = dispatch_zoom(7.0);

    // (the flash's time, whether the rasterizer inks it)
    let cases = [
        // Two seconds into the future: past the door's one-second band, and
        // `rasterize_glm_strikes` culls a flash later than the depicted
        // instant outright rather than clamping its age to zero.
        (at(23, 0, 2), false),
        (at(23, 0, 0), true),
        // The far edge of the fade: age exactly `WINDOW` still inks, because
        // `time_decay_color`'s ramp ends at an opaque red and never at a zero
        // alpha.
        (at(22, 55, 0), true),
        // Two seconds past it, so the door's own one-second band is cleared on
        // this side too.
        (at(22, 54, 58), false),
        (at(22, 0, 0), false),
    ];
    for (time, inks) in cases {
        assert_eq!(
            paints_ink(CENTRE.0, CENTRE.1, time, &bounds, &plan, dz, as_of),
            inks,
            "the rasterizer's own time culls are not what this case claims, at {time}"
        );
        assert_eq!(
            paints_in(
                CENTRE.0,
                CENTRE.1,
                time,
                &bounds,
                dz,
                plan.pixels_per_point,
                as_of
            ),
            inks,
            "the door and the rasterizer disagree about a flash at {time}"
        );
    }

    // The seam itself, both sides: the door admits and the rasterizer does not,
    // which is the direction stated above and is asserted so a later change
    // that inverted it would be caught rather than merely be safe.
    for time in [at(23, 0, 1), at(22, 54, 59)] {
        assert!(
            !paints_ink(CENTRE.0, CENTRE.1, time, &bounds, &plan, dz, as_of),
            "the fixture's seam is not one: the rasterizer inked at {time}"
        );
        assert!(
            paints_in(
                CENTRE.0,
                CENTRE.1,
                time,
                &bounds,
                dz,
                plan.pixels_per_point,
                as_of
            ),
            "the door refused a flash inside its own one-second band, at {time}"
        );
    }
}

/// **A scrubbed pane whose window holds no flash refuses**, which is the whole
/// of what the index's time term buys: the geographic term cannot see it,
/// because the flash is directly under the camera.
#[test]
fn an_instant_no_flash_reaches_is_refused_under_the_camera() {
    let pane = (960.0, 540.0);
    let plan = plan_overlay_texture(
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(pane.0, pane.1)),
        2048,
        2.0,
        0.125,
    );
    let bounds = plan.coverage(&viewport(CENTRE, 7.0, pane));
    let dz = dispatch_zoom(7.0);
    let time = at(20, 0, 0);
    // Scrubbed three hours past the flash: inside the box, outside the window.
    assert!(!paints_in(
        CENTRE.0,
        CENTRE.1,
        time,
        &bounds,
        dz,
        plan.pixels_per_point,
        at(23, 0, 0)
    ));
    assert!(!paints_ink(
        CENTRE.0,
        CENTRE.1,
        time,
        &bounds,
        &plan,
        dz,
        at(23, 0, 0)
    ));
    // Scrubbed to before it happened at all.
    assert!(!paints_in(
        CENTRE.0,
        CENTRE.1,
        time,
        &bounds,
        dz,
        plan.pixels_per_point,
        at(19, 0, 0)
    ));
}

/// **Everything the door cannot convert dispatches.** The one direction the
/// predicate may not be wrong in is `false`, so a context, a window, a box or a
/// row it cannot place is an admission — enumerated here rather than left to
/// the reader of the branch.
#[test]
fn what_the_door_cannot_convert_admits_everything() {
    let as_of = at(23, 0, 0);
    let time = at(22, 58, 0);
    let bounds = GeoBounds {
        min_lat: 33.0,
        max_lat: 37.0,
        min_lon: -100.0,
        max_lon: -95.0,
    };
    // Maine, degrees outside the box and in another grid cell entirely:
    // refused with a usable context, admitted without one.
    let (lat, lon) = (44.5, -69.5);
    assert!(!paints_in(lat, lon, time, &bounds, 7.0, 2.0, as_of));

    for (zoom, scale) in [
        (f64::NAN, 2.0f32),
        (f64::INFINITY, 2.0),
        (7.0, f32::NAN),
        (7.0, 0.0),
        (7.0, -1.0),
    ] {
        assert!(
            paints_in(lat, lon, time, &bounds, zoom, scale, as_of),
            "a context with zoom {zoom} and density {scale} produced a refusal"
        );
    }

    let mut occupancy = FlashOccupancy::default();
    occupancy.insert(lat, lon, time);
    for window in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert!(
            occupancy.any_paints_in(&bounds, 7.0, 2.0, as_of, window),
            "a fade window of {window} produced a refusal"
        );
    }

    // A box whose own edges are inverted or non-finite.
    for bad in [
        GeoBounds {
            min_lat: 37.0,
            max_lat: 33.0,
            min_lon: -100.0,
            max_lon: -95.0,
        },
        GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -95.0,
            max_lon: -100.0,
        },
        GeoBounds {
            min_lat: f64::NAN,
            max_lat: 37.0,
            min_lon: -100.0,
            max_lon: -95.0,
        },
        GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: f64::NAN,
            max_lon: -95.0,
        },
    ] {
        assert!(
            occupancy.any_paints_in(&bad, 7.0, 2.0, as_of, WINDOW),
            "a box this index cannot place produced a refusal: {bad:?}"
        );
    }

    // **A row the index cannot place stops the index describing the rows**, so
    // a set holding one admits everywhere — not just at the unplaceable row.
    for (bad_lat, bad_lon) in [
        (f64::NAN, -97.0),
        (35.0, f64::NAN),
        (f64::INFINITY, -97.0),
        (35.0, f64::NEG_INFINITY),
    ] {
        let mut occupancy = FlashOccupancy::default();
        occupancy.insert(bad_lat, bad_lon, time);
        assert!(
            occupancy.any_paints_in(&bounds, 7.0, 2.0, as_of, WINDOW),
            "a flash at ({bad_lat}, {bad_lon}) did not open the door"
        );
        // And it is the *set* that opens, not the one row: a second, placeable
        // flash on the far side of the planet must not close it again.
        occupancy.insert(-40.0, 150.0, time);
        assert!(
            occupancy.any_paints_in(&bounds, 7.0, 2.0, as_of, WINDOW),
            "a placeable flash closed a door an unplaceable one had opened"
        );
    }

    // An empty index refuses: the rasterizer over no flashes paints nothing,
    // and `GlmHandler::paints_in` never reaches this — it answers `true` for an
    // empty slab so `prepare_job`'s `None` keeps its one meaning.
    assert!(!FlashOccupancy::default().any_paints_in(&bounds, 7.0, 2.0, as_of, WINDOW));
}

/// **The index is a superset of the rows it was built from, over a whole
/// population rather than one flash at a time.**
///
/// Every case above puts exactly one flash in the index, which is what makes a
/// refusal attributable. This one builds an index over 4,000 flashes spread
/// across a continent and asserts that no box the index refuses holds a flash
/// the rasterizer would have inked — the property the door actually relies on,
/// where a word's shared time extent and a cell's shared occupancy are both in
/// play.
#[test]
fn a_populated_index_refuses_only_where_no_row_inks() {
    let as_of = at(23, 0, 0);
    let pane = (960.0, 540.0);
    let plan = plan_overlay_texture(
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(pane.0, pane.1)),
        2048,
        2.0,
        0.125,
    );
    // Two clusters and two ages: a live complex over Oklahoma and one over
    // Florida that went quiet an hour ago, so a box can be occupied and stale.
    let mut rows = Vec::new();
    for i in 0..2_000u32 {
        let f = f64::from(i);
        rows.push(FlashPaint {
            lat: 34.0 + (f * 0.7).sin() * 2.5,
            lon: -98.0 + (f * 0.31).cos() * 3.0,
            time: at(22, 57, 0) + chrono::TimeDelta::milliseconds(i64::from(i)),
            energy: None,
        });
        rows.push(FlashPaint {
            lat: 27.5 + (f * 0.13).cos() * 1.5,
            lon: -81.0 + (f * 0.53).sin() * 2.0,
            time: at(21, 50, 0) + chrono::TimeDelta::milliseconds(i64::from(i)),
            energy: None,
        });
    }
    let mut occupancy = FlashOccupancy::default();
    for row in &rows {
        occupancy.insert(row.lat, row.lon, row.time);
    }
    // **Zoom 9, so a box is smaller than the gap between the two clusters.**
    // At zoom 6 a 960-point pane covers 24° of longitude — wider than the
    // whole sweep below — and every position admits; the case would then have
    // asserted nothing, which is what its refusal floor catches.
    const Z: f64 = 9.0;
    let input = GlmStrikesInput {
        flashes: Arc::new(rows),
        zoom: dispatch_zoom(Z),
        is_dark: false,
        time_window_secs: WINDOW,
        now: as_of,
        device_scale: plan.pixels_per_point,
    };

    let mut refusals = 0usize;
    let mut admissions = 0usize;
    // A sweep of camera positions over both clusters, the gap between them and
    // ground with nothing on it at all.
    for lat_step in 0..12 {
        for lon_step in 0..12 {
            let centre = (
                22.0 + f64::from(lat_step) * 2.0,
                -112.0 + f64::from(lon_step) * 3.6,
            );
            let bounds = plan.coverage(&viewport(centre, Z, pane));
            let dz = dispatch_zoom(Z);
            if occupancy.any_paints_in(&bounds, dz, plan.pixels_per_point, as_of, WINDOW) {
                admissions += 1;
                continue;
            }
            refusals += 1;
            assert!(
                !has_ink(&rasterize::rasterize_glm_strikes(
                    &input,
                    &bounds,
                    plan.width,
                    plan.height
                )),
                "the index refused a box the rasterizer inked from a 4,000-row \
                 population: centre {centre:?} bounds {bounds:?}"
            );
        }
    }
    assert!(
        refusals > 40,
        "only {refusals} boxes were refused, {admissions} admitted"
    );
    assert!(
        admissions > 5,
        "only {admissions} boxes were admitted, {refusals} refused"
    );
}

/// **The index's footprint is a function of the grid and not of the rows.**
///
/// `footprint::glm_paint_rows` states this figure in a comment, and the whole
/// point of the census family that reads it is to find what nobody is counting
/// — so the constant is pinned rather than trusted. An index over 4,000 flashes
/// and an empty one are the same size.
#[test]
fn the_index_costs_the_same_whatever_the_row_count() {
    let empty = FlashOccupancy::default();
    assert_eq!(empty.heap_bytes(), 34_560);
    let mut full = FlashOccupancy::default();
    for i in 0..4_000u32 {
        let f = f64::from(i);
        full.insert(
            (f * 0.7).sin() * 80.0,
            (f * 0.31).cos() * 179.0,
            at(22, 58, 0),
        );
    }
    assert_eq!(full.heap_bytes(), empty.heap_bytes());
}
