//! The page's side of the rasterization worker.
//!
//! `rustdar_worker::offload` owns where a CPU-bound job runs and knows nothing
//! about the browser, so the worker is *installed* into the funnel as a
//! [`JobSink`] from here.
//!
//! Until the first [`attach`] succeeds, `offload_job` holds jobs for the
//! handshake window and then rasterizes inline. Every failure path ends in
//! "leave it inline **and start another worker**" — see [`lose`].

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
            log::info!("rasterization worker attached ({ours})");
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
        Some(proto::DONE) => deliver(data),
        other => log::warn!("ignoring a worker message of kind {other:?}"),
    }
}

/// Hand a `done` message to the job that asked for it.
///
/// The reply is the `OUT`/`OUT_KIND`/`TAILS` trio, or explicit nulls for a job
/// that produced nothing. Reading each buffer is ONE copy into linear memory,
/// unavoidable without a `SharedArrayBuffer`, which needs COOP/COEP headers
/// GitHub Pages does not let this deployment set.
fn deliver(data: &JsValue) {
    let Some(id) = proto::field(data, proto::ID).and_then(|v| v.as_f64()) else {
        log::error!("worker answered with no job id");
        return;
    };

    let reply = (|| {
        let out = proto::field(data, proto::OUT).filter(|v| !v.is_null())?;
        let kind = proto::field(data, proto::OUT_KIND)
            .and_then(|v| v.as_f64())
            .map(|v| v as u8)?;
        let head = out.dyn_into::<js_sys::Uint8Array>().ok()?.to_vec();
        // TAILS null or absent reads as no tails.
        let tails = match proto::field(data, proto::TAILS).filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => {
                let array = v.dyn_into::<js_sys::Array>().ok()?;
                let mut tails = Vec::with_capacity(array.length() as usize);
                for tail in array.iter() {
                    // The same checked cast per tail — one copy each.
                    tails.push(tail.dyn_into::<js_sys::Uint8Array>().ok()?.to_vec());
                }
                tails
            }
        };
        Some((kind, head, tails))
    })();
    // `None` still delivers: the caller's slot is released either way.
    offload::deliver_encoded_reply(id as u64, reply);
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
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        let message = js_sys::Object::new();
        proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::JOB));
        proto::set_field(&message, proto::ID, &JsValue::from_f64(id as f64));

        // Copied out of linear memory once, then transferred: ~1.3 MB for an 8-bit
        // moment, and structured-cloning would copy it again on arrival.
        let payload = js_sys::Uint8Array::from(request.to_bytes().as_slice());
        let transfer = js_sys::Array::new();
        transfer.push(&payload.buffer());
        proto::set_field(&message, proto::REQUEST, &payload);

        match self.worker.post_message_with_transfer(&message, &transfer) {
            Ok(()) => Ok(()),
            Err(e) => {
                // The funnel runs the job here instead; `onerror` retires a dead worker.
                log::warn!("could not post job {id} to the worker: {e:?}");
                Err(request)
            }
        }
    }
}
