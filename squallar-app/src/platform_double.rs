//! A [`PlatformBridge`] for tests, shaped after the four real ones.

use crate::platform::{PlatformBridge, RedrawWaker, drain_latest};
use squallar_kv::{KvStore, MemoryKvStore};
use squallar_location::LocationPermission;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Receiver;

/// System bar insets as `(top, bottom, left, right)`, as the trait carries them.
pub(crate) type Insets = (f32, f32, f32, f32);

/// A [`KvStore`] over a [`MemoryKvStore`] the test still holds.
pub(crate) struct SharedStore {
    inner: Rc<MemoryKvStore>,
    writes: Rc<std::cell::Cell<usize>>,
}

impl KvStore for SharedStore {
    fn load(&self, key: &str) -> Option<String> {
        self.inner.load(key)
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        // Counted, not just recorded.
        self.writes.set(self.writes.get() + 1);
        self.inner.store(key, value)
    }

    /// Spelled out rather than left to the trait's default, which would forward to
    /// `store` and count — correct today only by coincidence.
    fn store_now(&self, key: &str, value: &str) -> Result<(), String> {
        self.writes.set(self.writes.get() + 1);
        self.inner.store_now(key, value)
    }
}

/// The serial config the provider was last started with, or `None` when
/// stopped.
pub(crate) type GpsRecord = Rc<RefCell<Option<squallar_nmea_serial::SerialConfig>>>;

/// The [`RedrawWaker`] the app handed this bridge, or a fresh empty one if it
/// never did.
pub(crate) type WakerRecord = Rc<RefCell<RedrawWaker>>;

/// When [`TestBridge::kv`] answers with a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreAvailability {
    /// Only once a config directory has been set. Desktop and iOS derive one in
    /// their constructors; Android is told during `android_main`.
    WhenToldADirectory,
    /// Always.
    Always,
    /// Never.
    Never,
}

/// The location state a test shares with the bridge after `App` has taken it.
#[derive(Clone)]
pub(crate) struct LocationRecord {
    /// What `location_permission` answers.
    pub(crate) permission: Rc<Cell<LocationPermission>>,
    /// Whether the bridge is delivering.
    pub(crate) active: Rc<Cell<bool>>,
    /// How many times `request_location` has been called.
    pub(crate) requests: Rc<Cell<usize>>,
    /// How many times `location_permission` has been read. On Android that is a
    /// JNI call, so the poll cadence is a cost worth asserting on.
    pub(crate) queries: Rc<Cell<usize>>,
    /// What `request_location` returns. `true` on every real bridge but
    /// Android's, which is the only one that can tell.
    pub(crate) reaches_the_os: Rc<Cell<bool>>,
    /// What the app last told the bridge about how many times it has asked.
    pub(crate) attempts: Rc<Cell<Option<u8>>>,
}

impl Default for LocationRecord {
    fn default() -> Self {
        Self {
            permission: Rc::new(Cell::new(LocationPermission::default())),
            active: Rc::new(Cell::new(false)),
            requests: Rc::new(Cell::new(0)),
            queries: Rc::new(Cell::new(0)),
            reaches_the_os: Rc::new(Cell::new(true)),
            attempts: Rc::new(Cell::new(None)),
        }
    }
}

pub(crate) struct TestBridge {
    /// `false` on iOS, where `exit()` is an App Store rejection.
    supports_exit: bool,
    /// `true` on Android, where the event loop never unwinds.
    needs_process_exit: bool,
    /// `None` until the platform is told. Android learns its data path only
    /// after startup; desktop and iOS derive one at construction.
    config_dir: Option<PathBuf>,
    zone_cache_dir: Option<PathBuf>,
    store: Rc<MemoryKvStore>,
    /// Installed by `set_back_handler`.
    back_handler: Option<fn()>,
    /// What `exits_on_unhandled_back` answers. `false` on every bridge that
    /// ships — see the trait — so the `true` arm has no other way to be reached.
    exits_on_unhandled_back: bool,
    /// Injected, as Android injects it.
    back_press_taker: Option<fn() -> bool>,
    /// Injected, as Android injects it: the read is a JNI call.
    insets_querier: Option<fn() -> Insets>,
    /// Injected, as Android injects it.
    theme_detector: Option<fn() -> bool>,
    /// Whether the platform has a synchronous theme read of its own: desktop
    /// (`dark_light::detect`) and iOS (hard-coded light) do, Android does not.
    reads_theme_itself: bool,
    theme_receiver: Option<Receiver<bool>>,
    gps_fix_receiver: Option<Receiver<squallar_location::Fix>>,
    heading_receiver: Option<Receiver<f32>>,
    gps: GpsRecord,
    /// See [`WakerRecord`].
    waker: WakerRecord,
    writes: Rc<Cell<usize>>,
    /// See [`StoreAvailability`].
    store_availability: StoreAvailability,
    /// What `iana_timezone` answers.
    timezone: Option<String>,
    /// See [`LocationRecord`].
    location: LocationRecord,
    /// Every value `set_back_claimed` was handed, in order. A log rather than a
    /// flag: the whole contract is that the push is *edge-triggered*, and only
    /// the sequence can show a repeat that should not have been sent.
    back_claims: Rc<RefCell<Vec<bool>>>,
}

