//! Current METAR observations from the Iowa Environmental Mesonet:
//! `mesonet.agron.iastate.edu/api/1/currents.json?network=<ST>_ASOS`.
//!
//! Not `aviationweather.gov/data/cache/metars.cache.csv.gz`: verified
//! 2026-07-25 with `curl -H 'Origin: https://example.com'`, it answers `200`
//! with no `Access-Control-Allow-Origin` at all, so the web build cannot use it.
//!
//! # The request must stay "simple"
//!
//! Probed with curl: `mesonet.agron.iastate.edu` answers an OPTIONS preflight
//! with `405 Method Not Allowed`, but answers the plain `GET` with
//! `Access-Control-Allow-Origin: *`. Any non-safelisted request header —
//! `User-Agent` included — makes the browser preflight, and the request then
//! never happens. Hence [`rustdar_radar::tls::simple_client`] and
//! [`rustdar_radar::sources::DataSources::metar_sends_user_agent`] `== false`.
//!
//! Requests are viewport-scoped (see [`super::networks`]) because the
//! whole-network form is 54 MB ungzipped.
//!
//! # UNIT HAZARD — read before mapping a new field
//!
//! IEM reports neither Celsius nor hectopascals, unlike AWC's CSV. Each of
//! these silently produced a plausible wrong answer:
//!
//!   * `tmpf` / `dwpf` are **°F**. Read as `temp_c`, 90 °F renders as 194 °F.
//!   * `alti` is **inHg**. Read as hPa it is ~34× low.
//!   * `sknt` is a **float** (`14.0`). Parsed as `u16` it is rejected, and
//!     every wind speed in the feed becomes `None`.
//!
//! Guarded by putting the unit in the *type*: [`Fahrenheit`] and
//! [`InchesOfMercury`] can only reach [`MetarOb::temp_c`] /
//! [`MetarOb::altimeter_hpa`] through a named conversion. The `sknt` shape is
//! caught by [`Rejections`]; a column that silently empties is invisible.

use std::cell::{Cell, RefCell};

use serde::Deserialize;
use serde_json::Value;

use super::networks;
use super::types::{CloudLayer, FlightCategory, MetarOb, Visibility, WindDir};
use crate::types::GeoBounds;

// ── Units ─────────────────────────────────────────────────────────────────
//
// No `Deref`, no `From`: the only way out is the named conversion, so the
// conversion is visible at the assignment site. See the UNIT HAZARD note above.

/// IEM's temperature unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fahrenheit(pub f64);

impl Fahrenheit {
    pub fn to_celsius(self) -> f64 {
        (self.0 - 32.0) * 5.0 / 9.0
    }
}

/// IEM's pressure unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InchesOfMercury(pub f64);

impl InchesOfMercury {
    /// 1 inHg = 33.8639 hPa.
    pub fn to_hpa(self) -> f64 {
        self.0 * 33.8639
    }
}

// ── Rejection counting ────────────────────────────────────────────────────

/// Counts cells that were *present* but not a finite number, i.e. an upstream
/// schema or unit change. `null` is not a rejection: IEM writes it for "not
/// reported". Without this, such a change empties a column silently.
#[derive(Debug, Default)]
struct Rejections {
    count: Cell<u32>,
    sample: RefCell<String>,
}

impl Rejections {
    fn note(&self, field: &str, value: &Value) {
        self.count.set(self.count.get() + 1);
        let mut sample = self.sample.borrow_mut();
        if sample.is_empty() {
            *sample = format!("{field}={value}");
        }
    }

    fn count(&self) -> u32 {
        self.count.get()
    }

    /// Absent and explicitly-`null` cells arrive identically: serde maps JSON
    /// `null` to `None` for `Option<Value>`, so there is no `Value::Null` left
    /// to test. Both mean "not reported" and must not count as rejections.
    fn number(&self, field: &str, cell: &Option<Value>) -> Option<f64> {
        let value = cell.as_ref()?;
        match value.as_f64() {
            Some(n) if n.is_finite() => Some(n),
            _ => {
                self.note(field, value);
                None
            }
        }
    }
}

// ── Wire format ───────────────────────────────────────────────────────────

/// IEM's `currents.json` envelope: a pandas `orient="table"` dump.
#[derive(Debug, Deserialize)]
struct CurrentsResponse {
    #[serde(default)]
    data: Vec<Record>,
}

