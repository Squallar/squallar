//! NWS-style station model plot rendering via [`PointPainter`].
//!
//! Draws METAR surface observations as standard meteorological station model
//! plots with three progressive detail tiers based on map zoom level:
//!
//! - **Tier 1** (zoom < 6): flight-category-coloured filled circle.
//! - **Tier 2** (zoom 6–9): circle + temperature (upper-left) + dewpoint
//!   (lower-left) + wind barb.
//! - **Tier 3** (zoom ≥ 10): full station model — all of tier 2 plus altimeter
//!   (upper-right), visibility (left), present weather symbol, and station ID
//!   (lower-right).

use crate::metar::types::{CloudLayer, FlightCategory, MetarOb, WindDir};
use crate::render::draw::{DrawPointContext, PointPainter, TextAnchor};

// ── Zoom tier thresholds ──────────────────────────────────────────────────

const TIER2_ZOOM: f32 = 6.0;
const TIER3_ZOOM: f32 = 10.0;

// ── Layout constants (pixel offsets from station center) ──────────────────

const TEMP_OFFSET: [f32; 2] = [-14.0, -14.0];
const DEWP_OFFSET: [f32; 2] = [-14.0, 10.0];
const ALTIMETER_OFFSET: [f32; 2] = [14.0, -14.0];
const VIS_OFFSET: [f32; 2] = [-34.0, -2.0];
const WX_OFFSET: [f32; 2] = [-20.0, -2.0];
const ID_OFFSET: [f32; 2] = [14.0, 10.0];

// ── Wind barb geometry ────────────────────────────────────────────────────

const BARB_STAFF_LENGTH: f32 = 28.0;
const BARB_LINE_LENGTH: f32 = 10.0;
const HALF_BARB_LENGTH: f32 = 5.0;
const BARB_SPACING: f32 = 5.0;
const PENNANT_WIDTH: f32 = 4.0;
const BARB_STROKE_WIDTH: f32 = 1.5;

// ── Public entry point ────────────────────────────────────────────────────

/// Draw a complete station model plot for a single METAR observation.
pub fn draw_metar_station(ob: &MetarOb, painter: &mut dyn PointPainter, ctx: &DrawPointContext) {
    let zoom = ctx.zoom;
    let fc_color = flight_category_color(ob.flight_category);
    let text_color = if ctx.is_dark { [255, 255, 255, 230] } else { [20, 20, 20, 230] };
    let font_size = font_size_for_zoom(zoom);
    let circle_r = circle_radius_for_zoom(zoom);

    // ── Tier 1: cloud cover / flight category circle ──────────────────
    draw_cloud_cover_circle(painter, ob, fc_color, circle_r, ctx.is_dark);

    if zoom < TIER2_ZOOM {
        return;
    }

    // ── Tier 2: temperature + dewpoint + wind barb ────────────────────
    if let Some(tc) = ob.temp_c {
        let tf = tc * 9.0 / 5.0 + 32.0;
        painter.text(TEMP_OFFSET, &format!("{tf:.0}"), text_color, font_size, TextAnchor::BottomRight);
    }

    if let Some(td) = ob.dewp_c {
        let tdf = td * 9.0 / 5.0 + 32.0;
        painter.text(DEWP_OFFSET, &format!("{tdf:.0}"), text_color, font_size, TextAnchor::TopRight);
    }

    draw_wind_barb(painter, ob.wind_dir, ob.wind_speed_kt, circle_r, text_color);

    if zoom < TIER3_ZOOM {
        return;
    }

    // ── Tier 3: altimeter, visibility, wx, station ID ─────────────────
    if let Some(alt) = ob.altimeter_hpa {
        let in_hg_tenths = ((alt * 0.02953 * 100.0).round() as i32) % 1000;
        painter.text(
            ALTIMETER_OFFSET,
            &format!("{in_hg_tenths:03}"),
            text_color,
            font_size * 0.85,
            TextAnchor::BottomLeft,
        );
    }

    if let Some(vis) = ob.visibility {
        painter.text(VIS_OFFSET, &vis.label(), text_color, font_size * 0.85, TextAnchor::CenterRight);
    }

    if let Some(ref wx) = ob.wx_string {
        draw_wx_symbol(painter, WX_OFFSET, wx, text_color);
    }

    painter.text(ID_OFFSET, &ob.station_id, text_color, font_size * 0.75, TextAnchor::TopLeft);
}

