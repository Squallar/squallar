//! HRRR data fetching from the `noaa-hrrr-bdp-pds` S3 bucket.
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

use chrono::{NaiveDate, NaiveDateTime, Timelike, Utc};
use grib::{Grib2SubmessageDecoder, GridDefinitionTemplateValues, LatLons, SubMessage};
use rustdar_source::origins::DataSources;

use super::{GridCoords, HrrrFetchResult, HrrrGridData, ModelParameter, lambert};
use crate::fetch_policy::{FetchError, FetchFailure, NotFound};
use rustdar_geo::GeoBounds;

/// Live tests only. Production fetches with `ctx.client` (30 s).
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
/// **`(var, level)` is not unique.** A real index repeats it, and the repeats
/// are distinguished only by the forecast description — an instantaneous
/// reading against a maximum or an accumulation over the hour that ends there:
///
/// ```text
///  8:…:REFD:263 K level:1 hour fcst:        44:…:REFD:263 K level:0-1 hour max fcst:
/// 68:…:WEASD:surface:1 hour fcst:           85:…:WEASD:surface:0-1 hour acc fcst:
/// ```
///
/// So `forecast` completes the key. `(var, level, forecast)` **is** unique:
/// measured over f00, f01, f02, f06, f18, f24 and f48 of `hrrr.20260820` 00Z,
/// no triple repeats in any of the seven indexes. Passing `None` keeps the old
/// positional tie-break — the first, i.e. lowest-numbered, `(var, level)` hit —
/// which is a wrong-field read with no error whenever the pair is one of the
/// repeated ones, and is why production never passes `None`.
pub fn byte_range(
    records: &[IdxRecord],
    var: &str,
    level: &str,
    forecast: Option<&str>,
) -> Option<(u64, Option<u64>)> {
    let idx = records.iter().position(|r| {
        r.var == var && r.level == level && forecast.is_none_or(|want| r.forecast == want)
    })?;
    let start = records[idx].offset;
    // The *file's* next record, not the next matching one: the range must stop
    // where the bytes stop, and a qualified match is still delimited by whatever
    // physically follows it.
    let end = records.get(idx + 1).map(|next| next.offset - 1);
    Some((start, end))
}

/// The `.idx` forecast description `param`'s record carries in the `f{hour}`
/// file — the third component of the key [`byte_range`] selects on.
///
/// Measured across the published range of one 00Z run (`hrrr.20260820`, f00,
/// f01, f02, f06, f18, f24, f48), the grammar is exactly two shapes:
///
/// | | f00 | f`h`, h ≥ 1 |
/// |---|---|---|
/// | instantaneous | `anl` | `{h} hour fcst` |
/// | windowed maximum | `0-0 day max fcst` | `{h-1}-{h} hour max fcst` |
///
/// Note f00's `day` rather than `hour`: the zero-length window NCEP writes for
/// the analysis is not the same wording as the hourly ones, so it cannot be
/// derived from the `h ≥ 1` form.
///
/// `max` is not a claim about aggregation in general — the same index carries
/// `acc` (`APCP`, `WEASD`), `ave` and `min` records — it is that both windowed
/// parameters rustdar registers are `MXUPHL` maxima. A windowed parameter with
/// another aggregation must give this function a per-parameter word rather than
/// loosen the match: the uniqueness of the triple is the whole point.
pub fn record_forecast(param: &ModelParameter, forecast_hour: u8) -> String {
    match (param.is_windowed(), forecast_hour) {
        (false, 0) => "anl".to_string(),
        (false, h) => format!("{h} hour fcst"),
        (true, 0) => "0-0 day max fcst".to_string(),
        (true, h) => format!("{}-{h} hour max fcst", h - 1),
    }
}

/// The forecast hour a fetch will actually use: `requested`, raised to
/// [`ModelParameter::min_forecast_hour`] if it falls below it.
///
/// A floor is applied here, at the one place a requested hour enters the fetch,
/// rather than being enforced by every caller: the caller that forgets does not
/// get an error, it gets a windowed field over a zero-length window — a grid of
/// exactly 0.0 that draws as an empty map. Only ever raises, never lowers, so a
/// scrub to f18 stays f18 for every parameter.
///
/// Its only production callers are [`fetch_hrrr_data`] and
/// [`fetch_composite_hrrr_data`], the two doors into this module. Nothing
/// enforces that, so a third door has to remember to apply it.
pub fn effective_forecast_hour(param: &ModelParameter, requested: u8) -> u8 {
    requested.max(param.min_forecast_hour())
}

