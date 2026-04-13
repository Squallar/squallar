/// Platform-specific behavior abstracted behind a common trait.
/// Keeps `#[cfg(target_os = "android")]` blocks out of `app.rs`.
pub trait PlatformBridge {
    /// Poll for theme changes from the OS. Returns `Some(is_dark)` when
    /// a change is detected, `None` otherwise.
    fn poll_theme(&mut self) -> Option<bool>;

    /// Poll for GPS fix updates. Returns the latest [`GpsFix`] if available.
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

    /// Get the config directory.
    fn config_dir(&self) -> Option<&std::path::Path>;

    /// Request application exit. Returns `true` if the platform requires
    /// `std::process::exit` (Android), `false` for normal event-loop exit.
    fn needs_process_exit(&self) -> bool;

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

// ── Desktop implementation ──────────────────────────────────────────────

#[cfg(not(target_os = "android"))]
pub struct DesktopPlatform {
    back_handler: Option<fn()>,
    zone_cache_dir: Option<std::path::PathBuf>,
    config_dir: Option<std::path::PathBuf>,
    /// Active serial GPS reader (dropped to stop).
    gps_reader: Option<rustdar_gps::SerialGpsReader>,
    /// Receives GPS fixes from the serial reader thread.
    gps_fix_receiver: Option<std::sync::mpsc::Receiver<rustdar_gps::GpsFix>>,
}

#[cfg(not(target_os = "android"))]
impl Default for DesktopPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_os = "android"))]
impl DesktopPlatform {
    pub fn new() -> Self {
        Self {
            back_handler: None,
            zone_cache_dir: Self::default_zone_cache_dir(),
            config_dir: Self::default_config_dir(),
            gps_reader: None,
            gps_fix_receiver: None,
        }
    }

    fn default_config_dir() -> Option<std::path::PathBuf> {
        let base = std::env::var("XDG_CONFIG_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.config", h)))
            .or_else(|_| std::env::var("LOCALAPPDATA"))
            .ok()?;
        Some(std::path::PathBuf::from(base).join("rustdar"))
    }

    fn default_zone_cache_dir() -> Option<std::path::PathBuf> {
        let base = std::env::var("XDG_CACHE_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.cache", h)))
            .or_else(|_| std::env::var("LOCALAPPDATA"))
            .ok()?;
        Some(std::path::PathBuf::from(base).join("rustdar").join("zones"))
    }
}

#[cfg(not(target_os = "android"))]
impl PlatformBridge for DesktopPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        // Desktop uses WindowEvent::ThemeChanged; no polling needed.
        None
    }

    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix> {
        let receiver = self.gps_fix_receiver.as_ref()?;
        let mut latest = None;
        while let Ok(fix) = receiver.try_recv() {
            latest = Some(fix);
        }
        latest
    }

    fn poll_heading(&mut self) -> Option<f32> {
        None // No compass on desktop
    }

    fn query_insets(&self) -> Option<(f32, f32, f32, f32)> {
        None // No system bar insets on desktop
    }

    fn handle_back(&self) -> bool {
        if let Some(handler) = self.back_handler {
            handler();
            true
        } else {
            false
        }
    }

    fn detect_dark_theme(&self) -> bool {
        matches!(dark_light::detect(), Ok(dark_light::Mode::Dark))
    }

    fn set_back_handler(&mut self, handler: fn()) {
        self.back_handler = Some(handler);
    }

    fn set_zone_cache_dir(&mut self, dir: std::path::PathBuf) {
        self.zone_cache_dir = Some(dir);
    }

    fn zone_cache_dir(&self) -> Option<&std::path::Path> {
        self.zone_cache_dir.as_deref()
    }

    fn set_config_dir(&mut self, dir: std::path::PathBuf) {
        self.config_dir = Some(dir);
    }

    fn config_dir(&self) -> Option<&std::path::Path> {
        self.config_dir.as_deref()
    }

    fn needs_process_exit(&self) -> bool {
        false
    }

    fn start_gps(&mut self, config: &rustdar_gps::GpsConfig) {
        // Stop any existing reader first
        self.stop_gps();
        let (tx, rx) = std::sync::mpsc::channel();
        if let Some(reader) = rustdar_gps::SerialGpsReader::start(config, tx) {
            self.gps_reader = Some(reader);
            self.gps_fix_receiver = Some(rx);
            log::info!("Desktop serial GPS reader started");
        } else {
            log::warn!("No GPS port found — serial GPS not started");
        }
    }

    fn stop_gps(&mut self) {
        if self.gps_reader.take().is_some() {
            log::info!("Desktop serial GPS reader stopped");
        }
        self.gps_fix_receiver = None;
    }

    fn gps_active(&self) -> bool {
        self.gps_reader.is_some()
    }
}

