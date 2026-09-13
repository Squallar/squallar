//! The page's side of the rasterization worker.
//!
//! `squallar_worker::offload` owns where a CPU-bound job runs and knows nothing
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
use squallar_worker::offload::{self, JobRequest, JobSink};
use std::cell::Cell;
use wasm_bindgen::prelude::*;

/// Where the worker's bootstrap lives, relative to the page.
///
/// Relative on purpose: the site is served from a project-Pages subpath.
const WORKER_URL: &str = "./worker.js";

/// The query parameter that carries this page's shell-generation key on
/// [`WORKER_URL`] and, from there, on everything the worker tree imports.
/// Held equal to `sw.js`'s `SHELL_PIN_PARAM` by `tests/pwa_assets.rs`.
///
/// **Why a key rides in the URL at all.** The service worker pins each page to
/// one shell generation by its client id, and a deploy landing during boot
/// used to give the worker this page started a *different* generation than
/// the page: its script fetch carries the page's id, but its glue and wasm
/// arrive under the worker's own, never-pinned id, and the rayon threads'
/// glue imports arrive with no client id at all in Chromium. A key the page
/// mints, spelled on the worker's URL and propagated by `worker.js` onto its
/// imports, is the one identity every request in that tree can carry;
/// `sw.js` records the generation it resolved for the key at first sight and
/// answers every later request carrying it from the same one. See the
/// `MIXED SHELLS` header of `sw.js`.
const SHELL_PIN_PARAM: &str = "pin";

/// The prefix of the `name` a rasterization worker is started under, the rest
/// of which is its linear-memory ceiling in bytes. Held equal to `heap.js`'s
/// own `WORKER_NAME_PREFIX` by `tests/linear_memory_ceiling.rs`.
const WORKER_NAME_PREFIX: &str = "squallar-raster:";

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

    /// This page's shell-generation key, minted once: a respawn must land on
    /// the generation the page runs, and the service worker keeps that
    /// answer by key (see [`SHELL_PIN_PARAM`]). Opaque and random; it only
    /// has to differ between the tabs of one origin.
    static SHELL_PIN_KEY: String = fresh_shell_pin_key();
}

/// Two `Math.random()` draws as hex: 104 bits, which is uniqueness enough for
/// "the tabs open on one origin" without a `Crypto` feature for the one call.
fn fresh_shell_pin_key() -> String {
    const DRAW: f64 = (1u64 << 52) as f64;
    let draw = || (js_sys::Math::random() * DRAW) as u64;
    format!("{:013x}{:013x}", draw(), draw())
}

/// The URL the worker is started at: [`WORKER_URL`] carrying this page's key.
fn keyed_worker_url() -> String {
    SHELL_PIN_KEY.with(|key| format!("{WORKER_URL}?{SHELL_PIN_PARAM}={key}"))
}

