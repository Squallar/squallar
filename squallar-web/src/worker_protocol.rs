//! The message shapes the page and the rasterization worker agree on.
//!
//! Both halves are in this crate and compile from the same source, so the
//! field names here are the whole specification. What they cannot assume is
//! that they are the same *build*: see [`build_token`].

use wasm_bindgen::prelude::*;

/// Message tags. Read off `kind` on every message in both directions.
pub const KIND: &str = "kind";
pub const HELLO: &str = "hello";
pub const FATAL: &str = "fatal";
pub const JOB: &str = "job";
pub const DONE: &str = "done";
/// **Either direction**: the sender has finished copying the buffers of the
/// [`LOAN`] it names, and the lender may free them. See [`crate::shared_loan`]
/// for why a view onto the peer's memory needs one and what happens without it.
///
/// Symmetric on purpose. A `JOB` lends the page's request to the worker and a
/// `DONE` lends the worker's answer to the page, so both sides both lend and
/// borrow, and one kind serves both rather than two that could drift.
pub const RELEASE: &str = "release";

pub const ID: &str = "id";
/// Page → worker: the framed job, as `squallar_worker`'s `JobRequest::to_bytes`
/// writes it. Its leading byte is a **composed-registry index plus one**
/// (codes 1..=15, 0 unallocated), then the canonical envelope every kind
/// shares.
pub const REQUEST: &str = "req";
/// Page -> worker: a job's payload, when the row nominated one to be LENT in
/// place rather than written into [`REQUEST`].
///
/// Absent is the ordinary case and means the request is whole in `REQUEST`, as
/// every build before the split wrote it. Present means `REQUEST` is the head
/// alone and these bytes are the row's payload; the two are reassembled by
/// `JobRequest::from_parts`, never by concatenation.
pub const REQ_PAYLOAD: &str = "reqpay";
pub const TOKEN: &str = "token";
pub const ERROR: &str = "error";
/// The [`crate::shared_loan::LoanId`] a `JOB`, a `DONE` or a `RELEASE` names.
///
/// On a `JOB`/`DONE` it says "the typed arrays on this message are VIEWS into
/// my memory, and I am holding them until you send `RELEASE` with this number".
/// [`crate::shared_loan::NO_LOAN`] — or an absent field, which reads the same —
/// says the arrays are transferred copies the receiver already owns and there
/// is nothing to release. A build serving without COOP/COEP writes 0 on every
/// message, which is the fallback wire and also the Tier-2 negative control.
pub const LOAN: &str = "loan";
/// Worker → page, on `HELLO`: how many threads rayon's global pool ACTUALLY
/// has in the worker, read back out of rayon rather than echoing the number
/// `worker.js` asked `initThreadPool` for.
///
/// It rides the handshake because the page's console is the only one the
/// browser rig scans, and the worker's is not: `window.__rig_console` is a
/// page-side ring. Without this the fallback in `worker.js` — the arm a
/// browser takes when it has no `SharedArrayBuffer`, no nested Workers, or
/// was served without COOP/COEP — is invisible from outside, and every Tier-2
/// assertion passes on a single-threaded worker exactly as it does on a
/// pooled one. A gate that cannot tell those apart is not gating WS3b.
///
/// Absent reads as unknown, not as 1: a worker from a build before WS3b never
/// sets it, and reporting that as "1 thread" would be inventing a measurement.
pub const THREADS: &str = "threads";
/// Worker → page, on `HELLO` and on every `DONE`: `memory().buffer().byteLength`
/// of the WORKER's own instance, in bytes. The worker's heap is a second
/// linear memory under a second `--max-memory` that the page cannot read, so
/// the worker says, on the messages that already cross. Absent reads as
/// unknown, never as 0: a worker from a build before this field never sets
/// it, and the page's `linear_memory` answers `None` for the worker half.
pub const MEM: &str = "mem";
/// Worker → page, beside [`MEM`] on `HELLO` and every `DONE`: **live bytes**
/// on the WORKER's own instance — what its allocator has handed out and not
/// been handed back (`squallar_alloc::live_bytes`), in bytes. The figure
/// that can fall where `MEM` cannot: a linear memory never shrinks, so
/// `MEM − LIVE` is that heap's freed-but-reserved headroom. Absent reads as
/// unknown, never as 0: a worker from a build before this field never sets
/// it, and one whose counter was never installed says nothing either.
pub const LIVE: &str = "live";
/// Worker → page, on `HELLO` only: **the maximum the worker's own linear
/// memory was constructed with**, in bytes.
///
/// The page chose this figure and handed it over on the Worker's `name`
/// (`squallar-web/heap.js`), so it is mostly a confirmation — except when the
/// engine refused the supplied memory and the glue built one at the module's
/// declared bound instead, which is the one case the page's own copy is
/// wrong. Nothing can read a memory's maximum back
/// (`WebAssembly.Memory.prototype.type()` exists in neither engine), so this
/// message is the only witness. Absent reads as "what we asked for", never as
/// 0. On the hello alone because a ceiling cannot change for the life of an
/// instance and a `DONE` is the hot path.
pub const MEMMAX: &str = "memmax";
/// Worker → page, on `HELLO`: the page's end of the **tile lane** — a
/// `MessagePort` into a nested Worker that shares the rasterization worker's
/// memory and runs the `basemap/tiles` row on a thread of its own, so a
/// batch of vector tiles never waits behind a multi-second job in this
/// worker's message loop. Transferred, so it rides at most once. Absent on a
/// build before the lane, and on a worker whose spawn failed; the page reads
/// both as "no lane" and the tile pump keeps styling on the frame thread.
pub const LANE: &str = "lane";
/// Worker → lane, the first and only message the worker sends its nested
/// Worker: the module and memory to instantiate on ([`MODULE`], [`MEMORY`])
/// and the lane's end of the port ([`PORT`]). Read by `tile-lane.js`.
pub const LANE_INIT: &str = "laneinit";
pub const MODULE: &str = "module";
pub const MEMORY: &str = "memory";
pub const PORT: &str = "port";
/// Lane → page, first message on the lane port: the module is instantiated
/// on the worker's memory and the lane will answer `JOB`s. Carries [`MEM`],
/// which on this port is the SAME heap the worker reports — one memory, two
/// threads.
pub const LANE_HELLO: &str = "lanehello";
/// Worker → page: the lane's nested Worker raised an error. Its jobs are owed
/// answers that will not come, and the page fails them.
pub const LANE_LOST: &str = "lanelost";

