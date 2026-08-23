//! **The decision, on its own.** Which instants count as unserved, how wide a
//! window is asked for, and how many questions a moving clock produces.
//!
//! The dispatch these feed — the real `create_frame_list_task` call — is
//! `app_fetch/loop_refill_dispatch_tests.rs`. Split deliberately: the rules
//! below hold with no registry, no network and no `App`, so a failure here
//! names the rule and a failure there names the wire.

use super::*;
use rustdar_egui::pane::{LoopFrame, LoopPhase, PaneState};
use rustdar_overlays::render::overlay_state::OverlayRegistry;
use rustdar_source::id::LayerId;

/// **A registry that registers nothing**, which is what these rules are stated
/// against: every id is unknown to it, so `residency_for` answers
/// [`Residency::none`] and the window asked for is the span alone. The
/// widening a layer's own answer can do is pinned next door in
/// `app_fetch/loop_refill_dispatch_tests.rs`, where there is a handler to
/// answer it.
fn no_handlers() -> OverlayRegistry {
    OverlayRegistry::with_handlers(Vec::new())
}

/// [`refill_range`] as these tests mean it: no layer answering, so the window
/// is exactly one span ending at the instant.
fn span_range(span_secs: u64, instant: NaiveDateTime) -> (NaiveDateTime, NaiveDateTime) {
    refill_range(&Residency::none(), span_secs, instant)
}

/// Minute `m` of a fixed day. Absolute values never appear in an assertion —
/// only differences do — so the date is arbitrary and stated once.
fn ts(m: i64) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 5, 1)
        .expect("a real date")
        .and_hms_opt(12, 0, 0)
        .expect("a real time")
        + chrono::Duration::minutes(m)
}

const SPAN: u64 = 3600;

fn frame(m: i64) -> LoopFrame {
    LoopFrame {
        timestamp: ts(m),
        image: None,
        render_in_flight: false,
        render_failed: false,
    }
}

/// A pane whose transport layer is `id`, holding a settled loop over `frames`.
fn looping_pane(id: &LayerId, frames: &[i64]) -> PaneState {
    let mut pane = PaneState::new();
    pane.set_transport_layer(id.clone());
    let ls = pane.transport_state_mut();
    ls.phase = LoopPhase::Ready;
    ls.span_secs = SPAN;
    ls.frames = frames.iter().copied().map(frame).collect();
    pane
}

fn a_layer() -> LayerId {
    LayerId::new("test/transport")
}

/// **The one case there is**: the clock is earlier than the oldest frame the
/// transport layer holds.
#[test]
fn a_clock_before_every_frame_names_the_instant_nothing_answers_for() {
    let id = a_layer();
    let mut pane = looping_pane(&id, &[60, 70, 80]);
    pane.set_time_mode(TimeMode::AsOf(ts(30)));

    assert_eq!(
        pane.transport_state().qualifying_frame_at(pane.time.mode),
        None,
        "premise: WI-3's rule says this pane draws nothing",
    );
    assert_eq!(
        unserved_instant(&pane),
        Some(ts(30)),
        "and the instant it draws nothing FOR is what must be asked about",
    );
}

/// **Non-triviality, and the one that stops this becoming "fetch on every
/// clock move".** Four clocks a settled loop answers for, each silent.
///
/// The clock after the newest frame is the interesting one: `FrameSeries`
/// presents the latest frame at or before the clock, so running off the new
/// end is *not* a hole and must not be refilled.
#[test]
fn a_clock_a_frame_answers_asks_for_nothing() {
    let id = a_layer();
    let cases: [(TimeMode, &str); 4] = [
        (TimeMode::AsOf(ts(60)), "exactly on the oldest frame"),
        (TimeMode::AsOf(ts(75)), "between two frames"),
        (
            TimeMode::AsOf(ts(9999)),
            "past the newest frame - still answered by the newest",
        ),
        (TimeMode::Live, "live is 'the newest there is'"),
    ];
    for (mode, why) in cases {
        let mut pane = looping_pane(&id, &[60, 70, 80]);
        pane.set_time_mode(mode);
        assert_eq!(unserved_instant(&pane), None, "{why}");
    }
}

