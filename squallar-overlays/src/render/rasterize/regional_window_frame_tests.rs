//! The window on a **regional** grid, asked for from a box written a whole
//! turn up.
//!
//! The app works in one continuous longitude frame (`seam_frame_tests`):
//! `walkers::Projector::unproject` folds nothing and the map centre is never
//! normalised, so a user who pans east across the antimeridian and keeps going
//! reaches the same ground with its box written a turn up — `230..300` for the
//! ground at `-130..-60`. [`rasterize_gridded`] carries every grid *point* to
//! the representation nearest the box before it projects it
//! (`MercatorBounds::nearest_lon`, unconditionally), so the points would paint
//! — if they were projected. Which points are projected is
//! [`projection_window`]'s answer, and that reads the box's *edges* against
//! the grid's stored longitudes through [`GridCoords::index_bounds`]: on a
//! regular grid, `(min_lon - lon0) / dlon`. MRMS CONUS has `lon0 = -129.995`,
//! so a box at `230..300` lands at column 36,000 of 7,000, clamps to `ni`, and
//! the window is empty: nothing is projected, nothing paints, and the mosaic
//! is gone until the user pans back the way they came. The separable arm's
//! regional search (`axis_bracket`) reads the same raw numbers and fails the
//! same way.
//!
//! The Lambert arm is the control: `LambertConformalConic::theta` folds
//! `lon - lon0` into a turn before it scales (`hrrr/lambert.rs`), so HRRR's
//! window is one window from either spelling and always was.
//!
//! Pinned here: the window from the in-frame box and from the box a turn up
//! are one window, and not empty; the raster from both paints the same pixel
//! count; and an in-frame box's window is bit-identical to what it was before
//! the fold — the fold is the identity where there is nothing to fold.

use super::lambert_fixture::lambert_grid;
use super::*;
use crate::hrrr::{GridCoords, HrrrGridData, ModelParameter};
use std::sync::Arc;

/// MRMS CONUS, section 3 of a live granule as
/// `mrms::decode::tests::the_grid_is_the_regular_arm_built_from_section_three`
/// pins it: 7000 x 3500 at 0.01 deg, scanning south from 54.995 N and east
/// from -129.995 (the octets say 230.005; `mrms::decode` folds `lon0` alone).
const MRMS_NI: usize = 7000;
const MRMS_NJ: usize = 3500;
const MRMS_LAT0: f64 = 54.995;
const MRMS_LON0: f64 = -129.995;
const MRMS_DLAT: f64 = -0.01;
const MRMS_DLON: f64 = 0.01;

/// The transcription above, anchored to the crate's own statements of the
/// mosaic so it cannot drift from the source it stands for.
fn mrms_coords() -> GridCoords {
    let east = MRMS_LON0 + (MRMS_NI - 1) as f64 * MRMS_DLON;
    assert!(
        crate::mrms::MRMS_DOMAIN_LON.contains(&MRMS_LON0)
            && crate::mrms::MRMS_DOMAIN_LON.contains(&east),
        "the transcribed grid {MRMS_LON0}..{east} must sit inside the envelope MRMS declares, {:?}",
        crate::mrms::MRMS_DOMAIN_LON
    );
    assert_eq!(
        MRMS_NI * MRMS_NJ * crate::render::gridded::ScaledU16::ELEMENT_BYTES,
        crate::mrms::CONUS_GRID_BYTES,
        "the transcribed shape must be the one the mosaic's byte budget is stated in"
    );
    let coords = GridCoords::Regular {
        lat0: MRMS_LAT0,
        lon0: MRMS_LON0,
        dlat: MRMS_DLAT,
        dlon: MRMS_DLON,
        ni: MRMS_NI,
        nj: MRMS_NJ,
        scan_mode: 0,
    };
    assert!(
        !coords.wraps_longitude(),
        "a 70-degree mosaic does not close the globe, and this file is about the arm that does not"
    );
    coords
}

/// A regional separable grid: half-degree rows from 50 N, quarter-degree
/// columns from 120 W — the shape `wrapping_window_tests` pins the window of.
fn regional_separable() -> GridCoords {
    let coords = GridCoords::Separable {
        lat_axis: (0..100).map(|j| 50.0 - j as f64 * 0.5).collect(),
        lon_axis: (0..200).map(|i| -120.0 + i as f64 * 0.25).collect(),
        index: crate::hrrr::SeparableIndex::default(),
    };
    assert!(!coords.wraps_longitude());
    coords
}

fn box_of(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> GeoBounds {
    GeoBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    }
}

/// The same ground, written one turn east — what the box reads after one
/// circuit of the globe.
fn a_turn_up(b: &GeoBounds) -> GeoBounds {
    GeoBounds {
        min_lon: b.min_lon + 360.0,
        max_lon: b.max_lon + 360.0,
        ..*b
    }
}

