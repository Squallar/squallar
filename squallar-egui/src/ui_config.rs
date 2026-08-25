use serde::{Deserialize, Serialize};
use squallar_source::handler::{PaneMut, PaneRef};
use std::collections::BTreeMap;

use squallar_kv::KvStore;

/// Key the UI layout is persisted under.
pub const UI_CONFIG_KEY: &str = "ui";

/// **Where the last pre-slot config is kept, untouched, forever.**
///
/// WO-E6b is the one structural rewrite of this file's shape: a pane's flat
/// radar selection and its three parallel layer containers became one ordered
/// slot list. That rewrite is not reversible by a downgrade — an older build
/// reading a v3 file keeps every byte it cannot name but cannot put the panes
/// back together — so the bytes as they stood are copied here **once**,
/// before the first v3 write, and never written again.
///
/// The name says the version of what it holds: v2 is the last shape the flat
/// fields existed in. (The order that commissioned this called it
/// `ui_config.v1.backup`, written when the shape move was expected to be the
/// 1 → 2 rung; M2's `gps_config` split had already taken that rung, so the
/// bytes being preserved are v2 and the key says so.)
pub const UI_CONFIG_BACKUP_KEY: &str = "ui.v2.backup";

use squallar_overlays::spc::outlook::OutlookDay;
use squallar_source::id::{LayerId, known};
use squallar_source::product::FieldId;
use squallar_units::UserPreferences;

use super::PaneLayout;
use super::PaneState;

#[path = "ui_config/migrate.rs"]
mod migrate;
use crate::pane::{
    CrossSectionPane, MapPane, MapRender, OrbitCamera, PaneContent, SectionLine, VolumePane,
    VolumeRegion,
};
use crate::ui_layout::WidthClass;
use squallar_geo::GeoPoint;
use squallar_radar::fields as radar_fields;

/// Serializable per-pane state persisted across sessions.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct PaneConfig {
    /// **Legacy, read-only**: the retired per-pane layer toggles, keyed by
    /// the serde spellings of the layer-kind enum that used to type this
    /// map. The enum itself is gone; the wire format never changes shape by
    /// that (its keys were always strings on the wire), and the one fact
    /// still read out of it is the first pane's `"Radar"` entry, which
    /// seeds the global handler when a pre-`overlay_states` file is
    /// migrated. Saves write it empty, as they have since the overlay
    /// registry took over.
    layers: BTreeMap<String, bool>,
    spc_day: OutlookDay,
    /// Time step size in seconds (0 = single scan mode).
    #[serde(default = "default_time_step")]
    time_step_secs: i64,
    /// **The instant this pane is parked on**, UTC, or absent for a pane that
    /// is following live data.
    ///
    /// AN ABSOLUTE INSTANT, NOT A RELATIVE OFFSET, and that is a deliberate
    /// split from how the HRRR layer stores its run. That one encodes a
    /// relative token so a config closed on Friday with 18Z picked does not
    /// reopen on Monday three days in the past. The argument is sound for a
    /// forecast run, whose whole meaning is "the latest one"; it is wrong for
    /// a scrubbed radar timeline, where the instant IS the thing the user
    /// chose. A pane parked on the 2013-05-20 Moore volume that reopened on
    /// live data would have discarded the only state that mattered, which is
    /// what "reopen is exactly 1:1" exists to forbid.
    ///
    /// The pane makes it visible rather than silent: a parked pane says so in
    /// its transport and its status bar, so reopening into the past is
    /// legible, unlike the reopened-forecast case that argument was written
    /// about.
    /// Written as `YYYY-MM-DDTHH:MM:SS`, UTC and naive. A string rather than a
    /// `NaiveDateTime` because chrono's `serde` feature is not enabled anywhere
    /// in this workspace and turning it on for one optional field would widen a
    /// workspace-wide dependency; the model layer already encodes its run as a
    /// string for its own reasons. It also makes the field hand-writable, which
    /// is the whole point for a seeded scene.
    ///
    /// Read tolerantly: an unparseable value reads as live, exactly as a
    /// malformed product or site does elsewhere in this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    /// **Whether this pane's selection follows live data.**
    ///
    /// Persisted separately from [`Self::as_of`] and NOT derived from it,
    /// because `PaneState::viewing_live` documents the two as different
    /// questions: a pane playing a loop depicts an older instant every frame
    /// while still following the live site. Deriving this from a parked clock
    /// would stop the chunk feed the moment a loop played.
    ///
    /// It is load-bearing beyond the Live button: it is what gates the archive
    /// auto-poll. A pane restored parked but still flagged live had the poll
    /// fetch the current volume and install it straight over the archived one
    /// the pane had just asked for -- which is why a screenshot pinned to
    /// Hurricane Ian came back showing this afternoon's Florida convection.
    ///
    /// `skip_serializing_if` for the same reason [`Self::as_of`] has one: a live
    /// pane is the overwhelming default, and writing the key into every pane of
    /// every config would change the bytes of files that say nothing about it --
    /// which `a_config_naming_an_unregistered_layer_is_written_back_byte_preserved`
    /// exists to forbid.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    viewing_live: bool,
    /// **Whether this pane had a loop armed**, and whether it was playing.
    ///
    /// `"playing"`, `"paused"`, or absent for a pane with no loop. A loop is UI
    /// state a user set up deliberately — a span, a transport layer, a rate —
    /// and "reopen is exactly 1:1" does not carve out the expensive parts.
    /// Before this, closing the app on a playing loop reopened on a still
    /// picture with no sign that anything had been lost.
    ///
    /// **What is restored is the ARM, not the frames.** Frames are textures and
    /// were never persisted; a restored loop re-lists and re-fetches exactly as
    /// arming it by hand does, and converges on the same picture. That is the
    /// same bargain a restored parked scan already makes — [`Self::as_of`]
    /// names an instant whose volume is refetched on open — and it is why this
    /// is three states rather than a frame index: an index into a frame list
    /// that does not exist yet would name nothing.
    ///
    /// A string rather than a bool pair because the three states are exclusive
    /// and a `(armed, playing)` pair can spell a fourth that means nothing.
    /// Read tolerantly: an unrecognised value reads as no loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    loop_playback: Option<String>,
    /// **Which link group this pane belongs to**, as the group's index;
    /// `null` for a pane in no group at all.
    ///
    /// The default is deliberately not `Option`'s — an absent key must read
    /// as [`crate::pane::GroupId::FIRST`], not as "no group", because every
    /// config written before groups existed described one group holding every
    /// pane. That is the whole migration: no `migrate.rs` step, no version
    /// bump, and the flags beside it keep their meaning unchanged.
    #[serde(default = "default_group")]
    group: Option<u8>,
    /// Whether this pane follows shared time (plan §3.7). Defaults **true**.
    #[serde(default = "default_true")]
    time_link: bool,
    /// Whether this pane's viewport belongs to the linked group.
    #[serde(default = "default_true")]
    viewport_link: bool,
    /// Whether this pane's layer state belongs to the linked group.
    #[serde(default = "default_true")]
    layer_link: bool,
    /// **Which layer this pane's loop transport addresses** — the timeline
    /// the ∞ toggle and the transport buttons move. An absent key reads as
    /// [`known::RADAR`], which is what every config written before the field
    /// existed described: radar was the only layer the transport could
    /// address. See [`crate::pane::PaneState::transport_layer`].
    ///
    /// **Written only when it is not radar.** A key whose absence and whose
    /// default say the same thing must not appear in a file that never moved
    /// it: a saved config is asserted to come back byte-for-byte under a
    /// build that cannot serve one of its layers, and a new key on every pane
    /// would break that reopen for every user who never asked for a model
    /// transport.
    #[serde(default = "default_transport", skip_serializing_if = "is_radar")]
    transport: LayerId,
    /// **This pane's layer stack, bottom to top** — the v3 shape. One entry
    /// per layer, each carrying its own id, enabled flag and saved config;
    /// the list's order IS the draw order. Replaces v2's three parallel
    /// `draw_order` / `enabled_overlays` / `overlay_configs` containers, and
    /// carries the radar layer's slot, whose config holds this pane's site,
    /// product, elevation and live-chunk switch.
    ///
    /// The key is `layer_slots`, not `layers`: [`Self::layers`] is the v0
    /// toggle map and still has to be readable, so the two cannot share a
    /// name — an older build reading a v3 file would fail its whole pane on
    /// the type mismatch and salvage it to defaults.
    #[serde(default)]
    layer_slots: SlotList,
    /// **The layers this pane has been curated to exclude**, with the config
    /// each held when it left.
    ///
    /// The tombstone list behind
    /// [`LayerStack`](crate::pane::LayerStack). Since a stack stopped being a
    /// complete projection of the registry, a layer's *absence* from
    /// [`Self::layer_slots`] no longer says anything on its own — a build that
    /// serves a layer the file never named cannot tell "written by a build
    /// without it" from "removed by the user" — so the removal is written down
    /// rather than inferred, and this is where.
    ///
    /// **Additive, and no [`migrate::CONFIG_VERSION`] bump.** `#[serde(default)]`
    /// reads an absent key as "nothing removed", which is exactly what every
    /// file written before curation existed meant, so an existing config loads
    /// to the same stack it loads to today. `skip_serializing_if` keeps the key
    /// off the wire entirely until a user removes something, so a pane that has
    /// never been curated writes byte-for-byte what it wrote before — including
    /// the downgrade fixture, whose whole job is to be that byte capture.
    ///
    /// A [`SlotList`], not a bare id list, for two reasons: the config that
    /// rides along with the id is what makes re-adding restore settings instead
    /// of resetting them, and the type already carries an unknown half, so a
    /// tombstone written by a newer build survives a session under this one.
    #[serde(default, skip_serializing_if = "SlotList::is_empty")]
    removed_layers: SlotList,
    /// Map zoom level, as `walkers::MapMemory` reports it.
    #[serde(default)]
    zoom: Option<f64>,
    /// Where the map is centred, as `(lat, lon)`, when the user has panned away
    /// from the site.
    #[serde(default)]
    center: Option<(f64, f64)>,
    /// What kind of pane this is: a patch of ground, or a vertical
    /// cross-section through one.
    #[serde(default, deserialize_with = "kind_or_default")]
    kind: PaneKindConfig,
    /// How a map pane draws its ground: the plan view or the 3D volume.
    #[serde(default, deserialize_with = "render_or_default")]
    render: MapRender,
    /// A cross-section pane's own state, present only when [`Self::kind`] is
    /// `CrossSection`.
    #[serde(default)]
    cross_section: Option<CrossSectionConfig>,
    /// A map pane's 3D state, present when it is not the default one.
    #[serde(default)]
    volume: Option<VolumeConfig>,
    /// Every pane-level key this build does not know, verbatim.
    #[serde(flatten)]
    unknown: serde_json::Map<String, serde_json::Value>,
}

