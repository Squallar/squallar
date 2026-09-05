//! **The one staging buffer a mosaic is decoded into, retained between
//! granules.**
//!
//! [`FRAME_STAGING_BYTES`](super::FRAME_STAGING_BYTES) has always declared the
//! policy — *one grid stages at a time, on every arm* — and the handler's frame
//! gate has always enforced the concurrency half of it. What the policy did
//! **not** have was the allocation half: every granule built a **fresh**
//! 49,000,000 B `Vec<u16>` and freed the last one, so "one staging area" meant
//! one *slot* and N *allocations*. This module makes the buffer itself the
//! staging area, so the slot and the allocation are the same object.
//!
//! ## Why that is the shipping fix and a bigger budget is not
//!
//! wasm32 linear memory only ever **grows**, and the browser build is capped at
//! 1 GiB (`--max-memory=1073741824`, `.github/scripts/wasm-threads.sh`). A loop
//! playing over the mosaic put ~147 MB of large-block churn on that heap per
//! granule — the values vector, 98 MB at the `f32` width the store then
//! had, plus grib's 49 MB PNG image buffer,
//! allocated and freed in an interleaved order — and dlmalloc cannot coalesce
//! across a live block. Measured 2026-08-31: a pane with the layer on and a
//! loop playing, nobody touching the page, failed the 98 MB request at ~122 s
//! on Firefox 154 and Chromium 151 alike, **0.3 s apart**, because dlmalloc is
//! compiled into the module and both engines run the identical allocator over
//! the identical request sequence. The pool was 192 MB and free. The request
//! failed for want of a *contiguous* 98 MB, not for want of 98 MB.
//!
//! Retaining the block removes the request rather than making room for it. Two
//! mosaic blocks — 49 MB each at the `u16` store — end up permanently live in
//! the steady state — the one a cache holds and the one waiting in the slot —
//! and **neither is ever freed**.
//!
//! That left grib's 49 MB PNG image buffer as the only large block still
//! cycling. It has since stopped cycling too:
//! [`decode_png_into`](super::decode) streams section 7 a row at a time instead
//! of taking grib's whole-image `vec![0; n]`, so a warm decode's peak is
//! **0.43 MB, measured**, with no block over 1 MiB in it
//! (`tests/mrms_decode_image_buffer.rs`). The pool is still the fix for the
//! values-vector half; nothing here changes.
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
//!   "≥" rule would hand a 400-byte test grid a 49 MB block whose `len` no
//!   longer describes its footprint, which is the figure both byte budgets are
//!   spent against
//!   ([`MrmsGrid::resident_bytes`](super::MrmsGrid::resident_bytes));
//! * **content is never inherited.** Both ends clear — [`StagingPool::give`] on
//!   the way in and [`StagingPool::take`] on the way out — and the decode
//!   `push`es exactly `ni * nj` values into the empty buffer and refuses any
//!   other count. `tests/mrms_staging_identity.rs` poisons a mosaic block with
//!   a code neither shipped granule carries, feeds it back through the pool
//!   and decodes both shipped
//!   products through it; it is the check that fires if a `set_len` shortcut
//!   ever lands (measured: half a mosaic of poison reaches the grid and the
//!   summary moves).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// How many values one staged mosaic holds —
/// [`FRAME_STAGING_BYTES`](super::FRAME_STAGING_BYTES) in points.
///
/// Derived from the budget rather than restated, so a product whose grid
/// changes shape moves both together or neither.
///
/// # What a wrong divisor costs, and what now prevents one
///
/// The divisor is no longer written here to be kept in step by hand. It is
/// [`StagingPool::ELEMENT_BYTES`], whose `StagedCode` **is** the store's own
/// element, so the instruction this heading used to carry — "if you change what
/// [`StagingPool`] holds, change this divisor in the same edit" — is discharged
/// by the spelling instead of asked of the reader. It said `size_of::<f32>()`
/// while the slot was a `Vec<f32>`, which is the edit that has to stop being
/// possible rather than be remembered.
///
/// **The cost, if one ever were wrong.** It sizes the slot at half a mosaic (or
/// twice one), and [`StagingPool::give`] matches capacity **exactly**, never
/// "big enough" — so every buffer offered back is refused, the slot stays empty
/// for ever, and every decode allocates its own mosaic (49 MB at the narrow
/// store; it was 98 MB at the wide one). That is precisely the defect this
/// module exists to prevent, reintroduced through a constant nobody would think
/// to look at, and the only symptom is the `declined` counter climbing where
/// `reused` used to. On wasm32 it ends the way it ended before: a heap that only
/// grows, fragmented past a contiguous mosaic-sized request, and a page that
/// freezes with every screenshot and rAF check reporting it healthy.
///
/// **It would not, however, be silent, and this doc used to say it would.**
/// Measured on this tree: the divisor alone moved back to `size_of::<f32>()`
/// fails the build on the pin below — `assertion failed: STAGING_POINTS ==
/// 24_500_000`. The pin was doing its job and the prose beside it overstated
/// the exposure, which is its own defect — it reads as a reason to go carefully
/// where the build already refuses. What the literal genuinely could not do is
/// **re-derive**, which is what changed here.
///
/// The assertion below is the guard, because prose is not a gate: the slot
/// holds one CONUS mosaic and that is a **point** count, so a divisor error
/// moves it and fails the build.
///
/// # The divisor names the slot rather than restating its type
///
/// It is [`StagingPool::ELEMENT_BYTES`], whose `StagedCode` **is** the store's
/// own element — [`recycle`](StagingPool::recycle) moves a decoded grid's
/// `ScaledU16::codes` into [`StagingPool::give`], so the compiler holds the
/// slot's element and the grid's equal and neither this nor the budget above it
/// can name a width the store does not use.
///
/// The literal it replaces was not dead — moving it alone to
/// `size_of::<f32>()` fails this build on the pin below, which is exactly what
/// it was put there to do. What it could not do is **re-derive**: the divisor
/// and the `size_of::<u16>()` inside
/// [`CONUS_GRID_BYTES`](super::CONUS_GRID_BYTES) were two spellings of one
/// width that CANCELLED, so the pair read 24,500,000 whether or not either
/// still named the store. Now the numerator follows the store and the divisor
/// follows the slot, and a genuine widening carries both instead of
/// red-gating on a constant that was never the thing that moved.
pub const STAGING_POINTS: usize = super::FRAME_STAGING_BYTES / StagingPool::ELEMENT_BYTES;

