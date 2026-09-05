use crate::render::overlay_state::{PaneMut, PaneRef, PaneToggle};
use std::sync::Arc;

use squallar_source::job::{DescribedJob, JobCodec};

use crate::fetch_policy::Whole;
use crate::render::controls::{ControlEffect, ControlItem, ControlUpdate};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    FetchPayload, OverlayHandler, OverlayItem, OverlayState, RasterizeContext, RenderMode,
};
use squallar_source::id::{LayerId, known};
use squallar_source::time::TimeAxis;

/// **One radar site, as this crate is allowed to know it.** Name and position
/// and nothing else: the site table lives in `squallar-radar`, which this crate
/// must not name (WO-M3's edge cut), so the frontend that owns the table
/// installs the rows through [`RadarSitesFetchResult`].
#[derive(Debug, Clone, PartialEq)]
pub struct SiteRow {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// The site table arriving from the frontend. Not a *fetch* — this layer never
/// builds a task — but the same door, so the rows land where every other
/// layer's data lands.
pub struct RadarSitesFetchResult(pub Vec<SiteRow>);

impl crate::fetch_policy::FetchRound for RadarSitesFetchResult {
    type Shape = Whole;
}

/// The site table, plus which pane draws it. Per-frame interaction (text
/// labels, site clicking) still happens in `squallar-egui`.
pub(crate) struct RadarSitesHandler {
    pub state: OverlayState<Vec<SiteRow>, Whole>,
    /// **The layer's own default**, for a caller that supplied no pane.
    /// Nothing reads it into a pane.
    pub enabled: bool,
}

impl RadarSitesHandler {
    pub fn new() -> Self {
        Self {
            // Parked, because this handler implements `take_retired`:
            // the two are set together, so a park always has a drain.
            state: OverlayState::parked(),
            enabled: false,
        }
    }
}

impl OverlayHandler for RadarSitesHandler {
    fn id(&self) -> LayerId {
        known::RADAR_SITES
    }

    /// The radar network's sites are fixed installations. The list changes on
    /// the scale of decommissionings, not of anything a pane's timeline reaches.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::Live
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
    /// **Nothing this layer draws is ground any more.**
    ///
    /// The marker and the label plate went to the per-frame painter when a
    /// baked marker turned out to be stretched by every zoom gesture, and the
    /// coverage ring followed them when it stopped being drawn for all 160
    /// stations at once. What is left is a dot, a name and the selected
    /// station's ring — every one of them a length in points on the display,
    /// and the ring is selection feedback besides, which a whole-picture raster
    /// round trip would deliver a beat after the dot it belongs to.
    ///
    /// The network-wide coverage that *is* ground kept the raster and became
    /// [`known::RADAR_COVERAGE`].
    fn render_mode(&self) -> RenderMode {
        RenderMode::PerFrameDirect
    }

    /// The plate under a station's name is drawn in the theme's colours, and
    /// this layer is the plate's owner even though the painter that draws it
    /// lives in `squallar-egui`.
    fn theme_sensitive(&self) -> bool {
        true
    }
    fn default_enabled(&self) -> bool {
        false
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
        self.state.data_generation
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        !self.state.data.is_empty()
    }

    /// **None, because this layer no longer rasterizes anything.** A
    /// `PerFrameDirect` layer is never dispatched through the job funnel; the
    /// site table it holds is read straight off the handler by the per-frame
    /// painter.
    fn prepare_job(&self, _ctx: &RasterizeContext, _pane: &PaneRef<'_>) -> Option<DescribedJob> {
        None
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        None
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }

    /// The site table, installed by the frontend that owns it.
    /// The generation this layer's state parked, handed back for the app to
    /// free off the frame thread — see [`OverlayHandler::take_retired`].
    fn take_retired(&self) -> Vec<Box<dyn std::any::Any + Send>> {
        self.state.take_retired().into_iter().collect()
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, _pane: &PaneRef<'_>) {
        let Some(rows) = self.state.downcast_round::<RadarSitesFetchResult>(result) else {
            log::error!("radar sites handler received unexpected fetch result type");
            return;
        };
        self.state.set_data(rows.0);
    }
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        vec![ControlItem::Toggle {
            id: "enabled",
            label: "Radar Sites".to_string(),
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
}
