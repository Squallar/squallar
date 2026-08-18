//! HRRR data fetching from the `noaa-hrrr-bdp-pds` S3 bucket.
//!
//! Replaces the NOMADS filter CGI
//! (`nomads.ncep.noaa.gov/cgi-bin/filter_hrrr_2d.pl`), which answers `200` with
//! no `Access-Control-Allow-Origin` at all (verified 2026-07-25 with
//! `curl -H 'Origin: …'`) and is therefore unreachable from the web build.
//!
//! S3 serves whole files, but every HRRR GRIB2 file has an `.idx` sidecar —
//! ~9 KB of text listing each record's byte offset:
//!
//! ```text
//! 105:63110198:d=2026072514:CAPE:surface:anl:
//! 106:63976324:d=2026072514:CIN:surface:anl:
//! 107:64861905:d=2026072514:PWAT:entire atmosphere (considered as a single layer):anl:
//! ```
//!
//! so subsetting becomes: fetch the index, find the record, `Range`-request the
//! bytes to the next offset. Two requests, but the large one is *smaller* than
//! NOMADS' — 1.03 MB against 2.27 MB for the same field, measured.
//!
//! That difference is packing. The old request carried `subregion=`, which made
//! NOMADS re-encode through wgrib2, turning data representation template **5.3**
//! (complex packing with spatial differencing) into **5.0** (simple packing) and
//! re-rounding `Lo1` from 237280472 to 237280471 microdegrees. S3 serves the
//! operational bytes, so the live path decodes **5.3** and `Lo1` 237280472; a
//! test constant taken from a NOMADS download is off by one microdegree. grib
//! handles 5.0 and 5.3 in pure Rust — neither needs the JPEG2000 or CCSDS
//! features this crate drops.
//!
//! Dropping `subregion` did **not** enlarge the grid: NOMADS never subset
//! Lambert-conformal grids, so both paths return the full 1799x1059 CONUS grid
//! (1,905,141 points), and `parse_grib2` derives bounds from the grid it is
//! handed rather than from a requested region.

use chrono::{NaiveDate, NaiveDateTime, Timelike, Utc};
use grib::{Grib2SubmessageDecoder, GridDefinitionTemplateValues, LatLons, SubMessage};
use rustdar_source::origins::DataSources;

use super::{GridCoords, HrrrFetchResult, HrrrGridData, ModelParameter, lambert};
use crate::fetch_policy::{FetchError, FetchFailure, NotFound};
use crate::types::GeoBounds;

/// Live tests only. Production fetches with `ctx.client` (30 s).
///
/// Gated off wasm32 with the module that uses it, or it would be dead there.
#[cfg(all(test, not(target_arch = "wasm32")))]
const HRRR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

// ---------------------------------------------------------------------------
// The `.idx` sidecar
// ---------------------------------------------------------------------------

/// One line of a GRIB2 `.idx` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdxRecord {
    /// 1-based record number.
    pub number: usize,
    /// Byte offset of this record within the GRIB2 file.
    pub offset: u64,
    /// Variable abbreviation, e.g. `CIN`.
    pub var: String,
    /// Level description, e.g. `180-0 mb above ground`.
    pub level: String,
    /// Forecast description, e.g. `anl` or `0-1 hour max fcst`.
    pub forecast: String,
}

/// `number:offset:d=YYYYMMDDHH:VAR:LEVEL:FORECAST:`
///
/// No HRRR level field contains a colon, though many contain spaces,
/// parentheses and hyphens, so fields are taken by index. Malformed lines are
/// skipped; a trailing blank line is normal.
pub fn parse_idx(text: &str) -> Vec<IdxRecord> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_end_matches(':').trim();
            if line.is_empty() {
                return None;
            }
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 6 {
                return None;
            }
            Some(IdxRecord {
                number: fields[0].parse().ok()?,
                offset: fields[1].parse().ok()?,
                var: fields[3].to_string(),
                level: fields[4].to_string(),
                forecast: fields[5].to_string(),
            })
        })
        .collect()
}

