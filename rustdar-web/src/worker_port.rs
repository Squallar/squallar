//! The page's side of the rasterization worker.
//!
//! `rustdar_frontend::offload` owns the decision about where a CPU-bound job
//! runs, and knows nothing about the browser: the dependency runs
//! `rustdar-web` → `rustdar-frontend`, and adding `web-sys` to the frontend to
//! close the loop would put browser types in the crate desktop, Android and iOS
//! all share. So the worker is *installed* into the funnel as a
//! [`JobSink`], from here — and this file is where a `JobRequest` becomes the
//! bytes a `postMessage` transfer list can move, because that cost is the
//! browser's and belongs in the browser's adapter.
//!
//! Until [`attach`] succeeds — and forever, in a browser where it cannot —
//! `offload_job` runs rasterization inline, which is the behaviour the web
//! build had before any of this existed. Every failure path below therefore
//! ends in "leave it inline" rather than in an error the user sees.

use crate::worker_protocol as proto;
use rustdar_frontend::offload::{self, JobOutput, JobRequest, JobSink, RenderedFrame};
use wasm_bindgen::prelude::*;

/// Where the worker's bootstrap lives, relative to the page.
///
/// Relative on purpose: the site is served from a project-Pages subpath, so a
/// root-absolute URL works under a local server and 404s in production.
/// `.github/scripts/check-relative-paths.py` fails the build over it.
const WORKER_URL: &str = "./worker.js";

/// Start the rasterization worker and, once it identifies itself as this same
/// build, route [`offload::offload_job`] through it.
///
/// Returns immediately. The worker announces itself asynchronously, so
/// rasterization runs inline for the first frames and moves off the main
/// thread once the handshake lands — which is also the whole fallback story if
/// the handshake never does.
pub fn attach() {
    let options = web_sys::WorkerOptions::new();
    // A module worker, because `worker.js` `import`s the wasm-bindgen glue that
    // `--target web` emits. Classic workers cannot.
    options.set_type(web_sys::WorkerType::Module);

    let worker = match web_sys::Worker::new_with_options(WORKER_URL, &options) {
        Ok(worker) => worker,
        Err(e) => {
            log::warn!("no rasterization worker ({e:?}); rendering on the main thread");
            return;
        }
    };

    let on_message_worker = worker.clone();
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            handle_message(&on_message_worker, &event.data());
        });
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    // A worker that dies mid-job owes replies that will never arrive, and every
    // one of those is holding a render slot and a pane's in-flight mark.
    // `abandon_worker` fails them, which releases both and lets the next frame
    // re-dispatch — inline, because the port goes with it.
    let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        offload::abandon_worker("the worker reported an error");
    });
    worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();
}

fn handle_message(worker: &web_sys::Worker, data: &JsValue) {
    match proto::string_field(data, proto::KIND).as_deref() {
        Some(proto::HELLO) => {
            let theirs = proto::string_field(data, proto::TOKEN).unwrap_or_default();
            let ours = proto::build_token();
            if theirs != ours {
                // Not this build. See `worker_protocol::build_token` for how
                // that happens: the worker is its own service-worker client and
                // can be served a different shell generation than the page.
                log::warn!(
                    "rasterization worker is a different build ({theirs} vs {ours}); \
                     rendering on the main thread"
                );
                worker.terminate();
                offload::abandon_worker("build token mismatch");
                return;
            }
            log::info!("rasterization worker attached ({ours})");
            offload::set_worker(Box::new(Port {
                worker: worker.clone(),
            }));
        }
        Some(proto::FATAL) => {
            let error = proto::string_field(data, proto::ERROR).unwrap_or_default();
            log::warn!(
                "rasterization worker failed to start ({error}); rendering on the main thread"
            );
            worker.terminate();
            offload::abandon_worker("the worker failed to start");
        }
        Some(proto::DONE) => deliver(data),
        other => log::warn!("ignoring a worker message of kind {other:?}"),
    }
}

