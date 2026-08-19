//! A purpose-built [`LocationBridge`] double for the gate suite.
//!
//! Not the app crate's `TestBridge` — that double is `pub(crate)` there and
//! exporting it would put a whole platform surface on this crate's public
//! face. This one implements exactly the six-method bridge the gate sees,
//! with the same recording surface the suite has always used, and its three
//! same-named constructors keep the ports one-word mechanical.

use crate::bridge::LocationBridge;
use crate::permission::LocationPermission;
use rustdar_kv::{KvStore, MemoryKvStore};
use std::cell::Cell;
use std::rc::Rc;

/// When [`GateDouble::kv`] answers with a store.
///
/// Three real cases, not two: the third is the one the location memo cares
/// about, and it used to be unreachable through the app crate's double.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreAvailability {
    /// Only once a config directory has been set. Desktop and iOS derive one in
    /// their constructors; Android is told during `android_main`.
    WhenToldADirectory,
    /// Always. `localStorage` needs no path and is there from the first frame,
    /// which is why the web bridge never returns `None` for "not told where
    /// yet".
    Always,
    /// Never. A browser with site data blocked, or a desktop process with no
    /// `XDG_CONFIG_HOME`, `HOME` or `LOCALAPPDATA` — a container, a systemd
    /// unit. Both are documented in the bridges they come from, and on both the
    /// location permission itself works fine.
    Never,
}

/// The location state a test shares with the double.
///
/// Every field is behind an `Rc<Cell<_>>` so the test can still touch them
/// after the gate has taken the bridge — the interesting moments (the user
/// tapping Allow, a revocation in system settings) happen after that point.
#[derive(Clone)]
pub(crate) struct LocationRecord {
    /// What `location_permission` answers.
    pub(crate) permission: Rc<Cell<LocationPermission>>,
    /// Whether the bridge is delivering.
    pub(crate) active: Rc<Cell<bool>>,
    /// How many times `request_location` has been called.
    ///
    /// A counter, not a bool: "asked twice" is the failure mode with a dialog
    /// in it, and a bool records it as a success.
    pub(crate) requests: Rc<Cell<usize>>,
    /// How many times `location_permission` has been read. On Android that is a
    /// JNI call, so the poll cadence is a cost worth asserting on.
    pub(crate) queries: Rc<Cell<usize>>,
    /// What `request_location` returns. `true` on every real bridge but
    /// Android's, which is the only one that can tell.
    pub(crate) reaches_the_os: Rc<Cell<bool>>,
    /// What the gate last told the bridge about how many times it has asked.
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

/// A [`KvStore`] over a [`MemoryKvStore`] the test still holds — `kv` hands
/// out a fresh `Box` per call, so the backing store has to outlive the box for
/// a test to read back what the gate persisted through it.
struct SharedStore {
    inner: Rc<MemoryKvStore>,
}

impl KvStore for SharedStore {
    fn load(&self, key: &str) -> Option<String> {
        self.inner.load(key)
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        self.inner.store(key, value)
    }

    /// Spelled out rather than left to the trait's default, which would forward
    /// to `store` — correct today only by coincidence. A decorator that
    /// forwards one method and forgets the other silently turns a caller's
    /// durable write into a deferred one, and this is a decorator.
    fn store_now(&self, key: &str, value: &str) -> Result<(), String> {
        self.inner.store_now(key, value)
    }
}

/// The double. See the module note.
pub(crate) struct GateDouble {
    store: Rc<MemoryKvStore>,
    store_availability: StoreAvailability,
    /// `true` when the platform has been told where its data lives — the half
    /// of [`StoreAvailability::WhenToldADirectory`] a test controls with the
    /// constructor it picks.
    told_a_directory: bool,
    location: LocationRecord,
}

impl GateDouble {
    fn bare() -> Self {
        Self {
            store: Rc::new(MemoryKvStore::default()),
            store_availability: StoreAvailability::WhenToldADirectory,
            told_a_directory: false,
            location: LocationRecord::default(),
        }
    }

    /// `DesktopPlatform`: knows where config goes from the start.
    pub(crate) fn desktop() -> Self {
        Self {
            told_a_directory: true,
            ..Self::bare()
        }
    }