// ---------------------------------------------------------------------------
// Run selection
// ---------------------------------------------------------------------------

/// The most recent HRRR run that should be published as of `now` (UTC).
///
/// HRRR appears on S3 ~45-90 min after the run time. Two hours back is a safe
/// default, and `fetch_hrrr_data` falls back another hour on failure.
///
/// Pure, and split out of [`latest_available_run`] so the offset can be pinned
/// at fixed instants. Reading `Utc::now()` inside the only entry point left a
/// test no choice but to recompute the answer from the same clock, which is a
/// check that cannot fail.
pub fn run_for(now: NaiveDateTime) -> (NaiveDate, u8) {
    let safe_time = now - chrono::Duration::hours(2);
    (safe_time.date(), safe_time.time().hour() as u8)
}

/// [`run_for`] against the wall clock. The one place the clock is read.
pub fn latest_available_run() -> (NaiveDate, u8) {
    run_for(Utc::now().naive_utc())
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
// The analysis-axis listing
// ---------------------------------------------------------------------------

/// The run times the bucket really carries over `range`, from the `wrfsfcf00`
/// keys of `hrrr.YYYYMMDD/conus/`.
///
/// **Listed, never constructed.** The run cycle is hourly and utterly regular,
/// so a run time *could* be computed — and would then claim runs the archive
/// does not have. The archive begins at `hrrr.20140730` and has gaps; a
/// constructed list turns each of them into a frame that fetches a 404 and
/// stalls a loop on it.
///
/// `f00` and not every key: one listing of a day's `conus/` prefix returns
/// every forecast hour of every cycle, and the analysis axis wants one frame
/// per run. The forecast axis needs no listing at all.
///
/// The window is walked one UTC day at a time because the key prefix is the
/// day; a 24-hour scrub therefore costs one or two LISTs, not one per frame.
pub async fn list_analysis_runs(
    client: &reqwest::Client,
    sources: &DataSources,
    range: (NaiveDateTime, NaiveDateTime),
) -> Result<Vec<NaiveDateTime>, FetchError> {
    let (start, end) = range;
    if end < start {
        return Ok(Vec::new());
    }
    let mut runs: Vec<NaiveDateTime> = Vec::new();
    let mut day = start.date();
    let mut days = 0;
    loop {
        for run in list_runs_for_day(client, sources, day).await? {
            if run >= start && run <= end && !runs.contains(&run) {
                runs.push(run);
            }
        }
        days += 1;
        if day >= end.date() {
            break;
        }
        if days >= MAX_LISTED_DAYS {
            // The prefix is the day and the archive is twelve years deep, so a
            // window nobody bounded is a walk of four thousand LISTs. The
            // caller's own window is the intended bound; this is the one that
            // holds when it is wrong.
            log::warn!(
                "HRRR: analysis listing stopped at {MAX_LISTED_DAYS} days; \
                 the window {start} .. {end} is longer than any loop asks for",
            );
            break;
        }
        day += chrono::Duration::days(1);
    }
    runs.sort_unstable();
    Ok(runs)
}

/// How many UTC day prefixes one analysis listing will walk. A loop window is
/// hours and archive scrubbing is days; eight is generous for both and finite
/// against a range that was never bounded.
const MAX_LISTED_DAYS: usize = 8;

// The archive is `hrrr.20140730` forward: a cap that is not smaller than it
// bounds nothing. A build failure and not a runtime assertion, because both
// sides are constants and a test comparing them is a check that cannot fail.
const _: () = assert!(MAX_LISTED_DAYS < 4383);

/// One UTC day's `hrrr.YYYYMMDD/conus/` prefix, paginated.
///
/// A day of CONUS keys is ~800 objects and S3 pages at 1000, so this is
/// usually one request — but the continuation is followed rather than assumed
/// away, and a repeated or missing token stops the walk instead of spinning on
/// the identical first page.
async fn list_runs_for_day(
    client: &reqwest::Client,
    sources: &DataSources,
    day: NaiveDate,
) -> Result<Vec<NaiveDateTime>, FetchError> {
    let prefix = format!("hrrr.{}/conus/", day.format("%Y%m%d"));
    let mut runs = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut url = format!(
            "{}/?list-type=2&prefix={prefix}",
            sources.s3_bucket_url(&sources.hrrr_bucket),
        );
        if let Some(token) = &continuation {
            url.push_str("&continuation-token=");
            url.push_str(&urlencoded(token));
        }

        let resp =
            client.get(&url).send().await.map_err(|e| {
                FetchError::from_transport(&e, format!("S3 list request failed: {e}"))
            })?;
        if !resp.status().is_success() {
            // `IsBroken`: a bucket listing is not published on a schedule, so a
            // 404 here means the bucket is gone or renamed, not that the day
            // has not landed yet.
            return Err(FetchError::from_status(
                resp.status(),
                NotFound::IsBroken,
                format!("S3 returned HTTP {}", resp.status()),
            ));
        }
        let body = resp.text().await.map_err(|e| {
            FetchError::from_transport(&e, format!("Failed to read S3 list response: {e}"))
        })?;

        let doc = roxmltree::Document::parse(&body)
            .map_err(|e| FetchError::transient(format!("Failed to parse S3 XML: {e}")))?;
        for node in doc.descendants() {
            if node.tag_name().name() == "Key"
                && let Some(key) = node.text()
                && let Some(run) = run_of_analysis_key(key)
            {
                runs.push(run);
            }
        }

        let truncated = doc
            .descendants()
            .find(|n| n.tag_name().name() == "IsTruncated")
            .and_then(|n| n.text())
            .is_some_and(|t| t == "true");
        if !truncated {
            break;
        }
        let next = doc
            .descendants()
            .find(|n| n.tag_name().name() == "NextContinuationToken")
            .and_then(|n| n.text())
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        let Some(next) = next else {
            log::warn!("HRRR: S3 truncated '{prefix}' with no continuation token");
            break;
        };
        if continuation.as_deref() == Some(next.as_str()) {
            log::warn!("HRRR: S3 repeated its continuation token for '{prefix}'");
            break;
        }
        continuation = Some(next);
    }
    Ok(runs)
}

