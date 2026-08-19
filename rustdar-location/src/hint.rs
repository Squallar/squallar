//! The timezone → coordinate anchor table: the dependency-free half of the
//! app's coarse "where is the user" hint.
//!
//! What a timezone buys is a region, not a position — see the app-side
//! `location_hint` module for the feature's argument (starting site only,
//! never an override). Only the table and its lookup live HERE: this crate
//! resolves no radar site, because the domain vocabulary must not depend on
//! any radar catalogue.

/// A representative coordinate for an IANA timezone, and how coarse it is.
pub struct ZoneAnchor {
    /// The IANA zone name, exactly as `Intl.DateTimeFormat` and the platform
    /// timezone crates report it.
    pub zone: &'static str,
    pub lat: f64,
    pub lon: f64,
}

/// Representative coordinates for the timezones that overlap NEXRAD coverage.
///
/// Each anchor is the population centre of its zone rather than its geometric
/// centre: the goal is to be right for the most people, not to be equidistant
/// from the zone's corners. For `America/Chicago` that means Chicago itself,
/// which is wrong for Texas — but Texas is `America/Chicago` too and there is no
/// single point that serves both. Zones with their own entry (the Indiana and
/// North Dakota families, `America/Detroit`, `America/Menominee`) are exactly
/// the places where that split matters enough that IANA already made it.
///
/// Non-US zones near the border are included because NEXRAD reaches across it
/// and those users are otherwise sent to Oklahoma.
///
/// A zone earns an anchor only if it lands within reach of a radar. Mexico City,
/// Edmonton, Adak and Tokyo were all tried and removed: their nearest WSR-88D is
/// 700–1300 km away, far enough that the "hint" would be a different flavour of
/// wrong answer rather than a better one. They fall through to `None` and the
/// caller's default, which is the honest result for a place NEXRAD does not
/// cover. `the_live_anchor_survey` holds the line, and holds it against the
/// real catalogue rather than a fixture — which is why it is `#[ignore]`d.
/// `every_anchor_is_a_real_coordinate` is what runs on every row.
///
/// Public for the app crate's `#[ignore]` live survey, which audits every
/// anchor against the real radar network.
pub static ZONE_ANCHORS: &[ZoneAnchor] = &[
    // Eastern
    ZoneAnchor {
        zone: "America/New_York",
        lat: 40.7128,
        lon: -74.0060,
    },
    ZoneAnchor {
        zone: "America/Detroit",
        lat: 42.3314,
        lon: -83.0458,
    },
    ZoneAnchor {
        zone: "America/Kentucky/Louisville",
        lat: 38.2527,
        lon: -85.7585,
    },
    ZoneAnchor {
        zone: "America/Kentucky/Monticello",
        lat: 36.8298,
        lon: -84.8491,
    },
    ZoneAnchor {
        zone: "America/Indiana/Indianapolis",
        lat: 39.7684,
        lon: -86.1581,
    },
    ZoneAnchor {
        zone: "America/Indiana/Vincennes",
        lat: 38.6773,
        lon: -87.5286,
    },
    ZoneAnchor {
        zone: "America/Indiana/Winamac",
        lat: 41.0514,
        lon: -86.6033,
    },
    ZoneAnchor {
        zone: "America/Indiana/Marengo",
        lat: 38.3695,
        lon: -86.3439,
    },
    ZoneAnchor {
        zone: "America/Indiana/Petersburg",
        lat: 38.4917,
        lon: -87.2786,
    },
    ZoneAnchor {
        zone: "America/Indiana/Vevay",
        lat: 38.7478,
        lon: -85.0672,
    },
    ZoneAnchor {
        zone: "America/Indiana/Tell_City",
        lat: 37.9515,
        lon: -86.7678,
    },
    ZoneAnchor {
        zone: "America/Indiana/Knox",
        lat: 41.2959,
        lon: -86.6250,
    },
    ZoneAnchor {
        zone: "America/Toronto",
        lat: 43.6532,
        lon: -79.3832,
    },
    ZoneAnchor {
        zone: "America/Montreal",
        lat: 45.5019,
        lon: -73.5674,
    },
    ZoneAnchor {
        zone: "America/Nassau",
        lat: 25.0443,
        lon: -77.3504,
    },
    // Central
    ZoneAnchor {
        zone: "America/Chicago",
        lat: 41.8781,
        lon: -87.6298,
    },
    ZoneAnchor {
        zone: "America/Menominee",
        lat: 45.1078,
        lon: -87.6142,
    },
    ZoneAnchor {
        zone: "America/North_Dakota/Center",
        lat: 47.1164,
        lon: -101.2996,
    },
    ZoneAnchor {
        zone: "America/North_Dakota/New_Salem",
        lat: 46.8450,
        lon: -101.4118,
    },
    ZoneAnchor {
        zone: "America/North_Dakota/Beulah",
        lat: 47.2636,
        lon: -101.7779,
    },
    ZoneAnchor {
        zone: "America/Winnipeg",
        lat: 49.8951,
        lon: -97.1384,
    },
    ZoneAnchor {
        zone: "America/Regina",
        lat: 50.4452,
        lon: -104.6189,
    },
    ZoneAnchor {
        zone: "America/Monterrey",
        lat: 25.6866,
        lon: -100.3161,
    },
    // Mountain
    ZoneAnchor {
        zone: "America/Denver",
        lat: 39.7392,
        lon: -104.9903,
    },
    ZoneAnchor {
        zone: "America/Boise",
        lat: 43.6150,
        lon: -116.2023,
    },
    ZoneAnchor {
        zone: "America/Phoenix",
        lat: 33.4484,
        lon: -112.0740,
    },
    ZoneAnchor {
        zone: "America/Chihuahua",
        lat: 28.6330,
        lon: -106.0691,
    },
    // Pacific
    ZoneAnchor {
        zone: "America/Los_Angeles",
        lat: 34.0522,
        lon: -118.2437,
    },
    ZoneAnchor {
        zone: "America/Vancouver",
        lat: 49.2827,
        lon: -123.1207,
    },
    ZoneAnchor {
        zone: "America/Tijuana",
        lat: 32.5149,
        lon: -117.0382,
    },
    // Alaska, Hawaii and the territories, each of which has its own radars and
    // would otherwise be sent thousands of miles to the mainland.
    ZoneAnchor {
        zone: "America/Anchorage",
        lat: 61.2181,
        lon: -149.9003,
    },
    ZoneAnchor {
        zone: "America/Juneau",
        lat: 58.3019,
        lon: -134.4197,
    },
    ZoneAnchor {
        zone: "America/Sitka",
        lat: 57.0531,
        lon: -135.3300,
    },
    ZoneAnchor {
        zone: "America/Yakutat",
        lat: 59.5469,
        lon: -139.7272,
    },
    ZoneAnchor {
        zone: "America/Nome",
        lat: 64.5011,
        lon: -165.4064,
    },
    ZoneAnchor {
        zone: "America/Metlakatla",
        lat: 55.1292,
        lon: -131.5758,
    },
    ZoneAnchor {
        zone: "Pacific/Honolulu",
        lat: 21.3069,
        lon: -157.8583,
    },
    ZoneAnchor {
        zone: "Pacific/Guam",
        lat: 13.4443,
        lon: 144.7937,
    },
    ZoneAnchor {
        zone: "Pacific/Saipan",
        lat: 15.1770,
        lon: 145.7500,
    },
    ZoneAnchor {
        zone: "America/Puerto_Rico",
        lat: 18.4655,
        lon: -66.1057,
    },
    ZoneAnchor {
        zone: "America/St_Thomas",
        lat: 18.3419,
        lon: -64.9307,
    },
    ZoneAnchor {
        zone: "America/Virgin",
        lat: 18.3419,
        lon: -64.9307,
    },
    // Overseas WSR-88D sites. Small populations, but the alternative for them is
    // a radar on the other side of the planet.
    ZoneAnchor {
        zone: "Atlantic/Azores",
        lat: 38.7333,
        lon: -27.0833,
    },
    ZoneAnchor {
        zone: "Asia/Seoul",
        lat: 37.5665,
        lon: 126.9780,
    },
];

