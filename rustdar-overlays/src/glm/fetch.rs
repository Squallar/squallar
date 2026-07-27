//! Fetch GLM lightning flash data from AWS S3.
//!
//! Lists and downloads L2 LCFA NetCDF4 files from the public GOES buckets
//! declared in [`DataSources`]. Each granule covers ~20 seconds.

use std::collections::HashMap;

use chrono::{NaiveDateTime, TimeDelta, Utc};
use rustdar_radar::sources::DataSources;

use super::cf;
use super::{
    DeadFeed, FetchFailures, GLM_MIN_TIME_WINDOW_SECS, GlmDataLevel, GlmFetchOutcome, GlmFlash,
    GlmSatellite, LevelFailure,
};

/// One downloaded granule: what it parsed to, and when it is from.
#[derive(Clone)]
struct CachedGranule {
    /// Records parsed at the levels the user selected. Empty is normal.
    flashes: Vec<GlmFlash>,
    /// The newest instant this granule can vouch for, and the only thing
    /// [`GlmCache::evict_before`] reads.
    ///
    /// A stored field, not a predicate over `flashes`: an empty granule is a
    /// successful download, and any predicate over the vector answers "not in
    /// window" for it, which re-downloads every quiet granule on every poll.
    newest: NaiveDateTime,
}

impl CachedGranule {
    /// Age a granule against its newest flash, falling back to the start time
    /// its own S3 key encodes when it holds no flashes.
    ///
    /// A granule spans ~20 s from the start time it is keyed by, so its records
    /// all land at or after `granule_start`.
    fn new(granule_start: NaiveDateTime, flashes: Vec<GlmFlash>) -> Self {
        let newest = flashes.iter().map(|f| f.time).max().unwrap_or(granule_start);
        CachedGranule { flashes, newest }
    }
}

/// Cached GLM file data keyed by S3 object key.
#[derive(Default, Clone)]
pub struct GlmCache {
    entries: HashMap<String, CachedGranule>,
}

impl GlmCache {
    /// Remove cached entries whose flashes are entirely outside the time window.
    ///
    /// Granule granularity: an entry is one downloaded file, so a file survives
    /// as long as *one* flash in it is in window and [`flashes_in_window`] does
    /// the per-flash narrowing. Tightening this to `all` would evict the
    /// granule straddling the cutoff — the newest-but-one file — every poll.
    ///
    /// `cutoff` is inclusive, and the same instant goes to `flashes_in_window`.
    pub fn evict_before(&mut self, cutoff: NaiveDateTime) {
        self.entries.retain(|_key, granule| granule.newest >= cutoff);
    }

    /// Iterate over all cached flashes.
    pub fn all_flashes(&self) -> impl Iterator<Item = &GlmFlash> {
        self.entries.values().flat_map(|g| &g.flashes)
    }

    /// Check whether a key is already cached.
    ///
    /// True for a granule that parsed to nothing: [`plan_downloads`] reads this,
    /// and an already-downloaded empty granule must not be fetched again.
    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Insert parsed flashes for a given S3 key.
    ///
    /// `granule_start` is passed rather than inferred because a granule with no
    /// records has nothing to infer it from. See [`granule_start_of`].
    pub fn insert(&mut self, key: String, granule_start: NaiveDateTime, flashes: Vec<GlmFlash>) {
        self.entries.insert(key, CachedGranule::new(granule_start, flashes));
    }
}

/// Fold this poll's parsed granules into the cache.
///
/// Empty granules are cached like any other. Filtering them out here would make
/// [`plan_downloads`] re-queue every one of them on the next poll.
fn cache_granules(
    cache: &mut GlmCache,
    entries: Vec<(String, Vec<GlmFlash>)>,
    now: NaiveDateTime,
) {
    for (key, flashes) in entries {
        let granule_start = granule_start_of(&key, now);
        cache.insert(key, granule_start, flashes);
    }
}

/// The instant a granule is aged against, taken from the S3 key it was listed
/// under.
///
/// The fallback is unreachable for anything a poll listed ([`list_glm_files`]
/// admits a key only when its time parses and is in window). It is `now` so an
/// undatable granule expires one window out — never instantly (re-downloaded
/// every poll) and never not at all (unbounded cache).
fn granule_start_of(key: &str, now: NaiveDateTime) -> NaiveDateTime {
    parse_filename_start_time(key).unwrap_or(now)
}

/// Fetch GLM data from one or both satellites for the given time window.
///
/// Empty listings are *reported*, not logged: only the caller holds the previous
/// poll's state and can edge-trigger the message.
pub async fn fetch_glm_flashes(
    client: &reqwest::Client,
    sources: &DataSources,
    satellites: &[GlmSatellite],
    time_window_secs: f64,
    levels: &[GlmDataLevel],
    cache: &mut GlmCache,
) -> Result<GlmFetchOutcome, String> {
    // The zero-object warning below assumes the queried range is wide enough to
    // always cover an already-published granule.
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
        let bucket = sat.bucket(sources);
        let listing = list_glm_files(client, bucket, start, now).await?;

        // Zero objects means the feed is gone (dead bucket, renamed path,
        // satellite rotated out of the slot), not a quiet sky: GOES-East
        // rendered nothing for over a year once `noaa-goes16` went dead.
        // Objects present but no in-window flashes is normal and stays silent.
        if listing.objects_seen == 0 {
            dead_feeds.push(DeadFeed {
                satellite: sat,
                bucket: bucket.to_string(),
                prefixes: listing.prefixes.clone(),
            });
        }

        let new_keys = plan_downloads(&listing.keys, cache, &mut tally);

        if new_keys.is_empty() {
            continue;
        }
        log::info!("Downloading {} new GLM files from {}", new_keys.len(), sat.display_name());

        // Download in concurrent batches of 20
        let batch = download_and_parse_batch(client, sat, bucket, &new_keys, levels).await;
        acc.absorb(sat, levels, batch);
    }

    // Insert new data into cache
    cache_granules(cache, std::mem::take(&mut acc.entries), now);

    // Return all cached flashes within the window
    let filtered = flashes_in_window(cache, satellites, cutoff, now);

    log::info!("GLM: {} flashes in {:.0}s window", filtered.len(), time_window_secs);

    // Reported, not logged, for the same reason dead feeds are.
    Ok(build_outcome(filtered, dead_feeds, satellites.to_vec(), &tally, acc))
}

