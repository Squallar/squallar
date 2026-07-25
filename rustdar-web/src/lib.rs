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
//! 0` in the dev profile, and radar rasterization is roughly 2.5x slower there —
//! 2349 ms per Level II frame against a release figure in the 160-190 ms range.
//!
//! # The Firefox/Chromium gap on `radar-render`: there isn't one
//!
//! This section used to record Firefox at 899-962 ms per Level II frame against
//! Chromium's 162 ms — a 5.7x penalty — and treat it as the crate's open
//! performance question. **It does not reproduce.**
//!
//! Measured in the assembled bundle, both browsers driving this page against the
//! *same* archived sweep (`KTLX20260725_191018_V06`, 9 067 340 bytes), release,
//! `IMAGE_SIZE` 1024:
//!
//! | browser                          |  n | min    | median |
//! |----------------------------------|---:|-------:|-------:|
//! | Chromium (headless, SwiftShader) | 19 | 174 ms | 191 ms |
//! | Firefox (headed on Xvfb, NVIDIA) | 10 | 159 ms | 183 ms |
//!
//! Run as interleaved pairs, so each comparison shares a contention window, the
//! median Firefox/Chromium ratio is **0.88**. Firefox is slightly *faster* —
//! which is what the isolated harness had already found (233 ms against 261 ms)
//! and was disbelieved because the assembled app seemed to say otherwise. It
//! does not. There is no Firefox-specific penalty in this bundle to fix.
//!
//! Two things are needed to get a number that means anything, and the original
//! measurement had neither.
//!
//! **Pin the input.** The app loads whichever volume is newest, so two browsers
//! started minutes apart rasterize two different sweeps with different gate
//! counts. Comparing across them measures the weather. These runs went through a
//! caching proxy, so the second browser replayed the first one's bytes.
//!
//! **Watch the machine.** These runs shared a 32-core box with an unrelated
//! build fleet at load 26-72, and single samples there spanned 174-508 ms in
//! *Chromium alone* — a wider spread than the effect being chased. Minima and
//! matched pairs are the figures that survive that; one timing off a busy
//! machine is not evidence, and a 5.7x built from two of them is most likely
//! just the load at the two moments they were taken.
//!
//! Still true, and the actual place the frame goes on *both* browsers:
//! `rustdar_radar::types::lat_rad_to_mercator_y`. See `rustdar_radar::render`'s
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