/// A degree over Oklahoma, in frame; the box the MRMS decode tests window.
const OKLAHOMA: GeoBounds = GeoBounds {
    min_lat: 35.0,
    max_lat: 36.0,
    min_lon: -98.0,
    max_lon: -97.0,
};

fn window(coords: &GridCoords, ni: usize, nj: usize, b: &GeoBounds, w: u32, h: u32) -> IndexWindow {
    super::projection_window(coords, ni, nj, b, w, h)
}

/// One window from either spelling: the verdict, with the figures.
fn one_window(
    label: &str,
    coords: &GridCoords,
    ni: usize,
    nj: usize,
    here: &GeoBounds,
    w: u32,
    h: u32,
) -> IndexWindow {
    let up = a_turn_up(here);
    let win_here = window(coords, ni, nj, here, w, h);
    let win_up = window(coords, ni, nj, &up, w, h);
    println!(
        "{label}: box {:.3}..{:.3} -> {win_here:?}; a turn up {:.3}..{:.3} -> {win_up:?}",
        here.min_lon, here.max_lon, up.min_lon, up.max_lon
    );
    assert!(
        !win_here.is_empty(),
        "{label}: non-triviality — the in-frame box must window something, got {win_here:?}"
    );
    assert!(
        !win_up.is_empty(),
        "{label}: from the box a turn up ({:.3}..{:.3}) the window is EMPTY — \
         i0 == i1 == {} of ni = {ni} ({win_up:?}) — where the same ground in frame \
         ({:.3}..{:.3}) windows {win_here:?}. index_bounds read the box's edges against \
         the grid's own longitudes without carrying the box into the grid's frame, so \
         nothing is projected and the layer vanishes after one circuit of the globe.",
        up.min_lon,
        up.max_lon,
        win_up.i0,
        here.min_lon,
        here.max_lon,
    );
    assert_eq!(
        win_up, win_here,
        "{label}: the box a turn up ({:.3}..{:.3}) and the same ground in frame \
         ({:.3}..{:.3}) must name one window",
        up.min_lon, up.max_lon, here.min_lon, here.max_lon,
    );
    win_here
}

/// **MRMS, the mosaic that vanishes.** The box a turn up windows nothing on
/// main: `(230 - (-129.995)) / 0.01` is column 36,000 of 7,000.
#[test]
fn the_mrms_mosaic_windows_the_same_cells_from_a_box_written_a_turn_up() {
    let coords = mrms_coords();
    one_window("MRMS CONUS", &coords, MRMS_NI, MRMS_NJ, &OKLAHOMA, 512, 512);
    // And at the pane the seam probe uses, over the whole domain and past it.
    let wide = box_of(20.0, 55.0, -135.0, -55.0);
    one_window(
        "MRMS CONUS, the whole domain",
        &coords,
        MRMS_NI,
        MRMS_NJ,
        &wide,
        2878,
        1651,
    );
}

/// **The regional separable arm reads the same raw numbers.** `axis_bracket`
/// places an edge off the end of the axis *at* the end, so the box a turn up
/// is not empty but two columns at the grid's east edge — which the box does
/// not show, so the raster paints nothing all the same.
#[test]
fn a_regional_separable_grid_windows_the_same_cells_from_a_box_written_a_turn_up() {
    let coords = regional_separable();
    one_window(
        "regional separable",
        &coords,
        200,
        100,
        &box_of(30.0, 35.0, -100.0, -95.0),
        512,
        512,
    );
}

/// **The control: the Lambert arm folds on its own** —
/// `LambertConformalConic::theta` folds `lon - lon0` into a turn before it
/// scales — so HRRR answers one window from either spelling with or without
/// the fold upstream, and the fold must leave that window exactly where
/// `wrapping_window_tests::a_grid_that_does_not_wrap_keeps_the_window_it_had`
/// pins it.
#[test]
fn the_lambert_arm_answers_one_window_from_either_spelling_on_its_own() {
    let hrrr = lambert_grid(1799, 1059, 0b0100_0000);
    assert!(!hrrr.coords.wraps_longitude());
    let win = one_window(
        "HRRR CONUS",
        &hrrr.coords,
        hrrr.ni,
        hrrr.nj,
        &box_of(29.5, 41.5, -103.5, -91.5),
        1024,
        768,
    );
    assert_eq!(
        win,
        IndexWindow {
            i0: 700,
            i1: 1098,
            j0: 190,
            j1: 649,
        },
        "the window the wrapping suite pins for this box"
    );
}

