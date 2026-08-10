use super::lambert_fixture::{
    CELL_OFFSETS, box_of_cells, coverage, grid_anchored_at, lambert_grid, lambert_grid_stepped,
    materialised,
};
use super::*;

/// Texture sides chosen to straddle the interesting regime change: at 1-8 px
/// a grid cell is far smaller than a pixel, at 256 px far larger.
const SIDES: &[u32] = &[1, 2, 7, 64, 256];

/// Boxes stated as a fraction of the grid's own extent, so they mean the
/// same thing whatever grid they are applied to: centred, off each corner,
/// straddling each edge, enclosing everything, and missing entirely.
const BOXES: &[(&str, f64, f64, f64, f64)] = &[
    // (label, centre-x fraction, centre-y fraction, width, height) of bounds
    ("tiny centre", 0.5, 0.5, 0.05, 0.05),
    ("half", 0.5, 0.5, 0.5, 0.5),
    ("all of it", 0.5, 0.5, 1.0, 1.0),
    ("twice over", 0.5, 0.5, 2.0, 2.0),
    ("SW corner", 0.0, 0.0, 0.3, 0.3),
    ("NE corner", 1.0, 1.0, 0.3, 0.3),
    ("NW corner", 0.0, 1.0, 0.3, 0.3),
    ("SE corner", 1.0, 0.0, 0.3, 0.3),
    ("west edge", 0.0, 0.5, 0.4, 1.4),
    ("north edge", 0.5, 1.0, 1.4, 0.4),
    ("wide and thin", 0.5, 0.5, 3.0, 0.04),
    ("tall and thin", 0.5, 0.5, 0.04, 3.0),
    ("far east", 3.0, 0.5, 0.5, 0.5),
    ("far north", 0.5, 4.0, 0.5, 0.5),
];

fn box_over(g: &GeoBounds, fx: f64, fy: f64, w: f64, h: f64) -> GeoBounds {
    let (lon_span, lat_span) = (g.max_lon - g.min_lon, g.max_lat - g.min_lat);
    let (cx, cy) = (g.min_lon + lon_span * fx, g.min_lat + lat_span * fy);
    GeoBounds {
        min_lon: cx - lon_span * w / 2.0,
        max_lon: cx + lon_span * w / 2.0,
        min_lat: cy - lat_span * h / 2.0,
        max_lat: cy + lat_span * h / 2.0,
    }
}

/// **The window must be invisible in the output.** Not "close": the same
/// bytes, because the coordinates are the same coordinates and skipping a
/// point that could paint — or whose spacing sizes a point that paints — is
/// a defect, not a trade-off.
///
/// The reference grid is the *materialised* twin, which is both the shape
/// the rasterizer had before the grid went lazy and the arm
/// [`projection_window`] declines to narrow, so it really does project all
/// of them.
#[test]
fn the_window_paints_exactly_what_projecting_every_point_paints() {
    let lambert = lambert_grid(97, 61, 0b0100_0000);
    let every_point = materialised(&lambert);

    for &(label, fx, fy, bw, bh) in BOXES {
        let bounds = box_over(&lambert.bounds, fx, fy, bw, bh);
        for &side in SIDES {
            let windowed = rasterize_model_data(&lambert, &bounds, side, side);
            let reference = rasterize_model_data(&every_point, &bounds, side, side);
            assert_eq!(
                windowed.rgba, reference.rgba,
                "{label} at {side}x{side}: the window changed the picture",
            );
        }
    }
}

/// Non-square textures cannot be caught above: an `i`/`j` margin swapped
/// between the axes is invisible while width == height.
#[test]
fn the_window_survives_a_non_square_texture() {
    let lambert = lambert_grid(97, 61, 0b0100_0000);
    let every_point = materialised(&lambert);

    for &(label, fx, fy, bw, bh) in BOXES {
        let bounds = box_over(&lambert.bounds, fx, fy, bw, bh);
        for &(w, h) in &[(320u32, 24u32), (24, 320), (200, 3)] {
            assert_eq!(
                rasterize_model_data(&lambert, &bounds, w, h).rgba,
                rasterize_model_data(&every_point, &bounds, w, h).rgba,
                "{label} at {w}x{h}",
            );
        }
    }
}

