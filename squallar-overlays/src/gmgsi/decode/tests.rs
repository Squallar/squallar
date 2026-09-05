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
    let merc = |lat: f64| squallar_geo::lat_rad_to_mercator_y(lat.to_radians());

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
    let bounds = squallar_geo::GeoBounds {
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
    let bounds = squallar_geo::GeoBounds {
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
    let bounds = squallar_geo::GeoBounds {
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
    let bounds = squallar_geo::GeoBounds {
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
    let painted = color_for(scale, g.grid.values.get(FILL_ROW * NX + FILL_COL).unwrap());
    assert_eq!(
        painted[3], 0,
        "the fill cell painted {painted:?}; a fill must be invisible, not black"
    );

    // The neighbouring cell is ordinary data and DOES paint, so the assertion
    // above is not satisfied by a ramp that paints nothing anywhere.
    let neighbour = color_for(
        scale,
        g.grid.values.get(FILL_ROW * NX + FILL_COL + 1).unwrap(),
    );
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
    assert!(
        g.grid
            .values
            .get(FILL_ROW * NX + FILL_COL)
            .unwrap()
            .is_nan()
    );
}

#[test]
fn the_planted_equator_reading_survives_the_decode() {
    let g = grid();
    assert_eq!(
        g.grid.values.get(EQUATOR_ROW * NX + PRIME_COL).unwrap(),
        EQUATOR_READING
    );
    let (lat, lon) = g.grid.coords.at(EQUATOR_ROW * NX + PRIME_COL).unwrap();
    assert!(lat.abs() < 1e-3, "the equator row read {lat}");
    assert!(lon.abs() < 0.1, "the prime column read {lon}");
}

/// Every bit of the decoded granule, so a change to how it is decoded has to
/// be a change to what is decoded before it can pass.
///
/// The value was recorded on `b438a960`, **before** `squallar_netcdf`'s raw
/// domain stopped being a `Vec<f64>`, and is unchanged by it — which is the
/// whole claim that narrowing changes nothing on screen. It covers the raster
/// and both axes because the raster alone would miss a coordinate regression,
/// and it hashes `to_bits` because a NaN is not `==` itself and a fill cell is
/// exactly what a comparison would drop.
///
/// **A moved digest is a decode that changed, not a pin to re-record.**
#[test]
fn the_decoded_granule_is_bit_for_bit_what_it_was() {
    /// FNV-1a over the little-endian bytes. Chosen because it is four lines
    /// and has no dependency; the property wanted is "any changed bit changes
    /// the output", not cryptographic strength.
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x100_0000_01b3;

    let g = grid();
    let (lat_axis, lon_axis) = axes(&g);

    let mut h = OFFSET_BASIS;
    let mut feed = |bits: u64| {
        for byte in bits.to_le_bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(PRIME);
        }
    };
    for v in g.grid.values.iter() {
        feed(u64::from(v.to_bits()));
    }
    for v in lat_axis.iter().chain(lon_axis.iter()) {
        feed(v.to_bits());
    }

    assert_eq!(
        h, 0xb4f6_9c0c_7031_beff,
        "the committed granule decoded to different bits than it did on \
         b438a960. That is a change to the decode, not a stale pin",
    );
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
    synthetic_granule_with_data(coord, &vec![50f32; SYN_NY * SYN_NX])
}

/// [`synthetic_granule`] carrying `data` the caller chose — the door the
/// granules the product has never published come through.
fn synthetic_granule_with_data(
    coord: impl Fn(usize, usize, usize, usize) -> (f32, f32),
    data: &[f32],
) -> Vec<u8> {
    let (ny, nx) = (SYN_NY, SYN_NX);
    assert_eq!(data.len(), ny * nx, "the fixture's data must fill the grid");
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
        b.with_f32_data(data).with_shape(&[1, ny as u64, nx as u64]);
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

// -- The axis cache ---------------------------------------------------------

/// **The second granule that stores the same coordinate arrays is handed
/// the axes without a read, and they are the axes.**
///
/// Against a cache of this test's own: the shipped one is process-global and
/// every other test in this binary decodes the fixture through it.
#[test]
fn a_granule_storing_the_same_coordinate_arrays_is_handed_the_remembered_axes() {
    let cache = AxisCache::new();
    let first = decode_in(
        GRANULE.to_vec(),
        GmgsiChannel::LongwaveIr,
        crate::gmgsi::staging::global(),
        &cache,
    )
    .expect("decodes");
    assert_eq!(
        cache.totals(),
        AxisCacheTotals { hits: 0, misses: 2 },
        "premise: the first granule read both arrays",
    );

    let second = decode_in(
        GRANULE.to_vec(),
        GmgsiChannel::LongwaveIr,
        crate::gmgsi::staging::global(),
        &cache,
    )
    .expect("decodes");
    assert_eq!(
        cache.totals(),
        AxisCacheTotals { hits: 2, misses: 2 },
        "the second granule stores the same arrays byte for byte and must be \
         handed both axes without a read",
    );
    let (lat_a, lon_a) = axes(&first);
    let (lat_b, lon_b) = axes(&second);
    assert_eq!(
        lat_a
            .iter()
            .chain(lon_a)
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        lat_b
            .iter()
            .chain(lon_b)
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        "and they are the axes the read produced, bit for bit",
    );
    assert!(
        (lat_b[500] - LAT_500).abs() < 1e-4 && (lon_b[1] - LON_1).abs() < 1e-4,
        "and those are the granule's own",
    );
}

/// **A granule whose stored geometry differs is read, not remembered** —
/// and a granule whose coordinate variables cannot be fingerprinted at all
/// (the synthetic one stores them contiguous, not chunked) is read every
/// time. Either way the axes are that granule's, never the last one's.
#[test]
fn a_granule_storing_different_coordinate_arrays_is_read_and_gets_its_own_axes() {
    let cache = AxisCache::new();
    let real = decode_in(
        GRANULE.to_vec(),
        GmgsiChannel::LongwaveIr,
        crate::gmgsi::staging::global(),
        &cache,
    )
    .expect("decodes");
    let (real_lat, _) = axes(&real);
    assert_eq!(cache.totals().misses, 2);

    let synthetic = synthetic_granule(|j, i, ny, _nx| {
        (40.0 - 10.0 * j as f32 / ny as f32, -100.0 + 0.1 * i as f32)
    });
    let syn = decode_in(
        synthetic.clone(),
        GmgsiChannel::LongwaveIr,
        crate::gmgsi::staging::global(),
        &cache,
    )
    .expect("a separable synthetic granule decodes");
    assert_eq!(
        cache.totals(),
        AxisCacheTotals { hits: 0, misses: 4 },
        "a different granule must miss on both axes",
    );
    let (syn_lat, syn_lon) = axes(&syn);
    assert_eq!((syn_lat.len(), syn_lon.len()), (SYN_NY, SYN_NX));
    assert!(
        (syn_lat[0] - 40.0).abs() < 1e-5 && (syn_lon[10] - (-99.0)).abs() < 1e-4,
        "the synthetic granule's axes are its own: lat[0] {} lon[10] {}",
        syn_lat[0],
        syn_lon[10]
    );
    assert_ne!(
        real_lat.len(),
        syn_lat.len(),
        "premise: the two granules' geometries differ, so a remembered axis \
         would have been the wrong length as well as the wrong values",
    );

    // Contiguous coordinate variables carry no fingerprint, so the same
    // synthetic granule again is a miss again, and still correct.
    let again = decode_in(
        synthetic,
        GmgsiChannel::LongwaveIr,
        crate::gmgsi::staging::global(),
        &cache,
    )
    .expect("decodes");
    assert_eq!(cache.totals(), AxisCacheTotals { hits: 0, misses: 6 });
    assert_eq!(axes(&again).0.len(), SYN_NY);
}

// -- The staging slot, through the decode ------------------------------------

/// **A granule whose shape is not the constant the pool was declared with is
/// still decoded into the retained buffer.**
///
/// This is the shipping defect at the level it actually bites: not a buffer
/// handed to a pool by a test, but `decode_in` taking a buffer for the shape
/// it read off `data` and handing it back afterwards. Every GMGSI granule
/// dated 2026-09-03 is `[1, 3000, 4999]` against a pool declared at
/// `3000 * 5000`, so before the shape key every one of them allocated a fresh
/// 60 MB block and every recycle was refused. The synthetic granule stands in
/// for that here because it can be built in a test; what makes it the same
/// case is only that its point count is not [`crate::gmgsi::GRID_POINTS`].
///
/// Observed red before the fix: `allocated` 2, `reused` 0, `declined` 1.
#[test]
fn a_granule_that_is_not_the_declared_shape_is_decoded_into_the_retained_buffer() {
    let separable = |j: usize, i: usize, ny: usize, _nx: usize| {
        (40.0 - 10.0 * j as f32 / ny as f32, -100.0 + 0.1 * i as f32)
    };
    // A pool declared exactly as the shipped one is, so the only thing this
    // test changes is the shape of the granule going through it.
    let pool: crate::gmgsi::staging::StagingPool =
        crate::gmgsi::staging::StagingPool::new(crate::gmgsi::staging::STAGING_POINTS);
    let cache = AxisCache::new();
    assert_ne!(
        SYN_NY * SYN_NX,
        pool.nominal_points(),
        "premise: the granule's shape is not the one the pool was declared for",
    );

    let first = decode_in(
        synthetic_granule(separable),
        GmgsiChannel::LongwaveIr,
        &pool,
        &cache,
    )
    .expect("a separable granule decodes");
    assert_eq!(
        pool.totals().allocated,
        1,
        "premise: the first granule of a process has nothing to be handed",
    );
    let crate::render::gridded::GridValues::Bytes(raster) = &first.grid.values else {
        panic!("a GMGSI raster is a byte store");
    };
    assert_eq!(raster.codes().len(), SYN_NY * SYN_NX);
    let address = raster.codes().as_ptr() as usize;

    // The eviction the frame cache performs on the next arrival.
    crate::gmgsi::staging::recycle(&pool, first.grid);
    assert_eq!(
        pool.totals().declined,
        0,
        "the decode's own buffer must reach the slot: it is the shape the \
         product publishes, whatever the constant says",
    );
    assert_eq!(pool.retained_points(), SYN_NY * SYN_NX);

    let second = decode_in(
        synthetic_granule(separable),
        GmgsiChannel::LongwaveIr,
        &pool,
        &cache,
    )
    .expect("a separable granule decodes");
    assert_eq!(
        pool.totals(),
        crate::gmgsi::staging::StagingTotals {
            allocated: 1,
            reused: 1,
            declined: 0
        },
        "the second granule must be decoded into the first one's block",
    );
    assert_eq!(
        pool.health(),
        crate::gmgsi::staging::StagingHealth::Reusing,
        "and the pool must read as working",
    );
    let crate::render::gridded::GridValues::Bytes(reused) = &second.grid.values else {
        panic!("a GMGSI raster is a byte store");
    };
    assert_eq!(
        reused.codes().as_ptr() as usize,
        address,
        "and it must be the same block, not a fresh one of the same size: a \
         pool that reallocated would satisfy every count above and leave the \
         fragmentation it exists to remove exactly as it was",
    );
    assert_eq!(
        reused.codes().len(),
        SYN_NY * SYN_NX,
        "with the decode's own values in it and nothing inherited",
    );
    assert!(
        reused.codes().iter().all(|&c| c == 50),
        "the synthetic granule's `data` is 50.0 everywhere, which is code 50; \
         a code that is not is content the decode did not write",
    );
}

// -- The width the values are ----------------------------------------------

/// **Every value of the committed granule is a byte, and the raster costs one
/// byte a point.**
///
/// The losslessness proof, of the same shape MRMS's
/// (`mrms::decode::tests::every_mosaic_value_is_a_sixteen_bit_code_and_three_scalars`)
/// takes and against the same standard: **every stored value maps to a code
/// and back to the identical bit pattern**, over all 15,000,000 points, with
/// the reference read independently through the very call the decoder used
/// before this — `squallar_netcdf`'s own `read_unpacked_f32_into` — rather
/// than against a figure recorded from this code's output.
///
/// `to_bits` and not `==`, because the planted `_FillValue` is a `NaN` and a
/// `NaN` is not equal to itself: a value comparison would call the two grids
/// identical at exactly the point where a narrowing bug would live.
///
/// **Red on the tree before this change**, at the last assertion:
/// `resident_bytes` 60,000,000 against 15,000,004.
#[test]
fn every_value_of_the_granule_is_a_byte_and_the_raster_costs_one_byte_a_point() {
    let g = grid();
    let crate::render::gridded::GridValues::Bytes(store) = &g.grid.values else {
        panic!(
            "the committed granule must take the byte arm; it took {} bytes a \
             sample over {} points, so the raster costs {} B",
            g.grid.values.bytes_per_sample(),
            g.grid.values.len(),
            g.grid.values.resident_bytes(),
        );
    };

    // The reference: the same variable, read wide, by the layer that has
    // always read it.
    let granule = squallar_netcdf::Granule::from_vec(GRANULE.to_vec()).expect("the fixture opens");
    let mut wide: Vec<f32> = Vec::new();
    let read = granule
        .read_unpacked_f32_into("data", &mut wide)
        .expect("`data` reads")
        .expect("`data` is there");
    assert_eq!(read, NY * NX, "premise: the reference is the whole raster");
    assert_eq!(store.codes().len(), NY * NX);

    let mut absent = 0usize;
    for (k, reference) in wide.iter().enumerate() {
        let read_back = store.get(k).expect("every point is in the store");
        assert_eq!(
            read_back.to_bits(),
            reference.to_bits(),
            "point {k} was stored as {reference} ({:#010x}) and reads back as \
             {read_back} ({:#010x})",
            reference.to_bits(),
            read_back.to_bits(),
        );
        if reference.is_nan() {
            absent += 1;
        }
    }
    assert_eq!(
        absent,
        store.absent().len(),
        "and every missing point is one the store knows is missing",
    );
    assert_eq!(absent, 1, "premise: the fixture's one planted _FillValue");

    // **What the change is for.** 15,000,000 B of codes plus 4 B for the one
    // absent point's index, against 60,000,000 B as `f32`: 3.9999989x.
    assert_eq!(g.grid.values.resident_bytes(), 15_000_004);
    assert_eq!(g.grid.values.bytes_per_sample(), 1);
}

/// **The narrowed raster paints exactly the picture the wide one painted.**
///
/// The claim `every_value_of_the_granule_is_a_byte_...` makes about bits, made
/// about pixels: a colour lookup on a widened byte and on the original float
/// must agree, and this rasterizes both grids through the shipped path and
/// compares the RGBA byte for byte. Not against a recorded digest — against
/// the wide grid built from the same granule's own values in the same run, so
/// there is no pin to go stale and no reference this code produced.
///
/// The window covers the whole mosaic, so every code the granule carries — the
/// fill included — reaches a pixel.
#[test]
fn the_narrowed_raster_paints_the_same_pixels_as_the_wide_one() {
    use crate::render::rasterize::{GriddedInput, rasterize_gridded};

    let g = grid();
    let wide = crate::render::gridded::ResidentGrid {
        field: g.grid.field.clone(),
        ni: g.grid.ni,
        nj: g.grid.nj,
        coords: g.grid.coords.clone(),
        // `to_f32` is the store's own widening, which is the thing under test
        // only if it agrees with the file; `every_value_of_the_granule_...`
        // is what pins it against the file.
        values: crate::render::gridded::GridValues::F32(g.grid.values.to_f32()),
    };
    let bounds = g.bounds;
    let (w, h) = (240, 160);

    let narrow_px = rasterize_gridded(
        &GriddedInput::Resident(std::sync::Arc::new(g.grid)),
        &bounds,
        w,
        h,
    );
    let wide_px = rasterize_gridded(
        &GriddedInput::Resident(std::sync::Arc::new(wide)),
        &bounds,
        w,
        h,
    );

    assert_eq!(narrow_px.rgba.len(), (w * h * 4) as usize);
    assert_eq!(narrow_px.rgba.len(), wide_px.rgba.len());
    assert_eq!(
        (narrow_px.blank, narrow_px.alpha),
        (wide_px.blank, wide_px.alpha),
        "the two rasters must agree about whether they drew and how opaquely",
    );
    assert!(
        narrow_px.rgba.iter().any(|&b| b != 0),
        "premise: the raster drew something, so an equal pair is not two blank \
         textures agreeing",
    );
    let differing = narrow_px
        .rgba
        .iter()
        .zip(wide_px.rgba.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        differing,
        0,
        "{differing} of {} subpixels differ between the byte store and the f32 \
         store of the same granule",
        narrow_px.rgba.len(),
    );
}

/// **A granule carrying `_FillValue` keeps the byte arm**, with the missing
/// points beside the codes rather than inside them.
///
/// The corpus has never produced one — `n_fill = 0` on all 24 real granules —
/// which is not the same as "it cannot happen": the variable declares
/// `_FillValue = -9999` precisely so the producer may emit it. So the fixture
/// is built rather than found.
///
/// The reading at an absent point is `NaN` and paints nothing, and the codes
/// on either side of it are untouched — a fill that shifted the raster by one
/// point would be a globe misdrawn everywhere after it.
#[test]
fn a_granule_carrying_fill_values_keeps_the_byte_arm_and_paints_nothing_there() {
    let separable = |j: usize, i: usize, ny: usize, _nx: usize| {
        (40.0 - 10.0 * j as f32 / ny as f32, -100.0 + 0.1 * i as f32)
    };
    let mut data: Vec<f32> = (0..SYN_NY * SYN_NX).map(|k| (k % 250) as f32).collect();
    // Scattered, not a run: the absent set is a set of indices and a run would
    // not tell a sorted list from an accidentally ordered one.
    let planted = [7usize, 4001, 4002, 59_999];
    for &k in &planted {
        data[k] = -9999.0;
    }

    let g = decode(
        synthetic_granule_with_data(separable, &data),
        GmgsiChannel::LongwaveIr,
    )
    .expect("a granule with fills decodes");
    let crate::render::gridded::GridValues::Bytes(store) = &g.grid.values else {
        panic!("four absent points is inside the bound, so the byte arm holds");
    };
    assert_eq!(
        store.absent(),
        planted.iter().map(|&k| k as u32).collect::<Vec<_>>(),
        "the absent points are the planted ones, in order",
    );
    assert_eq!(
        g.grid.values.resident_bytes(),
        SYN_NY * SYN_NX + planted.len() * size_of::<u32>(),
        "one byte a point, plus four bytes an absent point",
    );

    let scale = crate::gmgsi::fields::scale(GmgsiChannel::LongwaveIr);
    for &k in &planted {
        assert!(
            g.grid.values.get(k).unwrap().is_nan(),
            "point {k} was written as _FillValue and must read back missing",
        );
        assert_eq!(
            color_for(scale, g.grid.values.get(k).unwrap())[3],
            0,
            "and must paint nothing",
        );
    }
    // Every other point is its own reading, so the fill displaced nothing.
    for k in [0usize, 6, 8, 4000, 4003, 30_000, 59_998] {
        assert_eq!(g.grid.values.get(k).unwrap(), (k % 250) as f32);
    }
}

/// **More absent points than the store carries: the whole granule goes wide,
/// exactly, rather than losing any of them.**
///
/// The other side of the trade `ByteCodes` states. A granule with a real
/// coverage gap has thousands of missing points and is not one whose absences
/// are a small reserved set; it takes four times the bytes and every value
/// still reads back bit for bit, which is the only property that may not be
/// traded.
#[test]
fn a_granule_with_more_fills_than_the_store_carries_is_decoded_wide_and_exactly() {
    use crate::render::gridded::MAX_ABSENT_POINTS;
    let separable = |j: usize, i: usize, ny: usize, _nx: usize| {
        (40.0 - 10.0 * j as f32 / ny as f32, -100.0 + 0.1 * i as f32)
    };
    let mut data: Vec<f32> = (0..SYN_NY * SYN_NX).map(|k| (k % 250) as f32).collect();
    for k in 0..=MAX_ABSENT_POINTS {
        data[k * 3] = -9999.0;
    }

    let g = decode(
        synthetic_granule_with_data(separable, &data),
        GmgsiChannel::LongwaveIr,
    )
    .expect("a granule with many fills decodes");
    let crate::render::gridded::GridValues::F32(raster) = &g.grid.values else {
        panic!("{} absent points is past the bound", MAX_ABSENT_POINTS + 1);
    };
    assert_eq!(raster.len(), SYN_NY * SYN_NX);
    assert_eq!(
        g.grid.values.resident_bytes(),
        SYN_NY * SYN_NX * size_of::<f32>(),
        "and it costs four bytes a point, which the census reports as it is",
    );
    for (k, v) in data.iter().enumerate() {
        let expected = if *v == -9999.0 { f32::NAN } else { *v };
        assert_eq!(
            raster[k].to_bits(),
            expected.to_bits(),
            "point {k} of a wide granule must be its own value",
        );
    }
}

/// **A granule whose values are not byte-exact is decoded wide, bit for bit,
/// from the value that first is not.**
///
/// The narrowing is not a bet that the product keeps publishing integers: it
/// is a question asked of every value on the way past. A granule carrying a
/// half-count, a negative, a value past 255 or a negative zero — none of which
/// survives a round trip through a byte — takes `GridValues::F32` **whole**,
/// including the byte-valued points before and after the one that failed.
///
/// `-0.0` is in the list deliberately. It is numerically zero and would pass
/// any `fract() == 0 && 0..=255` test, and it is not zero's bit pattern, so a
/// store that narrowed it would fail this suite's own `to_bits` standard.
#[test]
fn a_granule_whose_values_are_not_byte_exact_is_decoded_wide_and_bit_for_bit() {
    let separable = |j: usize, i: usize, ny: usize, _nx: usize| {
        (40.0 - 10.0 * j as f32 / ny as f32, -100.0 + 0.1 * i as f32)
    };
    for (name, offender) in [
        ("a half count", 82.5f32),
        ("a negative", -3.0),
        ("past the byte", 300.0),
        ("a negative zero", -0.0),
    ] {
        let mut data: Vec<f32> = (0..SYN_NY * SYN_NX).map(|k| (k % 250) as f32).collect();
        // In the middle, so the arm has to carry the codes it already took.
        data[SYN_NY * SYN_NX / 2] = offender;

        let g = decode(
            synthetic_granule_with_data(separable, &data),
            GmgsiChannel::LongwaveIr,
        )
        .unwrap_or_else(|e| panic!("{name} must decode, not be refused: {e}"));
        let crate::render::gridded::GridValues::F32(raster) = &g.grid.values else {
            panic!("{name} ({offender}) is not a byte code, so the grid is wide");
        };
        assert_eq!(raster.len(), SYN_NY * SYN_NX);
        let differing = raster
            .iter()
            .zip(data.iter())
            .filter(|(a, b)| a.to_bits() != b.to_bits())
            .count();
        assert_eq!(
            differing, 0,
            "{name}: {differing} values of a wide granule differ from the file",
        );
    }
}
