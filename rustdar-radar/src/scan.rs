use chrono::{Duration, NaiveDateTime, NaiveTime};

use crate::archive::Identifier;
use nexrad_model::data::Scan;

/// Errors from locating, downloading or decoding an archive scan.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Archive(#[from] crate::archive::ArchiveError),
    #[error(transparent)]
    Decode(#[from] nexrad_data::result::Error),
    /// Nothing in the archive matches the request. An ordinary outcome, not a
    /// failure to reach the bucket.
    #[error("{0}")]
    NoScan(String),
}

pub type Result<T> = std::result::Result<T, ScanError>;

// `crate::archive`'s two network entry points, shadowed so that every call site
// below routes through `tls::init()` without having to know about TLS. Now
// belt-and-braces: `crate::archive` builds its client through `tls::client`,
// which installs the provider itself. `pub(crate)` so the `tls` probe can poll
// one of them.

pub(crate) async fn list_files(site: &str, date: &chrono::NaiveDate) -> Result<Vec<Identifier>> {
    crate::tls::init();
    Ok(crate::archive::list_files(site, date).await?)
}

pub(crate) async fn download_file(identifier: Identifier) -> Result<nexrad_data::volume::File> {
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

/// Timestamp of the latest available scan, without downloading it.
pub async fn check_latest_scan(
    site: &str,
    date: &chrono::NaiveDate,
) -> Result<Option<NaiveDateTime>> {
    let Some((metas, effective_date)) = list_files_with_fallback(site, date).await? else {
        return Ok(None);
    };

    // `Option`, not a default: a default would be a spurious midnight.
    let mut latest_time: Option<NaiveTime> = None;
    for m in metas.iter() {
        let Some(time_str) = m.name().split('_').nth(1) else {
            continue;
        };
        if let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S")
            && latest_time.is_none_or(|lt| time > lt)
        {
            latest_time = Some(time);
        }
    }

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

    // After a fallback, closest-to-requested-time would pick a ~24-hour-old
    // scan near midnight, so take the previous day's latest instead.
    let meta = if fell_back {
        match (latest_time, latest_meta) {
            (Some(_), Some(lm)) => {
                log::info!("Using latest scan from previous day.");
                lm
            }
            _ => metas.first().expect("metas is non-empty"),
        }
    } else {
        match (best_meta, best_time) {
            (Some(m), Some(t)) => {
                // A closest match in the future with a latest in the past means
                // the request is newer than the archive: take the latest.
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

    let scan = downloaded_file.scan()?;
    Ok(scan)
}

/// Fetch the latest scan if it is newer than `current_timestamp`. One
/// `list_files` call, unlike `check_latest_scan` + `get_scan`, which LIST twice.
pub async fn check_and_fetch_latest(
    site: &str,
    date: &chrono::NaiveDate,
    current_timestamp: Option<NaiveDateTime>,
) -> Result<Option<(Scan, NaiveDateTime)>> {
    let Some((metas, effective_date)) = list_files_with_fallback(site, date).await? else {
        return Ok(None);
    };

    let mut latest_time: Option<NaiveTime> = None;
    let mut latest_meta = None;
    for m in metas.iter() {
        let Some(time_str) = m.name().split('_').nth(1) else {
            continue;
        };
        if let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S")
            && latest_time.is_none_or(|lt| time > lt)
        {
            latest_time = Some(time);
            latest_meta = Some(m);
        }
    }

    let (latest_time, latest_meta) = match (latest_time, latest_meta) {
        (Some(t), Some(m)) => (t, m),
        _ => return Ok(None),
    };

    let latest_dt = effective_date.and_time(latest_time);

    let should_fetch = current_timestamp.is_none_or(|current| latest_dt > current);
    if !should_fetch {
        log::info!("Already have latest scan");
        return Ok(None);
    }

    log::info!("Fetching newer scan: {}", latest_meta.name());
    let downloaded_file = download_file(latest_meta.clone()).await?;
    let scan = downloaded_file.scan()?;
    Ok(Some((scan, latest_dt)))
}

/// Scans within a time range, sorted oldest-first. One S3 LIST per date in the
/// range.
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

pub async fn download_scan(identifier: Identifier) -> Result<Scan> {
    log::info!("Downloading scan \"{}\"...", identifier.name());
    let downloaded_file = download_file(identifier).await?;
    let scan = downloaded_file.scan()?;
    Ok(scan)
}

/// The scan adjacent to `current_timestamp`, strictly after it when `forward`
/// and strictly before it otherwise, capped to the extremes of what the
/// neighbouring day holds. Returns `(Scan, actual_utc_timestamp)`.
pub async fn get_adjacent_scan(
    site: &str,
    current_timestamp: NaiveDateTime,
    forward: bool,
) -> Result<(Scan, NaiveDateTime)> {
    let date = current_timestamp.date();

    let mut all: Vec<(NaiveDateTime, Identifier)> = Vec::new();

    if let Some((metas, effective_date)) = list_files_with_fallback(site, &date).await? {
        for m in &metas {
            let Some(time_str) = m.name().split('_').nth(1) else {
                continue;
            };
            let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S") else {
                continue;
            };
            all.push((effective_date.and_time(time), m.clone()));
        }
    }

    // The neighbouring day, for requests near a midnight boundary.
    let neighbor = if forward {
        date + Duration::days(1)
    } else {
        date - Duration::days(1)
    };
    if let Some((metas, effective_date)) = list_files_with_fallback(site, &neighbor).await? {
        for m in &metas {
            let Some(time_str) = m.name().split('_').nth(1) else {
                continue;
            };
            let Ok(time) = NaiveTime::parse_from_str(time_str, "%H%M%S") else {
                continue;
            };
            all.push((effective_date.and_time(time), m.clone()));
        }
    }

    all.sort_by_key(|(dt, _)| *dt);
    all.dedup_by_key(|(dt, _)| *dt);

    let pick = if forward {
        all.iter()
            .find(|(dt, _)| *dt > current_timestamp)
            .or_else(|| all.last()) // cap to latest available
    } else {
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

/// Fetch the latest Level III product for a site. `product` is an AWIPS ID
/// such as `"N0S"`; see [`crate::types::RadarProduct::level3_products`].
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
