//! The model grid's windowed wire carry paints **the same bytes** as the
//! whole grid.

use super::lambert_fixture::lambert_grid;
use super::*;
use std::sync::Arc;

/// A viewport over the grid's interior, sized so its projection window is a proper subset.
fn fixture() -> (HrrrGridData, GeoBounds) {
    let grid = lambert_grid(97, 61, 0b0100_0000);
    let at = |i: usize, j: usize| grid.coords.at(j * grid.ni + i).expect("in range");
    let (lat_a, lon_a) = at(30, 18);
    let (lat_b, lon_b) = at(62, 42);
    let bounds = GeoBounds {
        min_lat: lat_a.min(lat_b),
        max_lat: lat_a.max(lat_b),
        min_lon: lon_a.min(lon_b),
        max_lon: lon_a.max(lon_b),
    };
    (grid, bounds)
}

const W: u32 = 160;
const H: u32 = 128;

/// The window form the encoder ships: the computed window and exactly its
/// values, cut with the same accessors `offload`'s encoder uses.
fn window_form(grid: &HrrrGridData, bounds: &GeoBounds, w: u32, h: u32) -> ModelWindow {
    let input = ModelDataInput::Whole(Arc::new(grid.clone()));
    let win = input.window_for(bounds, w, h);
    let mut values = Vec::with_capacity(win.area());
    input.for_each_window_row(&win, |row| values.extend_from_slice(row));
    ModelWindow {
        parameter: grid.parameter,
        ni: grid.ni,
        nj: grid.nj,
        coords: grid.coords.clone(),
        win,
        values,
    }
}

fn painted(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
}

/// **The parity, with its floors.** A proper-subset window's values render
/// byte-identically to the whole grid's — and the fixture is proven able to
/// express the difference before the equality is believed.
#[test]
fn the_windowed_values_paint_exactly_what_the_whole_grid_paints() {
    let (grid, bounds) = fixture();
    let whole = ModelDataInput::Whole(Arc::new(grid.clone()));
    let window = window_form(&grid, &bounds, W, H);

    // Non-triviality floors. A window covering the whole grid would make
    // this test the identity, and an empty one would compare two blank
    // buffers.
    assert!(
        window.win.area() > 0,
        "the viewport missed the grid; nothing below means anything",
    );
    assert!(
        window.win.area() < grid.ni * grid.nj,
        "the window is the whole grid ({} of {} points), so the subset claim \
         is not being tested — grow the grid or shrink the viewport",
        window.win.area(),
        grid.ni * grid.nj,
    );
    assert_eq!(
        window.values.len(),
        window.win.area(),
        "the cut carries exactly the window's values",
    );

    let direct = rasterize_model_data(&whole, &bounds, W, H);
    assert!(
        painted(&direct.rgba) > 100,
        "the fixture painted {} pixels; the equality below would be \
         near-vacuous",
        painted(&direct.rgba),
    );

    let via_window = rasterize_model_data(&ModelDataInput::Window(window), &bounds, W, H);
    assert_eq!(
        direct.rgba, via_window.rgba,
        "the window form paints a different picture from the whole grid: \
         either the cut lost values the raster reads, or the windowed \
         indexing reads them at the wrong place",
    );
    assert_eq!(direct.alpha, via_window.alpha, "one declaration, both arms");
}

