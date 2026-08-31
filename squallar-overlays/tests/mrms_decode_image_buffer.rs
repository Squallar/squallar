//! **The largest block one MRMS decode takes, once the staging buffer is
//! warm.**
//!
//! The mosaic's values are 98,000,000 B and `super::staging` retains them
//! between granules, so on the shipped path that block is allocated once per
//! process. What was still allocated **per granule** is `grib`'s PNG stage:
//! `read_image_buffer` does `vec![0; reader.output_buffer_size()]`, and at
//! 7000 x 3500 samples of 16 bits that is **49,000,000 B**, measured, every
//! granule, on all three committed fixtures.
//!
//! It is `vec![0; n]` — **infallible**. wasm32 links this module with a hard
//! 1 GiB memory ceiling (`--max-memory=1073741824`,
//! `.github/scripts/wasm-threads.sh`) and is `panic-strategy = "abort"`, so an
//! allocation the engine cannot serve reaches `alloc::handle_alloc_error` and
//! traps. Nothing unwinds through that: winit's web event-loop runner keeps its
//! `RefCell` borrowed for the life of the page, the frame loop stops for good,
//! and the canvas holds its last painted frame while rAF, the network and the
//! workers all carry on. Measured on the Tier-2 `firefox.huge` leg: the module
//! sat at 910 MiB of 1024 when a granule asked for the next 49 MB, and the
//! symbolised trap was `handle_alloc_error` under
//! `Grib2SubmessageDecoder::dispatch` under `mrms::fetch::fetch_key`.
//!
//! So the figure this file gates is not "how much a decode uses" — it is that
//! **no single allocation in a decode scales with the grid's area**. The
//! shipped path streams section 7 a PNG row at a time
//! (`mrms::decode::decode_png_into`), which is 14,000 B at the mosaic's width;
//! `mrms::decode::tests::the_row_walk_decodes_what_grib_decodes` is the
//! separate check that it decodes the same values `grib` does.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `gridded_projection_band.rs` gives: the instrument counts real
//! `GlobalAlloc` calls at or above a size threshold and knows nothing about the
//! decoder, so it compiles and runs against the **unmodified** tree and can
//! disagree with the fix.
//!
//! **16 MiB is the threshold** and it is chosen to sit in a gap, not near
//! anything: the blocks a warm decode takes below it are the gunzipped GRIB2
//! bytes (1,369,957 B) and `grib`'s all-ones dummy bitmap (3,062,500 B, then a
//! 3,062,506 B grow), and the block above it was the image buffer at
//! 49,000,000 B. Nothing measured lands between 3.1 MB and 49 MB.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use squallar_overlays::mrms::{MrmsProduct, decode, staging};

/// See the header: a gap, not a bound anything sits near.
const LARGE: usize = 16 * 1024 * 1024;

