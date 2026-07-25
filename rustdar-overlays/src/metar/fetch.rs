//! Fetch current METAR observations from the Aviation Weather Center bulk cache file.

use std::io::Read;

use flate2::read::GzDecoder;

use super::types::{CloudLayer, FlightCategory, MetarOb, Visibility, WindDir};

const METAR_CACHE_URL: &str =
    "https://aviationweather.gov/data/cache/metars.cache.csv.gz";

/// Fetch current METAR observations from the AWC bulk cache file.
///
/// Downloads a gzip-compressed CSV containing all current worldwide METARs,
/// decompresses it, and parses via header-driven column mapping.
pub async fn fetch_current_metars(
    client: &reqwest::Client,
) -> Result<Vec<MetarOb>, String> {
    log::info!("Fetching METAR cache from AWC");

    let bytes = client
        .get(METAR_CACHE_URL)
        .send()
        .await
        .map_err(|e| format!("METAR cache fetch failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("AWC returned error: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("Failed to read METAR cache bytes: {e}"))?;

    let mut decoder = GzDecoder::new(&bytes[..]);
    let mut csv_text = String::new();
    decoder
        .read_to_string(&mut csv_text)
        .map_err(|e| format!("Failed to decompress METAR cache: {e}"))?;

    parse_metar_csv(&csv_text)
}

/// Parse AWC METAR cache CSV into `MetarOb` structs.
///
/// Supports both old ADDS/TDS column names and AWC v4 names via fallback mapping.
fn parse_metar_csv(csv: &str) -> Result<Vec<MetarOb>, String> {
    let mut lines = csv.lines();

    // Find the header line — skip comment/info lines.
    let header_line = loop {
        match lines.next() {
            Some(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty()
                    || trimmed.starts_with('!')
                    || trimmed.starts_with('#')
                {
                    continue;
                }
                // Header should contain a known column name.
                if trimmed.contains("station_id")
                    || trimmed.contains("icaoId")
                    || trimmed.contains("raw_text")
                    || trimmed.contains("rawOb")
                {
                    break trimmed;
                }
                // Skip other preamble lines (e.g. "No errors", "N results").
                continue;
            }
            None => return Err("No header line found in METAR CSV".to_string()),
        }
    };

    let columns: Vec<&str> = header_line.split(',').map(|s| s.trim()).collect();

    // Find first matching column index for a list of candidate names.
    let col_idx = |names: &[&str]| -> Option<usize> {
        for name in names {
            if let Some(i) = columns.iter().position(|c| *c == *name) {
                return Some(i);
            }
        }
        None
    };

    let i_station = col_idx(&["station_id", "icaoId"]);
    let i_lat = col_idx(&["latitude", "lat"]);
    let i_lon = col_idx(&["longitude", "lon"]);
    let i_temp = col_idx(&["temp_c", "temp"]);
    let i_dewp = col_idx(&["dewpoint_c", "dewp"]);
    let i_wdir = col_idx(&["wind_dir_degrees", "wdir"]);
    let i_wspd = col_idx(&["wind_speed_kt", "wspd"]);
    let i_wgst = col_idx(&["wind_gust_kt", "wgst"]);
    let i_vis = col_idx(&["visibility_statute_mi", "visib"]);
    let i_altim = col_idx(&["altim_in_hg", "altim"]);
    let i_fltcat = col_idx(&["flight_category", "fltcat"]);
    let i_raw = col_idx(&["raw_text", "rawOb"]);
    let i_wx = col_idx(&["wx_string", "wxString"]);
    let i_time = col_idx(&["observation_time", "obsTime", "reportTime"]);
    let i_elev = col_idx(&["elevation_m", "elev"]);
    let i_name = col_idx(&["name", "station_name"]);

    // Detect repeated sky_cover / cloud_base_ft_agl pairs (old ADDS format).
    let cloud_cols: Vec<(usize, Option<usize>)> = {
        let mut pairs = Vec::new();
        for (i, col) in columns.iter().enumerate() {
            if *col == "sky_cover" {
                let base = if columns.get(i + 1).is_some_and(|c| *c == "cloud_base_ft_agl") {
                    Some(i + 1)
                } else {
                    None
                };
                pairs.push((i, base));
            }
        }
        pairs
    };

    // altim_in_hg means values are in inches of mercury; otherwise assume hPa.
    let altim_is_inhg = columns.contains(&"altim_in_hg");

    let Some(i_station) = i_station else {
        return Err("Missing station_id/icaoId column".to_string());
    };
    let Some(i_lat) = i_lat else {
        return Err("Missing latitude/lat column".to_string());
    };
    let Some(i_lon) = i_lon else {
        return Err("Missing longitude/lon column".to_string());
    };

    let min_fields = i_station.max(i_lat).max(i_lon) + 1;
    let mut observations = Vec::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('!')
            || trimmed.starts_with('#')
        {
            continue;
        }

        let fields = parse_csv_line(trimmed);
        if fields.len() < min_fields {
            continue;
        }

        let station_id = fields[i_station].to_string();
        if station_id.is_empty() {
            continue;
        }

        let Ok(lat) = fields[i_lat].parse::<f64>() else { continue };
        let Ok(lon) = fields[i_lon].parse::<f64>() else { continue };
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }

        let get_f64 = |idx: Option<usize>| -> Option<f64> {
            idx.and_then(|i| fields.get(i))
                .and_then(|s| if s.is_empty() { None } else { s.parse().ok() })
        };
        let get_u16 = |idx: Option<usize>| -> Option<u16> {
            idx.and_then(|i| fields.get(i))
                .and_then(|s| if s.is_empty() { None } else { s.parse().ok() })
        };
        let get_str = |idx: Option<usize>| -> String {
            idx.and_then(|i| fields.get(i))
                .map(|s| s.to_string())
                .unwrap_or_default()
        };

        let name = i_name
            .and_then(|i| fields.get(i))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| station_id.clone());

        let altimeter_hpa = get_f64(i_altim).map(|v| {
            if altim_is_inhg { v * 33.8639 } else { v }
        });

        let flight_category = i_fltcat
            .and_then(|i| fields.get(i))
            .and_then(|s| parse_flight_category(s));

        let wx_string = i_wx
            .and_then(|i| fields.get(i))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let clouds: Vec<CloudLayer> = cloud_cols
            .iter()
            .filter_map(|(cover_i, base_i)| {
                let cover = fields.get(*cover_i)?.to_string();
                if cover.is_empty() {
                    return None;
                }
                let base_ft = base_i
                    .and_then(|bi| fields.get(bi))
                    .and_then(|s| if s.is_empty() { None } else { s.parse().ok() });
                Some(CloudLayer { cover, base_ft })
            })
            .collect();

        let raw_ob = get_str(i_raw);
        let wind_speed_kt = get_u16(i_wspd);

        observations.push(MetarOb {
            station_id,
            name,
            lat,
            lon,
            elev_m: get_f64(i_elev),
            temp_c: get_f64(i_temp),
            dewp_c: get_f64(i_dewp),
            // NOT the raw column: `wind_dir_degrees=0` means calm *or* variable,
            // and reading it as a bearing pointed a barb due north for a quarter
            // of the feed. See `resolve_wind_dir`.
            wind_dir: resolve_wind_dir(&raw_ob, get_u16(i_wdir), wind_speed_kt),
            wind_speed_kt,
            wind_gust_kt: get_u16(i_wgst),
            // NOT `get_f64`: AWC writes an unrestricted visibility as `10+` /
            // `6+`, which `f64::from_str` rejects. Those are 78.5% of the feed's
            // non-empty values, so parsing them as a plain number blanked the
            // field for four stations in five — and specifically for the ones
            // with *good* visibility.
            visibility: i_vis
                .and_then(|i| fields.get(i))
                .and_then(|s| Visibility::parse(s)),
            altimeter_hpa,
            flight_category,
            raw_ob,
            clouds,
            wx_string,
            obs_time: get_str(i_time),
        });
    }

    log::info!("Parsed {} METAR observations from cache", observations.len());
    Ok(observations)
}

