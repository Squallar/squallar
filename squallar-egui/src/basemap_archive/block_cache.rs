//! A persistent block cache at the [`RangeSource`] seam.
//!
//! Every archive byte — basemap, terrain, pmtiles directories — crosses one
//! seam, [`RangeSource::read_range`], and this module is a decorator on that
//! seam: [`BlockCachedSource`] quantizes reads to [`BLOCK_BYTES`]-sized
//! blocks keyed `(generation, block index)`, serves a hit from disk, and on a
//! miss reads the aligned span through the inner source, writes it, and
//! serves it. Nothing above the seam knows the cache exists; nothing below it
//! changes.
//!
//! # Why no TTLs, no revalidation, no versioning
//!
//! The archives are **immutable and generation-keyed**: a publish is a new
//! path (`basemap/omt-20260828.pmtiles`, `terrain/7c94bc6966ab-20260829/…`),
//! never a rewrite of an old one. So the bytes at `(generation, offset)` can
//! never change, a cached block is correct forever, and invalidation is
//! exactly one operation: delete generation directories that are no longer
//! current — run once, at open.
//!
//! # Where the IO happens
//!
//! `read_range` already runs on the tile IO runtime's own thread — the thread
//! [`super::FileRangeSource`] documents as existing to block — so the file
//! reads and writes here block on purpose, exactly as that source's do. The
//! frame thread never sees any of this: construction
//! ([`BlockCachedSource::new`]) only stores configuration, and the one
//! directory walk that recovers the running byte total is deferred to the
//! first `read_range`.
//!
//! # wasm32
//!
//! The wasm arm of [`BlockCachedSource`] is a pass-through newtype with the
//! same constructor — a per-target selection of a *type*, following
//! [`super::transport`]'s pattern, never a `cfg` fork inside a function body.
//! There is no filesystem there; the browser's HTTP cache is that target's
//! persistence story. [`maybe_seed`] splits the same way.

use std::path::PathBuf;

/// Bytes in one cache block.
///
/// **64 KiB, and the choice is about read amplification versus file count.**
/// The reads crossing this seam are pmtiles header reads (16 KiB), directory
/// reads (a few KiB to a few tens of KiB) and tile bodies (most well under
/// 100 KiB; the committed Monaco fixture's densest z14 tile is ~256 KiB). At
/// 64 KiB a small directory read amplifies to at most one block fetch —
/// bounded 64 KiB of extra transfer — while a tile body spans a handful of
/// blocks whose aligned span is fetched as **one** inner read, so the
/// amplification never compounds. Smaller blocks would multiply files (the
/// eviction index and the open walk scale with file count: a full 1 GiB
/// desktop cache is 16,384 blocks at this size); larger blocks would make
/// every cold directory read pay a multiple of what it asked for.
pub const BLOCK_BYTES: u64 = 64 * 1024;

/// The wasm32 arm of [`BLOCK_CACHE_BYTES`]: zero, because the wrapper is a
/// pass-through there — no filesystem, no cache, no budget to hold. Named
/// outside the cascade because this workspace runs `cargo test` on one arm,
/// so the other two are only reachable from a test if they have names.
pub const WASM_BLOCK_CACHE_BYTES: u64 = 0;
/// The mobile arm — a quarter of the desktop figure, against cache storage
/// the OS reports to the user as clearable and will clear itself under
/// pressure. See [`WASM_BLOCK_CACHE_BYTES`] for why all three arms are named.
pub const MOBILE_BLOCK_CACHE_BYTES: u64 = 256 * 1024 * 1024;
/// The desktop arm: 1 GiB — room for a session's whole basemap working set
/// plus the terrain tiles under it. See [`WASM_BLOCK_CACHE_BYTES`].
pub const DESKTOP_BLOCK_CACHE_BYTES: u64 = 1024 * 1024 * 1024;

