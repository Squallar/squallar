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
//! release, `IMAGE_SIZE` 1024) give median 188 ms Firefox / 191 ms Chromium, and
//! across 12 interleaved pairs the median Firefox/Chromium ratio is 0.88 —
//! Firefox is slightly faster, matching the isolated harness (233 vs 261 ms).
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

#[cfg(target_arch = "wasm32")]
pub use entry::start;
