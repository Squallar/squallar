//! **WI-4b: a past-only layer that is not radar can loop.**
//!
//! WI-4 made the *forward* arm address the pane's transport layer. The
//! backward arm was left as it was — it wrote `radar_layer::begin_loop` into
//! radar's slot by name (then spelled `loop_state_mut()`), and gated the
//! whole function on the pane holding a radar scan. So a satellite- or
//! MRMS-shaped layer (`extends_future: false`) armed **radar's** timeline with
//! a radar geometry anchor, its own slot stayed inactive, and the supply arm
//! that WI-5 added — chosen by the arrival carrying no NEXRAD site — found
//! nothing waiting for it. A pane with no radar loaded at all got no loop
//! whatsoever.
//!
//! Three claims, each with a named mutation that turns it red:
//!
//! 1. Enabling a loop on a past-only non-radar transport arms **that layer's**
//!    timeline, and the listing it asks for reaches back from the wall clock.
//! 2. Radar's armed shape is unchanged, field for field.
//! 3. Disabling addresses the same layer enabling did.
//!
//! The subject is a **backward-reaching `FrameSeries` test layer** rather than
//! MRMS or GMGSI, which were still `TimeAxis::Live` when this was written.
//! Both declare `FrameSeries` and supply frames now; the double stays because
//! it records the ranges it is asked about, which neither real layer does.

use super::*;
use crate::app::App;
use squallar_source::handler::{FetchPayload, FetchTask, FrameListingResult, PaneRef};
use squallar_source::time::{FrameListing, FrameSource, FrameStamp, TimeAxis};
use std::sync::{Arc, Mutex};

use squallar_overlays::render::overlay_state::{
    FetchConfig, OverlayHandler, OverlayItem, OverlayRegistry, RenderMode, SourceEvent, Surface,
};

/// What the test layer was asked, shared with the test rather than downcast
/// back out of the registry: `OverlayRegistry` hands out `&dyn
/// OverlayHandler`, and a downcast door in production for one test is the
/// wrong trade.
#[derive(Default)]
struct Asked {
    /// One entry per `create_frame_list_task` call, captured **inside it** —
    /// before the future is spawned, so nothing here waits on an executor.
    listed_for: Vec<(NaiveDateTime, NaiveDateTime)>,
}

/// A layer whose stamps are all history: `extends_future: false`, so
/// `begin_loop_for_pane` sends it down the backward arm, and a horizon that
/// would be visible in the range if the *forward* arm ran by mistake.
struct PastLayer {
    id: LayerId,
    listed: Vec<NaiveDateTime>,
    asked: Arc<Mutex<Asked>>,
}

impl FrameSource for PastLayer {
    fn latest_at(&self, _pane: &PaneRef<'_>, t: NaiveDateTime) -> Option<FrameStamp> {
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

    /// This double stages nothing, so there is nothing to evict and nothing to
    /// take delivery of — it exists to record the ranges it is *asked* about.
    fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}

    fn apply_frame_listing(
        &mut self,
        _listing: FrameListing,
        _scope: FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn apply_frame(&mut self, _stamp: FrameStamp, _data: FetchPayload, _pane: &PaneRef<'_>) {}

    /// **Non-zero on purpose.** A backward layer is never asked for this, so
    /// a range that reaches 18 hours past the wall clock is proof the forward
    /// arm ran — a wrong branch shows as a wrong value, not as an absence.
    fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
        chrono::Duration::hours(18)
    }

    fn list_frames(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        range: (NaiveDateTime, NaiveDateTime),
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

    fn create_frame_list_task(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        range: (NaiveDateTime, NaiveDateTime),
    ) -> Option<FetchTask> {
        self.asked
            .lock()
            .expect("no poisoned lock")
            .listed_for
            .push(range);
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

    fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        Vec::new()
    }

    fn fetch_frame(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        _stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        None
    }
}

impl OverlayHandler for PastLayer {
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
        "Past"
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

    /// **The declaration the arm is chosen on.** Satellite's and MRMS's shape:
    /// stamped frames, none of them later than now.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: false,
        }
    }

