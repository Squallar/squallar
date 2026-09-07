//! **That a pooled render buffer too small for the next render is grown to what
//! that render needs, and not to twice what it already had.**
//!
//! All three plan-view slots hand their buffer to a `Vec` growth call, and all
//! three reached it through `Vec`'s **amortised** path, which takes
//! `max(2 * capacity, need)`. `within_slack` bounds a pooled buffer's capacity
//! only from *above*, so an **undersized** buffer always passed the filter and
//! always took that path — and the doubled buffer then passed `within_slack` on
//! the way back out, so it parked and stayed parked.
//!
//! The value grid reached it through arithmetic that reads as if it prevented
//! exactly this:
//!
//! ```ignore
//! values.clear();
//! values.reserve_exact(pixels.saturating_sub(values.capacity()));
//! ```
//!
//! `reserve_exact(additional)` guarantees room for `len + additional`, and
//! `len` is **zero** after the `clear`. So it asked for `pixels - capacity`
//! against a buffer already holding `capacity`, no-opped, and left the growth
//! to the `extend` in `RenderBuffers::into_output`. Measured by heaptrack on a
//! real arm: one live **400,040,000 B** allocation where the exact need was
//! **216,800,000 B**. The texture and the cell buffer had no arithmetic to get
//! wrong — they call `resize`/`resize_with`, which reserve amortised — so the
//! same defect was there in the spelling that hides it.
//!
//! Two instruments, because the defect has two halves and each hides the other:
//! the **allocation sizes** taken during the growing render (the transient),
//! and `pooled_bytes()` afterwards (what is then held for the session).
//!
//! One `#[test]`: the pools, the demand behind them and the counting window are
//! all process-global.

#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nexrad_level3::model::{RadialPacket, RadialRun};
use squallar_radar::frame::{RasterImage, RenderedFrame};
use squallar_radar::render::{
    pooled_bytes, recycle_image, render_level3_radial_to_image, trim_pools,
};
use squallar_radar::types::RadarProduct;

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const SCALE_FACTOR: f32 = 4.0;
const BINS: usize = 120;
const RADIALS: usize = 60;

/// The side the pool is warmed at.
const WARM: usize = 1000;

/// The side the growing render asks for. Chosen so the warm buffer is
/// **undersized but inside `POOL_SLACK`** — the one state that reaches the
/// amortised path — and so the doubling is unmistakable: 2 × 1000² is 84.9 %
/// over 1040²'s actual need.
const GROWN: usize = 1040;

const WARM_PX: usize = WARM * WARM;
const GROWN_PX: usize = GROWN * GROWN;

/// Bytes one pixel costs across the three slots: eight for its cell, four for
/// its RGBA texel, four for its `f32` value.
const SLOT_BYTES_PER_PX: usize = 8 + 4 + 4;

/// The largest single buffer a `GROWN` render legitimately needs: its cells, at
/// eight bytes a pixel. **The described extent** — nothing this render takes
/// has any business being larger, and a doubled buffer necessarily is.
const LARGEST_LEGITIMATE: usize = GROWN_PX * 8;

/// What counts as a block worth watching. Under the smallest of the three
/// buffers (`GROWN_PX * 4` = 4,326,400 B) so all three are seen.
const LARGE: usize = 4 * 1024 * 1024;

/// **The premise the whole file rests on, checked where it cannot drift.** The
/// warm buffer has to be too small for the growing render *and* still inside
/// `POOL_SLACK` — that pair is the one state which reaches `Vec`'s amortised
/// path, and a later edit to either side that broke it would leave every
/// assertion below passing against a render that never grew a pooled buffer at
/// all. A relation between constants belongs at compile time; the tree spells
/// it this way in `squallar-device-profile::constants`.
const _: () = const {
    assert!(WARM_PX < GROWN_PX, "the warm buffer must be undersized");
    assert!(
        WARM_PX * 2 >= GROWN_PX,
        "and still inside POOL_SLACK, or the pool would refuse it outright"
    );
    assert!(
        LARGE < GROWN_PX * 4,
        "the smallest of the three buffers is seen"
    );
};

static COUNTING: AtomicBool = AtomicBool::new(false);
static SIZES: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static SEEN: AtomicUsize = AtomicUsize::new(0);

struct LargeBlocks;

