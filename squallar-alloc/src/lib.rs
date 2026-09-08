#![warn(clippy::all)]
// `deny`, not `forbid`: the one `unsafe impl` below carries a scoped `allow`,
// which `forbid` cannot be overridden by. Everything else in the crate errors.
#![deny(unsafe_code)]

//! **Live host bytes, counted at the allocator.**
//!
//! Every other pressure input in the tree is a high-water mark: a wasm
//! instance's `memory().buffer().byteLength` only ever grows, and a GPU
//! `OutOfMemory` event says nothing about recovery. A governor that lowers a
//! presumption on such a signal and cannot observe the heap coming back is a
//! ratchet. This crate is the figure that can fall: what the process has
//! allocated less what it has freed, read off two `Relaxed` atomics that the
//! global allocator bumps on every call.
//!
//! # What it is, and what it is not
//!
//! [`live_bytes`] is **bytes the allocator has handed out and not yet been
//! handed back**, on the instance whose statics these are. It is the same
//! figure on native and on wasm, because the same [`Counting`] wraps
//! `std::alloc::System` on both. It is not the process's resident set (the
//! allocator's own reserve, fragmentation and the module's statics are all
//! outside it), and on wasm it is not `byteLength` — a linear memory never
//! shrinks, so `byteLength − live_bytes` is exactly the freed-but-reserved
//! headroom the high-water mark hides.
//!
//! **Per instance, not per process, on wasm.** The rasterization worker is a
//! second instantiation of the same module with its own linear memory, and a
//! wasm static lives in one memory; its counters are its own and the page's
//! are the page's. The two are never added — they are two heaps under two
//! ceilings — and the worker's figure crosses to the page on the messages
//! that already cross (`squallar_web::worker_protocol::LIVE`).
//!
//! # The counter's own rules
//!
//! * **Count only what was granted.** The counters move on a non-null return
//!   from `alloc`, `alloc_zeroed` and `realloc`, never on the request: an
//!   allocation the system refuses must not drift the live figure upward, or
//!   the one moment the figure matters most is the moment it lies.
//! * **Nothing here allocates.** The hooks are a delegation and two atomic
//!   adds; a `log::`, a `format!` or a `Vec` inside the allocator re-enters
//!   it.
//! * **Two atomics, `Relaxed`.** A reader wants a recent figure, not a
//!   synchronised one; the cost is one fetch-add on each call, which is the
//!   prediction the frame-time lane's `frame prepare` p50/p99 gate this
//!   against.
//!
//! # Installing it
//!
//! The `#[global_allocator]` declaration is not `unsafe` and lives in the
//! binary crates — the `squallar` shell and `squallar-web` — never here, so a
//! library that depends on this crate for the reading does not also install
//! it. A process that never installed it reads [`live_bytes`] as `None`: no
//! allocation has passed through the counter, which is the honest answer and
//! not a heap of zero bytes.

/// **What the operating system has given this process**, against what the
/// allocator above was asked for. `live_bytes` is a request; a resident set
/// is a fact, and on one native reading of the heavy scene the two were
/// 874 MiB apart with nothing able to name a byte of the gap.
pub mod process;

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::alloc::System;

/// Bytes every granted allocation has ever asked for, summed. Monotone.
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

/// Bytes every freed allocation had, summed. Monotone.
static FREED: AtomicU64 = AtomicU64::new(0);

/// `std::alloc::System`, counted. Declare it as the global allocator in a
/// binary crate:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;
/// ```
pub struct Counting;

