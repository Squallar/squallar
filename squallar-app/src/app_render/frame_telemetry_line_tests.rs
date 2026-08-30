//! The five frame timing sentences, pinned word for word.
//!
//! The browser rig reads telemetry sentences out of the console ring with
//! regexes it owns, so a sentence is an interface: an extra space is not a
//! compile error anywhere and silently turns a rig reading into `null`. The
//! raster lines pin themselves against `drive.py`'s own patterns; these lines
//! have no probe in `drive.py` yet, so they are pinned here as literals —
//! the sentence a probe will be written against is the sentence these tests
//! hold still.
//!
//! Every expected percentile below is derived by hand from the histogram's
//! documented edge formula (⌊62 500 × 2^(j/4)⌋ ns shifted per octave, upper
//! edge, rounded up to whole microseconds) — not read back from the code
//! under test.

use squallar_device_profile::hist::Hist;
use squallar_gpu::egui_renderer::pass_costs::PassCosts;

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