/// The inclusive byte range holding one record. The final record has no
/// successor, so its end is `None` and the caller asks for an open-ended range.
///
/// Matches on `var` **and** `level`: HRRR carries five `CAPE` records at
/// different levels and two `CIN`s, and `surface` appears on dozens of
/// variables.
///
/// **`(var, level)` is not unique**, and the tie-break is positional (first,
/// i.e. lowest-numbered, match). A real `wrfsfcf01` index repeats two pairs,
/// distinguished only by the forecast description this function ignores:
///
/// ```text
///  8:…:REFD:263 K level:1 hour fcst:        44:…:REFD:263 K level:0-1 hour max fcst:
/// 68:…:WEASD:surface:1 hour fcst:           85:…:WEASD:surface:0-1 hour acc fcst:
/// ```
///
/// rustdar requests neither, and
/// `live_every_parameter_selects_exactly_one_record` checks that against the
/// live index rather than assuming it — taking the instantaneous `REFD` where a
/// caller wanted the maximum is the same quiet class of error as the
/// constant-zero f00 `MXUPHL`. [`IdxRecord::forecast`] is carried as the
/// disambiguator for a parameter whose pair does repeat.
pub fn byte_range(records: &[IdxRecord], var: &str, level: &str) -> Option<(u64, Option<u64>)> {
    let idx = records
        .iter()
        .position(|r| r.var == var && r.level == level)?;
    let start = records[idx].offset;
    let end = records.get(idx + 1).map(|next| next.offset - 1);
    Some((start, end))
}

// ---------------------------------------------------------------------------
// Run selection
// ---------------------------------------------------------------------------

/// Determine the most recent HRRR run hour that should be available.
///
/// HRRR appears on S3 ~45-90 min after the run time. Two hours back is a safe
/// default, and `fetch_hrrr_data` falls back another hour on failure.
fn latest_available_run() -> (NaiveDate, u8) {
    let now = Utc::now().naive_utc();
    let safe_time = now - chrono::Duration::hours(2);
    (safe_time.date(), safe_time.time().hour() as u8)
}

/// The run one hour before this one, rolling back over midnight.
///
/// `hour` is a `u8`, so a bare `hour - 1` panics in debug and wraps to 255 for
/// the 00Z run — which [`latest_available_run`] returns for the whole
/// 02:00-02:59 UTC hour, every day.
fn previous_run(date: NaiveDate, hour: u8) -> (NaiveDate, u8) {
    if hour == 0 {
        (date - chrono::Duration::days(1), 23)
    } else {
        (date, hour - 1)
    }
}

// ---------------------------------------------------------------------------
// GRIB2 decoding
// ---------------------------------------------------------------------------

/// How to get the lat/lon of any grid point of a submessage, in scanning-mode
/// order.
///
/// `grib` is built with `default-features = false` (no C/C++), which drops
/// `gridpoints-proj` and with it the only `latlons()` for template 3.30 —
/// grib returns `NotSupported`. HRRR is 3.30 for every field, so without the
/// [`lambert`] branch below every HRRR fetch fails here. Other templates still
/// go through grib, which needs no PROJ for them, and are materialised because
/// there is nothing here to recompute them from.
fn grid_coords<R>(submessage: &SubMessage<'_, R>) -> Result<GridCoords, String> {
    let grid_def = submessage.grid_def();
    let template = GridDefinitionTemplateValues::try_from(grid_def)
        .map_err(|e| format!("Cannot read grid definition: {e}"))?;

    match template {
        GridDefinitionTemplateValues::Template30(ref lambert_grid) => {
            let geometry = lambert::LambertGrid::from_template(lambert_grid)?;
            check_point_count(geometry.len(), grid_def.num_points() as usize)?;
            Ok(GridCoords::Lambert(geometry))
        }
        _ => {
            let (lats, lons) = submessage
                .latlons()
                .map_err(|e| format!("Cannot compute grid lat/lons: {e}"))?
                .map(|(lat, lon)| (f64::from(lat), f64::from(lon)))
                .unzip();
            Ok(GridCoords::Explicit { lats, lons })
        }
    }
}