/// A representative coordinate for `zone`, or `None` if it is not one we map.
///
/// Unknown zones are deliberately not approximated. Guessing a coordinate from
/// a zone's UTC offset alone recovers a longitude and nothing about latitude,
/// which across `America/Chicago`'s span is the difference between Texas and
/// Manitoba — a confident wrong answer where `None` lets the caller keep a
/// default it can explain.
pub fn coordinate_for_timezone(zone: &str) -> Option<(f64, f64)> {
    ZONE_ANCHORS
        .iter()
        .find(|anchor| anchor.zone == zone)
        .map(|anchor| (anchor.lat, anchor.lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A duplicated zone name means the second entry is unreachable, which is
    /// invisible at runtime and exactly the kind of edit a table this shape
    /// invites.
    #[test]
    fn zone_names_are_unique() {
        let mut names: Vec<&str> = ZONE_ANCHORS.iter().map(|a| a.zone).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "a zone is listed twice");
    }

    /// Every anchor's coordinates are on the planet and finite.
    ///
    /// What is left of `anchors_sit_within_plausible_radar_range` that can be
    /// checked without the network: a transposed sign or a dropped digit shows
    /// up here, and whether the result is *near a radar* is the live survey's
    /// question.
    #[test]
    fn every_anchor_is_a_real_coordinate() {
        for anchor in ZONE_ANCHORS {
            assert!(
                anchor.lat.is_finite() && (-90.0..=90.0).contains(&anchor.lat),
                "{}: latitude {}",
                anchor.zone,
                anchor.lat,
            );
            assert!(
                anchor.lon.is_finite() && (-180.0..=180.0).contains(&anchor.lon),
                "{}: longitude {}",
                anchor.zone,
                anchor.lon,
            );
        }
    }

    /// The zones that were tried and rejected as out of coverage. Re-adding one
    /// without noticing would quietly hand those users a radar most of a
    /// continent away.
    ///
    /// Table-independent: these zones are not anchored at all, so the lookup
    /// misses before any radar is consulted.
    #[test]
    fn zones_outside_nexrad_coverage_stay_unanchored() {
        for zone in [
            "America/Mexico_City",
            "America/Edmonton",
            "America/Adak",
            "Asia/Tokyo",
        ] {
            assert_eq!(coordinate_for_timezone(zone), None, "{zone}");
        }
    }
}
