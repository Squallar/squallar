//! Where a long-running, CPU-bound job runs.
//!
//! Four places in this crate used to hand a closure somewhere it would not
//! stall the frame that created it: the static radar render, the loop-frame
//! render, the overlay rasterization and the radar-sites rasterization. All
//! of it is **described jobs** now — [`JobRequest`] values a worker can be
//! handed, the overlay rasterizations included (the overlay codec rows
//! carry the sites raster, the three polygon kinds, the two hit-map kinds
//! and the model grid). The opaque funnel those closures rode — an
//! `offload(name, FnOnce)` whose wasm arm ran the closure **inline on the
//! browser's one thread** — is deleted with its last rider: there is no
//! function left in this module that unconditionally runs work on the wasm
//! frame, which is what makes "an overlay cannot land inline" a property of
//! the API rather than of every dispatch site staying careful.
//!
//! # One shape, one funnel
//!
//! [`offload_job`] takes a [`Job`]. Its described form is a [`JobRequest`]:
//! given a worker it posts; without one it runs [`execute`] on the spot —
//! unless this thread has said it is [`expect_sink`]ing one shortly, in which
//! case the job waits for it rather than paying a browser's whole frame for a
//! transport that is on its way. That fallback is the **only** inline
//! execution left on wasm, it is shared by every job kind alike, and it runs
//! exactly when the alternative is a pane wedged forever behind a worker that
//! is lost. The fallback is not a second code path: it calls the same
//! [`execute`] and the same `deliver` the worker reply does, so there is no
//! pair to drift.
//!
//! [`Job::Nothing`] is the description of a render with nothing behind it —
//! `deliver(None)` run where it stands, because a result known before any
//! work exists has no work to move. [`Job::Opaque`] survives only off-wasm
//! and only as a test instrument; its own doc says why it must never carry a
//! rasterization.
//!
//! [`discard`] is the same decision applied to teardown — a job whose whole
//! body is a `drop`. Natively it goes to a lane of the pool kept for exactly
//! this; on the web, which has no thread to hand it to, it queues instead and
//! [`drain_deferred_drops`] frees what a frame can afford, because a free
//! nobody is waiting on is the one job that never has to run now. What keeps
//! that queue draining is a term in the frame loop's own wake-up condition —
//! see [`drain_deferred_drops`], where the invariant is written.

// Source-type-free in BOTH directions since WO-M7c closed the reply
// direction (the request direction closed at WO-M7.2): every job kind's
// input, codec, run body and reply codec lives with its pipeline, reached
// through the composed registry (`crate::job_registry`) — this funnel names
// the substrate's erased vocabulary (`rustdar_source::job`) and no source
// crate's types, which `arch_ratchets`'
// `offload_names_zero_source_crate_types` pins at zero.
use std::cell::RefCell;
use std::collections::HashMap;

