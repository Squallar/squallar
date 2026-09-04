//! **How much of a grid `rasterize_gridded` projects at once when the whole
//! grid is on the glass.**
//!
//! Zooming out is what puts it there. [`projection_window`] narrows to the cells
//! that can reach the texture, so a tight view over Oklahoma projects a few
//! thousand points; a world view over a CONUS mosaic projects all **24 500 000**
//! of them. The pre-projection buffer is a `(f32, f32)` per point, so that
//! window costs **196 MB in one infallible `Vec::with_capacity`** — on wasm32,
//! against the 1 GiB module ceiling, with `panic-strategy = "abort"` underneath,
//! that is `handle_alloc_error` and the trap that follows it.
//!
//! The cell loop never reads more than three rows at once: sizing a cell reads
//! `(i-1, j)`, `(i+1, j)`, `(i, j-1)` and `(i, j+1)` and nothing else. So the
//! whole-window buffer was never needed, and what this file gates is that no
//! single allocation in the raster scales with the window's *area*.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reasons
//! `mrms_staging_blocks.rs` sets out: the instrument counts real
//! `GlobalAlloc::alloc` calls at or above a size threshold and knows nothing
//! about the raster, so it compiles and runs against the **unmodified** tree and
//! can disagree with the fix.
//!
//! 64 MiB is the threshold because the texture this raster writes is 512 x 512 x
//! 4 = 1 MB and the values it reads are handed in before the window opens, so
//! the only thing left above the bar is the projection buffer itself.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use squallar_geo::GeoBounds;
use squallar_overlays::hrrr::GridCoords;
use squallar_overlays::render::rasterize::{
    GridWindow, GriddedInput, IndexWindow, rasterize_gridded,
};

/// Blocks at or above this are window-area-sized rather than texture-sized. See
/// the header for why nothing else in this test can reach it.
const LARGE: usize = 64 * 1024 * 1024;

static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Off outside the measured window, so the 98 MB values vector the fixture hands
/// in — and anything the harness itself does — is not in the figure.
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

    /// A grow past the bar counts: a `Vec` that reallocates its way up to the
    /// window's area has taken a fresh block that size, whatever the call was
    /// named.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size >= LARGE && layout.size() < new_size && COUNTING.load(Ordering::Relaxed) {
            LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

/// The CONUS mosaic's shape, from `squallar_overlays::mrms`: 0.01 degree
/// spacing over 70 degrees of longitude and 35 of latitude.
const NI: usize = 7000;
const NJ: usize = 3500;
const POINTS: usize = NI * NJ;

/// A value the composite's colour bar paints opaque, so the cell loop reaches
/// its four neighbour reads rather than skipping at the alpha test. A gate over
/// a loop that returned early would be measuring nothing.
const PAINTED_DBZ: f32 = 30.0;

/// The mosaic's own grid, in the closed form the decoder builds
/// (`mrms::decode::regular_coords`): scanning origin at the north-west corner,
/// signed steps, i-consecutive.
fn mosaic_coords() -> GridCoords {
    GridCoords::Regular {
        lat0: 54.995,
        lon0: -129.995,
        dlat: -0.01,
        dlon: 0.01,
        ni: NI,
        nj: NJ,
        scan_mode: 0,
    }
}

/// The whole mosaic on the glass — the window a zoomed-out view produces, here
/// carried explicitly so the figure does not depend on the projection maths.
fn whole_mosaic(values: Vec<f32>) -> GriddedInput {
    GriddedInput::Window(GridWindow {
        field: squallar_overlays::mrms::fields::spec(
            squallar_overlays::mrms::MrmsProduct::ReflectivityComposite,
        )
        .id
        .clone(),
        ni: NI,
        nj: NJ,
        coords: mosaic_coords(),
        win: IndexWindow {
            i0: 0,
            i1: NI,
            j0: 0,
            j1: NJ,
        },
        values: squallar_overlays::render::gridded::GridValues::F32(values),
    })
}

/// **A whole CONUS mosaic rasterises without one window-sized allocation.**
///
/// This is the user-visible shape of it: MRMS enabled, zoomed out until the
/// mosaic fits the viewport. Nothing about the texture changes with zoom — it is
/// 512 x 512 here and 2878 x 1566 in the browser either way — and nothing about
/// the values changes either. The only thing that grows is the index window, and
/// before this gate the projection buffer grew with it, squared.
///
/// **Counted, never timed.** The figure is `alloc` calls at or above 64 MiB,
/// which is a property of the code rather than of the machine or the load.
///
/// **Observed red on the unmodified tree (`0d9fac6b`): 1 block of 196 MB.**
#[test]
fn a_whole_mosaic_on_the_glass_takes_no_window_sized_block() {
    // Outside the window on purpose: the values are the source's, not the
    // raster's, and the raster is what is being measured.
    let input = whole_mosaic(vec![PAINTED_DBZ; POINTS]);
    let bounds = GeoBounds {
        min_lon: -130.0,
        max_lon: -60.0,
        min_lat: 20.0,
        max_lat: 55.0,
    };

    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_ALLOCS.load(Ordering::Relaxed);
    let out = rasterize_gridded(&input, &bounds, 512, 512);
    let took = LARGE_ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);

    assert_eq!(
        took,
        0,
        "rasterising a {NI}x{NJ} window took {took} window-sized blocks off the \
         allocator. The cell loop reads row j and its two neighbours and nothing \
         else, so the projection is three rows wide however far the view is \
         zoomed out; a buffer that scales with the window's area is {} MB here \
         and an infallible allocation against wasm32's 1 GiB ceiling",
        POINTS * std::mem::size_of::<(f32, f32)>() / 1_000_000,
    );

    // The raster still painted. A fix that returned early would reach the
    // assertion above green having drawn nothing.
    assert!(
        out.rgba.iter().any(|&b| b != 0),
        "the mosaic rasterised to an entirely blank texture, so the zero above \
         is a measurement of a raster that did not run",
    );

    // ── Non-triviality: the instrument can still count ────────────────────
    // A gate that had quietly stopped counting would reach both assertions
    // above green whatever the raster did.
    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_ALLOCS.load(Ordering::Relaxed);
    let projected: Vec<(f32, f32)> = Vec::with_capacity(POINTS);
    let seen = LARGE_ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);
    drop(projected);
    assert_eq!(
        seen, 1,
        "an explicit window-sized projection buffer must register as one large \
         block; it did not, so the zero above says nothing",
    );
}
