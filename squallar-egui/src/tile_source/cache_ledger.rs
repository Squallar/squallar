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
//!   [`Totals::puts_first`] (the slot was a pending marker — or there was no
//!   slot, the LRU having let the marker go while the request was out: the
//!   body is the tile's first sight either way and, the request having
//!   stayed open in `super::TileCache::in_flight`, its only one),
//!   [`Totals::puts_restyle`] (the slot held a tile re-asked for restyling),
//!   [`Totals::puts_duplicate`] (the slot already held a current tile: a
//!   second body nothing asked for), [`Totals::puts_orphan`] (no slot and no
//!   request open: a body nothing asked for and nothing holds). The last two
//!   are the two shapes of a body fetched for nothing, and both read zero
//!   while the in-flight set is right — a duplicate is a re-ask that got past
//!   it, an orphan an arrival it never recorded. Before the set, an evicted
//!   pending marker made one or the other: its body landed as an orphan when
//!   it beat the re-ask, and the re-ask's body as a duplicate when it did
//!   not — 9 and 16 respectively over 607 asks at cap 100 on the 12x12
//!   loopback pin.
//! * [`Totals::evicted_pending`] and [`Totals::evicted_resident`] — LRU
//!   evictions, split by whether the slot held a tile or only a marker. A
//!   pending eviction is a request still out whose body will land with no
//!   marker under it; the request stays open, so the tile is not asked for
//!   again, and the body lands as a first sight. [`Totals::evicted_bytes`]
//!   carries what the slots were charged (`super::CachedTile::bytes`).
//! * The **levels** — [`Totals::resident_entries`], [`Totals::resident_bytes`],
//!   [`Totals::overrun_bytes`], [`Totals::floor_entries`],
//!   [`Totals::wanted_on_glass`], [`Totals::wanted_net`],
//!   [`Totals::parsed_entries`], [`Totals::parsed_bytes`], [`Totals::snapped`]
//!   — are what is held or wanted right now, stored rather than added, and
//!   the only figures here that go down. With several sources of one role
//!   the last writer's level stands. `overrun` is what the working-set floor
//!   keeps resident past the byte budget; `floor` is that working set plus
//!   the in-flight markers; the two `wanted` figures are the last whole
//!   pass's cells at the drawn level and in the ancestor net, which is the
//!   tile term the application prices its scene with; `snapped` is `1` while
//!   the tile-sharpness rung holds the source at the whole zoom
//!   ([`super::snap`]). The reported line carries all of these but
//!   [`Totals::parsed_bytes`], because `overrun`, `floor` and the two
//!   `wanted` figures are what price [`Totals::refetch_still_wanted`] against
//!   the scene rather than against a number.
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
    /// A pending marker, or no slot with the request still open: the tile's
    /// first sight.
    First,
    /// A tile re-asked for under a newer style generation.
    Restyle,
    /// A current tile already there: a second body nothing asked for.
    Duplicate,
    /// No slot and no request open: a body nothing asked for.
    Orphan,
}

/// What an LRU eviction let go of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvictedKind {
    /// A `None` marker: a request still out (or a recorded failure). The
    /// request itself is not evicted with it.
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
    RefetchAfterEviction {
        /// Whether the previous pass's walk asked for this id too — see
        /// [`Totals::refetch_still_wanted`].
        still_wanted: bool,
    },
    Put(PutKind),
    Evicted {
        kind: EvictedKind,
        bytes: u64,
    },
    /// One grid cell a pass could draw **nothing** for. See
    /// [`Totals::blank_cells`].
    BlankCell,
}

