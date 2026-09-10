//! Fetch GLM lightning flash data from AWS S3.
//!
//! Lists and downloads L2 LCFA NetCDF4 files from the public GOES buckets
//! declared in [`DataSources`]. Each granule covers ~20 seconds.

use std::collections::HashMap;

use chrono::{NaiveDateTime, TimeDelta};
use squallar_source::origins::DataSources;
use squallar_source::time::Residency;

use super::{
    DeadFeed, FetchFailures, GLM_MAX_TIME_WINDOW_SECS, GLM_MIN_TIME_WINDOW_SECS, GlmDataLevel,
    GlmFetchOutcome, GlmFlash, GlmSatellite, LevelFailure, RecordDrops, WindowGap,
};
use crate::fetch_policy::{FetchError, NotFound};
use squallar_netcdf::cf;

#[derive(Clone)]
struct CachedGranule {
    /// **Behind an `Arc`, which is what makes [`GlmStore::snapshot`] a map of
    /// handles rather than a second copy of the rows.**
    ///
    /// A poll may not hold a `std::sync::Mutex` across an `await`, so it works
    /// on a clone of the whole `GlmCache` and writes it back — see
    /// [`poll_glm_into_store`]. With the rows owned inline that clone was a
    /// second `Vec<GlmFlash>` per granule, resident for the whole poll (list,
    /// download **and** parse) rather than momentarily, so the peak was
    /// **twice** [`GlmCache::retained_bytes`]: 6,681,600 B at the shipped
    /// default posture and 24,000,000 B at [`MAX_RETAINED_FLASHES`]. Shared,
    /// the clone is the `HashMap`'s table and one `String` key per granule —
    /// kilobytes at the ~16 granules the cap holds, and it does not scale with
    /// the flash count at all.
    ///
    /// **Sharing is sound because a granule's rows are immutable once built.**
    /// `entries` is private to this module and every writer of it
    /// ([`GlmCache::insert`], [`GlmCache::evict_before`],
    /// [`GlmCache::evict_oldest_over`]) replaces or removes a **whole**
    /// granule; nothing anywhere takes a `&mut Vec<GlmFlash>` out of one, so
    /// no copy-on-write door is needed and none is offered.
    flashes: std::sync::Arc<Vec<GlmFlash>>,
    newest: NaiveDateTime,
}

impl CachedGranule {
    fn new(granule_start: NaiveDateTime, flashes: Vec<GlmFlash>) -> Self {
        let newest = granule_newest(granule_start, &flashes);
        CachedGranule {
            flashes: std::sync::Arc::new(flashes),
            newest,
        }
    }
}

/// **The instant a granule is aged by**, and the one half of
/// [`GlmCache::evict_oldest_over`]'s sort key that is not the S3 key.
///
/// Shared with [`GranuleSink`] rather than spelled twice: the sink has to
/// decide whether a granule that has only just parsed sits above or below a
/// trim that has already happened, and a second definition of "how old is this
/// granule" is exactly how that decision would drift out of agreement with the
/// eviction it exists to match.
fn granule_newest(granule_start: NaiveDateTime, flashes: &[GlmFlash]) -> NaiveDateTime {
    flashes
        .iter()
        .map(|f| f.time)
        .max()
        .unwrap_or(granule_start)
}

#[derive(Default, Clone)]
pub struct GlmCache {
    entries: HashMap<String, CachedGranule>,
    /// **Rows the map is holding, maintained beside it rather than folded out
    /// of it** — the level [`Self::retained_bytes`] prices and the census
    /// reads through [`GlmStore::retained_bytes`].
    ///
    /// Beside, for the reason [`crate::staging::StagingPool`]'s
    /// `retained_points` is beside its slot: the reader is the frame thread's
    /// telemetry tick, the map is behind a `Mutex` a poll holds while it
    /// writes back, and a reader that missed the lock would have to answer a
    /// false **zero** on a store of up to 12 MB. A missing family shows up in
    /// the census residual; a false zero does not.
    ///
    /// Maintained at every site that adds or removes a granule and **nowhere
    /// else it could be** — `entries` is private to this module, so the four
    /// writers below are the whole set by the language's rule rather than by
    /// anyone's recollection. [`Self::flash_count`] stays an independent walk
    /// of the map so a test can disagree with this field; nothing shipped
    /// compares them, and [`Self::evict_oldest_over`] deliberately enforces
    /// the cap off the walk, so a drifted level cannot mis-evict.
    retained_flashes: usize,
}

/// **Bytes one retained row costs**, read through `size_of` rather than
/// spelled: `GlmFlash` owns nothing on the heap, so its `size_of` is the whole
/// price of a row and a field added to it carries this figure with it.
pub const FLASH_BYTES: usize = size_of::<GlmFlash>();

/// Ceiling on flashes retained across a depicted span, enforced by
/// [`GlmCache::evict_oldest_over`] and **only under a span posture** — a live
/// pane's window is bounded by [`super::GLM_MAX_TIME_WINDOW_SECS`] alone, as
/// it always was.
///
/// The denominator: `250_000 × size_of::<GlmFlash>()`, measured by
/// `a_spanned_poll_caps_what_it_retains_and_drops_its_oldest_hours_first` as
/// **12 000 000 bytes at 48 bytes a row** — the whole cost, since `GlmFlash`
/// owns nothing on the heap. Every raster job ships at most this many rows.
///
/// **How much *time* 250 000 rows covers is not measured here**, and it is a
/// function of the level and the weather rather than of this constant. The one
/// figure the tree carries is [`RecordDrops`]'s: 1584507 records over 105
/// granules with all three levels on, ~15k rows per 20 s granule, at which the
/// cap holds ~16 granules. The shipped default is groups and flashes with
/// events off ([`GlmDataLevel`]), for which no per-granule count has been
/// measured — do not infer one from the all-levels figure. What *is*
/// guaranteed is that eviction is oldest-first, so a loop that overflows the
/// cap keeps its **newest** hours lit rather than its oldest.
pub const MAX_RETAINED_FLASHES: usize = 250_000;

/// **How many granule GETs are in flight at once**, and therefore how many
/// granule *files* [`download_and_parse_batch`] may hold at one time.
///
/// A ceiling on **peak memory**, not on politeness — the same kind of constant
/// as `mrms::volume::STACK_FETCH_CONCURRENCY`, and priced the same way. Each
/// slot holds one whole granule body from the moment it arrives until its parse
/// returns: 280,380 B on the measured product granule
/// (`squallar-overlays/testdata/OR_GLM-L2-LCFA_G19_…nc`), so twenty slots are
/// **5,607,600 B** of file buffers.
///
/// **It bounds the bodies, not the parses.** `buffer_unordered` polls its
/// sub-futures inline from one task, so however many bodies are in flight
/// exactly one parse runs at a time — which is why the copy
/// [`parse_glm_granule`] removed was one granule and not twenty of them.
/// `tests/glm_poll_peak.rs::a_batch_holds_no_more_bodies_than_the_concurrency_cap`
/// measures the bodies: a 45-object batch reads 6,970,916–8,415,961 B under
/// this cap and 17,235,745–17,840,225 B without it.
///
/// **Justified, not derived.** MRMS holds four because one slot there is a
/// 49 MB values vector; a GLM slot is 175× smaller, so the same politeness
/// budget buys far more of them, and nothing measured here says where the
/// latency curve of a cold poll turns over. What the figure above does is make
/// the cost of the number sayable, which a bare `20` at the call site could
/// not.
///
/// **The shipped default posture never reaches it.** A batch is one
/// satellite's new keys, and a 300 s window at one granule per 20 s is 15 of
/// them; the two satellites are downloaded in sequence, so a cold live poll
/// peaks at 15 in flight. The cap binds under a wider window — this layer
/// allows up to [`super::GLM_MAX_TIME_WINDOW_SECS`], 90 granules — and under a
/// span or loop posture, which is where a cold round's file buffers are worth
/// bounding at all.
pub const GRANULE_FETCH_CONCURRENCY: usize = 20;

/// **What one [`GlmCache::evict_oldest_over`] call actually did**, split by
/// whether the rows came back.
#[derive(Default, Clone, PartialEq, Eq, Debug)]
pub struct Eviction {
    pub granules: usize,
    pub rows: usize,
    /// Rows in granules this cache was the **last owner of** — the only ones
    /// whose bytes the allocator gets back. See the method's own note.
    pub sole_rows: usize,
    /// **The highest sort key this trim refused**, in
    /// [`GlmCache::evict_oldest_over`]'s own `(newest, key)` terms — the
    /// retention floor a later arrival has to clear. `None` if nothing was
    /// evicted.
    ///
    /// It carries the S3 key and not the instant alone because the *key* is
    /// what breaks a tie in that method's sort, and both satellites publish on
    /// the same 20 s grid: a floor that compared instants only would refuse a
    /// tied granule the end-of-poll trim kept, and which of a tied pair
    /// survived would then depend on which one the wire answered first.
    pub floor: Option<(NaiveDateTime, String)>,
}

impl Eviction {
    fn absorb(&mut self, other: Eviction) {
        self.granules += other.granules;
        self.rows += other.rows;
        self.sole_rows += other.sole_rows;
        if other.floor > self.floor {
            self.floor = other.floor;
        }
    }
}

impl GlmCache {
    pub fn evict_before(&mut self, cutoff: NaiveDateTime) {
        let retained = &mut self.retained_flashes;
        self.entries.retain(|_key, granule| {
            let keep = granule.newest >= cutoff;
            if !keep {
                *retained = retained.saturating_sub(granule.flashes.len());
            }
            keep
        });
    }

    /// **A walk of the map**, and the independent second opinion
    /// [`Self::retained_flashes`] is checked against. Deliberately not routed
    /// through the maintained level: a cap enforced off the same field the
    /// level is read from would agree with itself after a mutation site
    /// forgot to update it.
    pub fn flash_count(&self) -> usize {
        self.entries.values().map(|g| g.flashes.len()).sum()
    }

    /// **Rows resident right now**, off the maintained level — no walk, no
    /// allocation. See [`Self::retained_flashes`]' field docs for why it is
    /// maintained rather than folded.
    pub fn retained_flashes(&self) -> usize {
        self.retained_flashes
    }

    /// **Bytes those rows cost**, which is the whole cost: a granule is one
    /// `Vec<GlmFlash>` and `GlmFlash` owns nothing on the heap, so
    /// `rows * FLASH_BYTES` prices the map's contents exactly. The `HashMap`'s
    /// own table and its `String` keys are not in it — at the shipped cap
    /// those are ~16 keys against 250,000 rows.
    pub fn retained_bytes(&self) -> usize {
        self.retained_flashes * FLASH_BYTES
    }

    /// Drop whole granules, oldest first, until at most `cap` flashes remain
    /// — the byte bound on span retention (see [`MAX_RETAINED_FLASHES`]).
    /// Whole granules so [`Self::contains_key`] stays the download planner's
    /// truth: a half-kept granule would be "cached" and never refetched.
    ///
    /// **What it reports is what it freed, not what it removed**, and the two
    /// are different numbers: a poll works on a [`GlmStore::snapshot`], whose
    /// granules are `Arc`s the store still holds, so removing a carried-in
    /// granule from this map drops a refcount and nothing else. Only a
    /// granule this cache is the last owner of returns bytes to the
    /// allocator, and [`Eviction::sole_rows`] is that half — measured with
    /// `Arc::strong_count` at the instant of removal rather than assumed.
    pub fn evict_oldest_over(&mut self, cap: usize) -> Eviction {
        let mut evicted = Eviction::default();
        let mut total = self.flash_count();
        if total <= cap {
            return evicted;
        }
        let mut by_age: Vec<(NaiveDateTime, String)> = self
            .entries
            .iter()
            .map(|(key, granule)| (granule.newest, key.clone()))
            .collect();
        by_age.sort();
        for (_, key) in by_age {
            if total <= cap {
                break;
            }
            if let Some(granule) = self.entries.remove(&key) {
                let rows = granule.flashes.len();
                total -= rows;
                self.retained_flashes = self.retained_flashes.saturating_sub(rows);
                evicted.granules += 1;
                evicted.rows += rows;
                if std::sync::Arc::strong_count(&granule.flashes) == 1 {
                    evicted.sole_rows += rows;
                }
                let refused = (granule.newest, key);
                if Some(&refused) > evicted.floor.as_ref() {
                    evicted.floor = Some(refused);
                }
            }
        }
        evicted
    }