/// A draw-order list as it appears on the wire: every layer id — a string
/// entry names a layer whether or not this build has a handler for it —
/// plus, verbatim, any list element that is not a string at all.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KindList {
    /// Every layer id in the list, in wire order — registered or not.
    pub(crate) known: Vec<LayerId>,
    /// The list elements that are not strings, verbatim, appended after the
    /// ids on the way back out.
    pub(crate) unknown: Vec<serde_json::Value>,
}

impl From<Vec<LayerId>> for KindList {
    fn from(known: Vec<LayerId>) -> Self {
        Self {
            known,
            unknown: Vec::new(),
        }
    }
}

impl Serialize for KindList {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.known.len() + self.unknown.len()))?;
        for kind in &self.known {
            seq.serialize_element(kind)?;
        }
        for value in &self.unknown {
            seq.serialize_element(value)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for KindList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
        let mut known = Vec::new();
        let mut unknown = Vec::new();
        for value in raw {
            match value {
                serde_json::Value::String(name) => known.push(LayerId::new(name)),
                other => unknown.push(other),
            }
        }
        if !unknown.is_empty() {
            let values: Vec<String> = unknown.iter().map(ToString::to_string).collect();
            log::warn!(
                "draw_order carries {} non-string entr(y/ies) ({}); keeping \
                 them verbatim for whatever wrote them",
                unknown.len(),
                values.join(", "),
            );
        }
        Ok(Self { known, unknown })
    }
}

/// **One layer's slot, as it appears on the wire** — the v3 per-pane shape.
///
/// `enabled` is deliberately an `Option`: a slot that states nothing is a
/// slot whose layer has no saved opinion, and the load resolves it from the
/// handler exactly as an absent `enabled_overlays` entry always did. Writing
/// a `false` there instead would turn "never chosen" into "chosen off".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct SlotConfig {
    /// The layer id — an open string, registered or not.
    id: String,
    /// Whether this pane draws the layer, or absent for "ask the handler".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    /// The layer's saved config, absent when there is none to save.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    config: serde_json::Value,
}

/// A pane's slot list as it appears on the wire: every entry this build can
/// read as a slot, plus, verbatim, every entry it cannot.
///
/// The unknown half exists for the same reason [`KindList`]'s does — a list
/// element written by a build that is not this one rides the session out and
/// goes back to the file untouched.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SlotList {
    known: Vec<SlotConfig>,
    unknown: Vec<serde_json::Value>,
}

impl SlotList {
    /// Whether the list would serialize to `[]` — the test
    /// `skip_serializing_if` on `removed_layers` reads, so a pane that has
    /// removed nothing writes no key at all.
    fn is_empty(&self) -> bool {
        self.known.is_empty() && self.unknown.is_empty()
    }
}

impl Serialize for SlotList {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.known.len() + self.unknown.len()))?;
        for slot in &self.known {
            seq.serialize_element(slot)?;
        }
        for value in &self.unknown {
            seq.serialize_element(value)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for SlotList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
        let mut known = Vec::new();
        let mut unknown = Vec::new();
        for value in raw {
            match SlotConfig::deserialize(&value) {
                Ok(slot) => known.push(slot),
                Err(_) => unknown.push(value),
            }
        }
        if !unknown.is_empty() {
            let values: Vec<String> = unknown.iter().map(ToString::to_string).collect();
            log::warn!(
                "layer_slots carries {} entr(y/ies) that are not slots ({}); \
                 keeping them verbatim for whatever wrote them",
                unknown.len(),
                values.join(", "),
            );
        }
        Ok(Self { known, unknown })
    }
}

/// The spelling [`PaneConfig::as_of`] is written and read in.
const AS_OF_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

/// The three states [`PaneConfig::loop_playback`] can hold.
const LOOP_PLAYING: &str = "playing";
const LOOP_PAUSED: &str = "paused";

/// **What this pane's loop was doing**, from its transport layer's phase, or
/// `None` for a pane with no loop.
///
/// The transport is the timeline playback walks, so its phase is the pane's
/// loop state; the other armed layers follow it. The transient phases —
/// listing, rendering — collapse to `paused`, because what they have in common
/// with a paused loop is "armed, not advancing", and a config written mid-fetch
/// must not reopen into a state that claims frames it never had.
fn loop_playback_of(pane: &PaneState) -> Option<String> {
    match pane.transport_state().phase {
        crate::pane::LoopPhase::Inactive => None,
        crate::pane::LoopPhase::Playing => Some(LOOP_PLAYING.to_string()),
        crate::pane::LoopPhase::FetchingScanList
        | crate::pane::LoopPhase::Rendering
        | crate::pane::LoopPhase::Ready
        | crate::pane::LoopPhase::Paused => Some(LOOP_PAUSED.to_string()),
    }
}

/// **Whether a restored pane wants a loop armed, and whether it should play.**
///
/// `None` for no loop. Read tolerantly, like every other field in this file: a
/// value that is neither spelling reads as no loop rather than as an error,
/// because a config that has been hand-edited or written by a future build must
/// still open.
pub fn loop_arm_from_config(value: Option<&str>) -> Option<crate::pane::LoopArm> {
    match value {
        Some(v) if v == LOOP_PLAYING => Some(crate::pane::LoopArm { playing: true }),
        Some(v) if v == LOOP_PAUSED => Some(crate::pane::LoopArm { playing: false }),
        _ => None,
    }
}

/// A parked instant, or `None` for anything this build cannot read as one.
fn parse_as_of(text: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(text, AS_OF_FORMAT).ok()
}

