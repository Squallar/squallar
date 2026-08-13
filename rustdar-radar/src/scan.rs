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
/// [`tests::live_one_pass_decode_matches_the_two_pass_decode`] — which is
/// `#[ignore]`d, wanting a real volume, so the equivalence is checked only
/// under `-- --ignored`.
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
///
/// # Why the records are decoded in parallel
///
/// The walk above is one walk, but it is not a cheap one, and almost all of it
/// is bzip2. Instructions retired for a dense volume (KFTG, 16.9 MB),
/// `perf stat -e instructions:u` differenced across 2 and 6 repetitions so
/// process setup and the file read cancel, release + `lto`:
///
/// | | this walk, end to end | decompression alone | share |
/// | --- | --- | --- | --- |
/// | `bzip2` → `libbz2-rs-sys` | 7,883,148,609 | 7,806,188,828 | **99.0%** |
/// | `bzip2-rs` | 5,313,317,394 | 5,235,648,591 | **98.5%** |
///
/// The Message 31 parse this walk exists for is the remainder — 77 M
/// instructions for a whole volume, which agrees with the 50–63 M
/// `vendor/nexrad-decode/VENDORED.md` measures for `decode_messages` plus
/// `into_radial` on a volume of that size.
///
/// An earlier version of this comment gave 92%, from `perf record` sample
/// shares rather than instruction counts. Both are true of different
/// quantities: decompression retires its instructions at a lower IPC than the
/// parse does, so its share of *cycles* is lower than its share of
/// *instructions*. The counted number is the one quoted here.
///
/// A volume is 50–130 LDM records, each **independently** compressed, so that
/// share is embarrassingly parallel at almost perfect granularity — and this is
/// a path every volume open, every timeline scrub, every "next scan", the
/// archive fallback when a chunk feed retires and every frame of a loop
/// download goes through, at 0.9–5.3 billion instructions a volume.
///
/// So [`contribution`] decodes one record on its own, the map runs under
/// [`crate::par`] — rayon on desktop, the sequential stand-in on the web, one
/// code path either way — and [`fold_contributions`] puts the pieces back
/// together **in record order**. Order is the whole of the correctness
/// argument and every part of it is stated there, including why the results
/// come back as a `Vec<Result<_>>` rather than a `Result<Vec<_>>`.
///
/// Nothing about the answer changes, and it is not meant to. The map is
/// order-preserving —
/// [`tests::the_map_collects_in_input_order_however_the_workers_finish`] holds
/// that down rather than assuming it — `fold_contributions` is where every
/// "first wins" in the walk is decided and the fixtures below pin it, and
/// [`tests::live_one_pass_decode_matches_the_two_pass_decode`] — `#[ignore]`d,
/// so not part of a default run — still holds the
/// whole thing against `File::scan()` and against
/// [`crate::nyquist::DeclaredNyquist::from_archive`]'s own serial walk on a
/// real volume.
///
/// # What does change: the work done on the way to an error
///
/// The answer is the serial walk's; the *effort* spent reaching it is not. The
/// serial walk stopped at the first malformed record, and this one decodes
/// every record before the fold ever looks at the first `Err` — a volume
/// corrupt at record 2 of 60 now costs a whole parallel decode, measured at
/// 23.5 ms against the 3 ms the serial walk took to give up. Three consequences,
/// all of them small and all of them deliberate:
///
/// * wasted work on a path that is already failing, which is the price of the
///   ordered error and cheap next to what the success path saves;
/// * a panic in a *later* record now surfaces where the serial walk would have
///   returned a clean `Err` from an earlier one, because the later record is
///   now reached;
/// * a decompression bomb in a later record is now decompressed, and up to
///   `threads` of them at once. Archive volumes come from the NWS bucket over
///   TLS, so this is a property to know rather than a threat model that
///   changed.
///
/// # One pool, shared with the renderer
///
/// This is rayon's *global* pool, which is also where [`crate::render`] and
/// [`crate::voxel`] run their `par_iter`s — some of them from the frame thread.
/// A render issued while a decode is in flight can now wait for a worker to
/// finish the record it is holding, which is one bzip2 decompress, 1–8 ms. The
/// decode window it might land in is also ~10× shorter than it was, so the
/// integral is very likely better; it is written down because it is a new
/// interaction, not because it has been observed to hurt.
fn decoded(file: &nexrad_data::volume::File) -> Result<DecodedScan> {
    use crate::par::*;

    // `Vec<Result<_>>`, not `Result<Vec<_>>`: rayon's `Result` collect keeps
    // whichever error a worker tripped on first, and the error this reports has
    // to be the first one in the *file*. `into_par_iter` is `crate::par`'s —
    // rayon's off wasm32, a plain `into_iter` on it, so the browser walks the
    // same records in the same order through the same fold.
    let contributions = file
        .records()?
        .into_par_iter()
        .map(contribution)
        .collect::<Vec<_>>();

    fold_contributions(file, contributions)
}

