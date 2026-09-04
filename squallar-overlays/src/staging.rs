//! **One retained decode buffer at the shape the product publishes, and the
//! running count of what it saved** — the shape every whole-grid source needs,
//! over any element.
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
//! ## The one capacity is discovered, never declared
//!
//! **A compile-time point count is a guess about a product, and a wrong guess
//! is silent.** This pool was declared at [`crate::gmgsi::GRID_POINTS`] =
//! `3000 * 5000`, and every GMGSI granule dated 2026-09-03 is `[1, 3000, 4999]`
//! on all four channels — the product's grid width moved. At 14,997,000 points
//! a pool keyed on 15,000,000 reuses nothing and accepts nothing back: `take`
//! matched the request against the constant and `give` matched the offered
//! capacity against it, so every decode allocated a fresh 60 MB block and every
//! one was freed again — precisely the churn this module exists to remove, with
//! the module in place. Nothing failed and nothing was slow. The committed
//! fixture is 5000 wide, so the suites went on proving the pool worked at a
//! shape the product had stopped publishing.
//!
//! So the retained buffer carries **its own** point count, and the reuse key is
//! that rather than a constant: a granule of a shape the slot is not holding
//! drops the retained buffer and becomes the shape the slot holds, counted by
//! [`StagingPool::resizes`]. The declared figure survives as
//! [`StagingPool::nominal_points`] — what the byte budgets around this pool were
//! sized for, and the reference a [`StagingPool::retained_points`] reading is
//! compared against the next time a product moves.
//!
//! ## Nothing here may hand a grid the wrong bytes
//!
//! The invariants are the ones the MRMS pool checks rather than reasons about,
//! and they are narrow on purpose:
//!
//! * **capacity is matched exactly, never merely "big enough".**
//!   [`StagingPool::take`] answers the pooled buffer only when the grid it is
//!   about to hold has exactly the retained buffer's point count. A "≥" rule
//!   would hand a 400-byte test grid a 60 MB block whose `len` no longer
//!   describes its footprint, which is the figure every byte budget is spent
//!   against. What the shape key changed is *which* count that is, never that
//!   it is exact;
//! * **content is never inherited.** Both ends clear — `give` on the way in and
//!   `take` on the way out — and a decode `push`es exactly its point count into
//!   the empty buffer and refuses any other count;
//! * **a pool that is not doing its job says so.** The three totals were always
//!   here and nothing ever read them, which is how a wholly inert pool shipped
//!   and held: `reused: 0, declined: N` reads at a glance exactly like a pool
//!   nobody has exercised yet. [`StagingPool::health`] is that reading as a
//!   verdict, so the difference is one value a test or an operator surface can
//!   assert on.

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
    /// **What the product was believed to publish when this pool was declared**
    /// — a budget figure and a reference point, and **not** the reuse key.
    ///
    /// The byte budgets around a pool (`GLOBAL_GRID_BYTES`, `GRID_CACHE_BYTES`,
    /// `FRAME_STAGING_BYTES`) are all derived from the same constant, so this
    /// is what the memory the layer may hold was sized against. Which buffer
    /// `take` hands out is decided by [`Self::retained_points`] alone; the two
    /// differing is the product having moved, which is a fact worth reading
    /// rather than an error.
    nominal_points: usize,
    /// Grid-sized buffers this pool had to allocate. **The figure the shipping
    /// defect is about**: one per granule without a pool, one per process, per
    /// live block, with it.
    allocated: AtomicUsize,
    /// Decodes that were handed a retained buffer instead.
    reused: AtomicUsize,
    /// Grids offered back whose buffer the slot could not take — the slot was
    /// full, the buffer owned no allocation, or another reference still held
    /// the grid. Reported rather than hidden: a pool that silently never
    /// recycles reads exactly like one that does until the tab dies.
    declined: AtomicUsize,
    /// **Decodes that arrived at a shape the slot was not holding**, so the
    /// retained buffer was dropped and the arriving shape became the retained
    /// one.
    ///
    /// Counted rather than absorbed, because it is the one event that tells an
    /// operator the product's grid moved under a build. Zero in the steady
    /// state; one per product change is the design working; a figure that
    /// climbs with `allocated` is two callers alternating shapes over one slot,
    /// which no shipped path does and which a second slot — not a bigger one —
    /// would be the fix for.
    resized: AtomicUsize,
    /// **Points the slot is holding right now** — a level, not a total, and the
    /// shape [`Self::take`] matches a request against.
    ///
    /// Maintained at the two transitions inside the slot's own critical
    /// section rather than read off the slot, because the reader that needs
    /// it is a frame-thread census and the slot is `try_lock`-only: a reader
    /// that missed the lock would have to report either a stale figure or a
    /// false zero, and a false zero on a 60 MB block is the exact shape of
    /// mistake the census exists to stop.
    retained_points: AtomicUsize,
}