/// A scan order that does not lay the flat index out as `j * ni + i` — the
/// only order the neighbour walk understands — must make the window decline
/// to narrow, rather than narrow the wrong axis.
///
/// GRIB2 Table 3.4: bit 3 (`0b0010_0000`) makes `j` the consecutive axis and
/// bit 4 (`0b0001_0000`) makes rows alternate; either breaks the layout. The
/// `i`/`j` *directions* (bits 1 and 2) do not — they only flip the step
/// signs — so those four modes must still narrow, and are the control here.
#[test]
fn a_scan_order_the_neighbour_walk_does_not_match_is_not_narrowed() {
    let whole = |g: &HrrrGridData| IndexWindow {
        i0: 0,
        i1: g.ni,
        j0: 0,
        j1: g.nj,
    };
    for mode in [
        0b0000_0000u8,
        0b0100_0000,
        0b1000_0000,
        0b1100_0000,
        0b0010_0000,
        0b0110_0000,
        0b0101_0000,
        0b0001_0000,
    ] {
        let grid = lambert_grid(41, 29, mode);
        let bounds = box_over(&grid.bounds, 0.5, 0.5, 0.4, 0.4);
        let row_major = mode & 0b0011_0000 == 0;
        let window = projection_window(&grid, &bounds, 128, 128);
        assert_eq!(
            window == whole(&grid),
            !row_major,
            "scanning mode {mode:#010b}: got {window:?}",
        );
        assert_eq!(
            rasterize_model_data(&grid, &bounds, 128, 128).rgba,
            rasterize_model_data(&materialised(&grid), &bounds, 128, 128).rgba,
            "scanning mode {mode:#010b}",
        );
    }
}

/// A window that never narrows would pass every test above. This is the
/// control: the point of the change is that a small viewport projects a
/// small fraction of the grid.
#[test]
fn a_small_viewport_narrows_the_window_sharply() {
    let grid = lambert_grid(1799, 1059, 0b0100_0000);
    let full = (grid.ni * grid.nj) as f64;

    let tight = projection_window(&grid, &coverage(35.5, -97.5, 3.0), 1024, 1024);
    let tight_points = ((tight.i1 - tight.i0) * (tight.j1 - tight.j0)) as f64;
    assert!(
        tight_points / full < 0.02,
        "a 3° viewport still projects {:.1}% of the grid",
        100.0 * tight_points / full,
    );

    let typical = projection_window(&grid, &coverage(35.5, -97.5, 12.0), 1024, 1024);
    let typical_points = ((typical.i1 - typical.i0) * (typical.j1 - typical.j0)) as f64;
    assert!(
        typical_points / full < 0.2,
        "a 12° viewport still projects {:.1}% of the grid",
        100.0 * typical_points / full,
    );

    // Off the grid entirely: nothing at all.
    assert!(
        projection_window(&grid, &coverage(35.5, -40.0, 12.0), 1024, 1024).is_empty(),
        "an Atlantic viewport must project nothing",
    );

    // And the whole domain must still be the whole domain.
    assert_eq!(
        projection_window(&grid, &coverage(37.0, -97.5, 75.0), 1024, 1024),
        IndexWindow {
            i0: 0,
            i1: grid.ni,
            j0: 0,
            j1: grid.nj
        },
    );
}

/// The fixed boxes above are the cases someone thought of. This is the
/// sweep: 400 boxes and texture shapes drawn from a fixed seed, ranging from
/// a box a fiftieth of the grid to one eight times its size, and from a
/// 1 px texture to a 300 px one. Any margin that is merely *usually* enough
/// fails here.
#[test]
fn the_window_survives_a_randomised_sweep_of_viewports() {
    let lambert = lambert_grid(73, 47, 0b0100_0000);
    let every_point = materialised(&lambert);

    // xorshift64*, so the cases are the same on every machine and run.
    let mut state = 0x2026_0725_1200_0001u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };

    let mut narrowed = 0;
    let whole = IndexWindow {
        i0: 0,
        i1: lambert.ni,
        j0: 0,
        j1: lambert.nj,
    };
    for case in 0..400 {
        let (fx, fy) = (next() * 3.0 - 1.0, next() * 3.0 - 1.0);
        // Log-uniform, so small boxes — the ones that narrow — are not
        // crowded out by large ones.
        let (bw, bh) = (0.02 * 400f64.powf(next()), 0.02 * 400f64.powf(next()));
        let w = 1 + (next() * 300.0) as u32;
        let h = 1 + (next() * 300.0) as u32;
        let bounds = box_over(&lambert.bounds, fx, fy, bw, bh);
        if projection_window(&lambert, &bounds, w, h) != whole {
            narrowed += 1;
        }
        assert_eq!(
            rasterize_model_data(&lambert, &bounds, w, h).rgba,
            rasterize_model_data(&every_point, &bounds, w, h).rgba,
            "case {case}: box ({fx:.3}, {fy:.3}) x ({bw:.3}, {bh:.3}) at {w}x{h}",
        );
    }
    // Without this the sweep could pass on 400 cases that all fell back to
    // the whole grid, which proves nothing about the window.
    assert!(
        narrowed > 200,
        "only {narrowed} of 400 cases narrowed at all"
    );
}

