//! The browser's [`PlatformBridge`], counterpart of
//! `rustdar_native::platform::DesktopPlatform`. Most of the trait is
//! capabilities a tab does not have, so most of this file is honest `None`s.

use rustdar_frontend::platform::{PlatformBridge, RedrawWaker};
use winit::platform::web::WindowAttributesExtWebSys;

const DARK_SCHEME_QUERY: &str = "(prefers-color-scheme: dark)";

pub struct WebPlatform {
    /// The canvas the winit window is bound to. Held because
    /// `window_attributes` is called after construction, on `resumed`.
    canvas: web_sys::HtmlCanvasElement,
    /// Last theme reported to the app, so `poll_theme` can answer "changed?"
    /// rather than "what is it?".
    last_theme: Option<bool>,
}

impl WebPlatform {
    pub fn new(canvas: web_sys::HtmlCanvasElement) -> Self {
        Self {
            canvas,
            last_theme: None,
        }
    }
}

impl PlatformBridge for WebPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        let current = self.detect_dark_theme();
        // Report only transitions. The app bumps every cached texture when this
        // returns `Some`, so answering unconditionally would re-render the whole
        // UI on every frame.
        if self.last_theme == Some(current) {
            return None;
        }
        self.last_theme = Some(current);
        Some(current)
    }

    fn detect_dark_theme(&self) -> bool {
        // A browser too old for `matchMedia`, or a failed query, is treated as
        // light — the same default the desktop bridge falls back to.
        web_sys::window()
            .and_then(|w| w.match_media(DARK_SCHEME_QUERY).ok().flatten())
            .is_some_and(|list| list.matches())
    }

    // `set_redraw_waker` keeps its default no-op: this bridge starts no
    // producers of its own since the location half moved to
    // `rustdar_location::web` (WO-RL-4) — the facade's arm holds the wake slot
    // the browser callbacks fire now.

    /// `DeviceOrientationEvent` needs a secure context and, on iOS, a separate
    /// gesture-gated permission. `HeadingSource` already falls back to the GPS
    /// bearing when no compass reports.
    fn poll_heading(&mut self) -> Option<f32> {
        None
    }

    /// A tab has no system bars to inset around.
    fn query_insets(&self) -> Option<(f32, f32, f32, f32)> {
        None
    }

    /// Nothing consumes a back gesture, so the app's own handling stands.
    fn handle_back(&self) -> bool {
        false
    }

    fn set_back_handler(&mut self, _handler: fn()) {}

    /// No filesystem, so no zone cache. The overlay layer treats the absence as
    /// "fetch every time" and the browser's HTTP cache sits underneath.
    fn set_zone_cache_dir(&mut self, _dir: std::path::PathBuf) {}

    fn zone_cache_dir(&self) -> Option<&std::path::Path> {
        None
    }

    /// Inert, not an oversight: `localStorage` is available from the first
    /// frame, which is why `kv` never returns `None` for "not told
    /// where yet" the way the Android bridge does.
    fn set_config_dir(&mut self, _dir: std::path::PathBuf) {}

    fn iana_timezone(&self) -> Option<String> {
        browser_timezone()
    }

    fn kv(&self) -> Option<Box<dyn rustdar_kv::KvStore>> {
        crate::kv::LocalStorageKvStore::new()
            .map(|store| Box::new(store) as Box<dyn rustdar_kv::KvStore>)
    }

    /// There is no process to exit; the event loop stopping is all there is.
    fn needs_process_exit(&self) -> bool {
        false
    }

    // The location service left this bridge at WO-RL-4: the browser arm lives
    // in `rustdar_location::web` (`WebBackend`, which `entry::start` hands to
    // the app inside its LocationFacade). Its shape — permission asked about
    // WITHOUT prompting at construction, the watch started only from the
    // gate's request — moved with it, unchanged.

    // ── Platform location service ───────────────────────────────────────
    //
    // The browser's is the one location service that was already wired in this
    // repo, and it was wired the wrong way round: `entry::start` called
    // `watchPosition` unconditionally at boot, so the page prompted on first
    // paint with no user gesture, and a refusal was an `info!` line the app
    // could neither see nor act on. The permission is asked about here, before
    // anything is asked *for*, and the watch is started only from
    // `request_location` — which the gate reaches only from a state that
    // licenses it.

    fn window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        // No `with_inner_size`, deliberately. winit's web backend reports
        // `inner_size()` from a cell written only by its ResizeObserver, so the
        // size is zero for the first frame or two either way (the zero-size
        // guard in `App::handle_redraw` is the actual fix) — and setting it
        // writes an inline pixel `width`/`height` that outranks the stylesheet's
        // `width: 100%`, pinning the canvas to its startup size forever.
        attributes
            .with_canvas(Some(self.canvas.clone()))
            // Otherwise the browser also handles events egui already consumed:
            // scrolling the map scrolls the page, dragging selects text.
            .with_prevent_default(true)
            // The canvas is already in the document; appending adds a second one
            // that nothing has sized.
            .with_append(false)
    }
}

/// The browser's IANA timezone, e.g. `"America/Denver"`.
///
/// `Intl.DateTimeFormat().resolvedOptions().timeZone` is the whole mechanism:
/// no permission, no prompt, no network, and an answer before the first frame.
/// It is the only "where is this user" signal a page gets for free, which is
/// why it is worth the coarse resolution — see the app's `location_hint` for
/// what that resolution is and is not good for. (Timezone is bridge business,
/// not a location arm — it stayed here when geolocation moved to
/// `rustdar_location::web` at WO-RL-4.)
///
/// Reached through `js_sys::Reflect` rather than a typed `web_sys` binding
/// because `ResolvedDateTimeFormatOptions` is an anonymous object in the spec
/// and `web_sys` exposes it as a bare `Object`.
#[cfg(target_arch = "wasm32")]
fn browser_timezone() -> Option<String> {
    use wasm_bindgen::JsValue;

    let resolved = js_sys::Intl::DateTimeFormat::default().resolved_options();
    let zone = js_sys::Reflect::get(&resolved, &JsValue::from_str("timeZone")).ok()?;
    // A browser too old for the `timeZone` key returns `undefined`, whose
    // `as_string` is `None` — the same miss as any other absent value.
    let zone = zone.as_string()?;
    // An empty string is not a zone, and would otherwise reach the anchor table
    // as a lookup that misses in a way that looks deliberate.
    (!zone.is_empty()).then_some(zone)
}
