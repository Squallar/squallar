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
    /// HHMM UTC, e.g. "1339" — display truth, kept verbatim.
    pub time: String,
    /// **Parsed at parse time, never at raster time**: the instant this
    /// report happened, derived from `time` by [`report_instant`]'s 12Z-12Z
    /// convective-day rule. `None` when `time` does not name a real clock
    /// reading; a `None` is never dropped for want of a readable time — the
    /// same rule as [`NwsAlert::valid_from`](crate::nws::alert::NwsAlert).
    pub valid: Option<chrono::NaiveDateTime>,
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

/// **The instant an HHMM in `today_*.csv` names, given the wall clock at
/// fetch.**
///
/// The CSV is one **convective day**: a window running 12Z to 12Z the next
/// calendar day. The date half of the instant is therefore derived, never the
/// fetch date itself: the file's window opened at 12Z on
/// `(anchor - 12h).date()`, an HHMM of 12:00 or later happened on that
/// calendar date, and an HHMM before 12:00 happened on the calendar date
/// **after** it. Stamping the fetch date onto every row would file each
/// post-midnight report ("0230") a full day early — the pinned rollover case
/// below.
///
/// `anchor` is the wall clock at fetch — the only instant that says which
/// convective day `today_*.csv` currently is. Never a scrubbed `as_of`: the
/// file fetched is always the current one regardless of what a pane depicts.
///
/// `None` for anything that is not four ASCII digits naming a real clock
/// reading; the caller keeps the raw string for display.
pub fn report_instant(hhmm: &str, anchor: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    if hhmm.len() != 4 || !hhmm.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hh, mm) = hhmm.split_at(2);
    let hour: u32 = hh.parse().ok()?;
    let minute: u32 = mm.parse().ok()?;
    let window_opened = (anchor - chrono::Duration::hours(12)).date();
    let date = if hour >= 12 {
        window_opened
    } else {
        window_opened.succ_opt()?
    };
    // `and_hms_opt` refuses hour > 23 / minute > 59, so "2461" is rejected
    // here rather than wrapped into a fictitious instant.
    date.and_hms_opt(hour, minute, 0)
}

/// **12Z on the day the convective day containing `at` opened** — the earliest
/// instant a row of that day's `today_*.csv` can carry.
///
/// The same 12Z-to-12Z window [`report_instant`] dates its rows against, read
/// the other way: given an instant, which window is it inside. `None` only if
/// the derived date has no 12:00, which no real calendar date lacks.
///
/// Used by the handler's residency answer: the picture at `at` is every report
/// of the day that has **already happened**, so the slice that feeds it opens
/// here and not at `at`.
pub fn convective_day_start(at: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    (at - chrono::Duration::hours(12))
        .date()
        .and_hms_opt(12, 0, 0)
}

/// Origin must come from
/// [`DataSources::spc_base`](squallar_source::origins::DataSources::spc_base),
/// never a literal, or these three escape the origin table.
pub(crate) fn report_url(
    sources: &squallar_source::origins::DataSources,
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
/// `anchor` is the wall clock at fetch, which dates every row's HHMM — see
/// [`report_instant`].
pub async fn fetch_storm_reports(
    client: &reqwest::Client,
    sources: &squallar_source::origins::DataSources,
    anchor: chrono::NaiveDateTime,
) -> Result<StormReportRound, FetchError> {
    log::info!("Fetching SPC storm reports");

    let (torn, hail, wind) = futures::future::join3(
        fetch_csv(
            client,
            &report_url(sources, StormReportKind::Tornado),
            StormReportKind::Tornado,
            anchor,
        ),
        fetch_csv(
            client,
            &report_url(sources, StormReportKind::Hail),
            StormReportKind::Hail,
            anchor,
        ),
        fetch_csv(
            client,
            &report_url(sources, StormReportKind::Wind),
            StormReportKind::Wind,
            anchor,
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
    anchor: chrono::NaiveDateTime,
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

    parse_csv(&text, kind, anchor).map_err(FetchError::transient)
}

/// `Time,{F_Scale|Size|Speed},Location,County,State,Lat,Lon,Comments`, lat/lon
/// in decimal degrees. Comments may contain commas, hence `splitn(8, ',')`.
fn parse_csv(
    text: &str,
    kind: StormReportKind,
    anchor: chrono::NaiveDateTime,
) -> Result<Vec<StormReport>, String> {
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

        let valid = report_instant(&time, anchor);
        reports.push(StormReport {
            kind,
            time,
            valid,
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

#[cfg(test)]
mod convective_day_tests {
    use super::report_instant;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    /// An afternoon HHMM lands on the calendar date the window opened on.
    #[test]
    fn an_afternoon_report_lands_on_the_date_the_window_opened() {
        assert_eq!(
            report_instant("1339", at(2026, 8, 22, 18, 0)),
            Some(at(2026, 8, 22, 13, 39)),
        );
    }

    /// **The 12Z-rollover pin.** "0230" in a file whose window opened at 12Z
    /// on the 22nd happened on the 23rd: the convective day runs 12Z->12Z, so
    /// every HHMM before 12:00 is on the calendar date AFTER the one the
    /// window opened on. Stamping the fetch date instead files it a day early.
    #[test]
    fn a_small_hours_report_lands_on_the_next_calendar_date() {
        assert_eq!(
            report_instant("0230", at(2026, 8, 22, 18, 0)),
            Some(at(2026, 8, 23, 2, 30)),
        );
    }

    /// The same convective day, fetched after midnight: the anchor's calendar
    /// date has already moved to the 23rd, but the window still opened at 12Z
    /// on the 22nd — so an afternoon HHMM still dates to the 22nd, and a
    /// small-hours one to the 23rd. Deriving the date from `anchor.date()`
    /// (instead of `anchor - 12h`) shears every afternoon report forward a
    /// day here.
    #[test]
    fn a_post_midnight_fetch_still_dates_the_afternoon_to_yesterday() {
        let anchor = at(2026, 8, 23, 5, 0);
        assert_eq!(
            report_instant("1339", anchor),
            Some(at(2026, 8, 22, 13, 39)),
        );
        assert_eq!(report_instant("0230", anchor), Some(at(2026, 8, 23, 2, 30)));
    }

    /// The boundary is exactly 12:00: "1200" opened the window, "1159" is the
    /// last minute before it closes — a day apart.
    #[test]
    fn the_boundary_splits_at_exactly_twelve_z() {
        let anchor = at(2026, 8, 22, 18, 0);
        assert_eq!(report_instant("1200", anchor), Some(at(2026, 8, 22, 12, 0)));
        assert_eq!(
            report_instant("1159", anchor),
            Some(at(2026, 8, 23, 11, 59))
        );
    }

    /// Junk stays `None` rather than becoming a fictitious instant: too
    /// short, non-digit (including multi-byte, which must not panic), and
    /// four digits that are not a clock reading.
    #[test]
    fn junk_times_parse_to_none_not_to_a_fictitious_instant() {
        let anchor = at(2026, 8, 22, 18, 0);
        for junk in ["12", "12345", "1é2", "éé", "🌀", "2461", "1260", ""] {
            assert_eq!(report_instant(junk, anchor), None, "{junk:?}");
        }
    }
}
