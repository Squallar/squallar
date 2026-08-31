//! A fixed-shape latency histogram for the frame thread to write into.
//!
//! Forty geometric bins spanning 62.5 µs to 64 ms — ten octaves, four bins per
//! octave — plus one clamp bin under the floor and one at or over the ceiling,
//! so no sample is ever unrepresentable. The span brackets every figure the
//! frame instrument quotes: a 4 ms service bar sits mid-range with a bin width
//! of 2¼ (≈19%) around it, and one 60 Hz frame (16.7 ms) is still four bins
//! under the ceiling.
//!
//! The shape is compile-time and the counts are `u32`, so a `Hist` is 176
//! bytes, allocates nothing after construction, and takes no lock: it is
//! **single-writer** by design — one owner records, and readers work on copies
//! ([`Hist::diff`] of two copies is the windowed view). [`Hist::record`] is
//! integer arithmetic end to end; no float touches the hot path.
//!
//! Beside the bins each histogram carries the **exact sum** of what it
//! recorded ([`Hist::mean_micros`]), because at four bins per octave a
//! percentile cannot resolve a small effect and the bin geometry will print a
//! `2.00×` that is not there. The sum survives [`Hist::diff`], so a windowed
//! mean is exact.
//!
//! [`AtomicHist`] is the same shape in a `static`, for the ledger sites that
//! own no recorder to write into.
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

/// Which slot `micros` lands in. **The one bin geometry**, shared by [`Hist`]
/// and [`AtomicHist`] so that a figure recorded through either is diffable
/// against the other — two searches that agreed only by inspection would be a
/// measurement bug waiting to be written, not an optimisation.
fn slot_of(micros: u32) -> usize {
    let ns = micros as u64 * 1_000;
    if ns < edge_ns(0) {
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
    }
}

