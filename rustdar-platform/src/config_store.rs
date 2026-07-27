//! Filesystem-backed [`ConfigStore`], shared by desktop, Android and iOS. The
//! web build never compiles it and supplies a `localStorage` one instead.

use rustdar_egui::config_store::ConfigStore;
use std::path::{Path, PathBuf};

/// Stores each key as `<dir>/<key>.json`.
pub struct FileConfigStore {
    dir: PathBuf,
}

impl FileConfigStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", key))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl ConfigStore for FileConfigStore {
    fn load(&self, key: &str) -> Option<String> {
        let path = self.path_for(key);
        match std::fs::read_to_string(&path) {
            Ok(content) => Some(content),
            Err(e) => {
                // A first run has no config yet; that is not worth a warning.
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("Failed to read config {:?}: {}", path, e);
                }
                None
            }
        }
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        // On write, not up front: an unwritten store leaves no empty directory.
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            return Err(format!("failed to create config dir {:?}: {}", self.dir, e));
        }
        let path = self.path_for(key);
        std::fs::write(&path, value).map_err(|e| format!("failed to write {:?}: {}", path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp directory that removes itself on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "rustdar-config-store-{}-{}-{:?}",
                tag,
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_stored_value_reads_back() {
        let dir = TempDir::new("roundtrip");
        let store = FileConfigStore::new(dir.0.clone());

        assert_eq!(store.load("ui"), None, "nothing stored yet");
        store
            .store("ui", "{\"a\":1}")
            .expect("store should succeed");
        assert_eq!(store.load("ui"), Some("{\"a\":1}".to_string()));
    }

    #[test]
    fn different_keys_do_not_collide() {
        let dir = TempDir::new("keys");
        let store = FileConfigStore::new(dir.0.clone());

        store.store("ui", "first").unwrap();
        store.store("other", "second").unwrap();

        assert_eq!(store.load("ui").as_deref(), Some("first"));
        assert_eq!(store.load("other").as_deref(), Some("second"));
    }

    /// The on-disk name is load-bearing: existing installs have a `ui.json`.
    #[test]
    fn the_ui_key_maps_to_ui_json() {
        let dir = TempDir::new("filename");
        let store = FileConfigStore::new(dir.0.clone());
        store
            .store(rustdar_egui::config_store::UI_CONFIG_KEY, "payload")
            .unwrap();

        let on_disk = dir.0.join("ui.json");
        assert!(on_disk.exists(), "expected {:?} to exist", on_disk);
        assert_eq!(std::fs::read_to_string(&on_disk).unwrap(), "payload");
    }

    /// The first-run path on every platform.
    #[test]
    fn store_creates_a_missing_directory() {
        let dir = TempDir::new("mkdir");
        let nested = dir.0.join("deeper");
        assert!(!nested.exists());

        let store = FileConfigStore::new(nested.clone());
        store.store("ui", "x").expect("store should create the dir");
        assert!(nested.join("ui.json").exists());
    }
}
