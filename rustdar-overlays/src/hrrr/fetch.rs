//! HRRR data fetching from NOAA NOMADS server-side filter.
//!
//! Uses the NOMADS filter CGI to download a single GRIB2 field (e.g. CIN)
//! from the latest available HRRR run. Most parameters come from f00 (the
//! analysis); updraft helicity comes from f01 because its f00 accumulation
//! window has zero length — see [`ModelParameter::forecast_hour`].
//! Composite parameters (e.g. bulk shear) fetch multiple fields and merge
//! them.
//!
//! Field selection (`var_*`/`lev_*`) is the only filtering that actually
//! takes effect. NOMADS does **not** subset Lambert-conformal grids, so the
//! `subregion`/`toplat`/`leftlon`/`rightlon`/`bottomlat` parameters below do
//! not reduce the area: every response is the full 1799×1059 CONUS grid,
//! 1.7–3.3 MB depending on the field's bit depth. That costs bandwidth but not
//! correctness — `parse_grib2` derives bounds from the grid it is actually
//! handed, never from the requested subregion.
//!
//! They are not, however, *inert*. Passing `subregion` at all makes NOMADS
//! re-encode the record through wgrib2 instead of streaming the operational
//! bytes, which has two visible effects:
//!
//!   * the data representation template changes from 5.3 (complex packing with
//!     spatial differencing) to 5.0 (simple packing) — both pure Rust in grib,
//!     so neither needs the JPEG2000 or CCSDS features this crate drops;
//!   * `Lo1` is re-rounded from 237280472 to 237280471 microdegrees, which
//!     rotates the computed grid by ~1.1e-6° at the far corner. Harmless,
//!     because the projection anchors on whatever `Lo1` the file states, but
//!     see `hrrr::lambert`'s fixture docs before re-deriving any test constant
//!     from a downloaded file.

use chrono::{NaiveDate, NaiveDateTime, Timelike, Utc};
use grib::{Grib2SubmessageDecoder, GridDefinitionTemplateValues, LatLons, SubMessage};

use super::{HrrrFetchResult, HrrrGridData, ModelParameter, lambert};
use crate::types::GeoBounds;

/// CONUS subregion bounds for the NOMADS filter request.
///
/// Inert for HRRR — see the module docs. Kept so the request still expresses
/// the intended area of interest if NOMADS ever gains Lambert subsetting.
const SUBREGION_TOP_LAT: f64 = 50.0;
const SUBREGION_BOT_LAT: f64 = 20.0;
const SUBREGION_LEFT_LON: f64 = -130.0;
const SUBREGION_RIGHT_LON: f64 = -60.0;

/// Build the NOMADS filter URL for a specific HRRR run and parameter.
///
/// The forecast hour comes from the parameter, not the caller: it is a
/// property of what the field *means*, not of when we happen to ask.
fn nomads_url(param: &ModelParameter, run_hour: u8, date: NaiveDate) -> String {
    nomads_url_raw(
        param.nomads_var(),
        param.nomads_level(),
        run_hour,
        date,
        param.forecast_hour(),
    )
}

/// Build a NOMADS filter URL from explicit var/lev strings.
fn nomads_url_raw(
    var: &str,
    lev: &str,
    run_hour: u8,
    date: NaiveDate,
    forecast_hour: u8,
) -> String {
    let date_str = date.format("%Y%m%d");
    format!(
        "https://nomads.ncep.noaa.gov/cgi-bin/filter_hrrr_2d.pl\
         ?dir=%2Fhrrr.{date_str}%2Fconus\
         &file=hrrr.t{run_hour:02}z.wrfsfcf{forecast_hour:02}.grib2\
         &{var}=on\
         &{lev}=on\
         &subregion=\
         &toplat={top}\
         &leftlon={left}\
         &rightlon={right}\
         &bottomlat={bot}",
        top = SUBREGION_TOP_LAT,
        bot = SUBREGION_BOT_LAT,
        left = SUBREGION_LEFT_LON,
        right = SUBREGION_RIGHT_LON,
    )
}