/// The site's location, as one Message 31's volume data block states it.
///
/// Stated on every radial; the first one wins, as it does in `scan()`.
struct SiteLocation {
    latitude: f32,
    longitude: f32,
    site_height: i16,
    tower_height: u16,
}

/// What one LDM record contributes to the volume.
///
/// Per-record accumulators rather than shared ones. The radial `Vec` is
/// order-dependent, and a table shared behind a lock would both serialise the
/// decode it exists to parallelise and turn "first radial wins" into "whichever
/// thread got there first". Everything here is folded in record order by
/// [`fold_contributions`].
struct RecordContribution {
    declared_nyquist: crate::nyquist::DeclaredNyquist,
    radials: Vec<nexrad_model::data::Radial>,
    coverage_pattern: Option<nexrad_model::data::VolumeCoveragePattern>,
    site_location: Option<SiteLocation>,
}

/// One record's decompress-and-decode: the body of what used to be the walk's
/// inner loop, lifted out so it can run on a worker.
///
/// The error type is `nexrad_data`'s rather than [`ScanError`] because every
/// failure reachable in here is one of upstream's — which is what keeps the
/// variant a caller sees the same one `scan()` raised — and because it is what
/// a worker has to be able to carry back.
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
                // Before `into_radial`, which is where the number is lost.
                out.declared_nyquist.declare_from_message(&m);
                // Through `nexrad_data`'s error rather than straight to
                // `ScanError`, so a decode failure here is the same variant it
                // was when `scan()` raised it.
                out.radials
                    .push(m.into_radial().map_err(nexrad_data::result::Error::from)?);
            }
            // First one wins, as in `scan()`: a repeat of message 5 inside one
            // volume is the same pattern restated. First *in this record* here;
            // the fold keeps the first record that has one, which is the same
            // volume-wide answer.
            MessageContents::VolumeCoveragePattern(m) if out.coverage_pattern.is_none() => {
                out.coverage_pattern = Some(crate::chunks::coverage_pattern_from(&m));
            }
            _ => {}
        }
    }

    Ok(out)
}

/// Fold the per-record results back into one volume, **in record order**.
///
/// Order is the whole of the correctness argument for decoding the records
/// apart, and this function is where all of it lives:
///
/// * **Errors fold in record order.** rayon's `Result` collect keeps whichever
///   error a worker reached first, so a volume with two malformed records could
///   report either — a race, and one that would surface as a flaky error
///   *variant* rather than a flaky decode. Taking the first `Err` walking the
///   `Vec` in record order reports the first failure in the *file*, which is
///   the one the serial walk reported. This is the failure mode
///   `rustdar-radar/Cargo.toml` warns about where it enables
///   `nexrad-data/parallel`.
/// * **The Nyquist table.** [`crate::nyquist::DeclaredNyquist::declare`] is
///   first-writer-wins, so each record's own table names a cut with its
///   earliest radial, and replaying those tables in record order into one
///   accumulator names each cut with the earliest radial in the volume —
///   exactly what one serial walk produced.
/// * **Radials** are `extend`ed in record order, which is the order the serial
///   walk `push`ed them in, and `Sweep::from_radials` is fed the same `Vec`.
/// * **The site and the coverage pattern** take the first record that states
///   one, which is the first radial and the first message 5 in the file.
///
/// It takes the whole `Vec` rather than an iterator of `Result`s because the
/// map has to run to completion anyway — rayon has already spent the work by
/// the time the first error is visible — and because short-circuiting on the
/// first `Err` would leave the reported error dependent on how far the caller
/// got, which is the property this exists to remove.
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
