use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rustdar_radar::level3::Level3Product;
use rustdar_radar::srm::StormMotionSample;
use rustdar_radar::types::{RadarProduct, RenderView};

use crate::WindowRef;
use crate::budget::Budgets;
use crate::channels::RenderResponse;

/// Drop guard that decrements an AtomicUsize counter on drop.
/// Guarantees the counter is decremented even if the thread panics.
pub(crate) struct RenderGuard(pub(crate) Arc<AtomicUsize>);

impl Drop for RenderGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The last successful render's pixels + metadata, so the texture can be
/// re-uploaded instantly after suspend/resume without re-rendering.
pub struct CachedPaneRender {
    /// See [`crate::channels::RenderedImage::image`] — held converted, so a
    /// resume is an upload and not a second unmultiply of 64 MiB.
    pub image: Arc<egui::ColorImage>,
    /// The half-width the cached pixels were projected at, km. Kept beside
    /// them because a restore has to place them where the render put them —
    /// see `restore_cached_renders`, which rebuilds the bounds from this and
    /// from nothing else.
    pub max_range_km: f64,
    pub value_data: Arc<Vec<f32>>,
    pub product: RadarProduct,
    pub elevation: f32,
    /// Where the cached sweep's cut declared its velocity folds, m/s. Kept for
    /// the same reason `max_range_km` is: a restore has to describe the pixels
    /// it puts back the way the render described them, and the volume behind
    /// them may be gone by then.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer these cached pixels were classified against came
    /// from. Kept for exactly the reason the fold limit is: the object behind it
    /// is per-volume and the cache holding it will have been replaced by the
    /// time a suspended pane resumes, so a restore that re-derived this would
    /// caption an old classification with a new volume's provenance.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
}

/// Per-pane render tracking state.
pub struct PaneRenderState {
    /// True while a background render is in progress for this pane.
    ///
    /// Private, with [`render_started`](Self::render_started) and
    /// [`render_finished`](Self::render_finished) the only ways it moves,
    /// because it is now half of a pair: `in_flight_plan_view` below says
    /// *which* picture is being made and is what a sibling pane reads to decide
    /// it need not ask for the same one. A caller that could set this flag on
    /// its own could leave a key behind for a render that had already answered,
    /// and every pane wanting that picture would then wait for a result nobody
    /// was going to send. The pair moves together or not at all — the same
    /// reason [`RenderCache`]'s two halves are private.
    render_in_flight: bool,
    /// Which **plan view** this pane's in-flight render is drawing, as the
    /// `(site, product, view, elevation)` key [`render_cache_key`] builds — or
    /// `None` when nothing is in flight, or when what is in flight is not a
    /// plan view.
    ///
    /// # What it is for
    ///
    /// A volume landing on a four-pane split of one site, product and tilt used
    /// to start **four** renders of one sweep: `dispatch_pane_renders` walks the
    /// panes in one pass, and the render cache it consults is only written when
    /// a result comes *back*, so the first pane's job is still in flight when
    /// the second, third and fourth ask. Each of those panes then also paid a
    /// full `RenderInput::extract` on the frame thread to build a payload
    /// identical to the one already on its way.
    ///
    /// With this, the first pane to ask is the only one that asks. The
    /// broadcast in `App::poll_render_results` already serves every sibling
    /// from the one result — that is what `PlanViewUploads` was built around —
    /// so the panes that skip here are not waiting on anything extra; they are
    /// waiting on the same result they would have been handed anyway.
    ///
    /// # Why the key is the cache's own
    ///
    /// It is built by [`render_cache_key`] and by nothing else, so "a render is
    /// already making this" and "the cache already holds this" are the same
    /// question asked at two moments. A dedupe keyed even slightly differently
    /// — on the raw elevation, say — would suppress a render whose result the
    /// cache would then file under another key, and the pane that skipped would
    /// never be served.
    in_flight_plan_view: Option<RenderCacheKey>,
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
            in_flight_plan_view: None,
            last_rendered: None,
            cached_render: None,
            results_wanted: Vec::new(),
        }
    }

    /// Whether a background render is running for this pane.
    pub fn render_in_flight(&self) -> bool {
        self.render_in_flight
    }

    /// Mark a render dispatched for this pane, `key` naming the plan view it
    /// draws — `None` for a render that is not a plan view, which is every
    /// cross-section.
    ///
    /// A section passes `None` rather than a key of its own on purpose. The
    /// only reader is the plan-view dispatch, which asks "is this picture
    /// already being made"; a section pane and a map pane can hold the same
    /// `(site, product, elevation)` and are not making the same picture, and
    /// [`RenderCacheKey`]'s view axis exists to say so. Keeping sections out of
    /// this field entirely is one fewer way for that axis to be got wrong.
    pub fn render_started(&mut self, key: Option<RenderCacheKey>) {
        self.render_in_flight = true;
        self.in_flight_plan_view = key;
    }

    /// Mark this pane's render finished — answered, discarded or abandoned.
    ///
    /// All three are the same fact for every reader: nothing is on its way any
    /// more, so this pane may dispatch again and no sibling should be waiting
    /// on it.
    pub fn render_finished(&mut self) {
        self.render_in_flight = false;
        self.in_flight_plan_view = None;
    }

    /// The flag a newly dispatched render reports through, live until this pane's
    /// renders are abandoned.
    ///
    /// Renders past stopping are dropped from the list first: the render holds the
    /// only other reference to its own flag and releases it the moment it reads it,
    /// so one strong reference means that render has already decided whether to
    /// answer and clearing its flag would change nothing.
    ///
    /// That release is sequenced before the send, so a result taken off the channel
    /// is proof that the flag it reported through is prunable here — the list is
    /// bounded by the renders still *stoppable*, and not by how long a finished
    /// worker takes to unwind. Before the release moved, this pruned on thread
    /// teardown instead, and a pane that dispatched under load kept flags for
    /// renders that had already answered.
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
///
/// The extent rides in the entry rather than being recomputed per pane, which
/// is what makes the sharing sound: two panes on the same `(site, product,
/// view, elevation)` are looking at one buffer, and a buffer projected at
/// 417 km has to be placed at 417 km on both of them.
pub struct CachedRenderOutput {
    pub image: Arc<egui::ColorImage>,
    pub max_range_km: f64,
    pub value_data: Arc<Vec<f32>>,
    /// Where the drawn sweep's cut declared its velocity folds, m/s — shared
    /// with the buffer for the reason the extent is, and it is the same
    /// argument: two panes on one `(site, product, view, elevation)` are
    /// looking at one raster of one cut, so they are looking at one fold limit.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer behind this shared raster came from — shared for
    /// the same argument as the two above: one buffer, one classification, one
    /// provenance, whichever pane is looking at it.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
}

/// `(site, product, view, elevation_tenths)` — see [`elevation_key`] and
/// [`render_cache_key`].
///
/// # Why the view is in the key
///
/// The cache is shared between panes, and what it shares is a *buffer*. A plan
/// view of reflectivity and a cross-section of reflectivity at the same site
/// are the same `(site, product, elevation)` and completely different shapes —
/// a square of ground against `SECTION_WIDTH × SECTION_HEIGHT` of a
/// vertical plane. Without this axis they collide in the LRU and one pane is
/// handed the other's buffers, which is not a wrong picture: it is a texture of
/// the wrong shape stretched over a pane's geography, with the hover reading
/// a vertical plane's values through a plan view's bounds.
///
/// It is added now, while the cache still holds only plan-view rasters, because
/// this is the last moment at which the change is mechanical.
pub type RenderCacheKey = (String, RadarProduct, RenderView, i32);

/// Bounded least-recently-used cache of render outputs shared between panes.
///
/// Each entry is a `side²` image plus a `side²` `f32` value grid — 32 MiB
/// apiece at 2048², 128 MiB at 4096² — and before this was bounded the only
/// thing that ever dropped one was `reset_panes*`, so switching product or
/// elevation grew the cache without limit.
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
fn elevation_key(elevation: f32) -> i32 {
    (elevation * 10.0).round() as i32
}

