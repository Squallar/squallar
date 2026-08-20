//! Abstract drawing primitives for per-frame overlay rendering.
//!
//! All coordinates are **pixel offsets from the point's screen-space centre**,
//! never absolute. The UI crate owns lat/lon → screen projection; handlers
//! specify layout offsets only.

use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::handler::OverlayItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAnchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    CenterLeft,
    CenterRight,
    Center,
    CenterTop,
    CenterBottom,
}

/// The UI crate projects and culls these, then calls `draw_point()`.
pub struct MapPoint {
    pub lat: f64,
    pub lon: f64,
    /// Index into the handler's data array; comes back in `draw_point()`.
    pub id: u32,
    pub selection: Arc<dyn OverlayItem>,
}

/// `offset` is `[x, y]` pixels from the point centre; colours are `[r, g, b, a]`.
pub trait PointPainter {
    fn circle_filled(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4]);

    fn circle_stroke(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4], width: f32);

    fn text(&mut self, offset: [f32; 2], text: &str, color: [u8; 4], size: f32, anchor: TextAnchor);

    fn line(&mut self, from: [f32; 2], to: [f32; 2], color: [u8; 4], width: f32);

    fn filled_polygon(&mut self, points: &[[f32; 2]], color: [u8; 4]);
}

pub struct DrawPointContext {
    pub zoom: f32,
    pub is_dark: bool,
}

pub struct HoverContext<'a> {
    pub prefs: &'a UserPreferences,
}
