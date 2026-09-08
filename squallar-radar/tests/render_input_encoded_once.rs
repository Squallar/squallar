//! **That a job carrying a `RenderInput` writes its gate bytes ONCE.**
//!
//! The three rows that carry one — `radar`, `section`, `voxels` — framed it as
//! `out.extend_from_slice(&input.input.to_bytes())`. That allocates a whole
//! second copy of the payload and memcpy's it into the message: a
//! `RenderInput` "owns its gate bytes and is the largest thing in the request
//! by three orders of magnitude", in its own struct's words, and the framing
//! ran at the dispatch site — on the FRAME THREAD, inside
//! `JobRequest::to_parts`.
//!
//! `RenderInput::write_bytes` is the same encoder over a buffer the caller
//! already owns, so the second copy has nowhere to be. The bytes are
//! unchanged, and the first assertion below is that they are.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `tests/decode_archive_not_copied.rs` gives: the second copy is a transient,
//! freed as the encode returns, so a level sampled before and after reads the
//! same number whether it happened or not. What can see it is the allocation.
//!
//! **One `#[test]`**, for the reason that suite gives too: the counter and its
//! window are process-global and libtest runs a binary's tests on several
//! threads.

#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nexrad_model::data::{
    MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
};
use squallar_radar::render_input::RenderInput;
use squallar_radar::types::RadarProduct;
use squallar_source::job::JobSpec;

/// A super-res reflectivity sweep's shape. Four of them put ~2.6 MB of gate
/// bytes in the payload, which is the order the app's own transport ledger
/// reads per request.
const N_RADIALS: usize = 720;
const N_GATES: usize = 1832;

/// What counts as a block worth watching: a quarter of the gate bytes. The
/// only things in this encode that come near it are the message and the
/// duplicate this suite is about.
const LARGE: usize = N_RADIALS * N_GATES / 4;

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

/// Count the large blocks `f` takes.
fn counting<T>(f: impl FnOnce() -> T) -> (T, usize, Vec<usize>) {
    SIZES.lock().expect("no poison").clear();
    LARGE_ALLOCS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let out = f();
    COUNTING.store(false, Ordering::Relaxed);
    let sizes = SIZES.lock().map(|s| s.clone()).unwrap_or_default();
    (out, LARGE_ALLOCS.load(Ordering::Relaxed), sizes)
}

fn vcp() -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        212,
        0,
        0.5,
        PulseWidth::Short,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        Vec::new(),
    )
}

fn a_sweep(elevation_number: u8, elevation_deg: f32) -> Sweep {
    let spacing = 360.0 / N_RADIALS as f32;
    let radials = (0..N_RADIALS)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                (i as f32 * spacing) % 360.0,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                Some(MomentData::from_fixed_point(
                    N_GATES as u16,
                    2125,
                    250,
                    8,
                    2.0,
                    66.0,
                    vec![(i % 200) as u8 + 2; N_GATES],
                )),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    Sweep::new(elevation_number, radials)
}

/// **A volume the whole-volume extractor keeps every sweep of**, so the
/// payload is the size a `voxels` or `section` dispatch really carries.
fn a_volume() -> Scan {
    Scan::new(
        vcp(),
        (1..=4u8)
            .map(|k| a_sweep(k, 0.5 + 0.9 * f32::from(k - 1)))
            .collect(),
    )
}

/// **The gate bytes are written once, and the same bytes as before.**
///
/// Floor — put `out.extend_from_slice(&input.input.to_bytes())` back in
/// `RadarPlanJob::encode`: the count below reads 2 instead of 1, the extra
/// block being a whole duplicate payload taken on the frame thread.
#[test]
fn a_radar_job_frames_its_render_input_without_copying_it() {
    let scan = a_volume();
    let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, 35.33, -97.28)
        .expect("a reflectivity volume extracts");
    let payload_len = input.to_bytes().len();
    assert!(
        payload_len >= 2 * LARGE,
        "the fixture's payload is {payload_len} B, too small for the \
         {LARGE} B block class this instrument watches — every count below \
         would be a zero that means nothing",
    );

    let job = squallar_radar::jobs::RadarPlanJob {
        input: Box::new(input),
        values_wanted: false,
        surface: squallar_radar::jobs::PlanSurface::Raster,
    };
    let ctx = squallar_source::job::EncodeCtx {
        geometry: squallar_source::job::JobGeometry {
            width: 0,
            height: 0,
            bounds: squallar_geo::GeoBounds {
                min_lat: 0.0,
                max_lat: 0.0,
                min_lon: 0.0,
                max_lon: 0.0,
            },
            side_ceiling_px: 4096,
        },
    };

    // Reserved exactly, so the destination's own allocation is one block and
    // any second one is the duplicate.
    let mut out = Vec::with_capacity(payload_len + 64);
    let ((), allocs, sizes) =
        counting(|| <squallar_radar::jobs::RadarPlanJob as JobSpec>::encode(&job, &ctx, &mut out));
    assert_eq!(
        allocs, 0,
        "framing a `RenderInput` into a buffer that was already large enough \
         took {allocs} block(s) of at least {LARGE} B: {sizes:?}. Each one is \
         a whole duplicated payload, taken at the dispatch site on the frame \
         thread.",
    );

    // Control: the instrument can see a block of this class.
    let ((), control, control_sizes) = counting(|| {
        let block = vec![0u8; payload_len];
        std::hint::black_box(&block);
    });
    assert_eq!(
        control, 1,
        "an explicit payload-sized allocation went unseen, so the figure \
         above says nothing: {control_sizes:?}",
    );

    // And the bytes are the ones that always crossed: the two spellings of
    // the encoder write the same message.
    assert_eq!(
        &out[2..],
        &job.input.to_bytes()[..],
        "`write_bytes` and `to_bytes` disagree, so the wire moved",
    );
    assert_eq!(&out[..2], &[0u8, 0u8], "the row's own two flag bytes");
}
