//! **WI-4b: a past-only layer that is not radar can loop.**
//!
//! WI-4 made the *forward* arm address the pane's transport layer. The
//! backward arm was left as it was — it wrote `radar_layer::begin_loop` into
//! `loop_state_mut()`, which is radar's slot by definition, and gated the
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
//! The subject is a **backward-reaching `FrameSeries` test layer**, not MRMS
//! or GMGSI: both are still `TimeAxis::Live` and implement none of the frame
//! contract, and converting them is a later item.

use super::*;
use crate::app::App;
use rustdar_source::handler::{FetchPayload, FetchTask, FrameListingResult, PaneRef};
use rustdar_source::time::{FrameListing, FrameStamp, TimeAxis};
use std::sync::{Arc, Mutex};

use rustdar_overlays::render::overlay_state::{
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
/// - Point the write back at `loop_state_mut()` and the "armed the transport
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
            rustdar_egui::pane::LoopPhase::FetchingScanList,
            "and it is waiting on the listing it just asked for",
        );
        assert!(
            !pane.loop_state().is_active(),
            "radar's slot was armed for a layer that is not radar",
        );
        assert!(
            pane.transport_state()
                .anchor_as::<rustdar_radar::loop_geometry::LoopGeometry>()
                .is_none(),
            "a layer with no geometry was given radar's site anchor",
        );
        assert_eq!(
            rustdar_egui::radar_layer::site(pane.transport_state()),
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
        rustdar_egui::pane::LoopPhase::Rendering,
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
    let ls = pane.loop_state();
    assert!(
        std::ptr::eq(ls, pane.transport_state()),
        "on a radar pane the two accessors are one slot, which is why writing \
         through the transport cannot move radar's timeline",
    );
    assert_eq!(rustdar_egui::radar_layer::site(ls), "KTLX");
    assert_eq!(rustdar_egui::radar_layer::coords(ls), (35.33, -97.27));
    assert_eq!(ls.phase, rustdar_egui::pane::LoopPhase::FetchingScanList);
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
    assert_eq!(ls.view, rustdar_radar::types::RenderView::PlanView);
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
            .loop_state()
            .is_active(),
    );
}

// ── 3. Disable is the mirror of enable ────────────────────────────────────

/// **Turning a loop off addresses the layer turning it on addressed.**
///
/// `handle_disable_loop` used to reset `loop_state_mut()` by name. On a pane
/// whose transport had moved that cleared a timeline nobody had armed and left
/// the running one running — and the ∞ button reads
/// `transport_state().is_active()`, so it stayed lit and re-emitted this same
/// action on every further click.
///
/// The radar slot is armed here as well, so the test can tell "disabled the
/// right one" from "disabled everything".
///
/// **Floor, run and observed:** put `loop_state_mut()` back in
/// `handle_disable_loop` and the transport layer is still active.
#[test]
fn disabling_a_loop_stops_the_layer_the_transport_addresses() {
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
        *pane.loop_state_mut() = rustdar_egui::radar_layer::begin_loop(
            600,
            &rustdar_radar::sites::RadarSite {
                name: "KOUN",
                network: rustdar_radar::sites::RadarNetwork::of_id("KOUN"),
                lat: 35.23,
                lon: -97.46,
                heights: None,
            },
            rustdar_radar::types::RenderView::PlanView,
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
        pane.loop_state().is_active(),
        "radar's own loop was torn down by a disable that was not about it",
    );
    assert_eq!(
        rustdar_egui::radar_layer::site(pane.loop_state()),
        "KOUN",
        "and it is the same radar loop it was",
    );
}
