//! The registry answers for every field it registers, refuses every code it
//! does not, and its generic ramp is the one its visibility test describes.

use super::*;
use crate::hrrr::ModelParameter;

/// A sweep wide enough to cross every posture: raw GRIB2 values from deeply
/// negative (CIN, lifted index) through the model's largest (visibility in
/// metres, temperature in kelvin).
fn sweep() -> Vec<f32> {
    let mut values: Vec<f32> = Vec::new();
    let mut v = -1000.0f32;
    while v <= 20_000.0 {
        values.push(v);
        v += if v.abs() < 1000.0 { 0.25 } else { 7.0 };
    }
    values.extend([f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.0]);
    values
}

/// The registry paints a model field exactly as the parameter itself does.
/// This is what licenses `rasterize_gridded` naming only a `FieldId`.
#[test]
fn every_model_field_paints_what_its_parameter_paints() {
    let sweep = sweep();
    assert!(sweep.len() > 5_000, "the sweep must be able to disagree");
    for &p in ModelParameter::all() {
        let paint = paint_for_code(p.as_str())
            .unwrap_or_else(|| panic!("{p:?} is registered under {:?}", p.as_str()));
        assert_eq!(paint.id.as_str(), p.as_str());
        for &v in &sweep {
            assert_eq!(
                paint.color_for_value(v),
                p.color_for_value(v),
                "{p:?} at {v}: the registry's ramp is not the parameter's",
            );
            assert_eq!(
                paint.paints(v),
                p.paints(v),
                "{p:?} at {v}: the registry's visibility test is not the \
                 parameter's",
            );
        }
    }
}

/// The refusal, from both doors. A code this build does not register resolves
/// to nothing rather than to some other field's colours.
#[test]
fn a_code_this_build_does_not_register_is_refused() {
    for code in ["sbcap", "SBCAPE", "", "mrms/reflectivity", "vis "] {
        assert!(
            paint_for_code(code).is_none(),
            "{code:?} resolved to a paint this build never registered",
        );
        assert!(field_scale(&FieldId::new(code)).is_none(), "{code:?}");
    }
    // Control: the lookup is not simply answering `None` to everything.
    assert!(paint_for_code("sbcape").is_some());
    assert!(
        field_scale(&FieldId::from_static("vis")).is_some(),
        "the scale door must resolve a registered code",
    );
}

fn a_scale(is_gradient: bool) -> LegendScale {
    LegendScale {
        thresholds: vec![
            (10.0, [0, 0, 0]),
            (20.0, [100, 100, 100]),
            (40.0, [200, 40, 0]),
        ],
        is_gradient,
        min_value: 10.0,
        max_value: 40.0,
    }
}

/// The generic ramp's three regions: nothing below the first stop, the stops'
/// own colours at the stops, and the last stop's colour held above it.
#[test]
fn the_generic_ramp_fades_out_below_and_clamps_above() {
    let scale = a_scale(true);
    assert_eq!(color_for(&scale, 9.9), [0, 0, 0, 0]);
    assert_eq!(color_for(&scale, f32::NAN), [0, 0, 0, 0]);
    assert_eq!(color_for(&scale, f32::NEG_INFINITY), [0, 0, 0, 0]);
    assert_eq!(color_for(&scale, f32::INFINITY), [0, 0, 0, 0]);

    assert_eq!(color_for(&scale, 10.0), [0, 0, 0, ALPHA]);
    assert_eq!(color_for(&scale, 20.0), [100, 100, 100, ALPHA]);
    assert_eq!(color_for(&scale, 40.0), [200, 40, 0, ALPHA]);
    assert_eq!(
        color_for(&scale, 4_000.0),
        [200, 40, 0, ALPHA],
        "above the last stop the ramp holds, it does not wrap or fade",
    );

    // Half way between the first two stops.
    assert_eq!(color_for(&scale, 15.0), [50, 50, 50, ALPHA]);
}