/// Take the worker's heap reading — and its live bytes, where the message
/// carries them — off a message that carries one, and hand both to
/// [`crate::worker_heap`], which holds the figures and the rule about when
/// one stops being current. A field the message does not carry arrives there
/// as `None` and leaves that figure alone.
fn note_worker_memory(data: &JsValue) {
    let reading = |key| proto::field(data, key).and_then(|v| v.as_f64());
    crate::worker_heap::note(reading(proto::MEM), reading(proto::LIVE));
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
    // **How the worker learns where its linear-memory ladder starts.**
    // `worker.js` constructs its `WebAssembly.Memory` before it can receive a
    // message, so the page's rung travels on `name`: the one channel that is
    // synchronous, present at the top of the worker's own script, and costs
    // no URL. A query string would work too (`sw.js` matches the shell with
    // `ignoreSearch`) at the price of a second spelling of a precached asset.
    // A respawn re-reads the cell — by then the last worker's REPORTED rung —
    // so a worker that comes back starts no higher than its predecessor got.
    if let Some(bytes) = crate::heap_max::worker_ladder_start() {
        options.set_name(&format!("{WORKER_NAME_PREFIX}{bytes}"));
    }

    let worker = match web_sys::Worker::new_with_options(&keyed_worker_url(), &options) {
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
    // The tile lane lives in that worker's memory and dies with it.
    offload::abandon_lane(reason);
    // The requests this page lent that worker are owed `RELEASE`s it will never
    // send. Nothing is reading them any more — a replaced worker's messages are
    // dropped by generation before they reach a handler — so the sweep is the
    // whole of the cleanup.
    crate::shared_loan::release_all(reason);
    // **And that worker's heap figures stop being current.** They stand —
    // the readout's rule is that the last figure said is what a reader wants
    // — but rasterization falls back to the page's own thread here and there
    // is no second instance behind them until a respawn says hello, so
    // nothing may take them as a bound (`crate::worker_heap`).
    crate::worker_heap::lost();
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
            // After the token check: a worker of another build is not the one
            // whose heap this page reports.
            note_worker_memory(data);
            // And its RESERVATION, which only the worker knows: the page
            // handed it a starting rung, and its own ladder may have stopped
            // lower, or taken the glue's fallback (`heap.js`, `initWithHeap`).
            // The page's policy for the worker is held under it
            // (`heap_max::worker_policy`).
            if let Some(bytes) = proto::field(data, proto::MEMMAX)
                .and_then(|v| v.as_f64())
                .filter(|v| v.is_finite() && *v > 0.0)
            {
                crate::heap_max::note_worker_reported(bytes as u64);
            }
            // The ladder resets **here**, on a worker that has proved itself.
            BACKOFF.with(|backoff| {
                let mut ladder = backoff.get();
                ladder.reset();
                backoff.set(ladder);
            });
            offload::set_worker(Box::new(Port {
                worker: worker.clone(),
            }));
            // The tile lane's port rides the same hello. A hello without one
            // — a build before the lane, or a spawn that failed — leaves the
            // tile pump on its own thread, which is what it does anyway
            // until the lane says hello.
            if let Some(port) = proto::field(data, proto::LANE)
                .and_then(|v| v.dyn_into::<web_sys::MessagePort>().ok())
            {
                listen_to_lane(generation, port);
            }
        }
        // The worker's nested lane raised an error: fail the batches it owed
        // and go back to styling on the page. The worker itself is fine.
        Some(proto::LANE_LOST) => offload::abandon_lane("the tile lane raised an error"),
        Some(proto::FATAL) => {
            let error = proto::string_field(data, proto::ERROR).unwrap_or_default();
            log::warn!(
                "rasterization worker failed to start ({error}); rendering on the main thread"
            );
            worker.terminate();
            lose(generation, "the worker failed to start");
        }
        Some(proto::DONE) => {
            note_worker_memory(data);
            deliver(worker, data)
        }
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
/// `&[u8]` of THIS instance's memory, which a view onto a foreign buffer
/// cannot be at any price.
///
/// **A raster reply then pays a second full-size buffer, and that is a
/// property of the types AND of where the wire puts the picture — not of the
/// types alone, as this sentence used to say.** `egui::ColorImage` holds
/// `Vec<Color32>`, a `Vec` is freed with the `Layout` it was taken with, and
/// so a `Vec<u8>` can never become one; the decode allocates the pixels while
/// the bytes it is reading are still live, because
/// `offload::deliver_encoded_reply` binds the head and passes it by borrow.
/// Both buffers are therefore live at the same instant. What would remove the
/// first one is not a cleverer cast here but the picture riding its own
/// buffer at offset zero, which is a change to the reply codec's wire — the
/// alignment reason it cannot be done from this side, and what moving it
/// would cost, are recorded on
/// `squallar_overlays::render::jobs::encode_overlay_out`.
///
/// Both spans are priced, cumulatively and at their worst, by [`account_reply`]
/// — see [`Traffic::copy_ns`]. They are main-thread time between two frames and
/// are in no `frame service` figure, which measures the redraw.
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

    // Whether this job's reply is a whole picture, asked BEFORE anything is
    // copied because the answer decides how it is copied. See [`Pull::split`].
    let raster = offload::reply_is_raster(id as u64);
    // Read here for the same reason `raster` is: delivering removes the job
    // from the registry, so the row cannot be named afterwards.
    let row = offload::reply_row_label(id as u64).unwrap_or("");
    let mut pull = Pull::default();

    let reply = (|| {
        // Undefined as well as null: `post_result` writes an explicit null on
        // every path, so undefined can only mean a worker that built its views
        // and found none — nothing to draw either way.
        let out = proto::field(data, proto::OUT).filter(|v| !v.is_null() && !v.is_undefined())?;
        let kind = proto::field(data, proto::OUT_KIND)
            .and_then(|v| v.as_f64())
            .map(|v| v as u8)?;
        let out = out.dyn_into::<js_sys::Uint8Array>().ok()?;
        // A raster row's picture is lifted straight into the element type its
        // consumer keeps; every other row's head is copied whole, exactly as
        // before. `None` from `split` is "this head has no liftable span" — a
        // blank, or a prefix that does not describe one — and falls back rather
        // than failing, because a head the transport cannot split is still a
        // head the row's own decoder reads.
        let split = if raster { pull.split(&out) } else { None };
        let (head, picture) = match split {
            Some((head, picture)) => (head, Some(picture)),
            None => (pull.whole(&out), None),
        };
        // TAILS null or absent reads as no tails.
        let tails = match proto::field(data, proto::TAILS).filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => {
                let array = v.dyn_into::<js_sys::Array>().ok()?;
                let mut tails = Vec::with_capacity(array.length() as usize);
                for tail in array.iter() {
                    // The same checked cast per tail — one copy each.
                    let tail = tail.dyn_into::<js_sys::Uint8Array>().ok()?;
                    tails.push(pull.whole(&tail));
                }
                tails
            }
        };
        Some(match picture {
            Some(picture) => Reply::Split(kind, head, picture, tails),
            None => Reply::Whole(kind, head, tails),
        })
    })();

    // **Before** `deliver_encoded_reply`, which runs the caller's delivery and
    // can be milliseconds of `ColorImage` building: every view above has been
    // copied out by now, and the worker is holding multiple MiB until it hears
    // so. Releasing after the delivery would hold them across it for no reason.
    release_to_worker(worker, loan);

    // `None` still delivers: the caller's slot is released either way.
    //
    // Clocked here rather than inside the funnel because THIS thread is the
    // browser's main thread and the funnel's is not: `deliver_encoded_reply`
    // runs the row's decode and the caller's delivery inline, so on this
    // target the whole of it is page-main-thread time between two frames.
    let deliver_start = web_time::Instant::now();
    match reply {
        // `into_wire` is the move that lets the funnel carry a picture without
        // naming a colour type: same size, same alignment, so `bytemuck`
        // re-labels the allocation rather than copying it.
        Some(Reply::Split(kind, head, picture, tails)) => {
            offload::deliver_encoded_reply_split(id as u64, kind, head, picture.into_wire(), tails)
        }
        Some(Reply::Whole(kind, head, tails)) => {
            offload::deliver_encoded_reply(id as u64, Some((kind, head, tails)))
        }
        None => offload::deliver_encoded_reply(id as u64, None),
    }
    account_reply(
        pull.moved,
        pull.copied_at_worker,
        pull.copy_ns,
        ns(deliver_start, web_time::Instant::now()),
        row,
    );
}