// ── Wind direction ────────────────────────────────────────────────────────

/// Decide a report's wind direction, preferring the raw METAR text.
///
/// The `wind_dir_degrees` column cannot answer this on its own: AWC writes `0`
/// for calm *and* for variable and never leaves it empty for either, while a
/// genuine northerly is reported as `360`. The raw report says which it is
/// outright — `00000KT` versus `VRBnnKT` — so that is the primary source.
///
/// The column is the fallback for reports with no parseable wind group (60 of
/// 4,933 rows in the measured cache, 59 of them with an empty direction).
/// Measured over that same cache the two sources agreed on **4,873 of 4,873**
/// rows that had both, so the fallback rule below is the column's convention
/// applied faithfully, not a second guess.
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

/// Turn a `(direction, speed)` pair into the three-way distinction.
///
/// `dir == None` means the source said `VRB` explicitly.
fn classify_wind(dir: Option<u16>, speed: u16) -> WindDir {
    match dir {
        None => WindDir::Variable,
        Some(0) if speed == 0 => WindDir::Calm,
        // A `000` bearing with a non-zero speed is not a legal METAR direction —
        // `000` is reserved for calm. Two Canadian AUTO stations reported
        // `00025KT` and `00022KT` in the measured cache. Whatever the sensor
        // meant, it is not "due north", so refuse to draw a bearing for it.
        Some(0) => WindDir::Variable,
        Some(d) => WindDir::Degrees(d),
    }
}