/// **The high-water mark of [`live_bytes`]**, and the term that decides
/// whether a residency lever can reach a wasm page's death at all.
///
/// On `wasm32-unknown-unknown` a linear memory never shrinks, so the page's
/// `byteLength` is approximately `peak(live) + fragmentation`. Those two are
/// reached by completely different work — a past peak is bytes that really
/// were held at some instant, so every residency lever in the tree moves it;
/// fragmentation is a property of allocation SHAPE and no residency lever
/// touches it — and until this static existed nothing could tell them apart.
/// A census that reported `live` at rest against a `byteLength` that had
/// already grown was steering by the wrong one of the two.
///
/// # **A RUNNING MAXIMUM ON THE GRANT PATH, and never a maximum over samples**
///
/// This is the one thing about this static that may not be "optimised" later,
/// so it is written down rather than left to be inferred from where the
/// `fetch_max` happens to sit.
///
/// A peak taken by a sampler — the 2 s telemetry tick, the 250 ms process
/// sampler, anything periodic — is a **false zero for every transient shorter
/// than its interval**, and the transients are the entire point. The pair
/// this was built to score exists only *during* one overlay delivery: a
/// reply's `Vec<u8>` and the `Vec<Color32>` collected out of it are both live
/// at the same instant, 79.05 MB together at the 4317x2477 plan, and gone
/// long before any tick. A sampled peak misses exactly that, **and misses it
/// in the direction that makes removing the copy look worthless** — the fix
/// would move a number nothing observed, so it would read as no fix at all.
///
/// A `fetch_max` at the allocation site cannot miss it: every grant that
/// could raise the peak raises it, because every grant runs through here.
/// That is why the cost lives on the hot path and not on a tick, and it is
/// why moving it to a tick would silently invert the instrument's verdict
/// rather than merely blur it.
static PEAK: AtomicU64 = AtomicU64::new(0);

/// A granted block of `bytes` came into being.
///
/// **Three memory operations rather than one, and the two new ones are the
/// price of [`PEAK`].** The `fetch_add` already answers what `ALLOCATED` was,
/// so the live figure at this instant is that plus this block less `FREED` —
/// one `Relaxed` load — and the peak is one `fetch_max`. On the wasm page,
/// which is single-threaded and where this instrument matters most, those are
/// plain loads and stores with no fence at all.
///
/// **The skew is named and it is bounded by one block.** `FREED` can advance
/// between the two reads, which makes the live figure read high and the peak
/// over-report by at most the concurrent free; it can also lag, which makes
/// it under-report by the same. That is the skew [`live_bytes`] already
/// documents for its own two loads, and a high-water mark tolerates it for
/// the same reason: a reader wants a recent figure, not a synchronised one.
#[inline]
fn granted(bytes: usize) {
    // **Filed BEFORE the peak is taken, and the order is load-bearing.** The
    // companion below records how many large blocks were live *at the peak*,
    // and this block is one of them: if the histogram ran after, a peak that
    // this very grant set would be described as having one fewer large block
    // than it did — and the block that set it would be the missing one.
    note_large_grant(bytes);
    let live = ALLOCATED.fetch_add(bytes as u64, Relaxed) + bytes as u64 - FREED.load(Relaxed);
    // **The companion is stored only on an ADVANCE**, and the branch is free:
    // `fetch_max` already answers what the peak was, so the comparison costs
    // nothing extra and the store is on a path that after warm-up almost
    // never runs. A peak is monotone, so "the peak moved" is rare by
    // construction — which is what makes it affordable to record what the
    // peak was MADE OF at the instant it was set, rather than leaving a
    // reader to infer it from a level sampled at some other time.
    if PEAK.fetch_max(live, Relaxed) < live {
        PEAK_LARGE.store(LIVE_LARGE.load(Relaxed), Relaxed);
    }
}

/// The smallest grant [`note_large_grant`] records.
///
/// 1 MiB, because the question the histogram answers is which blocks make a
/// wasm arena ratchet, and a block the allocator serves out of an existing
/// free chunk never does. Below this every grant is a compare and nothing
/// else.
pub const LARGE_GRANT_FLOOR: usize = 1 << 20;

/// Octaves the histogram covers, from [`LARGE_GRANT_FLOOR`] up.
const LARGE_GRANT_OCTAVES: usize = 20;

