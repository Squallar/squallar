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
    /// Receives fixes from the OS location service.
    ///
    /// A second channel rather than a second sender into the first, because the
    /// two sources have to be told apart: `poll_gps_fix` picks between them (see
    /// [`os_location::prefer_fix`]) and cannot do that once they are merged.
    ///
    /// `None` on the targets whose [`os_location`] arm is still `unsupported`,
    /// whose `start` never returns a reader.
    ///
    /// [`os_location`]: crate::os_location
    /// [`os_location::prefer_fix`]: crate::os_location::prefer_fix
    os_fix_receiver: Option<std::sync::mpsc::Receiver<rustdar_gps::GpsFix>>,
    /// The live subscription to the OS location service, dropped to stop.
    ///
    /// Also the answer to [`PlatformBridge::location_active`], which is why it
    /// is here and not a bool: "granted" and "delivering" are different states,
    /// and a separate flag is a second thing to keep in step with the reader
    /// that is actually producing.
    ///
    /// `None` on every target whose provider has not landed — `unsupported`'s
    /// reader is never constructed — and `None` on the ones that have until the
    /// user, or the gate, asks for a position.
    os_location_reader: Option<crate::os_location::OsLocationReader>,
    /// What the OS location service last said, or `None` before it has been
    /// asked anything.
    ///
    /// An atomic and not a `Cell`, because it is written from whatever thread
    /// the provider is given and read from
    /// [`location_permission`](PlatformBridge::location_permission), which is a
    /// `&self` getter on the frame path. That rules out a `Cell` (not `Send`),
    /// a `Receiver` (cannot be drained through `&self`) and a `Mutex` (a lock
    /// on the frame path). See [`encode_permission`].
    ///
    /// **It deliberately outlives the reader.** A revocation arrives as a
    /// permission change *and* stops delivery, and the gate responds to
    /// `Denied` by dropping the reader — so a state that lived inside the
    /// reader would evaporate at exactly the moment it started to matter, the
    /// bridge would fall back to "nobody has been asked", and the app would ask
    /// again straight into the refusal it just received.
    os_location_state: Option<std::sync::Arc<std::sync::atomic::AtomicU8>>,
    /// Windows: the `AppCapability` watcher behind
    /// [`PlatformBridge::location_permission`].
    ///
    /// Constructed with the bridge rather than with the reader above, and that
    /// ordering is the design. The permission has to be readable *before*
    /// anything starts — it is what decides whether starting is allowed — and
    /// has to stay readable after the user turns delivery off, or a permission
    /// revoked in Settings would never be noticed. It owns a worker thread; see
    /// [`crate::os_location::LocationService`].
    #[cfg(target_os = "windows")]
    os_location: crate::os_location::LocationService,
    /// Handed to the reader thread so a fix arriving while the loop is parked
    /// gets a frame to be shown on. See [`RedrawWaker`].
    redraw_waker: RedrawWaker,
}

/// [`rustdar_gps::LocationPermission`] as one byte, for the atomic above.
///
/// Hand-written rather than derived, and the discriminants are pinned by the
/// round-trip test at the bottom of this file: the enum is not `repr(u8)` and
/// nothing in `rustdar-gps` promises its variants keep their order, so a
/// `as u8` cast here would be a silent miscommunication the first time someone
/// inserts a variant.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
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
#[cfg(not(any(target_os = "android", target_os = "ios")))]
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
            os_fix_receiver: None,
            os_location_reader: None,
            os_location_state: None,
            #[cfg(target_os = "windows")]
            os_location: crate::os_location::LocationService::start(),
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

// ── Platform location service: the per-OS half ──────────────────────────
//
// One `cfg` pair, so that the `PlatformBridge` impl below reads the same on
// every desktop target and the difference between them is in exactly one place.
// Each provider that lands takes itself out of the `not(...)` arm; until all
// three have, the arm is what a desktop with no provider compiled answers.
//
// Only the four calls that genuinely differ are here. `stop_location` and
// `location_active` are not: both are about `os_location_reader`, which is the
// same field whoever filled it, and duplicating them per target would be two
// more places for a provider to forget to clear the dot.

