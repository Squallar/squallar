//! **WI-5: a non-radar transport layer's loop gets frames, and the layer says
//! which of them it is holding.**
//!
//! Three claims, and each has a named mutation that turns it red:
//!
//! 1. A listing that lands for a layer becomes that layer's frame list.
//! 2. The list is sampled to what the pane's **byte** share buys, so a loop's
//!    frames cannot exceed the pool share they are held in.
//! 3. The settle verdict for that layer is the layer's own `frames_resident`,
//!    not the `|_| false` placeholder WI-2 left behind.
//!
//! **Nothing here claims a forecast loop animates.** This item stops at the
//! frame list; what puts a picture on one is WI-6's draw fork and WI-6b's
//! producer, pinned in `loop_overlay_render_tests`. A loop built here reaches
//! `Rendering` and stops there, and that is asserted as such.

use super::*;
use squallar_source::handler::{FetchPayload, PaneRef};
use squallar_source::id::LayerId;
use squallar_source::time::{FrameListing, FrameSource, FrameStamp};
use std::sync::{Arc, Mutex};

use squallar_overlays::render::overlay_state::{
    FetchConfig, FetchTask, OverlayHandler, OverlayRegistry, RenderMode, SourceEvent, Surface,
};

fn ts(minute: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(minute)
}

/// What the test layer was asked, shared with the test rather than downcast
/// back out of the registry — `OverlayRegistry` hands out `&dyn
/// OverlayHandler`, and adding a downcast door to production for one test is
/// the wrong trade.
#[derive(Default)]
struct Asked {
    /// One entry per `fetch_frame` call, in call order.
    fetched: Vec<chrono::NaiveDateTime>,
    /// The windows `create_frame_list_task` was asked to list, captured
    /// **synchronously inside it** — before the future is spawned, so the
    /// end-to-end test below does not depend on an executor running.
    listed_for: Vec<(chrono::NaiveDateTime, chrono::NaiveDateTime)>,
}

/// A layer that lists frames, holds some of them, and writes down every frame
/// it is asked to fetch.
struct SupplyLayer {
    id: LayerId,
    listed: Vec<chrono::NaiveDateTime>,
    resident: Vec<chrono::NaiveDateTime>,
    asked: Arc<Mutex<Asked>>,
}

impl FrameSource for SupplyLayer {
    /// The newest stamp this layer has listed at or before `t`, over the whole
    /// set and unclipped — the same rule `list_frames` narrows to a window.
    fn latest_at(&self, _pane: &PaneRef<'_>, t: chrono::NaiveDateTime) -> Option<FrameStamp> {
        let mut stamps: Vec<FrameStamp> = self
            .listed
            .iter()
            .map(|valid| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect();
        stamps.sort_by_key(|stamp| stamp.valid);
        squallar_source::time::newest_at_or_before(&stamps, t)
    }

    /// Residency is a fixture field here, set at construction, so nothing may
    /// change it behind the suite's back: eviction and delivery are both
    /// no-ops and the resident set is exactly what the test declared.
    fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}

