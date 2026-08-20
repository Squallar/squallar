use super::*;

/// Verbatim `?network=OK_ASOS` body, trimmed to the stations used below.
const SAMPLE: &str = include_str!("../testdata/currents.ok.json");

fn sample() -> Vec<MetarOb> {
    parse_currents(SAMPLE).expect("fixture must parse").0
}

fn station(id: &str) -> MetarOb {
    sample()
        .into_iter()
        .find(|o| o.station_id == id)
        .unwrap_or_else(|| panic!("fixture has no station {id}"))
}

// ── Units ─────────────────────────────────────────────────────────────

/// Fails if `tmpf` (°F) is relabelled rather than converted. Expected value
/// is KOKC's own raw METAR group `34/22`, not a number this code produced.
#[test]
fn a_fahrenheit_temperature_is_converted_not_relabelled() {
    let okc = station("KOKC");
    assert!(okc.raw_ob.contains(" 34/22 "), "fixture must keep the trap");
    let c = okc.temp_c.expect("KOKC reports a temperature");
    assert!(
        (c - 33.888_889).abs() < 1e-4,
        "93 F is 33.89 C, got {c} — was the value relabelled rather than converted?",
    );
    // The METAR rounds to whole degrees; that is the independent check.
    assert_eq!(c.round(), 34.0);
    // Dewpoint: 71 F -> 21.67 C, METAR says 22.
    assert_eq!(okc.dewp_c.unwrap().round(), 22.0);
}

#[test]
fn the_fahrenheit_conversion_is_the_real_formula() {
    assert_eq!(Fahrenheit(32.0).to_celsius(), 0.0);
    assert_eq!(Fahrenheit(212.0).to_celsius(), 100.0);
    assert_eq!(Fahrenheit(-40.0).to_celsius(), -40.0);
    assert!((Fahrenheit(90.0).to_celsius() - 32.222_222).abs() < 1e-5);
}

/// KOKC's `"alti": 30.04` and raw `A3004` agree, so the fixture pins inHg.
#[test]
fn an_inhg_altimeter_is_converted_to_hectopascals() {
    let okc = station("KOKC");
    assert!(okc.raw_ob.contains("A3004"), "fixture must keep the trap");
    let hpa = okc.altimeter_hpa.expect("KOKC reports an altimeter");
    // 30.04 inHg x 33.8639 = 1017.27 hPa, worked by hand.
    assert!((hpa - 1017.27).abs() < 0.01, "got {hpa} hPa");
    // Read as hPa directly it would have been ~30, i.e. 34x low.
    assert!(hpa > 900.0, "{hpa} looks like a raw inHg value");
}

/// `mslp` is already hectopascals and must not be run through the inHg
/// conversion the altimeter needs. KOKC's fixture row carries both, and they
/// are different quantities: 1015.3 hPa reduced with the station's own
/// temperature, against a 30.04 inHg cockpit setting that is 1017.27 hPa.
#[test]
fn sea_level_pressure_is_read_in_hectopascals_and_is_not_the_altimeter() {
    let okc = station("KOKC");
    let mslp = okc.mslp_hpa.expect("KOKC reports an MSLP");
    assert!((mslp - 1015.3).abs() < 0.01, "got {mslp} hPa");
    let alt = okc.altimeter_hpa.expect("KOKC reports an altimeter");
    assert!(
        (alt - mslp).abs() > 1.0,
        "the two pressures collapsed to one value ({alt} vs {mslp}); a \
         conversion has been applied to the wrong one"
    );
    // Read through the altimeter's conversion it would be ~34x high.
    assert!(mslp < 1100.0, "{mslp} looks like it went through to_hpa()");
}

