//! The window on a grid whose **longitude axis closes the globe**.
//!
//! A wrapping grid used to return the whole grid from [`projection_window`] on
//! both axes. That is right for a Lambert grid — see
//! `projection_window_tests`, whose two wrapping suites still fail if either
//! axis is narrowed there — and wrong for a grid whose rows are parallels: no
//! longitude discontinuity can reach a latitude that longitude is no input to.
//!
//! GMGSI is the layer that makes it matter. 5000 columns x 3000 rows is
//! 15,000,000 points, projected in full at every zoom, and the window is also
//! what `render::jobs` cuts the values block to before it crosses the worker
//! port — 60 MB of `f32` per raster.

use super::lambert_fixture::materialised;
use super::*;
use crate::hrrr::{GridCoords, HrrrGridData, ModelParameter, summarize_values};

fn window(grid: &HrrrGridData, bounds: &GeoBounds, width: u32, height: u32) -> IndexWindow {
    super::projection_window(&grid.coords, grid.ni, grid.nj, bounds, width, height)
}

fn raster(grid: &HrrrGridData, bounds: &GeoBounds, width: u32, height: u32) -> Vec<u8> {
    super::rasterize_gridded(
        &GriddedInput::Whole(std::sync::Arc::new(grid.clone())),
        bounds,
        width,
        height,
    )
    .rgba
}

fn box_of(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> GeoBounds {
    GeoBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    }
}

// ---------------------------------------------------------------------------
// The committed GMGSI granule — the grid this whole file exists for.
// ---------------------------------------------------------------------------

/// The granule's shape, from its own `data(time, yc, xc)` dimensions.
const NX: usize = 5000;
const NY: usize = 3000;
const ALL_POINTS: usize = NX * NY;

fn gmgsi() -> (GridCoords, GeoBounds) {
    let g = crate::gmgsi::decode::decode(
        include_bytes!(
            "../../../testdata/\
             GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
        )
        .to_vec(),
        crate::gmgsi::GmgsiChannel::LongwaveIr,
    )
    .expect("the committed granule decodes");
    assert_eq!((g.grid.ni, g.grid.nj), (NX, NY));
    assert!(
        g.grid.coords.wraps_longitude(),
        "the fixture must actually wrap, or every claim below is about nothing",
    );
    (g.grid.coords, g.bounds)
}

fn gmgsi_window(coords: &GridCoords, bounds: &GeoBounds, w: u32, h: u32) -> IndexWindow {
    super::projection_window(coords, NX, NY, bounds, w, h)
}

/// **The window narrows.** Three viewports across the zoom range, as counts
/// and as a fraction of the granule's 15,000,000 points.
///
/// The exact edges are asserted, not just the area, because they are a figure
/// only [`GridCoords::index_bounds`]'s `Separable` arm can produce: every
/// other route through `projection_window` answers `0..5000 x 0..3000`, and
/// that arm was unreachable on this path until the wrap guard was narrowed.
#[test]
fn a_zoomed_view_of_the_global_mosaic_narrows_the_window() {
    let (coords, _) = gmgsi();

    // CONUS. 387,504 points, 2.58% of the grid.
    let conus = gmgsi_window(&coords, &box_of(24.0, 50.0, -125.0, -66.0), 1024, 768);
    assert_eq!(
        conus,
        IndexWindow {
            i0: 759,
            i1: 1587,
            j0: 691,
            j1: 1159
        },
    );
    assert_eq!(conus.area(), 387_504);

    // Europe, on the other side of the prime meridian. 301,205 points, 2.01%.
    let europe = gmgsi_window(&coords, &box_of(35.0, 60.0, -10.0, 30.0), 1024, 768);
    assert_eq!(
        europe,
        IndexWindow {
            i0: 2357,
            i1: 2920,
            j0: 447,
            j1: 984
        },
    );
    assert_eq!(europe.area(), 302_331);

    // A metro zoom, a fifth of a degree across. 64 points, 0.0004%.
    let metro = gmgsi_window(&coords, &box_of(35.4, 35.6, -97.6, -97.4), 1024, 768);
    assert_eq!(metro.area(), 64);

    for (label, win) in [("conus", conus), ("europe", europe), ("metro", metro)] {
        assert!(
            win.area() * 20 < ALL_POINTS,
            "{label} still projects {} of {ALL_POINTS} points",
            win.area(),
        );
    }
}

