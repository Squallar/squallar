//! HRRR data fetching from the `noaa-hrrr-bdp-pds` S3 bucket.
//!
//! This replaces the NOMADS filter CGI
//! (`nomads.ncep.noaa.gov/cgi-bin/filter_hrrr_2d.pl`), which sends no
//! `Access-Control-Allow-Origin` and is therefore unreachable from rustdar's
//! web build. Verified 2026-07-25 with `curl -H 'Origin: https://example.com'`:
//! `200`, and no CORS headers at all.
//!
//! # How a single field is fetched without a server
//!
//! NOMADS' whole purpose was server-side subsetting: ask for one variable, get
//! one GRIB2 record. S3 serves whole files, but every HRRR GRIB2 file has an
//! `.idx` sidecar — ~9 KB of text listing each record's byte offset:
//!
//! ```text
//! 105:63110198:d=2026072514:CAPE:surface:anl:
//! 106:63976324:d=2026072514:CIN:surface:anl:
//! 107:64861905:d=2026072514:PWAT:entire atmosphere (considered as a single layer):anl:
//! ```
//!
//! So the subsetting becomes: fetch the index, find the record, and issue an
//! HTTP `Range` request for the bytes between its offset and the next one.
//! Two requests instead of one, but the second is the only large one and it is
//! **smaller** than what NOMADS returned — 1.03 MB against 2.27 MB for the
//! same field, measured. The difference is packing: passing `subregion` to the
//! filter CGI (which rustdar did, for a subsetting HRRR never actually
//! supported) made NOMADS re-encode the record through wgrib2, turning data
//! representation template **5.3** — complex packing with spatial differencing
//! — into **5.0**, simple packing. S3 serves the operational bytes, so the
//! decode path now sees 5.3. Both are pure Rust in `grib`; neither needs the
//! JPEG2000 or CCSDS features this crate drops.
//!
//! One consequence worth knowing when reading `hrrr::lambert`'s fixtures: the
//! NOMADS re-encode also re-rounded `Lo1` from 237280472 to 237280471
//! microdegrees. S3 carries the operational 237280472. The projection anchors
//! on whatever `Lo1` the file states, so this changes nothing about
//! correctness, but a test constant derived from a NOMADS download is off by
//! one microdegree against an S3 one.
//!
//! # The grid did not change
//!
//! It is tempting to assume dropping a "subregion" parameter enlarges the
//! grid. It does not: NOMADS never subset Lambert-conformal grids in the first
//! place, so both paths return the full 1799×1059 CONUS grid — 1,905,141
//! points. `parse_grib2` derives its bounds from the grid it is handed and
//! never from a requested region, so nothing downstream needed changing.

use chrono::{NaiveDate, NaiveDateTime, Timelike, Utc};
use grib::{Grib2SubmessageDecoder, GridDefinitionTemplateValues, LatLons, SubMessage};
use rustdar_radar::sources::DataSources;

use super::{HrrrFetchResult, HrrrGridData, ModelParameter, lambert};
use crate::types::GeoBounds;

/// How long a single HRRR request may take.
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

/// Parse a GRIB2 `.idx` sidecar.
///
/// The format is colon-separated:
/// `number:offset:d=YYYYMMDDHH:VAR:LEVEL:FORECAST:`
///
/// The level field **contains colons in no HRRR record but contains spaces,
/// parentheses and hyphens in many**, so splitting is positional and bounded:
/// exactly the first three fields and the last two are structural, and
/// everything is taken by index rather than by scanning for delimiters.
/// Malformed lines are skipped rather than failing the parse — a trailing
/// blank line is normal.
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

