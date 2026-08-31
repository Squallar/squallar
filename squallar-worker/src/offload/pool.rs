//! The native transport: a bounded pool of threads behind [`JobSink`].
//!
//! A pool rather than a thread per job because the browser's transport is a
//! pool of one, and the funnel above should not be able to tell the two apart.
//!
//! `JobSink::send` takes the request **by value**, so this arm moves it — an
//! `mpsc::Sender::send` transfers the `Box<RenderInput>` and the `Arc` payloads
//! inside it. There is no `to_bytes` on this path and there must never be one.
//! Measured dispatch cost against payload size: 0.05 / 0.05 / 0.06 µs at 1 / 16
//! / 128 MiB, against 15.44 / 1393.28 / 31578.21 µs to serialise the same
//! payloads, and 8.40–13.62 µs for the thread-per-job it replaces.
//!
//! Three lanes, so radar-paced and map-paced work do not queue behind each
//! other. `rd-job` carries the radar renders and volume decodes (already
//! bounded by `render_dispatch`'s `Budgets::concurrent_renders`); `rd-opaque`
//! carries the overlay rasterizations, which follow the map and are wanted
//! *now* (measured: `rasterize_radar_sites` over 200 markers is 3.16 ms at
//! 1920×1080, 3.70 ms at 3840×2160), and the `terrain/` rows, which follow the
//! volume box for the same reason: a box that moves posts a volume rebuild and
//! a height resample together, and queueing the ground behind the thing
//! standing on it is the stall the split exists to prevent. Each is sized at
//! the render bound.
//! [`super::discard`]'s frees are a third queue, one thread wide because frees
//! serialise on the allocator — which also means the split removes the queueing
//! delay without making a free free: under glibc this is a cross-arena free
//! taking a lock the next decode wants.
//!
//! [`start`] builds all three lanes together, so the thread count is
//! `2 × concurrent_renders + 1`.
//!
//! Cancellation here is **pre-run only**: `super::execute` has no yield point,
//! so the one seam is the claim [`lane_job`] makes against the pending
//! registry at the last instant before running — a job withdrawn by
//! [`super::cancel_job`] while it queued is dropped whole, and one already
//! executing runs to completion with its reply refused at the registry.
//! `PaneRenderState::want_result`'s flag is a different, later gate: it lives
//! inside the `deliver` closure the registry holds, not in the request.

use super::{JobRequest, JobResult, JobSink};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

/// The pool's three queues, started once per process. Which lane a job rides is
/// a question about its **deadline**, not its shape, so both job lanes carry the
/// same `(id, request)` pair.
struct Pool {
    described: mpsc::Sender<(u64, JobRequest)>,
    interactive: mpsc::Sender<(u64, JobRequest)>,
    free: mpsc::Sender<Doomed>,
}

/// A payload on its way to the free lane, and the name it was discarded under.
type Doomed = (&'static str, Box<dyn std::any::Any + Send>);

/// Started on first use rather than at launch, so a build that never offloads
/// anything never pays for the threads.
static POOL: OnceLock<Pool> = OnceLock::new();

fn pool() -> &'static Pool {
    POOL.get_or_init(start)
}

fn start() -> Pool {
    // The same number the render admission check spends, from the same
    // resolver: `concurrent_renders` jobs can be outstanding by construction,
    // so a lane of that width never queues a job the app was willing to start.
    let threads = squallar_device_profile::budget::resolve(
        &squallar_device_profile::budget::DeviceProfile::for_target(),
    )
    .concurrent_renders
    .max(1);

    let (described, described_rx) = mpsc::channel();
    lane("rd-job", threads, described_rx, lane_job);

    // The same body as the described lane's, on the queue with the interactive
    // deadline: the lane a job rides must never change what running it means.
    let (interactive, interactive_rx) = mpsc::channel();
    lane("rd-opaque", threads, interactive_rx, lane_job);

    // One thread, deliberately: frees serialise on the allocator.
    let (free, free_rx) = mpsc::channel();
    lane("rd-free", 1, free_rx, |(name, payload): Doomed| {
        let started = web_time::Instant::now();
        // A `Drop` that panics must not take the lane's only thread with it, or
        // every later discard queues behind a receiver nobody is draining.
        if guarded(name, move || drop(payload)).is_none() {
            log::error!("{name}: a payload panicked while being freed");
        }
        log::debug!(
            "{name}: freed in {} µs off the frame",
            started.elapsed().as_micros()
        );
    });

    log::debug!("job pool started with {threads} thread(s) per lane, and one to free");
    Pool {
        described,
        interactive,
        free,
    }
}

