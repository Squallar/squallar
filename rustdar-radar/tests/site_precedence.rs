//! **The precedence is the design**: `Volume > Learned > Network > Unplaced`.

use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};
use rustdar_radar::site_position::{SitePosition, SitePositionSource};
use rustdar_radar::sites::{self, Datum, SiteFix};
use rustdar_radar::types::ScanInfo;

/// Serializes the tests in this file.
static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`GATE`], ignoring a poisoning left by an earlier failure — the guard
/// orders these tests, it does not protect an invariant, and turning one
/// failure into four would hide which rung actually broke.
fn serialized() -> std::sync::MutexGuard<'static, ()> {
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Latitude the *volume* states, degrees.
const VOLUME_LAT: f32 = 32.0005;
/// The micro-degrees [`VOLUME_LAT`] becomes.
const VOLUME_LAT_UDEG: i32 = 32_000_500;
/// Latitude a *learned* position holds. A whole degree from everything else,
/// which it may be — nothing checks a learned position against the catalogue,
/// because a learned position is a volume the check already ran on.
const LEARNED_LAT_UDEG: i32 = 31_000_000;
/// Latitude the *network catalogue* holds.
const NETWORK_LAT_UDEG: i32 = 32_000_000;
/// Longitude every candidate shares: only the latitude varies, so a failure
/// names a rung rather than a coordinate.
const LON_UDEG: i32 = -97_000_000;
/// The place the *network catalogue* names. Only it has one: a volume states
/// where a radar is and never what it is called.
const NETWORK_PLACE: &str = "Bunkerville";

fn learned() -> SitePosition {
    SitePosition {
        lat_udeg: LEARNED_LAT_UDEG,
        lon_udeg: LON_UDEG,
        site_height_m: 370,
        tower_height_m: 20,
    }
}

fn network() -> SiteFix<'static> {
    SiteFix::Network {
        lat_udeg: NETWORK_LAT_UDEG,
        lon_udeg: LON_UDEG,
        elevation_m: 400,
        place: Some(NETWORK_PLACE),
    }
}

fn vcp() -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        212,
        0,
        0.5,
        PulseWidth::Short,
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

/// A volume that states its own position, as `scan::decoded` builds one out of
/// the first Message 31's Volume Data Block.
fn scan_stating(lat: f32) -> Scan {
    Scan::with_site(
        nexrad_model::meta::Site::new(*b"KTLX", lat, -97.0, 370, 20),
        vcp(),
        Vec::new(),
    )
}

/// The shape a chunk-fed or pre-2010 volume arrives in: no site on it at all.
fn silent_scan() -> Scan {
    Scan::new(vcp(), Vec::new())
}

