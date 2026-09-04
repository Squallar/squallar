#![warn(clippy::all)]
#![forbid(unsafe_code)]
// The alloc-error hook (`alloc_failure::hook`) is nightly-only, and the
// wasm build is the one build on nightly (`.github/scripts/wasm-threads.sh`):
// a `cfg_attr` selects the feature for that target and leaves the host
// build, which never installs the hook, on stable.
#![cfg_attr(target_arch = "wasm32", feature(alloc_error_hook))]

//! squallar in the browser: wasm32, WebGPU with a WebGL2 fallback.
//!
//! The entry point, the concrete [`PlatformBridge`] and the capabilities that
//! bridge exposes. Everything visible on the page belongs to `squallar-app`.
//!
//! `squallar_app::app` asks for `BROWSER_WEBGPU | GL` on wasm32 and lets wgpu's
//! detecting constructor settle it with a real `requestAdapter()`. On
//! Firefox/Linux — the browser that governs here — WebGPU is still unshipped, so
//! what actually renders is WebGL2, exactly as before. What WebGPU adds is
//! reach, not speed: a Chromium whose driver is blocklisted answers WebGL2 with
//! SwiftShader. The startup log names the backend that answered.
//!
//! ```text
//! .github/scripts/wasm-threads.sh \
//!   wasm-pack build squallar-web --target web --release   # writes ./pkg
//! .github/browser-rig/serve.py --dir squallar-web --coep  # NOT http.server
//! ```
//!
//! Both halves of that are load-bearing since WS3b. The build must go through
//! `wasm-threads.sh`: this crate needs `+atomics`, a std rebuilt against it,
//! and a shared imported memory, and a bare `wasm-pack build` fails to compile
//! rather than quietly producing a single-threaded bundle. The server must
//! send COOP/COEP: without cross-origin isolation there is no
//! `SharedArrayBuffer`, so `squallar_web::rayon_pool` falls back to one thread
//! and the app runs — just at the speed the table below calls "before".
//! `python3 -m http.server` sends no such headers. Production does (CloudFront
//! Response Headers Policy on squallar.app).
//!
//! Measure in `--release`: workspace code is `opt-level = 0` in dev and radar
//! rasterization is ~2.5x slower there.
//!
//! **What the rayon pool bought, measured.** Tier-2 rig, this box, arms
//! INTERLEAVED (before, after, before, after) rather than run as two
//! campaigns, reading `deliver_job_reply`'s own "<kind> took <N> ms off the
//! frame". Medians, n=4 per cell, per browser and never pooled across them —
//! Chromium is on SwiftShader and Firefox on Xvfb/llvmpipe here:
//!
//! ```text
//!            before (1 thread)   after (8 threads)
//!   decode   Firefox   476 ms    126 ms     3.78x
//!            Chromium  474 ms    125 ms     3.78x
//!   radar    Firefox   136 ms     47 ms     2.92x
//!            Chromium  178 ms     74 ms     2.41x
//! ```
//!
//! The figure is END-TO-END job latency, not rasterizer CPU alone: it is
//! `job.started.elapsed()`, so it carries queueing and the postMessage
//! transport with it. The `alerts` and `discussions` kinds also moved, and
//! nothing here claims those: their time is dominated by network fetch.
//!
//! **What the shared loan bought, measured (WS3c).** The reply used to be
//! copied TWICE — once out of the worker's linear memory by
//! `Uint8Array::from`, once into the page's by `to_vec` — with the transfer
//! list moving the middle buffer between them. When the browser is
//! cross-origin isolated the worker now posts VIEWS onto its own
//! `SharedArrayBuffer` instead ([`shared_loan`]), so the first copy is gone and
//! the page's is the only one left. Same in the request direction.
//!
//! The figure is BYTES, not milliseconds, because bytes are what the change
//! moves and the page counts them first-hand (`worker_port::account`, reported
//! by the rig as `transport_bytes`). One Tier-2 pass per browser, `--skip-build`
//! on this box, one KTLX pane, the rig's default 10 s data window — a whole-run
//! total over the replies that arrived in it, NOT a per-frame figure and not a
//! median of anything:
//!
//! ```text
//!                       replies   B out       copied out of the worker
//!   isolated  Firefox     6       106798231            0
//!             Chromium    6       104125591            0
//!   not       Firefox     6       106798231    106798231
//!   isolated  Chromium    8       148243981    148243981
//! ```
//!
//! Firefox's two arms carried the SAME payload to the byte over the same reply
//! count, which is what makes its row a like-for-like reading of exactly what
//! was removed: ~17.8 MB per reply. Chromium's arms answered 6 and 8 replies,
//! so its totals are not comparable across rows and only the zero is.
//!
//! The "not isolated" rows are the same build served without COOP/COEP — the
//! GitHub Pages posture, still supported and still correct, just copying. They
//! are also the Tier-2 negative control: `drive.py --expect-zero-copy-replies`
//! passes on the rows above and fails on the rows below, while
//! `--expect-worker-round-trip` passes on all four. Nothing here claims a job
//! latency: the copy is a fraction of a job that also fetches, decodes and
//! rasterizes, and the rig's per-kind medians on a single pass are n=1..2.
//!
//! **There is no Firefox/Chromium gap on `radar-render`.** A separate and
//! narrower instrument, kept because the table above does not answer the same
//! question: against the same archived sweep, Chromium (headless, SwiftShader)
//! medians 191 ms and Firefox (headed, NVIDIA) 188 ms, and the 12 interleaved
//! pairs give a median Firefox/Chromium ratio of **0.88**. The GPUs are
//! deliberately unmatched, which does not touch it — `radar-render` is the
//! CPU-side rasterizer with no GPU call in the timed region. Predates WS3b and
//! so describes the single-threaded arm; the per-browser split in the table
//! above is over end-to-end job latency on unmatched software renderers and is
//! not a restatement of it.
//!
//! [`PlatformBridge`]: squallar_app::platform::PlatformBridge

