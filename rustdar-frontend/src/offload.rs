//! Where a long-running, CPU-bound job runs.
//!
//! Four places in this crate used to hand a closure somewhere it would not
//! stall the frame that created it: the static radar render, the loop-frame
//! render, the overlay rasterization and the radar-sites rasterization. All
//! four have the same shape — a `FnOnce` that ends by sending its result on an
//! `mpsc::Sender` and calling `notify_redraw` — and all four had the same
//! `std::thread::Builder` call written out inline. Three of the four are
//! described jobs now, the sites raster most recently
//! ([`JobRequest::Overlay`]); the handler-backed overlay rasterization is the
//! closure that remains.
//!
//! They are funnelled through here so the wasm arm exists once.
//!
//! # Two shapes, one funnel
//!
//! A closure cannot be posted to a Web Worker, so the funnel takes work in two
//! forms and makes one decision about both:
//!
//! * [`offload`] takes an opaque `FnOnce`. It runs on a thread natively and
//!   inline on the web, which is the best available answer for a job whose
//!   inputs cannot be described — see [`offload`]'s own note on which those are.
//! * [`offload_job`] takes a [`JobRequest`], which *is* a description. Given a
//!   worker it posts; without one it runs [`execute`] in exactly the place
//!   [`offload`] would have run the closure — unless this thread has said it is
//!   [`expect_sink`]ing one shortly, in which case the job waits for it rather
//!   than paying a browser's whole frame for a transport that is on its way.
//!
//! The second is not a second code path. Both arms of [`offload_job`] call the
//! same [`execute`] and the same `deliver`, so the fallback is derived from the
//! worker path rather than written beside it, and there is no pair to drift.
//!
//! [`discard`] is the same fork applied to teardown — a job whose whole body is
//! a `drop`. Natively it goes to a lane of the pool kept for exactly this; on
//! the web, where [`offload`]'s answer is "inline on this frame", it queues
//! instead and [`drain_deferred_drops`] frees what a frame can afford, because
//! a free nobody is waiting on is the one job that never has to run now. What
//! keeps that queue draining is a term in the frame loop's own wake-up
//! condition — see [`drain_deferred_drops`], where the invariant is written.

use rustdar_radar::render_input::RenderInput;
use rustdar_radar::voxel::{VoxelGrid, VoxelRequest, VoxelShape};
use rustdar_radar::xsect::{CrossSection, SectionRequest};
use std::cell::RefCell;
use std::collections::HashMap;

/// The storm motion vector [`RenderedFrame::storm_motion`] carries,
/// re-exported for the reason [`crate::tls`] is: a platform adapter that has to
/// *rebuild* one of these off a message port should not need its own
/// `rustdar-radar` dependency to name the type.
///
/// `rustdar-web` is the adapter in question, and it does not have one — the
/// browser build reaches this crate and stops. Naming the path through
/// `rustdar_radar` there compiles nowhere, which is a thing only a wasm32
/// target check finds.
pub use rustdar_radar::srv::SrvMotion;

/// Run `job` away from the frame that requested it.
///
/// Native spawns a named OS thread and returns immediately.
///
/// wasm32-unknown-unknown has no threads: `std::thread::Builder::spawn` there
/// returns `Err(Unsupported)` at *runtime* rather than failing to compile, so a
/// bare spawn site does not break the web build — it compiles clean and then
/// panics the first time the user asks for a radar frame. That is the failure
/// this function exists to remove. The web arm runs `job` inline.
///
/// Running inline blocks the frame. For rasterization that is a visible stall,
/// and [`offload_job`] is the answer for the paths that can describe their
/// input. The one that cannot stays here:
///
/// * `overlay-render` captures a `RasterizeFn` — a `Box<dyn FnOnce(..) -> ..>`
///   holding overlay handler state — and answers with a `HitMap` whose
///   `id_map` is a `HashMap<u32, Arc<dyn OverlayItem>>`. Neither a trait-object
///   closure nor a trait-object map crosses a message port. Making it portable
///   means returning a `u32` id image and rebuilding the map on this side, a
///   refactor of `rustdar-overlays` against a rasterizer that draws vector
///   shapes rather than the 28 M projections the radar one does. It is being
///   dismantled kind by kind: `sites-render` was the first to leave — it is
///   [`JobRequest::Overlay`] now — and the polygon kinds, the hit-map kinds
///   and the model grid follow, at which point this arm's remaining caller
///   count reaches zero and the inline path is deleted.
///
/// Inline execution preserves the contract the callers actually depend on. Each
/// `job` delivers through a channel that is drained on a later frame, so a send
/// that happens before the caller returns is indistinguishable from one that
/// happens after it — the receiver cannot tell, and neither can the render
/// budget, whose `RenderGuard` simply drops sooner.
///
/// The `Send` bound is kept on both arms deliberately. It costs the web arm
/// nothing (every existing caller already satisfies it, since they were written
/// for threads) and dropping it would silently license a `!Send` job that then
/// fails to compile on desktop.
pub fn offload(name: &'static str, job: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // The pool's opaque lane, not a thread of its own. A closure that
        // cannot be described still has somewhere bounded to run, and the one
        // remaining `std::thread::Builder::spawn` in this module is the one
        // that builds the lanes. See [`pool`].
        if let Err(job) = pool::run_opaque(name, Box::new(job)) {
            log::error!("{name}: the job pool has no worker left; running it here");
            job();
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        // Timed because this is the one arm where the cost lands on the frame.
        // The number is what decides whether a worker is needed and how many, so
        // it is logged rather than estimated.
        let started = web_time::Instant::now();
        job();
        log::info!(
            "{name} took {} ms on the main thread",
            started.elapsed().as_millis()
        );
    }
}

/// Free `payload` away from the frame that stopped needing it.
///
/// Teardown is CPU-bound work like any other job here, and "it runs rarely" is
/// not an exception the frame budget recognises: an evicted decoded volume is
/// 47–69 MiB across thousands of per-radial buffers, its drop is an allocator
/// walk over every one of them, and the caller handing it over is the frame
/// thread on every target.
///
/// Native hands the payload to the pool's **free lane** — a third queue, one
/// thread wide, and deliberately not the opaque lane that carries the overlay
/// rasterizations a pan is waiting on. See [`pool`]'s own doc. The web arm has
/// no lane to hand it to: [`offload`] runs inline on wasm, so routing through
/// it would put the free back on the exact frame this function exists to
/// spare. That arm files the payload in a thread-local queue, and
/// [`drain_deferred_drops`] retires what a frame can afford.
///
/// # Call it from the frame thread
///
/// Natively the payload goes to the pool and any thread may hand one over —
/// except on the one path that does not: a free lane with no live worker falls
/// back to [`defer_drop`], and that queue is **thread-local**. Filed from a
/// tokio worker it is filed where no drain will ever look, [`has_deferred_drops`]
/// on the frame thread reports empty, and the payload is held for the life of
/// the process. The fallback needs a lane whose every thread failed to spawn,
/// so it is not a path anything reaches in practice; it is a real leak when it
/// is reached, and this is the obligation that prevents it.
///
/// An obligation rather than an assertion, and `pub(crate)` so the callers that
/// could break it are one crate rather than everyone. A `debug_assert` against
/// a thread id the drain stamps looks like the cheaper answer and is not: to
/// catch a *different* thread filing, the stamp has to be process-global, and
/// `cargo test` drives this module from many threads at once — each one
/// legitimately filling and draining its own thread-local queue. A global stamp
/// would fail those honest tests; a thread-local one cannot see the case it
/// would exist for.
///
/// # Hand over the last reference, and hand over a pointer
///
/// Three things a caller has to get right, none of which the signature can
/// enforce:
///
/// * **The last reference.** Deferring a `drop` is not deferring a *free*: a
///   queued `Arc` whose twin is still held frees nothing when its turn comes,
///   and the memory stays resident behind the live handle. The budget is
///   priced per payload and paid per handle, so a caller that keeps a copy has
///   moved a refcount decrement off the frame and nothing else. The reverse is
///   observable too — the queued reference keeps `Arc::strong_count` one above
///   what it was for as long as the entry waits, and this crate does branch on
///   that count (`render_dispatch::PaneRenderState::want_result`).
/// * **A pointer, not a large value.** The payload is boxed on the way in, so
///   an `impl Send` passed by value is memcpy'd once *on the calling thread*.
///   Hand over a `Box`, an `Arc` or an owning collection's entry — something
///   whose move is a pointer — rather than a large struct by value.
/// * **Memory, not a resource whose teardown matters.** A payload's `Drop` may
///   never run at all. The pool is a `OnceLock` that is never dropped and whose
///   lane threads are detached, so whatever is queued when the process exits
///   dies with it — and a payload mid-`drop` is cut off partway. A browser tab
///   at unload is the same. For plain memory that is exactly right; the process
///   is releasing everything anyway. For a wgpu resource, an mmap, a file
///   handle or a temp-file guard it means the teardown is silently skipped, or
///   runs on a worker after `main` has dropped the device it belonged to. This
///   takes `impl Send + 'static`, which invites all of those, and none of them
///   belong here.
///
/// Use [`discard_each`] for a collection: a batch handed over whole is one
/// payload, freed in one turn, and the pacing is lost.
pub(crate) fn discard(name: &'static str, payload: impl Send + 'static) {
    let payload: Box<dyn std::any::Any + Send> = Box::new(payload);
    #[cfg(not(target_arch = "wasm32"))]
    // `Err` is a free lane with no live worker — see [`pool::run_free`]. The
    // answer is the queue, not a free where we stand: this is the frame thread,
    // which is the one place a multi-GiB teardown must not land.
    if let Err(payload) = pool::run_free(name, payload) {
        log::warn!("{name}: the free lane has no worker left; deferring the drop instead");
        defer_drop(name, payload);
    }
    #[cfg(target_arch = "wasm32")]
    defer_drop(name, payload);
}

/// [`discard`] each item of `payloads` separately.
///
/// **This is what a collection teardown wants**, and it exists because the
/// obvious thing is the wrong one: `discard(name, map.remove(site))` type-checks
/// — every collection is `Send + 'static` — and hands over a single payload
/// holding every volume in it, which the drain then frees in one turn on one
/// frame. That is the stall this module exists to remove, written in a form
/// that looks like the fix.
///
/// Per item, the pacing is real: the drain's budget is spent between whole
/// payloads, so entries are what a frame can stop between.
///
/// **Every obligation on [`discard`] applies to every item**, and the item is
/// where two of them are easiest to miss: an `IntoIterator<Item = T>` most
/// naturally yields `T` by value, so a collection of large structs memcpys each
/// one on the calling thread — iterate `Box`es or `Arc`s — and a collection of
/// handles frees nothing unless these are the last ones. It must be called from
/// the frame thread for the reason [`discard`] must.
pub(crate) fn discard_each<T: Send + 'static>(
    name: &'static str,
    payloads: impl IntoIterator<Item = T>,
) {
    for payload in payloads {
        discard(name, payload);
    }
}

