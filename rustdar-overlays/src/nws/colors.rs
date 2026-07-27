use crate::types::{NWS_FILL_ALPHA, STROKE_ALPHA};

/// All keywords must match; first matching entry wins.
struct AlertColorEntry {
    keywords: &'static [&'static str],
    r: u8,
    g: u8,
    b: u8,
}

impl AlertColorEntry {
    /// `lowercased_event` must already be lowercased; keywords are not.
    fn matches(&self, lowercased_event: &str) -> bool {
        self.keywords.iter().all(|kw| lowercased_event.contains(kw))
    }
}

/// **Order is load-bearing**: specific entries ("tornado" + "warning") must
/// precede less specific ones ("warning"). Every entry must match a real name
/// in `event_types.txt` — see `every_live_entry_matches_a_real_event`.
static ALERT_COLORS: &[AlertColorEntry] = &[
    // ── Warnings (most severe first) ──
    AlertColorEntry {
        keywords: &["tornado", "warning"],
        r: 255,
        g: 0,
        b: 0,
    }, // Red (reserved)
    AlertColorEntry {
        keywords: &["severe thunderstorm", "warning"],
        r: 255,
        g: 165,
        b: 0,
    }, // Orange
    AlertColorEntry {
        keywords: &["flash flood", "warning"],
        r: 139,
        g: 0,
        b: 0,
    }, // Dark red
    AlertColorEntry {
        keywords: &["flood", "warning"],
        r: 0,
        g: 255,
        b: 0,
    }, // Green
    AlertColorEntry {
        keywords: &["blizzard", "warning"],
        r: 255,
        g: 69,
        b: 0,
    }, // OrangeRed
    AlertColorEntry {
        keywords: &["winter storm", "warning"],
        r: 255,
        g: 105,
        b: 180,
    }, // Hot pink
    AlertColorEntry {
        keywords: &["ice storm", "warning"],
        r: 139,
        g: 0,
        b: 139,
    }, // Dark magenta
    AlertColorEntry {
        keywords: &["extreme cold", "warning"],
        r: 176,
        g: 196,
        b: 222,
    }, // Light steel blue
    AlertColorEntry {
        keywords: &["high wind", "warning"],
        r: 218,
        g: 165,
        b: 32,
    }, // Goldenrod
    AlertColorEntry {
        keywords: &["extreme heat", "warning"],
        r: 199,
        g: 21,
        b: 133,
    }, // MediumVioletRed
    AlertColorEntry {
        keywords: &["freeze", "warning"],
        r: 72,
        g: 61,
        b: 139,
    }, // Dark slate blue
    AlertColorEntry {
        keywords: &["red flag", "warning"],
        r: 210,
        g: 105,
        b: 30,
    }, // Chocolate
    AlertColorEntry {
        keywords: &["fire", "warning"],
        r: 205,
        g: 92,
        b: 92,
    }, // IndianRed
    AlertColorEntry {
        keywords: &["dust storm", "warning"],
        r: 255,
        g: 228,
        b: 196,
    }, // Bisque
    AlertColorEntry {
        keywords: &["gale", "warning"],
        r: 218,
        g: 112,
        b: 214,
    }, // Orchid
    // ── Watches ──
    AlertColorEntry {
        keywords: &["tornado", "watch"],
        r: 255,
        g: 255,
        b: 0,
    }, // Yellow (reserved)
    AlertColorEntry {
        keywords: &["severe thunderstorm", "watch"],
        r: 219,
        g: 112,
        b: 147,
    }, // PaleVioletRed
    AlertColorEntry {
        keywords: &["flash flood", "watch"],
        r: 60,
        g: 179,
        b: 113,
    }, // MediumSeaGreen
    AlertColorEntry {
        keywords: &["flood", "watch"],
        r: 46,
        g: 139,
        b: 87,
    }, // Sea green
    AlertColorEntry {
        keywords: &["winter storm", "watch"],
        r: 70,
        g: 130,
        b: 180,
    }, // Steel blue
    AlertColorEntry {
        keywords: &["extreme cold", "watch"],
        r: 95,
        g: 158,
        b: 160,
    }, // CadetBlue
    AlertColorEntry {
        keywords: &["extreme heat", "watch"],
        r: 128,
        g: 0,
        b: 0,
    }, // Maroon
    AlertColorEntry {
        keywords: &["fire", "watch"],
        r: 255,
        g: 222,
        b: 173,
    }, // NavajoWhite
    // ── Advisories / Statements ──
    AlertColorEntry {
        keywords: &["wind advisory"],
        r: 210,
        g: 180,
        b: 140,
    }, // Tan
    AlertColorEntry {
        keywords: &["winter weather advisory"],
        r: 123,
        g: 104,
        b: 238,
    }, // MediumSlateBlue
    AlertColorEntry {
        keywords: &["frost advisory"],
        r: 100,
        g: 149,
        b: 237,
    }, // CornflowerBlue
    AlertColorEntry {
        keywords: &["heat advisory"],
        r: 255,
        g: 127,
        b: 80,
    }, // Coral
    AlertColorEntry {
        keywords: &["cold weather advisory"],
        r: 175,
        g: 238,
        b: 238,
    }, // PaleTurquoise
    AlertColorEntry {
        keywords: &["dense fog advisory"],
        r: 112,
        g: 128,
        b: 144,
    }, // SlateGray
    AlertColorEntry {
        keywords: &["flood advisory"],
        r: 0,
        g: 255,
        b: 127,
    }, // SpringGreen
    AlertColorEntry {
        keywords: &["small craft advisory"],
        r: 127,
        g: 255,
        b: 212,
    }, // Aquamarine
    AlertColorEntry {
        keywords: &["high surf advisory"],
        r: 32,
        g: 178,
        b: 170,
    }, // LightSeaGreen
    AlertColorEntry {
        keywords: &["air quality"],
        r: 143,
        g: 188,
        b: 143,
    }, // DarkSeaGreen
    AlertColorEntry {
        keywords: &["rip current"],
        r: 0,
        g: 206,
        b: 209,
    }, // DarkTurquoise
    AlertColorEntry {
        keywords: &["beach hazards"],
        r: 72,
        g: 209,
        b: 204,
    }, // MediumTurquoise
    AlertColorEntry {
        keywords: &["special weather statement"],
        r: 255,
        g: 228,
        b: 181,
    }, // Moccasin
];

