//! **Does a GMGSI raster ever paint nothing while the grid covers the view?**
//!
//! A raster that paints nothing is not a cheap no-op: `RasterizeOutput::
//! settle_blank` gives its buffer up, the reply reaches the pane as a `Blank`,
//! and `OverlayTextureCache::show_blank` **clears the pane** — the picture that
//! was on the glass is gone. So a blank is correct exactly when the layer has
//! nothing to draw in the view, and is a user-visible loss of data whenever it
//! does.
//!
//! GMGSI is the hard case and the reported one. It is
//! [`GridCoords::Separable`], it closes the globe in longitude, and its rows
//! *are* parallels — so it does **not** take `projection_window`'s
//! wrapping early return. It goes through the box fold into the grid's
//! longitude frame (`GridCoords::lon_frame` + `render::geo::box_lon_shift`) and
//! then through `lon_axis_bracket`'s angular search, which is the whole of its
//! window.
//!
//! # The discriminator
//!
//! Ground truth is the axes themselves. A separable grid's points span
//! `[lat_axis.min, lat_axis.max]` in latitude and — because this one closes the
//! globe — **every** longitude. So "the grid covers this view" is a latitude
//! question alone, and it is exact rather than approximate:
//!
//! * view entirely north of `lat_axis.max`, or entirely south of
//!   `lat_axis.min`: the grid has no point in it, a blank is **legitimate**.
//! * view overlapping the latitude band at all: the grid has points in it and
//!   a blank is **spurious** — the defect.
//!
//! Judged on the **view**, not on the rasterized coverage box. The coverage box
//! is the view plus a margin the user cannot see; whether the user loses the
//! picture is a question about the view.
//!
//! # The path
//!
//! The real one. `rasterize_gridded` over a `GriddedInput::Resident` at
//! GMGSI's real shape and real axis constants, then the funnel's own two
//! steps in the funnel's own order — premultiply, then `has_ink` — because
//! `settle_blank` states premultiplied bytes as its precondition and a
//! straight buffer may carry colour under a zero alpha.
//!
//! Every value in the grid is finite and in-domain, so nothing here can be
//! blank for want of data: a blank in this file is a blank of *geometry*.

use super::*;
use crate::render::gridded::{GridValues, ResidentGrid};
use squallar_source::product::FieldId;
use std::sync::Arc;

// ── The real grid ─────────────────────────────────────────────────────────

const NI: usize = 5000;
const NJ: usize = 3000;
/// The measured column step (`GridCoords::Separable`'s own doc): the file's
/// `geospatial_lon_resolution` says 0.0722, the array steps this.
const LON_STEP: f64 = 0.072_008_9;
/// The measured column 0 (`GridCoords::Separable`'s own doc).
const LON_0: f64 = 179.999_61;
/// `geospatial_lat_max` / `geospatial_lat_min` from the reference granule
/// (`gmgsi::decode::tests`).
const LAT_MAX: f64 = 72.715_40;
const LAT_MIN: f64 = -72.736_80;

/// GMGSI's real longitude axis: it sweeps east once around from `+179.99961`,
/// so the wrap into `[-180, 180)` falls between column 0 and column 1.
fn lon_axis() -> Vec<f64> {
    (0..NI)
        .map(|i| {
            let raw = LON_0 + i as f64 * LON_STEP;
            (raw + 180.0).rem_euclid(360.0) - 180.0
        })
        .collect()
}

/// Uniform in **Mercator y**, not in latitude — the property `Separable`'s doc
/// measures off the file.
fn lat_axis() -> Vec<f64> {
    let y_max = squallar_geo::lat_rad_to_mercator_y(LAT_MAX.to_radians());
    let y_min = squallar_geo::lat_rad_to_mercator_y(LAT_MIN.to_radians());
    (0..NJ)
        .map(|j| {
            let y = y_max + (y_min - y_max) * j as f64 / (NJ - 1) as f64;
            merc_y_to_lat(y)
        })
        .collect()
}

