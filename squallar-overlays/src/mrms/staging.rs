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
//! ## What it stages is a BAND, not a mosaic
//!
//! The buffer was the decode's whole plane: 24.5 M codes in, one read out by
//! the tiler, parked for the next granule. It is
//! [`CONUS_BAND_BYTES`](super::CONUS_BAND_BYTES) — **224,000 B, 16 rows** —
//! since `decode::tile_png_codes` began building the store out of the row walk
//! instead, because a tile is decided by 16 rows and 16 rows is therefore the
//! largest thing a decode has to hold. **Nothing else about this module
//! changes**: one slot, one allocation, cleared at both ends, keyed on the
//! shape the last granule handed back, and handed back by the decode itself the
//! instant it is spent. The paragraphs below are about a 49 MB block and are
//! kept as written — they are why the slot exists, and the slot still exists.
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
//! ## The one capacity is discovered, never declared
//!
//! **A compile-time point count is a guess about a product, and a wrong guess
//! is silent.** This slot was keyed on [`STAGING_POINTS`]: `take` reused only
//! when the arriving grid had exactly that many points and `give` accepted a
//! buffer back only at exactly that capacity. Both spellings are a claim that
//! NOAA publishes a 7000 x 3500 CONUS mosaic, and **that claim is an
//! observation, not a definition** — the module header has said so since the
//! layer landed, in the words "measured against the live bucket".
//!
//! GMGSI is what an observation is worth. Its pool was keyed the same way on
//! `3000 * 5000`, the product started publishing `[1, 3000, 4999]`, and from
//! that granule on the pool reused nothing and accepted nothing back: every
//! decode allocated a fresh 60 MB block and every one was freed again —
//! precisely the churn the module exists to remove, with the module in place,
//! at full speed, with no error anywhere. Its suites went on passing because
//! the committed fixture was 5000 wide. A suite whose fixture is the constant
//! cannot notice the constant has stopped describing the product.
//!
//! **The CONUS grid has not moved, and this is precaution rather than repair.**
//! Read off `noaa-mrms-pds` on 2026-09-04, section 3 of one granule per day
//! across 17 dates spanning the bucket's whole retention — 2020-10-14, the
//! first day held, through today — on both shipped products: **every one is
//! grid definition template 3.0, `ni` 7000, `nj` 3500, 24,500,000 points at
//! 0.01 degrees**, DRT 5.41, 16-bit, no bitmap. Four other CONUS products
//! (`MergedReflectivityQC_00.50`, `_19.00`, `MESH_00.50`, `EchoTop_18_00.50`)
//! read the same today. Whether NOAA *guarantees* that shape is not something
//! this tree can establish; what it can say is that nothing in six years of
//! published CONUS granules has departed from it.
//!
//! So the retained buffer carries **its own** point count and the reuse key is
//! that rather than a constant, exactly as [`crate::staging`] does for GMGSI: a
//! grid of a shape the slot is not holding drops the retained buffer and
//! becomes the shape the slot holds, counted by
//! [`StagingPool::resizes`](crate::staging::StagingPool::resizes).
//! [`STAGING_POINTS`] survives as the **nominal** figure — what
//! [`FRAME_STAGING_BYTES`](super::FRAME_STAGING_BYTES) and
//! [`GRID_CACHE_BYTES`](super::GRID_CACHE_BYTES) are sized for, and the
//! reference a `retained_points` reading is compared against the next time a
//! product moves.
//!
//! ## Nothing here may hand a grid the wrong bytes
//!
//! A retained buffer that outlives the product it was filled for is a
//! data-corruption bug wearing a performance fix's clothes, so the invariants
//! are narrow and checked rather than reasoned about. **What the shape key
//! changed is *which* count "exactly" names, never that it is exact:**
//!
//! * **capacity is matched exactly, never merely "big enough".**
//!   [`StagingPool::take`] answers the pooled buffer only when the grid it is
//!   about to hold has exactly the retained buffer's point count. A "≥" rule
//!   would hand a 400-byte test grid a 49 MB block whose `len` no longer
//!   describes its footprint, which is the figure both byte budgets are
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
//!   summary moves);
//! * **a grid another reference still holds is genuinely in use.**
//!   [`StagingPool::recycle`] reclaims through `Arc::into_inner` and declines
//!   rather than prising a buffer out from under a raster job still reading it;
//! * **a pool that is not doing its job says so.** The three totals were always
//!   here and nothing ever read them, which is how GMGSI's wholly inert pool
//!   shipped and held: `reused: 0, declined: N` reads at a glance exactly like
//!   a pool nobody has exercised yet.
//!   [`StagingPool::health`](crate::staging::StagingPool::health) is that
//!   reading as a verdict.
//!
//! ## Why the slot itself is [`crate::staging`]'s and the doors are not
//!
//! The pool logic is one implementation, not two: this type wraps
//! [`crate::staging::StagingPool<StagedCode>`](crate::staging::StagingPool) and
//! forwards, so the invariants above are checked in one place and a repair to
//! either instance is a repair to both. What cannot move up is the two doors —
//! [`StagingPool::recycle`] and [`StagingPool::recycle_shared`] take an
//! [`MrmsGrid`](super::MrmsGrid), which is this layer's type, and
//! `render::handlers::mrms` calls them as **inherent** methods on this concrete
//! name. A free function like GMGSI's would be a change to that handler; a
//! wrapper is not.