// The two terms, pinned APART, so a build failure names which one moved rather
// than only that the quotient did. The point count is stated as a literal on
// purpose: deriving it from `FRAME_STAGING_BYTES` again would be the same
// division restated and could not disagree with itself.
const _: () = assert!(StagingPool::ELEMENT_BYTES == 2);
// One CONUS mosaic, in points — 7000 x 3500.
const _: () = assert!(STAGING_POINTS == 24_500_000);

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
    slot: Mutex<Option<Vec<StagedCode>>>,
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
    /// **Bytes the slot is holding right now** — a level, not a total: one
    /// mosaic's worth while a buffer is parked here, zero while it is out
    /// being decoded into.
    ///
    /// Maintained at the two transitions inside the slot's own critical
    /// section rather than read off the slot, because the one caller that
    /// needs it is a frame-thread census and the slot is `try_lock`-only: a
    /// reader that missed the lock would have to report either a stale figure
    /// or a false zero, and a false zero on a 49 MB block is the exact shape
    /// of mistake the census exists to stop.
    retained: AtomicUsize,
}

/// Running totals off [`StagingPool`], in the order
/// `(allocated, reused, declined)`.
///
/// Always on, like `squallar_egui::overlay_cache::ledger` and `UploadTotals`:
/// three relaxed counters cost nothing beside a 49 MB decode, and a figure that
/// only exists under a `cfg` is a figure nobody reads when the tab dies in the
/// field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagingTotals {
    pub allocated: usize,
    pub reused: usize,
    pub declined: usize,
}

