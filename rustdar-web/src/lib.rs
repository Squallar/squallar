#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! rustdar in the browser: wasm32 + WebGL2.
//!
//! This crate is to the browser what the `rustdar` crate is to the desktop — the
//! entry point, the concrete [`PlatformBridge`] and the capabilities that bridge
//! exposes. Everything visible on the page belongs to `rustdar-frontend`.
//!
//! WebGL2, not WebGPU: Firefox has no stable WebGPU, and taking WebGPU where it
//! exists would give the same binary two rendering paths with only one ever
//! exercised. `rustdar_frontend::app` pins `Backends::GL` on wasm32.
//!
//! ```text
//! cd rustdar-web
//! wasm-pack build --target web --release      # writes ./pkg
//! python3 -m http.server 8731                 # or any static server
//! # then open http://127.0.0.1:8731/index.html
//! ```
//!
//! `--target web` is required: `index.html` loads `./pkg/rustdar_web.js` as an
//! ES module and calls the exported [`start`]. It must be *served*, not opened
//! as `file://`.
//!
//! # Performance
//!
//! Measure in `--release`. Workspace code is `opt-level = 0` in dev and radar
//! rasterization is ~2.5x slower there: 2349 ms per Level II frame against
//! 160-190 ms.
//!
//! **There is no Firefox/Chromium gap on `radar-render`.** An earlier claim of
//! 899-962 ms in Firefox against 162 ms in Chromium (5.7x) does not reproduce.
//! Both browsers against the same archived sweep (`KTLX20260725_191018_V06`,
//! 9 067 340 bytes, release, at the 1024 `IMAGE_SIZE` the web arm had when
//! these were taken — a static web render is 2048 now, so the absolute figures
//! below are a floor and the browser-to-browser comparison is the finding):
//!
//! | browser                          |  n | min    | median |
//! |----------------------------------|---:|-------:|-------:|
//! | Chromium (headless, SwiftShader) | 23 | 174 ms | 191 ms |
//! | Firefox (headed on Xvfb, NVIDIA) | 14 | 159 ms | 188 ms |
//!
//! The controlled comparison is the 12 runs that went as **interleaved pairs**,
//! one browser straight after the other so both meet the same contention: there
//! the median Firefox/Chromium ratio is **0.88**. Firefox is slightly faster,
//! matching the isolated harness (233 vs 261 ms).
//!
//! The GPUs are deliberately unmatched — headless Firefox has no WebGL2 here, so
//! Chromium ran headless on SwiftShader and Firefox headed on the host NVIDIA.
//! That would wreck a comparison of frame *presentation* and does not touch this
//! one: `radar-render` is the CPU-side rasterizer, it runs to a `Vec<u8>` on the
//! main thread with no GPU call in the timed region. It also cuts *against* the
//! conclusion — the browser on real hardware is the flattered one, and it is the
//! one already winning.
//!
//! The original number was taken without pinning the input (the app loads
//! whichever volume is newest, so two browsers started minutes apart rasterize
//! different sweeps) and on a 32-core box at load 26-72, where single samples
//! spanned 174-508 ms in Chromium alone. Pin the volume and interleave the runs
//! before quoting a ratio.
//!
//! The hot spot on both browsers is
//! `rustdar_geo::lat_rad_to_mercator_y`; see `rustdar_radar::render`'s
//! `RenderBuffers`.
//!
//! [`PlatformBridge`]: rustdar_frontend::platform::PlatformBridge

// `geolocation` moved to `rustdar_location::web` at WO-RL-4 (seam ruling 6:
// every remote location arm lives in the facade); `entry` hands the app a
// `WebBackend` inside its LocationFacade.
pub mod kv;

/// How long the page waits before starting another rasterization worker.
///
/// Not wasm32-gated, for the reason `kv` is not:
/// the ladder is pure arithmetic, `worker_port` is gated, and a retry policy
/// that only a browser can walk is a retry policy no test ever checks.
pub mod worker_retry;

#[cfg(target_arch = "wasm32")]
pub mod bridge;

#[cfg(target_arch = "wasm32")]
mod entry;

/// The rasterization worker. `worker` is the module `worker.js` boots inside a
/// dedicated Worker; `worker_port` is the page's half, which installs it into
/// `rustdar_worker::offload`. Both are the same wasm module, instantiated
/// twice — see [`worker`] for why.
#[cfg(target_arch = "wasm32")]
mod worker;
#[cfg(target_arch = "wasm32")]
mod worker_port;
/// Public for the Tier-1 browser gate (`tests/tier1_wasm.rs`), which pins the
/// build-token compare against real JS values through the same Reflect
/// helpers `worker_port::handle_message` reads with. Still wasm-gated, so
/// nothing native gains a surface.
#[cfg(target_arch = "wasm32")]
pub mod worker_protocol;

#[cfg(target_arch = "wasm32")]
pub use entry::start;
#[cfg(target_arch = "wasm32")]
pub use worker::rustdar_worker_main;

#[cfg(test)]
mod entry_probes {
    // Relocated from the geolocation module when the browser arm moved to
    // `rustdar_location::web` (WO-RL-4). Lives HERE, in the ungated crate
    // root, because `entry` itself is wasm-gated and a probe inside it would
    // never compile on the host — where every test in this crate runs.

    /// **The audited defect, pinned at the one place it was written.**
    ///
    /// `entry::start` used to open a channel and call the geolocation watch on
    /// it before the first frame, so the browser's permission dialog appeared
    /// on first paint, with no user gesture, before the page had shown the
    /// user anything. `watchPosition` *is* the prompt on this platform: there
    /// is no way to start a watch quietly, which is why the only defence is
    /// not calling it — and why the assertion is about the entry point's
    /// source rather than about a flag somebody could set.
    ///
    /// The prompt now happens from the facade's web arm
    /// (`rustdar_location::web::WebBackend::request`), which the gate reaches
    /// only from a state that licenses one. Constructing the backend is fine —
    /// its permission QUERY prompts nobody — so `WebBackend::new` is not on
    /// the needle list; the verbs that subscribe are.
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
