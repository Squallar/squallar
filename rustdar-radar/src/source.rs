//! The radar layer as a source — its registration, its toggle and its saved
//! state, and nothing else.

use rustdar_source::handler::{PaneMut, PaneRef, PaneToggle};
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
    /// Discrete stamped volumes, never ahead of the wall clock. The nominal
    /// step is the WSR-88D precipitation cadence; the measured truth for a
    /// window is the loop's own `cadence_secs`, which this never overrides.
    fn time_axis(&self) -> rustdar_source::time::TimeAxis {
        rustdar_source::time::TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(300),
            extends_future: false,
        }
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

    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        vec![ControlItem::Toggle {
            id: "enabled",
            label: "Radar".to_string(),
            enabled: self.is_enabled(pane),
        }]
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        if update.id == "enabled"
            && let ControlValue::Bool(val) = update.value
            && !PaneToggle::set(pane, val)
        {
            self.enabled = val;
        }
        ControlEffect::None
    }

    // ── Per-pane state (WO-M10b) ──────────────────────────────────────
    //
    // This layer's only per-pane fact is whether the pane draws it, so its
    // state IS the toggle. `self.enabled` survives as the LAYER'S DEFAULT for
    // a caller that supplies no pane; nothing reads it into a pane, and the
    // global `serialize_state` no longer carries it — the pane's slot does.

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
}
