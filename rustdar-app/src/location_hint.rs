//! A coarse guess at where the user is, costing no permission prompt.

use rustdar_location::coordinate_for_timezone;

/// The radar site to open on for a device reporting IANA timezone `zone`.
pub fn site_for_timezone(zone: &str) -> Option<&'static str> {
    let (lat, lon) = coordinate_for_timezone(zone)?;
    rustdar_radar::sites::nearest_wsr88d_site(lat, lon).map(|(site, _)| site.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The radars these tests resolve against — [`crate::test_sites`], which is the crate's
    /// one fixture network.
    use crate::test_sites::install as install_radars;

    /// The whole point of the feature, stated as a test: viewers in different parts of the
    /// country get different radars, each the nearest to them.
    #[test]
    fn distinct_regions_get_distinct_sites() {
        install_radars();
        assert_eq!(site_for_timezone("America/Los_Angeles"), Some("KSOX"));
        assert_eq!(site_for_timezone("America/New_York"), Some("KDIX"));
        assert_eq!(site_for_timezone("America/Denver"), Some("KFTG"));
        assert_eq!(site_for_timezone("America/Chicago"), Some("KLOT"));
    }

    /// The territories are the case a CONUS-shaped assumption breaks: each must resolve to
    /// its own island's radar, not to the mainland.
    #[test]
    fn territories_resolve_to_their_own_radars() {
        install_radars();
        assert_eq!(site_for_timezone("Pacific/Honolulu"), Some("PHMO"));
        assert_eq!(site_for_timezone("America/Puerto_Rico"), Some("TJUA"));
    }

    /// An anchor resolves past a TDWR and past the ROC test bed.
    #[test]
    fn an_anchor_skips_the_tdwr_and_the_test_bed() {
        install_radars();
        let (site, _) = rustdar_radar::sites::nearest_wsr88d_site(35.4676, -97.5164)
            .expect("the fixture places radars here");
        assert_eq!(site.name, "KTLX");
    }

    /// Nothing may resolve to the old hardcoded default by accident.
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

    /// Empty and malformed input reach here from a browser that returns something
    /// unexpected, and must be ordinary misses rather than panics.
    #[test]
    fn junk_input_is_an_ordinary_miss() {
        install_radars();
        assert_eq!(site_for_timezone(""), None);
        assert_eq!(site_for_timezone("not/a/zone"), None);
        assert_eq!(site_for_timezone("america/chicago"), None);
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
    /// cargo test -p rustdar-app --all-features -- --ignored the_live_anchor_survey
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

        for anchor in rustdar_location::ZONE_ANCHORS {
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