    /// **The frame-series answer, through the one `<=` in the workspace.** A
    /// double that inherited `Residency::none()` here would make every
    /// residency-routed decision below read as "the layer said nothing", which
    /// is the one answer that cannot distinguish a working route from a
    /// deleted one.
    fn residency_for(
        &self,
        pane: &PaneRef<'_>,
        stops: &[NaiveDateTime],
    ) -> squallar_source::time::Residency {
        squallar_source::time::frame_residency(self, pane, stops)
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

fn past_id() -> LayerId {
    LayerId::new("test/past")
}

const LOOKBACK: u64 = 3 * 3600;

/// A one-pane app whose only registered layer is the past-only test one.
fn app_with_past(listed: Vec<NaiveDateTime>) -> (App, Arc<Mutex<Asked>>) {
    let asked: Arc<Mutex<Asked>> = Default::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(PastLayer {
        id: past_id(),
        listed,
        asked: Arc::clone(&asked),
    })]);
    (app, asked)
}

/// Put a listing for the test layer on the one arrival path and drain it —
/// the same two calls the frame pump makes.
fn deliver(app: &mut App, range: (NaiveDateTime, NaiveDateTime), frames: Vec<NaiveDateTime>) {
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: past_id(),
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

fn a_scan_at(timestamp: NaiveDateTime) -> squallar_radar::types::ScanInfo {
    squallar_radar::types::ScanInfo {
        site: squallar_radar::sites::RadarSite {
            name: "KTLX",
            network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
            lat: 35.33,
            lon: -97.27,
            heights: None,
        },
        site_source: squallar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp,
        vcp_number: 212,
        available_products: vec![squallar_radar::types::RadarProduct::Reflectivity],
        product_elevations: std::collections::HashMap::new(),
        status: String::new(),
    }
}

// ── 1. The acceptance, driven end to end ──────────────────────────────────

/// **The acceptance of WI-4b.** A pane with **no radar scan at all** whose
/// transport addresses a past-only layer turns a loop on through the real
/// `EnableLoop` action, and ends with that layer's own frame list populated.
///
/// Everything is read from the production path: the window comes out of the
/// layer (what it was *asked* for), the listing is answered over that same
/// window on the one arrival channel, and the frames are read back off the
/// pane. If the two ends disagree about which window this loop is for, the
/// arrival matches no pane and the frame list stays empty — the exact failure
/// mode that hid in WI-4's forward arm for a whole work item.
///
/// **Floors, each run and observed:**
/// - Point the write back at `time_state_mut(&known::RADAR)` and the "armed the transport
///   layer" assertion reads inactive.
/// - Restore the `scan_info` early return at the top of the function and no
///   listing is asked for at all.
#[test]
fn enabling_a_past_only_non_radar_loop_lands_frames_on_that_layers_timeline() {
    let now = chrono::Utc::now().naive_utc();
    // Stamps strictly behind the wall clock, so they sit in a backward range
    // and in no forward one.
    let listed: Vec<_> = (1..=4)
        .map(|i| now - chrono::Duration::minutes(i * 30))
        .rev()
        .collect();
    let (mut app, asked) = app_with_past(listed.clone());
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        pane.set_transport_layer(past_id());
        assert!(
            pane.scan_info.is_none(),
            "premise: a satellite-shaped pane has no radar scan to anchor on",
        );
    }

    let before = chrono::Utc::now().naive_utc();
    app.handle_gui_action(
        GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: LOOKBACK,
        },
        None,
    );
    let after = chrono::Utc::now().naive_utc();

    // ── The transport layer's timeline is the one that was armed ──────────
    {
        let pane = app.gui.pane(0).expect("the fixture built one pane");
        assert!(
            pane.transport_state().is_active(),
            "the transport layer's own timeline is inactive: the loop was \
             armed on somebody else's slot",
        );
        assert_eq!(
            pane.transport_state().phase,
            squallar_egui::pane::LoopPhase::FetchingScanList,
            "and it is waiting on the listing it just asked for",
        );
        assert!(
            !pane.time_state(&known::RADAR).is_active(),
            "radar's slot was armed for a layer that is not radar",
        );
        assert!(
            pane.transport_state()
                .anchor_as::<squallar_radar::loop_geometry::LoopGeometry>()
                .is_none(),
            "a layer with no geometry was given radar's site anchor",
        );
        assert_eq!(
            squallar_egui::radar_layer::site(pane.transport_state()),
            "",
            "and reading a site off that anchor answers empty rather than \
             panicking, which is what the arrival filter compares against",
        );
    }

    // ── The window the layer was asked for reaches BACKWARD ───────────────
    let window = {
        let seen = asked.lock().expect("no poisoned lock");
        assert_eq!(
            seen.listed_for.len(),
            1,
            "the action must have asked the layer for exactly one window, and \
             it asked for {:?} — a radar-less pane still gets no loop",
            seen.listed_for,
        );
        seen.listed_for[0]
    };
    assert!(
        window.1 >= before && window.1 <= after,
        "a past-only loop ends at the wall clock: {} is not in {before}..{after}",
        window.1,
    );
    assert_eq!(
        window.1 - window.0,
        chrono::Duration::seconds(LOOKBACK as i64),
        "and reaches exactly the lookback behind it",
    );

    // ── The listing lands, over the window the layer itself named ─────────
    deliver(&mut app, window, listed.clone());

    let pane = app.gui.pane(0).expect("the fixture built one pane");
    let ls = pane.transport_state();
    assert_eq!(
        ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
        listed,
        "the loop was enabled and its listing landed, and the frame list is \
         still empty: the two ends disagree about which window this loop is for",
    );
    assert_eq!(
        ls.span_secs,
        (window.1 - window.0).num_seconds() as u64,
        "the recorded span is not the window that was listed for, which is \
         the thing the arrival matches a landing listing to a pane on",
    );
    assert_eq!(
        ls.phase,
        squallar_egui::pane::LoopPhase::Rendering,
        "the loop is still waiting on a listing that has already landed",
    );
    assert_eq!(
        ls.current_frame(),
        ls.frames.len() - 1,
        "a freshly built loop is parked on its newest frame",
    );
}

