use std::collections::{BTreeMap, HashMap};
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicBool, Ordering};

use crate::fetch_policy::DataCompleteness;
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

/// Why one zone's boundary did not arrive.
///
/// The leaf of the accounting, and the reason the panel can say *why* rather
/// than only *how many*. Every route out of [`fetch_zone_geometry`] that is not
/// a boundary lands on one of these; there is no fall-through, which is what
/// stops a new failure mode from being invisible the way all five of these were.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ZoneFailure {
    /// The request never produced a status: DNS, TLS, a dropped connection,
    /// a timeout — or, on web, a CORS rejection wearing the browser's
    /// deliberately opaque `TypeError`.
    Unreachable,
    /// The origin answered, and not with a boundary.
    Http(u16),
    /// A body that would not read, or would not parse as JSON.
    Unreadable,
    /// Parsed, and carried nothing this renderer can draw: a null `geometry`,
    /// a type that is not Polygon or MultiPolygon, or rings that simplify away
    /// to fewer than three points.
    NoBoundary,
}

impl std::fmt::Display for ZoneFailure {
    /// Reads as a phrase in a list — `"198 HTTP 503, 7 no usable boundary"` —
    /// so it is lowercase and carries no count of its own.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable => f.write_str("unreachable"),
            Self::Http(status) => write!(f, "HTTP {status}"),
            Self::Unreadable => f.write_str("unreadable"),
            Self::NoBoundary => f.write_str("no usable boundary"),
        }
    }
}

/// What one pass of [`resolve_zone_geometries`] managed, in the terms the layer
/// needs to say so.
///
/// This function returned `()`. It could not report failure, so nothing
/// downstream could: a zone whose fetch failed was skipped at the one `if let`
/// that consults the cache, an alert whose zones all failed kept an empty
/// feature list and drew nothing, and the alerts layer went on reporting
/// `Updated 0s ago` — truthfully, because the *alert* fetch had succeeded.
/// Observed once: 212 of 297 warnings absent from the map with a fully green
/// status line.
///
/// Alerts are counted in three buckets rather than two because the middle one
/// is the worst: an alert that resolved some of its zones draws a real shape
/// that is the **wrong** shape, and a three-county outline says nothing about
/// the two counties missing from it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneResolution {
    /// Alerts that arrived with zone references and no geometry of their own —
    /// everything this pass was asked to place on the map.
    pub alerts_expected: usize,
    /// ...that ended up with every zone they named.
    pub alerts_complete: usize,
    /// ...that ended up with some of them.
    pub alerts_partial: usize,
    /// ...that ended up with none, and so draw nothing at all.
    pub alerts_missing: usize,
    /// Distinct zone URLs needed, and obtained (from disk cache or network).
    pub zones_requested: usize,
    pub zones_resolved: usize,
    /// Why the rest were not obtained, commonest first. Sums to
    /// `zones_requested - zones_resolved`.
    pub failures: Vec<(ZoneFailure, usize)>,
}

impl ZoneResolution {
    /// The layer-agnostic report the UI renders, from the zone-specific one.
    ///
    /// The translation lives here, not in the handler: the handler's job is to
    /// hold what it was given, and the words for "zone boundary" belong to the
    /// module that fetches them.
    pub fn completeness(&self) -> DataCompleteness {
        DataCompleteness {
            expected: self.alerts_expected,
            partial: self.alerts_partial,
            missing: self.alerts_missing,
            parts_requested: self.zones_requested,
            parts_resolved: self.zones_resolved,
            unit: "alerts",
            part_unit: "zone boundaries",
            reasons: self
                .failures
                .iter()
                .map(|(why, count)| (why.to_string(), *count))
                .collect(),
        }
    }
}