    fn apply_frame_listing(
        &mut self,
        _listing: FrameListing,
        _scope: FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn apply_frame(&mut self, _stamp: FrameStamp, _data: FetchPayload, _pane: &PaneRef<'_>) {}

    /// Every stamp this layer knows about, clipped to the window it was asked
    /// for — the same shape the model layer's closed-form `Forecast` arm has.
    fn list_frames(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        FrameListing {
            range,
            frames: self
                .listed
                .iter()
                .filter(|valid| range.0 <= **valid && **valid <= range.1)
                .map(|valid| FrameStamp {
                    valid: *valid,
                    run: None,
                })
                .collect(),
            complete: true,
        }
    }

    /// Far enough forward that a whole listing sits inside the range this
    /// horizon produces.
    fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
        chrono::Duration::hours(18)
    }

    /// A closed form with no round trip, like the model layer's `Forecast`
    /// arm: the answer is ready the moment the task is built.
    fn create_frame_list_task(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        self.asked
            .lock()
            .expect("no poisoned lock")
            .listed_for
            .push(range);
        let frames: Vec<FrameStamp> = self
            .listed
            .iter()
            .filter(|valid| range.0 <= **valid && **valid <= range.1)
            .map(|valid| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect();
        Some(squallar_source::handler::FrameListingResult::task(
            self.id.clone(),
            async move {
                squallar_source::handler::FrameListingResult {
                    listing: FrameListing {
                        range,
                        frames,
                        complete: true,
                    },
                    scope: Box::new(()),
                }
            },
        ))
    }

    fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        self.resident
            .iter()
            .map(|valid| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect()
    }

    fn fetch_frame(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        self.asked
            .lock()
            .expect("no poisoned lock")
            .fetched
            .push(stamp.valid);
        Some(FetchTask {
            kind: self.id.clone(),
            future: Box::pin(async move { Box::new(()) as FetchPayload }),
        })
    }
}

impl OverlayHandler for SupplyLayer {
    fn id(&self) -> LayerId {
        self.id.clone()
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        999
    }
    fn display_name(&self) -> &str {
        "Supply"
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
    fn retain_selections(
        &self,
        _selections: &mut Vec<Arc<dyn squallar_overlays::render::overlay_state::OverlayItem>>,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn time_axis(&self) -> squallar_source::time::TimeAxis {
        squallar_source::time::TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: true,
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

const SUPPLY: &str = "test/supply";

fn supply_id() -> LayerId {
    LayerId::new(SUPPLY)
}

/// A one-pane app whose only registered layer is the test one, with `listed`
/// frames on offer and `resident` of them already held.
fn app_with_supply(
    listed: Vec<chrono::NaiveDateTime>,
    resident: Vec<chrono::NaiveDateTime>,
) -> (super::super::App, Arc<Mutex<Asked>>) {
    let asked: Arc<Mutex<Asked>> = Default::default();
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(SupplyLayer {
        id: supply_id(),
        listed,
        resident,
        asked: Arc::clone(&asked),
    })]);
    (app, asked)
}

/// Arm `pane_idx`'s test layer as a loop waiting on a listing over `range`,
/// and point the pane's transport at it.
fn awaiting_listing(
    app: &mut super::super::App,
    pane_idx: usize,
    range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
) {
    let pane = app
        .gui
        .pane_mut(pane_idx)
        .expect("the fixture built a pane");
    pane.set_transport_layer(supply_id());
    *pane.time_state_mut(&supply_id()) = squallar_egui::pane::LayerTimeState::begin(
        (range.1 - range.0).num_seconds() as u64,
        squallar_radar::types::RenderView::PlanView,
        Box::new(()),
    );
    // What the production dispatch records beside the phase: the window the
    // ask covered, which is what the arrival is matched on.
    pane.time_state_mut(&supply_id()).asked_range = Some(range);
    assert_eq!(
        pane.time_state(&supply_id()).phase,
        squallar_egui::pane::LoopPhase::FetchingScanList,
        "precondition: the layer must be waiting on a listing, or the arrival \
         has nothing to answer",
    );
    assert!(
        pane.time_state(&supply_id()).frames.is_empty(),
        "precondition: it has no frames yet",
    );
}

/// Put a listing for the test layer on the one arrival path and drain it.
fn deliver(
    app: &mut super::super::App,
    range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    frames: Vec<chrono::NaiveDateTime>,
) {
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: supply_id(),
            listing: FrameListing {
                range,
                frames: frames
                    .into_iter()
                    .map(|valid| FrameStamp { valid, run: None })
                    .collect(),
                complete: true,
            },
            scope: Box::new(()),
        })
        .expect("the receiver is alive");
    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();
}

/// A frame image, for the tests that need one frame to be showable. Radar's
/// variant is the only one that exists at this work item (WI-6 adds the
/// overlay one); nothing here looks inside it.
fn textured(ctx: &egui::Context) -> squallar_egui::pane::LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    squallar_egui::pane::LoopFrameImage::PlanView(squallar_egui::pane::RadarImageData {
        texture: ctx.load_texture("test", image, egui::TextureOptions::NEAREST),
        lat: 35.0,
        lon: -97.0,
        max_range_km: 100.0,
        placed: squallar_radar::types::ImageBounds::from_radar_site(35.0, -97.0, 100.0).into(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
    })
}

// ── 1. The listing becomes frames ─────────────────────────────────────────

/// **The acceptance of WI-5.** A listing that lands for a non-radar layer
/// becomes that layer's frame list, through the arrival path production uses.
///
/// Before this item the arrival reached `deliver_frame_listing` and stopped:
/// `accept_loop_scan_listings` was handed `(site, window)` pairs that only
/// radar's own scope produced, so a model listing built no loop and the frame
/// list stayed empty for ever.
///
/// **Floor: delete the `build_loop_frames` call in the non-radar arm** and the
/// frame-count assertion below reads 0.
#[test]
fn a_listing_for_a_non_radar_layer_becomes_that_layers_frame_list() {
    let listed: Vec<_> = (0..6).map(|i| ts(i * 60)).collect();
    let range = (ts(-60), ts(6 * 60));
    let (mut app, asked) = app_with_supply(listed.clone(), Vec::new());
    awaiting_listing(&mut app, 0, range);

    deliver(&mut app, range, listed.clone());

    let pane = app.gui.pane(0).expect("the fixture built a pane");
    let ls = pane.time_state(&supply_id());
    assert_eq!(
        ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
        listed,
        "the listing did not reach the layer's frame list through the contract",
    );
    assert_eq!(
        ls.phase,
        squallar_egui::pane::LoopPhase::Rendering,
        "the loop was left waiting on a listing that had already landed",
    );
    assert_eq!(
        ls.current_frame(),
        ls.frames.len() - 1,
        "a freshly built loop is parked on its newest frame",
    );
    assert_eq!(
        ls.cadence_secs,
        Some(3600),
        "the source's own cadence was not read off the listing",
    );

    // ── Fetch dispatch, on the same arrival ──────────────────────────────
    let fetched = asked.lock().expect("no poisoned lock").fetched.clone();
    assert_eq!(
        {
            let mut sorted = fetched.clone();
            sorted.sort_unstable();
            sorted
        },
        listed,
        "every frame the loop means to hold must be asked for; a frame list \
         with no fetch behind it is a loop that never fills",
    );
    assert_eq!(
        fetched.first().copied(),
        ls.frames.last().map(|f| f.timestamp),
        "and the first one asked for is the one the playhead is parked on — \
         `render_set_indices` walks outward from it",
    );
}

/// The pane's transport is the layer, so the transport accessors see the same
/// timeline the arrival built. (`loop_state` stays radar's by definition and
/// is untouched — WI-1's split.)
#[test]
fn the_transport_state_and_the_radar_state_are_not_the_same_loop() {
    let listed: Vec<_> = (0..4).map(|i| ts(i * 60)).collect();
    let range = (ts(-60), ts(4 * 60));
    let (mut app, _asked) = app_with_supply(listed.clone(), Vec::new());
    awaiting_listing(&mut app, 0, range);
    deliver(&mut app, range, listed.clone());

    let pane = app.gui.pane(0).expect("the fixture built a pane");
    assert_eq!(
        pane.transport_state()
            .frames
            .iter()
            .map(|f| f.timestamp)
            .collect::<Vec<_>>(),
        listed,
        "the transport addresses the layer the listing was for",
    );
    assert!(
        pane.time_state(&known::RADAR).frames.is_empty(),
        "and radar's own timeline was not touched by another layer's listing",
    );
}

// ── 2. Sampling, and the memory contract as a test ────────────────────────

/// **What one frame of this suite's layer costs before it has rasterized**:
/// `LoopFrameModel`'s `overlay` arm, which is `overlay_frame_bytes`'s fallback
/// on a headless pane. It is the device class's default window planned by
/// `plan_overlay_texture` — 2880x1620x4 = **18.66 MB** on the desktop and
/// mobile arms, 2048x1152x4 = **9.44 MB** on wasm — and **not** the radar
/// loop frame's 16 MiB / 4 MiB, which is what this fallback was before WB-7.
fn overlay_bytes() -> usize {
    LoopFrameModel::from_budgets(&test_budgets()).overlay
}

/// Whether [`overlay_bytes`] is still a different number from the radar loop
/// frame it was before WB-7 — 18.66 MB against 16 MiB on this arm, 9.44 MB
/// against 4 MiB on wasm. The suite's expected counts are all derived from
/// `overlay_bytes`, so this is the one thing here that a build pricing an
/// overlay frame as a radar frame would fail.
fn held_is_priced_as_an_overlay(frame_bytes: usize) -> bool {
    frame_bytes != test_budgets().loop_frame_bytes()
}

/// **The memory contract, asserted rather than written down**: a loop's frame
/// list times what one frame costs never exceeds the pool share the pane is
/// holding them in.
///
/// The denominator is stated: `allocation.share_bytes` for a single loop on
/// this build's own resolved budgets, and `overlay_frame_bytes`'s fallback —
/// [`overlay_bytes`], the **class's own overlay frame** — because a headless
/// pane has never rasterized and so has no texture to measure. A real pane
/// measures its own (a 1280x960-**point** pane is 1920x1440 texels =
/// 11.06 MB).
///
/// **Floor — `price_the_overlay_as_radar`: make `overlay_frame_bytes` fall
/// back to `budgets.loop_frame_bytes()`, as it did before WB-7.** The frame
/// list comes out at 36 on this arm instead of 32 (denominator: the desktop
/// pool floor, 576 MiB of share, one animating layer, 16 MiB per radar frame
/// against 18.66 MB per overlay frame) and the count assertion reds. That is
/// the whole of gap (a): a model frame priced as a radar frame is a model
/// frame the pool cannot see.
///
/// **Floor: remove the `listing_sample_indices` call from `build_loop_frames`**
/// — the count assertion reads the whole listing and the byte assertion goes
/// over the share.
#[test]
fn a_long_listing_is_sampled_to_what_the_panes_byte_share_buys() {
    let allocation = test_loop_allocation();
    let frame_bytes = overlay_bytes();
    assert!(
        held_is_priced_as_an_overlay(frame_bytes),
        "precondition: the price this list is built at ({frame_bytes} B) is \
         the radar loop frame's ({} B). Every figure below reads the overlay \
         price back off the same model, so with the two equal this whole \
         suite passes for a build that cannot see an overlay frame at all.",
        test_budgets().loop_frame_bytes(),
    );
    let held = layer_share(allocation, None, frame_bytes, 1);
    assert!(
        held >= 2,
        "precondition: the share must buy a loop at all, and it bought {held}",
    );

    // Three times the cap, so the sampling has to bind.
    let listed: Vec<_> = (0..(held as i64 * 3)).map(|i| ts(i * 60)).collect();
    let range = (ts(-60), ts(held as i64 * 3 * 60));
    let (mut app, _asked) = app_with_supply(listed.clone(), Vec::new());
    awaiting_listing(&mut app, 0, range);
    deliver(&mut app, range, listed.clone());

    let pane = app.gui.pane(0).expect("the fixture built a pane");
    let ls = pane.time_state(&supply_id());
    // **The memory contract first**, deliberately: it is the claim, and an
    // assertion that runs after a count check reads as a corollary of it
    // rather than as the bound.
    assert!(
        ls.frames.len() * frame_bytes <= allocation.share_bytes,
        "a loop of {} frames at {frame_bytes} B each is {} B, over the {} B \
         share this pane's loop is held in — the frame list is the thing that \
         has to fit, and prose about it is not the bound",
        ls.frames.len(),
        ls.frames.len() * frame_bytes,
        allocation.share_bytes,
    );
    assert_eq!(
        ls.frames.len(),
        held,
        "the frame list was not sampled down to the cap the byte share buys",
    );
    // Endpoint-anchored, not a fixed stride: both ends of the window survive.
    assert_eq!(
        ls.frames.first().map(|f| f.timestamp),
        listed.first().copied(),
        "the oldest frame went, so the loop is short of the window it listed",
    );
    assert_eq!(
        ls.frames.last().map(|f| f.timestamp),
        listed.last().copied(),
        "the newest frame went, so the loop stops short of what the pane shows",
    );
}

/// **`sampled` is a recorded decision and cannot lie.** The frames alone
/// cannot say whether a wider gap is a dropped frame or a slower source, so
/// the caption reads this flag — and a flag that disagreed with the list would
/// caption a thinned loop as complete.
///
/// **Floor: force `ls.sampled = Some(false)` in `build_loop_frames`** and the
/// sampled half goes red while the unsampled half still passes.
#[test]
fn sampled_says_whether_the_listing_had_to_be_thinned() {
    let allocation = test_loop_allocation();
    let held = layer_share(allocation, None, overlay_bytes(), 1);

    for (count, expected) in [(held / 2, false), (held * 3, true)] {
        let listed: Vec<_> = (0..count as i64).map(|i| ts(i * 60)).collect();
        let range = (ts(-60), ts(count as i64 * 60));
        let (mut app, _asked) = app_with_supply(listed.clone(), Vec::new());
        awaiting_listing(&mut app, 0, range);
        deliver(&mut app, range, listed.clone());

        let pane = app.gui.pane(0).expect("the fixture built a pane");
        let ls = pane.time_state(&supply_id());
        assert_eq!(
            ls.sampled,
            Some(expected),
            "a listing of {count} against a cap of {held} became {} frames, \
             and `sampled` said {:?}",
            ls.frames.len(),
            ls.sampled,
        );
    }
}

/// The floor of the byte division, stated rather than hidden: a share that
/// cannot buy two frames still gets two, because one frame is a still picture.
#[test]
fn a_share_too_small_for_two_frames_still_gets_two() {
    let allocation = test_loop_allocation();
    assert_eq!(
        layer_share(allocation, None, allocation.share_bytes * 4, 1),
        squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE,
        "the floor is where the budget stops being divisible, and it is the \
         one place the byte bound is knowingly exceeded",
    );
    assert_eq!(
        layer_share(allocation, None, 0, 1),
        squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE,
        "a frame that costs nothing is a model built wrong, and it must not \
         become an unbounded loop",
    );
}

/// Two animating layers halve each other's **bytes**, and the two of them
/// together still fit the one share.
#[test]
fn two_animating_layers_each_get_half_the_bytes() {
    let allocation = test_loop_allocation();
    let bytes = overlay_bytes();
    let one = layer_share(allocation, None, bytes, 1);
    let two = layer_share(allocation, None, bytes, 2);
    assert!(
        two < one,
        "a pane animating two layers spends its share twice: {one} frames \
         became {two}",
    );
    assert!(
        two * 2 * bytes <= allocation.share_bytes,
        "and the two of them together must still fit the one share",
    );
}

// ── 3. The residency oracle ───────────────────────────────────────────────

/// **The oracle is consulted through the production walk, and its polarity is
/// the point.**
///
/// A frame whose data the layer is holding is **not** settled: it is owed a
/// texture, and `dispatch_overlay_loop_renders` (WI-6b) is what makes one. A
/// frame the layer holds nothing for, with nothing being rendered, **is**
/// settled — there is no more to wait for.
///
/// Asserted through `update_loop_readiness` and on the phase it leaves behind,
/// because the phase is the only thing the two answers differ on: one loop,
/// one textured frame, and the layer's residency the only variable between the
/// two halves.
///
/// **Floor: restore WI-2's `|_| false` in `update_loop_readiness`** and the
/// first half goes red — under that placeholder every frame settles the
/// instant it exists, so a loop with one picture and three frames still
/// loading is promoted to `Ready` and its Play button lights up on a loop that
/// is three quarters empty.
///
/// The frames carry radar's image variant because it is the only one that
/// exists at this work item; nothing under test looks inside it.
#[test]
fn a_layer_whose_frames_are_resident_has_not_settled_them() {
    fn one_textured_loop(
        resident: Vec<chrono::NaiveDateTime>,
    ) -> (super::super::App, egui::Context) {
        let listed: Vec<_> = (0..4).map(|i| ts(i * 60)).collect();
        let range = (ts(-60), ts(4 * 60));
        let (mut app, _asked) = app_with_supply(listed.clone(), resident);
        awaiting_listing(&mut app, 0, range);
        deliver(&mut app, range, listed);

        let ctx = egui::Context::default();
        let pane = app.gui.pane_mut(0).expect("a pane");
        let ls = pane.time_state_mut(&supply_id());
        assert_eq!(ls.frames.len(), 4, "precondition: four frames landed");
        let newest = ls.frames.len() - 1;
        assert_eq!(
            ls.current_frame(),
            newest,
            "precondition: the playhead is on the frame about to be textured,              so the render set is centred on it",
        );
        ls.frames[newest].image = Some(textured(&ctx));
        (app, ctx)
    }

    // ── The layer is holding data for frames that have no picture yet ────
    let (mut app, _ctx) = one_textured_loop((0..4).map(|i| ts(i * 60)).collect());
    let before = app
        .gui
        .pane(0)
        .expect("a pane")
        .time_state(&supply_id())
        .frames
        .len();
    app.update_loop_readiness();

    let ls = app.gui.pane(0).expect("a pane").time_state(&supply_id());
    assert_eq!(
        ls.frames.len(),
        before,
        "the readiness pass must not wipe a timeline whose data has landed",
    );
    assert_eq!(
        ls.phase,
        squallar_egui::pane::LoopPhase::Rendering,
        "the layer is holding data for frames with no picture, so the batch          has NOT settled and the loop is still working",
    );

    // ── The layer is holding nothing more ───────────────────────────────
    let (mut app, _ctx) = one_textured_loop(vec![ts(3 * 60)]);
    app.update_loop_readiness();
    let ls = app.gui.pane(0).expect("a pane").time_state(&supply_id());
    assert!(
        ls.is_render_ready(),
        "with nothing further resident the batch has settled and the one frame \
         that can be shown promotes the loop — which is what makes the half \
         above a statement about the oracle rather than about the frames",
    );
}

/// The oracle answers per frame, not per layer: a partly-filled loop is
/// settled exactly where the layer holds nothing.
#[test]
fn the_residency_oracle_answers_one_frame_at_a_time() {
    let stamps: Vec<_> = (0..4).map(|i| ts(i * 60)).collect();
    let mut ls = squallar_egui::pane::LayerTimeState::new();
    ls.phase = squallar_egui::pane::LoopPhase::Rendering;
    ls.frames = stamps
        .iter()
        .map(|ts| squallar_egui::pane::LoopFrame {
            timestamp: *ts,
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    ls.settle_playhead(squallar_egui::pane::TimeMode::Live);

    let oracle = frames_are_resident(&stamps[..1]);
    assert!(
        oracle(&ls.frames[0]),
        "the layer holds the first frame's data",
    );
    assert!(
        !oracle(&ls.frames[1]),
        "and holds nothing for the second, which is a different answer",
    );
}

// ── Non-triviality: radar's own arm is unchanged ──────────────────────────

/// **The safety property, asserted rather than assumed**: a pane whose
/// transport is radar behaves exactly as it did.
///
/// Everything the radar arm produces from a listing is pinned here in one
/// place — the sampled frame list, the two recorded decisions, the phase, the
/// playhead and the frame plan the downloads are derived from — so a change
/// that re-points, re-samples or re-orders radar's supply cannot pass.
#[test]
fn the_radar_arm_produces_the_same_loop_it_always_did() {
    use squallar_source::id::known;

    // More scans than any arm's cap, so the sampling has to bind on all three.
    let count = 100i64;
    let span_secs = count as u64 * 240;
    let scans: Vec<_> = (0..count)
        .map(|i| ts(i * 4) - chrono::Duration::minutes(count * 4))
        .collect();
    let range = (
        *scans.first().expect("scans"),
        *scans.first().expect("scans") + chrono::Duration::seconds(span_secs as i64),
    );

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        pane.scan_info = Some(crate::app::tests::scan_info_for("KTLX"));
        *pane.time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
            span_secs,
            &squallar_radar::sites::RadarSite {
                name: "KTLX",
                network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
                lat: 35.33,
                lon: -97.28,
                heights: None,
            },
            squallar_radar::types::RenderView::PlanView,
        );
        pane.time_state_mut(&known::RADAR).asked_range = Some(range);
    }

    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: known::RADAR,
            listing: FrameListing {
                range,
                frames: scans
                    .iter()
                    .map(|valid| FrameStamp {
                        valid: *valid,
                        run: None,
                    })
                    .collect(),
                complete: true,
            },
            scope: Box::new(squallar_radar::source::RadarListing {
                site: "KTLX".to_string(),
                range,
                scans: scans
                    .iter()
                    .map(|ts| {
                        (
                            *ts,
                            squallar_radar::archive::Identifier::new(format!("KTLX{ts}")),
                        )
                    })
                    .collect(),
            }),
        })
        .expect("the receiver is alive");
    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();

