#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! rustdar in the browser: wasm32 + WebGL2.
//!
//! This crate is to the browser what `rustdar-platform` is to the desktop — the
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
//! 9 067 340 bytes, release, `IMAGE_SIZE` 1024):
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
//! `rustdar_radar::types::lat_rad_to_mercator_y`; see `rustdar_radar::render`'s
//! `RenderBuffers`.
//!
//! [`PlatformBridge`]: rustdar_frontend::platform::PlatformBridge

pub mod config_store;
pub mod geolocation;

#[cfg(target_arch = "wasm32")]
pub mod bridge;

#[cfg(target_arch = "wasm32")]
mod entry;

/// The rasterization worker. `worker` is the module `worker.js` boots inside a
/// dedicated Worker; `worker_port` is the page's half, which installs it into
/// `rustdar_frontend::offload`. Both are the same wasm module, instantiated
/// twice — see [`worker`] for why.
#[cfg(target_arch = "wasm32")]
mod worker;
#[cfg(target_arch = "wasm32")]
mod worker_port;
#[cfg(target_arch = "wasm32")]
mod worker_protocol;

#[cfg(target_arch = "wasm32")]
pub use entry::start;
#[cfg(target_arch = "wasm32")]
pub use worker::rustdar_worker_main;
