//! The browser entry point.
//!
//! The counterpart of `rustdar_platform::run::run`, and it diverges from it in
//! exactly two places that matter — see [`start`].

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::web::EventLoopExtWebSys;

/// The id of the `<canvas>` in `index.html` this app renders into.
const CANVAS_ID: &str = "rustdar-canvas";

/// Boot rustdar into the canvas named by [`CANVAS_ID`].
///
/// Exported to JS and called by `index.html` once the DOM exists.
///
/// # Why this is not `run_app`
///
/// `EventLoop::run_app` never returns, which on a platform with a real stack is
/// fine and in a browser is not: there is nothing to return *to*, and winit's
/// web backend implements the same non-returning signature by throwing a JS
/// exception to unwind out of Rust. That works, but it means every call that
/// started the loop sees an exception, and the wasm module is left running. The
/// web backend's own answer is [`EventLoopExtWebSys::spawn_app`], which hands
/// the loop to the browser's event dispatch and returns normally.
///
/// # Why `Wait` and not `Poll`
///
/// `ControlFlow::Poll` schedules its next iteration through winit's web
/// scheduler, which uses `Scheduler.postTask` where available. Tearing down each
/// of those tasks goes through `AbortController.abort()`, and in Firefox that
/// teardown alone is a large fraction of main-thread time — for an app that
/// only redraws when something changed, all of it wasted. rustdar is already
/// `Wait` on desktop and the frame path is built for it: every async completion
/// ends in `notify_redraw`, so nothing depends on a frame arriving unbidden.
#[wasm_bindgen]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    // `Info` rather than `Debug`: the per-frame paths log at debug, and the
    // browser console is synchronous enough that logging every frame is itself
    // a measurable cost.
    console_log::init_with_level(log::Level::Info)
        .map_err(|e| JsValue::from_str(&format!("logger init failed: {e}")))?;

    log::info!("rustdar starting (wasm32, WebGL2)");

    let canvas = canvas_by_id(CANVAS_ID)?;

    let event_loop =
        EventLoop::new().map_err(|e| JsValue::from_str(&format!("event loop: {e}")))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    // Start the geolocation watch before the bridge is boxed. The permission
    // prompt appears on this call and fixes queue in the channel until the
    // first frame drains them.
    //
    // The receiver goes in through the bridge rather than through
    // `App::set_gps_fix_receiver`, which is `#[cfg(target_os = "android")]` and
    // does not exist here. Widening that cfg would have worked equally well;
    // going through the bridge keeps the change inside this crate.
    let (fix_sender, fix_receiver) = std::sync::mpsc::channel();
    crate::geolocation::start_watch(fix_sender);

    let mut platform = crate::bridge::WebPlatform::new(canvas);
    rustdar_frontend::platform::PlatformBridge::set_gps_fix_receiver(&mut platform, fix_receiver);

    let app = rustdar_frontend::app::App::new(Box::new(platform));

    event_loop.spawn_app(app);
    Ok(())
}

/// Look up the canvas the page has already laid out.
///
/// Fetched by id rather than created here because `index.html` owns the layout:
/// the canvas has to be sized by CSS before winit reads its dimensions, and an
/// element created in Rust would be appended with no styling and a 300x150
/// default.
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
