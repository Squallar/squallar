//! An LRU bounded in **bytes**, with a floor in **entries**.
//!
//! The tile cache is the resource model in miniature. **Need** is the working
//! set — the tiles on the glass and the ancestor net under them, measured by
//! the pass that drew them — and it is held whatever the budget says: a cache
//! that evicts a tile still on the glass refetches it next frame, for
//! something the user never stopped looking at. **Economy** is everything
//! resident beyond that — the history a pan comes back to — and it is worth
//! exactly what the budget allows. **Capacity** arrives as the budget, from the
//! device's brackets or from a measured card, and it only ever limits.
//!
//! So eviction here has two conditions and not one: an entry goes when the
//! cache is over its byte budget **and** holds more entries than its floor.
//! What is left over the budget while the floor forbids eviction is the
//! [`ByteLru::overrun_bytes`], a level the ledger reports rather than a state
//! the cache hides.
//!
//! **Shrink lazily, grow eagerly.** [`ByteLru::set_budget`] never evicts: a
//! budget lowered under the resident bytes leaves debt, and [`ByteLru::put`]
//! and [`ByteLru::trim_one`] pay it down — the former bounded per call, the
//! latter one entry per call, from the pump, so a resize or a pane split never
//! lands a synchronous drop of a hundred tiles on the frame thread. Evicted
//! values are handed back to the caller in every case, because the caller is
//! the one that knows where they may be freed (`offload::discard` from the
//! frame thread, inline from an IO thread).

use std::hash::Hash;

use lru::LruCache;

/// What every entry is charged at least, in bytes: the LRU node — key, value
/// and two list links — plus the hash table's bucket. The cache is built on
/// `LruCache::unbounded()`, so this is what keeps a cache of pending markers
/// (a `None` slot holds no tile and prices at nothing else) bounded in bytes
/// like everything else in it; `tests::a_marker_is_charged_at_least_its_node`
/// holds the figure above the node's real size.
pub const MARKER_BYTES: u64 = 128;

/// The most entries one [`ByteLru::put`] evicts to make room. The rest of any
/// debt is left for [`ByteLru::trim_one`], one per pump, so a budget that just
/// fell by a hundred tiles costs the frame that puts the next tile eight
/// evictions and not a hundred. Eight because an entry can be a thousand times
/// another's size — a 456-byte ocean tile against a 1.03 MB city core — and
/// one city core displacing eight ocean tiles is the common shape of a pan
/// into town; the debt beyond that is paid at one per pump.
const EVICTIONS_PER_PUT: usize = 8;

/// One entry the cache let go of, handed back for the caller to free.
#[derive(Debug, PartialEq, Eq)]
pub struct Evicted<K, V> {
    pub key: K,
    pub value: V,
    /// What the entry was charged.
    pub bytes: u64,
}

/// A value and what it was charged.
struct Charged<V> {
    value: V,
    bytes: u64,
}

/// See the module doc.
pub struct ByteLru<K: Hash + Eq, V> {
    slots: LruCache<K, Charged<V>>,
    budget: u64,
    resident: u64,
    floor_entries: usize,
}

impl<K: Hash + Eq, V> ByteLru<K, V> {
    /// An empty cache allowed `budget` bytes of residency, with no floor.
    pub fn new(budget: u64) -> Self {
        Self {
            slots: LruCache::unbounded(),
            budget,
            resident: 0,
            floor_entries: 0,
        }
    }

