//! Concrete [`PlatformBridge`] implementations. The trait lives in
//! `rustdar-frontend`, which must never name a per-OS type.

use rustdar_frontend::platform::{PlatformBridge, RedrawWaker, drain_latest};

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
    /// The OS location service and everything the bridge keeps beside it. See
    /// [`OsLocation`].
    os_location: OsLocation,
    /// Handed to the reader thread so a fix arriving while the loop is parked
    /// gets a frame to be shown on. See [`RedrawWaker`].
    redraw_waker: RedrawWaker,
}

// ── The OS location service, once, for every bridge that has one ────────
//
// Written here rather than inside each bridge because `DesktopPlatform` and
// `IosPlatform` want exactly the same thing from it, and get it from the same
// provider: `crate::os_location`'s arm table chooses, and that table is the
// entire `cfg` surface.
//
// **No `cfg` in this file decides how location is done.** Every `target_os`
// left below says which *bridge* exists — Android's, iOS's, the desktop one —
// or, on `OsLocation` and the two permission codecs, whether the `os_location`
// module is compiled at all, which `lib.rs` gates on the same axis and for the
// same reason: Android reaches its location service over JNI from another crate
// entirely. That is checkable, and it is worth checking, because it was false
// twice: `platform.rs` carried a four-armed table of inherent `impl`s that every
// landing provider had to edit, and a `#[cfg(target_os = "windows")]` field
// beside it.

/// The provider, the permission it last reported, and the channel it delivers
/// on.
#[cfg(not(target_os = "android"))]
struct OsLocation {
    /// The platform's own location service.
    ///
    /// Built once, from `set_redraw_waker`, and held for the life of the
    /// bridge. **Not** dropped by `stop_location`: on two of the three
    /// platforms this value *is* the permission watcher, and a revocation made
    /// in system settings while delivery is off would go unnoticed if it were
    /// torn down with the stream. `None` means this target has no provider at
    /// all, which the bridge renders as `Unavailable`.
    provider: Option<crate::os_location::OsLocationReader>,
    /// What the provider last reported.
    ///
    /// An atomic and not a `Cell`, because it is written from whatever thread
    /// the provider is given — a GeoClue session thread, a WinRT RPC thread,
    /// the main run loop — and read from
    /// [`location_permission`](PlatformBridge::location_permission), which is a
    /// `&self` getter on the frame path. That rules out a `Cell` (not `Send`),
    /// a `Receiver` (cannot be drained through `&self`) and a `Mutex` (a lock
    /// on the frame path). See [`encode_permission`].
    ///
    /// **It deliberately outlives every session.** A revocation arrives as a
    /// permission change *and* stops delivery, and the gate responds to
    /// `Denied` by calling `stop_location` — so a state that lived inside the
    /// thing being stopped would evaporate at exactly the moment it started to
    /// matter, the bridge would fall back to "nobody has been asked", and the
    /// app would ask again straight into the refusal it just received.
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Receives fixes from the provider.
    ///
    /// A channel of its own rather than a second sender into the serial
    /// reader's, because on desktop the two sources have to be told apart:
    /// `poll_gps_fix` picks between them (see [`prefer_fix`]) and cannot do
    /// that once they are merged.
    ///
    /// `None` until a provider exists, and never dropped afterwards — the
    /// provider keeps the matching `Sender` for the life of the bridge, so the
    /// channel survives a stop and is reused by the next start.
    ///
    /// [`prefer_fix`]: crate::os_location::prefer_fix
    fixes: Option<std::sync::mpsc::Receiver<rustdar_gps::GpsFix>>,
}