/// What [`deliver`] pulled out of one `DONE` message.
enum Reply {
    /// The head whole — every row whose reply is not a picture, and a picture
    /// reply whose head carries no liftable span.
    Whole(u8, Vec<u8>, Vec<Vec<u8>>),
    /// The head with the picture's span removed, and the picture already in the
    /// element type an `egui::ColorImage` holds.
    Split(
        u8,
        Vec<u8>,
        squallar_overlays::render::raster_buf::RasterBuf,
        Vec<Vec<u8>>,
    ),
}

/// The counters [`deliver`] fills while it pulls a reply out of the worker's
/// memory, and the two ways it can pull one.
///
/// A struct rather than closures over three locals, because both spellings need
/// the same three counters and only one closure may borrow them at a time.
/// Counting and copying stay ONE step either way, so a buffer cannot be counted
/// into `moved` and copied outside the clock.
#[derive(Default)]
struct Pull {
    moved: usize,
    copied_at_worker: usize,
    /// **Nanoseconds, not the microseconds the send direction accumulates in.**
    /// A reply is one head and zero or more tails, and a tail can be a few
    /// hundred bytes; a per-call truncation to whole microseconds would report
    /// zero for a hundred of those and understate the total by the whole of the
    /// small end.
    copy_ns: u64,
}

impl Pull {
    fn count(&mut self, array: &js_sys::Uint8Array) {
        let len = array.length() as usize;
        self.moved += len;
        if !crate::shared_loan::is_foreign_shared(array) {
            self.copied_at_worker += len;
        }
    }

    /// One buffer, copied out whole into this instance's memory.
    fn whole(&mut self, array: &js_sys::Uint8Array) -> Vec<u8> {
        self.count(array);
        note_block(array.length() as usize);
        let start = web_time::Instant::now();
        let bytes = array.to_vec();
        self.copy_ns += ns(start, web_time::Instant::now());
        bytes
    }

