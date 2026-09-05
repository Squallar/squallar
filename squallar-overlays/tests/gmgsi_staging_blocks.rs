//! **How many mosaic-sized blocks the shipped GMGSI decode asks the allocator
//! for, granule after granule.**
//!
//! Its own binary with a counting `#[global_allocator]`, for the reasons
//! `mrms_staging_blocks.rs` gives: the instrument must be able to disagree
//! with the fix (`LargeBlocks` counts real `GlobalAlloc` calls and knows
//! nothing about the pool), and a filtered run in this workspace is not
//! self-contained (the shipped slot is process-global — one binary, one test).
//!
//! # The threshold, and what sits on each side of it
//!
//! Every block this decode makes at scale is **exactly 60,000,000 B** — the
//! raster, and every stored-byte buffer `hdf5_pure` builds for a 3000 x 5000
//! `f32` variable — so unlike `gmgsi_decode_blocks.rs`, whose 64 MiB bar sits
//! *above* the payload to catch a widened intermediate, this bar sits
//! **below** it, at 32 MiB, to count the payload-sized blocks themselves.
//! Nothing else in a decode comes near: a 256-row coordinate window is 5 MB,
//! a `data` chunk 4.2 MB, the granule 7.5 MB.
//!
//! # What the count was, and what it is
//!
//! Measured on the committed granule, debug build, this counter, 2026-09-04:
//!
//! | | grid-sized blocks per decode |
//! |---|---|
//! | before (`833bad45`) | **91** |
//! | pool + one handle per coordinate variable | 5 |
//! | and the axis cache, steady state | **1** |
//!
//! The 91 were: 88 from the two 2-D coordinate variables — each stored as
//! one 60 MB chunk that `hdf5_pure` re-inflated **and re-unshuffled** for
//! every one of the 44 row windows the axis walk read, because its default
//! chunk cache holds 1 MiB and is per handle — plus, for `data`, the
//! assembled stored bytes, the storage-width `f32` copy, and the unpacked
//! raster.
//!
//! Holding each coordinate variable's chunk across its windows
//! ([`squallar_netcdf::Variable`]) makes the 88 into 4 — inflate and
//! unshuffle, once each — and the byte-identity axis cache
//! (`gmgsi::decode::AxisCache`) makes those 4 into 0 on every granule that
//! stores the same coordinate arrays as the last, which is every granule of
//! the product. The 1 that remains is `hdf5_pure`'s and transient: the
//! stored bytes it assembles for `data` before decoding them — `data` is
//! `(time=1, yc, xc)`, so its public API in 0.44 has no window narrower than
//! the whole. **The raster is not among them**: it is decoded into the slot's
//! retained buffer, and the first control below is what shows that; the
//! second shows the 4 the axis cache removed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use squallar_overlays::gmgsi::{GmgsiChannel, decode, staging};

/// Blocks at or above this are counted.
///
/// **12 MiB, down from 32.** The raster is 15,000,000 B now that a GMGSI value
/// is stored as the byte it is, so a 32 MiB bar would stop counting the very
/// block this gate is about — the first control's difference would read zero,
/// which the control itself says is the reading of a broken instrument. The
/// bar sits between everything a decode touches that is not a whole grid (a
/// `data` chunk is 1 x 793 x 1322 `f32` = 4,193,384 B, a 256-row coordinate
/// window 5,120,000 B, the committed granule's own bytes 533,762 B) and the
/// 15,000,000 B raster. The reader's assembled `data` bytes are 60,000,000 B
/// and are over both bars, which is why `READER_BLOCKS` is unmoved.
const LARGE: usize = 12 * 1024 * 1024;

static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Off outside the measured window, so the fixture copy and the warm-up are
/// not in the figure.
static COUNTING: AtomicBool = AtomicBool::new(false);
static SEEN: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct LargeBlocks;

impl LargeBlocks {
    fn note(size: usize) {
        if size < LARGE || !COUNTING.load(Ordering::Relaxed) {
            return;
        }
        LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut seen) = SEEN.lock() {
            seen.push(size);
        }
    }
}

