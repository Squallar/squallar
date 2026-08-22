use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicBool, Ordering};

use crate::fetch_policy::DataCompleteness;
use crate::types::{HatchPattern, OverlayFeature};
use rustdar_geo::GeoPolygon;

use super::alert::NwsAlert;
use super::colors::alert_color;
use super::zone_pack::{self, Kind, ZonePack};

#[cfg(not(target_arch = "wasm32"))]
const CACHE_TTL_SECS: u64 = 365 * 24 * 3600;

/// Which simplification produced the polygons on disk.
///
/// The cache stores the *simplified* rings for a year, so a change to
/// [`crate::render::geo::simplify_ring`] does not reach a zone already on disk.
/// An entry with a different value, or with none, is refetched.
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
    schema: u32,
    fetched_at: u64,
    polygons: Vec<GeoPolygon>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ZoneFailure {
    /// The request never produced a status: DNS, TLS, a dropped connection, a
    /// timeout — or a CORS rejection wearing the browser's opaque `TypeError`.
    Unreachable,
    Http(u16),
    Unreadable,
    /// Parsed, and carried nothing this renderer can draw. No zone in the
    /// published corpus reaches this variant — measured over all 11,651 — so a
    /// count against it means the origin sent something undrawable.
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

/// What one pass of [`resolve_zone_geometries`] managed. Alerts are counted in
/// three buckets rather than two because the middle one is the worst: a
/// partly-resolved alert draws a real shape that is the wrong shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneResolution {
    /// Alerts that arrived with zone references and no geometry of their own.
    pub alerts_expected: usize,
    /// ...that ended up with every zone they named.
    pub alerts_complete: usize,
    /// ...that ended up with some of them.
    pub alerts_partial: usize,
    /// ...that ended up with none, and so draw nothing at all.
    pub alerts_missing: usize,
    pub zones_requested: usize,
    pub zones_resolved: usize,
    /// Why the rest were not obtained, commonest first.
    pub failures: Vec<(ZoneFailure, usize)>,
}

