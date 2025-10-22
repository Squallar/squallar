#![warn(clippy::all)]
#![forbid(unsafe_code)]

use std::sync::Arc;
use winit::window::Window;

/// Type alias for a reference-counted Window
pub type WindowRef = Arc<Window>;

pub mod app;
pub mod app_state;
pub mod constants;
pub mod egui_renderer;
pub mod input;
pub mod run;

pub use crate::run::run;