/// Names NWS hazard simplification retired: "Excessive Heat Warning/Watch" →
/// "Extreme Heat Warning/Watch"; "Wind Chill Warning/Watch/Advisory" →
/// "Extreme Cold Warning/Watch" and "Cold Weather Advisory". Archives and
/// third-party mirrors still carry them.
///
/// Consulted after `ALERT_COLORS` and before the fallbacks, so a retired name
/// can never shadow a live product. Each must match *no* current event name;
/// see `retired_entries_are_actually_retired`.
static RETIRED_ALERT_COLORS: &[AlertColorEntry] = &[
    AlertColorEntry {
        keywords: &["excessive heat", "warning"],
        r: 199,
        g: 21,
        b: 133,
    }, // = Extreme Heat Warning
    AlertColorEntry {
        keywords: &["excessive heat", "watch"],
        r: 128,
        g: 0,
        b: 0,
    }, // = Extreme Heat Watch
    AlertColorEntry {
        keywords: &["wind chill", "warning"],
        r: 176,
        g: 196,
        b: 222,
    }, // = Extreme Cold Warning
    AlertColorEntry {
        keywords: &["wind chill", "watch"],
        r: 95,
        g: 158,
        b: 160,
    }, // = Extreme Cold Watch
    AlertColorEntry {
        keywords: &["wind chill advisory"],
        r: 175,
        g: 238,
        b: 238,
    }, // = Cold Weather Advisory
];

/// Keyed off the product suffix. Must avoid the reserved tornado colours: an
/// unrecognised warning still reads as severe, but must never be mistakable
/// for a tornado warning. A silent upstream rename lands here.
static FALLBACK_COLORS: &[AlertColorEntry] = &[
    AlertColorEntry {
        keywords: &["warning"],
        r: 178,
        g: 34,
        b: 34,
    }, // Firebrick
    AlertColorEntry {
        keywords: &["watch"],
        r: 240,
        g: 230,
        b: 140,
    }, // Khaki
    AlertColorEntry {
        keywords: &["advisory"],
        r: 255,
        g: 215,
        b: 0,
    }, // Gold
    AlertColorEntry {
        keywords: &["statement"],
        r: 245,
        g: 222,
        b: 179,
    }, // Wheat
];

/// Reserved for "no styling at all"; no table entry may use it.
const UNKNOWN_EVENT: (u8, u8, u8) = (200, 200, 200);

/// Returns `(fill_rgba, stroke_rgba)`. Tries `ALERT_COLORS`, then the retired
/// aliases, then the suffix fallbacks; first all-keyword match wins.
pub fn alert_color(event: &str) -> ([u8; 4], [u8; 4]) {
    let e = event.to_lowercase();
    for entry in ALERT_COLORS
        .iter()
        .chain(RETIRED_ALERT_COLORS)
        .chain(FALLBACK_COLORS)
    {
        if entry.matches(&e) {
            return rgb(entry.r, entry.g, entry.b);
        }
    }
    let (r, g, b) = UNKNOWN_EVENT;
    rgb(r, g, b)
}

