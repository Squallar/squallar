use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rustdar_radar::level3::Level3Product;
use rustdar_radar::srm::StormMotionSample;
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
    /// One flag per render dispatched for this pane and not yet finished, held
    /// alongside the copy the render thread carries.
    ///
    /// Clearing one abandons that render: the worker drops its result instead of
    /// sending it. This is per **pane**, which is the finest granularity the
    /// dispatch path can name — `spawn_level2_render` is handed a pane index and
    /// no site — and it is what keeps a new scan for one site from discarding the
    /// in-flight renders of panes on every *other* site, each of which then costs
    /// a fresh 2048² image and value grid to redo.
    ///
    /// **Only [`reset_panes_for_site`](RenderDispatcher::reset_panes_for_site) and
    /// [`reset_panes`](RenderDispatcher::reset_panes) clear these, and both clear
    /// `render_in_flight` on the same pane in the same pass.** That pairing is
    /// what makes a suppressed send safe: the receiver clears `render_in_flight`
    /// when a result arrives, so abandoning a render without clearing the flag
    /// would leave the pane believing a render it will never hear about is still
    /// running, and it would never dispatch another.
    results_wanted: Vec<Arc<AtomicBool>>,
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
            results_wanted: Vec::new(),
        }
    }

    /// The flag a newly dispatched render reports through, live until this pane's
    /// renders are abandoned.
    ///
    /// Finished renders are dropped from the list first: the worker holds the only
    /// other reference to its own flag, so one strong reference means it is gone.
    fn want_result(&mut self) -> Arc<AtomicBool> {
        self.results_wanted.retain(|f| Arc::strong_count(f) > 1);
        let flag = Arc::new(AtomicBool::new(true));
        self.results_wanted.push(Arc::clone(&flag));
        flag
    }

    /// Stop wanting every render currently running for this pane.
    ///
    /// A pane can have more than one: `reset_panes*` clears `render_in_flight`
    /// while a render is still going, so the next dispatch spawns a second one
    /// before the first has landed. Abandoning only the newest would leave the
    /// older free to arrive last and paint the previous scan over the new one.
    fn abandon_results(&mut self) {
        for flag in self.results_wanted.drain(..) {
            flag.store(false, Ordering::Relaxed);
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
            let k = self
                .recency
                .remove(pos)
                .expect("position() just yielded it");
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
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
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
        debug_assert_eq!(
            self.entries.len(),
            self.recency.len(),
            "recency queue out of step"
        );
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
/// Whether a Level II product reads the whole volume rather than one sweep.
///
/// Mirrors the `whole_volume` branch in `RenderInput::extract`, which is what
/// actually decides how much of the `Scan` travels: `EchoTopsInterpolated`
/// integrates every reflectivity tilt, and `NormalizedRotation` collects every
/// velocity tilt for its wind-profile fit. `VilDensity` and the hail pair
/// integrate every reflectivity tilt the way echo tops does — the hail SHI
/// column reads the same local VIL machinery. All would read a volume still
/// being assembled as a complete short one — `compute_echo_tops` clamps every
/// column to the topmost tilt present, with no error and no NaN to notice.
pub fn needs_whole_volume(product: RadarProduct) -> bool {
    matches!(
        product,
        RadarProduct::EchoTopsInterpolated
            | RadarProduct::NormalizedRotation
            | RadarProduct::VilDensity
            | RadarProduct::ProbabilityOfSevereHail
            | RadarProduct::MaxExpectedHailSize
            | RadarProduct::HydrometeorClassification
    )
}

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
    /// The latest Level III product per (product, tilt, site).
    ///
    /// Holds the whole [`Level3Product`], not just the message, so the stamp —
    /// which object it came from and when it was written — reaches the UI
    /// alongside the pixels. See [`rustdar_radar::level3::ProductStamp`].
    ///
    /// Private, so [`cache_level3`](Self::cache_level3) really is the only way
    /// in: an insert that bypassed it would drop the storm motion vector on the
    /// floor, and the pane would render with another volume's.
    level3_data: HashMap<(RadarProduct, String, String), Arc<Level3Product>>,
    /// Environmental 0 °C / −20 °C heights per site, from Open-Meteo — staged
    /// for the hail products, which will read them at render time. Written by
    /// the sounding drain in `app_render`; read back by
    /// `spawn_level3_fetches`'s TTL gate, which refetches on poll only once
    /// [`rustdar_radar::sounding::EnvHeights::is_stale`] says the entry has
    /// aged out. Survives both reset paths: the environment does not change
    /// because a pane was reset, and the TTL is the eviction policy.
    pub env_heights: HashMap<String, rustdar_radar::sounding::EnvHeights>,
    /// Generation counter to discard stale render results after a **full** reset.
    ///
    /// Bumped by [`reset_panes`](Self::reset_panes) only. Per-site resets abandon
    /// the affected panes' renders individually — see
    /// [`PaneRenderState::results_wanted`] — because this counter is global and a
    /// bump of it discards the in-flight renders of every pane on every other
    /// site, which then respawn: a wasted 2048² image and value grid per pane per
    /// cross-site poll, recurring every poll interval in a multi-site layout.
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
    /// The storm motion override the storm-relative renders on screen were
    /// built with. Nothing else about a pane changes when the user edits the
    /// vector, so without this the field would keep the old motion until the
    /// next scan. Routed into the Level II render parameters by
    /// [`spawn_level2_render`](Self::spawn_level2_render); with no override
    /// the renderer applies the Bunkers right-mover from the volume's own
    /// wind profile (`rustdar_radar::srv`). The RPG-vector history that used
    /// to live beside this left with the five Level III SRM fetches.
    last_storm_motion_override: Option<StormMotionSample>,
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
            env_heights: HashMap::new(),
            render_generation: 0,
            fetch_generations: HashMap::new(),
            // Owned here so there is exactly one render budget counter in the process.
            renders_in_flight: Arc::new(AtomicUsize::new(0)),
            render_cache: RenderCache::new(MAX_RENDER_CACHE_ENTRIES),
            last_storm_motion_override: None,
        }
    }

    /// Cache a fetched Level III product.
    ///
    /// The only way into [`level3_data`](Self::level3_data). The map keeps one
    /// object per `(product, tilt, site)`.
    pub fn cache_level3(
        &mut self,
        product: RadarProduct,
        tilt_code: String,
        site: String,
        fetched: Level3Product,
    ) {
        self.level3_data
            .insert((product, tilt_code, site), Arc::new(fetched));
    }

    /// Record the storm motion override in force and, if it moved, drop every
    /// storm-relative render that used the old one.
    ///
    /// Returns whether anything was invalidated. Both the per-pane state and
    /// the shared render cache have to go: the cache is keyed on
    /// `(site, product, elevation)`, which the vector is not part of, so a
    /// stale entry would be handed straight back to the next pane that asked.
    ///
    /// Every tilt: the field this records is the same one
    /// [`spawn_level2_render`](Self::spawn_level2_render) reads into the
    /// render parameters, so the vector a pane is invalidated for cannot
    /// differ from the one it is redrawn with.
    pub fn set_storm_motion_override(&mut self, motion: Option<StormMotionSample>) -> bool {
        if self.last_storm_motion_override == motion {
            return false;
        }
        self.last_storm_motion_override = motion;
        for prs in &mut self.pane_render {
            if matches!(
                prs.last_rendered,
                Some((RadarProduct::StormRelativeVelocity, _))
            ) {
                prs.last_rendered = None;
            }
        }
        self.render_cache
            .retain(|(_site, product, _elev)| *product != RadarProduct::StormRelativeVelocity);
        true
    }

    /// Record a site's environmental heights and, if the pair actually moved,
    /// drop that site's hail renders — the per-site counterpart of
    /// [`set_storm_motion_override`](Self::set_storm_motion_override), for the
    /// other render parameter that is not part of the cache key. Written by
    /// the sounding drain in `app_render`; the field it writes is the same one
    /// [`env_heights_km_msl_for`](Self::env_heights_km_msl_for) reads into the
    /// render parameters, so the environment a pane is invalidated for cannot
    /// differ from the one it is redrawn with.
    ///
    /// An unchanged pair still refreshes the entry — that restarts the TTL the
    /// poll's refetch gate reads — but invalidates nothing: soundings refetch
    /// on a timer and normally land identical, and redrawing every hail pane
    /// each time would repeat hourly for no visible change.
    ///
    /// Returns whether anything was invalidated.
    pub fn set_env_heights(
        &mut self,
        site: &str,
        heights: rustdar_radar::sounding::EnvHeights,
        gui: &rustdar_egui::Gui,
    ) -> bool {
        let hail = |p: RadarProduct| {
            matches!(
                p,
                RadarProduct::ProbabilityOfSevereHail | RadarProduct::MaxExpectedHailSize
            )
        };
        let unchanged = self.env_heights.get(site).is_some_and(|old| {
            old.h0c_km_msl == heights.h0c_km_msl && old.hm20c_km_msl == heights.hm20c_km_msl
        });
        self.env_heights.insert(site.to_string(), heights);
        if unchanged {
            return false;
        }
        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
            if gui.pane(idx).is_some_and(|p| p.site == site)
                && prs.last_rendered.is_some_and(|(p, _)| hail(p))
            {
                prs.last_rendered = None;
            }
        }
        self.render_cache
            .retain(|(s, product, _elev)| s != site || !hail(*product));
        true
    }

    /// Ensure the pane_render vec has at least `count` entries.
    pub fn ensure_pane_count(&mut self, count: usize) {
        while self.pane_render.len() < count {
            self.pane_render.push(PaneRenderState::new());
        }
    }

    /// Reset render state for panes on a specific site (e.g. after a new scan loads for that site).
    ///
    /// Only those panes' in-flight renders are abandoned. The global
    /// [`render_generation`](Self::render_generation) is deliberately *not* bumped:
    /// it is a single comparison for every pane, so bumping it here would throw
    /// away the renders of panes on other sites — whose data has not changed —
    /// and have them redone on every poll of every site.
    pub fn reset_panes_for_site(&mut self, site: &str, gui: &rustdar_egui::Gui) {
        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
            if gui.pane(idx).is_some_and(|p| p.site == site) {
                prs.last_rendered = None;
                prs.cached_render = None;
                prs.render_in_flight = false;
                // Paired with the line above: see `results_wanted`.
                prs.abandon_results();
            }
        }
        self.level3_data.retain(|(_prod, _tilt, s), _| s != site);
        self.render_cache.retain(|(s, _prod, _elev)| s != site);
    }

    /// The narrow counterpart to [`reset_panes_for_site`], for the real-time
    /// chunk feed: one elevation cut completed, not a whole volume.
    ///
    /// A pane showing another tilt is showing an image that is still correct,
    /// and resetting it costs more than a wasted render. `RenderInput::extract`
    /// answers `None` for a tilt the volume does not yet carry, which dispatches
    /// `Job::renders_nothing`; that unwinds the pane's in-flight mark but
    /// consumes a slot in the render budget, and it would happen for every
    /// unarrived tilt on every cut of every volume.
    ///
    /// `angles` are matched against each pane's **snapped** render elevation —
    /// what `get_rendering_params` resolves and what `last_rendered` records —
    /// not against `selected_elevation`, which may name a tilt no sweep carries.
    ///
    /// Volume-wide Level II products are skipped here and taken by
    /// [`reset_panes_for_volume`]: `EchoTopsInterpolated` integrates every
    /// reflectivity tilt, and NROT fits its wind profile from every velocity
    /// tilt, so both would read a partial volume as a complete short one.
    /// Level III panes are skipped outright — their pixels come from
    /// `level3_data`, which a Level II cut says nothing about.
    ///
    /// Returns how many panes were invalidated, for the log and the tests.
    pub fn reset_panes_for_tilts(
        &mut self,
        site: &str,
        gui: &rustdar_egui::Gui,
        angles: &[f32],
    ) -> usize {
        let hit = self.invalidate_panes_where(site, gui, |product, elevation| {
            if product.is_level3() || needs_whole_volume(product) {
                return false;
            }
            angles
                .iter()
                .any(|a| (a - elevation).abs() <= rustdar_egui::pane::ELEVATION_TOLERANCE)
        });
        // Only the tilts that changed. A whole-site `retain` would throw away the
        // images the untouched panes are still sharing.
        self.render_cache.retain(|(s, _prod, elev)| {
            s != site || !angles.iter().any(|a| elevation_key(*a) == *elev)
        });
        hit
    }

    /// The tilt-independent half: the Level II products that integrate a whole
    /// volume. Called only when a volume closes complete.
    pub fn reset_panes_for_volume(&mut self, site: &str, gui: &rustdar_egui::Gui) -> usize {
        self.invalidate_panes_where(site, gui, |product, _| {
            !product.is_level3() && needs_whole_volume(product)
        })
    }

    /// The one place the `abandon_results` + `render_in_flight` pairing is
    /// written for the narrow resets. Both public methods above go through it,
    /// so there is one new home for the invariant rather than two.
    fn invalidate_panes_where(
        &mut self,
        site: &str,
        gui: &rustdar_egui::Gui,
        mut want: impl FnMut(RadarProduct, f32) -> bool,
    ) -> usize {
        let mut hit = 0;
        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
            let matches = gui.pane(idx).is_some_and(|p| p.site == site)
                && gui
                    .get_rendering_params_for_pane(idx)
                    .is_some_and(|(product, elevation)| want(product, elevation));
            if matches {
                prs.last_rendered = None;
                prs.cached_render = None;
                prs.render_in_flight = false;
                // Paired with the line above: see `results_wanted`.
                prs.abandon_results();
                hit += 1;
            }
        }
        hit
    }

    /// Reset all pane render state (e.g. after a new scan loads).
    pub fn reset_panes(&mut self) {
        for prs in &mut self.pane_render {
            prs.last_rendered = None;
            prs.cached_render = None;
            prs.render_in_flight = false;
            prs.abandon_results();
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
    /// This site's current fetch generation, without bumping it.
    ///
    /// What a chunk round inherits: bumping would let a five-second tick
    /// supersede a manual navigation whose fetch is still in the air.
    pub fn fetch_generation_for(&self, site: &str) -> u64 {
        self.fetch_generations.get(site).copied().unwrap_or(0)
    }

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
    pub fn get_cached_render(
        &mut self,
        site: &str,
        product: RadarProduct,
        elevation: f32,
    ) -> Option<&CachedRenderOutput> {
        self.render_cache
            .get(&(site.to_string(), product, elevation_key(elevation)))
    }

    /// Store a render result in the cache for sharing across panes.
    pub fn cache_render(
        &mut self,
        site: &str,
        product: RadarProduct,
        elevation: f32,
        output: CachedRenderOutput,
    ) {
        self.render_cache.insert(
            (site.to_string(), product, elevation_key(elevation)),
            output,
        );
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
    /// The Level III product for `site` closest to `elevation`, matched on the
    /// **Product Description Block's** elevation angle rather than on the AWIPS
    /// mnemonic.
    ///
    /// Ties break on elevation number so a split cut or a SAILS/MRLE repeat,
    /// which share an angle, resolve to the same one every frame.
    fn nearest_tilt(
        &self,
        product: RadarProduct,
        site: &str,
        elevation: f32,
    ) -> Option<Arc<Level3Product>> {
        self.level3_data
            .iter()
            .filter(|((p, _tilt, s), _l3)| *p == product && s == site)
            .min_by(|(_, a), (_, b)| {
                let da = (a.message.pdb.elevation_angle() - elevation).abs();
                let db = (b.message.pdb.elevation_angle() - elevation).abs();
                da.partial_cmp(&db)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(
                        a.message
                            .pdb
                            .elevation_number
                            .cmp(&b.message.pdb.elevation_number),
                    )
            })
            .map(|(_, msg)| Arc::clone(msg))
    }

    /// Record on `pane` how old the Level III object behind `render` is, so the
    /// status bar can say. `None` for a Level II product, which has no
    /// `ProductStamp` and whose age is the scan time the bar already shows.
    ///
    /// Three values have to agree for the answer to mean anything — the
    /// product and elevation of *this* render, and the site of *this* pane —
    /// and they are read here rather than by the caller, which is what makes a
    /// pane that took this image from a sibling's broadcast report the image's
    /// age rather than whatever it was showing before.
    ///
    /// Assigned unconditionally, so switching a pane to a Level II product
    /// clears the last Level III object's age rather than leaving it captioning
    /// a field it does not describe.
    ///
    /// Resolved through [`nearest_tilt`](Self::nearest_tilt) — the same
    /// selection the render was spawned from — rather than being handed to
    /// `spawn_render` and carried back up the render thread. A value that is
    /// only *passed along* cannot be tested at the point it is passed, and
    /// `try_spawn_level3_render` has no test callers by design — the same
    /// reasoning that keeps the storm motion override a field read here
    /// rather than an argument (see `storm_motion_override_kt`).
    ///
    /// The cost is one render's worth of latency in the other direction: if a
    /// newer object for this tilt lands while the render is in flight, this
    /// reports the newer stamp for the frame or two before the re-render it
    /// triggered arrives. `poll_level3_results` clears `last_rendered` for
    /// every pane on the site, so that re-render is already queued.
    pub fn stamp_pane_with_product_age(
        &self,
        pane: &mut rustdar_egui::pane::PaneState,
        render: &CachedPaneRender,
    ) {
        pane.level3_time = self
            .nearest_tilt(render.product, &pane.site, render.elevation)
            .and_then(|tilt| tilt.stamp.time);
    }

    /// The storm motion override as the `(speed_kt, direction_deg)` pair the
    /// Level II render parameters carry, or `None` — Bunkers applies.
    ///
    /// Read from [`last_storm_motion_override`](Self::last_storm_motion_override),
    /// the same field [`set_storm_motion_override`](Self::set_storm_motion_override)
    /// invalidates on, so the vector a pane is invalidated for cannot differ
    /// from the one it is drawn with.
    pub(crate) fn storm_motion_override_kt(&self) -> Option<(f32, f32)> {
        self.last_storm_motion_override
            .map(|s| (s.motion.speed_kt, s.motion.direction_deg))
    }

    /// The environmental heights a Level II render's parameters carry: the
    /// site's `(0 °C, −20 °C)` pair in km MSL for the hail products, `None`
    /// for every other product — and `None` when no sounding has landed,
    /// which the hail render answers by drawing nothing
    /// ([`rustdar_radar::hail`]).
    ///
    /// Read from [`env_heights`](Self::env_heights), the same map
    /// [`set_env_heights`](Self::set_env_heights) invalidates on, so the
    /// environment a pane is invalidated for cannot differ from the one it is
    /// drawn with.
    pub(crate) fn env_heights_km_msl_for(
        &self,
        product: RadarProduct,
        site: &str,
    ) -> Option<(f64, f64)> {
        matches!(
            product,
            RadarProduct::ProbabilityOfSevereHail
                | RadarProduct::MaxExpectedHailSize
                | RadarProduct::HydrometeorClassification
        )
        .then(|| {
            self.env_heights
                .get(site)
                .map(|h| (h.h0c_km_msl, h.hm20c_km_msl))
        })
        .flatten()
    }

    /// Spawn a Level III render for a pane if applicable.
    /// Returns `true` if a render was spawned.
    ///
    /// Storm-relative velocity never comes through here any more: it is a
    /// Level II product, derived where the Level II render runs — see
    /// [`spawn_level2_render`](Self::spawn_level2_render) and
    /// [`rustdar_radar::srv`].
    pub fn try_spawn_level3_render(
        &mut self,
        pane_idx: usize,
        params: &RenderParams,
        site: &str,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
    ) -> bool {
        let Some(l3_msg) = self.nearest_tilt(params.product, site, params.elevation) else {
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
        self.spawn_render(
            pane_idx,
            params.product,
            params.elevation,
            sender,
            window,
            // The product's bytes rather than its decoded form: a
            // `Level3Message` has no wire form, and re-decoding is cheap against
            // the render it precedes — so on the web the decode moves off the
            // main thread with it.
            crate::offload::Job::Described(crate::offload::JobRequest::Level3 {
                bytes: std::sync::Arc::clone(&l3_msg.bytes),
                product,
                radar_lat: lat,
                radar_lon: lon,
            }),
        );
        true
    }

    /// Spawn a Level II render for a pane. `site` names the pane's radar for
    /// the per-site render parameters; the projection geometry still comes
    /// from `params`.
    pub fn spawn_level2_render(
        &mut self,
        pane_idx: usize,
        params: &RenderParams,
        site: &str,
        data: Arc<nexrad_model::data::Scan>,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
    ) {
        let product = params.product;
        let elevation = params.elevation;
        let lat = params.lat;
        let lon = params.lon;
        // The storm motion override rides the render parameters for the one
        // product that reads it. Read here, from the field the invalidation
        // reads, not passed by the caller — `dispatch_pane_renders` has no
        // test callers, so an argument it merely forwarded would be untested
        // by construction (the lesson the old Level III path's
        // `storm_motion_for` note recorded).
        let storm_motion = (product == RadarProduct::StormRelativeVelocity)
            .then(|| self.storm_motion_override_kt())
            .flatten();
        // The environmental heights ride the same way for the hail pair and
        // the classification, read from the field `set_env_heights`
        // invalidates on. A missing or stale-kept entry means the product
        // runs on its adaptation defaults, which is the documented
        // no-sounding behavior, not an error.
        let env_heights = self.env_heights_km_msl_for(product, site);
        log::info!(
            "Spawning background render for pane {}: {:?} at {:.1}°",
            pane_idx,
            product,
            elevation
        );
        // Extracted here, against the volume, because the volume is the thing
        // that must not travel: a decoded `Scan` is tens of megabytes and a
        // `RenderInput` is the one sweep the renderer actually reads.
        //
        // `None` means no sweep carries this product, which is exactly what the
        // renderer would have answered — so the job is dispatched anyway and
        // answers nothing, leaving the in-flight bookkeeping to unwind the way
        // a failed render always has.
        let job = match rustdar_radar::render_input::RenderInput::extract(
            &data,
            elevation,
            product,
            lat,
            lon,
            storm_motion,
            env_heights,
        ) {
            Some(input) => {
                crate::offload::Job::Described(crate::offload::JobRequest::Radar {
                    input: Box::new(input),
                    // A static pane keeps the grid: it is what a hover reads.
                    values_wanted: true,
                })
            }
            None => crate::offload::Job::renders_nothing(),
        };
        self.spawn_render(pane_idx, product, elevation, sender, window, job);
    }

    /// Shared dispatch for both Level II and Level III renders.
    ///
    /// The tail below — the guard, the cancellation check, the send and the
    /// redraw — is handed to the funnel as `deliver` rather than written into
    /// the job. That is what lets the Level II arm run in a browser worker
    /// without a second copy of it: `deliver` runs on this thread wherever the
    /// rasterization happened, and holds the two things that must not outlive
    /// the render either way.
    fn spawn_render(
        &mut self,
        pane_idx: usize,
        product: RadarProduct,
        elevation: f32,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
        job: crate::offload::Job,
    ) {
        // Check concurrent render limit
        let current = self.renders_in_flight.load(Ordering::Relaxed);
        if current >= MAX_CONCURRENT_RENDERS {
            return;
        }
        self.renders_in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(Arc::clone(&self.renders_in_flight));

        let generation = self.render_generation;
        // Cleared if this pane's data changes while the render runs, which is
        // where a per-site reset stops a result — the global `generation` above
        // cannot, since it says nothing about which site a result belongs to.
        //
        // `deliver` carries the only other reference to it, which is also what
        // `want_result`'s `Arc::strong_count` pruning reads as "still running".
        let wanted = self.pane_render[pane_idx].want_result();
        crate::offload::offload_job("radar-render", job, move |frame| {
            let _guard = guard;
            // Sent whether or not there is a frame, because the receiver is what
            // clears `render_in_flight` and a pane that never hears back stops
            // dispatching. Still gated on `wanted`: an abandoned render must not
            // clear the flag belonging to the render that superseded it.
            if wanted.load(Ordering::Relaxed) {
                let _ = sender.send(RenderResponse {
                    rendered: frame.map(|frame| crate::channels::RenderedImage {
                        image_data: Arc::new(frame.image),
                        max_range_km: frame.max_range_km,
                        value_data: Arc::new(frame.values),
                    }),
                    product,
                    elevation,
                    generation,
                    pane_idx,
                });
            }
            crate::app::notify_redraw(&window);
        });
        self.pane_render[pane_idx].render_in_flight = true;
    }
}

#[cfg(test)]
mod level3_dispatch_tests {
    use super::*;
    use nexrad_level3::model::{
        DataLayer, DataPacket, Level3Message, MessageHeader, ProductDescriptionBlock, RadialPacket,
        RadialRun, SymbologyBlock,
    };
    use rustdar_radar::level3::ProductStamp;

    /// A minimal Level III product: enough PDB for tilt selection plus one
    /// radial so a render would have something to work on.
    fn product(product_code: i16, elevation_tenths: i16, elevation_number: u16) -> Level3Product {
        let pdb = ProductDescriptionBlock {
            block_divider: -1,
            latitude: 44.849,
            longitude: -93.565,
            height: 1000,
            product_code,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 39,
            volume_scan_date: 20661,
            volume_scan_time: 7108,
            generation_date: 20661,
            generation_time: 7108,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number,
            product_specific_3: elevation_tenths,
            thresholds: [0u16; 16],
            product_specific_47_53: [0; 7],
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        };
        Level3Product {
            message: Level3Message {
                header: MessageHeader {
                    message_code: product_code,
                    date_of_message: 20661,
                    time_of_message: 7108,
                    message_length: 0,
                    source_id: 0,
                    destination_id: 0,
                    number_of_blocks: 3,
                },
                pdb,
                symbology: Some(SymbologyBlock {
                    block_id: 1,
                    block_length: 0,
                    num_layers: 1,
                    layers: vec![DataLayer {
                        layer_length: 0,
                        packets: vec![DataPacket::DigitalRadial(RadialPacket {
                            first_range_bin: 0,
                            num_range_bins: 2,
                            i_center: 0,
                            j_center: 0,
                            scale_factor: 0.999,
                            is_legacy: false,
                            xdr_data_scale: None,
                            xdr_data_offset: None,
                            radials: vec![RadialRun {
                                start_angle: 0.0,
                                angle_delta: 1.0,
                                gate_values: vec![129, 140],
                            }],
                        })],
                    }],
                }),
            },
            stamp: ProductStamp::from_key("MPX_EET_2026_07_26_01_55_52"),
            // No render in these tests, so nothing decodes them.
            bytes: std::sync::Arc::new(Vec::new()),
        }
    }

    fn cache(d: &mut RenderDispatcher, p: RadarProduct, code: &str, site: &str, l3: Level3Product) {
        d.cache_level3(p, code.to_string(), site.to_string(), l3);
    }

    /// Every remaining Level III product resolves from its cache — none is
    /// filtered. Storm-relative velocity is deliberately absent: it is a
    /// Level II product now and never reaches this cache at all — and the
    /// hydrometeor classification joined it (the hybrid composite derives
    /// from Level II; see `rustdar_radar::hhc`).
    #[test]
    fn every_level3_product_resolves_from_its_cache() {
        let mut d = RenderDispatcher::new();
        for (radar_product, code, product_code) in [
            (RadarProduct::SpecificDifferentialPhase, "N0K", 163i16),
            (RadarProduct::EchoTops, "EET", 135),
            (RadarProduct::VerticallyIntegratedLiquid, "DVL", 134),
            (RadarProduct::PrecipitationRate, "DPR", 176),
        ] {
            let p = product(product_code, 5, 1);
            cache(&mut d, radar_product, code, "KMPX", p);
            let picked = d
                .nearest_tilt(radar_product, "KMPX", 0.5)
                .unwrap_or_else(|| panic!("{code} must render"));
            assert_eq!(picked.message.pdb.product_code, product_code);
        }
        assert!(
            d.nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", 0.5)
                .is_none(),
            "nothing was cached for SRV, and nothing ever is: it derives from Level II",
        );
    }

    /// Another site's products must never be borrowed. Both sites carry the
    /// same elevations, so a filter that dropped the site would still return
    /// something plausible.
    #[test]
    fn a_tilt_is_never_taken_from_another_site() {
        let mut d = RenderDispatcher::new();
        cache(
            &mut d,
            RadarProduct::EchoTops,
            "EET",
            "KMPX",
            product(135, 5, 1),
        );
        let mut other = product(135, 5, 1);
        other.message.pdb.volume_scan_time = 9999;
        cache(&mut d, RadarProduct::EchoTops, "EET", "KFSD", other);

        let picked = d
            .nearest_tilt(RadarProduct::EchoTops, "KFSD", 0.5)
            .expect("KFSD has an EET");
        assert_eq!(
            picked.message.pdb.volume_scan_time, 9999,
            "took KMPX's product"
        );
        assert!(
            d.nearest_tilt(RadarProduct::EchoTops, "KABR", 0.5)
                .is_none()
        );
        assert!(
            d.nearest_tilt(RadarProduct::PrecipitationRate, "KMPX", 0.5)
                .is_none()
        );
    }

    /// Two cached objects at one angle resolve deterministically: the angle
    /// alone leaves the choice to hash order, so the tie breaks on elevation
    /// number.
    ///
    /// Asserted across **freshly built maps**, not repeated calls on one map:
    /// `std`'s `RandomState` re-seeds per `HashMap` instance, so one map
    /// iterates in the same order every time and a stability loop over it
    /// cannot see the tie-break at all.
    #[test]
    fn two_cuts_at_one_angle_resolve_the_same_way_every_time() {
        for round in 0..60 {
            let mut d = RenderDispatcher::new();
            let mut cuts = [("EE1", 9u16), ("EE2", 3)];
            if round % 2 == 1 {
                cuts.reverse();
            }
            for (code, elev_num) in cuts {
                let p = product(135, 13, elev_num);
                cache(&mut d, RadarProduct::EchoTops, code, "KMPX", p);
            }
            let picked = d
                .nearest_tilt(RadarProduct::EchoTops, "KMPX", 1.3)
                .expect("both cuts are at 1.3°")
                .message
                .pdb
                .elevation_number;
            assert_eq!(
                picked, 3,
                "round {round}: the lower cut number must break the tie, or the pane \
                 shows whichever cut the hash happened to yield",
            );
        }
    }

    /// A pane-sized render of `product` at `elevation`. Only the two fields
    /// the age lookup reads carry anything.
    fn rendered(product: RadarProduct, elevation: f32) -> CachedPaneRender {
        CachedPaneRender {
            image_data: Arc::new(Vec::new()),
            max_range_km: 230.0,
            value_data: Arc::new(Vec::new()),
            product,
            elevation,
        }
    }

    /// A pane on `site`, as `apply_render_to_pane` hands one over.
    fn pane_on(site: &str) -> rustdar_egui::pane::PaneState {
        rustdar_egui::pane::PaneState::with_site(site.to_string())
    }

    /// The age a pane is stamped with belongs to the object it is showing:
    /// its own site's, cleared for a Level II product — and storm-relative
    /// velocity **is** a Level II product now, so a pane switching to it
    /// must lose the Level III age rather than caption a derived field with
    /// some other product's stamp.
    #[test]
    fn a_render_stamps_its_pane_with_its_own_sites_product_age() {
        let mut d = RenderDispatcher::new();
        cache(
            &mut d,
            RadarProduct::EchoTops,
            "EET",
            "KMPX",
            product(135, 5, 1),
        );

        let mut pane = pane_on("KMPX");
        d.stamp_pane_with_product_age(&mut pane, &rendered(RadarProduct::EchoTops, 0.5));
        assert!(pane.level3_time.is_some(), "the EET stamp is readable");

        let mut elsewhere = pane_on("KTLX");
        d.stamp_pane_with_product_age(&mut elsewhere, &rendered(RadarProduct::EchoTops, 0.5));
        assert_eq!(
            elsewhere.level3_time, None,
            "another site's products are not this pane's",
        );

        let mut srv = pane_on("KMPX");
        d.stamp_pane_with_product_age(&mut srv, &rendered(RadarProduct::EchoTops, 0.5));
        assert!(srv.level3_time.is_some(), "precondition: it was dated");
        d.stamp_pane_with_product_age(
            &mut srv,
            &rendered(RadarProduct::StormRelativeVelocity, 0.5),
        );
        assert_eq!(
            srv.level3_time, None,
            "SRV derives from the Level II volume; no bucket object stands behind it",
        );
    }

    /// A key whose tail does not parse is an **unknown** age, not a fresh one.
    #[test]
    fn an_unreadable_key_reports_no_age_rather_than_a_wrong_one() {
        let mut d = RenderDispatcher::new();
        let mut p = product(135, 5, 1);
        p.stamp = ProductStamp::from_key("not-a-key");
        cache(&mut d, RadarProduct::EchoTops, "EET", "KMPX", p);

        assert!(
            d.nearest_tilt(RadarProduct::EchoTops, "KMPX", 0.5)
                .is_some(),
            "precondition: the product is still drawn — an unreadable key is worth \
             rendering, just not worth dating",
        );
        let mut pane = pane_on("KMPX");
        d.stamp_pane_with_product_age(&mut pane, &rendered(RadarProduct::EchoTops, 0.5));
        assert_eq!(pane.level3_time, None);
    }

    /// The override wins, routes into the Level II render parameters, and
    /// only for the product that reads it.
    ///
    /// `storm_motion_override_kt` is the exact value `spawn_level2_render`
    /// hands `RenderInput::extract`, read from the same field
    /// `set_storm_motion_override` invalidates on — so the vector a pane is
    /// invalidated for cannot differ from the one it is redrawn with.
    #[test]
    fn the_override_routes_into_the_level2_render_params() {
        let mut d = RenderDispatcher::new();
        assert_eq!(d.storm_motion_override_kt(), None, "no override, Bunkers");

        d.set_storm_motion_override(Some(
            StormMotionSample::user_override(45.0, 210.0).expect("finite"),
        ));
        assert_eq!(d.storm_motion_override_kt(), Some((45.0, 210.0)));

        d.set_storm_motion_override(None);
        assert_eq!(d.storm_motion_override_kt(), None);
    }

    /// Editing the vector changes nothing else about a pane, so both the
    /// per-pane state and the shared render cache have to be dropped by hand.
    #[test]
    fn changing_the_override_invalidates_the_storm_relative_renders() {
        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(3);
        d.pane_render[0].last_rendered = Some((RadarProduct::StormRelativeVelocity, 1.3));
        d.pane_render[1].last_rendered = Some((RadarProduct::Reflectivity, 0.5));
        d.pane_render[2].last_rendered = Some((RadarProduct::StormRelativeVelocity, 2.4));
        d.cache_render("KMPX", RadarProduct::StormRelativeVelocity, 1.3, output());
        d.cache_render("KMPX", RadarProduct::Reflectivity, 0.5, output());

        assert!(d.set_storm_motion_override(Some(
            StormMotionSample::user_override(30.0, 240.0).expect("finite")
        )));
        assert_eq!(d.pane_render[0].last_rendered, None);
        assert_eq!(
            d.pane_render[1].last_rendered,
            Some((RadarProduct::Reflectivity, 0.5)),
            "an unrelated product must not be re-rendered",
        );
        assert_eq!(d.pane_render[2].last_rendered, None);
        assert!(
            d.get_cached_render("KMPX", RadarProduct::StormRelativeVelocity, 1.3)
                .is_none(),
            "the shared cache is keyed on (site, product, elevation), which the vector is \
             not part of, so a stale entry would be handed straight back",
        );
        assert!(
            d.get_cached_render("KMPX", RadarProduct::Reflectivity, 0.5)
                .is_some()
        );
    }

    /// Re-applying the same override must not invalidate anything, or every
    /// frame re-renders every storm-relative pane.
    #[test]
    fn an_unchanged_override_invalidates_nothing() {
        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);
        let o = Some(StormMotionSample::user_override(30.0, 240.0).expect("finite"));
        assert!(d.set_storm_motion_override(o));
        d.pane_render[0].last_rendered = Some((RadarProduct::StormRelativeVelocity, 1.3));
        assert!(!d.set_storm_motion_override(o));
        assert_eq!(
            d.pane_render[0].last_rendered,
            Some((RadarProduct::StormRelativeVelocity, 1.3))
        );
        // Turning it back off is a change again.
        assert!(d.set_storm_motion_override(None));
        assert_eq!(d.pane_render[0].last_rendered, None);
    }

    fn output() -> CachedRenderOutput {
        CachedRenderOutput {
            image_data: Arc::new(Vec::new()),
            max_range_km: 230.0,
            value_data: Arc::new(Vec::new()),
        }
    }
}

#[cfg(test)]
mod render_cache_tests {
    use super::*;

    fn key(site: &str, elevation_tenths: i32) -> RenderCacheKey {
        (
            site.to_string(),
            RadarProduct::Reflectivity,
            elevation_tenths,
        )
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

        assert!(
            cache.get(&key("KTLX", 0)).is_some(),
            "the read should have saved it"
        );
        assert!(
            cache.get(&key("KTLX", 1)).is_none(),
            "untouched since insert, so it goes"
        );
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
                panic!(
                    "{site} was evicted with only {} panes' worth cached",
                    sites.len()
                );
            };
            assert_eq!(
                hit.max_range_km, i as f64,
                "{site} came back as another pane's render"
            );
        }
    }
}