    /// **One picture, copied out ONCE, into the element type it will be drawn
    /// from.**
    ///
    /// The head of a raster reply states its picture's span in a fixed prefix
    /// (`squallar_overlays::render::jobs::overlay_pixel_span`), so those bytes
    /// can be copied straight into a `Vec<Color32>` instead of into a `Vec<u8>`
    /// that the decode would then have to walk into a second buffer the
    /// picture's own size. Both buffers were live at once — measured at 75.4 MiB
    /// for one picture on a 2878x1566 canvas — and this is the half that goes.
    ///
    /// A copy into a 4-aligned destination is sound; it is the VIEW that
    /// alignment forbids, which is why the picture has to be born as pixels
    /// here rather than cast afterwards. `RasterBuf::as_mut_bytes` is that
    /// destination and needs no `unsafe`, which this crate forbids outright.
    ///
    /// `None` where the prefix describes no span — a blank, a short head, or a
    /// length that is not whole pixels — and the caller copies the head whole.
    fn split(
        &mut self,
        array: &js_sys::Uint8Array,
    ) -> Option<(Vec<u8>, squallar_overlays::render::raster_buf::RasterBuf)> {
        use squallar_overlays::render::jobs::{OVERLAY_PIXEL_PREFIX_BYTES, overlay_pixel_span};
        use squallar_overlays::render::raster_buf::RasterBuf;

        let total = array.length() as usize;
        let mut prefix = [0u8; OVERLAY_PIXEL_PREFIX_BYTES];
        let read = OVERLAY_PIXEL_PREFIX_BYTES.min(total);
        array.subarray(0, read as u32).copy_to(&mut prefix[..read]);
        let (offset, len) = overlay_pixel_span(&prefix[..read])?;
        // A span the head cannot contain is a corrupt or foreign message, not a
        // picture to copy the tail of.
        if offset.checked_add(len)? > total {
            return None;
        }

        self.count(array);
        note_block(len);
        let start = web_time::Instant::now();
        let mut picture = RasterBuf::transparent(len / 4);
        array
            .subarray(offset as u32, (offset + len) as u32)
            .copy_to(picture.as_mut_bytes());
        // The head with the span taken out: its prefix, then whatever followed
        // the picture. One allocation, and small — the cells block and five
        // bytes.
        let mut head = vec![0u8; total - len];
        array
            .subarray(0, offset as u32)
            .copy_to(&mut head[..offset]);
        array
            .subarray((offset + len) as u32, total as u32)
            .copy_to(&mut head[offset..]);
        self.copy_ns += ns(start, web_time::Instant::now());
        note_block(head.len());
        Some((head, picture))
    }
}

// ── The tile lane ────────────────────────────────────────────────────────────
//
// A second channel beside the worker's, deliberately not the worker's: its
// bytes are not in the `transport:` ledger above, whose denominator is the
// funnel's jobs, and its replies are decoded by the same `deliver_encoded_reply`
// through the row recorded at dispatch. What crosses it is small — a batch's
// MVT bodies out (2.4 KB median), styled shapes back (~10 KB typical, 652 KB
// at the measured tail) — and its running figure is the tile pump's own
// `tile bodies: N offloaded` line, a count that cannot be evicted from the
// console ring.

/// Start listening on the lane's port. The lane is installed into the funnel
/// only when it says hello: a port whose lane never came up installs nothing,
/// and the pump keeps styling on this thread.
fn listen_to_lane(generation: u64, port: web_sys::MessagePort) {
    let handler_port = port.clone();
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            handle_lane_message(generation, &handler_port, &event.data());
        });
    // Setting `onmessage` starts the port.
    port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();
}

fn handle_lane_message(generation: u64, port: &web_sys::MessagePort, data: &JsValue) {
    if GENERATION.with(Cell::get) != generation {
        log::debug!("ignoring a message from a tile lane this page has already replaced");
        return;
    }
    match proto::string_field(data, proto::KIND).as_deref() {
        Some(proto::LANE_HELLO) => {
            // One memory, two threads: the lane's heap reading IS the worker's.
            note_worker_memory(data);
            offload::set_lane(Box::new(LanePort { port: port.clone() }));
            log::info!("tile lane attached; vector tile batches leave the frame thread");
        }
        Some(proto::DONE) => {
            note_worker_memory(data);
            deliver_from_lane(port, data);
        }
        // The lane has finished copying a request out of this page's memory.
        Some(proto::RELEASE) => crate::shared_loan::release(proto::loan_field(data)),
        Some(proto::FATAL) => {
            let error = proto::string_field(data, proto::ERROR).unwrap_or_default();
            log::warn!("tile lane failed to start ({error}); vector tiles stay on this thread");
            offload::abandon_lane("the tile lane failed to start");
        }
        other => log::warn!("ignoring a tile lane message of kind {other:?}"),
    }
}

