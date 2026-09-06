//! **The GMGSI antimeridian seam at whole-world zoom.**
//!
//! [`MercatorBounds::project`] applies no wrap shift — "Longitude is mapped
//! linearly and no shift is applied here, so each caller states its own frame."
//! GMGSI's longitude axis closes the globe *between column 0 and column 1*:
//! column 0 is `+179.99961` and column 1 is `179.99961 + 0.0720089 = 180.07162`,
//! stored wrapped as `-179.92838`. The two are one ordinary 0.072-degree cell
//! apart on the ground and 359.928 degrees apart as numbers.
//!
//! `rasterize_gridded` sizes every cell from its neighbours' **projected
//! pixels** (`dx_left`/`dx_right`, 0.55 of the neighbour spacing). Where the box
//! is wide enough to hold both `+180` and `-180` — which is exactly a viewport
//! wider than the world — those two columns project to two far-apart pixels and
//! the cell between them is sized from that distance.
//!
//! The viewport under test is the one a user's laptop was on: one pane, zoom
//! 3.326757482253017, centre 41.498847583948724 / -86.78362446681776, 2878 x
//! 1651 px. The pane is 112.0 % of the 2568.8 px world, so the visible span is
//! 403.4 degrees and the rasterised box (after 25 % overdraw a side) is 605.2.
//!
//! Two controls, both against the same code path:
//!
//! * **Seam moved, everything else identical** — a global axis that ascends
//!   `-180 -> +180` without wrapping mid-axis. `wraps_longitude()` is still
//!   true, `projection_window` still declines to narrow, the same 15 000 000
//!   cells are drawn. Only the discontinuity has moved off an adjacent index
//!   pair.
//! * **Zoom 5.0**, where the world is 8192 px, the pane is 35 % of it, and the
//!   box does not contain the antimeridian at all.

use super::*;
use crate::render::gridded::{GridValues, ResidentGrid};
use squallar_source::product::FieldId;
use std::sync::Arc;

// ── The frozen viewport, and the two transcriptions that reach it ──────────

/// `walkers::mercator::unproject_at_scale`, verbatim. `pub(crate)` there;
/// reached in the app through `walkers::Projector::unproject`. **Longitude is
/// linear in x with no wrap**, which is why a pane wider than the world yields
/// a box holding both `+180` and `-180`.
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

const PANE_W: f64 = 2878.0;
const PANE_H: f64 = 1651.0;
const CENTRE: (f64, f64) = (41.498_847_583_948_724, -86.783_624_466_817_76);
const FROZEN_ZOOM: f64 = 3.326_757_482_253_017;
const CONTROL_ZOOM: f64 = 5.0;
/// `OVERDRAW_FRACTION` — the shipped 150 % oversample, one quarter a side.
const OVERDRAW: f64 = 0.25;

// ── The GMGSI grid, at its real shape ─────────────────────────────────────

const NI: usize = 5000;
const NJ: usize = 3000;
/// The measured column step: `geospatial_lon_resolution` says 0.0722, the array
/// steps this (`GridCoords::Separable`'s own doc).
const LON_STEP: f64 = 0.072_008_9;
/// The measured column 0 (`GridCoords::Separable`'s own doc).
const LON_0: f64 = 179.999_61;
/// `geospatial_lat_max` / `geospatial_lat_min` from the reference granule
/// (`gmgsi::decode::tests`).
const LAT_MAX: f64 = 72.715_40;
const LAT_MIN: f64 = -72.736_80;

/// GMGSI's real longitude axis: it sweeps east once around starting at
/// `+179.99961`, so the wrap into `[-180, 180)` falls between **column 0 and
/// column 1**. Column 1 is `-179.92838`, which is what the `Separable` doc
/// records off the file.
fn wrapping_lon_axis() -> Vec<f64> {
    (0..NI)
        .map(|i| {
            let raw = LON_0 + i as f64 * LON_STEP;
            (raw + 180.0).rem_euclid(360.0) - 180.0
        })
        .collect()
}

/// **The control axis.** The same 5000 columns of the same step covering the
/// same turn, but anchored so the axis ascends `-180 -> +180` and the wrap
/// falls between the *last* column and the first — an index pair no cell
/// straddles. `wraps_longitude()` is still true and the code path is identical.
fn ascending_lon_axis() -> Vec<f64> {
    (0..NI)
        .map(|i| -180.0 + 0.001 + i as f64 * LON_STEP)
        .collect()
}

