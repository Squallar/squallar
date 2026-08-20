//! The radar layer as a source — its registration, its toggle and its saved
//! state, and nothing else.
//!
//! Moved here from `rustdar_overlays::render::handlers::radar` at WO-M9. It is
//! metadata only: radar fetching, decoding, rendering and every per-frame
//! decoration live where they have always lived (this crate's pipeline, plus
//! `rustdar-egui` for the draw). What moved is the ~90 lines that say *this is
//! a layer* — so that the layer stack, the catalogue and the config file learn
//! about radar from the crate that owns radar data, exactly as they learn about
//! lightning from the crate that owns lightning data.
//!
//! Nothing in the compute path moved with it, and the land that moved it gated
//! on exactly that: the radar suite ran unedited, and this file plus `lib.rs`
//! were the whole of the crate's diff.

use std::sync::Arc;

use rustdar_source::controls::{
    ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use rustdar_source::handler::{FetchPayload, OverlayItem, RenderMode, SourceHandler, Surface};
use rustdar_source::id::{LayerId, known};

/// **The radar layer's registration — one row, and the only one this crate
/// has.**
///
/// The app's whole layer set is `rustdar_egui::sources::all`, which chains this
/// with `rustdar_overlays::render::handlers::sources`. Neither source crate can
/// see the other; the composition is what sees both.
pub fn sources() -> Vec<Box<dyn SourceHandler>> {
    vec![Box::new(RadarSource::new())]
}

/// Toggle and config state only. Radar fetching, rendering and per-frame
/// decorations live in this crate's pipeline and in `rustdar-egui`; this exists
/// so radar's toggle lives with every other layer's.
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

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        vec![ControlItem::Toggle {
            id: "enabled",
            label: "Radar".to_string(),
            enabled: self.enabled,
        }]
    }

    fn apply_control(
        &mut self,
        update: &ControlUpdate,
        _ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
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
