//! Fetch GLM lightning flash data from AWS S3.
//!
//! Lists and downloads NetCDF4 files from the `noaa-goes19`/`noaa-goes18`
//! public S3 buckets. Files are LCFA (Lightning Cluster-Filter Algorithm)
//! Level 2 products, each covering ~20 seconds.

use std::collections::HashMap;

use chrono::{NaiveDateTime, TimeDelta, Utc};

use super::cf;
use super::{
    DeadFeed, FetchFailures, GLM_MIN_TIME_WINDOW_SECS, GlmDataLevel, GlmFetchOutcome, GlmFlash,
    GlmSatellite, LevelFailure,
};

/// Cached GLM file data keyed by S3 object key.
#[derive(Default, Clone)]
pub struct GlmCache {
    entries: HashMap<String, Vec<GlmFlash>>,
}

impl GlmCache {
    /// Remove cached entries whose flashes are entirely outside the time window.
    pub fn evict_before(&mut self, cutoff: NaiveDateTime) {
        self.entries.retain(|_key, flashes| {
            flashes.iter().any(|f| f.time >= cutoff)
        });
    }

    /// Iterate over all cached flashes.
    pub fn all_flashes(&self) -> impl Iterator<Item = &GlmFlash> {
        self.entries.values().flatten()
    }

    /// Check whether a key is already cached.
    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Insert parsed flashes for a given S3 key.
    pub fn insert(&mut self, key: String, flashes: Vec<GlmFlash>) {
        self.entries.insert(key, flashes);
    }
}

/// Fetch GLM data from one or both satellites for the given time window.
///
/// - Lists S3 objects covering the time range
/// - Downloads only files not already in `cache`
/// - Parses NetCDF4 to extract lat/lon/energy/area/time at selected levels
/// - Returns all observations within the time window, plus any satellite whose
///   listing was completely empty
///
/// Empty listings are *reported*, not logged, here: the caller holds the
/// previous poll's state and can therefore say something only when the feed
/// changes, instead of once every poll forever.
pub async fn fetch_glm_flashes(
    client: &reqwest::Client,
    satellites: &[GlmSatellite],
    time_window_secs: f64,
    levels: &[GlmDataLevel],
    cache: &mut GlmCache,
) -> Result<GlmFetchOutcome, String> {
    // The zero-object warning below assumes the queried range is wide enough to
    // always cover an already-published granule. That assumption is the UI's
    // slider minimum; keep a caller from quietly invalidating it.
    debug_assert!(
        time_window_secs >= GLM_MIN_TIME_WINDOW_SECS,
        "GLM time window {time_window_secs}s is below GLM_MIN_TIME_WINDOW_SECS \
         ({GLM_MIN_TIME_WINDOW_SECS}s); a window under S3 publish latency makes \
         the zero-object check report a live feed as dead.",
    );

    let now = Utc::now().naive_utc();
    let window = TimeDelta::milliseconds((time_window_secs * 1000.0) as i64);
    let start = now - window;
    let cutoff = start;

    // Evict old cache entries
    cache.evict_before(cutoff);

    // List & download new files for each satellite
    let mut acc = PollAccumulator::default();
    let mut dead_feeds = Vec::new();
    let mut tally = PollTally::default();

    for &sat in satellites {
        let listing = list_glm_files(client, sat, start, now).await?;

        // A listing that returns no objects *at all* means the feed itself is
        // gone (dead bucket, renamed product path, satellite rotated out of the
        // slot) — not merely a quiet sky. GOES-East silently rendered nothing
        // for over a year because `noaa-goes16` went dead and zero files looked
        // exactly like zero lightning. A listing that returns objects but no
        // in-window flashes is normal and stays silent.
        if listing.objects_seen == 0 {
            dead_feeds.push(DeadFeed {
                satellite: sat,
                bucket: sat.bucket(),
                prefixes: listing.prefixes.clone(),
            });
        }

        let new_keys = plan_downloads(&listing.keys, cache, &mut tally);

        if new_keys.is_empty() {
            continue;
        }
        log::info!("Downloading {} new GLM files from {}", new_keys.len(), sat.display_name());

        // Download in concurrent batches of 20
        let batch = download_and_parse_batch(client, sat, &new_keys, levels).await;
        acc.absorb(sat, levels, batch);
    }

    // Insert new data into cache
    for (key, flashes) in std::mem::take(&mut acc.entries) {
        cache.insert(key, flashes);
    }

    // Return all cached flashes within the window
    let filtered: Vec<GlmFlash> = cache.all_flashes()
        .filter(|f| f.time >= cutoff && f.time <= now)
        .cloned()
        .collect();

    log::info!("GLM: {} flashes in {:.0}s window", filtered.len(), time_window_secs);

    // Failures are *reported*, not logged here, for the same reason dead feeds
    // are: only the caller knows what the previous poll looked like, and only
    // the caller can put it on screen.
    Ok(build_outcome(filtered, dead_feeds, satellites.to_vec(), &tally, acc))
}

/// Assemble the outcome a poll reports.
///
/// Extracted for the same reason the denominator was: this is where each error
/// bucket is bound to the field the UI reads, and inline in the async fetch it
/// was a struct literal no test could reach — the two `summarize_failures` calls
/// could be swapped, turning every 503 into "product change?", with the suite
/// green. Pure, so the binding is pinned by a test.
fn build_outcome(
    flashes: Vec<GlmFlash>,
    dead_feeds: Vec<DeadFeed>,
    queried: Vec<GlmSatellite>,
    tally: &PollTally,
    acc: PollAccumulator,
) -> GlmFetchOutcome {
    GlmFetchOutcome {
        flashes,
        dead_feeds,
        queried,
        parse_failures: summarize_failures(tally.in_window, acc.parse_errors),
        transport_failures: summarize_failures(tally.in_window, acc.transport_errors),
        // Deliberately not routed through `summarize_failures`: a level failure
        // has no meaningful file-count denominator. It is not "3 of 14 files
        // broke", it is "this layer is gone from the product".
        level_failures: acc.level_failures,
        evaluated_levels: acc.evaluated_levels,
    }
}

/// What one poll accumulated across satellites, before it is shaped into the
/// outcome the UI reads.
///
/// Exists for the same reason `plan_downloads` and `build_outcome` do: folding a
/// batch into the running totals used to be four bare `extend` calls inside the
/// async fetch, where no test could reach them. Dropping any one of them — the
/// level failures especially — silently reinstates the bug it was added to
/// report.
#[derive(Default)]
struct PollAccumulator {
    entries: Vec<(String, Vec<GlmFlash>)>,
    parse_errors: Vec<String>,
    transport_errors: Vec<String>,
    level_failures: Vec<LevelFailure>,
    /// (satellite, level) pairs this poll actually gathered evidence about.
    ///
    /// A level can only be found broken by parsing a granule, so a poll that
    /// downloads nothing new — routine, since the 20 s poll interval races the
    /// ~20 s granule cadence — learns nothing about any level. Without this the
    /// caller cannot tell "this layer is healthy again" from "we did not look",
    /// and would announce a recovery that never happened every time the two
    /// clocks slipped past each other.
    evaluated_levels: Vec<(GlmSatellite, GlmDataLevel)>,
}

impl PollAccumulator {
    /// Fold one satellite's batch in.
    fn absorb(&mut self, satellite: GlmSatellite, levels: &[GlmDataLevel], batch: BatchOutcome) {
        self.parse_errors.extend(batch.parse_errors);
        self.transport_errors.extend(batch.transport_errors);
        self.level_failures.extend(batch.level_failures);

        // Evidence requires a granule that actually parsed. A batch where every
        // file failed to download or open tells us nothing about the levels
        // inside them, so it must not read as levels being healthy.
        if !batch.entries.is_empty() {
            for &level in levels {
                self.evaluated_levels.push((satellite, level));
            }
        }
        self.entries.extend(batch.entries);
    }
}

/// Running totals for one poll, accumulated across satellites.
#[derive(Default)]
struct PollTally {
    /// Every file the listings placed in the window — the denominator the
    /// failure ratio is measured against.
    in_window: usize,
}

/// Decide what to download, and record what the window contains.
///
/// The tally is updated *here* rather than by the caller so that exactly one
/// expression in the codebase defines the failure denominator. The caller sits
/// inside a network round trip and cannot be unit-tested, so leaving it to pick
/// between `keys.len()` and `new_keys.len()` would put the decision somewhere
/// no test can reach — which is how the biased denominator survived review.
///
/// The two counts are deliberately different and must stay that way: cached
/// successes drop out of the returned keys while failures are never cached and
/// are retried every poll, so using the download count as the denominator makes
/// a single persistent failure look like a total outage after a few ticks.
fn plan_downloads<'a>(
    keys: &'a [String],
    cache: &GlmCache,
    tally: &mut PollTally,
) -> Vec<&'a str> {
    tally.in_window += keys.len();
    keys.iter()
        .filter(|k| !cache.contains_key(k.as_str()))
        .map(|k| k.as_str())
        .collect()
}