    /// Granules held, for the fires-counter's before/after — a walk of the
    /// map's own length, not a maintained level.
    pub fn granule_count(&self) -> usize {
        self.entries.len()
    }

    pub fn all_flashes(&self) -> impl Iterator<Item = &GlmFlash> {
        self.entries.values().flat_map(|g| g.flashes.iter())
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// **A re-inserted key replaces its granule**, so the level loses the rows
    /// the old one held before it gains the new one's — an insert counted as a
    /// pure addition would drift upward on every refetch of a key already
    /// held.
    pub fn insert(&mut self, key: String, granule_start: NaiveDateTime, flashes: Vec<GlmFlash>) {
        let added = flashes.len();
        if let Some(replaced) = self
            .entries
            .insert(key, CachedGranule::new(granule_start, flashes))
        {
            self.retained_flashes = self.retained_flashes.saturating_sub(replaced.flashes.len());
        }
        self.retained_flashes += added;
    }
}

/// **The shared lightning cache, and the byte level a frame may read without
/// waiting for it.**
///
/// The handler's store used to be a bare `Arc<Mutex<GlmCache>>`, which is a
/// store held *beside* its `OverlayState` and therefore in no census family at
/// all: [`crate::render::footprint`] prices what an `OverlayState` installed,
/// and up to [`MAX_RETAINED_FLASHES`] rows — 12,000,000 B — were in neither
/// that figure nor `overlay grids`. Dark bytes in the governor's residual are
/// exactly what makes a whole-heap reading unattributable.
///
/// **Two fields rather than one, and that is the whole design.** The map is
/// behind the `Mutex` a poll holds while it clones the cache out and writes it
/// back; the level is an atomic beside it. A census on the frame thread's
/// telemetry tick may not block, and the two lock-free alternatives are both
/// worse than the atomic: `try_lock` answers a **false zero** on a 12 MB store
/// whenever a poll is writing back, and a false zero is worse than a missing
/// family — the missing one shows up in the residual. Same shape, same reason,
/// as [`crate::staging::StagingPool`]'s `retained_points`.
///
/// **The level cannot go stale behind a mutation**, because [`Self::with_mut`]
/// is the only door to a `&mut GlmCache` this type has and it republishes
/// [`GlmCache::retained_bytes`] before it releases the lock. The `Mutex` is
/// private, so that is the language's guarantee rather than a convention.
pub struct GlmStore {
    cache: std::sync::Mutex<GlmCache>,
    /// Published copy of [`GlmCache::retained_bytes`], refreshed at the end of
    /// every [`Self::with_mut`]. `Relaxed`, like every census level: a reader
    /// wants a recent figure, none wants a synchronised one.
    retained_bytes: std::sync::atomic::AtomicUsize,
    /// **One round of this layer at a time**, and the whole of the fix for a
    /// lost update — see [`poll_glm_into_store`].
    ///
    /// `Gui::panes_owed_a_round` splits on the depicted instant, so six loop
    /// panes at six playheads dispatch six polls in one frame; every one of
    /// them ran `snapshot` → private cache → [`Self::replace`], so the last to
    /// finish published a cache built on a snapshot taken before the other
    /// five wrote theirs and **five rounds' granules were discarded**. A
    /// download the poll had already paid for, and one the next round pays for
    /// again because [`GlmCache::contains_key`] is the download planner's
    /// truth.
    ///
    /// The same shape and the same reason as `MrmsHandler::frame_gate` and
    /// `GmgsiHandler`'s: an async mutex, FIFO-fair, held across the awaits a
    /// `std::sync::MutexGuard` may not cross. Serialising costs the round
    /// nothing it was not already paying — the bytes are the bottleneck either
    /// way — and it buys the followers their predecessor's granules, which is
    /// what turns six polls of one archive into one poll and five plans that
    /// find their keys already held.
    round: futures::lock::Mutex<()>,
    /// **Which set of levels the cached granules were parsed under.** Bumped by
    /// [`Self::clear`], which is what a level change calls.
    ///
    /// The gate above cannot serialise a *frame-thread* write against a poll:
    /// `GlmHandler::clear_cache` runs on the control edit, not on the fetch
    /// task, so a level change during an in-flight round would be undone by
    /// that round's write-back — the same lost update in the other direction,
    /// and worse, because what it restores was parsed under the levels the user
    /// just turned off. A round whose generation moved under it discards its
    /// own cache instead ([`Self::replace_if_current`]).
    generation: std::sync::atomic::AtomicUsize,
}

impl Default for GlmStore {
    fn default() -> Self {
        GlmStore {
            cache: std::sync::Mutex::new(GlmCache::default()),
            retained_bytes: std::sync::atomic::AtomicUsize::new(0),
            round: futures::lock::Mutex::new(()),
            generation: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl GlmStore {
    /// **Bytes the cache is holding, without taking its lock** — the figure
    /// `GlmHandler::resident_source_bytes` answers with.
    ///
    /// One relaxed load, so it is safe on the frame thread's telemetry tick and
    /// on the allocation-error hook's path behind it. Never a walk, never a
    /// lock, and never a zero it does not mean.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// **The only way to mutate the cache**, and therefore the only place the
    /// published level can fall behind — which is why the republish is here
    /// rather than at each caller.
    ///
    /// A poisoned lock is taken over rather than propagated, as every other
    /// caller of this mutex already did: a panicked poll leaves a *stale*
    /// cache, not an invalid one, and refusing to fetch again would be a worse
    /// outcome than redrawing yesterday's flashes.
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut GlmCache) -> R) -> R {
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let out = f(&mut guard);
        self.retained_bytes
            .store(guard.retained_bytes(), std::sync::atomic::Ordering::Relaxed);
        out
    }

    /// A clone of the cache, for a poll that must not hold a `std::sync::Mutex`
    /// across an `await`. See [`poll_glm_into_store`] for what the poll then
    /// does with it.
    ///
    /// **A map of handles, not a second copy of the rows.** Each granule's
    /// `Vec<GlmFlash>` sits behind an `Arc` (see `CachedGranule`), so this
    /// allocates the `HashMap`'s table and one `String` key per granule and
    /// shares every row with the store. It used to clone the rows as well,
    /// which put a second [`Self::retained_bytes`] on the heap for the whole
    /// length of a poll — list, download and parse — and made the peak
    /// 6,681,600 B at the shipped default posture and 24,000,000 B at
    /// [`MAX_RETAINED_FLASHES`].
    ///
    /// What this costs is still not in [`Self::retained_bytes`], which is a
    /// level of what the *store* holds; it is a local of the poll's future and
    /// is priced nowhere. It no longer scales with the flash count, which is
    /// what made it worth pricing.
    pub fn snapshot(&self) -> GlmCache {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// [`Self::snapshot`] and the level generation it was taken under, read
    /// **under one hold of the lock** so the pair cannot describe two different
    /// states of the store.
    pub fn snapshot_at(&self) -> (GlmCache, usize) {
        let guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
        (guard.clone(), generation)
    }

    /// Put a whole cache in place of the current one — the poll's write-back
    /// and the level-change clear, both of which replace rather than edit.
    pub fn replace(&self, cache: GlmCache) {
        self.with_mut(|held| *held = cache);
    }

    /// **Write a round back only if its levels are still the ones the layer is
    /// showing**, and say whether it did.
    ///
    /// `false` is a level change that landed while the round was in flight: the
    /// granules in `cache` were parsed under the old level set, so publishing
    /// them would put rows the user just turned off back on the map, and would
    /// undo the clear as well. Discarding the round is the only answer that
    /// leaves the store describing one level set.
    pub fn replace_if_current(&self, generation: usize, cache: GlmCache) -> bool {
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if self.generation.load(std::sync::atomic::Ordering::Acquire) != generation {
            return false;
        }
        *guard = cache;
        self.retained_bytes
            .store(guard.retained_bytes(), std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// **Drop every granule and declare a new level generation** — what a
    /// change to the level selection calls, and the one write that is allowed
    /// to happen off the fetch task.
    ///
    /// The bump is inside the lock and released with it, so a round that
    /// snapshotted before this call cannot read the new generation and then
    /// write the old cache back.
    pub fn clear(&self) {
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = GlmCache::default();
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.retained_bytes
            .store(guard.retained_bytes(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// **One poll of the archive, against the shared store** — the
/// snapshot/write-back pair, spelled in the module that owns the cache rather
/// than inline in `GlmHandler::create_fetch_tasks`.
///
/// **What the copy is for, and what it is not for.** A `std::sync::MutexGuard`
/// may not be held across an `await`, so the poll works on a clone and writes
/// it back. That is the whole of it: the write-back is **unconditional**, so a
/// round that errors at the listing still installs whatever the local copy
/// reached — the old cache is *not* a rollback and nothing here treats it as
/// one. What the shape does guarantee is that a future **dropped** before it
/// completes leaves the store untouched, because the write-back is the last
/// thing it does. Both properties are the ones the inline spelling had.
///
/// **And one round at a time, which is what makes the copy safe at all.** The
/// snapshot/write-back pair is a read-modify-write, so two of them in flight
/// lose one of the two: six loop panes at six playheads dispatch six polls in
/// one frame ([`GlmStore::round`] carries the measured shape), each snapshots
/// the store, each downloads its own residency and the last to finish publishes
/// a cache that never saw the other five. Every row of theirs was downloaded,
/// parsed and then discarded, and the next round re-downloads it because
/// [`GlmCache::contains_key`] is the download planner's truth.
///
/// The gate makes the followers cheap rather than merely correct: a poll that
/// starts after its predecessor's write-back plans against the granules that
/// predecessor installed, so an ask covering the same instants downloads
/// **nothing**. Six polls of one archive become one poll and five plans.
///
/// The rows are shared with the store for the length of the poll rather than
/// copied (see `CachedGranule`), so the peak is one
/// [`GlmCache::retained_bytes`] plus the granule map, not two of them. That is
/// per **poll**, and with the gate it is also per app: the peak of a round is
/// one poll's, where six concurrent polls held six.
pub async fn poll_glm_into_store(
    store: &GlmStore,
    client: &reqwest::Client,
    sources: &DataSources,
    satellites: &[GlmSatellite],
    levels: &[GlmDataLevel],
    as_of: NaiveDateTime,
    depicted: Residency,
) -> Result<GlmFetchOutcome, FetchError> {
    // **Counted before it is awaited**, because the answer is the question:
    // a `try_lock` that fails is a round that would have raced the one holding
    // it, and after this landing there is nothing else that number can be read
    // off. `futures::lock::Mutex` is FIFO-fair, so a queued round is served in
    // arrival order rather than starved.
    let held = store.round.try_lock();
    let _round = match held {
        Some(guard) => guard,
        None => {
            gauge::round_queued();
            // Logged as well as counted: the gauge is process-global and a leg
            // reads it off the log, so a mechanism that fires only in a
            // counter nothing prints is a mechanism no leg can report.
            log::info!(
                "GLM: a round found another already in flight and queued behind \
                 it rather than racing its write-back",
            );
            store.round.lock().await
        }
    };
    let (mut local_cache, generation) = store.snapshot_at();
    let result = fetch_glm_flashes(
        client,
        sources,
        satellites,
        levels,
        &mut local_cache,
        as_of,
        depicted,
    )
    .await;
    if !store.replace_if_current(generation, local_cache) {
        gauge::round_discarded_stale();
        log::info!(
            "GLM: the level selection changed while a round was in flight; its \
             granules were parsed under the old levels and are discarded rather \
             than published over the clear",
        );
    }
    result
}

/// **What the streaming install actually did, across every poll of the
/// process** — always on, no feature gate and no sampling, because the levels
/// it reports are the whole evidence for the cut that introduced it.
///
/// Every figure names its denominator. `polls` is the number of polls that
/// reached the install stage at all — the divisor for every other row here.
/// `peak_rows` and `unstreamed_peak_rows` are **high-water marks over polls**,
/// not sums: the largest single poll's cache high-water, against what that
/// same poll would have held with one end-of-poll trim. They are directly
/// comparable and their difference is this mechanism's saving, priced on the
/// same poll of the same leg rather than across a night — which matters here
/// because this app's `live_peak` has a 703 MiB within-night spread on
/// identical code, so a paired before/after on the peak could not have said
/// this.
///
/// `trim_sole_rows` is the honest half of the eviction. A poll works on a
/// [`GlmStore::snapshot`], so a granule it evicts that the store still holds is
/// a refcount and not a free; only a granule this cache was the last owner of
/// returns bytes. See [`GlmCache::evict_oldest_over`].
pub mod gauge {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

    static POLLS: AtomicUsize = AtomicUsize::new(0);
    static PEAK_ROWS: AtomicUsize = AtomicUsize::new(0);
    static UNSTREAMED_PEAK_ROWS: AtomicUsize = AtomicUsize::new(0);
    static TRIM_GRANULES: AtomicUsize = AtomicUsize::new(0);
    static TRIM_ROWS: AtomicUsize = AtomicUsize::new(0);
    static TRIM_SOLE_ROWS: AtomicUsize = AtomicUsize::new(0);
    static REFUSED_GRANULES: AtomicUsize = AtomicUsize::new(0);
    static REFUSED_ROWS: AtomicUsize = AtomicUsize::new(0);
    static ROUNDS_QUEUED: AtomicUsize = AtomicUsize::new(0);
    static ROUNDS_DISCARDED_STALE: AtomicUsize = AtomicUsize::new(0);
    static KEYS_PLANNED: AtomicUsize = AtomicUsize::new(0);
    static KEYS_ALREADY_HELD: AtomicUsize = AtomicUsize::new(0);
    static EMPTY_DELIVERIES: AtomicUsize = AtomicUsize::new(0);
    static EMPTY_DELIVERY_ROWS: AtomicUsize = AtomicUsize::new(0);
    static STOPPED_GRANULES: AtomicUsize = AtomicUsize::new(0);
    static STOPPED_BYTES: AtomicU64 = AtomicU64::new(0);
    static FETCHED_GRANULES: AtomicUsize = AtomicUsize::new(0);
    static FETCHED_BYTES: AtomicU64 = AtomicU64::new(0);
    static PLANNED_BYTES: AtomicU64 = AtomicU64::new(0);
    static BOUND_EXCEEDED: AtomicUsize = AtomicUsize::new(0);

    /// **A round that found the gate held**, and therefore a round that used to
    /// race the one holding it — the fires-counter for
    /// [`super::GlmStore::round`]. A mechanism whose counter can only read zero
    /// is a mechanism nobody can show ever ran; this one reads the number of
    /// polls that would have lost or clobbered a write-back.
    pub(super) fn round_queued() {
        ROUNDS_QUEUED.fetch_add(1, Relaxed);
    }

    /// A round whose level generation moved under it, so its granules were
    /// discarded rather than published over the clear.
    pub(super) fn round_discarded_stale() {
        ROUNDS_DISCARDED_STALE.fetch_add(1, Relaxed);
    }

    /// **What the download planner did with the store it found**, per poll and
    /// summed: keys it planned a GET for, and keys it skipped because the cache
    /// already held that granule.
    ///
    /// The second is the gate's saving in the units the network is billed in. A
    /// follower of a coalesced round plans against its predecessor's granules,
    /// so an ask over the same instants reads `planned == 0` and every one of
    /// its keys lands here instead.
    pub(super) fn planned(planned: usize, already_held: usize, planned_bytes: u64) {
        KEYS_PLANNED.fetch_add(planned, Relaxed);
        KEYS_ALREADY_HELD.fetch_add(already_held, Relaxed);
        PLANNED_BYTES.fetch_add(planned_bytes, Relaxed);
    }

    /// **The early stop's fires-counter** — see [`super::FloorCell::refuses`].
    ///
    /// `stopped` against `fetched` is the cut priced in the units the wire is
    /// billed in, and `planned_bytes` above is the denominator both share, all
    /// three summed over the same polls of the same leg. A mechanism that
    /// executed zero times reads zero here, which is the only thing that
    /// distinguishes it from one that did.
    ///
    /// `bound_exceeded` is the soundness witness rather than a saving:
    /// [`super::granule_bound_of`] substitutes a key-derived upper bound for a
    /// quantity only the parsed rows carry, and this is the count of granules
    /// whose rows went past it. Nonzero means the early stop can refuse a
    /// granule [`super::GranuleSink::install`] would have kept.
    pub(super) fn early_stop(
        stopped_granules: usize,
        stopped_bytes: u64,
        fetched_granules: usize,
        fetched_bytes: u64,
        bound_exceeded: usize,
    ) {
        STOPPED_GRANULES.fetch_add(stopped_granules, Relaxed);
        STOPPED_BYTES.fetch_add(stopped_bytes, Relaxed);
        FETCHED_GRANULES.fetch_add(fetched_granules, Relaxed);
        FETCHED_BYTES.fetch_add(fetched_bytes, Relaxed);
        BOUND_EXCEEDED.fetch_add(bound_exceeded, Relaxed);
    }

    /// **A poll that downloaded granules and then delivered no flashes at
    /// all** — the residual this landing does *not* cut, counted so the next
    /// one has a figure to move.
    ///
    /// One ceiling of [`super::MAX_RETAINED_FLASHES`] serves every pane, and
    /// eviction is oldest-first across the whole store. A pane whose playhead
    /// sits behind its neighbours' therefore carries in granules **newer than
    /// its own horizon**, which fill the ceiling by themselves, and the
    /// retention floor then refuses every granule that poll just downloaded:
    /// the poll delivers zero rows for a window that has lightning in it.
    /// Measured on the handed-over HEAVY6 leg as `0 flashes held over 1500s of
    /// residency in 5 range(s)` from a poll that had installed 18 granules.
    /// That is a capacity fact about one shared ceiling, not a race, and the
    /// gate above neither causes nor cures it.
    pub(super) fn empty_delivery(rows_downloaded: usize) {
        EMPTY_DELIVERIES.fetch_add(1, Relaxed);
        EMPTY_DELIVERY_ROWS.fetch_add(rows_downloaded, Relaxed);
    }

    /// One poll's figures, folded in. The two peaks are `fetch_max`, the trim
    /// counts are sums.
    pub(super) fn record(
        peak_rows: usize,
        unstreamed_peak_rows: usize,
        trim: &super::Eviction,
        refused_granules: usize,
        refused_rows: usize,
    ) {
        POLLS.fetch_add(1, Relaxed);
        PEAK_ROWS.fetch_max(peak_rows, Relaxed);
        UNSTREAMED_PEAK_ROWS.fetch_max(unstreamed_peak_rows, Relaxed);
        TRIM_GRANULES.fetch_add(trim.granules, Relaxed);
        TRIM_ROWS.fetch_add(trim.rows, Relaxed);
        TRIM_SOLE_ROWS.fetch_add(trim.sole_rows, Relaxed);
        REFUSED_GRANULES.fetch_add(refused_granules, Relaxed);
        REFUSED_ROWS.fetch_add(refused_rows, Relaxed);
    }

    /// `(polls, peak_rows, unstreamed_peak_rows, trim_granules, trim_rows,
    /// trim_sole_rows, refused_granules, refused_rows, rounds_queued,
    /// rounds_discarded_stale, keys_planned, keys_already_held,
    /// empty_deliveries, empty_delivery_rows, stopped_granules,
    /// stopped_bytes, fetched_granules, fetched_bytes, planned_bytes,
    /// bound_exceeded)`.
    ///
    /// Appended to rather than reshaped: the readers index it positionally, and
    /// a row's position is what a published figure was read at.
    #[allow(clippy::type_complexity)]
    pub fn read() -> (
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        u64,
        usize,
        u64,
        u64,
        usize,
    ) {
        (
            POLLS.load(Relaxed),
            PEAK_ROWS.load(Relaxed),
            UNSTREAMED_PEAK_ROWS.load(Relaxed),
            TRIM_GRANULES.load(Relaxed),
            TRIM_ROWS.load(Relaxed),
            TRIM_SOLE_ROWS.load(Relaxed),
            REFUSED_GRANULES.load(Relaxed),
            REFUSED_ROWS.load(Relaxed),
            ROUNDS_QUEUED.load(Relaxed),
            ROUNDS_DISCARDED_STALE.load(Relaxed),
            KEYS_PLANNED.load(Relaxed),
            KEYS_ALREADY_HELD.load(Relaxed),
            EMPTY_DELIVERIES.load(Relaxed),
            EMPTY_DELIVERY_ROWS.load(Relaxed),
            STOPPED_GRANULES.load(Relaxed),
            STOPPED_BYTES.load(Relaxed),
            FETCHED_GRANULES.load(Relaxed),
            FETCHED_BYTES.load(Relaxed),
            PLANNED_BYTES.load(Relaxed),
            BOUND_EXCEEDED.load(Relaxed),
        )
    }
}

/// **The retention floor as it stands right now**, shared between
/// [`GranuleSink::install`], which is the only thing that raises it, and the
/// in-flight downloads of [`download_and_parse_batch`], which read it before
/// they ask for a byte.
///
/// **One authority and not two.** `install` applies its refusal test to *this*
/// value rather than to its own [`Eviction::floor`] copy, so a floor the sink
/// stopped publishing is a floor `install` stops enforcing — which
/// `the_streamed_trim_keeps_what_one_end_of_poll_trim_would_have_kept` fails on
/// over all 40,320 permutations of its fixture. A second copy read only by the
/// downloads could have drifted silently.
///
/// An `Arc<Mutex<_>>` rather than a `Rc<RefCell<_>>` because the poll's future
/// is spawned and must stay `Send`; the lock is taken once per granule, on the
/// fetch task, between network awaits.
#[derive(Clone, Default)]
struct FloorCell(std::sync::Arc<std::sync::Mutex<Option<(NaiveDateTime, String)>>>);

impl FloorCell {
    fn publish(&self, floor: Option<(NaiveDateTime, String)>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = floor;
    }

    /// **The floor test, spelled once and applied to two arguments.**
    ///
    /// [`GranuleSink::install`] passes the granule's own [`granule_newest`];
    /// the early stop passes [`granule_bound_of`]'s upper bound on that same
    /// quantity for the same key. The bound is never below `newest` and the key
    /// half of the tuple is identical, so `(newest, key) <= (bound, key)`
    /// lexicographically and a `true` on the bound implies a `true` on
    /// `newest`: **every granule stopped early is one `install` would have
    /// refused.** The converse does not hold, and does not need to — a granule
    /// the bound is too generous for is downloaded and then refused by this
    /// same test, exactly as it was before.
    fn refuses(&self, newest: NaiveDateTime, key: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|(floor_newest, floor_key)| {
                (newest, key) <= (*floor_newest, floor_key.as_str())
            })
    }
}

/// **Where a granule's rows go the instant they parse.**
///
/// They used to go into `PollAccumulator::entries`, a `Vec` that held every
/// granule of every satellite until the poll's last download returned, at
/// which point the whole set was inserted and *then* trimmed to
/// [`MAX_RETAINED_FLASHES`]. So the ceiling bounded what a poll **left
/// behind** and never what it **held**: the transient peak was the residency's
/// entire download, and a residency is a loop's whole span rather than one
/// window. Streaming the install moves the ceiling onto the peak, and the
/// retained set at the end is unchanged — eviction is oldest-first and the
/// total only ever grows, so the survivors are the newest suffix within the
/// cap whether the trim runs once at the end or after every insert.
///
/// **The counters are not diagnostics.** `peak_rows` against
/// `installed_rows + carried_rows` is this cut's own before/after, read on the
/// same poll of the same leg, which is the only way to price it: this app's
/// `live_peak` has a 703 MiB within-night spread on identical code.
struct GranuleSink<'a> {
    cache: &'a mut GlmCache,
    /// Dates a granule whose key will not parse — see [`granule_start_of`].
    as_of: NaiveDateTime,
    /// `Some` exactly when the posture test in [`fetch_glm_flashes`] says the
    /// ceiling applies. `None` is a live pane, whose window bounds it instead.
    cap: Option<usize>,
    /// Rows the cache carried into this poll, after `evict_before`.
    carried_rows: usize,
    /// Rows this poll installed, summed over every granule — including the
    /// ones the cap dropped again. `carried_rows + installed_rows` is what the
    /// cache would have held at the old end-of-poll trim.
    installed_rows: usize,
    installed_granules: usize,
    /// High-water of the cache's own level across the poll.
    peak_rows: usize,
    /// **The fires-counter**: what the in-poll trim did, summed. Its `floor`
    /// is also read back by [`Self::install`] — the trim's own refusal is what
    /// tells a later arrival it is too old to admit.
    evicted: Eviction,
    /// Granules the floor refused before they reached the cache — rows the
    /// end-of-poll trim would have admitted and then evicted, and which this
    /// shape never inserts at all.
    refused_granules: usize,
    refused_rows: usize,
    /// The live floor every in-flight download reads — see [`FloorCell`].
    floor: FloorCell,
    /// **The early stop's fires-counter**: granules whose GET was never issued
    /// because the floor had already risen above the newest flash their key can
    /// carry, and the object bytes that GET would have put on the wire and into
    /// a body buffer.
    stopped_granules: usize,
    stopped_bytes: u64,
    /// Bodies this poll did download, so [`Self::stopped_bytes`] has an
    /// in-poll denominator instead of a cross-leg one.
    fetched_granules: usize,
    fetched_bytes: u64,
    /// Granules whose parsed newest flash sat **later** than
    /// [`granule_bound_of`]'s bound on it. The witness for the early stop's
    /// soundness, and zero is the whole claim: a nonzero reading is the bound
    /// failing to bound, which is the early stop refusing a granule `install`
    /// would have kept.
    bound_exceeded: usize,
}

impl<'a> GranuleSink<'a> {
    fn new(cache: &'a mut GlmCache, as_of: NaiveDateTime, cap: Option<usize>) -> Self {
        let carried_rows = cache.retained_flashes();
        GranuleSink {
            cache,
            as_of,
            cap,
            carried_rows,
            installed_rows: 0,
            installed_granules: 0,
            peak_rows: carried_rows,
            evicted: Eviction::default(),
            refused_granules: 0,
            refused_rows: 0,
            floor: FloorCell::default(),
            stopped_granules: 0,
            stopped_bytes: 0,
            fetched_granules: 0,
            fetched_bytes: 0,
            bound_exceeded: 0,
        }
    }

    /// A download the early stop never issued — see [`FloorCell::refuses`].
    fn stopped(&mut self, object_bytes: u64) {
        self.stopped_granules += 1;
        self.stopped_bytes += object_bytes;
    }

    /// A body that did arrive, counted where its length is still known: the
    /// parse takes the `Vec` and nothing downstream can ask how long it was.
    fn fetched(&mut self, body_bytes: usize) {
        self.fetched_granules += 1;
        self.fetched_bytes += body_bytes as u64;
    }

    /// **A granule below the retention floor is not admitted at all.**
    ///
    /// Without that test the streamed trim is not the end-of-poll trim: a
    /// granule that arrives *after* the trim has already refused a newer one
    /// fits under the ceiling by itself and is kept, leaving a **hole** in the
    /// retained history — measured on a fixture whose arrivals ran
    /// newest-first, where the end-of-poll trim kept slots 5–7 and the streamed
    /// one kept 3, 5, 6, 7. More rows, and worse: each frame of a loop draws
    /// its own window, so a gap in the middle is one frame blank between two
    /// that are lit, which for a nowcasting signal reads as "no lightning
    /// then" rather than as "not retained".
    ///
    /// With it, the retained set is the newest suffix within the ceiling in
    /// every arrival order — the same set, granule for granule, that one
    /// end-of-poll trim leaves. That is pinned exhaustively over every
    /// permutation of a fixture by
    /// `the_streamed_trim_keeps_what_one_end_of_poll_trim_would_have_kept`.
    fn install(&mut self, key: String, flashes: Vec<GlmFlash>) {
        let granule_start = granule_start_of(&key, self.as_of);
        let newest = granule_newest(granule_start, &flashes);
        // The early stop's substitute quantity, checked against the real one on
        // every granule that gets this far. See [`granule_bound_of`].
        if granule_bound_of(&key).is_some_and(|bound| newest > bound) {
            self.bound_exceeded += 1;
        }
        if self.floor.refuses(newest, &key) {
            self.refused_granules += 1;
            self.refused_rows += flashes.len();
            return;
        }
        self.installed_rows += flashes.len();
        self.installed_granules += 1;
        self.cache.insert(key, granule_start, flashes);
        self.peak_rows = self.peak_rows.max(self.cache.retained_flashes());
        if let Some(cap) = self.cap {
            let evicted = self.cache.evict_oldest_over(cap);
            self.evicted.absorb(evicted);
            // The downloads still in flight read this, not a copy of it.
            self.floor.publish(self.evicted.floor.clone());
        }
    }

    /// **The last trim, through the same counters as every other one, and the
    /// poll's figures on the way out.**
    ///
    /// The trim here is the one the poll ran before the sink existed, kept
    /// because a poll that installs nothing still has to bound a cache an
    /// earlier poll left over the ceiling under a different posture. Counting
    /// it here rather than outside is what makes [`gauge`]'s trim rows the
    /// **whole** trim: a counter that saw only the in-stream half would read
    /// zero on exactly the shape that regressed the streaming away, and a
    /// mechanism whose counter cannot report its own absence is the defect the
    /// counter exists to catch.
    fn finish(self) -> PollLevels {
        // Refused rows belong in the control: the old shape parked every
        // downloaded granule and trimmed once, so a granule this floor turned
        // away is a granule that used to be resident until the poll ended.
        let unstreamed_peak_rows = self.carried_rows + self.installed_rows + self.refused_rows;
        let GranuleSink {
            cache,
            cap,
            installed_granules,
            installed_rows,
            peak_rows,
            mut evicted,
            refused_granules,
            refused_rows,
            stopped_granules,
            stopped_bytes,
            fetched_granules,
            fetched_bytes,
            bound_exceeded,
            ..
        } = self;
        if let Some(cap) = cap {
            evicted.absorb(cache.evict_oldest_over(cap));
        }
        PollLevels {
            installed_granules,
            downloaded_rows: installed_rows + refused_rows,
            peak_rows,
            unstreamed_peak_rows,
            evicted,
            refused_granules,
            refused_rows,
            stopped_granules,
            stopped_bytes,
            fetched_granules,
            fetched_bytes,
            bound_exceeded,
        }
    }
}

/// One poll's own figures, read out of the sink as it closes — see [`gauge`]
/// for what each one's denominator is.
struct PollLevels {
    installed_granules: usize,
    /// Rows this poll took off the wire and parsed — installed plus refused.
    /// The denominator for [`gauge::empty_delivery`]: a poll that delivers
    /// nothing having downloaded this many is the starvation, and one that
    /// downloaded nothing is simply a quiet sky.
    downloaded_rows: usize,
    peak_rows: usize,
    /// What the poll would have peaked at with the trim only at the end: every
    /// row it carried in plus every row it installed, none of them freed until
    /// the last download returned. **This poll's own control for its own
    /// peak**, which is the only kind this app can price a memory cut with.
    ///
    /// **Its denominator moved when the early stop landed, and it is a
    /// different figure now.** It is a control over the granules the poll
    /// *fetched*, and the early stop is a cut in exactly that set: rows it
    /// stops are absent from both sides of the ratio, because a granule whose
    /// GET was never issued has no row count to add to either. So the ratio
    /// this reports fell — from 12.501× on the poll that installed 3.2 M rows —
    /// and nothing about the streamed install regressed; the poll simply no
    /// longer downloads the rows the trim used to throw away. What prices the
    /// early stop is [`gauge::early_stop`]'s bytes, not this.
    unstreamed_peak_rows: usize,
    evicted: Eviction,
    refused_granules: usize,
    refused_rows: usize,
    /// The early stop's own figures — see [`GranuleSink::stopped_granules`].
    stopped_granules: usize,
    stopped_bytes: u64,
    fetched_granules: usize,
    fetched_bytes: u64,
    bound_exceeded: usize,
}

/// The instant a granule is aged against, from the S3 key it was listed under;
/// the depicted instant on the fallback, so an undatable granule expires one
/// window out of the picture that asked for it.
fn granule_start_of(key: &str, as_of: NaiveDateTime) -> NaiveDateTime {
    parse_filename_start_time(key).unwrap_or(as_of)
}

/// **One poll of the lightning archive, for what `depicted` says this pane
/// must be holding.**
///
/// `depicted` is [`squallar_source::handler::SourceHandler::residency_for`]'s
/// own answer — one `time_window_secs` slice behind every instant the pane's
/// clock can stop on, coalesced. It replaced a `start`/`cutoff`/`horizon`
/// triple derived here from `(as_of, span, frames)`, which is the shape three
/// consecutive GLM bugs lived in: the layer's window was subtracted **here**
/// while the caller measured the span **there**, so two authorities described
/// one loop and a twelve-hour sweep was lit on a single frame.
///
/// **The window is subtracted exactly once, and not in this function.** Every
/// bound below is read off `depicted`; `time_window_secs` survives only for
/// the publish-latency assertion and the log line.
///
/// `as_of` still travels beside it, and only for what a residency cannot say:
/// it dates a granule whose key will not parse ([`granule_start_of`]) and it
/// is the fallback bound for a residency asking for nothing.
pub async fn fetch_glm_flashes(
    client: &reqwest::Client,
    sources: &DataSources,
    satellites: &[GlmSatellite],
    levels: &[GlmDataLevel],
    cache: &mut GlmCache,
    as_of: NaiveDateTime,
    depicted: Residency,
) -> Result<GlmFetchOutcome, FetchError> {
    // The zero-object warning below assumes every queried range is wide enough
    // to always cover an already-published granule.
    //
    // **Read off the residency rather than off a `time_window_secs` argument**,
    // which is what the layer's window used to be handed down here as: the
    // ranges *are* the window, one per stop and coalesced, so a caller that
    // built them any other way is caught by the same claim.
    debug_assert!(
        depicted
            .ranges()
            .iter()
            .all(|range| range.duration().num_milliseconds() as f64 / 1000.0
                >= GLM_MIN_TIME_WINDOW_SECS),
        "a GLM poll was asked over a range narrower than \
         GLM_MIN_TIME_WINDOW_SECS ({GLM_MIN_TIME_WINDOW_SECS}s): {:?}. A window \
         under S3 publish latency makes the zero-object check report a live \
         feed as dead.",
        depicted.ranges(),
    );

    // **The objects, from the flashes** — see [`listed_ranges`]. These decide
    // what is listed and which listed keys are worth downloading; on a live
    // pane there is one range and it is `[as_of - window, as_of]`, so the
    // request is byte-for-byte the one it always was.
    let listed = listed_ranges(&depicted);
    // `list_glm_files` is addressed by `{year}/{doy}/{hour}`, so these are the
    // archive hours asked for. The upper bound is the residency's own newest
    // instant and **not the sampled one**: a poll landing while the playhead
    // sits on the loop's oldest frame would otherwise cover a range entirely
    // behind the loop, and retention cannot rescue what was never fetched.
    //
    // `None` is a residency asking for nothing, which the handler above cannot
    // produce — it always names `as_of` among its stops. A round that asks to
    // hold nothing asks the archive for nothing, so the degenerate range is
    // the sampled instant itself rather than an invented window around it.
    let (start, horizon) = listed
        .first()
        .zip(listed.last())
        .map_or((as_of, as_of), |(first, last)| (first.0, last.1));
    // **Un-widened, and that is the rule rather than an oversight.** Residency
    // states which *flashes* must be held; [`GRANULE_SPAN`] is a statement
    // about which S3 *objects* have to be asked for. A cutoff carrying it
    // would retain 40 s of archive nothing depicts, on every window — and an
    // instant-anchored one would evict exactly the granules the pane's other
    // frames display.
    let cutoff = depicted.extent().map_or(start, |(oldest, _)| oldest);

    cache.evict_before(cutoff);

    // **The byte bound on span retention, read once before a byte is
    // downloaded** — because the trim it gates now runs after every install
    // rather than after the last one, which is what puts the ceiling on what a
    // poll HOLDS and not only on what it leaves behind. A live pane's cache is
    // bounded by its window exactly as it always was; a span could otherwise
    // hold a day of storm at Event level. Oldest first, so an overflowing loop
    // keeps its newest hours lit.
    //
    // The posture test is the residency's own shape, unchanged and in the same
    // place in the poll's order: a live pane asks for one range and nothing
    // more, while a span or a loop asks for several — or for one much wider
    // than a window, which is the span posture coalesced.
    let cap = (depicted.ranges().len() > 1 || depicted.total() > longest_single_window())
        .then_some(MAX_RETAINED_FLASHES);
    let mut sink = GranuleSink::new(cache, as_of, cap);

    let mut acc = PollAccumulator::default();
    let mut dead_feeds = Vec::new();
    let mut window_gaps = Vec::new();
    let mut tally = PollTally::default();

    let mut listing_failures: Vec<(GlmSatellite, FetchError)> = Vec::new();
    let mut queried = Vec::new();

    for &sat in satellites {
        let bucket = sat.bucket(sources);
        let listing = match list_glm_files(client, sources, bucket, start, horizon).await {
            Ok(listing) => listing,
            Err(e) => {
                log::warn!("GLM: {} listing failed: {e}", sat.display_name());
                listing_failures.push((sat, e));
                continue;
            }
        };
        queried.push(sat);

        // Zero objects means the feed is gone (dead bucket, renamed path,
        // satellite rotated out of the slot), not a quiet sky. Objects present
        // with no in-window *keys* is a third case; `else if` because zero
        // objects can only produce zero keys.
        if listing.objects_seen == 0 {
            dead_feeds.push(DeadFeed {
                satellite: sat,
                bucket: bucket.to_string(),
                prefixes: listing.prefixes.clone(),
            });
        } else if listing.keys.is_empty() {
            window_gaps.push(WindowGap {
                satellite: sat,
                objects_seen: listing.objects_seen,
            });
        }

        let new_keys = plan_downloads(&listing.keys, sink.cache, &listed, &mut tally);

        if new_keys.is_empty() {
            continue;
        }
        log::info!(
            "Downloading {} new GLM files from {}",
            new_keys.len(),
            sat.display_name()
        );

        let batch =
            download_and_parse_batch(client, sources, sat, bucket, &new_keys, levels, &mut sink)
                .await;
        acc.absorb(sat, levels, batch);
    }

    if queried.is_empty() && !satellites.is_empty() {
        let verdicts: Vec<FetchError> = listing_failures
            .iter()
            .map(|(_, e)| e.clone())
            .collect::<Vec<_>>();
        return Err(FetchError::of_round(
            &verdicts,
            format!(
                "no GLM satellite could be listed ({} failed)",
                verdicts.len()
            ),
        ));
    }

    // **The counters, always on and off the frame thread.** `peak` is the
    // cache's own high-water across the poll; `unstreamed` is what it would
    // have been with the trim only at the end, which is the same poll's own
    // control. `sole` is the half of the eviction that returned bytes to the
    // allocator: a poll works on a `GlmStore::snapshot`, so a carried-in
    // granule it drops is a refcount and not a free.
    let PollLevels {
        installed_granules,
        downloaded_rows,
        peak_rows,
        unstreamed_peak_rows,
        evicted,
        refused_granules,
        refused_rows,
        stopped_granules,
        stopped_bytes,
        fetched_granules,
        fetched_bytes,
        bound_exceeded,
    } = sink.finish();
    gauge::record(
        peak_rows,
        unstreamed_peak_rows,
        &evicted,
        refused_granules,
        refused_rows,
    );
    gauge::planned(tally.planned, tally.already_held, tally.planned_bytes);
    gauge::early_stop(
        stopped_granules,
        stopped_bytes,
        fetched_granules,
        fetched_bytes,
        bound_exceeded,
    );
    log::info!(
        "GLM poll: {installed_granules} granules installed, peak {peak_rows} rows \
         ({} B), unstreamed peak would be {unstreamed_peak_rows} rows ({} B); \
         in-poll trim dropped {} granules / {} rows, {} of them sole ({} B \
         freed); the floor refused {refused_granules} granules / \
         {refused_rows} rows before they were cached; the plan asked for {} \
         keys and skipped {} the store already held; the early stop refused \
         {stopped_granules} granules / {stopped_bytes} B of object before a \
         GET against {fetched_granules} fetched / {fetched_bytes} B, of {} B \
         planned, and {bound_exceeded} parsed granules ran past their key's \
         bound",
        peak_rows * FLASH_BYTES,
        unstreamed_peak_rows * FLASH_BYTES,
        evicted.granules,
        evicted.rows,
        evicted.sole_rows,
        evicted.sole_rows * FLASH_BYTES,
        tally.planned,
        tally.already_held,
        tally.planned_bytes,
    );

    // Still keyed on `satellites`, not `queried`: a satellite whose listing
    // failed still has earlier granules in window. The upper bound is the
    // span's `horizon`, not the sampled instant: the raster culls per depicted
    // frame, so returning the whole retained span is what lets every frame of
    // a loop draw its own window from one delivery.
    let filtered = flashes_in_window(cache, satellites, cutoff, horizon);

    // **A poll that downloaded rows and delivers none.** Not this landing's
    // cut and not a race: one ceiling serves every pane, so a pane whose
    // playhead sits behind its neighbours' carries in granules newer than its
    // own horizon, and the floor then refuses everything it just downloaded.
    // Counted where the delivery is built, which is the only place both halves
    // of the figure exist at once.
    if filtered.is_empty() && downloaded_rows > 0 {
        gauge::empty_delivery(downloaded_rows);
        log::info!(
            "GLM: this poll downloaded {downloaded_rows} rows and delivered \
             none - every granule it fetched was older than the retention \
             floor the panes ahead of it had already raised",
        );
    }

    // **The figure names its denominator**: this is the retained set over the
    // whole residency, not over one frame's window. The rasterizer culls each
    // depicted frame to its own window from the same delivery.
    log::info!(
        "GLM: {} flashes held over {:.0}s of residency in {} range(s)",
        filtered.len(),
        depicted.total().num_milliseconds() as f64 / 1000.0,
        depicted.ranges().len(),
    );

    Ok(build_outcome(
        filtered,
        dead_feeds,
        window_gaps,
        queried,
        listing_failures,
        &tally,
        acc,
    ))
}

/// Select the cached flashes inside this poll's retention window, from the
/// satellites it was asked for — the per-flash half of a two-stage narrowing
/// ([`GlmCache::evict_before`] does the per-granule half). Both bounds are
/// inclusive: the bounds are sampled once per poll and a granule's last
/// flashes can be stamped after its start.
///
/// `horizon` is `as_of` itself on a live pane; under a depicted span it is
/// `as_of + span`, so frames *ahead* of the sampled instant keep their
/// flashes. **Culling to each frame's own window is the rasterizer's job**
/// (`rasterize_glm_strikes` drops a flash later than its depicted instant, and
/// one older than the fade window) — this set is what any frame of the span
/// may draw from, not what one frame shows.
fn flashes_in_window(
    cache: &GlmCache,
    satellites: &[GlmSatellite],
    cutoff: NaiveDateTime,
    horizon: NaiveDateTime,
) -> Vec<GlmFlash> {
    // **Presized off the level, not grown into.** A `Filter` reports a lower
    // size hint of zero, so `collect` started this Vec at four rows and
    // doubled: the capacity it settled on was the next power of two above the
    // row count, and it is not a transient — it travels into
    // `GlmFetchOutcome::flashes` and on into the render handler's
    // `Arc<GlmSlab>`, so up to 2× the rows stayed held for as long as the
    // delivery was. At 420,000 rows that was 25,165,824 B of capacity carrying
    // 20,160,000 B of rows.
    //
    // `retained_flashes` is an exact upper bound and a free read — the
    // maintained level, not a walk: every row this filter can keep is a row the
    // cache holds. What it over-reserves by is what the filter drops: the
    // granule straddling the window's end, and — for the window it takes those
    // granules to age out of `evict_before` — a satellite just deselected. That
    // worst case is under 2× the rows kept, which is what doubling settled on
    // anyway, so the reservation is never dearer than the growth it replaced
    // and is exact on a steady pane.
    let mut in_window = Vec::with_capacity(cache.retained_flashes());
    in_window.extend(
        cache
            .all_flashes()
            .filter(|f| satellites.contains(&f.satellite) && f.time >= cutoff && f.time <= horizon)
            .cloned(),
    );
    in_window
}

fn build_outcome(
    flashes: Vec<GlmFlash>,
    dead_feeds: Vec<DeadFeed>,
    window_gaps: Vec<WindowGap>,
    queried: Vec<GlmSatellite>,
    listing_failures: Vec<(GlmSatellite, FetchError)>,
    tally: &PollTally,
    acc: PollAccumulator,
) -> GlmFetchOutcome {
    GlmFetchOutcome {
        flashes,
        dead_feeds,
        window_gaps,
        record_drops: acc.drops,
        queried,
        listing_failures,
        parse_failures: summarize_failures(tally.in_window, acc.parse_errors),
        transport_failures: summarize_failures(tally.in_window, acc.transport_errors),
        // Not routed through `summarize_failures`: a level failure has no
        // file-count denominator.
        level_failures: acc.level_failures,
        evaluated_levels: acc.evaluated_levels,
    }
}

#[derive(Default)]
struct PollAccumulator {
    parse_errors: Vec<String>,
    transport_errors: Vec<String>,
    level_failures: Vec<LevelFailure>,
    /// (satellite, level) pairs this poll gathered evidence about: a poll that
    /// downloads nothing new learns nothing.
    evaluated_levels: Vec<(GlmSatellite, GlmDataLevel)>,
    /// Summed across both satellites: the drop counts share one denominator.
    drops: RecordDrops,
}

impl PollAccumulator {
    fn absorb(&mut self, satellite: GlmSatellite, levels: &[GlmDataLevel], batch: BatchOutcome) {
        self.parse_errors.extend(batch.parse_errors);
        self.transport_errors.extend(batch.transport_errors);
        self.level_failures.extend(batch.level_failures);
        self.drops.absorb(batch.drops);

        if batch.installed > 0 {
            for &level in levels {
                self.evaluated_levels.push((satellite, level));
            }
        }
    }
}

#[derive(Default)]
struct PollTally {
    in_window: usize,
    /// Keys this poll planned a GET for, and keys it skipped because the cache
    /// already held that granule — [`gauge::planned`]'s two halves. Both are
    /// counted over keys the residency covers, so they share one denominator
    /// and `planned + already_held` is the covered set.
    planned: usize,
    already_held: usize,
    /// The object bytes those planned keys named — the denominator
    /// `stopped_bytes` is a fraction of, summed in the same poll that stopped
    /// them so no cross-leg pairing is involved.
    planned_bytes: u64,
}

/// The tally counts every listed key, not the returned ones: a download-count
/// denominator makes one persistent failure look like a total outage.
fn plan_downloads<'a>(
    keys: &'a [ListedObject],
    cache: &GlmCache,
    listed: &[(NaiveDateTime, NaiveDateTime)],
    tally: &mut PollTally,
) -> Vec<&'a ListedObject> {
    tally.in_window += keys.len();
    let mut planned: Vec<&'a ListedObject> = keys
        .iter()
        .filter(|o| covers(listed, o.key.as_str()))
        .filter(|o| !cache.contains_key(o.key.as_str()))
        .collect();
    let covered = keys
        .iter()
        .filter(|o| covers(listed, o.key.as_str()))
        .count();
    tally.planned += planned.len();
    tally.already_held += covered - planned.len();
    tally.planned_bytes += planned.iter().map(|o| o.bytes).sum::<u64>();
    // **Newest first, and that ordering is what makes the early stop worth
    // having.** The retention floor only ever rises, and it rises when the
    // ceiling refuses a granule; a batch taken oldest-first fills the ceiling
    // with rows the newest arrivals then evict, so the floor arrives late and
    // the downloads it would have stopped have already been paid for. Taken
    // newest-first the ceiling is full after its first ~16 granules and the
    // floor stands above every remaining key in the batch.
    //
    // **It cannot change what the poll retains.** The retained set is the
    // newest suffix within the ceiling in *every* arrival order, pinned over
    // all 40,320 permutations of an 8-granule fixture by
    // `the_streamed_trim_keeps_what_one_end_of_poll_trim_would_have_kept`. This
    // reorders which of them are asked for first, not which survive.
    //
    // Descending on [`granule_bound_of`] and then on the key, which is
    // [`GlmCache::evict_oldest_over`]'s own sort reversed; `None` bounds sort
    // last and are never stopped.
    planned.sort_unstable_by(|a, b| {
        granule_bound_of(&b.key)
            .cmp(&granule_bound_of(&a.key))
            .then_with(|| b.key.cmp(&a.key))
    });
    planned
}

/// How much of a granule's content can start before the key's own timestamp
/// does — GLM publishes a granule every 20 s, and the key names the start of
/// the 20 s it covers. Doubled, so a listing that skips one publication (the
/// 40 s gap seen 4 times in 4289 measured granules) still keeps the granule
/// whose content reaches into a window's opening.
const GRANULE_SPAN: TimeDelta = TimeDelta::seconds(40);

/// The widest window a **live** pane can ask over — one range at the layer's
/// own maximum setting. A residency totalling more than this cannot be a live
/// round however it was built, which is what makes it the byte-cap's posture
/// test rather than a second reading of `depicted_span_secs`.
fn longest_single_window() -> TimeDelta {
    TimeDelta::milliseconds((GLM_MAX_TIME_WINDOW_SECS * 1000.0) as i64)
}

/// **The S3 objects a residency has to be asked for**: each range it names,
/// reached back by [`GRANULE_SPAN`].
///
/// **The widening is the caller's, deliberately.** `residency_for` answers
/// which *flashes* a set of stops obliges this layer to hold; that a granule
/// straddling a range's opening carries some of them is a fact about how the
/// bucket is cut, not about the picture. So the listing and the download
/// filter apply it here and the eviction cutoff does not.
///
/// Ascending and disjoint in, ascending in out — `Residency` coalesces, and
/// widening every range by the same amount cannot reorder them. Two widened
/// ranges may overlap where their unwidened originals merely sat 40 s apart;
/// [`covers`] is a union test, so that changes nothing about which keys pass.
fn listed_ranges(depicted: &Residency) -> Vec<(NaiveDateTime, NaiveDateTime)> {
    depicted
        .ranges()
        .iter()
        .map(|range| (range.start - GRANULE_SPAN, range.end))
        .collect()
}

/// Whether `key`'s granule falls in any listed range — the download filter,
/// and the whole of what keeps a twelve-hour loop costing its thirteen windows
/// instead of its twelve hours.
///
/// The test is on the granule's own **start**, which is all a key carries. A
/// key whose start cannot be read passes: an undatable granule is not evidence
/// that nothing is depicted there, and the flash filter downstream is what
/// decides its rows. An empty set is a residency asking for nothing, and
/// refusing every key on it would make a round that listed the archive
/// download none of it.
fn covers(listed: &[(NaiveDateTime, NaiveDateTime)], key: &str) -> bool {
    if listed.is_empty() {
        return true;
    }
    let Some(start) = parse_filename_start_time(key) else {
        return true;
    };
    listed
        .iter()
        .any(|&(from, to)| start >= from && start <= to)
}

/// **One object the listing named, with the size it declared.**
///
/// The size is carried because the early stop's whole figure is a download it
/// did not make, and a GET that never happens can only be priced by what the
/// listing said the object weighed. An average over the ones that *were*
/// fetched would be a scaled figure, which is not a measurement.
struct ListedObject {
    key: String,
    /// The object's own `Size` element. `0` when the listing named none, so the
    /// counter under-reports rather than invents — the same direction
    /// [`granule_bound_of`] fails in.
    bytes: u64,
}

struct GlmListing {
    keys: Vec<ListedObject>,
    objects_seen: usize,
    prefixes: Vec<String>,
}

/// The `Size` beside a `Key` in the same `Contents` element.
fn object_size(key_node: roxmltree::Node<'_, '_>) -> u64 {
    key_node
        .parent()
        .into_iter()
        .flat_map(|contents| contents.children())
        .find(|n| n.tag_name().name() == "Size")
        .and_then(|n| n.text())
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0)
}

/// S3 path: `GLM-L2-LCFA/{year}/{day_of_year}/{hour}/`
/// Files: `OR_GLM-L2-LCFA_G{sat}_s{start}_e{end}_c{creation}.nc`
async fn list_glm_files(
    client: &reqwest::Client,
    sources: &DataSources,
    bucket: &str,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<GlmListing, FetchError> {
    let mut all_keys = Vec::new();
    let mut objects_seen = 0usize;

    // A *single* prefix when `start` and `end` share a UTC hour, which requires
    // `GLM_MIN_TIME_WINDOW_SECS` > S3 publish latency for the hour's first
    // object: that granule lands 27–30 s after the boundary (measured on
    // noaa-goes19), so a 60 s minimum leaves ~30 s of headroom.
    let mut prefixes = Vec::new();
    let mut t = start;
    loop {
        let year = t.format("%Y").to_string();
        let doy = t.format("%j").to_string();
        let hour = t.format("%H").to_string();
        let prefix = format!("GLM-L2-LCFA/{year}/{doy}/{hour}/");
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
        if t >= end {
            break;
        }
        t += TimeDelta::hours(1);
        if t > end {
            t = end;
        }
    }

    for prefix in &prefixes {
        let mut continuation_token: Option<String> = None;
        loop {
            let mut url = format!(
                "{}/?list-type=2&prefix={prefix}",
                sources.s3_bucket_url(bucket),
            );
            if let Some(ref token) = continuation_token {
                url.push_str("&continuation-token=");
                url.push_str(&urlencoded(token));
            }

            let resp = client.get(&url).send().await.map_err(|e| {
                FetchError::from_transport(&e, format!("S3 list request failed: {e}"))
            })?;

            if !resp.status().is_success() {
                // `IsBroken`: a bucket listing is not published on a schedule.
                // A 404 on `?list-type=2` means the bucket is gone or renamed.
                return Err(FetchError::from_status(
                    resp.status(),
                    NotFound::IsBroken,
                    format!("S3 returned HTTP {}", resp.status()),
                ));
            }

            let body = squallar_source::http::body_to_string(resp)
                .await
                .map_err(|e| {
                    FetchError::from_transport(&e, format!("Failed to read S3 list response: {e}"))
                })?;

            let doc = roxmltree::Document::parse(&body)
                .map_err(|e| FetchError::transient(format!("Failed to parse S3 XML: {e}")))?;

            for node in doc.descendants() {
                if node.tag_name().name() == "Key"
                    && let Some(key) = node.text()
                {
                    objects_seen += 1;
                    if key.ends_with(".nc")
                        && let Some(file_start) = parse_filename_start_time(key)
                        && file_start >= start
                        && file_start <= end
                    {
                        all_keys.push(ListedObject {
                            key: key.to_string(),
                            bytes: object_size(node),
                        });
                    }
                }
            }

            let is_truncated = doc
                .descendants()
                .find(|n| n.tag_name().name() == "IsTruncated")
                .and_then(|n| n.text())
                .is_some_and(|t| t == "true");

            if !is_truncated {
                break;
            }

            // A truncated response with no usable continuation token would
            // re-issue the identical first-page request forever.
            let next = doc
                .descendants()
                .find(|n| n.tag_name().name() == "NextContinuationToken")
                .and_then(|n| n.text())
                .filter(|t| !t.is_empty())
                .map(|s| s.to_string());

            let Some(next) = next else {
                log::warn!(
                    "GLM: S3 reported a truncated listing for '{prefix}' in bucket \
                     '{bucket}' but returned no continuation token; \
                     results for this prefix may be incomplete"
                );
                break;
            };

            if continuation_token.as_deref() == Some(next.as_str()) {
                log::warn!(
                    "GLM: S3 repeated the same continuation token for '{prefix}' in \
                     bucket '{bucket}'; stopping pagination to avoid a spin"
                );
                break;
            }

            continuation_token = Some(next);
        }
    }

    Ok(GlmListing {
        keys: all_keys,
        objects_seen,
        prefixes,
    })
}

/// Parse the start timestamp from a GLM filename: the `s` field of
/// `OR_GLM-L2-LCFA_G19_s20261120145200_e...nc` is `YYYYDDDHHMMSSf`, DDD = day of
/// year, f = tenths of a second.
fn parse_filename_start_time(key: &str) -> Option<NaiveDateTime> {
    parse_filename_field_time(key, "_s")
}

/// **An upper bound on a granule's newest flash, from its key alone** — the
/// substitute quantity the early stop in [`download_and_parse_batch`] applies
/// [`FloorCell::refuses`] to, because the real one ([`granule_newest`]) is a
/// fold over rows that only exist once the body has been downloaded and parsed.
///
/// The `_e` field of the key is the granule's own declared coverage end, so no
/// flash it carries is stamped later. The reader keeps whole seconds while the
/// field names tenths, so the bound is rounded up by one second: a bound a
/// tenth short of the content would refuse a granule [`GranuleSink::install`]
/// admits, and the whole soundness of the early stop is that it cannot.
///
/// `None` on a key with no datable `_e`, which stops nothing — the same
/// direction [`covers`] fails in.
///
/// **That the bound really bounds is measured, not assumed.**
/// [`GranuleSink::install`] compares every granule it parses against this
/// function's answer for its own key and counts the excesses
/// ([`PollLevels::bound_exceeded`]); a leg reporting anything but zero there is
/// reporting that the early stop can refuse a granule the floor would not.
fn granule_bound_of(key: &str) -> Option<NaiveDateTime> {
    Some(parse_filename_field_time(key, "_e")? + TimeDelta::seconds(1))
}

/// The 14-digit `YYYYDDDHHMMSSf` field named by `field` (`_s` or `_e`),
/// truncated to the second.
fn parse_filename_field_time(key: &str, field: &str) -> Option<NaiveDateTime> {
    let filename = key.rsplit('/').next()?;
    let idx = filename.find(field)?;
    let s_field = &filename[idx + field.len()..];
    // `get`, not `[..14]`: a multi-byte character in the field would put a
    // range boundary inside a UTF-8 sequence and panic.
    let digits = s_field.get(..14)?;
    let year: i32 = digits.get(0..4)?.parse().ok()?;
    let doy: u32 = digits.get(4..7)?.parse().ok()?;
    let hour: u32 = digits.get(7..9)?.parse().ok()?;
    let min: u32 = digits.get(9..11)?.parse().ok()?;
    let sec: u32 = digits.get(11..13)?.parse().ok()?;

    let date = chrono::NaiveDate::from_yo_opt(year, doy)?;
    let time = chrono::NaiveTime::from_hms_opt(hour, min, sec)?;
    Some(NaiveDateTime::new(date, time))
}

#[derive(Default)]
struct BatchOutcome {
    /// **Granules this batch installed**, not the granules themselves: the rows
    /// went straight into the cache through [`GranuleSink`] as each parse
    /// returned. A count is all any caller ever read — `absorb` asks only
    /// whether the batch learned anything about the levels it queried.
    installed: usize,
    /// One message per file that downloaded but would not parse.
    parse_errors: Vec<String>,
    /// One message per file that never arrived, tracked separately so a network
    /// problem is never reported as a product schema change.
    transport_errors: Vec<String>,
    level_failures: Vec<LevelFailure>,
    /// Summed over every granule that parsed, **not** deduplicated.
    drops: RecordDrops,
}

/// **The stream is consumed one granule at a time and each one is installed
/// before the next is taken**, which is what bounds a poll's rows by the cache's
/// own ceiling rather than by the size of the residency.
///
/// It used to `.collect()` the whole `buffer_unordered` stream into a `Vec` and
/// hand it back, so a satellite's every parsed granule was resident at once and
/// then joined the *other* satellite's in `PollAccumulator::entries`. That is
/// what made [`GRANULE_FETCH_CONCURRENCY`]'s bound on *bodies* the only bound
/// this function had: the bodies were capped at twenty and the parsed rows at
/// nothing.
async fn download_and_parse_batch(
    client: &reqwest::Client,
    sources: &DataSources,
    satellite: GlmSatellite,
    bucket: &str,
    objects: &[&ListedObject],
    levels: &[GlmDataLevel],
    sink: &mut GranuleSink<'_>,
) -> BatchOutcome {
    use futures::stream::StreamExt;

    let levels_owned: Vec<GlmDataLevel> = levels.to_vec();
    let futs: Vec<_> = objects
        .iter()
        .map(|&object| {
            let client = client.clone();
            let url = sources.s3_object_url(bucket, &object.key);
            let key_owned = object.key.clone();
            let object_bytes = object.bytes;
            let bound = granule_bound_of(&object.key);
            let floor = sink.floor.clone();
            let lvls = levels_owned.clone();
            async move {
                // **The floor as it stands at this instant, not as it stood
                // when the batch was planned.** A plan-time filter cannot work
                // here and the reason is arithmetic: the retained level sits
                // one granule *under* the ceiling when a poll begins, so the
                // sound floor at plan time is almost always "none" — measured
                // at 355 of 12,463 keys, 2.8%. The floor this reads was raised
                // by the granules of this same batch that have already
                // installed, which is why the ordering above is newest-first.
                if let Some(bound) = bound
                    && floor.refuses(bound, &key_owned)
                {
                    return Ok(Fetched::Stopped { object_bytes });
                }
                match download_and_parse_one(&client, &url, satellite, &lvls).await {
                    Ok((body_bytes, parsed)) => Ok(Fetched::Parsed {
                        key: key_owned,
                        body_bytes,
                        parsed,
                    }),
                    Err(e) => {
                        // Debug, not warn: with `GRANULE_FETCH_CONCURRENCY`
                        // files in flight one schema change would produce a
                        // wall of identical lines.
                        log::debug!("Failed to fetch GLM file {key_owned}: {}", e.message());
                        let labelled = format!("{key_owned}: {}", e.message());
                        Err(match e {
                            FileError::Parse(_) => FileError::Parse(labelled),
                            FileError::Transport(_) => FileError::Transport(labelled),
                        })
                    }
                }
            }
        })
        .collect();

    let mut stream = futures::stream::iter(futs).buffer_unordered(GRANULE_FETCH_CONCURRENCY);
    let mut outcome = BatchOutcome::default();
    while let Some(result) = stream.next().await {
        outcome.absorb_one(result, sink);
    }
    outcome
}

impl BatchOutcome {
    /// One finished download, folded in — and its rows handed to the sink
    /// rather than parked in this struct. The failure bookkeeping is byte for
    /// byte what `from_results` did; only the rows changed owner.
    fn absorb_one(&mut self, result: Result<Fetched, FileError>, sink: &mut GranuleSink<'_>) {
        match result {
            Ok(Fetched::Stopped { object_bytes }) => sink.stopped(object_bytes),
            Ok(Fetched::Parsed {
                key,
                body_bytes,
                parsed,
            }) => {
                sink.fetched(body_bytes);
                self.drops.absorb(parsed.drops);
                for failure in parsed.level_failures {
                    if !self.level_failures.iter().any(|f: &LevelFailure| {
                        f.satellite == failure.satellite && f.level == failure.level
                    }) {
                        self.level_failures.push(failure);
                    }
                }
                self.installed += 1;
                sink.install(key, parsed.records);
            }
            Err(FileError::Parse(e)) => self.parse_errors.push(e),
            Err(FileError::Transport(e)) => self.transport_errors.push(e),
        }
    }
}

#[cfg(test)]
impl BatchOutcome {
    /// **The shape [`BatchOutcome::absorb_one`] replaced**, kept for the
    /// partition arms: they ask what this function does with a mixed batch of
    /// successes and both failure kinds, and that is unchanged. The rows now
    /// need somewhere to go, so it folds through a sink over a scratch cache
    /// with no cap — the arms assert on the failure buckets and the installed
    /// count, never on the cache.
    fn from_results(results: Vec<Result<Fetched, FileError>>) -> Self {
        let mut cache = GlmCache::default();
        let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let mut sink = GranuleSink::new(&mut cache, epoch, None);
        let mut outcome = BatchOutcome::default();
        for result in results {
            outcome.absorb_one(result, &mut sink);
        }
        outcome
    }
}

/// **What one slot of the batch produced**, which is now two different things:
/// a granule that was downloaded and parsed, or one whose GET was never issued
/// because the retention floor had already risen above it.
///
/// A third `FileError` variant would have been wrong — a stop is not a failure
/// and must not reach [`BatchOutcome::transport_errors`], where it would be
/// reported to the user as a feed problem and counted against the batch.
enum Fetched {
    Parsed {
        key: String,
        /// The body's length, taken here because [`parse_downloaded_file`]
        /// consumes the `Vec` and nothing downstream can ask how long it was.
        body_bytes: usize,
        parsed: GranuleParse,
    },
    Stopped {
        object_bytes: u64,
    },
}

/// Why one file did not contribute: a file that arrives and will not parse
/// indicts the product, one that never arrives indicts the network. A captive
/// portal answering 200 with an HTML page is reported as `Parse`.
#[derive(Debug)]
enum FileError {
    Transport(String),
    Parse(String),
}

impl FileError {
    fn message(&self) -> &str {
        match self {
            FileError::Transport(e) | FileError::Parse(e) => e,
        }
    }
}

fn summarize_failures(in_window: usize, errors: Vec<String>) -> Option<FetchFailures> {
    let sample_error = errors.first()?.clone();
    Some(FetchFailures {
        in_window,
        failed: errors.len(),
        sample_error,
    })
}

async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?
        .error_for_status()
        .map_err(|e| format!("HTTP status error: {e}"))?;
    squallar_source::http::body_to_vec(response)
        .await
        .map_err(|e| format!("Failed to read body: {e}"))
}

async fn download_and_parse_one(
    client: &reqwest::Client,
    url: &str,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<(usize, GranuleParse), FileError> {
    let bytes = download_bytes(client, url)
        .await
        .map_err(FileError::Transport)?;
    let body_bytes = bytes.len();
    parse_downloaded_file(bytes, satellite, levels).map(|parsed| (body_bytes, parsed))
}

/// **Takes the body**, so the buffer the transport allocated is the only copy
/// of this granule that ever exists — see [`parse_glm_granule`].
fn parse_downloaded_file(
    bytes: Vec<u8>,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, FileError> {
    parse_glm_granule(bytes, satellite, levels).map_err(FileError::Parse)
}

struct LevelVars {
    lat: &'static str,
    lon: &'static str,
    energy: &'static str,
    area: Option<&'static str>,
    time_offset: &'static str,
    level: GlmDataLevel,
}

const FLASH_VARS: LevelVars = LevelVars {
    lat: "flash_lat",
    lon: "flash_lon",
    energy: "flash_energy",
    area: Some("flash_area"),
    time_offset: "flash_time_offset_of_first_event",
    level: GlmDataLevel::Flash,
};

const GROUP_VARS: LevelVars = LevelVars {
    lat: "group_lat",
    lon: "group_lon",
    energy: "group_energy",
    area: Some("group_area"),
    time_offset: "group_time_offset",
    level: GlmDataLevel::Group,
};

// The L2 LCFA product has no `event_area` variable — only groups and flashes
// carry area coverage (confirmed against a live `noaa-goes19` granule).
const EVENT_VARS: LevelVars = LevelVars {
    lat: "event_lat",
    lon: "event_lon",
    energy: "event_energy",
    area: None,
    time_offset: "event_time_offset",
    level: GlmDataLevel::Event,
};

pub(crate) trait VarSource {
    fn read_unpacked(&self, name: &str) -> Result<Option<cf::UnpackedVar>, String>;
    fn time_coverage_start(&self) -> Option<String>;
}

impl VarSource for squallar_netcdf::Granule {
    fn read_unpacked(&self, name: &str) -> Result<Option<cf::UnpackedVar>, String> {
        squallar_netcdf::Granule::read_unpacked(self, name)
    }
    fn time_coverage_start(&self) -> Option<String> {
        self.global_str("time_coverage_start")
    }
}

/// Parse a granule from **borrowed** bytes, copying them.
///
/// `squallar_netcdf::Granule` needs an owned buffer, so this is
/// [`parse_glm_granule`] plus a copy of the whole file. `cfg(test)` because
/// every caller is a test holding a `&'static [u8]` fixture or a builder's
/// return value — the download path owns its bytes and goes through the owned
/// door, and a shipped caller appearing here would be one paying for a copy it
/// does not need.
#[cfg(test)]
pub(crate) fn parse_glm_netcdf(
    data: &[u8],
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, String> {
    parse_glm_granule(data.to_vec(), satellite, levels)
}

/// Parse a granule from bytes this call **takes ownership of** — the download
/// path's door, and the no-copy form of [`parse_glm_netcdf`].
///
/// By value on purpose, the same argument `gmgsi::decode` makes: the reader
/// needs an owned buffer, so taking it here lets `Granule::from_vec` adopt the
/// body the transport already allocated instead of `Granule::open` copying it.
/// **What the copy cost, measured**: 280,380 B on the product granule, for the
/// length of one parse. Not one per in-flight slot —
/// [`GRANULE_FETCH_CONCURRENCY`] bounds the *bodies* in flight, but
/// `buffer_unordered` polls its sub-futures inline from a single task, so one
/// parse runs at a time and one granule at a time was doubled.
/// `tests/glm_poll_peak.rs::a_granule_under_the_parser_exists_once` is the
/// gate, and `pub` is what lets an integration test hold it: the same window
/// taken around a whole poll cannot see this, because the transport's own
/// buffers vary by more than a granule between runs.
pub fn parse_glm_granule(
    data: Vec<u8>,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, String> {
    let file = squallar_netcdf::Granule::from_vec(data)?;
    parse_with_source(&file, satellite, levels)
}

fn parse_with_source<S: VarSource>(
    file: &S,
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<GranuleParse, String> {
    // Fallback epoch; the per-variable epoch named in `units` wins where
    // present — see `parse_level_records`.
    let time_origin = file
        .time_coverage_start()
        .as_deref()
        .and_then(cf::parse_cf_epoch)
        .ok_or_else(|| "Missing or invalid time_coverage_start attribute".to_string())?;

    let mut all_records = Vec::new();
    let mut failures: Vec<LevelFailure> = Vec::new();
    let mut drops = RecordDrops::default();

    for level in levels {
        let vars = match level {
            GlmDataLevel::Flash => &FLASH_VARS,
            GlmDataLevel::Group => &GROUP_VARS,
            GlmDataLevel::Event => &EVENT_VARS,
        };
        // One level failing must not take the others with it.
        match parse_level_records(file, vars, &time_origin, satellite) {
            Ok((records, level_drops)) => {
                all_records.extend(records);
                // Only levels that *parsed* contribute a denominator.
                drops.absorb(level_drops);
            }
            Err(e) => {
                warn_once(
                    level_parse_key(satellite, vars.lat),
                    &format!(
                        "GLM {}: {} level could not be parsed: {e}",
                        satellite.display_name(),
                        vars.level.display_name(),
                    ),
                );
                failures.push(LevelFailure {
                    satellite,
                    level: *level,
                    sample_error: e,
                });
            }
        }
    }

    // Every requested level failing makes the granule unusable, so it is
    // reported as a failed *file*.
    if !failures.is_empty() && failures.len() == levels.len() {
        return Err(failures.swap_remove(0).sample_error);
    }

    // A *partial* failure keeps the healthy levels and reports the broken one:
    // `Err` would discard good group records, and a bare `Ok` reads as
    // "everything is fine" while the layer sits empty.
    Ok(GranuleParse {
        records: all_records,
        level_failures: failures,
        drops,
    })
}

/// What one granule's bytes parsed to — [`parse_glm_granule`]'s product, and
/// `pub` for the same reason that door is.
#[derive(Debug)]
pub struct GranuleParse {
    pub records: Vec<GlmFlash>,
    pub level_failures: Vec<LevelFailure>,
    pub drops: RecordDrops,
}

/// Unit spellings accepted for `*_area`, mapped to the multiplier into km².
/// The L2 LCFA product declares `flash_area:units = "m2"`: raw count 1826 is
/// 1826 × 152601.9 m² = 278.7 km², not "1826.0 km²".
const AREA_UNITS: &[(&str, f64)] = &[
    ("m2", 1e-6),
    ("m^2", 1e-6),
    ("m**2", 1e-6),
    ("meter2", 1e-6),
    ("meters2", 1e-6),
    ("km2", 1.0),
    ("km^2", 1.0),
    ("km**2", 1.0),
];

/// Unit spellings accepted for `*_energy`, mapped to the multiplier that turns
/// them into joules.
///
/// GLM declares `units = "J"` with a scale factor around 1e-16, so real values
/// land between roughly 1e-15 and 1e-12 J. No SI prefixes: lookup is
/// case-folded, so "mJ" and "MJ" would collide into a silent factor of 1e9.
const ENERGY_UNITS: &[(&str, f64)] = &[("j", 1.0), ("joule", 1.0), ("joules", 1.0)];

/// Parse records for one GLM hierarchy level. Every variable goes through CF
/// unpacking (see [`super::cf`]): most are `_Unsigned` packed shorts and reading
/// them raw yields meaningless numbers.
fn parse_level_records<S: VarSource>(
    file: &S,
    vars: &LevelVars,
    time_origin: &chrono::NaiveDateTime,
    satellite: GlmSatellite,
) -> Result<(Vec<GlmFlash>, RecordDrops), String> {
    // Required *columns*: absence is a schema change and fails the level. An
    // absent *value* arrives quietly as `None` inside `UnpackedVar::values`.
    let lats = read_required_unpacked(file, vars.lat)?;
    let lons = read_required_unpacked(file, vars.lon)?;
    let energies = read_required_unpacked(file, vars.energy)?;
    let times = read_required_unpacked(file, vars.time_offset)?;

    // Every variable at a level shares one dimension, so a short column means a
    // corrupt or restructured file.
    let count = lats.values.len();
    for (name, len) in [
        (vars.lon, lons.values.len()),
        (vars.energy, energies.values.len()),
        (vars.time_offset, times.values.len()),
    ] {
        if len != count {
            return Err(format!(
                "GLM variable length mismatch: '{name}' has {len} values but '{}' has {count}",
                vars.lat,
            ));
        }
    }

    // Area is the one optional column: events have no area variable.
    let areas = match vars.area {
        Some(name) => match read_optional_unpacked(file, name)? {
            Some(v) if v.values.len() == count => Some(v),
            Some(v) => {
                log::warn!(
                    "GLM {}: '{name}' has {} values but '{}' has {count}; omitting area",
                    satellite.display_name(),
                    v.values.len(),
                    vars.lat,
                );
                None
            }
            None => None,
        },
        None => None,
    };

    // The time axis names its own epoch and unit, and wins over the
    // granule-level `time_coverage_start` so the two cannot silently drift.
    let time_units = match times.units.as_deref() {
        Some(u) => cf::parse_time_units(u).ok_or_else(|| {
            format!(
                "GLM {} declares time units {u:?} that squallar cannot interpret; \
                 refusing to guess an epoch",
                vars.time_offset
            )
        })?,
        None => cf::TimeUnits {
            seconds_per_unit: 1.0,
            epoch: *time_origin,
        },
    };
    if time_units.epoch != *time_origin {
        log::warn!(
            "GLM {}: {} units epoch {} disagrees with time_coverage_start {}; using the \
             variable's own epoch",
            satellite.display_name(),
            vars.time_offset,
            time_units.epoch,
            time_origin,
        );
    }

    // Unit resolution is scoped to the field it describes: `None` must not take
    // position, time or the other hierarchy levels down with it.
    let energy_to_j = unit_multiplier(satellite, vars.energy, Some(&energies), ENERGY_UNITS, "J");
    let area_to_km2 = unit_multiplier(
        satellite,
        vars.area.unwrap_or("area"),
        areas.as_ref(),
        AREA_UNITS,
        "km2",
    );

    let mut records = Vec::with_capacity(count);
    let mut drops = RecordDrops {
        considered: count,
        ..RecordDrops::default()
    };

    for i in 0..count {
        // A `_FillValue` in any field that places a strike in space and time
        // makes the detection unusable; drop it rather than fabricate a number.
        let (Some(lat), Some(lon), Some(offset)) =
            (lats.values[i], lons.values[i], times.values[i])
        else {
            drops.fill_values += 1;
            continue;
        };

        let lon = normalize_longitude(lon);

        // Backstop against a coordinate that unpacked to nonsense. Effectively
        // guards latitude only: the wrap above can carry a mis-unpacked
        // longitude back into the valid interval.
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            drops.off_globe += 1;
            continue;
        }

        // Energy and area are descriptive, not locating: an unreported value
        // leaves the field `None`. Never zero — `0f32.log10()` is -inf and
        // `rasterize` draws unknown as the smallest possible bolt.
        // `_FillValue = -1s` means a value can be absent in a present column.
        let energy = column_value(Some(&energies), i)
            .zip(energy_to_j)
            .map(|(v, to_j)| (v * to_j) as f32);
        let area = column_value(areas.as_ref(), i)
            .zip(area_to_km2)
            .map(|(v, to_km2)| (v * to_km2) as f32);

        // Microseconds, not milliseconds: GLM's time `scale_factor` is
        // 3.814756e-4 s, so representable instants are 0.38 ms apart and
        // millisecond truncation collapses adjacent ones. On a `milliseconds
        // since` axis it would truncate the sub-millisecond offsets to zero.
        let micros = (offset * time_units.seconds_per_unit * 1e6) as i64;
        let time = time_units.epoch + TimeDelta::microseconds(micros);

        records.push(GlmFlash {
            lat,
            lon,
            energy,
            area,
            time,
            satellite,
            level: vars.level,
        });
    }

    if drops.dropped() > 0 {
        log::warn!(
            "GLM {} {}: dropped {} record(s) with fill values and {} with \
             out-of-range coordinates (of {count})",
            satellite.display_name(),
            vars.level.display_name(),
            drops.fill_values,
            drops.off_globe,
        );
    }

    Ok((records, drops))
}

fn warn_missing_variable_once(name: &'static str) {
    warn_once(
        missing_variable_key(name),
        &format!(
            "GLM: variable '{name}' is absent from the L2 LCFA file - the product \
             schema has changed and this field can no longer be read"
        ),
    );
}

/// A variable absent from the file is a schema change: warned once and failed
/// here. An individual `_FillValue` (or out-of-`valid_range`) value is a
/// per-record condition, carried as a `None` in [`cf::UnpackedVar::values`].
fn read_required_unpacked<S: VarSource>(
    file: &S,
    name: &'static str,
) -> Result<cf::UnpackedVar, String> {
    match file.read_unpacked(name)? {
        Some(var) => Ok(var),
        None => {
            warn_missing_variable_once(name);
            Err(format!(
                "GLM file has no '{name}' variable (product schema change?)"
            ))
        }
    }
}

fn read_optional_unpacked<S: VarSource>(
    file: &S,
    name: &'static str,
) -> Result<Option<cf::UnpackedVar>, String> {
    let var = file.read_unpacked(name)?;
    if var.is_none() {
        warn_missing_variable_once(name);
    }
    Ok(var)
}

/// GLM stores longitude in an *unwrapped* frame anchored on the spacecraft, so
/// the valid interval depends on `add_offset`, which tracks the satellite
/// sub-point (verified on live granules):
///
/// | slot                  | `event_lon:add_offset` | unpackable range   |
/// |-----------------------|------------------------|--------------------|
/// | GOES-East (G16, G19)  | -141.56                | -141.56 …   -8.44  |
/// | GOES-West (G18)       | -203.56                | -203.56 …  -70.44  |
///
/// GOES-West therefore runs past the antimeridian: a real detection at
/// 172.72°E is stored as -187.28. One wrap is enough for any offset the product
/// uses; anything further out is left alone for the range check downstream.
pub(super) fn normalize_longitude(lon: f64) -> f64 {
    if (-180.0..=180.0).contains(&lon) || lon.abs() > 540.0 {
        lon
    } else if lon < 0.0 {
        lon + 360.0
    } else {
        lon - 360.0
    }
}

fn column_value(column: Option<&cf::UnpackedVar>, i: usize) -> Option<f64> {
    column?.values.get(i).copied().flatten()
}

/// Resolve the multiplier converting a variable's declared `units` into the unit
/// squallar stores, or `None` if that cannot be done.
///
/// A value is reported only when the file says what unit it is in and we can
/// convert it: assuming an absent `units` attribute is canonical would report
/// `flash_area`, shipped as `m2`, a million times too large.
fn unit_multiplier(
    satellite: GlmSatellite,
    name: &str,
    column: Option<&cf::UnpackedVar>,
    table: &[(&str, f64)],
    canonical: &str,
) -> Option<f64> {
    // No column at all is a product property, not an anomaly: there is no
    // `event_area`.
    let column = column?;
    let sat = satellite.display_name();

    let Some(units) = column.units.as_deref() else {
        warn_once(
            units_key(satellite, name, "absent"),
            &format!(
                "GLM {sat}: {name} declares no units attribute; reporting the field as \
             unknown rather than assuming {canonical}"
            ),
        );
        return None;
    };

    let key = units.trim().to_ascii_lowercase();
    let found = table
        .iter()
        .find(|(spelling, _)| *spelling == key)
        .map(|(_, multiplier)| *multiplier);

    if found.is_none() {
        warn_once(
            units_key(satellite, name, &key),
            &format!(
                "GLM {sat}: {name} declares units {units:?}, which squallar cannot convert \
             to {canonical}; reporting the field as unknown. This is an upstream \
             product change - the conversion table in `glm::fetch` needs the new \
             spelling."
            ),
        );
    }
    found
}

fn slot_key(satellite: GlmSatellite) -> &'static str {
    match satellite {
        GlmSatellite::GoesEast => "goes-east",
        GlmSatellite::GoesWest => "goes-west",
    }
}

pub(super) fn level_parse_key(satellite: GlmSatellite, lat_var: &str) -> String {
    format!("{}:level-parse:{lat_var}", slot_key(satellite))
}

/// Dedup key for "a variable the product used to have is gone". *Not*
/// satellite-qualified: the variable set is a property of the product schema.
pub(super) fn missing_variable_key(name: &str) -> String {
    format!("variable-absent:{name}")
}

/// Dedup key for "this variable declares a unit we cannot convert", keyed on
/// the satellite *and* the offending spelling.
pub(super) fn units_key(satellite: GlmSatellite, name: &str, spelling: &str) -> String {
    format!("{}:{name}:units:{spelling}", slot_key(satellite))
}

pub(crate) fn warn_once(key: String, message: &str) {
    if claim_warning(key) {
        log::warn!("{message}");
    }
}

pub(crate) fn claim_warning(key: String) -> bool {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = seen.lock().unwrap_or_else(|e| e.into_inner());
    guard.insert(key)
}

fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            _ => {
                for b in c.to_string().as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

// Native-only: builds a loopback client with `ClientBuilder::timeout`, which
// reqwest's wasm builder does not have.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
