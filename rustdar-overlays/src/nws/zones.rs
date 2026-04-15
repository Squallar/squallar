use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::types::{GeoPolygon, HatchPattern, OverlayFeature};

use super::alert::NwsAlert;
use super::colors::alert_color;

/// TTL for cached zone geometries (1 year in seconds).
const CACHE_TTL_SECS: u64 = 365 * 24 * 3600;

/// Guards first-per-session WARN log for zone cache write failures.
static CACHE_WRITE_WARNED: AtomicBool = AtomicBool::new(false);

/// Log a cache write failure at WARN level the first time, then DEBUG.
fn log_cache_write_failure(msg: &str) {
    if CACHE_WRITE_WARNED
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        log::warn!("{msg} (further cache failures logged at debug level)");
    } else {
        log::debug!("{msg}");
    }
}

/// A cached zone geometry entry, serialized to JSON on disk.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedZone {
    /// Unix timestamp (seconds since epoch) when this entry was fetched.
    fetched_at: u64,
    /// Simplified polygon data for the zone.
    polygons: Vec<GeoPolygon>,
}

/// Resolve zone/county geometries for alerts that have no inline polygon.
///
/// Fetches boundary polygons from the NWS zones API for each `affectedZones`
/// URL, then builds `OverlayFeature`s and populates each alert's `features`.
/// Zone URLs are deduplicated so each county is fetched at most once.
/// Fetches run concurrently for speed.
///
/// When `cache_dir` is provided, zone geometries are cached on disk to avoid
/// re-fetching 1000+ HTTP requests on every app launch. Cached entries expire
/// after 1 year.
pub async fn resolve_zone_geometries(
    client: &reqwest::Client,
    alerts: &mut [NwsAlert],
    cache_dir: Option<&Path>,
) {
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

    // Check disk cache first to avoid unnecessary HTTP requests
    let mut zone_cache: HashMap<String, Vec<GeoPolygon>> = HashMap::new();
    let mut urls_to_fetch: Vec<String> = Vec::new();

    for url in &needed_urls {
        let cached = match cache_dir {
            Some(dir) => read_cached_zone(dir, url).await,
            None => None,
        };
        if let Some(polys) = cached {
            zone_cache.insert(url.clone(), polys);
        } else {
            urls_to_fetch.push(url.clone());
        }
    }

    log::info!(
        "Zone geometries: {} cached, {} to fetch",
        zone_cache.len(),
        urls_to_fetch.len(),
    );

    if !urls_to_fetch.is_empty() {
        // Fetch zone geometries with bounded concurrency to be respectful of
        // the NWS API and avoid exhausting file descriptors on low-ulimit systems.
        use futures::stream::{self, StreamExt};
        const MAX_CONCURRENT_FETCHES: usize = 10;

        let results: Vec<_> = stream::iter(urls_to_fetch.into_iter().map(|url| {
            // reqwest::Client is backed by an Arc internally, so cloning is
            // just an Arc::clone (O(1) ref-count bump, no connection pool copy).
            let client = client.clone();
            async move {
                let result = fetch_zone_geometry(&client, &url).await;
                (url, result)
            }
        }))
        .buffer_unordered(MAX_CONCURRENT_FETCHES)
        .collect()
        .await;

        for (url, result) in results {
            if let Some(polys) = result {
                // Write to disk cache for next time
                if let Some(dir) = cache_dir {
                    write_cached_zone(dir, &url, &polys).await;
                }
                zone_cache.insert(url, polys);
            }
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
    let json = fetch_zone_json(client, url).await?;
    parse_zone_polygons(&json, url)
}

/// Send an HTTP GET for a single NWS zone and return the parsed JSON body.
async fn fetch_zone_json(
    client: &reqwest::Client,
    url: &str,
) -> Option<serde_json::Value> {
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

    serde_json::from_str(&text)
        .map_err(|e| {
            log::debug!("Invalid JSON from zone {}: {}", url, e);
            e
        })
        .ok()
}

/// Extract and simplify polygons from a NWS zone GeoJSON Feature.
///
/// Zone API returns a Feature directly — geometry is at the top level.
/// County polygons are simplified because they have 100+ vertices each,
/// too detailed for map rendering and expensive for ear-clip triangulation.
fn parse_zone_polygons(json: &serde_json::Value, url: &str) -> Option<Vec<GeoPolygon>> {
    let polys = super::alert::parse_geometry(json.get("geometry"))?;

    let simplified: Vec<GeoPolygon> = polys
        .into_iter()
        .map(|polygon| {
            polygon
                .into_iter()
                .map(|ring| crate::render::geo::simplify_ring(&ring, crate::types::SIMPLIFY_EPSILON))
                .filter(|r| r.len() >= 3)
                .collect()
        })
        .filter(|p: &GeoPolygon| !p.is_empty())
        .collect();

    if simplified.is_empty() {
        log::debug!("Zone {} produced no polygons after simplification", url);
        None
    } else {
        Some(simplified)
    }
}

// ── Disk cache helpers ───────────────────────────────────────────────────

/// Current unix timestamp in seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Extract a cache-friendly key from a NWS zone URL.
///
/// E.g. `https://api.weather.gov/zones/county/TXC113` → `"county_TXC113"`.
fn zone_cache_key(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let mut parts = trimmed.rsplit('/');
    let id = parts.next().filter(|s| !s.is_empty())?;
    let kind = parts.next().unwrap_or("zone");
    Some(format!("{kind}_{id}"))
}

/// Read a zone geometry from the disk cache.
///
/// Returns `None` if the file is missing, corrupt, or older than the TTL.
async fn read_cached_zone(cache_dir: &Path, url: &str) -> Option<Vec<GeoPolygon>> {
    let key = zone_cache_key(url)?;
    let path = cache_dir.join(format!("{key}.json"));
    let data = tokio::fs::read_to_string(&path).await.ok()?;
    let cached: CachedZone = serde_json::from_str(&data).ok()?;

    if unix_now().saturating_sub(cached.fetched_at) > CACHE_TTL_SECS {
        let _ = tokio::fs::remove_file(&path).await;
        return None;
    }

    Some(cached.polygons)
}

/// Write a zone geometry to the disk cache.
async fn write_cached_zone(cache_dir: &Path, url: &str, polygons: &[GeoPolygon]) {
    let Some(key) = zone_cache_key(url) else {
        return;
    };
    if let Err(e) = tokio::fs::create_dir_all(cache_dir).await {
        log_cache_write_failure(&format!("Failed to create zone cache directory: {e}"));
        return;
    }
    let entry = CachedZone {
        fetched_at: unix_now(),
        polygons: polygons.to_vec(),
    };
    let path = cache_dir.join(format!("{key}.json"));
    match serde_json::to_string(&entry) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&path, json).await {
                log_cache_write_failure(&format!(
                    "Failed to write zone cache {}: {e}",
                    path.display(),
                ));
            }
        }
        Err(e) => log_cache_write_failure(&format!("Failed to serialize zone cache: {e}")),
    }
}
