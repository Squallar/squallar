//! **That the per-frame idle hook actually reaches the staging pools' release
//! lever**, watched at the allocator and through the shipped registry.
//!
//! `StagingPool::release_retained` was written for two callers and, until the
//! change this suite pins, had neither. Its own doc names them: "an idle policy
//! in the layer that owns the source, and a memory governor's tier-2 pressure
//! step". This is the first — `SourceHandler::release_data`, which
//! `Gui::release_data_of_layers_no_pane_draws` calls once a frame for every
//! layer no pane has enabled.
//!
//! `gmgsi_staging_release.rs` already proves the lever hands the block back.
//! What it cannot prove is that anything pulls it, and a lever nothing pulls is
//! the whole defect: two sources retain a decode buffer apiece — GMGSI a
//! 15,000,000 B mosaic and MRMS a 224,000 B band — resident whether or not
//! anything is decoding. So this
//! suite goes through `OverlayRegistry::default()`, the same construction the
//! application ships, and asks the handlers by their `LayerId`.
//!
//! Its own binary with a counting `#[global_allocator]`, and **one test**, for
//! the reasons the sibling suites give: the shipped slots are process-global,
//! so a second test in the same binary would be racing this one's window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::id::known;

/// Under both parked blocks and over everything else either release touches.
///
/// **It was `12 * 1024 * 1024`**, chosen to sit under 15,000,000 B and
/// 49,000,000 B when both slots held a whole mosaic. MRMS's slot holds a 16-row
/// **band** now — `decode::tile_png_codes` tiles out of the PNG row walk and no
/// plane is built for it to keep — so a 12 MiB bar counts GMGSI's release and
/// reads **zero blocks freed** for MRMS's, which is exactly what a lever
/// nothing pulls looks like. Stated as the smaller of the two slots, off its
/// own constant, so a slot that changes shape again carries this with it.
const LARGE: usize = squallar_overlays::mrms::CONUS_BAND_BYTES;

static LARGE_FREES: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);
static FREED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct LargeBlocks;

unsafe impl GlobalAlloc for LargeBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if layout.size() >= LARGE && COUNTING.load(Ordering::Relaxed) {
            LARGE_FREES.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut freed) = FREED.lock() {
                freed.push(layout.size());
            }
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

/// Count the large blocks `f` hands back, and their sizes.
fn counting<T>(f: impl FnOnce() -> T) -> (T, usize, Vec<usize>) {
    FREED.lock().expect("no poison").clear();
    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_FREES.load(Ordering::Relaxed);
    let out = f();
    let frees = LARGE_FREES.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);
    let freed = FREED.lock().map(|f| f.clone()).unwrap_or_default();
    (out, frees, freed)
}

/// **A layer no pane draws gives its parked mosaic back to the allocator.**
///
/// Floor — delete `release_data` from either handler: that source's `released`
/// reads `false`, its free count reads 0 instead of 1, and its
/// `retained_bytes()` stays at the mosaic.
#[test]
fn the_idle_hook_releases_both_sources_retained_mosaics() {
    const GMGSI_MOSAIC: usize = squallar_overlays::gmgsi::staging::STAGING_POINTS * size_of::<u8>();
    let mrms_mosaic = squallar_overlays::mrms::staging::STAGING_POINTS
        * squallar_overlays::mrms::staging::StagingPool::ELEMENT_BYTES;

    let mut registry = OverlayRegistry::default();

    // Park one mosaic in each shipped slot, through the pools' own doors and at
    // the capacity each slot retains — the state an evicted granule leaves.
    let gmgsi_pool = squallar_overlays::gmgsi::staging::global();
    let mrms_pool = squallar_overlays::mrms::staging::global();
    gmgsi_pool.give(
        gmgsi_pool
            .take(squallar_overlays::gmgsi::staging::STAGING_POINTS)
            .expect("a GMGSI mosaic fits on a test host"),
    );
    mrms_pool.give(
        mrms_pool
            .take(squallar_overlays::mrms::staging::STAGING_POINTS)
            .expect("a MRMS mosaic fits on a test host"),
    );
    assert_eq!(
        (gmgsi_pool.retained_bytes(), mrms_pool.retained_bytes()),
        (GMGSI_MOSAIC, mrms_mosaic),
        "premise: both shipped slots are holding a mosaic",
    );

    for (id, mosaic, name) in [
        (known::GMGSI, GMGSI_MOSAIC, "GMGSI"),
        (known::MRMS, mrms_mosaic, "MRMS"),
    ] {
        let (released, frees, freed) = counting(|| {
            registry
                .get_handler_mut(&id)
                .expect("the shipped registry carries this layer")
                .release_data()
        });
        assert!(
            released,
            "{name}: the idle hook must find the parked mosaic and say so — a \
             `false` here is a layer that answers 'nothing to release' with a \
             whole grid parked",
        );
        assert_eq!(
            frees, 1,
            "{name}: releasing must hand exactly one block back to the \
             allocator. A hook that only cleared the pool's own figure would \
             leave the mosaic resident with nothing left able to reclaim it, \
             and `retained_bytes` would read zero either way. Freed: {freed:?}",
        );
        assert_eq!(
            freed,
            vec![mosaic],
            "{name}: and the block freed must be the mosaic itself",
        );
    }

    assert_eq!(
        (gmgsi_pool.retained_bytes(), mrms_pool.retained_bytes()),
        (0, 0),
        "both pools' own levels must agree with the allocator",
    );

    // The hook runs every frame. A frame that finds nothing must cost nothing
    // and must say so, or the caller cannot be allowed to call it every frame.
    for (id, name) in [(known::GMGSI, "GMGSI"), (known::MRMS, "MRMS")] {
        let (again, idle_frees, _) = counting(|| {
            registry
                .get_handler_mut(&id)
                .expect("the shipped registry carries this layer")
                .release_data()
        });
        assert!(!again, "{name}: a second frame has nothing to find");
        assert_eq!(idle_frees, 0, "{name}: and costs the allocator nothing");
    }

    // Non-triviality: the instrument can see a free of the class measured.
    let ((), control_frees, control) = counting(|| {
        let mut mosaic: Vec<u8> = Vec::new();
        mosaic
            .try_reserve_exact(GMGSI_MOSAIC)
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
