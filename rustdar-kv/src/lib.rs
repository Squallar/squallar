//! A place to keep small named blobs across sessions.
//!
//! String keys, string blobs, never load-bearing. The keys are logical names,
//! not paths: desktop, Android and iOS back this with a file per key, the web
//! build lands in `localStorage`, which is not a file and has no directory to
//! put one in, and tests hold everything in memory. Backends that *do* have a
//! filesystem map the key onto a filename themselves.
//!
//! # Three methods on purpose
//!
//! [`KvStore`] is `load`, `store`, `store_now` — and deliberately nothing
//! more. No enumeration, no deletion, no transactions; a backend that cannot
//! tell "absent" from "unreadable" answers `None` for both. If a consumer
//! needs a fourth verb it is asking for a database, and this crate is
//! deliberately not one — the fence guards against the database connotation
//! the moment a key-value trait grows a query surface.
//!
//! # Today's keys and their owners
//!
//! | key                | owner                                          |
//! |--------------------|------------------------------------------------|
//! | `"ui"`             | rustdar-egui `ui_config` (the UI layout)       |
//! | `"location"`       | rustdar-frontend `location_permission` (memo)  |
//! | `"loop_pool"`      | rustdar-frontend `loop_pool` (device memo)     |
//! | `"site_catalogue"` | rustdar-frontend `site_catalogue`              |
//! | `"site_positions"` | rustdar-frontend `site_positions`              |
//! | `"budget_steps"`   | rustdar-frontend `budget` (device memo)        |
//!
//! The key *strings* are on-disk compatibility: a changed string silently
//! orphans every config or memo an existing install has saved. Each constant
//! lives beside its owner; this table is the map, not the definition.
//!
//! Future consumers noted: rustdar-location (the `LocationGate` memo) arrives
//! at the location split.

/// A place to keep small named blobs of configuration across sessions.
///
/// Blobs are addressed by a short logical key, not a path. `localStorage` has
/// no paths and no directories, so a `&Path` here would force every
/// non-filesystem backend to accept one and then pick it apart again. Backends
/// that *do* have a filesystem map the key onto a filename themselves.
///
/// Neither method is allowed to be load-bearing. Configuration is a
/// convenience: a backend that cannot read returns `None` and the caller falls
/// back to defaults, and a failed write is reported but never propagated into
/// a frame or an exit path. Losing a saved window layout must not lose data or
/// stop the app.
pub trait KvStore {
    /// Read the blob previously stored under `key`, or `None` if there is none.
    ///
    /// A backend that cannot distinguish "absent" from "unreadable" should
    /// return `None` for both — the caller treats them identically.
    fn load(&self, key: &str) -> Option<String>;

    /// Persist `value` under `key`, replacing anything already stored there.
    ///
    /// The `Err` string is for logging, which is why it is not a typed error:
    /// no caller branches on the reason, and the backends have nothing in
    /// common to enumerate.
    ///
    /// A backend may return before the bytes are anywhere durable — see
    /// [`store_now`](Self::store_now) for the callers that cannot accept that.
    /// `Ok` from those backends means "accepted", not "written", so a failure
    /// discovered afterwards is logged by the backend and never reaches here.
    fn store(&self, key: &str, value: &str) -> Result<(), String>;

    /// Persist `value` under `key`, and do not return until it is written.
    ///
    /// For the two moments where the process may not exist a moment later: an
    /// exit, and an Android suspend where the system may kill the app without
    /// another turn of the event loop. A deferred write has no chance to run
    /// after either, so those callers pay the latency deliberately.
    ///
    /// The default is [`store`](Self::store), which is already correct for
    /// every backend that writes inline — the in-memory one, and the
    /// `localStorage` one, which has no thread to defer to. Only a backend
    /// that defers has anything to override here.
    fn store_now(&self, key: &str, value: &str) -> Result<(), String> {
        self.store(key, value)
    }
}

/// A [`KvStore`] held entirely in memory.
///
/// Nothing survives the process. That makes it the right backend for tests,
/// which need the round trip without touching the user's real config, and a
/// usable fallback for a platform that has not told us where to persist yet.
#[derive(Default)]
pub struct MemoryKvStore {
    entries: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl KvStore for MemoryKvStore {
    fn load(&self, key: &str) -> Option<String> {
        self.entries.lock().ok()?.get(key).cloned()
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "kv store mutex poisoned".to_string())?;
        entries.insert(key.to_owned(), value.to_owned());
        Ok(())
    }
}