/// The elevation slot for a view that has no elevation.
///
/// A section cuts across every tilt and a voxel grid resamples all of them, so
/// the pane's nominal elevation says nothing about the buffer — two sections of
/// one product at one site are the same render whatever tilt each pane's
/// selector happens to be parked on, and keying them apart would store the same
/// picture several times and evict the plan views to do it.
///
/// **A sentinel would have been wrong here and the view axis is what makes this
/// safe.** Any `i32` chosen for "no elevation" collides with a real
/// [`elevation_key`] — `0` is a genuine 0.0° plan render — so before the key
/// carried the view there was no value this could be. With the view in the key
/// the slot is only ever compared against other entries of the same view, so
/// `0` is not a sentinel at all: it is the one bucket a viewless render has.
const NO_ELEVATION_SLOT: i32 = 0;

/// The cache key for one render, and the only place one is built.
///
/// Written once rather than at each call site because the three rules above —
/// which axis discriminates, which slot a viewless view uses, and which
/// `(view, product)` pairs have no tilt to discriminate on — are the kind that
/// a second copy gets half right.
///
/// The third of those is [`RenderView::elevation_selects_picture`], asked
/// rather than restated here: the loop path keys its frames on the same
/// question, and the two answering it separately is what let a section loop
/// discard every frame for a tilt no section can see.
fn render_cache_key(
    site: &str,
    product: RadarProduct,
    view: RenderView,
    elevation: f32,
) -> RenderCacheKey {
    let elevation = if view.elevation_selects_picture(product) {
        elevation_key(elevation)
    } else {
        NO_ELEVATION_SLOT
    };
    (site.to_string(), product, view, elevation)
}

/// Whether a render of `view` showing `product` has to be given the whole
/// volume rather than the one sweep `render::find_sweep` picks.
///
/// **One predicate, two halves, and neither can answer for the other.** The
/// product half is [`RadarProduct::reads_whole_volume`] — "does this field
/// integrate the column?" — and the view half is
/// [`RenderView::reads_whole_volume`] — "does this picture slice vertically?".
/// A reflectivity cross-section answers *no* to the first and *yes* to the
/// second, so a dispatch that asked only the product question would extract one
/// sweep and the section would be interpolated across the tilts that were not
/// there: no error, no `NaN`, a smooth plausible layer that looks *better* than
/// the truth.
///
/// It reads both rather than restating either. That is the lesson the campaign
/// already paid for once: a hand-maintained second copy of the product half
/// omitted storm-relative velocity, and live SRV panes fitted their dealias
/// seed from volumes the feed had deliberately skipped cuts of.
///
/// `App::cut_selection_for` still asks the two halves at two different points
/// rather than calling this, and deliberately: the view is known for a pane
/// before its render parameters resolve, and the whole window between
/// converting a pane and its volume arriving — which is exactly when the first
/// section is cut — is time in which the product half cannot be asked at all.
/// The safety property is the same; only the order differs.
pub fn needs_whole_volume(view: RenderView, product: RadarProduct) -> bool {
    view.reads_whole_volume() || product.reads_whole_volume()
}

/// Manages radar rendering dispatch and Level III data caching.
///
/// Tracks per-pane render state, owns the Level III data cache, and
/// provides generation-based staleness checks for both fetches and renders.
pub struct RenderDispatcher {
    /// Per-pane render tracking (indexed by pane index).
    pub pane_render: Vec<PaneRenderState>,
    /// The latest fetched Level III object per `(AWIPS code, site)`.
    ///
    /// Keyed by the **code**, not by the product that wanted it, because an
    /// object is not owned by a product: `DVL` is `VerticallyIntegratedLiquid`'s
    /// whole field *and* VIL density's numerator, `EET` is `EchoTops`' field
    /// *and* its denominator. A product-keyed cache had to be filled once per
    /// product, which meant fetching the same ~100 KB object twice on every site
    /// poll; keyed this way one fetch serves every reader
    /// ([`RadarProduct::level3_readers`]).
    ///
    /// Which entries a product may read is still narrow, and is decided in one
    /// place — the product's own [`RadarProduct::level3_products`] list, applied
    /// by [`nearest_tilt`](Self::nearest_tilt) and
    /// [`cached_by_code`](Self::cached_by_code). Nothing resolves an object it
    /// does not name, so sharing the map does not let a product read a field it
    /// has no palette for.
    ///
    /// Holds the whole [`Level3Product`], not just the message, so the stamp —
    /// which object it came from and when it was written — reaches the UI
    /// alongside the pixels. See [`rustdar_radar::level3::ProductStamp`].
    ///
    /// Private, so [`cache_level3`](Self::cache_level3) really is the only way
    /// in: an insert that bypassed it would drop the storm motion vector on the
    /// floor, and the pane would render with another volume's.
    level3_data: HashMap<(String, String), Arc<Level3Product>>,
    /// Environmental 0 °C / −20 °C heights per site, from Open-Meteo — staged
    /// for the products [`RadarProduct::reads_env_heights`] names, which will
    /// read them at render time, and which
    /// [`set_env_heights`](Self::set_env_heights) invalidates on. Written by
    /// the sounding drain in `app_render`; read back by
    /// `spawn_level3_fetches`'s TTL gate, which refetches on poll only once
    /// [`rustdar_radar::sounding::EnvHeights::is_stale`] says the entry has
    /// aged out. Survives both reset paths: the environment does not change
    /// because a pane was reset, and the TTL is the eviction policy.
    pub env_heights: HashMap<String, rustdar_radar::sounding::EnvHeights>,
    /// The RPG's own Melting Layer object per site — the top rung of
    /// `rustdar_radar::hca::resolve_melting_layer`, staged for the one product
    /// that reads it and invalidated by
    /// [`set_melting_layer`](Self::set_melting_layer).
    ///
    /// # Why this one carries a volume and `env_heights` does not
    ///
    /// A sounding is a fact about a place and an hour: applying an hour-old one
    /// to this volume is merely old, which is why that map is keyed by site
    /// alone and aged out on a TTL. An `N0M` object is a fact about **one
    /// volume**. Applied to another it does not degrade, it lies — it places a
    /// measured-looking layer at a height nothing measured for the volume on
    /// screen, which is the same failure as the fleet default with the caption
    /// removed. So the volume it names is stored with it and
    /// [`melting_layer_product_for`](Self::melting_layer_product_for) refuses
    /// every other volume.
    ///
    /// Survives both reset paths for the reason `env_heights` does, and more
    /// safely: a stale entry here cannot be *applied*, so eviction is not what
    /// makes it correct. Keeping it is what lets a pane switched back to a
    /// volume still in hand classify without a second round-trip.
    melting_layer: HashMap<String, MeltingLayerObject>,
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
    /// This is the single source of truth for the concurrent-render budget and is
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
    /// Bounded by `MAX_RENDER_CACHE_ENTRIES` on an LRU policy: it is a sharing
    /// cache for the panes on screen, not a history, and each entry costs
    /// `side² × 8` bytes — 32 MiB at the base side and 128 MiB for a
    /// long-range sweep on a device that can take one. See
    /// `constants::MAX_RENDER_CACHE_ENTRIES` for why the bound stays a count
    /// and what the resulting ceiling is.
    pub render_cache: RenderCache,
    /// Whether the device this process is drawing on can hold a long-range
    /// raster — `AppState::long_range_raster_ok`, copied here because the
    /// dispatch sites are where it turns into a job.
    ///
    /// `false` until a device exists, which is the safe direction: a static
    /// render dispatched before `ensure_rendering_state` has installed one
    /// would be dispatched at the base size, not at a size nothing can upload.
    /// Nothing dispatches that early in the shipped app — the frame loop
    /// returns before `dispatch_pane_renders` while `state` is `None` — so
    /// what actually observes the default is this crate's tests, and the base
    /// size is what they were written against.
    /// Background radar renders that may be in flight at once — this build's
    /// `Budgets::concurrent_renders`, held rather than read from a `cfg`
    /// constant. See [`RenderDispatcher::with_budgets`].
    concurrent_renders: usize,
    long_range_raster_ok: bool,
    /// The storm motion override the storm-relative renders on screen were
    /// built with. Nothing else about a pane changes when the user edits the
    /// vector, so without this the field would keep the old motion until the
    /// next scan. Routed into the Level II render parameters by
    /// [`spawn_level2_render`](Self::spawn_level2_render); with no override
    /// the renderer applies the Bunkers right-mover from the volume's own
    /// wind profile (`rustdar_radar::srv`). The RPG-vector history that used
    /// to live beside this left with the five Level III SRM fetches.
    last_storm_motion_override: Option<StormMotionSample>,
    /// The last whole-volume payload extracted for a cross-section, and what it
    /// was extracted from.
    ///
    /// # Why one entry and not a cache
    ///
    /// `RenderInput::extract_volume` walks every sweep of a volume carrying the
    /// moment and copies its gates out: **15.6 MB** for a full reflectivity
    /// ladder, and the walk itself is the expensive half. Everything that makes
    /// a section pane want another cut re-uses the same volume and the same
    /// moment — moving the line, a second section pane cut from the same map,
    /// a line redrawn because the first one missed the storm — so a single entry
    /// keyed on [`SectionInputKey`] catches all of it, and a second entry
    /// would only ever hold a volume nothing on screen is looking at.
    ///
    /// The dominant protection is upstream of this and is a property of the
    /// interaction rather than of a cache: sections are re-cut **on commit**,
    /// never per drag frame. The rubber band is drawn locally with no render at
    /// all, so the payload is built when a line is finished and not while it is
    /// being aimed. That matters most exactly where this cache helps least —
    /// wasm, where the concurrent-render budget is 1 with no preemption, so a
    /// per-frame dispatch would not merely be wasteful but would queue behind
    /// itself.
    section_input: Option<SectionInput>,
}

