use chrono::{Duration, NaiveDateTime, NaiveTime};

use nexrad_data::aws::archive::{download_file, list_files};
use nexrad_model::data::Scan;
use nexrad_data::result::Result;

/// Check for new radar files without downloading them
/// Returns the timestamp of the latest available scan, or None if no files found
pub async fn check_latest_scan(
    site: &str,
    date: &chrono::NaiveDate,
) -> Result<Option<NaiveDateTime>> {
    let metas = list_files(site, date).await?;

    // If no files found for the requested date, try the previous day.
    // This handles the period just after midnight UTC before new scans appear.
    let (metas, effective_date) = if metas.is_empty() {
        let prev = *date - Duration::days(1);
        log::info!("No files for {date}, trying previous day {prev}");
        let prev_metas = list_files(site, &prev).await?;
        if prev_metas.is_empty() {
            return Ok(None);
        }
        (prev_metas, prev)
    } else {
        (metas, *date)
    };

    // Find the latest scan, using Option to avoid returning a spurious midnight time
    let mut latest_time: Option<NaiveTime> = None;
    for m in metas.iter() {
        let Some(time_str) = m.name().split('_').nth(1) else {
            continue;
        };
        if let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S") {
            if latest_time.map_or(true, |lt| time > lt) {
                latest_time = Some(time);
            }
        }
    }

    // Only return Some if at least one filename parsed successfully
    Ok(latest_time.map(|t| effective_date.and_time(t)))
}

pub async fn get_scan(site: &str, timestamp: NaiveDateTime) -> Result<Scan> {
    let metas = list_files(site, &timestamp.date()).await?;

    // If no files found for the requested date, try the previous day.
    // This handles the period just after midnight UTC before new scans appear.
    let (metas, fell_back) = if metas.is_empty() {
        let prev = timestamp.date() - Duration::days(1);
        log::info!("No files for {}, trying previous day {}", timestamp.date(), prev);
        let prev_metas = list_files(site, &prev).await?;
        if prev_metas.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No files found for the specified date or previous day.",
            )
            .into());
        }
        (prev_metas, true)
    } else {
        (metas, false)
    };

    log::info!("Found {} files.", metas.len());

    // Parse each filename once and find closest + latest in a single pass
    let mut best_meta = None;
    let mut min_diff = i64::MAX;
    let mut latest_meta = None;
    let mut latest_time: Option<NaiveTime> = None;
    let mut best_time: Option<NaiveTime> = None;

    for m in metas.iter() {
        let Some(time_str) = m.name().split('_').nth(1) else {
            continue;
        };
        let Ok(parsed_time) = NaiveTime::parse_from_str(time_str, "%H%M%S") else {
            continue;
        };

        // Track the latest available scan
        if latest_time.map_or(true, |lt| parsed_time > lt) {
            latest_time = Some(parsed_time);
            latest_meta = Some(m);
        }

        let diff = parsed_time
            .signed_duration_since(timestamp.time())
            .num_seconds()
            .abs();

        if diff < min_diff {
            min_diff = diff;
            best_meta = Some(m);
            best_time = Some(parsed_time);
        }
    }

    // When we fell back to the previous day, always use the latest scan from
    // that day rather than the closest-to-requested-time match (which would
    // pick a ~24-hour-old scan near midnight).
    let meta = if fell_back {
        match (latest_time, latest_meta) {
            (Some(_), Some(lm)) => {
                log::info!("Using latest scan from previous day.");
                lm
            }
            _ => metas.first().expect("metas is non-empty"),
        }
    } else {
        // Normal case: pick the closest match to the requested time
        match (best_meta, best_time) {
            (Some(m), Some(t)) => {
                // If the closest match is in the future and latest is in the past,
                // use the latest available scan instead
                if let (Some(lt), Some(lm)) = (latest_time, latest_meta) {
                    if t > timestamp.time() && lt < timestamp.time() {
                        log::info!("Requested time is too new, using latest available scan.");
                        lm
                    } else {
                        m
                    }
                } else {
                    m
                }
            }
            _ => metas.first().expect("metas is non-empty"),
        }
    };

    log::info!(
        "Nearest file to {:?} is {:?}.",
        timestamp.time(),
        meta.name()
    );

    log::info!("Downloading file \"{}\"...", meta.name());
    let downloaded_file = download_file(meta.clone()).await?;

    log::info!("Data file size (bytes): {}", downloaded_file.data().len());

    // The new API handles decompression and decoding automatically
    let scan = downloaded_file.scan()?;
    Ok(scan)
}

