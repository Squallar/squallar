use chrono::{Duration, NaiveDateTime, NaiveTime};

use crate::archive::Identifier;
use crate::sites::RadarNetwork;
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
#[derive(Debug, PartialEq)]
pub struct DecodedScan {
    pub scan: Scan,
    pub declared_nyquist: crate::nyquist::DeclaredNyquist,
}

/// How much of the circle one assembled sweep actually covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepCoverage {
    /// The cut's elevation number, as [`nexrad_model::data::Sweep`] reports it.
    pub elevation_number: u8,
    /// Radials the sweep holds.
    pub radials: usize,
    /// The angle between adjacent radials, from
    /// `azimuth::median_azimuth_step_deg` — the sweep's own measured
    pub azimuth_step_degrees: f64,
    /// The widest gap between adjacent azimuths, degrees, walked circularly so
    pub largest_gap_degrees: f64,
    /// Whether the sweep covers the circle — its widest gap is one the sampler
    /// would interpolate across. See `azimuth::covers_the_circle`.
    pub is_whole: bool,
}

impl SweepCoverage {
    /// How much of the circle the sweep covers, degrees.
    #[must_use]
    pub fn arc_degrees(&self) -> f64 {
        if self.is_whole {
            360.0
        } else {
            (360.0 - self.largest_gap_degrees).max(0.0)
        }
    }
}

/// Measure every sweep in a scan. See [`SweepCoverage`].
#[must_use]
pub fn sweep_coverage(scan: &Scan) -> Vec<SweepCoverage> {
    scan.sweeps()
        .iter()
        .map(|sweep| {
            let azimuths: Vec<f64> = sweep
                .radials()
                .iter()
                .map(|radial| f64::from(radial.azimuth_angle_degrees()))
                .collect();
            SweepCoverage {
                elevation_number: sweep.elevation_number(),
                radials: sweep.radials().len(),
                azimuth_step_degrees: crate::azimuth::median_azimuth_step_deg(
                    azimuths.iter().copied(),
                )
                .unwrap_or(0.0),
                largest_gap_degrees: crate::azimuth::largest_azimuth_gap_deg(
                    azimuths.iter().copied(),
                )
                .unwrap_or(0.0),
                is_whole: crate::azimuth::covers_the_circle(&azimuths),
            }
        })
        .collect()
}

impl DecodedScan {
    /// This volume's per-sweep coverage. See [`SweepCoverage`].
    #[must_use]
    pub fn sweep_coverage(&self) -> Vec<SweepCoverage> {
        sweep_coverage(&self.scan)
    }
}

/// Decode a downloaded volume into its `Scan` and its declared Nyquist table,
/// in **one** pass over the file.
fn decoded(file: &nexrad_data::volume::File) -> Result<DecodedScan> {
    use crate::par::*;

    let contributions = file
        .records()?
        .into_par_iter()
        .map(contribution)
        .collect::<Vec<_>>();

    fold_contributions(file, contributions)
}

/// The site's location, as one Message 31's volume data block states it.
struct SiteLocation {
    latitude: f32,
    longitude: f32,
    site_height: i16,
    tower_height: u16,
}

/// What one LDM record contributes to the volume.
struct RecordContribution {
    declared_nyquist: crate::nyquist::DeclaredNyquist,
    radials: Vec<nexrad_model::data::Radial>,
    coverage_pattern: Option<nexrad_model::data::VolumeCoveragePattern>,
    site_location: Option<SiteLocation>,
}