/// A payload awaiting its frame-paced free, and the name it was discarded
/// under — which is all the drain's log has to identify it by, the same shape
/// the pool's lanes carry.
type DeferredDrop = (&'static str, Box<dyn std::any::Any + Send>);

thread_local! {
    /// What [`discard`] is holding until a frame can afford to free it.
    ///
    /// Thread-local because a browser has one thread, so here a thread-local
    /// queue is a process-wide queue with a cheaper lock. Natively it holds
    /// only what the free lane refused — a lane with no worker at all — so an
    /// entry is always one this thread put here, and the drain that empties it
    /// runs on this thread too.
    ///
    /// **This is the opposite structure to [`PENDING`], deliberately**, and the
    /// contrast is worth stating because that registry's doc argues at length
    /// for *not* being thread-local. It has to be global: a job is submitted
    /// from the frame thread or a tokio worker and answered on a pool thread,
    /// so the map is reached from threads that are not the submitter's. Nothing
    /// of the sort happens here — every producer of an entry is the thread that
    /// consumes it — so the reason that made a registry global is exactly the
    /// reason absent from this queue.
    ///
    /// A `VecDeque`: the drain takes from the front one payload at a time, so
    /// the entry that has waited longest is always the next to go and no
    /// teardown can starve behind a later one.
    static DEFERRED_DROPS: RefCell<std::collections::VecDeque<DeferredDrop>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
}

/// File `payload` for [`drain_deferred_drops`] to retire.
///
/// Reached on wasm for every discard and natively only for one the free lane
/// could not take. The `cfg` in [`discard`] is over the *routing*, never over
/// whether this queue exists, which is what lets a host test drive the queue a
/// browser will use.
///
/// `pub(crate)` for exactly that: `app::tests` reaches it to put a payload on
/// the queue the way a browser's [`discard`] does, because the native routing
/// would hand it to the pool and the frame loop is what that test is about.
pub(crate) fn defer_drop(name: &'static str, payload: Box<dyn std::any::Any + Send>) {
    DEFERRED_DROPS.with(|q| q.borrow_mut().push_back((name, payload)));
}

/// Whether this thread is still holding anything it has promised to free.
///
/// Read by `App::handle_redraw` — see [`drain_deferred_drops`] for the
/// invariant it is read *for*, which is the whole reason this is public.
pub(crate) fn has_deferred_drops() -> bool {
    DEFERRED_DROPS.with(|q| !q.borrow().is_empty())
}

/// Free deferred payloads until `budget` is spent, and answer how many went.
///
/// # A non-empty queue must keep the frame loop awake
///
/// **This is a contract on the caller, and without it the pacing below is a
/// statement about frames that says nothing about memory.** The app rests on
/// `ControlFlow::Wait` and draws only when something asks it to, so a queue
/// that does not ask is a queue that stops draining: a site switch fills it,
/// one frame frees what it can, the fetch that woke the loop settles, and the
/// remainder stays resident until the user next touches the application — at
/// exactly the moment the application decided it wanted the memory back.
/// `App::handle_redraw` therefore names [`has_deferred_drops`] among the terms
/// that request the next frame, beside the renders and loops already there.
///
/// The re-arm is necessary and not sufficient, and these are the cases it does
/// not reach. Four sit between the drain and the re-arm — the drain is early in
/// `handle_redraw`, on purpose, so each of these still frees its budget's worth
/// per frame *if a frame happens*, and what they cost is the next frame not
/// being asked for:
///
/// * a **minimized** window returns before the re-arm;
/// * a **zero-area** window does too, which is the normal state of a browser's
///   first frame or two rather than an edge case;
/// * **no window or no renderer** returns earliest of all — which is what
///   `suspended` leaves behind, so it is every Android background;
/// * a **backgrounded browser tab** gets no animation frames at all, so nothing
///   written here reaches it.
///
/// All four are bounded by what was queued when the app stopped drawing, and
/// all four end when it draws again.
///
/// # Peak memory goes up, and that is the trade
///
/// Deferring a free does not reduce what is held; it moves *when* it is
/// released. A wasm teardown now has a window in which the maps have let go and
/// the memory has not come back — as much as the queue is holding, on a target
/// whose address space is 4 GiB. For a site switch that is the right trade: the
/// alternative is the same bytes released a few milliseconds sooner and a frame
/// visibly dropped to do it. It is worth stating plainly because the window is
/// open-ended in exactly one of the cases above — a backgrounded tab stops
/// drawing, so it stops draining, while holding everything the switch queued.
///
/// # A time budget, not a count
///
/// The queue's entries are whatever a caller discarded, and their frees differ
/// by orders of magnitude — a `PolarField` and a whole `DecodedScan` are both
/// one entry. A count would have to be priced against a per-entry millisecond
/// figure nobody has for the browser, on payloads that are not one size; a
/// duration is priced against the frame, which is 16.7 ms on every target and
/// is the thing actually being protected.
///
/// **At least one payload goes per call, whatever the budget says.** The
/// elapsed check is made *after* a free rather than before it, so the drain
/// cannot be turned into a no-op — not by a budget set to zero, and not by a
/// platform whose clock is too coarse to resolve one.
///
/// The price is that this **paces rather than bounds**: a call costs the budget
/// plus one whole payload, and on wasm one payload can be the 47–69 MiB volume
/// the mechanism exists to keep off a frame. Firefox's
/// `privacy.reduceTimerPrecision` is on by default and clamps the clock to
/// ~100 µs with jitter, which cuts the same way — `elapsed` under-reports, so
/// the overrun is if anything larger than the numbers here suggest. Sixty
/// volumes freed a few per frame instead of sixty on one frame is the win;
/// a frame-time guarantee is not on offer, and
/// [`crate::constants::DEFERRED_DROP_BUDGET_PER_FRAME`] says so where a reader
/// looking for the bound will land.
pub(crate) fn drain_deferred_drops(budget: std::time::Duration) -> usize {
    // Nothing to do, and nothing measured: this runs on every frame of every
    // target, and on wasm `Instant::now` is a call across the JS boundary to
    // `performance.now()`. An empty queue must cost a thread-local read.
    if !has_deferred_drops() {
        return 0;
    }
    let started = web_time::Instant::now();
    let mut freed = 0;
    // The payload whose free cost the most, and **only where that can be
    // measured**. A per-payload cost needs a clock finer than one free, and
    // wasm's is not: `performance.now()` is clamped to ~100 µs with jitter
    // under Firefox's default `privacy.reduceTimerPrecision`, so nearly every
    // payload would read 0 µs and the "dearest" would be whichever one the
    // comparison happened to keep — a name presented as a measurement and
    // arrived at by tie-breaking. Natively the clock resolves it, so the
    // attribution is native-only and says so rather than being quietly wrong on
    // the arm that actually pays.
    #[cfg(not(target_arch = "wasm32"))]
    let mut dearest: (&'static str, u128) = ("nothing", 0);
    // One at a time, with the borrow released before the payload is dropped:
    // a `Drop` that discards something of its own then finds the queue
    // borrowable rather than panicking the frame.
    while let Some((name, payload)) = DEFERRED_DROPS.with(|q| q.borrow_mut().pop_front()) {
        #[cfg(not(target_arch = "wasm32"))]
        let before = started.elapsed();
        drop(payload);
        freed += 1;
        // The one read per payload the loop needs anyway, reused for both the
        // budget test and the cost above rather than taken twice.
        let elapsed = started.elapsed();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let cost = elapsed.saturating_sub(before).as_micros();
            if cost > dearest.1 {
                dearest = (name, cost);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = name;
        if elapsed >= budget {
            break;
        }
    }
    // Microseconds, and once per drain rather than once per payload. A free is
    // routinely sub-millisecond, so `as_millis` printed "0 ms" for the thing
    // this line exists to measure; and one line per entry is a console write
    // per payload for the length of a teardown, which on the target that has to
    // be measured is itself a frame cost.
    #[cfg(not(target_arch = "wasm32"))]
    log::debug!(
        "freed {freed} deferred payload(s) in {} µs; dearest was {} at {} µs; {} left",
        started.elapsed().as_micros(),
        dearest.0,
        dearest.1,
        DEFERRED_DROPS.with(|q| q.borrow().len()),
    );
    #[cfg(target_arch = "wasm32")]
    log::debug!(
        "freed {freed} deferred payload(s) in {} µs on the frame thread; {} left",
        started.elapsed().as_micros(),
        DEFERRED_DROPS.with(|q| q.borrow().len()),
    );
    freed
}

/// A CPU-bound job described as data, so it can be executed somewhere that does
/// not share this thread's memory.
///
/// Every variant is an *input* to a render, never its output: what travels is
/// the smallest thing the renderer can be re-run from, because re-running it is
/// how the worker and this thread stay byte-identical without a second
/// implementation to keep in step.
#[derive(Debug, Clone, PartialEq)]
pub enum JobRequest {
    /// Rasterize a Level II frame.
    Radar {
        /// Boxed because a `RenderInput` owns its gate bytes and is the largest
        /// thing in the enum by three orders of magnitude.
        input: Box<RenderInput>,
        /// Whether the caller wants the numbers behind the gates, or only the
        /// geometry of where they are.
        ///
        /// Static pane renders want both — the numbers are what a hover reads.
        /// A **loop frame** wants only the geometry: 5.03 MiB of values for the
        /// widest sweep, across a loop of up to 36 frames, is not affordable and
        /// does not have to be paid, because the volume the frame was rendered
        /// from is resident for as long as the loop lives and the wedges are
        /// what turn a point back into a gate of it. See
        /// [`rustdar_radar::hover::SweepGates`].
        ///
        /// It used to mean the `side²` `f32` raster grid, which no longer
        /// leaves `rustdar-radar` on any path — see [`RenderedFrame::polar`].
        /// The geometry is kept on both settings; only the values are dropped,
        /// and the texture is unaffected either way.
        values_wanted: bool,
        /// The largest side this render's raster may have. See
        /// [`JobRequest::side_ceiling_px`].
        side_ceiling_px: u32,
    },
    /// Rasterize a Level III radial product.
    ///
    /// The product's *bytes*, not its decoded form: a `Level3Message` holds
    /// run-length radial packets with no serde derives anywhere in the graph,
    /// and re-decoding is both cheap against the render and a use of the one
    /// decoder rather than a second description of the format. The decode moves
    /// off the main thread with the render as a result.
    Level3 {
        bytes: std::sync::Arc<Vec<u8>>,
        product: rustdar_radar::types::RadarProduct,
        radar_lat: f64,
        radar_lon: f64,
        /// See [`JobRequest::side_ceiling_px`].
        side_ceiling_px: u32,
    },
    /// Rasterize a Level III product **derived from two objects of the same
    /// volume**: VIL density, Digital VIL over Enhanced Echo Tops
    /// (`rustdar_radar::vild`).
    ///
    /// A second variant rather than a `Vec<Arc<Vec<u8>>>` on the one above: the
    /// two objects are not interchangeable — the first is the numerator and the
    /// second the denominator — and a positional pair says so where a list
    /// would leave it to a comment. The bytes travel for the same reason
    /// [`JobRequest::Level3`]'s do.
    Level3Pair {
        dvl: std::sync::Arc<Vec<u8>>,
        eet: std::sync::Arc<Vec<u8>>,
        radar_lat: f64,
        radar_lon: f64,
        /// See [`JobRequest::side_ceiling_px`].
        side_ceiling_px: u32,
    },
    /// Draw a vertical cross-section through a volume.
    ///
    /// The geometry rides here rather than on the [`RenderInput`]: a section's
    /// endpoints are not a render parameter *of reflectivity*, and a
    /// `RenderInput` carrying them would make every plan-view payload's bytes
    /// depend on where somebody last drew a line.
    ///
    /// The `input` is a [`RenderInput::extract_volume`] payload — every tilt
    /// carrying the moment, and the cut table that keys them.
    Section {
        input: Box<RenderInput>,
        request: SectionRequest,
    },
    /// Resample a volume into a Cartesian grid for a raymarch.
    Voxels {
        input: Box<RenderInput>,
        request: VoxelRequest,
    },
    /// Rasterize an overlay layer — the first kind of frame-following work to
    /// leave [`offload`]'s opaque arm, which on the web runs closures **inline
    /// on the browser's one thread** (a measured 224 ms against a 290 ms p50
    /// gesture frame for the layer set that prompted this).
    ///
    /// The raster's geometry travels on this variant — one statement of the
    /// texture's size and ground for every overlay kind — and everything a
    /// particular kind's rasterizer reads beyond that travels in `input`,
    /// whose variant *is* the kind. A separate kind field would be a second
    /// statement of one fact, which is the disagreement
    /// [`agree_on_product`] exists to refuse elsewhere.
    ///
    /// Only the sites render is described so far. The handler-backed kinds
    /// still capture a `RasterizeFn` trait object and stay on [`offload`]'s
    /// opaque arm — see that function's own note — until each gains a wire
    /// form of its own.
    Overlay {
        /// Texture width in physical texels, from the pane's
        /// `OverlayTexturePlan` — never re-derived on the far side.
        width: u32,
        /// Texture height. See `width`.
        height: u32,
        /// The ground the texture covers: the viewport plus overdraw, exactly
        /// as `OverlayTexturePlan::coverage` answered it at the dispatch site.
        bounds: rustdar_overlays::types::GeoBounds,
        /// What the kind's rasterizer reads. See [`OverlayJobInput`].
        input: OverlayJobInput,
    },
    /// **Decode a downloaded Level II archive volume.**
    ///
    /// The one job here that does not rasterize anything, and the one whose
    /// input is not already a decoded volume — it is what *produces* the
    /// volume every other variant is built from.
    ///
    /// It is a job for the reason the renders are: on the web the work has to
    /// happen somewhere that is not the one thread the browser has.
    /// `rustdar_radar::scan`'s own doc predicted this — the walk is paid "on
    /// cold start, on every timeline scrub, on every 'next scan', and once per
    /// frame of a loop download … and on the web it is paid on the browser's
    /// main thread" — and the frame-thread audit put a number on it: **1021.9
    /// ms in Firefox 153 and 911.4 ms in Chrome 151** for a 16.9 MB, 21-sweep
    /// volume, against 42–66 ms on a native thread pool. Nothing else this
    /// application does blocks a frame for a second.
    ///
    /// # The bytes, not a `File`
    ///
    /// `nexrad_data::volume::File` owns a `Vec<u8>` and nothing else, so the
    /// archive bytes *are* the job's input and no wrapper has to cross. They
    /// arrive here straight off the download, which is the split this variant
    /// exists to make: the network half belongs to whoever has the fetch stack
    /// and stays on the async task, and the CPU half comes here.
    ///
    /// `Arc` so that the dispatch site — which may hold the bytes for a retry —
    /// does not have to hand over its only copy, and so the enum's `Clone`
    /// costs a refcount rather than 16 MB.
    Decode { archive: std::sync::Arc<Vec<u8>> },
}

/// What one overlay kind's rasterizer reads, per kind — the payload of
/// [`JobRequest::Overlay`].
///
/// Each variant carries **the rasterizer's own input type**, not a copy of its
/// fields: the wire decodes back into the struct the direct call takes, so
/// "described over a port" and "called on this thread" run the same function
/// on the same value and byte-identity is a property of the type. See
/// `rustdar_overlays::render::rasterize::SitesInput`, whose doc states the
/// contract from the rasterizer's side.
///
/// One variant so far. The polygon kinds, the hit-map kinds and the model grid
/// are next, in that order of difficulty; each lands here as a variant plus an
/// [`OVERLAY_INPUT_SITES`]-style code, and until then those kinds stay on
/// [`offload`]'s opaque arm.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayJobInput {
    /// The radar-site markers: catalogue rows plus the appearance inputs.
    Sites(rustdar_overlays::render::rasterize::SitesInput),
}

/// What a job produces.
///
/// Widened from a bare [`RenderedFrame`] when a section and a voxel grid became
/// things a worker could be asked for. **[`RenderedFrame`] itself is
/// deliberately untouched**, and in particular did not gain a width and a
/// height even once a plan view stopped having one size: its consumers derive
/// the side from the buffer's own length and check it — a whole number of
/// pixels, a perfect square, a side inside this build's own bounds
/// (`constants::raster_side_from_rgba_len`), which is the same guard a named
/// constant was — a `ColorImage` panic on a render worker means no response
/// ever arrives and the pane stays blank forever —
/// without the payload being trusted to describe itself. See
/// [`JobOutput::frame`].
#[derive(Debug, PartialEq)]
pub enum JobOutput {
    Frame(RenderedFrame),
    /// Boxed: a `CrossSection` owns three `SECTION_WIDTH × SECTION_HEIGHT`
    /// planes, which is megabytes against the enum's other variants.
    Section(Box<CrossSection>),
    /// Boxed for the same reason, more so: a desktop grid is 8 MiB of indices.
    Voxels(Box<VoxelGrid>),
    /// A decoded Level II volume — the answer to [`JobRequest::Decode`], and
    /// the only output here that is not a picture of anything.
    ///
    /// Boxed like the two above and for a stronger version of their reason: a
    /// `DecodedScan` owns every gate of every radial of every sweep, 47–69 MiB
    /// across the volumes this was measured on, which is an order of magnitude
    /// past the largest thing that had been in this enum.
    Volume(Box<rustdar_radar::scan::DecodedScan>),
    /// An overlay raster — the answer to [`JobRequest::Overlay`]: the RGBA
    /// texture, **always premultiplied** ([`execute`]'s overlay arm converts
    /// any rasterizer that declares straight alpha), and nothing else.
    ///
    /// Deliberately a bare buffer with no width, height or framing, for the
    /// reason [`RenderedFrame`] has no width: nothing on this port describes
    /// its own shape. The dispatch site captured the texture's dimensions and
    /// its `deliver` believes the buffer only if its length is exactly
    /// `width × height × 4` of *those* values — a payload from another build,
    /// or one tagged with the wrong kind, fails that arithmetic and reads as
    /// "nothing to draw" rather than as a picture of the wrong shape.
    OverlayRaster(Vec<u8>),
}

impl JobOutput {
    /// The frame, or `None` for an output of another kind.
    ///
    /// This is what makes widening the result type safe for every existing
    /// consumer: a `Section` handed to a frame consumer becomes `None`, which
    /// is "nothing to draw" — a state every path already handles, with
    /// `deliver` still running and the render budget still unwound.
    pub fn frame(self) -> Option<RenderedFrame> {
        match self {
            Self::Frame(frame) => Some(frame),
            Self::Section(_) | Self::Voxels(_) | Self::Volume(_) | Self::OverlayRaster(_) => None,
        }
    }

    /// The section, or `None` for an output of another kind.
    pub fn section(self) -> Option<Box<CrossSection>> {
        match self {
            Self::Section(section) => Some(section),
            Self::Frame(_) | Self::Voxels(_) | Self::Volume(_) | Self::OverlayRaster(_) => None,
        }
    }

    /// The voxel grid, or `None` for an output of another kind.
    pub fn voxels(self) -> Option<Box<VoxelGrid>> {
        match self {
            Self::Voxels(grid) => Some(grid),
            Self::Frame(_) | Self::Section(_) | Self::Volume(_) | Self::OverlayRaster(_) => None,
        }
    }

    /// The decoded volume, or `None` for an output of another kind.
    ///
    /// The same shape as the three above, and the same reason: a consumer
    /// handed the wrong kind sees `None`, which every caller already treats as
    /// "the job produced nothing" — a state a failed decode and a failed
    /// render have always shared.
    pub fn volume(self) -> Option<Box<rustdar_radar::scan::DecodedScan>> {
        match self {
            Self::Volume(volume) => Some(volume),
            Self::Frame(_) | Self::Section(_) | Self::Voxels(_) | Self::OverlayRaster(_) => None,
        }
    }

    /// The overlay raster, or `None` for an output of another kind.
    ///
    /// The narrow accessor the four above are, for the same reason — and the
    /// buffer it answers is believed nowhere until its length is checked
    /// against the dispatch's own dimensions; see [`JobOutput::OverlayRaster`].
    pub fn overlay_raster(self) -> Option<Vec<u8>> {
        match self {
            Self::OverlayRaster(rgba) => Some(rgba),
            Self::Frame(_) | Self::Section(_) | Self::Voxels(_) | Self::Volume(_) => None,
        }
    }

    /// Which decoder owns this output's bytes when it travels as the worker
    /// reply's out-of-band payload, or `None` for a frame — which does not
    /// travel that way at all, having its own fields on the message.
    ///
    /// # Its own code space, and no longer `RenderView`'s
    ///
    /// It used to be `view().wire_code()`, and that read as a tidy reuse right
    /// up until an output arrived that is not a view of anything. A decoded
    /// volume is not a plan view, a cross-section or a raymarch grid; it is
    /// what all three are *made from*. Widening `RenderView` to admit it would
    /// have put a variant into the enum that decides pane layout, tilt-family
    /// widening and download scope
    /// ([`RenderView::reads_whole_volume`](rustdar_radar::types::RenderView::reads_whole_volume)),
    /// none of which a decode has an answer for.
    ///
    /// So the wire gets its own byte. The two existing values are unchanged
    /// deliberately — a cross-section is still 2 and a voxel grid still 3 — so
    /// this is a widening of the code space rather than a renumbering of it,
    /// and the protocol version bump beside it is what refuses a worker that
    /// predates the fourth.
    pub fn out_kind(&self) -> Option<u8> {
        match self {
            // A frame rides the `IMAGE`/`POLAR`/`MAX_RANGE` fields.
            Self::Frame(_) => None,
            Self::Section(_) => Some(OUT_KIND_SECTION),
            Self::Voxels(_) => Some(OUT_KIND_VOXELS),
            Self::Volume(_) => Some(OUT_KIND_VOLUME),
            Self::OverlayRaster(_) => Some(OUT_KIND_OVERLAY),
        }
    }
}

/// A cross-section raster. Was `RenderView::CrossSection`'s wire code and keeps
/// its value — see [`JobOutput::out_kind`].
pub const OUT_KIND_SECTION: u8 = 2;
/// A Cartesian voxel grid. Was `RenderView::Volume`'s wire code.
pub const OUT_KIND_VOXELS: u8 = 3;
/// A decoded Level II volume. The first code that never was a `RenderView`.
pub const OUT_KIND_VOLUME: u8 = 4;
/// An overlay raster: raw premultiplied RGBA, no framing of its own.
///
/// The one `OUT` payload without a magic to refuse the wrong bytes — raw
/// pixels have no header to carry one. What stands in for the magic is the
/// length check at the consumer ([`JobOutput::OverlayRaster`]): another kind's
/// payload arriving under this code is believed only if it happens to be
/// exactly `width × height × 4` bytes of the raster the dispatch asked for,
/// and anything else is "nothing to draw". The protocol version beside the
/// code (`rustdar-web`'s `PROTOCOL_VERSION`, bumped to 9 with it) is what
/// keeps a worker that predates the fifth code from being attached at all.
pub const OUT_KIND_OVERLAY: u8 = 5;

/// What a rasterizing job produces: the RGBA texture, the half-width it was
/// projected at, and the per-pixel value grid (`NAN` where no gate landed).
///
/// Named fields, as the renderer's own [`rustdar_radar::render::SweepRender`]
/// has: the two buffers are the same shape to a message port, and transposing
/// them would swap a texture for a value grid somewhere with no type error to
/// catch it. A separate type and not that one because this is what crosses the
/// port.
///
/// The extent and the fold limit are metadata and stay metadata — they say
/// where the pixels *are* and what speed they wrap at, never how many of them
/// there are. How many there are is the buffer's own length, checked rather
/// than believed at each consumer (`constants::raster_side_from_rgba_len`);
/// nothing on this port describes its own shape, which is what keeps a
/// malformed payload from being believed. Adding a second `f64` beside the
/// extent does not weaken that: neither number can be read as a dimension,
/// and the guard that protects a pane from a blank texture reads the length
/// and only the length.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFrame {
    pub image: Vec<u8>,
    pub max_range_km: f64,
    /// The gates behind the pixels, at the resolution the radar measured them.
    ///
    /// **The `side²` `f32` raster grid is not here and does not leave the
    /// renderer.** It used to: `7362² × 4` = 206.75 MiB on desktop, and 16 MiB
    /// through the browser's `postMessage` — transferred, but still copied once
    /// into the worker's linear memory and once back out of the page's. This is
    /// the same numbers at the resolution they were measured at, about 5 MiB
    /// for the widest sweep the fleet flies, and it is what a hover reads. See
    /// [`rustdar_radar::render::polar`].
    pub polar: rustdar_radar::render::polar::PolarField,
    /// Where the rendered sweep's cut declared its velocity folds, m/s, or
    /// `None` for a raster with no one cut behind it — every Level III
    /// product and every volume product — and for a volume that declared
    /// nothing, which is every Message 1 volume.
    ///
    /// See [`rustdar_radar::render::SweepRender::nyquist_ms`], which is where
    /// it comes from and which explains what it is a property of.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer this raster was classified against came from,
    /// or `None` for every raster that classified nothing — which is every
    /// product but the hybrid classification.
    ///
    /// See [`rustdar_radar::hca::MeltingLayerSource`]. It rides beside
    /// `nyquist_ms` and for the same reason: it is a fact about *this* picture
    /// that the far end cannot recompute, and here it is the difference
    /// between a classification measured for this volume and one standing on a
    /// fleet constant that has been measured 3 km wrong.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where the storm motion vector this raster was shifted by came from, or
    /// `None` for every raster that shifted nothing — which is every product
    /// but storm-relative velocity.
    ///
    /// See [`rustdar_radar::srv::SrvMotion`]. It rides beside
    /// `melting_layer_source` and for the same reason: it is a fact about
    /// *this* picture that the far end cannot recompute — the projection of
    /// this vector is already inside every gate value, and the two derived
    /// rungs are computed from a wind profile the page never sees.
    ///
    /// The whole vector rather than its provenance byte, because the legend
    /// draws the speed and direction and only apologises for nothing.
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
}

/// A [`MeltingLayerSource`](rustdar_radar::hca::MeltingLayerSource) as a
/// number, for the one boundary that can only carry numbers.
///
/// The enum lives in `rustdar-radar`, which has no wire form for it and needs
/// none: nothing in that crate crosses a message port. The browser's
/// page↔worker port does, and it carries JS values — so the mapping is written
/// here, beside [`RenderedFrame`], which is the type that actually crosses.
///
/// A newtype rather than two free functions so the pair cannot drift apart:
/// [`from_wire_code`](Self::from_wire_code) is exhaustive over the same match
/// arms [`wire_code`](Self::wire_code) writes, so adding a variant upstream
/// fails this build rather than silently encoding as "unknown".
///
/// `None` from `from_wire_code` is a byte this build does not have — a page and
/// a worker on opposite sides of a deploy, which the protocol token already
/// refuses — and reads as "no source stated", the same as an absent field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeltingLayerWire(pub rustdar_radar::hca::MeltingLayerSource);

impl MeltingLayerWire {
    pub fn wire_code(self) -> u8 {
        use rustdar_radar::hca::MeltingLayerSource as S;
        match self.0 {
            S::Rpg => 0,
            S::RadarDetected => 1,
            S::Sounding => 2,
            S::FleetDefault => 3,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code).
    pub fn from_wire_code(code: u8) -> Option<Self> {
        use rustdar_radar::hca::MeltingLayerSource as S;
        let source = match code {
            0 => S::Rpg,
            1 => S::RadarDetected,
            2 => S::Sounding,
            3 => S::FleetDefault,
            _ => return None,
        };
        Some(Self(source))
    }
}

/// A [`StormMotionSource`](rustdar_radar::srv::StormMotionSource) as a number,
/// for the same boundary [`MeltingLayerWire`] crosses.
///
/// Written here for the reason that one is: the enum lives in `rustdar-radar`,
/// which crosses no message port and needs no wire form; the browser's
/// page↔worker port does, and it carries JS values.
///
/// A newtype rather than two free functions so the pair cannot drift apart:
/// [`from_wire_code`](Self::from_wire_code) is exhaustive over the same match
/// arms [`wire_code`](Self::wire_code) writes, so adding a rung upstream fails
/// this build rather than silently encoding as "unknown" — which for this
/// value would mean an SRV pane reporting a Bunkers prediction as the RPG's own
/// cell average, the one confusion the whole path exists to prevent.
///
/// The numbering **is** the declaration order, which is the fallback order, so
/// a code reads as a rung of the chain. `None` from `from_wire_code` is a byte
/// this build does not have — a page and a worker on opposite sides of a
/// deploy, which the protocol token already refuses — and reads as "no source
/// stated", the same as an absent field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StormMotionWire(pub rustdar_radar::srv::StormMotionSource);

impl StormMotionWire {
    pub fn wire_code(self) -> u8 {
        use rustdar_radar::srv::StormMotionSource as S;
        match self.0 {
            S::UserOverride => 0,
            S::RpgScitAverage => 1,
            S::BunkersRightMover => 2,
            S::MeanWind => 3,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code).
    pub fn from_wire_code(code: u8) -> Option<Self> {
        use rustdar_radar::srv::StormMotionSource as S;
        let source = match code {
            0 => S::UserOverride,
            1 => S::RpgScitAverage,
            2 => S::BunkersRightMover,
            3 => S::MeanWind,
            _ => return None,
        };
        Some(Self(source))
    }
}

/// `None` where the renderer found nothing to draw — a scan with no matching
/// sweep. Callers treat it as the failure the renderer already meant by it.
pub type JobResult = Option<JobOutput>;

impl From<rustdar_radar::render::SweepRender> for RenderedFrame {
    /// The renderer's own answer, whole. One conversion for all three
    /// rasterizing arms, so a Level III frame and a Level II one cannot come to
    /// describe themselves differently.
    fn from(render: rustdar_radar::render::SweepRender) -> Self {
        // **Where the raster value grid dies, on every path.** It is the
        // rasterizer's own instrument — its tests measure painted ranges and
        // ring bounds off it, and the colouring pass writes through it — and
        // nothing outside that crate has needed it since the readout started
        // reading gates. This is the one conversion all three rasterizing arms
        // come through, so putting it here is what makes "it never leaves the
        // renderer" a property of the type rather than of three call sites.
        //
        // Handed back rather than freed: the slot is waiting for it, and on
        // desktop this is a 206.75 MiB allocation glibc can never recycle. See
        // `rustdar_radar::render::POOLED_VALUES`.
        rustdar_radar::render::recycle_values(render.values);
        Self {
            image: render.image,
            max_range_km: render.max_range_km,
            polar: render.polar,
            nyquist_ms: render.nyquist_ms,
            melting_layer_source: render.melting_layer_source,
            storm_motion: render.storm_motion,
        }
    }
}

/// A rasterizing job, described where it can be and opaque where it cannot.
///
/// Both arms reach [`offload_job`], which is the point: there is one place that
/// decides where work runs, and adding a job kind does not add a dispatch site.
pub enum Job {
    /// Portable. Goes to the worker when one is attached, and runs through
    /// [`execute`] when none is. Every rasterizing dispatch is one of these.
    Described(JobRequest),
    /// Not describable, so it runs where [`offload`] runs things — a thread
    /// natively, this frame in the browser.
    ///
    /// Nothing in production is one today; it is what [`Job::renders_nothing`]
    /// is built from, and the shape a future job kind takes before it has a
    /// wire form. Reaching for it for a *rasterizing* job would put that job
    /// back on the browser's main thread, which is the thing this module
    /// exists to stop.
    Opaque(Box<dyn FnOnce() -> JobResult + Send>),
}

impl Job {
    /// A job whose answer is "nothing to draw".
    ///
    /// Used where a request cannot even be described because there is no data
    /// behind it. It is deliberately still a *job*: the caller has already
    /// taken a slot in the render budget and marked its pane in flight, and
    /// those are unwound by `deliver` running, not by returning early.
    pub fn renders_nothing() -> Self {
        Self::Opaque(Box::new(|| None))
    }
}

impl JobRequest {
    /// Encode for a worker. The framing is one tag byte and then the variant's
    /// own bytes, so a new variant cannot be mistaken for an old one.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Radar {
                input,
                values_wanted,
                side_ceiling_px,
            } => {
                let mut out = Vec::new();
                out.push(TAG_RADAR);
                out.push(u8::from(*values_wanted));
                out.extend_from_slice(&side_ceiling_px.to_le_bytes());
                out.extend_from_slice(&input.to_bytes());
                out
            }
            Self::Level3 {
                bytes,
                product,
                radar_lat,
                radar_lon,
                side_ceiling_px,
            } => {
                let mut out = vec![TAG_LEVEL3];
                out.extend_from_slice(&side_ceiling_px.to_le_bytes());
                out.extend_from_slice(&product.wire_code().to_le_bytes());
                out.extend_from_slice(&radar_lat.to_le_bytes());
                out.extend_from_slice(&radar_lon.to_le_bytes());
                out.extend_from_slice(bytes);
                out
            }
            Self::Level3Pair {
                dvl,
                eet,
                radar_lat,
                radar_lon,
                side_ceiling_px,
            } => {
                // The first object is length-prefixed and the second takes the
                // rest, so neither length can lie about the other.
                let mut out = vec![TAG_LEVEL3_PAIR];
                out.extend_from_slice(&side_ceiling_px.to_le_bytes());
                out.extend_from_slice(&radar_lat.to_le_bytes());
                out.extend_from_slice(&radar_lon.to_le_bytes());
                out.extend_from_slice(&(dvl.len() as u32).to_le_bytes());
                out.extend_from_slice(dvl);
                out.extend_from_slice(eet);
                out
            }
            // Both of the two below put the `RenderInput` **last**, because
            // `RenderInput::from_bytes` refuses trailing bytes: it has to be
            // handed exactly the remainder, so nothing may follow it.
            Self::Section { input, request } => {
                let mut out = vec![TAG_SECTION];
                encode_section_request(&mut out, request);
                out.extend_from_slice(&input.to_bytes());
                out
            }
            Self::Voxels { input, request } => {
                let mut out = vec![TAG_VOXELS];
                encode_voxel_request(&mut out, request);
                out.extend_from_slice(&input.to_bytes());
                out
            }
            Self::Overlay {
                width,
                height,
                bounds,
                input,
            } => {
                let mut out = vec![TAG_OVERLAY];
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                // The box in its declaration order, spelled here and in the
                // decoder and nowhere else.
                out.extend_from_slice(&bounds.min_lat.to_le_bytes());
                out.extend_from_slice(&bounds.max_lat.to_le_bytes());
                out.extend_from_slice(&bounds.min_lon.to_le_bytes());
                out.extend_from_slice(&bounds.max_lon.to_le_bytes());
                encode_overlay_input(&mut out, input);
                out
            }
            // The tag and then the archive, which takes the rest: an archive
            // volume has no framing this needs to know about, and a length
            // prefix would be a second statement of a length the buffer
            // already has.
            Self::Decode { archive } => {
                let mut out = Vec::with_capacity(1 + archive.len());
                out.push(TAG_DECODE);
                out.extend_from_slice(archive);
                out
            }
        }
    }

    /// `None` on an unknown tag or a payload this build cannot read — the two
    /// ends of a message port can be different builds, so that has to be a
    /// clean refusal rather than a misparse.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (tag, rest) = bytes.split_first()?;
        match *tag {
            TAG_RADAR => {
                let (values_wanted, rest) = rest.split_first()?;
                let (ceiling, rest) = rest.split_at_checked(4)?;
                Some(Self::Radar {
                    values_wanted: flag(*values_wanted)?,
                    side_ceiling_px: u32::from_le_bytes(ceiling.try_into().ok()?),
                    input: Box::new(RenderInput::from_bytes(rest)?),
                })
            }
            TAG_LEVEL3 => {
                let mut r = Reader::new(rest);
                Some(Self::Level3 {
                    side_ceiling_px: r.u32()?,
                    product: rustdar_radar::types::RadarProduct::from_wire_code(r.u16()?)?,
                    radar_lat: r.f64()?,
                    radar_lon: r.f64()?,
                    bytes: std::sync::Arc::new(r.rest().to_vec()),
                })
            }
            TAG_LEVEL3_PAIR => {
                let mut r = Reader::new(rest);
                let side_ceiling_px = r.u32()?;
                let radar_lat = r.f64()?;
                let radar_lon = r.f64()?;
                let dvl_len = r.u32()? as usize;
                Some(Self::Level3Pair {
                    side_ceiling_px,
                    radar_lat,
                    radar_lon,
                    dvl: std::sync::Arc::new(r.take(dvl_len)?.to_vec()),
                    eet: std::sync::Arc::new(r.rest().to_vec()),
                })
            }
            TAG_SECTION => {
                let mut r = Reader::new(rest);
                let request = decode_section_request(&mut r)?;
                let input = RenderInput::from_bytes(r.rest())?;
                agree_on_product(request.product, &input)?;
                Some(Self::Section {
                    input: Box::new(input),
                    request,
                })
            }
            TAG_VOXELS => {
                let mut r = Reader::new(rest);
                let request = decode_voxel_request(&mut r)?;
                let input = RenderInput::from_bytes(r.rest())?;
                agree_on_product(request.product, &input)?;
                Some(Self::Voxels {
                    input: Box::new(input),
                    request,
                })
            }
            TAG_DECODE => Some(Self::Decode {
                archive: std::sync::Arc::new(rest.to_vec()),
            }),
            TAG_OVERLAY => {
                let mut r = Reader::new(rest);
                let width = r.u32()?;
                let height = r.u32()?;
                // Refused at the boundary for `decode_voxel_request`'s reason:
                // these bytes arrive on a message port and the two numbers are
                // what [`execute`]'s overlay arm allocates a `width × height`
                // pixmap from, so without a ceiling a malformed job is a
                // multi-gigabyte allocation rather than a refusal. The ceiling
                // is the largest raster *any* target of this workspace
                // affords — the desktop plan-view side, squared — which every
                // real overlay plan sits under (a plan never exceeds the
                // adapter's texture limit, and its pixel count is the viewport
                // plus a quarter overdraw). A zero side is refused with it:
                // the rasterizer would answer a zero-length buffer whose
                // "success" no consumer could tell from a failure.
                let ceiling = crate::constants::DESKTOP_RASTER_SIDE_CEILING as u64;
                let pixels = u64::from(width) * u64::from(height);
                if width == 0 || height == 0 || pixels > ceiling * ceiling {
                    return None;
                }
                let bounds = rustdar_overlays::types::GeoBounds {
                    min_lat: r.f64()?,
                    max_lat: r.f64()?,
                    min_lon: r.f64()?,
                    max_lon: r.f64()?,
                };
                let input = decode_overlay_input(&mut r)?;
                // The site list is length-counted rather than "the rest", so
                // unlike the archive arms nothing may follow it: trailing
                // bytes mean the two builds' layouts disagree.
                r.rest().is_empty().then_some(())?;
                Some(Self::Overlay {
                    width,
                    height,
                    bounds,
                    input,
                })
            }
            _ => None,
        }
    }

    /// The largest raster side this job's render may produce.
    ///
    /// **Four bytes on the wire, and a size rather than a flag.** It used to be
    /// one `full_res` byte selecting between two constants, which could only
    /// ever answer "the long-range size" or "the base size" — and the
    /// long-range size was itself a literal, so a device offering eight times
    /// it per axis was told about none of that. What the renderer needs is one
    /// number, "how big a texture is this result allowed to become", and there
    /// are two callers who know one:
    ///
    ///   * **A loop frame** says [`crate::constants::LOOP_IMAGE_SIZE`]. A loop
    ///     holds frames by the dozen, so it renders leaner by policy — it
    ///     already drops the value grid for the same reason.
    ///   * **A static render** says what the device can hold,
    ///     `crate::constants::raster_side_ceiling_px` of this adapter's
    ///     `max_texture_dimension_2d`, which is a real measurement of a real
    ///     device rather than a class the build guessed at. A handheld that
    ///     reports the GLES floor still says the base size and still gets a
    ///     correct picture rather than a texture creation that fails.
    ///
    /// Neither is "the picture the display always made": the extent is the
    /// sweep's either way, and `rustdar_radar::types::raster_side_px` spends
    /// this ceiling only as far as the sweep's own gates justify. The figure is
    /// resolved at the dispatch site rather than here because this type travels
    /// to a worker that has no device to ask.
    ///
    /// The [`JobRequest::Section`] and [`JobRequest::Voxels`] arms carry no
    /// ceiling: a section's raster is a constant of the view (`xsect`'s
    /// `SECTION_WIDTH`) and a voxel grid's shape is already on the wire.
    fn side_ceiling_px(&self) -> usize {
        match self {
            Self::Radar {
                side_ceiling_px, ..
            }
            | Self::Level3 {
                side_ceiling_px, ..
            }
            | Self::Level3Pair {
                side_ceiling_px, ..
            } => *side_ceiling_px as usize,
            // An overlay's raster is not ceiling-sized at all: the texture's
            // exact dimensions are the request's own `width` and `height`.
            Self::Section { .. }
            | Self::Voxels { .. }
            | Self::Decode { .. }
            | Self::Overlay { .. } => 0,
        }
    }

    /// For the timing log, so a slow job says which kind it was.
    fn kind(&self) -> &'static str {
        match self {
            Self::Radar { input, .. } => match input.product() {
                rustdar_radar::types::RadarProduct::NormalizedRotation => "radar/nrot",
                rustdar_radar::types::RadarProduct::StormRelativeVelocity => "radar/srv",
                _ => "radar",
            },
            Self::Level3 { .. } => "level3",
            Self::Level3Pair { .. } => "level3/vild",
            Self::Section { .. } => "section",
            Self::Voxels { .. } => "voxels",
            Self::Decode { .. } => "decode",
            Self::Overlay { input, .. } => match input {
                OverlayJobInput::Sites(_) => "overlay/sites",
            },
        }
    }
}

