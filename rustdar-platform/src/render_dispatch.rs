use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nexrad_level3::model::Level3Message;
use rustdar_radar::render::{render_level3_message_to_image, render_radar_to_image};
use rustdar_radar::types::RadarProduct;

use crate::WindowRef;
use crate::channels::RenderResponse;
use crate::constants::{MAX_CONCURRENT_RENDERS, MAX_RENDER_CACHE_ENTRIES};

/// Drop guard that decrements an AtomicUsize counter on drop.
/// Guarantees the counter is decremented even if the thread panics.
pub(crate) struct RenderGuard(pub(crate) Arc<AtomicUsize>);

impl Drop for RenderGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Cached raw RGBA + metadata from the last successful render so we can
/// re-upload the texture instantly after suspend/resume without re-rendering.
pub struct CachedPaneRender {
    pub image_data: Arc<Vec<u8>>,
    pub max_range_km: f64,
    pub value_data: Arc<Vec<f32>>,
    pub product: RadarProduct,
    pub elevation: f32,
}

/// Per-pane render tracking state.
pub struct PaneRenderState {
    /// True while a background render is in progress for this pane.
    pub render_in_flight: bool,
    /// Last rendered radar parameters to detect changes.
    pub last_rendered: Option<(RadarProduct, f32)>,
    /// Cached render for instant texture restore after suspend/resume.
    pub cached_render: Option<CachedPaneRender>,
}

impl Default for PaneRenderState {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneRenderState {
    pub fn new() -> Self {
        Self {
            render_in_flight: false,
            last_rendered: None,
            cached_render: None,
        }
    }
}

/// Cached radar render output, shared across panes that show the same product/elevation.
pub struct CachedRenderOutput {
    pub image_data: Arc<Vec<u8>>,
    pub max_range_km: f64,
    pub value_data: Arc<Vec<f32>>,
}

/// `(site, product, elevation_tenths)` — see [`elevation_key`].
pub type RenderCacheKey = (String, RadarProduct, i32);

/// Bounded least-recently-used cache of render outputs shared between panes.
///
/// Each entry is an `IMAGE_SIZE²` RGBA image plus an `IMAGE_SIZE²` `f32` value
/// grid — 32 MiB apiece at 2048² — and before this was bounded the only thing
/// that ever dropped one was `reset_panes*`, so switching product or elevation
/// grew the cache without limit.
///
/// The recency queue holds exactly the keys of `entries`, each exactly once,
/// oldest use first. Every method that touches one touches the other; the pair
/// is private so no caller can desynchronise them.
pub struct RenderCache {
    entries: HashMap<RenderCacheKey, CachedRenderOutput>,
    recency: VecDeque<RenderCacheKey>,
    capacity: usize,
}

impl RenderCache {
    /// `capacity` is floored at 1 — a zero-capacity cache would evict every entry
    /// on the way in, which is a silent way to disable pane sharing entirely.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Move `key` to the most-recently-used end. No-op if absent.
    fn touch(&mut self, key: &RenderCacheKey) {
        if let Some(pos) = self.recency.iter().position(|k| k == key) {
            let k = self.recency.remove(pos).expect("position() just yielded it");
            self.recency.push_back(k);
        }
    }

    /// Look up an entry, marking it most-recently-used.
    ///
    /// Takes `&mut self` deliberately: a lookup that did not count as a use would
    /// let the pane currently on screen age out while an unwatched one survived.
    pub fn get(&mut self, key: &RenderCacheKey) -> Option<&CachedRenderOutput> {
        if !self.entries.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.entries.get(key)
    }

    /// Insert an entry, evicting the least recently used until within capacity.
    pub fn insert(&mut self, key: RenderCacheKey, value: CachedRenderOutput) {
        if self.entries.insert(key.clone(), value).is_some() {
            // Replacing an existing entry: it is already in `recency`, just refresh it.
            self.touch(&key);
        } else {
            self.recency.push_back(key);
        }
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.recency.pop_front() else { break };
            self.entries.remove(&oldest);
        }
    }

