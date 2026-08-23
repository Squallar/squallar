//! **A loop's range is the transport layer's shape, not radar's.**
//!
//! A layer that declares `extends_future` gets a range anchored on the wall
//! clock and reaching forward to its own horizon; a layer whose stamps are all
//! history keeps the scan-anchored, backward-only range it has always had.
//! Both arms are observed at the one place the range leaves the shell — the
//! `create_frame_list_task` call — so neither can be satisfied by a pane that
//! merely happens to hold the other's data.

use super::*;
use crate::app::App;
use rustdar_source::handler::{FetchTask, FrameListingResult, PaneRef};
use rustdar_source::id::{LayerId, known};
use rustdar_source::time::{FrameListing, FrameSource, FrameStamp, TimeAxis};
use std::sync::{Arc, Mutex};

/// The ranges a layer was asked to list, captured **synchronously inside**
/// `create_frame_list_task` — before the future is spawned, so the assertion
/// does not depend on an executor running.
type Ranges = Arc<Mutex<Vec<(NaiveDateTime, NaiveDateTime)>>>;

/// A handler that answers a chosen time axis and horizon and records every
/// range it is asked to list.
struct RangeRecorder {
    id: LayerId,
    extends_future: bool,
    horizon: chrono::Duration,
    seen: Ranges,
}

impl FrameSource for RangeRecorder {
    /// This double answers no stamps at all: it exists to record the ranges
    /// the shell asks it to list, and every one of the six methods below that
    /// would name a frame is deliberately empty so a range that arrives can
    /// only have come from the shell's own arithmetic.
    fn latest_at(&self, _pane: &PaneRef<'_>, _t: NaiveDateTime) -> Option<FrameStamp> {
        None
    }

    fn list_frames(
        &self,
        _ctx: &rustdar_overlays::render::overlay_state::FetchConfig,
        _pane: &PaneRef<'_>,
        range: (NaiveDateTime, NaiveDateTime),
    ) -> FrameListing {
        FrameListing::empty(range)
    }

    fn fetch_frame(
        &self,
        _ctx: &rustdar_overlays::render::overlay_state::FetchConfig,
        _pane: &PaneRef<'_>,
        _stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        None
    }

    fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        Vec::new()
    }

    fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}

    fn apply_frame_listing(
        &mut self,
        _listing: FrameListing,
        _scope: rustdar_overlays::render::overlay_state::FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn apply_frame(
        &mut self,
        _stamp: FrameStamp,
        _data: rustdar_overlays::render::overlay_state::FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
        self.horizon
    }

    fn create_frame_list_task(
        &self,
        _ctx: &rustdar_overlays::render::overlay_state::FetchConfig,
        _pane: &PaneRef<'_>,
        range: (NaiveDateTime, NaiveDateTime),
    ) -> Option<FetchTask> {
        self.seen.lock().expect("no poisoned lock").push(range);
        let id = self.id.clone();
        Some(FrameListingResult::task(id, async move {
            FrameListingResult {
                listing: FrameListing {
                    range,
                    frames: Vec::new(),
                    complete: true,
                },
                scope: Box::new(()),
            }
        }))
    }
}

impl rustdar_overlays::render::overlay_state::OverlayHandler for RangeRecorder {
    fn id(&self) -> LayerId {
        self.id.clone()
    }
    fn surface(&self) -> rustdar_overlays::render::overlay_state::Surface {
        rustdar_overlays::render::overlay_state::Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        999
    }
    fn display_name(&self) -> &str {
        "Range Recorder"
    }
    fn render_mode(&self) -> rustdar_overlays::render::overlay_state::RenderMode {
        rustdar_overlays::render::overlay_state::RenderMode::Texture
    }
    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        false
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _f: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(
        &mut self,
        _result: rustdar_overlays::render::overlay_state::FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }
    fn retain_selections(
        &self,
        _selections: &mut Vec<
            std::sync::Arc<dyn rustdar_overlays::render::overlay_state::OverlayItem>,
        >,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn time_axis(&self) -> TimeAxis {
        TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: self.extends_future,
        }
    }

    /// This layer comes in stamped frames, and answers every one of
    /// [`FrameSource`]'s methods below.
    fn frames(&self) -> Option<&dyn FrameSource> {
        Some(self)
    }

    fn frames_mut(&mut self) -> Option<&mut dyn FrameSource> {
        Some(self)
    }
}

