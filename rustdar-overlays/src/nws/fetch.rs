use std::path::Path;

use rustdar_radar::sources::DataSources;

use super::alert::{NwsAlert, parse_alerts};
use super::zones::resolve_zone_geometries;
use crate::fetch_policy::{FetchError, NotFound};

/// Unlike SPC and IEM, api.weather.gov *requires* a `User-Agent`; `client` must
/// carry one. The URL comes from [`DataSources::nws_alerts_url`] so the origin
/// stays visible to the validations derived from the origin table.
/// `zone_cache_dir` backs the on-disk zone-geometry cache, without which each
/// launch issues 1000+ requests.
///
/// A 404 is **broken**, not routine: `/alerts/active` is a standing endpoint
/// that answers with an empty `features` array on a quiet day, so its absence
/// means the API moved.
pub async fn fetch_active_alerts(
    client: &reqwest::Client,
    sources: &DataSources,
    zone_cache_dir: Option<&Path>,
) -> Result<Vec<NwsAlert>, FetchError> {
    let url = sources.nws_alerts_url();
    log::info!("Fetching NWS active alerts from {url}");

    let response = client
        .get(&url)
        .header("Accept", "application/geo+json")
        .send()
        .await
        .map_err(|e| {
            FetchError::from_transport(&e, format!("NWS alerts HTTP request failed: {e}"))
        })?;

    if !response.status().is_success() {
        return Err(FetchError::from_status(
            response.status(),
            NotFound::IsBroken,
            format!("NWS API returned HTTP {} for alerts", response.status()),
        ));
    }

    let text = response.text().await.map_err(|e| {
        FetchError::from_transport(&e, format!("Failed to read NWS alerts response body: {e}"))
    })?;

    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| FetchError::transient(format!("Invalid JSON from NWS alerts: {e}")))?;

    let mut alerts = parse_alerts(&json);

    // Many alerts carry zone references instead of geometry.
    resolve_zone_geometries(client, &mut alerts, zone_cache_dir).await;

    Ok(alerts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fails if the alerts URL bypasses `nws_api_base`. Without this the origin
    /// table's reachability check inspects a string no URL is built from —
    /// which is what this module's own `const NWS_ALERTS_URL` used to be.
    #[test]
    fn the_alerts_url_comes_from_the_declared_origin() {
        let sources = DataSources {
            nws_api_base: std::borrow::Cow::Borrowed("http://127.0.0.1:8080"),
            ..DataSources::production()
        };
        let url = sources.nws_alerts_url();
        assert!(
            url.starts_with("http://127.0.0.1:8080/"),
            "{url} does not come from nws_api_base",
        );
        assert!(
            !url.contains("api.weather.gov"),
            "{url} still hardcodes the origin"
        );
    }

    /// The exact URL the hardcoded constant used to name, probed live
    /// 2026-07-25 (see the origin table's per-origin evidence). Hardcoded, so
    /// threading `nws_api_base` cleanly while mangling the path still fails.
    #[test]
    fn the_production_alerts_url_is_the_one_that_was_probed() {
        assert_eq!(
            DataSources::production().nws_alerts_url(),
            "https://api.weather.gov/alerts/active?status=actual",
        );
    }
}
