#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! rustdar in the browser: wasm32 + WebGL2.
//!
//! The entry point, the concrete [`PlatformBridge`] and the capabilities that
//! bridge exposes. Everything visible on the page belongs to `rustdar-app`.
//!
//! WebGL2, not WebGPU: Firefox has no stable WebGPU. `rustdar_app::app` pins
//! `Backends::GL` on wasm32.
//!
//! ```text
//! cd rustdar-web
//! wasm-pack build --target web --release      # writes ./pkg
//! python3 -m http.server 8731                 # or any static server
//! # then open http://127.0.0.1:8731/index.html
//! ```
//!
//! Measure in `--release`: workspace code is `opt-level = 0` in dev and radar
//! rasterization is ~2.5x slower there.
//!
//! **There is no Firefox/Chromium gap on `radar-render`.** Against the same
//! archived sweep, Chromium (headless, SwiftShader) medians 191 ms and Firefox
//! (headed, NVIDIA) 188 ms; the 12 interleaved pairs give a median
//! Firefox/Chromium ratio of **0.88**. The GPUs are deliberately unmatched,
//! which does not touch this — `radar-render` is the CPU-side rasterizer with
//! no GPU call in the timed region.
//!
//! [`PlatformBridge`]: rustdar_app::platform::PlatformBridge

pub mod kv;

/// How long the page waits before starting another rasterization worker.
///
/// Not wasm32-gated: the ladder is pure arithmetic, `worker_port` is gated, and
/// a retry policy only a browser can walk is one no test ever checks.
pub mod worker_retry;

#[cfg(target_arch = "wasm32")]
pub mod bridge;

#[cfg(target_arch = "wasm32")]
mod entry;

/// The rasterization worker. `worker` is the module `worker.js` boots inside a
/// dedicated Worker; `worker_port` is the page's half, which installs it into
/// `rustdar_worker::offload`.
#[cfg(target_arch = "wasm32")]
mod worker;
#[cfg(target_arch = "wasm32")]
mod worker_port;
/// Public for the Tier-1 browser gate (`tests/tier1_wasm.rs`).
#[cfg(target_arch = "wasm32")]
pub mod worker_protocol;

#[cfg(target_arch = "wasm32")]
pub use entry::start;
#[cfg(target_arch = "wasm32")]
pub use worker::rustdar_worker_main;

#[cfg(test)]
mod entry_probes {
    /// `entry::start` used to open a channel and call the geolocation watch before
    /// the first frame, so the browser's permission dialog appeared on first paint
    /// with no user gesture. `watchPosition` *is* the prompt on this platform.
    #[test]
    fn nothing_asks_the_browser_for_a_position_at_page_load() {
        let entry = include_str!("entry.rs");
        let start = entry
            .find("pub fn start()")
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