/// Select the cached flashes that fall inside this poll's window, from the
/// satellites this poll was asked for.
///
/// The per-flash half of a two-stage narrowing; [`GlmCache::evict_before`] does
/// the per-granule half and neither subsumes the other.
///
/// The satellite clause is the only thing standing between a deselected bird
/// and the screen: the cache deliberately survives a "Both" → "East" switch
/// (records carry their satellite), so without it GOES-West's cached flashes
/// kept rendering for up to the whole 30-minute window. Filtering here rather
/// than clearing the cache makes re-selection restore instantly, with no
/// re-download.
///
/// Both time bounds are inclusive and both are load-bearing. `<= now` is not
/// redundant: `now` is wall-clock sampled once per poll and can sit behind the
/// data — a granule is listed by its start time and spans ~20 s, so its last
/// flashes can be stamped after `now`, and an NTP step backwards does the same.
/// Such flashes stay cached and appear next poll, which is why dropping this
/// clause is silent.
fn flashes_in_window(
    cache: &GlmCache,
    satellites: &[GlmSatellite],
    cutoff: NaiveDateTime,
    now: NaiveDateTime,
) -> Vec<GlmFlash> {
    cache
        .all_flashes()
        .filter(|f| satellites.contains(&f.satellite) && f.time >= cutoff && f.time <= now)
        .cloned()
        .collect()
}

/// Assemble the outcome a poll reports.
///
/// Pure and separate from the async fetch so a test can pin which error bucket
/// lands in which field — swapping the two `summarize_failures` calls turns
/// every 503 into "product change?".
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
        // Not routed through `summarize_failures`: a level failure has no
        // file-count denominator.
        level_failures: acc.level_failures,
        evaluated_levels: acc.evaluated_levels,
    }
}

/// What one poll accumulated across satellites, before it is shaped into the
/// outcome the UI reads.
#[derive(Default)]
struct PollAccumulator {
    entries: Vec<(String, Vec<GlmFlash>)>,
    parse_errors: Vec<String>,
    transport_errors: Vec<String>,
    level_failures: Vec<LevelFailure>,
    /// (satellite, level) pairs this poll actually gathered evidence about.
    ///
    /// A level is only found broken by parsing, so a poll that downloads nothing
    /// new learns nothing — routine, since the 20 s poll interval races the
    /// ~20 s granule cadence. Without this, "healthy again" and "did not look"
    /// are indistinguishable to the caller.
    evaluated_levels: Vec<(GlmSatellite, GlmDataLevel)>,
}

