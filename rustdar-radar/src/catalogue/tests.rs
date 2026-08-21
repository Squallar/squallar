//! Offline tests for the catalogue's decisions, and `#[ignore]`d live ones for
//! the two round-trips.

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
#[test]
fn the_bucket_decides_which_radars_exist_and_the_nws_decides_where() {
    let positions = positions();
    let catalogue = SiteCatalogue::union(ids(&["KTLX", "TPBI", "KCRI"]), &positions);

    for (case, id, member, placed) in [
        (
            "in both: a member, and placed",
            "KTLX",
            true,
            Some(CataloguePosition {
                lat_udeg: 35_333_340,
                lon_udeg: -97_277_500,
                elevation_m: 370,
                network: Some(RadarNetwork::Wsr88d),
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

/// An unplaced member contributes membership and no position.
#[test]
fn a_member_the_nws_cannot_place_supplies_membership_and_no_position() {
    let catalogue = SiteCatalogue::union(ids(&["TPBI", "KTLX"]), &positions());
    assert_eq!(catalogue.len(), 2, "both are members");

    let fixes: Vec<(&str, SiteFix)> = catalogue.fixes().collect();
    assert_eq!(fixes.len(), 2, "and both have something to say");
    assert_eq!(
        fixes
            .iter()
            .find(|(id, _)| *id == "TPBI")
            .map(|(_, fix)| *fix),
        Some(SiteFix::Unplaced),
        "the one the NWS cannot place says only that it exists",
    );
    assert!(
        matches!(
            fixes.iter().find(|(id, _)| *id == "KTLX"),
            Some((_, SiteFix::Network { .. })),
        ),
        "and the placed one carries its position",
    );

    // The whole point: it is a member the table can list, and never a row.
    let table = crate::sites::build_table(catalogue.fixes());
    assert_eq!(table.unplaced(), ["TPBI"]);
    assert!(table.get("TPBI").is_none());
    assert!(table.get("KTLX").is_some());
}

/// The bucket root can hold anything somebody uploaded, and only a four-byte
/// ICAO is a radar.
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
#[test]
fn a_station_is_placed_only_when_the_record_is_complete_and_in_metres() {
    let placed = positions();

    assert_eq!(
        placed.get("KOAX"),
        Some(&CataloguePosition {
            lat_udeg: 41_320_280,
            lon_udeg: -96_366_830,
            elevation_m: 350,
            network: Some(RadarNetwork::Wsr88d),
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

    assert_eq!(
        placed.len(),
        9,
        "KTLX, KOAX, KRAX and TDJT, plus the five the datum check reads: \
         KMAX, KMKX, KINX, KCRP, KCBW",
    );
}

/// An unreadable body places nothing rather than propagating.
#[test]
fn an_unreadable_station_list_places_nothing() {
    for body in ["", "null", "<html>503</html>", r#"{"features": null}"#] {
        assert!(parse_stations(body).is_empty(), "{body:?}");
    }
}

/// The cached blob round-trips, integers and all.
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

/// A cache written before the elevation field was renamed still loads.
#[test]
fn a_cache_written_under_the_old_field_name_still_loads() {
    let old = r#"{"KTLX":{"lat_udeg":35333340,"lon_udeg":-97277500,"feedhorn_m":370},"TPBI":null}"#;
    let back: SiteCatalogue = serde_json::from_str(old).expect("an old cache still parses");

    assert_eq!(
        back.position("KTLX").map(|p| p.elevation_m),
        Some(370),
        "the old spelling carries the same number onto the renamed field",
    );
    assert!(
        back.contains("TPBI"),
        "and the rest of the blob survives with it",
    );
}

/// A cache written before the network field existed still loads, and loads
/// with the network unknown rather than guessed.
#[test]
fn a_cache_written_before_the_network_was_learned_loads_without_one() {
    let old =
        r#"{"KTLX":{"lat_udeg":35333340,"lon_udeg":-97277500,"elevation_m":370},"TPBI":null}"#;
    let back: SiteCatalogue = serde_json::from_str(old).expect("an old cache still parses");

    assert_eq!(
        back.position("KTLX").map(|p| p.elevation_m),
        Some(370),
        "the position is what a cache is for and it survives untouched",
    );
    assert_eq!(
        back.network("KTLX"),
        None,
        "a cache that never carried a network must not invent one",
    );
    assert!(
        back.contains("TPBI"),
        "and the rest of the blob survives with it",
    );

    // The control: the same blob WITH the field parses it, so the `None` above
    // is the cache's silence and not a field this type cannot read at all.
    let new =
        r#"{"KTLX":{"lat_udeg":35333340,"lon_udeg":-97277500,"elevation_m":370,"network":"Tdwr"}}"#;
    let back: SiteCatalogue = serde_json::from_str(new).expect("a new cache parses");
    assert_eq!(back.network("KTLX"), Some(RadarNetwork::Tdwr));
}

/// **The API is the authority on which network a radar is on; the prefix rule
/// is the offline approximation of it.** Where the station record states a type
/// this build recognises, the two must agree.
#[test]
fn the_prefix_rule_agrees_with_the_api_on_every_placed_station() {
    let placed = positions();

    let mut checked = 0;
    let mut seen: Vec<RadarNetwork> = Vec::new();
    for (id, position) in &placed {
        let Some(stated) = position.network else {
            continue;
        };
        checked += 1;
        if !seen.contains(&stated) {
            seen.push(stated);
        }
        assert_eq!(
            RadarNetwork::of_id(id),
            stated,
            "the station record says {id} is {stated:?} and the identifier rule \
             disagrees -- update of_id's exception list, then re-record the \
             fixture: the API is the authority on network, the prefix rule is \
             the offline approximation",
        );
    }

    // Non-triviality: a fixture that stated one network, or none, would make
    // the walk above pass without comparing anything. Nine is the fixture's
    // whole placed population -- three of its twelve stations are the
    // incomplete records `a_station_is_placed_only_when_the_record_is_complete_and_in_metres`
    // exists for, and an unplaced station has no row to carry a network.
    assert_eq!(checked, 9, "the fixture's placed population moved");
    assert_eq!(
        seen.len(),
        2,
        "the fixture must state both networks or the walk cannot fail: {seen:?}",
    );
}

/// The live bucket root really does answer with the whole network in one page.
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
#[tokio::test]
#[ignore = "hits the live api.weather.gov"]
async fn the_live_station_list_places_the_network_in_metres() {
    let placed = fetch_station_positions(&DataSources::production())
        .await
        .expect("api.weather.gov/radar/stations");
    assert!(placed.len() > 150, "got {}", placed.len());

    // Against a position that came out of the radar's own volume: median
    // 1.5 m, maximum 187 m over the network, nothing past a kilometre.
    crate::sites::fixture::install();
    let ktlx = placed.get("KTLX").expect("KTLX is a station");
    let stated = crate::sites::get_radar_site("KTLX").expect("the fixture places KTLX");
    let metres = crate::sites::distance_km(
        crate::site_position::degrees_from_micro(ktlx.lat_udeg),
        crate::site_position::degrees_from_micro(ktlx.lon_udeg),
        stated.lat,
        stated.lon,
    ) * 1000.0;
    assert!(
        metres < 1000.0,
        "KTLX is {metres:.0} m from where its own volume puts it"
    );
}

/// The union, end to end, against both live endpoints — the one place the
/// counterexamples can be checked rather than asserted.
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
