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
        app.loop_pool_state.allocation().over_pool_bytes(),
        &app.capacity(),
        app.gpu_probe,
        crate::pressure::LinearMemoryWatch::default(),
        &app.budget_readout,
        // As the tick spells it, so the pane rows sit behind the same fixed
        // fields here as on the real line. `None` in this crate's tests: the
        // test binary installs no counting allocator.
        squallar_alloc::live_bytes(),
        &crate::recovery::HostRecovery::untouched(),
        &app.gpu_recovery,
        squallar_egui::admission::Totals::default(),
        app.admission_costs.spare,
        None,
    );
    assert!(
        line.contains("pane0 gpu ") && line.contains("pane1 gpu "),
        "the consumer's line carries no pane rows, so nothing proves the \
         composition above reached it: {line}",
    );
}

/// **The layers menu's rows ride the same tick, and the frame path builds
/// none of them.**
///
/// The rows are one `Vec` per pane of one entry per layer, and the identities
/// behind them come off the same pane walk `dispatch_loop_renders` takes every
/// frame. Collecting them there would have been free to write and would have
/// put a per-pane allocation on the frame thread to publish a level nothing
/// reads more than every 2 s — the defect this module exists for, one land
/// later and in a second place. The flag `App::walk_panes` takes is what stops
/// it, and this is what holds the flag honest: the count, not the figures.
#[test]
fn the_layer_rows_are_composed_on_the_tick_and_never_on_a_frame() {
    let mut app = n_pane_app(2, SITE);

    frames(&mut app);
    assert!(
        app.budget_readout.pane_layers.is_empty(),
        "{FRAMES} frames over an unmoving scene composed {} pane(s) of layer \
         rows; the rows are back on the frame thread",
        app.budget_readout.pane_layers.len(),
    );

    tick(&mut app);
    assert_eq!(
        app.budget_readout.pane_layers.len(),
        2,
        "the tick composed no layer rows, so the layers menu would show the \
         figures of whatever scene was last there",
    );
    assert_eq!(
        app.budget_readout.pane_layers.len(),
        app.budget_readout.panes.len(),
        "the two vectors are indexed by the same pane index and the menu reads \
         one by the other's position",
    );
    // Non-vacuity: an empty row set would satisfy every count above.
    assert!(
        app.budget_readout.pane_layers[0]
            .iter()
            .any(|row| row.layer == squallar_source::id::known::RADAR),
        "a 2D pane is charged a static raster whatever its eye says, so its \
         radar row has to exist for the counts above to mean anything: {:?}",
        app.budget_readout.pane_layers[0],
    );

    let composed = app.budget_readout.generation;
    frames(&mut app);
    assert_eq!(
        app.budget_readout.generation, composed,
        "a second period of frames recomposed between two ticks",
    );
}

/// **The walk the frame path takes collects nothing**, and the tick's walk on
/// the same application collects something.
///
/// The test above shows the frame path never *publishes* rows; that would also
/// be true of a frame path that built them and dropped them, which is the whole
/// of the cost. So the subject here is `App::loop_demand` itself — the one
/// spelling `App::observe_loop_demand` and `App::scene_of` take — and
/// `walk_panes(true)` beside it is the control that makes the empty answer an
/// omission rather than an incapacity.
///
/// **Asserted on `loop_demand` and not on `walk_panes(false)`**, and the
/// difference is a tamper: pointing `loop_demand` at the collecting arm left a
/// `walk_panes(false)` assertion green, because that spelling is not the one
/// the frame path uses.
#[test]
fn the_walk_the_frame_path_takes_collects_no_layer_rows() {
    let app = n_pane_app(2, SITE);

    assert!(
        app.loop_demand_for_test().pane_layers.is_empty(),
        "the walk the frame path takes collected layer rows",
    );
    assert_eq!(
        app.walk_panes(true).pane_layers.len(),
        2,
        "the tick's walk collected no layer rows either, so the arm above \
         proves nothing",
    );
}

/// **A row's figures are the model's own terms, not a second arithmetic.**
///
/// The pane the readout is composed over is a still 2D pane: `fit` charges it
/// one static raster and one decoded volume at the reserve, and nothing else.
/// So its radar row's `own` is exactly `static_rasters` in the GPU pool — read
/// off the same `PaneTerms` the pane's own entry carries, which is what makes
/// "the readout divides `fit`'s terms" a checked claim rather than a comment.
#[test]
fn a_radar_rows_own_bytes_are_the_panes_own_priced_terms() {
    use squallar_device_profile::admit::Pool;

    let mut app = n_pane_app(1, SITE);
    tick(&mut app);

    let pane = app.budget_readout.panes[0];
    let radar = app.budget_readout.pane_layers[0]
        .iter()
        .find(|row| row.layer == squallar_source::id::known::RADAR)
        .expect("a 2D pane carries a radar row");

    assert!(
        pane.terms.static_rasters > 0,
        "precondition: `fit` charges a 2D pane a static raster",
    );
    let expected = match radar.charge.pool {
        // The pane runs no loop and draws no 3D grid, so radar's own bytes are
        // the static raster alone.
        Pool::Gpu => pane.terms.static_rasters,
        // On the host it is the still it is parked at.
        Pool::Host => pane.terms.loop_scans_host + pane.terms.still_scans_host,
        // One memory: the two above, summed the way `fit::over` sums them.
        Pool::Joint => {
            pane.terms.static_rasters + pane.terms.loop_scans_host + pane.terms.still_scans_host
        }
    };
    assert_eq!(
        radar.own_bytes, expected,
        "the radar row's own bytes are not the pane's own priced terms in the \
         pool the row is reported in ({:?})",
        radar.charge.pool,
    );
    assert_eq!(
        radar.charge.cost_bytes,
        radar.shared_bytes + radar.own_bytes,
        "a row's cost is the two figures beside it and nothing else",
    );
}

/// **The room a thing has is the pool's allowance less everything else**, and
/// the corollary that follows from it: a pane is over exactly when the scene
/// is. Both are asserted here because the readout's whole meaning rests on
/// them, and a difference of two figures is the kind of arithmetic that reads
/// right while being off by one term.
#[test]
fn a_panes_room_is_the_allowance_less_the_rest_of_the_scene() {
    let mut app = n_pane_app(2, SITE);
    tick(&mut app);

    let gpu = app.budget_readout.gpu;
    for (idx, pane) in app.budget_readout.panes.iter().enumerate() {
        if pane.charge.pool != squallar_device_profile::admit::Pool::Gpu {
            continue;
        }
        let allowed = pane.charge.allowed_bytes.expect("a known pool has a room");
        assert_eq!(
            allowed,
            gpu.allowance_bytes
                .saturating_sub(gpu.need_bytes.saturating_sub(pane.charge.cost_bytes)),
            "pane {idx}'s room is not the GPU allowance less the rest of the \
             scene",
        );
        assert_eq!(
            pane.charge.over(),
            gpu.need_bytes > gpu.allowance_bytes,
            "pane {idx} is over its room without the scene being over the pool, \
             or the other way round - the two are the same statement",
        );
    }
}