/// The product is on the wire twice — once in the request's own geometry and
/// once inside the [`RenderInput`] — and two statements of one fact can
/// disagree.
///
/// They must not be allowed to. A section of a moment the payload does not
/// carry does not fail: `VolumeSampler` builds no rung for it, every sample
/// comes back `NoCoverage`, and the raster is a full-size, correctly-shaped
/// picture of clear air. That is indistinguishable from a genuinely empty
/// section, so it is refused here rather than drawn.
///
/// The alternative — carrying the product only in the payload and filling the
/// request's field from it at decode — was rejected because it makes
/// [`JobRequest`] not round-trip: a caller who built an inconsistent pair would
/// get a *different* request back rather than a refusal, which moves the
/// disagreement from the wire into the type.
fn agree_on_product(wanted: rustdar_radar::types::RadarProduct, input: &RenderInput) -> Option<()> {
    (wanted == input.product()).then_some(())
}

/// A wire boolean, refusing anything that is not 0 or 1.
///
/// The two ends of a message port can be different builds, and a byte outside
/// the pair is a payload this one cannot read rather than a `true` to guess at
/// — the same refusal `values_wanted` has always made, now spelt once for the
/// several flags that make it.
fn flag(byte: u8) -> Option<bool> {
    match byte {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

fn encode_section_request(out: &mut Vec<u8>, request: &SectionRequest) {
    out.extend_from_slice(&request.product.wire_code().to_le_bytes());
    out.extend_from_slice(&request.start.0.to_le_bytes());
    out.extend_from_slice(&request.start.1.to_le_bytes());
    out.extend_from_slice(&request.end.0.to_le_bytes());
    out.extend_from_slice(&request.end.1.to_le_bytes());
    match request.top_km_msl {
        None => out.push(0),
        Some(top) => {
            out.push(1);
            out.extend_from_slice(&top.to_le_bytes());
        }
    }
}

fn decode_section_request(r: &mut Reader) -> Option<SectionRequest> {
    let product = rustdar_radar::types::RadarProduct::from_wire_code(r.u16()?)?;
    Some(SectionRequest {
        start: (r.f64()?, r.f64()?),
        end: (r.f64()?, r.f64()?),
        top_km_msl: match r.u8()? {
            0 => None,
            1 => Some(r.f64()?),
            _ => return None,
        },
        product,
    })
}

fn encode_voxel_request(out: &mut Vec<u8>, request: &VoxelRequest) {
    out.push(u8::from(request.values_wanted));
    out.extend_from_slice(&request.product.wire_code().to_le_bytes());
    out.extend_from_slice(&request.centre.0.to_le_bytes());
    out.extend_from_slice(&request.centre.1.to_le_bytes());
    // Tagged rather than sent as a sentinel width, the same shape the storm
    // motion override above is sent in: `None` means "as wide as the volume
    // reaches", which is a decision `build_voxels` makes on the worker side
    // with the volume in hand, and no f64 can stand for it without also being
    // a width somebody could legitimately ask for.
    // East then north, and both always written when the tag says `Some`: the
    // two axes are independent, so a wire that carried one and squared it on
    // the far side would silently resample ground the pane is not framing.
    match request.half_extent_km {
        None => out.push(0),
        Some(half) => {
            out.push(1);
            out.extend_from_slice(&half.east_km.to_le_bytes());
            out.extend_from_slice(&half.north_km.to_le_bytes());
        }
    }
    out.extend_from_slice(&request.base_km_msl.to_le_bytes());
    out.extend_from_slice(&request.top_km_msl.to_le_bytes());
    // `u16` per axis rather than `u8`: `MAX_AXIS` is 1625, which does not fit
    // in a byte, and a wrapped axis would arrive as a shorter one rather than
    // as an error. It fits a `u16` with room to spare, and
    // `the_arithmetic_bound_is_the_largest_cubable_axis` is what keeps this
    // encoding and that bound agreeing if the bound moves again.
    for n in [request.shape.nx, request.shape.ny, request.shape.nz] {
        out.extend_from_slice(&(n as u16).to_le_bytes());
    }
}

fn decode_voxel_request(r: &mut Reader) -> Option<VoxelRequest> {
    let values_wanted = match r.u8()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let product = rustdar_radar::types::RadarProduct::from_wire_code(r.u16()?)?;
    let request = VoxelRequest {
        centre: (r.f64()?, r.f64()?),
        half_extent_km: match r.u8()? {
            0 => None,
            1 => Some(rustdar_radar::voxel::HalfExtentKm {
                east_km: r.f64()?,
                north_km: r.f64()?,
            }),
            _ => return None,
        },
        base_km_msl: r.f64()?,
        top_km_msl: r.f64()?,
        product,
        shape: VoxelShape {
            nx: r.u16()? as usize,
            ny: r.u16()? as usize,
            nz: r.u16()? as usize,
        },
        values_wanted,
    };
    // `build_voxels` refuses an unsupported shape too, and logs it — but that
    // refusal happens after the whole payload has been decoded and the sampler
    // built. Refusing here keeps the same rule at the boundary where the bytes
    // are untrusted, and it is the shape check that `is_supported` owns rather
    // than a second copy of the bounds.
    //
    // The **cell count** is checked beside it, and that half is new since
    // `MAX_AXIS` stopped being the 256 a GLES 3.0 device guarantees.
    // `is_supported` now admits 1625 an axis, which is 4.29 *billion* cells —
    // the bound is on what `VoxelShape::cells` can represent, not on what a
    // machine can hold, and unlike `VoxelGrid::from_bytes` there is no payload
    // in hand here whose length would have to match. A request is thirty-odd
    // bytes and `build_voxels` allocates the grid it names, so without this a
    // malformed job would be a multi-gigabyte allocation rather than a refusal.
    // `VOXEL_TEXTURE_BUDGET_BYTES` is one byte per cell of the largest index
    // plane this workspace produces, which is exactly the ceiling wanted: every
    // shape any tier can ask for is at or under it.
    let affordable = request.shape.cells() <= rustdar_radar::voxel::VOXEL_TEXTURE_BUDGET_BYTES;
    (request.shape.is_supported() && affordable).then_some(request)
}

const TAG_RADAR: u8 = 1;
const TAG_LEVEL3: u8 = 2;
/// Tag 3 was the Level III SRM derivation job, retired when storm-relative
/// velocity became a Level II product; the number stays reserved so a stale
/// worker's job cannot be misread as a future kind.
#[allow(dead_code)]
const TAG_SRM_RETIRED: u8 = 3;
/// The two-object Level III derivation: VIL density. Its product is not on the
/// wire — the tag names it, because there is exactly one such product and a
/// wire code would let a mismatched pair claim to be another one.
const TAG_LEVEL3_PAIR: u8 = 4;
/// A vertical cross-section. **5, not 4** — the next free number, not the next
/// one that looks free. Posted as tag 4 a section lands in the
/// [`TAG_LEVEL3_PAIR`] arm, which reads two `f64`s and a `u32` length and takes
/// the rest: on a section's plausible bytes that *succeeds*, and renders a
/// VIL-density product out of cross-section geometry.
const TAG_SECTION: u8 = 5;
/// A Cartesian voxel grid.
const TAG_VOXELS: u8 = 6;
/// A Level II archive volume to decode. The one job that is not a render.
const TAG_DECODE: u8 = 7;
/// An overlay rasterization. The kind *within* the overlay is a second byte —
/// [`OVERLAY_INPUT_SITES`] — inside the payload, so overlay kinds can be added
/// without spending a job tag each.
const TAG_OVERLAY: u8 = 8;

/// [`OverlayJobInput::Sites`]'s code inside a [`TAG_OVERLAY`] payload.
///
/// Starts at 1, leaving 0 unallocated on this inner wire exactly as the outer
/// tag space leaves it: a zeroed buffer must never decode.
const OVERLAY_INPUT_SITES: u8 = 1;

/// The variant's own bytes after [`JobRequest::Overlay`]'s fixed header:
/// one input-kind byte, then the kind's fields.
fn encode_overlay_input(out: &mut Vec<u8>, input: &OverlayJobInput) {
    match input {
        OverlayJobInput::Sites(sites) => {
            out.push(OVERLAY_INPUT_SITES);
            out.extend_from_slice(&sites.zoom.to_le_bytes());
            out.push(u8::from(sites.is_dark));
            out.extend_from_slice(&sites.device_scale.to_le_bytes());
            // Count-prefixed, then each row; the name last per row, length-
            // prefixed. A name is a catalogue identifier — four bytes for
            // every WSR-88D — so the `u16` is generous; the truncation keeps
            // the encoder total on input it will never see, and a cut that
            // split a multi-byte character is refused by the decoder's UTF-8
            // check rather than misread.
            out.extend_from_slice(&(sites.sites.len() as u32).to_le_bytes());
            for site in &sites.sites {
                out.extend_from_slice(&site.lat.to_le_bytes());
                out.extend_from_slice(&site.lon.to_le_bytes());
                out.push(u8::from(site.is_current));
                out.push(u8::from(site.is_loading));
                let name = &site.name.as_bytes()[..site.name.len().min(usize::from(u16::MAX))];
                out.extend_from_slice(&(name.len() as u16).to_le_bytes());
                out.extend_from_slice(name);
            }
        }
    }
}

/// The inverse of [`encode_overlay_input`]. `None` for an input kind this
/// build does not have, a flag byte outside `{0, 1}`, a name that is not
/// UTF-8, or a buffer shorter than its own count claims.
///
/// The count is read but never trusted with an allocation: the rows are
/// pushed as they decode, so a count claiming four billion sites fails on
/// [`Reader::take`]'s first short read instead of reserving for a list the
/// buffer cannot hold.
fn decode_overlay_input(r: &mut Reader) -> Option<OverlayJobInput> {
    match r.u8()? {
        OVERLAY_INPUT_SITES => {
            let zoom = r.f64()?;
            let is_dark = flag(r.u8()?)?;
            let device_scale = r.f32()?;
            let count = r.u32()? as usize;
            let mut sites = Vec::new();
            for _ in 0..count {
                let lat = r.f64()?;
                let lon = r.f64()?;
                let is_current = flag(r.u8()?)?;
                let is_loading = flag(r.u8()?)?;
                let name_len = usize::from(r.u16()?);
                let name = std::str::from_utf8(r.take(name_len)?).ok()?.to_owned();
                sites.push(rustdar_overlays::render::rasterize::RadarSiteInfo {
                    name,
                    lat,
                    lon,
                    is_current,
                    is_loading,
                });
            }
            Some(OverlayJobInput::Sites(
                rustdar_overlays::render::rasterize::SitesInput {
                    sites,
                    zoom,
                    is_dark,
                    device_scale,
                },
            ))
        }
        _ => None,
    }
}

/// A bounds-checked cursor over a job's fixed-width header.
///
/// Every accessor answers `None` rather than panicking: these bytes arrive on a
/// message port and are not trusted. The variable-length tail is whatever
/// [`rest`](Reader::rest) is left holding, so no length prefix can lie about it.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// Rewrite an RGBA8 raster from the rasterizers' straight alpha into the
/// premultiplied bytes a [`egui::Color32`] *is*, in place.
///
/// # This is the whole of the premultiply, and it is deliberately not new
/// arithmetic
///
/// It calls [`egui::Color32::from_rgba_unmultiplied`] — the same function every
/// consumer used to call on this buffer — and writes the four bytes back where
/// they came from. `Color32::from_rgba_premultiplied` is
/// `Self([r, g, b, a])`, a constructor that computes nothing, so a consumer
/// reading this buffer through it lands on exactly the `Color32` the old
/// consumer computed. Not "within a tolerance": the same call, on the same
/// inputs, in the same order, moved earlier. `premultiply_tests` proves it
/// exhaustively — the conversion is per-channel independent given the alpha, so
/// 256 × 256 pairs cover every pixel that can exist.
///
/// Reimplementing the arithmetic here would have been the mistake. It is not
/// `channel * alpha / 255`: the α = 0 arm answers `TRANSPARENT` rather than a
/// zeroed channel triple, the α = 255 arm skips the multiply, and the arm in
/// between reads a 64 KiB lookup table `ecolor` builds once. Any of the three
/// written out by hand is a picture that shifts.
///
/// In place, because the buffer is a plan view's `POOLED_IMAGE` texture or a
/// section's pooled plane and the point of the pools is that neither is
/// reallocated per render. A second buffer here would hand back the allocation
/// those slots exist to avoid, at the raster sizes where it matters most —
/// 206.75 MiB at the 7362 px desktop ceiling.
fn premultiply_raster(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let converted =
            egui::Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3]);
        pixel.copy_from_slice(&converted.to_array());
    }
}