/// Uniform in **Mercator y**, not in latitude — the property `Separable`'s doc
/// measures off the file (a constant -0.628397 of `y` per 500 rows; this
/// construction gives -0.6284 over the same interval).
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

/// Values that make each column identifiable in the output: column 0 paints
/// white (count 255), column 1 near-black (count 0), column 2 mid-grey
/// (count 128), every other column NaN, which
/// `render::gridded::color_for`'s non-finite guard paints as fully
/// transparent. The painted pixels of each colour are then exactly the rect
/// that column filled.
fn tagged_values() -> GridValues {
    let mut v = vec![f32::NAN; NI * NJ];
    for j in 0..NJ {
        v[j * NI] = 255.0;
        v[j * NI + 1] = 0.0;
        v[j * NI + 2] = 128.0;
    }
    GridValues::F32(v)
}

fn grid(lon_axis: Vec<f64>, values: GridValues) -> GriddedInput {
    GriddedInput::Resident(Arc::new(ResidentGrid {
        field: FieldId::from_static("GmgsiLongwaveIr"),
        ni: NI,
        nj: NJ,
        coords: crate::hrrr::GridCoords::Separable {
            lat_axis: lat_axis(),
            lon_axis,
        },
        values,
    }))
}

/// The pixels of one exact RGB in the output: how many, and the x range they
/// span.
fn extent_of(rgba: &[u8], width: u32, rgb: [u8; 3]) -> (usize, Option<(u32, u32)>) {
    let mut count = 0usize;
    let (mut lo, mut hi) = (u32::MAX, 0u32);
    for (n, px) in rgba.chunks_exact(4).enumerate() {
        if px[3] != 0 && px[0] == rgb[0] && px[1] == rgb[1] && px[2] == rgb[2] {
            count += 1;
            let x = n as u32 % width;
            lo = lo.min(x);
            hi = hi.max(x);
        }
    }
    (count, (count > 0).then_some((lo, hi)))
}

struct Leg {
    label: &'static str,
    zoom: f64,
    wrapping: bool,
}

fn run(leg: &Leg) {
    let world = 2f64.powf(leg.zoom) * 256.0;
    let view = viewport_bounds(leg.zoom);
    let cov = coverage(&view, OVERDRAW);
    let w = (PANE_W * (1.0 + 2.0 * OVERDRAW)) as u32;
    let h = (PANE_H * (1.0 + 2.0 * OVERDRAW)) as u32;

    let lon_axis = if leg.wrapping {
        wrapping_lon_axis()
    } else {
        ascending_lon_axis()
    };

    println!("\n================ {} ================", leg.label);
    println!(
        "zoom {}  world {world:.1} px  pane/world {:.4}",
        leg.zoom,
        PANE_W / world
    );
    println!(
        "box lon [{:.4}, {:.4}]  span {:.4}   texture {w} x {h}",
        cov.min_lon,
        cov.max_lon,
        cov.max_lon - cov.min_lon
    );
    println!(
        "lon_axis[0..3] = [{:.5}, {:.5}, {:.5}]  (|d(0,1)| = {:.5} deg)",
        lon_axis[0],
        lon_axis[1],
        lon_axis[2],
        (lon_axis[1] - lon_axis[0]).abs()
    );

    // The real projection, on the real box. `project` applies no wrap shift.
    let mb = MercatorBounds::from_geo(&cov);
    let mid = NJ / 2;
    let lat_mid = lat_axis()[mid];
    let px: Vec<f32> = (0..3)
        .map(|i| mb.project(lat_mid, lon_axis[i], w as f32, h as f32).0)
        .collect();
    println!(
        "MercatorBounds::project x of columns 0,1,2 at row {mid}: \
         {:.1}, {:.1}, {:.1}  ->  |x1-x0| = {:.1} px, |x2-x1| = {:.1} px",
        px[0],
        px[1],
        px[2],
        (px[1] - px[0]).abs(),
        (px[2] - px[1]).abs()
    );

    let input = grid(lon_axis, tagged_values());
    let win = input.window_for(&cov, w, h);
    println!(
        "window i [{}, {})  j [{}, {})  area {}",
        win.i0,
        win.i1,
        win.j0,
        win.j1,
        win.area()
    );

    let out = rasterize_gridded(&input, &cov, w, h);
    let total_px = (w as usize) * (h as usize);
    for (name, rgb) in [
        ("column 0 (count 255, white)", [0xff, 0xff, 0xff]),
        ("column 1 (count 0,   black)", [0x0a, 0x0a, 0x0a]),
        ("column 2 (count 128, mid)  ", [0x89, 0x89, 0x89]),
    ] {
        let (count, span) = extent_of(&out.rgba, w, rgb);
        match span {
            Some((lo, hi)) => println!(
                "  {name}: {count:>10} px  x [{lo}, {hi}]  width {:>5} px  \
                 ({:.2} % of the picture)",
                hi - lo + 1,
                100.0 * count as f64 / total_px as f64
            ),
            None => println!("  {name}: {count:>10} px  (nothing painted)"),
        }
    }
    // Column 2's exact mid-grey is whatever the ramp gives count 128; report
    // every distinct painted colour so no column is missed by a wrong guess.
    let mut seen: Vec<([u8; 3], usize)> = Vec::new();
    for px in out.rgba.chunks_exact(4) {
        if px[3] == 0 {
            continue;
        }
        let key = [px[0], px[1], px[2]];
        match seen.iter_mut().find(|(c, _)| *c == key) {
            Some((_, n)) => *n += 1,
            None => seen.push((key, 1)),
        }
    }
    seen.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("  distinct painted colours (rgb, count):");
    for (c, n) in seen.iter().take(6) {
        println!("    #{:02x}{:02x}{:02x}  {n:>10}", c[0], c[1], c[2]);
    }
    let painted: usize = seen.iter().map(|(_, n)| n).sum();
    println!(
        "  painted total {painted} of {total_px} ({:.2} %)",
        100.0 * painted as f64 / total_px as f64
    );
}

