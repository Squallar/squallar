//! **That a job's wire head is ALLOCATED at its size, not doubled into.**
//!
//! `JobRequest::to_parts` and `to_bytes` run at the dispatch site, on the
//! FRAME THREAD, and both started from a `Vec::new()`. One row in the whole
//! registry prices its own payload and reserves for it — `GriddedJob`, whose
//! encoder records the cost of not doing so: doubling from empty into a
//! multi-megabyte head copies about 133 % of the head again across ~21
//! reallocations. Every other row grew.
//!
//! The NWS-alerts row is the one this suite drives because it is the shape
//! that loses most: its head is polygon geometry written two `f64`s at a time
//! with no bulk block anywhere.
//!
//! **The window that repeats is a gesture, not a settled one.** This paragraph
//! used to say "a settled window of 138-167 overlay dispatches is 138-167
//! encodes", and that was wrong in the direction that matters: a settled pane
//! dispatches *nothing*. `squallar-app`'s
//! `an_idle_pane_asks_for_no_further_rasters_once_its_layers_hold_a_picture`
//! is the pin on it, and a census over 350 settled frames at 175 Hz recorded
//! zero encodes. What repeats is the pan — the input is memoised behind a
//! stable `Arc` while the envelope's viewport moves — and over two loops of
//! the `pan-zoom-2d` script, 7,000 frames, this row encoded 27 times from one
//! input with 26 of those writing identical bytes.
//!
//! Those 26 are gone now: `encode_cache` remembers a context-independent row's
//! bytes against its input. So the warm window below is served without running
//! the encoder at all, and the third window clears the table to keep a real
//! encode under the sizing claim.
//!
//! What replaced the `Vec::new()` is a per-wire-code high-water mark of what
//! that code has actually written, so **the first message of a kind grows
//! exactly as it always did and every later one is allocated once**. The two
//! windows below are that pair, and the second is the claim.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `squallar-radar/tests/decode_archive_not_copied.rs` gives: the growth is a
//! transient, freed inside the encode, so a level sampled before and after
//! reads the same number whether it happened or not. What can see it is the
//! allocation itself.
//!
//! **One `#[test]`**, for the reason that suite gives too: the counter and its
//! window are process-global and libtest runs a binary's tests on several
//! threads.

#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// What counts as a block worth watching. Far above anything in this encode
/// but the head itself and the growth steps that lead to it.
const LARGE: usize = 64 * 1024;

static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Bytes a `realloc` had to carry across — the copy the growth walk pays.
static MOVED: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);
static SIZES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct LargeBlocks;

impl LargeBlocks {
    fn note(size: usize, moved: usize) {
        if size < LARGE || !COUNTING.load(Ordering::Relaxed) {
            return;
        }
        LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        MOVED.fetch_add(moved, Ordering::Relaxed);
        if let Ok(mut sizes) = SIZES.lock() {
            sizes.push(size);
        }
    }
}

unsafe impl GlobalAlloc for LargeBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::note(layout.size(), 0);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::note(layout.size(), 0);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            Self::note(new_size, layout.size());
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

/// Count the large blocks `f` takes, and the bytes its reallocations carried.
fn counting<T>(f: impl FnOnce() -> T) -> (T, usize, usize, Vec<usize>) {
    SIZES.lock().expect("no poison").clear();
    LARGE_ALLOCS.store(0, Ordering::Relaxed);
    MOVED.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let out = f();
    COUNTING.store(false, Ordering::Relaxed);
    let sizes = SIZES.lock().map(|s| s.clone()).unwrap_or_default();
    (
        out,
        LARGE_ALLOCS.load(Ordering::Relaxed),
        MOVED.load(Ordering::Relaxed),
        sizes,
    )
}

/// An alerts input whose geometry is the size the transport actually moves.
///
/// **The shape is the real one and the size is a measured one; the
/// coordinates are synthetic.** `api.weather.gov/alerts/active` answered 455
/// alerts on 2026-09-08, and most of them carry no polygon at all — they name
/// UGC zones, and the layer resolves those against the zone pack before the
/// input is built. So the feed alone encodes to 41,022 B and the app's own
/// transport ledger reads 1.78-2.47 MiB per request; the difference is the
/// resolved zone rings. `zones` x `ring` below is set to land inside that
/// measured range rather than at the feed's floor, because the growth walk
/// this suite pins is a function of the head's SIZE and of nothing else.
fn an_alerts_input() -> squallar_overlays::render::rasterize::AlertsInput {
    use squallar_overlays::render::rasterize::{AlertPaint, AlertsInput};
    use squallar_source::feature::{HatchPattern, OverlayFeature};

    // 455 alerts, each one zone-shaped ring of 290 vertices: 16 B a vertex on
    // the wire.
    let zones = 455;
    let ring_len = 290;
    let alerts = (0..zones)
        .map(|z| {
            let ring: Vec<(f64, f64)> = (0..ring_len)
                .map(|v| {
                    let t = v as f64 / ring_len as f64 * std::f64::consts::TAU;
                    (35.0 + z as f64 * 0.01 + t.sin(), -97.0 + t.cos())
                })
                .collect();
            AlertPaint {
                id: format!("urn:oid:2.49.0.1.840.0.{z}"),
                category: squallar_overlays::nws::alert::AlertCategory::Warning,
                features: std::sync::Arc::new(vec![OverlayFeature::new(
                    vec![vec![ring]],
                    [255, 0, 0, 96],
                    [255, 0, 0, 255],
                    "TOR".to_string(),
                    "Tornado Warning".to_string(),
                    HatchPattern::None,
                )]),
            }
        })
        .collect();
    AlertsInput {
        alerts,
        enabled_categories: squallar_overlays::nws::alert::AlertCategory::ALL.to_vec(),
        hidden_ids: Default::default(),
        device_scale: 2.0,
    }
}