/// The run time an **analysis** key names, or `None` for anything else in the
/// prefix.
///
/// `hrrr.20260820/conus/hrrr.t14z.wrfsfcf00.grib2` -> 2026-08-20 14:00. The
/// `.idx` sidecars, the sub-hourly `wrfsubh` files and every f01+ key are all
/// in the same prefix and all answer `None` — the match is on the whole
/// filename, not on a substring, so `wrfsfcf00.grib2.idx` does not read as an
/// analysis grid.
pub fn run_of_analysis_key(key: &str) -> Option<NaiveDateTime> {
    let (dir, file) = key.rsplit_once('/')?;
    let day = dir.strip_suffix("/conus")?.rsplit_once("hrrr.")?.1;
    // The length check is load-bearing: `%Y%m%d` accepts `2026082` and reads
    // it as 2026-08-02, so a truncated prefix would come back as a real run
    // three weeks from where the key says.
    if day.len() != 8 || !day.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let date = NaiveDate::parse_from_str(day, "%Y%m%d").ok()?;
    let hour = file
        .strip_prefix("hrrr.t")?
        .strip_suffix("z.wrfsfcf00.grib2")?;
    let hour: u32 = hour.parse().ok()?;
    date.and_hms_opt(hour, 0, 0)
}

/// Percent-encode an S3 continuation token for a query string. Tokens are
/// base64 and routinely carry `+`, `/` and `=`.
fn urlencoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// GRIB2 decoding
// ---------------------------------------------------------------------------

/// How to get the lat/lon of any grid point of a submessage, in scanning-mode
/// order.
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

/// The longitude envelope HRRR declares to [`check_domain_longitude`].
///
/// HRRR CONUS measures -134.0955..-60.9172, corner-verified against real GRIB2
/// section 3 in `hrrr::lambert::tests`. This is that with margin either side.
pub const HRRR_DOMAIN_LON: std::ops::RangeInclusive<f64> = -140.0..=-50.0;

/// Marks [`check_domain_longitude`]'s refusal so [`classify_parse_error`] can
/// tell it apart from a genuine decode failure.
const DOMAIN_REFUSAL_MARK: &str = "unsupported model domain";

/// A `parse_grib2` error, classified for the retry ladder.
fn classify_parse_error(message: String) -> FetchError {
    if message.contains(DOMAIN_REFUSAL_MARK) {
        FetchError::permanent(message)
    } else {
        FetchError::transient(message)
    }
}