/// Hand a lane's `done` to the batch that asked for it: the same trio, the
/// same one copy into this page's memory, the same `RELEASE` before the
/// delivery — on the lane's port, because the loan book is per thread and the
/// lane's thread is the lender.
fn deliver_from_lane(port: &web_sys::MessagePort, data: &JsValue) {
    let Some(id) = proto::field(data, proto::ID).and_then(|v| v.as_f64()) else {
        log::error!("the tile lane answered with no job id");
        return;
    };
    let loan = proto::loan_field(data);
    let reply = (|| {
        let out = proto::field(data, proto::OUT).filter(|v| !v.is_null() && !v.is_undefined())?;
        let kind = proto::field(data, proto::OUT_KIND)
            .and_then(|v| v.as_f64())
            .map(|v| v as u8)?;
        let head = out.dyn_into::<js_sys::Uint8Array>().ok()?.to_vec();
        let tails = match proto::field(data, proto::TAILS).filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => {
                let array = v.dyn_into::<js_sys::Array>().ok()?;
                let mut tails = Vec::with_capacity(array.length() as usize);
                for tail in array.iter() {
                    tails.push(tail.dyn_into::<js_sys::Uint8Array>().ok()?.to_vec());
                }
                tails
            }
        };
        Some((kind, head, tails))
    })();
    if loan != crate::shared_loan::NO_LOAN {
        let message = js_sys::Object::new();
        proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::RELEASE));
        proto::set_loan(&message, loan);
        if let Err(e) = port.post_message(&message) {
            log::warn!("could not release the tile lane's loan {loan}: {e:?}");
        }
    }
    offload::deliver_encoded_reply(id as u64, reply);
}

/// The installed lane. Owns the page's end of the port.
struct LanePort {
    port: web_sys::MessagePort,
}

impl JobSink for LanePort {
    /// The request wire `Port::send` speaks, on the lane's port: `to_bytes`,
    /// lent as a view when the page can lend and copied-and-transferred when
    /// it cannot. Not routed through `Port::send` because that is the funnel's
    /// transport and its ledger; this channel keeps its own count.
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        let message = js_sys::Object::new();
        proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::JOB));
        proto::set_field(&message, proto::ID, &JsValue::from_f64(id as f64));
        proto::set_loan(&message, crate::shared_loan::NO_LOAN);

        let bytes = request.to_bytes();
        let transfer = js_sys::Array::new();
        let loan = match crate::shared_loan::lend(vec![bytes]) {
            Ok((loan, views)) => {
                proto::set_loan(&message, loan);
                proto::set_field(&message, proto::REQUEST, &views.get(0));
                loan
            }
            Err(mut bytes) => {
                let bytes = bytes.pop().unwrap_or_default();
                let payload = js_sys::Uint8Array::from(bytes.as_slice());
                transfer.push(&payload.buffer());
                proto::set_field(&message, proto::REQUEST, &payload);
                crate::shared_loan::NO_LOAN
            }
        };
        match self
            .port
            .post_message_with_transferable(&message, &transfer)
        {
            Ok(()) => Ok(()),
            Err(e) => {
                log::warn!("could not post batch {id} to the tile lane: {e:?}");
                crate::shared_loan::release(loan);
                Err(request)
            }
        }
    }
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
    /// Whole microseconds this page has spent in `JobRequest::to_bytes`, on the
    /// FRAME THREAD, encoding requests for the worker. Cumulative.
    ///
    /// Split from [`Self::post_us`] because the two are different problems with
    /// different fixes and the cut that contains them was measured holding
    /// 73-96% of the web overlay dispatch: encoding is this page's own CPU and
    /// answers to a smaller or lazier wire format, while posting is the
    /// browser's and answers to sending less or sending it elsewhere. A single
    /// figure over the pair names neither.
    encode_us: u64,
    /// Whole microseconds spent in `postMessage` itself, cumulative. See
    /// [`Self::encode_us`].
    post_us: u64,
    /// Whole NANOSECONDS this page has spent in `Uint8Array::to_vec` pulling a
    /// reply's head and tails out of the worker's view and into this
    /// instance's linear memory, on the browser's MAIN THREAD. Cumulative,
    /// over [`Self::replies`].
    ///
    /// Nanoseconds because the denominator counts small buffers as well as
    /// pictures; the line reports microseconds.
    copy_ns: u64,
    /// The single worst reply's [`Self::copy_ns`]. The cumulative figure
    /// answers "what share of the thread", this one answers "how long was the
    /// thread gone", and a p99 question needs the second.
    worst_copy_ns: u64,
    /// **Whose reply that worst copy was** — the job row's label, and the
    /// bytes it moved. See [`Self::worst_deliver_row`], which exists for the
    /// same reason.
    worst_copy_row: &'static str,
    worst_copy_bytes: u64,
    /// Whole nanoseconds in `offload::deliver_encoded_reply` — the row's
    /// decode and the caller's delivery, both of which run inline on this
    /// thread — cumulative over the same denominator. **Never added to
    /// [`Self::copy_ns`]'s bytes and never subtracted from it either**: they
    /// are two disjoint spans of one message's handling, and their sum is what
    /// the reply cost the main thread.
    deliver_ns: u64,
    /// The single worst reply's [`Self::deliver_ns`].
    worst_deliver_ns: u64,
    /// **Whose reply that worst delivery was** — the job row's label, and the
    /// bytes it moved.
    ///
    /// Without these the maximum is unattributable, because the denominator
    /// `replies` counts EVERY row the worker answers: a whole-picture overlay
    /// raster and a radar volume decode land in the same figure, and reading
    /// the worst one as a picture is an inference the ledger cannot support.
    /// The label is read from the registry BEFORE the delivery, since
    /// delivering removes the job — see `offload::reply_row_label`.
    worst_deliver_row: &'static str,
    worst_deliver_bytes: u64,
    /// Reply buffers of at least [`LARGE_BLOCK_BYTES`], counted per BUFFER
    /// rather than per reply: `to_vec` makes one allocation per head and per
    /// tail, and one allocation is what the allocator sees.
    blocks_large: u64,
    /// Reply buffers under that size, which reuse freely and are counted only
    /// so the large figure has a denominator beside it.
    blocks_small: u64,
    /// Of [`Self::blocks_large`], how many arrived at an exact size the table
    /// had no free slot left for. **Not folded into a neighbouring size** —
    /// see [`BlockTable`].
    blocks_unlisted: u64,
}

