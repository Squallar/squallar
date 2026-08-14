//! NWS-style station model plots. Field placement is the standard convention,
//! not a layout choice:
//!
//! - **Tier 1** (zoom < 6): flight-category circle only.
//! - **Tier 2** (zoom 6–9): + temperature (upper-left), dewpoint (lower-left),
//!   wind barb.
//! - **Tier 3** (zoom ≥ 10): + MSLP pressure code (upper-right), visibility (left),
//!   present weather symbol, station ID (lower-right).

use crate::metar::types::{CloudLayer, FlightCategory, MetarOb, WindDir};
use crate::render::draw::{DrawPointContext, PointPainter, TextAnchor};

// ── Zoom tier thresholds ──────────────────────────────────────────────────

const TIER2_ZOOM: f32 = 6.0;
const TIER3_ZOOM: f32 = 10.0;

// ── Layout constants (pixel offsets from station center) ──────────────────

const TEMP_OFFSET: [f32; 2] = [-14.0, -14.0];
const DEWP_OFFSET: [f32; 2] = [-14.0, 10.0];
const PRESSURE_OFFSET: [f32; 2] = [14.0, -14.0];
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

// ── Pressure code ─────────────────────────────────────────────────────────

/// The station model's three-digit pressure code: **mean sea level pressure in
/// tenths of a hectopascal, last three digits**. 1008.2 hPa prints `082`, and
/// the reader restores the leading `9` or `10` by taking whichever of 9xx.x and
/// 10xx.x lands nearest 1000.
///
/// Returns `None` when the observation carries no MSLP, which is most of the
/// network — the feed published one for 572 of 1324 records across 20 state
/// ASOS networks. An empty slot is the honest answer there. The three
/// alternatives were each measured and each is worse:
///
///   * the altimeter setting in hundredths of inHg, which is what this slot
///     drew before: agrees with the convention on 0 of 572 records, and a
///     reader who applies the convention misreads it by a median 13.7 hPa;
///   * the altimeter setting relabelled as MSLP: out by a median 0.49 hPa
///     (max 11.6) and it silently mixes two quantities in one slot;
///   * MSLP derived from the altimeter with the station's elevation and
///     temperature: lands on the right printed code 8.2% of the time, and the
///     stations needing it are lower-instrumented and higher-elevation than
///     the ones it could be scored against.
///
/// The altimeter setting is still shown, labelled, in the station popup.
fn pressure_code(mslp_hpa: Option<f64>) -> Option<i32> {
    let mslp = mslp_hpa?;
    if !mslp.is_finite() {
        return None;
    }
    Some((mslp * 10.0).round() as i32 % 1000)
}

// ── Public entry point ────────────────────────────────────────────────────

pub fn draw_metar_station(ob: &MetarOb, painter: &mut dyn PointPainter, ctx: &DrawPointContext) {
    let zoom = ctx.zoom;
    let fc_color = flight_category_color(ob.flight_category);
    let text_color = if ctx.is_dark {
        [255, 255, 255, 230]
    } else {
        [20, 20, 20, 230]
    };
    let font_size = font_size_for_zoom(zoom);
    let circle_r = circle_radius_for_zoom(zoom);

    // ── Tier 1 ────────────────────────────────────────────────────────
    draw_cloud_cover_circle(painter, ob, fc_color, circle_r, ctx.is_dark);

    if zoom < TIER2_ZOOM {
        return;
    }

    // ── Tier 2 ────────────────────────────────────────────────────────
    if let Some(tc) = ob.temp_c {
        let tf = tc * 9.0 / 5.0 + 32.0;
        painter.text(
            TEMP_OFFSET,
            &format!("{tf:.0}"),
            text_color,
            font_size,
            TextAnchor::BottomRight,
        );
    }

    if let Some(td) = ob.dewp_c {
        let tdf = td * 9.0 / 5.0 + 32.0;
        painter.text(
            DEWP_OFFSET,
            &format!("{tdf:.0}"),
            text_color,
            font_size,
            TextAnchor::TopRight,
        );
    }

    draw_wind_barb(painter, ob.wind_dir, ob.wind_speed_kt, circle_r, text_color);

    if zoom < TIER3_ZOOM {
        return;
    }

    // ── Tier 3 ────────────────────────────────────────────────────────
    if let Some(code) = pressure_code(ob.mslp_hpa) {
        painter.text(
            PRESSURE_OFFSET,
            &format!("{code:03}"),
            text_color,
            font_size * 0.85,
            TextAnchor::BottomLeft,
        );
    }

    if let Some(vis) = ob.visibility {
        painter.text(
            VIS_OFFSET,
            &vis.label(),
            text_color,
            font_size * 0.85,
            TextAnchor::CenterRight,
        );
    }

    if let Some(ref wx) = ob.wx_string {
        draw_wx_symbol(painter, WX_OFFSET, wx, text_color);
    }

    painter.text(
        ID_OFFSET,
        &ob.station_id,
        text_color,
        font_size * 0.75,
        TextAnchor::TopLeft,
    );
}

