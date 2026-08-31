//! **The one staging buffer a mosaic is decoded into, retained between
//! granules.**
//!
//! [`FRAME_STAGING_BYTES`](super::FRAME_STAGING_BYTES) has always declared the
//! policy — *one grid stages at a time, on every arm* — and the handler's frame
//! gate has always enforced the concurrency half of it. What the policy did
//! **not** have was the allocation half: every granule built a **fresh**
//! 98,000,000 B `Vec<f32>` and freed the last one, so "one staging area" meant
//! one *slot* and N *allocations*. This module makes the buffer itself the
//! staging area, so the slot and the allocation are the same object.
//!
//! ## Why that is the shipping fix and a bigger budget is not
//!
//! wasm32 linear memory only ever **grows**, and the browser build is capped at
//! 1 GiB (`--max-memory=1073741824`, `.github/scripts/wasm-threads.sh`). A loop
//! playing over the mosaic put ~147 MB of large-block churn on that heap per
//! granule — the 98 MB values vector plus grib's 49 MB PNG image buffer,
//! allocated and freed in an interleaved order — and dlmalloc cannot coalesce
//! across a live block. Measured 2026-08-31: a pane with the layer on and a
//! loop playing, nobody touching the page, failed the 98 MB request at ~122 s
//! on Firefox 154 and Chromium 151 alike, **0.3 s apart**, because dlmalloc is
//! compiled into the module and both engines run the identical allocator over
//! the identical request sequence. The pool was 192 MB and free. The request
//! failed for want of a *contiguous* 98 MB, not for want of 98 MB.
//!
//! Retaining the block removes the request rather than making room for it. Two
//! 98 MB blocks end up permanently live in the steady state — the one a cache
//! holds and the one waiting in the slot — and **neither is ever freed**, so
//! the only large block still cycling is grib's PNG buffer, which is the same
//! size every granule and therefore lands back in its own hole.
//!
//! Widening the *fallible* reserve across more of the decode was considered and
//! refused: fallibility converts a hard failure into constant degradation, a
//! ratchet where layers quietly stop drawing. The fallible reserve in
//! [`parse_grib2_raw`](super::decode::parse_grib2_raw) stays as the net that
//! keeps the page alive; it is not the cure.
//!
//! ## Nothing here may hand a grid the wrong bytes
//!
//! A retained buffer that outlives the product it was filled for is a
//! data-corruption bug wearing a performance fix's clothes, so the invariants
//! are narrow and checked rather than reasoned about:
//!
//! * **capacity is matched exactly, never merely "big enough".**
//!   [`StagingPool::take`] answers the pooled buffer only when the grid it is
//!   about to hold has exactly [`STAGING_POINTS`] points, and
//!   [`StagingPool::give`] accepts one back only at exactly that capacity. A
//!   "≥" rule would hand a 400-byte test grid a 98 MB block whose `len` no
//!   longer describes its footprint, which is the figure both byte budgets are
//!   spent against
//!   ([`MrmsGrid::resident_bytes`](super::MrmsGrid::resident_bytes));
//! * **content is never inherited.** Both ends clear — [`StagingPool::give`] on
//!   the way in and [`StagingPool::take`] on the way out — and the decode
//!   `push`es exactly `ni * nj` values into the empty buffer and refuses any
//!   other count. `tests/mrms_staging_identity.rs` poisons a mosaic block with
//!   `0xDEADBEEF`, feeds it back through the pool and decodes both shipped
//!   products through it; it is the check that fires if a `set_len` shortcut
//!   ever lands (measured: half a mosaic of poison reaches the grid and the
//!   summary moves).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// How many `f32` one staged mosaic holds —
/// [`FRAME_STAGING_BYTES`](super::FRAME_STAGING_BYTES) in points.
///
/// Derived from the budget rather than restated, so a product whose grid
/// changes shape moves both together or neither.
pub const STAGING_POINTS: usize = super::FRAME_STAGING_BYTES / size_of::<f32>();

/// **One retained mosaic-sized buffer, and the running count of what it saved.**
///
/// A single slot, because the budget is a single grid. A decode that arrives
/// while the slot is empty allocates its own and is free to hand it back
/// afterwards; nothing waits, and nothing is throttled here — the
/// one-at-a-time throttle is the handler's frame gate and stays there.
///
/// **Injectable rather than only global** for the reason `MrmsGridCache`'s own
/// budget is injected: a suite that can only observe the process-wide slot cannot tell
/// "the pool worked" from "another test in this binary happened to leave a
/// buffer in it", and a filtered run in this workspace is explicitly not
/// self-contained. The shipped path uses [`global`].
pub struct StagingPool {
    /// `try_lock` only — see [`Self::take`].
    slot: Mutex<Option<Vec<f32>>>,
    /// Mosaic-sized buffers this pool had to allocate. **The figure the
    /// shipping defect is about**: it was one per granule and is now one per
    /// process, per live block.
    allocated: AtomicUsize,
    /// Decodes that were handed a retained buffer instead.
    reused: AtomicUsize,
    /// Grids offered back whose buffer the slot could not take — the slot was
    /// full, the capacity did not match, or another `Arc` was still holding
    /// the grid. Reported rather than hidden: a pool that silently never
    /// recycles reads exactly like one that does until the tab dies.
    declined: AtomicUsize,
}

