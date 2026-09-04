//! The rasterization worker's side of the boundary: a dedicated Web Worker
//! started by [`crate::worker_port`], running the *same wasm module* the page
//! runs, instantiated a second time. A second module would have meant a
//! second `(glue, wasm)` pair for `sw.js`'s per-client shell pinning to keep
//! atomic. Nothing here touches `window`: there is not one.
//!
//! # The tile lane
//!
//! `handle_job` runs a job to completion inside `onmessage`, so this worker's
//! message loop is a FIFO and a model rasterization of 3.9-5.0 s (the `huge`
//! leg, 2026-09-02) holds it for that long. The basemap's vector tile batches
//! must not wait there — the tile pump measured that wait and chose the frame
//! thread over it — so they ride a **lane**: one more nested Worker,
//! instantiated on THIS worker's memory exactly as wasm-bindgen-rayon
//! instantiates the pool's threads (`init({module, memory})`), with a
//! `MessagePort` of its own to the page and [`squallar_tile_lane_main`] as
//! its entry. Same heap, same statics, same codec rows; a message loop the
//! jobs here cannot occupy. It runs its jobs **serially** on its own thread,
//! through a one-thread rayon pool, so a batch never contends with a radar
//! render for the pool's threads either.

use crate::worker_protocol as proto;
use std::cell::{OnceCell, RefCell};
use wasm_bindgen::prelude::*;

/// Where the lane's bootstrap lives, relative to this worker's script.
/// Relative for the reason `worker_port::WORKER_URL` is: a project-Pages
/// subpath. Precached by `sw.js` beside `worker.js`.
const LANE_URL: &str = "./tile-lane.js";

thread_local! {
    /// The lane's `Worker` handle, rooted for the life of this worker. Firefox
    /// collects a Worker that shares this memory but is not held by a live
    /// `Worker` object (bug 1702191); wasm-bindgen-rayon roots its helpers
    /// for the same reason.
    static LANE_WORKER: RefCell<Option<web_sys::Worker>> = const { RefCell::new(None) };

    /// The lane thread's one-thread rayon pool, built on first use. See
    /// [`execute_serially`].
    static LANE_POOL: OnceCell<Option<rayon::ThreadPool>> = const { OnceCell::new() };
}

/// Boot the worker: install the message handler, start the tile lane and
/// announce readiness. Called by `worker.js` after `init()`, under a
/// distinctive name because it shares an export namespace with
/// [`crate::start`].
///
/// `heap_max_bytes` is the ceiling this worker's memory was constructed with.
/// It arrives here rather than being read because no engine will say what a
/// memory's maximum is, and it is a separate figure from the page's: the page
/// chose it (a worker global has neither `matchMedia` nor `maxTouchPoints`)
/// and handed it over on this Worker's `name`, and `worker.js` passes on what
/// it actually got — which differs from what was asked for exactly when the
/// engine refused the supplied memory and the glue built its own.
#[wasm_bindgen]
pub fn squallar_worker_main(heap_max_bytes: f64) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    // Before the hook, which prints this ceiling beside the refused request.
    crate::heap_max::declare_this(heap_max_bytes);
    // The worker is its own instance with its own heap — the MRMS grid's
    // 93 MB refusal was here — so it installs its own hook
    // (`crate::alloc_failure`).
    crate::alloc_failure::hook::install(crate::alloc_failure::Instance::RasterWorker);
    // Ignored rather than propagated: a second `init` is not a reason to
    // refuse jobs.
    let _ = console_log::init_with_level(log::Level::Info);

    let scope = worker_scope()?;
    let handler_scope = scope.clone();
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            handle_message(&handler_scope, &event.data());
        });
    scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let hello = js_sys::Object::new();
    proto::set_field(&hello, proto::KIND, &JsValue::from_str(proto::HELLO));
    proto::set_field(
        &hello,
        proto::TOKEN,
        &JsValue::from_str(&proto::build_token()),
    );
    // Asked of rayon, not of `worker.js`. `current_num_threads` reports the
    // pool that actually got built, so the fallback arm reports 1 by telling
    // the truth rather than by anyone remembering to say so.
    let threads = rayon::current_num_threads();
    proto::set_field(&hello, proto::THREADS, &JsValue::from_f64(threads as f64));
    say_memory(&hello);
    say_memory_max(&hello);

    // The lane's port rides the hello, transferred. A lane that could not be
    // started is a hello without one, which the page reads as "no lane":
    // tiles stay on its own thread, exactly as before the lane existed.
    let transfer = js_sys::Array::new();
    match spawn_tile_lane(&scope) {
        Ok(port) => {
            proto::set_field(&hello, proto::LANE, &port);
            transfer.push(&port);
        }
        Err(e) => log::warn!("no tile lane ({e:?}); vector tiles stay on the page's thread"),
    }
    scope.post_message_with_transfer(&hello, &transfer)?;

    log::info!("squallar rasterization worker ready (rayon: {threads} threads)");
    Ok(())
}