/// One grid for the whole sweep: 15 000 000 points is 60 MB, and building it
/// per leg would dominate the run.
fn grid() -> GriddedInput {
    static GRID: std::sync::OnceLock<Arc<ResidentGrid>> = std::sync::OnceLock::new();
    GriddedInput::Resident(
        GRID.get_or_init(|| {
            Arc::new(ResidentGrid {
                field: FieldId::from_static("GmgsiLongwaveIr"),
                ni: NI,
                nj: NJ,
                coords: crate::hrrr::GridCoords::Separable {
                    lat_axis: lat_axis(),
                    lon_axis: lon_axis(),
                    index: crate::hrrr::SeparableIndex::default(),
                },
                // Every point a real reading, so no cell can decline for want
                // of a value. `color_for` paints the whole 0..255 count range.
                values: GridValues::F32(vec![200.0; NI * NJ]),
            })
        })
        .clone(),
    )
}

// ── The viewport, transcribed from the shipped path ───────────────────────

/// `walkers::mercator::unproject_at_scale`, verbatim — longitude is linear in
/// x with **no wrap**, which is why a pan past the antimeridian yields a box
/// stated outside ±180.
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
fn viewport_bounds(centre: (f64, f64), zoom: f64, pane_w: f64, pane_h: f64) -> GeoBounds {
    let total = 2f64.powf(zoom) * 256.0;
    let (cx, cy) = project(centre.0, centre.1, total);
    let (nw_lat, nw_lon) = unproject(cx - pane_w / 2.0, cy - pane_h / 2.0, total);
    let (se_lat, se_lon) = unproject(cx + pane_w / 2.0, cy + pane_h / 2.0, total);
    GeoBounds {
        min_lat: nw_lat.min(se_lat),
        max_lat: nw_lat.max(se_lat),
        min_lon: nw_lon.min(se_lon),
        max_lon: nw_lon.max(se_lon),
    }
}

/// `squallar_egui::overlay_cache::OverlayTexturePlan::coverage`, verbatim —
/// latitude clamped to the Mercator limit, longitude not clamped.
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

/// `OVERDRAW_FRACTION` — the shipped 150 % oversample, one quarter a side.
const OVERDRAW: f64 = 0.25;

// ── One leg ───────────────────────────────────────────────────────────────

struct Reading {
    view: GeoBounds,
    cov: GeoBounds,
    win: IndexWindow,
    ink: bool,
    /// Whether the grid has any point inside the **view**.
    covered: bool,
    /// What the raster said about itself, once settled — `None` for a raster
    /// with ink in it.
    reason: Option<BlankReason>,
}

/// Rasterize one viewport through the real path and read the funnel's own
/// verdict off it.
fn probe(centre: (f64, f64), zoom: f64, pane_w: f64, pane_h: f64) -> Reading {
    let view = viewport_bounds(centre, zoom, pane_w, pane_h);
    let cov = coverage(&view, OVERDRAW);
    let width = (pane_w * (1.0 + 2.0 * OVERDRAW)) as u32;
    let height = (pane_h * (1.0 + 2.0 * OVERDRAW)) as u32;

    let input = grid();
    let win = input.window_for(&cov, width, height);
    let mut out = rasterize_gridded(&input, &cov, width, height);

    // The funnel's order: `JobOut::straight_rasters_mut` premultiplies, then
    // `JobOut::discard_blank_rasters` asks `has_ink`. Asking before the
    // premultiply would read colour under a zero alpha as ink.
    for pixel in out.rgba.as_chunks_mut::<4>().0 {
        let c = egui_color_premultiplied(pixel);
        pixel.copy_from_slice(&c);
    }
    let ink = has_ink(&out.rgba);
    // The funnel's own next step, so the reason read below is the one a pane
    // would actually be handed rather than the field before it was spent.
    out.settle_blank();
    let reason = out.blank_reason();

    // Exact for a separable grid that closes the globe: its points span the
    // whole turn in longitude and `[LAT_MIN, LAT_MAX]` in latitude.
    let covered = view.max_lat > LAT_MIN && view.min_lat < LAT_MAX;

    Reading {
        view,
        cov,
        win,
        ink,
        covered,
        reason,
    }
}

/// `egui::Color32::from_rgba_unmultiplied`, which is what
/// `squallar_worker::offload::premultiply_raster` calls. Reproduced rather
/// than reached, because that crate depends on this one.
fn egui_color_premultiplied(p: &[u8; 4]) -> [u8; 4] {
    let a = p[3] as u32;
    if a == 0 {
        return [0, 0, 0, 0];
    }
    if a == 255 {
        return *p;
    }
    let mul = |c: u8| ((c as u32 * a + 127) / 255) as u8;
    [mul(p[0]), mul(p[1]), mul(p[2]), p[3]]
}