/// The grid we walked must hold exactly as many points as section 3 declares.
///
/// A mismatch means it is not the grid the data was packed against, and the
/// values would then be laid out over the wrong coordinates — a plausible field
/// in the wrong places, which looks like weather.
fn check_point_count(computed: usize, declared: usize) -> Result<(), String> {
    if computed != declared {
        return Err(format!(
            "Lambert grid point count mismatch: {declared} declared in \
             section 3 vs {computed} computed",
        ));
    }
    Ok(())
}

/// `!= 1`, not `< 1`. Zero means the range delimited nothing; **two** means it
/// spanned a record boundary, which is the dangerous one — concatenated records
/// decode fine as a sequence, and taking the first produces a plausible grid for
/// the wrong field. Relaxing to `< 1` restores that bug.
fn exactly_one_submessage(count: usize) -> Result<(), String> {
    if count != 1 {
        return Err(format!(
            "expected exactly one GRIB2 submessage, found {count} - the byte \
             range does not delimit a single record",
        ));
    }
    Ok(())
}

/// The longitude envelope `rasterize_model_data`'s frame handling has actually
/// been shown correct for.
///
/// HRRR CONUS measures -134.0955..-60.9172, corner-verified against real GRIB2
/// section 3 in `hrrr::lambert::tests`. This is that with margin either side,
/// and the margin matters in one direction only: the near edge is 40° from the
/// antimeridian, far enough that no viewport containing this domain can also
/// be written in a second longitude frame.
const VALIDATED_DOMAIN_LON: std::ops::RangeInclusive<f64> = -140.0..=-50.0;

/// Marks [`check_domain_longitude`]'s refusal so [`classify_parse_error`] can
/// tell it apart from a genuine decode failure.
///
/// A string rather than a typed error only because `parse_grib2` reports every
/// other fault as one too; the coupling is pinned by
/// `the_domain_refusal_is_classified_permanent` rather than left to a reader
/// noticing both ends.
const DOMAIN_REFUSAL_MARK: &str = "unsupported model domain";

/// A `parse_grib2` error, classified for the retry ladder.
///
/// Everything `parse_grib2` rejects is transient — a truncated or mis-ranged
/// record is worth another go — *except* a domain the renderer cannot place,
/// which will be exactly as unplaceable next time.
fn classify_parse_error(message: String) -> FetchError {
    if message.contains(DOMAIN_REFUSAL_MARK) {
        FetchError::permanent(message)
    } else {
        FetchError::transient(message)
    }
}

