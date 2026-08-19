//! The page's side of the rasterization worker.
//!
//! `rustdar_frontend::offload` owns the decision about where a CPU-bound job
//! runs, and knows nothing about the browser: the dependency runs
//! `rustdar-web` → `rustdar-frontend`, and adding `web-sys` to the frontend to
//! close the loop would put browser types in the crate desktop, Android and iOS
//! all share. So the worker is *installed* into the funnel as a
//! [`JobSink`], from here — and this file is where a `JobRequest` becomes the
//! bytes a `postMessage` transfer list can move, because that cost is the
//! browser's and belongs in the browser's adapter.
//!
//! Until the first [`attach`] succeeds, `offload_job` holds jobs for the
//! handshake window and then runs rasterization inline, which is the behaviour
//! the web build had before any of this existed. Every failure path below ends
//! in "leave it inline **and start another worker**" rather than in an error
//! the user sees — see [`lose`], which is what keeps a single mid-session
//! worker error from making that fallback permanent.

use crate::worker_protocol as proto;
use crate::worker_retry::Backoff;
use rustdar_frontend::offload::{self, JobRequest, JobSink};
use std::cell::Cell;
use wasm_bindgen::prelude::*;

/// Where the worker's bootstrap lives, relative to the page.
///
/// Relative on purpose: the site is served from a project-Pages subpath, so a
/// root-absolute URL works under a local server and 404s in production.
/// `.github/scripts/check-relative-paths.py` fails the build over it.
const WORKER_URL: &str = "./worker.js";

/// How long a job will wait for a worker that has just been started, before it
/// gives up and runs on the page's own thread.
///
/// This is the window `offload::expect_sink` is armed with, once per attempt,
/// and it is a bound on a handshake rather than on the recovery: the backoff
/// between attempts is deliberately *not* covered, because a job held across a
/// minute-long wait is a pane blank for a minute, which is worse than the stall
/// the wait exists to avoid.
///
/// Five seconds is a policy and not a measurement — there is no browser here to
/// measure — chosen against what each side costs. Too short and a cold start
/// pays the inline decode it would have paid anyway, which is exactly today's
/// behaviour and no worse; too long and a browser that will never produce a
/// worker holds the first paint. The work inside the window is a `Worker`
/// construction, a fetch of `worker.js`, a fetch and instantiation of a
/// multi-megabyte wasm module the page has itself just loaded (so it is served
/// from cache), and one `postMessage`.
const HANDSHAKE_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

thread_local! {
    /// Which worker this page is listening to.
    ///
    /// **A worker that has been replaced must not be able to speak for the
    /// page.** Every closure below captures the generation it was created
    /// under and drops any event that does not match: a terminated worker's
    /// queued `onerror` would otherwise abandon the sink its *replacement* had
    /// just installed, and a stale `HELLO` would install a port onto a `Worker`
    /// nothing is listening to — both of which turn a recovery into the
    /// permanent inline fallback this file exists to remove.
    static GENERATION: Cell<u64> = const { Cell::new(0) };

    /// The ladder the next respawn waits. See [`crate::worker_retry`].
    static BACKOFF: Cell<Backoff> = const { Cell::new(Backoff::new()) };

    /// Whether a respawn is already on a timer, so that a `FATAL` and an
    /// `onerror` from the same dying worker schedule one attempt rather than
    /// two — and so that two attempts can never run at once, which would leave
    /// the loser's `Worker` alive with nobody holding it.
    static RESPAWN_SCHEDULED: Cell<bool> = const { Cell::new(false) };
}

