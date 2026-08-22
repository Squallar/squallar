//! **The other half of WI-3, on the wire.**
//!
//! WI-3 stopped a looping pane presenting a frame valid *after* the instant its
//! clock names. What it could not do alone is get the data for that instant:
//! the frames came from one listing captured at enable time and nothing ever
//! widened it, so "draws nothing" was permanent. These observe the ask leaving
//! the shell, at the one place it does — the `create_frame_list_task` call —
//! and, in the same breath, that the pane is **still drawing nothing** while it
//! is in the air.
//!
//! The rules the dispatch obeys are pinned separately in `loop_refill/tests.rs`;
//! a failure there names the rule, a failure here names the wire.

use super::*;
use crate::app::App;
use rustdar_source::handler::{FetchTask, FrameListingResult, PaneRef};
use rustdar_source::id::{LayerId, known};
use rustdar_source::time::{FrameListing, TimeAxis};
use std::sync::{Arc, Mutex};

type Ranges = Arc<Mutex<Vec<(NaiveDateTime, NaiveDateTime)>>>;

/// Records every range it is asked to list, synchronously, before any future
/// is spawned — so nothing here depends on an executor running.
struct RangeRecorder {
    id: LayerId,
    seen: Ranges,
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
            typical_step: std::time::Duration::from_secs(300),
            extends_future: false,
        }
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

const SPAN: u64 = 3600;

fn ts(m: i64) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 5, 1)
        .expect("a real date")
        .and_hms_opt(12, 0, 0)
        .expect("a real time")
        + chrono::Duration::minutes(m)
}

fn frame(m: i64) -> rustdar_egui::pane::LoopFrame {
    rustdar_egui::pane::LoopFrame {
        timestamp: ts(m),
        image: None,
        render_in_flight: false,
        render_failed: false,
    }
}

/// A one-pane app looping `id` over `frames`, with a recorder registered under
/// that id and under nothing else.
fn app_looping(id: &LayerId, frames: &[i64]) -> (App, Ranges) {
    use rustdar_overlays::render::overlay_state::OverlayRegistry;

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    let seen: Ranges = Default::default();
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(RangeRecorder {
        id: id.clone(),
        seen: Arc::clone(&seen),
    })]);
    let pane = app.gui.pane_mut(0).expect("the fixture has one pane");
    pane.set_transport_layer(id.clone());
    let ls = pane.transport_state_mut();
    ls.phase = rustdar_egui::pane::LoopPhase::Ready;
    ls.span_secs = SPAN;
    ls.frames = frames.iter().copied().map(frame).collect();
    (app, seen)
}

/// Run the pump's refill row `passes` times at 60 fps, starting `from`.
fn pump(app: &mut App, from: web_time::Instant, passes: u32) {
    for pass in 0..passes {
        app.refill_unserved_loop_windows(from + std::time::Duration::from_millis(16) * pass);
    }
}

/// Long enough that one more pass counts the instant as settled.
fn settled() -> std::time::Duration {
    crate::loop_refill::REFILL_SETTLE + std::time::Duration::from_millis(1)
}

/// **Every read of the recorder goes through here, and the guard is dropped
/// before the value is used.**
///
/// Not tidiness: `assert!`'s message arguments are evaluated *while the
/// temporaries in its condition are still alive*, so a second `seen.lock()` in
/// the failure message deadlocks the test instead of failing it — a floor that
/// hangs rather than reddens, which is a floor that cannot be trusted. Found
/// by mutation, not by review.
fn recorded(seen: &Ranges) -> Vec<(NaiveDateTime, NaiveDateTime)> {
    seen.lock().expect("no poisoned lock").clone()
}

