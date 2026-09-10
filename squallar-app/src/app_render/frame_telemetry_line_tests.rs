//! The six frame telemetry sentences, pinned word for word — twice.
//!
//! The browser rig reads telemetry sentences out of the console ring with
//! regexes it owns, so a sentence is an interface: an extra space is not a
//! compile error anywhere and silently turns a rig reading into `null`.
//! Since WO-5 the probe exists (`drive.py`'s `FRAME_LINE_PROBE`), so each
//! sentence is held at both ends: as a literal here (the hand-derived-edge
//! pins) and against the rig's own pattern, read out of `drive.py` the way
//! `raster_telemetry_line_tests` reads the raster patterns — a copy of a
//! literal is a second place for it to be wrong.
//!
//! Every expected percentile below is derived by hand from the histogram's
//! documented edge formula (⌊62 500 × 2^(j/4)⌋ ns shifted per octave, upper
//! edge, rounded up to whole microseconds) — not read back from the code
//! under test.

use squallar_device_profile::hist::Hist;
use squallar_gpu::egui_renderer::geometry_staging::GeometryStagingTotals;
use squallar_gpu::egui_renderer::pass_costs::{PassCosts, StagedGeometry};

/// The rig driver, read at compile time so a moved or deleted file is a
/// build failure rather than a skipped test.
const DRIVE_PY: &str = include_str!("../../../.github/browser-rig/drive.py");

/// The **second** reader of the `frame need:` sentence, read at compile time
/// for `DRIVE_PY`'s reason.
///
/// `drive.py` scrapes that line in the browser; this scrapes it natively, and
/// for as long as the pin below asked only about `drive.py` the two could
/// drift. They did: the claim added in September 2026 grew `drive.py` a field
/// and left this file expecting ten, and the pin stayed green while the native
/// rig's probe no longer matched the line at all. A positional regex that
/// stops matching prints `frame need: n/a`, which reads as "a binary older
/// than the line" -- a plausible, boring story ready-made to explain away an
/// instrument that has simply broken.
const NATIVE_ROW_PY: &str = include_str!("../../../.github/browser-rig/native_row.py");

/// The module whose telemetry sentences these tests pin, read at compile time
/// for `DRIVE_PY`'s reason — the two source-reading gates below ask what the
/// app writes, not what a fixture says it writes.
const APP_RENDER: &str = include_str!("../app_render.rs");

/// The body of a `var <name> = /…/;` regex literal in `drive.py`. Same
/// extraction as `raster_telemetry_line_tests::pattern`, restated here only
/// because the two modules pin different lines.
fn pattern(name: &str) -> String {
    let head = format!("var {name} = /");
    let at = DRIVE_PY.find(&head).unwrap_or_else(|| {
        panic!(
            "drive.py no longer declares `{head}…`; the rig's probe for this \
             line moved and this test can no longer read it"
        )
    });
    let rest = &DRIVE_PY[at + head.len()..];
    let end = rest
        .find("/;")
        .expect("the regex literal is not closed on its own line");
    rest[..end].to_string()
}

/// The sentence a frame-line pattern describes, given what each capture
/// group should capture, in order.
///
/// Deliberately not a regex match, for `raster_telemetry_line_tests`'
/// reason: a match answers "the rig could read something", and what is
/// wanted is "the rig reads exactly this". The frame patterns carry three
/// group spellings beyond plain `(\d+)` — the `none`/`over` percentile
/// alternation, the histogram digit list, and the script name — plus two
/// escaped parens; everything else regexy failing the leftover check below
/// is what keeps the substitution honest.
fn rendered(pattern: &str, groups: &[&str]) -> String {
    const GROUP_SPELLINGS: [&str; 4] =
        [r"(\d+|none|over)", r"(\d+)", r"([0-9,]+)", r"([a-z0-9-]+)"];
    let mut out = String::new();
    let mut rest = pattern;
    let mut values = groups.iter();
    while let Some((at, spelling)) = GROUP_SPELLINGS
        .iter()
        .filter_map(|g| rest.find(g).map(|at| (at, *g)))
        .min()
    {
        out.push_str(&rest[..at]);
        out.push_str(
            values
                .next()
                .expect("the pattern has more capture groups than values were offered"),
        );
        rest = &rest[at + spelling.len()..];
    }
    assert!(
        values.next().is_none(),
        "more values were offered than the pattern has capture groups",
    );
    out.push_str(rest);
    let out = out.replace(r"\(", "(").replace(r"\)", ")");
    assert!(
        !out.contains(['\\', '[', ']', '*', '+', '?', '|', '^', '$']),
        "the pattern has a metacharacter outside its known group spellings, \
         so substituting values into it no longer produces the sentence it \
         matches: {out:?}",
    );
    out
}

/// A histogram's counts the way the line embeds them — from the public
/// counts, not from the formatter under test.
fn counts_string(h: &Hist) -> String {
    h.counts()
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Three samples in one bin: n and all three percentiles land on that bin's
/// upper edge, and the embedded histogram holds all three in that one slot.
/// 100 us sits in [88 388, 105 112) ns — geometric bin slot 3, upper edge
/// 106 us.
#[test]
fn the_idle_service_line_reads_exactly_as_pinned() {
    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let mut slots = [0u32; 42];
    slots[3] = 3;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::frame_service_idle_line(&h),
        format!(
            "frame service (idle): n=3, p50=106 us, p90=106 us, p99=106 us, hist={expected_hist}"
        ),
    );
}

/// An empty family prints `n=0` and `none` percentiles rather than nothing:
/// "nobody touched the window" must be a readable figure, because it is the
/// manual check that the interact family is not contaminated by idle frames.
#[test]
fn an_empty_family_prints_a_zero_not_an_absence() {
    let empty_hist = ["0"; 42].join(",");
    assert_eq!(
        super::frame_service_idle_line(&Hist::new()),
        format!(
            "frame service (idle): n=0, p50=none us, p90=none us, p99=none us, hist={empty_hist}"
        ),
    );
}

/// The interact line embeds all 42 histogram counts — under-clamp first, the
/// 40 geometric bins, over-clamp last — and a sample past the 64 ms ceiling
/// answers `over`, never a made-up edge.
#[test]
fn the_interact_service_line_embeds_the_whole_histogram() {
    let mut h = Hist::new();
    h.record(10); // under the 62.5 us floor -> first slot
    h.record(100_000); // 100 ms, past the ceiling -> last slot
    let expected_hist = format!("1{}{}", ",0".repeat(40), ",1");
    assert_eq!(
        super::frame_service_interact_line(&h),
        format!(
            "frame service (interact): n=2, p50=63 us, p90=over us, \
             p99=over us, hist={expected_hist}",
        ),
    );
}

/// Each segment prints under its own name with a distinct value, so a
/// transposed pair cannot read as a correct line. Expected edges, derived by
/// hand: 100 us -> 106, 200 -> 211, 500 -> 595, 1 000 -> 1 190,
/// 2 000 -> 2 379, 4 000 -> 4 757, 3 000 -> 3 364, 5 000 -> 5 657.
#[test]
fn the_segments_line_names_each_segment_and_the_acquire_separately() {
    let mut s = crate::frame_ledger::SegmentHists::default();
    s.pre.record(100);
    s.pump.record(200);
    s.ui.record(500);
    s.prepare.record(1_000);
    s.finish.record(2_000);
    s.post.record(4_000);
    let mut acquire = Hist::new();
    acquire.record(3_000);
    acquire.record(5_000);
    assert_eq!(
        super::frame_segments_line(&s, &acquire),
        "frame segments (interact, p99 us): pre=106, pump=211, ui=595, \
         prepare=1190, finish=2379, post=4757; acquire n=2, p50=3364 us, \
         p99=5657 us",
    );
}

/// The prep-costs line carries the pass count (its non-vacuity floor) and
/// each phase total under its own name, distinct values in every position.
#[test]
fn the_prep_costs_line_reads_exactly_as_pinned() {
    let costs = PassCosts {
        passes: 7,
        tessellate_us: 1_111,
        upload_apply_us: 2_222,
        mirror_us: 3_333,
        buffers_and_callbacks_us: 4_444,
    };
    assert_eq!(
        super::prep_costs_line(&costs),
        "frame prep costs: 7 passes, 1111 us tessellate, 2222 us upload apply, \
         3333 us mirror, 4444 us buffers and callbacks",
    );
}

/// The prep-geometry line carries its own non-vacuity floor and three
/// distinct totals, so a transposed field cannot read as a correct line.
///
/// `stagings` deliberately exceeds what a pass count would be: eleven
/// stagings is a run in which some passes rendered a pane mirror and staged
/// twice. A formatter that printed `passes` here instead could not pass.
#[test]
fn the_prep_geometry_line_reads_exactly_as_pinned() {
    let staged = StagedGeometry {
        calls: 11,
        vertices: 2222,
        indices: 3333,
        bytes: 57_776,
    };
    let routes = GeometryStagingTotals {
        staged: 9,
        declined: 2,
        bytes: 57_776,
    };
    assert_eq!(
        super::prep_geometry_line(&staged, &routes),
        "frame prep geometry: 11 stagings, 2222 vertices, 3333 indices, \
         57776 B staged, 9 through the ring, 2 declined",
    );
}

/// The cadence line embeds its histogram like the interact line does, with
/// the samples in the slots the edge formula puts them: 4 000 us in
/// geometric bin 24 (slot 25), 16 667 us in bin 32 (slot 33).
#[test]
fn the_cadence_line_embeds_the_whole_histogram() {
    let mut h = Hist::new();
    h.record(4_000);
    h.record(4_000);
    h.record(16_667);
    let mut slots = [0u32; 42];
    slots[25] = 2;
    slots[33] = 1;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::frame_cadence_line(&h),
        format!("frame cadence: n=3, p50=4757 us, p99=19028 us, hist={expected_hist}"),
    );
}

/// The gpu-passes line names every family with its own pass count and its
/// own percentiles, distinct values in every position, and carries the
/// collected-frames floor last. The raymarch `n` deliberately exceeds its
/// sample count — six panes are six encoded passes but one bracketed sample
/// per frame — so a formatter that conflated the two denominators could not
/// pass. Edges derived by hand as above: 1 000 us -> 1190, 100 -> 106,
/// 4 000 -> 4757.
#[test]
fn the_gpu_passes_line_reads_exactly_as_pinned() {
    use squallar_gpu::gpu_probe::{GpuPassReport, ProbedPass};
    let mut report = GpuPassReport {
        hists: [Hist::new(); 4],
        passes: [6, 0, 1, 2],
        frames: 3,
        non_monotone: [0; 4],
    };
    report.hists[ProbedPass::Raymarch as usize].record(1_000);
    report.hists[ProbedPass::Mirror as usize].record(100);
    report.hists[ProbedPass::Main as usize].record(4_000);
    report.hists[ProbedPass::Main as usize].record(4_000);
    assert_eq!(
        super::gpu_passes_line(&report),
        "gpu passes: raymarch n=6, p50=1190 us, p99=1190 us; \
         ground n=0, p50=none us, p99=none us; \
         mirror n=1, p50=106 us, p99=106 us; \
         main n=2, p50=4757 us, p99=4757 us; 3 frames",
    );
}

/// The absence sentence, pinned verbatim: it is what every WebGL2 leg says
/// in place of figures, and a probe scraping for it must find these exact
/// words — an absence stated as one, never an extrapolation.
#[test]
fn the_gpu_passes_absence_line_is_pinned_verbatim() {
    assert_eq!(
        super::GPU_PASSES_UNAVAILABLE_LINE,
        "gpu passes: unavailable (adapter lacks TIMESTAMP_QUERY)",
    );
}

