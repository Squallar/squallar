//! **One retained decode buffer of a fixed size, and the running count of what
//! it saved** — the shape every whole-grid source needs, over any element.
//!
//! [`crate::mrms::staging`] is the original and carries the full account of the
//! failure this design answers: wasm32 linear memory only grows, the browser
//! build is capped at 1 GiB, dlmalloc cannot coalesce across a live block, and
//! a loop that allocates and frees a fresh mosaic-sized block per granule
//! fragments that heap until a request fails with twice its size free.
//! Retaining the block removes the request rather than making room for it.
//!
//! This module is that pool with the two facts MRMS had spelled as constants —
//! the element type and the point count — made parameters, so a second source
//! with a different grid does not copy the type and the tests with it. MRMS
//! keeps its own for now; nothing here is a change to it.
//!
//! ## Nothing here may hand a grid the wrong bytes
//!
//! The invariants are the ones the MRMS pool checks rather than reasons about,
//! and they are narrow on purpose:
//!
//! * **capacity is matched exactly, never merely "big enough".**
//!   [`StagingPool::take`] answers the pooled buffer only when the grid it is
//!   about to hold has exactly the pool's point count, and
//!   [`StagingPool::give`] accepts one back only at exactly that capacity. A
//!   "≥" rule would hand a 400-byte test grid a 60 MB block whose `len` no
//!   longer describes its footprint, which is the figure every byte budget is
//!   spent against;
//! * **content is never inherited.** Both ends clear — `give` on the way in and
//!   `take` on the way out — and a decode `push`es exactly its point count into
//!   the empty buffer and refuses any other count.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// **One retained grid-sized buffer, and the running count of what it saved.**
///
/// A single slot, because the budget it implements is a single grid. A decode
/// that arrives while the slot is empty allocates its own and is free to hand
/// it back afterwards; nothing waits, and nothing is throttled here — the
/// one-at-a-time throttle is each handler's frame gate and stays there.
///
/// **Injectable rather than only global**, for the reason every cache budget in
/// this crate is injected: a suite that can only observe a process-wide slot
/// cannot tell "the pool worked" from "another test in this binary happened to
/// leave a buffer in it", and a filtered run in this workspace is explicitly
/// not self-contained. Each source's shipped path uses its own `global()`.
pub struct StagingPool<T> {
    /// `try_lock` only — see [`Self::take`].
    slot: Mutex<Option<Vec<T>>>,
    /// The one capacity this pool deals in, in elements.
    points: usize,
    /// Grid-sized buffers this pool had to allocate. **The figure the shipping
    /// defect is about**: one per granule without a pool, one per process, per
    /// live block, with it.
    allocated: AtomicUsize,
    /// Decodes that were handed a retained buffer instead.
    reused: AtomicUsize,
    /// Grids offered back whose buffer the slot could not take — the slot was
    /// full, the capacity did not match, or another reference still held the
    /// grid. Reported rather than hidden: a pool that silently never recycles
    /// reads exactly like one that does until the tab dies.
    declined: AtomicUsize,
    /// **Bytes the slot is holding right now** — a level, not a total.
    ///
    /// Maintained at the two transitions inside the slot's own critical
    /// section rather than read off the slot, because the reader that needs
    /// it is a frame-thread census and the slot is `try_lock`-only: a reader
    /// that missed the lock would have to report either a stale figure or a
    /// false zero, and a false zero on a 60 MB block is the exact shape of
    /// mistake the census exists to stop.
    retained: AtomicUsize,
}

/// Running totals off [`StagingPool`], in the order
/// `(allocated, reused, declined)`.
///
/// Always on, like `squallar_egui::overlay_cache::ledger` and `UploadTotals`:
/// three relaxed counters cost nothing beside a 60 MB decode, and a figure that
/// only exists under a `cfg` is a figure nobody reads when the tab dies in the
/// field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagingTotals {
    pub allocated: usize,
    pub reused: usize,
    pub declined: usize,
}

