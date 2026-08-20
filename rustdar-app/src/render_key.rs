//! The identity of one render: *which picture*, as a value a cache can hash and compare
//! exactly.

use rustdar_radar::types::RenderView;
use rustdar_source::id::{LayerId, known};
use rustdar_source::product::FieldId;

/// Quantize an elevation angle to tenths of a degree for cache key use.
///
/// **Delegates rather than rounds.** The quantum is one value in one place —
/// `rustdar_egui::pane::elevation_tenths` — because the acceptance check that
/// asks "is this picture already in hand?" compares the same two angles this
/// key separates them by, and a second spelling here is how those two answers
/// would drift apart.
pub(crate) fn elevation_key(elevation: f32) -> i32 {
    rustdar_egui::pane::elevation_tenths(elevation)
}

/// Whether the radar layer's cached raster would come out different in the other UI theme —
/// [`OverlayHandler::theme_sensitive`]'s answer for [`known::RADAR`].
const RADAR_THEME_SENSITIVE: bool = false;

/// The `view_key` every key carries today — see [`RenderKey::view_key`].
const RESERVED_VIEW_KEY: u32 = 0;

/// The parts of a pane's selection that pick *which* picture, quantized.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SelectKey {
    pub site: String,
    /// The field these pixels depict, by its open-string id.
    ///
    /// **A `FieldId` rather than the radar layer's own enum since WO-E9e**
    /// (amendment M8): a render's identity is a UI-side fact about a picture,
    /// and the pane whose selection it mirrors holds an id. `render_cache_key`
    /// is the one place a key is built, and it is where the projection happens.
    pub product: FieldId,
    pub elevation_tenths: Option<i32>,
    /// The UI theme this picture was baked under, **present iff the owning layer declares
    /// that its raster branches on the theme** — the rule is [`SelectKey::theme_part`], and
    /// the declaration is the layer's own [`OverlayHandler::theme_sensitive`].
    pub theme: Option<bool>,
}

impl SelectKey {
    /// The theme part of a key, computed from the owning layer's **own** declaration.
    pub fn theme_part(theme_sensitive: bool, is_dark: bool) -> Option<bool> {
        theme_sensitive.then_some(is_dark)
    }
}

/// The identity of one render, and the key its output is cached under.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RenderKey {
    pub kind: LayerId,
    pub select: SelectKey,
    pub view: RenderView,
    pub view_key: u32,
}

/// The cache key for one radar render, and the only place one is built.
pub(crate) fn render_cache_key(
    site: &str,
    product: &FieldId,
    view: RenderView,
    elevation: f32,
) -> RenderKey {
    // Whether the tilt is part of *which picture* this is stays the radar
    // layer's rule, asked of it by id. An id this build does not register keeps
    // its tilt in the key: that is the answer that cannot collapse two
    // different pictures onto one identity.
    let tilt_selects = rustdar_radar::fields::product_for(product)
        .is_none_or(|p| view.elevation_selects_picture(p));
    RenderKey {
        kind: known::RADAR,
        select: SelectKey {
            site: site.to_string(),
            product: product.clone(),
            elevation_tenths: tilt_selects.then(|| elevation_key(elevation)),
            // Absent, and no theme reading is threaded in to make it so.
            theme: SelectKey::theme_part(RADAR_THEME_SENSITIVE, false),
        },
        view,
        view_key: RESERVED_VIEW_KEY,
    }
}

#[cfg(test)]
mod tests;

/// The radar layer's own field value an id names.
///
/// The app builds render inputs out of radar's own machinery, which is keyed by
/// radar's field; the pane and the render key hold ids. This is the one place
/// that crossing is written down.
pub(crate) fn radar_field(id: &FieldId) -> Option<rustdar_radar::types::RadarProduct> {
    rustdar_radar::fields::product_for(id)
}

/// The id the radar layer registers `product` under — the C1 projection.
pub(crate) fn field_id_of(product: rustdar_radar::types::RadarProduct) -> FieldId {
    rustdar_radar::fields::spec(product).id.clone()
}
