//! Fetch GLM lightning flash data from AWS S3.
//!
//! Lists and downloads NetCDF4 files from the `noaa-goes16`/`noaa-goes18`
//! public S3 buckets. Files are LCFA (Lightning Cluster-Filter Algorithm)
//! Level 2 products, each covering ~20 seconds.

use std::collections::HashMap;

use chrono::{NaiveDateTime, TimeDelta, Utc};

use super::{GlmDataLevel, GlmFlash, GlmSatellite};

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

    /// Get all cached flashes as a flat vec.
    pub fn all_flashes(&self) -> Vec<GlmFlash> {
        self.entries.values().flatten().cloned().collect()
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
/// - Returns all observations within the time window
pub async fn fetch_glm_flashes(
    client: &reqwest::Client,
    satellites: &[GlmSatellite],
    time_window_secs: f64,
    levels: &[GlmDataLevel],
    cache: &mut GlmCache,
) -> Result<Vec<GlmFlash>, String> {
    let now = Utc::now().naive_utc();
    let window = TimeDelta::milliseconds((time_window_secs * 1000.0) as i64);
    let start = now - window;
    let cutoff = start;

    // Evict old cache entries
    cache.evict_before(cutoff);

    // List & download new files for each satellite
    let mut new_entries: Vec<(String, Vec<GlmFlash>)> = Vec::new();

    for &sat in satellites {
        let keys = list_glm_files(client, sat, start, now).await?;
        let new_keys: Vec<&str> = keys.iter()
            .filter(|k| !cache.contains_key(k.as_str()))
            .map(|k| k.as_str())
            .collect();

        if new_keys.is_empty() {
            continue;
        }
        log::info!("Downloading {} new GLM files from {}", new_keys.len(), sat.display_name());

        // Download in concurrent batches of 20
        let results = download_and_parse_batch(client, sat, &new_keys, levels).await;
        for (key, flashes) in results {
            new_entries.push((key, flashes));
        }
    }

    // Insert new data into cache
    for (key, flashes) in new_entries {
        cache.insert(key, flashes);
    }

    // Return all cached flashes within the window
    let all = cache.all_flashes();
    let filtered: Vec<GlmFlash> = all.into_iter()
        .filter(|f| f.time >= cutoff && f.time <= now)
        .collect();

    log::info!("GLM: {} flashes in {:.0}s window", filtered.len(), time_window_secs);
    Ok(filtered)
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
) -> Result<Vec<String>, String> {
    let bucket = satellite.bucket();
    let mut all_keys = Vec::new();

    // Collect all (year, doy, hour) tuples we need to query
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
                if node.tag_name().name() == "Key" {
                    if let Some(key) = node.text() {
                        if key.ends_with(".nc") {
                            // Filter by start time encoded in filename
                            if let Some(file_start) = parse_filename_start_time(key) {
                                if file_start >= start && file_start <= end {
                                    all_keys.push(key.to_string());
                                }
                            }
                        }
                    }
                }
            }

            // Check for truncation (pagination)
            let is_truncated = doc.descendants()
                .find(|n| n.tag_name().name() == "IsTruncated")
                .and_then(|n| n.text())
                .is_some_and(|t| t == "true");

            if is_truncated {
                continuation_token = doc.descendants()
                    .find(|n| n.tag_name().name() == "NextContinuationToken")
                    .and_then(|n| n.text())
                    .map(|s| s.to_string());
            } else {
                break;
            }
        }
    }

    Ok(all_keys)
}

/// Parse the start timestamp from a GLM filename.
///
/// Filename: `OR_GLM-L2-LCFA_G16_s20261120145200_e...nc`
/// The `s` field is `YYYYDDDHHMMSSf` where DDD = day of year, f = tenths of second.
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

/// Download and parse a batch of GLM NetCDF files concurrently.
async fn download_and_parse_batch(
    client: &reqwest::Client,
    satellite: GlmSatellite,
    keys: &[&str],
    levels: &[GlmDataLevel],
) -> Vec<(String, Vec<GlmFlash>)> {
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
                Ok(flashes) => Some((key_owned, flashes)),
                Err(e) => {
                    log::warn!("Failed to fetch GLM file {key_owned}: {e}");
                    None
                }
            }
        }
    }).collect();

    futures::stream::iter(futs)
        .buffer_unordered(20)
        .filter_map(|r| async { r })
        .collect()
        .await
}

/// Download a single GLM NetCDF file and parse data from it.
async fn download_and_parse_one(
    client: &reqwest::Client,
    url: &str,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<Vec<GlmFlash>, String> {
    let bytes = client.get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?
        .error_for_status()
        .map_err(|e| format!("HTTP status error: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("Failed to read body: {e}"))?;

    parse_glm_netcdf(&bytes, satellite, levels)
}

/// Variable name sets for each GLM data level.
struct LevelVars {
    lat: &'static str,
    lon: &'static str,
    energy: &'static str,
    area: &'static str,
    time_offset: &'static str,
    level: GlmDataLevel,
}

const FLASH_VARS: LevelVars = LevelVars {
    lat: "flash_lat",
    lon: "flash_lon",
    energy: "flash_energy",
    area: "flash_area",
    time_offset: "flash_time_offset_of_first_event",
    level: GlmDataLevel::Flash,
};

const GROUP_VARS: LevelVars = LevelVars {
    lat: "group_lat",
    lon: "group_lon",
    energy: "group_energy",
    area: "group_area",
    time_offset: "group_time_offset",
    level: GlmDataLevel::Group,
};

const EVENT_VARS: LevelVars = LevelVars {
    lat: "event_lat",
    lon: "event_lon",
    energy: "event_energy",
    area: "event_area",
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
    let lats = read_var_f32(file, vars.lat)?;
    let lons = read_var_f32(file, vars.lon)?;
    let energies = read_var_f32(file, vars.energy)?;
    let areas = read_var_f32(file, vars.area)?;
    let time_offsets = read_var_f64_or_f32(file, vars.time_offset)?;

    let count = lats.len();
    if lons.len() != count || time_offsets.len() != count {
        return Err(format!("Variable length mismatch for {} in GLM file", vars.lat));
    }

    let mut records = Vec::with_capacity(count);
    for i in 0..count {
        let millis = (time_offsets[i] * 1000.0) as i64;
        let time = *time_origin + TimeDelta::milliseconds(millis);

        records.push(GlmFlash {
            lat: lats[i] as f64,
            lon: lons[i] as f64,
            energy: *energies.get(i).unwrap_or(&0.0),
            area: *areas.get(i).unwrap_or(&0.0),
            time,
            satellite,
            level: vars.level,
        });
    }

    Ok(records)
}

/// Read a 1-D f32 variable from a NetCDF file. Returns empty vec if not found.
fn read_var_f32(file: &netcdf::File, name: &str) -> Result<Vec<f32>, String> {
    let var = match file.variable(name) {
        Some(v) => v,
        None => return Ok(Vec::new()),
    };
    var.get_values::<f32, _>(..)
        .map_err(|e| format!("Failed to read {name}: {e}"))
}

/// Read a 1-D variable as f64 (trying f64 first, then f32).
fn read_var_f64_or_f32(file: &netcdf::File, name: &str) -> Result<Vec<f64>, String> {
    let var = match file.variable(name) {
        Some(v) => v,
        None => return Ok(Vec::new()),
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