impl<T: Copy> StagingPool<T> {
    /// A pool dealing in buffers of exactly `points` elements.
    pub const fn new(points: usize) -> Self {
        Self {
            slot: Mutex::new(None),
            points,
            allocated: AtomicUsize::new(0),
            reused: AtomicUsize::new(0),
            declined: AtomicUsize::new(0),
            retained: AtomicUsize::new(0),
        }
    }

    /// The one capacity this pool deals in, in elements.
    pub const fn points(&self) -> usize {
        self.points
    }

    /// A buffer able to hold `points` values, retained from a previous granule
    /// when one is waiting and it is exactly the right size.
    ///
    /// **`try_lock`, never `lock`.** The critical section is a pointer move and
    /// the only contenders are a live fetch and a frame fetch, so contention is
    /// vanishingly rare — and on wasm32 with atomics, blocking the main thread
    /// is not something this path may ever do. A contended pool simply
    /// allocates, which is the behaviour every decode had before this existed.
    ///
    /// The fresh arm reserves fallibly: `with_capacity` calls
    /// `handle_alloc_error` on failure, and on a `panic-strategy = "abort"`
    /// target that leaves winit's event loop borrowed for the life of the page.
    pub fn take(&self, points: usize) -> Result<Vec<T>, String> {
        if points == self.points
            && let Ok(mut slot) = self.slot.try_lock()
            && let Some(mut buffer) = slot.take()
        {
            // The slot is empty from here whichever arm below runs, including
            // the mismatch that drops the buffer.
            self.retained.store(0, Ordering::Relaxed);
            // Belt and braces: `give` already refuses any other capacity, and
            // a buffer that somehow arrived at another one must not be handed
            // out as if it were grid-sized. Falling out of this block drops it
            // and allocates fresh below, exactly as a contended slot does.
            if buffer.capacity() == self.points {
                self.reused.fetch_add(1, Ordering::Relaxed);
                // Both ends, deliberately. `give` clears on the way in, and a
                // buffer is cleared again on the way out, so "the decode
                // starts from empty" does not depend on any one caller having
                // done the right thing. For a `Copy` element this is a store
                // to `len` and nothing else.
                buffer.clear();
                return Ok(buffer);
            }
        }
        let mut fresh: Vec<T> = Vec::new();
        fresh.try_reserve_exact(points).map_err(|_| {
            format!(
                "cannot hold a {} MB staging grid in this build's memory",
                points.saturating_mul(size_of::<T>()) / (1024 * 1024),
            )
        })?;
        self.allocated.fetch_add(1, Ordering::Relaxed);
        Ok(fresh)
    }

    /// Offer a spent buffer back.
    ///
    /// Refused — and counted as refused — unless the slot is empty and the
    /// capacity is exactly [`Self::points`]. A refused buffer is dropped here,
    /// which is what every buffer did before this module existed.
    pub fn give(&self, mut values: Vec<T>) {
        if values.capacity() != self.points {
            self.declined.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Before the lock: the slot must never hold a buffer whose length is
        // anything but zero, whichever arm below runs.
        values.clear();
        let Ok(mut slot) = self.slot.try_lock() else {
            self.declined.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if slot.is_some() {
            self.declined.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.retained
            .store(values.capacity() * size_of::<T>(), Ordering::Relaxed);
        *slot = Some(values);
    }

    /// Count one offer that never reached [`Self::give`] because something
    /// else still held the grid — a source's `recycle` reports its
    /// `Arc::into_inner` miss through here so the totals stay one ledger.
    pub fn decline(&self) {
        self.declined.fetch_add(1, Ordering::Relaxed);
    }

    /// **What the slot is holding**, in bytes: one grid while a buffer is
    /// parked, zero while it is out with a decode.
    ///
    /// A level off the `retained` field, so this is one relaxed load and takes
    /// no lock — safe to read on the frame thread and safe to read from an
    /// allocation-error hook. It counts the retained buffer's **capacity**,
    /// which is what the allocator is holding; the buffer is always empty
    /// while it is in the slot, so its length would read zero and say nothing.
    pub fn retained_bytes(&self) -> usize {
        self.retained.load(Ordering::Relaxed)
    }

    pub fn totals(&self) -> StagingTotals {
        StagingTotals {
            allocated: self.allocated.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
            declined: self.declined.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests;
