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

/// A decoded archive volume, and the per-cut numbers the decode drops.
///
/// `nexrad_data::volume::File::scan()` builds a `nexrad_model::data::Scan`, and
/// the model's `Radial` has no field for Message 31's declared Nyquist
/// velocity — so the moment a download becomes a `Scan`, where each cut folds
/// is gone. The velocity fold guard in [`crate::sampler`] needs it, and by the
/// time anything reaches the guard the raw file is long dropped.
///
/// So every entry point here hands back both, read from the same bytes on the
/// same walk. `declared_nyquist` is empty rather than absent for a volume that
/// declared nothing — an all-Message-1 archive, which has no such field —
/// and readers estimate for the cuts it does not name. See [`crate::nyquist`].
pub struct DecodedScan {
    pub scan: Scan,
    pub declared_nyquist: crate::nyquist::DeclaredNyquist,
}

/// Decode a downloaded volume into its `Scan` and its declared Nyquist table,
/// in **one** pass over the file.
///
/// # Why this is not `file.scan()` plus a second read
///
/// It used to be exactly that: `nexrad_data::volume::File::scan()` for the
/// model types, then [`crate::nyquist::DeclaredNyquist::from_archive`] for the
/// one radial-header field the model has no room for. Both walk every LDM
/// record, bzip2-decompress it and parse its Message 31s; the only thing the
/// second walk does differently is stop before `into_radial`.
///
/// So it was not a small surcharge on the decode, it was **another decode**.
/// Measured over eight archived volumes (1.1–3.2 MB compressed, best of five
/// runs each), the Nyquist walk cost 1210 ms against `scan()`'s own 1238 ms —
/// 98% — and the pair together 2448 ms against **1243 ms** for the single walk
/// here. Reading one number per cut had been doubling the cost of every volume
/// this application opens.
///
/// That is not a price paid once at startup. It is paid on cold start, on every
/// timeline scrub, on every "next scan" step, and once per frame of a loop
/// download — up to sixty of them — and on the web it is paid on the browser's
/// main thread.
///
/// So the walk happens once and both consumers read the same decompressed
/// records. Nothing about the result changes: the radials, their order, the
/// site and the coverage pattern are `scan()`'s, and the Nyquist table is
/// `from_archive`'s, both pinned against those two functions by
/// [`tests::live_one_pass_decode_matches_the_two_pass_decode`].
///
/// # Why the body restates `scan()` rather than calling it
///
/// The number the fold guard needs is on the Message 31 radial and gone by the
/// time `into_radial` has run, and `scan()` neither returns it nor takes a
/// callback. Reading it therefore has to happen *inside* the walk, and there is
/// no way into upstream's. What is restated is only the traversal: the message-5
/// translation is [`crate::chunks::coverage_pattern_from`], already in this
/// crate for the chunk path, and the radial and sweep construction are
/// upstream's own `into_radial` and `Sweep::from_radials`.
///
/// Message 1 volumes decode to no radials here, exactly as they do through
/// `scan()`, which also matches only `DigitalRadarData`. Widening that is a
/// separate change with its own evidence to gather, not a side effect of this
/// one.
fn decoded(file: &nexrad_data::volume::File) -> Result<DecodedScan> {
    use nexrad_decode::messages::MessageContents;

    // The site's location is stated on every radial's volume block; the first
    // one wins, as it does in `scan()`.
    struct SiteLocation {
        latitude: f32,
        longitude: f32,
        site_height: i16,
        tower_height: u16,
    }

    let mut declared_nyquist = crate::nyquist::DeclaredNyquist::empty();
    let mut radials: Vec<nexrad_model::data::Radial> = Vec::new();
    let mut coverage_pattern = None;
    let mut site_location: Option<SiteLocation> = None;

    for record in file.records()? {
        let record = if record.compressed() {
            record.decompress()?
        } else {
            record
        };
        for message in record.messages()? {
            match message.into_contents() {
                MessageContents::DigitalRadarData(m) => {
                    if site_location.is_none()
                        && let Some(volume) = m.volume_data_block()
                    {
                        site_location = Some(SiteLocation {
                            latitude: volume.inner().latitude_raw(),
                            longitude: volume.inner().longitude_raw(),
                            site_height: volume.inner().site_height_raw(),
                            tower_height: volume.inner().tower_height_raw(),
                        });
                    }
                    // Before `into_radial`, which is where the number is lost.
                    declared_nyquist.declare_from_message(&m);
                    // Through `nexrad_data`'s error rather than straight to
                    // `ScanError`, so a decode failure here is the same variant
                    // it was when `scan()` raised it.
                    radials.push(m.into_radial().map_err(nexrad_data::result::Error::from)?);
                }
                // First one wins, as in `scan()`: a repeat of message 5 inside
                // one volume is the same pattern restated.
                MessageContents::VolumeCoveragePattern(m) if coverage_pattern.is_none() => {
                    coverage_pattern = Some(crate::chunks::coverage_pattern_from(&m));
                }
                _ => {}
            }
        }
    }

    // `scan()`'s own outcome for a volume with no message 5: there is no
    // coverage pattern to invent, and every reader of a `Scan` assumes one.
    let coverage_pattern =
        coverage_pattern.ok_or(nexrad_data::result::Error::MissingCoveragePattern)?;

    let site = site_location.map(|loc| {
        let mut identifier = [0u8; 4];
        if let Some(icao) = file.header().and_then(|h| h.icao_of_radar()) {
            let bytes = icao.as_bytes();
            let len = bytes.len().min(4);
            identifier[..len].copy_from_slice(&bytes[..len]);
        }
        nexrad_model::meta::Site::new(
            identifier,
            loc.latitude,
            loc.longitude,
            loc.site_height,
            loc.tower_height,
        )
    });

    let sweeps = nexrad_model::data::Sweep::from_radials(radials);
    let scan = match site {
        Some(site) => Scan::with_site(site, coverage_pattern, sweeps),
        None => Scan::new(coverage_pattern, sweeps),
    };

    Ok(DecodedScan {
        scan,
        declared_nyquist,
    })
}

