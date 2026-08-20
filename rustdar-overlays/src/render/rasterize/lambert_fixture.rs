use super::*;
use crate::hrrr::{GridCoords, ModelParameter, lambert::LambertGrid, summarize_values};

/// An `ni` x `nj` grid on HRRR's own Lambert projection and 3 km step.
/// `scanning_mode` is HRRR's `0b0100_0000` unless a test wants another.
pub(crate) fn lambert_grid(ni: usize, nj: usize, scanning_mode: u8) -> HrrrGridData {
    lambert_grid_stepped(ni, nj, scanning_mode, 3_000_000, 262_500_000)
}

/// `step` is the grid spacing in micro-metres and `lov` the central meridian
/// in microdegrees. `lov` matters because it places the cone's seam at
/// `lov + 180`: HRRR's 262.5 puts it at 82.5 E, well away from the
/// anti-meridian, while `lov = 0` puts the two on top of each other.
pub(crate) fn lambert_grid_stepped(
    ni: usize,
    nj: usize,
    scanning_mode: u8,
    step: u32,
    lov: u32,
) -> HrrrGridData {
    use grib::def::grib2::template::param_set::ScanningMode;
    let mut template = crate::hrrr::lambert::hrrr_conus_grid();
    template.ni = ni as u32;
    template.nj = nj as u32;
    template.scanning_mode = ScanningMode(scanning_mode);
    template.lov = lov;
    template.dx = step;
    template.dy = step;
    let geometry = LambertGrid::from_template(&template).unwrap();

    let parameter = ModelParameter::SurfaceBasedCape;
    let values: Vec<f32> = (0..ni * nj)
        .map(|k| ((k % 4001) + (k / ni.max(1)) % 997) as f32)
        .collect();
    let (visible_points, value_range) = summarize_values(&values, parameter);

    let mut bounds = GeoBounds {
        min_lat: f64::MAX,
        max_lat: f64::MIN,
        min_lon: f64::MAX,
        max_lon: f64::MIN,
    };
    for k in 0..ni * nj {
        let (lat, lon) = geometry.latlon_at(k).unwrap();
        bounds.min_lat = bounds.min_lat.min(lat);
        bounds.max_lat = bounds.max_lat.max(lat);
        bounds.min_lon = bounds.min_lon.min(lon);
        bounds.max_lon = bounds.max_lon.max(lon);
    }

    HrrrGridData {
        parameter,
        values,
        coords: GridCoords::Lambert(geometry),
        ni,
        nj,
        bounds,
        ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        forecast_hour: 0,
        visible_points,
        value_range,
    }
}

pub(crate) fn materialised(grid: &HrrrGridData) -> HrrrGridData {
    let n = grid.ni * grid.nj;
    let (mut lats, mut lons) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for k in 0..n {
        let (lat, lon) = grid.coords.at(k).expect("a full grid");
        lats.push(lat);
        lons.push(lon);
    }
    HrrrGridData {
        coords: GridCoords::Explicit { lats, lons },
        ..grid.clone()
    }
}

/// A box `cells` grid cells across, centred on grid point `(i, j)`.
pub(crate) fn box_of_cells(
    grid: &HrrrGridData,
    i: usize,
    j: usize,
    cells: f64,
    offset: (f64, f64),
) -> GeoBounds {
    let at = |i: usize, j: usize| grid.coords.at(j * grid.ni + i).expect("in range");
    let (lat, lon) = at(i, j);
    let (nlat, nlon) = at(i + 1, j + 1);
    let (dlat, dlon) = ((nlat - lat).abs(), (nlon - lon).abs());
    // `offset` shifts the centre off the lattice point, in cells. A box
    // centred exactly on a grid point is the one case the window's
    // `floor - 1` / `ceil + 1` rounding covers for free, so an aligned-only
    // sweep cannot see a margin that is too small.
    let (clat, clon) = (lat + dlat * offset.1, lon + dlon * offset.0);
    GeoBounds {
        min_lat: clat - dlat * cells / 2.0,
        max_lat: clat + dlat * cells / 2.0,
        min_lon: clon - dlon * cells / 2.0,
        max_lon: clon + dlon * cells / 2.0,
    }
}

/// Sub-cell offsets a viewport centre can land on, in cells.
pub(crate) const CELL_OFFSETS: &[(f64, f64)] = &[
    (0.0, 0.0),
    (0.5, 0.0),
    (0.0, 0.5),
    (0.5, 0.5),
    (0.37, -0.29),
    (-0.44, 0.18),
];

/// The 97x61 grid moved to `first_point_lon`, keeping HRRR's projection.
/// Used to park a grid on the cone seam at `lon0 + 180 = 82.5 E`.
pub(crate) fn grid_anchored_at(first_point_lon: u32) -> HrrrGridData {
    let mut grid = lambert_grid(97, 61, 0b0100_0000);
    let mut template = crate::hrrr::lambert::hrrr_conus_grid();
    template.ni = 97;
    template.nj = 61;
    template.first_point_lat = 30_000_000;
    template.first_point_lon = first_point_lon;
    let geometry = LambertGrid::from_template(&template).unwrap();
    grid.bounds = GeoBounds {
        min_lat: f64::MAX,
        max_lat: f64::MIN,
        min_lon: f64::MAX,
        max_lon: f64::MIN,
    };
    for k in 0..grid.ni * grid.nj {
        let (lat, lon) = geometry.latlon_at(k).unwrap();
        grid.bounds.min_lat = grid.bounds.min_lat.min(lat);
        grid.bounds.max_lat = grid.bounds.max_lat.max(lat);
        grid.bounds.min_lon = grid.bounds.min_lon.min(lon);
        grid.bounds.max_lon = grid.bounds.max_lon.max(lon);
    }
    grid.coords = GridCoords::Lambert(geometry);
    grid
}

/// A texture's coverage: a lat/lon box centred on `(lat, lon)`.
pub(crate) fn coverage(lat: f64, lon: f64, span: f64) -> GeoBounds {
    GeoBounds {
        min_lat: lat - span / 2.0,
        max_lat: lat + span / 2.0,
        min_lon: lon - span / 2.0,
        max_lon: lon + span / 2.0,
    }
}