/// Windows: `AppCapability` for the state, `Geolocator` to prompt and deliver.
/// See [`crate::os_location`].
#[cfg(target_os = "windows")]
impl DesktopPlatform {
    fn os_location_permission(&self) -> rustdar_gps::LocationPermission {
        self.os_location.permission()
    }

    /// Subscribe first, then ask.
    ///
    /// Both, and in that order, because they are one user-visible act. The gate
    /// calls this in two situations — never asked, and granted but not
    /// delivering — and neither would be served by a method that did only half
    /// of it. Subscribing before the answer arrives is deliberate: a
    /// `Geolocator` with no permission simply reports no positions, and having
    /// the subscription already in place is what makes a grant produce a fix
    /// immediately rather than a poll interval later.
    ///
    /// The `bool` says the request reached the worker. It cannot say more —
    /// whether a dialog appears is up to the Windows build, and whether it is
    /// answered is up to the user — which is why `PlatformBridge` documents
    /// that nothing durable may hang off it.
    fn os_location_request(&mut self) -> bool {
        if self.os_location_reader.is_none() {
            let (fixes, receiver) = std::sync::mpsc::channel();
            let wake = self.redraw_waker.clone();
            match self.os_location.start_delivery(fixes, move || wake.wake()) {
                Some(reader) => {
                    self.os_location_reader = Some(reader);
                    self.os_fix_receiver = Some(receiver);
                    log::info!("OS location delivery started");
                }
                None => log::warn!("the OS location worker is gone; no fixes will arrive"),
            }
        }
        self.os_location.request_access()
    }

    fn os_location_settings_available(&self) -> bool {
        true
    }

    fn os_location_open_settings(&mut self) {
        if !self.os_location.open_settings() {
            log::warn!("the OS location worker is gone; cannot open system settings");
        }
    }
}

/// Linux: GeoClue2 over `zbus`, on a connection the session thread owns.
/// See [`crate::os_location`].
#[cfg(target_os = "linux")]
impl DesktopPlatform {
    /// Whatever the provider last reported, or [`Prompt`] before it has been
    /// asked anything.
    ///
    /// **`Prompt` and not `Unknown` for the un-asked case, deliberately.**
    /// `Unknown` means "the platform has not answered yet, look again shortly",
    /// and this arm's provider does not answer *until it is started* — the
    /// first D-Bus round trip is the first answer, and it can sit on an agent
    /// dialog, so it cannot be made on the frame path. Reporting `Unknown`
    /// would leave the gate waiting for an answer that only asking produces:
    /// the settings pane would read "Checking…" forever and nothing would ever
    /// prompt. `Prompt` is also the honest description of the state — nobody
    /// has been asked — and the one state in which asking is legitimate.
    ///
    /// [`Prompt`]: rustdar_gps::LocationPermission::Prompt
    fn os_location_permission(&self) -> rustdar_gps::LocationPermission {
        match &self.os_location_state {
            Some(state) => decode_permission(state.load(std::sync::atomic::Ordering::Relaxed)),
            None => rustdar_gps::LocationPermission::Prompt,
        }
    }

    /// Start a location session, if one is not already running.
    ///
    /// The `bool` is the honest answer for this bridge and not much of one: it
    /// says a provider was constructed, which on Linux is true before any of
    /// the work that can fail has happened. See the trait's note — nothing
    /// durable may hang off it.
    fn os_location_request(&mut self) -> bool {
        if self.os_location_reader.is_some() {
            return true;
        }
        let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(encode_permission(
            rustdar_gps::LocationPermission::Unknown,
        )));
        self.os_location_state = Some(std::sync::Arc::clone(&state));

        let (tx, rx) = std::sync::mpsc::channel();
        let wake = self.redraw_waker.clone();
        // Both callbacks run on the provider's thread. The wake is what makes a
        // fix visible under `ControlFlow::Wait`; the report is what makes a
        // revocation visible at all, since the gate stops polling once the
        // answer is `Granted` and delivery is live.
        let reported = std::sync::Arc::clone(&state);
        let reader = crate::os_location::OsLocationReader::start(
            &rustdar_gps::GpsConfig::default(),
            tx,
            move || wake.wake(),
            move |permission| {
                reported.store(
                    encode_permission(permission),
                    std::sync::atomic::Ordering::Relaxed,
                );
            },
        );

