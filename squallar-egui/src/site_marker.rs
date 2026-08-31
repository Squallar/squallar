//! **A radar site's marker is a screen-space affordance, not a piece of the
//! landscape**, and this module is where its size is decided.
//!
//! The distinction is not stylistic. The marker's own click target is already
//! screen-space — `visible_radar_sites` sizes `icon_rect` from the live map
//! zoom in points, with no reference to any texture — so a marker drawn any
//! other way is drawn somewhere its own hit box is not.

#[cfg(test)]
mod tests;

/// The marker's geometry at one map zoom, in **points**: what the map puts on
/// glass, independent of anything cached.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MarkerShape {
    /// Radius of the filled disc.
    pub radius: f32,
    /// Width of the white ring around it.
    pub stroke: f32,
}

/// How much a marker may grow per zoom level, in points.
///
/// The size ramp below is deliberate: a continental view holds every station in
/// the network, and 200 discs at the close-in size is a blob rather than a map.
/// It is a *gentle* ramp — one point per zoom level, and flat past zoom 7 —
/// which is the whole difference between a marker that grows with the map and
/// one that is dragged by it. A texture stretched through a two-level zoom
/// gesture multiplies by four; this ramp adds two points.
pub(crate) const MAX_GROWTH_PER_ZOOM: f32 = 1.0;

/// The marker at a given map zoom. The clamp ends the ramp at zoom 7, where a
/// station's own neighbours are already further apart than the disc is wide.
pub(crate) fn marker_shape(zoom: f64) -> MarkerShape {
    let radius = ((5.0 + zoom as f32 * MAX_GROWTH_PER_ZOOM).clamp(4.0, 12.0)).max(1.0);
    MarkerShape {
        radius,
        stroke: (radius * 0.3).clamp(0.5, 2.0),
    }
}

/// One turn of longitude, in points, as this projector draws it.
///
/// Measured off the projector — the x it puts 180° from the prime meridian,
/// doubled — rather than computed from the zoom and a tile size, so it cannot
/// drift from what the projector actually does.
pub(crate) fn world_width_in_points(projector: &walkers::Projector) -> f32 {
    let x0 = projector.project(walkers::lat_lon(0.0, 0.0)).to_pos2().x;
    let x180 = projector.project(walkers::lat_lon(0.0, 180.0)).to_pos2().x;
    ((x180 - x0) * 2.0).abs()
}

/// Bring a projected x into the turn centred on `centre_x`.
///
/// A datum more than half a turn away in the written coordinates names the same
/// ground as one just off the opposite edge, and this picks the one the pane is
/// actually looking at. A degenerate width — a projector with no scale yet, an
/// overflow — is left alone rather than guessed at: folding by nonsense moves a
/// station that was already placed correctly.
pub(crate) fn fold_into_turn(x: f32, centre_x: f32, world_width: f32) -> f32 {
    if !world_width.is_finite() || world_width <= 1.0 {
        return x;
    }
    let half = world_width / 2.0;
    centre_x + (x - centre_x + half).rem_euclid(world_width) - half
}

/// What the map is saying about this station: an ordinary site, the one this
/// pane is showing, or the one it is switching to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkerRole {
    Ordinary,
    Current,
    Loading,
}

impl MarkerRole {
    /// Blue, red, purple. Three fills for three things, and the map has no
    /// other way to say which station it is on.
    pub(crate) fn fill(self) -> egui::Color32 {
        match self {
            Self::Ordinary => egui::Color32::from_rgb(100, 150, 255),
            Self::Current => egui::Color32::from_rgb(255, 100, 100),
            Self::Loading => egui::Color32::from_rgb(160, 32, 240),
        }
    }
}

/// Draw one station's marker at a screen position.
///
/// **Every length here is a point on the display**, so the marker is whatever
/// size the live map zoom says and nothing between the map and the glass can
/// stretch it. That is the whole difference from the raster this replaced: a
/// texture is placed by its geographic corners and therefore scales with the
/// gesture, which put the marker four times its size two zoom levels into a
/// pinch and snapped it back when the zoom went still.
pub(crate) fn draw_site_marker(
    painter: &egui::Painter,
    center: egui::Pos2,
    zoom: f64,
    role: MarkerRole,
) {
    let shape = marker_shape(zoom);
    painter.circle_filled(center, shape.radius, role.fill());
    painter.circle_stroke(
        center,
        shape.radius,
        egui::Stroke::new(shape.stroke, egui::Color32::WHITE),
    );
}

/// The plate a station's name is written on, so the name stays readable over
/// whatever the basemap put under it.
///
/// Drawn from the laid-out text rather than from a character count: the name
/// is drawn by egui at a point size, so only egui knows how wide it came out.
pub(crate) fn draw_site_label(
    painter: &egui::Painter,
    anchor: egui::Pos2,
    name: &str,
    font: egui::FontId,
    text_color: egui::Color32,
    is_dark: bool,
) {
    let plate = if is_dark {
        egui::Color32::from_black_alpha(140)
    } else {
        egui::Color32::from_white_alpha(140)
    };
    let galley = painter.layout_no_wrap(name.to_owned(), font, text_color);
    let rect = egui::Align2::CENTER_TOP.anchor_size(anchor, galley.size());
    painter.rect_filled(rect.expand2(egui::vec2(2.0, 1.0)), 2.0, plate);
    painter.galley(rect.min, galley, text_color);
}
