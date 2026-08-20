//! Today's preliminary tornado, hail and wind reports, from three SPC CSVs.

use crate::fetch_policy::{FetchError, NotFound};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StormReportKind {
    Tornado,
    Hail,
    Wind,
}

impl StormReportKind {
    /// Reads inside a sentence — `"tornado reports did not load"` — so it is
    /// lowercase and carries no punctuation of its own.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tornado => "tornado",
            Self::Hail => "hail",
            Self::Wind => "wind",
        }
    }
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
/// [`DataSources::spc_base`](rustdar_source::origins::DataSources::spc_base),
/// never a literal, or these three escape the origin table.
pub(crate) fn report_url(
    sources: &rustdar_source::origins::DataSources,
    kind: StormReportKind,
) -> String {
    let name = match kind {
        StormReportKind::Tornado => "torn",
        StormReportKind::Hail => "hail",
        StormReportKind::Wind => "wind",
    };
    format!("{}/climo/reports/today_{name}.csv", sources.spc_base)
}

/// What one round of the three CSVs delivered.
#[derive(Debug)]
pub struct StormReportRound {
    pub reports: Vec<StormReport>,
    /// The kinds whose CSV did not answer, and why. Empty on a whole round.
    pub failed_kinds: Vec<(StormReportKind, FetchError)>,
}

impl StormReportRound {
    /// The layer-agnostic report the UI renders.
    pub fn completeness(&self) -> crate::fetch_policy::DataCompleteness {
        let absent: Vec<&(StormReportKind, FetchError)> = self
            .failed_kinds
            .iter()
            .filter(|(_, e)| e.failure != crate::fetch_policy::FetchFailure::Absent)
            .collect();
        crate::fetch_policy::DataCompleteness {
            expected: 3,
            partial: 0,
            missing: absent.len(),
            parts_requested: 0,
            parts_resolved: 0,
            unit: "report kinds",
            part_unit: "CSVs",
            reasons: absent
                .iter()
                .map(|(kind, e)| (format!("{}: {}", kind.label(), e.message), 1))
                .collect(),
        }
    }
}

/// `client` must be the preflight-safe [`crate::spc::fetch::spc_client`].
pub async fn fetch_storm_reports(
    client: &reqwest::Client,
    sources: &rustdar_source::origins::DataSources,
) -> Result<StormReportRound, FetchError> {
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
    let mut failed_kinds: Vec<(StormReportKind, FetchError)> = Vec::new();
    for (kind, outcome) in [
        (StormReportKind::Tornado, torn),
        (StormReportKind::Hail, hail),
        (StormReportKind::Wind, wind),
    ] {
        match outcome {
            Ok(r) => reports.extend(r),
            Err(e) => {
                log::warn!("Failed to fetch {} reports: {e}", kind.label());
                failed_kinds.push((kind, e));
            }
        }
    }

    if failed_kinds.len() == 3 {
        let failures: Vec<FetchError> = failed_kinds.into_iter().map(|(_, e)| e).collect();
        return Err(FetchError::of_round(
            &failures,
            "no storm report CSV could be fetched",
        ));
    }

    if !failed_kinds.is_empty() {
        // WARN, not INFO: the layer is about to draw a report set with a whole
        // kind missing from it, on a fresh clock. The line above is per-CSV and
        // says nothing about what the round as a whole ended up holding.
        log::warn!(
            "Storm reports incomplete: {} of 3 kinds did not load ({}), so none of \
             those reports are on the map",
            failed_kinds.len(),
            failed_kinds
                .iter()
                .map(|(kind, _)| kind.label())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    log::info!("Fetched {} storm reports total", reports.len());
    Ok(StormReportRound {
        reports,
        failed_kinds,
    })
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
