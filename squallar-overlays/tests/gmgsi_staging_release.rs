//! **That the release lever actually gives the block back**, watched by the
//! allocator rather than by the pool's own counter.
//!
//! `retained_bytes()` falling to zero is what a lever that merely *forgot* the
//! buffer would also print, and a forgotten 60 MB block is worse than a parked
//! one: it is the same resident bytes with nothing left that can reclaim them.
//! So the figure here is a real `GlobalAlloc::dealloc` at the mosaic size, in a
//! window that contains one call to
//! [`StagingPool::release_retained`](squallar_overlays::staging::StagingPool::release_retained)
//! and nothing else.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reasons
//! `gmgsi_staging_blocks.rs` gives: the instrument must be able to disagree with
//! the fix, and the shipped slot is process-global, so one binary, one test.
//!
//! The bar is 32 MiB, below the 60,000,000 B payload and above everything else
//! a decode touches (a 256-row coordinate window is 5 MB, a `data` chunk
//! 4.2 MB, the granule 7.5 MB). Frees are counted through `dealloc` only: every
//! `Vec` drop takes that path, and a shrinking `realloc` is not something any
//! buffer on this path performs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use squallar_overlays::gmgsi::{GmgsiChannel, decode, staging};

const LARGE: usize = 32 * 1024 * 1024;

static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
static LARGE_FREES: AtomicUsize = AtomicUsize::new(0);
/// Off outside the measured window, so the fixture copy and the warm-up are
/// not in the figure.
static COUNTING: AtomicBool = AtomicBool::new(false);
static FREED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct LargeBlocks;

impl LargeBlocks {
    fn note_alloc(size: usize) {
        if size < LARGE || !COUNTING.load(Ordering::Relaxed) {
            return;
        }
        LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
    }

    fn note_free(size: usize) {
        if size < LARGE || !COUNTING.load(Ordering::Relaxed) {
            return;
        }
        LARGE_FREES.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut freed) = FREED.lock() {
            freed.push(size);
        }
    }
}

unsafe impl GlobalAlloc for LargeBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::note_alloc(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::note_alloc(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::note_free(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.size() < new_size {
            Self::note_alloc(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

const GRANULE: &[u8] = include_bytes!(
    "../testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
);

/// One granule through the whole shipped path, exactly as
/// `gmgsi::fetch::fetch_key` runs it.
fn decode_granule() -> decode::GmgsiGrid {
    decode::decode(GRANULE.to_vec(), GmgsiChannel::LongwaveIr)
        .expect("the committed granule decodes")
}

/// Hand a staged mosaic back the way the frame cache's eviction does.
fn evict(grid: decode::GmgsiGrid) {
    staging::recycle(staging::global(), grid.grid);
}

/// Count the large blocks `f` takes and gives back.
fn counting<T>(f: impl FnOnce() -> T) -> (T, usize, usize, Vec<usize>) {
    FREED.lock().expect("no poison").clear();
    COUNTING.store(true, Ordering::Relaxed);
    let allocs_before = LARGE_ALLOCS.load(Ordering::Relaxed);
    let frees_before = LARGE_FREES.load(Ordering::Relaxed);
    let out = f();
    let allocs = LARGE_ALLOCS.load(Ordering::Relaxed) - allocs_before;
    let frees = LARGE_FREES.load(Ordering::Relaxed) - frees_before;
    COUNTING.store(false, Ordering::Relaxed);
    let freed = FREED.lock().map(|f| f.clone()).unwrap_or_default();
    (out, allocs, frees, freed)
}

/// **The lever frees the block, and the pool is a working pool afterwards.**
///
/// The three things a memory governor's tier-2 step needs to be true: pulling
/// the lever returns the grid to the allocator (not merely to a counter),
/// pulling it on an empty slot costs nothing and reports nothing, and a decode
/// arriving after a release still runs and refills the slot.
///
/// **Floor — `forget_only`:** make `release_retained` store zero into
/// `retained_points` and leave the buffer in the slot; the pool's own figure
/// still reads 0 and the free count below reads 0 instead of 1.
#[test]
fn the_release_lever_returns_the_retained_mosaic_to_the_allocator() {
    const MOSAIC_BYTES: usize = staging::STAGING_POINTS * size_of::<f32>();

    // Warm the slot the way an arriving granule's eviction does.
    evict(decode_granule());
    assert_eq!(
        staging::global().retained_bytes(),
        MOSAIC_BYTES,
        "premise: a mosaic is parked in the slot",
    );

    let (released, allocs, frees, freed) = counting(|| staging::global().release_retained());
    assert!(released, "the lever must find the parked mosaic");
    assert_eq!(
        frees, 1,
        "releasing must hand exactly one block back to the allocator. A lever \
         that only cleared the pool's own figure would leave the 60 MB \
         resident with nothing left able to reclaim it, and `retained_bytes` \
         would read zero either way — which is why this gate counts `dealloc` \
         and not the counter. Freed: {freed:?}",
    );
    assert_eq!(
        freed,
        vec![MOSAIC_BYTES],
        "and the block freed must be the mosaic itself, not something near it",
    );
    assert_eq!(
        allocs, 0,
        "and nothing may be taken to release something. Freed: {freed:?}",
    );
    assert_eq!(
        staging::global().retained_bytes(),
        0,
        "with the pool's own level agreeing with the allocator",
    );

    // ── Control: the lever on an empty slot is free and honest ───────────
    let (again, _, idle_frees, _) = counting(|| staging::global().release_retained());
    assert!(!again, "a second pull has nothing to find and says so");
    assert_eq!(
        idle_frees, 0,
        "and costs the allocator nothing, so an idle policy or a governor may \
         call it on every pass without paying for the passes that find nothing",
    );

    // ── A decode after a release is an ordinary decode ────────────────────
    let before = staging::global().totals();
    let grid = decode_granule();
    assert_eq!(
        staging::global().totals().allocated,
        before.allocated + 1,
        "the decode after a release pays for its own block, exactly as the \
         first decode of a process does",
    );
    let squallar_overlays::render::gridded::GridValues::F32(raster) = &grid.grid.values else {
        panic!("a GMGSI raster is f32");
    };
    assert_eq!(
        raster.len(),
        staging::STAGING_POINTS,
        "and it is a whole granule, not a short one",
    );
    evict(grid);
    assert_eq!(
        staging::global().retained_bytes(),
        MOSAIC_BYTES,
        "and the slot refills, so a release is not a teardown",
    );
    let staged = staging::global()
        .take(staging::STAGING_POINTS)
        .expect("the slot is full again");
    assert_eq!(
        staging::global().totals().reused,
        before.reused + 1,
        "and the granule after that is handed the retained buffer again",
    );
    drop(staged);

    // ── Non-triviality: the instrument can see a free ─────────────────────
    let ((), _, control_frees, control) = counting(|| {
        let mut mosaic: Vec<f32> = Vec::new();
        mosaic
            .try_reserve_exact(staging::STAGING_POINTS)
            .expect("a mosaic buffer fits on a test host");
        drop(mosaic);
    });
    assert_eq!(
        control_frees, 1,
        "an explicit mosaic-sized reservation, dropped, must register as one \
         freed large block; it did not, so the figures above say nothing. \
         Freed: {control:?}",
    );
}