/// Buckets inside one octave. **Eight, and the number is the requirement.**
///
/// A plain `log2` histogram cannot answer the question this exists for: a
/// 31.6 MB request and a 34 MB one are the difference between "an existing
/// free chunk absorbs it" and "the heap ratchets", and a reader that puts two
/// such sizes in one bucket has lost the whole signal. Eight linear
/// sub-buckets inside each octave is a resolution of one eighth of the
/// octave — 4 MiB wide at 32 MiB — so those two land in bucket 39 and bucket
/// 40 rather than together.
///
/// **And where the resolution still is not enough, [`LargeGrant::max`] is
/// exact.** Two requests inside one bucket differ by less than an eighth of
/// their own size; the bucket still records the largest of them to the byte,
/// which is the figure that decides whether a free chunk can serve it.
const LARGE_GRANT_SUBS: usize = 8;

/// How many buckets [`large_grant`] indexes.
pub const LARGE_GRANT_BUCKETS: usize = LARGE_GRANT_OCTAVES * LARGE_GRANT_SUBS;

/// Grants at or above [`LARGE_GRANT_FLOOR`], by size: how many, how many
/// bytes, and the largest single one. Three arrays rather than an array of
/// triples so each is a flat run of `AtomicU64` with no padding.
static LARGE_COUNT: [AtomicU64; LARGE_GRANT_BUCKETS] =
    [const { AtomicU64::new(0) }; LARGE_GRANT_BUCKETS];
static LARGE_BYTES: [AtomicU64; LARGE_GRANT_BUCKETS] =
    [const { AtomicU64::new(0) }; LARGE_GRANT_BUCKETS];
static LARGE_MAX: [AtomicU64; LARGE_GRANT_BUCKETS] =
    [const { AtomicU64::new(0) }; LARGE_GRANT_BUCKETS];

/// Blocks at or above [`LARGE_GRANT_FLOOR`] that are live **right now**. A
/// level, unlike the three histogram arrays, which are cumulative flow.
static LIVE_LARGE: AtomicU64 = AtomicU64::new(0);

/// [`LIVE_LARGE`] **at the instant [`PEAK`] was last set** — what the peak was
/// made of, not what is live now.
///
/// Stored only when the peak advances, which is rare after warm-up. That is
/// the whole design: a reader asking "the peak was 880 MiB — of what?" gets
/// an answer taken at the peak rather than one sampled later, when the
/// transients that set it are long gone. Sampling this on a tick would answer
/// a different question and look like the same one.
static PEAK_LARGE: AtomicU64 = AtomicU64::new(0);

/// Which bucket `bytes` falls in, or `None` below [`LARGE_GRANT_FLOOR`].
///
/// The octave is `floor(log2(bytes))` and the sub-bucket is the three bits
/// below the leading one, which is the linear position inside the octave.
/// Sizes past the top octave saturate into the last bucket rather than
/// wrapping — a 512 GiB request is not a case this needs resolution on, but
/// it is one it must not index out of.
fn large_grant_bucket(bytes: usize) -> Option<usize> {
    if bytes < LARGE_GRANT_FLOOR {
        return None;
    }
    let bytes = bytes as u64;
    let octave = 63 - bytes.leading_zeros() as usize;
    let sub = ((bytes >> (octave - 3)) & 7) as usize;
    let idx = (octave - 20) * LARGE_GRANT_SUBS + sub;
    Some(idx.min(LARGE_GRANT_BUCKETS - 1))
}

/// File one grant in the histogram. Nothing below the floor, and never a
/// panic: the index is read through `get`, because an allocator that indexes
/// out of bounds takes the process down from inside the allocator.
#[inline]
fn note_large_grant(bytes: usize) {
    let Some(idx) = large_grant_bucket(bytes) else {
        return;
    };
    LIVE_LARGE.fetch_add(1, Relaxed);
    if let (Some(count), Some(sum), Some(max)) = (
        LARGE_COUNT.get(idx),
        LARGE_BYTES.get(idx),
        LARGE_MAX.get(idx),
    ) {
        count.fetch_add(1, Relaxed);
        sum.fetch_add(bytes as u64, Relaxed);
        max.fetch_max(bytes as u64, Relaxed);
    }
}

