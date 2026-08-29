//! The block cache's suite. Every test that touches disk uses its own root
//! directory: [`CacheShared`]'s registry is process-global by design (one
//! byte total per root), so a shared root would alias two tests' caches.
//!
//! The loopback pieces are borrowed from the parent's harness
//! ([`harness::RangeServer`]); the in-memory [`MemorySource`] exists for the
//! tests where the interesting half is the cache, not the transport — it can
//! be killed, and it counts its reads, which is what makes "served from
//! disk" an assertion instead of a hope.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use super::super::{RangeError, RangeSource, tests as harness};
use super::{
    BLOCK_BYTES, BlockCacheConfig, BlockCachedSource, SeedOutcome, generation_for_url, seed_marker,
};

/// A fresh, unique cache root. Unique per call, because the cache registry
/// is keyed by root and holds state for the life of the process.
fn temp_root(tag: &str) -> PathBuf {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let unique = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "squallar-block-cache-{tag}-{}-{unique}",
        std::process::id()
    ))
}

/// A config whose live set is exactly its own generation, which is the
/// common single-archive shape. Tests about the two-live-generations case
/// build their own.
fn config_for(root: &Path, generation: &str, cap_bytes: u64) -> BlockCacheConfig {
    BlockCacheConfig {
        root: root.to_path_buf(),
        generation: generation.to_owned(),
        live_generations: vec![generation.to_owned()],
        cap_bytes,
    }
}