/// Refuse a model domain whose longitude the renderer has never been shown to
/// place correctly — and say what decision the refusal is asking for.
///
/// This is a guard against a state that decays *silently*. Today
/// `DataSources::hrrr_url` hardcodes `/conus/`, so the only domain that can
/// reach here is inside [`VALIDATED_DOMAIN_LON`] by 40°, and this never fires.
/// The day a non-CONUS domain is added it fires immediately, at the moment and
/// in front of the person who can choose correctly — rather than shipping a
/// layer that silently paints nothing.
///
/// It is deliberately a refusal and not a log line: a warning nobody can act
/// on is a defect, and this one has exactly one reader, who is mid-change.
///
/// The refusal carries [`DOMAIN_REFUSAL_MARK`] so the fetch layer can classify
/// it [`FetchFailure::Permanent`]. A domain does not become placeable by being
/// asked for again, and the default for a parse error here is `Transient` —
/// which would retry a configuration mistake on the backoff ladder forever and
/// present it as though the network were at fault.
fn check_domain_longitude(bounds: &GeoBounds) -> Result<(), String> {
    // `min > max` and not just `!is_finite`: the bounds walk above seeds
    // `min_lon` at `f64::MAX` and `max_lon` at `f64::MIN`, and both of those
    // *are* finite, so a grid whose coordinates could not be walked arrives
    // here as an inverted extent rather than a NaN one. Reported as itself,
    // because it is a decode problem and the message below is not about
    // decoding.
    if !bounds.min_lon.is_finite() || !bounds.max_lon.is_finite() || bounds.min_lon > bounds.max_lon
    {
        return Err(format!(
            "Model grid reported a non-finite or inverted longitude extent \
             ({}..{}); its coordinates could not be walked",
            bounds.min_lon, bounds.max_lon
        ));
    }
    if VALIDATED_DOMAIN_LON.contains(&bounds.min_lon)
        && VALIDATED_DOMAIN_LON.contains(&bounds.max_lon)
    {
        return Ok(());
    }
    Err(format!(
        "{DOMAIN_REFUSAL_MARK}: spans longitude {:.4}..{:.4}, outside the \
         {:.1}..{:.1} envelope the renderer's longitude handling has been \
         validated for (HRRR CONUS is -134.0955..-60.9172).\n\
         \n\
         This is not a decode failure - the grid decoded fine. The parse side \
         folds longitude into [-180,180] (`lambert::normalize_longitude_degrees`) \
         while the viewport is deliberately left unfolded \
         (`OverlayTexturePlan::coverage`). For a domain near or across the \
         antimeridian the two disagree by a whole turn: measured on a grid \
         parked at the seam, identical ground paints 3294 pixels written one \
         way and 0 written the other. A layer added without resolving this \
         renders blank, with nothing to say why.\n\
         \n\
         The repair was left unbuilt on purpose, because three candidates are \
         each correct for a different domain shape and nothing in the tree \
         said which this domain needs:\n\
         \x20 * a rigid whole-grid shift - exact only if the domain does not \
         straddle the antimeridian;\n\
         \x20 * a per-point shift - tears cells, because `rasterize_model_data` \
         sizes every cell from its neighbours' pixel spacing, so two adjacent \
         points shifted differently stretch one cell across the texture;\n\
         \x20 * `GridCoords::wraps_longitude` - guards the index window, not \
         this, and measures `false` for a seam-parked grid.\n\
         \n\
         Whoever added this domain is the person who can choose between them. \
         The measurement, the instrument (`seam_probe.rs`) and the reasoning \
         are in `campaigns/overlays/t17/` on the `campaign-harness` branch. \
         Widen VALIDATED_DOMAIN_LON only together with the repair the new \
         domain's shape calls for.",
        bounds.min_lon,
        bounds.max_lon,
        VALIDATED_DOMAIN_LON.start(),
        VALIDATED_DOMAIN_LON.end(),
    ))
}

