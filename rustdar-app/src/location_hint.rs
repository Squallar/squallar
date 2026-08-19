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
//! is nothing stored, and a real [`Fix`](rustdar_location::Fix) — which arrives
//! only if the user has already granted location — supersedes it.

use rustdar_location::coordinate_for_timezone;

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
