use std::collections::HashMap;
use std::sync::Arc;

use nexrad_level3::model::Level3Message;
use rustdar_radar::render::{render_level3_message_to_image, render_radar_to_image};
use rustdar_radar::types::RadarProduct;

use crate::WindowRef;
use crate::channels::RenderResponse;

/// Per-pane render tracking state.
pub struct PaneRenderState {
    /// True while a background render is in progress for this pane.
    pub render_in_flight: bool,
    /// Last rendered radar parameters to detect changes.
    pub last_rendered: Option<(RadarProduct, f32)>,
    /// Cached raw RGBA + metadata from the last successful render so we can
    /// re-upload the texture instantly after suspend/resume without re-rendering.
    pub cached_render: Option<(Vec<u8>, f64, Vec<f32>, RadarProduct, f32)>,
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

/// Manages radar rendering dispatch and Level III data caching.
///
/// Tracks per-pane render state, owns the Level III data cache, and
/// provides generation-based staleness checks for both fetches and renders.
pub struct RenderDispatcher {
    /// Per-pane render tracking (indexed by pane index).
    pub pane_render: Vec<PaneRenderState>,
    /// Decoded Level III product data, keyed by (RadarProduct, tilt_code).
    pub level3_data: HashMap<(RadarProduct, String), Arc<Level3Message>>,
    /// Generation counter to discard stale render results after site/scan changes.
    pub render_generation: u64,
    /// Generation counter to discard stale fetch results from older requests.
    pub fetch_generation: u64,
}

impl RenderDispatcher {
    pub fn new() -> Self {
        Self {
            pane_render: vec![PaneRenderState::new()],
            level3_data: HashMap::new(),
            render_generation: 0,
            fetch_generation: 0,
        }
    }

    /// Ensure the pane_render vec has at least `count` entries.
    pub fn ensure_pane_count(&mut self, count: usize) {
        while self.pane_render.len() < count {
            self.pane_render.push(PaneRenderState::new());
        }
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

    /// Increment the fetch generation and return the new value.
    pub fn next_fetch_generation(&mut self) -> u64 {
        self.fetch_generation += 1;
        self.fetch_generation
    }

    /// Check if a fetch generation is stale.
    pub fn is_fetch_stale(&self, generation: u64) -> bool {
        generation < self.fetch_generation
    }

    /// Check if a render generation is stale.
    pub fn is_render_stale(&self, generation: u64) -> bool {
        generation < self.render_generation
    }

    /// Spawn a Level III render for a pane if applicable.
    /// Returns `true` if a render was spawned.
    pub fn try_spawn_level3_render(
        &mut self,
        pane_idx: usize,
        product: RadarProduct,
        elevation: f32,
        lat: f64,
        lon: f64,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
    ) -> bool {
        let best_l3 = self
            .level3_data
            .iter()
            .filter(|((p, _), _)| *p == product)
            .min_by(|(_, a), (_, b)| {
                let da = (a.pdb.elevation_angle() - elevation).abs();
                let db = (b.pdb.elevation_angle() - elevation).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(_, msg)| Arc::clone(msg));

        let Some(l3_msg) = best_l3 else {
            return false;
        };

        log::info!(
            "Spawning Level III render for pane {}: {:?}",
            pane_idx,
            product
        );
        let generation = self.render_generation;

        std::thread::spawn(move || {
            if let Some((image, range, values)) =
                render_level3_message_to_image(&l3_msg, product, lat, lon)
            {
                let _ = sender.send(RenderResponse {
                    image_data: image,
                    max_range_km: range,
                    value_data: values,
                    product,
                    elevation,
                    generation,
                    pane_idx,
                });
            }
            if let Some(window) = window {
                window.request_redraw();
            }
        });
        self.pane_render[pane_idx].render_in_flight = true;
        true
    }

    /// Spawn a Level II render for a pane.
    pub fn spawn_level2_render(
        &mut self,
        pane_idx: usize,
        product: RadarProduct,
        elevation: f32,
        lat: f64,
        lon: f64,
        data: Arc<nexrad_model::data::Scan>,
        sender: std::sync::mpsc::Sender<RenderResponse>,
        window: Option<WindowRef>,
    ) {
        log::info!(
            "Spawning background render for pane {}: {:?} at {:.1}°",
            pane_idx,
            product,
            elevation
        );
        let generation = self.render_generation;

        std::thread::spawn(move || {
            if let Some((image, range, values)) =
                render_radar_to_image(&data, elevation, product, lat, lon)
            {
                let _ = sender.send(RenderResponse {
                    image_data: image,
                    max_range_km: range,
                    value_data: values,
                    product,
                    elevation,
                    generation,
                    pane_idx,
                });
            }
            if let Some(window) = window {
                window.request_redraw();
            }
        });
        self.pane_render[pane_idx].render_in_flight = true;
    }
}