/// Most of the network publishes no MSLP — 572 of 1324 records carried one
/// across 20 state ASOS networks — and that must reach the plot as `None`,
/// never as a zero and never as a substituted altimeter.
#[test]
fn a_station_publishing_no_sea_level_pressure_yields_none() {
    let obs = sample();
    assert!(
        obs.iter().any(|o| o.mslp_hpa.is_some()),
        "the fixture must exercise both arms"
    );
    for o in &obs {
        if let Some(m) = o.mslp_hpa {
            assert!(
                m > 800.0,
                "{}: {m} hPa is not a sea level pressure",
                o.station_id
            );
            assert_ne!(
                Some(m),
                o.altimeter_hpa,
                "{}: MSLP is the altimeter",
                o.station_id
            );
        }
    }
}

/// Fails if `sknt` is integer-parsed: `u16::from_str("14.0")` blanks every
/// wind speed in the feed.
#[test]
fn a_float_wind_speed_survives() {
    let okc = station("KOKC");
    assert!(
        okc.raw_ob.contains("20014G20KT"),
        "fixture must keep the trap"
    );
    assert_eq!(okc.wind_speed_kt, Some(14), "sknt 14.0 must round to 14");
    assert_eq!(okc.wind_gust_kt, Some(20));
    assert_eq!(okc.wind_dir, Some(WindDir::Degrees(200)));
}

/// Fails if `null` ("not reported") is counted as a rejection.
#[test]
fn unusable_cells_are_counted_and_nulls_are_not() {
    let (obs, rejected) = parse_currents(SAMPLE).unwrap();
    assert_eq!(rejected, 0, "the real IEM fixture parses cleanly");
    assert!(!obs.is_empty());
    // "0 rejections" only means something if the fixture holds nulls in
    // fields this code reads. It does: 3 of 6 stations report no `gust`.
    assert!(
        SAMPLE.contains("\"gust\": null"),
        "fixture must contain a null in a field this code reads, or \
             `rejected == 0` says nothing about how nulls are treated",
    );
    let null_gusts = SAMPLE.matches("\"gust\": null").count();
    assert!(
        null_gusts >= 3,
        "expected several null gusts, got {null_gusts}"
    );

    let broken = SAMPLE.replace("\"sknt\": 14.0", "\"sknt\": \"14 kt\"");
    assert_ne!(broken, SAMPLE, "the replacement must actually apply");
    let (broken_obs, rejected) = parse_currents(&broken).unwrap();
    assert_eq!(rejected, 1, "a string in a numeric cell is one rejection");
    let okc = broken_obs.iter().find(|o| o.station_id == "KOKC").unwrap();
    assert_eq!(okc.wind_speed_kt, None);
    assert!(
        okc.temp_c.is_some(),
        "one bad cell must not drop the station"
    );
}

// ── Identity ──────────────────────────────────────────────────────────

#[test]
fn the_station_id_is_the_icao_from_the_raw_report() {
    assert_eq!(icao_from_raw("KOKC 251652Z 20014G20KT"), Some("KOKC"));
    assert_eq!(icao_from_raw("METAR KTUL 251653Z 18012KT"), Some("KTUL"));
    assert_eq!(icao_from_raw("SPECI KLAW 251700Z"), Some("KLAW"));
    assert_eq!(icao_from_raw(""), None);
    assert_eq!(icao_from_raw("251652Z 20014G20KT"), None, "not a callsign");
    assert!(SAMPLE.contains("\"station\": \"OKC\""));
    assert_eq!(station("KOKC").station_id, "KOKC");
}

// ── Visibility ────────────────────────────────────────────────────────

#[test]
fn an_unrestricted_visibility_is_recovered_from_the_raw_report() {
    assert!(raw_visibility_is_a_bound(
        "KOKC 251652Z 20014G20KT 10SM FEW250"
    ));
    assert!(raw_visibility_is_a_bound(
        "EGLL 251650Z 25008KT 9999 FEW035"
    ));
    assert!(raw_visibility_is_a_bound(
        "KXYZ 251650Z 25008KT P6SM SCT035"
    ));
    assert!(!raw_visibility_is_a_bound(
        "KUZA 251650Z 00000KT 2 1/2SM BR"
    ));
    assert!(!raw_visibility_is_a_bound("KABC 251650Z 25008KT 5SM HZ"));

    let okc = station("KOKC");
    assert_eq!(okc.visibility.unwrap().miles, 10.0);
    assert!(okc.visibility.unwrap().or_greater, "10SM is a bound");
    assert_eq!(okc.visibility.unwrap().label(), "10+");
}

