//! The page's side of the rasterization worker.
//!
//! `rustdar_worker::offload` owns where a CPU-bound job runs and knows nothing
//! about the browser, so the worker is *installed* into the funnel as a
//! [`JobSink`] from here.
//!
//! Until the first [`attach`] succeeds, `offload_job` holds jobs for the
//! handshake window and then rasterizes inline. Every failure path ends in
//! "leave it inline **and start another worker**" — see [`lose`].
//!
//! # Why the page and the worker do not share ONE linear memory
//!
//! [`crate::shared_loan`] removes the producer's copy in each direction and
//! leaves the consumer's, and the obvious next step is to delete the second
//! copy too by giving both instances the same memory: `worker.js` would call
//! `init(module, memory)` with the page's, and a raster the worker wrote would
//! be addressable from here with no copy at all. The mechanism is real and
//! already in the bundle — the module is built `--import-memory`, wasm-bindgen
//! therefore emits the threading glue whose init takes a memory, and
//! `wasm-bindgen-rayon` uses exactly that call to put the worker's nested rayon
//! threads on the worker's memory.
//!
//! It is refused anyway, and for a reason that is about the PAGE, not about the
//! handoff. One memory is one heap and one set of `static`s, shared with the
//! browser's main thread — and on wasm32 a contended lock is a blocking wait
//! the main thread is not allowed to perform:
//!
//! * `std::sync::Mutex`, `RwLock` and `OnceLock` reach
//!   `library/std/src/sys/sync/futex/wasm.rs`, whose `futex_wait` is
//!   `memory_atomic_wait32(.., -1)`. That instruction TRAPS on an agent that
//!   cannot block, which every browser main thread is. std states the rule
//!   itself, in `library/std/src/sys/alloc/wasm.rs`: "The main thread in a web
//!   browser *cannot ever block*, no exceptions." The allocator's own lock
//!   spins for precisely this reason; nothing else does.
//! * The uncontended path is a plain CAS, so this is a race and not a certain
//!   failure — which makes it worse, not better. The exposed set is every
//!   `static` lock reachable from both a rasterization job and a frame, and it
//!   is not a set this workspace controls: `wgpu`, `naga` and `egui` are in it.
//! * rayon's global pool is one such `static` too. The page installs a
//!   one-thread `use_current_thread` pool (`crate::rayon_pool`) and the worker
//!   installs the real one; on a shared memory those are the same slot, the
//!   second `build_global` fails, and the loser's `par_iter` submits work to a
//!   pool whose only worker is a thread sitting in the JS event loop. That is a
//!   deadlock, not a slowdown.
//!
//! So the second copy stays. It is bounded, it is on a worker thread's output
//! and a frame thread's input rather than in the middle of either, and a torn
//! or recycled raster reaching a texture upload would be far worse than a
//! memcpy. Reopening this needs the main thread kept OUT of the shared memory,
//! which is a different architecture and not a follow-up to this one.

use crate::worker_protocol as proto;
use crate::worker_retry::Backoff;
use rustdar_worker::offload::{self, JobRequest, JobSink};
use std::cell::Cell;
use wasm_bindgen::prelude::*;

/// Where the worker's bootstrap lives, relative to the page.
///
/// Relative on purpose: the site is served from a project-Pages subpath.
const WORKER_URL: &str = "./worker.js";

/// How long a job will wait for a worker that has just been started, before it
/// gives up and runs on the page's own thread.
///
/// Bounds the handshake and not the recovery: a job held across a minute-long
/// backoff is a pane blank for a minute. A policy, not a measurement.
const HANDSHAKE_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

