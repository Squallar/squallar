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
    /// Map zoom level, as `walkers::MapMemory` reports it.
    ///
    /// `Option` rather than a defaulted `f64` so a config written before the
    /// viewport was persisted is distinguishable from one that genuinely saved
    /// the default zoom. The former must leave `PaneState::with_site`'s choice
    /// alone; the latter must override it.
    #[serde(default)]
    zoom: Option<f64>,
    /// Where the map is centred, as `(lat, lon)`, when the user has panned away
    /// from the site.
    ///
    /// `None` means the map is following the radar site rather than sitting at a
    /// detached centre — the state `MapMemory::detached` reports as `None` — and
    /// restoring it has to re-establish *following*, not centre on the site's
    /// coordinates and call it the same thing. The two look identical until the
    /// pane changes site.
    #[serde(default)]
    center: Option<(f64, f64)>,
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
            zoom: None,
            center: None,
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
    /// Feed live panes from the real-time chunk bucket rather than polling the
    /// archive for completed volumes.
    ///
    /// The container carries `#[serde(default)]`, so a config written before
    /// this field existed takes `UiConfig::default()`'s value — the same
    /// mechanism `auto_poll` relies on.
    live_chunks: bool,
    /// Subscribe to the push-notification service for new chunks.
    chunk_notifications: bool,
    /// Where that service lives. Empty means the built-in default.
    #[serde(default)]
    notifier_endpoint: String,
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
            live_chunks: true,
            chunk_notifications: true,
            notifier_endpoint: String::new(),
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
        let Some(json) = self.ui_config_json() else {
            return;
        };
        if let Err(e) = store.store(UI_CONFIG_KEY, &json) {
            log::error!("Failed to write config: {}", e);
        }
    }

    /// The configuration this `Gui` would persist, as JSON.
    ///
    /// Exposed separately from [`save_ui_config`](Self::save_ui_config) so the
    /// periodic autosave can ask "has anything changed?" without a storage
    /// write. Comparing this against the last written string is what keeps a
    /// three-second timer from becoming a three-second write loop.
    ///
    /// `None` only if serialization fails, which is already logged.
    pub fn ui_config_json(&self) -> Option<String> {
        // Guard against NaN/Infinity in f32 fields which cause serde_json to fail.
        let fps = if self.loop_speed_fps.is_finite() {
            self.loop_speed_fps
        } else {
            5.0
        };
        let pane_configs: Vec<PaneConfig> = self
            .panes
            .iter()
            .map(|pane| PaneConfig {
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
                // Same NaN guard as `loop_speed_fps` above, and for the same
                // reason: `serde_json` refuses to serialize a non-finite float,
                // and it fails the *whole* config, so one bad zoom would silently
                // stop persisting everything else too.
                zoom: pane
                    .map_memory
                    .zoom()
                    .is_finite()
                    .then(|| pane.map_memory.zoom()),
                center: pane
                    .map_memory
                    .detached()
                    .map(|p| (p.y(), p.x()))
                    .filter(|(lat, lon)| lat.is_finite() && lon.is_finite()),
            })
            .collect();
        let config = UiConfig {
            pane_count: self.pane_layout.pane_count,
            active_pane: self.active_pane,
            viewport_sync: self.viewport_sync,
            sync_layers: self.sync_layers,
            auto_poll: self.auto_poll.enabled,
            live_chunks: self.live_chunks,
            chunk_notifications: self.chunk_notifications,
            notifier_endpoint: self.notifier_endpoint.clone(),
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
            Ok(json) => Some(json),
            Err(e) => {
                log::error!("Failed to serialize config: {}", e);
                None
            }
        }
    }

    /// Load UI layout configuration from `store`.
    ///
    /// A missing or unparseable config leaves `self` untouched, so the caller
    /// keeps whatever defaults it was constructed with.
    ///
    /// Returns whether a config was actually applied. The caller uses that to
    /// tell a returning user from a first run: only a first run may have its
    /// radar site chosen for it, because on any later run the stored site is the
    /// user's own choice and overriding it would be the bug, not the feature.
    ///
    /// An unparseable config counts as *not* loaded. That is the honest answer —
    /// nothing was applied — and it means a corrupted store still gets a sensibly
    /// located default rather than the compiled-in one.
    pub fn load_ui_config(&mut self, store: &dyn ConfigStore) -> bool {
        let Some(content) = store.load(UI_CONFIG_KEY) else {
            return false;
        };
        let config = match serde_json::from_str::<UiConfig>(&content) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Failed to parse config: {}", e);
                return false;
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
            let site = config
                .panes
                .get(self.panes.len())
                .map(|pc| pc.site.clone())
                .unwrap_or_else(|| config.site.clone());
            self.panes.push(PaneState::with_site(site));
        }
        self.pane_layout = PaneLayout::for_count(count);
        self.active_pane = if config.active_pane < count {
            config.active_pane
        } else {
            0
        };

        self.viewport_sync = config.viewport_sync;
        self.sync_layers = config.sync_layers;
        self.auto_poll.enabled = config.auto_poll;
        self.live_chunks = config.live_chunks;
        self.chunk_notifications = config.chunk_notifications;
        self.notifier_endpoint = config.notifier_endpoint;

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
                && let Some(&enabled) = pc.layers.get(&LayerKind::Radar)
            {
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
            restore_viewport(pane, pc);
        }

        // Restore handler-owned overlay states (backward-compatible: old configs have empty map)
        if !config.overlay_states.is_empty() {
            self.overlays
                .deserialize_handler_states(&config.overlay_states);
        } else if let Some(enabled) = legacy_radar_enabled {
            // Migrating from legacy config: no overlay_states saved yet.
            // Apply the old per-pane Radar toggle to the global handler.
            self.overlays.set_enabled(OverlayKind::Radar, enabled);
        }

        // Fill in any overlay kinds not yet in per-pane enabled maps
        // (e.g. newly added overlays or first load after migration).
        self.initialize_pane_enabled();
        true
    }

    /// Point every pane at `site`, for a first run with no stored config.
    ///
    /// Only legitimate before the user has seen anything: it overwrites the site
    /// on each pane and on the fetch config unconditionally. Guarding that is
    /// the caller's job — see [`load_ui_config`](Self::load_ui_config).
    pub fn set_initial_site(&mut self, site: &str) {
        self.radar.config.site = site.to_string();
        for pane in &mut self.panes {
            pane.site = site.to_string();
        }
    }
}