/// The inclusive byte range holding one record, for an HTTP `Range` header.
///
/// A record runs from its own offset to one byte before the *next* record's.
/// The final record has no successor, so its end is unknown from the index
/// alone and `None` is returned for the end — the caller asks for an
/// open-ended range.
///
/// Matching is on `var` **and** `level` together. Either alone is ambiguous:
/// HRRR carries five `CAPE` records at different levels and two `CIN`s, and
/// `surface` appears on dozens of variables. The first match wins, which is
/// the lowest-numbered record — for the fields rustdar requests there is
/// exactly one match, and `every_parameter_selects_exactly_one_record` is what
/// keeps that true.
pub fn byte_range(
    records: &[IdxRecord],
    var: &str,
    level: &str,
) -> Option<(u64, Option<u64>)> {
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

// ---------------------------------------------------------------------------
// GRIB2 decoding
// ---------------------------------------------------------------------------

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
///
/// # One submessage
///
/// The bytes handed here must be exactly one GRIB2 record carrying exactly one
/// submessage, and that is now *checked* rather than assumed. Under NOMADS the
/// server guaranteed it. Under byte-ranging the guarantee comes from this
/// crate's own arithmetic in [`byte_range`], and an off-by-one there would
/// silently deliver two records — of which the old `iter().next()` would have
/// decoded the first and thrown the rest away, producing a plausible grid for
/// the wrong field. Refusing is the only safe answer.
fn parse_grib2(bytes: &[u8], param: ModelParameter) -> Result<HrrrGridData, String> {
    let grib2 = grib::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| format!("GRIB2 parse error: {e}"))?;

    // Counted in its own pass, before any `SubMessage` is held: grib's
    // iterator borrows the reader through a `RefCell`, and advancing it while
    // a submessage is alive panics with "RefCell already borrowed".
    let count = grib2.iter().count();
    if count != 1 {
        return Err(format!(
            "expected exactly one GRIB2 submessage, found {count} — the byte \
             range does not delimit a single record",
        ));
    }

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
    //
    // S3 serves the operational encoding, DRT 5.3 (complex packing with
    // spatial differencing), where NOMADS' filter re-encoded to 5.0. Both are
    // pure Rust in grib and `dispatch()` picks by template, so this call is
    // unchanged — but it is the line that would fail if the feature set in
    // Cargo.toml were ever trimmed further.
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
        format!("no `{var}:{level}` record in {idx_url} ({} records)", records.len())
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

    // 206 is the expected success. A 200 means the server ignored `Range` and
    // sent the whole 130 MB file; that is not something to quietly accept.
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
    log::info!("Received {} bytes of GRIB2 data for {var}:{level}", bytes.len());
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

    let (prev_date, prev_hour) = if hour == 0 {
        (date - chrono::Duration::days(1), 23u8)
    } else {
        (date, hour - 1)
    };

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

    let (prev_date, prev_hour) = if hour == 0 {
        (date - chrono::Duration::days(1), 23u8)
    } else {
        (date, hour - 1)
    };

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

/// Build the client HRRR fetches use.
///
/// S3 allows a `User-Agent` on its preflight (`Access-Control-Allow-Headers:
/// user-agent`, verified), so the ordinary client is fine here — unlike the
/// METAR feed. See `rustdar_radar::tls::simple_client`.
pub fn hrrr_client() -> Result<reqwest::Client, String> {
    rustdar_radar::tls::client(rustdar_radar::tls::USER_AGENT, HRRR_TIMEOUT)
        .build()
        .map_err(|e| format!("could not build the HRRR client: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twelve verbatim lines from a live `hrrr.t14z.wrfsfcf00.grib2.idx`,
    /// including the two shapes that break a naive parser: a level containing
    /// spaces and parentheses (`PWAT`), and a variable whose name is itself a
    /// colon-free description (`var discipline=0 ...`).
    const SAMPLE_IDX: &str = "\
1:0:d=2026072514:REFC:entire atmosphere:anl:
2:300130:d=2026072514:RETOP:cloud top:anl:
3:499431:d=2026072514:var discipline=0 center=7 local_table=1 parmcat=16 parm=201:entire atmosphere:anl:
4:812221:d=2026072514:VIL:entire atmosphere:anl:
5:1064231:d=2026072514:VIS:surface:anl:
105:63110198:d=2026072514:CAPE:surface:anl:
106:63976324:d=2026072514:CIN:surface:anl:
107:64861905:d=2026072514:PWAT:entire atmosphere (considered as a single layer):anl:
131:94635452:d=2026072514:HLCY:3000-0 m above ground:anl:
132:95300000:d=2026072514:HLCY:1000-0 m above ground:anl:
145:99000000:d=2026072514:CAPE:180-0 mb above ground:anl:
146:99500000:d=2026072514:CIN:180-0 mb above ground:anl:
";

    fn records() -> Vec<IdxRecord> {
        parse_idx(SAMPLE_IDX)
    }

    // ── Index parsing ─────────────────────────────────────────────────────

    /// Fields come out at the right positions, including a level carrying
    /// spaces and parentheses.
    ///
    /// Expected values are read off the fixture text by eye, not produced by
    /// the parser.
    #[test]
    fn an_idx_line_splits_into_number_offset_var_and_level() {
        let r = records();
        assert_eq!(r.len(), 12, "every fixture line must parse");

        assert_eq!(r[0].number, 1);
        assert_eq!(r[0].offset, 0);
        assert_eq!(r[0].var, "REFC");
        assert_eq!(r[0].level, "entire atmosphere");
        assert_eq!(r[0].forecast, "anl");

        let pwat = r.iter().find(|r| r.var == "PWAT").unwrap();
        assert_eq!(pwat.offset, 64_861_905);
        assert_eq!(
            pwat.level, "entire atmosphere (considered as a single layer)",
            "a level with spaces and parentheses must survive intact",
        );
    }

    #[test]
    fn a_blank_or_malformed_idx_line_is_skipped_not_fatal() {
        assert!(parse_idx("").is_empty());
        assert!(parse_idx("\n\n").is_empty());
        assert!(parse_idx("garbage\n").is_empty());
        // A well-formed line survives alongside a broken one.
        let mixed = format!("nonsense\n{}", SAMPLE_IDX.lines().next().unwrap());
        assert_eq!(parse_idx(&mixed).len(), 1);
    }

    // ── Byte ranges ───────────────────────────────────────────────────────

    /// A record runs from its own offset to one byte before the next.
    ///
    /// The expected end is hand-computed from the fixture: CIN starts at
    /// 63,976,324 and PWAT at 64,861,905, so CIN ends at 64,861,904.
    #[test]
    fn a_byte_range_ends_one_byte_before_the_next_record() {
        let (start, end) = byte_range(&records(), "CIN", "surface").unwrap();
        assert_eq!(start, 63_976_324);
        assert_eq!(end, Some(64_861_904));
        // The length is the gap between the two offsets.
        assert_eq!(end.unwrap() - start + 1, 64_861_905 - 63_976_324);
    }

    /// An off-by-one here delivers a second record's first byte, which
    /// `parse_grib2` now refuses rather than silently decoding the wrong
    /// field. Pinning the arithmetic separately says which of the two broke.
    #[test]
    fn a_byte_range_does_not_overlap_the_following_record() {
        let r = records();
        for pair in r.windows(2) {
            let (_, end) = byte_range(&r, &pair[0].var, &pair[0].level).unwrap();
            assert_eq!(
                end,
                Some(pair[1].offset - 1),
                "{}:{} must stop before {}:{}",
                pair[0].var, pair[0].level, pair[1].var, pair[1].level,
            );
        }
    }

    /// The last record has no successor, so its end is open.
    #[test]
    fn the_final_records_range_is_open_ended() {
        let (start, end) = byte_range(&records(), "CIN", "180-0 mb above ground").unwrap();
        assert_eq!(start, 99_500_000);
        assert_eq!(end, None, "nothing in the index bounds the last record");
    }

    /// Matching on the variable alone is ambiguous: the fixture has two `CIN`
    /// and two `CAPE` records at different levels, and two `HLCY`s. Selecting
    /// by variable only would return surface CIN for mixed-layer CIN — a
    /// plausible-looking, entirely wrong field.
    #[test]
    fn a_record_is_selected_by_variable_and_level_together() {
        let r = records();
        assert_eq!(byte_range(&r, "CIN", "surface").unwrap().0, 63_976_324);
        assert_eq!(
            byte_range(&r, "CIN", "180-0 mb above ground").unwrap().0,
            99_500_000,
        );
        assert_eq!(byte_range(&r, "CAPE", "surface").unwrap().0, 63_110_198);
        assert_eq!(
            byte_range(&r, "CAPE", "180-0 mb above ground").unwrap().0,
            99_000_000,
        );
        // ...and the two SRH layers, which differ only in level.
        assert_eq!(
            byte_range(&r, "HLCY", "3000-0 m above ground").unwrap().0,
            94_635_452,
        );
        assert_eq!(
            byte_range(&r, "HLCY", "1000-0 m above ground").unwrap().0,
            95_300_000,
        );
    }

    /// A level spelling that matches nothing must fail loudly rather than
    /// fall back to a near miss. This is the failure mode the ascending
    /// `2000-5000` spellings had.
    #[test]
    fn an_unmatched_variable_or_level_yields_no_range() {
        let r = records();
        assert_eq!(byte_range(&r, "CIN", "2000-5000 m above ground"), None);
        assert_eq!(byte_range(&r, "NOSUCH", "surface"), None);
        assert_eq!(byte_range(&r, "CIN", "Surface"), None, "matching is exact");
    }

    // ── Parameter → index record ──────────────────────────────────────────

    /// Every parameter's `(var, level)` pair, transcribed **verbatim** from a
    /// real `hrrr.t14z.wrfsfcf00.grib2.idx` (UH from the `f01` index, which
    /// spells its levels identically).
    ///
    /// There is no rule to infer here, which is the whole trap: HRRR orders
    /// layer bounds inconsistently between fields — `HLCY:3000-0` and
    /// `MXUPHL:5000-2000` put the top first, while `VUCSH:0-6000` and
    /// `CAPE:0-3000 m` put the bottom first — and the index is matched
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

    /// Pins every non-composite parameter to the index record it selects.
    ///
    /// The comparison is now direct — the accessor returns the index's own
    /// string, with no `var_`/`lev_` encoding in between — so there is no
    /// transformation that could agree with itself.
    #[test]
    fn every_parameter_selects_a_real_index_record() {
        for &(param, var, level) in IDX_RECORDS {
            assert_eq!(param.grib_var(), var, "{}", param.display_name());
            assert_eq!(param.grib_level(), level, "{}", param.display_name());
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
            assert_eq!(got_var, var);
            assert_eq!(got_lev, level);
        }
    }

    /// No two parameters may select the same record — that would mean one of
    /// them is displaying the other's field.
    #[test]
    fn no_two_parameters_select_the_same_index_record() {
        let mut seen = std::collections::HashSet::new();
        for &(param, var, level) in IDX_RECORDS {
            assert!(
                seen.insert((var, level)),
                "{} selects `{var}:{level}`, which another parameter already claims",
                param.display_name(),
            );
        }
    }

    // ── Forecast hour ─────────────────────────────────────────────────────

    /// f00 `MXUPHL` is a `0-0 day max fcst` — a maximum over a zero-length
    /// window, which is identically 0.0 everywhere.
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

    /// The forecast hour must reach the object key; if it does not, the UH fix
    /// silently reverts to the constant-zero f00 record.
    #[test]
    fn the_object_key_carries_the_parameters_forecast_hour() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();
        let key = |p: ModelParameter| DataSources::hrrr_key(&date, 3, p.forecast_hour());
        assert!(key(ModelParameter::MaxUH2to5km).contains("wrfsfcf01.grib2"));
        assert!(key(ModelParameter::MaxUH0to2km).contains("wrfsfcf01.grib2"));
        assert!(key(ModelParameter::SurfaceBasedCin).contains("wrfsfcf00.grib2"));
    }

    // ── Live checks ───────────────────────────────────────────────────────

    /// The full S3 path, end to end, for a representative spread of fields.
    ///
    /// This is the check the migration lives or dies on: it fetches the real
    /// index, computes a real byte range, issues a real `Range` request, and
    /// decodes the operational DRT 5.3 bytes S3 serves (NOMADS re-encoded to
    /// 5.0, so this path was never exercised before).
    ///
    /// Run with:
    ///   `cargo test -p rustdar-overlays -- --ignored --nocapture live_hrrr`
    #[tokio::test]
    #[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
    async fn live_hrrr_fetches_and_decodes_from_s3() {
        let client = hrrr_client().expect("client");
        let sources = DataSources::production();

        // One surface field, one layer field, one whose level carries spaces
        // and parentheses, and one windowed field from f01.
        for param in [
            ModelParameter::SurfaceBasedCape,
            ModelParameter::MixedLayerCin,
            ModelParameter::PrecipitableWater,
            ModelParameter::MaxUH2to5km,
        ] {
            let grid = match fetch_hrrr_data(&client, &sources, &param).await.0 {
                Ok(g) => g,
                Err(e) => panic!("{} fetch failed: {e}", param.display_name()),
            };
            let (lo, hi) = grid.value_range.expect("finite values");
            println!(
                "{}: f{:02}, {}x{} = {} pts, range {lo}..{hi}, {} visible, ref {}",
                param.display_name(),
                grid.forecast_hour,
                grid.ni,
                grid.nj,
                grid.values.len(),
                grid.visible_points,
                grid.ref_time,
            );

            // The full CONUS grid, which is what both NOMADS and S3 return.
            // 1799 x 1059 is HRRR's operational grid, from the model's own
            // documentation.
            assert_eq!(grid.ni, 1799, "{}", param.display_name());
            assert_eq!(grid.nj, 1059, "{}", param.display_name());
            assert_eq!(grid.values.len(), 1_905_141, "{}", param.display_name());
            assert_eq!(grid.lats.len(), grid.values.len());

            assert!(
                lo < hi,
                "{} decoded as a constant field ({lo})",
                param.display_name(),
            );
            assert!(grid.blank_notice().is_none(), "{}", param.display_name());

            // The Lambert grid must cover CONUS. Corner latitudes/longitudes
            // for HRRR's domain are published: SW corner 21.14 N, 237.28 E.
            assert!(
                grid.bounds.min_lat < 25.0 && grid.bounds.max_lat > 47.0,
                "{} bounds {:?} do not span CONUS",
                param.display_name(),
                grid.bounds,
            );
        }
    }

    /// The composite path also works over byte ranges — two records from the
    /// same index, merged.
    #[tokio::test]
    #[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
    async fn live_hrrr_composite_merges_two_ranged_records() {
        let client = hrrr_client().expect("client");
        let sources = DataSources::production();
        let param = ModelParameter::BulkShear6km;

        let grid = match fetch_composite_hrrr_data(&client, &sources, &param).await.0 {
            Ok(g) => g,
            Err(e) => panic!("bulk shear fetch failed: {e}"),
        };
        let (lo, hi) = grid.value_range.expect("finite values");
        println!("bulk shear: {} pts, range {lo}..{hi}", grid.values.len());
        assert_eq!(grid.values.len(), 1_905_141);
        // A magnitude is non-negative by construction; if the merge had
        // returned one component instead, negatives would appear.
        assert!(lo >= 0.0, "a vector magnitude cannot be negative, got {lo}");
        assert!(hi > 0.0);
    }

    /// `parse_grib2` refuses bytes carrying more than one record.
    ///
    /// This is the guard that makes a byte-range arithmetic bug loud instead
    /// of silent: two concatenated records decode fine as a *sequence*, and
    /// the previous `iter().next()` would have taken the first and discarded
    /// the rest — a correct-looking grid for whichever field happened to come
    /// first.
    ///
    /// Built from a real record fetched here rather than a committed fixture,
    /// because a single HRRR record is ~1 MB and there is nothing to be gained
    /// from storing one. The single-record case is asserted first, so a
    /// `parse_grib2` that rejected *everything* would fail rather than pass.
    #[tokio::test]
    #[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
    async fn live_parse_grib2_refuses_more_than_one_submessage() {
        let client = hrrr_client().expect("client");
        let sources = DataSources::production();
        let (date, hour) = latest_available_run();

        let one = match fetch_record(&client, &sources, date, hour, 0, "CIN", "surface").await {
            Ok(b) => b,
            Err(_) => fetch_record(&client, &sources, date, hour - 1, 0, "CIN", "surface")
                .await
                .expect("CIN fetch"),
        };

        // Control: one record must parse.
        let single = parse_grib2(&one, ModelParameter::SurfaceBasedCin);
        assert!(single.is_ok(), "a single record must decode: {single:?}");

        // Two concatenated records must not.
        let mut two = one.clone();
        two.extend_from_slice(&one);
        let err = parse_grib2(&two, ModelParameter::SurfaceBasedCin)
            .expect_err("two records must be refused, not silently truncated");
        println!("two-record error: {err}");
        assert!(
            err.contains("exactly one GRIB2 submessage"),
            "expected the one-submessage guard to fire, got: {err}",
        );
    }

    /// The byte range really is a small fraction of the file.
    ///
    /// This is the reason for the whole `.idx` dance, and it is measured
    /// rather than asserted from theory: if a future change dropped the
    /// `Range` header, everything above would still pass while transferring
    /// ~130 MB per field.
    #[tokio::test]
    #[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
    async fn live_a_ranged_record_is_a_small_fraction_of_the_file() {
        let client = hrrr_client().expect("client");
        let sources = DataSources::production();
        let (date, hour) = latest_available_run();

        let bytes = fetch_record(&client, &sources, date, hour, 0, "CIN", "surface")
            .await
            .or(fetch_record(&client, &sources, date, hour - 1, 0, "CIN", "surface").await)
            .expect("CIN fetch");

        println!("surface CIN record: {} bytes", bytes.len());
        // NOMADS returned 2.27 MB for the same field; the operational record
        // is ~1.03 MB. Bound it well clear of both a whole file (~130 MB) and
        // an empty response.
        assert!(
            (100_000..8_000_000).contains(&bytes.len()),
            "{} bytes is not a single GRIB2 record",
            bytes.len(),
        );
        // And it must be a GRIB2 message, not an error page.
        assert_eq!(&bytes[..4], b"GRIB", "range did not start at a record boundary");
    }
}
