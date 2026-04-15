use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nexrad_level3::model::Level3Message;
use rustdar_radar::render::{render_level3_message_to_image, render_radar_to_image};
use rustdar_radar::types::RadarProduct;

use crate::WindowRef;
use crate::channels::RenderResponse;
use crate::constants::MAX_CONCURRENT_RENDERS;

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

/// Quantize an elevation angle to tenths of a degree for cache key use.
/// Matches the 0.01 tolerance used in `dispatch_pane_renders()`.
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
    pub renders_in_flight: Arc<AtomicUsize>,
    /// Cache of the latest render output per (site, product, elevation_tenths), shared
    /// across panes that display the same product at the same elevation on the same site.
    pub render_cache: HashMap<(String, RadarProduct, i32), CachedRenderOutput>,
}

impl RenderDispatcher {
    pub fn new(renders_in_flight: Arc<AtomicUsize>) -> Self {
        Self {
            pane_render: vec![PaneRenderState::new()],
            level3_data: HashMap::new(),
            render_generation: 0,
            fetch_generations: HashMap::new(),
            renders_in_flight,
            render_cache: HashMap::new(),
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
        self.render_cache.retain(|(s, _prod, _elev), _| s != site);
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
    pub fn get_cached_render(&self, site: &str, product: RadarProduct, elevation: f32) -> Option<&CachedRenderOutput> {
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
        std::thread::spawn(move || {
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
        });
        self.pane_render[pane_idx].render_in_flight = true;
    }
}
