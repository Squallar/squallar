use std::collections::HashMap;
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicBool, Ordering};

use crate::types::{GeoPolygon, HatchPattern, OverlayFeature};

use super::alert::NwsAlert;
use super::colors::alert_color;

/// One year. Zone boundaries are effectively static.
#[cfg(not(target_arch = "wasm32"))]
const CACHE_TTL_SECS: u64 = 365 * 24 * 3600;

#[cfg(not(target_arch = "wasm32"))]
static CACHE_WRITE_WARNED: AtomicBool = AtomicBool::new(false);

/// WARN once per session, then DEBUG: a bad cache dir fails on every zone, and
/// 1000+ identical warnings drown the log.
#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedZone {
    /// Unix seconds.
    fetched_at: u64,
    /// Already simplified.
    polygons: Vec<GeoPolygon>,
}

/// Fills in `features` for alerts that carry only `affectedZones`. URLs are
/// deduplicated, so each county is fetched at most once. Without `cache_dir`
/// this is 1000+ requests on every launch.
pub async fn resolve_zone_geometries(
    client: &reqwest::Client,
    alerts: &mut [NwsAlert],
    cache_dir: Option<&Path>,
) {
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
        // Bounded: unbounded exhausts file descriptors on low-ulimit systems.
        use futures::stream::{self, StreamExt};
        const MAX_CONCURRENT_FETCHES: usize = 10;

        let results: Vec<_> = stream::iter(urls_to_fetch.into_iter().map(|url| {
            // reqwest::Client is Arc-backed: this is a ref-count bump, not a
            // connection-pool copy.
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

async fn fetch_zone_geometry(client: &reqwest::Client, url: &str) -> Option<Vec<GeoPolygon>> {
    let json = fetch_zone_json(client, url).await?;
    parse_zone_polygons(&json, url)
}

async fn fetch_zone_json(client: &reqwest::Client, url: &str) -> Option<serde_json::Value> {
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

/// The zones API returns a bare Feature, not a FeatureCollection: `geometry`
/// is at the top level. County rings run 100+ vertices each, which is finer
/// than the map shows, so they are simplified here: fewer vertices to project
/// and fill on every render, and smaller files in the on-disk zone cache.
fn parse_zone_polygons(json: &serde_json::Value, url: &str) -> Option<Vec<GeoPolygon>> {
    let polys = super::alert::parse_geometry(json.get("geometry"))?;

    let simplified: Vec<GeoPolygon> = polys
        .into_iter()
        .map(|polygon| {
            polygon
                .into_iter()
                .map(|ring| {
                    crate::render::geo::simplify_ring(&ring, crate::types::SIMPLIFY_EPSILON)
                })
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

#[cfg(not(target_arch = "wasm32"))]
fn unix_now() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `https://api.weather.gov/zones/county/TXC113` → `"county_TXC113"`. The kind
/// must stay in the key: the same id exists under several zone kinds.
#[cfg(not(target_arch = "wasm32"))]
fn zone_cache_key(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let mut parts = trimmed.rsplit('/');
    let id = parts.next().filter(|s| !s.is_empty())?;
    let kind = parts.next().unwrap_or("zone");
    Some(format!("{kind}_{id}"))
}

/// `None` if missing, corrupt, or past the TTL.
#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
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

// ── Web: no filesystem ───────────────────────────────────────────────────
//
// Same signatures rather than cfg at the call sites, so the caching *policy*
// has one body on every target and cannot drift between native and web.
//
// Real behavioural difference, not a stub: on web every zone is re-fetched
// each session, and the browser's own HTTP cache is the layer that absorbs it.

#[cfg(target_arch = "wasm32")]
async fn read_cached_zone(_cache_dir: &Path, _url: &str) -> Option<Vec<GeoPolygon>> {
    None
}

#[cfg(target_arch = "wasm32")]
async fn write_cached_zone(_cache_dir: &Path, _url: &str, _polygons: &[GeoPolygon]) {}