impl PaneConfig {
    /// This pane's radar slot, the one that carries its selection.
    fn radar_slot(&self) -> Option<&SlotConfig> {
        self.layer_slots
            .known
            .iter()
            .find(|slot| slot.id == known::RADAR.as_str())
    }

    /// One member of the radar slot's config, or `None` when there is no
    /// radar slot, no config, or no such member.
    fn radar_member(&self, key: &str) -> Option<&serde_json::Value> {
        self.radar_slot()?.config.get(key)
    }

    /// This pane's radar site, read through the same tolerance the field had
    /// when it was a field: a name no radar could be called is ignored.
    fn site(&self) -> String {
        self.radar_member("site")
            .and_then(|v| site_or_default(v).ok())
            .unwrap_or_default()
    }

    /// This pane's radar product, read through [`product_or_default`] — the
    /// one tolerant path, unchanged by the move into the slot.
    fn selected_product(&self) -> FieldId {
        self.radar_member("product")
            .and_then(|v| product_or_default(v).ok())
            .unwrap_or(radar_fields::known::REFLECTIVITY)
    }

    /// This pane's elevation angle in degrees.
    fn selected_elevation(&self) -> f32 {
        self.radar_member("elevation")
            .and_then(serde_json::Value::as_f64)
            .map(|v| v as f32)
            .unwrap_or(0.0)
    }
}

/// A pane kind, as it appears on the wire.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PaneKindConfig {
    #[default]
    Map,
    CrossSection,
    /// Written by builds in which a 3D view was a pane kind of its own.
    Volume,
}

/// A cross-section pane, as persisted.
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
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct SectionLineConfig {
    a_lat: f64,
    a_lon: f64,
    b_lat: f64,
    b_lon: f64,
}

/// A 3D pane's picked region, as persisted.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct VolumeRegionConfig {
    centre_lat: f64,
    centre_lon: f64,
    half_east_km: f64,
    half_north_km: f64,
}

impl VolumeRegionConfig {
    /// The region this block names, or `None` for one that names none —
    /// including every block written by the square-drag form this replaced.
    fn restore(&self) -> Option<VolumeRegion> {
        if !(self.half_east_km > 0.0 && self.half_north_km > 0.0) {
            return None;
        }
        VolumeRegion::new(
            GeoPoint {
                lat: self.centre_lat,
                lon: self.centre_lon,
            },
            squallar_radar::voxel::HalfExtentKm {
                east_km: self.half_east_km,
                north_km: self.half_north_km,
            },
        )
    }
}

/// A 3D pane, as persisted: where the eye is, how far the vertical is stretched,
/// and what ground was picked.
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
    /// Whether this pane has turned the map floor **off**, in
    /// [`crate::pane::VolumePane::hide_floor`]'s own inverted sense.
    hide_floor: bool,
    /// Lit volume or isosurface. `#[serde(default)]` on the struct makes an
    /// older config a lit volume; the lenient deserializer makes a *newer*
    /// config's unknown mode a lit volume too, instead of a failed load —
    /// the same forward tolerance the product enum has.
    #[serde(deserialize_with = "view_mode_or_default")]
    view_mode: crate::pane::VolumeViewMode,
    /// The ground this pane resamples, or absent for the volume's own reach.
    region: Option<VolumeRegionConfig>,
    /// Which map pane the region was dragged on, or absent for a pane nobody
    /// aimed. See [`crate::pane::VolumePane::source_pane`].
    source_pane: Option<usize>,
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

impl VolumeConfig {
    /// Whether this says anything a freshly opened pane does not already say.
    fn differs_from_default(&self) -> bool {
        let default = Self::default();
        self.yaw_deg != default.yaw_deg
            || self.pitch_deg != default.pitch_deg
            || self.eye_distance != default.eye_distance
            || self.pivot != default.pivot
            || self.vertical_exaggeration != default.vertical_exaggeration
            || self.hide_floor != default.hide_floor
            || self.view_mode != default.view_mode
            || self.region.is_some()
            || self.source_pane.is_some()
    }
}

impl Default for VolumeConfig {
    /// `OrbitCamera`'s own default, read out of it rather than restated — a
    /// second copy of the angles would drift, and the drift would show up as a
    /// 3D pane that opened at a different angle depending on whether its config
    /// predated the field.
    fn default() -> Self {
        let camera = OrbitCamera::default();
        Self {
            yaw_deg: camera.yaw_deg(),
            pitch_deg: camera.pitch_deg(),
            eye_distance: camera.eye_distance(),
            pivot: camera.pivot(),
            vertical_exaggeration: camera.vertical_exaggeration(),
            hide_floor: false,
            view_mode: crate::pane::VolumeViewMode::default(),
            region: None,
            source_pane: None,
        }
    }
}

fn default_time_step() -> i64 {
    600
}

/// Whether a `bool` field is at its `true` default, for `skip_serializing_if`.
fn is_true(value: &bool) -> bool {
    *value
}

fn default_true() -> bool {
    true
}

/// A pane whose config names no group is a pane from before groups existed,
/// and every such pane was in the one group the flags described. See
/// [`PaneConfig::group`].
fn default_group() -> Option<u8> {
    Some(crate::pane::GroupId::FIRST.index())
}

/// A pane whose config names no transport layer is a pane from before the
/// transport could address anything but radar. See [`PaneConfig::transport`].
fn default_transport() -> LayerId {
    known::RADAR
}

/// Whether a transport layer is the one an absent key already means. See
/// [`PaneConfig::transport`].
fn is_radar(id: &LayerId) -> bool {
    *id == known::RADAR
}