/// Start the tile lane on this worker's memory and answer the page's end of
/// its port.
///
/// The nested Worker gets one message: the module and memory to instantiate
/// on — `wasm_bindgen::module()` and `memory()`, the pair wasm-bindgen-rayon
/// hands its own helpers — and its end of a fresh `MessageChannel`. Its
/// `onerror` tells the page the lane is lost, so the batches it owed are
/// failed rather than waited for.
fn spawn_tile_lane(
    scope: &web_sys::DedicatedWorkerGlobalScope,
) -> Result<web_sys::MessagePort, JsValue> {
    let options = web_sys::WorkerOptions::new();
    options.set_type(web_sys::WorkerType::Module);
    let lane = web_sys::Worker::new_with_options(LANE_URL, &options)?;
    let channel = web_sys::MessageChannel::new()?;

    let on_error_scope = scope.clone();
    let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        let lost = js_sys::Object::new();
        proto::set_field(&lost, proto::KIND, &JsValue::from_str(proto::LANE_LOST));
        if let Err(e) = on_error_scope.post_message(&lost) {
            log::error!("the tile lane raised an error and the page could not be told: {e:?}");
        }
    });
    lane.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();

    let init = js_sys::Object::new();
    proto::set_field(&init, proto::KIND, &JsValue::from_str(proto::LANE_INIT));
    proto::set_field(&init, proto::MODULE, &wasm_bindgen::module());
    proto::set_field(&init, proto::MEMORY, &wasm_bindgen::memory());
    let lane_end = channel.port1();
    proto::set_field(&init, proto::PORT, &lane_end);
    let transfer = js_sys::Array::new();
    transfer.push(&lane_end);
    lane.post_message_with_transfer(&init, &transfer)?;

    LANE_WORKER.with(|w| *w.borrow_mut() = Some(lane));
    Ok(channel.port2())
}

/// The tile lane's entry: install the port's message handler and say hello.
/// Called by `tile-lane.js` after `init({module, memory})` on the
/// rasterization worker's memory, with the lane's end of the port.
///
/// The panic hook, the logger and the allocation-error hook are process
/// statics in that shared memory and the worker's thread has installed all
/// three; these calls are the no-ops they are documented to be on a second
/// call, kept so the lane is correct on a build where the order ever changes.
///
/// The allocation hook is here for that reason and no other. This is a third
/// wasm entry point but **not** a third heap: the lane is initialised on the
/// rasterization worker's own memory, so a refusal reaching it is a refusal
/// of the same linear memory the worker's hook already names, and
/// `set_alloc_error_hook` is a pointer store either way. What the call buys
/// is that the lane cannot be the entry that runs first with no hook set --
/// the same thing the two lines above buy, by the same argument.
#[wasm_bindgen]
pub fn squallar_tile_lane_main(port: web_sys::MessagePort) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    crate::alloc_failure::hook::install(crate::alloc_failure::Instance::TileLane);

    let handler_port = port.clone();
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            handle_lane_message(&handler_port, &event.data());
        });
    // Setting `onmessage` starts the port; no explicit `start()` is owed.
    port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let hello = js_sys::Object::new();
    proto::set_field(&hello, proto::KIND, &JsValue::from_str(proto::LANE_HELLO));
    say_memory(&hello);
    port.post_message(&hello)?;

    log::info!("squallar tile lane ready (serial, on the rasterization worker's memory)");
    Ok(())
}

