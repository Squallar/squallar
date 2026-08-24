use crate::render::overlay_state::{PaneMut, PaneRef, PaneToggle};
use std::any::Any;
use std::sync::Arc;

use squallar_units::UserPreferences;

use crate::fetch_policy::Assembled;
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayState,
    PopupContent, PopupSection, RasterizeContext, RenderMode,
};
use crate::render::rasterize;
use crate::spc::reports::{StormReport, StormReportKind, StormReportRound};
use squallar_source::id::{LayerId, known};
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::time::TimeAxis;

// `pub`, not `pub(crate)`: the described-job dispatch tests in `squallar-app`
// and the hit-map zip tests in `squallar-worker` construct this payload type.
pub struct StormReportsFetchResult(pub Result<StormReportRound, crate::fetch_policy::FetchError>);
/// [`Assembled`]: three CSVs, one per report kind, fetched independently and
/// refused as a round only when **all three** failed. One failing arrives here
/// as `Ok` with a whole kind of report absent from the map.
///
/// [`Assembled`]: crate::fetch_policy::Assembled
impl crate::fetch_policy::FetchRound for StormReportsFetchResult {
    type Shape = crate::fetch_policy::Assembled;
}

#[derive(Debug)]
pub(crate) struct StormReportItem {
    pub report: StormReport,
    /// The reports feed carries no IDs, so position in the fetch is the only
    /// identity available — what `matches()` and the hit map both key on.
    pub index: usize,
}

impl OverlayItem for StormReportItem {
    fn layer_id(&self) -> LayerId {
        known::STORM_REPORTS
    }

    fn popup_content(&self, prefs: &UserPreferences) -> PopupContent {
        let report = &self.report;
        let kind_str = match report.kind {
            StormReportKind::Tornado => "Tornado",
            StormReportKind::Hail => "Hail",
            StormReportKind::Wind => "Wind",
        };
        // The feed gives HHMM with no date, so local conversion assumes today.
        //
        // Four ASCII *digits*, not four bytes: `time` is a CSV field whose
        // charset nothing checks, and `len() == 4` admitted "1é2" (where
        // `&time[..2]` split `é` down the middle — a panic on the render thread,
        // taking the app) and "éé" (four bytes that split *legally* into a clock
        // reading "é:é"). Anything rejected is shown below as the feed sent it.
        let is_hhmm = report.time.len() == 4 && report.time.bytes().all(|b| b.is_ascii_digit());
        let split = is_hhmm.then(|| report.time.split_at_checked(2)).flatten();
        let formatted_time = if let Some((hh, mm)) = split {
            let hhmm = format!("{hh}:{mm}");
            match prefs.timezone {
                squallar_units::TimezonePreference::Utc => format!("{hhmm} UTC"),
                squallar_units::TimezonePreference::Local => {
                    if let (Ok(h), Ok(m)) = (hh.parse::<u32>(), mm.parse::<u32>()) {
                        let today = chrono::Utc::now().date_naive();
                        if let Some(naive) = today.and_hms_opt(h, m, 0) {
                            let utc_dt = chrono::TimeZone::from_utc_datetime(&chrono::Utc, &naive);
                            let local_dt = utc_dt.with_timezone(&chrono::Local);
                            local_dt.format("%H:%M %Z").to_string()
                        } else {
                            format!("{hhmm} UTC")
                        }
                    } else {
                        format!("{hhmm} UTC")
                    }
                }
            }
        } else {
            format!("{} UTC", report.time)
        };
        let mut sections = vec![PopupSection::Text(format!(
            "{formatted_time} - {}, {} {}",
            report.location, report.county, report.state
        ))];
        if let Some(mag) = report.magnitude {
            let mag_text = match report.kind {
                StormReportKind::Tornado => format!("F/EF Scale: {mag}"),
                // Hundredths of an inch on the wire (`StormReport::magnitude`).
                // The precision comes from the unit, the same rule as the MEHS
                // readout (`RadarProduct::format_value`).
                StormReportKind::Hail => {
                    let inches = (mag / 100.0) as f32;
                    let converted = prefs.hail_size.convert_from_inches(inches);
                    let decimals = prefs.hail_size.decimals();
                    format!("Size: {converted:.decimals$}{}", prefs.hail_size.suffix())
                }
                StormReportKind::Wind => {
                    let converted = prefs.speed.convert_from_knots(mag as f32);
                    format!("Speed: {converted:.0} {}", prefs.speed.suffix())
                }
            };
            sections.push(PopupSection::Text(mag_text));
        }
        if !report.comments.is_empty() {
            sections.push(PopupSection::Text(report.comments.clone()));
        }
        PopupContent {
            title: format!("SPC Storm Report: {kind_str}"),
            accent_rgb: match report.kind {
                StormReportKind::Tornado => [220, 40, 40],
                StormReportKind::Hail => [40, 180, 40],
                StormReportKind::Wind => [40, 80, 220],
            },
            width: 350.0,
            sections,
            actions: Vec::new(),
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<StormReportItem>()
            .is_some_and(|o| o.index == self.index)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub(crate) struct StormReportsHandler {
    pub state: OverlayState<Vec<Arc<StormReportItem>>, Assembled>,
    pub enabled: bool,
}

impl StormReportsHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            enabled: false,
        }
    }

