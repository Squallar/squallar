use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use crate::nws::alert::{AlertCategory, NwsAlert};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupAction, PopupActionKind, PopupContent, PopupSection, RasterizeContext,
    RasterizeFn, RenderMode,
};
use crate::render::rasterize::{self, RasterizeOutput};
use crate::types::GeoBounds;

pub(crate) struct NwsAlertFetchResult(pub Result<Vec<NwsAlert>, String>);

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
        let [r, g, b, _] = alert
            .features
            .first()
            .map(|f| f.stroke_rgba)
            .unwrap_or([200, 200, 200, 255]);

        let mut sections = Vec::new();

        if let Some(headline) = &alert.headline {
            sections.push(PopupSection::Heading(headline.clone()));
        }

        sections.push(PopupSection::KeyValueGrid(vec![
            ("Areas".into(), alert.area_desc.clone()),
            ("Issued by".into(), alert.sender_name.clone()),
            (
                "Effective".into(),
                prefs.timezone.format_rfc3339(&alert.effective),
            ),
            (
                "Expires".into(),
                prefs.timezone.format_rfc3339(&alert.expires),
            ),
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
            actions: vec![PopupAction {
                label: "Hide from map".into(),
                target: Arc::new(AlertItem {
                    alert: alert.clone(),
                }),
                kind: PopupActionKind::HideFromMap,
            }],
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
    /// User-dismissed alert IDs. Pruned on refetch so an ID reused upstream
    /// does not stay hidden forever.
    pub hidden_alerts: HashSet<String>,
    /// Empty means the whole overlay is off — see `is_enabled`.
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

    /// The **warning-set** signature: a fold over the ids of the alerts that
    /// would draw — category-enabled and not hidden — rather than the fetch
    /// counter. NWS alerts auto-poll every two minutes and the active set is
    /// usually unchanged, so a consumer keyed on [`data_generation`] would
    /// re-render on every poll for nothing; this token moves exactly when
    /// the drawn set moves (a warning issued, expired, hidden, or a whole
    /// category toggled off). XOR of per-id hashes, so it is order-free the
    /// way a set is; ids are unique within a response, and an alert whose
    /// geometry changes upstream arrives under a fresh id.
    ///
    /// [`data_generation`]: OverlayHandler::data_generation
    fn content_signature(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut folded = 0u64;
        let mut visible = 0u64;
        for item in &self.state.data {
            if self.enabled_categories.contains(&item.alert.category)
                && !self.hidden_alerts.contains(&item.alert.id)
            {
                let mut hasher = DefaultHasher::new();
                item.alert.id.hash(&mut hasher);
                folded ^= hasher.finish();
                visible += 1;
            }
        }
        folded ^ visible.rotate_left(32)
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

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(120)
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn clickable_items(&self) -> Vec<ClickableItem> {
        self.state
            .data
            .iter()
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

    fn apply_fetch_result(&mut self, result: FetchPayload) {
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
            self.state
                .data
                .iter()
                .any(|item| item.matches(sel.as_ref()))
        });
    }

    fn prepare_rasterize(&self, _ctx: &RasterizeContext) -> Option<RasterizeFn> {
        if self.state.data.is_empty() {
            return None;
        }
        let alerts: Vec<NwsAlert> = self.state.data.iter().map(|i| i.alert.clone()).collect();
        let enabled_categories: Vec<AlertCategory> =
            self.enabled_categories.iter().copied().collect();
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
            RasterizeOutput {
                rgba,
                hit_map: None,
            }
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        log::info!("Fetching NWS active alerts");
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let zone_cache = ctx.zone_cache_dir.clone();
        vec![FetchTask {
            kind: OverlayKind::NwsAlerts,
            future: Box::pin(async move {
                let result = crate::nws::fetch::fetch_active_alerts(
                    &client,
                    &sources,
                    zone_cache.as_deref(),
                )
                .await
                .map_err(|e| e.to_string());
                Box::new(NwsAlertFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let mut items = vec![
            ControlItem::Heading {
                text: "\u{26a0}  NWS Alerts".into(),
            },
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
                items.push(ControlItem::InfoText {
                    text: "Fetching\u{2026}".into(),
                });
            }
            if self.has_data() {
                let visible = self.clickable_items().len();
                items.push(ControlItem::InfoText {
                    text: format!("{visible} alerts shown"),
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
                    if !was_enabled && self.is_enabled() && !self.has_data() && !self.state.fetching
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

    fn serialize_state(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled_categories": self.enabled_categories,
        })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(cats) = value
            .get("enabled_categories")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            self.enabled_categories = cats;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{HatchPattern, OverlayFeature};

    /// A minimal alert with geometry, identified by `id`.
    fn alert(id: &str, event: &str) -> NwsAlert {
        let polygons = vec![vec![vec![(35.0, -97.0), (35.5, -97.0), (35.5, -96.5)]]];
        let (fill, stroke) = crate::nws::colors::alert_color(event);
        NwsAlert {
            id: id.to_string(),
            event: event.to_string(),
            category: AlertCategory::from_event(event),
            severity: "Severe".parse().unwrap(),
            urgency: "Immediate".parse().unwrap(),
            certainty: "Observed".parse().unwrap(),
            headline: None,
            description: String::new(),
            instruction: None,
            area_desc: String::new(),
            sender_name: String::new(),
            effective: String::new(),
            expires: String::new(),
            onset: None,
            ends: None,
            affected_zones: Vec::new(),
            features: vec![OverlayFeature::new(
                polygons,
                fill,
                stroke,
                event.to_string(),
                String::new(),
                HatchPattern::None,
            )],
        }
    }

    fn handler_with(alerts: Vec<NwsAlert>) -> NwsAlertHandler {
        let mut handler = NwsAlertHandler::new();
        handler.apply_fetch_result(Box::new(NwsAlertFetchResult(Ok(alerts))));
        handler
    }

    /// The signature names the **set**, not the fetch: a refetch returning
    /// the same warning ids must keep it, which is exactly what
    /// `data_generation` — bumped on every `set_data` — cannot do. This is
    /// the mutation the method exists to be different from: swap the body
    /// for `self.data_generation()` and this test fails on the second
    /// fetch.
    #[test]
    fn a_refetch_of_the_same_warning_set_keeps_the_signature() {
        let mut handler = handler_with(vec![alert("a", "Tornado Warning")]);
        let first = handler.content_signature();
        let generation_before = handler.data_generation();
        handler.apply_fetch_result(Box::new(NwsAlertFetchResult(Ok(vec![alert(
            "a",
            "Tornado Warning",
        )]))));
        assert_ne!(
            handler.data_generation(),
            generation_before,
            "fixture: the refetch really did bump the generation",
        );
        assert_eq!(
            handler.content_signature(),
            first,
            "an unchanged warning set across a poll must keep its signature",
        );
    }

    /// Every way the drawn set can change moves the signature: a new
    /// warning, an expiry, a hide, a category turned off.
    #[test]
    fn every_change_to_the_drawn_set_moves_the_signature() {
        let mut handler = handler_with(vec![alert("a", "Tornado Warning")]);
        let one_warning = handler.content_signature();

        // A second warning issues mid-session.
        handler.apply_fetch_result(Box::new(NwsAlertFetchResult(Ok(vec![
            alert("a", "Tornado Warning"),
            alert("b", "Severe Thunderstorm Warning"),
        ]))));
        let two_warnings = handler.content_signature();
        assert_ne!(two_warnings, one_warning, "a new warning must move it");

        // The first expires out of the feed.
        handler.apply_fetch_result(Box::new(NwsAlertFetchResult(Ok(vec![alert(
            "b",
            "Severe Thunderstorm Warning",
        )]))));
        let b_only = handler.content_signature();
        assert_ne!(b_only, two_warnings, "an expiry must move it");
        assert_ne!(
            b_only, one_warning,
            "a different single warning is a different set",
        );

        // The user hides the survivor.
        handler.hidden_alerts.insert("b".to_string());
        assert_ne!(
            handler.content_signature(),
            b_only,
            "hiding an alert must move it",
        );
        handler.hidden_alerts.clear();

        // The whole category goes off.
        handler.enabled_categories.remove(&AlertCategory::Warning);
        assert_ne!(
            handler.content_signature(),
            b_only,
            "disabling the category must move it",
        );
    }

    /// The fold is order-free: the same set in another order is the same
    /// signature — feed order is not part of what gets drawn.
    #[test]
    fn the_signature_is_a_set_signature_not_a_sequence_signature() {
        let forward = handler_with(vec![
            alert("a", "Tornado Warning"),
            alert("b", "Flash Flood Warning"),
        ]);
        let backward = handler_with(vec![
            alert("b", "Flash Flood Warning"),
            alert("a", "Tornado Warning"),
        ]);
        assert_eq!(forward.content_signature(), backward.content_signature());
    }
}
