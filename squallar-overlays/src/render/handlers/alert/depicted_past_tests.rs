//! **What a pane depicting the past puts on the glass.**
//!
//! The gate here is the *status line* and not the store behind it, because the
//! store was never the thing that failed. On 2026-08-31 a pane with a playing
//! loop read `3 shown` against a live feed holding 466, and every layer below
//! the glass was healthy while it did: the fetch returned `Ok`, the zone pack
//! installed, `926/926` zone geometries resolved, the raster dispatched and the
//! texture landed. The layer that lied was the *choice of source* — and only
//! the count a human reads off the panel can tell that apart from a quiet day.
//!
//! # Why the archive half is unreachable from this process, and why that is
//! the sharpest possible fixture
//!
//! `fetch_archived_alerts` builds its own client through
//! [`DataSources::iem_client`](squallar_source::origins::DataSources::iem_client),
//! which is `https_only(true)` — IEM answers `OPTIONS` with `405`, so that half
//! must not carry a `User-Agent`, and the builder that drops it also refuses
//! cleartext. A loopback stub is `http://127.0.0.1`, so the archive request
//! cannot leave this process however the origin is pointed.
//!
//! That is not a hole in the fixture; it is the whole lever. A pane depicting
//! the past, with the archive silent and the live feed answering, reads
//! **`0 shown`** under the substituting policy and **`6 shown`** under the
//! union — because the substituting policy never asked the live feed at all.
//! The stub's request counter states that second fact independently of the
//! panel text.

use super::*;
use crate::nws::fetch::ActiveAlerts;
use crate::render::overlay_state::FetchConfig;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Serve canned bodies keyed on the path with the query string **stripped**.
/// Every accepted connection is counted, and every response closes, so the
/// count is a count of requests. An unrouted path answers 500, so a test states
/// only what it means to succeed.
fn serve(routes: HashMap<String, String>) -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::Relaxed);
            let mut scratch = [0u8; 8192];
            let read = stream.read(&mut scratch).unwrap_or(0);
            let request = String::from_utf8_lossy(&scratch[..read]);
            let target = request.split_whitespace().nth(1).unwrap_or("");
            let path = target.split('?').next().unwrap_or("");
            let (code, body) = match routes.get(path) {
                Some(body) => (200, body.clone()),
                None => (500, "upstream is unwell".to_string()),
            };
            let response = format!(
                "HTTP/1.1 {code} .\r\nContent-Type: application/geo+json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

/// `tls::init()` is required even for a cleartext URL: with `rustls-no-provider`
/// and `aws-lc-rs` out of the graph, `build()` panics without a provider
/// whatever scheme is used.
fn loopback_client() -> reqwest::Client {
    squallar_source::tls::init();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

/// One `/alerts/active` row, carrying its own polygon so the round asks zone
/// resolution for nothing and the stub needs no zone routes.
fn live_feature(id: &str, event: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Feature",
        "geometry": {
            "type": "Polygon",
            "coordinates": [[[-97.0, 35.0], [-97.0, 35.5], [-96.5, 35.5], [-97.0, 35.0]]],
        },
        "properties": {
            "id": id,
            "event": event,
            "severity": "Severe",
            "urgency": "Immediate",
            "certainty": "Observed",
            "effective": "2020-01-01T00:00:00+00:00",
            "expires": "2099-01-01T00:00:00+00:00",
            "description": "",
            "areaDesc": "",
            "senderName": "",
        },
    })
}

/// The live feed's six rows, across all four categories — the shape the archive
/// cannot supply, since `sbw.geojson` is polygon warnings and nothing else.
fn live_routes() -> (HashMap<String, String>, usize) {
    let live = vec![
        live_feature("urn:live:1", "Tornado Warning"),
        live_feature("urn:live:2", "Severe Thunderstorm Watch"),
        live_feature("urn:live:3", "Small Craft Advisory"),
        live_feature("urn:live:4", "Heat Advisory"),
        live_feature("urn:live:5", "Gale Warning"),
        live_feature("urn:live:6", "Special Weather Statement"),
    ];
    let n = live.len();
    let body = serde_json::json!({ "type": "FeatureCollection", "features": live }).to_string();
    (HashMap::from([("/alerts/active".to_string(), body)]), n)
}