/// Free `payload` away from the frame that stopped needing it.
///
/// Teardown is CPU-bound work like any other job here, and "it runs rarely" is
/// not an exception the frame budget recognises: an evicted decoded volume is
/// 47–69 MiB across thousands of per-radial buffers, its drop is an allocator
/// walk over every one of them, and the caller handing it over is the frame
/// thread on every target.
///
/// Native hands the payload to the pool's **free lane** — a third queue, one
/// thread wide, and deliberately not the interactive lane that carries the
/// overlay rasterizations a pan is waiting on. See [`pool`]'s own doc. The
/// web arm has no lane to hand it to — wasm has no threads, and routing a
/// free through the job funnel would spend the worker (or, without one, this
/// very frame) on the one kind of work nobody is waiting for. That arm files
/// the payload in a thread-local queue, and [`drain_deferred_drops`] retires
/// what a frame can afford.
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
/// It is an *input* to a render, never its output: what travels is the
/// smallest thing the renderer can be re-run from, because re-running it is
/// how the worker and this thread stay byte-identical without a second
/// implementation to keep in step.
///
/// # One envelope over one described job
///
/// This used to be an enum of every job kind, each arm restating its own
/// payload and its own slice of the envelope. The kinds live with their
/// pipelines now — the radar and overlays crates' `jobs` modules each
/// publish their rows as [`rustdar_source::job::JobCodec`]s, composed by
/// `crate::job_registry::job_codecs` — and what remains here is exactly the
/// pair every kind shares: the type-erased input and the run envelope. The
/// codec row that owns `job`'s input type (the private `row_for` lookup) is
/// what encodes, decodes and runs it; this crate's judgment about a
/// described job begins and ends at that lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRequest {
    /// What the kind's renderer reads, behind the one type-erased seam: the
    /// kind's own input struct (`RadarPlanJob`, `SitesInput` and their
    /// siblings), and the codec row that owns it — the private `row_for`
    /// lookup finds the row by the input's type; the row carries the
    /// encode/decode/run bodies.
    pub job: rustdar_source::job::DescribedJob,
    /// The run envelope — the one statement of the raster's size and ground.
    ///
    /// For an **overlay** job: texture width and height in physical texels
    /// from the pane's `OverlayTexturePlan` (never re-derived on the far
    /// side), and the ground the texture covers exactly as
    /// `OverlayTexturePlan::coverage` answered it at the dispatch site; its
    /// `side_ceiling_px` is 0 on every overlay dispatch and every decode —
    /// the texture's exact dimensions are the request's own, so there is no
    /// ceiling to spend.
    ///
    /// For the **radar** kinds the envelope carries only `side_ceiling_px` —
    /// the largest raster side the render may produce — and the rest is
    /// zeroed (the private `ceiling_only_geometry` spells the shape once).
    /// **Four bytes on the wire, and a
    /// size rather than a flag.** It used to be one `full_res` byte selecting
    /// between two constants, which could only ever answer "the long-range
    /// size" or "the base size" — and the long-range size was itself a
    /// literal, so a device offering eight times it per axis was told about
    /// none of that. What the renderer needs is one number, "how big a
    /// texture is this result allowed to become", and there are two callers
    /// who know one:
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
    /// sweep's either way, and the radar crate's `raster_side_px` spends
    /// this ceiling only as far as the sweep's own gates justify. The figure
    /// is resolved at the dispatch site rather than in the row because this
    /// type travels to a worker that has no device to ask. The section, voxel
    /// and decode kinds carry no ceiling — a section's raster is a constant
    /// of the view (`xsect`'s `SECTION_WIDTH`), a voxel grid's shape is
    /// already on the wire, and a decode draws nothing — so their dispatch
    /// sites say 0, which is the same effective value the envelope has always
    /// answered for them.
    pub geometry: rustdar_source::job::JobGeometry,
}

impl JobRequest {
    /// Describe `input` under `geometry` — the one construction every
    /// dispatch site uses, so the type erasure happens in exactly one place.
    pub fn describe(
        input: impl rustdar_source::job::JobInput,
        geometry: rustdar_source::job::JobGeometry,
    ) -> Self {
        Self {
            job: rustdar_source::job::DescribedJob::new(input),
            geometry,
        }
    }
}

/// The envelope of a job with no raster geometry of its own — the radar
/// kinds, whose rows read only `side_ceiling_px` off it. Width, height and
/// bounds are zeroed: nothing on those rows reads them, the canonical
/// envelope carries the zeroes across the wire verbatim, and so a round trip
/// is the identity.
pub(crate) fn ceiling_only_geometry(side_ceiling_px: u32) -> rustdar_source::job::JobGeometry {
    rustdar_source::job::JobGeometry {
        width: 0,
        height: 0,
        bounds: rustdar_source::geo::GeoBounds {
            min_lat: 0.0,
            max_lat: 0.0,
            min_lon: 0.0,
            max_lon: 0.0,
        },
        side_ceiling_px,
    }
}

