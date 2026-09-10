//! Which IEM ASOS networks a viewport needs.
//!
//! IEM's ASOS networks are per-state (`OK_ASOS`, `TX_ASOS`, ...) at ~72 KB of
//! JSON each. `?networkclass=ASOS` fetches everything in one request but is
//! **54 MB, served ungzipped** — and it returns valid JSON, so nothing
//! downstream would notice the switch. Do not use it.
//!
//! Bounds below are decoded from the `extent` column of
//! `https://mesonet.agron.iastate.edu/api/1/networks.json` (PostGIS polygon,
//! EPSG:4326). They are *station* extents, not political borders.

use squallar_geo::GeoBounds;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StateNetwork {
    /// Two-letter postal code; the network id is `{state}_ASOS`.
    pub state: &'static str,
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl StateNetwork {
    /// The station extent as the shared bounds type.
    fn bounds(&self) -> GeoBounds {
        GeoBounds {
            min_lat: self.min_lat,
            max_lat: self.max_lat,
            min_lon: self.min_lon,
            max_lon: self.max_lon,
        }
    }

    /// Inclusive on purpose: a false positive costs one 72 KB request, a false
    /// negative silently drops every station in a state.
    pub fn intersects(&self, view: &GeoBounds) -> bool {
        self.bounds().intersects(view)
    }
}

/// **How many of the viewport's networks are in flight at once** — a bound on
/// what one round holds in memory, and never on what it asks for.
///
/// A continental view overlaps 48 of the 54 networks: 48 requests and 3.4 MB
/// of JSON. Under `futures::future::join_all` nothing starts a request late,
/// so every body that has arrived is live at the same instant and the round's
/// transient is the whole round. Measured 2026-09-10, one continental round,
/// live-heap peak over the pre-round level (`squallar_alloc`):
///
/// ```text
///                        every request at once     six at a time
///   local fixture,
///   48 x 71,799 B body        8,534,825 B           1,923,557 B
///   the real IEM              6,079,110 B           2,044,989 B
/// ```
///
/// Same total transfer, same 2,640 observations held either way (1,620,288 B
/// of them). The bodies are the whole difference.
///
/// **And the unbounded shape drops networks.** On the real-IEM leg above,
/// 48 simultaneous requests came back with `KS_ASOS` refused — a state blank
/// on the map for no reason but the shape of the round. The bounded leg lost
/// none.
///
/// Six rather than one because the round is latency-bound: the requests are
/// independent and a strictly serial round pays 48 round trips end to end.
/// What six costs against all-at-once is under a second of *data* latency on a
/// continental round (measured 872 ms and 1,729 ms against 483 ms; two runs
/// and one, so the direction is measured and the magnitude is not).
pub const MAX_IN_FLIGHT: usize = 6;

/// Stands in when the first overlay fetch precedes the first rendered frame, so
/// there is no map extent yet. Fetching nothing looks like an outage.
pub const DEFAULT_VIEWPORT: GeoBounds = GeoBounds {
    min_lat: 30.0,
    max_lat: 45.0,
    min_lon: -104.0,
    max_lon: -85.0,
};

/// Decoded from `networks.json` on 2026-07-25. 54 entries: 50 states plus AS,
/// GU, PR, VI. There is no `DC_ASOS`; those stations sit in `VA_ASOS`/`MD_ASOS`.
///
/// `AK` (-176.75..174.22) and `GU` (144.70..166.74) span the antimeridian, so
/// IEM's axis-aligned extents for them are enormous. Stored as published: the
/// cost is an occasional extra request, and tightening them by hand is the
/// transcription this table exists to avoid.
pub const NETWORKS: &[StateNetwork] = &[
    StateNetwork {
        state: "AK",
        min_lat: 51.7780,
        max_lat: 71.3826,
        min_lon: -176.7460,
        max_lon: 174.2169,
    },
    StateNetwork {
        state: "AL",
        min_lat: 28.9500,
        max_lat: 34.9600,
        min_lon: -88.3456,
        max_lon: -85.0289,
    },
    StateNetwork {
        state: "AR",
        min_lat: 33.1210,
        max_lat: 36.5042,
        min_lon: -94.5900,
        max_lon: -89.7300,
    },
    StateNetwork {
        state: "AS",
        min_lat: -14.4310,
        max_lat: -14.2310,
        min_lon: -170.8105,
        max_lon: -170.6105,
    },
    StateNetwork {
        state: "AZ",
        min_lat: 31.3208,
        max_lat: 37.0599,
        min_lon: -114.7060,
        max_lon: -108.9614,
    },
    StateNetwork {
        state: "CA",
        min_lat: 32.4631,
        max_lat: 41.8837,
        min_lon: -124.3380,
        max_lon: -114.5233,
    },
    StateNetwork {
        state: "CO",
        min_lat: 37.0515,
        max_lat: 40.8503,
        min_lon: -108.8593,
        max_lon: -102.1410,
    },
    StateNetwork {
        state: "CT",
        min_lat: 41.0583,
        max_lat: 42.0381,
        min_lon: -73.5800,
        max_lon: -71.9500,
    },
    StateNetwork {
        state: "DE",
        min_lat: 38.5892,
        max_lat: 39.7728,
        min_lon: -75.7008,
        max_lon: -75.2589,
    },
    StateNetwork {
        state: "FL",
        min_lat: 24.4561,
        max_lat: 30.9458,
        min_lon: -87.4180,
        max_lon: -79.9848,
    },
    StateNetwork {
        state: "GA",
        min_lat: 30.6825,
        max_lat: 34.9544,
        min_lon: -85.3903,
        max_lon: -81.0460,
    },
    StateNetwork {
        state: "GU",
        min_lat: 13.3839,
        max_lat: 19.3800,
        min_lon: 144.6972,
        max_lon: 166.7419,
    },
    StateNetwork {
        state: "HI",
        min_lat: 19.6203,
        max_lat: 28.3082,
        min_lon: -177.4756,
        max_lon: -154.9485,
    },
    StateNetwork {
        state: "IA",
        min_lat: 40.3615,
        max_lat: 43.5008,
        min_lon: -96.4795,
        max_lon: -90.2328,
    },
    StateNetwork {
        state: "ID",
        min_lat: 42.0069,
        max_lat: 48.8260,
        min_lon: -117.1154,
        max_lon: -110.9979,
    },
    StateNetwork {
        state: "IL",
        min_lat: 36.9647,
        max_lat: 42.5222,
        min_lon: -91.2946,
        max_lon: -87.4295,
    },
    StateNetwork {
        state: "IN",
        min_lat: 37.9441,
        max_lat: 41.8200,
        min_lon: -87.6205,
        max_lon: -84.7428,
    },
    StateNetwork {
        state: "KS",
        min_lat: 36.9008,
        max_lat: 40.0042,
        min_lon: -101.9800,
        max_lon: -94.6311,
    },
    StateNetwork {
        state: "KY",
        min_lat: 36.5106,
        max_lat: 39.1431,
        min_lon: -88.8744,
        max_lon: -82.4674,
    },
    StateNetwork {
        state: "LA",
        min_lat: 26.1367,
        max_lat: 32.8561,
        min_lon: -95.1641,
        max_lon: -87.6810,
    },
    StateNetwork {
        state: "MA",
        min_lat: 41.1531,
        max_lat: 42.8172,
        min_lon: -73.3892,
        max_lon: -69.8933,
    },
    StateNetwork {
        state: "MD",
        min_lat: 38.0460,
        max_lat: 39.8078,
        min_lon: -79.4394,
        max_lon: -75.0239,
    },
    StateNetwork {
        state: "ME",
        min_lat: 43.2939,
        max_lat: 47.3855,
        min_lon: -71.0479,
        max_lon: -66.9127,
    },
    StateNetwork {
        state: "MI",
        min_lat: 41.6358,
        max_lat: 47.5669,
        min_lon: -90.2314,
        max_lon: -82.4289,
    },
    StateNetwork {
        state: "MN",
        min_lat: 43.5212,
        max_lat: 49.4183,
        min_lon: -97.0430,
        max_lon: -90.2457,
    },
    StateNetwork {
        state: "MO",
        min_lat: 36.1259,
        max_lat: 40.4525,
        min_lon: -95.0150,
        max_lon: -89.4577,
    },
    StateNetwork {
        state: "MS",
        min_lat: 28.1206,
        max_lat: 35.0787,
        min_lon: -91.3973,
        max_lon: -88.0659,
    },
    StateNetwork {
        state: "MT",
        min_lat: 44.5500,
        max_lat: 49.0738,
        min_lon: -115.5902,
        max_lon: -104.0926,
    },
    StateNetwork {
        state: "NC",
        min_lat: 33.8292,
        max_lat: 36.5600,
        min_lon: -83.9630,
        max_lon: -75.5225,
    },
    StateNetwork {
        state: "ND",
        min_lat: 45.9149,
        max_lat: 49.0406,
        min_lon: -104.0821,
        max_lon: -96.5074,
    },
    StateNetwork {
        state: "NE",
        min_lat: 39.9788,
        max_lat: 42.9567,
        min_lon: -104.0950,
        max_lon: -95.4920,
    },
    StateNetwork {
        state: "NH",
        min_lat: 42.6818,
        max_lat: 44.6761,
        min_lon: -72.4042,
        max_lon: -70.7233,
    },
    StateNetwork {
        state: "NJ",
        min_lat: 38.9085,
        max_lat: 41.3002,
        min_lon: -75.1783,
        max_lon: -73.9562,
    },
    StateNetwork {
        state: "NM",
        min_lat: 31.7804,
        max_lat: 37.0000,
        min_lon: -109.0300,
        max_lon: -102.9793,
    },
    StateNetwork {
        state: "NV",
        min_lat: 35.8475,
        max_lat: 42.0532,
        min_lon: -119.9764,
        max_lon: -114.4264,
    },
    StateNetwork {
        state: "NY",
        min_lat: 40.5386,
        max_lat: 45.0334,
        min_lon: -79.3720,
        max_lon: -71.8233,
    },
    StateNetwork {
        state: "OH",
        min_lat: 38.7405,
        max_lat: 41.8780,
        min_lon: -84.8844,
        max_lon: -80.5739,
    },
    StateNetwork {
        state: "OK",
        min_lat: 33.8094,
        max_lat: 37.0092,
        min_lon: -101.6053,
        max_lon: -94.5200,
    },
    StateNetwork {
        state: "OR",
        min_lat: 41.9500,
        max_lat: 46.2569,
        min_lon: -124.5249,
        max_lon: -116.9128,
    },
    StateNetwork {
        state: "PA",
        min_lat: 39.6290,
        max_lat: 42.1800,
        min_lon: -80.5134,
        max_lon: -74.9134,
    },
    StateNetwork {
        state: "PR",
        min_lat: 17.9083,
        max_lat: 18.5949,
        min_lon: -67.2485,
        max_lon: -65.5386,
    },
    StateNetwork {
        state: "RI",
        min_lat: 41.0700,
        max_lat: 42.0208,
        min_lon: -71.8989,
        max_lon: -71.1815,
    },
    StateNetwork {
        state: "SC",
        min_lat: 32.1244,
        max_lat: 35.0878,
        min_lon: -82.9868,
        max_lon: -78.6239,
    },
    StateNetwork {
        state: "SD",
        min_lat: 42.6653,
        max_lat: 46.0187,
        min_lon: -103.9620,
        max_lon: -96.4660,
    },
    StateNetwork {
        state: "TN",
        min_lat: 34.9353,
        max_lat: 36.7219,
        min_lon: -90.1540,
        max_lon: -82.0734,
    },
    StateNetwork {
        state: "TX",
        min_lat: 25.8146,
        max_lat: 36.5140,
        min_lon: -106.4800,
        max_lon: -91.9333,
    },
    StateNetwork {
        state: "UT",
        min_lat: 36.9111,
        max_lat: 41.8913,
        min_lon: -114.1309,
        max_lon: -109.2412,
    },
    StateNetwork {
        state: "VA",
        min_lat: 36.4729,
        max_lat: 39.2435,
        min_lon: -83.3178,
        max_lon: -75.3631,
    },
    StateNetwork {
        state: "VI",
        min_lat: 17.6000,
        max_lat: 18.4373,
        min_lon: -65.0734,
        max_lon: -64.7047,
    },
    StateNetwork {
        state: "VT",
        min_lat: 42.7935,
        max_lat: 45.0403,
        min_lon: -73.3486,
        max_lon: -71.9180,
    },
    StateNetwork {
        state: "WA",
        min_lat: 45.5186,
        max_lat: 48.8927,
        min_lon: -124.6626,
        max_lon: -117.0096,
    },
    StateNetwork {
        state: "WI",
        min_lat: 42.4950,
        max_lat: 46.8887,
        min_lon: -92.7900,
        max_lon: -86.8240,
    },
    StateNetwork {
        state: "WV",
        min_lat: 37.1958,
        max_lat: 40.2750,
        min_lon: -82.6550,
        max_lon: -77.8847,
    },
    StateNetwork {
        state: "WY",
        min_lat: 40.9374,
        max_lat: 45.0117,
        min_lon: -111.1424,
        max_lon: -104.0302,
    },
];

/// **Every network the viewport overlaps**, in [`NETWORKS`] order.
///
/// There is no cap on the count. There was one — the twelve nearest the
/// viewport centre — and a continental view overlaps 48 networks, so it
/// dropped thirty-six states' worth of stations off the map with nothing in
/// the tree able to say so: the round reported the count it had *kept* as the
/// number it asked for, so `MetarRound::completeness` read
/// `expected: 12, missing: 0` over a map blank from the Rockies to the
/// Atlantic. A user reported it as missing stations, which is what it was.
///
/// What the cap was protecting against is real and is now bounded where it
/// belongs: how much of a round is resident at once, by
/// [`MAX_IN_FLIGHT`], rather than how much of the country is on the map.
pub fn networks_for_viewport(view: &GeoBounds) -> Vec<&'static str> {
    NETWORKS
        .iter()
        .filter(|n| n.intersects(view))
        .map(|n| n.state)
        .collect()
}