impl Default for PaneConfig {
    fn default() -> Self {
        Self {
            layers: BTreeMap::new(),
            spc_day: OutlookDay::Day1,
            time_step_secs: 600,
            as_of: None,
            viewing_live: true,
            loop_playback: None,
            group: default_group(),
            time_link: true,
            viewport_link: true,
            layer_link: true,
            transport: default_transport(),
            layer_slots: SlotList::default(),
            removed_layers: SlotList::default(),
            zoom: None,
            center: None,
            kind: PaneKindConfig::Map,
            render: MapRender::Plan,
            cross_section: None,
            volume: None,
            unknown: serde_json::Map::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct UiConfig {
    /// The config format this file speaks — see [`migrate`]. Absent reads as
    /// version 1 (every file written before the field existed), through the
    /// field-level default rather than [`migrate::CONFIG_VERSION`], because
    /// "what an old file means" is a fact about history and must not move
    /// when the current version does. A version greater than this build's is
    /// not an error: the tolerant load proceeds, preservation carries what
    /// this build cannot read, and the next save writes this build's own
    /// version over a file it can honestly describe.
    #[serde(default = "migrate::first_version")]
    config_version: u32,
    pane_count: usize,
    active_pane: usize,
    /// **Read-only legacy**: the retired global viewport-sync toggle.
    #[serde(skip_serializing, default = "default_true")]
    viewport_sync: bool,
    /// **Read-only legacy**: the retired global layer-sync toggle,
    /// on the same terms as `viewport_sync`. On load, false seeds every
    /// restored pane's `layer_link` **and** `time_link` off.
    #[serde(skip_serializing, default = "default_true")]
    sync_layers: bool,
    // **The archive poll and the chunk feed's three switches used to be four
    // root keys here.** WO-E8b's v3 → v4 migration moves them into
    // `overlay_states["Radar"]`, where the layer that owns them describes
    // them through `serialize_state`/`deserialize_state` like every other
    // handler's settings. The migration is what an older file walks up
    // through; nothing reads them here any more.
    loop_lookback_secs: u64,
    loop_speed_fps: f32,
    /// **The theme the map draws in**: `"system"`, `"light"` or `"dark"`.
    ///
    /// Absent means system, which is what every config written before this
    /// existed means and what the application did unconditionally. Skipped on
    /// save while it is the default for the same reason `as_of` and
    /// `viewing_live` are: writing it into every file would move the bytes of
    /// configs that express no opinion, which
    /// `a_config_naming_an_unregistered_layer_is_written_back_byte_preserved`
    /// forbids.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    theme: String,
    time_step_secs: i64,
    /// Per-pane persistent state (product, elevation, layers).
    panes: Vec<PaneConfig>,
    /// User unit/timezone preferences. Read leniently: one field of a
    /// container this rich going unreadable must cost the preferences their
    /// customization, not the user the whole file.
    #[serde(deserialize_with = "lenient_or_default")]
    preferences: UserPreferences,
    /// Handler-owned config state (overlay kind name → serialized state).
    #[serde(default)]
    overlay_states: serde_json::Map<String, serde_json::Value>,
    /// Serial GPS configuration (port, baud). Lenient for
    /// [`Self::preferences`]' reason.
    #[serde(default, deserialize_with = "lenient_or_default")]
    serial_config: squallar_nmea_serial::SerialConfig,
    /// How the directional heading is determined.
    #[serde(default)]
    heading_source: squallar_location::HeadingSource,
    /// The user's storm-motion override — the audit's known persistence gap,
    /// closed here. `#[serde(default)]` makes an older config load as
    /// "override off, default vector", which is what those sessions were.
    #[serde(default, deserialize_with = "lenient_or_default")]
    storm_motion_override: super::StormMotionOverride,
    /// Which derived rung storm-relative velocity falls to when no override and
    /// no NWS vector applies — the Storm motion section's first control.
    #[serde(default, deserialize_with = "srv_fallback_or_default")]
    srv_fallback: squallar_radar::srv::SrvFallback,
    /// Whether the pane pill rows render at full opacity unconditionally —
    /// the Interface section's "Pin pane controls". `#[serde(default)]`
    /// loads an older config as unpinned, which is what those sessions were.
    #[serde(default)]
    pin_pane_controls: bool,
    /// **The user's starred radar sites**, bare ICAO identifiers in the order
    /// they were starred — the same spelling a pick persists, so a favourite
    /// and a current site are the one kind of value.
    ///
    /// At the root rather than on a pane: a favourite is a fact about the
    /// person, not about a window. Additive on `pin_pane_controls`' terms —
    /// `#[serde(default)]`, **no `CONFIG_VERSION` bump and no `migrate.rs`
    /// step**. Absence loads as "nothing starred", which is what every session
    /// written before this field was.
    #[serde(default)]
    favorite_sites: Vec<String>,
    /// **How the window splits between panes**, and where the user dragged the
    /// dividers. App-wide rather than per-pane: all three describe the window.
    ///
    /// Declared here rather than left to the `unknown` passthrough on purpose
    /// — a `#[serde(flatten)]` map swallows any key the struct does not name,
    /// and these would round-trip as opaque baggage nothing could read.
    ///
    /// Additive on `pin_pane_controls`' terms: `#[serde(default)]`, **no
    /// `CONFIG_VERSION` bump and no `migrate.rs` step.** Absence loads as
    /// `Auto` with the `PaneLayout::for_count` defaults, which is exactly what
    /// a session written before this field was.
    #[serde(default)]
    split_orientation: crate::pane::SplitOrientation,
    /// Row heights, one per grid row, each a fraction of the map panel.
    /// **Never trusted**: `PaneLayout::adopt_ratios` checks the arity against
    /// the grid this window actually has, the floor against `MIN_RATIO` and
    /// the sum against 1.0, and refuses the lot on any failure.
    #[serde(default)]
    row_ratios: Vec<f32>,
    /// Column widths within each row, on the same terms.
    #[serde(default)]
    col_ratios: Vec<Vec<f32>>,
    /// The user's saved presets (§3.11). Built-ins are compiled in and never
    /// written here; an older config simply has none.
    #[serde(default)]
    presets: Vec<super::PresetConfig>,
    /// The user's Volume Alpha curves, one entry per *edited* product.
    #[serde(default)]
    volume_alpha: Vec<VolumeAlphaConfig>,
    /// The user's isosurface thresholds, one entry per *edited* product —
    /// the same store-of-exceptions arrangement as `volume_alpha`, for the
    /// same reason: absence means the argued per-product default, and an old
    /// config without this field loads as "nothing edited".
    #[serde(default)]
    volume_iso: Vec<VolumeIsoConfig>,
    /// Every top-level key this build does not know, verbatim — the
    /// [`PaneConfig::unknown`] arrangement at file scope, carried in
    /// `Gui::config_unknown_fields` between load and save. What makes a
    /// downgrade safe: a newer build's settings survive a session under this
    /// one instead of being silently dropped on the first autosave.
    #[serde(flatten)]
    unknown: serde_json::Map<String, serde_json::Value>,
}

/// **Field-id renames, as a migration rather than a break.**
///
/// A row is `(old spelling, current spelling)`: a saved key on the left is read
/// as the id on the right, so renaming a field costs a row here instead of
/// silently orphaning every user's curves and thresholds for it.
///
/// **Empty today, deliberately.** No radar field has been renamed, and the
/// spellings `squallar_radar::fields` registers are byte-identical to the ones
/// the product enum used to serialize — which is the whole zero-migration
/// guarantee WO-E9c rests on. The table exists so the *next* rename is a row.
///
/// **Honesty note**: with this table empty, [`resolve_field_alias`] is the
/// identity and no test can distinguish the load path calling it from the load
/// path not calling it. What is proven is the mechanism — `resolve_field_alias`
/// is exercised against a non-empty fixture table below — and the emptiness of
/// this table, so a first row is a deliberate act by a person.
pub(crate) const FIELD_ALIASES: &[(&str, &str)] = &[];

/// Read one saved field key through `aliases`.
///
/// Single-hop on purpose: a chain would let two rows conspire into a rename
/// nobody wrote down, and a cycle would not terminate.
fn resolve_field_alias(aliases: &[(&str, &str)], id: FieldId) -> FieldId {
    match aliases.iter().find(|(old, _)| *old == id.as_str()) {
        Some((_, current)) => FieldId::new(*current),
        None => id,
    }
}

/// The order the two volume-editor tables are written to disk in.
///
/// **Byte-compatible with what it replaces**: the sort was `crate::field_facts::code(&product)`
/// and it still is for every field this build registers, so a file with no
/// unknown ids is written in exactly the same order it always was. A field
/// this build does **not** register has no code to sort by; those entries sort
/// last, by their id, so the file stays deterministic instead of depending on
/// a `HashMap`'s per-process seed.
fn save_order_key(field: &FieldId) -> (bool, String) {
    match squallar_radar::fields::spec_for(field) {
        Some(spec) => (false, spec.code.to_owned()),
        None => (true, field.as_str().to_owned()),
    }
}

/// One field's persisted isosurface threshold.
#[derive(Serialize, Deserialize)]
struct VolumeIsoConfig {
    /// **The on-disk key and its spelling are unchanged.** `FieldId` is
    /// `#[serde(transparent)]` and the radar crate registers each field under
    /// the product enum's own serde spelling, so this member reads and writes
    /// exactly the bytes it did when it was a source enum.
    ///
    /// No longer dropped when this build does not know the name: under the
    /// open-id doctrine an unrecognised id is preserved inert (it applies to
    /// nothing, because no pane can select a field the registry does not
    /// offer) and written back verbatim, which is what makes a downgrade safe.
    product: FieldId,
    /// In the field's own units. Validated finite on load —
    /// `IsoThresholds::restore` refuses non-finite values, the same door every
    /// persisted float goes through.
    threshold: f32,
}

/// One field's persisted Volume Alpha curve.
#[derive(Serialize, Deserialize)]
struct VolumeAlphaConfig {
    /// Same spelling, same key, same open-id tolerance as
    /// [`VolumeIsoConfig::product`]. A curve saved for one field is still
    /// never applied to another — an id that resolves to no field is applied
    /// to nothing at all, which is the guarantee the old drop-on-load bought.
    product: FieldId,
    /// Exactly [`crate::volume_alpha::CURVE_LEN`] alphas, entry 0 first.
    alpha: Vec<u8>,
}

/// Deserialize a pane's selected [`FieldId`], falling back to the default
/// field when this build does not register the name on disk.
///
/// **The fallback is what a *selection* needs and what the iso/alpha tables
/// deliberately do not do.** A curve saved for an unknown field is preserved
/// inert because it applies to nothing; a pane's selection has to name a field
/// this build can actually draw, or the pane has no picture at all.
///
/// **The id that comes back is always the registry's own `&'static` spelling**,
/// not the bytes off the disk: `spec_for` resolves it and its `id` is cloned,
/// so a pane's field is a borrowed `Cow` and reading it costs no allocation on
/// the frame path. `the_loaded_field_is_the_registrys_own_static_spelling`
/// is the pin.
pub(crate) fn product_or_default<'de, D>(deserializer: D) -> Result<FieldId, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let named = FieldId::deserialize(&value).ok();
    match named
        .as_ref()
        .and_then(|id| squallar_radar::fields::spec_for(id))
    {
        Some(spec) => Ok(spec.id.clone()),
        None => {
            log::warn!(
                "config names a product this build does not know ({value}); \
                 falling back to {}",
                crate::field_facts::name(&radar_fields::known::REFLECTIVITY),
            );
            Ok(radar_fields::known::REFLECTIVITY)
        }
    }
}

/// Deserialize the persisted derived-rung preference, falling back to the
/// shipped default on a name this build does not know.
pub(crate) fn srv_fallback_or_default<'de, D>(
    deserializer: D,
) -> Result<squallar_radar::srv::SrvFallback, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match squallar_radar::srv::SrvFallback::deserialize(&value) {
        Ok(fallback) => Ok(fallback),
        Err(_) => {
            let default = squallar_radar::srv::SrvFallback::default();
            log::warn!(
                "config names a storm-motion fallback this build does not know \
                 ({value}); falling back to {}",
                default.source().label(),
            );
            Ok(default)
        }
    }
}

