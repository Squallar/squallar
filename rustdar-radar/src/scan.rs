use chrono::{Duration, NaiveDateTime, NaiveTime};

use nexrad_data::aws::archive::Identifier;
use nexrad_model::data::Scan;
use nexrad_data::result::Result;

// `nexrad-data`'s two network entry points, wrapped so that every call installs
// the TLS crypto provider first.
//
// These deliberately shadow the upstream names: every call site below reads as a
// plain `list_files(..)` / `download_file(..)` and is routed through
// `tls::init()` whether or not whoever wrote it knew about TLS. The alternative
// -- calling `tls::init()` from each of the seven call sites -- is one `git
// revert` away from a graph where some paths are covered and some are not.
//
// The client these reach is built inside a `once_cell::sync::Lazy` in
// `nexrad-data`, so it is constructed on the first S3 request rather than at
// startup. With `rustls-no-provider` and no provider installed, that is a
// `panic!("No provider set")` on first use. See `crate::tls`.
//
// `pub(crate)` rather than private so the `tls` probe can poll one of them.

pub(crate) async fn list_files(
    site: &str,
    date: &chrono::NaiveDate,
) -> Result<Vec<Identifier>> {
    crate::tls::init();
    nexrad_data::aws::archive::list_files(site, date).await
}

pub(crate) async fn download_file(
    identifier: Identifier,
) -> Result<nexrad_data::volume::File> {
    crate::tls::init();
    nexrad_data::aws::archive::download_file(identifier).await
}

/// List files for the given date, falling back to the previous day if empty.
/// Returns `None` if both days are empty, otherwise `(files, effective_date)`.
async fn list_files_with_fallback(
    site: &str,
    date: &chrono::NaiveDate,
) -> Result<Option<(Vec<Identifier>, chrono::NaiveDate)>> {
    let metas = list_files(site, date).await?;
    if !metas.is_empty() {
        return Ok(Some((metas, *date)));
    }
    let prev = *date - Duration::days(1);
    log::info!("No files for {date}, trying previous day {prev}");
    let prev_metas = list_files(site, &prev).await?;
    if prev_metas.is_empty() {
        return Ok(None);
    }
    Ok(Some((prev_metas, prev)))
}

/// Check for new radar files without downloading them
/// Returns the timestamp of the latest available scan, or None if no files found
pub async fn check_latest_scan(
    site: &str,
    date: &chrono::NaiveDate,
) -> Result<Option<NaiveDateTime>> {
    let Some((metas, effective_date)) = list_files_with_fallback(site, date).await? else {
        return Ok(None);
    };

    // Find the latest scan, using Option to avoid returning a spurious midnight time
    let mut latest_time: Option<NaiveTime> = None;
    for m in metas.iter() {
        let Some(time_str) = m.name().split('_').nth(1) else {
            continue;
        };
        if let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S")
            && latest_time.is_none_or(|lt| time > lt) {
                latest_time = Some(time);
            }
    }

    // Only return Some if at least one filename parsed successfully
    Ok(latest_time.map(|t| effective_date.and_time(t)))
}

