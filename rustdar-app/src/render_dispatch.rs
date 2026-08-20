use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rustdar_radar::level3::Level3Product;
use rustdar_radar::srm::StormMotionSample;
use rustdar_radar::types::{RadarProduct, RenderView};

use crate::WindowRef;
use crate::channels::RenderResponse;
use crate::render_key::{RenderKey, elevation_key, render_cache_key};
use rustdar_device_profile::budget::Budgets;

/// Drop guard that decrements an AtomicUsize counter on drop.
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
    /// resume is an upload and not a second walk of 64 MiB.
    pub image: Arc<egui::ColorImage>,
    /// The half-width the cached pixels were projected at, km.
    pub max_range_km: f64,
    /// The gates behind these pixels, for the readout — see
    /// [`rustdar_radar::hover::HoverSource`].
    pub hover: Arc<rustdar_radar::hover::HoverSource>,
    pub product: RadarProduct,
    pub elevation: f32,
    /// Where the cached sweep's cut declared its velocity folds, m/s.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer these cached pixels were classified against came from.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where the storm motion vector these cached pixels were shifted by came
    /// from. Kept for exactly the reason the melting layer is: the `N0S`
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
}

/// Per-pane render tracking state.
pub struct PaneRenderState {
    /// True while a background render is in progress for this pane.
    render_in_flight: bool,
    /// Which **plan view** this pane's in-flight render is drawing, as the `(site,
    /// product, view, elevation)` key [`render_cache_key`] builds.
    in_flight_plan_view: Option<RenderKey>,
    /// Last rendered radar parameters to detect changes.
    pub last_rendered: Option<(RadarProduct, f32)>,
    /// Cached render for instant texture restore after suspend/resume.
    pub cached_render: Option<CachedPaneRender>,
    /// One flag per render dispatched for this pane and not yet finished, held
    /// alongside the copy the render thread carries.
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

    /// Mark a render dispatched for this pane, `key` naming the plan view it draws —
    /// `None` for a render that is not a plan view.
    pub fn render_started(&mut self, key: Option<RenderKey>) {
        self.render_in_flight = true;
        self.in_flight_plan_view = key;
    }

    /// Mark this pane's render finished — answered, discarded or abandoned.
    pub fn render_finished(&mut self) {
        self.render_in_flight = false;
        self.in_flight_plan_view = None;
    }

    /// The flag a newly dispatched render reports through, live until this pane's
    /// renders are abandoned.
    fn want_result(&mut self) -> Arc<AtomicBool> {
        self.results_wanted.retain(|f| Arc::strong_count(f) > 1);
        let flag = Arc::new(AtomicBool::new(true));
        self.results_wanted.push(Arc::clone(&flag));
        flag
    }

    /// Stop wanting every render currently running for this pane.
    fn abandon_results(&mut self) {
        for flag in self.results_wanted.drain(..) {
            flag.store(false, Ordering::Relaxed);
        }
    }
}

/// Cached radar render output, shared across panes that show the same product/elevation.
pub struct CachedRenderOutput {
    pub image: Arc<egui::ColorImage>,
    pub max_range_km: f64,
    /// The gates behind this shared raster, shared with it for the reason the
    /// extent is.
    pub hover: Arc<rustdar_radar::hover::HoverSource>,
    /// Where the drawn sweep's cut declared its velocity folds, m/s — shared with the
    /// buffer for the reason the extent is.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer behind this shared raster came from — shared for the
    /// same argument as the two above: one buffer, one classification.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where the storm motion vector behind this shared raster came from — shared for
    /// the same argument as the three above: one buffer.
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
}

/// Bounded least-recently-used cache of render outputs shared between panes.
pub struct RenderCache {
    entries: HashMap<RenderKey, CachedRenderOutput>,
    recency: VecDeque<RenderKey>,
    capacity: usize,
    /// Bytes the resident entries occupy, kept in step with `entries` by
    /// [`Self::insert`], [`Self::retain`] and [`Self::clear`].
    resident_bytes: usize,
    byte_capacity: usize,
}

