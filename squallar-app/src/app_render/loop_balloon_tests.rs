//! **The balloon reaches the frames a loop textures, and the lookback it never
//! touches.**
//!
//! `loop_pool::plan` gives every loop its base and spends the room by time;
//! these pin the two seams the plan crosses on its way to the screen —
//! `loop_render_budget`, which is where a grant becomes the frames a loop
//! keeps textured, and the pane walk, which reads the user's lookback and
//! must never write it.

use super::*;
use crate::loop_pool::{LoopKey, LoopKind, LoopNeed, LoopPoolLimits};
use squallar_egui::pane::LayerTimeState;
use squallar_radar::types::RenderView;

const MIB: usize = 1024 * 1024;

/// A plan-view loop on `pane` over six hours at a 300 s cadence: base 25 (two hours at
/// 300 s, the rung's span), ceiling 60 (73 listed, `MAX_LOOP_FRAMES`).
fn six_hours(pane: usize, cadence_secs: Option<u32>) -> LoopNeed {
    let budgets = test_budgets();
    let span_secs = 6 * 60 * 60;
    let base = budgets.frames_for_span_of(span_secs as usize, cadence_secs);
    LoopNeed {
        key: LoopKey { pane },
        kind: LoopKind::PlanView,
        span_secs,
        cadence_secs,
        frame_bytes: LoopFrameModel::from_budgets(&budgets).plan_view,
        base_frames: base,
        // A loop that knows its cadence has a listing, and one that does
        // not has none: the two arrive together.
        max_frames: crate::loop_pool::loop_ceiling_frames(
            cadence_secs.map(|_| 73),
            span_secs,
            cadence_secs,
            base,
            budgets.loop_frames_held,
        ),
    }
}

fn planned(needs: impl IntoIterator<Item = LoopNeed>) -> LoopAllocation {
    let budgets = test_budgets();
    let mut demand = LoopDemand::default();
    for need in needs {
        demand.push(need);
    }
    LoopPool::new(3072 * MIB, LoopPoolLimits::from_budgets(&budgets))
        .plan(LoopFrameModel::from_budgets(&budgets), &demand)
}

fn loop_at(cadence_secs: Option<u32>) -> LayerTimeState {
    let mut ls = LayerTimeState::begin(6 * 60 * 60, RenderView::PlanView, Box::new(()));
    ls.phase = squallar_egui::pane::LoopPhase::Rendering;
    ls.cadence_secs = cadence_secs;
    ls
}

/// **A grant planned with the loop's own cadence is the textured budget, above the
/// rung's span.** The base already held the lookback to that span, and the frames above
/// it are the balloon — clamping to `frames_for_span` again here is exactly what would
/// take the balloon back. On this build's desktop budgets a six-hour plan-view loop at
/// 300 s is held to 25 by the span and granted 60 by a 3072 MiB pool; the grant wins.
#[test]
fn a_grant_planned_with_the_loops_cadence_textures_above_the_rungs_span() {
    let budgets = test_budgets();
    let allocation = planned([six_hours(0, Some(300))]);
    let ls = loop_at(Some(300));
    let span_clamp = budgets.frames_for_span(Some(300));
    assert_eq!(
        span_clamp, 25,
        "two hours at 300 s, as the rung's span answers"
    );
    assert_eq!(allocation.frames_for_pane(0), Some(60), "the grant");
    assert_eq!(
        loop_render_budget(&allocation, 0, &ls, &budgets),
        60,
        "the grant is the textured budget, not the span clamp",
    );
    assert!(loop_render_budget(&allocation, 0, &ls, &budgets) > span_clamp);
}

/// **A pane the plan has not seen is held to its kind's ceiling and the span**, as a
/// loop with no grant always was — the plan catches up within the dwell.
#[test]
fn a_pane_the_plan_has_not_seen_reads_the_kinds_ceiling_and_the_span() {
    let budgets = test_budgets();
    let allocation = planned([six_hours(0, Some(300))]);
    let ls = loop_at(Some(300));
    assert!(
        allocation.frames_for_pane(1).is_none(),
        "precondition: pane 1 is unseen"
    );
    assert_eq!(
        loop_render_budget(&allocation, 1, &ls, &budgets),
        allocation
            .frames_for(RenderView::PlanView)
            .min(budgets.frames_for_span(Some(300))),
    );
    assert_eq!(loop_render_budget(&allocation, 1, &ls, &budgets), 25);
    // And with no cadence yet, the ceiling held to the render budget — what a
    // loop with no cadence has always bought.
    let fresh = loop_at(None);
    assert_eq!(
        loop_render_budget(&allocation, 1, &fresh, &budgets),
        allocation
            .frames_for(RenderView::PlanView)
            .min(budgets.loop_render_budget),
    );
    assert_eq!(loop_render_budget(&allocation, 1, &fresh, &budgets), 36);
}

