//! Concrete [`PlatformBridge`] implementations. The trait lives in
//! `squallar-app`, which must never name a per-OS type.

#[cfg(target_os = "android")]
use squallar_app::platform::drain_latest;
use squallar_app::platform::{FormFactor, HostSignals, PlatformBridge, RedrawWaker};

/// System bar insets as `(top, bottom, left, right)`. Aliased because
/// `clippy::type_complexity` rejects the bare fn pointer in the field below.
#[cfg(target_os = "android")]
type InsetsQuerier = fn() -> (f32, f32, f32, f32);

/// Where the web deploy serves the committed NWS zone-geometry pack
/// (`squallar-web/zones.pack`, staged beside `index.html` by `build.yaml`'s
/// `web-wasm32` row). Native targets download it from here once, keep it
/// beside the zone cache, and read the file thereafter; the web build resolves
/// the same asset relative to its own page instead.
///
/// The fifth literal of the squallar.app cutover (2026-08-30): it moved in the
/// same change as `deploy-cloudfront`'s role, bucket and distribution, because
/// a native build that downloads its pack from an origin the deploy no longer
/// writes would pin users to whatever bytes the tombstone leaves behind.
pub const ZONE_PACK_URL: &str = "https://squallar.app/zones.pack";

/// Point zone resolution at the deployed pack — the native mirror of
/// `squallar-web`'s `name_the_zone_pack`. Called once by each entry point,
/// before the first alerts round consumes it. Nothing is fetched here: the
/// download happens later, on the alerts fetch task, and only when no pack
/// file is already beside the zone cache. A failed download degrades to the
/// HTTP zone resolution that shipped before the pack existed.
pub fn name_the_zone_pack() {
    squallar_overlays::nws::zone_pack_source::use_download_url(ZONE_PACK_URL.to_string());
}

/// This machine's IANA timezone name, or `None` if it cannot be determined.
///
/// A failure here is ordinary: a container with no `/etc/localtime`, or a `TZ`
/// naming a POSIX offset rather than a zone. The caller falls back to its
/// compiled-in default site.
fn system_timezone() -> Option<String> {
    match iana_time_zone::get_timezone() {
        Ok(zone) => Some(zone),
        Err(e) => {
            log::debug!("no system timezone available: {e}");
            None
        }
    }
}