/// One site's `N0M` object **and the volume start it names**.
///
/// The pair is the type. A bare `Arc<Vec<u8>>` would be an object with no
/// statement of what it is an object *of*, and every consumer would then have
/// to remember to ask — which is exactly the omission that produces a
/// confidently misplaced melting layer. Held together, the question cannot be
/// skipped: [`RenderDispatcher::melting_layer_product_for`] is the only reader
/// and it compares before it hands anything out.
///
/// The bytes and not the decoded message: `rustdar_radar::render_input`
/// carries the object to the worker as a blob, and a `Level3Message` has no
/// wire form — the same reason `try_spawn_level3_render` dispatches bytes.
pub struct MeltingLayerObject {
    /// The Level II volume start this object's PDB names, already validated by
    /// `rustdar_radar::level3::fetch_product_for_volume` — so this field is
    /// never a guess and never "close enough by listing time".
    pub volume_start: chrono::NaiveDateTime,
    pub bytes: Arc<Vec<u8>>,
}

/// A whole-volume payload and the volume it came out of.
///
/// The product is part of the key because `extract_volume` narrows to one
/// moment: a payload extracted for reflectivity carries no velocity, and handing
/// it to a velocity section would produce a picture of an empty ladder rather
/// than an error.
///
/// The ladder fingerprint is part of the key for the reason
/// [`SectionTarget::ladder`](rustdar_egui::pane::SectionTarget) exists: on the
/// live feed the volume time is frozen for the whole volume while the merged
/// volume refreshes sweep by sweep, so `(site, collected, product)` alone
/// would hand a payload extracted before a seal back to a cut dispatched
/// after it. And because the fingerprint moves only when a rung's chosen
/// sweep or the declared pattern changes, the walk this cache exists to avoid
/// runs only when the picture will actually differ — a Doppler-half seal no
/// longer invalidates a reflectivity payload it never changed.
struct SectionInput {
    key: SectionInputKey,
    /// `Arc` so the cache and the job in flight can hold it at once; the job
    /// needs an owned `RenderInput`, so what crosses to the worker is a clone of
    /// the bytes rather than a second walk of the volume.
    input: Arc<rustdar_radar::render_input::RenderInput>,
}

/// What a section dispatch did — three answers, because two of them used to
/// be one.
///
/// [`Busy`](Self::Busy) and [`NoPayload`](Self::NoPayload) were both `false`,
/// and the caller's reading of `false` is "take no budget, write no staleness
/// key, ask again next frame". That is right for a full budget and wrong for a
/// volume that carries nothing to cut: the pane re-asked on every frame and
/// painted "Cutting the cross-section…" for as long as the volume stood. A
/// permanent wait is the worst state a pane can be in — it looks like progress
/// and there is nothing to do about it — and this codebase shipped that exact
/// bug once before and fixed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionDispatch {
    /// A cut is in flight. The caller writes the staleness key.
    Dispatched,
    /// The render budget is full, or the pane index is out of range. Nothing
    /// was taken and nothing is wrong: ask again next frame, key unwritten.
    Busy,
    /// This volume carries no field to cut under this product — no sweep
    /// holds the moment, or the derivation refused it. The caller names the
    /// state and *does* write the key: the key carries the volume stamp and
    /// the ladder, so the next volume asks again on its own.
    NoPayload,
}

/// What a cached section payload is a payload *of*.
///
/// A struct with a derived `PartialEq` rather than a chain of `&&`s at the reuse
/// site, because the failure mode of the chain is forgetting a clause, and the
/// consequence of forgetting one is not an error — it is the wrong volume, or an
/// empty ladder, or (the quietest of the three) a picture of the right volume
/// with most of it missing. Adding a field to this struct therefore cannot
/// silently leave the comparison behind.
#[derive(Clone, Debug, PartialEq)]
struct SectionInputKey {
    site: String,
    collected: chrono::NaiveDateTime,
    product: RadarProduct,
    /// The fingerprint of the ladder this payload was extracted under —
    /// exactly the choices `extract_volume_parts` copied.
    ladder: u64,
    /// The storm motion vector the payload was **derived** with, as raw bits,
    /// and `None` for every product that does not read one.
    ///
    /// This field is the fix for a silent wrong-field. A storm-relative
    /// section is not a slice of a measured moment: `extract_volume_parts`
    /// runs the SRV derivation on the way out, so the payload *is* a function
    /// of the vector. Without the vector in the key, an override edit left
    /// `reusable` true, `extract()` unrun and the previous vector's field
    /// shipped to the worker — while the plan view and the 3D volume both
    /// re-derived correctly, because their invalidations do cover it. The
    /// user dragged the vector and watched the section visibly redraw showing
    /// the old one, for up to a whole volume, with nothing saying so.
    ///
    /// In the key rather than as an eviction in
    /// [`set_storm_motion_override`](RenderDispatcher::set_storm_motion_override),
    /// which is the other place it could go. Two reasons. The payload cache
    /// holds exactly one entry, so an eviction would throw away a
    /// *reflectivity* payload the vector never touched and charge the next
    /// cut a 15.6 MB re-walk for an edit that could not have changed it. And
    /// identity is what this struct is for: its doc above is the promise that
    /// a thing the payload depends on cannot be left out of the comparison,
    /// and the vector is such a thing.
    ///
    /// Bits rather than `f32`s so the comparison is reflexive. A NaN vector
    /// would never equal itself, and the consequence would not be a wrong
    /// picture but a re-extraction of the whole volume on every frame the
    /// section stood — the quiet kind of failure this file already carries
    /// two notes about.
    storm_motion: Option<(u32, u32)>,
}

impl SectionInputKey {
    /// The key a payload would have to carry to serve `target` under the
    /// storm motion vector `motion`.
    ///
    /// `motion` is the same `(speed_kt, direction_from_deg)` pair
    /// [`RenderDispatcher::storm_motion_override_kt`] hands the extraction —
    /// read from the dispatcher's own field at the call site, never taken
    /// from the caller, so the vector a payload is keyed on cannot differ
    /// from the vector it was derived with.
    fn of(target: &rustdar_egui::pane::SectionTarget, motion: Option<(f32, f32)>) -> Self {
        Self {
            site: target.volume.site.clone(),
            collected: target.volume.collected,
            product: target.product,
            ladder: target.ladder,
            storm_motion: motion.map(|(speed, direction)| (speed.to_bits(), direction.to_bits())),
        }
    }
}

/// A finished plan-view raster in egui's pixel layout, or `None` if its length
/// is not one this build can have produced.
///
/// **Where this runs is the point.** It is called from `spawn_render`'s
/// `deliver`, which is the render thread natively — so the unmultiply is off
/// the frame thread entirely — and the browser's main thread, which is the only
/// thread a browser has and where a loop frame has always been converted for
/// the same reason. What the frame thread receives either way is a buffer it
/// hands straight to `Context::load_texture`.
///
/// The side is derived from the length rather than named, because a static
/// render is `IMAGE_SIZE` or `LONG_RANGE_IMAGE_SIZE` square depending on how
/// far its sweep reached and the caller here does not know which. Validating
/// against the closed set is what makes deriving it safe:
/// `ColorImage::from_rgba_unmultiplied` asserts on a mismatch, and this is
/// called on a render worker natively — where a panic means no `RenderResponse`
/// ever arrives, `render_in_flight` never clears, and the pane stays blank for
/// good. `None` instead routes a malformed buffer down the "no matching sweep"
/// path the dispatcher already retires cleanly.
fn plan_view_image(rgba: &[u8]) -> Option<egui::ColorImage> {
    let Some(side) = crate::constants::raster_side_from_rgba_len(rgba.len()) else {
        log::error!(
            "a radar render produced {} bytes, which is no raster size this build makes",
            rgba.len(),
        );
        return None;
    };
    Some(egui::ColorImage::from_rgba_unmultiplied([side, side], rgba))
}

