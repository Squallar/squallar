//! GMGSI decode, against the committed granule.
//!
//! `testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc`
//! carries the **real** `lat`/`lon` arrays byte-exact and the real attributes;
//! only `data` is synthetic. See [`super::super`] for the recipe and its cost.
//!
//! Every figure asserted here was read off the granule before the code that
//! reads it existed, so none of them is a re-recording of this decoder's own
//! output.

use super::*;
use crate::render::gridded::color_for;

const GRANULE: &[u8] = include_bytes!(
    "../../../testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
);

/// Grid shape, from the granule's `data(time, yc, xc)` dimensions.
const NY: usize = 3000;
const NX: usize = 5000;

/// `lat[0]`, `lat[500]`, `lat[2999]` and `lon[0]`, `lon[1]`, `lon[4999]`.
const LAT_0: f64 = 72.71540832519531;
const LAT_500: f64 = 58.19306945800781;
const LAT_LAST: f64 = -72.73677062988281;
const LON_0: f64 = 179.99961853027344;
const LON_1: f64 = -179.92837524414063;
const LON_LAST: f64 = 179.97239685058594;

/// Where the fixture plants the real LW equator reading, and what it is.
const EQUATOR_ROW: usize = 1499;
const PRIME_COL: usize = 2500;
const EQUATOR_READING: f32 = 82.0;

/// Where the fixture plants its single `_FillValue`.
const FILL_ROW: usize = 1000;
const FILL_COL: usize = 1000;

fn grid() -> GmgsiGrid {
    decode(GRANULE.to_vec(), GmgsiChannel::LongwaveIr).expect("the committed granule decodes")
}

fn axes(g: &GmgsiGrid) -> (&[f64], &[f64]) {
    match &g.grid.coords {
        GridCoords::Separable { lat_axis, lon_axis } => (lat_axis, lon_axis),
        other => panic!("GMGSI must decode onto Separable, got {other:?}"),
    }
}