/// [`execute`]'s output stage: every raster an output carries leaves in egui's
/// premultiplied convention.
///
/// # Why it is here and not at the consumer
///
/// Because *here* is wherever the job ran, and the consumer is wherever the
/// picture is drawn, and on the web those are two different threads. The
/// per-pixel walk this performs is 4.2–4.6 ms at the 2048 px browser ceiling
/// against a 16.7 ms frame budget, and it used to be spent on the browser's
/// main thread; it is now spent in the worker. Natively both were already the
/// same spawned thread, so what moves there is the line and not the cost —
/// except for the static cross-section, whose upload
/// (`app_render::upload_section_raster`) converted **on the frame thread** on
/// both targets and now converts on neither.
///
/// A `Voxels` output carries no raster. It is listed rather than caught by a
/// wildcard so that a fourth output kind carrying pixels has to say here
/// whether it is premultiplied, instead of silently declining to be.
fn premultiplied(output: JobOutput) -> JobOutput {
    match output {
        JobOutput::Frame(mut frame) => {
            premultiply_raster(&mut frame.image);
            JobOutput::Frame(frame)
        }
        JobOutput::Section(mut section) => {
            premultiply_raster(section.image_mut());
            JobOutput::Section(section)
        }
        JobOutput::Voxels(grid) => JobOutput::Voxels(grid),
        // A decoded volume carries gate codes, not pixels.
        JobOutput::Volume(volume) => JobOutput::Volume(volume),
        // Already premultiplied: [`execute`]'s overlay arm converted at the
        // rasterizer's own declaration, which is the one place the declared
        // `AlphaMode` is still in hand. Running the table again here would
        // double-multiply every translucent pixel.
        JobOutput::OverlayRaster(rgba) => JobOutput::OverlayRaster(rgba),
    }
}

