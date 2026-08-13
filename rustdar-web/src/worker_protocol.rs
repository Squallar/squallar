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
/// Worker → page: where the rendered sweep's cut declared its velocity folds,
/// m/s, or **null** for a raster with no one cut behind it.
///
/// Null is the encoding of `None`, not a compatibility shim: a Level III
/// product, a volume product and a Message 1 volume all legitimately have no
/// such number, and `Option::None` has to cross as something. The page reads it
/// back through the same `as_f64` filter every numeric field takes, so an
/// absent field and a null one resolve alike — which is what makes writing it
/// unconditionally in every arm of `post_result` the whole of the contract.
pub const NYQUIST: &str = "nyq";
/// Worker → page: where the melting layer the rendered raster was classified
/// against came from, as
/// [`rustdar_frontend::offload::MeltingLayerWire::wire_code`], or **null** for
/// a raster that classified nothing — which is every product but the hybrid
/// classification.
///
/// A number and not a string for the reason [`OUT_KIND`] is one: the page turns
/// it back into the enum through the same exhaustive pair that wrote it, so a
/// byte this build does not have resolves to "no source stated" rather than to
/// a plausible-looking label. Null encodes `None`, exactly as [`NYQUIST`]'s
/// does, and the page reads both back through the same `as_f64` filter.
pub const MELTING_LAYER: &str = "mls";

/// Worker → page: an output that is not a plan-view frame — a cross-section
/// raster or a voxel grid — as **one** transferred `Uint8Array` in the payload
/// type's own wire form.
///
/// One field rather than a field per kind, and one array rather than one per
/// plane, because the codec that produced it
/// (`rustdar_radar::xsect::CrossSection::to_bytes`,
/// `rustdar_radar::voxel::VoxelGrid::to_bytes`) already carries its own magic,
/// version and length prefixes. Splitting a section into three typed arrays
/// here would put a second description of those planes' lengths on the wire,
/// and the two could disagree in a way the receiving side would have to
/// invent an answer for.
///
/// On a `Frame` reply this is null and [`IMAGE`]/[`VALUES`]/[`MAX_RANGE`] are
/// written exactly as they always were; on a `Section` or `Voxels` reply those
/// three are null and this is set. A `None` result writes all four null-ish,
/// because the page holds a slot per id and silence would strand it.
pub const OUT: &str = "out";
/// Worker → page: which kind of output [`OUT`] carries, as
/// `rustdar_radar::types::RenderView::wire_code`.
///
/// The payload wire forms are self-describing enough to *refuse* the wrong one
/// — each has its own magic — but "try to decode it as a section, and if that
/// fails try a grid" turns a corrupt payload into a silently different kind.
/// The tag says which decoder to run, and the magic says whether it was right.
pub const OUT_KIND: &str = "outkind";

/// Bumped whenever the shapes above change.
///
/// Part of [`build_token`] rather than checked on its own: a page and a worker
/// running different protocol versions are, by definition, different builds.
///
/// Version 2 added [`OUT`] and [`OUT_KIND`], when a job could answer with
/// something other than a plan-view frame. Version 3 added [`NYQUIST`], when
/// the plan view began reporting the fold limit of the sweep it drew. It is
/// folded into the token, so a page and a worker on opposite sides of a deploy
/// boundary terminate cleanly rather than exchanging a reply one of them cannot
/// read.
///
/// A missing [`NYQUIST`] would degrade rather than break — the page would read
/// `None` and the legend would say nothing — but "degrades quietly" is exactly
/// the class of mismatch a version number exists to convert into a clean
/// termination, and a page silently unable to name a fold limit is the same
/// silence this workstream is closing everywhere else.
///
/// Version 4 added [`MELTING_LAYER`], and that one is not merely a caption: a
/// page reading `None` from an older worker would draw *no* qualification over
/// a classification standing on the fleet default, which is the picture that
/// scores 16 % against the RPG's own answer. Silently indistinguishable from
/// the 95 % one is precisely what the version number is here to prevent.
const PROTOCOL_VERSION: u32 = 4;

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
