//! **The budget readout is composed on its consumer's cadence, not the
//! frame's.**
//!
//! The readout is a set of *levels* — what each pane's stores hold, what each
//! pool has spare — and the one thing that reads it is
//! `budget_telemetry::budget_state_line`, inside `App::report_frame_telemetry`,
//! which runs at most once per `RASTER_TELEMETRY_PERIOD`. Composing it on the
//! loop walk instead put a per-pane store walk, a mutex lock per 3D pane and a
//! structural compare across the Gui seam on every frame, to publish figures
//! nothing read more than every 2 s.
//!
//! **What a figures test would not have caught.** Every assertion about *what*
//! the readout says passes identically whether it is composed once a frame or
//! once a tick — that is exactly why the per-frame composition landed green.
//! What is asserted here is the count: the frame path composes zero times,
//! however many frames run, and the tick composes exactly one.
//!
//! The cadence is asked, never waited on: `telemetry_is_due` takes both
//! instants, so clearing the last-said stamp is what makes a tick due.

use crate::app::App;
use crate::app::tests::n_pane_app;

const SITE: &str = "KTLX";

/// Two seconds of 120 Hz — one whole telemetry period of frames, which is the
/// number the defect composed a readout on.
const FRAMES: usize = 240;

/// One telemetry tick, asked for rather than waited on.
fn tick(app: &mut App) {
    app.frame_telemetry_said = None;
    app.report_frame_telemetry();
}

/// Drive the frame path's loop walk `FRAMES` times over a scene that does not
/// move. This is `dispatch_loop_renders`' whole budget half.
fn frames(app: &mut App) {
    for _ in 0..FRAMES {
        let _ = app.observe_loop_demand();
    }
}

#[test]
fn a_static_scene_composes_the_readout_once_per_tick_and_never_once_per_frame() {
    let mut app = n_pane_app(2, SITE);
    assert_eq!(
        app.budget_readout.generation, 0,
        "precondition: a fresh application has composed no readout",
    );

    frames(&mut app);
    assert_eq!(
        app.budget_readout.generation, 0,
        "{FRAMES} frames over an unmoving scene composed the readout \
         {} time(s); composition is back on the frame thread",
        app.budget_readout.generation,
    );

    tick(&mut app);
    assert_eq!(
        app.budget_readout.generation, 1,
        "the tick that reads the readout did not compose one, so \
         `budget state:` prints whatever was last left there",
    );
    assert_eq!(
        app.budget_readout.panes.len(),
        2,
        "the composition has to be real, or the count above is vacuous",
    );

    // The second interval, so the assertion is about the cadence rather than
    // about first-call laziness.
    frames(&mut app);
    assert_eq!(
        app.budget_readout.generation, 1,
        "a second period of frames composed again between two ticks",
    );
    tick(&mut app);
    assert_eq!(
        app.budget_readout.generation, 2,
        "the second tick did not compose; the readout would age forever",
    );
}

/// **The tick is the only door, and it is a real one.** A tick that is not due
/// composes nothing — so the generation counts periods, not calls — and the
/// composition it does take is the one the line beside it reads.
#[test]
fn a_tick_inside_the_period_composes_nothing_and_the_line_reads_what_it_composed() {
    let mut app = n_pane_app(2, SITE);
    tick(&mut app);
    let composed = app.budget_readout.generation;
    assert_eq!(composed, 1, "precondition: the first tick composed one");

    // Not due: the stamp the tick just wrote is still inside the period.
    app.report_frame_telemetry();
    assert_eq!(
        app.budget_readout.generation, composed,
        "a tick inside the period composed a readout anyway, so the cadence \
         is the frame's again by another route",
    );

    // And what the consumer prints is the composition, not a default: two
    // pane groups, one per pane the readout was composed over.
    let line = crate::budget_telemetry::budget_state_line(
        &app.budgets,
        &app.device_profile,
        None,
        app.loop_pool.bytes(),
        app.loop_pool_state.allocation().balloon_bytes(),
        &app.capacity(),
        app.gpu_probe,
        crate::pressure::LinearMemoryWatch::default(),
        &app.budget_readout,
        // As the tick spells it, so the pane rows sit behind the same fixed
        // fields here as on the real line. `None` in this crate's tests: the
        // test binary installs no counting allocator.
        squallar_alloc::live_bytes(),
    );
    assert!(
        line.contains("pane0 gpu ") && line.contains("pane1 gpu "),
        "the consumer's line carries no pane rows, so nothing proves the \
         composition above reached it: {line}",
    );
}