pub async fn get_scan(site: &str, timestamp: NaiveDateTime) -> Result<Scan> {
    let date = timestamp.date();
    let Some((metas, effective_date)) = list_files_with_fallback(site, &date).await? else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No files found for the specified date or previous day.",
        )
        .into());
    };
    let fell_back = effective_date != date;

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
        if latest_time.is_none_or(|lt| parsed_time > lt) {
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
    let Some((metas, effective_date)) = list_files_with_fallback(site, date).await? else {
        return Ok(None);
    };

    // Find the latest scan
    let mut latest_time: Option<NaiveTime> = None;
    let mut latest_meta = None;
    for m in metas.iter() {
        let Some(time_str) = m.name().split('_').nth(1) else {
            continue;
        };
        if let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S")
            && latest_time.is_none_or(|lt| time > lt) {
                latest_time = Some(time);
                latest_meta = Some(m);
            }
    }

    let (latest_time, latest_meta) = match (latest_time, latest_meta) {
        (Some(t), Some(m)) => (t, m),
        _ => return Ok(None),
    };

    let latest_dt = effective_date.and_time(latest_time);

    // Check if we already have this scan
    let should_fetch = current_timestamp.is_none_or(|current| latest_dt > current);
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

/// List all available scans within a time range, returning their timestamps
/// and file identifiers sorted oldest-first.
///
/// Issues one S3 LIST per date in the range (at most 2–3 calls for 24h).
pub async fn list_scans_for_range(
    site: &str,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<Vec<(NaiveDateTime, Identifier)>> {
    let mut results = Vec::new();
    let mut date = start.date();
    let end_date = end.date();

    while date <= end_date {
        if let Some((metas, effective_date)) = list_files_with_fallback(site, &date).await? {
            for m in &metas {
                let Some(time_str) = m.name().split('_').nth(1) else {
                    continue;
                };
                let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S") else {
                    continue;
                };
                let dt = effective_date.and_time(time);
                if dt >= start && dt <= end {
                    results.push((dt, m.clone()));
                }
            }
        }
        date += Duration::days(1);
    }

    results.sort_by_key(|(dt, _)| *dt);
    results.dedup_by_key(|(dt, _)| *dt);
    Ok(results)
}

/// Download a single scan by its file identifier.
pub async fn download_scan(identifier: Identifier) -> Result<Scan> {
    log::info!("Downloading scan \"{}\"...", identifier.name());
    let downloaded_file = download_file(identifier).await?;
    let scan = downloaded_file.scan()?;
    Ok(scan)
}

/// Find and download the adjacent scan (next or previous) relative to the
/// given UTC timestamp. Returns `(Scan, actual_utc_timestamp)`.
///
/// For *forward*: returns the first scan strictly after `current_timestamp`.
///   If none exists on that day, returns the latest available scan.
/// For *backward*: returns the last scan strictly before `current_timestamp`.
///   If none exists on that day, tries the previous day.
pub async fn get_adjacent_scan(
    site: &str,
    current_timestamp: NaiveDateTime,
    forward: bool,
) -> Result<(Scan, NaiveDateTime)> {
    let date = current_timestamp.date();

    // Collect scans from the current day (and neighbor day for boundary cases).
    let mut all: Vec<(NaiveDateTime, Identifier)> = Vec::new();

    // Always list the current day
    if let Some((metas, effective_date)) = list_files_with_fallback(site, &date).await? {
        for m in &metas {
            let Some(time_str) = m.name().split('_').nth(1) else { continue };
            let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S") else { continue };
            all.push((effective_date.and_time(time), m.clone()));
        }
    }

    // For forward: also list the next day if near the boundary
    // For backward: also list the previous day
    let neighbor = if forward { date + Duration::days(1) } else { date - Duration::days(1) };
    if let Some((metas, effective_date)) = list_files_with_fallback(site, &neighbor).await? {
        for m in &metas {
            let Some(time_str) = m.name().split('_').nth(1) else { continue };
            let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S") else { continue };
            all.push((effective_date.and_time(time), m.clone()));
        }
    }

    all.sort_by_key(|(dt, _)| *dt);
    all.dedup_by_key(|(dt, _)| *dt);

    let pick = if forward {
        // First scan strictly after current_timestamp
        all.iter()
            .find(|(dt, _)| *dt > current_timestamp)
            .or_else(|| all.last()) // cap to latest available
    } else {
        // Last scan strictly before current_timestamp
        all.iter()
            .rev()
            .find(|(dt, _)| *dt < current_timestamp)
            .or_else(|| all.first()) // cap to earliest available
    };

    let Some((ts, ident)) = pick else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No adjacent scan found",
        ).into());
    };

    let ts = *ts;
    let downloaded = download_file(ident.clone()).await?;
    let scan = downloaded.scan()?;
    Ok((scan, ts))
}

// ---------------------------------------------------------------------------
// Level III product fetching
// ---------------------------------------------------------------------------

/// Errors that can occur during Level III fetch operations.
#[derive(Debug, thiserror::Error)]
pub enum Level3FetchError {
    /// HTTP request failed.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// No matching product found.
    #[error("not found: {0}")]
    NotFound(String),
    /// Level III decoding failed.
    #[error("decode error: {0}")]
    Decode(#[from] nexrad_level3::result::Error),
}

use nexrad_level3::model::Level3Message;

// ---------------------------------------------------------------------------
// TGFTP (NWS) Level III product fetching
// ---------------------------------------------------------------------------

const TGFTP_BASE_URL: &str = "https://tgftp.nws.noaa.gov/SL.us008001/DF.of/DC.radar";

/// Fetch the latest Level III product from NWS TGFTP.
///
/// The `sn.last` endpoint always returns the most recent product, so no
/// listing or timestamp matching is needed.
///
/// `tgftp_dir` is the directory component, e.g. `"56rm0"` for SRM tilt 0.
/// `site` is the 4-letter ICAO code (e.g. `"KTLX"`) — lowercased for the URL.
pub async fn get_tgftp_product(
    site: &str,
    tgftp_dir: &str,
) -> std::result::Result<Level3Message, Level3FetchError> {
    let site_lower = site.to_lowercase();
    let url = format!(
        "{TGFTP_BASE_URL}/DS.{tgftp_dir}/SI.{site_lower}/sn.last"
    );

    log::info!("Fetching TGFTP product: {url}");
    let client = crate::tls::client(
        crate::tls::USER_AGENT,
        std::time::Duration::from_secs(30),
    )
    .build()?;
    let resp = client.get(&url).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(Level3FetchError::NotFound(format!(
            "TGFTP product DS.{tgftp_dir} not found for {site}"
        )));
    }
    let resp = resp.error_for_status()?;
    let bytes = resp.bytes().await?;

    let message = nexrad_level3::decode::decode_product(&bytes)?;
    Ok(message)
}
