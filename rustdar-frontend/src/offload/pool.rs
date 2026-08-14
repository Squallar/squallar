//! The native transport: a bounded pool of threads behind [`JobSink`].
//!
//! # Why a pool and not a thread
//!
//! Because the browser's transport is a pool of one, and the funnel above this
//! module should not be able to tell the two apart. Everything that decides a
//! job's fate — its id, its entry in the registry, the sink that owes it an
//! answer, the single place a reply retires it — is written once in
//! `super::offload_job` and reached identically from here and from
//! `rustdar-web`'s `Port`. What differs is the mechanism underneath, which is
//! what a transport *is*.
//!
//! What desktop had instead was a fresh named OS thread per job, unbounded:
//! no id, no registry entry, no bound past the render admission check, and no
//! failure path — a render thread that panicked took its pane's in-flight mark
//! and its render slot with it, permanently. All four of those come back here.
//!
//! # Nothing is copied
//!
//! `JobSink::send` takes the request **by value**, so this arm moves it: an
//! `mpsc::Sender::send` transfers ownership of the `Box<RenderInput>` and the
//! `Arc` payloads inside it, and the receiving thread gets the same
//! allocation. There is no `to_bytes` on this path and there must never be one
//! — the browser serialises because a `postMessage` transfer list is the only
//! handover a browser has, and that is the browser's charge, not the design's.
//!
//! Asserted by allocation identity in this module's tests, and measured as
//! dispatch cost against payload size, which is the shape a copy cannot fake:
//!
//! | payload | this transport | what serialising it would cost |
//! |---|---|---|
//! | 1 MiB | 0.05 µs | 15.44 µs |
//! | 16 MiB | 0.05 µs | 1393.28 µs |
//! | 128 MiB | 0.06 µs | 31578.21 µs |
//!
//! Flat across a 128× payload. The thread-per-job it replaces cost 8.40–13.62
//! µs on the calling thread — which for a still render is the frame thread —
//! so dispatch got ~200× cheaper as well as bounded.
//!
//! # Two lanes
//!
//! Described jobs and opaque closures do not queue behind each other.
//!
//! A described job is a rasterization or a volume decode: tens to hundreds of
//! milliseconds, and the render admission check in `render_dispatch` already
//! bounds how many can be outstanding at `Budgets::concurrent_renders`. An
//! opaque closure is [`super::offload`]'s escape hatch — the two overlay
//! rasterizations that cannot be described, which follow the map and are
//! wanted *now*. One lane would let a full slate of radar renders put an
//! overlay behind a second of work that a thread-per-job never made it wait
//! for. Two lanes, each sized at the render bound, is the shape that keeps the
//! bound and does not invent that stall.
//!
//! What the opaque lane's own bound costs was measured rather than assumed:
//! `rasterize_radar_sites` over 200 markers is **3.16 ms at 1920×1080 and 3.70
//! ms at 3840×2160**, so a burst wider than the lane pays single-digit
//! milliseconds to wait — against a thread spawn that was never free either.
//!
//! # In-flight cancellation is not here, on purpose
//!
//! `super::execute` has no yield point, and the only place to add one is the
//! rasterizer's inner loop. A queued job could be dropped before it starts —
//! the pool is what makes that possible for the first time — but the signal
//! that would justify it (`PaneRenderState::want_result`'s flag) lives inside
//! the `deliver` closure the registry holds, not in the request, so it is a
//! change to the funnel rather than to this module.

use super::{JobRequest, JobResult, JobSink};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

/// The pool's two queues, started once per process.
struct Pool {
    described: mpsc::Sender<(u64, JobRequest)>,
    opaque: mpsc::Sender<Opaque>,
}

/// An [`super::offload`] closure and the name it was dispatched under, which is
/// all a panic report has to identify it by.
type Opaque = (&'static str, Box<dyn FnOnce() + Send>);

/// Started on first use rather than at launch, so a build that never offloads
/// anything — every test binary in this workspace that does not touch the
/// funnel — never pays for the threads.
static POOL: OnceLock<Pool> = OnceLock::new();

fn pool() -> &'static Pool {
    POOL.get_or_init(start)
}

fn start() -> Pool {
    // The same number the render admission check spends, from the same
    // resolver, because the two bound the same thing: `concurrent_renders`
    // jobs can be outstanding by construction, so a lane of that width is a
    // lane that never queues a job the application was willing to start.
    //
    // Resolved here rather than handed in because this pool is a process
    // singleton reached from any thread, and `resolve` is a pure function of a
    // profile every caller would have to build identically anyway.
    let threads = crate::budget::resolve(&crate::budget::DeviceProfile::for_target())
        .concurrent_renders
        .max(1);

    let (described, described_rx) = mpsc::channel();
    lane("rd-job", threads, described_rx, |(id, request)| {
        // Delivered on this thread, which is where it was delivered when this
        // was a thread per job. See `super::deliver_job_reply` for why the
        // transport and not the funnel decides that.
        super::deliver_job_reply(id, run(super::JobRequest::kind(&request), &request));
    });

    let (opaque, opaque_rx) = mpsc::channel();
    lane("rd-opaque", threads, opaque_rx, |(name, job): Opaque| {
        // No registry entry and no id: an opaque job is a closure that owns its
        // own delivery, which is exactly why it could not be described.
        if guarded(name, job).is_none() {
            log::error!("{name} panicked; its result will never arrive");
        }
    });

    log::debug!("job pool started with {threads} thread(s) per lane");
    Pool { described, opaque }
}

