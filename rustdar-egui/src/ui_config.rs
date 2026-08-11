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
    VolumeRegion,
};
use crate::ui_layout::WidthClass;

/// Serializable per-pane state persisted across sessions.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct PaneConfig {
    /// Tolerant of product names this build does not know: a config written by
    /// a later version must not poison the whole load (see
    /// [`product_or_default`]). The pane falls back to Reflectivity — the same
    /// product a fresh pane starts on — and the rest of the file survives.
    #[serde(deserialize_with = "product_or_default")]
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
    /// Whether this pane follows shared time (plan §3.7). Defaults **true**:
    /// a config written before the field existed described panes that all
    /// behaved as linked, and a downgrade dropping the field degrades to
    /// exactly that.
    #[serde(default = "default_true")]
    time_link: bool,
    /// Whether this pane's viewport belongs to the linked group (M11).
    /// Defaults **true** on the same reasoning as `time_link` — with the
    /// legacy `UiConfig::viewport_sync` global folded in on load, so an old
    /// config that had the global off comes back with every pane unlinked.
    #[serde(default = "default_true")]
    viewport_link: bool,
    /// Whether this pane's layer state belongs to the linked group (M11).
    /// Defaults **true**; the legacy `UiConfig::sync_layers` global is
    /// folded in on load exactly as `viewport_link`'s is.
    #[serde(default = "default_true")]
    layer_link: bool,
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

/// A 3D pane, as persisted: where the eye is, how far the vertical is stretched,
/// and what ground was picked.
///
/// The voxel grid is not here for the same reason the section raster is not:
/// it is derived from a volume, and rebuilding it is what opening the pane does.
/// The *region* is here rather than derived, and that is the difference between
/// it and the grid — it is a choice the user made with a drag, and losing it on
/// restart would silently put a carefully aimed 20 km box back to the 460 km
/// default with the pane still claiming to be a 3D view of a storm.
///
/// Flat `f64`s and `f32`s throughout, never the domain types, for the reason
/// [`SectionLineConfig`] gives: serde reads any number into these, and the
/// validating constructors (`VolumeRegion::new`, `OrbitCamera::restore`) are the
/// gate on the way back in.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct VolumeConfig {
    yaw_deg: f32,
    pitch_deg: f32,
    eye_distance: f32,
    /// The look-at point, in box-half-extent fractions. See
    /// [`OrbitCamera::pivot`](crate::pane::OrbitCamera::pivot).
    pivot: [f32; 3],
    vertical_exaggeration: f32,
    /// The picked region, or `None` for the default box about the site.
    region: Option<VolumeRegionConfig>,
    /// Which map pane the region was dragged on. Validated against the pane
    /// count on load, the same as a section's.
    source_pane: Option<usize>,
    /// Whether this pane has turned the map floor **off**, in
    /// [`crate::pane::VolumePane::hide_floor`]'s own inverted sense.
    ///
    /// Inverted on the wire as well as in memory so that `Default::default()` —
    /// `false` — is the floor showing: a config written before this field
    /// existed has no key, `#[serde(default)]` supplies `false`, and the pane
    /// comes back with its floor, which is the shipped default. Storing the
    /// positive `show_floor` instead would make every pre-existing config
    /// restore with the floor *off*.
    hide_floor: bool,
    /// Lit volume or isosurface. `#[serde(default)]` on the struct makes an
    /// older config a lit volume; the lenient deserializer makes a *newer*
    /// config's unknown mode a lit volume too, instead of a failed load —
    /// the same forward tolerance the product enum has.
    #[serde(deserialize_with = "view_mode_or_default")]
    view_mode: crate::pane::VolumeViewMode,
}

/// Deserialize a [`crate::pane::VolumeViewMode`], falling back to the default
/// (lit volume) when the name is unknown — see [`product_or_default`] for the
/// class of failure this closes.
fn view_mode_or_default<'de, D>(deserializer: D) -> Result<crate::pane::VolumeViewMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match crate::pane::VolumeViewMode::deserialize(&value) {
        Ok(mode) => Ok(mode),
        Err(_) => {
            log::warn!(
                "config names a 3D view mode this build does not know ({value}); using the lit volume"
            );
            Ok(crate::pane::VolumeViewMode::default())
        }
    }
}

