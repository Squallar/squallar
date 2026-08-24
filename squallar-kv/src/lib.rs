//! A place to keep small named blobs across sessions.
//!
//! String keys, string blobs, never load-bearing. The keys are logical names,
//! not paths: a filesystem backend maps the key onto a filename itself, the web
//! build lands in `localStorage`, and tests hold everything in memory.
//!
//! [`KvStore`] is `load`, `store`, `store_now` and deliberately nothing more:
//! no enumeration, no deletion, no transactions. A backend that cannot tell
//! "absent" from "unreadable" answers `None` for both.
//!
//! The key *strings* are on-disk compatibility: a changed string silently
//! orphans every config or memo an existing install has saved. Each constant
//! lives beside its owner.

/// A place to keep small named blobs of configuration across sessions.
///
/// Neither method is load-bearing: a backend that cannot read returns `None`
/// and the caller falls back to defaults.
pub trait KvStore {
    /// Read the blob previously stored under `key`, or `None` if there is none.
    ///
    /// A backend that cannot distinguish "absent" from "unreadable" returns `None`.
    fn load(&self, key: &str) -> Option<String>;

    /// Persist `value` under `key`, replacing anything already stored there.
    ///
    /// The `Err` string is for logging, not branching. A backend may return before
    /// the bytes are anywhere durable — see [`store_now`](Self::store_now).
    fn store(&self, key: &str, value: &str) -> Result<(), String>;

    /// Persist `value` under `key`, and do not return until it is written.
    ///
    /// For the two moments where the process may not exist a moment later: an exit,
    /// and an Android suspend. Only a deferring backend overrides.
    fn store_now(&self, key: &str, value: &str) -> Result<(), String> {
        self.store(key, value)
    }
}

/// A [`KvStore`] held entirely in memory.
///
/// Nothing survives the process.
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