thread_local! {
    /// Which worker this page is listening to.
    ///
    /// **A worker that has been replaced must not speak for the page.** Every
    /// closure below captures the generation it was created under.
    static GENERATION: Cell<u64> = const { Cell::new(0) };

    /// The ladder the next respawn waits. See [`crate::worker_retry`].
    static BACKOFF: Cell<Backoff> = const { Cell::new(Backoff::new()) };

    /// Whether a respawn is already on a timer, so a `FATAL` and an `onerror` from
    /// the same dying worker schedule one attempt.
    static RESPAWN_SCHEDULED: Cell<bool> = const { Cell::new(false) };
}

/// Start the rasterization worker and, once it identifies itself as this same
/// build, route [`offload::offload_job`] through it.
pub fn attach() {
    spawn();
}

/// One attempt: a fresh generation, a fresh arming of the funnel's wait, and a
/// `Worker`.
fn spawn() {
    let generation = GENERATION.with(|g| {
        let next = g.get().wrapping_add(1);
        g.set(next);
        next
    });

    // Armed **before** the worker exists: a job dispatched in this same turn of the
    // event loop would otherwise pay the decode on this thread.
    offload::expect_sink(HANDSHAKE_WINDOW);
    // And a timer for the deadline: `offload_job` notices a lapsed wait only when
    // another job arrives.
    after(HANDSHAKE_WINDOW.as_millis() as i32, || {
        offload::flush_expired_sink_wait()
    });

    let options = web_sys::WorkerOptions::new();
    // A module worker, because `worker.js` `import`s the wasm-bindgen glue that
    // `--target web` emits. Classic workers cannot.
    options.set_type(web_sys::WorkerType::Module);

    let worker = match web_sys::Worker::new_with_options(WORKER_URL, &options) {
        Ok(worker) => worker,
        Err(e) => {
            log::warn!("no rasterization worker ({e:?}); rendering on the main thread");
            lose(generation, "the worker could not be constructed");
            return;
        }
    };

    let on_message_worker = worker.clone();
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            handle_message(generation, &on_message_worker, &event.data());
        });
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    // A worker that dies mid-job owes replies that never arrive, each holding a
    // render slot; `abandon_worker` fails them.
    let on_error_worker = worker.clone();
    let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        on_error_worker.terminate();
        lose(generation, "the worker reported an error");
    });
    worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();
}

/// Give up on this worker and start the clock on another.
///
/// [`offload::abandon_worker`] fails the jobs *this* worker owed and
/// [`schedule_respawn`] makes sure the job after them has somewhere to go. A
/// stale generation returns without doing either.
fn lose(generation: u64, reason: &str) {
    if GENERATION.with(Cell::get) != generation {
        log::debug!("ignoring {reason} from a worker this page has already replaced");
        return;
    }
    offload::abandon_worker(reason);
    // The requests this page lent that worker are owed `RELEASE`s it will never
    // send. Nothing is reading them any more — a replaced worker's messages are
    // dropped by generation before they reach a handler — so the sweep is the
    // whole of the cleanup.
    crate::shared_loan::release_all(reason);
    schedule_respawn();
}

/// Put the next [`spawn`] on a timer, one rung further up the ladder.
///
/// Idempotent while a respawn is outstanding.
fn schedule_respawn() {
    if RESPAWN_SCHEDULED.with(|scheduled| scheduled.replace(true)) {
        return;
    }
    let delay_ms = BACKOFF.with(|backoff| {
        let mut ladder = backoff.get();
        let delay = ladder.next_delay_ms();
        backoff.set(ladder);
        delay
    });
    log::warn!("starting another rasterization worker in {delay_ms} ms");
    after(delay_ms as i32, || {
        RESPAWN_SCHEDULED.with(|scheduled| scheduled.set(false));
        spawn();
    });
}

/// Run `then` after `delay_ms`.
///
/// A callback that cannot be scheduled is dropped, not run here: `then` invoked
/// on the spot would recurse until the stack ran out.
fn after(delay_ms: i32, then: impl FnOnce() + 'static) {
    let Some(window) = web_sys::window() else {
        log::error!("no window to schedule a {delay_ms} ms timer against");
        return;
    };
    let callback = Closure::once_into_js(then);
    if let Err(e) = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay_ms)
    {
        log::error!("could not schedule a {delay_ms} ms timer ({e:?})");
    }
}

