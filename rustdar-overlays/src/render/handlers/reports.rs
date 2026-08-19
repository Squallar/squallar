use std::any::Any;
use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::fetch_policy::Assembled;
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupContent, PopupSection, RasterizeContext, RenderMode,
};
use crate::render::rasterize;
use crate::spc::reports::{StormReport, StormReportKind, StormReportRound};
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::{DescribedJob, JobCodec};

// `pub`, not `pub(crate)`, for `alert::NwsAlertFetchResult`'s reason: the
// described-job dispatch tests in `rustdar-app` and the hit-map zip tests in `rustdar-worker` seed a
// live registry through `apply_fetch_result`, and the payload type has to be
// nameable where the test constructs it.
pub struct StormReportsFetchResult(pub Result<StormReportRound, crate::fetch_policy::FetchError>);
/// [`Assembled`]: three CSVs, one per report kind, fetched independently and
/// refused as a round only when **all three** failed. One failing arrives here
/// as `Ok` with a whole kind of report absent from the map.
///
/// Observed: the tornado CSV answering 503 took every tornado report in the
/// country off the map, byte-for-byte indistinguishable from a quiet day on
/// every user-visible surface.
///
/// [`Assembled`]: crate::fetch_policy::Assembled
impl crate::fetch_policy::FetchRound for StormReportsFetchResult {
    type Shape = crate::fetch_policy::Assembled;
}

#[derive(Debug)]
pub(crate) struct StormReportItem {
    pub report: StormReport,
    /// The reports feed carries no IDs, so position in the fetch is the only
    /// identity available. It is what `matches()` and the hit map both key on.
    pub index: usize,
}

impl OverlayItem for StormReportItem {
    fn kind(&self) -> OverlayKind {
        OverlayKind::StormReports
    }

