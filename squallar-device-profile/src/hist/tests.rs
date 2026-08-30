use super::{GEOMETRIC_BINS, Hist, SLOTS, edge_ns};

/// The bin edges are strictly increasing, anchored at the documented floor and
/// ceiling, and bins four apart differ by exactly a factor of two — the
/// property that makes the geometric spacing exact rather than approximate.
#[test]
fn the_edges_are_monotone_and_double_every_octave() {
    assert_eq!(edge_ns(0), 62_500, "the floor moved off 62.5 us");
    assert_eq!(
        edge_ns(GEOMETRIC_BINS),
        64_000_000,
        "the ceiling moved off 64 ms"
    );
    for i in 0..GEOMETRIC_BINS {
        assert!(
            edge_ns(i) < edge_ns(i + 1),
            "edges {i} and {} are not increasing: {} vs {}",
            i + 1,
            edge_ns(i),
            edge_ns(i + 1),
        );
    }
    for i in 0..=(GEOMETRIC_BINS - 4) {
        assert_eq!(
            edge_ns(i + 4),
            2 * edge_ns(i),
            "edge {} is not exactly double edge {i}",
            i + 4,
        );
    }
}

/// A recorded value's quantile answer is never below the value itself (for
/// values the geometric range can bound at all), and never more than one bin
/// width (a factor of 2^(1/4), rounded up) above it.
#[test]
fn the_percentile_answer_is_a_conservative_upper_edge() {
    for micros in [63u32, 100, 1_000, 3_999, 4_000, 16_667, 63_999] {
        let mut h = Hist::new();
        h.record(micros);
        let answer = h
            .percentile_upper_micros(1.0)
            .expect("one sample was recorded");
        assert!(
            answer >= micros,
            "{micros} us was answered as {answer} us — the estimate \
             understates the sample it holds",
        );
        // 2^(1/4) < 1.1893, plus 1 us for the ceil on the edge itself.
        let ceiling = (u64::from(micros) * 11_893).div_ceil(10_000) + 1;
        assert!(
            u64::from(answer) <= ceiling,
            "{micros} us was answered as {answer} us — more than one bin \
             width above the sample",
        );
    }
}

/// The clamp bins hold what the range cannot: a sample under the floor
/// answers the floor's edge, one at or over the ceiling answers `u32::MAX`,
/// and both are counted in the total.
#[test]
fn the_clamp_bins_catch_what_the_range_cannot() {
    let mut h = Hist::new();
    h.record(10); // under 62.5 us
    assert_eq!(h.counts()[0], 1, "an under-floor sample missed its clamp");
    assert_eq!(h.percentile_upper_micros(1.0), Some(63));

    let mut h = Hist::new();
    h.record(64_000); // exactly the ceiling
    h.record(100_000); // past it
    assert_eq!(
        h.counts()[SLOTS - 1],
        2,
        "at-or-over-ceiling samples missed their clamp"
    );
    assert_eq!(
        h.percentile_upper_micros(0.5),
        Some(u32::MAX),
        "the over-ceiling clamp has no upper edge and must not invent one",
    );
    assert_eq!(h.total(), 2);
}

/// An empty histogram answers `None`, not a number.
#[test]
fn an_empty_histogram_has_no_percentile() {
    assert_eq!(Hist::new().percentile_upper_micros(0.5), None);
    assert_eq!(Hist::new().total(), 0);
}

/// `diff` of two snapshots equals a fresh histogram of only the samples
/// recorded between them — bin for bin, on samples spread across the range
/// and both clamps.
#[test]
fn diff_recovers_exactly_the_window_between_two_snapshots() {
    let before = [10u32, 100, 100, 5_000];
    let after = [200u32, 5_000, 70_000, 63];

    let mut recorder = Hist::new();
    for &v in &before {
        recorder.record(v);
    }
    let snapshot = recorder;
    for &v in &after {
        recorder.record(v);
    }

    let mut expected = Hist::new();
    for &v in &after {
        expected.record(v);
    }
    assert_eq!(
        recorder.diff(&snapshot),
        expected,
        "the window between two snapshots is not the samples recorded \
         between them",
    );
    assert_eq!(recorder.diff(&snapshot).total(), after.len() as u64);

    // A swapped pair saturates to empty rather than underflowing.
    assert_eq!(snapshot.diff(&recorder).total(), 0);
}

/// Percentiles of a known mixture land in the known bins: 90 samples at
/// 100 us and 10 at 10 000 us put p50 on the 100 us bin's upper edge and
/// p95 on the 10 000 us bin's — both values computed from the documented
/// edge formula independently of the implementation.
#[test]
fn percentiles_of_a_known_distribution_land_on_the_known_edges() {
    let mut h = Hist::new();
    for _ in 0..90 {
        h.record(100);
    }
    for _ in 0..10 {
        h.record(10_000);
    }
    assert_eq!(h.total(), 100);
    // 100 us = 100 000 ns sits in [88 388, 105 112) ns; upper edge
    // ceil(105 112 / 1000) = 106 us.
    assert_eq!(h.percentile_upper_micros(0.50), Some(106));
    assert_eq!(h.percentile_upper_micros(0.90), Some(106));
    // 10 000 us = 10^7 ns sits in [9 513 600, 11 313 664) ns
    // (74 325 << 7 and 88 388 << 7); upper edge ceil(11 313 664 / 1000)
    // = 11 314 us.
    assert_eq!(h.percentile_upper_micros(0.95), Some(11_314));
    assert_eq!(h.percentile_upper_micros(1.0), Some(11_314));
}

/// Recording is total over the input domain and the slot walk matches the
/// binary search: every representable sample lands in exactly one slot whose
/// edges bracket it.
#[test]
fn every_sample_lands_inside_its_own_slots_edges() {
    for micros in [0u32, 62, 63, 74, 75, 500, 8_000, 63_999, 64_000, u32::MAX] {
        let mut h = Hist::new();
        h.record(micros);
        let slot = h
            .counts()
            .iter()
            .position(|&c| c == 1)
            .expect("the sample was counted somewhere");
        let ns = u64::from(micros) * 1_000;
        match slot {
            0 => assert!(ns < edge_ns(0), "{micros} us clamped low wrongly"),
            s if s == SLOTS - 1 => assert!(
                ns >= edge_ns(GEOMETRIC_BINS),
                "{micros} us clamped high wrongly"
            ),
            s => assert!(
                edge_ns(s - 1) <= ns && ns < edge_ns(s),
                "{micros} us landed in slot {s} whose edges do not hold it",
            ),
        }
    }
}