/// **A grant planned before the loop's listing said its cadence is held to the rung's
/// span at that cadence** — the answer a loop with no grant gets — until the plan
/// catches up. Without this the frames a listing lands into would be sized by a grant
/// that priced the loop at the whole render budget, and the plan would take most of
/// them back fifteen frames later.
#[test]
fn a_grant_planned_before_the_cadence_was_known_is_held_to_the_span_until_the_plan_catches_up() {
    let budgets = test_budgets();
    let before = planned([six_hours(0, None)]);
    let grant = before.grant_for_pane(0).expect("pane 0 asked for a loop");
    assert!(
        grant.cadence_secs.is_none(),
        "precondition: planned with no cadence"
    );
    assert_eq!(
        grant.frames, 36,
        "no cadence: the base is the render budget, and so is the ceiling"
    );

    // The listing has landed and said 300 s; the plan has not caught up.
    let ls = loop_at(Some(300));
    assert_eq!(
        loop_render_budget(&before, 0, &ls, &budgets),
        grant.frames.min(budgets.frames_for_span(Some(300))),
    );
    assert_eq!(loop_render_budget(&before, 0, &ls, &budgets), 25);

    // Once it has, the grant is the answer.
    let after = planned([six_hours(0, Some(300))]);
    assert_eq!(loop_render_budget(&after, 0, &ls, &budgets), 60);
}

/// **The persisted lookback is the user's, and the pool path never writes it.** The
/// slider writes `Gui.loop_lookback_secs`; the pool reads each pane's span and grants
/// density inside it. A source pin, with its control: the setting is written somewhere
/// — the timeline UI — and nowhere on the pool's path.
#[test]
fn the_persisted_lookback_is_never_written_by_the_pool_path() {
    const POOL: &str = include_str!("../loop_pool.rs");
    const RENDER: &str = include_str!("../app_render.rs");
    const TIMELINE: &str = include_str!("../../../squallar-egui/src/ui_timeline.rs");

    // Control: the setting exists and is written by the timeline UI.
    assert!(
        TIMELINE.contains("loop_lookback_secs"),
        "control: the timeline UI no longer names `loop_lookback_secs`; the pin below \
         would then hold against nothing",
    );
    for (name, source) in [("loop_pool.rs", POOL), ("app_render.rs", RENDER)] {
        assert!(
            !source.contains("loop_lookback_secs"),
            "{name} reaches the persisted lookback; the pool buys density inside the \
             user's window and never touches the window",
        );
        assert!(
            !source.contains("set_loop_span_secs"),
            "{name} re-arms the panes' spans; only the slider does that",
        );
        // The pane's span is read on the walk and written by nobody here.
        let writes = source
            .split_whitespace()
            .collect::<String>()
            .matches(".span_secs=")
            .count();
        assert_eq!(
            writes, 0,
            "{name} assigns a span: {writes} occurrence(s) of `.span_secs =`",
        );
    }
    // And the walk does read it, or the pin above is vacuous.
    assert!(
        RENDER.contains("pane.time.span_secs"),
        "control: the pane walk no longer reads the pane's lookback",
    );
}

/// The balloon the `budget state:` line carries is the allocation's, and it is a real
/// zero when every loop holds its base or less.
#[test]
fn the_balloon_figure_is_the_allocations_and_zero_means_none() {
    let none = planned([six_hours(0, None)]);
    assert_eq!(
        none.balloon_bytes(),
        0,
        "no cadence, no ceiling above the base, no balloon"
    );
    let some = planned([six_hours(0, Some(300))]);
    assert_eq!(
        some.balloon_bytes(),
        35 * LoopFrameModel::from_budgets(&test_budgets()).plan_view,
        "60 granted over a base of 25, at 16 MiB a frame",
    );
    assert_eq!(
        test_loop_allocation().balloon_bytes(),
        0,
        "an idle application"
    );
}