fn report(label: &str, r: &Reading) -> String {
    format!(
        "{label}: view lat [{:.4}, {:.4}] lon [{:.4}, {:.4}]  \
         box lat [{:.4}, {:.4}] lon [{:.4}, {:.4}]  \
         window i {}..{} j {}..{} (area {})  ink {}  grid-covers-view {}  \
         reason {:?}",
        r.view.min_lat,
        r.view.max_lat,
        r.view.min_lon,
        r.view.max_lon,
        r.cov.min_lat,
        r.cov.max_lat,
        r.cov.min_lon,
        r.cov.max_lon,
        r.win.i0,
        r.win.i1,
        r.win.j0,
        r.win.j1,
        r.win.area(),
        r.ink,
        r.covered,
        r.reason.map(BlankReason::name),
    )
}

// ── The sweep ─────────────────────────────────────────────────────────────

/// A small pane: the window a raster projects is set by the *ground* the box
/// covers, not by the texture, so a small pane keeps the pixel loop cheap
/// without narrowing the question.
const PANE_W: f64 = 480.0;
const PANE_H: f64 = 320.0;

enum Edge {
    North,
    South,
}

/// The pane centre that puts one edge of the **view** exactly on `lat`.
///
/// Placing a sliver by offsetting the centre would put it at a different
/// distance from the grid's edge at every zoom, because a pane of fixed pixel
/// height spans 2.5 degrees at zoom 4 and 0.03 at zoom 12. The edge is the
/// thing being aimed at, so the edge is what is solved for — in the pixel
/// space `walkers` itself works in.
fn centre_for_edge(lat: f64, zoom: f64, pane_h: f64, edge: Edge) -> f64 {
    let total = 2f64.powf(zoom) * 256.0;
    let (_, py) = project(lat, 0.0, total);
    // Screen y grows southward, so the south edge sits below the centre.
    let centre_y = match edge {
        Edge::South => py - pane_h / 2.0,
        Edge::North => py + pane_h / 2.0,
    };
    unproject(total / 2.0, centre_y, total).0
}

