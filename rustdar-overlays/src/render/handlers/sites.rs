use std::sync::Arc;

use rustdar_source::job::JobCodec;

use crate::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, PaneControlContext, PaneControlContextMut,
};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{FetchPayload, OverlayHandler, OverlayItem, RenderMode};
use rustdar_source::id::{LayerId, known};

/// Toggle state only. Rasterization and per-frame interaction (text labels,
/// site clicking) happen in `rustdar-egui`.
pub(crate) struct RadarSitesHandler {
    pub enabled: bool,
}

impl RadarSitesHandler {
    pub fn new() -> Self {
        Self { enabled: false }
    }
}

impl OverlayHandler for RadarSitesHandler {
    fn id(&self) -> LayerId {
        known::RADAR_SITES
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        100
    }
    fn display_name(&self) -> &str {
        "Radar Sites"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }
    fn default_enabled(&self) -> bool {
        false
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

    /// The sites row is registered like every other texture kind's, while
    /// `prepare_job` stays the default `None`: the described input needs
    /// `pane.site`/`loading_site`, which this handler cannot see until
    /// per-pane handler state exists (M10), so the frontend dispatch builds
    /// the `SitesInput` itself and frames it with this row.
    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/sites")
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
            label: "Radar Sites".to_string(),
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
