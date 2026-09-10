//! The `loop state:` sentence, pinned at both ends, and the count it carries.
//!
//! Same seam as `frame_telemetry_line_tests`: the sentence is an interface
//! read by a regex in another language in another directory, so it is held as
//! a literal here AND against `drive.py`'s own pattern. A copy of a literal is
//! a second place for it to be wrong.

use super::{LoopState, SkippedTicks, loop_state_line};

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
        shared: 7,
        sole_pinned_bytes: 1_310_720,
        sole_pinned_frames: 11,
        sole_walks: 23,
    }
}

/// A skip reading with two panes at different counts, so the tail's
/// attribution cannot read correctly with the pair transposed and the total
/// cannot be a copy of either half. Pane 1 and pane 3, never 0 and 1, so an
/// index printed as a count (or the reverse) is visible.
fn distinct_skips() -> SkippedTicks {
    let mut skips = SkippedTicks::default();
    for _ in 0..4 {
        skips.note(1);
    }
    for _ in 0..2 {
        skips.note(3);
    }
    skips
}

/// The literal pin. Every figure appears once and in the order the sentence
/// documents; the byte figures are bytes, never MiB, because the row printer
/// is the only thing that should be dividing.
#[test]
fn the_loop_state_line_reads_exactly_as_pinned() {
    assert_eq!(
        loop_state_line(&distinct(), &distinct_skips()),
        "loop state: 2 panes, 5 layers animating, 61 frames listed, \
         47 resident, 6 in flight, 3 failed; allowed plan=14 section=28 \
         volume=4 overlay=9, cap 36, held 60; share 29360128 B, \
         pool 58720256 B, floor 60817408 B, ceiling 3221225472 B; \
         advance 100000 us; shared 7; skipped by pane 1=4 3=2; \
         ticks skipped 6; sole pinned 1310720 B over 11 frames, \
         sole walks 23",
    );
}

/// **The healthy case is a positive statement, not an absence.** A run where
/// nothing skipped prints `none` and a zero rather than an empty run of
/// pairs, so a reader never has to tell "no skips" apart from "the tail was
/// truncated" — and the rig's `(\d+)` still has a digit to capture.
#[test]
fn a_run_that_skipped_nothing_still_says_so() {
    let line = loop_state_line(&distinct(), &SkippedTicks::default());
    assert!(
        line.ends_with(
            "; skipped by pane none; ticks skipped 0; sole pinned 1310720 B \
             over 11 frames, sole walks 23",
        ),
        "a clean run must still name the counter: {line}",
    );
}

/// **The rig reads the loop line the app actually writes.** The other end of
/// the seam: an extra space here is not a compile error and turns the rig's
/// whole loop reading into `null`, which reads as "nothing was looping" —
/// exactly the silent under-fill the line exists to expose.
///
/// A PREFIX rather than the whole sentence, and the assertion is no weaker
/// for it: `loop_state_re` is unanchored and stops at `shared`, so what it
/// reads *is* this prefix, and the tail past it is pinned exactly by the
/// `strip_prefix` remainder below and by its own probe in
/// [`the_rig_reads_the_skipped_tick_count_the_app_actually_writes`]. Every
/// character of the line is still held by one of the three.
#[test]
fn the_rig_reads_the_loop_line_the_app_actually_writes() {
    let line = loop_state_line(&distinct(), &distinct_skips());
    let fixed = rendered(
        &pattern("loop_state_re"),
        &[
            "2",
            "5",
            "61",
            "47",
            "6",
            "3",
            "14",
            "28",
            "4",
            "9",
            "36",
            "60",
            "29360128",
            "58720256",
            "60817408",
            "3221225472",
            "100000",
            "7",
        ],
    );
    let tail = line.strip_prefix(&fixed).unwrap_or_else(|| {
        panic!(
            "the `loop state:` line and the rig's probe have drifted; the rig \
             reads {fixed:?} and the app writes {line:?}"
        )
    });
    assert_eq!(
        tail,
        "; skipped by pane 1=4 3=2; ticks skipped 6; \
         sole pinned 1310720 B over 11 frames, sole walks 23",
        "the tail past the rig's fixed columns is not what it is pinned to",
    );
}