impl Default for RenderDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderDispatcher {
    /// A dispatcher for this build's own budgets.
    ///
    /// The convenience arm over [`with_budgets`](Self::with_budgets), for the
    /// tests that are about invalidation rather than about capacity. The
    /// application takes the other one, so the two numbers this struct spends
    /// arrive from the same resolved `Budgets` everything else reads.
    pub fn new() -> Self {
        Self::with_budgets(&crate::budget::resolve(
            &crate::budget::DeviceProfile::for_target(),
        ))
    }

    /// A dispatcher holding the concurrency and cache budgets it is handed.
    ///
    /// Handed rather than read from a `cfg` constant, for the reason
    /// `volume::quality::select` and `LoopPool::for_device` take theirs as
    /// parameters: this workspace runs `cargo test` on exactly one of three
    /// arms, and a budget read inline is a budget checkable on one row.
    pub fn with_budgets(budgets: &Budgets) -> Self {
        Self {
            pane_render: vec![PaneRenderState::new()],
            level3_data: HashMap::new(),
            env_heights: HashMap::new(),
            melting_layer: HashMap::new(),
            render_generation: 0,
            fetch_generations: HashMap::new(),
            // Owned here so there is exactly one render budget counter in the process.
            renders_in_flight: Arc::new(AtomicUsize::new(0)),
            render_cache: RenderCache::new(budgets.render_cache_entries),
            concurrent_renders: budgets.concurrent_renders,
            long_range_raster_ok: false,
            last_storm_motion_override: None,
            section_input: None,
        }
    }

    /// Background radar renders that may be in flight at once.
    ///
    /// The single budget both the loop and the static pane paths spend, so the
    /// two call sites outside this module read it from here rather than each
    /// naming a constant.
    pub fn concurrent_renders(&self) -> usize {
        self.concurrent_renders
    }

    /// Record what the device that has just been created can hold. See
    /// [`long_range_raster_ok`](Self::long_range_raster_ok).
    ///
    /// Called where the device is installed rather than read from there per
    /// dispatch, because the dispatcher outlives the device: a lost surface
    /// drops `AppState` and a new one is built, and a dispatcher that reached
    /// for a device would have to answer the question with no device in hand.
    pub fn set_long_range_raster_ok(&mut self, ok: bool) {
        self.long_range_raster_ok = ok;
    }

    /// Whether a **static** render dispatched now may take the long-range
    /// raster size — the value that becomes `JobRequest`'s `full_res`.
    ///
    /// Static, because a loop frame's answer is always `false` by policy; see
    /// `offload::JobRequest::side_ceiling_px` for both halves of that byte.
    fn static_full_res(&self) -> bool {
        self.long_range_raster_ok
    }

    /// Cache a fetched Level III object under the `(AWIPS code, site)` it is.
    ///
    /// The only way into [`level3_data`](Self::level3_data). No product is named:
    /// the object is whatever `code` says it is, and every product that reads
    /// that code reads this one entry.
    pub fn cache_level3(&mut self, code: String, site: String, fetched: Level3Product) {
        self.level3_data.insert((code, site), Arc::new(fetched));
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
        self.render_cache.retain(|(_site, product, _view, _elev)| {
            *product != RadarProduct::StormRelativeVelocity
        });
        true
    }