/// **The rig reads the frame lines the app actually writes** — every family,
/// against `drive.py`'s own patterns, with a distinct value in each position
/// so a transposed pair cannot read as a correct line. The same fixtures as
/// the literal pins above, so the two ends of the seam pin one sentence.
#[test]
fn the_rig_reads_the_frame_lines_the_app_actually_writes() {
    let mut interact = Hist::new();
    interact.record(10);
    interact.record(100_000);
    let hist = counts_string(&interact);
    assert_eq!(
        super::frame_service_interact_line(&interact),
        rendered(
            &pattern("svc_interact_re"),
            &["2", "63", "over", "over", &hist],
        ),
        "the `frame service (interact):` line and the rig's probe have drifted",
    );

    let mut idle = Hist::new();
    for _ in 0..3 {
        idle.record(100);
    }
    let idle_hist = counts_string(&idle);
    assert_eq!(
        super::frame_service_idle_line(&idle),
        rendered(
            &pattern("svc_idle_re"),
            &["3", "106", "106", "106", &idle_hist],
        ),
        "the `frame service (idle):` line and the rig's probe have drifted",
    );

    let mut s = crate::frame_ledger::SegmentHists::default();
    s.pre.record(100);
    s.pump.record(200);
    s.ui.record(500);
    s.prepare.record(1_000);
    s.finish.record(2_000);
    s.post.record(4_000);
    let mut acquire = Hist::new();
    acquire.record(3_000);
    acquire.record(5_000);
    assert_eq!(
        super::frame_segments_line(&s, &acquire),
        rendered(
            &pattern("segments_re"),
            &[
                "106", "211", "595", "1190", "2379", "4757", "2", "3364", "5657"
            ],
        ),
        "the `frame segments` line and the rig's probe have drifted",
    );

    let costs = PassCosts {
        passes: 7,
        tessellate_us: 1_111,
        upload_apply_us: 2_222,
        mirror_us: 3_333,
        buffers_and_callbacks_us: 4_444,
    };
    assert_eq!(
        super::prep_costs_line(&costs),
        rendered(
            &pattern("prep_costs_re"),
            &["7", "1111", "2222", "3333", "4444"],
        ),
        "the `frame prep costs:` line and the rig's probe have drifted",
    );

    let staged = StagedGeometry {
        calls: 11,
        vertices: 2222,
        indices: 3333,
        bytes: 57_776,
    };
    let routes = GeometryStagingTotals {
        staged: 9,
        declined: 2,
        bytes: 57_776,
    };
    assert_eq!(
        super::prep_geometry_line(&staged, &routes),
        rendered(
            &pattern("prep_geometry_re"),
            &["11", "2222", "3333", "57776", "9", "2"],
        ),
        "the `frame prep geometry:` line and the rig's probe have drifted",
    );

    use squallar_gpu::gpu_probe::{GpuPassReport, ProbedPass};
    let mut report = GpuPassReport {
        hists: [Hist::new(); 4],
        passes: [6, 0, 1, 2],
        frames: 3,
        non_monotone: [0; 4],
    };
    report.hists[ProbedPass::Raymarch as usize].record(1_000);
    report.hists[ProbedPass::Mirror as usize].record(100);
    report.hists[ProbedPass::Main as usize].record(4_000);
    report.hists[ProbedPass::Main as usize].record(4_000);
    assert_eq!(
        super::gpu_passes_line(&report),
        rendered(
            &pattern("gpu_passes_re"),
            &[
                "6", "1190", "1190", "0", "none", "none", "1", "106", "106", "2", "4757", "4757",
                "3",
            ],
        ),
        "the `gpu passes:` line and the rig's probe have drifted",
    );

    let mut cadence = Hist::new();
    cadence.record(4_000);
    cadence.record(4_000);
    cadence.record(16_667);
    let hist = counts_string(&cadence);
    assert_eq!(
        super::frame_cadence_line(&cadence),
        rendered(&pattern("cadence_re"), &["3", "4757", "19028", &hist]),
        "the `frame cadence:` line and the rig's probe have drifted",
    );
}

/// The floor under the seam test above: `rendered` really can disagree.
/// Without it a `pattern` that returned the sentence itself, or a `rendered`
/// that ignored its argument, would hold the equality whatever the app wrote.
#[test]
fn a_frame_line_that_drifted_by_one_space_is_not_accepted() {
    let empty_hist = counts_string(&Hist::new());
    let good = rendered(
        &pattern("svc_idle_re"),
        &["0", "none", "none", "none", &empty_hist],
    );
    assert_eq!(super::frame_service_idle_line(&Hist::new()), good);
    let drifted = good.replacen(" us", "  us", 1);
    assert_ne!(drifted, good, "the perturbation perturbed nothing");
    assert_ne!(
        super::frame_service_idle_line(&Hist::new()),
        drifted,
        "a line with one extra space compared equal to the real one, so the \
         seam test above cannot fail",
    );
}

/// The absence sentence's other end: the rig scans for it verbatim, so the
/// exact words must appear in `drive.py` — an absence a probe cannot find is
/// reported as `null`, which reads as "the probe never ran".
#[test]
fn the_rig_scans_for_the_absence_sentence_verbatim() {
    assert!(
        DRIVE_PY.contains(super::GPU_PASSES_UNAVAILABLE_LINE),
        "drive.py no longer scans for {:?}",
        super::GPU_PASSES_UNAVAILABLE_LINE,
    );
}

/// The key is the sentence's other half: a rig leg seeds the `localStorage`
/// name derived from this literal, so a rename here mutes every frame line
/// on the web with no error anywhere. Pinned the way the sentences are.
#[test]
fn the_frame_telemetry_key_is_pinned() {
    assert_eq!(super::FRAME_TELEMETRY_KEY, "frame_telemetry");
}

/// The rig launchers, read at compile time the way [`DRIVE_PY`] is.
const RUN_TIER2: &str = include_str!("../../../.github/browser-rig/run_tier2.sh");
const RUN_MEASURE: &str = include_str!("../../../.github/browser-rig/run_measure.sh");

/// **Both rig launchers seed the key that makes the frame lines loud.**
///
/// Same seam as the raster switch's
/// `the_rig_seeds_the_key_that_makes_the_lines_loud`: the frame lines are
/// `debug` unseeded and `console_log` boots at `Info`, so without this seed
/// the Tier-2 gesture leg's count assert and every run_measure.sh window
/// read the interact family as never-written.
#[test]
fn the_rig_seeds_the_key_that_makes_the_frame_lines_loud() {
    let seeded = format!("\"squallar.{}\": \"1\"", super::FRAME_TELEMETRY_KEY);
    for (name, text) in [("run_tier2.sh", RUN_TIER2), ("run_measure.sh", RUN_MEASURE)] {
        assert!(
            text.contains(&seeded),
            "{name} no longer seeds {seeded}, so the app writes every frame \
             line at `debug`, the console ring never hears them, and the \
             interact count reads as never-written",
        );
    }
}

/// **run_measure.sh arms only scripts this build knows.** The seed key is
/// [`super::GESTURE_SCRIPT_KEY`]'s `localStorage` spelling, and each name it
/// seeds must resolve through the player's own vocabulary — a renamed script
/// on either side would leave a scene silently unarmed, which the rows would
/// only reveal as a missing gesture window.
#[test]
fn the_measure_rig_arms_scripts_this_build_knows() {
    use squallar_egui::gesture_player::GestureScript;

    let key = format!("\"squallar.{}\": \"", super::GESTURE_SCRIPT_KEY);
    let mut seeded = Vec::new();
    for at in RUN_MEASURE
        .match_indices(&key)
        .map(|(at, _)| at + key.len())
    {
        let rest = &RUN_MEASURE[at..];
        let end = rest.find('"').expect("the seed value is quoted");
        seeded.push(&rest[..end]);
    }
    assert!(
        !seeded.is_empty(),
        "run_measure.sh no longer seeds {key}…, so no scene arms the player \
         and every row loses its gesture window",
    );
    for name in &seeded {
        assert!(
            GestureScript::from_name(name).is_some(),
            "run_measure.sh seeds gesture script {name:?}, which this build \
             does not know; that scene boots with a disarmed player",
        );
    }
}

/// Only a stored `"1"` makes the frame lines loud — the same one-value
/// contract as the raster switch, held for this key on its own store.
#[test]
fn only_the_seeded_value_makes_the_frame_lines_loud() {
    use squallar_kv::{KvStore, MemoryKvStore};

    assert!(
        !super::frame_telemetry_is_loud(None),
        "an install with nowhere to persist must not be loud",
    );

    let store = MemoryKvStore::default();
    assert!(
        !super::frame_telemetry_is_loud(Some(&store)),
        "an install that never set the key must not be loud",
    );

    store
        .store(super::FRAME_TELEMETRY_KEY, "0")
        .expect("the memory store always accepts a write");
    assert!(
        !super::frame_telemetry_is_loud(Some(&store)),
        "an explicit `0` turned the frame lines on",
    );

    store
        .store(super::RASTER_TELEMETRY_KEY, "1")
        .expect("the memory store always accepts a write");
    assert!(
        !super::frame_telemetry_is_loud(Some(&store)),
        "the raster key turned the frame lines on; the two instruments are \
         separate switches and a rig leg seeds only the ones it reads",
    );

    store
        .store(super::FRAME_TELEMETRY_KEY, "1")
        .expect("the memory store always accepts a write");
    assert!(
        super::frame_telemetry_is_loud(Some(&store)),
        "the one value that turns the frame lines on did not",
    );
}

// ── The two windowable families ────────────────────────────────────────────
//
// `frame segment (<name>)` and `tile take (<name>)` share one formatter
// (`named_hist_line`) and therefore one sentence shape, pinned here at both
// ends like the five above it.

/// The `frame segment (…)` sentence, pinned as a literal.
///
/// 100 us sits in [88 388, 105 112) ns — geometric bin slot 3, upper edge
/// 106 us — derived from the documented edge formula, not read back from the
/// code under test. `sum` is the exact arithmetic sum of what was recorded,
/// and it is deliberately NOT any percentile: 3 × 100 = 300, while every
/// percentile answers the bin's 106 us upper edge. A line that printed a
/// percentile where the sum belongs would still look plausible.
#[test]
fn the_frame_segment_line_reads_exactly_as_pinned() {
    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let mut slots = [0u32; 42];
    slots[3] = 3;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::named_hist_line("frame segment", "pump", &h),
        format!(
            "frame segment (pump): n=3, sum=300 us, p50=106 us, p90=106 us, \
             p99=106 us, hist={expected_hist}"
        ),
    );
}

/// **The sum is not derivable from the bins, which is the whole point.**
///
/// Two histograms with identical bin contents and different true costs: at
/// four bins per octave a bin is ≈19% wide, so 90 us and 105 us are the same
/// bin and every percentile of the two is byte-identical. Only the sum tells
/// them apart. This is the non-triviality floor under `sum=`: without it, a
/// formatter that printed a percentile there would pass every other test in
/// this file.
#[test]
fn the_sum_separates_two_histograms_the_bins_cannot() {
    let (mut low, mut high) = (Hist::new(), Hist::new());
    for _ in 0..4 {
        low.record(90);
        high.record(105);
    }
    assert_eq!(
        low.counts(),
        high.counts(),
        "the two samples landed in different bins, so this test is not about \
         what it says it is about",
    );
    assert_eq!(
        low.percentile_upper_micros(0.99),
        high.percentile_upper_micros(0.99)
    );
    assert_ne!(
        low.sum_micros(),
        high.sum_micros(),
        "two histograms the bins cannot separate also have equal sums, so the \
         running sum buys nothing",
    );
    assert_eq!((low.sum_micros(), high.sum_micros()), (360, 420));
    assert_ne!(
        super::named_hist_line("frame segment", "ui", &low),
        super::named_hist_line("frame segment", "ui", &high),
        "the two lines are identical, so a 17% cost difference is invisible \
         to every reader of this instrument",
    );
}

/// All six segments are emitted, under their own names, every tick.
///
/// A count and an identity: six lines, six distinct names, each naming the
/// segment whose histogram it carries. The `n` conjunct is what stops a
/// mis-wired call from reading green — `pre` carrying `ui`'s histogram would
/// keep the names right and the figures wrong.
#[test]
fn every_frame_segment_is_reported_under_its_own_name() {
    let mut segments = crate::frame_ledger::SegmentHists::default();
    // A different sample count per segment, so a swapped pair cannot pass.
    for (slot, hist) in [
        &mut segments.pre,
        &mut segments.pump,
        &mut segments.ui,
        &mut segments.prepare,
        &mut segments.finish,
        &mut segments.post,
    ]
    .into_iter()
    .enumerate()
    {
        for _ in 0..=slot {
            hist.record(1_000);
        }
    }
    let lines = super::frame_segment_lines(&segments);
    let names = ["pre", "pump", "ui", "prepare", "finish", "post"];
    assert_eq!(lines.len(), names.len());
    for (slot, (line, name)) in lines.iter().zip(names).enumerate() {
        assert!(
            line.starts_with(&format!("frame segment ({name}): n={}, ", slot + 1)),
            "segment {slot} reported as {line:?}, which is not {name}'s line \
             carrying {name}'s histogram",
        );
    }
}

/// The `frame prepare (…)` sentence, pinned as a literal.
///
/// Same formatter as `frame segment (…)` and deliberately a **different
/// prefix**: the six are cuts of the prepare segment, and a reader who could
/// mistake one for a seventh segment would add it to the very span it
/// decomposes. The `sum=` is the load-bearing field for the same reason it is
/// on the segment line — 3 × 100 = 300, where every percentile of that
/// histogram answers the bin's 106 us upper edge.
#[test]
fn the_frame_prepare_line_reads_exactly_as_pinned() {
    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let mut slots = [0u32; 42];
    slots[3] = 3;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::named_hist_line("frame prepare", "tessellate", &h),
        format!(
            "frame prepare (tessellate): n=3, sum=300 us, p50=106 us, \
             p90=106 us, p99=106 us, hist={expected_hist}"
        ),
    );
}

