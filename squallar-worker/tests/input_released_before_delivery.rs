//! **That a job's INPUT is freed before its reply is delivered.**
//!
//! Both native run sites used to hold the request across the delivery:
//!
//! ```ignore
//! super::deliver_job_reply(id, run(kind, &request));  // pool::lane_job
//! deliver(execute(&request))                          // offload::run_here
//! ```
//!
//! In both, `request` drops at the end of the statement — *after* `deliver`
//! has run. And `deliver` is where the frame's picture is allocated (an
//! `egui::ColorImage`; `offload::deliver_job_reply` records 206.75 MiB at the
//! 7362 px desktop ceiling). So the peak across the call was `input + output`
//! where `max(input, output)` was available for a `drop`.
//!
//! **The claim is a level at one instant, so that is what this measures**: the
//! live heap at the moment `deliver` is entered. Peak-above-entry over the
//! whole call would answer a different question — it is dominated by the
//! rasterizer's own transients, which this change does not touch.
//!
//! **Two input sizes, and the pair is the point.** The output is a raster of
//! FIXED geometry (`a_request` pins 1248x714) and does not vary with the zone
//! count, so the only thing that differs between the two legs is the input.
//! That gives one figure that MUST scale and one that MUST NOT:
//!
//! - the level at dispatch scales with the input — the control, without which
//!   a flat reading below would mean only that the instrument is blind;
//! - the level at `deliver` does not — the claim.
//!
//! A flat figure is the PASS here, which is exactly why the scaling control is
//! not optional.
//!
//! Its own binary with a `#[global_allocator]`, for the reason
//! `job_head_allocated_once.rs` gives. **It counts every block, not just large
//! ones**: this input is 455 rings of 290 vertices, 4,640 B each, so a 64 KiB
//! large-block filter — the idiom that suite uses — would read zero for the
//! very bytes the claim is about.
//!
//! **One `#[test]`**: the level is process-global and libtest runs a binary's
//! tests on several threads.

#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

/// Live bytes: every block this process holds, summed, while `WATCHING`.
static LIVE: AtomicIsize = AtomicIsize::new(0);
static WATCHING: AtomicBool = AtomicBool::new(false);

struct Live;

