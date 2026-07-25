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
    log::info!("Fetching METARs for {} network(s): {states:?}", states.len());

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
            "METAR: {rejected_total} present-but-unparseable cell(s) — a schema \
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
        let Some(lat) = rejects.number("lat", &record.lat) else { continue };
        let Some(lon) = rejects.number("lon", &record.lon) else { continue };
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
            t.len() == 4 && t.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
}

/// IEM decodes visibility to a plain number, losing the "or more" bound: `10SM`
/// is the maximum a US METAR reports and ICAO `9999` means "10 km or more", so
/// neither is a measured value. Recovered from the raw text into `or_greater`.
fn visibility_from(miles: f64, raw: &str) -> Option<Visibility> {
    if !miles.is_finite() || miles < 0.0 {
        return None;
    }
    Some(Visibility { miles, or_greater: raw_visibility_is_a_bound(raw) })
}

/// Three spellings mean "or more": US `10SM`, the `P` prefix (`P6SM`), ICAO `9999`.
fn raw_visibility_is_a_bound(raw: &str) -> bool {
    raw.split_whitespace().any(|token| {
        token == "10SM"
            || token == "9999"
            || (token.starts_with('P') && token.ends_with("SM"))
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
fn resolve_wind_dir(
    raw_ob: &str,
    csv_dir: Option<u16>,
    csv_speed: Option<u16>,
) -> Option<WindDir> {
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

    if !(2..=3).contains(&speed_digits.len())
        || !speed_digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }

    Some((dir, speed_digits.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `?network=OK_ASOS` body, trimmed to the stations used below.
    const SAMPLE: &str = include_str!("testdata/currents.ok.json");

    fn sample() -> Vec<MetarOb> {
        parse_currents(SAMPLE).expect("fixture must parse").0
    }

    fn station(id: &str) -> MetarOb {
        sample()
            .into_iter()
            .find(|o| o.station_id == id)
            .unwrap_or_else(|| panic!("fixture has no station {id}"))
    }

    // ── Units ─────────────────────────────────────────────────────────────

    /// Fails if `tmpf` (°F) is relabelled rather than converted. Expected value
    /// is KOKC's own raw METAR group `34/22`, not a number this code produced.
    #[test]
    fn a_fahrenheit_temperature_is_converted_not_relabelled() {
        let okc = station("KOKC");
        assert!(okc.raw_ob.contains(" 34/22 "), "fixture must keep the trap");
        let c = okc.temp_c.expect("KOKC reports a temperature");
        assert!(
            (c - 33.888_889).abs() < 1e-4,
            "93 F is 33.89 C, got {c} — was the value relabelled rather than converted?",
        );
        // The METAR rounds to whole degrees; that is the independent check.
        assert_eq!(c.round(), 34.0);
        // Dewpoint: 71 F -> 21.67 C, METAR says 22.
        assert_eq!(okc.dewp_c.unwrap().round(), 22.0);
    }

    /// Hand-worked arithmetic, independent of any fixture.
    #[test]
    fn the_fahrenheit_conversion_is_the_real_formula() {
        assert_eq!(Fahrenheit(32.0).to_celsius(), 0.0);
        assert_eq!(Fahrenheit(212.0).to_celsius(), 100.0);
        assert_eq!(Fahrenheit(-40.0).to_celsius(), -40.0);
        assert!((Fahrenheit(90.0).to_celsius() - 32.222_222).abs() < 1e-5);
    }

    /// KOKC's `"alti": 30.04` and raw `A3004` agree, so the fixture pins inHg.
    #[test]
    fn an_inhg_altimeter_is_converted_to_hectopascals() {
        let okc = station("KOKC");
        assert!(okc.raw_ob.contains("A3004"), "fixture must keep the trap");
        let hpa = okc.altimeter_hpa.expect("KOKC reports an altimeter");
        // 30.04 inHg x 33.8639 = 1017.27 hPa, worked by hand.
        assert!((hpa - 1017.27).abs() < 0.01, "got {hpa} hPa");
        // Read as hPa directly it would have been ~30, i.e. 34x low.
        assert!(hpa > 900.0, "{hpa} looks like a raw inHg value");
    }

    /// Fails if `sknt` is integer-parsed: `u16::from_str("14.0")` blanks every
    /// wind speed in the feed.
    #[test]
    fn a_float_wind_speed_survives() {
        let okc = station("KOKC");
        assert!(okc.raw_ob.contains("20014G20KT"), "fixture must keep the trap");
        assert_eq!(okc.wind_speed_kt, Some(14), "sknt 14.0 must round to 14");
        assert_eq!(okc.wind_gust_kt, Some(20));
        assert_eq!(okc.wind_dir, Some(WindDir::Degrees(200)));
    }

    /// Fails if `null` ("not reported") is counted as a rejection.
    #[test]
    fn unusable_cells_are_counted_and_nulls_are_not() {
        let (obs, rejected) = parse_currents(SAMPLE).unwrap();
        assert_eq!(rejected, 0, "the real IEM fixture parses cleanly");
        assert!(!obs.is_empty());
        // "0 rejections" only means something if the fixture holds nulls in
        // fields this code reads. It does: 3 of 6 stations report no `gust`.
        assert!(
            SAMPLE.contains("\"gust\": null"),
            "fixture must contain a null in a field this code reads, or \
             `rejected == 0` says nothing about how nulls are treated",
        );
        let null_gusts = SAMPLE.matches("\"gust\": null").count();
        assert!(null_gusts >= 3, "expected several null gusts, got {null_gusts}");

        let broken = SAMPLE.replace("\"sknt\": 14.0", "\"sknt\": \"14 kt\"");
        assert_ne!(broken, SAMPLE, "the replacement must actually apply");
        let (broken_obs, rejected) = parse_currents(&broken).unwrap();
        assert_eq!(rejected, 1, "a string in a numeric cell is one rejection");
        let okc = broken_obs.iter().find(|o| o.station_id == "KOKC").unwrap();
        assert_eq!(okc.wind_speed_kt, None);
        // The rest of the record survives — that is why the field is `Value`.
        assert!(okc.temp_c.is_some(), "one bad cell must not drop the station");
    }

    // ── Identity ──────────────────────────────────────────────────────────

    /// Fails if the station id comes from IEM's local 3-letter `station` field
    /// instead of the ICAO callsign at the head of the raw report.
    #[test]
    fn the_station_id_is_the_icao_from_the_raw_report() {
        assert_eq!(icao_from_raw("KOKC 251652Z 20014G20KT"), Some("KOKC"));
        assert_eq!(icao_from_raw("METAR KTUL 251653Z 18012KT"), Some("KTUL"));
        assert_eq!(icao_from_raw("SPECI KLAW 251700Z"), Some("KLAW"));
        assert_eq!(icao_from_raw(""), None);
        assert_eq!(icao_from_raw("251652Z 20014G20KT"), None, "not a callsign");
        // End to end: the fixture's `station` field is "OKC".
        assert!(SAMPLE.contains("\"station\": \"OKC\""));
        assert_eq!(station("KOKC").station_id, "KOKC");
    }

    // ── Visibility ────────────────────────────────────────────────────────

    /// Fails if `10SM`/`9999`/`P6SM` collapse to a bare measured number.
    #[test]
    fn an_unrestricted_visibility_is_recovered_from_the_raw_report() {
        assert!(raw_visibility_is_a_bound("KOKC 251652Z 20014G20KT 10SM FEW250"));
        assert!(raw_visibility_is_a_bound("EGLL 251650Z 25008KT 9999 FEW035"));
        assert!(raw_visibility_is_a_bound("KXYZ 251650Z 25008KT P6SM SCT035"));
        assert!(!raw_visibility_is_a_bound("KUZA 251650Z 00000KT 2 1/2SM BR"));
        assert!(!raw_visibility_is_a_bound("KABC 251650Z 25008KT 5SM HZ"));

        let okc = station("KOKC");
        assert_eq!(okc.visibility.unwrap().miles, 10.0);
        assert!(okc.visibility.unwrap().or_greater, "10SM is a bound");
        assert_eq!(okc.visibility.unwrap().label(), "10+");
    }

    #[test]
    fn a_measured_visibility_is_not_marked_as_a_bound() {
        let v = visibility_from(2.5, "KUZA 251650Z 00000KT 2 1/2SM BR OVC004").unwrap();
        assert_eq!(v.miles, 2.5);
        assert!(!v.or_greater);
        assert_eq!(v.label(), "2.5");
    }

    #[test]
    fn a_nonsensical_visibility_is_rejected() {
        assert_eq!(visibility_from(-1.0, "x"), None);
        assert_eq!(visibility_from(f64::NAN, "x"), None);
        assert_eq!(visibility_from(f64::INFINITY, "x"), None);
    }

    // ── Ceiling and flight category ───────────────────────────────────────

    /// Fails if FEW/SCT count as a ceiling, which calls a clear day IFR.
    #[test]
    fn only_broken_or_worse_layers_form_a_ceiling() {
        let few_only = vec![CloudLayer { cover: "FEW".into(), base_ft: Some(2500) }];
        assert_eq!(ceiling_ft(&few_only), None, "FEW is not a ceiling");

        let sct_only = vec![CloudLayer { cover: "SCT".into(), base_ft: Some(1800) }];
        assert_eq!(ceiling_ft(&sct_only), None, "SCT is not a ceiling");

        let mixed = vec![
            CloudLayer { cover: "FEW".into(), base_ft: Some(800) },
            CloudLayer { cover: "SCT".into(), base_ft: Some(1500) },
            CloudLayer { cover: "BKN".into(), base_ft: Some(2500) },
            CloudLayer { cover: "OVC".into(), base_ft: Some(4000) },
        ];
        assert_eq!(
            ceiling_ft(&mixed),
            Some(2500),
            "the lowest BKN/OVC wins, and the lower FEW/SCT are ignored",
        );

        let obscured = vec![CloudLayer { cover: "VV".into(), base_ft: Some(200) }];
        assert_eq!(ceiling_ft(&obscured), Some(200), "VV is a ceiling");
    }

    /// KOKC's fixture record is `10SM FEW250`: no ceiling, good visibility.
    #[test]
    fn a_clear_report_with_good_visibility_is_vfr() {
        let okc = station("KOKC");
        assert_eq!(ceiling_ft(&okc.clouds), None);
        assert_eq!(okc.flight_category, Some(FlightCategory::VFR));
    }

    /// Both sides of every boundary. Values are FAA/AWC's, not this code's.
    #[test]
    fn flight_category_thresholds_match_the_faa_definitions() {
        let ceiling_only = |ft: u32| derive_flight_category(None, Some(ft));
        assert_eq!(ceiling_only(499), Some(FlightCategory::LIFR));
        assert_eq!(ceiling_only(500), Some(FlightCategory::IFR));
        assert_eq!(ceiling_only(999), Some(FlightCategory::IFR));
        assert_eq!(ceiling_only(1000), Some(FlightCategory::MVFR));
        assert_eq!(ceiling_only(3000), Some(FlightCategory::MVFR));
        assert_eq!(ceiling_only(3001), Some(FlightCategory::VFR));

        let vis_only = |m: f64| {
            derive_flight_category(Some(Visibility { miles: m, or_greater: false }), None)
        };
        assert_eq!(vis_only(0.5), Some(FlightCategory::LIFR));
        assert_eq!(vis_only(1.0), Some(FlightCategory::IFR));
        assert_eq!(vis_only(2.9), Some(FlightCategory::IFR));
        assert_eq!(vis_only(3.0), Some(FlightCategory::MVFR));
        assert_eq!(vis_only(5.0), Some(FlightCategory::MVFR));
        assert_eq!(vis_only(5.1), Some(FlightCategory::VFR));
    }

    /// Fails if the *better* of ceiling and visibility wins, or only one is read.
    #[test]
    fn the_worse_of_ceiling_and_visibility_decides_the_category() {
        let vis10 = Some(Visibility { miles: 10.0, or_greater: true });
        assert_eq!(
            derive_flight_category(vis10, Some(300)),
            Some(FlightCategory::LIFR),
            "a 300 ft ceiling is LIFR regardless of visibility",
        );
        let vis_half = Some(Visibility { miles: 0.5, or_greater: false });
        assert_eq!(
            derive_flight_category(vis_half, Some(25_000)),
            Some(FlightCategory::LIFR),
            "half a mile is LIFR regardless of ceiling",
        );
        // Mixed: MVFR ceiling, IFR visibility -> IFR.
        let vis2 = Some(Visibility { miles: 2.0, or_greater: false });
        assert_eq!(
            derive_flight_category(vis2, Some(1500)),
            Some(FlightCategory::IFR),
        );
    }

    #[test]
    fn a_report_with_neither_input_has_no_category() {
        assert_eq!(derive_flight_category(None, None), None);
    }

    // ── Wind ──────────────────────────────────────────────────────────────

    #[test]
    fn calm_and_variable_are_told_apart() {
        assert_eq!(resolve_wind_dir("K1 251650Z 00000KT", None, None), Some(WindDir::Calm));
        assert_eq!(resolve_wind_dir("K1 251650Z VRB03KT", None, None), Some(WindDir::Variable));
        assert_eq!(
            resolve_wind_dir("K1 251650Z 36003KT", None, None),
            Some(WindDir::Degrees(360)),
            "a genuine northerly is 360, never 0",
        );
    }

    /// Fails if the wind scan reads past `RMK`.
    #[test]
    fn a_vrb_in_the_remarks_does_not_make_the_station_variable() {
        let raw = "GCGM 251650Z 00000KT RMK R09/VRB07G21KT";
        assert_eq!(resolve_wind_dir(raw, Some(0), Some(0)), Some(WindDir::Calm));
    }

    #[test]
    fn wind_tokens_are_recognised_by_shape_not_by_substring() {
        assert_eq!(parse_wind_token("18006KT"), Some((Some(180), 6)));
        assert_eq!(parse_wind_token("20014G20KT"), Some((Some(200), 14)));
        assert_eq!(parse_wind_token("VRB03KT"), Some((None, 3)));
        assert_eq!(parse_wind_token("34002MPS"), Some((Some(340), 2)));
        for bad in ["E00000KT", "R09/VRB07G21KT", "9999", "A2986", "18006", "VRBKT"] {
            assert_eq!(parse_wind_token(bad), None, "{bad:?} is not a wind group");
        }
    }

    /// Fails if `000` with a speed is drawn as due north; `000` means calm.
    #[test]
    fn a_zero_bearing_with_speed_is_not_treated_as_north() {
        assert_eq!(classify_wind(Some(0), 25), WindDir::Variable);
        assert_eq!(classify_wind(Some(0), 0), WindDir::Calm);
        assert_eq!(classify_wind(Some(360), 5), WindDir::Degrees(360));
    }

    #[test]
    fn every_fixture_station_parses_and_keeps_its_raw_report() {
        let obs = sample();
        assert!(obs.len() >= 4, "fixture is too small to be meaningful");
        for o in &obs {
            assert!(!o.station_id.is_empty());
            assert!(!o.raw_ob.is_empty(), "{} lost its raw report", o.station_id);
            assert!(!o.obs_time.is_empty(), "{} lost its timestamp", o.station_id);
            assert!((-90.0..=90.0).contains(&o.lat));
            assert!((-180.0..=180.0).contains(&o.lon));
        }
    }

    /// Fails if a malformed body yields an empty result: that renders as "no
    /// observations" and hides an API change.
    #[test]
    fn a_malformed_body_is_an_error() {
        assert!(parse_currents("not json").is_err());
        assert!(parse_currents("{\"data\": 5}").is_err());
    }

    // ── Live checks ───────────────────────────────────────────────────────

    /// `cargo test -p rustdar-overlays -- --ignored --nocapture live_metar`
    #[ignore = "hits the live mesonet.agron.iastate.edu API"]
    #[tokio::test]
    async fn live_metar_fetch_carries_every_mapped_field() {
        let client = rustdar_radar::tls::simple_client(std::time::Duration::from_secs(60))
            .build()
            .expect("client");
        let sources = rustdar_radar::sources::DataSources::production();
        // Central Oklahoma — KTLX's neighbourhood.
        let view = GeoBounds { min_lat: 34.3, max_lat: 36.3, min_lon: -98.3, max_lon: -96.3 };

        let obs = fetch_current_metars(&client, &sources, &view)
            .await
            .expect("METAR fetch must succeed");
        println!("fetched {} observations", obs.len());
        assert!(obs.len() > 20, "expected a state's worth of ASOS sites");

        // A field that is `None` for every station is the silent-column failure.
        let has = |f: &dyn Fn(&MetarOb) -> bool| obs.iter().filter(|o| f(o)).count();
        for (name, count) in [
            ("temp_c", has(&|o| o.temp_c.is_some())),
            ("dewp_c", has(&|o| o.dewp_c.is_some())),
            ("wind_dir", has(&|o| o.wind_dir.is_some())),
            ("wind_speed_kt", has(&|o| o.wind_speed_kt.is_some())),
            ("visibility", has(&|o| o.visibility.is_some())),
            ("altimeter_hpa", has(&|o| o.altimeter_hpa.is_some())),
            ("flight_category", has(&|o| o.flight_category.is_some())),
            ("raw_ob", has(&|o| !o.raw_ob.is_empty())),
            ("obs_time", has(&|o| !o.obs_time.is_empty())),
        ] {
            println!("  {name}: {count}/{}", obs.len());
            assert!(count > 0, "{name} is None for every station");
        }

        // A relabelled `tmpf` reads 60-110 across most of the US in summer.
        for o in obs.iter().filter(|o| o.temp_c.is_some()) {
            let c = o.temp_c.unwrap();
            assert!(
                (-60.0..=60.0).contains(&c),
                "{} reports {c} C — that is a Fahrenheit value in a Celsius field",
                o.station_id,
            );
        }
        for o in obs.iter().filter(|o| o.altimeter_hpa.is_some()) {
            let hpa = o.altimeter_hpa.unwrap();
            assert!(
                (870.0..=1090.0).contains(&hpa),
                "{} reports {hpa} hPa — that is an inHg value in a hPa field",
                o.station_id,
            );
        }
    }

    /// Fails if a network is added or removed upstream, which otherwise shows
    /// up only as a state quietly missing from the map.
    #[ignore = "hits the live mesonet.agron.iastate.edu API"]
    #[tokio::test]
    async fn live_networks_table_matches_iems_own_extents() {
        let client = rustdar_radar::tls::simple_client(std::time::Duration::from_secs(60))
            .build()
            .expect("client");
        let body = client
            .get("https://mesonet.agron.iastate.edu/api/1/networks.json")
            .send()
            .await
            .expect("networks.json")
            .text()
            .await
            .expect("body");

        #[derive(serde::Deserialize)]
        struct Net {
            id: String,
        }
        #[derive(serde::Deserialize)]
        struct Nets {
            data: Vec<Net>,
        }
        let nets: Nets = serde_json::from_str(&body).expect("networks.json parses");

        let upstream: std::collections::HashSet<String> = nets
            .data
            .iter()
            .filter_map(|n| n.id.strip_suffix("_ASOS"))
            .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_uppercase()))
            .map(str::to_string)
            .collect();
        let ours: std::collections::HashSet<String> = networks::NETWORKS
            .iter()
            .map(|n| n.state.to_string())
            .collect();

        let missing: Vec<_> = upstream.difference(&ours).collect();
        let extra: Vec<_> = ours.difference(&upstream).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "networks table has drifted from IEM: missing {missing:?}, extra {extra:?}",
        );
    }
}