/// Result of listing S3 for one satellite, with enough context to tell an
/// absent feed apart from a quiet one.
struct GlmListing {
    /// Keys that are `.nc` files whose encoded start time falls in the window.
    keys: Vec<String>,
    /// Total objects S3 returned across all prefixes, before any filtering.
    /// Zero means the bucket/prefix has nothing in it whatsoever.
    objects_seen: usize,
    /// Prefixes queried, for diagnostics.
    prefixes: Vec<String>,
}

/// List GLM LCFA file keys on S3 for the given time range.
///
/// S3 path: `GLM-L2-LCFA/{year}/{day_of_year}/{hour}/`
/// Files: `OR_GLM-L2-LCFA_G{sat}_s{start}_e{end}_c{creation}.nc`
async fn list_glm_files(
    client: &reqwest::Client,
    satellite: GlmSatellite,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<GlmListing, String> {
    let bucket = satellite.bucket();
    let mut all_keys = Vec::new();
    let mut objects_seen = 0usize;

    // Collect all (year, doy, hour) tuples we need to query.
    //
    // This emits a *single* prefix whenever `start` and `end` fall in the same
    // UTC hour — with a 60 s window at 00:14:00, iteration 1 pushes `.../00/`,
    // `t` then clamps to `end` and iteration 2 dedups and breaks. So the
    // zero-object warning below can be looking at one young hour prefix, and
    // does not get to assume a previous, already-populated hour is in the set.
    //
    // What keeps that from crying wolf at the top of every hour is a coupling
    // worth stating explicitly:
    //
    //     GLM_MIN_TIME_WINDOW_SECS  >  S3 publish latency for the hour's first object
    //
    // The single-prefix case requires `now >= hour_start + window`, and the
    // 00:00:00 granule of each hour lands 27–30 s after the boundary (measured
    // across two consecutive live hours on noaa-goes19; worst case in that
    // sample was 41 s for a mid-hour file). With the minimum window at 60 s the
    // prefix always holds at least one object by the time a single-prefix query
    // is possible, leaving ~30 s of headroom. Lowering the slider minimum below
    // roughly 45 s would reintroduce a spurious "feed dead" warning once an
    // hour; see GLM_MIN_TIME_WINDOW_SECS in render::handlers::glm.
    let mut prefixes = Vec::new();
    let mut t = start;
    loop {
        let year = t.format("%Y").to_string();
        let doy = t.format("%j").to_string();
        let hour = t.format("%H").to_string();
        let prefix = format!("GLM-L2-LCFA/{year}/{doy}/{hour}/");
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
        if t >= end {
            break;
        }
        // Advance by 1 hour to catch all hour boundaries
        t += TimeDelta::hours(1);
        if t > end {
            t = end;
        }
    }

    for prefix in &prefixes {
        let mut continuation_token: Option<String> = None;
        loop {
            let mut url = format!(
                "https://{bucket}.s3.amazonaws.com/?list-type=2&prefix={prefix}"
            );
            if let Some(ref token) = continuation_token {
                url.push_str("&continuation-token=");
                url.push_str(&urlencoded(token));
            }

            let resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("S3 list request failed: {e}"))?;

            if !resp.status().is_success() {
                return Err(format!("S3 returned HTTP {}", resp.status()));
            }

            let body = resp.text().await
                .map_err(|e| format!("Failed to read S3 list response: {e}"))?;

            let doc = roxmltree::Document::parse(&body)
                .map_err(|e| format!("Failed to parse S3 XML: {e}"))?;

            for node in doc.descendants() {
                if node.tag_name().name() == "Key"
                    && let Some(key) = node.text() {
                        objects_seen += 1;
                        if key.ends_with(".nc") {
                            // Filter by start time encoded in filename
                            if let Some(file_start) = parse_filename_start_time(key)
                                && file_start >= start && file_start <= end {
                                    all_keys.push(key.to_string());
                                }
                        }
                    }
            }

            // Check for truncation (pagination)
            let is_truncated = doc.descendants()
                .find(|n| n.tag_name().name() == "IsTruncated")
                .and_then(|n| n.text())
                .is_some_and(|t| t == "true");

            if !is_truncated {
                break;
            }

            // A truncated response with no usable continuation token would
            // otherwise re-issue the identical first-page request forever,
            // inside an async fetch task with no timeout. Stop and say why.
            let next = doc.descendants()
                .find(|n| n.tag_name().name() == "NextContinuationToken")
                .and_then(|n| n.text())
                .filter(|t| !t.is_empty())
                .map(|s| s.to_string());

            let Some(next) = next else {
                log::warn!(
                    "GLM: S3 reported a truncated listing for '{prefix}' in bucket \
                     '{bucket}' but returned no continuation token; \
                     results for this prefix may be incomplete"
                );
                break;
            };

            // Defensive: a token that repeats would loop just as tightly.
            if continuation_token.as_deref() == Some(next.as_str()) {
                log::warn!(
                    "GLM: S3 repeated the same continuation token for '{prefix}' in \
                     bucket '{bucket}'; stopping pagination to avoid a spin"
                );
                break;
            }

            continuation_token = Some(next);
        }
    }

    Ok(GlmListing {
        keys: all_keys,
        objects_seen,
        prefixes,
    })
}

/// Parse the start timestamp from a GLM filename.
///
/// Filename: `OR_GLM-L2-LCFA_G19_s20261120145200_e...nc`
/// The `s` field is `YYYYDDDHHMMSSf` where DDD = day of year, f = tenths of second.
/// The `G{nn}` satellite token is not inspected, so this works for any GOES bird.
fn parse_filename_start_time(key: &str) -> Option<NaiveDateTime> {
    // Find the `_s` field in the filename
    let filename = key.rsplit('/').next()?;
    let s_idx = filename.find("_s")?;
    let s_field = &filename[s_idx + 2..];
    // Extract YYYYDDDHHMMSSf (14 chars)
    if s_field.len() < 14 {
        return None;
    }
    let digits = &s_field[..14];
    let year: i32 = digits[0..4].parse().ok()?;
    let doy: u32 = digits[4..7].parse().ok()?;
    let hour: u32 = digits[7..9].parse().ok()?;
    let min: u32 = digits[9..11].parse().ok()?;
    let sec: u32 = digits[11..13].parse().ok()?;

    let date = chrono::NaiveDate::from_yo_opt(year, doy)?;
    let time = chrono::NaiveTime::from_hms_opt(hour, min, sec)?;
    Some(NaiveDateTime::new(date, time))
}

/// Outcome of one download batch: what parsed, and what did not.
struct BatchOutcome {
    entries: Vec<(String, Vec<GlmFlash>)>,
    /// One message per file that downloaded but would not parse. Returned
    /// rather than only logged: a batch where *every* file fails renders
    /// exactly like a quiet sky, so the count has to reach the UI.
    parse_errors: Vec<String>,
    /// One message per file that never arrived. Tracked separately so a network
    /// problem is never reported as a product schema change.
    transport_errors: Vec<String>,
    /// Levels that failed inside files that otherwise parsed, deduplicated per
    /// (satellite, level). A schema change hits every granule in the window
    /// identically, so reporting it once per file would be noise.
    level_failures: Vec<LevelFailure>,
}

/// Download and parse a batch of GLM NetCDF files concurrently.
async fn download_and_parse_batch(
    client: &reqwest::Client,
    satellite: GlmSatellite,
    keys: &[&str],
    levels: &[GlmDataLevel],
) -> BatchOutcome {
    use futures::stream::StreamExt;

    let bucket = satellite.bucket();
    let levels_owned: Vec<GlmDataLevel> = levels.to_vec();
    let futs: Vec<_> = keys.iter().map(|&key| {
        let client = client.clone();
        let url = format!("https://{bucket}.s3.amazonaws.com/{key}");
        let key_owned = key.to_string();
        let lvls = levels_owned.clone();
        async move {
            match download_and_parse_one(&client, &url, satellite, &lvls).await {
                Ok(parsed) => Ok((key_owned, parsed)),
                Err(e) => {
                    // Per-file detail stays at debug: with 20 files in flight,
                    // warning on each turns a single schema change into a wall
                    // of identical lines. The aggregate is reported instead.
                    log::debug!("Failed to fetch GLM file {key_owned}: {}", e.message());
                    let labelled = format!("{key_owned}: {}", e.message());
                    Err(match e {
                        FileError::Parse(_) => FileError::Parse(labelled),
                        FileError::Transport(_) => FileError::Transport(labelled),
                    })
                }
            }
        }
    }).collect();

    let results: Vec<Result<(String, GranuleParse), FileError>> = futures::stream::iter(futs)
        .buffer_unordered(20)
        .collect()
        .await;

    BatchOutcome::from_results(results)
}