/// Parse GRIB2 bytes into `HrrrGridData`.
///
/// The bytes must be exactly one record with exactly one submessage, and that is
/// checked rather than assumed: NOMADS guaranteed it server-side, byte-ranging
/// guarantees it only via [`byte_range`]'s arithmetic, and an off-by-one there
/// delivers two records that decode to a plausible grid for the wrong field.
fn parse_grib2(bytes: &[u8], param: ModelParameter) -> Result<HrrrGridData, String> {
    let grib2 = grib::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| format!("GRIB2 parse error: {e}"))?;

    // Its own pass, before any `SubMessage` is held: grib's iterator borrows the
    // reader through a `RefCell`, so advancing it while a submessage is alive
    // panics with "RefCell already borrowed".
    exactly_one_submessage(grib2.iter().count())?;

    let (_index, submessage) = grib2
        .iter()
        .next()
        .ok_or_else(|| "No submessages in GRIB2 data".to_string())?;

    // Borrows submessage, releases here.
    let coords = grid_coords(&submessage)?;

    let (ni, nj) = submessage
        .grid_shape()
        .map_err(|e| format!("Cannot determine grid shape: {e}"))?;

    // Read before the submessage is consumed for decoding. A malformed reference
    // time is a hard error: `unwrap_or_default()` gives 1970-01-01 00:00, which
    // the pane control renders as "Model Data (00:00z)" — corrupt data made to
    // look merely oddly-timed.
    let raw_time = submessage.temporal_raw_info();
    let t = &raw_time.ref_time_unchecked;
    let ref_date = NaiveDate::from_ymd_opt(t.year as i32, t.month as u32, t.day as u32)
        .ok_or_else(|| {
            format!(
                "GRIB2 reference date is not a real date: {}-{:02}-{:02}",
                t.year, t.month, t.day
            )
        })?;
    let ref_clock =
        chrono::NaiveTime::from_hms_opt(t.hour as u32, t.minute as u32, t.second as u32)
            .ok_or_else(|| {
                format!(
                    "GRIB2 reference time is not a real time: {:02}:{:02}:{:02}",
                    t.hour, t.minute, t.second
                )
            })?;
    let ref_time = NaiveDateTime::new(ref_date, ref_clock);

    // S3 serves the operational DRT 5.3 (complex packing with spatial
    // differencing); NOMADS re-encoded to 5.0. `dispatch()` picks by template
    // and both are pure Rust in grib, but this is the line that fails if the
    // feature set in Cargo.toml is trimmed further.
    let decoder =
        Grib2SubmessageDecoder::from(submessage).map_err(|e| format!("Decode init error: {e}"))?;
    let values: Vec<f32> = decoder
        .dispatch()
        .map_err(|e| format!("Decode error: {e}"))?
        .collect();

    if values.is_empty() {
        return Err("No grid points decoded from GRIB2".into());
    }

    // One streaming pass for the bounds: nothing is retained, so the 30 MB of
    // coordinates this used to build never exists.
    let Some(bounds) = GeoBounds::from_points((0..coords.len()).map_while(|i| coords.at(i))) else {
        return Err("GRIB2 grid decoded no coordinates".into());
    };
    // Here, not in the rasterizer: this is where a *domain* first states its
    // extent, and the person who changed the domain is standing here.
    check_domain_longitude(&bounds)?;

    let (visible_points, value_range) = super::summarize_values(&values, param);

    Ok(HrrrGridData {
        parameter: param,
        values,
        coords,
        ni,
        nj,
        bounds,
        ref_time,
        forecast_hour: param.forecast_hour(),
        visible_points,
        value_range,
    })
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

/// Fetch one GRIB2 record: index, locate, range-request.
async fn fetch_record(
    client: &reqwest::Client,
    sources: &DataSources,
    date: NaiveDate,
    run_hour: u8,
    forecast_hour: u8,
    var: &str,
    level: &str,
) -> Result<Vec<u8>, FetchError> {
    let idx_url = sources.hrrr_idx_url(&date, run_hour, forecast_hour);
    // `IsRoutine`: the bucket holds a rolling window of runs and each run's
    // files land over several minutes, so a 404 here is "that run is not up
    // (yet, or any more)" — a normal answer for a model, and the reason this
    // function has a previous-hour fallback at all. It must not read as a
    // refusal.
    let idx_response = client
        .get(&idx_url)
        .send()
        .await
        .map_err(|e| FetchError::from_transport(&e, format!("index request failed: {e}")))?;
    // Checked here rather than through `error_for_status()`, which funnels into
    // `NotFound::IsBroken` and would read a not-yet-published run as a refusal.
    if !idx_response.status().is_success() {
        return Err(FetchError::from_status(
            idx_response.status(),
            NotFound::IsRoutine,
            format!("index {idx_url}: HTTP {}", idx_response.status()),
        ));
    }
    let idx_text = idx_response
        .text()
        .await
        .map_err(|e| FetchError::from_transport(&e, format!("index body read failed: {e}")))?;

    let records = parse_idx(&idx_text);
    if records.is_empty() {
        return Err(FetchError::transient(format!(
            "{idx_url} parsed to no records"
        )));
    }

    let (start, end) = byte_range(&records, var, level).ok_or_else(|| {
        FetchError::transient(format!(
            "no `{var}:{level}` record in {idx_url} ({} records)",
            records.len()
        ))
    })?;

    let grib_url = sources.hrrr_grib_url(&date, run_hour, forecast_hour);
    let range = match end {
        Some(end) => format!("bytes={start}-{end}"),
        None => format!("bytes={start}-"),
    };
    log::info!("Fetching HRRR {var}:{level} from {grib_url} [{range}]");

    let response = client
        .get(&grib_url)
        .header(reqwest::header::RANGE, &range)
        .send()
        .await
        .map_err(|e| FetchError::from_transport(&e, format!("range request failed: {e}")))?;

    // A 200 means the server ignored `Range` and is sending the whole 130 MB.
    if response.status() == reqwest::StatusCode::OK {
        return Err(FetchError::transient(format!(
            "{grib_url} ignored the Range header and would return the whole file"
        )));
    }
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(FetchError::from_status(
            response.status(),
            NotFound::IsRoutine,
            format!("HTTP {} for {grib_url}", response.status()),
        ));
    }

    let bytes = response.bytes().await.map_err(|e| {
        FetchError::from_transport(&e, format!("Failed to read response body: {e}"))
    })?;
    log::info!(
        "Received {} bytes of GRIB2 data for {var}:{level}",
        bytes.len()
    );
    Ok(bytes.to_vec())
}