/// One bucket of the large-grant histogram.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LargeGrant {
    /// The smallest size this bucket accepts, in bytes.
    pub low: u64,
    /// The smallest size the NEXT bucket accepts: this one is `low..high`.
    pub high: u64,
    /// Grants filed here, ever. Monotone.
    pub count: u64,
    /// Their sizes summed. Monotone.
    pub bytes: u64,
    /// **The largest single grant in this bucket, exactly** — the figure that
    /// decides whether a free chunk can serve a repeat of it, and the reason
    /// a bucket is still readable at finer resolution than its own width.
    pub max: u64,
}

/// Read one bucket, or `None` past the last. Atomic loads only: hook-safe,
/// and it allocates nothing.
pub fn large_grant(idx: usize) -> Option<LargeGrant> {
    let count = LARGE_COUNT.get(idx)?.load(Relaxed);
    let octave = 20 + idx / LARGE_GRANT_SUBS;
    let sub = idx % LARGE_GRANT_SUBS;
    let step = 1u64 << (octave - 3);
    let low = (1u64 << octave) + step * sub as u64;
    Some(LargeGrant {
        low,
        high: low + step,
        count,
        bytes: LARGE_BYTES.get(idx)?.load(Relaxed),
        max: LARGE_MAX.get(idx)?.load(Relaxed),
    })
}

/// **The peak [`live_bytes`] has ever reached** on this instance.
///
/// `None` on the same terms [`live_bytes`] is: nothing has been granted
/// through the counter, so this binary never declared [`Counting`].
///
/// # What it is for
///
/// `byteLength - live_peak` is the part of a wasm page's linear memory that
/// no instant of this program ever needed — fragmentation and the allocator's
/// own reserve — and `live_peak` is the part that some instant did.
/// **Measured 2026-09-08 the two were confused**: a Tier-2 `long` leg died at
/// a 1024 MiB page ceiling holding 490 MiB live, and three sessions read the
/// 399 MiB difference as a holder nobody had counted. It is not a holder. It
/// is one of these two terms, and which one decides whether any residency
/// lever in this tree can reach the death at all.
pub fn live_peak_bytes() -> Option<u64> {
    if ALLOCATED.load(Relaxed) == 0 {
        return None;
    }
    Some(PEAK.load(Relaxed))
}

/// **How many blocks over [`LARGE_GRANT_FLOOR`] were live when
/// [`live_peak_bytes`] was last set** — what the peak was composed of.
///
/// Answers "the peak was 880 MiB and N large blocks were live" directly,
/// where a level read on a tick can only answer it by inference and would be
/// answering about a different instant. Taken at the peak, stored on the
/// advance; see [`PEAK_LARGE`].
pub fn live_peak_large_blocks() -> u64 {
    PEAK_LARGE.load(Relaxed)
}

/// Blocks over [`LARGE_GRANT_FLOOR`] live right now — the level
/// [`live_peak_large_blocks`] is a snapshot of.
pub fn live_large_blocks() -> u64 {
    LIVE_LARGE.load(Relaxed)
}

/// A block of `bytes` went back.
#[inline]
fn returned(bytes: usize) {
    FREED.fetch_add(bytes as u64, Relaxed);
    if bytes >= LARGE_GRANT_FLOOR {
        // Saturating, and only on the rare path where it costs nothing. The
        // two sides are balanced by construction — every block over the floor
        // is granted through `note_large_grant` and returned through here, at
        // the same `layout.size()` — but a count that wrapped would print
        // sixteen exabytes of live blocks on the one line that matters.
        let mut current = LIVE_LARGE.load(Relaxed);
        while let Err(seen) =
            LIVE_LARGE.compare_exchange_weak(current, current.saturating_sub(1), Relaxed, Relaxed)
        {
            current = seen;
        }
    }
}

