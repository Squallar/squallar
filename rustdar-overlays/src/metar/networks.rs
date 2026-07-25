//! Which IEM ASOS networks a viewport needs.
//!
//! The Iowa Environmental Mesonet serves current observations one *network* at
//! a time, and its ASOS networks are per-state: `OK_ASOS`, `TX_ASOS`, and so
//! on. A state's worth of observations is ~72 KB of JSON, so a viewport is
//! served by fetching the handful of states it overlaps.
//!
//! The alternative — `?networkclass=ASOS`, one request for everything — is
//! **54 MB and served ungzipped**. It is not an option, and because it returns
//! perfectly valid JSON nothing downstream would notice if someone switched to
//! it; [`crate::metar::fetch`]'s URL test is the guard.
//!
//! # Where these numbers come from
//!
//! Every bound below is decoded from the `extent` column of IEM's own
//! `https://mesonet.agron.iastate.edu/api/1/networks.json`, which gives each
//! network a PostGIS polygon in EPSG:4326. They are the extents of the
//! *stations in the network*, not political borders, so they are exactly the
//! right thing to test a viewport against — and they are IEM's numbers rather
//! than a hand-copied gazetteer.
//!
//! `networks_table_matches_iems_own_extents` re-fetches that endpoint and
//! checks this table against it, so drift shows up as a test failure rather
//! than as a state quietly dropping out of the map.

use crate::types::GeoBounds;

/// One IEM ASOS network and the extent of its stations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StateNetwork {
    /// Two-letter postal code; the network id is `{state}_ASOS`.
    pub state: &'static str,
    /// Southernmost station latitude.
    pub min_lat: f64,
    /// Northernmost station latitude.
    pub max_lat: f64,
    /// Westernmost station longitude.
    pub min_lon: f64,
    /// Easternmost station longitude.
    pub max_lon: f64,
}

impl StateNetwork {
    /// Whether this network's stations could fall inside `view`.
    ///
    /// A plain bounding-box overlap. Deliberately inclusive: a false positive
    /// costs one 72 KB request, a false negative silently drops every station
    /// in a state.
    pub fn intersects(&self, view: &GeoBounds) -> bool {
        self.min_lat <= view.max_lat
            && self.max_lat >= view.min_lat
            && self.min_lon <= view.max_lon
            && self.max_lon >= view.min_lon
    }

    /// Great-circle-ish distance from the network's centre to a point, in
    /// degrees. Used only to rank networks, never to decide membership.
    fn centre_distance(&self, lat: f64, lon: f64) -> f64 {
        let clat = (self.min_lat + self.max_lat) / 2.0;
        let clon = (self.min_lon + self.max_lon) / 2.0;
        ((clat - lat).powi(2) + (clon - lon).powi(2)).sqrt()
    }
}

/// Most networks to fetch for one viewport.
///
/// A fully zoomed-out map overlaps every entry in [`NETWORKS`], which would be
/// 54 requests and ~3.9 MB. At that zoom the station plot is unreadable
/// anyway, so the nearest [`MAX_NETWORKS`] to the viewport centre are taken and
/// the rest dropped. This is a *transfer* cap, not a correctness rule — see
/// [`networks_for_viewport`].
pub const MAX_NETWORKS: usize = 12;

/// The viewport to assume when none is known yet.
///
/// The first overlay fetch can be issued before any frame has been rendered,
/// so there is no map extent to scope by. Rather than fetch nothing — which
/// renders as "no observations" and looks like an outage — this stands in for
/// "somewhere in the United States" and lets [`MAX_NETWORKS`] pick the states
/// nearest its centre. It is a fallback, not a default the app should rely on:
/// once a frame has been drawn the real viewport is used.
pub const DEFAULT_VIEWPORT: GeoBounds = GeoBounds {
    min_lat: 30.0,
    max_lat: 45.0,
    min_lon: -104.0,
    max_lon: -85.0,
};