/// Put a pane's map back where it was left: same zoom, same centre.
///
/// Both fields are restored only when present, so a config written before the
/// viewport was persisted leaves `PaneState::with_site`'s defaults intact rather
/// than snapping every pane to zoom 0 over the Atlantic.
///
/// A rejected zoom is not an error worth propagating. `walkers` clamps to a
/// valid range and refuses anything outside it; the saved value came from
/// `walkers` in the first place, so the only way to land here is a hand-edited
/// or version-skewed config, where keeping the default is the right answer.
fn restore_viewport(pane: &mut PaneState, pc: &PaneConfig) {
    if let Some(zoom) = pc.zoom
        && pane.map_memory.set_zoom(zoom).is_err()
    {
        log::warn!("saved zoom {zoom} is out of range; keeping the default");
    }
    // No `else`: a saved `None` means the map was following its site, which is
    // already the state a fresh `MapMemory` is in. Calling `follow_my_position`
    // here would be a no-op on a fresh pane and would fight the pane-reuse path
    // on a reload, so leaving it alone is both simpler and more correct.
    if let Some((lat, lon)) = pc.center {
        pane.map_memory.center_at(walkers::lat_lon(lat, lon));
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
    let mut result: Vec<OverlayKind> = saved
        .iter()
        .copied()
        .filter(|k| all_set.contains(k))
        .collect();

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

    /// Zoom and pan are what "come back to where I left off" actually means, and
    /// neither was persisted before.
    #[test]
    fn a_panned_and_zoomed_map_comes_back_where_it_was_left() {
        let store = MemoryConfigStore::default();

        let baseline = crate::Gui::new();
        let default_zoom = baseline.pane(0).unwrap().map_memory.zoom();
        assert_ne!(
            default_zoom, 9.0,
            "the test zoom must differ from the default"
        );
        assert!(
            baseline.pane(0).unwrap().map_memory.detached().is_none(),
            "a fresh pane follows its site; the test then pans it away"
        );

        let mut gui = crate::Gui::new();
        {
            let pane = gui.pane_mut(0).unwrap();
            pane.map_memory.set_zoom(9.0).unwrap();
            pane.map_memory
                .center_at(walkers::lat_lon(44.9778, -93.2650));
        }
        gui.save_ui_config(&store);

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);

        let pane = restored.pane(0).unwrap();
        assert_eq!(pane.map_memory.zoom(), 9.0);
        let center = pane.map_memory.detached().expect("the pan was persisted");
        // `Position` is (x, y) = (lon, lat). A transposition here is silently a
        // valid coordinate, just the wrong hemisphere.
        assert!((center.y() - 44.9778).abs() < 1e-9, "lat {}", center.y());
        assert!((center.x() + 93.2650).abs() < 1e-9, "lon {}", center.x());
    }

    /// Following the site and being centred on the site's coordinates look the
    /// same until the pane changes site, at which point one moves and the other
    /// does not. A round trip must not silently convert the first into the second.
    #[test]
    fn a_map_following_its_site_does_not_come_back_pinned() {
        let store = MemoryConfigStore::default();

        let mut gui = crate::Gui::new();
        gui.pane_mut(0).unwrap().map_memory.set_zoom(7.0).unwrap();
        assert!(gui.pane(0).unwrap().map_memory.detached().is_none());
        gui.save_ui_config(&store);

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);

        assert_eq!(restored.pane(0).unwrap().map_memory.zoom(), 7.0);
        assert!(
            restored.pane(0).unwrap().map_memory.detached().is_none(),
            "an un-panned map was restored as pinned to a fixed centre"
        );
    }

    /// Configs written before the viewport was persisted must keep the built-in
    /// default zoom rather than being read as "saved zoom 0".
    #[test]
    fn a_config_predating_viewport_persistence_keeps_the_default_zoom() {
        let store = MemoryConfigStore::default();
        let default_zoom = crate::Gui::new().pane(0).unwrap().map_memory.zoom();

        // A config with panes but no `zoom`/`center` keys at all — exactly the
        // shape every already-installed copy of the app has on disk right now.
        store
            .store(
                UI_CONFIG_KEY,
                r#"{"pane_count":1,"site":"KMPX","panes":[{"site":"KMPX"}]}"#,
            )
            .unwrap();

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);

        assert_eq!(restored.pane(0).unwrap().site, "KMPX");
        assert_eq!(
            restored.pane(0).unwrap().map_memory.zoom(),
            default_zoom,
            "an absent zoom was treated as a saved value"
        );
        assert!(restored.pane(0).unwrap().map_memory.detached().is_none());
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