// ── 2. Radar is unchanged ─────────────────────────────────────────────────

/// **The safety property: radar's backward loop arms exactly what it always
/// did.** Radar's is the loop shipping in the product, and it is the thing a
/// generic backward arm is most likely to alter.
///
/// Every field of the armed timeline is read back and asserted against a
/// literal — not against a second call to `radar_layer::begin_loop`, which
/// would move with any mutation of the expression under test — together with
/// the range the layer was asked to list.
///
/// **Floor, run and observed:** make the backward arm unconditionally generic
/// (drop the `radar_site` arm so radar takes the placeholder anchor too) and
/// the site assertion reads `""`.
#[test]
fn radars_backward_loop_arms_the_same_shape_it_always_did() {
    let scan_at = chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .expect("a real date")
        .and_hms_opt(3, 20, 0)
        .expect("a real time");
    // Radar registered as a past-only layer, so the range it is asked for is
    // observable at the one place the shell hands it over. The layer's
    // 18-hour horizon is never consulted on this arm.
    let asked: Arc<Mutex<Asked>> = Default::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(PastLayer {
        id: known::RADAR,
        listed: Vec::new(),
        asked: Arc::clone(&asked),
    })]);
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        pane.scan_info = Some(a_scan_at(scan_at));
        assert_eq!(
            pane.transport_layer(),
            &known::RADAR,
            "premise: a radar pane's transport addresses radar",
        );
    }

    app.handle_gui_action(
        GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: LOOKBACK,
        },
        None,
    );

    assert_eq!(
        asked
            .lock()
            .expect("no poisoned lock")
            .listed_for
            .as_slice(),
        [(
            scan_at - chrono::Duration::seconds(LOOKBACK as i64),
            scan_at,
        )],
        "radar's range is still anchored on the pane's own scan and walks the \
         lookback back from it",
    );

    let pane = app.gui.pane(0).expect("the fixture built one pane");
    let ls = pane.time_state(&known::RADAR);
    assert!(
        std::ptr::eq(ls, pane.transport_state()),
        "on a radar pane the two accessors are one slot, which is why writing \
         through the transport cannot move radar's timeline",
    );
    assert_eq!(squallar_egui::radar_layer::site(ls), "KTLX");
    assert_eq!(squallar_egui::radar_layer::coords(ls), (35.33, -97.27));
    assert_eq!(ls.phase, squallar_egui::pane::LoopPhase::FetchingScanList);
    assert_eq!(ls.span_secs, LOOKBACK, "the window it was listed for");
    assert_eq!(
        ls.asked_range,
        Some((
            scan_at - chrono::Duration::seconds(LOOKBACK as i64),
            scan_at
        )),
        "and the very window it asked, recorded whole — what the arrival is \
         matched on",
    );
    assert_eq!(ls.view, squallar_radar::types::RenderView::PlanView);
    assert!(ls.frames.is_empty());
    assert_eq!(ls.current_frame(), 0);
    assert_eq!(ls.playhead_stamp(), None, "nothing to be parked on yet");
    assert!(ls.listing.is_none());
    assert_eq!(ls.sampled, None);
    assert_eq!(ls.cadence_secs, None);
    assert!(ls.last_advance.is_none());
    assert!(
        ls.listing_since.is_some(),
        "the clock on the listing phase started",
    );
    assert!(ls.rendered_for.is_none());
    assert!(ls.view_key.is_none());
}