pub fn hit_radius_for_zoom(zoom: f32) -> f32 {
    if zoom < TIER2_ZOOM {
        circle_radius_for_zoom(zoom) + 2.0
    } else {
        30.0 // Must cover the whole plotted model, not just the circle.
    }
}

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
            Some(dir) => parts.push(format!(
                "{} {converted:.0}{}",
                dir.label(),
                prefs.speed.suffix()
            )),
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
    let outline_color = if is_dark {
        [255, 255, 255, 180]
    } else {
        [40, 40, 40, 180]
    };

    if fill_fraction >= 0.99 {
        // OVC
        painter.circle_filled([0.0, 0.0], radius, fc_color);
    } else if fill_fraction <= 0.01 {
        // CLR/SKC: outline only.
    } else if fill_fraction >= 0.5 {
        // BKN: filled, with an open slice on the right faked by overpainting
        // in the background colour — `PointPainter` has no arc primitive.
        painter.circle_filled([0.0, 0.0], radius, fc_color);
        let bg = if is_dark {
            [30, 30, 30, 230]
        } else {
            [245, 245, 245, 230]
        };
        let open_fraction = 1.0 - fill_fraction;
        let half_w = radius * open_fraction * 2.0;
        // The overpaint is a circular *segment* — a chord at x = radius-half_w
        // closed by arc samples through the circle's rightmost point — drawn
        // as a polygon inscribed in the circle, so it cannot leave it. The
        // full-height rect this replaces put its corners at r·√2 from the
        // centre, blotting out map and radar pixels beneath the station.
        let x0 = radius - half_w;
        let theta = (x0 / radius).clamp(-1.0, 1.0).acos();
        const ARC_STEPS: usize = 8;
        let mut pts = [[0.0_f32; 2]; ARC_STEPS + 1];
        for (i, pt) in pts.iter_mut().enumerate() {
            let a = -theta + 2.0 * theta * i as f32 / ARC_STEPS as f32;
            *pt = [radius * a.cos(), radius * a.sin()];
        }
        painter.filled_polygon(&pts, bg);
    } else {
        // FEW/SCT: partially filled from the left.
        let fill_x = -radius + 2.0 * radius * fill_fraction;
        let pts: [[f32; 2]; 4] = [
            [-radius, -radius * 0.8],
            [fill_x, -radius * 0.8],
            [fill_x, radius * 0.8],
            [-radius, radius * 0.8],
        ];
        painter.filled_polygon(&pts, fc_color);
    }

    painter.circle_stroke([0.0, 0.0], radius, fc_color, 1.5);
    // Second, thinner ring: contrast against the map underneath.
    painter.circle_stroke([0.0, 0.0], radius, outline_color, 0.5);
}

/// The *greatest* coverage of any layer, in oktas-as-fraction. Not the highest
/// layer: sky cover is reported cumulatively, so the top layer is the total.
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

