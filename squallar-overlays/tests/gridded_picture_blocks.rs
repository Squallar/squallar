//! **How many picture-sized blocks one gridded overlay costs**, from the
//! rasterizer to the buffer the consumer's texture takes.
//!
//! An `egui::ColorImage` holds `Vec<Color32>`, and `Color32` is
//! `#[repr(align(4))]`. A `Vec` must go back to the allocator with the
//! `Layout` it came from, so a `Vec<u8>` picture can never *become* one by
//! move — `bytemuck::allocation::try_cast_vec` refuses on exactly that ground
//! — and a consumer handed bytes has no choice but to allocate a second buffer
//! the size of the picture and copy into it. At the measured desktop case,
//! 4317 x 2477, that second block is **40.79 MiB**, taken on the thread the
//! reply lands on.
//!
//! [`rasterize_gridded`] is the one producer that does not go through
//! tiny-skia, so it is the one that can decide the layout at birth. What this
//! file gates is that it does: **one picture-sized block for the whole path**,
//! and the block the consumer ends up holding is the same allocation the
//! rasterizer wrote — not a copy of it.
//!
//! Its own binary with a counting `#[global_allocator]`, on the terms
//! `gridded_projection_band.rs` sets out: the instrument counts real
//! `GlobalAlloc` calls at or above a size threshold, knows nothing about the
//! raster, and so compiles and runs against a tree where the fix is absent and
//! disagrees with it. **Observed on the byte-buffer spelling: 2 blocks.**

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ecolor::Color32;
use squallar_geo::GeoBounds;
use squallar_overlays::hrrr::GridCoords;
use squallar_overlays::render::rasterize::{
    GridWindow, GriddedInput, IndexWindow, rasterize_gridded,
};
use squallar_source::job::JobOut;

/// The texture below is 2048 x 1024 x 4 = 8 MiB; half of it is comfortably
/// above every other buffer the counted window can reach (the projection band
/// and the two rect rows are tens of kilobytes at this width) and comfortably
/// below one picture.
const LARGE: usize = 4 * 1024 * 1024;

const W: u32 = 2048;
const H: u32 = 1024;

static LARGE_BLOCKS: AtomicUsize = AtomicUsize::new(0);
/// Off outside the measured window, so the fixture's own values vector — and
/// anything the harness does — is not in the figure.
static COUNTING: AtomicBool = AtomicBool::new(false);

struct PictureBlocks;

fn note(size: usize) {
    if size >= LARGE && COUNTING.load(Ordering::Relaxed) {
        LARGE_BLOCKS.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for PictureBlocks {
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

    /// A grow past the bar counts: a `Vec` that reallocates its way up to a
    /// picture has taken a fresh block that size, whatever the call was named.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.size() < new_size {
            note(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: PictureBlocks = PictureBlocks;

const NI: usize = 700;
const NJ: usize = 350;

/// A regular grid over CONUS in the closed form the decoders build: scanning
/// origin at the north-west corner, signed steps, i-consecutive.
fn grid(values: Vec<f32>) -> GriddedInput {
    GriddedInput::Window(GridWindow {
        field: squallar_overlays::mrms::fields::spec(
            squallar_overlays::mrms::MrmsProduct::ReflectivityComposite,
        )
        .id
        .clone(),
        ni: NI,
        nj: NJ,
        coords: GridCoords::Regular {
            lat0: 54.95,
            lon0: -129.95,
            dlat: -0.1,
            dlon: 0.1,
            ni: NI,
            nj: NJ,
            scan_mode: 0,
        },
        win: IndexWindow {
            i0: 0,
            i1: NI,
            j0: 0,
            j1: NJ,
        },
        values: squallar_overlays::render::gridded::GridValues::F32(values),
    })
}

/// **One picture, one block — and the consumer holds the rasterizer's own.**
///
/// The whole path a gridded overlay takes: the raster, the run funnel's output
/// stage (`JobOut::straight_rasters_mut` then
/// `JobOut::discard_blank_rasters`, called through the trait so this is the
/// funnel's own seam and not a paraphrase of it), and
/// `RasterBuf::into_pixels`, which is what the arrival hands
/// `egui::ColorImage::new`.
///
/// **Counted, never timed.** Blocks at or above 4 MiB is a property of the
/// code, not of the machine or the load — which is what makes it a gate and
/// not a reading.
///
/// The pointer check is the other half, and it is the half a count cannot
/// make: two allocations and one allocation are both consistent with a
/// consumer that copies, if the copy is made and the original freed. Equal
/// addresses say the buffer was *transferred*.
#[test]
fn one_gridded_overlay_costs_one_picture_sized_block_and_hands_it_over() {
    // A value the composite's colour bar paints opaque, varied so the cells
    // really differ: a raster that returned early would reach the count green
    // having drawn nothing, which is what the ink assertion below refuses.
    let values: Vec<f32> = (0..NI * NJ).map(|k| 20.0 + (k % 37) as f32).collect();
    let input = grid(values);
    let bounds = GeoBounds {
        min_lon: -130.0,
        max_lon: -60.0,
        min_lat: 20.0,
        max_lat: 55.0,
    };

    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_BLOCKS.load(Ordering::Relaxed);

    let mut out = rasterize_gridded(&input, &bounds, W, H);
    let written = out.rgba.as_bytes().as_ptr() as usize;

    // The funnel's output stage, through the trait it really goes through.
    for raster in out.straight_rasters_mut() {
        for px in raster.as_chunks_mut::<4>().0 {
            let c = Color32::from_rgba_unmultiplied(px[0], px[1], px[2], px[3]);
            px.copy_from_slice(&c.to_array());
        }
    }
    out.discard_blank_rasters();
    assert_eq!(
        out.blank, None,
        "the fixture must paint, or every figure below is a reading of an \
         empty picture",
    );

    // What the arrival gives `egui::ColorImage::new`.
    let pixels = out.rgba.into_pixels();
    let took = LARGE_BLOCKS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);

    assert_eq!(
        pixels.len(),
        (W * H) as usize,
        "the picture the consumer holds is the texture's own size",
    );
    assert!(
        pixels.iter().any(|px| px.a() != 0),
        "the fixture painted nothing; the block count above is a reading of an \
         empty raster and proves nothing",
    );

    assert_eq!(
        took,
        1,
        "one gridded overlay took {took} picture-sized blocks. One is the \
         raster itself; a second is the copy a byte buffer forces on a \
         consumer that holds `Vec<Color32>` — {} MiB at this texture and \
         40.79 MiB at the 4317x2477 desktop case, on the thread the reply \
         lands on",
        (W as usize * H as usize * 4) >> 20,
    );

    assert_eq!(
        pixels.as_ptr() as usize,
        written,
        "the consumer's buffer is not the one the rasterizer wrote. A count of \
         one block is also what a copy-and-free looks like; equal addresses \
         are what say the picture was handed over rather than reproduced",
    );
}