    /// Insert `value` under `key`, charged `bytes` (raised to
    /// [`MARKER_BYTES`] if smaller), as the most recent entry. A value already
    /// under `key` is replaced and returned; it is not an eviction. Entries
    /// evicted to make room — at most [`EVICTIONS_PER_PUT`], least recent
    /// first, never below the floor, never the entry just put — are pushed
    /// onto `evicted`.
    pub fn put(
        &mut self,
        key: K,
        value: V,
        bytes: u64,
        evicted: &mut Vec<Evicted<K, V>>,
    ) -> Option<V> {
        let bytes = bytes.max(MARKER_BYTES);
        let replaced = self.slots.put(key, Charged { value, bytes }).map(|old| {
            self.resident = self.resident.saturating_sub(old.bytes);
            old.value
        });
        self.resident = self.resident.saturating_add(bytes);
        // Never the entry just put: it is the most recent, so it is the
        // victim only when it is the sole entry, and an entry larger than the
        // whole budget is still admitted — the budget bounds history, it does
        // not refuse a tile. A later trim from the pump may still take it once
        // it is history; here it is what was asked for.
        for _ in 0..EVICTIONS_PER_PUT {
            if self.slots.len() <= 1 {
                break;
            }
            match self.trim_one() {
                Some(gone) => evicted.push(gone),
                None => break,
            }
        }
        replaced
    }

    /// Evict the least recent entry if the cache is over budget and above its
    /// floor, else nothing. One entry, so a pump can call it on every pass and
    /// pay a shrink down one tile at a time.
    pub fn trim_one(&mut self) -> Option<Evicted<K, V>> {
        if self.resident <= self.budget || self.slots.len() <= self.floor_entries {
            return None;
        }
        let (key, charged) = self.slots.pop_lru()?;
        self.resident = self.resident.saturating_sub(charged.bytes);
        Some(Evicted {
            key,
            value: charged.value,
            bytes: charged.bytes,
        })
    }

