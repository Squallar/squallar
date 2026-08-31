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
    /// Which of the three `name` is, for a pane showing `current` and switching
    /// to `loading`.
    ///
    /// **One rule, two painters.** The dots and the selected station's ring both
    /// ask it, and two spellings is how a marker and the ring belonging to it
    /// end up different colours. Loading outranks current: a pane mid-switch is
    /// on neither station yet, and the one it is going to is the one the user
    /// just asked about.
    pub(crate) fn for_station(name: &str, current: &str, loading: Option<&str>) -> Self {
        if loading == Some(name) {
            Self::Loading
        } else if current == name {
            Self::Current
        } else {
            Self::Ordinary
        }
    }

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

/// Where a station's 230 km coverage ring lands on the glass: centre in points,
/// radius in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RingPlacement {
    pub center: egui::Pos2,
    pub radius: f32,
}

/// Below this the ring is smaller than the marker drawn over it and reads as a
/// smudge round the dot rather than as coverage. In points, so it means the
/// same thing on every display density.
pub(crate) const MIN_RING_RADIUS_POINTS: f32 = 3.0;

/// The ring for one station, or `None` if it cannot be drawn legibly here.
///
/// **The radius is measured off the projector, not computed from the zoom.**
/// A point one coverage radius due north of the station is projected and the
/// screen distance taken. Web Mercator is conformal, so a circle this small
/// comes back a circle rather than an ellipse, and latitude scaling is handled
/// for free — the same 230 km is more points at Nome than at Key West, which is
/// what the ground looks like on this projection. It is the same construction
/// the coverage raster uses, so the per-frame ring and the ground wash cannot
/// disagree about how big 230 km is.
///
/// **The fold is not optional.** `walkers::Projector` is linear in longitude and
/// folds nothing, so a station written -165.30 seen from a map centred at 170E
/// projects 335 degrees west of centre — PAEC landed at x = -947 on a 1920 pt
/// canvas — and its ring would be drawn off the world. `centre_x` and
/// `world_width` come from the pane exactly as they do for the markers.
pub(crate) fn ring_placement(
    projector: &walkers::Projector,
    lat: f64,
    lon: f64,
    centre_x: f32,
    world_width: f32,
) -> Option<RingPlacement> {
    let here = projector.project(walkers::lat_lon(lat, lon)).to_pos2();
    let north = projector
        .project(walkers::lat_lon(
            lat + squallar_overlays::render::rasterize::COVERAGE_RADIUS_DEG_LAT,
            lon,
        ))
        .to_pos2();
    let radius = (here.y - north.y).abs();
    if !radius.is_finite() || radius < MIN_RING_RADIUS_POINTS {
        return None;
    }
    Some(RingPlacement {
        center: egui::pos2(fold_into_turn(here.x, centre_x, world_width), here.y),
        radius,
    })
}

/// The ring's line width, in points. One ring is on screen, so it does not thin
/// with the zoom the way 160 overlapping rings had to.
const RING_STROKE_POINTS: f32 = 1.5;