impl BatchOutcome {
    /// Split per-file results into what parsed and what did not.
    ///
    /// Separated from the async download so the partition is reachable from a
    /// test: this is the step that used to discard every error into a log line.
    fn from_results(results: Vec<Result<(String, GranuleParse), FileError>>) -> Self {
        let mut outcome = BatchOutcome {
            entries: Vec::new(),
            parse_errors: Vec::new(),
            transport_errors: Vec::new(),
            level_failures: Vec::new(),
        };
        for result in results {
            match result {
                Ok((key, parsed)) => {
                    // One batch is one satellite, so within this loop the level
                    // is what discriminates. The satellite is compared anyway:
                    // it is the identity `LevelFailure` is keyed on, and the
                    // accumulator does merge across satellites.
                    for failure in parsed.level_failures {
                        if !outcome.level_failures.iter().any(|f: &LevelFailure| {
                            f.satellite == failure.satellite && f.level == failure.level
                        }) {
                            outcome.level_failures.push(failure);
                        }
                    }
                    outcome.entries.push((key, parsed.records));
                }
                Err(FileError::Parse(e)) => outcome.parse_errors.push(e),
                Err(FileError::Transport(e)) => outcome.transport_errors.push(e),
            }
        }
        outcome
    }
}

/// Why one file did not contribute.
///
/// The distinction is the whole point: a file that arrives and will not parse
/// indicts the product, a file that never arrives indicts the network, and
/// telling a user their GLM product changed because S3 returned 503 SlowDown to
/// a 20-way concurrent GET burst is a false alarm of exactly the kind this
/// branch exists to remove.
///
/// Classification is by *stage*, not by content, which has one known blind
/// spot: a captive portal or proxy that answers 200 with an HTML error page
/// produces bytes that fail to parse, and those are reported as `Parse` —
/// "product change?" — when the real cause is the network. Distinguishing that
/// would mean sniffing the body for the NetCDF magic number. Left alone
/// deliberately: it misreports a transient local-network condition, whereas
/// classifying by content risks misreading a genuine product change as a
/// network fault, which is the failure this branch is built to prevent.
#[derive(Debug)]
enum FileError {
    /// Connection failure, non-2xx status, or a truncated body.
    Transport(String),
    /// The bytes arrived but were not the product we expect.
    Parse(String),
}

impl FileError {
    fn message(&self) -> &str {
        match self {
            FileError::Transport(e) | FileError::Parse(e) => e,
        }
    }
}

/// Reduce a poll's per-file errors to the report the UI consumes.
///
/// `in_window` is every file the listing placed in the window, not just the
/// ones downloaded this tick — see [`FetchFailures::in_window`]. `None` means
/// nothing failed. Pure, so the classification that decides whether the user
/// sees "everything failed" is testable without a network round trip.
fn summarize_failures(in_window: usize, errors: Vec<String>) -> Option<FetchFailures> {
    let sample_error = errors.first()?.clone();
    Some(FetchFailures {
        in_window,
        failed: errors.len(),
        sample_error,
    })
}

/// Fetch the raw bytes of one object. Every failure here is a transport
/// failure, by construction — the function has no idea what the bytes mean.
async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    client.get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?
        .error_for_status()
        .map_err(|e| format!("HTTP status error: {e}"))?
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("Failed to read body: {e}"))
}

