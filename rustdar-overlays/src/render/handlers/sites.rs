use std::any::Any;
use std::sync::Arc;

use crate::render::controls::{ControlEffect, ControlItem, ControlUpdate, PaneControlContext, PaneControlContextMut};
use crate::render::overlay_state::{OverlayHandler, OverlayItem, OverlayKind, RenderMode};

/// Handler for NEXRAD radar site markers.
///
/// Manages toggle state. Rasterization and per-frame interaction (text labels,
/// site clicking) are handled in `rustdar-egui` via the texture + interactions
/// code path in the draw loop.
pub(crate) struct RadarSitesHandler {
    pub enabled: bool,
}

impl RadarSitesHandler {
    pub fn new() -> Self {
        Self { enabled: false }
    }
}

impl OverlayHandler for RadarSitesHandler {
    fn kind(&self) -> OverlayKind { OverlayKind::RadarSites }
    fn display_name(&self) -> &str { "Radar Sites" }
    fn render_mode(&self) -> RenderMode { RenderMode::Texture }
    fn default_enabled(&self) -> bool { false }
    fn is_enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, enabled: bool) { self.enabled = enabled; }

    fn data_generation(&self) -> u64 { 0 }
    fn has_data(&self) -> bool { true }
    fn is_fetching(&self) -> bool { false }
    fn set_fetching(&mut self, _fetching: bool) {}
    fn fetch_time(&self) -> Option<std::time::Instant> { None }

    fn apply_fetch_result(&mut self, _result: Box<dyn Any + Send>) {}
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>) {}

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        vec![
            ControlItem::Toggle {
                id: "enabled",
                label: "\u{1f4e1}  Radar Sites".to_string(),
                enabled: self.enabled,
            },
        ]
    }

    fn apply_control(&mut self, update: &ControlUpdate, _ctx: &mut PaneControlContextMut<'_>) -> ControlEffect {
        if update.id == "enabled"
            && let crate::render::controls::ControlValue::Bool(val) = update.value {
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