/// Deserialize a persisted radar site, dropping one built out of bytes no
/// identifier contains.
pub(crate) fn site_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let site = String::deserialize(deserializer)?;
    if site.is_empty() || squallar_radar::sites::is_ascii_site_id(&site) {
        return Ok(site);
    }
    log::warn!("config names a site no radar could be called ({site:?}); ignoring it");
    Ok(String::new())
}

/// Deserialize a [`PaneKindConfig`], falling back to `Map` when the name is one
/// this build does not know.
fn kind_or_default<'de, D>(deserializer: D) -> Result<PaneKindConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match PaneKindConfig::deserialize(&value) {
        Ok(kind) => Ok(kind),
        Err(_) => {
            log::warn!(
                "config names a pane kind this build does not know ({value}); \
                 falling back to a map pane"
            );
            Ok(PaneKindConfig::Map)
        }
    }
}

/// Deserialize a [`MapRender`], falling back to the plan view when the name is
/// one this build does not know.
fn render_or_default<'de, D>(deserializer: D) -> Result<MapRender, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match MapRender::deserialize(&value) {
        Ok(render) => Ok(render),
        Err(_) => {
            log::warn!(
                "config names a map render mode this build does not know ({value}); \
                 falling back to the plan view"
            );
            Ok(MapRender::default())
        }
    }
}

/// Deserialize any of the rich top-level containers, falling back to its
/// default when the stored shape cannot be read — [`product_or_default`]'s
/// shape, generalised. One corrupt container must cost its own settings,
/// never the whole file.
fn lenient_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match T::deserialize(&value) {
        Ok(parsed) => Ok(parsed),
        Err(e) => {
            log::warn!(
                "a saved setting cannot be read and is reset to its default \
                 ({e}); the rest of the file is kept"
            );
            Ok(T::default())
        }
    }
}