fn config_at(base: &str, as_of: chrono::NaiveDateTime) -> FetchConfig {
    let mut sources = squallar_source::origins::DataSources::production();
    sources.nws_api_base = base.to_string().into();
    sources.iem_base = base.to_string().into();
    FetchConfig {
        client: loopback_client(),
        // `None`, so `ensure_installed` finds no pack source and makes no
        // request: every fixture row above carries its own polygon, so zone
        // resolution has nothing to ask for either. The stub therefore serves
        // exactly the origin under test and nothing else.
        zone_cache_dir: None,
        sources,
        viewport: None,
        as_of,
        depicted_span_secs: Some(14_400),
        depicted_frames: Vec::new(),
    }
}

/// Drive the handler's own fetch task to completion and hand the payload to the
/// handler, exactly as the app's fetch drain does.
fn round(config: &FetchConfig) -> NwsAlertHandler {
    let mut handler = NwsAlertHandler::new();
    let mut tasks = handler.create_fetch_tasks(config, &PaneRef::across(&[]));
    assert_eq!(tasks.len(), 1, "the layer issues one fetch task per round");
    let payload = runtime().block_on(tasks.remove(0).future);
    handler.apply_fetch_result(payload, &PaneRef::across(&[]));
    handler
}

fn shown(handler: &NwsAlertHandler) -> String {
    handler
        .status_line(&PaneRef::across(&[]))
        .expect("the layer has a status line with categories enabled")
}

fn four_hours_back() -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc() - chrono::Duration::hours(4)
}

/// **THE GATE. A pane depicting the past must not lose the live feed.**
///
/// Red at `61dbfba3`, where `create_fetch_tasks` returned the archive task
/// *instead of* the live one: the live feed was never asked (`0` requests), the
/// archive could not answer, the round errored, and the panel read `0 shown`
/// over six alerts the pane could have drawn. The live fault was the same shape
/// with the archive answering — `3 shown` against 466 — and the same assertion
/// reddens on both, because both are the live feed missing from the picture.
///
/// Two assertions, deliberately: the count on the glass is what a human reads,
/// and the request count says *why* it moved without depending on any body the
/// stub happened to serve.
#[test]
fn a_pane_depicting_the_past_still_shows_the_live_feed() {
    let (routes, live_n) = live_routes();
    let (base, requests) = serve(routes);
    let as_of = four_hours_back();

    assert!(
        as_of
            < chrono::Utc::now().naive_utc()
                - chrono::Duration::minutes(super::ARCHIVE_CUTOFF_MINUTES),
        "premise: this instant is behind the cutoff, so the archive is in play at all",
    );

    let handler = round(&config_at(&base, as_of));

    assert_eq!(
        requests.load(Ordering::Relaxed),
        1,
        "a pane depicting the past never asked the live alerts feed",
    );
    assert_eq!(
        shown(&handler),
        format!("{live_n} shown - W/Wa/Adv/Oth"),
        "a depicted past dropped the live feed; the panel under-reports what the \
         pane can draw",
    );
}

/// **The control the gate above is worthless without**: a live pane is
/// unchanged, and it passes on baseline by design. Without it, "ask the live
/// feed on every round, always" would satisfy the gate while every scrubbed
/// pane silently lost the expired warnings the archive is the only source of.
#[test]
fn a_live_pane_is_unchanged() {
    let (routes, live_n) = live_routes();
    let (base, requests) = serve(routes);

    let handler = round(&config_at(&base, chrono::Utc::now().naive_utc()));

    assert_eq!(
        shown(&handler),
        format!("{live_n} shown - W/Wa/Adv/Oth"),
        "a live pane's picture moved",
    );
    assert_eq!(
        requests.load(Ordering::Relaxed),
        1,
        "a live pane issued a request it did not used to",
    );
}

// ── The union itself ────────────────────────────────────────────────────────
//
// Above this line the archive cannot answer, which is what makes the gate
// sharp and also what leaves the *merge* untested. These drive
// `union_of_feed_and_archive` directly with both halves in hand and assert on
// the same glass-level reading, so the count a scrubbed pane reports is pinned
// for every combination of the two sources.

fn payload(
    live: Result<ActiveAlerts, crate::fetch_policy::FetchError>,
    archived: Result<ActiveAlerts, crate::fetch_policy::FetchError>,
) -> FetchPayload {
    Box::new(NwsAlertFetchResult(union_of_feed_and_archive(
        live, archived,
    )))
}

