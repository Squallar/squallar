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
use rustdar_radar::sources::DataSources;

use super::{GridCoords, HrrrFetchResult, HrrrGridData, ModelParameter, lambert};
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
    let mut min_lat = f64::MAX;
    let mut max_lat = f64::MIN;
    let mut min_lon = f64::MAX;
    let mut max_lon = f64::MIN;

    for index in 0..coords.len() {
        let Some((lat, lon)) = coords.at(index) else {
            break;
        };
        if lat < min_lat {
            min_lat = lat;
        }
        if lat > max_lat {
            max_lat = lat;
        }
        if lon < min_lon {
            min_lon = lon;
        }
        if lon > max_lon {
            max_lon = lon;
        }
    }

    let bounds = GeoBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    };

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
) -> Result<Vec<u8>, String> {
    let idx_url = sources.hrrr_idx_url(&date, run_hour, forecast_hour);
    let idx_text = client
        .get(&idx_url)
        .send()
        .await
        .map_err(|e| format!("index request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("index {idx_url}: {e}"))?
        .text()
        .await
        .map_err(|e| format!("index body read failed: {e}"))?;

    let records = parse_idx(&idx_text);
    if records.is_empty() {
        return Err(format!("{idx_url} parsed to no records"));
    }

    let (start, end) = byte_range(&records, var, level).ok_or_else(|| {
        format!(
            "no `{var}:{level}` record in {idx_url} ({} records)",
            records.len()
        )
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
        .map_err(|e| format!("range request failed: {e}"))?;

    // A 200 means the server ignored `Range` and is sending the whole 130 MB.
    if response.status() == reqwest::StatusCode::OK {
        return Err(format!(
            "{grib_url} ignored the Range header and would return the whole file"
        ));
    }
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(format!("HTTP {} for {grib_url}", response.status()));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response body: {e}"))?;
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

    match try_fetch(client, sources, param, date, hour).await {
        Ok(data) => return HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::warn!("HRRR fetch for {date} {hour:02}z failed: {e}, trying previous hour");
        }
    }

    let (prev_date, prev_hour) = previous_run(date, hour);

    match try_fetch(client, sources, param, prev_date, prev_hour).await {
        Ok(data) => HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::error!("HRRR fallback fetch also failed: {e}");
            HrrrFetchResult(Err(format!("HRRR fetch failed: {e}")))
        }
    }
}

/// Attempt a single HRRR fetch for a specific run.
async fn try_fetch(
    client: &reqwest::Client,
    sources: &DataSources,
    param: &ModelParameter,
    date: NaiveDate,
    hour: u8,
) -> Result<HrrrGridData, String> {
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
    parse_grib2(&bytes, *param)
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

    match try_fetch_composite(client, sources, param, &parts, date, hour).await {
        Ok(data) => return HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::warn!(
                "HRRR composite fetch for {date} {hour:02}z failed: {e}, trying previous hour"
            );
        }
    }

    let (prev_date, prev_hour) = previous_run(date, hour);

    match try_fetch_composite(client, sources, param, &parts, prev_date, prev_hour).await {
        Ok(data) => HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::error!("HRRR composite fallback fetch also failed: {e}");
            HrrrFetchResult(Err(format!("HRRR composite fetch failed: {e}")))
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
) -> Result<HrrrGridData, String> {
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
        grids.push(parse_grib2(&bytes, *param)?);
    }

    if grids.len() < 2 {
        return Err("Composite requires at least 2 components".into());
    }

    // Merge: compute magnitude √(a² + b²) element-wise.
    let base = &grids[0];
    let other = &grids[1];

    if base.values.len() != other.values.len() {
        return Err(format!(
            "Grid size mismatch: {} vs {}",
            base.values.len(),
            other.values.len()
        ));
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
/// `rustdar_radar::sources`.
#[cfg(all(test, not(target_arch = "wasm32")))]
fn hrrr_client() -> Result<reqwest::Client, String> {
    rustdar_radar::tls::client(rustdar_radar::tls::USER_AGENT, HRRR_TIMEOUT)
        .build()
        .map_err(|e| format!("could not build the HRRR client: {e}"))
}

// Native-only: the live fetches at the tail are `#[tokio::test]`, and that
// dev-dependency is target-gated off wasm32.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