    /// The value under `key`, made the most recent entry — a use.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.slots.get(key).map(|charged| &charged.value)
    }

    /// [`Self::get`], mutably.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.slots.get_mut(key).map(|charged| &mut charged.value)
    }

    /// The value under `key`, with recency left alone.
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.slots.peek(key).map(|charged| &charged.value)
    }

    /// Whether `key` holds an entry, with recency left alone.
    pub fn contains(&self, key: &K) -> bool {
        self.slots.contains(key)
    }

    /// Remove the entry under `key`, answering it and what it was charged.
    pub fn pop(&mut self, key: &K) -> Option<(V, u64)> {
        let charged = self.slots.pop(key)?;
        self.resident = self.resident.saturating_sub(charged.bytes);
        Some((charged.value, charged.bytes))
    }

    /// Re-price the entry under `key` at `bytes` (raised to [`MARKER_BYTES`])
    /// without touching its recency or its value — a marker that became a tile
    /// in place. Answers whether there was an entry to re-price. Evicts
    /// nothing: the next put or trim pays for the difference.
    pub fn recharge(&mut self, key: &K, bytes: u64) -> bool {
        let bytes = bytes.max(MARKER_BYTES);
        match self.slots.peek_mut(key) {
            Some(charged) => {
                self.resident = self
                    .resident
                    .saturating_sub(charged.bytes)
                    .saturating_add(bytes);
                charged.bytes = bytes;
                true
            }
            None => false,
        }
    }

    /// Allow `bytes` of residency from now on. A rise takes effect at once —
    /// there is nothing to do; a fall leaves debt for [`Self::put`] and
    /// [`Self::trim_one`] to pay, and evicts nothing here.
    pub fn set_budget(&mut self, bytes: u64) {
        self.budget = bytes;
    }

    /// Hold at least `entries` entries whatever the budget says — the pass's
    /// measured working set, plus what may be in flight for it. Lowering it
    /// evicts nothing here either.
    pub fn set_floor_entries(&mut self, entries: usize) {
        self.floor_entries = entries;
    }

    /// Entries held.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether nothing is held.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// What is held, in bytes as charged.
    pub fn resident_bytes(&self) -> u64 {
        self.resident
    }

    /// What may be held.
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// The floor in entries.
    pub fn floor_entries(&self) -> usize {
        self.floor_entries
    }

    /// What is held over the budget: zero while the budget holds, else the
    /// bytes the floor (or a trim not yet paid) keeps resident past it.
    pub fn overrun_bytes(&self) -> u64 {
        self.resident.saturating_sub(self.budget)
    }

    /// What the **working set alone** holds past the budget:
    /// [`Self::overrun_bytes`] once every entry the floor does not protect is
    /// gone, and zero while any remains. The distinction is what the
    /// tile-sharpness rung arms on (`super::snap`): a shrink not yet paid is
    /// economy leaving one entry a pump and reads as plain overrun for as many
    /// pumps as it has entries, which must never shed a rung; a working set
    /// that does not fit reads here, and only here.
    pub fn floor_overrun_bytes(&self) -> u64 {
        if self.slots.len() <= self.floor_entries {
            self.overrun_bytes()
        } else {
            0
        }
    }

    /// The mean charge of a resident entry, floored at [`MARKER_BYTES`] — what
    /// a consumer projecting a working set's cost multiplies by.
    pub fn mean_entry_bytes(&self) -> u64 {
        let entries = self.slots.len() as u64;
        if entries == 0 {
            return MARKER_BYTES;
        }
        (self.resident / entries).max(MARKER_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys<V>(evicted: &[Evicted<u32, V>]) -> Vec<u32> {
        evicted.iter().map(|e| e.key).collect()
    }

    /// Recency at a byte bound: what goes is the least recently used, a use
    /// refreshes, and the entry just put is never its own victim.
    #[test]
    fn eviction_at_a_byte_bound_takes_the_least_recent() {
        let mut cache: ByteLru<u32, &str> = ByteLru::new(3 * MARKER_BYTES);
        let mut evicted = Vec::new();
        for k in 0..3 {
            cache.put(k, "v", MARKER_BYTES, &mut evicted);
        }
        assert!(evicted.is_empty(), "a fill to the budget evicts nothing");
        assert_eq!(cache.resident_bytes(), 3 * MARKER_BYTES);
        // Touch 0 so 1 is the oldest.
        assert!(cache.get(&0).is_some());
        cache.put(3, "v", MARKER_BYTES, &mut evicted);
        assert_eq!(keys(&evicted), vec![1]);
        assert_eq!(cache.len(), 3);
        assert!(cache.contains(&0) && cache.contains(&2) && cache.contains(&3));
        assert_eq!(cache.resident_bytes(), 3 * MARKER_BYTES);
        assert_eq!(cache.overrun_bytes(), 0);
    }

    /// A big entry displaces several small ones, and an entry larger than the
    /// whole budget is still admitted — the budget is a bound on history, not
    /// a refusal — with the overrun reported.
    #[test]
    fn a_large_entry_displaces_what_it_costs_and_an_oversize_one_is_still_held() {
        let mut cache: ByteLru<u32, ()> = ByteLru::new(4 * MARKER_BYTES);
        let mut evicted = Vec::new();
        for k in 0..4 {
            cache.put(k, (), MARKER_BYTES, &mut evicted);
        }
        cache.put(9, (), 3 * MARKER_BYTES, &mut evicted);
        assert_eq!(keys(&evicted), vec![0, 1, 2]);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.resident_bytes(), 4 * MARKER_BYTES);

        let mut cache: ByteLru<u32, ()> = ByteLru::new(MARKER_BYTES);
        let mut evicted = Vec::new();
        cache.put(1, (), 10 * MARKER_BYTES, &mut evicted);
        assert!(
            evicted.is_empty(),
            "the entry just put is never its own victim"
        );
        assert_eq!(cache.overrun_bytes(), 9 * MARKER_BYTES);
    }

    /// The floor holds N entries at a budget of one byte: nothing under the
    /// floor is ever evicted, and the excess reads as overrun.
    #[test]
    fn the_floor_holds_its_entries_at_a_budget_of_one_byte() {
        let mut cache: ByteLru<u32, ()> = ByteLru::new(1);
        cache.set_floor_entries(144);
        let mut evicted = Vec::new();
        for k in 0..144 {
            cache.put(k, (), MARKER_BYTES, &mut evicted);
        }
        assert!(
            evicted.is_empty(),
            "the floor was breached: {:?}",
            keys(&evicted)
        );
        assert_eq!(cache.len(), 144);
        assert_eq!(cache.overrun_bytes(), 144 * MARKER_BYTES - 1);
        // The 145th is history, and the least recent goes for it.
        cache.put(144, (), MARKER_BYTES, &mut evicted);
        assert_eq!(keys(&evicted), vec![0]);
        assert_eq!(cache.len(), 144);
        // Lowering the floor evicts nothing by itself; the next trim does.
        cache.set_floor_entries(10);
        assert_eq!(cache.len(), 144);
        assert_eq!(cache.trim_one().map(|e| e.key), Some(1));
    }

    /// A shrink is lazy: `set_budget` evicts nothing, and each trim pays down
    /// one entry.
    #[test]
    fn a_shrink_evicts_nothing_at_once_and_one_per_trim() {
        let mut cache: ByteLru<u32, ()> = ByteLru::new(10 * MARKER_BYTES);
        let mut evicted = Vec::new();
        for k in 0..10 {
            cache.put(k, (), MARKER_BYTES, &mut evicted);
        }
        cache.set_budget(2 * MARKER_BYTES);
        assert_eq!(cache.len(), 10, "set_budget evicted synchronously");
        assert_eq!(cache.overrun_bytes(), 8 * MARKER_BYTES);
        for expected in 0..8 {
            let gone = cache.trim_one().expect("debt remains");
            assert_eq!(gone.key, expected);
            assert_eq!(gone.bytes, MARKER_BYTES);
        }
        assert_eq!(cache.trim_one().map(|e| e.key), None, "the debt is paid");
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.overrun_bytes(), 0);
    }

    /// **The working set's overrun is told from a shrink's.** Ten entries over
    /// a budget of two read as eight of plain overrun either way; with a floor
    /// of four they are history leaving and the working set reads zero until
    /// the history is gone, then the two floor entries the budget cannot hold.
    #[test]
    fn the_floor_overrun_is_zero_while_history_remains() {
        let mut cache: ByteLru<u32, ()> = ByteLru::new(10 * MARKER_BYTES);
        let mut evicted = Vec::new();
        for k in 0..10 {
            cache.put(k, (), MARKER_BYTES, &mut evicted);
        }
        cache.set_floor_entries(4);
        cache.set_budget(2 * MARKER_BYTES);
        assert_eq!(cache.overrun_bytes(), 8 * MARKER_BYTES);
        assert_eq!(
            cache.floor_overrun_bytes(),
            0,
            "a shrink not yet paid read as the working set overrunning"
        );
        for _ in 0..6 {
            cache.trim_one().expect("history remains");
        }
        assert_eq!(cache.len(), 4, "the trim went under the floor");
        assert_eq!(cache.trim_one().map(|e| e.key), None, "the floor holds");
        assert_eq!(cache.overrun_bytes(), 2 * MARKER_BYTES);
        assert_eq!(
            cache.floor_overrun_bytes(),
            2 * MARKER_BYTES,
            "the history is gone and the floor's own overrun did not read"
        );
        cache.set_budget(4 * MARKER_BYTES);
        assert_eq!(
            cache.floor_overrun_bytes(),
            0,
            "a budget that holds the floor overruns nothing"
        );
    }

    /// A put under debt pays at most `EVICTIONS_PER_PUT` of it.
    #[test]
    fn a_put_under_debt_evicts_a_bounded_number() {
        let mut cache: ByteLru<u32, ()> = ByteLru::new(100 * MARKER_BYTES);
        let mut evicted = Vec::new();
        for k in 0..100 {
            cache.put(k, (), MARKER_BYTES, &mut evicted);
        }
        cache.set_budget(MARKER_BYTES);
        cache.put(100, (), MARKER_BYTES, &mut evicted);
        assert_eq!(evicted.len(), EVICTIONS_PER_PUT);
        assert_eq!(
            keys(&evicted),
            (0..EVICTIONS_PER_PUT as u32).collect::<Vec<_>>()
        );
        assert_eq!(cache.len(), 101 - EVICTIONS_PER_PUT);
    }

    /// Growing evicts nothing and clears the overrun.
    #[test]
    fn a_grow_evicts_nothing() {
        let mut cache: ByteLru<u32, ()> = ByteLru::new(MARKER_BYTES);
        cache.set_floor_entries(5);
        let mut evicted = Vec::new();
        for k in 0..5 {
            cache.put(k, (), MARKER_BYTES, &mut evicted);
        }
        assert_eq!(cache.overrun_bytes(), 4 * MARKER_BYTES);
        cache.set_budget(100 * MARKER_BYTES);
        assert_eq!(cache.trim_one().map(|e| e.key), None);
        assert_eq!(cache.overrun_bytes(), 0);
        assert_eq!(cache.len(), 5);
    }

    /// Recharge moves the resident figure by the difference and nothing else;
    /// a replacement under the same key is a replacement, not an eviction.
    #[test]
    fn recharge_and_replacement_keep_the_resident_figure_honest() {
        let mut cache: ByteLru<u32, &str> = ByteLru::new(1 << 20);
        let mut evicted = Vec::new();
        assert_eq!(cache.put(1, "marker", MARKER_BYTES, &mut evicted), None);
        assert!(cache.recharge(&1, 1000));
        assert_eq!(cache.resident_bytes(), 1000);
        assert!(
            cache.recharge(&1, 1),
            "a tiny recharge is raised to the marker"
        );
        assert_eq!(cache.resident_bytes(), MARKER_BYTES);
        assert!(!cache.recharge(&2, 1000), "nothing under 2 to re-price");
        assert_eq!(cache.put(1, "tile", 5000, &mut evicted), Some("marker"));
        assert_eq!(cache.resident_bytes(), 5000);
        assert!(evicted.is_empty());
        assert_eq!(cache.pop(&1), Some(("tile", 5000)));
        assert_eq!(cache.resident_bytes(), 0);
        assert!(cache.is_empty());
    }

    /// The overrun arithmetic and the mean, at the floor.
    #[test]
    fn overrun_and_mean_entry_bytes_read_as_the_arithmetic_says() {
        let mut cache: ByteLru<u32, ()> = ByteLru::new(1000);
        assert_eq!(
            cache.mean_entry_bytes(),
            MARKER_BYTES,
            "empty: the floor figure"
        );
        let mut evicted = Vec::new();
        cache.set_floor_entries(3);
        cache.put(1, (), 600, &mut evicted);
        cache.put(2, (), 600, &mut evicted);
        cache.put(3, (), 600, &mut evicted);
        assert!(evicted.is_empty());
        assert_eq!(cache.resident_bytes(), 1800);
        assert_eq!(cache.overrun_bytes(), 800);
        assert_eq!(cache.mean_entry_bytes(), 600);
        assert_eq!(cache.budget(), 1000);
        assert_eq!(cache.floor_entries(), 3);
    }

    /// The marker charge covers what a node of the tile cache's own types
    /// really occupies, so a cache of markers is priced from above.
    #[test]
    fn a_marker_is_charged_at_least_its_node() {
        let node = std::mem::size_of::<walkers::TileId>()
            + std::mem::size_of::<Charged<super::super::CachedTile>>()
            + 2 * std::mem::size_of::<usize>()
            + std::mem::size_of::<usize>()
            + 1;
        assert!(
            MARKER_BYTES as usize >= node,
            "a tile cache node is {node} bytes; MARKER_BYTES ({MARKER_BYTES}) under-prices it"
        );
    }
}
