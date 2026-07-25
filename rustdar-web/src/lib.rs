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
//! [`PlatformBridge`]: rustdar_frontend::platform::PlatformBridge

pub mod config_store;
pub mod geolocation;

#[cfg(target_arch = "wasm32")]
pub mod bridge;

#[cfg(target_arch = "wasm32")]
mod entry;

#[cfg(target_arch = "wasm32")]
pub use entry::start;