unsafe impl GlobalAlloc for Live {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCHING.load(Ordering::Relaxed) {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if WATCHING.load(Ordering::Relaxed) {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if WATCHING.load(Ordering::Relaxed) {
            LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if WATCHING.load(Ordering::Relaxed) {
            LIVE.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Live = Live;

fn live() -> isize {
    LIVE.load(Ordering::Relaxed)
}

/// An alerts input of `zones` rings, 290 vertices each. The shape is
/// `job_head_allocated_once.rs`'s measured one; only the count is a knob here.
fn an_alerts_input(zones: usize) -> squallar_overlays::render::rasterize::AlertsInput {
    use squallar_overlays::render::rasterize::{AlertPaint, AlertsInput};
    use squallar_source::feature::{HatchPattern, OverlayFeature};

    let ring_len = 290;
    let alerts = (0..zones)
        .map(|z| {
            let ring: Vec<(f64, f64)> = (0..ring_len)
                .map(|v| {
                    let t = v as f64 / ring_len as f64 * std::f64::consts::TAU;
                    (35.0 + z as f64 * 0.001 + t.sin(), -97.0 + t.cos())
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

/// The envelope a pane's overlay dispatch hands the funnel. The geometry is
/// FIXED across both legs, so the output does not vary with the input.
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

/// What one dispatch recorded.
struct Leg {
    /// Live bytes the built input itself added, on the dispatching thread.
    input_bytes: isize,
    /// Live bytes at the instant `deliver` was entered, above the level before
    /// the input was built.
    at_deliver: isize,
    /// `inputs_released_before_delivery` across this dispatch.
    released: u64,
    /// `inputs_solely_owned_at_release` across this dispatch.
    solely_owned: u64,
}

/// Dispatch one job of `zones` alerts through the funnel and report the level
/// at its delivery. Natively `offload_job`'s default sink is the process pool
/// (`offload::default_sink`), so this runs the real `pool::lane_job`.
///
/// `co_owned` keeps a clone of the `DescribedJob` for the length of the run —
/// **the production shape for five overlay layers**, whose `JobMemo` rows hold
/// the dispatched job across its own execution
/// (`squallar-overlays/src/render/signature_memo.rs`).
fn a_leg(zones: usize, co_owned: bool) -> Leg {
    let (done, finished) = std::sync::mpsc::channel();

    WATCHING.store(true, Ordering::Relaxed);
    let before_input = live();
    let request = a_request(an_alerts_input(zones));
    let input_bytes = live() - before_input;
    // Held across the whole dispatch, exactly as a memo row would.
    let memo = co_owned.then(|| request.job.clone());

    let released_before = squallar_worker::offload::inputs_released_before_delivery();
    let owned_before = squallar_worker::offload::inputs_solely_owned_at_release();
    squallar_worker::offload::offload_job(
        "alerts",
        squallar_worker::offload::Job::Described(request),
        move |result| {
            // The level at the one instant the claim is about: the reply is in
            // hand and the picture is about to be built.
            let _ = done.send((live(), result.is_some()));
        },
    );
    let (at_deliver, produced) = finished.recv().expect("the job's deliver never ran");
    let released = squallar_worker::offload::inputs_released_before_delivery() - released_before;
    let solely_owned = squallar_worker::offload::inputs_solely_owned_at_release() - owned_before;
    drop(memo);
    WATCHING.store(false, Ordering::Relaxed);

    assert!(
        produced,
        "the {zones}-zone job answered nothing, so it never reached `execute` \
         and every level below is of an empty run",
    );
    Leg {
        input_bytes,
        at_deliver: at_deliver - before_input,
        released,
        solely_owned,
    }
}

/// **A job's input is not resident when its reply is delivered — and the
/// counter says whether that freed anything.**
///
/// Floor — put `super::deliver_job_reply(id, run(kind, &request));` back in
/// `pool::lane_job`: `at_deliver` gains the whole input on each sole-owned leg
/// and the flatness assertion fails by the input delta.
#[test]
fn a_jobs_input_is_freed_before_its_reply_is_delivered() {
    let small = a_leg(455, false);
    let big = a_leg(2275, false);
    let shared = a_leg(2275, true);

    // The release happened on every leg — the path these figures are about was
    // taken at all.
    assert_eq!(
        (small.released, big.released, shared.released),
        (1, 1, 1),
        "the release counter read {} / {} / {} across three dispatches; a \
         reading of 0 means neither run site was taken and every level below \
         says nothing about this change",
        small.released,
        big.released,
        shared.released,
    );

    // The control: the instrument can see this input at all, and the two
    // sole-owned legs really do differ by a large number of bytes.
    let input_delta = big.input_bytes - small.input_bytes;
    assert!(
        small.input_bytes > 1_000_000 && input_delta > 4_000_000,
        "the fixture's inputs measured {} B and {} B (delta {input_delta} B); \
         the legs must differ by enough input for the delivery-level \
         comparison below to mean anything",
        small.input_bytes,
        big.input_bytes,
    );

    // The claim: with the lane the only owner, the level at `deliver` does NOT
    // carry the input, so the two legs read alike however far apart their
    // inputs are.
    let delivered_delta = (big.at_deliver - small.at_deliver).abs();
    assert!(
        delivered_delta * 4 < input_delta,
        "the live heap at `deliver` differed by {delivered_delta} B between a \
         {} B input and a {} B input (delta {input_delta} B). The input is \
         still resident when the reply is delivered: the peak is \
         `input + output` where `max(input, output)` was available. Levels at \
         delivery were {} B and {} B above entry.",
        small.input_bytes,
        big.input_bytes,
        small.at_deliver,
        big.at_deliver,
    );
    assert_eq!(
        (small.solely_owned, big.solely_owned),
        (1, 1),
        "the sole-owner counter read {} / {} on two legs that ARE sole-owned; \
         it cannot be trusted to report a release that frees nothing",
        small.solely_owned,
        big.solely_owned,
    );

    // And the other half, which is what most real dispatches look like: the
    // release still runs, and frees nothing, and the counter SAYS so. A cut
    // that reported only `released` here would read as a full success while
    // the bytes stayed.
    assert_eq!(
        shared.solely_owned, 0,
        "a dispatch whose `DescribedJob` a memo still holds reported itself \
         solely owned; the counter would call a release that frees nothing a \
         success",
    );
    assert!(
        shared.at_deliver * 2 > shared.input_bytes,
        "the co-owned leg's input ({} B) was not resident at delivery ({} B), \
         so this leg is not reproducing the shape it exists to reproduce",
        shared.input_bytes,
        shared.at_deliver,
    );

    println!(
        "sole-owned: input {} B -> at deliver {} B; input {} B -> at deliver \
         {} B (input delta {input_delta} B, delivery delta {delivered_delta} B)",
        small.input_bytes, small.at_deliver, big.input_bytes, big.at_deliver,
    );
    println!(
        "co-owned:   input {} B -> at deliver {} B (released {}, solely owned {})",
        shared.input_bytes, shared.at_deliver, shared.released, shared.solely_owned,
    );
}
