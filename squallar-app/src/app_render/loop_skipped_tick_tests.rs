//! **A playback tick that wanted a frame and did not get one is counted.**
//!
//! `advance_loop_playback` scans forward from the frame the clock is on for
//! the first frame holding a picture and stamps `last_advance`
//! *unconditionally* — there is no branch that waits for the frame that was
//! due. So a loop whose frames arrive slower than it plays them does not slow
//! down and does not log: it plays a thinned set at full wall-clock speed.
//! That is decimation, which the frame-density ruling forbids, and until
//! `crate::loop_telemetry::SkippedTicks` nothing in the process counted it.
//! The `loop state:` line's other figures cannot see it — `resident` against
//! `listed` is a LEVEL and reads healthy on the ticks either side of a skip.
//!
//! **Both arms are driven, and the over-firing one is the dangerous one.** A
//! counter that fires on a healthy loop is worse than no counter: it would put
//! a permanent non-zero on every field reading and make the real signal
//! unreadable. [`a_loop_whose_frames_are_all_ready_never_counts_a_skip`] and
//! [`a_single_frame_loop_repeating_itself_is_not_a_skip`] are that arm.
//!
//! **And the frame set is never reduced.** The counter observes; it does not
//! decide. [`counting_a_skipped_tick_does_not_change_the_frame_set`] holds
//! that: lookback and frame density are tier 1, so a change here that made a
//! loop shorter or thinner would be the defect the counter exists to report.
//!
//! The arrangement is the shipping radar path — an untouched pane's transport
//! is radar — built the way `loop_playback_transport_tests` builds it, with
//! `image: None` on the frames that stand for a picture that has not arrived.

use squallar_egui::pane::{LayerTimeState, LoopFrame, LoopPhase, TimeMode};

use super::loop_overlay_render_tests::app_with_frames;
use super::loop_playback_transport_tests::textured_frames;

/// Five minutes apart, oldest first — a radar loop's spacing.
fn stamps(n: usize) -> Vec<chrono::NaiveDateTime> {
    let base = chrono::Utc::now().naive_utc();
    let minute = chrono::Duration::minutes(5);
    (0..n)
        .map(|i| base - minute * (n as i32 - i as i32))
        .collect()
}

/// A radar timeline over `at`, holding a picture only on the indices in
/// `ready`. A frame with `image: None` is a frame whose render has not landed
/// — listed, named, and not yet drawable, which is exactly the state the tick
/// scans past.
fn frames_ready_at(
    ctx: &egui::Context,
    at: &[chrono::NaiveDateTime],
    ready: &[usize],
) -> Vec<LoopFrame> {
    let mut frames = textured_frames(ctx, at);
    for (i, frame) in frames.iter_mut().enumerate() {
        if !ready.contains(&i) {
            frame.image = None;
        }
    }
    frames
}

/// A headless app whose pane 0 plays a radar loop over `at`, with pictures on
/// `ready` and the clock parked on `from`.
fn app_playing(
    ctx: &egui::Context,
    at: &[chrono::NaiveDateTime],
    ready: &[usize],
    from: usize,
) -> crate::app::App {
    let (mut app, _asked) = app_with_frames(Vec::new());
    {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        let ls = pane.transport_state_mut();
        *ls = LayerTimeState::begin(
            3600,
            squallar_radar::types::RenderView::PlanView,
            Box::new(()),
        );
        ls.frames = frames_ready_at(ctx, at, ready);
        ls.phase = LoopPhase::Playing;
    }
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .set_time_mode(TimeMode::AsOf(at[from]));
    app
}

/// One tick with the interval already elapsed. `last_advance` is cleared
/// rather than slept through: the interval is wall-clock and a test that
/// waited for it would be asserting the clock instead of the property.
fn tick(app: &mut crate::app::App) {
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .transport_state_mut()
        .last_advance = None;
    app.advance_loop_playback();
}

/// The stamps pane 0's transport lists, in order — the frame SET, which is
/// what tier 1 protects.
fn listed(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
    app.gui
        .pane(0)
        .expect("pane 0")
        .transport_state()
        .frames
        .iter()
        .map(|f| f.timestamp)
        .collect()
}

// ── The claim ───────────────────────────────────────────────────────────────

/// **A tick that lands past the frame it was due to show is counted, and
/// attributed.**
///
/// Four frames, pictures on 0 and 2 only — the shape of a loop whose renders
/// are arriving at half the rate it plays. Parked on 0:
///
/// * tick 1 is due frame 1, which has no picture, and lands on 2. One skip.
/// * tick 2 is due frame 3, which has no picture, and wraps to 0. Two.
/// * tick 3 is due frame 1 again. Three.
///
/// So three ticks over a half-filled loop show two of its four frames at full
/// speed, and the count is 3 rather than 0. **The figure is TICKS**: tick 2
/// scanned past two slots (3 and, on the wrap, nothing before 0) and counts
/// once, the same as tick 1 which scanned past one.
///
/// **The tamper**: drop the `if landed_at != Some(1)` arm from
/// `advance_loop_playback`. The count reads 0 and this fails naming it —
/// which is the state of the tree before the counter, expressed as a figure.
#[test]
fn a_tick_that_lands_past_the_frame_it_was_due_is_counted() {
    let ctx = egui::Context::default();
    let at = stamps(4);
    let mut app = app_playing(&ctx, &at, &[0, 2], 0);

    for _ in 0..3 {
        tick(&mut app);
    }

    assert_eq!(
        app.loop_skips.total(),
        3,
        "three playback ticks over a loop holding two of its four pictures \
         showed a thinned set at full speed and none of them was counted; \
         this is the silent decimation the counter exists to make visible",
    );
    let attributed: Vec<(usize, u64)> = app.loop_skips.attributed().collect();
    assert_eq!(
        attributed,
        vec![(0, 3)],
        "the skips were counted but not attributed to pane 0, so a degraded \
         loop can be detected and not located",
    );
}

