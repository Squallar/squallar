use crate::render::overlay_state::{PaneMut, PaneRef, PaneToggle};
use std::sync::Arc;

use crate::render::controls::{ControlEffect, ControlItem, ControlUpdate};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{FetchPayload, OverlayHandler, OverlayItem, RenderMode};
use rustdar_source::id::{LayerId, known};

/// Toggle state only: the legend is a screen-space HUD element, not
/// geo-projected, so the draw loop renders it directly.
pub(crate) struct ColorScaleHandler {
    pub enabled: bool,
}

impl ColorScaleHandler {
    pub fn new() -> Self {
        Self { enabled: true }
    }
}

impl OverlayHandler for ColorScaleHandler {
    fn id(&self) -> LayerId {
        known::COLOR_SCALE
    }
    fn surface(&self) -> Surface {
        Surface::Glass
    }
    fn draw_order_weight(&self) -> u32 {
        120
    }
    fn display_name(&self) -> &str {
        "Color Scale"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::PerFrameDirect
    }
    fn default_enabled(&self) -> bool {
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

    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }

    fn apply_fetch_result(&mut self, _result: FetchPayload) {}
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        vec![ControlItem::Toggle {
            id: "enabled",
            label: "Color Scale".to_string(),
            enabled: self.is_enabled(pane),
        }]
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        if update.id == "enabled"
            && let crate::render::controls::ControlValue::Bool(val) = update.value
            && !PaneToggle::set(pane, val)
        {
            self.enabled = val;
        }
        ControlEffect::None
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

    fn serialize_state(&self) -> serde_json::Value {
        serde_json::json!({ "enabled": self.enabled })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(enabled) = value.get("enabled").and_then(|v| v.as_bool()) {
            self.enabled = enabled;
        }
    }
}
