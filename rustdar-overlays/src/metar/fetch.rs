//! Fetch current METAR observations from the Aviation Weather Center bulk cache file.

use std::io::Read;

use flate2::read::GzDecoder;

use super::types::{CloudLayer, FlightCategory, MetarOb};

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
    let altim_is_inhg = columns.iter().any(|c| *c == "altim_in_hg");

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

        observations.push(MetarOb {
            station_id,
            name,
            lat,
            lon,
            elev_m: get_f64(i_elev),
            temp_c: get_f64(i_temp),
            dewp_c: get_f64(i_dewp),
            wind_dir: get_u16(i_wdir),
            wind_speed_kt: get_u16(i_wspd),
            wind_gust_kt: get_u16(i_wgst),
            visibility_mi: get_f64(i_vis),
            altimeter_hpa,
            flight_category,
            raw_ob: get_str(i_raw),
            clouds,
            wx_string,
            obs_time: get_str(i_time),
        });
    }

    log::info!("Parsed {} METAR observations from cache", observations.len());
    Ok(observations)
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
