#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The portable half of rustdar.
//!
//! Everything here is shared by every target: the winit application handler,
//! the wgpu/egui renderer, the fetch and render dispatch, and the app state
//! they operate on. The per-OS entry points and the concrete
//! [`platform::PlatformBridge`] implementations live in `rustdar-platform`,
//! which depends on this crate — never the other way round.

use std::sync::Arc;
use winit::window::Window;

/// Process-wide TLS setup, re-exported so entry points can install the crypto
/// provider without taking their own dependency on `rustdar-radar`.
///
/// See [`rustdar_radar::tls`] for why the provider has to be installed at all
/// and why calling it from an entry point is not the load-bearing guarantee.
pub use rustdar_radar::tls;

/// Type alias for a reference-counted Window
pub type WindowRef = Arc<Window>;

pub mod app;
pub mod app_state;
pub mod channels;
pub mod chunk_feed;
pub mod chunk_notify;
pub mod constants;
pub mod egui_renderer;
pub mod input;
pub mod location_hint;
pub mod location_permission;
pub mod loop_downloads;
pub mod loop_pool;
/// The rule behind the `mobile` cfg. Compiled only for tests — the production
/// copy is `include!`d by `build.rs`, which runs before this crate exists.
#[cfg(test)]
mod mobile_cfg;
pub mod offload;
pub mod platform;
/// The [`PlatformBridge`](platform::PlatformBridge) test double. Compiled only
/// for tests; it is the seam every `App`-level test is driven through.
#[cfg(test)]
pub(crate) mod platform_double;
pub mod render_dispatch;
pub mod site_catalogue;
/// The radars this crate's tests run against. See the module note for why
/// there is exactly one such list.
#[cfg(test)]
pub(crate) mod test_sites;
pub mod site_positions;
pub mod volume;