/// Clickable/hoverable radius for a station at the given zoom.
pub fn hit_radius_for_zoom(zoom: f32) -> f32 {
    if zoom < TIER2_ZOOM {
        circle_radius_for_zoom(zoom) + 2.0
    } else {
        30.0 // Larger hit area when full model is displayed
    }
}

/// Build a short hover tooltip string for a METAR observation.
pub fn hover_text_for_metar(ob: &MetarOb, prefs: &rustdar_units::UserPreferences) -> String {
    let mut parts = vec![ob.station_id.clone()];

    if let Some(tc) = ob.temp_c {
        let tf = tc * 9.0 / 5.0 + 32.0;
        if let Some(td) = ob.dewp_c {
            let tdf = td * 9.0 / 5.0 + 32.0;
            parts.push(format!("{tf:.0}°F/{tdf:.0}°F"));
        } else {
            parts.push(format!("{tf:.0}°F"));
        }
    }

    if let Some(vis) = ob.visibility {
        parts.push(format!("{}SM", vis.label()));
    }

    if let Some(speed) = ob.wind_speed_kt {
        let converted = prefs.speed.convert_from_knots(speed as f32);
        match ob.wind_dir {
            Some(WindDir::Calm) => parts.push("CALM".into()),
            Some(dir) => {
                parts.push(format!("{} {converted:.0}{}", dir.label(), prefs.speed.suffix()))
            }
            None => parts.push(format!("{converted:.0}{}", prefs.speed.suffix())),
        }
    }

    if let Some(fc) = ob.flight_category {
        parts.push(fc.label().to_string());
    }

    parts.join(" | ")
}

// ── Internal helpers ──────────────────────────────────────────────────────

fn circle_radius_for_zoom(zoom: f32) -> f32 {
    (3.0 + zoom * 0.5).clamp(3.0, 7.0)
}

fn font_size_for_zoom(zoom: f32) -> f32 {
    (zoom * 1.0 + 2.0).clamp(9.0, 14.0)
}

fn flight_category_color(fc: Option<FlightCategory>) -> [u8; 4] {
    fc.map(|f| f.color_rgba()).unwrap_or([150, 150, 150, 220])
}

// ── Cloud cover circle ────────────────────────────────────────────────────

fn draw_cloud_cover_circle(
    painter: &mut dyn PointPainter,
    ob: &MetarOb,
    fc_color: [u8; 4],
    radius: f32,
    is_dark: bool,
) {
    let fill_fraction = cloud_cover_fraction(&ob.clouds);
    let outline_color = if is_dark { [255, 255, 255, 180] } else { [40, 40, 40, 180] };

    if fill_fraction >= 0.99 {
        // OVC — fully filled
        painter.circle_filled([0.0, 0.0], radius, fc_color);
    } else if fill_fraction <= 0.01 {
        // CLR/SKC — empty, outline only
        // (draw nothing for fill)
    } else if fill_fraction >= 0.5 {
        // BKN — filled circle with a vertical open slice on the right
        painter.circle_filled([0.0, 0.0], radius, fc_color);
        // Approximate the open slice with a small unfilled region:
        // Draw the background color over the right side
        let bg = if is_dark { [30, 30, 30, 230] } else { [245, 245, 245, 230] };
        let open_fraction = 1.0 - fill_fraction;
        let half_w = radius * open_fraction * 2.0;
        // Cover the right portion with background-colored half
        let pts: [[f32; 2]; 4] = [
            [radius - half_w, -radius],
            [radius, -radius],
            [radius, radius],
            [radius - half_w, radius],
        ];
        painter.filled_polygon(&pts, bg);
    } else {
        // FEW/SCT — partially filled from the left
        // Fill portion from -radius to the fill boundary
        let fill_x = -radius + 2.0 * radius * fill_fraction;
        let pts: [[f32; 2]; 4] = [
            [-radius, -radius * 0.8],
            [fill_x, -radius * 0.8],
            [fill_x, radius * 0.8],
            [-radius, radius * 0.8],
        ];
        painter.filled_polygon(&pts, fc_color);
    }

    // Outline in flight category color
    painter.circle_stroke([0.0, 0.0], radius, fc_color, 1.5);
    // Thin outline for contrast
    painter.circle_stroke([0.0, 0.0], radius, outline_color, 0.5);
}

