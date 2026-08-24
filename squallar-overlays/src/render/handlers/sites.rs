use crate::render::overlay_state::{PaneMut, PaneRef, PaneToggle};
use std::sync::Arc;

use squallar_source::job::{DescribedJob, JobCodec};

use crate::fetch_policy::Whole;
use crate::render::controls::{ControlEffect, ControlItem, ControlUpdate};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    FetchPayload, OverlayHandler, OverlayItem, OverlayState, RasterizeContext, RenderMode,
};
use crate::render::rasterize;
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
            state: OverlayState::new(),
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
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    /// `is_dark` rides into the described job (`SitesInput`) and picks the
    /// label plate colour (`text_bg`), so a cached raster is a raster in one theme.
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

    /// **The pane's own site, read from the radar slot beside it.** This is
    /// the whole reason WO-M6 deferred this layer and WO-M10c closes it: the
    /// input used to be built inline in `app_fetch` because the handler could
    /// not see which site this pane is on, or which one it is loading.
    fn prepare_job(&self, ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        if self.state.data.is_empty() {
            return None;
        }
        let current = pane
            .sibling(&known::RADAR)
            .and_then(|config| config.get("site"))
            .and_then(serde_json::Value::as_str);
        let loading = pane.loading_site;
        Some(DescribedJob::new(rasterize::SitesInput {
            sites: self
                .state
                .data
                .iter()
                .map(|site| rasterize::RadarSiteInfo {
                    name: site.name.clone(),
                    lat: site.lat,
                    lon: site.lon,
                    is_current: current == Some(site.name.as_str()),
                    is_loading: loading == Some(site.name.as_str()),
                })
                .collect(),
            zoom: ctx.zoom,
            is_dark: ctx.is_dark,
            device_scale: ctx.device_scale,
        }))
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/sites")
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }

    /// The site table, installed by the frontend that owns it.
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
