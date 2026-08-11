//! The site table is resolved at runtime, through the functions production
//! actually calls.
//!
//! `sites`' own unit tests build a [`SiteTable`](rustdar_radar::sites::SiteTable)
//! and interrogate it directly, which proves the construction but leaves the
//! part every caller depends on untested: the free functions —
//! `get_radar_site`, `nearest_wsr88d_site`, `radars` — read the table this
//! process *resolved*, and not the compiled-in array.
//!
//! That is the whole claim of the refactor, and it is a claim about a
//! process-wide value, so it is tested here rather than in the library's own
//! test binary: an integration test file is its own process, and these are the
//! only tests in it.
//!
//! # Why these can run in parallel
//!
//! `resolve` only ever *adds* radars — see its documentation — so the table
//! grows monotonically and no test here can undo another's. Each test uses its
//! own identifier and asserts nothing about the table's exact length, only
//! that its own radar is in it. Both properties are load-bearing: an
//! assertion like `len() == 208` would pass or fail depending on which test
//! ran first, and a test that cannot fail reliably is worse than no test.

use rustdar_radar::site_position::SitePosition;
use rustdar_radar::sites::{self, Datum, SiteFix};

/// A position in the middle of the South Pacific, ~5000 km from the nearest
/// real radar.
///
/// Far enough that a nearest-search answering with the added row cannot be
/// confused with it answering with something genuinely nearby, and far enough
/// from every other identifier used here that they cannot answer for each
/// other.
fn remote(lat_udeg: i32, lon_udeg: i32) -> SiteFix {
    SiteFix::Learned(SitePosition {
        lat_udeg,
        lon_udeg,
        site_height_m: 100,
        tower_height_m: 20,
    })
}

/// The refactor's whole point, through the production lookup.
///
/// Before this change `get_radar_site` read a `HashMap` built once from a
/// `[RadarSite; 207]`, so an identifier outside those 207 could not be named,
/// placed or drawn no matter what the application had learned about it. A
/// radar commissioned after the build was invisible to the binary for the
/// binary's whole life.
///
/// Every assertion below is unreachable if `radars()` goes back to being that
/// array: an array cannot grow a row.
#[test]
fn a_radar_the_seed_never_heard_of_is_reachable_through_the_production_lookup() {
    const SITE: &str = "ZZZA";

    assert!(
        sites::get_radar_site(SITE).is_none(),
        "precondition: {SITE} must not be a seed row, or this proves nothing",
    );
    let before = sites::radars().len();

    sites::resolve([(SITE, remote(-30_000_000, -140_000_000))]);

    let row = sites::get_radar_site(SITE).expect("resolved radars must be findable by name");
    assert_eq!(row.name, SITE, "it carries its own ICAO, not UNKNOWN");
    assert_eq!((row.lat, row.lon), (-30.0, -140.0));
    assert!(
        row.heights.is_some(),
        "a row with no elevation anchors a cross-section at sea level",
    );
    assert_eq!(row.height_ft(Datum::Feedhorn), Some(394));

    // Findable by search, not merely by name: this is the route automatic site
    // selection and `radar_height_ft_near` take.
    let (found, dist) = sites::nearest_radar_site(-30.0, -140.0).expect("a finite coordinate");
    assert_eq!(found.name, SITE, "at {dist} km");
    let (found, _) = sites::nearest_wsr88d_site(-30.0, -140.0).expect("a finite coordinate");
    assert_eq!(found.name, SITE, "and through the WSR-88D filter");

    // And present in the walk every drawing consumer does. `>` rather than an
    // exact length: a sibling test may have added its own radar by now.
    assert!(sites::radars().len() > before);
    assert!(sites::radars().iter().any(|r| r.name == SITE));
}

/// A later resolution that learns nothing does not take the radar away again.
///
/// This is the Android shape inverted, and it is why `resolve` extends the
/// table in hand rather than rebuilding from the seed. `App::new` resolves
/// with whatever store it has — on Android, none — and `set_config_dir`
/// resolves again once there is one. If the empty resolution could win, the
/// platform that needs this most would be the one platform where it never
/// worked.
#[test]
fn a_later_resolution_that_learns_nothing_keeps_what_is_already_known() {
    const SITE: &str = "ZZZB";

    sites::resolve([(SITE, remote(-31_000_000, -141_000_000))]);
    assert!(sites::get_radar_site(SITE).is_some(), "precondition");

    sites::resolve(std::iter::empty());

    let row = sites::get_radar_site(SITE)
        .expect("an empty resolution must not make the process forget a radar");
    assert_eq!((row.lat, row.lon), (-31.0, -141.0));
}

/// Adding a radar leaves every seeded answer where it was.
///
/// The counterweight to the tests above: a table that can grow is only useful
/// if growing it is safe. `every_site_is_its_own_nearest_neighbour` covers the
/// seed against itself; this covers the seed against an arrival.
#[test]
fn adding_a_radar_does_not_move_the_ones_already_there() {
    const SITE: &str = "ZZZC";

    let ktlx = sites::get_radar_site("KTLX").expect("a seed row");
    let (before, _) = sites::nearest_wsr88d_site(35.4676, -97.5164).expect("a finite coordinate");
    assert_eq!(before.name, "KTLX", "precondition");

    sites::resolve([(SITE, remote(-32_000_000, -142_000_000))]);

    let after = sites::get_radar_site("KTLX").expect("still a row");
    assert_eq!(
        (after.lat, after.lon),
        (ktlx.lat, ktlx.lon),
        "a seeded radar must not move when another is added",
    );
    let (still, _) = sites::nearest_wsr88d_site(35.4676, -97.5164).expect("a finite coordinate");
    assert_eq!(still.name, "KTLX");
}

/// Every row the process resolved records an elevation, arrivals included.
///
/// `sites`' own `every_site_records_an_elevation` walks the resolved table
/// too, but in a binary where nothing ever resolves anything, so it only ever
/// sees the seed. This is the same invariant asserted where it can actually be
/// broken: a missing elevation once reached `radar_height_ft_near` and came
/// back as sea level — plausible for a coastal site, and 90 m of error at
/// KLWX.
#[test]
fn every_resolved_row_records_an_elevation_including_the_arrivals() {
    const SITE: &str = "ZZZD";

    sites::resolve([(SITE, remote(-33_000_000, -143_000_000))]);
    assert!(
        sites::radars().iter().any(|r| r.name == SITE),
        "precondition: the arrival must be in the walk",
    );

    let missing: Vec<&str> = sites::radars()
        .iter()
        .filter(|s| s.height_ft(Datum::Feedhorn).is_none())
        .map(|s| s.name)
        .collect();
    assert!(
        missing.is_empty(),
        "these rows would anchor a section at sea level: {missing:?}",
    );
}
