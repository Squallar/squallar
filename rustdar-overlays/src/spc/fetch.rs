use super::outlook::{OutlookDay, OutlookProduct, SpcOutlook, outlook_url, parse_geojson};

/// Fetch an SPC outlook product and parse it into an `SpcOutlook`.
pub async fn fetch_outlook(
    client: &reqwest::Client,
    day: OutlookDay,
    product: OutlookProduct,
) -> Result<SpcOutlook, String> {
    let url = outlook_url(day, product);
    log::info!("Fetching SPC outlook: {}", url);

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed for {}: {}", url, e))?;

    if !response.status().is_success() {
        return Err(format!(
            "SPC returned HTTP {} for {}",
            response.status(),
            url
        ));
    }

    let text = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid JSON from {}: {}", url, e))?;

    parse_geojson(&json, day, product)
}

/// Fetch all available outlook products for a given day.
/// Returns results for each product (some may fail independently).
pub async fn fetch_all_for_day(
    client: &reqwest::Client,
    day: OutlookDay,
) -> Vec<(OutlookProduct, Result<SpcOutlook, String>)> {
    let products = available_products(day);
    let mut results = Vec::new();

    for product in products {
        let result = fetch_outlook(client, day, product).await;
        results.push((product, result));
    }

    results
}

/// Which products are available for a given day.
pub fn available_products(day: OutlookDay) -> Vec<OutlookProduct> {
    match day {
        OutlookDay::Day1 => vec![
            OutlookProduct::Categorical,
            OutlookProduct::Tornado,
            OutlookProduct::Wind,
            OutlookProduct::Hail,
        ],
        OutlookDay::Day2 => vec![
            OutlookProduct::Categorical,
            OutlookProduct::Probabilistic,
        ],
        OutlookDay::Day3 => vec![
            OutlookProduct::Categorical,
            OutlookProduct::Probabilistic,
        ],
    }
}