/// One record's decompress-and-decode: the body of what used to be the walk's
/// inner loop, lifted out so it can run on a worker.
fn contribution(
    record: nexrad_data::volume::Record<'_>,
) -> std::result::Result<RecordContribution, nexrad_data::result::Error> {
    use nexrad_decode::messages::MessageContents;

    let record = if record.compressed() {
        record.decompress()?
    } else {
        record
    };

    let mut out = RecordContribution {
        declared_nyquist: crate::nyquist::DeclaredNyquist::empty(),
        radials: Vec::new(),
        coverage_pattern: None,
        site_location: None,
    };

    for message in record.messages()? {
        match message.into_contents() {
            MessageContents::DigitalRadarData(m) => {
                if out.site_location.is_none()
                    && let Some(volume) = m.volume_data_block()
                {
                    out.site_location = Some(SiteLocation {
                        latitude: volume.inner().latitude_raw(),
                        longitude: volume.inner().longitude_raw(),
                        site_height: volume.inner().site_height_raw(),
                        tower_height: volume.inner().tower_height_raw(),
                    });
                }
                out.declared_nyquist.declare_from_message(&m);
                out.radials
                    .push(m.into_radial().map_err(nexrad_data::result::Error::from)?);
            }
            MessageContents::VolumeCoveragePattern(m) if out.coverage_pattern.is_none() => {
                out.coverage_pattern = Some(crate::chunks::coverage_pattern_from(&m));
            }
            _ => {}
        }
    }

    Ok(out)
}

/// Fold the per-record results back into one volume, **in record order**.
fn fold_contributions(
    file: &nexrad_data::volume::File,
    contributions: Vec<std::result::Result<RecordContribution, nexrad_data::result::Error>>,
) -> Result<DecodedScan> {
    let mut declared_nyquist = crate::nyquist::DeclaredNyquist::empty();
    let mut radials: Vec<nexrad_model::data::Radial> = Vec::new();
    let mut coverage_pattern = None;
    let mut site_location: Option<SiteLocation> = None;

    for contribution in contributions {
        let contribution = contribution?;
        for (elevation_number, metres_per_second) in contribution.declared_nyquist.iter() {
            declared_nyquist.declare(elevation_number, metres_per_second);
        }
        radials.extend(contribution.radials);
        if coverage_pattern.is_none() {
            coverage_pattern = contribution.coverage_pattern;
        }
        if site_location.is_none() {
            site_location = contribution.site_location;
        }
    }

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

    for cut in sweep_coverage(&scan).iter().filter(|c| !c.is_whole) {
        log::warn!(
            "Cut {} covers {:.1}° of azimuth, not 360°: {} radials at {:.2}° spacing with a \
             {:.1}° gap. Radials absent from the volume, not dropped in decoding.",
            cut.elevation_number,
            cut.arc_degrees(),
            cut.radials,
            cut.azimuth_step_degrees,
            cut.largest_gap_degrees,
        );
    }

    Ok(DecodedScan {
        scan,
        declared_nyquist,
    })
}

/// Decode an archive volume that is already in memory.
///
/// [`decoded`] over a [`nexrad_data::volume::File`] built from `bytes`, which is
/// the same walk every download below ends in — one pass, both the `Scan` and
/// the declared Nyquist table, off the same decompressed records.
///
/// # The seam
///
/// **This is the one place a volume's network chooses how it is decoded**, and
/// today both arms choose the same routine. It is named rather than implied
/// because of what sits behind it: **anything that produces a [`DecodedScan`]
/// inherits the entire downstream pipeline** — render, derive, voxel, xsect,
/// hover and the loop paths, every one of them digest-pinned and none of them
/// asking which instrument the volume came off. A working TDWR decoder is
/// therefore *one function* away from the whole application once
/// [`crate::sites::RadarNetwork`]'s spike list closes; nothing above this line
/// has to learn a second source.
///
/// The classification is [`RadarNetwork::of_id`] over the volume header's ICAO.
/// A file with no header, or a header with no ICAO, classifies as
/// [`RadarNetwork::Wsr88d`] — which is what this function answered before the
/// match existed, and is why adding it changes no behaviour.
///
/// **What is pinned and what is not**, because the difference matters to
/// whoever fills the arm in. `both_network_arms_decode_a_volume_to_the_same_answer`
/// pins that *each arm decodes*, and `of_id_is_the_prefix_rule_it_replaced` pins
/// the rule. Nothing pins that this function applies that rule *to the header*:
/// with both arms calling one routine the choice is unobservable from outside,
/// and hardcoding either arm passes every test in the tree — measured, not
/// assumed. That remainder closes itself the moment the arms diverge, and it is
/// left open rather than answered with a log-capture harness built for one
/// `debug!` line.
pub fn decode_bytes(bytes: Vec<u8>) -> Result<DecodedScan> {
    // GUNZIP FIRST, AND BEFORE THE HEADER READ RATHER THAN AFTER.
    //
    // Archives before ~2016 store volumes gzip-wrapped, and every step below
    // fails on one in a way that does not name the cause: `header()` cannot
    // parse compressed bytes so it returns `None`, the ICAO comes back empty,
    // the network match falls to the WSR-88D arm, and `records()` finally
    // refuses with `CompressedFile`. What the user saw was "Could not decode
    // the volume" for a file that had downloaded perfectly.
    //
    // `decompress` returns `self` unchanged when the magic bytes are absent, so
    // a modern `_V06` volume pays one two-byte comparison. It takes `self` by
    // value, so the compressed buffer is freed as the decompressed one is
    // built rather than both being held.
    let file = nexrad_data::volume::File::new(bytes).decompress()?;
    let icao = file.header().and_then(|h| h.icao_of_radar());
    match RadarNetwork::of_id(icao.as_deref().unwrap_or("")) {
        RadarNetwork::Wsr88d => decoded(&file),
        RadarNetwork::Tdwr => {
            // Once per volume, never per radial. The plug-in point for a real
            // TDWR decoder is this arm; what it has to fix first is the spike
            // list on `RadarNetwork`, item 1 above all.
            log::debug!(
                "decoding a TDWR volume ({}): WSR-88D framing routine; Message-31 \
                 padding defect stands (features.md, TDWR entry)",
                icao.as_deref().unwrap_or("no ICAO"),
            );
            decoded(&file)
        }
    }
}

