use crate::render::overlay_state::{PaneMut, PaneRef, PaneToggle};
use std::sync::Arc;

use squallar_source::job::{DescribedJob, JobCodec};

use crate::fetch_policy::Whole;
use crate::render::controls::{ControlEffect, ControlItem, ControlUpdate};
use crate::render::handlers::sites::{RadarSitesFetchResult, SiteRow};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    FetchPayload, OverlayHandler, OverlayItem, OverlayState, RasterizeContext, RenderMode,
};
use crate::render::rasterize;
use squallar_source::id::{LayerId, known};
use squallar_source::time::TimeAxis;

/// **Where the radar network can see, as ground.**
///
/// The ground half of what used to be one `RadarSites` layer. The markers, the
/// station names and the selected station's ring are lengths in points and are
/// painted per frame; a 230 km coverage radius is 230 km whatever the zoom, so
/// it belongs in a raster placed by its geographic corners, and that is this.
///
/// **Off by default, and that is the whole lesson of the split.** Every station
/// drawing its own outline at continental zoom overlapped into a mesh that hid
/// the map under it. Drawn as a filled wash the same geometry answers the
/// question it was always for — "is this storm inside anybody's coverage" —
/// because overlapping circles filled under a single non-zero winding rule
/// merge into one region instead of stacking into 160 edges.
pub(crate) struct RadarCoverageHandler {
    pub state: OverlayState<Vec<SiteRow>, Whole>,
    /// **The layer's own default**, for a caller that supplied no pane.
    /// Nothing reads it into a pane.
    pub enabled: bool,
    /// The last built paint input per (generation, device scale) — see
    /// [`Self::prepare_job`].
    pub(crate) job_memo: crate::render::signature_memo::JobMemo,
}

impl RadarCoverageHandler {
    pub fn new() -> Self {
        Self {
            // Parked, because this handler implements `take_retired`:
            // the two are set together, so a park always has a drain.
            state: OverlayState::parked(),
            enabled: false,
            job_memo: crate::render::signature_memo::JobMemo::new(
                crate::render::footprint::coverage_job,
            ),
        }
    }
}

impl OverlayHandler for RadarCoverageHandler {
    fn id(&self) -> LayerId {
        known::RADAR_COVERAGE
    }

    /// Fixed installations. The list changes on the scale of decommissionings,
    /// not of anything a pane's timeline reaches.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::Live
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    /// Under the site markers, which sit at 100. The wash is context for the
    /// dots, so the dots draw over it.
    fn draw_order_weight(&self) -> u32 {
        95
    }
    fn display_name(&self) -> &str {
        "Radar Coverage"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    /// **False, and measurably so**: the wash is one ink, mixed from the
    /// station colour and an alpha, and nothing in this raster is drawn in a
    /// theme colour. Answering `true` would re-rasterize the whole network
    /// every time the theme flipped for no change in a single texel.
    fn theme_sensitive(&self) -> bool {
        false
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

    /// **No network, no clock, no weather.** The site table is compiled into
    /// the binary and pushed through the ordinary arrival door at boot, so this
    /// answers a job on the first frame of a cold start on a machine with no
    /// connection at all. Nothing in the input is read from the pane: the wash
    /// is the whole network, so it does not depend on which station the pane is
    /// on, and two panes at the same viewport share a raster.
    ///
    /// Built once per (generation, device scale): the table is installed at
    /// boot and never polled, so after the first dispatch every ask is a
    /// refcount clone.
    fn prepare_job(&self, ctx: &RasterizeContext, _pane: &PaneRef<'_>) -> Option<DescribedJob> {
        if self.state.data.is_empty() {
            return None;
        }
        self.job_memo.get_or_build(
            self.state.data_generation,
            u64::from(ctx.device_scale.to_bits()),
            || {
                Some(DescribedJob::new(rasterize::CoverageInput {
                    sites: self
                        .state
                        .data
                        .iter()
                        .map(|site| rasterize::CoverageSite {
                            lat: site.lat,
                            lon: site.lon,
                        })
                        .collect(),
                    device_scale: ctx.device_scale,
                }))
            },
        )
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/coverage")
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }

    /// The same table `RadarSites` is handed, through the same door and by the
    /// same publisher — one arrival, two layers, so the two can never disagree
    /// about which stations exist.
    /// The generation this layer's state parked and the inputs its memo
    /// retired, handed back for the app to free off the frame thread — see
    /// [`OverlayHandler::take_retired`].
    fn take_retired(&self) -> Vec<Box<dyn std::any::Any + Send>> {
        crate::render::overlay_state::retired_batch(
            self.state.take_retired(),
            self.job_memo.take_retired(),
        )
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, _pane: &PaneRef<'_>) {
        let Some(rows) = self.state.downcast_round::<RadarSitesFetchResult>(result) else {
            log::error!("radar coverage handler received unexpected fetch result type");
            return;
        };
        self.state.set_data(rows.0);
    }
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        vec![ControlItem::Toggle {
            id: "enabled",
            label: "Radar Coverage".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn handler_with_sites(n: usize) -> RadarCoverageHandler {
        let mut handler = RadarCoverageHandler::new();
        handler.apply_fetch_result(
            Box::new(RadarSitesFetchResult(
                (0..n)
                    .map(|i| SiteRow {
                        name: format!("K{i:03}"),
                        lat: 30.0 + i as f64 * 0.1,
                        lon: -100.0 + i as f64 * 0.1,
                    })
                    .collect(),
            )),
            &PaneRef::across(&[]),
        );
        handler
    }

    fn ctx(device_scale: f32) -> RasterizeContext {
        let clock = chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
            .unwrap()
            .and_hms_opt(19, 0, 0)
            .unwrap();
        RasterizeContext {
            is_dark: false,
            zoom: 5.0,
            device_scale,
            now: clock,
            as_of: clock,
            frame: None,
        }
    }

    /// The site table never polls, so after the first dispatch every ask at
    /// the same device scale is a refcount clone of one built input.
    #[test]
    fn the_built_input_is_reused_until_the_device_scale_moves() {
        let handler = handler_with_sites(160);
        let pane = PaneRef::bare(0);
        let first = handler.prepare_job(&ctx(1.0), &pane).unwrap();
        let second = handler.prepare_job(&ctx(1.0), &pane).unwrap();
        assert!(Arc::ptr_eq(&first.0, &second.0));
        assert_eq!(handler.job_memo.builds.get(), 1);
        assert_eq!(
            first
                .downcast_ref::<rasterize::CoverageInput>()
                .unwrap()
                .sites
                .len(),
            160,
        );

        handler.prepare_job(&ctx(2.0), &pane);
        assert_eq!(handler.job_memo.builds.get(), 2, "device scale is a term");
    }

    #[test]
    fn an_empty_table_describes_no_job_and_holds_nothing() {
        let handler = RadarCoverageHandler::new();
        assert!(handler.prepare_job(&ctx(1.0), &PaneRef::bare(0)).is_none());
        assert_eq!(handler.job_memo.builds.get(), 0);
    }
}