/// Write this worker's own heap size onto `message` — see [`proto::MEM`].
/// Skipped rather than zeroed when the memory cannot be read: absent is
/// unknown, and 0 would read as a measurement.
fn say_memory(message: &js_sys::Object) {
    if let Some(bytes) = crate::shared_loan::memory_bytes() {
        proto::set_field(message, proto::MEM, &JsValue::from_f64(bytes as f64));
    }
}

/// The worker's own CEILING, said once on the hello rather than on every
/// reply: unlike [`say_memory`]'s reading it cannot change for the life of the
/// instance, and a `DONE` is the hot path.
fn say_memory_max(message: &js_sys::Object) {
    if let Some(bytes) = crate::heap_max::this_instance() {
        proto::set_field(message, proto::MEMMAX, &JsValue::from_f64(bytes as f64));
    }
}

fn worker_scope() -> Result<web_sys::DedicatedWorkerGlobalScope, JsValue> {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .map_err(|_| JsValue::from_str("not running inside a dedicated worker"))
}

/// Where a reply goes: this worker's global scope, or the lane's port. Both
/// post a message with a transfer list; the browser spells the method
/// differently on each.
trait Poster {
    fn post(&self, message: &JsValue, transfer: &js_sys::Array) -> Result<(), JsValue>;
}

impl Poster for web_sys::DedicatedWorkerGlobalScope {
    fn post(&self, message: &JsValue, transfer: &js_sys::Array) -> Result<(), JsValue> {
        self.post_message_with_transfer(message, transfer)
    }
}

impl Poster for web_sys::MessagePort {
    fn post(&self, message: &JsValue, transfer: &js_sys::Array) -> Result<(), JsValue> {
        self.post_message_with_transferable(message, transfer)
    }
}

/// How a job's bytes are run: the worker's arm is [`execute_encoded`] on the
/// global pool; the lane's is [`execute_serially`].
type Run = fn(&[u8], Option<&[u8]>) -> Option<(u8, Vec<u8>, Vec<Vec<u8>>)>;

/// Rasterize one job and post the answer back. A message this build cannot
/// read is answered with a failed job rather than dropped: the page holds a
/// render slot and a pane's in-flight mark against every id it posted.
fn handle_message(scope: &web_sys::DedicatedWorkerGlobalScope, data: &JsValue) {
    match proto::string_field(data, proto::KIND).as_deref() {
        Some(proto::JOB) => handle_job(scope, data, squallar_worker::offload::execute_encoded),
        // The page has finished copying a reply out of this worker's memory.
        Some(proto::RELEASE) => crate::shared_loan::release(proto::loan_field(data)),
        _ => log::warn!("worker ignoring a message that is not a job"),
    }
}

/// The lane's message loop: the same two kinds the worker answers, run
/// serially, answered on the port.
fn handle_lane_message(port: &web_sys::MessagePort, data: &JsValue) {
    match proto::string_field(data, proto::KIND).as_deref() {
        Some(proto::JOB) => handle_job(port, data, execute_serially),
        Some(proto::RELEASE) => crate::shared_loan::release(proto::loan_field(data)),
        _ => log::warn!("tile lane ignoring a message that is not a job"),
    }
}

/// [`squallar_worker::offload::execute_encoded`] on a rayon pool of exactly
/// this thread.
///
/// The lane shares the worker's global pool — one memory, one `static` — and
/// a `par_iter` submitted to it from here would queue behind whatever radar
/// render holds its threads. `use_current_thread` with one thread builds a
/// pool that spawns nothing (wasm has no `std::thread::spawn`) and runs every
/// job of an `install` on the caller, so the batch is serial and never waits
/// on the pool. A pool that cannot be built falls back to the global one,
/// said once: slower under contention, never wrong.
fn execute_serially(bytes: &[u8], payload: Option<&[u8]>) -> Option<(u8, Vec<u8>, Vec<Vec<u8>>)> {
    LANE_POOL.with(|cell| {
        let pool = cell.get_or_init(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .use_current_thread()
                .build()
                .map_err(|e| {
                    log::warn!("tile lane: no serial pool ({e}); batches share the worker's pool")
                })
                .ok()
        });
        match pool {
            Some(pool) => {
                pool.install(|| squallar_worker::offload::execute_encoded(bytes, payload))
            }
            None => squallar_worker::offload::execute_encoded(bytes, payload),
        }
    })
}