impl LargeBlocks {
    fn note(size: usize) {
        if size < LARGE || !COUNTING.load(Ordering::Relaxed) {
            return;
        }
        SEEN.fetch_add(1, Ordering::Relaxed);
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

/// Every block at or over [`LARGE`] that `f` takes, with its size.
fn counting<T>(f: impl FnOnce() -> T) -> (T, Vec<usize>) {
    SIZES.lock().expect("no poison").clear();
    COUNTING.store(true, Ordering::Relaxed);
    let out = f();
    COUNTING.store(false, Ordering::Relaxed);
    let sizes = SIZES.lock().map(|s| s.clone()).unwrap_or_default();
    (out, sizes)
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

/// One render at `side`, taken to all three of its buffers' production death
/// sites — the walk `render_pool_residency.rs` documents, and for its reason: a
/// version that skipped them would exercise one slot of three.
fn render_at(p: &RadialPacket, side: usize) {
    let out = render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None, side)
        .expect("the packet renders");
    assert_eq!(out.values.len(), side * side);
    assert_eq!(out.image.len(), side * side * 4);
    let frame = RenderedFrame::from(out);
    match frame.image {
        RasterImage::Bytes(bytes) => recycle_image(bytes),
        RasterImage::Pixels(_) => panic!("a renderer's own output is `Bytes`"),
    }
}

/// **A pooled buffer is grown to what the render needs, not to twice itself.**
///
/// Floors, in order:
/// * `checkout_values`: put `reserve_exact(pixels.saturating_sub(capacity))`
///   back — an 8,000,000 B block appears where 4,326,400 B is needed;
/// * `checkout_image`: delete its `reserve_exact(len)` — likewise;
/// * `RenderBuffers::checkout`: delete its `reserve_exact` — a 16,000,000 B
///   block appears where 8,652,800 B is needed.
///
/// Any one of the three moves `pooled_bytes` off the exact figure below.
#[test]
fn a_pooled_buffer_too_small_is_grown_to_the_need_and_not_to_twice_itself() {
    let sweep = packet();

    // ── Premise: the pool holds an UNDERSIZED buffer that is inside slack ───
    // Two renders, because the first render of a size earns no carry.
    trim_pools();
    render_at(&sweep, WARM);
    render_at(&sweep, WARM);
    assert_eq!(
        pooled_bytes(),
        WARM_PX * SLOT_BYTES_PER_PX,
        "premise: all three slots are holding a {WARM}² buffer",
    );

    // ── The reading: what the growing render takes ─────────────────────────
    let ((), sizes) = counting(|| render_at(&sweep, GROWN));
    let oversized: Vec<usize> = sizes
        .iter()
        .copied()
        .filter(|&b| b > LARGEST_LEGITIMATE)
        .collect();
    assert!(
        oversized.is_empty(),
        "the growing render took {oversized:?}, each larger than the biggest \
         buffer a {GROWN}² render needs ({LARGEST_LEGITIMATE} B of cells). \
         That is `Vec`'s amortised `max(2 * capacity, need)` doubling a pooled \
         buffer that was merely undersized. All blocks seen: {sizes:?}",
    );

    // ── The half the transient hides: what is then HELD ────────────────────
    assert_eq!(
        pooled_bytes(),
        GROWN_PX * SLOT_BYTES_PER_PX,
        "after growing to {GROWN}² the pool holds {} B where the three slots \
         cost {} B. A doubled buffer passes `within_slack` on the way back out \
         too, so it parks and stays parked for the session. Blocks seen: \
         {sizes:?}",
        pooled_bytes(),
        GROWN_PX * SLOT_BYTES_PER_PX,
    );

    // ── Non-triviality: the instrument can see a block of this class ───────
    let seen_before = SEEN.load(Ordering::Relaxed);
    let ((), control) = counting(|| {
        let block: Vec<u8> = vec![0; LARGEST_LEGITIMATE];
        std::hint::black_box(&block);
    });
    assert_eq!(
        control,
        vec![LARGEST_LEGITIMATE],
        "an explicit {LARGEST_LEGITIMATE} B allocation went unseen or came back \
         a different size, so the empty `oversized` above says nothing",
    );
    assert!(
        SEEN.load(Ordering::Relaxed) > seen_before,
        "the counter never fired at all",
    );

    trim_pools();
}
