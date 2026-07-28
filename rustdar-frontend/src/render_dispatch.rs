use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rustdar_radar::level3::Level3Product;
use rustdar_radar::render::{render_derived_srm_to_image, render_level3_message_to_image};
use rustdar_radar::srm::{self, StormMotionSample};
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

/// Whether a cached Level III product is one a pane can show.
///
/// Storm-relative velocity derives every tilt, 0.5° included, and fetches
/// `N0S` on top of the four for the storm motion vector in its Product
/// Description Block — no velocity product carries one, because halfword 51 is
/// the BZ2 compression flag there. `N0S` must never be *drawn*: it is 1 km at
/// the RPG's 16 display levels where the derived tilts are 0.25 km at 254, and
/// its gate values already have the RPG's own vector in them, so a storm motion
/// override cannot reach it.
///
/// Filtering here rather than in the elevation comparison because nothing in
/// that comparison could separate the two: at `TLX` the bucket's `N0S` and
/// `N0G` both report 0.5° and both report elevation number 1, so an unfiltered
/// search resolves the 0.5° pane to whichever of them the hash yielded.
fn is_renderable_tilt(product: RadarProduct, msg: &nexrad_level3::model::Level3Message) -> bool {
    product != RadarProduct::StormRelativeVelocity || srm::is_velocity_source(msg)
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

/// What to append to the SRM render log to qualify the vector it used.
///
/// A user override belongs to no volume, so neither volume claim applies to it
/// — it used to be annotated `(previous volume)`, which said the RPG had fitted
/// this vector for some earlier scan.
fn motion_provenance_suffix(provenance: srm::MotionProvenance) -> &'static str {
    match provenance {
        srm::MotionProvenance::SameVolume => "",
        srm::MotionProvenance::PreviousVolume => " (previous volume)",
        srm::MotionProvenance::UserOverride => " (user override)",
    }
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
    /// Latest VAD Wind Profile levels per site — (height km, u, v). NROT
    /// renders pass these to the winds-aware render entry so its dealiaser
    /// settles fold branches the volume alone cannot.
    pub vwp_levels: HashMap<String, Vec<(f64, f64, f64)>>,
    /// Environmental 0 °C / −20 °C heights per site, from Open-Meteo — staged
    /// for the hail products, which will read them at render time. Written by
    /// the sounding drain in `app_render`; read back by
    /// `spawn_level3_fetches`'s TTL gate, which refetches on poll only once
    /// [`rustdar_radar::sounding::EnvHeights::is_stale`] says the entry has
    /// aged out. Like [`vwp_levels`](Self::vwp_levels), survives both reset
    /// paths: the environment does not change because a pane was reset, and
    /// the TTL is the eviction policy.
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
    /// The storm motion override the derived SRM tilts on screen were built
    /// with. Nothing else about a pane changes when the user edits the vector,
    /// so without this the field would keep the old motion until the next fetch.
    last_storm_motion_override: Option<StormMotionSample>,
    /// The last few volumes' storm motion vectors per site, newest first.
    ///
    /// Only `N0S` carries a vector and only one object of it is ever cached, so
    /// without a history the four tilts get whichever volume's vector arrived
    /// last. That is not a boundary transient: `N0S` and `N0G` are published
    /// when the 0.5° cut finishes, `N1G`/`N2U`/`N3U` when their own cuts do, so
    /// for most of a volume the newest vector is a volume **ahead** of the
    /// upper tilts. Measured over 22 sites — see
    /// `rustdar_radar::srm`'s volume-pairing section — the newest vector
    /// belonged to another volume on 306 of 792 renders, and on the upper three
    /// tilts specifically 38-54%.
    ///
    /// Survives [`reset_panes_for_site`](Self::reset_panes_for_site), which
    /// runs on every poll; only a full [`reset_panes`](Self::reset_panes)
    /// clears it. Bounded to [`STORM_MOTION_HISTORY`] volumes per site.
    storm_motion_history: HashMap<String, VecDeque<StormMotionSample>>,
}