fn handle_job(poster: &dyn Poster, data: &JsValue, run: Run) {
    let Some(id) = proto::field(data, proto::ID).and_then(|v| v.as_f64()) else {
        log::error!("worker got a job with no id; nothing to answer");
        return;
    };
    let id = id as u64;

    // A checked cast, not a Uint8Array-from-view construction: that constructor
    // is the JS COPY constructor, so the old spelling paid a hidden JS→JS copy
    // of the whole request (up to ~47-69 MiB). A payload that is not a typed
    // array refuses to the empty request, answered as a failed job.
    let request = proto::field(data, proto::REQUEST)
        .and_then(|v| v.dyn_into::<js_sys::Uint8Array>().ok())
        .map(|v| v.to_vec())
        .unwrap_or_default();
    // Absent is the ordinary case: the request is whole in `REQUEST`, as every
    // build before the split wrote it. Present means the page LENT its payload
    // in place rather than copying it into the message, and `REQUEST` is the
    // head alone.
    let payload = proto::field(data, proto::REQ_PAYLOAD)
        .and_then(|v| v.dyn_into::<js_sys::Uint8Array>().ok())
        .map(|v| v.to_vec());

    // `to_vec` above IS the copy the borrower owes, and it has happened, so the
    // page may free the request now — **before** the job runs, not after.
    // A decode request is the archive, up to ~47-69 MiB; holding it for the
    // length of the rasterization would double the peak for the whole job.
    release_to_page(poster, proto::loan_field(data));

    // **What this instance is holding for jobs, while it holds it.** The
    // rasterization worker is a second module instance with a second 1 GiB
    // ceiling that no budget prices and no lever reaches, and these two
    // buffers are what it holds off the wire: the head, and — since a row
    // may nominate an already-resident payload rather than write it — the
    // whole grid behind it. The row then allocates again to cut its window,
    // so this figure is the floor of the worker's peak for the job, not the
    // peak. Counted around the call because a rayon pool can be running
    // several, and the level has to be the sum of what is actually in flight.
    let held = request.len() + payload.as_ref().map_or(0, Vec::len);
    jobs_in_flight::entered(held);
    let result = run(&request, payload.as_deref());
    jobs_in_flight::left(held);
    if result.is_none() {
        log::debug!("worker job {id} produced no frame");
    }
    if let Err(e) = post_result(poster, id, result) {
        log::error!("worker could not answer job {id}: {e:?}");
    }
}

/// **The bytes this instance's running jobs are holding**, as a level the
/// heap census can print.
///
/// Its own counter rather than a `set` on the census directly, because a
/// level is not what a caller has: several jobs run at once on the worker's
/// rayon pool, so what each one knows is its own delta. The census stays a
/// pure set of levels and this is where the arithmetic lives.
///
/// `Relaxed` is enough: every writer is adding or subtracting its own figure
/// under `fetch_add`/`fetch_sub`, which are atomic whatever the ordering, and
/// the only reader — the allocation-error hook, and the telemetry line —
/// wants a recent figure rather than a synchronised one.
mod jobs_in_flight {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static HELD: AtomicU64 = AtomicU64::new(0);

    /// A job has taken its bytes off the wire and is about to run.
    pub(super) fn entered(bytes: usize) {
        publish(HELD.fetch_add(bytes as u64, Relaxed) + bytes as u64);
    }

    /// It has finished and its buffers are about to be dropped.
    pub(super) fn left(bytes: usize) {
        publish(
            HELD.fetch_sub(bytes as u64, Relaxed)
                .saturating_sub(bytes as u64),
        );
    }

    fn publish(level: u64) {
        squallar_egui::heap_census::set_job_in_flight_bytes(level);
    }
}

/// Tell the page it may free the request it lent, if it lent one.
///
/// Best-effort by design: a failed post here costs the page one buffer held
/// until the worker is retired, which `abandon_worker` sweeps. It must not
/// abandon the job.
fn release_to_page(poster: &dyn Poster, loan: crate::shared_loan::LoanId) {
    if loan == crate::shared_loan::NO_LOAN {
        return;
    }
    let message = js_sys::Object::new();
    proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::RELEASE));
    proto::set_loan(&message, loan);
    if let Err(e) = poster.post(&message, &js_sys::Array::new()) {
        log::warn!("worker could not release the page's loan {loan}: {e:?}");
    }
}

