use super::*;
use nexrad_model::meta::Site;

/// A Volume Data Block as `crate::scan::decoded` builds one.
fn volume_site(lat: f32, lon: f32, site_height_m: i16, tower_height_m: u16) -> Site {
    Site::new(*b"KTLX", lat, lon, site_height_m, tower_height_m)
}

/// The encoding itself, asserted so that nobody can "simplify" it back to
/// `f64`.
///
/// This is not a restatement of the struct definition. `serde_json` writes a
/// non-finite float as `null` and then *fails to read `null` back*, so a
/// single `f64` field is enough to destroy the whole persisted record on the
/// next load — a run after the bug, with nothing pointing at the cause. An
/// `i32` has no non-finite values, so the failure class is closed by
/// construction.
///
/// `is_i64()` is the assertion that fails if a field becomes a float:
/// `serde_json` would then emit `35.33306`, which is a `Number` but not an
/// integer one.
#[test]
fn every_field_is_encoded_as_an_integer_not_a_float() {
    let pos = SitePosition::from_volume(&volume_site(35.33306, -97.2775, 370, 20))
        .expect("a real position");
    let json = serde_json::to_value(pos).expect("integers always serialize");
    for field in ["lat_udeg", "lon_udeg", "site_height_m", "tower_height_m"] {
        let value = json.get(field).unwrap_or_else(|| panic!("no {field}"));
        assert!(
            value.is_i64(),
            "{field} serialized as {value}, which is not an integer — a float \
             here writes `null` for a non-finite value and fails the next load",
        );
    }
    // And the text really has no floating point in it, which is the property
    // the next load depends on.
    let text = serde_json::to_string(&pos).expect("integers always serialize");
    assert!(!text.contains('.'), "{text}");
    assert!(!text.contains("null"), "{text}");
    assert_eq!(
        serde_json::from_str::<SitePosition>(&text).expect("round-trips"),
        pos,
    );
}

/// A volume that states nothing usable states nothing, rather than being
/// encoded into an `i32` that looks like a measurement.
#[test]
fn a_position_that_is_not_a_place_is_refused_rather_than_encoded() {
    for (name, lat, lon) in [
        ("nan latitude", f32::NAN, -97.2775),
        ("nan longitude", 35.33306, f32::NAN),
        ("infinite latitude", f32::INFINITY, -97.2775),
        ("negative infinite longitude", 35.33306, f32::NEG_INFINITY),
        ("latitude off the planet", 91.0, -97.2775),
        ("longitude off the planet", 35.33306, 181.0),
        // A zeroed Volume Data Block, not a radar in the Gulf of Guinea.
        ("null island", 0.0, 0.0),
    ] {
        assert_eq!(
            SitePosition::from_volume(&volume_site(lat, lon, 370, 20)),
            None,
            "{name} was encoded",
        );
    }

    // The boundary the range check must *not* refuse: a real site right on it.
    assert!(SitePosition::from_volume(&volume_site(90.0, 180.0, 0, 20)).is_some());
    // And zero in one coordinate alone is a real place — the Greenwich
    // meridian runs through England and the equator through Kenya.
    assert!(SitePosition::from_volume(&volume_site(0.0, -97.2775, 370, 20)).is_some());
}

/// Micro-degrees are finer than the `f32` the volume states, so the round trip
/// through them loses nothing that was there.
///
/// The tolerance is one micro-degree — 0.11 m — against a 43 m re-survey step
/// and a 250 m data cell.
#[test]
fn the_encoding_is_finer_than_the_f32_it_encodes() {
    for (lat, lon) in [
        (35.33306f32, -97.2775f32),
        (45.45583, -98.41333),
        (21.13389, -157.18),
        (-14.33306, -170.7125),
    ] {
        let pos = SitePosition::from_volume(&volume_site(lat, lon, 370, 20)).expect("a place");
        assert!(
            (pos.lat() - f64::from(lat)).abs() <= 1e-6,
            "{lat} came back as {}",
            pos.lat(),
        );
        assert!(
            (pos.lon() - f64::from(lon)).abs() <= 1e-6,
            "{lon} came back as {}",
            pos.lon(),
        );
    }
}

/// The learned value and the volume it was learned from are **bit-identical**,
/// not merely close.
///
/// This is what keeps the 1:1-on-reopen rule true across a restart: a pane
/// that rendered from the volume in one session and from the cache in the next
/// divides the same integer both times, so it lands on the same pixel rather
/// than a sub-metre away from it.
#[test]
fn a_learned_position_reproduces_the_volumes_own_to_the_bit() {
    let from_volume =
        SitePosition::from_volume(&volume_site(35.33306, -97.2775, 370, 20)).expect("a place");
    let text = serde_json::to_string(&from_volume).expect("serializes");
    let reloaded: SitePosition = serde_json::from_str(&text).expect("round-trips");

    assert_eq!(reloaded, from_volume);
    assert_eq!(reloaded.lat().to_bits(), from_volume.lat().to_bits());
    assert_eq!(reloaded.lon().to_bits(), from_volume.lon().to_bits());
}