/// Running totals off [`StagingPool`], in the order
/// `(allocated, reused, declined)`.
///
/// Always on, like `squallar_egui::overlay_cache::ledger` and `UploadTotals`:
/// three relaxed counters cost nothing beside a 98 MB decode, and a figure that
/// only exists under a `cfg` is a figure nobody reads when the tab dies in the
/// field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagingTotals {
    pub allocated: usize,
    pub reused: usize,
    pub declined: usize,
}

impl StagingPool {
    pub const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            allocated: AtomicUsize::new(0),
            reused: AtomicUsize::new(0),
            declined: AtomicUsize::new(0),
        }
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
    /// The fresh arm keeps the `try_reserve_exact` the safety net landed:
    /// `with_capacity` here calls `handle_alloc_error` on failure, and on a
    /// `panic-strategy = "abort"` target that leaves winit's event loop
    /// borrowed for the life of the page.
    pub fn take(&self, points: usize) -> Result<Vec<f32>, String> {
        if points == STAGING_POINTS
            && let Ok(mut slot) = self.slot.try_lock()
            && let Some(mut buffer) = slot.take()
            // Belt and braces: `give` already refuses any other capacity, and
            // a buffer that somehow arrived at another one must not be handed
            // out as if it were mosaic-sized.
            && buffer.capacity() == STAGING_POINTS
        {
            self.reused.fetch_add(1, Ordering::Relaxed);
            // Both ends, deliberately. `give` clears on the way in, and a
            // buffer is cleared again on the way out, so the "the decode
            // starts from empty" invariant does not depend on any one caller
            // having done the right thing. For an `f32` this is a store to
            // `len` and nothing else.
            buffer.clear();
            return Ok(buffer);
        }
        let mut fresh: Vec<f32> = Vec::new();
        fresh.try_reserve_exact(points).map_err(|_| {
            format!(
                "MRMS: cannot hold a {} MB staging grid in this build's memory",
                points.saturating_mul(size_of::<f32>()) / (1024 * 1024),
            )
        })?;
        self.allocated.fetch_add(1, Ordering::Relaxed);
        Ok(fresh)
    }

    /// Offer a spent mosaic buffer back.
    ///
    /// Refused — and counted as refused — unless the slot is empty and the
    /// capacity is exactly [`STAGING_POINTS`]. A refused buffer is dropped
    /// here, which is what every buffer did before this module existed.
    pub fn give(&self, mut values: Vec<f32>) {
        if values.capacity() != STAGING_POINTS {
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
        *slot = Some(values);
    }

    /// Take a [`MrmsGrid`](super::MrmsGrid)'s values back into the pool, if
    /// this is the last reference to them.
    ///
    /// `Arc::into_inner` rather than a clone-and-drop: a grid whose raster job
    /// is still in flight is genuinely still in use, and prising the values out
    /// from under it would be a use-after-free by another name. That case is
    /// counted as `declined` and the grid drops normally.
    pub fn recycle(&self, grid: super::MrmsGrid) {
        match std::sync::Arc::into_inner(grid.grid) {
            Some(resident) => self.give(resident.values),
            None => {
                self.declined.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// [`Self::recycle`] for the live cache's shared carry.
    pub fn recycle_shared(&self, grid: std::sync::Arc<super::MrmsGrid>) {
        match std::sync::Arc::into_inner(grid) {
            Some(grid) => self.recycle(grid),
            None => {
                self.declined.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn totals(&self) -> StagingTotals {
        StagingTotals {
            allocated: self.allocated.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
            declined: self.declined.load(Ordering::Relaxed),
        }
    }
}

impl Default for StagingPool {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide staging area — what every shipped decode uses.
///
/// One slot for the whole application, not one per handler or one per thread. A
/// thread-local would be 98 MB per worker thread on native for a path that runs
/// one decode at a time by design, and the live fetch and the loop's frame
/// fetch are exactly the two callers that must share the one slot the budget
/// names.
static GLOBAL: StagingPool = StagingPool::new();

/// See [`GLOBAL`].
pub fn global() -> &'static StagingPool {
    &GLOBAL
}

#[cfg(test)]
mod tests;
