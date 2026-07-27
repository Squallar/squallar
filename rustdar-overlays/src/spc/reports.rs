//! Today's preliminary tornado, hail and wind reports, from three SPC CSVs.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StormReportKind {
    Tornado,
    Hail,
    Wind,
}

#[derive(Debug, Clone)]
pub struct StormReport {
    pub kind: StormReportKind,
    /// HHMM UTC, e.g. "1339".
    pub time: String,
    /// Unit depends on `kind`: tornado = F/EF number; hail = hundredths of an
    /// inch (100 = 1.00"); wind = knots. `None` for the feed's "UNK".
    pub magnitude: Option<f64>,
    pub location: String,
    pub county: String,
    pub state: String,
    pub lat: f64,
    pub lon: f64,
    pub comments: String,
}

/// Origin must come from
/// [`DataSources::spc_base`](rustdar_radar::sources::DataSources::spc_base),
/// never a literal, or these three escape the origin table.
pub(crate) fn report_url(
    sources: &rustdar_radar::sources::DataSources,
    kind: StormReportKind,
) -> String {
    let name = match kind {
        StormReportKind::Tornado => "torn",
        StormReportKind::Hail => "hail",
        StormReportKind::Wind => "wind",
    };
    format!("{}/climo/reports/today_{name}.csv", sources.spc_base)
}

/// `client` must be the preflight-safe [`crate::spc::fetch::spc_client`].
pub async fn fetch_storm_reports(
    client: &reqwest::Client,
    sources: &rustdar_radar::sources::DataSources,
) -> Result<Vec<StormReport>, String> {
    log::info!("Fetching SPC storm reports");

    let (torn, hail, wind) = futures::future::join3(
        fetch_csv(
            client,
            &report_url(sources, StormReportKind::Tornado),
            StormReportKind::Tornado,
        ),
        fetch_csv(
            client,
            &report_url(sources, StormReportKind::Hail),
            StormReportKind::Hail,
        ),
        fetch_csv(
            client,
            &report_url(sources, StormReportKind::Wind),
            StormReportKind::Wind,
        ),
    )
    .await;

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

/// `Time,{F_Scale|Size|Speed},Location,County,State,Lat,Lon,Comments`, lat/lon
/// in decimal degrees. Comments may contain commas, hence `splitn(8, ',')`.
fn parse_csv(text: &str, kind: StormReportKind) -> Result<Vec<StormReport>, String> {
    let mut reports = Vec::new();

    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

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
            lat,
            lon,
            comments,
        });
    }

    Ok(reports)
}