/// **The band shape this build's decode stages**, in points —
/// [`CONUS_BAND_BYTES`](super::CONUS_BAND_BYTES) divided by the width of one
/// staged value: 16 rows of 7000 codes, **112,000**.
///
/// **It was the whole mosaic, 24,500,000.** The decode built a 49,000,000 B
/// plane and the tiler read it once; the plane was this slot, parked between
/// granules, and once the store itself had gone tiled it was **81 % of what one
/// looping pane held**. `TileBands` is fed from the PNG row walk now, so the
/// largest buffer a decode holds is one tile-row band and that is what the slot
/// holds too — the same "one allocation, not one per granule" property this
/// module exists for, at 1/219th of the bytes.
///
/// **Not the slot's capacity.** It was, and on GMGSI that exact spelling was
/// the defect: a product whose grid moves by one column leaves a pool keyed on
/// the constant reusing nothing and accepting nothing back, silently, for the
/// life of the process. The slot takes its one capacity from the grid that
/// hands a buffer back, whatever shape that is; this figure is what the pool
/// reports as
/// [`nominal_points`](crate::staging::StagingPool::nominal_points) — the
/// reference a [`retained_points`](crate::staging::StagingPool::retained_points)
/// reading is compared against.
///
/// # The divisor names the slot rather than restating its type
///
/// It is [`StagingPool::ELEMENT_BYTES`], whose [`StagedCode`] **is** the store's
/// own element — [`recycle`](StagingPool::recycle) moves a decoded grid's
/// `ScaledU16::codes` into [`give`](StagingPool::give), so the compiler holds
/// the slot's element and the grid's equal and neither this nor the budget
/// above it can name a width the store does not use. A literal
/// `size_of::<u16>()` would be the same defect one turn later: it goes on
/// reading two after the slot it describes has moved.
///
/// The literal it replaced was not dead — moving it alone to `size_of::<f32>()`
/// fails this build on the pin below, which is exactly what it was put there to
/// do. What it could not do is **re-derive**: the divisor and the
/// `size_of::<u16>()` inside [`CONUS_GRID_BYTES`](super::CONUS_GRID_BYTES) were
/// two spellings of one width that CANCELLED, so the pair read 24,500,000
/// whether or not either still named the store. Now the numerator follows the
/// store and the divisor follows the slot.
///
/// The assertion below is the guard, because prose is not a gate: the budget
/// holds one CONUS mosaic and that is a **point** count, so a divisor error
/// moves it and fails the build.
pub const STAGING_POINTS: usize = super::CONUS_BAND_BYTES / StagingPool::ELEMENT_BYTES;

// The two terms, pinned APART, so a build failure names which one moved rather
// than only that the quotient did. The point count is stated as a literal on
// purpose: deriving it from `FRAME_STAGING_BYTES` again would be the same
// division restated and could not disagree with itself.
const _: () = assert!(StagingPool::ELEMENT_BYTES == 2);
// One tile-row band of the CONUS mosaic, in points — 16 of the 3500 rows every
// granule `noaa-mrms-pds` has published since 2020-10-14 declares in section 3.
// A *nominal* figure: it is what the slot holds on the shipped product and it
// does not key the slot, which takes its shape from the grid that hands a
// buffer back.
const _: () = assert!(STAGING_POINTS == 112_000);

/// **The element the slot holds** — the store's own, not a restatement of it.
///
/// [`StagingPool::recycle`] moves a decoded grid's `ScaledU16::codes` into
/// [`StagingPool::give`], so this alias and the grid's element are held equal
/// by the compiler rather than by two edits agreeing.
pub type StagedCode = crate::render::gridded::ScaledCode;

pub use crate::staging::{StagingHealth, StagingTotals};

/// **One retained mosaic-sized buffer, and the running count of what it saved.**
///
/// A single slot, because the budget is a single grid. A decode that arrives
/// while the slot is empty allocates its own and is free to hand it back
/// afterwards; nothing waits, and nothing is throttled here — the
/// one-at-a-time throttle is the handler's frame gate and stays there.
///
/// **Injectable rather than only global** for the reason `MrmsGridCache`'s own
/// budget is injected: a suite that can only observe the process-wide slot
/// cannot tell "the pool worked" from "another test in this binary happened to
/// leave a buffer in it", and a filtered run in this workspace is explicitly
/// not self-contained. The shipped path uses [`global`].
///
/// A wrapper over [`crate::staging::StagingPool`] rather than a second copy of
/// it — see this module's last heading for what that buys and what it cannot.
pub struct StagingPool(crate::staging::StagingPool<StagedCode>);