// `crate::archive`'s two network entry points, shadowed so that every call site
// below routes through `tls::init()` without having to know about TLS. Now
// belt-and-braces: `crate::archive` builds its client through `tls::client`,
// which installs the provider itself. `pub(crate)` so the `tls` probe can poll
// one of them.
//
// This is also where the production origin table is bound, exactly as
// `get_level3_product` binds it below: threading `&DataSources` out through
// this module's public surface would ripple into every frontend call site for
// no gain — nothing above here overrides an origin.

pub(crate) async fn list_files(site: &str, date: &chrono::NaiveDate) -> Result<Vec<Identifier>> {
    crate::tls::init();
    Ok(crate::archive::list_files(&crate::sources::DataSources::production(), site, date).await?)
}

pub(crate) async fn download_file(identifier: Identifier) -> Result<nexrad_data::volume::File> {
    crate::tls::init();
    Ok(
        crate::archive::download_file(&crate::sources::DataSources::production(), identifier)
            .await?,
    )
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

pub async fn get_scan(site: &str, timestamp: NaiveDateTime) -> Result<DecodedScan> {
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

    decoded(&downloaded_file)
}

/// Fetch the latest scan if it is newer than `current_timestamp`. One
/// `list_files` call, unlike `check_latest_scan` + `get_scan`, which LIST twice.
pub async fn check_and_fetch_latest(
    site: &str,
    date: &chrono::NaiveDate,
    current_timestamp: Option<NaiveDateTime>,
) -> Result<Option<(DecodedScan, NaiveDateTime)>> {
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
    Ok(Some((decoded(&downloaded_file)?, latest_dt)))
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

pub async fn download_scan(identifier: Identifier) -> Result<DecodedScan> {
    log::info!("Downloading scan \"{}\"...", identifier.name());
    let downloaded_file = download_file(identifier).await?;
    decoded(&downloaded_file)
}

/// The scan adjacent to `current_timestamp`, strictly after it when `forward`
/// and strictly before it otherwise, capped to the extremes of what the
/// neighbouring day holds. Returns `(Scan, actual_utc_timestamp)`.
pub async fn get_adjacent_scan(
    site: &str,
    current_timestamp: NaiveDateTime,
    forward: bool,
) -> Result<(DecodedScan, NaiveDateTime)> {
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
    Ok((decoded(&downloaded)?, ts))
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

// ---------------------------------------------------------------------------
// Real-time chunks
// ---------------------------------------------------------------------------

/// A poller for one site's real-time chunk feed, with the crypto provider
/// installed.
///
/// The production origin table is bound in [`poll_chunks`] for the same reason
/// it is bound in `list_files`: threading `&DataSources` out through this
/// module's public surface would ripple into every frontend call site for no
/// gain, since nothing above here overrides an origin.
pub fn chunk_poller(site: &str) -> crate::chunks::ChunkPoller {
    crate::tls::init();
    crate::chunks::ChunkPoller::new(site)
}

/// [`chunk_poller`] resuming from a volume index a caller already knows, which
/// skips the ~10-request discovery search.
pub fn resume_chunk_poller(
    site: &str,
    volume: crate::chunks::VolumeIndex,
) -> crate::chunks::ChunkPoller {
    crate::tls::init();
    crate::chunks::ChunkPoller::resume(site, volume)
}

/// One poll round against the production chunk bucket.
///
/// No sleeping and no looping: the caller owns the timer. That is a wasm
/// requirement rather than a preference — see [`crate::chunks::ChunkPoller`] —
/// and [`crate::chunks::ChunkPoller::suggested_interval`] advises the delay.
pub async fn poll_chunks(
    poller: &mut crate::chunks::ChunkPoller,
) -> std::result::Result<crate::chunks::PollOutcome, crate::chunks::ChunkError> {
    crate::tls::init();
    poller
        .poll(&crate::sources::DataSources::production())
        .await
}

/// Fetch and ingest one chunk a push notification named.
///
/// The counterpart to [`poll_chunks`] for the notification path: the caller
/// already knows the object key, so this is a single `GET` with no listing,
/// discovery or rollover probe. See
/// [`crate::chunks::ChunkPoller::fetch_notified`].
pub async fn fetch_notified_chunk(
    poller: &mut crate::chunks::ChunkPoller,
    id: &crate::chunks::ChunkId,
) -> std::result::Result<crate::chunks::PollOutcome, crate::chunks::ChunkError> {
    crate::tls::init();
    poller
        .fetch_notified(&crate::sources::DataSources::production(), id)
        .await
}

#[cfg(test)]
mod tests;