/// The total-bytes cap for this target, hardcoded per target — a `cfg`
/// cascade selecting a value, which is the one thing a `cfg` may do here.
#[cfg(target_arch = "wasm32")]
pub const BLOCK_CACHE_BYTES: u64 = WASM_BLOCK_CACHE_BYTES;
/// See the wasm32 arm above.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "android", target_os = "ios")
))]
pub const BLOCK_CACHE_BYTES: u64 = MOBILE_BLOCK_CACHE_BYTES;
/// See the wasm32 arm above.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub const BLOCK_CACHE_BYTES: u64 = DESKTOP_BLOCK_CACHE_BYTES;

/// The deepest zoom the background seed warms — z0 through here is 1365
/// tiles, the whole world at a glance.
///
/// The measured total for the published `omt-20260828` generation is in the
/// commit that landed the seed; it is a small fraction of every non-wasm
/// target's [`BLOCK_CACHE_BYTES`], and [`seed_shallow`] re-checks against the
/// cap at run time rather than trusting this prose.
pub const SEED_MAX_ZOOM: u8 = 5;

/// The marker file that records "this generation has been seeded", so a
/// second launch does not walk 1365 tiles to discover 1365 hits. Named for
/// the zoom it covers: deepening [`SEED_MAX_ZOOM`] renames the marker, and
/// the deeper seed then runs once over a cache that already holds the
/// shallower zooms — all hits up to the new depth.
pub fn seed_marker() -> String {
    format!(".seeded-z{SEED_MAX_ZOOM}")
}

/// The one derivation of a cache generation from an archive URL.
///
/// **The generation is the URL's path, injectively encoded** — for the
/// published archives that path carries the distinguishing segment
/// (`omt-20260828`, `7c94bc6966ab-20260829`), so two generations can never
/// map to one directory, which is the property that makes a disk hit safe to
/// serve without revalidation. A wrong key here silently serves stale bytes
/// across generations; `generation_keys_are_derived_from_the_path` in the
/// tests is the pin.
///
/// Deliberately the *path only*, not the host: a local mirror of a published
/// generation (`SQUALLAR_BASEMAP_ARCHIVE` pointing another host at the same
/// archive path — over TLS, since the archive client is `https_only`) serves
/// byte-identical content, and sharing its cache is correct. Two *different*
/// archives published under one path cannot exist, because the path is the
/// generation.
///
/// The encoding keeps `[A-Za-z0-9.-]` and spells every other byte `_XX`
/// (uppercase hex, `_` itself becomes `_5F`), so it is injective — no two
/// paths encode to one name — and every output is a portable single path
/// component.
pub fn generation_for_url(url: &str) -> String {
    // The path if it parses as a URL; the whole string otherwise, so a
    // non-URL still gets a stable, injective key rather than a panic — the
    // sources themselves reject unparseable URLs at construction anyway.
    let path = reqwest::Url::parse(url)
        .map(|parsed| parsed.path().to_owned())
        .unwrap_or_else(|_| url.to_owned());
    let trimmed = path.strip_prefix('/').unwrap_or(&path);

    let mut name = String::with_capacity(trimmed.len());
    for byte in trimmed.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' => name.push(byte as char),
            other => {
                use std::fmt::Write as _;
                let _ = write!(name, "_{other:02X}");
            }
        }
    }
    name
}

/// Everything the cache needs to know, decided by the caller that knows the
/// URLs — `tiles.rs`, which is where every archive URL lives and where
/// `live_archive_urls` enumerates them.
///
/// `live_generations` is every generation the *process* currently reads, not
/// just this source's: the archives have different generations all alive at
/// once and their sources are built lazily and in no fixed order, so a GC that
/// only knew the opening source's generation would delete the others' caches
/// at every launch.
#[derive(Clone, Debug)]
pub struct BlockCacheConfig {
    /// The cache root. Generation directories live directly under it.
    pub root: PathBuf,
    /// This source's generation, from [`generation_for_url`].
    pub generation: String,
    /// Every generation currently live in this process, from the same
    /// derivation. The open-time GC keeps exactly these.
    pub live_generations: Vec<String>,
    /// Total-bytes cap over the whole root: [`BLOCK_CACHE_BYTES`] outside the
    /// tests, a small figure inside them so eviction is exercisable without a
    /// gigabyte fixture.
    pub cap_bytes: u64,
}