/// **The rows narrow even where the view walks over the seam**, which is the
/// half of the saving no discontinuity can take away — and the half a later
/// "simplification" back to one `return full` would quietly give back.
///
/// Pinned separately from the columns on purpose: the same view is the one
/// asserted below to keep every column, so a single test could not tell the
/// two claims apart.
#[test]
fn the_rows_narrow_even_where_the_view_walks_over_the_seam() {
    let (coords, _) = gmgsi();
    for bounds in [
        box_of(30.0, 50.0, 170.0, 190.0),
        box_of(30.0, 50.0, -190.0, -170.0),
        box_of(30.0, 50.0, 179.0, 181.0),
    ] {
        let win = gmgsi_window(&coords, &bounds, 1024, 768);
        assert!(
            win.j1 - win.j0 < NY / 4,
            "{bounds:?} kept {} of {NY} rows",
            win.j1 - win.j0,
        );
        // 1,865,000 points for the 20-degree box: 12.43% of the grid, against
        // 100% before. The least any of these viewports improves.
        assert!(
            win.area() * 4 < ALL_POINTS,
            "{bounds:?} still projects {} of {ALL_POINTS} points",
            win.area(),
        );
    }
}

/// **The seam still closes.** A view over the anti-meridian keeps *every*
/// column the unwindowed grid had — the granule's column 4999 sits 0.027
/// degrees west of its column 0, so a single narrow span near the fold would
/// drop the far end and tear the map open down the anti-meridian.
///
/// `0..5000` is exactly the pre-change answer for this view, which was the
/// whole grid and so correct by construction.
#[test]
fn a_view_over_the_anti_meridian_keeps_every_column() {
    let (coords, _) = gmgsi();
    for bounds in [
        box_of(30.0, 50.0, 170.0, 190.0),
        box_of(30.0, 50.0, -190.0, -170.0),
        box_of(30.0, 50.0, 179.0, 181.0),
        box_of(-60.0, 60.0, 179.9, 180.1),
        box_of(0.0, 10.0, 179.99, 179.999),
        box_of(-85.0, 85.0, -180.0, 180.0),
    ] {
        let win = gmgsi_window(&coords, &bounds, 1024, 768);
        assert_eq!(
            (win.i0, win.i1),
            (0, NX),
            "{bounds:?} narrowed the columns across the fold to {}..{}",
            win.i0,
            win.i1,
        );
    }
}

