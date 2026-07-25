//! Fetch GLM lightning flash data from AWS S3.
//!
//! Lists and downloads NetCDF4 files from the `noaa-goes19`/`noaa-goes18`
//! public S3 buckets. Files are LCFA (Lightning Cluster-Filter Algorithm)
//! Level 2 products, each covering ~20 seconds.

use std::collections::HashMap;

use chrono::{NaiveDateTime, TimeDelta, Utc};

use super::{
    DeadFeed, FetchFailures, GLM_MIN_TIME_WINDOW_SECS, GlmDataLevel, GlmFetchOutcome, GlmFlash,
    GlmSatellite,
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
    let mut new_entries: Vec<(String, Vec<GlmFlash>)> = Vec::new();
    let mut dead_feeds = Vec::new();
    let mut parse_errors: Vec<String> = Vec::new();
    let mut transport_errors: Vec<String> = Vec::new();
    // Denominator for the failure ratio: every file the listing puts inside the
    // window, counted whether or not this poll had to download it. Counting
    // only the uncached ones biases the ratio upward over time, because
    // successes get cached and drop out while failures are retried forever.
    let mut in_window = 0usize;

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

        let plan = plan_downloads(&listing.keys, cache);
        in_window += plan.in_window;
        let new_keys = plan.new_keys;

        if new_keys.is_empty() {
            continue;
        }
        log::info!("Downloading {} new GLM files from {}", new_keys.len(), sat.display_name());

        // Download in concurrent batches of 20
        let batch = download_and_parse_batch(client, sat, &new_keys, levels).await;
        parse_errors.extend(batch.parse_errors);
        transport_errors.extend(batch.transport_errors);
        for (key, flashes) in batch.entries {
            new_entries.push((key, flashes));
        }
    }

    // Insert new data into cache
    for (key, flashes) in new_entries {
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
    Ok(GlmFetchOutcome {
        flashes: filtered,
        dead_feeds,
        queried: satellites.to_vec(),
        parse_failures: summarize_failures(in_window, parse_errors),
        transport_failures: summarize_failures(in_window, transport_errors),
    })
}

/// What one satellite's listing means for this poll.
struct PollPlan<'a> {
    /// Every key the listing placed in the window — the failure-ratio
    /// denominator.
    in_window: usize,
    /// The subset that still has to be downloaded.
    new_keys: Vec<&'a str>,
}

