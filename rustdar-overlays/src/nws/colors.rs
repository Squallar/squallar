use crate::types::{NWS_FILL_ALPHA, STROKE_ALPHA};

/// Alert color table entry: all keywords must match (case-insensitive),
/// and the first matching entry wins (most specific entries come first).
struct AlertColorEntry {
    keywords: &'static [&'static str],
    r: u8,
    g: u8,
    b: u8,
}

/// Priority-ordered color table for NWS alert event names.
///
/// Each entry's keywords are matched case-insensitively against the event
/// name. The first entry where *all* keywords are found wins. More specific
/// entries (e.g. "tornado" + "warning") must appear before less specific
/// fallbacks (e.g. "warning" alone).
static ALERT_COLORS: &[AlertColorEntry] = &[
    // ── Warnings (most severe first) ──
    AlertColorEntry { keywords: &["tornado", "warning"],           r: 255, g: 0,   b: 0   }, // Red
    AlertColorEntry { keywords: &["severe thunderstorm", "warning"], r: 255, g: 165, b: 0   }, // Orange
    AlertColorEntry { keywords: &["flash flood", "warning"],       r: 139, g: 0,   b: 0   }, // Dark red
    AlertColorEntry { keywords: &["flood", "warning"],             r: 0,   g: 255, b: 0   }, // Green
    AlertColorEntry { keywords: &["blizzard", "warning"],          r: 255, g: 69,  b: 0   }, // OrangeRed
    AlertColorEntry { keywords: &["winter storm", "warning"],      r: 255, g: 105, b: 180 }, // Hot pink
    AlertColorEntry { keywords: &["ice storm", "warning"],         r: 139, g: 0,   b: 139 }, // Dark magenta
    AlertColorEntry { keywords: &["wind chill", "warning"],        r: 176, g: 196, b: 222 }, // Light steel blue
    AlertColorEntry { keywords: &["high wind", "warning"],         r: 218, g: 165, b: 32  }, // Goldenrod
    AlertColorEntry { keywords: &["excessive heat", "warning"],    r: 199, g: 21,  b: 133 }, // MediumVioletRed
    AlertColorEntry { keywords: &["freeze", "warning"],            r: 72,  g: 61,  b: 139 }, // Dark slate blue
    AlertColorEntry { keywords: &["fire", "warning"],              r: 255, g: 69,  b: 0   }, // OrangeRed
    AlertColorEntry { keywords: &["dust storm", "warning"],        r: 255, g: 228, b: 196 }, // Bisque

    // ── Watches ──
    AlertColorEntry { keywords: &["tornado", "watch"],             r: 255, g: 255, b: 0   }, // Yellow
    AlertColorEntry { keywords: &["severe thunderstorm", "watch"], r: 219, g: 112, b: 147 }, // PaleVioletRed
    AlertColorEntry { keywords: &["flash flood", "watch"],         r: 46,  g: 139, b: 87  }, // Sea green
    AlertColorEntry { keywords: &["flood", "watch"],               r: 46,  g: 139, b: 87  }, // Sea green
    AlertColorEntry { keywords: &["winter storm", "watch"],        r: 70,  g: 130, b: 180 }, // Steel blue
    AlertColorEntry { keywords: &["wind chill", "watch"],          r: 95,  g: 158, b: 160 }, // CadetBlue
    AlertColorEntry { keywords: &["excessive heat", "watch"],      r: 128, g: 0,   b: 0   }, // Maroon
    AlertColorEntry { keywords: &["fire", "watch"],                r: 255, g: 222, b: 173 }, // NavajoWhite

    // ── Advisories / Statements ──
    AlertColorEntry { keywords: &["wind advisory"],                r: 210, g: 180, b: 140 }, // Tan
    AlertColorEntry { keywords: &["winter weather advisory"],      r: 123, g: 104, b: 238 }, // MediumSlateBlue
    AlertColorEntry { keywords: &["frost advisory"],               r: 100, g: 149, b: 237 }, // CornflowerBlue
    AlertColorEntry { keywords: &["heat advisory"],                r: 255, g: 127, b: 80  }, // Coral
    AlertColorEntry { keywords: &["dense fog advisory"],           r: 112, g: 128, b: 144 }, // SlateGray
    AlertColorEntry { keywords: &["flood advisory"],               r: 0,   g: 255, b: 127 }, // SpringGreen
    AlertColorEntry { keywords: &["special weather statement"],    r: 255, g: 228, b: 181 }, // Moccasin

    // ── Fallbacks by category suffix ──
    AlertColorEntry { keywords: &["warning"],                      r: 255, g: 0,   b: 0   }, // Generic red
    AlertColorEntry { keywords: &["watch"],                        r: 255, g: 255, b: 0   }, // Generic yellow
    AlertColorEntry { keywords: &["advisory"],                     r: 255, g: 215, b: 0   }, // Generic gold
    AlertColorEntry { keywords: &["statement"],                    r: 255, g: 215, b: 0   }, // Generic gold
];

/// Map an NWS alert event name to (fill_rgba, stroke_rgba) colors.
///
/// Colors follow standard weather display conventions. The event name
/// is matched case-insensitively against the `ALERT_COLORS` table; the
/// first entry where all keywords match wins.
pub fn alert_color(event: &str) -> ([u8; 4], [u8; 4]) {
    let e = event.to_lowercase();
    for entry in ALERT_COLORS {
        if entry.keywords.iter().all(|kw| e.contains(kw)) {
            return rgb(entry.r, entry.g, entry.b);
        }
    }
    // Unknown event type
    rgb(200, 200, 200)
}

/// Helper: produce (fill_rgba, stroke_rgba) from an RGB triple.
fn rgb(r: u8, g: u8, b: u8) -> ([u8; 4], [u8; 4]) {
    ([r, g, b, NWS_FILL_ALPHA], [r, g, b, STROKE_ALPHA])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tornado_warning_is_red() {
        let (fill, stroke) = alert_color("Tornado Warning");
        assert_eq!(fill, [255, 0, 0, NWS_FILL_ALPHA]);
        assert_eq!(stroke, [255, 0, 0, STROKE_ALPHA]);
    }

    #[test]
    fn tornado_watch_is_yellow() {
        let (fill, _) = alert_color("Tornado Watch");
        assert_eq!(fill, [255, 255, 0, NWS_FILL_ALPHA]);
    }

    #[test]
    fn unknown_event_gets_default() {
        let (fill, _) = alert_color("Something New");
        assert_eq!(fill, [200, 200, 200, NWS_FILL_ALPHA]);
    }
}