/// Copy the stored config to [`UI_CONFIG_BACKUP_KEY`] if it predates the slot
/// shape and nothing has been copied there yet. **One-time**: the guard is
/// the backup key's own absence, so a second call — or a second session —
/// cannot overwrite the original with something this build has already
/// rewritten.
///
/// Called from both public entry points that can precede a v3 write:
/// [`super::Gui::load_ui_config`] and [`super::Gui::save_ui_config`]. The
/// frame-loop autosave in `squallar-app` is downstream of a load in every flow
/// that has one, so the load's copy is already in place by the time it runs.
pub fn back_up_pre_slot_config(store: &dyn KvStore) {
    if store.load(UI_CONFIG_BACKUP_KEY).is_some() {
        return;
    }
    let Some(content) = store.load(UI_CONFIG_KEY) else {
        return;
    };
    let version = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .as_ref()
        .and_then(|v| v.get("config_version"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| u64::from(migrate::first_version()));
    if version >= u64::from(migrate::CONFIG_VERSION) {
        return;
    }
    if let Err(e) = store.store_now(UI_CONFIG_BACKUP_KEY, &content) {
        log::warn!("could not keep a copy of the pre-slot config: {e}");
    }
}

/// The wire key a layer's state is filed under — the id string
/// ([`LayerId::as_str`]). Never `format!("{:?}")`.
fn layer_key(id: &LayerId) -> String {
    id.as_str().to_string()
}

/// **The radar slot's config on the wire**: this pane's own selection, and
/// the live-chunk switch, written over whatever else the slot carries so an
/// entry a newer build put there rides the session out.
///
/// `product` is written with the product enum's own `Serialize`, which is the
/// spelling [`product_or_default`] reads — the round trip is the same two
/// functions the flat field used, moved.
fn radar_slot_config(pane: &PaneState, global_live_chunks: bool) -> serde_json::Value {
    let mut map = match pane.slot(&known::RADAR).map(|slot| slot.config.clone()) {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    map.insert(
        "site".to_string(),
        serde_json::Value::String(pane.site().to_string()),
    );
    match serde_json::to_value(pane.selected_product()) {
        Ok(product) => {
            map.insert("product".to_string(), product);
        }
        Err(e) => log::error!("this pane's product cannot be written to its slot ({e})"),
    }
    let elevation = if pane.selected_elevation().is_finite() {
        pane.selected_elevation()
    } else {
        0.0
    };
    map.insert(
        "elevation".to_string(),
        serde_json::Value::from(f64::from(elevation)),
    );
    map.insert(
        "live_chunks".to_string(),
        serde_json::Value::Bool(pane.radar_live_chunks().unwrap_or(global_live_chunks)),
    );
    serde_json::Value::Object(map)
}

/// A pane's whole layer stack as the file carries it, in draw order, with the
/// radar slot's config replaced by [`radar_slot_config`].
fn pane_slot_list(pane: &PaneState, global_live_chunks: bool) -> SlotList {
    let mut known: Vec<SlotConfig> = pane
        .layers
        .iter()
        .map(|slot| SlotConfig {
            id: layer_key(&slot.id),
            enabled: Some(slot.enabled),
            config: if slot.id == known::RADAR {
                radar_slot_config(pane, global_live_chunks)
            } else {
                slot.config.clone()
            },
        })
        .collect();
    if !known.iter().any(|slot| slot.id == known::RADAR.as_str()) {
        // Can't happen: the load gives every pane a slot for every registered
        // layer. It is written down anyway because the alternative failure is
        // silent — a pane with no radar slot has nowhere to keep its site.
        log::warn!("a pane reached the save with no radar slot; appending one for its selection");
        known.push(SlotConfig {
            id: layer_key(&known::RADAR),
            enabled: Some(true),
            config: radar_slot_config(pane, global_live_chunks),
        });
    }
    SlotList {
        known,
        unknown: pane.config_baggage.layer_slots.clone(),
    }
}

/// A pane's tombstones as the file carries them: the id each removal names,
/// and the configuration it left with, so the next re-add restores settings
/// rather than resetting them.
///
/// `enabled` is deliberately never written: a removed layer's flag is not a
/// fact about anything, and writing one would put a second, contradictable
/// answer beside the slot list's.
fn pane_removed_list(pane: &PaneState) -> SlotList {
    SlotList {
        known: pane
            .layers
            .removed()
            .iter()
            .map(|gone| SlotConfig {
                id: layer_key(&gone.id),
                enabled: None,
                config: gone.config.clone(),
            })
            .collect(),
        unknown: pane.config_baggage.removed_layers.clone(),
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            config_version: migrate::CONFIG_VERSION,
            pane_count: 1,
            active_pane: 0,
            viewport_sync: true,
            sync_layers: true,
            loop_lookback_secs: 3600,
            loop_speed_fps: crate::pane::DEFAULT_LOOP_SPEED_FPS,
            theme: String::new(),
            time_step_secs: 600,
            panes: vec![PaneConfig::default()],
            preferences: UserPreferences::default(),
            overlay_states: serde_json::Map::new(),
            serial_config: squallar_nmea_serial::SerialConfig::default(),
            heading_source: squallar_location::HeadingSource::default(),
            storm_motion_override: super::StormMotionOverride::default(),
            srv_fallback: squallar_radar::srv::SrvFallback::default(),
            pin_pane_controls: false,
            favorite_sites: Vec::new(),
            split_orientation: crate::pane::SplitOrientation::Auto,
            // Empty rather than the one-pane run: the fields' absence is what
            // says "no dividers were described", and `adopt_ratios` refuses an
            // empty run against any grid, which is the same outcome.
            row_ratios: Vec::new(),
            col_ratios: Vec::new(),
            presets: Vec::new(),
            volume_alpha: Vec::new(),
            volume_iso: Vec::new(),
            unknown: serde_json::Map::new(),
        }
    }
}

impl super::Gui {
    /// Save UI layout configuration to `store`, waiting for the write.
    pub fn save_ui_config(&self, store: &dyn KvStore) {
        let Some(json) = self.ui_config_json() else {
            return;
        };
        back_up_pre_slot_config(store);
        if let Err(e) = store.store_now(UI_CONFIG_KEY, &json) {
            log::error!("Failed to write config: {}", e);
        }
    }

    /// The configuration this `Gui` would persist, as JSON.
    pub fn ui_config_json(&self) -> Option<String> {
        let fps = if self.loop_speed_fps.is_finite() {
            self.loop_speed_fps
        } else {
            5.0
        };
        // The layer's own answer, read once: a pane that carries no copy of
        // the switch is saved with this one.
        let global_live_chunks = crate::radar_layer::live_chunks_default(self);
        let pane_configs: Vec<PaneConfig> = self
            .panes
            .iter()
            .map(|pane| {
                let (kind, render, cross_section, volume) = content_config(pane);
                PaneConfig {
                    kind,
                    render,
                    cross_section,
                    volume,
                    layers: BTreeMap::new(),
                    spc_day: OutlookDay::Day1,
                    time_step_secs: pane.time.step.as_secs(),
                    viewing_live: pane.viewing_live,
                    loop_playback: loop_playback_of(pane),
                    as_of: match pane.time.mode {
                        crate::pane::TimeMode::Live => None,
                        crate::pane::TimeMode::AsOf(at) => {
                            Some(at.format(AS_OF_FORMAT).to_string())
                        }
                    },
                    group: pane.group.map(crate::pane::GroupId::index),
                    time_link: pane.time_link,
                    viewport_link: pane.viewport_link,
                    layer_link: pane.layer_link,
                    transport: pane.transport_layer().clone(),
                    layer_slots: pane_slot_list(pane, global_live_chunks),
                    removed_layers: pane_removed_list(pane),
                    unknown: pane.config_baggage.fields.clone(),
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
            config_version: migrate::CONFIG_VERSION,
            pane_count: self.pane_layout.pane_count,
            active_pane: self.active_pane,
            viewport_sync: true,
            sync_layers: true,
            loop_lookback_secs: self.loop_lookback_secs,
            loop_speed_fps: fps,
            theme: match self.theme {
                crate::pane::ThemeChoice::System => String::new(),
                other => other.as_str().to_owned(),
            },
            time_step_secs: self.panes.first().map_or(600, |p| p.time.step.as_secs()),
            panes: pane_configs,
            preferences: self.preferences.clone(),
            // The handlers' live state written OVER the carried entries no
            // handler consumed at load: a kind this build serves is its
            // handler's to describe, and one it does not is handed back
            // exactly as it arrived — the overlay_states half of the
            // downgrade-safety story.
            overlay_states: {
                let mut states = self.overlay_states_baggage.clone();
                for (name, state) in self.overlays.serialize_handler_states() {
                    states.insert(name, state);
                }
                states
            },
            serial_config: self.serial_config.clone(),
            heading_source: self.heading_source,
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
            srv_fallback: self.srv_fallback,
            pin_pane_controls: self.pin_pane_controls,
            favorite_sites: self.favorite_sites.clone(),
            split_orientation: self.split_orientation,
            row_ratios: {
                let (rows, _) = self.pane_layout.ratios();
                rows.to_vec()
            },
            col_ratios: {
                let (_, cols) = self.pane_layout.ratios();
                cols.to_vec()
            },
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
                            product: pane.product.clone(),
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
                let mut curves: Vec<VolumeAlphaConfig> = self
                    .volume_alpha
                    .entries()
                    .map(|(field, curve)| VolumeAlphaConfig {
                        product: field.clone(),
                        alpha: curve.alphas().to_vec(),
                    })
                    .collect();
                curves.sort_by_cached_key(|c| save_order_key(&c.product));
                curves
            },
            volume_iso: {
                let mut thresholds: Vec<VolumeIsoConfig> = self
                    .volume_iso
                    .entries()
                    .map(|(field, threshold)| VolumeIsoConfig {
                        product: field.clone(),
                        threshold,
                    })
                    .collect();
                thresholds.sort_by_cached_key(|c| save_order_key(&c.product));
                thresholds
            },
            unknown: self.config_unknown_fields.clone(),
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
    pub fn load_ui_config(&mut self, store: &dyn KvStore) -> bool {
        back_up_pre_slot_config(store);
        let Some(content) = store.load(UI_CONFIG_KEY) else {
            return false;
        };
        let mut value = match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("Failed to parse config: {}", e);
                return false;
            }
        };
        migrate::migrate_to_current(&mut value);
        sanitize_config_tree(&mut value);
        let config = match UiConfig::deserialize(&value) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Failed to read config: {}", e);
                return false;
            }
        };

        let count = config.pane_count.clamp(1, WidthClass::max_panes_absolute());
        // **A pane the file COUNTED BUT NEVER DESCRIBED opens on the first
        // pane the file does name.** The seed used to be the top-level `site`
        // key; there is no app-wide site any more, and the nearest honest
        // substitute for a pane the file says nothing at all about is the
        // file's own first answer rather than a compiled-in default the user
        // never chose.
        //
        // **Only that shape.** A pane whose entry names a site this build
        // refuses — a hand-edited identifier no radar could have, or a pane
        // the salvage reset to defaults in place — is not a gap: the file had
        // an opinion about it and the opinion was rejected. Those keep
        // whatever startup picked, exactly as they did when the refused value
        // left the global empty too. Borrowing a neighbour's radar there
        // would invent a choice the user never made.
        //
        // No save this build makes reaches the seed at all: every pane is
        // written, so `panes` is never shorter than `pane_count`.
        let described_site = config
            .panes
            .iter()
            .map(PaneConfig::site)
            .find(|site| !site.is_empty());
        while self.panes.len() < count {
            let pane = match config.panes.get(self.panes.len()) {
                // Described. Its own answer, and where the description named
                // no site, a fresh pane's — which is exactly what a described
                // pane already in the vector comes to, since the loop below
                // leaves one that named none alone.
                Some(pc) => match pc.site() {
                    site if site.is_empty() => PaneState::new(),
                    site => PaneState::with_site(site),
                },
                // Counted but never described: the gap.
                None => match described_site.clone() {
                    Some(site) => PaneState::with_site(site),
                    None => PaneState::new(),
                },
            };
            self.panes.push(pane);
        }
        // **Before the layout is built, because it is one of its two inputs.**
        self.split_orientation = config.split_orientation;
        self.pane_layout = PaneLayout::for_count(count, self.layout.width, self.split_orientation);
        // **Two attempts, one validating function, and the second is the one
        // that counts.** A load happens before any frame has run, so
        // `self.layout.width` here is still the default rather than this
        // window's — the grid built above may not be the grid the first frame
        // settles on. The attempt below is what makes a headless load (a test,
        // a save straight after a load) see its own dividers; the stash is
        // what lets `Gui::settle_pane_layout` re-offer them against the real
        // grid one frame later. Both refuse a file that does not describe the
        // grid it is offered against, so neither can install a bad ratio.
        self.pane_layout
            .adopt_ratios(&config.row_ratios, &config.col_ratios);
        self.restored_ratios = Some((config.row_ratios, config.col_ratios));
        self.active_pane = if config.active_pane < count {
            config.active_pane
        } else {
            0
        };

        // The file's numbers are the pane's numbers: the two settings are
        // persisted once and every pane's posture carries the same value, so a
        // pane's own reads and the global cannot answer differently.
        self.set_loop_span_secs(config.loop_lookback_secs);
        self.set_loop_speed_fps(config.loop_speed_fps);
        self.theme = crate::pane::ThemeChoice::parse(&config.theme);
        self.preferences = config.preferences;
        self.serial_config = config.serial_config;
        self.heading_source = config.heading_source;
        self.storm_motion_override = config.storm_motion_override;
        self.srv_fallback = config.srv_fallback;
        self.pin_pane_controls = config.pin_pane_controls;
        self.favorite_sites = config.favorite_sites;
        self.presets = config.presets;

        self.volume_alpha = crate::volume_alpha::AlphaCurves::default();
        for entry in config.volume_alpha {
            let Ok(alphas) = <[u8; crate::volume_alpha::CURVE_LEN]>::try_from(entry.alpha) else {
                log::warn!(
                    "the saved Volume Alpha curve for {} is not {} entries; dropping it",
                    entry.product,
                    crate::volume_alpha::CURVE_LEN,
                );
                continue;
            };
            self.volume_alpha.set(
                &resolve_field_alias(FIELD_ALIASES, entry.product),
                crate::volume_alpha::AlphaCurve::from_alphas(alphas),
            );
        }

        self.volume_iso = crate::volume_iso::IsoThresholds::default();
        for entry in config.volume_iso {
            self.volume_iso.restore(
                resolve_field_alias(FIELD_ALIASES, entry.product),
                entry.threshold,
            );
        }

        // **The handlers' own state is restored FIRST**, ahead of the pane
        // loop, because a slot that states no `enabled` is resolved from the
        // handler and the answer has to be the one this file asked for. The
        // two blocks are independent — the pane loop never touches the
        // registry and this one never touches a pane — so the order is free
        // to be the one that makes the resolution honest.
        let handler_keys: std::collections::HashSet<String> = self
            .overlays
            .handlers()
            .map(|h| layer_key(&h.id()))
            .collect();
        self.overlay_states_baggage = config
            .overlay_states
            .iter()
            .filter(|(name, _)| !handler_keys.contains(name.as_str()))
            .map(|(name, state)| (name.clone(), state.clone()))
            .collect();

        if !config.overlay_states.is_empty() {
            self.overlays
                .deserialize_handler_states(&config.overlay_states);
        }
        // **Independent of the block above, and it has to be** (WO-E8b). The
        // v0 toggle map's Radar entry is the only thing that carries that
        // era's radar switch, and no handler's state blob has ever held it —
        // radar's `enabled` lives in the pane's slot, not in its
        // `serialize_state`. It used to be reached through an `else`, which
        // was safe only while a v0 file had no `overlay_states` at all; the
        // v3 → v4 key move gives every file one, so the `else` would have
        // retired this migration silently. A file this build wrote answers
        // `None` here — the save writes `layers` as an empty object — so the
        // two cannot fight.
        if let Some(enabled) = config
            .panes
            .iter()
            .find_map(|pc| pc.layers.get("Radar").copied())
        {
            self.overlays
                .set_enabled(&known::RADAR, enabled, &mut PaneMut::bare(0));
        }

        let mut zoom_restored = false;
        for (i, pane) in self.panes.iter_mut().enumerate().take(count) {
            let pc = config.panes.get(i);
            let Some(pc) = pc else {
                pane.time.step = crate::pane::TimeStep::from_secs(config.time_step_secs);
                pane.group = Some(crate::pane::GroupId::FIRST);
                pane.viewport_link = config.viewport_sync;
                pane.layer_link = config.sync_layers;
                pane.time_link = config.sync_layers;
                pane.config_baggage = crate::pane::PaneConfigBaggage::default();
                continue;
            };
            pane.set_selected_product(pc.selected_product());
            pane.set_selected_elevation(pc.selected_elevation());
            let pane_site = pc.site();
            if !pane_site.is_empty() {
                pane.set_site(pane_site);
            }
            pane.time.step = crate::pane::TimeStep::from_secs(pc.time_step_secs);
            pane.viewing_live = pc.viewing_live;
            // A request for the app to act on, not a state to assume: arming a
            // loop needs a listing dispatch this crate cannot make.
            pane.loop_arm_pending = loop_arm_from_config(pc.loop_playback.as_deref());
            // `set_time_mode`, not a bare field write: it settles every layer's
            // playhead onto the restored clock. Assigning the field left the
            // playheads where `Gui::new` put them, so the reload path had to
            // re-select the instant later purely to settle them -- and that
            // re-selection dispatched an overlay refetch for every layer,
            // including Radar, which fetches out of band and can only answer
            // "no fetch task could be built". The user saw that as an error
            // toast on a pane whose volume had loaded perfectly.
            pane.set_time_mode(
                pc.as_of
                    .as_deref()
                    .and_then(parse_as_of)
                    .map_or(crate::pane::TimeMode::Live, crate::pane::TimeMode::AsOf),
            );
            // **Downstream of the legacy fold, not a replacement for it.** The
            // retired globals still seed the flags below; the group only says
            // which panes those flags are scoped to, and a file that names no
            // group says "all of them" through `default_group`. An index no
            // layout can reach reads as no group rather than as a panic.
            pane.group = pc.group.and_then(crate::pane::GroupId::from_index);
            pane.time_link = pc.time_link && config.sync_layers;
            pane.viewport_link = pc.viewport_link && config.viewport_sync;
            pane.layer_link = pc.layer_link && config.sync_layers;
            pane.set_transport_layer(pc.transport.clone());
            pane.set_content(restore_content(i, pc, count));
            pane.config_baggage = crate::pane::PaneConfigBaggage {
                layer_slots: pc.layer_slots.unknown.clone(),
                removed_layers: pc.removed_layers.unknown.clone(),
                fields: pc.unknown.clone(),
            };
            // The tombstones first: a file that names the same id in both lists
            // is a file that contradicts itself, and the slot list wins — a
            // layer visibly in the stack is not removed from it — which is what
            // `LayerStack::set_slots` enforces below.
            let removed: Vec<crate::pane::RemovedLayer> = pc
                .removed_layers
                .known
                .iter()
                .map(|slot| crate::pane::RemovedLayer {
                    id: LayerId::new(slot.id.clone()),
                    config: slot.config.clone(),
                })
                .collect();
            let slots: Vec<crate::pane::LayerSlot> = pc
                .layer_slots
                .known
                .iter()
                .map(|slot| {
                    let id = LayerId::new(slot.id.clone());
                    let handler = self.overlays.handler_by_id(&id);
                    crate::pane::LayerSlot {
                        // Absent → whatever the handler says now, which is
                        // what an absent `enabled_overlays` entry has always
                        // resolved to. Unknown id → false: nothing draws it.
                        enabled: slot.enabled.unwrap_or_else(|| {
                            handler.is_some_and(|h| h.is_enabled(&PaneRef::bare(i)))
                        }),
                        config: slot.config.clone(),
                        id,
                        // Derived from `config` at the next hydrate, never
                        // read off the wire.
                        state: None,
                        // A timeline is a live position, and a file names
                        // none: this pane starts wherever a fresh one does.
                        time: crate::pane::LayerTimeState::new(),
                    }
                })
                .collect();
            pane.layers = crate::pane::LayerStack::from_parts(Vec::new(), removed);
            pane.layers.set_slots(slots);
            let unregistered: Vec<&str> = pane
                .layers
                .iter()
                .filter(|slot| self.overlays.handler_by_id(&slot.id).is_none())
                .map(|slot| slot.id.as_str())
                .collect();
            if !unregistered.is_empty() {
                log::warn!(
                    "pane {i}'s config names layer id(s) no handler serves \
                     ({}); keeping them for the build that does",
                    unregistered.join(", "),
                );
            }
            zoom_restored |= restore_viewport(pane, pc);
        }

        if zoom_restored {
            self.initial_zoom_set = true;
        }

        self.config_unknown_fields = config.unknown;

        self.initialize_pane_enabled();
        for pane in &mut self.panes {
            pane.release_disabled_overlay_textures();
        }
        true
    }

    /// Point every pane at `site`, for a first run with no stored config.
    /// **Every pane, and nothing else** — there is no app-wide site for this
    /// to write beside them.
    pub fn set_initial_site(&mut self, site: &str) {
        for pane in &mut self.panes {
            pane.set_site(site.to_string());
        }
    }
}