/// A loop already being supplied is not asked again — the phases are the
/// dedupe against a refill re-asking for its own in-flight window every frame.
#[test]
fn a_loop_still_being_built_is_not_a_hole() {
    let id = a_layer();
    for phase in [
        LoopPhase::FetchingScanList,
        LoopPhase::Rendering,
        LoopPhase::Inactive,
    ] {
        let mut pane = looping_pane(&id, &[60, 70, 80]);
        pane.transport_state_mut().phase = phase;
        pane.set_time_mode(TimeMode::AsOf(ts(30)));
        assert_eq!(
            unserved_instant(&pane),
            None,
            "a loop in {phase:?} is being supplied already",
        );
    }
    // And the positive control on the same fixture, so the four `None`s above
    // cannot be an empty walk.
    let mut pane = looping_pane(&id, &[60, 70, 80]);
    pane.transport_state_mut().phase = LoopPhase::Paused;
    pane.set_time_mode(TimeMode::AsOf(ts(30)));
    assert_eq!(
        unserved_instant(&pane),
        Some(ts(30)),
        "control: a settled loop on the same clock IS a hole",
    );
}

/// **The bound, stated as arithmetic**: one span, ending at the instant. The
/// distance from the loaded window never appears.
#[test]
fn the_window_asked_for_is_one_span_ending_at_the_instant() {
    for reach_mins in [1i64, 60, 60 * 24 * 365 * 30] {
        let target = ts(60) - chrono::Duration::minutes(reach_mins);
        let (start, end) = span_range(SPAN, target);
        assert_eq!(end, target, "the window ends at the instant asked about");
        assert_eq!(
            (end - start).num_seconds(),
            SPAN as i64,
            "and is exactly one span wide however far back {target} is",
        );
    }
}

/// **No thrash, with its denominator.** A clock swept across 20 distinct
/// unserved instants at 60 fps, then parked: **21 observations, 1 question.**
///
/// The sweep is the shape a keyboard nudge held down makes — the drag itself
/// never moves the pane clock, `ui_timeline::commit_archive_scrub` only fires
/// on release — and each step re-arms the settle, so nothing goes out until
/// the hand stops.
#[test]
fn a_clock_sweeping_twenty_instants_asks_once() {
    let id = a_layer();
    let mut pane = looping_pane(&id, &[60, 70, 80]);
    let mut watch = LoopRefillWatch::default();
    let t0 = web_time::Instant::now();
    let frame_time = std::time::Duration::from_millis(16);

    let mut asks = Vec::new();
    for step in 0..20i64 {
        pane.set_time_mode(TimeMode::AsOf(ts(step - 30)));
        asks.extend(watch.settled_asks(
            std::slice::from_mut(&mut pane),
            &no_handlers(),
            t0 + frame_time * (step as u32),
        ));
    }
    assert!(
        asks.is_empty(),
        "a clock still travelling asked {} time(s) across 20 instants in \
         {frame_time:?} steps",
        asks.len(),
    );

    // The hand stops. One more pass — the twenty-first observation — a settle
    // later.
    asks.extend(watch.settled_asks(
        std::slice::from_mut(&mut pane),
        &no_handlers(),
        t0 + frame_time * 20 + REFILL_SETTLE,
    ));
    assert_eq!(
        asks.len(),
        1,
        "21 observations across 20 distinct unserved instants must produce \
         exactly one question, and produced {}",
        asks.len(),
    );
    assert_eq!(
        asks[0].range,
        span_range(SPAN, ts(19 - 30)),
        "and it is the instant the clock STOPPED on, not one it passed through",
    );
}