/// Do the work.
///
/// Pure, and the *only* implementation: the worker calls it, the native thread
/// calls it, and the inline fallback calls it. That is what makes a frame
/// rendered in a worker byte-identical to one rendered on this thread — the
/// two are not two renderers that agree, they are one renderer.
///
/// Every raster that leaves here is **premultiplied**, which is the one thing
/// this function does that the rasterizers underneath it do not — see
/// [`premultiplied`], and [`premultiply_raster`] for why the conversion is a
/// call to egui's own constructor rather than arithmetic written out again.
/// It runs here rather than at the consumer because here is off the browser's
/// main thread and off the frame thread on both targets.
///
/// "Pure" is a claim about what it *returns*, and it survives four pieces of
/// process-wide state underneath, all of them buffer pools and all admissible
/// for the same one reason:
///
/// * the plan-view rasterizer carries its cell buffer between calls
///   (`rustdar_radar::render`'s `POOLED_CELLS`);
/// * the section rasterizer carries its three planes between cuts
///   (`rustdar_radar::xsect`'s `POOLED_PLANES`), which the `Section` arm below
///   reaches through `render_section`; and
/// * the plan-view rasterizer carries the RGBA texture and the value grid it
///   answers with (`rustdar_radar::render`'s `POOLED_IMAGE` and
///   `POOLED_VALUES`).
///
/// No buffer can be handed out in any state but the one a fresh allocation would
/// be in — drained for the cells, re-seeded and resized to the raster for the
/// planes and the texture, empty for the grid — so no call can observe
/// another's. The byte-identity above was re-measured across five sites and
/// twelve products with the first in place, across nine volumes at eight sites
/// and six cut geometries with the second, and across seven sites, six products
/// and three image sides with the last two. Anything added below that *is*
/// observable between calls breaks the worker equivalence this paragraph
/// promises, because a worker does not share this process's memory.
///
/// The last pair is also the one place this function *writes* to that state:
/// `From<SweepRender> for RenderedFrame` hands the raster value grid back to
/// the renderer's slot on every path through here. That is still a claim about
/// a buffer and not about a result — what the next call receives is a re-seeded
/// buffer, which is what it would have allocated, so a call that finds the slot
/// full and one that finds it empty return the same bytes. Worker equivalence is untouched by it and was re-measured with
/// the write in place; what a worker's write reaches is that instance's own
/// slot, in its own linear memory, which is a fact about where the win lands
/// and not about what comes back.
pub fn execute(request: &JobRequest) -> JobResult {
    // Read once, off the request, so the three rasterizing arms cannot come to
    // disagree about how large a picture this job was allowed to make.
    let side_ceiling_px = request.side_ceiling_px();
    let output = match request {
        JobRequest::Radar {
            input,
            values_wanted,
            ..
        } => rustdar_radar::render::render_from_sized(input, side_ceiling_px).map(|render| {
            let mut frame = RenderedFrame::from(render);
            if !*values_wanted {
                // A loop frame keeps its geometry and drops its numbers. 5.03
                // MiB apiece across a loop of up to 36 frames is not affordable
                // and does not have to be paid: the volume the frame was
                // rendered from is resident for as long as the loop lives, and
                // 5.8 KiB of wedges is what turns a point back into a gate of
                // it. See `rustdar_radar::hover::SweepGates`.
                //
                // A *Level II* loop frame is the one render that reaches this
                // arm. The Level III loop has no `values_wanted` to reach it by
                // and strips at the consumer instead; `app_fetch`'s `deliver`
                // is the site, and stripping an already-stripped field there is
                // what makes the one call safe for both.
                frame.polar.strip_values();
            }
            JobOutput::Frame(frame)
        }),
        JobRequest::Level3 {
            bytes,
            product,
            radar_lat,
            radar_lon,
            ..
        } => decode_level3(bytes).and_then(|message| {
            rustdar_radar::render::render_level3_message_to_image_sized(
                &message,
                *product,
                *radar_lat,
                *radar_lon,
                side_ceiling_px,
            )
            .map(Into::into)
            .map(JobOutput::Frame)
        }),
        JobRequest::Level3Pair {
            dvl,
            eet,
            radar_lat,
            radar_lon,
            ..
        } => match (decode_level3(dvl), decode_level3(eet)) {
            (Some(dvl), Some(eet)) => rustdar_radar::render::render_derived_vild_to_image_sized(
                &dvl,
                &eet,
                *radar_lat,
                *radar_lon,
                side_ceiling_px,
            )
            .map(Into::into)
            .map(JobOutput::Frame),
            // One of the two did not decode, which `decode_level3` has already
            // logged: nothing to draw, the same answer a missing sweep gets.
            _ => None,
        },
        // The `Scan` is rebuilt from the payload and dropped again here, which
        // is the same shape the `Radar` arm has: one renderer, run wherever the
        // job landed, rather than a worker-side reimplementation that could
        // come to disagree with the main thread's.
        // The storm motion override rides the `RenderInput` — the lane the
        // plan-view SRV render already uses — and is threaded here into the
        // derivation seam both vertical renderers share. The RPG's own vector
        // rides the same payload, one field over, and is threaded through
        // beside it: the two are rungs of one chain and the derivation is what
        // arbitrates between them, so a caller that passed only the override
        // would silently demote every vertical SRV cut to a derived rung while
        // the map beside it used the RPG's.
        //
        // So does the declared Nyquist table, and it has to be lifted back out
        // separately: `to_scan` rebuilds model types, and the model type is
        // precisely what dropped the number. Pairing the two here is what
        // keeps this thread's velocity fold guard on the same limits the
        // thread that extracted the payload used.
        JobRequest::Section { input, request } => {
            let (scan, declared) = (input.to_scan(), input.declared_nyquist());
            rustdar_radar::xsect::render_section(
                rustdar_radar::nyquist::Volume::new(&scan, &declared),
                request,
                input.radar_lat(),
                input.radar_lon(),
                input.storm_motion(),
            )
            .map(|section| JobOutput::Section(Box::new(section)))
        }
        // The one arm that produces a volume rather than consuming one. It is
        // also the one arm whose input is bigger than a pointer: `File::new`
        // takes the archive by value, so the bytes are cloned out of the `Arc`
        // here. That is one 16 MB memcpy against a decode of ~1000 ms in a
        // browser, and it happens wherever the job ran rather than on the
        // thread that asked for it.
        JobRequest::Decode { archive } => {
            match rustdar_radar::scan::decode_bytes(archive.as_ref().clone()) {
                Ok(volume) => Some(JobOutput::Volume(Box::new(volume))),
                Err(e) => {
                    // "Nothing to draw", which is what every other arm's
                    // failure already means, and what the caller's `deliver`
                    // already handles: the fetch reports it and the pane keeps
                    // whatever it had.
                    log::error!("could not decode a Level II volume: {e}");
                    None
                }
            }
        }
        JobRequest::Voxels { input, request } => {
            let (scan, declared) = (input.to_scan(), input.declared_nyquist());
            rustdar_radar::voxel::build_voxels_with_motion(
                rustdar_radar::nyquist::Volume::new(&scan, &declared),
                request,
                input.radar_lat(),
                input.radar_lon(),
                input.storm_motion(),
            )
            .map(|grid| JobOutput::Voxels(Box::new(grid)))
        }
        JobRequest::Overlay {
            width,
            height,
            bounds,
            input,
        } => {
            let output = match input {
                OverlayJobInput::Sites(sites) => {
                    rustdar_overlays::render::rasterize::rasterize_radar_sites(
                        sites, bounds, *width, *height,
                    )
                }
            };
            // The output contract is **premultiplied, always** — stated on
            // [`JobOutput::OverlayRaster`] — where each rasterizer's own
            // convention is whatever it declares. The sites rasterizer
            // declares premultiplied on every path (tiny-skia's pixmap *is*
            // premultiplied), so the arm below is dead for it today; it is
            // written now because the model-grid rasterizer declares straight
            // alpha and reaches this seam in a later slice, and a seam that
            // silently dropped the declaration would ship it double-bright.
            //
            // The conversion is [`premultiply_raster`] — egui's own
            // constructor per pixel, the exact call the frame-thread consumer
            // used to make — so via-wire and direct bytes stay identical.
            let mut rgba = output.rgba;
            match output.alpha {
                rustdar_overlays::render::rasterize::AlphaMode::Premultiplied => {}
                rustdar_overlays::render::rasterize::AlphaMode::Straight => {
                    premultiply_raster(&mut rgba)
                }
            }
            // `output.hit_map` is dropped, correctly *for the kinds described
            // so far*: the sites rasterizer answers `None` on every path. The
            // hit-map kinds cannot take this arm until the id-image reply
            // exists — that is the next-but-one slice, and their inputs are
            // not describable here yet either.
            Some(JobOutput::OverlayRaster(rgba))
        }
    };
    // One place, after every arm, so no rasterizing arm can be added that
    // forgets it — the alternative is five call sites and a sixth that does not
    // exist yet.
    output.map(premultiplied)
}