/// Extract `(direction, speed)` from a raw METAR's wind group.
///
/// Returns the direction as `None` for an explicit `VRB`.
///
/// Scanning stops at `RMK`/`TEMPO`/`BECMG`/`NOSIG`, because those sections
/// carry *other* winds: `GCGM` reports `00000KT` but has `R09/VRB07G21KT` in
/// its remarks, so a plain substring search for "VRB" would call a dead-calm
/// station variable. Taking the first match also keeps forecast groups such as
/// `... 04004KT ... TEMPO VRB15G25KT` from overriding the observed wind.
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
///
/// Speed units are deliberately not converted: only the direction is read
/// here, and the speed is used solely to separate calm from variable.
fn parse_wind_token(token: &str) -> Option<(Option<u16>, u16)> {
    let body = token
        .strip_suffix("KT")
        .or_else(|| token.strip_suffix("MPS"))
        .or_else(|| token.strip_suffix("KMH"))?;

    // Drop the gust suffix; it plays no part in the direction.
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

/// Parse a single CSV line, handling quoted fields.
fn parse_csv_line(line: &str) -> Vec<&str> {
    if !line.contains('"') {
        return line.split(',').collect();
    }
    let mut fields = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    let bytes = line.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'"' {
            in_quotes = !in_quotes;
        } else if bytes[i] == b',' && !in_quotes {
            fields.push(line[start..i].trim_matches('"'));
            start = i + 1;
        }
    }
    fields.push(line[start..].trim_matches('"'));
    fields
}