        match reader {
            Some(reader) => {
                self.os_location_reader = Some(reader);
                self.os_fix_receiver = Some(rx);
                log::info!("OS location session requested");
                true
            }
            None => {
                // No provider compiled for this target. `Unavailable` and not
                // `Denied`: there is no switch anywhere that changes it, so the
                // pane must not send the user looking for one.
                state.store(
                    encode_permission(rustdar_gps::LocationPermission::Unavailable),
                    std::sync::atomic::Ordering::Relaxed,
                );
                false
            }
        }
    }

    /// No page to offer. GeoClue's permission is a property of the `.desktop`
    /// file and of the agent's own policy, not of a settings pane any desktop
    /// environment agrees on, so the button would be one that does nothing on
    /// most installs. `packaging/linux/README.md` is where the fix is written
    /// down instead.
    fn os_location_settings_available(&self) -> bool {
        false
    }

    fn os_location_open_settings(&mut self) {}
}

/// macOS: `CLLocationManager`, the same provider iOS reaches through
/// [`IosPlatform`]. See [`crate::os_location`].
#[cfg(target_os = "macos")]
impl DesktopPlatform {
    /// The provider's cached `CLAuthorizationStatus`, or [`Prompt`] before one
    /// has been built.
    ///
    /// `Prompt` and not `Unavailable` for the un-built case, for the reason the
    /// Linux arm gives above: `Unavailable` is terminal for the gate, so a
    /// bridge that answered it on frame one would never ask and never be asked
    /// again. Building a `CLLocationManager` prompts nobody, so the first
    /// `request_location` is free to do it.
    ///
    /// [`Prompt`]: rustdar_gps::LocationPermission::Prompt
    fn os_location_permission(&self) -> rustdar_gps::LocationPermission {
        self.os_location_reader.as_ref().map_or(
            rustdar_gps::LocationPermission::Prompt,
            crate::os_location::OsLocationReader::permission,
        )
    }

    /// Build the CoreLocation bridge if it is not up yet, then ask.
    ///
    /// The waker handed to it is the app's: this is only ever reached from a
    /// gate step, which is many frames after `set_redraw_waker` replaced the
    /// placeholder `new()` installed.
    fn os_location_request(&mut self) -> bool {
        if self.os_location_reader.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            let wake = self.redraw_waker.clone();
            self.os_location_reader = crate::os_location::OsLocationReader::start(
                &rustdar_gps::GpsConfig::default(),
                tx,
                move || wake.wake(),
            );
            // Only keep the receiver if something is going to push into it; an
            // open one would make `poll_gps_fix` drain a channel forever.
            self.os_fix_receiver = self.os_location_reader.is_some().then_some(rx);
        }
        self.os_location_reader
            .as_mut()
            .is_some_and(crate::os_location::OsLocationReader::request)
    }

    /// `x-apple.systempreferences:` URLs need `NSWorkspace`, which is AppKit
    /// and not in this crate's graph. The pane's "turn it back on in System
    /// Settings" wording stands on its own.
    fn os_location_settings_available(&self) -> bool {
        false
    }

    fn os_location_open_settings(&mut self) {}
}

/// Every other desktop target — wasm32 included.
///
/// `Unavailable` and not `Unknown`, and the difference matters. `Unknown` means
/// "ask again shortly", so the gate would poll a bridge that is never going to
/// answer and the settings pane would sit on "Checking…" for the life of the
/// process. `Unavailable` is the truth: this build has no OS location provider,
/// the pane says so, and nothing spins.
#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    not(target_os = "windows"),
    not(target_os = "linux"),
    not(target_os = "macos")
))]
impl DesktopPlatform {
    fn os_location_permission(&self) -> rustdar_gps::LocationPermission {
        rustdar_gps::LocationPermission::Unavailable
    }

    /// Nothing to ask, so nothing reached the OS.
    fn os_location_request(&mut self) -> bool {
        false
    }

    fn os_location_settings_available(&self) -> bool {
        false
    }

    fn os_location_open_settings(&mut self) {}
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
    // Every per-target decision is in the `cfg` set above this `impl`, so what
    // is left here is the part that is the same everywhere.

    fn location_permission(&self) -> rustdar_gps::LocationPermission {
        self.os_location_permission()
    }