    /// Drop every entry whose key fails `keep`.
    pub fn retain(&mut self, keep: impl Fn(&RenderCacheKey) -> bool) {
        self.entries.retain(|k, _| keep(k));
        self.recency.retain(|k| keep(k));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.recency.clear();
    }

    #[cfg(test)]
    pub fn entry_count(&self) -> usize {
        debug_assert_eq!(self.entries.len(), self.recency.len(), "recency queue out of step");
        self.entries.len()
    }

    /// Keys ordered least- to most-recently-used.
    #[cfg(test)]
    pub fn recency_order(&self) -> Vec<RenderCacheKey> {
        self.recency.iter().cloned().collect()
    }
}

/// Quantize an elevation angle to tenths of a degree for cache key use.
///
/// Coarser than `rustdar_egui::pane::ELEVATION_TOLERANCE`, deliberately: that is a
/// pairwise comparison, this has to be a hashable bucket, and no exact bucketing
/// agrees with a tolerance at the edges. Tenths is finer than any real sweep spacing,
/// so two selections that compare equal never land in different buckets in practice.
fn elevation_key(elevation: f32) -> i32 {
    (elevation * 10.0).round() as i32
}

/// Manages radar rendering dispatch and Level III data caching.
///
/// Tracks per-pane render state, owns the Level III data cache, and
/// provides generation-based staleness checks for both fetches and renders.
pub struct RenderDispatcher {
    /// Per-pane render tracking (indexed by pane index).
    pub pane_render: Vec<PaneRenderState>,
    /// Decoded Level III product data, keyed by (RadarProduct, tilt_code, site).
    pub level3_data: HashMap<(RadarProduct, String, String), Arc<Level3Message>>,
    /// Generation counter to discard stale render results after site/scan changes.
    pub render_generation: u64,
    /// Per-site fetch generation counters to discard stale fetch results.
    pub fetch_generations: HashMap<String, u64>,
    /// Shared counter for concurrent background render threads.
    ///
    /// This is the single source of truth for the `MAX_CONCURRENT_RENDERS` budget and is
    /// shared by *both* render paths: static pane renders (`spawn_render` below) and loop
    /// frame renders (`App::spawn_loop_frame_render` / `App::dispatch_loop_renders`).
    /// Never introduce a second counter — two independent counters would each enforce the
    /// limit separately and allow up to 2x the intended number of concurrent 2048x2048
    /// render threads (and the matching memory spike). All call sites must reach this
    /// field, cloning the `Arc` only to hand a `RenderGuard` to a spawned thread.
    pub renders_in_flight: Arc<AtomicUsize>,
    /// Cache of the latest render output per (site, product, elevation_tenths), shared
    /// across panes that display the same product at the same elevation on the same site.
    ///
    /// Bounded by `MAX_RENDER_CACHE_ENTRIES` on an LRU policy: it is a sharing cache
    /// for the panes on screen, not a history, and each entry costs `IMAGE_SIZE² × 8`
    /// bytes.
    pub render_cache: RenderCache,
}

impl Default for RenderDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderDispatcher {
    pub fn new() -> Self {
        Self {
            pane_render: vec![PaneRenderState::new()],
            level3_data: HashMap::new(),
            render_generation: 0,
            fetch_generations: HashMap::new(),
            // Owned here so there is exactly one render budget counter in the process.
            renders_in_flight: Arc::new(AtomicUsize::new(0)),
            render_cache: RenderCache::new(MAX_RENDER_CACHE_ENTRIES),
        }
    }

    /// Ensure the pane_render vec has at least `count` entries.
    pub fn ensure_pane_count(&mut self, count: usize) {
        while self.pane_render.len() < count {
            self.pane_render.push(PaneRenderState::new());
        }
    }

