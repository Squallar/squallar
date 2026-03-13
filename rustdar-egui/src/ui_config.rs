use super::{PaneLayout, PaneState, MAX_PANES_DESKTOP, MAX_PANES_MOBILE};

impl super::Gui {
    /// Save UI layout configuration to a file.
    /// Call this when the app exits or when the user changes pane settings.
    pub fn save_ui_config(&self, config_dir: &std::path::Path) {
        let _ = std::fs::create_dir_all(config_dir);
        let path = config_dir.join("ui.conf");
        let mut lines = Vec::new();
        lines.push(format!("pane_count={}", self.pane_layout.pane_count));
        lines.push(format!("viewport_sync={}", self.viewport_sync));
        lines.push(format!("sync_layers={}", self.sync_layers));
        lines.push(format!("auto_poll={}", self.auto_poll_enabled));
        lines.push(format!("site={}", self.radar_config.site));
        let content = lines.join("\n");
        let _ = std::fs::write(path, content);
    }

    /// Load UI layout configuration from a file.
    /// Call this at app startup before the first frame.
    pub fn load_ui_config(&mut self, config_dir: &std::path::Path) {
        let path = config_dir.join("ui.conf");
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        for line in content.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "pane_count" => {
                    if let Ok(count) = value.trim().parse::<usize>() {
                        let max = if cfg!(target_os = "android") {
                            MAX_PANES_MOBILE
                        } else {
                            MAX_PANES_DESKTOP
                        };
                        let count = count.clamp(1, max);
                        while self.panes.len() < count {
                            self.panes.push(PaneState::new());
                        }
                        self.pane_layout = PaneLayout::for_count(count);
                        if self.active_pane >= count {
                            self.active_pane = 0;
                        }
                    }
                }
                "viewport_sync" => {
                    self.viewport_sync = value.trim() == "true";
                }
                "sync_layers" => {
                    self.sync_layers = value.trim() == "true";
                }
                "auto_poll" => {
                    self.auto_poll_enabled = value.trim() == "true";
                }
                "site" => {
                    let site = value.trim().to_string();
                    if !site.is_empty() {
                        self.radar_config.site = site;
                    }
                }
                _ => {}
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