pub mod kv;

/// How long the page waits before starting another rasterization worker.
///
/// Not wasm32-gated: the ladder is pure arithmetic, `worker_port` is gated, and
/// a retry policy only a browser can walk is one no test ever checks.
pub mod worker_retry;

/// The device's form factor from pointer media and touch points. Not
/// wasm32-gated for `worker_retry`'s reason: the classifier is pure and its
/// truth table runs on the host; only the reads in `bridge` are gated.
pub mod form_factor;

/// The browser's per-tab WebGPU allowance, probed by allocating until refused.
/// Not wasm32-gated for the same reason: the doubling plan, the texture
/// shapes and the stopping rules are pure and host-tested; only `gpu_probe::run`,
/// which holds a device, is gated.
pub mod gpu_probe;

#[cfg(target_arch = "wasm32")]
pub mod bridge;

#[cfg(target_arch = "wasm32")]
mod entry;

/// The rasterization worker. `worker` is the module `worker.js` boots inside a
/// dedicated Worker; `worker_port` is the page's half, which installs it into
/// `squallar_worker::offload`.
#[cfg(target_arch = "wasm32")]
mod worker;
#[cfg(target_arch = "wasm32")]
mod worker_port;

/// Where rayon gets its threads. Public because `worker.js` calls into it
/// before it will accept a job.
#[cfg(target_arch = "wasm32")]
pub mod rayon_pool;

/// Lending the peer a view onto this instance's memory instead of a copy of it.
///
/// Not wasm32-gated, and deliberately: the ownership protocol — which region
/// may be freed, and when — is plain bookkeeping, and it is the half a bug
/// would put a recycled raster on the screen through. It is tested on the host.
/// Only the part that builds `Uint8Array` views is gated.
pub mod shared_loan;

/// What an allocation failure says before the instance aborts. Not
/// wasm32-gated for `shared_loan`'s reason: the line is pure and host-tested;
/// only the hook that reads the instance's memory is gated.
pub mod alloc_failure;

/// **What ceiling this instance's linear memory was constructed with**, as JS
/// decided it per device before the module existed. Not wasm32-gated: it is
/// two atomics and a truth about them, and the host tests read them.
pub mod heap_max;

/// `initThreadPool`, the JS half of `wasm-bindgen-rayon`'s pool. Re-exported
/// because wasm-bindgen only emits a binding for an export this crate names:
/// the symbol is defined in the dependency, and without this line `worker.js`
/// would import a function `pkg/squallar_web.js` does not have.
#[cfg(target_arch = "wasm32")]
pub use wasm_bindgen_rayon::init_thread_pool;
/// Public for the Tier-1 browser gate (`tests/tier1_wasm.rs`).
#[cfg(target_arch = "wasm32")]
pub mod worker_protocol;

#[cfg(target_arch = "wasm32")]
pub use entry::start;
#[cfg(target_arch = "wasm32")]
pub use worker::squallar_worker_main;

#[cfg(test)]
mod entry_probes {
    /// `entry::start` used to open a channel and call the geolocation watch before
    /// the first frame, so the browser's permission dialog appeared on first paint
    /// with no user gesture. `watchPosition` *is* the prompt on this platform.
    #[test]
    fn nothing_asks_the_browser_for_a_position_at_page_load() {
        let entry = include_str!("entry.rs");
        // `pub fn start(` and not `pub fn start()`: the entry point takes the
        // two per-device linear-memory ceilings JS chose before the module
        // existed (`crate::heap_max`). A search for the empty parameter list
        // would silently find nothing and pass vacuously.
        let start = entry
            .find("pub fn start(")
            .map(|i| &entry[i..])
            .expect("entry::start is gone");
        for asked in ["watch_position", "start_watch", "request_location"] {
            assert!(
                !start.contains(asked),
                "the browser entry point calls {asked} at boot, so the page \
                 prompts for location on first paint with no user gesture"
            );
        }
    }
}