/// Numeric fields are `Value`, not `f64`: a type change upstream would abort
/// deserialization of the *whole* response and lose every station in the state.
#[derive(Debug, Deserialize)]
struct Record {
    #[serde(default)]
    station: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    lat: Option<Value>,
    #[serde(default)]
    lon: Option<Value>,
    /// Degrees **Fahrenheit**.
    #[serde(default)]
    tmpf: Option<Value>,
    /// Degrees **Fahrenheit**.
    #[serde(default)]
    dwpf: Option<Value>,
    /// Wind direction, degrees true.
    #[serde(default)]
    drct: Option<Value>,
    /// Wind speed in knots — a **float**.
    #[serde(default)]
    sknt: Option<Value>,
    /// Gust in knots.
    #[serde(default)]
    gust: Option<Value>,
    /// Visibility in statute miles.
    #[serde(default)]
    vsby: Option<Value>,
    /// Altimeter setting in **inches of mercury**.
    #[serde(default)]
    alti: Option<Value>,
    /// Present weather codes.
    #[serde(default)]
    wxcodes: Option<Value>,
    /// The raw METAR text.
    #[serde(default)]
    raw: Option<String>,
    /// Observation time, ISO 8601 UTC.
    #[serde(default)]
    utc_valid: Option<String>,
    #[serde(default)]
    skyc1: Option<String>,
    #[serde(default)]
    skyl1: Option<Value>,
    #[serde(default)]
    skyc2: Option<String>,
    #[serde(default)]
    skyl2: Option<Value>,
    #[serde(default)]
    skyc3: Option<String>,
    #[serde(default)]
    skyl3: Option<Value>,
    #[serde(default)]
    skyc4: Option<String>,
    #[serde(default)]
    skyl4: Option<Value>,
}

// ── Fetch ─────────────────────────────────────────────────────────────────

/// One request per state network the viewport overlaps, concurrently. A failed
/// network is skipped, not fatal, unless every one fails.
pub async fn fetch_current_metars(
    client: &reqwest::Client,
    sources: &rustdar_radar::sources::DataSources,
    viewport: &GeoBounds,
) -> Result<Vec<MetarOb>, String> {
    let states = networks::networks_for_viewport(viewport);
    if states.is_empty() {
        log::info!("METAR: viewport overlaps no ASOS network");
        return Ok(Vec::new());
    }
    log::info!(
        "Fetching METARs for {} network(s): {states:?}",
        states.len()
    );

    let requests = states.iter().map(|state| {
        let url = sources.metar_state_url(state);
        async move {
            let body = client
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("{state}: request failed: {e}"))?
                .error_for_status()
                .map_err(|e| format!("{state}: {e}"))?
                .text()
                .await
                .map_err(|e| format!("{state}: body read failed: {e}"))?;
            parse_currents(&body).map_err(|e| format!("{state}: {e}"))
        }
    });

    let results = futures::future::join_all(requests).await;

    let mut all = Vec::new();
    let mut rejected_total = 0u32;
    let mut failures = 0usize;
    for result in results {
        match result {
            Ok((obs, rejected)) => {
                all.extend(obs);
                rejected_total += rejected;
            }
            Err(e) => {
                failures += 1;
                log::warn!("METAR network fetch failed: {e}");
            }
        }
    }

    if failures == states.len() {
        return Err(format!("all {failures} METAR network fetches failed"));
    }

    if rejected_total > 0 {
        log::warn!(
            "METAR: {rejected_total} present-but-unparseable cell(s) - a schema \
             or unit change upstream?"
        );
    }
    log::info!("Parsed {} METAR observations", all.len());
    Ok(all)
}