/// **Does `view` reach a network `held` never asked for?** — the one question
/// that makes a viewport-scoped round stale for a reason other than its age.
///
/// Allocates nothing and stops at the first miss, because it is asked on the
/// frame the map moves. `held` is what the round on the layer actually
/// requested, not what the current view wants: a round is only ever stale
/// against the view, and comparing against a recomputed want would compare a
/// value with itself.
pub fn viewport_reaches_beyond(view: &GeoBounds, held: &[&'static str]) -> bool {
    NETWORKS
        .iter()
        .any(|n| n.intersects(view) && !held.contains(&n.state))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> GeoBounds {
        GeoBounds {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        }
    }

    /// Bounds are KTLX (35.33 N, 97.28 W) padded by a degree.
    #[test]
    fn a_viewport_over_a_state_selects_that_state() {
        let states = networks_for_viewport(&view(34.3, 36.3, -98.3, -96.3));
        assert!(states.contains(&"OK"), "got {states:?}");
    }

    #[test]
    fn a_viewport_over_a_state_skips_distant_states() {
        let mut states = networks_for_viewport(&view(34.3, 36.3, -98.3, -96.3));
        states.sort_unstable();
        assert_eq!(
            states,
            ["OK", "TX"],
            "only Oklahoma and the Texas panhandle reach this box",
        );
        for far in ["ME", "FL", "WA", "PR", "AS"] {
            assert!(
                !states.contains(&far),
                "{far} is not near Oklahoma: {states:?}"
            );
        }
    }

    #[test]
    fn a_viewport_straddling_a_border_selects_both_states() {
        let states = networks_for_viewport(&view(33.2, 34.6, -98.0, -96.5));
        assert!(states.contains(&"OK"), "got {states:?}");
        assert!(states.contains(&"TX"), "got {states:?}");
    }

    /// **The user's screenshot, as an assertion.** A continental view asks for
    /// every network it overlaps, and the four corners of the country are in
    /// it. Under the old cap this answered twelve: the twelve nearest the
    /// centre of the map, which is exactly the central-third-only picture that
    /// was reported.
    #[test]
    fn a_continental_viewport_asks_for_every_network_it_overlaps() {
        let continent = view(24.0, 50.0, -125.0, -66.0);
        let states = networks_for_viewport(&continent);
        let overlapping = NETWORKS.iter().filter(|n| n.intersects(&continent)).count();
        assert_eq!(
            states.len(),
            overlapping,
            "the selection must be the overlap and nothing less: {states:?}",
        );
        assert!(
            overlapping > 40,
            "premise: a continental view really does overlap most of the \
             table, or this test is asserting nothing ({overlapping})",
        );
        for corner in ["CA", "WA", "FL", "ME", "TX", "MN"] {
            assert!(
                states.contains(&corner),
                "{corner} is on the continental map: {states:?}",
            );
        }
    }

    #[test]
    fn a_narrow_viewport_still_asks_for_no_more_than_it_overlaps() {
        let states = networks_for_viewport(&view(30.0, 45.0, -108.0, -88.0));
        assert!(states.contains(&"KS"), "got {states:?}");
        assert!(
            !states.contains(&"AS"),
            "American Samoa is 8,000 km away: {states:?}",
        );
        assert!(
            !states.contains(&"CA") && !states.contains(&"ME"),
            "neither coast reaches this box: {states:?}",
        );
    }

    /// The refetch predicate: what a round holds, against where the map now is.
    #[test]
    fn a_view_reaching_a_network_the_round_never_asked_for_is_beyond_it() {
        let plains = view(33.0, 40.0, -103.0, -94.0);
        let held = networks_for_viewport(&plains);
        assert!(
            !viewport_reaches_beyond(&plains, &held),
            "the very view a round was fetched for cannot outrun it: {held:?}",
        );
        // A map that shrinks inside what the round holds asks for a subset.
        let inside = view(33.2, 39.8, -102.5, -94.5);
        assert!(
            networks_for_viewport(&inside).len() < held.len(),
            "premise: this is the subset case",
        );
        assert!(!viewport_reaches_beyond(&inside, &held));

        // And a pan that keeps the very same ask.
        let over_oklahoma = view(34.3, 36.3, -98.3, -96.3);
        let ok_held = networks_for_viewport(&over_oklahoma);
        let panned = view(34.4, 36.4, -98.2, -96.2);
        assert_eq!(
            networks_for_viewport(&panned),
            ok_held,
            "premise: the pan asks for the very same networks",
        );
        assert!(!viewport_reaches_beyond(&panned, &ok_held));
        // The zoom out the user did.
        assert!(
            viewport_reaches_beyond(&view(24.0, 50.0, -125.0, -66.0), &held),
            "the continent reaches forty networks this round never asked for",
        );
    }

    #[test]
    fn overlap_is_inclusive_at_the_boundary() {
        let ok = NETWORKS.iter().find(|n| n.state == "OK").unwrap();
        let touching = view(ok.max_lat, ok.max_lat + 1.0, ok.min_lon, ok.max_lon);
        assert!(ok.intersects(&touching), "a shared edge is an overlap");

        let clear = view(ok.max_lat + 0.001, ok.max_lat + 1.0, ok.min_lon, ok.max_lon);
        assert!(!ok.intersects(&clear), "a gap is not an overlap");
    }

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

    /// Coordinates are published airport reference points, independent of IEM
    /// and not derived from this table.
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
                (n.min_lat..=n.max_lat).contains(&lat) && (n.min_lon..=n.max_lon).contains(&lon),
                "{state} extent {:?} does not contain {airport} ({lat}, {lon})",
                (n.min_lat, n.max_lat, n.min_lon, n.max_lon),
            );
        }
    }
}
