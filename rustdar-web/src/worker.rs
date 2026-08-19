//! The rasterization worker's side of the boundary.
//!
//! This runs inside a dedicated Web Worker started by [`crate::worker_port`].
//! It is the *same wasm module* the page runs, instantiated a second time —
//! see `worker.js` — so it can call
//! `rustdar_worker::offload::execute_encoded` directly and there is
//! exactly one rasterizer in the deployment.
//!
//! A second module would have meant a second `(glue, wasm)` pair for `sw.js`'s
//! per-client shell pinning to keep atomic, and that machinery exists because
//! getting it wrong produces a `LinkError` at startup. One module costs a
//! second compile and a second linear memory instead, both of which are off the
//! main thread.
//!
//! Nothing here touches `window`: there is not one. The module's DOM-facing
//! code is simply never called on this instance, which is why instantiating it
//! twice is safe — there is no `#[wasm_bindgen(start)]` anywhere in the crate,
//! so nothing runs until the page or `worker.js` asks for it.

use crate::worker_protocol as proto;
use wasm_bindgen::prelude::*;

/// Boot the worker: install the message handler and announce readiness.
///
/// Called by `worker.js` after `init()`. Exported under a distinctive name
/// because it shares an export namespace with [`crate::start`], the page's
/// entry point, and the two must never be confused for one another.
#[wasm_bindgen]
pub fn rustdar_worker_main() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    // Ignored rather than propagated: a second `init` in the same worker is not
    // a reason to refuse jobs, and the page has its own logger either way.
    let _ = console_log::init_with_level(log::Level::Info);

    let scope = worker_scope()?;
    let handler_scope = scope.clone();
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            handle_message(&handler_scope, &event.data());
        });
    scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    // Leaked deliberately, as `geolocation::start_watch` does: the handler must
    // outlive this call and lives exactly as long as the worker does.
    on_message.forget();

    let hello = js_sys::Object::new();
    proto::set_field(&hello, proto::KIND, &JsValue::from_str(proto::HELLO));
    proto::set_field(
        &hello,
        proto::TOKEN,
        &JsValue::from_str(&proto::build_token()),
    );
    scope.post_message(&hello)?;

    log::info!("rustdar rasterization worker ready");
    Ok(())
}

fn worker_scope() -> Result<web_sys::DedicatedWorkerGlobalScope, JsValue> {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .map_err(|_| JsValue::from_str("not running inside a dedicated worker"))
}

/// Rasterize one job and post the answer back.
///
/// A message this build cannot read is answered with a failed job rather than
/// dropped: the page is holding a render slot and a pane's in-flight mark
/// against every id it posted, and only a reply releases them.
fn handle_message(scope: &web_sys::DedicatedWorkerGlobalScope, data: &JsValue) {
    if proto::string_field(data, proto::KIND).as_deref() != Some(proto::JOB) {
        log::warn!("worker ignoring a message that is not a job");
        return;
    }
    let Some(id) = proto::field(data, proto::ID).and_then(|v| v.as_f64()) else {
        log::error!("worker got a job with no id; nothing to answer");
        return;
    };
    let id = id as u64;

    // A checked cast, not a Uint8Array-from-view construction: that
    // constructor is the JS `new Uint8Array(typedArray)` COPY constructor,
    // so the old spelling paid a hidden JS→JS copy of the whole request
    // (up to the ~47-69 MiB decode archive) before `to_vec`'s unavoidable
    // JS→wasm crossing (WO-M7d, the same latent class as the page's
    // deliver). A payload that is not a typed array refuses to the empty
    // request, which `execute_encoded` answers as a failed job — the
    // refusal posture every malformed message already has.
    let request = proto::field(data, proto::REQUEST)
        .and_then(|v| v.dyn_into::<js_sys::Uint8Array>().ok())
        .map(|v| v.to_vec())
        .unwrap_or_default();

    let result = rustdar_worker::offload::execute_encoded(&request);
    if result.is_none() {
        // Either the payload was unreadable or the renderer found no sweep. The
        // page cannot tell them apart and does not need to: both mean "no
        // frame", which is what a failed render has always meant.
        log::debug!("worker job {id} produced no frame");
    }
    if let Err(e) = post_result(scope, id, result) {
        log::error!("worker could not answer job {id}: {e:?}");
    }
}

/// Post the answer, moving the buffers rather than copying them.
///
/// The reply is the `OUT`/`OUT_KIND`/`TAILS` trio since WO-M7d: `OUT`
/// carries the row's `encode_out` HEAD (scalars and framing — 29 bytes for
/// a full frame) as one transferred `Uint8Array`; `TAILS` carries the
/// row's nominated large flat buffers (the frame's polar block and image)
/// as an array of per-tail `Uint8Array`s, each transferred; and `OUT_KIND`
/// says which registry row encoded them — the dense composed-registry
/// code, the same code space the request direction speaks. Every buffer is
/// built as one copy out of this instance's linear memory (unavoidable
/// without a `SharedArrayBuffer` this deployment cannot have) and then
/// *transferred*, so the page adopts it instead of receiving a second
/// copy: 26.08 MiB of worker-side traffic per widest 2048² still frame
/// where the one-buffer WO-M7c shape paid 68.16 (the concatenation and the
/// encode sink's double-buffer are gone — `execute_encoded`'s comment
/// carries the derivation).
///
/// `None` writes explicit nulls rather than posting nothing, because the page
/// holds a render slot, a pane's in-flight mark and a pending-map entry against
/// every id it posted, and only a reply releases them. Silence wedges the pane
/// forever; a null reply is "nothing to draw", which every path already handles.
fn post_result(
    scope: &web_sys::DedicatedWorkerGlobalScope,
    id: u64,
    result: Option<(u8, Vec<u8>, Vec<Vec<u8>>)>,
) -> Result<(), JsValue> {
    let message = js_sys::Object::new();
    proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::DONE));
    proto::set_field(&message, proto::ID, &JsValue::from_f64(id as f64));

    let transfer = js_sys::Array::new();
    // Written first and overwritten by the arm that has a payload, so no path
    // out of this function can leave a field absent — an absent field and a
    // null one read the same on the page, but only one of them is true by
    // construction.
    proto::set_field(&message, proto::OUT, &JsValue::NULL);
    proto::set_field(&message, proto::OUT_KIND, &JsValue::NULL);
    proto::set_field(&message, proto::TAILS, &JsValue::NULL);

    if let Some((kind, head, tails)) = result {
        let out = js_sys::Uint8Array::from(head.as_slice());
        transfer.push(&out.buffer());
        proto::set_field(&message, proto::OUT, &out);
        proto::set_field(
            &message,
            proto::OUT_KIND,
            &JsValue::from_f64(f64::from(kind)),
        );
        // Each tail is its own Uint8Array over its own ArrayBuffer, and each
        // buffer rides the transfer list — the page adopts them; nothing
        // multi-MiB is structured-cloned.
        let tails_array = js_sys::Array::new();
        for tail in tails {
            let t = js_sys::Uint8Array::from(tail.as_slice());
            transfer.push(&t.buffer());
            tails_array.push(&t);
        }
        proto::set_field(&message, proto::TAILS, &tails_array);
    }
    scope.post_message_with_transfer(&message, &transfer)
}