    let allocation = app.loop_allocation();
    let ls_for_cap = app.gui.pane(0).expect("a pane").time_state(&known::RADAR);
    let held = layer_share(
        allocation,
        Some(loop_frames_held(allocation, ls_for_cap, &app.budgets)),
        LoopFrameModel::from_budgets(&app.budgets).bytes_for(ls_for_cap.view),
        1,
    );
    let expected: Vec<_> = squallar_egui::pane::listing_sample_indices(scans.len(), held)
        .expect("the listing must not fit this build's cap, or this proves nothing")
        .into_iter()
        .map(|i| scans[i])
        .collect();

    let pane = app.gui.pane(0).expect("the fixture built one pane");
    let ls = pane.time_state(&known::RADAR);
    assert_eq!(
        ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
        expected,
        "radar's frame list is no longer the endpoint-anchored sample of its \
         own listing",
    );
    assert_eq!(ls.sampled, Some(true), "and it was thinned to get there");
    assert_eq!(
        ls.cadence_secs,
        Some(240),
        "the site's own cadence, read before the sampling threw scans away",
    );
    assert_eq!(ls.phase, squallar_egui::pane::LoopPhase::Rendering);
    assert_eq!(
        ls.current_frame(),
        ls.frames.len() - 1,
        "parked on the newest",
    );
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        expected.len(),
        "the frame plan the downloads are derived from must describe the same \
         scans as the frame list",
    );
}

