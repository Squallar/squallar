//! Offline tests for the catalogue's decisions, and `#[ignore]`d live ones for
//! the two round-trips.
//!
//! Everything that decides anything is pure, so almost all of this runs with no
//! socket: the union rule, the identifier filter, the station parse and the
//! cache round-trip. The live tests exist because a fixture cannot notice that
//! an endpoint moved, and they are `#[ignore]`d because CI is hermetic — each
//! one names its own invocation.

use super::*;

/// The response captured from `api.weather.gov/radar/stations`, trimmed to the
/// rows these tests need and to the awkward shapes the live body contains.
const STATIONS: &str = include_str!("../../testdata/nws_radar_stations.json");

fn positions() -> BTreeMap<String, CataloguePosition> {
    parse_stations(STATIONS)
}

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

/// **The union is a union.** The bucket decides membership; the NWS decides
/// position; neither decides both.
///
/// One table rather than three tests, because the property is the *rule* and a
/// rule asserted one case at a time is a rule nobody has checked. The three
/// rows are the three live counterexamples that made the union necessary — the
/// identifiers are real and so is each one's asymmetry.
#[test]
fn the_bucket_decides_which_radars_exist_and_the_nws_decides_where() {
    let positions = positions();
    // `TPBI` and `KCRI` are in the bucket and not in the fixture, exactly as
    // they are in the bucket and 404 from the live API. `TDJT` is the mirror
    // image: in the fixture, and not in the bucket.
    let catalogue = SiteCatalogue::union(ids(&["KTLX", "TPBI", "KCRI"]), &positions);

    for (case, id, member, placed) in [
        (
            "in both: a member, and placed",
            "KTLX",
            true,
            Some(CataloguePosition {
                lat_udeg: 35_333_340,
                lon_udeg: -97_277_500,
                feedhorn_m: 370,
            }),
        ),
        (
            "bucket only, renamed at the NWS: still a member, unplaced",
            "TPBI",
            true,
            None,
        ),
        (
            "bucket only, never at the NWS: still a member, unplaced",
            "KCRI",
            true,
            None,
        ),
        (
            "NWS only, no archive data: not a member at all",
            "TDJT",
            false,
            None,
        ),
    ] {
        assert_eq!(catalogue.contains(id), member, "{case}");
        assert_eq!(catalogue.position(id), placed, "{case}");
    }

    assert_eq!(catalogue.len(), 3, "membership is exactly the bucket's");
}

/// The half of the union rule that is easiest to lose: an identifier the NWS
/// has and the archive does not must not reach the site table.
///
/// `contains` above says it is not a *member*; this says the same thing where
/// it would be felt — a radar the user could pick, centre on and then see
/// nothing for, forever, because no volume for it exists.
#[test]
fn an_nws_only_radar_never_becomes_selectable() {
    let catalogue = SiteCatalogue::union(ids(&["KTLX"]), &positions());
    let fixed: Vec<&str> = catalogue.fixes().map(|(id, _)| id).collect();
    assert_eq!(fixed, ["KTLX"]);
    assert!(
        !fixed.contains(&"TDJT"),
        "TDJT is listed by the NWS and has no archive data at all; a fix for \
         it would put a radar on the map that can never show anything",
    );
}

/// An unplaced member contributes nothing rather than a placeholder.
///
/// The alternative — a fix at (0, 0), or at the bucket's idea of nowhere —
/// would be persisted, would outlive the fetch that produced it, and would draw
/// a marker in the Gulf of Guinea.
#[test]
fn a_member_the_nws_cannot_place_supplies_no_fix() {
    let catalogue = SiteCatalogue::union(ids(&["TPBI", "KTLX"]), &positions());
    assert_eq!(catalogue.len(), 2, "both are members");
    assert_eq!(
        catalogue.fixes().count(),
        1,
        "but only the placed one has anything to say",
    );
}

/// The bucket root can hold anything somebody uploaded, and only a four-byte
/// ICAO is a radar.
///
/// Without the filter a stray prefix becomes a row with a name, a marker and a
/// place in the site list — and, once cached, one that survives restarts.
#[test]
fn only_four_byte_identifiers_survive_the_bucket_listing() {
    let parsed = parse_bucket_ids(&[
        "KTLX/".to_owned(),
        "TPBI/".to_owned(),
        // Everything below is a shape the root has held or could hold.
        "logs/".to_owned(),
        "index.html".to_owned(),
        "KTL/".to_owned(),
        "KTLXX/".to_owned(),
        "ktlx/".to_owned(),
        "K-LX/".to_owned(),
        // A duplicate must file one radar, not two.
        "KTLX/".to_owned(),
    ]);
    assert_eq!(parsed, ["KTLX", "TPBI"]);
}