/// Returns observations plus the count of present-but-unusable cells. The
/// count is a return value, not just a log line, because tests assert on it.
fn parse_currents(body: &str) -> Result<(Vec<MetarOb>, u32), String> {
    let response: CurrentsResponse =
        serde_json::from_str(body).map_err(|e| format!("bad currents.json: {e}"))?;

    let rejects = Rejections::default();
    let mut observations = Vec::with_capacity(response.data.len());

    for record in &response.data {
        let Some(lat) = rejects.number("lat", &record.lat) else {
            continue;
        };
        let Some(lon) = rejects.number("lon", &record.lon) else {
            continue;
        };
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }

        let raw_ob = record.raw.clone().unwrap_or_default();

        // IEM keys on the local 3-letter id ("OKC"); the ICAO callsign ("KOKC")
        // is the first token of the raw report. The app wants the ICAO.
        let station_id = icao_from_raw(&raw_ob)
            .map(str::to_string)
            .unwrap_or_else(|| record.station.clone());
        if station_id.is_empty() {
            continue;
        }

        let temp_c = rejects
            .number("tmpf", &record.tmpf)
            .map(|v| Fahrenheit(v).to_celsius());
        let dewp_c = rejects
            .number("dwpf", &record.dwpf)
            .map(|v| Fahrenheit(v).to_celsius());
        let altimeter_hpa = rejects
            .number("alti", &record.alti)
            .map(|v| InchesOfMercury(v).to_hpa());

        // `sknt` is a float: round, do not parse. `u16::from_str("14.0")` fails
        // and blanks the whole column.
        let wind_speed_kt = rejects
            .number("sknt", &record.sknt)
            .map(|v| v.round() as u16);
        let wind_gust_kt = rejects
            .number("gust", &record.gust)
            .map(|v| v.round() as u16);
        let csv_dir = rejects
            .number("drct", &record.drct)
            .map(|v| v.round() as u16);

        let clouds = cloud_layers(record, &rejects);
        let visibility = rejects
            .number("vsby", &record.vsby)
            .and_then(|miles| visibility_from(miles, &raw_ob));

        observations.push(MetarOb {
            station_id,
            name: record
                .name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| record.station.clone()),
            lat,
            lon,
            // IEM's currents feed carries no station elevation.
            elev_m: None,
            temp_c,
            dewp_c,
            wind_dir: resolve_wind_dir(&raw_ob, csv_dir, wind_speed_kt),
            wind_speed_kt,
            wind_gust_kt,
            visibility,
            altimeter_hpa,
            // Not reported by IEM; derived.
            flight_category: derive_flight_category(visibility, ceiling_ft(&clouds)),
            raw_ob,
            clouds,
            wx_string: record
                .wxcodes
                .as_ref()
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            obs_time: record.utc_valid.clone().unwrap_or_default(),
        });
    }

    if rejects.count() > 0 {
        log::warn!(
            "METAR: dropped {} present-but-unparseable cell(s) (first: {:?})",
            rejects.count(),
            rejects.sample.borrow(),
        );
    }

    Ok((observations, rejects.count()))
}