    /// What the rasterizer reads, captured once. The rows are
    /// [`rasterize::ReportPaint`], not whole [`StormReport`]s: the time,
    /// magnitude and comments are popup content the raster never draws.
    /// **Row `i` is `state.data[i]`'s report**, the indexing
    /// [`Self::hit_items`] answers.
    fn paint_input(&self, ctx: &RasterizeContext) -> Option<rasterize::ReportsInput> {
        if self.state.data.is_empty() {
            return None;
        }
        Some(rasterize::ReportsInput {
            // **Every row travels, even one later than `as_of`** — the as-of
            // cull is the rasterizer's, because a row's position is its
            // hit-map id. Filtering here would desynchronize the map from
            // [`Self::hit_items`], which has no instant to filter by.
            reports: self
                .state
                .data
                .iter()
                .map(|i| rasterize::ReportPaint {
                    kind: i.report.kind,
                    lat: i.report.lat,
                    lon: i.report.lon,
                    valid: i.report.valid,
                })
                .collect(),
            zoom: ctx.zoom,
            is_dark: ctx.is_dark,
            device_scale: ctx.device_scale,
            // The **depicted** instant, which on a live pane is the wall
            // clock — the same field GLM sends as `now`.
            as_of: ctx.as_of,
        })
    }
}

impl OverlayHandler for StormReportsHandler {
    fn id(&self) -> LayerId {
        known::STORM_REPORTS
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        60
    }

    fn display_name(&self) -> &str {
        "SPC Storm Reports"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    /// A report is a point event with an instant (WB-2): the picture at
    /// `as_of` is which of today's reports have **already happened**, so a
    /// scrubbed pane shows the reports that existed then and none from later
    /// in the day. The filter itself rides the wire and runs in the
    /// rasterizer ([`rasterize::rasterize_storm_reports`]), where rows and
    /// hit-map ids are minted together — see the cull's own comment for why
    /// it cannot live in [`Self::paint_input`]. No archive fetch: the whole
    /// convective day is already in memory, so scrubbing within it needs
    /// nothing from the network.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::EventLifetime
    }

    /// **The convective day behind each stop, not the stop alone** — the one
    /// `EventLifetime` layer here whose picture is *cumulative*.
    ///
    /// A report is a point event, and the picture at a stop is every report
    /// of the day that has **already happened** — so the slice feeding a stop
    /// opens at 12Z when that convective day opened
    /// ([`crate::spc::reports::convective_day_start`]) and closes at the stop.
    /// Answering the instant alone, the way the alert and outlook handlers
    /// do, would be an under-reach a retention rule reads as permission to
    /// drop every report before the playhead, emptying the map behind a
    /// scrub.
    ///
    /// Thirteen hourly stops therefore **coalesce into one range** — each
    /// stop's slice contains its predecessor's — and that is honest: this
    /// layer holds one CSV per kind for the whole day and there is nothing
    /// finer to ask for. A stop whose derived day start does not name a real
    /// clock reading contributes nothing rather than a fabricated window.
    fn residency_for(
        &self,
        _pane: &PaneRef<'_>,
        stops: &[chrono::NaiveDateTime],
    ) -> squallar_source::time::Residency {
        squallar_source::time::Residency::over(
            stops
                .iter()
                .filter_map(|&stop| Some((crate::spc::reports::convective_day_start(stop)?, stop))),
        )
    }