/// Worker → page: **the answer's HEAD** — scalars and framing in the dispatched
/// codec row's own `encode_out` form, as one transferred `Uint8Array`; null
/// for a job that produced nothing (all three reply fields are written null
/// explicitly, because the page holds a slot per id). The row's nominated
/// LARGE buffers ride [`TAILS`], whose count the row's own decoder judges.
pub const OUT: &str = "out";
/// Worker → page: which codec row's `encode_out` wrote [`OUT`]. The page does
/// not route on it — the reply is decoded through the row recorded at
/// dispatch and this tag is *verified* against that row's code.
pub const OUT_KIND: &str = "outkind";
/// Worker → page: the row's nominated large flat buffers as a `js_sys::Array`
/// of per-tail `Uint8Array`s, EACH transferred, so the page adopts every big
/// buffer instead of copying it out of a concatenation; null on the nothing
/// arm. Order is the row's own convention, pinned by the wire-identity rows.
pub const TAILS: &str = "tails";

/// What the page and the worker compare before the page trusts the worker.
///
/// They can differ: a dedicated worker is its own service-worker client, so
/// `sw.js`'s per-client shell pin does not cover it, and a worker started
/// across a deploy can fetch a newer generation's module — a silent protocol
/// disagreement rather than a linker error. `GITHUB_SHA` distinguishes two
/// deploys in CI; locally the second segment is a digest of the wire's pinned
/// identity rows, which does not cover the nested payload layouts.
pub fn build_token() -> String {
    match option_env!("GITHUB_SHA") {
        Some(sha) => format!("{}/{}", env!("CARGO_PKG_VERSION"), sha),
        None => format!(
            "{}/wire-{:016x}",
            env!("CARGO_PKG_VERSION"),
            squallar_worker::wire_identity::wire_digest()
        ),
    }
}

/// `js_sys::Reflect::get` with the failure folded into `None`: every read here
/// is of a field that may simply be absent on a message from another build.
pub fn field(object: &JsValue, key: &str) -> Option<JsValue> {
    js_sys::Reflect::get(object, &JsValue::from_str(key)).ok()
}

pub fn string_field(object: &JsValue, key: &str) -> Option<String> {
    field(object, key)?.as_string()
}

/// The [`LOAN`] on a message, with absent, null and unreadable all reading as
/// [`NO_LOAN`].
///
/// Folded together deliberately: every one of them means "nothing on this
/// message is a view into the sender's memory", which is the safe reading. A
/// build from before this wire existed sets no field at all and must be
/// understood as carrying copies, not as lending loan 0.
pub fn loan_field(object: &JsValue) -> crate::shared_loan::LoanId {
    field(object, LOAN)
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map_or(crate::shared_loan::NO_LOAN, |v| {
            v as crate::shared_loan::LoanId
        })
}

/// Write a [`LOAN`] onto a message. [`NO_LOAN`](crate::shared_loan::NO_LOAN) is
/// written explicitly rather than omitted, so every message says which wire it
/// is on and a reader never has to distinguish absent from zero.
pub fn set_loan(object: &js_sys::Object, loan: crate::shared_loan::LoanId) {
    set_field(object, LOAN, &JsValue::from_f64(f64::from(loan)));
}

/// Set `key` on `object`, or log why it could not be: `Reflect::set` fails only
/// on a frozen or non-object target, but swallowing a `#[must_use]` error
/// would hide a message that arrived half-built.
pub fn set_field(object: &js_sys::Object, key: &str, value: &JsValue) {
    if js_sys::Reflect::set(object, &JsValue::from_str(key), value).is_err() {
        log::error!("could not set {key} on a worker message");
    }
}