/// Draw the selected station's coverage ring.
///
/// **Screen space, per frame, and that is the point of it.** This is selection
/// feedback about a marker that is itself painted per frame; put in the overlay
/// raster it arrived a whole-picture round trip after the dot it belongs to,
/// which splits one affordance across two latencies. It also spent an
/// 8 MB full-canvas RGBA upload on a single hairline circle.
pub(crate) fn draw_coverage_ring(
    painter: &egui::Painter,
    placement: RingPlacement,
    role: MarkerRole,
) {
    let ink = role.fill();
    // A wash faint enough to read as "inside coverage" over any basemap, and
    // never as a fill the user has to see through.
    painter.circle_filled(
        placement.center,
        placement.radius,
        egui::Color32::from_rgba_unmultiplied(ink.r(), ink.g(), ink.b(), 14),
    );
    painter.circle_stroke(
        placement.center,
        placement.radius,
        egui::Stroke::new(RING_STROKE_POINTS, ink),
    );
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

/// How badly a station wants the screen its name needs.
///
/// **A rank, not a distance.** Anything measured off the viewport — range to
/// the centre, order of projection — changes as the map moves, so the same
/// ground would keep a name at one scroll position and drop it at the next, and
/// a label that blinks while you pan is worse than one that collides. Every
/// value here is a property of the station or of the pane's own selection, and
/// neither moves when the map does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LabelRank {
    /// The station the ring is drawn for. It goes first and therefore always
    /// places: nothing has claimed any screen yet when it asks.
    Selected,
    /// The station this pane's data is from, when it is not the selected one.
    Current,
    /// A WSR-88D. The 160-station NEXRAD network is what the map is *of*.
    Primary,
    /// A terminal-doppler or other secondary installation. `TDTW` sits inside
    /// Detroit's `KDTX`; when only one of the two names fits, the network
    /// radar is the one a reader is orienting by.
    Secondary,
}

/// The order in which labels compete for screen.
///
/// **A total order with no ties**, which is what makes the result reproducible:
/// `sort_by_key` over `(rank, index)` cannot be perturbed by hash iteration, by
/// which station happened to be projected first, or by anything else that is
/// not the rank and the station's fixed row in the table. The same viewport
/// therefore drops the same names every frame, and re-entering a viewport
/// restores exactly the labels that were there before.
pub(crate) fn label_order(ranks: &[LabelRank]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..ranks.len()).collect();
    order.sort_by_key(|&i| (ranks[i], i));
    order
}

/// Air between two plates, in points. The separating-axis test counts touching
/// rectangles as overlapping, so this is the visible gap and not the difference
/// between touching and not.
const LABEL_GUTTER_POINTS: f32 = 1.0;

/// The screen a station's name would take at `anchor`, plate and gutter
/// included.
///
/// Measured from the laid-out galley rather than from a character count: the
/// name is set by egui at a point size, so only egui knows how wide it came
/// out.
pub(crate) fn label_area(
    painter: &egui::Painter,
    anchor: egui::Pos2,
    name: &str,
    font: egui::FontId,
) -> egui::Rect {
    let galley = painter.layout_no_wrap(name.to_owned(), font, egui::Color32::WHITE);
    egui::Align2::CENTER_TOP
        .anchor_size(anchor, galley.size())
        .expand2(egui::vec2(2.0, 1.0))
        .expand(LABEL_GUTTER_POINTS)
}

/// Claim the screen a name needs, and draw it there if it was free.
///
/// **First to ask keeps it**, which is [`walkers::OccupiedAreas`]' rule and the
/// one the city labels already run under — the same machinery, so the two label
/// systems cannot drift into two different ideas of "overlapping". The order
/// the asking happens in is [`label_order`]'s, and that is where the stability
/// lives: this function has no opinion about who deserves the spot, only about
/// whether it is taken.
///
/// Returns whether the label drew.
pub(crate) fn try_draw_site_label(
    painter: &egui::Painter,
    occupied: &mut walkers::OccupiedAreas,
    anchor: egui::Pos2,
    name: &str,
    font: egui::FontId,
    text_color: egui::Color32,
    is_dark: bool,
) -> bool {
    let area = label_area(painter, anchor, name, font.clone());
    let claim = walkers::text::OrientedRect::new(area.center(), 0.0, area.size());
    if !occupied.try_occupy(claim) {
        return false;
    }

    let plate = if is_dark {
        egui::Color32::from_black_alpha(140)
    } else {
        egui::Color32::from_white_alpha(140)
    };
    let galley = painter.layout_no_wrap(name.to_owned(), font, text_color);
    let rect = egui::Align2::CENTER_TOP.anchor_size(anchor, galley.size());
    painter.rect_filled(rect.expand2(egui::vec2(2.0, 1.0)), 2.0, plate);
    painter.galley(rect.min, galley, text_color);
    true
}