/// Fills in `features` for alerts that carry only `affectedZones`, and reports
/// what it managed. URLs are deduplicated, so each county is fetched at most
/// once. Without `cache_dir` this is 1000+ requests on every launch.
///
/// **Nothing is dropped and nothing is invented.** An alert whose zones did not
/// resolve keeps its place in the list with an empty feature vector — it is
/// still selectable, still counted, still there next poll to try again — and no
/// stand-in geometry is ever produced for it. What changes is that the count of
/// them comes back to the caller instead of dying here; see [`ZoneResolution`].
pub async fn resolve_zone_geometries(
    client: &reqwest::Client,
    alerts: &mut [NwsAlert],
    cache_dir: Option<&Path>,
) -> ZoneResolution {
    let mut resolution = ZoneResolution::default();
    let mut needed_urls: Vec<String> = Vec::new();
    for alert in alerts.iter() {
        if alert.features.is_empty() && !alert.affected_zones.is_empty() {
            resolution.alerts_expected += 1;
            for url in &alert.affected_zones {
                if !needed_urls.contains(url) {
                    needed_urls.push(url.clone());
                }
            }
        }
    }
    resolution.zones_requested = needed_urls.len();

    if needed_urls.is_empty() {
        return resolution;
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

    // Tallied by cause rather than kept per URL: the panel says how many and
    // why, and a thousand-line list of counties is not a sentence anyone reads.
    let mut failures: BTreeMap<ZoneFailure, usize> = BTreeMap::new();

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
            match result {
                Ok(polys) => {
                    if let Some(dir) = cache_dir {
                        write_cached_zone(dir, &url, &polys).await;
                    }
                    zone_cache.insert(url, polys);
                }
                Err(why) => *failures.entry(why).or_default() += 1,
            }
        }
    }

    resolution.zones_resolved = zone_cache.len();
    // Commonest first, and by kind within a count, so the sentence is stable
    // across polls that failed the same way.
    let mut failures: Vec<(ZoneFailure, usize)> = failures.into_iter().collect();
    failures.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    resolution.failures = failures;

    for alert in alerts.iter_mut() {
        if !alert.features.is_empty() || alert.affected_zones.is_empty() {
            continue;
        }

        let (fill_rgba, stroke_rgba) = alert_color(&alert.event);

        let mut placed = 0usize;
        for url in &alert.affected_zones {
            if let Some(polys) = zone_cache.get(url) {
                placed += 1;
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
        // Against the alert's own list, not against the round: an alert is
        // whole when *its* zones are all there, however many others failed.
        if placed == alert.affected_zones.len() {
            resolution.alerts_complete += 1;
        } else if placed > 0 {
            resolution.alerts_partial += 1;
        } else {
            resolution.alerts_missing += 1;
        }
    }

    let ZoneResolution {
        alerts_expected,
        alerts_partial,
        alerts_missing,
        zones_requested,
        zones_resolved,
        ..
    } = resolution;
    if alerts_partial > 0 || alerts_missing > 0 {
        // WARN, not INFO: this is the layer under-drawing, and the log line was
        // the only trace of it that ever existed.
        log::warn!(
            "Zone geometries incomplete: {zones_resolved}/{zones_requested} boundaries \
             resolved, so {alerts_missing} of {alerts_expected} alerts draw nothing and \
             {alerts_partial} draw only part of their area ({})",
            resolution
                .failures
                .iter()
                .map(|(why, count)| format!("{count} {why}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    } else {
        log::info!("Resolved {zones_resolved}/{zones_requested} zone geometries");
    }

    resolution
}

async fn fetch_zone_geometry(
    client: &reqwest::Client,
    url: &str,
) -> Result<Vec<GeoPolygon>, ZoneFailure> {
    let json = fetch_zone_json(client, url).await?;
    parse_zone_polygons(&json, url).ok_or(ZoneFailure::NoBoundary)
}

async fn fetch_zone_json(
    client: &reqwest::Client,
    url: &str,
) -> Result<serde_json::Value, ZoneFailure> {
    let response = client
        .get(url)
        .header("Accept", "application/geo+json")
        .send()
        .await
        .map_err(|e| {
            log::debug!("Failed to fetch zone {}: {}", url, e);
            // A transport error can still carry a status (redirect policy,
            // `error_for_status` upstream); reporting it as unreachable would
            // blame the network for the origin's answer.
            e.status()
                .map_or(ZoneFailure::Unreachable, |s| ZoneFailure::Http(s.as_u16()))
        })?;

    if !response.status().is_success() {
        log::debug!(
            "Zone geometry fetch returned HTTP {} for {}",
            response.status(),
            url
        );
        return Err(ZoneFailure::Http(response.status().as_u16()));
    }

    let text = response.text().await.map_err(|e| {
        log::debug!("Failed to read zone response body for {}: {}", url, e);
        ZoneFailure::Unreadable
    })?;

    serde_json::from_str(&text).map_err(|e| {
        log::debug!("Invalid JSON from zone {}: {}", url, e);
        ZoneFailure::Unreadable
    })
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
