//! A purpose-built [`LocationBridge`] double for the gate suite. Not the app
//! crate's `TestBridge` — exporting that would put a whole platform surface on
//! this crate's public face.

use crate::bridge::LocationBridge;
use crate::permission::LocationPermission;
use squallar_kv::{KvStore, MemoryKvStore};
use std::cell::Cell;
use std::rc::Rc;

/// When [`GateDouble::kv`] answers with a store. Three real cases, not two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreAvailability {
    /// Only once a config directory has been set. Desktop and iOS derive one in
    /// their constructors; Android is told during `android_main`.
    WhenToldADirectory,
    /// Always. `localStorage` needs no path and is there from the first frame.
    Always,
    /// Never. A browser with site data blocked, or a desktop process with no
    /// `XDG_CONFIG_HOME`, `HOME` or `LOCALAPPDATA`.
    Never,
}

/// The location state a test shares with the double, behind `Rc<Cell<_>>` so the
/// test can still touch it after the gate has taken the bridge.
#[derive(Clone)]
pub(crate) struct LocationRecord {
    pub(crate) permission: Rc<Cell<LocationPermission>>,
    pub(crate) active: Rc<Cell<bool>>,
    /// A counter, not a bool: "asked twice" is the failure mode with a dialog
    /// in it.
    pub(crate) requests: Rc<Cell<usize>>,
    /// On Android this is a JNI call, so the poll cadence is worth asserting on.
    pub(crate) queries: Rc<Cell<usize>>,
    /// What `request_location` returns. `true` on every real bridge but
    /// Android's, which is the only one that can tell.
    pub(crate) reaches_the_os: Rc<Cell<bool>>,
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

/// A [`KvStore`] over a [`MemoryKvStore`] the test still holds — `kv` hands out
/// a fresh `Box` per call, so the backing store has to outlive the box.
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
    /// to `store`: a decorator that forwards one method and forgets the other
    /// turns a caller's durable write into a deferred one.
    fn store_now(&self, key: &str, value: &str) -> Result<(), String> {
        self.inner.store_now(key, value)
    }
}

pub(crate) struct GateDouble {
    store: Rc<MemoryKvStore>,
    store_availability: StoreAvailability,
    /// The half of [`StoreAvailability::WhenToldADirectory`] a test controls.
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

    pub(crate) fn desktop() -> Self {
        Self {
            told_a_directory: true,
            ..Self::bare()
        }
    }

    /// `AndroidPlatform`: no config home until `android_main` supplies one, and
    /// the one bridge whose `request_location` can honestly fail to reach the OS.
    #[allow(dead_code)]
    pub(crate) fn android() -> Self {
        Self::bare()
    }

    pub(crate) fn web() -> Self {
        Self {
            store_availability: StoreAvailability::Always,
            ..Self::bare()
        }
    }

    /// Persist into `store` rather than a fresh one, so a test can close an app
    /// and open another over the same blobs.
    pub(crate) fn with_store(mut self, store: Rc<MemoryKvStore>) -> Self {
        self.store = store;
        self
    }

    /// Answer `kv` with `None`, permanently. See [`StoreAvailability::Never`].
    pub(crate) fn without_kv(mut self) -> Self {
        self.store_availability = StoreAvailability::Never;
        self
    }

    pub(crate) fn with_permission(self, permission: LocationPermission) -> Self {
        self.location.permission.set(permission);
        self
    }

    /// Only Android's bridge can honestly answer `false`; the default is `true`.
    pub(crate) fn with_request_reaching_the_os(self, reaches: bool) -> Self {
        self.location.reaches_the_os.set(reaches);
        self
    }

    /// See [`LocationRecord`]. Taken before the gate takes the bridge.
    pub(crate) fn location_record(&self) -> LocationRecord {
        self.location.clone()
    }

    /// The cell behind `location_permission`, for tests that need the OS to
    /// change its mind mid-session.
    pub(crate) fn permission_cell(&self) -> Rc<Cell<LocationPermission>> {
        Rc::clone(&self.location.permission)
    }

    pub(crate) fn location_requests(&self) -> usize {
        self.location.requests.get()
    }

    pub(crate) fn permission_queries(&self) -> usize {
        self.location.queries.get()
    }
}

impl LocationBridge for GateDouble {
    fn location_permission(&self) -> LocationPermission {
        self.location.queries.set(self.location.queries.get() + 1);
        self.location.permission.get()
    }

    /// Starts delivery, as every real bridge does — the web's `watchPosition` is
    /// literally the same call as the prompt. Deliberately starts even from
    /// `Prompt`: a double that refused would hide the case the gate must handle.
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