/// The station parse, against the captured body.
///
/// The skips are the interesting half and each one is a different reason: a
/// null elevation, an elevation in feet, and no geometry. All three leave the
/// radar *unplaced* rather than placed at a default, which is a state the union
/// already has a meaning for.
#[test]
fn a_station_is_placed_only_when_the_record_is_complete_and_in_metres() {
    let placed = positions();

    assert_eq!(
        placed.get("KOAX"),
        Some(&CataloguePosition {
            lat_udeg: 41_320_280,
            lon_udeg: -96_366_830,
            feedhorn_m: 350,
        }),
        "GeoJSON coordinates are [lon, lat]; a swap puts Omaha in the \
         Indian Ocean",
    );

    for (case, id) in [
        ("a null elevation", "PAPD"),
        (
            "an elevation in feet, which would read 2.3x too high",
            "KFTG",
        ),
        ("no geometry at all", "KABR"),
    ] {
        assert!(!placed.contains_key(id), "{case}: {id} must stay unplaced");
    }

    assert_eq!(placed.len(), 4, "KTLX, KOAX, KRAX and TDJT");
}

/// An unreadable body places nothing rather than propagating.
///
/// The caller's fallback is the cache plus the seed, and it is the same for a
/// truncated response, an HTML error page and a schema change.
#[test]
fn an_unreadable_station_list_places_nothing() {
    for body in ["", "null", "<html>503</html>", r#"{"features": null}"#] {
        assert!(parse_stations(body).is_empty(), "{body:?}");
    }
}

/// The cached blob round-trips, integers and all.
///
/// The encoding is what the cache is: a float here would be written as `null`
/// by `serde_json` the one time it went non-finite and would then fail to
/// deserialize on the *next* load, costing every position rather than one.
/// An unplaced member has to survive too — it is the difference between the
/// union and an intersection.
#[test]
fn the_catalogue_round_trips_through_its_persisted_form() {
    let catalogue = SiteCatalogue::union(ids(&["KTLX", "TPBI", "KOAX"]), &positions());
    let json = serde_json::to_string(&catalogue).expect("integers serialize");
    assert!(
        !json.contains("null,") || json.contains(r#""TPBI":null"#),
        "the unplaced member is what `null` means here: {json}",
    );
    let back: SiteCatalogue = serde_json::from_str(&json).expect("and deserialize");
    assert_eq!(back, catalogue);
    assert!(back.contains("TPBI"), "including the unplaced member");
    assert_eq!(back.position("KTLX"), catalogue.position("KTLX"));

    // Stable across runs, so the blob is diffable and a test can compare it.
    let again = serde_json::to_string(&SiteCatalogue::union(
        ids(&["KOAX", "TPBI", "KTLX"]),
        &positions(),
    ))
    .expect("integers serialize");
    assert_eq!(json, again, "insertion order must not reach the blob");
}

/// The live bucket root really does answer with the whole network in one page.
///
/// ```text
/// cargo test -p rustdar-radar --all-features -- --ignored the_live_bucket_root
/// ```
#[tokio::test]
#[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
async fn the_live_bucket_root_lists_every_site_in_one_request() {
    let ids = fetch_bucket_ids(&DataSources::production())
        .await
        .expect("the bucket root is one delimited listing");
    assert!(
        ids.len() > 150,
        "the WSR-88D network alone is ~160 radars; got {}",
        ids.len(),
    );
    assert!(ids.contains(&"KTLX".to_owned()));
    assert!(
        ids.iter().all(|id| id.len() == 4),
        "the filter must leave only ICAOs: {ids:?}",
    );
}

/// The live station list really does place the network, and really is the
/// thing the fixture was captured from.
///
/// ```text
/// cargo test -p rustdar-radar --all-features -- --ignored the_live_station_list
/// ```
#[tokio::test]
#[ignore = "hits the live api.weather.gov"]
async fn the_live_station_list_places_the_network_in_metres() {
    let placed = fetch_station_positions(&DataSources::production())
        .await
        .expect("api.weather.gov/radar/stations");
    assert!(placed.len() > 150, "got {}", placed.len());

    // Against the seed, which was read out of the volumes themselves: median
    // 1.5 m, maximum 187 m, nothing past a kilometre.
    let ktlx = placed.get("KTLX").expect("KTLX is a station");
    let seed = crate::sites::get_radar_site("KTLX").expect("KTLX is a seed row");
    let metres = crate::sites::distance_km(
        crate::site_position::degrees_from_micro(ktlx.lat_udeg),
        crate::site_position::degrees_from_micro(ktlx.lon_udeg),
        seed.lat,
        seed.lon,
    ) * 1000.0;
    assert!(metres < 1000.0, "KTLX is {metres:.0} m from its seeded row");
}

/// The union, end to end, against both live endpoints — the one place the
/// counterexamples can be checked rather than asserted.
///
/// ```text
/// cargo test -p rustdar-radar --all-features -- --ignored the_live_catalogue
/// ```
#[tokio::test]
#[ignore = "hits the live unidata-nexrad-level2-chunks bucket and api.weather.gov"]
async fn the_live_catalogue_is_neither_source_on_its_own() {
    let catalogue = fetch(&DataSources::production())
        .await
        .expect("both halves");
    assert!(catalogue.len() > 150);
    assert!(
        catalogue.fixes().count() > 100,
        "most members should be placed",
    );
    assert!(
        catalogue.contains("KTLX") && catalogue.position("KTLX").is_some(),
        "the ordinary case",
    );
}
