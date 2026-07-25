//! Where persisted UI configuration goes.
//!
//! The UI layer knows *what* to persist and in what format; it deliberately
//! knows nothing about *where*. Desktop, Android and iOS all land on a file;
//! the web build has to land in `localStorage`, which is not a file and has no
//! directory to put one in.

/// Key the UI layout is persisted under.
///
/// The filesystem backend turns this into `ui.json`, which is the name the
/// config has always had on disk — the key was chosen to keep it that way so
/// existing configs keep loading.
pub const UI_CONFIG_KEY: &str = "ui";

/// A place to keep small named blobs of UI configuration across sessions.
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
pub trait ConfigStore {
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
    fn store(&self, key: &str, value: &str) -> Result<(), String>;
}

/// A [`ConfigStore`] held entirely in memory.
///
/// Nothing survives the process. That makes it the right backend for tests,
/// which need the round trip without touching the user's real config, and a
/// usable fallback for a platform that has not told us where to persist yet.
#[derive(Default)]
pub struct MemoryConfigStore {
    entries: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl ConfigStore for MemoryConfigStore {
    fn load(&self, key: &str) -> Option<String> {
        self.entries.lock().ok()?.get(key).cloned()
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "config store mutex poisoned".to_string())?;
        entries.insert(key.to_owned(), value.to_owned());
        Ok(())
    }
}