fn rgb(r: u8, g: u8, b: u8) -> ([u8; 4], [u8; 4]) {
    ([r, g, b, NWS_FILL_ALPHA], [r, g, b, STROKE_ALPHA])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real event names captured from `api.weather.gov`; provenance and refresh
    /// instructions are in the file's own header.
    const EVENT_TYPES_FIXTURE: &str = include_str!("event_types.txt");

    /// Reserved: nothing else may render as these. Pure red must mean a tornado
    /// is on the ground.
    const TORNADO_WARNING_RED: (u8, u8, u8) = (255, 0, 0);
    const TORNADO_WATCH_YELLOW: (u8, u8, u8) = (255, 255, 0);

    fn sample_events() -> Vec<&'static str> {
        EVENT_TYPES_FIXTURE
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    }

    fn rgb_of(event: &str) -> (u8, u8, u8) {
        let (fill, _) = alert_color(event);
        (fill[0], fill[1], fill[2])
    }

    #[test]
    fn fixture_is_populated() {
        // An empty or mis-parsed fixture makes every "for each real event"
        // assertion below pass vacuously.
        let events = sample_events();
        assert!(
            events.len() > 100,
            "expected the full NWS product list, got {} entries",
            events.len()
        );
        assert!(events.contains(&"Tornado Warning"));
        assert!(events.contains(&"Extreme Heat Warning"));
    }

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

    // ── The bug this module was fixed for ──────────────────────────────────

    #[test]
    fn extreme_heat_is_not_tornado_coloured() {
        // Keyed off the retired "Excessive Heat" name, these fell through to
        // the suffix rows and painted byte-identically to tornado red/yellow.
        assert_eq!(rgb_of("Extreme Heat Warning"), (199, 21, 133));
        assert_eq!(rgb_of("Extreme Heat Watch"), (128, 0, 0));
        assert_ne!(rgb_of("Extreme Heat Warning"), TORNADO_WARNING_RED);
        assert_ne!(rgb_of("Extreme Heat Watch"), TORNADO_WATCH_YELLOW);
    }

    #[test]
    fn extreme_cold_is_not_tornado_coloured() {
        // Same failure mode via the "Wind Chill" rename.
        assert_eq!(rgb_of("Extreme Cold Warning"), (176, 196, 222));
        assert_eq!(rgb_of("Extreme Cold Watch"), (95, 158, 160));
        assert_eq!(rgb_of("Cold Weather Advisory"), (175, 238, 238));
        assert_ne!(rgb_of("Extreme Cold Warning"), TORNADO_WARNING_RED);
        assert_ne!(rgb_of("Extreme Cold Watch"), TORNADO_WATCH_YELLOW);
    }

    #[test]
    fn red_flag_warning_has_its_own_colour() {
        // "Red Flag Warning" contains no specific keyword — not even "fire" —
        // so it used to land on the generic red row.
        assert_eq!(rgb_of("Red Flag Warning"), (210, 105, 30));
        assert_ne!(rgb_of("Red Flag Warning"), TORNADO_WARNING_RED);
        assert_ne!(rgb_of("Red Flag Warning"), rgb_of("Fire Weather Watch"));
    }

    #[test]
    fn retired_names_still_render_like_their_replacements() {
        assert_eq!(
            rgb_of("Excessive Heat Warning"),
            rgb_of("Extreme Heat Warning")
        );
        assert_eq!(rgb_of("Excessive Heat Watch"), rgb_of("Extreme Heat Watch"));
        assert_eq!(rgb_of("Wind Chill Warning"), rgb_of("Extreme Cold Warning"));
        assert_eq!(rgb_of("Wind Chill Watch"), rgb_of("Extreme Cold Watch"));
        assert_eq!(
            rgb_of("Wind Chill Advisory"),
            rgb_of("Cold Weather Advisory")
        );
    }

    // ── Collision guards ───────────────────────────────────────────────────

    #[test]
    fn only_tornado_warning_is_pure_red() {
        let reds: Vec<&str> = sample_events()
            .into_iter()
            .filter(|e| rgb_of(e) == TORNADO_WARNING_RED)
            .collect();
        assert_eq!(
            reds,
            vec!["Tornado Warning"],
            "pure red is reserved for Tornado Warning; these events also claim it"
        );
    }

    #[test]
    fn only_tornado_watch_is_pure_yellow() {
        let yellows: Vec<&str> = sample_events()
            .into_iter()
            .filter(|e| rgb_of(e) == TORNADO_WATCH_YELLOW)
            .collect();
        assert_eq!(
            yellows,
            vec!["Tornado Watch"],
            "pure yellow is reserved for Tornado Watch; these events also claim it"
        );
    }

    #[test]
    fn semantically_distinct_hazards_have_distinct_colours() {
        // One representative per styled hazard/severity class. Same family and
        // tier (Flood Warning vs Coastal Flood Warning) may share a colour.
        const DISTINCT: &[&str] = &[
            "Tornado Warning",
            "Tornado Watch",
            "Severe Thunderstorm Warning",
            "Severe Thunderstorm Watch",
            "Flash Flood Warning",
            "Flash Flood Watch",
            "Flood Warning",
            "Flood Watch",
            "Flood Advisory",
            "Blizzard Warning",
            "Winter Storm Warning",
            "Winter Storm Watch",
            "Winter Weather Advisory",
            "Ice Storm Warning",
            "Extreme Cold Warning",
            "Extreme Cold Watch",
            "Cold Weather Advisory",
            "High Wind Warning",
            "Wind Advisory",
            "Extreme Heat Warning",
            "Extreme Heat Watch",
            "Heat Advisory",
            "Freeze Warning",
            "Frost Advisory",
            "Red Flag Warning",
            "Fire Weather Watch",
            "Fire Warning",
            "Dust Storm Warning",
            "Dense Fog Advisory",
            "Small Craft Advisory",
            "Gale Warning",
            "High Surf Advisory",
            "Rip Current Statement",
            "Beach Hazards Statement",
            "Air Quality Alert",
            "Special Weather Statement",
            // Generic-fallback representatives, one per suffix tier.
            "Avalanche Warning",
            "Avalanche Watch",
            "Ashfall Advisory",
            "Severe Weather Statement",
        ];

        let mut seen: Vec<(&str, (u8, u8, u8))> = Vec::new();
        for event in DISTINCT {
            let c = rgb_of(event);
            if let Some((other, _)) = seen.iter().find(|(_, sc)| *sc == c) {
                panic!("{event:?} and {other:?} are different hazards but share colour {c:?}");
            }
            seen.push((event, c));
        }
    }

    #[test]
    fn nothing_shares_the_unknown_colour() {
        for entry in ALERT_COLORS
            .iter()
            .chain(RETIRED_ALERT_COLORS)
            .chain(FALLBACK_COLORS)
        {
            assert_ne!(
                (entry.r, entry.g, entry.b),
                UNKNOWN_EVENT,
                "entry {:?} uses the unknown-event grey",
                entry.keywords
            );
        }
    }

    // ── Rename detection ───────────────────────────────────────────────────

    #[test]
    fn every_live_entry_matches_a_real_event() {
        // Turns the next upstream rename into a test failure. Otherwise the
        // keywords stop matching, the alerts fall through to a generic colour,
        // nothing panics, and the map quietly starts lying.
        let events = sample_events();
        for entry in ALERT_COLORS.iter().chain(FALLBACK_COLORS) {
            let matched = events.iter().any(|e| entry.matches(&e.to_lowercase()));
            assert!(
                matched,
                "ALERT_COLORS entry {:?} matches no current NWS event name. \
                 The product was probably renamed upstream; alerts that used to \
                 hit this row are now falling through to a generic colour. \
                 Update the keywords and move the old name into \
                 RETIRED_ALERT_COLORS.",
                entry.keywords
            );
        }
    }

    #[test]
    fn every_live_entry_is_reachable() {
        // A row can also die by being fully shadowed by an earlier row, which
        // the match-a-real-event check above does not catch.
        let events = sample_events();
        for (i, entry) in ALERT_COLORS.iter().enumerate() {
            let wins = events.iter().any(|e| {
                let lower = e.to_lowercase();
                ALERT_COLORS.iter().position(|c| c.matches(&lower)) == Some(i)
            });
            assert!(
                wins,
                "ALERT_COLORS entry {:?} is shadowed by an earlier entry and can never win",
                entry.keywords
            );
        }
    }

    #[test]
    fn retired_entries_are_actually_retired() {
        // A retired alias that starts matching a live name can shadow a real
        // entry, and means the product was un-retired.
        let events = sample_events();
        for entry in RETIRED_ALERT_COLORS {
            let live: Vec<&&str> = events
                .iter()
                .filter(|e| entry.matches(&e.to_lowercase()))
                .collect();
            assert!(
                live.is_empty(),
                "RETIRED_ALERT_COLORS entry {:?} matches live event(s) {:?}; \
                 move it back into ALERT_COLORS",
                entry.keywords,
                live
            );
        }
    }

    #[test]
    fn no_real_event_is_left_unstyled_by_severity() {
        // Severity must stay legible even for products styled only by suffix.
        for event in sample_events() {
            let lower = event.to_lowercase();
            let has_suffix = ["warning", "watch", "advisory", "statement"]
                .iter()
                .any(|s| lower.contains(s));
            if has_suffix {
                assert_ne!(
                    rgb_of(event),
                    UNKNOWN_EVENT,
                    "{event:?} carries a severity suffix but renders as unknown grey"
                );
            }
        }
    }
}
