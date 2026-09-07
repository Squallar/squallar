//! **Does the plan-view render pool PARK a large raster, or THRASH on it?**
//!
//! The two want opposite fixes, and `render_pool_residency.rs` pins each half
//! in isolation without ever putting them together: a size rendered twice is
//! carried, and twenty renders at a quarter of the pixels retire it. What the
//! application actually does is neither — it **interleaves** them. A still pane
//! renders at the device ceiling roughly once per volume per pane, and loop
//! frames render at `LOOP_IMAGE_SIZE` continuously in between.
//!
//! `demand`'s window is eight renders over two live generations, so the
//! question is arithmetic on that window and this file is the measurement of
//! it. Both instruments are here because either alone is ambiguous:
//!
//! * **`pooled_bytes()`** — what the slots hold. It answers "parked".
//! * **large allocations, counted at the allocator** — a slot that is empty
//!   because its buffer was dropped and a slot that is empty because it was
//!   never filled read identically in `pooled_bytes`. Only the allocation count
//!   separates "the pool is small" from "the pool is being rebuilt".
//!
//! **The sensitivity arm is not optional.** A null from an instrument never
//! shown to respond is not a null, so this file first drives a case that must
//! thrash overwhelmingly and a case that must park completely, and shows the
//! two figures separate them, before reading the interleave.
//!
//! One `#[test]`: the pools and the demand behind them are process-wide, and a
//! second test in this binary would be reading this one's window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nexrad_level3::model::{RadialPacket, RadialRun};
use squallar_radar::frame::{RasterImage, RenderedFrame};
use squallar_radar::render::{
    pooled_bytes, recycle_image, render_level3_radial_to_image, trim_pools,
};
use squallar_radar::types::{IMAGE_SIZE, RadarProduct};

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const SCALE_FACTOR: f32 = 4.0;
const BINS: usize = 120;
const RADIALS: usize = 60;

/// The still side. `types::IMAGE_SIZE`, standing in for the desktop ceiling —
/// the rule under test is a function of the ratio and of `capacity`, never of
/// an absolute figure, and this runs in a fraction of the time 7362² would.
const STILL: usize = IMAGE_SIZE;

/// The loop side. What a browser draws its loop frames at.
const LOOP: usize = 1024;

/// The still:loop ratios swept, in loop frames between one still render.
/// `demand` keeps two generations of `DEMAND_WINDOW_RENDERS` = 8, so the
/// interesting range is either side of sixteen.
const RATIOS: [usize; 6] = [4, 8, 12, 16, 20, 32];

/// Cycles of the interleave to read.
const CYCLES: usize = 6;

/// Bytes one pixel costs across the three slots: eight for its cell, four for
/// its RGBA texel, four for its `f32` value.
const SLOT_BYTES_PER_PX: usize = 8 + 4 + 4;

/// What counts as a block worth watching: half a still render's cell buffer.
/// Above every block a `LOOP`-sided render takes (its cells are
/// `1024² × 8` = 8 MiB) and below the still's own (`2048² × 8` = 32 MiB).
const LARGE: usize = STILL * STILL * 8 / 2;

static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);
static SIZES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct LargeBlocks;