thread_local! {
    /// The count and the high-water mark, **per thread for the same reason
    /// `COUNTING` is**: two tests measuring at once through one pair of global
    /// counters would each zero the other's window.
    static LARGE_ALLOCS: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

thread_local! {
    /// **Per thread, not per process**, and that is the difference between a
    /// measurement and a race. `cargo test` runs this binary's tests on
    /// parallel threads by default, so a global flag would have each test
    /// counting the other's allocations and the harness's — and a counter that
    /// picks up a neighbour's block reports a red that is nothing to do with
    /// the decoder. Every figure here is therefore "blocks allocated **by the
    /// thread under measurement**".
    ///
    /// `const`-initialised `Cell`: no lazy init and no destructor, so reading
    /// it from inside the allocator cannot allocate and cannot recurse.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

/// Begin a measured window on this thread, from zero.
fn start_counting() {
    let _ = LARGE_ALLOCS.try_with(|c| c.set(0));
    let _ = LARGEST.try_with(|c| c.set(0));
    let _ = COUNTING.try_with(|c| c.set(true));
}

/// End it, and answer `(blocks, largest)` for the window just closed.
fn stop_counting() -> (usize, usize) {
    let _ = COUNTING.try_with(|c| c.set(false));
    (
        LARGE_ALLOCS.try_with(|c| c.get()).unwrap_or(0),
        LARGEST.try_with(|c| c.get()).unwrap_or(0),
    )
}

struct LargeBlocks;

fn note(size: usize) {
    if size < LARGE {
        return;
    }
    // `try_with`: during thread teardown the local is gone, and a panic inside
    // the global allocator is not a diagnosis anyone can read.
    if COUNTING.try_with(|c| c.get()).unwrap_or(false) {
        let _ = LARGE_ALLOCS.try_with(|c| c.set(c.get() + 1));
        let _ = LARGEST.try_with(|c| c.set(c.get().max(size)));
    }
}

unsafe impl GlobalAlloc for LargeBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    /// A grow past the bar counts: a `Vec` that reallocates its way up to the
    /// grid's area has taken a fresh block that size, whatever the call was
    /// named.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.size() < new_size {
            note(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

const COMPOSITE_GZ: &[u8] =
    include_bytes!("../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz");
const RATE_GZ: &[u8] = include_bytes!("../testdata/MRMS_PrecipRate_00.00_20260822-032400.grib2.gz");

fn gz_for(product: MrmsProduct) -> &'static [u8] {
    match product {
        MrmsProduct::ReflectivityComposite => COMPOSITE_GZ,
        MrmsProduct::PrecipRate => RATE_GZ,
    }
}

/// The CONUS mosaic, from `squallar_overlays::mrms`.
const POINTS: usize = 7000 * 3500;

/// **No block in a warm decode scales with the grid.**
///
/// Warm because that is the shipped steady state: the pool holds the mosaic
/// buffer between granules, so the 98,000,000 B values vector is a
/// once-per-process cost and the only per-granule block left above the bar was
/// `grib`'s image buffer.
///
/// Both shipped products, because they carry different packing parameters and
/// different section-7 sizes (1,369,957 B of GRIB2 against 510,555 B) — and the
/// image buffer is sized off the *grid*, not off the payload, so a fix that
/// only held for the larger granule would be visible here.
#[test]
fn a_warm_decode_takes_no_grid_sized_block() {
    for &product in MrmsProduct::all() {
        let missing = product.missing_codes();
        let grib = decode::gunzip(gz_for(product)).expect("gzip member");

        // Its own pool, not the process-wide one: a filtered run in this
        // workspace is not self-contained, and a test reading the global slot
        // cannot tell its own reuse from another test's leftovers.
        let pool = staging::StagingPool::new();
        let warm = decode::parse_grib2_raw_in(&grib, missing, &pool).expect("warm-up decodes");
        pool.give(warm.values);
        assert_eq!(
            pool.totals().allocated,
            1,
            "{}: the warm-up should have allocated exactly the one mosaic \
             buffer this measurement then reuses",
            product.as_str(),
        );

        start_counting();
        let raw = decode::parse_grib2_raw_in(&grib, missing, &pool).expect("decodes");
        let (blocks, largest) = stop_counting();

        // **Non-triviality, before the assertion that matters.** A decode that
        // produced nothing would take no large block either, and would pass.
        assert_eq!(
            raw.values.len(),
            POINTS,
            "{}: the measured decode produced {} values, not the whole mosaic \
             — a decode that did nothing takes no blocks and proves nothing",
            product.as_str(),
            raw.values.len(),
        );
        assert!(
            raw.values.iter().filter(|v| v.is_finite()).count() > 1_000_000,
            "{}: the measured decode found almost no finite readings",
            product.as_str(),
        );
        assert_eq!(
            pool.totals().reused,
            1,
            "{}: the measured decode did not take the retained buffer, so the \
             98 MB block below would be in this figure",
            product.as_str(),
        );

        assert_eq!(
            blocks,
            0,
            "{}: one warm decode took {blocks} block(s) of at least \
             {LARGE} B, the largest {largest} B. A per-granule allocation \
             that scales with the {POINTS}-point grid is what traps the wasm32 \
             module against its 1 GiB ceiling, where nothing unwinds.",
            product.as_str(),
        );

        pool.give(raw.values);
    }
}

/// **The instrument can still count.**
///
/// Every assertion above is that a counter stayed at zero, which is exactly
/// what a broken counter reports. This takes a block over the bar deliberately
/// and requires the counter to see it, through the same `#[global_allocator]`,
/// the same threshold and the same start/stop pair — so a threshold set too
/// high, an allocator that stopped being installed, or a window that never
/// opens fails here rather than passing everywhere.
#[test]
fn the_counter_can_see_a_large_block() {
    start_counting();
    let mut deliberate: Vec<u8> = Vec::with_capacity(LARGE + 1024);
    // Touched so nothing optimises the allocation away.
    deliberate.push(1);
    let (blocks, largest) = stop_counting();

    assert_eq!(deliberate.len(), 1);
    assert!(
        blocks >= 1,
        "the counting allocator did not see a {LARGE}-byte allocation, so \
         every zero this file asserts is vacuous",
    );
    assert!(largest >= LARGE, "largest seen was {largest} B");
}
