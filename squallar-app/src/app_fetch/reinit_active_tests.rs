//! **WO-T3.9: which panes `reinit_active_loops` re-arms, and how wide.**
//!
//! Jump-to-live and a scan arrival both slide a running loop's window forward
//! by re-arming it. The walk that picks which panes to re-arm was wrong twice,
//! and the two halves are independent:
//!
//! 1. It filtered on `time_state(&known::RADAR).is_active()`, so a pane whose
//!    transport is GMGSI, MRMS or the model — an inactive radar slot — was
//!    skipped **entirely**. Its window never slid forward on either caller.
//! 2. It took the width from `LayerTimeState::span_secs`, the width a listing
//!    was *recorded* as having been asked over. That figure carries whatever
//!    `armed_start` widened the window by, so feeding it back in grows the
//!    window every time; what `handle_enable_loop` is fed everywhere else is
//!    the Lookback setting raised to the transport's own floor
//!    (`Gui::loop_span_secs_for`, now `PaneState::loop_span_secs`).
//!
//! The subject is a **coarse frame-series double** rather than the real GMGSI:
//! it declares the same `min_loop_frames`, and it records the ranges it is
//! asked to list, which the real layer does not — and it reaches no bucket.

use super::*;
use crate::app::App;
use squallar_source::handler::{FetchPayload, FetchTask, FrameListingResult, PaneRef};
use squallar_source::time::{FrameListing, FrameSource, FrameStamp, TimeAxis};
use std::sync::{Arc, Mutex};

use squallar_overlays::render::overlay_state::{
    FetchConfig, OverlayHandler, OverlayItem, OverlayRegistry, RenderMode, Surface,
};

/// One entry per `create_frame_list_task` call, captured **inside it** — before
/// the future is spawned, so nothing here waits on an executor.
pub(super) type Asked = Arc<Mutex<Vec<(chrono::NaiveDateTime, chrono::NaiveDateTime)>>>;

/// A past-only frame-series layer that records what window it is listed over,
/// declares a loop-width floor of its own, and holds a resident set the test
/// stages.
///
/// `min_frames` is the whole reason this is not a copy of `PastLayer` next
/// door: it is what makes the Lookback slider and the window a loop is really
/// listed over two different numbers, which is where the second half of
/// WO-T3.9's defect lives. `resident` is what `step_button_tests` next door
/// needs, and every fixture in *this* file leaves it empty on purpose — see
/// [`CoarseLayer::latest_at`].
pub(super) struct CoarseLayer {
    pub(super) id: LayerId,
    pub(super) min_frames: usize,
    /// Ascending, as [`FrameSource::frames_resident`]'s contract requires.
    pub(super) resident: Vec<chrono::NaiveDateTime>,
    pub(super) asked: Asked,
}

