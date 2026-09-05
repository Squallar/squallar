//! **What the counting wrapper costs per allocation, measured rather than
//! predicted.**
//!
//! The design predicted "two relaxed atomics per allocation"; this is the
//! figure that prediction is checked against. The same alloc/dealloc workload
//! runs through [`System`] directly and through [`Counting`], which delegates
//! to `System` and adds one `fetch_add` on the way out of `alloc` and one on
//! the way into `dealloc` — so the delta is the wrapper and nothing else.
//! Neither is installed as the global allocator here: both are called
//! directly, so what is measured is the wrapper's own instructions and not
//! whatever else a process does.
//!
//! **The denominator is one alloc+dealloc PAIR**, not one `alloc`. A pair
//! carries two atomic adds, one per side, which is what "two relaxed atomics
//! per allocation" amounts to once both halves are counted.
//!
//! `#[ignore]`d: a measurement instrument, not a criterion. Its answer is this
//! box's, and a threshold asserted on a shared machine would be a load
//! detector rather than a gate — the repo has been bitten by exactly that. It
//! prints; a human reads it.
//!
//! ```text
//! cargo test -p squallar-alloc --test counting_overhead -- --ignored --nocapture
//! ```
//!
//! The arms are **interleaved** (the arm that goes first alternates each
//! round) rather than run as two campaigns, because this box's load moves by
//! tens of points within a session and a sequential comparison would be
//! measuring the box. Both the median and the minimum across rounds are
//! printed: for a fixed per-operation cost the minimum is the least
//! contaminated estimator, since interference can only add, and a median far
//! above it says the box was busy while it ran.
#![cfg(not(target_arch = "wasm32"))]
// The crate under test is `deny(unsafe_code)` with one scoped allow; this
// suite calls `GlobalAlloc` directly, so it carries the same shape.
#![deny(unsafe_code)]

use squallar_alloc::Counting;
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// Sizes one round cycles through, so the measurement is not a single
/// free-list bucket's fast path. Bytes.
const SIZES: [usize; 4] = [32, 64, 256, 1024];

/// alloc+dealloc pairs per arm per round.
///
/// **Sized so the effect is resolvable, not so the suite is quick.** At
/// 500_000 a round took ~2 ms, scheduler noise swamped a ~0.5 ns/pair effect
/// and individual rounds reported the wrapper as FASTER than what it
/// delegates to — impossible, and the tell that the instrument could not see
/// what it was pointed at. At 10 million a round is ~45 ms and the per-round
/// deltas are all the same sign.
const PAIRS_PER_ROUND: usize = 10_000_000;

/// Interleaved rounds per arm that are KEPT.
const ROUNDS: usize = 8;

/// Rounds run and discarded before those, on top of the explicit warm-up.
///
/// **Not tuning: a physically impossible reading is what put it here.** The
/// first kept round once produced a delta of -3.27 ns/pair — the wrapper
/// measuring faster than the allocator it delegates to, which cannot happen —
/// because the first timed round still pays page faults and branch-predictor
/// cold starts that the arm running second does not. Discarding it is the
/// honest fix; keeping it and reporting a negative minimum would not be.
const DISCARDED_ROUNDS: usize = 1;

/// One round of `pairs` alloc/dealloc pairs through `allocator`, cycling
/// [`SIZES`].
///
/// The pointer goes through `black_box` so the pair cannot be optimised away,
/// and the layout comes back out of `black_box` so the size cycle cannot be
/// folded into one constant bucket.
#[allow(
    unsafe_code,
    reason = "calls GlobalAlloc directly; that is the thing being measured"
)]
fn round<A: GlobalAlloc>(allocator: &A, pairs: usize) -> Duration {
    let layouts: Vec<Layout> = SIZES
        .iter()
        .map(|size| Layout::from_size_align(*size, 8).expect("a valid layout"))
        .collect();
    let started = Instant::now();
    for i in 0..pairs {
        let layout = black_box(layouts[i % layouts.len()]);
        // SAFETY: `layout` has a non-zero size, which is `alloc`'s only
        // precondition; the block is freed with the same layout below and its
        // contents are never read.
        let ptr = unsafe { allocator.alloc(layout) };
        assert!(!ptr.is_null(), "the system refused a {layout:?}");
        // SAFETY: `ptr` came from this allocator with this layout one line
        // above, and is not used afterwards.
        unsafe { allocator.dealloc(black_box(ptr), layout) };
    }
    started.elapsed()
}

