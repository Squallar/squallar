//! The message shapes the page and the rasterization worker agree on.
//!
//! Both halves are in this crate and compile from the same source, so the
//! field names here are the whole specification — there is no second
//! implementation to keep in step. What they cannot assume is that they are the
//! same *build*: see [`build_token`].

use wasm_bindgen::prelude::*;

/// Message tags. Read off `kind` on every message in both directions.
pub const KIND: &str = "kind";
/// Worker → page, once, when it is ready to take jobs.
pub const HELLO: &str = "hello";
/// Worker → page, when it could not start at all.
pub const FATAL: &str = "fatal";
/// Page → worker: rasterize this.
pub const JOB: &str = "job";
/// Worker → page: the answer to a job.
pub const DONE: &str = "done";

pub const ID: &str = "id";
/// Page → worker: the framed job, as `rustdar_frontend`'s
/// `JobRequest::to_bytes` writes it.
///
/// As of WO-M7b the leading byte of that payload is a **composed-registry
/// index plus one** (`rustdar_frontend::job_registry`, codes 1..=13 with 0
/// unallocated), followed by the one canonical envelope every kind shares.
/// The sparse `TAG_*` era — its per-tag numbering and the retired-tag holes
/// — and the hand-versioned changelog that once narrated it are git history
/// (the changelog was deleted with the protocol's version number at WO-M5);
/// what stands over the framing now is the [`build_token`] below, never a
/// hand-kept number.
pub const REQUEST: &str = "req";
pub const TOKEN: &str = "token";
pub const ERROR: &str = "error";

/// Worker → page: **the whole answer to a job**, every kind alike, as one
/// transferred `Uint8Array` in the dispatched codec row's own `encode_out`
/// form — or null for a job that produced nothing (a `None` result writes
/// both fields null explicitly, because the page holds a slot per id and
/// silence would strand it).
///
/// One field for every kind since WO-M7c closed the reply direction onto
/// the codec table: the plan-view frame, whose reply used to ride eight
/// named fields beside this one, travels in its own wire form
/// (`rustdar_radar::frame::RenderedFrame::to_bytes`) like every other
/// output. One array rather than one per buffer because each payload's
/// codec carries its own counts and refusals; a second description of the
/// same lengths on this message could disagree with the first in a way the
/// receiving side would have to invent an answer for.
pub const OUT: &str = "out";
/// Worker → page: which codec row's `encode_out` wrote [`OUT`] — the row's
/// **dense composed-registry code** (index plus one), the same one code
/// space the request direction's leading byte speaks, so one table names
/// every kind in both directions.
///
/// The page does not route on it: the reply is decoded through the row
/// recorded when the job was dispatched, and this tag is *verified* against
/// that row's code (`offload::deliver_encoded_reply`) — a mismatch is a
/// corrupt message or another build's reply, refused as "nothing to draw"
/// rather than decoded as whatever the tag claims.
pub const OUT_KIND: &str = "outkind";

/// What the page and the worker compare before the page trusts the worker.
///
/// They can differ. A dedicated worker is its own service-worker client, so
/// `sw.js`'s per-client shell pin — which exists precisely to keep one page on
/// one `(glue, wasm)` generation — does not cover it, and a worker started
/// across a deploy can fetch a *newer* generation's module than the page is
/// running. Two separate wasm instances do not produce the linker error that
/// mismatch would cause inside one; they produce a protocol disagreement,
/// which is silent and much worse.
///
/// `GITHUB_SHA` is what actually distinguishes two deploys; it is present in
/// CI and absent locally, where the token's second segment is a digest of the
/// wire's pinned identity rows instead
/// (`rustdar_frontend::wire_identity::wire_digest`). Both halves of one build
/// digest the same module, so the fallback cannot false-mismatch a matched
/// pair — and a local pair whose pinned wire rows differ now diverges, where
/// the deleted hand-kept protocol number matched any two local builds alike.
/// What the digest deliberately does not cover — the nested payload layouts,
/// pinned by `rustdar-radar`'s own suites — leaves a rasterizer-only local
/// staleness reading as the same build: a missed detection, accepted, because
/// locally there is no service-worker deploy skew to create such a pair and
/// production always has the SHA. (A cached CI build can bake a stale SHA,
/// but it bakes the *same* stale SHA into both halves — still only ever a
/// missed detection, never a false one.)
pub fn build_token() -> String {
    match option_env!("GITHUB_SHA") {
        // CI/production: the SHA distinguishes deploys — finer than any
        // hand-kept number, and it cannot be forgotten.
        Some(sha) => format!("{}/{}", env!("CARGO_PKG_VERSION"), sha),
        // Local dev: no SHA. Digest the wire's pinned identity rows instead —
        // see rustdar_frontend::wire_identity for scope and residuals.
        None => format!(
            "{}/wire-{:016x}",
            env!("CARGO_PKG_VERSION"),
            rustdar_frontend::wire_identity::wire_digest()
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

/// Set `key` on `object`, or log why it could not be.
///
/// `Reflect::set` fails only on a frozen or non-object target, neither of which
/// a freshly built `Object` is — but the result is `#[must_use]` and swallowing
/// it silently would hide a message that arrived half-built.
pub fn set_field(object: &js_sys::Object, key: &str, value: &JsValue) {
    if js_sys::Reflect::set(object, &JsValue::from_str(key), value).is_err() {
        log::error!("could not set {key} on a worker message");
    }
}