/// Start the rasterization worker and, once it identifies itself as this same
/// build, route [`offload::offload_job`] through it — and start another
/// whenever the one in hand is lost.
///
/// Returns immediately. The worker announces itself asynchronously, so
/// rasterization waits [`HANDSHAKE_WINDOW`] for the first frames and moves off
/// the main thread once the handshake lands.
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

    // Armed **before** the worker exists, because a job dispatched in this same
    // turn of the event loop would otherwise find no sink and no wait and pay
    // the whole decode on this thread. `expect_sink` arms nothing on a thread
    // that already has a sink, so a spawn racing a live worker is a no-op here.
    offload::expect_sink(HANDSHAKE_WINDOW);
    // And a timer for the deadline, because `offload_job` notices a lapsed wait
    // only when another job arrives — and the case this covers is the one where
    // no other job is coming, a first paint waiting on the volume decode it
    // queued. Scheduled per attempt; a later arming makes the earlier timer a
    // no-op rather than a premature flush.
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

    // A worker that dies mid-job owes replies that will never arrive, and every
    // one of those is holding a render slot and a pane's in-flight mark.
    // `abandon_worker` fails them, which releases both and lets the next frame
    // re-dispatch; `lose` then starts a replacement, so "the next frame" is not
    // condemned to run inline for the rest of the session.
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
/// The two halves are separate on purpose. [`offload::abandon_worker`] fails
/// the jobs *this* worker owed — they were in flight when it died and their
/// answers are not coming, so their panes re-ask — and [`schedule_respawn`]
/// makes sure the job after them has somewhere to go.
///
/// A stale generation returns without doing either. See [`GENERATION`].
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
/// Idempotent while a respawn is outstanding: a dying worker commonly produces
/// both a `FATAL` and an `onerror`, and two attempts in flight at once would
/// leave whichever lost the race running with nobody holding its handle.
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
/// # A callback that cannot be scheduled is dropped, not run here
///
/// Running it now instead looks like the safer fallback and is the opposite of
/// one. `spawn` reaches this through [`schedule_respawn`], so a `then` invoked
/// on the spot re-enters `spawn` — and a browser whose `Worker::new` throws
/// synchronously would go `spawn` → `lose` → `schedule_respawn` → here →
/// `spawn` until the stack ran out, on the page's own thread. The delay is not
/// decoration on this ladder; it is what makes it a ladder.
///
/// Nothing is lost by dropping it, because this module only ever runs on the
/// page (`worker.rs` is the worker's half), and a page has a `window`. Both
/// branches below are therefore states a browser does not reach, logged so that
/// one which somehow does says so rather than looking like a worker that never
/// answered.
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
    // A message from a worker this page has replaced says nothing about the one
    // it is listening to. Dropped before `DONE` as well as before the lifecycle
    // kinds: the reply belongs to a job `abandon_worker` already failed, and
    // `deliver_job_reply` would find no pending entry for it anyway.
    if GENERATION.with(Cell::get) != generation {
        log::debug!("ignoring a message from a worker this page has already replaced");
        return;
    }
    match proto::string_field(data, proto::KIND).as_deref() {
        Some(proto::HELLO) => {
            let theirs = proto::string_field(data, proto::TOKEN).unwrap_or_default();
            let ours = proto::build_token();
            if theirs != ours {
                // Not this build. See `worker_protocol::build_token` for how
                // that happens: the worker is its own service-worker client and
                // can be served a different shell generation than the page.
                //
                // Respawned rather than abandoned for good, and that is the
                // intended reading of the token check: it refuses a *pair*, not
                // a browser. The mismatch is a page and a worker served
                // different shell generations, so the next worker is fetched
                // against a service worker that has by then usually settled —
                // and a mismatch that does not settle costs one construction
                // per backoff rung and nothing else.
                log::warn!(
                    "rasterization worker is a different build ({theirs} vs {ours}); \
                     rendering on the main thread"
                );
                worker.terminate();
                lose(generation, "build token mismatch");
                return;
            }
            log::info!("rasterization worker attached ({ours})");
            // The ladder resets **here**, on a worker that has proved itself,
            // and not on one that merely constructed. See `Backoff::reset`.
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
/// The reply is the `OUT`/`OUT_KIND`/`TAILS` trio — the head, the registry
/// code naming the row that wrote it, and the row's nominated large
/// buffers, each transferred (WO-M7d) — or explicit nulls for a job that
/// produced nothing. Reading each buffer below is ONE copy into this
/// instance's linear memory (`to_vec`), unavoidable without a
/// `SharedArrayBuffer`, which needs COOP/COEP headers GitHub Pages does
/// not let this deployment set. It used to be two: the old spelling — a
/// Uint8Array-from-view construction over the arriving payload — is the JS
/// `new Uint8Array(typedArray)` COPY constructor, a hidden whole-payload
/// JS→JS copy AHEAD of the crossing, ~21 MiB per widest still frame, which
/// the prose that stood here then under-claimed as "the first copy... no
/// way around that one". The checked `dyn_into` cast constructs nothing; a
/// value that is not a typed array refuses to `None`, the posture every
/// malformed reply already has.
///
/// Everything after the copies is `offload::deliver_encoded_reply`'s: it
/// holds the codec row recorded when the job was dispatched, verifies the
/// reply's kind against that row's code (a mismatch is another build's
/// reply or a corrupt message, delivered as "nothing to draw"), and decodes
/// through the row — which also judges the tail COUNT and ADOPTS the
/// frame's image tail whole, the page-side copy WO-M7d killed — so this
/// crate stays the browser adapter and the payload codecs are reachable
/// from a host test rather than only from a browser.
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
        // TAILS null or absent reads as no tails: a frame row then refuses
        // at its tail count — correctly — and every tail-less row decodes
        // exactly as it always did.
        let tails = match proto::field(data, proto::TAILS).filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => {
                let array = v.dyn_into::<js_sys::Array>().ok()?;
                let mut tails = Vec::with_capacity(array.length() as usize);
                for tail in array.iter() {
                    // The same checked cast per tail — one copy each, no
                    // constructor.
                    tails.push(tail.dyn_into::<js_sys::Uint8Array>().ok()?.to_vec());
                }
                tails
            }
        };
        Some((kind, head, tails))
    })();
    // `None` — a missing payload, a missing kind, a job that produced
    // nothing — still delivers: the caller's slot is released either way.
    offload::deliver_encoded_reply(id as u64, reply);
}