/// Cloud cover fraction from the highest cloud layer coverage.
fn cloud_cover_fraction(clouds: &[CloudLayer]) -> f32 {
    let mut max_fraction = 0.0_f32;
    for layer in clouds {
        let f = match layer.cover.as_str() {
            "CLR" | "SKC" | "CAVOK" | "NSC" => 0.0,
            "FEW" => 0.125,
            "SCT" => 0.375,
            "BKN" => 0.75,
            "OVC" | "VV" => 1.0,
            _ => 0.0,
        };
        max_fraction = max_fraction.max(f);
    }
    max_fraction
}

// ── Wind barb ─────────────────────────────────────────────────────────────

/// Draw a standard WMO wind barb extending from the station circle.
///
/// The barb staff extends in the direction the wind blows FROM.
/// - Pennant (filled triangle) = 50 kt
/// - Full barb (line) = 10 kt
/// - Half barb (short line) = 5 kt
/// - Calm (speed 0 or None) = larger circle outline, no staff
/// - Variable direction = that circle plus a second concentric ring
///
/// A staff is drawn only for [`WindDir::Degrees`]. Defaulting a missing or
/// variable direction to `0` would point the barb due north — which is what a
/// quarter of the AWC feed used to render, since AWC encodes both calm and
/// variable as `wind_dir_degrees=0`.
fn draw_wind_barb(
    painter: &mut dyn PointPainter,
    wind_dir: Option<WindDir>,
    wind_speed: Option<u16>,
    circle_radius: f32,
    color: [u8; 4],
) {
    let speed = wind_speed.unwrap_or(0);

    // No direction to point: calm, variable, or no wind data at all.
    let Some(dir_deg) = wind_dir.and_then(WindDir::bearing) else {
        painter.circle_stroke([0.0, 0.0], circle_radius + 2.0, color, 1.0);
        // A variable wind is a real wind, so mark it apart from dead calm
        // rather than letting a gusty VRB18KT look like nothing at all.
        if wind_dir == Some(WindDir::Variable) {
            painter.circle_stroke([0.0, 0.0], circle_radius + 4.5, color, 1.0);
        }
        return;
    };

    // Calm — no barb, just a slightly larger circle
    if speed < 3 {
        painter.circle_stroke([0.0, 0.0], circle_radius + 2.0, color, 1.0);
        return;
    }

    let dir_deg = dir_deg as f32;
    // Wind direction is where wind comes FROM — barb points that direction
    let dir_rad = (dir_deg - 90.0).to_radians(); // Rotate so 0° (north) points up

    let (sin_d, cos_d) = dir_rad.sin_cos();

    // Staff endpoint (from circle edge outward)
    let staff_start_x = cos_d * circle_radius;
    let staff_start_y = sin_d * circle_radius;
    let staff_end_x = cos_d * (circle_radius + BARB_STAFF_LENGTH);
    let staff_end_y = sin_d * (circle_radius + BARB_STAFF_LENGTH);

    // Draw staff
    painter.line(
        [staff_start_x, staff_start_y],
        [staff_end_x, staff_end_y],
        color,
        BARB_STROKE_WIDTH,
    );

    // Decompose speed into pennants (50kt), full barbs (10kt), half barbs (5kt)
    let mut remaining = speed;
    let pennants = remaining / 50;
    remaining %= 50;
    let full_barbs = remaining / 10;
    remaining %= 10;
    let half_barbs = if remaining >= 3 { 1 } else { 0 };

    // Perpendicular direction for barb lines (always to the left when facing the wind)
    let perp_x = -sin_d;
    let perp_y = cos_d;

    // Start drawing from the end of the staff, working inward
    let mut pos = 0.0_f32; // Distance from staff end back toward center

    // Pennants
    for _ in 0..pennants {
        let base_x = staff_end_x - cos_d * pos;
        let base_y = staff_end_y - sin_d * pos;
        let tip_x = base_x + perp_x * BARB_LINE_LENGTH;
        let tip_y = base_y + perp_y * BARB_LINE_LENGTH;
        let next_x = staff_end_x - cos_d * (pos + PENNANT_WIDTH);
        let next_y = staff_end_y - sin_d * (pos + PENNANT_WIDTH);

        painter.filled_polygon(
            &[[base_x, base_y], [tip_x, tip_y], [next_x, next_y]],
            color,
        );
        pos += PENNANT_WIDTH + 1.0;
    }

    // Full barbs
    for _ in 0..full_barbs {
        let base_x = staff_end_x - cos_d * pos;
        let base_y = staff_end_y - sin_d * pos;
        let tip_x = base_x + perp_x * BARB_LINE_LENGTH;
        let tip_y = base_y + perp_y * BARB_LINE_LENGTH;

        painter.line([base_x, base_y], [tip_x, tip_y], color, BARB_STROKE_WIDTH);
        pos += BARB_SPACING;
    }

    // Half barb
    if half_barbs > 0 {
        // If this is the only barb, offset it slightly from the end
        if pennants == 0 && full_barbs == 0 {
            pos += BARB_SPACING;
        }
        let base_x = staff_end_x - cos_d * pos;
        let base_y = staff_end_y - sin_d * pos;
        let tip_x = base_x + perp_x * HALF_BARB_LENGTH;
        let tip_y = base_y + perp_y * HALF_BARB_LENGTH;

        painter.line([base_x, base_y], [tip_x, tip_y], color, BARB_STROKE_WIDTH);
    }
}

