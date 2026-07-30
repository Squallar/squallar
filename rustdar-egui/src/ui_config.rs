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
use crate::pane::{
    CrossSectionPane, GeoPoint, OrbitCamera, PaneContent, PaneKind, SectionLine, VolumePane,
};
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
    /// What kind of pane this is: a plan-view map, a vertical cross-section or a
    /// 3D volume view.
    ///
    /// `PaneKind::default()` is `Map`, so a config written before pane kinds
    /// existed loads as a screen full of maps — which is what it was.
    #[serde(default)]
    kind: PaneKind,
    /// A cross-section pane's own state, present only when [`Self::kind`] is
    /// `CrossSection`.
    ///
    /// Two fields that must agree, which the in-memory representation
    /// deliberately does not allow — `PaneContent` derives the kind from the
    /// content precisely so they cannot disagree. On the wire they can, because a
    /// file can say anything, so `restore_content` treats a mismatch as a corrupt
    /// pane and falls back to `Map`.
    #[serde(default)]
    cross_section: Option<CrossSectionConfig>,
    /// A 3D pane's own state, present only when [`Self::kind`] is `Volume`. Same
    /// arrangement as [`Self::cross_section`].
    #[serde(default)]
    volume: Option<VolumeConfig>,
}

/// A cross-section pane, as persisted.
///
/// The rendered raster is deliberately not here and never will be: it is derived
/// from the volume and the line, and a volume is not persisted either. What is
/// worth keeping is the *question* the pane is asking.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct CrossSectionConfig {
    /// The drawn line, or `None` for a pane converted but not yet aimed — an
    /// ordinary state, and the one a freshly converted pane is in.
    line: Option<SectionLineConfig>,
    /// Which map pane the line was drawn on. Validated against the restored pane
    /// count: a config saved from a six-pane layout and opened on a phone can name
    /// a pane that is no longer there.
    source_pane: Option<usize>,
}

/// A section line's endpoints, in degrees.
///
/// Four flat `f64`s rather than a `SectionLine`, because `SectionLine`'s fields
/// are private and its only constructor *validates* — which is exactly what
/// wants to happen on the way back in, and must not be bypassed by a
/// `Deserialize` impl. So the wire form is dumb and
/// [`SectionLine::new`](crate::pane::SectionLine::new) is the gate.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct SectionLineConfig {
    a_lat: f64,
    a_lon: f64,
    b_lat: f64,
    b_lon: f64,
}

/// A 3D pane, as persisted: where the eye is, and nothing else.
///
/// The voxel grid is not here for the same reason the section raster is not:
/// it is derived from a volume, and rebuilding it is what opening the pane does.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct VolumeConfig {
    yaw_deg: f32,
    pitch_deg: f32,
    eye_distance: f32,
}