/// Start `threads` workers that take `T`s off `rx` and hand each to `run`.
///
/// The receiver is shared under a `Mutex` rather than duplicated: `mpsc` has
/// one consumer, and the workers take turns being it. The lock is held across
/// the blocking `recv` — which is what makes an idle worker cost nothing and a
/// woken one take exactly one task — and released before `run`, so the work
/// itself is as parallel as the thread count.
fn lane<T: Send + 'static>(name: &'static str, threads: usize, rx: mpsc::Receiver<T>, run: fn(T)) {
    let queue = Arc::new(Mutex::new(rx));
    for n in 0..threads {
        let queue = Arc::clone(&queue);
        let spawned = std::thread::Builder::new()
            .name(format!("{name}-{n}"))
            .spawn(move || {
                loop {
                    let task = {
                        // A poisoned queue is a worker that panicked while
                        // holding it, which cannot happen — nothing under this
                        // lock but `recv` — and if it somehow did, the receiver
                        // behind it is intact and refusing it would retire the
                        // whole lane.
                        let queue = queue.lock().unwrap_or_else(|e| e.into_inner());
                        queue.recv()
                    };
                    let Ok(task) = task else {
                        // Every sender is gone: the process is going away, and
                        // this lane has nothing left to be handed.
                        return;
                    };
                    // The backstop under the per-arm one. A `deliver` that
                    // panics must not take the worker with it, or the lane
                    // narrows by one for the rest of the session and every job
                    // it would have run waits on the remainder.
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(task))).is_err()
                    {
                        log::error!("{name}-{n}: a job panicked after its work was done");
                    }
                }
            });
        if let Err(e) = spawned {
            // Fewer threads than asked for is a slower pool, not a broken one:
            // whatever did start drains the same queue. Zero is the case that
            // matters, and `Handle::send` answers `Err` for it once the channel
            // has no live receiver — which puts the job back in the funnel's
            // hands to run inline.
            log::error!("could not start {name}-{n} ({e}); the lane is one thread short");
        }
    }
}

/// Run a described job, answering `None` for one that panicked.
///
/// **This is the failure path native did not have.** A rasterizer that panics
/// used to take the whole thread down with the job's `deliver` still un-run,
/// which left the pane's in-flight mark set and its render slot taken for the
/// rest of the session — a pane that goes blank and never recovers, with
/// nothing in the log tying it to the panic. Answering `None` is the same
/// "nothing to draw" every other failure already produces, and the caller's
/// slot is released on the way through.
fn run(kind: &'static str, request: &JobRequest) -> JobResult {
    guarded(kind, || super::execute(request)).flatten()
}

/// `f`'s value, or `None` if it panicked. The panic is logged here rather than
/// only by the default hook, so the message names the job.
fn guarded<T>(kind: &'static str, f: impl FnOnce() -> T) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(_) => {
            log::error!("a {kind} job panicked; it answers with nothing");
            None
        }
    }
}

/// Hand `job` to the opaque lane. [`super::offload`]'s native arm.
///
/// `Err` carries the closure back, for [`JobSink::send`]'s reason: the only way
/// to refuse is a lane with no worker left, and the caller's answer is to run
/// the closure where it stands. Slow, and the honest alternative to dropping a
/// job whose `deliver` is the only thing that will ever release the caller's
/// render slot.
pub(super) fn run_opaque(
    name: &'static str,
    job: Box<dyn FnOnce() + Send>,
) -> Result<(), Box<dyn FnOnce() + Send>> {
    pool()
        .opaque
        .send((name, job))
        .map_err(|returned| returned.0.1)
}

/// This thread's handle to the pool.
///
/// A cloned `mpsc::Sender` — `Send` but not `Sync`, so each thread holds its
/// own, which is also what the funnel's thread-local wants. The pool behind
/// them is one.
pub(super) fn sink() -> Box<dyn JobSink> {
    Box::new(Handle {
        described: pool().described.clone(),
    })
}

struct Handle {
    described: mpsc::Sender<(u64, JobRequest)>,
}

impl JobSink for Handle {
    /// # The queue is unbounded, and that is the back-pressure story
    ///
    /// Admission is upstream and already bounded: `render_dispatch` refuses a
    /// render past `Budgets::concurrent_renders` and the pane asks again next
    /// frame. A bound here as well would be a second, differently-shaped
    /// refusal for the same condition, and the funnel's answer to a refusal is
    /// to run the job on the calling thread — which for a rasterization is the
    /// frame.
    ///
    /// So the only `Err` this can answer is a lane with no live worker at all,
    /// and the job goes back to the funnel with nothing copied: `mpsc` hands
    /// the value back inside its `SendError`.
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        self.described
            .send((id, request))
            .map_err(|returned| returned.0.1)
    }
}

#[cfg(test)]
mod tests;