// ── Desktop implementation ──

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub struct DesktopPlatform {
    back_handler: Option<fn()>,
    zone_cache_dir: Option<std::path::PathBuf>,
    basemap_cache_dir: Option<std::path::PathBuf>,
    basemap_dir: Option<std::path::PathBuf>,
    config_dir: Option<std::path::PathBuf>,
    /// Handed to the theme/back producers this bridge starts.
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
            basemap_cache_dir: Self::default_basemap_cache_dir(),
            basemap_dir: Self::default_basemap_dir(),
            config_dir: Self::default_config_dir(),
            redraw_waker: RedrawWaker::new(),
        }
    }

    fn default_config_dir() -> Option<std::path::PathBuf> {
        let base = std::env::var("XDG_CONFIG_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.config", h)))
            .or_else(|_| std::env::var("LOCALAPPDATA"))
            .ok()?;
        Some(std::path::PathBuf::from(base).join("squallar"))
    }

    fn default_zone_cache_dir() -> Option<std::path::PathBuf> {
        let base = std::env::var("XDG_CACHE_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.cache", h)))
            .or_else(|_| std::env::var("LOCALAPPDATA"))
            .ok()?;
        Some(
            std::path::PathBuf::from(base)
                .join("squallar")
                .join("zones"),
        )
    }

    /// `default_zone_cache_dir`'s twin: the archive block cache, a sibling
    /// directory so both live under one clearable `squallar` cache root.
    fn default_basemap_cache_dir() -> Option<std::path::PathBuf> {
        let base = std::env::var("XDG_CACHE_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.cache", h)))
            .or_else(|_| std::env::var("LOCALAPPDATA"))
            .ok()?;
        Some(
            std::path::PathBuf::from(base)
                .join("squallar")
                .join("basemap"),
        )
    }

    /// Where downloaded offline basemap areas persist. The same root and the
    /// same fallback chain as `default_zone_cache_dir`: nothing on a desktop
    /// clears this behind the user, so the one clearable `squallar` root
    /// keeps all three together. A distinct leaf from `basemap` — that one
    /// is the evictable block cache, and the GC there must never walk over
    /// the user's downloads. The directory is not created here; that is the
    /// download engine's job.
    fn default_basemap_dir() -> Option<std::path::PathBuf> {
        let base = std::env::var("XDG_CACHE_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.cache", h)))
            .or_else(|_| std::env::var("LOCALAPPDATA"))
            .ok()?;
        Some(
            std::path::PathBuf::from(base)
                .join("squallar")
                .join("basemap-downloads"),
        )
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl PlatformBridge for DesktopPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        // Desktop uses WindowEvent::ThemeChanged; no polling needed.
        None
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

    /// **The desktop theme, at startup, with the portal's timeout allowed for.**
    ///
    /// `dark_light` gives the XDG desktop portal **25 ms** to answer a D-Bus
    /// round trip and returns `Err(Timeout)` otherwise. This used to be a bare
    /// `matches!(.., Ok(Dark))`, so a missed 25 ms window read as "light" —
    /// and on desktop this runs exactly once, because [`Self::poll_theme`]
    /// answers `None` on the grounds that `WindowEvent::ThemeChanged` covers
    /// changes. A change is not what is being missed: the FIRST answer is, and
    /// nothing asks again. The result is Squallar opening light on a dark
    /// desktop and staying that way until the user changes their theme.
    ///
    /// Found while shooting marketing captures ten at a time, where the loaded
    /// machine lost the 25 ms race on 15 of 105 launches and rendered the
    /// basemap in light mode under dark chrome. The load made it frequent; it
    /// was never impossible on one launch, and a cold portal at login is the
    /// same race.
    ///
    /// Three attempts, so the budget is ~75 ms of startup in the bad case and
    /// unchanged in the good one — the first call returns immediately when the
    /// portal is up. Not spawned in the background, because the theme is read
    /// before the first frame is drawn and an answer that arrives later would
    /// repaint the whole map in front of the user.
    ///
    /// **"Could not tell" resolves to DARK, and that is a change.** It used to
    /// resolve to light. The chrome is dark whatever this returns, so a light
    /// basemap under it was the app disagreeing with itself; `NoPreference`
    /// gets the same treatment for the same reason.
    fn detect_dark_theme(&self) -> bool {
        const ATTEMPTS: usize = 3;
        for attempt in 0..ATTEMPTS {
            match dark_light::detect() {
                Ok(dark_light::Mode::Dark) => return true,
                Ok(dark_light::Mode::Light) => return false,
                // The portal answered and said "no preference": no retry will
                // turn that into an opinion.
                Ok(dark_light::Mode::Unspecified) => break,
                Err(e) => {
                    log::debug!("theme probe {}/{ATTEMPTS} did not answer: {e}", attempt + 1);
                }
            }
        }
        log::debug!("no theme preference could be read; using dark");
        true
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

    fn set_basemap_cache_dir(&mut self, dir: std::path::PathBuf) {
        self.basemap_cache_dir = Some(dir);
    }

    fn basemap_cache_dir(&self) -> Option<&std::path::Path> {
        self.basemap_cache_dir.as_deref()
    }

    fn set_basemap_dir(&mut self, dir: std::path::PathBuf) {
        self.basemap_dir = Some(dir);
    }

    fn basemap_dir(&self) -> Option<&std::path::Path> {
        self.basemap_dir.as_deref()
    }

    fn set_config_dir(&mut self, dir: std::path::PathBuf) {
        self.config_dir = Some(dir);
    }

    fn iana_timezone(&self) -> Option<String> {
        system_timezone()
    }

    fn needs_process_exit(&self) -> bool {
        false
    }

    /// A desktop build is a desktop, whatever pointer is plugged in: a build
    /// fact, not a reading. RAM and threads come from `crate::capacity`.
    fn host_signals(&self) -> HostSignals {
        crate::capacity::host_signals(FormFactor::Desktop)
    }

    /// Handed over before any window exists, which is what [`RedrawWaker`]'s slot
    /// is for.
    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        self.redraw_waker = waker;
    }

    fn kv(&self) -> Option<Box<dyn squallar_kv::KvStore>> {
        self.config_dir
            .clone()
            .map(|dir| Box::new(crate::kv::FileKvStore::new(dir)) as Box<_>)
    }
}

// ── Android implementation ──