/// Post the answer without copying it out of this instance's memory.
///
/// The reply is the `OUT`/`OUT_KIND`/`TAILS` trio: `OUT` carries the row's
/// `encode_out` HEAD and `TAILS` the row's nominated large flat buffers, as
/// per-tail `Uint8Array`s.
///
/// **Two wires, chosen by what the browser can carry** — see
/// [`crate::shared_loan`]:
///
/// * Cross-origin isolated: each array is a VIEW onto this worker's own
///   `SharedArrayBuffer` memory, posted with no transfer list. Nothing is
///   copied here at all. The `LOAN` names the buffers this worker is now
///   holding until the page sends `RELEASE`, and `post_result` must not drop
///   `result` before that — which is why the loan book takes it by value.
/// * Otherwise: the old wire. `Uint8Array::from` copies each buffer out of
///   linear memory and the transfer list moves the copy. 26.08 MiB per widest
///   2048² still frame, where the one-buffer shape paid 68.16.
///
/// The page copies once either way; what the first arm removes is THIS side's
/// copy, so the reply costs one memcpy instead of two.
///
/// The loan book is thread-local, so a reply the lane lends is the lane's to
/// release: the page answers `RELEASE` on the port the `DONE` arrived on.
///
/// `None` writes explicit nulls rather than posting nothing: the page holds a
/// render slot against every id, and silence wedges it.
fn post_result(
    poster: &dyn Poster,
    id: u64,
    result: Option<(u8, Vec<u8>, Vec<Vec<u8>>)>,
) -> Result<(), JsValue> {
    let message = js_sys::Object::new();
    proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::DONE));
    proto::set_field(&message, proto::ID, &JsValue::from_f64(id as f64));
    // Read AFTER the job ran, so the reply carries the heap the job left
    // behind — the figure the page's watermark wants.
    say_memory(&message);

    let transfer = js_sys::Array::new();
    // Written first and overwritten by the arm that has a payload, so no path
    // can leave a field absent — absent and null read the same on the page.
    proto::set_field(&message, proto::OUT, &JsValue::NULL);
    proto::set_field(&message, proto::OUT_KIND, &JsValue::NULL);
    proto::set_field(&message, proto::TAILS, &JsValue::NULL);
    proto::set_loan(&message, crate::shared_loan::NO_LOAN);

    // Tracked out here so the post's failure arm can discharge it. A loan whose
    // message never left is one the page will never send a `RELEASE` for, and
    // it would hold a whole reply -- MiB of raster -- until this worker died.
    let mut lent = crate::shared_loan::NO_LOAN;
    if let Some((kind, head, tails)) = result {
        proto::set_field(
            &message,
            proto::OUT_KIND,
            &JsValue::from_f64(f64::from(kind)),
        );
        // The head first and the tails in the row's own order, as ONE list, so
        // both wires build the same sequence and the split back into `OUT` and
        // `TAILS` below is one statement written once rather than one per arm.
        let mut buffers = Vec::with_capacity(1 + tails.len());
        buffers.push(head);
        buffers.extend(tails);

        let views = match crate::shared_loan::lend(buffers) {
            Ok((loan, views)) => {
                proto::set_loan(&message, loan);
                lent = loan;
                views
            }
            Err(buffers) => {
                // The one `transfer.push` on this wire, and it is inside the
                // loop over EVERY buffer: a head or a tail left off the
                // transfer list would be structured-cloned, which is up to
                // ~16 MiB of JS→JS copy per image tail on top of the copy
                // `Uint8Array::from` just paid.
                let copies = js_sys::Array::new();
                for buffer in &buffers {
                    let copy = js_sys::Uint8Array::from(buffer.as_slice());
                    transfer.push(&copy.buffer());
                    copies.push(&copy);
                }
                copies
            }
        };
        proto::set_field(&message, proto::OUT, &views.get(0));
        proto::set_field(&message, proto::TAILS, &views.slice(1, views.length()));
    }
    let posted = poster.post(&message, &transfer);
    if posted.is_err() {
        crate::shared_loan::release(lent);
    }
    posted
}