// ── Present weather symbols (vector-drawn WMO standard) ───────────────────
//
// WMO present weather symbols are NOT Unicode characters — they are
// specialized meteorological glyphs that must be drawn as vector graphics.
// Each symbol is composed of lines, dots, circles, and filled polygons
// rendered via the `PointPainter` trait at a given offset from center.
//
// Symbol scale: drawn within a ~10×10 pixel bounding box centered on `off`.

/// Draw the WMO present weather symbol for a METAR wx_string.
///
/// Parses the METAR present weather group and draws the most significant
/// phenomenon as a standard WMO symbol using line/circle primitives.
fn draw_wx_symbol(painter: &mut dyn PointPainter, off: [f32; 2], wx: &str, color: [u8; 4]) {
    let wx_upper = wx.to_uppercase();

    // Dispatch to drawing function for most significant phenomenon
    if wx_upper.contains("TS") {
        draw_wx_thunderstorm(painter, off, color);
    } else if wx_upper.contains("FZRA") {
        draw_wx_freezing_rain(painter, off, color);
    } else if wx_upper.contains("FZDZ") {
        draw_wx_freezing_drizzle(painter, off, color);
    } else if wx_upper.contains("PL") || wx_upper.contains("IC") {
        draw_wx_ice_pellets(painter, off, color);
    } else if wx_upper.contains("+RA") {
        draw_wx_rain_heavy(painter, off, color);
    } else if wx_upper.contains("RA") {
        draw_wx_rain(painter, off, color);
    } else if wx_upper.contains("DZ") {
        draw_wx_drizzle(painter, off, color);
    } else if wx_upper.contains("+SN") {
        draw_wx_snow_heavy(painter, off, color);
    } else if wx_upper.contains("SN") || wx_upper.contains("SG") {
        draw_wx_snow(painter, off, color);
    } else if wx_upper.contains("GR") || wx_upper.contains("GS") {
        draw_wx_hail(painter, off, color);
    } else if wx_upper.contains("FG") {
        draw_wx_fog(painter, off, color);
    } else if wx_upper.contains("BR") {
        draw_wx_mist(painter, off, color);
    } else if wx_upper.contains("HZ") {
        draw_wx_haze(painter, off, color);
    } else if wx_upper.contains("FU") || wx_upper.contains("VA") {
        draw_wx_smoke(painter, off, color);
    } else if wx_upper.contains("SQ") {
        draw_wx_squall(painter, off, color);
    }
}