/// **The experiment.** Three columns paint and 4997 do not, so every painted
/// pixel in the output is a pixel one of those three cells filled.
#[test]
fn how_wide_a_rect_the_seam_columns_fill_at_the_frozen_viewport() {
    for leg in [
        Leg {
            label: "FROZEN VIEWPORT, real GMGSI axis (seam between columns 0 and 1)",
            zoom: FROZEN_ZOOM,
            wrapping: true,
        },
        Leg {
            label: "CONTROL A: same viewport, seam moved off the 0/1 index pair",
            zoom: FROZEN_ZOOM,
            wrapping: false,
        },
        Leg {
            label: "CONTROL B: zoom 5.0, real GMGSI axis, world not wrapped",
            zoom: CONTROL_ZOOM,
            wrapping: true,
        },
    ] {
        run(&leg);
    }
}

/// The same picture as the app would really draw it: **every column carries a
/// count**, so the seam cell's rect overwrites correct columns rather than
/// painting into empty space. Column 0 is tagged white and column 1 mid-grey;
/// every other column is count 0. Whatever survives of those two tags is a
/// region of the picture showing the wrong column's brightness.
#[test]
fn the_seam_cells_overwrite_a_full_height_stripe_of_correct_columns() {
    let cov = coverage(&viewport_bounds(FROZEN_ZOOM), OVERDRAW);
    let w = (PANE_W * 1.5) as u32;
    let h = (PANE_H * 1.5) as u32;

    for (label, axis) in [
        (
            "real GMGSI axis (seam between columns 0 and 1)",
            wrapping_lon_axis(),
        ),
        ("control: seam off the 0/1 index pair", ascending_lon_axis()),
    ] {
        let mut v = vec![0.0f32; NI * NJ];
        for j in 0..NJ {
            v[j * NI] = 255.0;
            v[j * NI + 1] = 128.0;
        }
        let input = grid(axis, GridValues::F32(v));
        let out = rasterize_gridded(&input, &cov, w, h);
        println!("\n{label}");
        let mut total = 0usize;
        for (name, rgb) in [
            ("column 0 (white)", [0xff, 0xff, 0xff]),
            ("column 1 (mid)  ", [0x85, 0x85, 0x85]),
        ] {
            let (count, span) = extent_of(&out.rgba, w, rgb);
            total += count;
            println!(
                "  {name}: {count:>9} px  x {span:?}  ({:.3} % of the {w} x {h} picture)",
                100.0 * count as f64 / (w as f64 * h as f64)
            );
        }
        println!(
            "  the two seam columns hold {total} px = {:.2} % of the picture",
            100.0 * total as f64 / (w as f64 * h as f64)
        );
    }
}