impl LargeBlocks {
    fn note(size: usize) {
        if size < LARGE || !COUNTING.load(Ordering::Relaxed) {
            return;
        }
        LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut sizes) = SIZES.lock() {
            sizes.push(size);
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

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            Self::note(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

/// Count the still-sized blocks `f` takes.
fn counting<T>(f: impl FnOnce() -> T) -> (T, usize, Vec<usize>) {
    SIZES.lock().expect("no poison").clear();
    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_ALLOCS.load(Ordering::Relaxed);
    let out = f();
    let took = LARGE_ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);
    let sizes = SIZES.lock().map(|s| s.clone()).unwrap_or_default();
    (out, took, sizes)
}

fn packet() -> RadialPacket {
    let radials = (0..RADIALS)
        .map(|i| RadialRun {
            start_angle: i as f32 * (360.0 / RADIALS as f32),
            angle_delta: 360.0 / RADIALS as f32,
            gate_values: (0..BINS)
                .map(|j| {
                    let dbz =
                        20.0 + (j as f64 / 30.0).sin() * 25.0 + (i as f64 / 45.0).cos() * 15.0;
                    ((dbz * SCALE as f64 + OFFSET as f64).round() as i64).clamp(2, 250) as u16
                })
                .collect(),
        })
        .collect();
    RadialPacket {
        first_range_bin: 0,
        num_range_bins: BINS as u16,
        i_center: 0,
        j_center: 0,
        scale_factor: SCALE_FACTOR,
        is_legacy: false,
        xdr_data_scale: None,
        xdr_data_offset: None,
        radials,
    }
}

/// One render at `side`, taken to both of its buffers' production death sites —
/// the same walk `render_pool_residency.rs` documents and for the same reason:
/// a version that skipped them would exercise one slot of three.
fn render_at(p: &RadialPacket, side: usize) {
    let out = render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None, side)
        .expect("the packet renders");
    assert_eq!(out.values.len(), side * side);
    let frame = RenderedFrame::from(out);
    match frame.image {
        RasterImage::Bytes(bytes) => recycle_image(bytes),
        RasterImage::Pixels(_) => panic!("a renderer's own output is `Bytes`"),
    }
}

#[test]
fn the_pool_under_a_still_and_loop_interleave_is_measured_not_assumed() {
    let sweep = packet();
    let still_slots = STILL * STILL * SLOT_BYTES_PER_PX;

    // ── Sensitivity 1: a case that MUST park ────────────────────────────────
    // Nothing but still renders. The second one onwards has demand behind it,
    // so the slots carry and no later render allocates a still-sized block.
    trim_pools();
    render_at(&sweep, STILL);
    render_at(&sweep, STILL);
    let parked = pooled_bytes();
    let ((), settled_allocs, settled_sizes) = counting(|| {
        for _ in 0..CYCLES {
            render_at(&sweep, STILL);
        }
    });
    assert_eq!(
        parked, still_slots,
        "premise: a size rendered twice is carried, so this arm really is the \
         parked case ({parked} of {still_slots})",
    );
    assert_eq!(
        settled_allocs, 0,
        "a settled still-only session re-allocated {settled_allocs} still-sized \
         block(s) {settled_sizes:?} — the parked case is supposed to allocate \
         none, so the instrument below cannot tell parking from thrashing",
    );

    // ── Sensitivity 2: a case that MUST thrash ──────────────────────────────
    // Alternating sides at a 4x ratio, one still between every loop frame. The
    // still's own buffer is never within the loop's slack, so it is dropped
    // every time and re-taken every time.
    trim_pools();
    let ((), thrash_allocs, thrash_sizes) = counting(|| {
        for _ in 0..CYCLES {
            render_at(&sweep, STILL);
            render_at(&sweep, LOOP);
        }
    });
    assert!(
        thrash_allocs >= CYCLES,
        "the deliberately-thrashing walk allocated {thrash_allocs} still-sized \
         blocks over {CYCLES} cycles {thrash_sizes:?}. Fewer than one per cycle \
         means this instrument cannot see a thrash, and a low reading from the \
         interleave below would say nothing at all",
    );

    // ── The reading: the still:loop ratio swept across the window ───────────
    //
    // `demand` keeps two generations of eight renders, so a still's size is
    // forgotten only after between eight and sixteen renders with no still in
    // them. The application's ratio is not one number — a pane re-renders its
    // still on every map movement and its loop runs at
    // `DEFAULT_LOOP_SPEED_FPS` in between — so what this reports is where the
    // behaviour changes, not one verdict.
    let mut table = Vec::new();
    for loop_frames in RATIOS {
        trim_pools();
        // The first still of a size earns no carry, so start from a session
        // that has already shown it once.
        render_at(&sweep, STILL);
        let mut parked_after_still = Vec::new();
        let ((), allocs, _) = counting(|| {
            for _ in 0..CYCLES {
                for _ in 0..loop_frames {
                    render_at(&sweep, LOOP);
                }
                render_at(&sweep, STILL);
                parked_after_still.push(pooled_bytes());
            }
        });
        table.push((loop_frames, allocs, parked_after_still.clone()));
        println!(
            "loop frames per still = {loop_frames:>2}: {allocs:>2} still-sized \
             allocations over {CYCLES} cycles, pooled_bytes after each still = \
             {parked_after_still:?}"
        );
    }
    println!("still slots at {STILL}px = {still_slots} B");

    // **Both regimes exist**, and the transition is inside the window's own
    // arithmetic. Without this the file would report one number and imply it
    // was the answer for every session.
    //
    // Classified on `pooled_bytes` after every still of the run, not on the
    // allocation count: the counted window contains the walk's own warm-up, so
    // a parked run still shows the two or three blocks it was built from. What
    // separates the regimes is whether the slots are full at every still (the
    // buffer is carried) or empty at every still (it was dropped and will be
    // taken again).
    let parks: Vec<usize> = table
        .iter()
        .filter(|(_, _, cycles)| cycles.iter().all(|&b| b >= still_slots))
        .map(|(n, _, _)| *n)
        .collect();
    let rebuilds: Vec<usize> = table
        .iter()
        .filter(|(_, allocs, cycles)| cycles.iter().all(|&b| b == 0) && *allocs >= CYCLES)
        .map(|(n, _, _)| *n)
        .collect();
    println!("parks at {parks:?} loop frames per still; rebuilds at {rebuilds:?}");
    assert!(
        !parks.is_empty(),
        "no ratio parked the still raster, so the retention this lane was sent \
         to release does not happen at any ratio measured: {table:?}",
    );
    assert!(
        !rebuilds.is_empty(),
        "no ratio rebuilt the still raster, so the demand window never decays \
         and `POOL_SLACK`/`DEMAND_WINDOW_RENDERS` do nothing at any ratio \
         measured: {table:?}",
    );
    assert!(
        parks.iter().max() < rebuilds.iter().min(),
        "the two regimes interleave rather than separating at a boundary \
         ({parks:?} against {rebuilds:?}), so the behaviour is not a function \
         of how much history a still's size survives in: {table:?}",
    );
    assert!(
        parks.len() + rebuilds.len() >= table.len() - 1,
        "more than one ratio in {table:?} is neither wholly parked nor wholly \
         rebuilt; a third shape means the mechanism is not the two-generation \
         window",
    );
    trim_pools();
}
