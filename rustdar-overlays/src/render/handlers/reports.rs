use std::any::Any;
use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupContent, PopupSection, RasterizeContext, RasterizeFn, RenderMode,
};
use crate::render::rasterize;
use crate::spc::reports::{StormReport, StormReportKind};
use crate::types::GeoBounds;

pub(crate) struct StormReportsFetchResult(pub Result<Vec<StormReport>, String>);

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
        let formatted_time = if report.time.len() == 4 {
            let hhmm = format!("{}:{}", &report.time[..2], &report.time[2..]);
            match prefs.timezone {
                rustdar_units::TimezonePreference::Utc => format!("{hhmm} UTC"),
                rustdar_units::TimezonePreference::Local => {
                    if let (Ok(h), Ok(m)) = (
                        report.time[..2].parse::<u32>(),
                        report.time[2..].parse::<u32>(),
                    ) {
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
            "{formatted_time} — {}, {} {}",
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
    pub state: OverlayState<Vec<Arc<StormReportItem>>>,
    pub enabled: bool,
}

impl StormReportsHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            enabled: false,
        }
    }
}

impl OverlayHandler for StormReportsHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::StormReports
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

    fn fetch_time(&self) -> Option<web_time::Instant> {
        self.state.fetch_time
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(300)
    }

    fn clickable_items(&self) -> Vec<ClickableItem> {
        Vec::new() // Clicks resolve through the rasterizer's `HitMap` instead.
    }

    fn apply_fetch_result(&mut self, result: FetchPayload) {
        let Some(fetch) = result.downcast::<StormReportsFetchResult>().ok() else {
            log::error!("Storm reports handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(reports) => {
                log::info!("Received {} storm reports", reports.len());
                let items = reports
                    .into_iter()
                    .enumerate()
                    .map(|(i, report)| Arc::new(StormReportItem { report, index: i }))
                    .collect();
                self.state.set_data(items);
            }
            Err(e) => {
                log::error!("Storm reports fetch failed: {e}");
            }
        }
        self.state.fetching = false;
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

    fn prepare_rasterize(&self, ctx: &RasterizeContext) -> Option<RasterizeFn> {
        if self.state.data.is_empty() {
            return None;
        }
        let reports: Vec<StormReport> = self.state.data.iter().map(|i| i.report.clone()).collect();
        let items: Vec<Arc<dyn OverlayItem>> = self
            .state
            .data
            .iter()
            .map(|i| i.clone() as Arc<dyn OverlayItem>)
            .collect();
        let zoom = ctx.zoom;
        let is_dark = ctx.is_dark;
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            rasterize::rasterize_storm_reports(
                &reports, &items, bounds, width, height, zoom, is_dark,
            )
        }))
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
            kind: OverlayKind::StormReports,
            future: Box::pin(async move {
                let result = crate::spc::reports::fetch_storm_reports(&client, &sources).await;
                Box::new(StormReportsFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "\u{26a1}  SPC Storm Reports".to_string()
        } else {
            format!("\u{26a1}  SPC Storm Reports ({count})")
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.enabled,
        }];

        if self.enabled {
            items.push(ControlItem::ButtonRow {
                buttons: vec![ControlButton {
                    id: "refresh",
                    label: "\u{1f504} Refresh".into(),
                    enabled: !self.state.fetching,
                    highlight: false,
                }],
            });
            if self.state.fetching {
                items.push(ControlItem::InfoText {
                    text: "Fetching\u{2026}".into(),
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
                    if val && !self.has_data() && !self.state.fetching {
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
}
