//! Fetch GLM lightning flash data from AWS S3.
//!
//! Lists and downloads L2 LCFA NetCDF4 files from the public GOES buckets
//! declared in [`DataSources`]. Each granule covers ~20 seconds.

use std::collections::HashMap;

use chrono::{NaiveDateTime, TimeDelta};
use rustdar_source::origins::DataSources;

use super::{
    DeadFeed, FetchFailures, GLM_MIN_TIME_WINDOW_SECS, GlmDataLevel, GlmFetchOutcome, GlmFlash,
    GlmSatellite, LevelFailure, RecordDrops, WindowGap,
};
use crate::fetch_policy::{FetchError, NotFound};
use rustdar_netcdf::cf;

#[derive(Clone)]
struct CachedGranule {
    flashes: Vec<GlmFlash>,
    newest: NaiveDateTime,
}

impl CachedGranule {
    fn new(granule_start: NaiveDateTime, flashes: Vec<GlmFlash>) -> Self {
        let newest = flashes
            .iter()
            .map(|f| f.time)
            .max()
            .unwrap_or(granule_start);
        CachedGranule { flashes, newest }
    }
}

#[derive(Default, Clone)]
pub struct GlmCache {
    entries: HashMap<String, CachedGranule>,
}

/// Ceiling on flashes retained across a depicted span, enforced by
/// [`GlmCache::evict_oldest_over`] and **only under a span posture** — a live
/// pane's window is bounded by [`super::GLM_MAX_TIME_WINDOW_SECS`] alone, as
/// it always was.
///
/// The denominator: `250_000 × size_of::<GlmFlash>()`, measured by
/// `a_spanned_poll_caps_what_it_retains_and_drops_its_oldest_hours_first` as
/// **12 000 000 bytes at 48 bytes a row** — the whole cost, since `GlmFlash`
/// owns nothing on the heap. Every raster job ships at most this many rows.
///
/// **How much *time* 250 000 rows covers is not measured here**, and it is a
/// function of the level and the weather rather than of this constant. The one
/// figure the tree carries is [`RecordDrops`]'s: 1584507 records over 105
/// granules with all three levels on, ~15k rows per 20 s granule, at which the
/// cap holds ~16 granules. The shipped default is groups and flashes with
/// events off ([`GlmDataLevel`]), for which no per-granule count has been
/// measured — do not infer one from the all-levels figure. What *is*
/// guaranteed is that eviction is oldest-first, so a loop that overflows the
/// cap keeps its **newest** hours lit rather than its oldest.
pub const MAX_RETAINED_FLASHES: usize = 250_000;

impl GlmCache {
    pub fn evict_before(&mut self, cutoff: NaiveDateTime) {
        self.entries
            .retain(|_key, granule| granule.newest >= cutoff);
    }

    pub fn flash_count(&self) -> usize {
        self.entries.values().map(|g| g.flashes.len()).sum()
    }

    /// Drop whole granules, oldest first, until at most `cap` flashes remain
    /// — the byte bound on span retention (see [`MAX_RETAINED_FLASHES`]).
    /// Whole granules so [`Self::contains_key`] stays the download planner's
    /// truth: a half-kept granule would be "cached" and never refetched.
    pub fn evict_oldest_over(&mut self, cap: usize) {
        let mut total = self.flash_count();
        if total <= cap {
            return;
        }
        let mut by_age: Vec<(NaiveDateTime, String)> = self
            .entries
            .iter()
            .map(|(key, granule)| (granule.newest, key.clone()))
            .collect();
        by_age.sort();
        for (_, key) in by_age {
            if total <= cap {
                break;
            }
            if let Some(granule) = self.entries.remove(&key) {
                total -= granule.flashes.len();
            }
        }
    }

