//! Concrete [`PlatformBridge`] implementations. The trait lives in
//! `rustdar-frontend`, which must never name a per-OS type.

use rustdar_frontend::platform::{PlatformBridge, RedrawWaker};
// The iOS bridge has no pollable channels yet; see the note above `IosPlatform`.
#[cfg(not(target_os = "ios"))]
use rustdar_frontend::platform::drain_latest;

/// System bar insets as `(top, bottom, left, right)`. Aliased because
/// `clippy::type_complexity` rejects the bare fn pointer in the field below.
#[cfg(target_os = "android")]
type InsetsQuerier = fn() -> (f32, f32, f32, f32);

/// This machine's IANA timezone name, or `None` if it cannot be determined.
///
/// Shared by all three native bridges, which answer this identically —
/// `iana-time-zone` already covers Linux, macOS, Windows, Android and iOS, so
/// there is nothing per-OS left for the bridges to decide.
///
/// A failure here is ordinary: a container with no `/etc/localtime`, or a `TZ`
/// naming a POSIX offset rather than a zone. The caller falls back to its
/// compiled-in default site, which is what it did before this existed.
fn system_timezone() -> Option<String> {
    match iana_time_zone::get_timezone() {
        Ok(zone) => Some(zone),
        Err(e) => {
            log::debug!("no system timezone available: {e}");
            None
        }
    }
}

