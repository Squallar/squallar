//! The one place this crate's loop suites spell a render identity.

use rustdar_egui::pane::RenderTarget;
use rustdar_radar::types::RadarProduct;

/// The render identity of a loop frame for `site`, `product` and `elevation`.
pub(crate) fn key(site: impl Into<String>, product: RadarProduct, elevation: f32) -> RenderTarget {
    RenderTarget::new(site, product, elevation)
}