/// **The question this file exists to answer.**
///
/// Pans across longitude — including the antimeridian seam and the two frames
/// `walkers` hands out either side of it — and across latitude, including
/// GMGSI's own coverage edges, at four zooms. A blank where the grid covers
/// the view is the defect; every one is printed with its viewport and its
/// computed window.
#[test]
fn a_gmgsi_raster_paints_something_wherever_the_grid_covers_the_view() {
    let mut legs: Vec<(String, (f64, f64), f64)> = Vec::new();

    // Longitude, all the way round, at two zooms. The seam is sampled from
    // both sides and in both of the frames `unproject` can state it in.
    for zoom in [4.0_f64, 6.0] {
        for lon in [
            -180.0,
            -150.0,
            -120.0,
            -90.0,
            -60.0,
            -30.0,
            0.0,
            30.0,
            60.0,
            90.0,
            120.0,
            150.0,
            179.0,
            179.999_61,
            179.99,
            -179.928_38,
            -179.0,
            180.5,
            200.0,
            -200.0,
        ] {
            legs.push((format!("lon {lon} @ z{zoom}"), (40.0, lon), zoom));
        }
    }

    // Latitude, including GMGSI's own edges, where a blank is legitimate.
    for zoom in [4.0_f64, 6.0] {
        for lat in [
            -84.0, -80.0, -74.0, -72.7368, -72.0, -60.0, -30.0, 0.0, 30.0, 60.0, 72.0, 72.7154,
            74.0, 80.0, 84.0,
        ] {
            legs.push((format!("lat {lat} @ z{zoom}"), (lat, -86.78), zoom));
        }
    }

    // The zoomed-out end, where the box is wider than the world.
    for zoom in [2.0_f64, 3.0] {
        for lon in [-86.78, 0.0, 179.999_61, 180.5] {
            legs.push((format!("wide lon {lon} @ z{zoom}"), (40.0, lon), zoom));
        }
    }

    // The zoomed-in end, where the window is a handful of cells and a bracket
    // that rounded the wrong way would leave nothing to project. Walked over
    // the seam as well as inland, because the angular search is the arm that
    // locates a column there.
    for zoom in [8.0_f64, 10.0, 12.0, 14.0] {
        for lon in [-86.78, 179.999_61, 179.99, -179.928_38, 180.5] {
            legs.push((format!("deep lon {lon} @ z{zoom}"), (40.0, lon), zoom));
        }
    }

    // **Barely covered.** The view's southern edge a sliver *below* the grid's
    // top row, and its northern edge a sliver *above* the bottom one — the
    // boundary between a blank that is right and one that is a disappearance,
    // approached from the covered side. `offset` is placed on the view's own
    // edge rather than on its centre, so a sliver is a sliver at every zoom.
    for zoom in [4.0_f64, 8.0, 12.0, 14.0] {
        for offset in [0.000_01_f64, 0.0001, 0.001, 0.01, 0.05, 0.5] {
            legs.push((
                format!("north sliver {offset} @ z{zoom}"),
                (
                    centre_for_edge(LAT_MAX - offset, zoom, PANE_H, Edge::South),
                    -86.78,
                ),
                zoom,
            ));
            legs.push((
                format!("south sliver {offset} @ z{zoom}"),
                (
                    centre_for_edge(LAT_MIN + offset, zoom, PANE_H, Edge::North),
                    -86.78,
                ),
                zoom,
            ));
        }
    }

    let mut blanks = 0usize;
    let mut spurious: Vec<String> = Vec::new();
    // Collected rather than asserted per leg: a sweep that dies on its first
    // violation reports one viewport where the interesting output is the
    // *shape* of the set — which zooms, which longitudes, which edge.
    let mut misreasoned: Vec<String> = Vec::new();
    let total = legs.len();

    for (label, centre, zoom) in legs {
        let r = probe(centre, zoom, PANE_W, PANE_H);
        if !r.ink {
            blanks += 1;
            if r.covered {
                spurious.push(report(&label, &r));
            } else {
                println!("legitimate blank — {}", report(&label, &r));
            }
            // **The reason has to agree with the ground truth**, or the
            // always-on counter that reports it is decorative. `covered` is
            // computed here from the axes; the reason is computed in the
            // rasterizer from what its own loop saw. Two independent
            // derivations of one fact, asserted equal.
            let reason = r.reason.expect("a settled blank names its reason");
            if reason.clears_covered_ground() != r.covered {
                misreasoned.push(format!(
                    "{} — called {:?} (clears-covered-ground {}), axes say \
                     grid-covers-view {}",
                    report(&label, &r),
                    reason,
                    reason.clears_covered_ground(),
                    r.covered,
                ));
            }
        } else if r.reason.is_some() {
            misreasoned.push(format!(
                "{} — a raster WITH ink named a blank reason",
                report(&label, &r)
            ));
        }
    }

    println!(
        "\nGMGSI blank rate over a realistic pan: {blanks}/{total} \
         ({:.1} %) of rasters painted nothing; {} of them spurious.",
        100.0 * blanks as f64 / total as f64,
        spurious.len()
    );

    assert!(
        spurious.is_empty(),
        "{} raster(s) painted nothing while the GMGSI grid covers the view — \
         each one is a picture that disappears from under the user:\n{}",
        spurious.len(),
        spurious.join("\n")
    );
    // **The second claim, and the one the always-on counter rests on.** The
    // sweep above proves the rasters are right; this proves the raster's own
    // account of itself is right, against a discriminator computed here from
    // the axes rather than from anything the rasterizer saw. A reason nobody
    // checks is a field, not telemetry.
    assert!(
        misreasoned.is_empty(),
        "{} raster(s) disagreed with the axes about why they were blank — \
         `overlay blanks:` would report these wrongly and nothing would \
         say so:\n{}",
        misreasoned.len(),
        misreasoned.join("\n")
    );
}

