//! A [`PlatformBridge`] for tests, shaped after the four real ones.
//!
//! # What it is for
//!
//! `App` reaches the OS only through this trait, so a double is the only way to
//! drive an `App` at all on a host: everything from "does this platform let the
//! app quit" to "where does config go" arrives through it. The three
//! constructors below are named after the bridges they imitate, and their
//! answers are taken from those bridges rather than invented — see
//! `rustdar-platform`'s `platform.rs` and `rustdar-web`'s `bridge.rs`.
//!
//! # Why it records in fields rather than in a call log
//!
//! Every setter here keeps what it was handed, exactly as the real bridges do,
//! and the getters answer from it. That is deliberate: a log of "the app called
//! `set_insets_querier`" can only be asserted against the querier the test
//! itself supplied, which proves nothing about what the app did with it. Asking
//! instead what `query_insets` *now answers*, and then what the UI *now shows*,
//! puts production code between the fixture and the assertion. Every probe in
//! the suite is written that way; where a call really has no downstream
//! observable — `start_gps` is the only one — the argument is kept behind a
//! handle the test shares, and that is stated at the field.
//!
//! # Where it stops short of the real bridges
//!
//! Nothing here starts a thread. `AndroidPlatform::set_theme_detector` spawns a
//! two-second poller alongside the assignment, because NativeActivity emits no
//! `ThemeChanged`; this refuses a second detector the way that one does but
//! spawns nothing, and `theme_channel` is the test's stand-in for the poller.
//! That is why the refusal is keyed on the detector rather than on the receiver
//! the real one would have created.
//!
//! Nothing here is `Send`: `App` never requires it of a bridge, and `Rc` keeps
//! the shared handles cheap.

use crate::platform::{PlatformBridge, RedrawWaker, drain_latest};
use rustdar_egui::config_store::{ConfigStore, MemoryConfigStore};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Receiver;

/// System bar insets as `(top, bottom, left, right)`, as the trait carries them.
pub(crate) type Insets = (f32, f32, f32, f32);

/// A [`ConfigStore`] over a [`MemoryConfigStore`] the test still holds.
///
/// `config_store` hands out a fresh `Box` per call, so the backing store has to
/// outlive the box for a test to read back what the app persisted through it.
pub(crate) struct SharedStore {
    inner: Rc<MemoryConfigStore>,
    writes: Rc<std::cell::Cell<usize>>,
}

impl ConfigStore for SharedStore {
    fn load(&self, key: &str) -> Option<String> {
        self.inner.load(key)
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        // Counted, not just recorded. The autosave runs on a timer for the life
        // of the process, so "does an unchanged config still write?" is a
        // question about cost that reading the stored value cannot answer.
        self.writes.set(self.writes.get() + 1);
        self.inner.store(key, value)
    }
}

/// The GPS config the bridge was last started with, or `None` when stopped.
///
/// The one thing here with no downstream observable: `start_gps` opens a serial
/// port and nothing about the app changes. Shared so a test can see *which*
/// config reached it — passing the wrong one is the failure that matters, and
/// `gps_active` alone cannot tell.
pub(crate) type GpsRecord = Rc<RefCell<Option<rustdar_gps::GpsConfig>>>;

/// The [`RedrawWaker`] the app handed this bridge, or a fresh empty one if it
/// never did.
///
/// The second thing here kept behind a shared handle rather than read back
/// through a getter, and for the same reason as [`GpsRecord`]: the real bridges
/// spend a waker on threads (`DesktopPlatform::start_gps`,
/// `AndroidPlatform::set_theme_detector`) that this double deliberately does not
/// start, so there is no downstream observable to ask. Waking through it and
/// watching what the *app* installed fire is what puts production code between
/// the fixture and the assertion.
pub(crate) type WakerRecord = Rc<RefCell<RedrawWaker>>;

