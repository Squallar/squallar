//! The browser entry point; the counterpart of `rustdar_platform::run::run`.

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::web::EventLoopExtWebSys;

/// The id of the `<canvas>` in `index.html` this app renders into.
const CANVAS_ID: &str = "rustdar-canvas";

/// Boot rustdar into the canvas named by [`CANVAS_ID`]. Exported to JS and
/// called by `index.html` once the DOM exists.
///
/// Not `run_app`: it never returns, and winit's web backend implements that
/// signature by throwing a JS exception to unwind out of Rust, so every caller
/// sees an exception. [`EventLoopExtWebSys::spawn_app`] returns normally.
///
/// `Wait`, not `Poll`: `Poll` schedules through winit's web scheduler
/// (`Scheduler.postTask`), whose teardown via `AbortController.abort()` is a
/// large fraction of Firefox main-thread time — all wasted for an app that only
/// redraws on change. Every async completion here ends in `notify_redraw`, so
/// nothing depends on an unbidden frame.
#[wasm_bindgen]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    // `Info`, not `Debug`: per-frame paths log at debug and the browser console
    // is synchronous enough that logging every frame is a measurable cost.
    console_log::init_with_level(log::Level::Info)
        .map_err(|e| JsValue::from_str(&format!("logger init failed: {e}")))?;

    log::info!("rustdar starting (wasm32, WebGL2)");

    let canvas = canvas_by_id(CANVAS_ID)?;

    let event_loop =
        EventLoop::new().map_err(|e| JsValue::from_str(&format!("event loop: {e}")))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    // The receiver goes in through the bridge because
    // `App::set_gps_fix_receiver` is `#[cfg(target_os = "android")]`.
    let (fix_sender, fix_receiver) = std::sync::mpsc::channel();
    crate::geolocation::start_watch(fix_sender);

    let mut platform = crate::bridge::WebPlatform::new(canvas);
    rustdar_frontend::platform::PlatformBridge::set_gps_fix_receiver(&mut platform, fix_receiver);

    let app = rustdar_frontend::app::App::new(Box::new(platform));

    event_loop.spawn_app(app);
    Ok(())
}

/// Fetched by id rather than created here: the canvas has to be sized by CSS
/// before winit reads its dimensions, and an element created in Rust would be
/// appended unstyled at the 300x150 default.
fn canvas_by_id(id: &str) -> Result<web_sys::HtmlCanvasElement, JsValue> {
    web_sys::window()
        .ok_or_else(|| JsValue::from_str("no window"))?
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("no element with id {id:?}")))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| JsValue::from_str(&format!("element {id:?} is not a <canvas>")))
}