// ── Android implementation ──────────────────────────────────────────────

#[cfg(target_os = "android")]
pub struct AndroidPlatform {
    theme_receiver: std::sync::mpsc::Receiver<bool>,
    gps_fix_receiver: Option<std::sync::mpsc::Receiver<rustdar_gps::GpsFix>>,
    heading_receiver: Option<std::sync::mpsc::Receiver<f32>>,
    insets_querier: Option<fn() -> (f32, f32, f32, f32)>,
    back_handler: Option<fn()>,
    zone_cache_dir: Option<std::path::PathBuf>,
    config_dir: Option<std::path::PathBuf>,
}

#[cfg(target_os = "android")]
impl AndroidPlatform {
    pub fn new() -> Self {
        use rustdar_android_theme as android_theme;

        let (theme_sender, theme_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut last_theme = android_theme::detect_dark_theme();
            let _ = theme_sender.send(last_theme);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let current = android_theme::detect_dark_theme();
                if current != last_theme {
                    last_theme = current;
                    if theme_sender.send(current).is_err() {
                        break;
                    }
                }
            }
        });

        Self {
            theme_receiver,
            gps_fix_receiver: None,
            heading_receiver: None,
            insets_querier: None,
            back_handler: None,
            zone_cache_dir: None,
            config_dir: None,
        }
    }
}

#[cfg(target_os = "android")]
impl PlatformBridge for AndroidPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        let mut latest = None;
        while let Ok(theme) = self.theme_receiver.try_recv() {
            latest = Some(theme);
        }
        latest
    }

    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix> {
        let receiver = self.gps_fix_receiver.as_ref()?;
        let mut latest = None;
        while let Ok(fix) = receiver.try_recv() {
            latest = Some(fix);
        }
        latest
    }

    fn poll_heading(&mut self) -> Option<f32> {
        let receiver = self.heading_receiver.as_ref()?;
        let mut latest = None;
        while let Ok(heading) = receiver.try_recv() {
            latest = Some(heading);
        }
        latest
    }

    fn query_insets(&self) -> Option<(f32, f32, f32, f32)> {
        self.insets_querier.map(|q| q())
    }

    fn handle_back(&self) -> bool {
        if let Some(handler) = self.back_handler {
            handler();
            true
        } else {
            false
        }
    }

    fn detect_dark_theme(&self) -> bool {
        rustdar_android_theme::detect_dark_theme()
    }

    fn set_back_handler(&mut self, handler: fn()) {
        self.back_handler = Some(handler);
    }

    fn set_zone_cache_dir(&mut self, dir: std::path::PathBuf) {
        self.zone_cache_dir = Some(dir);
    }

    fn zone_cache_dir(&self) -> Option<&std::path::Path> {
        self.zone_cache_dir.as_deref()
    }

    fn set_config_dir(&mut self, dir: std::path::PathBuf) {
        self.config_dir = Some(dir);
    }

    fn config_dir(&self) -> Option<&std::path::Path> {
        self.config_dir.as_deref()
    }

    fn needs_process_exit(&self) -> bool {
        true
    }

    fn set_gps_fix_receiver(&mut self, receiver: std::sync::mpsc::Receiver<rustdar_gps::GpsFix>) {
        self.gps_fix_receiver = Some(receiver);
    }

    fn set_heading_receiver(&mut self, receiver: std::sync::mpsc::Receiver<f32>) {
        self.heading_receiver = Some(receiver);
    }

    fn set_insets_querier(&mut self, querier: fn() -> (f32, f32, f32, f32)) {
        self.insets_querier = Some(querier);
    }
}

/// Create the platform-appropriate bridge.
#[cfg(not(target_os = "android"))]
pub fn create_platform() -> DesktopPlatform {
    DesktopPlatform::new()
}

#[cfg(target_os = "android")]
pub fn create_platform() -> AndroidPlatform {
    AndroidPlatform::new()
}