/// The installed port. Owns the `Worker` handle, so the worker lives exactly as
/// long as the funnel is willing to send it jobs.
struct Port {
    worker: web_sys::Worker,
}

impl JobSink for Port {
    /// # The serialisation lives here, and nowhere above here
    ///
    /// A `JobRequest` is not a thing a `Worker` can be handed: the only payload
    /// a `postMessage` transfer list moves is a detachable `ArrayBuffer`, so
    /// this arm turns the request into bytes on its way out. That is the
    /// browser's charge for handover and it is charged where it is incurred —
    /// the funnel calls `send(id, request)` and names no representation, so a
    /// transport that can move an owned value pays none of this.
    ///
    /// `to_bytes` borrows, so a failed post still owns the request and hands it
    /// back for the funnel to run inline.
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        let message = js_sys::Object::new();
        proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::JOB));
        proto::set_field(&message, proto::ID, &JsValue::from_f64(id as f64));

        // Copied out of linear memory once, then transferred: the request is
        // one radar sweep, ~1.3 MB for an 8-bit moment and more for NROT, and
        // structured-cloning it would copy it a second time on arrival.
        let payload = js_sys::Uint8Array::from(request.to_bytes().as_slice());
        let transfer = js_sys::Array::new();
        transfer.push(&payload.buffer());
        proto::set_field(&message, proto::REQUEST, &payload);

        match self.worker.post_message_with_transfer(&message, &transfer) {
            Ok(()) => Ok(()),
            Err(e) => {
                // The funnel runs the job here instead. A sink that keeps
                // refusing is a worker that has died, and `onerror` retires it.
                log::warn!("could not post job {id} to the worker: {e:?}");
                Err(request)
            }
        }
    }
}