/// All six prepare cuts are emitted, under their own names, every tick — and
/// each carries its own histogram.
///
/// The `n` conjunct is what stops a mis-wired call from reading green: `plan`
/// carrying `upload`'s histogram would keep every name right and every figure
/// wrong, which is exactly the failure a split is most likely to have.
#[test]
fn every_prepare_phase_is_reported_under_its_own_name() {
    let mut phases = crate::frame_ledger::PrepareHists::default();
    // A different sample count per cut, so a swapped pair cannot pass.
    for (slot, hist) in [
        &mut phases.plan,
        &mut phases.end_pass,
        &mut phases.tessellate,
        &mut phases.upload,
        &mut phases.mirror,
        &mut phases.buffers,
    ]
    .into_iter()
    .enumerate()
    {
        for _ in 0..=slot {
            hist.record(1_000);
        }
    }
    let lines = super::frame_prepare_lines(&phases);
    let names = [
        "plan",
        "end-pass",
        "tessellate",
        "upload",
        "mirror",
        "buffers",
    ];
    assert_eq!(lines.len(), names.len());
    for (slot, (line, name)) in lines.iter().zip(names).enumerate() {
        assert!(
            line.starts_with(&format!("frame prepare ({name}): n={}, ", slot + 1)),
            "prepare cut {slot} reported as {line:?}, which is not {name}'s \
             line carrying {name}'s histogram",
        );
    }
}

/// **The prepare cuts do not collide with the segment lines the rig already
/// reads.** Both use `named_hist_line` and both name a `prepare`; the rig keys
/// its families on the prefix, so a shared one would file six cuts of a span
/// alongside the span itself under one name and let a reader sum them.
#[test]
fn the_prepare_cuts_are_not_readable_as_frame_segments() {
    let phases = crate::frame_ledger::PrepareHists::default();
    let segments = crate::frame_ledger::SegmentHists::default();
    let prepare_lines = super::frame_prepare_lines(&phases);
    let segment_lines = super::frame_segment_lines(&segments);

    for line in &prepare_lines {
        assert!(
            !line.starts_with("frame segment ("),
            "a prepare cut is written as a frame segment ({line:?}); the rig \
             would file it beside the six that telescope to service",
        );
    }
    assert!(
        segment_lines
            .iter()
            .any(|line| line.starts_with("frame segment (prepare): ")),
        "the segment line these cuts decompose is no longer written, so \
         nothing carries the total they sum to",
    );
}

/// The rig's `frame prepare` probe reads what the app writes, character for
/// character — the same both-ends pin the segment and tile lines carry.
#[test]
fn the_rig_reads_the_prepare_lines_the_app_actually_writes() {
    let mut h = Hist::new();
    h.record(100);
    h.record(4_000);
    let hist = counts_string(&h);
    assert_eq!(
        super::named_hist_line("frame prepare", "end-pass", &h),
        rendered(
            &pattern("frame_prepare_re"),
            &["end-pass", "2", "4100", "106", "4757", "4757", &hist],
        ),
        "the `frame prepare (…)` line and the rig's probe have drifted",
    );
}

/// The rig's `frame ui` probe reads what the app writes.
///
/// **This gate is why the family exists on the wire at all.** The app has
/// written six `frame ui (…)` cuts since the ui split landed, and the rig
/// scraped none of them: there was no `frame_ui_re`, and `ui:` was missing from
/// the window prefixes, so the LARGEST segment of an interact frame was the one
/// nobody could see. The conviction chain went to `post` partly because `post`
/// was decomposed and visible and this was not.
#[test]
fn the_rig_reads_the_ui_lines_the_app_actually_writes() {
    let mut h = Hist::new();
    h.record(100);
    h.record(4_000);
    let hist = counts_string(&h);
    assert_eq!(
        super::named_hist_line("frame ui", "layout", &h),
        rendered(
            &pattern("frame_ui_re"),
            &["layout", "2", "4100", "106", "4757", "4757", &hist],
        ),
        "the `frame ui (…)` line and the rig's probe have drifted",
    );
}

/// The rig's `frame pre` probe reads what the app writes.
///
/// `pre` was the last segment in the file with no split, and the only one a
/// reader could not open: mean 325.6 µs, p99 1,682 µs and 10,357 µs on one
/// latched frame on scene D. A family the app writes and the rig has no regex
/// for is ABSENT from the artifact rather than empty, which is what kept
/// `frame ui (…)` — the largest segment of an interact frame — invisible for
/// weeks.
#[test]
fn the_rig_reads_the_pre_lines_the_app_actually_writes() {
    let mut h = Hist::new();
    h.record(100);
    h.record(4_000);
    let hist = counts_string(&h);
    assert_eq!(
        super::named_hist_line("frame pre", "ingest", &h),
        rendered(
            &pattern("frame_pre_re"),
            &["ingest", "2", "4100", "106", "4757", "4757", &hist],
        ),
        "the `frame pre (…)` line and the rig's probe have drifted",
    );
    // **`frame pre` and `frame prepare` are two families, not one.** Their
    // names share a prefix, so a probe anchored on `frame pre` alone would
    // swallow every `frame prepare (…)` line and file six cuts of the wrong
    // segment under `pre:`. Held on the fixed heads of the two patterns
    // against the two sentences, which is the whole of what separates them —
    // no regex engine needed, and none is a dependency here.
    let head = |name: &str| {
        pattern(name)
            .split_once(r"\(")
            .expect("a family probe opens with `<prefix> \\(`")
            .0
            .to_string()
    };
    let (pre_head, prepare_head) = (head("frame_pre_re"), head("frame_prepare_re"));
    assert_eq!(
        (pre_head.as_str(), prepare_head.as_str()),
        ("frame pre ", "frame prepare ")
    );
    let prepare_line = super::named_hist_line("frame prepare", "plan", &h);
    let pre_line = super::named_hist_line("frame pre", "platform", &h);
    assert!(
        !prepare_line.contains(&format!("{pre_head}(")),
        "the `frame pre` probe's head occurs inside a `frame prepare` line, \
         so the two families would be scraped into one: {prepare_line:?}",
    );
    assert!(
        !pre_line.contains(&format!("{prepare_head}(")),
        "the `frame prepare` probe's head occurs inside a `frame pre` line: \
         {pre_line:?}",
    );
}

/// **Every cut family the app writes is one the rig both scrapes and windows.**
///
/// The per-family gates above each pin one name, so a family added with no
/// probe at all passes every one of them by not being mentioned. That is how
/// `ui` stayed invisible: nothing asserted the SET matched. This does, so the
/// next family cannot arrive half-wired.
#[test]
fn every_frame_cut_family_the_app_writes_is_scraped_and_windowed_by_the_rig() {
    for (family, key) in [
        ("frame pre", "pre"),
        ("frame prepare", "prepare"),
        ("frame ui", "ui"),
        // One level below `frame ui (stack)`. It is a cut family like the
        // rest and is subject to both halves of this gate: a probe that reads
        // it, and a window prefix that keeps it once read.
        ("frame stack", "stack"),
        // The same, one cut across: the split below `frame ui (panes)`.
        ("frame panes", "panes"),
        ("frame post", "post"),
        ("frame pump", "pump"),
        ("frame dispatch", "dispatch"),
        // Not a cut family — it is `frame service`'s own two families with
        // the `finish` split's eighth cut taken out — but it is carried on
        // the named-family wire for the `sum` a window needs, so it is
        // subject to both halves of this gate exactly as the cuts are.
        ("frame service less present", "lesspresent"),
    ] {
        let re = family.replace(' ', "_") + "_re";
        assert!(
            DRIVE_PY.contains(&re),
            "the app writes `{family} (…)` lines and the rig has no `{re}` to \
             read them, so the family is invisible to every leg",
        );
        assert!(
            DRIVE_PY.contains(&format!("\"{key}:\"")),
            "`{key}:` is not in the rig's window prefixes, so `{family}` is \
             scraped and then dropped before any window reports it",
        );
    }
}

/// **No dispatch site asks `== RenderMode::Texture`; every one asks
/// `has_texture()`.**
///
/// Three sites in this crate compared the mode to `Texture` by equality, so
/// when `TextureAndPoint` landed the dispatcher skipped it: METAR's geometry
/// left the frame thread and arrived nowhere — an 81 % drop in vertices per
/// frame that read as a win until the picture-dispatch log was checked and
/// `overlay/metar` appeared zero times. The overlays crate had the same idiom
/// in six pins and they were widened; these three were one crate over and
/// were not. This is the gate that would have caught it.
#[test]
fn every_dispatch_site_asks_has_texture_not_texture_equality() {
    for (name, src) in [
        ("app_fetch.rs", include_str!("../app_fetch.rs")),
        ("app_render.rs", include_str!("../app_render.rs")),
    ] {
        let hits: Vec<usize> = src
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                l.contains("RenderMode::Texture)") && !l.trim_start().starts_with("//")
            })
            .map(|(i, _)| i + 1)
            .collect();
        assert!(
            hits.is_empty(),
            "{name} compares a render mode to `Texture` by equality at lines \
             {hits:?}; a hybrid layer has a texture too, and this idiom is how \
             its picture stopped being dispatched. Ask `has_texture()`.",
        );
    }
}

/// The rig's `frame worst` probe reads what the app writes.
///
/// **This is the p99 instrument, and no browser leg has ever seen it.** Phase 1
/// landed `frame worst:` — one frame's six segments plus the never-cleared
/// since-boot maximum, outside the `if interacted` arm — precisely because the
/// scene D verdict is p99 AND max, and every windowed histogram is a
/// distribution that cannot name the frame. The rig had no regex for it. The
/// enumeration that found this (`"frame [a-z]+` in `app_render.rs` against
/// `var frame_[a-z]+_re` in `drive.py`) read ten families against five.
#[test]
fn the_rig_reads_the_worst_frame_line_the_app_actually_writes() {
    // Nine DISTINCT ui cuts, summing to this frame's own `ui` of 9514: a
    // fixture with repeats could not tell a transposed pair of columns from a
    // correct one, and the positional group order is exactly what this test
    // exists to hold.
    let w = crate::frame_ledger::WorstFrame {
        service: 13_455,
        segments: [64, 55, 9_514, 2_829, 700, 293],
        ui_cuts: [11, 402, 1_207, 96, 6_902, 4, 812, 3, 77],
        // Seven DISTINCT stack cuts summing to this frame's own `ui_stack`
        // of 6902 — the fifth ui cut, opened up — for the nine's reason
        // exactly: a repeat could not tell a transposed pair of columns from
        // a correct one, and these seven sit between the ui nine and the pre
        // seven in a positional regex, so a miscount lands here as plausible
        // integers rather than as an absence.
        stack_cuts: [21, 96, 340, 1_180, 4_100, 900, 265],
        // Seven DISTINCT pre cuts summing to this frame's own `pre` of 64,
        // for the ui nine's reason: a repeat could not tell a transposed pair
        // of columns from a correct one.
        pre_cuts: [3, 21, 9, 14, 2, 7, 8],
        interact: true,
    };
    // The since-boot maximum is a whole frame too: a boot-time compile spike
    // reads here as `boot: idle, prepare=<nearly all>` rather than as a bare
    // number nobody can attribute.
    let boot = crate::frame_ledger::WorstFrame {
        service: 22_628,
        segments: [100, 90, 300, 21_000, 800, 338],
        ui_cuts: [7, 19, 41, 5, 133, 2, 61, 1, 31],
        stack_cuts: [3, 8, 14, 27, 61, 12, 8],
        pre_cuts: [4, 31, 12, 20, 6, 9, 18],
        interact: false,
    };
    assert_eq!(
        w.ui_cuts.iter().sum::<u32>(),
        w.segments[2],
        "the fixture's ui cuts do not telescope to its ui, so the pin below \
         would pin a line describing no frame that could exist",
    );
    assert_eq!(boot.ui_cuts.iter().sum::<u32>(), boot.segments[2]);
    assert_eq!(
        w.pre_cuts.iter().sum::<u32>(),
        w.segments[0],
        "the fixture's pre cuts do not telescope to its pre, so the pin below \
         would pin a line describing no frame that could exist",
    );
    assert_eq!(boot.pre_cuts.iter().sum::<u32>(), boot.segments[0]);
    // And one level further down: the seven telescope to the FIFTH ui cut,
    // not to `ui`. A fixture whose stack cuts summed to `ui` would pin a line
    // describing a frame that cannot exist.
    assert_eq!(
        w.stack_cuts.iter().sum::<u32>(),
        w.ui_cuts[4],
        "the fixture's stack cuts do not telescope to its ui_stack, so the \
         pin below would pin a line whose stack_* columns decompose no frame",
    );
    assert_eq!(boot.stack_cuts.iter().sum::<u32>(), boot.ui_cuts[4]);
    assert_eq!(
        super::frame_worst_line(Some(w), Some(boot)),
        rendered(
            &pattern("frame_worst_re"),
            &[
                "13455", "interact", "22628", "64", "55", "9514", "2829", "700", "293", "11",
                "402", "1207", "96", "6902", "4", "812", "3", "77", "21", "96", "340", "1180",
                "4100", "900", "265", "3", "21", "9", "14", "2", "7", "8", "idle", "100", "90",
                "300", "21000", "800", "338", "7", "19", "41", "5", "133", "2", "61", "1", "31",
                "3", "8", "14", "27", "61", "12", "8", "4", "31", "12", "20", "6", "9", "18",
            ],
        ),
        "the `frame worst:` line and the rig's probe have drifted",
    );
    assert_eq!(
        super::frame_worst_line(None, Some(boot)),
        rendered(
            &pattern("frame_worst_none_re"),
            &[
                "22628", "idle", "100", "90", "300", "21000", "800", "338", "7", "19", "41", "5",
                "133", "2", "61", "1", "31", "3", "8", "14", "27", "61", "12", "8", "4", "31",
                "12", "20", "6", "9", "18",
            ],
        ),
        "the no-frame spelling and the rig's probe have drifted",
    );
}