    /// Record a site's environmental heights and, if the pair actually moved,
    /// drop that site's renders of every product that reads it — the per-site
    /// counterpart of
    /// [`set_storm_motion_override`](Self::set_storm_motion_override), for the
    /// other render parameter that is not part of the cache key. Written by
    /// the sounding drain in `app_render`; the field it writes is the same one
    /// [`env_heights_km_msl_for`](Self::env_heights_km_msl_for) reads into the
    /// render parameters, so the environment a pane is invalidated for cannot
    /// differ from the one it is redrawn with.
    ///
    /// The set is [`RadarProduct::reads_env_heights`] and not a list written
    /// here: this predicate named the hail pair alone while the render
    /// parameters already carried the pair to the hybrid classification too,
    /// so an HCA pane went on showing a default-melting-layer picture until
    /// the volume rolled. Asking the product resolves that by construction —
    /// the invalidation set cannot fall behind the consuming set, because
    /// they are the same sentence.
    ///
    /// An unchanged pair still refreshes the entry — that restarts the TTL the
    /// poll's refetch gate reads — but invalidates nothing: soundings refetch
    /// on a timer and normally land identical, and redrawing every affected
    /// pane each time would repeat hourly for no visible change.
    ///
    /// Returns whether anything was invalidated.
    pub fn set_env_heights(
        &mut self,
        site: &str,
        heights: rustdar_radar::sounding::EnvHeights,
        gui: &rustdar_egui::Gui,
    ) -> bool {
        let unchanged = self.env_heights.get(site).is_some_and(|old| {
            old.h0c_km_msl == heights.h0c_km_msl && old.hm20c_km_msl == heights.hm20c_km_msl
        });
        self.env_heights.insert(site.to_string(), heights);
        if unchanged {
            return false;
        }
        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
            if gui.pane(idx).is_some_and(|p| p.site == site)
                && prs
                    .last_rendered
                    .is_some_and(|(p, _)| p.reads_env_heights())
            {
                prs.last_rendered = None;
            }
        }
        self.render_cache
            .retain(|(s, product, _view, _elev)| s != site || !product.reads_env_heights());
        true
    }

    /// Record a site's `N0M` object for the volume it names and, if that
    /// changes what the classification would stand on, drop the site's
    /// classification renders.
    ///
    /// The per-volume counterpart of
    /// [`set_env_heights`](Self::set_env_heights), and it invalidates on a
    /// narrower set for a narrower reason: exactly one product reads a melting
    /// layer, so exactly one product's pictures can change when one lands.
    ///
    /// The comparison is on the **volume**, not on the bytes. Two objects for
    /// one volume are the same answer however they were keyed, and an object
    /// for a different volume is a different answer even if the bytes were
    /// identical — which is the whole distinction this path is built on.
    ///
    /// Returns whether anything was invalidated.
    pub fn set_melting_layer(
        &mut self,
        site: &str,
        object: MeltingLayerObject,
        gui: &rustdar_egui::Gui,
    ) -> bool {
        let unchanged = self
            .melting_layer
            .get(site)
            .is_some_and(|old| old.volume_start == object.volume_start);
        self.melting_layer.insert(site.to_string(), object);
        if unchanged {
            return false;
        }
        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
            if gui.pane(idx).is_some_and(|p| p.site == site)
                && prs
                    .last_rendered
                    .is_some_and(|(p, _)| p == RadarProduct::HydrometeorClassification)
            {
                prs.last_rendered = None;
            }
        }
        self.render_cache.retain(|(s, product, _view, _elev)| {
            s != site || *product != RadarProduct::HydrometeorClassification
        });
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
                prs.render_finished();
                // Paired with the line above: see `results_wanted`.
                prs.abandon_results();
            }
        }
        self.level3_data.retain(|(_code, s), _| s != site);
        self.render_cache
            .retain(|(s, _prod, _view, _elev)| s != site);
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
    /// The products [`RadarProduct::reads_whole_volume`] names are skipped here:
    /// every one of them would read a volume still being assembled as a complete
    /// short one, with no error and no NaN. What refreshes them is the
    /// `volume_complete` branch of `App::apply_chunk_outcome`, which calls
    /// [`reset_panes_for_site`](Self::reset_panes_for_site) — every pane on the
    /// site, whatever its product. Level III panes are skipped here too, for a
    /// different reason: their pixels come from `level3_data`, which a Level II
    /// cut says nothing about.
    ///
    /// Returns how many panes were invalidated, for the log and the tests.
    pub fn reset_panes_for_tilts(
        &mut self,
        site: &str,
        gui: &rustdar_egui::Gui,
        angles: &[f32],
    ) -> usize {
        let hit = self.invalidate_panes_where(site, gui, |product, elevation| {
            if product.is_level3() || product.reads_whole_volume() {
                return false;
            }
            angles
                .iter()
                .any(|a| (a - elevation).abs() <= rustdar_egui::pane::ELEVATION_TOLERANCE)
        });
        // Only the tilts that changed. A whole-site `retain` would throw away the
        // images the untouched panes are still sharing.
        self.render_cache.retain(|(s, _prod, view, elev)| {
            // Elevation-blind for the vertical views, whose slot is
            // `NO_ELEVATION_SLOT` rather than a tilt: a completed cut changes
            // what a section is cut from whatever tilt the pane names.
            s != site
                || !(match view {
                    RenderView::PlanView => angles.iter().any(|a| elevation_key(*a) == *elev),
                    RenderView::CrossSection | RenderView::Volume => true,
                })
        });
        hit
    }

    /// The `abandon_results` + `render_in_flight` pairing, written once for the
    /// tilt reset above.
    ///
    /// A `reset_panes_for_volume` — the complement of what
    /// [`reset_panes_for_tilts`](Self::reset_panes_for_tilts) skips, i.e. the
    /// whole-volume Level II products on their own — used to sit beside it and go
    /// through here too. It was deleted rather than wired up: the `volume_complete`
    /// branch of `App::apply_chunk_outcome` is the only path that would have
    /// called it, and it needs the *wider*
    /// [`reset_panes_for_site`](Self::reset_panes_for_site) for three separate
    /// reasons. The branch fires at a volume *boundary* — `ChunkPoller::roll`
    /// produces `closed` — and the volume it installs is the one that just ended,
    /// so every pane on the site was drawing the volume before it, not just the
    /// whole-volume readers. The `if/else` there means the closing round's own
    /// `sealed_elevations`, which belong to the volume that just started, never
    /// reach `reset_panes_for_tilts`, so the site reset is what stands in for
    /// them. And `reset_panes_for_site` also drops the site's `level3_data` and
    /// `render_cache`, which the `spawn_level3_fetches` on the next line depends
    /// on and which a pane-only reset does not touch.
    ///
    /// Kept as a separate function anyway: the pairing is the invariant, and it
    /// wants one home whether one caller reads it or two.
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
                prs.render_finished();
                // Paired with the line above: see `results_wanted`.
                prs.abandon_results();
                hit += 1;
            }
        }
        hit
    }

    /// Reset every pane's render state, every site's, and bump
    /// [`render_generation`](Self::render_generation).
    ///
    /// **No production caller, and not for the reason it looks like.** It is worth
    /// recording which, because the chunk feed's volume-close path sits next door
    /// and the two are unrelated. This lost its last caller in March 2026, months
    /// before the real-time feed existed, when the archive drain and the manual
    /// navigation both narrowed to
    /// [`reset_panes_for_site`](Self::reset_panes_for_site) — the right call for
    /// both, since a scan arriving for one site has no business discarding another
    /// site's in-flight renders, which is exactly what bumping the global
    /// generation does. `App::apply_chunk_outcome`'s completed-volume branch wants
    /// the per-site reset for the same reason and calls it.
    ///
    /// So `render_generation` never advances in a running app and
    /// [`is_render_stale`](Self::is_render_stale) is always false. That is not a
    /// hole — per-site resets abandon their panes' renders individually, through
    /// `PaneRenderState::results_wanted` — but it does mean this function and
    /// that counter stand or fall together, which is a judgement for whoever next
    /// needs a global invalidation rather than a thing to delete in passing.
    pub fn reset_panes(&mut self) {
        for prs in &mut self.pane_render {
            prs.last_rendered = None;
            prs.cached_render = None;
            prs.render_finished();
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
        self.pane_render.iter().any(|prs| prs.render_in_flight())
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
        view: RenderView,
        elevation: f32,
    ) -> Option<&CachedRenderOutput> {
        self.render_cache
            .get(&render_cache_key(site, product, view, elevation))
    }

    /// Store a render result in the cache for sharing across panes.
    pub fn cache_render(
        &mut self,
        site: &str,
        product: RadarProduct,
        view: RenderView,
        elevation: f32,
        output: CachedRenderOutput,
    ) {
        self.render_cache
            .insert(render_cache_key(site, product, view, elevation), output);
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
    /// The Level III object for `site` closest to `elevation`, out of the objects
    /// `product` names — matched on the **Product Description Block's** elevation
    /// angle rather than on the AWIPS mnemonic.
    ///
    /// The candidate set is [`RadarProduct::level3_products`], which is what
    /// keeps a shared cache from letting one product read another's field: echo
    /// tops considers `EET` and nothing else, however many other objects the site
    /// has served. A product naming several codes sees all of them here, which is
    /// only meaningful for tilts of one field — VIL density's two inputs are not
    /// that, and it resolves them through
    /// [`cached_by_code`](Self::cached_by_code) instead.
    ///
    /// Ties break on elevation number so a split cut or a SAILS/MRLE repeat,
    /// which share an angle, resolve to the same one every frame — and then on
    /// the AWIPS code, which makes the order **total**. Without that last step
    /// VIL density's two whole-volume inputs, both at elevation 0 and both
    /// numbered 0, compare `Equal` and `min_by` yields whichever the hash
    /// happened to visit first: the field's reported age would flip between
    /// `DVL`'s stamp and `EET`'s from one process to the next. Alphabetical puts
    /// `DVL` first, which is the numerator — the object the field is a density
    /// *of*.
    fn nearest_tilt(
        &self,
        product: RadarProduct,
        site: &str,
        elevation: f32,
    ) -> Option<Arc<Level3Product>> {
        let wanted = product.level3_products()?;
        self.level3_data
            .iter()
            .filter(|((code, s), _l3)| s == site && wanted.contains(&code.as_str()))
            .min_by(|((code_a, _), a), ((code_b, _), b)| {
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
                    .then_with(|| code_a.cmp(code_b))
            })
            .map(|(_, msg)| Arc::clone(msg))
    }

    /// Record on `pane` when the data behind `render` was collected, so the status
    /// bar can say how old the image is.
    ///
    /// **Every product gets one**, from whichever datasource it came from: the
    /// `ProductStamp` time of the Level III object behind it, or the pane's own
    /// Level II volume time. That uniformity is the point — an age drawn only for
    /// the bucket-fetched products let the user read the datasource off the status
    /// bar, and let its absence mean something too.
    ///
    /// Three values have to agree for the Level III answer to mean anything — the
    /// product and elevation of *this* render, and the site of *this* pane — and
    /// they are read here rather than by the caller, which is what makes a
    /// pane that took this image from a sibling's broadcast report the image's
    /// age rather than whatever it was showing before.
    ///
    /// Assigned unconditionally, so switching a pane between datasources replaces
    /// the time rather than leaving the previous one captioning a field it does not
    /// describe.
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
    pub fn stamp_pane_with_data_time(
        &self,
        pane: &mut rustdar_egui::pane::PaneState,
        render: &CachedPaneRender,
    ) {
        // A Level III product's own object, or — for anything read off the volume,
        // derived products included — the volume this pane has loaded. Falling back
        // to the scan time for a Level III product whose stamp is unreadable would
        // report a bucket object as being as fresh as the volume, so the branch is
        // on the product rather than on whether a stamp was found.
        pane.data_time = if render.product.is_level3() {
            self.nearest_tilt(render.product, &pane.site, render.elevation)
                .and_then(|tilt| tilt.stamp.time)
        } else {
            pane.scan_info.as_ref().map(|info| info.timestamp)
        };
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
    /// site's `(0 °C, −20 °C)` pair in km MSL for the products
    /// [`RadarProduct::reads_env_heights`] names, `None` for every other
    /// product — and `None` when no sounding has landed, which the hail
    /// render answers by drawing nothing ([`rustdar_radar::hail`]) and the
    /// hybrid classification by falling back to the operational adaptation
    /// defaults ([`rustdar_radar::hca`]).
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
        product
            .reads_env_heights()
            .then(|| {
                self.env_heights
                    .get(site)
                    .map(|h| (h.h0c_km_msl, h.hm20c_km_msl))
            })
            .flatten()
    }

    /// The `N0M` object a render of `volume_start` may classify against — and
    /// `None` for every other volume, whatever is cached.
    ///
    /// # The comparison is the feature
    ///
    /// The cache holds one object per site and a site produces a new volume
    /// every four to six minutes, so "the object we have" and "the object for
    /// the picture being drawn" agree only in the window between the fetch
    /// landing and the next volume rolling. Outside it the cached object names
    /// the *previous* volume, whose melting layer is a real measurement of a
    /// real atmosphere — and that is exactly what makes handing it over worse
    /// than handing over nothing. `resolve_melting_layer`'s lower rungs know
    /// they are guessing and say so on screen; an object from the volume before
    /// would be reported as [`Rpg`](rustdar_radar::hca::MeltingLayerSource::Rpg)
    /// — measured, for this volume — while being neither.
    ///
    /// There is deliberately no "latest object" fallback for the same reason.
    /// `fetch_product_for_volume` already refuses an object whose PDB does not
    /// name the volume, so nothing unvalidated can reach the map; this is the
    /// other half — a validated object cannot outlive the volume it validated
    /// against.
    ///
    /// Gated on the product as [`env_heights_km_msl_for`](Self::env_heights_km_msl_for)
    /// is. `RenderInput::with_melting_layer_product` drops the object for every
    /// other product anyway, so this gate buys no correctness — it buys not
    /// cloning an `Arc` and not describing a render as classification-shaped
    /// when it is a reflectivity sweep.
    pub(crate) fn melting_layer_product_for(
        &self,
        product: RadarProduct,
        site: &str,
        volume_start: chrono::NaiveDateTime,
    ) -> Option<Arc<Vec<u8>>> {
        if product != RadarProduct::HydrometeorClassification {
            return None;
        }
        let cached = self.melting_layer.get(site)?;
        (cached.volume_start == volume_start).then(|| Arc::clone(&cached.bytes))
    }

    /// The volume the site's cached `N0M` object names, if there is one.
    ///
    /// The fetch gate's read: a poll that would ask for an object already in
    /// hand for this volume asks for nothing instead.
    pub(crate) fn melting_layer_volume(&self, site: &str) -> Option<chrono::NaiveDateTime> {
        self.melting_layer.get(site).map(|held| held.volume_start)
    }

    /// The object cached for one `(AWIPS code, site)`.
    ///
    /// The by-code counterpart of [`nearest_tilt`](Self::nearest_tilt), for a
    /// product whose cached objects are not tilts of itself but the **inputs** of
    /// a derivation: VIL density's `DVL` and `EET` (`rustdar_radar::vild`).
    /// Selecting those by nearest PDB elevation would be meaningless — both are
    /// whole-volume products at elevation 0 — and would resolve by hash order.
    ///
    /// `product` is taken so the caller cannot ask for an object the product does
    /// not name: it is the same restriction `nearest_tilt` applies, written once
    /// per resolution path rather than trusted to the two call sites below.
    fn cached_by_code(
        &self,
        product: RadarProduct,
        site: &str,
        code: &str,
    ) -> Option<Arc<Level3Product>> {
        if !product.level3_products()?.contains(&code) {
            return None;
        }
        self.level3_data
            .get(&(code.to_string(), site.to_string()))
            .map(Arc::clone)
    }

    /// Spawn a Level III render for a pane if applicable.
    /// Returns `true` if a render was spawned.
    ///
    /// Storm-relative velocity never comes through here any more: it is a
    /// Level II product, derived where the Level II render runs — see
    /// [`spawn_level2_render`](Self::spawn_level2_render) and
    /// [`rustdar_radar::srv`].
    ///
    /// VIL density takes the two-object path: it is derived from `DVL` over
    /// `EET`, so both have to be in hand before anything can be drawn, and the
    /// radar crate refuses the pair outright if they are not from the same
    /// volume scan (`rustdar_radar::vild::Refusal`). `false` here — no render
    /// spawned — is the same answer a product with no cached object gets, so
    /// the pane keeps whatever it was showing and tries again next frame, which
    /// is what happens for the volume or two while only one of the pair has
    /// landed.
    pub fn try_spawn_level3_render(
        &mut self,
        pane_idx: usize,
        params: &RenderParams,
        site: &str,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
    ) -> bool {
        if params.product == RadarProduct::VilDensity {
            let (Some(dvl), Some(eet)) = (
                self.cached_by_code(params.product, site, "DVL"),
                self.cached_by_code(params.product, site, "EET"),
            ) else {
                return false;
            };
            log::info!("Spawning VIL density render for pane {pane_idx} from DVL over EET");
            self.spawn_render(
                pane_idx,
                site,
                params.product,
                params.elevation,
                sender,
                window,
                crate::offload::Job::Described(crate::offload::JobRequest::Level3Pair {
                    dvl: std::sync::Arc::clone(&dvl.bytes),
                    eet: std::sync::Arc::clone(&eet.bytes),
                    radar_lat: params.lat,
                    radar_lon: params.lon,
                    full_res: self.static_full_res(),
                }),
            );
            return true;
        }

        let Some(l3_msg) = self.nearest_tilt(params.product, site, params.elevation) else {
            return false;
        };

        let lat = params.lat;
        let lon = params.lon;
        let product = params.product;
        // Read before `spawn_render` borrows `self` mutably.
        let full_res_for_this_render = self.static_full_res();

        log::info!(
            "Spawning Level III render for pane {}: {:?}",
            pane_idx,
            product
        );
        self.spawn_render(
            pane_idx,
            site,
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
                full_res: full_res_for_this_render,
            }),
        );
        true
    }

    /// Spawn a Level II render for a pane. `site` names the pane's radar for
    /// the per-site render parameters; the projection geometry still comes
    /// from `params`.
    ///
    /// `declared` is what the volume's cuts said about where their velocity
    /// folds ([`rustdar_radar::nyquist::DeclaredNyquist`]), which `data` cannot
    /// carry — the model type has no field for it. It is a *render* parameter
    /// here and not merely provenance: NROT and SRV dealias, and the interval
    /// they fold around is this number where the volume states one and an
    /// estimate off the sweep's own extremes where it does not. Passing an
    /// empty table is not an error and not a compile failure; it is the
    /// plan view estimating a limit the section pane beside it was told.
    ///
    /// # Budget first, extraction second
    ///
    /// The `render_slot_free` gate below is not a duplicate of the one inside
    /// [`spawn_render`](Self::spawn_render): it is the same gate asked *before*
    /// the extraction rather than after it. `RenderInput::extract` walks the
    /// sweeps this render reads and copies their gates out — a whole-volume
    /// product's payload is every velocity tilt in the volume — and it ran
    /// unconditionally, so a pane that then found the budget full had already
    /// spent that walk on the frame thread and threw the payload away.
    ///
    /// It is not a one-off, which is what makes it worth a gate. Nothing about
    /// a refused dispatch is recorded — no slot taken, no `render_in_flight`
    /// set — so the pane asks again on the very next frame, and pays again, for
    /// as long as the budget stays full. Measured on this volume corpus at
    /// 0.1–1.0 ms a pane for a single tilt and 0.7–2.4 ms for a four-pane split
    /// of storm-relative velocity, *per frame*. It matters most on wasm, where
    /// The web arm's budget is 1 and so any second render at all is a
    /// starved frame for as long as the first one runs.
    ///
    /// This is the shape [`spawn_section_render`](Self::spawn_section_render)
    /// has always had and that [`render_slot_free`](Self::render_slot_free)
    /// exists for; the plan view is joining its siblings on it. Advisory for
    /// the reason that function documents — the count can move underneath —
    /// and a false negative costs a frame's retry, exactly as `spawn_render`'s
    /// own refusal does.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_level2_render(
        &mut self,
        pane_idx: usize,
        params: &RenderParams,
        site: &str,
        data: Arc<nexrad_model::data::Scan>,
        declared: &rustdar_radar::nyquist::DeclaredNyquist,
        volume_start: chrono::NaiveDateTime,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
    ) {
        if !self.render_slot_free() {
            return;
        }
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
        // And the RPG's own melting layer for **this** volume, for the one
        // product that classifies. `volume_start` is a parameter and not a
        // field read because the dispatcher genuinely does not know which
        // volume it is drawing — `data` is the volume and carries no start the
        // pairing agrees on — so this is the one render input the caller has to
        // name. Everything downstream of it is still a field read: the object
        // is looked up in the map `set_melting_layer` invalidates on, and the
        // lookup refuses any volume but this one.
        let melting_layer = self.melting_layer_product_for(product, site, volume_start);
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
                    // Stamped after extraction rather than threaded through it,
                    // which is the shape `with_declared_nyquist` is documented
                    // for: the walk builds the payload out of the sweeps, and
                    // the one thing the sweeps do not carry comes from the
                    // table the caller holds. The wire field already existed
                    // for the vertical views — this is the plan view joining
                    // them on it, so a section and the map under it fold the
                    // same sweep at the same speed.
                    // Stamped alongside the fold table and for the same
                    // reason: the walk builds the payload out of the sweeps,
                    // and the melting layer is not in them. `None` here is
                    // not an error — it is `resolve_melting_layer` falling to
                    // its next rung, which the pane then says out loud.
                    input: Box::new(
                        input
                            .with_declared_nyquist(declared)
                            .with_melting_layer_product(melting_layer),
                    ),
                    // A static pane keeps the grid: it is what a hover reads.
                    values_wanted: true,
                    // And it is the one render kind that may take the
                    // long-range raster, if this device can hold one.
                    full_res: self.static_full_res(),
                })
            }
            None => crate::offload::Job::renders_nothing(),
        };
        self.spawn_render(pane_idx, site, product, elevation, sender, window, job);
    }

    /// The storm motion vector the cached section payload will be **derived**
    /// with, or `None` if no payload is cached.
    ///
    /// The one observable that distinguishes "the section re-derived" from
    /// "the section redrew the previous vector's field", which are otherwise
    /// the same picture arriving at the same time.
    #[cfg(test)]
    pub(crate) fn section_payload_motion(&self) -> Option<Option<(f32, f32)>> {
        self.section_input
            .as_ref()
            .map(|cached| cached.input.storm_motion_override())
    }

    /// Cut a vertical cross-section for a section pane, in the background.
    ///
    /// See [`SectionDispatch`] for the three answers and why "the budget is
    /// full" and "this volume has nothing to cut" must not be the same one.
    /// Native builds have several slots; **wasm has exactly one, with no
    /// preemption**, so on the platform where this matters most a section
    /// queues behind whatever plan view is already rendering rather than
    /// displacing it.
    ///
    /// The volume payload is taken from
    /// [`section_input`](Self::section_input) when it is for this volume, moment
    /// and ladder, and produced by `extract` — the caller's walk over the
    /// merged current volume — when it is not. See that field for what the
    /// walk costs and why one entry is the right size; taking a closure
    /// rather than a `&Scan` is what lets the walk read a volume that is not
    /// one `Scan` without this module holding the app's scan state.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_section_render(
        &mut self,
        pane_idx: usize,
        target: &rustdar_egui::pane::SectionTarget,
        extract: impl FnOnce() -> Option<rustdar_radar::render_input::RenderInput>,
        sender: std::sync::mpsc::Sender<crate::channels::SectionResponse>,
        window: Option<WindowRef>,
    ) -> SectionDispatch {
        // Bounds-checked once, here, rather than left to the two `pane_render`
        // indexes further down. It cannot be out of range today — the only
        // caller reaches this through `pane_render.get(pane_idx)` two lines
        // earlier — but the two indexes straddle the budget increment and the
        // `RenderGuard`, so an out-of-range pane would not merely panic, it
        // would panic with the in-flight count already raised. Returning `false`
        // is the same contract as a full budget: nothing has been taken, no
        // staleness key is written, and the pane asks again next frame.
        if pane_idx >= self.pane_render.len() {
            return SectionDispatch::Busy;
        }
        if self.renders_in_flight.load(Ordering::Relaxed) >= self.concurrent_renders {
            return SectionDispatch::Busy;
        }

        let product = target.product;
        // Read here, off the dispatcher's own field, for the reason
        // `spawn_level2_render` reads it here rather than taking it as an
        // argument: `dispatch_section_renders` has no test callers, so a
        // vector merely forwarded from there would be untested by
        // construction. The `then` gate is the same one the plan-view path
        // uses — only the storm-relative product's payload is a function of
        // the vector, and keying the other eight on it would re-walk 15.6 MB
        // of gates every time the user nudged a vector they were not looking
        // through.
        let motion = (product == RadarProduct::StormRelativeVelocity)
            .then(|| self.storm_motion_override_kt())
            .flatten();
        let wanted_key = SectionInputKey::of(target, motion);
        let reusable = self
            .section_input
            .as_ref()
            .is_some_and(|cached| cached.key == wanted_key);
        if !reusable {
            let Some(input) = extract() else {
                // No sweep carries this moment, or the derivation refused.
                // Not an error and not a job: the caller has taken no budget
                // slot and marked nothing in flight, so there is nothing to
                // unwind — unlike `spawn_level2_render`, which dispatches
                // `renders_nothing` precisely because it has. It IS a state
                // with a name, though, and saying so is what keeps the pane
                // from waiting forever.
                log::info!("no volume payload for a {product:?} section");
                return SectionDispatch::NoPayload;
            };
            self.section_input = Some(SectionInput {
                key: wanted_key,
                input: Arc::new(input),
            });
        }
        // Always `Some`: either it was reusable or it was just written.
        let Some(cached) = self.section_input.as_ref() else {
            return SectionDispatch::Busy;
        };

        let request = rustdar_radar::xsect::SectionRequest {
            start: (target.line.a().lat, target.line.a().lon),
            end: (target.line.b().lat, target.line.b().lon),
            // The site's elevation plus 20 km, which clears every beam in every
            // operational VCP at every range — so the axis clips nothing and is
            // the same height whatever the line is, which is what makes two
            // sections of one storm comparable by eye.
            top_km_msl: None,
            product,
        };

        self.renders_in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(Arc::clone(&self.renders_in_flight));
        let generation = self.render_generation;
        let wanted = self.pane_render[pane_idx].want_result();
        let target = target.clone();

        let job = crate::offload::Job::Described(crate::offload::JobRequest::Section {
            input: Box::new((*cached.input).clone()),
            request,
        });
        crate::offload::offload_job("section-render", job, move |output| {
            let _guard = guard;
            // An output of another kind becomes `None` — "nothing to draw" —
            // which the receiver already handles, with the budget still unwound
            // and the pane still told.
            let section = output.and_then(crate::offload::JobOutput::section);
            if wanted.load(Ordering::Relaxed) {
                let _ = sender.send(crate::channels::SectionResponse {
                    pane_idx,
                    generation,
                    target,
                    section,
                });
            }
            crate::app::notify_redraw(&window);
        });
        // `None`: a section is not a plan view, and the only reader of that key
        // is the plan-view dispatch. See `PaneRenderState::render_started`.
        self.pane_render[pane_idx].render_started(None);
        SectionDispatch::Dispatched
    }

    /// Whether a render slot is free right now — the caller's pre-flight for
    /// work that is only worth paying when a dispatch can actually follow.
    ///
    /// `handle_prepare_volume` reads this **before** running the merged-volume
    /// extraction, the same shape [`Self::spawn_section_render`] has built in:
    /// budget first, extraction only when a slot will be taken. Advisory by
    /// nature — the count can move between this and the spawn — but every
    /// increment happens on the frame thread, so within one handler a `true`
    /// cannot turn stale (workers only ever *free* slots), and a `false` costs
    /// a frame's retry exactly as the spawn's own refusal does.
    pub fn render_slot_free(&self) -> bool {
        self.renders_in_flight.load(Ordering::Relaxed) < self.concurrent_renders
    }

    /// Resample a volume into a voxel grid, away from the frame thread.
    ///
    /// Returns `false` when the render budget is full — the caller dispatches
    /// nothing, opens no `Building` entry, and the level-triggered pane asks
    /// again next frame. Taking a budget slot is what keeps the wasm worker's
    /// FIFO honest: without it a ~150 ms resample could sit queued in front of
    /// the plan-view render of the very sweep that triggered it.
    ///
    /// The reply carries the *target*, not a pane index: the store refcounts
    /// builds by target, and `VolumeStore::complete` resolves every pane
    /// attached to it — including panes that attached after this dispatch.
    pub fn spawn_voxel_build(
        &mut self,
        target: &rustdar_egui::pane::VolumeTarget,
        input: rustdar_radar::render_input::RenderInput,
        request: rustdar_radar::voxel::VoxelRequest,
        sender: std::sync::mpsc::Sender<crate::channels::VoxelResponse>,
        window: Option<WindowRef>,
    ) -> bool {
        if self.renders_in_flight.load(Ordering::Relaxed) >= self.concurrent_renders {
            return false;
        }
        self.renders_in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(Arc::clone(&self.renders_in_flight));
        let target = target.clone();
        let started = web_time::Instant::now();

        let job = crate::offload::Job::Described(crate::offload::JobRequest::Voxels {
            input: Box::new(input),
            request,
        });
        crate::offload::offload_job("voxels", job, move |output| {
            let _guard = guard;
            let grid = output.and_then(crate::offload::JobOutput::voxels);
            // The claim the whole worker move is measured by: the resample no
            // longer spends this time on the frame thread. Logged with the
            // outcome so a refused build is distinguishable from a slow one.
            log::info!(
                "3D volume view: {} for {} in {} ms off the frame thread",
                if grid.is_some() { "built" } else { "no grid" },
                target.volume.site,
                started.elapsed().as_millis(),
            );
            // Sent unconditionally: this message is what resolves the store's
            // `Building` entry, and a build that never reports back leaves
            // every attached pane painting its old grid forever.
            let _ = sender.send(crate::channels::VoxelResponse { target, grid });
            crate::app::notify_redraw(&window);
        });
        true
    }

    /// Shared dispatch for both Level II and Level III renders.
    ///
    /// The tail below — the guard, the cancellation check, the send and the
    /// redraw — is handed to the funnel as `deliver` rather than written into
    /// the job. That is what lets the Level II arm run in a browser worker
    /// without a second copy of it: `deliver` runs on this thread wherever the
    /// rasterization happened, and holds the two things that must not outlive
    /// the render either way.
    ///
    /// `site` is here for one reason: it completes the
    /// [`RenderCacheKey`] this render will be *filed* under, and this is where
    /// that key is recorded as in flight
    /// ([`PaneRenderState::in_flight_plan_view`]). Both callers already hold
    /// the site, and building the key here rather than at each of them is what
    /// makes the dedupe and the cache literally the same key — see that field.
    ///
    /// The extra argument takes the count past clippy's threshold. Bundling
    /// them into a struct would only move the same seven values behind a name
    /// that says nothing the parameters do not.
    #[allow(clippy::too_many_arguments)]
    fn spawn_render(
        &mut self,
        pane_idx: usize,
        site: &str,
        product: RadarProduct,
        elevation: f32,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
        job: crate::offload::Job,
    ) {
        // Check concurrent render limit
        let current = self.renders_in_flight.load(Ordering::Relaxed);
        if current >= self.concurrent_renders {
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
        // `want_result`'s `Arc::strong_count` pruning reads as "still stoppable".
        let wanted = self.pane_render[pane_idx].want_result();
        crate::offload::offload_job("radar-render", job, move |output| {
            let _guard = guard;
            // An output of another kind is `None` here — "nothing to draw",
            // which every path below already handles. See `JobOutput::frame`.
            let frame = output.and_then(crate::offload::JobOutput::frame);
            // Sent whether or not there is a frame, because the receiver is what
            // clears `render_in_flight` and a pane that never hears back stops
            // dispatching. Still gated on `wanted`: an abandoned render must not
            // clear the flag belonging to the render that superseded it.
            //
            // Read once and released immediately, because this read is where
            // the flag stops meaning anything. An abandonment that lands after
            // it cannot stop the send — the decision is already taken — so
            // holding the reference until the thread finishes unwinding would
            // only leave `want_result` reading a render that has already
            // answered as one still running. Released here, the release is
            // sequenced before the send, so a pane holding the result knows
            // the flag behind it is already prunable.
            let still_wanted = wanted.load(Ordering::Relaxed);
            drop(wanted);
            if still_wanted {
                let _ = sender.send(RenderResponse {
                    rendered: frame.and_then(|frame| {
                        // The RGBA bytes are finished with the moment they have
                        // been copied into the `ColorImage`, and this is the
                        // only place that copy happens for a static render — so
                        // this is where they go back to the renderer's slot
                        // rather than to the allocator. Recycled before the `?`
                        // so that a texture the display layer rejects gives its
                        // buffer up too.
                        //
                        // The value grid does *not* go back here. It leaves in
                        // the `Arc` below and is held by the pane, the render
                        // cache and every hover that samples it, so this thread
                        // is not its last owner and there is no moment on this
                        // path at which it is nobody's. See
                        // `rustdar_radar::render::POOLED_VALUES`, which records
                        // what that costs.
                        let picture = plan_view_image(&frame.image);
                        rustdar_radar::render::recycle_image(frame.image);
                        Some(crate::channels::RenderedImage {
                            image: Arc::new(picture?),
                            max_range_km: frame.max_range_km,
                            value_data: Arc::new(frame.values),
                            nyquist_ms: frame.nyquist_ms,
                            melting_layer_source: frame.melting_layer_source,
                        })
                    }),
                    product,
                    elevation,
                    generation,
                    pane_idx,
                });
            }
            crate::app::notify_redraw(&window);
        });
        // The key this render's result will be cached under, recorded as in
        // flight so a sibling pane wanting the same picture asks for nothing.
        // `RenderView::PlanView` because this function dispatches nothing else
        // — the two callers are the Level II and Level III *plan-view* spawns,
        // and a section goes through `spawn_section_render`.
        self.pane_render[pane_idx].render_started(Some(render_cache_key(
            site,
            product,
            RenderView::PlanView,
            elevation,
        )));
    }

    /// Whether some pane already has **this exact plan view** in flight.
    ///
    /// The dispatch pass's answer to "somebody else is already making this".
    /// A linear scan of the pane list, which is at most
    /// [`rustdar_egui::pane::MAX_PANES_DESKTOP`] entries and is walked once per
    /// pane that has missed the render cache — so a set keyed for lookup would
    /// buy nothing but a second structure to keep in step with the flags.
    ///
    /// The key is [`render_cache_key`]'s, which is the whole point: see
    /// [`PaneRenderState::in_flight_plan_view`]. The view is named here rather
    /// than taken, for the same reason `spawn_render` names it: this question
    /// has no meaning for any other view — a section stores no key at all — so
    /// a `view` argument would only be a way to ask it wrongly.
    ///
    /// # A render that panics now wedges a key group, not a pane
    ///
    /// [`plan_view_image`] names the standing hazard: a render that panics
    /// sends no [`RenderResponse`], so `render_in_flight` never clears and that
    /// pane stays blank for good. **This widens the blast radius.** The panes
    /// that deferred to that render are waiting on a result nobody will send
    /// either, so what goes blank is every pane on that key rather than the one
    /// whose render died. Panes on other keys are untouched — nothing here
    /// crosses keys.
    ///
    /// Only the non-deterministic failures reach it. Everything the render
    /// path can fail *predictably* comes back as a `RenderResponse` that drew
    /// nothing — a malformed buffer takes `plan_view_image`'s `None` rather
    /// than the `ColorImage` assertion, and no matching sweep takes
    /// `Job::renders_nothing` — and `render_finished` clears flag and key alike
    /// on every one of them, which is what
    /// `a_render_that_answers_with_nothing_releases_its_siblings` pins. A
    /// reader deciding whether to extend this mechanism should still know the
    /// radius changed.
    ///
    /// # The two `String`s are deliberate
    ///
    /// This builds a whole key to compare it and drops it, and `render_started`
    /// builds the one it stores. Comparing the tuple's parts instead would
    /// avoid both allocations — and would mean writing out
    /// [`render_cache_key`]'s rules a second time: which axis discriminates,
    /// and which slot a tilt-independent product falls in. That the dedupe key
    /// and the cache key have one definition is the entire safety argument for
    /// this suppression (see [`PaneRenderState::in_flight_plan_view`]), and it
    /// is not worth trading for a four-byte `malloc` on a path reached once per
    /// dispatch plus once per deferring pane per frame while its render runs —
    /// tens of allocations against the 70–175 ms of CPU per extra pane, per
    /// volume, that the suppression saves.
    pub fn plan_view_in_flight(&self, site: &str, product: RadarProduct, elevation: f32) -> bool {
        let key = render_cache_key(site, product, RenderView::PlanView, elevation);
        self.pane_render
            .iter()
            .any(|prs| prs.in_flight_plan_view.as_ref() == Some(&key))
    }
}

#[cfg(test)]
mod level3_dispatch_tests;

#[cfg(test)]
mod render_cache_tests;

#[cfg(test)]
mod render_invalidation_tests;

#[cfg(test)]
mod section_payload_cache_tests;

#[cfg(test)]
mod raster_size_tests;

#[cfg(test)]
mod budget_order_tests;