#[cfg(test)]
mod render_invalidation_tests {
    use super::*;
    use std::sync::mpsc;

    /// A render that does not finish until the test releases it.
    ///
    /// The gate is the whole point: a reset only has something to act on while a
    /// render is *running*, and a render of nothing would routinely finish before
    /// the reset landed, so the test would pass on timing rather than on the
    /// abandonment.
    ///
    /// Deliberately a `Job::Opaque`: it has to *block*, and a described job is
    /// executed by the funnel with no handle to hold it open. What is under
    /// test is the abandonment protocol around a running render, which is the
    /// same for both job shapes — `deliver` carries the flag either way.
    fn gated_render() -> (mpsc::Sender<()>, crate::offload::Job) {
        let (release, held) = mpsc::channel::<()>();
        (
            release,
            crate::offload::Job::Opaque(Box::new(move || {
                held.recv().expect("every gated render is released");
                Some((Vec::new(), 230.0, Vec::new()).into())
            })),
        )
    }

    /// [`gated_render`] for a render that answers nothing — what
    /// `Job::renders_nothing` produces when no sweep carries the product, held
    /// open so the abandonment protocol can be exercised around it.
    fn gated_nothing() -> (mpsc::Sender<()>, crate::offload::Job) {
        let (release, held) = mpsc::channel::<()>();
        (
            release,
            crate::offload::Job::Opaque(Box::new(move || {
                held.recv().expect("every gated render is released");
                None
            })),
        )
    }

