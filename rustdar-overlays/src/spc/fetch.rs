use super::discussion::{SpcDiscussion, parse_md_rss};
use super::outlook::{OutlookDay, OutlookProduct, SpcOutlook, outlook_url, parse_geojson};

const SPC_MD_RSS_URL: &str = "https://www.spc.noaa.gov/products/spcmdrss.xml";

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

/// Fetch all currently active SPC Mesoscale Discussions from the RSS feed.
pub async fn fetch_active_discussions(
    client: &reqwest::Client,
) -> Result<Vec<SpcDiscussion>, String> {
    log::info!("Fetching SPC Mesoscale Discussions from {}", SPC_MD_RSS_URL);

    let response = client
        .get(SPC_MD_RSS_URL)
        .send()
        .await
        .map_err(|e| format!("SPC MD RSS request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "SPC returned HTTP {} for MD RSS feed",
            response.status()
        ));
    }

    let text = response
        .text()
        .await
        .map_err(|e| format!("Failed to read SPC MD RSS body: {}", e))?;

    parse_md_rss(&text)
}

/// Which products are available for a given day.
pub fn available_products(day: OutlookDay) -> Vec<OutlookProduct> {
    if day.is_extended() {
        // Days 4-8: single "any severe" probabilistic product
        return vec![OutlookProduct::Probabilistic];
    }
    match day {
        OutlookDay::Day1 | OutlookDay::Day2 => vec![
            OutlookProduct::Categorical,
            OutlookProduct::Tornado,
            OutlookProduct::Wind,
            OutlookProduct::Hail,
        ],
        OutlookDay::Day3 => vec![
            OutlookProduct::Categorical,
            OutlookProduct::Probabilistic,
        ],
        _ => unreachable!(),
    }
}