fn handle_message(generation: u64, worker: &web_sys::Worker, data: &JsValue) {
    // A message from a worker this page has replaced is dropped, before `DONE` as
    // well as the lifecycle kinds.
    if GENERATION.with(Cell::get) != generation {
        log::debug!("ignoring a message from a worker this page has already replaced");
        return;
    }
    match proto::string_field(data, proto::KIND).as_deref() {
        Some(proto::HELLO) => {
            let theirs = proto::string_field(data, proto::TOKEN).unwrap_or_default();
            let ours = proto::build_token();
            if theirs != ours {
                // Not this build: the worker is its own service-worker client and can be
                // served a different shell generation than the page. The token check refuses
                // a *pair*, not a browser, so it respawns.
                log::warn!(
                    "rasterization worker is a different build ({theirs} vs {ours}); \
                     rendering on the main thread"
                );
                worker.terminate();
                lose(generation, "build token mismatch");
                return;
            }
            // The thread count is reported as the worker stated it, or as
            // `?` when the worker did not state one at all — a pre-WS3b
            // build. Printing `1` for an absent field would read as a
            // measured single-threaded pool.
            let threads = proto::field(data, proto::THREADS)
                .and_then(|v| v.as_f64())
                .map_or_else(|| "?".to_string(), |n| (n as usize).to_string());
            log::info!("rasterization worker attached ({ours}, rayon: {threads} threads)");
            // The ladder resets **here**, on a worker that has proved itself.
            BACKOFF.with(|backoff| {
                let mut ladder = backoff.get();
                ladder.reset();
                backoff.set(ladder);
            });
            offload::set_worker(Box::new(Port {
                worker: worker.clone(),
            }));
        }
        Some(proto::FATAL) => {
            let error = proto::string_field(data, proto::ERROR).unwrap_or_default();
            log::warn!(
                "rasterization worker failed to start ({error}); rendering on the main thread"
            );
            worker.terminate();
            lose(generation, "the worker failed to start");
        }
        Some(proto::DONE) => deliver(worker, data),
        // The worker has finished copying a request out of this page's memory.
        Some(proto::RELEASE) => crate::shared_loan::release(proto::loan_field(data)),
        other => log::warn!("ignoring a worker message of kind {other:?}"),
    }
}