/// A picked region, as persisted.
///
/// Two flat `f64`s and a third, rather than a `VolumeRegion`, because
/// `VolumeRegion`'s fields are private and its constructor is the validation —
/// exactly the arrangement [`SectionLineConfig`] has and for the same reason.
#[derive(Serialize, Deserialize, Default, Clone, Copy)]
#[serde(default)]
struct VolumeRegionConfig {
    centre_lat: f64,
    centre_lon: f64,
    half_width_km: f64,
}

impl Default for VolumeConfig {
    /// `OrbitCamera`'s own default, read out of it rather than restated — a
    /// second copy of the angles would drift, and the drift would show up as a
    /// 3D pane that opened at a different angle depending on whether its config
    /// predated the field.
    ///
    /// This is also what a config written before the pan and the exaggeration
    /// existed deserializes to, because of `#[serde(default)]` on the struct: an
    /// old file has no `pivot` and no `vertical_exaggeration`, and it comes back
    /// centred and at the default stretch rather than at a zeroed 0× that would
    /// collapse the box.
    fn default() -> Self {
        let camera = OrbitCamera::default();
        Self {
            yaw_deg: camera.yaw_deg(),
            pitch_deg: camera.pitch_deg(),
            eye_distance: camera.eye_distance(),
            pivot: camera.pivot(),
            vertical_exaggeration: camera.vertical_exaggeration(),
            region: None,
            source_pane: None,
            // The floor shows. Matches `VolumePane`'s derived default through
            // the same inversion — see the field's own doc.
            hide_floor: false,
            view_mode: crate::pane::VolumeViewMode::default(),
        }
    }
}

fn default_site() -> String {
    String::new()
}

fn default_time_step() -> i64 {
    600
}

fn default_true() -> bool {
    true
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
            time_link: true,
            viewport_link: true,
            layer_link: true,
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
    /// **Read-only legacy** (M11): the retired global viewport-sync toggle.
    /// Never written again — `skip_serializing` — but still read, so an old
    /// config that had it off loads with every restored pane's
    /// `viewport_link` seeded off (see `load_ui_config`). Absent on the wire
    /// (every config this build writes) it defaults true, which folds into
    /// the per-pane fields as a no-op.
    #[serde(skip_serializing, default = "default_true")]
    viewport_sync: bool,
    /// **Read-only legacy** (M11): the retired global layer-sync toggle,
    /// on the same terms as `viewport_sync`. On load, false seeds every
    /// restored pane's `layer_link` **and** `time_link` off — under the old
    /// model this one global gated the whole shared-time fan-out too, so the
    /// pane's stored `time_link` was inert while it was off, and honouring
    /// the observed behaviour means seeding both.
    #[serde(skip_serializing, default = "default_true")]
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
    /// The user's storm-motion override — the audit's known persistence gap,
    /// closed here. `#[serde(default)]` makes an older config load as
    /// "override off, default vector", which is what those sessions were.
    #[serde(default)]
    storm_motion_override: super::StormMotionOverride,
    /// Whether the pane pill rows render at full opacity unconditionally —
    /// the Interface section's "Pin pane controls". `#[serde(default)]`
    /// loads an older config as unpinned, which is what those sessions were.
    #[serde(default)]
    pin_pane_controls: bool,
    /// The user's saved presets (§3.11). Built-ins are compiled in and never
    /// written here; an older config simply has none.
    #[serde(default)]
    presets: Vec<super::PresetConfig>,
    /// The user's Volume Alpha curves, one entry per *edited* product.
    ///
    /// A list of exceptions rather than a curve per product, because absence
    /// is the meaningful default: a product with no entry renders through its
    /// palette's own alpha bit-exactly, and an old config without this field
    /// loads as "nothing edited" through the container's `#[serde(default)]`.
    #[serde(default)]
    volume_alpha: Vec<VolumeAlphaConfig>,
    /// The user's isosurface thresholds, one entry per *edited* product —
    /// the same store-of-exceptions arrangement as `volume_alpha`, for the
    /// same reason: absence means the argued per-product default, and an old
    /// config without this field loads as "nothing edited".
    #[serde(default)]
    volume_iso: Vec<VolumeIsoConfig>,
}