impl PollAccumulator {
    /// Fold one satellite's batch in.
    fn absorb(&mut self, satellite: GlmSatellite, levels: &[GlmDataLevel], batch: BatchOutcome) {
        self.parse_errors.extend(batch.parse_errors);
        self.transport_errors.extend(batch.transport_errors);
        self.level_failures.extend(batch.level_failures);

        // Evidence requires a granule that actually parsed: a batch where every
        // file failed says nothing about the levels inside them.
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
/// The tally counts every listed key, not the returned ones, and the two must
/// stay different: cached successes drop out of the returned keys while failures
/// are never cached and are retried every poll, so a download-count denominator
/// makes one persistent failure look like a total outage after a few ticks.
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

/// Result of listing S3 for one satellite.
struct GlmListing {
    /// Keys that are `.nc` files whose encoded start time falls in the window.
    keys: Vec<String>,
    /// Total objects S3 returned across all prefixes, before any filtering.
    /// Zero means the bucket/prefix has nothing in it whatsoever.
    objects_seen: usize,
    /// Prefixes queried, for diagnostics.
    prefixes: Vec<String>,
}

/// List GLM LCFA file keys on S3 for the given time range. `bucket` is the
/// slot's declared origin — see [`GlmSatellite::bucket`].
///
/// S3 path: `GLM-L2-LCFA/{year}/{day_of_year}/{hour}/`
/// Files: `OR_GLM-L2-LCFA_G{sat}_s{start}_e{end}_c{creation}.nc`
async fn list_glm_files(
    client: &reqwest::Client,
    bucket: &str,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<GlmListing, String> {
    let mut all_keys = Vec::new();
    let mut objects_seen = 0usize;

    // Collect all (year, doy, hour) tuples we need to query. This emits a
    // *single* prefix when `start` and `end` share a UTC hour, so the
    // zero-object warning can be looking at one young hour prefix.
    //
    // Required coupling:
    //
    //     GLM_MIN_TIME_WINDOW_SECS  >  S3 publish latency for the hour's first object
    //
    // The hour's 00:00:00 granule lands 27–30 s after the boundary (measured
    // across two consecutive live hours on noaa-goes19; worst case in that
    // sample 41 s for a mid-hour file), and a single-prefix query requires
    // `now >= hour_start + window`, so a 60 s minimum leaves ~30 s of headroom.
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
            // re-issue the identical first-page request forever, inside an
            // async task with no timeout.
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
    let filename = key.rsplit('/').next()?;
    let s_idx = filename.find("_s")?;
    let s_field = &filename[s_idx + 2..];
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
    /// exactly like a quiet sky.
    parse_errors: Vec<String>,
    /// One message per file that never arrived. Tracked separately so a network
    /// problem is never reported as a product schema change.
    transport_errors: Vec<String>,
    /// Levels that failed inside files that otherwise parsed, deduplicated per
    /// (satellite, level).
    level_failures: Vec<LevelFailure>,
}

/// Download and parse a batch of GLM NetCDF files concurrently. `bucket` is
/// the slot's declared origin — see [`GlmSatellite::bucket`].
async fn download_and_parse_batch(
    client: &reqwest::Client,
    satellite: GlmSatellite,
    bucket: &str,
    keys: &[&str],
    levels: &[GlmDataLevel],
) -> BatchOutcome {
    use futures::stream::StreamExt;

    let levels_owned: Vec<GlmDataLevel> = levels.to_vec();
    let futs: Vec<_> = keys.iter().map(|&key| {
        let client = client.clone();
        let url = DataSources::s3_object_url(bucket, key);
        let key_owned = key.to_string();
        let lvls = levels_owned.clone();
        async move {
            match download_and_parse_one(&client, &url, satellite, &lvls).await {
                Ok(parsed) => Ok((key_owned, parsed)),
                Err(e) => {
                    // Debug, not warn: with 20 files in flight a single schema
                    // change would produce a wall of identical lines. The
                    // aggregate is reported instead.
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
    /// Separated from the async download so the partition is testable.
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
                    // One batch is one satellite, but the satellite is compared
                    // anyway: it is part of `LevelFailure`'s identity and the
                    // accumulator merges across satellites.
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

/// Why one file did not contribute. A file that arrives and will not parse
/// indicts the product; a file that never arrives indicts the network.
///
/// Classified by *stage*, not by content, with one known blind spot: a captive
/// portal answering 200 with an HTML error page is reported as `Parse`.
/// Accepted deliberately — sniffing the body for the NetCDF magic number risks
/// misreading a genuine product change as a network fault.
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
/// nothing failed.
fn summarize_failures(in_window: usize, errors: Vec<String>) -> Option<FetchFailures> {
    let sample_error = errors.first()?.clone();
    Some(FetchFailures {
        in_window,
        failed: errors.len(),
        sample_error,
    })
}

/// Fetch the raw bytes of one object. Every failure here is a transport failure
/// by construction.
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
/// Split so the transport/parse classification happens at exactly two call
/// sites rather than at each error site, where one could drift.
async fn download_and_parse_one(
    client: &reqwest::Client,
    url: &str,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, FileError> {
    let bytes = download_bytes(client, url).await.map_err(FileError::Transport)?;
    parse_downloaded_file(&bytes, satellite, levels)
}

/// Parse bytes that already arrived. Any failure here is a parse failure.
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

// The L2 LCFA product has no `event_area` variable — only groups and flashes
// carry area coverage (confirmed against a live `noaa-goes19` granule).
const EVENT_VARS: LevelVars = LevelVars {
    lat: "event_lat",
    lon: "event_lon",
    energy: "event_energy",
    area: None,
    time_offset: "event_time_offset",
    level: GlmDataLevel::Event,
};

/// Where variables come from.
///
/// The parser is written against this so multiple readers share *identical*
/// parsing, unit conversion and time handling — any difference a differential
/// test finds is then a difference in the reader.
pub(crate) trait VarSource {
    /// Read a variable with CF packing applied, or `Ok(None)` if it is absent.
    fn read_unpacked(&self, name: &str) -> Result<Option<cf::UnpackedVar>, String>;
    /// The `time_coverage_start` global attribute, verbatim.
    fn time_coverage_start(&self) -> Option<String>;
}

impl VarSource for super::h5::Granule {
    fn read_unpacked(&self, name: &str) -> Result<Option<cf::UnpackedVar>, String> {
        super::h5::Granule::read_unpacked(self, name)
    }
    fn time_coverage_start(&self) -> Option<String> {
        self.global_str("time_coverage_start")
    }
}

/// Parse GLM data from NetCDF4/HDF5 bytes in memory.
pub(crate) fn parse_glm_netcdf(
    data: &[u8],
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, String> {
    let file = super::h5::Granule::open(data)?;
    parse_with_source(&file, satellite, levels)
}

fn parse_with_source<S: VarSource>(
    file: &S,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, String> {
    // Fallback epoch. Every `*_time_offset` variable also names its own epoch
    // in `units`, and in every granule inspected the two agree exactly
    // (`time_coverage_start` = "2026-07-24T12:00:00.0Z", `units` = "seconds
    // since 2026-07-24 12:00:00.000"). The per-variable epoch wins where
    // present — see `parse_level_records`.
    let time_origin = file
        .time_coverage_start()
        .as_deref()
        .and_then(cf::parse_cf_epoch)
        .ok_or_else(|| "Missing or invalid time_coverage_start attribute".to_string())?;

    let mut all_records = Vec::new();
    let mut failures: Vec<LevelFailure> = Vec::new();

    for level in levels {
        let vars = match level {
            GlmDataLevel::Flash => &FLASH_VARS,
            GlmDataLevel::Group => &GROUP_VARS,
            GlmDataLevel::Event => &EVENT_VARS,
        };
        // One level failing must not take the others with it: the three are
        // independent variable sets, selected independently.
        match parse_level_records(file, vars, &time_origin, satellite) {
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

    // Every requested level failing makes the granule unusable, so it is
    // reported as a failed *file* and joins the "N/M files failed to parse"
    // count. The first error is propagated verbatim: it becomes
    // `FetchFailures::sample_error`, the only place the operator sees a cause.
    if !failures.is_empty() && failures.len() == levels.len() {
        return Err(failures.swap_remove(0).sample_error);
    }

    // A *partial* failure keeps the healthy levels and reports the broken one
    // through `level_failures`. `Err` would discard good group records over a
    // flash-only schema change; a bare `Ok` means `parse_failures: None`, which
    // the panel reads as "everything is fine" while the layer sits empty.
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
/// The L2 LCFA product declares `flash_area:units = "m2"`: raw count 1826
/// is 1826 × 152601.9 m² = 278.7 km², not "1826.0 km²".
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
/// land between roughly 1e-15 and 1e-12 J. No SI prefixes: lookup is
/// case-folded, so "mJ" and "MJ" would collide into a silent factor of 1e9.
const ENERGY_UNITS: &[(&str, f64)] = &[("j", 1.0), ("joule", 1.0), ("joules", 1.0)];

/// Parse records for one GLM hierarchy level from the NetCDF file.
///
/// Every variable goes through CF unpacking (see [`super::cf`]): most GLM
/// variables are `_Unsigned` packed shorts and reading them raw yields
/// meaningless numbers.
fn parse_level_records<S: VarSource>(
    file: &S,
    vars: &LevelVars,
    time_origin: &chrono::NaiveDateTime,
    satellite: GlmSatellite,
) -> Result<Vec<GlmFlash>, String> {
    // Required *columns*: absence is a schema change and fails the level. An
    // absent *value* is a different condition and arrives quietly as `None`
    // inside `UnpackedVar::values`.
    let lats = read_required_unpacked(file, vars.lat)?;
    let lons = read_required_unpacked(file, vars.lon)?;
    let energies = read_required_unpacked(file, vars.energy)?;
    let times = read_required_unpacked(file, vars.time_offset)?;

    // Every variable at a level shares one dimension (`number_of_flashes`,
    // `number_of_groups`, `number_of_events`), so a short column means a
    // corrupt or restructured file.
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

    // Area is the one optional column: events have no area variable, so
    // `vars.area` is `None` there and we never look. A level that should have
    // one but does not degrades to no area rather than failing the file.
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

    // The time axis names its own epoch and unit, and wins over the
    // granule-level `time_coverage_start` so the two cannot silently drift.
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

    // Unit resolution is scoped to the field it describes: `None` makes that
    // descriptive field unknown, and must not take position, time or the other
    // hierarchy levels down with it.
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
        // A `_FillValue` in any field that places a strike in space and time
        // makes the detection unusable; drop it rather than fabricate a number.
        let (Some(lat), Some(lon), Some(offset)) =
            (lats.values[i], lons.values[i], times.values[i])
        else {
            missing += 1;
            continue;
        };

        let lon = normalize_longitude(lon);

        // Backstop against a coordinate that unpacked to nonsense. Effectively
        // guards latitude only: the wrap above can carry a mis-unpacked
        // longitude back into the valid interval.
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            off_globe += 1;
            continue;
        }

        // Energy and area are descriptive, not locating: an unreported value
        // leaves the field `None` and keeps the record. Never zero — `0f32
        // .log10()` is -inf and `rasterize` draws unknown as the smallest
        // possible bolt. `flash_energy`/`event_energy` carry
        // `_FillValue = -1s`, so a value can be absent in a present column:
        // required column, optional value.
        let energy = column_value(Some(&energies), i)
            .zip(energy_to_j)
            .map(|(v, to_j)| (v * to_j) as f32);
        let area = column_value(areas.as_ref(), i)
            .zip(area_to_km2)
            .map(|(v, to_km2)| (v * to_km2) as f32);

        // Microseconds, not milliseconds: GLM's time `scale_factor` is
        // 3.814756e-4 s, so representable instants are 0.38 ms apart and
        // millisecond truncation collapses adjacent ones. On a `milliseconds
        // since` axis it would truncate the sub-millisecond offsets to zero.
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
/// Not satellite-qualified, unlike the unit warnings: the variable *set* is a
/// property of the product schema, not of a bird.
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
/// The *column presence* half of a two-level distinction:
///
/// * **Variable absent from the file** — a schema change. Warned once and
///   failed here. Returning an empty column instead is what produced
///   "Area: 0.0 km²" for a year, callers having padded it with zeros.
///
/// * **An individual value is `_FillValue`** (or outside `valid_range`) — a
///   per-record condition the product defines: `flash_area`, `group_area`,
///   `flash_energy` and `event_energy` all carry `_FillValue = -1s`. Carried
///   quietly as a `None` inside [`cf::UnpackedVar::values`].
fn read_required_unpacked<S: VarSource>(
    file: &S,
    name: &'static str,
) -> Result<cf::UnpackedVar, String> {
    match file.read_unpacked(name)? {
        Some(var) => Ok(var),
        None => {
            warn_missing_variable_once(name);
            Err(format!("GLM file has no '{name}' variable (product schema change?)"))
        }
    }
}

/// Read a variable a level may legitimately lack, with CF unpacking applied.
///
/// Returns `Ok(None)` when absent, but still reports it: declared optionality
/// lives in [`LevelVars::area`], so reaching here means we asked for something
/// the product used to have.
fn read_optional_unpacked<S: VarSource>(
    file: &S,
    name: &'static str,
) -> Result<Option<cf::UnpackedVar>, String> {
    let var = file.read_unpacked(name)?;
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
/// 172.72°E is stored as -187.28. Rejecting those as out-of-range deleted 60 of
/// 3228 events in the granule this was measured on, and *selectively* —
/// `group_lon`/`flash_lon` are genuine floats, already wrapped, reaching
/// +172.90 in the same file.
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
/// Collapsing the three into one `None` is safe only here: the column-level
/// conditions have already been reported by
/// [`read_required_unpacked`]/[`read_optional_unpacked`].
fn column_value(column: Option<&cf::UnpackedVar>, i: usize) -> Option<f64> {
    column?.values.get(i).copied().flatten()
}

/// Resolve the multiplier converting a variable's declared `units` into the
/// unit rustdar stores and displays, or `None` if that cannot be done.
///
/// The contract is symmetric: **a value is reported only when the file says
/// what unit it is in and we can convert that unit.** Both an absent `units`
/// attribute and an unrecognized one yield `None`. Assuming an absent one is
/// canonical would report `flash_area` — shipped as `m2` — a million times too
/// large, silently.
///
/// Failure is scoped to the field: losing `*_area`/`*_energy` costs a popup row
/// and must not cost the strike its position, time, or the other levels.
///
/// Diagnostics name the satellite: the two birds are separate product streams
/// that can diverge, so a problem on one must not suppress the other's warning.
fn unit_multiplier(
    satellite: GlmSatellite,
    name: &str,
    column: Option<&cf::UnpackedVar>,
    table: &[(&str, f64)],
    canonical: &str,
) -> Option<f64> {
    // No column at all is a product property, not an anomaly: there is no
    // `event_area`. A genuinely *missing* variable was already reported by
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

/// Stable per-slot token for the dedup keys below. Deliberately not the S3
/// bucket: that is resolved from [`DataSources`] at fetch time and a test can
/// point it elsewhere, while a warning's identity must not move when it does.
fn slot_key(satellite: GlmSatellite) -> &'static str {
    match satellite {
        GlmSatellite::GoesEast => "goes-east",
        GlmSatellite::GoesWest => "goes-west",
    }
}

/// Dedup key for "a hierarchy level would not parse". Satellite-qualified: a
/// change hitting one bird must not suppress the report for the other.
pub(super) fn level_parse_key(satellite: GlmSatellite, lat_var: &str) -> String {
    format!("{}:level-parse:{lat_var}", slot_key(satellite))
}

/// Dedup key for "a variable the product used to have is gone". Deliberately
/// *not* satellite-qualified: the variable set is a property of the product
/// schema, so qualifying it would report the same fact twice.
pub(super) fn missing_variable_key(name: &str) -> String {
    format!("variable-absent:{name}")
}

/// Dedup key for "this variable declares a unit we cannot convert". Keyed on
/// the satellite *and* the offending spelling, so a second bad spelling still
/// reports, and so does the same one on the other bird.
pub(super) fn units_key(satellite: GlmSatellite, name: &str, spelling: &str) -> String {
    format!("{}:{name}:units:{spelling}", slot_key(satellite))
}

/// Log a warning the first time a given key is seen, then stay quiet.
///
/// GLM polls every 20 s across up to two satellites, and the conditions this
/// guards are permanent once they appear. The single registry for the module:
/// absent-variable reports, unit problems and level-parse failures all key
/// into it, so one condition can never crowd out another.
pub(crate) fn warn_once(key: String, message: &str) {
    if claim_warning(key) {
        log::warn!("{message}");
    }
}

/// Record `key` as seen and report whether this is the first time.
///
/// Split out from [`warn_once`] so deduplication is testable without a log
/// capture.
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

// Native-only: builds a loopback client with `ClientBuilder::timeout`, which
// reqwest's wasm builder does not have.
#[cfg(all(test, not(target_arch = "wasm32")))]
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
    /// Writes plain unpacked `f32` in canonical units: the subject here is
    /// *column presence*, not CF packing, which `glm::tests` covers against
    /// packed shorts. `units` is still declared — an undeclared unit is
    /// reported as unknown.
    fn synthetic_glm_file(spec: Fixture<'_>) -> Vec<u8> {
        let mut file = hdf5_pure::FileBuilder::new();
        file.set_attr(
            "time_coverage_start",
            hdf5_pure::AttrValue::String("2026-07-24T12:00:00.0Z".into()),
        );

        {
            let mut put = |name: &str, values: &[f32]| {
                if spec.omit.contains(&name) {
                    return;
                }
                // A "short" column is one element instead of two: the length
                // *is* the data here, so there are no dimensions to mismatch.
                let values = if spec.short == Some(name) { &values[..1] } else { values };
                let var = file.create_dataset(name);
                var.with_f32_data(values);
                // Only the two unit-converted fields need a `units` attribute.
                let units = match name {
                    n if n.ends_with("_area") => Some("km2"),
                    n if n.ends_with("_energy") => Some("J"),
                    _ => None,
                };
                if let Some(u) = units {
                    var.set_attr("units", hdf5_pure::AttrValue::String(u.into()));
                }
            };

            put("flash_lat", &[35.0, 36.0]);
            put("flash_lon", &[-97.0, -98.0]);
            put("flash_energy", &[1.0e-14, 2.0e-14]);
            put("flash_area", &[128.0, 256.0]);
            put(
                "flash_time_offset_of_first_event",
                &[1.0, 2.0],
            );

            put("event_lat", &[35.5, 36.5]);
            put("event_lon", &[-97.5, -98.5]);
            put("event_energy", &[3.0e-15, 4.0e-15]);
            put("event_time_offset", &[3.0, 4.0]);
            // Note: no `event_area` — that is the point of the fixture.
        }

        file.finish().expect("write fixture")
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
        // Pins that the right column was read, in order, without pinning the
        // fixture's own scale: area = [128, 256] has a ratio of exactly 2,
        // which lat [35, 36] and lon [-97, -98] do not, and which a pure
        // scaling preserves (`flash_area` has add_offset = 0 in the product).
        // The `> 1.0` floor excludes the ~1e-14 energy column.
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
        // Fails if events fall back to Some(0.0), rendered as "Area: 0.0 km²".
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
    /// Verbatim, not `contains`: with the required-variable gate removed the
    /// downstream length check also errors, and its message interpolates both
    /// the offending name and `vars.lat`, so the two gates shadow each other.
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

    /// The case only the required-variable gate can catch: with every column
    /// equally absent there is no length mismatch to trip the downstream check.
    /// Fails if an absent variable reads back as an empty column, which parses
    /// cleanly into zero records — a blank map reported as success.
    #[test]
    fn a_whole_level_vanishing_is_an_error_not_zero_records() {
        let bytes = synthetic_glm_file(Fixture { omit: &FLASH_LEVEL_VARS, ..Default::default() });
        let err = parse_flashes(&bytes)
            .expect_err("an entirely absent level must not read as 'no lightning'");
        assert_eq!(err, absent_variable_error("flash_lat"));
    }

    /// Only the time variable is omitted, so the other columns stay equally
    /// sized and no length mismatch can mask the gate. The verbatim assertion
    /// proves which gate fired.
    #[test]
    fn a_missing_time_variable_alone_is_an_error() {
        let bytes = synthetic_glm_file(Fixture {
            omit: &["flash_time_offset_of_first_event"],
            ..Default::default()
        });
        let err = parse_flashes(&bytes).expect_err("absent time variable must surface");
        assert_eq!(err, absent_variable_error("flash_time_offset_of_first_event"));
    }

    /// Fails if energy falls back to 0.0, which the rasterizer turns into
    /// `0f32.log10()` = -inf and draws as a minimum-size bolt — total data loss
    /// that looks like a normal render.
    #[test]
    fn missing_energy_does_not_default_to_zero() {
        let bytes = synthetic_glm_file(Fixture {
            omit: &["flash_energy"],
            ..Default::default()
        });
        let err = parse_flashes(&bytes).expect_err("absent energy must surface");
        assert_eq!(err, absent_variable_error("flash_energy"));
    }

    /// A separate gate from the required-variable one: a *present but short*
    /// variable is corruption, and indexing past it would panic.
    #[test]
    fn a_short_required_column_is_rejected() {
        for short in ["flash_lon", "flash_energy", "flash_time_offset_of_first_event"] {
            let bytes = synthetic_glm_file(Fixture { short: Some(short), ..Default::default() });
            let err = parse_flashes(&bytes)
                .expect_err("a short column must be rejected, not indexed past");
            assert_eq!(err, length_mismatch_error(short, 1, "flash_lat", 2));
        }
    }

    /// Every per-file error must survive the batch partition, in the right
    /// bucket. Fails if errors are discarded into a log line, which is what
    /// made a total parse failure read as "Updated 0s ago".
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

    /// Fails if the accumulator drops any bucket — invisible from the async
    /// fetch that calls it.
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

    /// ...but only a granule that actually parsed is evidence: treating a batch
    /// where every file failed as evidence announces a recovery on an outage.
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

    /// The failure denominator counts the whole window, not this poll's
    /// downloads. Fails if the two are conflated, which makes one corrupt
    /// granule read as "1/1 — everything failed" after a few ticks.
    #[test]
    fn poll_plan_separates_window_size_from_work_to_do() {
        let keys: Vec<String> = (0..12).map(|i| format!("k{i}.nc")).collect();

        // Empty granules: the steady state a quiet sky produces, which must
        // read as "already downloaded".
        let mut cache = GlmCache::default();
        for key in keys.iter().take(9) {
            cache.insert(key.clone(), t0(), Vec::new());
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
            cache.insert(key.clone(), t0(), Vec::new());
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

    // ---------------------------------------------------------------------
    // Retention: `GlmCache::evict_before` and `flashes_in_window`. Every way
    // of getting these wrong renders identically — a quiet sky.
    // ---------------------------------------------------------------------

    /// An arbitrary but fixed instant to hang the retention tests off, so they
    /// never consult the wall clock.
    fn t0() -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    /// A wall clock that shares no instant with any fixture S3 key.
    ///
    /// `t0()` is 2026-07-24 = day of year 205, the same day the
    /// `..._s2026205....nc` fixture keys encode, so passing it as `now` makes
    /// "dated from the key" and "dated from the wall clock" indistinguishable.
    /// Any fixture feeding a `now` alongside a real key must use this.
    fn wall_clock_unlike_keys() -> NaiveDateTime {
        t0() + TimeDelta::hours(3) + TimeDelta::minutes(7)
    }

    /// A flash whose only interesting property is when it happened.
    fn flash_at(time: NaiveDateTime) -> GlmFlash {
        GlmFlash {
            lat: 38.967,
            lon: -82.1,
            energy: Some(1.0e-14),
            area: Some(278.65),
            time,
            satellite: GlmSatellite::GoesEast,
            level: GlmDataLevel::Flash,
        }
    }

    /// Cache keys sorted, so assertions on "what is left" are order-stable.
    fn cached_keys(cache: &GlmCache) -> Vec<String> {
        let mut keys: Vec<String> = cache.entries.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Cache a granule that parsed to at least one record, dating it the way S3
    /// does: a granule is keyed by the *start* of the ~20 s span it covers, so
    /// its records land at or after that instant.
    ///
    /// A granule with no records has no start to derive, so those fixtures must
    /// state one explicitly through [`GlmCache::insert`].
    fn cache_granule(cache: &mut GlmCache, key: &str, flashes: Vec<GlmFlash>) {
        let start = flashes
            .iter()
            .map(|f| f.time)
            .min()
            .expect("use GlmCache::insert directly for a granule that parsed to nothing");
        cache.insert(key.to_string(), start, flashes);
    }

    /// `cutoff` is *inclusive*: a granule whose newest flash lands exactly on
    /// the edge would otherwise be evicted and immediately re-downloaded.
    ///
    /// The tick either side is one millisecond because GLM times unpack through
    /// a `0.0003814756 s` scale factor — sub-second is the real resolution here,
    /// not a contrived epsilon.
    #[test]
    fn evict_before_keeps_a_granule_sitting_exactly_on_the_cutoff() {
        let cutoff = t0();
        let mut cache = GlmCache::default();
        cache_granule(&mut cache, "exactly_at.nc", vec![flash_at(cutoff)]);
        cache_granule(
            &mut cache,
            "one_tick_before.nc",
            vec![flash_at(cutoff - TimeDelta::milliseconds(1))],
        );
        cache_granule(
            &mut cache,
            "one_tick_after.nc",
            vec![flash_at(cutoff + TimeDelta::milliseconds(1))],
        );

        cache.evict_before(cutoff);

        assert_eq!(
            cached_keys(&cache),
            vec!["exactly_at.nc".to_string(), "one_tick_after.nc".to_string()],
            "the cutoff is inclusive: only the granule strictly before it goes"
        );
    }

    /// Eviction is per *granule*, not per flash. A granule spans ~20 s, so the
    /// newest-but-one file straddles the cutoff on essentially every poll;
    /// tightening this to "all flashes in window" evicts and re-downloads a
    /// live file every tick.
    #[test]
    fn evict_before_keeps_a_granule_that_straddles_the_cutoff() {
        let cutoff = t0();
        let stale = cutoff - TimeDelta::seconds(10);
        let fresh = cutoff + TimeDelta::seconds(10);

        let mut cache = GlmCache::default();
        cache_granule(&mut cache, "straddles.nc", vec![flash_at(stale), flash_at(fresh)]);

        cache.evict_before(cutoff);

        assert_eq!(cached_keys(&cache), vec!["straddles.nc".to_string()]);
        let times: Vec<NaiveDateTime> = cache.all_flashes().map(|f| f.time).collect();
        assert_eq!(
            times.len(),
            2,
            "the granule is kept intact; trimming individual flashes here would \
             mean re-downloading the file to get them back, and is not this \
             stage's job"
        );
        assert!(times.contains(&stale) && times.contains(&fresh));
    }

    /// The two ends of the range, and the degenerate case in between. A no-op
    /// eviction grows the cache without bound; a clear-everything eviction
    /// re-downloads the whole window every poll.
    #[test]
    fn evict_before_handles_an_empty_cache_and_both_extremes() {
        // Empty cache: must not panic, must stay empty.
        let mut empty = GlmCache::default();
        empty.evict_before(t0());
        assert_eq!(empty.all_flashes().count(), 0);
        assert!(cached_keys(&empty).is_empty());

        // Everything is stale.
        let mut cache = GlmCache::default();
        for i in 1..=3 {
            cache_granule(
                &mut cache,
                &format!("old{i}.nc"),
                vec![flash_at(t0() - TimeDelta::minutes(i))],
            );
        }
        cache.evict_before(t0());
        assert!(
            cached_keys(&cache).is_empty(),
            "an eviction that keeps stale granules is a cache that grows forever"
        );

        // Nothing is stale.
        let mut cache = GlmCache::default();
        for i in 1..=3 {
            cache_granule(
                &mut cache,
                &format!("new{i}.nc"),
                vec![flash_at(t0() + TimeDelta::minutes(i))],
            );
        }
        cache.evict_before(t0());
        assert_eq!(
            cached_keys(&cache).len(),
            3,
            "an eviction that clears live granules re-downloads the whole window \
             every poll"
        );
    }

    /// A granule that parsed to *zero* records is aged by its own start time.
    ///
    /// Fails if eviction goes back to a predicate over the flashes, which
    /// evicts every empty granule immediately: at the 30-minute maximum window
    /// that re-fetched roughly 90 granules × ~250 KB ≈ 22 MB every 20 s.
    #[test]
    fn evict_before_ages_an_empty_granule_by_its_own_start_time() {
        let start = t0();
        let mut cache = GlmCache::default();
        cache.insert("quiet.nc".into(), start, Vec::new());

        // A cutoff far behind the granule: an empty granule inside the window
        // is downloaded data, and must survive exactly like a populated one.
        cache.evict_before(start - TimeDelta::days(365));
        assert!(
            cache.contains_key("quiet.nc"),
            "an empty parse is a successful download; evicting it here is what \
             re-fetched the whole listing window every poll"
        );

        // ...and it is not immortal either: past its own start it goes, on the
        // same schedule a populated granule would.
        cache.evict_before(start + TimeDelta::milliseconds(1));
        assert!(
            !cache.contains_key("quiet.nc"),
            "an empty granule that never expires is the opposite bug: a cache \
             that grows without bound"
        );
    }

    /// An empty granule ages out on *exactly* the same schedule as a populated
    /// one covering the same instant — retention must not be bought by making
    /// quiet granules special.
    #[test]
    fn an_empty_granule_ages_out_on_the_same_schedule_as_a_populated_one() {
        let start = t0();
        let tick = TimeDelta::milliseconds(1);

        for cutoff in [start - tick, start, start + tick] {
            let mut cache = GlmCache::default();
            cache.insert("quiet.nc".into(), start, Vec::new());
            cache_granule(&mut cache, "busy.nc", vec![flash_at(start)]);

            cache.evict_before(cutoff);

            assert_eq!(
                cache.contains_key("quiet.nc"),
                cache.contains_key("busy.nc"),
                "at cutoff {cutoff} the empty granule and the populated one that \
                 covers the same instant disagreed: quiet={}, busy={}",
                cache.contains_key("quiet.nc"),
                cache.contains_key("busy.nc"),
            );
        }
    }

    /// End to end: cache → evict → plan. Both halves are needed — eviction is
    /// what drops the entry and `plan_downloads` is what re-queues it — so
    /// pinning either alone leaves the re-fetch loop reachable.
    #[test]
    fn a_quiet_granule_is_downloaded_once_not_once_per_poll() {
        // A real GLM key, so the granule is dated the way production dates it.
        let key = "GLM-L2-LCFA/2026/205/12/\
                   OR_GLM-L2-LCFA_G19_s20262051200000_e20262051200200_c20262051200214.nc";
        let start = parse_filename_start_time(key).expect("fixture key must be datable");
        let listing = vec![key.to_string()];

        // Poll 1: nothing cached, so it is queued and downloaded. It parses to
        // no records — a quiet 20 s over the ocean.
        let mut cache = GlmCache::default();
        let mut tally = PollTally::default();
        assert_eq!(
            plan_downloads(&listing, &cache, &mut tally).len(),
            1,
            "an uncached granule must be downloaded once"
        );
        // `now` is nowhere near the key's own time — see
        // `wall_clock_unlike_keys`.
        cache.insert(key.to_string(), granule_start_of(key, wall_clock_unlike_keys()), Vec::new());

        // Polls 2..n, still inside the window: the listing keeps offering it and
        // the cache must keep answering "already have it" (~250 KB per miss).
        for poll in 1..=5 {
            let cutoff = start - TimeDelta::minutes(30) + TimeDelta::seconds(20 * poll);
            cache.evict_before(cutoff);
            let mut tally = PollTally::default();
            assert!(
                plan_downloads(&listing, &cache, &mut tally).is_empty(),
                "poll {poll}: a granule already downloaded and found empty was \
                 re-queued — this is the every-poll re-fetch, back"
            );
            assert_eq!(tally.in_window, 1, "it is still in the window, just not new work");
        }

        // Past the cutoff it leaves, like any other granule. It is out of the
        // listing by then too, so nothing re-queues it.
        cache.evict_before(start + TimeDelta::milliseconds(1));
        assert!(
            !cache.contains_key(key),
            "a stale empty granule must be evicted, or the cache never shrinks"
        );
    }

    /// Every granule a poll parsed reaches the cache, empty ones included:
    /// dropping them here makes `plan_downloads` re-queue them every poll.
    #[test]
    fn cache_granules_keeps_the_empty_ones_too() {
        let busy = "GLM-L2-LCFA/2026/205/12/\
                    OR_GLM-L2-LCFA_G19_s20262051200000_e20262051200200_c20262051200214.nc";
        let quiet = "GLM-L2-LCFA/2026/205/12/\
                     OR_GLM-L2-LCFA_G19_s20262051200200_e20262051200400_c20262051200414.nc";
        // Not `t0()`: see `wall_clock_unlike_keys`.
        let now = wall_clock_unlike_keys();

        let mut cache = GlmCache::default();
        cache_granules(
            &mut cache,
            vec![
                (busy.to_string(), vec![flash_at(t0())]),
                (quiet.to_string(), Vec::new()),
            ],
            now,
        );

        assert!(cache.contains_key(busy));
        assert!(
            cache.contains_key(quiet),
            "a granule that downloaded and parsed to nothing is still downloaded"
        );

        // And it is dated from its key, not from `now`.
        cache.evict_before(
            parse_filename_start_time(quiet).expect("fixture key") + TimeDelta::milliseconds(1),
        );
        assert!(!cache.contains_key(quiet), "the empty granule ages by its own start time");
    }

    /// The fallback in [`granule_start_of`] is unreachable for anything a poll
    /// listed, and must stay bounded rather than become either bug it replaced.
    #[test]
    fn granule_start_comes_from_the_key_and_falls_back_to_now() {
        // Not `t0()`: see `wall_clock_unlike_keys`.
        let now = wall_clock_unlike_keys();
        let key = "GLM-L2-LCFA/2026/205/12/\
                   OR_GLM-L2-LCFA_G19_s20262051200000_e20262051200200_c20262051200214.nc";
        assert_eq!(
            granule_start_of(key, now),
            chrono::NaiveDate::from_yo_opt(2026, 205)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            "a listed key carries its own start time; `now` must not be reached"
        );

        assert_eq!(
            granule_start_of("not-a-glm-key.nc", now),
            now,
            "an undatable granule expires one window from now — never instantly \
             (re-fetched every poll) and never not at all (unbounded cache)"
        );
    }

    /// Both window bounds are inclusive and both are load-bearing: losing the
    /// lower one shows hours-old bolts inside a retained granule, losing the
    /// upper one publishes flashes stamped after the poll's own instant.
    #[test]
    fn the_window_filter_includes_both_bounds() {
        let cutoff = t0();
        let now = cutoff + TimeDelta::minutes(5);
        let tick = TimeDelta::milliseconds(1);

        let mut cache = GlmCache::default();
        cache_granule(
            &mut cache,
            "spread.nc",
            vec![
                flash_at(cutoff - tick),
                flash_at(cutoff),
                flash_at(cutoff + TimeDelta::minutes(2)),
                flash_at(now),
                flash_at(now + tick),
            ],
        );

        let mut got: Vec<NaiveDateTime> =
            flashes_in_window(&cache, &[GlmSatellite::GoesEast], cutoff, now)
                .into_iter()
                .map(|f| f.time)
                .collect();
        got.sort();

        assert_eq!(
            got,
            vec![cutoff, cutoff + TimeDelta::minutes(2), now],
            "both ends are inclusive and nothing outside them survives"
        );
    }

    /// A flash from the other bird, for the satellite-selection tests.
    fn west_flash_at(time: NaiveDateTime) -> GlmFlash {
        GlmFlash { satellite: GlmSatellite::GoesWest, ..flash_at(time) }
    }

    /// "Both" → "East" must stop GOES-West's cached flashes rendering *now*,
    /// not once they age out of the (up to 30-minute) window — and must not
    /// cost the cache: re-selecting West restores its flashes instantly, with
    /// no re-download.
    #[test]
    fn deselecting_a_satellite_hides_its_cached_flashes_without_evicting_them() {
        let cutoff = t0();
        let now = cutoff + TimeDelta::minutes(5);
        let t = cutoff + TimeDelta::minutes(2);

        let mut cache = GlmCache::default();
        cache_granule(&mut cache, "east.nc", vec![flash_at(t)]);
        cache_granule(&mut cache, "west.nc", vec![west_flash_at(t)]);

        // Control: with both selected, both birds render.
        let both = [GlmSatellite::GoesEast, GlmSatellite::GoesWest];
        assert_eq!(flashes_in_window(&cache, &both, cutoff, now).len(), 2);

        // "Both" → "East": the in-window West flash disappears from the poll.
        let east_only = flashes_in_window(&cache, &[GlmSatellite::GoesEast], cutoff, now);
        assert!(
            east_only.iter().all(|f| f.satellite == GlmSatellite::GoesEast),
            "a deselected bird's cached flashes must not render"
        );
        assert_eq!(east_only.len(), 1, "the East flash still renders");

        // ...but the West granule was hidden, not evicted: re-selection needs
        // nothing from the network.
        assert!(
            cache.contains_key("west.nc"),
            "deselection filters the poll output; evicting here would make \
             re-selection re-download the whole window"
        );
        assert_eq!(
            flashes_in_window(&cache, &both, cutoff, now).len(),
            2,
            "re-selecting the bird restores its flashes straight from cache"
        );
    }

    /// An NTP step backwards (or a resume with a stale RTC) puts `now` behind
    /// flashes already cached. They must be hidden from the poll and nothing
    /// more — still cached, so they reappear without a re-download when the
    /// clock recovers.
    #[test]
    fn a_backwards_clock_hides_flashes_without_losing_them() {
        let window = TimeDelta::minutes(5);
        let ahead = t0() + TimeDelta::minutes(3);

        let mut cache = GlmCache::default();
        cache_granule(&mut cache, "granule.nc", vec![flash_at(t0()), flash_at(ahead)]);

        // The clock steps back: `now` lands between the two cached flashes.
        let now = t0() + TimeDelta::minutes(1);
        let cutoff = now - window;
        cache.evict_before(cutoff);

        assert!(
            cache.contains_key("granule.nc"),
            "a backwards clock must not evict data it has not caught up to yet"
        );
        let during: Vec<NaiveDateTime> =
            flashes_in_window(&cache, &[GlmSatellite::GoesEast], cutoff, now)
                .into_iter()
                .map(|f| f.time)
                .collect();
        assert_eq!(
            during,
            vec![t0()],
            "the flash stamped after `now` is withheld, not published"
        );

        // The clock catches up. Nothing had to be fetched again.
        let now = ahead + TimeDelta::minutes(1);
        let cutoff = now - window;
        cache.evict_before(cutoff);
        let mut after: Vec<NaiveDateTime> =
            flashes_in_window(&cache, &[GlmSatellite::GoesEast], cutoff, now)
                .into_iter()
                .map(|f| f.time)
                .collect();
        after.sort();
        assert_eq!(
            after,
            vec![t0(), ahead],
            "both flashes were held in the cache the whole time"
        );
    }

    fn level_failure(satellite: GlmSatellite, level: GlmDataLevel) -> LevelFailure {
        LevelFailure { satellite, level, sample_error: format!("{level:?} broke") }
    }

    /// Every bucket must land in its own field: swapping two makes every 503
    /// read as "product change?", dropping the level bucket makes a broken
    /// layer silent.
    #[test]
    fn build_outcome_binds_each_bucket_to_its_own_field() {
        let tally = PollTally { in_window: 12 };
        let acc = PollAccumulator {
            parse_errors: vec!["a.nc: GLM file has no 'flash_lat' variable".into()],
            transport_errors: vec!["b.nc: HTTP status error: 503".into()],
            level_failures: vec![level_failure(GlmSatellite::GoesWest, GlmDataLevel::Flash)],
            // A *superset* of the failures: Group was evaluated and found
            // healthy. Identical sets would let `evaluated_levels` be derived
            // from `level_failures`, giving a layer that can never clear.
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

        // Carried through untouched: not summarised into a file count.
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
    /// Hermetic: loopback port 1 (`tcpmux`) is not listening, so the connection
    /// is refused immediately.
    #[test]
    fn an_unreachable_host_is_a_transport_failure() {
        // `reqwest` is pinned to `rustls-no-provider`, so `build()` panics with
        // "No provider set" unless a crypto provider is installed first.
        // `tls::client` is not used because it sets `https_only`, and the
        // cleartext loopback URL below is the point of the test.
        rustdar_radar::tls::init();
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
    /// wording.
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

    /// The floor, asserted against literals rather than
    /// `MIN_FILES_FOR_TOTAL_VERDICT`: a loop over `1..CONST` is empty when the
    /// constant is 1 and passes vacuously.
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

    /// Losing the one optional column degrades the popup, not the whole overlay.
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
