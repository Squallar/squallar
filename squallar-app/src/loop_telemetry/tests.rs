//! The `loop state:` sentence, pinned at both ends, and the count it carries.
//!
//! Same seam as `frame_telemetry_line_tests`: the sentence is an interface
//! read by a regex in another language in another directory, so it is held as
//! a literal here AND against `drive.py`'s own pattern. A copy of a literal is
//! a second place for it to be wrong.

use super::{LoopState, loop_state_line};

/// The rig driver and the measurement launcher, read at compile time so a
/// moved or deleted file is a build failure rather than a skipped test.
const DRIVE_PY: &str = include_str!("../../../.github/browser-rig/drive.py");
const RUN_MEASURE: &str = include_str!("../../../.github/browser-rig/run_measure.sh");

/// The body of a `var <name> = /…/;` regex literal in `drive.py`.
fn pattern(name: &str) -> String {
    let head = format!("var {name} = /");
    let at = DRIVE_PY.find(&head).unwrap_or_else(|| {
        panic!(
            "drive.py no longer declares `{head}…`; the rig's probe for the \
             loop line moved and this test can no longer read it"
        )
    });
    let rest = &DRIVE_PY[at + head.len()..];
    let end = rest
        .find("/;")
        .expect("the regex literal is not closed on its own line");
    rest[..end].to_string()
}

/// The sentence a pattern describes, given what each capture group should
/// capture, in order. The loop pattern's groups are all plain `(\d+)`;
/// anything regexy surviving the substitution fails the leftover check, which
/// is what keeps this honest rather than a match test (a match answers "the
/// rig could read something"; what is wanted is "the rig reads exactly this").
fn rendered(pattern: &str, groups: &[&str]) -> String {
    const GROUP: &str = r"(\d+)";
    let mut out = String::new();
    let mut rest = pattern;
    let mut values = groups.iter();
    while let Some(at) = rest.find(GROUP) {
        out.push_str(&rest[..at]);
        out.push_str(
            values
                .next()
                .expect("the pattern has more capture groups than values were offered"),
        );
        rest = &rest[at + GROUP.len()..];
    }
    assert!(
        values.next().is_none(),
        "more values were offered than the pattern has capture groups",
    );
    out.push_str(rest);
    assert!(
        !out.contains(['\\', '[', ']', '*', '+', '?', '|', '^', '$', '(', ')']),
        "the pattern has a metacharacter outside its one known group \
         spelling, so substituting values into it no longer produces the \
         sentence it matches: {out:?}",
    );
    out
}

/// The budgets the compiled target resolves to — the same figures the running
/// app would put on the line, rather than a hand-picked arm.
fn target_budgets() -> squallar_device_profile::budget::Budgets {
    squallar_device_profile::budget::resolve(
        &squallar_device_profile::budget::DeviceProfile::for_target(),
    )
}

/// A reading with a distinct value in every position, so a transposed pair
/// cannot read as a correct line.
fn distinct() -> LoopState {
    LoopState {
        panes: 2,
        layers: 5,
        listed: 61,
        resident: 47,
        in_flight: 6,
        failed: 3,
        allowed_plan: 14,
        allowed_section: 28,
        allowed_volume: 4,
        allowed_overlay: 9,
        share_bytes: 29_360_128,
        cap: 36,
        held: 60,
        pool_bytes: 58_720_256,
        floor_bytes: 60_817_408,
        ceiling_bytes: 3_221_225_472,
        advance_us: 100_000,
    }
}

/// The literal pin. Every figure appears once and in the order the sentence
/// documents; the byte figures are bytes, never MiB, because the row printer
/// is the only thing that should be dividing.
#[test]
fn the_loop_state_line_reads_exactly_as_pinned() {
    assert_eq!(
        loop_state_line(&distinct()),
        "loop state: 2 panes, 5 layers animating, 61 frames listed, \
         47 resident, 6 in flight, 3 failed; allowed plan=14 section=28 \
         volume=4 overlay=9, cap 36, held 60; share 29360128 B, \
         pool 58720256 B, floor 60817408 B, ceiling 3221225472 B; \
         advance 100000 us",
    );
}

/// **The rig reads the loop line the app actually writes.** The other end of
/// the seam: an extra space here is not a compile error and turns the rig's
/// whole loop reading into `null`, which reads as "nothing was looping" —
/// exactly the silent under-fill the line exists to expose.
#[test]
fn the_rig_reads_the_loop_line_the_app_actually_writes() {
    assert_eq!(
        loop_state_line(&distinct()),
        rendered(
            &pattern("loop_state_re"),
            &[
                "2", "5", "61", "47", "6", "3", "14", "28", "4", "9", "36", "60", "29360128",
                "58720256", "60817408", "3221225472", "100000",
            ],
        ),
        "the `loop state:` line and the rig's probe have drifted",
    );
}

/// The floor under the seam test above: `rendered` really can disagree.
/// Without it a `pattern` that returned the sentence itself would hold the
/// equality whatever the app wrote.
#[test]
fn a_loop_line_that_drifted_by_one_space_is_not_accepted() {
    let good = rendered(
        &pattern("loop_state_re"),
        &[
            "2", "5", "61", "47", "6", "3", "14", "28", "4", "9", "36", "60", "29360128",
            "58720256", "60817408", "3221225472", "100000",
        ],
    );
    assert_eq!(loop_state_line(&distinct()), good);
    let drifted = good.replacen(" resident", "  resident", 1);
    assert_ne!(drifted, good, "the perturbation perturbed nothing");
    assert_ne!(
        loop_state_line(&distinct()),
        drifted,
        "a line with one extra space compared equal to the real one, so the \
         seam test above cannot fail",
    );
}