/// **[`Counting`] really is counting**, asserted the only way a level can
/// show it: hold a block open and watch the level carry exactly its bytes.
///
/// Sized distinctively so the assertion cannot pass on somebody else's
/// allocation — nothing else in this process allocates 3 MiB + 17 B.
#[allow(
    unsafe_code,
    reason = "calls GlobalAlloc directly, as the rest of this suite does"
)]
fn counting_accounts_for_a_block_it_is_holding() {
    const ODD: usize = (3 << 20) + 17;
    let layout = Layout::from_size_align(ODD, 8).expect("a valid layout");
    let before = squallar_alloc::live_bytes().expect("the warm-up rounds have counted");
    // SAFETY: a non-zero-size layout; freed below with the same layout and
    // never read.
    let ptr = unsafe { Counting.alloc(layout) };
    assert!(!ptr.is_null());
    let held = squallar_alloc::live_bytes().expect("still counting");
    assert_eq!(
        held - before,
        ODD as u64,
        "holding a {ODD} B block did not move the level by {ODD} B, so \
         `Counting` is not the allocator this suite is timing",
    );
    // SAFETY: `ptr` came from this allocator with this layout.
    unsafe { Counting.dealloc(ptr, layout) };
    assert_eq!(
        squallar_alloc::live_bytes(),
        Some(before),
        "freeing the block did not bring the level back",
    );
}

/// Stand-ins for the crate's own two counters, so the control below executes
/// the same instruction sequence `Counting` does without going near an
/// allocator. Separate cache lines are NOT forced: the real pair share a
/// module and are adjacent, and the point is to reproduce what the wrapper
/// does, not to model a better version of it.
static CONTROL_ALLOCATED: AtomicU64 = AtomicU64::new(0);
static CONTROL_FREED: AtomicU64 = AtomicU64::new(0);

/// **The control: two relaxed `fetch_add`s and nothing else**, `pairs` times.
///
/// This is what the delta above is a measurement OF, isolated from the
/// allocator it normally hides inside. Without it the delta is a number with
/// no expectation to check against — and the expectation matters, because two
/// `lock`-prefixed read-modify-writes are conventionally quoted at tens of
/// cycles each, which is an order of magnitude above what the delta reads.
/// If this control agrees with the delta, the delta is the atomics; if the
/// control is much larger, the atomics are executing in the shadow of the
/// allocator's own work and the delta is the honest marginal cost anyway.
fn atomics_only(pairs: usize) -> Duration {
    let started = Instant::now();
    for i in 0..pairs {
        let bytes = black_box(i as u64 & 0xff);
        CONTROL_ALLOCATED.fetch_add(bytes, Relaxed);
        CONTROL_FREED.fetch_add(bytes, Relaxed);
    }
    black_box(CONTROL_ALLOCATED.load(Relaxed));
    black_box(CONTROL_FREED.load(Relaxed));
    started.elapsed()
}