#[test]
fn the_granule_decodes_onto_a_separable_grid_of_its_declared_shape() {
    let g = grid();
    assert_eq!((g.grid.nj, g.grid.ni), (NY, NX));
    assert_eq!(g.grid.values.len(), NY * NX);
    let (lat_axis, lon_axis) = axes(&g);
    assert_eq!((lat_axis.len(), lon_axis.len()), (NY, NX));
    assert_eq!(
        g.valid_time,
        chrono::NaiveDate::from_ymd_opt(2025, 6, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    );
}

#[test]
fn len_is_the_product_of_the_two_axes() {
    let g = grid();
    assert_eq!(g.grid.coords.len(), NY * NX);
    assert_eq!(g.grid.coords.len(), 15_000_000);
}

/// C.1's ordering floor.
///
/// A transpose applied to both `at()` and a materialised comparison passes any
/// "the corners match" check while yielding a globe rotated 90 degrees. These
/// two steps are what a transpose cannot survive, and they are a fact of the
/// **file** — the `data(time, yc, xc)` dimension order — not of this code.
#[test]
fn at_walks_the_grid_row_major_with_longitude_fastest() {
    let g = grid();
    let c = &g.grid.coords;

    let (lat0, lon0) = c.at(0).expect("index 0 is in the grid");
    assert!((lat0 - LAT_0).abs() < 1e-4, "at(0) latitude was {lat0}");
    assert!((lon0 - LON_0).abs() < 1e-4, "at(0) longitude was {lon0}");

    // One step of index moves one COLUMN: same latitude, next longitude.
    let (lat1, lon1) = c.at(1).expect("index 1 is in the grid");
    assert_eq!(lat1, lat0, "at(1) must not change latitude");
    assert!((lon1 - LON_1).abs() < 1e-4, "at(1) longitude was {lon1}");

    // One row's worth of index moves one ROW: same longitude, next latitude.
    let (lat_row1, lon_row1) = c.at(NX).expect("index NX is in the grid");
    assert_eq!(lon_row1, lon0, "at(NX) must not change longitude");
    assert!(
        lat_row1 < lat0,
        "at(NX) must step south, went {lat0} -> {lat_row1}"
    );

    let (lat_end, lon_end) = c.at(NY * NX - 1).expect("the last index is in the grid");
    assert!((lat_end - LAT_LAST).abs() < 1e-4, "at(last) lat {lat_end}");
    assert!((lon_end - LON_LAST).abs() < 1e-4, "at(last) lon {lon_end}");
}

/// C.2. Two halves, and the first one goes red the moment the code stops
/// reading the array.
#[test]
fn the_latitude_axis_is_the_granules_own_and_not_the_declared_uniform_one() {
    let g = grid();
    let (lat_axis, _) = axes(&g);

    // Half one: the axis is what the file says.
    assert!(
        (lat_axis[500] - LAT_500).abs() < 1e-4,
        "row 500 read {} rather than the granule's {LAT_500}",
        lat_axis[500]
    );
    let (row_500_lat, _) = g.grid.coords.at(500 * NX).expect("row 500 is in the grid");
    assert!((row_500_lat - LAT_500).abs() < 1e-4);

    // Half two: and that is nowhere near what the declared corners imply.
    // `geospatial_lat_max` 72.71540 and `geospatial_lat_min` -72.73680,
    // interpolated linearly across 2999 steps.
    let uniform =
        72.71540069580078 + (-72.73680114746094 - 72.71540069580078) * (500.0 / (NY - 1) as f64);
    assert!(
        (uniform - 48.4653).abs() < 1e-3,
        "the uniform axis moved: {uniform}"
    );
    assert!(
        (lat_axis[500] - uniform).abs() > 9.0,
        "the real axis and the uniform one are only {} apart at row 500",
        (lat_axis[500] - uniform).abs()
    );
}

/// An explicit **characterization of the fixture**, not a test of this code:
/// nothing in `decode` can change either number. It is here because the
/// declared resolution is the plausible shortcut C.2 exists to refuse, and the
/// size of the gap is the reason.
#[test]
fn fixture_characterization_the_declared_resolution_is_not_the_longitude_step() {
    let g = grid();
    let (_, lon_axis) = axes(&g);
    // Measured across the 4998 steps from column 1 to column 4999. Column 0 is
    // excluded: it sits on the far side of the antimeridian.
    let measured = (lon_axis[NX - 1] - lon_axis[1]) / (NX - 2) as f64;
    assert!(
        (measured - 0.0720089).abs() < 1e-6,
        "longitude step measured {measured}"
    );
    let declared = 0.0722000002861023_f64;
    let drift = (declared - measured) * (NX - 2) as f64;
    assert!(
        (drift - 0.955).abs() < 0.01,
        "the declared resolution drifts {drift} degrees across the axis"
    );
}

/// The separability the representation exploits, characterized in the geometry
/// that explains it: constant in Mercator y, wildly varying in latitude.
#[test]
fn fixture_characterization_the_latitude_axis_is_uniform_in_mercator_y() {
    let g = grid();
    let (lat_axis, _) = axes(&g);
    let merc = |lat: f64| rustdar_geo::lat_rad_to_mercator_y(lat.to_radians());

    let lat_steps: Vec<f64> = (0..NY - 500)
        .step_by(500)
        .map(|j| lat_axis[j + 500] - lat_axis[j])
        .collect();
    let merc_steps: Vec<f64> = (0..NY - 500)
        .step_by(500)
        .map(|j| merc(lat_axis[j + 500]) - merc(lat_axis[j]))
        .collect();

    let spread = |v: &[f64]| {
        let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |a, &b| {
            (a.0.min(b.abs()), a.1.max(b.abs()))
        });
        (lo, hi)
    };
    let (lat_lo, lat_hi) = spread(&lat_steps);
    assert!(
        lat_hi / lat_lo > 2.0,
        "latitude steps {lat_steps:?} were meant to vary by more than 2x"
    );
    let (merc_lo, merc_hi) = spread(&merc_steps);
    assert!(
        merc_hi - merc_lo < 1e-5,
        "Mercator-y steps {merc_steps:?} vary by {}",
        merc_hi - merc_lo
    );
}

#[test]
fn the_envelope_is_the_granules_declared_one() {
    let g = grid();
    // The declared lon_min/lon_max are the axis EXTREMES, not its first and
    // last column: the grid begins a hair west of the antimeridian, so column 0
    // holds the maximum and column 1 the minimum.
    assert!((g.bounds.max_lat - 72.71540069580078).abs() < 1e-4);
    assert!((g.bounds.min_lat - -72.73680114746094).abs() < 1e-4);
    assert!((g.bounds.min_lon - -179.92799377441406).abs() < 1e-3);
    assert!((g.bounds.max_lon - 180.0).abs() < 1e-3);
}

/// `wraps_longitude` returning `false` opens a seam down the antimeridian.
#[test]
fn wraps_longitude_is_true_for_a_global_mosaic() {
    let g = grid();
    assert!(
        g.grid.coords.wraps_longitude(),
        "a 359.93-degree axis of 0.072-degree cells covers the turn"
    );

    // And false for an axis that plainly does not, so the arm is not a
    // constant `true`.
    let regional = GridCoords::Separable {
        lat_axis: vec![40.0, 39.0, 38.0],
        lon_axis: (0..100).map(|i| -100.0 + i as f64 * 0.1).collect(),
    };
    assert!(!regional.wraps_longitude());
}