/// Fetch HRRR model data for the given parameter.
///
/// Tries the latest available run first; if that fails, falls back to the
/// previous hour.
pub async fn fetch_hrrr_data(
    client: &reqwest::Client,
    sources: &DataSources,
    param: &ModelParameter,
) -> HrrrFetchResult {
    let (date, hour) = latest_available_run();

    let first = match try_fetch(client, sources, param, date, hour).await {
        Ok(data) => return HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::warn!("HRRR fetch for {date} {hour:02}z failed: {e}, trying previous hour");
            e
        }
    };

    let (prev_date, prev_hour) = previous_run(date, hour);

    match try_fetch(client, sources, param, prev_date, prev_hour).await {
        Ok(data) => HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::error!("HRRR fallback fetch also failed: {e}");
            HrrrFetchResult(Err(round_verdict(
                [first, e],
                "HRRR fetch failed for both candidate runs",
            )))
        }
    }
}

/// One verdict for a two-run attempt, carrying both attempts' words.
///
/// The two runs are the same product an hour apart, so either succeeding fixes
/// the layer — which is exactly the shape [`FetchFailure::of_round`] is for, and
/// "refused only if every part was" is the right rule here for a sharper reason
/// than usual: the fallback is the *older* run, so it is the one **less** likely
/// to be missing. A refusal that survives both is a refusal of the product.
///
/// # Why an all-404 round does not stay routine
///
/// A single run 404ing is genuinely routine — the bucket carries a rolling
/// window and each run's files land over several minutes, which is the whole
/// reason this fallback exists. So [`fetch_record`] classifies its 404s
/// [`IsRoutine`](NotFound::IsRoutine), and a lone one reads as
/// [`Absent`](FetchFailure::Absent): "not published right now", ladder reset,
/// ordinary hourly poll, no fault reported. Correct.
///
/// But the bucket should always carry *at least one* of the last two hourly
/// runs. Both missing is not the publication schedule; it is a moved path, a
/// renamed product, or an outage. Left as `Absent` that state is invisible for
/// ever — `Absent` resets the ladder, stamps the clock and reports no fault, so
/// a permanently moved HRRR would poll hourly and say nothing, which is the
/// same silence this module exists to end, just wearing a friendlier verdict.
///
/// Escalated to [`Transient`](FetchFailure::Transient) rather than
/// [`Permanent`](FetchFailure::Permanent) deliberately: a 404 alone cannot tell
/// a moved product from an outage, and `Transient` costs the ceiling — one poll
/// an hour, what a healthy HRRR costs — while still surfacing as "not loading"
/// in the layer's own panel. Claiming a refusal here would be claiming more
/// than two 404s can prove.
fn round_verdict(parts: [FetchError; 2], context: &str) -> FetchError {
    let mut round = FetchError::of_round(&parts, context);
    if round.failure == FetchFailure::Absent {
        round.failure = FetchFailure::Transient;
    }
    round
}

/// Attempt a single HRRR fetch for a specific run.
async fn try_fetch(
    client: &reqwest::Client,
    sources: &DataSources,
    param: &ModelParameter,
    date: NaiveDate,
    hour: u8,
) -> Result<HrrrGridData, FetchError> {
    let bytes = fetch_record(
        client,
        sources,
        date,
        hour,
        param.forecast_hour(),
        param.grib_var(),
        param.grib_level(),
    )
    .await?;
    // A decode failure is transient by the module's own rule: a truncated body
    // and a changed product encoding are indistinguishable from one sample.
    parse_grib2(&bytes, *param).map_err(classify_parse_error)
}