/// A reading of one role, taken together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub requests: u64,
    pub restyle_asks: u64,
    pub refetch_after_eviction: u64,
    /// **The subset of [`Self::refetch_after_eviction`] the cache dropped
    /// off the glass** — the reading that tells the two refetches apart,
    /// because they have opposite repairs.
    ///
    /// A refetch on its own says nothing. A viewport that panned or zoomed
    /// away and came back legitimately re-asks for what the cache dropped
    /// behind it, and the answer there is *nothing*: holding it would be
    /// holding history for a scene the user had left. But a cell the cache
    /// let go of while the walk still had it is a cache under its working
    /// set, and the answer there is the floor or the budget. `refetch after
    /// eviction` alone cannot separate them, and it was read at 60-67 % of
    /// asks on a panning web leg with no way to say which it was.
    ///
    /// **Two conditions, and neither is sufficient.** The id has to be one
    /// the previous pass's walk asked for (`super::AskQueue`, rotated at
    /// `HttpsTiles::note_wanted`) *and* the eviction has to be no older than
    /// that pass. Dropping the second half is not a smaller reading, it is a
    /// wrong one: the refused-ask queue retries cells the last pass wanted by
    /// construction, so a cell that genuinely left the glass twenty passes
    /// ago and is retried now would satisfy the walk's test alone. Measured,
    /// that misreading was **bimodal** — 0 or ~150 of 330 refetches on the
    /// same reversing sweep, once in forty runs, depending on how the request
    /// channel happened to fill.
    ///
    /// Dropping the *first* half is no better: the pass distance alone
    /// cannot tell a cell the walk is still asking for from one it has left,
    /// and a channel refusal delays a re-ask by however many passes it takes
    /// the queue to reach it.
    ///
    /// Zero on every fixture run against a healthy tree — a static window, a
    /// window sliding one and three columns a pass, and one that reverses and
    /// refetches half its asks. With the floor removed and the budget put
    /// under one window's cells it reads **36 of 211 refetches on a window
    /// that never moved**, holding 22 entries against 36 on the glass.
    ///
    /// That 36 is the figure to read the counter's direction from: it
    /// **under**-reports a broken cache and never over-reports one. Both
    /// conditions have to hold at the moment of the ask, and under real
    /// pressure the re-ask is often several passes behind the eviction that
    /// caused it, because the request channel refused it in between. What the
    /// counter is for is the sign and not the size — zero against non-zero —
    /// and the resident-against-wanted levels beside it on the line are what
    /// price how far under the cache is.
    pub refetch_still_wanted: u64,
    pub puts_first: u64,
    pub puts_restyle: u64,
    pub puts_duplicate: u64,
    pub puts_orphan: u64,
    pub evicted_pending: u64,
    pub evicted_resident: u64,
    pub evicted_bytes: u64,
    /// **Cells a pass drew as a hole** — one per `HttpsTiles::ground_at` that
    /// found neither the tile nor any ancestor of it resident, which is the
    /// vanishing basemap tile as the user sees it.
    ///
    /// The denominator is **grid cells walked, not tiles**: `draw_tile_layer`
    /// calls `ground_at` once per cell per pass per pane, so a pane blank for
    /// ten passes counts its span ten times. Read it against `wanted on
    /// glass`, which is that same walk's cell count for one pass, and never
    /// against `asks` or `puts`, which count tiles.
    ///
    /// **Why it is here and not in a draw counter.** The only ancestor level
    /// anything ever fetches is the net at
    /// [`crate::tiles::WARM_ANCESTOR_STEPS`]; every level between it and the
    /// drawn one is resident by accident or not at all, so this is what a net
    /// cell's eviction looks like from the glass, and until this counter
    /// existed that eviction was invisible to every figure the ledger
    /// published. It is one `fetch_add` on the miss path and none on the hit
    /// path.
    pub blank_cells: u64,
    /// A level: slots held right now, markers included.
    pub resident_entries: u64,
    /// A level: what those slots are charged, every entry at least the
    /// marker's node — see `super::CachedTile::bytes`.
    pub resident_bytes: u64,
    /// A level: resident bytes past the budget, which only the working-set
    /// floor (or a shrink not yet paid) can produce.
    pub overrun_bytes: u64,
    /// A level: the entries the cache is holding whatever the budget says —
    /// the measured working set plus the in-flight markers.
    pub floor_entries: u64,
    /// A level: cells at the drawn level the last whole pass wanted.
    pub wanted_on_glass: u64,
    /// A level: cells of the ancestor net the last whole pass wanted.
    pub wanted_net: u64,
    /// A level: parses held in the source's parsed-geometry cache.
    pub parsed_entries: u64,
    /// A level: what those parses are charged.
    pub parsed_bytes: u64,
    /// A level: `1` while the tile-sharpness rung holds a source of this
    /// role at the whole zoom below the fractional one, else `0` — see
    /// [`super::snap`]. The reported line's trailing `snap` field.
    pub snapped: u64,
}

/// The four cache levels [`set_resident`] stores together, so a reading is
/// never half of one publish and half of another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Levels {
    pub resident_entries: u64,
    pub resident_bytes: u64,
    pub overrun_bytes: u64,
    pub floor_entries: u64,
}

