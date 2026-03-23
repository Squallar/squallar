//! Abstract drawing primitives for per-frame overlay rendering.
//!
//! The [`PointPainter`] trait defines GUI-framework-agnostic drawing operations
//! that overlay handlers use to render point-based overlays (e.g. METAR station
//! models). The egui crate provides the concrete implementation that translates
//! these calls into `egui::Painter` operations.
//!
//! All coordinates are **pixel offsets relative to the point's center position**
//! on screen. The UI crate handles lat/lon → screen projection; the overlay
//! handler only specifies layout offsets.

use rustdar_units::UserPreferences;

use super::overlay_state::SelectedOverlay;

/// Text anchor position relative to the draw point.
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

/// A geographic point that should be rendered per-frame by the UI crate.
///
/// Returned by [`super::overlay_state::OverlayHandler::per_frame_points()`].
/// The UI crate projects each point to screen coordinates, culls off-screen
/// points, then calls `draw_point()` for each visible point.
pub struct MapPoint {
    pub lat: f64,
    pub lon: f64,
    /// Index into the handler's data array (passed back to `draw_point()`
    /// and `hover_text()`).
    pub id: u32,
    /// What to store in the selection list when the user clicks this point.
    pub selection: SelectedOverlay,
}

/// Abstract drawing surface for point-based overlays.
///
/// All `offset` parameters are `[x, y]` pixel offsets from the point's center.
/// Colors are `[r, g, b, a]`. The UI crate translates these to its native
/// painter calls after applying the screen-space center position.
pub trait PointPainter {
    /// Draw a filled circle.
    fn circle_filled(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4]);

    /// Draw a circle outline (stroke only).
    fn circle_stroke(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4], width: f32);

    /// Draw a text string.
    fn text(&mut self, offset: [f32; 2], text: &str, color: [u8; 4], size: f32, anchor: TextAnchor);

    /// Draw a line segment.
    fn line(&mut self, from: [f32; 2], to: [f32; 2], color: [u8; 4], width: f32);

    /// Draw a filled convex polygon.
    fn filled_polygon(&mut self, points: &[[f32; 2]], color: [u8; 4]);
}

/// Context passed to [`super::overlay_state::OverlayHandler::draw_point()`].
pub struct DrawPointContext {
    /// Current map zoom level.
    pub zoom: f32,
    /// Whether the app is in dark theme.
    pub is_dark: bool,
}

/// Context passed to [`super::overlay_state::OverlayHandler::hover_text()`].
pub struct HoverContext<'a> {
    /// User unit and timezone preferences.
    pub prefs: &'a UserPreferences,
}
