#![warn(clippy::all)]
#![forbid(unsafe_code)]

use std::sync::Arc;
use winit::window::Window;

/// Type alias for a reference-counted Window
pub type WindowRef = Arc<Window>;

pub mod app;
pub mod app_state;
pub mod channels;
pub mod config_store;
pub mod constants;
pub mod egui_renderer;
pub mod input;
pub mod loop_downloads;
pub mod platform;
pub mod render_dispatch;
pub mod run;

pub use crate::run::run;
