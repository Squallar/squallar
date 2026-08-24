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
pub fn squallar_worker_main() -> Result<(), JsValue> {
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
    // Asked of rayon, not of `worker.js`. `current_num_threads` reports the
    // pool that actually got built, so the fallback arm reports 1 by telling
    // the truth rather than by anyone remembering to say so.
    let threads = rayon::current_num_threads();
    proto::set_field(&hello, proto::THREADS, &JsValue::from_f64(threads as f64));
    scope.post_message(&hello)?;

    log::info!("squallar rasterization worker ready (rayon: {threads} threads)");
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
    match proto::string_field(data, proto::KIND).as_deref() {
        Some(proto::JOB) => handle_job(scope, data),
        // The page has finished copying a reply out of this worker's memory.
        Some(proto::RELEASE) => crate::shared_loan::release(proto::loan_field(data)),
        _ => log::warn!("worker ignoring a message that is not a job"),
    }
}

fn handle_job(scope: &web_sys::DedicatedWorkerGlobalScope, data: &JsValue) {
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

    // `to_vec` above IS the copy the borrower owes, and it has happened, so the
    // page may free the request now — **before** the job runs, not after.
    // A decode request is the archive, up to ~47-69 MiB; holding it for the
    // length of the rasterization would double the peak for the whole job.
    release_to_page(scope, proto::loan_field(data));

    let result = squallar_worker::offload::execute_encoded(&request);
    if result.is_none() {
        log::debug!("worker job {id} produced no frame");
    }
    if let Err(e) = post_result(scope, id, result) {
        log::error!("worker could not answer job {id}: {e:?}");
    }
}

/// Tell the page it may free the request it lent, if it lent one.
///
/// Best-effort by design: a failed post here costs the page one buffer held
/// until the worker is retired, which `abandon_worker` sweeps. It must not
/// abandon the job.
fn release_to_page(scope: &web_sys::DedicatedWorkerGlobalScope, loan: crate::shared_loan::LoanId) {
    if loan == crate::shared_loan::NO_LOAN {
        return;
    }
    let message = js_sys::Object::new();
    proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::RELEASE));
    proto::set_loan(&message, loan);
    if let Err(e) = scope.post_message(&message) {
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
    let posted = scope.post_message_with_transfer(&message, &transfer);
    if posted.is_err() {
        crate::shared_loan::release(lent);
    }
    posted
}