/// **The acceptance.** A looping pane scrubbed to an instant before everything
/// it holds issues a listing request naming that instant.
///
/// The transport is a layer that is not radar, so nothing here can be satisfied
/// by radar's own scan plumbing: the ask is built from `transport_layer()` and
/// dispatched through the generic `create_frame_list_task`.
#[test]
fn a_pane_scrubbed_before_its_loop_window_asks_that_layer_for_that_instant() {
    let id = LayerId::new("test/transport");
    let (mut app, seen) = app_looping(&id, &[60, 70, 80]);

    let pane = app.gui.pane_mut(0).expect("one pane");
    pane.set_time_mode(rustdar_egui::pane::TimeMode::AsOf(ts(30)));
    assert_eq!(
        pane.transport_state().qualifying_frame_at(pane.time.mode),
        None,
        "premise: this is WI-3's blank - no frame this pane holds is valid at 12:30",
    );

    let t0 = web_time::Instant::now();
    app.refill_unserved_loop_windows(t0);
    assert!(
        recorded(&seen).is_empty(),
        "the first sight of an instant is not yet a question",
    );

    app.refill_unserved_loop_windows(t0 + settled());
    let ranges = recorded(&seen);
    assert_eq!(
        ranges.len(),
        1,
        "a settled clock over a hole must produce exactly one listing request",
    );
    assert_eq!(
        ranges[0],
        (ts(30) - chrono::Duration::seconds(SPAN as i64), ts(30)),
        "and it must name the instant asked about, one span wide, ending there",
    );
}

/// **The trap.** The pane must still draw NOTHING while the request is in the
/// air.
///
/// A "fix" that reinstates the nearest frame while fetching would look right on
/// screen and be precisely the fabrication WI-3 removed — a picture from 13:00
/// captioned 12:30. Asserted on the presentational accessors, which are the
/// ones that reach the glass.
#[test]
fn the_pane_draws_nothing_while_the_refill_is_in_the_air() {
    let id = known::RADAR;
    let (mut app, seen) = app_looping(&id, &[60, 70, 80]);

    let pane = app.gui.pane_mut(0).expect("one pane");
    pane.set_time_mode(rustdar_egui::pane::TimeMode::AsOf(ts(30)));

    let t0 = web_time::Instant::now();
    app.refill_unserved_loop_windows(t0);

    // **Before the request, with the frames still in hand.** This is where a
    // nearest-frame fallback would be visible and where it must not be: the
    // pane is holding 13:00, 13:10 and 13:20 and is being asked about 12:30.
    let pane = app.gui.pane(0).expect("one pane");
    assert_eq!(
        pane.transport_state().frames.len(),
        3,
        "premise: the frames the fallback would reach for are right there",
    );
    assert_eq!(
        pane.transport_state().playhead_stamp(),
        None,
        "a frame valid AFTER the instant asked about is not an answer for it",
    );
    assert!(
        pane.active_image().is_none(),
        "and it does not reach the glass while a refill is being decided",
    );

    app.refill_unserved_loop_windows(t0 + settled());
    assert_eq!(
        recorded(&seen).len(),
        1,
        "premise: the request went out, so what follows is the in-flight state",
    );

    let pane = app.gui.pane(0).expect("one pane");
    assert_eq!(
        pane.transport_state().playhead_stamp(),
        None,
        "no stamp is named while the answer is in the air",
    );
    assert_eq!(
        pane.transport_state().qualifying_frame(),
        None,
        "and no frame is presented",
    );
    assert!(
        pane.active_image().is_none(),
        "and nothing reaches the glass - a nearest-frame fallback here is the \
         exact bug WI-3 removed",
    );
    assert_eq!(
        pane.data_time_on_screen(),
        None,
        "and nothing is captioned, because nothing is on screen",
    );
}

/// **Non-triviality: an answered clock asks for nothing**, however much it
/// moves.
///
/// Twenty in-window instants swept at 60 fps and then parked well past the
/// settle — the same 21 observations the acceptance needed to produce one
/// question — produce none. Radar's existing archive scrub is exactly this
/// shape, so "ask on every clock move" cannot pass both this and the test
/// above.
#[test]
fn a_clock_the_loop_answers_for_never_asks_however_far_it_travels() {
    let id = known::RADAR;
    let (mut app, seen) = app_looping(&id, &[60, 70, 80]);

    let t0 = web_time::Instant::now();
    for step in 0..20i64 {
        app.gui
            .pane_mut(0)
            .expect("one pane")
            .set_time_mode(rustdar_egui::pane::TimeMode::AsOf(ts(60 + step)));
        app.refill_unserved_loop_windows(t0 + std::time::Duration::from_millis(16) * step as u32);
    }
    // Parked, long past the settle.
    app.refill_unserved_loop_windows(t0 + settled() * 4);

    let asked = recorded(&seen);
    assert!(
        asked.is_empty(),
        "an answered clock asked {} time(s) across 21 observations",
        asked.len(),
    );
    let pane = app.gui.pane(0).expect("one pane");
    assert_eq!(
        pane.transport_state().frames.len(),
        3,
        "and its frames are untouched",
    );
    assert!(
        pane.transport_state().is_render_ready(),
        "and its loop is still the settled loop it was",
    );
}