/// The two `frame service less present (…)` sentences, pinned as literals and
/// against the rig's own pattern.
///
/// **The two service sentences must not read each other's lines.** `frame
/// service (interact)` and `frame service less present (interact)` share a
/// prefix and end in the same parenthesised family word, so a probe anchored
/// on the shorter one would file a figure with the present taken OUT under the
/// name of the figure the campaign's bar is stated in — the exact confusion
/// the longer name exists to prevent, and one that reads as data rather than
/// as a null. Held on the fixed heads of the patterns against both sentences,
/// on `the_rig_reads_the_pre_lines_the_app_actually_writes`' terms.
#[test]
fn the_rig_reads_the_service_less_present_lines_the_app_actually_writes() {
    let mut h = Hist::new();
    h.record(100);
    h.record(4_000);
    let hist = counts_string(&h);
    let expected_hist = hist.clone();
    let [interact, idle] = super::frame_service_less_present_lines(&h, &Hist::new());
    assert_eq!(
        interact,
        format!(
            "frame service less present (interact): n=2, sum=4100 us, \
             p50=106 us, p90=4757 us, p99=4757 us, hist={expected_hist}"
        ),
    );
    assert_eq!(
        idle,
        format!(
            "frame service less present (idle): n=0, sum=0 us, p50=none us, \
             p90=none us, p99=none us, hist={}",
            ["0"; 42].join(","),
        ),
        "an empty family must print a zero, not an absence",
    );
    assert_eq!(
        interact,
        rendered(
            &pattern("frame_service_less_present_re"),
            &["interact", "2", "4100", "106", "4757", "4757", &hist],
        ),
        "the `frame service less present (…)` line and the rig's probe have \
         drifted",
    );

    // The two service sentences, held apart on the probes' fixed heads.
    let head = |name: &str| {
        pattern(name)
            .split_once(r"\(")
            .expect("a family probe opens with `<prefix> \\(`")
            .0
            .to_string()
    };
    let (service_head, less_head) = (
        head("svc_interact_re"),
        head("frame_service_less_present_re"),
    );
    assert_eq!(
        (service_head.as_str(), less_head.as_str()),
        ("frame service ", "frame service less present ")
    );
    assert!(
        !interact.contains(&format!("{service_head}(")),
        "the `frame service` probe's head occurs inside a `frame service less \
         present` line, so the figure with the present taken out would be \
         scraped as the one the bar is stated in: {interact:?}",
    );
    let plain = super::frame_service_interact_line(&h);
    assert!(
        !plain.contains(&format!("{less_head}(")),
        "the `frame service less present` probe's head occurs inside a \
         `frame service` line: {plain:?}",
    );
}

/// The body of `App::report_frame_telemetry`, sliced out of `app_render.rs`.
///
/// The method sits in an `impl` block, so its closing brace is the first `}`
/// at four-space indent after its signature; every block inside it closes at
/// eight or more. Read off the source rather than brace-counted, because a
/// brace inside one of the body's own comments would unbalance a counter and
/// silently hand back half a body — a shorter body can only make the gate
/// below fire, never pass, but it would fire for the wrong reason.
fn report_frame_telemetry_body() -> &'static str {
    const HEAD: &str = "fn report_frame_telemetry(&mut self) {";
    let at = APP_RENDER.find(HEAD).expect(
        "app_render.rs no longer declares `report_frame_telemetry`; the frame \
         telemetry tick moved and this test can no longer read it",
    );
    let rest = &APP_RENDER[at + HEAD.len()..];
    let end = rest
        .find("\n    }\n")
        .expect("`report_frame_telemetry` does not close at four-space indent");
    &rest[..end]
}

/// **Every `frame …` line formatter in `app_render.rs` is one the report
/// actually says.**
///
/// A formatter, its literal pin, its rig regex, its rename-list entry and its
/// window prefix can all be in place while **nothing calls it**, and every
/// other gate in this file still passes: the pins format a line and compare
/// it, the family enumeration reads the `"frame …` literal out of the source,
/// and the probe reads a sentence that is never written. The family is then
/// absent from every leg for the reason absence is hardest to see — an
/// unwritten family and an arm that produced no samples look identical in the
/// artifact, and this campaign has twice read the second when it had the
/// first.
///
/// The count is a **floor, not a pin**: families land and it climbs. It is
/// here so that an extraction which quietly matches nothing — the shape a
/// rename of the formatters would take — fails loudly instead of passing over
/// an empty list.
#[test]
fn every_frame_line_formatter_is_one_the_report_says() {
    const KNOWN_FORMATTER_FLOOR: usize = 16;
    let body = report_frame_telemetry_body();
    let mut names: Vec<&str> = APP_RENDER
        .match_indices("\nfn frame_")
        .map(|(at, _)| {
            let rest = &APP_RENDER[at + 1..];
            let end = rest.find('(').unwrap_or(rest.len());
            &rest[3..end]
        })
        .filter(|name| name.ends_with("_line") || name.ends_with("_lines"))
        .collect();
    names.sort_unstable();
    names.dedup();
    assert!(
        names.len() >= KNOWN_FORMATTER_FLOOR,
        "only {} `frame …` line formatters were found in app_render.rs, under \
         the {KNOWN_FORMATTER_FLOOR} known to exist: the extraction has \
         stopped matching and this gate is passing over an empty list: \
         {names:?}",
        names.len(),
    );
    for name in &names {
        assert!(
            body.contains(&format!("{name}(")),
            "`{name}` formats a `frame …` sentence that \
             `report_frame_telemetry` never says, so the family is ABSENT \
             from every leg's artifact — indistinguishable there from an arm \
             that produced no samples",
        );
    }
}

/// **Every `frame <name>` line family the app writes has a rig probe that reads
/// it — by an explicit table, so a NEW family fails here until it is claimed.**
///
/// The per-family pins each hold one probe in step with one formatter, so a
/// family with no probe at all passes every one of them by never being named.
/// That is how `ui` (the largest segment) and then `worst` (the p99 instrument)
/// each went unscraped for weeks. This enumerates the app's own `"frame …`
/// literals and demands a named regex for each; an unlisted family reddens it.
#[test]
fn every_frame_line_family_the_app_writes_has_a_named_rig_probe() {
    const APP: &str = include_str!("../app_render.rs");
    let carried_by: &[(&str, &[&str])] = &[
        ("cadence", &["cadence_re"]),
        ("dispatch", &["frame_dispatch_re"]),
        ("finish", &["frame_finish_re"]),
        // The last segment to get a split, and a top-three tail owner while
        // it had none. `frame pre` and `frame prepare` are DIFFERENT families
        // whose names share a prefix; the enumeration below reads to the
        // first non-lowercase character, so the two are separate rows here
        // and separate regexes in drive.py.
        ("pre", &["frame_pre_re"]),
        // The verdict family — frames drawn against frames that needed
        // drawing. One probe, and its groups are all mandatory: an unlisted
        // family is invisible to every leg, which is what this table exists
        // to stop.
        ("need", &["frame_need_re"]),
        ("post", &["frame_post_re"]),
        ("prep", &["prep_costs_re", "prep_geometry_re"]),
        ("prepare", &["frame_prepare_re"]),
        ("pump", &["frame_pump_re"]),
        ("segment", &["frame_segment_re"]),
        ("segments", &["segments_re"]),
        // `frame ui (stack)`, opened up. A THIRD prefix on purpose: a reader
        // pattern-matching `frame ui` must not be able to sum two levels of
        // the same span, so this family is neither `ui` nor a `ui` cut name.
        ("stack", &["frame_stack_re"]),
        // `frame ui (panes)`, opened up — `stack`'s sibling one cut across,
        // and a fourth prefix for the third one's reason: `panes` is also a
        // `ui` CUT NAME, so a probe anchored on anything looser than the
        // literal `frame panes (` would scrape `frame ui (panes)` as one of
        // its own eight.
        ("panes", &["frame_panes_re"]),
        // Three probes, one family word. `frame service less present (…)`
        // is the SAME `service` family word — the enumeration below reads to
        // the first non-lowercase character — and it is deliberately the
        // same: it is `frame service`'s two lines with one cut taken out,
        // over `frame service`'s own frames, and never a family beside them.
        (
            "service",
            &[
                "svc_interact_re",
                "svc_idle_re",
                "frame_service_less_present_re",
            ],
        ),
        ("ui", &["frame_ui_re"]),
        ("worst", &["frame_worst_re", "frame_worst_none_re"]),
    ];
    let mut families: Vec<&str> = APP
        .match_indices("\"frame ")
        .map(|(at, _)| {
            let rest = &APP[at + 7..];
            let end = rest
                .find(|c: char| !c.is_ascii_lowercase())
                .unwrap_or(rest.len());
            &rest[..end]
        })
        .filter(|f| !f.is_empty())
        .collect();
    families.sort_unstable();
    families.dedup();
    for family in &families {
        let probes = carried_by
            .iter()
            .find(|(f, _)| f == family)
            .map(|(_, p)| *p)
            .unwrap_or_else(|| {
                panic!(
                    "the app writes a `frame {family}` line and this table does not say \
                     which rig regex reads it — claim it here, and add the regex to \
                     drive.py if there is none; an unclaimed family is invisible to \
                     every leg"
                )
            });
        for probe in probes {
            assert!(
                DRIVE_PY.contains(&format!("var {probe} = /")),
                "`frame {family}` is claimed by `{probe}` but drive.py has no such regex",
            );
        }
    }
    assert_eq!(
        families.len(),
        carried_by.len(),
        "the table names families the app no longer writes: {families:?}",
    );
}

/// The `frame pump (…)` sentence, pinned as a literal.
///
/// Same formatter as `frame segment (…)` and deliberately a **different
/// prefix**: the eight are cuts of the `pump` segment, and a reader who could
/// mistake one for a ninth segment would add it to the very span it
/// decomposes.
#[test]
fn the_frame_pump_line_reads_exactly_as_pinned() {
    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let mut slots = [0u32; 42];
    slots[3] = 3;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::named_hist_line("frame pump", "apply", &h),
        format!(
            "frame pump (apply): n=3, sum=300 us, p50=106 us, \
             p90=106 us, p99=106 us, hist={expected_hist}"
        ),
    );
}

/// All eight pump cuts are emitted, under their own names, every tick — and
/// each carries its own histogram.
///
/// The `n` conjunct is what stops a mis-wired call from reading green, and it
/// matters here for `post`'s reason exactly: most of the eight are near-zero
/// on a healthy leg, so a `settle` line carrying `apply`'s histogram would
/// look like the reading everyone expects.
#[test]
fn every_pump_phase_is_reported_under_its_own_name() {
    let mut phases = crate::frame_ledger::PumpHists::default();
    // A different sample count per cut, so a swapped pair cannot pass.
    for (slot, hist) in [
        &mut phases.begin,
        &mut phases.restore,
        &mut phases.promote,
        &mut phases.raster,
        &mut phases.apply,
        &mut phases.advance,
        &mut phases.dispatch,
        &mut phases.settle,
    ]
    .into_iter()
    .enumerate()
    {
        for _ in 0..=slot {
            hist.record(1_000);
        }
    }
    let lines = super::frame_pump_lines(&phases);
    let names = [
        "begin", "restore", "promote", "raster", "apply", "advance", "dispatch", "settle",
    ];
    assert_eq!(lines.len(), names.len());
    for (slot, (line, name)) in lines.iter().zip(names).enumerate() {
        assert!(
            line.starts_with(&format!("frame pump ({name}): n={}, ", slot + 1)),
            "pump cut {slot} reported as {line:?}, which is not {name}'s line \
             carrying {name}'s histogram",
        );
    }
}