unsafe impl GlobalAlloc for LargeBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::note(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    /// A grow past the bar counts: a `Vec` that reallocates its way up to
    /// 60 MB has taken a fresh 60 MB block, whatever the call was named.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.size() < new_size {
            Self::note(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

const GRANULE: &[u8] = include_bytes!(
    "../testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
);

/// The transient stored-byte blocks one steady-state decode costs inside
/// `hdf5_pure` — the assembled `data` bytes — so a moved count names what
/// moved. See the header.
const READER_BLOCKS: usize = 1;

/// What reading the two coordinate variables costs on top: inflate and
/// unshuffle, once each. Paid by the first granule and by any granule whose
/// stored geometry differs from the last.
const COORDINATE_BLOCKS: usize = 4;

/// One granule through the whole shipped path, exactly as
/// `gmgsi::fetch::fetch_key` runs it.
fn decode_granule() -> decode::GmgsiGrid {
    decode::decode(GRANULE.to_vec(), GmgsiChannel::LongwaveIr)
        .expect("the committed granule decodes")
}

/// Hand a staged mosaic back the way the frame cache's eviction does.
///
/// The production caller is `GmgsiFrameCache::insert`, private to the
/// handler; what it does is this one line, and the gate on the caller is
/// `handlers::gmgsi::tests::an_evicted_frame_granule_is_offered_to_the_staging_pool`.
fn evict(grid: decode::GmgsiGrid) {
    staging::recycle(staging::global(), grid.grid);
}

/// Count the large blocks `f` makes.
fn counting<T>(f: impl FnOnce() -> T) -> (T, usize, Vec<usize>) {
    SEEN.lock().expect("no poison").clear();
    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_ALLOCS.load(Ordering::Relaxed);
    let out = f();
    let took = LARGE_ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);
    let seen = SEEN.lock().map(|s| s.clone()).unwrap_or_default();
    (out, took, seen)
}

/// **N granules, no new raster-sized block** — and the control that shows
/// the instrument would have seen one.
///
/// The pipeline advances one granule at a time — that is what
/// `FRAME_STAGING_BYTES` declares and what `GmgsiHandler::frame_gate`
/// enforces — so this drives that exact sequence: decode, stage, evict the
/// previous one, decode the next. The figure per granule is `READER_BLOCKS`,
/// every one of them `hdf5_pure`'s and freed before the decode returns; the
/// raster comes out of the slot.
///
/// **Counted, never timed.** The figure is `alloc` calls at or above 32 MiB,
/// a property of the code rather than of the machine or the load.
///
/// **Two deliberate-regression controls, in the same binary.** The same
/// decode over a pool that has nothing to hand out — what every granule got
/// before the slot existed — costs exactly one more block, and that block is
/// the raster. The same decode over an axis cache that remembers nothing
/// costs exactly `COORDINATE_BLOCKS` more, the two chunks read again. A gate
/// that had quietly stopped counting would read zero on every arm and fail
/// both differences.
///
/// **Observed red on the unmodified tree (`833bad45`): 91 per granule.**
/// **Floor — `always_fresh`:** make `StagingPool::take` skip the slot and
/// always allocate; the pooled arm then reads `READER_BLOCKS + 1` and the
/// first control's difference reads zero. **Floor — `never_remember`:** make
/// `AxisCache::axis` skip the lookup; the pooled arm reads
/// `READER_BLOCKS + COORDINATE_BLOCKS` and the second control's difference
/// reads zero.
#[test]
fn granules_decode_through_one_retained_mosaic_block() {
    /// Three, so an unpooled path is a clear multiple over the ceiling
    /// rather than a near miss, and so the sequence outlives the warm-up.
    const GRANULES: usize = 3;

    // The warm-up is outside the window on purpose: the first mosaic of a
    // process has nothing to be handed and no axes to be remembered, and
    // what this gate is about is the steady state a playing loop lives in.
    evict(decode_granule());
    assert_eq!(
        staging::global().retained_bytes(),
        staging::STAGING_POINTS * size_of::<u8>(),
        "premise: the warm-up's raster is parked in the slot, at the byte a \
         point the decode stores it as",
    );
    assert_eq!(
        decode::axis_cache().totals(),
        decode::AxisCacheTotals { hits: 0, misses: 2 },
        "premise: the warm-up read and remembered both axes",
    );

    let ((), took, seen) = counting(|| {
        for _ in 0..GRANULES {
            evict(decode_granule());
        }
    });
    assert_eq!(
        took,
        GRANULES * READER_BLOCKS,
        "{GRANULES} granules decoded one at a time took {took} blocks at or \
         above {LARGE} B; the reader alone costs {READER_BLOCKS} per granule \
         (the assembled `data` bytes), the coordinate arrays are remembered, \
         and the raster must not add to that: past the warm-up a staged \
         granule's buffer IS the next granule's buffer. Blocks seen, in \
         order: {seen:?}",
    );
    assert_eq!(
        staging::global().totals().reused,
        GRANULES,
        "premise: every measured decode was handed the retained buffer (the \
         warm-up allocated it)",
    );
    assert_eq!(
        decode::axis_cache().totals(),
        decode::AxisCacheTotals {
            hits: 2 * GRANULES,
            misses: 2
        },
        "premise: every measured decode was handed both axes without a read",
    );

    // ── Control one: the same decode with nothing to be handed ───────────
    // A pool of this test's own, empty, so `take` allocates — exactly what
    // the shipped path did before the slot existed.
    let fresh: staging::StagingPool = staging::StagingPool::new(staging::STAGING_POINTS);
    let (grid, bypassed, seen) = counting(|| {
        decode::decode_in(
            GRANULE.to_vec(),
            GmgsiChannel::LongwaveIr,
            &fresh,
            decode::axis_cache(),
        )
        .expect("the committed granule decodes")
    });
    assert_eq!(
        fresh.totals().allocated,
        1,
        "premise: the control's pool allocated the raster",
    );
    assert_eq!(
        bypassed,
        READER_BLOCKS + 1,
        "with the slot bypassed one decode must cost exactly one more block \
         than a pooled one, and that block is the raster. Blocks seen: {seen:?}",
    );
    let squallar_overlays::render::gridded::GridValues::Bytes(raster) = &grid.grid.values else {
        panic!("a GMGSI raster is a byte store");
    };
    assert_eq!(raster.codes().len(), staging::STAGING_POINTS);
    // **The block the allocator was actually asked for**, not the length the
    // grid reports afterwards: a decode that grew into its buffer would report
    // the same length off a block of some other size. `Vec::capacity` is not
    // reachable through the store, and this is the better evidence anyway —
    // the instrument saw the request.
    assert!(
        seen.contains(&(staging::STAGING_POINTS * size_of::<u8>())),
        "and the extra block is capacity-exact at one byte a point, so it is \
         the raster and not a growth copy. Blocks seen: {seen:?}",
    );
    drop(grid);

    // ── Control two: the same decode with no axes remembered ─────────────
    // An axis cache of this test's own, empty, so both coordinate arrays are
    // read and verified — exactly what every granule did before the cache
    // existed. Through the shipped pool, so the raster is not in the figure.
    let forgetful = decode::AxisCache::new();
    let (grid, unremembered, seen) = counting(|| {
        decode::decode_in(
            GRANULE.to_vec(),
            GmgsiChannel::LongwaveIr,
            staging::global(),
            &forgetful,
        )
        .expect("the committed granule decodes")
    });
    assert_eq!(
        forgetful.totals(),
        decode::AxisCacheTotals { hits: 0, misses: 2 },
        "premise: the control read both coordinate arrays",
    );
    assert_eq!(
        unremembered,
        READER_BLOCKS + COORDINATE_BLOCKS,
        "with nothing remembered one decode must cost exactly the two \
         coordinate chunks more (inflated and unshuffled once each) than a \
         steady-state one. Blocks seen: {seen:?}",
    );
    evict(grid);

    // ── Non-triviality: the instrument can still count ────────────────────
    let (mosaic, counted, _) = counting(|| {
        // At the raster's own width, so what this proves the instrument can
        // see is the block class the figures above are about.
        let mut mosaic: Vec<u8> = Vec::new();
        mosaic
            .try_reserve_exact(staging::STAGING_POINTS)
            .expect("a mosaic buffer fits on a test host");
        mosaic
    });
    drop(mosaic);
    assert_eq!(
        counted, 1,
        "an explicit mosaic-sized reservation must register as one large block; \
         it did not, so the figures above say nothing",
    );
}
