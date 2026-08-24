//! **NWS warnings that were valid at a past instant.**
//!
//! `api.weather.gov/alerts/active` answers with what is in force now and has no
//! archive, so a pane scrubbed to a storm ten years gone fetched today's
//! polygons and filtered every one of them away — the layer reported "268
//! shown" over a 2013 volume and drew nothing. The Iowa State Mesonet keeps the
//! storm-based-warning archive and addresses it by timestamp, which is the
//! shape [`squallar_source::handler::FetchConfig::as_of`] was written for.
//!
//! # This translates rather than parses
//!
//! IEM answers GeoJSON in its own property vocabulary — VTEC `phenomena` and
//! `significance` codes rather than a spelled-out `event`, `issue`/`expire`
//! rather than `effective`/`expires`. Rather than a second parser, this
//! rewrites those properties into the shape the live feed uses and hands the
//! result to [`super::alert::parse_alerts`]. One parser keeps one set of
//! colours, one category rule and one validity window; a copy would drift, and
//! the first thing to drift would be the colour of a tornado warning.
//!
//! # What it does not carry
//!
//! Storm-based warnings only — the polygon products. Zone-based watches and
//! advisories live behind a different IEM service, so a scrubbed pane draws the
//! warnings and not the watches. That is a smaller picture than the live layer
//! shows, and it is named here rather than left for someone to notice.

use super::alert::NwsAlert;
use super::fetch::ActiveAlerts;
use squallar_source::fetch_policy::FetchError;
use squallar_source::origins::DataSources;

/// A VTEC phenomenon/significance pair, as the event name the live feed spells.
///
/// Only the pairs that appear in the storm-based-warning archive: SBW carries
/// polygon warnings, so the significance is `W` in every row that matters. An
/// unknown pair is rendered from its own codes rather than dropped — an
/// unrecognised warning still has a polygon and a time, and drawing it grey
/// beats not drawing it.
fn event_name(phenomena: &str, significance: &str) -> String {
    let noun = match phenomena {
        "TO" => "Tornado",
        "SV" => "Severe Thunderstorm",
        "FF" => "Flash Flood",
        "MA" => "Marine",
        "EW" => "Extreme Wind",
        "SQ" => "Snow Squall",
        "DS" => "Dust Storm",
        "FA" => "Flood",
        "SM" => "Special Marine",
        other => return format!("{other} {significance}"),
    };
    let kind = match significance {
        "W" => "Warning",
        "A" => "Watch",
        "Y" => "Advisory",
        "S" => "Statement",
        other => other,
    };
    format!("{noun} {kind}")
}

/// The severity the live feed would report for this event.
///
/// IEM publishes no severity field, and `parse_alerts` requires one. These are
/// the CAP values api.weather.gov sends for the same products, so a warning
/// archived in 2013 sorts and paints exactly as the same warning would today.
fn severity_of(phenomena: &str, significance: &str) -> &'static str {
    match (phenomena, significance) {
        ("TO", "W") | ("EW", "W") => "Extreme",
        (_, "W") => "Severe",
        (_, "A") => "Moderate",
        _ => "Minor",
    }
}

/// Rewrite one IEM storm-based-warning collection into the live feed's shape.
///
/// Public for the tests, which pin the translation against a captured IEM
/// response rather than against this function's own idea of one.
pub fn translate(iem: &serde_json::Value) -> serde_json::Value {
    let features = iem
        .get("features")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();

    let translated: Vec<serde_json::Value> = features
        .iter()
        .filter_map(|feature| {
            let props = feature.get("properties")?;
            let get = |key: &str| props.get(key).and_then(|v| v.as_str()).unwrap_or_default();

            let phenomena = get("phenomena");
            let significance = get("significance");
            if phenomena.is_empty() {
                return None;
            }
            let event = event_name(phenomena, significance);
            let wfo = get("wfo");
            let eventid = props
                .get("eventid")
                .map(|v| v.to_string())
                .unwrap_or_default();
            let issue = get("issue");
            let expire = get("expire");

            Some(serde_json::json!({
                "type": "Feature",
                "geometry": feature.get("geometry").cloned().unwrap_or(serde_json::Value::Null),
                "properties": {
                    // Stable and unique per archived warning, so a refetch of the
                    // same instant does not duplicate anything.
                    "id": format!("iem-sbw-{wfo}-{phenomena}{significance}-{eventid}-{issue}"),
                    "event": event,
                    "severity": severity_of(phenomena, significance),
                    // IEM carries neither, and the live feed's values for a
                    // polygon warning are always these.
                    "urgency": "Immediate",
                    "certainty": "Observed",
                    "effective": issue,
                    "expires": expire,
                    "senderName": format!("NWS {wfo}"),
                    "areaDesc": format!("NWS {wfo}"),
                    // No free text in the archive. Empty rather than invented:
                    // a fabricated warning description on a screenshot is the
                    // one thing worse than a missing one.
                    "description": "",
                },
            }))
        })
        .collect();

    serde_json::json!({ "type": "FeatureCollection", "features": translated })
}

/// How long IEM gets to answer. One small GeoJSON body.
const ARCHIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Fetch the warnings that were valid at `at`, UTC.
pub async fn fetch_archived_alerts(
    sources: &DataSources,
    at: chrono::NaiveDateTime,
) -> Result<ActiveAlerts, FetchError> {
    // NOT the application-wide client, which carries a `User-Agent`: IEM
    // answers `OPTIONS` with `405` and no `Access-Control-Allow-Methods`, so a
    // `User-Agent` makes this a preflighted request the browser never sends —
    // silently, and on web only. See `DataSources::iem_client`.
    let client = sources
        .iem_client(ARCHIVE_TIMEOUT)
        .build()
        .map_err(|e| FetchError::permanent(format!("could not build the IEM client: {e}")))?;

    let url = sources.nws_alerts_archive_url(at);
    log::info!("Fetching archived NWS warnings valid at {at} from {url}");

    let response = client.get(&url).send().await.map_err(|e| {
        FetchError::from_transport(&e, format!("archived NWS alerts request failed: {e}"))
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::permanent(format!(
            "archived NWS alerts returned {status} for {url}"
        )));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| FetchError::permanent(format!("archived NWS alerts were not JSON: {e}")))?;

    let alerts: Vec<NwsAlert> = super::alert::parse_alerts(&translate(&body));
    log::info!("{} archived warning(s) valid at {at}", alerts.len());
    // `whole`: every storm-based warning carries its own polygon, so nothing
    // here waits on zone-geometry resolution the way the live feed's watches do.
    Ok(ActiveAlerts::whole(alerts))
}

#[cfg(test)]
#[path = "archive/tests.rs"]
mod tests;