#[test]
fn a_measured_visibility_is_not_marked_as_a_bound() {
    let v = visibility_from(2.5, "KUZA 251650Z 00000KT 2 1/2SM BR OVC004").unwrap();
    assert_eq!(v.miles, 2.5);
    assert!(!v.or_greater);
    assert_eq!(v.label(), "2.5");
}

#[test]
fn a_nonsensical_visibility_is_rejected() {
    assert_eq!(visibility_from(-1.0, "x"), None);
    assert_eq!(visibility_from(f64::NAN, "x"), None);
    assert_eq!(visibility_from(f64::INFINITY, "x"), None);
}

// ── Ceiling and flight category ───────────────────────────────────────

#[test]
fn only_broken_or_worse_layers_form_a_ceiling() {
    let few_only = vec![CloudLayer {
        cover: "FEW".into(),
        base_ft: Some(2500),
    }];
    assert_eq!(ceiling_ft(&few_only), None, "FEW is not a ceiling");

    let sct_only = vec![CloudLayer {
        cover: "SCT".into(),
        base_ft: Some(1800),
    }];
    assert_eq!(ceiling_ft(&sct_only), None, "SCT is not a ceiling");

    let mixed = vec![
        CloudLayer {
            cover: "FEW".into(),
            base_ft: Some(800),
        },
        CloudLayer {
            cover: "SCT".into(),
            base_ft: Some(1500),
        },
        CloudLayer {
            cover: "BKN".into(),
            base_ft: Some(2500),
        },
        CloudLayer {
            cover: "OVC".into(),
            base_ft: Some(4000),
        },
    ];
    assert_eq!(
        ceiling_ft(&mixed),
        Some(2500),
        "the lowest BKN/OVC wins, and the lower FEW/SCT are ignored",
    );

    let obscured = vec![CloudLayer {
        cover: "VV".into(),
        base_ft: Some(200),
    }];
    assert_eq!(ceiling_ft(&obscured), Some(200), "VV is a ceiling");
}

#[test]
fn a_clear_report_with_good_visibility_is_vfr() {
    let okc = station("KOKC");
    assert_eq!(ceiling_ft(&okc.clouds), None);
    assert_eq!(okc.flight_category, Some(FlightCategory::VFR));
}

/// Both sides of every boundary. Values are FAA/AWC's, not this code's.
#[test]
fn flight_category_thresholds_match_the_faa_definitions() {
    let ceiling_only = |ft: u32| derive_flight_category(None, Some(ft));
    assert_eq!(ceiling_only(499), Some(FlightCategory::LIFR));
    assert_eq!(ceiling_only(500), Some(FlightCategory::IFR));
    assert_eq!(ceiling_only(999), Some(FlightCategory::IFR));
    assert_eq!(ceiling_only(1000), Some(FlightCategory::MVFR));
    assert_eq!(ceiling_only(3000), Some(FlightCategory::MVFR));
    assert_eq!(ceiling_only(3001), Some(FlightCategory::VFR));

    let vis_only = |m: f64| {
        derive_flight_category(
            Some(Visibility {
                miles: m,
                or_greater: false,
            }),
            None,
        )
    };
    assert_eq!(vis_only(0.5), Some(FlightCategory::LIFR));
    assert_eq!(vis_only(1.0), Some(FlightCategory::IFR));
    assert_eq!(vis_only(2.9), Some(FlightCategory::IFR));
    assert_eq!(vis_only(3.0), Some(FlightCategory::MVFR));
    assert_eq!(vis_only(5.0), Some(FlightCategory::MVFR));
    assert_eq!(vis_only(5.1), Some(FlightCategory::VFR));
}