    /// One pane, on `site`, which is how `reset_panes_for_site` reads the layout.
    fn gui_showing(site: &str) -> rustdar_egui::Gui {
        let mut gui = rustdar_egui::Gui::new();
        gui.pane_mut(0).expect("a fresh Gui has one pane").site = site.to_string();
        gui
    }

    /// The environmental heights route into the hail render parameters from
    /// the same map the sounding drain writes, and a moved pair drops exactly
    /// that site's hail renders — the per-site sibling of
    /// `changing_the_override_invalidates_the_storm_relative_renders`.
    #[test]
    fn a_landed_sounding_routes_into_hail_renders_and_a_moved_pair_drops_them() {
        let heights = |h0: f64, hm20: f64| rustdar_radar::sounding::EnvHeights {
            h0c_km_msl: h0,
            hm20c_km_msl: hm20,
            fetched_at: chrono::Utc::now(),
        };
        let mut d = RenderDispatcher::new();
        let gui = gui_showing("KTLX");
        d.ensure_pane_count(1);

        assert_eq!(
            d.env_heights_km_msl_for(RadarProduct::ProbabilityOfSevereHail, "KTLX"),
            None,
            "before any sounding lands the render must draw nothing, not zeros",
        );
        assert!(
            d.set_env_heights("KTLX", heights(4.2, 7.1), &gui),
            "the first pair is a change from nothing",
        );
        assert_eq!(
            d.env_heights_km_msl_for(RadarProduct::MaxExpectedHailSize, "KTLX"),
            Some((4.2, 7.1)),
        );
        assert_eq!(
            d.env_heights_km_msl_for(RadarProduct::Reflectivity, "KTLX"),
            None,
            "only the hail pair reads the environment",
        );
        assert_eq!(
            d.env_heights_km_msl_for(RadarProduct::ProbabilityOfSevereHail, "KOUN"),
            None,
            "the environment is per-site",
        );

        d.pane_render[0].last_rendered = Some((RadarProduct::ProbabilityOfSevereHail, 0.5));
        d.cache_render("KTLX", RadarProduct::MaxExpectedHailSize, 0.5, cached(1.0));
        d.cache_render("KTLX", RadarProduct::Reflectivity, 0.5, cached(2.0));

        assert!(
            !d.set_env_heights("KTLX", heights(4.2, 7.1), &gui),
            "an identical refetch restarts the TTL and drops nothing",
        );
        assert_eq!(
            d.pane_render[0].last_rendered,
            Some((RadarProduct::ProbabilityOfSevereHail, 0.5)),
        );

        assert!(
            d.set_env_heights("KOUN", heights(1.0, 2.5), &gui),
            "another site's first sounding is a change there",
        );
        assert_eq!(
            d.pane_render[0].last_rendered,
            Some((RadarProduct::ProbabilityOfSevereHail, 0.5)),
            "another site's sounding must not touch this pane",
        );

        assert!(d.set_env_heights("KTLX", heights(4.4, 7.3), &gui));
        assert_eq!(
            d.pane_render[0].last_rendered, None,
            "a hail pane drawn against the old pair has to be redrawn",
        );
        assert!(
            d.get_cached_render("KTLX", RadarProduct::MaxExpectedHailSize, 0.5)
                .is_none(),
            "the shared cache is keyed on (site, product, elevation), which \
             the environment is not part of",
        );
        assert!(
            d.get_cached_render("KTLX", RadarProduct::Reflectivity, 0.5)
                .is_some(),
            "an unrelated product keeps its frame",
        );
    }

