//! Where rayon gets its threads in the browser.
//!
//! `rustdar_radar::par` is rayon on every target as of WS3b, so *something* has
//! to have called `build_global` before the first `par_iter` on this thread's
//! pool — rayon panics on a global pool it could not initialize, and
//! `std::thread::spawn` on wasm32-unknown-unknown returns `Unsupported`. The
//! threads have to come from Web Workers, which only JS can create.
//!
//! Two arms, and they are deliberately not the same:
//!
//! * The **rasterization worker** takes the real pool. `wasm-bindgen-rayon`'s
//!   `initThreadPool` (re-exported from `crate`, called by `worker.js`) spawns
//!   [`threads`] nested Workers over that worker's own shared linear memory.
//!   Every heavy CPU path the app has is already offloaded there by design, so
//!   this is where the whole win is.
//! * The **page** takes [`install_serial_pool`], which spawns nothing. It is a
//!   real rayon global pool of exactly one thread — the calling thread — so
//!   `par_iter` on the main thread runs inline, which is precisely what the
//!   deleted `par::seq` stand-ins did. The main thread only reaches a rayon
//!   path through `offload_job`'s inline fallback (the handshake window, or a
//!   worker that will not come up), and that path stays as fast and as
//!   frame-safe as it is today. Spinning the frame thread against a pool of
//!   Workers would trade interaction latency for data latency.
//!
//! [`install_serial_pool`] is also what the worker falls back to when
//! `initThreadPool` rejects — a browser that refuses nested Workers or has no
//! `SharedArrayBuffer` still rasterizes, at today's speed, instead of panicking
//! on an uninitialized pool.

use wasm_bindgen::prelude::*;

/// Threads to ask for in the rasterization worker.
///
/// Reads `navigator.hardwareConcurrency` through `Reflect` rather than through
/// a `web-sys` binding because the worker's global is a `WorkerNavigator` and
/// the page's is a `Navigator`; the property is the same on both and this way
/// the crate does not carry a feature for each. A global that will not name a
/// count reads as [`DEFAULT_THREADS`].
///
/// Clamped to [`rustdar_device_profile::constants::WASM_MAX_RAYON_THREADS`],
/// which is the memory budget: every rayon thread costs a stack inside the one
/// shared linear memory, and `hardwareConcurrency` on a big desktop is a number
/// this module has no business believing.
#[wasm_bindgen(js_name = rustdarRayonThreads)]
pub fn threads() -> usize {
    let reported = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("navigator"))
        .ok()
        .and_then(|nav| js_sys::Reflect::get(&nav, &JsValue::from_str("hardwareConcurrency")).ok())
        .and_then(|n| n.as_f64())
        .filter(|n| n.is_finite() && *n >= 1.0)
        .map_or(DEFAULT_THREADS, |n| n as usize);

    reported.clamp(1, rustdar_device_profile::constants::WASM_MAX_RAYON_THREADS)
}

/// What a global that will not report `hardwareConcurrency` is worth. Every
/// browser this app supports reports it; the constant is the shape of the
/// `Option`, not a measurement.
const DEFAULT_THREADS: usize = 4;

/// Give this thread a rayon global pool that spawns nothing.
///
/// `num_threads(1)` plus `use_current_thread()` builds a pool whose only worker
/// is the caller, so `build_global` needs no `std::thread::spawn` — the call
/// wasm32-unknown-unknown cannot serve. Work submitted to it runs inline on the
/// submitting thread.
///
/// Idempotent in effect, not in fact: `build_global` fails if a global pool is
/// already installed, and that failure is the success case for a second call.
/// Logged at debug, never propagated.
#[wasm_bindgen(js_name = rustdarRayonSerialPool)]
pub fn install_serial_pool() {
    match rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .use_current_thread()
        .build_global()
    {
        Ok(()) => log::debug!("rayon: this thread is its own pool"),
        Err(e) => log::debug!("rayon: a global pool is already installed ({e})"),
    }
}
