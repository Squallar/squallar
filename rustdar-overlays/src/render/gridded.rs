//! The gridded raster's colour registry: which fields
//! [`rasterize_gridded`](super::rasterize::rasterize_gridded) can paint, and how.
//!
//! The raster itself knows only a [`FieldId`]. This module is the one place
//! that turns that identity into a colour, so a new gridded source registers a
//! row here rather than adding an arm to the rasterizer, the codec or the wire.
//!
//! **A code this build does not register is refused, never defaulted.** That is
//! the same posture the model codec has always had — it decoded a parameter
//! only if the code named itself back — carried across to field identity: an
//! unresolved id means a newer build's field, and painting it through some
//! other field's scale would be a silent misread.

use std::sync::LazyLock;

use rustdar_source::product::{FieldId, LegendScale};

/// The alpha every gridded overlay paints at. HRRR's eleven ramps all use it,
/// and a raster drawn under the radar layer wants to be seen through.
const ALPHA: u8 = 160;

/// How one gridded field is painted.
///
/// The colour and the visibility test are stored side by side because the two
/// are asked at very different rates: the colour once per drawn cell, the
/// visibility once per *grid point* on the fetch path — see
/// [`crate::hrrr::summarize_values`]. A field whose ramp is a plain walk over
/// its own [`LegendScale`] gets both from [`FieldPaint::over_scale`]; one whose
/// ramp is not — every HRRR parameter, for the two reasons in
/// [`register_model_fields`] — supplies its own pair.
pub struct FieldPaint {
    /// The field this paints, borrowed from the registering source's own
    /// `ProductSpec`, so a decoder can hand back the registry's spelling rather
    /// than one it parsed.
    pub id: &'static FieldId,
    /// The colour bar consumers read. **Not necessarily the ramp**: see
    /// [`register_model_fields`].
    pub scale: &'static LegendScale,
    color: Box<dyn Fn(f32) -> [u8; 4] + Send + Sync>,
    visible: Box<dyn Fn(f32) -> bool + Send + Sync>,
}

impl FieldPaint {
    /// The default: paint through the field's own scale with [`color_for`], and
    /// call a value visible exactly when that scale's first stop admits it.
    pub fn over_scale(id: &'static FieldId, scale: &'static LegendScale) -> Self {
        FieldPaint {
            id,
            scale,
            color: Box::new(move |v| color_for(scale, v)),
            visible: Box::new(move |v| paints_over_scale(scale, v)),
        }
    }

    pub fn color_for_value(&self, value: f32) -> [u8; 4] {
        (self.color)(value)
    }

    /// Whether `value` paints anything, answered without building a colour.
    pub fn paints(&self, value: f32) -> bool {
        (self.visible)(value)
    }
}

impl std::fmt::Debug for FieldPaint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldPaint")
            .field("id", &self.id)
            .field("stops", &self.scale.thresholds.len())
            .finish_non_exhaustive()
    }
}

/// The generic ramp over a colour bar: transparent below the first stop,
/// interpolated between stops when `is_gradient` and flat-banded when not, and
/// clamped to the last stop's colour above it.
///
/// The NaN guard is load-bearing for the same reason the model's own ramps have
/// one: NaN fails every comparison, so an unguarded missing point would fall
/// through to the top of the scale — see `rasterize/model_nan_tests.rs`.
///
/// `value` must be in the same units the scale's stops are stated in. That is
/// not a free property: the model's scales are stated in *display* units for
/// six of its sixteen parameters while its grids carry raw GRIB2 values, which
/// is one of the two reasons those fields do not use this function.
pub fn color_for(scale: &LegendScale, value: f32) -> [u8; 4] {
    if !value.is_finite() {
        return [0, 0, 0, 0];
    }
    let stops = &scale.thresholds;
    let (Some(&(first_value, _)), Some(&(last_value, last_color))) = (stops.first(), stops.last())
    else {
        return [0, 0, 0, 0];
    };
    if value < first_value {
        return [0, 0, 0, 0];
    }
    if value >= last_value {
        return [last_color[0], last_color[1], last_color[2], ALPHA];
    }
    // `stops` is ascending (`hrrr::fields::tests` and the radar palettes both
    // pin that), so the bracket is a partition point. `k + 1` is in range
    // because `value < last_value` was answered above.
    let k = stops.partition_point(|&(v, _)| v <= value) - 1;
    let (lo_value, lo_color) = stops[k];
    let (hi_value, hi_color) = stops[k + 1];
    if !scale.is_gradient {
        return [lo_color[0], lo_color[1], lo_color[2], ALPHA];
    }
    let t = if hi_value > lo_value {
        (value - lo_value) / (hi_value - lo_value)
    } else {
        0.0
    };
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    [
        mix(lo_color[0], hi_color[0]),
        mix(lo_color[1], hi_color[1]),
        mix(lo_color[2], hi_color[2]),
        ALPHA,
    ]
}

/// [`color_for`]'s visibility test, without building the colour: everything from
/// the first stop up.
pub fn paints_over_scale(scale: &LegendScale, value: f32) -> bool {
    value.is_finite() && scale.thresholds.first().is_some_and(|&(v, _)| value >= v)
}

/// Every gridded field this build can paint, in registration order.
static PAINTS: LazyLock<Vec<FieldPaint>> = LazyLock::new(register_model_fields);

/// The model's sixteen, each keeping its **own** ramp rather than taking
/// [`color_for`] over its registered scale.
///
/// Two properties of those scales make the generic ramp a different picture,
/// and both are the scale's business rather than the ramp's:
///
/// * six parameters state their stops in **display** units (kt, °F, in, mi)
///   while the grid carries raw GRIB2 values, so the generic ramp would compare
///   metres against miles;
/// * the ramps have three different postures outside their stops — CIN, lifted
///   index and visibility are transparent *above* their last stop, temperature
///   is transparent nowhere, and the rest are transparent below their first —
///   and a `LegendScale` states no posture at all.
///
/// Neither is a defect to repair here: the scale is what the *legend* draws, in
/// the units the legend prints. A gridded source whose scale is in its values'
/// own units and whose ramp fades out below its first stop registers with
/// [`FieldPaint::over_scale`] and needs none of this.
fn register_model_fields() -> Vec<FieldPaint> {
    crate::hrrr::ModelParameter::all()
        .iter()
        .map(|&p| {
            let spec = crate::hrrr::fields::spec(p);
            FieldPaint {
                id: &spec.id,
                scale: spec.scale,
                color: Box::new(move |v| p.color_for_value(v)),
                visible: Box::new(move |v| p.paints(v)),
            }
        })
        .collect()
}

/// How `id` is painted, or `None` for a field this build does not register.
pub fn field_paint(id: &FieldId) -> Option<&'static FieldPaint> {
    paint_for_code(id.as_str())
}

/// [`field_paint`] from the bare spelling — the form a decoder has in hand
/// before it is willing to build a `FieldId` it might not honour.
pub fn paint_for_code(code: &str) -> Option<&'static FieldPaint> {
    PAINTS.iter().find(|paint| paint.id.as_str() == code)
}

/// The colour bar `id` is drawn through, or `None` for a field this build does
/// not register.
pub fn field_scale(id: &FieldId) -> Option<&'static LegendScale> {
    field_paint(id).map(|paint| paint.scale)
}

#[cfg(test)]
mod tests;