/// **Street-level zoom.** `walkers` allows zoom 0-26 and nothing here
/// narrows that, so a texture routinely covers a fraction of one 3 km cell.
/// A margin stated as a fraction of the box cannot reach 0.55 of a cell once
/// the box is smaller than a cell, and the overlay goes mostly blank —
/// 5.5 M of 6.3 M pixels wrong at zoom 19, worsening upward.
///
/// Sized in cells so the regime, not the grid, is what varies. Corners are
/// included because the one-sided neighbour branches live there.
#[test]
fn the_window_survives_a_viewport_smaller_than_one_grid_cell() {
    let lambert = lambert_grid(97, 61, 0b0100_0000);
    let every_point = materialised(&lambert);

    for &cells in &[0.005, 0.05, 0.3, 0.7, 1.0, 2.0, 6.0] {
        for &(i, j) in &[(48, 30), (0, 0), (95, 59), (1, 1), (10, 55), (80, 2)] {
            for &offset in CELL_OFFSETS {
                let bounds = box_of_cells(&lambert, i, j, cells, offset);
                for &(w, h) in &[(64u32, 64u32), (512, 512), (385, 3), (3, 385), (1, 1)] {
                    assert_eq!(
                        rasterize_model_data(&lambert, &bounds, w, h).rgba,
                        rasterize_model_data(&every_point, &bounds, w, h).rgba,
                        "{cells} cells around ({i}, {j}) offset {offset:?} at {w}x{h}",
                    );
                }
            }
        }
    }
}

/// **Past the anti-meridian.** `expand_for_overdraw` clamps latitude to the
/// Mercator limit but leaves longitude alone, and `walkers::unproject`
/// neither wraps nor clamps it, so panning west at low zoom produces a
/// texture running past -180. Grid longitudes are normalised to -180..180,
/// so such a box is not the interval it looks like.
///
/// The numbers are the reviewer's: -277 is where `lon0 - 180 = -277.5` plus
/// the growth crosses the cone's seam, and -290..-110 is a viewport of
/// -230..-170 expanded by `OVERDRAW_FRACTION = 1.0`.
#[test]
fn the_window_survives_a_texture_running_past_the_antimeridian() {
    let lambert = lambert_grid(97, 61, 0b0100_0000);
    let every_point = materialised(&lambert);

    for &(min_lon, max_lon) in &[
        (-170.0, -30.0),  // control: inside the fold, must still narrow
        (-277.0, -20.0),  // just past the seam
        (-290.0, -110.0), // the overdraw-expanded viewport
        (-310.0, -10.0),
        (-360.0, 0.0),
        (10.0, 300.0), // and the eastern side
    ] {
        let bounds = GeoBounds {
            min_lat: 15.0,
            max_lat: 55.0,
            min_lon,
            max_lon,
        };
        for &side in &[64u32, 512] {
            assert_eq!(
                rasterize_model_data(&lambert, &bounds, side, side).rgba,
                rasterize_model_data(&every_point, &bounds, side, side).rgba,
                "longitude {min_lon}..{max_lon} at {side}x{side}",
            );
        }
    }
}

/// A grid that itself straddles the anti-meridian: its `i` neighbour is a
/// whole turn away in longitude, so the cell's rect is stretched across the
/// texture and the "0.55 of a cell" reach this window is built on stops
/// describing it. The window has to decline rather than model that.
///
/// Not hypothetical arithmetic: with the guard removed this sweep fails 372
/// of 600 cases on the 400 km grid and 5 of 600 on the 200 km one, while the
/// HRRR-shaped control stays at 0 either way.
///
/// **`LoV` is swept, and that is the point.** The guard this replaced asked
/// whether the grid's `min_lon..max_lon` contained the seam, which for a
/// wrapping grid is `-180..180` — so the seam only falls *inside* it when
/// `LoV` puts it somewhere other than the anti-meridian. HRRR's 262.5 does;
/// `LoV = 0` does not, and that case failed 398 of 600 while the test
/// claiming to cover it passed. `LoV = 0` is the likeliest value there is
/// for a global or European Lambert model.
#[test]
fn a_grid_that_wraps_the_globe_is_not_narrowed() {
    for &(ni, nj, step, lov, label) in &[
        (
            300usize,
            40usize,
            200_000_000u32,
            262_500_000u32,
            "200 km, LoV 262.5",
        ),
        (120, 60, 400_000_000, 262_500_000, "400 km, LoV 262.5"),
        (
            120,
            60,
            400_000_000,
            0,
            "400 km, LoV 0 (seam on the anti-meridian)",
        ),
        (300, 40, 200_000_000, 0, "200 km, LoV 0"),
        (120, 60, 400_000_000, 5_000_000, "400 km, LoV 5"),
        (120, 60, 400_000_000, 355_000_000, "400 km, LoV 355"),
    ] {
        let grid = lambert_grid_stepped(ni, nj, 0b0100_0000, step, lov);
        assert!(
            grid.bounds.max_lon - grid.bounds.min_lon > 180.0,
            "{label}: fixture must actually wrap, spans {}",
            grid.bounds.max_lon - grid.bounds.min_lon,
        );
        assert!(
            grid.coords.wraps_longitude(),
            "{label}: the guard must see the wrap whatever LoV is",
        );
        let every_point = materialised(&grid);

        let mut state = 0x1234_5678_9abc_def1u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };

        for case in 0..300 {
            let (clat, clon) = (next() * 160.0 - 80.0, next() * 360.0 - 180.0);
            let dlat = 0.001 * 20000f64.powf(next());
            let dlon = 0.001 * 20000f64.powf(next());
            let (w, h) = (1 + (next() * 160.0) as u32, 1 + (next() * 160.0) as u32);
            let bounds = GeoBounds {
                min_lat: (clat - dlat / 2.0).max(-85.0),
                max_lat: (clat + dlat / 2.0).min(85.0),
                min_lon: clon - dlon / 2.0,
                max_lon: clon + dlon / 2.0,
            };
            if bounds.min_lat >= bounds.max_lat {
                continue;
            }
            assert_eq!(
                rasterize_model_data(&grid, &bounds, w, h).rgba,
                rasterize_model_data(&every_point, &bounds, w, h).rgba,
                "{label} case {case}: {bounds:?} at {w}x{h}",
            );
        }
    }
}