/// **And the picture the granule itself paints does not move.** The narrowed
/// window against the whole grid — the `93e8606d` answer for this view, forced
/// through the carried-window arm — on the real 15,000,000-point granule.
///
/// The viewport is the one where the fold column's stretched rectangle reaches
/// furthest into the texture: at `-10..30` E, column 0 projects to x 4864 with
/// a half-extent of 5068 px, so its rectangle covers the whole 1024 px width.
/// It is drawn first, at `i = 0`, and every column that follows paints over
/// it — which is why the two rasters agree to the byte here and why the
/// synthetic sweep below has to keep every point visible to stay honest.
///
/// This is the slowest test in the module by far: the reference projects all
/// 15,000,000 points, which is the cost the rest of this file removes.
#[test]
fn the_granules_own_picture_does_not_move() {
    let g = crate::gmgsi::decode::decode(
        include_bytes!(
            "../../../testdata/\
             GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
        )
        .to_vec(),
        crate::gmgsi::GmgsiChannel::LongwaveIr,
    )
    .expect("the committed granule decodes");
    let field = crate::gmgsi::fields::spec(crate::gmgsi::GmgsiChannel::LongwaveIr)
        .id
        .clone();
    let bounds = box_of(35.0, 60.0, -10.0, 30.0);
    let (w, h) = (512u32, 384u32);

    let narrowed = super::rasterize_gridded(
        &GriddedInput::Resident(std::sync::Arc::new(crate::render::gridded::ResidentGrid {
            field: field.clone(),
            ni: NX,
            nj: NY,
            coords: g.grid.coords.clone(),
            values: g.grid.values.clone(),
        })),
        &bounds,
        w,
        h,
    );
    // The carried-window arm returns `win` as given, so this is the whole grid
    // whatever `projection_window` now says.
    let whole = super::rasterize_gridded(
        &GriddedInput::Window(GridWindow {
            field,
            ni: NX,
            nj: NY,
            coords: g.grid.coords.clone(),
            win: IndexWindow {
                i0: 0,
                i1: NX,
                j0: 0,
                j1: NY,
            },
            values: g.grid.values.clone(),
        }),
        &bounds,
        w,
        h,
    );
    assert!(
        whole.rgba.iter().any(|&b| b != 0),
        "the reference painted nothing, so matching it proves nothing",
    );
    assert_eq!(
        narrowed.rgba, whole.rgba,
        "the narrowed window changed what the granule paints over Europe",
    );
}

// ---------------------------------------------------------------------------
// A globe-closing grid small enough to rasterize every point of, which is what
// makes a *picture* comparison affordable.
// ---------------------------------------------------------------------------

/// A `Separable` grid with the granule's own fold: column 0 a hair west of the
/// anti-meridian holding the axis maximum, column 1 the minimum, and the
/// columns sweeping east once around from there.
fn wrapping_separable(ni: usize, nj: usize) -> HrrrGridData {
    let step = 360.0 / ni as f64;
    let lon_axis: Vec<f64> = (0..ni)
        .map(|i| {
            let raw = -180.0 + (i as f64 - 0.5) * step;
            if raw < -180.0 { raw + 360.0 } else { raw }
        })
        .collect();
    let lat_axis: Vec<f64> = (0..nj)
        .map(|j| 78.0 - j as f64 * (156.0 / (nj - 1) as f64))
        .collect();
    separable_grid(lat_axis, lon_axis)
}

/// Values every one of which paints, and which paint a **different colour**
/// at each neighbour on both axes.
///
/// Both halves matter, for opposite reasons, and both are asserted rather than
/// assumed by [`the_fixtures_paint_a_different_colour_at_every_neighbour`].
///
/// *Different* is what makes a dropped column or row visible at all: a fixture
/// of one flat value repaints the same bytes whatever the window does.
///
/// *Every one paints* is what keeps the comparison honest on a grid that
/// closes the globe. `rasterize_gridded` sizes a cell from its `i` neighbours,
/// and the fold column's neighbour is a whole turn away, so its rectangle is
/// stretched across the texture — 0.55 of a turn in pixels — wherever the
/// projection puts it. Drawn first, at `i = 0`, it is painted over by every
/// column that follows, so it is invisible while something follows it and
/// survives only where the columns that belong to the viewport are
/// transparent. That artifact is the whole-grid reference's, not the window's:
/// the narrowed window omits the fold column and simply does not draw it. See
/// this module's report of the difference; the fixture holds it out of the
/// oracle rather than encoding it as truth.
fn every_point_paints(ni: usize, nj: usize) -> Vec<f32> {
    (0..ni * nj)
        .map(|k| {
            let (i, j) = (k % ni.max(1), k / ni.max(1));
            // CAPE paints at or above 250; the two strides are coprime with
            // the ramp's own steps, so neither neighbour repeats a colour.
            250.0 + ((i * 337 + j * 1009) % 3701) as f32
        })
        .collect()
}