/// **The pump cuts do not collide with the segment lines the rig already
/// reads**, and `frame pump (dispatch)` does not collide with
/// `frame post (dispatch)`.
///
/// The second conjunct is the sharp one: two different spans under two
/// different parents share the cut name `dispatch`, and the only thing
/// keeping them apart in the rig's family map is the prefix. A shared prefix
/// would file the `Dispatch` pump walk and `dispatch_overlay_renders` under
/// one key and let a reader sum them.
#[test]
fn the_pump_cuts_are_not_readable_as_frame_segments_or_post_cuts() {
    let phases = crate::frame_ledger::PumpHists::default();
    let segments = crate::frame_ledger::SegmentHists::default();
    let pump_lines = super::frame_pump_lines(&phases);
    let segment_lines = super::frame_segment_lines(&segments);

    for line in &pump_lines {
        assert!(
            !line.starts_with("frame segment ("),
            "a pump cut is written as a frame segment ({line:?}); the rig \
             would file it beside the six that telescope to service",
        );
        assert!(
            !line.starts_with("frame post ("),
            "a pump cut is written as a post cut ({line:?}); `dispatch` is a \
             name both splits use and they are different spans",
        );
    }
    assert!(
        pump_lines
            .iter()
            .any(|line| line.starts_with("frame pump (dispatch): ")),
        "the pump walk's own dispatch cut is no longer written",
    );
    assert!(
        super::frame_post_lines(&crate::frame_ledger::PostHists::default())
            .iter()
            .any(|line| line.starts_with("frame post (dispatch): ")),
        "the tail's dispatch cut is no longer written, so the collision this \
         test guards can no longer be checked",
    );
    assert!(
        segment_lines
            .iter()
            .any(|line| line.starts_with("frame segment (pump): ")),
        "the segment line these cuts decompose is no longer written, so \
         nothing carries the total they sum to",
    );
}

/// The rig's `frame pump` probe reads what the app writes, character for
/// character — the same both-ends pin the post, segment, prepare and tile
/// lines carry.
#[test]
fn the_rig_reads_the_pump_lines_the_app_actually_writes() {
    let mut h = Hist::new();
    h.record(100);
    h.record(4_000);
    let hist = counts_string(&h);
    assert_eq!(
        super::named_hist_line("frame pump", "apply", &h),
        rendered(
            &pattern("frame_pump_re"),
            &["apply", "2", "4100", "106", "4757", "4757", &hist],
        ),
        "the `frame pump (…)` line and the rig's probe have drifted",
    );
}

/// The `frame post (…)` sentence, pinned as a literal.
///
/// Same formatter as `frame segment (…)` and deliberately a **different
/// prefix**, for `frame prepare`'s reason exactly: the six are cuts of the
/// `post` segment, and a reader who could mistake one for a seventh segment
/// would add it to the very span it decomposes.
#[test]
fn the_frame_post_line_reads_exactly_as_pinned() {
    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let mut slots = [0u32; 42];
    slots[3] = 3;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::named_hist_line("frame post", "dispatch", &h),
        format!(
            "frame post (dispatch): n=3, sum=300 us, p50=106 us, \
             p90=106 us, p99=106 us, hist={expected_hist}"
        ),
    );
}

/// All six post cuts are emitted, under their own names, every tick — and
/// each carries its own histogram.
///
/// The `n` conjunct is what stops a mis-wired call from reading green, and it
/// matters more here than on the other two splits: six of the seven cuts read
/// under the histogram floor on a real leg, so a `wake` line carrying
/// `handle`'s histogram would look exactly like the reading everyone expects.
#[test]
fn every_post_phase_is_reported_under_its_own_name() {
    let mut phases = crate::frame_ledger::PostHists::default();
    // A different sample count per cut, so a swapped pair cannot pass.
    for (slot, hist) in [
        &mut phases.handle,
        &mut phases.dispatch,
        &mut phases.back,
        &mut phases.wake,
        &mut phases.poll,
        &mut phases.repaint,
        &mut phases.close,
    ]
    .into_iter()
    .enumerate()
    {
        for _ in 0..=slot {
            hist.record(1_000);
        }
    }
    let lines = super::frame_post_lines(&phases);
    let names = [
        "handle", "dispatch", "back", "wake", "poll", "repaint", "close",
    ];
    assert_eq!(lines.len(), names.len());
    for (slot, (line, name)) in lines.iter().zip(names).enumerate() {
        assert!(
            line.starts_with(&format!("frame post ({name}): n={}, ", slot + 1)),
            "post cut {slot} reported as {line:?}, which is not {name}'s line \
             carrying {name}'s histogram",
        );
    }
}

/// **The post cuts do not collide with the segment lines the rig already
/// reads.** Both use `named_hist_line` and both name a `post`; the rig keys
/// its families on the prefix, so a shared one would file six cuts of a span
/// alongside the span itself under one name and let a reader sum them.
#[test]
fn the_post_cuts_are_not_readable_as_frame_segments() {
    let phases = crate::frame_ledger::PostHists::default();
    let segments = crate::frame_ledger::SegmentHists::default();
    let post_lines = super::frame_post_lines(&phases);
    let segment_lines = super::frame_segment_lines(&segments);

    for line in &post_lines {
        assert!(
            !line.starts_with("frame segment ("),
            "a post cut is written as a frame segment ({line:?}); the rig \
             would file it beside the six that telescope to service",
        );
    }
    assert!(
        segment_lines
            .iter()
            .any(|line| line.starts_with("frame segment (post): ")),
        "the segment line these cuts decompose is no longer written, so \
         nothing carries the total they sum to",
    );
}

/// The rig's `frame post` probe reads what the app writes, character for
/// character — the same both-ends pin the segment, prepare and tile lines
/// carry.
///
/// **This is the non-vacuity floor under the whole split on the web arm.**
/// The rig cannot save a raw console log (the ring holds 1200 entries and
/// only the last 60 reach the artefact), so a `frame post` line the probe
/// fails to match is not a missing family in the JSON — it is an absent one,
/// indistinguishable from a cut that never fired.
#[test]
fn the_rig_reads_the_post_lines_the_app_actually_writes() {
    let mut h = Hist::new();
    h.record(100);
    h.record(4_000);
    let hist = counts_string(&h);
    assert_eq!(
        super::named_hist_line("frame post", "dispatch", &h),
        rendered(
            &pattern("frame_post_re"),
            &["dispatch", "2", "4100", "106", "4757", "4757", &hist],
        ),
        "the `frame post (…)` line and the rig's probe have drifted",
    );
}

/// The `frame ui (…)` sentence, pinned as a literal.
///
/// Same formatter as `frame segment (…)` and deliberately a **different
/// prefix**: the six are cuts of the `ui` segment, and a reader who could
/// mistake one for a seventh segment would add it to the very span it
/// decomposes. The `sum=` is the load-bearing field for the same reason it is
/// on the segment line — 3 × 100 = 300, where every percentile of that
/// histogram answers the bin's 106 us upper edge.
#[test]
fn the_frame_ui_line_reads_exactly_as_pinned() {
    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let mut slots = [0u32; 42];
    slots[3] = 3;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::named_hist_line("frame ui", "shell", &h),
        format!(
            "frame ui (shell): n=3, sum=300 us, p50=106 us, p90=106 us, \
             p99=106 us, hist={expected_hist}"
        ),
    );
}

/// All six `ui` cuts are emitted, under their own names, every tick — and
/// each carries its own histogram.
///
/// The `n` conjunct is what stops a mis-wired call from reading green:
/// `shell` carrying `panes`' histogram would keep every name right and every
/// figure wrong, which is the failure a split is most likely to have — and
/// the two are neighbours across the seam this whole instrument exists to
/// resolve, so a swap there would answer the question backwards.
#[test]
fn every_ui_phase_is_reported_under_its_own_name() {
    let mut phases = crate::frame_ledger::UiHists::default();
    // A different sample count per cut, so a swapped pair cannot pass.
    for (slot, hist) in [
        &mut phases.poll,
        &mut phases.layout,
        &mut phases.topbar,
        &mut phases.statusbar,
        &mut phases.stack,
        &mut phases.dialog,
        &mut phases.panes,
        &mut phases.apply,
        &mut phases.chrome,
    ]
    .into_iter()
    .enumerate()
    {
        for _ in 0..=slot {
            hist.record(1_000);
        }
    }
    let lines = super::frame_ui_lines(&phases);
    let names = [
        "poll",
        "layout",
        "topbar",
        "statusbar",
        "stack",
        "dialog",
        "panes",
        "apply",
        "chrome",
    ];
    assert_eq!(lines.len(), names.len());
    for (slot, (line, name)) in lines.iter().zip(names).enumerate() {
        assert!(
            line.starts_with(&format!("frame ui ({name}): n={}, ", slot + 1)),
            "ui cut {slot} reported as {line:?}, which is not {name}'s line \
             carrying {name}'s histogram",
        );
    }
}

/// **The `ui` cuts do not collide with the segment lines the rig already
/// reads.**
///
/// Both spellings go through `named_hist_line` and both name a `ui`, so
/// `frame segment (ui)` and the six `frame ui (…)` lines share a substring.
/// A rig regex anchored loosely enough to match both would read a cut as the
/// segment — and since the cuts sum to the segment, the mistake is *plausible
/// arithmetic* rather than an obvious null, which is how it would survive
/// review. Held here: the segment line does not start with the cut prefix,
/// and no cut line starts with the segment prefix.
#[test]
fn the_ui_cut_lines_are_not_mistakable_for_the_ui_segment_line() {
    let mut h = Hist::new();
    h.record(1_000);
    let segment_line = super::named_hist_line("frame segment", "ui", &h);
    assert!(
        !segment_line.starts_with("frame ui ("),
        "the ui segment line {segment_line:?} reads as one of its own cuts",
    );
    for line in super::frame_ui_lines(&crate::frame_ledger::UiHists::default()) {
        assert!(
            line.starts_with("frame ui ("),
            "a ui cut is not under the cut prefix: {line:?}",
        );
        assert!(
            !line.starts_with("frame segment"),
            "the ui cut {line:?} reads as a seventh frame segment, which a \
             reader would add to the span it decomposes",
        );
    }
}
/// The `frame stack (…)` sentence, pinned as a literal.
///
/// Same formatter as `frame ui (…)` and deliberately a **third prefix**: the
/// seven are cuts of `frame ui (stack)`, which is itself a cut of
/// `frame segment (ui)`. A reader who could mistake one for the other would
/// add the same microseconds to the very span they decompose, twice over. The
/// `sum=` is the load-bearing field for `frame ui`'s reason — 3 x 100 = 300,
/// where every percentile of that histogram answers the bin's 106 us upper
/// edge.
#[test]
fn the_frame_stack_line_reads_exactly_as_pinned() {
    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let mut slots = [0u32; 42];
    slots[3] = 3;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::named_hist_line("frame stack", "gate", &h),
        format!(
            "frame stack (gate): n=3, sum=300 us, p50=106 us, p90=106 us, \
             p99=106 us, hist={expected_hist}"
        ),
    );
}

/// All seven `stack` cuts are emitted, under their own names, every tick —
/// and each carries its own histogram.
///
/// The `n` conjunct is what stops a mis-wired call from reading green:
/// `render` carrying `inspector`'s histogram would keep every name right and
/// every figure wrong. Those two are the neighbours across the one seam this
/// split exists to resolve — is the stack tail the rows or the inspector —
/// so a swap there would answer the question backwards.
#[test]
fn every_stack_phase_is_reported_under_its_own_name() {
    let mut phases = crate::frame_ledger::StackHists::default();
    // A different sample count per cut, so a swapped pair cannot pass.
    for (slot, hist) in [
        &mut phases.snap,
        &mut phases.gate,
        &mut phases.hydrate,
        &mut phases.statuses,
        &mut phases.render,
        &mut phases.inspector,
        &mut phases.settle,
    ]
    .into_iter()
    .enumerate()
    {
        for _ in 0..=slot {
            hist.record(1_000);
        }
    }
    let lines = super::frame_stack_lines(&phases);
    let names = [
        "snap",
        "gate",
        "hydrate",
        "statuses",
        "render",
        "inspector",
        "settle",
    ];
    assert_eq!(lines.len(), names.len());
    for (slot, (line, name)) in lines.iter().zip(names).enumerate() {
        assert!(
            line.starts_with(&format!("frame stack ({name}): n={}, ", slot + 1)),
            "stack cut {slot} reported as {line:?}, which is not {name}'s line \
             carrying {name}'s histogram",
        );
    }
}