    fn dispatch(
        d: &mut RenderDispatcher,
        pane_idx: usize,
        results: &mpsc::Sender<RenderResponse>,
    ) -> mpsc::Sender<()> {
        let (release, render) = gated_render();
        d.spawn_render(
            pane_idx,
            RadarProduct::Reflectivity,
            0.5,
            results.clone(),
            None,
            render,
        );
        release
    }

    /// How many renders were not abandoned. Ends when the last worker drops its
    /// sender, so nothing here waits on a timeout.
    fn arrivals(
        results: mpsc::Sender<RenderResponse>,
        rx: mpsc::Receiver<RenderResponse>,
    ) -> usize {
        drop(results);
        rx.iter().count()
    }

    /// The defect: a scan arriving for one site bumped a single global generation,
    /// so every pane on every *other* site had its in-flight render discarded at
    /// the receiver and respawned — a 2048² image and value grid redone per pane
    /// per poll, recurring every interval in any multi-site layout.
    #[test]
    fn a_scan_for_one_site_leaves_another_sites_render_alone() {
        let gui = gui_showing("KOUN");
        let mut d = RenderDispatcher::new();
        let (results, rx) = mpsc::channel();
        let release = dispatch(&mut d, 0, &results);

        // A scan for the other site lands while the KOUN pane is still rendering.
        let generation = d.render_generation;
        d.reset_panes_for_site("KTLX", &gui);
        assert!(
            !d.is_render_stale(generation),
            "a per-site reset must not move the global generation — the receiver \
             compares every pane against it"
        );

        release.send(()).expect("the render is still running");
        assert_eq!(
            arrivals(results, rx),
            1,
            "the KOUN pane's render was thrown away for a KTLX scan"
        );
    }

