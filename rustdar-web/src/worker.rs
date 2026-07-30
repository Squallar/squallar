//! The rasterization worker's side of the boundary.
//!
//! This runs inside a dedicated Web Worker started by [`crate::worker_port`].
//! It is the *same wasm module* the page runs, instantiated a second time —
//! see `worker.js` — so it can call `rustdar_frontend::offload::execute_bytes`
//! directly and there is exactly one rasterizer in the deployment.
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
use rustdar_frontend::offload::{JobOutput, JobResult, RenderedFrame};
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

    let request = proto::field(data, proto::REQUEST)
        .map(|v| js_sys::Uint8Array::new(&v).to_vec())
        .unwrap_or_default();

    let result = rustdar_frontend::offload::execute_bytes(&request);
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
/// The frame's two buffers are `IMAGE_SIZE² × 4` bytes each — 4 MiB apiece at
/// the browser's 1024². They are built as typed arrays (one copy out of this
/// instance's linear memory, which is unavoidable without a `SharedArrayBuffer`
/// this deployment cannot have) and then *transferred*, so the page adopts them
/// instead of receiving a second copy.
///
/// # Three arms, and every one of them writes every field
///
/// The `Frame` arm writes `IMAGE`, `VALUES` and `MAX_RANGE` **byte for byte as
/// it always did** — that is the point of the shape below, and it is what makes
/// "the working path is unchanged" a property of the code rather than a claim.
/// The other two write those three null and put the whole output in `OUT` as a
/// single transferred `Uint8Array` in the payload type's own wire form.
///
/// `None` writes explicit nulls rather than posting nothing, because the page
/// holds a render slot, a pane's in-flight mark and a pending-map entry against
/// every id it posted, and only a reply releases them. Silence wedges the pane
/// forever; a null reply is "nothing to draw", which every path already handles.
fn post_result(
    scope: &web_sys::DedicatedWorkerGlobalScope,
    id: u64,
    result: JobResult,
) -> Result<(), JsValue> {
    let message = js_sys::Object::new();
    proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::DONE));
    proto::set_field(&message, proto::ID, &JsValue::from_f64(id as f64));

    let transfer = js_sys::Array::new();
    // Written first and overwritten by the arm that has one, so no path out of
    // this function can leave a field absent — an absent field and a null one
    // read the same on the page, but only one of them is true by construction.
    proto::set_field(&message, proto::IMAGE, &JsValue::NULL);
    proto::set_field(&message, proto::VALUES, &JsValue::NULL);
    proto::set_field(&message, proto::MAX_RANGE, &JsValue::from_f64(0.0));
    proto::set_field(&message, proto::OUT, &JsValue::NULL);
    proto::set_field(&message, proto::OUT_KIND, &JsValue::NULL);

    match result {
        None => {}
        Some(JobOutput::Frame(RenderedFrame {
            image,
            max_range_km,
            values,
        })) => {
            let image = js_sys::Uint8Array::from(image.as_slice());
            let values = js_sys::Float32Array::from(values.as_slice());
            transfer.push(&image.buffer());
            transfer.push(&values.buffer());
            proto::set_field(&message, proto::IMAGE, &image);
            proto::set_field(&message, proto::VALUES, &values);
            proto::set_field(&message, proto::MAX_RANGE, &JsValue::from_f64(max_range_km));
        }
        Some(output) => {
            let kind = output.view().wire_code();
            let bytes = match output {
                JobOutput::Section(section) => section.to_bytes(),
                JobOutput::Voxels(grid) => grid.to_bytes(),
                // Answered above; naming it keeps the match exhaustive by value
                // so a fourth output kind stops the build here rather than
                // falling into a catch-all that posts an empty payload.
                JobOutput::Frame(_) => unreachable!("the frame arm is above"),
            };
            let out = js_sys::Uint8Array::from(bytes.as_slice());
            transfer.push(&out.buffer());
            proto::set_field(&message, proto::OUT, &out);
            proto::set_field(
                &message,
                proto::OUT_KIND,
                &JsValue::from_f64(f64::from(kind)),
            );
        }
    }
    scope.post_message_with_transfer(&message, &transfer)
}
