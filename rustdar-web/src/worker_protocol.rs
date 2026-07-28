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
pub const REQUEST: &str = "req";
pub const TOKEN: &str = "token";
pub const ERROR: &str = "error";
pub const IMAGE: &str = "image";
pub const VALUES: &str = "values";
pub const MAX_RANGE: &str = "range";

/// Bumped whenever the shapes above change.
///
/// Part of [`build_token`] rather than checked on its own: a page and a worker
/// running different protocol versions are, by definition, different builds.
const PROTOCOL_VERSION: u32 = 1;

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
/// CI and absent locally, where the protocol version carries the check alone.
/// A cached build can bake a stale SHA, but it bakes the *same* stale SHA into
/// both halves — so the failure mode is only ever a missed detection, never a
/// false one.
pub fn build_token() -> String {
    format!(
        "{}/{}/{}",
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION,
        option_env!("GITHUB_SHA").unwrap_or("dev"),
    )
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