/// What a pane's kind and per-kind state should be persisted as.
fn content_config(
    pane: &PaneState,
) -> (
    PaneKindConfig,
    MapRender,
    Option<CrossSectionConfig>,
    Option<VolumeConfig>,
) {
    /// What a pane that could not be saved as itself is written as: a plain
    /// plan-view map with no sub-config.
    const AS_MAP: (
        PaneKindConfig,
        MapRender,
        Option<CrossSectionConfig>,
        Option<VolumeConfig>,
    ) = (PaneKindConfig::Map, MapRender::Plan, None, None);

    match &pane.content {
        PaneContent::Map(map) => {
            let camera = map.volume.camera;
            let config = VolumeConfig {
                yaw_deg: camera.yaw_deg(),
                pitch_deg: camera.pitch_deg(),
                eye_distance: camera.eye_distance(),
                pivot: camera.pivot(),
                vertical_exaggeration: camera.vertical_exaggeration(),
                hide_floor: map.volume.hide_floor,
                view_mode: map.volume.view_mode,
                region: map.volume.region.map(|region| VolumeRegionConfig {
                    centre_lat: region.centre().lat,
                    centre_lon: region.centre().lon,
                    half_east_km: region.half_east_km(),
                    half_north_km: region.half_north_km(),
                }),
                source_pane: map.volume.source_pane,
            };
            if !config.yaw_deg.is_finite()
                || !config.pitch_deg.is_finite()
                || !config.eye_distance.is_finite()
                || !config.pivot.iter().all(|p| p.is_finite())
                || !config.vertical_exaggeration.is_finite()
                || !config.region.as_ref().is_none_or(|region| {
                    region.centre_lat.is_finite()
                        && region.centre_lon.is_finite()
                        && region.half_east_km.is_finite()
                        && region.half_north_km.is_finite()
                })
            {
                log::warn!("a map pane's 3D camera is not finite; saving it as a plain map");
                return AS_MAP;
            }
            let volume = (config.differs_from_default()).then_some(config);
            (PaneKindConfig::Map, map.render, None, volume)
        }
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
                return AS_MAP;
            }
            (
                PaneKindConfig::CrossSection,
                MapRender::Plan,
                Some(CrossSectionConfig {
                    line,
                    source_pane: section.source_pane,
                }),
                None,
            )
        }
    }
}