    pub fn all_flashes(&self) -> impl Iterator<Item = &GlmFlash> {
        self.entries.values().flat_map(|g| &g.flashes)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn insert(&mut self, key: String, granule_start: NaiveDateTime, flashes: Vec<GlmFlash>) {
        self.entries
            .insert(key, CachedGranule::new(granule_start, flashes));
    }
}

fn cache_granules(
    cache: &mut GlmCache,
    entries: Vec<(String, Vec<GlmFlash>)>,
    as_of: NaiveDateTime,
) {
    for (key, flashes) in entries {
        let granule_start = granule_start_of(&key, as_of);
        cache.insert(key, granule_start, flashes);
    }
}

/// The instant a granule is aged against, from the S3 key it was listed under;
/// the depicted instant on the fallback, so an undatable granule expires one
/// window out of the picture that asked for it.
fn granule_start_of(key: &str, as_of: NaiveDateTime) -> NaiveDateTime {
    parse_filename_start_time(key).unwrap_or(as_of)
}

/// **What the pane depicts**, as one argument: the instant a poll sampled, and
/// how far the pane's clock can sweep from it before the next poll.
///
/// One struct rather than two parameters because they are never meaningful
/// apart — a span with no instant names nothing — and because the pair is what
/// a *loop* is: `as_of` moves every tick and `span_secs` does not.
#[derive(Clone, Copy, Debug)]
pub struct DepictedWindow {
    /// The instant the picture is of. On a live pane this is the wall clock.
    pub as_of: NaiveDateTime,
    /// [`rustdar_source::handler::FetchConfig::depicted_span_secs`] — `None` on
    /// a live pane, where the pane depicts one moving instant and nothing else.
    pub span_secs: Option<u64>,
}

pub async fn fetch_glm_flashes(
    client: &reqwest::Client,
    sources: &DataSources,
    satellites: &[GlmSatellite],
    time_window_secs: f64,
    levels: &[GlmDataLevel],
    cache: &mut GlmCache,
    depicted: DepictedWindow,
) -> Result<GlmFetchOutcome, FetchError> {
    let DepictedWindow {
        as_of,
        span_secs: depicted_span_secs,
    } = depicted;
    // The zero-object warning below assumes the queried range is wide enough to
    // always cover an already-published granule.
    debug_assert!(
        time_window_secs >= GLM_MIN_TIME_WINDOW_SECS,
        "GLM time window {time_window_secs}s is below GLM_MIN_TIME_WINDOW_SECS \
         ({GLM_MIN_TIME_WINDOW_SECS}s); a window under S3 publish latency makes \
         the zero-object check report a live feed as dead.",
    );

    // **The depicted instant, never the wall clock.** `list_glm_files` is
    // addressed by `{year}/{doy}/{hour}`, so a range anchored on `as_of` asks
    // the archive for the hours the picture is of; on a live pane `as_of` *is*
    // the wall clock, the span is zero, and the request is the one it always
    // was.
    let window = TimeDelta::milliseconds((time_window_secs * 1000.0) as i64);
    // **The pane's whole depicted span, not the poll's sampled instant.**
    // Under a loop (or a parked scrub) `as_of` is one sample of a clock that
    // sweeps `span` between polls, so the listing reaches back over the span
    // the pane can depict and eviction is anchored on the span's own edge —
    // anchoring both on the sample is what lit a two-hour loop on a single
    // frame, each poll evicting every other frame's granules. `None` is a
    // live pane: span zero, and every quantity below is byte-for-byte what
    // it always was.
    let span = TimeDelta::seconds(depicted_span_secs.unwrap_or(0) as i64);
    let start = as_of - window - span;
    let cutoff = start;
    // Frames *ahead* of the sampled instant are still inside the loop: the
    // listing has to reach them, granules a previous poll fetched for them
    // must survive this one, and their flashes must stay in the returned set.
    // Bounded by the span — the loop's newest frame is at most `span` past any
    // instant inside it. Zero on a live pane, where the future is empty
    // anyway.
    //
    // **The listing's upper bound, not only retention's.** A listing that
    // stopped at the sample downloaded nothing ahead of it, so a poll landing
    // while the playhead sat on the loop's OLDEST frame covered
    // `[oldest - window - span, oldest]` — entirely *behind* the loop — and
    // exactly one frame of the sweep had strikes. Retention alone cannot fix
    // that: there is nothing held to retain.
    let horizon = as_of + span;

    // Anchored on the depicted span for the same reason: an instant-anchored
    // cutoff evicts exactly the granules the pane's other frames display.
    cache.evict_before(cutoff);

    let mut acc = PollAccumulator::default();
    let mut dead_feeds = Vec::new();
    let mut window_gaps = Vec::new();
    let mut tally = PollTally::default();

    let mut listing_failures: Vec<(GlmSatellite, FetchError)> = Vec::new();
    let mut queried = Vec::new();

    for &sat in satellites {
        let bucket = sat.bucket(sources);
        let listing = match list_glm_files(client, sources, bucket, start, horizon).await {
            Ok(listing) => listing,
            Err(e) => {
                log::warn!("GLM: {} listing failed: {e}", sat.display_name());
                listing_failures.push((sat, e));
                continue;
            }
        };
        queried.push(sat);

        // Zero objects means the feed is gone (dead bucket, renamed path,
        // satellite rotated out of the slot), not a quiet sky. Objects present
        // with no in-window *keys* is a third case; `else if` because zero
        // objects can only produce zero keys.
        if listing.objects_seen == 0 {
            dead_feeds.push(DeadFeed {
                satellite: sat,
                bucket: bucket.to_string(),
                prefixes: listing.prefixes.clone(),
            });
        } else if listing.keys.is_empty() {
            window_gaps.push(WindowGap {
                satellite: sat,
                objects_seen: listing.objects_seen,
            });
        }

        let new_keys = plan_downloads(&listing.keys, cache, &mut tally);

        if new_keys.is_empty() {
            continue;
        }
        log::info!(
            "Downloading {} new GLM files from {}",
            new_keys.len(),
            sat.display_name()
        );

        let batch = download_and_parse_batch(client, sources, sat, bucket, &new_keys, levels).await;
        acc.absorb(sat, levels, batch);
    }

    if queried.is_empty() && !satellites.is_empty() {
        let verdicts: Vec<FetchError> = listing_failures
            .iter()
            .map(|(_, e)| e.clone())
            .collect::<Vec<_>>();
        return Err(FetchError::of_round(
            &verdicts,
            format!(
                "no GLM satellite could be listed ({} failed)",
                verdicts.len()
            ),
        ));
    }

    cache_granules(cache, std::mem::take(&mut acc.entries), as_of);

    // The byte bound on span retention, **only under a span posture**: a live
    // pane's cache is bounded by its window exactly as it always was, while a
    // span could otherwise hold a day of storm at Event level. Oldest first,
    // so an overflowing loop keeps its newest hours lit.
    if !span.is_zero() {
        cache.evict_oldest_over(MAX_RETAINED_FLASHES);
    }

    // Still keyed on `satellites`, not `queried`: a satellite whose listing
    // failed still has earlier granules in window. The upper bound is the
    // span's `horizon`, not the sampled instant: the raster culls per depicted
    // frame, so returning the whole retained span is what lets every frame of
    // a loop draw its own window from one delivery.
    let filtered = flashes_in_window(cache, satellites, cutoff, horizon);

    log::info!(
        "GLM: {} flashes in {:.0}s window",
        filtered.len(),
        time_window_secs
    );

    Ok(build_outcome(
        filtered,
        dead_feeds,
        window_gaps,
        queried,
        listing_failures,
        &tally,
        acc,
    ))
}

/// Select the cached flashes inside this poll's retention window, from the
/// satellites it was asked for — the per-flash half of a two-stage narrowing
/// ([`GlmCache::evict_before`] does the per-granule half). Both bounds are
/// inclusive: the bounds are sampled once per poll and a granule's last
/// flashes can be stamped after its start.
///
/// `horizon` is `as_of` itself on a live pane; under a depicted span it is
/// `as_of + span`, so frames *ahead* of the sampled instant keep their
/// flashes. **Culling to each frame's own window is the rasterizer's job**
/// (`rasterize_glm_strikes` drops a flash later than its depicted instant, and
/// one older than the fade window) — this set is what any frame of the span
/// may draw from, not what one frame shows.
fn flashes_in_window(
    cache: &GlmCache,
    satellites: &[GlmSatellite],
    cutoff: NaiveDateTime,
    horizon: NaiveDateTime,
) -> Vec<GlmFlash> {
    cache
        .all_flashes()
        .filter(|f| satellites.contains(&f.satellite) && f.time >= cutoff && f.time <= horizon)
        .cloned()
        .collect()
}

fn build_outcome(
    flashes: Vec<GlmFlash>,
    dead_feeds: Vec<DeadFeed>,
    window_gaps: Vec<WindowGap>,
    queried: Vec<GlmSatellite>,
    listing_failures: Vec<(GlmSatellite, FetchError)>,
    tally: &PollTally,
    acc: PollAccumulator,
) -> GlmFetchOutcome {
    GlmFetchOutcome {
        flashes,
        dead_feeds,
        window_gaps,
        record_drops: acc.drops,
        queried,
        listing_failures,
        parse_failures: summarize_failures(tally.in_window, acc.parse_errors),
        transport_failures: summarize_failures(tally.in_window, acc.transport_errors),
        // Not routed through `summarize_failures`: a level failure has no
        // file-count denominator.
        level_failures: acc.level_failures,
        evaluated_levels: acc.evaluated_levels,
    }
}

#[derive(Default)]
struct PollAccumulator {
    entries: Vec<(String, Vec<GlmFlash>)>,
    parse_errors: Vec<String>,
    transport_errors: Vec<String>,
    level_failures: Vec<LevelFailure>,
    /// (satellite, level) pairs this poll gathered evidence about: a poll that
    /// downloads nothing new learns nothing.
    evaluated_levels: Vec<(GlmSatellite, GlmDataLevel)>,
    /// Summed across both satellites: the drop counts share one denominator.
    drops: RecordDrops,
}

impl PollAccumulator {
    fn absorb(&mut self, satellite: GlmSatellite, levels: &[GlmDataLevel], batch: BatchOutcome) {
        self.parse_errors.extend(batch.parse_errors);
        self.transport_errors.extend(batch.transport_errors);
        self.level_failures.extend(batch.level_failures);
        self.drops.absorb(batch.drops);

        if !batch.entries.is_empty() {
            for &level in levels {
                self.evaluated_levels.push((satellite, level));
            }
        }
        self.entries.extend(batch.entries);
    }
}

#[derive(Default)]
struct PollTally {
    in_window: usize,
}

/// The tally counts every listed key, not the returned ones: a download-count
/// denominator makes one persistent failure look like a total outage.
fn plan_downloads<'a>(keys: &'a [String], cache: &GlmCache, tally: &mut PollTally) -> Vec<&'a str> {
    tally.in_window += keys.len();
    keys.iter()
        .filter(|k| !cache.contains_key(k.as_str()))
        .map(|k| k.as_str())
        .collect()
}