impl Traffic {
    const ZERO: Self = Self {
        replies: 0,
        out_moved: 0,
        out_copied: 0,
        in_moved: 0,
        in_copied: 0,
        encode_us: 0,
        post_us: 0,
        copy_ns: 0,
        worst_copy_ns: 0,
        worst_copy_row: "",
        worst_copy_bytes: 0,
        deliver_ns: 0,
        worst_deliver_ns: 0,
        worst_deliver_row: "",
        worst_deliver_bytes: 0,
        blocks_large: 0,
        blocks_small: 0,
        blocks_unlisted: 0,
    };
}

/// Whole microseconds from `a` to `b`, saturating.
fn us(a: web_time::Instant, b: web_time::Instant) -> u64 {
    b.duration_since(a).as_micros().min(u128::from(u64::MAX)) as u64
}

/// Whole nanoseconds from `a` to `b`, saturating. What the reply direction
/// accumulates in; see [`Traffic::copy_ns`].
fn ns(a: web_time::Instant, b: web_time::Instant) -> u64 {
    b.duration_since(a).as_nanos().min(u128::from(u64::MAX)) as u64
}

/// Fold one message into the ledger and log the running totals.
///
/// The line is the Tier-2 instrument (`drive.py --expect-zero-copy-replies`)
/// and the measurement at once, which is deliberate: a gate that reads a
/// different number than the report would let the two drift. It is worded so
/// that a transport which quietly reverted to copying cannot satisfy it —
/// `out_copied` would climb — and so that a transport that moved NOTHING
/// cannot satisfy it either, because the assertion also requires `out_moved`
/// to be positive.
///
/// The two directions fold through [`account_sent`] and [`account_reply`]
/// rather than through one call with the other direction's figures written as
/// zeros: a zero passed positionally is indistinguishable from a measurement
/// that came out zero.
fn account(delta: impl FnOnce(&mut Traffic)) {
    let totals = TRAFFIC.with(|traffic| {
        let mut totals = traffic.get();
        delta(&mut totals);
        traffic.set(totals);
        totals
    });
    log::info!(
        "transport: {} replies, {} B out with {} B copied out of the worker, \
         {} B in with {} B copied out of this page, {} us encoding, {} us posting, \
         {} us copying replies in, {} us worst reply copy, {} us delivering replies, \
         {} us worst delivery",
        totals.replies,
        totals.out_moved,
        totals.out_copied,
        totals.in_moved,
        totals.in_copied,
        totals.encode_us,
        totals.post_us,
        totals.copy_ns / 1_000,
        totals.worst_copy_ns / 1_000,
        totals.deliver_ns / 1_000,
        totals.worst_deliver_ns / 1_000,
    );
}