/// **A tick that finds no picture anywhere is counted too** — the worse case,
/// and the one a `landed_at != Some(1)` test has to reach through `None`
/// rather than through a larger offset.
///
/// A loop whose frames are all listed and none rendered ticks on for ever,
/// stamping its clock every interval and showing nothing.
#[test]
fn a_tick_that_finds_no_picture_at_all_is_counted() {
    let ctx = egui::Context::default();
    let at = stamps(4);
    let mut app = app_playing(&ctx, &at, &[], 0);

    for _ in 0..5 {
        tick(&mut app);
    }

    assert_eq!(
        app.loop_skips.total(),
        5,
        "a loop with no pictures at all ticked five times and reported \
         nothing; `landed_at` is None on every one of them",
    );
}

// ── The over-firing arm ─────────────────────────────────────────────────────

/// **A healthy loop counts ZERO.** The direction that matters more than the
/// one above: a counter that fires on a full loop puts a rising number on
/// every field reading and buries the signal it exists to carry.
///
/// Every frame holds a picture, so every tick is due the next frame and gets
/// it — offset 1, all the way round including the wrap from last to first.
#[test]
fn a_loop_whose_frames_are_all_ready_never_counts_a_skip() {
    let ctx = egui::Context::default();
    let at = stamps(4);
    let mut app = app_playing(&ctx, &at, &[0, 1, 2, 3], 0);

    // More ticks than frames, so the wrap is walked twice.
    for _ in 0..9 {
        tick(&mut app);
    }

    assert_eq!(
        app.loop_skips.total(),
        0,
        "a fully rendered loop reported skipped ticks. Every tick landed on \
         the frame it was due, including both wraps, so this counter fires on \
         healthy playback and no field reading of it can be believed",
    );
}

/// **A one-frame loop repeating itself is not a skip.** The boundary the
/// modular arithmetic decides: with `num_frames == 1` the only candidate
/// offset is 1, which wraps to the frame the clock is already on. That is the
/// loop playing its whole set, not skipping any of it.
#[test]
fn a_single_frame_loop_repeating_itself_is_not_a_skip() {
    let ctx = egui::Context::default();
    let at = stamps(1);
    let mut app = app_playing(&ctx, &at, &[0], 0);

    for _ in 0..4 {
        tick(&mut app);
    }

    assert_eq!(
        app.loop_skips.total(),
        0,
        "a one-frame loop showing its one frame counted a skip on every tick",
    );
}

// ── The dangerous direction ─────────────────────────────────────────────────

/// **Counting a skipped tick does not shorten or thin the loop.**
///
/// Lookback and frame density are tier 1: a loop refuses rather than decimates.
/// The counter is an observation and must stay one, so this holds the frame
/// SET — every stamp, in order, listed and unlisted alike — identical across
/// playback that counts skips on most of its ticks.
///
/// Asserted as the stamp list rather than as a length, because a length is
/// preserved by a substitution: a pass that dropped an unrendered frame and
/// re-listed a rendered one twice would keep `frames.len()` and would be
/// exactly the thinning the ruling forbids.
///
/// **The unrendered frames are named in the assertion.** They are the ones a
/// pass under memory pressure would be tempted to drop — they hold no picture
/// and look like slack — and they are precisely the frames whose absence
/// would be silent decimation.
#[test]
fn counting_a_skipped_tick_does_not_change_the_frame_set() {
    let ctx = egui::Context::default();
    let at = stamps(6);
    let mut app = app_playing(&ctx, &at, &[0, 3], 0);

    let before = listed(&app);
    assert_eq!(
        before, at,
        "premise: the fixture must list all six frames, or this test could \
         pass on a loop that was already short",
    );

    for _ in 0..12 {
        tick(&mut app);
    }

    assert!(
        app.loop_skips.total() > 0,
        "premise: this arrangement must actually skip, or the property below \
         is asserted over playback that never exercised the new arm",
    );
    assert_eq!(
        listed(&app),
        at,
        "playback changed the frame set. Lookback and frame density are tier \
         1 — a loop refuses rather than decimates — and the four frames with \
         no picture yet are exactly the ones whose silent removal this \
         counter exists to report, not to perform",
    );
    assert_eq!(
        app.gui
            .pane(0)
            .expect("pane 0")
            .transport_state()
            .frames
            .iter()
            .filter(|f| f.image.is_none())
            .count(),
        4,
        "the frames still awaiting a picture were dropped from the list, so \
         the loop is now shorter than the span the user asked for",
    );
}