    /// Reset render state for panes on a specific site (e.g. after a new scan loads for that site).
    pub fn reset_panes_for_site(&mut self, site: &str, gui: &rustdar_egui::Gui) {
        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
            if gui.pane(idx).is_some_and(|p| p.site == site) {
                prs.last_rendered = None;
                prs.cached_render = None;
                prs.render_in_flight = false;
            }
        }
        self.render_generation += 1;
        self.level3_data.retain(|(_prod, _tilt, s), _| s != site);
        self.render_cache.retain(|(s, _prod, _elev)| s != site);
    }

    /// Reset all pane render state (e.g. after a new scan loads).
    pub fn reset_panes(&mut self) {
        for prs in &mut self.pane_render {
            prs.last_rendered = None;
            prs.cached_render = None;
            prs.render_in_flight = false;
        }
        self.render_generation += 1;
        self.level3_data.clear();
        self.render_cache.clear();
    }

    /// Clear render state for suspend/resume or surface loss.
    /// Keeps `cached_render` intact for instant texture restore.
    pub fn clear_last_rendered(&mut self) {
        for prs in &mut self.pane_render {
            prs.last_rendered = None;
        }
    }

    /// Check if any pane has a render in flight.
    pub fn any_render_in_flight(&self) -> bool {
        self.pane_render.iter().any(|prs| prs.render_in_flight)
    }

    /// Increment the fetch generation for a site and return the new value.
    pub fn next_fetch_generation(&mut self, site: &str) -> u64 {
        let entry = self.fetch_generations.entry(site.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    /// Check if a fetch generation is stale for a site.
    pub fn is_fetch_stale(&self, site: &str, generation: u64) -> bool {
        self.fetch_generations.get(site).copied().unwrap_or(0) > generation
    }

    /// Check if a render generation is stale.
    pub fn is_render_stale(&self, generation: u64) -> bool {
        generation < self.render_generation
    }

    /// Look up a cached render result for the given site, product, and elevation.
    ///
    /// `&mut self` because a hit counts as a use for the LRU: a pane that keeps
    /// reusing its cached render must not age out behind one nobody is looking at.
    pub fn get_cached_render(&mut self, site: &str, product: RadarProduct, elevation: f32) -> Option<&CachedRenderOutput> {
        self.render_cache.get(&(site.to_string(), product, elevation_key(elevation)))
    }

    /// Store a render result in the cache for sharing across panes.
    pub fn cache_render(&mut self, site: &str, product: RadarProduct, elevation: f32, output: CachedRenderOutput) {
        self.render_cache.insert((site.to_string(), product, elevation_key(elevation)), output);
    }
}

/// Parameters identifying a radar product to render at a specific location.
pub struct RenderParams {
    pub product: RadarProduct,
    pub elevation: f32,
    pub lat: f64,
    pub lon: f64,
}

impl RenderDispatcher {
    /// Spawn a Level III render for a pane if applicable.
    /// Returns `true` if a render was spawned.
    pub fn try_spawn_level3_render(
        &mut self,
        pane_idx: usize,
        params: &RenderParams,
        site: &str,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
    ) -> bool {
        let best_l3 = self
            .level3_data
            .iter()
            .filter(|((p, _tilt, s), _)| *p == params.product && s == site)
            .min_by(|(_, a), (_, b)| {
                let da = (a.pdb.elevation_angle() - params.elevation).abs();
                let db = (b.pdb.elevation_angle() - params.elevation).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(_, msg)| Arc::clone(msg));

        let Some(l3_msg) = best_l3 else {
            return false;
        };

        let lat = params.lat;
        let lon = params.lon;
        let product = params.product;
        log::info!(
            "Spawning Level III render for pane {}: {:?}",
            pane_idx,
            product
        );
        self.spawn_render(pane_idx, params.product, params.elevation, sender, window, move || {
            render_level3_message_to_image(&l3_msg, product, lat, lon)
        });
        true
    }

    /// Spawn a Level II render for a pane.
    pub fn spawn_level2_render(
        &mut self,
        pane_idx: usize,
        params: &RenderParams,
        data: Arc<nexrad_model::data::Scan>,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
    ) {
        let product = params.product;
        let elevation = params.elevation;
        let lat = params.lat;
        let lon = params.lon;
        log::info!(
            "Spawning background render for pane {}: {:?} at {:.1}°",
            pane_idx,
            product,
            elevation
        );
        self.spawn_render(pane_idx, product, elevation, sender, window, move || {
            render_radar_to_image(&data, elevation, product, lat, lon)
        });
    }

    /// Shared thread dispatch for both Level II and Level III renders.
    fn spawn_render(
        &mut self,
        pane_idx: usize,
        product: RadarProduct,
        elevation: f32,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
        render_fn: impl FnOnce() -> Option<(Vec<u8>, f64, Vec<f32>)> + Send + 'static,
    ) {
        // Check concurrent render limit
        let current = self.renders_in_flight.load(Ordering::Relaxed);
        if current >= MAX_CONCURRENT_RENDERS {
            return;
        }
        self.renders_in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(Arc::clone(&self.renders_in_flight));

        let generation = self.render_generation;
        std::thread::Builder::new()
            .name("radar-render".into())
            .spawn(move || {
            let _guard = guard;
            if let Some((image, range, values)) = render_fn() {
                let _ = sender.send(RenderResponse {
                    image_data: Arc::new(image),
                    max_range_km: range,
                    value_data: Arc::new(values),
                    product,
                    elevation,
                    generation,
                    pane_idx,
                });
            }
            crate::app::notify_redraw(&window);
        }).expect("failed to spawn radar-render thread");
        self.pane_render[pane_idx].render_in_flight = true;
    }
}

#[cfg(test)]
mod render_cache_tests {
    use super::*;

    fn key(site: &str, elevation_tenths: i32) -> RenderCacheKey {
        (site.to_string(), RadarProduct::Reflectivity, elevation_tenths)
    }

    /// A distinguishable entry — `max_range_km` doubles as the identity so a test
    /// can tell which render it got back.
    fn output(range: f64) -> CachedRenderOutput {
        CachedRenderOutput {
            image_data: Arc::new(Vec::new()),
            max_range_km: range,
            value_data: Arc::new(Vec::new()),
        }
    }

    /// The bound the cache exists for. Before this it was a bare `HashMap` that only
    /// `reset_panes*` ever shrank, so cycling products grew it without limit at
    /// ~32 MiB per entry.
    #[test]
    fn inserting_past_capacity_evicts_instead_of_growing() {
        let mut cache = RenderCache::new(3);
        for i in 0..10 {
            cache.insert(key("KTLX", i), output(i as f64));
        }
        assert_eq!(cache.entry_count(), 3, "capacity must bound the cache");
        // The three newest survived; everything older is gone.
        assert!(cache.get(&key("KTLX", 9)).is_some());
        assert!(cache.get(&key("KTLX", 8)).is_some());
        assert!(cache.get(&key("KTLX", 7)).is_some());
        assert!(cache.get(&key("KTLX", 6)).is_none());
        assert!(cache.get(&key("KTLX", 0)).is_none());
    }

    /// Least *recently used*, not least recently inserted. A pane that keeps reading
    /// its entry must not lose it to one nobody has touched since it was written.
    #[test]
    fn a_read_protects_an_entry_from_eviction() {
        let mut cache = RenderCache::new(3);
        cache.insert(key("KTLX", 0), output(0.0));
        cache.insert(key("KTLX", 1), output(1.0));
        cache.insert(key("KTLX", 2), output(2.0));

        // Touch the oldest, making the *second* oldest the eviction candidate.
        assert!(cache.get(&key("KTLX", 0)).is_some());
        cache.insert(key("KTLX", 3), output(3.0));

        assert!(cache.get(&key("KTLX", 0)).is_some(), "the read should have saved it");
        assert!(cache.get(&key("KTLX", 1)).is_none(), "untouched since insert, so it goes");
        assert_eq!(cache.entry_count(), 3);
    }

    /// Re-inserting an existing key replaces the value and refreshes its position,
    /// rather than queueing the key a second time and corrupting the eviction order.
    #[test]
    fn reinserting_a_key_replaces_it_without_duplicating_it() {
        let mut cache = RenderCache::new(2);
        cache.insert(key("KTLX", 0), output(0.0));
        cache.insert(key("KTLX", 1), output(1.0));
        cache.insert(key("KTLX", 0), output(99.0));

        assert_eq!(cache.entry_count(), 2, "a replacement is not a new entry");
        assert_eq!(cache.recency_order(), vec![key("KTLX", 1), key("KTLX", 0)]);
        assert_eq!(cache.get(&key("KTLX", 0)).unwrap().max_range_km, 99.0);

        // With `0` refreshed, `1` is now the oldest and is what a third insert evicts.
        cache.insert(key("KTLX", 2), output(2.0));
        assert!(cache.get(&key("KTLX", 1)).is_none());
        assert!(cache.get(&key("KTLX", 0)).is_some());
    }

    /// `reset_panes_for_site` drops one site's entries. The recency queue has to lose
    /// them too, or it later evicts a key that is no longer in the map while the real
    /// oldest entry survives.
    #[test]
    fn retain_drops_keys_from_the_recency_queue_as_well() {
        let mut cache = RenderCache::new(4);
        cache.insert(key("KTLX", 0), output(0.0));
        cache.insert(key("KOUN", 1), output(1.0));
        cache.insert(key("KTLX", 2), output(2.0));

        cache.retain(|(site, _, _)| site != "KTLX");

        assert_eq!(cache.entry_count(), 1);
        assert_eq!(cache.recency_order(), vec![key("KOUN", 1)]);

        // Fill past capacity; KOUN is the oldest real entry and must be the one to go.
        for i in 10..14 {
            cache.insert(key("KDDC", i), output(i as f64));
        }
        assert_eq!(cache.entry_count(), 4);
        assert!(cache.get(&key("KOUN", 1)).is_none());
        assert!(cache.get(&key("KDDC", 13)).is_some());
    }

    #[test]
    fn clear_empties_both_halves() {
        let mut cache = RenderCache::new(4);
        cache.insert(key("KTLX", 0), output(0.0));
        cache.insert(key("KTLX", 1), output(1.0));
        cache.clear();
        assert_eq!(cache.entry_count(), 0);
        assert!(cache.recency_order().is_empty());
    }

    /// A zero capacity would evict every entry on the way in, silently disabling the
    /// cross-pane sharing the cache exists for.
    #[test]
    fn capacity_is_floored_at_one() {
        let mut cache = RenderCache::new(0);
        cache.insert(key("KTLX", 0), output(0.0));
        assert_eq!(cache.entry_count(), 1);
        assert!(cache.get(&key("KTLX", 0)).is_some());
    }

    /// The cache the dispatcher actually builds must hold every pane that can be on
    /// screen at once, or the panes evict each other and every layout change
    /// re-renders.
    ///
    /// Asserted by filling a real `RenderDispatcher` rather than by comparing
    /// `MAX_RENDER_CACHE_ENTRIES` against the pane limit. Those two constants can
    /// both be right while the dispatcher hands `RenderCache::new` something else
    /// entirely — a comparison of constants observes the *intent*, and this is the
    /// one place the intent is wired up.
    #[test]
    fn the_dispatchers_own_cache_holds_every_pane_on_screen() {
        let max_panes = if cfg!(target_os = "android") {
            rustdar_egui::pane::MAX_PANES_MOBILE
        } else {
            rustdar_egui::pane::MAX_PANES_DESKTOP
        };
        let sites: Vec<String> = (0..max_panes).map(|i| format!("SITE{i}")).collect();
        assert!(
            MAX_RENDER_CACHE_ENTRIES >= sites.len(),
            "precondition: the bound itself is too small — {MAX_RENDER_CACHE_ENTRIES} \
             entries for {} panes",
            sites.len()
        );

        let mut dispatcher = RenderDispatcher::new();
        for (i, site) in sites.iter().enumerate() {
            dispatcher.cache_render(site, RadarProduct::Reflectivity, 0.5, output(i as f64));
        }

        // A full screen of panes, each on its own site: none may have evicted another.
        for (i, site) in sites.iter().enumerate() {
            let hit = dispatcher.get_cached_render(site, RadarProduct::Reflectivity, 0.5);
            let Some(hit) = hit else {
                panic!("{site} was evicted with only {} panes' worth cached", sites.len());
            };
            assert_eq!(hit.max_range_km, i as f64, "{site} came back as another pane's render");
        }
    }
}