/// A too-narrow window silently crops the raster, so the assertion is that the
/// bracket **contains** the truth, not that it equals some recorded number.
#[test]
fn index_bounds_brackets_both_axes_of_the_granule() {
    let g = grid();
    let bounds = rustdar_geo::GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -100.0,
        max_lon: -90.0,
    };
    let (i_min, i_max, j_min, j_max) = g
        .grid
        .coords
        .index_bounds(&bounds, NX, NY)
        .expect("a separable grid of its own shape answers");

    let (lat_axis, lon_axis) = axes(&g);
    // Every row inside the box must be inside the bracket.
    for (j, &lat) in lat_axis.iter().enumerate() {
        if (30.0..=40.0).contains(&lat) {
            assert!(
                (j as f64) >= j_min - 1.0 && (j as f64) <= j_max + 1.0,
                "row {j} at {lat} fell outside the bracket {j_min}..{j_max}"
            );
        }
    }
    // And the bracket must be a real saving, not the whole axis back.
    assert!(
        j_max - j_min < 400.0,
        "the latitude bracket {j_min}..{j_max} saved nothing"
    );

    // The same two claims for the longitude axis. It is not monotonic — the
    // granule begins a hair west of the antimeridian, so column 0 holds the
    // maximum — and this box sits nowhere near that fold, so the columns
    // covering it are one contiguous run and the bracket names it.
    for (i, &lon) in lon_axis.iter().enumerate() {
        if (-100.0..=-90.0).contains(&lon) {
            assert!(
                (i as f64) >= i_min - 1.0 && (i as f64) <= i_max + 1.0,
                "column {i} at {lon} fell outside the bracket {i_min}..{i_max}"
            );
        }
    }
    assert!(
        i_max - i_min < 200.0,
        "the longitude bracket {i_min}..{i_max} saved nothing"
    );
}

/// The other half of the longitude axis's two cases: a box that **does** walk
/// over the granule's fold covers two disjoint runs of columns, and one
/// interval that is not narrower than the truth is then the whole axis.
///
/// Its rows are parallels either way, so the latitude bracket must still be a
/// real saving here — that is the narrowing a seam-crossing view keeps.
#[test]
fn index_bounds_widens_only_the_columns_across_the_granules_fold() {
    let g = grid();
    let bounds = rustdar_geo::GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: 179.0,
        max_lon: 181.0,
    };
    let (i_min, i_max, j_min, j_max) = g
        .grid
        .coords
        .index_bounds(&bounds, NX, NY)
        .expect("a separable grid of its own shape answers");
    assert_eq!(
        (i_min, i_max),
        (0.0, (NX - 1) as f64),
        "a box across the fold has no single column interval but all of them",
    );
    assert!(
        j_max - j_min < 400.0,
        "the latitude bracket {j_min}..{j_max} went wide with the columns"
    );
}

#[test]
fn index_bounds_refuses_a_shape_that_is_not_its_own() {
    let g = grid();
    let bounds = rustdar_geo::GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -100.0,
        max_lon: -90.0,
    };
    assert!(g.grid.coords.index_bounds(&bounds, NX, NY - 1).is_none());
    assert!(g.grid.coords.index_bounds(&bounds, NX - 1, NY).is_none());
}

#[test]
fn index_bounds_brackets_both_axes_when_the_longitude_axis_is_monotonic() {
    let coords = GridCoords::Separable {
        lat_axis: (0..100).map(|j| 50.0 - j as f64 * 0.5).collect(),
        lon_axis: (0..200).map(|i| -120.0 + i as f64 * 0.25).collect(),
    };
    let bounds = rustdar_geo::GeoBounds {
        min_lat: 30.0,
        max_lat: 35.0,
        min_lon: -100.0,
        max_lon: -95.0,
    };
    let (i_min, i_max, j_min, j_max) = coords.index_bounds(&bounds, 200, 100).unwrap();
    // lat 35 -> row 30, lat 30 -> row 40; lon -100 -> col 80, -95 -> col 100.
    assert!((j_min - 30.0).abs() < 1e-9, "j_min {j_min}");
    assert!((j_max - 40.0).abs() < 1e-9, "j_max {j_max}");
    assert!((i_min - 80.0).abs() < 1e-9, "i_min {i_min}");
    assert!((i_max - 100.0).abs() < 1e-9, "i_max {i_max}");
}