/// The product these bytes decode to, or `None` — which the caller reports as a
/// render that drew nothing, the same answer a scan with no matching sweep gets.
fn decode_level3(bytes: &[u8]) -> Option<nexrad_level3::model::Level3Message> {
    match nexrad_level3::decode::decode_product(bytes) {
        Ok(message) => Some(message),
        Err(e) => {
            log::error!("could not decode a Level III product for rendering: {e}");
            None
        }
    }
}

/// [`execute`] straight off the wire, for a worker that holds bytes rather than
/// a `JobRequest`. `None` for a payload it cannot read, which the caller
/// reports back as a failed job rather than dropping silently.
pub fn execute_bytes(bytes: &[u8]) -> JobResult {
    execute(&JobRequest::from_bytes(bytes)?)
}

/// The reverse of the non-frame half of a worker reply: a
/// [`RenderView::wire_code`](rustdar_radar::types::RenderView::wire_code) byte
/// and the payload type's own bytes, back into a [`JobOutput`].
///
/// Here rather than in `rustdar-web` for the reason [`execute_bytes`] is here:
/// the browser crate is the adapter, this crate owns what a job means, and a
/// decode that lived over there would be reachable only from a browser. It also
/// keeps `rustdar-web` from needing a `rustdar-radar` dependency of its own.
///
/// `None` for a kind byte this build does not have, for a payload the type's
/// own codec refuses, and for a `PlanView` tag — a frame does not travel this
/// way, and a reply that says it does comes from a build whose protocol is not
/// this one. All three are "nothing to draw", which is what a failed render has
/// always meant, and all three still deliver.
pub fn decode_output(kind: u8, bytes: &[u8]) -> Option<JobOutput> {
    match kind {
        OUT_KIND_SECTION => {
            CrossSection::from_bytes(bytes).map(|section| JobOutput::Section(Box::new(section)))
        }
        OUT_KIND_VOXELS => {
            VoxelGrid::from_bytes(bytes).map(|grid| JobOutput::Voxels(Box::new(grid)))
        }
        OUT_KIND_VOLUME => rustdar_radar::scan::DecodedScan::from_bytes(bytes)
            .map(|volume| JobOutput::Volume(Box::new(volume))),
        // Raw RGBA: no codec to refuse it here, so acceptance is deferred to
        // the dispatch's own length check — see [`OUT_KIND_OVERLAY`] for why
        // that is the guard rather than a magic.
        OUT_KIND_OVERLAY => Some(JobOutput::OverlayRaster(bytes.to_vec())),
        _ => {
            log::error!(
                "a worker sent an out-of-band payload tagged {kind}, which this build has no decoder for"
            );
            None
        }
    }
}

/// The gates behind a rendered frame, back from the bytes a worker posted.
///
/// Here for the two reasons [`decode_output`] is here — the browser crate is
/// the adapter and this crate owns what a job means, and it keeps `rustdar-web`
/// from needing a `rustdar-radar` dependency of its own — and for a third: a
/// frame arrives through the `IMAGE` field rather than through `OUT`, so it
/// does not go past `decode_output` at all.
///
/// An empty field for anything this build did not write. That is the same
/// answer a loop frame gives on purpose, and it reads the same way at the far
/// end: a readout with no gates to find says nothing rather than panicking on a
/// slice index in a browser, where nobody would see the panic.
pub fn decode_polar(bytes: &[u8]) -> rustdar_radar::render::polar::PolarField {
    rustdar_radar::render::polar::PolarField::from_bytes(bytes).unwrap_or_else(|| {
        log::error!(
            "a worker sent {} bytes of gates this build cannot read",
            bytes.len()
        );
        rustdar_radar::render::polar::PolarField::default()
    })
}

// ── The job sink ─────────────────────────────────────────────────────────────

/// A place to send [`JobRequest`]s that is not this thread.
///
/// Implemented by `rustdar-web` over a dedicated `Worker`. It is a trait, and
/// installed rather than constructed here, because the dependency runs the
/// other way: `rustdar-web` depends on this crate, and nothing in this crate
/// may reach back for `web-sys`.
///
/// # The payload is a `JobRequest`, not bytes
///
/// [`send`](Self::send) takes the request **by value**, and that is the whole
/// point of the shape. What both transports actually implement is one
/// operation — **handover**: `postMessage` with a transfer list is a move, and
/// so is a channel send. They differ only in what the platform charges for it.
/// A browser cannot hand over anything but a detachable buffer, so
/// `rustdar-web`'s implementation serialises; a transport that can move an
/// owned value has no reason to, and must not be made to.
///
/// This used to take a `Vec<u8>`, which meant the funnel called `to_bytes` on
/// behalf of every implementation including ones that did not want it. The
/// serialisation now lives inside the implementation that needs it, and the
/// funnel names no representation at all.
pub trait JobSink {
    /// Hand `request` over to be executed. `id` comes back with the reply so
    /// the funnel can pair them.
    ///
    /// # Refusal returns the request
    ///
    /// `Err(request)` is "I could not take this", and it carries the job back
    /// so the caller can run it here instead of waiting for a reply that is not
    /// coming. That is why this is a `Result<(), JobRequest>` and not a `bool`:
    /// a `bool` forces the caller to keep a copy against a refusal it almost
    /// never sees, which is exactly the serialise-on-everyone's-behalf that
    /// taking bytes used to force. Giving it back costs an implementation
    /// nothing it would not already have: `JobRequest::to_bytes` **borrows**,
    /// so the browser's arm still owns the request after building the buffer it
    /// failed to post, and a moving arm has not moved it if the send failed.
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest>;
}

/// The state a posted job needs when its reply lands.
struct Pending {
    kind: &'static str,
    started: web_time::Instant,
    /// Which installed sink owes this job an answer.
    ///
    /// The registry below is one map for the whole process, so an entry has to
    /// say who is carrying it. [`abandon_worker`] fails **that sink's** jobs and
    /// no others, which is the difference between "the browser's worker died"
    /// and "every job anywhere is now cancelled" — and, under `cargo test`,
    /// between a test tearing down its own fake port and a test tearing down
    /// the port of whichever test happens to be running beside it.
    sink: u64,
    /// Holds the `RenderGuard`, the pane's `Arc<AtomicBool>` and the response
    /// channel. Consuming it is what decrements the render budget and clears
    /// the pane's in-flight mark, so it must run on *every* path out of the
    /// pending map — reply, worker loss, or shutdown.
    deliver: Box<dyn FnOnce(JobResult) + Send>,
}