impl TestBridge {
    fn bare() -> Self {
        Self {
            supports_exit: true,
            needs_process_exit: false,
            config_dir: None,
            zone_cache_dir: None,
            store: Rc::new(MemoryKvStore::default()),
            back_handler: None,
            exits_on_unhandled_back: false,
            back_press_taker: None,
            insets_querier: None,
            theme_detector: None,
            reads_theme_itself: true,
            theme_receiver: None,
            gps_fix_receiver: None,
            heading_receiver: None,
            gps: Rc::new(RefCell::new(None)),
            waker: Rc::new(RefCell::new(RedrawWaker::new())),
            writes: Rc::new(Cell::new(0)),
            store_availability: StoreAvailability::WhenToldADirectory,
            timezone: None,
            location: LocationRecord::default(),
            back_claims: Rc::new(RefCell::new(Vec::new())),
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
    /// injected callbacks and channels; config has no home until `android_main`.
    pub(crate) fn android() -> Self {
        Self {
            needs_process_exit: true,
            reads_theme_itself: false,
            ..Self::bare()
        }
    }

    /// `IosPlatform`: every poll answers `None`, the theme is hard-coded light, egui-
    /// winit supplies the insets so the bridge has none.
    pub(crate) fn ios() -> Self {
        Self {
            supports_exit: false,
            config_dir: Some(PathBuf::from("/ios/config")),
            zone_cache_dir: Some(PathBuf::from("/ios/zones")),
            ..Self::bare()
        }
    }

    /// `WebPlatform`: `localStorage` from the first frame with no directory involved,
    /// no filesystem for the zone cache, no back handler.
    pub(crate) fn web() -> Self {
        Self {
            store_availability: StoreAvailability::Always,
            ..Self::bare()
        }
    }

    /// A platform that quits when a back press finds nothing to close.
    ///
    /// No shipped bridge is one, deliberately (see
    /// [`PlatformBridge::exits_on_unhandled_back`]). This exists so the arm that
    /// serves one is reachable at all: without it the `Exit` resolution has no
    /// caller and the dispatch that quits is unexercised, which is a worse state
    /// than an opt-in nobody has taken.
    pub(crate) fn that_quits_on_unhandled_back(mut self) -> Self {
        self.exits_on_unhandled_back = true;
        self
    }

    /// Every predictive-back claim the app has pushed, in order. Taken before the
    /// bridge is handed to `App`, which owns it from then on.
    pub(crate) fn back_claim_log(&self) -> Rc<RefCell<Vec<bool>>> {
        Rc::clone(&self.back_claims)
    }

    /// A handle on the blobs `kv` hands out, for seeding a config
    /// before the app loads it and for reading back what the app saved.
    pub(crate) fn store(&self) -> Rc<MemoryKvStore> {
        Rc::clone(&self.store)
    }

    /// Persist into `store` rather than into a fresh one, so a test can close an app
    /// and open another over the same blobs.
    pub(crate) fn with_store(mut self, store: Rc<MemoryKvStore>) -> Self {
        self.store = store;
        self
    }

    /// Answer `kv` with `None`, permanently.
    pub(crate) fn without_kv(mut self) -> Self {
        self.store_availability = StoreAvailability::Never;
        self
    }

    /// What `location_permission` answers to begin with.
    pub(crate) fn with_permission(self, permission: LocationPermission) -> Self {
        self.location.permission.set(permission);
        self
    }

    /// See [`LocationRecord`]. Taken before the bridge is boxed into an `App`.
    pub(crate) fn location_record(&self) -> LocationRecord {
        self.location.clone()
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

    /// Feed the provider's `poll_fix`, standing in for the browser's geolocation watch
    /// and Android's location poll.
    pub(crate) fn gps_channel(&mut self) -> std::sync::mpsc::Sender<squallar_location::Fix> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.gps_fix_receiver = Some(rx);
        tx
    }

    /// Mint the facade arm this bridge's records back.
    pub(crate) fn location_provider(&mut self) -> TestLocationProvider {
        TestLocationProvider {
            record: self.location.clone(),
            gps: Rc::clone(&self.gps),
            fixes: self.gps_fix_receiver.take(),
        }
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

    fn exits_on_unhandled_back(&self) -> bool {
        self.exits_on_unhandled_back
    }

    fn poll_back_press(&mut self) -> bool {
        self.back_press_taker.is_some_and(|take| take())
    }

    fn set_back_press_taker(&mut self, taker: fn() -> bool) {
        self.back_press_taker = Some(taker);
    }

    fn set_back_claimed(&mut self, claimed: bool) {
        self.back_claims.borrow_mut().push(claimed);
    }

    fn detect_dark_theme(&self) -> bool {
        match self.theme_detector {
            Some(detect) => detect(),
            None => {
                // Android's only theme source is the injected detector.
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

    fn set_heading_receiver(&mut self, receiver: Receiver<f32>) {
        self.heading_receiver = Some(receiver);
    }

    fn set_insets_querier(&mut self, querier: fn() -> Insets) {
        self.insets_querier = Some(querier);
    }

    /// A second detector is refused, as `AndroidPlatform` refuses one.
    fn set_theme_detector(&mut self, detector: fn() -> bool) {
        if self.theme_detector.is_some() {
            return;
        }
        self.theme_detector = Some(detector);
    }

    /// `None` until this platform has been told where config lives, which is what
    /// makes `App::set_config_dir` observable.
    fn kv(&self) -> Option<Box<dyn KvStore>> {
        let available = match self.store_availability {
            StoreAvailability::WhenToldADirectory => self.config_dir.is_some(),
            StoreAvailability::Always => true,
            StoreAvailability::Never => false,
        };
        available.then(|| {
            Box::new(SharedStore {
                inner: Rc::clone(&self.store),
                writes: Rc::clone(&self.writes),
            }) as Box<_>
        })
    }
}

/// The facade's arm, for tests: the location half of the old `TestBridge`.
pub(crate) struct TestLocationProvider {
    record: LocationRecord,
    /// See [`GpsRecord`].
    gps: GpsRecord,
    /// Fixes the test pushes through [`TestBridge::gps_channel`], standing in
    /// for the browser's geolocation watch and Android's location poll.
    fixes: Option<Receiver<squallar_location::Fix>>,
}

impl squallar_location::LocationProvider for TestLocationProvider {
    fn permission(&self) -> LocationPermission {
        self.record.queries.set(self.record.queries.get() + 1);
        self.record.permission.get()
    }

    /// Starts delivery, as every real arm does — the web's `watchPosition`
    /// is literally the same call as the prompt.
    fn request(&mut self) -> bool {
        self.record.requests.set(self.record.requests.get() + 1);
        let reached = self.record.reaches_the_os.get();
        if reached && self.record.permission.get() == LocationPermission::Granted {
            self.record.active.set(true);
        }
        reached
    }

    fn stop(&mut self) {
        self.record.active.set(false);
    }

    fn active(&self) -> bool {
        self.record.active.get()
    }

    fn set_attempts(&mut self, attempts: u8) {
        self.record.attempts.set(Some(attempts));
    }

    fn poll_fix(&mut self) -> Option<squallar_location::Fix> {
        self.fixes.as_ref().and_then(drain_latest)
    }

    fn start_serial(&mut self, config: &squallar_nmea_serial::SerialConfig) {
        *self.gps.borrow_mut() = Some(config.clone());
    }

    fn stop_serial(&mut self) {
        *self.gps.borrow_mut() = None;
    }

    fn serial_active(&self) -> bool {
        self.gps.borrow().is_some()
    }
}