/// **The `stack` cuts do not collide with the two levels above them.**
///
/// Three spellings name a `stack`: `frame segment (ui)` contains it,
/// `frame ui (stack)` IS it, and `frame stack (…)` opens it up. A rig regex
/// anchored loosely enough to match two of them would read a cut as its own
/// parent — and because the seven sum to `frame ui (stack)`, which sums into
/// `frame segment (ui)`, the mistake is *plausible arithmetic* rather than an
/// obvious null. Held in every direction: no `frame stack` line reads as a
/// `frame ui` line or a segment line, and `frame ui (stack)` does not read as
/// one of its own cuts.
#[test]
fn the_stack_cut_lines_are_not_mistakable_for_their_parents() {
    let mut h = Hist::new();
    h.record(1_000);
    let parent_cut = super::named_hist_line("frame ui", "stack", &h);
    assert!(
        !parent_cut.starts_with("frame stack ("),
        "the `frame ui (stack)` line {parent_cut:?} reads as one of its own \
         cuts, so a reader would add the seven to the one they decompose",
    );
    let segment_line = super::named_hist_line("frame segment", "ui", &h);
    assert!(
        !segment_line.starts_with("frame stack ("),
        "the ui segment line {segment_line:?} reads as a stack cut",
    );
    for line in super::frame_stack_lines(&crate::frame_ledger::StackHists::default()) {
        assert!(
            line.starts_with("frame stack ("),
            "a stack cut is not under the cut prefix: {line:?}",
        );
        assert!(
            !line.starts_with("frame ui ("),
            "the stack cut {line:?} reads as a tenth `ui` cut, which a reader \
             would add to the `ui` nine it is already inside",
        );
        assert!(
            !line.starts_with("frame segment"),
            "the stack cut {line:?} reads as a seventh frame segment",
        );
    }
}

/// The rig's `frame stack` probe reads what the app writes.
///
/// A family the app writes and the rig has no regex for is ABSENT from the
/// artifact rather than empty — the failure that kept `frame ui (…)`, the
/// largest segment of an interact frame, invisible for weeks. This family is
/// the one below it and names the owner of its tail, so the same absence here
/// would leave a 16-20x spike undecomposed for the same reason.
#[test]
fn the_rig_reads_the_stack_lines_the_app_actually_writes() {
    let mut h = Hist::new();
    h.record(100);
    h.record(4_000);
    let hist = counts_string(&h);
    assert_eq!(
        super::named_hist_line("frame stack", "hydrate", &h),
        rendered(
            &pattern("frame_stack_re"),
            &["hydrate", "2", "4100", "106", "4757", "4757", &hist],
        ),
        "the `frame stack (…)` line and the rig's probe have drifted",
    );
    // **The probe's own anchor may not appear in the line it decomposes.**
    // `frame_stack_re` is anchored on the literal `frame stack (`, and the
    // parent cut's line spells `frame ui (stack):` — one substring away from
    // being scraped as one of its own seven, which would read as plausible
    // arithmetic rather than as a null.
    let parent = super::named_hist_line("frame ui", "stack", &h);
    assert!(
        !parent.contains("frame stack ("),
        "the rig's stack anchor appears in the `frame ui (stack)` line it \
         decomposes: {parent:?}",
    );
    assert!(
        pattern("frame_stack_re").starts_with("frame stack \\("),
        "the rig's stack probe is no longer anchored on the literal the check \
         above tests for, so that check has stopped covering the collision",
    );
}

/// The `frame panes (…)` sentence, pinned as a literal.
///
/// Same formatter as `frame ui (…)` and `frame stack (…)`, and deliberately a
/// **fourth prefix**: the eight are cuts of `frame ui (panes)`, which is
/// itself a cut of `frame segment (ui)`. The `sum=` is the load-bearing field
/// — 3 x 100 = 300, where every percentile of that histogram answers the
/// bin's 106 us upper edge, which is why no share may be read off a
/// percentile.
#[test]
fn the_frame_panes_line_reads_exactly_as_pinned() {
    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let mut slots = [0u32; 42];
    slots[3] = 3;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::named_hist_line("frame panes", "content", &h),
        format!(
            "frame panes (content): n=3, sum=300 us, p50=106 us, p90=106 us, \
             p99=106 us, hist={expected_hist}"
        ),
    );
}

/// All eight `panes` cuts are emitted, under their own names, every tick —
/// and each carries its own histogram.
///
/// The `n` conjunct is what stops a mis-wired call from reading green:
/// `content` carrying `widget`'s histogram would keep every name right and
/// every figure wrong, and those two are the pair this split exists to tell
/// apart — the layer walk against the map widget around it — so a swap there
/// would answer the question backwards.
#[test]
fn every_panes_phase_is_reported_under_its_own_name() {
    let mut phases = crate::frame_ledger::PanesHists::default();
    // A different sample count per cut, so a swapped pair cannot pass.
    for (slot, hist) in [
        &mut phases.setup,
        &mut phases.panel,
        &mut phases.resolve,
        &mut phases.widget,
        &mut phases.content,
        &mut phases.tools,
        &mut phases.credit,
        &mut phases.residual,
    ]
    .into_iter()
    .enumerate()
    {
        for _ in 0..=slot {
            hist.record(1_000);
        }
    }
    let lines = super::frame_panes_lines(&phases);
    let names = [
        "setup", "panel", "resolve", "widget", "content", "tools", "credit", "residual",
    ];
    assert_eq!(lines.len(), names.len());
    for (slot, (line, name)) in lines.iter().zip(names).enumerate() {
        assert!(
            line.starts_with(&format!("frame panes ({name}): n={}, ", slot + 1)),
            "panes cut {slot} reported as {line:?}, which is not {name}'s \
             line carrying {name}'s histogram",
        );
    }
}

/// **The `panes` cuts do not collide with the two levels above them.**
///
/// `the_stack_cut_lines_are_not_mistakable_for_their_parents`, one cut
/// across, and the collision is one word closer here: `panes` is a `ui` CUT
/// NAME as well as this family's prefix, so `frame ui (panes)` and
/// `frame panes (…)` share both words in the other order. A rig regex
/// anchored loosely enough to match two of them would read a cut as its own
/// parent — and because the eight sum to `frame ui (panes)`, which sums into
/// `frame segment (ui)`, the mistake is *plausible arithmetic* rather than an
/// obvious null.
#[test]
fn the_panes_cut_lines_are_not_mistakable_for_their_parents() {
    let mut h = Hist::new();
    h.record(1_000);
    let parent_cut = super::named_hist_line("frame ui", "panes", &h);
    assert!(
        !parent_cut.starts_with("frame panes ("),
        "the `frame ui (panes)` line {parent_cut:?} reads as one of its own \
         cuts, so a reader would add the eight to the one they decompose",
    );
    assert!(
        !parent_cut.contains("frame panes ("),
        "the rig's panes anchor appears inside the `frame ui (panes)` line it \
         decomposes: {parent_cut:?}",
    );
    let segment_line = super::named_hist_line("frame segment", "ui", &h);
    assert!(
        !segment_line.starts_with("frame panes ("),
        "the ui segment line {segment_line:?} reads as a panes cut",
    );
    // And the other neighbour one level down: `frame stack (…)` is the same
    // shape under a different parent cut, and the two are never added.
    for line in super::frame_stack_lines(&crate::frame_ledger::StackHists::default()) {
        assert!(
            !line.starts_with("frame panes ("),
            "the stack cut {line:?} reads as a panes cut, and the two \
             decompose DIFFERENT cuts of `ui`",
        );
    }
    for line in super::frame_panes_lines(&crate::frame_ledger::PanesHists::default()) {
        assert!(
            line.starts_with("frame panes ("),
            "a panes cut is not under the cut prefix: {line:?}",
        );
        assert!(
            !line.starts_with("frame ui ("),
            "the panes cut {line:?} reads as a tenth `ui` cut, which a reader \
             would add to the `ui` nine it is already inside",
        );
        assert!(
            !line.starts_with("frame segment"),
            "the panes cut {line:?} reads as a seventh frame segment",
        );
        assert!(
            !line.starts_with("frame stack ("),
            "the panes cut {line:?} reads as a `stack` cut, which decomposes \
             a different cut of the same segment",
        );
    }
}

/// The rig's `frame panes` probe reads what the app writes.
///
/// `the_rig_reads_the_stack_lines_the_app_actually_writes`, one cut across: a
/// family the app writes and the rig has no regex for is ABSENT from the
/// artifact rather than empty. This family decomposes the largest cut of the
/// largest segment, so the same absence here would leave 81.6 % of `ui`
/// undecomposed on the arm that measured it.
#[test]
fn the_rig_reads_the_panes_lines_the_app_actually_writes() {
    let mut h = Hist::new();
    h.record(100);
    h.record(4_000);
    let hist = counts_string(&h);
    assert_eq!(
        super::named_hist_line("frame panes", "widget", &h),
        rendered(
            &pattern("frame_panes_re"),
            &["widget", "2", "4100", "106", "4757", "4757", &hist],
        ),
        "the `frame panes (…)` line and the rig's probe have drifted",
    );
    assert!(
        pattern("frame_panes_re").starts_with("frame panes \\("),
        "the rig's panes probe is no longer anchored on the literal the \
         collision check tests for, so that check has stopped covering it",
    );
}

/// The `tile take (…)` sentence, pinned as a literal, and only for a family
/// that has samples.
///
/// The five families are never added to each other and none of them is added
/// to `overlay rasters`, `texture uploads`, `basemap tiles` or any frame
/// segment — see `squallar_egui::tile_source::take_ledger`, which states the
/// denominator in full. The unit here is ONE TAKE.
#[test]
fn the_tile_take_line_reads_exactly_as_pinned() {
    use squallar_egui::tile_source::take_ledger::{TakeKind, Totals};

    let mut families = [Hist::new(); 5];
    for _ in 0..3 {
        families[1].record(100);
    }
    let totals = Totals { families };
    assert_eq!(
        totals.family(TakeKind::Raster).total(),
        3,
        "the fixture did not populate the raster family, so the pin below \
         would be pinning a line about a different family",
    );

    let mut slots = [0u32; 42];
    slots[3] = 3;
    let expected_hist = slots.map(|c| c.to_string()).join(",");
    assert_eq!(
        super::tile_take_lines(&totals),
        vec![format!(
            "tile take (raster): n=3, sum=300 us, p50=106 us, p90=106 us, \
             p99=106 us, hist={expected_hist}"
        )],
        "a family with no takes was reported, or the one with takes was not",
    );
}

/// **An empty ledger says nothing, and a moved one says exactly what moved.**
///
/// The tile families differ from the frame segments deliberately: three of the
/// five are structurally empty on any one arm (`put` is native-only; `sniffed`
/// and `restyle` need a plain-HTTP source and a theme flip), so emitting them
/// at `n=0` forever would be console the reader steps over — and the ring the
/// rig scrapes holds 1200 entries and evicts. The frame segments, which are
/// never structurally absent, are emitted unconditionally instead.
#[test]
fn a_tile_family_with_no_takes_is_not_reported() {
    use squallar_egui::tile_source::take_ledger::{FAMILIES, TakeKind, Totals};

    let empty = Totals {
        families: [Hist::new(); 5],
    };
    assert!(
        super::tile_take_lines(&empty).is_empty(),
        "an app that has taken no tiles wrote a line about every family",
    );

    // One take in each family: five lines, in the declared family order, each
    // naming its own family exactly once.
    let mut families = [Hist::new(); 5];
    for hist in &mut families {
        hist.record(1_000);
    }
    let lines = super::tile_take_lines(&Totals { families });
    assert_eq!(lines.len(), FAMILIES.len());
    for (line, kind) in lines.iter().zip(FAMILIES) {
        assert!(
            line.starts_with(&format!("tile take ({}): n=1, ", kind.label())),
            "the take families are not reported in their declared order: \
             {line:?} is not {}'s line",
            kind.label(),
        );
    }
    assert_ne!(
        lines.len(),
        super::tile_take_lines(&empty).len(),
        "the populated and empty ledgers produced the same number of lines, \
         so this test could not have failed",
    );
    assert_eq!(TakeKind::Vector.label(), "vector");
}