pub(crate) struct TestBridge {
    /// `false` on iOS, where `exit()` is an App Store rejection.
    supports_exit: bool,
    /// `true` on Android, where the event loop never unwinds.
    needs_process_exit: bool,
    /// `None` until the platform is told. Android learns its data path only
    /// after startup; desktop and iOS derive one at construction.
    config_dir: Option<PathBuf>,
    zone_cache_dir: Option<PathBuf>,
    store: Rc<MemoryConfigStore>,
    /// Installed by `set_back_handler`. Android installs one at startup and the
    /// others never do, which is the whole of why back minimises there and
    /// quits everywhere else.
    back_handler: Option<fn()>,
    /// Injected, as Android injects it. Only that platform has a second way to
    /// deliver back — `OnBackInvokedDispatcher`, which bypasses the input
    /// queue — so on the other two this stays `None` and `poll_back_press`
    /// answers `false` forever.
    back_press_taker: Option<fn() -> bool>,
    /// Injected, as Android injects it: the read is a JNI call.
    insets_querier: Option<fn() -> Insets>,
    /// Injected, as Android injects it.
    theme_detector: Option<fn() -> bool>,
    /// Whether the platform has a synchronous theme read of its own: desktop
    /// (`dark_light::detect`) and iOS (hard-coded light) do, Android does not.
    /// Both of those answer light here — a host-dependent desktop answer is
    /// exactly the kind of invention this double avoids, and nothing reads it.
    reads_theme_itself: bool,
    theme_receiver: Option<Receiver<bool>>,
    gps_fix_receiver: Option<Receiver<rustdar_gps::GpsFix>>,
    heading_receiver: Option<Receiver<f32>>,
    gps: GpsRecord,
    /// See [`WakerRecord`].
    waker: WakerRecord,
    writes: Rc<std::cell::Cell<usize>>,
    /// What `iana_timezone` answers. `None` stands for the platforms and
    /// environments that cannot say — a container with no zone configured, or a
    /// browser too old for `Intl` — where the app must keep its own default.
    timezone: Option<String>,
}

impl TestBridge {
    fn bare() -> Self {
        Self {
            supports_exit: true,
            needs_process_exit: false,
            config_dir: None,
            zone_cache_dir: None,
            store: Rc::new(MemoryConfigStore::default()),
            back_handler: None,
            back_press_taker: None,
            insets_querier: None,
            theme_detector: None,
            reads_theme_itself: true,
            theme_receiver: None,
            gps_fix_receiver: None,
            heading_receiver: None,
            gps: Rc::new(RefCell::new(None)),
            waker: Rc::new(RefCell::new(RedrawWaker::new())),
            writes: Rc::new(std::cell::Cell::new(0)),
            timezone: None,
        }
    }

    /// `DesktopPlatform`: knows where config goes from the start, has no system
    /// bars, installs no back handler, and exits through the event loop.
    pub(crate) fn desktop() -> Self {
        Self {
            config_dir: Some(PathBuf::from("/desktop/config")),
            zone_cache_dir: Some(PathBuf::from("/desktop/zones")),
            ..Self::bare()
        }
    }

    /// `AndroidPlatform`: theme, insets, GPS and compass all arrive through
    /// injected callbacks and channels, config has no home until `android_main`
    /// supplies one, and exit has to go through `process::exit`.
    pub(crate) fn android() -> Self {
        Self {
            needs_process_exit: true,
            reads_theme_itself: false,
            ..Self::bare()
        }
    }

    /// `IosPlatform`: every poll answers `None`, the theme is hard-coded light,
    /// egui-winit supplies the insets so the bridge has none, and quitting is
    /// not something the platform permits.
    pub(crate) fn ios() -> Self {
        Self {
            supports_exit: false,
            config_dir: Some(PathBuf::from("/ios/config")),
            zone_cache_dir: Some(PathBuf::from("/ios/zones")),
            ..Self::bare()
        }
    }

    /// A handle on the blobs `config_store` hands out, for seeding a config
    /// before the app loads it and for reading back what the app saved.
    pub(crate) fn store(&self) -> Rc<MemoryConfigStore> {
        Rc::clone(&self.store)
    }

    /// How many times the app has written config through this bridge.
    pub(crate) fn write_count(&self) -> Rc<std::cell::Cell<usize>> {
        Rc::clone(&self.writes)
    }

    /// See [`GpsRecord`].
    pub(crate) fn gps_record(&self) -> GpsRecord {
        Rc::clone(&self.gps)
    }

    /// See [`WakerRecord`]. Taken before the bridge is boxed into an `App`,
    /// like every other handle here.
    pub(crate) fn waker_record(&self) -> WakerRecord {
        Rc::clone(&self.waker)
    }

