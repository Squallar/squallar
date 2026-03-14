use std::collections::HashMap;
use std::sync::Arc;

use nexrad_level3::model::{DataPacket, Level3Message};
use rustdar_radar::render::{render_level3_radial_to_image, render_radar_to_image};
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

    /// Clear render state for suspend/resume (keeps cached_render intact).
    pub fn clear_for_suspend(&mut self) {
        for prs in &mut self.pane_render {
            prs.last_rendered = None;
        }
    }

    /// Clear render state on surface loss (graphics state destroyed).
    pub fn clear_for_surface_loss(&mut self) {
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
            log::debug!(
                "L3 {:?}: pdb product_code={}, thresholds={:?}, ps47_53={:?}",
                product,
                l3_msg.pdb.product_code,
                l3_msg.pdb.thresholds,
                l3_msg.pdb.product_specific_47_53
            );
            // Extract radial packet from symbology
            let radial_packet = l3_msg.symbology.as_ref().and_then(|sym| {
                log::debug!(
                    "L3 {:?}: symbology has {} layers",
                    product,
                    sym.layers.len()
                );
                for (li, layer) in sym.layers.iter().enumerate() {
                    log::debug!(
                        "L3 {:?}: layer {} has {} packets",
                        product,
                        li,
                        layer.packets.len()
                    );
                    for (pi, pkt) in layer.packets.iter().enumerate() {
                        match pkt {
                            DataPacket::DigitalRadial(rp) => {
                                log::debug!(
                                    "L3 {:?}: layer[{}].packet[{}] = DigitalRadial: radials={}, bins={}, scale_factor={}, is_legacy={}, first_range_bin={}",
                                    product, li, pi, rp.radials.len(), rp.num_range_bins, rp.scale_factor, rp.is_legacy, rp.first_range_bin
                                );
                                if let Some(r0) = rp.radials.first() {
                                    let non_zero: usize =
                                        r0.gate_values.iter().filter(|&&v| v > 1).count();
                                    let max_val =
                                        r0.gate_values.iter().copied().max().unwrap_or(0);
                                    log::debug!(
                                        "L3 {:?}: first radial: start_angle={}, delta={}, gates={}, non_zero(>1)={}, max_gate_val={}, first_10={:?}",
                                        product, r0.start_angle, r0.angle_delta, r0.gate_values.len(), non_zero, max_val,
                                        &r0.gate_values[..r0.gate_values.len().min(10)]
                                    );
                                }
                            }
                            DataPacket::Raster(_) => {
                                log::debug!(
                                    "L3 {:?}: layer[{}].packet[{}] = Raster",
                                    product,
                                    li,
                                    pi
                                );
                            }
                        }
                    }
                }
                sym.layers.iter().find_map(|layer| {
                    layer.packets.iter().find_map(|pkt| {
                        if let DataPacket::DigitalRadial(rp) = pkt {
                            Some(rp)
                        } else {
                            None
                        }
                    })
                })
            });
            if radial_packet.is_none() {
                log::warn!(
                    "L3 {:?}: no radial packet found in symbology!",
                    product
                );
            }
            if let Some(rp) = radial_packet {
                let scale = l3_msg.pdb.data_scale();
                let offset = l3_msg.pdb.data_offset();
                let vil_lut = l3_msg.pdb.build_vil_lut();
                let legacy_lut;
                let lut: Option<&[f32]> = if vil_lut.is_some() {
                    vil_lut.as_deref()
                } else if rp.is_legacy {
                    legacy_lut = l3_msg.pdb.decode_legacy_thresholds();
                    Some(legacy_lut.as_slice())
                } else {
                    None
                };
                log::debug!(
                    "L3 {:?}: rendering with scale={}, offset={}, legacy={}, lut_len={:?}, gate_interval_km={}, first_gate_range_km={}",
                    product, scale, offset, rp.is_legacy, lut.map(|l| l.len()), rp.gate_interval_km(), rp.first_gate_range_km()
                );
                if let Some((image, range, values)) =
                    render_level3_radial_to_image(rp, product, lat, lon, scale, offset, lut)
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
                } else {
                    log::warn!(
                        "L3 {:?}: render_level3_radial_to_image returned None",
                        product
                    );
                }
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