const LOOKBACK: u64 = 600;

/// A one-pane app whose only registered layer is a recorder under `id`.
fn app_with(id: &LayerId, extends_future: bool, horizon: chrono::Duration) -> (App, Ranges) {
    use rustdar_overlays::render::overlay_state::OverlayRegistry;

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let seen: Ranges = Default::default();
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(RangeRecorder {
        id: id.clone(),
        extends_future,
        horizon,
        seen: Arc::clone(&seen),
    })]);
    (app, seen)
}

fn a_scan_at(timestamp: NaiveDateTime) -> rustdar_radar::types::ScanInfo {
    rustdar_radar::types::ScanInfo {
        site: rustdar_radar::sites::RadarSite {
            name: "KTLX",
            network: rustdar_radar::sites::RadarNetwork::of_id("KTLX"),
            lat: 35.33,
            lon: -97.27,
            heights: None,
        },
        site_source: rustdar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp,
        vcp_number: 212,
        available_products: vec![rustdar_radar::types::RadarProduct::Reflectivity],
        product_elevations: std::collections::HashMap::new(),
        status: String::new(),
    }
}

/// **The acceptance: a forward-reaching transport gets a forward-reaching
/// range.**
///
/// The pane has **no radar scan at all**, which is the whole point — a
/// model-only pane is exactly the pane the old gate refused. What it has is a
/// transport layer that declares `extends_future`, and that alone must produce
/// a listing request whose end is past the wall clock.
#[test]
fn a_forward_reaching_transport_asks_for_a_range_that_ends_after_now() {
    let id = LayerId::new("test/forecast");
    let horizon = chrono::Duration::hours(18);
    let (mut app, seen) = app_with(&id, true, horizon);

    let pane = app.gui.pane_mut(0).expect("the fixture has one pane");
    pane.set_transport_layer(id.clone());
    assert!(
        pane.scan_info.is_none(),
        "premise: this pane has no radar scan to anchor a loop on",
    );

    let before = chrono::Utc::now().naive_utc();
    app.handle_gui_action(
        GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: LOOKBACK,
        },
        None,
    );
    let after = chrono::Utc::now().naive_utc();

    let ranges = seen.lock().expect("no poisoned lock").clone();
    assert_eq!(
        ranges.len(),
        1,
        "no task dispatched: a pane with no radar scan still got no listing",
    );
    let (start, end) = ranges[0];

    assert!(
        end > after,
        "the range must reach past the wall clock: end {end} is not after {after}",
    );
    assert!(
        end >= before + horizon && end <= after + horizon,
        "and it must reach the layer's own horizon: {end} is not {before}..{after} + 18h",
    );
    let lookback = chrono::Duration::seconds(LOOKBACK as i64);
    assert!(
        start >= before - lookback && start <= after - lookback,
        "while the past half is still the lookback behind now: {start} is not \
         {before}..{after} - 600s",
    );
}

/// **Non-triviality: a backward-only transport is untouched.**
///
/// The same dispatch, the same recorder, the same assertion site — only the
/// declared axis differs. The range must still be the pane's own scan and the
/// lookback behind it, to the second, so "make every range reach forward"
/// cannot pass both this and the test above.
#[test]
fn a_backward_only_transport_still_ends_at_the_panes_own_scan() {
    let scan_at = chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .expect("a real date")
        .and_hms_opt(3, 20, 0)
        .expect("a real time");
    // A horizon that would be used if the arm were chosen on anything other
    // than the declared axis — so a wrong branch shows up as a wrong value
    // rather than as an absence.
    let (mut app, seen) = app_with(&known::RADAR, false, chrono::Duration::hours(18));

    let pane = app.gui.pane_mut(0).expect("the fixture has one pane");
    pane.scan_info = Some(a_scan_at(scan_at));
    assert_eq!(
        pane.transport_layer(),
        &known::RADAR,
        "premise: this pane's transport is radar",
    );

    app.handle_gui_action(
        GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: LOOKBACK,
        },
        None,
    );

    let ranges = seen.lock().expect("no poisoned lock").clone();
    assert_eq!(ranges.len(), 1, "no task dispatched");
    assert_eq!(
        ranges[0],
        (
            scan_at - chrono::Duration::seconds(LOOKBACK as i64),
            scan_at,
        ),
        "a backward-only transport's range is still anchored on the pane's scan",
    );
}