#[test]
fn nearest_finds_the_cell_and_crosses_the_antimeridian() {
    let g = grid();
    let c = &g.grid.coords;

    // A point on the equator at the prime meridian lands on the planted cell.
    let idx = c.at(EQUATOR_ROW * NX + PRIME_COL).unwrap();
    let found = c.nearest(idx.0, idx.1).expect("its own point is nearest");
    assert_eq!(found, EQUATOR_ROW * NX + PRIME_COL);

    // A longitude a thousandth of a degree east of the antimeridian is spelled
    // as a large NEGATIVE number. Column 0 sits at +179.99962, which is
    // 0.00138 degrees away the short way round and 359.99862 away the long
    // way; column 1 sits at -179.92838, 0.07062 away. Comparing longitudes
    // arithmetically instead of angularly picks column 1 — a whole cell wrong,
    // and wrong in the one place a global grid closes on itself.
    let col = c.nearest(0.0, -179.999).expect("covered") % NX;
    assert_eq!(
        col, 0,
        "a point 0.001 degrees east of the antimeridian chose column {col}, \
         not the column 0.00138 degrees away across the seam"
    );
}

/// `cell_span_degrees` is the pad `projection_window` grows its box by, so an
/// answer that **under-covers** the local cell crops the raster. The property
/// is therefore two-sided: never below either step, never wastefully above.
///
/// The latitude step is local and really does vary — a constant Mercator-y step
/// means `dlat = dy * cos(lat)`, so rows are widest at the equator and narrow
/// towards the poles.
#[test]
fn cell_span_degrees_covers_the_local_cell_at_every_latitude() {
    let g = grid();
    let (lat_axis, _) = axes(&g);
    /// Measured across columns 1..4999.
    const LON_STEP: f64 = 0.0720089;

    for &j in &[1usize, 500, 1499, 2500, NY - 2] {
        let lat = lat_axis[j];
        let span = g
            .grid
            .coords
            .cell_span_degrees(lat)
            .expect("a separable grid answers");
        let local = (lat_axis[j + 1] - lat_axis[j])
            .abs()
            .max((lat_axis[j] - lat_axis[j - 1]).abs());
        assert!(
            span >= local - 1e-9,
            "row {j} at {lat}: span {span} under-covers its own {local}-degree cell"
        );
        assert!(
            span >= LON_STEP - 1e-6,
            "row {j} at {lat}: span {span} under-covers the {LON_STEP}-degree column"
        );
        assert!(
            span <= 2.0 * local.max(LON_STEP),
            "row {j} at {lat}: span {span} is more than twice the cell it pads"
        );
    }

    // And the local step is worth being local about: rows near the equator are
    // more than twice as tall as rows at the top of the grid.
    let step = |j: usize| (lat_axis[j + 1] - lat_axis[j]).abs();
    assert!(
        step(1499) / step(1) > 2.0,
        "equator rows span {} and polar rows {}",
        step(1499),
        step(1)
    );

    // On GMGSI the two steps are within 1e-5 of each other, so the assertions
    // above cannot tell a missing LATITUDE term from a present one. This grid
    // can: its rows are 20x its columns, and only near the middle.
    let tall = GridCoords::Separable {
        lat_axis: (0..40)
            .map(|j| {
                if j < 20 {
                    60.0 - j as f64 * 0.1
                } else {
                    58.0 - (j - 20) as f64 * 2.0
                }
            })
            .collect(),
        lon_axis: (0..40).map(|i| -100.0 + i as f64 * 0.1).collect(),
    };
    let near_the_top = tall.cell_span_degrees(59.0).expect("answers");
    let down_the_tall_part = tall.cell_span_degrees(40.0).expect("answers");
    assert!(
        down_the_tall_part > 1.5,
        "a 2-degree row reported a {down_the_tall_part}-degree cell"
    );
    assert!(
        near_the_top < 0.5,
        "a 0.1-degree row reported a {near_the_top}-degree cell"
    );
}

/// C.4. The assertion is on the painted **alpha**, not on NaN-ness: a ramp
/// mapping NaN to its bottom stop would satisfy a NaN check while painting a
/// black globe.
#[test]
fn the_planted_fill_value_paints_nothing() {
    let g = grid();
    let scale = crate::gmgsi::fields::scale(GmgsiChannel::LongwaveIr);
    let painted = color_for(scale, g.grid.values[FILL_ROW * NX + FILL_COL]);
    assert_eq!(
        painted[3], 0,
        "the fill cell painted {painted:?}; a fill must be invisible, not black"
    );

    // The neighbouring cell is ordinary data and DOES paint, so the assertion
    // above is not satisfied by a ramp that paints nothing anywhere.
    let neighbour = color_for(scale, g.grid.values[FILL_ROW * NX + FILL_COL + 1]);
    assert_ne!(neighbour[3], 0, "the cell beside the fill painted nothing");
}