/// One request this page handed the worker.
fn account_sent(in_moved: usize, in_copied: usize, encode_us: u64, post_us: u64) {
    account(|totals| {
        totals.in_moved += in_moved as u64;
        totals.in_copied += in_copied as u64;
        totals.encode_us += encode_us;
        totals.post_us += post_us;
    });
}

/// One reply this page copied out of the worker and delivered.
///
/// A reply that carried no buffer at all moves NOTHING here — not the count and
/// not the two clocks — so every figure in the reply direction stands over the
/// one denominator `replies` names. Its delivery is a `None` handed to a
/// caller's channel and is not what these clocks are asked about.
fn account_reply(
    out_moved: usize,
    out_copied: usize,
    copy_ns: u64,
    deliver_ns: u64,
    row: &'static str,
) {
    if out_moved == 0 && out_copied == 0 {
        return;
    }
    account(|totals| {
        totals.replies += 1;
        totals.out_moved += out_moved as u64;
        totals.out_copied += out_copied as u64;
        totals.copy_ns += copy_ns;
        // The label and the bytes move WITH the maximum, in the same branch
        // that raises it, so a reported row can only ever be the row of the
        // figure beside it.
        if copy_ns > totals.worst_copy_ns {
            totals.worst_copy_ns = copy_ns;
            totals.worst_copy_row = row;
            totals.worst_copy_bytes = out_moved as u64;
        }
        totals.deliver_ns += deliver_ns;
        if deliver_ns > totals.worst_deliver_ns {
            totals.worst_deliver_ns = deliver_ns;
            totals.worst_deliver_row = row;
            totals.worst_deliver_bytes = out_moved as u64;
        }
    });
    let totals = TRAFFIC.with(Cell::get);
    log_blocks(&totals);
    log_worst(&totals);
}

/// **Who the two maxima on the `transport:` line belong to.**
///
/// A separate line for the reason `log_blocks` is one: that sentence is held
/// field for field against the rig's regex by `transport_line_shape.rs`, and
/// `native_row.py` `int()`s every group it reads, so a non-numeric field added
/// there would turn the whole reading null rather than error. This line is read
/// off the console export the rig already keeps whole.
///
/// `-` for a maximum whose row could not be named: the registry had no pending
/// job for that id, which is a late or withdrawn reply. It is written rather
/// than skipped so the line's shape never depends on the data.
fn log_worst(totals: &Traffic) {
    let name = |row: &'static str| if row.is_empty() { "-" } else { row };
    log::info!(
        "worst reply: copy {} us on `{}` moving {} B, \
         delivery {} us on `{}` moving {} B, over {} replies",
        totals.worst_copy_ns / 1_000,
        name(totals.worst_copy_row),
        totals.worst_copy_bytes,
        totals.worst_deliver_ns / 1_000,
        name(totals.worst_deliver_row),
        totals.worst_deliver_bytes,
        totals.replies,
    );
}

// ── The reply block table ────────────────────────────────────────────────────

/// How big one reply buffer has to be to earn a row in [`BLOCKS`].
///
/// A `Vec<u8>` this size is served by `dlmalloc` out of a region it took from
/// `memory.grow` and **never gives back**, so it is the size class whose
/// distribution decides whether the page's linear memory ratchets. Anything
/// under it is served out of the small bins and reuses freely.
const LARGE_BLOCK_BYTES: usize = 1 << 20;

/// How many DISTINCT exact sizes the table learns before it stops learning
/// new ones. Fixed because this table may not allocate to grow — it is read
/// on the path that is being measured for allocation.
const BLOCK_SIZE_SLOTS: usize = 24;

/// Every large reply buffer's **exact requested size**, with how many times
/// that exact size was asked for.
///
/// **Exact, never a power-of-two class, and that is the whole point of the
/// table.** The question it exists to answer is whether a picture-sized
/// request can be served out of the hole its predecessor left, and 33,554,432
/// and 35,651,584 answer that question differently while sharing every
/// bucket a class-based histogram would put them in. A smear of near-but-not-
/// equal sizes is a heap that ratchets under perfect reuse; spikes at a few
/// exact sizes is a heap that reuses and sends the question back to how many
/// large blocks are live at once.
///
/// A size the table has no slot for is counted in
/// [`Traffic::blocks_unlisted`] rather than folded into a neighbour, because
/// a size folded into a neighbour is indistinguishable from a size that was
/// really asked for.
type BlockTable = [(u64, u32); BLOCK_SIZE_SLOTS];

thread_local! {
    static BLOCKS: Cell<BlockTable> = const { Cell::new([(0, 0); BLOCK_SIZE_SLOTS]) };
}