/// Rain (RA): a filled dot
fn draw_wx_rain(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    painter.circle_filled(off, 2.0, color);
}

/// Heavy rain (+RA): two filled dots vertically
fn draw_wx_rain_heavy(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    painter.circle_filled([off[0], off[1] - 3.0], 2.0, color);
    painter.circle_filled([off[0], off[1] + 3.0], 2.0, color);
}

/// Drizzle (DZ): a comma — short vertical line with a curve
fn draw_wx_drizzle(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    painter.line(
        [off[0], off[1] - 3.0],
        [off[0], off[1] + 2.0],
        color, 1.5,
    );
    painter.line(
        [off[0], off[1] + 2.0],
        [off[0] - 1.5, off[1] + 4.0],
        color, 1.5,
    );
}

/// Snow (SN/SG): a six-pointed asterisk (3 crossing lines)
fn draw_wx_snow(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    let r = 4.0_f32;
    // Vertical
    painter.line([off[0], off[1] - r], [off[0], off[1] + r], color, 1.2);
    // 60° diagonals
    let dx = r * 0.866; // cos(30°)
    let dy = r * 0.5;   // sin(30°)
    painter.line([off[0] - dx, off[1] - dy], [off[0] + dx, off[1] + dy], color, 1.2);
    painter.line([off[0] - dx, off[1] + dy], [off[0] + dx, off[1] - dy], color, 1.2);
}

/// Heavy snow (+SN): two asterisks stacked
fn draw_wx_snow_heavy(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    draw_wx_snow(painter, [off[0], off[1] - 4.5], color);
    draw_wx_snow(painter, [off[0], off[1] + 4.5], color);
}

/// Freezing rain (FZRA): rain dot with a small arc/line above
fn draw_wx_freezing_rain(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    // Rain dot
    painter.circle_filled([off[0], off[1] + 1.0], 2.0, color);
    // "S"-curve freezing indicator above
    painter.line(
        [off[0] - 3.0, off[1] - 4.0],
        [off[0], off[1] - 2.0],
        color, 1.2,
    );
    painter.line(
        [off[0], off[1] - 2.0],
        [off[0] + 3.0, off[1] - 4.0],
        color, 1.2,
    );
}

/// Freezing drizzle (FZDZ): drizzle comma with freezing indicator
fn draw_wx_freezing_drizzle(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    draw_wx_drizzle(painter, [off[0], off[1] + 1.5], color);
    // "S"-curve above
    painter.line(
        [off[0] - 3.0, off[1] - 4.0],
        [off[0], off[1] - 2.0],
        color, 1.2,
    );
    painter.line(
        [off[0], off[1] - 2.0],
        [off[0] + 3.0, off[1] - 4.0],
        color, 1.2,
    );
}

/// Ice pellets (PL/IC): a small triangle (pointing up)
fn draw_wx_ice_pellets(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    let pts: [[f32; 2]; 3] = [
        [off[0], off[1] - 4.0],
        [off[0] - 3.5, off[1] + 3.0],
        [off[0] + 3.5, off[1] + 3.0],
    ];
    painter.filled_polygon(&pts, color);
}

