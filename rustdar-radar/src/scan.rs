use chrono::{Duration, NaiveDateTime, NaiveTime};

use crate::archive::Identifier;
use nexrad_model::data::Scan;

/// Errors from locating, downloading or decoding an archive scan.
///
/// Shaped like the neighbouring [`crate::level3::Level3Error`]: one variant per layer, so
/// a `{:?}` in the frontend's log line says which one failed. Every consumer
/// formats rather than matches.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Listing or downloading from the archive bucket failed.
    #[error(transparent)]
    Archive(#[from] crate::archive::ArchiveError),
    /// `nexrad-data` could not decode the downloaded volume.
    #[error(transparent)]
    Decode(#[from] nexrad_data::result::Error),
    /// The archive holds nothing matching the request.
    ///
    /// An ordinary outcome — a site with no data for a date, or a navigation
    /// step past the end of the archive — not a failure to reach the bucket.
    #[error("{0}")]
    NoScan(String),
}

/// Convenience alias for this module's Level II operations.
pub type Result<T> = std::result::Result<T, ScanError>;

// The archive's two network entry points, wrapped so that every call installs
// the TLS crypto provider first.
//
// These deliberately shadow the names in `crate::archive`: every call site below
// reads as a plain `list_files(..)` / `download_file(..)` and is routed through
// `tls::init()` whether or not whoever wrote it knew about TLS. The alternative
// -- calling `tls::init()` from each of the seven call sites -- is one `git
// revert` away from a graph where some paths are covered and some are not.
//
// Since `crate::archive` builds its client through `tls::client`, which installs
// the provider itself, these `init()` calls are now belt-and-braces rather than
// the load-bearing guarantee they were when the client was constructed inside
// `nexrad-data`'s `once_cell::sync::Lazy` and could not be reached from here.
// `crate::tls` probes both paths in fresh processes.
//
// `pub(crate)` rather than private so the `tls` probe can poll one of them.

pub(crate) async fn list_files(
    site: &str,
    date: &chrono::NaiveDate,
) -> Result<Vec<Identifier>> {
    crate::tls::init();
    Ok(crate::archive::list_files(site, date).await?)
}

pub(crate) async fn download_file(
    identifier: Identifier,
) -> Result<nexrad_data::volume::File> {
    crate::tls::init();
    Ok(crate::archive::download_file(identifier).await?)
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
        return Err(ScanError::NoScan(
            "No files found for the specified date or previous day.".to_string(),
        ));
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
        return Err(ScanError::NoScan("No adjacent scan found".to_string()));
    };

    let ts = *ts;
    let downloaded = download_file(ident.clone()).await?;
    let scan = downloaded.scan()?;
    Ok((scan, ts))
}

// ---------------------------------------------------------------------------
// Level III product fetching
// ---------------------------------------------------------------------------

/// Fetch the latest Level III product for a site.
///
/// Thin wrapper over [`crate::level3::fetch_latest_product`] that supplies the
/// production origins and the current UTC time, keeping the two Level II/III
/// entry points side by side. `product` is an AWIPS ID such as `"N0S"`; see
/// [`crate::types::RadarProduct::level3_products`].
///
/// This replaces `get_tgftp_product`. TGFTP's `sn.last` needed no listing, but
/// TGFTP is unreachable from a browser — see [`crate::level3`] for what
/// changed and why.
pub async fn get_level3_product(
    site: &str,
    product: &str,
) -> std::result::Result<crate::level3::Level3Product, crate::level3::Level3Error> {
    crate::tls::init();
    crate::level3::fetch_latest_product(
        &crate::sources::DataSources::production(),
        site,
        product,
        chrono::Utc::now().naive_utc(),
    )
    .await
}
