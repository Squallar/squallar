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

pub const ID: &str = "id";
/// Page → worker: the framed job, as `rustdar_worker`'s `JobRequest::to_bytes`
/// writes it. Its leading byte is a **composed-registry index plus one**
/// (codes 1..=13, 0 unallocated), then the canonical envelope every kind
/// shares.
pub const REQUEST: &str = "req";
pub const TOKEN: &str = "token";
pub const ERROR: &str = "error";

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
            rustdar_worker::wire_identity::wire_digest()
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

/// Set `key` on `object`, or log why it could not be: `Reflect::set` fails only
/// on a frozen or non-object target, but swallowing a `#[must_use]` error
/// would hide a message that arrived half-built.
pub fn set_field(object: &js_sys::Object, key: &str, value: &JsValue) {
    if js_sys::Reflect::set(object, &JsValue::from_str(key), value).is_err() {
        log::error!("could not set {key} on a worker message");
    }
}
