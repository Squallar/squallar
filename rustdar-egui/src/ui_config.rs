use serde::{Deserialize, Serialize};

use super::{PaneLayout, PaneState, MAX_PANES_DESKTOP, MAX_PANES_MOBILE};

#[derive(Serialize, Deserialize)]
struct UiConfig {
    pane_count: usize,
    viewport_sync: bool,
    sync_layers: bool,
    auto_poll: bool,
    site: String,
    #[serde(default = "default_loop_lookback_secs")]
    loop_lookback_secs: u64,
    #[serde(default = "default_loop_speed_fps")]
    loop_speed_fps: f32,
}

fn default_loop_lookback_secs() -> u64 { 3600 }
fn default_loop_speed_fps() -> f32 { 5.0 }

impl super::Gui {
    /// Save UI layout configuration to a JSON file.
    pub fn save_ui_config(&self, config_dir: &std::path::Path) {
        let _ = std::fs::create_dir_all(config_dir);
        let path = config_dir.join("ui.json");
        let config = UiConfig {
            pane_count: self.pane_layout.pane_count,
            viewport_sync: self.viewport_sync,
            sync_layers: self.sync_layers,
            auto_poll: self.auto_poll.enabled,
            site: self.radar.config.site.clone(),
            loop_lookback_secs: self.loop_lookback_secs,
            loop_speed_fps: self.loop_speed_fps,
        };
        if let Ok(json) = serde_json::to_string_pretty(&config) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Load UI layout configuration from a JSON file.
    pub fn load_ui_config(&mut self, config_dir: &std::path::Path) {
        let path = config_dir.join("ui.json");
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(config) = serde_json::from_str::<UiConfig>(&content) else {
            return;
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
        if self.active_pane >= count {
            self.active_pane = 0;
        }

        self.viewport_sync = config.viewport_sync;
        self.sync_layers = config.sync_layers;
        self.auto_poll.enabled = config.auto_poll;

        if !config.site.is_empty() {
            self.radar.config.site = config.site;
        }

        self.loop_lookback_secs = config.loop_lookback_secs;
        self.loop_speed_fps = config.loop_speed_fps;
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
