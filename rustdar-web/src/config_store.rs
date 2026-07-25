//! [`ConfigStore`] backed by the browser's `localStorage`.
//!
//! This is the reason [`ConfigStore`] addresses blobs by logical key rather than
//! by path: `localStorage` is a flat string-to-string map with no directories
//! and no filenames, so there is nothing here for a `&Path` to mean.

/// Prefix every key is written under.
///
/// An artifact served from a shared origin — a project page, a preview
/// deployment, a `localhost` port reused between projects — sees the same
/// `localStorage` as every other app on that origin. A bare `"ui"` key would
/// collide with any of them. The prefix is not security, it is namespacing.
const KEY_PREFIX: &str = "rustdar.";

/// Map a logical config key onto the `localStorage` key it occupies.
///
/// Split out from the store itself, and public, so it can be tested without a
/// browser: the `Storage` object only exists on wasm32, but the naming rule is
/// ordinary string handling and is the part that would break silently if it
/// changed — an altered prefix does not fail, it just quietly stops finding
/// every layout the previous build saved.
pub fn storage_key(key: &str) -> String {
    format!("{KEY_PREFIX}{key}")
}

/// A [`ConfigStore`] that persists into `window.localStorage`.
///
/// Holds the `Storage` handle rather than re-reaching for it per call. The
/// handle is obtained once in [`LocalStorageConfigStore::new`], which is also
/// where the failure is absorbed: `localStorage` throws rather than returning
/// null when the user has blocked site data, and in a sandboxed iframe the
/// access throws on every attempt. Both are configuration-is-unavailable, which
/// the trait already models as "load returns None, store returns Err" — neither
/// is allowed to stop the app.
#[cfg(target_arch = "wasm32")]
pub struct LocalStorageConfigStore {
    storage: web_sys::Storage,
}

#[cfg(target_arch = "wasm32")]
impl LocalStorageConfigStore {
    /// Obtain the backing store, or `None` where the browser refuses access.
    ///
    /// Returning `Option` rather than papering over the failure with a
    /// no-op store keeps the "is there anywhere to persist?" question answered
    /// in one place — `PlatformBridge::config_store` already returns an
    /// `Option`, and the caller already treats `None` as "use defaults".
    pub fn new() -> Option<Self> {
        // Three things can fail here and all three mean the same thing, so they
        // collapse into one `ok()?` chain: no window (we are not on a page),
        // the getter throwing (site data blocked, or a sandboxed iframe), and
        // the getter succeeding with null (specified, if unusual).
        let storage = web_sys::window()?.local_storage().ok()??;
        Some(Self { storage })
    }
}

#[cfg(target_arch = "wasm32")]
impl rustdar_egui::config_store::ConfigStore for LocalStorageConfigStore {
    fn load(&self, key: &str) -> Option<String> {
        // `get_item` returns Err only if the store has become inaccessible
        // since construction, and Ok(None) for a key that was never written.
        // The trait says to report both as None, so the whole thing folds.
        self.storage.get_item(&storage_key(key)).ok()?
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        self.storage
            .set_item(&storage_key(key), value)
            // The JsValue carries the DOMException; `QuotaExceededError` is the
            // one that actually happens, when the origin's ~5 MB budget is full.
            // Stringified rather than matched because no caller branches on it.
            .map_err(|e| format!("localStorage write failed: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_egui::config_store::UI_CONFIG_KEY;

    /// The prefix is applied. Without it the key would be a bare `"ui"` in an
    /// origin-wide namespace shared with every other app on the host.
    #[test]
    fn keys_are_namespaced_to_rustdar() {
        assert_eq!(storage_key(UI_CONFIG_KEY), "rustdar.ui");
    }

    /// Distinct logical keys stay distinct once prefixed — the mapping has to be
    /// injective or two configs would overwrite each other.
    #[test]
    fn distinct_keys_stay_distinct() {
        assert_ne!(storage_key("ui"), storage_key("other"));
    }

    /// The prefix is a prefix, not a replacement: the logical key has to survive
    /// into the stored name, and it has to be at the *end*. A mapping that
    /// merely contained the key — `"rustdar.layout.v1"` — would still read back
    /// consistently within one build while silently orphaning every layout saved
    /// by the previous one.
    #[test]
    fn the_logical_key_survives_verbatim_at_the_end() {
        let mapped = storage_key("layout");
        assert!(mapped.starts_with(KEY_PREFIX), "{mapped}");
        assert_eq!(&mapped[KEY_PREFIX.len()..], "layout", "{mapped}");
    }
}