/// Running totals off [`StagingPool`], in the order
/// `(allocated, reused, declined)`.
///
/// Always on, like `squallar_egui::overlay_cache::ledger` and `UploadTotals`:
/// relaxed counters cost nothing beside a 60 MB decode, and a figure that only
/// exists under a `cfg` is a figure nobody reads when the tab dies in the field.
///
/// [`StagingPool::resizes`] is deliberately **not** a fourth field here: this
/// struct is built and compared as a whole by suites outside this module, and a
/// figure worth adding is not worth a churn of unrelated assertions to add it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagingTotals {
    pub allocated: usize,
    pub reused: usize,
    pub declined: usize,
}

/// **Whether the pool is doing the one thing it exists to do**, as a verdict
/// rather than three numbers a reader has to interpret.
///
/// The counters could always answer this and nothing ever asked them, which is
/// how a pool that reused nothing at all shipped and held: `reused: 0` is the
/// reading of a healthy cold pool *and* of a permanently inert one, and only
/// the company it keeps separates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagingHealth {
    /// No decode has asked this pool for a buffer yet. Says nothing either way.
    Cold,
    /// At least one decode was handed the retained buffer, so the pool is
    /// removing allocations.
    Reusing,
    /// Decodes have run and **not one** of them was handed the retained buffer.
    ///
    /// **This is the shipping defect's own reading.** Every decode allocated and
    /// every offer was refused, at full speed, with no error anywhere — the pool
    /// present, wired, counted, and removing nothing.
    Inert,
}

impl<T: Copy> StagingPool<T> {
    /// A pool whose surrounding byte budgets were sized for `points` elements.
    ///
    /// `points` is the *nominal* shape — see [`Self::nominal_points`]. The
    /// buffer this pool actually retains takes its capacity from the granule
    /// that hands one back, whatever shape that is.
    pub const fn new(points: usize) -> Self {
        Self {
            slot: Mutex::new(None),
            nominal_points: points,
            allocated: AtomicUsize::new(0),
            reused: AtomicUsize::new(0),
            declined: AtomicUsize::new(0),
            resized: AtomicUsize::new(0),
            retained_points: AtomicUsize::new(0),
        }
    }

    /// **What the product was believed to publish when this pool was declared**,
    /// in elements — the figure the surrounding byte budgets were sized for.
    ///
    /// Not the reuse key: [`Self::retained_points`] is. The two differing means
    /// the product's grid has moved off what this build was sized for, which on
    /// the change that prompted this costs 12,000 B against a 60,000,000 B
    /// budget and is worth a look rather than an alarm.
    pub const fn nominal_points(&self) -> usize {
        self.nominal_points
    }