/// The state ASOS networks IEM publishes, with the extents it publishes them
/// with.
///
/// Decoded from `networks.json` on 2026-07-25. 54 entries: the 50 states plus
/// American Samoa, Guam, Puerto Rico and the US Virgin Islands. There is no
/// `DC_ASOS`; the District's stations sit in `VA_ASOS` and `MD_ASOS`.
///
/// Two entries are wider than their territory: `AK` (-176.75..174.22) and `GU`
/// (144.70..166.74) both span the antimeridian, so IEM's axis-aligned extent
/// for them is enormous. They are stored as published rather than "corrected",
/// because the consequence is only an occasional extra request, and inventing
/// tighter numbers here would be exactly the hand-transcription this table
/// exists to avoid.
pub const NETWORKS: &[StateNetwork] = &[
    StateNetwork { state: "AK", min_lat: 51.7780, max_lat: 71.3826, min_lon: -176.7460, max_lon: 174.2169 },
    StateNetwork { state: "AL", min_lat: 28.9500, max_lat: 34.9600, min_lon: -88.3456, max_lon: -85.0289 },
    StateNetwork { state: "AR", min_lat: 33.1210, max_lat: 36.5042, min_lon: -94.5900, max_lon: -89.7300 },
    StateNetwork { state: "AS", min_lat: -14.4310, max_lat: -14.2310, min_lon: -170.8105, max_lon: -170.6105 },
    StateNetwork { state: "AZ", min_lat: 31.3208, max_lat: 37.0599, min_lon: -114.7060, max_lon: -108.9614 },
    StateNetwork { state: "CA", min_lat: 32.4631, max_lat: 41.8837, min_lon: -124.3380, max_lon: -114.5233 },
    StateNetwork { state: "CO", min_lat: 37.0515, max_lat: 40.8503, min_lon: -108.8593, max_lon: -102.1410 },
    StateNetwork { state: "CT", min_lat: 41.0583, max_lat: 42.0381, min_lon: -73.5800, max_lon: -71.9500 },
    StateNetwork { state: "DE", min_lat: 38.5892, max_lat: 39.7728, min_lon: -75.7008, max_lon: -75.2589 },
    StateNetwork { state: "FL", min_lat: 24.4561, max_lat: 30.9458, min_lon: -87.4180, max_lon: -79.9848 },
    StateNetwork { state: "GA", min_lat: 30.6825, max_lat: 34.9544, min_lon: -85.3903, max_lon: -81.0460 },
    StateNetwork { state: "GU", min_lat: 13.3839, max_lat: 19.3800, min_lon: 144.6972, max_lon: 166.7419 },
    StateNetwork { state: "HI", min_lat: 19.6203, max_lat: 28.3082, min_lon: -177.4756, max_lon: -154.9485 },
    StateNetwork { state: "IA", min_lat: 40.3615, max_lat: 43.5008, min_lon: -96.4795, max_lon: -90.2328 },
    StateNetwork { state: "ID", min_lat: 42.0069, max_lat: 48.8260, min_lon: -117.1154, max_lon: -110.9979 },
    StateNetwork { state: "IL", min_lat: 36.9647, max_lat: 42.5222, min_lon: -91.2946, max_lon: -87.4295 },
    StateNetwork { state: "IN", min_lat: 37.9441, max_lat: 41.8200, min_lon: -87.6205, max_lon: -84.7428 },
    StateNetwork { state: "KS", min_lat: 36.9008, max_lat: 40.0042, min_lon: -101.9800, max_lon: -94.6311 },
    StateNetwork { state: "KY", min_lat: 36.5106, max_lat: 39.1431, min_lon: -88.8744, max_lon: -82.4674 },
    StateNetwork { state: "LA", min_lat: 26.1367, max_lat: 32.8561, min_lon: -95.1641, max_lon: -87.6810 },
    StateNetwork { state: "MA", min_lat: 41.1531, max_lat: 42.8172, min_lon: -73.3892, max_lon: -69.8933 },
    StateNetwork { state: "MD", min_lat: 38.0460, max_lat: 39.8078, min_lon: -79.4394, max_lon: -75.0239 },
    StateNetwork { state: "ME", min_lat: 43.2939, max_lat: 47.3855, min_lon: -71.0479, max_lon: -66.9127 },
    StateNetwork { state: "MI", min_lat: 41.6358, max_lat: 47.5669, min_lon: -90.2314, max_lon: -82.4289 },
    StateNetwork { state: "MN", min_lat: 43.5212, max_lat: 49.4183, min_lon: -97.0430, max_lon: -90.2457 },
    StateNetwork { state: "MO", min_lat: 36.1259, max_lat: 40.4525, min_lon: -95.0150, max_lon: -89.4577 },
    StateNetwork { state: "MS", min_lat: 28.1206, max_lat: 35.0787, min_lon: -91.3973, max_lon: -88.0659 },
    StateNetwork { state: "MT", min_lat: 44.5500, max_lat: 49.0738, min_lon: -115.5902, max_lon: -104.0926 },
    StateNetwork { state: "NC", min_lat: 33.8292, max_lat: 36.5600, min_lon: -83.9630, max_lon: -75.5225 },
    StateNetwork { state: "ND", min_lat: 45.9149, max_lat: 49.0406, min_lon: -104.0821, max_lon: -96.5074 },
    StateNetwork { state: "NE", min_lat: 39.9788, max_lat: 42.9567, min_lon: -104.0950, max_lon: -95.4920 },
    StateNetwork { state: "NH", min_lat: 42.6818, max_lat: 44.6761, min_lon: -72.4042, max_lon: -70.7233 },
    StateNetwork { state: "NJ", min_lat: 38.9085, max_lat: 41.3002, min_lon: -75.1783, max_lon: -73.9562 },
    StateNetwork { state: "NM", min_lat: 31.7804, max_lat: 37.0000, min_lon: -109.0300, max_lon: -102.9793 },
    StateNetwork { state: "NV", min_lat: 35.8475, max_lat: 42.0532, min_lon: -119.9764, max_lon: -114.4264 },
    StateNetwork { state: "NY", min_lat: 40.5386, max_lat: 45.0334, min_lon: -79.3720, max_lon: -71.8233 },
    StateNetwork { state: "OH", min_lat: 38.7405, max_lat: 41.8780, min_lon: -84.8844, max_lon: -80.5739 },
    StateNetwork { state: "OK", min_lat: 33.8094, max_lat: 37.0092, min_lon: -101.6053, max_lon: -94.5200 },
    StateNetwork { state: "OR", min_lat: 41.9500, max_lat: 46.2569, min_lon: -124.5249, max_lon: -116.9128 },
    StateNetwork { state: "PA", min_lat: 39.6290, max_lat: 42.1800, min_lon: -80.5134, max_lon: -74.9134 },
    StateNetwork { state: "PR", min_lat: 17.9083, max_lat: 18.5949, min_lon: -67.2485, max_lon: -65.5386 },
    StateNetwork { state: "RI", min_lat: 41.0700, max_lat: 42.0208, min_lon: -71.8989, max_lon: -71.1815 },
    StateNetwork { state: "SC", min_lat: 32.1244, max_lat: 35.0878, min_lon: -82.9868, max_lon: -78.6239 },
    StateNetwork { state: "SD", min_lat: 42.6653, max_lat: 46.0187, min_lon: -103.9620, max_lon: -96.4660 },
    StateNetwork { state: "TN", min_lat: 34.9353, max_lat: 36.7219, min_lon: -90.1540, max_lon: -82.0734 },
    StateNetwork { state: "TX", min_lat: 25.8146, max_lat: 36.5140, min_lon: -106.4800, max_lon: -91.9333 },
    StateNetwork { state: "UT", min_lat: 36.9111, max_lat: 41.8913, min_lon: -114.1309, max_lon: -109.2412 },
    StateNetwork { state: "VA", min_lat: 36.4729, max_lat: 39.2435, min_lon: -83.3178, max_lon: -75.3631 },
    StateNetwork { state: "VI", min_lat: 17.6000, max_lat: 18.4373, min_lon: -65.0734, max_lon: -64.7047 },
    StateNetwork { state: "VT", min_lat: 42.7935, max_lat: 45.0403, min_lon: -73.3486, max_lon: -71.9180 },
    StateNetwork { state: "WA", min_lat: 45.5186, max_lat: 48.8927, min_lon: -124.6626, max_lon: -117.0096 },
    StateNetwork { state: "WI", min_lat: 42.4950, max_lat: 46.8887, min_lon: -92.7900, max_lon: -86.8240 },
    StateNetwork { state: "WV", min_lat: 37.1958, max_lat: 40.2750, min_lon: -82.6550, max_lon: -77.8847 },
    StateNetwork { state: "WY", min_lat: 40.9374, max_lat: 45.0117, min_lon: -111.1424, max_lon: -104.0302 },
];