/// The answer a job's `deliver` receives: the row's typed output behind the
/// erasure seam, or `None` where the job produced nothing — a scan with no
/// matching sweep, an archive that did not decode, bytes another build
/// wrote. Callers treat `None` as the failure the renderer already meant by
/// it, and read the typed output back with
/// [`DescribedOut::take`](rustdar_source::job::DescribedOut::take) — the
/// wrong kind answers `None` there too, which is the same "nothing to draw"
/// every path already handles, with the budget still unwound and the pane
/// still told.
///
/// This dissolved the `JobOutput` enum (WO-M7c): the reply vocabulary is the
/// registry's own erased seam now, so this module names no output kind — a
/// frame, a section, a grid, a volume and an overlay raster are all the
/// row's business, in both directions.
pub type JobResult = Option<rustdar_source::job::DescribedOut>;

/// A rasterizing job. Every arm reaches [`offload_job`], which is the point:
/// there is one place that decides where work runs, and adding a job kind
/// does not add a dispatch site.
pub enum Job {
    /// Portable. Goes to the worker when one is attached, and runs through
    /// [`execute`] when none is. Every rasterizing dispatch is one of these.
    Described(JobRequest),
    /// A job whose answer is known to be "nothing to draw" before anything
    /// runs — a render asked of data that is not there. There is no work to
    /// move anywhere, so [`offload_job`] runs its `deliver(None)` where it
    /// stands; it is deliberately still a *job*, because the caller has
    /// already taken a slot in the render budget and marked its pane in
    /// flight, and those are unwound by `deliver` running, not by returning
    /// early. (Every `deliver` ends in a channel send drained on a later
    /// frame, so a send before the dispatch returns is indistinguishable
    /// from one after it.)
    Nothing,
    /// An arbitrary closure — **a test instrument, not a transport**, and
    /// deliberately absent from the wasm build: this variant does not exist
    /// there, so no dispatch compiled for the browser can route work through
    /// an opaque closure at all, which is the compile-level half of "the
    /// inline overlay path cannot come back". Production constructs none on
    /// any target (`Job::renders_nothing()` used to be one and is
    /// [`Job::Nothing`] now); what remains are `render_dispatch`'s
    /// invalidation tests, which need a render that *blocks* until released
    /// — a property no described job can have, since [`execute`] has no
    /// yield point. Natively it runs on a thread of its own rather than a
    /// pool lane, so a gated test job can never starve the lane that carries
    /// real overlay work.
    #[cfg(not(target_arch = "wasm32"))]
    Opaque(Box<dyn FnOnce() -> JobResult + Send>),
}

impl Job {
    /// A job whose answer is "nothing to draw". See [`Job::Nothing`].
    pub fn renders_nothing() -> Self {
        Self::Nothing
    }
}

impl JobRequest {
    /// Encode for a worker. **One shape for every row** (WO-M7b): the row's
    /// dense wire code, the canonical envelope, and then the row's own bytes
    /// — so one kind cannot be mistaken for another and no row spells the
    /// envelope for itself.
    ///
    /// The framing, byte for byte:
    /// `[code u8 = composed-index + 1][width u32][height u32][min_lat f64]`
    /// `[max_lat f64][min_lon f64][max_lon f64][side_ceiling_px u32][row bytes]`.
    /// The envelope is [`JobRequest::geometry`] in its declaration order,
    /// spelled here and in the decoder and nowhere else; a row whose kind
    /// reads none of a field still carries the field, zeroed — a decode job
    /// hauls 44 zero envelope bytes ahead of a ~16 MB archive, which is
    /// nothing, and variant framing would put a second shape on a wire whose
    /// whole point is having one.
    ///
    /// The geometry ALSO rides in the [`EncodeCtx`](rustdar_source::job::EncodeCtx)
    /// because one input — the model grid — is *cut to it* on the way out, at
    /// the one moment that knows what ground the texture covers; every other
    /// row ignores it and writes no envelope bytes of its own.
    pub fn to_bytes(&self) -> Vec<u8> {
        let row = row_for(&self.job);
        let ctx = rustdar_source::job::EncodeCtx {
            geometry: self.geometry,
        };
        let mut out = Vec::new();
        out.push(wire_code(row));
        out.extend_from_slice(&self.geometry.width.to_le_bytes());
        out.extend_from_slice(&self.geometry.height.to_le_bytes());
        out.extend_from_slice(&self.geometry.bounds.min_lat.to_le_bytes());
        out.extend_from_slice(&self.geometry.bounds.max_lat.to_le_bytes());
        out.extend_from_slice(&self.geometry.bounds.min_lon.to_le_bytes());
        out.extend_from_slice(&self.geometry.bounds.max_lon.to_le_bytes());
        out.extend_from_slice(&self.geometry.side_ceiling_px.to_le_bytes());
        (row.encode)(&self.job, &ctx, &mut out);
        out
    }