/// Fetch a composite HRRR parameter (e.g. bulk shear) that requires
/// multiple fields merged into one grid.
pub async fn fetch_composite_hrrr_data(
    client: &reqwest::Client,
    sources: &DataSources,
    param: &ModelParameter,
) -> HrrrFetchResult {
    let parts = match param.composite_parts() {
        Some(p) => p,
        None => return fetch_hrrr_data(client, sources, param).await,
    };

    let (date, hour) = latest_available_run();

    let first = match try_fetch_composite(client, sources, param, &parts, date, hour).await {
        Ok(data) => return HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::warn!(
                "HRRR composite fetch for {date} {hour:02}z failed: {e}, trying previous hour"
            );
            e
        }
    };

    let (prev_date, prev_hour) = previous_run(date, hour);

    match try_fetch_composite(client, sources, param, &parts, prev_date, prev_hour).await {
        Ok(data) => HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::error!("HRRR composite fallback fetch also failed: {e}");
            HrrrFetchResult(Err(round_verdict(
                [first, e],
                "HRRR composite fetch failed for both candidate runs",
            )))
        }
    }
}

/// Attempt a composite HRRR fetch for a specific run.
async fn try_fetch_composite(
    client: &reqwest::Client,
    sources: &DataSources,
    param: &ModelParameter,
    parts: &[(&str, &str)],
    date: NaiveDate,
    hour: u8,
) -> Result<HrrrGridData, FetchError> {
    let mut grids: Vec<HrrrGridData> = Vec::with_capacity(parts.len());

    for (var, level) in parts {
        let bytes = fetch_record(
            client,
            sources,
            date,
            hour,
            param.forecast_hour(),
            var,
            level,
        )
        .await?;
        grids.push(parse_grib2(&bytes, *param).map_err(classify_parse_error)?);
    }

    if grids.len() < 2 {
        return Err(FetchError::transient(
            "Composite requires at least 2 components",
        ));
    }

    // Merge: compute magnitude √(a² + b²) element-wise.
    let base = &grids[0];
    let other = &grids[1];

    if base.values.len() != other.values.len() {
        return Err(FetchError::transient(format!(
            "Grid size mismatch: {} vs {}",
            base.values.len(),
            other.values.len()
        )));
    }

    let values: Vec<f32> = base
        .values
        .iter()
        .zip(other.values.iter())
        .map(|(&u, &v)| (u * u + v * v).sqrt())
        .collect();

    // Recomputed from the merged magnitudes: each component's own summary says
    // nothing about the vector magnitude the user sees.
    let (visible_points, value_range) = super::summarize_values(&values, *param);

    Ok(HrrrGridData {
        parameter: *param,
        values,
        coords: base.coords.clone(),
        ni: base.ni,
        nj: base.nj,
        bounds: base.bounds,
        ref_time: base.ref_time,
        forecast_hour: base.forecast_hour,
        visible_points,
        value_range,
    })
}

/// The client the **live tests in this module** use, `#[cfg(test)]` so it cannot
/// be mistaken for production (which passes `ctx.client`, timeout 30 s).
///
/// A `User-Agent` is fine on this origin, unlike IEM and SPC: S3 answers the
/// preflight `200` with `Access-Control-Allow-Headers: user-agent`. See
/// `rustdar_source::origins`.
#[cfg(all(test, not(target_arch = "wasm32")))]
fn hrrr_client() -> Result<reqwest::Client, String> {
    rustdar_source::tls::client(rustdar_source::tls::USER_AGENT, HRRR_TIMEOUT)
        .build()
        .map_err(|e| format!("could not build the HRRR client: {e}"))
}

// Native-only: the live fetches at the tail are `#[tokio::test]`, and that
// dev-dependency is target-gated off wasm32.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