// ── Desktop implementation ──────────────────────────────────────────────

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub struct DesktopPlatform {
    back_handler: Option<fn()>,
    zone_cache_dir: Option<std::path::PathBuf>,
    config_dir: Option<std::path::PathBuf>,
    /// Active serial GPS reader (dropped to stop).
    gps_reader: Option<rustdar_gps::SerialGpsReader>,
    /// Receives GPS fixes from the serial reader thread.
    gps_fix_receiver: Option<std::sync::mpsc::Receiver<rustdar_gps::GpsFix>>,
    /// Receives fixes from the OS location service.
    ///
    /// A second channel rather than a second sender into the first, because the
    /// two sources have to be told apart: `poll_gps_fix` picks between them (see
    /// [`os_location::prefer_fix`]) and cannot do that once they are merged.
    ///
    /// `None` on every target today — [`os_location`] compiles only its
    /// `unsupported` arm, whose `start` never returns a reader. The field is
    /// here so the drain below is written once and the providers land against a
    /// consumer that already exists.
    ///
    /// [`os_location`]: crate::os_location
    /// [`os_location::prefer_fix`]: crate::os_location::prefer_fix
    os_fix_receiver: Option<std::sync::mpsc::Receiver<rustdar_gps::GpsFix>>,
    /// Handed to the reader thread so a fix arriving while the loop is parked
    /// gets a frame to be shown on. See [`RedrawWaker`].
    redraw_waker: RedrawWaker,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Default for DesktopPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl DesktopPlatform {
    pub fn new() -> Self {
        Self {
            back_handler: None,
            zone_cache_dir: Self::default_zone_cache_dir(),
            config_dir: Self::default_config_dir(),
            gps_reader: None,
            gps_fix_receiver: None,
            os_fix_receiver: None,
            redraw_waker: RedrawWaker::new(),
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

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl PlatformBridge for DesktopPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        // Desktop uses WindowEvent::ThemeChanged; no polling needed.
        None
    }

    /// Drains **both** sources every time, not the first one that answers.
    ///
    /// Draining conditionally would leave the loser's channel filling up: the
    /// OS provider pushes on its own schedule and nothing else empties it, so a
    /// serial fix arriving first would build an unbounded backlog behind it
    /// that later surfaces as minutes-old positions. See
    /// [`prefer_fix`](crate::os_location::prefer_fix) for which one wins and
    /// why it is not simply "serial".
    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix> {
        let serial = self.gps_fix_receiver.as_ref().and_then(drain_latest);
        let os = self.os_fix_receiver.as_ref().and_then(drain_latest);
        crate::os_location::prefer_fix(serial, os)
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

    fn config_store(&self) -> Option<Box<dyn rustdar_egui::config_store::ConfigStore>> {
        self.config_dir
            .clone()
            .map(|dir| Box::new(crate::config_store::FileConfigStore::new(dir)) as Box<_>)
    }

    fn iana_timezone(&self) -> Option<String> {
        system_timezone()
    }

    fn needs_process_exit(&self) -> bool {
        false
    }

    /// The waker is taken here rather than at `start_gps`, because `start_gps`
    /// is reached from a menu toggle and carries nothing but a config. It is
    /// also handed over before any window exists, which is what
    /// [`RedrawWaker`]'s slot is for.
    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        self.redraw_waker = waker;
    }

    fn start_gps(&mut self, config: &rustdar_gps::GpsConfig) {
        // Stop any existing reader first
        self.stop_gps();
        let (tx, rx) = std::sync::mpsc::channel();
        // The reader is a thread of its own, and `poll_gps_fix` is drained only
        // on a frame: under `ControlFlow::Wait` a fix it pushes while the app is
        // idle is invisible until something else happens to draw one.
        let wake = self.redraw_waker.clone();
        if let Some(reader) = rustdar_gps::SerialGpsReader::start(config, tx, move || wake.wake()) {
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

    // ── Platform location service ───────────────────────────────────────
    //
    // Stubs. Linux (GeoClue2 over zbus), Windows (`AppCapability` +
    // `Geolocator`) and macOS (`CLLocationManager`) each land here as their own
    // piece of work, with their own dependency, their own permission mechanics
    // and — on Linux and macOS — their own packaging: GeoClue's `Start()` fails
    // without a `.desktop` file to resolve `DesktopId` against, and macOS needs
    // a signed bundle or `locationd` re-prompts on every rebuild.
    //
    // `Unavailable` and not `Unknown` in the meantime, and the difference
    // matters. `Unknown` means "ask again shortly", so the gate would poll a
    // bridge that is never going to answer and the settings pane would sit on
    // "Checking…" for the life of the process. `Unavailable` is the truth: this
    // build has no OS location provider, the pane says so, and nothing spins.

    fn location_permission(&self) -> rustdar_gps::LocationPermission {
        rustdar_gps::LocationPermission::Unavailable
    }

    /// Nothing to ask, so nothing reached the OS.
    fn request_location(&mut self) -> bool {
        false
    }

    fn stop_location(&mut self) {}
}

// ── Android implementation ──────────────────────────────────────────────

#[cfg(target_os = "android")]
pub struct AndroidPlatform {
    /// Injected by `rustdar-android`: the read is a JNI call and this crate is
    /// `#![deny(unsafe_code)]`.
    theme_detector: Option<fn() -> bool>,
    /// Theme changes from the poll thread `set_theme_detector` starts.
    theme_receiver: Option<std::sync::mpsc::Receiver<bool>>,
    gps_fix_receiver: Option<std::sync::mpsc::Receiver<rustdar_gps::GpsFix>>,
    heading_receiver: Option<std::sync::mpsc::Receiver<f32>>,
    insets_querier: Option<InsetsQuerier>,
    back_handler: Option<fn()>,
    /// Injected by `rustdar-android`: the flag it reads is set by the JNI
    /// callback `BackHandler.java` invokes on the UI thread.
    back_press_taker: Option<fn() -> bool>,
    zone_cache_dir: Option<std::path::PathBuf>,
    config_dir: Option<std::path::PathBuf>,
    /// Injected by `rustdar-android`: all four are JNI calls, for the same
    /// reason `theme_detector` is injected. `None` until they are installed —
    /// see [`PlatformBridge::location_permission`] below for why that is
    /// reported as `Unavailable` rather than `Unknown`.
    location_hooks: Option<rustdar_frontend::platform::LocationHooks>,
    /// What the app last said about how many times it has asked, kept for the
    /// hooks to read. Android is the one platform that cannot tell "never
    /// asked" from "permanently denied" without it — see
    /// [`PlatformBridge::set_location_attempts`].
    location_attempts: u8,
    /// Handed to the theme poller below, so a light/dark switch noticed on that
    /// thread gets a frame to be applied on. See [`RedrawWaker`].
    redraw_waker: RedrawWaker,
}

#[cfg(target_os = "android")]
impl Default for AndroidPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "android")]
impl AndroidPlatform {
    pub fn new() -> Self {
        Self {
            theme_detector: None,
            theme_receiver: None,
            gps_fix_receiver: None,
            heading_receiver: None,
            insets_querier: None,
            back_handler: None,
            back_press_taker: None,
            zone_cache_dir: None,
            config_dir: None,
            location_hooks: None,
            location_attempts: 0,
            redraw_waker: RedrawWaker::new(),
        }
    }
}

#[cfg(target_os = "android")]
impl PlatformBridge for AndroidPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        self.theme_receiver.as_ref().and_then(drain_latest)
    }

    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix> {
        self.gps_fix_receiver.as_ref().and_then(drain_latest)
    }

    fn poll_heading(&mut self) -> Option<f32> {
        self.heading_receiver.as_ref().and_then(drain_latest)
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

    fn poll_back_press(&mut self) -> bool {
        self.back_press_taker.is_some_and(|take| take())
    }

    fn set_back_press_taker(&mut self, taker: fn() -> bool) {
        self.back_press_taker = Some(taker);
    }

    fn detect_dark_theme(&self) -> bool {
        match self.theme_detector {
            Some(detect) => detect(),
            None => {
                // Loud because the failure is invisible: a missing detector
                // just looks like a working app to anyone not in dark mode, and
                // there is no fallback -- NativeActivity never emits
                // `WindowEvent::ThemeChanged`, so the poll channel is the only
                // theme input here.
                log::warn!(
                    "no theme detector installed; assuming light. \
                     android_main must call set_theme_detector before run_app"
                );
                debug_assert!(
                    false,
                    "AndroidPlatform::detect_dark_theme with no detector injected"
                );
                false
            }
        }
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

    fn config_store(&self) -> Option<Box<dyn rustdar_egui::config_store::ConfigStore>> {
        self.config_dir
            .clone()
            .map(|dir| Box::new(crate::config_store::FileConfigStore::new(dir)) as Box<_>)
    }

    fn iana_timezone(&self) -> Option<String> {
        system_timezone()
    }

    fn needs_process_exit(&self) -> bool {
        true
    }

    /// Taken before the theme poller is started, which is the only ordering
    /// this bridge depends on: `android_main` calls `App::new` (which delivers
    /// this) and only then `set_theme_detector` (which spawns the thread).
    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        self.redraw_waker = waker;
    }

    fn set_gps_fix_receiver(&mut self, receiver: std::sync::mpsc::Receiver<rustdar_gps::GpsFix>) {
        self.gps_fix_receiver = Some(receiver);
    }

    fn set_heading_receiver(&mut self, receiver: std::sync::mpsc::Receiver<f32>) {
        self.heading_receiver = Some(receiver);
    }

    fn set_insets_querier(&mut self, querier: InsetsQuerier) {
        self.insets_querier = Some(querier);
    }

    /// NativeActivity gets no `WindowEvent::ThemeChanged`, so a light/dark
    /// switch is only visible by re-reading `Configuration.uiMode` on a timer.
    fn set_theme_detector(&mut self, detector: fn() -> bool) {
        if self.theme_receiver.is_some() {
            // Refuse rather than half-apply: assigning would leave the
            // synchronous path on the new detector while the running thread
            // keeps calling the old one.
            log::warn!("theme detector already installed; ignoring the second one");
            return;
        }
        self.theme_detector = Some(detector);

        match rustdar_frontend::platform::spawn_state_poller(
            "theme-detect",
            std::time::Duration::from_secs(2),
            detector,
            self.redraw_waker.clone(),
        ) {
            Ok(receiver) => self.theme_receiver = Some(receiver),
            // Not fatal: `detect_dark_theme` still answers synchronously, so
            // the app opens in the right theme, it just stops tracking changes.
            Err(e) => {
                log::error!("could not start theme polling, theme will not track changes: {e}")
            }
        }
    }

    // ── Platform location service ───────────────────────────────────────
    //
    // Everything here is a `checkSelfPermission` / `requestPermissions` /
    // `LocationHelper` call over JNI, which needs `unsafe` and the process
    // `JavaVM`. This crate is `#![deny(unsafe_code)]` and cannot depend on
    // `rustdar-android` — that crate depends on this one — so the calls arrive
    // as `fn` pointers, exactly as the theme detector does.

    /// `Unavailable` until the hooks are installed, deliberately not `Unknown`.
    ///
    /// `Unknown` is "the platform has not answered *yet*", and the gate keeps
    /// polling for one. A bridge with no hooks is never going to answer, so
    /// that would be a JNI-shaped poll that never terminates and a settings
    /// pane parked on "Checking…" for the life of the process. `android_main`
    /// installs the hooks before `run_app`, so on a wired build this window
    /// closes before the first frame.
    fn location_permission(&self) -> rustdar_gps::LocationPermission {
        match self.location_hooks {
            Some(hooks) => (hooks.query)(self.location_attempts),
            None => rustdar_gps::LocationPermission::Unavailable,
        }
    }

    fn request_location(&mut self) -> bool {
        self.location_hooks.is_some_and(|hooks| (hooks.request)())
    }

    fn stop_location(&mut self) {
        if let Some(hooks) = self.location_hooks {
            (hooks.stop)();
        }
    }

    fn location_active(&self) -> bool {
        self.location_hooks.is_some_and(|hooks| (hooks.active)())
    }

    /// Refuses a second set, as `set_theme_detector` refuses a second detector
    /// and for the same reason: a half-replaced set would leave the state query
    /// and the request pointing at different implementations, which is a bug
    /// with no symptom until somebody is standing in front of a permission
    /// dialog that never appears.
    fn set_location_hooks(&mut self, hooks: rustdar_frontend::platform::LocationHooks) {
        if self.location_hooks.is_some() {
            log::warn!("location hooks already installed; ignoring the second set");
            return;
        }
        self.location_hooks = Some(hooks);
    }

    fn set_location_attempts(&mut self, attempts: u8) {
        self.location_attempts = attempts;
    }
}

// ── iOS implementation ──────────────────────────────────────────────────
//
// Bare on purpose: GPS, compass and theme are the next unit of work, and they
// are `None` here so they cannot confound the gate this build exists to prove
// (that wgpu/Metal and winit's UIKit path work at all).
//
// There is no insets querier and must not be one: egui-winit already fills
// `RawInput::safe_area_insets` on iOS. Android's side channel works around a
// platform gap iOS does not have.
//
// Nothing will be injected here the way Android injects. That split exists
// because Android's entry point is in another crate; iOS's is in this one.

#[cfg(target_os = "ios")]
pub struct IosPlatform {
    back_handler: Option<fn()>,
    zone_cache_dir: Option<std::path::PathBuf>,
    config_dir: Option<std::path::PathBuf>,
}

#[cfg(target_os = "ios")]
impl Default for IosPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "ios")]
impl IosPlatform {
    pub fn new() -> Self {
        Self {
            back_handler: None,
            zone_cache_dir: Self::sandbox_subdir("Library/Caches/rustdar/zones"),
            config_dir: Self::sandbox_subdir("Library/Application Support/rustdar"),
        }
    }