/// Decide what to download, and what the window contains.
///
/// The two numbers are deliberately different and must stay that way: cached
/// successes drop out of `new_keys` while failures are never cached and are
/// retried every poll, so using `new_keys.len()` as the denominator makes a
/// single persistent failure look like a total outage after a few ticks. Pure,
/// so that distinction is testable without touching the network.
fn plan_downloads<'a>(keys: &'a [String], cache: &GlmCache) -> PollPlan<'a> {
    PollPlan {
        in_window: keys.len(),
        new_keys: keys
            .iter()
            .filter(|k| !cache.contains_key(k.as_str()))
            .map(|k| k.as_str())
            .collect(),
    }
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
                Ok(flashes) => Ok((key_owned, flashes)),
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

    let results: Vec<Result<(String, Vec<GlmFlash>), FileError>> = futures::stream::iter(futs)
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
    fn from_results(results: Vec<Result<(String, Vec<GlmFlash>), FileError>>) -> Self {
        let mut outcome = BatchOutcome {
            entries: Vec::new(),
            parse_errors: Vec::new(),
            transport_errors: Vec::new(),
        };
        for result in results {
            match result {
                Ok(entry) => outcome.entries.push(entry),
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
) -> Result<Vec<GlmFlash>, FileError> {
    let bytes = download_bytes(client, url).await.map_err(FileError::Transport)?;
    parse_glm_netcdf(&bytes, satellite, levels).map_err(FileError::Parse)
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
fn parse_glm_netcdf(data: &[u8], satellite: GlmSatellite, levels: &[GlmDataLevel]) -> Result<Vec<GlmFlash>, String> {
    let file = netcdf::open_mem(None, data)
        .map_err(|e| format!("Failed to open NetCDF: {e}"))?;

    // Read the time origin from global attribute
    let time_origin = file.attribute("time_coverage_start")
        .and_then(|a| a.value().ok())
        .and_then(|v| match v {
            netcdf::AttributeValue::Str(s) => Some(s),
            _ => None,
        })
        .and_then(|s| {
            chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.fZ").ok()
        })
        .ok_or_else(|| "Missing or invalid time_coverage_start attribute".to_string())?;

    let mut all_records = Vec::new();

    for level in levels {
        let vars = match level {
            GlmDataLevel::Flash => &FLASH_VARS,
            GlmDataLevel::Group => &GROUP_VARS,
            GlmDataLevel::Event => &EVENT_VARS,
        };
        let records = parse_level_records(&file, vars, &time_origin, satellite)?;
        all_records.extend(records);
    }

    Ok(all_records)
}

/// Parse records for one GLM hierarchy level from the NetCDF file.
fn parse_level_records(
    file: &netcdf::File,
    vars: &LevelVars,
    time_origin: &chrono::NaiveDateTime,
    satellite: GlmSatellite,
) -> Result<Vec<GlmFlash>, String> {
    let lats = read_required_f32(file, vars.lat)?;
    let lons = read_required_f32(file, vars.lon)?;
    let energies = read_required_f32(file, vars.energy)?;
    let time_offsets = read_required_f64_or_f32(file, vars.time_offset)?;

    // Every variable at a level shares one dimension (`number_of_flashes`,
    // `number_of_groups`, `number_of_events`), so a short column is never
    // legitimate — it means a corrupt or restructured file. Reject it instead
    // of padding the tail with zeros.
    let count = lats.len();
    for (name, len) in [
        (vars.lon, lons.len()),
        (vars.energy, energies.len()),
        (vars.time_offset, time_offsets.len()),
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
    //
    // Note this reasons about *column presence* only. Once CF unpacking lands,
    // individual values can also be absent even where the column exists:
    // `flash_area`/`group_area` carry `_FillValue = -1s` and
    // `valid_range = 0s, -6s`. `Option<f32>` already accommodates per-value
    // absence — `fix/glm-cf-unpacking` should map fill values to `None` here
    // rather than letting them through as a number.
    let areas = match vars.area {
        Some(name) => match read_optional_f32(file, name)? {
            Some(values) if values.len() == count => Some(values),
            Some(values) => {
                log::warn!(
                    "GLM: '{name}' has {} values but '{}' has {count}; omitting area",
                    values.len(),
                    vars.lat,
                );
                None
            }
            None => None,
        },
        None => None,
    };

    let mut records = Vec::with_capacity(count);
    for i in 0..count {
        let millis = (time_offsets[i] * 1000.0) as i64;
        let time = *time_origin + TimeDelta::milliseconds(millis);

        records.push(GlmFlash {
            lat: lats[i] as f64,
            lon: lons[i] as f64,
            energy: energies[i],
            area: areas.as_ref().map(|a| a[i]),
            time,
            satellite,
            level: vars.level,
        });
    }

    Ok(records)
}

/// Report a variable the product was expected to have but does not, once per
/// variable per process.
///
/// A variable vanishing from the product is a permanent schema change, not a
/// transient condition, so repeating the message on every 20-second poll would
/// bury it in exactly the way that let the original bug survive a year.
fn warn_missing_variable_once(name: &'static str) {
    static WARNED: std::sync::Mutex<std::collections::BTreeSet<&'static str>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());

    let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if warned.insert(name) {
        log::warn!(
            "GLM: variable '{name}' is absent from the L2 LCFA file — the product \
             schema has changed and this field can no longer be read"
        );
    }
}

/// Read a 1-D f32 variable that the level cannot do without.
///
/// Absence is an error, deliberately. The previous `Ok(Vec::new())` fallback is
/// the mechanism that produced "Area: 0.0 km²" for a year: a variable that does
/// not exist read back as an empty column, and callers padded it with zeros. The
/// same silence would turn a renamed `flash_energy` into every bolt drawing at
/// minimum size, or a renamed `flash_lat` into "no lightning anywhere".
fn read_required_f32(file: &netcdf::File, name: &'static str) -> Result<Vec<f32>, String> {
    let Some(var) = file.variable(name) else {
        warn_missing_variable_once(name);
        return Err(format!("GLM file has no '{name}' variable (product schema change?)"));
    };
    var.get_values::<f32, _>(..)
        .map_err(|e| format!("Failed to read {name}: {e}"))
}

/// Read a 1-D f32 variable that a level may legitimately lack.
///
/// Returns `Ok(None)` when absent, but still reports it: the *declared*
/// optionality lives in [`LevelVars::area`], so reaching here with a name in
/// hand means we asked for something the product used to have.
fn read_optional_f32(file: &netcdf::File, name: &'static str) -> Result<Option<Vec<f32>>, String> {
    let Some(var) = file.variable(name) else {
        warn_missing_variable_once(name);
        return Ok(None);
    };
    var.get_values::<f32, _>(..)
        .map(Some)
        .map_err(|e| format!("Failed to read {name}: {e}"))
}

/// Read a required 1-D variable as f64 (trying f64 first, then f32).
fn read_required_f64_or_f32(file: &netcdf::File, name: &'static str) -> Result<Vec<f64>, String> {
    let Some(var) = file.variable(name) else {
        warn_missing_variable_once(name);
        return Err(format!("GLM file has no '{name}' variable (product schema change?)"));
    };
    // Try f64 first
    if let Ok(vals) = var.get_values::<f64, _>(..) {
        return Ok(vals);
    }
    // Fall back to f32 → f64
    var.get_values::<f32, _>(..)
        .map(|vals| vals.into_iter().map(|v| v as f64).collect())
        .map_err(|e| format!("Failed to read {name}: {e}"))
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
    use crate::glm::MIN_FILES_FOR_TOTAL_VERDICT;

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
    /// NOTE: this fixture writes plain unpacked `f32`. The real product stores
    /// these as `short` with `_Unsigned`, `scale_factor` and `add_offset`, so
    /// the assertions below deliberately do not pin absolute *values* —
    /// see the TODO on `GlmFlash::area`. `fix/glm-cf-unpacking` owns the CF
    /// unpacking work and should extend this fixture to packed shorts; changing
    /// it here would collide with that branch.
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

    fn parse_flashes(bytes: &[u8]) -> Result<Vec<GlmFlash>, String> {
        parse_glm_netcdf(bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Flash])
    }

    #[test]
    fn flash_level_reports_area_and_event_level_reports_none() {
        let bytes = synthetic_glm_file(Fixture::default());

        let flashes = parse_flashes(&bytes).expect("parse flash level");
        assert_eq!(flashes.len(), 2);
        // Pin that the *right column* was read, in order, without pinning the
        // absolute scale (which `fix/glm-cf-unpacking` will change). The fixture
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

        let events = parse_glm_netcdf(
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
            Ok(("a.nc".into(), Vec::new())),
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

        let plan = plan_downloads(&keys, &cache);
        assert_eq!(
            plan.in_window, 12,
            "the window still contains every listed file, cached or not"
        );
        assert_eq!(plan.new_keys.len(), 3, "only the uncached ones need downloading");

        // The pathological steady state: everything cached but one straggler,
        // which is what a 20 s poll against 20 s granules looks like.
        let mut cache = GlmCache::default();
        for key in keys.iter().take(11) {
            cache.insert(key.clone(), Vec::new());
        }
        let plan = plan_downloads(&keys, &cache);
        assert_eq!(plan.new_keys.len(), 1);
        let report = summarize_failures(plan.in_window, vec!["k11.nc: boom".into()])
            .expect("one failure");
        assert!(
            !report.is_total(),
            "one straggler failing must never read as a total outage, got {report:?}"
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

    /// A single bad granule on a quiet tick must not read as a product-wide
    /// schema change. Granules land every 20 s and the poll interval is 20 s, so
    /// "one uncached file, and it failed" is an ordinary steady state.
    #[test]
    fn one_bad_granule_is_never_a_total_failure() {
        for in_window in 1..MIN_FILES_FOR_TOTAL_VERDICT {
            let errors: Vec<String> = (0..in_window).map(|i| format!("f{i}: boom")).collect();
            let report = summarize_failures(in_window, errors).expect("failures present");
            assert!(
                !report.is_total(),
                "{in_window} file(s) is too small a sample to declare a systematic failure"
            );
        }

        let errors: Vec<String> = (0..MIN_FILES_FOR_TOTAL_VERDICT)
            .map(|i| format!("f{i}: boom"))
            .collect();
        let report =
            summarize_failures(MIN_FILES_FOR_TOTAL_VERDICT, errors).expect("failures present");
        assert!(report.is_total(), "at the floor, a full sweep is a real verdict");
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
        assert!(flashes[0].energy > 0.0);
    }
}
