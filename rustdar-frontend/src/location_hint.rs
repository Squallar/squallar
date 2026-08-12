//! A coarse guess at where the user is, costing no permission prompt.
//!
//! The problem this solves is the first paint. Opening every user on one
//! hardcoded site means most of them are looking at weather a thousand miles
//! away, and the fix that would be exact — the Geolocation API — cannot run
//! without a modal permission prompt that most people decline and that nobody
//! wants before they have seen the app do anything.
//!
//! The IANA timezone is the useful middle. Every platform already knows it, it
//! needs no permission and no network, it is available synchronously before the
//! first frame, and it leaves the machine not at all. What it buys is a region,
//! not a position: `America/Chicago` is one zone from the Gulf coast to the
//! Canadian border. That is imprecise and it is still enormously better than
//! defaulting a viewer in Minnesota to Oklahoma.
//!
//! So the hint is a *starting* site, never an override. Precedence is settled in
//! the caller: a stored configuration always wins, this fills in only when there
//! is nothing stored, and a real [`GpsFix`](rustdar_gps::GpsFix) — which arrives
//! only if the user has already granted location — supersedes it.

/// A representative coordinate for an IANA timezone, and how coarse it is.
struct ZoneAnchor {
    /// The IANA zone name, exactly as `Intl.DateTimeFormat` and the platform
    /// timezone crates report it.
    zone: &'static str,
    lat: f64,
    lon: f64,
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
static ZONE_ANCHORS: &[ZoneAnchor] = &[
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

/// The radar site to open on for a device reporting IANA timezone `zone`.
///
/// `None` when the zone is unmapped, which leaves the caller on its compiled-in
/// default rather than on a site derived from a guess.
pub fn site_for_timezone(zone: &str) -> Option<&'static str> {
    let (lat, lon) = coordinate_for_timezone(zone)?;
    rustdar_radar::sites::nearest_wsr88d_site(lat, lon).map(|(site, _)| site.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The radars these tests resolve against — [`crate::test_sites`], which
    /// is the crate's one fixture network.
    ///
    /// # What the compiled-in table used to do here, and what replaced it
    ///
    /// Every assertion in this module used to run against the whole network,
    /// because the whole network was a `const` in `rustdar-radar`. That let
    /// four tests check the *anchors* rather than the mechanism: that every
    /// zone resolves to something, that none is more than 400 km from a radar,
    /// that the territories reach their own island's radar, and that no anchor
    /// silently lands on the old hardcoded `KTLX`.
    ///
    /// A binary that carries no radars cannot check any of that hermetically,
    /// and a fixture pretending to is worse than not checking: with fifteen
    /// radars in the table, "every anchor is within 400 km of one" is false for
    /// most of them and true for no interesting reason. So that work moved to
    /// [`the_live_anchor_survey`], which fetches the real network and checks
    /// all four properties at once, and what stays here is the mechanism: an
    /// anchor resolves to the nearest **operational WSR-88D**, and anchors far
    /// apart resolve to different radars.
    use crate::test_sites::install as install_radars;

    /// The whole point of the feature, stated as a test: viewers in different
    /// parts of the country get different radars, each the nearest to them.
    ///
    /// The expected identifiers are the fixture's, and each one is the nearest
    /// fixture radar to that zone's anchor by a wide margin — so a wrong
    /// anchor, a transposed coordinate or a broken nearest-search all still
    /// fail here. What it no longer proves is that the anchor is right for the
    /// *real* network; that is [`the_live_anchor_survey`].
    #[test]
    fn distinct_regions_get_distinct_sites() {
        install_radars();
        assert_eq!(site_for_timezone("America/Los_Angeles"), Some("KSOX"));
        assert_eq!(site_for_timezone("America/New_York"), Some("KDIX"));
        assert_eq!(site_for_timezone("America/Denver"), Some("KFTG"));
        assert_eq!(site_for_timezone("America/Chicago"), Some("KLOT"));
    }

    /// The territories are the case a CONUS-shaped assumption breaks: each
    /// must resolve to its own island's radar, not to the mainland.
    ///
    /// They are the one group a small fixture still tests honestly, because
    /// the property is that an island is nearer to itself than to a continent
    /// — and that holds whether the fixture has eleven radars or two hundred.
    #[test]
    fn territories_resolve_to_their_own_radars() {
        install_radars();
        assert_eq!(site_for_timezone("Pacific/Honolulu"), Some("PHMO"));
        assert_eq!(site_for_timezone("America/Puerto_Rico"), Some("TJUA"));
    }

    /// An anchor resolves past a TDWR and past the ROC test bed.
    ///
    /// The two filters `nearest_wsr88d_site` applies, reached through the
    /// route that actually uses them. Oklahoma City is the case that
    /// exercises both at once: the literal nearest radar is the TDWR `TOKC`,
    /// which has no Level II data, and the nearest WSR-88D is `KCRI`, which
    /// scans to whatever the ROC is testing that day.
    ///
    /// There is no anchor at Oklahoma City — that is
    /// `no_covered_zone_silently_resolves_to_the_old_default`'s business — so
    /// this asks the coordinate directly.
    #[test]
    fn an_anchor_skips_the_tdwr_and_the_test_bed() {
        install_radars();
        let (site, _) = rustdar_radar::sites::nearest_wsr88d_site(35.4676, -97.5164)
            .expect("the fixture places radars here");
        assert_eq!(site.name, "KTLX");
    }

    /// Nothing may resolve to the old hardcoded default by accident. KTLX is a
    /// legitimate answer only for a zone anchored near Oklahoma, and no anchor
    /// is.
    ///
    /// Weaker than it was — with eleven radars in the table, plenty of anchors
    /// are nearest to `KTLX` simply because nothing else is close. So it is
    /// asked of the four anchors the fixture genuinely covers, and asked of
    /// the whole set by [`the_live_anchor_survey`].
    #[test]
    fn no_covered_zone_silently_resolves_to_the_old_default() {
        install_radars();
        for zone in [
            "America/Los_Angeles",
            "America/New_York",
            "America/Denver",
            "America/Chicago",
        ] {
            assert_ne!(
                site_for_timezone(zone),
                Some("KTLX"),
                "{zone} resolved to the hardcoded default, which would hide a \
                 broken anchor behind the old behaviour",
            );
        }
    }

    /// With no radars at all there is no hint, and no guess in place of one.
    ///
    /// The state a fresh install launches in, and the reason
    /// `App::poll_site_catalogue` runs the hint again when the first catalogue
    /// lands: `site_for_timezone` cannot answer here, and inventing an answer
    /// would open the app on a radar it has no position for.
    ///
    /// Asserted against a table this test builds rather than the process one,
    /// which a sibling has certainly populated by now.
    #[test]
    fn a_process_with_no_radars_offers_no_hint() {
        let empty = rustdar_radar::sites::build_table(std::iter::empty());
        let (lat, lon) = coordinate_for_timezone("America/Chicago").expect("an anchored zone");
        assert!(
            empty.nearest_wsr88d(lat, lon).is_none(),
            "an anchor cannot resolve against a table with nothing in it",
        );
    }

    /// An unmapped zone must not be approximated into a confident wrong answer.
    #[test]
    fn an_unmapped_zone_yields_no_hint() {
        install_radars();
        assert_eq!(site_for_timezone("Europe/Warsaw"), None);
        assert_eq!(site_for_timezone("Africa/Cairo"), None);
        assert_eq!(coordinate_for_timezone("Antarctica/Davis"), None);
    }

    /// Empty and malformed input reach here from a browser that returns
    /// something unexpected, and must be ordinary misses rather than panics.
    #[test]
    fn junk_input_is_an_ordinary_miss() {
        install_radars();
        assert_eq!(site_for_timezone(""), None);
        assert_eq!(site_for_timezone("not/a/zone"), None);
        assert_eq!(site_for_timezone("america/chicago"), None);
    }

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

    /// **Every anchor, against the real network.**
    ///
    /// The four properties the compiled-in table used to check on every CI row,
    /// gathered into the one test that can still check them: each anchor
    /// resolves to a radar, none is more than 400 km from it, none lands on the
    /// old hardcoded `KTLX`, and the four named regions reach the radars they
    /// are meant to.
    ///
    /// `#[ignore]`d because CI is hermetic, which is the same reason
    /// `rustdar_radar::catalogue`'s live tests are. Run it when an anchor is
    /// added or moved:
    ///
    /// ```text
    /// cargo test -p rustdar-frontend --all-features -- --ignored the_live_anchor_survey
    /// ```
    ///
    /// 400 km is deliberately looser than a WSR-88D's ~230 km reflectivity
    /// range. Sparse regions legitimately exceed it — the Alaskan panhandle and
    /// the prairie provinces are the closest calls, at ~310–370 km — and there
    /// the nearest radar is still the right one to open on. The threshold
    /// admits those and excludes the 700 km-plus cases that are genuinely
    /// outside the network.
    ///
    /// Native-only, because the runtime it blocks on is: `tokio` is a
    /// `cfg(not(target_arch = "wasm32"))` dependency of this crate, since
    /// wasm rejects the multi-threaded runtime. The wasm rows build
    /// `--all-targets`, so without this gate the crate would name a
    /// dependency it does not have there.
    #[test]
    #[ignore = "fetches the live radar catalogue"]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_live_anchor_survey() {
        let sources = rustdar_radar::sources::DataSources::production();
        let catalogue = tokio::runtime::Runtime::new()
            .expect("a runtime")
            .block_on(rustdar_radar::catalogue::fetch(&sources))
            .expect("the live catalogue");
        let table = rustdar_radar::sites::build_table(catalogue.fixes());
        assert!(
            table.rows().len() > 150,
            "only {} radars placed; the survey needs the real network",
            table.rows().len(),
        );

        for anchor in ZONE_ANCHORS {
            let (site, dist) = table
                .nearest_wsr88d(anchor.lat, anchor.lon)
                .expect("anchor coordinates are finite");
            assert!(
                dist < 400.0,
                "{} is {dist:.0} km from its nearest site ({}) — either the \
                 anchor coordinate is wrong or the zone is outside NEXRAD \
                 coverage and should not be anchored at all",
                anchor.zone,
                site.name,
            );
            assert_ne!(
                site.name, "KTLX",
                "{} resolved to the old hardcoded default, which would hide a \
                 broken anchor behind the old behaviour",
                anchor.zone,
            );
        }

        for (zone, want) in [
            ("America/Los_Angeles", "KSOX"),
            ("America/New_York", "KDIX"),
            ("America/Denver", "KFTG"),
            ("America/Chicago", "KLOT"),
            ("Pacific/Honolulu", "PHMO"),
            ("Pacific/Guam", "PGUA"),
            ("America/Puerto_Rico", "TJUA"),
            ("America/Anchorage", "PAHG"),
        ] {
            let (lat, lon) = coordinate_for_timezone(zone).expect("an anchored zone");
            let (site, _) = table.nearest_wsr88d(lat, lon).expect("finite");
            assert_eq!(site.name, want, "{zone}");
        }
    }
}
