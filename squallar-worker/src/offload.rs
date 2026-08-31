//! Where a long-running, CPU-bound job runs.
//!
//! [`offload_job`] takes a [`Job`], whose described form is a [`JobRequest`]:
//! given a worker it posts, and without one it runs [`execute`] on the spot —
//! unless this thread is [`expect_sink`]ing one shortly, in which case the job
//! waits. That fallback is the only inline execution left on wasm, and it calls
//! the same [`execute`] and `deliver` the worker reply does.

use std::cell::RefCell;
use std::collections::HashMap;

/// Free `payload` away from the frame that stopped needing it.
///
/// An evicted decoded volume is 47–69 MiB across thousands of per-radial
/// buffers. Native hands it to the pool's free lane (one thread wide, not the
/// interactive lane); wasm files it in a thread-local queue that
/// [`drain_deferred_drops`] retires.
///
/// **Call it from the frame thread** — the deferred queue is thread-local, so a
/// payload filed from a tokio worker is filed where no drain will look. Hand
/// over the LAST reference, a POINTER rather than a large value, and MEMORY
/// rather than a resource whose teardown matters (the pool's lanes are detached
/// and never dropped, so a `Drop` may never run). [`discard_each`] for a
/// collection — a batch handed over whole is one payload freed in one turn.
pub fn discard(name: &'static str, payload: impl Send + 'static) {
    let payload: Box<dyn std::any::Any + Send> = Box::new(payload);
    #[cfg(not(target_arch = "wasm32"))]
    // `Err` is a free lane with no live worker; this is the frame thread, which
    // is where a multi-GiB teardown must not land.
    if let Err(payload) = pool::run_free(name, payload) {
        log::warn!("{name}: the free lane has no worker left; deferring the drop instead");
        defer_drop(name, payload);
    }
    #[cfg(target_arch = "wasm32")]
    defer_drop(name, payload);
}

/// [`discard`] each item of `payloads` separately.
///
/// `discard(name, map.remove(site))` type-checks and hands over one payload
/// holding every volume in it, which the drain then frees in one turn on one
/// frame. Every obligation on [`discard`] applies to every item.
pub fn discard_each<T: Send + 'static>(name: &'static str, payloads: impl IntoIterator<Item = T>) {
    for payload in payloads {
        discard(name, payload);
    }
}

