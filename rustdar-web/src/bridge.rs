//! The browser's [`PlatformBridge`], counterpart of
//! `rustdar_platform::platform::DesktopPlatform`. Most of the trait is
//! capabilities a tab does not have, so most of this file is honest `None`s.

use rustdar_frontend::platform::{PlatformBridge, drain_latest};
use winit::platform::web::WindowAttributesExtWebSys;

const DARK_SCHEME_QUERY: &str = "(prefers-color-scheme: dark)";

pub struct WebPlatform {
    /// The canvas the winit window is bound to. Held because
    /// `window_attributes` is called after construction, on `resumed`.
    canvas: web_sys::HtmlCanvasElement,
    /// Fixes pushed by the geolocation watch. `None` until the watch starts.
    gps_fix_receiver: Option<std::sync::mpsc::Receiver<rustdar_gps::GpsFix>>,
    /// Last theme reported to the app, so `poll_theme` can answer "changed?"
    /// rather than "what is it?".
    last_theme: Option<bool>,
}

impl WebPlatform {
    pub fn new(canvas: web_sys::HtmlCanvasElement) -> Self {
        Self {
            canvas,
            gps_fix_receiver: None,
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

    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix> {
        self.gps_fix_receiver.as_ref().and_then(drain_latest)
    }

    fn set_gps_fix_receiver(
        &mut self,
        receiver: std::sync::mpsc::Receiver<rustdar_gps::GpsFix>,
    ) {
        self.gps_fix_receiver = Some(receiver);
    }

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
    /// frame, which is why `config_store` never returns `None` for "not told
    /// where yet" the way the Android bridge does.
    fn set_config_dir(&mut self, _dir: std::path::PathBuf) {}

    fn config_store(&self) -> Option<Box<dyn rustdar_egui::config_store::ConfigStore>> {
        crate::config_store::LocalStorageConfigStore::new()
            .map(|store| Box::new(store) as Box<dyn rustdar_egui::config_store::ConfigStore>)
    }

    /// There is no process to exit; the event loop stopping is all there is.
    fn needs_process_exit(&self) -> bool {
        false
    }

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
