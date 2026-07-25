#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! rustdar in the browser: wasm32 + WebGL2.
//!
//! This crate is to the browser what `rustdar-platform` is to the desktop — the
//! entry point, the concrete [`PlatformBridge`] and the capabilities that bridge
//! exposes. Everything visible on the page belongs to `rustdar-frontend`.
//!
//! # WebGL2, not WebGPU
//!
//! Firefox has no stable WebGPU. A build that took WebGPU where it was
//! available would run one rendering path in Chrome and another in Firefox off
//! the same binary, and only one of them would ever be exercised during
//! development. `rustdar_frontend::app` pins `Backends::GL` on wasm32 so both
//! browsers run the path that was actually tested.
//!
//! # Building and running
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
//! as a `file://` URL — wasm instantiation and the tile fetches both need a real
//! origin.
//!
//! Build `--release` before judging performance. Workspace code is `opt-level =
//! 0` in the dev profile, and radar rasterization is roughly 2.5x slower there:
//! measured in Firefox, 2349 ms per Level II frame dev against 899-962 ms
//! release. Chromium runs the same release job in 162 ms.
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
