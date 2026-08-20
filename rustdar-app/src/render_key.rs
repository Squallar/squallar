//! The identity of one render: *which picture*, as a value a cache can hash and compare
//! exactly.

use rustdar_radar::types::{RadarProduct, RenderView};
use rustdar_source::id::{LayerId, known};

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
    pub product: RadarProduct,
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
    product: RadarProduct,
    view: RenderView,
    elevation: f32,
) -> RenderKey {
    RenderKey {
        kind: known::RADAR,
        select: SelectKey {
            site: site.to_string(),
            product,
            elevation_tenths: view
                .elevation_selects_picture(product)
                .then(|| elevation_key(elevation)),
            // Absent, and no theme reading is threaded in to make it so.
            theme: SelectKey::theme_part(RADAR_THEME_SENSITIVE, false),
        },
        view,
        view_key: RESERVED_VIEW_KEY,
    }
}

#[cfg(test)]
mod tests;
