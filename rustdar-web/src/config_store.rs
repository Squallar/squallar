//! [`ConfigStore`] backed by the browser's `localStorage`.
//!
//! This is the reason [`ConfigStore`] addresses blobs by logical key rather than
//! by path: `localStorage` is a flat string-to-string map with no directories
//! and no filenames, so there is nothing here for a `&Path` to mean.

/// Namespacing, not security: a shared origin (project page, preview deploy,
/// reused `localhost` port) sees one `localStorage` for every app on it, and a
/// bare `"ui"` would collide.
const KEY_PREFIX: &str = "rustdar.";

/// Map a logical config key onto its `localStorage` key.
///
/// Split out and public so it is testable without a browser. An altered prefix
/// does not fail; it quietly stops finding every layout the previous build saved.
pub fn storage_key(key: &str) -> String {
    format!("{KEY_PREFIX}{key}")
}

/// A [`ConfigStore`] that persists into `window.localStorage`.
///
/// The handle is obtained once in [`LocalStorageConfigStore::new`], which is
/// also where failure is absorbed: `localStorage` *throws* rather than returning
/// null when site data is blocked or the page is a sandboxed iframe.
#[cfg(target_arch = "wasm32")]
pub struct LocalStorageConfigStore {
    storage: web_sys::Storage,
}

#[cfg(target_arch = "wasm32")]
impl LocalStorageConfigStore {
    /// Obtain the backing store, or `None` where the browser refuses access.
    pub fn new() -> Option<Self> {
        // Three distinct failures, all meaning "nowhere to persist": no window,
        // the getter throwing (site data blocked, sandboxed iframe), and the
        // getter returning null.
        let storage = web_sys::window()?.local_storage().ok()??;
        Some(Self { storage })
    }
}

#[cfg(target_arch = "wasm32")]
impl rustdar_egui::config_store::ConfigStore for LocalStorageConfigStore {
    fn load(&self, key: &str) -> Option<String> {
        // Err (store became inaccessible) and Ok(None) (never written) both
        // report as None per the trait, so the chain folds.
        self.storage.get_item(&storage_key(key)).ok()?
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        self.storage
            .set_item(&storage_key(key), value)
            // `QuotaExceededError` (origin's ~5 MB budget full) is the one that
            // happens. Stringified because no caller branches on it.
            .map_err(|e| format!("localStorage write failed: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_egui::config_store::UI_CONFIG_KEY;

    #[test]
    fn keys_are_namespaced_to_rustdar() {
        assert_eq!(storage_key(UI_CONFIG_KEY), "rustdar.ui");
    }

    /// The mapping has to be injective or two configs overwrite each other.
    #[test]
    fn distinct_keys_stay_distinct() {
        assert_ne!(storage_key("ui"), storage_key("other"));
    }

    /// The logical key must survive verbatim and at the *end*. A mapping that
    /// merely contained it (`"rustdar.layout.v1"`) reads back consistently
    /// within one build while orphaning everything the previous one saved.
    #[test]
    fn the_logical_key_survives_verbatim_at_the_end() {
        let mapped = storage_key("layout");
        assert!(mapped.starts_with(KEY_PREFIX), "{mapped}");
        assert_eq!(&mapped[KEY_PREFIX.len()..], "layout", "{mapped}");
    }
}