    /// `None` on an unallocated code or a payload this build cannot read —
    /// the two ends of a message port can be different builds, so that has to
    /// be a clean refusal rather than a misparse.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (code, rest) = bytes.split_first()?;
        // The code selects the row — `None` for a code this build does not
        // have, including 0, which a zeroed buffer is made of.
        let row = row_for_code(*code)?;
        let mut r = rustdar_source::wire::Reader::new(rest);
        // The canonical envelope, mirroring `to_bytes` field for field.
        let width = r.u32()?;
        let height = r.u32()?;
        let bounds = rustdar_source::geo::GeoBounds {
            min_lat: r.f64()?,
            max_lat: r.f64()?,
            min_lon: r.f64()?,
            max_lon: r.f64()?,
        };
        let side_ceiling_px = r.u32()?;
        if row.label.starts_with("overlay/") {
            // Refused at the boundary for the voxel affordability guard's
            // reason: these bytes arrive on a message port and the two
            // numbers are what [`execute`]'s output allocates a
            // `width × height` pixmap from, so without a ceiling a malformed
            // job is a multi-gigabyte allocation rather than a refusal. The
            // ceiling is the largest raster *any* target of this workspace
            // affords — the desktop plan-view side, squared — which every
            // real overlay plan sits under (a plan never exceeds the
            // adapter's texture limit, and its pixel count is the viewport
            // plus a quarter overdraw). A zero side is refused with it:
            // the rasterizer would answer a zero-length buffer whose
            // "success" no consumer could tell from a failure. The radar
            // kinds legitimately say 0 × 0 — nothing on those rows reads the
            // pair — so the guard is the overlay rows' own, judged by the
            // same label prefix the pool's lane routing already routes on.
            let ceiling = crate::constants::DESKTOP_RASTER_SIDE_CEILING as u64;
            let pixels = u64::from(width) * u64::from(height);
            if width == 0 || height == 0 || pixels > ceiling * ceiling {
                return None;
            }
        }
        let geometry = rustdar_source::job::JobGeometry {
            width,
            height,
            bounds,
            side_ceiling_px,
        };
        // The row decodes its own payload; the envelope passes through the
        // row unchanged on every kind — no row's wire form carries envelope
        // bytes of its own since WO-M7b.
        let (job, geometry) = (row.decode)(&mut r, geometry)?;
        if row.label.starts_with("overlay/") {
            // Every overlay input's lists are length-counted rather than
            // "the rest", so nothing may follow the payload: trailing bytes
            // mean the two builds' layouts disagree. The radar-family tails
            // ARE the rest — a `RenderInput` refuses trailing bytes itself,
            // and the opaque archives absorb them by design — so there is
            // nothing to check on those rows.
            r.rest().is_empty().then_some(())?;
        }
        Some(Self { job, geometry })
    }

    /// The codec row's label — the shipped kind string, for the native
    /// pool's lane loops and the tests. The dispatch resolves the row once
    /// and reads `row.label` itself (WO-M7c), and the browser has no pool,
    /// so on wasm32 this is dead — stated with an `expect` so a new wasm
    /// caller retires the attribute rather than silently voiding it.
    #[cfg_attr(target_arch = "wasm32", expect(dead_code))]
    fn kind(&self) -> &'static str {
        row_for(&self.job).label
    }
}

