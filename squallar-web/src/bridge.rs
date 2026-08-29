//! The browser's [`PlatformBridge`]. Most of the trait is capabilities a tab
//! does not have, so most of this file is honest `None`s.

use squallar_app::platform::PlatformBridge;
use winit::platform::web::WindowAttributesExtWebSys;

const DARK_SCHEME_QUERY: &str = "(prefers-color-scheme: dark)";

pub struct WebPlatform {
    /// The canvas the winit window is bound to. Held because `window_attributes`
    /// is called after construction, on `resumed`.
    canvas: web_sys::HtmlCanvasElement,
    /// Last theme reported to the app, so `poll_theme` can answer "changed?".
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
        // Report only transitions: the app bumps every cached texture when this
        // returns `Some`.
        if self.last_theme == Some(current) {
            return None;
        }
        self.last_theme = Some(current);
        Some(current)
    }

    fn detect_dark_theme(&self) -> bool {
        // A browser too old for `matchMedia`, or a failed query, is treated as light.
        web_sys::window()
            .and_then(|w| w.match_media(DARK_SCHEME_QUERY).ok().flatten())
            .is_some_and(|list| list.matches())
    }

    /// `DeviceOrientationEvent` needs a secure context and, on iOS, a separate
    /// gesture-gated permission.
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

    /// No filesystem, so no zone cache.
    fn set_zone_cache_dir(&mut self, _dir: std::path::PathBuf) {}

    fn zone_cache_dir(&self) -> Option<&std::path::Path> {
        None
    }

    /// No filesystem for the archive block cache either: it stays disabled,
    /// and the browser's HTTP cache is this target's persistence story.
    fn set_basemap_cache_dir(&mut self, _dir: std::path::PathBuf) {}

    fn basemap_cache_dir(&self) -> Option<&std::path::Path> {
        None
    }

    /// Inert, not an oversight: `localStorage` is available from the first frame.
    fn set_config_dir(&mut self, _dir: std::path::PathBuf) {}

    fn iana_timezone(&self) -> Option<String> {
        browser_timezone()
    }

    fn kv(&self) -> Option<Box<dyn squallar_kv::KvStore>> {
        crate::kv::LocalStorageKvStore::new()
            .map(|store| Box::new(store) as Box<dyn squallar_kv::KvStore>)
    }

    /// There is no process to exit; the event loop stopping is all there is.
    fn needs_process_exit(&self) -> bool {
        false
    }

    // The browser's location service lives in `squallar_location::web`: the
    // permission is asked about before anything is asked *for*, and the watch is
    // started only from `request_location`.

    fn window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        // No `with_inner_size`, deliberately: it writes an inline pixel
        // `width`/`height` that outranks the stylesheet's `width: 100%`.
        attributes
            .with_canvas(Some(self.canvas.clone()))
            // Otherwise the browser also handles events egui already consumed:
            // scrolling the map scrolls the page, dragging selects text.
            .with_prevent_default(true)
            // The canvas is already in the document; appending adds a second one.
            .with_append(false)
    }
}

/// The browser's IANA timezone, e.g. `"America/Denver"`.
///
/// `Intl.DateTimeFormat().resolvedOptions().timeZone` is the whole mechanism:
/// no permission, no prompt, no network, and an answer before the first frame.
///
/// Reached through `js_sys::Reflect` because `ResolvedDateTimeFormatOptions`
/// is an anonymous object in the spec and `web_sys` exposes it as `Object`.
#[cfg(target_arch = "wasm32")]
fn browser_timezone() -> Option<String> {
    use wasm_bindgen::JsValue;

    let resolved = js_sys::Intl::DateTimeFormat::default().resolved_options();
    let zone = js_sys::Reflect::get(&resolved, &JsValue::from_str("timeZone")).ok()?;
    // A browser too old for the `timeZone` key returns `undefined`.
    let zone = zone.as_string()?;
    // An empty string is not a zone, and would reach the anchor table as a lookup
    // that misses in a way that looks deliberate.
    (!zone.is_empty()).then_some(zone)
}
