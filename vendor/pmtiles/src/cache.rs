use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock};

use crate::{DirEntry, Directory, PmtResult, TileId};

/// Result of a directory cache lookup.
pub enum DirCacheResult {
    /// The directory was not found in the cache.
    NotCached,
    /// The tile was not found in the directory.
    NotFound,
    /// The tile was found in the directory.
    Found(DirEntry),
}

impl From<Option<&DirEntry>> for DirCacheResult {
    fn from(entry: Option<&DirEntry>) -> Self {
        match entry {
            Some(entry) => DirCacheResult::Found(entry.clone()),
            None => DirCacheResult::NotFound,
        }
    }
}

/// A cache for `PMTiles` directories.
///
/// VENDORED: the cache is keyed by archive offset, and that key is `u64`. It
/// was `usize`, which on a 32-bit target meant two leaf directories exactly
/// 4 GiB apart collided on one key and served each other's entries.
pub trait DirectoryCache {
    /// Get a directory entry from the cache, or insert it using the provided fetcher function.
    fn get_dir_entry_or_insert(
        &self,
        offset: u64,
        tile_id: TileId,
        fetcher: impl Future<Output = PmtResult<Directory>> + Send,
    ) -> impl Future<Output = PmtResult<Option<DirEntry>>> + Send;
}

/// A cache that does not cache anything.
pub struct NoCache;

impl DirectoryCache for NoCache {
    #[inline]
    async fn get_dir_entry_or_insert(
        &self,
        _: u64,
        tile_id: TileId,
        fetcher: impl Future<Output = PmtResult<Directory>> + Send,
    ) -> PmtResult<Option<DirEntry>> {
        let dir = fetcher.await?;
        Ok(dir.find_tile_id(tile_id).cloned())
    }
}

/// A simple HashMap-based implementation of a `PMTiles` directory cache.
#[derive(Default)]
pub struct HashMapCache {
    /// The internal cache storage.
    pub cache: Arc<RwLock<HashMap<u64, Directory>>>,
}

impl HashMapCache {
    fn get_dir_entry(&self, offset: u64, tile_id: TileId) -> DirCacheResult {
        // Panic if the lock is poisoned is not something the user can handle
        #[expect(clippy::unwrap_used)]
        if let Some(dir) = self.cache.read().unwrap().get(&offset) {
            return dir.find_tile_id(tile_id).into();
        }
        DirCacheResult::NotCached
    }

    fn insert_dir(&self, offset: u64, directory: Directory) {
        // Panic if the lock is poisoned is not something the user can handle
        #[expect(clippy::unwrap_used)]
        self.cache.write().unwrap().insert(offset, directory);
    }
}

impl DirectoryCache for HashMapCache {
    async fn get_dir_entry_or_insert(
        &self,
        offset: u64,
        tile_id: TileId,
        fetcher: impl Future<Output = PmtResult<Directory>> + Send,
    ) -> PmtResult<Option<DirEntry>> {
        let dir_entry = self.get_dir_entry(offset, tile_id);
        match dir_entry {
            DirCacheResult::Found(entry) => Ok(Some(entry)),
            DirCacheResult::NotFound => Ok(None),
            DirCacheResult::NotCached => {
                let directory = fetcher.await?;
                let dir_entry = directory.find_tile_id(tile_id).cloned();
                self.insert_dir(offset, directory);
                Ok(dir_entry)
            }
        }
    }
}

/// Provides an implementation of `DirectoryCache` using the `moka` crate.
#[cfg(feature = "moka")]
pub struct MokaCache {
    /// This is the internal moka future cache.
    pub cache: moka::future::Cache<u64, Directory>,
}

