//! Today's preliminary tornado, hail and wind reports, from three SPC CSVs.

use crate::fetch_policy::{FetchError, NotFound};

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
///
/// Three CSVs, and a partial result is a real result — a day with tornado and
/// hail reports but a wind CSV that would not load is still worth drawing. But
/// **all three failing is not `Ok(vec![])`**: that is indistinguishable from a
/// quiet day, and it used to be reported as one, which both hid the outage from
/// the user and stamped the poll clock as though the fetch had succeeded.
pub async fn fetch_storm_reports(
    client: &reqwest::Client,
    sources: &rustdar_radar::sources::DataSources,
) -> Result<Vec<StormReport>, FetchError> {
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
    let mut failures: Vec<FetchError> = Vec::new();
    for (label, outcome) in [("tornado", torn), ("hail", hail), ("wind", wind)] {
        match outcome {
            Ok(r) => reports.extend(r),
            Err(e) => {
                log::warn!("Failed to fetch {label} reports: {e}");
                failures.push(e);
            }
        }
    }

    if failures.len() == 3 {
        // Every CSV failed. The round's verdict is the merge of all three, not
        // whichever happened to be listed first: `failures[0]` is always the
        // tornado CSV, so one 400 there condemned the layer even when hail and
        // wind had merely timed out. Merged, the round is refused only if all
        // three were — the rule every other multi-request round here follows.
        return Err(FetchError::of_round(
            &failures,
            "no storm report CSV could be fetched",
        ));
    }

    log::info!("Fetched {} storm reports total", reports.len());
    Ok(reports)
}

/// A 404 is **routine**: `today_*.csv` is rebuilt each convective day, and a
/// kind with nothing in it yet is a normal answer rather than an outage.
async fn fetch_csv(
    client: &reqwest::Client,
    url: &str,
    kind: StormReportKind,
) -> Result<Vec<StormReport>, FetchError> {
    let response = client.get(url).send().await.map_err(|e| {
        FetchError::from_transport(&e, format!("HTTP request failed for {url}: {e}"))
    })?;

    if !response.status().is_success() {
        return Err(FetchError::from_status(
            response.status(),
            NotFound::IsRoutine,
            format!("SPC returned HTTP {} for {url}", response.status()),
        ));
    }

    let text = response.text().await.map_err(|e| {
        FetchError::from_transport(&e, format!("Failed to read response body: {e}"))
    })?;

    parse_csv(&text, kind).map_err(FetchError::transient)
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
