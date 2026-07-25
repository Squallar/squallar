use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::config_store::{ConfigStore, UI_CONFIG_KEY};

use rustdar_overlays::render::layers::LayerKind;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_overlays::spc::outlook::OutlookDay;
use rustdar_radar::types::RadarProduct;
use rustdar_units::UserPreferences;

use super::PaneLayout;
use super::PaneState;
use crate::ui_layout::WidthClass;

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
    /// Per-pane overlay enabled state (master visibility per overlay kind).
    #[serde(default)]
    enabled_overlays: HashMap<OverlayKind, bool>,
    /// Per-pane overlay handler config snapshots.
    #[serde(default)]
    overlay_configs: HashMap<OverlayKind, serde_json::Value>,
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
            enabled_overlays: HashMap::new(),
            overlay_configs: HashMap::new(),
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
    /// Handler-owned config state (overlay kind name → serialized state).
    #[serde(default)]
    overlay_states: serde_json::Map<String, serde_json::Value>,
    /// GPS configuration (serial port, baud, heading source).
    #[serde(default)]
    gps_config: rustdar_gps::GpsConfig,
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
            overlay_states: serde_json::Map::new(),
            gps_config: rustdar_gps::GpsConfig::default(),
        }
    }
}

impl super::Gui {
    /// Save UI layout configuration to `store`.
    pub fn save_ui_config(&self, store: &dyn ConfigStore) {
        // Guard against NaN/Infinity in f32 fields which cause serde_json to fail.
        let fps = if self.loop_speed_fps.is_finite() { self.loop_speed_fps } else { 5.0 };
        let pane_configs: Vec<PaneConfig> = self.panes.iter().map(|pane| {
            PaneConfig {
                selected_product: pane.selected_product,
                selected_elevation: if pane.selected_elevation.is_finite() {
                    pane.selected_elevation
                } else {
                    0.0
                },
                layers: BTreeMap::new(),
                spc_day: OutlookDay::Day1,
                site: pane.site.clone(),
                time_step_secs: pane.time_step_secs,
                draw_order: pane.draw_order.clone(),
                enabled_overlays: pane.enabled_overlays.clone(),
                overlay_configs: pane.overlay_configs.clone(),
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
            overlay_states: self.overlays.serialize_handler_states(),
            gps_config: self.gps_config.clone(),
        };
        match serde_json::to_string_pretty(&config) {
            Ok(json) => {
                if let Err(e) = store.store(UI_CONFIG_KEY, &json) {
                    log::error!("Failed to write config: {}", e);
                }
            }
            Err(e) => {
                log::error!("Failed to serialize config: {}", e);
            }
        }
    }

    /// Load UI layout configuration from `store`.
    ///
    /// A missing or unparseable config leaves `self` untouched, so the caller
    /// keeps whatever defaults it was constructed with.
    pub fn load_ui_config(&mut self, store: &dyn ConfigStore) {
        let Some(content) = store.load(UI_CONFIG_KEY) else {
            return;
        };
        let config = match serde_json::from_str::<UiConfig>(&content) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Failed to parse config: {}", e);
                return;
            }
        };

        // Clamp to the *absolute* maximum, not the current screen's. Clamping
        // to what this device would offer silently destroys the user's layout:
        // a 5-pane config opened once on a phone comes back as 4 panes and is
        // written back as 4 on the next save. The config is shared state, so it
        // is clamped to what the format allows; the pane picker does the
        // per-device narrowing at the point of *editing*.
        let count = config.pane_count.clamp(1, WidthClass::max_panes_absolute());
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
        self.gps_config = config.gps_config;

        // Restore per-pane state.
        // Migrate legacy per-pane Radar toggle from old `layers` map to the
        // global RadarHandler, using the first pane's value (all panes were
        // synced anyway when there was a per-pane layer manager).
        let mut legacy_radar_enabled: Option<bool> = None;
        for (i, pane) in self.panes.iter_mut().enumerate().take(count) {
            let pc = config.panes.get(i);
            let Some(pc) = pc else {
                // Fall back to global time_step_secs for panes without PaneConfig
                pane.time_step_secs = config.time_step_secs;
                continue;
            };
            pane.selected_product = pc.selected_product;
            pane.selected_elevation = pc.selected_elevation;
            if !pc.site.is_empty() {
                pane.site = pc.site.clone();
            } else if !config.site.is_empty() {
                pane.site = config.site.clone();
            }
            pane.time_step_secs = pc.time_step_secs;
            // Capture the first pane's legacy Radar toggle for migration.
            if legacy_radar_enabled.is_none()
                && let Some(&enabled) = pc.layers.get(&LayerKind::Radar) {
                    legacy_radar_enabled = Some(enabled);
                }
            pane.draw_order = reconcile_draw_order(&pc.draw_order);
            // Restore per-pane overlay enabled state.
            if !pc.enabled_overlays.is_empty() {
                pane.enabled_overlays = pc.enabled_overlays.clone();
            }
            // Restore per-pane overlay handler configs.
            if !pc.overlay_configs.is_empty() {
                pane.overlay_configs = pc.overlay_configs.clone();
            }
        }

        // Restore handler-owned overlay states (backward-compatible: old configs have empty map)
        if !config.overlay_states.is_empty() {
            self.overlays.deserialize_handler_states(&config.overlay_states);
        } else if let Some(enabled) = legacy_radar_enabled {
            // Migrating from legacy config: no overlay_states saved yet.
            // Apply the old per-pane Radar toggle to the global handler.
            self.overlays.set_enabled(OverlayKind::Radar, enabled);
        }

        // Fill in any overlay kinds not yet in per-pane enabled maps
        // (e.g. newly added overlays or first load after migration).
        self.initialize_pane_enabled();
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

#[cfg(test)]
mod tests {
    use crate::config_store::{ConfigStore, MemoryConfigStore, UI_CONFIG_KEY};

    /// Settings the user changed must come back after a save/load cycle.
    ///
    /// Every asserted field is first checked to *differ* from what a fresh
    /// `Gui` starts with. Without that guard this test would still pass if
    /// `load_ui_config` did nothing at all, since the default would supply the
    /// expected value on its own.
    #[test]
    fn changed_settings_survive_a_save_and_load() {
        let store = MemoryConfigStore::default();

        let baseline = crate::Gui::new();
        assert_ne!(baseline.loop_lookback_secs, 7200);
        assert_ne!(baseline.loop_speed_fps, 12.5);
        assert!(baseline.viewport_sync, "default is on; test flips it off");

        let mut gui = crate::Gui::new();
        gui.loop_lookback_secs = 7200;
        gui.loop_speed_fps = 12.5;
        gui.viewport_sync = false;
        gui.save_ui_config(&store);

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);

        assert_eq!(restored.loop_lookback_secs, 7200);
        assert_eq!(restored.loop_speed_fps, 12.5);
        assert!(!restored.viewport_sync);
    }

    /// A pane layout wider than a phone offers survives the round trip.
    ///
    /// This is the data-loss bug the clamp exists to prevent, asserted at the
    /// call site rather than on the constant. `max_panes_absolute()`'s *value*
    /// was already pinned in `ui_layout`, but nothing checked that
    /// `load_ui_config` used it: reverting the clamp to
    /// `WidthClass::Compact.max_panes()` — the precise regression — passed the
    /// whole suite. A 6-pane desktop layout opened once on a phone came back as
    /// 4 and was written back as 4 on the next save.
    #[test]
    fn a_pane_layout_wider_than_a_phone_offers_survives_the_round_trip() {
        use crate::pane::{MAX_PANES_DESKTOP, MAX_PANES_MOBILE};
        use crate::ui_layout::WidthClass;

        assert!(
            MAX_PANES_DESKTOP > WidthClass::Compact.max_panes(),
            "precondition: the saved layout must be wider than a compact screen \
             would offer, or the clamp under test is never reached"
        );

        let store = MemoryConfigStore::default();
        let mut gui = crate::Gui::new();
        gui.set_pane_count_for_test(MAX_PANES_DESKTOP);
        gui.save_ui_config(&store);

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);
        assert_eq!(
            restored.pane_count(),
            MAX_PANES_DESKTOP,
            "the config was clamped to the current device's limit, so the \
             user's layout is gone and the next save writes the truncated one"
        );

        // Saving it again must not quietly narrow it either — the round trip
        // is what turns a one-off clamp into permanent data loss.
        let second = MemoryConfigStore::default();
        restored.save_ui_config(&second);
        let mut again = crate::Gui::new();
        again.load_ui_config(&second);
        assert_eq!(again.pane_count(), MAX_PANES_DESKTOP);

        assert_ne!(
            MAX_PANES_DESKTOP, MAX_PANES_MOBILE,
            "precondition: the two limits must differ, or nothing above can \
             tell a correct clamp from the broken one"
        );
    }

