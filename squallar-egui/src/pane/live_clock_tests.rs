//! **A pane flagged live depicts now unless its clock is the playhead of the
//! loop it is running or waiting to re-arm** — every way the live flag, the
//! clock and the loop come apart, asked of the one read that holds the rule,
//! [`PaneState::time_mode`], and of what a layer handler is told.
//!
//! The report, 2026-09-12: overlays with a past (mesoscale discussions, NWS
//! alerts) missing right after opening the app, cured only by Set Time then
//! Live. The first table is every entry into that state. The second is what the
//! rule must leave alone: a running loop's playhead, a paused loop's chosen
//! frame through a re-arm and a restore (reopen is exactly 1:1), and a park the
//! user made. The third is the flag's one writer: clearing the flag never brings
//! back a clock the read declared dead.
//!
//! Loops here are armed the way `App::handle_enable_loop` arms them — the
//! lineage door, [`PaneState::begin_or_continue_loop`], before the timeline is
//! written — so no row passes on a loop no door minted.

use super::*;
use squallar_source::id::known;

fn stale() -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
        .and_then(|d| d.and_hms_opt(18, 0, 0))
        .expect("a real instant")
}

fn frame() -> NaiveDateTime {
    stale() + chrono::Duration::minutes(5)
}

fn pane() -> PaneState {
    PaneState::with_site("KTLX".to_string())
}

/// Arm the transport the way `App::handle_enable_loop` does: the lineage door
/// with the wish still in place, the wish consumed, then the timeline
/// `arm_layer_loop` writes.
fn arm(pane: &mut PaneState) {
    pane.begin_or_continue_loop();
    pane.loop_arm_pending = None;
    *pane.transport_state_mut() = LayerTimeState::begin(3600, RenderView::PlanView, Box::new(()));
}

/// A file's paused loop brought back the way `load_ui_config` does it: the
/// wish, then the restore door.
fn restore_paused(pane: &mut PaneState, at: NaiveDateTime) {
    pane.loop_arm_pending = Some(LoopArm { playing: false });
    pane.restore_time_mode(TimeMode::AsOf(at));
}

/// What the pane says it depicts, and what a layer handler is told.
fn depicted(pane: &PaneState) -> (TimeMode, Option<NaiveDateTime>) {
    (
        pane.time_mode(),
        pane.view(0).layer(&known::NWS_ALERTS).as_of,
    )
}

/// One way of bringing a pane into a state, for the row tables below.
type Setup = fn(&mut PaneState);

