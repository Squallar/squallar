//! What one source's tile cache did — asked, fetched twice, put, evicted —
//! classified **at the cache**, where a first sight and a repeat are
//! distinguishable.
//!
//! **Product telemetry, not a campaign instrument**, on the terms of
//! [`super::take_ledger`], [`crate::basemap_ledger`] and
//! [`crate::tile_mesh::ledger`]: always on, no feature gate, no lock, no
//! allocation. Every write is one relaxed `fetch_add` (or one relaxed `store`
//! for a level) on a `static`. The sentence that reports these numbers is
//! written by `squallar-app`, so nothing formats on a path that records.
//!
//! # Why a ledger at the cache, when the GPU store already counts
//!
//! [`crate::tile_mesh::ledger`] counts mesh **uploads** and **evictions** in
//! the renderer's store. That store keys on an identity minted per
//! [`crate::tile_mesh::TileMeshes`], so it cannot tell an upload that is a
//! tile's first sight from one that is the same tile fetched again after the
//! LRU dropped it, nor from a restyle that legitimately re-uploads. A reading
//! of many uploads against nearly as many evictions is therefore
//! **undiagnosable from the store**. The cache is where the three cases are
//! different events: the slot a body lands in was a pending marker, a
//! stale-styled tile, a tile already present, or nothing at all.
//!
//! # The denominators, and no two of them are added
//!
//! Every counter is per **role** — [`CacheRole::Base`] for the basemap
//! sources, [`CacheRole::Terrain`] for the hillshade — because the two caches
//! hold different things at different prices and a sum over them would
//! describe neither. Within a role:
//!
//! * [`Totals::requests`] — **fresh asks**: a `None` marker was put and a
//!   body is owed by the archive or the network. Never per tile drawn, never
//!   per frame; a cache hit is not a request.
//! * [`Totals::restyle_asks`] — **re-asks under a new style generation**: a
//!   slot from an older generation was re-stamped and a re-styling is owed
//!   out of the parsed cache. Disjoint from `requests`; a theme flip moves
//!   this and not that.
//! * [`Totals::refetch_after_eviction`] — the **subset of `requests`** whose
//!   id the cache remembers evicting recently. Every one of these is a body
//!   the cache once held and let go while something still wanted it. The
//!   figure "downright broken on web" is diagnosed by: a cache below the
//!   working set shows this climbing on a static viewport.
//! * The four **puts**, one per tile landing and each landing in exactly one:
//!   [`Totals::puts_first`] (the slot was a pending marker, or nothing this
//!   cache remembers), [`Totals::puts_restyle`] (the slot held a tile
//!   re-asked for restyling), [`Totals::puts_duplicate`] (the slot already
//!   held a current tile — two bodies for one ask), [`Totals::puts_orphan`]
//!   (the slot was gone and its id is in the evicted memory: the pending
//!   marker was evicted before its body landed, so the body was fetched for
//!   nothing and will be fetched again).
//! * [`Totals::evicted_pending`] and [`Totals::evicted_resident`] — LRU
//!   evictions, split by whether the slot held a tile or only a marker. A
//!   pending eviction is a fetch in flight whose result now has no home.
//!   [`Totals::evicted_bytes`] prices the resident ones as
//!   `super::slot_bytes` prices a slot.
//! * The three **levels** — [`Totals::resident_entries`],
//!   [`Totals::resident_bytes`], [`Totals::parsed_entries`] — are what is held
//!   right now, stored rather than added, and the only figures here that go
//!   down. With several sources of one role the last writer's level stands.
//!
//! `requests − puts` is asks still in flight or failed, not a rate. `uploads`
//! (the GPU store's) is never compared to any figure here by subtraction: an
//! upload is a mesh buffer write and a put is a cache slot, and a put with no
//! fills uploads nothing.
//!
//! **No figure recorded here gates CI.** The browser rig reads the line and
//! asserts a *delta* of zero over a static viewport on an opt-in leg.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Which cache a reading is about. Stamped on a source at construction:
/// the basemap constructors mark `Base`, the hillshade constructor `Terrain`,
/// and a plain HTTP raster source is `Base` too — it draws the ground.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheRole {
    Base,
    Terrain,
}

/// The roles in report order. The order is the line order, pinned by a test.
pub const ROLES: [CacheRole; 2] = [CacheRole::Base, CacheRole::Terrain];

impl CacheRole {
    const fn index(self) -> usize {
        match self {
            CacheRole::Base => 0,
            CacheRole::Terrain => 1,
        }
    }

    /// The word the reported line carries in its parenthesis. Lowercase and
    /// stable: `.github/browser-rig/drive.py` matches on it.
    pub const fn label(self) -> &'static str {
        match self {
            CacheRole::Base => "base",
            CacheRole::Terrain => "terrain",
        }
    }
}

/// What a body landing in the cache found there. See the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PutKind {
    First,
    Restyle,
    Duplicate,
    Orphan,
}