    /// Loading from a store with nothing in it must leave the defaults alone
    /// rather than zeroing them — this is every first run.
    #[test]
    fn an_empty_store_leaves_defaults_untouched() {
        let store = MemoryConfigStore::default();
        let mut gui = crate::Gui::new();
        let expected = gui.loop_lookback_secs;

        gui.load_ui_config(&store);

        assert_eq!(gui.loop_lookback_secs, expected);
    }

    /// A corrupt config must not wipe the user's session or panic.
    #[test]
    fn unparseable_config_is_ignored() {
        let store = MemoryConfigStore::default();
        store.store(UI_CONFIG_KEY, "{ not json").unwrap();

        let mut gui = crate::Gui::new();
        let expected = gui.loop_lookback_secs;
        gui.load_ui_config(&store);

        assert_eq!(gui.loop_lookback_secs, expected);
    }

    /// Saving writes under the shared key, which is what the filesystem backend
    /// maps onto `ui.json`.
    #[test]
    fn save_writes_under_the_ui_key() {
        let store = MemoryConfigStore::default();
        assert!(store.load(UI_CONFIG_KEY).is_none());

        crate::Gui::new().save_ui_config(&store);

        let written = store.load(UI_CONFIG_KEY).expect("config should be stored");
        assert!(
            serde_json::from_str::<super::UiConfig>(&written).is_ok(),
            "stored blob should parse back as a UiConfig"
        );
    }
}
