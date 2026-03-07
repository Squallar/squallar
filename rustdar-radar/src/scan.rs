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