/// What an LRU eviction let go of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvictedKind {
    /// A `None` marker: a fetch in flight (or a recorded failure).
    Pending,
    /// A tile that could draw.
    Resident,
}

/// One thing the cache did. Applied to the source's own [`Totals`] and to the
/// process-wide statics by the same code, so the two cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheEvent {
    Request,
    RestyleAsk,
    RefetchAfterEviction,
    Put(PutKind),
    Evicted { kind: EvictedKind, bytes: u64 },
}

/// A reading of one role, taken together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub requests: u64,
    pub restyle_asks: u64,
    pub refetch_after_eviction: u64,
    pub puts_first: u64,
    pub puts_restyle: u64,
    pub puts_duplicate: u64,
    pub puts_orphan: u64,
    pub evicted_pending: u64,
    pub evicted_resident: u64,
    pub evicted_bytes: u64,
    /// A level: slots held right now, markers included.
    pub resident_entries: u64,
    /// A level: what those slots are priced at, on `super::slot_bytes`'
    /// terms — a lower bound.
    pub resident_bytes: u64,
    /// A level: parses held in the source's parsed-geometry cache.
    pub parsed_entries: u64,
}

impl Totals {
    /// Record one event. The per-source mirror of [`note`].
    pub fn apply(&mut self, event: CacheEvent) {
        match event {
            CacheEvent::Request => self.requests += 1,
            CacheEvent::RestyleAsk => self.restyle_asks += 1,
            CacheEvent::RefetchAfterEviction => self.refetch_after_eviction += 1,
            CacheEvent::Put(PutKind::First) => self.puts_first += 1,
            CacheEvent::Put(PutKind::Restyle) => self.puts_restyle += 1,
            CacheEvent::Put(PutKind::Duplicate) => self.puts_duplicate += 1,
            CacheEvent::Put(PutKind::Orphan) => self.puts_orphan += 1,
            CacheEvent::Evicted {
                kind: EvictedKind::Pending,
                ..
            } => self.evicted_pending += 1,
            CacheEvent::Evicted {
                kind: EvictedKind::Resident,
                bytes,
            } => {
                self.evicted_resident += 1;
                self.evicted_bytes += bytes;
            }
        }
    }

    /// Every put, whatever it found — each landing is in exactly one kind.
    pub fn puts(&self) -> u64 {
        self.puts_first + self.puts_restyle + self.puts_duplicate + self.puts_orphan
    }

    /// Every eviction, pending and resident.
    pub fn evicted(&self) -> u64 {
        self.evicted_pending + self.evicted_resident
    }

    /// How far along the counters are, as one number, so a reporter can tell
    /// "nothing happened since I last looked" in a single compare. The levels
    /// are deliberately out of it: they are not monotonic.
    fn progress(&self) -> u64 {
        self.requests
            .wrapping_add(self.restyle_asks)
            .wrapping_add(self.refetch_after_eviction)
            .wrapping_add(self.puts())
            .wrapping_add(self.evicted())
            .wrapping_add(self.evicted_bytes)
    }

    /// The windowed reading between two snapshots: counters subtracted,
    /// levels kept as this reading's — a level has no window.
    pub fn diff(&self, earlier: &Totals) -> Totals {
        Totals {
            requests: self.requests.saturating_sub(earlier.requests),
            restyle_asks: self.restyle_asks.saturating_sub(earlier.restyle_asks),
            refetch_after_eviction: self
                .refetch_after_eviction
                .saturating_sub(earlier.refetch_after_eviction),
            puts_first: self.puts_first.saturating_sub(earlier.puts_first),
            puts_restyle: self.puts_restyle.saturating_sub(earlier.puts_restyle),
            puts_duplicate: self.puts_duplicate.saturating_sub(earlier.puts_duplicate),
            puts_orphan: self.puts_orphan.saturating_sub(earlier.puts_orphan),
            evicted_pending: self.evicted_pending.saturating_sub(earlier.evicted_pending),
            evicted_resident: self
                .evicted_resident
                .saturating_sub(earlier.evicted_resident),
            evicted_bytes: self.evicted_bytes.saturating_sub(earlier.evicted_bytes),
            resident_entries: self.resident_entries,
            resident_bytes: self.resident_bytes,
            parsed_entries: self.parsed_entries,
        }
    }
}

/// One role's counters.
struct RoleLedger {
    requests: AtomicU64,
    restyle_asks: AtomicU64,
    refetch_after_eviction: AtomicU64,
    puts_first: AtomicU64,
    puts_restyle: AtomicU64,
    puts_duplicate: AtomicU64,
    puts_orphan: AtomicU64,
    evicted_pending: AtomicU64,
    evicted_resident: AtomicU64,
    evicted_bytes: AtomicU64,
    resident_entries: AtomicU64,
    resident_bytes: AtomicU64,
    parsed_entries: AtomicU64,
    /// The last [`Totals::progress`] handed out by [`totals_if_moved`].
    reported: AtomicU64,
}

