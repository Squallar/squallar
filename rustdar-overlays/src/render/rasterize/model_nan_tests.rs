use super::*;
use crate::hrrr::ModelParameter;

const BOUNDS: GeoBounds = GeoBounds {
    min_lat: 34.9,
    max_lat: 35.2,
    min_lon: -97.2,
    max_lon: -96.9,
};

/// 2x2 over `BOUNDS`, summarised the way the fetch path does.
fn grid(parameter: ModelParameter, values: Vec<f32>) -> HrrrGridData {
    let (visible_points, value_range) = crate::hrrr::summarize_values(&values, parameter);
    HrrrGridData {
        parameter,
        values,
        coords: crate::hrrr::GridCoords::Explicit {
            lats: vec![35.1, 35.1, 35.0, 35.0],
            lons: vec![-97.1, -97.0, -97.1, -97.0],
        },
        ni: 2,
        nj: 2,
        bounds: BOUNDS,
        ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap(),
        forecast_hour: parameter.forecast_hour(),
        visible_points,
        value_range,
    }
}

fn painted_pixels(grid: &HrrrGridData) -> usize {
    let input = ModelDataInput::Whole(std::sync::Arc::new(grid.clone()));
    let out = rasterize_model_data(&input, &BOUNDS, 64, 64);
    out.rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
}

/// Fails if a NaN grid point paints. The ramps in `hrrr/mod.rs` are
/// descending `if` chains ending in an unguarded `else`: NaN fails every
/// comparison and lands there, where `f32::min` returns the non-NaN
/// operand — so an unguarded missing point renders as the *most extreme*
/// value, under a tooltip that `format_value` leaves blank.
///
/// Unreachable while every HRRR field ships Section 6
/// `bitmap_indicator = 255`; live the moment NOMADS ships a bitmapped one.
#[test]
fn a_missing_grid_point_paints_nothing() {
    for parameter in ModelParameter::all() {
        let all_nan = grid(*parameter, vec![f32::NAN; 4]);
        assert_eq!(
            painted_pixels(&all_nan),
            0,
            "{} painted a missing grid point",
            parameter.display_name(),
        );
    }
}

#[test]
fn the_fixture_paints_when_values_are_present() {
    let alarming = grid(ModelParameter::SurfaceBasedCin, vec![-400.0; 4]);
    assert!(
        painted_pixels(&alarming) > 0,
        "fixture must draw a real field, or the NaN test proves nothing",
    );
}

/// Fails if NaN is merely *some* colour rather than fully transparent.
#[test]
fn nan_does_not_take_the_extreme_branch_of_any_ramp() {
    for parameter in ModelParameter::all() {
        let nan = parameter.color_for_value(f32::NAN);
        assert_eq!(
            nan,
            [0, 0, 0, 0],
            "{} maps a missing point to a visible colour",
            parameter.display_name(),
        );

        // A value that legitimately saturates this ramp's top branch.
        let extreme = match parameter {
            ModelParameter::SurfaceBasedCin | ModelParameter::MixedLayerCin => -600.0,
            ModelParameter::LiftedIndex => -20.0,
            ModelParameter::Visibility => 0.0,
            ModelParameter::Temperature2m => 400.0,
            _ => 10_000.0,
        };
        assert_ne!(
            parameter.color_for_value(extreme),
            nan,
            "{}: a missing point is indistinguishable from a saturated one",
            parameter.display_name(),
        );
    }
}

/// Infinities take the same path as NaN through the ramps.
#[test]
fn infinite_values_paint_nothing_either() {
    for parameter in ModelParameter::all() {
        for value in [f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                parameter.color_for_value(value),
                [0, 0, 0, 0],
                "{} painted {value}",
                parameter.display_name(),
            );
        }
    }
}

/// `px_coords` is NaN-padded when `ni * nj` exceeds the coordinate arrays;
/// those points must be skipped, not projected somewhere arbitrary.
///
/// A padded point is a *neighbour* of a real one, and neighbour spacing is
/// what sizes each cell. NaN survives that arithmetic and falls out at the
/// 0.5 px floor via `f32::max`; any real coordinate — `(0.0, 0.0)` being
/// the obvious wrong choice — stretches the cell across the texture
/// instead. Hence the bottom-row assertion: the four real points all sit in
/// the upper two thirds.
#[test]
fn a_grid_shape_mismatch_does_not_paint_padded_points() {
    let mut g = grid(ModelParameter::SurfaceBasedCin, vec![-400.0; 4]);
    // Claim 4x4 while supplying only 4 coordinates and 4 values.
    g.ni = 4;
    g.nj = 4;
    let out = rasterize_model_data(
        &ModelDataInput::Whole(std::sync::Arc::new(g)),
        &BOUNDS,
        64,
        64,
    );
    assert_eq!(out.rgba.len(), 64 * 64 * 4, "must not overrun the buffer");

    let bottom_row = &out.rgba[(63 * 64 * 4)..];
    assert_eq!(
        bottom_row.chunks_exact(4).filter(|px| px[3] > 0).count(),
        0,
        "a padded neighbour stretched a cell to the bottom of the texture",
    );
    assert!(
        out.rgba.chunks_exact(4).any(|px| px[3] > 0),
        "control: the four real points must still paint",
    );
}
