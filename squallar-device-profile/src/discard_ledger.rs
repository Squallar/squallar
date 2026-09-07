//! **Bytes handed over to be freed and not freed yet**, on whichever of the
//! two routes this target uses.
//!
//! `squallar_worker::offload::discard` hands a payload away from the frame
//! that stopped needing it, and the bytes stay resident until something else
//! finishes the drop. There are two such holders and a payload is in exactly
//! one of them:
//!
//! * the **free lane** — native's route, the job pool's one-thread `rd-free`
//!   queue. The frame thread's handover is a channel send; the bytes live on
//!   until that lane reaches them.
//! * the **deferred queue** — wasm's route (and native's only when the free
//!   lane has no worker left), a thread-local queue drained against
//!   [`constants::DEFERRED_DROP_BUDGET_PER_FRAME`] on the frame thread.
//!
//! The counters live here, rather than beside either queue, for the reason
//! [`crate::hist`] does: the filler and the reader are on opposite sides of a
//! crate boundary — `squallar-worker` fills, `squallar-egui`'s heap census
//! reads — and this is the one crate both already stand on without a cycle.
//! The census reads [`in_flight_bytes`] **through**, at the instant of the
//! read, so the 2 s telemetry tick and the allocation-error hook see the same
//! truth; a figure published on the tick would miss a burst almost every time
//! it happened (below).
//!
//! # What these figures are worth
//!
//! A **floor**, and zero exactly when both holders are empty. A caller that
//! knows what its payload holds says so with an `offload::Priced` — an evicted
//! volume's sweeps arrive priced at their gate bytes — and anything filed
//! unpriced counts its own struct size and nothing behind it.
//!
//! # Why the window is short and the bytes are not
//!
//! Probe-measured on the Linux desktop arm, freeing volume-shaped payloads
//! allocated on one thread and dropped on another as the lane does (10 sweeps
//! of 720 radials x 6 moments x 1832 gates, ~43k allocations a volume):
//! the frame thread's handover of a full 8-volume burst costs 4–7 µs, and the
//! lane then takes **27.9 ms** to free 604 MiB, or 25.3 ms to free 391 MiB.
//!
//! **The drain tracks allocation count, not bytes** — the same ~43k `free()`
//! calls whether the volume is median or maximum shape — so a reader sizing
//! this against megabytes will get the wrong answer. What varies the window is
//! how many buffers were handed over, not how fat they were.
//!
//! The 604 MiB is the desktop resident cap's arithmetic (8 volumes at the
//! 74.63 MiB documented maximum), exercised by that probe; it is not an
//! observed peak in the running app.
//!
//! The counting itself is free at that scale: the same probe with these
//! counters wired in handed the burst over in 4-6 µs against 4-7 µs without
//! them, which is inside the unchanged spread. One relaxed add per discard on
//! the frame thread, one compare-and-swap per free on a thread that is already
//! spending hundreds of microseconds in the allocator.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

thread_local! {
    /// The deferred queue's entries at the prices they were filed at. Kept
    /// beside the queue's own thread because that queue is thread-local:
    /// every producer of an entry is the thread that consumes it.
    static QUEUED: Cell<u64> = const { Cell::new(0) };
}

/// The free lane's outstanding payloads. An atomic rather than a cell because
/// this one **is** shared: the frame thread files and the `rd-free` thread
/// retires. `Relaxed` throughout — every reader wants a recent figure, none
/// wants a synchronised one.
static ON_LANE: AtomicU64 = AtomicU64::new(0);

/// Price `bytes` into the deferred queue's total, as it is filed.
pub fn queue_filed(bytes: u64) {
    QUEUED.with(|total| total.set(total.get().saturating_add(bytes)));
}

/// Take `bytes` back out of the deferred queue's total, as its payload is
/// dropped. Called **after** the drop, so the total reads "filed and not yet
/// freed" to the byte.
pub fn queue_freed(bytes: u64) {
    QUEUED.with(|total| total.set(total.get().saturating_sub(bytes)));
}

/// What this thread's deferred queue is holding.
pub fn queued_bytes() -> u64 {
    QUEUED.with(Cell::get)
}

/// Price `bytes` onto the free lane, as the payload is handed to it.
pub fn lane_filed(bytes: u64) {
    ON_LANE.fetch_add(bytes, Relaxed);
}

/// Take `bytes` back off the free lane, as its payload is dropped.
pub fn lane_freed(bytes: u64) {
    // `fetch_sub` would wrap past zero; a lost decrement must read as a small
    // overcount, never as `u64::MAX` bytes in flight.
    let _ = ON_LANE.fetch_update(Relaxed, Relaxed, |held| Some(held.saturating_sub(bytes)));
}

/// What the free lane is holding.
pub fn lane_bytes() -> u64 {
    ON_LANE.load(Relaxed)
}

/// **Bytes discarded and not yet freed, by either route.**
///
/// The two holders are disjoint by construction — `offload::discard` files a
/// payload in exactly one — so this is their sum and not an upper bound. One
/// of the two is structurally zero on any given target: the lane never runs on
/// wasm, and the queue takes a payload natively only when the lane has no
/// worker left.
pub fn in_flight_bytes() -> u64 {
    queued_bytes().saturating_add(lane_bytes())
}

#[cfg(test)]
mod tests;