    /// The other half: a scan for the pane's own site does invalidate it, or the
    /// pane paints the previous volume over the new one and then stops, since
    /// `last_rendered` records that render as the one it is showing.
    #[test]
    fn a_scan_for_the_panes_own_site_abandons_its_render() {
        let gui = gui_showing("KOUN");
        let mut d = RenderDispatcher::new();
        let (results, rx) = mpsc::channel();
        let release = dispatch(&mut d, 0, &results);

        d.reset_panes_for_site("KOUN", &gui);
        assert!(
            !d.pane_render[0].render_in_flight,
            "the pairing an abandoned send depends on: the pane must not be left \
             waiting for a result that will never come"
        );

        release.send(()).expect("the render is still running");
        assert_eq!(arrivals(results, rx), 0);
    }

    /// A pane can have more than one render running: the reset above clears
    /// `render_in_flight` while the first is still going, so the next dispatch
    /// starts a second. Abandoning only the newest would leave the older free to
    /// arrive last and paint the scan the reset was meant to replace.
    #[test]
    fn every_render_a_pane_has_running_is_abandoned_at_once() {
        let gui = gui_showing("KOUN");
        let mut d = RenderDispatcher::new();
        let (results, rx) = mpsc::channel();
        let first = dispatch(&mut d, 0, &results);
        let second = dispatch(&mut d, 0, &results);

        d.reset_panes_for_site("KOUN", &gui);

        second.send(()).expect("both renders are still running");
        first.send(()).expect("both renders are still running");
        assert_eq!(arrivals(results, rx), 0);
    }