impl RenderCache {
    /// `capacity` is floored at 1 — a zero-capacity cache would evict every entry
    /// on the way in, which is a silent way to disable pane sharing entirely.
    pub fn new(capacity: usize, byte_capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: VecDeque::new(),
            capacity: capacity.max(1),
            resident_bytes: 0,
            byte_capacity,
        }
    }

    /// What one entry costs: the texture egui holds and the value grid a hover
    /// reads, both `side² × 4`.
    fn entry_bytes(value: &CachedRenderOutput) -> usize {
        value.image.pixels.len() * std::mem::size_of::<egui::Color32>()
            + value.hover.resident_bytes()
    }

    /// Move `key` to the most-recently-used end. No-op if absent.
    fn touch(&mut self, key: &RenderKey) {
        if let Some(pos) = self.recency.iter().position(|k| k == key) {
            let k = self
                .recency
                .remove(pos)
                .expect("position() just yielded it");
            self.recency.push_back(k);
        }
    }

    /// Look up an entry, marking it most-recently-used.
    pub fn get(&mut self, key: &RenderKey) -> Option<&CachedRenderOutput> {
        if !self.entries.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.entries.get(key)
    }

    /// Insert an entry, evicting the least recently used until within **both**
    /// capacities.
    pub fn insert(&mut self, key: RenderKey, value: CachedRenderOutput) {
        let bytes = Self::entry_bytes(&value);
        if let Some(old) = self.entries.insert(key.clone(), value) {
            // Replacing an existing entry: it is already in `recency`, just refresh it.
            self.resident_bytes = self.resident_bytes.saturating_sub(Self::entry_bytes(&old));
            self.touch(&key);
        } else {
            self.recency.push_back(key);
        }
        self.resident_bytes += bytes;
        while self.entries.len() > self.capacity
            || (self.resident_bytes > self.byte_capacity && self.entries.len() > 1)
        {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            if let Some(gone) = self.entries.remove(&oldest) {
                self.resident_bytes = self.resident_bytes.saturating_sub(Self::entry_bytes(&gone));
            }
        }
    }

    /// Drop every entry whose key fails `keep`.
    pub fn retain(&mut self, keep: impl Fn(&RenderKey) -> bool) {
        let freed: usize = self
            .entries
            .iter()
            .filter(|(k, _)| !keep(k))
            .map(|(_, v)| Self::entry_bytes(v))
            .sum();
        self.resident_bytes = self.resident_bytes.saturating_sub(freed);
        self.entries.retain(|k, _| keep(k));
        self.recency.retain(|k| keep(k));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.recency.clear();
        self.resident_bytes = 0;
    }

    /// Bytes the resident entries occupy.
    #[cfg(test)]
    pub fn resident_bytes(&self) -> usize {
        self.resident_bytes
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
    pub fn recency_order(&self) -> Vec<RenderKey> {
        self.recency.iter().cloned().collect()
    }
}

/// Whether a render of `view` showing `product` has to be given the whole
/// volume rather than the one sweep `render::find_sweep` picks.
pub fn needs_whole_volume(view: RenderView, product: RadarProduct) -> bool {
    view.reads_whole_volume() || product.reads_whole_volume()
}

/// Manages radar rendering dispatch and Level III data caching.
pub struct RenderDispatcher {
    /// Per-pane render tracking (indexed by pane index).
    pub pane_render: Vec<PaneRenderState>,
    /// The latest fetched Level III object per `(AWIPS code, site)`.
    level3_data: HashMap<(String, String), Arc<Level3Product>>,
    /// Environmental 0 °C / −20 °C heights per site, from Open-Meteo — staged for the
    /// products [`RadarProduct::reads_env_heights`] names.
    pub env_heights: HashMap<String, rustdar_radar::sounding::EnvHeights>,
    /// The RPG's own Melting Layer object per site — the top rung of
    /// `rustdar_radar::hca::resolve_melting_layer`.
    melting_layer: HashMap<String, MeltingLayerObject>,
    /// The RPG's own storm motion vector per site — the second rung of
    /// `rustdar_radar::srv::storm_motion`.
    storm_motion: HashMap<String, StormMotionObject>,
    /// Generation counter to discard stale render results after a **full** reset.
    pub render_generation: u64,
    /// Per-site fetch generation counters to discard stale fetch results.
    pub fetch_generations: HashMap<String, u64>,
    /// Shared counter for concurrent background render threads.
    pub renders_in_flight: Arc<AtomicUsize>,
    /// Cache of the latest render output per (site, product, elevation_tenths), shared
    /// across panes that display the same product at the same elevation on the same site.
    pub render_cache: RenderCache,
    /// Background radar renders that may be in flight at once — this build's
    /// `Budgets::concurrent_renders`, held rather than read from a `cfg`
    concurrent_renders: usize,
    /// The largest plan-view raster the device this process is drawing on can hold —
    /// `AppState::raster_side_ceiling_px`.
    raster_side_ceiling_px: usize,
    /// The storm motion override the storm-relative renders on screen were built with.
    last_storm_motion_override: Option<StormMotionSample>,
    /// Which derived rung the storm-relative renders on screen fell to when no
    /// override and no RPG vector applied — the reader's own choice.
    last_srv_fallback: rustdar_radar::srv::SrvFallback,
    /// The last whole-volume payload extracted for a cross-section, and what it
    /// was extracted from.
    section_input: Option<SectionInput>,
    /// Plan-view extraction payloads.
    extract_cache: HashMap<ExtractKey, Arc<rustdar_radar::render_input::RenderInput>>,
    /// The recency queue of `extract_cache`, oldest first — the same private
    /// pairing [`RenderCache`] keeps, for the same reason.
    extract_recency: VecDeque<ExtractKey>,
    /// The dispatcher-local mpsc the native arrival-time extractions home over.
    // On wasm the arrival drain populates inline and nothing sends.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    extract_results_tx: std::sync::mpsc::Sender<ExtractResult>,
    extract_results_rx: std::sync::mpsc::Receiver<ExtractResult>,
    /// Frame-thread plan-view extractions performed at dispatch — the frame-thread-
    /// payment probe for the plan view path.
    #[cfg(test)]
    pub(crate) plan_view_extractions: std::cell::Cell<u32>,
    /// Whether an adjacent-tilt pre-render is out.
    speculative_in_flight: bool,
}