/// Hail (GR/GS): open triangle with a line underneath
fn draw_wx_hail(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    // Triangle outline via lines
    painter.line(
        [off[0], off[1] - 4.0],
        [off[0] - 3.5, off[1] + 2.0],
        color, 1.2,
    );
    painter.line(
        [off[0], off[1] - 4.0],
        [off[0] + 3.5, off[1] + 2.0],
        color, 1.2,
    );
    painter.line(
        [off[0] - 3.5, off[1] + 2.0],
        [off[0] + 3.5, off[1] + 2.0],
        color, 1.2,
    );
    // Horizontal line beneath
    painter.line(
        [off[0] - 3.5, off[1] + 4.5],
        [off[0] + 3.5, off[1] + 4.5],
        color, 1.2,
    );
}

/// Thunderstorm (TS): arrow pointing right, with a kink (lightning bolt shape)
fn draw_wx_thunderstorm(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    // Lightning bolt: zigzag from top to bottom
    let pts = [
        [off[0] + 1.0, off[1] - 5.0],
        [off[0] - 2.0, off[1] - 1.0],
        [off[0] + 1.0, off[1] - 1.0],
        [off[0] - 2.0, off[1] + 3.0],
    ];
    for i in 0..pts.len() - 1 {
        painter.line(pts[i], pts[i + 1], color, 1.5);
    }
    // Small arrowhead at bottom
    painter.line(
        [off[0] - 2.0, off[1] + 3.0],
        [off[0] - 0.5, off[1] + 2.0],
        color, 1.5,
    );
    painter.line(
        [off[0] - 2.0, off[1] + 3.0],
        [off[0] - 3.5, off[1] + 2.0],
        color, 1.5,
    );
}

/// Fog (FG): three horizontal lines
fn draw_wx_fog(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    for i in -1..=1 {
        let y = off[1] + i as f32 * 3.5;
        painter.line([off[0] - 5.0, y], [off[0] + 5.0, y], color, 1.2);
    }
}

/// Mist (BR): two horizontal lines
fn draw_wx_mist(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    painter.line(
        [off[0] - 5.0, off[1] - 2.0],
        [off[0] + 5.0, off[1] - 2.0],
        color, 1.2,
    );
    painter.line(
        [off[0] - 5.0, off[1] + 2.0],
        [off[0] + 5.0, off[1] + 2.0],
        color, 1.2,
    );
}

/// Haze (HZ): figure-eight / infinity-like shape (two open circles side by side)
fn draw_wx_haze(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    painter.circle_stroke([off[0] - 3.0, off[1]], 2.5, color, 1.0);
    painter.circle_stroke([off[0] + 3.0, off[1]], 2.5, color, 1.0);
}

/// Smoke (FU/VA): a curved hook rising upward
fn draw_wx_smoke(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    // Rising curve: small "S" shaped smoke wisp
    painter.line(
        [off[0], off[1] + 4.0],
        [off[0] + 2.0, off[1] + 1.0],
        color, 1.2,
    );
    painter.line(
        [off[0] + 2.0, off[1] + 1.0],
        [off[0] - 2.0, off[1] - 2.0],
        color, 1.2,
    );
    painter.line(
        [off[0] - 2.0, off[1] - 2.0],
        [off[0], off[1] - 5.0],
        color, 1.2,
    );
}

