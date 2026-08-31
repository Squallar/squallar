//! **How many payload-sized blocks one GMGSI decode asks the allocator for.**
//!
//! Its own binary with a counting `#[global_allocator]`, for the two reasons
//! `mrms_staging_blocks.rs`'s header gives and one of its own.
//!
//! * **The instrument must be able to disagree with the fix.** `LargeBlocks`
//!   counts real `GlobalAlloc::alloc` calls and knows nothing about NetCDF4,
//!   which is why it compiles and runs against the **unmodified** tree.
//! * **A filtered run in this workspace is not self-contained.** One binary,
//!   one test.
//! * The failure this is about is an **abort, not a panic**: wasm32 is
//!   `panic-strategy = "abort"`, so `handle_alloc_error` produces no
//!   `panicked at` line and every panic-count gate in the browser rig reads
//!   zero through it. Nothing downstream can observe the block; only counting
//!   it here can.
//!
//! # The threshold, and its denominator
//!
//! One granule is 3000 x 5000 = 15,000,000 points. `data` is `float` on disk —
//! read off both the real `GLOBCOMPLIR_v3r0_blend` granule and the committed
//! fixture on 2026-08-31 — so the blocks a decode makes at that scale are:
//!
//! | block | bytes | is it waste? |
//! |---|---|---|
//! | `data` in its declared `f32` storage | 60,000,000 | no — the read |
//! | a widened `Vec<f64>` raw domain | **120,000,000** | **yes** |
//! | the `Vec<f32>` raster | 60,000,000 | no — the payload |
//!
//! `LARGE` is 64 MiB because it falls between the 60,000,000 B the decode is
//! entitled to, which must **not** be counted, and a 120,000,000 B widening,
//! which must. `hdf5_pure`'s own chunk buffers sit below it too: `lat` and
//! `lon` are a single 3000 x 5000 `f32` chunk, 60,000,000 B, and `data`'s
//! chunks are 1 x 793 x 1322.
//!
//! `OBSERVE` is lower, and only records: a red here should say what it saw
//! rather than only that it saw something. It is also what shows that the
//! 60,000,000 B blocks under the bar are many — a decode inflates the whole
//! single-chunk `lat`/`lon` variable once per windowed row read.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use squallar_overlays::gmgsi::{GmgsiChannel, decode};

/// Blocks at or above this are counted. See the header for why it is between
/// 60 MB and 120 MB rather than round.
const LARGE: usize = 64 * 1024 * 1024;

/// Blocks at or above this are *recorded* for the failure message. Below the
/// payload on purpose, so a red names every large block rather than only the
/// ones over the bar.
const OBSERVE: usize = 32 * 1024 * 1024;

static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Off outside the measured window, so nothing the harness itself does — the
/// fixture copy, the test framework — is in the figure.
static COUNTING: AtomicBool = AtomicBool::new(false);
static SEEN: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct LargeBlocks;

impl LargeBlocks {
    /// Record one request. Never allocates while the counting flag is set for
    /// its own bookkeeping beyond the `Vec` push, which is why `OBSERVE` is
    /// high enough that the log stays a handful of entries.
    fn note(size: usize) {
        if size < OBSERVE || !COUNTING.load(Ordering::Relaxed) {
            return;
        }
        if size >= LARGE {
            LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
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
    /// 120 MB has taken a fresh 120 MB block, whatever the call was named.
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

/// Grid shape, from the granule's `data(time, yc, xc)` dimensions.
const POINTS: usize = 3000 * 5000;

/// **A granule decodes without a block larger than the raster it produces.**
///
/// The raster *is* the payload: 15,000,000 `f32`, 60,000,000 B, and it has to
/// exist. Anything bigger than that in a decode is an intermediate, and on
/// wasm32 — 1 GiB of linear memory that only grows, measured at 192 MB free
/// under a playing loop — an infallible 120 MB intermediate is the difference
/// between a decode and an abort.
///
/// **Counted, never timed.** The figure is `alloc` calls at or above 64 MiB,
/// a property of the code rather than of the machine or the load.
#[test]
fn a_granule_decodes_without_a_block_larger_than_its_raster() {
    // Outside the window: the fixture copy the decoder takes ownership of.
    let bytes = GRANULE.to_vec();

    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_ALLOCS.load(Ordering::Relaxed);
    let grid =
        decode::decode(bytes, GmgsiChannel::LongwaveIr).expect("the committed granule decodes");
    let took = LARGE_ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);

    let seen = SEEN.lock().map(|s| s.clone()).unwrap_or_default();
    assert_eq!(
        grid.grid.values.len(),
        POINTS,
        "the decode produced a raster"
    );
    assert_eq!(
        took,
        0,
        "one GMGSI decode took {took} block(s) at or above {LARGE} B. The \
         raster it returns is {} B and is the largest thing the decode is \
         entitled to build; every block above that is an intermediate, and on \
         wasm32 an infallible one aborts rather than failing. Blocks at or \
         above {OBSERVE} B during the decode, in order: {seen:?}",
        POINTS * 4,
    );

    // ── Non-triviality: the instrument can still count ────────────────────
    // A gate that had quietly stopped counting would reach the assertion above
    // green whatever the decode did.
    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_ALLOCS.load(Ordering::Relaxed);
    let mut wide: Vec<f64> = Vec::new();
    wide.try_reserve_exact(POINTS)
        .expect("a 120 MB reservation fits on a test host");
    let counted = LARGE_ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);
    drop(wide);
    assert_eq!(
        counted, 1,
        "an explicit {POINTS}-element f64 reservation — the exact block this \
         gate is about — must register as one large block; it did not, so the \
         zero above says nothing",
    );
}