/// The composed codec registry this crate frames jobs against — the six
/// radar rows and then the seven overlay rows, each half in its own
/// load-bearing order. The composition itself lives in
/// [`crate::job_registry`], which is the one frontend module that names the
/// source crates' registries; this funnel consumes rows and never their
/// crates.
pub(crate) use crate::job_registry::job_codecs;

/// The dense wire code of `row`: its index in the composed registry, **plus
/// one** — codes `1..=13`, `radar` = 1 … `overlay/model` = 13.
///
/// Plus one, never the bare index: **0 stays unallocated so a zeroed buffer
/// never decodes** — a stale or corrupt message must be a refusal, not a
/// misparse into whatever kind index 0 happens to be. There is no number to
/// choose here anymore: the code IS the composition order, so "renumbering"
/// is recomposing the registry, which re-pins the literal table in
/// `offload::tests`, moves the framing rows, and thereby changes the build
/// token — the correct consequence: two builds that disagree refuse each
/// other at the handshake and respawn rather than misread a byte.
///
/// Panics on a row outside the composed registry: the caller resolved the
/// row with [`row_for`], which draws from the same registry — a miss is a
/// build defect, and a silently-wrong code byte would be a payload decoded
/// as another kind.
fn wire_code(row: &rustdar_source::job::JobCodec) -> u8 {
    let index = job_codecs()
        .position(|candidate| std::ptr::eq(candidate, row))
        .unwrap_or_else(|| {
            panic!(
                "the codec row {:?} is not in the composed registry",
                row.label
            )
        });
    u8::try_from(index + 1).expect("the composed registry outgrew the u8 code space")
}

/// The inverse of [`wire_code`]: the row a decoded code byte selects, or
/// `None` for a code this build does not have — 0 (a zeroed buffer), 14 and
/// beyond (a kind this composition does not carry). The two ends of a
/// message port can be different builds, so that has to be a clean refusal
/// rather than a misparse.
fn row_for_code(code: u8) -> Option<&'static rustdar_source::job::JobCodec> {
    let index = usize::from(code.checked_sub(1)?);
    job_codecs().nth(index)
}

/// The codec row that owns `job`'s input type — a scan of [`job_codecs`] by
/// `TypeId`, which is the one type-erased judgment this crate makes about a
/// described job; everything downstream of it (encode, decode, run, label)
/// is the row's.
///
/// Panics on an input type outside the registry, deliberately: every
/// constructor of a described job — the typed dispatch sites, the overlay
/// handlers' `prepare_job`, this file's own decode — draws from the
/// registry, so a miss is a build defect and a silent skip here would be a
/// job encoded as zero bytes or logged under no name (silent partial
/// success). The refusal-shaped path is [`row_for_code`], on the decode
/// side, where the bytes may genuinely be another build's.
fn row_for(job: &rustdar_source::job::DescribedJob) -> &'static rustdar_source::job::JobCodec {
    let input_type = job.0.as_any().type_id();
    job_codecs()
        .find(|row| (row.input_type)() == input_type)
        .unwrap_or_else(|| {
            panic!("no codec row owns the input type of {job:?}; the registry is incomplete")
        })
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