/// Check for the latest scan and fetch it if newer than a reference timestamp.
/// Combines check + fetch into a single `list_files` call, avoiding the
/// duplicate S3 LIST that happens when `check_latest_scan` + `get_scan` are
/// called separately.
pub async fn check_and_fetch_latest(
    site: &str,
    date: &chrono::NaiveDate,
    current_timestamp: Option<NaiveDateTime>,
) -> Result<Option<(Scan, NaiveDateTime)>> {
    let metas = list_files(site, date).await?;

    // If no files found for the requested date, try the previous day.
    // This handles the period just after midnight UTC before new scans appear.
    let (metas, effective_date) = if metas.is_empty() {
        let prev = *date - Duration::days(1);
        log::info!("No files for {date}, trying previous day {prev}");
        let prev_metas = list_files(site, &prev).await?;
        if prev_metas.is_empty() {
            return Ok(None);
        }
        (prev_metas, prev)
    } else {
        (metas, *date)
    };

    // Find the latest scan
    let mut latest_time: Option<NaiveTime> = None;
    let mut latest_meta = None;
    for m in metas.iter() {
        let Some(time_str) = m.name().split('_').nth(1) else {
            continue;
        };
        if let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S") {
            if latest_time.map_or(true, |lt| time > lt) {
                latest_time = Some(time);
                latest_meta = Some(m);
            }
        }
    }

    let (latest_time, latest_meta) = match (latest_time, latest_meta) {
        (Some(t), Some(m)) => (t, m),
        _ => return Ok(None),
    };

    let latest_dt = effective_date.and_time(latest_time);

    // Check if we already have this scan
    let should_fetch = current_timestamp.map_or(true, |current| latest_dt > current);
    if !should_fetch {
        log::info!("Already have latest scan");
        return Ok(None);
    }

    // Download directly using the already-resolved meta (no second list_files!)
    log::info!("Fetching newer scan: {}", latest_meta.name());
    let downloaded_file = download_file(latest_meta.clone()).await?;
    let scan = downloaded_file.scan()?;
    Ok(Some((scan, latest_dt)))
}

// ---------------------------------------------------------------------------
// Level III product fetching
// ---------------------------------------------------------------------------

const LEVEL3_BUCKET_URL: &str = "https://unidata-nexrad-level3.s3.amazonaws.com";

/// Errors that can occur during Level III fetch operations.
#[derive(Debug)]
pub enum Level3FetchError {
    /// HTTP request failed.
    Http(reqwest::Error),
    /// No matching product found on S3.
    NotFound(String),
    /// Level III decoding failed.
    Decode(nexrad_level3::result::Error),
    /// XML listing parse error.
    XmlParse(String),
}

impl std::fmt::Display for Level3FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Level3FetchError::Http(e) => write!(f, "HTTP error: {e}"),
            Level3FetchError::NotFound(msg) => write!(f, "not found: {msg}"),
            Level3FetchError::Decode(e) => write!(f, "decode error: {e}"),
            Level3FetchError::XmlParse(msg) => write!(f, "XML parse error: {msg}"),
        }
    }
}

impl std::error::Error for Level3FetchError {}

impl From<reqwest::Error> for Level3FetchError {
    fn from(e: reqwest::Error) -> Self {
        Level3FetchError::Http(e)
    }
}

impl From<nexrad_level3::result::Error> for Level3FetchError {
    fn from(e: nexrad_level3::result::Error) -> Self {
        Level3FetchError::Decode(e)
    }
}

/// Convert a 4-letter ICAO radar site code (e.g. "KTLX") to the 3-letter
/// code used in the Level III S3 bucket (e.g. "TLX").
/// Non-CONUS sites that don't start with 'K' (e.g. "PGUA", "TJUA") are
/// returned with just the first character stripped, matching the bucket
/// convention.
fn level3_site_code(site: &str) -> &str {
    if site.len() == 4 {
        &site[1..]
    } else {
        site
    }
}