/// Turn a `done` message back into a [`JobResult`](offload::JobResult) and hand
/// it to the job that asked for it.
///
/// The buffers arrive transferred, so reading them here is the first copy back
/// into this instance's linear memory; there is no way around that one without
/// a `SharedArrayBuffer`, which needs COOP/COEP headers GitHub Pages does not
/// let this deployment set.
fn deliver(data: &JsValue) {
    let Some(id) = proto::field(data, proto::ID).and_then(|v| v.as_f64()) else {
        log::error!("worker answered with no job id");
        return;
    };

    let image = proto::field(data, proto::IMAGE).filter(|v| !v.is_null());
    let frame = image.map(|image| {
        JobOutput::Frame(RenderedFrame {
            image: js_sys::Uint8Array::new(&image).to_vec(),
            max_range_km: proto::field(data, proto::MAX_RANGE)
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            // A message this build did not write, or one whose two halves
            // disagree about the picture, decodes to nothing — the readout goes
            // quiet rather than the page panicking on a slice index in a
            // browser. See `PolarField::from_bytes`.
            polar: proto::field(data, proto::POLAR)
                .filter(|v| !v.is_null())
                .map(|v| offload::decode_polar(&js_sys::Uint8Array::new(&v).to_vec()))
                .unwrap_or_default(),
            // `as_f64` is `None` for the null this field carries whenever the
            // rendered raster had no one cut behind it, so the absence needs no
            // separate test — see `proto::NYQUIST`.
            nyquist_ms: proto::field(data, proto::NYQUIST).and_then(|v| v.as_f64()),
            // Null (a raster that classified nothing) and a byte this build
            // does not have both land on `None` — see `proto::MELTING_LAYER`.
            // The page then draws no qualification, which is the same thing it
            // draws for a measured layer; the protocol token is what keeps a
            // worker old enough to produce that from ever being attached.
            melting_layer_source: proto::field(data, proto::MELTING_LAYER)
                .and_then(|v| v.as_f64())
                .and_then(|v| offload::MeltingLayerWire::from_wire_code(v as u8))
                .map(|wire| wire.0),
            // The same filter for the same reason — see `proto::STORM_MOTION`.
            // A raster that applied no vector and a byte this build cannot read
            // both land on `None`, and the pane then draws no vector at all;
            // the protocol token is what keeps a worker old enough to mean the
            // second by the first from ever being attached.
            //
            // All three fields or none. A trio that arrived half-formed would
            // otherwise become a legend reading a real source beside a zeroed
            // speed, which is a confident lie about what shifted the picture —
            // so the `?`-chain drops the lot.
            storm_motion: (|| {
                let source = proto::field(data, proto::STORM_MOTION)
                    .and_then(|v| v.as_f64())
                    .and_then(|v| offload::StormMotionWire::from_wire_code(v as u8))?
                    .0;
                let speed_kt =
                    proto::field(data, proto::STORM_MOTION_SPEED).and_then(|v| v.as_f64())? as f32;
                let direction_deg =
                    proto::field(data, proto::STORM_MOTION_DIR).and_then(|v| v.as_f64())? as f32;
                Some(rustdar_radar::srv::SrvMotion {
                    speed_kt,
                    direction_deg,
                    source,
                })
            })(),
        })
    });

    // A frame and an `OUT` payload are mutually exclusive on the wire; `or_else`
    // rather than a branch so a message carrying both — which only a build
    // mismatch the token check already refuses could produce — still resolves to
    // exactly one output rather than to a pair somebody has to arbitrate.
    offload::deliver_job_reply(id as u64, frame.or_else(|| decode_out(data)));
}

/// The non-frame half of a reply: one transferred `Uint8Array` plus the tag
/// saying which decoder owns it.
///
/// `None` for anything this build cannot read — a missing payload, a kind byte
/// it does not have, or bytes the payload type's own codec refuses. All three
/// are "nothing to draw", which is what a failed render has always been, and
/// all three still deliver: the caller's slot is released either way.
/// The decoding itself is [`offload::decode_output`], in the frontend beside
/// [`offload::execute_bytes`] — so this crate stays the browser adapter and the
/// payload codecs are reachable from a host test rather than only from a
/// browser.
fn decode_out(data: &JsValue) -> Option<offload::JobOutput> {
    let out = proto::field(data, proto::OUT).filter(|v| !v.is_null())?;
    let kind = proto::field(data, proto::OUT_KIND)
        .and_then(|v| v.as_f64())
        .map(|v| v as u8)?;
    offload::decode_output(kind, &js_sys::Uint8Array::new(&out).to_vec())
}

/// The installed port. Owns the `Worker` handle, so the worker lives exactly as
/// long as the funnel is willing to send it jobs.
struct Port {
    worker: web_sys::Worker,
}

impl JobSink for Port {
    /// # The serialisation lives here, and nowhere above here
    ///
    /// A `JobRequest` is not a thing a `Worker` can be handed: the only payload
    /// a `postMessage` transfer list moves is a detachable `ArrayBuffer`, so
    /// this arm turns the request into bytes on its way out. That is the
    /// browser's charge for handover and it is charged where it is incurred —
    /// the funnel calls `send(id, request)` and names no representation, so a
    /// transport that can move an owned value pays none of this.
    ///
    /// `to_bytes` borrows, so a failed post still owns the request and hands it
    /// back for the funnel to run inline.
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        let message = js_sys::Object::new();
        proto::set_field(&message, proto::KIND, &JsValue::from_str(proto::JOB));
        proto::set_field(&message, proto::ID, &JsValue::from_f64(id as f64));

        // Copied out of linear memory once, then transferred: the request is
        // one radar sweep, ~1.3 MB for an 8-bit moment and more for NROT, and
        // structured-cloning it would copy it a second time on arrival.
        let payload = js_sys::Uint8Array::from(request.to_bytes().as_slice());
        let transfer = js_sys::Array::new();
        transfer.push(&payload.buffer());
        proto::set_field(&message, proto::REQUEST, &payload);

        match self.worker.post_message_with_transfer(&message, &transfer) {
            Ok(()) => Ok(()),
            Err(e) => {
                // The funnel runs the job here instead. A sink that keeps
                // refusing is a worker that has died, and `onerror` retires it.
                log::warn!("could not post job {id} to the worker: {e:?}");
                Err(request)
            }
        }
    }
}