    /// `is_dark` rides into the described job (`StormReportsInput`) and picks
    /// the marker outline, so a cached raster is a raster in one theme.
    fn theme_sensitive(&self) -> bool {
        true
    }

    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        PaneToggle::is_on(pane, self.enabled)
    }

    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        if !PaneToggle::set(pane, enabled) {
            self.enabled = enabled;
        }
    }

    fn status_line(&self, _pane: &PaneRef<'_>) -> Option<String> {
        if !self.enabled {
            return None;
        }
        Some(format!("{} reports", self.state.data.len()))
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        !self.state.data.is_empty()
    }

    fn is_fetching(&self) -> bool {
        self.state.fetching
    }

    fn set_fetching(&mut self, fetching: bool, _pane: &PaneRef<'_>) {
        self.state.fetching = fetching;
    }

    fn retry(&self) -> Option<&crate::fetch_policy::FetchRetry> {
        Some(&self.state.retry)
    }

    fn retry_mut(&mut self) -> Option<&mut crate::fetch_policy::FetchRetry> {
        Some(&mut self.state.retry)
    }

    fn fetch_time(&self) -> Option<web_time::Instant> {
        self.state.fetch_time
    }

    fn item_count(&self, _pane: &PaneRef<'_>) -> usize {
        self.state.data.len()
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(300)
    }

    fn clickable_items<'a>(&'a self, _pane: &PaneRef<'_>) -> Vec<ClickableItem<'a>> {
        Vec::new() // Clicks resolve through the rasterizer's `HitMap` instead.
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, _pane: &PaneRef<'_>) {
        let Some(fetch) = self.state.downcast_round::<StormReportsFetchResult>(result) else {
            log::error!("Storm reports handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(round) => {
                log::info!("Received {} storm reports", round.reports.len());
                // The coverage report travels with the data it describes:
                // `fetch_storm_reports` refuses the round only when **all three**
                // CSVs failed, so one or two failing arrives here as `Ok` with a
                // whole kind of report absent — coverage, not health.
                let coverage = round.completeness();
                let items = round
                    .reports
                    .into_iter()
                    .enumerate()
                    .map(|(i, report)| Arc::new(StormReportItem { report, index: i }))
                    .collect();
                self.state.set_data_with_coverage(items, coverage);
            }
            Err(e) => {
                log::error!("Storm reports fetch failed: {e}");
                // **Undocumented behaviour, named rather than changed.** This
                // branch does not clear `state.data`, so a total outage leaves
                // the previous poll's reports on the map, which can look current.
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {
        let count = self.state.data.len();
        selections.retain(|sel| {
            if sel.layer_id() != known::STORM_REPORTS {
                return true;
            }
            sel.as_any()
                .downcast_ref::<StormReportItem>()
                .is_some_and(|r| r.index < count)
        });
    }

    fn prepare_job(&self, ctx: &RasterizeContext, _pane: &PaneRef<'_>) -> Option<DescribedJob> {
        Some(DescribedJob::new(self.paint_input(ctx)?))
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/reports")
    }

    /// Index-aligned with [`Self::paint_input`]'s rows: `hit_items()[i]` **is**
    /// the item whose report travelled at row `i` — the invariant
    /// [`rasterize::HitMap::from_cells`] zips on.
    fn hit_items(&self) -> Option<Vec<Arc<dyn OverlayItem>>> {
        if self.state.data.is_empty() {
            return None;
        }
        Some(
            self.state
                .data
                .iter()
                .map(|i| i.clone() as Arc<dyn OverlayItem>)
                .collect(),
        )
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, _pane: &PaneRef<'_>) -> Vec<FetchTask> {
        log::info!("Fetching SPC storm reports");
        // NOT `ctx.client`: SPC answers OPTIONS with 403, so a `User-Agent`
        // makes all three CSVs fail in the browser. See `spc::fetch`.
        let client = match crate::spc::fetch::spc_client(&ctx.sources) {
            Ok(c) => c,
            Err(e) => {
                log::error!("{e}");
                return Vec::new();
            }
        };
        let sources = ctx.sources.clone();
        vec![FetchTask {
            kind: known::STORM_REPORTS,
            future: Box::pin(async move {
                // The anchor is the WALL CLOCK, never `ctx.as_of`: the file
                // fetched is always the current `today_*.csv`, and only the
                // present says which convective day that file is. A scrubbed
                // pane's `as_of` crossing 12Z would re-date every row of a
                // file that has not changed.
                let anchor = chrono::Utc::now().naive_utc();
                let result =
                    crate::spc::reports::fetch_storm_reports(&client, &sources, anchor).await;
                Box::new(StormReportsFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "SPC Storm Reports".to_string()
        } else {
            format!("SPC Storm Reports ({count})")
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.is_enabled(pane),
        }];

        // Ungated on enabled: a hidden layer's options stay visible and
        // editable, Refresh still fetches, and the status lines keep reporting.
        items.push(ControlItem::ButtonRow {
            buttons: vec![ControlButton {
                id: "refresh",
                label: "\u{21bb} Refresh".into(),
                enabled: !self.state.fetching,
                highlight: false,
            }],
        });
        if self.state.fetching {
            items.push(ControlItem::InfoText {
                text: "Fetching...".into(),
            });
        }
        if let Some(t) = self.state.fetch_time {
            let secs = t.elapsed().as_secs();
            let text = if secs < 60 {
                format!("Updated {secs}s ago")
            } else {
                format!("Updated {}m ago", secs / 60)
            };
            items.push(ControlItem::InfoText { text });
        }

        items
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        match update.id {
            "enabled" => {
                if let ControlValue::Bool(val) = update.value {
                    self.enabled = val;
                    if val
                        && self
                            .state
                            .enable_should_refetch(self.has_data(&pane.as_ref()))
                    {
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    // ── Per-pane state (WO-M10b) ──────────────────────────────────────
    //
    // This layer's only per-pane fact is whether the pane draws it, so its
    // state IS the toggle. `self.enabled` survives as the registry's own copy
    // until WO-M10c deletes the swap that keeps it; every answer below prefers
    // the pane's when a pane is supplied.

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        PaneToggle::create(enabled)
    }

    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        PaneToggle::restore(&value, enabled)
    }

    fn serialize_pane_state(&self, state: &dyn std::any::Any) -> serde_json::Value {
        PaneToggle::save(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squallar_units::HailSizeUnit;

    /// A hail report's size reads in the user's hail-size unit, at the precision
    /// that unit carries: this popup converted but did not drop the two decimals
    /// it needs for inches, so a millimetre reading claimed a hundredth of a mm.
    #[test]
    fn a_hail_reports_size_reads_in_the_users_hail_size_unit() {
        let item = StormReportItem {
            report: StormReport {
                kind: StormReportKind::Hail,
                time: "2015".into(),
                valid: None,
                // 175 hundredths — golf ball, the SPC feed's own encoding.
                magnitude: Some(175.0),
                location: "NORMAN".into(),
                county: "CLEVELAND".into(),
                state: "OK".into(),
                lat: 35.22,
                lon: -97.44,
                comments: String::new(),
            },
            index: 0,
        };
        for (unit, expected) in [
            (HailSizeUnit::Inches, "Size: 1.75\""),
            (HailSizeUnit::Centimeters, "Size: 4.4cm"),
            (HailSizeUnit::Millimeters, "Size: 44mm"),
        ] {
            let prefs = UserPreferences {
                hail_size: unit,
                ..UserPreferences::default()
            };
            let content = item.popup_content(&prefs);
            let Some(PopupSection::Text(size)) = content.sections.get(1) else {
                panic!(
                    "{unit:?}: the magnitude line is not the popup's second section \
                     ({} sections)",
                    content.sections.len(),
                );
            };
            assert_eq!(size, expected, "{unit:?}");
        }
    }

    /// A report whose time field is multi-byte still opens its popup.
    ///
    /// The gate was `time.len() == 4`, in bytes, so `"1é2"` cleared it and
    /// `&time[..2]` then split `é` down the middle — and `popup_content` runs on
    /// the render thread, so one row of a public CSV took the whole app down.
    /// Both clocks are exercised because the local branch re-derives the halves.
    #[test]
    fn a_report_whose_time_is_multibyte_still_opens_its_popup() {
        for time in ["1é2", "éé", "é", "🌀", "12é4", "20:5"] {
            for timezone in [
                squallar_units::TimezonePreference::Utc,
                squallar_units::TimezonePreference::Local,
            ] {
                let item = StormReportItem {
                    report: StormReport {
                        kind: StormReportKind::Tornado,
                        time: time.into(),
                        valid: None,
                        magnitude: None,
                        location: "NORMAN".into(),
                        county: "CLEVELAND".into(),
                        state: "OK".into(),
                        lat: 35.22,
                        lon: -97.44,
                        comments: String::new(),
                    },
                    index: 0,
                };
                let prefs = UserPreferences {
                    timezone,
                    ..UserPreferences::default()
                };
                let content = item.popup_content(&prefs);
                assert!(
                    content.sections.iter().any(|section| matches!(
                        section,
                        PopupSection::Text(text) if text.contains(time),
                    )),
                    "{time:?} ({timezone:?}) must still reach the popup verbatim",
                );
            }
        }
    }

    /// The fix must not have stopped an ordinary HHMM being split into a clock.
    ///
    /// Without this the guard could reject every time and the test above would
    /// still pass, leaving every report reading `2015 UTC` instead of `20:15`.
    #[test]
    fn an_ordinary_hhmm_still_reads_as_a_clock() {
        let item = StormReportItem {
            report: StormReport {
                kind: StormReportKind::Tornado,
                time: "2015".into(),
                valid: None,
                magnitude: None,
                location: "NORMAN".into(),
                county: "CLEVELAND".into(),
                state: "OK".into(),
                lat: 35.22,
                lon: -97.44,
                comments: String::new(),
            },
            index: 0,
        };
        let prefs = UserPreferences {
            timezone: squallar_units::TimezonePreference::Utc,
            ..UserPreferences::default()
        };
        let content = item.popup_content(&prefs);
        assert!(
            content.sections.iter().any(|section| matches!(
                section,
                PopupSection::Text(text) if text.contains("20:15"),
            )),
            "an ASCII HHMM must still be split into a clock",
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod round_tests {
    use super::*;
    use crate::render::overlay_state::{OverlayFetchResult, OverlayRegistry};
    use crate::spc::reports::fetch_storm_reports;

    fn http(status_line: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: text/csv\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        )
    }

    /// Serve the three `today_*.csv` from loopback, so the round under test is
    /// driven over a real socket. The origin comes from `DataSources::spc_base`.
    fn spc_serving(
        responses: Vec<(&'static str, String)>,
    ) -> squallar_source::origins::DataSources {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut scratch = [0u8; 4096];
                let read = stream.read(&mut scratch).unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..read]).to_string();
                let out = responses
                    .iter()
                    .find(|(name, _)| request.contains(&format!("today_{name}.csv")))
                    .map(|(_, r)| r.clone())
                    .unwrap_or_else(|| http("404 Not Found", ""));
                let _ = stream.write_all(out.as_bytes());
                let _ = stream.flush();
            }
        });
        squallar_source::origins::DataSources {
            spc_base: format!("http://127.0.0.1:{port}").into(),
            ..squallar_source::origins::DataSources::production()
        }
    }

    const TORN: &str = "Time,F_Scale,Location,County,State,Lat,Lon,Comments\n\
         2030,UNK,SHAWNEE,POTTAWATOMIE,OK,35.33,-96.92,on the ground\n";
    const HAIL: &str = "Time,Size,Location,County,State,Lat,Lon,Comments\n\
         2015,175,NORMAN,CLEVELAND,OK,35.22,-97.44,golf ball\n";
    const WIND: &str = "Time,Speed,Location,County,State,Lat,Lon,Comments\n\
         2020,60,MOORE,CLEVELAND,OK,35.34,-97.48,trees down\n";

    fn round(responses: Vec<(&'static str, String)>) -> (Option<String>, Option<String>) {
        squallar_source::tls::init();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let sources = spc_serving(responses);
        let result = runtime.block_on(fetch_storm_reports(
            &client,
            &sources,
            chrono::Utc::now().naive_utc(),
        ));

        let kind = known::STORM_REPORTS;
        let mut registry = OverlayRegistry::default();
        registry.set_enabled(&kind, true, &mut PaneMut::bare(0));
        registry.apply_fetch_result(
            OverlayFetchResult {
                kind: kind.clone(),
                data: Box::new(StormReportsFetchResult(result)) as FetchPayload,
            },
            &PaneRef::bare(0),
        );
        let ctx = PaneRef::bare(0);
        let note = registry
            .controls(&kind, &ctx)
            .into_iter()
            .find_map(|item| match item {
                ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
                _ => None,
            });
        (registry.status_line(&kind, &PaneRef::bare(0)), note)
    }

    /// **A CSV that would not load takes a whole kind of report off the map.**
    /// `fetch_storm_reports` refuses the round only when all three failed, which
    /// is right; what was wrong is that it then said nothing — health `Ok`,
    /// `is_incomplete()` false, no mark, a fresh clock.
    #[test]
    fn a_report_csv_that_would_not_load_marks_the_layer_and_names_the_kind() {
        let (line, note) = round(vec![
            ("torn", http("503 Service Unavailable", "down")),
            ("hail", http("200 OK", HAIL)),
            ("wind", http("200 OK", WIND)),
        ]);
        let line = line.expect("an enabled reports layer states its own line");
        assert!(
            line.starts_with("! incomplete"),
            "every tornado report in the country is absent and the row says \
             nothing: {line}",
        );
        assert!(
            line.contains("2 reports"),
            "the layer's own line must survive the mark: {line}",
        );
        let note = note.expect("the options must say what the row is marking");
        assert!(
            note.contains("missing 1 of 3 report kinds"),
            "the note must count the kinds, not the reports: {note}",
        );
        assert!(
            note.contains("tornado") && note.contains("503"),
            "the note must name which kind and why: {note}",
        );
        assert!(
            !line.contains("not updating"),
            "two CSVs answered on a fresh clock — this is not stale: {line}",
        );
    }

    /// A 404 is the SPC rebuilding `today_*.csv` for a kind with nothing in it
    /// yet, which is a **normal answer**. Marking the layer for it would put a
    /// fault on the row on every quiet day.
    #[test]
    fn a_kind_with_nothing_reported_yet_is_not_a_fault() {
        let (line, note) = round(vec![
            ("torn", http("404 Not Found", "")),
            ("hail", http("200 OK", HAIL)),
            ("wind", http("200 OK", WIND)),
        ]);
        let line = line.expect("line");
        assert!(
            !line.starts_with("!"),
            "a quiet tornado day is not an outage: {line}",
        );
        assert_eq!(note, None, "a quiet day must not raise a note: {note:?}");
    }

    #[test]
    fn a_whole_round_carries_no_mark() {
        let (line, note) = round(vec![
            ("torn", http("200 OK", TORN)),
            ("hail", http("200 OK", HAIL)),
            ("wind", http("200 OK", WIND)),
        ]);
        let line = line.expect("line");
        assert!(line.contains("3 reports"), "{line}");
        assert!(!line.starts_with("!"), "nothing failed: {line}");
        assert_eq!(note, None);
    }

    /// All three failing is still a failed round, on the health axis: this one
    /// is stale, not incomplete.
    #[test]
    fn every_csv_failing_is_still_a_failed_round() {
        let (line, _) = round(vec![
            ("torn", http("503 Service Unavailable", "down")),
            ("hail", http("503 Service Unavailable", "down")),
            ("wind", http("503 Service Unavailable", "down")),
        ]);
        let line = line.expect("line");
        assert!(
            line.contains("not updating"),
            "a round where nothing answered is stale: {line}",
        );
        assert!(
            !line.contains("incomplete"),
            "nothing arrived to be incomplete about: {line}",
        );
    }
}

/// **The one `EventLifetime` layer whose ask is a stretch and not an
/// instant.** The picture at a stop is every report of the convective day
/// that has already happened, so the slice feeding it opens at 12Z and closes
/// at the stop.
///
/// Thirteen hourly stops therefore coalesce into **one** range — each stop's
/// slice contains its predecessor's. That is the honest answer for a layer
/// holding one CSV per kind for the whole day, and it is the opposite of the
/// lightning layer's thirteen: the difference is in what the pictures are
/// functions of, and the type carries both.
#[cfg(test)]
mod residency_tests {
    use super::*;
    use squallar_source::handler::SourceHandler;

    fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
            .expect("a real date")
            .and_hms_opt(h, m, 0)
            .expect("a real time")
    }

    #[test]
    fn a_report_stop_asks_for_the_convective_day_behind_it() {
        let handler = StormReportsHandler::new();
        let pane = PaneRef::bare(0);

        let one = handler.residency_for(&pane, &[at(20, 0)]);
        assert_eq!(
            one.ranges().len(),
            1,
            "one stop, one slice: {:?}",
            one.ranges(),
        );
        assert_eq!(
            one.total(),
            chrono::Duration::hours(8),
            "20Z is eight hours into a convective day that opened at 12Z",
        );
        assert!(
            one.covers(at(12, 0)),
            "the day's first report is inside the ask; an instant-only answer \
             would let a retention rule drop every report behind the playhead",
        );
        assert!(
            !one.covers(at(11, 59)),
            "and the previous convective day is not this layer's to hold",
        );

        let looped: Vec<chrono::NaiveDateTime> = (13..=23).map(|h| at(h, 0)).collect();
        let many = handler.residency_for(&pane, &looped);
        assert_eq!(
            many.ranges().len(),
            1,
            "every stop's slice contains its predecessor's, so eleven stops \
             coalesce to one: {:?}",
            many.ranges(),
        );
        assert_eq!(
            many.total(),
            chrono::Duration::hours(11),
            "12Z to the newest stop, counted once",
        );
        for stop in &looped {
            assert!(many.covers(*stop), "the stop at {stop} is inside the ask");
        }
    }

    /// A stop **before** 12Z belongs to the convective day that opened at 12Z
    /// the previous calendar day — the rollover `report_instant` dates its
    /// rows by, read the other way.
    #[test]
    fn a_small_hours_stop_belongs_to_yesterdays_convective_day() {
        let handler = StormReportsHandler::new();
        let pane = PaneRef::bare(0);

        let residency = handler.residency_for(&pane, &[at(2, 30)]);
        assert_eq!(
            residency.extent(),
            Some((
                chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
                    .expect("a real date")
                    .and_hms_opt(12, 0, 0)
                    .expect("a real time"),
                at(2, 30),
            )),
            "02:30 is 14.5 h into the day that opened at 12Z yesterday, not \
             half an hour into one that opens at midnight",
        );
    }
}

/// **WB-2: the pane clock reaches the reports raster.** End-to-end through the
/// real rasterizer — what the worker receives and what it puts on the glass —
/// never a re-statement of the handler's own filter (there is none: the cull
/// is the rasterizer's, so rows and hit-map ids stay aligned).
#[cfg(test)]
mod as_of_tests {
    use super::*;

    fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn handler_with_report(valid: Option<chrono::NaiveDateTime>) -> StormReportsHandler {
        let mut handler = StormReportsHandler::new();
        handler.apply_fetch_result(
            Box::new(StormReportsFetchResult(Ok(StormReportRound {
                reports: vec![StormReport {
                    kind: StormReportKind::Tornado,
                    time: "2130".into(),
                    valid,
                    magnitude: None,
                    location: "NORMAN".into(),
                    county: "CLEVELAND".into(),
                    state: "OK".into(),
                    lat: 35.0,
                    lon: -97.5,
                    comments: String::new(),
                }],
                failed_kinds: Vec::new(),
            }))),
            &PaneRef::bare(0),
        );
        handler
    }

    fn ctx(as_of: chrono::NaiveDateTime) -> RasterizeContext {
        RasterizeContext {
            is_dark: false,
            zoom: 6.5,
            device_scale: 1.0,
            now: as_of,
            as_of,
            frame: None,
        }
    }

    /// The raster the handler's input really produces at `as_of`: whether any
    /// ink landed, how many distinct reports the hit map records, and how
    /// many rows travelled.
    fn rendered(
        handler: &StormReportsHandler,
        as_of: chrono::NaiveDateTime,
    ) -> (bool, usize, usize) {
        let input = handler.paint_input(&ctx(as_of)).expect("data is present");
        let bounds = squallar_geo::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        };
        let out = rasterize::rasterize_storm_reports(&input, &bounds, 96, 64);
        let inked = out.rgba.iter().any(|&b| b != 0);
        let ids: std::collections::HashSet<u32> = out
            .hit_cells
            .expect("the reports rasterizer builds cells")
            .cells
            .values()
            .flatten()
            .copied()
            .collect();
        (inked, ids.len(), input.reports.len())
    }

    /// **The floor's assertion: absent before its time.** A 21:30 report puts
    /// zero reports on the glass at 21:29 and one at 21:31 — ink and hit map
    /// both. The row still TRAVELS in both cases: the cull is the
    /// rasterizer's, so the hit-map alignment invariant (`items.len() ==
    /// input.reports.len()`) holds while the picture changes.
    #[test]
    fn a_report_is_absent_before_its_instant_and_present_from_it() {
        let handler = handler_with_report(Some(at(21, 30)));

        let (inked, hits, rows) = rendered(&handler, at(21, 29));
        assert_eq!(rows, 1, "the row must travel even while culled");
        assert!(
            !inked,
            "a 21:30 report put ink on the glass at 21:29 - it has not \
             happened yet",
        );
        assert_eq!(hits, 0, "and the hit map must not offer it to a hover");

        let (inked, hits, rows) = rendered(&handler, at(21, 31));
        assert_eq!(rows, 1);
        assert!(inked, "one minute after its instant the report must draw");
        assert_eq!(hits, 1, "and be hoverable");
    }

    /// **The cross-cutting non-triviality: a live pane is byte-identical.**
    /// On a live pane `as_of` is the wall clock and every already-fetched
    /// report has already happened, so the cull passes every row — the rgba
    /// is byte-for-byte the rgba of the same input with no instants at all
    /// (the pre-WB-2 picture).
    #[test]
    fn a_live_pane_draws_byte_identical_to_the_unclocked_picture() {
        let now = at(23, 0);
        let dated = handler_with_report(Some(at(21, 30)));
        let undated = handler_with_report(None);
        let bounds = squallar_geo::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        };
        let a = rasterize::rasterize_storm_reports(
            &dated.paint_input(&ctx(now)).expect("data"),
            &bounds,
            96,
            64,
        );
        let b = rasterize::rasterize_storm_reports(
            &undated.paint_input(&ctx(now)).expect("data"),
            &bounds,
            96,
            64,
        );
        assert!(
            a.rgba.iter().any(|&px| px != 0),
            "non-triviality floor: the compared picture must have ink in it",
        );
        assert_eq!(
            a.rgba, b.rgba,
            "a live pane's reports raster gained an as-of dependence it must \
             not have",
        );
    }
}