fn dead() -> crate::fetch_policy::FetchError {
    crate::fetch_policy::FetchError::permanent("the origin is unwell".to_string())
}

fn feed(ids: &[&str]) -> ActiveAlerts {
    ActiveAlerts::whole(
        ids.iter()
            .map(|id| alert_named(id, "Severe Thunderstorm Warning"))
            .collect(),
    )
}

fn shown_after(payload: FetchPayload) -> String {
    let mut handler = NwsAlertHandler::new();
    handler.apply_fetch_result(payload, &PaneRef::across(&[]));
    handler
        .status_line(&PaneRef::across(&[]))
        .unwrap_or_else(|| "<no status line>".to_string())
}

/// Both halves answering is the union, at the count on the glass. Six against
/// three rather than one against one: a one-and-one fixture cannot tell a union
/// apart from either half, and nine is reachable only by holding both.
#[test]
fn both_sources_answering_is_the_union_on_the_glass() {
    let live = feed(&["a", "b", "c", "d", "e", "f"]);
    let archived = feed(&["x", "y", "z"]);
    assert_eq!(
        shown_after(payload(Ok(live), Ok(archived))),
        "9 shown - W/Wa/Adv/Oth",
    );
}

/// The union is keyed on the alert id, so a warning both sources carry is one
/// row and not two — the live feed's copy, which is the one that went through
/// zone resolution.
#[test]
fn a_warning_both_sources_carry_is_one_row() {
    let live = feed(&["shared", "live-only"]);
    let archived = feed(&["shared", "archive-only"]);
    assert_eq!(
        shown_after(payload(Ok(live), Ok(archived))),
        "3 shown - W/Wa/Adv/Oth",
        "the same warning from two sources drew twice",
    );
}

/// **A half that fails is a warning, not a silent thinner map.** The round
/// still delivers what it has — refusing the whole picture because the smaller
/// source is down would be a worse under-draw than the one this file exists for
/// — and it delivers the larger half.
#[test]
fn a_dead_archive_leaves_the_live_feed_on_the_glass() {
    assert_eq!(
        shown_after(payload(
            Ok(feed(&["a", "b", "c", "d", "e", "f"])),
            Err(dead())
        )),
        "6 shown - W/Wa/Adv/Oth",
        "one dead source took the other's alerts off the map with it",
    );
}

/// The mirror, and the case the archive branch was written for in the first
/// place: with the live feed unreachable the archived warnings still draw.
#[test]
fn a_dead_feed_leaves_the_archive_on_the_glass() {
    assert_eq!(
        shown_after(payload(Err(dead()), Ok(feed(&["x", "y", "z"])))),
        "3 shown - W/Wa/Adv/Oth",
        "the archive branch's own case regressed",
    );
}

/// Two dead sources are not a successful empty round. A `FetchPayload` carrying
/// `Ok(vec![])` would stamp a fresh clock over a blank layer — the silent
/// partial success this whole file is downstream of.
#[test]
fn two_dead_sources_are_an_error_and_not_an_empty_picture() {
    assert!(
        union_of_feed_and_archive(Err(dead()), Err(dead())).is_err(),
        "a round with no data anywhere reported success",
    );
}

/// A minimal alert carrying its own geometry, for the merge tests above.
fn alert_named(id: &str, event: &str) -> NwsAlert {
    let (fill, stroke) = crate::nws::colors::alert_color(event);
    NwsAlert {
        id: id.to_string(),
        event: event.to_string(),
        category: AlertCategory::from_event(event),
        severity: "Severe".parse().unwrap(),
        urgency: "Immediate".parse().unwrap(),
        certainty: "Observed".parse().unwrap(),
        headline: None,
        description: String::new(),
        instruction: None,
        area_desc: String::new(),
        sender_name: String::new(),
        effective: String::new(),
        expires: String::new(),
        onset: None,
        ends: None,
        valid_from: None,
        valid_until: None,
        affected_zones: Vec::new(),
        features: Arc::new(vec![crate::types::OverlayFeature::new(
            vec![vec![vec![(35.0, -97.0), (35.5, -97.0), (35.5, -96.5)]]],
            fill,
            stroke,
            event.to_string(),
            String::new(),
            crate::types::HatchPattern::None,
        )]),
    }
}