impl StagingPool {
    /// **Bytes one element of the slot occupies** — the width
    /// [`STAGING_POINTS`] divides the byte budget by, so that constant does not
    /// have to name a type.
    pub const ELEMENT_BYTES: usize = crate::staging::StagingPool::<StagedCode>::ELEMENT_BYTES;

    /// A pool whose surrounding byte budgets were sized for [`STAGING_POINTS`]
    /// values. Which buffer it retains is decided by the grid that hands one
    /// back, not by that figure.
    pub const fn new() -> Self {
        Self(crate::staging::StagingPool::new(STAGING_POINTS))
    }

    /// A buffer able to hold `points` values, retained from a previous granule
    /// when one is waiting and it is exactly the right size.
    ///
    /// **A request at another shape empties the slot** rather than leaving a
    /// block nobody can use parked in it; see [`crate::staging::StagingPool`],
    /// which is where the `try_lock` rule and the fallible reserve live too.
    pub fn take(&self, points: usize) -> Result<Vec<StagedCode>, String> {
        self.0.take(points).map_err(|e| format!("MRMS: {e}"))
    }

    /// Offer a spent mosaic buffer back.
    ///
    /// **The offered buffer's own capacity becomes the shape the slot holds.**
    /// Refused — and counted as refused — when the slot is already full or the
    /// buffer owns no allocation to retain. A refused buffer is dropped here,
    /// which is what every buffer did before this module existed.
    pub fn give(&self, values: Vec<StagedCode>) {
        self.0.give(values);
    }

    /// **Drop the retained buffer**, answering whether there was one to drop.
    ///
    /// The pool's one lever, shaped for **two callers with no knowledge of each
    /// other**: an idle policy in the layer that owns the source, and a memory
    /// governor's tier-2 pressure step. See [`crate::staging::StagingPool`].
    pub fn release_retained(&self) -> bool {
        self.0.release_retained()
    }

    /// **The retained buffer itself**, still owned, for a caller that frees it
    /// on a lane rather than here. See [`crate::staging::StagingPool`].
    pub fn take_retained(&self) -> Option<Vec<StagedCode>> {
        self.0.take_retained()
    }

    /// **Whether this pool may park an offered buffer.** See
    /// [`crate::staging::StagingPool`].
    pub fn is_retaining(&self) -> bool {
        self.0.is_retaining()
    }

    /// Turn parking on or off. See [`crate::staging::StagingPool`].
    pub fn set_retaining(&self, retaining: bool) {
        self.0.set_retaining(retaining);
    }

    /// **Decodes this pool has served** — one per [`Self::take`], however it
    /// was served. See [`crate::staging::StagingPool::decodes_served`].
    pub fn decodes_served(&self) -> u64 {
        self.0.decodes_served()
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
            // **A tiled grid has already given its plane back**, at the decode,
            // the instant the tiler had read it — which is the whole reason the
            // slot is one buffer rather than one per resident grid. There is
            // nothing here to offer and nothing was refused, so this is neither
            // a `give` nor a `decline`: counting it as either would make the
            // shipped path read like a pool that stopped being refilled.
            Some(crate::render::gridded::ResidentGrid {
                values: crate::render::gridded::GridValues::Tiled(_),
                ..
            }) => {}
            _ => self.0.decline(),
        }
    }

    /// [`Self::recycle`] for the live cache's shared carry.
    pub fn recycle_shared(&self, grid: std::sync::Arc<super::MrmsGrid>) {
        match std::sync::Arc::into_inner(grid) {
            Some(grid) => self.recycle(grid),
            None => self.0.decline(),
        }
    }

    /// **What the slot is holding**, in bytes: one grid at whatever shape the
    /// last granule handed back, zero while it is out with a decode.
    ///
    /// Derived from the retained buffer's own capacity, so a slot holding a
    /// shape this build was not sized for reports what the allocator is
    /// actually holding rather than the nominal figure. One relaxed load and a
    /// multiply, taking no lock — safe to read on the frame thread and safe to
    /// read from an allocation-error hook. It counts **capacity**: the buffer
    /// is always empty while it is in the slot, so its length would read zero
    /// and say nothing.
    pub fn retained_bytes(&self) -> usize {
        self.0.retained_bytes()
    }

    /// **The shape the slot is holding**, in points — zero while nothing is
    /// parked. The count [`Self::take`] matches a request against.
    pub fn retained_points(&self) -> usize {
        self.0.retained_points()
    }

    /// **The shape this build's byte budgets were sized for**, in points —
    /// [`STAGING_POINTS`], and not the reuse key. The two differing means the
    /// product's grid has moved off what this build was sized for.
    pub const fn nominal_points(&self) -> usize {
        self.0.nominal_points()
    }

    pub fn totals(&self) -> StagingTotals {
        self.0.totals()
    }

    /// **Decodes that arrived at a shape the slot was not holding.** Zero in
    /// the steady state; one per product change by design.
    pub fn resizes(&self) -> usize {
        self.0.resizes()
    }

    /// **Whether this pool is removing allocations, in one value.** See
    /// [`StagingHealth`] for why three counters were not enough on their own.
    pub fn health(&self) -> StagingHealth {
        self.0.health()
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