/// A table figure the volume cannot contradict is kept, because it is the
/// finer expression of the same metre.
///
/// `KABR` records 1302 ft, which is 396.85 m; its volume truncates to 396 m.
/// Replacing the row with `feet(396)` would move it 3 ft for no reason, and
/// would do so to 206 rows at once.
#[test]
fn a_foot_figure_the_volume_cannot_contradict_survives() {
    let table = SiteHeights::BaseAndTower {
        base_ft: 1302,
        tower_ft: 79,
    };
    let pos = SitePosition::from_volume(&volume_site(45.45583, -98.41333, 396, 24)).expect("place");
    assert_eq!(pos.heights_over(Some(table)), table);
}

/// …and one it *does* contradict is replaced.
///
/// `RKSG` is the case: the row carried Osan's 131 ft while its volumes report
/// Camp Humphreys' 439 m, 423 m away. That is the whole point of reading the
/// volume — a relocated radar, not a mis-transcribed one.
#[test]
fn a_foot_figure_the_volume_contradicts_is_replaced() {
    let stale = SiteHeights::BaseAndTower {
        base_ft: 131,
        tower_ft: 79,
    };
    let pos = SitePosition::from_volume(&volume_site(37.20757, 127.28556, 439, 24)).expect("place");
    assert_eq!(
        pos.heights_over(Some(stale)),
        SiteHeights::BaseAndTower {
            base_ft: 1440,
            tower_ft: 79,
        },
        "the base must move onto the volume and the tower must not",
    );
}

/// A site the table has never heard of gets both heights from its volume.
#[test]
fn a_site_with_no_row_takes_its_heights_from_its_volume() {
    let pos = SitePosition::from_volume(&volume_site(35.33306, -97.2775, 370, 24)).expect("place");
    assert_eq!(
        pos.heights_over(None),
        SiteHeights::BaseAndTower {
            base_ft: 1214,
            tower_ft: 79,
        },
    );
}

/// Equal heights are a TDWR reporting one feedhorn figure, and the base is
/// unknown rather than equal to it.
#[test]
fn a_volume_whose_two_heights_are_equal_reports_one_feedhorn() {
    let pos = SitePosition::from_volume(&volume_site(35.2760, -97.5100, 411, 411)).expect("place");
    let heights = pos.heights_over(None);
    assert_eq!(heights, SiteHeights::FeedhornOnly { feedhorn_ft: 1348 });

    // The table's finer figure for the same metre still wins.
    assert_eq!(
        pos.heights_over(Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1351 })),
        SiteHeights::FeedhornOnly { feedhorn_ft: 1351 },
    );

    // And the shape follows the volume even when the row disagrees about it.
    assert_eq!(
        pos.heights_over(Some(SiteHeights::BaseAndTower {
            base_ft: 1351,
            tower_ft: 79,
        })),
        SiteHeights::FeedhornOnly { feedhorn_ft: 1348 },
        "a volume reporting one height must not be given a tower it did not \
         report",
    );
}

/// A row keeps its name and nothing else: the name is the one thing a volume
/// cannot supply, and everything else it supplies is measured.
#[test]
fn applying_a_position_keeps_the_rows_name_and_replaces_its_geometry() {
// The radars this renders against; there are none until a test asks.
crate::sites::fixture::install();
    let row = crate::sites::get_radar_site("KTLX").expect("in the table");
    let pos = SitePosition::from_volume(&volume_site(35.4, -97.3, 439, 24)).expect("place");

    let moved = pos.applied_to(Some(row));
    assert_eq!(moved.name, "KTLX");
    // Bit-identical to the position, not merely near it: `applied_to` must not
    // introduce a second spelling of the same place.
    assert_eq!(moved.lat.to_bits(), pos.lat().to_bits());
    assert_eq!(moved.lon.to_bits(), pos.lon().to_bits());
    assert!((moved.lat - 35.4).abs() < 1e-5, "{}", moved.lat);
    assert!((moved.lon + 97.3).abs() < 1e-5, "{}", moved.lon);
    assert_ne!(
        moved.lat, row.lat,
        "the row's own latitude must not survive"
    );

    let nameless = pos.applied_to(None);
    assert_eq!(nameless.name, crate::sites::UNKNOWN_SITE_NAME);
    assert_eq!(nameless.lat.to_bits(), pos.lat().to_bits());
}