/// **End to end: the mosaic paints the same pixels from either spelling.**
/// Three cells tagged inside a degree over Oklahoma on the real 7000 x 3500
/// shape; from the box a turn up, main paints zero of them.
#[test]
fn the_mrms_mosaic_paints_the_same_pixel_count_from_a_box_written_a_turn_up() {
    let coords = mrms_coords();
    // Zeroed pages, untouched until read, so 98 MB of `f32` costs nothing
    // but the cells written; CAPE below 250 paints nothing.
    let mut values = vec![0.0f32; MRMS_NI * MRMS_NJ];
    // Column `i` is at `lon0 + i * dlon`, row `j` at `lat0 + j * dlat`:
    // (3250, 1950) is -97.495 / 35.495, inside the box by half a degree.
    let tagged = [(3250usize, 1950usize), (3220, 1920), (3280, 1985)];
    for &(i, j) in &tagged {
        let (lat, lon) = coords.at(j * MRMS_NI + i).expect("in range");
        assert!(
            OKLAHOMA.contains_point(lat, lon),
            "non-triviality: cell ({i}, {j}) at {lat}/{lon} must be inside {OKLAHOMA:?}"
        );
        values[j * MRMS_NI + i] = 2_500.0;
    }
    let parameter = ModelParameter::SurfaceBasedCape;
    let grid = HrrrGridData {
        parameter,
        values,
        coords,
        ni: MRMS_NI,
        nj: MRMS_NJ,
        bounds: box_of(
            MRMS_LAT0 + (MRMS_NJ - 1) as f64 * MRMS_DLAT,
            MRMS_LAT0,
            MRMS_LON0,
            MRMS_LON0 + (MRMS_NI - 1) as f64 * MRMS_DLON,
        ),
        ref_time: chrono::NaiveDate::from_ymd_opt(2026, 9, 6)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        forecast_hour: 0,
        visible_points: tagged.len(),
        value_range: Some((0.0, 2_500.0)),
    };
    let input = GriddedInput::Whole(Arc::new(grid));
    let painted = |b: &GeoBounds| {
        super::rasterize_gridded(&input, b, 512, 512)
            .rgba
            .chunks_exact(4)
            .filter(|px| px[3] > 0)
            .count()
    };
    let here = painted(&OKLAHOMA);
    let up = painted(&a_turn_up(&OKLAHOMA));
    println!("MRMS CONUS raster: {here} px in frame, {up} px from the box a turn up");
    assert!(
        here > 0,
        "non-triviality: the tagged cells must paint in frame"
    );
    assert_eq!(
        up,
        here,
        "from the box a turn up ({:.3}..{:.3}) the mosaic paints {up} px where the same \
         ground in frame paints {here} px: the projection window was empty and no point \
         was projected",
        OKLAHOMA.min_lon + 360.0,
        OKLAHOMA.max_lon + 360.0,
    );
}

/// **The fold is the identity in frame.** These windows were read off main at
/// `d331f621`, before the fold landed, and a box already in its grid's frame
/// must get exactly them: the fold moves a box by a whole turn or not at all.
#[test]
fn an_in_frame_box_gets_the_window_it_had_before_the_fold() {
    let mrms = mrms_coords();
    let separable = regional_separable();
    let hrrr = lambert_grid(1799, 1059, 0b0100_0000);
    let cases = [
        (
            "MRMS, a degree over Oklahoma",
            &mrms,
            MRMS_NI,
            MRMS_NJ,
            OKLAHOMA,
            512,
            512,
            IndexWindow {
                i0: 3197,
                i1: 3302,
                j0: 1897,
                j1: 2002,
            },
        ),
        (
            "MRMS, a box off the grid's west edge",
            &mrms,
            MRMS_NI,
            MRMS_NJ,
            box_of(40.0, 45.0, -135.0, -125.0),
            1024,
            768,
            IndexWindow {
                i0: 0,
                i1: 504,
                j0: 996,
                j1: 1503,
            },
        ),
        (
            "MRMS, the whole domain at the seam probe's pane",
            &mrms,
            MRMS_NI,
            MRMS_NJ,
            box_of(20.0, 55.0, -135.0, -55.0),
            2878,
            1651,
            IndexWindow {
                i0: 0,
                i1: 7000,
                j0: 0,
                j1: 3500,
            },
        ),
        (
            "a regional separable grid",
            &separable,
            200,
            100,
            box_of(30.0, 35.0, -100.0, -95.0),
            512,
            512,
            IndexWindow {
                i0: 77,
                i1: 103,
                j0: 28,
                j1: 42,
            },
        ),
        (
            "HRRR, a 12-degree box",
            &hrrr.coords,
            hrrr.ni,
            hrrr.nj,
            box_of(29.5, 41.5, -103.5, -91.5),
            1024,
            768,
            IndexWindow {
                i0: 700,
                i1: 1098,
                j0: 190,
                j1: 649,
            },
        ),
    ];
    for (label, coords, ni, nj, b, w, h, before) in cases {
        let now = window(coords, ni, nj, &b, w, h);
        println!("{label}: {now:?}");
        assert_eq!(now, before, "{label}: an in-frame box's window moved");
    }
}