/// Download a single GLM NetCDF file and parse data from it.
///
/// The two stages are split so the transport/parse classification is carried by
/// the type system at exactly two call sites, rather than repeated at each
/// error site where one could drift. Mislabelling a 503 as a parse failure
/// tells the user their product changed when S3 merely throttled them.
async fn download_and_parse_one(
    client: &reqwest::Client,
    url: &str,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, FileError> {
    let bytes = download_bytes(client, url).await.map_err(FileError::Transport)?;
    parse_downloaded_file(&bytes, satellite, levels)
}

/// Parse bytes that already arrived. Any failure here is a parse failure, and
/// the split from the download makes that testable without a network round trip.
fn parse_downloaded_file(
    bytes: &[u8],
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, FileError> {
    parse_glm_netcdf(bytes, satellite, levels).map_err(FileError::Parse)
}

/// Variable name sets for each GLM data level.
struct LevelVars {
    lat: &'static str,
    lon: &'static str,
    energy: &'static str,
    /// Name of the area variable, or `None` if the level has no area in the
    /// L2 LCFA product. Only groups and flashes report area coverage.
    area: Option<&'static str>,
    time_offset: &'static str,
    level: GlmDataLevel,
}

const FLASH_VARS: LevelVars = LevelVars {
    lat: "flash_lat",
    lon: "flash_lon",
    energy: "flash_energy",
    area: Some("flash_area"),
    time_offset: "flash_time_offset_of_first_event",
    level: GlmDataLevel::Flash,
};

const GROUP_VARS: LevelVars = LevelVars {
    lat: "group_lat",
    lon: "group_lon",
    energy: "group_energy",
    area: Some("group_area"),
    time_offset: "group_time_offset",
    level: GlmDataLevel::Group,
};

// The L2 LCFA product has no `event_area` variable: an event is a single
// sensor pixel detection, and only groups and flashes carry area coverage
// (confirmed against a live `noaa-goes19` GLM-L2-LCFA granule). Asking for a
// non-existent variable used to yield an all-zero column, so every event
// popup reported "0.0 km²".
const EVENT_VARS: LevelVars = LevelVars {
    lat: "event_lat",
    lon: "event_lon",
    energy: "event_energy",
    area: None,
    time_offset: "event_time_offset",
    level: GlmDataLevel::Event,
};

/// Parse GLM data from NetCDF4 bytes in memory.
pub(crate) fn parse_glm_netcdf(
    data: &[u8],
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, String> {
    let file = netcdf::open_mem(None, data)
        .map_err(|e| format!("Failed to open NetCDF: {e}"))?;

    // Read the time origin from global attribute. Every `*_time_offset`
    // variable also names its own epoch in its `units` attribute, and in every
    // granule inspected the two agree exactly (`time_coverage_start` =
    // "2026-07-24T12:00:00.0Z", `event_time_offset:units` = "seconds since
    // 2026-07-24 12:00:00.000"). The per-variable epoch wins where present —
    // see `parse_level_records` — and this is the fallback.
    let time_origin = file.attribute("time_coverage_start")
        .and_then(|a| a.value().ok())
        .and_then(|v| match v {
            netcdf::AttributeValue::Str(s) => Some(s),
            _ => None,
        })
        .and_then(|s| cf::parse_cf_epoch(&s))
        .ok_or_else(|| "Missing or invalid time_coverage_start attribute".to_string())?;

    let mut all_records = Vec::new();
    let mut failures: Vec<LevelFailure> = Vec::new();

    for level in levels {
        let vars = match level {
            GlmDataLevel::Flash => &FLASH_VARS,
            GlmDataLevel::Group => &GROUP_VARS,
            GlmDataLevel::Event => &EVENT_VARS,
        };
        // One level failing must not take the others with it. The three levels
        // are independent variable sets and the user selects them
        // independently, so a schema change confined to `flash_*` should not
        // black out the default-on group layer as well.
        match parse_level_records(&file, vars, &time_origin, satellite) {
            Ok(records) => all_records.extend(records),
            Err(e) => {
                warn_once(
                    level_parse_key(satellite, vars.lat),
                    &format!(
                        "GLM {}: {} level could not be parsed: {e}",
                        satellite.display_name(),
                        vars.level.display_name(),
                    ),
                );
                failures.push(LevelFailure {
                    satellite,
                    level: *level,
                    sample_error: e,
                });
            }
        }
    }

    // Every requested level failing means the granule itself is unusable, so it
    // is reported as a failed *file* — which is what it is, and what puts it in
    // the "N/M files failed to parse" count the panel already renders.
    //
    // The first underlying error is propagated verbatim rather than summarised.
    // It becomes `FetchFailures::sample_error`, which reaches the operator
    // through `log::warn!` in the handler (the panel itself renders only
    // counts), so "GLM file has no 'flash_lat' variable (product schema
    // change?)" is worth far more there than a tally of how many levels failed.
    if !failures.is_empty() && failures.len() == levels.len() {
        return Err(failures.swap_remove(0).sample_error);
    }

    // A *partial* failure keeps the healthy levels and reports the broken one
    // separately. Neither parent of this code did both: returning `Err` here
    // would discard perfectly good group records over a flash-only schema
    // change, and returning a bare `Ok` — as the first cut of this did — leaves
    // the Flashes layer empty with nothing on screen to explain it, because
    // `Ok` means `parse_failures: None` means the panel says everything is
    // fine. `LevelFailure` is the third channel that makes both possible.
    Ok(GranuleParse { records: all_records, level_failures: failures })
}

/// One granule's worth of parsed records, plus any level that did not parse.
#[derive(Debug)]
pub(crate) struct GranuleParse {
    pub records: Vec<GlmFlash>,
    /// Empty in the overwhelmingly common case. Non-empty means some levels
    /// parsed and others did not — see [`LevelFailure`].
    pub level_failures: Vec<LevelFailure>,
}

/// Unit spellings accepted for `*_area`, mapped to the multiplier that turns
/// them into km² — the unit [`GlmFlash::area`] documents and the popup labels.
///
/// The L2 LCFA product declares `flash_area:units = "m2"`, so the values are
/// **square metres** and must be divided by a million. Reading the packed
/// count straight out of the file and labelling it "km²" was wrong twice over:
/// a flash of raw count 1826 was shown as "1826.0 km²" when it is really
/// 1826 × 152601.9 m² = 278.7 km².
const AREA_UNITS: &[(&str, f64)] = &[
    ("m2", 1e-6),
    ("m^2", 1e-6),
    ("m**2", 1e-6),
    ("meter2", 1e-6),
    ("meters2", 1e-6),
    ("km2", 1.0),
    ("km^2", 1.0),
    ("km**2", 1.0),
];

/// Unit spellings accepted for `*_energy`, mapped to the multiplier that turns
/// them into joules.
///
/// GLM declares `units = "J"` with a scale factor around 1e-16, so real values
/// land between roughly 1e-15 and 1e-12 J. Deliberately no SI prefixes here:
/// case-folding would make "mJ" and "MJ" collide, and a silent factor of 1e9
/// is exactly the failure this module exists to prevent.
const ENERGY_UNITS: &[(&str, f64)] = &[("j", 1.0), ("joule", 1.0), ("joules", 1.0)];

/// Parse records for one GLM hierarchy level from the NetCDF file.
///
/// Every variable goes through [`cf::read_unpacked`], which applies the CF
/// packing conventions the `netcdf` crate does not. See that module for the
/// rules; the short version is that most GLM variables are `_Unsigned` packed
/// shorts and reading them raw yields meaningless numbers.
fn parse_level_records(
    file: &netcdf::File,
    vars: &LevelVars,
    time_origin: &chrono::NaiveDateTime,
    satellite: GlmSatellite,
) -> Result<Vec<GlmFlash>, String> {
    // Required *columns*. Absence here is a product schema change, and it is
    // loud: `read_required_unpacked` warns once per variable and fails the
    // level. That is a different condition from an absent *value*, which
    // arrives quietly as a `None` inside `UnpackedVar::values` — see the note
    // on [`read_required_unpacked`].
    let lats = read_required_unpacked(file, vars.lat)?;
    let lons = read_required_unpacked(file, vars.lon)?;
    let energies = read_required_unpacked(file, vars.energy)?;
    let times = read_required_unpacked(file, vars.time_offset)?;

    // Every variable at a level shares one dimension (`number_of_flashes`,
    // `number_of_groups`, `number_of_events`), so a short column is never
    // legitimate — it means a corrupt or restructured file. Reject it instead
    // of padding the tail with zeros.
    let count = lats.values.len();
    for (name, len) in [
        (vars.lon, lons.values.len()),
        (vars.energy, energies.values.len()),
        (vars.time_offset, times.values.len()),
    ] {
        if len != count {
            return Err(format!(
                "GLM variable length mismatch: '{name}' has {len} values but '{}' has {count}",
                vars.lat,
            ));
        }
    }

    // Area is the one genuinely optional column, and the criterion is the
    // product's own schema, not how load-bearing the field feels: events have no
    // area variable at all, so `vars.area` is `None` there and we never look.
    // Every level always has lat/lon/energy/time, so their absence is drift and
    // is refused above — including energy, whose loss would otherwise be
    // *silently* absorbed by the rasterizer as a uniform minimum bolt size.
    // When a level that should have an area is missing one, degrade to no area
    // instead of failing the file, but say so.
    let areas = match vars.area {
        Some(name) => match read_optional_unpacked(file, name)? {
            Some(v) if v.values.len() == count => Some(v),
            Some(v) => {
                log::warn!(
                    "GLM {}: '{name}' has {} values but '{}' has {count}; omitting area",
                    satellite.display_name(),
                    v.values.len(),
                    vars.lat,
                );
                None
            }
            None => None,
        },
        None => None,
    };

    // The time axis names its own epoch and unit. Prefer them over the
    // granule-level `time_coverage_start`: in every granule inspected the two
    // agree, and reading the variable's own metadata means they cannot
    // silently drift apart.
    let time_units = match times.units.as_deref() {
        Some(u) => cf::parse_time_units(u).ok_or_else(|| {
            format!(
                "GLM {} declares time units {u:?} that rustdar cannot interpret; \
                 refusing to guess an epoch",
                vars.time_offset
            )
        })?,
        None => cf::TimeUnits { seconds_per_unit: 1.0, epoch: *time_origin },
    };
    if time_units.epoch != *time_origin {
        log::warn!(
            "GLM {}: {} units epoch {} disagrees with time_coverage_start {}; using the \
             variable's own epoch",
            satellite.display_name(),
            vars.time_offset,
            time_units.epoch,
            time_origin,
        );
    }

    // Unit resolution is scoped to the field it describes. `None` means "this
    // product declares a unit we cannot convert", which makes the *descriptive*
    // field unknown — it must not take position and time down with it, and it
    // must not take the other two hierarchy levels down either.
    let energy_to_j = unit_multiplier(satellite, vars.energy, Some(&energies), ENERGY_UNITS, "J");
    let area_to_km2 = unit_multiplier(
        satellite,
        vars.area.unwrap_or("area"),
        areas.as_ref(),
        AREA_UNITS,
        "km2",
    );

    let mut records = Vec::with_capacity(count);
    let mut missing = 0usize;
    let mut off_globe = 0usize;

    for i in 0..count {
        // A `_FillValue` in any of the three fields that place a strike in
        // space and time makes the detection unusable, so the record is
        // dropped rather than published with a fabricated number. This
        // matches how the rest of rustdar treats rows it cannot parse (see
        // `spc::discussion`, `metar::fetch`).
        let (Some(lat), Some(lon), Some(offset)) =
            (lats.values[i], lons.values[i], times.values[i])
        else {
            missing += 1;
            continue;
        };

        let lon = normalize_longitude(lon);

        // Backstop against a coordinate that unpacked to nonsense. Note this
        // only really guards latitude now: longitude has a legitimate
        // wrap-around above, so a mis-unpacked longitude can land back inside
        // the valid interval. Latitude has no such convention, and it is what
        // caught the original bug — the unfixed code produced -94°.
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            off_globe += 1;
            continue;
        }

        // Energy and area are descriptive, not locating: a strike whose
        // brightness the product did not report is still a real strike at a
        // real place and time, so the record survives and the field reads
        // `None`.
        //
        // Never zero, and never a skipped record. `flash_energy` and
        // `event_energy` carry `_FillValue = -1s`, so a *value* can be absent
        // in a column that is present, and the three obvious handlings are all
        // wrong: substituting a default reintroduces `0f32.log10()` = -inf and
        // draws unknown as the smallest possible bolt (the tempting one, and
        // the exact bug `Option` was introduced to remove); failing the file
        // passes a granule-wide verdict on one record, for a condition the
        // product defines per record; skipping the record deletes a real,
        // located strike over a descriptive field, which is the same mistake as
        // rejecting GOES-West longitudes.
        //
        // `Option<f32>` is the fourth option, and the column stays *required*
        // above — a renamed or absent `*_energy` still fails loudly. Required
        // column, optional value.
        let energy = column_value(Some(&energies), i)
            .zip(energy_to_j)
            .map(|(v, to_j)| (v * to_j) as f32);
        let area = column_value(areas.as_ref(), i)
            .zip(area_to_km2)
            .map(|(v, to_km2)| (v * to_km2) as f32);

        // Microseconds, not milliseconds. GLM's time `scale_factor` is
        // 3.814756e-4 s, so consecutive representable instants are 0.38 ms
        // apart: truncating to whole milliseconds collapses roughly three in
        // five adjacent pairs onto the same timestamp, with a worst-case error
        // of a full millisecond against a 0.38 ms quantum. Microseconds leave
        // 381 µs of separation, which is sufficient rather than merely better.
        //
        // It also interacts with the unit multiplier above: on a `milliseconds
        // since` axis the sub-millisecond offsets — which is where the granule
        // boundary sits — would all truncate to zero.
        let micros = (offset * time_units.seconds_per_unit * 1e6) as i64;
        let time = time_units.epoch + TimeDelta::microseconds(micros);

        records.push(GlmFlash {
            lat,
            lon,
            energy,
            area,
            time,
            satellite,
            level: vars.level,
        });
    }

    if missing > 0 || off_globe > 0 {
        log::warn!(
            "GLM {} {}: dropped {missing} record(s) with fill values and {off_globe} with \
             out-of-range coordinates (of {count})",
            satellite.display_name(),
            vars.level.display_name(),
        );
    }

    Ok(records)
}

/// Report a variable the product was expected to have but does not, once per
/// variable per process.
///
/// A variable vanishing from the product is a permanent schema change, not a
/// transient condition, so repeating the message on every 20-second poll would
/// bury it in exactly the way that let the original bug survive a year.
///
/// Not satellite-qualified, unlike the unit warnings: the variable *set* is a
/// property of the product schema, and naming the bird would imply a
/// per-satellite condition that this is not.
fn warn_missing_variable_once(name: &'static str) {
    warn_once(
        missing_variable_key(name),
        &format!(
            "GLM: variable '{name}' is absent from the L2 LCFA file — the product \
             schema has changed and this field can no longer be read"
        ),
    );
}

/// Read a variable the level cannot do without, with CF unpacking applied.
///
/// This is the *column presence* half of a two-level distinction that both
/// halves of this module needed and neither could express alone:
///
/// * **The variable is absent from the file.** A schema change — permanent,
///   affects every record, and nothing downstream can compensate. Warned once
///   and failed here. The previous `Ok(Vec::new())` fallback is the mechanism
///   that produced "Area: 0.0 km²" for a year: a variable that does not exist
///   read back as an empty column and callers padded it with zeros. The same
///   silence would turn a renamed `flash_energy` into every bolt drawing at
///   minimum size, or a renamed `flash_lat` into "no lightning anywhere".
///
/// * **An individual value is `_FillValue`** (or outside `valid_range`). A
///   per-record condition the product deliberately defines — `flash_area`,
///   `group_area`, `flash_energy` and `event_energy` all carry
///   `_FillValue = -1s`. Quiet, and carried as a `None` inside
///   [`cf::UnpackedVar::values`] for the caller to decide per field.
///
/// Conflating the two costs something either way round: treating a fill value
/// as a schema change fails whole granules over one bad record, and treating
/// an absent column as "all values missing" is the silence this branch exists
/// to remove.
fn read_required_unpacked(
    file: &netcdf::File,
    name: &'static str,
) -> Result<cf::UnpackedVar, String> {
    match cf::read_unpacked(file, name)? {
        Some(var) => Ok(var),
        None => {
            warn_missing_variable_once(name);
            Err(format!("GLM file has no '{name}' variable (product schema change?)"))
        }
    }
}

/// Read a variable a level may legitimately lack, with CF unpacking applied.
///
/// Returns `Ok(None)` when absent, but still reports it: the *declared*
/// optionality lives in [`LevelVars::area`], so reaching here with a name in
/// hand means we asked for something the product used to have.
fn read_optional_unpacked(
    file: &netcdf::File,
    name: &'static str,
) -> Result<Option<cf::UnpackedVar>, String> {
    let var = cf::read_unpacked(file, name)?;
    if var.is_none() {
        warn_missing_variable_once(name);
    }
    Ok(var)
}

/// Fold a GLM longitude into the conventional [-180, 180] interval.
///
/// GLM stores longitude in an *unwrapped* frame anchored on the spacecraft, so
/// the valid interval depends on `add_offset`, which tracks the satellite
/// sub-point (verified on live granules):
///
/// | slot                  | `event_lon:add_offset` | unpackable range   |
/// |-----------------------|------------------------|--------------------|
/// | GOES-East (G16, G19)  | -141.56                | -141.56 …   -8.44  |
/// | GOES-West (G18)       | -203.56                | -203.56 …  -70.44  |
///
/// GOES-West therefore runs past the antimeridian: a real detection at
/// 172.72°E is stored as -187.28. Rejecting those as out-of-range deleted 60
/// of 3228 events in the granule this was measured on, and did so *selectively* —
/// `group_lon`/`flash_lon` are genuine floats already wrapped, reaching
/// +172.90 in that same file, so the groups and flashes survived while their
/// own constituent events were thrown away.
///
/// One wrap is enough for any offset the product uses; anything further out is
/// left alone so the range check downstream still sees it.
pub(super) fn normalize_longitude(lon: f64) -> f64 {
    if (-180.0..=180.0).contains(&lon) || lon.abs() > 540.0 {
        lon
    } else if lon < 0.0 {
        lon + 360.0
    } else {
        lon - 360.0
    }
}

/// Value of a column at `i`, or `None` if the variable is absent from the
/// product, the column is short, or the element is `_FillValue`.
///
/// Collapsing those three into one `None` is deliberate *here* and only here:
/// by this point the column-level conditions have already been reported by
/// [`read_required_unpacked`]/[`read_optional_unpacked`], and what the caller
/// needs per record is simply whether there is a number.
fn column_value(column: Option<&cf::UnpackedVar>, i: usize) -> Option<f64> {
    column?.values.get(i).copied().flatten()
}

/// Resolve the multiplier converting a variable's declared `units` into the
/// unit rustdar stores and displays, or `None` if that cannot be done.
///
/// The contract is deliberately symmetric: **a value is reported only when the
/// file says what unit it is in and we can convert that unit.** Both an absent
/// `units` attribute and an unrecognized one yield `None`, i.e. "unknown".
///
/// The earlier asymmetry was worse than either branch alone. An absent
/// attribute assumed the value was already canonical, so a dropped `units` on
/// `flash_area` — which the product ships as `m2` — would have silently
/// reported areas a million times too large; a merely *misspelled* one failed
/// the whole granule. A dropped attribute and a renamed one are the same kind
/// of upstream change and should not diverge that sharply.
///
/// Failure is scoped to the field. `*_area` and `*_energy` are descriptive, so
/// losing them costs a popup row; it must not cost the strike its position and
/// time, nor take the other two hierarchy levels with it.
///
/// Diagnostics name the satellite. Both birds can be enabled at once and they
/// are separate product streams that can diverge, so an operator needs to know
/// which one changed — and keying on it means a problem appearing on one slot
/// does not suppress the warning for the other.
fn unit_multiplier(
    satellite: GlmSatellite,
    name: &str,
    column: Option<&cf::UnpackedVar>,
    table: &[(&str, f64)],
    canonical: &str,
) -> Option<f64> {
    // No column at all is a product property, not an anomaly: the L2 LCFA
    // product simply has no `event_area`. Nothing to report and nothing to
    // warn about — a genuinely *missing* variable was already reported by
    // `read_optional_unpacked`.
    let column = column?;
    let sat = satellite.display_name();

    let Some(units) = column.units.as_deref() else {
        warn_once(units_key(satellite, name, "absent"), &format!(
            "GLM {sat}: {name} declares no units attribute; reporting the field as \
             unknown rather than assuming {canonical}"
        ));
        return None;
    };

    let key = units.trim().to_ascii_lowercase();
    let found = table
        .iter()
        .find(|(spelling, _)| *spelling == key)
        .map(|(_, multiplier)| *multiplier);

    if found.is_none() {
        warn_once(units_key(satellite, name, &key), &format!(
            "GLM {sat}: {name} declares units {units:?}, which rustdar cannot convert \
             to {canonical}; reporting the field as unknown. This is an upstream \
             product change — the conversion table in `glm::fetch` needs the new \
             spelling."
        ));
    }
    found
}

/// Dedup key for "a hierarchy level would not parse".
///
/// Keyed on the satellite as well as the level: the two birds are separate
/// product streams that can diverge, so a change hitting one must not suppress
/// the report for the other.
pub(super) fn level_parse_key(satellite: GlmSatellite, lat_var: &str) -> String {
    format!("{}:level-parse:{lat_var}", satellite.bucket())
}

/// Dedup key for "a variable the product used to have is gone".
///
/// Deliberately *not* satellite-qualified: the variable set is a property of
/// the product schema, so naming the bird would imply a per-satellite condition
/// this is not, and would report the same schema fact twice.
pub(super) fn missing_variable_key(name: &str) -> String {
    format!("variable-absent:{name}")
}

/// Dedup key for "this variable declares a unit we cannot convert".
///
/// Keyed on the satellite *and* the offending spelling, so a second, different
/// bad spelling still reports, and so does the same one on the other bird.
pub(super) fn units_key(satellite: GlmSatellite, name: &str, spelling: &str) -> String {
    format!("{}:{name}:units:{spelling}", satellite.bucket())
}

/// Log a warning the first time a given key is seen, then stay quiet.
///
/// GLM polls every 20 seconds across up to two satellites, so an upstream
/// schema change would otherwise produce an unbounded stream of identical
/// lines and bury itself. The conditions this guards are all permanent once
/// they appear.
///
/// The single registry for the module: absent-variable reports
/// ([`warn_missing_variable_once`]), unit problems and level-parse failures
/// all key into it, so one condition can never crowd out another and each is
/// reported exactly once.
pub(crate) fn warn_once(key: String, message: &str) {
    if claim_warning(key) {
        log::warn!("{message}");
    }
}

/// Record `key` as seen and report whether this is the first time.
///
/// Split out from [`warn_once`] so the deduplication can be tested without a
/// log capture — in particular that distinct keys really are distinct, which
/// is what stops a problem on one satellite from suppressing the warning for
/// the other.
pub(crate) fn claim_warning(key: String) -> bool {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = seen.lock().unwrap_or_else(|e| e.into_inner());
    guard.insert(key)
}

/// Minimal URL encoding for continuation tokens.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            _ => {
                for b in c.to_string().as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only groups and flashes carry area coverage in the L2 LCFA product.
    #[test]
    fn only_group_and_flash_levels_declare_an_area_variable() {
        assert_eq!(FLASH_VARS.area, Some("flash_area"));
        assert_eq!(GROUP_VARS.area, Some("group_area"));
        assert_eq!(
            EVENT_VARS.area, None,
            "the GLM L2 LCFA product has no `event_area` variable"
        );
    }

    /// Deletes the scratch file even if the test body panics.
    struct TempNc(std::path::PathBuf);

    impl Drop for TempNc {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Every flash-level variable, so tests can omit "all of them".
    const FLASH_LEVEL_VARS: [&str; 5] = [
        "flash_lat",
        "flash_lon",
        "flash_energy",
        "flash_area",
        "flash_time_offset_of_first_event",
    ];

    /// The error a missing variable must produce, verbatim.
    fn absent_variable_error(name: &str) -> String {
        format!("GLM file has no '{name}' variable (product schema change?)")
    }

    /// The error a short column must produce, verbatim.
    fn length_mismatch_error(name: &str, len: usize, reference: &str, count: usize) -> String {
        format!("GLM variable length mismatch: '{name}' has {len} values but '{reference}' has {count}")
    }

    #[derive(Default)]
    struct Fixture<'a> {
        /// Variables to leave out entirely, simulating a schema change.
        omit: &'a [&'a str],
        /// Variable to write against a deliberately shorter dimension,
        /// simulating a corrupt or restructured file.
        short: Option<&'a str>,
    }

    /// Build a minimal in-memory GLM-shaped NetCDF4 file: flashes carry an
    /// area variable, events deliberately do not (mirroring the real product).
    ///
    /// NOTE: this fixture writes plain unpacked `f32` in already-canonical
    /// units, deliberately. Its subject is *column presence* — which variables
    /// a level requires, and what happens when one is missing or short — not
    /// the CF packing. The real `_Unsigned`/`scale_factor`/`add_offset` shapes
    /// are exercised against packed shorts in `glm::tests`, so duplicating them
    /// here would only couple these tests to constants they do not care about.
    /// The `units` attributes are still declared, because a value whose unit is
    /// undeclared is reported as unknown.
    fn synthetic_glm_file(spec: Fixture<'_>) -> Vec<u8> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let guard = TempNc(std::env::temp_dir().join(format!(
            "rustdar-glm-test-{}-{unique}.nc",
            std::process::id(),
        )));
        let path = &guard.0;
        let _ = std::fs::remove_file(path);

        {
            let mut file = netcdf::create(path).expect("create netcdf");
            file.add_attribute("time_coverage_start", "2026-07-24T12:00:00.0Z")
                .expect("add time_coverage_start");

            file.add_dimension("number_of_flashes", 2).expect("flash dim");
            file.add_dimension("number_of_events", 2).expect("event dim");
            file.add_dimension("truncated", 1).expect("short dim");

            let mut put = |name: &str, dim: &str, values: &[f32]| {
                if spec.omit.contains(&name) {
                    return;
                }
                let (dim, values) = if spec.short == Some(name) {
                    ("truncated", &values[..1])
                } else {
                    (dim, values)
                };
                let mut var = file
                    .add_variable::<f32>(name, &[dim])
                    .unwrap_or_else(|e| panic!("add {name}: {e}"));
                // Declare units on the two fields that are unit-converted.
                // These fixtures are about *column* presence, so the values are
                // written already in rustdar's canonical units rather than the
                // product's packed `m2` — but they must still say so, because a
                // value whose unit is undeclared is reported as unknown. The
                // real packing is exercised in `glm::tests`.
                let units = match name {
                    n if n.ends_with("_area") => Some("km2"),
                    n if n.ends_with("_energy") => Some("J"),
                    _ => None,
                };
                if let Some(u) = units {
                    var.put_attribute("units", u).expect("units");
                }
                var.put_values(values, ..)
                    .unwrap_or_else(|e| panic!("put {name}: {e}"));
            };

            put("flash_lat", "number_of_flashes", &[35.0, 36.0]);
            put("flash_lon", "number_of_flashes", &[-97.0, -98.0]);
            put("flash_energy", "number_of_flashes", &[1.0e-14, 2.0e-14]);
            put("flash_area", "number_of_flashes", &[128.0, 256.0]);
            put(
                "flash_time_offset_of_first_event",
                "number_of_flashes",
                &[1.0, 2.0],
            );

            put("event_lat", "number_of_events", &[35.5, 36.5]);
            put("event_lon", "number_of_events", &[-97.5, -98.5]);
            put("event_energy", "number_of_events", &[3.0e-15, 4.0e-15]);
            put("event_time_offset", "number_of_events", &[3.0, 4.0]);
            // Note: no `event_area` — that is the point of the fixture.
        }

        std::fs::read(path).expect("read back netcdf")
    }

    /// Records only, for tests that do not care about level failures.
    fn parse_records(
        bytes: &[u8],
        satellite: GlmSatellite,
        levels: &[GlmDataLevel],
    ) -> Result<Vec<GlmFlash>, String> {
        parse_glm_netcdf(bytes, satellite, levels).map(|p| p.records)
    }

    fn parse_flashes(bytes: &[u8]) -> Result<Vec<GlmFlash>, String> {
        parse_glm_netcdf(bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Flash])
            .map(|p| p.records)
    }

    #[test]
    fn flash_level_reports_area_and_event_level_reports_none() {
        let bytes = synthetic_glm_file(Fixture::default());

        let flashes = parse_flashes(&bytes).expect("parse flash level");
        assert_eq!(flashes.len(), 2);
        // Pin that the *right column* was read, in order, without pinning the
        // absolute scale, which is the fixture's own and not the product's. The
        // fixture
        // writes area = [128, 256], a ratio of exactly 2; lat is [35, 36] and
        // lon is [-97, -98], so a ratio test excludes both, and a pure scaling
        // preserves it (`flash_area` has add_offset = 0 in the real product).
        // The `> 1.0` floor then excludes the energy column, which is ~1e-14.
        //
        // Note this is a supporting check: the authoritative protection against
        // sourcing area from the wrong variable is
        // `only_group_and_flash_levels_declare_an_area_variable`.
        let areas: Vec<f32> = flashes.iter().map(|f| f.area.expect("flash area")).collect();
        assert!(
            (areas[1] / areas[0] - 2.0).abs() < 1e-3,
            "area column should track the file's values, got {areas:?}"
        );
        assert!(areas.iter().all(|a| *a > 1.0), "got {areas:?}");

        let events = parse_records(
            &bytes,
            GlmSatellite::GoesEast,
            &[GlmDataLevel::Event],
        )
        .expect("parse event level");
        assert_eq!(events.len(), 2);
        // Previously this silently produced Some(0.0) via `unwrap_or(&0.0)`,
        // which the popup rendered as "Area: 0.0 km²".
        assert!(
            events.iter().all(|e| e.area.is_none()),
            "events must not report a fabricated area"
        );
        // The rest of the event record must still parse.
        assert!((events[0].lat - 35.5).abs() < 1e-4);
        assert!((events[0].lon - (-97.5)).abs() < 1e-4);
    }

    /// A required variable disappearing from the product must fail the parse,
    /// not quietly yield zeros or an empty result set.
    ///
    /// Asserts the error *verbatim*. Containment is not enough: with the
    /// required-variable gate removed, the length check downstream also errors
    /// and its message interpolates both the offending name and `vars.lat`, so a
    /// `contains(missing)` assertion passes on entirely the wrong error and the
    /// two gates shadow each other.
    #[test]
    fn missing_required_variable_is_an_error_not_a_silent_default() {
        for missing in [
            "flash_lat",
            "flash_lon",
            "flash_energy",
            "flash_time_offset_of_first_event",
        ] {
            let bytes = synthetic_glm_file(Fixture { omit: &[missing], ..Default::default() });
            let err = parse_flashes(&bytes).expect_err(
                "a missing required variable must surface, not read back as an empty column",
            );
            assert_eq!(err, absent_variable_error(missing));
        }
    }

    /// The case only the required-variable gate can catch.
    ///
    /// When an entire level vanishes, every column is equally absent, so no
    /// length mismatch exists for the downstream check to trip on. Reverting
    /// `read_required_f32` to `Ok(Vec::new())` makes this parse cleanly into
    /// zero records — a blank map reported as success.
    #[test]
    fn a_whole_level_vanishing_is_an_error_not_zero_records() {
        let bytes = synthetic_glm_file(Fixture { omit: &FLASH_LEVEL_VARS, ..Default::default() });
        let err = parse_flashes(&bytes)
            .expect_err("an entirely absent level must not read as 'no lightning'");
        assert_eq!(err, absent_variable_error("flash_lat"));
    }

    /// Same case for the f64 reader, which has its own absence path.
    ///
    /// Only the time variable is omitted; the other columns stay present and
    /// equally sized, so no length mismatch exists to mask the gate. The
    /// verbatim assertion is what proves which gate fired.
    #[test]
    fn a_missing_time_variable_alone_is_an_error() {
        let bytes = synthetic_glm_file(Fixture {
            omit: &["flash_time_offset_of_first_event"],
            ..Default::default()
        });
        let err = parse_flashes(&bytes).expect_err("absent time variable must surface");
        assert_eq!(err, absent_variable_error("flash_time_offset_of_first_event"));
    }

    /// Energy in particular used to fall back to 0.0, which the rasterizer turns
    /// into `0f32.log10()` = -inf and draws as a minimum-size bolt — a total
    /// data loss that looked like a normal render.
    #[test]
    fn missing_energy_does_not_default_to_zero() {
        let bytes = synthetic_glm_file(Fixture {
            omit: &["flash_energy"],
            ..Default::default()
        });
        let err = parse_flashes(&bytes).expect_err("absent energy must surface");
        assert_eq!(err, absent_variable_error("flash_energy"));
    }

    /// The length check is a separate gate from the required-variable gate and
    /// needs its own coverage: a variable that is *present but short* is
    /// corruption, and indexing past it would panic.
    #[test]
    fn a_short_required_column_is_rejected() {
        for short in ["flash_lon", "flash_energy", "flash_time_offset_of_first_event"] {
            let bytes = synthetic_glm_file(Fixture { short: Some(short), ..Default::default() });
            let err = parse_flashes(&bytes)
                .expect_err("a short column must be rejected, not indexed past");
            assert_eq!(err, length_mismatch_error(short, 1, "flash_lat", 2));
        }
    }

    /// Every per-file error must survive the batch partition, sorted into the
    /// right bucket. This is the step that previously discarded them all into
    /// `log::warn!` + `None`, which is why a total parse failure reached the
    /// user as "Updated 0s ago".
    #[test]
    fn batch_partition_keeps_every_error_and_separates_the_kinds() {
        let outcome = BatchOutcome::from_results(vec![
            Ok(("a.nc".into(), GranuleParse { records: Vec::new(), level_failures: Vec::new() })),
            Err(FileError::Parse("b.nc: bad variable".into())),
            Err(FileError::Transport("c.nc: HTTP status error: 503".into())),
            Err(FileError::Parse("d.nc: bad variable".into())),
        ]);
        assert_eq!(outcome.entries.len(), 1);
        assert_eq!(
            outcome.parse_errors,
            vec!["b.nc: bad variable", "d.nc: bad variable"]
        );
        assert_eq!(
            outcome.transport_errors,
            vec!["c.nc: HTTP status error: 503"],
            "a 503 is a network problem and must never be counted as a parse failure"
        );
    }

    /// A schema change hits every granule in the window identically, so the
    /// same broken level in twenty files is one report — but two *different*
    /// broken levels are two, and collapsing them would hide a layer.
    #[test]
    fn batch_partition_dedups_level_failures_per_level_not_per_file() {
        let both_broken = || {
            vec![
                level_failure(GlmSatellite::GoesEast, GlmDataLevel::Flash),
                level_failure(GlmSatellite::GoesEast, GlmDataLevel::Group),
            ]
        };
        let outcome = BatchOutcome::from_results(vec![
            Ok(("a.nc".into(), GranuleParse { records: Vec::new(), level_failures: both_broken() })),
            Ok(("b.nc".into(), GranuleParse { records: Vec::new(), level_failures: both_broken() })),
            Ok(("c.nc".into(), GranuleParse { records: Vec::new(), level_failures: both_broken() })),
        ]);

        assert_eq!(
            outcome.level_failures.len(),
            2,
            "three files reporting the same two broken levels is two reports, got {:?}",
            outcome.level_failures,
        );
        for level in [GlmDataLevel::Flash, GlmDataLevel::Group] {
            assert!(
                outcome.level_failures.iter().any(|f| f.level == level),
                "{level:?} must survive dedup, got {:?}",
                outcome.level_failures,
            );
        }
    }

    /// The accumulator is where a batch becomes the poll's totals. Dropping any
    /// bucket here is invisible from the async fetch that calls it, and dropping
    /// the level bucket reinstates the round-2 regression whole.
    #[test]
    fn the_accumulator_forwards_every_bucket() {
        let mut acc = PollAccumulator::default();
        acc.absorb(
            GlmSatellite::GoesWest,
            &[GlmDataLevel::Group, GlmDataLevel::Flash],
            BatchOutcome {
                entries: vec![("a.nc".into(), Vec::new())],
                parse_errors: vec!["p".into()],
                transport_errors: vec!["t".into()],
                level_failures: vec![level_failure(GlmSatellite::GoesWest, GlmDataLevel::Flash)],
            },
        );

        assert_eq!(acc.entries.len(), 1);
        assert_eq!(acc.parse_errors, vec!["p"]);
        assert_eq!(acc.transport_errors, vec!["t"]);
        assert_eq!(acc.level_failures.len(), 1, "the level bucket must not be dropped");
        assert_eq!(
            acc.evaluated_levels,
            vec![
                (GlmSatellite::GoesWest, GlmDataLevel::Group),
                (GlmSatellite::GoesWest, GlmDataLevel::Flash),
            ],
            "a granule that parsed is evidence about every level it was asked for"
        );
    }

    /// ...but only a granule that actually parsed is evidence. A batch where
    /// every file failed tells us nothing about the levels inside them, and
    /// treating that as evidence would announce a recovery on an outage.
    #[test]
    fn a_batch_that_parsed_nothing_is_not_evidence() {
        let mut acc = PollAccumulator::default();
        acc.absorb(
            GlmSatellite::GoesEast,
            &[GlmDataLevel::Flash],
            BatchOutcome {
                entries: Vec::new(),
                parse_errors: vec!["every file failed".into()],
                transport_errors: Vec::new(),
                level_failures: Vec::new(),
            },
        );
        assert!(
            acc.evaluated_levels.is_empty(),
            "a batch with no successful parse cannot vouch for any level"
        );
    }

    /// The failure denominator counts the whole window, not just this poll's
    /// downloads. Successes are cached and stop being re-downloaded; failures
    /// are retried every poll. Conflating the two is what made one corrupt
    /// granule read as "1/1 — everything failed" after a few ticks.
    #[test]
    fn poll_plan_separates_window_size_from_work_to_do() {
        let keys: Vec<String> = (0..12).map(|i| format!("k{i}.nc")).collect();

        let mut cache = GlmCache::default();
        for key in keys.iter().take(9) {
            cache.insert(key.clone(), Vec::new());
        }

        let mut tally = PollTally::default();
        let new_keys = plan_downloads(&keys, &cache, &mut tally);
        assert_eq!(
            tally.in_window, 12,
            "the window still contains every listed file, cached or not"
        );
        assert_eq!(new_keys.len(), 3, "only the uncached ones need downloading");

        // The tally accumulates across satellites rather than being overwritten.
        let other: Vec<String> = (0..4).map(|i| format!("w{i}.nc")).collect();
        plan_downloads(&other, &GlmCache::default(), &mut tally);
        assert_eq!(tally.in_window, 16);

        // The pathological steady state: everything cached but one straggler,
        // which is what a 20 s poll against 20 s granules looks like.
        let mut cache = GlmCache::default();
        for key in keys.iter().take(11) {
            cache.insert(key.clone(), Vec::new());
        }
        let mut tally = PollTally::default();
        let new_keys = plan_downloads(&keys, &cache, &mut tally);
        assert_eq!(new_keys.len(), 1);
        let report = summarize_failures(tally.in_window, vec!["k11.nc: boom".into()])
            .expect("one failure");
        assert!(
            !report.is_total(),
            "one straggler failing must never read as a total outage, got {report:?}"
        );
    }

    fn level_failure(satellite: GlmSatellite, level: GlmDataLevel) -> LevelFailure {
        LevelFailure { satellite, level, sample_error: format!("{level:?} broke") }
    }

    /// Every bucket must land in its own field. Swapping two makes every 503
    /// read as "product change?"; *dropping* the level bucket restores the
    /// round-2 regression whole, since an unreported level failure is exactly
    /// the silence `LevelFailure` was added to break.
    #[test]
    fn build_outcome_binds_each_bucket_to_its_own_field() {
        let tally = PollTally { in_window: 12 };
        let acc = PollAccumulator {
            parse_errors: vec!["a.nc: GLM file has no 'flash_lat' variable".into()],
            transport_errors: vec!["b.nc: HTTP status error: 503".into()],
            level_failures: vec![level_failure(GlmSatellite::GoesWest, GlmDataLevel::Flash)],
            // Deliberately a *superset* of the failures: Group was evaluated
            // and found healthy. Identical sets would let `evaluated_levels` be
            // derived from `level_failures` — "evidence from asking, not from
            // parsing" — which at integration level degenerates into a layer
            // that can never clear.
            evaluated_levels: vec![
                (GlmSatellite::GoesWest, GlmDataLevel::Flash),
                (GlmSatellite::GoesWest, GlmDataLevel::Group),
            ],
            ..Default::default()
        };
        let outcome =
            build_outcome(Vec::new(), Vec::new(), vec![GlmSatellite::GoesEast], &tally, acc);

        assert_eq!(
            outcome.parse_failures.expect("parse failures").sample_error,
            "a.nc: GLM file has no 'flash_lat' variable",
        );
        assert_eq!(
            outcome.transport_failures.expect("transport failures").sample_error,
            "b.nc: HTTP status error: 503",
        );
        assert_eq!(outcome.queried, vec![GlmSatellite::GoesEast]);

        // The level bucket is carried through untouched — not summarised into a
        // file count, and not dropped.
        assert_eq!(
            outcome.level_failures,
            vec![level_failure(GlmSatellite::GoesWest, GlmDataLevel::Flash)],
        );
        assert_eq!(
            outcome.evaluated_levels,
            vec![
                (GlmSatellite::GoesWest, GlmDataLevel::Flash),
                (GlmSatellite::GoesWest, GlmDataLevel::Group),
            ],
            "the evidence set must survive independently of the failures, or \
             every quiet poll reads as a recovery"
        );
    }

    /// A level failure is not a file failure. Routing it through
    /// `summarize_failures` would announce "N/M files failed to parse" while the
    /// other layers are still drawing.
    #[test]
    fn build_outcome_keeps_level_failures_out_of_the_file_counts() {
        let tally = PollTally { in_window: 9 };
        let acc = PollAccumulator {
            level_failures: vec![level_failure(GlmSatellite::GoesEast, GlmDataLevel::Group)],
            ..Default::default()
        };
        let outcome = build_outcome(Vec::new(), Vec::new(), Vec::new(), &tally, acc);

        assert!(outcome.parse_failures.is_none(), "no *file* failed");
        assert!(outcome.transport_failures.is_none());
        assert_eq!(outcome.level_failures.len(), 1);
    }

    /// Both kinds share the window as their denominator, and an empty bucket
    /// stays `None` rather than reporting a zero-failure failure.
    #[test]
    fn build_outcome_leaves_an_empty_bucket_unreported() {
        let tally = PollTally { in_window: 14 };
        let acc = PollAccumulator {
            parse_errors: vec!["a.nc: boom".into()],
            ..Default::default()
        };
        let outcome = build_outcome(Vec::new(), Vec::new(), Vec::new(), &tally, acc);

        assert_eq!(outcome.parse_failures.expect("parse failures").in_window, 14);
        assert!(
            outcome.transport_failures.is_none(),
            "nothing failed to download, so there is nothing to report"
        );
    }

    /// Bytes that arrived but are not the product are a *parse* failure. Tagging
    /// them Transport would point the user at their network for a product
    /// problem.
    #[test]
    fn garbage_bytes_are_a_parse_failure_not_a_transport_failure() {
        let err = parse_downloaded_file(
            b"this is not a netcdf file",
            GlmSatellite::GoesEast,
            &[GlmDataLevel::Flash],
        )
        .expect_err("garbage must not parse");

        assert!(
            matches!(err, FileError::Parse(_)),
            "expected a parse failure, got {err:?}"
        );
    }

    /// A valid granule still parses through the classified wrapper.
    #[test]
    fn a_good_granule_parses_through_the_classified_stage() {
        let bytes = synthetic_glm_file(Fixture::default());
        let flashes =
            parse_downloaded_file(&bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Flash])
                .expect("fixture should parse");
        assert_eq!(flashes.records.len(), 2);
        assert!(flashes.level_failures.is_empty());
    }

    /// A download that never lands is a *transport* failure, all the way out
    /// through `download_and_parse_one`.
    ///
    /// Hermetic: port 1 on loopback is the privileged `tcpmux` port and is not
    /// listening, so the connection is refused immediately. No server is started
    /// and no external network is touched. This is the end-to-end half of the
    /// classification — without it, tagging the download stage `Parse` compiles,
    /// passes, and tells users their GLM product changed whenever S3 throttles.
    #[test]
    fn an_unreachable_host_is_a_transport_failure() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client");

        let err = runtime
            .block_on(download_and_parse_one(
                &client,
                "http://127.0.0.1:1/nonexistent.nc",
                GlmSatellite::GoesEast,
                &[GlmDataLevel::Flash],
            ))
            .expect_err("nothing listens on loopback port 1");

        assert!(
            matches!(err, FileError::Transport(_)),
            "a refused connection must not be reported as a product problem, got {err:?}"
        );
    }

    #[test]
    fn summarize_failures_reports_none_when_everything_worked() {
        assert!(summarize_failures(12, Vec::new()).is_none());
    }

    /// The total/partial distinction drives both the log severity and the panel
    /// wording, so pin it on the boundary rather than trusting the caller.
    #[test]
    fn summarize_failures_distinguishes_total_from_partial() {
        let partial = summarize_failures(12, vec!["a".into(), "b".into()])
            .expect("failures present");
        assert_eq!((partial.failed, partial.in_window), (2, 12));
        assert!(!partial.is_total());
        assert_eq!(partial.sample_error, "a", "should keep the first error as the sample");

        let total = summarize_failures(3, vec!["a".into(), "b".into(), "c".into()])
            .expect("failures present");
        assert!(total.is_total(), "every file in the window failed");
    }

    fn all_failed(in_window: usize) -> FetchFailures {
        let errors: Vec<String> = (0..in_window).map(|i| format!("f{i}: boom")).collect();
        summarize_failures(in_window, errors).expect("failures present")
    }

    /// The floor, asserted against literals.
    ///
    /// Deliberately *not* parameterised on `MIN_FILES_FOR_TOTAL_VERDICT`: a loop
    /// over `1..CONST` is empty when the constant is 1 and passes vacuously, so
    /// the constant could be reverted to a value that restores the "All 1 files
    /// failed" defect with the suite green. The literals below are the
    /// behaviour; the constant is just where it is written down.
    #[test]
    fn total_verdict_needs_more_than_one_file() {
        assert!(
            !all_failed(1).is_total(),
            "a single-file window is too small a sample to call systematic"
        );
        assert!(
            all_failed(2).is_total(),
            "two files is the smallest honest verdict, and is what a 60 s window holds"
        );
        assert!(all_failed(3).is_total());
        assert!(all_failed(14).is_total(), "the default 300 s window");
    }

    /// The floor must not swallow the case it exists for: one bad granule among
    /// several is partial, at every window size.
    #[test]
    fn one_bad_granule_is_never_a_total_failure() {
        for in_window in [2usize, 5, 14, 89] {
            let report = summarize_failures(in_window, vec!["f0: boom".into()])
                .expect("failures present");
            assert!(
                !report.is_total(),
                "1 of {in_window} failing is a bad granule, not a product change"
            );
        }
    }

    /// A short *optional* column degrades instead of failing the file, and must
    /// not hand back a half-length area column.
    #[test]
    fn a_short_area_column_degrades_to_no_area() {
        let bytes = synthetic_glm_file(Fixture {
            short: Some("flash_area"),
            ..Default::default()
        });
        let flashes = parse_flashes(&bytes).expect("a short area must not fail the file");
        assert_eq!(flashes.len(), 2);
        assert!(flashes.iter().all(|f| f.area.is_none()));
    }

    /// Area is the one optional column, so losing it degrades the popup rather
    /// than the whole overlay: the flashes still parse, they just carry no area.
    #[test]
    fn missing_optional_area_degrades_without_failing_the_file() {
        let bytes = synthetic_glm_file(Fixture {
            omit: &["flash_area"],
            ..Default::default()
        });
        let flashes = parse_flashes(&bytes)
            .expect("a missing area must not blank the whole overlay");
        assert_eq!(flashes.len(), 2);
        assert!(flashes.iter().all(|f| f.area.is_none()));
        // Position and energy are untouched.
        assert!((flashes[0].lat - 35.0).abs() < 1e-4);
        assert!(flashes[0].energy.is_some_and(|e| e > 0.0));
    }
}
