#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The portable half of squallar.

use std::sync::Arc;
use winit::window::Window;

/// Process-wide TLS setup, re-exported so entry points can install the crypto provider
/// without taking their own dependency on `squallar-radar`.
pub use squallar_radar::tls;

pub type WindowRef = Arc<Window>;

pub mod app;

/// Route the browser frame pump's basemap tiles through the rasterization
/// worker. `squallar-web` calls this once at startup; `app::fetch` is
/// `pub(crate)`, so the one item that crosses is named here rather than the
/// module being opened up for it.
#[cfg(target_arch = "wasm32")]
pub use app::fetch::install_tile_offloader;

pub mod app_state;
#[cfg(test)]
pub(crate) mod budget_arms;
pub mod budget_memo;
pub(crate) mod budget_telemetry;
pub mod channels;
pub(crate) mod frame_ledger;
pub mod input;
pub mod location_hint;
pub mod loop_pool;
pub(crate) mod loop_refill;
pub(crate) mod loop_telemetry;
pub mod platform;
/// The [`PlatformBridge`](platform::PlatformBridge) test double.
#[cfg(test)]
pub(crate) mod platform_double;
pub mod render_dispatch;
pub mod render_key;
pub mod site_catalogue;
pub mod site_positions;
#[cfg(test)]
pub(crate) mod test_keys;
/// The radars this crate's tests run against.
#[cfg(test)]
pub(crate) mod test_sites;
/// The `Ready` volume fixture the app-side store/release tests stand a pane on.
#[cfg(test)]
pub(crate) mod volume_fixture;
pub(crate) mod volume_inventory;