impl Totals {
    /// Record one event. The per-source mirror of [`note`].
    pub fn apply(&mut self, event: CacheEvent) {
        match event {
            CacheEvent::Request => self.requests += 1,
            CacheEvent::RestyleAsk => self.restyle_asks += 1,
            CacheEvent::RefetchAfterEviction { still_wanted } => {
                self.refetch_after_eviction += 1;
                if still_wanted {
                    self.refetch_still_wanted += 1;
                }
            }
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
            CacheEvent::BlankCell => self.blank_cells += 1,
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
            .wrapping_add(self.refetch_still_wanted)
            .wrapping_add(self.puts())
            .wrapping_add(self.evicted())
            .wrapping_add(self.evicted_bytes)
            .wrapping_add(self.blank_cells)
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
            refetch_still_wanted: self
                .refetch_still_wanted
                .saturating_sub(earlier.refetch_still_wanted),
            puts_first: self.puts_first.saturating_sub(earlier.puts_first),
            puts_restyle: self.puts_restyle.saturating_sub(earlier.puts_restyle),
            puts_duplicate: self.puts_duplicate.saturating_sub(earlier.puts_duplicate),
            puts_orphan: self.puts_orphan.saturating_sub(earlier.puts_orphan),
            evicted_pending: self.evicted_pending.saturating_sub(earlier.evicted_pending),
            evicted_resident: self
                .evicted_resident
                .saturating_sub(earlier.evicted_resident),
            evicted_bytes: self.evicted_bytes.saturating_sub(earlier.evicted_bytes),
            blank_cells: self.blank_cells.saturating_sub(earlier.blank_cells),
            resident_entries: self.resident_entries,
            resident_bytes: self.resident_bytes,
            overrun_bytes: self.overrun_bytes,
            floor_entries: self.floor_entries,
            wanted_on_glass: self.wanted_on_glass,
            wanted_net: self.wanted_net,
            parsed_entries: self.parsed_entries,
            parsed_bytes: self.parsed_bytes,
            snapped: self.snapped,
        }
    }
}

/// One role's counters.
struct RoleLedger {
    requests: AtomicU64,
    restyle_asks: AtomicU64,
    refetch_after_eviction: AtomicU64,
    refetch_still_wanted: AtomicU64,
    puts_first: AtomicU64,
    puts_restyle: AtomicU64,
    puts_duplicate: AtomicU64,
    puts_orphan: AtomicU64,
    evicted_pending: AtomicU64,
    evicted_resident: AtomicU64,
    evicted_bytes: AtomicU64,
    blank_cells: AtomicU64,
    resident_entries: AtomicU64,
    resident_bytes: AtomicU64,
    overrun_bytes: AtomicU64,
    floor_entries: AtomicU64,
    wanted_on_glass: AtomicU64,
    wanted_net: AtomicU64,
    parsed_entries: AtomicU64,
    parsed_bytes: AtomicU64,
    snapped: AtomicU64,
    /// The last [`Totals::progress`] handed out by [`totals_if_moved`].
    reported: AtomicU64,
}

impl RoleLedger {
    const fn new() -> Self {
        Self {
            requests: AtomicU64::new(0),
            restyle_asks: AtomicU64::new(0),
            refetch_after_eviction: AtomicU64::new(0),
            refetch_still_wanted: AtomicU64::new(0),
            puts_first: AtomicU64::new(0),
            puts_restyle: AtomicU64::new(0),
            puts_duplicate: AtomicU64::new(0),
            puts_orphan: AtomicU64::new(0),
            evicted_pending: AtomicU64::new(0),
            evicted_resident: AtomicU64::new(0),
            evicted_bytes: AtomicU64::new(0),
            blank_cells: AtomicU64::new(0),
            resident_entries: AtomicU64::new(0),
            resident_bytes: AtomicU64::new(0),
            overrun_bytes: AtomicU64::new(0),
            floor_entries: AtomicU64::new(0),
            wanted_on_glass: AtomicU64::new(0),
            wanted_net: AtomicU64::new(0),
            parsed_entries: AtomicU64::new(0),
            parsed_bytes: AtomicU64::new(0),
            snapped: AtomicU64::new(0),
            reported: AtomicU64::new(0),
        }
    }
}