/// Every job any sink in this process owes an answer for.
///
/// # Why this is process-wide and not thread-local
///
/// It used to be a `thread_local!`, which was exactly right while the only
/// transport was a browser Web Worker: a browser has one thread, so a
/// thread-local map is a process-wide map with a cheaper lock.
///
/// A native pool is the same architecture on a target that has more than one
/// thread, and it moves two things at once. `offload_job` is called from the
/// frame thread *and* from tokio's workers — `App::decode_offloaded` runs
/// inside a spawned future — so the registry has to be reachable from any of
/// them; and the reply is produced on a pool thread, which is not the thread
/// that submitted. A thread-local map would have made the pool answer into a
/// map nobody was holding.
///
/// The lock is uncontended on the browser (one thread) and held only for a
/// hash-map operation anywhere else. Nothing user-supplied runs under it: a
/// `deliver` is always called *after* its entry has been removed, so a job that
/// dispatches another job from inside its own delivery cannot deadlock.
static PENDING: std::sync::LazyLock<std::sync::Mutex<HashMap<u64, Pending>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// The registry, recovered from a poisoned lock rather than propagating the
/// panic.
///
/// A thread that panicked while holding this lock left a `HashMap` behind, not
/// a half-written one — every operation under it is a single insert, remove or
/// scan. Refusing to hand it back would turn one panicked render into an
/// application that can never dispatch or retire another job, and every pane
/// would wedge holding a render slot. See [`Pending::deliver`] for what a slot
/// that is never retired costs.
fn pending() -> std::sync::MutexGuard<'static, HashMap<u64, Pending>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Job ids, unique across the process because [`PENDING`] is.
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Sink ids. A fresh one per installed sink, so [`Pending::sink`] identifies the
/// *installation* and not merely the kind of transport: a port that is retired
/// and replaced does not inherit the jobs the previous one owed.
static NEXT_SINK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

thread_local! {
    /// The sink this thread hands jobs to, and the id its jobs are filed under.
    ///
    /// Thread-local rather than global because the browser's implementation
    /// owns a `web_sys::Worker`, which is `!Send` and can only ever be touched
    /// on the thread that created it. The *registry* above is shared; the
    /// handle to the transport is not, and does not need to be.
    static WORKER: RefCell<Option<(u64, Box<dyn JobSink>)>> = RefCell::new(installed(default_sink()));
}

/// The transport a thread starts with, before anything is installed over it.
///
/// **This is the module's one target fork, and it selects a transport rather
/// than a behaviour** — the same shape `rustdar-egui/src/tile_source.rs` has
/// carried for its two runtimes since before any of this existed. Both arms
/// answer the same question with the mechanism their platform has: natively a
/// handle to the process's job pool, and in a browser nothing, because a
/// browser's transport is a `Worker` that has to start and prove itself first
/// (`rustdar-web`'s `worker_port::attach`) and until it does there is genuinely
/// nowhere else for a job to run.
///
/// It replaces a fork that was **behavioural**: `offload` used to spawn an
/// unbounded OS thread on one arm and block the frame on the other, with no
/// id, no registry entry and no failure path on the arm that had threads.
#[cfg(not(target_arch = "wasm32"))]
fn default_sink() -> Option<Box<dyn JobSink>> {
    Some(pool::sink())
}

/// See the native arm.
#[cfg(target_arch = "wasm32")]
fn default_sink() -> Option<Box<dyn JobSink>> {
    None
}

/// Give a sink the id its jobs will be filed under.
fn installed(port: Option<Box<dyn JobSink>>) -> Option<(u64, Box<dyn JobSink>)> {
    let sink = NEXT_SINK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    port.map(|port| (sink, port))
}

/// Route [`offload_job`] through `port` from now on.
///
/// Called from `rustdar-web`'s `worker_port` every time a worker proves itself
/// with a build-token handshake — once at startup and once per respawn after a
/// loss. Until the first one lands, [`offload_job`] holds jobs for the window
/// [`expect_sink`] was armed with and then runs them inline, which is the
/// behaviour the web build had before any of this existed.
///
/// A port already installed here is **abandoned**, not dropped: it may be
/// carrying jobs, and a job whose sink is replaced out from under it would
/// otherwise sit in the registry forever holding a render slot that nothing
/// can ever release.
///
/// Whatever [`expect_sink`] was holding for this moment is handed over here,
/// oldest first. The hand-over runs whether or not anything was armed, so a
/// port installed by a path that never expects one — the native pool, a test's
/// port — costs one empty-queue check.
pub fn set_worker(port: Box<dyn JobSink>) {
    abandon_worker("replaced by a new port");
    WORKER.with(|w| *w.borrow_mut() = installed(Some(port)));
    hand_waiting_jobs_to_the_sink();
}

/// Give up on the worker: it died, or answered the handshake with a build that
/// is not this one.
///
/// Every job **it** still owes is failed rather than forgotten. Dropping them
/// would leak the render budget and leave panes marked in-flight forever;
/// failing them clears both, and the next frame re-dispatches — inline now,
/// because the port is gone.
///
/// Scoped to the retired sink's own jobs. The registry holds every job in the
/// process, and a browser worker dying says nothing about a job some other
/// sink is carrying.
///
/// **What [`expect_sink`] is holding is left alone.** Losing a worker is the
/// moment a replacement is started, not the moment waiting stops, and a queue
/// emptied here would run on the frame thread the jobs that the respawn is
/// about to have somewhere to put. The wait's own deadline is what ends it —
/// see [`expect_sink`].
pub fn abandon_worker(reason: &str) {
    let Some((sink, _port)) = WORKER.with(|w| w.borrow_mut().take()) else {
        return;
    };
    let orphaned: Vec<Pending> = {
        let mut registry = pending();
        let ids: Vec<u64> = registry
            .iter()
            .filter(|(_, job)| job.sink == sink)
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| registry.remove(id)).collect()
    };
    log::warn!(
        "rasterization worker abandoned ({reason}); failing {} in-flight job(s)",
        orphaned.len()
    );
    for job in orphaned {
        (job.deliver)(None);
    }
}

/// Whether jobs are currently going to a worker. For diagnostics and tests.
pub fn worker_attached() -> bool {
    WORKER.with(|w| w.borrow().is_some())
}

// ── The wait for a sink ──────────────────────────────────────────────────────

/// How many jobs a thread will hold while it waits for the sink it is
/// expecting, before the oldest is given up on.
///
/// A count and not a byte budget, which is worth saying plainly because the job
/// kinds here are not one size: a full queue of `Radar` requests is ~1.3 MB
/// apiece, and a full queue of [`JobRequest::Decode`]s is 16.9 MB apiece — 541
/// MB, on a target whose address space is 4 GiB. A byte budget would be the
/// honest bound and it is not available: a `JobRequest`'s cost is its payload's
/// length on two arms, a `RenderInput`'s owned gates on three more, and nothing
/// here can price the last without walking it.
///
/// What makes the count safe anyway is that this queue only fills while there
/// is **no sink at all** — a handshake in flight, or a worker being replaced —
/// and the window it fills for is the caller's own deadline, seconds rather
/// than minutes. Thirty-two is well past what the application dispatches into
/// such a window: renders are capped by `render_dispatch`'s concurrency limit,
/// and a decode is one per pane per fetch.
pub const SINK_WAIT_LIMIT: usize = 32;

