use std::sync::Arc;

use crate::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, PaneControlContext, PaneControlContextMut,
};
use crate::render::overlay_state::{
    FetchPayload, OverlayHandler, OverlayItem, OverlayKind, RenderMode,
};

/// Toggle state only: the tiles are rendered by the walkers integration in
/// `rustdar-egui`.
pub(crate) struct CityLabelsHandler {
    pub enabled: bool,
}

impl CityLabelsHandler {
    pub fn new() -> Self {
        Self { enabled: true }
    }
}

impl OverlayHandler for CityLabelsHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::CityLabels
    }
    fn display_name(&self) -> &str {
        "City Labels"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Tile
    }
    fn default_enabled(&self) -> bool {
        true
    }
    fn is_enabled(&self) -> bool {
        self.enabled
    }
    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }

    fn apply_fetch_result(&mut self, _result: FetchPayload) {}
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>) {}

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        vec![ControlItem::Toggle {
            id: "enabled",
            label: "City Labels".to_string(),
            enabled: self.enabled,
        }]
    }

    fn apply_control(
        &mut self,
        update: &ControlUpdate,
        _ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
        if update.id == "enabled"
            && let crate::render::controls::ControlValue::Bool(val) = update.value
        {
            self.enabled = val;
        }
        ControlEffect::None
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