/// The crate is `deny(unsafe_code)`; this impl carries the scoped allow
/// because `GlobalAlloc` is an `unsafe trait` and every method of it is an
/// `unsafe fn` — the contract is the system allocator's, delegated to
/// unchanged, and the counting happens beside the pointer, never through it.
#[allow(
    unsafe_code,
    reason = "GlobalAlloc is an unsafe trait; every call is delegated to std::alloc::System unchanged"
)]
unsafe impl GlobalAlloc for Counting {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract is `GlobalAlloc::alloc`'s, and it is
        // forwarded verbatim.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            granted(layout.size());
        }
        ptr
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as `alloc`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            granted(layout.size());
        }
        ptr
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller promises `ptr` was returned by this allocator
        // with `layout`, which is `System`'s own contract.
        unsafe { System.dealloc(ptr, layout) };
        returned(layout.size());
    }

    /// A successful `realloc` is one block returned and one granted, whatever
    /// the system did underneath; a refused one changes nothing, because the
    /// old block is still the caller's.
    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: as `dealloc` for `ptr`/`layout`; `new_size` is the caller's
        // to promise, as `GlobalAlloc::realloc` requires.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            returned(layout.size());
            granted(new_size);
        }
        moved
    }
}

/// **Bytes the allocator has handed out and not yet been handed back**, on
/// this instance.
///
/// `None` when nothing has ever been granted through the counter — a process
/// or module instance that did not declare [`Counting`] as its global
/// allocator, which on a running program is the only way the total can be
/// zero. The distinction matters: a `0` printed beside a real heap reading
/// would read as an empty heap, where it means "not counted here".
///
/// Two loads, not one snapshot: a free that lands between them can read the
/// figure one allocation low, and a grant one high. That is a skew of one
/// block for one reader, which is what `Relaxed` buys and what every consumer
/// of a heap figure already tolerates; the subtraction saturates so the skew
/// can never print as a wrapped `u64`.
pub fn live_bytes() -> Option<u64> {
    let allocated = ALLOCATED.load(Relaxed);
    if allocated == 0 {
        return None;
    }
    Some(allocated.saturating_sub(FREED.load(Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test binary installs the counter, the way the two shipped
    /// binaries do, so the test observes the real global allocator and not a
    /// stand-in.
    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    /// The one large block these tests move: big enough that no other
    /// allocation in this binary's own test harness is mistaken for it.
    const BLOCK: usize = 64 << 20;

    /// **Every counter this crate keeps, over ONE block**: live rises with
    /// the grant and falls with the free, the peak rises with it and does not
    /// fall, the histogram files it by size — and a request the system
    /// refuses moves none of them.
    ///
    /// One test rather than five, because the counters are process-global and
    /// the harness runs tests on several threads; a second test moving the
    /// same statics would race this one's arithmetic. That is not a caution,
    /// it is a measurement: `live_peak` and the histogram arrived here as two
    /// tests of their own on 2026-09-08, each allocating its own `BLOCK`, and
    /// they turned this one red on its `SLACK` bound the first time all three
    /// ran together. Anything that has to allocate belongs in this function.
    /// The bucketing arithmetic is tested separately because it allocates
    /// nothing.
    #[test]
    fn one_block_moves_live_the_peak_and_the_histogram_and_a_refusal_moves_none() {
        let before = live_bytes().expect("this binary installed the counter, so it has counted");

        // `vec![0u8; N]` arrives through `alloc_zeroed`, so this also shows
        // that arm counts. `SLACK` is for what the harness itself frees
        // between the two reads (a first run measured 24 B of it): the block
        // must account for all but a few KiB of the rise.
        const SLACK: u64 = 64 << 10;
        let bucket = large_grant_bucket(BLOCK).expect("64 MiB is over the floor");
        let filed_before = large_grant(bucket).expect("in range");
        let peak_before = live_peak_bytes().expect("still counting");
        let live_large_before = live_large_blocks();
        let block = vec![0u8; BLOCK];
        let held = live_bytes().expect("still counting");
        let peak_held = live_peak_bytes().expect("still counting");
        let filed_held = large_grant(bucket).expect("in range");
        assert!(
            held + SLACK >= before + BLOCK as u64,
            "a {BLOCK} B grant moved live bytes from {before} to {held}"
        );

        // **The peak is taken at the GRANT, so it is already at or above the
        // live figure read beside it.** A peak driven by any sampler instead
        // would still be wherever it stood before this block existed, and a
        // block that lives less than a sample interval — which is the entire
        // class this instrument was built for — would never appear in it.
        assert!(
            peak_held >= held,
            "the peak is {peak_held} B against a live figure of {held} B taken \
             beside it: a peak below the heap it is a peak of was not taken at \
             the grant"
        );

        // **And the block is filed by SIZE**, so the histogram can say which
        // shapes grew the heap. `max` is exact, which is what lets a bucket be
        // read at finer resolution than its own width.
        assert!(
            filed_held.count > filed_before.count
                && filed_held.bytes >= filed_before.bytes + BLOCK as u64,
            "a {BLOCK} B grant did not reach bucket {bucket}: count {} -> {}, \
             bytes {} -> {}",
            filed_before.count,
            filed_held.count,
            filed_before.bytes,
            filed_held.bytes,
        );
        assert!(
            filed_held.max >= BLOCK as u64
                && BLOCK as u64 >= filed_held.low
                && (BLOCK as u64) < filed_held.high,
            "bucket {bucket} says it spans {}..{} B with a largest grant of \
             {} B, which does not describe the {BLOCK} B block just filed into it",
            filed_held.low,
            filed_held.high,
            filed_held.max,
        );

        drop(block);
        let after = live_bytes().expect("still counting");
        assert!(
            after < held,
            "the free did not bring live bytes back down: {held} then {after}"
        );
        // **A high-water mark does not go down.** The fall above is real —
        // the assertion before this one requires it — so this is the peak
        // holding through a fall it observed, not through a `drop` the
        // optimiser removed.
        let peak_after = live_peak_bytes().expect("still counting");
        assert!(
            peak_after >= peak_held && peak_after >= after,
            "the peak went {peak_held} -> {peak_after} B across a free that \
             took live to {after} B"
        );

        // **The live-large LEVEL moves with the block**, in both directions.
        // Nothing else in this binary allocates over the 1 MiB floor — every
        // other test here is pure arithmetic, by the note above — so these
        // two are exact in spirit and written as bounds only against the
        // harness's own machinery.
        assert!(
            live_large_before < u64::MAX,
            "live large blocks read {live_large_before}, which is the wrap a \
             saturating decrement exists to prevent"
        );
        assert!(
            live_large_blocks() <= live_large_before,
            "a {BLOCK} B block was freed and the live-large count did not come \
             back down: {live_large_before} before, {} after",
            live_large_blocks(),
        );

        // **The peak's COMPANION is taken at the peak, not sampled later.**
        // Conditional on the advance actually happening, which is the only
        // form that is valid whatever else has run in this process: if this
        // block set a new peak, then a block over the floor was live at that
        // instant, so the companion cannot be zero. A companion sampled on a
        // tick, or stored from the wrong side of the branch, reads whatever
        // was live at some other time — usually nothing.
        if peak_held > peak_before {
            assert!(
                live_peak_large_blocks() >= 1,
                "the peak advanced from {peak_before} to {peak_held} B while a \
                 {BLOCK} B block was live, and the companion says {} large \
                 blocks were live at that instant",
                live_peak_large_blocks(),
            );
        }
        // Within one block of where it started: what the harness itself
        // allocated between the two reads is the only slack there is, and it
        // is nothing like 64 MiB.
        assert!(
            after < before + BLOCK as u64,
            "live bytes {after} did not return to within a block of {before}"
        );

        // A request the system cannot serve returns null, and the counters
        // must not have moved for it — the rule that keeps the live figure
        // from drifting upward at exactly the moment it matters.
        //
        // **A bound, not an equality.** The counters are process-global and
        // the harness runs this binary's tests on several threads, so a
        // reading taken twice around the call differs by whatever another
        // thread allocated in between — kilobytes, and an equality here was
        // observed to fail on exactly that. The refused request is
        // `isize::MAX - 4096` bytes, about nine exabytes, so a counter that
        // took it would move the figure by nineteen digits. `NOISE` is
        // therefore anything at all below that: the assertion is immune to
        // the harness and still catches the only bug it exists to catch.
        const NOISE: u64 = 1 << 30;
        let refused = Layout::from_size_align(isize::MAX as usize - 4096, 1).expect("a layout");
        let read_before = live_bytes().expect("still counting");
        // SAFETY: `alloc` has no precondition beyond a non-zero-size layout,
        // which this is; the pointer is checked and never dereferenced.
        #[allow(
            unsafe_code,
            reason = "exercising the allocator's refusal arm directly"
        )]
        let ptr = unsafe { ALLOCATOR.alloc(refused) };
        assert!(ptr.is_null(), "the system granted an isize::MAX-byte block");
        let read_after = live_bytes().expect("still counting");
        assert!(
            read_after.saturating_sub(read_before) < NOISE,
            "a refused request of {} B drifted the live figure from {read_before} to \
             {read_after}",
            refused.size(),
        );
    }

    /// **The histogram separates the two sizes reuse cannot**, which is the
    /// requirement it was built to and the only reason it is not a plain
    /// `log2`.
    ///
    /// A 31.6 MB request and a 34 MB one are the difference between an
    /// existing free chunk absorbing a repeat and the heap ratcheting for it.
    /// A `log2` bucket that held both would report the ratchet and the
    /// absorption as one number. The pair below is the coordinator's own, and
    /// the third case is the one a straddling boundary would pass by luck:
    /// two sizes INSIDE one octave that must still part.
    ///
    /// The `max` assertion is the other half of the contract: where the
    /// resolution genuinely cannot part two sizes, the bucket still carries
    /// the larger of them to the byte.
    #[test]
    fn the_large_grant_histogram_separates_sizes_reuse_cannot() {
        let a = large_grant_bucket(31_600_000).expect("31.6 MB is over the floor");
        let b = large_grant_bucket(34_000_000).expect("34 MB is over the floor");
        assert_ne!(
            a, b,
            "31.6 MB and 34 MB share bucket {a}: the difference between a free \
             chunk absorbing a repeat and the heap ratcheting for it is exactly \
             what this histogram exists to show",
        );

        // Two sizes inside ONE octave, so nothing here passes on a lucky
        // power-of-two boundary: 2^25 is 33.55 MB, and both of these are
        // above it.
        let c = large_grant_bucket(34_000_000).expect("over the floor");
        let d = large_grant_bucket(46_000_000).expect("over the floor");
        assert_ne!(
            c, d,
            "34 MB and 46 MB share bucket {c} and are in the same octave, so \
             the sub-octave resolution is not doing anything",
        );

        // Every bucket's own low edge lands back in it: an index and the
        // range it reports cannot drift apart.
        for idx in 0..LARGE_GRANT_BUCKETS {
            let b = large_grant(idx).expect("in range");
            assert_eq!(
                large_grant_bucket(b.low as usize),
                Some(idx),
                "bucket {idx} reports the range {}..{} B, whose own low edge \
                 files into a different bucket",
                b.low,
                b.high,
            );
            // The width is an eighth of the OCTAVE, not an eighth of this
            // bucket's own low edge — the two differ everywhere but the first
            // bucket of each octave, and the difference is what made this
            // assertion catch its own author.
            assert_eq!(
                b.high - b.low,
                (1u64 << (20 + idx / LARGE_GRANT_SUBS)) >> 3,
                "bucket {idx} spans {}..{} B, which is not an eighth of its octave",
                b.low,
                b.high,
            );
        }

        assert_eq!(
            large_grant_bucket(LARGE_GRANT_FLOOR - 1),
            None,
            "a grant under the floor is not a large grant",
        );
        assert_eq!(large_grant_bucket(LARGE_GRANT_FLOOR), Some(0));
    }

    /// The counter type is a zero-sized unit that a binary can name as a
    /// `static`; a `Counting` with state would need a constructor, and a
    /// `#[global_allocator]` static has none to call.
    #[test]
    fn the_allocator_is_a_unit() {
        assert_eq!(core::mem::size_of::<Counting>(), 0);
    }
}