/// What one [`seed_shallow`] run did — returned rather than only logged, so
/// the tests can assert on the mechanism instead of on prose.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The marker was present; not one tile was read.
    AlreadySeeded,
    /// Every tile through [`SEED_MAX_ZOOM`] was read; the marker is now set.
    Seeded {
        /// Tiles the archive held (absent coordinates are not counted).
        tiles: usize,
    },
    /// The cache filled to its cap mid-seed; the seed stopped and left no
    /// marker, so the next launch resumes over what are now hits.
    CapReached,
    /// The cache would not open, or a read failed; no marker was left.
    Aborted,
}

// ---------------------------------------------------------------------------
// Native: the real cache
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::collections::{BTreeSet, HashMap};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    use super::super::{ArchiveRangeSource, BasemapArchive, RangeError, RangeSource};
    use super::{BLOCK_BYTES, BlockCacheConfig, SEED_MAX_ZOOM, SeedOutcome};

    /// [`super::BlockCachedSource`]'s native body: the decorator that caches.
    pub struct BlockCachedSource<S> {
        inner: Arc<S>,
        /// `None` degrades to pass-through: no cache dir was configured.
        /// Every read then costs exactly what it cost before this module
        /// existed. (A root that refuses to open degrades the same way, per
        /// read, inside [`CacheShared::ensure_open`].)
        cache: Option<CacheBinding>,
    }

    /// One source's handle on the shared cache: which generation it reads,
    /// over which root.
    #[derive(Clone)]
    pub(super) struct CacheBinding {
        pub(super) generation: String,
        pub(super) shared: Arc<CacheShared>,
    }

    impl CacheBinding {
        pub(super) fn obtain(config: BlockCacheConfig) -> Self {
            Self {
                generation: config.generation.clone(),
                shared: CacheShared::obtain(config),
            }
        }
    }

    impl<S> BlockCachedSource<S> {
        /// Wrap `inner`. With `config: None` this is a pass-through.
        ///
        /// Called from a frame, so it only stores configuration — the
        /// directory walk, the stale-generation GC and the byte-total
        /// recovery all wait for the first `read_range`, which runs on the
        /// IO task.
        pub fn new(inner: S, config: Option<BlockCacheConfig>) -> Self {
            Self {
                inner: Arc::new(inner),
                cache: config.map(CacheBinding::obtain),
            }
        }
    }

    /// The per-root shared state: one running byte total, one eviction
    /// index, one open walk — shared by every source over the same root
    /// (every archive `tiles.rs` declares, and every source rebuilt after a
    /// `MapTileState::clear`; theme flips and layer toggles no longer rebuild
    /// anything), because two sources each keeping a private total would both
    /// be wrong about the sum.
    pub(super) struct CacheShared {
        root: PathBuf,
        cap_bytes: u64,
        live_generations: Vec<String>,
        /// The open walk's verdict, run once per process per root, on the IO
        /// side: `true` and the index is populated, `false` and every read is
        /// a pass-through (the root would not open — say, an unwritable
        /// path). `OnceLock` so a second source arriving mid-walk blocks
        /// briefly instead of walking twice.
        opened: OnceLock<bool>,
        index: Mutex<CacheIndex>,
    }

    /// The eviction bookkeeping: a running total and an oldest-first order,
    /// kept in memory so no write ever scans the tree. Recovered by the one
    /// walk at open; updated by every write.
    #[derive(Default)]
    struct CacheIndex {
        total_bytes: u64,
        /// Block file → (mtime nanos, size). The map is the authority on what
        /// is counted; `by_age` is the same set ordered for eviction.
        by_path: HashMap<PathBuf, (u64, u64)>,
        by_age: BTreeSet<(u64, PathBuf)>,
    }

    impl CacheIndex {
        fn insert(&mut self, path: PathBuf, mtime: u64, size: u64) {
            if let Some((old_mtime, old_size)) = self.by_path.insert(path.clone(), (mtime, size)) {
                // A rewrite of a block that is already counted — the
                // concurrent-writer case. The content is identical by
                // immutability, so this only refreshes the bookkeeping.
                self.by_age.remove(&(old_mtime, path.clone()));
                self.total_bytes = self.total_bytes.saturating_sub(old_size);
            }
            self.by_age.insert((mtime, path));
            self.total_bytes += size;
        }

        fn remove(&mut self, path: &Path) {
            if let Some((mtime, size)) = self.by_path.remove(path) {
                self.by_age.remove(&(mtime, path.to_path_buf()));
                self.total_bytes = self.total_bytes.saturating_sub(size);
            }
        }

        fn oldest(&self) -> Option<PathBuf> {
            self.by_age.first().map(|(_, path)| path.clone())
        }
    }

    /// One registry of roots for the process, so every source over a root
    /// shares one [`CacheShared`]. First configuration wins; every caller in
    /// the tree builds its config from the same constants and the same URL
    /// set, so there is nothing for a second one to disagree about.
    fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<CacheShared>>> {
        static ROOTS: OnceLock<Mutex<HashMap<PathBuf, Arc<CacheShared>>>> = OnceLock::new();
        ROOTS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    impl CacheShared {
        fn obtain(config: BlockCacheConfig) -> Arc<Self> {
            let mut roots = registry()
                .lock()
                .expect("the cache registry is not poisoned");
            Arc::clone(roots.entry(config.root.clone()).or_insert_with(|| {
                Arc::new(Self {
                    root: config.root,
                    cap_bytes: config.cap_bytes,
                    live_generations: config.live_generations,
                    opened: OnceLock::new(),
                    index: Mutex::new(CacheIndex::default()),
                })
            }))
        }

        /// Whether the cache is usable, opening it on the first ask.
        ///
        /// Runs on the IO task (its callers are `read_range` and the seed),
        /// blocks on purpose there, and never runs twice: the GC of stale
        /// generations and the walk that recovers the byte total happen
        /// here, once per process per root.
        pub(super) fn ensure_open(&self) -> bool {
            *self.opened.get_or_init(|| {
                if let Err(error) = std::fs::create_dir_all(&self.root) {
                    log::warn!(
                        "the archive block cache at {} will not open, so this session \
                         is uncached: {error}",
                        self.root.display()
                    );
                    return false;
                }
                self.gc_stale_generations();
                self.recover_totals();
                true
            })
        }

        /// Delete every generation directory that is not live.
        ///
        /// The whole of invalidation: content within a generation is
        /// immutable, so the only thing that can ever be stale is a
        /// generation the process no longer reads. `live_generations` is the
        /// full process-wide set — every archive `tiles.rs` declares, not the
        /// one being opened — so the order the sources open in cannot cost any
        /// of them its cache.
        fn gc_stale_generations(&self) {
            let Ok(entries) = std::fs::read_dir(&self.root) else {
                return;
            };
            for entry in entries.flatten() {
                let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if is_dir && !self.live_generations.iter().any(|live| live == name) {
                    log::info!("archive block cache: dropping stale generation {name}");
                    if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                        log::warn!("could not drop stale generation {name}: {error}");
                    }
                }
            }
        }

        /// One walk at open recovers the running total and the age order, so
        /// nothing per-write ever scans the tree. Stray temp files from a
        /// process that died mid-write are deleted here; dot-files (the seed
        /// markers) are neither counted nor evictable.
        fn recover_totals(&self) {
            let mut index = self.index.lock().expect("the cache index is not poisoned");
            let Ok(generations) = std::fs::read_dir(&self.root) else {
                return;
            };
            for generation in generations.flatten() {
                if !generation.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let Ok(blocks) = std::fs::read_dir(generation.path()) else {
                    continue;
                };
                for block in blocks.flatten() {
                    let name = block.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if name.starts_with('.') {
                        continue;
                    }
                    if name.contains(".tmp") {
                        // A writer died between write and rename. The rename
                        // never happened, so nothing ever served these bytes.
                        let _ = std::fs::remove_file(block.path());
                        continue;
                    }
                    let Ok(meta) = block.metadata() else { continue };
                    index.insert(block.path(), mtime_nanos(&meta), meta.len());
                }
            }
        }

        fn generation_dir(&self, generation: &str) -> PathBuf {
            self.root.join(generation)
        }

        /// The file holding `block_index` of `generation`.
        fn block_path(&self, generation: &str, block_index: u64) -> PathBuf {
            self.generation_dir(generation)
                .join(format!("{block_index:010}"))
        }

        /// The bytes currently stored, in total — what the seed's cap check
        /// reads.
        pub(super) fn total_bytes(&self) -> u64 {
            self.index
                .lock()
                .expect("the cache index is not poisoned")
                .total_bytes
        }

        pub(super) fn cap_bytes(&self) -> u64 {
            self.cap_bytes
        }

        /// Store one block: write a temp file, rename into place, account,
        /// evict.
        ///
        /// **Concurrent writers of one block are idempotent by
        /// construction**: the content at `(generation, offset)` is
        /// immutable, so two writers hold byte-identical bodies, each renames
        /// its own uniquely-named temp file over the same destination, and
        /// whichever rename lands last changes nothing but the mtime. Pinned
        /// by `concurrent_writers_of_one_block_are_idempotent` in the tests.
        ///
        /// A cache write that fails is logged and swallowed: the caller
        /// already holds the bytes, and a full disk must degrade the cache,
        /// never the read.
        pub(super) fn store_block(&self, generation: &str, block_index: u64, bytes: &[u8]) {
            static WRITER_SEQ: AtomicU64 = AtomicU64::new(0);

            let dir = self.generation_dir(generation);
            if let Err(error) = std::fs::create_dir_all(&dir) {
                log::warn!("archive block cache: {}: {error}", dir.display());
                return;
            }

            let path = self.block_path(generation, block_index);
            let unique = WRITER_SEQ.fetch_add(1, Ordering::Relaxed);
            let temp = dir.join(format!(
                "{block_index:010}.tmp.{}.{unique}",
                std::process::id()
            ));

            if let Err(error) = std::fs::write(&temp, bytes) {
                log::warn!("archive block cache: {}: {error}", temp.display());
                let _ = std::fs::remove_file(&temp);
                return;
            }
            if let Err(error) = std::fs::rename(&temp, &path) {
                log::warn!("archive block cache: {}: {error}", path.display());
                let _ = std::fs::remove_file(&temp);
                return;
            }

            let mut index = self.index.lock().expect("the cache index is not poisoned");
            index.insert(path, now_nanos(), bytes.len() as u64);

            // Evict oldest-mtime blocks until back under the cap. The loop
            // walks the in-memory order, never the tree.
            while index.total_bytes > self.cap_bytes {
                let Some(oldest) = index.oldest() else { break };
                let _ = std::fs::remove_file(&oldest);
                index.remove(&oldest);
            }
        }

        /// One block's stored bytes, or `None` on a miss. A hit is a plain
        /// file read; a file shorter than [`BLOCK_BYTES`] is the archive
        /// ending inside the block, which is exactly what the inner source
        /// answered when it was stored.
        pub(super) fn load_block(&self, generation: &str, block_index: u64) -> Option<Vec<u8>> {
            std::fs::read(self.block_path(generation, block_index)).ok()
        }

        /// Whether `marker` exists under `generation`.
        pub(super) fn has_marker(&self, generation: &str, marker: &str) -> bool {
            self.generation_dir(generation).join(marker).exists()
        }

        /// Write `marker` under `generation` (atomically, like a block).
        pub(super) fn set_marker(&self, generation: &str, marker: &str) {
            let dir = self.generation_dir(generation);
            if std::fs::create_dir_all(&dir).is_err() {
                return;
            }
            let temp = dir.join(format!("{marker}.tmp.{}", std::process::id()));
            if std::fs::write(&temp, b"").is_ok()
                && std::fs::rename(&temp, dir.join(marker)).is_err()
            {
                let _ = std::fs::remove_file(&temp);
            }
        }
    }

    /// A file's mtime as nanos since the epoch, `0` for one before it —
    /// eviction order only, nothing here needs wall-clock truth.
    fn mtime_nanos(meta: &std::fs::Metadata) -> u64 {
        meta.modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos() as u64)
    }

    fn now_nanos() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64)
    }

    impl<S: ArchiveRangeSource> RangeSource for BlockCachedSource<S> {
        fn read_range(
            &self,
            offset: u64,
            length: usize,
        ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
            let inner = Arc::clone(&self.inner);
            let cache = self.cache.clone();

            async move {
                let Some(cache) = cache else {
                    return inner.read_range(offset, length).await;
                };
                if !cache.shared.ensure_open() {
                    return inner.read_range(offset, length).await;
                }
                if length == 0 {
                    return Ok(Vec::new());
                }

                let generation = cache.generation.as_str();
                let shared = &cache.shared;
                let first = offset / BLOCK_BYTES;
                let last = (offset + length as u64 - 1) / BLOCK_BYTES;
                let count =
                    usize::try_from(last - first + 1).expect("a read's block span fits in memory");

                // Hits first, so the misses are known before anything is
                // fetched and a contiguous run of them costs one inner read.
                let mut blocks: Vec<Option<Vec<u8>>> = (first..=last)
                    .map(|index| shared.load_block(generation, index))
                    .collect();

                let mut cursor = 0usize;
                while cursor < count {
                    if blocks[cursor].is_some() {
                        cursor += 1;
                        continue;
                    }
                    // The contiguous miss run starting here.
                    let run_end = (cursor..count)
                        .take_while(|&at| blocks[at].is_none())
                        .last()
                        .expect("the run holds at least `cursor`");
                    let span_offset = (first + cursor as u64) * BLOCK_BYTES;
                    let span_length = (run_end - cursor + 1) * BLOCK_BYTES as usize;

                    // The aligned span through the inner source. Short is the
                    // archive ending — `read_range`'s own contract — so the
                    // tail blocks of the run simply come back partial or
                    // empty, and an empty block is not stored (there is
                    // nothing in it to serve).
                    let fetched = inner.read_range(span_offset, span_length).await?;
                    for (at, slot) in blocks.iter_mut().enumerate().take(run_end + 1).skip(cursor) {
                        let start = ((at - cursor) * BLOCK_BYTES as usize).min(fetched.len());
                        let end = ((at - cursor + 1) * BLOCK_BYTES as usize).min(fetched.len());
                        let body = fetched[start..end].to_vec();
                        if !body.is_empty() {
                            shared.store_block(generation, first + at as u64, &body);
                        }
                        *slot = Some(body);
                    }
                    cursor = run_end + 1;
                }

                // Concatenate and slice the asked-for window out. A block
                // shorter than BLOCK_BYTES is the archive ending inside it,
                // so assembly stops there — everything past it is beyond the
                // end, and `read_range` reads *up to* `length`.
                let mut assembled = Vec::with_capacity(length + BLOCK_BYTES as usize);
                for block in blocks {
                    let block = block.expect("every block was filled above");
                    let short = (block.len() as u64) < BLOCK_BYTES;
                    assembled.extend_from_slice(&block);
                    if short {
                        break;
                    }
                }

                let skip = (offset - first * BLOCK_BYTES) as usize;
                if skip >= assembled.len() {
                    return Ok(Vec::new());
                }
                let end = (skip + length).min(assembled.len());
                Ok(assembled[skip..end].to_vec())
            }
        }
    }

    /// See [`super::super::assert_source_bounds`]: the wrapper must satisfy
    /// the archive bound wherever its inner source does.
    const _: fn() =
        super::super::assert_source_bounds::<BlockCachedSource<super::super::HttpRangeSource>>;

    // -----------------------------------------------------------------------
    // The seed
    // -----------------------------------------------------------------------

    /// Warm the cache with the basemap's z0–[`SEED_MAX_ZOOM`] tiles, in the
    /// background, once per generation.
    ///
    /// Spawned onto the IO runtime this is called from — the archive task's
    /// own current-thread runtime — so it interleaves with tile requests
    /// rather than delaying them, and never goes near the frame thread. The
    /// mechanism is the cache itself: every read goes through `archive`,
    /// whose source is the caching wrapper, so a seeded range and a browsed
    /// range are the same bytes in the same blocks.
    ///
    /// Answers whether a seed task was spawned: `false` when `config` is
    /// `None` (no cache dir means nothing to warm — the tiles would be
    /// dropped on the floor and refetched anyway).
    pub fn maybe_seed<S: ArchiveRangeSource>(
        archive: &Arc<BasemapArchive<S>>,
        config: Option<BlockCacheConfig>,
    ) -> bool {
        let Some(config) = config else {
            return false;
        };
        let archive = Arc::clone(archive);
        tokio::spawn(async move {
            let outcome = seed_shallow(&archive, config).await;
            log::info!("basemap cache seed: {outcome:?}");
        });
        true
    }

    /// The seed itself. See [`maybe_seed`] for where it runs.
    ///
    /// The enumeration asks the archive for every coordinate through
    /// [`SEED_MAX_ZOOM`]; the archive's directories answer which exist
    /// (absence is positive, and the directory reads warm the cache too).
    /// The bytes are dropped — residency is the block cache's, not the tile
    /// LRU's. Before every tile the running total is checked against the
    /// cap, so a seed that would exceed it stops politely instead of
    /// churning its own writes back out.
    pub async fn seed_shallow<S: ArchiveRangeSource>(
        archive: &BasemapArchive<S>,
        config: BlockCacheConfig,
    ) -> SeedOutcome {
        let binding = CacheBinding::obtain(config);
        let marker = super::seed_marker();
        if !binding.shared.ensure_open() {
            return SeedOutcome::Aborted;
        }
        if binding.shared.has_marker(&binding.generation, &marker) {
            return SeedOutcome::AlreadySeeded;
        }

        let deepest = SEED_MAX_ZOOM.min(archive.max_zoom());
        let mut tiles = 0usize;
        for z in 0..=deepest {
            let side = 1u32 << z;
            for x in 0..side {
                for y in 0..side {
                    if binding.shared.total_bytes() >= binding.shared.cap_bytes() {
                        log::info!(
                            "basemap cache seed stopped at the byte cap \
                             ({} of {})",
                            binding.shared.total_bytes(),
                            binding.shared.cap_bytes()
                        );
                        return SeedOutcome::CapReached;
                    }
                    match archive.warm_tile(z, x, y).await {
                        Ok(true) => tiles += 1,
                        Ok(false) => {}
                        Err(error) => {
                            // A transport fault mid-seed: stop, leave no
                            // marker, let the next launch resume over hits.
                            log::warn!("basemap cache seed aborted: {error}");
                            return SeedOutcome::Aborted;
                        }
                    }
                }
            }
        }

        binding.shared.set_marker(&binding.generation, &marker);
        SeedOutcome::Seeded { tiles }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{BlockCachedSource, maybe_seed, seed_shallow};

// ---------------------------------------------------------------------------
// wasm32: the pass-through
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::sync::Arc;

    use super::super::{ArchiveRangeSource, BasemapArchive, RangeError, RangeSource};
    use super::BlockCacheConfig;

    /// The wasm32 arm: the same constructor, no cache — a selection of a
    /// type, per the module doc. The browser's HTTP cache is this target's
    /// persistence story.
    pub struct BlockCachedSource<S> {
        inner: S,
    }

    impl<S> BlockCachedSource<S> {
        /// Wrap `inner`; the configuration is what there is no filesystem to
        /// honour. (In practice it is always `None` here: no platform bridge
        /// hands the web target a cache directory.)
        pub fn new(inner: S, _config: Option<BlockCacheConfig>) -> Self {
            Self { inner }
        }
    }

    impl<S: ArchiveRangeSource> RangeSource for BlockCachedSource<S> {
        fn read_range(
            &self,
            offset: u64,
            length: usize,
        ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
            self.inner.read_range(offset, length)
        }
    }

    /// The wasm32 arm of the seed: never runs — there is no disk for it to
    /// warm. The same selection-of-a-body split as `tiles::archive_url`.
    pub fn maybe_seed<S: ArchiveRangeSource>(
        _archive: &Arc<BasemapArchive<S>>,
        _config: Option<BlockCacheConfig>,
    ) -> bool {
        false
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{BlockCachedSource, maybe_seed};

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests;
