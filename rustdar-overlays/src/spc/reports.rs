//! SPC Storm Reports: fetch and parse today's preliminary tornado, hail, and
//! wind reports from the SPC CSV endpoints.

use crate::types::OverlayFeature;

/// The kind of storm report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StormReportKind {
    Tornado,
    Hail,
    Wind,
}

/// A single SPC preliminary storm report.
#[derive(Debug, Clone)]
pub struct StormReport {
    pub kind: StormReportKind,
    /// Time in HHMM UTC format (e.g. "1339").
    pub time: String,
    /// Magnitude field — meaning depends on `kind`:
    /// - Tornado: F/EF scale string (e.g. "EF0") or `None` for "UNK"
    /// - Hail: size in hundredths of inches (e.g. 100 = 1.00")
    /// - Wind: speed in knots, or `None` for "UNK"
    pub magnitude: Option<f64>,
    pub location: String,
    pub county: String,
    pub state: String,
    pub lat: f64,
    pub lon: f64,
    pub comments: String,
    /// Small circular polygon feature for click hit-testing.
    pub feature: OverlayFeature,
}

const TORN_URL: &str = "https://www.spc.noaa.gov/climo/reports/today_torn.csv";
const HAIL_URL: &str = "https://www.spc.noaa.gov/climo/reports/today_hail.csv";
const WIND_URL: &str = "https://www.spc.noaa.gov/climo/reports/today_wind.csv";

/// Fetch today's tornado, hail, and wind reports from SPC in parallel.
pub async fn fetch_storm_reports(
    client: &reqwest::Client,
) -> Result<Vec<StormReport>, String> {
    log::info!("Fetching SPC storm reports");

    let (torn, hail, wind) = futures::future::join3(
        fetch_csv(client, TORN_URL, StormReportKind::Tornado),
        fetch_csv(client, HAIL_URL, StormReportKind::Hail),
        fetch_csv(client, WIND_URL, StormReportKind::Wind),
    ).await;

    let mut reports = Vec::new();
    match torn {
        Ok(r) => reports.extend(r),
        Err(e) => log::warn!("Failed to fetch tornado reports: {e}"),
    }
    match hail {
        Ok(r) => reports.extend(r),
        Err(e) => log::warn!("Failed to fetch hail reports: {e}"),
    }
    match wind {
        Ok(r) => reports.extend(r),
        Err(e) => log::warn!("Failed to fetch wind reports: {e}"),
    }

    log::info!("Fetched {} storm reports total", reports.len());
    Ok(reports)
}

async fn fetch_csv(
    client: &reqwest::Client,
    url: &str,
    kind: StormReportKind,
) -> Result<Vec<StormReport>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed for {url}: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("SPC returned HTTP {} for {url}", response.status()));
    }

    let text = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {e}"))?;

    parse_csv(&text, kind)
}

/// Parse a SPC storm report CSV (header + data rows).
///
/// CSV format: `Time,{F_Scale|Size|Speed},Location,County,State,Lat,Lon,Comments`
/// Lat/Lon are decimal degrees. The Comments field may contain commas, so we
/// split at most 7 commas and take the remainder as comments.
fn parse_csv(text: &str, kind: StormReportKind) -> Result<Vec<StormReport>, String> {
    let mut reports = Vec::new();

    for (i, line) in text.lines().enumerate() {
        // Skip the header row
        if i == 0 {
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Split into at most 8 parts (7 commas) — comments may contain commas
        let parts: Vec<&str> = line.splitn(8, ',').collect();
        if parts.len() < 7 {
            continue;
        }

        let time = parts[0].trim().to_string();
        let mag_str = parts[1].trim();
        let location = parts[2].trim().to_string();
        let county = parts[3].trim().to_string();
        let state = parts[4].trim().to_string();

        let lat: f64 = match parts[5].trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let lon: f64 = match parts[6].trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Skip reports with clearly invalid coordinates
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }

        let comments = if parts.len() >= 8 {
            parts[7].trim().to_string()
        } else {
            String::new()
        };

        let magnitude = match mag_str {
            "UNK" | "" => None,
            s => s.parse::<f64>().ok(),
        };

        reports.push(StormReport {
            kind,
            time,
            magnitude,
            location,
            county,
            state,
            feature: OverlayFeature::point(lat, lon),
            lat,
            lon,
            comments,
        });
    }

    Ok(reports)
}
