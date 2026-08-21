//! The site table is resolved at runtime, through the functions production
//! actually calls.

use rustdar_radar::site_position::SitePosition;
use rustdar_radar::sites::{self, Datum, SiteFix};

fn remote(lat_udeg: i32, lon_udeg: i32) -> SiteFix<'static> {
    SiteFix::Learned(SitePosition {
        lat_udeg,
        lon_udeg,
        site_height_m: 100,
        tower_height_m: 20,
    })
}

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

    let (found, dist) = sites::nearest_radar_site(-30.0, -140.0).expect("a finite coordinate");
    assert_eq!(found.name, SITE, "at {dist} km");
    let (found, _) = sites::nearest_wsr88d_site(-30.0, -140.0).expect("a finite coordinate");
    assert_eq!(found.name, SITE, "and through the WSR-88D filter");

    assert!(sites::radars().len() > before);
    assert!(sites::radars().iter().any(|r| r.name == SITE));
}

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

    assert_eq!(
        info.site_source,
        rustdar_radar::site_position::SitePositionSource::Unknown,
    );
    assert!(info.site.heights.is_none());
}

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