/// Do the work.
///
/// Pure, and the *only* implementation: the worker calls it, the native thread
/// calls it, and the inline fallback calls it. That is what makes a frame
/// rendered in a worker byte-identical to one rendered on this thread — the
/// two are not two renderers that agree, they are one renderer.
///
/// Every raster that leaves here is **premultiplied**, which is the one thing
/// this function does that the rasterizers underneath it do not — each
/// output type states its straight rasters
/// (`rustdar_source::job::JobOut::straight_rasters_mut`, required so a new
/// kind cannot silently decline to answer), and [`premultiply_raster`] says
/// why the conversion is a call to egui's own constructor rather than
/// arithmetic written out again. It runs here rather than at the consumer
/// because here is off the browser's main thread and off the frame thread
/// on both targets: the per-pixel walk is 4.2–4.6 ms at the 2048 px browser
/// ceiling against a 16.7 ms frame budget, and it used to be spent on the
/// browser's main thread.
///
/// "Pure" is a claim about what it *returns*, and it survives four pieces of
/// process-wide state underneath, all of them buffer pools and all admissible
/// for the same one reason:
///
/// * the plan-view rasterizer carries its cell buffer between calls
///   (the radar renderer's `POOLED_CELLS`);
/// * the section rasterizer carries its three planes between cuts
///   (the section renderer's `POOLED_PLANES`), which the section row
///   reaches through `render_section`; and
/// * the plan-view rasterizer carries the RGBA texture and the value grid it
///   answers with (the radar renderer's `POOLED_IMAGE` and
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
    // The row that owns the input's type runs it — the same moved renderer
    // bodies the per-kind match here used to call, now one registry lookup.
    // The rows read the run envelope themselves (the radar raster rows read
    // `side_ceiling_px` off it once, so no two of them can come to disagree
    // about how large a picture a job was allowed to make); the GLM row still
    // ages every flash against the `now` its input carries (the page's clock
    // at dispatch, the parity gates' whole subject), and the model row draws
    // whichever carry arrived — those facts live beside the rows in their
    // source crates now.
    let row = row_for(&request.job);
    let mut out = (row.run)(&request.job, &request.geometry)?;
    // The output stage, after every kind, so no rasterizing row can be added
    // that forgets it: each output type states its own premultiply posture
    // (`JobOut::straight_rasters_mut` is required, no default), and whatever
    // it hands over is converted in place, here, with egui's own arithmetic.
    // What the old exhaustive match guaranteed by listing five kinds, the
    // trait now guarantees structurally for the sixth that does not exist
    // yet.
    for raster in out.0.straight_rasters_mut() {
        premultiply_raster(raster);
    }
    Some(out)
}

/// [`execute`] straight off the wire, for a worker that holds bytes rather than
/// a `JobRequest`. `None` for a payload it cannot read, which the caller
/// reports back as a failed job rather than dropping silently.
pub fn execute_bytes(bytes: &[u8]) -> JobResult {
    execute(&JobRequest::from_bytes(bytes)?)
}

