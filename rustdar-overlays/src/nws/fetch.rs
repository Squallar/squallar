use std::path::Path;

use super::alert::{NwsAlert, parse_alerts};
use super::zones::resolve_zone_geometries;

const NWS_ALERTS_URL: &str = "https://api.weather.gov/alerts/active?status=actual";

/// Unlike SPC and IEM, api.weather.gov *requires* a `User-Agent`; `client` must
/// carry one. `zone_cache_dir` backs the on-disk zone-geometry cache, without
/// which each launch issues 1000+ requests.
pub async fn fetch_active_alerts(
    client: &reqwest::Client,
    zone_cache_dir: Option<&Path>,
) -> Result<Vec<NwsAlert>, String> {
    log::info!("Fetching NWS active alerts from {}", NWS_ALERTS_URL);

    let response = client
        .get(NWS_ALERTS_URL)
        .header("Accept", "application/geo+json")
        .send()
        .await
        .map_err(|e| format!("NWS alerts HTTP request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "NWS API returned HTTP {} for alerts",
            response.status()
        ));
    }

    let text = response
        .text()
        .await
        .map_err(|e| format!("Failed to read NWS alerts response body: {}", e))?;

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid JSON from NWS alerts: {}", e))?;

    let mut alerts = parse_alerts(&json);

    // Many alerts carry zone references instead of geometry.
    resolve_zone_geometries(client, &mut alerts, zone_cache_dir).await;

    Ok(alerts)
}