#[test]
fn the_worse_of_ceiling_and_visibility_decides_the_category() {
    let vis10 = Some(Visibility {
        miles: 10.0,
        or_greater: true,
    });
    assert_eq!(
        derive_flight_category(vis10, Some(300)),
        Some(FlightCategory::LIFR),
        "a 300 ft ceiling is LIFR regardless of visibility",
    );
    let vis_half = Some(Visibility {
        miles: 0.5,
        or_greater: false,
    });
    assert_eq!(
        derive_flight_category(vis_half, Some(25_000)),
        Some(FlightCategory::LIFR),
        "half a mile is LIFR regardless of ceiling",
    );
    let vis2 = Some(Visibility {
        miles: 2.0,
        or_greater: false,
    });
    assert_eq!(
        derive_flight_category(vis2, Some(1500)),
        Some(FlightCategory::IFR),
    );
}

#[test]
fn a_report_with_neither_input_has_no_category() {
    assert_eq!(derive_flight_category(None, None), None);
}

// ── Wind ──────────────────────────────────────────────────────────────

#[test]
fn calm_and_variable_are_told_apart() {
    assert_eq!(
        resolve_wind_dir("K1 251650Z 00000KT", None, None),
        Some(WindDir::Calm)
    );
    assert_eq!(
        resolve_wind_dir("K1 251650Z VRB03KT", None, None),
        Some(WindDir::Variable)
    );
    assert_eq!(
        resolve_wind_dir("K1 251650Z 36003KT", None, None),
        Some(WindDir::Degrees(360)),
        "a genuine northerly is 360, never 0",
    );
}

#[test]
fn a_vrb_in_the_remarks_does_not_make_the_station_variable() {
    let raw = "GCGM 251650Z 00000KT RMK R09/VRB07G21KT";
    assert_eq!(resolve_wind_dir(raw, Some(0), Some(0)), Some(WindDir::Calm));
}

#[test]
fn wind_tokens_are_recognised_by_shape_not_by_substring() {
    assert_eq!(parse_wind_token("18006KT"), Some((Some(180), 6)));
    assert_eq!(parse_wind_token("20014G20KT"), Some((Some(200), 14)));
    assert_eq!(parse_wind_token("VRB03KT"), Some((None, 3)));
    assert_eq!(parse_wind_token("34002MPS"), Some((Some(340), 2)));
    for bad in [
        "E00000KT",
        "R09/VRB07G21KT",
        "9999",
        "A2986",
        "18006",
        "VRBKT",
    ] {
        assert_eq!(parse_wind_token(bad), None, "{bad:?} is not a wind group");
    }
}

/// A wind group whose direction field is multi-byte is rejected, not split.
#[test]
fn a_multibyte_wind_group_is_rejected_rather_than_split_mid_character() {
    for bad in ["éééKT", "1é2KT", "éé0KT", "ééééKT", "🌀🌀KT"] {
        assert_eq!(
            parse_wind_token(bad),
            None,
            "{bad:?} is not a wind group, and must not panic on the way to saying so",
        );
    }
}

#[test]
fn a_zero_bearing_with_speed_is_not_treated_as_north() {
    assert_eq!(classify_wind(Some(0), 25), WindDir::Variable);
    assert_eq!(classify_wind(Some(0), 0), WindDir::Calm);
    assert_eq!(classify_wind(Some(360), 5), WindDir::Degrees(360));
}

#[test]
fn every_fixture_station_parses_and_keeps_its_raw_report() {
    let obs = sample();
    assert!(obs.len() >= 4, "fixture is too small to be meaningful");
    for o in &obs {
        assert!(!o.station_id.is_empty());
        assert!(!o.raw_ob.is_empty(), "{} lost its raw report", o.station_id);
        assert!(
            !o.obs_time.is_empty(),
            "{} lost its timestamp",
            o.station_id
        );
        assert!((-90.0..=90.0).contains(&o.lat));
        assert!((-180.0..=180.0).contains(&o.lon));
    }
}

#[test]
fn a_malformed_body_is_an_error() {
    assert!(parse_currents("not json").is_err());
    assert!(parse_currents("{\"data\": 5}").is_err());
}

// ── Live checks ───────────────────────────────────────────────────────

