//! **WO-E7d: the share is actually WIRED, not just computed.**
//!
//! `layer_share` is pinned directly beside it, but a correct divider that
//! nothing calls divides nothing. Today every pane animates exactly one layer,
//! so removing either call site changes no observable behaviour — which means
//! the wiring has no coverage from the existing suites at all. These build the
//! two-animating-layer pane the build cannot make yet and check that the cap
//! the append path enforces is the divided one.

use super::append_polled_frame_to_loops;
use rustdar_egui::pane::{LayerTimeState, PaneState};
use rustdar_source::id::known;

fn ts(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .expect("a real date")
        .and_hms_opt(0, 0, 0)
        .expect("a real time")
        + chrono::Duration::minutes(i64::from(minute))
}

fn site() -> rustdar_radar::sites::RadarSite {
    rustdar_radar::sites::RadarSite {
        name: "KTLX",
        lat: 35.33,
        lon: -97.27,
        heights: None,
    }
}

/// A pane whose radar layer is animating, plus `extra` further animating
/// layers stacked above nothing in particular — the count is what the divider
/// reads.
fn pane_animating(extra: &[rustdar_source::id::LayerId]) -> PaneState {
    let mut pane = PaneState::with_site("KTLX".to_string());
    *pane.loop_state_mut() = rustdar_egui::radar_layer::begin_loop(
        24 * 3600,
        &site(),
        rustdar_radar::types::RenderView::PlanView,
    );
    for id in extra {
        let mut timeline = LayerTimeState::new();
        timeline.phase = rustdar_egui::pane::LoopPhase::Rendering;
        *pane.time_state_mut(id) = timeline;
    }
    pane
}

fn fill(pane: &mut PaneState, minutes: u32) {
    let allocation = crate::app::render::test_loop_allocation();
    let budgets = crate::app::render::test_budgets();
    for minute in 0..minutes {
        let mut panes = [std::mem::take(pane)];
        append_polled_frame_to_loops(&mut panes, "KTLX", ts(minute), allocation, &budgets);
        *pane = panes.into_iter().next().expect("one pane");
    }
}

/// **The cap the append path enforces is the divided one.** One animating
/// layer holds the whole budget; two hold half each. Both arms are measured
/// from the same fill, so neither can pass on a number chosen here.
#[test]
fn a_pane_animating_two_layers_splits_the_frame_cap_between_them() {
    let budgets = crate::app::render::test_budgets();
    let whole = crate::app::render::layer_share(budgets.loop_frames_held, 1);
    let halved = crate::app::render::layer_share(budgets.loop_frames_held, 2);
    assert!(
        halved < whole,
        "precondition: this build's cap ({whole}) is big enough that halving \
         it is a different number, or the test below cannot tell the two \
         arms apart",
    );

    let mut alone = pane_animating(&[]);
    fill(&mut alone, 80);
    assert_eq!(
        alone.animating_layers().count(),
        1,
        "precondition: one animating layer",
    );
    assert_eq!(
        alone.loop_state().frames.len(),
        whole,
        "a pane animating one layer fills to the whole cap",
    );

    let mut shared = pane_animating(&[known::MODEL_DATA]);
    fill(&mut shared, 80);
    assert_eq!(
        shared.animating_layers().count(),
        2,
        "precondition: two animating layers",
    );
    assert_eq!(
        shared.loop_state().frames.len(),
        halved,
        "and a pane animating two fills to half of it - the append path reads \
         the divided share, not the whole cap",
    );
}

/// The **listing** path divides too: a loop built from a 200-scan listing is
/// sampled down to the share, not to the whole cap.
///
/// This pins `accept_scan_listing`'s own arithmetic. Its *caller-side* count —
/// that `poll_loop_scan_list_results` passes the pane's real
/// `animating_layers().count()` rather than a constant — is NOT pinned here
/// and cannot be without driving the scan-list channel through a whole `App`;
/// it is named in the WO-E7d log entry as a residual rather than left to be
/// discovered. The append path above IS pinned end-to-end.
#[test]
fn a_listing_is_sampled_down_to_the_share_not_to_the_whole_cap() {
    let allocation = crate::app::render::test_loop_allocation();
    let budgets = crate::app::render::test_budgets();
    let whole = crate::app::render::layer_share(budgets.loop_frames_held, 1);
    let halved = crate::app::render::layer_share(budgets.loop_frames_held, 2);
    assert!(halved < whole, "precondition: halving the cap is visible");

    let scans: Vec<(chrono::NaiveDateTime, rustdar_radar::archive::Identifier)> = (0..200)
        .map(|minute| {
            (
                ts(minute),
                rustdar_radar::archive::Identifier::new(format!("scan-{minute}")),
            )
        })
        .collect();

    let build = |animating: usize| {
        let mut pane = pane_animating(&[]);
        crate::app::render::accept_scan_listing_for_test(
            allocation,
            &budgets,
            pane.loop_state_mut(),
            "KTLX",
            scans.clone(),
            animating,
        );
        pane.loop_state().frames.len()
    };

    assert_eq!(build(1), whole, "one animating layer takes the whole cap");
    assert_eq!(build(2), halved, "two split it");
}