/// Restore a map pane in `render`, with whatever 3D state the file carries.
fn restore_map(
    pane_idx: usize,
    pc: &PaneConfig,
    render: MapRender,
    pane_count: usize,
) -> PaneContent {
    let Some(saved) = pc.volume.as_ref() else {
        return PaneContent::Map(Box::new(MapPane {
            render,
            volume: VolumePane::default(),
        }));
    };
    let Some(camera) = OrbitCamera::restore(
        saved.yaw_deg,
        saved.pitch_deg,
        saved.eye_distance,
        saved.pivot,
        saved.vertical_exaggeration,
    ) else {
        log::warn!(
            "pane {pane_idx}'s saved 3D camera is not finite; loading it as a plain plan view"
        );
        return PaneContent::default();
    };
    PaneContent::Map(Box::new(MapPane {
        render,
        volume: VolumePane {
            camera,
            region: saved.region.as_ref().and_then(VolumeRegionConfig::restore),
            source_pane: saved.source_pane.filter(|idx| {
                let inside = *idx < pane_count;
                if !inside {
                    log::warn!(
                        "pane {pane_idx}'s 3D region was picked on pane {idx}, which this \
                         layout does not have; forgetting where it came from"
                    );
                }
                inside
            }),
            rendered_for: None,
            hide_floor: saved.hide_floor,
            alpha_editor_open: false,
            view_mode: saved.view_mode,
        },
    }))
}

/// The pane content a saved [`PaneConfig`] describes, or `Map` where it describes
/// nothing usable.
fn restore_content(pane_idx: usize, pc: &PaneConfig, pane_count: usize) -> PaneContent {
    match pc.kind {
        PaneKindConfig::Map => restore_map(pane_idx, pc, pc.render, pane_count),
        PaneKindConfig::Volume => restore_map(pane_idx, pc, MapRender::Volume, pane_count),
        PaneKindConfig::CrossSection => {
            let Some(section) = pc.cross_section.as_ref() else {
                log::warn!(
                    "pane {pane_idx} is a cross-section with no section state; loading it as a map"
                );
                return PaneContent::default();
            };
            let line = match section.line.as_ref() {
                None => None,
                Some(saved) => {
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
                        return PaneContent::default();
                    }
                    restored
                }
            };
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
                ..Default::default()
            }))
        }
    }
}

/// Put a pane's map back where it was left: same zoom, same centre.
fn restore_viewport(pane: &mut PaneState, pc: &PaneConfig) -> bool {
    let mut zoom_restored = false;
    if let Some(zoom) = pc.zoom {
        if pane.map_memory.set_zoom(zoom).is_err() {
            log::warn!("saved zoom {zoom} is out of range; keeping the default");
        } else {
            zoom_restored = true;
        }
    }
    if let Some((lat, lon)) = pc.center {
        pane.map_memory.center_at(walkers::lat_lon(lat, lon));
    }
    zoom_restored
}

/// Repair the raw config tree unit by unit, so `UiConfig::deserialize`
/// cannot fail on anything short of a root that is not a config at all.
fn sanitize_config_tree(value: &mut serde_json::Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    if root.get("config_version").is_some_and(|v| !v.is_u64()) {
        log::warn!("the saved config_version is not a version; reading the file as the oldest");
        root.remove("config_version");
    }
    match root.get_mut("panes") {
        None => {}
        Some(serde_json::Value::Array(panes)) => {
            for (i, pane) in panes.iter_mut().enumerate() {
                if let Err(e) = PaneConfig::deserialize(&*pane) {
                    log::warn!(
                        "pane {i}'s saved config cannot be read ({e}); \
                         restoring that pane to defaults in place"
                    );
                    *pane = serde_json::Value::Object(serde_json::Map::new());
                }
            }
        }
        Some(_) => {
            log::warn!("the saved pane list is not a list; dropping it");
            root.remove("panes");
        }
    }
    match root.get_mut("presets") {
        None => {}
        Some(serde_json::Value::Array(presets)) => {
            presets.retain(|preset| match super::PresetConfig::deserialize(preset) {
                Ok(_) => true,
                Err(e) => {
                    log::warn!("dropping a saved preset that cannot be read: {e}");
                    false
                }
            });
        }
        Some(_) => {
            log::warn!("the saved preset list is not a list; dropping it");
            root.remove("presets");
        }
    }
    if root.get("overlay_states").is_some_and(|v| !v.is_object()) {
        log::warn!("the saved overlay states are not a map; dropping them");
        root.remove("overlay_states");
    }
}

#[path = "ui_config/theme_config_tests.rs"]
#[cfg(test)]
mod theme_config_tests;

#[path = "ui_config/loop_persistence_tests.rs"]
#[cfg(test)]
mod loop_persistence_tests;

#[path = "ui_config/live_chunks_config_tests.rs"]
#[cfg(test)]
mod live_chunks_config_tests;

#[path = "ui_config/notifier_config_tests.rs"]
#[cfg(test)]
mod notifier_config_tests;

#[path = "ui_config/storm_motion_config_tests.rs"]
#[cfg(test)]
mod storm_motion_config_tests;

#[path = "ui_config/as_of_config_tests.rs"]
#[cfg(test)]
mod as_of_config_tests;

/// The split preference and the dragged dividers, across a restart.
#[path = "ui_config/pane_layout_config_tests.rs"]
#[cfg(test)]
mod pane_layout_config_tests;

#[path = "ui_config/presets_config_tests.rs"]
#[cfg(test)]
mod presets_config_tests;

#[path = "ui_config/fixture_tests.rs"]
#[cfg(test)]
mod fixture_tests;

/// The browser rig's own scene seed, parsed by the config it is written for:
/// the site survives the migration chain and the layer it switches on really
/// rasterizes to a texture.
#[path = "ui_config/rig_seed_tests.rs"]
#[cfg(test)]
mod rig_seed_tests;

#[path = "ui_config/tests.rs"]
#[cfg(test)]
mod tests;