/// **A radar pane with no scan still gets no loop**, which is the half of the
/// old gate that was correct: radar's range ends at the scan the pane is
/// showing, and there is none.
///
/// Stated beside the test above so the gate's two halves are visibly one
/// decision rather than a rule that was simply deleted.
#[test]
fn a_radar_pane_with_no_scan_still_arms_nothing() {
    let asked: Arc<Mutex<Asked>> = Default::default();
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(PastLayer {
        id: known::RADAR,
        listed: Vec::new(),
        asked: Arc::clone(&asked),
    })]);
    assert!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .scan_info
            .is_none(),
        "premise: the pane holds no scan",
    );

    app.handle_gui_action(
        GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: LOOKBACK,
        },
        None,
    );

    assert!(
        asked
            .lock()
            .expect("no poisoned lock")
            .listed_for
            .is_empty(),
        "radar was asked for a listing with no scan to anchor its range on",
    );
    assert!(
        !app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .time_state(&known::RADAR)
            .is_active(),
    );
}

// ── 3. Disable is the mirror of enable ────────────────────────────────────

/// **Turning a loop off stops every timeline turning it on armed.**
///
/// `handle_disable_loop` used to reset radar's slot by name. On a pane
/// whose transport had moved that cleared a timeline nobody had armed and left
/// the running one running — and the ∞ button reads
/// `transport_state().is_active()`, so it stayed lit and re-emitted this same
/// action on every further click. That half is what the first assertion below
/// still holds, and it is still the one the ∞ button reads.
///
/// **The second assertion was inverted deliberately.** It used to require that
/// radar's slot survive a disable "that was not about it", on the reading that
/// a pane's loop is one layer's. It is not: `handle_enable_loop` arms every
/// enabled frame-series layer on the pane, because a satellite layer that is
/// not armed paints one instant for the whole playback (the reported *"the
/// GMGSI never changes"*). One ∞ button, one clock, one loop — so turning it
/// off has to take down everything it turned on. A timeline left running under
/// a stopped transport settles its playhead off a clock nothing moves, which
/// is a frozen frame presented as the live picture.
///
/// The radar slot is armed here as well, so the test can tell "cleared the
/// transport" from "cleared nothing but the slot it was named after".
///
/// **Floor, run and observed:** put `*pane.time_state_mut(&known::RADAR) =
/// LayerTimeState::new()` back in `handle_disable_loop` and the transport
/// layer is still active.
#[test]
fn disabling_a_loop_stops_every_timeline_the_pane_armed() {
    let now = chrono::Utc::now().naive_utc();
    let listed: Vec<_> = (1..=4)
        .map(|i| now - chrono::Duration::minutes(i * 30))
        .rev()
        .collect();
    let (mut app, _asked) = app_with_past(listed);
    {
        let pane = app.gui.pane_mut(0).expect("the fixture built one pane");
        pane.set_transport_layer(past_id());
        // A radar loop running beside it, so "reset the transport" and "reset
        // everything" are distinguishable answers.
        *pane.time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
            600,
            &squallar_radar::sites::RadarSite {
                name: "KOUN",
                network: squallar_radar::sites::RadarNetwork::of_id("KOUN"),
                lat: 35.23,
                lon: -97.46,
                heights: None,
            },
            squallar_radar::types::RenderView::PlanView,
        );
    }

    app.handle_gui_action(
        GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: LOOKBACK,
        },
        None,
    );
    assert!(
        app.gui
            .pane(0)
            .expect("the fixture built one pane")
            .transport_state()
            .is_active(),
        "precondition: the transport layer's loop is running",
    );

    app.handle_gui_action(GuiAction::DisableLoop { pane_idx: 0 }, None);

    let pane = app.gui.pane(0).expect("the fixture built one pane");
    assert!(
        !pane.transport_state().is_active(),
        "the ∞ button reads this, so the loop the user turned off is still on",
    );
    assert!(
        !pane.time_state(&known::RADAR).is_active(),
        "radar's timeline is still running under a stopped transport: its \
         playhead now settles off a clock nothing moves, so the pane paints \
         one frozen radar frame with no loop to explain it",
    );
    assert!(
        pane.animating_layers().next().is_none(),
        "the pane is still animating {:?} after the ∞ button was turned off",
        pane.animating_layers()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>(),
    );
}