    /// A full reset is site-blind by design — surface loss, a layout change — and
    /// keeps discarding everything.
    #[test]
    fn a_full_reset_abandons_every_panes_render() {
        let mut d = RenderDispatcher::new();
        let (results, rx) = mpsc::channel();
        let release = dispatch(&mut d, 0, &results);

        let generation = d.render_generation;
        d.reset_panes();
        assert!(
            d.is_render_stale(generation),
            "and the global generation still moves, so a result already in the \
             channel is discarded on arrival"
        );

        release.send(()).expect("the render is still running");
        assert_eq!(arrivals(results, rx), 0);
    }

    /// The lock-out this closes: a render that finds no sweep used to send
    /// nothing at all. `render_in_flight` is cleared by the receiver or by a
    /// reset and nowhere else, and `dispatch_pane_renders` refuses to dispatch
    /// while it is set — so the pane went quiet until something unrelated reset
    /// it, and a user changing product saw nothing happen.
    ///
    /// Rare against an archive volume, which carries every cut it will ever
    /// have. Routine against a volume still being assembled from the real-time
    /// chunk feed, where an upper tilt has simply not been scanned yet.
    #[test]
    fn a_render_that_finds_nothing_still_reports_back() {
        let mut d = RenderDispatcher::new();
        let (results, rx) = mpsc::channel();
        let (release, nothing) = gated_nothing();
        d.spawn_render(
            0,
            RadarProduct::Reflectivity,
            0.5,
            results.clone(),
            None,
            nothing,
        );

        release.send(()).expect("the render is still running");
        drop(results);
        let replies: Vec<_> = rx.iter().collect();
        assert_eq!(
            replies.len(),
            1,
            "a render with nothing to draw stayed silent, so its pane is still \
             marked in flight and will never dispatch again"
        );
        assert!(
            replies[0].rendered.is_none(),
            "there was no sweep to draw, but a frame arrived anyway"
        );
    }