/// File one reply buffer's exact size.
fn note_block(len: usize) {
    if len < LARGE_BLOCK_BYTES {
        TRAFFIC.with(|traffic| {
            let mut totals = traffic.get();
            totals.blocks_small += 1;
            traffic.set(totals);
        });
        return;
    }
    let len = len as u64;
    let listed = BLOCKS.with(|blocks| {
        let mut table = blocks.get();
        // The first slot that is either this exact size or unused. Slots are
        // filled front to back and never vacated, so a size already in the
        // table is always found before the first free slot and a size is
        // never listed twice.
        let Some(at) = table
            .iter()
            .position(|(size, count)| *size == len || *count == 0)
        else {
            return false;
        };
        let seen = if table[at].0 == len { table[at].1 } else { 0 };
        table[at] = (len, seen + 1);
        blocks.set(table);
        true
    });
    TRAFFIC.with(|traffic| {
        let mut totals = traffic.get();
        totals.blocks_large += 1;
        if !listed {
            totals.blocks_unlisted += 1;
        }
        traffic.set(totals);
    });
}

/// The block table as one sentence, newest totals first and then every exact
/// size the table holds, largest first.
///
/// A separate line from `transport:` deliberately: that sentence is held field
/// for field against the rig's regex by `transport_line_shape.rs`, and a
/// variable-length list cannot be held that way. This one is read off the
/// console export the rig already keeps whole.
fn log_blocks(totals: &Traffic) {
    let mut table = BLOCKS.with(Cell::get);
    table.sort_unstable_by_key(|(size, _)| std::cmp::Reverse(*size));
    let sizes = table
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(size, count)| format!("{size}x{count}"))
        .collect::<Vec<_>>()
        .join(", ");
    log::info!(
        "reply blocks: {} large (>= {} B) in {} exact sizes, {} small, \
         {} unlisted; {}",
        totals.blocks_large,
        LARGE_BLOCK_BYTES,
        table.iter().filter(|(_, count)| *count > 0).count(),
        totals.blocks_small,
        totals.blocks_unlisted,
        sizes,
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

        // `to_parts`, not `to_bytes`: a row whose payload is ALREADY resident
        // nominates it instead of writing it, so the grid is not memcpy'd into
        // a wire buffer here — on the frame thread, at the dispatch site.
        // Every other row answers `None` and the head IS the whole message,
        // byte-identical to what `to_bytes` wrote.
        let encode_start = web_time::Instant::now();
        let (head, resident) = request.to_parts();
        let encode_us = us(encode_start, web_time::Instant::now());
        let moved = head.len() + resident.as_ref().map_or(0, |p| p.len());
        let transfer = js_sys::Array::new();
        // The three pieces come from ONE `ResidentBytes`, which captured them
        // from its own owner — so they describe one allocation by construction
        // rather than by this call site getting the pairing right.
        let parts: Vec<crate::shared_loan::Lent> =
            std::iter::once(crate::shared_loan::Lent::Owned(head))
                .chain(
                    resident
                        .as_ref()
                        .map(|p| crate::shared_loan::Lent::Borrowed {
                            owner: p.owner(),
                            addr: p.addr(),
                            len: p.len(),
                        }),
                )
                .collect();
        let split = parts.len() > 1;
        let (loan, copied) = match crate::shared_loan::lend_parts(parts) {
            Ok((loan, views)) => {
                proto::set_loan(&message, loan);
                proto::set_field(&message, proto::REQUEST, &views.get(0));
                if split {
                    proto::set_field(&message, proto::REQ_PAYLOAD, &views.get(1));
                }
                (loan, 0)
            }
            Err(_) => {
                // No lending on this deployment, so the payload has to be
                // written after all. `to_bytes` rather than concatenating the
                // parts: the head was encoded WITHOUT its values and the two
                // spellings put them in different places, so joining them would
                // produce a message no decoder reads.
                let bytes = request.to_bytes();
                let sent = bytes.len();
                let payload = js_sys::Uint8Array::from(bytes.as_slice());
                transfer.push(&payload.buffer());
                proto::set_field(&message, proto::REQUEST, &payload);
                (crate::shared_loan::NO_LOAN, sent)
            }
        };
        // The post is hoisted out of the `match` so this call's own encode and
        // post figures reach `account` on the line it writes, rather than
        // trailing a dispatch behind into the next one.
        let post_start = web_time::Instant::now();
        let posted = self.worker.post_message_with_transfer(&message, &transfer);
        account_sent(
            moved,
            copied,
            encode_us,
            us(post_start, web_time::Instant::now()),
        );

        match posted {
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
