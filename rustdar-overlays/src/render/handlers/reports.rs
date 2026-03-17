use std::any::Any;

use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchTask, OverlayHandler, OverlayKind, OverlayState,
    PopupContent, PopupSection, RasterizeContext, SelectedOverlay,
};
use crate::render::rasterize;
use crate::spc::reports::{StormReport, StormReportKind};
use crate::types::GeoBounds;

/// Type-erased fetch result for SPC storm reports.
pub(crate) struct StormReportsFetchResult(pub Result<Vec<StormReport>, String>);

pub(crate) struct StormReportsHandler {
    pub state: OverlayState<Vec<StormReport>>,
}

impl StormReportsHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
        }
    }
}

impl OverlayHandler for StormReportsHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::StormReports
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation    }

    fn has_data(&self) -> bool {
        !self.state.data.is_empty()
    }

    fn is_fetching(&self) -> bool {
        self.state.fetching
    }

    fn set_fetching(&mut self, fetching: bool) {
        self.state.fetching = fetching;
    }

    fn fetch_time(&self) -> Option<std::time::Instant> {
        self.state.fetch_time
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(300) // Refresh every 5 min
    }

    fn clickable_items(&self, _layers: &crate::render::layers::LayerManager) -> Vec<ClickableItem<'_>> {
        self.state
            .data
            .iter()
            .enumerate()
            .map(|(i, report)| ClickableItem {
                features: vec![&report.feature],
                label: None,
                id: SelectedOverlay::StormReport { index: i },
            })
            .collect()
    }

    fn popup_content(&self, selected: &SelectedOverlay) -> Option<PopupContent> {
        let SelectedOverlay::StormReport { index } = selected else { return None };
        let report = self.state.data.get(*index)?;
        let kind_str = match report.kind {
            StormReportKind::Tornado => "Tornado",
            StormReportKind::Hail => "Hail",
            StormReportKind::Wind => "Wind",
        };
        // Format HHMM → "HH:MM UTC"
        let formatted_time = if report.time.len() == 4 {
            format!("{}:{} UTC", &report.time[..2], &report.time[2..])
        } else {
            format!("{} UTC", report.time)
        };
        let mut sections = vec![
            PopupSection::Text(format!("{formatted_time} — {}, {} {}", report.location, report.county, report.state)),
        ];
        if let Some(mag) = report.magnitude {
            let mag_text = match report.kind {
                StormReportKind::Tornado => format!("F/EF Scale: {mag}"),
                StormReportKind::Hail => format!("Size: {:.2}\"", mag / 100.0),
                StormReportKind::Wind => format!("Speed: {mag} kt"),
            };
            sections.push(PopupSection::Text(mag_text));
        }
        if !report.comments.is_empty() {
            sections.push(PopupSection::Text(report.comments.clone()));
        }
        Some(PopupContent {
            title: format!("Storm Report: {kind_str}"),
            accent_rgb: match report.kind {
                StormReportKind::Tornado => [220, 40, 40],
                StormReportKind::Hail => [40, 180, 40],
                StormReportKind::Wind => [40, 80, 220],
            },
            width: 350.0,
            sections,
            actions: Vec::new(),
        })
    }

    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>) {
        let Some(fetch) = result.downcast::<StormReportsFetchResult>().ok() else {
            log::error!("Storm reports handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(reports) => {
                log::info!("Received {} storm reports", reports.len());
                self.state.data = reports;
                self.state.data_generation = self.state.data_generation.wrapping_add(1);
                self.state.fetch_time = Some(std::time::Instant::now());
            }
            Err(e) => {
                log::error!("Storm reports fetch failed: {e}");
            }
        }
        self.state.fetching = false;
    }

    fn retain_selections(&self, selections: &mut Vec<SelectedOverlay>) {
        let count = self.state.data.len();
        selections.retain(|s| {
            if let SelectedOverlay::StormReport { index } = s {
                *index < count
            } else {
                true
            }
        });
    }

    fn prepare_rasterize(
        &self,
        ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> Vec<u8> + Send>> {
        if self.state.data.is_empty() {
            return None;
        }
        let reports = self.state.data.clone();
        let zoom = ctx.zoom;
        let is_dark = ctx.is_dark;
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            rasterize::rasterize_storm_reports(&reports, bounds, width, height, zoom, is_dark)
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        log::info!("Fetching SPC storm reports");
        let client = ctx.client.clone();
        vec![FetchTask {
            kind: OverlayKind::StormReports,
            future: Box::pin(async move {
                let result = crate::spc::reports::fetch_storm_reports(&client).await;
                Box::new(StormReportsFetchResult(result)) as Box<dyn Any + Send>
            }),
        }]
    }
}