/// Both new sentences, held against the rig's own patterns rather than
/// against a second copy of the literal — the seam that actually breaks when
/// one end moves.
#[test]
fn the_rig_reads_the_two_windowable_families_the_app_actually_writes() {
    use squallar_egui::tile_source::take_ledger::Totals;

    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let hist = counts_string(&h);

    let segments = crate::frame_ledger::SegmentHists {
        pump: h,
        ..Default::default()
    };
    assert_eq!(
        super::frame_segment_lines(&segments)[1],
        rendered(
            &pattern("frame_segment_re"),
            &["pump", "3", "300", "106", "106", "106", &hist],
        ),
        "the `frame segment (…):` line and the rig's probe have drifted",
    );

    let mut families = [Hist::new(); 5];
    families[0] = h;
    assert_eq!(
        super::tile_take_lines(&Totals { families })[0],
        rendered(
            &pattern("tile_take_re"),
            &["vector", "3", "300", "106", "106", "106", &hist],
        ),
        "the `tile take (…):` line and the rig's probe have drifted",
    );
}

/// The floor under the seam test above, in the shape
/// `a_frame_line_that_drifted_by_one_space_is_not_accepted` already uses:
/// without it, a `pattern` returning the sentence itself would hold the
/// equality whatever the app wrote.
#[test]
fn a_windowable_line_that_drifted_by_one_space_is_not_accepted() {
    let hist = counts_string(&Hist::new());
    let good = rendered(
        &pattern("frame_segment_re"),
        &["pre", "0", "0", "none", "none", "none", &hist],
    );
    assert_eq!(
        super::named_hist_line("frame segment", "pre", &Hist::new()),
        good,
    );
    let drifted = good.replacen(" us", "  us", 1);
    assert_ne!(drifted, good, "the perturbation perturbed nothing");
    assert_ne!(
        super::named_hist_line("frame segment", "pre", &Hist::new()),
        drifted,
        "a line with one extra space compared equal to the real one, so the \
         seam test above cannot fail",
    );
}

/// The `tile bodies:` sentence, pinned at both ends.
///
/// It is the line the two `tile phase` families cannot do without. Those are
/// take families, and a take family with no samples is not printed — so once
/// the pump offloads they fall towards `n = 0` and then go quiet, and a
/// reader cannot tell work that left the frame thread from a line nobody
/// collected. This is emitted unconditionally for that reason, so `0
/// offloaded, 0 decoded on the frame thread` is a reading.
///
/// Both halves of the rig went without it: `drive.py` scraped `tile phase`
/// alone, which is precisely the family that goes silent, so the measurement
/// an offload change needs was unparsed on the web and absent from every
/// native row.
#[test]
fn the_tile_bodies_line_reads_exactly_as_pinned() {
    use squallar_egui::tile_source::take_ledger::Disposition;

    assert_eq!(
        super::tile_disposition_line(&Disposition {
            offloaded: 41,
            inline: 7,
        }),
        "tile bodies: 41 offloaded, 7 decoded on the frame thread",
    );
    assert_eq!(
        super::tile_disposition_line(&Disposition {
            offloaded: 41,
            inline: 7,
        }),
        rendered(&pattern("tile_bodies_re"), &["41", "7"]),
        "the `tile bodies:` line and the rig's probe have drifted",
    );
    // The all-zero reading is a SENTENCE, not a silence: it is the whole
    // reason this line is emitted unconditionally where the phase families
    // are not.
    assert_eq!(
        super::tile_disposition_line(&Disposition {
            offloaded: 0,
            inline: 0,
        }),
        "tile bodies: 0 offloaded, 0 decoded on the frame thread",
    );
}