// ── The production wiring, end to end ─────────────────────────────────────

/// **The pin that says none of the above is decorative.**
///
/// Every other test here arms the pane's timeline by hand. This one turns the
/// loop on through the real `EnableLoop` action, takes the window the layer
/// was **actually asked to list** out of the layer itself, and answers with a
/// listing over that same window. If anything between the two ends disagrees
/// about which window this loop is for, the arrival matches no pane and the
/// frame list stays empty.
///
/// **The bug it caught, and the reason it exists:** WI-4's forward arm wrote
/// `LayerTimeState::begin(lookback_secs, ..)` while asking for a window of
/// `lookback + horizon`. `accept_loop_scan_listings` matches a landing listing
/// to a waiting pane on `span_secs`, so **every forecast listing was dropped
/// in silence** — 600 against 65 400 — and no amount of testing the builder
/// with a hand-set timeline would have shown it. `span_secs` is now the window
/// that was asked for, which is what its own doc comment always said it was.
///
/// **Floor: put `lookback_secs` back in the `begin` call** in
/// `arm_layer_loop`'s forward arm (the per-layer half `begin_loop_for_pane`
/// now calls once per frame-series layer) and the frame-list assertion reads
/// empty.
#[test]
fn enabling_a_forecast_loop_through_the_action_ends_with_frames_on_the_pane() {
    let now = chrono::Utc::now().naive_utc();
    // Stamps from the wall clock forward, so they sit in the forward-reaching
    // half of the range the transport asks for and nowhere else.
    let listed: Vec<_> = (0..6)
        .map(|i| now + chrono::Duration::hours(i) + chrono::Duration::minutes(1))
        .collect();
    let (mut app, asked) = app_with_supply(listed.clone(), Vec::new());
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built a pane");
        pane.set_transport_layer(supply_id());
        assert!(
            pane.scan_info.is_none(),
            "premise: a forecast pane has no radar scan to anchor a loop on",
        );
    }

    app.handle_gui_action(
        squallar_egui::actions::GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: 600,
        },
        None,
    );

    let window = {
        let seen = asked.lock().expect("no poisoned lock");
        assert_eq!(
            seen.listed_for.len(),
            1,
            "precondition: the action must have asked the layer for exactly \
             one window, and it asked for {:?}",
            seen.listed_for,
        );
        seen.listed_for[0]
    };
    assert!(
        window.1 > now,
        "precondition: the window must reach past the wall clock, or this is \
         not the forward arm at all",
    );

    // The listing the layer's own task would have produced, delivered on the
    // one arrival path. The window is the layer's, not one chosen here.
    deliver(&mut app, window, listed.clone());

    let pane = app.gui.pane(0).expect("the fixture built a pane");
    let ls = pane.transport_state();
    assert_eq!(
        ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
        listed,
        "the loop was enabled and its listing landed, and the frame list is \
         still empty: the two ends disagree about which window this loop is \
         for",
    );
    assert_eq!(
        ls.span_secs,
        (window.1 - window.0).num_seconds() as u64,
        "and the recorded span is the window that was listed for, which is \
         the thing the arrival matches on",
    );
}

