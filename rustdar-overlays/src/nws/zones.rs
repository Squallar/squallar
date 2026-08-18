use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicBool, Ordering};

use crate::fetch_policy::DataCompleteness;
use crate::types::{GeoPolygon, HatchPattern, OverlayFeature};

use super::alert::NwsAlert;
use super::colors::alert_color;

/// One year. Zone boundaries are effectively static.
#[cfg(not(target_arch = "wasm32"))]
const CACHE_TTL_SECS: u64 = 365 * 24 * 3600;

/// Which simplification produced the polygons on disk.
///
/// The cache stores the *simplified* rings, not what the origin sent, and it
/// holds them for a year. So a change to [`crate::render::geo::simplify_ring`]
/// does not reach a zone anyone has already looked at — it reaches it next
/// August. Without this, fixing a simplifier that was deleting small islands
/// would have left every island already deleted on this machine deleted, and
/// the fix would have read as working only on a cache nobody has.
///
/// Bump it whenever the geometry written here changes shape. An entry with a
/// different value — or with no `schema` field at all, which is every entry
/// written before this existed — fails to deserialize or fails the check, and
/// is refetched.
#[cfg(not(target_arch = "wasm32"))]
const ZONE_CACHE_SCHEMA: u32 = 1;

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
    /// [`ZONE_CACHE_SCHEMA`]. No `serde` default, so an entry from before it
    /// existed does not parse and is refetched rather than trusted.
    schema: u32,
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
    /// or a type that is not `Polygon`, `MultiPolygon`, or a
    /// `GeometryCollection` of those.
    ///
    /// It used to mean two further things, and both of them were us rather than
    /// the origin: a `GeometryCollection`, which is how the NWS serves 227 of
    /// its 11,651 zones, and a zone every one of whose rings the simplifier ate.
    /// With those closed, **no zone in the published corpus reaches this
    /// variant** — measured over all 11,651 of them by the `zone_geometry_tests`
    /// module, which is `cfg(test)` and so cannot be linked from here. A count
    /// against it now really does mean the origin sent something undrawable.
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

/// An alert this pass is asked to place: one that named zones and brought no
/// geometry of its own.
fn needs_zones(alert: &NwsAlert) -> bool {
    alert.features.is_empty() && !alert.affected_zones.is_empty()
}

/// A zone URL as the seen-set in [`distinct_zone_urls`] keys it.
///
/// A newtype over the `&str` rather than the `&str` itself because it is then
/// the **one place a zone URL is hashed or compared**, and under `cfg(test)`
/// those two impls tally each examination. That is what lets
/// `the_zone_list_costs_one_pass_not_one_scan_per_url` state the cost of the
/// dedup as a count of URLs looked at rather than as a ratio of two
/// `Instant`s, which is a reading of whatever else the machine was doing.
///
/// **The tally is stable within a band, not exact**, and the distinction
/// matters enough to spell out. The hash half is one call per reference and is
/// the same integer everywhere. The `eq` half is not: `hashbrown` calls it only
/// when a 7-bit control byte collides, so it depends on `RandomState`'s
/// per-process seed. Measured over two runs of the small round, the hash half
/// held at 5,580 and the `eq` half moved 184 to 202, carrying the total from
/// 5,764 to 5,782 — 10% of the `eq` half, 0.3% of the total, against the 1.7x
/// of headroom the assertion leaves. That is why the band is a band
/// and not an equality, and it is the one respect in which this count is weaker
/// than `the_hover_lookup_does_not_walk_the_gates`', which has no randomness in
/// it and is asserted as an exact `64`.
///
/// Outside `cfg(test)` the two impls are what `derive` would have written and
/// forward straight to `&str`'s, so no build that ships pays for the wrapper or
/// knows it was here.
///
/// **The tally has to move with the key type.** A rewrite of the dedup that is
/// perfectly good on its own terms -- a `BTreeSet<String>`, a sort-and-dedup, a
/// `HashMap<&str, usize>` -- stops going through these impls, the count reads
/// zero, and the test red-gates on a change that broke nothing. The failure
/// message says so in as many words rather than leaving it to be worked out,
/// but the counter belongs wherever the membership decision ends up.
struct SeenUrl<'a>(&'a str);