/// The sentence `native_row.py`'s own `LINE` fixture describes, given one
/// value per `%d` in order — the native half of "no reader may drift".
///
/// Read out of that file rather than restated here, on `pattern`'s terms: a
/// copy in this module would agree with itself forever while the rig read
/// something else.
fn native_row_line(values: &[&str]) -> String {
    const HEAD: &str = "    LINE = (";
    let at = NATIVE_ROW_PY.find(HEAD).unwrap_or_else(|| {
        panic!(
            "native_row.py no longer declares `{HEAD}…`; the native rig's \
             fixture for this line moved and this test can no longer read it"
        )
    });
    let rest = &NATIVE_ROW_PY[at + HEAD.len()..];
    let end = rest
        .find(')')
        .expect("native_row.py's LINE tuple is not closed");
    // Python's implicit concatenation of adjacent string literals: the
    // template is every quoted run inside the parentheses, in order.
    let template: String = rest[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .collect::<Vec<_>>()
        .concat();
    let template = template
        .strip_prefix("[..] INFO ")
        .expect("native_row.py's fixture no longer carries a log prefix this test can strip");
    assert_eq!(
        template.matches("%d").count(),
        values.len(),
        "native_row.py's fixture has {} fields and {} values were offered: {template}",
        template.matches("%d").count(),
        values.len(),
    );
    let mut out = String::new();
    let mut rest = template;
    for value in values {
        let at = rest.find("%d").expect("counted above");
        out.push_str(&rest[..at]);
        out.push_str(value);
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    out
}

/// The `frame need:` sentence, pinned at both ends — **the one line here that
/// is not about what a frame cost**.
///
/// Its three groups are three denominators and are never added across, which
/// is why they are pinned as one sentence rather than three: a reader who saw
/// `caused` and `charged` on separate lines would have no reason not to sum
/// them, and they answer different questions over different populations.
///
/// Emitted unconditionally, on `tile_disposition_line`'s terms: `0 drawn` is a
/// reading — no frame has presented — and not a silence a reader cannot tell
/// from a line nobody collected.
#[test]
fn the_frame_need_line_reads_exactly_as_pinned() {
    use crate::frame_need::Reading;
    use squallar_egui::frame_need::NeedCause;

    let r = Reading {
        drawn: 12_345,
        needed: 12_000,
        causes: [900, 11_000, 400, 3, 120],
        charged: [5, 10, 2, 0, 1, 0, 0, 295, 30, 2],
    };
    // The verdict is arithmetic on the two counts, never a third counter that
    // could drift from them.
    assert_eq!(r.unnecessary(), 345);
    assert!(r.charges_balance());
    assert!(r.causes_cover_the_needed());

    let expected = "frame need: 12345 drawn, 12000 needed, 345 unnecessary; \
                    caused input=900 arrival=11000 animation=400 surface=3 \
                    clock=120; charged upload=5 loop=10 hold=2 restore=0 \
                    chunk=1 drops=0 gesture=0 egui=295 timed=30 external=2";
    assert_eq!(super::frame_need_line(&r), expected);
    const FIELDS: [&str; 18] = [
        "12345", "12000", "345", "900", "11000", "400", "3", "120", "5", "10", "2", "0", "1", "0",
        "0", "295", "30", "2",
    ];
    assert_eq!(
        super::frame_need_line(&r),
        rendered(&pattern("frame_need_re"), &FIELDS),
        "the `frame need:` line and the browser rig's probe have drifted",
    );
    // **The second reader, and the reason this assertion exists.** Holding the
    // line against `drive.py` alone let `native_row.py` fall a field behind
    // and stay silent about it; the same fields against both readers is what
    // makes "no reader may drift" true rather than true of one of them.
    assert_eq!(
        super::frame_need_line(&r),
        native_row_line(&FIELDS),
        "the `frame need:` line and the NATIVE rig's fixture have drifted",
    );
    // The fixture matching is not enough on its own: `native_row.py` also
    // names the claims, in order, to key the charge map it prints, and a
    // template can still be right while those names are reordered or one is
    // missing. Read from the file, checked against the enum.
    let names_after = |head: &str, what: &str| -> Vec<String> {
        let at = NATIVE_ROW_PY
            .find(head)
            .unwrap_or_else(|| panic!("native_row.py no longer keys its {what} map by name"));
        let rest = &NATIVE_ROW_PY[at + head.len()..];
        let end = rest.find("), fnd[").unwrap_or_else(|| {
            panic!("native_row.py's {what} key tuple is not closed as this test expects")
        });
        rest[..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let keys = names_after("\"charged\": dict(zip((", "charge");
    // The same hold on the CAUSE names, and it is not symmetry for its own
    // sake: the charge names were held and the cause names were not, so a
    // cause added to the enum and forgotten in this tuple would have gone on
    // labelling one cause's count with the next cause's name — silently, since
    // `zip` stops at the shorter side and drops the tail without a word.
    assert_eq!(
        names_after("\"caused\": dict(zip((", "cause"),
        NeedCause::ALL
            .iter()
            .map(|c| c.name().to_owned())
            .collect::<Vec<_>>(),
        "native_row.py names the causes differently from `NeedCause::ALL`, so \
         its rows label one cause's count with another cause's name",
    );
    let claims: Vec<String> = crate::frame_need::WakeClaim::ALL
        .iter()
        .map(|c| c.name().to_owned())
        .collect();
    assert_eq!(
        keys, claims,
        "native_row.py names the charges differently from `WakeClaim::ALL`, \
         so its rows label one claim's count with another claim's name",
    );
    // The charge slice starts past every cause, so a cause added without
    // moving this slice hands the charge map a cause count and drops a claim.
    let charges_at = NeedCause::COUNT + 3;
    assert!(
        NATIVE_ROW_PY.contains(&format!(
            "), fnd[{charges_at}:{}])),",
            charges_at + claims.len()
        )),
        "native_row.py slices a different number of charges than there are \
         claims, so the charge map it builds is short or over-long",
    );
    // The all-zero reading is a SENTENCE, for the reason `tile bodies:`' is.
    assert_eq!(
        super::frame_need_line(&Reading::default()),
        "frame need: 0 drawn, 0 needed, 0 unnecessary; caused input=0 \
         arrival=0 animation=0 surface=0 clock=0; charged upload=0 loop=0 \
         hold=0 restore=0 chunk=0 drops=0 gesture=0 egui=0 timed=0 \
         external=0",
    );
}

/// The `tile phase (…)` sentence, pinned at both ends, and the identity that
/// keeps it a decomposition rather than a sixth take family.
///
/// The two phases have **different resumability** — `parse` is per source
/// layer (at most sixteen), `style` is per feature (thousands) — which is the
/// only reason the split earns its two clock reads, and the reason its
/// denominator has to stay separate from the take families'.
#[test]
fn the_tile_phase_line_reads_exactly_as_pinned() {
    use squallar_egui::tile_source::take_ledger::{PHASES, PhaseTotals, VectorPhase};

    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    let hist = counts_string(&h);

    // An empty ledger says nothing, on `tile_take_lines`' terms.
    assert!(
        super::tile_phase_lines(&PhaseTotals {
            phases: [Hist::new(); 2],
        })
        .is_empty(),
        "an app that has decoded no vector body wrote a phase line anyway",
    );

    let only_style = PhaseTotals {
        phases: [Hist::new(), h],
    };
    assert_eq!(
        super::tile_phase_lines(&only_style),
        vec![format!(
            "tile phase (style): n=3, sum=300 us, p50=106 us, p90=106 us, \
             p99=106 us, hist={hist}"
        )],
        "a restyle records `style` and no `parse`, and the line has to be able \
         to say exactly that",
    );
    assert_eq!(
        super::tile_phase_lines(&only_style)[0],
        rendered(
            &pattern("tile_phase_re"),
            &["style", "3", "300", "106", "106", "106", &hist],
        ),
        "the `tile phase (…):` line and the rig's probe have drifted",
    );

    // Both phases: two lines, in the declared order, each naming its own.
    let both = PhaseTotals { phases: [h, h] };
    let lines = super::tile_phase_lines(&both);
    assert_eq!(lines.len(), PHASES.len());
    for (line, phase) in lines.iter().zip(PHASES) {
        assert!(
            line.starts_with(&format!("tile phase ({}): n=3, ", phase.label())),
            "the phases are not reported in their declared order: {line:?} is \
             not {}'s line",
            phase.label(),
        );
    }
    assert_eq!(
        (VectorPhase::Parse.label(), VectorPhase::Style.label()),
        ("parse", "style"),
    );
    // The decomposition is NOT a take family: a phase reading carries no take
    // count, and nothing here can be added to `tile take (vector)`.
    assert_ne!(
        VectorPhase::Parse.label(),
        squallar_egui::tile_source::take_ledger::TakeKind::Vector.label(),
        "a phase and a take family share a word, so a reader could add them",
    );
}

/// The `frame worst:` sentence, pinned as a literal.
///
/// **Its denominator is not any other frame line's**: one frame, whichever
/// family it was in, of the last telemetry period. The six figures are that
/// frame's own microseconds and they sum to its service — they are not
/// percentiles and they are never added to `frame segments`, which already
/// contains this frame if it was an interact one.
#[test]
fn the_worst_frame_line_reads_exactly_as_pinned() {
    let worst = crate::frame_ledger::WorstFrame {
        service: 6_728,
        segments: [61, 54, 4_402, 1_580, 611, 20],
        ui_cuts: [12, 310, 903, 41, 2_800, 6, 288, 2, 40],
        stack_cuts: [9, 41, 118, 402, 1_900, 300, 30],
        pre_cuts: [2, 18, 11, 21, 1, 4, 4],
        interact: false,
    };
    assert_eq!(
        worst.segments.iter().sum::<u32>(),
        worst.service,
        "the fixture's segments do not telescope to its service, so the pin \
         below would pin a line describing no frame that could exist",
    );
    assert_eq!(
        worst.ui_cuts.iter().sum::<u32>(),
        worst.segments[2],
        "the fixture's nine ui cuts do not telescope to its ui, so the pin \
         below would pin a line whose ui_* columns decompose no frame",
    );
    assert_eq!(
        worst.pre_cuts.iter().sum::<u32>(),
        worst.segments[0],
        "the fixture's seven pre cuts do not telescope to its pre, so the pin \
         below would pin a line whose pre_* columns decompose no frame",
    );
    assert_eq!(
        worst.stack_cuts.iter().sum::<u32>(),
        worst.ui_cuts[4],
        "the fixture's seven stack cuts do not telescope to its ui_stack, so \
         the pin below would pin a line whose stack_* columns decompose no \
         frame",
    );
    let boot = crate::frame_ledger::WorstFrame {
        service: 9_513,
        segments: [1, 2, 3, 9_500, 4, 3],
        ui_cuts: [0, 1, 0, 0, 1, 0, 1, 0, 0],
        stack_cuts: [0, 0, 0, 0, 1, 0, 0],
        pre_cuts: [0, 1, 0, 0, 0, 0, 0],
        interact: false,
    };
    assert_eq!(boot.ui_cuts.iter().sum::<u32>(), boot.segments[2]);
    assert_eq!(boot.pre_cuts.iter().sum::<u32>(), boot.segments[0]);
    assert_eq!(boot.stack_cuts.iter().sum::<u32>(), boot.ui_cuts[4]);
    assert_eq!(
        super::frame_worst_line(Some(worst), Some(boot)),
        "frame worst: service=6728 us, family=idle, since_boot=9513 us, \
         pre=61 us, pump=54 us, ui=4402 us, prepare=1580 us, finish=611 us, \
         post=20 us, ui_poll=12 us, ui_layout=310 us, ui_topbar=903 us, \
         ui_statusbar=41 us, ui_stack=2800 us, ui_dialog=6 us, ui_panes=288 us, \
         ui_apply=2 us, ui_chrome=40 us, stack_snap=9 us, stack_gate=41 us, \
         stack_hydrate=118 us, stack_statuses=402 us, stack_render=1900 us, \
         stack_inspector=300 us, stack_settle=30 us, pre_platform=2 us, \
         pre_ingest=18 us, \
         pre_evict=11 us, pre_drops=21 us, pre_autosave=1 us, pre_gate=4 us, \
         pre_ensure=4 us, boot: idle, pre=1 us, pump=2 us, \
         ui=3 us, prepare=9500 us, finish=4 us, post=3 us, ui_poll=0 us, \
         ui_layout=1 us, ui_topbar=0 us, ui_statusbar=0 us, ui_stack=1 us, \
         ui_dialog=0 us, ui_panes=1 us, ui_apply=0 us, ui_chrome=0 us, \
         stack_snap=0 us, stack_gate=0 us, stack_hydrate=0 us, \
         stack_statuses=0 us, stack_render=1 us, stack_inspector=0 us, \
         stack_settle=0 us, \
         pre_platform=0 us, pre_ingest=1 us, pre_evict=0 us, pre_drops=0 us, \
         pre_autosave=0 us, pre_gate=0 us, pre_ensure=0 us",
    );
}

/// The family column is reported, never assumed. A worst frame that DID carry
/// the click says so, and the same formatter says it.
#[test]
fn the_worst_frame_line_names_the_interact_family_too() {
    let worst = crate::frame_ledger::WorstFrame {
        service: 600,
        segments: [100; 6],
        ui_cuts: [1, 2, 3, 4, 80, 5, 3, 1, 1],
        stack_cuts: [2, 4, 6, 8, 50, 7, 3],
        pre_cuts: [10, 30, 20, 25, 5, 4, 6],
        interact: true,
    };
    assert!(
        super::frame_worst_line(Some(worst), None).contains("family=interact"),
        "an interact worst frame is not reported as one, so the column \
         cannot distinguish the two cases it exists to distinguish",
    );
}

/// A period in which nothing presented prints the ABSENCE, not a frame of
/// zeros. "No frame presented" and "a frame that cost nothing" are different
/// claims and a reader cannot be asked to tell them apart from six zeros.
#[test]
fn the_worst_frame_line_says_absence_rather_than_a_zero_frame() {
    let boot = crate::frame_ledger::WorstFrame {
        service: 9_513,
        segments: [1, 2, 3, 9_500, 4, 3],
        ui_cuts: [0, 1, 0, 0, 1, 0, 1, 0, 0],
        stack_cuts: [0, 0, 0, 0, 1, 0, 0],
        pre_cuts: [0, 1, 0, 0, 0, 0, 0],
        interact: false,
    };
    let line = super::frame_worst_line(None, Some(boot));
    assert!(
        !line.contains("service=0 us"),
        "an empty period reads as a frame that cost nothing: {line:?}",
    );
    assert_eq!(
        line,
        "frame worst: no frame presented this period, since_boot=9513 us, boot: idle, \
         pre=1 us, pump=2 us, ui=3 us, prepare=9500 us, finish=4 us, post=3 us, \
         ui_poll=0 us, ui_layout=1 us, ui_topbar=0 us, ui_statusbar=0 us, \
         ui_stack=1 us, ui_dialog=0 us, ui_panes=1 us, ui_apply=0 us, ui_chrome=0 us, \
         stack_snap=0 us, stack_gate=0 us, stack_hydrate=0 us, stack_statuses=0 us, \
         stack_render=1 us, stack_inspector=0 us, stack_settle=0 us, \
         pre_platform=0 us, pre_ingest=1 us, pre_evict=0 us, pre_drops=0 us, \
         pre_autosave=0 us, pre_gate=0 us, pre_ensure=0 us",
        "an empty period must still carry the session maximum, or a console \
         ring that dropped the bad tick reads as a run with no bad frame",
    );
}

/// **`frame worst` does not collide with the segment lines the rig reads.**
///
/// It carries the same six names as `frame segments (interact, p99 us): pre=,
/// pump=, ...` and the same `pre=NNN us` shape, so a regex anchored on the
/// figures rather than the prefix would scrape one frame's microseconds into
/// a percentile column — and because a single frame's segments are plausible
/// percentile values, the mistake would read as data rather than as a null.
/// Held here in both directions.
#[test]
fn the_worst_frame_line_is_not_mistakable_for_a_segment_line() {
    let worst = crate::frame_ledger::WorstFrame {
        service: 600,
        segments: [100; 6],
        ui_cuts: [1, 2, 3, 4, 80, 5, 3, 1, 1],
        stack_cuts: [2, 4, 6, 8, 50, 7, 3],
        pre_cuts: [10, 30, 20, 25, 5, 4, 6],
        interact: true,
    };
    let worst_line = super::frame_worst_line(Some(worst), None);
    assert!(
        worst_line.starts_with("frame worst: "),
        "the worst-frame line is not under its own prefix: {worst_line:?}",
    );
    assert!(
        !worst_line.starts_with("frame segment"),
        "the worst-frame line reads as a segment line, and a reader would \
         add one frame to a distribution that already contains it: \
         {worst_line:?}",
    );
    let mut h = Hist::new();
    h.record(1_000);
    for line in super::frame_segment_lines(&crate::frame_ledger::SegmentHists::default())
        .into_iter()
        .chain([super::frame_segments_line(
            &crate::frame_ledger::SegmentHists::default(),
            &h,
        )])
    {
        assert!(
            !line.starts_with("frame worst"),
            "a segment line reads as the worst-frame line: {line:?}",
        );
    }
}

/// **Every dispatch cut is reported under its own name**, on
/// [`every_post_phase_is_reported_under_its_own_name`]'s terms exactly: seven
/// figures written in one order and read in another is a silent
/// misattribution, and this split exists to answer *which* cut costs, so a
/// swapped pair would be the whole finding, inverted.
#[test]
fn every_dispatch_cut_is_reported_under_its_own_name() {
    let mut cuts = crate::frame_ledger::DispatchHists::default();
    // A different sample count per cut, so a swapped pair cannot pass.
    for (slot, hist) in [
        &mut cuts.dedupe,
        &mut cuts.marks,
        &mut cuts.hydrate,
        &mut cuts.prepare,
        &mut cuts.hitmap,
        &mut cuts.offload,
        &mut cuts.residual,
    ]
    .into_iter()
    .enumerate()
    {
        for _ in 0..=slot {
            hist.record(1_000);
        }
    }
    let lines = super::frame_dispatch_lines(&cuts);
    let names = [
        "dedupe", "marks", "hydrate", "prepare", "hitmap", "offload", "residual",
    ];
    assert_eq!(lines.len(), names.len());
    for (slot, (line, name)) in lines.iter().zip(names).enumerate() {
        assert!(
            line.starts_with(&format!("frame dispatch ({name}): n={}, ", slot + 1)),
            "dispatch cut {slot} reported as {line:?}, which is not {name}'s \
             line carrying {name}'s histogram",
        );
    }
}

/// **The dispatch cuts collide with neither line the rig already reads.**
/// These seven are one level below `frame post (dispatch)`, which is itself
/// one level below `frame segment (post)`. All three spellings come out of
/// `named_hist_line` and all three name a dispatch or a post; the rig keys
/// its families on the prefix, so a shared prefix anywhere in that chain
/// would let a reader add a span to its own decomposition.
#[test]
fn the_dispatch_cuts_are_readable_as_neither_post_cuts_nor_segments() {
    let cuts = crate::frame_ledger::DispatchHists::default();
    let mut phases = crate::frame_ledger::PostHists::default();
    phases.dispatch.record(1_000);

    for line in &super::frame_dispatch_lines(&cuts) {
        assert!(
            !line.starts_with("frame post (") && !line.starts_with("frame segment ("),
            "a dispatch cut is written as {line:?}; the rig would file it \
             beside the span it decomposes",
        );
    }
    assert!(
        super::frame_post_lines(&phases)
            .iter()
            .any(|line| line.starts_with("frame post (dispatch): ")),
        "the post cut these seven decompose is no longer written, so nothing \
         carries the total they sum to",
    );
}

/// **The fires-counter row reads in both directions.**
///
/// The failure this guards is not "the number is wrong", it is "the number is
/// missing and nothing says so". A row that is omitted whenever the saving is
/// zero cannot be told apart from a build that has no counter at all, and that
/// is precisely how a ~94 MiB cut on this campaign shipped delivering exactly
/// nothing for a day: its precondition never held, its counter read zero, and a
/// silent zero looked identical to a mechanism nobody had wired yet.
///
/// So the row is gated on the **denominator** — evictions actually performed —
/// and never on the saving. No evictions at all is a genuinely empty
/// population and prints nothing; evictions that spared nothing print a real
/// `0`, which is the reading that says the rank ran and did not fire.
#[test]
fn the_demand_ranked_row_is_absent_without_evictions_and_says_zero_with_them() {
    assert_eq!(
        super::demand_ranked_eviction_line((0, 0), (0, 0), (0, 0)),
        None,
        "no eviction anywhere is an empty population, and an empty population \
         has no row rather than a row of zeroes",
    );

    let fired_nothing = super::demand_ranked_eviction_line((40, 0), (0, 0), (0, 0))
        .expect("evictions happened, so the population is not empty");
    assert!(
        fired_nothing.contains("mrms frames 0 of 40 spared"),
        "a rank that ran and never fired must SAY zero, not go quiet: {fired_nothing:?}",
    );

    let fired =
        super::demand_ranked_eviction_line((40, 3), (12, 1), (30, 5)).expect("evictions happened");
    assert_eq!(
        fired,
        "demand-ranked evictions: mrms frames 3 of 40 spared, \
         gmgsi frames 1 of 12 spared, render cache 5 of 30 spared",
        "the sentence is an interface the browser rig reads with its own regex",
    );

    // A store that never evicted must not be able to hide a store that did.
    assert!(
        super::demand_ranked_eviction_line((0, 0), (0, 0), (7, 2)).is_some(),
        "the denominator is the SUM across the three stores, so one silent \
         store cannot suppress another's reading",
    );
}