    fn request_location(&mut self) -> bool {
        self.os_location_request()
    }

    /// Drops the subscription and the channel behind it. The permission is left
    /// exactly as the provider last reported it.
    ///
    /// Not a revocation — no platform lets an app hand a permission back — and
    /// not conditional on which provider filled the field. This is also the path
    /// a *revoked* permission takes, which is why it must actually release the
    /// receiver: leaving a drained-but-live channel in place would let a fix
    /// already in flight land on the map after consent was withdrawn.
    fn stop_location(&mut self) {
        if self.os_location_reader.take().is_some() {
            log::info!("OS location delivery stopped");
        }
        self.os_fix_receiver = None;
    }

    /// The reader itself, rather than a flag beside it. See the field.
    fn location_active(&self) -> bool {
        self.os_location_reader.is_some()
    }

    fn location_settings_available(&self) -> bool {
        self.os_location_settings_available()
    }

    fn open_location_settings(&mut self) {
        self.os_location_open_settings();
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
    /// Receives fixes from CoreLocation. There is no serial reader on iOS, so
    /// unlike the desktop bridge this is the only source and nothing has to be
    /// chosen between.
    os_fix_receiver: Option<std::sync::mpsc::Receiver<rustdar_gps::GpsFix>>,
    /// See `DesktopPlatform::os_location`; the reasoning is identical, and so
    /// is the code the two forward to.
    os_location: Option<crate::os_location::OsLocationReader>,
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
            os_fix_receiver: None,
            os_location: None,
            redraw_waker: RedrawWaker::new(),
        }
    }

    /// UIKit points `HOME` at the app's sandbox container, so this needs no
    /// `NSHomeDirectory` call and therefore no ObjC.
    fn sandbox_subdir(rel: &str) -> Option<std::path::PathBuf> {
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(rel))
    }

    /// Bring up CoreLocation. Same reasoning, same call, as
    /// [`DesktopPlatform::start_os_location`].
    ///
    /// One thing is genuinely different and it is upstream of here: this bridge
    /// is constructed before `UIApplicationMain` has run, so there is no
    /// `UIApplication` and no *running* run loop when the provider is built.
    /// Neither is required to build one — the main thread is still the main
    /// thread — and the callbacks CoreLocation schedules are delivered once
    /// UIKit starts spinning the loop a few milliseconds later.
    fn start_os_location(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = self.redraw_waker.clone();
        self.os_location = crate::os_location::OsLocationReader::start(
            &rustdar_gps::GpsConfig::default(),
            tx,
            move || wake.wake(),
        );
        self.os_fix_receiver = self.os_location.is_some().then_some(rx);
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
        self.os_fix_receiver.as_ref().and_then(drain_latest)
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

    /// See [`IosPlatform::start_os_location`] for why the waker's arrival is
    /// what brings CoreLocation up.
    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        self.redraw_waker = waker;
        self.start_os_location();
    }

    // ── Platform location service ───────────────────────────────────────
    //
    // Identical forwarding to `DesktopPlatform`'s, because it is the same
    // provider: `os_location`'s arm table selects `apple` for both. What
    // differs between the two platforms lives inside that file.

    fn location_permission(&self) -> rustdar_gps::LocationPermission {
        self.os_location.as_ref().map_or(
            rustdar_gps::LocationPermission::Unavailable,
            crate::os_location::OsLocationReader::permission,
        )
    }

    fn request_location(&mut self) -> bool {
        self.os_location
            .as_mut()
            .is_some_and(crate::os_location::OsLocationReader::request)
    }

    fn stop_location(&mut self) {
        if let Some(reader) = self.os_location.as_mut() {
            reader.stop();
        }
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
        self.os_location
            .as_ref()
            .is_some_and(crate::os_location::OsLocationReader::active)
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

    /// A fresh bridge has asked nothing, so it must report the one state in
    /// which asking is legitimate. `Unknown` here would park the gate waiting
    /// for an answer that only asking can produce, and `Unavailable` would
    /// declare a working machine incapable.
    #[test]
    fn a_bridge_that_has_not_asked_yet_reports_prompt() {
        let platform = DesktopPlatform::new();
        assert_eq!(platform.location_permission(), P::Prompt);
        assert!(!platform.location_active());
    }
}