/// One product's persisted isosurface threshold.
#[derive(Serialize, Deserialize)]
struct VolumeIsoConfig {
    /// `None` for a product this build does not know; the entry is dropped
    /// on load, exactly as a Volume Alpha curve's is.
    #[serde(default, deserialize_with = "known_product_or_none")]
    product: Option<RadarProduct>,
    /// In the product's own units. Validated finite on load —
    /// `IsoThresholds::set` refuses non-finite values, the same door every
    /// persisted float goes through.
    threshold: f32,
}

/// One product's persisted Volume Alpha curve.
///
/// The alphas are stored as the same 256 bytes the LUT's alpha channel holds
/// — **deliberately not floats**. The finiteness filter every persisted float
/// goes through (see [`content_config`]) exists because `serde_json` writes a
/// NaN as `null` and the next load loses the whole file; a `u8` has no
/// non-finite values, so this encoding closes that class of loss by
/// construction instead of by filter. It is also exact: the render quantises
/// alpha to these bytes anyway, so nothing finer would survive the round trip.
#[derive(Serialize, Deserialize)]
struct VolumeAlphaConfig {
    /// `None` when the saved name is a product this build does not know — the
    /// entry is then dropped on load with a log line, because a curve drawn
    /// for one product must never be applied to another (see
    /// [`known_product_or_none`]). Saves always write `Some`, which serializes
    /// as the bare product name, so the on-disk format is unchanged.
    #[serde(default, deserialize_with = "known_product_or_none")]
    product: Option<RadarProduct>,
    /// Exactly [`crate::volume_alpha::CURVE_LEN`] alphas, entry 0 first.
    /// Validated on load — a wrong length is dropped with a warning, and
    /// entry 0 is re-clamped to transparent by `AlphaCurve::from_alphas`, so
    /// a hand-edited file cannot make the no-data index visible.
    alpha: Vec<u8>,
}

/// Deserialize a [`RadarProduct`], falling back to the default product when
/// the name is unknown.
///
/// `RadarProduct` is the one enum on the config wire without a tolerance
/// story: `PaneKind` falls back to `Map`, unknown `OverlayKind`s are filtered
/// out, and the worker wire's `from_wire_code` returns `None` — but a bare
/// `#[derive(Deserialize)]` enum fails on an unknown variant, and that error
/// used to propagate up and fail the *entire* config load. One product name
/// from a newer build would cost the user their site, layout and curves,
/// permanently, because the autosave then rewrites the file from defaults.
pub(crate) fn product_or_default<'de, D>(deserializer: D) -> Result<RadarProduct, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match RadarProduct::deserialize(&value) {
        Ok(product) => Ok(product),
        Err(_) => {
            log::warn!(
                "config names a product this build does not know ({value}); \
                 falling back to {}",
                RadarProduct::Reflectivity.name(),
            );
            Ok(RadarProduct::Reflectivity)
        }
    }
}

