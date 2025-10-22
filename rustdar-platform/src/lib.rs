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
pub mod texture_manager;
#[cfg(target_arch = "wasm32")]
pub mod wasm_canvas;
pub mod world;

pub use crate::run::run;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

// Export the run function for WASM
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn start() {
    wasm_bindgen_futures::spawn_local(run::run());
}