fn parse_flight_category(s: &str) -> Option<FlightCategory> {
    match s {
        "VFR" => Some(FlightCategory::VFR),
        "MVFR" => Some(FlightCategory::MVFR),
        "IFR" => Some(FlightCategory::IFR),
        "LIFR" => Some(FlightCategory::LIFR),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Thirteen verbatim rows from a live `metars.cache.csv.gz`, chosen to cover
    /// every shape the live-data bugs turned on: `10+`, `6+`, a fractional
    /// measurement, a measurement above 10, an empty visibility, calm, fast and
    /// slow `VRB`, a genuine northerly, an `RMK`-only `VRB`, a `000` bearing
    /// carrying speed, metric `MPS`, and a report with no wind group at all.
    const SAMPLE: &str = include_str!("testdata/metars.sample.csv");

    fn sample() -> Vec<MetarOb> {
        parse_metar_csv(SAMPLE).expect("fixture must parse")
    }

    fn station(id: &str) -> MetarOb {
        sample()
            .into_iter()
            .find(|o| o.station_id == id)
            .unwrap_or_else(|| panic!("fixture has no station {id}"))
    }

    #[test]
    fn every_fixture_row_survives_parsing() {
        assert_eq!(sample().len(), 13, "fixture rows must all be accepted");
    }

    /// `10+` and `6+` are 78.5% of the live feed's non-empty visibilities.
    /// Parsing them straight into `f64` yields `Err`, and the `.ok()` that used
    /// to follow turned four of every five stations blank.
    #[test]
    fn an_unrestricted_visibility_survives_its_trailing_plus() {
        assert_eq!(
            Visibility::parse("10+"),
            Some(Visibility { miles: 10.0, or_greater: true })
        );
        assert_eq!(
            Visibility::parse("6+"),
            Some(Visibility { miles: 6.0, or_greater: true })
        );
        assert_eq!(station("KEDU").visibility.unwrap().miles, 10.0);
        assert_eq!(station("LTAR").visibility.unwrap().miles, 6.0);
    }

    /// The `+` is the whole point: dropping it would make `10+` and a measured
    /// `10` indistinguishable, which is what the stale `Option<f64>` did.
    #[test]
    fn or_greater_separates_a_bound_from_a_measurement() {
        assert!(Visibility::parse("10+").unwrap().or_greater);
        assert!(!Visibility::parse("15").unwrap().or_greater);
        assert!(station("KEDU").visibility.unwrap().or_greater, "10+ is a bound");
        assert!(!station("CYYH").visibility.unwrap().or_greater, "15SM is measured");
        assert!(!station("KUZA").visibility.unwrap().or_greater, "2.5 is measured");
    }

    /// A measurement of 15 statute miles is not "10+"; the live cache carries 77
    /// such rows (Canadian `15SM`, and metric reports converted from km), so the
    /// `>= 10.0` branches were rare rather than strictly dead — and rendering
    /// them as "10+" understated a real observation.
    #[test]
    fn a_measurement_above_ten_renders_as_itself_not_as_ten_plus() {
        assert_eq!(Visibility::parse("15").unwrap().label(), "15");
        assert_eq!(Visibility::parse("12.43").unwrap().label(), "12.4");
        assert_eq!(station("CYYH").visibility.unwrap().label(), "15");
    }

    #[test]
    fn visibility_labels_keep_the_plus_and_drop_the_pointless_decimal() {
        let cases = [
            ("10+", "10+"),
            ("6+", "6+"),
            ("15", "15"),
            ("9", "9"),
            ("2.5", "2.5"),
            // Rust's `{:.1}` rounds half to even, as the previous formatting
            // did; 1/4 SM has always shown as 0.2. Pinned, not endorsed.
            ("0.25", "0.2"),
        ];
        for (raw, want) in cases {
            assert_eq!(Visibility::parse(raw).unwrap().label(), want, "input {raw:?}");
        }
    }

    #[test]
    fn an_absent_or_nonsensical_visibility_is_none() {
        for bad in ["", "   ", "+", "M1/4", "abc", "-3", "inf", "NaN"] {
            assert_eq!(Visibility::parse(bad), None, "input {bad:?} must not parse");
        }
        assert_eq!(station("K20U").visibility, None, "K20U reports no visibility");
    }

    // ── Wind: calm vs variable vs a real bearing ──────────────────────────

    /// AWC writes `wind_dir_degrees=0` for calm *and* variable and never leaves
    /// it empty for either — 1,249 of 4,933 rows in the measured cache. Reading
    /// it as a bearing pointed a barb due north for all of them.
    #[test]
    fn a_zero_direction_column_never_becomes_a_northerly_bearing() {
        for id in ["LTAR", "KUZA", "KHHV", "KGOP", "K8A1", "GCGM", "CWHO"] {
            let dir = station(id).wind_dir.expect("fixture rows carry wind data");
            assert_ne!(
                dir,
                WindDir::Degrees(0),
                "{id} reports wind_dir_degrees=0, which is not a bearing"
            );
            assert_eq!(dir.bearing(), None, "{id} must not offer a barb direction");
        }
    }

    /// `00000KT` is calm; `VRBnnKT` is a real wind with no steady direction.
    #[test]
    fn calm_and_variable_are_told_apart() {
        assert_eq!(station("KUZA").wind_dir, Some(WindDir::Calm));
        assert_eq!(station("KHHV").wind_dir, Some(WindDir::Calm));
        assert_eq!(station("LTAR").wind_dir, Some(WindDir::Variable));
        assert_eq!(station("KGOP").wind_dir, Some(WindDir::Variable));
    }

    /// 202 of the 295 variable reports in the measured cache blow at 1–2 kt, so
    /// inferring "variable" from a speed threshold would miss two thirds of them.
    /// The raw text says `VRB` outright.
    #[test]
    fn a_slow_variable_wind_is_still_variable() {
        let k8a1 = station("K8A1");
        assert_eq!(k8a1.wind_speed_kt, Some(2), "K8A1 reports VRB02KT");
        assert_eq!(k8a1.wind_dir, Some(WindDir::Variable));
    }

    /// The counterpart: a genuine northerly is reported as 360, never 0, and
    /// must keep its bearing.
    #[test]
    fn a_genuine_northerly_keeps_its_bearing() {
        let ktri = station("KTRI");
        assert_eq!(ktri.wind_dir, Some(WindDir::Degrees(360)));
        assert_eq!(ktri.wind_dir.unwrap().bearing(), Some(360));
    }

    #[test]
    fn ordinary_bearings_pass_through_including_metric_reports() {
        assert_eq!(station("KEDU").wind_dir, Some(WindDir::Degrees(180)));
        assert_eq!(station("K20U").wind_dir, Some(WindDir::Degrees(190)));
        assert_eq!(station("CYYH").wind_dir, Some(WindDir::Degrees(40)));
        assert_eq!(
            station("UUBW").wind_dir,
            Some(WindDir::Degrees(340)),
            "34002MPS is a bearing even though the speed is metric"
        );
    }

    /// GCGM reports `00000KT` but carries `R09/VRB07G21KT` in its remarks. A
    /// substring search for "VRB" would call a dead-calm station variable.
    #[test]
    fn a_vrb_confined_to_the_remarks_does_not_make_the_station_variable() {
        assert!(station("GCGM").raw_ob.contains("VRB"), "fixture must keep the trap");
        assert_eq!(station("GCGM").wind_dir, Some(WindDir::Calm));
    }

    /// Forecast groups carry their own winds; only the observed group counts.
    #[test]
    fn a_forecast_group_does_not_override_the_observed_wind() {
        let raw = "METAR LFBI 250530Z AUTO 04004KT 340V080 9999 NCD 19/09 \
                   Q1009 TEMPO VRB15G25KT 4000 TSRA BKN070CB";
        assert_eq!(resolve_wind_dir(raw, Some(40), Some(4)), Some(WindDir::Degrees(40)));
    }

    /// `000` is reserved for calm, so a `000` bearing carrying 25 kt is not a
    /// northerly — two Canadian AUTO stations report exactly this.
    #[test]
    fn a_zero_bearing_with_speed_is_not_treated_as_north() {
        let cwho = station("CWHO");
        assert_eq!(cwho.wind_speed_kt, Some(25), "CWHO reports 00025KT");
        assert_eq!(cwho.wind_dir, Some(WindDir::Variable));
        assert_eq!(cwho.wind_dir.unwrap().bearing(), None);
    }

    /// 59 of 4,933 rows report no wind at all. That is unknown, not calm.
    #[test]
    fn a_report_without_any_wind_data_has_no_direction() {
        let k40u = station("K40U");
        assert!(raw_wind_group(&k40u.raw_ob).is_none(), "K40U has no wind group");
        assert_eq!(k40u.wind_dir, None);
        assert_eq!(k40u.wind_speed_kt, None);
    }

    // ── Wind group tokenizer ──────────────────────────────────────────────

    #[test]
    fn wind_tokens_are_recognised_by_shape_not_by_substring() {
        assert_eq!(parse_wind_token("18006KT"), Some((Some(180), 6)));
        assert_eq!(parse_wind_token("36003KT"), Some((Some(360), 3)));
        assert_eq!(parse_wind_token("00000KT"), Some((Some(0), 0)));
        assert_eq!(parse_wind_token("VRB03KT"), Some((None, 3)));
        assert_eq!(parse_wind_token("VRB06G13KT"), Some((None, 6)));
        assert_eq!(parse_wind_token("34002MPS"), Some((Some(340), 2)));
        assert_eq!(parse_wind_token("120100KMH"), Some((Some(120), 100)));

        // Not wind groups.
        let rejects = [
            "E00000KT",
            "R09/VRB07G21KT",
            "9999",
            "A2986",
            "18006",
            "VRBKT",
            "18006G",
            "1800X6KT",
        ];
        for bad in rejects {
            assert_eq!(parse_wind_token(bad), None, "{bad:?} is not a wind group");
        }
    }

    /// The observed wind precedes `RMK`; anything after it belongs to remarks.
    #[test]
    fn scanning_for_the_wind_group_stops_at_the_remarks_marker() {
        let raw = "METAR CWHO 250500Z AUTO RMK AO1 SLP105 18006KT";
        assert_eq!(raw_wind_group(raw), None, "a wind inside RMK is not the observed wind");
    }

    /// The column agreed with the raw text on 4,873 of 4,873 measured rows, so
    /// the fallback applies the column's own convention rather than guessing.
    #[test]
    fn the_column_is_the_fallback_when_the_raw_text_has_no_wind_group() {
        let no_group = "METAR K40U 250535Z AUTO 10SM CLR 25/09 A3027";
        assert_eq!(resolve_wind_dir(no_group, None, None), None);
        assert_eq!(resolve_wind_dir(no_group, Some(0), Some(0)), Some(WindDir::Calm));
        assert_eq!(resolve_wind_dir(no_group, Some(0), Some(7)), Some(WindDir::Variable));
        assert_eq!(
            resolve_wind_dir(no_group, Some(360), Some(7)),
            Some(WindDir::Degrees(360))
        );
    }
}