/// The identity of one plan-view extraction — **today's tuple, exactly the
/// arguments [`rustdar_radar::render_input::RenderInput::extract`] takes**
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ExtractKey {
    pub(crate) site: String,
    pub(crate) volume_start: chrono::NaiveDateTime,
    pub(crate) product: RadarProduct,
    pub(crate) elevation_bits: u32,
    pub(crate) storm_motion_bits: Option<(u32, u32)>,
    pub(crate) env_heights_bits: Option<(u64, u64)>,
}

/// One homed arrival-time extraction: the key it was computed under and the
/// unstamped payload — the four stamps are applied at dispatch.
pub(crate) type ExtractResult = (ExtractKey, Arc<rustdar_radar::render_input::RenderInput>);

/// The extract tuple the arrival hook and the dispatch share: the cache key
/// plus the two extraction arguments its float bits came from.
pub(crate) type ExtractTuple = (ExtractKey, Option<(f32, f32)>, Option<(f64, f64)>);

/// At most this many extraction payloads stay resident: one per pane a desktop
/// split can show, roughly.
const EXTRACT_CACHE_CAP: usize = 8;

/// Whether this build may pre-render an adjacent tilt at all — the
/// platform/budget half of the speculative gate.
pub(crate) fn speculative_render_allowed(web: bool, concurrent_renders: usize) -> bool {
    !web && concurrent_renders > 2
}

/// One rendered frame's trip into the shape the frame thread applies.
fn rendered_image_from(
    frame: rustdar_radar::frame::RenderedFrame,
) -> Option<crate::channels::RenderedImage> {
    let picture = plan_view_image(&frame.image);
    rustdar_radar::render::recycle_image(frame.image);
    Some(crate::channels::RenderedImage {
        image: Arc::new(picture?),
        max_range_km: frame.max_range_km,
        hover: Arc::new(rustdar_radar::hover::HoverSource::resident(frame.polar)),
        nyquist_ms: frame.nyquist_ms,
        melting_layer_source: frame.melting_layer_source,
        storm_motion: frame.storm_motion,
    })
}

/// The one place an [`ExtractKey`] is built, so the arrival-time populate and the
/// dispatch lookup cannot key the same tuple two ways.
fn extract_key(
    site: &str,
    volume_start: chrono::NaiveDateTime,
    product: RadarProduct,
    elevation: f32,
    storm_motion: Option<(f32, f32)>,
    env_heights: Option<(f64, f64)>,
) -> ExtractKey {
    ExtractKey {
        site: site.to_string(),
        volume_start,
        product,
        elevation_bits: elevation.to_bits(),
        storm_motion_bits: storm_motion.map(|(s, d)| (s.to_bits(), d.to_bits())),
        env_heights_bits: env_heights.map(|(h0, hm20)| (h0.to_bits(), hm20.to_bits())),
    }
}

/// One site's `N0M` object **and the volume start it names**.
pub struct MeltingLayerObject {
    /// The Level II volume start this object's PDB names, already validated by
    /// `rustdar_radar::level3::fetch_product_for_volume`.
    pub volume_start: chrono::NaiveDateTime,
    pub bytes: Arc<Vec<u8>>,
}

/// One site's `N0S` storm motion vector **and the volume start it names**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StormMotionObject {
    /// The Level II volume start this object's PDB named, already validated by
    /// `rustdar_radar::level3::fetch_product_for_volume`.
    pub volume_start: chrono::NaiveDateTime,
    /// `(speed_kt, direction_from_deg)`, exactly as the PDB stated them.
    pub motion: (f32, f32),
}

/// A whole-volume payload and the volume it came out of.
struct SectionInput {
    key: SectionInputKey,
    /// `Arc` so the cache and the job in flight can hold it at once; the job needs an
    /// owned `RenderInput`.
    input: Arc<rustdar_radar::render_input::RenderInput>,
}

/// What a section dispatch did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionDispatch {
    /// A cut is in flight. The caller writes the staleness key.
    Dispatched,
    /// The render budget is full, or the pane index is out of range. Nothing
    /// was taken and nothing is wrong: ask again next frame, key unwritten.
    Busy,
    /// This volume carries no field to cut under this product — no sweep holds the
    /// moment, or the derivation refused it.
    NoPayload,
}

/// What a cached section payload is a payload *of*.
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
    storm_motion: Option<(u32, u32)>,
    /// Which derived rung the payload was derived with when the field above is
    /// `None`.
    srv_fallback: rustdar_radar::srv::SrvFallback,
}

impl SectionInputKey {
    /// The key a payload would have to carry to serve `target` under the
    /// storm motion vector `motion`.
    fn of(
        target: &rustdar_egui::pane::SectionTarget,
        motion: Option<(f32, f32)>,
        fallback: rustdar_radar::srv::SrvFallback,
    ) -> Self {
        Self {
            site: target.volume.site.clone(),
            collected: target.volume.collected,
            product: target.product,
            ladder: target.ladder,
            storm_motion: motion.map(|(speed, direction)| (speed.to_bits(), direction.to_bits())),
            srv_fallback: fallback,
        }
    }
}

