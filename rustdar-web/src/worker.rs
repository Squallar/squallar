//! The rasterization worker's side of the boundary: a dedicated Web Worker
//! started by [`crate::worker_port`], running the *same wasm module* the page
//! runs, instantiated a second time. A second module would have meant a
//! second `(glue, wasm)` pair for `sw.js`'s per-client shell pinning to keep
//! atomic. Nothing here touches `window`: there is not one.

use crate::worker_protocol as proto;
use wasm_bindgen::prelude::*;

/// Boot the worker: install the message handler and announce readiness. Called
/// by `worker.js` after `init()`, under a distinctive name because it shares
/// an export namespace with [`crate::start`].
#[wasm_bindgen]
pub fn rustdar_worker_main() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
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
    scope.post_message(&hello)?;

    log::info!("rustdar rasterization worker ready");
    Ok(())
}

fn worker_scope() -> Result<web_sys::DedicatedWorkerGlobalScope, JsValue> {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .map_err(|_| JsValue::from_str("not running inside a dedicated worker"))
}

/// Rasterize one job and post the answer back. A message this build cannot
/// read is answered with a failed job rather than dropped: the page holds a
/// render slot and a pane's in-flight mark against every id it posted.
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

    // A checked cast, not a Uint8Array-from-view construction: that constructor
    // is the JS COPY constructor, so the old spelling paid a hidden JS→JS copy
    // of the whole request (up to ~47-69 MiB). A payload that is not a typed
    // array refuses to the empty request, answered as a failed job.
    let request = proto::field(data, proto::REQUEST)
        .and_then(|v| v.dyn_into::<js_sys::Uint8Array>().ok())
        .map(|v| v.to_vec())
        .unwrap_or_default();

    let result = rustdar_worker::offload::execute_encoded(&request);
    if result.is_none() {
        log::debug!("worker job {id} produced no frame");
    }
    if let Err(e) = post_result(scope, id, result) {
        log::error!("worker could not answer job {id}: {e:?}");
    }
}

/// Post the answer, moving the buffers rather than copying them.
///
/// The reply is the `OUT`/`OUT_KIND`/`TAILS` trio: `OUT` carries the row's
/// `encode_out` HEAD as one transferred `Uint8Array`, `TAILS` the row's
/// nominated large flat buffers as per-tail `Uint8Array`s, each transferred.
/// Every buffer is one copy out of this instance's linear memory (unavoidable
/// without a `SharedArrayBuffer`) and is then transferred: 26.08 MiB per
/// widest 2048² still frame, where the one-buffer shape paid 68.16.
///
/// `None` writes explicit nulls rather than posting nothing: the page holds a
/// render slot against every id, and silence wedges it.
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
    // can leave a field absent — absent and null read the same on the page.
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
        // Each tail rides the transfer list; nothing multi-MiB is cloned.
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
