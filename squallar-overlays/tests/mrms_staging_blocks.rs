//! **How many mosaic-sized blocks the shipped MRMS decode asks the allocator
//! for.**
//!
//! Its own binary with a counting `#[global_allocator]`, rather than an
//! assertion read off `StagingPool::totals`, for two reasons that are the whole
//! design of this file.
//!
//! * **The instrument must be able to disagree with the fix.** A pool that
//!   reported one reuse per decode while still taking a fresh block each time
//!   would satisfy any assertion built on its own counters — the checker and the
//!   checked would have come from one belief. `LargeBlocks` counts real
//!   `GlobalAlloc::alloc` calls at or above 16 MiB and knows nothing about MRMS,
//!   which is also why it compiles and runs against the **unmodified** tree.
//! * **A filtered run in this workspace is not self-contained.** The shipped
//!   slot is process-global, so a test sharing a binary with the decode suite
//!   could not tell its own reuse from a buffer another test happened to leave
//!   behind. One binary, one test.
//!
//! **16 MiB is the threshold, and it moved down from 64 MiB when the store
//! narrowed.** 64 MiB was chosen to fall between the two large blocks a decode
//! made when this file was written: grib's PNG image buffer at
//! 24 500 000 x 2 = 49 MB, which must **not** be counted because it is not what
//! this fix touches, and a CONUS mosaic's values at 24 500 000 x 4 = 98 MB,
//! which must. Both of those premises are now gone. The image buffer no longer
//! exists at all — `mrms::decode` streams section 7 a PNG row at a time, and
//! `tests/mrms_decode_image_buffer.rs` gates that separately — and the mosaic
//! itself is 24 500 000 x 2 = 49,000,000 B, because the store keeps MRMS's own
//! 16-bit code rather than a widened `f32`.
//!
//! **So the block this file exists to count fell BELOW the old bar.** Left at
//! 64 MiB the measurement is vacuous in the worst direction: `took == 0` is
//! then true whatever the pool does, because no allocation a decode makes can
//! reach the bar at all — and the non-triviality control below kept passing
//! while it happened, because it reserved a `Vec<f32>` (98,000,000 B) rather
//! than the `Vec<u16>` the decode actually takes. A control that is bigger than
//! the thing it stands in for cannot detect this. It reserves the real width
//! now.
//!
//! 16 MiB is the same bar `tests/mrms_decode_image_buffer.rs` uses and the same
//! gap: the other blocks a warm decode takes are the gunzipped GRIB2 bytes
//! (1,369,957 B) and grib's dummy bitmap (3,062,500 B). Nothing measured lands
//! between 3.1 MB and 49 MB. Moving a threshold because the thing below it went
//! away is how a threshold stops meaning anything; moving one because the thing
//! it measures moved underneath it is the opposite, and not doing so is what
//! leaves a green gate measuring nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use squallar_overlays::mrms::{MrmsGrid, MrmsProduct, decode, staging};

/// Blocks at or above this are the ones the shipping freeze was about. See the
/// header for why it is 16 MiB — under the 49,000,000 B mosaic it must count,
/// and far over the 3.1 MB of blocks it must not.
const LARGE: usize = 16 * 1024 * 1024;

static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Off outside the measured window, so the fixture decode that primes the slot
/// and anything the harness itself does are not in the figure.
static COUNTING: AtomicBool = AtomicBool::new(false);

struct LargeBlocks;

unsafe impl GlobalAlloc for LargeBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= LARGE && COUNTING.load(Ordering::Relaxed) {
            LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= LARGE && COUNTING.load(Ordering::Relaxed) {
            LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    /// A grow past the bar counts: a `Vec` that reallocates its way up to a
    /// mosaic has taken a fresh mosaic-sized block, whatever the call was named.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size >= LARGE && layout.size() < new_size && COUNTING.load(Ordering::Relaxed) {
            LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

const COMPOSITE_GZ: &[u8] =
    include_bytes!("../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz");

/// One granule through the whole shipped path, exactly as
/// `mrms::fetch::decode_body` runs it.
fn decode_granule(product: MrmsProduct) -> MrmsGrid {
    let grib = decode::gunzip(COMPOSITE_GZ).expect("the committed granule is a gzip member");
    decode::parse_grib2(&grib, product).expect("the committed granule decodes")
}

/// Hand a staged mosaic back the way the frame cache's eviction does.
///
/// The production caller is `MrmsFrameCache::insert`, private to the handler;
/// what it does is this one line, and doing it here keeps this file to the
/// decode path it is about. The gate on the caller is
/// `handlers::mrms::tests::an_evicted_frame_granule_is_offered_to_the_staging_pool`.
fn evict(grid: MrmsGrid) {
    staging::global().recycle(grid);
}

/// **N granules, no new mosaic-sized block.**
///
/// The pipeline advances one granule at a time — that is what
/// `FRAME_STAGING_BYTES` declares and what `MrmsHandler::frame_gate` enforces —
/// so this drives that exact sequence: decode, stage, evict the previous one,
/// decode the next. Before the retained buffer each pass took a fresh mosaic
/// block and freed the last, and on wasm32, where linear memory only grows and
/// dlmalloc cannot coalesce across a live block, that churn is what fragmented
/// a 1 GiB heap until a mosaic request failed with 192 MB free.
///
/// **Counted, never timed.** The figure is `alloc` calls at or above 16 MiB,
/// which is a property of the code rather than of the machine or the load.
///
/// **Observed red on the unmodified tree (`e02086ad`): 5 of 5.**
/// **Floor — `always_fresh`:** make `StagingPool::take` skip the slot and
/// always allocate.
#[test]
fn granules_decode_through_one_retained_mosaic_block() {
    /// Five, so an unpooled path is an order over the ceiling rather than a
    /// near miss, and so the sequence outlives the warm-up.
    const GRANULES: usize = 5;

    // The warm-up is outside the window on purpose: the first mosaic of a
    // process has nothing to be handed, and what this gate is about is the
    // steady state a playing loop lives in.
    evict(decode_granule(MrmsProduct::ReflectivityComposite));

    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_ALLOCS.load(Ordering::Relaxed);
    for _ in 0..GRANULES {
        evict(decode_granule(MrmsProduct::ReflectivityComposite));
    }
    let took = LARGE_ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);

    assert_eq!(
        took, 0,
        "{GRANULES} granules decoded one at a time took {took} mosaic-sized \
         blocks off the allocator. Past the warm-up a staged granule's buffer \
         IS the next granule's buffer: one slot, one block, and since the row \
         walk landed there is no per-granule allocation over 1 MiB left in a \
         playing loop at all",
    );

    // ── Non-triviality: the instrument can still count ────────────────────
    // A gate that had quietly stopped counting would reach the assertion above
    // green whatever the decode did.
    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_ALLOCS.load(Ordering::Relaxed);
    // **`Vec<u16>`, which is the width the decode's buffer actually is.** A
    // `Vec<f32>` here would reserve 98,000,000 B and clear any bar the real
    // 49,000,000 B block could not — a control that passes for a reason the
    // measurement cannot share, which is how the bar was left above the block
    // it counts.
    let mut mosaic: Vec<u16> = Vec::new();
    mosaic
        .try_reserve_exact(staging::STAGING_POINTS)
        .expect("a mosaic buffer fits on a test host");
    let seen = LARGE_ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);
    drop(mosaic);
    assert_eq!(
        seen, 1,
        "an explicit mosaic-sized reservation must register as one large block; \
         it did not, so the zero above says nothing",
    );
}