/// `"KOKC 251652Z 20014G20KT ..."` → `"KOKC"`. Some reports lead with a
/// `METAR`/`SPECI` keyword, which is skipped.
fn icao_from_raw(raw: &str) -> Option<&str> {
    raw.split_whitespace()
        .find(|t| !matches!(*t, "METAR" | "SPECI"))
        .filter(|t| {
            t.len() == 4
                && t.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
}

/// IEM decodes visibility to a plain number, losing the "or more" bound: `10SM`
/// is the maximum a US METAR reports and ICAO `9999` means "10 km or more", so
/// neither is a measured value. Recovered from the raw text into `or_greater`.
fn visibility_from(miles: f64, raw: &str) -> Option<Visibility> {
    if !miles.is_finite() || miles < 0.0 {
        return None;
    }
    Some(Visibility {
        miles,
        or_greater: raw_visibility_is_a_bound(raw),
    })
}

/// Three spellings mean "or more": US `10SM`, the `P` prefix (`P6SM`), ICAO `9999`.
fn raw_visibility_is_a_bound(raw: &str) -> bool {
    raw.split_whitespace().any(|token| {
        token == "10SM" || token == "9999" || (token.starts_with('P') && token.ends_with("SM"))
    })
}

/// IEM reports at most four `skyc`/`skyl` slots.
fn cloud_layers(record: &Record, rejects: &Rejections) -> Vec<CloudLayer> {
    let slots = [
        (&record.skyc1, &record.skyl1, "skyl1"),
        (&record.skyc2, &record.skyl2, "skyl2"),
        (&record.skyc3, &record.skyl3, "skyl3"),
        (&record.skyc4, &record.skyl4, "skyl4"),
    ];
    slots
        .iter()
        .filter_map(|(cover, level, field)| {
            let cover = cover.as_ref()?.trim();
            if cover.is_empty() {
                return None;
            }
            Some(CloudLayer {
                cover: cover.to_string(),
                base_ft: rejects.number(field, level).map(|v| v.round() as u32),
            })
        })
        .collect()
}

/// Base of the lowest BKN/OVC layer, feet AGL. FEW and SCT are *not* ceilings
/// (sky still visible through them); `VV` (vertical visibility) is.
fn ceiling_ft(clouds: &[CloudLayer]) -> Option<u32> {
    clouds
        .iter()
        .filter(|l| matches!(l.cover.as_str(), "BKN" | "OVC" | "VV" | "OVX"))
        .filter_map(|l| l.base_ft)
        .min()
}

/// IEM reports no flight category. Thresholds are the FAA's (AIM 7-1-8 / AWC);
/// the **worse** of ceiling and visibility decides.
///
/// ```text
///            ceiling (ft AGL)          visibility (statute miles)
///   LIFR     < 500                     < 1
///   IFR      500 to < 1000             1 to < 3
///   MVFR     1000 to 3000              3 to 5
///   VFR      > 3000                    > 5
/// ```
///
/// `None` only when *neither* input is available.
fn derive_flight_category(
    visibility: Option<Visibility>,
    ceiling: Option<u32>,
) -> Option<FlightCategory> {
    let from_ceiling = ceiling.map(|ft| {
        if ft < 500 {
            FlightCategory::LIFR
        } else if ft < 1000 {
            FlightCategory::IFR
        } else if ft <= 3000 {
            FlightCategory::MVFR
        } else {
            FlightCategory::VFR
        }
    });

    let from_visibility = visibility.map(|v| {
        // An "or greater" report is a lower bound; using the bound itself is
        // the conservative reading, and matches AWC's published category.
        let m = v.miles;
        if m < 1.0 {
            FlightCategory::LIFR
        } else if m < 3.0 {
            FlightCategory::IFR
        } else if m <= 5.0 {
            FlightCategory::MVFR
        } else {
            FlightCategory::VFR
        }
    });

    match (from_ceiling, from_visibility) {
        (Some(a), Some(b)) => Some(worse(a, b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Worst first, so the pair-wise minimum is the reported category.
fn severity(c: FlightCategory) -> u8 {
    match c {
        FlightCategory::LIFR => 0,
        FlightCategory::IFR => 1,
        FlightCategory::MVFR => 2,
        FlightCategory::VFR => 3,
    }
}

fn worse(a: FlightCategory, b: FlightCategory) -> FlightCategory {
    if severity(a) <= severity(b) { a } else { b }
}

// ── Wind direction ────────────────────────────────────────────────────────

/// Prefers the raw METAR text: the numeric column reports `0` for both calm and
/// variable, while a genuine northerly is `360`. The raw report distinguishes
/// them outright — `00000KT` versus `VRBnnKT`.
fn resolve_wind_dir(raw_ob: &str, csv_dir: Option<u16>, csv_speed: Option<u16>) -> Option<WindDir> {
    if let Some((dir, speed)) = raw_wind_group(raw_ob) {
        return Some(classify_wind(dir, speed));
    }
    csv_dir.map(|d| classify_wind(Some(d), csv_speed.unwrap_or(0)))
}

/// `dir == None` means the source said `VRB` explicitly.
fn classify_wind(dir: Option<u16>, speed: u16) -> WindDir {
    match dir {
        None => WindDir::Variable,
        Some(0) if speed == 0 => WindDir::Calm,
        // `000` with a non-zero speed is not a legal METAR bearing — `000` is
        // reserved for calm — so it is not "due north". Draw no bearing.
        Some(0) => WindDir::Variable,
        Some(d) => WindDir::Degrees(d),
    }
}

/// Stops at `RMK`/`TEMPO`/`BECMG`/`NOSIG`: those sections carry *other* winds.
/// `00000KT ... RMK R09/VRB07G21KT` is dead calm, not variable.
fn raw_wind_group(raw_ob: &str) -> Option<(Option<u16>, u16)> {
    for token in raw_ob.split_whitespace() {
        if matches!(token, "RMK" | "TEMPO" | "BECMG" | "NOSIG") {
            return None;
        }
        if let Some(found) = parse_wind_token(token) {
            return Some(found);
        }
    }
    None
}

/// Parse one `dddffKT` / `VRBffKT` token (optionally `Gff`, in KT/MPS/KMH).
fn parse_wind_token(token: &str) -> Option<(Option<u16>, u16)> {
    let body = token
        .strip_suffix("KT")
        .or_else(|| token.strip_suffix("MPS"))
        .or_else(|| token.strip_suffix("KMH"))?;

    let body = match body.split_once('G') {
        Some((before, gust)) => {
            if gust.is_empty() || !gust.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            before
        }
        None => body,
    };

    let (dir, speed_digits) = match body.strip_prefix("VRB") {
        Some(rest) => (None, rest),
        None => {
            if body.len() < 5 {
                return None;
            }
            let (d, rest) = body.split_at(3);
            if !d.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (Some(d.parse::<u16>().ok()?), rest)
        }
    };

    if !(2..=3).contains(&speed_digits.len()) || !speed_digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    Some((dir, speed_digits.parse().ok()?))
}

// Native-only: the live IEM checks at the tail are `#[tokio::test]`, and that
// dev-dependency is target-gated off wasm32.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
