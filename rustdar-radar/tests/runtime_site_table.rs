//! The site table is resolved at runtime, through the functions production
//! actually calls.
//!
//! `sites`' own unit tests build a [`SiteTable`](rustdar_radar::sites::SiteTable)
//! and interrogate it directly, which proves the construction but leaves the
//! part every caller depends on untested: the free functions —
//! `get_radar_site`, `nearest_wsr88d_site`, `radars` — read the table this
//! process *resolved*, and there is nothing else for them to read. The binary
//! carries no radars at all.
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
/// `get_radar_site` used to read a `HashMap` built once from a
/// `[RadarSite; 207]`, so an identifier outside those 207 could not be named,
/// placed or drawn no matter what the application had learned about it. A
/// radar commissioned after the build was invisible to the binary for the
/// binary's whole life.
///
/// Every assertion below is unreachable if `radars()` goes back to being an
/// array: an array cannot grow a row.
#[test]
fn a_radar_the_binary_never_heard_of_is_reachable_through_the_production_lookup() {
    const SITE: &str = "ZZZA";

    assert!(
        sites::get_radar_site(SITE).is_none(),
        "precondition: nothing may have placed {SITE} yet, or this proves nothing",
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

/// Adding a radar leaves every answer already in the table where it was.
///
/// The counterweight to the tests above: a table that can grow is only useful
/// if growing it is safe.
///
/// The radar it must not move is one this test resolved itself. It used to be
/// `KTLX`, read out of the compiled-in table — and with that table deleted,
/// borrowing a sibling test's radar would make this pass or fail on which test
/// ran first. Its own identifier at its own position is what keeps it able to
/// fail for one reason.
#[test]
fn adding_a_radar_does_not_move_the_ones_already_there() {
    const INCUMBENT: &str = "ZZZE";
    const ARRIVAL: &str = "ZZZC";

    sites::resolve([(INCUMBENT, remote(20_000_000, 30_000_000))]);
    let before = sites::get_radar_site(INCUMBENT).expect("this test placed it");
    let (nearest, _) = sites::nearest_wsr88d_site(20.0, 30.0).expect("a finite coordinate");
    assert_eq!(nearest.name, INCUMBENT, "precondition");

    sites::resolve([(ARRIVAL, remote(-32_000_000, -142_000_000))]);

    let after = sites::get_radar_site(INCUMBENT).expect("still a row");
    assert_eq!(
        (after.lat, after.lon),
        (before.lat, before.lon),
        "a radar already in the table must not move when another is added",
    );
    let (still, _) = sites::nearest_wsr88d_site(20.0, 30.0).expect("a finite coordinate");
    assert_eq!(still.name, INCUMBENT);
}

/// Every row the process resolved records an elevation, arrivals included.
///
/// `sites`' own `every_placed_row_records_an_elevation` walks a table it built
/// itself. This is the same invariant asserted against the *process* table,
/// through the free function every drawing consumer walks: a missing elevation
/// once reached `radar_height_ft_near` and came back as sea level — plausible
/// for a coastal site, and 90 m of error at KLWX.
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

/// A radar the catalogue lists and cannot place reaches the site list without
/// reaching the map.
///
/// `TPBI` and `KCRI` are the real cases and the compiled-in table was the only
/// thing placing them. This is the shape they take without it, through the
/// functions the application calls: known, listed, and absent from every
/// answer that needs coordinates.
#[test]
fn a_member_with_no_position_is_known_without_becoming_a_row() {
    const SITE: &str = "ZZZF";

    assert!(
        !sites::knows_site(SITE),
        "precondition: nothing may have listed {SITE} yet",
    );

    sites::resolve([(SITE, SiteFix::Unplaced)]);

    assert!(sites::knows_site(SITE), "the site list can find it");
    assert!(
        sites::unplaced().contains(&SITE),
        "and it is in the list of radars with no position",
    );

    assert!(
        sites::get_radar_site(SITE).is_none(),
        "with no position to hand out",
    );
    assert!(
        sites::radars().iter().all(|r| r.name != SITE),
        "and no row, so nothing draws a marker for it",
    );
}

/// Opening an unplaceable radar places it, and it stops being merely a member.
///
/// The route that makes the previous test tolerable rather than a dead end:
/// Level II data is fetched by identifier, so a user can open `TPBI`, and the
/// volume that comes back states where it is.
#[test]
fn opening_an_unplaceable_radar_places_it() {
    const SITE: &str = "ZZZG";

    sites::resolve([(SITE, SiteFix::Unplaced)]);
    assert!(sites::unplaced().contains(&SITE), "precondition");

    sites::resolve([(SITE, remote(-34_000_000, -144_000_000))]);

    let row = sites::get_radar_site(SITE).expect("the volume placed it");
    assert_eq!((row.lat, row.lon), (-34.0, -144.0));
    assert!(
        !sites::unplaced().contains(&SITE),
        "and it must not stay in the member list, or the site list shows it twice",
    );
    assert_eq!(
        sites::radars().iter().filter(|r| r.name == SITE).count(),
        1,
        "exactly one row",
    );
}