/// Nanoseconds per alloc+dealloc pair, from a round's duration.
fn ns_per_pair(elapsed: Duration, pairs: usize) -> f64 {
    elapsed.as_nanos() as f64 / pairs as f64
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

/// **The wrapper's cost per alloc+dealloc pair, both arms interleaved.**
///
/// Prints the two arms and their delta. Asserts only what cannot be a load
/// artefact: that the counter actually counted. A `Counting` arm the optimiser
/// had folded into `System` would report a flattering zero, and that is the
/// one result worth refusing rather than printing.
#[test]
#[ignore = "a measurement instrument, not a criterion; see the doc comment for the invocation"]
fn the_counting_wrapper_costs_this_much_per_allocation() {
    // Warm both paths so neither arm pays a first-touch cost the other does not.
    round(&System, 50_000);
    round(&Counting, 50_000);

    // **The falsifiability floor, before anything is timed.** A `Counting`
    // arm the optimiser had folded into `System` would report a flattering
    // delta of zero, and that is the one result worth refusing rather than
    // printing. `live_bytes` is a LEVEL, not a running total — every pair
    // above frees what it allocated, so it reads the same before and after a
    // round however many allocations went through it — so the check holds a
    // block OPEN across the reading and asserts the level moved by exactly
    // that block's size.
    counting_accounts_for_a_block_it_is_holding();

    let mut system = Vec::with_capacity(ROUNDS);
    let mut counting = Vec::with_capacity(ROUNDS);
    for r in 0..ROUNDS + DISCARDED_ROUNDS {
        // The arm that goes first alternates, so a drift in either direction
        // over the run cancels rather than accruing to one arm.
        if r.is_multiple_of(2) {
            system.push(ns_per_pair(
                round(&System, PAIRS_PER_ROUND),
                PAIRS_PER_ROUND,
            ));
            counting.push(ns_per_pair(
                round(&Counting, PAIRS_PER_ROUND),
                PAIRS_PER_ROUND,
            ));
        } else {
            counting.push(ns_per_pair(
                round(&Counting, PAIRS_PER_ROUND),
                PAIRS_PER_ROUND,
            ));
            system.push(ns_per_pair(
                round(&System, PAIRS_PER_ROUND),
                PAIRS_PER_ROUND,
            ));
        }
    }
    // The discarded rounds ran first and are dropped here rather than skipped
    // above, so both arms paid exactly the same warm-up.
    system.drain(..DISCARDED_ROUNDS);
    counting.drain(..DISCARDED_ROUNDS);
    assert_eq!(system.len(), ROUNDS);
    assert_eq!(counting.len(), ROUNDS);

    // **The delta is PAIRED, per round, and never a difference of two
    // independent minima.** The arms of one round run adjacently, so their
    // difference cancels the drift both of them sat in; `min(counting) -
    // min(system)` takes its two terms from different rounds with different
    // cache and load states and can land anywhere, including below the true
    // cost. The first spelling of this suite did exactly that and reported
    // 0.42 ns/pair for two `lock`-prefixed read-modify-writes, which is not a
    // figure any x86 can produce — the arms are what is summarised, the
    // per-round difference is what is measured.
    let mut control: Vec<f64> = (0..ROUNDS)
        .map(|_| ns_per_pair(atomics_only(PAIRS_PER_ROUND), PAIRS_PER_ROUND))
        .collect();

    let mut deltas: Vec<f64> = counting
        .iter()
        .zip(system.iter())
        .map(|(c, s)| c - s)
        .collect();
    let per_round: Vec<String> = deltas.iter().map(|d| format!("{d:.2}")).collect();

    let sys_min = system.iter().copied().fold(f64::MAX, f64::min);
    let cnt_min = counting.iter().copied().fold(f64::MAX, f64::min);
    let delta_min = deltas.iter().copied().fold(f64::MAX, f64::min);
    let ctl_min = control.iter().copied().fold(f64::MAX, f64::min);
    let sys_med = median(&mut system);
    let cnt_med = median(&mut counting);
    let delta_med = median(&mut deltas);
    let ctl_med = median(&mut control);

    println!(
        "\ncounting allocator overhead, {ROUNDS} interleaved rounds of \
         {PAIRS_PER_ROUND} alloc+dealloc pairs ({DISCARDED_ROUNDS} discarded \
         first), sizes {SIZES:?} B:\n  \
         System              min {sys_min:6.2} ns/pair   median {sys_med:6.2} ns/pair\n  \
         Counting            min {cnt_min:6.2} ns/pair   median {cnt_med:6.2} ns/pair\n  \
         delta (paired)      min {delta_min:6.2} ns/pair   median {delta_med:6.2} ns/pair\n  \
         per-round deltas    [{}] ns/pair\n  \
         control: 2 fetch_adds alone, no allocator\n  \
         two relaxed adds    min {ctl_min:6.2} ns/pair   median {ctl_med:6.2} ns/pair\n",
        per_round.join(", "),
    );
}