#[cfg(target_os = "android")]
pub struct AndroidPlatform {
    /// Injected by `android::entry`: the read is a JNI call, and the bridge stays
    /// `deny(unsafe_code)`-clean by the injection rule in `src/android/mod.rs`.
    theme_detector: Option<fn() -> bool>,
    /// Theme changes from the poll thread `set_theme_detector` starts.
    theme_receiver: Option<std::sync::mpsc::Receiver<bool>>,
    heading_receiver: Option<std::sync::mpsc::Receiver<f32>>,
    insets_querier: Option<InsetsQuerier>,
    back_handler: Option<fn()>,
    /// Injected by `android::entry`: the flag it reads is set by the JNI callback
    /// `BackHandler.kt` invokes on the UI thread (`android::back`).
    back_press_taker: Option<fn() -> bool>,
    /// Injected by `android::entry`: the `Activity.isFinishing()` read that
    /// tells a suspend caused by a finish from one caused by backgrounding.
    terminal_suspend_probe: Option<fn() -> bool>,
    /// Injected by `android::entry`: the JNI static call that publishes this
    /// app's claim on the next press to `BackHandler.setClaimed`.
    back_claim_reporter: Option<fn(bool)>,
    zone_cache_dir: Option<std::path::PathBuf>,
    basemap_cache_dir: Option<std::path::PathBuf>,
    /// Downloaded offline basemap areas. Unlike its two cache siblings this
    /// must be **durable**: Android advertises "Clear cache" as safe and
    /// sheds `getCacheDir()` under storage pressure, and an offline area that
    /// silently vanishes defeats the low-bandwidth purpose it was downloaded
    /// for. `android_main` derives it under the files root (`config`'s
    /// parent), and — unlike the siblings — sets it on this bridge *before*
    /// `App::new`, because the Gui learns it at construction and there is no
    /// setter to push it through later.
    basemap_dir: Option<std::path::PathBuf>,
    config_dir: Option<std::path::PathBuf>,
    /// Handed to the theme poller below, so a light/dark switch noticed on that
    /// thread gets a frame to be applied on.
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
            heading_receiver: None,
            insets_querier: None,
            back_handler: None,
            back_press_taker: None,
            terminal_suspend_probe: None,
            back_claim_reporter: None,
            zone_cache_dir: None,
            basemap_cache_dir: None,
            basemap_dir: None,
            config_dir: None,
            redraw_waker: RedrawWaker::new(),
        }
    }
}

#[cfg(target_os = "android")]
impl PlatformBridge for AndroidPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        self.theme_receiver.as_ref().and_then(drain_latest)
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

    /// Absent probe answers `false`: an app that has not installed one is an
    /// app that would rather stay running than end a loop by accident.
    fn suspend_is_terminal(&self) -> bool {
        self.terminal_suspend_probe
            .is_some_and(|finishing| finishing())
    }

    fn set_terminal_suspend_probe(&mut self, probe: fn() -> bool) {
        self.terminal_suspend_probe = Some(probe);
    }

    fn set_back_claimed(&mut self, claimed: bool) {
        if let Some(report) = self.back_claim_reporter {
            report(claimed);
        }
    }

    fn set_back_claim_reporter(&mut self, reporter: fn(bool)) {
        self.back_claim_reporter = Some(reporter);
    }

    fn detect_dark_theme(&self) -> bool {
        match self.theme_detector {
            Some(detect) => detect(),
            None => {
                // Loud because the failure is invisible: NativeActivity never emits
                // `WindowEvent::ThemeChanged`, so the poll is the only theme input.
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

    fn set_basemap_cache_dir(&mut self, dir: std::path::PathBuf) {
        self.basemap_cache_dir = Some(dir);
    }

    fn basemap_cache_dir(&self) -> Option<&std::path::Path> {
        self.basemap_cache_dir.as_deref()
    }

    fn set_basemap_dir(&mut self, dir: std::path::PathBuf) {
        self.basemap_dir = Some(dir);
    }

    fn basemap_dir(&self) -> Option<&std::path::Path> {
        self.basemap_dir.as_deref()
    }

    fn set_config_dir(&mut self, dir: std::path::PathBuf) {
        self.config_dir = Some(dir);
    }

    fn iana_timezone(&self) -> Option<String> {
        system_timezone()
    }

    fn needs_process_exit(&self) -> bool {
        true
    }

    /// An Android build is a handheld: a build fact, not a reading. RAM is
    /// `/proc/meminfo`, the same reader as Linux.
    fn host_signals(&self) -> HostSignals {
        crate::capacity::host_signals(FormFactor::Handheld)
    }

    /// Taken before the theme poller is started, which is the only ordering this
    /// bridge depends on.
    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        self.redraw_waker = waker;
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
            // Refuse rather than half-apply: assigning would leave the synchronous path on
            // the new detector while the running thread keeps calling the old one.
            log::warn!("theme detector already installed; ignoring the second one");
            return;
        }
        self.theme_detector = Some(detector);

        match squallar_app::platform::spawn_state_poller(
            "theme-detect",
            std::time::Duration::from_secs(2),
            detector,
            self.redraw_waker.clone(),
        ) {
            Ok(receiver) => self.theme_receiver = Some(receiver),
            // Not fatal: `detect_dark_theme` still answers synchronously; it just stops
            // tracking changes.
            Err(e) => {
                log::error!("could not start theme polling, theme will not track changes: {e}")
            }
        }
    }

    fn kv(&self) -> Option<Box<dyn squallar_kv::KvStore>> {
        self.config_dir
            .clone()
            .map(|dir| Box::new(crate::kv::FileKvStore::new(dir)) as Box<_>)
    }
}