    /// The counterweight, and the reason the report is gated on `results_wanted`
    /// rather than sent unconditionally: an abandoned render must stay silent.
    /// Reporting would clear `render_in_flight` for the render that *superseded*
    /// it, and the pane would dispatch a third while the second was still going.
    #[test]
    fn an_abandoned_render_that_finds_nothing_reports_nothing() {
        let gui = gui_showing("KOUN");
        let mut d = RenderDispatcher::new();
        let (results, rx) = mpsc::channel();
        let (release, nothing) = gated_nothing();
        d.spawn_render(
            0,
            RadarProduct::Reflectivity,
            0.5,
            results.clone(),
            None,
            nothing,
        );

        d.reset_panes_for_site("KOUN", &gui);

        release.send(()).expect("the render is still running");
        assert_eq!(arrivals(results, rx), 0);
    }

    /// One pane on `site` showing `product`, with `available` as the tilt list
    /// its selection snaps within.
    ///
    /// One pane rather than several because `Gui::set_pane_count_for_test` is
    /// `#[cfg(test)]` inside `rustdar-egui` and so does not exist for this
    /// crate's tests. The property under test — that a reset picks panes by
    /// their snapped tilt — is the same either way, and the pair of tests below
    /// covers both answers.
    fn gui_on_tilt(
        site: &str,
        product: RadarProduct,
        selected: f32,
        available: &[f32],
    ) -> rustdar_egui::Gui {
        use rustdar_radar::sites::RadarSite;
        use rustdar_radar::types::ScanInfo;
        let mut gui = rustdar_egui::Gui::new();
        let pane = gui.pane_mut(0).expect("a fresh Gui has one pane");
        pane.site = site.to_string();
        pane.selected_product = product;
        pane.selected_elevation = selected;
        let mut product_elevations = std::collections::HashMap::new();
        product_elevations.insert(product, available.to_vec());
        pane.scan_info = Some(ScanInfo {
            site: RadarSite {
                name: "KOUN",
                lat: 35.2,
                lon: -97.4,
                elev: None,
            },
            timestamp: chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            vcp_number: 212,
            available_products: vec![product],
            product_elevations,
            status: String::new(),
        });
        gui
    }