/// A grid anchored across the projection's own **seam** — the meridian
/// opposite the central one, here 82.5 E. `theta` folds there and only then
/// multiplies by the cone constant, so two `i`-adjacent cells either side of
/// it land a third of a turn apart in the plane: the cell is stretched
/// across the whole texture and no cell-sized reach describes it.
///
/// Distinct from both the wrapping grid above and a seam-crossing *box* —
/// these boxes are small, sit nowhere near the seam, and the grid spans
/// under 3 degrees. With the guard removed this fails 102-138 of 800 cases
/// per anchor; the seam check inside `index_bounds` does not see it, because
/// nothing is wrong with the box.
#[test]
fn a_grid_sitting_across_the_projection_seam_is_not_narrowed() {
    for anchor in [81_000_000u32, 82_400_000, 82_500_000, 83_000_000] {
        let grid = grid_anchored_at(anchor);
        let every_point = materialised(&grid);
        assert!(
            grid.bounds.max_lon - grid.bounds.min_lon < 5.0,
            "fixture must be a small grid, not a wrapping one",
        );

        let mut state = 0x0bad_c0de_1234_5678u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };

        for case in 0..250 {
            let (lon_span, lat_span) = (
                grid.bounds.max_lon - grid.bounds.min_lon,
                grid.bounds.max_lat - grid.bounds.min_lat,
            );
            let clon = grid.bounds.min_lon + lon_span * (next() * 2.0 - 0.5);
            let clat = grid.bounds.min_lat + lat_span * (next() * 2.0 - 0.5);
            let dlon = 0.001 * 3000f64.powf(next());
            let dlat = 0.001 * 3000f64.powf(next());
            let bounds = GeoBounds {
                min_lon: clon - dlon / 2.0,
                max_lon: clon + dlon / 2.0,
                min_lat: (clat - dlat / 2.0).max(-85.0),
                max_lat: (clat + dlat / 2.0).min(85.0),
            };
            if bounds.min_lat >= bounds.max_lat {
                continue;
            }
            let (w, h) = (1 + (next() * 180.0) as u32, 1 + (next() * 180.0) as u32);
            assert_eq!(
                rasterize_model_data(&grid, &bounds, w, h).rgba,
                rasterize_model_data(&every_point, &bounds, w, h).rgba,
                "anchor {anchor} case {case}: {bounds:?} at {w}x{h}",
            );
        }
    }
}

/// At real HRRR scale, once per regime. The small grids above vary the
/// geometry; this pins that 1,905,141 points behave like 5,917.
#[test]
fn the_window_is_invisible_at_full_hrrr_scale() {
    let lambert = lambert_grid(1799, 1059, 0b0100_0000);
    let every_point = materialised(&lambert);

    let cases = [
        (coverage(35.5, -97.5, 12.0), "a typical pane"),
        (
            box_of_cells(&lambert, 900, 530, 0.02, (0.37, -0.29)),
            "well inside one cell, off-lattice",
        ),
        (
            GeoBounds {
                min_lat: 15.0,
                max_lat: 55.0,
                min_lon: -290.0,
                max_lon: -110.0,
            },
            "past the anti-meridian",
        ),
    ];
    for (bounds, what) in cases {
        assert_eq!(
            rasterize_model_data(&lambert, &bounds, 512, 512).rgba,
            rasterize_model_data(&every_point, &bounds, 512, 512).rgba,
            "{what}",
        );
    }
}