impl Default for VolumeConfig {
    /// `OrbitCamera`'s own default, read out of it rather than restated — a
    /// second copy of three angles would drift, and the drift would show up as a
    /// 3D pane that opened at a different angle depending on whether its config
    /// predated the field.
    fn default() -> Self {
        let camera = OrbitCamera::default();
        Self {
            yaw_deg: camera.yaw_deg(),
            pitch_deg: camera.pitch_deg(),
            eye_distance: camera.eye_distance(),
        }
    }
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
            kind: PaneKind::Map,
            cross_section: None,
            volume: None,
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
        // Guard every float against NaN and infinity on the way out.
        //
        // Not because `serde_json` fails on them — it does not, which is the
        // correction to what this comment used to say. It writes `null`, the save
        // succeeds, and it is the *next load* that fails, because `null` will not
        // deserialize back into a number. So one bad float takes the whole file
        // with it, one run later, and permanently: the next autosave rewrites it
        // from defaults. Pinned by
        // `a_non_finite_float_would_poison_the_config_file_permanently`.
        let fps = if self.loop_speed_fps.is_finite() {
            self.loop_speed_fps
        } else {
            5.0
        };
        let pane_configs: Vec<PaneConfig> = self
            .panes
            .iter()
            .map(|pane| {
                // Filtered, not written out and hoped for: see `content_config`.
                let (kind, cross_section, volume) = content_config(pane);
                PaneConfig {
                    kind,
                    cross_section,
                    volume,
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
                    // reason, stated there.
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
                }
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
            // Assigned as a whole rather than through `PaneState::set_kind`,
            // because the kind and the per-kind state arrive together and
            // `restore_content` has already decided both. This is also the one
            // legitimate writer of `content` outside the UI pass — the deferred
            // `Gui::request_pane_kind` exists for the writers *inside* it, where
            // the pane may be `mem::take`n.
            pane.content = restore_content(i, pc, count);
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

/// What a pane's kind and per-kind state should be persisted as.
///
/// # Every float goes through the finiteness filter
///
/// `serde_json` does not refuse a non-finite float — it writes `null` — so the
/// save succeeds and the **next load** is what fails, because `null` will not
/// deserialize back into a number. A single NaN in a camera angle therefore costs
/// the user the site, the layout, the layers and everything else, one run later,
/// permanently, with nothing at the time to connect the two. `loop_speed_fps` and
/// the map zoom already carry the same guard for the same reason.
///
/// Belt and braces, deliberately, and **not covered by any test** — which is
/// worth stating rather than leaving to be discovered. `SectionLine` and
/// `OrbitCamera` both have private fields and exactly one validating writer
/// apiece (`SectionLine::new`, `OrbitCamera::{restore, nudge}`), so a non-finite
/// value in either is *unconstructible*: no test can build one to feed these two
/// branches, and mutating them away therefore fails nothing. The only way to pin
/// them would be a `#[cfg(test)]` constructor that skips validation — a backdoor
/// into the very invariant they exist to back up, which is a worse trade than an
/// unpinned branch.
///
/// They stay because the cost of being wrong is asymmetric and the guarantees
/// they lean on live in another module: a filter drops one pane's kind, a missing
/// filter drops the user's entire configuration. What *is* pinned is the
/// mechanism and the outcome —
/// `a_non_finite_float_would_poison_the_config_file_permanently`.
///
/// A pane whose floats do not pass is written as a plain `Map` with no sub-config
/// rather than as its own kind with the sub-config omitted. The latter is the
/// shape `restore_content` treats as corrupt, so it would be a file that reads as
/// broken rather than as simple.
fn content_config(
    pane: &PaneState,
) -> (PaneKind, Option<CrossSectionConfig>, Option<VolumeConfig>) {
    match &pane.content {
        PaneContent::Map => (PaneKind::Map, None, None),
        PaneContent::CrossSection(section) => {
            let line = section.line.map(|line| SectionLineConfig {
                a_lat: line.a().lat,
                a_lon: line.a().lon,
                b_lat: line.b().lat,
                b_lon: line.b().lon,
            });
            let finite = line.as_ref().is_none_or(|l| {
                l.a_lat.is_finite()
                    && l.a_lon.is_finite()
                    && l.b_lat.is_finite()
                    && l.b_lon.is_finite()
            });
            if !finite {
                log::warn!("a section pane's endpoints are not finite; saving it as a map");
                return (PaneKind::Map, None, None);
            }
            (
                PaneKind::CrossSection,
                Some(CrossSectionConfig {
                    line,
                    source_pane: section.source_pane,
                }),
                None,
            )
        }
        PaneContent::Volume(volume) => {
            let camera = volume.camera;
            let config = VolumeConfig {
                yaw_deg: camera.yaw_deg(),
                pitch_deg: camera.pitch_deg(),
                eye_distance: camera.eye_distance(),
            };
            if !config.yaw_deg.is_finite()
                || !config.pitch_deg.is_finite()
                || !config.eye_distance.is_finite()
            {
                log::warn!("a 3D pane's camera is not finite; saving it as a map");
                return (PaneKind::Map, None, None);
            }
            (PaneKind::Volume, None, Some(config))
        }
    }
}

/// The pane content a saved [`PaneConfig`] describes, or `Map` where it describes
/// nothing usable.
///
/// # Why every refusal is a fall back to `Map` rather than a refusal to load
///
/// A config file can say anything: it is hand-editable, it is shared between
/// versions of the app, and it is written by a *later* version than the one
/// reading it as often as the reverse. The in-memory representation deliberately
/// cannot express a kind that disagrees with its state — `PaneContent` derives
/// the kind from the content — so every one of these cases is a shape that only
/// exists on the wire, and the honest reading of it is "this pane's kind was not
/// recoverable".
///
/// `Map` is the right fallback because it is the kind that needs nothing: it has
/// no per-kind state to be missing, every all-panes path in the app already
/// serves it, and a user who finds a map where they left a 3D view can convert it
/// back in one click. The alternative — refusing the whole config — would throw
/// away the site, the layout and every layer setting over one bad number.
///
/// Each case gets a `log::warn!` naming the pane, because a pane quietly coming
/// back as the wrong kind is otherwise indistinguishable from a user having
/// converted it themselves and forgotten.
fn restore_content(pane_idx: usize, pc: &PaneConfig, pane_count: usize) -> PaneContent {
    match pc.kind {
        PaneKind::Map => PaneContent::Map,
        PaneKind::CrossSection => {
            // A kind with no sub-config. Not merely missing state: it says the
            // file was written by something that did not agree with itself, and a
            // section pane invented here would have no line and no source.
            let Some(section) = pc.cross_section.as_ref() else {
                log::warn!(
                    "pane {pane_idx} is a cross-section with no section state; loading it as a map"
                );
                return PaneContent::Map;
            };
            // `None` is the ordinary state of a pane converted but not yet aimed,
            // and must not be confused with a line that failed to load.
            let line = match section.line.as_ref() {
                None => None,
                Some(saved) => {
                    // Through `SectionLine::new`, which is where non-finite,
                    // out-of-range and coincident endpoints are all refused —
                    // rather than by re-deriving those checks here, where they
                    // would be a second copy free to disagree.
                    let restored = SectionLine::new(
                        GeoPoint {
                            lat: saved.a_lat,
                            lon: saved.a_lon,
                        },
                        GeoPoint {
                            lat: saved.b_lat,
                            lon: saved.b_lon,
                        },
                    );
                    if restored.is_none() {
                        log::warn!(
                            "pane {pane_idx}'s saved section line is not a line that can be cut; \
                             loading it as a map"
                        );
                        return PaneContent::Map;
                    }
                    restored
                }
            };
            // A layout saved wider than the one being restored — six panes opened
            // on a phone — brings back indices that now name a different pane or
            // no pane at all. Dropped rather than clamped: retargeting a section
            // onto whichever map happens to sit at a nearby index is worse than
            // treating it as never having been aimed from anywhere.
            let source_pane = section.source_pane.filter(|idx| {
                let inside = *idx < pane_count;
                if !inside {
                    log::warn!(
                        "pane {pane_idx}'s section was drawn on pane {idx}, which this layout \
                         does not have; forgetting where it came from"
                    );
                }
                inside
            });
            PaneContent::CrossSection(Box::new(CrossSectionPane {
                line,
                source_pane,
                rendered_for: None,
            }))
        }
        PaneKind::Volume => {
            let Some(volume) = pc.volume.as_ref() else {
                log::warn!("pane {pane_idx} is a 3D view with no camera; loading it as a map");
                return PaneContent::Map;
            };
            // `OrbitCamera::restore` is the gate: it refuses non-finite angles
            // outright and wraps or clamps merely out-of-range ones, so a restored
            // camera can never hold a value `nudge` would not produce.
            let Some(camera) =
                OrbitCamera::restore(volume.yaw_deg, volume.pitch_deg, volume.eye_distance)
            else {
                log::warn!("pane {pane_idx}'s saved camera is not finite; loading it as a map");
                return PaneContent::Map;
            };
            PaneContent::Volume(Box::new(VolumePane {
                camera,
                rendered_for: None,
            }))
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
        use crate::pane::{OrbitDelta, PaneKind};

        let store = MemoryConfigStore::default();

        let baseline = crate::Gui::new();
        assert_ne!(baseline.loop_lookback_secs, 7200);
        assert_ne!(baseline.loop_speed_fps, 12.5);
        assert!(baseline.viewport_sync, "default is on; test flips it off");
        assert_eq!(
            baseline.pane(0).unwrap().kind(),
            PaneKind::Map,
            "default is a map; test converts it"
        );

        let mut gui = crate::Gui::new();
        gui.loop_lookback_secs = 7200;
        gui.loop_speed_fps = 12.5;
        gui.viewport_sync = false;
        // A 3D pane whose camera has been moved off its default, so the assertion
        // below is about the saved value rather than about two defaults agreeing.
        gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);
        let nudged = {
            let volume = gui.pane_mut(0).unwrap().volume_mut().expect("converted");
            volume.camera.nudge(OrbitDelta {
                yaw_deg: -47.5,
                pitch_deg: 12.25,
                zoom_factor: 1.5,
            });
            volume.camera
        };
        assert_ne!(
            nudged,
            crate::pane::OrbitCamera::default(),
            "precondition: the camera must differ from the default"
        );
        gui.save_ui_config(&store);

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);

        assert_eq!(restored.loop_lookback_secs, 7200);
        assert_eq!(restored.loop_speed_fps, 12.5);
        assert!(!restored.viewport_sync);
        assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Volume);
        assert_eq!(
            restored.pane(0).unwrap().volume().map(|v| v.camera),
            Some(nudged),
            "the pane came back as a 3D view aimed somewhere else"
        );
    }

    /// A cross-section pane's line and source survive the round trip.
    ///
    /// Separate from the test above because a section pane is the kind nothing
    /// creates yet: it is reachable only through `set_kind`, and its persistence
    /// has to be right *before* WP-G's draw interaction starts producing them —
    /// otherwise the first line a user ever draws is also the first one to be
    /// silently lost on restart.
    ///
    /// The endpoints are compared exactly. They are `f64` written and read as
    /// decimal by `serde_json`, which round-trips every finite `f64` exactly, and
    /// `SectionTarget`'s staleness comparison is bitwise — so an approximate
    /// assertion here would hide the one kind of drift that matters.
    #[test]
    fn a_drawn_section_line_survives_a_save_and_load() {
        use crate::pane::{GeoPoint, PaneKind, SectionLine};

        let store = MemoryConfigStore::default();
        let a = GeoPoint {
            lat: 35.0,
            lon: -97.8,
        };
        let b = GeoPoint {
            lat: 35.6,
            lon: -96.9,
        };

        let mut gui = crate::Gui::new();
        gui.set_pane_count_for_test(2);
        gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
        {
            let section = gui
                .pane_mut(1)
                .unwrap()
                .cross_section_mut()
                .expect("converted");
            section.line = SectionLine::new(a, b);
            section.source_pane = Some(0);
        }
        assert_eq!(
            gui.pane(0).unwrap().kind(),
            PaneKind::Map,
            "precondition: the other pane stays a map, so the kind is per pane"
        );
        gui.save_ui_config(&store);

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);

        assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Map);
        let section = restored
            .pane(1)
            .unwrap()
            .cross_section()
            .expect("pane 1 came back as something other than a section");
        assert_eq!(
            section.line.map(|line| (line.a(), line.b())),
            Some((a, b)),
            "the line came back somewhere else"
        );
        assert_eq!(section.source_pane, Some(0));
        assert_eq!(
            section.rendered_for, None,
            "the staleness key must not be persisted: it names a volume that is \
             not loaded, so a restored pane would think its image was current"
        );
    }

    /// Every shape a config can describe that the in-memory representation
    /// cannot, and each one falls back to a map rather than failing the load.
    ///
    /// `PaneContent` derives the kind from the content, so none of these is
    /// representable in the app — they exist only on the wire, where a file can
    /// say anything: hand-edited, shared between versions, or written by a later
    /// version than the one reading it. `Map` is the fallback because it is the
    /// kind that needs nothing, and refusing the whole config would throw away
    /// the site, the layout and every layer setting over one bad number.
    #[test]
    fn a_pane_config_that_cannot_be_a_pane_loads_as_a_map() {
        use crate::pane::PaneKind;

        for (name, pane_json) in [
            (
                "a section with no section state at all",
                r#"{"kind":"CrossSection"}"#,
            ),
            ("a 3D view with no camera", r#"{"kind":"Volume"}"#),
            (
                "a section line off the earth, which walks a well-defined great \
                 circle over nowhere and renders as empty coverage",
                r#"{"kind":"CrossSection","cross_section":{"line":
                   {"a_lat":1e9,"a_lon":-97.8,"b_lat":35.6,"b_lon":-96.9}}}"#,
            ),
            (
                "a zero-length section line, which has no bearing to walk along",
                r#"{"kind":"CrossSection","cross_section":{"line":
                   {"a_lat":35.0,"a_lon":-97.8,"b_lat":35.0,"b_lon":-97.8}}}"#,
            ),
        ] {
            let store = MemoryConfigStore::default();
            store
                .store(
                    UI_CONFIG_KEY,
                    &format!(r#"{{"pane_count":1,"site":"KTLX","panes":[{pane_json}]}}"#),
                )
                .unwrap();

            let mut restored = crate::Gui::new();
            assert!(
                restored.load_ui_config(&store),
                "{name}: the config must still load — falling back is per pane, \
                 not a refusal of the file"
            );
            assert_eq!(
                restored.pane(0).unwrap().kind(),
                PaneKind::Map,
                "{name}: loaded as a pane whose kind and state disagree"
            );
            assert_eq!(
                restored.pane(0).unwrap().site,
                "KTLX",
                "{name}: the rest of the pane was lost with its kind"
            );
        }
    }

    /// A section pane converted but not yet aimed is an ordinary state, not a
    /// corrupt one.
    ///
    /// It is what every section pane looks like between being created and having
    /// a line drawn on a map, so a loader that treated a missing line as
    /// unrecoverable would convert it back to a map on every restart.
    #[test]
    fn a_section_pane_with_no_line_yet_comes_back_as_a_section() {
        use crate::pane::PaneKind;

        let store = MemoryConfigStore::default();
        store
            .store(
                UI_CONFIG_KEY,
                r#"{"pane_count":1,"site":"KTLX",
                    "panes":[{"kind":"CrossSection","cross_section":{}}]}"#,
            )
            .unwrap();

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);

        let section = restored
            .pane(0)
            .unwrap()
            .cross_section()
            .expect("an unaimed section is a section");
        assert!(section.line.is_none());
        assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::CrossSection);
    }

    /// A source-pane index the restored layout does not have is forgotten, and the
    /// pane stays a section.
    ///
    /// This is a six-pane desktop config opened on a phone: the clamp narrows the
    /// layout, and an index saved against the wider one now names a different pane
    /// or none at all. Dropped rather than clamped, because retargeting a section
    /// onto whichever map happens to sit nearby is worse than treating it as never
    /// having been aimed from anywhere — and the kind is kept, because the line
    /// itself is still a perfectly good line.
    #[test]
    fn a_section_sourced_from_a_pane_that_is_gone_forgets_where_it_came_from() {
        use crate::pane::PaneKind;

        let store = MemoryConfigStore::default();
        store
            .store(
                UI_CONFIG_KEY,
                r#"{"pane_count":1,"site":"KTLX","panes":[
                    {"kind":"CrossSection","cross_section":{
                        "line":{"a_lat":35.0,"a_lon":-97.8,"b_lat":35.6,"b_lon":-96.9},
                        "source_pane":5}}]}"#,
            )
            .unwrap();

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);

        assert_eq!(
            restored.pane_count(),
            1,
            "precondition: one pane, so 5 is out"
        );
        let section = restored
            .pane(0)
            .unwrap()
            .cross_section()
            .expect("the kind survives a stale source index");
        assert_eq!(section.source_pane, None);
        assert!(
            section.line.is_some(),
            "the line is still a line; only where it was drawn was lost"
        );
        assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::CrossSection);
    }

    /// A config written before pane kinds existed loads as a screen full of maps.
    ///
    /// The container carries `#[serde(default)]`, so no per-field attribute is
    /// needed — the same mechanism `live_chunks` and `notifier_endpoint` rely on.
    /// This is the shape every already-installed copy has on disk.
    #[test]
    fn a_config_predating_pane_kinds_loads_as_maps() {
        use crate::pane::PaneKind;

        let store = MemoryConfigStore::default();
        store
            .store(
                UI_CONFIG_KEY,
                r#"{"pane_count":2,"site":"KMPX",
                    "panes":[{"site":"KMPX","zoom":7.0},{"site":"KOUN"}]}"#,
            )
            .unwrap();

        let mut restored = crate::Gui::new();
        assert!(restored.load_ui_config(&store));

        assert_eq!(
            (0..2)
                .map(|i| restored.pane(i).unwrap().kind())
                .collect::<Vec<_>>(),
            vec![PaneKind::Map, PaneKind::Map],
        );
        assert_eq!(restored.pane(0).unwrap().map_memory.zoom(), 7.0);
        assert_eq!(restored.pane(1).unwrap().site, "KOUN");
    }

    /// A finite camera outside the documented range is clamped, not discarded.
    ///
    /// The distinction the loader draws: a value that is *unusable* — non-finite,
    /// off the earth, a line with no bearing — costs the pane its kind, and one
    /// that is merely *out of range* is brought inside it. Only a hand-edited or
    /// version-skewed config can produce the second, and `restore_viewport`
    /// reasons the same way about a saved zoom: there is nothing to propagate, and
    /// the nearest legal camera beats discarding the pane over a number.
    #[test]
    fn a_saved_camera_out_of_range_is_clamped_rather_than_dropped() {
        use crate::pane::PaneKind;

        let store = MemoryConfigStore::default();
        store
            .store(
                UI_CONFIG_KEY,
                r#"{"pane_count":1,"site":"KTLX","panes":[
                    {"kind":"Volume","volume":
                        {"yaw_deg":-30.0,"pitch_deg":1000.0,"eye_distance":0.001}}]}"#,
            )
            .unwrap();

        let mut restored = crate::Gui::new();
        restored.load_ui_config(&store);

        assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Volume);
        let camera = restored
            .pane(0)
            .unwrap()
            .volume()
            .expect("a 3D pane")
            .camera;
        assert_eq!(camera.yaw_deg(), 330.0, "yaw wraps rather than clamping");
        assert!(
            camera.pitch_deg().abs() < 90.0,
            "pitch {}",
            camera.pitch_deg()
        );
        assert!(
            camera.eye_distance() > 1.0,
            "distance {}",
            camera.eye_distance()
        );
    }

    /// What the write-side finiteness filter actually prevents — and it is worse
    /// than "the config fails to serialize".
    ///
    /// `serde_json` does **not** refuse a non-finite float. It writes `null`. So
    /// the write succeeds silently, the file on disk looks fine, and it is the
    /// *next load* that fails: `null` will not deserialize into an `f32`, so
    /// `from_str::<UiConfig>` errors, `load_ui_config` logs one warning and
    /// returns `false`, and every setting in the file is gone. The user's only
    /// symptom is the app forgetting everything — one run after the mistake, with
    /// nothing at the time to connect the two, and permanently, because the next
    /// autosave rewrites the file from defaults.
    ///
    /// That is why the guard is on the *write* side for every float, including the
    /// ones whose in-memory writers already promise finiteness
    /// (`OrbitCamera::nudge`, `SectionLine::new`): a filter costs one pane its
    /// kind, a missing filter costs the user their whole configuration.
    #[test]
    fn a_non_finite_float_would_poison_the_config_file_permanently() {
        use crate::pane::PaneKind;

        assert_eq!(
            serde_json::to_string(&f32::NAN).expect("serde_json writes it happily"),
            "null",
            "if this ever starts erroring instead, these guards become about a \
             failed save rather than about a file that can never be read again"
        );
        assert!(
            serde_json::from_str::<f32>("null").is_err(),
            "and this is the half that makes it permanent"
        );

        // The property the filter protects: a `Gui` with a non-map pane writes a
        // config that loads back, rather than one that reads as corrupt.
        let mut gui = crate::Gui::new();
        gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);
        let json = gui
            .ui_config_json()
            .expect("a 3D pane stopped the config from being written at all");
        // Checked per field rather than by looking for `null` anywhere: the file
        // legitimately contains several, because an absent `Option` is written that
        // way and reads back as `None`. It is the **non-**`Option` numbers that
        // cannot survive one.
        let written: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        for field in ["yaw_deg", "pitch_deg", "eye_distance"] {
            let value = &written["panes"][0]["volume"][field];
            assert!(
                value.is_f64(),
                "{field} was written as {value}, which will fail every future load"
            );
        }

        let store = MemoryConfigStore::default();
        store.store(UI_CONFIG_KEY, &json).unwrap();
        let mut restored = crate::Gui::new();
        assert!(restored.load_ui_config(&store));
        assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Volume);
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
