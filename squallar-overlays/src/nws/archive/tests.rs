use super::*;

/// The real IEM response for the Moore EF5, trimmed to the three OUN polygon
/// warnings in force as it was on the ground. A captured response and not a
/// hand-written one: the fields this translates are IEM's to name, and a fixture
/// invented here would pin what I assumed rather than what the service sends.
const MOORE: &str = include_str!("moore_2013_sbw.json");

fn moore() -> serde_json::Value {
    serde_json::from_str(MOORE).expect("the captured IEM response parses")
}

/// The translation produces alerts the live parser accepts, with the event
/// names the live feed spells.
#[test]
fn a_captured_iem_response_becomes_named_alerts() {
    let alerts = crate::nws::alert::parse_alerts(&translate(&moore()));
    assert_eq!(alerts.len(), 3, "all three warnings survive translation");

    let events: Vec<&str> = alerts.iter().map(|a| a.event.as_str()).collect();
    assert!(
        events.contains(&"Tornado Warning"),
        "the tornado warning must be spelled as the live feed spells it, got {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == "Severe Thunderstorm Warning")
            .count(),
        2,
        "both severe warnings must translate, got {events:?}"
    );
}

/// Every translated alert carries a polygon.
///
/// Storm-based warnings are the polygon products, which is why
/// `ActiveAlerts::whole` is the right constructor: nothing here waits on zone
/// resolution. An alert with no geometry would be dropped by `parse_alerts` and
/// the count above would catch it, but this says the reason out loud.
#[test]
fn every_translated_warning_carries_its_own_geometry() {
    let alerts = crate::nws::alert::parse_alerts(&translate(&moore()));
    for alert in &alerts {
        assert!(
            !alert.features.is_empty(),
            "{} arrived with no polygon",
            alert.event
        );
        assert!(
            alert.affected_zones.is_empty(),
            "{} references zones; storm-based warnings should not",
            alert.event
        );
    }
}

/// The validity window is the archived one, so the as-of filter keeps these at
/// the instant they were valid and drops them elsewhere.
///
/// This is the whole point of the source: a warning issued at 19:42Z and
/// expiring at 20:15Z must be visible at 19:59Z — the volume the Moore
/// screenshot is pinned to — and invisible an hour later.
#[test]
fn the_archived_window_is_what_the_as_of_filter_sees() {
    let alerts = crate::nws::alert::parse_alerts(&translate(&moore()));
    let tornado = alerts
        .iter()
        .find(|a| a.event == "Tornado Warning")
        .expect("the tornado warning");

    let at = |h, m| {
        chrono::NaiveDate::from_ymd_opt(2013, 5, 20)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    };

    let valid_at = |t| {
        tornado.valid_from.is_none_or(|from| from <= t)
            && tornado.valid_until.is_none_or(|until| t < until)
    };

    assert!(valid_at(at(19, 59)), "must be valid at the pinned volume");
    assert!(
        !valid_at(at(18, 0)),
        "must not be valid before it was issued"
    );
    assert!(!valid_at(at(21, 0)), "must not be valid after it expired");
}

/// A phenomenon this build does not spell is still drawn.
///
/// An unrecognised warning has a polygon and a window like any other, and
/// dropping it would silently shrink the picture. The fallback names it from its
/// own codes rather than inventing a noun.
#[test]
fn an_unknown_phenomenon_is_carried_rather_than_dropped() {
    let mut json = moore();
    json["features"][0]["properties"]["phenomena"] = serde_json::json!("ZZ");
    let alerts = crate::nws::alert::parse_alerts(&translate(&json));
    assert_eq!(alerts.len(), 3, "the unknown one is still an alert");
    assert!(
        alerts.iter().any(|a| a.event.starts_with("ZZ")),
        "it should be named from its codes, got {:?}",
        alerts.iter().map(|a| &a.event).collect::<Vec<_>>()
    );
}

/// A tornado warning is Extreme, which is what the live feed reports and what
/// the colour and sort order key off.
#[test]
fn severity_matches_what_the_live_feed_reports() {
    assert_eq!(severity_of("TO", "W"), "Extreme");
    assert_eq!(severity_of("SV", "W"), "Severe");
    assert_eq!(severity_of("SV", "A"), "Moderate");
}

/// The archive URL names the instant, in the spelling IEM accepts.
#[test]
fn the_url_addresses_the_instant() {
    let sources = squallar_source::origins::DataSources::production();
    let at = chrono::NaiveDate::from_ymd_opt(2013, 5, 20)
        .unwrap()
        .and_hms_opt(19, 59, 0)
        .unwrap();
    let url = sources.nws_alerts_archive_url(at);
    assert!(url.contains("ts=2013-05-20T19:59:00Z"), "{url}");
    assert!(url.contains("sbw.geojson"), "{url}");
}
