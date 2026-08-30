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
    const GROUP_SPELLINGS: [&str; 4] = [
        r"(\d+|none|over)",
        r"(\d+)",
        r"([0-9,]+)",
        r"([a-z0-9-]+)",
    ];
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
/// upper edge. 100 us sits in [88 388, 105 112) ns; upper edge 106 us.
#[test]
fn the_idle_service_line_reads_exactly_as_pinned() {
    let mut h = Hist::new();
    for _ in 0..3 {
        h.record(100);
    }
    assert_eq!(
        super::frame_service_idle_line(&h),
        "frame service (idle): n=3, p50=106 us, p90=106 us, p99=106 us",
    );
}

/// An empty family prints `n=0` and `none` percentiles rather than nothing:
/// "nobody touched the window" must be a readable figure, because it is the
/// manual check that the interact family is not contaminated by idle frames.
#[test]
fn an_empty_family_prints_a_zero_not_an_absence() {
    assert_eq!(
        super::frame_service_idle_line(&Hist::new()),
        "frame service (idle): n=0, p50=none us, p90=none us, p99=none us",
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
    assert_eq!(
        super::frame_service_idle_line(&idle),
        rendered(&pattern("svc_idle_re"), &["3", "106", "106", "106"]),
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
            &["106", "211", "595", "1190", "2379", "4757", "2", "3364", "5657"],
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
    let good = rendered(&pattern("svc_idle_re"), &["0", "none", "none", "none"]);
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