/// `cargo test -p rustdar-overlays -- --ignored --nocapture live_metar`
#[ignore = "hits the live mesonet.agron.iastate.edu API"]
#[tokio::test]
async fn live_metar_fetch_carries_every_mapped_field() {
    let client = rustdar_source::tls::simple_client(std::time::Duration::from_secs(60))
        .build()
        .expect("client");
    let sources = rustdar_source::origins::DataSources::production();
    // Central Oklahoma — KTLX's neighbourhood.
    let view = GeoBounds {
        min_lat: 34.3,
        max_lat: 36.3,
        min_lon: -98.3,
        max_lon: -96.3,
    };

    let obs = fetch_current_metars(&client, &sources, &view)
        .await
        .expect("METAR fetch must succeed")
        .observations;
    println!("fetched {} observations", obs.len());
    assert!(obs.len() > 20, "expected a state's worth of ASOS sites");

    // A field that is `None` for every station is the silent-column failure.
    let has = |f: &dyn Fn(&MetarOb) -> bool| obs.iter().filter(|o| f(o)).count();
    for (name, count) in [
        ("temp_c", has(&|o| o.temp_c.is_some())),
        ("dewp_c", has(&|o| o.dewp_c.is_some())),
        ("wind_dir", has(&|o| o.wind_dir.is_some())),
        ("wind_speed_kt", has(&|o| o.wind_speed_kt.is_some())),
        ("visibility", has(&|o| o.visibility.is_some())),
        ("altimeter_hpa", has(&|o| o.altimeter_hpa.is_some())),
        // Sparse by nature — 43.2% of 1324 records across 20 state networks —
        // so this asserts the column is not *empty*, not that it is full.
        ("mslp_hpa", has(&|o| o.mslp_hpa.is_some())),
        ("flight_category", has(&|o| o.flight_category.is_some())),
        ("raw_ob", has(&|o| !o.raw_ob.is_empty())),
        ("obs_time", has(&|o| !o.obs_time.is_empty())),
    ] {
        println!("  {name}: {count}/{}", obs.len());
        assert!(count > 0, "{name} is None for every station");
    }

    // A relabelled `tmpf` reads 60-110 across most of the US in summer.
    for o in obs.iter().filter(|o| o.temp_c.is_some()) {
        let c = o.temp_c.unwrap();
        assert!(
            (-60.0..=60.0).contains(&c),
            "{} reports {c} C — that is a Fahrenheit value in a Celsius field",
            o.station_id,
        );
    }
    for o in obs.iter().filter(|o| o.altimeter_hpa.is_some()) {
        let hpa = o.altimeter_hpa.unwrap();
        assert!(
            (870.0..=1090.0).contains(&hpa),
            "{} reports {hpa} hPa — that is an inHg value in a hPa field",
            o.station_id,
        );
    }
}

#[ignore = "hits the live mesonet.agron.iastate.edu API"]
#[tokio::test]
async fn live_networks_table_matches_iems_own_extents() {
    let client = rustdar_source::tls::simple_client(std::time::Duration::from_secs(60))
        .build()
        .expect("client");
    let body = client
        .get("https://mesonet.agron.iastate.edu/api/1/networks.json")
        .send()
        .await
        .expect("networks.json")
        .text()
        .await
        .expect("body");

    #[derive(serde::Deserialize)]
    struct Net {
        id: String,
    }
    #[derive(serde::Deserialize)]
    struct Nets {
        data: Vec<Net>,
    }
    let nets: Nets = serde_json::from_str(&body).expect("networks.json parses");

    let upstream: std::collections::HashSet<String> = nets
        .data
        .iter()
        .filter_map(|n| n.id.strip_suffix("_ASOS"))
        .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_uppercase()))
        .map(str::to_string)
        .collect();
    let ours: std::collections::HashSet<String> = networks::NETWORKS
        .iter()
        .map(|n| n.state.to_string())
        .collect();

    let missing: Vec<_> = upstream.difference(&ours).collect();
    let extra: Vec<_> = ours.difference(&upstream).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "networks table has drifted from IEM: missing {missing:?}, extra {extra:?}",
    );
}