/// Volumes of storm motion kept per site. Four covers roughly twenty minutes of
/// VCP 212 — far more than the one-volume lag that produces almost every
/// mismatch — and the lookup is a linear scan of this many entries.
const STORM_MOTION_HISTORY: usize = 4;

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
            vwp_levels: HashMap::new(),
            env_heights: HashMap::new(),
            render_generation: 0,
            fetch_generations: HashMap::new(),
            // Owned here so there is exactly one render budget counter in the process.
            renders_in_flight: Arc::new(AtomicUsize::new(0)),
            render_cache: RenderCache::new(MAX_RENDER_CACHE_ENTRIES),
            last_storm_motion_override: None,
            storm_motion_history: HashMap::new(),
        }
    }

    /// Cache a fetched Level III product, recording its storm motion vector
    /// against its volume if it carries one.
    ///
    /// The only way into [`level3_data`](Self::level3_data). The map keeps one
    /// object per `(product, tilt, site)`, so an `N0S` that is not captured here
    /// is gone the moment the next volume's arrives — and the next volume's is
    /// the wrong one for three of the four tilts most of the time.
    pub fn cache_level3(
        &mut self,
        product: RadarProduct,
        tilt_code: String,
        site: String,
        fetched: Level3Product,
    ) {
        if let Some(sample) = StormMotionSample::from_message(&fetched.message) {
            let history = self.storm_motion_history.entry(site.clone()).or_default();
            if !history.iter().any(|s| s.volume == sample.volume) {
                history.push_back(sample);
                // Sorted rather than assumed: objects do not always arrive in
                // volume order, and both the fallback (the front) and the
                // eviction (the back) depend on newest-first holding.
                history
                    .make_contiguous()
                    .sort_by_key(|s| std::cmp::Reverse(s.volume));
                history.truncate(STORM_MOTION_HISTORY);
            }
        }
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
    /// All four tilts, 0.5° included. While that tilt rendered `N0S` directly
    /// this invalidated it to redraw a byte-identical image, because the RPG's
    /// vector was already in the gate values.
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
        // `storm_motion_history` deliberately survives. This runs on **every**
        // auto-poll for a live pane, immediately before the five products are
        // refetched, so clearing it here would leave the history holding only
        // the volume the newest `N0S` came from — which is the volume the upper
        // tilts have *not* reached, and the pairing the history exists to fix.
        // Nothing evicts a site, so the map grows with sites visited: four
        // samples of ~40 bytes plus the key, call it 150 bytes **per site**,
        // and ~160 NEXRAD sites exist, so ~24 KB is the ceiling for a session
        // that visited every radar in the country. A vector that no longer
        // matches any volume is never selected, so keeping them is inert.
        self.render_cache.retain(|(s, _prod, _elev)| s != site);
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
        self.storm_motion_history.clear();
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
    /// mnemonic — `N1G` is 1.3° in VCP 212, not the 1.5° its name suggests.
    ///
    /// Ties break on elevation number so a split cut or a SAILS/MRLE repeat,
    /// which share an angle, resolve to the same one every frame.
    ///
    /// Products that are cached but not displayable are filtered out first; see
    /// [`is_renderable_tilt`].
    fn nearest_tilt(
        &self,
        product: RadarProduct,
        site: &str,
        elevation: f32,
    ) -> Option<Arc<Level3Product>> {
        self.level3_data
            .iter()
            .filter(|((p, _tilt, s), l3)| {
                *p == product && s == site && is_renderable_tilt(product, &l3.message)
            })
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
    /// `try_spawn_level3_render` has no test callers by design; see its note,
    /// and `storm_motion_for`'s.
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

    /// The storm motion vector to apply to `velocity`: the user's if they set
    /// one, otherwise the vector belonging to the volume `velocity` itself came
    /// from, and only failing that the newest one seen. Every tilt goes through
    /// here, so an override reaches all four.
    ///
    /// The RPG re-fits the SCIT average every volume, and all four tilts of a
    /// volume share the fit — so pairing a velocity product with another
    /// volume's vector is simply wrong, not merely stale. It is also the normal
    /// case rather than a boundary race: `N0S` is published with the 0.5° cut
    /// and the upper tilts a cut or more later, so for most of a volume the
    /// newest vector belongs to a volume the upper tilts have not reached. See
    /// [`storm_motion_history`](Self::storm_motion_history).
    ///
    /// The fallback stays because the alternative is a blank pane: at 0.5° the
    /// SAILS repeat can arrive before the volume's `N0S` exists at all, and a
    /// vector one volume out beats no storm-relative velocity.
    ///
    /// Takes the whole message and reads the volume off it, and reads the
    /// override from `self`. Both were once values the caller worked out and
    /// passed in, and the caller is `try_spawn_level3_render` — one production
    /// call site, no test call sites. A mutant that replaced the volume key
    /// with a constant, or dropped the override argument, therefore compiled
    /// and passed the entire suite while disabling the feature in production;
    /// both were confirmed to survive. Argument passing can only be tested from
    /// the outermost caller, so the fix is to have no argument to pass: both
    /// reads now sit inside a unit the tests reach.
    ///
    /// Reading [`last_storm_motion_override`](Self::last_storm_motion_override)
    /// also makes it structurally impossible for the vector a pane is
    /// *invalidated* for to differ from the one it is *drawn* with: both now
    /// come from the same field.
    fn storm_motion_for(
        &self,
        site: &str,
        velocity: &nexrad_level3::model::Level3Message,
    ) -> Option<StormMotionSample> {
        if self.last_storm_motion_override.is_some() {
            return self.last_storm_motion_override;
        }
        // `Some(..)`, not a bare key: `StormMotionSample::volume` is `None` for
        // a user override, and an override must never be selected here as if it
        // were some volume's RPG fit. Only `from_message` samples are recorded,
        // so in practice every entry carries a key — but the comparison says
        // which of the two it means.
        let volume = Some(velocity.pdb.volume_key());
        let history = self.storm_motion_history.get(site)?;
        history
            .iter()
            .find(|s| s.volume == volume)
            .or_else(|| history.front())
            .copied()
    }

    /// Spawn a Level III render for a pane if applicable.
    /// Returns `true` if a render was spawned.
    ///
    /// No storm-relative velocity tilt is rendered from a product on the wire;
    /// all four are computed here from dealiased velocity. See
    /// [`rustdar_radar::srm`].
    ///
    /// The storm motion override is **not** a parameter: it is read from the
    /// dispatcher by [`storm_motion_for`](Self::storm_motion_for), which is
    /// where the volume key is read too. This function has no test callers, so
    /// anything it merely forwards is untested by construction — see
    /// `storm_motion_for`'s note. Keep it that way: give this function values
    /// to act on, not values to pass along.
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

        if srm::is_velocity_source(&l3_msg.message) {
            let Some(sample) = self.storm_motion_for(site, &l3_msg.message) else {
                // No `N0S` yet. Rendering the velocity field raw would paint a
                // base-velocity couplet under a storm-relative label.
                log::debug!(
                    "Pane {pane_idx}: {:.1}° SRM waiting on a storm motion vector",
                    params.elevation,
                );
                return false;
            };
            let Some(derived) = srm::derive(&l3_msg.message, &sample) else {
                return false;
            };
            log::info!(
                "Spawning derived SRM render for pane {pane_idx}: {:.1}° (elevation {}) from \
                 product {}, {:.1} kt from {:.1}°{}",
                derived.elevation_angle,
                derived.elevation_number,
                l3_msg.message.pdb.product_code,
                derived.motion.speed_kt,
                derived.motion.direction_deg,
                motion_provenance_suffix(derived.motion_provenance),
            );
            self.spawn_render(
                pane_idx,
                product,
                params.elevation,
                sender,
                window,
                crate::offload::Job::Opaque(Box::new(move || {
                    render_derived_srm_to_image(&derived, lat, lon).map(Into::into)
                })),
            );
            return true;
        }

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
            crate::offload::Job::Opaque(Box::new(move || {
                render_level3_message_to_image(&l3_msg.message, product, lat, lon).map(Into::into)
            })),
        );
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
        winds: Option<Vec<(f64, f64, f64)>>,
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
            winds.as_deref(),
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
            if let Some(frame) = frame
                && wanted.load(Ordering::Relaxed)
            {
                let _ = sender.send(RenderResponse {
                    image_data: Arc::new(frame.image),
                    max_range_km: frame.max_range_km,
                    value_data: Arc::new(frame.values),
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
mod srm_dispatch_tests {
    use super::*;
    use nexrad_level3::model::{
        DataLayer, DataPacket, Level3Message, MessageHeader, ProductDescriptionBlock, RadialPacket,
        RadialRun, SymbologyBlock,
    };
    use rustdar_radar::level3::ProductStamp;

    /// A minimal Level III product: enough PDB for tilt selection, storm motion
    /// and derivation, plus one radial so `derive` has something to work on.
    fn product(
        product_code: i16,
        elevation_tenths: i16,
        elevation_number: u16,
        volume_time: u32,
        ps47_53: [i16; 7],
    ) -> Level3Product {
        let mut thresholds = [0u16; 16];
        // Halfwords 31-33 of a real N1G: -63.5 m/s, 0.5 m/s, 254 levels.
        thresholds[0] = -635i16 as u16;
        thresholds[1] = 5;
        thresholds[2] = 254;
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
            volume_scan_time: volume_time,
            generation_date: 20661,
            generation_time: volume_time,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number,
            product_specific_3: elevation_tenths,
            thresholds,
            product_specific_47_53: ps47_53,
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
                    time_of_message: volume_time,
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
            stamp: ProductStamp::from_key("MPX_N1G_2026_07_26_01_55_52"),
        }
    }

    /// Cache a fixture the way production does. Going through
    /// [`RenderDispatcher::cache_level3`] rather than touching `level3_data`
    /// directly is what populates the storm motion history, so a test that
    /// inserted straight into the map would find no vector at all.
    fn cache(d: &mut RenderDispatcher, p: RadarProduct, code: &str, site: &str, l3: Level3Product) {
        d.cache_level3(p, code.to_string(), site.to_string(), l3);
    }

    /// The volume every fixture product belongs to.
    const VOLUME: (u16, u32) = (20661, 7108);

    /// A velocity message from `volume_time`, for asking which vector applies
    /// to it. `storm_motion_for` reads the volume off the message rather than
    /// taking one from the caller, so a test that passed a bare key would leave
    /// the extraction — the wiring into production — unexercised.
    fn velocity_from(volume_time: u32) -> Level3Message {
        product(154, 13, 3, volume_time, VELOCITY_PS).message
    }

    /// Halfwords 47-53 of a real N0S: 25.7 kt from 296.1°, SCIT average.
    const N0S_PS: [i16; 7] = [-109, 76, -1, 7663, 257, 2961, 0];
    /// Halfwords 47-53 of a real N1G. Halfword 51 is the BZ2 compression flag.
    const VELOCITY_PS: [i16; 7] = [-93, 74, 0, 8097, 1, 13, 16382];

    /// The five keys one site's SRM fetch produces: the four derived tilts at
    /// the elevations VCP 212 really produces, plus `N0S`, which is fetched for
    /// its vector and must never be drawn.
    ///
    /// `N0S` and `N0G` are given the **same** 0.5° and the same elevation
    /// number 1, which is what `TLX` really publishes — so nothing in the
    /// nearest-angle search or its tie-break can separate them, and only
    /// `is_renderable_tilt` can.
    const SRM_FIXTURE: [(&str, i16, i16, u16, [i16; 7]); 5] = [
        ("N0S", 56, 5, 1, N0S_PS),
        ("N0G", 154, 5, 1, VELOCITY_PS),
        ("N1G", 154, 13, 3, VELOCITY_PS),
        ("N2U", 99, 24, 5, VELOCITY_PS),
        ("N3U", 99, 31, 6, VELOCITY_PS),
    ];

    fn loaded() -> RenderDispatcher {
        let mut d = RenderDispatcher::new();
        for (code, product_code, tenths, elev_num, ps) in SRM_FIXTURE {
            let p = product(product_code, tenths, elev_num, 7108, ps);
            cache(&mut d, RadarProduct::StormRelativeVelocity, code, "KMPX", p);
        }
        d
    }

    /// Tilt selection reads the Product Description Block. VCP 212's cuts are
    /// 0.5/1.3/2.4/3.1, so a selector keyed on the mnemonics' nominal
    /// 0.5/1.5/2.4/3.4 would hand back the wrong product for two of the four —
    /// silently, since every one of them decodes.
    #[test]
    fn a_tilt_is_chosen_by_its_pdb_elevation_not_its_mnemonic() {
        let d = loaded();
        let at = |e: f32| {
            d.nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", e)
                .map(|p| (p.message.pdb.elevation_angle(), p.message.pdb.product_code))
        };
        assert_eq!(at(0.5), Some((0.5, 154)), "0.5° derives from N0G, not N0S");
        assert_eq!(
            at(1.3),
            Some((1.3, 154)),
            "N1G is 1.3°, not the nominal 1.5°"
        );
        assert_eq!(at(2.4), Some((2.4, 99)));
        assert_eq!(at(3.1), Some((3.1, 99)));
        // 1.5° — what the mnemonic claims — must still resolve to the 1.3° cut,
        // and nothing must resolve to a 1.5° that does not exist.
        assert_eq!(at(1.5), Some((1.3, 154)));
        assert_eq!(at(9.9), Some((3.1, 99)), "the highest cut is the nearest");
        // 1.9° is the one request where reading the angle and assuming it
        // disagree: VCP 212's real cuts put it 0.6° from 1.3° and 0.5° from
        // 2.4°, so it belongs to `N2U`; the mnemonics' nominal 1.5°/2.4° put it
        // 0.4° from `N1G` and would hand back a field a whole cut too low.
        assert_eq!(
            at(1.9),
            Some((2.4, 99)),
            "ranked by the PDB, not by the mnemonic"
        );
    }

    /// Only storm-relative velocity filters its candidates. Every other
    /// Level III product is rendered straight from the message on the wire, and
    /// none of them is dealiased velocity — so a filter that applied to all of
    /// them would leave the KDP, echo-tops, VIL, hydrometeor and
    /// precipitation-rate panes permanently blank, with nothing else about them
    /// changed.
    #[test]
    fn the_other_level3_products_are_not_filtered_at_all() {
        let mut d = RenderDispatcher::new();
        for (radar_product, code, product_code) in [
            (RadarProduct::SpecificDifferentialPhase, "N0K", 163i16),
            (RadarProduct::EchoTops, "EET", 135),
            (RadarProduct::VerticallyIntegratedLiquid, "DVL", 134),
            (RadarProduct::HydrometeorClassification, "HHC", 177),
            (RadarProduct::PrecipitationRate, "DPR", 176),
        ] {
            let p = product(product_code, 5, 1, 7108, VELOCITY_PS);
            cache(&mut d, radar_product, code, "KMPX", p);
            let picked = d
                .nearest_tilt(radar_product, "KMPX", 0.5)
                .unwrap_or_else(|| {
                    panic!("{code} is not dealiased velocity, and must render regardless")
                });
            assert_eq!(picked.message.pdb.product_code, product_code);
        }
    }

    /// The RPG's own product 56 must never be chosen as a tilt.
    ///
    /// It shares both the angle and the elevation number with `N0G`, so the
    /// tie-break cannot decide between them and hash order would: run over
    /// **freshly built maps** in both insertion orders, because `std`'s
    /// `RandomState` re-seeds per `HashMap` and one map iterates identically
    /// every time.
    #[test]
    fn the_rpgs_own_storm_relative_product_is_never_drawn() {
        for round in 0..60 {
            let mut d = RenderDispatcher::new();
            let mut fixture = SRM_FIXTURE;
            if round % 2 == 1 {
                fixture.reverse();
            }
            for (code, product_code, tenths, elev_num, ps) in fixture {
                let p = product(product_code, tenths, elev_num, 7108, ps);
                cache(&mut d, RadarProduct::StormRelativeVelocity, code, "KMPX", p);
            }
            for elevation in [0.0, 0.5, 0.9, 1.3, 2.4, 3.1] {
                let picked = d
                    .nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", elevation)
                    .expect("every elevation resolves to some tilt");
                assert_ne!(
                    picked.message.pdb.product_code, 56,
                    "round {round}, {elevation}°: the pane would show the RPG's 1 km \
                     field with its own vector baked in",
                );
            }
        }
    }

    /// With no `N0G` the 0.5° pane falls back to the next derived tilt, never
    /// to `N0S`. The fallback is a wrong *elevation*, which the pane can at
    /// least be honest about; `N0S` would be a wrong *kind of field*.
    #[test]
    fn a_missing_lowest_tilt_does_not_fall_back_to_the_rpgs_product() {
        let mut d = loaded();
        d.level3_data.remove(&(
            RadarProduct::StormRelativeVelocity,
            "N0G".to_string(),
            "KMPX".to_string(),
        ));
        let picked = d
            .nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", 0.5)
            .expect("the upper tilts are still loaded");
        assert_eq!(picked.message.pdb.product_code, 154);
        assert_eq!(
            picked.message.pdb.elevation_angle(),
            1.3,
            "the nearest surviving cut"
        );
        // With every velocity product gone there is no tilt at all, rather than
        // `N0S` reappearing as one.
        for code in ["N1G", "N2U", "N3U"] {
            d.level3_data.remove(&(
                RadarProduct::StormRelativeVelocity,
                code.to_string(),
                "KMPX".to_string(),
            ));
        }
        assert!(
            d.nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", 0.5)
                .is_none()
        );
        // …and the vector is still there to be read, so the filter removed it
        // from the screen and not from the cache.
        assert!(
            d.storm_motion_for("KMPX", &velocity_from(VOLUME.1))
                .is_some()
        );
    }

    /// Every tilt is derived, so every tilt honours the override — including
    /// 0.5°, which ignored it entirely while it rendered `N0S`.
    #[test]
    fn the_storm_motion_override_reaches_every_tilt_including_the_lowest() {
        let mut d = loaded();
        for elevation in [0.5f32, 1.3, 2.4, 3.1] {
            let tilt = d
                .nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", elevation)
                .unwrap_or_else(|| panic!("{elevation}° has a tilt"));
            // The RPG's own vector first, then the same tilt once an override
            // is in force. Set through the public setter, which is the only way
            // production sets it, so the field the renderer reads and the field
            // the invalidation reads cannot come apart.
            let rpg = srm::derive(
                &tilt.message,
                &d.storm_motion_for("KMPX", &tilt.message)
                    .expect("N0S is loaded"),
            )
            .unwrap_or_else(|| panic!("{elevation}° derives"));
            d.set_storm_motion_override(Some(
                StormMotionSample::user_override(45.0, 210.0).expect("finite"),
            ));
            let overridden = srm::derive(
                &tilt.message,
                &d.storm_motion_for("KMPX", &tilt.message)
                    .expect("the override is a vector"),
            )
            .unwrap_or_else(|| panic!("{elevation}° derives"));
            d.set_storm_motion_override(None);

            assert_eq!(overridden.motion.speed_kt, 45.0, "{elevation}°");
            assert_eq!(overridden.motion.direction_deg, 210.0, "{elevation}°");
            assert!(!overridden.motion.is_scit_average, "{elevation}°");
            // Recording the vector is not applying it: the gates have to move.
            assert_ne!(
                overridden.packet.radials[0].gate_values, rpg.packet.radials[0].gate_values,
                "{elevation}°: the override was recorded but never reached the field",
            );
        }
    }

    /// The render log must not tell the user an override came from an earlier
    /// volume.
    ///
    /// `(previous volume)` is a claim about the *RPG's* fit: it says the SCIT
    /// average applied here was computed for some other scan, which is the one
    /// case where the derived field is known to drift. A hand-entered vector
    /// belongs to no volume at all, so the annotation described provenance it
    /// never had — and it appeared on every override, because the sentinel
    /// volume key `(0, 0)` matched nothing.
    ///
    /// Driven through the real `derive`, so it fails for a provenance the
    /// derivation stops assigning as well as for a suffix that comes back.
    #[test]
    fn an_overridden_vector_is_not_logged_as_a_previous_volume() {
        let d = loaded();
        let tilt = d
            .nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", 0.5)
            .expect("0.5° has a tilt");

        let rpg = d
            .storm_motion_for("KMPX", &tilt.message)
            .expect("N0S is loaded");
        let own_volume = srm::derive(&tilt.message, &rpg).expect("derives");
        assert_eq!(
            motion_provenance_suffix(own_volume.motion_provenance),
            "",
            "precondition: this tilt's own volume's vector must be unannotated, \
             or the assertion below passes whatever the override does",
        );

        let stale = StormMotionSample {
            volume: rpg.volume.map(|(date, time)| (date, time - 1)),
            ..rpg
        };
        assert_eq!(
            motion_provenance_suffix(
                srm::derive(&tilt.message, &stale)
                    .expect("derives")
                    .motion_provenance
            ),
            " (previous volume)",
            "precondition: a genuinely stale RPG vector must still say so",
        );

        let overridden = srm::derive(
            &tilt.message,
            &StormMotionSample::user_override(30.0, 240.0).expect("finite"),
        )
        .expect("derives");
        assert_eq!(
            overridden.motion_provenance,
            srm::MotionProvenance::UserOverride
        );
        assert_eq!(
            motion_provenance_suffix(overridden.motion_provenance),
            " (user override)",
            "an override must not be annotated with any volume claim",
        );
    }

    /// Every tilt is 0.25 km. `N0S` is 1 km, so while it was rendered the 0.5°
    /// pane was four times coarser than the three above it.
    #[test]
    fn every_tilt_is_a_quarter_kilometre_field() {
        let d = loaded();
        let s = d
            .storm_motion_for("KMPX", &velocity_from(VOLUME.1))
            .expect("N0S is loaded");
        for elevation in [0.5f32, 1.3, 2.4, 3.1] {
            let tilt = d
                .nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", elevation)
                .unwrap_or_else(|| panic!("{elevation}° has a tilt"));
            let derived =
                srm::derive(&tilt.message, &s).unwrap_or_else(|| panic!("{elevation}° derives"));
            assert!(
                (derived.packet.gate_interval_km() - 0.25).abs() < 1e-9,
                "{elevation}°: {} km gates",
                derived.packet.gate_interval_km(),
            );
        }
    }

    /// Another site's products must never be borrowed. Both sites carry the
    /// same elevations, so a filter that dropped the site would still return
    /// something plausible.
    #[test]
    fn a_tilt_is_never_taken_from_another_site() {
        let mut d = loaded();
        let p = product(154, 13, 9, 9999, VELOCITY_PS);
        cache(
            &mut d,
            RadarProduct::StormRelativeVelocity,
            "N1G",
            "KFSD",
            p,
        );
        let picked = d
            .nearest_tilt(RadarProduct::StormRelativeVelocity, "KFSD", 1.3)
            .expect("KFSD has one tilt");
        assert_eq!(
            picked.message.pdb.volume_scan_time, 9999,
            "took KMPX's product"
        );
        assert!(
            d.nearest_tilt(RadarProduct::StormRelativeVelocity, "KABR", 1.3)
                .is_none()
        );
        assert!(
            d.nearest_tilt(RadarProduct::EchoTops, "KMPX", 1.3)
                .is_none()
        );
    }

    /// Split cuts and SAILS/MRLE repeats share an elevation angle, so the angle
    /// alone leaves the choice to hash order.
    ///
    /// Asserted across **freshly built maps**, not repeated calls on one map:
    /// `std`'s `RandomState` re-seeds per `HashMap` instance, so one map
    /// iterates in the same order every time and a stability loop over it
    /// cannot see the tie-break at all. Sixty maps, and both insertion orders,
    /// make an unbroken tie land on 9 with overwhelming probability.
    #[test]
    fn two_cuts_at_one_angle_resolve_the_same_way_every_time() {
        for round in 0..60 {
            let mut d = RenderDispatcher::new();
            let mut cuts = [("N1G", 9u16), ("NBG", 3)];
            if round % 2 == 1 {
                cuts.reverse();
            }
            for (code, elev_num) in cuts {
                let p = product(154, 13, elev_num, 7108, VELOCITY_PS);
                cache(&mut d, RadarProduct::StormRelativeVelocity, code, "KMPX", p);
            }
            let picked = d
                .nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", 1.3)
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

    /// The vector comes from the one product that has one. Halfword 51 is the
    /// BZ2 compression flag on `N1G`/`N2U`/`N3U`, so a search that took the
    /// first cached product would report 0.1 kt from 1.3°.
    #[test]
    fn the_vector_is_taken_from_the_product_that_carries_one() {
        let d = loaded();
        let s = d
            .storm_motion_for("KMPX", &velocity_from(VOLUME.1))
            .expect("N0S is loaded");
        assert_eq!(s.motion.speed_kt, 25.7);
        assert_eq!(s.motion.direction_deg, 296.1);
        assert!(s.motion.is_scit_average);
        assert_eq!(s.volume, Some((20661, 7108)));
    }

    /// Without an `N0S` there is no vector, and rendering the velocity field
    /// raw would put a base-velocity couplet under a storm-relative label.
    #[test]
    fn no_n0s_means_no_vector_rather_than_a_zero_one() {
        let mut d = RenderDispatcher::new();
        for (code, product_code, tenths, elev_num, ps) in SRM_FIXTURE {
            if code == "N0S" {
                continue;
            }
            let p = product(product_code, tenths, elev_num, 7108, ps);
            cache(&mut d, RadarProduct::StormRelativeVelocity, code, "KMPX", p);
        }
        assert!(
            d.storm_motion_for("KMPX", &velocity_from(VOLUME.1))
                .is_none()
        );
        // A user override fills the gap — that is what it is for.
        d.set_storm_motion_override(Some(
            StormMotionSample::user_override(40.0, 200.0).expect("finite"),
        ));
        assert_eq!(
            d.storm_motion_for("KMPX", &velocity_from(VOLUME.1))
                .map(|s| s.motion.speed_kt),
            Some(40.0),
        );
    }

    /// An `N0S` from a later volume must not be applied to a velocity product
    /// from an earlier one.
    ///
    /// This is the normal case, not a boundary race: `N0S` and `N0G` are
    /// published when the 0.5° cut completes and `N1G`/`N2U`/`N3U` when theirs
    /// do, so for most of a volume the newest vector is a volume ahead of the
    /// upper three tilts. Measured over 22 sites, the newest vector belonged to
    /// another volume on 306 of 792 renders, and where the fit had really moved
    /// taking the newest cost up to 82 points of within-one-level agreement —
    /// `KFSD` was caught applying 66.5 kt where 19.4 kt belonged, for 17.1%
    /// against 99.87% on the same gates.
    #[test]
    fn each_tilt_gets_its_own_volumes_vector() {
        let mut d = loaded();
        // The next volume's N0S arrives, carrying a different fit. The upper
        // tilts are still the previous volume's.
        let next = (20661u16, 7392u32);
        let mut later = N0S_PS;
        later[4] = 402; // 40.2 kt
        later[5] = 1500; // from 150.0°
        cache(
            &mut d,
            RadarProduct::StormRelativeVelocity,
            "N0S",
            "KMPX",
            product(56, 5, 1, next.1, later),
        );

        // Through the products the dispatcher would really hand to `derive`,
        // resolved the way `try_spawn_level3_render` resolves them. Asking with
        // a volume key the test made up would leave the one line that reads the
        // volume off the message — the whole wiring into production — free to
        // return a constant with every assertion here still passing.
        for elevation in [0.5f32, 1.3, 2.4, 3.1] {
            let tilt = d
                .nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", elevation)
                .unwrap_or_else(|| panic!("{elevation}° has a tilt"));
            let s = d
                .storm_motion_for("KMPX", &tilt.message)
                .unwrap_or_else(|| panic!("{elevation}° has a vector"));
            assert_eq!(
                s.volume,
                Some(tilt.message.pdb.volume_key()),
                "{elevation}° was paired with another volume's vector",
            );
            assert_eq!(
                s.motion.speed_kt, 25.7,
                "{elevation}° got the newest vector rather than its own volume's",
            );
        }

        // And the new volume's own tilt resolves to the new fit, so the match
        // above is a match and not the lookup failing the same way twice.
        let new = d
            .storm_motion_for("KMPX", &velocity_from(next.1))
            .expect("the new volume is recorded");
        assert_eq!(new.motion.speed_kt, 40.2);

        // A volume nobody has a vector for still renders, on the newest —
        // better a vector one volume out than a blank storm-relative pane. The
        // 0.5° SAILS repeat can genuinely arrive before its volume's `N0S`.
        let unseen = d
            .storm_motion_for("KMPX", &velocity_from(9999))
            .expect("an unknown volume falls back");
        assert_eq!(
            unseen.volume,
            Some(next),
            "the fallback is the newest, not an arbitrary one"
        );
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

    /// The age a pane is stamped with belongs to **the tilt it is showing**.
    ///
    /// `level3::latest_key` falls back to the previous UTC day, so the object a
    /// downed site serves can be most of a day old while the Level II scan line
    /// beside it looks current — that is the whole reason the pane draws an age
    /// at all. A lookup that answered with the site's *newest* stamp, or with
    /// the first entry the map happened to iterate, would caption a stale field
    /// with a fresh time and be worse than drawing nothing.
    ///
    /// Every tilt is given a distinct stamp, so a reader tied to any one of
    /// them fails on the others. `1.9°` is the discriminating request: VCP
    /// 212's real cuts put it nearer 2.4° than 1.3°, so an implementation that
    /// resolved the tilt any other way lands on a different stamp.
    #[test]
    fn a_render_stamps_its_pane_with_the_age_of_the_tilt_it_chose() {
        let mut d = RenderDispatcher::new();
        // Minute `nn` in the key, per tilt — nothing else about the fixtures
        // differs, so only the tilt selection can decide which comes back.
        let stamps = [("N0G", 11u32), ("N1G", 22), ("N2U", 33), ("N3U", 44)];
        for (code, product_code, tenths, elev_num, ps) in SRM_FIXTURE {
            let mut p = product(product_code, tenths, elev_num, 7108, ps);
            if let Some((_, minute)) = stamps.iter().find(|(c, _)| *c == code) {
                p.stamp = ProductStamp::from_key(format!("MPX_{code}_2026_07_26_01_{minute}_00"));
            }
            cache(&mut d, RadarProduct::StormRelativeVelocity, code, "KMPX", p);
        }

        let minute_at = |elevation: f32| {
            let mut pane = pane_on("KMPX");
            d.stamp_pane_with_product_age(
                &mut pane,
                &rendered(RadarProduct::StormRelativeVelocity, elevation),
            );
            pane.level3_time.map(|t| chrono::Timelike::minute(&t))
        };
        assert_eq!(minute_at(0.5), Some(11), "0.5° is N0G");
        assert_eq!(minute_at(1.3), Some(22), "1.3° is N1G");
        assert_eq!(minute_at(2.4), Some(33), "2.4° is N2U");
        assert_eq!(minute_at(3.1), Some(44), "3.1° is N3U");
        assert_eq!(
            minute_at(1.9),
            Some(33),
            "1.9° belongs to the 2.4° cut, so it must carry that cut's stamp",
        );

        // The pane's *own* site, not any site with a tilt cached: two panes
        // showing the same product on different radars must not share an age.
        let mut elsewhere = pane_on("KTLX");
        d.stamp_pane_with_product_age(
            &mut elsewhere,
            &rendered(RadarProduct::StormRelativeVelocity, 0.5),
        );
        assert_eq!(
            elsewhere.level3_time, None,
            "another site's tilts are not this pane's",
        );

        // …and a Level II render clears it rather than leaving the Level III
        // age captioning a volume it has nothing to do with.
        let mut switched = pane_on("KMPX");
        d.stamp_pane_with_product_age(
            &mut switched,
            &rendered(RadarProduct::StormRelativeVelocity, 0.5),
        );
        assert!(switched.level3_time.is_some(), "precondition: it was dated");
        d.stamp_pane_with_product_age(&mut switched, &rendered(RadarProduct::Reflectivity, 0.5));
        assert_eq!(
            switched.level3_time, None,
            "a Level II product has no ProductStamp at all",
        );
    }

    /// A key whose tail does not parse is an **unknown** age, not a fresh one.
    ///
    /// `ProductStamp::time` is `None` there, and the pane draws no age line —
    /// which is the honest answer. Reporting the epoch, or silently falling
    /// back to now, would both claim something the key does not say.
    #[test]
    fn an_unreadable_key_reports_no_age_rather_than_a_wrong_one() {
        let mut d = RenderDispatcher::new();
        let mut p = product(154, 5, 1, 7108, VELOCITY_PS);
        p.stamp = ProductStamp::from_key("not-a-key");
        cache(
            &mut d,
            RadarProduct::StormRelativeVelocity,
            "N0G",
            "KMPX",
            p,
        );

        assert!(
            d.nearest_tilt(RadarProduct::StormRelativeVelocity, "KMPX", 0.5)
                .is_some(),
            "precondition: the tilt is still drawn — an unreadable key is worth \
             rendering, just not worth dating",
        );
        let mut pane = pane_on("KMPX");
        d.stamp_pane_with_product_age(
            &mut pane,
            &rendered(RadarProduct::StormRelativeVelocity, 0.5),
        );
        assert_eq!(pane.level3_time, None);
    }

    /// The history is bounded and keyed on the volume, so a long session cannot
    /// grow it and a re-fetched `N0S` cannot fill it with one volume.
    #[test]
    fn the_storm_motion_history_is_bounded_and_deduplicated() {
        let mut d = RenderDispatcher::new();
        for i in 0..3 {
            for _repeat in 0..4 {
                cache(
                    &mut d,
                    RadarProduct::StormRelativeVelocity,
                    "N0S",
                    "KMPX",
                    product(56, 5, 1, 7000 + i, N0S_PS),
                );
            }
        }
        assert_eq!(
            d.storm_motion_history["KMPX"].len(),
            3,
            "a repeated volume must not take a slot from a different one",
        );
        for i in 3..12 {
            cache(
                &mut d,
                RadarProduct::StormRelativeVelocity,
                "N0S",
                "KMPX",
                product(56, 5, 1, 7000 + i, N0S_PS),
            );
        }
        assert_eq!(d.storm_motion_history["KMPX"].len(), STORM_MOTION_HISTORY);
        // The oldest went, not the newest.
        assert!(
            d.storm_motion_for("KMPX", &velocity_from(7011))
                .is_some_and(|s| s.volume.map(|v| v.1) == Some(7011))
        );
        assert!(
            d.storm_motion_for("KMPX", &velocity_from(7000))
                .is_some_and(|s| s.volume.map(|v| v.1) == Some(7011)),
            "volume 7000 aged out and must fall back to the newest",
        );

        // An out-of-order arrival must not become the fallback or evict a newer
        // volume. Fetches complete in whatever order the bucket answers in.
        cache(
            &mut d,
            RadarProduct::StormRelativeVelocity,
            "N0S",
            "KMPX",
            product(56, 5, 1, 6500, N0S_PS),
        );
        assert!(
            d.storm_motion_for("KMPX", &velocity_from(1))
                .is_some_and(|s| s.volume.map(|v| v.1) == Some(7011)),
            "an old object arriving late became the newest",
        );
        assert!(
            d.storm_motion_for("KMPX", &velocity_from(7011))
                .is_some_and(|s| s.volume.map(|v| v.1) == Some(7011)),
            "an old object arriving late evicted a newer volume",
        );
    }

    /// The history outlives a per-site reset, and this is the whole fix.
    ///
    /// `reset_panes_for_site` runs on every auto-poll for a live pane, right
    /// before the five products are refetched. A history cleared there would
    /// hold nothing but the volume the newest `N0S` came from — which is
    /// exactly the volume the upper tilts have not reached — so every tilt
    /// would fall back to the newest vector and the per-volume pairing would
    /// never once fire in production. Only a full reset forgets.
    #[test]
    fn a_poll_reset_keeps_the_history_that_the_next_poll_needs() {
        let mut d = loaded();
        // A poll arrives: panes reset, products refetched, the next volume's
        // N0S lands while the upper tilts are still the previous volume's.
        d.reset_panes_for_site("KMPX", &rustdar_egui::Gui::new());
        let next = (20661u16, 7392u32);
        cache(
            &mut d,
            RadarProduct::StormRelativeVelocity,
            "N0S",
            "KMPX",
            product(56, 5, 1, next.1, N0S_PS),
        );
        let paired = d
            .storm_motion_for("KMPX", &velocity_from(VOLUME.1))
            .expect("the previous volume's vector is still known");
        assert_eq!(
            paired.volume,
            Some(VOLUME),
            "the poll reset dropped the history, so an upper tilt fell back to the newest \
             vector — the pairing this exists to fix",
        );

        // A full reset does forget, and takes every site with it.
        d.reset_panes();
        assert!(
            d.storm_motion_for("KMPX", &velocity_from(VOLUME.1))
                .is_none()
        );
    }

    /// The override wins over the RPG's own vector, or the setting does nothing.
    #[test]
    fn a_user_override_displaces_the_rpg_vector() {
        let mut d = loaded();
        // The site's own vector is 25.7 kt, so this must not be it.
        d.set_storm_motion_override(Some(
            StormMotionSample::user_override(45.0, 210.0).expect("finite"),
        ));
        let s = d
            .storm_motion_for("KMPX", &velocity_from(VOLUME.1))
            .unwrap();
        assert_eq!(s.motion.speed_kt, 45.0);
        assert_eq!(s.motion.direction_deg, 210.0);
        assert!(!s.motion.is_scit_average);
    }

    /// Editing the vector changes nothing else about a pane, so both the
    /// per-pane state and the shared render cache have to be dropped by hand.
    #[test]
    fn changing_the_override_invalidates_the_storm_relative_renders() {
        let mut d = loaded();
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
        let mut d = loaded();
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

    /// One pane, on `site`, which is how `reset_panes_for_site` reads the layout.
    fn gui_showing(site: &str) -> rustdar_egui::Gui {
        let mut gui = rustdar_egui::Gui::new();
        gui.pane_mut(0).expect("a fresh Gui has one pane").site = site.to_string();
        gui
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