/// **No thrash, the other denominator.** A pane parked on a hole the source
/// genuinely cannot fill: **600 pump passes — ten seconds at 60 fps — one
/// question.**
///
/// This is the case a bare "ask when nothing qualifies" gets wrong: the
/// condition stays true forever, so without the mark it is a listing per
/// frame.
#[test]
fn a_pane_parked_on_a_hole_asks_once_across_six_hundred_passes() {
    let id = a_layer();
    let mut pane = looping_pane(&id, &[60, 70, 80]);
    pane.set_time_mode(TimeMode::AsOf(ts(30)));
    let mut watch = LoopRefillWatch::default();
    let t0 = web_time::Instant::now();

    let mut asks = Vec::new();
    for pass in 0..600u32 {
        asks.extend(watch.settled_asks(
            std::slice::from_mut(&mut pane),
            &no_handlers(),
            t0 + std::time::Duration::from_millis(16) * pass,
        ));
    }
    assert_eq!(
        asks.len(),
        1,
        "600 passes over one unserved instant produced {} question(s)",
        asks.len(),
    );
}

/// Scrubbing back into the window and out again asks again — the mark is per
/// instant, not per pane for the life of the loop.
#[test]
fn leaving_the_hole_and_returning_asks_again() {
    let id = a_layer();
    let mut pane = looping_pane(&id, &[60, 70, 80]);
    let mut watch = LoopRefillWatch::default();
    let t0 = web_time::Instant::now();
    let settled = REFILL_SETTLE + std::time::Duration::from_millis(1);

    pane.set_time_mode(TimeMode::AsOf(ts(30)));
    let mut asks = watch.settled_asks(std::slice::from_mut(&mut pane), &no_handlers(), t0);
    asks.extend(watch.settled_asks(
        std::slice::from_mut(&mut pane),
        &no_handlers(),
        t0 + settled,
    ));
    assert_eq!(asks.len(), 1, "the first hole asks once");

    // Back inside the window, then out to the same instant again.
    pane.set_time_mode(TimeMode::AsOf(ts(75)));
    let quiet = watch.settled_asks(
        std::slice::from_mut(&mut pane),
        &no_handlers(),
        t0 + settled * 2,
    );
    assert!(quiet.is_empty(), "an answered clock asks for nothing");

    pane.set_time_mode(TimeMode::AsOf(ts(30)));
    let mut again = watch.settled_asks(
        std::slice::from_mut(&mut pane),
        &no_handlers(),
        t0 + settled * 3,
    );
    again.extend(watch.settled_asks(
        std::slice::from_mut(&mut pane),
        &no_handlers(),
        t0 + settled * 5,
    ));
    assert_eq!(again.len(), 1, "returning to the same hole asks again");
}

/// The ask names the transport layer, whatever that layer is — the whole point
/// of reading through `transport_state` rather than through radar by name.
#[test]
fn the_ask_names_the_panes_own_transport_layer() {
    let id = LayerId::new("test/some-forecast");
    let mut pane = looping_pane(&id, &[60]);
    pane.set_time_mode(TimeMode::AsOf(ts(30)));
    let mut watch = LoopRefillWatch::default();
    let t0 = web_time::Instant::now();
    let _ = watch.settled_asks(std::slice::from_mut(&mut pane), &no_handlers(), t0);
    let asks = watch.settled_asks(
        std::slice::from_mut(&mut pane),
        &no_handlers(),
        t0 + REFILL_SETTLE + std::time::Duration::from_millis(1),
    );
    assert_eq!(asks.len(), 1, "the hole is asked about");
    assert_eq!(asks[0].layer, id, "and it is asked of the transport layer");
}