impl RoleLedger {
    const fn new() -> Self {
        Self {
            requests: AtomicU64::new(0),
            restyle_asks: AtomicU64::new(0),
            refetch_after_eviction: AtomicU64::new(0),
            puts_first: AtomicU64::new(0),
            puts_restyle: AtomicU64::new(0),
            puts_duplicate: AtomicU64::new(0),
            puts_orphan: AtomicU64::new(0),
            evicted_pending: AtomicU64::new(0),
            evicted_resident: AtomicU64::new(0),
            evicted_bytes: AtomicU64::new(0),
            resident_entries: AtomicU64::new(0),
            resident_bytes: AtomicU64::new(0),
            parsed_entries: AtomicU64::new(0),
            reported: AtomicU64::new(0),
        }
    }
}

/// `static` rather than owned by a source because the report wants every
/// source of a role in one reading, and because a role can outlive any one
/// source (the base map is rebuilt on a theme change).
static LEDGER: [RoleLedger; ROLES.len()] = [RoleLedger::new(), RoleLedger::new()];

/// Record one event against `role`. The whole hot-path API, one `fetch_add`.
pub fn note(role: CacheRole, event: CacheEvent) {
    let ledger = &LEDGER[role.index()];
    match event {
        CacheEvent::Request => ledger.requests.fetch_add(1, Relaxed),
        CacheEvent::RestyleAsk => ledger.restyle_asks.fetch_add(1, Relaxed),
        CacheEvent::RefetchAfterEviction => ledger.refetch_after_eviction.fetch_add(1, Relaxed),
        CacheEvent::Put(PutKind::First) => ledger.puts_first.fetch_add(1, Relaxed),
        CacheEvent::Put(PutKind::Restyle) => ledger.puts_restyle.fetch_add(1, Relaxed),
        CacheEvent::Put(PutKind::Duplicate) => ledger.puts_duplicate.fetch_add(1, Relaxed),
        CacheEvent::Put(PutKind::Orphan) => ledger.puts_orphan.fetch_add(1, Relaxed),
        CacheEvent::Evicted {
            kind: EvictedKind::Pending,
            ..
        } => ledger.evicted_pending.fetch_add(1, Relaxed),
        CacheEvent::Evicted {
            kind: EvictedKind::Resident,
            bytes,
        } => {
            ledger.evicted_bytes.fetch_add(bytes, Relaxed);
            ledger.evicted_resident.fetch_add(1, Relaxed)
        }
    };
}

/// What one source of `role` holds right now. A level, so stored not added.
pub fn set_resident(role: CacheRole, entries: u64, bytes: u64) {
    let ledger = &LEDGER[role.index()];
    ledger.resident_entries.store(entries, Relaxed);
    ledger.resident_bytes.store(bytes, Relaxed);
}

/// How many parses one source of `role`'s parsed-geometry cache holds. A
/// level, stored where the parse lands.
pub fn set_parsed_entries(role: CacheRole, entries: u64) {
    LEDGER[role.index()].parsed_entries.store(entries, Relaxed);
}

/// Read one role.
pub fn totals(role: CacheRole) -> Totals {
    let ledger = &LEDGER[role.index()];
    Totals {
        requests: ledger.requests.load(Relaxed),
        restyle_asks: ledger.restyle_asks.load(Relaxed),
        refetch_after_eviction: ledger.refetch_after_eviction.load(Relaxed),
        puts_first: ledger.puts_first.load(Relaxed),
        puts_restyle: ledger.puts_restyle.load(Relaxed),
        puts_duplicate: ledger.puts_duplicate.load(Relaxed),
        puts_orphan: ledger.puts_orphan.load(Relaxed),
        evicted_pending: ledger.evicted_pending.load(Relaxed),
        evicted_resident: ledger.evicted_resident.load(Relaxed),
        evicted_bytes: ledger.evicted_bytes.load(Relaxed),
        resident_entries: ledger.resident_entries.load(Relaxed),
        resident_bytes: ledger.resident_bytes.load(Relaxed),
        parsed_entries: ledger.parsed_entries.load(Relaxed),
    }
}

/// [`totals`], but only when a counter has moved since the last time this
/// was asked for `role` — the telemetry writer's read, so a role with no
/// activity writes no line and an idle app writes none at all.
pub fn totals_if_moved(role: CacheRole) -> Option<Totals> {
    let totals = totals(role);
    let progress = totals.progress();
    if LEDGER[role.index()].reported.swap(progress, Relaxed) == progress {
        return None;
    }
    Some(totals)
}

// No `reset` here, deliberately, on `super::take_ledger`'s terms: the
// statics are process-global and a test binary runs its cases in parallel
// over them, so the tests assert differences of two readings, or read a
// source's own `Totals`, which nothing else moves.

#[cfg(test)]
mod tests;