/// The sufficiency proof, from the other side: a value **outside** the
/// window is dead to this raster — move it and nothing moves — while its
/// paired control **inside** the window moves pixels. Together with the
/// parity above this is what licenses never shipping the outside values.
#[test]
fn a_value_outside_the_window_is_dead_and_one_inside_is_not() {
    let (grid, bounds) = fixture();
    let win = ModelDataInput::Whole(Arc::new(grid.clone())).window_for(&bounds, W, H);
    let reference = rasterize_model_data(
        &ModelDataInput::Whole(Arc::new(grid.clone())),
        &bounds,
        W,
        H,
    );
    assert!(painted(&reference.rgba) > 100, "fixture floor");

    // A point the window excludes. `win` is a proper subset (pinned above),
    // and the fixture's window sits in the grid's interior, so (0, 0) is
    // outside it.
    assert!(
        win.i0 > 0 && win.j0 > 0,
        "the window touches the grid corner, so there is no outside point \
         to probe and this test proves nothing",
    );
    let mut outside = grid.clone();
    outside.values[0] = 4000.0;
    let moved_outside =
        rasterize_model_data(&ModelDataInput::Whole(Arc::new(outside)), &bounds, W, H);
    assert_eq!(
        reference.rgba, moved_outside.rgba,
        "a value outside the projection window changed the picture — the \
         window is not the raster's whole read set, and the wire that ships \
         only the window is losing information",
    );

    // The paired positive control: the same mutation *inside* the drawn
    // window must move pixels, or the probe above could not have seen
    // anything either way.
    let draw = win.interior(grid.ni, grid.nj);
    let (ci, cj) = ((draw.i0 + draw.i1) / 2, (draw.j0 + draw.j1) / 2);
    let mut inside = grid.clone();
    inside.values[cj * grid.ni + ci] = 4000.0;
    let moved_inside = rasterize_model_data(
        &ModelDataInput::Window(window_form(&inside, &bounds, W, H)),
        &bounds,
        W,
        H,
    );
    assert_ne!(
        reference.rgba, moved_inside.rgba,
        "a value inside the window did not change the picture, so the dead \
         probe above was a check that could not fail",
    );
}

/// The extent of the carried window is load-bearing: cut one ring tighter
/// than [`projection_window`] says and the picture changes.
#[test]
fn a_window_one_ring_too_tight_changes_the_picture() {
    let (grid, bounds) = fixture();
    let honest = window_form(&grid, &bounds, W, H);
    let direct = rasterize_model_data(
        &ModelDataInput::Whole(Arc::new(grid.clone())),
        &bounds,
        W,
        H,
    );

    let tight_win = IndexWindow {
        i0: honest.win.i0 + 1,
        i1: honest.win.i1 - 1,
        j0: honest.win.j0 + 1,
        j1: honest.win.j1 - 1,
    };
    let input = ModelDataInput::Whole(Arc::new(grid.clone()));
    let mut tight_values = Vec::with_capacity(tight_win.area());
    input.for_each_window_row(&tight_win, |row| tight_values.extend_from_slice(row));
    let tight = rasterize_model_data(
        &ModelDataInput::Window(ModelWindow {
            win: tight_win,
            values: tight_values,
            ..honest
        }),
        &bounds,
        W,
        H,
    );
    assert_ne!(
        direct.rgba, tight.rgba,
        "shaving a ring off the window left the picture untouched, so the \
         parity gate could not catch an encoder that cut the window too \
         tight — the fixture's viewport needs to reach its window's edge",
    );
}

/// A viewport the grid never reaches: the window is empty, the cut is empty,
/// and both forms answer the same blank raster rather than anything partial.
#[test]
fn an_empty_window_is_a_blank_raster_on_both_forms() {
    let (grid, _) = fixture();
    let atlantic = GeoBounds {
        min_lat: 29.5,
        max_lat: 41.5,
        min_lon: -46.0,
        max_lon: -34.0,
    };
    let window = window_form(&grid, &atlantic, W, H);
    assert_eq!(
        window.win.area(),
        0,
        "premise: the viewport misses the grid"
    );
    assert!(window.values.is_empty());

    let whole = rasterize_model_data(
        &ModelDataInput::Whole(Arc::new(grid.clone())),
        &atlantic,
        W,
        H,
    );
    let windowed = rasterize_model_data(&ModelDataInput::Window(window), &atlantic, W, H);
    assert_eq!(whole.rgba, windowed.rgba);
    assert_eq!(painted(&whole.rgba), 0);
}