/// **`resident` is the subset holding a picture, and `listed` is every slot.**
/// The whole point of the pair: a loop that lists its cap and holds three
/// frames animates three while every phase reads healthy. Built with a frame
/// of each kind so no counter can be reading another's field, and with a
/// listed-but-untouched frame so that the three subsets provably do not sum
/// to `listed`.
#[test]
fn resident_counts_only_the_frames_that_hold_a_picture() {
    use squallar_egui::pane::{LoopFrame, LoopPhase, PaneState};

    let stamp = |m: u32| {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 31)
            .expect("a real date")
            .and_hms_opt(0, m, 0)
            .expect("a real time")
    };
    let mut pane = PaneState::new();
    {
        let ls = pane.time_state_mut(&squallar_source::id::known::RADAR);
        ls.phase = LoopPhase::Playing;
        ls.frames = vec![
            // Listed, dispatched, nothing back yet.
            LoopFrame {
                timestamp: stamp(0),
                image: None,
                render_in_flight: true,
                render_failed: false,
            },
            // Listed, refused.
            LoopFrame {
                timestamp: stamp(1),
                image: None,
                render_in_flight: false,
                render_failed: true,
            },
            // Listed, never dispatched — in none of the three subsets, which
            // is what makes their sum smaller than `listed`.
            LoopFrame {
                timestamp: stamp(2),
                image: None,
                render_in_flight: false,
                render_failed: false,
            },
        ];
    }
    let budgets = target_budgets();
    let allocation = crate::loop_pool::LoopPool::new(
        budgets.loop_pool_floor_bytes,
        crate::loop_pool::LoopPoolLimits::from_budgets(&budgets),
    )
    .plan(
        crate::loop_pool::LoopFrameModel::from_budgets(&budgets),
        crate::loop_pool::LoopDemand::default(),
    );
    let panes = [pane];
    let state = LoopState::gather(
        panes.iter(),
        allocation,
        &budgets,
        std::time::Duration::from_millis(100),
    );

    assert_eq!(state.panes, 1, "one pane is animating");
    assert_eq!(state.layers, 1, "one layer is animating");
    assert_eq!(state.listed, 3, "three frame slots are held");
    assert_eq!(
        state.resident, 0,
        "no frame holds a picture, so a loop reporting frames resident here \
         is counting slots",
    );
    assert_eq!(state.in_flight, 1);
    assert_eq!(state.failed, 1);
    assert!(
        state.in_flight + state.failed + state.resident < state.listed,
        "the three subsets summed to the whole, so one of them is not the \
         subset the line says it is",
    );
    assert_eq!(state.advance_us, 100_000, "10 fps is a 100 ms step");
    assert_eq!(state.held, budgets.loop_frames_held);
    assert_eq!(state.cap, budgets.loop_render_budget);
}

/// A pane animating nothing contributes nothing — the control that makes the
/// counts above readable. Without it every figure could be a constant.
#[test]
fn a_pane_with_no_loop_is_not_counted() {
    use squallar_egui::pane::PaneState;

    let budgets = target_budgets();
    let allocation = crate::loop_pool::LoopPool::new(
        budgets.loop_pool_floor_bytes,
        crate::loop_pool::LoopPoolLimits::from_budgets(&budgets),
    )
    .plan(
        crate::loop_pool::LoopFrameModel::from_budgets(&budgets),
        crate::loop_pool::LoopDemand::default(),
    );
    let panes = [PaneState::new()];
    let state = LoopState::gather(
        panes.iter(),
        allocation,
        &budgets,
        std::time::Duration::from_millis(100),
    );
    assert_eq!((state.panes, state.layers, state.listed), (0, 0, 0));
}

/// The E scenes must seed the frame-telemetry key too, or the `loop state:`
/// line is written at `debug`, the console ring never hears it, and every E
/// row loses the denominators that make it an E row. Pinned per scene rather
/// than once for the file: the seeds are separate strings and one can lose
/// the key while the others keep it.
#[test]
fn every_e_scene_seed_asks_for_the_lines_that_denominate_it() {
    for scene in ["E1)", "E2)", "E3)"] {
        let at = RUN_MEASURE.find(scene).unwrap_or_else(|| {
            panic!("run_measure.sh no longer defines scene {scene} at all")
        });
        let line_end = RUN_MEASURE[at..]
            .find(";;")
            .map(|end| at + end)
            .expect("a scene arm ends in `;;`");
        let arm = &RUN_MEASURE[at..line_end];
        assert!(
            arm.contains("\"squallar.frame_telemetry\": \"1\""),
            "scene {scene} no longer seeds frame_telemetry, so its `loop \
             state:` line is never heard and the row has no loop denominators",
        );
        assert!(
            arm.contains("\\\"loop_playback\\\":\\\"playing\\\""),
            "scene {scene} no longer seeds a playing loop",
        );
    }
}