pub(crate) async fn list_files(site: &str, date: &chrono::NaiveDate) -> Result<Vec<Identifier>> {
    crate::tls::init();
    Ok(crate::archive::list_files(&crate::sources::DataSources::production(), site, date).await?)
}

pub(crate) async fn download_file(identifier: Identifier) -> Result<Vec<u8>> {
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

/// Locate and download the archive volume nearest `timestamp`, **undecoded**.
pub async fn fetch_scan(site: &str, timestamp: NaiveDateTime) -> Result<Vec<u8>> {
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

    log::info!("Data file size (bytes): {}", downloaded_file.len());

    Ok(downloaded_file)
}

/// Download the latest scan if it is newer than `current_timestamp`,
pub async fn fetch_latest_if_newer(
    site: &str,
    date: &chrono::NaiveDate,
    current_timestamp: Option<NaiveDateTime>,
) -> Result<Option<(Vec<u8>, NaiveDateTime)>> {
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
    Ok(Some((downloaded_file, latest_dt)))
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

/// Whether two statements of a Level II volume start name the **same volume
/// scan**: they agree once truncated to the whole second.
pub fn names_same_volume(a: NaiveDateTime, b: NaiveDateTime) -> bool {
    a.and_utc().timestamp() == b.and_utc().timestamp()
}

/// Download one archive object by identifier, **undecoded**. Every loop frame
/// comes through here. See [`fetch_scan`] for why the decode is the caller's.
pub async fn fetch_scan_object(identifier: Identifier) -> Result<Vec<u8>> {
    log::info!("Downloading scan \"{}\"...", identifier.name());
    download_file(identifier).await
}

/// The scan adjacent to `current_timestamp`, strictly after it when `forward`
/// and strictly before it otherwise, capped to the extremes of what the
/// neighbouring day holds, **undecoded**. Returns
/// `(archive_bytes, actual_utc_timestamp)`. See [`fetch_scan`] for why the
/// decode is the caller's.
pub async fn fetch_adjacent_scan(
    site: &str,
    current_timestamp: NaiveDateTime,
    forward: bool,
) -> Result<(Vec<u8>, NaiveDateTime)> {
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
    Ok((downloaded, ts))
}

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
pub async fn poll_chunks(
    poller: &mut crate::chunks::ChunkPoller,
) -> std::result::Result<crate::chunks::PollOutcome, crate::chunks::ChunkError> {
    crate::tls::init();
    poller
        .poll(&crate::sources::DataSources::production())
        .await
}

/// Fetch and ingest one chunk a push notification named.
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
