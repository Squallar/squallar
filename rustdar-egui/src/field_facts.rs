//! The read side of a field id: what the UI needs to know about the field a
//! pane has selected, resolved from the id the pane stores.
//!
//! Since WO-E9e a pane's selection is a [`FieldId`] — an open string — rather
//! than a source's own enum, so every display read goes through here instead of
//! through a method on a type the UI is not supposed to name.

use rustdar_radar::fields as radar_fields;
use rustdar_source::product::{FieldId, ProductSpec};
use rustdar_units::UserPreferences;

/// The registry row `id` names.
///
/// **Total, and that is a claim about the callers rather than about ids.** A
/// pane's selection is always a field this build registers: the constructor
/// starts at `known::REFLECTIVITY`, the load path falls back to it for an id
/// this build does not know (`ui_config::field_or_default`), and every writer
/// is fed from the catalogue, which lists only registered fields. An id that
/// resolves to nothing means one of those three was bypassed, so this says so
/// in the log and hands back the same row the load path would have — a pane
/// that draws the default field is recoverable; one that draws nothing is not.
pub(crate) fn facts(id: &FieldId) -> &'static ProductSpec {
    match radar_fields::spec_for(id) {
        Some(spec) => spec,
        None => {
            log::warn!(
                "a pane holds the field id {} that this build does not register; \
                 reading the default field's facts instead",
                id.as_str()
            );
            radar_fields::spec_for(&radar_fields::known::REFLECTIVITY)
                .expect("the default field is registered by this crate")
        }
    }
}

/// The display name of the field `id` names.
pub(crate) fn name(id: &FieldId) -> &'static str {
    facts(id).name
}

/// The short lowercase code of the field `id` names (`"ref"`, `"vel"`, …).
pub(crate) fn code(id: &FieldId) -> &'static str {
    facts(id).code
}

/// The unit string a colour bar for `id` is titled with, in the reader's own
/// units.
///
/// **Byte-identical to the `unit_label` it replaces**, which was
/// `match self.quantity() { Unitless { label } => label, q => q.suffix(prefs) }`
/// — and `Quantity::suffix`'s own `Unitless` arm returns that same label, so
/// the two spellings collapse to one.
pub(crate) fn unit_label(id: &FieldId, prefs: &UserPreferences) -> &'static str {
    facts(id).quantity.suffix(prefs)
}

/// The readout line the pane prints for one sampled value of `id`.
///
/// Delegates to [`radar_fields::format_value`] — the per-field prefixes, the
/// per-field precision and the hydrometeor class names are the radar crate's
/// vocabulary, not something a [`ProductSpec`] states, so the string is
/// unchanged rather than rebuilt here.
pub(crate) fn format_value(id: &FieldId, value: f32, prefs: &UserPreferences) -> String {
    radar_fields::format_value(id, value, prefs).unwrap_or_else(|| {
        log::warn!(
            "no readout for the field id {}, which this build does not register",
            id.as_str()
        );
        String::new()
    })
}

/// Whether `view` makes the sweep angle part of *which picture* `id` names.
///
/// [`rustdar_radar::types::RenderView::elevation_selects_picture`]'s answer,
/// asked by id rather than by the radar crate's own enum.
pub(crate) fn elevation_selects_picture(
    view: rustdar_radar::types::RenderView,
    id: &FieldId,
) -> bool {
    match radar_fields::product_for(id) {
        Some(product) => view.elevation_selects_picture(product),
        // An id this build does not register has no plan-view rule to read.
        // Keeping the tilt in the key is the answer that cannot collapse two
        // different pictures onto one identity, which is the failure that
        // would show as the wrong sweep on the glass.
        None => true,
    }
}
