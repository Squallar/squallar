use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use crate::nws::alert::{AlertCategory, NwsAlert};
use crate::render::controls::{ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext, PaneControlContextMut};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupAction, PopupActionKind, PopupContent, PopupSection, RasterizeContext,
    RenderMode,
};
use crate::render::rasterize::{self, RasterizeOutput};
use crate::types::GeoBounds;

/// Type-erased fetch result for NWS alerts.
pub(crate) struct NwsAlertFetchResult(pub Result<Vec<NwsAlert>, String>);

/// Clickable item representing a single NWS alert.
#[derive(Debug)]
pub(crate) struct AlertItem {
    pub alert: NwsAlert,
}

impl OverlayItem for AlertItem {
    fn kind(&self) -> OverlayKind {
        OverlayKind::NwsAlerts
    }

    fn popup_content(&self, prefs: &rustdar_units::UserPreferences) -> PopupContent {
        let alert = &self.alert;
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
            ("Effective".into(), prefs.timezone.format_rfc3339(&alert.effective)),
            ("Expires".into(), prefs.timezone.format_rfc3339(&alert.expires)),
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

        PopupContent {
            title: alert.event.clone(),
            accent_rgb: [r, g, b],
            width: 380.0,
            sections,
            actions: Vec::new(), // TODO: re-add hide action with Arc<dyn OverlayItem>
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<AlertItem>()
            .is_some_and(|o| o.alert.id == self.alert.id)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub(crate) struct NwsAlertHandler {
    pub state: OverlayState<Vec<Arc<AlertItem>>>,
    /// Alert IDs hidden by the user (not rendered on the map).
    pub hidden_alerts: HashSet<String>,
    /// Which alert categories are enabled.
    pub enabled_categories: HashSet<AlertCategory>,
}

impl NwsAlertHandler {
    pub fn new() -> Self {
        let mut enabled = HashSet::new();
        enabled.insert(AlertCategory::Warning);
        enabled.insert(AlertCategory::Watch);
        enabled.insert(AlertCategory::Advisory);
        Self {
            state: OverlayState::new(),
            hidden_alerts: HashSet::new(),
            enabled_categories: enabled,
        }
    }
}

impl OverlayHandler for NwsAlertHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::NwsAlerts
    }

    fn display_name(&self) -> &str {
        "NWS Alerts"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn is_enabled(&self) -> bool {
        !self.enabled_categories.is_empty()
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

    fn clickable_items(&self) -> Vec<ClickableItem> {
        self.state.data.iter()
            .filter(|item| {
                self.enabled_categories.contains(&item.alert.category)
                    && !self.hidden_alerts.contains(&item.alert.id)
            })
            .map(|item| ClickableItem {
                features: item.alert.features.clone(),
                label: None,
                item: item.clone() as Arc<dyn OverlayItem>,
            })
            .collect()
    }

    fn handle_popup_action(&mut self, action: &PopupAction) -> bool {
        match action.kind {
            PopupActionKind::HideFromMap => {
                if let Some(alert_item) = action.target.as_any().downcast_ref::<AlertItem>() {
                    self.hidden_alerts.insert(alert_item.alert.id.clone());
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
                let current_ids: HashSet<String> = alerts.iter().map(|a| a.id.clone()).collect();
                self.hidden_alerts.retain(|id| current_ids.contains(id));
                let items = alerts
                    .into_iter()
                    .map(|alert| Arc::new(AlertItem { alert }))
                    .collect();
                self.state.set_data(items);
            }
            Err(e) => {
                log::error!("NWS alerts fetch failed: {}", e);
            }
        }
        self.state.fetching = false;
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>) {
        selections.retain(|sel| {
            if sel.kind() != OverlayKind::NwsAlerts {
                return true;
            }
            self.state.data.iter().any(|item| item.matches(sel.as_ref()))
        });
    }

    fn prepare_rasterize(
        &self,
        _ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>> {
        if self.state.data.is_empty() {
            return None;
        }
        let alerts: Vec<NwsAlert> = self.state.data.iter().map(|i| i.alert.clone()).collect();
        let enabled_categories: Vec<AlertCategory> = self.enabled_categories.iter().copied().collect();
        let hidden_alerts = self.hidden_alerts.clone();
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            let rgba = rasterize::rasterize_nws_alerts(
                &alerts,
                &enabled_categories,
                &hidden_alerts,
                bounds,
                width,
                height,
            );
            RasterizeOutput { rgba, hit_map: None }
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

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let mut items = vec![
            ControlItem::Heading { text: "\u{26a0}  NWS Alerts".into() },
            ControlItem::Toggle {
                id: "warnings",
                label: "\u{26a0}  Warnings".into(),
                enabled: self.enabled_categories.contains(&AlertCategory::Warning),
            },
            ControlItem::Toggle {
                id: "watches",
                label: "Watches".into(),
                enabled: self.enabled_categories.contains(&AlertCategory::Watch),
            },
            ControlItem::Toggle {
                id: "advisories",
                label: "Advisories".into(),
                enabled: self.enabled_categories.contains(&AlertCategory::Advisory),
            },
        ];

        if self.is_enabled() {
            items.push(ControlItem::ButtonRow {
                buttons: vec![ControlButton {
                    id: "refresh",
                    label: "\u{1f504} Refresh".into(),
                    enabled: !self.state.fetching,
                    highlight: false,
                }],
            });
            if self.state.fetching {
                items.push(ControlItem::InfoText { text: "Fetching\u{2026}".into() });
            }
            if self.has_data() {
                let visible = self.clickable_items().len();
                items.push(ControlItem::InfoText { text: format!("{visible} alerts shown") });
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

    fn apply_control(&mut self, update: &ControlUpdate, _ctx: &mut PaneControlContextMut<'_>) -> ControlEffect {
        match update.id {
            "warnings" | "watches" | "advisories" => {
                let category = match update.id {
                    "warnings" => AlertCategory::Warning,
                    "watches" => AlertCategory::Watch,
                    "advisories" => AlertCategory::Advisory,
                    _ => return ControlEffect::None,
                };
                if let ControlValue::Bool(enabled) = update.value {
                    let was_enabled = self.is_enabled();
                    if enabled {
                        self.enabled_categories.insert(category);
                    } else {
                        self.enabled_categories.remove(&category);
                    }
                    self.state.data_generation = self.state.data_generation.wrapping_add(1);
                    if !was_enabled && self.is_enabled() && !self.has_data() && !self.state.fetching {
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
        serde_json::json!({
            "enabled_categories": self.enabled_categories,
        })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(cats) = value.get("enabled_categories").and_then(|v| serde_json::from_value(v.clone()).ok()) {
            self.enabled_categories = cats;
        }
    }
}
