//! The one place this crate's loop suites spell a render identity.

use squallar_egui::pane::RenderTarget;
use squallar_source::product::FieldId;

/// The render identity of a loop frame for `site`, `product` and `elevation`.
///
/// The field is named by its id: since WO-E9e a render target holds a `FieldId`
/// rather than the radar layer's own enum.
pub(crate) fn key(site: impl Into<String>, product: &FieldId, elevation: f32) -> RenderTarget {
    RenderTarget::new(site, product, elevation)
}