impl OverlayHandler for CoarseLayer {
    fn id(&self) -> LayerId {
        self.id.clone()
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        997
    }
    fn display_name(&self) -> &str {
        "Coarse"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
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
    fn set_fetching(&mut self, _f: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
    fn retain_selections(&self, _sel: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn time_axis(&self) -> TimeAxis {
        TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(HOUR),
            extends_future: false,
        }
    }

    /// The declaration under test: thirteen hourly frames is a twelve-hour
    /// floor, GMGSI's own answer. Zero is radar's, and means the window is
    /// exactly the one the slider names.
    fn min_loop_frames(&self) -> usize {
        self.min_frames
    }

    /// The frame-series answer, through the one `<=` in the workspace. A
    /// double inheriting `Residency::none()` would make `armed_start` read as
    /// "the layer said nothing" and could not tell a live route from a deleted
    /// one.
    fn residency_for(
        &self,
        pane: &PaneRef<'_>,
        stops: &[chrono::NaiveDateTime],
    ) -> squallar_source::time::Residency {
        squallar_source::time::frame_residency(self, pane, stops)
    }

    fn frames(&self) -> Option<&dyn FrameSource> {
        Some(self)
    }

    fn frames_mut(&mut self) -> Option<&mut dyn FrameSource> {
        Some(self)
    }
}

impl CoarseLayer {
    fn stamps(&self) -> Vec<FrameStamp> {
        self.resident
            .iter()
            .map(|valid| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect()
    }
}

impl FrameSource for CoarseLayer {
    /// The frame-series rule, through the one `<=` in the workspace. **Every
    /// fixture in this file stages nothing**, so nothing qualifies and
    /// `armed_start` widens no window — which is what leaves the arithmetic
    /// WO-T3.9 is about exactly the lookback.
    fn latest_at(&self, _pane: &PaneRef<'_>, t: chrono::NaiveDateTime) -> Option<FrameStamp> {
        squallar_source::time::newest_at_or_before(&self.stamps(), t)
    }

    fn list_frames(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        FrameListing::empty(range)
    }

    fn create_frame_list_task(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        self.asked.lock().expect("no poisoned lock").push(range);
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

    fn fetch_frame(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        _stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        None
    }

    fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        self.stamps()
    }

    fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}

    fn apply_frame_listing(
        &mut self,
        _listing: FrameListing,
        _scope: FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn apply_frame(&mut self, _stamp: FrameStamp, _data: FetchPayload, _pane: &PaneRef<'_>) {}

    fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
        chrono::Duration::zero()
    }
}

const HOUR: u64 = 3600;
/// The Lookback slider's own setting throughout — one hour, the default.
const SLIDER: u64 = HOUR;
/// Thirteen hourly frames, end to end: what a coarse layer's floor raises
/// `SLIDER` to.
const FLOOR: u64 = 12 * HOUR;
const SITE: &str = "KTLX";

fn satellite() -> LayerId {
    LayerId::new("test/coarse")
}

fn at(hour: u32, minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 4, 2)
        .expect("a real date")
        .and_hms_opt(hour, minute, 0)
        .expect("a real time")
}

/// A one-pane app registering `layers`, with nothing else in the registry.
fn app_with(layers: Vec<(LayerId, usize)>) -> (App, Asked) {
    let asked: Asked = Default::default();
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    app.gui.overlays = OverlayRegistry::with_handlers(
        layers
            .into_iter()
            .map(|(id, min_frames)| {
                Box::new(CoarseLayer {
                    id,
                    min_frames,
                    // Deliberately empty here: see `CoarseLayer::latest_at`.
                    resident: Vec::new(),
                    asked: Arc::clone(&asked),
                }) as Box<dyn OverlayHandler>
            })
            .collect(),
    );
    app.gui.pane_mut(0).expect("one pane").time.span_secs = SLIDER;
    (app, asked)
}

/// Arm `id`'s timeline by hand, recording `recorded_span` as the width the
/// listing it is replacing was asked over.
fn arm(app: &mut App, id: &LayerId, recorded_span: u64) {
    let pane = app.gui.pane_mut(0).expect("one pane");
    pane.set_transport_layer(id.clone());
    let ls = pane.time_state_mut(id);
    ls.phase = squallar_egui::pane::LoopPhase::Ready;
    ls.span_secs = recorded_span;
    ls.asked_range = Some((at(0, 0), at(1, 0)));
}

/// Put a volume in the jump-to-live cache for `SITE`, which is the branch that
/// reaches `reinit_active_loops`.
fn cache_a_live_volume(app: &mut App, stamp: chrono::NaiveDateTime) {
    let info = squallar_radar::types::ScanInfo::from_scan(
        &crate::app::tests::empty_scan(),
        SITE,
        stamp,
        None,
    );
    app.latest_cached_scans.insert(
        SITE.to_string(),
        (
            Arc::new(crate::app::tests::empty_scan()),
            Default::default(),
            info,
            stamp,
        ),
    );
}

fn jump_to_live(app: &mut App) {
    app.handle_gui_action(GuiAction::JumpToLive { pane_idx: 0 }, None);
}

/// The widths of every listing asked for, in arrival order.
fn asked_widths(asked: &Asked) -> Vec<u64> {
    asked
        .lock()
        .expect("no poisoned lock")
        .iter()
        .map(|(start, end)| (*end - *start).num_seconds().max(0) as u64)
        .collect()
}

/// **WO-T3.9's acceptance.** A satellite-transport pane's loop slides forward
/// when the pane jumps to live, and it slides over the window its own
/// transport asks for.
///
/// Before this, `reinit_active_loops` filtered on radar's slot. A pane whose
/// transport is a satellite has an inactive radar slot, so it was skipped
/// entirely: **no listing was asked for at all**, and the loop kept the window
/// captured whenever it was last armed by hand. The `asked` recorder below is
/// empty in that world.
///
/// The second half is the width. The pane's recorded `span_secs` is
/// deliberately neither the slider nor the floor — it is what a previous arm
/// left behind once `armed_start` widened its window — so an implementation
/// reading it back is visible as a third number rather than as a coincidence.
///
/// **Floor** — the transport really does declare a floor above the slider, so
/// "it used `loop_span_secs`" is distinguishable from "it used the slider".
#[test]
fn jumping_to_live_slides_a_satellite_loops_window_forward() {
    let (mut app, asked) = app_with(vec![(satellite(), 13)]);
    // Neither `SLIDER` nor `FLOOR`: the recorded width a previous arm left.
    const DRIFTED: u64 = 60_000;
    arm(&mut app, &satellite(), DRIFTED);
    cache_a_live_volume(&mut app, at(6, 0));
    assert!(
        asked_widths(&asked).is_empty(),
        "precondition: the loop was armed by hand, so every range recorded \
         below was asked for by the jump",
    );

    jump_to_live(&mut app);

    assert_eq!(
        asked_widths(&asked),
        vec![FLOOR],
        "a satellite-transport pane's loop did not slide forward when the \
         pane jumped to live. An empty list is the pane being skipped \
         entirely — the radar-addressed filter; {SLIDER} is the Lookback \
         slider read without the transport's floor; {DRIFTED} is the width \
         the previous listing was recorded as having been asked over, which \
         grows every time it is fed back in",
    );

    let pane = app.gui.pane(0).expect("one pane");
    let (start, end) = pane
        .time_state(&satellite())
        .asked_range
        .expect("the re-armed timeline records the window it asked for");
    assert!(
        start > at(1, 0) && end > at(1, 0),
        "the window did not move off the one the fixture armed by hand \
         ({start}..{end}); sliding forward is the whole point",
    );
}

/// **The floor: an ordinary radar-transport pane is re-armed exactly as
/// before.** Its transport declares no width floor, and its recorded span is
/// the one a fresh radar arm leaves — the slider — so the two readings agree
/// and this pane must come out byte-identical to the pre-fix walk.
#[test]
fn a_radar_transport_panes_re_armed_window_is_unchanged() {
    let (mut app, asked) = app_with(vec![(known::RADAR, 0)]);
    let scan_at = at(6, 0);
    arm(&mut app, &known::RADAR, SLIDER);
    app.gui.pane_mut(0).expect("one pane").scan_info =
        Some(squallar_radar::types::ScanInfo::from_scan(
            &crate::app::tests::empty_scan(),
            SITE,
            scan_at,
            None,
        ));
    cache_a_live_volume(&mut app, scan_at);

    jump_to_live(&mut app);

    assert_eq!(
        asked.lock().expect("no poisoned lock").as_slice(),
        [(scan_at - chrono::Duration::seconds(SLIDER as i64), scan_at)],
        "radar's re-armed range is still the lookback walked back from the \
         scan the pane is showing",
    );
}

/// **The second half of the defect, on the one pane the old walk did select.**
///
/// A radar-transport pane whose recorded `span_secs` has drifted away from the
/// slider — the state `armed_start` leaves once a listing has landed and the
/// window was widened to reach the frame its first stop is drawn from — was
/// re-armed over the *drifted* figure, so every jump-to-live and every scan
/// arrival widened it again. This pane's window is deliberately **not**
/// unchanged; the floor above is the case that must be.
#[test]
fn a_re_arm_takes_the_lookback_and_not_the_width_the_last_listing_recorded() {
    let (mut app, asked) = app_with(vec![(known::RADAR, 0)]);
    let scan_at = at(6, 0);
    const DRIFTED: u64 = 2 * HOUR;
    arm(&mut app, &known::RADAR, DRIFTED);
    app.gui.pane_mut(0).expect("one pane").scan_info =
        Some(squallar_radar::types::ScanInfo::from_scan(
            &crate::app::tests::empty_scan(),
            SITE,
            scan_at,
            None,
        ));
    cache_a_live_volume(&mut app, scan_at);

    jump_to_live(&mut app);

    assert_eq!(
        asked_widths(&asked),
        vec![SLIDER],
        "the re-arm took the width the last listing was recorded as having \
         been asked over ({DRIFTED}s) rather than the Lookback setting, so a \
         loop grows a little on every jump to live",
    );
}