    /// **The shape the slot is holding**, in elements — zero while nothing is
    /// parked. This is the count [`Self::take`] matches a request against.
    pub fn retained_points(&self) -> usize {
        self.retained_points.load(Ordering::Relaxed)
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
    /// **A request at another shape empties the slot.** The retained buffer is
    /// then for a shape the product is no longer publishing, and holding it
    /// would keep the slot permanently full, refuse every offer at the new
    /// shape, and leave the pool inert for the life of the process — which is
    /// the defect this module was found to have. It is dropped here, before the
    /// reserve below and not after, so an allocator that cannot coalesce can
    /// serve the new request out of the block just freed; the two are within
    /// 12,000 B of each other on the change that prompted this.
    ///
    /// The fresh arm reserves fallibly: `with_capacity` calls
    /// `handle_alloc_error` on failure, and on a `panic-strategy = "abort"`
    /// target that leaves winit's event loop borrowed for the life of the page.
    pub fn take(&self, points: usize) -> Result<Vec<T>, String> {
        {
            // Scoped, so the guard and any dropped buffer are both released
            // before the reserve below asks for a block.
            if let Ok(mut slot) = self.slot.try_lock()
                && let Some(mut buffer) = slot.take()
            {
                // The slot is empty from here whichever arm below runs,
                // including the mismatch that drops the buffer.
                self.retained_points.store(0, Ordering::Relaxed);
                // Exactly, never "big enough": a grid handed a longer block
                // would report `len` bytes while holding the whole slot, and
                // `len` is the figure every byte budget is spent against.
                if buffer.capacity() == points {
                    self.reused.fetch_add(1, Ordering::Relaxed);
                    // Both ends, deliberately. `give` clears on the way in, and
                    // a buffer is cleared again on the way out, so "the decode
                    // starts from empty" does not depend on any one caller
                    // having done the right thing. For a `Copy` element this is
                    // a store to `len` and nothing else.
                    buffer.clear();
                    return Ok(buffer);
                }
                self.resized.fetch_add(1, Ordering::Relaxed);
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
    /// **The offered buffer's own capacity becomes the shape the slot holds** —
    /// a pool is sized by the product feeding it, not by a constant the build
    /// was written with. Refused — and counted as refused — when the slot is
    /// already full, or when the buffer owns no allocation to retain. A refused
    /// buffer is dropped here, which is what every buffer did before this
    /// module existed.
    pub fn give(&self, mut values: Vec<T>) {
        // A zero-capacity `Vec` owns nothing, so retaining it would park no
        // block at all while making the next real offer read as a full slot and
        // the next decode read as a resize.
        if values.capacity() == 0 {
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
        self.retained_points
            .store(values.capacity(), Ordering::Relaxed);
        *slot = Some(values);
    }

    /// **Drop the retained buffer**, answering whether there was one to drop.
    ///
    /// The pool's one lever, and it is deliberately shaped for **two callers
    /// with no knowledge of each other**: an idle policy in the layer that owns
    /// the source, and a memory governor's tier-2 pressure step. Both want the
    /// same thing — the grid-sized block the slot is holding while nothing is
    /// decoding handed back to the allocator — and neither has to know the
    /// other exists or to have run first. Two sources retain a grid apiece
    /// (GMGSI ~60 MB, MRMS 49 MB), so this is ~109 MB resident whether or not
    /// anything is decoding.
    ///
    /// `false` means nothing was released: the slot was empty, or —
    /// vanishingly rarely — a decode held the lock. **`try_lock`, never
    /// `lock`**, for the reason [`Self::take`] gives: on wasm32 with atomics
    /// this path may not block the main thread. A caller that must have the
    /// block back calls again; it is not a failure to report.
    ///
    /// **This is one `free` of one block** — the same free every declined offer
    /// already performs — so it is not work in the sense the frame thread cares
    /// about. The cost is entirely on the other side: the next decode allocates
    /// a grid again, which is the allocation this module exists to remove. That
    /// is why a *clock* is a poor trigger and *pressure* is a good one — under
    /// pressure the block is worth more free than parked, whereas a short idle
    /// threshold re-introduces exactly one mosaic-sized allocate-and-free per
    /// poll, on a heap that cannot coalesce, for a layer that is still on.
    pub fn release_retained(&self) -> bool {
        let Ok(mut slot) = self.slot.try_lock() else {
            return false;
        };
        match slot.take() {
            Some(buffer) => {
                self.retained_points.store(0, Ordering::Relaxed);
                drop(buffer);
                true
            }
            None => false,
        }
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
    /// Derived from [`Self::retained_points`] rather than stored beside it, so
    /// the two cannot disagree. One relaxed load and a multiply, taking no lock
    /// — safe to read on the frame thread and safe to read from an
    /// allocation-error hook. It counts the retained buffer's **capacity**,
    /// which is what the allocator is holding; the buffer is always empty
    /// while it is in the slot, so its length would read zero and say nothing.
    pub fn retained_bytes(&self) -> usize {
        self.retained_points().saturating_mul(size_of::<T>())
    }

    pub fn totals(&self) -> StagingTotals {
        StagingTotals {
            allocated: self.allocated.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
            declined: self.declined.load(Ordering::Relaxed),
        }
    }

    /// **Decodes that arrived at a shape the slot was not holding.** See the
    /// field: zero in the steady state, one per product change by design.
    pub fn resizes(&self) -> usize {
        self.resized.load(Ordering::Relaxed)
    }

    /// **Whether this pool is removing allocations, in one value.** See
    /// [`StagingHealth`] for why three counters were not enough on their own.
    pub fn health(&self) -> StagingHealth {
        let totals = self.totals();
        if totals.allocated == 0 && totals.reused == 0 {
            StagingHealth::Cold
        } else if totals.reused > 0 {
            StagingHealth::Reusing
        } else {
            StagingHealth::Inert
        }
    }
}

#[cfg(test)]
mod tests;