#[cfg(feature = "moka")]
impl DirectoryCache for MokaCache {
    async fn get_dir_entry_or_insert(
        &self,
        offset: u64,
        tile_id: TileId,
        fetcher: impl Future<Output = PmtResult<Directory>> + Send,
    ) -> PmtResult<Option<DirEntry>> {
        let directory = self.cache.try_get_with(offset, fetcher).await;
        let directory = directory.map_err(|e| {
            crate::PmtError::DirectoryCacheError(format!("Moka cache fetch error: {e}"))
        })?;
        Ok(directory.find_tile_id(tile_id).cloned())
    }
}

// VENDORED: these tests open fixture archives off the filesystem and drive
// them on a tokio runtime, so they are native-only. The gate is here because
// this workspace's wasm check is `--all-targets`, which builds test targets:
// without it, `tempfile`/`rstest`/`tokio`'s test macros would have to compile
// for wasm32-unknown-unknown to keep that gate green. The matching
// target-gate on the dev-dependencies is in Cargo.toml.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "moka")]
    use crate::MokaCache;
    use crate::{DirEntry, Directory, DirectoryCache, HashMapCache, PmtError};

    #[tokio::test]
    async fn test_hash_map_cache() {
        let cache = HashMapCache::default();
        let offset = 0;
        let tile_id = TileId::new(0).unwrap();
        let mut dir_to_cache = Directory::default();
        dir_to_cache.entries.push(DirEntry::default());

        // Initially, the cache should be empty.
        let get_result = cache.get_dir_entry(offset, tile_id);
        assert!(matches!(get_result, DirCacheResult::NotCached));

        // Insert a directory into the cache.
        cache.insert_dir(offset, dir_to_cache);

        // Now, the cache should return Found since the directory contains an entry.
        let get_result = cache.get_dir_entry(offset, tile_id);
        assert!(matches!(get_result, DirCacheResult::Found(_)));

        // The fetcher won't get called, because the entry is already cached.
        let get_result = cache
            .get_dir_entry_or_insert(offset, tile_id, async { Err(PmtError::InvalidEntry) })
            .await
            .unwrap();
        assert!(get_result.is_some());

        // Now the fetcher will be executed.
        let get_result = cache
            .get_dir_entry_or_insert(offset + 10, tile_id, async { Err(PmtError::InvalidEntry) })
            .await;
        assert!(get_result.is_err());

        // The fetcher will be executed and will contain a tile
        let get_result = cache
            .get_dir_entry_or_insert(offset + 10, tile_id, async {
                let mut dir = Directory::default();
                let dir_entry = DirEntry {
                    offset: (offset + 10) as u64,
                    ..Default::default()
                };
                dir.entries.push(dir_entry);
                Ok(dir)
            })
            .await;
        assert!(get_result.is_ok());
        assert!(get_result.unwrap().is_some());
    }

    #[cfg(feature = "moka")]
    #[tokio::test]
    async fn test_moka_cache() {
        let cache = MokaCache {
            cache: moka::future::Cache::new(100),
        };
        let offset = 0;
        let tile_id = TileId::new(0).unwrap();
        let mut dir_to_cache = Directory::default();
        dir_to_cache.entries.push(DirEntry::default());

        // Returns an Err
        let get_result = cache
            .get_dir_entry_or_insert(offset, tile_id, async { Err(PmtError::InvalidEntry) })
            .await;
        assert!(get_result.is_err());

        // Now inserts the directory into the cache and returns the DirEntry.
        let get_result = cache
            .get_dir_entry_or_insert(offset, tile_id, async {
                let mut dir = Directory::default();
                let dir_entry = DirEntry {
                    offset: (offset + 10) as u64,
                    ..Default::default()
                };
                dir.entries.push(dir_entry);
                Ok(dir)
            })
            .await;
        assert!(get_result.is_ok());
        assert!(get_result.unwrap().is_some());

        // Repeating the request with the fetcher that returns an Err, but this time the fetcher
        // will not be called because the Directory is cached.
        let get_result = cache
            .get_dir_entry_or_insert(offset, tile_id, async { Err(PmtError::InvalidEntry) })
            .await;
        assert!(get_result.is_ok());
        assert!(get_result.unwrap().is_some());
    }
}
