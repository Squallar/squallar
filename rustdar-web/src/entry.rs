//! The browser entry point; the counterpart of `rustdar_native::run::run`.

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
/// signature by throwing a JS exception. [`EventLoopExtWebSys::spawn_app`]
/// returns normally.
/// `Wait`, not `Poll`: `Poll` schedules through winit's web scheduler, whose
/// teardown via `AbortController.abort()` is a large fraction of Firefox
/// main-thread time.
#[wasm_bindgen]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    // `Info`, not `Debug`: per-frame paths log at debug and the browser console
    // is synchronous enough that logging every frame is a measurable cost.
    console_log::init_with_level(log::Level::Info)
        .map_err(|e| JsValue::from_str(&format!("logger init failed: {e}")))?;

    log::info!("rustdar starting (wasm32, WebGL2)");

    // Before the app, because the first alerts round is what consumes it. Not
    // fatal and not awaited: the asset is fetched later, on the fetch task, and
    // a browser that will not name its own location simply resolves zones the
    // way it did before the pack existed.
    name_the_zone_pack();

    let canvas = canvas_by_id(CANVAS_ID)?;

    let event_loop =
        EventLoop::new().map_err(|e| JsValue::from_str(&format!("event loop: {e}")))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    // Started before the app so the handshake is in flight while the event loop and
    // WebGL context come up. It never blocks.
    crate::worker_port::attach();

    // `WebBackend::new` starts a *permission query*, which prompts nobody; the
    // watch waits for the gate.
    let platform = crate::bridge::WebPlatform::new(canvas);
    let location =
        rustdar_location::LocationFacade::new(Box::new(rustdar_location::web::WebBackend::new()));
    let app = rustdar_app::app::App::new(Box::new(platform), location);

    event_loop.spawn_app(app);
    Ok(())
}

/// Point zone resolution at the pack served beside `index.html`.
///
/// The URL has to be built here because only the browser knows it: the app is
/// served from `/rustdar/` on Pages and from `/` on CloudFront and on a local
/// `http.server`, and the pack sits next to the page in all three.
/// [`asset_url`](rustdar_overlays::nws::zone_pack_source::asset_url) does the
/// resolving and is tested on the host; this is the two lines that cannot be.
fn name_the_zone_pack() {
    use rustdar_overlays::nws::zone_pack::PACK_FILE_NAME;
    use rustdar_overlays::nws::zone_pack_source::{PackSource, asset_url, use_source};

    let Some(href) = web_sys::window().and_then(|window| window.location().href().ok()) else {
        log::info!("No document location; NWS zones will resolve over HTTP");
        return;
    };
    match asset_url(&href, PACK_FILE_NAME) {
        Some(url) => {
            log::info!("NWS zone pack will be fetched from {url}");
            use_source(PackSource::Url(url));
        }
        None => log::info!("{href} names no directory to fetch {PACK_FILE_NAME} from"),
    }
}

/// Fetched by id rather than created here: the canvas has to be sized by CSS
/// before winit reads its dimensions.
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