/// **The rig reads the skipped-tick count the app actually writes.** The
/// counter's own end of the seam: it sits past `loop_state_re`'s last group,
/// behind a variable-arity per-pane group, so it needs a probe that finds it
/// by label rather than by column — and that probe has to agree with the
/// sentence exactly as the other one does.
///
/// **This asserted `ends_with` until 2026-09-09, and end-of-line was never
/// the property.** `loop_skipped_re` is unanchored and takes its first match,
/// so where the count sits in the line is irrelevant to it; what the rig
/// actually needs is that the probe MATCHES and that no second `ticks
/// skipped` can be found before the real one. Pinning the position instead
/// meant the first figure appended behind the counter reddened this test
/// while the rig it guards kept reading correctly — a seam test failing for a
/// reason the seam does not have. Both halves are still pinned exactly, so
/// every character of the tail is held.
#[test]
fn the_rig_reads_the_skipped_tick_count_the_app_actually_writes() {
    let line = loop_state_line(&distinct(), &distinct_skips());
    let probe = rendered(&pattern("loop_skipped_re"), &["6"]);
    let (before, after) = line.split_once(&probe).unwrap_or_else(|| {
        panic!(
            "the skipped-tick probe reads {probe:?}, which does not appear in \
             the line at all: {line:?}"
        )
    });
    assert_eq!(
        after, "; sole pinned 1310720 B over 11 frames, sole walks 23",
        "what follows the skipped-tick count is not what it is pinned to",
    );
    assert!(
        !before.contains("ticks skipped"),
        "an earlier `ticks skipped` would take the rig's unanchored probe \
         first and it would read the wrong figure: {line:?}",
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
            "2",
            "5",
            "61",
            "47",
            "6",
            "3",
            "14",
            "28",
            "4",
            "9",
            "36",
            "60",
            "29360128",
            "58720256",
            "60817408",
            "3221225472",
            "100000",
            "7",
        ],
    );
    let line = loop_state_line(&distinct(), &distinct_skips());
    assert!(line.starts_with(&good));
    let drifted = good.replacen(" resident", "  resident", 1);
    assert_ne!(drifted, good, "the perturbation perturbed nothing");
    assert!(
        !line.starts_with(&drifted),
        "a line with one extra space still matched as a prefix, so the seam \
         test above cannot fail",
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
    let mut state = LoopState::default();
    state.count_pane(&pane);

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
}

/// **10 fps is a 100 000 us step**, and the line reports the interval the
/// transport actually advances on rather than the fps the config spells —
/// `loop_interval` clamps, so the two can disagree and only one of them is
/// what the loop did.
#[test]
fn the_advance_is_the_interval_the_transport_steps_on() {
    use squallar_egui::pane::DEFAULT_LOOP_SPEED_FPS;
    assert_eq!(DEFAULT_LOOP_SPEED_FPS, 10.0);
    assert_eq!(
        std::time::Duration::from_secs_f32(1.0 / DEFAULT_LOOP_SPEED_FPS).as_micros(),
        100_000,
        "the scene E seeds pin loop_speed_fps=10.0 and every row reads back \
         `advance 100000 us`; if this moves, the rows and the seed disagree",
    );
}

/// A pane animating nothing contributes nothing — the control that makes the
/// counts above readable. Without it every figure could be a constant.
#[test]
fn a_pane_with_no_loop_is_not_counted() {
    use squallar_egui::pane::PaneState;

    let mut state = LoopState::default();
    state.count_pane(&PaneState::new());
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
        let at = RUN_MEASURE
            .find(scene)
            .unwrap_or_else(|| panic!("run_measure.sh no longer defines scene {scene} at all"));
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

/// **The fires counter is on the line, and it is what separates a healthy zero
/// from a walk that never ran.**
///
/// `sole pinned 0 B` is the reading a scene where every frame's volume is
/// still cached produces — the common, healthy case — and it is character for
/// character what a `sole_pinned_volume_bytes` that was never called would
/// print. `sole walks` is the only thing on the line that tells them apart,
/// so a zero-bytes reading with a positive walk count has to be expressible
/// and has to read differently from a zero-walk one.
#[test]
fn a_healthy_zero_and_a_walk_that_never_ran_read_differently() {
    let ran = LoopState {
        sole_pinned_bytes: 0,
        sole_pinned_frames: 0,
        sole_walks: 104,
        ..distinct()
    };
    let never = LoopState {
        sole_walks: 0,
        ..ran
    };
    let ran = loop_state_line(&ran, &SkippedTicks::default());
    let never = loop_state_line(&never, &SkippedTicks::default());
    assert!(
        ran.ends_with("sole pinned 0 B over 0 frames, sole walks 104"),
        "a walk that found nothing sole must still say it walked: {ran}",
    );
    assert!(
        never.ends_with("sole pinned 0 B over 0 frames, sole walks 0"),
        "a walk that never ran must be readable as such: {never}",
    );
    assert_ne!(
        ran, never,
        "the fires counter is the ONLY term separating these two readings; \
         without it on the line they are the same sentence",
    );
}

/// **`share` is a permission and `sole pinned` is a measurement, and the line
/// must not let them be confused.**
///
/// `share` is a mean of what the pool's planner CHARGED at nominal per-frame
/// prices — it reads 576 MiB on a REST1 scene whose `frames listed` is 0 —
/// so it can stand at gigabytes while the store measurably holds nothing.
/// That combination is not a contradiction and has to be expressible, because
/// it is what a real idle reading looks like; a lane that read `share` as
/// residency was out by four orders of magnitude.
#[test]
fn a_huge_share_and_zero_measured_bytes_is_a_legal_reading() {
    let idle = LoopState {
        listed: 0,
        resident: 0,
        share_bytes: 603_979_776,
        sole_pinned_bytes: 0,
        sole_pinned_frames: 0,
        sole_walks: 88,
        ..distinct()
    };
    let line = loop_state_line(&idle, &SkippedTicks::default());
    assert!(
        line.contains("0 frames listed, 0 resident"),
        "the store's own counts have to be readable as zero: {line}",
    );
    assert!(
        line.contains("share 603979776 B"),
        "the permission stands at the pool floor regardless: {line}",
    );
    assert!(
        line.ends_with("sole pinned 0 B over 0 frames, sole walks 88"),
        "and the measured figure is zero beside it, walked: {line}",
    );
}