struct GlmListing {
    keys: Vec<String>,
    objects_seen: usize,
    prefixes: Vec<String>,
}

/// S3 path: `GLM-L2-LCFA/{year}/{day_of_year}/{hour}/`
/// Files: `OR_GLM-L2-LCFA_G{sat}_s{start}_e{end}_c{creation}.nc`
async fn list_glm_files(
    client: &reqwest::Client,
    sources: &DataSources,
    bucket: &str,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<GlmListing, FetchError> {
    let mut all_keys = Vec::new();
    let mut objects_seen = 0usize;

    // A *single* prefix when `start` and `end` share a UTC hour, which requires
    // `GLM_MIN_TIME_WINDOW_SECS` > S3 publish latency for the hour's first
    // object: that granule lands 27–30 s after the boundary (measured on
    // noaa-goes19), so a 60 s minimum leaves ~30 s of headroom.
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
        t += TimeDelta::hours(1);
        if t > end {
            t = end;
        }
    }

    for prefix in &prefixes {
        let mut continuation_token: Option<String> = None;
        loop {
            let mut url = format!(
                "{}/?list-type=2&prefix={prefix}",
                sources.s3_bucket_url(bucket),
            );
            if let Some(ref token) = continuation_token {
                url.push_str("&continuation-token=");
                url.push_str(&urlencoded(token));
            }

            let resp = client.get(&url).send().await.map_err(|e| {
                FetchError::from_transport(&e, format!("S3 list request failed: {e}"))
            })?;

            if !resp.status().is_success() {
                // `IsBroken`: a bucket listing is not published on a schedule.
                // A 404 on `?list-type=2` means the bucket is gone or renamed.
                return Err(FetchError::from_status(
                    resp.status(),
                    NotFound::IsBroken,
                    format!("S3 returned HTTP {}", resp.status()),
                ));
            }

            let body = resp.text().await.map_err(|e| {
                FetchError::from_transport(&e, format!("Failed to read S3 list response: {e}"))
            })?;

            let doc = roxmltree::Document::parse(&body)
                .map_err(|e| FetchError::transient(format!("Failed to parse S3 XML: {e}")))?;

            for node in doc.descendants() {
                if node.tag_name().name() == "Key"
                    && let Some(key) = node.text()
                {
                    objects_seen += 1;
                    if key.ends_with(".nc")
                        && let Some(file_start) = parse_filename_start_time(key)
                        && file_start >= start
                        && file_start <= end
                    {
                        all_keys.push(key.to_string());
                    }
                }
            }

            let is_truncated = doc
                .descendants()
                .find(|n| n.tag_name().name() == "IsTruncated")
                .and_then(|n| n.text())
                .is_some_and(|t| t == "true");

            if !is_truncated {
                break;
            }

            // A truncated response with no usable continuation token would
            // re-issue the identical first-page request forever.
            let next = doc
                .descendants()
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

/// Parse the start timestamp from a GLM filename: the `s` field of
/// `OR_GLM-L2-LCFA_G19_s20261120145200_e...nc` is `YYYYDDDHHMMSSf`, DDD = day of
/// year, f = tenths of a second.
fn parse_filename_start_time(key: &str) -> Option<NaiveDateTime> {
    let filename = key.rsplit('/').next()?;
    let s_idx = filename.find("_s")?;
    let s_field = &filename[s_idx + 2..];
    // `get`, not `[..14]`: a multi-byte character in the `_s` field would put
    // a range boundary inside a UTF-8 sequence and panic.
    let digits = s_field.get(..14)?;
    let year: i32 = digits.get(0..4)?.parse().ok()?;
    let doy: u32 = digits.get(4..7)?.parse().ok()?;
    let hour: u32 = digits.get(7..9)?.parse().ok()?;
    let min: u32 = digits.get(9..11)?.parse().ok()?;
    let sec: u32 = digits.get(11..13)?.parse().ok()?;

    let date = chrono::NaiveDate::from_yo_opt(year, doy)?;
    let time = chrono::NaiveTime::from_hms_opt(hour, min, sec)?;
    Some(NaiveDateTime::new(date, time))
}

struct BatchOutcome {
    entries: Vec<(String, Vec<GlmFlash>)>,
    /// One message per file that downloaded but would not parse.
    parse_errors: Vec<String>,
    /// One message per file that never arrived, tracked separately so a network
    /// problem is never reported as a product schema change.
    transport_errors: Vec<String>,
    level_failures: Vec<LevelFailure>,
    /// Summed over every granule that parsed, **not** deduplicated.
    drops: RecordDrops,
}

async fn download_and_parse_batch(
    client: &reqwest::Client,
    sources: &DataSources,
    satellite: GlmSatellite,
    bucket: &str,
    keys: &[&str],
    levels: &[GlmDataLevel],
) -> BatchOutcome {
    use futures::stream::StreamExt;

    let levels_owned: Vec<GlmDataLevel> = levels.to_vec();
    let futs: Vec<_> = keys
        .iter()
        .map(|&key| {
            let client = client.clone();
            let url = sources.s3_object_url(bucket, key);
            let key_owned = key.to_string();
            let lvls = levels_owned.clone();
            async move {
                match download_and_parse_one(&client, &url, satellite, &lvls).await {
                    Ok(parsed) => Ok((key_owned, parsed)),
                    Err(e) => {
                        // Debug, not warn: with 20 files in flight one schema
                        // change would produce a wall of identical lines.
                        log::debug!("Failed to fetch GLM file {key_owned}: {}", e.message());
                        let labelled = format!("{key_owned}: {}", e.message());
                        Err(match e {
                            FileError::Parse(_) => FileError::Parse(labelled),
                            FileError::Transport(_) => FileError::Transport(labelled),
                        })
                    }
                }
            }
        })
        .collect();

    let results: Vec<Result<(String, GranuleParse), FileError>> = futures::stream::iter(futs)
        .buffer_unordered(20)
        .collect()
        .await;

    BatchOutcome::from_results(results)
}

impl BatchOutcome {
    fn from_results(results: Vec<Result<(String, GranuleParse), FileError>>) -> Self {
        let mut outcome = BatchOutcome {
            entries: Vec::new(),
            parse_errors: Vec::new(),
            transport_errors: Vec::new(),
            level_failures: Vec::new(),
            drops: RecordDrops::default(),
        };
        for result in results {
            match result {
                Ok((key, parsed)) => {
                    outcome.drops.absorb(parsed.drops);
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

/// Why one file did not contribute: a file that arrives and will not parse
/// indicts the product, one that never arrives indicts the network. A captive
/// portal answering 200 with an HTML page is reported as `Parse`.
#[derive(Debug)]
enum FileError {
    Transport(String),
    Parse(String),
}

impl FileError {
    fn message(&self) -> &str {
        match self {
            FileError::Transport(e) | FileError::Parse(e) => e,
        }
    }
}

fn summarize_failures(in_window: usize, errors: Vec<String>) -> Option<FetchFailures> {
    let sample_error = errors.first()?.clone();
    Some(FetchFailures {
        in_window,
        failed: errors.len(),
        sample_error,
    })
}

async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    client
        .get(url)
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

async fn download_and_parse_one(
    client: &reqwest::Client,
    url: &str,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, FileError> {
    let bytes = download_bytes(client, url)
        .await
        .map_err(FileError::Transport)?;
    parse_downloaded_file(&bytes, satellite, levels)
}

fn parse_downloaded_file(
    bytes: &[u8],
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, FileError> {
    parse_glm_netcdf(bytes, satellite, levels).map_err(FileError::Parse)
}

struct LevelVars {
    lat: &'static str,
    lon: &'static str,
    energy: &'static str,
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

pub(crate) trait VarSource {
    fn read_unpacked(&self, name: &str) -> Result<Option<cf::UnpackedVar>, String>;
    fn time_coverage_start(&self) -> Option<String>;
}

impl VarSource for rustdar_netcdf::Granule {
    fn read_unpacked(&self, name: &str) -> Result<Option<cf::UnpackedVar>, String> {
        rustdar_netcdf::Granule::read_unpacked(self, name)
    }
    fn time_coverage_start(&self) -> Option<String> {
        self.global_str("time_coverage_start")
    }
}

pub(crate) fn parse_glm_netcdf(
    data: &[u8],
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, String> {
    let file = rustdar_netcdf::Granule::open(data)?;
    parse_with_source(&file, satellite, levels)
}

fn parse_with_source<S: VarSource>(
    file: &S,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, String> {
    // Fallback epoch; the per-variable epoch named in `units` wins where
    // present — see `parse_level_records`.
    let time_origin = file
        .time_coverage_start()
        .as_deref()
        .and_then(cf::parse_cf_epoch)
        .ok_or_else(|| "Missing or invalid time_coverage_start attribute".to_string())?;

    let mut all_records = Vec::new();
    let mut failures: Vec<LevelFailure> = Vec::new();
    let mut drops = RecordDrops::default();

    for level in levels {
        let vars = match level {
            GlmDataLevel::Flash => &FLASH_VARS,
            GlmDataLevel::Group => &GROUP_VARS,
            GlmDataLevel::Event => &EVENT_VARS,
        };
        // One level failing must not take the others with it.
        match parse_level_records(file, vars, &time_origin, satellite) {
            Ok((records, level_drops)) => {
                all_records.extend(records);
                // Only levels that *parsed* contribute a denominator.
                drops.absorb(level_drops);
            }
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
    // reported as a failed *file*.
    if !failures.is_empty() && failures.len() == levels.len() {
        return Err(failures.swap_remove(0).sample_error);
    }

    // A *partial* failure keeps the healthy levels and reports the broken one:
    // `Err` would discard good group records, and a bare `Ok` reads as
    // "everything is fine" while the layer sits empty.
    Ok(GranuleParse {
        records: all_records,
        level_failures: failures,
        drops,
    })
}

#[derive(Debug)]
pub(crate) struct GranuleParse {
    pub records: Vec<GlmFlash>,
    pub level_failures: Vec<LevelFailure>,
    pub drops: RecordDrops,
}

/// Unit spellings accepted for `*_area`, mapped to the multiplier into km².
/// The L2 LCFA product declares `flash_area:units = "m2"`: raw count 1826 is
/// 1826 × 152601.9 m² = 278.7 km², not "1826.0 km²".
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

/// Parse records for one GLM hierarchy level. Every variable goes through CF
/// unpacking (see [`super::cf`]): most are `_Unsigned` packed shorts and reading
/// them raw yields meaningless numbers.
fn parse_level_records<S: VarSource>(
    file: &S,
    vars: &LevelVars,
    time_origin: &chrono::NaiveDateTime,
    satellite: GlmSatellite,
) -> Result<(Vec<GlmFlash>, RecordDrops), String> {
    // Required *columns*: absence is a schema change and fails the level. An
    // absent *value* arrives quietly as `None` inside `UnpackedVar::values`.
    let lats = read_required_unpacked(file, vars.lat)?;
    let lons = read_required_unpacked(file, vars.lon)?;
    let energies = read_required_unpacked(file, vars.energy)?;
    let times = read_required_unpacked(file, vars.time_offset)?;

    // Every variable at a level shares one dimension, so a short column means a
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

    // Area is the one optional column: events have no area variable.
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
        None => cf::TimeUnits {
            seconds_per_unit: 1.0,
            epoch: *time_origin,
        },
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

    // Unit resolution is scoped to the field it describes: `None` must not take
    // position, time or the other hierarchy levels down with it.
    let energy_to_j = unit_multiplier(satellite, vars.energy, Some(&energies), ENERGY_UNITS, "J");
    let area_to_km2 = unit_multiplier(
        satellite,
        vars.area.unwrap_or("area"),
        areas.as_ref(),
        AREA_UNITS,
        "km2",
    );

    let mut records = Vec::with_capacity(count);
    let mut drops = RecordDrops {
        considered: count,
        ..RecordDrops::default()
    };

    for i in 0..count {
        // A `_FillValue` in any field that places a strike in space and time
        // makes the detection unusable; drop it rather than fabricate a number.
        let (Some(lat), Some(lon), Some(offset)) =
            (lats.values[i], lons.values[i], times.values[i])
        else {
            drops.fill_values += 1;
            continue;
        };

        let lon = normalize_longitude(lon);

        // Backstop against a coordinate that unpacked to nonsense. Effectively
        // guards latitude only: the wrap above can carry a mis-unpacked
        // longitude back into the valid interval.
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            drops.off_globe += 1;
            continue;
        }

        // Energy and area are descriptive, not locating: an unreported value
        // leaves the field `None`. Never zero — `0f32.log10()` is -inf and
        // `rasterize` draws unknown as the smallest possible bolt.
        // `_FillValue = -1s` means a value can be absent in a present column.
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

    if drops.dropped() > 0 {
        log::warn!(
            "GLM {} {}: dropped {} record(s) with fill values and {} with \
             out-of-range coordinates (of {count})",
            satellite.display_name(),
            vars.level.display_name(),
            drops.fill_values,
            drops.off_globe,
        );
    }

    Ok((records, drops))
}

fn warn_missing_variable_once(name: &'static str) {
    warn_once(
        missing_variable_key(name),
        &format!(
            "GLM: variable '{name}' is absent from the L2 LCFA file - the product \
             schema has changed and this field can no longer be read"
        ),
    );
}

/// A variable absent from the file is a schema change: warned once and failed
/// here. An individual `_FillValue` (or out-of-`valid_range`) value is a
/// per-record condition, carried as a `None` in [`cf::UnpackedVar::values`].
fn read_required_unpacked<S: VarSource>(
    file: &S,
    name: &'static str,
) -> Result<cf::UnpackedVar, String> {
    match file.read_unpacked(name)? {
        Some(var) => Ok(var),
        None => {
            warn_missing_variable_once(name);
            Err(format!(
                "GLM file has no '{name}' variable (product schema change?)"
            ))
        }
    }
}

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
/// 172.72°E is stored as -187.28. One wrap is enough for any offset the product
/// uses; anything further out is left alone for the range check downstream.
pub(super) fn normalize_longitude(lon: f64) -> f64 {
    if (-180.0..=180.0).contains(&lon) || lon.abs() > 540.0 {
        lon
    } else if lon < 0.0 {
        lon + 360.0
    } else {
        lon - 360.0
    }
}

fn column_value(column: Option<&cf::UnpackedVar>, i: usize) -> Option<f64> {
    column?.values.get(i).copied().flatten()
}

/// Resolve the multiplier converting a variable's declared `units` into the unit
/// rustdar stores, or `None` if that cannot be done.
///
/// A value is reported only when the file says what unit it is in and we can
/// convert it: assuming an absent `units` attribute is canonical would report
/// `flash_area`, shipped as `m2`, a million times too large.
fn unit_multiplier(
    satellite: GlmSatellite,
    name: &str,
    column: Option<&cf::UnpackedVar>,
    table: &[(&str, f64)],
    canonical: &str,
) -> Option<f64> {
    // No column at all is a product property, not an anomaly: there is no
    // `event_area`.
    let column = column?;
    let sat = satellite.display_name();

    let Some(units) = column.units.as_deref() else {
        warn_once(
            units_key(satellite, name, "absent"),
            &format!(
                "GLM {sat}: {name} declares no units attribute; reporting the field as \
             unknown rather than assuming {canonical}"
            ),
        );
        return None;
    };

    let key = units.trim().to_ascii_lowercase();
    let found = table
        .iter()
        .find(|(spelling, _)| *spelling == key)
        .map(|(_, multiplier)| *multiplier);

    if found.is_none() {
        warn_once(
            units_key(satellite, name, &key),
            &format!(
                "GLM {sat}: {name} declares units {units:?}, which rustdar cannot convert \
             to {canonical}; reporting the field as unknown. This is an upstream \
             product change - the conversion table in `glm::fetch` needs the new \
             spelling."
            ),
        );
    }
    found
}

fn slot_key(satellite: GlmSatellite) -> &'static str {
    match satellite {
        GlmSatellite::GoesEast => "goes-east",
        GlmSatellite::GoesWest => "goes-west",
    }
}

pub(super) fn level_parse_key(satellite: GlmSatellite, lat_var: &str) -> String {
    format!("{}:level-parse:{lat_var}", slot_key(satellite))
}

/// Dedup key for "a variable the product used to have is gone". *Not*
/// satellite-qualified: the variable set is a property of the product schema.
pub(super) fn missing_variable_key(name: &str) -> String {
    format!("variable-absent:{name}")
}

/// Dedup key for "this variable declares a unit we cannot convert", keyed on
/// the satellite *and* the offending spelling.
pub(super) fn units_key(satellite: GlmSatellite, name: &str, spelling: &str) -> String {
    format!("{}:{name}:units:{spelling}", slot_key(satellite))
}

pub(crate) fn warn_once(key: String, message: &str) {
    if claim_warning(key) {
        log::warn!("{message}");
    }
}

pub(crate) fn claim_warning(key: String) -> bool {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = seen.lock().unwrap_or_else(|e| e.into_inner());
    guard.insert(key)
}

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
mod tests;