/// Start `threads` workers that take `T`s off `rx` and hand each to `run`.
///
/// The receiver is shared under a `Mutex` rather than duplicated: `mpsc` has one
/// consumer, and the workers take turns being it. The lock is held across the
/// blocking `recv` and released before `run`.
fn lane<T: Send + 'static>(name: &'static str, threads: usize, rx: mpsc::Receiver<T>, run: fn(T)) {
    let queue = Arc::new(Mutex::new(rx));
    for n in 0..threads {
        let queue = Arc::clone(&queue);
        let spawned = std::thread::Builder::new()
            .name(format!("{name}-{n}"))
            .spawn(move || {
                loop {
                    let task = {
                        // A poisoned queue cannot happen — nothing under this
                        // lock but `recv` — and refusing it would retire the lane.
                        let queue = queue.lock().unwrap_or_else(|e| e.into_inner());
                        queue.recv()
                    };
                    let Ok(task) = task else {
                        // Every sender is gone: the process is going away.
                        return;
                    };
                    // The backstop under the per-arm one: a `deliver` that
                    // panics must not take the worker with it, or the lane
                    // narrows by one for the rest of the session.
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(task))).is_err()
                    {
                        log::error!("{name}-{n}: a job panicked after its work was done");
                    }
                }
            });
        if let Err(e) = spawned {
            // Fewer threads than asked for is a slower pool, not a broken one.
            // Zero is the case that matters, and `Handle::send` answers `Err`
            // once the channel has no live receiver, putting the job back in
            // the funnel's hands to run inline.
            log::error!("could not start {name}-{n} ({e}); the lane is one thread short");
        }
    }
}

/// Queued jobs the lanes dropped because they were withdrawn before they ran —
/// the counted "never executed" half of a [`super::cancel_job`]. Diagnostics
/// and tests; nothing gates on it.
static WITHDRAWN_BEFORE_RUN: AtomicU64 = AtomicU64::new(0);

/// How many queued jobs the lanes have skipped unrun. See [`lane_job`].
#[cfg_attr(not(test), expect(dead_code))]
pub(super) fn withdrawn_before_run() -> u64 {
    WITHDRAWN_BEFORE_RUN.load(Relaxed)
}

/// One queued job, both lanes' shared body: claim it against the registry,
/// then run and deliver.
///
/// The claim is asked at the last instant before [`super::execute`], which has
/// no yield point — pre-run is the only cancellation there is. A job whose
/// registry entry is gone was withdrawn ([`super::cancel_job`]) or failed
/// (`super::abandon_worker`) while it queued; its `deliver` has already run,
/// so running the job now could only spend the lane's time on an answer the
/// registry would refuse.
pub(super) fn lane_job((id, request): (u64, JobRequest)) {
    let kind = JobRequest::kind(&request);
    if !super::job_is_owed(id) {
        WITHDRAWN_BEFORE_RUN.fetch_add(1, Relaxed);
        log::debug!("{kind}: job {id} was withdrawn while it queued; never ran");
        return;
    }
    // Delivered on this thread. See `super::deliver_job_reply`.
    super::deliver_job_reply(id, run(kind, &request));
}

/// Run a described job, answering `None` for one that panicked.
///
/// **This is the failure path native did not have.** A rasterizer that panicked
/// used to take the thread down with the job's `deliver` still un-run, leaving
/// the pane's in-flight mark set and its render slot taken for the rest of the
/// session. `None` is the same "nothing to draw" every other failure produces.
fn run(kind: &'static str, request: &JobRequest) -> JobResult {
    guarded(kind, || super::execute(request)).flatten()
}

/// `f`'s value, or `None` if it panicked. Logged here so the message names the
/// job.
fn guarded<T>(kind: &'static str, f: impl FnOnce() -> T) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(_) => {
            log::error!("a {kind} job panicked; it answers with nothing");
            None
        }
    }
}

/// Hand `payload` to the free lane. [`super::discard`]'s native arm.
///
/// `Err` carries the payload back: the queue is unbounded, so a refusal is a
/// lane with no live worker and never back-pressure. The caller's answer is not
/// to free it where it stands — [`super::discard`] files it in the deferred
/// queue instead.
pub(super) fn run_free(
    name: &'static str,
    payload: Box<dyn std::any::Any + Send>,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    pool().free.send((name, payload)).map_err(|back| back.0.1)
}

/// This thread's handle to the pool: a cloned `mpsc::Sender`, `Send` but not
/// `Sync`, so each thread holds its own. The pool behind them is one.
pub(super) fn sink() -> Box<dyn JobSink> {
    Box::new(Handle {
        described: pool().described.clone(),
        interactive: pool().interactive.clone(),
    })
}

struct Handle {
    described: mpsc::Sender<(u64, JobRequest)>,
    /// The `rd-opaque` lane's sender, for the described overlay jobs whose
    /// deadline is the map's.
    interactive: mpsc::Sender<(u64, JobRequest)>,
}

impl JobSink for Handle {
    /// The queue is unbounded, and that is the back-pressure story: admission
    /// is upstream and already bounded by `Budgets::concurrent_renders`, and a
    /// second refusal here would make the funnel run the job on the calling
    /// thread — which for a rasterization is the frame. So the only `Err` this
    /// can answer is a lane with no live worker at all, and the job goes back
    /// with nothing copied: `mpsc` hands the value back inside its `SendError`.
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        // The one routing decision this transport makes, over the job's
        // deadline rather than its kind: an overlay follows the map, and a
        // height field follows the volume box, and neither must queue behind a
        // slate of radar renders. The question is asked of the row's own label
        // prefix; `offload::tests`' lane test pins both families.
        let label = super::row_for(&request.job).label;
        let lane = if label.starts_with("overlay/") || label.starts_with("terrain/") {
            &self.interactive
        } else {
            &self.described
        };
        lane.send((id, request)).map_err(|returned| returned.0.1)
    }
}

#[cfg(test)]
mod tests;
