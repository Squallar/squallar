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

/// A radar the catalogue can only *name* is still recognised as a TDWR.
///
/// `TPBI` is the case, and it is the one the compiled-in table used to settle
/// by placing it: a terminal radar with real Level II data that
/// `api.weather.gov/radar/stations` will not place. With the table deleted it
/// has no row, and a row is where `ScanInfo::from_scan` used to get the name
/// that `is_tdwr` reads.
///
/// Falling back to `UNKNOWN_SITE_NAME` there would make `is_wsr88d` answer
/// **true** for it, and the picker would offer the four Level III products a
/// TDWR's SPG does not generate — five entries that draw an empty pane for the
/// rest of the session, because `ScanInfo` accumulates.
///
/// So the name comes from the membership list rather than from a row. Fails on
/// revert: take the `sites::static_name` lookup out of `from_scan` and this
/// site is a WSR-88D with a full product list.
#[test]
fn an_unplaceable_tdwr_is_still_a_tdwr() {
    const SITE: &str = "TZZH";

    sites::resolve([(SITE, SiteFix::Unplaced)]);
    assert!(
        sites::get_radar_site(SITE).is_none(),
        "precondition: it must have no row, or this proves nothing",
    );

    let info = rustdar_radar::types::ScanInfo::from_scan(
        &silent_scan(),
        SITE,
        chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
            .expect("a real date")
            .and_hms_opt(3, 0, 0)
            .expect("a real time"),
        None,
    );

    assert_eq!(
        info.site.name, SITE,
        "a radar the catalogue named must not be called UNKNOWN",
    );
    assert!(info.site.is_tdwr(), "and the T prefix must still be read");
    assert!(!info.site.is_wsr88d());

    // It still has no position, which is the other half of what `Unplaced`
    // means and must not have been invented along with the name.
    assert_eq!(
        info.site_source,
        rustdar_radar::site_position::SitePositionSource::Unknown,
    );
    assert!(info.site.heights.is_none());
}

/// The volume that finally places such a radar keeps its name, too.
///
/// The path `TPBI` actually takes: listed, opened, and placed by the volume
/// that comes back. `SitePosition::applied_to` has no row to take a name from
/// and reaches for `UNKNOWN_SITE_NAME`; the membership list is what stops it.
#[test]
fn a_volume_for_an_unplaceable_radar_names_it_from_the_membership_list() {
    const SITE: &str = "TZZI";

    sites::resolve([(SITE, SiteFix::Unplaced)]);
    let scan = nexrad_model::data::Scan::with_site(
        nexrad_model::meta::Site::new(*b"TZZI", -35.0, -145.0, 100, 100),
        vcp(),
        Vec::new(),
    );

    let info = rustdar_radar::types::ScanInfo::from_scan(
        &scan,
        SITE,
        chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
            .expect("a real date")
            .and_hms_opt(3, 0, 0)
            .expect("a real time"),
        None,
    );

    assert_eq!(info.site.name, SITE, "not UNKNOWN");
    assert!(info.site.is_tdwr());
    assert_eq!(
        info.site_source,
        rustdar_radar::site_position::SitePositionSource::Volume,
    );
    assert_eq!((info.site.lat, info.site.lon), (-35.0, -145.0));
}

/// A volume coverage pattern, which `Scan::with_site` needs and nothing here
/// reads.
fn vcp() -> nexrad_model::data::VolumeCoveragePattern {
    nexrad_model::data::VolumeCoveragePattern::new(
        212,
        0,
        0.5,
        nexrad_model::data::PulseWidth::Short,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        Vec::new(),
    )
}

/// A volume that states no site at all — the chunk-fed and pre-2010 shape.
fn silent_scan() -> nexrad_model::data::Scan {
    nexrad_model::data::Scan::new(vcp(), Vec::new())
}