/// [`execute_bytes`] plus the reply's own wire form: the row's registry
/// code, its `encode_out` HEAD, and the row's nominated TAILS — exactly
/// what the worker posts back as the `OUT`/`OUT_KIND`/`TAILS` trio
/// (WO-M7c; head/tails split at WO-M7d; `rustdar_web::worker` is the one
/// production caller).
///
/// Here rather than in `rustdar-web` for the reason [`execute_bytes`] is
/// here: the browser crate is the adapter, this crate owns what a job means,
/// and an encode that lived over there would be reachable only from a
/// browser. The code is the row's dense registry code — the SAME code space
/// the request direction speaks (`wire_code`, composed-index plus one) —
/// so one table names every kind in both directions and a reply cannot be
/// tagged with a vocabulary the requests do not have.
///
/// `None` for a payload this build cannot read and for a job that produced
/// nothing — the page cannot tell them apart and does not need to: both
/// mean "nothing to draw", and the caller still posts the explicit-null
/// reply that keeps the pane from wedging.
pub fn execute_encoded(bytes: &[u8]) -> Option<(u8, Vec<u8>, Vec<Vec<u8>>)> {
    let request = JobRequest::from_bytes(bytes)?;
    let row = row_for(&request.job);
    let out = execute(&request)?;
    // The head is scalars and framing — 64 covers every current row's
    // fixed prefix, and an encoder that writes a big head (the delegating
    // section/voxel/decode rows, the overlay raster) reserves its own
    // exact need on top. A row's LARGE FLAT buffers do not land in the
    // head at all: they ride `tails`, each transferred to the page as its
    // own buffer (WO-M7d).
    //
    // The one-buffer shape this replaces cost this worker, per widest
    // 2048² still frame (image 16.00 MiB + polar-with-values 5.04 MiB,
    // derived by code reading), 5 memcpys / 68.16 MiB: polar `to_bytes`
    // 5.04 + the head concat of polar 5.04 and image 16.00 + an UNCLAIMED
    // whole-reply double-buffer 21.04 (this sink had no capacity and the
    // frame codec extended it with a second finished Vec — the comment
    // that stood here claimed "one extra memcpy, ~20 MiB" and
    // under-claimed) + the wasm→JS crossing 21.04. The tails shape costs
    // polar `to_bytes` 5.04 + the per-buffer wasm→JS crossings
    // 5.04 + 16.00 = 26.08 MiB / 3 memcpys — parity with the pre-M7c
    // wire. PX3 is the runtime measurement that judges these figures.
    let mut head = Vec::with_capacity(64);
    let mut tails = Vec::new();
    (row.encode_out)(out, &mut head, &mut tails);
    Some((wire_code(row), head, tails))
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
    /// The codec row the dispatched job resolved to, recorded at dispatch —
    /// **before** the send, with the entry (the register-before-send order
    /// below) — so an encoded reply is decoded through the row *this page*
    /// dispatched under, never through whatever the reply's own tag claims:
    /// [`deliver_encoded_reply`] verifies the reply's kind against this
    /// row's code and refuses a mismatch as "nothing to draw".
    row: &'static rustdar_source::job::JobCodec,
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
/// kinds here are not one size: a full queue of radar requests is ~1.3 MB
/// apiece, and a full queue of archive decodes is 16.9 MB apiece — 541
/// MB, on a target whose address space is 4 GiB. A byte budget would be the
/// honest bound and it is not available: a `JobRequest`'s cost is its payload's
/// length on two kinds, a `RenderInput`'s owned gates on three more, and
/// nothing here can price the last without walking it.
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
/// an existing in-code measurement, quoted from the `decode` row's
/// `DecodeJob` rather than re-taken here — is 1021.9 ms in Firefox
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

/// Run `request` without a transport — the funnel's last resort, and the
/// **only** inline execution wasm has left now that the opaque funnel is
/// deleted: a browser whose worker is lost (or refused the job) pays the
/// render on its one thread, because the alternative is a pane wedged behind
/// a reply that will never come. With a healthy worker this never runs, and
/// the respawn machinery exists to keep that the steady state.
///
/// Natively "here" is still not the calling thread: a thread is spawned for
/// the job — the calling thread is the frame's on every rasterizing path,
/// and the one reachable way in natively is a pool whose lane died, which a
/// lane-mate would not survive either. Only a failed spawn runs it truly
/// here, the same last-resort ladder the opaque test instrument descends.
///
/// One function because three arms reach it — no sink and nothing expected, a
/// sink that refused, and a wait that ran out — and the three must not come to
/// disagree about where "here" is.
fn run_here(name: &'static str, request: JobRequest, deliver: Box<dyn FnOnce(JobResult) + Send>) {
    #[cfg(target_arch = "wasm32")]
    {
        // Timed because this is the one arm where the cost lands on the
        // frame; the number is what says whether worker respawn is keeping up.
        let started = web_time::Instant::now();
        deliver(execute(&request));
        log::info!(
            "{name} took {} ms on the main thread",
            started.elapsed().as_millis()
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let task = std::sync::Arc::new(std::sync::Mutex::new(Some(move || {
            deliver(execute(&request))
        })));
        let on_thread = std::sync::Arc::clone(&task);
        let spawned = std::thread::Builder::new()
            .name(format!("rd-fallback-{name}"))
            .spawn(move || {
                if let Some(f) = on_thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    f()
                }
            });
        if let Err(e) = spawned {
            log::error!("{name}: no thread for the fallback ({e}); running it here");
            if let Some(f) = task.lock().unwrap_or_else(|e| e.into_inner()).take() {
                f()
            }
        }
    }
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
        // Nothing to run and nothing to post: the answer is already known,
        // and `deliver` is what unwinds the caller's marks. See
        // [`Job::Nothing`] for why running it here is indistinguishable from
        // any other placement.
        Job::Nothing => return deliver(None),
        // The test instrument. A thread of its own, not a pool lane — see
        // [`Job::Opaque`] — and inline only if the spawn itself fails, which
        // is the same last resort a refused described job takes. The task
        // sits behind an `Option` so the failed-spawn arm can still run it
        // here: `deliver` is what unwinds the caller's in-flight marks, and
        // a dropped closure would strand them (`spawn` consumes its closure,
        // so unlike the pool's `SendError` there is no value handed back).
        #[cfg(not(target_arch = "wasm32"))]
        Job::Opaque(run) => {
            let task = std::sync::Arc::new(std::sync::Mutex::new(Some(move || deliver(run()))));
            let on_thread = std::sync::Arc::clone(&task);
            let spawned = std::thread::Builder::new()
                .name(format!("rd-opaque-{name}"))
                .spawn(move || {
                    if let Some(f) = on_thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        f()
                    }
                });
            if let Err(e) = spawned {
                log::error!("{name}: no thread for an opaque job ({e}); running it here");
                // The thread never started, so the task is still here.
                if let Some(f) = task.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    f()
                }
            }
            return;
        }
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
    // Resolved once, ahead of the send: the label names the job in the log,
    // and the row itself rides the pending entry so the reply is decoded
    // through what THIS dispatch resolved (see `Pending::row`).
    let row = row_for(&request.job);
    let kind = row.label;
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
                row,
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