/// **The element the slot holds** — the store's own, not a restatement of it.
///
/// [`StagingPool::recycle`] moves a decoded grid's `ScaledU16::codes` into
/// [`StagingPool::give`], so this alias and the grid's element are held equal
/// by the compiler rather than by two edits agreeing.
pub type StagedCode = crate::render::gridded::ScaledCode;

impl StagingPool {
    /// **Bytes one element of the slot occupies** — the width
    /// [`STAGING_POINTS`] divides the byte budget by, so that constant does not
    /// have to name a type.
    ///
    /// A literal `size_of::<u16>()` would be the same defect one turn later:
    /// it goes on reading two after the slot it describes has moved.
    pub const ELEMENT_BYTES: usize = size_of::<StagedCode>();

    pub const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            allocated: AtomicUsize::new(0),
            reused: AtomicUsize::new(0),
            declined: AtomicUsize::new(0),
            retained: AtomicUsize::new(0),
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
    pub fn take(&self, points: usize) -> Result<Vec<StagedCode>, String> {
        if points == STAGING_POINTS
            && let Ok(mut slot) = self.slot.try_lock()
            && let Some(mut buffer) = slot.take()
        {
            // The slot is empty from here whichever arm below runs, including
            // the mismatch that drops the buffer.
            self.retained.store(0, Ordering::Relaxed);
            // Belt and braces: `give` already refuses any other capacity, and
            // a buffer that somehow arrived at another one must not be handed
            // out as if it were mosaic-sized. Falling out of this block drops
            // it and allocates fresh below, exactly as a contended slot does.
            if buffer.capacity() == STAGING_POINTS {
                self.reused.fetch_add(1, Ordering::Relaxed);
                // Both ends, deliberately. `give` clears on the way in, and a
                // buffer is cleared again on the way out, so the "the decode
                // starts from empty" invariant does not depend on any one
                // caller having done the right thing. For a `u16` this is a
                // store to `len` and nothing else.
                buffer.clear();
                return Ok(buffer);
            }
        }
        let mut fresh: Vec<StagedCode> = Vec::new();
        fresh.try_reserve_exact(points).map_err(|_| {
            format!(
                "MRMS: cannot hold a {} MB staging grid in this build's memory",
                points.saturating_mul(size_of::<u16>()) / (1024 * 1024),
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
    pub fn give(&self, mut values: Vec<StagedCode>) {
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
        self.retained
            .store(values.capacity() * size_of::<u16>(), Ordering::Relaxed);
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
    /// (GMGSI 15 MB, MRMS 49 MB), so this is ~64 MB resident whether or not
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
                self.retained.store(0, Ordering::Relaxed);
                drop(buffer);
                true
            }
            None => false,
        }
    }

    /// Take a [`MrmsGrid`](super::MrmsGrid)'s values back into the pool, if
    /// this is the last reference to them.
    ///
    /// `Arc::into_inner` rather than a clone-and-drop: a grid whose raster job
    /// is still in flight is genuinely still in use, and prising the values out
    /// from under it would be a use-after-free by another name. That case is
    /// counted as `declined` and the grid drops normally.
    pub fn recycle(&self, grid: super::MrmsGrid) {
        // **Only the narrow arm's buffer fits this slot.** The pool is a
        // `Vec<u16>` because that is what every shipped MRMS granule decodes
        // into; a grid that fell to `GridValues::F32` — a packing wider than
        // 16 bits, or one that went through grib's own `dispatch()` — holds a
        // differently-typed allocation that this slot cannot take, and it is
        // counted as declined rather than silently dropped uncounted.
        match std::sync::Arc::into_inner(grid.grid) {
            Some(crate::render::gridded::ResidentGrid {
                values: crate::render::gridded::GridValues::Scaled(scaled),
                ..
            }) => self.give(scaled.codes),
            _ => {
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

    /// **What the slot is holding**, in bytes: one mosaic
    /// ([`super::FRAME_STAGING_BYTES`]) while a buffer is parked, zero while
    /// it is out with a decode.
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

impl Default for StagingPool {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide staging area — what every shipped decode uses.
///
/// One slot for the whole application, not one per handler or one per thread. A
/// thread-local would be 49 MB per worker thread on native for a path that runs
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