    /// `AndroidPlatform`: config has no home until `android_main` supplies
    /// one — and it is the one bridge whose `request_location` can honestly
    /// fail to reach the OS, so `with_request_reaching_the_os` matters here.
    ///
    /// No gate test drives it today (the suite's Android-shaped cases run
    /// through the app crate's own double), but the constructor set mirrors
    /// the old `TestBridge` trio so a ported or future test is a one-word
    /// mechanical change.
    #[allow(dead_code)]
    pub(crate) fn android() -> Self {
        Self::bare()
    }

    /// `WebPlatform`: `localStorage` from the first frame with no directory
    /// involved.
    pub(crate) fn web() -> Self {
        Self {
            store_availability: StoreAvailability::Always,
            ..Self::bare()
        }
    }

    /// Persist into `store` rather than into a fresh one, so a test can close
    /// an app and open another over the same blobs — which is the only way to
    /// exercise anything the gate is supposed to remember across restarts.
    pub(crate) fn with_store(mut self, store: Rc<MemoryKvStore>) -> Self {
        self.store = store;
        self
    }

    /// Answer `kv` with `None`, permanently.
    ///
    /// See [`StoreAvailability::Never`] for the two shipping configurations
    /// this stands for.
    pub(crate) fn without_kv(mut self) -> Self {
        self.store_availability = StoreAvailability::Never;
        self
    }

    /// What `location_permission` answers to begin with.
    pub(crate) fn with_permission(self, permission: LocationPermission) -> Self {
        self.location.permission.set(permission);
        self
    }

    /// Whether `request_location` reports that the ask reached the OS.
    ///
    /// Only Android's bridge can honestly answer `false`; the default here is
    /// `true`, as the other four fabricate it.
    pub(crate) fn with_request_reaching_the_os(self, reaches: bool) -> Self {
        self.location.reaches_the_os.set(reaches);
        self
    }

    /// See [`LocationRecord`]. Taken before the gate takes the bridge.
    pub(crate) fn location_record(&self) -> LocationRecord {
        self.location.clone()
    }

    /// The cell behind `location_permission`, for the tests that need the OS to
    /// change its mind mid-session.
    pub(crate) fn permission_cell(&self) -> Rc<Cell<LocationPermission>> {
        Rc::clone(&self.location.permission)
    }

    /// How many times the gate has called `request_location`.
    pub(crate) fn location_requests(&self) -> usize {
        self.location.requests.get()
    }

    /// How many times the gate has read `location_permission`.
    pub(crate) fn permission_queries(&self) -> usize {
        self.location.queries.get()
    }
}

impl LocationBridge for GateDouble {
    fn location_permission(&self) -> LocationPermission {
        self.location.queries.set(self.location.queries.get() + 1);
        self.location.permission.get()
    }

    /// Starts delivery, as every real bridge does — the web's `watchPosition`
    /// is literally the same call as the prompt.
    ///
    /// Deliberately starts even from `Prompt`: on a platform where the ask and
    /// the subscription are one call there is no other order available, and a
    /// double that refused would hide the case the gate has to handle.
    fn request_location(&mut self) -> bool {
        self.location.requests.set(self.location.requests.get() + 1);
        let reached = self.location.reaches_the_os.get();
        if reached && self.location.permission.get() == LocationPermission::Granted {
            self.location.active.set(true);
        }
        reached
    }

    fn stop_location(&mut self) {
        self.location.active.set(false);
    }

    fn location_active(&self) -> bool {
        self.location.active.get()
    }

    fn set_location_attempts(&mut self, attempts: u8) {
        self.location.attempts.set(Some(attempts));
    }

    fn kv(&self) -> Option<Box<dyn KvStore>> {
        let available = match self.store_availability {
            StoreAvailability::WhenToldADirectory => self.told_a_directory,
            StoreAvailability::Always => true,
            StoreAvailability::Never => false,
        };
        available.then(|| {
            Box::new(SharedStore {
                inner: Rc::clone(&self.store),
            }) as Box<_>
        })
    }
}