/// A job held for the sink its thread is expecting: the name it was dispatched
/// under — which is all a log line has to identify it by, the same shape
/// [`DeferredDrop`] carries — the request, and the delivery that has to run on
/// every path out.
type WaitingJob = (&'static str, JobRequest, Box<dyn FnOnce(JobResult) + Send>);

/// A job that is **not** being held: the request and the delivery that still
/// owes its caller an answer, without the name a [`WaitingJob`] carries.
///
/// The name is dropped because the one consumer already has it — [`run_here`]
/// is called from the arm whose `name` this is — and carrying a second copy
/// would let the two disagree about which job a log line is describing.
type UnheldJob = (JobRequest, Box<dyn FnOnce(JobResult) + Send>);

/// What [`expect_sink`] armed, and what has arrived since.
struct SinkWait {
    /// When to stop waiting, or `None` for a thread expecting nothing — in
    /// which case a job that finds no sink runs where it stands, exactly as it
    /// did before any of this existed.
    until: Option<web_time::Instant>,
    /// A `VecDeque` for [`DEFERRED_DROPS`]' reason turned around: the hand-over
    /// takes from the front, so the job that has waited longest is the first to
    /// reach the sink — and the eviction takes from the front too, so the job
    /// given up on is the one whose caller has been waiting longest and is
    /// likeliest to have re-asked already.
    jobs: std::collections::VecDeque<WaitingJob>,
}

thread_local! {
    /// This thread's wait. Thread-local because [`WORKER`] is: what is being
    /// waited for is *that* handle, and a browser has one thread anyway.
    ///
    /// It exists on every target for the reason [`DEFERRED_DROPS`] does — the
    /// `cfg` in this module is over routing, never over whether a queue exists
    /// — which is what lets a host test drive the queue a browser will use.
    /// Nothing native arms it; see [`expect_sink`].
    static SINK_WAIT: RefCell<SinkWait> = const {
        RefCell::new(SinkWait {
            until: None,
            jobs: std::collections::VecDeque::new(),
        })
    };
}

/// Hold jobs for up to `within` instead of running them here, because a sink is
/// genuinely about to be installed.
///
/// # The state this exists for
///
/// A browser has no sink until its worker answers a build-token handshake, and
/// none again from the moment that worker is lost until a replacement answers
/// one. In both windows [`offload_job`] falls through to running the job on the
/// browser's **one** thread, and the audit's figure for the largest of them —
/// an existing in-code measurement, quoted from
/// [`JobRequest::Decode`] rather than re-taken here — is 1021.9 ms in Firefox
/// and 911.4 ms in Chrome for a 16.9 MB volume. Waiting a moment for the
/// transport that is on its way costs less than that, and costs it once.
///
/// # It arms nothing that already has somewhere to send
///
/// **A thread with a sink installed is expecting nothing, and this returns
/// without arming.** That is what keeps the native build out of the queue by
/// construction rather than by convention: `default_sink` is the job pool, so a
/// native thread has a sink from its first job and can never reach the arm at
/// all. The browser's adapter is the only caller either way — `rustdar-web`'s
/// `worker_port`, which is compiled for wasm32 alone.
///
/// # `within` is a window on one attempt, not on the whole recovery
///
/// The deadline should cover the handshake a caller has just started and no
/// more. A caller that is waiting out a backoff before it can even try again
/// must not arm across the wait: a job held for a minute is a pane blank for a
/// minute, which is worse than the stall this exists to remove. When the
/// deadline passes, everything held runs where it would have run anyway — see
/// [`flush_expired_sink_wait`], which is what a caller with no further jobs
/// coming needs so that "expired" does not mean "hung".
pub fn expect_sink(within: std::time::Duration) {
    if worker_attached() {
        return;
    }
    SINK_WAIT.with(|q| q.borrow_mut().until = Some(web_time::Instant::now() + within));
}

/// Whether this thread is holding jobs for a sink it expects. For diagnostics
/// and tests.
pub fn expecting_sink() -> bool {
    SINK_WAIT.with(|q| q.borrow().until.is_some())
}

/// How many jobs this thread is holding for a sink. For diagnostics and tests.
pub fn jobs_waiting_for_sink() -> usize {
    SINK_WAIT.with(|q| q.borrow().jobs.len())
}

/// Empty this thread's wait without running or delivering anything, for a test
/// whose queue is fixture rather than subject.
///
/// Deliberately not a production path: a `deliver` dropped rather than run is
/// the leaked render slot [`Pending::deliver`] warns about, and every path a
/// browser takes out of this queue ends in one running. What this exists for is
/// the **unwind**, which is [`InstalledTestWorker`]'s reason: a failed assertion
/// leaves this thread's wait armed and full, [`SINK_WAIT`] is a thread-local,
/// and the harness's next test on this thread would file its jobs into a queue
/// nobody is coming for — then fail for reasons that have nothing to do with it.
#[cfg(test)]
pub(crate) fn clear_sink_wait() {
    SINK_WAIT.with(|q| {
        let mut q = q.borrow_mut();
        q.until = None;
        q.jobs.clear();
    });
}

/// Run `request` where the caller stands, which is [`offload`]'s answer for
/// this target: a pool lane natively, this frame in a browser.
///
/// One function because three arms reach it — no sink and nothing expected, a
/// sink that refused, and a wait that ran out — and the three must not come to
/// disagree about where "here" is.
fn run_here(name: &'static str, request: JobRequest, deliver: Box<dyn FnOnce(JobResult) + Send>) {
    offload(name, move || deliver(execute(&request)));
}

/// Hold `request` for the sink this thread expects, or hand it back for
/// [`run_here`].
///
/// Returns `Some` in exactly the two cases that are today's behaviour: nothing
/// is expected, or the wait has run out. In the second the jobs already held go
/// with it — they run here too, which is where they would have run had the
/// queue never existed.
///
/// Nothing a caller wrote runs while the queue is borrowed. An evicted job's
/// `deliver` may dispatch another job, and that job would find this queue
/// borrowed and panic the frame; the borrow is released before any of it.
fn wait_for_sink(
    name: &'static str,
    request: JobRequest,
    deliver: Box<dyn FnOnce(JobResult) + Send>,
) -> Option<UnheldJob> {
    enum Outcome {
        /// Held. Carries whatever was evicted to make room.
        Held(Option<WaitingJob>),
        /// Not held, and everything that was held comes back with it.
        RunHere(Vec<WaitingJob>, WaitingJob),
    }

    let now = web_time::Instant::now();
    let outcome = SINK_WAIT.with(|q| {
        let mut q = q.borrow_mut();
        match q.until {
            None => Outcome::RunHere(Vec::new(), (name, request, deliver)),
            Some(until) if now >= until => {
                q.until = None;
                Outcome::RunHere(q.jobs.drain(..).collect(), (name, request, deliver))
            }
            Some(_) => {
                q.jobs.push_back((name, request, deliver));
                // Push then evict, so the bound is on what is *held*: the
                // 33rd arrival leaves 32 behind it, and the one given up on is
                // the oldest rather than the newest.
                Outcome::Held(if q.jobs.len() > SINK_WAIT_LIMIT {
                    q.jobs.pop_front()
                } else {
                    None
                })
            }
        }
    });

    match outcome {
        Outcome::Held(evicted) => {
            if let Some((evicted_name, _request, deliver)) = evicted {
                // `None` is "the job produced nothing", which every consumer
                // already handles and which is exactly what `abandon_worker`
                // hands a job whose worker died. **What it costs is not the
                // same at every call site**, and the three differ enough to be
                // worth writing down rather than summarised as "the callers
                // re-ask":
                //
                //  * `radar-render` is level-triggered and costs a frame.
                //    `App::dispatch_pane_renders` re-dispatches any pane whose
                //    `last_rendered` does not match its selection, and a `None`
                //    clears `render_in_flight` without setting `last_rendered`.
                //  * `loop-render` and `loop-section` cost the *frame of the
                //    loop*. `accept_render_result` marks a reply with no image
                //    `render_failed`, and the planner skips a failed frame — so
                //    that one frame stays blank until something invalidates the
                //    flags, which a product or elevation change does.
                //  * `level2-decode` costs the request. `app_fetch` turns
                //    `None` into a "could not decode the volume" the user sees,
                //    and nothing re-asks for a scrub.
                //
                // That is the trade at the bound — 32 jobs deep with no sink at
                // all — against a queue that grows without one. It is reached
                // only while nothing is installed, and the deadline is what
                // keeps that window short.
                log::warn!(
                    "{evicted_name}: {SINK_WAIT_LIMIT} jobs are already waiting for a sink; \
                     giving up on the oldest"
                );
                deliver(None);
            }
            None
        }
        Outcome::RunHere(held, (_, request, deliver)) => {
            for (held_name, held_request, held_deliver) in held {
                log::warn!("{held_name}: the sink never arrived; running the job here");
                run_here(held_name, held_request, held_deliver);
            }
            Some((request, deliver))
        }
    }
}

/// Run everything a lapsed [`expect_sink`] is still holding, here.
///
/// **This is what makes "the wait expires" different from "the wait hangs".**
/// [`wait_for_sink`] notices a lapsed deadline only when another job arrives,
/// and the case this exists for is precisely the one where no other job is
/// coming: a browser whose worker never answers, holding the volume decode the
/// first paint is waiting on. The caller that armed the wait is the caller that
/// has to come back for it — `rustdar-web`'s `worker_port` schedules a timer
/// for the deadline it asked for.
///
/// A no-op for a deadline that has not passed, and for one that was re-armed
/// past this call's timer, so a caller may schedule as many as it starts.
pub fn flush_expired_sink_wait() {
    let held: Vec<WaitingJob> = SINK_WAIT.with(|q| {
        let mut q = q.borrow_mut();
        match q.until {
            Some(until) if web_time::Instant::now() >= until => {
                q.until = None;
                q.jobs.drain(..).collect()
            }
            _ => Vec::new(),
        }
    });
    for (name, request, deliver) in held {
        log::warn!("{name}: the sink never arrived; running the job here");
        run_here(name, request, deliver);
    }
}

/// Give the sink just installed everything that was being held for it, oldest
/// first, and stop waiting.
///
/// Each job goes back through [`dispatch`] rather than to the sink directly, so
/// a port that refuses one still falls through to [`run_here`] — the queue
/// hands jobs to the funnel, not around it. It cannot recurse: the queue is
/// emptied before the first re-dispatch, and a sink is installed by the time
/// any of them runs.
fn hand_waiting_jobs_to_the_sink() {
    let held: Vec<WaitingJob> = SINK_WAIT.with(|q| {
        let mut q = q.borrow_mut();
        q.until = None;
        q.jobs.drain(..).collect()
    });
    if !held.is_empty() {
        log::info!("handing {} waiting job(s) to the new sink", held.len());
    }
    for (name, request, deliver) in held {
        dispatch(name, request, deliver);
    }
}

/// A [`JobSink`] installed for the length of a test, retired when this drops.
///
/// [`WORKER`] is a thread-local and the test harness reuses its threads, so a
/// port left installed is a port the *next* test on that thread inherits — and
/// that test posts its renders into a recorder nobody reads instead of running
/// them, then fails for reasons that have nothing to do with it.
///
/// **Retiring the port on the test's last line does not cover the case that
/// matters.** A failed assertion unwinds straight past that line, so the first
/// failure in a module quietly contaminates every test that runs after it on
/// the same thread and buries the real fault under wreckage it caused. `Drop`
/// runs on the unwind too, which is the whole reason this is a guard and not a
/// function called at the end.
#[cfg(test)]
pub struct InstalledTestWorker;

#[cfg(test)]
impl Drop for InstalledTestWorker {
    fn drop(&mut self) {
        abandon_worker("test teardown");
    }
}

/// Route [`offload_job`] through `port` until the returned guard drops.
///
/// The test-only counterpart of [`set_worker`] — see [`InstalledTestWorker`]
/// for why the retirement is a guard rather than a call at the end of the test.
#[cfg(test)]
pub fn install_test_worker(port: Box<dyn JobSink>) -> InstalledTestWorker {
    set_worker(port);
    InstalledTestWorker
}

/// Run `request` away from the frame that requested it, and hand the result to
/// `deliver`.
///
/// `deliver` runs where the result can be used: on the spawned thread natively,
/// and on the main thread in the browser. It is the whole tail of the old
/// closure — the `RenderGuard`, the cancellation check, the channel send and
/// the redraw — so the cancellation semantics are not reimplemented here, they
/// are carried inside it.
///
/// That is also what keeps `PaneRenderState::want_result`'s pruning honest. It
/// treats `Arc::strong_count(flag) > 1` as "still stoppable", and the second
/// reference used to be the one the offloaded closure held. It is now the one
/// `deliver` holds, kept alive by the pending map for exactly as long as the
/// job is outstanding, and released inside `deliver` at the cancellation check
/// — which is the last moment at which clearing the flag would change anything.
pub fn offload_job(name: &'static str, job: Job, deliver: impl FnOnce(JobResult) + Send + 'static) {
    let request = match job {
        Job::Described(request) => request,
        // Nothing to post. This is the same `offload` the opaque callers use
        // directly, reached through the funnel rather than around it.
        Job::Opaque(run) => return offload(name, move || deliver(run())),
    };
    // Boxed here rather than at the one place that stores it, because every arm
    // below either files it or runs it and the two must be the same closure.
    dispatch(name, request, Box::new(deliver));
}

/// [`offload_job`]'s described half, over an already-boxed delivery.
///
/// Split out for one caller: [`hand_waiting_jobs_to_the_sink`] holds boxed
/// deliveries and has to put them back through the same lifecycle — the same
/// registry, the same id space, the same fallthrough — rather than reach for
/// the sink itself. A generic `offload_job` cannot take one back.
fn dispatch(name: &'static str, request: JobRequest, deliver: Box<dyn FnOnce(JobResult) + Send>) {
    let kind = request.kind();
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Try the sink on every target: one implementation of the lifecycle, and
    // whichever transport this target installed underneath it.
    //
    // The request goes in **by value**, so nothing is serialised here on behalf
    // of a transport that would rather move it; `JobSink` says what that buys.
    // `Err` hands the job straight back, which is what lets the fallthrough
    // below exist without the funnel keeping a copy against a refusal it almost
    // never sees.
    let handoff = WORKER.with(|w| {
        let borrowed = w.borrow();
        let Some((sink_id, sink)) = borrowed.as_ref() else {
            return Handoff::NoSink(request, deliver);
        };
        // Registered **before** the handover, not after it. A transport that
        // executes in another thread of this same process can finish and answer
        // before `send` has returned, and a reply that arrives before its entry
        // does is a reply with no job to pair it to — the job would be dropped
        // silently, holding its render slot forever. The browser's transport
        // cannot lose that race and the native pool can, so the order is the
        // one that is right for both.
        pending().insert(
            id,
            Pending {
                kind,
                started: web_time::Instant::now(),
                sink: *sink_id,
                deliver,
            },
        );
        match sink.send(id, request) {
            Ok(()) => Handoff::Taken,
            Err(back) => Handoff::Refused(back),
        }
    });

    match handoff {
        Handoff::Taken => {}
        // Held for the sink this thread is expecting, if it is expecting one;
        // run here if it is not, which is what this arm has always done and
        // what it still does on every target that installs a sink up front.
        // See [`expect_sink`].
        Handoff::NoSink(request, deliver) => {
            if let Some((request, deliver)) = wait_for_sink(name, request, deliver) {
                run_here(name, request, deliver);
            }
        }
        Handoff::Refused(request) => {
            // The sink exists but would not take the job. Falling through runs
            // it here, which is slow but correct; a sink that keeps refusing is
            // a worker that has died, and `abandon_worker` is what retires it.
            log::warn!("{name}: the sink refused the job; running it here");
            // `None` if the sink was abandoned between the insert and the
            // refusal, in which case `abandon_worker` has already failed this
            // job and running it again would answer one render twice.
            if let Some(job) = pending().remove(&id) {
                run_here(name, request, job.deliver);
            }
        }
    }
}

/// What [`offload_job`] learned by offering the job to this thread's sink.
///
/// A three-way answer rather than an `Option`, because "there is no sink" and
/// "the sink would not take it" run the job in the same place for entirely
/// different reasons: the first is the normal state of a build with no
/// transport installed, and the second is a transport in trouble that has to be
/// logged and whose registry entry has to be reclaimed.
enum Handoff {
    /// The sink took it; the reply will come through [`deliver_job_reply`].
    Taken,
    /// No sink is installed on this thread. Carries the job back untouched, so
    /// it can be held for one this thread is expecting ([`expect_sink`]) or run
    /// here if it is expecting none.
    NoSink(JobRequest, Box<dyn FnOnce(JobResult) + Send>),
    /// A sink is installed and refused. The `deliver` is in the registry under
    /// this job's id.
    Refused(JobRequest),
}

/// Hand a sink's answer to the job that asked for it.
///
/// The one place a job leaves the registry with a result, whichever transport
/// produced it: `rustdar-web` calls this from the worker's `onmessage`, and the
/// native pool calls it from the pool thread that ran the job.
///
/// # It runs `deliver` on the calling thread, deliberately
///
/// Which thread that is, is the transport's business and not this function's. A
/// browser has exactly one choice — a Web Worker cannot touch the page's
/// memory, so the reply crosses a message port and `deliver` runs on the main
/// thread. A native pool has two, and it takes the one that keeps a measured
/// cost off the frame.
///
/// `deliver` builds the `egui::ColorImage` (`render_dispatch::plan_view_image`),
/// and that is still a copy after the premultiply moved to the producer: 206.75
/// MiB at the 7362 px desktop ceiling, **13.85 ms** on this box, against a 16.7
/// ms frame. Delivering on the frame thread would spend 83% of a frame budget
/// per still render to make the two arms agree about a thread nobody can
/// observe — `offload`'s own note explains why: every `deliver` ends in a
/// channel send drained on a later frame, so a send before the caller returns
/// and one after it are indistinguishable to the receiver and to the render
/// budget. Running it here is running it on the pool thread, which is where it
/// ran when this was a thread per job.
///
/// An `id` with no pending entry is ignored: it is a reply to a job that
/// [`abandon_worker`] already failed, and delivering it twice would send two
/// responses for one render.
///
/// The entry is removed **before** `deliver` runs, so the registry lock is not
/// held across anything a caller wrote and a job that dispatches another job
/// from inside its own delivery is not a deadlock.
pub fn deliver_job_reply(id: u64, result: JobResult) {
    let Some(job) = pending().remove(&id) else {
        log::debug!("reply {id} has no pending job; already abandoned");
        return;
    };
    // The counterpart of `offload`'s wasm log line: the same measurement, for
    // the arms where the time is *not* spent on the frame's thread.
    log::info!(
        "{} took {} ms off the frame",
        job.kind,
        job.started.elapsed().as_millis()
    );
    (job.deliver)(result);
}

/// How many jobs this thread's sink owes an answer for. For diagnostics and
/// tests.
///
/// Scoped to the sink rather than counting the whole registry, for
/// [`Pending::sink`]'s reason: the registry is process-wide, and a count of
/// every job everywhere would answer a question nobody asked — and would make
/// any assertion on it depend on what the test running beside it happened to be
/// doing.
pub fn jobs_in_worker() -> usize {
    let Some(sink) = WORKER.with(|w| w.borrow().as_ref().map(|(id, _)| *id)) else {
        return 0;
    };
    pending().values().filter(|job| job.sink == sink).count()
}

/// The native transport. See the module's own doc for why it is a pool.
#[cfg(not(target_arch = "wasm32"))]
mod pool;

#[cfg(test)]
mod tests;

/// The premultiply runs at the producer, and no pixel moved when it got there.
#[cfg(test)]
mod premultiply_tests;

/// The deferred-drop queue frees at the drain's pace, never at the push, and a
/// native discard never comes near it.
#[cfg(test)]
mod discard_tests;

/// A job dispatched while a sink is on its way waits for it, within a bound and
/// within a deadline — and a native thread never waits at all.
#[cfg(test)]
mod sink_wait_tests;