/// [`deliver_job_reply`] for a reply that arrived as bytes — the browser's
/// `OUT`/`OUT_KIND`/`TAILS` trio (head, row code, and the row's nominated
/// large buffers, WO-M7d), or `None` for a worker that answered explicit
/// nulls (a job that produced nothing).
///
/// The decode runs through **the row recorded at dispatch**
/// (`Pending::row`), never through a registry lookup on the reply's own
/// tag: the tag is *verified* against that row's code and a mismatch is
/// logged and delivered as `None` — a reply of the wrong kind is another
/// build's or a corrupt message, and "nothing to draw" is what every
/// consumer already does with one, with the slot still released. The bytes
/// the row's own codec refuses land on the same answer through
/// `decode_out`'s failure channel.
///
/// The row is read without removing the entry, and the removal, the timing
/// log and the delivery stay [`deliver_job_reply`]'s — one path out of the
/// registry, whichever form the reply took. On the one thread a browser has
/// nothing can retire the entry between the read and the call; anywhere
/// else the entry-less case is the already-abandoned job both functions
/// already ignore.
pub fn deliver_encoded_reply(id: u64, reply: Option<(u8, Vec<u8>, Vec<Vec<u8>>)>) {
    let row = pending().get(&id).map(|job| job.row);
    let result = match (row, reply) {
        (Some(row), Some((kind, head, tails))) => {
            if kind == wire_code(row) {
                (row.decode_out)(&head, tails)
            } else {
                log::error!(
                    "a worker answered job {id} with out-kind {kind} where the \
                     dispatched `{}` row's code is {}; treating it as a failed \
                     job",
                    row.label,
                    wire_code(row),
                );
                None
            }
        }
        _ => None,
    };
    deliver_job_reply(id, result);
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