#[cfg(not(target_os = "android"))]
impl OsLocation {
    /// No provider yet. Constructing one needs the app's waker, which does not
    /// exist when a bridge is built; see [`start`](Self::start).
    fn new() -> Self {
        Self {
            provider: None,
            state: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(encode_permission(
                rustdar_gps::LocationPermission::Unknown,
            ))),
            fixes: None,
        }
    }

    /// Bring the provider up.
    ///
    /// Called from [`set_redraw_waker`](PlatformBridge::set_redraw_waker) and
    /// nowhere else, which is the only point where both of its requirements
    /// hold. `App::with_instance` calls that exactly once, immediately after
    /// constructing itself and before any frame — so the provider exists before
    /// the gate's first `location_permission()` — and it is also the only point
    /// where the app's real waker is in hand: `set_redraw_waker` *replaces* the
    /// field, so a clone taken in `new()` would fire into a slot nothing ever
    /// fills.
    ///
    /// Prompts nobody. That is the contract's first phase; see
    /// [`OsLocationProvider::start`](crate::os_location::OsLocationProvider::start).
    fn start(&mut self, waker: &RedrawWaker) {
        use crate::os_location::OsLocationProvider as _;

        let (fixes, receiver) = std::sync::mpsc::channel();
        let wake = waker.clone();
        let reported = std::sync::Arc::clone(&self.state);
        let report_waker = waker.clone();
        self.provider =
            crate::os_location::OsLocationReader::start(crate::os_location::OsLocationSink {
                fixes,
                wake: std::sync::Arc::new(move || wake.wake()),
                // Store *and* wake. The store is what the frame path reads; the
                // wake is what gets a frame drawn for it to be read on, which
                // under `ControlFlow::Wait` is not otherwise going to happen —
                // a permission change is exactly the kind of event nothing else
                // is causing a redraw for.
                report: std::sync::Arc::new(move |permission| {
                    reported.store(
                        encode_permission(permission),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    report_waker.wake();
                }),
            });
        // Only keep the receiver if something is going to push into it; an open
        // one would make `poll_gps_fix` drain a channel forever.
        self.fixes = self.provider.is_some().then_some(receiver);
    }

    /// Whatever the provider last reported, or `Unavailable` when this build
    /// has none.
    ///
    /// `Unavailable` and not `Unknown` for the no-provider case, and the
    /// difference matters. `Unknown` means "ask again shortly", so the gate
    /// would poll a bridge that is never going to answer and the settings pane
    /// would sit on "Checking…" for the life of the process. `Unavailable` is
    /// the truth: this build has no OS location provider, the pane says so, and
    /// nothing spins.
    fn permission(&self) -> rustdar_gps::LocationPermission {
        match self.provider {
            Some(_) => decode_permission(self.state.load(std::sync::atomic::Ordering::Relaxed)),
            None => rustdar_gps::LocationPermission::Unavailable,
        }
    }

    fn request(&mut self) -> bool {
        use crate::os_location::OsLocationProvider as _;
        self.provider
            .as_mut()
            .is_some_and(crate::os_location::OsLocationReader::request)
    }

    /// Stop the stream and discard anything already in it.
    ///
    /// The drain is the part that is easy to leave out and is not optional:
    /// this is the path a *revoked* permission takes, and a fix that was in
    /// flight when consent was withdrawn must not land on the map one frame
    /// later. The receiver itself stays — the provider still holds the matching
    /// `Sender`, and a later `request` delivers down the same channel.
    ///
    /// The permission is left exactly as the provider last reported it. This is
    /// an off switch, not a revocation, and no platform lets an app hand a
    /// permission back.
    fn stop(&mut self) {
        use crate::os_location::OsLocationProvider as _;

        let was_active = self.active();
        if let Some(provider) = self.provider.as_mut() {
            provider.stop();
        }
        if let Some(receiver) = self.fixes.as_ref() {
            let _ = drain_latest(receiver);
        }
        if was_active {
            log::info!("OS location delivery stopped");
        }
    }

    fn active(&self) -> bool {
        use crate::os_location::OsLocationProvider as _;
        self.provider
            .as_ref()
            .is_some_and(crate::os_location::OsLocationReader::active)
    }

    fn open_settings(&mut self) {
        use crate::os_location::OsLocationProvider as _;
        if let Some(provider) = self.provider.as_mut() {
            provider.open_settings();
        }
    }

    /// The newest fix waiting, if any.
    fn poll_fix(&self) -> Option<rustdar_gps::GpsFix> {
        self.fixes.as_ref().and_then(drain_latest)
    }
}