/// The whole ladder, in one resolution.
#[test]
fn the_precedence_is_volume_learned_network_unplaced() {
    let _gate = serialized();
    for site in ["ZZPT", "ZZPX", "ZZPY", "ZZPU", "ZZPV"] {
        assert!(
            !sites::knows_site(site),
            "precondition: {site} must be unknown, or this proves nothing",
        );
    }

    sites::resolve([
        // Bare membership first, and for every identifier that will be placed.
        ("ZZPT", SiteFix::Unplaced),
        ("ZZPX", SiteFix::Unplaced),
        ("ZZPY", SiteFix::Unplaced),
        ("ZZPV", SiteFix::Unplaced),
        // Then the fetched positions.
        ("ZZPT", network()),
        ("ZZPX", network()),
        ("ZZPY", network()),
        // Then the learned ones, for two of them.
        ("ZZPT", SiteFix::Learned(learned())),
        ("ZZPY", SiteFix::Learned(learned())),
    ]);

    // What the *table* says — the map marker, the site list row, the height
    // datum a cross-section is anchored on.
    for (case, site, want_lat) in [
        (
            "a learned position outranks a fetched one",
            "ZZPT",
            f64::from(LEARNED_LAT_UDEG) / 1e6,
        ),
        (
            "a fetched position outranks bare membership",
            "ZZPX",
            f64::from(NETWORK_LAT_UDEG) / 1e6,
        ),
        (
            "and learned beats fetched wherever both speak",
            "ZZPY",
            f64::from(LEARNED_LAT_UDEG) / 1e6,
        ),
    ] {
        let row =
            sites::get_radar_site(site).unwrap_or_else(|| panic!("{case}: {site} is missing"));
        assert_eq!(row.lat, want_lat, "{case}");
        assert_eq!(row.name, site, "{case}: a row must carry its own ICAO");
        assert!(
            !sites::unplaced().contains(&site),
            "{case}: and a placed radar is not also a bare member",
        );
    }

    // Bare membership is what is left when nothing carries a position.
    assert!(
        sites::get_radar_site("ZZPV").is_none(),
        "a listing with no position must not become a row",
    );
    assert!(sites::unplaced().contains(&"ZZPV"), "it is still a member");

    // And the rung the seed used to occupy: nothing.
    assert!(
        !sites::knows_site("ZZPU"),
        "a radar no source spoke for is one this process has never heard of; \
         a compiled-in table would answer here and there is none",
    );

    // And what `ScanInfo::from_scan` says, which is the rung above the table.
    for (case, scan, memo, want_lat, want_source) in [
        (
            "the volume in hand outranks everything, learned included",
            scan_stating(VOLUME_LAT),
            Some(learned()),
            f64::from(VOLUME_LAT_UDEG) / 1e6,
            SitePositionSource::Volume,
        ),
        (
            "and outranks the table with nothing learned",
            scan_stating(VOLUME_LAT),
            None,
            f64::from(VOLUME_LAT_UDEG) / 1e6,
            SitePositionSource::Volume,
        ),
        (
            "a silent volume falls to what was learned",
            silent_scan(),
            Some(learned()),
            f64::from(LEARNED_LAT_UDEG) / 1e6,
            SitePositionSource::Learned,
        ),
        (
            "and to the table below that — which now holds the learned fix",
            silent_scan(),
            None,
            f64::from(LEARNED_LAT_UDEG) / 1e6,
            SitePositionSource::Table,
        ),
    ] {
        let info = ScanInfo::from_scan(&scan, "ZZPT", at(), memo);
        assert_eq!(info.site.lat, want_lat, "{case}");
        assert_eq!(info.site_source, want_source, "{case}");
    }

    // The bottom of the ladder, through the same call: a silent volume for a
    // radar nothing has placed has no position at all. This used to be
    // unreachable for any real ICAO, because the seed placed all of them.
    let orphan = ScanInfo::from_scan(&silent_scan(), "ZZPU", at(), None);
    assert_eq!(orphan.site_source, SitePositionSource::Unknown);
}

/// **The rung above the ladder**: a volume outranks the catalogue by metres,
/// not by kilometres.
#[test]
fn a_volume_that_disagrees_with_the_catalogue_does_not_displace_it() {
    let _gate = serialized();
    for site in ["ZZPQ", "ZZPN"] {
        assert!(
            !sites::knows_site(site),
            "precondition: {site} must be unknown, or this proves nothing",
        );
    }
    // `ZZPQ` is placed by the catalogue. `ZZPN` is placed by nothing.
    sites::resolve([("ZZPQ", network())]);

    let catalogue_lat = f64::from(NETWORK_LAT_UDEG) / 1e6;
    for (case, site, volume_lat, want_lat, want_source) in [
        (
            "a volume within the limit still outranks the catalogue",
            "ZZPQ",
            VOLUME_LAT,
            f64::from(VOLUME_LAT_UDEG) / 1e6,
            SitePositionSource::Volume,
        ),
        (
            "a volume a degree away does not — 111 km is not a re-survey",
            "ZZPQ",
            32.0 + 1.0,
            catalogue_lat,
            SitePositionSource::Table,
        ),
        (
            "and the sign of the disagreement is not what decides it",
            "ZZPQ",
            32.0 - 1.0,
            catalogue_lat,
            SitePositionSource::Table,
        ),
        (
            "the Gulf of Guinea, which is where a forged pair divides to",
            "ZZPQ",
            0.1,
            catalogue_lat,
            SitePositionSource::Table,
        ),
        (
            "a radar the catalogue never placed keeps its volume's word",
            "ZZPN",
            0.1,
            0.1,
            SitePositionSource::Volume,
        ),
    ] {
        let info = ScanInfo::from_scan(&scan_stating(volume_lat), site, at(), None);
        assert_eq!(info.site.lat, want_lat, "{case}");
        assert_eq!(info.site_source, want_source, "{case}");
    }

    // The refusal is a refusal of *this volume*, not a refusal to learn: the
    // catalogue's own row is exactly where it was, so nothing was half-applied
    // on the way past.
    let row = sites::get_radar_site("ZZPQ").expect("the catalogue placed it");
    assert_eq!(row.lat, catalogue_lat);
    assert_eq!(row.lon, f64::from(LON_UDEG) / 1e6);
}