/// Every entry into "flagged live over a clock no loop owns".
fn dead_clock_rows() -> [(&'static str, Setup); 10] {
    [
        ("a file with a live flag and a parked clock, no loop", |p| {
            p.restore_time_mode(TimeMode::AsOf(stale()))
        }),
        ("a clock written with no loop running", |p| {
            p.set_time_mode(TimeMode::AsOf(stale()))
        }),
        ("a time link carried the live flag over a user park", |p| {
            p.set_viewing_live(false);
            p.set_time_mode(TimeMode::AsOf(stale()));
            p.set_viewing_live(true);
        }),
        ("the loop was switched off after playing", |p| {
            arm(p);
            p.set_time_mode(TimeMode::AsOf(frame()));
            p.stop_every_layer_loop();
        }),
        (
            "the loop left loop mode through a direct reset (empty or refused listing)",
            |p| {
                arm(p);
                p.set_time_mode(TimeMode::AsOf(frame()));
                *p.transport_state_mut() = LayerTimeState::new();
            },
        ),
        ("the transport moved to a layer with no loop", |p| {
            arm(p);
            p.set_time_mode(TimeMode::AsOf(frame()));
            p.set_transport_layer(known::GMGSI);
        }),
        ("a new loop was begun over the last loop's clock", |p| {
            arm(p);
            p.set_time_mode(TimeMode::AsOf(frame()));
            p.stop_every_layer_loop();
            arm(p);
        }),
        (
            "a file's playing loop: the frame that happened to be up is not the loop's",
            |p| {
                p.loop_arm_pending = Some(LoopArm { playing: true });
                p.restore_time_mode(TimeMode::AsOf(frame()));
            },
        ),
        ("a file's paused loop whose wish was then cancelled", |p| {
            restore_paused(p, frame());
            p.loop_arm_pending = None;
        }),
        (
            "a loop no door minted: a timeline written by hand owns nothing",
            |p| {
                *p.transport_state_mut() =
                    LayerTimeState::begin(3600, RenderView::PlanView, Box::new(()));
                p.set_time_mode(TimeMode::AsOf(frame()));
            },
        ),
    ]
}

#[test]
fn a_live_flag_over_a_clock_no_loop_owns_depicts_now() {
    for (why, setup) in dead_clock_rows() {
        let mut p = pane();
        setup(&mut p);
        assert!(
            p.viewing_live(),
            "premise: the pane is flagged live ({why})"
        );
        assert_eq!(
            depicted(&p),
            (TimeMode::Live, None),
            "{why}: the pane must depict now, and tell every layer so — a stale \
             instant here is what fetched the archive's discussions and filtered \
             today's alerts away",
        );
    }
}

#[test]
fn a_loops_own_playhead_and_a_user_park_are_depicted_as_written() {
    let rows: [(&str, Setup, NaiveDateTime); 7] = [
        (
            "a live pane's running loop wrote its playhead",
            |p| {
                arm(p);
                p.set_time_mode(TimeMode::AsOf(frame()));
            },
            frame(),
        ),
        (
            "a live loop paused on a frame and re-armed in place (a lookback change)",
            |p| {
                arm(p);
                p.set_time_mode(TimeMode::AsOf(frame()));
                arm(p);
            },
            frame(),
        ),
        (
            "a live loop begun anew, then written by its own playback",
            |p| {
                arm(p);
                p.set_time_mode(TimeMode::AsOf(stale()));
                p.stop_every_layer_loop();
                arm(p);
                p.set_time_mode(TimeMode::AsOf(frame()));
            },
            frame(),
        ),
        (
            "a file's paused loop, before it re-arms",
            |p| restore_paused(p, frame()),
            frame(),
        ),
        (
            "a file's paused loop, once it re-arms",
            |p| {
                restore_paused(p, frame());
                arm(p);
            },
            frame(),
        ),
        (
            "the user parked the pane",
            |p| {
                p.set_viewing_live(false);
                p.set_time_mode(TimeMode::AsOf(stale()));
            },
            stale(),
        ),
        (
            "the user parked the pane and its loop then stopped",
            |p| {
                p.set_viewing_live(false);
                arm(p);
                p.set_time_mode(TimeMode::AsOf(frame()));
                p.stop_every_layer_loop();
            },
            frame(),
        ),
    ];

    for (why, setup, at) in rows {
        let mut p = pane();
        setup(&mut p);
        assert_eq!(
            depicted(&p),
            (TimeMode::AsOf(at), Some(at)),
            "{why}: the clock is the playhead of what the pane is showing, or a \
             park the user made, and must be depicted as written",
        );
    }
}

/// **The flag's writer keeps a dead clock dead.** Clearing the flag is what
/// makes a stored clock readable as a park, so every entry above is cleared,
/// set again and cleared again: each time the pane must depict now. The
/// controls are the two clocks a loop owns, which clearing must keep.
#[test]
fn clearing_the_live_flag_never_brings_back_a_clock_the_read_declared_dead() {
    for (why, setup) in dead_clock_rows() {
        let mut p = pane();
        setup(&mut p);
        for step in ["cleared", "set again", "cleared again"] {
            p.set_viewing_live(step == "set again");
            assert_eq!(
                depicted(&p),
                (TimeMode::Live, None),
                "{why}, flag {step}: the pane went back to a clock the read had \
                 declared dead",
            );
        }
    }

    let controls: [(&str, Setup); 2] = [
        ("a live loop's own playhead", |p| {
            arm(p);
            p.set_time_mode(TimeMode::AsOf(frame()));
        }),
        ("a file's paused loop", |p| restore_paused(p, frame())),
    ];
    for (why, setup) in controls {
        let mut p = pane();
        setup(&mut p);
        p.set_viewing_live(false);
        assert_eq!(
            depicted(&p),
            (TimeMode::AsOf(frame()), Some(frame())),
            "{why}: clearing the flag must keep a clock the loop owns",
        );
    }
}