    fn cached(range: f64) -> CachedRenderOutput {
        CachedRenderOutput {
            image_data: Arc::new(Vec::new()),
            max_range_km: range,
            value_data: Arc::new(Vec::new()),
        }
    }

    /// The defect this avoids: a cut completing in the real-time feed changes one
    /// sweep, not the volume, so a pane on another tilt is still showing a
    /// correct image. Resetting it dispatches a render whose `extract` answers
    /// `None` — a wasted slot in the render budget, on every cut of every volume.
    #[test]
    fn a_finished_tilt_leaves_a_pane_on_another_tilt_alone() {
        let gui = gui_on_tilt("KOUN", RadarProduct::Reflectivity, 4.0, &[0.5, 4.0]);
        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);
        let (results, rx) = mpsc::channel();
        let release = dispatch(&mut d, 0, &results);

        assert_eq!(
            d.reset_panes_for_tilts("KOUN", &gui, &[0.5]),
            0,
            "the 4.0° pane was invalidated by a 0.5° cut completing"
        );
        assert!(d.pane_render[0].render_in_flight);

        release.send(()).expect("still running");
        assert_eq!(
            arrivals(results, rx),
            1,
            "its render should survive: the image it is showing is still correct"
        );
    }

    /// The counterweight: the pane whose tilt it was must be invalidated, or the
    /// new sweep never reaches the screen.
    #[test]
    fn a_finished_tilt_invalidates_the_pane_showing_it() {
        let gui = gui_on_tilt("KOUN", RadarProduct::Reflectivity, 0.5, &[0.5, 4.0]);
        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);
        let (results, rx) = mpsc::channel();
        let release = dispatch(&mut d, 0, &results);

        assert_eq!(d.reset_panes_for_tilts("KOUN", &gui, &[0.5]), 1);
        assert!(
            !d.pane_render[0].render_in_flight,
            "the pairing an abandoned send depends on"
        );
        release.send(()).expect("still running");
        assert_eq!(arrivals(results, rx), 0);
    }

    /// Echo tops integrates every reflectivity tilt and clamps each column to the
    /// topmost one present, so a partial volume gives a plausible, low, wrong
    /// number with no error and no NaN. It must wait for the volume to close.
    #[test]
    fn a_finished_tilt_leaves_the_volumetric_pane_for_the_closing_volume() {
        let gui = gui_on_tilt("KOUN", RadarProduct::EchoTopsInterpolated, 0.5, &[0.5]);
        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);

        assert_eq!(
            d.reset_panes_for_tilts("KOUN", &gui, &[0.5]),
            0,
            "echo tops was invalidated by a single cut completing"
        );
        assert_eq!(
            d.reset_panes_for_volume("KOUN", &gui),
            1,
            "and the closing volume did not pick it up"
        );
    }

    /// NROT fits its wind profile from every velocity tilt — the only wind
    /// source since the NVW fetch left — so it is volume-wide too, and only
    /// the closing volume refreshes it.
    #[test]
    fn nrot_waits_for_the_volume() {
        let gui = gui_on_tilt("KOUN", RadarProduct::NormalizedRotation, 0.5, &[0.5]);
        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);

        assert_eq!(
            d.reset_panes_for_tilts("KOUN", &gui, &[0.5]),
            0,
            "NROT fits its profile from every velocity tilt, so a partial \
             volume would halve its shear"
        );
        assert_eq!(d.reset_panes_for_volume("KOUN", &gui), 1);
    }

    /// A Level III pane's pixels come from `level3_data`; a Level II cut
    /// completing says nothing about them, and its tilts are refetched only when
    /// the volume closes.
    #[test]
    fn a_finished_tilt_does_not_touch_a_level3_pane() {
        let gui = gui_on_tilt(
            "KOUN",
            RadarProduct::VerticallyIntegratedLiquid,
            0.5,
            &[0.5],
        );
        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);
        assert_eq!(d.reset_panes_for_tilts("KOUN", &gui, &[0.5]), 0);
        assert_eq!(d.reset_panes_for_volume("KOUN", &gui), 0);
    }

    /// A whole-site `render_cache.retain` would throw away the images the panes
    /// this reset deliberately left alone are still sharing.
    #[test]
    fn a_tilt_reset_keeps_the_other_tilts_cached_renders() {
        let gui = gui_on_tilt("KOUN", RadarProduct::Reflectivity, 0.5, &[0.5, 4.0]);
        let mut d = RenderDispatcher::new();
        d.ensure_pane_count(1);
        d.cache_render("KOUN", RadarProduct::Reflectivity, 0.5, cached(1.0));
        d.cache_render("KOUN", RadarProduct::Reflectivity, 4.0, cached(2.0));

        d.reset_panes_for_tilts("KOUN", &gui, &[0.5]);
        assert!(
            d.get_cached_render("KOUN", RadarProduct::Reflectivity, 0.5)
                .is_none(),
            "the completed tilt's stale image survived"
        );
        assert!(
            d.get_cached_render("KOUN", RadarProduct::Reflectivity, 4.0)
                .is_some(),
            "an untouched tilt's image was evicted with it"
        );
    }

    /// The flag list is bounded by what is actually running, not by how many
    /// renders a session has dispatched.
    #[test]
    fn finished_renders_stop_being_tracked() {
        let mut d = RenderDispatcher::new();
        let (results, rx) = mpsc::channel();
        for _ in 0..5 {
            let release = dispatch(&mut d, 0, &results);
            release.send(()).expect("the render is still running");
            // The worker has to drop its flag before the next dispatch prunes.
            rx.recv().expect("an unabandoned render arrives");
        }
        // Each dispatch prunes before pushing, so only the render just added — and
        // at most one whose worker had not quite dropped its flag — can be held.
        assert!(
            d.pane_render[0].results_wanted.len() <= 2,
            "flags accumulated: {}",
            d.pane_render[0].results_wanted.len()
        );
    }
}