#[cfg(test)]
mod live_chunks_config_tests {
    use super::*;
    use crate::Gui;

    /// The setting survives a save/load cycle in both positions.
    #[test]
    fn the_live_chunks_setting_round_trips() {
        for enabled in [true, false] {
            let mut gui = Gui::new();
            gui.set_live_chunks(enabled);
            let json = gui.ui_config_json().expect("serialises");
            let parsed: UiConfig = serde_json::from_str(&json).expect("parses");
            assert_eq!(parsed.live_chunks, enabled);
        }
    }

    /// A config written before the field existed takes the default rather than
    /// failing to parse — the mechanism `#[serde(default)]` on the container
    /// provides, and the one `auto_poll` already relies on.
    #[test]
    fn a_config_written_before_this_field_defaults_to_chunks() {
        let old = r#"{"pane_count":1,"active_pane":0,"auto_poll":true,"site":"KTLX"}"#;
        let parsed: UiConfig = serde_json::from_str(old).expect("an older config still parses");
        assert!(
            parsed.live_chunks,
            "an existing install would silently lose the low-latency feed"
        );
    }
}

#[cfg(test)]
mod notifier_config_tests {
    use super::*;
    use crate::Gui;

    /// Both notification settings survive a save/load cycle.
    #[test]
    fn the_notifier_settings_round_trip() {
        let mut gui = Gui::new();
        gui.set_chunk_notifications(false);
        gui.set_notifier_endpoint("wss://example.test");
        let json = gui.ui_config_json().expect("serialises");
        let parsed: UiConfig = serde_json::from_str(&json).expect("parses");
        assert!(!parsed.chunk_notifications);
        assert_eq!(parsed.notifier_endpoint, "wss://example.test");
    }

    /// A config written before these fields existed keeps the low-latency
    /// defaults rather than failing to parse or silently opting out.
    #[test]
    fn an_older_config_defaults_to_notifications_on() {
        let old = r#"{"pane_count":1,"auto_poll":true,"site":"KTLX"}"#;
        let parsed: UiConfig = serde_json::from_str(old).expect("an older config still parses");
        assert!(parsed.chunk_notifications);
        assert!(parsed.live_chunks);
    }

    /// A cleared endpoint box falls back to the built-in default rather than
    /// acting as a silent off switch — turning the feature off is what the
    /// toggle is for.
    #[test]
    fn an_empty_endpoint_falls_back_to_the_default() {
        let mut gui = Gui::new();
        gui.set_notifier_endpoint("   ");
        assert_eq!(gui.notifier_endpoint(), crate::DEFAULT_NOTIFIER_ENDPOINT);
        gui.set_notifier_endpoint("wss://example.test/");
        assert_eq!(gui.notifier_endpoint(), "wss://example.test/");
    }
}
