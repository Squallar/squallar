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

/// Type alias for a reference-counted Window
pub type WindowRef = Arc<Window>;

pub mod app;
pub mod app_state;
pub mod channels;
pub mod constants;
pub mod egui_renderer;
pub mod input;
pub mod loop_downloads;
pub mod platform;
pub mod render_dispatch;