/// Squall (SQ): arrow-like symbol pointing up-right
fn draw_wx_squall(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    // Vertical staff
    painter.line(
        [off[0], off[1] + 4.0],
        [off[0], off[1] - 4.0],
        color, 1.5,
    );
    // Arrowhead at top
    painter.line(
        [off[0], off[1] - 4.0],
        [off[0] - 3.0, off[1] - 1.0],
        color, 1.5,
    );
    painter.line(
        [off[0], off[1] - 4.0],
        [off[0] + 3.0, off[1] - 1.0],
        color, 1.5,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metar::types::Visibility;
    use rustdar_units::{SpeedUnit, UserPreferences};

    /// Records what the station model asked to be drawn, so tests can assert on
    /// geometry and text without a real painter.
    #[derive(Default)]
    struct RecordingPainter {
        texts: Vec<String>,
        /// `(radius, width)` per circle outline.
        strokes: Vec<(f32, f32)>,
        /// `(from, to)` per line segment.
        lines: Vec<([f32; 2], [f32; 2])>,
    }

    impl PointPainter for RecordingPainter {
        fn circle_filled(&mut self, _o: [f32; 2], _r: f32, _c: [u8; 4]) {}
        fn circle_stroke(&mut self, _o: [f32; 2], r: f32, _c: [u8; 4], w: f32) {
            self.strokes.push((r, w));
        }
        fn text(&mut self, _o: [f32; 2], t: &str, _c: [u8; 4], _s: f32, _a: TextAnchor) {
            self.texts.push(t.to_string());
        }
        fn line(&mut self, from: [f32; 2], to: [f32; 2], _c: [u8; 4], _w: f32) {
            self.lines.push((from, to));
        }
        fn filled_polygon(&mut self, _p: &[[f32; 2]], _c: [u8; 4]) {}
    }

    fn ob(vis: Option<Visibility>) -> MetarOb {
        wind_ob(None, None, vis)
    }

    fn wind_ob(
        dir: Option<WindDir>,
        speed: Option<u16>,
        vis: Option<Visibility>,
    ) -> MetarOb {
        MetarOb {
            station_id: "KTST".into(),
            name: "KTST".into(),
            lat: 35.0,
            lon: -97.0,
            elev_m: None,
            temp_c: None,
            dewp_c: None,
            wind_dir: dir,
            wind_speed_kt: speed,
            wind_gust_kt: None,
            visibility: vis,
            altimeter_hpa: None,
            flight_category: None,
            raw_ob: String::new(),
            clouds: Vec::new(),
            wx_string: None,
            obs_time: String::new(),
        }
    }

    fn knots() -> UserPreferences {
        UserPreferences { speed: SpeedUnit::Knots, ..Default::default() }
    }

    fn plot(vis: Option<Visibility>) -> RecordingPainter {
        let mut p = RecordingPainter::default();
        let ctx = DrawPointContext { zoom: 12.0, is_dark: true };
        draw_metar_station(&ob(vis), &mut p, &ctx);
        p
    }

    /// Formerly `if vis >= 10.0 { "10" }` — a branch the discarded `10+` values
    /// could never reach. It is reachable now, and it keeps the `+`.
    #[test]
    fn the_plot_shows_the_or_greater_marker() {
        let p = plot(Some(Visibility { miles: 10.0, or_greater: true }));
        assert!(p.texts.contains(&"10+".to_string()), "drew {:?}", p.texts);
    }

    /// And a measurement above 10 is not flattened into that bound.
    #[test]
    fn the_plot_does_not_flatten_a_measurement_into_the_bound() {
        let p = plot(Some(Visibility { miles: 15.0, or_greater: false }));
        assert!(p.texts.contains(&"15".to_string()), "drew {:?}", p.texts);
        assert!(!p.texts.contains(&"10+".to_string()));
    }

    /// Formerly `if vis >= 10.0 { "10+SM" }`, unreachable for the same reason.
    #[test]
    fn hover_text_reports_unrestricted_visibility() {
        let text =
            hover_text_for_metar(&ob(Some(Visibility { miles: 10.0, or_greater: true })), &knots());
        assert!(text.contains("10+SM"), "got {text:?}");
    }

    #[test]
    fn hover_text_keeps_a_measurement_distinct_from_the_bound() {
        let text =
            hover_text_for_metar(&ob(Some(Visibility { miles: 15.0, or_greater: false })), &knots());
        assert!(text.contains("15SM") && !text.contains("10+"), "got {text:?}");
    }

    // ── Wind barb ─────────────────────────────────────────────────────────

    fn barb(dir: Option<WindDir>, speed: Option<u16>) -> RecordingPainter {
        let mut p = RecordingPainter::default();
        draw_wind_barb(&mut p, dir, speed, 5.0, [0, 0, 0, 255]);
        p
    }

    /// A barb staff runs outward from the circle; nothing else in the barb does.
    fn staff_end(p: &RecordingPainter) -> Option<[f32; 2]> {
        p.lines.first().map(|(_, to)| *to)
    }

    /// The bug: 93 of 4,933 measured rows were variable winds at 3 kt or more,
    /// and every one drew a staff pointing due north.
    #[test]
    fn a_variable_wind_draws_no_staff_however_hard_it_blows() {
        for speed in [3, 6, 18, 25] {
            let p = barb(Some(WindDir::Variable), Some(speed));
            assert!(
                p.lines.is_empty(),
                "VRB at {speed} kt must not draw a directional staff"
            );
        }
    }

    #[test]
    fn calm_and_no_wind_data_draw_no_staff_either() {
        assert!(barb(Some(WindDir::Calm), Some(0)).lines.is_empty());
        assert!(barb(None, None).lines.is_empty());
    }

    /// A variable wind is a real wind; it must not look identical to dead calm.
    #[test]
    fn a_variable_wind_is_marked_apart_from_dead_calm() {
        let calm = barb(Some(WindDir::Calm), Some(0));
        let vrb = barb(Some(WindDir::Variable), Some(6));
        assert_eq!(calm.strokes.len(), 1, "calm draws one ring");
        assert_eq!(vrb.strokes.len(), 2, "variable adds a second ring");
        assert_ne!(vrb.strokes[0].0, vrb.strokes[1].0, "the rings differ in size");
    }

    /// The counterpart: a real 360° wind still gets a staff, pointing north.
    #[test]
    fn a_genuine_northerly_still_draws_a_northward_staff() {
        let p = barb(Some(WindDir::Degrees(360)), Some(10));
        let end = staff_end(&p).expect("360° is a bearing and must draw a staff");
        // Screen space: north is -y, and the staff runs from the circle outward.
        assert!(end[0].abs() < 1e-3, "a northerly staff has no x component: {end:?}");
        assert!(end[1] < -5.0, "a northerly staff points up-screen: {end:?}");
    }

    /// Direction still drives the staff — this is what regressed silently.
    #[test]
    fn the_staff_follows_the_reported_bearing() {
        let east = staff_end(&barb(Some(WindDir::Degrees(90)), Some(10))).unwrap();
        assert!(east[0] > 5.0 && east[1].abs() < 1e-3, "090° points right: {east:?}");

        let south = staff_end(&barb(Some(WindDir::Degrees(180)), Some(10))).unwrap();
        assert!(south[1] > 5.0 && south[0].abs() < 1e-3, "180° points down: {south:?}");
    }

    // ── Hover text ────────────────────────────────────────────────────────

    /// The `"VRB"` fallback used to be unreachable: AWC never leaves the
    /// direction column empty for a variable wind, so `wind_dir` was always
    /// `Some(0)` and the hover read "000°".
    #[test]
    fn hover_text_says_vrb_for_a_variable_wind() {
        let text =
            hover_text_for_metar(&wind_ob(Some(WindDir::Variable), Some(6), None), &knots());
        assert!(text.contains("VRB 6kt"), "got {text:?}");
        assert!(!text.contains("000"), "a variable wind is not a 000° bearing: {text:?}");
    }

    #[test]
    fn hover_text_says_calm_rather_than_a_direction_and_a_zero() {
        let text = hover_text_for_metar(&wind_ob(Some(WindDir::Calm), Some(0), None), &knots());
        assert!(text.contains("CALM"), "got {text:?}");
        assert!(!text.contains("000"), "got {text:?}");
    }

    #[test]
    fn hover_text_keeps_a_real_bearing() {
        let text =
            hover_text_for_metar(&wind_ob(Some(WindDir::Degrees(360)), Some(3), None), &knots());
        assert!(text.contains("360° 3kt"), "got {text:?}");
    }
}
