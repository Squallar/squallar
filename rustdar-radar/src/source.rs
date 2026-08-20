//! The radar layer as a source — its registration, its toggle and its saved
//! state, and nothing else.

use rustdar_source::handler::{PaneMut, PaneRef};
use std::sync::Arc;

use rustdar_source::controls::{ControlEffect, ControlItem, ControlUpdate, ControlValue};
use rustdar_source::handler::{FetchPayload, OverlayItem, RenderMode, SourceHandler, Surface};
use rustdar_source::id::{LayerId, known};

/// **The radar layer's registration — one row, and the only one this crate
/// has.**
pub fn sources() -> Vec<Box<dyn SourceHandler>> {
    vec![Box::new(RadarSource::new())]
}

/// Toggle and config state only. Radar fetching, rendering and per-frame
pub struct RadarSource {
    pub enabled: bool,
}

impl RadarSource {
    pub fn new() -> Self {
        Self { enabled: true }
    }
}

impl Default for RadarSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceHandler for RadarSource {
    fn id(&self) -> LayerId {
        known::RADAR
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        30
    }
    fn display_name(&self) -> &str {
        "Radar"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
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

    fn controls(&self, _ctx: &PaneRef<'_>) -> Vec<ControlItem> {
        vec![ControlItem::Toggle {
            id: "enabled",
            label: "Radar".to_string(),
            enabled: self.enabled,
        }]
    }

    fn apply_control(&mut self, update: &ControlUpdate, _ctx: &mut PaneMut<'_>) -> ControlEffect {
        if update.id == "enabled"
            && let ControlValue::Bool(val) = update.value
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
