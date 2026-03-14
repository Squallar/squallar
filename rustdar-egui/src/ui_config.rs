use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use rustdar_overlays::render::layers::LayerKind;
use rustdar_overlays::spc::outlook::OutlookDay;
use rustdar_radar::types::RadarProduct;

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
            time_step_secs: self.time_step_secs,
            panes: pane_configs,
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
            self.panes.push(PaneState::new());
        }
        self.pane_layout = PaneLayout::for_count(count);
        self.active_pane = if config.active_pane < count { config.active_pane } else { 0 };

        self.viewport_sync = config.viewport_sync;
        self.sync_layers = config.sync_layers;
        self.auto_poll.enabled = config.auto_poll;

        if !config.site.is_empty() {
            self.radar.config.site = config.site;
        }

        self.loop_lookback_secs = config.loop_lookback_secs;
        self.loop_speed_fps = config.loop_speed_fps;
        self.time_step_secs = config.time_step_secs;

        // Restore per-pane state.
        for (i, pane) in self.panes.iter_mut().enumerate().take(count) {
            let pc = config.panes.get(i);
            let Some(pc) = pc else { continue };
            pane.selected_product = pc.selected_product;
            pane.selected_elevation = pc.selected_elevation;
            pane.layers.spc_day = pc.spc_day;
            for (&kind, &enabled) in &pc.layers {
                pane.layers.set_enabled(kind, enabled);
            }
        }
    }

    /// Get the config directory path for persistence.
    /// Returns None on Android (uses set_config_dir externally).
    pub fn default_config_dir() -> Option<std::path::PathBuf> {
        #[cfg(target_os = "android")]
        { return None; }

        #[cfg(not(target_os = "android"))]
        {
            let base = std::env::var("XDG_CONFIG_HOME")
                .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.config", h)))
                .or_else(|_| std::env::var("LOCALAPPDATA"))
                .ok()?;
            Some(std::path::PathBuf::from(base).join("rustdar"))
        }
    }
}