/// Hand a `done` message to the job that asked for it.
///
/// The reply is the `OUT`/`OUT_KIND`/`TAILS` trio, or explicit nulls for a job
/// that produced nothing. Reading each buffer is ONE copy into this page's
/// linear memory, and **that copy does not retire**: `decode_out` takes a
/// `Vec<u8>`, `egui::ColorImage` holds one and `wgpu`'s `write_texture` takes a
/// `&[u8]` — all of them slices of THIS instance's memory, which a view onto a
/// foreign buffer cannot be at any price. The second copy is a property of the
/// types, not of the transport.
///
/// What WS3c retires is the other one. A `SharedArrayBuffer` crosses
/// `postMessage` by sharing, so when the browser is cross-origin isolated the
/// worker posts VIEWS onto its own memory instead of copies of it, and this
/// page copies once instead of twice. [`crate::shared_loan`] holds the
/// protocol; here the borrow is discharged by copying every buffer
/// **synchronously, before this function returns**, and then sending `RELEASE`.
/// Nothing may hold a view past that point — the region is the worker's to
/// reuse the moment the release lands.
///
/// Both wires are read by the same code: a view and a transferred copy are both
/// `Uint8Array`s and `to_vec` is the same call on either. What tells them apart
/// is [`crate::shared_loan::is_foreign_shared`], which is an OBSERVATION of the
/// buffer that arrived, not a report of what the sender intended — the Tier-2
/// assertion is built on it for exactly that reason.
fn deliver(worker: &web_sys::Worker, data: &JsValue) {
    let Some(id) = proto::field(data, proto::ID).and_then(|v| v.as_f64()) else {
        log::error!("worker answered with no job id");
        return;
    };
    let loan = proto::loan_field(data);

    let mut moved = 0usize;
    let mut copied_at_worker = 0usize;
    let mut count = |array: &js_sys::Uint8Array| {
        let len = array.length() as usize;
        moved += len;
        if !crate::shared_loan::is_foreign_shared(array) {
            copied_at_worker += len;
        }
    };

    let reply = (|| {
        // Undefined as well as null: `post_result` writes an explicit null on
        // every path, so undefined can only mean a worker that built its views
        // and found none — nothing to draw either way.
        let out = proto::field(data, proto::OUT).filter(|v| !v.is_null() && !v.is_undefined())?;
        let kind = proto::field(data, proto::OUT_KIND)
            .and_then(|v| v.as_f64())
            .map(|v| v as u8)?;
        let out = out.dyn_into::<js_sys::Uint8Array>().ok()?;
        count(&out);
        let head = out.to_vec();
        // TAILS null or absent reads as no tails.
        let tails = match proto::field(data, proto::TAILS).filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => {
                let array = v.dyn_into::<js_sys::Array>().ok()?;
                let mut tails = Vec::with_capacity(array.length() as usize);
                for tail in array.iter() {
                    // The same checked cast per tail — one copy each.
                    let tail = tail.dyn_into::<js_sys::Uint8Array>().ok()?;
                    count(&tail);
                    tails.push(tail.to_vec());
                }
                tails
            }
        };
        Some((kind, head, tails))
    })();

    // **Before** `deliver_encoded_reply`, which runs the caller's delivery and
    // can be milliseconds of `ColorImage` building: every view above has been
    // copied out by now, and the worker is holding multiple MiB until it hears
    // so. Releasing after the delivery would hold them across it for no reason.
    release_to_worker(worker, loan);
    account(moved, copied_at_worker, 0, 0);

    // `None` still delivers: the caller's slot is released either way.
    offload::deliver_encoded_reply(id as u64, reply);
}

/// Tell the worker it may free the reply this page has now copied out.
///
/// A failed post costs the worker a held buffer until it is retired, and must
/// not fail the job: the answer is already in this page's memory.
fn release_to_worker(worker: &web_sys::Worker, loan: crate::shared_loan::LoanId) {
    if loan == crate::shared_loan::NO_LOAN {
        return;
    }
    let message = js_sys::Object::new();
    proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::RELEASE));
    proto::set_loan(&message, loan);
    if let Err(e) = worker.post_message(&message) {
        log::warn!("could not release the worker's loan {loan}: {e:?}");
    }
}

thread_local! {
    /// What the wire has actually moved and what it actually copied, since the
    /// page loaded. See [`account`].
    static TRAFFIC: Cell<Traffic> = const { Cell::new(Traffic::ZERO) };
}

/// The transport's own ledger, in bytes.
///
/// Cumulative rather than per-message so that ONE log line carries the whole
/// answer: a per-message line would make the reader sum a console ring that
/// evicts, and the ring is the only instrument the browser rig can read.
#[derive(Clone, Copy)]
struct Traffic {
    replies: u64,
    out_moved: u64,
    /// Of [`Self::out_moved`], how much arrived as a buffer the worker had
    /// copied out of its own memory. **Zero is the whole claim of WS3c**, and
    /// it is counted from what the page received, not from what the worker
    /// meant to send.
    out_copied: u64,
    in_moved: u64,
    /// Of [`Self::in_moved`], how much this page copied out of its own memory
    /// to hand over, rather than lending in place.
    in_copied: u64,
}

