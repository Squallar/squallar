//! The HRRR parameters projected into the substrate's read contract:
//! [`rustdar_source::product::ProductSpec`].
//!
//! A projection, not a second table — every value is read from
//! [`ModelParameter`]'s own accessors. The colour scales are built once here
//! because [`ModelParameter::legend_thresholds`] allocates a fresh `Vec` per
//! call and `ProductSpec::scale` is a borrow.

use std::sync::LazyLock;

use rustdar_source::product::{FieldId, LegendScale, ProductSpec};
use rustdar_units::Quantity;

use super::ModelParameter;

/// The group label every model parameter files under.
pub const GROUP: &str = "HRRR parameters";

/// Every parameter's colour bar, built once.
///
/// **`is_gradient` is per parameter, and it is read from
/// [`ModelParameter::is_banded`] rather than asserted here.** It was `true` for
/// all sixteen when every ramp [`ModelParameter::color_for_value`] dispatched to
/// interpolated between its stops with `lerp_color`. The five reflectivity
/// fields broke that: a dBZ bar is read by which 5 dBZ band a pixel is in, their
/// ramp steps rather than interpolates, and a bar drawn as a wash would explain
/// a picture that is not on screen.
///
/// **This is what the legend draws, not what the raster paints through.** Seven
/// of the twenty-three state their stops in *display* units while the grid
/// carries raw GRIB2 values, and the ramps have three different postures outside
/// their stops that a `LegendScale` cannot express — so the raster resolves a
/// parameter's own ramp instead, through
/// [`crate::render::gridded::field_paint`], which records both reasons.
static SCALES: LazyLock<Vec<LegendScale>> = LazyLock::new(|| {
    ModelParameter::all()
        .iter()
        .map(|p| {
            let thresholds = p.legend_thresholds();
            let min_value = thresholds.first().map_or(0.0, |e| e.0);
            let max_value = thresholds.last().map_or(1.0, |e| e.0);
            LegendScale {
                thresholds,
                is_gradient: !p.is_banded(),
                min_value,
                max_value,
            }
        })
        .collect()
});

/// Every model parameter's registration, projected once.
static FIELDS: LazyLock<Vec<ProductSpec>> = LazyLock::new(|| {
    ModelParameter::all()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let scale = &SCALES[i];
            ProductSpec {
                // The parameter's persisted spelling: `serialize_state` writes
                // `as_str()` and `deserialize_state` reads it, so this is the
                // key already sitting in every user's config file.
                id: FieldId::from_static(p.as_str()),
                name: p.display_name(),
                code: p.as_str(),
                // `all()` is the bound, not a remembered count: this was
                // spelled "sixteen parameters fit in a u8" and stopped being
                // true the moment the seventh storm-scale field landed.
                sort_order: u8::try_from(i)
                    .expect("ModelParameter::all() is far shorter than u8::MAX"),
                group: GROUP,
                // `Unitless`: the parameter converts to its own display unit in
                // `convert_for_display` and labels itself, so the binding must
                // pass the value through rather than convert it twice.
                quantity: Quantity::Unitless {
                    label: p.unit_label(),
                },
                scale,
                value_domain: (scale.min_value, scale.max_value),
                // No model parameter has vertical extent, so none reaches an
                // isosurface slider. These ends describe the field's own domain
                // — the sequential reading and the unit the scale is printed in
                // — rather than a widget that exists.
                domain_label_ends: ("\u{2265}", p.unit_label()),
                vertical: false,
                tilted: false,
            }
        })
        .collect()
});

/// Every model parameter, in [`ModelParameter::all`]'s order.
pub fn products() -> &'static [ProductSpec] {
    &FIELDS
}

/// The registration for `param`.
pub fn spec(param: ModelParameter) -> &'static ProductSpec {
    let i = ModelParameter::all()
        .iter()
        .position(|&p| p == param)
        .expect("every parameter is in `all()`");
    &FIELDS[i]
}

#[cfg(test)]
mod tests;