// ── iOS implementation ──
//
// Compass and theme are still the next unit of work and are `None` here.
//
// There is no insets querier and must not be one: egui-winit already fills
// `RawInput::safe_area_insets` on iOS. Android's side channel works around a
// platform gap iOS does not have.

#[cfg(target_os = "ios")]
pub struct IosPlatform {
    back_handler: Option<fn()>,
    zone_cache_dir: Option<std::path::PathBuf>,
    basemap_cache_dir: Option<std::path::PathBuf>,
    basemap_dir: Option<std::path::PathBuf>,
    config_dir: Option<std::path::PathBuf>,
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
            zone_cache_dir: Self::sandbox_subdir("Library/Caches/squallar/zones"),
            basemap_cache_dir: Self::sandbox_subdir("Library/Caches/squallar/basemap"),
            // Downloaded offline areas. Caches, like its siblings, and on
            // Apple's own rule: re-downloadable data belongs there, and the
            // Application Support alternative would put up to hundreds of MB
            // of re-fetchable tiles into every iCloud backup. The trade is
            // that iOS may purge Caches under severe storage pressure — the
            // download engine's generation handling already has to cope with
            // missing bytes, so purge lands on a handled path.
            basemap_dir: Self::sandbox_subdir("Library/Caches/squallar/basemap-downloads"),
            config_dir: Self::sandbox_subdir("Library/Application Support/squallar"),
            redraw_waker: RedrawWaker::new(),
        }
    }

    /// UIKit points `HOME` at the app's sandbox container, so this needs no ObjC.
    fn sandbox_subdir(rel: &str) -> Option<std::path::PathBuf> {
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(rel))
    }
}

#[cfg(target_os = "ios")]
impl PlatformBridge for IosPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
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

    fn set_basemap_cache_dir(&mut self, dir: std::path::PathBuf) {
        self.basemap_cache_dir = Some(dir);
    }

    fn basemap_cache_dir(&self) -> Option<&std::path::Path> {
        self.basemap_cache_dir.as_deref()
    }

    fn set_basemap_dir(&mut self, dir: std::path::PathBuf) {
        self.basemap_dir = Some(dir);
    }

    fn basemap_dir(&self) -> Option<&std::path::Path> {
        self.basemap_dir.as_deref()
    }

    fn set_config_dir(&mut self, dir: std::path::PathBuf) {
        self.config_dir = Some(dir);
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

    /// An iOS build is a handheld: a build fact, not a reading.
    fn host_signals(&self) -> HostSignals {
        crate::capacity::host_signals(FormFactor::Handheld)
    }

    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        self.redraw_waker = waker;
    }

    fn kv(&self) -> Option<Box<dyn squallar_kv::KvStore>> {
        self.config_dir
            .clone()
            .map(|dir| Box::new(crate::kv::FileKvStore::new(dir)) as Box<_>)
    }
}

/// Create the platform-appropriate bridge.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn create_platform() -> DesktopPlatform {
    DesktopPlatform::new()
}

/// Create the platform-appropriate location facade: the arm is per-OS and
/// feature-fenced inside squallar-location. Android's JNI arm requires
/// `squallar_location::android::init` first.
#[cfg(not(target_os = "android"))]
pub fn create_location() -> squallar_location::LocationFacade {
    squallar_location::LocationFacade::new(Box::new(
        squallar_location::os_location::OsBackend::new(),
    ))
}

/// See the non-android arm above.
#[cfg(target_os = "android")]
pub fn create_location() -> squallar_location::LocationFacade {
    squallar_location::LocationFacade::new(Box::new(
        squallar_location::android::AndroidBackend::new(),
    ))
}

#[cfg(target_os = "android")]
pub fn create_platform() -> AndroidPlatform {
    AndroidPlatform::new()
}

#[cfg(target_os = "ios")]
pub fn create_platform() -> IosPlatform {
    IosPlatform::new()
}