fn separable_grid(lat_axis: Vec<f64>, lon_axis: Vec<f64>) -> HrrrGridData {
    let (ni, nj) = (lon_axis.len(), lat_axis.len());
    let bounds = GeoBounds {
        min_lat: lat_axis.iter().copied().fold(f64::MAX, f64::min),
        max_lat: lat_axis.iter().copied().fold(f64::MIN, f64::max),
        min_lon: lon_axis.iter().copied().fold(f64::MAX, f64::min),
        max_lon: lon_axis.iter().copied().fold(f64::MIN, f64::max),
    };
    let parameter = ModelParameter::SurfaceBasedCape;
    let values = every_point_paints(ni, nj);
    let (visible_points, value_range) = summarize_values(&values, |v| parameter.paints(v));
    HrrrGridData {
        parameter,
        values,
        coords: GridCoords::Separable { lat_axis, lon_axis },
        ni,
        nj,
        bounds,
        ref_time: chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        forecast_hour: 0,
        visible_points,
        value_range,
    }
}

/// A regular grid whose 360 one-degree columns close the globe, spelled from
/// `lon0`. **`lon0 = 0` is the hazard**: `(lon - lon0) / dlon` is then a whole
/// turn out for every box in the western hemisphere and lands *negative*,
/// which clamps to an **empty** window — a silent blank, not a wide window.
fn wrapping_regular(lon0: f64) -> HrrrGridData {
    let (ni, nj) = (360usize, 160usize);
    let parameter = ModelParameter::SurfaceBasedCape;
    let values = every_point_paints(ni, nj);
    let (visible_points, value_range) = summarize_values(&values, |v| parameter.paints(v));
    HrrrGridData {
        parameter,
        values,
        coords: GridCoords::Regular {
            lat0: 79.5,
            lon0,
            dlat: -1.0,
            dlon: 1.0,
            ni,
            nj,
            scan_mode: 0b0100_0000,
        },
        ni,
        nj,
        bounds: GeoBounds {
            min_lat: 79.5 - (nj - 1) as f64,
            max_lat: 79.5,
            min_lon: lon0,
            max_lon: lon0 + (ni - 1) as f64,
        },
        ref_time: chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        forecast_hour: 0,
        visible_points,
        value_range,
    }
}

/// [`every_point_paints`] claims two things of every fixture below, and a
/// sweep comparing pictures is worth nothing if either is false.
#[test]
fn the_fixtures_paint_a_different_colour_at_every_neighbour() {
    for grid in [
        wrapping_separable(120, 61),
        wrapping_regular(-180.0),
        quarter_degree_regular(),
    ] {
        let paint =
            crate::render::gridded::field_paint(&crate::hrrr::fields::spec(grid.parameter).id)
                .expect("the fixture's field is registered");
        assert_eq!(
            grid.visible_points,
            grid.ni * grid.nj,
            "a transparent point lets the fold column's stretched rectangle \
             survive into the reference, which is not the window's doing",
        );
        for j in 0..grid.nj {
            for i in 0..grid.ni {
                let here = paint.color_for_value(grid.values[j * grid.ni + i]);
                assert_ne!(here[3], 0, "point ({i}, {j}) paints nothing");
                if i + 1 < grid.ni {
                    assert_ne!(
                        here,
                        paint.color_for_value(grid.values[j * grid.ni + i + 1]),
                        "columns {i} and {} repeat a colour at row {j}",
                        i + 1,
                    );
                }
                if j + 1 < grid.nj {
                    assert_ne!(
                        here,
                        paint.color_for_value(grid.values[(j + 1) * grid.ni + i]),
                        "rows {j} and {} repeat a colour at column {i}",
                        j + 1,
                    );
                }
            }
        }
    }
}

