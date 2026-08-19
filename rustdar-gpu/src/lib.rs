#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The wgpu boundary of rustdar.
//!
//! The one egui/wgpu renderer every target — desktop, web (wasm32 + WebGL2),
//! Android and iOS — draws through: frame prepare/submit ([`egui_renderer`]),
//! the banded texture upload path, the pane mirror, the staging ring
//! ([`staging_ring`]), and the device-request policy ([`device`]).
//!
//! What deliberately does NOT live here: the app loop, the window/surface
//! lifecycle (rustdar-frontend's `AppState` spans surface, renderer and
//! volume support and stays above), and the 3D volume stack (rustdar-volumetric
//! depends on this crate, never the reverse — a dev-dep back from here onto it
//! arrives with the GPU test suite at WO-RV and is legal because dev-deps
//! never enter the normal graph).

/// The device-request policy: which surface format, which limits, which
/// present mode — the forks that are silent when they go the wrong way.
pub mod device;
pub mod egui_renderer;
pub mod staging_ring;

/// Type alias for a reference-counted Window.
///
/// Duplicated from rustdar-frontend deliberately — two type aliases to the
/// same type are the same type, and the alternative is this crate reaching up
/// into the app crate for a name.
pub type WindowRef = std::sync::Arc<winit::window::Window>;