/// A finished plan-view raster in egui's pixel layout, or `None` if its length
/// is not one this build can have produced.
fn plan_view_image(rgba: &[u8]) -> Option<egui::ColorImage> {
    let Some(side) = rustdar_device_profile::constants::raster_side_from_rgba_len(rgba.len())
    else {
        log::error!(
            "a radar render produced {} bytes, which is no raster size this build makes",
            rgba.len(),
        );
        return None;
    };
    Some(egui::ColorImage::from_rgba_premultiplied(
        [side, side],
        rgba,
    ))
}

impl Default for RenderDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderDispatcher {
    /// A dispatcher for this build's own budgets.
    pub fn new() -> Self {
        Self::with_budgets(&rustdar_device_profile::budget::resolve(
            &rustdar_device_profile::budget::DeviceProfile::for_target(),
        ))
    }

    /// A dispatcher holding the concurrency and cache budgets it is handed.
    pub fn with_budgets(budgets: &Budgets) -> Self {
        // The extract-results pair is dispatcher-local: it is not a ChannelHub
        // channel and never becomes one.
        let (extract_results_tx, extract_results_rx) = std::sync::mpsc::channel();
        Self {
            pane_render: vec![PaneRenderState::new()],
            level3_data: HashMap::new(),
            env_heights: HashMap::new(),
            melting_layer: HashMap::new(),
            storm_motion: HashMap::new(),
            render_generation: 0,
            fetch_generations: HashMap::new(),
            // Owned here so there is exactly one render budget counter in the process.
            renders_in_flight: Arc::new(AtomicUsize::new(0)),
            render_cache: RenderCache::new(
                budgets.render_cache_entries,
                budgets.render_cache_budget_bytes(),
            ),
            concurrent_renders: budgets.concurrent_renders,
            raster_side_ceiling_px: budgets.image_side_px,
            last_storm_motion_override: None,
            last_srv_fallback: rustdar_radar::srv::SrvFallback::default(),
            section_input: None,
            extract_cache: HashMap::new(),
            extract_recency: VecDeque::new(),
            extract_results_tx,
            extract_results_rx,
            #[cfg(test)]
            plan_view_extractions: std::cell::Cell::new(0),
            speculative_in_flight: false,
        }
    }

    /// Background radar renders that may be in flight at once.
    pub fn concurrent_renders(&self) -> usize {
        self.concurrent_renders
    }

    /// Record what the device that has just been created can hold. See
    /// [`raster_side_ceiling_px`](Self::raster_side_ceiling_px).
    pub fn set_raster_side_ceiling_px(&mut self, side: usize) {
        self.raster_side_ceiling_px = side;
    }

    /// The ceiling a **static** render dispatched now may take — the number
    /// that becomes the request envelope's `side_ceiling_px`.
    fn static_side_ceiling_px(&self) -> usize {
        self.raster_side_ceiling_px
    }

    /// Cache a fetched Level III object under the `(AWIPS code, site)` it is.
    pub fn cache_level3(&mut self, code: String, site: String, fetched: Level3Product) {
        self.level3_data.insert((code, site), Arc::new(fetched));
    }

    /// Record the storm motion override in force and, if it moved, drop every
    /// storm-relative render that used the old one.
    pub fn set_storm_motion_choice(
        &mut self,
        motion: Option<StormMotionSample>,
        fallback: rustdar_radar::srv::SrvFallback,
    ) -> bool {
        if self.last_storm_motion_override == motion && self.last_srv_fallback == fallback {
            return false;
        }
        self.last_storm_motion_override = motion;
        self.last_srv_fallback = fallback;
        for prs in &mut self.pane_render {
            if matches!(
                prs.last_rendered,
                Some((RadarProduct::StormRelativeVelocity, _))
            ) {
                prs.last_rendered = None;
            }
        }
        self.render_cache
            .retain(|k| k.select.product != RadarProduct::StormRelativeVelocity);
        true
    }

