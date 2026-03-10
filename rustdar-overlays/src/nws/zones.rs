use std::collections::HashMap;

use crate::types::{GeoPolygon, HatchPattern, OverlayFeature};

use super::alert::NwsAlert;
use super::colors::alert_color;

/// Resolve zone/county geometries for alerts that have no inline polygon.
///
/// Fetches boundary polygons from the NWS zones API for each `affectedZones`
/// URL, then builds `OverlayFeature`s and populates each alert's `features`.
/// Zone URLs are deduplicated so each county is fetched at most once.
/// Fetches run concurrently for speed.
pub async fn resolve_zone_geometries(client: &reqwest::Client, alerts: &mut [NwsAlert]) {
    // Collect unique zone URLs needed (only for alerts with no features yet)
    let mut needed_urls: Vec<String> = Vec::new();
    for alert in alerts.iter() {
        if alert.features.is_empty() && !alert.affected_zones.is_empty() {
            for url in &alert.affected_zones {
                if !needed_urls.contains(url) {
                    needed_urls.push(url.clone());
                }
            }
        }
    }

    if needed_urls.is_empty() {
        return;
    }

    log::info!(
        "Fetching {} zone geometries for zone-based alerts",
        needed_urls.len()
    );

    // Fetch all zone geometries concurrently
    let futs: Vec<_> = needed_urls
        .iter()
        .map(|url| {
            let client = client.clone();
            let url = url.clone();
            async move {
                let result = fetch_zone_geometry(&client, &url).await;
                (url, result)
            }
        })
        .collect();

    let results = futures::future::join_all(futs).await;

    // Build lookup from URL → polygons
    let mut zone_cache: HashMap<String, Vec<GeoPolygon>> = HashMap::new();
    for (url, result) in results {
        if let Some(polys) = result {
            zone_cache.insert(url, polys);
        }
    }

    log::info!(
        "Resolved {}/{} zone geometries",
        zone_cache.len(),
        needed_urls.len()
    );

    // Populate features for zone-based alerts
    for alert in alerts.iter_mut() {
        if !alert.features.is_empty() || alert.affected_zones.is_empty() {
            continue;
        }

        let (fill_rgba, stroke_rgba) = alert_color(&alert.event);

        for url in &alert.affected_zones {
            if let Some(polys) = zone_cache.get(url) {
                alert.features.push(OverlayFeature::new(
                    polys.clone(),
                    fill_rgba,
                    stroke_rgba,
                    alert.event.clone(),
                    alert.headline.clone().unwrap_or_default(),
                    HatchPattern::None,
                ));
            }
        }
    }
}

/// Fetch polygon geometry for a single NWS zone/county.
///
/// The NWS zones API returns a GeoJSON Feature (not FeatureCollection)
/// with the zone's boundary polygon in the `geometry` field.
async fn fetch_zone_geometry(
    client: &reqwest::Client,
    url: &str,
) -> Option<Vec<GeoPolygon>> {
    let response = client
        .get(url)
        .header("Accept", "application/geo+json")
        .send()
        .await
        .map_err(|e| {
            log::debug!("Failed to fetch zone {}: {}", url, e);
            e
        })
        .ok()?;

    if !response.status().is_success() {
        log::debug!(
            "Zone geometry fetch returned HTTP {} for {}",
            response.status(),
            url
        );
        return None;
    }

    let text = response
        .text()
        .await
        .map_err(|e| {
            log::debug!("Failed to read zone response body for {}: {}", url, e);
            e
        })
        .ok()?;

    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| {
            log::debug!("Invalid JSON from zone {}: {}", url, e);
            e
        })
        .ok()?;

    // Zone API returns a Feature directly — geometry is at the top level
    let polys = super::alert::parse_geometry(json.get("geometry"))?;

    // Simplify county polygons (they have 100+ vertices each — too detailed
    // for map rendering and very expensive for ear-clip triangulation).
    let simplified: Vec<GeoPolygon> = polys
        .into_iter()
        .map(|polygon| {
            polygon
                .into_iter()
                .map(|ring| crate::types::simplify_ring(&ring, 0.005))
                .filter(|r| r.len() >= 3)
                .collect()
        })
        .filter(|p: &GeoPolygon| !p.is_empty())
        .collect();

    if simplified.is_empty() { None } else { Some(simplified) }
}
