use super::alert::{NwsAlert, parse_alerts};
use super::zones::resolve_zone_geometries;

const NWS_ALERTS_URL: &str = "https://api.weather.gov/alerts/active?status=actual";

/// Fetch all active NWS alerts and parse them into `NwsAlert` structs.
///
/// Calls `GET /alerts/active?status=actual` on the NWS API.
/// The `reqwest::Client` must have a `User-Agent` header configured.
pub async fn fetch_active_alerts(
    client: &reqwest::Client,
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

    // Resolve zone/county geometries for alerts that only have zone references
    resolve_zone_geometries(client, &mut alerts).await;

    Ok(alerts)
}