/// **Bounded.** An absurd scrub target — thirty years before anything held —
/// asks one question over one span, not a backfill of the gap.
///
/// The distance travelled never appears in the size of the question: this range
/// is the same width as the acceptance test's, which reached back thirty
/// minutes.
#[test]
fn an_absurd_scrub_target_asks_for_one_span_and_not_the_gap() {
    let id = LayerId::new("test/transport");
    let (mut app, seen) = app_looping(&id, &[60, 70, 80]);

    let far = ts(60) - chrono::Duration::days(365 * 30);
    app.gui
        .pane_mut(0)
        .expect("one pane")
        .set_time_mode(rustdar_egui::pane::TimeMode::AsOf(far));

    let t0 = web_time::Instant::now();
    pump(&mut app, t0, 1);
    app.refill_unserved_loop_windows(t0 + settled());

    let ranges = recorded(&seen);
    assert_eq!(ranges.len(), 1, "one question, not a walk back to it");
    let (start, end) = ranges[0];
    assert_eq!(end, far, "ending at the instant asked about");
    assert_eq!(
        (end - start).num_seconds(),
        SPAN as i64,
        "and exactly one span wide - a range that reached from {far} to the \
         loop's own window would be 30 years of listing",
    );
}

/// **And bounded in count.** Six hundred pump passes over one unanswerable
/// instant — ten seconds at 60 fps — issue one request, not six hundred.
///
/// The condition stays true forever when the source genuinely has nothing
/// there, which is what makes the mark load-bearing rather than an
/// optimisation.
#[test]
fn six_hundred_passes_over_one_hole_issue_one_request() {
    let id = LayerId::new("test/transport");
    let (mut app, seen) = app_looping(&id, &[60, 70, 80]);
    app.gui
        .pane_mut(0)
        .expect("one pane")
        .set_time_mode(rustdar_egui::pane::TimeMode::AsOf(ts(30)));

    pump(&mut app, web_time::Instant::now(), 600);

    let asked = recorded(&seen);
    assert_eq!(
        asked.len(),
        1,
        "600 passes over one unserved instant issued {} request(s)",
        asked.len(),
    );
}

/// The handoff: a refilled loop is back in the phase `accept_loop_scan_listings`
/// looks for, with the very window it asked for recorded, so the listing that
/// lands is the one that builds its frames.
///
/// Pinned because the acceptance path is not this item's — it matches a landed
/// listing on `(phase, asked window, site)` and nothing else, and a refill that
/// left any of the three wrong would ask a question whose answer is dropped.
/// The window is the whole range and not its width: a refill's window is the
/// same width as the live window it replaces, anchored at the scrub target,
/// and width-matching filed one pane's era into another pane's loop.
#[test]
fn a_refilled_loop_waits_in_the_phase_the_listing_path_looks_for() {
    let id = known::RADAR;
    let (mut app, seen) = app_looping(&id, &[60, 70, 80]);
    app.gui
        .pane_mut(0)
        .expect("one pane")
        .set_time_mode(rustdar_egui::pane::TimeMode::AsOf(ts(30)));

    let t0 = web_time::Instant::now();
    app.refill_unserved_loop_windows(t0);
    app.refill_unserved_loop_windows(t0 + settled());

    let ls = app.gui.pane(0).expect("one pane").transport_state();
    assert_eq!(
        ls.phase,
        rustdar_egui::pane::LoopPhase::FetchingScanList,
        "a refilling loop waits in the listing phase",
    );
    assert_eq!(ls.span_secs, SPAN, "over the span it asked for",);
    let asked = recorded(&seen);
    assert_eq!(asked.len(), 1, "one refill, one question: {asked:?}");
    assert_eq!(
        ls.asked_range,
        Some(asked[0]),
        "recording the very window it put on the wire, which is how the \
         arrival is matched to it — the width alone cannot tell this ask \
         from the live window it replaced",
    );
    assert!(
        ls.frames.is_empty(),
        "holding no frames - the window it asked for cannot overlap the one it \
         held, because the unserved instant is earlier than all of it",
    );
    assert!(
        ls.listing_since.is_some(),
        "and its wait is stamped, as a freshly enabled loop's is",
    );
}