/// **The failing assertion.** One GMGSI cell is 0.0720089 degrees of longitude.
/// At the frozen viewport the picture is 4317 px over a 605.05-degree box, so
/// one cell is `0.0720089 / 605.05 * 4317 = 0.51` px. No cell may paint a rect
/// wider than a handful of pixels. Columns 0 and 1 paint **1668 px and 1412
/// px**, because `MercatorBounds::project` gives them x values 2568 px apart
/// and `rasterize_gridded`'s `dx_left`/`dx_right` size the cell from that
/// distance.
///
/// FAILS on main. `squallar-overlays/src/render/rasterize.rs:2155-2168` is the
/// unshifted term.
#[test]
fn a_gmgsi_cell_never_paints_wider_than_the_cell_is() {
    let cov = coverage(&viewport_bounds(FROZEN_ZOOM), OVERDRAW);
    let w = (PANE_W * 1.5) as u32;
    let h = (PANE_H * 1.5) as u32;
    let one_cell_px = LON_STEP / (cov.max_lon - cov.min_lon) * w as f64;
    // Ten cells of slack: the 0.55 overlap, the 0.5 px floor, and rounding.
    let ceiling = (one_cell_px * 10.0).max(8.0);

    let input = grid(wrapping_lon_axis(), tagged_values());
    let out = rasterize_gridded(&input, &cov, w, h);
    for (col, rgb) in [
        (0usize, [0xff, 0xff, 0xff]),
        (1, [0x0a, 0x0a, 0x0a]),
        (2, [0x85, 0x85, 0x85]),
    ] {
        let (count, span) = extent_of(&out.rgba, w, rgb);
        let width = span.map_or(0, |(lo, hi)| hi - lo + 1);
        assert!(
            (width as f64) <= ceiling,
            "GMGSI column {col} is {one_cell_px:.2} px of longitude wide but painted a \
             rect {width} px wide ({count} pixels, {:.2} % of the {w} x {h} picture); \
             ceiling is {ceiling:.1} px",
            100.0 * count as f64 / (w as f64 * h as f64)
        );
    }
}

/// **The near arm.** The same assertion at a regional zoom, where the box does
/// not reach either representation of the anti-meridian. This must be green
/// *with and without* the fix: it is what shows the gate above is not passing
/// because the ceiling is loose or because nothing paints, and it is the
/// healthy input that most resembles the defect — the very same wrapping axis,
/// the very same seam, just a box that does not hold it.
#[test]
fn a_regional_view_of_the_same_wrapping_grid_is_green_either_way() {
    // Zoom 7 over the frozen centre: the world is 32768 px, the pane 8.8 % of
    // it, and the box spans 47 degrees around -86.8 — nowhere near +/-180.
    let cov = coverage(&viewport_bounds(7.0), OVERDRAW);
    assert!(
        cov.min_lon > -180.0 && cov.max_lon < 180.0,
        "the near arm must not reach the seam, box is [{}, {}]",
        cov.min_lon,
        cov.max_lon
    );
    let w = (PANE_W * 1.5) as u32;
    let h = (PANE_H * 1.5) as u32;
    let one_cell_px = LON_STEP / (cov.max_lon - cov.min_lon) * w as f64;
    let ceiling = (one_cell_px * 10.0).max(8.0);

    // Every column paints, so this is a full picture and the assertion has
    // something to be wrong about.
    let input = grid(
        wrapping_lon_axis(),
        GridValues::F32(vec![200.0f32; NI * NJ]),
    );
    let out = rasterize_gridded(&input, &cov, w, h);
    let painted = out.rgba.chunks_exact(4).filter(|px| px[3] != 0).count();
    assert!(
        painted > (w as usize) * (h as usize) / 2,
        "the near arm painted only {painted} px, so it proves nothing"
    );

    // The seam columns must be absent from the picture entirely: their two
    // representations both sit outside this box.
    let input = grid(wrapping_lon_axis(), tagged_values());
    let out = rasterize_gridded(&input, &cov, w, h);
    for (col, rgb) in [
        (0usize, [0xff, 0xff, 0xff]),
        (1, [0x0a, 0x0a, 0x0a]),
        (2, [0x85, 0x85, 0x85]),
    ] {
        let (count, span) = extent_of(&out.rgba, w, rgb);
        let width = span.map_or(0, |(lo, hi)| hi - lo + 1);
        assert!(
            (width as f64) <= ceiling,
            "GMGSI column {col} painted a rect {width} px wide ({count} px) at a \
             regional zoom; one cell is {one_cell_px:.4} px, ceiling {ceiling:.1}"
        );
    }
}