/// A fetched elevation must not take a learned row's measured tower away.
#[test]
fn a_fetched_position_moves_a_row_without_taking_its_base_datum() {
    let _gate = serialized();
    sites::resolve([("ZZPS", SiteFix::Learned(learned()))]);
    let known = sites::get_radar_site("ZZPS")
        .expect("this test learned it")
        .clone();
    let base = known
        .height_ft(Datum::SiteBase)
        .expect("a learned WSR-88D records both datums");

    sites::resolve([("ZZPS", network())]);

    let row = sites::get_radar_site("ZZPS").expect("still a row");
    assert_eq!(
        row.lat,
        f64::from(NETWORK_LAT_UDEG) / 1e6,
        "the position moved",
    );
    assert_eq!(
        row.height_ft(Datum::SiteBase),
        Some(base),
        "and the heights did not: the catalogue restates the same ground with \
         no tower beside it, so it has nothing better to say here",
    );
    assert_eq!(
        row.height_ft(Datum::Feedhorn),
        known.height_ft(Datum::Feedhorn)
    );
}

/// A radar only the catalogue knows still records an elevation.
#[test]
fn an_arrival_only_the_catalogue_knows_takes_its_elevation() {
    let _gate = serialized();
    sites::resolve([("ZZPZ", network())]);

    let row = sites::get_radar_site("ZZPZ").expect("the catalogue placed it");
    // 400 m, the fix's elevation, in feet. `ZZPZ` is not `T`-prefixed, so the
    // record is read as the ground it is.
    assert_eq!(
        row.height_ft(Datum::SiteBase),
        Some(1312),
        "the station record's elevation is the ground, and is stated exactly",
    );
    assert_eq!(
        row.height_ft(Datum::Feedhorn),
        Some(1312 + 95),
        "and the feedhorn is that ground plus the nominal tower, because the \
         record carries no tower of its own",
    );
    assert!(
        sites::radars().iter().any(|r| r.name == "ZZPZ"),
        "and the walk the map and the site list both do reaches it",
    );
}

/// **The name is not on the precedence ladder.** A volume outranks the
/// catalogue about where a radar is; it says nothing about what place it is at,
/// and losing that rung must not lose the name.
///
/// The order here is the one that hurts: the catalogue first, then a volume
/// that displaces its position. Read after ranking rather than before, the
/// second resolution drops the `Network` fix whole and the name goes with it —
/// so every site the user has actually watched would be the one with no name.
#[test]
fn a_volume_takes_the_position_from_the_catalogue_and_leaves_the_name() {
    let _gate = serialized();
    assert!(
        !sites::knows_site("ZZPA"),
        "precondition: ZZPA must be unknown, or this proves nothing",
    );
    sites::resolve([("ZZPA", network())]);
    assert_eq!(
        sites::table().place("ZZPA"),
        Some(NETWORK_PLACE),
        "the catalogue named it",
    );

    sites::resolve([("ZZPA", SiteFix::Learned(learned()))]);

    let row = sites::get_radar_site("ZZPA").expect("still a row");
    assert_eq!(
        row.lat,
        f64::from(LEARNED_LAT_UDEG) / 1e6,
        "the control: the volume did take the position, so this is the case \
         that would have dropped the name",
    );
    assert_eq!(row.place(), Some(NETWORK_PLACE), "and the name stayed");
}

/// A radar nothing has named answers `None`, not an empty string. Every site
/// is in this state on a fresh install, before any catalogue lands.
#[test]
fn a_radar_no_catalogue_has_named_has_no_place_at_all() {
    let _gate = serialized();
    assert!(
        !sites::knows_site("ZZPB"),
        "precondition: ZZPB must be unknown, or this proves nothing",
    );
    sites::resolve([("ZZPB", SiteFix::Learned(learned()))]);

    let row = sites::get_radar_site("ZZPB").expect("the volume placed it");
    assert_eq!(row.place(), None);
    assert_eq!(sites::table().place("ZZPB"), None);
    assert_eq!(
        sites::table().place("ZZPC"),
        None,
        "and so does a radar the table has never heard of",
    );
}

/// Resolving twice with the same catalogue must build nothing the second time.
/// Since the catalogue now carries names, this is also what pins the name
/// **not** being leaked afresh on every launch: a name equal to the one on
/// record is not a change.
#[test]
fn a_second_resolution_with_the_same_catalogue_reuses_the_table() {
    let _gate = serialized();
    let fixes = || [("ZZPW", network()), ("ZZPR", network())];

    let first = sites::resolve(fixes());
    let second = sites::resolve(fixes());
    assert!(
        std::ptr::eq(first, second),
        "the same fixes twice must not build a second table",
    );

    // And a resolution that says nothing at all cannot undo one that did.
    let third = sites::resolve(std::iter::empty());
    assert!(std::ptr::eq(second, third));
    assert!(sites::get_radar_site("ZZPW").is_some());
}

fn at() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
        .expect("a real date")
        .and_hms_opt(1, 0, 0)
        .expect("a real time")
}
