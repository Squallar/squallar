//! [`squallar_kv::KvStore`] backed by the browser's `localStorage`.
//!
//! This is why [`squallar_kv::KvStore`] addresses blobs by logical key rather
//! than by path: `localStorage` is a flat string-to-string map.

/// Namespacing, not security: a shared origin sees one `localStorage` for every
/// app on it, and a bare `"ui"` would collide.
const KEY_PREFIX: &str = "squallar.";

/// Map a logical config key onto its `localStorage` key. Public so it is
/// testable without a browser. An altered prefix does not fail; it quietly
/// stops finding every layout the previous build saved.
pub fn storage_key(key: &str) -> String {
    format!("{KEY_PREFIX}{key}")
}

/// A [`squallar_kv::KvStore`] that persists into `window.localStorage`. The
/// handle is obtained once in `new`, which is where failure is absorbed:
/// `localStorage` *throws* when site data is blocked.
#[cfg(target_arch = "wasm32")]
pub struct LocalStorageKvStore {
    storage: web_sys::Storage,
}

#[cfg(target_arch = "wasm32")]
impl LocalStorageKvStore {
    pub fn new() -> Option<Self> {
        // Three failures, all meaning "nowhere to persist": no window, the
        // getter throwing, and the getter returning null.
        let storage = web_sys::window()?.local_storage().ok()??;
        Some(Self { storage })
    }
}

#[cfg(target_arch = "wasm32")]
impl squallar_kv::KvStore for LocalStorageKvStore {
    fn load(&self, key: &str) -> Option<String> {
        // Err and Ok(None) both report as None per the trait.
        self.storage.get_item(&storage_key(key)).ok()?
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        self.storage
            .set_item(&storage_key(key), value)
            // `QuotaExceededError` is the one that happens; stringified because
            // no caller branches on it.
            .map_err(|e| format!("localStorage write failed: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squallar_egui::UI_CONFIG_KEY;

    #[test]
    fn keys_are_namespaced_to_squallar() {
        assert_eq!(storage_key(UI_CONFIG_KEY), "squallar.ui");
    }

    #[test]
    fn distinct_keys_stay_distinct() {
        assert_ne!(storage_key("ui"), storage_key("other"));
    }

    /// The logical key must survive verbatim and at the *end*. A mapping that
    /// merely contained it would orphan everything the previous build saved.
    #[test]
    fn the_logical_key_survives_verbatim_at_the_end() {
        let mapped = storage_key("layout");
        assert!(mapped.starts_with(KEY_PREFIX), "{mapped}");
        assert_eq!(&mapped[KEY_PREFIX.len()..], "layout", "{mapped}");
    }
}