// ── The window, not its width ─────────────────────────────────────────────

/// **Two panes, one layer, equal spans, two eras — each pane ends with its
/// own window's frames, whichever listing lands first.**
///
/// The reachable wrong picture: pane 0 loops the latest hour; pane 1 was
/// refilled over an hour of 1997 after a deep scrub. Both wait in
/// `FetchingScanList` and both windows are 3600 s wide, so a match on the
/// width alone answers whichever pane it reaches first with whichever listing
/// lands first — pane 0 presents 1997's frames as the latest hour, or pane 1
/// presents 2024's as 1997. A confidently wrong picture, not a missing one.
/// What separates them is the exact window each pane recorded when it asked
/// (`asked_range`), which every producer echoes verbatim.
///
/// Driven through the production arrival path (`SourceEvent::Frames` in, the
/// registry's own `list_frames` back out), in both delivery orders.
///
/// **Floor, run and observed:** revert the `waiting` filter in
/// `accept_loop_scan_listings` to `span_secs` and the first listing to land
/// builds BOTH panes — the archive-first order fails with pane 0 holding
/// `1997-06-15` stamps where `2024-01-01` ones were asked, and the
/// modern-first order fails pane 1 the mirrored way. Make the filter match
/// nothing instead and both panes sit empty in `FetchingScanList`, which the
/// phase assertion names.
fn each_loop_ask_gets_its_own_era(modern_listing_first: bool) {
    // Two anchors 27 years apart, one span. Four frames inside each hour,
    // every one carrying its own era on its face.
    let hour_before = |anchor: chrono::NaiveDateTime| {
        let range = (anchor - chrono::Duration::seconds(3600), anchor);
        let frames: Vec<chrono::NaiveDateTime> = (0..4)
            .rev()
            .map(|i| anchor - chrono::Duration::minutes(15 * i))
            .collect();
        (range, frames)
    };
    let (modern_range, modern) = hour_before(ts(12 * 60));
    let archive_anchor = chrono::NaiveDate::from_ymd_opt(1997, 6, 15)
        .unwrap()
        .and_hms_opt(7, 0, 0)
        .unwrap();
    let (archive_range, archive) = hour_before(archive_anchor);
    assert_eq!(
        modern_range.1 - modern_range.0,
        archive_range.1 - archive_range.0,
        "premise: the two windows must be exactly one width, or the width \
         could be what separates them and this test is about nothing",
    );

    // One layer holding both eras, as the real archive does.
    let mut listed = archive.clone();
    listed.extend(modern.iter().copied());
    let asked: Arc<Mutex<Asked>> = Default::default();
    let mut app = crate::app::tests::n_pane_app(2, "KTLX");
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(SupplyLayer {
        id: supply_id(),
        listed,
        resident: Vec::new(),
        asked: Arc::clone(&asked),
    })]);
    awaiting_listing(&mut app, 0, modern_range);
    awaiting_listing(&mut app, 1, archive_range);

    let deliveries = if modern_listing_first {
        [
            (modern_range, modern.clone()),
            (archive_range, archive.clone()),
        ]
    } else {
        [
            (archive_range, archive.clone()),
            (modern_range, modern.clone()),
        ]
    };
    for (range, frames) in deliveries {
        deliver(&mut app, range, frames);
    }

    for (pane_idx, expected, range) in [
        (0usize, &modern, modern_range),
        (1, &archive, archive_range),
    ] {
        let ls = app
            .gui
            .pane(pane_idx)
            .expect("the fixture built two panes")
            .time_state(&supply_id());
        assert_eq!(
            ls.phase,
            squallar_egui::pane::LoopPhase::Rendering,
            "pane {pane_idx} must have been built by its own listing — a \
             loop still fetching here means the arrival matched nothing",
        );
        assert_eq!(
            &ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
            expected,
            "pane {pane_idx} asked about {}..{} and holds another era's \
             stamps: one pane's window was answered with the other's listing",
            range.0,
            range.1,
        );
    }
}

