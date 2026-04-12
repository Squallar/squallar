//! HRRR data fetching from NOAA NOMADS server-side filter.
//!
//! Uses the NOMADS filter CGI to download a single GRIB2 field (e.g. CIN)
//! for the latest HRRR f00 (analysis) run, keeping download size small
//! (~200-500 KB).

use chrono::{NaiveDate, NaiveDateTime, Timelike, Utc};
use grib::{Grib2SubmessageDecoder, LatLons};

use super::{HrrrFetchResult, HrrrGridData, ModelParameter};
use crate::types::GeoBounds;

/// CONUS subregion bounds for the NOMADS filter request.
const SUBREGION_TOP_LAT: f64 = 50.0;
const SUBREGION_BOT_LAT: f64 = 20.0;
const SUBREGION_LEFT_LON: f64 = -130.0;
const SUBREGION_RIGHT_LON: f64 = -60.0;

/// Build the NOMADS filter URL for a specific HRRR run and parameter.
fn nomads_url(param: &ModelParameter, run_hour: u8, date: NaiveDate) -> String {
    let date_str = date.format("%Y%m%d");
    format!(
        "https://nomads.ncep.noaa.gov/cgi-bin/filter_hrrr_2d.pl\
         ?dir=%2Fhrrr.{date_str}%2Fconus\
         &file=hrrr.t{run_hour:02}z.wrfsfcf00.grib2\
         &{var}=on\
         &{lev}=on\
         &subregion=\
         &toplat={top}\
         &leftlon={left}\
         &rightlon={right}\
         &bottomlat={bot}",
        var = param.nomads_var(),
        lev = param.nomads_level(),
        top = SUBREGION_TOP_LAT,
        bot = SUBREGION_BOT_LAT,
        left = SUBREGION_LEFT_LON,
        right = SUBREGION_RIGHT_LON,
    )
}

/// Determine the most recent HRRR run hour that should be available.
///
/// HRRR data typically appears on NOMADS ~45-90 min after the run time.
/// We go back 2 hours from now as a safe default.
fn latest_available_run() -> (NaiveDate, u8) {
    let now = Utc::now().naive_utc();
    let safe_time = now - chrono::Duration::hours(2);
    let date = safe_time.date();
    let hour = safe_time.time().hour() as u8;
    (date, hour)
}

/// Parse GRIB2 bytes into `HrrrGridData`.
fn parse_grib2(bytes: &[u8], param: ModelParameter) -> Result<HrrrGridData, String> {
    let grib2 = grib::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| format!("GRIB2 parse error: {e}"))?;

    // The filtered file should contain exactly one submessage.
    let (_index, submessage) = grib2
        .iter()
        .next()
        .ok_or_else(|| "No submessages in GRIB2 data".to_string())?;

    // Collect lat/lon grid points first (borrows submessage, releases on collect).
    let latlon_pairs: Vec<(f32, f32)> = submessage
        .latlons()
        .map_err(|e| format!("Cannot compute grid lat/lons: {e}"))?
        .collect();

    // Get grid dimensions.
    let (ni, nj) = submessage
        .grid_shape()
        .map_err(|e| format!("Cannot determine grid shape: {e}"))?;

    // Extract reference time before consuming the submessage for decoding.
    let raw_time = submessage.temporal_raw_info();
    let t = &raw_time.ref_time_unchecked;
    let ref_time = NaiveDateTime::new(
        NaiveDate::from_ymd_opt(t.year as i32, t.month as u32, t.day as u32)
            .unwrap_or_default(),
        chrono::NaiveTime::from_hms_opt(t.hour as u32, t.minute as u32, t.second as u32)
            .unwrap_or_default(),
    );

    // Decode data values (may consume the submessage).
    let decoder =
        Grib2SubmessageDecoder::from(submessage).map_err(|e| format!("Decode init error: {e}"))?;
    let values: Vec<f32> = decoder
        .dispatch()
        .map_err(|e| format!("Decode error: {e}"))?
        .collect();

    if values.is_empty() {
        return Err("No grid points decoded from GRIB2".into());
    }

    // Build coordinate arrays and compute bounds.
    let mut lats = Vec::with_capacity(latlon_pairs.len());
    let mut lons = Vec::with_capacity(latlon_pairs.len());
    let mut min_lat = f64::MAX;
    let mut max_lat = f64::MIN;
    let mut min_lon = f64::MAX;
    let mut max_lon = f64::MIN;

    for &(lat, lon) in &latlon_pairs {
        let lat = lat as f64;
        let lon = lon as f64;
        lats.push(lat);
        lons.push(lon);
        if lat < min_lat { min_lat = lat; }
        if lat > max_lat { max_lat = lat; }
        if lon < min_lon { min_lon = lon; }
        if lon > max_lon { max_lon = lon; }
    }

    let bounds = GeoBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    };

    Ok(HrrrGridData {
        parameter: param,
        values,
        lats,
        lons,
        ni,
        nj,
        bounds,
        ref_time,
    })
}

/// Fetch HRRR model data for the given parameter from NOMADS.
///
/// Tries the latest available run first; if that fails (404), falls back
/// to the previous hour.
pub async fn fetch_hrrr_data(
    client: &reqwest::Client,
    param: &ModelParameter,
) -> HrrrFetchResult {
    let (date, hour) = latest_available_run();

    // Try latest run.
    match try_fetch(client, param, date, hour).await {
        Ok(data) => return HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::warn!("HRRR fetch for {date} {hour:02}z failed: {e}, trying previous hour");
        }
    }

    // Fallback: previous hour (handle midnight rollback).
    let (prev_date, prev_hour) = if hour == 0 {
        (date - chrono::Duration::days(1), 23u8)
    } else {
        (date, hour - 1)
    };

    match try_fetch(client, param, prev_date, prev_hour).await {
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
    param: &ModelParameter,
    date: NaiveDate,
    hour: u8,
) -> Result<HrrrGridData, String> {
    let url = nomads_url(param, hour, date);
    log::info!("Fetching HRRR {} from {url}", param.display_name());

    let response = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response body: {e}"))?;

    log::info!("Received {} bytes of GRIB2 data", bytes.len());

    parse_grib2(&bytes, *param)
}
