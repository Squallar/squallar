//! A fixed-shape latency histogram for the frame thread to write into.
//!
//! Forty geometric bins spanning 62.5 µs to 64 ms — ten octaves, four bins per
//! octave — plus one clamp bin under the floor and one at or over the ceiling,
//! so no sample is ever unrepresentable. The span brackets every figure the
//! frame instrument quotes: a 4 ms service bar sits mid-range with a bin width
//! of 2¼ (≈19%) around it, and one 60 Hz frame (16.7 ms) is still four bins
//! under the ceiling.
//!
//! The shape is compile-time and the counts are `u32`, so a `Hist` is 168
//! bytes, allocates nothing after construction, and takes no lock: it is
//! **single-writer** by design — one owner records, and readers work on copies
//! ([`Hist::diff`] of two copies is the windowed view). [`Hist::record`] is
//! integer arithmetic end to end; no float touches the hot path.
//!
//! **No figure derived from this type ever gates CI.** Percentiles are
//! estimates quantized to bin edges, answered conservatively (the *upper* edge
//! of the bin holding the ranked sample), so an estimate is never smaller than
//! the true value by more than nothing and never flatters a slow frame.

/// Geometric bins between the floor and the ceiling.
pub const GEOMETRIC_BINS: usize = 40;

/// Total count slots: under-floor clamp + geometric bins + at-or-over-ceiling
/// clamp, in that order.
pub const SLOTS: usize = GEOMETRIC_BINS + 2;

/// The first octave of bin edges, nanoseconds: ⌊62 500 × 2^(j/4)⌋ for
/// j = 0..4. Every later edge is one of these shifted left, so bins exactly
/// four apart differ by exactly a factor of two.
const FIRST_OCTAVE_NS: [u64; 4] = [62_500, 74_325, 88_388, 105_112];

/// Bin edge `i` in nanoseconds, for `i` in `0..=GEOMETRIC_BINS`. Edge 0 is the
/// 62.5 µs floor; edge 40 is the 64 ms ceiling; geometric bin `k` (slot
/// `k + 1`) covers `[edge(k), edge(k + 1))`.
const fn edge_ns(i: usize) -> u64 {
    FIRST_OCTAVE_NS[i % 4] << (i / 4)
}

/// The recorded-count histogram. See the module doc for shape and contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hist {
    counts: [u32; SLOTS],
}

impl Default for Hist {
    fn default() -> Self {
        Self::new()
    }
}

impl Hist {
    /// An empty histogram. `const`, so it can seed a `static` or a field
    /// without a runtime step.
    pub const fn new() -> Self {
        Self { counts: [0; SLOTS] }
    }

    /// Count one sample of `micros`. Integer arithmetic only, no allocation,
    /// no lock; a sample under the floor or at/over the ceiling lands in its
    /// clamp bin. Counts saturate rather than wrap.
    pub fn record(&mut self, micros: u32) {
        let ns = micros as u64 * 1_000;
        let slot = if ns < edge_ns(0) {
            0
        } else if ns >= edge_ns(GEOMETRIC_BINS) {
            SLOTS - 1
        } else {
            // Invariant: edge_ns(lo) <= ns < edge_ns(hi).
            let mut lo = 0;
            let mut hi = GEOMETRIC_BINS;
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if ns >= edge_ns(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo + 1
        };
        self.counts[slot] = self.counts[slot].saturating_add(1);
    }

    /// Samples recorded, clamp bins included.
    pub fn total(&self) -> u64 {
        self.counts.iter().map(|&c| u64::from(c)).sum()
    }

    /// The raw count slots: `[0]` under the floor, `[1..=40]` the geometric
    /// bins in edge order, `[41]` at or over the ceiling.
    pub fn counts(&self) -> &[u32; SLOTS] {
        &self.counts
    }

    /// The counts this histogram gained since `earlier` — the windowed view
    /// two snapshots of one recorder make. `earlier` must be an earlier copy
    /// of the same recorder; the subtraction saturates, so a swapped pair
    /// reads as zeros rather than as garbage.
    pub fn diff(&self, earlier: &Hist) -> Hist {
        let mut out = Hist::new();
        for (slot, (now, then)) in self.counts.iter().zip(earlier.counts.iter()).enumerate() {
            out.counts[slot] = now.saturating_sub(*then);
        }
        out
    }

    /// The conservative `q`-quantile in microseconds: the **upper** edge
    /// (rounded up) of the bin holding the `⌈q·total⌉`-th smallest sample.
    /// `None` on an empty histogram. A sample in the under-floor clamp answers
    /// the floor's edge; one in the over-ceiling clamp answers `u32::MAX`,
    /// because that bin has no upper edge to be conservative with.
    pub fn percentile_upper_micros(&self, q: f64) -> Option<u32> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        let rank = ((q * total as f64).ceil() as u64).clamp(1, total);
        let mut seen = 0u64;
        for (slot, &count) in self.counts.iter().enumerate() {
            seen += u64::from(count);
            if seen >= rank {
                if slot == SLOTS - 1 {
                    return Some(u32::MAX);
                }
                // The under-floor clamp's upper bound is edge 0 and geometric
                // slot `s` covers `[edge(s-1), edge(s))`, so `edge_ns(slot)`
                // is the upper bound of every non-clamp slot. Rounded UP to
                // whole microseconds: conservative.
                return Some(edge_ns(slot).div_ceil(1_000).min(u64::from(u32::MAX)) as u32);
            }
        }
        unreachable!("total() counted a sample the walk did not reach");
    }
}

#[cfg(test)]
mod tests;
