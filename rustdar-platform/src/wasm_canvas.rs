//! WASM-specific canvas utilities for viewport and canvas management.

use crate::WindowRef;
use crate::constants::{RENDER_HEIGHT, RENDER_WIDTH};
use winit::dpi::PhysicalSize;
use winit::platform::web::WindowExtWebSys;

/// Returns viewport dimensions in physical pixels, or fallback dimensions if unavailable.
///
/// This queries the browser window for its inner dimensions (in CSS pixels),
/// then multiplies by device pixel ratio to get physical pixels for rendering.
pub fn get_viewport_dimensions() -> (u32, u32) {
    if let Some(web_window) = web_sys::window() {
        let logical_w = web_window
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(RENDER_WIDTH as f64);
        let logical_h = web_window
            .inner_height()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(RENDER_HEIGHT as f64);
        let dpr = web_window.device_pixel_ratio();

        let w = (logical_w * dpr) as u32;
        let h = (logical_h * dpr) as u32;
        (w, h)
    } else {
        (RENDER_WIDTH, RENDER_HEIGHT)
    }
}

/// Apply CSS styles to canvas element to make it fill the viewport.
///
/// This overrides winit's default inline styles which can constrain the canvas.
/// The styles ensure the canvas stretches to 100% of its container.
pub fn apply_canvas_styles(canvas: &web_sys::HtmlCanvasElement) {
    let _ = canvas.style().set_property("width", "100%");
    let _ = canvas.style().set_property("height", "100%");
    let _ = canvas.style().set_property("max-width", "none");
    let _ = canvas.style().set_property("max-height", "none");
}

/// Resize canvas to match viewport dimensions and update window size.
///
/// This is used both during initial window setup and when handling resize events.
/// It queries the browser viewport, updates the canvas element's pixel dimensions,
/// applies CSS styling, and notifies winit of the new size.
pub fn resize_canvas_to_viewport(window: &WindowRef) {
    let (width, height) = get_viewport_dimensions();

    if let Some(canvas) = window.canvas() {
        canvas.set_width(width);
        canvas.set_height(height);
        apply_canvas_styles(&canvas);
    }

    let _ = window.request_inner_size(PhysicalSize::new(width, height));
}