/// **The fold is a no-op for this grid**, which a previous lane asserted and
/// this checks: `lon_axis_bracket` locates each endpoint angularly, and an
/// angular distance is invariant under a whole turn, so the box carried into
/// the grid's frame by `box_lon_shift` must yield the identical window.
///
/// Stated as a property over the same pan the sweep above walks, because the
/// claim is about every box and not about one.
#[test]
fn carrying_the_box_a_whole_turn_leaves_a_gmgsi_window_unchanged() {
    let input = grid();
    let (ni, nj) = input.shape();
    let coords = input.coords();

    for lon in [-180.0_f64, -90.0, 0.0, 90.0, 179.0, 179.999_61, 180.5] {
        for zoom in [3.0_f64, 5.0, 7.0] {
            let view = viewport_bounds((40.0, lon), zoom, PANE_W, PANE_H);
            let here = coverage(&view, OVERDRAW);
            let turned = GeoBounds {
                min_lon: here.min_lon + 360.0,
                max_lon: here.max_lon + 360.0,
                ..here
            };
            let a = projection_window(coords, ni, nj, &here, 600, 400);
            let b = projection_window(coords, ni, nj, &turned, 600, 400);
            assert_eq!(
                a, b,
                "lon {lon} @ z{zoom}: the same ground a turn apart took two windows"
            );
        }
    }
}

/// **The other blank, and the one the counter exists to separate from the
/// first.** A mosaic with a hole in it — a satellite out, a channel not yet
/// composited — is blank over ground the grid does cover, and it clears the
/// pane exactly as the polar blank does.
///
/// Both arms in one test on purpose. A reason counter that only ever answered
/// [`BlankReason::OutsideCoverage`] would satisfy the sweep above completely
/// and say nothing: `clears_covered_ground` would read `false` on every leg,
/// including the legs where it must read `true`. The pair is what makes the
/// discriminator falsifiable — see `distinct_blank_reasons` on the ledger's
/// `Totals`, which is the same conjunct one layer up.
#[test]
fn a_hole_in_the_mosaic_is_a_different_blank_from_a_pan_off_the_grid() {
    // A grid with no readings at all, at the same shape and the same axes:
    // every cell is inside the window, and not one of them is paintable.
    let hollow = GriddedInput::Resident(Arc::new(ResidentGrid {
        field: FieldId::from_static("GmgsiLongwaveIr"),
        ni: NI,
        nj: NJ,
        coords: crate::hrrr::GridCoords::Separable {
            lat_axis: lat_axis(),
            lon_axis: lon_axis(),
            index: crate::hrrr::SeparableIndex::default(),
        },
        // `render::gridded::color_for`'s non-finite guard paints NaN fully
        // transparent, which is how an absent GMGSI point is stored.
        values: GridValues::F32(vec![f32::NAN; NI * NJ]),
    }));

    // Mid-latitude, well inside the grid's own band: the view is covered by
    // any reading of the axes.
    let view = viewport_bounds((40.0, -86.78), 5.0, PANE_W, PANE_H);
    let cov = coverage(&view, OVERDRAW);
    let width = (PANE_W * (1.0 + 2.0 * OVERDRAW)) as u32;
    let height = (PANE_H * (1.0 + 2.0 * OVERDRAW)) as u32;

    let mut out = rasterize_gridded(&hollow, &cov, width, height);
    out.settle_blank();
    let reason = out
        .blank_reason()
        .expect("a mosaic with no readings in it paints nothing");
    assert_eq!(
        reason,
        BlankReason::NoDataInWindow,
        "a data gap over covered ground was filed as {reason:?}",
    );
    assert!(
        reason.clears_covered_ground(),
        "a data gap clears a pane the layer still covers, so it must count \
         against `blanks_over_covered_ground`",
    );

    // The control, the same viewport and the same code, with readings in it:
    // the discriminator is the data and not the geometry.
    let mut inked = rasterize_gridded(&grid(), &cov, width, height);
    inked.settle_blank();
    assert_eq!(
        inked.blank_reason(),
        None,
        "the same viewport over a grid WITH readings must not be blank at \
         all — otherwise the test above proves nothing about the data",
    );

    // And the polar arm, so the two verdicts are taken from one run.
    let polar = probe((84.0, -86.78), 5.0, PANE_W, PANE_H);
    assert_eq!(
        polar.reason,
        Some(BlankReason::OutsideCoverage),
        "a view past the top of GMGSI's latitude axis is the one blank that \
         is correct, and must not be counted as covered ground",
    );
    assert!(!polar.reason.expect("blank").clears_covered_ground());
}