    /// UIKit points `HOME` at the app's sandbox container, so this needs no
    /// `NSHomeDirectory` call and therefore no ObjC.
    fn sandbox_subdir(rel: &str) -> Option<std::path::PathBuf> {
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(rel))
    }
}

#[cfg(target_os = "ios")]
impl PlatformBridge for IosPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        None
    }

    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix> {
        None
    }

    fn poll_heading(&mut self) -> Option<f32> {
        None
    }

    fn query_insets(&self) -> Option<(f32, f32, f32, f32)> {
        // See the module note above: egui-winit already supplies these.
        None
    }

    fn handle_back(&self) -> bool {
        if let Some(handler) = self.back_handler {
            handler();
            true
        } else {
            false
        }
    }

    /// `dark-light` 2.0's iOS arm returns `Mode::Light` unconditionally, so the
    /// replacement is a `UITraitCollection.userInterfaceStyle` read.
    fn detect_dark_theme(&self) -> bool {
        false
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

    fn config_store(&self) -> Option<Box<dyn rustdar_egui::config_store::ConfigStore>> {
        self.config_dir
            .clone()
            .map(|dir| Box::new(crate::config_store::FileConfigStore::new(dir)) as Box<_>)
    }

    fn iana_timezone(&self) -> Option<String> {
        system_timezone()
    }

    fn needs_process_exit(&self) -> bool {
        false
    }

    fn supports_exit(&self) -> bool {
        false
    }

    /// `CLLocationManager` is the next unit of work here, alongside GPS,
    /// compass and theme. `Unavailable` rather than `Unknown` for the reason
    /// given on the Android arm: `Unknown` asks the gate to keep waiting for an
    /// answer that is not coming.
    ///
    /// iOS is the platform where this is most nearly free — `ios/Info.plist`
    /// already carries `NSLocationWhenInUseUsageDescription` and the staticlib
    /// link already passes `-framework CoreLocation` — and it is still its own
    /// change, because `IosPlatform::new()` runs before `UIApplicationMain`
    /// and the delegate has to be constructed with a `MainThreadMarker` it
    /// cannot obtain there.
    fn location_permission(&self) -> rustdar_gps::LocationPermission {
        rustdar_gps::LocationPermission::Unavailable
    }

    fn request_location(&mut self) -> bool {
        false
    }

    fn stop_location(&mut self) {}
}

/// Create the platform-appropriate bridge.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn create_platform() -> DesktopPlatform {
    DesktopPlatform::new()
}

#[cfg(target_os = "android")]
pub fn create_platform() -> AndroidPlatform {
    AndroidPlatform::new()
}

#[cfg(target_os = "ios")]
pub fn create_platform() -> IosPlatform {
    IosPlatform::new()
}