/// **The window must be invisible in the output**, on a globe-closing grid as
/// on every other: the same bytes, because the coordinates are the same
/// coordinates. 500 boxes and texture shapes from a fixed seed, drawn over the
/// whole globe and deliberately dense around the fold, against the same grid
/// with every point's coordinates materialised — which answers `None` from
/// `index_bounds` and so projects all of them.
#[test]
fn the_window_paints_exactly_what_projecting_every_point_paints_on_a_closing_grid() {
    let grid = wrapping_separable(120, 61);
    assert!(grid.coords.wraps_longitude(), "the fixture must wrap");
    let every_point = materialised(&grid);
    let whole = IndexWindow {
        i0: 0,
        i1: grid.ni,
        j0: 0,
        j1: grid.nj,
    };

    // xorshift64*, so the cases are the same on every machine and run.
    let mut state = 0x2026_0822_0100_0001u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };

    let (mut narrowed, mut across_the_fold) = (0, 0);
    for case in 0..500 {
        // Half the cases anywhere on the globe, half within 10 degrees of the
        // fold — the boxes that decide whether the seam closes.
        let clon = if case % 2 == 0 {
            next() * 360.0 - 180.0
        } else {
            180.0 - next() * 20.0
        };
        let clat = next() * 150.0 - 75.0;
        let (dlon, dlat) = (0.05 * 4000f64.powf(next()), 0.05 * 2000f64.powf(next()));
        let (w, h) = (1 + (next() * 200.0) as u32, 1 + (next() * 200.0) as u32);
        let bounds = GeoBounds {
            min_lon: clon - dlon / 2.0,
            max_lon: clon + dlon / 2.0,
            min_lat: (clat - dlat / 2.0).max(-84.0),
            max_lat: (clat + dlat / 2.0).min(84.0),
        };
        if bounds.min_lat >= bounds.max_lat {
            continue;
        }
        let win = window(&grid, &bounds, w, h);
        if win != whole {
            narrowed += 1;
        }
        if (win.i0, win.i1) == (0, grid.ni) && win.j1 - win.j0 < grid.nj {
            across_the_fold += 1;
        }
        assert_eq!(
            raster(&grid, &bounds, w, h),
            raster(&every_point, &bounds, w, h),
            "case {case}: {bounds:?} at {w}x{h} — the window changed the picture",
        );
    }
    // Without these the sweep could pass on 500 cases that all fell back to
    // the whole grid, which proves nothing about the window.
    assert!(
        narrowed > 300,
        "only {narrowed} of 500 cases narrowed at all"
    );
    assert!(
        across_the_fold > 20,
        "only {across_the_fold} of 500 cases were the fold case — the rows \
         narrowed while the columns stayed whole",
    );
}

/// The same claim as a picture, at the one place the map can tear: a view
/// centred on the anti-meridian, where the grid's last column and its first
/// are neighbours. Non-blank is asserted too — a raster of nothing matches a
/// raster of nothing.
#[test]
fn a_view_centred_on_the_anti_meridian_paints_what_every_point_paints() {
    let grid = wrapping_separable(240, 121);
    let every_point = materialised(&grid);
    for &(span, w, h) in &[(0.5f64, 256u32, 128u32), (4.0, 512, 256), (40.0, 300, 200)] {
        for &centre in &[180.0f64, -180.0, 179.5, -179.5] {
            let bounds = box_of(20.0, 60.0, centre - span / 2.0, centre + span / 2.0);
            let painted = raster(&grid, &bounds, w, h);
            assert!(
                painted.iter().any(|&b| b != 0),
                "{bounds:?} at {w}x{h} painted nothing, so matching proves nothing",
            );
            assert_eq!(
                painted,
                raster(&every_point, &bounds, w, h),
                "{bounds:?} at {w}x{h} — the seam tore",
            );
        }
    }
}

