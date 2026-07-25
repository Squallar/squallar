//! The seam between the portable app and whatever OS it is running on.
//!
//! Only the *trait* lives here. Every concrete implementation lives beside the
//! entry point that constructs it (`rustdar-platform` for desktop and Android),
//! because this crate must build for targets whose bridges it has never heard
//! of — and because a crate that named them would have to depend on the crate
//! that depends on it.

/// Drain all pending messages from `rx`, returning the last one (if any).
///
/// Sensor and theme channels are state, not events: only the newest value
/// matters and the ones behind it are already stale. Draining rather than
/// taking one per frame keeps a fast producer from building a backlog the UI
/// then walks through one frame at a time.
pub fn drain_latest<T>(rx: &std::sync::mpsc::Receiver<T>) -> Option<T> {
    let mut latest = None;
    while let Ok(val) = rx.try_recv() {
        latest = Some(val);
    }
    latest
}

/// Platform-specific behavior abstracted behind a common trait.
/// Keeps `#[cfg(target_os = "android")]` blocks out of `app.rs`.
pub trait PlatformBridge {
    /// Poll for theme changes from the OS. Returns `Some(is_dark)` when
    /// a change is detected, `None` otherwise.
    fn poll_theme(&mut self) -> Option<bool>;

    /// Poll for GPS fix updates. Returns the latest [`GpsFix`](rustdar_gps::GpsFix) if available.
    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix>;

    /// Poll for compass heading updates. Returns degrees (0–360) if available.
    fn poll_heading(&mut self) -> Option<f32>;

    /// Query system bar insets (top, bottom, left, right) in logical pixels.
    fn query_insets(&self) -> Option<(f32, f32, f32, f32)>;

    /// Handle the back button press. Returns `true` if the platform consumed
    /// the event (e.g. Android moveTaskToBack), `false` if the app should exit.
    fn handle_back(&self) -> bool;

    /// Detect the current system dark theme preference.
    fn detect_dark_theme(&self) -> bool;

    /// Set a callback for back-button behavior.
    fn set_back_handler(&mut self, handler: fn());

    /// Set persistent cache directory for zone geometries.
    fn set_zone_cache_dir(&mut self, dir: std::path::PathBuf);

    /// Get the zone cache directory.
    fn zone_cache_dir(&self) -> Option<&std::path::Path>;

    /// Set the config directory for UI config persistence.
    fn set_config_dir(&mut self, dir: std::path::PathBuf);

    /// Where to persist UI configuration, or `None` if this platform has not
    /// been told yet (Android learns its data path only after startup).
    ///
    /// Returns a store rather than a directory so the trait carries no
    /// filesystem assumption: a web bridge hands back a `localStorage` backend,
    /// which has no path to return.
    fn config_store(&self) -> Option<Box<dyn rustdar_egui::config_store::ConfigStore>>;

    /// Request application exit. Returns `true` if the platform requires
    /// `std::process::exit` (Android), `false` for normal event-loop exit.
    fn needs_process_exit(&self) -> bool;

    /// Adjust the attributes the main window is created with.
    ///
    /// Defaulted because only the web bridge has anything to add: winit's web
    /// backend has to be told which `<canvas>` the window *is* before the window
    /// exists, and that element is a `web_sys` type this crate cannot name
    /// without taking a browser dependency on every target. Returning the
    /// attributes unchanged is the correct behaviour everywhere else, so this is
    /// a hook rather than a required method.
    fn window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        attributes
    }

    /// Set a receiver for GPS fix updates (Android only, no-op on desktop).
    fn set_gps_fix_receiver(&mut self, _receiver: std::sync::mpsc::Receiver<rustdar_gps::GpsFix>) {}

    /// Set a receiver for compass heading updates (Android only, no-op on desktop).
    fn set_heading_receiver(&mut self, _receiver: std::sync::mpsc::Receiver<f32>) {}

    /// Set a callback that queries system bar insets (Android only, no-op on desktop).
    fn set_insets_querier(&mut self, _querier: fn() -> (f32, f32, f32, f32)) {}

    /// Start the desktop serial GPS reader (no-op on Android).
    fn start_gps(&mut self, _config: &rustdar_gps::GpsConfig) {}

    /// Stop the desktop serial GPS reader (no-op on Android).
    fn stop_gps(&mut self) {}

    /// Whether the desktop serial GPS reader is currently running.
    fn gps_active(&self) -> bool { false }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the newest value survives a drain — that is the whole point of it.
    #[test]
    fn drain_latest_returns_the_newest_value() {
        let (tx, rx) = std::sync::mpsc::channel();
        for v in [1, 2, 3] {
            tx.send(v).unwrap();
        }

        assert_eq!(drain_latest(&rx), Some(3));
    }

    /// A drained channel must report empty rather than replaying the last value.
    #[test]
    fn drain_latest_is_empty_once_drained() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(7).unwrap();

        assert_eq!(drain_latest(&rx), Some(7));
        assert_eq!(drain_latest(&rx), None, "the value must not be replayed");
    }

    #[test]
    fn drain_latest_on_an_empty_channel_is_none() {
        let (_tx, rx) = std::sync::mpsc::channel::<u8>();
        assert_eq!(drain_latest(&rx), None);
    }
}