// ── 4. The armed window is the layer's own ────────────────────────────────

/// **The acceptance.** The window a loop is armed over reaches back to the
/// frame its own first stop is drawn from, which the lookback alone cannot
/// name.
///
/// A layer whose steps are coarser than where the lookback lands has no frame
/// stamped inside the window's leading partial step — the picture that step is
/// drawn from is the granule *before* the window. A listing clipped at the
/// lookback's edge names none, and the first step of the sweep is blank. The
/// layer is the only thing that knows this, and `residency_for` is where it
/// says so.
#[test]
fn an_armed_window_is_the_one_the_layer_asked_for() {
    let now = chrono::Utc::now().naive_utc();
    // One granule an hour before the lookback's own edge, and one inside the
    // window. The first is what the window's first stop is drawn from.
    let behind = now - chrono::Duration::seconds(LOOKBACK as i64) - chrono::Duration::hours(1);
    let inside = now - chrono::Duration::minutes(10);
    let (mut app, asked) = app_with_past(vec![behind, inside]);
    app.gui
        .pane_mut(0)
        .expect("the fixture built one pane")
        .set_transport_layer(past_id());

    app.handle_gui_action(
        GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: LOOKBACK,
        },
        None,
    );

    let listed = asked.lock().expect("no poisoned lock").listed_for.clone();
    assert_eq!(listed.len(), 1, "one arm, one listing: {listed:?}");
    let (start, end) = listed[0];
    assert_eq!(
        start,
        behind,
        "the window must reach the granule its own first stop is drawn from, \
         and stopped {} short",
        start - behind,
    );
    assert!(
        end >= inside,
        "and it still ends where the lookback said, not at the granule",
    );
    // The pane records the ask it made, or the listing that lands matches
    // nothing.
    assert_eq!(
        app.gui
            .pane(0)
            .expect("one pane")
            .transport_state()
            .asked_range,
        Some((start, end)),
        "the recorded window and the window asked for are one quantity",
    );
}

/// **The floor.** A layer that knows nothing before the lookback's edge is
/// armed over exactly the lookback, to the second — so "always widen" cannot
/// pass the acceptance above, and a radar pane arming its first loop is
/// untouched.
#[test]
fn a_layer_with_nothing_behind_the_lookback_is_armed_over_the_lookback() {
    let now = chrono::Utc::now().naive_utc();
    // Everything this layer knows sits well inside the window.
    let listed: Vec<NaiveDateTime> = (1..=3)
        .map(|i| now - chrono::Duration::minutes(i * 10))
        .rev()
        .collect();
    let (mut app, asked) = app_with_past(listed);
    app.gui
        .pane_mut(0)
        .expect("the fixture built one pane")
        .set_transport_layer(past_id());

    let before = chrono::Utc::now().naive_utc();
    app.handle_gui_action(
        GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: LOOKBACK,
        },
        None,
    );
    let after = chrono::Utc::now().naive_utc();

    let listed_for = asked.lock().expect("no poisoned lock").listed_for.clone();
    assert_eq!(listed_for.len(), 1, "one arm, one listing: {listed_for:?}");
    let (start, end) = listed_for[0];
    assert_eq!(
        (end - start).num_seconds(),
        LOOKBACK as i64,
        "the window is exactly the lookback wide",
    );
    assert!(
        end >= before && end <= after,
        "and it ends at the wall clock this arm was taken on",
    );
}