/// Refuse a gridded domain whose longitude the renderer has never been shown to
/// place correctly — and say what decision the refusal is asking for.
///
/// `domain` is the **source's own** declared envelope, and `source` names it in
/// the refusal. It was a module constant until the substrate became shared: one
/// envelope covering every gridded source would have to be the union, which is
/// the widest claim nobody measured rather than the narrowest each source can
/// actually stand behind.
pub(crate) fn check_domain_longitude(
    bounds: &GeoBounds,
    domain: &std::ops::RangeInclusive<f64>,
    source: &str,
) -> Result<(), String> {
    // `min > max` and not just `!is_finite`: the bounds walk above seeds
    // `min_lon` at `f64::MAX` and `max_lon` at `f64::MIN`, and both of those
    // *are* finite, so a grid whose coordinates could not be walked arrives
    // here as an inverted extent rather than a NaN one.
    if !bounds.min_lon.is_finite() || !bounds.max_lon.is_finite() || bounds.min_lon > bounds.max_lon
    {
        return Err(format!(
            "Model grid reported a non-finite or inverted longitude extent \
             ({}..{}); its coordinates could not be walked",
            bounds.min_lon, bounds.max_lon
        ));
    }
    if domain.contains(&bounds.min_lon) && domain.contains(&bounds.max_lon) {
        return Ok(());
    }
    Err(format!(
        "{DOMAIN_REFUSAL_MARK}: {source} spans longitude {:.4}..{:.4}, outside \
         the {:.1}..{:.1} envelope it declares (HRRR CONUS is \
         -134.0955..-60.9172).\n\
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
         \x20 * a per-point shift - tears cells, because `rasterize_gridded` \
         sizes every cell from its neighbours' pixel spacing, so two adjacent \
         points shifted differently stretch one cell across the texture;\n\
         \x20 * `GridCoords::wraps_longitude` - guards the index window, not \
         this, and measures `false` for a seam-parked grid.\n\
         \n\
         THE DECISION THIS TEXT ASKED FOR HAS BEEN TAKEN, AND IT IS ONLY HALF \
         OF ONE. The envelope is no longer a module constant shared by every \
         gridded source: each source passes its own, so widening one source's \
         claim can no longer widen another's by accident. HRRR declares \
         `HRRR_DOMAIN_LON` (-140..-50), unchanged and still the only envelope \
         the seam measurement was taken under.\n\
         \n\
         What was NOT decided is the repair itself. A source that declares a \
         domain reaching the antimeridian - a global satellite composite is \
         the obvious one - still needs one of the three candidates above \
         chosen and built, and declaring a wider envelope without it buys a \
         blank layer rather than a drawn one. Whoever adds that source is the \
         person who can choose. The measurement, the instrument \
         (`seam_probe.rs`) and the reasoning are in `campaigns/overlays/t17/` \
         on the `campaign-harness` branch.",
        bounds.min_lon,
        bounds.max_lon,
        domain.start(),
        domain.end(),
    ))
}