/// [`rustdar_gps::LocationPermission`] as one byte, for the atomic above.
///
/// Hand-written rather than derived, and the discriminants are pinned by the
/// round-trip test at the bottom of this file: the enum is not `repr(u8)` and
/// nothing in `rustdar-gps` promises its variants keep their order, so a
/// `as u8` cast here would be a silent miscommunication the first time someone
/// inserts a variant.
#[cfg(not(target_os = "android"))]
fn encode_permission(permission: rustdar_gps::LocationPermission) -> u8 {
    use rustdar_gps::LocationPermission as P;
    match permission {
        P::Unknown => 0,
        P::Prompt => 1,
        P::Granted => 2,
        P::Denied => 3,
        P::Unavailable => 4,
    }
}

/// The inverse of [`encode_permission`], with anything unrecognised read as
/// `Unknown` — the one state that neither asks nor concludes.
#[cfg(not(target_os = "android"))]
fn decode_permission(raw: u8) -> rustdar_gps::LocationPermission {
    use rustdar_gps::LocationPermission as P;
    match raw {
        1 => P::Prompt,
        2 => P::Granted,
        3 => P::Denied,
        4 => P::Unavailable,
        _ => P::Unknown,
    }
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
            os_location: OsLocation::new(),
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
        crate::os_location::prefer_fix(serial, self.os_location.poll_fix())
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
    ///
    /// It is also where the OS location provider is brought up, which is not
    /// opportunism: see [`OsLocation::start`].
    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        self.redraw_waker = waker;
        self.os_location.start(&self.redraw_waker);
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
    // Six one-line forwards to [`OsLocation`], and not one of them names a
    // target. Everything per-OS — GeoClue2 over zbus, `AppCapability` +
    // `Geolocator`, `CLLocationManager` — is behind `crate::os_location`, which
    // is the entire `cfg` surface, and every arm of it implements the same
    // trait.

    fn location_permission(&self) -> rustdar_gps::LocationPermission {
        self.os_location.permission()
    }

    fn request_location(&mut self) -> bool {
        self.os_location.request()
    }

    fn stop_location(&mut self) {
        self.os_location.stop();
    }

    fn location_active(&self) -> bool {
        self.os_location.active()
    }

    /// Asked once, at startup, and before any provider exists — which is why
    /// the trait's is an associated function rather than a method.
    fn location_settings_available(&self) -> bool {
        use crate::os_location::OsLocationProvider as _;
        crate::os_location::OsLocationReader::settings_available()
    }

    fn open_location_settings(&mut self) {
        self.os_location.open_settings();
    }
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
// Compass and theme are still the next unit of work and are `None` here. The
// location service is not: it is the same `os_location` provider the desktop
// bridge uses, because CoreLocation is the same API on both.
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
    /// The same [`OsLocation`] the desktop bridge holds, running the same
    /// provider: `os_location`'s arm table selects `apple` for macOS and iOS
    /// alike, because `CLLocationManager` is the same API on both. What differs
    /// between the two platforms lives inside that file.
    os_location: OsLocation,
    redraw_waker: RedrawWaker,
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
            os_location: OsLocation::new(),
            redraw_waker: RedrawWaker::new(),
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

    /// One source, so no [`prefer_fix`](crate::os_location::prefer_fix): iOS
    /// has no serial port to plug a dongle into and the `gps-serial` feature is
    /// not compiled here at all.
    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix> {
        self.os_location.poll_fix()
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

    /// Brings CoreLocation up; see [`OsLocation::start`].
    ///
    /// One thing is genuinely different here and it is upstream of this call:
    /// this bridge is constructed before `UIApplicationMain` has run, so there
    /// is no `UIApplication` and no *running* run loop when the provider is
    /// built. Neither is required to build one — the main thread is still the
    /// main thread — and the callbacks CoreLocation schedules are delivered
    /// once UIKit starts spinning the loop a few milliseconds later.
    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        self.redraw_waker = waker;
        self.os_location.start(&self.redraw_waker);
    }

    // ── Platform location service ───────────────────────────────────────
    //
    // The same forwards `DesktopPlatform` writes, to the same [`OsLocation`],
    // running the same provider.

    fn location_permission(&self) -> rustdar_gps::LocationPermission {
        self.os_location.permission()
    }

    fn request_location(&mut self) -> bool {
        self.os_location.request()
    }

    fn stop_location(&mut self) {
        self.os_location.stop();
    }

    /// Whether CoreLocation was asked to deliver — **not** whether it is
    /// delivering.
    ///
    /// The gap is real and iOS-only: with no `UIBackgroundModes: location` in
    /// `ios/Info.plist`, the OS stops delivering while the app is backgrounded
    /// and gives no callback saying so, so this keeps reporting `true` and the
    /// map keeps the last dot. The settings pane's fix-age line, which is timed
    /// from arrival, is what tells the user the dot is stale. See the module
    /// note in `os_location/apple.rs` for why the fix for that is not simply
    /// setting `allowsBackgroundLocationUpdates`.
    fn location_active(&self) -> bool {
        self.os_location.active()
    }

    fn location_settings_available(&self) -> bool {
        use crate::os_location::OsLocationProvider as _;
        crate::os_location::OsLocationReader::settings_available()
    }

    fn open_location_settings(&mut self) {
        self.os_location.open_settings();
    }
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

#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
mod tests {
    use super::*;
    use rustdar_gps::LocationPermission as P;

    const ALL: &[P] = &[P::Unknown, P::Prompt, P::Granted, P::Denied, P::Unavailable];

    /// The provider thread writes this byte and the frame path reads it, so a
    /// mapping that is not a bijection is a permission silently turning into a
    /// different one — most damagingly `Denied` arriving as `Granted`.
    #[test]
    fn every_permission_survives_the_trip_through_the_atomic() {
        for &permission in ALL {
            assert_eq!(decode_permission(encode_permission(permission)), permission);
        }
    }

    /// Distinct codes, checked separately from the round trip: a collision
    /// where two variants share a byte would still round-trip for one of them
    /// and quietly rewrite the other.
    #[test]
    fn no_two_permissions_share_a_code() {
        let mut codes: Vec<u8> = ALL.iter().map(|&p| encode_permission(p)).collect();
        codes.sort_unstable();
        let count = codes.len();
        codes.dedup();
        assert_eq!(
            codes.len(),
            count,
            "two permissions encode to the same byte"
        );
    }

    /// The atomic starts at zero, and a `AtomicU8::new(0)` that meant anything
    /// else would have the bridge claiming an answer before one exists.
    /// `Unknown` is the state that neither asks nor concludes, which is the
    /// only safe thing for a value nobody has written yet to mean.
    #[test]
    fn an_unwritten_atomic_reads_as_unknown() {
        assert_eq!(decode_permission(0), P::Unknown);
        assert_eq!(encode_permission(P::Unknown), 0);
    }

    /// Nothing writes a byte outside the mapping today, but the decode is on
    /// the frame path and a garbage value must not become a *grant*.
    #[test]
    fn an_unrecognised_code_reads_as_unknown_rather_than_as_a_grant() {
        assert_eq!(decode_permission(200), P::Unknown);
        assert_eq!(decode_permission(u8::MAX), P::Unknown);
    }

    /// A bridge whose provider has not been built has nothing to report, and
    /// `Unavailable` is the honest answer: it is the state the settings pane
    /// renders as "not available on this platform", and the gate treats it as
    /// terminal rather than spinning on "Checking…".
    #[test]
    fn a_bridge_with_no_provider_yet_reports_unavailable() {
        let platform = DesktopPlatform::new();
        assert_eq!(platform.location_permission(), P::Unavailable);
        assert!(!platform.location_active());
    }

    /// The contract's first phase, pinned on whichever arm this host compiles.
    ///
    /// Two rules, and both have been got wrong by a provider already. Bringing
    /// one up must **not** start delivering — that is `request_location`'s job
    /// and the user's decision — and it must leave the bridge with something
    /// the gate can act on, which `Unavailable` is not: the gate reads it as
    /// terminal and never asks again, so a provider that came up and reported
    /// nothing would be a location service the app could never turn on.
    #[test]
    fn bringing_a_provider_up_reports_a_state_and_delivers_nothing() {
        let mut platform = DesktopPlatform::new();
        platform.set_redraw_waker(RedrawWaker::new());

        assert!(
            !platform.location_active(),
            "`start` delivered without anybody asking"
        );
        assert_eq!(
            platform.location_permission() == P::Unavailable,
            platform.os_location.provider.is_none(),
            "`Unavailable` must mean no provider and nothing else"
        );
    }
}