/// One set rather than a set owned by a source, because the report wants every
/// source of a role in one reading, and because a role can outlive any one
/// source (the base map is rebuilt on a theme change).
///
/// # A test build: one set per thread, and why
///
/// `cfg(test)`, and the `test-support` feature `squallar-app` turns on through
/// its dev-dependency, replace that one shared set with a **per-thread** one.
/// [`crate::overlay_cache::ledger`] carries the full account of the
/// arrangement; the reason to repeat it here is that these counters are not
/// only reported.
///
/// **`squallar-app` reads them as an absolute LEVEL and prices the scene with
/// it.** `app_render::tile_need_for` turns `wanted_on_glass`, `wanted_net` and
/// the `resident_bytes / resident_entries` ratio into a `scene::TileNeed`,
/// `app_render::tile_needs` makes those the scene's `tile_sources`, and every
/// budget decision is taken against that scene — the ladder's `fit`, and the
/// promotion margin in `app_render::observe_host_recovery`, whose
/// `model_spare` clears its bar by about 20 MiB of 195 on the six-picture web
/// fixture. So a sibling test merely putting a tile moved another test's
/// budget arithmetic: a governor deciding on a figure that was never its own.
///
/// The note beside `totals_if_moved` still holds for readers that take
/// **differences** of two readings. A reader that takes a level cannot be made
/// correct that way, so the object the tests shared is removed instead of
/// being locked — the same conclusion, and for the same reason, as the
/// crate-wide lock that arrangement deleted.
#[cfg(not(any(test, feature = "test-support")))]
fn sink() -> &'static [RoleLedger; ROLES.len()] {
    static SHARED: [RoleLedger; ROLES.len()] = [RoleLedger::new(), RoleLedger::new()];
    &SHARED
}

/// One set of counters per thread — the production arm above carries the whole
/// account of why.
#[cfg(any(test, feature = "test-support"))]
fn sink() -> &'static [RoleLedger; ROLES.len()] {
    thread_local! {
        /// Leaked rather than borrowed, so this arm hands back the same
        /// `&'static` the production arm does and every body below stays one
        /// spelling. One set per thread that touches this ledger at all; the
        /// process exit frees them.
        static OWN: &'static [RoleLedger; ROLES.len()] =
            Box::leak(Box::new([RoleLedger::new(), RoleLedger::new()]));
    }
    OWN.with(|ledger| *ledger)
}

/// Record one event against `role`. The whole hot-path API, one `fetch_add`.
pub fn note(role: CacheRole, event: CacheEvent) {
    let ledger = &sink()[role.index()];
    match event {
        CacheEvent::Request => ledger.requests.fetch_add(1, Relaxed),
        CacheEvent::RestyleAsk => ledger.restyle_asks.fetch_add(1, Relaxed),
        CacheEvent::RefetchAfterEviction { still_wanted } => {
            ledger.refetch_after_eviction.fetch_add(1, Relaxed);
            if still_wanted {
                ledger.refetch_still_wanted.fetch_add(1, Relaxed);
            }
            0
        }
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
        CacheEvent::BlankCell => ledger.blank_cells.fetch_add(1, Relaxed),
    };
}

/// What one source of `role` holds right now. Levels, so stored not added.
pub fn set_resident(role: CacheRole, levels: Levels) {
    let ledger = &sink()[role.index()];
    ledger
        .resident_entries
        .store(levels.resident_entries, Relaxed);
    ledger.resident_bytes.store(levels.resident_bytes, Relaxed);
    ledger.overrun_bytes.store(levels.overrun_bytes, Relaxed);
    ledger.floor_entries.store(levels.floor_entries, Relaxed);
    publish_styled_census();
}

/// Republish the styled-tile level of [`crate::heap_census`], **both roles
/// summed**.
///
/// The census wants one figure per allocation family and a source knows only
/// its own role, so the sum has to happen somewhere that can see both — and
/// this module is the only such place. Doing it here rather than at the
/// census's own read means no caller has to remember to add the two, which is
/// the failure mode a per-role figure invites.
///
/// Denominator: exactly what the styled `ByteLru`s charge, which is the
/// tessellated shapes each `CachedTile` holds on this instance's heap. The
/// undecoded bodies and the parses are separate families with their own
/// levels; the tile meshes are the GPU's and are on neither.
///
/// Two `Relaxed` loads and one store, on a path that has just done four
/// stores to the same cache lines.
fn publish_styled_census() {
    let bytes = sink()
        .iter()
        .map(|ledger| ledger.resident_bytes.load(Relaxed))
        .fold(0u64, u64::saturating_add);
    crate::heap_census::set_tile_cache_bytes(bytes);
}

