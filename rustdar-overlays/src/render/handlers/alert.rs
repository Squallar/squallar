use std::any::Any;
use std::collections::HashSet;

use crate::nws::alert::NwsAlert;
use crate::render::layers::LayerManager;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchTask, OverlayHandler, OverlayKind, OverlayState,
    PopupAction, PopupActionKind, PopupContent, PopupSection, RasterizeContext,
    SelectedOverlay, format_iso_time,
};
use crate::render::rasterize;
use crate::types::GeoBounds;

/// Type-erased fetch result for NWS alerts.
pub(crate) struct NwsAlertFetchResult(pub Result<Vec<NwsAlert>, String>);

pub(crate) struct NwsAlertHandler {
    pub state: OverlayState<Vec<NwsAlert>>,
    /// Alert IDs hidden by the user (not rendered on the map).
    pub hidden_alerts: HashSet<String>,
}

impl NwsAlertHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            hidden_alerts: HashSet::new(),
        }
    }
}

impl OverlayHandler for NwsAlertHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::NwsAlerts
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

    fn fetch_time(&self) -> Option<std::time::Instant> {
        self.state.fetch_time
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(120)
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn clickable_items(&self, layers: &LayerManager) -> Vec<ClickableItem<'_>> {
        let enabled_categories = layers.enabled_nws_categories();
        self.state.data.iter()
            .filter(|alert| {
                enabled_categories.contains(&alert.category)
                    && !self.hidden_alerts.contains(&alert.id)
            })
            .map(|alert| ClickableItem {
                features: alert.features.iter().collect(),
                label: None,
                id: SelectedOverlay::Alert(alert.id.clone()),
            })
            .collect()
    }

    fn popup_content(&self, selected: &SelectedOverlay) -> Option<PopupContent> {
        let SelectedOverlay::Alert(alert_id) = selected else { return None };
        let alert = self.state.data.iter().find(|a| a.id == *alert_id)?;
        let [r, g, b, _] = alert.features.first()
            .map(|f| f.stroke_rgba)
            .unwrap_or([200, 200, 200, 255]);

        let mut sections = Vec::new();

        if let Some(headline) = &alert.headline {
            sections.push(PopupSection::Heading(headline.clone()));
        }

        sections.push(PopupSection::KeyValueGrid(vec![
            ("Areas".into(), alert.area_desc.clone()),
            ("Issued by".into(), alert.sender_name.clone()),
            ("Effective".into(), format_iso_time(&alert.effective)),
            ("Expires".into(), format_iso_time(&alert.expires)),
        ]));

        sections.push(PopupSection::Separator);

        sections.push(PopupSection::ScrollableText {
            text: alert.description.clone(),
            monospace: false,
            max_height: 250.0,
        });

        if let Some(instruction) = &alert.instruction {
            sections.push(PopupSection::Separator);
            sections.push(PopupSection::ColoredText {
                text: instruction.clone(),
                rgb: [r, g, b],
                bold: true,
            });
        }

        Some(PopupContent {
            title: alert.event.clone(),
            accent_rgb: [r, g, b],
            width: 380.0,
            sections,
            actions: vec![PopupAction {
                label: "\u{1f6ab}  Hide from map".into(),
                target: selected.clone(),
                kind: PopupActionKind::HideFromMap,
            }],
        })
    }

    fn handle_popup_action(&mut self, action: &PopupAction) -> bool {
        match action.kind {
            PopupActionKind::HideFromMap => {
                if let SelectedOverlay::Alert(ref id) = action.target {
                    self.hidden_alerts.insert(id.clone());
                    self.state.data_generation = self.state.data_generation.wrapping_add(1);
                    return true;
                }
                false
            }
        }
    }

    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>) {
        let Some(fetch) = result.downcast::<NwsAlertFetchResult>().ok() else {
            log::error!("NWS alert handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(alerts) => {
                log::info!("Received {} NWS alerts", alerts.len());
                // Clean hidden_alerts of IDs no longer present
                let current_ids: HashSet<String> = alerts.iter().map(|a| a.id.clone()).collect();
                self.hidden_alerts.retain(|id| current_ids.contains(id));
                self.state.set_data(alerts);
            }
            Err(e) => {
                log::error!("NWS alerts fetch failed: {}", e);
            }
        }
        self.state.fetching = false;
    }

    fn retain_selections(&self, selections: &mut Vec<SelectedOverlay>) {
        let current_ids: HashSet<&str> = self.state.data.iter().map(|a| a.id.as_str()).collect();
        selections.retain(|sel| match sel {
            SelectedOverlay::Alert(id) => current_ids.contains(id.as_str()),
            _ => true,
        });
    }

    fn prepare_rasterize(
        &self,
        ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> Vec<u8> + Send>> {
        if self.state.data.is_empty() {
            return None;
        }
        let alerts = self.state.data.clone();
        let enabled_categories = ctx.enabled_nws_categories.clone();
        let hidden_alerts = self.hidden_alerts.clone();
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            rasterize::rasterize_nws_alerts(
                &alerts,
                &enabled_categories,
                &hidden_alerts,
                bounds,
                width,
                height,
            )
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        log::info!("Fetching NWS active alerts");
        let client = ctx.client.clone();
        let zone_cache = ctx.zone_cache_dir.clone();
        vec![FetchTask {
            kind: OverlayKind::NwsAlerts,
            future: Box::pin(async move {
                let result = crate::nws::fetch::fetch_active_alerts(
                    &client,
                    zone_cache.as_deref(),
                )
                    .await
                    .map_err(|e| format!("{e}"));
                Box::new(NwsAlertFetchResult(result)) as Box<dyn Any + Send>
            }),
        }]
    }
}