/// List available Level III product keys for a site/product/date on S3.
///
/// Keys in the `unidata-nexrad-level3` bucket are flat:
/// `{SITE3}_{CODE}_{YYYY}_{MM}_{DD}_{HH}_{mm}_{ss}`
///
/// Returns a sorted vec of (key, NaiveDateTime) pairs.
async fn list_level3_keys(
    client: &reqwest::Client,
    site: &str,
    product_code: &str,
    date: &chrono::NaiveDate,
) -> std::result::Result<Vec<(String, NaiveDateTime)>, Level3FetchError> {
    let site3 = level3_site_code(site);
    let prefix = format!(
        "{site3}_{product_code}_{:04}_{:02}_{:02}",
        date.year(),
        date.month(),
        date.day()
    );
    let url = format!("{LEVEL3_BUCKET_URL}?list-type=2&prefix={prefix}");

    let resp = client.get(&url).send().await?.error_for_status()?;
    let body = resp.text().await?;

    // Parse the simple S3 XML listing to extract <Key> elements
    let mut keys = Vec::new();
    for line in body.split("<Key>") {
        if let Some(end) = line.find("</Key>") {
            let key = &line[..end];
            if let Some(dt) = parse_level3_key_datetime(key) {
                keys.push((key.to_string(), dt));
            }
        }
    }

    keys.sort_by_key(|(_, dt)| *dt);
    Ok(keys)
}

/// Parse a Level III S3 key like `TLX_N0S_2024_03_08_15_30_42` into a NaiveDateTime.
fn parse_level3_key_datetime(key: &str) -> Option<NaiveDateTime> {
    let parts: Vec<&str> = key.split('_').collect();
    // Expected: [SITE3, CODE, YYYY, MM, DD, HH, mm, ss]
    if parts.len() < 7 {
        return None;
    }
    let year: i32 = parts[2].parse().ok()?;
    let month: u32 = parts[3].parse().ok()?;
    let day: u32 = parts[4].parse().ok()?;
    let hour: u32 = parts[5].parse().ok()?;
    let minute: u32 = parts[6].parse().ok()?;
    let second: u32 = parts.get(7).and_then(|s| s.parse().ok()).unwrap_or(0);

    let date = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
    let time = NaiveTime::from_hms_opt(hour, minute, second)?;
    Some(date.and_time(time))
}

use chrono::Datelike;
use nexrad_level3::model::Level3Message;

/// Fetch the latest Level III product for a site.
///
/// Lists available keys for the given date, picks the one closest to
/// `timestamp`, downloads it, and decodes it.
pub async fn get_level3_product(
    site: &str,
    product_code: &str,
    timestamp: NaiveDateTime,
) -> std::result::Result<Level3Message, Level3FetchError> {
    let client = reqwest::Client::new();
    let date = timestamp.date();

    let mut keys = list_level3_keys(&client, site, product_code, &date).await?;

    // If no keys found, try previous day (same midnight-crossing logic as Level II)
    if keys.is_empty() {
        let prev = date - Duration::days(1);
        keys = list_level3_keys(&client, site, product_code, &prev).await?;
    }

    if keys.is_empty() {
        return Err(Level3FetchError::NotFound(format!(
            "No Level III {product_code} files found for {site} near {timestamp}"
        )));
    }

    // Find the key closest to the requested timestamp
    let best_key = keys
        .iter()
        .min_by_key(|(_, dt)| (dt.signed_duration_since(timestamp)).num_seconds().unsigned_abs())
        .map(|(k, _)| k.clone())
        .ok_or_else(|| Level3FetchError::NotFound("empty key list".to_string()))?;

    log::info!("Downloading Level III product: {best_key}");
    let url = format!("{LEVEL3_BUCKET_URL}/{best_key}");
    let resp = client.get(&url).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;

    let message = nexrad_level3::decode::decode_product(&bytes)?;
    Ok(message)
}

/// Check for the latest available Level III product timestamp without downloading.
pub async fn check_latest_level3(
    site: &str,
    product_code: &str,
    date: &chrono::NaiveDate,
) -> std::result::Result<Option<NaiveDateTime>, Level3FetchError> {
    let client = reqwest::Client::new();
    let keys = list_level3_keys(&client, site, product_code, date).await?;
    Ok(keys.last().map(|(_, dt)| *dt))
}