/// C.4's floor. A healthy granule has no fill at all — all four reference
/// channels report 0 of 15,000,000 — which is exactly why the fixture has to
/// plant one. If the plant is lost, or if fills start leaking in, this moves.
#[test]
fn the_fixture_carries_exactly_the_one_planted_fill() {
    let g = grid();
    let missing = g.grid.values.iter().filter(|v| v.is_nan()).count();
    assert_eq!(
        missing, 1,
        "expected exactly the planted _FillValue at ({FILL_ROW}, {FILL_COL})"
    );
    assert!(g.grid.values[FILL_ROW * NX + FILL_COL].is_nan());
}

#[test]
fn the_planted_equator_reading_survives_the_decode() {
    let g = grid();
    assert_eq!(g.grid.values[EQUATOR_ROW * NX + PRIME_COL], EQUATOR_READING);
    let (lat, lon) = g.grid.coords.at(EQUATOR_ROW * NX + PRIME_COL).unwrap();
    assert!(lat.abs() < 1e-3, "the equator row read {lat}");
    assert!(lon.abs() < 0.1, "the prime column read {lon}");
}

/// A grid whose latitude genuinely varies along a row cannot be described by
/// one axis per dimension, and the failure is invisible: every method still
/// answers. So it is refused.
#[test]
fn a_non_separable_granule_is_refused() {
    let bytes = synthetic_granule(|j, i, ny, nx| {
        let lat = 40.0 - 10.0 * j as f32 / ny as f32;
        // Latitude tilts along the row: not separable.
        (lat + 5.0 * i as f32 / nx as f32, -100.0 + 0.1 * i as f32)
    });
    let err = decode(bytes, GmgsiChannel::LongwaveIr).expect_err("a tilted grid is refused");
    assert!(err.contains("not separable"), "error was {err:?}");
}

/// The same builder with an honestly separable grid decodes, so the refusal
/// above is the tilt and not the fixture builder.
#[test]
fn a_separable_synthetic_granule_decodes() {
    let bytes = synthetic_granule(|j, i, ny, _nx| {
        (40.0 - 10.0 * j as f32 / ny as f32, -100.0 + 0.1 * i as f32)
    });
    let g = decode(bytes, GmgsiChannel::LongwaveIr).expect("a separable grid decodes");
    assert_eq!((g.grid.nj, g.grid.ni), (SYN_NY, SYN_NX));
}

const SYN_NY: usize = 200;
const SYN_NX: usize = 300;

/// A GMGSI-shaped granule with 2-D `lat`/`lon`, small enough to build in a
/// test. `coord` returns `(lat, lon)` for a grid point.
fn synthetic_granule(coord: impl Fn(usize, usize, usize, usize) -> (f32, f32)) -> Vec<u8> {
    let (ny, nx) = (SYN_NY, SYN_NX);
    let mut lat = vec![0f32; ny * nx];
    let mut lon = vec![0f32; ny * nx];
    for j in 0..ny {
        for i in 0..nx {
            let (a, o) = coord(j, i, ny, nx);
            lat[j * nx + i] = a;
            lon[j * nx + i] = o;
        }
    }
    let mut w = hdf5_pure::FileBuilder::new();
    w.set_attr(
        "time_coverage_start",
        hdf5_pure::AttrValue::String("2025-06-01T12:00:00Z".into()),
    );
    {
        let b = w.create_dataset("data");
        b.with_f32_data(&vec![50f32; ny * nx])
            .with_shape(&[1, ny as u64, nx as u64]);
        b.set_attr("_FillValue", hdf5_pure::AttrValue::F64(-9999.0));
        b.set_attr("units", hdf5_pure::AttrValue::String("K".into()));
        b.set_attr(
            "long_name",
            hdf5_pure::AttrValue::String("0-255 Brightness Temperature".into()),
        );
    }
    for (name, vals) in [("lat", &lat), ("lon", &lon)] {
        let b = w.create_dataset(name);
        b.with_f32_data(vals).with_shape(&[ny as u64, nx as u64]);
        b.set_attr("_FillValue", hdf5_pure::AttrValue::F64(2143289344.0));
    }
    w.finish().expect("the synthetic granule builds")
}