impl PartialEq for SeenUrl<'_> {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(test)]
        note_url_examination();
        self.0 == other.0
    }
}

impl Eq for SeenUrl<'_> {}

impl std::hash::Hash for SeenUrl<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        #[cfg(test)]
        note_url_examination();
        self.0.hash(state);
    }
}

#[cfg(test)]
thread_local! {
    /// Zone URLs examined -- hashed or compared -- on this thread since
    /// [`take_url_examinations`] last took the tally.
    ///
    /// Thread-local rather than a `static`: the suite runs its tests in
    /// parallel threads of one process, and [`distinct_zone_urls`] runs start
    /// to finish on the thread that called it, so a per-thread tally is both
    /// the whole tally and nobody else's.
    static URL_EXAMINATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One more URL looked at by the dedup.
#[cfg(test)]
fn note_url_examination() {
    URL_EXAMINATIONS.with(|n| n.set(n.get() + 1));
}

/// The examinations since this was last called, and the tally back to zero.
#[cfg(test)]
fn take_url_examinations() -> u64 {
    URL_EXAMINATIONS.with(|n| n.replace(0))
}

/// Every zone boundary a round has to obtain, once each, in the order the
/// alerts first name them.
///
/// Named and separated from [`resolve_zone_geometries`] because it is the one
/// part of the round whose cost is set by the size of the round rather than by
/// the network, and because that makes it measurable on its own. A busy
/// afternoon names on the order of 1,900 zones across 220 geometryless alerts,
/// and on wasm32 this runs on the browser's main thread — the thread that also
/// has to keep the map moving under the user's hand.
///
/// First-seen order is part of the contract: it is the order the fetches are
/// issued in, so a round that is interrupted has fetched a prefix that depends
/// on nothing but the alert feed. That is why the answer stays a `Vec` and the
/// set is only the membership test beside it — a bare `HashSet` would return
/// the same URLs in an order that changes every run.
///
/// The set is not an optimisation detail, it is the difference between one
/// pass and `n²/2` string compares — which is why it is keyed by [`SeenUrl`],
/// so that a test can count those compares instead of timing them. A
/// measured live round — 361 alerts, 223 of
/// them geometryless, 1,904 references, 1,690 distinct — spent **22.5 ms in
/// Firefox and 13.7 ms in Chrome** scanning the list it was building, on the
/// browser's main thread, once per alert poll. That is a dropped frame at
/// 60 Hz and three at 144 Hz, in the middle of a pan.
pub fn distinct_zone_urls(alerts: &[NwsAlert]) -> Vec<String> {
    let mut needed_urls: Vec<String> = Vec::new();
    let mut seen: HashSet<SeenUrl<'_>> = HashSet::new();
    for alert in alerts.iter().filter(|alert| needs_zones(alert)) {
        for url in &alert.affected_zones {
            if seen.insert(SeenUrl(url.as_str())) {
                needed_urls.push(url.clone());
            }
        }
    }
    needed_urls
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
    let needed_urls = distinct_zone_urls(alerts);
    let mut resolution = ZoneResolution {
        alerts_expected: alerts.iter().filter(|alert| needs_zones(alert)).count(),
        zones_requested: needed_urls.len(),
        ..Default::default()
    };

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
                // The alerts of this round are not yet published, so this is
                // the parse-time build finishing, not a copy-on-write: the
                // `Arc` is still uniquely held and `make_mut` mutates in
                // place.
                Arc::make_mut(&mut alert.features).push(OverlayFeature::new(
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
///
/// The `len() >= 3` filter is defence, not policy, and it can no longer fire:
/// [`parse_polygon_coords`](crate::types::parse_polygon_coords) admits no ring
/// shorter than that and [`simplify_ring`](crate::render::geo::simplify_ring)
/// no longer shortens one past it. When it *could* fire it was the whole
/// mechanism by which small islands left the map, and — because it ran over
/// every ring of a polygon including the first — a way for a surviving hole to
/// be promoted to an exterior ring and painted solid.
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

/// `None` if missing, corrupt, written by a different simplification, or past
/// the TTL.
///
/// The last three are one case, not three, and the reason to say so is the
/// removal: a file that will not parse will not parse next poll either, and one
/// left in place is re-read for every alert that names its zone until its
/// year-long TTL — which it cannot reach, because reaching it requires parsing
/// the timestamp inside it. Corrupt entries used to be exactly that, silently
/// skipped and never cleared.
#[cfg(not(target_arch = "wasm32"))]
async fn read_cached_zone(cache_dir: &Path, url: &str) -> Option<Vec<GeoPolygon>> {
    let key = zone_cache_key(url)?;
    let path = cache_dir.join(format!("{key}.json"));
    let data = tokio::fs::read_to_string(&path).await.ok()?;

    let usable = match serde_json::from_str::<CachedZone>(&data) {
        Ok(cached)
            if cached.schema == ZONE_CACHE_SCHEMA
                && unix_now().saturating_sub(cached.fetched_at) <= CACHE_TTL_SECS =>
        {
            Some(cached.polygons)
        }
        _ => None,
    };

    if usable.is_none() {
        let _ = tokio::fs::remove_file(&path).await;
    }
    usable
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
        schema: ZONE_CACHE_SCHEMA,
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

/// Why a boundary goes missing, against the geometry the NWS really serves.
#[cfg(test)]
mod zone_geometry_tests;

// ── Tests ────────────────────────────────────────────────────────────────
//
// Native-only: the loopback stub is `std::net::TcpListener` and a real thread,
// neither of which exists on wasm32. The code under test is not gated — this is
// the same production body on both targets.

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::nws::alert::{AlertCategory, NwsAlert};

    /// A canned GeoJSON zone Feature — the bare `Feature` the zones API really
    /// returns, with `geometry` at the top level rather than a
    /// `FeatureCollection`. Big enough that `simplify_ring` cannot reduce it
    /// below three points.
    fn zone_body(lat: f64) -> String {
        format!(
            r#"{{"type":"Feature","geometry":{{"type":"Polygon","coordinates":
             [[[-97.0,{lat}],[-97.0,{}],[-96.0,{}],[-96.0,{lat}],[-97.0,{lat}]]]}}}}"#,
            lat + 1.0,
            lat + 1.0,
        )
    }

    /// Serve canned responses by path from a loopback socket, forever.
    ///
    /// Routed rather than one-shot (`rustdar_radar::archive`'s `serve_once` is
    /// the single-response shape): the whole point here is a round of many
    /// requests where *some* succeed, which one response cannot express. An
    /// unrouted path answers 500, so a test states only what it wants to
    /// succeed.
    fn serve(routes: HashMap<String, (u16, String)>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut scratch = [0u8; 4096];
                let read = stream.read(&mut scratch).unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..read]);
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let (code, body) = routes
                    .get(path)
                    .cloned()
                    .unwrap_or((500, "upstream is unwell".to_string()));
                let response = format!(
                    "HTTP/1.1 {code} .\r\nContent-Type: application/geo+json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// `tls::init()` is required even for a cleartext URL: with
    /// `rustls-no-provider` and `aws-lc-rs` out of the graph, `build()` panics
    /// without a provider whatever scheme is used.
    fn loopback_client() -> reqwest::Client {
        rustdar_source::tls::init();
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client")
    }

    /// A zone-based alert exactly as the parser admits one: zone URLs, no
    /// geometry of its own, no features.
    fn zone_alert(id: &str, zones: Vec<String>) -> NwsAlert {
        NwsAlert {
            id: id.to_string(),
            event: "Tornado Warning".to_string(),
            category: AlertCategory::Warning,
            severity: "Severe".parse().unwrap(),
            urgency: "Immediate".parse().unwrap(),
            certainty: "Observed".parse().unwrap(),
            headline: None,
            description: String::new(),
            instruction: None,
            area_desc: String::new(),
            sender_name: String::new(),
            effective: String::new(),
            expires: String::new(),
            onset: None,
            ends: None,
            affected_zones: zones,
            features: Arc::new(Vec::new()),
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime")
    }

    /// Everything resolves: three counties, three outlines, nothing to report.
    ///
    /// The counterweight to the two below — without it, a resolver that failed
    /// on everything would satisfy them both.
    #[test]
    fn a_round_that_resolves_every_zone_reports_nothing_missing() {
        let routes: HashMap<String, (u16, String)> = (0..3)
            .map(|i| {
                (
                    format!("/zones/county/OKC{i:03}"),
                    (200, zone_body(35.0 + f64::from(i))),
                )
            })
            .collect();
        let base = serve(routes);
        let urls: Vec<String> = (0..3)
            .map(|i| format!("{base}/zones/county/OKC{i:03}"))
            .collect();
        let mut alerts = vec![zone_alert("a", urls)];

        let resolution = runtime().block_on(resolve_zone_geometries(
            &loopback_client(),
            &mut alerts,
            None,
        ));

        assert_eq!(alerts[0].features.len(), 3, "all three counties must draw");
        assert_eq!(
            resolution,
            ZoneResolution {
                alerts_expected: 1,
                alerts_complete: 1,
                alerts_partial: 0,
                alerts_missing: 0,
                zones_requested: 3,
                zones_resolved: 3,
                failures: Vec::new(),
            },
        );
        assert!(
            resolution.completeness().is_complete(),
            "a whole round must not mark the layer",
        );
    }

    /// **The observed bug.** Every zone fails, so the alert draws nothing — and
    /// the round still has to say so, because the alert fetch itself succeeded
    /// and nothing else on screen will.
    ///
    /// The failure is counted by cause, not merely counted: 503 and a body that
    /// is not JSON are different things to whoever is reading the panel.
    #[test]
    fn a_round_that_resolves_no_zone_says_the_alert_draws_nothing() {
        let base = serve(HashMap::from([(
            "/zones/county/OKC001".to_string(),
            (200, "this is not JSON".to_string()),
        )]));
        let mut alerts = vec![zone_alert(
            "a",
            vec![
                format!("{base}/zones/county/OKC000"),
                format!("{base}/zones/county/OKC001"),
            ],
        )];

        let resolution = runtime().block_on(resolve_zone_geometries(
            &loopback_client(),
            &mut alerts,
            None,
        ));

        assert!(
            alerts[0].features.is_empty(),
            "premise: nothing resolved, so nothing draws",
        );
        assert_eq!(
            alerts.len(),
            1,
            "the alert must keep its place in the list - it is not dropped and \
             nothing is invented for it",
        );
        assert_eq!(resolution.alerts_missing, 1);
        assert_eq!(resolution.alerts_complete, 0);
        assert_eq!(resolution.zones_resolved, 0);
        assert_eq!(
            resolution.failures,
            vec![(ZoneFailure::Http(500), 1), (ZoneFailure::Unreadable, 1)],
            "each cause is counted separately: a refusal and a body that would \
             not parse are different faults",
        );

        let note = resolution
            .completeness()
            .status_note()
            .expect("a round that placed nothing must say so");
        assert!(
            note.contains("missing 1 of 1 alerts") && note.contains("0 of 2 zone boundaries"),
            "the note must be countable: {note}",
        );
        assert!(
            note.contains("HTTP 500") && note.contains("unreadable"),
            "the note must say why, not only how many: {note}",
        );
    }

    /// **The worst case, and the one nothing could see.** Two of three counties
    /// resolve, so the alert draws a real outline that is the *wrong* outline —
    /// and unlike a missing alert, there is nothing about the picture that looks
    /// wrong at all.
    ///
    /// Counted apart from `alerts_missing` for exactly that reason: "212 not on
    /// the map" and "6 drawing two thirds of themselves" are different things to
    /// tell someone standing under the third county.
    #[test]
    fn a_round_that_resolves_some_of_an_alerts_zones_reports_it_as_partial() {
        let base = serve(HashMap::from([
            ("/zones/county/OKC000".to_string(), (200, zone_body(35.0))),
            ("/zones/county/OKC001".to_string(), (200, zone_body(36.0))),
            // OKC002 is unrouted, so it answers 500.
        ]));
        let mut alerts = vec![zone_alert(
            "a",
            (0..3)
                .map(|i| format!("{base}/zones/county/OKC{i:03}"))
                .collect(),
        )];

        let resolution = runtime().block_on(resolve_zone_geometries(
            &loopback_client(),
            &mut alerts,
            None,
        ));

        assert_eq!(
            alerts[0].features.len(),
            2,
            "the two that resolved must still draw - a partial answer is not \
             thrown away",
        );
        assert_eq!(resolution.alerts_partial, 1);
        assert_eq!(resolution.alerts_missing, 0);
        assert_eq!(resolution.alerts_complete, 0);
        assert_eq!(
            (resolution.zones_resolved, resolution.zones_requested),
            (2, 3)
        );
        assert_eq!(resolution.failures, vec![(ZoneFailure::Http(500), 1)]);

        let note = resolution
            .completeness()
            .status_note()
            .expect("a partly drawn alert must say so");
        assert!(
            note.contains("1 of 1 alerts drawing only part of their area"),
            "a wrong shape must not be reported as a missing one: {note}",
        );
    }

    /// An alert is whole when **its own** zones are all there, however badly the
    /// rest of the round went. Two alerts, one county each, one of the two
    /// counties dead: one complete and one missing, not two partials.
    #[test]
    fn each_alert_is_judged_against_its_own_zones_and_not_the_round() {
        let base = serve(HashMap::from([(
            "/zones/county/OKC000".to_string(),
            (200, zone_body(35.0)),
        )]));
        let mut alerts = vec![
            zone_alert("a", vec![format!("{base}/zones/county/OKC000")]),
            zone_alert("b", vec![format!("{base}/zones/county/OKC001")]),
        ];

        let resolution = runtime().block_on(resolve_zone_geometries(
            &loopback_client(),
            &mut alerts,
            None,
        ));

        assert_eq!(resolution.alerts_complete, 1);
        assert_eq!(resolution.alerts_missing, 1);
        assert_eq!(resolution.alerts_partial, 0);
        assert_eq!(alerts[0].features.len(), 1);
        assert!(alerts[1].features.is_empty());
    }

    /// An alert that arrived with its own geometry is not this pass's business:
    /// it is not counted, and its features are left exactly as they were. Without
    /// this the many alerts that carry *both* a polygon and an `affectedZones`
    /// list would read as 1-of-5 partial for ever.
    #[test]
    fn an_alert_that_brought_its_own_geometry_is_not_counted_as_a_zone_alert() {
        let base = serve(HashMap::new());
        let mut inline = zone_alert("a", vec![format!("{base}/zones/county/OKC000")]);
        Arc::make_mut(&mut inline.features).push(OverlayFeature::new(
            vec![vec![vec![(35.0, -97.0), (36.0, -97.0), (36.0, -96.0)]]],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
            "Tornado Warning".to_string(),
            String::new(),
            HatchPattern::None,
        ));
        let mut alerts = vec![inline];

        let resolution = runtime().block_on(resolve_zone_geometries(
            &loopback_client(),
            &mut alerts,
            None,
        ));

        assert_eq!(
            resolution,
            ZoneResolution::default(),
            "an alert carrying its own polygon asks nothing of zone resolution",
        );
        assert_eq!(alerts[0].features.len(), 1, "its own geometry is untouched");
    }

    /// A body that parses but carries nothing drawable is its own cause, not a
    /// transport failure — the panel line is what a user reads back, and
    /// "unreachable" for a zone the origin served would send them to their
    /// router.
    #[test]
    fn a_zone_with_no_drawable_boundary_is_reported_as_such() {
        let base = serve(HashMap::from([(
            "/zones/county/OKC000".to_string(),
            (200, r#"{"type":"Feature","geometry":null}"#.to_string()),
        )]));
        let mut alerts = vec![zone_alert("a", vec![format!("{base}/zones/county/OKC000")])];

        let resolution = runtime().block_on(resolve_zone_geometries(
            &loopback_client(),
            &mut alerts,
            None,
        ));

        assert_eq!(resolution.failures, vec![(ZoneFailure::NoBoundary, 1)]);
        assert_eq!(resolution.alerts_missing, 1);
    }

    /// The contract [`distinct_zone_urls`] is allowed to be fast within: each
    /// URL once, in the order the alerts first name it, and an alert that
    /// brought its own geometry contributes nothing.
    #[test]
    fn the_zone_list_is_first_seen_order_with_no_repeats() {
        let mut carries_geometry = zone_alert("has-own", vec!["z/never".to_string()]);
        Arc::make_mut(&mut carries_geometry.features).push(OverlayFeature::new(
            Vec::new(),
            [0; 4],
            [0; 4],
            String::new(),
            String::new(),
            HatchPattern::None,
        ));
        let alerts = vec![
            zone_alert("a", vec!["z/c".to_string(), "z/a".to_string()]),
            carries_geometry,
            zone_alert("b", vec!["z/a".to_string(), "z/b".to_string()]),
            zone_alert("c", vec!["z/b".to_string(), "z/c".to_string()]),
        ];

        assert_eq!(distinct_zone_urls(&alerts), vec!["z/c", "z/a", "z/b"]);
    }

    /// A round costs one pass over its zone references, not one scan of
    /// everything already collected per reference.
    ///
    /// **A count of URLs looked at, not a duration.** The property is a
    /// traversal count, and a traversal count is the same integer on every
    /// machine under every load; a duration is a statement about the machine
    /// that took it. This test used to divide two `Instant`s and assert the
    /// ratio was under 8, and on a contended box the numerator and denominator
    /// sample different slices of a machine that is busy with something else —
    /// it read 11.2x at a load average of 46 with nothing whatever wrong with
    /// the code, and no number of repeats rescues a ratio, because both halves
    /// of it are noise.
    ///
    /// [`SeenUrl`] is the one place a zone URL is hashed or compared, so
    /// [`take_url_examinations`] is the whole cost of the dedup. It is asserted
    /// in **two halves, and the first one is not optional**: that the count is
    /// there at all and is the size one pass is — **between one and five
    /// examinations per reference, and a little under three in fact** — and
    /// only then that the per-reference figure does not move when the round
    /// grows tenfold. A counter that had gone dead reads zero at both sizes,
    /// and zero is perfectly invariant under anything; a test asserting only
    /// the shape would pass on it while protecting nothing.
    ///
    /// Where the three comes from: one hash per reference, plus the set's own
    /// rehashes as it grows, which `hashbrown` pays by re-hashing every element
    /// it already holds at each doubling and which sum to about two more each.
    /// The band is one to five rather than something tighter because that
    /// number is a property of the set's growth policy, not of this code, and a
    /// band that tracked it exactly would red-gate on a dependency bump. Even
    /// five is a factor of two hundred below the rescan at this size.
    ///
    /// Reverting the set to `needed_urls.contains` looks at
    /// `references × distinct / 2` instead: a thousand times as many at the
    /// small size, growing a hundredfold rather than tenfold when the round
    /// does. The live round this was written for -- 1,904 references, 1,690
    /// distinct -- cost 22.5 ms on Firefox's main thread that way, per alert
    /// poll.
    #[test]
    fn the_zone_list_costs_one_pass_not_one_scan_per_url() {
        /// Zones per alert, as the feed groups them.
        const PER_ALERT: usize = 8;
        /// Distinct URLs at the small size, each named exactly once, so this is
        /// the reference count too.
        const SMALL: usize = 2_000;
        /// ...and at the large one. Ten rather than the two that would separate
        /// linear from quadratic, so that an implementation scaling only partly
        /// has less room to sit: one pass grows tenfold and a rescan per URL a
        /// hundredfold. The threshold sits at twenty, which is 2x above linear
        /// and 5x below quadratic -- not the same margin on both sides, and the
        /// tight side is the one that would cry wolf.
        const LARGE: usize = SMALL * 10;

        /// Full-length zone URLs, because the compare being counted here is a
        /// `String` compare and short strings would flatter it.
        fn round(distinct: usize) -> Vec<NwsAlert> {
            (0..distinct.div_ceil(PER_ALERT))
                .map(|alert| {
                    zone_alert(
                        &format!("a{alert}"),
                        (0..PER_ALERT)
                            .map(|zone| {
                                let n = alert * PER_ALERT + zone;
                                format!("https://api.weather.gov/zones/county/ZZC{n:06}")
                            })
                            .collect(),
                    )
                })
                .collect()
        }

        /// What one pass over `alerts` looked at, with the tally taken fresh
        /// first so that nothing the fixture did is counted in it.
        fn examinations(alerts: &[NwsAlert]) -> u64 {
            let _ = take_url_examinations();
            let urls = distinct_zone_urls(alerts);
            let examined = take_url_examinations();
            assert_eq!(urls.len(), alerts.len() * PER_ALERT);
            examined
        }

        let small = examinations(&round(SMALL));
        let large = examinations(&round(LARGE));

        // Half one, and the half that does the work: the number is *there*, and
        // it is the size one pass is. A counter that had gone dead reads zero at
        // both sizes, and zero is perfectly invariant under anything, so a
        // ratio alone would pass over it.
        for (examined, references, size) in [
            (small, SMALL as u64, "small"),
            (large, LARGE as u64, "large"),
        ] {
            assert!(
                (references..references * 5).contains(&examined),
                "the {size} round looked at {examined} URLs for {references} \
                 references, {:.2} each, where one pass is a little under three \
                 -- a hash apiece, and about two more amortized over the \
                 set's rehashes as it grows. Below one, the dedup is no longer \
                 deciding through the membership test this counts and nothing \
                 here means anything; above five it is scanning. A rescan per \
                 URL looks at {}.",
                examined as f64 / references as f64,
                references * references / 2,
            );
        }

        // Half two: and it does not grow when the round does. Mostly implied by
        // half one, which bounds both sizes per reference -- it bites on its own
        // only where the small round comes in under 5,000 -- but it is the
        // sentence the test is named for and it names the two growth curves in
        // the failure, so it stays.
        assert!(
            large < small * 20,
            "ten times the round looked at {:.1}x the URLs ({small} -> \
             {large}). One pass grows 10x and a rescan-per-URL grows 100x, so \
             this is the rescan.",
            large as f64 / small as f64,
        );
    }

    /// Nothing to do is not a fault: a round of alerts that all carry geometry
    /// leaves the layer unmarked.
    #[test]
    fn a_round_with_no_zone_alerts_reports_nothing() {
        let mut alerts: Vec<NwsAlert> = Vec::new();
        let resolution = runtime().block_on(resolve_zone_geometries(
            &loopback_client(),
            &mut alerts,
            None,
        ));
        assert_eq!(resolution, ZoneResolution::default());
        assert_eq!(resolution.completeness().status_mark(), None);
    }
}