/// **The acceptance, as a rule.** A pane animating two layers, scrubbed before
/// both loaded windows, asks for **both** — one ask per layer, each over that
/// layer's own span.
///
/// Before the walk landed this produced exactly one ask, naming the transport,
/// and the secondary timeline was never widened again: it went on holding
/// frames stamped after the clock, which `overlay_texture_on_screen` refuses to
/// draw, so the layer was blank for the rest of the loop's life.
#[test]
fn a_deep_scrub_refills_every_animating_layer_not_just_the_transport() {
    let transport = LayerId::new("test/transport");
    let secondary = LayerId::new("test/secondary");
    // Twelve times the transport's span, which is the satellite-under-radar
    // shape: the two windows are different widths and each layer must get its
    // own.
    const WIDE: u64 = SPAN * 12;

    let mut pane = looping_pane(&transport, &[60, 70, 80]);
    let ls = pane.time_state_mut(&secondary);
    ls.phase = LoopPhase::Ready;
    ls.span_secs = WIDE;
    ls.frames = [60i64, 120, 180].into_iter().map(frame).collect();
    pane.set_time_mode(TimeMode::AsOf(ts(30)));

    let mut watch = LoopRefillWatch::default();
    let t0 = web_time::Instant::now();
    let _ = watch.settled_asks(std::slice::from_mut(&mut pane), &no_handlers(), t0);
    let asks = watch.settled_asks(
        std::slice::from_mut(&mut pane),
        &no_handlers(),
        t0 + REFILL_SETTLE + std::time::Duration::from_millis(1),
    );

    let named: Vec<&LayerId> = asks.iter().map(|ask| &ask.layer).collect();
    assert!(
        named.contains(&&secondary),
        "the secondary timeline went blank and was never asked about: {named:?}",
    );
    assert!(
        named.contains(&&transport),
        "and the transport must not be traded away for it: {named:?}",
    );
    assert_eq!(asks.len(), 2, "one ask per unserved layer, and no more");

    let ask_for = |id: &LayerId| {
        asks.iter()
            .find(|ask| ask.layer == *id)
            .expect("just asserted present")
            .range
    };
    assert_eq!(
        ask_for(&transport),
        span_range(SPAN, ts(30)),
        "the transport is listed over the transport's own span",
    );
    assert_eq!(
        ask_for(&secondary),
        span_range(WIDE, ts(30)),
        "and the secondary over ITS span - handing it the transport's hour \
         would list a window its own frames are two hours apart inside",
    );
}

/// **The floor for the walk**: a pane animating exactly one layer asks exactly
/// what it asked before — one question, naming the transport, one span wide.
///
/// Without this, "ask every layer for every span there is" passes the
/// acceptance above.
#[test]
fn a_pane_animating_one_layer_asks_exactly_what_it_always_did() {
    let transport = LayerId::new("test/transport");
    let mut pane = looping_pane(&transport, &[60, 70, 80]);
    // A slot that exists and is NOT animating, so "one animating layer" is a
    // claim about the phase rather than about the pane having one slot.
    pane.time_state_mut(&LayerId::new("test/idle")).span_secs = SPAN * 9;
    pane.set_time_mode(TimeMode::AsOf(ts(30)));

    let mut watch = LoopRefillWatch::default();
    let t0 = web_time::Instant::now();
    let _ = watch.settled_asks(std::slice::from_mut(&mut pane), &no_handlers(), t0);
    let asks = watch.settled_asks(
        std::slice::from_mut(&mut pane),
        &no_handlers(),
        t0 + REFILL_SETTLE + std::time::Duration::from_millis(1),
    );

    assert_eq!(
        asks,
        vec![RefillAsk {
            pane_idx: 0,
            layer: transport,
            range: span_range(SPAN, ts(30)),
        }],
        "one animating layer, one ask, byte for byte the one this made before",
    );
}

/// **A layer that knows a frame before the window widens the ask back to it**,
/// and the width is the only thing that moves.
///
/// The window one span wide names its own first stop, and that stop is drawn
/// from the frame at or before it — which for a layer coarser than the scrub
/// sits earlier than the window. Clipping the listing at the span's edge is
/// what leaves the first step of a sweep blank.
#[test]
fn a_layers_own_residency_widens_the_window_and_never_narrows_it() {
    let target = ts(600);
    // The layer's own answer: the frame at ts(0) is what the window's first
    // stop is drawn from, and it sits nine hours before the span's edge.
    let hold = Residency::over([(ts(0), target)]);

    assert_eq!(
        refill_range(&hold, SPAN, target),
        (ts(0), target),
        "the ask reaches back to the frame the window's own first stop is \
         drawn from",
    );
    // The floor: a residency INSIDE the span cannot shrink the ask.
    let inside = Residency::over([(target - chrono::Duration::minutes(1), target)]);
    assert_eq!(
        refill_range(&inside, SPAN, target),
        span_range(SPAN, target),
        "a layer answering for less than the span still gets the whole span",
    );
}