/// The state codes to fetch for a viewport, nearest-first, capped at
/// [`MAX_NETWORKS`].
///
/// Ordering matters only because of the cap: without it the result would be an
/// unordered set. Nearest-to-centre is the ranking because the station a user
/// is looking at is the one nearest the middle of their screen.
pub fn networks_for_viewport(view: &GeoBounds) -> Vec<&'static str> {
    let clat = (view.min_lat + view.max_lat) / 2.0;
    let clon = (view.min_lon + view.max_lon) / 2.0;

    let mut hits: Vec<&StateNetwork> =
        NETWORKS.iter().filter(|n| n.intersects(view)).collect();
    hits.sort_by(|a, b| {
        a.centre_distance(clat, clon)
            .partial_cmp(&b.centre_distance(clat, clon))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(MAX_NETWORKS);
    hits.into_iter().map(|n| n.state).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> GeoBounds {
        GeoBounds { min_lat, max_lat, min_lon, max_lon }
    }

    /// A viewport over central Oklahoma must fetch Oklahoma.
    ///
    /// The bounds are KTLX's neighbourhood (35.33 N, 97.28 W) padded by a
    /// degree — an ordinary radar view.
    #[test]
    fn a_viewport_over_a_state_selects_that_state() {
        let states = networks_for_viewport(&view(34.3, 36.3, -98.3, -96.3));
        assert!(states.contains(&"OK"), "got {states:?}");
    }

    /// ...and must not fetch states nowhere near it. This is the half that
    /// makes the scoping worth doing.
    ///
    /// The assertion is on the **exact set**, and on its being well under
    /// [`MAX_NETWORKS`], for a specific reason: a version of
    /// `networks_for_viewport` that skipped the intersection test entirely
    /// still excludes Maine and American Samoa from an Oklahoma view, because
    /// the nearest-first cap does that on its own. Naming only the absent
    /// states therefore asserts what the *cap* guarantees, not what the
    /// *filter* does. Requiring the result to be exactly `{OK, TX}` — two
    /// networks against a cap of twelve — cannot be satisfied by the cap.
    #[test]
    fn a_viewport_over_a_state_skips_distant_states() {
        let mut states = networks_for_viewport(&view(34.3, 36.3, -98.3, -96.3));
        states.sort_unstable();
        assert_eq!(
            states,
            ["OK", "TX"],
            "only Oklahoma and the Texas panhandle reach this box",
        );
        assert!(
            states.len() < MAX_NETWORKS,
            "the result must be smaller than the cap, or the cap is doing the \
             filtering and this test proves nothing",
        );
        for far in ["ME", "FL", "WA", "PR", "AS"] {
            assert!(!states.contains(&far), "{far} is not near Oklahoma: {states:?}");
        }
    }

    /// A viewport straddling a border fetches both sides.
    ///
    /// The Red River at ~33.9 N separates Oklahoma from Texas; a view spanning
    /// it needs both networks or half the stations vanish.
    #[test]
    fn a_viewport_straddling_a_border_selects_both_states() {
        let states = networks_for_viewport(&view(33.2, 34.6, -98.0, -96.5));
        assert!(states.contains(&"OK"), "got {states:?}");
        assert!(states.contains(&"TX"), "got {states:?}");
    }

    /// The cap holds for a whole-country view, which would otherwise be 54
    /// requests and ~3.9 MB.
    #[test]
    fn a_continental_viewport_is_capped() {
        let states = networks_for_viewport(&view(24.0, 50.0, -125.0, -66.0));
        assert!(
            states.len() <= MAX_NETWORKS,
            "{} networks selected, cap is {MAX_NETWORKS}",
            states.len(),
        );
        assert!(!states.is_empty());
    }

    /// The cap keeps the *nearest* networks, not the first ones in table
    /// order. The table is alphabetical, so a truncation without the sort
    /// would return AK/AL/AR/AS/AZ... for a view centred on Kansas.
    #[test]
    fn the_cap_keeps_the_networks_nearest_the_viewport_centre() {
        // Centred on Kansas, wide enough to overlap far more than the cap.
        let states = networks_for_viewport(&view(30.0, 45.0, -108.0, -88.0));
        assert_eq!(states.len(), MAX_NETWORKS);
        assert!(states.contains(&"KS"), "got {states:?}");
        assert!(
            !states.contains(&"AS"),
            "American Samoa is alphabetically 4th but 8,000 km away: {states:?}",
        );
        // The nearest network to the centre of that box is Kansas itself.
        assert_eq!(states[0], "KS", "nearest-first ordering: {states:?}");
    }

    /// Overlap is inclusive at the edge: a viewport touching a network's
    /// boundary still selects it. An exclusive comparison drops the stations
    /// exactly on the seam.
    #[test]
    fn overlap_is_inclusive_at_the_boundary() {
        let ok = NETWORKS.iter().find(|n| n.state == "OK").unwrap();
        let touching = view(ok.max_lat, ok.max_lat + 1.0, ok.min_lon, ok.max_lon);
        assert!(ok.intersects(&touching), "a shared edge is an overlap");

        let clear = view(ok.max_lat + 0.001, ok.max_lat + 1.0, ok.min_lon, ok.max_lon);
        assert!(!ok.intersects(&clear), "a gap is not an overlap");
    }

    /// The table must stay well-formed: two-letter codes, no duplicates, and
    /// no inverted bounds.
    #[test]
    fn the_network_table_is_well_formed() {
        assert_eq!(NETWORKS.len(), 54, "50 states + AS, GU, PR, VI");
        let mut seen = std::collections::HashSet::new();
        for n in NETWORKS {
            assert_eq!(n.state.len(), 2, "{} is not a postal code", n.state);
            assert!(
                n.state.chars().all(|c| c.is_ascii_uppercase()),
                "{} is not uppercase",
                n.state,
            );
            assert!(seen.insert(n.state), "{} appears twice", n.state);
            assert!(n.min_lat <= n.max_lat, "{} has inverted latitudes", n.state);
            assert!((-90.0..=90.0).contains(&n.min_lat), "{} min_lat", n.state);
            assert!((-90.0..=90.0).contains(&n.max_lat), "{} max_lat", n.state);
            assert!((-180.0..=180.0).contains(&n.min_lon), "{} min_lon", n.state);
            assert!((-180.0..=180.0).contains(&n.max_lon), "{} max_lon", n.state);
        }
        // There is no DC network; its stations live in VA and MD.
        assert!(!seen.contains("DC"));
    }

    /// Spot-checks against geography, independent of IEM: each state's extent
    /// must contain a well-known airport in that state.
    ///
    /// Coordinates are published airport reference points, not values derived
    /// from this table.
    #[test]
    fn each_spot_checked_extent_contains_a_known_airport_in_that_state() {
        // (state, airport, lat, lon)
        let cases = [
            ("OK", "KOKC Will Rogers", 35.3931, -97.6007),
            ("TX", "KDFW", 32.8968, -97.0380),
            ("FL", "KMIA", 25.7959, -80.2870),
            ("ME", "KBGR Bangor", 44.8074, -68.8281),
            ("WA", "KSEA", 47.4502, -122.3088),
            ("PR", "TJSJ San Juan", 18.4394, -66.0018),
        ];
        for (state, airport, lat, lon) in cases {
            let n = NETWORKS.iter().find(|n| n.state == state).unwrap();
            assert!(
                (n.min_lat..=n.max_lat).contains(&lat)
                    && (n.min_lon..=n.max_lon).contains(&lon),
                "{state} extent {:?} does not contain {airport} ({lat}, {lon})",
                (n.min_lat, n.max_lat, n.min_lon, n.max_lon),
            );
        }
    }
}
