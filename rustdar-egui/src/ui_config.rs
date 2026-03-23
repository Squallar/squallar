use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use rustdar_overlays::render::layers::LayerKind;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_overlays::spc::outlook::OutlookDay;
use rustdar_radar::types::RadarProduct;
use rustdar_units::UserPreferences;

use super::{PaneLayout, PaneState, MAX_PANES_DESKTOP, MAX_PANES_MOBILE};

/// Serializable per-pane state persisted across sessions.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct PaneConfig {
    selected_product: RadarProduct,
    selected_elevation: f32,
    /// Layer kind → enabled flag.
    layers: BTreeMap<LayerKind, bool>,
    spc_day: OutlookDay,
    /// Radar site code for this pane (e.g. "KTLX").
    #[serde(default = "default_site")]
    site: String,
    /// Time step size in seconds (0 = single scan mode).
    #[serde(default = "default_time_step")]
    time_step_secs: i64,
    /// Visual stacking order for all map layers (bottom to top).
    #[serde(default = "OverlayKind::default_draw_order")]
    draw_order: Vec<OverlayKind>,
}

fn default_site() -> String {
    String::new()
}

fn default_time_step() -> i64 {
    600
}

impl Default for PaneConfig {
    fn default() -> Self {
        let layers = LayerKind::all()
            .iter()
            .map(|&k| {
                let enabled = matches!(
                    k,
                    LayerKind::Radar
                        | LayerKind::SpcMesoscaleDiscussions
                        | LayerKind::NwsWarnings
                        | LayerKind::NwsWatches
                        | LayerKind::NwsAdvisories
                        | LayerKind::CityLabels
                );
                (k, enabled)
            })
            .collect();
        Self {
            selected_product: RadarProduct::Reflectivity,
            selected_elevation: 0.0,
            layers,
            spc_day: OutlookDay::Day1,
            site: String::new(),
            time_step_secs: 600,
            draw_order: OverlayKind::default_draw_order(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct UiConfig {
    pane_count: usize,
    active_pane: usize,
    viewport_sync: bool,
    sync_layers: bool,
    auto_poll: bool,
    site: String,
    loop_lookback_secs: u64,
    loop_speed_fps: f32,
    time_step_secs: i64,
    /// Per-pane persistent state (product, elevation, layers).
    panes: Vec<PaneConfig>,
    /// User unit/timezone preferences.
    preferences: UserPreferences,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            pane_count: 1,
            active_pane: 0,
            viewport_sync: true,
            sync_layers: true,
            auto_poll: true,
            site: "KTLX".to_string(),
            loop_lookback_secs: 3600,
            loop_speed_fps: 5.0,
            time_step_secs: 600,
            panes: vec![PaneConfig::default()],
            preferences: UserPreferences::default(),
        }
    }
}

impl super::Gui {
    /// Save UI layout configuration to a JSON file.
    pub fn save_ui_config(&self, config_dir: &std::path::Path) {
        if let Err(e) = std::fs::create_dir_all(config_dir) {
            log::error!("Failed to create config dir {:?}: {}", config_dir, e);
            return;
        }
        let path = config_dir.join("ui.json");
        // Guard against NaN/Infinity in f32 fields which cause serde_json to fail.
        let fps = if self.loop_speed_fps.is_finite() { self.loop_speed_fps } else { 5.0 };
        let pane_configs: Vec<PaneConfig> = self.panes.iter().map(|pane| {
            let layers = LayerKind::all()
                .iter()
                .map(|&k| (k, pane.layers.is_enabled(k)))
                .collect();
            PaneConfig {
                selected_product: pane.selected_product,
                selected_elevation: if pane.selected_elevation.is_finite() {
                    pane.selected_elevation
                } else {
                    0.0
                },
                layers,
                spc_day: pane.layers.spc_day,
                site: pane.site.clone(),
                time_step_secs: pane.time_step_secs,
                draw_order: pane.draw_order.clone(),
            }
        }).collect();
        let config = UiConfig {
            pane_count: self.pane_layout.pane_count,
            active_pane: self.active_pane,
            viewport_sync: self.viewport_sync,
            sync_layers: self.sync_layers,
            auto_poll: self.auto_poll.enabled,
            site: self.radar.config.site.clone(),
            loop_lookback_secs: self.loop_lookback_secs,
            loop_speed_fps: fps,
            time_step_secs: self.panes.first().map(|p| p.time_step_secs).unwrap_or(600),
            panes: pane_configs,
            preferences: self.preferences.clone(),
        };
        match serde_json::to_string_pretty(&config) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    log::error!("Failed to write config to {:?}: {}", path, e);
                }
            }
            Err(e) => {
                log::error!("Failed to serialize config: {}", e);
            }
        }
    }

    /// Load UI layout configuration from a JSON file.
    pub fn load_ui_config(&mut self, config_dir: &std::path::Path) {
        let path = config_dir.join("ui.json");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("Failed to read config {:?}: {}", path, e);
                }
                return;
            }
        };
        let config = match serde_json::from_str::<UiConfig>(&content) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Failed to parse config {:?}: {}", path, e);
                return;
            }
        };

        let max = if cfg!(target_os = "android") {
            MAX_PANES_MOBILE
        } else {
            MAX_PANES_DESKTOP
        };
        let count = config.pane_count.clamp(1, max);
        while self.panes.len() < count {
            let site = config.panes.get(self.panes.len()).map(|pc| pc.site.clone()).unwrap_or_else(|| config.site.clone());
            self.panes.push(PaneState::with_site(site));
        }
        self.pane_layout = PaneLayout::for_count(count);
        self.active_pane = if config.active_pane < count { config.active_pane } else { 0 };

        self.viewport_sync = config.viewport_sync;
        self.sync_layers = config.sync_layers;
        self.auto_poll.enabled = config.auto_poll;

        if !config.site.is_empty() {
            self.radar.config.site = config.site.clone();
        }

        self.loop_lookback_secs = config.loop_lookback_secs;
        self.loop_speed_fps = config.loop_speed_fps;
        self.preferences = config.preferences;

        // Restore per-pane state.
        for (i, pane) in self.panes.iter_mut().enumerate().take(count) {
            let pc = config.panes.get(i);
            let Some(pc) = pc else {
                // Fall back to global time_step_secs for panes without PaneConfig
                pane.time_step_secs = config.time_step_secs;
                continue;
            };
            pane.selected_product = pc.selected_product;
            pane.selected_elevation = pc.selected_elevation;
            pane.layers.spc_day = pc.spc_day;
            if !pc.site.is_empty() {
                pane.site = pc.site.clone();
            } else if !config.site.is_empty() {
                pane.site = config.site.clone();
            }
            pane.time_step_secs = pc.time_step_secs;
            for (&kind, &enabled) in &pc.layers {
                pane.layers.set_enabled(kind, enabled);
            }
            pane.draw_order = reconcile_draw_order(&pc.draw_order);
        }
    }
}

/// Reconcile a saved draw order with the current set of known `OverlayKind` variants.
///
/// - Preserves the saved ordering for recognized variants.
/// - Filters out any unknown/stale variants that no longer exist.
/// - Appends any new variants (present in `default_draw_order` but missing from save)
///   in their default relative order.
fn reconcile_draw_order(saved: &[OverlayKind]) -> Vec<OverlayKind> {
    let all_set: std::collections::HashSet<OverlayKind> =
        OverlayKind::all().iter().copied().collect();

    // Keep only recognized kinds, in saved order.
    let mut result: Vec<OverlayKind> = saved.iter().copied().filter(|k| all_set.contains(k)).collect();

    // Append any missing kinds (new variants added since save).
    for &kind in OverlayKind::all() {
        if !result.contains(&kind) {
            result.push(kind);
        }
    }
    result
}
