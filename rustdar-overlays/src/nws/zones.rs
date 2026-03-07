use std::collections::HashMap;

use crate::types::{GeoPolygon, GeoPolygonRing, HatchPattern, OverlayFeature};

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
                alert.features.push(OverlayFeature {
                    polygons: polys.clone(),
                    fill_rgba,
                    stroke_rgba,
                    label: alert.event.clone(),
                    label2: alert.headline.clone().unwrap_or_default(),
                    hatch: HatchPattern::None,
                });
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
                .map(|ring| simplify_ring(&ring, 0.005))
                .filter(|r| r.len() >= 3)
                .collect()
        })
        .filter(|p: &GeoPolygon| !p.is_empty())
        .collect();

    if simplified.is_empty() { None } else { Some(simplified) }
}

/// Ramer-Douglas-Peucker polygon ring simplification.
///
/// Reduces vertex count by removing points within `epsilon` degrees of the
/// line between their neighbours. An epsilon of ~0.002 (~200 m) keeps shapes
/// visually accurate at typical map zoom levels while cutting vertex counts by
/// 80-90%.
fn simplify_ring(ring: &GeoPolygonRing, epsilon: f64) -> GeoPolygonRing {
    if ring.len() <= 3 {
        return ring.clone();
    }
    rdp_simplify(ring, epsilon)
}

fn rdp_simplify(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let first = points[0];
    let last = points[points.len() - 1];
    let mut max_dist = 0.0_f64;
    let mut max_idx = 0;

    for (i, &pt) in points.iter().enumerate().skip(1).take(points.len() - 2) {
        let d = perpendicular_distance(pt, first, last);
        if d > max_dist {
            max_dist = d;
            max_idx = i;
        }
    }

    if max_dist > epsilon {
        let mut left = rdp_simplify(&points[..=max_idx], epsilon);
        let right = rdp_simplify(&points[max_idx..], epsilon);
        left.pop(); // Remove duplicate junction point
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}

fn perpendicular_distance(
    point: (f64, f64),
    line_start: (f64, f64),
    line_end: (f64, f64),
) -> f64 {
    let dx = line_end.0 - line_start.0;
    let dy = line_end.1 - line_start.1;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-12 {
        let px = point.0 - line_start.0;
        let py = point.1 - line_start.1;
        return (px * px + py * py).sqrt();
    }
    let num = ((point.0 - line_start.0) * dy - (point.1 - line_start.1) * dx).abs();
    num / len_sq.sqrt()
}