/// Republish the parsed-tile level of [`crate::heap_census`], **both roles
/// summed**. See [`publish_styled_census`] for why the sum lives here;
/// the denominator is `walkers::mvt::ParsedTile::heap_bytes`, which is what
/// the parsed `ByteLru`s charge.
fn publish_parsed_census() {
    let bytes = sink()
        .iter()
        .map(|ledger| ledger.parsed_bytes.load(Relaxed))
        .fold(0u64, u64::saturating_add);
    crate::heap_census::set_tile_parsed_bytes(bytes);
}

/// What the last whole pass drawing one source of `role` wanted: cells at the
/// drawn level and in the ancestor net. Levels, stored at the pass boundary.
pub fn set_wanted(role: CacheRole, on_glass: u64, net: u64) {
    let ledger = &sink()[role.index()];
    ledger.wanted_on_glass.store(on_glass, Relaxed);
    ledger.wanted_net.store(net, Relaxed);
}

/// How many parses one source of `role`'s parsed-geometry cache holds, and
/// what they are charged. Levels, stored where the parse lands or leaves.
pub fn set_parsed(role: CacheRole, entries: u64, bytes: u64) {
    let ledger = &sink()[role.index()];
    ledger.parsed_entries.store(entries, Relaxed);
    ledger.parsed_bytes.store(bytes, Relaxed);
    publish_parsed_census();
}

/// Whether the tile-sharpness rung holds one source of `role` at the whole
/// zoom. A level, stored where the source's [`super::snap::SnapState`] flips.
pub fn set_snapped(role: CacheRole, snapped: bool) {
    sink()[role.index()]
        .snapped
        .store(u64::from(snapped), Relaxed);
}

/// Read one role.
pub fn totals(role: CacheRole) -> Totals {
    let ledger = &sink()[role.index()];
    Totals {
        requests: ledger.requests.load(Relaxed),
        restyle_asks: ledger.restyle_asks.load(Relaxed),
        refetch_after_eviction: ledger.refetch_after_eviction.load(Relaxed),
        refetch_still_wanted: ledger.refetch_still_wanted.load(Relaxed),
        puts_first: ledger.puts_first.load(Relaxed),
        puts_restyle: ledger.puts_restyle.load(Relaxed),
        puts_duplicate: ledger.puts_duplicate.load(Relaxed),
        puts_orphan: ledger.puts_orphan.load(Relaxed),
        evicted_pending: ledger.evicted_pending.load(Relaxed),
        evicted_resident: ledger.evicted_resident.load(Relaxed),
        evicted_bytes: ledger.evicted_bytes.load(Relaxed),
        blank_cells: ledger.blank_cells.load(Relaxed),
        resident_entries: ledger.resident_entries.load(Relaxed),
        resident_bytes: ledger.resident_bytes.load(Relaxed),
        overrun_bytes: ledger.overrun_bytes.load(Relaxed),
        floor_entries: ledger.floor_entries.load(Relaxed),
        wanted_on_glass: ledger.wanted_on_glass.load(Relaxed),
        wanted_net: ledger.wanted_net.load(Relaxed),
        parsed_entries: ledger.parsed_entries.load(Relaxed),
        parsed_bytes: ledger.parsed_bytes.load(Relaxed),
        snapped: ledger.snapped.load(Relaxed),
    }
}

/// [`totals`], but only when a counter has moved since the last time this
/// was asked for `role` — the telemetry writer's read, so a role with no
/// activity writes no line and an idle app writes none at all.
pub fn totals_if_moved(role: CacheRole) -> Option<Totals> {
    let totals = totals(role);
    let progress = totals.progress();
    if sink()[role.index()].reported.swap(progress, Relaxed) == progress {
        return None;
    }
    Some(totals)
}

// No `reset` here, deliberately, on `super::take_ledger`'s terms. In a
// production build the counters are process-global, so a reset would be one
// source clearing every other source's account of a role it shares. In a test
// build `sink` gives each thread its own set, so there is nothing left to
// reset across: a test's own writes are the only writes it can see. Readers
// still prefer a difference of two readings, or a source's own `Totals`.

#[cfg(test)]
mod tests;
