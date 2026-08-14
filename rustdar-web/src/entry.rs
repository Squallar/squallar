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
/// redraws on change. Every async completion here asks for a frame when it
/// lands, so nothing depends on an unbidden one.
///
/// # What used to be here, and why it is not
///
/// This function used to open an `mpsc` channel, call `geolocation::start_watch`
/// on it and hand the receiver to the app — three lines that between them
/// produced the browser's location prompt **on first paint, with no user
/// gesture**, before the page had shown the user anything at all. A refusal was
/// logged at `info!` and reached nothing, so a denial and a device with no
/// signal were the same empty channel forever and the app could never re-ask,
/// explain, or offer a way back.
///
/// [`WebPlatform`] owns all of it now: the permission state, the query that
/// reads it without prompting, and the watch. The prompt happens from
/// `request_location`, which the gate in `rustdar_frontend::location_permission`
/// reaches only from a state that licenses one — never before the browser has
/// answered, never more than once per install unprompted, and never again after
/// a refusal.
///
/// [`WebPlatform`]: crate::bridge::WebPlatform
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

    // Started before the app so the handshake is in flight while the event loop
    // and WebGL context come up. It never blocks: a job dispatched before the
    // worker answers waits out `worker_port::HANDSHAKE_WINDOW` and then runs on
    // this thread, and a worker that is lost later is replaced rather than
    // written off — which is what keeps one mid-session error from putting
    // every later scan, scrub and loop frame back on this thread.
    crate::worker_port::attach();

    // Nothing about location happens here. `WebPlatform::new` starts a
    // *permission query*, which prompts nobody; `App::new` hands the bridge the
    // waker its callbacks fire, and the watch waits for the gate. See the note
    // above.
    let platform = crate::bridge::WebPlatform::new(canvas);
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