/// A **regular** grid that closes the globe. Its columns get the conservative
/// answer — all of them — because `lon0` may put the linear column formula a
/// whole turn out; its rows are parallels and narrow as ever.
#[test]
fn a_wrapping_regular_grid_keeps_every_column_and_still_narrows_its_rows() {
    for lon0 in [-180.0, 0.0] {
        let grid = wrapping_regular(lon0);
        assert!(
            grid.coords.wraps_longitude(),
            "lon0 {lon0}: 360 columns of one degree close the globe",
        );
        let win = window(&grid, &box_of(30.0, 40.0, -100.0, -90.0), 512, 512);
        assert_eq!(
            (win.i0, win.i1),
            (0, grid.ni),
            "lon0 {lon0}: the columns must not be cut on a wrapping regular grid",
        );
        assert!(
            !win.is_empty(),
            "lon0 {lon0}: the window went empty, which paints a silent blank",
        );
        assert!(
            win.j1 - win.j0 < grid.nj / 4,
            "lon0 {lon0}: kept {} of {} rows",
            win.j1 - win.j0,
            grid.nj,
        );
    }
}

/// And the picture is unchanged, on the spelling whose longitudes the raster's
/// own linear projection can reach.
#[test]
fn a_wrapping_regular_grid_paints_what_projecting_every_point_paints() {
    let grid = wrapping_regular(-180.0);
    let every_point = materialised(&grid);
    for bounds in [
        box_of(30.0, 40.0, -100.0, -90.0),
        box_of(-60.0, 60.0, -179.0, 179.0),
        box_of(20.0, 60.0, 170.0, 190.0),
        box_of(20.0, 60.0, -190.0, -170.0),
        box_of(0.0, 5.0, -0.5, 0.5),
    ] {
        for &(w, h) in &[(64u32, 64u32), (300, 17)] {
            let painted = raster(&grid, &bounds, w, h);
            assert!(
                painted.iter().any(|&b| b != 0),
                "{bounds:?} at {w}x{h} painted nothing",
            );
            assert_eq!(painted, raster(&every_point, &bounds, w, h), "{bounds:?}");
        }
    }
}

/// **The row pad is spent in the units it is measured in.** `merc_pad` grows
/// the box along Mercator y, and a cell `cell_deg` degrees tall is
/// `cell_deg / cos(lat)` of it: 1.4x at 45 N, 3.2x at 72 N. Stating the pad in
/// degrees and spending it in Mercator y reaches `cos(lat)` of the way, and
/// `low`/`high` only hand back one index of slack, so past about 65 N a row
/// whose cell paints is left unprojected and the raster loses a band.
///
/// **Nothing here wraps**, and that is the point: this is a defect of
/// `93e8606d`'s own live path, found by the sweep below and reproduced there
/// before any of this file's changes were in the tree. It is fixed here
/// because the sweep cannot be honest about a globe-closing grid — which
/// reaches 72 N and beyond — while it is open.
#[test]
fn a_coarse_row_at_high_latitude_is_still_projected() {
    let grid = separable_grid(
        (0..61).map(|j| 78.0 - j as f64 * 2.6).collect(),
        (0..120).map(|i| -60.0 + i as f64).collect(),
    );
    assert!(
        !grid.coords.wraps_longitude(),
        "the pad is not a wrapping grid's problem, and must not be tested as one",
    );
    let every_point = materialised(&grid);
    for &(min_lat, max_lat) in &[
        (71.66746209907618, 71.74377650861598),
        (74.0, 74.1),
        (68.2, 68.5),
        (45.0, 45.2),
        (0.0, 0.2),
    ] {
        let bounds = box_of(min_lat, max_lat, 32.72, 50.39);
        for &(w, h) in &[(63u32, 10u32), (256, 128)] {
            let painted = raster(&grid, &bounds, w, h);
            assert!(
                painted.iter().any(|&b| b != 0),
                "{min_lat}..{max_lat} at {w}x{h} painted nothing",
            );
            assert_eq!(
                painted,
                raster(&every_point, &bounds, w, h),
                "{min_lat}..{max_lat} at {w}x{h}: the window lost a row",
            );
        }
    }
}