/// URLs for each component of a composite parameter, in merge order.
///
/// Split out from the fetch loop so the forecast hour reaching the wire is
/// testable without a network round trip — the single-field path had that
/// covered and this one did not.
fn composite_urls(
    param: &ModelParameter,
    parts: &[(&str, &str)],
    run_hour: u8,
    date: NaiveDate,
) -> Vec<String> {
    parts
        .iter()
        .map(|(var, lev)| nomads_url_raw(var, lev, run_hour, date, param.forecast_hour()))
        .collect()
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

/// Lat/lon of every grid point of a submessage, in scanning-mode order.
///
/// `grib` is built with `default-features = false` so it links no C or C++ —
/// see `rustdar-overlays/Cargo.toml`. That drops its `gridpoints-proj` feature,
/// and with it the *only* implementation of `latlons()` for grid definition
/// template 3.30 (Lambert conformal): grib returns `NotSupported` instead.
///
/// HRRR is template 3.30 for every field, so without the branch below every
/// HRRR fetch would fail here. [`lambert::latlons`] is the pure-Rust
/// replacement. Any other template still goes through grib, which handles the
/// regular lat/lon and Gaussian ones with no PROJ involvement.
fn grid_latlons<R>(submessage: &SubMessage<'_, R>) -> Result<Vec<(f64, f64)>, String> {
    let grid_def = submessage.grid_def();
    let template = GridDefinitionTemplateValues::try_from(grid_def)
        .map_err(|e| format!("Cannot read grid definition: {e}"))?;

    match template {
        GridDefinitionTemplateValues::Template30(ref lambert_grid) => {
            let points = lambert::latlons(lambert_grid)?;
            // Same guard grib applies to its own iterators: section 3 states
            // how many points there are, and a mismatch means the grid we
            // walked is not the grid the data was packed against.
            let declared = grid_def.num_points() as usize;
            if points.len() != declared {
                return Err(format!(
                    "Lambert grid point count mismatch: {declared} declared in \
                     section 3 vs {} computed",
                    points.len(),
                ));
            }
            Ok(points)
        }
        _ => Ok(submessage
            .latlons()
            .map_err(|e| format!("Cannot compute grid lat/lons: {e}"))?
            .map(|(lat, lon)| (f64::from(lat), f64::from(lon)))
            .collect()),
    }
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
    let latlon_pairs = grid_latlons(&submessage)?;

    // Get grid dimensions.
    let (ni, nj) = submessage
        .grid_shape()
        .map_err(|e| format!("Cannot determine grid shape: {e}"))?;

    // Extract reference time before consuming the submessage for decoding.
    //
    // A malformed reference time is a hard error, not something to paper over.
    // These previously fell back to `unwrap_or_default()`, i.e. 1970-01-01
    // 00:00, which the pane control renders as "Model Data (00:00z)" — stale
    // or corrupt data made to look merely oddly-timed. Refusing the message
    // surfaces it as a fetch failure instead.
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

    let (visible_points, value_range) = super::summarize_values(&values, param);

    Ok(HrrrGridData {
        parameter: param,
        values,
        lats,
        lons,
        ni,
        nj,
        bounds,
        ref_time,
        forecast_hour: param.forecast_hour(),
        visible_points,
        value_range,
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

/// Fetch a composite HRRR parameter (e.g. bulk shear) that requires
/// multiple NOMADS fields merged into one grid.
///
/// Fetches each component sequentially, then combines them. For wind shear
/// this means fetching U and V components and computing magnitude √(U²+V²).
pub async fn fetch_composite_hrrr_data(
    client: &reqwest::Client,
    param: &ModelParameter,
) -> HrrrFetchResult {
    let parts = match param.composite_parts() {
        Some(p) => p,
        None => return fetch_hrrr_data(client, param).await,
    };

    let (date, hour) = latest_available_run();

    match try_fetch_composite(client, param, &parts, date, hour).await {
        Ok(data) => return HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::warn!(
                "HRRR composite fetch for {date} {hour:02}z failed: {e}, trying previous hour"
            );
        }
    }

    let (prev_date, prev_hour) = if hour == 0 {
        (date - chrono::Duration::days(1), 23u8)
    } else {
        (date, hour - 1)
    };

    match try_fetch_composite(client, param, &parts, prev_date, prev_hour).await {
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
    param: &ModelParameter,
    parts: &[(&str, &str)],
    date: NaiveDate,
    hour: u8,
) -> Result<HrrrGridData, String> {
    let mut grids: Vec<HrrrGridData> = Vec::with_capacity(parts.len());
    let urls = composite_urls(param, parts, hour, date);

    for ((var, lev), url) in parts.iter().zip(urls) {
        log::info!("Fetching HRRR composite component {var} {lev} from {url}");

        let response = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("HTTP request failed for {var}: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("HTTP {} for {var}", response.status()));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read body for {var}: {e}"))?;

        log::info!("Received {} bytes for {var}", bytes.len());
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

    // Recomputed from the merged magnitudes: the summary each component grid
    // carries describes that component alone, which says nothing about the
    // vector magnitude the user actually sees.
    let (visible_points, value_range) = super::summarize_values(&values, *param);

    Ok(HrrrGridData {
        parameter: *param,
        values,
        lats: base.lats.clone(),
        lons: base.lons.clone(),
        ni: base.ni,
        nj: base.nj,
        bounds: base.bounds,
        ref_time: base.ref_time,
        forecast_hour: base.forecast_hour,
        visible_points,
        value_range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url_for(param: ModelParameter) -> String {
        nomads_url(&param, 3, NaiveDate::from_ymd_opt(2026, 7, 25).unwrap())
    }

    /// The exact level spellings HRRR uses. Taken verbatim from a real
    /// `hrrr.t03z.wrfsfcf01.grib2.idx`:
    ///
    /// ```text
    /// 45:...:MXUPHL:5000-2000 m above ground:0-1 hour max fcst:
    /// 47:...:MXUPHL:2000-0 m above ground:0-1 hour max fcst:
    /// ```
    ///
    /// NOMADS matches these literally. The ascending spellings this code
    /// shipped with — `2000-5000` and `0-2000` — match no record and the
    /// filter CGI answers HTTP 500 `invalid parameter`, which is why both UH
    /// parameters were 100% broken.
    #[test]
    fn uh_level_strings_use_hrrrs_descending_bound_order() {
        assert_eq!(
            ModelParameter::MaxUH2to5km.nomads_level(),
            "lev_5000-2000_m_above_ground",
        );
        assert_eq!(
            ModelParameter::MaxUH0to2km.nomads_level(),
            "lev_2000-0_m_above_ground",
        );
    }

    /// Every parameter's `var_`/`lev_` pair, derived from the record it must
    /// select in a real `hrrr.t03z.wrfsfcf00.grib2.idx` (UH from the `f01`
    /// index, which spells its levels identically).
    ///
    /// There is no rule to infer here, which is the whole trap: HRRR orders
    /// layer bounds inconsistently between fields — `HLCY:3000-0` and
    /// `MXUPHL:5000-2000` put the top first, while `VUCSH:0-6000` and
    /// `CAPE:0-3000 m` put the bottom first — and NOMADS matches the string
    /// literally, with no near-miss handling. A level spelling can therefore
    /// only be validated against the index verbatim.
    const IDX_RECORDS: &[(ModelParameter, &str, &str)] = &[
        (ModelParameter::SurfaceBasedCin, "CIN", "surface"),
        (ModelParameter::MixedLayerCin, "CIN", "180-0 mb above ground"),
        (ModelParameter::SurfaceBasedCape, "CAPE", "surface"),
        (ModelParameter::MixedLayerCape, "CAPE", "180-0 mb above ground"),
        (ModelParameter::MostUnstableCape, "CAPE", "255-0 mb above ground"),
        (ModelParameter::LiftedIndex, "LFTX", "500-1000 mb"),
        (ModelParameter::Srh1km, "HLCY", "1000-0 m above ground"),
        (ModelParameter::Srh3km, "HLCY", "3000-0 m above ground"),
        (ModelParameter::MaxUH2to5km, "MXUPHL", "5000-2000 m above ground"),
        (ModelParameter::MaxUH0to2km, "MXUPHL", "2000-0 m above ground"),
        (ModelParameter::SurfaceWindGust, "GUST", "surface"),
        (
            ModelParameter::PrecipitableWater,
            "PWAT",
            "entire atmosphere (considered as a single layer)",
        ),
        (ModelParameter::Temperature2m, "TMP", "2 m above ground"),
        (ModelParameter::Dewpoint2m, "DPT", "2 m above ground"),
        (ModelParameter::Visibility, "VIS", "surface"),
    ];

    /// NOMADS' filter query encodes an index field exactly as it appears in
    /// the `.idx`, with spaces replaced by underscores.
    fn as_query_terms(var: &str, level: &str) -> (String, String) {
        (format!("var_{var}"), format!("lev_{}", level.replace(' ', "_")))
    }

    /// Pins every non-composite parameter to the index record it selects.
    #[test]
    fn every_parameter_selects_a_real_index_record() {
        for &(param, var, level) in IDX_RECORDS {
            let (want_var, want_lev) = as_query_terms(var, level);
            assert_eq!(
                param.nomads_var(),
                want_var,
                "{} must select `{var}:{level}`",
                param.display_name(),
            );
            assert_eq!(
                param.nomads_level(),
                want_lev,
                "{} must select `{var}:{level}`",
                param.display_name(),
            );
        }
    }

    /// The table above is only a guard if it covers everything.
    #[test]
    fn the_index_table_covers_every_non_composite_parameter() {
        for param in ModelParameter::all() {
            if param.is_composite() {
                continue;
            }
            assert!(
                IDX_RECORDS.iter().any(|&(p, _, _)| p == *param),
                "{} is not pinned to an index record",
                param.display_name(),
            );
        }
    }

    /// Composite components select real index records too.
    #[test]
    fn composite_components_select_real_index_records() {
        let parts = ModelParameter::BulkShear6km.composite_parts().unwrap();
        let expected = [
            ("VUCSH", "0-6000 m above ground"),
            ("VVCSH", "0-6000 m above ground"),
        ];
        assert_eq!(parts.len(), expected.len());
        for (&(got_var, got_lev), (var, level)) in parts.iter().zip(expected) {
            let (want_var, want_lev) = as_query_terms(var, level);
            assert_eq!(got_var, want_var);
            assert_eq!(got_lev, want_lev);
        }
    }

    /// f00 `MXUPHL` is a `0-0 day max fcst` — a maximum over a zero-length
    /// window, which is identically 0.0 everywhere. Fixing only the level
    /// string would turn an HTTP 500 into a permanently blank overlay, so the
    /// request has to name a forecast hour with a real accumulation window.
    #[test]
    fn uh_requests_a_forecast_hour_with_a_nonzero_window() {
        for param in [ModelParameter::MaxUH2to5km, ModelParameter::MaxUH0to2km] {
            assert!(
                param.forecast_hour() > 0,
                "{} must not come from f00: its accumulation window there has \
                 zero length and the field is constant 0.0",
                param.display_name(),
            );
            assert!(param.is_windowed());
        }
    }

    /// Everything else is instantaneous, so f00 is both valid and freshest.
    #[test]
    fn non_windowed_parameters_still_come_from_the_analysis() {
        for param in ModelParameter::all() {
            if param.is_windowed() {
                continue;
            }
            assert_eq!(
                param.forecast_hour(),
                0,
                "{} is instantaneous and should come from f00",
                param.display_name(),
            );
        }
    }

    /// The forecast hour must reach the `file=` term; if it does not, the UH
    /// fix silently reverts to the constant-zero f00 message.
    #[test]
    fn url_file_term_carries_the_parameters_forecast_hour() {
        assert!(
            url_for(ModelParameter::MaxUH2to5km).contains("wrfsfcf01.grib2"),
            "UH must request f01",
        );
        assert!(
            url_for(ModelParameter::MaxUH0to2km).contains("wrfsfcf01.grib2"),
            "UH must request f01",
        );
        assert!(
            url_for(ModelParameter::SurfaceBasedCin).contains("wrfsfcf00.grib2"),
            "instantaneous fields must request f00",
        );
    }

    #[test]
    fn url_carries_run_date_hour_var_and_level() {
        let url = url_for(ModelParameter::MaxUH2to5km);
        assert!(url.contains("hrrr.20260725"), "{url}");
        assert!(url.contains("hrrr.t03z."), "{url}");
        assert!(url.contains("var_MXUPHL=on"), "{url}");
        assert!(url.contains("lev_5000-2000_m_above_ground=on"), "{url}");
    }

    /// Composite components go through a separate URL builder, which must
    /// take its forecast hour from the parameter rather than assuming f00.
    ///
    /// The only composite today is bulk shear, which *is* f00 — so asserting
    /// on it alone cannot distinguish "derived from the parameter" from
    /// "hardcoded to zero". The builder is generic over the parameter, so it
    /// is exercised here with a windowed one too: the day a windowed
    /// composite is added, this fails instead of silently fetching f00.
    #[test]
    fn composite_urls_derive_the_forecast_hour_from_the_parameter() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();

        let shear = ModelParameter::BulkShear6km;
        let parts = shear.composite_parts().unwrap();
        let urls = composite_urls(&shear, &parts, 3, date);
        assert_eq!(urls.len(), parts.len());
        for ((var, _), url) in parts.iter().zip(&urls) {
            assert!(url.contains("wrfsfcf00.grib2"), "{url}");
            assert!(url.contains(&format!("{var}=on")), "{url}");
        }

        let windowed = ModelParameter::MaxUH2to5km;
        let urls = composite_urls(
            &windowed,
            &[("var_MXUPHL", "lev_5000-2000_m_above_ground")],
            3,
            date,
        );
        assert!(
            urls[0].contains("wrfsfcf01.grib2"),
            "composite URLs must follow the parameter's forecast hour: {}",
            urls[0],
        );
    }

    /// Live end-to-end check against NOMADS. Ignored by default so CI stays
    /// offline; this is the check that would have caught the original bug,
    /// since every offline assertion above is only as good as the level
    /// spellings being right.
    ///
    /// Run with: `cargo test -p rustdar-overlays -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "hits the live NOMADS filter CGI"]
    async fn live_uh_fetch_returns_a_non_constant_field() {
        // Builds the client the way the application does, which is also what
        // installs the crypto provider: with `rustls-no-provider` and no
        // `aws-lc-rs` left in the graph, a bare `Client::new()` panics here.
        let client = rustdar_radar::tls::client(
            rustdar_radar::tls::USER_AGENT,
            std::time::Duration::from_secs(120),
        )
        .build()
        .expect("client");
        for param in [ModelParameter::MaxUH2to5km, ModelParameter::MaxUH0to2km] {
            let grid = match fetch_hrrr_data(&client, &param).await.0 {
                Ok(g) => g,
                Err(e) => panic!("{} fetch failed: {e}", param.display_name()),
            };
            let (lo, hi) = grid.value_range.expect("finite values");
            println!(
                "{}: f{:02}, {} pts, range {lo}..{hi}, {} visible",
                param.display_name(),
                grid.forecast_hour,
                grid.values.len(),
                grid.visible_points,
            );
            assert!(
                lo < hi,
                "{} decoded as a constant field ({lo}) — this is the f00 \
                 zero-window failure the forecast-hour fix exists to avoid",
                param.display_name(),
            );
            assert!(grid.blank_notice().is_none(), "{}", param.display_name());
        }
    }
}