/// The recorded-count histogram. See the module doc for shape and contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hist {
    counts: [u32; SLOTS],
    /// The exact sum of every `micros` recorded — see [`Hist::mean_micros`].
    sum_micros: u64,
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
        Self {
            counts: [0; SLOTS],
            sum_micros: 0,
        }
    }

    /// Count one sample of `micros`. Integer arithmetic only, no allocation,
    /// no lock; a sample under the floor or at/over the ceiling lands in its
    /// clamp bin. Counts saturate rather than wrap.
    pub fn record(&mut self, micros: u32) {
        let slot = slot_of(micros);
        self.counts[slot] = self.counts[slot].saturating_add(1);
        self.sum_micros = self.sum_micros.saturating_add(u64::from(micros));
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
        out.sum_micros = self.sum_micros.saturating_sub(earlier.sum_micros);
        out
    }

    /// The **exact** sum of every recorded sample, in whole microseconds —
    /// carried beside the bins rather than estimated from them.
    pub fn sum_micros(&self) -> u64 {
        self.sum_micros
    }

    /// The **exact** arithmetic mean in whole microseconds (truncated), or
    /// `None` on an empty histogram.
    ///
    /// # Why a mean sits beside a percentile that cannot be wrong
    ///
    /// The bins are four per octave, so one bin's width is ≈19% and every
    /// percentile this type answers is quantized to a bin edge. Two readings
    /// one bin apart differ by anywhere from 0% to 19%, and **any true ratio
    /// between 1.68× and 2.38× prints as exactly `2.00×`** — an artifact of
    /// the geometry, not a measurement. A running sum costs one `u64` add per
    /// [`Hist::record`] and eight bytes per histogram, and it makes a
    /// small-effect comparison answerable without abandoning the shape that
    /// makes the tail readable.
    ///
    /// It survives [`Hist::diff`], so the **windowed** mean is exact too:
    /// that is the figure a gesture window needs and the one a
    /// cumulative-from-boot percentile cannot give.
    ///
    /// **A mean is not a substitute for the tail**, and no figure derived
    /// here gates CI either. A distribution with a bimodal tail — which is
    /// exactly what a per-tile cost is — has a mean that describes none of
    /// its samples. Read it against the percentiles, never instead of them.
    pub fn mean_micros(&self) -> Option<u64> {
        let total = self.total();
        (total > 0).then(|| self.sum_micros / total)
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

/// [`Hist`]'s shape in a `static`: the same 42 slots and the same running sum,
/// written with relaxed `fetch_add`s so a ledger can record without owning a
/// recorder and without taking a lock.
///
/// This is the [`crate::hist`] answer to the ledger pattern the app already
/// uses for counts (`squallar_egui::overlay_cache::ledger` and friends: always
/// on, no feature gate, every write one `fetch_add` with `Relaxed`). It exists
/// because [`Hist`] is deliberately **single-writer** — a cost measured deep
/// inside a crate, at a site that owns no recorder to hand upward, has nowhere
/// to put a `Hist`.
///
/// # It is not a lock, so it is not a snapshot
///
/// [`AtomicHist::snapshot`] reads the slots one at a time. A concurrent
/// recorder can therefore land between two of those loads, and a snapshot's
/// [`Hist::total`] can disagree with its own sum by the handful of samples
/// that crossed the read. That is the same tear every `_if_moved` ledger
/// reading in this workspace already accepts, and for the same reason: these
/// are running totals whose consumer is a difference of two readings taken
/// seconds apart, so a tear of a few samples is far below the grain of any
/// question asked of them. **Never assert an exact total across a snapshot.**
///
/// The counts are `u32` like [`Hist`]'s and saturate the same way; the sum is
/// `u64`. Size is 176 bytes, allocation-free, `const`-constructible.
#[derive(Debug)]
pub struct AtomicHist {
    counts: [core::sync::atomic::AtomicU32; SLOTS],
    sum_micros: core::sync::atomic::AtomicU64,
}

impl Default for AtomicHist {
    fn default() -> Self {
        Self::new()
    }
}

impl AtomicHist {
    /// An empty histogram. `const`, so it can seed a `static` directly.
    pub const fn new() -> Self {
        #[expect(
            clippy::declare_interior_mutable_const,
            reason = "the array initialiser needs a const to repeat; each \
                      element is a fresh zero, which is the whole point"
        )]
        const ZERO: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        Self {
            counts: [ZERO; SLOTS],
            sum_micros: core::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Count one sample of `micros`: two relaxed `fetch_add`s and one integer
    /// bin search, no allocation and no lock. Counts saturate rather than
    /// wrap, at the cost of a compare-exchange loop only once a slot is
    /// actually at `u32::MAX` — which needs four billion samples in one bin.
    pub fn record(&self, micros: u32) {
        use core::sync::atomic::Ordering::Relaxed;
        let slot = &self.counts[slot_of(micros)];
        if slot.fetch_add(1, Relaxed) == u32::MAX {
            // Wrapped to zero. Put it back on the ceiling; a saturated bin is
            // a bin that has stopped being a measurement either way, and this
            // keeps it from reading as an empty one.
            slot.store(u32::MAX, Relaxed);
        }
        self.sum_micros.fetch_add(u64::from(micros), Relaxed);
    }

    /// The current counts as a plain [`Hist`], so every reader of this type
    /// uses the same percentiles, the same mean and the same
    /// [`Hist::diff`]-based windowing as a directly-owned recorder. See the
    /// type doc: this read is not atomic as a whole.
    pub fn snapshot(&self) -> Hist {
        use core::sync::atomic::Ordering::Relaxed;
        let mut out = Hist::new();
        for (slot, count) in self.counts.iter().enumerate() {
            out.counts[slot] = count.load(Relaxed);
        }
        out.sum_micros = self.sum_micros.load(Relaxed);
        out
    }

    /// Put every slot and the sum back to zero.
    ///
    /// For tests that read a window rather than a running total, exactly as
    /// the count ledgers' `reset` is. Nothing shipped calls it: a reported
    /// line is cumulative from boot and a windowed reading is the difference
    /// of two.
    pub fn reset_for_test(&self) {
        use core::sync::atomic::Ordering::Relaxed;
        for count in &self.counts {
            count.store(0, Relaxed);
        }
        self.sum_micros.store(0, Relaxed);
    }
}

#[cfg(test)]
mod tests;
