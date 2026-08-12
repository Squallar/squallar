//! **The precedence is the design**: `Volume > Learned > Network > Unplaced`.
//!
//! Four sources can speak about a radar, and they disagree. The volume in hand
//! is the radar reporting itself; a learned position is the same radar
//! reporting itself in an earlier session; the network catalogue is a published
//! record *about* the radar; a bucket listing is the bare fact that the radar
//! exists. The ordering is not a preference — it is how close each source sits
//! to the instrument.
//!
//! There used to be a fifth rung under all of these, a compiled-in table of
//! the network as it stood on the day the binary was built. Deleting it
//! removed the bottom of the ladder rather than reordering what is left: a
//! radar no source speaks for is now a radar the process has never heard of,
//! and that is the row this file ends on.
//!
//! One table rather than a test per pair, because an ordering asserted one pair
//! at a time is an ordering nobody has checked. Every row uses the same
//! candidate positions, a degree apart, so a row that resolves to the wrong one
//! says which one in the failure message rather than in the last decimal place.
//!
//! # Why this is its own integration test binary
//!
//! `sites::resolve` sets a process-wide table, and this file resolves one and
//! then moves rows inside it, which no sibling test could tolerate. An
//! integration test file is its own process, so that stays contained here. It
//! is also the only way to exercise the thing under test: the free functions
//! `get_radar_site` and `radars` read the table the process resolved, which is
//! what the map marker, the site list and the section's height datum all walk.
//!
//! Every identifier below is one this file placed itself. None of them is a
//! real ICAO, and that is deliberate: with nothing compiled in, a test that
//! named `KTLX` would be asserting against whatever another test had said
//! about `KTLX`, which is a test that passes for the wrong reason.

use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};
use rustdar_radar::site_position::{SitePosition, SitePositionSource};
use rustdar_radar::sites::{self, Datum, SiteFix, SiteFixRank};
use rustdar_radar::types::ScanInfo;

/// Serializes the tests in this file.
///
/// They share one process-wide table and every one of them resolves into it,
/// so they are not independent the way `runtime_site_table.rs`'s are — that
/// file's tests only ever *add* radars under identifiers unique to each test,
/// and these move rows and compare table identities. Taking the gate is what
/// makes `a_second_resolution_with_the_same_catalogue_reuses_the_table`
/// meaningful at all: a sibling resolving in between would rebuild the table
/// and the pointer comparison would fail for a reason that is not the bug.
static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`GATE`], ignoring a poisoning left by an earlier failure — the guard
/// orders these tests, it does not protect an invariant, and turning one
/// failure into four would hide which rung actually broke.
fn serialized() -> std::sync::MutexGuard<'static, ()> {
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Latitude the *volume* states, degrees. A whole degree from everything else.
const VOLUME_LAT: f32 = 30.0;
/// Latitude a *learned* position holds.
const LEARNED_LAT_UDEG: i32 = 31_000_000;
/// Latitude the *network catalogue* holds.
const NETWORK_LAT_UDEG: i32 = 32_000_000;
/// Longitude every candidate shares: only the latitude varies, so a failure
/// names a rung rather than a coordinate.
const LON_UDEG: i32 = -97_000_000;

fn learned() -> SitePosition {
    SitePosition {
        lat_udeg: LEARNED_LAT_UDEG,
        lon_udeg: LON_UDEG,
        site_height_m: 370,
        tower_height_m: 20,
    }
}

fn network() -> SiteFix {
    SiteFix::Network {
        lat_udeg: NETWORK_LAT_UDEG,
        lon_udeg: LON_UDEG,
        feedhorn_m: 400,
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

/// The ranks, as an ordering, before anything is built on top of them.
///
/// Trivial and worth having: `extended` picks the strongest fix per radar by
/// comparing these, so a variant reordering would silently invert the whole
/// ladder while every structural test still passed.
#[test]
fn the_ranks_descend_learned_network_unplaced() {
    let _gate = serialized();
    assert!(SiteFixRank::Learned < SiteFixRank::Network);
    assert!(SiteFixRank::Network < SiteFixRank::Unplaced);
    assert_eq!(SiteFix::Learned(learned()).rank(), SiteFixRank::Learned);
    assert_eq!(network().rank(), SiteFixRank::Network);
    assert_eq!(SiteFix::Unplaced.rank(), SiteFixRank::Unplaced);
}

/// The whole ladder, in one resolution.
///
/// The fixes are supplied **weakest-first** on purpose. A resolution that
/// simply applied them in order — or that let the last one win — would put the
/// bare listing on `ZZPV` and the fetched position on `ZZPT` and `ZZPY`, and
/// this is the row that catches it.
///
/// `ZZPU` is the rung that replaced the seed: a radar every source is silent
/// about. It used to resolve to a compiled-in row; it now resolves to nothing
/// at all, and that is the deletion stated as a behaviour.
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
            f64::from(VOLUME_LAT),
            SitePositionSource::Volume,
        ),
        (
            "and outranks the table with nothing learned",
            scan_stating(VOLUME_LAT),
            None,
            f64::from(VOLUME_LAT),
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

/// A fetched elevation must not take a learned row's base datum away.
///
/// The catalogue has *one* height where a Volume Data Block has two, so a fix
/// that overwrote heights the way it overwrites position would turn every
/// `BaseAndTower` row into a `FeedhornOnly` one — and every
/// [`Datum::SiteBase`] query about that radar would start answering `None` the
/// first time a catalogue landed. Position and heights come from different
/// places and are allowed to move independently.
///
/// The row it lands on is one this test learned, because a learned volume is
/// now the only thing that ever separates a base from a tower.
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
        "and the base datum did not: the catalogue has no tower figure to \
         separate, so it has nothing better to say here",
    );
    assert_eq!(
        row.height_ft(Datum::Feedhorn),
        known.height_ft(Datum::Feedhorn)
    );
}

/// A radar only the catalogue knows still records an elevation.
///
/// The counterweight to the test above: keeping `known` must not become
/// "never write heights". A row with none anchors a cross-section at sea level,
/// which reads as a measurement rather than as a gap.
#[test]
fn an_arrival_only_the_catalogue_knows_takes_its_elevation() {
    let _gate = serialized();
    sites::resolve([("ZZPZ", network())]);

    let row = sites::get_radar_site("ZZPZ").expect("the catalogue placed it");
    // 400 m, the fix's feedhorn, in feet.
    assert_eq!(row.height_ft(Datum::Feedhorn), Some(1312));
    assert_eq!(
        row.height_ft(Datum::SiteBase),
        None,
        "the catalogue cannot separate a tower, so the base is unknown \
         rather than equal to the feedhorn",
    );
    assert!(
        sites::radars().iter().any(|r| r.name == "ZZPZ"),
        "and the walk the map and the site list both do reaches it",
    );
}

/// Resolving twice with the same catalogue must build nothing the second time.
///
/// Android genuinely resolves twice — `App::new` with no store, then
/// `set_config_dir` with one — and both calls hand over the same cached
/// catalogue. Every row leaks, so a second resolution that rebuilt would leak a
/// whole table for a catalogue that had not changed.
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