impl ZoneResolution {
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

fn needs_zones(alert: &NwsAlert) -> bool {
    alert.features.is_empty() && !alert.affected_zones.is_empty()
}

/// A zone URL as the seen-set in [`distinct_zone_urls`] keys it.
///
/// A newtype over the `&str` because it is then the one place a zone URL is
/// hashed or compared, and under `cfg(test)` those impls tally each examination
/// — so the dedup's cost is a count of URLs looked at, not a ratio of two
/// `Instant`s. Stable within a band, not exact: `hashbrown` calls `eq` only on a
/// control-byte collision.
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
    /// Zone URLs examined on this thread since [`take_url_examinations`] last
    /// took the tally. Thread-local: the suite runs its tests in parallel threads.
    static URL_EXAMINATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_url_examination() {
    URL_EXAMINATIONS.with(|n| n.set(n.get() + 1));
}

#[cfg(test)]
fn take_url_examinations() -> u64 {
    URL_EXAMINATIONS.with(|n| n.replace(0))
}

/// Every zone boundary a round has to obtain, once each, in the order the
/// alerts first name them.
///
/// First-seen order is part of the contract: it is the order the fetches are
/// issued in. The set beside the `Vec` is the difference between one pass and
/// `n²/2` string compares — a measured live round (1,904 references, 1,690
/// distinct) spent **22.5 ms in Firefox and 13.7 ms in Chrome** rescanning.
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
/// what it managed. URLs are deduplicated.
///
/// Resolves against the installed zone pack first — see
/// [`resolve_zone_geometries_from`], which is this without the global.
///
/// An alert whose zones did not resolve keeps its place in the list with an
/// empty feature vector; no stand-in geometry is ever produced for it.
pub async fn resolve_zone_geometries(
    client: &reqwest::Client,
    alerts: &mut [NwsAlert],
    cache_dir: Option<&Path>,
) -> ZoneResolution {
    // Bound before borrowing: `installed()` hands back an `Arc` that would
    // otherwise be a temporary dropped at the end of this expression.
    let pack = zone_pack::installed();
    resolve_zone_geometries_from(client, alerts, cache_dir, pack.as_deref()).await
}

/// [`resolve_zone_geometries`] with the pack passed rather than read off the
/// process-wide slot, so a test states which pack it is resolving against
/// instead of inheriting whatever another test in the binary installed.
pub async fn resolve_zone_geometries_from(
    client: &reqwest::Client,
    alerts: &mut [NwsAlert],
    cache_dir: Option<&Path>,
    pack: Option<&ZonePack>,
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
    let mut from_pack = 0usize;
    let mut from_disk = 0usize;

    for url in &needed_urls {
        // The pack before anything else: it is one already-resident file with
        // no syscall and no request per zone, and it is the only source web has
        // at all. A miss here is routine — the pack is one published edition
        // and the alerts feed still names ids that edition retired.
        if let Some(polys) = pack.and_then(|pack| zone_geometry_from_pack(pack, url)) {
            from_pack += 1;
            zone_cache.insert(url.clone(), polys);
            continue;
        }
        let cached = match cache_dir {
            Some(dir) => read_cached_zone(dir, url).await,
            None => None,
        };
        if let Some(polys) = cached {
            from_disk += 1;
            zone_cache.insert(url.clone(), polys);
        } else {
            urls_to_fetch.push(url.clone());
        }
    }

    log::info!(
        "Zone geometries: {from_pack} from the pack, {from_disk} cached, {} to fetch",
        urls_to_fetch.len(),
    );

    // Tallied by cause rather than kept per URL: a thousand-line list of
    // counties is not a sentence anyone reads.
    let mut failures: BTreeMap<ZoneFailure, usize> = BTreeMap::new();

    if !urls_to_fetch.is_empty() {
        // Bounded: unbounded exhausts file descriptors on low-ulimit systems.
        use futures::stream::{self, StreamExt};
        const MAX_CONCURRENT_FETCHES: usize = 10;

        let results: Vec<_> = stream::iter(urls_to_fetch.into_iter().map(|url| {
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
                // The alerts of this round are not yet published, so the `Arc`
                // is still uniquely held and `make_mut` mutates in place.
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
        // WARN, not INFO: this is the layer under-drawing.
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

/// The zones API returns a bare Feature, not a FeatureCollection: `geometry` is
/// at the top level. County rings run 100+ vertices each, finer than the map
/// shows, so they are simplified here.
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

#[cfg(not(target_arch = "wasm32"))]
fn unix_now() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `https://api.weather.gov/zones/county/TXC113` → `(County, "TXC113")`, the
/// pair the pack's index is keyed on.
///
/// The same `(kind, id)` pair [`zone_cache_key`] spells as a filename, typed:
/// `FLC087` alone names four different geometries, and a lookup that dropped
/// the kind would answer one of them for all four.
///
/// `None` if the URL does not end in `<kind>/<id>` with a kind the pack knows,
/// which leaves that zone on the HTTP path rather than guessing at it.
fn zone_kind_and_id(url: &str) -> Option<(Kind, &str)> {
    let trimmed = url.trim_end_matches('/');
    let mut segments = trimmed.rsplit('/');
    let id = segments.next().filter(|s| !s.is_empty())?;
    let kind = Kind::from_url_segment(segments.next()?)?;
    Some((kind, id))
}

/// One zone out of the pack, or `None` to carry on to the cache and the
/// network.
///
/// The emptiness filter is not paranoia about the reader: it is the difference
/// between a zone that resolved and one that resolved to nothing drawable. A
/// `Some(vec![])` here would be counted as resolved and paint no area, which is
/// exactly the silent under-draw `ZoneResolution` exists to make visible.
fn zone_geometry_from_pack(pack: &ZonePack, url: &str) -> Option<Vec<GeoPolygon>> {
    let (kind, id) = zone_kind_and_id(url)?;
    pack.get(kind, id)
        .filter(|polygons| polygons.iter().any(|polygon| !polygon.is_empty()))
}

/// `https://api.weather.gov/zones/county/TXC113` → `"county_TXC113"`: the kind
/// must stay in the key, since the same id exists under several zone kinds.
#[cfg(not(target_arch = "wasm32"))]
fn zone_cache_key(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let mut parts = trimmed.rsplit('/');
    let id = parts.next().filter(|s| !s.is_empty())?;
    let kind = parts.next().unwrap_or("zone");
    Some(format!("{kind}_{id}"))
}

/// `None` if missing, corrupt, written by a different simplification, or past
/// the TTL. A file that will not parse will not parse next poll either, so a
/// corrupt entry is removed rather than re-read for every alert naming its zone.
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

// Web: no filesystem. Same signatures rather than cfg at the call sites, so the
// caching *policy* has one body on every target. On web every zone is re-fetched
// each session, and the browser's own HTTP cache absorbs it.

#[cfg(target_arch = "wasm32")]
async fn read_cached_zone(_cache_dir: &Path, _url: &str) -> Option<Vec<GeoPolygon>> {
    None
}

#[cfg(target_arch = "wasm32")]
async fn write_cached_zone(_cache_dir: &Path, _url: &str, _polygons: &[GeoPolygon]) {}

#[cfg(test)]
mod zone_geometry_tests;

// Native-only: the loopback stub is `std::net::TcpListener` and a real thread.
// The code under test is not gated.

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::nws::alert::{AlertCategory, NwsAlert};

    fn zone_body(lat: f64) -> String {
        format!(
            r#"{{"type":"Feature","geometry":{{"type":"Polygon","coordinates":
             [[[-97.0,{lat}],[-97.0,{}],[-96.0,{}],[-96.0,{lat}],[-97.0,{lat}]]]}}}}"#,
            lat + 1.0,
            lat + 1.0,
        )
    }

    /// Serve canned responses by path from a loopback socket, forever. An
    /// unrouted path answers 500, so a test states only what it wants to succeed.
    fn serve(routes: HashMap<String, (u16, String)>) -> String {
        serve_counting(routes).0
    }

    /// [`serve`], plus the count of requests it has actually been asked for.
    ///
    /// The instrument the pack's whole reason for existing is measured on. It
    /// counts accepted connections, and every response closes, so it is a count
    /// of requests — including the ones that 500, so a route that is not there
    /// still registers as having been asked for.
    fn serve_counting(
        routes: HashMap<String, (u16, String)>,
    ) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counter.fetch_add(1, Ordering::Relaxed);
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
        (format!("http://127.0.0.1:{port}"), requests)
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
            valid_from: None,
            valid_until: None,
            affected_zones: zones,
            features: Arc::new(Vec::new()),
        }
    }

    /// Spelled out at every call, because the alternative reads the
    /// process-wide slot — and a pack another test in this binary installed
    /// would silently resolve a zone this one arranged to fail. That is not
    /// hypothetical: it turned two of the tests below red the first time the
    /// whole suite ran together.
    const NO_PACK: Option<&ZonePack> = None;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime")
    }

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

        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            NO_PACK,
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
    /// the round still has to say so, counted by cause.
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

        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            NO_PACK,
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
    /// resolve, so the alert draws an outline that is the *wrong* outline.
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

        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            NO_PACK,
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
    /// rest of the round went.
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

        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            NO_PACK,
        ));

        assert_eq!(resolution.alerts_complete, 1);
        assert_eq!(resolution.alerts_missing, 1);
        assert_eq!(resolution.alerts_partial, 0);
        assert_eq!(alerts[0].features.len(), 1);
        assert!(alerts[1].features.is_empty());
    }

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

        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            NO_PACK,
        ));

        assert_eq!(
            resolution,
            ZoneResolution::default(),
            "an alert carrying its own polygon asks nothing of zone resolution",
        );
        assert_eq!(alerts[0].features.len(), 1, "its own geometry is untouched");
    }

    #[test]
    fn a_zone_with_no_drawable_boundary_is_reported_as_such() {
        let base = serve(HashMap::from([(
            "/zones/county/OKC000".to_string(),
            (200, r#"{"type":"Feature","geometry":null}"#.to_string()),
        )]));
        let mut alerts = vec![zone_alert("a", vec![format!("{base}/zones/county/OKC000")])];

        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            NO_PACK,
        ));

        assert_eq!(resolution.failures, vec![(ZoneFailure::NoBoundary, 1)]);
        assert_eq!(resolution.alerts_missing, 1);
    }

    /// The contract [`distinct_zone_urls`] is allowed to be fast within: each URL
    /// once, in first-name order, and an alert with its own geometry adds none.
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
    /// **A count of URLs looked at, not a duration.** [`SeenUrl`] is the one
    /// place a zone URL is hashed or compared. Asserted in two halves: that the
    /// count is there and is the size one pass is — **one to five examinations
    /// per reference** — and then that it does not move when the round grows
    /// tenfold. A dead counter reads zero at both sizes.
    #[test]
    fn the_zone_list_costs_one_pass_not_one_scan_per_url() {
        const PER_ALERT: usize = 8;
        /// Distinct URLs at the small size — the reference count too.
        const SMALL: usize = 2_000;
        /// ...and at the large one. Ten rather than two: one pass grows tenfold
        /// and a rescan per URL a hundredfold.
        const LARGE: usize = SMALL * 10;

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

        fn examinations(alerts: &[NwsAlert]) -> u64 {
            let _ = take_url_examinations();
            let urls = distinct_zone_urls(alerts);
            let examined = take_url_examinations();
            assert_eq!(urls.len(), alerts.len() * PER_ALERT);
            examined
        }

        let small = examinations(&round(SMALL));
        let large = examinations(&round(LARGE));

        // Half one: the number is *there*, and it is the size one pass is.
        // A dead counter reads zero at both sizes, so a ratio alone would pass.
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

        // Half two: it does not grow when the round does. Mostly implied by half
        // one, but it names the two growth curves in the failure.
        assert!(
            large < small * 20,
            "ten times the round looked at {:.1}x the URLs ({small} -> \
             {large}). One pass grows 10x and a rescan-per-URL grows 100x, so \
             this is the rescan.",
            large as f64 / small as f64,
        );
    }

    #[test]
    fn a_round_with_no_zone_alerts_reports_nothing() {
        let mut alerts: Vec<NwsAlert> = Vec::new();
        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            NO_PACK,
        ));
        assert_eq!(resolution, ZoneResolution::default());
        assert_eq!(resolution.completeness().status_mark(), None);
    }

    // ── the pack ─────────────────────────────────────────────────────────
    //
    // The pack is built here by `zone_pack::write`, the same encoder
    // `tools/nws-zone-pack` calls, and the geometry it carries is deliberately
    // nowhere near the geometry the loopback origin serves — so every assertion
    // below can say *which source answered*, not merely that something did.

    use crate::nws::zone_pack::{self, Coding, Kind, PackedZone, ZonePack};

    /// A square the origin never serves: the origin's zones sit at lon -97..-96
    /// and these at -100..-99, so one look at a drawn vertex says where it came
    /// from.
    fn packed_square(lat: f64) -> Vec<GeoPolygon> {
        vec![vec![vec![
            (lat, -100.0),
            (lat, -99.0),
            (lat + 1.0, -99.0),
            (lat + 1.0, -100.0),
            (lat, -100.0),
        ]]]
    }

    fn pack_of(zones: &[(Kind, &str, f64)]) -> ZonePack {
        let mut entries: Vec<PackedZone> = zones
            .iter()
            .map(|&(kind, ugc, lat)| {
                (
                    zone_pack::key(kind, ugc).expect("a six-character UGC"),
                    packed_square(lat),
                )
            })
            .collect();
        entries.sort_by_key(|(key, _)| *key);
        ZonePack::open(zone_pack::write(&entries, Coding::Varint, 5, 0.005))
            .expect("a pack of real squares must open")
    }

    /// Every drawn vertex, so a test can assert which origin the shape came
    /// from rather than only that a shape arrived.
    fn drawn_vertices(alert: &NwsAlert) -> Vec<(f64, f64)> {
        alert
            .features
            .iter()
            .flat_map(|feature| feature.polygons.iter())
            .flat_map(|polygon| polygon.iter())
            .flat_map(|ring| ring.iter().copied())
            .collect()
    }

    /// **The point of the whole exercise, measured.** Three zones that are in
    /// the pack draw without the origin being asked once — and the origin is
    /// sitting there able to answer, so a resolver that ignored the pack would
    /// pass every other assertion here and fail only this count.
    #[test]
    fn a_zone_in_the_pack_draws_without_one_request() {
        let (base, requests) = serve_counting(
            (0..3)
                .map(|i| {
                    (
                        format!("/zones/county/OKC{i:03}"),
                        (200, zone_body(35.0 + f64::from(i))),
                    )
                })
                .collect(),
        );
        let pack = pack_of(&[
            (Kind::County, "OKC000", 35.0),
            (Kind::County, "OKC001", 36.0),
            (Kind::County, "OKC002", 37.0),
        ]);
        let mut alerts = vec![zone_alert(
            "a",
            (0..3)
                .map(|i| format!("{base}/zones/county/OKC{i:03}"))
                .collect(),
        )];

        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            Some(&pack),
        ));

        assert_eq!(
            requests.load(Ordering::Relaxed),
            0,
            "the origin was asked for a zone the pack already carries",
        );
        assert_eq!(alerts[0].features.len(), 3, "all three must still draw");
        assert_eq!(
            (resolution.zones_resolved, resolution.zones_requested),
            (3, 3)
        );
        assert_eq!(resolution.alerts_complete, 1);
        assert!(resolution.completeness().is_complete());

        let vertices = drawn_vertices(&alerts[0]);
        assert_eq!(vertices.len(), 15, "three squares of five points");
        assert!(
            vertices.iter().all(|&(_, lon)| lon <= -99.0),
            "the shapes drawn are not the pack's: {vertices:?}",
        );
    }

    /// **The other half, and the one that keeps the fallback honest.** 0.70% of
    /// live ids miss the shipped edition — pure retirement — and every one of
    /// them must still resolve. A pack that swallowed its misses would turn a
    /// retired id into an alert that draws nothing.
    #[test]
    fn a_zone_absent_from_the_pack_still_resolves_over_http() {
        let (base, requests) = serve_counting(HashMap::from([
            ("/zones/county/OKC000".to_string(), (200, zone_body(35.0))),
            ("/zones/county/OKC001".to_string(), (200, zone_body(36.0))),
        ]));
        // Only the first is in the pack; the second is the retired id.
        let pack = pack_of(&[(Kind::County, "OKC000", 35.0)]);
        let mut alerts = vec![zone_alert(
            "a",
            vec![
                format!("{base}/zones/county/OKC000"),
                format!("{base}/zones/county/OKC001"),
            ],
        )];

        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            Some(&pack),
        ));

        assert_eq!(
            requests.load(Ordering::Relaxed),
            1,
            "exactly the one zone the pack does not carry may be fetched",
        );
        assert_eq!(alerts[0].features.len(), 2, "both zones must draw");
        assert_eq!(resolution.alerts_complete, 1);
        assert_eq!(resolution.alerts_partial, 0);
        assert!(
            resolution.completeness().is_complete(),
            "a fetched tail is not an incomplete round",
        );

        let vertices = drawn_vertices(&alerts[0]);
        let from_pack = vertices.iter().filter(|&&(_, lon)| lon <= -99.0).count();
        let from_origin = vertices.iter().filter(|&&(_, lon)| lon >= -97.0).count();
        assert_eq!(
            (from_pack, from_origin),
            (5, 5),
            "one square from each source: {vertices:?}",
        );
    }

    /// The join is on the **pair**. `FLC087` under `fire` is a different shape
    /// from `FLC087` under `county`, and a resolver keyed on the bare id would
    /// answer the county's shape for the fire zone: a real, filled, correctly
    /// coloured polygon in the wrong place.
    #[test]
    fn the_pack_is_joined_on_kind_and_ugc_not_on_the_ugc_alone() {
        let (base, requests) = serve_counting(HashMap::from([(
            "/zones/fire/FLC087".to_string(),
            (200, zone_body(24.0)),
        )]));
        let pack = pack_of(&[(Kind::County, "FLC087", 40.0), (Kind::Fire, "FLC087", 25.0)]);
        let mut alerts = vec![zone_alert("a", vec![format!("{base}/zones/fire/FLC087")])];

        runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            Some(&pack),
        ));

        assert_eq!(requests.load(Ordering::Relaxed), 0, "it is in the pack");
        let vertices = drawn_vertices(&alerts[0]);
        assert!(
            vertices
                .iter()
                .all(|&(lat, _)| (25.0..=26.0).contains(&lat)),
            "the fire zone drew the county zone's shape: {vertices:?}",
        );
    }

    /// ...and when only the *other* kind is present, the pack must miss rather
    /// than answer with the shape it does have.
    #[test]
    fn a_ugc_present_under_another_kind_is_a_miss_and_goes_to_the_origin() {
        let (base, requests) = serve_counting(HashMap::from([(
            "/zones/fire/FLC087".to_string(),
            (200, zone_body(24.0)),
        )]));
        let pack = pack_of(&[(Kind::County, "FLC087", 40.0)]);
        let mut alerts = vec![zone_alert("a", vec![format!("{base}/zones/fire/FLC087")])];

        let resolution = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            Some(&pack),
        ));

        assert_eq!(
            requests.load(Ordering::Relaxed),
            1,
            "the county shape must not stand in for the fire zone",
        );
        assert_eq!(resolution.zones_resolved, 1);
        let vertices = drawn_vertices(&alerts[0]);
        assert!(
            vertices
                .iter()
                .all(|&(lat, _)| (24.0..=25.0).contains(&lat)),
            "the origin's answer is what must be drawn: {vertices:?}",
        );
    }

    /// A URL whose kind segment the pack does not know stays on the HTTP path.
    /// Guessing a kind is how a zone gets the wrong shape.
    #[test]
    fn an_unrecognised_zone_kind_is_left_to_the_origin() {
        assert_eq!(
            zone_kind_and_id("https://api.weather.gov/zones/county/TXC113"),
            Some((Kind::County, "TXC113")),
            "the control: the shape every zone URL has",
        );
        assert_eq!(
            zone_kind_and_id("https://api.weather.gov/zones/county/TXC113/"),
            Some((Kind::County, "TXC113")),
            "a trailing slash is not a different zone",
        );
        for odd in [
            "https://api.weather.gov/zones/coastal/TXZ213",
            "https://api.weather.gov/zones/TXC113",
            "TXC113",
            "",
        ] {
            assert_eq!(
                zone_kind_and_id(odd),
                None,
                "{odd:?} must be left to the origin, not guessed at",
            );
        }

        let (base, requests) = serve_counting(HashMap::from([(
            "/zones/coastal/TXZ213".to_string(),
            (200, zone_body(28.0)),
        )]));
        let pack = pack_of(&[(Kind::Forecast, "TXZ213", 40.0)]);
        let mut alerts = vec![zone_alert(
            "a",
            vec![format!("{base}/zones/coastal/TXZ213")],
        )];

        runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            Some(&pack),
        ));

        assert_eq!(requests.load(Ordering::Relaxed), 1);
        let vertices = drawn_vertices(&alerts[0]);
        assert!(
            vertices
                .iter()
                .all(|&(lat, _)| (28.0..=29.0).contains(&lat)),
            "an unknown kind must not be resolved out of the forecast set: {vertices:?}",
        );
    }

    /// The control for every count above: with no pack installed, the same
    /// round is the fan-out this work exists to remove. Without this, a
    /// resolver that never issued a request at all would read as a triumph.
    #[test]
    fn without_a_pack_the_same_round_is_one_request_per_zone() {
        let routes: HashMap<String, (u16, String)> = (0..12)
            .map(|i| {
                (
                    format!("/zones/county/OKC{i:03}"),
                    (200, zone_body(35.0 + f64::from(i))),
                )
            })
            .collect();
        let (base, requests) = serve_counting(routes);
        let urls: Vec<String> = (0..12)
            .map(|i| format!("{base}/zones/county/OKC{i:03}"))
            .collect();

        let mut alerts = vec![zone_alert("a", urls.clone())];
        let without = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            None,
        ));
        let fanned_out = requests.swap(0, Ordering::Relaxed);

        let pack = pack_of(
            &(0..12)
                .map(|i| (Kind::County, format!("OKC{i:03}"), 35.0 + f64::from(i)))
                .collect::<Vec<_>>()
                .iter()
                .map(|(kind, ugc, lat)| (*kind, ugc.as_str(), *lat))
                .collect::<Vec<_>>(),
        );
        let mut alerts = vec![zone_alert("a", urls)];
        let with = runtime().block_on(resolve_zone_geometries_from(
            &loopback_client(),
            &mut alerts,
            None,
            Some(&pack),
        ));

        assert_eq!(
            (fanned_out, requests.load(Ordering::Relaxed)),
            (12, 0),
            "twelve zones cost twelve requests without the pack and none with it",
        );
        assert_eq!(
            (without.zones_resolved, with.zones_resolved),
            (12, 12),
            "and both rounds resolved the same twelve zones",
        );
    }
}