    /// Report `zone` as the device's IANA timezone.
    pub(crate) fn with_timezone(mut self, zone: &str) -> Self {
        self.timezone = Some(zone.to_string());
        self
    }

    /// Feed `poll_gps_fix`, standing in for the browser's geolocation watch and
    /// Android's location callbacks.
    pub(crate) fn gps_channel(&mut self) -> std::sync::mpsc::Sender<rustdar_gps::GpsFix> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.gps_fix_receiver = Some(rx);
        tx
    }

    /// Feed `poll_theme`, standing in for Android's polling thread.
    pub(crate) fn theme_channel(&mut self) -> std::sync::mpsc::Sender<bool> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.theme_receiver = Some(rx);
        tx
    }
}

impl PlatformBridge for TestBridge {
    fn poll_theme(&mut self) -> Option<bool> {
        self.theme_receiver.as_ref().and_then(drain_latest)
    }

    fn poll_gps_fix(&mut self) -> Option<rustdar_gps::GpsFix> {
        self.gps_fix_receiver.as_ref().and_then(drain_latest)
    }

    fn poll_heading(&mut self) -> Option<f32> {
        self.heading_receiver.as_ref().and_then(drain_latest)
    }

    fn query_insets(&self) -> Option<Insets> {
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
                // Android's only theme source is the injected detector, so a
                // missing one is a wiring bug rather than an answer;
                // `AndroidPlatform::detect_dark_theme` fails the same way.
                debug_assert!(
                    self.reads_theme_itself,
                    "TestBridge::android detect_dark_theme with no detector injected",
                );
                false
            }
        }
    }

    fn set_back_handler(&mut self, handler: fn()) {
        self.back_handler = Some(handler);
    }

    fn set_zone_cache_dir(&mut self, dir: PathBuf) {
        self.zone_cache_dir = Some(dir);
    }

    fn zone_cache_dir(&self) -> Option<&Path> {
        self.zone_cache_dir.as_deref()
    }

    fn set_config_dir(&mut self, dir: PathBuf) {
        self.config_dir = Some(dir);
    }

    /// `None` until this platform has been told where config lives, which is
    /// what makes `App::set_config_dir` observable: before it, there is no
    /// store to load from.
    fn config_store(&self) -> Option<Box<dyn ConfigStore>> {
        self.config_dir.as_ref().map(|_| {
            Box::new(SharedStore {
                inner: Rc::clone(&self.store),
                writes: Rc::clone(&self.writes),
            }) as Box<_>
        })
    }

    fn iana_timezone(&self) -> Option<String> {
        self.timezone.clone()
    }

    fn needs_process_exit(&self) -> bool {
        self.needs_process_exit
    }

    fn supports_exit(&self) -> bool {
        self.supports_exit
    }

    /// Kept, as the real bridges keep it — they hand it to threads this double
    /// does not start, so [`WakerRecord`] is where a test picks it up.
    fn set_redraw_waker(&mut self, waker: RedrawWaker) {
        *self.waker.borrow_mut() = waker;
    }

    fn set_gps_fix_receiver(&mut self, receiver: Receiver<rustdar_gps::GpsFix>) {
        self.gps_fix_receiver = Some(receiver);
    }

    fn set_heading_receiver(&mut self, receiver: Receiver<f32>) {
        self.heading_receiver = Some(receiver);
    }

    fn set_insets_querier(&mut self, querier: fn() -> Insets) {
        self.insets_querier = Some(querier);
    }

    /// A second detector is refused, as `AndroidPlatform` refuses one: there,
    /// accepting would leave the running poll thread on the old detector while
    /// the synchronous path used the new one.
    fn set_theme_detector(&mut self, detector: fn() -> bool) {
        if self.theme_detector.is_some() {
            return;
        }
        self.theme_detector = Some(detector);
    }

    fn start_gps(&mut self, config: &rustdar_gps::GpsConfig) {
        *self.gps.borrow_mut() = Some(config.clone());
    }

    fn stop_gps(&mut self) {
        *self.gps.borrow_mut() = None;
    }

    fn gps_active(&self) -> bool {
        self.gps.borrow().is_some()
    }
}