/// `is_gradient: false` paints the band's own colour across the band rather
/// than interpolating into the next one.
#[test]
fn a_banded_scale_paints_flat_bands() {
    let banded = a_scale(false);
    let ramped = a_scale(true);
    assert_eq!(color_for(&banded, 15.0), [0, 0, 0, ALPHA]);
    assert_eq!(color_for(&banded, 19.9), [0, 0, 0, ALPHA]);
    assert_eq!(color_for(&banded, 20.0), [100, 100, 100, ALPHA]);
    assert_ne!(
        color_for(&banded, 15.0),
        color_for(&ramped, 15.0),
        "the two flags produced the same picture, so the flag is not read",
    );
}

/// The cheap visibility test and the ramp itself agree everywhere, which is
/// what lets `summarize_values` ask the cheap one.
#[test]
fn the_generic_visibility_test_agrees_with_the_generic_ramp() {
    let mut agreed_both_ways = (false, false);
    for scale in [a_scale(true), a_scale(false)] {
        for &v in &sweep() {
            let painted = color_for(&scale, v)[3] != 0;
            assert_eq!(
                paints_over_scale(&scale, v),
                painted,
                "the short-circuit and the ramp disagree at {v}",
            );
            if painted {
                agreed_both_ways.0 = true;
            } else {
                agreed_both_ways.1 = true;
            }
        }
    }
    assert_eq!(
        agreed_both_ways,
        (true, true),
        "the sweep never produced both answers, so the agreement is vacuous",
    );
}

/// An empty colour bar paints nothing rather than indexing past its own end.
#[test]
fn a_scale_with_no_stops_paints_nothing() {
    let empty = LegendScale {
        thresholds: Vec::new(),
        is_gradient: true,
        min_value: 0.0,
        max_value: 1.0,
    };
    assert_eq!(color_for(&empty, 5.0), [0, 0, 0, 0]);
    assert!(!paints_over_scale(&empty, 5.0));
}

/// `over_scale` is the registration a source with no bespoke ramp uses, and it
/// is the generic pair — not a third behaviour.
#[test]
fn over_scale_registers_the_generic_pair() {
    static ID: FieldId = FieldId::from_static("test/field");
    static SCALE: LazyLock<LegendScale> = LazyLock::new(|| a_scale(true));
    let paint = FieldPaint::over_scale(&ID, &SCALE);
    for &v in &sweep() {
        assert_eq!(paint.color_for_value(v), color_for(&SCALE, v), "at {v}");
        assert_eq!(paint.paints(v), paints_over_scale(&SCALE, v), "at {v}");
    }
    assert_eq!(paint.id.as_str(), "test/field");
}

/// The refusal reaches the raster: a window naming a field this build does not
/// register paints **nothing**, and the identical window naming a registered
/// one paints, so the blank is the refusal rather than an empty fixture.
#[test]
fn a_raster_of_an_unregistered_field_paints_nothing() {
    use crate::render::rasterize::{GridWindow, GriddedInput, IndexWindow, rasterize_gridded};
    use rustdar_geo::GeoBounds;

    let bounds = GeoBounds {
        min_lat: 34.9,
        max_lat: 35.2,
        min_lon: -97.2,
        max_lon: -96.9,
    };
    let window = |field: FieldId| {
        GriddedInput::Window(GridWindow {
            field,
            ni: 2,
            nj: 2,
            coords: crate::hrrr::GridCoords::Explicit {
                lats: vec![35.1, 35.1, 35.0, 35.0],
                lons: vec![-97.1, -97.0, -97.1, -97.0],
            },
            win: IndexWindow {
                i0: 0,
                i1: 2,
                j0: 0,
                j1: 2,
            },
            values: vec![4000.0; 4],
        })
    };
    let painted = |input| {
        rasterize_gridded(&input, &bounds, 64, 64)
            .rgba
            .chunks_exact(4)
            .filter(|px| px[3] > 0)
            .count()
    };
    assert!(
        painted(window(FieldId::from_static("sbcape"))) > 0,
        "control: the registered field must paint, or the zero below is the \
         fixture rather than the refusal",
    );
    assert_eq!(
        painted(window(FieldId::new("sbcape/v2"))),
        0,
        "a field this build does not register was painted anyway",
    );
}