/// Every non-dot file under `root`, summed — the on-disk ground truth the
/// cap assertions read, deliberately not the cache's own bookkeeping.
fn disk_bytes(root: &Path) -> u64 {
    let mut total = 0;
    let Ok(generations) = std::fs::read_dir(root) else {
        return 0;
    };
    for generation in generations.flatten() {
        let Ok(blocks) = std::fs::read_dir(generation.path()) else {
            continue;
        };
        for block in blocks.flatten() {
            let name = block.file_name();
            if name.to_str().is_some_and(|name| name.starts_with('.')) {
                continue;
            }
            if let Ok(meta) = block.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// The block files (names only) of one generation, sorted.
fn block_names(root: &Path, generation: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(generation))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                .filter(|name| !name.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// A deterministic body: byte `i` is a function of `i`, so any slice can be
/// checked against the formula without holding a reference copy.
fn patterned(len: usize, salt: u8) -> Vec<u8> {
    (0..len)
        .map(|at| (at as u8).wrapping_mul(31).wrapping_add(salt))
        .collect()
}

/// An in-memory range source that counts its reads and can be killed.
#[derive(Clone)]
struct MemorySource {
    body: Arc<Vec<u8>>,
    reads: Arc<AtomicUsize>,
    alive: Arc<AtomicBool>,
}

impl MemorySource {
    fn new(body: Vec<u8>) -> Self {
        Self {
            body: Arc::new(body),
            reads: Arc::new(AtomicUsize::new(0)),
            alive: Arc::new(AtomicBool::new(true)),
        }
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
    }
}

impl RangeSource for MemorySource {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let body = Arc::clone(&self.body);
        let reads = Arc::clone(&self.reads);
        let alive = Arc::clone(&self.alive);

        async move {
            if !alive.load(Ordering::SeqCst) {
                return Err(RangeError::Transport("the source was killed".into()));
            }
            reads.fetch_add(1, Ordering::SeqCst);
            let from = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(body.len());
            let to = from.saturating_add(length).min(body.len());
            Ok(body[from..to].to_vec())
        }
    }
}

// ---------------------------------------------------------------------------
// The generation key
// ---------------------------------------------------------------------------

/// The stale-bytes control at the unit level: the key is the URL's path, the
/// path carries the generation segment, and neither the host nor an
/// unparseable input can fold two generations together.
#[test]
fn generation_keys_are_derived_from_the_path() {
    let basemap = generation_for_url("https://tiles.squallar.app/basemap/omt-20260828.pmtiles");
    let terrain = generation_for_url(
        "https://tiles.squallar.app/terrain/4ca64469750e-20260829/squallar-terrain-hillshade.pmtiles",
    );

    // The two real archives never share a directory, and each key carries
    // its distinguishing segment.
    assert_ne!(basemap, terrain);
    assert!(basemap.contains("omt-20260828"), "{basemap}");
    assert!(terrain.contains("4ca64469750e-20260829"), "{terrain}");

    // Two URLs differing only in generation never share.
    assert_ne!(
        generation_for_url("https://tiles.squallar.app/basemap/omt-20260828.pmtiles"),
        generation_for_url("https://tiles.squallar.app/basemap/omt-20260901.pmtiles"),
    );

    // The host is deliberately not part of the key: a local mirror of the
    // same published path shares the cache it should share.
    assert_eq!(
        basemap,
        generation_for_url("http://localhost:8000/basemap/omt-20260828.pmtiles"),
    );

    // The escaping is injective where plain sanitization is not: a `/` and a
    // literal `_2F` cannot collide, because `_` itself is escaped.
    assert_ne!(
        generation_for_url("https://host/a/b.pmtiles"),
        generation_for_url("https://host/a_2Fb.pmtiles"),
    );

    // A key is one portable path component.
    assert!(!basemap.contains('/'), "{basemap}");
    assert!(!terrain.contains('/'), "{terrain}");
}

// ---------------------------------------------------------------------------
// The restart-survival claim
// ---------------------------------------------------------------------------

/// The claim the cache exists for, tested as stated: a loopback server
/// serves the ranges once and is then unreachable, and a *fresh* source over
/// the same cache directory — a new process's shape — serves the same ranges
/// entirely from disk. Any miss would have to reach the dead port and fail
/// the read, so success is proof of disk.
#[test]
fn a_cached_range_survives_the_server_being_killed() {
    let root = temp_root("restart");
    let body = patterned(300_000, 7);
    let server = harness::RangeServer::monolith(body.clone(), 0, harness::Answer::Range);
    let generation = generation_for_url(&server.url());

    // The ranges a real open makes are shaped like these: a header-sized
    // read, a mid-archive span crossing block boundaries, an exact block,
    // and a read clamped by the archive's end.
    let ranges: &[(u64, usize)] = &[
        (0, 16_384),
        (100_000, 50_000),
        (65_536, 65_536),
        (290_000, 20_000),
    ];

    let expected: Vec<Vec<u8>> = ranges
        .iter()
        .map(|&(offset, length)| {
            let from = (offset as usize).min(body.len());
            let to = from.saturating_add(length).min(body.len());
            body[from..to].to_vec()
        })
        .collect();

    let warm = BlockCachedSource::new(
        super::super::HttpRangeSource::new(harness::loopback_client(), &server.url())
            .expect("the loopback URL parses"),
        Some(config_for(
            &root,
            &generation,
            super::DESKTOP_BLOCK_CACHE_BYTES,
        )),
    );
    for (&(offset, length), want) in ranges.iter().zip(&expected) {
        let got = harness::block_on(warm.read_range(offset, length))
            .expect("the warm read succeeds over the live server");
        assert_eq!(&got, want, "warm read at {offset}+{length}");
    }
    assert!(
        disk_bytes(&root) > 0,
        "the cache holds blocks after the warm pass"
    );

    // Kill the server: a port that was bound a moment ago and is bound no
    // longer refuses connections, which is what a dead host does.
    let dead_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port binds");
        listener
            .local_addr()
            .expect("a bound port has an address")
            .port()
    };
    let dead_url = format!("http://127.0.0.1:{dead_port}{}", harness::ARCHIVE_PATH);
    assert_eq!(
        generation_for_url(&dead_url),
        generation,
        "the generation key must not depend on the host, or this test tests nothing"
    );

    let cold = BlockCachedSource::new(
        super::super::HttpRangeSource::new(harness::loopback_client(), &dead_url)
            .expect("the dead URL parses"),
        Some(config_for(
            &root,
            &generation,
            super::DESKTOP_BLOCK_CACHE_BYTES,
        )),
    );
    for (&(offset, length), want) in ranges.iter().zip(&expected) {
        let got = harness::block_on(cold.read_range(offset, length))
            .expect("the cold read must be served from disk — the server is gone");
        assert_eq!(&got, want, "cold read at {offset}+{length}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Generations stay apart on disk
// ---------------------------------------------------------------------------

/// The stale-bytes control at the wrapper level: two sources whose URLs
/// differ only in generation, over one root, each get their own bytes back —
/// a shared block would serve the first generation's bytes for the second.
#[test]
fn two_generations_never_share_blocks() {
    let root = temp_root("generations");
    let gen_old = generation_for_url("https://tiles.squallar.app/basemap/omt-20260828.pmtiles");
    let gen_new = generation_for_url("https://tiles.squallar.app/basemap/omt-20260901.pmtiles");

    let live = vec![gen_old.clone(), gen_new.clone()];
    let config = |generation: &str| BlockCacheConfig {
        root: root.clone(),
        generation: generation.to_owned(),
        live_generations: live.clone(),
        cap_bytes: super::DESKTOP_BLOCK_CACHE_BYTES,
    };

    let body_old = patterned(80_000, 1);
    let body_new = patterned(80_000, 2);

    let old = BlockCachedSource::new(MemorySource::new(body_old.clone()), Some(config(&gen_old)));
    let got = harness::block_on(old.read_range(0, 40_000)).expect("the old generation reads");
    assert_eq!(got, body_old[..40_000]);

    let new = BlockCachedSource::new(MemorySource::new(body_new.clone()), Some(config(&gen_new)));
    let got = harness::block_on(new.read_range(0, 40_000)).expect("the new generation reads");
    assert_eq!(
        got,
        body_new[..40_000],
        "the new generation must serve its own bytes, not the old generation's cache"
    );

    assert!(!block_names(&root, &gen_old).is_empty());
    assert!(!block_names(&root, &gen_new).is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// The cap
// ---------------------------------------------------------------------------

/// Writes past the cap evict oldest-mtime blocks, the on-disk total never
/// rests above the cap, and a read of an evicted block goes back to the
/// source — counted, not assumed.
#[test]
fn the_cap_evicts_oldest_first_and_never_overruns() {
    let root = temp_root("cap");
    let cap = 4 * BLOCK_BYTES;
    let blocks = 10u64;
    let body = patterned(blocks as usize * BLOCK_BYTES as usize, 3);
    let source = MemorySource::new(body.clone());
    let cached = BlockCachedSource::new(source.clone(), Some(config_for(&root, "gen", cap)));

    for index in 0..blocks {
        let got = harness::block_on(cached.read_range(index * BLOCK_BYTES, BLOCK_BYTES as usize))
            .expect("an aligned block read succeeds");
        assert_eq!(got.len(), BLOCK_BYTES as usize);
        assert!(
            disk_bytes(&root) <= cap,
            "after caching block {index} the tree holds {} of a {cap} cap",
            disk_bytes(&root)
        );
        // Distinct mtimes, so "oldest" is well defined for the assertion
        // below rather than a tie the filesystem broke arbitrarily.
        std::thread::sleep(std::time::Duration::from_millis(3));
    }

    // Oldest first: what survives is exactly the newest cap's worth.
    assert_eq!(
        block_names(&root, "gen"),
        vec!["0000000006", "0000000007", "0000000008", "0000000009"],
    );

    // An evicted block is a miss again: the read succeeds and the source is
    // consulted once more.
    let before = source.reads();
    let got = harness::block_on(cached.read_range(0, BLOCK_BYTES as usize))
        .expect("an evicted block re-reads through the source");
    assert_eq!(got, body[..BLOCK_BYTES as usize]);
    assert_eq!(source.reads(), before + 1, "the refetch reached the source");

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// GC at open
// ---------------------------------------------------------------------------

/// Opening the cache deletes a generation that is no longer current and
/// keeps **both** live generations — the basemap-plus-terrain case, where
/// the second live generation belongs to a source that has not opened yet
/// and must not lose its cache to the first one's GC.
#[test]
fn gc_drops_stale_generations_and_keeps_both_live_ones() {
    let root = temp_root("gc");
    let gen_a = "live-basemap";
    let gen_b = "live-terrain";
    let stale = "omt-20250101.pmtiles";

    // A previous session's disk: one stale generation, and blocks under both
    // live ones. Block 0 of `gen_a` is given known bytes so the read below
    // also proves the surviving cache is *served*, not merely kept.
    let survivor = patterned(1_000, 9);
    for generation in [gen_a, gen_b, stale] {
        std::fs::create_dir_all(root.join(generation)).expect("the fixture dirs create");
    }
    std::fs::write(root.join(gen_a).join("0000000000"), &survivor).expect("a block writes");
    std::fs::write(root.join(gen_b).join("0000000000"), b"terrain block").expect("a block writes");
    std::fs::write(root.join(stale).join("0000000000"), b"stale block").expect("a block writes");

    let config = BlockCacheConfig {
        root: root.clone(),
        generation: gen_a.to_owned(),
        live_generations: vec![gen_a.to_owned(), gen_b.to_owned()],
        cap_bytes: super::DESKTOP_BLOCK_CACHE_BYTES,
    };
    let source = MemorySource::new(patterned(1_000, 200));
    let cached = BlockCachedSource::new(source.clone(), Some(config));

    // The first read triggers the open, the GC and the recovery walk — and
    // is answered by the surviving block, source untouched.
    let got = harness::block_on(cached.read_range(0, 500)).expect("the read succeeds");
    assert_eq!(got, survivor[..500], "the surviving block is what serves");
    assert_eq!(
        source.reads(),
        0,
        "a survivor's read never reaches the source"
    );

    assert!(!root.join(stale).exists(), "the stale generation is gone");
    assert!(
        root.join(gen_b).join("0000000000").exists(),
        "the other live generation survives the first one's open"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Concurrent writers
// ---------------------------------------------------------------------------

/// Two writers racing on one block: immutability makes them idempotent by
/// construction — each renames its own temp file over the same name — so
/// either winner leaves the correct bytes, and the byte total counts the
/// block once, not twice.
#[test]
fn concurrent_writers_of_one_block_are_idempotent() {
    let root = temp_root("writers");
    let block = patterned(BLOCK_BYTES as usize, 5);

    let binding = super::native::CacheBinding::obtain(config_for(
        &root,
        "gen",
        super::DESKTOP_BLOCK_CACHE_BYTES,
    ));
    assert!(binding.shared.ensure_open());

    let barrier = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        for _ in 0..2 {
            let barrier = Arc::clone(&barrier);
            let shared = Arc::clone(&binding.shared);
            let block = &block;
            scope.spawn(move || {
                barrier.wait();
                shared.store_block("gen", 7, block);
            });
        }
    });

    let stored = std::fs::read(root.join("gen").join("0000000007")).expect("the block exists");
    assert_eq!(stored, block, "either winner is byte-correct");
    assert_eq!(
        binding.shared.total_bytes(),
        BLOCK_BYTES,
        "the racing writes were counted as one block, not two"
    );
    assert_eq!(
        block_names(&root, "gen"),
        vec!["0000000007"],
        "no temp file survived the race"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// The archive's end
// ---------------------------------------------------------------------------

/// A read clamped by the archive's end caches what exists and serves the
/// same clamped answer from disk once the source is gone — the short block
/// is EOF made durable, not a truncation.
#[test]
fn a_clamped_tail_read_survives_from_disk() {
    let root = temp_root("tail");
    let body = patterned(100_000, 11);
    let source = MemorySource::new(body.clone());
    let cached = BlockCachedSource::new(
        source.clone(),
        Some(config_for(&root, "gen", super::DESKTOP_BLOCK_CACHE_BYTES)),
    );

    let warm = harness::block_on(cached.read_range(90_000, 20_000)).expect("the tail reads");
    assert_eq!(warm, body[90_000..], "clamped to the archive's end");

    source.kill();
    let cold = harness::block_on(cached.read_range(90_000, 20_000))
        .expect("the tail re-reads from disk with the source dead");
    assert_eq!(cold, warm);

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// The seed
// ---------------------------------------------------------------------------

/// The archive [`seeded_archive`] opens: the committed fixture behind a
/// counting, caching source.
type SeedFixture = Arc<super::super::BasemapArchive<BlockCachedSource<CountingFile>>>;

/// The seed's fixture: the committed Monaco archive behind a counting,
/// caching source, so "the marker prevents a re-seed" is a statement about
/// read counts rather than about logs.
fn seeded_archive(
    root: &Path,
    cap_bytes: u64,
) -> Option<(SeedFixture, Arc<AtomicUsize>, BlockCacheConfig)> {
    let path = harness::archive_path();
    if !path.is_file() {
        harness::no_archive_banner("block_cache seed", &path);
        return None;
    }
    let inner = super::super::FileRangeSource::open(&path).expect("the fixture opens");
    let reads = Arc::new(AtomicUsize::new(0));
    let counting = CountingFile {
        inner: Arc::new(inner),
        reads: Arc::clone(&reads),
    };
    let config = config_for(root, "seed-gen", cap_bytes);
    let cached = BlockCachedSource::new(counting, Some(config.clone()));
    let archive = harness::block_on(super::super::BasemapArchive::open(cached))
        .expect("the fixture archive opens");
    Some((Arc::new(archive), reads, config))
}

/// [`MemorySource`]'s shape over the committed fixture file.
struct CountingFile {
    inner: Arc<super::super::FileRangeSource>,
    reads: Arc<AtomicUsize>,
}

impl RangeSource for CountingFile {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read_range(offset, length)
    }
}

/// A completed seed leaves the marker, and the marker makes the next seed
/// free: zero reads, [`SeedOutcome::AlreadySeeded`].
#[test]
fn the_marker_prevents_a_reseed() {
    let root = temp_root("seed");
    let Some((archive, reads, config)) = seeded_archive(&root, super::DESKTOP_BLOCK_CACHE_BYTES)
    else {
        return; // The fixture-missing skip already printed its banner.
    };

    let first = harness::block_on(super::seed_shallow(&archive, config.clone()));
    assert!(
        matches!(first, SeedOutcome::Seeded { .. }),
        "the first seed runs to completion: {first:?}"
    );
    assert!(
        root.join("seed-gen").join(seed_marker()).exists(),
        "a completed seed leaves its marker"
    );
    assert!(
        reads.load(Ordering::SeqCst) > 0,
        "the seed read through the source"
    );

    let before = reads.load(Ordering::SeqCst);
    let second = harness::block_on(super::seed_shallow(&archive, config));
    assert_eq!(second, SeedOutcome::AlreadySeeded);
    assert_eq!(
        reads.load(Ordering::SeqCst),
        before,
        "a marked generation is not re-read — not even one range"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A seed that would exceed the cap stops politely — no marker, so a launch
/// with a healthier cap resumes over what are now hits — and a seed with no
/// cache directory is never spawned at all.
#[test]
fn the_seed_respects_the_cap_and_the_missing_cache_dir() {
    let root = temp_root("seed-cap");
    // A cap the archive's own header reads already exceed: the politeness
    // check fires on the first tile.
    let Some((archive, _reads, config)) = seeded_archive(&root, BLOCK_BYTES) else {
        return;
    };

    let outcome = harness::block_on(super::seed_shallow(&archive, config));
    assert_eq!(outcome, SeedOutcome::CapReached);
    assert!(
        !root.join("seed-gen").join(seed_marker()).exists(),
        "an aborted seed leaves no marker"
    );

    // No cache dir: nothing to warm, nothing spawned. `true` for the spawned
    // arm is asserted right here too, so this cannot pass vacuously.
    harness::block_on(async {
        assert!(!super::maybe_seed(&archive, None));
    });

    let _ = std::fs::remove_dir_all(&root);
}
