/// Default fill alpha for alert polygons (semi-transparent).
const FILL_ALPHA: u8 = 80;
/// Stroke is fully opaque.
const STROKE_ALPHA: u8 = 255;

/// Map an NWS alert event name to (fill_rgba, stroke_rgba) colors.
///
/// Colors follow standard weather display conventions. The event name
/// is matched case-insensitively by checking if it contains known substrings.
pub fn alert_color(event: &str) -> ([u8; 4], [u8; 4]) {
    let e = event.to_lowercase();

    // ── Warnings (most severe first) ──
    if e.contains("tornado") && e.contains("warning") {
        return rgb(255, 0, 0); // Red
    }
    if e.contains("severe thunderstorm") && e.contains("warning") {
        return rgb(255, 165, 0); // Orange
    }
    if e.contains("flash flood") && e.contains("warning") {
        return rgb(139, 0, 0); // Dark red
    }
    if e.contains("flood") && e.contains("warning") {
        return rgb(0, 255, 0); // Green
    }
    if e.contains("blizzard") && e.contains("warning") {
        return rgb(255, 69, 0); // OrangeRed
    }
    if e.contains("winter storm") && e.contains("warning") {
        return rgb(255, 105, 180); // Hot pink
    }
    if e.contains("ice storm") && e.contains("warning") {
        return rgb(139, 0, 139); // Dark magenta
    }
    if e.contains("wind chill") && e.contains("warning") {
        return rgb(176, 196, 222); // Light steel blue
    }
    if e.contains("high wind") && e.contains("warning") {
        return rgb(218, 165, 32); // Goldenrod
    }
    if e.contains("excessive heat") && e.contains("warning") {
        return rgb(199, 21, 133); // MediumVioletRed
    }
    if e.contains("freeze") && e.contains("warning") {
        return rgb(72, 61, 139); // Dark slate blue
    }
    if e.contains("fire") && e.contains("warning") {
        return rgb(255, 69, 0); // OrangeRed
    }
    if e.contains("dust storm") && e.contains("warning") {
        return rgb(255, 228, 196); // Bisque
    }

    // ── Watches ──
    if e.contains("tornado") && e.contains("watch") {
        return rgb(255, 255, 0); // Yellow
    }
    if e.contains("severe thunderstorm") && e.contains("watch") {
        return rgb(219, 112, 147); // PaleVioletRed
    }
    if e.contains("flash flood") && e.contains("watch") {
        return rgb(46, 139, 87); // Sea green
    }
    if e.contains("flood") && e.contains("watch") {
        return rgb(46, 139, 87); // Sea green
    }
    if e.contains("winter storm") && e.contains("watch") {
        return rgb(70, 130, 180); // Steel blue
    }
    if e.contains("wind chill") && e.contains("watch") {
        return rgb(95, 158, 160); // CadetBlue
    }
    if e.contains("excessive heat") && e.contains("watch") {
        return rgb(128, 0, 0); // Maroon
    }
    if e.contains("fire") && e.contains("watch") {
        return rgb(255, 222, 173); // NavajoWhite
    }

    // ── Advisories / Statements ──
    if e.contains("wind advisory") {
        return rgb(210, 180, 140); // Tan
    }
    if e.contains("winter weather advisory") {
        return rgb(123, 104, 238); // MediumSlateBlue
    }
    if e.contains("frost advisory") {
        return rgb(100, 149, 237); // CornflowerBlue
    }
    if e.contains("heat advisory") {
        return rgb(255, 127, 80); // Coral
    }
    if e.contains("dense fog advisory") {
        return rgb(112, 128, 144); // SlateGray
    }
    if e.contains("flood advisory") {
        return rgb(0, 255, 127); // SpringGreen
    }
    if e.contains("special weather statement") {
        return rgb(255, 228, 181); // Moccasin
    }

    // ── Fallback by category suffix ──
    if e.contains("warning") {
        return rgb(255, 0, 0); // Generic red warning
    }
    if e.contains("watch") {
        return rgb(255, 255, 0); // Generic yellow watch
    }
    if e.contains("advisory") || e.contains("statement") {
        return rgb(255, 215, 0); // Generic gold advisory
    }

    // Unknown event type
    rgb(200, 200, 200)
}

/// Helper: produce (fill_rgba, stroke_rgba) from an RGB triple.
fn rgb(r: u8, g: u8, b: u8) -> ([u8; 4], [u8; 4]) {
    ([r, g, b, FILL_ALPHA], [r, g, b, STROKE_ALPHA])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tornado_warning_is_red() {
        let (fill, stroke) = alert_color("Tornado Warning");
        assert_eq!(fill, [255, 0, 0, FILL_ALPHA]);
        assert_eq!(stroke, [255, 0, 0, STROKE_ALPHA]);
    }

    #[test]
    fn tornado_watch_is_yellow() {
        let (fill, _) = alert_color("Tornado Watch");
        assert_eq!(fill, [255, 255, 0, FILL_ALPHA]);
    }

    #[test]
    fn unknown_event_gets_default() {
        let (fill, _) = alert_color("Something New");
        assert_eq!(fill, [200, 200, 200, FILL_ALPHA]);
    }
}