/// **A grid that does not wrap keeps the window it had**, on all three arms
/// that answer: a Lambert grid on HRRR's own projection, a regular grid, and a
/// regional separable grid whose longitude axis is plainly monotonic.
///
/// The four windows on the right were **read off `93e8606d`** by running this
/// same call there, before any of this. Two claims, and they are deliberately
/// not the same claim:
///
/// * the **columns** are those four numbers exactly — nothing here changes how
///   a grid that does not close the globe brackets its longitude axis;
/// * the **rows** contain them, and never fall inside them. `merc_pad` used to
///   spend a pad measured in degrees of latitude on an axis measured in
///   Mercator y, which is short by `cos(lat)`; correcting it can only widen a
///   window, and one index each way at these latitudes is what it does.
#[test]
fn a_grid_that_does_not_wrap_keeps_the_window_it_had() {
    let lambert = super::lambert_fixture::lambert_grid(1799, 1059, 0b0100_0000);
    assert!(!lambert.coords.wraps_longitude());
    let regular = quarter_degree_regular();
    assert!(!regular.coords.wraps_longitude());
    let regional = separable_grid(
        (0..100).map(|j| 50.0 - j as f64 * 0.5).collect(),
        (0..200).map(|i| -120.0 + i as f64 * 0.25).collect(),
    );
    assert!(!regional.coords.wraps_longitude());

    let cases: [(&str, &HrrrGridData, GeoBounds, u32, u32, IndexWindow); 4] = [
        (
            "hrrr, a 12-degree box",
            &lambert,
            box_of(29.5, 41.5, -103.5, -91.5),
            1024,
            768,
            IndexWindow {
                i0: 700,
                i1: 1098,
                j0: 191,
                j1: 649,
            },
        ),
        (
            "hrrr, the pane the suite next door uses",
            &lambert,
            super::lambert_fixture::coverage(35.5, -97.5, 12.0),
            1024,
            1024,
            IndexWindow {
                i0: 700,
                i1: 1098,
                j0: 191,
                j1: 649,
            },
        ),
        (
            "a quarter-degree regular grid",
            &regular,
            box_of(30.0, 40.0, -100.0, -90.0),
            512,
            512,
            IndexWindow {
                i0: 118,
                i1: 162,
                j0: 78,
                j1: 122,
            },
        ),
        (
            "a regional separable grid",
            &regional,
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
    ];

    for (label, grid, bounds, w, h, before) in cases {
        let now = window(grid, &bounds, w, h);
        assert_eq!(
            (now.i0, now.i1),
            (before.i0, before.i1),
            "{label}: the columns moved, and nothing here may move them",
        );
        assert!(
            now.j0 <= before.j0 && now.j1 >= before.j1,
            "{label}: the rows narrowed from {before:?} to {now:?}, which crops",
        );
        assert!(
            before.j0 - now.j0 <= 1 && now.j1 - before.j1 <= 1,
            "{label}: the rows widened from {before:?} to {now:?} by more than \
             the one index the Mercator pad correction is worth",
        );
    }
}

/// A regular grid of quarter-degree cells over North America — 90 degrees of
/// longitude, so nothing about it closes the globe.
fn quarter_degree_regular() -> HrrrGridData {
    let mut grid = wrapping_regular(-130.0);
    let (ni, nj) = (360usize, 160usize);
    grid.coords = GridCoords::Regular {
        lat0: 60.0,
        lon0: -130.0,
        dlat: -0.25,
        dlon: 0.25,
        ni,
        nj,
        scan_mode: 0b0100_0000,
    };
    grid.bounds = GeoBounds {
        min_lat: 60.0 - 0.25 * (nj - 1) as f64,
        max_lat: 60.0,
        min_lon: -130.0,
        max_lon: -130.0 + 0.25 * (ni - 1) as f64,
    };
    grid
}