    fn popup_content(&self, prefs: &UserPreferences) -> PopupContent {
        let report = &self.report;
        let kind_str = match report.kind {
            StormReportKind::Tornado => "Tornado",
            StormReportKind::Hail => "Hail",
            StormReportKind::Wind => "Wind",
        };
        // The feed gives HHMM with no date, so local conversion has to assume
        // today's date.
        //
        // Four ASCII digits, which is what "the feed gives HHMM" means, tested
        // as such rather than as a byte count. `time` is a field of the SPC
        // reports CSV and nothing between the fetch and here checks its
        // charset, so `len() == 4` admitted two different wrong values: "1é2"
        // is four bytes and `&time[..2]` split `é` down the middle — a panic in
        // `popup_content`, on the render thread, taking the app rather than the
        // overlay — and "éé" is four bytes that split *legally* into a clock
        // reading "é:é". The digit test rejects both, and anything it rejects
        // is shown below exactly as the feed sent it, which is the only place
        // the user can see that it was unreadable.
        let is_hhmm = report.time.len() == 4 && report.time.bytes().all(|b| b.is_ascii_digit());
        // `split_at_checked` even so. The digit test above already makes the
        // split legal, and it costs nothing to have the total form guard it
        // anyway rather than leave the next edit to this line one condition
        // away from the panic it replaced.
        let split = is_hhmm.then(|| report.time.split_at_checked(2)).flatten();
        let formatted_time = if let Some((hh, mm)) = split {
            let hhmm = format!("{hh}:{mm}");
            match prefs.timezone {
                rustdar_units::TimezonePreference::Utc => format!("{hhmm} UTC"),
                rustdar_units::TimezonePreference::Local => {
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
                // The precision comes from the unit rather than being fixed at
                // hundredths, so a report reads `1.75"`, `4.4cm` or `44mm` and
                // not `44.45mm` — a hundredth of a millimetre nobody estimated.
                // Same rule as the MEHS readout (`RadarProduct::format_value`),
                // so the two hail sizes a pane can show agree about how precise
                // a hail size is.
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

    /// What the rasterizer reads, captured once — the **one** builder
    /// `prepare_job` answers from, kept a private helper so a second dispatch
    /// path could not quietly capture different state.
    ///
    /// The rows are [`rasterize::ReportPaint`], not whole [`StormReport`]s:
    /// the kind and the coordinates are everything the raster reads, and the
    /// described job serialises this struct onto a message port — the time,
    /// magnitude and comments are popup content the raster never draws.
    ///
    /// **Row `i` is `state.data[i]`'s report**, which is the same indexing
    /// [`Self::hit_items`] answers — one iteration order, stated in both
    /// places, because a hit-map id is a position in this list and the item
    /// it resolves to is the same position in that one.
    fn paint_input(&self, ctx: &RasterizeContext) -> Option<rasterize::ReportsInput> {
        if self.state.data.is_empty() {
            return None;
        }
        Some(rasterize::ReportsInput {
            reports: self
                .state
                .data
                .iter()
                .map(|i| rasterize::ReportPaint {
                    kind: i.report.kind,
                    lat: i.report.lat,
                    lon: i.report.lon,
                })
                .collect(),
            zoom: ctx.zoom,
            is_dark: ctx.is_dark,
            device_scale: ctx.device_scale,
        })
    }
}

impl OverlayHandler for StormReportsHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::StormReports
    }
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

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// E.g. `"27 reports"` — today's filtered report count.
    fn status_line(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        Some(format!("{} reports", self.state.data.len()))
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    fn has_data(&self) -> bool {
        !self.state.data.is_empty()
    }

    fn is_fetching(&self) -> bool {
        self.state.fetching
    }

    fn set_fetching(&mut self, fetching: bool) {
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

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(300)
    }

    fn clickable_items(&self) -> Vec<ClickableItem<'_>> {
        Vec::new() // Clicks resolve through the rasterizer's `HitMap` instead.
    }

    fn apply_fetch_result(&mut self, result: FetchPayload) {
        let Some(fetch) = self.state.downcast_round::<StormReportsFetchResult>(result) else {
            log::error!("Storm reports handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(round) => {
                log::info!("Received {} storm reports", round.reports.len());
                // The coverage report travels with the data it describes.
                // `fetch_storm_reports` refuses the round only when **all
                // three** CSVs failed, so one or two failing arrives here as
                // `Ok` — a good answer with a whole kind of report absent from
                // it, which is the coverage axis and not the health one. It
                // used to go through `set_data`, which declares an answer
                // whole, so every tornado report in the country could be off
                // the map under a green `Updated 0s ago`.
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
                // **Undocumented behaviour, named rather than changed.**
                // `spc::reports::fetch` errors only when all three CSVs failed,
                // and this branch does not clear `state.data` — so a total
                // outage leaves the previous poll's reports on the map instead
                // of emptying it. Probably the better answer for a product that
                // only accumulates through the day, but it does mean the map
                // can be showing an hour-old report set that looks current.
                // What makes that safe is the health note
                // `OverlayRegistry::controls` now prepends for every layer;
                // before it, this layer said nothing at all.
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>) {
        let count = self.state.data.len();
        selections.retain(|sel| {
            if sel.kind() != OverlayKind::StormReports {
                return true;
            }
            sel.as_any()
                .downcast_ref::<StormReportItem>()
                .is_some_and(|r| r.index < count)
        });
    }

    fn prepare_job(&self, ctx: &RasterizeContext) -> Option<DescribedJob> {
        Some(DescribedJob::new(self.paint_input(ctx)?))
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/reports")
    }

    /// Index-aligned with [`Self::paint_input`]'s rows: both iterate
    /// `state.data` in order, so `hit_items()[i]` **is** the item whose
    /// report travelled at row `i` — the invariant
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

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
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
                let result = crate::spc::reports::fetch_storm_reports(&client, &sources).await;
                Box::new(StormReportsFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "SPC Storm Reports".to_string()
        } else {
            format!("SPC Storm Reports ({count})")
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.enabled,
        }];

        // Ungated on enabled (the every-option rule, M9.1): a hidden
        // layer's options stay visible and editable - edits take effect
        // when the eye shows it again - Refresh still fetches (nothing
        // on the fetch path reads enabled), and the status lines keep
        // reporting.
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

    fn apply_control(
        &mut self,
        update: &ControlUpdate,
        _ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
        match update.id {
            "enabled" => {
                if let ControlValue::Bool(val) = update.value {
                    self.enabled = val;
                    if val && self.state.enable_should_refetch(self.has_data()) {
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    fn serialize_state(&self) -> serde_json::Value {
        serde_json::json!({ "enabled": self.enabled })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(enabled) = value.get("enabled").and_then(|v| v.as_bool()) {
            self.enabled = enabled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_units::HailSizeUnit;

    /// A hail report's size reads in the user's hail-size unit, at the precision
    /// that unit carries.
    ///
    /// This popup is the app's *other* hail size, beside the MEHS product's
    /// readout, and it already converted; what it did not do was drop the two
    /// decimals it needs for inches, so a millimetre reading claimed a hundredth
    /// of a millimetre out of a size somebody estimated by eye against a golf
    /// ball. The inches row is unchanged — `{:.2}` and the inch mark are what
    /// this line has always printed for the default.
    #[test]
    fn a_hail_reports_size_reads_in_the_users_hail_size_unit() {
        let item = StormReportItem {
            report: StormReport {
                kind: StormReportKind::Hail,
                time: "2015".into(),
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
    /// `time` is `parts[0]` of the SPC storm-reports CSV, trimmed and kept
    /// verbatim — nothing between the fetch and here checks its charset. The
    /// gate was `time.len() == 4`, in bytes, so `"1é2"` cleared it and
    /// `&time[..2]` then split `é` down the middle. This is the worst-placed
    /// of the family: `popup_content` runs on the render thread, so one row of
    /// a public CSV took the whole app down, not one overlay.
    ///
    /// Both clocks are exercised because the local branch re-derives the same
    /// two halves, and it was the branch a user with the default timezone
    /// would land on.
    #[test]
    fn a_report_whose_time_is_multibyte_still_opens_its_popup() {
        for time in ["1é2", "éé", "é", "🌀", "12é4", "20:5"] {
            for timezone in [
                rustdar_units::TimezonePreference::Utc,
                rustdar_units::TimezonePreference::Local,
            ] {
                let item = StormReportItem {
                    report: StormReport {
                        kind: StormReportKind::Tornado,
                        time: time.into(),
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
                // The assertion is that this returns at all. An unreadable
                // time is shown as it stands rather than repaired: the popup
                // is the only place the user can see the feed said something
                // this build could not read.
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
    /// Without this, the guard could reject every time and the test above would
    /// still pass, leaving every report reading `2015 UTC` instead of `20:15`.
    #[test]
    fn an_ordinary_hhmm_still_reads_as_a_clock() {
        let item = StormReportItem {
            report: StormReport {
                kind: StormReportKind::Tornado,
                time: "2015".into(),
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
            timezone: rustdar_units::TimezonePreference::Utc,
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
    use crate::render::controls::PaneControlContext;
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
    /// driven over a real socket rather than around it. The origin comes from
    /// `DataSources::spc_base`, which the fetch never spells.
    fn spc_serving(responses: Vec<(&'static str, String)>) -> rustdar_source::origins::DataSources {
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
        rustdar_source::origins::DataSources {
            spc_base: format!("http://127.0.0.1:{port}").into(),
            ..rustdar_source::origins::DataSources::production()
        }
    }

    const TORN: &str = "Time,F_Scale,Location,County,State,Lat,Lon,Comments\n\
         2030,UNK,SHAWNEE,POTTAWATOMIE,OK,35.33,-96.92,on the ground\n";
    const HAIL: &str = "Time,Size,Location,County,State,Lat,Lon,Comments\n\
         2015,175,NORMAN,CLEVELAND,OK,35.22,-97.44,golf ball\n";
    const WIND: &str = "Time,Speed,Location,County,State,Lat,Lon,Comments\n\
         2020,60,MOORE,CLEVELAND,OK,35.34,-97.48,trees down\n";

    /// Fetch over the socket and push the result through the production ingest
    /// path, returning the row and the options note a user would see.
    fn round(responses: Vec<(&'static str, String)>) -> (Option<String>, Option<String>) {
        rustdar_source::tls::init();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let sources = spc_serving(responses);
        let result = runtime.block_on(fetch_storm_reports(&client, &sources));

        let kind = OverlayKind::StormReports;
        let mut registry = OverlayRegistry::default();
        registry.set_enabled(kind, true);
        registry.apply_fetch_result(OverlayFetchResult {
            kind: kind.id(),
            data: Box::new(StormReportsFetchResult(result)) as FetchPayload,
        });
        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        let note = registry
            .controls(kind, &ctx)
            .into_iter()
            .find_map(|item| match item {
                ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
                _ => None,
            });
        (registry.status_line(kind), note)
    }

    /// **A CSV that would not load takes a whole kind of report off the map.**
    ///
    /// `fetch_storm_reports` refuses the round only when all three failed,
    /// which is right — hail and wind reports are worth drawing without the
    /// tornado CSV. What was wrong is that it then said nothing. Measured over
    /// this socket against the shipped code: health `Ok`, `is_incomplete()`
    /// false, no mark, a fresh clock, and the round byte-for-byte
    /// indistinguishable from a whole one on every surface a user can see.
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
    /// fault on the row on every quiet day, which is the failure mode in the
    /// other direction and would teach a user to ignore the mark.
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

    /// The control: three CSVs answering carries no mark at all.
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

    /// All three failing is still a failed round, on the health axis. The two
    /// axes must not have swapped places: this one is stale, not incomplete —
    /// the previous poll's reports stay on the map deliberately.
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