/// Deserialize a [`RadarProduct`] as `None` when the name is unknown, so the
/// caller can drop the entry it keys rather than misassign it.
///
/// The distinction from [`product_or_default`] matters: a pane with an unknown
/// product can honestly show the default product, but an alpha curve saved for
/// an unknown product must not be *reassigned* — applied to Reflectivity it
/// would silently change what the user sees, which is worse than losing it.
fn known_product_or_none<'de, D>(deserializer: D) -> Result<Option<RadarProduct>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match RadarProduct::deserialize(&value) {
        Ok(product) => Ok(Some(product)),
        Err(_) => {
            log::warn!("dropping a config entry keyed by an unknown product ({value})");
            Ok(None)
        }
    }
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
            storm_motion_override: super::StormMotionOverride::default(),
            pin_pane_controls: false,
            presets: Vec::new(),
            volume_alpha: Vec::new(),
            volume_iso: Vec::new(),
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
    ///
    /// # An asymmetry, examined and deliberately left
    ///
    /// This writes `self.panes` **unbounded** while `load_ui_config` restores only
    /// `.take(count)`. So a session split down from six panes to two writes six
    /// `PaneConfig`s and reads two back, and the four extra entries are dead
    /// weight in the file.
    ///
    /// Both of the tidy fixes are worse. Writing only `count` would delete the
    /// hidden panes' state permanently on the next autosave — the very state
    /// `Gui::panes` keeps them around for, so that re-splitting restores what they
    /// were showing. Reading all of them would resurrect panes past the layout's
    /// clamp, which is what the clamp exists to prevent. The asymmetry is what
    /// makes a re-split after a restart remember anything at all, and it costs a
    /// few hundred bytes.
    ///
    /// It does have one live consequence, handled where it lands rather than here:
    /// `config.panes` can be longer than the restored `pane_count`, so a section
    /// pane's `source_pane` has to be validated against the count and not against
    /// the list — see `restore_content`.
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
                    time_link: pane.time_link,
                    viewport_link: pane.viewport_link,
                    layer_link: pane.layer_link,
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
            // Dead values behind `skip_serializing`: the legacy globals are
            // never written again — the per-pane links above are the state.
            viewport_sync: true,
            sync_layers: true,
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
            // The same NaN guard every persisted float gets (see the note on
            // this function): `DragValue` parses "nan", and one non-finite
            // number costs the whole file on the *next* load.
            storm_motion_override: {
                let motion = self.storm_motion_override;
                let default = super::StormMotionOverride::default();
                super::StormMotionOverride {
                    enabled: motion.enabled,
                    speed_kt: if motion.speed_kt.is_finite() {
                        motion.speed_kt
                    } else {
                        default.speed_kt
                    },
                    direction_deg: if motion.direction_deg.is_finite() {
                        motion.direction_deg
                    } else {
                        default.direction_deg
                    },
                }
            },
            pin_pane_controls: self.pin_pane_controls,
            // The elevations go through the same finiteness door; the capture
            // path already filters, so this guards only hand-poked state.
            presets: self
                .presets
                .iter()
                .map(|preset| super::PresetConfig {
                    name: preset.name.clone(),
                    pane_count: preset.pane_count,
                    panes: preset
                        .panes
                        .iter()
                        .map(|pane| super::catalog::PresetPane {
                            product: pane.product,
                            elevation: if pane.elevation.is_finite() {
                                pane.elevation
                            } else {
                                0.0
                            },
                        })
                        .collect(),
                    overlays: preset.overlays.clone(),
                })
                .collect(),
            volume_alpha: {
                // Sorted by product code so the autosave's "has anything
                // changed?" string comparison cannot be defeated by
                // `HashMap` iteration order.
                let mut curves: Vec<VolumeAlphaConfig> = self
                    .volume_alpha
                    .entries()
                    .map(|(product, curve)| VolumeAlphaConfig {
                        product: Some(product),
                        alpha: curve.alphas().to_vec(),
                    })
                    .collect();
                curves.sort_by_key(|c| c.product.map(|p| p.code()));
                curves
            },
            volume_iso: {
                // Sorted for the same autosave-comparison reason as the
                // curves; non-finite thresholds cannot exist in the store
                // (`IsoThresholds::set` refuses them), so no filter here.
                let mut thresholds: Vec<VolumeIsoConfig> = self
                    .volume_iso
                    .entries()
                    .map(|(product, threshold)| VolumeIsoConfig {
                        product: Some(product),
                        threshold,
                    })
                    .collect();
                thresholds.sort_by_key(|c| c.product.map(|p| p.code()));
                thresholds
            },
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
        self.storm_motion_override = config.storm_motion_override;
        self.pin_pane_controls = config.pin_pane_controls;
        self.presets = config.presets;

        // The Volume Alpha curves. Replaced wholesale rather than merged —
        // the store starts empty and a load is the session's beginning — and
        // validated entry by entry: a curve of the wrong length is a config
        // from a different format (or a hand edit) and is dropped with its
        // name in the log, not truncated into a curve the user never drew.
        // An old config simply has no entries, which is the untouched,
        // bit-exact state.
        self.volume_alpha = crate::volume_alpha::AlphaCurves::default();
        for entry in config.volume_alpha {
            // `None` is a product name this build does not know — already
            // logged by the deserializer; the curve is dropped rather than
            // applied to a product the user never drew it for.
            let Some(product) = entry.product else {
                continue;
            };
            let Ok(alphas) = <[u8; crate::volume_alpha::CURVE_LEN]>::try_from(entry.alpha) else {
                log::warn!(
                    "the saved Volume Alpha curve for {} is not {} entries; dropping it",
                    product.name(),
                    crate::volume_alpha::CURVE_LEN,
                );
                continue;
            };
            // `from_alphas` re-clamps entry 0, so a hand-edited file cannot
            // make the no-data index visible.
            self.volume_alpha.set(
                product,
                crate::volume_alpha::AlphaCurve::from_alphas(alphas),
            );
        }

        // The isosurface thresholds, replaced wholesale for the same reason.
        // A `None` product (unknown name) is dropped; a non-finite threshold
        // is refused by `set` itself, the same door every persisted float
        // goes through.
        self.volume_iso = crate::volume_iso::IsoThresholds::default();
        for entry in config.volume_iso {
            let Some(product) = entry.product else {
                continue;
            };
            self.volume_iso.set(product, entry.threshold);
        }

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
                // The legacy-global fold below, for a pane the config never
                // described: its links are the defaults ANDed with the same
                // globals, so a legacy sync-off layout cannot grow a linked
                // pane out of thin air.
                pane.viewport_link = config.viewport_sync;
                pane.layer_link = config.sync_layers;
                pane.time_link = config.sync_layers;
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
            // The M11 migration fold: the per-pane links ANDed with the
            // legacy globals. A config this build wrote carries no globals,
            // serde defaults them true, and the AND is the identity; an old
            // config with a global **off** loads with every pane's
            // corresponding link off — which is the behaviour that config
            // described. `sync_layers` folds into `time_link` too: under the
            // old model that one global gated the whole shared-time fan-out,
            // so a stored `time_link` was inert while it was off, and
            // honouring observed behaviour means seeding both. The globals
            // are never written again (`skip_serializing`), so this fold
            // runs at most once per legacy file.
            pane.time_link = pc.time_link && config.sync_layers;
            pane.viewport_link = pc.viewport_link && config.viewport_sync;
            pane.layer_link = pc.layer_link && config.sync_layers;
            // Capture the first pane's legacy Radar toggle for migration.
            if legacy_radar_enabled.is_none()
                && let Some(&enabled) = pc.layers.get(&LayerKind::Radar)
            {
                legacy_radar_enabled = Some(enabled);
            }
            // `set_content` rather than a write to `content`, because the kind and
            // the per-kind state arrive together here and `restore_content` has
            // already decided both — and because that setter is what enforces what
            // a kind implies, so a restored non-map pane arrives with the same
            // invariants as a converted one. This is the legitimate writer outside
            // the UI pass; `Gui::request_pane_kind` exists for the writers *inside*
            // it, where the pane may be `mem::take`n.
            pane.set_content(restore_content(i, pc, count));
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
                pivot: camera.pivot(),
                vertical_exaggeration: camera.vertical_exaggeration(),
                region: volume.region.map(|region| VolumeRegionConfig {
                    centre_lat: region.centre().lat,
                    centre_lon: region.centre().lon,
                    half_width_km: region.half_width_km(),
                }),
                source_pane: volume.source_pane,
                hide_floor: volume.hide_floor,
                view_mode: volume.view_mode,
            };
            // Every float that reaches the file, not merely the three angles:
            // `serde_json` writes a non-finite `f32` as `null`, which comes back
            // through `#[serde(default)]` as the *default* rather than as an
            // error — so a NaN pivot would be laundered into a centred one and a
            // NaN exaggeration into 3×, and the pane would silently move. The
            // constructors make this unreachable today; it is here because the
            // failure is a silent one and the check is four comparisons.
            if !config.yaw_deg.is_finite()
                || !config.pitch_deg.is_finite()
                || !config.eye_distance.is_finite()
                || !config.pivot.iter().all(|p| p.is_finite())
                || !config.vertical_exaggeration.is_finite()
                || !config.region.is_none_or(|r| {
                    r.centre_lat.is_finite()
                        && r.centre_lon.is_finite()
                        && r.half_width_km.is_finite()
                })
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
                // A restored pane holds nothing rendered, so it holds no reason
                // for that either: `rendered_for: None` is what makes the
                // dispatcher cut the section again against whatever volume the
                // pane's site loads, and `unavailable: None` is what stops a
                // reason from a previous session outliving its cause.
                ..Default::default()
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
            let Some(camera) = OrbitCamera::restore(
                volume.yaw_deg,
                volume.pitch_deg,
                volume.eye_distance,
                volume.pivot,
                volume.vertical_exaggeration,
            ) else {
                log::warn!("pane {pane_idx}'s saved camera is not finite; loading it as a map");
                return PaneContent::Map;
            };
            // Through `VolumeRegion::new`, which is where an off-Earth or
            // non-finite centre is refused and a half-width past the resampler's
            // limits is wound back to them — rather than re-deriving those checks
            // here, where they would be a second copy free to disagree.
            //
            // A region that does not survive that gate costs the *region* and not
            // the pane, which is the opposite of the camera above. The difference
            // is what each one is: a pane with no camera has no view at all, but a
            // pane with no region has a perfectly good default box about its site
            // — so dropping to that is strictly better than dropping to a map.
            let region = match volume.region {
                None => None,
                Some(saved) => {
                    let restored = VolumeRegion::new(
                        GeoPoint {
                            lat: saved.centre_lat,
                            lon: saved.centre_lon,
                        },
                        saved.half_width_km,
                    );
                    if restored.is_none() {
                        log::warn!(
                            "pane {pane_idx}'s saved 3D region is not a patch of ground that can \
                             be resampled; falling back to the default box about the site"
                        );
                    }
                    restored
                }
            };
            // The same bound a section's source pane gets, and dropped rather
            // than clamped for the same reason: a layout saved wider than the one
            // being restored brings back an index that now names a different pane.
            let source_pane = volume.source_pane.filter(|idx| {
                let inside = *idx < pane_count;
                if !inside {
                    log::warn!(
                        "pane {pane_idx}'s 3D region was dragged on pane {idx}, which this layout \
                         does not have; forgetting where it came from"
                    );
                }
                inside
            });
            PaneContent::Volume(Box::new(VolumePane {
                camera,
                region,
                source_pane,
                rendered_for: None,
                // Persisted, like every other choice that changes *what the
                // pane is a picture of*. Reopening the app is 1:1 with how it
                // was closed, live data excepted, and a floor the user turned
                // off coming back on is a visible difference on launch.
                //
                // The inversion is `VolumePane::hide_floor`'s own and is carried
                // straight through rather than flipped here, so a config written
                // before the field existed reads `false` through
                // `#[serde(default)]` and keeps the shipped default: the floor
                // shows. No finiteness filter applies — it is a `bool`, and the
                // `null`-for-non-finite hazard the camera floats carry (see the
                // save side) has no analogue here.
                hide_floor: volume.hide_floor,
                // Not persisted either — the curves are (per product, below
                // the pane list); an open tool window is session posture.
                alpha_editor_open: false,
                view_mode: volume.view_mode,
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

#[path = "ui_config/live_chunks_config_tests.rs"]
#[cfg(test)]
mod live_chunks_config_tests;

#[path = "ui_config/notifier_config_tests.rs"]
#[cfg(test)]
mod notifier_config_tests;

#[path = "ui_config/storm_motion_config_tests.rs"]
#[cfg(test)]
mod storm_motion_config_tests;

#[path = "ui_config/presets_config_tests.rs"]
#[cfg(test)]
mod presets_config_tests;

#[path = "ui_config/tests.rs"]
#[cfg(test)]
mod tests;