/// The envelope a pane's overlay dispatch hands the funnel.
fn a_request(
    input: squallar_overlays::render::rasterize::AlertsInput,
) -> squallar_worker::offload::JobRequest {
    squallar_worker::offload::JobRequest::describe(
        input,
        squallar_source::job::JobGeometry {
            width: 1248,
            height: 714,
            bounds: squallar_geo::GeoBounds {
                min_lat: 30.0,
                max_lat: 40.0,
                min_lon: -100.0,
                max_lon: -90.0,
            },
            side_ceiling_px: 0,
        },
    )
}

/// **The second encode of a kind allocates its head once and carries nothing.**
///
/// Floor — put `Vec::new()` back in `JobRequest::to_parts`'s `head_buffer`:
/// the warm window reads the cold window's counts instead of 1 and 0.
#[test]
fn a_repeat_dispatch_allocates_its_head_once_and_grows_nothing() {
    let request = a_request(an_alerts_input());

    // The first message of this kind. Unchanged behaviour, and the control
    // that the instrument can see a growth walk at all.
    let ((cold, cold_payload), cold_allocs, cold_moved, cold_sizes) =
        counting(|| request.to_parts());
    assert!(
        cold_payload.is_none(),
        "the alerts row has no resident payload to lend, so its whole message \
         is the head this suite is about",
    );
    assert!(
        cold.len() >= LARGE,
        "the fixture encodes to {} B, under the {LARGE} B this instrument \
         watches — every count below would be a zero that means nothing",
        cold.len(),
    );
    assert!(
        cold_allocs > 1,
        "a first encode of a kind grows into its buffer and takes several \
         blocks; this one took {cold_allocs} ({cold_sizes:?}), so the \
         instrument is not seeing the walk it is here to price",
    );
    assert!(
        cold_moved > 0,
        "the growth walk carried no bytes, so the `moved` figure below says \
         nothing",
    );

    // Every dispatch after it, which is what a pan is made of.
    let encodes_before = squallar_worker::encode_cache::totals().encoded;
    let ((warm, _), warm_allocs, warm_moved, warm_sizes) = counting(|| request.to_parts());
    assert_eq!(
        squallar_worker::encode_cache::totals().encoded,
        encodes_before,
        "the repeat dispatch ran the encoder again; `encode_cache` is supposed \
         to have served it from the first encode of this same input",
    );
    assert_eq!(
        warm, cold,
        "the sized buffer changed the bytes on the wire; it may only change \
         how they were allocated",
    );
    assert_eq!(
        warm_allocs,
        1,
        "a repeat encode of {} B took {warm_allocs} blocks of at least \
         {LARGE} B ({warm_sizes:?}); it must take exactly one — the head \
         itself, at the size the last one needed. The first encode of the \
         same bytes took {cold_allocs} ({cold_sizes:?}) and carried \
         {cold_moved} B, which is the walk this is supposed to have removed.",
        warm.len(),
    );
    assert_eq!(
        warm_moved,
        0,
        "a repeat encode carried {warm_moved} B across reallocations. The \
         cold walk carried {cold_moved} B for the same {} B head, and that is \
         the copy this sizing exists to remove — on the frame thread, once \
         per dispatch.",
        cold.len(),
    );

    // **The sizing still governs a real encode.** The window above no longer
    // runs the encoder at all, so on its own it would stop being evidence
    // about how an encode allocates. Clearing the table puts the encoder back
    // under the same high-water mark.
    squallar_worker::encode_cache::clear();
    let encodes_before = squallar_worker::encode_cache::totals().encoded;
    let ((again, _), again_allocs, again_moved, again_sizes) = counting(|| request.to_parts());
    assert_eq!(
        squallar_worker::encode_cache::totals().encoded,
        encodes_before + 1,
        "a cleared table did not fall back to the encoder, so the counts below \
         are about a copy and not about an encode",
    );
    assert_eq!(again, cold, "the re-encode wrote different bytes");
    assert_eq!(
        again_moved, 0,
        "a re-encode into a buffer sized at this code's high-water mark \
         carried {again_moved} B across reallocations ({again_sizes:?}); the \
         cold walk carried {cold_moved} B, and that is the copy this sizing \
         exists to remove",
    );
    assert_eq!(
        again_allocs, 2,
        "a re-encode took {again_allocs} blocks of at least {LARGE} B \
         ({again_sizes:?}); it must take exactly two — the head itself, at the \
         size the last one needed, and the copy `encode_cache` files so the \
         next dispatch of this input needs neither",
    );
}