/// A payload awaiting its frame-paced free, and the name it was discarded under.
type DeferredDrop = (&'static str, Box<dyn std::any::Any + Send>);

thread_local! {
    /// What [`discard`] is holding until a frame can afford to free it.
    /// Thread-local: every producer of an entry is the thread that consumes it.
    /// A `VecDeque`, so the longest-waiting entry goes next.
    static DEFERRED_DROPS: RefCell<std::collections::VecDeque<DeferredDrop>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
}

/// File `payload` for [`drain_deferred_drops`] to retire. Reached on wasm for
/// every discard and natively only for one the free lane could not take; the
/// queue exists on both targets so a host test can drive it.
pub fn defer_drop(name: &'static str, payload: Box<dyn std::any::Any + Send>) {
    DEFERRED_DROPS.with(|q| q.borrow_mut().push_back((name, payload)));
}

/// Whether this thread is still holding anything it has promised to free.
pub fn has_deferred_drops() -> bool {
    DEFERRED_DROPS.with(|q| !q.borrow().is_empty())
}

/// Free deferred payloads until `budget` is spent, and answer how many went.
///
/// **A non-empty queue must keep the frame loop awake** — a contract on the
/// caller, since the app rests on `ControlFlow::Wait`. `App::handle_redraw`
/// names [`has_deferred_drops`] among the terms that request the next frame; a
/// minimized or zero-area window, a missing renderer and a backgrounded tab all
/// return before that re-arm.
///
/// A time budget rather than a count, since entries differ by orders of
/// magnitude. **At least one payload goes per call**: the elapsed check is made
/// after a free, so this paces rather than bounds.
pub fn drain_deferred_drops(budget: std::time::Duration) -> usize {
    // An empty queue must not pay wasm's `performance.now()` crossing.
    if !has_deferred_drops() {
        return 0;
    }
    let started = web_time::Instant::now();
    let mut freed = 0;
    // Native-only: wasm's clock is clamped to ~100 µs under Firefox's default
    // `privacy.reduceTimerPrecision`, so the "dearest" would be a tie-break.
    #[cfg(not(target_arch = "wasm32"))]
    let mut dearest: (&'static str, u128) = ("nothing", 0);
    // One at a time, with the borrow released before the payload is dropped: a
    // `Drop` that discards something of its own must find the queue borrowable.
    while let Some((name, payload)) = DEFERRED_DROPS.with(|q| q.borrow_mut().pop_front()) {
        #[cfg(not(target_arch = "wasm32"))]
        let before = started.elapsed();
        drop(payload);
        freed += 1;
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
    // Microseconds, and once per drain: a free is routinely sub-millisecond.
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
/// It is an *input* to a render, never its output: re-running the renderer is
/// how the worker and this thread stay byte-identical without a second
/// implementation. The codec row that owns `job`'s input type encodes, decodes
/// and runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRequest {
    /// The kind's own input struct, found by `row_for` from the input's type.
    pub job: squallar_source::job::DescribedJob,
    /// The run envelope — the one statement of the raster's size and ground.
    ///
    /// For an **overlay** job: texture width and height in physical texels and
    /// the ground the texture covers, both as the pane's `OverlayTexturePlan`
    /// answered them at the dispatch site; `side_ceiling_px` is 0 there and on
    /// every decode.
    ///
    /// For the **radar** kinds only `side_ceiling_px` — the largest raster side
    /// the render may produce — with the rest zeroed (`ceiling_only_geometry`).
    /// A loop frame says [`squallar_device_profile::constants::LOOP_IMAGE_SIZE`];
    /// a static render says `raster_side_ceiling_px` of this adapter's
    /// `max_texture_dimension_2d`. Section, voxel and decode kinds say 0.
    pub geometry: squallar_source::job::JobGeometry,
}

impl JobRequest {
    /// Describe `input` under `geometry` — the one construction every dispatch
    /// site uses.
    pub fn describe(
        input: impl squallar_source::job::JobInput,
        geometry: squallar_source::job::JobGeometry,
    ) -> Self {
        Self {
            job: squallar_source::job::DescribedJob::new(input),
            geometry,
        }
    }
}

/// The envelope of a job with no raster geometry of its own — the radar kinds.
/// Width, height and bounds are zeroed and cross the wire verbatim.
pub fn ceiling_only_geometry(side_ceiling_px: u32) -> squallar_source::job::JobGeometry {
    squallar_source::job::JobGeometry {
        width: 0,
        height: 0,
        bounds: squallar_geo::GeoBounds {
            min_lat: 0.0,
            max_lat: 0.0,
            min_lon: 0.0,
            max_lon: 0.0,
        },
        side_ceiling_px,
    }
}

/// The answer a job's `deliver` receives: the row's typed output behind the
/// erasure seam, or `None` where the job produced nothing. Read the typed output
/// back with [`DescribedOut::take`](squallar_source::job::DescribedOut::take);
/// the wrong kind answers `None` there too.
pub type JobResult = Option<squallar_source::job::DescribedOut>;

/// A rasterizing job. Every arm reaches [`offload_job`], so adding a job kind
/// does not add a dispatch site.
pub enum Job {
    /// Portable. Goes to the worker when one is attached, [`execute`] when not.
    Described(JobRequest),
    /// A job whose answer is known to be "nothing to draw" before anything runs.
    /// Still a *job*: the caller has taken a render-budget slot and marked its
    /// pane in flight, and those are unwound by `deliver` running.
    Nothing,
    /// An arbitrary closure — **a test instrument, not a transport**, and absent
    /// from the wasm build. `render_dispatch`'s invalidation tests need a render
    /// that *blocks* until released, which no described job can be since
    /// [`execute`] has no yield point. Natively it runs on a thread of its own.
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
    /// Encode for a worker. One shape for every row:
    /// `[code u8 = composed-index + 1][width u32][height u32][min_lat f64]`
    /// `[max_lat f64][min_lon f64][max_lon f64][side_ceiling_px u32][row bytes]`.
    /// The envelope is [`JobRequest::geometry`] in declaration order, spelled
    /// here and in the decoder and nowhere else; a row that reads none of a
    /// field still carries it, zeroed. The geometry also rides in the
    /// [`EncodeCtx`](squallar_source::job::EncodeCtx) because one input — the
    /// model grid — is cut to it on the way out.
    pub fn to_bytes(&self) -> Vec<u8> {
        let row = row_for(&self.job);
        let ctx = squallar_source::job::EncodeCtx {
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

    /// `None` on an unallocated code or a payload this build cannot read — the
    /// two ends of a message port can be different builds.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (code, rest) = bytes.split_first()?;
        let row = row_for_code(*code)?;
        let mut r = squallar_source::wire::Reader::new(rest);
        // The canonical envelope, mirroring `to_bytes` field for field.
        let width = r.u32()?;
        let height = r.u32()?;
        let bounds = squallar_geo::GeoBounds {
            min_lat: r.f64()?,
            max_lat: r.f64()?,
            min_lon: r.f64()?,
            max_lon: r.f64()?,
        };
        let side_ceiling_px = r.u32()?;
        if row.label.starts_with("overlay/") {
            // These bytes arrive on a message port, and the pair is what
            // [`execute`]'s output allocates a `width × height` pixmap from: no
            // ceiling would make a malformed job a multi-gigabyte allocation.
            // The ceiling is the largest raster any target affords (the desktop
            // plan-view side, squared). Zero sides are refused with it.
            let ceiling = squallar_device_profile::constants::DESKTOP_RASTER_SIDE_CEILING as u64;
            let pixels = u64::from(width) * u64::from(height);
            if width == 0 || height == 0 || pixels > ceiling * ceiling {
                return None;
            }
        }
        let geometry = squallar_source::job::JobGeometry {
            width,
            height,
            bounds,
            side_ceiling_px,
        };
        // The row decodes its own payload; the envelope passes through unchanged.
        let (job, geometry) = (row.decode)(&mut r, geometry)?;
        if row.label.starts_with("overlay/") {
            // Every overlay input's lists are length-counted, so trailing bytes
            // mean the two builds' layouts disagree. The radar-family tails ARE
            // the rest, so there is nothing to check on those rows.
            r.rest().is_empty().then_some(())?;
        }
        Some(Self { job, geometry })
    }

    /// The codec row's label — the shipped kind string, for the native pool's
    /// lane loops and the tests. Dead on wasm32, which has no pool.
    #[cfg_attr(target_arch = "wasm32", expect(dead_code))]
    fn kind(&self) -> &'static str {
        row_for(&self.job).label
    }
}

/// The composed codec registry this crate frames jobs against. The composition
/// lives in [`crate::job_registry`]; this funnel consumes rows, never crates.
pub(crate) use crate::job_registry::job_codecs;

/// The dense wire code of `row`: its index in the composed registry, **plus
/// one** — codes `1..=15`. Plus one so **0 stays unallocated and a zeroed buffer
/// never decodes**. Panics on a row outside the registry: the caller resolved it
/// with [`row_for`] from the same registry, so a miss is a build defect and a
/// wrong code byte would be a payload decoded as another kind.
fn wire_code(row: &squallar_source::job::JobCodec) -> u8 {
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

/// The inverse of [`wire_code`]: the row a decoded code byte selects, or `None`
/// for a code this build does not have — 0 (a zeroed buffer), 15 and beyond.
fn row_for_code(code: u8) -> Option<&'static squallar_source::job::JobCodec> {
    let index = usize::from(code.checked_sub(1)?);
    job_codecs().nth(index)
}

/// The codec row that owns `job`'s input type — a scan of [`job_codecs`] by
/// `TypeId`, the one type-erased judgment this crate makes about a described job.
/// Panics on an input type outside the registry, where a silent skip would be a
/// job encoded as zero bytes; the refusal-shaped path is [`row_for_code`], where
/// the bytes may genuinely be another build's.
fn row_for(job: &squallar_source::job::DescribedJob) -> &'static squallar_source::job::JobCodec {
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
/// It calls [`egui::Color32::from_rgba_unmultiplied`] rather than reimplementing
/// the arithmetic, which is not `channel * alpha / 255`: the α = 0 arm answers
/// `TRANSPARENT`, the α = 255 arm skips the multiply, and the arm between reads
/// a 64 KiB lookup table `ecolor` builds once. `premultiply_tests` covers all
/// 256 × 256 pairs. In place, because the buffer is a pooled texture or plane —
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
/// calls it, and the inline fallback calls it, which is what makes a frame
/// rendered in a worker byte-identical to one rendered here.
///
/// Every raster that leaves here is **premultiplied** — each output type states
/// its straight rasters (`JobOut::straight_rasters_mut`, required so a new kind
/// cannot silently decline). It runs here because here is off the frame thread
/// on both targets: 4.2–4.6 ms at the 2048 px browser ceiling against 16.7 ms.
///
/// "Pure" is a claim about what it *returns*, and it survives four buffer pools
/// underneath (`POOLED_CELLS`, `POOLED_PLANES`, `POOLED_IMAGE`,
/// `POOLED_VALUES`): none is handed out in any state but the one a fresh
/// allocation would be in. Anything added below that IS observable between
/// calls breaks worker equivalence.
pub fn execute(request: &JobRequest) -> JobResult {
    // The row that owns the input's type runs it, and reads the run envelope
    // itself, so no two raster rows can disagree about the size allowed.
    let row = row_for(&request.job);
    let mut out = (row.run)(&request.job, &request.geometry)?;
    // The output stage, after every kind, so no rasterizing row can be added
    // that forgets it (`JobOut::straight_rasters_mut` is required, no default).
    for raster in out.0.straight_rasters_mut() {
        premultiply_raster(raster);
    }
    Some(out)
}

/// [`execute`] straight off the wire, for a worker that holds bytes. `None` for
/// a payload it cannot read, which the caller reports back as a failed job.
pub fn execute_bytes(bytes: &[u8]) -> JobResult {
    execute(&JobRequest::from_bytes(bytes)?)
}

/// [`execute_bytes`] plus the reply's own wire form: the row's registry code, its
/// `encode_out` HEAD, and the row's nominated TAILS — the
/// `OUT`/`OUT_KIND`/`TAILS` trio the worker posts back (`squallar_web::worker` is
/// the one production caller). The code is the SAME code space the request
/// direction speaks (`wire_code`). `None` for a payload this build cannot read
/// and for a job that produced nothing — both mean "nothing to draw", and the
/// caller still posts the explicit-null reply that keeps the pane from wedging.
pub fn execute_encoded(bytes: &[u8]) -> Option<(u8, Vec<u8>, Vec<Vec<u8>>)> {
    let request = JobRequest::from_bytes(bytes)?;
    let row = row_for(&request.job);
    let out = execute(&request)?;
    // The head is scalars and framing — 64 covers every current row's fixed
    // prefix. A row's LARGE FLAT buffers ride `tails`, each transferred to the
    // page as its own buffer: per widest 2048² still frame that is 26.08 MiB
    // over 3 memcpys (derived by code reading) against the one-buffer shape's
    // 68.16 MiB over 5. PX3 is the runtime measurement that judges these.
    let mut head = Vec::with_capacity(64);
    let mut tails = Vec::new();
    (row.encode_out)(out, &mut head, &mut tails);
    Some((wire_code(row), head, tails))
}

// ── The job sink ─────────────────────────────────────────────────────────────

/// A place to send [`JobRequest`]s that is not this thread.
///
/// Implemented by `squallar-web` over a dedicated `Worker`, and installed rather
/// than constructed here because that crate depends on this one.
/// [`send`](Self::send) takes the request **by value**: both transports
/// implement handover, and a browser cannot hand over anything but a detachable
/// buffer, so `squallar-web`'s implementation serialises where one that can move
/// an owned value need not.
pub trait JobSink {
    /// Hand `request` over to be executed. `id` comes back with the reply so the
    /// funnel can pair them.
    ///
    /// `Err(request)` carries the job back so the caller can run it here instead
    /// of waiting for a reply that is not coming. It costs an implementation
    /// nothing: `JobRequest::to_bytes` **borrows**.
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest>;
}

/// The state a posted job needs when its reply lands.
struct Pending {
    kind: &'static str,
    started: web_time::Instant,
    /// Which installed sink owes this job an answer, so [`abandon_worker`] can
    /// fail **that sink's** jobs and no others out of the process-wide registry.
    sink: u64,
    /// The codec row the dispatched job resolved to, recorded at dispatch, so an
    /// encoded reply is decoded through the row *this page* dispatched under —
    /// [`deliver_encoded_reply`] verifies the reply's tag against its code.
    row: &'static squallar_source::job::JobCodec,
    /// Holds the `RenderGuard`, the pane's `Arc<AtomicBool>` and the response
    /// channel. Consuming it decrements the render budget and clears the pane's
    /// in-flight mark, so it must run on *every* path out of the pending map.
    deliver: Box<dyn FnOnce(JobResult) + Send>,
}

/// Every job any sink in this process owes an answer for.
///
/// Process-wide rather than thread-local: `offload_job` is called from the frame
/// thread and from tokio's workers, and the reply is produced on a pool thread
/// that is not the submitter's. Nothing user-supplied runs under the lock — a
/// `deliver` is always called after its entry has been removed.
static PENDING: std::sync::LazyLock<std::sync::Mutex<HashMap<u64, Pending>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// The registry, recovered from a poisoned lock rather than propagating the
/// panic: every operation under it is a single insert, remove or scan, and
/// refusing it would leave every pane wedged holding a render slot.
fn pending() -> std::sync::MutexGuard<'static, HashMap<u64, Pending>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether job `id` is still owed an answer — the claim [`pool::lane_job`]
/// makes at the last instant before running a queued job. Native-only because
/// the pool is; the browser transport has no pre-run seam on this side of the
/// port.
#[cfg(not(target_arch = "wasm32"))]
fn job_is_owed(id: u64) -> bool {
    pending().contains_key(&id)
}

/// Job ids, unique across the process because [`PENDING`] is.
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Sink ids. A fresh one per installed sink, so [`Pending::sink`] identifies the
/// *installation*: a port retired and replaced does not inherit its jobs.
static NEXT_SINK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

thread_local! {
    /// The sink this thread hands jobs to, and the id its jobs are filed under.
    /// Thread-local because the browser's implementation owns a
    /// `web_sys::Worker`, which is `!Send`; the registry above is shared.
    static WORKER: RefCell<Option<(u64, Box<dyn JobSink>)>> = RefCell::new(installed(default_sink()));
}

/// The transport a thread starts with, before anything is installed over it.
/// The module's one target fork, and it selects a transport rather than a
/// behaviour: natively the process's job pool, and in a browser nothing, since a
/// browser's `Worker` has to start and prove itself first.
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
/// Called from `squallar-web`'s `worker_port` every time a worker proves itself
/// with a build-token handshake. A port already installed here is **abandoned**,
/// not dropped: a job whose sink is replaced under it would sit in the registry
/// forever holding a render slot. Whatever [`expect_sink`] held is handed over.
pub fn set_worker(port: Box<dyn JobSink>) {
    abandon_worker("replaced by a new port");
    WORKER.with(|w| *w.borrow_mut() = installed(Some(port)));
    hand_waiting_jobs_to_the_sink();
}

/// Give up on the worker: it died, or answered the handshake with another build.
///
/// Every job **it** still owes is failed rather than forgotten — dropping them
/// would leak the render budget and leave panes marked in-flight forever. Scoped
/// to the retired sink's own jobs. **What [`expect_sink`] is holding is left
/// alone**; the wait's own deadline is what ends that.
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

/// How many jobs a thread will hold while it waits for the sink it is expecting,
/// before the oldest is given up on.
///
/// A count and not a byte budget — a full queue is ~1.3 MB apiece for radar
/// requests and 16.9 MB apiece for archive decodes (541 MB) on a target whose
/// address space is 4 GiB — and it is safe anyway because this queue only fills
/// while there is **no sink at all**, for the caller's own deadline.
pub const SINK_WAIT_LIMIT: usize = 32;

/// A job held for the sink its thread is expecting: name, request, delivery.
type WaitingJob = (&'static str, JobRequest, Box<dyn FnOnce(JobResult) + Send>);

/// A job that is **not** being held: request and delivery. The name is dropped
/// because [`run_here`]'s caller already has it.
type UnheldJob = (JobRequest, Box<dyn FnOnce(JobResult) + Send>);

/// What [`expect_sink`] armed, and what has arrived since.
struct SinkWait {
    /// When to stop waiting, or `None` for a thread expecting nothing — in
    /// which case a job that finds no sink runs where it stands.
    until: Option<web_time::Instant>,
    /// A `VecDeque`: hand-over and eviction both take from the front, so the
    /// longest-waiting job reaches the sink first and is the first given up on.
    jobs: std::collections::VecDeque<WaitingJob>,
}

thread_local! {
    /// This thread's wait. Thread-local because [`WORKER`] is; it exists on
    /// every target so a host test can drive the queue a browser will use.
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
/// A browser has no sink until its worker answers a build-token handshake, and
/// none again until a replacement does. In those windows [`offload_job`] falls
/// through to the browser's **one** thread: 1021.9 ms in Firefox and 911.4 ms
/// in Chrome for a 16.9 MB volume (the `decode` row's `DecodeJob` measurement).
///
/// **A thread with a sink installed is expecting nothing and this returns
/// without arming.** `within` is a window on one attempt, not on the whole
/// recovery: a job held for a minute is a pane blank for a minute.
pub fn expect_sink(within: std::time::Duration) {
    if worker_attached() {
        return;
    }
    SINK_WAIT.with(|q| q.borrow_mut().until = Some(web_time::Instant::now() + within));
}

/// Whether this thread is holding jobs for a sink it expects. For tests.
pub fn expecting_sink() -> bool {
    SINK_WAIT.with(|q| q.borrow().until.is_some())
}

/// How many jobs this thread is holding for a sink. For diagnostics and tests.
pub fn jobs_waiting_for_sink() -> usize {
    SINK_WAIT.with(|q| q.borrow().jobs.len())
}

/// Empty this thread's wait without running or delivering anything, for a test
/// whose queue is fixture rather than subject. Not a production path: a
/// `deliver` dropped rather than run is a leaked render slot. It exists for the
/// **unwind**, since [`SINK_WAIT`] is a thread-local the harness reuses.
#[cfg(test)]
pub(crate) fn clear_sink_wait() {
    SINK_WAIT.with(|q| {
        let mut q = q.borrow_mut();
        q.until = None;
        q.jobs.clear();
    });
}

/// Run `request` without a transport — the funnel's last resort, and the
/// **only** inline execution wasm has left: a browser whose worker is lost pays
/// the render on its one thread, because the alternative is a pane wedged behind
/// a reply that will never come. Natively "here" is still not the calling thread
/// — a thread is spawned, and only a failed spawn runs it truly here. One
/// function because three arms reach it.
fn run_here(name: &'static str, request: JobRequest, deliver: Box<dyn FnOnce(JobResult) + Send>) {
    #[cfg(target_arch = "wasm32")]
    {
        // Timed because this is the one arm where the cost lands on the frame.
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
/// [`run_here`] — `Some` when nothing is expected, or when the wait has run out,
/// in which case the jobs already held come back with it. Nothing a caller wrote
/// runs while the queue is borrowed: an evicted job's `deliver` may dispatch
/// another job, which would otherwise find this queue borrowed.
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
                // Push then evict, so the bound is on what is *held*.
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
                // handles, but the cost differs: `radar-render` is
                // level-triggered and costs a frame; `loop-render` marks the
                // frame `render_failed`; `level2-decode` costs the request.
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
/// [`wait_for_sink`] notices a lapsed deadline only when another job arrives, and
/// this exists for the case where none is coming; the caller that armed the wait
/// schedules a timer for it. A no-op for a deadline that has not passed.
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
/// first, and stop waiting. Each job goes back through [`dispatch`] rather than
/// to the sink directly, so a port that refuses one still falls through to
/// [`run_here`]. It cannot recurse: the queue is emptied before the first one.
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
/// [`WORKER`] is a thread-local and the harness reuses its threads, so a port
/// left installed is one the *next* test inherits — and a failed assertion
/// unwinds straight past a retirement written on the test's last line, while
/// `Drop` runs on the unwind.
pub struct InstalledTestWorker;

impl Drop for InstalledTestWorker {
    fn drop(&mut self) {
        abandon_worker("test teardown");
    }
}

/// Route [`offload_job`] through `port` until the returned guard drops. The
/// test-only counterpart of [`set_worker`]; see [`InstalledTestWorker`].
pub fn install_test_worker(port: Box<dyn JobSink>) -> InstalledTestWorker {
    set_worker(port);
    InstalledTestWorker
}

/// Run `request` away from the frame that requested it, and hand the result to
/// `deliver`, which runs where the result can be used: on the spawned thread
/// natively, and on the main thread in the browser. It carries the
/// `RenderGuard`, the cancellation check, the channel send and the redraw.
///
/// That keeps `PaneRenderState::want_result`'s pruning honest: it treats
/// `Arc::strong_count(flag) > 1` as "still stoppable", and the second reference
/// is the one `deliver` holds for as long as the job is outstanding.
///
/// Answers the id [`cancel_job`] withdraws by, for the described jobs — the
/// only ones a registry entry is owed for. `None` is a job with no such
/// handle: the answer-is-known arm and the test instrument.
pub fn offload_job(
    name: &'static str,
    job: Job,
    deliver: impl FnOnce(JobResult) + Send + 'static,
) -> Option<u64> {
    let request = match job {
        Job::Described(request) => request,
        // The answer is known; `deliver` is what unwinds the caller's marks.
        Job::Nothing => {
            deliver(None);
            return None;
        }
        // The test instrument: a thread of its own, not a pool lane, and inline
        // only if the spawn itself fails. The `Option` lets that arm still run
        // it — `spawn` consumes its closure, so there is no value handed back.
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
            return None;
        }
    };
    Some(dispatch(name, request, Box::new(deliver)))
}

/// [`offload_job`]'s described half, over an already-boxed delivery. Split out
/// for [`hand_waiting_jobs_to_the_sink`], which holds boxed deliveries and has
/// to put them back through the same registry, id space and fallthrough.
/// Answers the job's id — the handle [`cancel_job`] withdraws by.
fn dispatch(
    name: &'static str,
    request: JobRequest,
    deliver: Box<dyn FnOnce(JobResult) + Send>,
) -> u64 {
    // Resolved once, ahead of the send: the row rides the pending entry so the
    // reply is decoded through what THIS dispatch resolved (see `Pending::row`).
    let row = row_for(&request.job);
    let kind = row.label;
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Try the sink on every target: one lifecycle, whichever transport this
    // target installed. The request goes in by value; `Err` hands it back.
    let handoff = WORKER.with(|w| {
        let borrowed = w.borrow();
        let Some((sink_id, sink)) = borrowed.as_ref() else {
            return Handoff::NoSink(request, deliver);
        };
        // Registered **before** the handover: a transport in another thread of
        // this process can answer before `send` returns, and a reply arriving
        // before its entry would be dropped, holding its slot forever.
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
        // Held for the sink this thread expects, run here if it expects none.
        Handoff::NoSink(request, deliver) => {
            if let Some((request, deliver)) = wait_for_sink(name, request, deliver) {
                run_here(name, request, deliver);
            }
        }
        Handoff::Refused(request) => {
            // Running it here is slow but correct; a sink that keeps refusing is
            // a worker that has died, and `abandon_worker` is what retires it.
            log::warn!("{name}: the sink refused the job; running it here");
            // `None` if the sink was abandoned between the insert and the
            // refusal, in which case that job has already been failed.
            if let Some(job) = pending().remove(&id) {
                run_here(name, request, job.deliver);
            }
        }
    }
    id
}

/// What [`offload_job`] learned by offering the job to this thread's sink.
/// Three-way rather than an `Option`: "no sink" is the normal state of a build
/// with no transport, and "refused" is a transport in trouble to be reclaimed.
enum Handoff {
    /// The sink took it; the reply will come through [`deliver_job_reply`].
    Taken,
    /// No sink is installed on this thread. Carries the job back untouched.
    NoSink(JobRequest, Box<dyn FnOnce(JobResult) + Send>),
    /// A sink is installed and refused. The `deliver` is in the registry under
    /// this job's id.
    Refused(JobRequest),
}

/// Hand a sink's answer to the job that asked for it — the one place a job
/// leaves the registry with a result, whichever transport produced it.
///
/// **It runs `deliver` on the calling thread, deliberately.** `deliver` builds
/// the `egui::ColorImage`, which is 206.75 MiB at the 7362 px desktop ceiling,
/// **13.85 ms** on this box, against a 16.7 ms frame; every `deliver` ends in a
/// channel send drained on a later frame, so its placement is unobservable.
/// An `id` with no pending entry is ignored, and the entry is removed **before**
/// `deliver` runs.
pub fn deliver_job_reply(id: u64, result: JobResult) {
    let Some(job) = pending().remove(&id) else {
        log::debug!("reply {id} has no pending job; already abandoned");
        return;
    };
    // The same measurement, for the arms where the time is not on the frame
    // thread.
    log::info!(
        "{} took {} ms off the frame",
        job.kind,
        job.started.elapsed().as_millis()
    );
    (job.deliver)(result);
}

/// Withdraw job `id`: the caller that dispatched it has moved past its answer,
/// so the answer must not be waited for — its `deliver` runs with "nothing"
/// now, on this thread, and whatever the transport still produces for the id
/// is refused at the registry like any other late reply.
///
/// `true` is "the job was still owed and is withdrawn". Whether it also never
/// *runs* depends on where it was: the native pool claims each queued job
/// against this registry at the last instant before `execute`
/// (`pool::lane_job`), so a job withdrawn while it queued is dropped whole —
/// pre-run is the only cancellation there is, because `execute` has no yield
/// point. One already executing runs to completion and its reply is ignored;
/// on the browser transport the worker likewise finishes what it already
/// holds, and the withdrawal saves the page-side decode and delivery instead.
///
/// `false` — already answered, already failed, or never registered (a job the
/// funnel ran inline) — obliges nothing: its `deliver` has run or will run
/// elsewhere, exactly once either way.
pub fn cancel_job(id: u64) -> bool {
    let Some(job) = pending().remove(&id) else {
        return false;
    };
    log::debug!(
        "{}: job {id} withdrawn {} ms after dispatch; delivering nothing",
        job.kind,
        job.started.elapsed().as_millis(),
    );
    (job.deliver)(None);
    true
}

/// [`deliver_job_reply`] for a reply that arrived as bytes — the browser's
/// `OUT`/`OUT_KIND`/`TAILS` trio, or `None` for a worker that answered explicit
/// nulls.
///
/// The decode runs through **the row recorded at dispatch** (`Pending::row`),
/// never a lookup on the reply's own tag: the tag is verified against that row's
/// code and a mismatch is delivered as `None`, with the slot still released.
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
/// tests. Scoped to the sink rather than the process-wide registry, whose count
/// would depend on what test runs beside it.
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

/// The deferred-drop queue frees at the drain's pace, never at the push.
#[cfg(test)]
mod discard_tests;

/// A job dispatched while a sink is on its way waits for it, within a bound and
/// within a deadline.
#[cfg(test)]
mod sink_wait_tests;