/// Parse GRIB2 bytes into `HrrrGridData`.
///
/// `forecast_hour` is the hour the bytes were *requested* at, not one the
/// parameter implies: since the forecast hour became a floor rather than a
/// value ([`ModelParameter::min_forecast_hour`]) the same parameter decodes at
/// many hours, and `valid_time()` is `ref_time + forecast_hour`.
fn parse_grib2(
    bytes: &[u8],
    param: ModelParameter,
    forecast_hour: u8,
) -> Result<HrrrGridData, String> {
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
    // differencing); NOMADS re-encoded to 5.0.
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
    check_domain_longitude(&bounds, &HRRR_DOMAIN_LON, "HRRR")?;

    let (visible_points, value_range) = super::summarize_values(&values, |v| param.paints(v));

    Ok(HrrrGridData {
        parameter: param,
        values,
        coords,
        ni,
        nj,
        bounds,
        ref_time,
        forecast_hour,
        visible_points,
        value_range,
    })
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

/// Fetch one GRIB2 record: index, locate, range-request.
///
/// `run` is `(date, run_hour)` in one argument rather than two so the record
/// key stays under the argument ceiling now that `forecast` is part of it.
async fn fetch_record(
    client: &reqwest::Client,
    sources: &DataSources,
    run: (NaiveDate, u8),
    forecast_hour: u8,
    var: &str,
    level: &str,
    forecast: &str,
) -> Result<Vec<u8>, FetchError> {
    let (date, run_hour) = run;
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

    // Named in full, `forecast` included: the qualifier is what makes the key
    // unique, so a miss on it is the likeliest way this ever fails and the
    // message has to say which of the three components was not found.
    let (start, end) = byte_range(&records, var, level, Some(forecast)).ok_or_else(|| {
        FetchError::transient(format!(
            "no `{var}:{level}:{forecast}` record in {idx_url} ({} records)",
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

/// Fetch HRRR model data for the given parameter, from `run` at `f_hour`.
///
/// `f_hour` is clamped up to [`ModelParameter::min_forecast_hour`]: a windowed
/// parameter at f00 is a maximum over a zero-length window, identically 0.0
/// everywhere, and asking for it is always a mistake rather than a choice.
pub async fn fetch_hrrr_data(
    client: &reqwest::Client,
    sources: &DataSources,
    param: &ModelParameter,
    run: (NaiveDate, u8),
    f_hour: u8,
) -> HrrrFetchResult {
    let (date, hour) = run;
    let f_hour = effective_forecast_hour(param, f_hour);

    let first = match try_fetch(client, sources, param, date, hour, f_hour).await {
        Ok(data) => return HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::warn!("HRRR fetch for {date} {hour:02}z failed: {e}, trying previous hour");
            e
        }
    };

    // The previous *run*, at the same forecast hour: the fallback trades an
    // hour of valid time for a run that is certainly published, which is the
    // ladder this function has always climbed.
    let (prev_date, prev_hour) = previous_run(date, hour);

    match try_fetch(client, sources, param, prev_date, prev_hour, f_hour).await {
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
fn round_verdict(parts: [FetchError; 2], context: &str) -> FetchError {
    let mut round = FetchError::of_round(&parts, context);
    if round.failure == FetchFailure::Absent {
        round.failure = FetchFailure::Transient;
    }
    round
}

/// Attempt a single HRRR fetch for a specific run and forecast hour.
async fn try_fetch(
    client: &reqwest::Client,
    sources: &DataSources,
    param: &ModelParameter,
    date: NaiveDate,
    hour: u8,
    f_hour: u8,
) -> Result<HrrrGridData, FetchError> {
    let bytes = fetch_record(
        client,
        sources,
        (date, hour),
        f_hour,
        param.grib_var(),
        param.grib_level(),
        &record_forecast(param, f_hour),
    )
    .await?;
    // A decode failure is transient by the module's own rule: a truncated body
    // and a changed product encoding are indistinguishable from one sample.
    parse_grib2(&bytes, *param, f_hour).map_err(classify_parse_error)
}

/// Fetch a composite HRRR parameter (e.g. bulk shear) that requires
/// multiple fields merged into one grid.
pub async fn fetch_composite_hrrr_data(
    client: &reqwest::Client,
    sources: &DataSources,
    param: &ModelParameter,
    run: (NaiveDate, u8),
    f_hour: u8,
) -> HrrrFetchResult {
    let parts = match param.composite_parts() {
        Some(p) => p,
        None => return fetch_hrrr_data(client, sources, param, run, f_hour).await,
    };

    let (date, hour) = run;
    let f_hour = effective_forecast_hour(param, f_hour);

    let first = match try_fetch_composite(client, sources, param, &parts, date, hour, f_hour).await
    {
        Ok(data) => return HrrrFetchResult(Ok(data)),
        Err(e) => {
            log::warn!(
                "HRRR composite fetch for {date} {hour:02}z failed: {e}, trying previous hour"
            );
            e
        }
    };

    let (prev_date, prev_hour) = previous_run(date, hour);

    match try_fetch_composite(client, sources, param, &parts, prev_date, prev_hour, f_hour).await {
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

/// Attempt a composite HRRR fetch for a specific run and forecast hour.
async fn try_fetch_composite(
    client: &reqwest::Client,
    sources: &DataSources,
    param: &ModelParameter,
    parts: &[(&str, &str)],
    date: NaiveDate,
    hour: u8,
    f_hour: u8,
) -> Result<HrrrGridData, FetchError> {
    let mut grids: Vec<HrrrGridData> = Vec::with_capacity(parts.len());
    let forecast = record_forecast(param, f_hour);

    for (var, level) in parts {
        let bytes =
            fetch_record(client, sources, (date, hour), f_hour, var, level, &forecast).await?;
        grids.push(parse_grib2(&bytes, *param, f_hour).map_err(classify_parse_error)?);
    }

    if grids.len() < 2 {
        return Err(FetchError::transient(
            "Composite requires at least 2 components",
        ));
    }

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
    let (visible_points, value_range) = super::summarize_values(&values, |v| param.paints(v));

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
