//! The five frame timing sentences, pinned word for word — twice.
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
use squallar_gpu::egui_renderer::pass_costs::PassCosts;

/// The rig driver, read at compile time so a moved or deleted file is a
/// build failure rather than a skipped test.
const DRIVE_PY: &str = include_str!("../../../.github/browser-rig/drive.py");

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

    use squallar_gpu::gpu_probe::{GpuPassReport, ProbedPass};
    let mut report = GpuPassReport {
        hists: [Hist::new(); 4],
        passes: [6, 0, 1, 2],
        frames: 3,
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
        &mut phases.shell,
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
    let names = ["poll", "layout", "shell", "panes", "apply", "chrome"];
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