impl Traffic {
    const ZERO: Self = Self {
        replies: 0,
        out_moved: 0,
        out_copied: 0,
        in_moved: 0,
        in_copied: 0,
    };
}

/// Add one message to the ledger and log the running totals.
///
/// The line is the Tier-2 instrument (`drive.py --expect-zero-copy-replies`)
/// and the measurement at once, which is deliberate: a gate that reads a
/// different number than the report would let the two drift. It is worded so
/// that a transport which quietly reverted to copying cannot satisfy it —
/// `out_copied` would climb — and so that a transport that moved NOTHING
/// cannot satisfy it either, because the assertion also requires `out_moved`
/// to be positive.
fn account(out_moved: usize, out_copied: usize, in_moved: usize, in_copied: usize) {
    let totals = TRAFFIC.with(|traffic| {
        let mut totals = traffic.get();
        if out_moved > 0 || out_copied > 0 {
            totals.replies += 1;
        }
        totals.out_moved += out_moved as u64;
        totals.out_copied += out_copied as u64;
        totals.in_moved += in_moved as u64;
        totals.in_copied += in_copied as u64;
        traffic.set(totals);
        totals
    });
    log::info!(
        "transport: {} replies, {} B out with {} B copied out of the worker, \
         {} B in with {} B copied out of this page",
        totals.replies,
        totals.out_moved,
        totals.out_copied,
        totals.in_moved,
        totals.in_copied,
    );
}

/// The installed port. Owns the `Worker` handle, so the worker lives as long as
/// the funnel will send it jobs.
struct Port {
    worker: web_sys::Worker,
}

impl JobSink for Port {
    /// A `JobRequest` is not a thing a `Worker` can be handed: the only payload a
    /// `postMessage` transfer list moves is a detachable `ArrayBuffer`. `to_bytes`
    /// borrows, so a failed post hands the request back to run inline.
    ///
    /// The same two wires as the reply direction and for the same reason.
    /// Isolated, the request bytes are LENT — a view onto this page's own
    /// memory, released when the worker says it has copied them, which it does
    /// before it starts the job rather than after. Otherwise `Uint8Array::from`
    /// copies them out and the transfer list moves the copy: ~1.3 MB for an
    /// 8-bit moment and up to ~47-69 MiB for a decode.
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        let message = js_sys::Object::new();
        proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::JOB));
        proto::set_field(&message, proto::ID, &JsValue::from_f64(id as f64));
        proto::set_loan(&message, crate::shared_loan::NO_LOAN);

        let bytes = request.to_bytes();
        let moved = bytes.len();
        let transfer = js_sys::Array::new();
        let (loan, copied) = match crate::shared_loan::lend(vec![bytes]) {
            Ok((loan, views)) => {
                proto::set_loan(&message, loan);
                proto::set_field(&message, proto::REQUEST, &views.get(0));
                (loan, 0)
            }
            Err(mut bytes) => {
                // One buffer went in, so one comes back; an empty `Vec` here
                // would be a bookkeeping bug rather than a case, and the empty
                // request it produces is answered as a failed job.
                let bytes = bytes.pop().unwrap_or_default();
                let payload = js_sys::Uint8Array::from(bytes.as_slice());
                transfer.push(&payload.buffer());
                proto::set_field(&message, proto::REQUEST, &payload);
                (crate::shared_loan::NO_LOAN, moved)
            }
        };
        account(0, 0, moved, copied);

        match self.worker.post_message_with_transfer(&message, &transfer) {
            Ok(()) => Ok(()),
            Err(e) => {
                // The funnel runs the job here instead; `onerror` retires a dead worker.
                log::warn!("could not post job {id} to the worker: {e:?}");
                // A message that never left cannot be answered with a `RELEASE`,
                // so the loan is discharged here. Without this every refused
                // post would hold its request until the worker was retired —
                // and a refusing worker is exactly the one that keeps refusing.
                crate::shared_loan::release(loan);
                Err(request)
            }
        }
    }
}