    /// Record a site's environmental heights and, if the pair actually moved, drop that
    /// site's renders of every product that reads it.
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
            if gui.pane(idx).is_some_and(|p| p.site() == site)
                && prs
                    .last_rendered
                    .is_some_and(|(p, _)| p.reads_env_heights())
            {
                prs.last_rendered = None;
            }
        }
        self.render_cache
            .retain(|k| k.select.site != site || !k.select.product.reads_env_heights());
        true
    }

    /// Record a site's `N0M` object for the volume it names, dropping the renders
    /// whose classification it changes.
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
            if gui.pane(idx).is_some_and(|p| p.site() == site)
                && prs
                    .last_rendered
                    .is_some_and(|(p, _)| p == RadarProduct::HydrometeorClassification)
            {
                prs.last_rendered = None;
            }
        }
        self.render_cache.retain(|k| {
            k.select.site != site || k.select.product != RadarProduct::HydrometeorClassification
        });
        true
    }

    /// Record a site's `N0S` storm motion vector for the volume it names, dropping
    /// the storm-relative renders it changes.
    pub fn set_storm_motion(
        &mut self,
        site: &str,
        object: StormMotionObject,
        gui: &rustdar_egui::Gui,
    ) -> bool {
        let unchanged = self
            .storm_motion
            .get(site)
            .is_some_and(|old| old.volume_start == object.volume_start);
        self.storm_motion.insert(site.to_string(), object);
        if unchanged {
            return false;
        }
        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
            if gui.pane(idx).is_some_and(|p| p.site() == site)
                && prs
                    .last_rendered
                    .is_some_and(|(p, _)| p == RadarProduct::StormRelativeVelocity)
            {
                prs.last_rendered = None;
            }
        }
        self.render_cache.retain(|k| {
            k.select.site != site || k.select.product != RadarProduct::StormRelativeVelocity
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
    pub fn reset_panes_for_site(&mut self, site: &str, gui: &rustdar_egui::Gui) {
        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
            if gui.pane(idx).is_some_and(|p| p.site() == site) {
                prs.last_rendered = None;
                prs.cached_render = None;
                prs.render_finished();
                // Paired with the line above: see `results_wanted`.
                prs.abandon_results();
            }
        }
        self.level3_data.retain(|(_code, s), _| s != site);
        self.render_cache.retain(|k| k.select.site != site);
    }

    /// The narrow counterpart to [`reset_panes_for_site`], for the real-time
    /// chunk feed: one elevation cut completed, not a whole volume.
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
        self.render_cache.retain(|k| {
            // Elevation-blind for the vertical views, whose elevation part is absent
            // rather than a tilt.
            k.select.site != site
                || !(match k.view {
                    RenderView::PlanView => angles
                        .iter()
                        .any(|a| k.select.elevation_tenths == Some(elevation_key(*a))),
                    RenderView::CrossSection | RenderView::Volume => true,
                })
        });
        hit
    }

    /// The `abandon_results` + `render_in_flight` pairing, written once for the
    /// tilt reset above.
    fn invalidate_panes_where(
        &mut self,
        site: &str,
        gui: &rustdar_egui::Gui,
        mut want: impl FnMut(RadarProduct, f32) -> bool,
    ) -> usize {
        let mut hit = 0;
        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
            let matches = gui.pane(idx).is_some_and(|p| p.site() == site)
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
    /// `product` names.
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
    pub fn data_time_for_render(
        &self,
        pane: &rustdar_egui::pane::PaneState,
        render: &CachedPaneRender,
    ) -> Option<chrono::NaiveDateTime> {
        // A Level III product's own object, or — for anything read off the volume,
        // derived products included — the volume this pane has loaded.
        if render.product.is_level3() {
            self.nearest_tilt(render.product, pane.site(), render.elevation)
                .and_then(|tilt| tilt.stamp.time)
        } else {
            pane.scan_info.as_ref().map(|info| info.timestamp)
        }
    }

    /// The storm motion override as the `(speed_kt, direction_deg)` pair the
    /// Level II render parameters carry, or `None` — a lower rung applies.
    pub(crate) fn storm_motion_override_kt(&self) -> Option<(f32, f32)> {
        self.last_storm_motion_override
            .map(|s| (s.motion.speed_kt, s.motion.direction_deg))
    }

    /// [`set_storm_motion_choice`](Self::set_storm_motion_choice) with the
    /// shipped derived rung, for a test whose subject is the override alone.
    #[cfg(test)]
    pub(crate) fn set_storm_motion_choice_default(
        &mut self,
        motion: Option<StormMotionSample>,
    ) -> bool {
        self.set_storm_motion_choice(motion, rustdar_radar::srv::SrvFallback::default())
    }

    /// Which derived rung a Level II render's payload should carry.
    pub(crate) fn srv_fallback(&self) -> rustdar_radar::srv::SrvFallback {
        self.last_srv_fallback
    }

    /// The environmental heights a Level II render's parameters carry: the site's
    /// `(0 °C, −20 °C)` pair in km MSL, for the products that read them.
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
        rustdar_radar::scan::names_same_volume(cached.volume_start, volume_start)
            .then(|| Arc::clone(&cached.bytes))
    }

    /// The volume the site's cached `N0M` object names, if there is one.
    pub(crate) fn melting_layer_volume(&self, site: &str) -> Option<chrono::NaiveDateTime> {
        self.melting_layer.get(site).map(|held| held.volume_start)
    }

    /// The RPG's own storm motion vector for **this** volume of this site, or
    /// `None`.
    pub(crate) fn rpg_storm_motion_for(
        &self,
        product: RadarProduct,
        site: &str,
        volume_start: chrono::NaiveDateTime,
    ) -> Option<(f32, f32)> {
        if product != RadarProduct::StormRelativeVelocity {
            return None;
        }
        let cached = self.storm_motion.get(site)?;
        rustdar_radar::scan::names_same_volume(cached.volume_start, volume_start)
            .then_some(cached.motion)
    }

    /// The volume the site's cached `N0S` vector names, if there is one.
    pub(crate) fn storm_motion_volume(&self, site: &str) -> Option<chrono::NaiveDateTime> {
        self.storm_motion.get(site).map(|held| held.volume_start)
    }

    /// The object cached for one `(AWIPS code, site)`.
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
                rustdar_worker::offload::Job::Described(
                    rustdar_worker::offload::JobRequest::describe(
                        rustdar_radar::jobs::Level3PairJob {
                            dvl: std::sync::Arc::clone(&dvl.bytes),
                            eet: std::sync::Arc::clone(&eet.bytes),
                            radar_lat: params.lat,
                            radar_lon: params.lon,
                        },
                        rustdar_worker::offload::ceiling_only_geometry(
                            self.static_side_ceiling_px() as u32,
                        ),
                    ),
                ),
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
        let ceiling_for_this_render = self.static_side_ceiling_px() as u32;

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
            // The product's bytes rather than its decoded form: a `Level3Message` has
            // no wire form.
            rustdar_worker::offload::Job::Described(rustdar_worker::offload::JobRequest::describe(
                rustdar_radar::jobs::Level3Job {
                    bytes: std::sync::Arc::clone(&l3_msg.bytes),
                    product,
                    radar_lat: lat,
                    radar_lon: lon,
                },
                rustdar_worker::offload::ceiling_only_geometry(ceiling_for_this_render),
            )),
        );
        true
    }

    /// Spawn a Level II render for a pane.
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
        // The storm motion override rides the render parameters for the one product
        // that reads it.
        let (key, storm_motion, env_heights) =
            self.extract_tuple_for(site, volume_start, product, elevation);
        // And the RPG's own melting layer for **this** volume, for the one product that
        // classifies.
        let melting_layer = self.melting_layer_product_for(product, site, volume_start);
        // And the RPG's own storm motion for **this** volume, for the one product that
        // shifts by one.
        let rpg_storm_motion = self.rpg_storm_motion_for(product, site, volume_start);
        log::info!(
            "Spawning background render for pane {}: {:?} at {:.1}°",
            pane_idx,
            product,
            elevation
        );
        // Extracted here, against the volume, because the volume is the thing that must
        // not travel.
        let cached = self
            .extract_cache_lookup(&key)
            .map(|input| (*input).clone());
        #[cfg(test)]
        if cached.is_none() {
            self.plan_view_extractions
                .set(self.plan_view_extractions.get() + 1);
        }
        let extracted = cached.or_else(|| {
            rustdar_radar::render_input::RenderInput::extract(
                &data,
                elevation,
                product,
                lat,
                lon,
                storm_motion,
                env_heights,
            )
        });
        let job = match extracted {
            Some(input) => {
                rustdar_worker::offload::Job::Described(
                    rustdar_worker::offload::JobRequest::describe(
                        rustdar_radar::jobs::RadarPlanJob {
                            // Stamped after extraction rather than threaded through it.
                            input: Box::new(
                                input
                                    .with_declared_nyquist(declared)
                                    .with_srv_fallback(self.last_srv_fallback)
                                    .with_melting_layer_product(melting_layer)
                                    .with_rpg_storm_motion(rpg_storm_motion),
                            ),
                            // A static pane keeps the grid: it is what a hover reads.
                            values_wanted: true,
                        },
                        // And it is the one render kind that may take the
                        // long-range raster, if this device can hold one.
                        rustdar_worker::offload::ceiling_only_geometry(
                            self.static_side_ceiling_px() as u32,
                        ),
                    ),
                )
            }
            None => rustdar_worker::offload::Job::renders_nothing(),
        };
        self.spawn_render(pane_idx, site, product, elevation, sender, window, job);
    }

    /// The extract tuple a pane's arrival-time populate must build — **the same reads
    /// the dispatch above makes**, off the same fields.
    pub(crate) fn extract_tuple_for(
        &self,
        site: &str,
        volume_start: chrono::NaiveDateTime,
        product: RadarProduct,
        elevation: f32,
    ) -> ExtractTuple {
        let storm_motion = (product == RadarProduct::StormRelativeVelocity)
            .then(|| self.storm_motion_override_kt())
            .flatten();
        let env_heights = self.env_heights_km_msl_for(product, site);
        (
            extract_key(
                site,
                volume_start,
                product,
                elevation,
                storm_motion,
                env_heights,
            ),
            storm_motion,
            env_heights,
        )
    }

    /// The cached extraction for `key`, marked most-recently-used.
    fn extract_cache_lookup(
        &mut self,
        key: &ExtractKey,
    ) -> Option<Arc<rustdar_radar::render_input::RenderInput>> {
        if !self.extract_cache.contains_key(key) {
            return None;
        }
        if let Some(pos) = self.extract_recency.iter().position(|k| k == key) {
            let k = self
                .extract_recency
                .remove(pos)
                .expect("position() just yielded it");
            self.extract_recency.push_back(k);
        }
        self.extract_cache.get(key).map(Arc::clone)
    }

    /// File an arrival-time extraction under its key, evicting the least recently used
    /// past [`EXTRACT_CACHE_CAP`].
    pub(crate) fn populate_extract(
        &mut self,
        key: ExtractKey,
        input: Arc<rustdar_radar::render_input::RenderInput>,
    ) {
        if self.extract_cache.insert(key.clone(), input).is_none() {
            self.extract_recency.push_back(key);
        } else if let Some(pos) = self.extract_recency.iter().position(|k| k == &key) {
            let k = self
                .extract_recency
                .remove(pos)
                .expect("position() just yielded it");
            self.extract_recency.push_back(k);
        }
        while self.extract_cache.len() > EXTRACT_CACHE_CAP {
            let Some(oldest) = self.extract_recency.pop_front() else {
                break;
            };
            self.extract_cache.remove(&oldest);
        }
    }

    /// Drain the extract-results channel into the cache — the body of the
    /// `poll_extract_results` FRAME_PUMP row.
    pub(crate) fn poll_extract_results(&mut self) {
        while let Ok((key, input)) = self.extract_results_rx.try_recv() {
            self.populate_extract(key, input);
        }
    }

    /// The sender the native arrival-time extraction homes over, cloned per spawned
    /// walk.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn extract_sender(&self) -> std::sync::mpsc::Sender<ExtractResult> {
        self.extract_results_tx.clone()
    }

    /// Drop every cached extraction that fails `keep` — the volume-eviction pass's
    /// hook; entries die with their volume.
    pub(crate) fn retain_extracts(&mut self, keep: impl Fn(&ExtractKey) -> bool) {
        self.extract_cache.retain(|k, _| keep(k));
        self.extract_recency.retain(|k| keep(k));
    }

    /// How many extractions are resident — the populate tests' observable.
    #[cfg(test)]
    pub(crate) fn extract_cache_len(&self) -> usize {
        debug_assert_eq!(
            self.extract_cache.len(),
            self.extract_recency.len(),
            "extract recency queue out of step"
        );
        self.extract_cache.len()
    }

    /// The sites of the resident extractions — the shown-panes-only proof.
    #[cfg(test)]
    pub(crate) fn extract_cache_sites(&self) -> Vec<String> {
        let mut sites: Vec<String> = self.extract_cache.keys().map(|k| k.site.clone()).collect();
        sites.sort();
        sites
    }

    /// The storm motion vector the cached section payload will be **derived**
    /// with, or `None` if no payload is cached.
    #[cfg(test)]
    pub(crate) fn section_payload_motion(&self) -> Option<Option<(f32, f32)>> {
        self.section_input
            .as_ref()
            .map(|cached| cached.input.storm_motion_override())
    }

    /// Cut a vertical cross-section for a section pane, in the background.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_section_render(
        &mut self,
        pane_idx: usize,
        target: &rustdar_egui::pane::SectionTarget,
        extract: impl FnOnce() -> Option<rustdar_radar::render_input::RenderInput>,
        sender: std::sync::mpsc::Sender<crate::channels::SectionResponse>,
        window: Option<WindowRef>,
    ) -> SectionDispatch {
        // Bounds-checked once, here, rather than left to the two `pane_render` indexes
        // further down.
        if pane_idx >= self.pane_render.len() {
            return SectionDispatch::Busy;
        }
        if self.renders_in_flight.load(Ordering::Relaxed) >= self.concurrent_renders {
            return SectionDispatch::Busy;
        }

        let product = target.product;
        // Read here, off the dispatcher's own field.
        let motion = (product == RadarProduct::StormRelativeVelocity)
            .then(|| self.storm_motion_override_kt())
            .flatten();
        let wanted_key = SectionInputKey::of(target, motion, self.srv_fallback());
        let reusable = self
            .section_input
            .as_ref()
            .is_some_and(|cached| cached.key == wanted_key);
        if !reusable {
            let Some(input) = extract() else {
                // No sweep carries this moment, or the derivation refused.
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
            // operational VCP at every range.
            top_km_msl: None,
            product,
        };

        self.renders_in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(Arc::clone(&self.renders_in_flight));
        let generation = self.render_generation;
        let wanted = self.pane_render[pane_idx].want_result();
        let target = target.clone();

        let job =
            rustdar_worker::offload::Job::Described(rustdar_worker::offload::JobRequest::describe(
                rustdar_radar::jobs::SectionJob {
                    input: Box::new((*cached.input).clone()),
                    request,
                },
                // A section's raster is a constant of the view, so its envelope
                // carries no ceiling — the same effective 0 it has always had.
                rustdar_worker::offload::ceiling_only_geometry(0),
            ));
        rustdar_worker::offload::offload_job("section-render", job, move |output| {
            let _guard = guard;
            // An output of another kind becomes `None` — "nothing to draw" — which the
            // receiver already handles.
            let section = output
                .and_then(|out| out.take::<rustdar_radar::xsect::CrossSection>())
                .map(Box::new);
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
    pub fn render_slot_free(&self) -> bool {
        self.renders_in_flight.load(Ordering::Relaxed) < self.concurrent_renders
    }

    /// Resample a volume into a voxel grid, away from the frame thread.
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

        let job =
            rustdar_worker::offload::Job::Described(rustdar_worker::offload::JobRequest::describe(
                rustdar_radar::jobs::VoxelJob {
                    input: Box::new(input),
                    request,
                },
                // A voxel grid's shape is already on the wire, so its envelope
                // carries no ceiling — the same effective 0 it has always had.
                rustdar_worker::offload::ceiling_only_geometry(0),
            ));
        rustdar_worker::offload::offload_job("voxels", job, move |output| {
            let _guard = guard;
            // An output of another kind is `None` — "nothing to draw".
            let grid = output
                .and_then(|out| out.take::<rustdar_radar::voxel::VoxelGrid>())
                .map(Box::new);
            // The claim the whole worker move is measured by: the resample no longer
            // spends this time on the frame thread.
            log::info!(
                "3D volume view: {} for {} in {} ms off the frame thread",
                if grid.is_some() { "built" } else { "no grid" },
                target.volume.site,
                started.elapsed().as_millis(),
            );
            // Sent unconditionally: this message is what resolves the store's
            // `Building` entry.
            let _ = sender.send(crate::channels::VoxelResponse { target, grid });
            crate::app::notify_redraw(&window);
        });
        true
    }

    /// Shared dispatch for both Level II and Level III renders.
    #[allow(clippy::too_many_arguments)]
    fn spawn_render(
        &mut self,
        pane_idx: usize,
        site: &str,
        product: RadarProduct,
        elevation: f32,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
        job: rustdar_worker::offload::Job,
    ) {
        let current = self.renders_in_flight.load(Ordering::Relaxed);
        if current >= self.concurrent_renders {
            return;
        }
        self.renders_in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(Arc::clone(&self.renders_in_flight));

        let generation = self.render_generation;
        // Cleared if this pane's data changes while the render runs, which is where a
        // per-site reset stops a result.
        let wanted = self.pane_render[pane_idx].want_result();
        rustdar_worker::offload::offload_job("radar-render", job, move |output| {
            let _guard = guard;
            // An output of another kind is `None` here — "nothing to draw",
            // which every path below already handles.
            let frame = output.and_then(|out| out.take::<rustdar_radar::frame::RenderedFrame>());
            // Sent whether or not there is a frame.
            let still_wanted = wanted.load(Ordering::Relaxed);
            drop(wanted);
            if still_wanted {
                let _ = sender.send(RenderResponse {
                    rendered: frame.and_then(rendered_image_from),
                    product,
                    elevation,
                    generation,
                    pane_idx,
                    speculative_for: None,
                });
            }
            crate::app::notify_redraw(&window);
        });
        // The key this render's result will be cached under, recorded as in
        // flight so a sibling pane wanting the same picture asks for nothing.
        self.pane_render[pane_idx].render_started(Some(render_cache_key(
            site,
            product,
            RenderView::PlanView,
            elevation,
        )));
    }

    /// One adjacent-tilt pre-render into the existing [`RenderCache`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_speculative_render(
        &mut self,
        site: &str,
        product: RadarProduct,
        elevation: f32,
        volume_start: chrono::NaiveDateTime,
        lat: f64,
        lon: f64,
        data: Arc<nexrad_model::data::Scan>,
        declared: &rustdar_radar::nyquist::DeclaredNyquist,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
    ) {
        if self.speculative_in_flight {
            return;
        }
        if self.renders_in_flight.load(Ordering::Relaxed) >= self.concurrent_renders {
            return;
        }
        let (key, storm_motion, env_heights) =
            self.extract_tuple_for(site, volume_start, product, elevation);
        let cached = self
            .extract_cache_lookup(&key)
            .map(|input| (*input).clone());
        let Some(input) = cached.or_else(|| {
            rustdar_radar::render_input::RenderInput::extract(
                &data,
                elevation,
                product,
                lat,
                lon,
                storm_motion,
                env_heights,
            )
        }) else {
            // The volume does not carry this tilt — nothing taken, nothing
            // marked, and nothing to retry: the ladder said it would.
            return;
        };
        let melting_layer = self.melting_layer_product_for(product, site, volume_start);
        let rpg_storm_motion = self.rpg_storm_motion_for(product, site, volume_start);
        let job = rustdar_worker::offload::Job::Described(
            rustdar_worker::offload::JobRequest::describe(
                rustdar_radar::jobs::RadarPlanJob {
                    // The same four stamps the interactive dispatch applies,
                    // at the same moment, for the same reasons it documents.
                    input: Box::new(
                        input
                            .with_declared_nyquist(declared)
                            .with_srv_fallback(self.last_srv_fallback)
                            .with_melting_layer_product(melting_layer)
                            .with_rpg_storm_motion(rpg_storm_motion),
                    ),
                    // The body's trap, held: this raster becomes the pane's
                    // static render on tilt-step, and a hover reads the grid.
                    values_wanted: true,
                },
                rustdar_worker::offload::ceiling_only_geometry(self.static_side_ceiling_px() as u32),
            ),
        );
        self.renders_in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(Arc::clone(&self.renders_in_flight));
        self.speculative_in_flight = true;
        let generation = self.render_generation;
        let speculative_for = Some(site.to_string());
        rustdar_worker::offload::offload_job("radar-render", job, move |output| {
            let _guard = guard;
            let frame = output.and_then(|out| out.take::<rustdar_radar::frame::RenderedFrame>());
            // Sent unconditionally: the receiver is what clears the one-speculative
            // bool.
            let _ = sender.send(RenderResponse {
                rendered: frame.and_then(rendered_image_from),
                product,
                elevation,
                generation,
                // Pane-less: the marker below is what routes this result.
                pane_idx: usize::MAX,
                speculative_for,
            });
            crate::app::notify_redraw(&window);
        });
        // Deliberately NO pane bookkeeping here — that is the whole point.
    }

    /// Whether a speculative render is out right now — the one-at-a-time half
    /// of the speculative gate.
    pub(crate) fn speculative_in_flight(&self) -> bool {
        self.speculative_in_flight
    }

    /// The speculative render answered (with a raster or with nothing) —
    /// speculation may dispatch again.
    pub(crate) fn speculative_finished(&mut self) {
        self.speculative_in_flight = false;
    }

    /// Whether some pane already has **this exact plan view** in flight.
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