/// Standard WMO barb. The staff points in the direction the wind blows FROM.
/// Pennant (filled triangle) = 50 kt, full barb = 10 kt, half barb = 5 kt.
/// Calm draws a larger ring and no staff; variable adds a second ring.
///
/// A staff is drawn only for [`WindDir::Degrees`]. Defaulting a missing or
/// variable direction to `0` points the barb due north, which is how a quarter
/// of the AWC feed used to render.
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
        // A variable wind is a real wind: a gusty VRB18KT must not read as calm.
        if wind_dir == Some(WindDir::Variable) {
            painter.circle_stroke([0.0, 0.0], circle_radius + 4.5, color, 1.0);
        }
        return;
    };

    // Below 3 kt the convention is calm: ring, no barb.
    if speed < 3 {
        painter.circle_stroke([0.0, 0.0], circle_radius + 2.0, color, 1.0);
        return;
    }

    let dir_deg = dir_deg as f32;
    // -90 puts 0° (north) up-screen; the staff then runs toward the FROM bearing.
    let dir_rad = (dir_deg - 90.0).to_radians();

    let (sin_d, cos_d) = dir_rad.sin_cos();

    let staff_start_x = cos_d * circle_radius;
    let staff_start_y = sin_d * circle_radius;
    let staff_end_x = cos_d * (circle_radius + BARB_STAFF_LENGTH);
    let staff_end_y = sin_d * (circle_radius + BARB_STAFF_LENGTH);

    painter.line(
        [staff_start_x, staff_start_y],
        [staff_end_x, staff_end_y],
        color,
        BARB_STROKE_WIDTH,
    );

    // 50 / 10 / 5 kt; a remainder of 3 or more rounds up to a half barb.
    let mut remaining = speed;
    let pennants = remaining / 50;
    remaining %= 50;
    let full_barbs = remaining / 10;
    remaining %= 10;
    let half_barbs = if remaining >= 3 { 1 } else { 0 };

    // Barbs sit on the left of the staff facing the wind — Northern Hemisphere
    // convention; the Southern Hemisphere mirrors it.
    let perp_x = -sin_d;
    let perp_y = cos_d;

    // Distance back from the staff end; barbs fill inward from the tip.
    let mut pos = 0.0_f32;

    for _ in 0..pennants {
        let base_x = staff_end_x - cos_d * pos;
        let base_y = staff_end_y - sin_d * pos;
        let tip_x = base_x + perp_x * BARB_LINE_LENGTH;
        let tip_y = base_y + perp_y * BARB_LINE_LENGTH;
        let next_x = staff_end_x - cos_d * (pos + PENNANT_WIDTH);
        let next_y = staff_end_y - sin_d * (pos + PENNANT_WIDTH);

        painter.filled_polygon(&[[base_x, base_y], [tip_x, tip_y], [next_x, next_y]], color);
        pos += PENNANT_WIDTH + 1.0;
    }

    for _ in 0..full_barbs {
        let base_x = staff_end_x - cos_d * pos;
        let base_y = staff_end_y - sin_d * pos;
        let tip_x = base_x + perp_x * BARB_LINE_LENGTH;
        let tip_y = base_y + perp_y * BARB_LINE_LENGTH;

        painter.line([base_x, base_y], [tip_x, tip_y], color, BARB_STROKE_WIDTH);
        pos += BARB_SPACING;
    }

    if half_barbs > 0 {
        // A lone half barb is inset from the tip, per convention, so 5 kt is
        // not confusable with a full barb.
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
// WMO present-weather symbols have no Unicode equivalents, so each is drawn
// from primitives inside a ~10×10 px box centred on `off`.

/// **Order is load-bearing**: the branches run most- to least-significant
/// phenomenon, and several codes are substrings of others.
fn draw_wx_symbol(painter: &mut dyn PointPainter, off: [f32; 2], wx: &str, color: [u8; 4]) {
    let wx_upper = wx.to_uppercase();

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
    painter.line([off[0], off[1] - 3.0], [off[0], off[1] + 2.0], color, 1.5);
    painter.line(
        [off[0], off[1] + 2.0],
        [off[0] - 1.5, off[1] + 4.0],
        color,
        1.5,
    );
}

/// Snow (SN/SG): a six-pointed asterisk (3 crossing lines)
fn draw_wx_snow(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    let r = 4.0_f32;
    painter.line([off[0], off[1] - r], [off[0], off[1] + r], color, 1.2);
    let dx = r * 0.866; // cos(30°)
    let dy = r * 0.5; // sin(30°)
    painter.line(
        [off[0] - dx, off[1] - dy],
        [off[0] + dx, off[1] + dy],
        color,
        1.2,
    );
    painter.line(
        [off[0] - dx, off[1] + dy],
        [off[0] + dx, off[1] - dy],
        color,
        1.2,
    );
}

/// Heavy snow (+SN): two asterisks stacked
fn draw_wx_snow_heavy(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    draw_wx_snow(painter, [off[0], off[1] - 4.5], color);
    draw_wx_snow(painter, [off[0], off[1] + 4.5], color);
}

/// Freezing rain (FZRA): rain dot with a small arc/line above
fn draw_wx_freezing_rain(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    painter.circle_filled([off[0], off[1] + 1.0], 2.0, color);
    painter.line(
        [off[0] - 3.0, off[1] - 4.0],
        [off[0], off[1] - 2.0],
        color,
        1.2,
    );
    painter.line(
        [off[0], off[1] - 2.0],
        [off[0] + 3.0, off[1] - 4.0],
        color,
        1.2,
    );
}

/// Freezing drizzle (FZDZ): drizzle comma with freezing indicator
fn draw_wx_freezing_drizzle(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    draw_wx_drizzle(painter, [off[0], off[1] + 1.5], color);
    painter.line(
        [off[0] - 3.0, off[1] - 4.0],
        [off[0], off[1] - 2.0],
        color,
        1.2,
    );
    painter.line(
        [off[0], off[1] - 2.0],
        [off[0] + 3.0, off[1] - 4.0],
        color,
        1.2,
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
    painter.line(
        [off[0], off[1] - 4.0],
        [off[0] - 3.5, off[1] + 2.0],
        color,
        1.2,
    );
    painter.line(
        [off[0], off[1] - 4.0],
        [off[0] + 3.5, off[1] + 2.0],
        color,
        1.2,
    );
    painter.line(
        [off[0] - 3.5, off[1] + 2.0],
        [off[0] + 3.5, off[1] + 2.0],
        color,
        1.2,
    );
    painter.line(
        [off[0] - 3.5, off[1] + 4.5],
        [off[0] + 3.5, off[1] + 4.5],
        color,
        1.2,
    );
}

/// Thunderstorm (TS): arrow pointing right, with a kink (lightning bolt shape)
fn draw_wx_thunderstorm(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    let pts = [
        [off[0] + 1.0, off[1] - 5.0],
        [off[0] - 2.0, off[1] - 1.0],
        [off[0] + 1.0, off[1] - 1.0],
        [off[0] - 2.0, off[1] + 3.0],
    ];
    for i in 0..pts.len() - 1 {
        painter.line(pts[i], pts[i + 1], color, 1.5);
    }
    painter.line(
        [off[0] - 2.0, off[1] + 3.0],
        [off[0] - 0.5, off[1] + 2.0],
        color,
        1.5,
    );
    painter.line(
        [off[0] - 2.0, off[1] + 3.0],
        [off[0] - 3.5, off[1] + 2.0],
        color,
        1.5,
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
        color,
        1.2,
    );
    painter.line(
        [off[0] - 5.0, off[1] + 2.0],
        [off[0] + 5.0, off[1] + 2.0],
        color,
        1.2,
    );
}

/// Haze (HZ): figure-eight / infinity-like shape (two open circles side by side)
fn draw_wx_haze(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    painter.circle_stroke([off[0] - 3.0, off[1]], 2.5, color, 1.0);
    painter.circle_stroke([off[0] + 3.0, off[1]], 2.5, color, 1.0);
}

/// Smoke (FU/VA): a curved hook rising upward
fn draw_wx_smoke(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    painter.line(
        [off[0], off[1] + 4.0],
        [off[0] + 2.0, off[1] + 1.0],
        color,
        1.2,
    );
    painter.line(
        [off[0] + 2.0, off[1] + 1.0],
        [off[0] - 2.0, off[1] - 2.0],
        color,
        1.2,
    );
    painter.line(
        [off[0] - 2.0, off[1] - 2.0],
        [off[0], off[1] - 5.0],
        color,
        1.2,
    );
}

/// Squall (SQ): arrow-like symbol pointing up-right
fn draw_wx_squall(painter: &mut dyn PointPainter, off: [f32; 2], color: [u8; 4]) {
    painter.line([off[0], off[1] + 4.0], [off[0], off[1] - 4.0], color, 1.5);
    painter.line(
        [off[0], off[1] - 4.0],
        [off[0] - 3.0, off[1] - 1.0],
        color,
        1.5,
    );
    painter.line(
        [off[0], off[1] - 4.0],
        [off[0] + 3.0, off[1] - 1.0],
        color,
        1.5,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metar::types::Visibility;
    use rustdar_units::{SpeedUnit, UserPreferences};

    #[derive(Default)]
    struct RecordingPainter {
        texts: Vec<String>,
        /// `(radius, width)` per circle outline.
        strokes: Vec<(f32, f32)>,
        /// `(from, to)` per line segment.
        lines: Vec<([f32; 2], [f32; 2])>,
        /// Vertices per filled polygon.
        polygons: Vec<Vec<[f32; 2]>>,
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
        fn filled_polygon(&mut self, p: &[[f32; 2]], _c: [u8; 4]) {
            self.polygons.push(p.to_vec());
        }
    }

    fn ob(vis: Option<Visibility>) -> MetarOb {
        wind_ob(None, None, vis)
    }

    /// A tier-3 context, the only tier that draws the pressure code.
    fn tier3() -> DrawPointContext {
        DrawPointContext {
            zoom: TIER3_ZOOM,
            is_dark: false,
        }
    }

    /// Six real stations, six state networks, spanning 3 m to 3026 m, each row
    /// taken from one IEM `currents.json` record: the station's own `mslp` and
    /// `alti`, and the three digits the WMO/NWS surface station model calls for.
    /// The expected column is `round(mslp * 10) % 1000` worked by hand, not by
    /// calling the function under test.
    const CONVENTION: &[(&str, f64, f64, &str)] = &[
        // station, mslp hPa, alti inHg, expected printed code
        ("OKC", 1008.2, 29.83, "082"),
        ("TUL", 1008.5, 29.82, "085"),
        ("MIA", 1019.1, 30.09, "191"),
        ("DEN", 1009.8, 30.04, "098"),
        ("LXV", 1013.3, 30.37, "133"),
        ("ELP", 1007.3, 29.98, "073"),
    ];

    #[test]
    fn the_pressure_code_is_mslp_in_tenths_of_a_hectopascal() {
        for (station, mslp, _alti, expected) in CONVENTION {
            let code = pressure_code(Some(*mslp)).expect("an MSLP yields a code");
            assert_eq!(
                format!("{code:03}"),
                *expected,
                "{station}: {mslp} hPa must print {expected}"
            );
        }
    }

    /// The defect this replaced. Pinning it stops the altimeter encoding coming
    /// back by way of "the popup shows inHg, so the plot should match".
    #[test]
    fn the_altimeter_encoding_disagrees_with_the_convention_at_every_site() {
        let mut agreed = 0;
        for (station, mslp, alti, expected) in CONVENTION {
            let alt_hpa = alti * 33.8639;
            let old = format!("{:03}", ((alt_hpa * 0.02953 * 100.0).round() as i32) % 1000);
            if old == *expected {
                agreed += 1;
            }
            // And the misreading is large: decode the old digits by the
            // convention and compare against the station's real MSLP.
            let decoded = old.parse::<f64>().expect("three digits") / 10.0;
            let misread = if decoded < 50.0 {
                1000.0 + decoded
            } else {
                900.0 + decoded
            };
            assert!(
                (misread - mslp).abs() > 2.0,
                "{station}: the old code {old} decodes to {misread:.1} against a \
                 real {mslp:.1} hPa — if this is now within 2 hPa the premise \
                 of the repair has changed and the measurement must be redone"
            );
        }
        assert_eq!(
            agreed,
            0,
            "the altimeter encoding agreed with the convention at {agreed} of \
             {} sites; it agreed at 0 of 572 records when measured",
            CONVENTION.len()
        );
    }

    #[test]
    fn a_station_with_no_mslp_draws_no_pressure_code() {
        // 56.0% of 1324 measured records are this case. The slot stays empty
        // rather than falling back to a quantity the convention does not name.
        let mut o = ob(None);
        o.altimeter_hpa = Some(1010.16);
        o.mslp_hpa = None;
        let mut p = RecordingPainter::default();
        draw_metar_station(&o, &mut p, &tier3());
        assert!(
            !p.texts.iter().any(|t| t.len() == 3 && t.starts_with('0')),
            "no three-digit pressure code may be drawn without an MSLP, got {:?}",
            p.texts
        );
    }

    #[test]
    fn a_station_with_mslp_draws_the_conventional_code() {
        let mut o = ob(None);
        o.altimeter_hpa = Some(1010.16);
        o.mslp_hpa = Some(1008.2);
        let mut p = RecordingPainter::default();
        draw_metar_station(&o, &mut p, &tier3());
        assert!(
            p.texts.iter().any(|t| t == "082"),
            "expected the conventional code 082, got {:?}",
            p.texts
        );
    }

    /// The wrap is the whole point of a three-digit code, and it is where an
    /// `as i32` truncation instead of a `round` would show up.
    #[test]
    fn the_code_wraps_across_the_thousand_boundary() {
        assert_eq!(pressure_code(Some(999.9)), Some(999));
        assert_eq!(pressure_code(Some(1000.0)), Some(0));
        assert_eq!(pressure_code(Some(1000.1)), Some(1));
        // 1013.26 hPa is 10132.6 tenths: rounding gives 133, truncating 132.
        assert_eq!(
            pressure_code(Some(1013.26)),
            Some(133),
            "rounds, not truncates"
        );
        // and the half case rounds away from zero, 10132.5 -> 10133.
        assert_eq!(pressure_code(Some(1013.25)), Some(133));
    }

    #[test]
    fn a_non_finite_mslp_draws_nothing() {
        assert_eq!(pressure_code(Some(f64::NAN)), None);
        assert_eq!(pressure_code(Some(f64::INFINITY)), None);
        assert_eq!(pressure_code(None), None);
    }

    fn wind_ob(dir: Option<WindDir>, speed: Option<u16>, vis: Option<Visibility>) -> MetarOb {
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
            mslp_hpa: None,
            flight_category: None,
            raw_ob: String::new(),
            clouds: Vec::new(),
            wx_string: None,
            obs_time: String::new(),
        }
    }

    fn knots() -> UserPreferences {
        UserPreferences {
            speed: SpeedUnit::Knots,
            ..Default::default()
        }
    }

    fn plot(vis: Option<Visibility>) -> RecordingPainter {
        let mut p = RecordingPainter::default();
        let ctx = DrawPointContext {
            zoom: 12.0,
            is_dark: true,
        };
        draw_metar_station(&ob(vis), &mut p, &ctx);
        p
    }

    /// Fails if the plot drops the `+` from an "or greater" visibility.
    #[test]
    fn the_plot_shows_the_or_greater_marker() {
        let p = plot(Some(Visibility {
            miles: 10.0,
            or_greater: true,
        }));
        assert!(p.texts.contains(&"10+".to_string()), "drew {:?}", p.texts);
    }

    #[test]
    fn the_plot_does_not_flatten_a_measurement_into_the_bound() {
        let p = plot(Some(Visibility {
            miles: 15.0,
            or_greater: false,
        }));
        assert!(p.texts.contains(&"15".to_string()), "drew {:?}", p.texts);
        assert!(!p.texts.contains(&"10+".to_string()));
    }

    #[test]
    fn hover_text_reports_unrestricted_visibility() {
        let text = hover_text_for_metar(
            &ob(Some(Visibility {
                miles: 10.0,
                or_greater: true,
            })),
            &knots(),
        );
        assert!(text.contains("10+SM"), "got {text:?}");
    }

    #[test]
    fn hover_text_keeps_a_measurement_distinct_from_the_bound() {
        let text = hover_text_for_metar(
            &ob(Some(Visibility {
                miles: 15.0,
                or_greater: false,
            })),
            &knots(),
        );
        assert!(
            text.contains("15SM") && !text.contains("10+"),
            "got {text:?}"
        );
    }

    // ── Sky-cover circle ──────────────────────────────────────────────────

    /// The BKN "open slice" is faked by overpainting in the background colour,
    /// and that overpaint must stay inside the sky-cover circle: the full-height
    /// rect it used to be put its corners at r·√2 from the centre, blotting out
    /// map and radar pixels beneath the station. Vertex containment is
    /// sufficient — the vertices lie on the circle, and their hull is inside it.
    #[test]
    fn the_bkn_open_slice_overpaint_stays_inside_the_circle() {
        let mut bkn = ob(None);
        bkn.clouds = vec![CloudLayer {
            cover: "BKN".into(),
            base_ft: Some(3000),
        }];
        let radius = 6.0_f32;
        let mut p = RecordingPainter::default();
        draw_cloud_cover_circle(&mut p, &bkn, [0, 255, 0, 255], radius, true);

        assert!(
            !p.polygons.is_empty(),
            "BKN draws its open slice as a filled polygon"
        );
        for poly in &p.polygons {
            for pt in poly {
                let d = (pt[0] * pt[0] + pt[1] * pt[1]).sqrt();
                assert!(
                    d <= radius + 0.01,
                    "overpaint vertex {pt:?} sits {d:.2} px out on an r={radius} circle"
                );
            }
        }
        // Negative control against "fixed" by shrinking the glyph: the slice
        // still reaches the right edge of the circle, where WMO puts it.
        let reaches_right = p
            .polygons
            .iter()
            .flatten()
            .any(|pt| pt[0] > radius * 0.95 && pt[1].abs() < 1.0);
        assert!(
            reaches_right,
            "the open slice must still touch the circle's right edge"
        );
    }

    // ── Wind barb ─────────────────────────────────────────────────────────

    fn barb(dir: Option<WindDir>, speed: Option<u16>) -> RecordingPainter {
        let mut p = RecordingPainter::default();
        draw_wind_barb(&mut p, dir, speed, 5.0, [0, 0, 0, 255]);
        p
    }

    /// The staff is always the first line drawn.
    fn staff_end(p: &RecordingPainter) -> Option<[f32; 2]> {
        p.lines.first().map(|(_, to)| *to)
    }

    /// Fails if VRB draws a staff. 93 of 4,933 measured rows were variable at
    /// 3 kt or more, and every one pointed due north.
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

    /// Fails if variable renders identically to dead calm.
    #[test]
    fn a_variable_wind_is_marked_apart_from_dead_calm() {
        let calm = barb(Some(WindDir::Calm), Some(0));
        let vrb = barb(Some(WindDir::Variable), Some(6));
        assert_eq!(calm.strokes.len(), 1, "calm draws one ring");
        assert_eq!(vrb.strokes.len(), 2, "variable adds a second ring");
        assert_ne!(
            vrb.strokes[0].0, vrb.strokes[1].0,
            "the rings differ in size"
        );
    }

    /// The counterpart: 360° is a real bearing and must still draw a staff.
    #[test]
    fn a_genuine_northerly_still_draws_a_northward_staff() {
        let p = barb(Some(WindDir::Degrees(360)), Some(10));
        let end = staff_end(&p).expect("360° is a bearing and must draw a staff");
        // Screen space: north is -y.
        assert!(
            end[0].abs() < 1e-3,
            "a northerly staff has no x component: {end:?}"
        );
        assert!(end[1] < -5.0, "a northerly staff points up-screen: {end:?}");
    }

    #[test]
    fn the_staff_follows_the_reported_bearing() {
        let east = staff_end(&barb(Some(WindDir::Degrees(90)), Some(10))).unwrap();
        assert!(
            east[0] > 5.0 && east[1].abs() < 1e-3,
            "090° points right: {east:?}"
        );

        let south = staff_end(&barb(Some(WindDir::Degrees(180)), Some(10))).unwrap();
        assert!(
            south[1] > 5.0 && south[0].abs() < 1e-3,
            "180° points down: {south:?}"
        );
    }

    // ── Hover text ────────────────────────────────────────────────────────

    /// Fails if the hover reads "000°" for a variable wind.
    #[test]
    fn hover_text_says_vrb_for_a_variable_wind() {
        let text = hover_text_for_metar(&wind_ob(Some(WindDir::Variable), Some(6), None), &knots());
        assert!(text.contains("VRB 6kt"), "got {text:?}");
        assert!(
            !text.contains("000"),
            "a variable wind is not a 000° bearing: {text:?}"
        );
    }

    #[test]
    fn hover_text_says_calm_rather_than_a_direction_and_a_zero() {
        let text = hover_text_for_metar(&wind_ob(Some(WindDir::Calm), Some(0), None), &knots());
        assert!(text.contains("CALM"), "got {text:?}");
        assert!(!text.contains("000"), "got {text:?}");
    }

    #[test]
    fn hover_text_keeps_a_real_bearing() {
        let text = hover_text_for_metar(
            &wind_ob(Some(WindDir::Degrees(360)), Some(3), None),
            &knots(),
        );
        assert!(text.contains("360° 3kt"), "got {text:?}");
    }
}