#[test]
fn two_loop_asks_with_equal_spans_each_get_their_own_era_modern_listing_first() {
    each_loop_ask_gets_its_own_era(true);
}

#[test]
fn two_loop_asks_with_equal_spans_each_get_their_own_era_archive_listing_first() {
    each_loop_ask_gets_its_own_era(false);
}

/// **A re-enable a second later is answered by its own ask, not its
/// predecessor's.** Two enables of the "same" window issued a second apart
/// have different `now` anchors, so their ranges differ by a second at both
/// ends. The stale ask's listing finds no waiting pane — its replacement is
/// already in the air — and the pane keeps waiting; the current ask's listing
/// is what builds the loop. Refusing the stale answer loses nothing: the
/// fresh one was dispatched at re-enable and lands on its heels.
///
/// The other half of the crossed-wires floor: a matcher that answers nobody
/// fails here at the final phase assertion, so "match the window" cannot be
/// weakened to "match nothing".
#[test]
fn a_re_enabled_loop_is_answered_by_its_own_latest_ask() {
    let listed: Vec<chrono::NaiveDateTime> = (1..=4).map(|i| ts(-10 * i)).rev().collect();
    let (mut app, _asked) = app_with_supply(listed.clone(), Vec::new());
    let first = (ts(0) - chrono::Duration::seconds(3600), ts(0));
    let second = (
        first.0 + chrono::Duration::seconds(1),
        first.1 + chrono::Duration::seconds(1),
    );
    awaiting_listing(&mut app, 0, first);
    // The re-enable, one second later: the recorded ask moves with it.
    awaiting_listing(&mut app, 0, second);

    // The stale ask's answer lands first, as it will whenever the listings
    // come back in dispatch order.
    deliver(&mut app, first, listed.clone());
    {
        let ls = app.gui.pane(0).expect("a pane").time_state(&supply_id());
        assert!(
            ls.frames.is_empty(),
            "the superseded ask's listing built the loop over an anchor the \
             pane no longer asks about; its replacement is in the air",
        );
        assert_eq!(
            ls.phase,
            squallar_egui::pane::LoopPhase::FetchingScanList,
            "and the pane is still waiting on the ask it actually holds",
        );
    }

    deliver(&mut app, second, listed.clone());
    let ls = app.gui.pane(0).expect("a pane").time_state(&supply_id());
    assert_eq!(
        ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
        listed,
        "the loop's own latest ask must build it",
    );
    assert_eq!(
        ls.phase,
        squallar_egui::pane::LoopPhase::Rendering,
        "a loop whose own listing landed is building, not fetching",
    );
}
