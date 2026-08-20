use crate::overlay_cache::OverlayTextureCache;
use chrono::NaiveDateTime;
use rustdar_device_profile::budget::MAX_PANES_DESKTOP;
use rustdar_radar::hover::HoverSource;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::{RadarProduct, RenderView, ScanInfo};
use rustdar_source::id::{LayerId, known};
use std::collections::HashMap;
use std::sync::Arc;
use walkers::MapMemory;

#[path = "pane_content.rs"]
mod content;

pub use content::{
    BASE_HALF_WIDTH_KM, CrossSectionPane, DEFAULT_VERTICAL_EXAGGERATION, MAX_EYE_DISTANCE,
    MAX_VERTICAL_EXAGGERATION, MIN_EYE_DISTANCE, MIN_VERTICAL_EXAGGERATION, MapPane, MapRender,
    OrbitCamera, OrbitDelta, PaneContent, PaneKind, SectionLine, SectionTarget, SectionUnavailable,
    VolumePane, VolumeRegion, VolumeStamp, VolumeTarget, VolumeViewMode, box_size_km,
    resolution_km,
};

const DEFAULT_PANE_ZOOM: f64 = 4.0;

pub type PaneId = usize;

#[derive(Clone)]
pub struct RadarImageData {
    pub texture: egui::TextureHandle,
    pub lat: f64,
    pub lon: f64,
    /// The half-width this frame was projected at, km — what the renderer
    /// handed back. The range ring is drawn on it.
    pub max_range_km: f64,
    pub placed: rustdar_geo::PlacedRaster,
    pub hover: Arc<HoverSource>,
    /// Where the cut this frame was drawn from declared its velocity folds,
    /// m/s, or `None` for a frame no single cut is behind.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer this frame was classified against came from, or
    /// `None` for a frame that classified nothing.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where the storm motion vector this frame was shifted by came from, or
    /// `None` for a frame that shifted nothing.
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
}

/// Holds a rendered cross-section raster and the little that has to travel with
/// it for the pane to draw honest axes over it.
#[derive(Clone)]
pub struct SectionImageData {
    pub texture: egui::TextureHandle,
    pub axes: rustdar_radar::xsect::SectionAxes,
    pub tilt_elevations_deg: Vec<f64>,
    /// When each of those rungs was flown, milliseconds since the Unix epoch,
    /// in the same order — `CrossSection::tilt_collected_ms`.
    pub tilt_collected_ms: Vec<i64>,
    /// The fingerprint of the tilt ladder this frame was cut from, from
    /// [`rustdar_radar::sampler::ladder_fingerprint`].
    pub ladder: u64,
}

/// A finished loop frame's picture, in whichever shape its pane draws.
#[derive(Clone)]
pub enum LoopFrameImage {
    PlanView(RadarImageData),
    /// A `SECTION_WIDTH × SECTION_HEIGHT` vertical slice along the loop's line.
    Section(SectionImageData),
    /// A **resident voxel grid**, named rather than held.
    Volume(VolumeFrameGrid),
}

/// The resident grid one 3D loop frame marches, named by what built it.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeFrameGrid {
    /// The store id of the resident grid. Unique per build; the GPU side keeps
    /// exactly the ids the store is still holding.
    pub id: u64,
    pub target: VolumeTarget,
}

impl LoopFrameImage {
    /// Which view this picture is, so a consumer that holds one can be checked
    /// against the loop it is about to be placed in.
    pub fn view(&self) -> RenderView {
        match self {
            Self::PlanView(_) => RenderView::PlanView,
            Self::Section(_) => RenderView::CrossSection,
            Self::Volume(_) => RenderView::Volume,
        }
    }

    pub fn plan_view(&self) -> Option<&RadarImageData> {
        match self {
            Self::PlanView(image) => Some(image),
            Self::Section(_) | Self::Volume(_) => None,
        }
    }

    pub fn section(&self) -> Option<&SectionImageData> {
        match self {
            Self::Section(image) => Some(image),
            Self::PlanView(_) | Self::Volume(_) => None,
        }
    }

    pub fn volume(&self) -> Option<&VolumeFrameGrid> {
        match self {
            Self::Volume(grid) => Some(grid),
            Self::PlanView(_) | Self::Section(_) => None,
        }
    }
}

/// A single rendered frame in a radar loop.
pub struct LoopFrame {
    pub timestamp: NaiveDateTime,
    pub image: Option<LoopFrameImage>,
    pub render_in_flight: bool,
    /// True once a render for this frame has been attempted and produced nothing
    /// (no matching sweep for the selected product/elevation, or the render itself
    /// failed). Terminal for this frame's current scan data.
    pub render_failed: bool,
}

/// Which entries of a listing of `total` scans a loop that may hold `held`
/// frames keeps, oldest-first — or `None` when the listing fits and every scan
/// becomes a frame.
pub fn listing_sample_indices(total: usize, held: usize) -> Option<Vec<usize>> {
    if total <= held {
        return None;
    }
    Some(
        (0..held)
            .map(|i| i * (total - 1) / (held - 1).max(1))
            .collect(),
    )
}

/// Tolerance for comparing two selected elevation angles. Shared with the render
/// dispatcher, which uses it when deciding whether two panes' selections are the
/// same and whether a queued render already covers a frame.
pub const ELEVATION_TOLERANCE: f32 = 0.01;

/// Every input `render_radar_to_image` is given *except the scan itself*: the radar
/// site whose coordinates set the projection, and the product/elevation selection
/// that picks the sweep out of that scan.
#[derive(Clone, Debug)]
pub struct RenderTarget {
    /// NEXRAD site code supplying the projection geometry (e.g. "KTLX").
    pub site: String,
    pub product: RadarProduct,
    /// The pane's *selected* elevation, not the per-scan snapped sweep angle.
    pub elevation: f32,
}

impl RenderTarget {
    pub fn new(site: impl Into<String>, product: RadarProduct, elevation: f32) -> Self {
        Self {
            site: site.into(),
            product,
            elevation,
        }
    }

    /// Whether this target names the same image as `site`/`product`/`elevation`.
    /// Site and product are exact; elevation is compared within
    /// `ELEVATION_TOLERANCE`, since the selection is an `f32` that round-trips
    /// through the UI and the scan's own sweep angles.
    pub fn matches_parts(&self, site: &str, product: RadarProduct, elevation: f32) -> bool {
        self.site == site
            && self.product == product
            && (self.elevation - elevation).abs() <= ELEVATION_TOLERANCE
    }

    pub fn matches(&self, other: &RenderTarget) -> bool {
        self.matches_parts(&other.site, other.product, other.elevation)
    }
}

/// What a **section** loop's frames were cut for, beyond what
/// [`RenderTarget`] already says.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionLoopKey {
    pub line: SectionLine,
    /// The storm motion vector the frames were derived with, as raw bits, and
    /// `None` for every product that does not read one.
    pub storm_motion: Option<(u32, u32)>,
    /// Which derived rung the frames fell to when there was no override and no
    /// RPG vector — the reader's `Settings > Storm motion` choice.
    pub srv_fallback: rustdar_radar::srv::SrvFallback,
}

impl SectionLoopKey {
    /// The key for `line` under the storm motion vector `motion` and derived
    /// rung `fallback`, in the same `(speed_kt, direction_from_deg)` form the
    /// extraction is handed.
    pub fn new(
        line: SectionLine,
        motion: Option<(f32, f32)>,
        fallback: rustdar_radar::srv::SrvFallback,
    ) -> Self {
        Self {
            line,
            storm_motion: motion.map(|(speed, direction)| (speed.to_bits(), direction.to_bits())),
            srv_fallback: fallback,
        }
    }
}

/// The 3D counterpart of [`SectionLoopKey`]: what a resident voxel grid depends
/// on that a [`RenderTarget`] cannot say.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeLoopKey {
    /// The ground every frame is resampled over, or `None` for the default box
    /// about the site.
    pub region: Option<VolumeRegion>,
    /// The storm motion vector the grids were derived with, as raw bits, and
    /// `None` for every product that does not read one.
    pub storm_motion: Option<(u32, u32)>,
    /// Which derived rung the grids fell to; both derived rungs leave
    /// `storm_motion` `None`, so nothing else in this key moves.
    pub srv_fallback: rustdar_radar::srv::SrvFallback,
}

impl VolumeLoopKey {
    /// The key for `region` under the storm motion vector `motion` and derived
    /// rung `fallback`, in the same `(speed_kt, direction_from_deg)` form the
    /// extraction is handed.
    pub fn new(
        region: Option<VolumeRegion>,
        motion: Option<(f32, f32)>,
        fallback: rustdar_radar::srv::SrvFallback,
    ) -> Self {
        Self {
            region,
            storm_motion: motion.map(|(speed, direction)| (speed.to_bits(), direction.to_bits())),
            srv_fallback: fallback,
        }
    }
}

/// The view-specific half of a loop's render key.
#[derive(Clone, Debug, PartialEq)]
pub enum LoopViewKey {
    Section(SectionLoopKey),
    Volume(VolumeLoopKey),
}

/// The two sweep angles a sibling broadcast has to reconcile.
#[derive(Clone, Copy, Debug)]
pub struct BroadcastSweep {
    pub rendered: f32,
    /// The sweep the receiving loop's *own* scan for this frame resolves the same
    /// selection to, or `None` if it has no scan for the frame yet (or that scan
    /// carries no sweep for the product). `None` refuses the image.
    pub own: Option<f32>,
}

impl BroadcastSweep {
    /// Whether the incoming image depicts the sweep this loop would have rendered.
    /// Compared within [`ELEVATION_TOLERANCE`], as every other angle comparison is.
    pub fn agrees(&self) -> bool {
        self.own
            .is_some_and(|own| (own - self.rendered).abs() <= ELEVATION_TOLERANCE)
    }
}

/// The state phases for a radar loop playback instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopPhase {
    Inactive,
    FetchingScanList,
    /// Scans listed and downloads/renders started, waiting to reach render budget.
    Rendering,
    /// Sufficient frames have rendered to allow playback, but playing is not started.
    Ready,
    Playing,
    Paused,
}

pub struct LoopPlaybackState {
    pub phase: LoopPhase,
    pub current_frame: usize,
    pub frames: Vec<LoopFrame>,
    pub lookback_secs: u64,
    /// Whether the listing this loop was built from had to be **sampled** to fit
    /// the frame cap — `Some(true)` when scans were dropped, `Some(false)` when
    /// every scan in the window became a frame, `None` before a listing has been
    /// accepted at all.
    pub listing_sampled: Option<bool>,
    /// The site's own scan cadence over this loop's window, in seconds: the median
    /// gap between consecutive scans in the listing the loop was built from,
    /// measured **before** any sampling. `None` until a listing has been accepted.
    /// Measured cadences: TDWR 360 s (VCP 80 and 90), WSR-88D precip (VCP
    /// 212/215) 259 s, clear-air (VCP 35) 517 s. A median, not a mean: a site
    /// that changes VCP mid-window mixes two cadences.
    pub scan_step_secs: Option<u32>,
    pub last_advance: Option<web_time::Instant>,
    /// When this loop entered [`LoopPhase::FetchingScanList`], or `None` for a
    /// loop that was never built ([`Self::new`]).
    pub listing_since: Option<web_time::Instant>,
    /// NEXRAD site code the loop's geometry belongs to, captured at loop creation
    /// from the same lookup as `site_lat`/`site_lon` — not the pane's live `site`.
    pub site: String,
    pub site_lat: f64,
    pub site_lon: f64,
    /// The [`RenderTarget`] every frame's render state was produced for, or `None`
    /// before the first dispatch. When the pane's selection moves both the
    /// texture and the `render_failed` flag are stale; see `retarget_renders`.
    pub rendered_for: Option<RenderTarget>,
    /// Which kind of picture this loop's frames are, fixed for the life of the
    /// state by the pane kind that started it.
    pub view: RenderView,
    /// The view-specific half of the render key, or `None` for a plan-view
    /// loop.
    pub view_key: Option<LoopViewKey>,
}

/// Config-file content addressed to a build that is not this one, carried
/// between load and save so the file survives a session under this build.
#[derive(Clone, Debug, Default)]
pub struct PaneConfigBaggage {
    /// `layer_slots` entries that are not slot objects at all, verbatim,
    /// re-appended after the slots on save.
    pub layer_slots: Vec<serde_json::Value>,
    /// Whole pane-level fields this build does not know.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// **One layer, in one pane: its identity, whether it draws, and its saved
/// configuration — the three facts that used to live in three parallel
/// containers.**
///
/// A pane's slots are an ordered list and **the vector's order IS the draw
/// order**, bottom to top. That is the whole reason the three collections
/// merged: `draw_order`, `enabled_overlays` and `overlay_configs` were keyed
/// on the same [`LayerId`] and had to be kept in step by hand, and a
/// transform that dropped an id from one of them left the other two saying
/// something the user could not see.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerSlot {
    /// Which layer this slot is. Open — an id no handler in this build serves
    /// is a layer a newer build has, and it keeps its slot and its position.
    pub id: LayerId,
    /// Whether this pane draws the layer.
    pub enabled: bool,
    /// The layer's saved configuration, as JSON. `null` means "nothing saved"
    /// — a handler with a `null` slot keeps whatever state it already has,
    /// which is exactly what an absent map entry meant before.
    pub config: serde_json::Value,
}

impl LayerSlot {
    /// A slot for `id` with nothing configured.
    pub fn new(id: LayerId, enabled: bool) -> Self {
        Self {
            id,
            enabled,
            config: serde_json::Value::Null,
        }
    }
}

/// **The radar slot's config is the pane's, not a handler's.** It carries
/// this pane's own selection — site, product, elevation — and the live-chunk
/// switch that used to be one global, none of which any handler produces.
/// [`PaneState::adopt_handler_state`] and [`PaneState::slot_config_map`] both
/// hold these four back from the radar handler for that reason, and
/// `the_radar_handler_and_the_pane_do_not_claim_the_same_slot_members` is
/// what says the two owners never collide.
pub const RADAR_SLOT_PANE_KEYS: [&str; 4] = ["site", "product", "elevation", "live_chunks"];

/// Per-pane state: each pane independently selects a radar product,
/// elevation, layer toggles, and maintains its own map viewport.
pub struct PaneState {
    /// This pane's radar site. **Private since WO-E6b**: it is persisted as
    /// a member of the radar slot's config, decoded on load and re-emitted on
    /// save, so every read and write goes through [`Self::site`] /
    /// [`Self::set_site`] and there is one place to change when WO-E9 moves
    /// the selection again.
    site: String,
    pub scan_info: Option<ScanInfo>,
    /// When the data behind this pane's current radar image was collected (UTC).
    pub data_time: Option<NaiveDateTime>,
    /// Private since WO-E6b — see [`Self::site`].
    selected_product: RadarProduct,
    /// Private since WO-E6b — see [`Self::site`].
    selected_elevation: f32,
    pub viewing_live: bool,
    /// Time navigation step size in seconds (0 = single scan mode).
    pub time_step_secs: i64,
    /// Whether this pane follows shared time (plan §3.7). Persisted; default
    /// **true** — every pane before the field existed behaved as linked.
    pub time_link: bool,
    /// Whether this pane's viewport belongs to the linked group. Persisted;
    /// default **true**.
    pub viewport_link: bool,
    /// Whether this pane's layer state belongs to the linked group. Persisted;
    /// default **true**.
    pub layer_link: bool,
    pub hover_value: Option<String>,
    /// Hover tooltip text from overlay handlers (e.g. model data CIN value).
    pub overlay_hover_value: Option<String>,
    pub last_hover_pos: Option<egui::Pos2>,
    pub map_memory: MapMemory,
    /// Per-overlay-type texture caches (background-rendered), keyed by
    /// [`LayerId`]. Only texture overlay kinds (SPC, NWS, discussions) have
    /// cache entries; entries are created lazily.
    pub overlay_textures: HashMap<LayerId, OverlayTextureCache>,
    /// **This pane's layer stack: one [`LayerSlot`] per layer, bottom to top.**
    /// The vector's order is the draw order; each slot carries its own enabled
    /// flag and its own saved config. Replaces the three parallel containers
    /// `draw_order` / `enabled_overlays` / `overlay_configs` (WO-E6b).
    pub layers: Vec<LayerSlot>,
    /// Config-file content addressed to a build that is not this one — see
    /// [`PaneConfigBaggage`]. Written by `load_ui_config`, read only by
    /// `ui_config_json`, and never acted on in between.
    pub config_baggage: PaneConfigBaggage,
    /// Radar display state. Always present; in single-frame mode holds at most
    /// one frame (the current static radar image). In multi-frame mode holds
    /// the full animated loop.
    pub loop_state: LoopPlaybackState,
    /// Which site is currently being loaded for this pane (transient loading indicator).
    pub loading_site: Option<String>,
    /// Generation counter for RadarSites texture invalidation.
    /// Bumped when site, loading_site, or theme changes.
    pub radar_sites_render_gen: u64,
    pub content: PaneContent,
}

impl Default for LoopPlaybackState {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopPlaybackState {
    pub fn new() -> Self {
        Self {
            phase: LoopPhase::Inactive,
            current_frame: 0,
            frames: Vec::new(),
            lookback_secs: 0,
            listing_sampled: None,
            scan_step_secs: None,
            last_advance: None,
            listing_since: None,
            site: String::new(),
            site_lat: 0.0,
            site_lon: 0.0,
            rendered_for: None,
            view: RenderView::PlanView,
            view_key: None,
        }
    }

    pub fn new_for_loop(lookback_secs: u64, site: &RadarSite, view: RenderView) -> Self {
        Self {
            phase: LoopPhase::FetchingScanList,
            current_frame: 0,
            frames: Vec::new(),
            lookback_secs,
            listing_sampled: None,
            scan_step_secs: None,
            last_advance: None,
            // The one place `FetchingScanList` is written, so the one place the
            // clock on that phase starts. See [`Self::listing_since`].
            listing_since: Some(web_time::Instant::now()),
            site: site.name.to_string(),
            site_lat: site.lat,
            site_lon: site.lon,
            rendered_for: None,
            view,
            view_key: None,
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.phase, LoopPhase::Inactive)
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.phase, LoopPhase::Playing)
    }

    pub fn is_render_ready(&self) -> bool {
        matches!(
            self.phase,
            LoopPhase::Ready | LoopPhase::Playing | LoopPhase::Paused
        )
    }

    pub fn is_fetching(&self) -> bool {
        matches!(self.phase, LoopPhase::FetchingScanList)
    }

    /// How long this loop has been waiting for its scan listing, or `None` if
    /// it is not waiting for one.
    pub fn listing_wait(&self, now: web_time::Instant) -> Option<std::time::Duration> {
        if !self.is_fetching() {
            return None;
        }
        Some(
            self.listing_since
                .map_or(std::time::Duration::ZERO, |since| {
                    now.saturating_duration_since(since)
                }),
        )
    }

    pub fn has_playback_started(&self) -> bool {
        matches!(self.phase, LoopPhase::Playing | LoopPhase::Paused)
    }

    pub fn is_rendered_for(&self, target: &RenderTarget) -> bool {
        self.rendered_for
            .as_ref()
            .is_some_and(|t| t.matches(target))
    }

    /// The index of the frame a finished render for `timestamp`, produced for
    /// `target`, must be written to — or `None` if the result has to be dropped.
    pub fn frame_awaiting_render_result(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<usize> {
        if self.view != RenderView::PlanView || !self.is_active() || !self.is_rendered_for(target) {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.render_in_flight)
    }

    pub fn frame_awaiting_render_result_mut(
        &mut self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<&mut LoopFrame> {
        let idx = self.frame_awaiting_render_result(timestamp, target)?;
        Some(&mut self.frames[idx])
    }

    /// The index of the frame that should receive a texture finished by *another*
    /// pane for `timestamp`/`target`, or `None` if this loop cannot use it.
    pub fn frame_accepting_broadcast(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        sweep: BroadcastSweep,
    ) -> Option<usize> {
        if self.view != RenderView::PlanView
            || !self.is_active()
            || !self.is_rendered_for(target)
            || !sweep.agrees()
        {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.image.is_none())
    }

    pub fn frame_accepting_broadcast_mut(
        &mut self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        sweep: BroadcastSweep,
    ) -> Option<&mut LoopFrame> {
        let idx = self.frame_accepting_broadcast(timestamp, target, sweep)?;
        Some(&mut self.frames[idx])
    }

    /// The index of a frame this loop can hand to a pane keyed to `target`, letting
    /// that pane skip a render it would otherwise dispatch.
    pub fn frame_donatable_to(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<usize> {
        if self.view != RenderView::PlanView || !self.is_active() || !self.is_rendered_for(target) {
            return None;
        }
        self.frames.iter().position(|f| {
            f.timestamp == timestamp && f.image.as_ref().is_some_and(|i| i.plan_view().is_some())
        })
    }

    /// Whether this loop's frames are cut for `target` **and** `key` — the
    /// section counterpart of [`Self::is_rendered_for`].
    pub fn is_cut_for(&self, target: &RenderTarget, key: &SectionLoopKey) -> bool {
        self.is_rendered_for(target) && self.section_key() == Some(key)
    }

    /// The section half of this loop's key, or `None` — including for a loop
    /// whose key is a *volume* key.
    pub fn section_key(&self) -> Option<&SectionLoopKey> {
        match &self.view_key {
            Some(LoopViewKey::Section(key)) => Some(key),
            Some(LoopViewKey::Volume(_)) | None => None,
        }
    }

    pub fn volume_key(&self) -> Option<&VolumeLoopKey> {
        match &self.view_key {
            Some(LoopViewKey::Volume(key)) => Some(key),
            Some(LoopViewKey::Section(_)) | None => None,
        }
    }

    /// The index of the frame awaiting a **section** result for
    /// `timestamp`/`target`/`key`, or `None` if this loop is not owed one.
    pub fn frame_awaiting_section_result(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
    ) -> Option<usize> {
        if self.view != RenderView::CrossSection
            || !self.is_active()
            || !self.is_cut_for(target, key)
        {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.render_in_flight)
    }

    pub fn frame_awaiting_section_result_mut(
        &mut self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
    ) -> Option<&mut LoopFrame> {
        let idx = self.frame_awaiting_section_result(timestamp, target, key)?;
        Some(&mut self.frames[idx])
    }

    /// The index of the frame that should receive a **section** raster finished
    /// by another pane, or `None` if this loop cannot use it.
    pub fn frame_accepting_section_broadcast(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
        ladder: u64,
        own_ladder: Option<u64>,
    ) -> Option<usize> {
        if self.view != RenderView::CrossSection
            || !self.is_active()
            || !self.is_cut_for(target, key)
            || own_ladder != Some(ladder)
        {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.image.is_none())
    }

    pub fn frame_accepting_section_broadcast_mut(
        &mut self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
        ladder: u64,
        own_ladder: Option<u64>,
    ) -> Option<&mut LoopFrame> {
        let idx =
            self.frame_accepting_section_broadcast(timestamp, target, key, ladder, own_ladder)?;
        Some(&mut self.frames[idx])
    }

    /// The index of a **section** frame this loop can hand to a pane keyed to
    /// `target`/`key`, letting that pane skip a cut it would otherwise dispatch.
    pub fn section_frame_donatable_to(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
        wanted_ladder: u64,
    ) -> Option<usize> {
        if self.view != RenderView::CrossSection
            || !self.is_active()
            || !self.is_cut_for(target, key)
        {
            return None;
        }
        self.frames.iter().position(|f| {
            f.timestamp == timestamp
                && f.image
                    .as_ref()
                    .and_then(LoopFrameImage::section)
                    .is_some_and(|s| s.ladder == wanted_ladder)
        })
    }

    /// Point the loop's frame renders at `product`/`elevation`, discarding every
    /// frame's render state if that differs from what the frames were last rendered
    /// for. Returns `true` if frames were invalidated.
    pub fn retarget_renders(&mut self, product: RadarProduct, elevation: f32) -> bool {
        self.retarget_renders_for(product, elevation, None)
    }

    /// [`Self::retarget_renders`] including the section half of the key.
    pub fn retarget_renders_for(
        &mut self,
        product: RadarProduct,
        elevation: f32,
        section: Option<SectionLoopKey>,
    ) -> bool {
        self.retarget_renders_keyed(product, elevation, section.map(LoopViewKey::Section))
    }

    /// [`Self::retarget_renders`] including whichever view half this loop has.
    pub fn retarget_renders_keyed(
        &mut self,
        product: RadarProduct,
        elevation: f32,
        key: Option<LoopViewKey>,
    ) -> bool {
        let tilt_matters = self.view.elevation_selects_picture(product);
        // Runs for every looping pane every frame, and almost always finds no change,
        // so ask before building a target rather than allocating one to throw away.
        if self.rendered_for.as_ref().is_some_and(|t| {
            if tilt_matters {
                t.matches_parts(&self.site, product, elevation)
            } else {
                t.site == self.site && t.product == product
            }
        }) && self.view_key == key
        {
            return false;
        }

        // Nothing to discard before the first dispatch — frames start blank.
        let had_previous_target = self.rendered_for.is_some();
        self.rendered_for = Some(RenderTarget::new(self.site.clone(), product, elevation));
        self.view_key = key;
        if !had_previous_target {
            return false;
        }

        for frame in &mut self.frames {
            frame.image = None;
            frame.render_in_flight = false;
            frame.render_failed = false;
        }
        true
    }

    /// Bring the frame list back inside `held`, by the same even sampling the
    /// listing was capped with. Returns whether anything was dropped.
    pub fn cap_frames(&mut self, held: usize) -> bool {
        let Some(indices) = listing_sample_indices(self.frames.len(), held) else {
            return false;
        };
        // The survivor nearest the playhead, so a paused loop stays on the
        // moment the user parked it rather than jumping to the start.
        self.current_frame = indices
            .iter()
            .enumerate()
            .min_by_key(|(_, frame)| frame.abs_diff(self.current_frame))
            .map(|(position, _)| position)
            .unwrap_or(0);
        let mut kept: Vec<Option<LoopFrame>> = self.frames.drain(..).map(Some).collect();
        self.frames = indices.into_iter().filter_map(|i| kept[i].take()).collect();
        self.listing_sampled = Some(true);
        true
    }

    /// Drop textures outside the intended render set once more than `budget` frames
    /// are textured, capping loop memory.
    pub fn evict_textures_outside_render_set(&mut self, budget: usize) {
        let textured = self.frames.iter().filter(|f| f.image.is_some()).count();
        if textured <= budget {
            return;
        }
        let keep = self.render_set_indices(budget);
        for (idx, frame) in self.frames.iter_mut().enumerate() {
            if !keep.contains(&idx) {
                frame.image = None;
            }
        }
    }

    /// Indices of the frames the renderer intends to have textured: up to `budget`
    /// frames, walking outward from the playhead (forward first, then backward).
    pub fn render_set_indices(&self, budget: usize) -> Vec<usize> {
        let num_frames = self.frames.len();
        let budget = num_frames.min(budget);
        let current = self.current_frame;

        let mut indices = Vec::with_capacity(budget);
        for offset in 0..budget {
            let fwd = (current + offset) % num_frames;
            if !indices.contains(&fwd) {
                indices.push(fwd);
            }
            if indices.len() >= budget {
                break;
            }
            let bwd = (current + num_frames - offset) % num_frames;
            if !indices.contains(&bwd) {
                indices.push(bwd);
            }
            if indices.len() >= budget {
                break;
            }
        }
        indices
    }

    /// True when no frame in the intended render set is still waiting on a texture.
    pub fn render_set_settled(
        &self,
        budget: usize,
        scan_available: impl Fn(&LoopFrame) -> bool,
    ) -> bool {
        self.render_set_indices(budget).into_iter().all(|idx| {
            let frame = &self.frames[idx];
            frame.image.is_some()
                || (!frame.render_in_flight && (frame.render_failed || !scan_available(frame)))
        })
    }
}

impl PaneState {
    /// The NEXRAD site this pane is viewing.
    ///
    /// # Why a method beside a public field
    ///
    /// This accessor and the five below it are plain delegation to the
    /// same-named fields and do **nothing else** — no invalidation, no
    /// generation bump, no derived state. They exist so that every production
    /// read and write of the three radar-selection fields already goes through
    /// a function on the day WO-E6b moves those fields behind a layer slot: at
    /// that point the field becomes private and only these six bodies change.
    /// A call site converted here is a call site that migration never touches.
    ///
    /// The fields stay `pub` deliberately — the test suites read them directly
    /// and are migrated in E6b's own named sub-task, and a field and a method
    /// of the same name are two different namespaces in Rust, so both spellings
    /// resolve.
    ///
    /// **The setters must never grow a side effect.** Load flows assign a
    /// pane's site part-way through applying a config, before the scan, the
    /// product list or the viewport have been restored; anything "helpful"
    /// added here (dropping the scan, bumping
    /// [`radar_sites_render_gen`](Self::radar_sites_render_gen), clearing the
    /// loop) would fire in the middle of that sequence and change load
    /// ordering. The invalidations that belong to a site change already live at
    /// the call sites that mean it.
    pub fn site(&self) -> &str {
        &self.site
    }

    /// Set the site this pane is viewing. Plain assignment — see [`Self::site`].
    pub fn set_site(&mut self, site: String) {
        self.site = site;
    }

    /// The radar product this pane has selected.
    pub fn selected_product(&self) -> RadarProduct {
        self.selected_product
    }

    /// Set the selected radar product. Plain assignment — see [`Self::site`].
    pub fn set_selected_product(&mut self, product: RadarProduct) {
        self.selected_product = product;
    }

    /// The elevation angle this pane has selected, in degrees.
    pub fn selected_elevation(&self) -> f32 {
        self.selected_elevation
    }

    /// Set the selected elevation angle. Plain assignment — see [`Self::site`].
    pub fn set_selected_elevation(&mut self, elevation: f32) {
        self.selected_elevation = elevation;
    }

    pub fn new() -> Self {
        Self::with_site("KTLX".to_string())
    }

    pub fn with_site(site: String) -> Self {
        let mut map_memory = MapMemory::default();
        let _ = map_memory.set_zoom(DEFAULT_PANE_ZOOM);
        Self {
            site,
            scan_info: None,
            data_time: None,
            selected_product: RadarProduct::Reflectivity,
            selected_elevation: 0.0,
            viewing_live: true,
            time_step_secs: 600,
            time_link: true,
            viewport_link: true,
            layer_link: true,
            hover_value: None,
            overlay_hover_value: None,
            last_hover_pos: None,
            map_memory,
            // Lazily filled by `overlay_cache_mut`; an absent entry answers
            // every read exactly as a fresh empty cache did.
            overlay_textures: HashMap::new(),
            // **Empty on purpose**, and it is the pane's whole layer state
            // that starts empty rather than only its flags: a pane born here
            // has no saved opinion about any layer, and `Gui` seeds it from
            // the handlers through `initialize_pane_enabled` — which every
            // site that makes a pane calls, because the flags always needed
            // that seeding and now the order comes with them.
            layers: Vec::new(),
            config_baggage: PaneConfigBaggage::default(),
            loop_state: LoopPlaybackState::new(),
            loading_site: None,
            radar_sites_render_gen: 0,
            content: PaneContent::Map(Box::default()),
        }
    }

    pub fn kind(&self) -> PaneKind {
        self.content.kind()
    }

    /// What a render dispatched for this pane produces.
    pub fn render_view(&self) -> rustdar_radar::types::RenderView {
        self.content.render_view()
    }

    /// Whether this pane is drawing the plan view every pane used to be.
    pub fn is_map(&self) -> bool {
        self.render_view() == rustdar_radar::types::RenderView::PlanView
    }

    /// Whether this pane paints map geography through a projector *somewhere*
    /// on screen this frame.
    pub fn draws_ground(&self) -> bool {
        match self.render_view() {
            rustdar_radar::types::RenderView::PlanView => true,
            // Its own map, drawn below the frame and copied onto the box's
            // bottom face — geography on a surface, which is all this asks.
            rustdar_radar::types::RenderView::Volume => {
                self.volume().is_some_and(|volume| !volume.hide_floor)
            }
            rustdar_radar::types::RenderView::CrossSection => false,
        }
    }

    /// Whether this pane takes part in the shared viewport — the group
    /// [`Gui::sync_viewports`](crate::Gui) moves together, at **both** ends.
    pub fn shares_viewport(&self) -> bool {
        self.is_map()
    }

    /// Whether this pane draws the map layers at all — the Layers panel's gate,
    /// and so the question "would a row here toggle anything?".
    pub fn draws_map_layers(&self) -> bool {
        self.render_view() != rustdar_radar::types::RenderView::CrossSection
    }

    /// This pane's render mode, if it is a map pane at all.
    pub fn map_render(&self) -> Option<MapRender> {
        self.map().map(|map| map.render)
    }

    /// Whether this pane can animate a sequence of past volumes.
    pub fn can_loop(&self) -> bool {
        self.render_view().can_loop() && self.cross_section().is_none_or(|s| s.line.is_some())
    }

    /// This pane's cross-section state, or `None` if it is not a section pane.
    pub fn cross_section(&self) -> Option<&CrossSectionPane> {
        match &self.content {
            PaneContent::CrossSection(section) => Some(section),
            _ => None,
        }
    }

    pub fn cross_section_mut(&mut self) -> Option<&mut CrossSectionPane> {
        match &mut self.content {
            PaneContent::CrossSection(section) => Some(section),
            _ => None,
        }
    }

    /// This pane's map state, or `None` if it is a cross-section pane.
    pub fn map(&self) -> Option<&MapPane> {
        match &self.content {
            PaneContent::Map(map) => Some(map),
            PaneContent::CrossSection(_) => None,
        }
    }

    pub fn map_mut(&mut self) -> Option<&mut MapPane> {
        match &mut self.content {
            PaneContent::Map(map) => Some(map),
            PaneContent::CrossSection(_) => None,
        }
    }

    /// This pane's 3D volume state, or `None` unless it is *currently drawing*
    /// the volume.
    pub fn volume(&self) -> Option<&VolumePane> {
        self.map()
            .filter(|map| map.render == MapRender::Volume)
            .map(|map| &map.volume)
    }

    pub fn volume_mut(&mut self) -> Option<&mut VolumePane> {
        self.map_mut()
            .filter(|map| map.render == MapRender::Volume)
            .map(|map| &mut map.volume)
    }

    /// Draw this map pane's ground in `render` from now on, keeping everything
    /// about *what it is looking at* — and keeping the other mode's state, so
    /// that switching back returns the same picture.
    pub fn set_map_render(&mut self, render: MapRender) -> bool {
        let Some(map) = self.map_mut() else {
            return false;
        };
        if map.render == render {
            return true;
        }
        map.render = render;
        self.loop_state = LoopPlaybackState::new();
        true
    }

    /// Convert this pane to `kind`, keeping everything about *what it is looking
    /// at*: its site, its scan, its product and elevation selection, its
    /// viewport and its layer toggles.
    pub fn set_kind(&mut self, kind: PaneKind) {
        if self.kind() == kind {
            return;
        }
        self.set_content(PaneContent::for_kind(kind));
    }

    /// Make this pane draw `view`, whichever combination of kind and render
    /// mode that takes.
    pub fn set_view(&mut self, view: rustdar_radar::types::RenderView) {
        use rustdar_radar::types::RenderView;
        match view {
            RenderView::PlanView | RenderView::Volume => {
                self.set_kind(PaneKind::Map);
                let render = if view == RenderView::Volume {
                    MapRender::Volume
                } else {
                    MapRender::Plan
                };
                self.set_map_render(render);
            }
            RenderView::CrossSection => self.set_kind(PaneKind::CrossSection),
        }
    }

    /// Replace this pane's per-kind content wholesale, as the config loader does
    /// when it has both the kind and the state in hand.
    pub fn set_content(&mut self, content: PaneContent) {
        let previous = self.render_view();
        self.content = content;
        if self.render_view() != previous || !self.render_view().can_loop() {
            self.loop_state = LoopPlaybackState::new();
        }
    }

    pub fn active_image(&self) -> Option<&RadarImageData> {
        self.active_loop_image().and_then(LoopFrameImage::plan_view)
    }

    /// The playing frame's cross-section raster, or `None` when this pane is not
    /// animating a section.
    pub fn active_section_image(&self) -> Option<&SectionImageData> {
        self.active_loop_image().and_then(LoopFrameImage::section)
    }

    /// The playing frame's resident voxel grid, or `None` when this pane is not
    /// animating a 3D volume — including while its loop is still filling.
    pub fn active_volume_frame(&self) -> Option<&VolumeFrameGrid> {
        self.active_loop_image().and_then(LoopFrameImage::volume)
    }

    /// Whichever picture the loop's playhead is on, before it is narrowed to a
    /// kind. One lookup, so the two accessors above cannot walk to different
    /// frames.
    fn active_loop_image(&self) -> Option<&LoopFrameImage> {
        self.loop_state
            .frames
            .get(self.loop_state.current_frame)
            .and_then(|f| f.image.as_ref())
    }

    /// When the data behind the image *currently on screen* was collected.
    pub fn data_time_on_screen(&self) -> Option<NaiveDateTime> {
        if self.loop_state.is_active() {
            return self
                .loop_state
                .frames
                .get(self.loop_state.current_frame)
                .map(|f| f.timestamp);
        }
        self.data_time
    }

    /// What the radar image on screen depicts, **when that is not what this pane
    /// has selected** — the product and sweep the pixels really are, so a caller
    /// can say so.
    pub fn stale_image_on_screen(&self) -> Option<(RadarProduct, f32)> {
        if self.loop_state.is_active() {
            return None;
        }
        let meta = self
            .overlay_cache(&known::RADAR)?
            .current()?
            .radar_meta
            .as_ref()?;
        let matches_selection = match self.get_rendering_params() {
            Some((product, elevation)) => {
                meta.product == product && (meta.elevation - elevation).abs() <= ELEVATION_TOLERANCE
            }
            // No params means this pane's scan does not offer the selected
            // product at all, so no render will be dispatched and the old image
            // will stand indefinitely. There is no snapped angle to compare
            // against, so the product alone decides.
            None => meta.product == self.selected_product(),
        };
        (!matches_selection).then_some((meta.product, meta.elevation))
    }

    /// Where the picture **on the glass** folds, m/s — the Nyquist velocity
    /// the cut behind those pixels declared, or `None` when nothing on screen
    /// can carry that annotation. The ramp is fixed at ±36.01 m/s while a
    /// WSR-88D's Doppler cuts declare 23.84–62.94 m/s (ten volumes measured in
    /// `rustdar_radar::nyquist`), so past ±Vny the sign wraps. A TDWR always
    /// answers `None`: it declares `nyquist_velocity = 0` on every cut.
    pub fn displayed_nyquist_ms(&self) -> Option<f64> {
        if self.selected_product() != RadarProduct::Velocity || !self.is_map() {
            return None;
        }
        if self.loop_state.is_active() {
            // No product gate here, and none is needed: `retarget_renders`
            // drops every frame texture the moment the selection moves, so a
            // looping pane cannot hold a frame depicting anything else.
            return self.active_image().and_then(|frame| frame.nyquist_ms);
        }
        if self.stale_image_on_screen().is_some() {
            return None;
        }
        self.overlay_cache(&known::RADAR)?
            .current()?
            .radar_meta
            .as_ref()?
            .nyquist_ms
    }

    /// Where the melting layer behind the classification **on screen** came
    /// from, or `None` when the picture is not a classification.
    pub fn displayed_melting_layer_source(&self) -> Option<rustdar_radar::hca::MeltingLayerSource> {
        if self.selected_product() != RadarProduct::HydrometeorClassification || !self.is_map() {
            return None;
        }
        if self.loop_state.is_active() {
            return self
                .active_image()
                .and_then(|frame| frame.melting_layer_source);
        }
        if self.stale_image_on_screen().is_some() {
            return None;
        }
        self.overlay_cache(&known::RADAR)?
            .current()?
            .radar_meta
            .as_ref()?
            .melting_layer_source
    }

    /// The storm motion vector behind the storm-relative field **on screen**,
    /// or `None` when the picture is not storm-relative.
    pub fn displayed_storm_motion(&self) -> Option<rustdar_radar::srv::SrvMotion> {
        if self.selected_product() != RadarProduct::StormRelativeVelocity || !self.is_map() {
            return None;
        }
        if self.loop_state.is_active() {
            return self.active_image().and_then(|frame| frame.storm_motion);
        }
        if self.stale_image_on_screen().is_some() {
            return None;
        }
        self.overlay_cache(&known::RADAR)?
            .current()?
            .radar_meta
            .as_ref()?
            .storm_motion
    }

    /// This pane's slot for `id`, or `None` for a layer it has no slot for.
    pub fn slot(&self, id: &LayerId) -> Option<&LayerSlot> {
        self.layers.iter().find(|slot| slot.id == *id)
    }

    /// This pane's slot for `id`, mutably.
    pub fn slot_mut(&mut self, id: &LayerId) -> Option<&mut LayerSlot> {
        self.layers.iter_mut().find(|slot| slot.id == *id)
    }

    /// The draw order, bottom to top — the slot list's own order.
    pub fn draw_order(&self) -> impl DoubleEndedIterator<Item = &LayerId> + ExactSizeIterator + '_ {
        self.layers.iter().map(|slot| &slot.id)
    }

    /// The draw order as an owned list, for the callers that reorder it or
    /// hold it across a borrow of the pane.
    pub fn draw_order_vec(&self) -> Vec<LayerId> {
        self.draw_order().cloned().collect()
    }

    /// Reorder the slot list to `order`, carrying each slot's enabled flag and
    /// config with it. Ids in `order` this pane has no slot for join as fresh
    /// disabled slots; slots `order` omits keep their relative order at the
    /// end, so a partial list can never silently drop a layer.
    pub fn set_draw_order(&mut self, order: &[LayerId]) {
        let mut remaining = std::mem::take(&mut self.layers);
        let mut reordered: Vec<LayerSlot> = Vec::with_capacity(remaining.len());
        for id in order {
            match remaining.iter().position(|slot| slot.id == *id) {
                Some(pos) => reordered.push(remaining.remove(pos)),
                None => reordered.push(LayerSlot::new(id.clone(), false)),
            }
        }
        reordered.append(&mut remaining);
        self.layers = reordered;
    }

    /// Give this pane a slot for every `id` it lacks one for, inserted at the
    /// position `weight_of` puts it in among the slots that already have
    /// weights — the reconcile a saved order gets when the build serves a
    /// layer the file never named.
    pub fn insert_missing_slots(
        &mut self,
        wanted: &[(LayerId, u32, bool)],
        weight_of: &dyn Fn(&LayerId) -> Option<u32>,
    ) {
        for (id, weight, enabled) in wanted {
            if self.layers.iter().any(|slot| slot.id == *id) {
                continue;
            }
            let pos = self
                .layers
                .iter()
                .position(|slot| weight_of(&slot.id).is_some_and(|w| w > *weight));
            let slot = LayerSlot::new(id.clone(), *enabled);
            match pos {
                Some(pos) => self.layers.insert(pos, slot),
                None => self.layers.push(slot),
            }
        }
    }

    pub fn is_overlay_enabled(&self, id: &LayerId) -> bool {
        self.slot(id).is_some_and(|slot| slot.enabled)
    }

    pub fn set_overlay_enabled(&mut self, id: LayerId, enabled: bool) {
        match self.slot_mut(&id) {
            Some(slot) => slot.enabled = enabled,
            // A layer with no slot has no place in the draw order either, so
            // it joins at the top rather than being toggled into invisibility.
            None => self.layers.push(LayerSlot::new(id, enabled)),
        }
    }

    /// This pane's enabled flags as a map, for the callers that compare whole
    /// sets rather than walk the stack.
    pub fn enabled_map(&self) -> HashMap<LayerId, bool> {
        self.layers
            .iter()
            .map(|slot| (slot.id.clone(), slot.enabled))
            .collect()
    }

    /// The configs the registry swap is fed, keyed by id. **Slots with a
    /// `null` config are omitted**: `load_pane_configs` leaves a handler it
    /// finds no entry for exactly as it is, which is what an absent map entry
    /// has always meant here.
    ///
    /// The radar slot is handed over **without** [`RADAR_SLOT_PANE_KEYS`] —
    /// those members are the pane's, and the radar handler owns the rest of
    /// its own slot exactly as every other handler owns all of its.
    pub fn slot_config_map(&self) -> HashMap<LayerId, serde_json::Value> {
        self.layers
            .iter()
            .filter(|slot| !slot.config.is_null())
            .filter_map(|slot| {
                if slot.id != known::RADAR {
                    return Some((slot.id.clone(), slot.config.clone()));
                }
                let mut handler_half = slot.config.as_object()?.clone();
                handler_half.retain(|key, _| !RADAR_SLOT_PANE_KEYS.contains(&key.as_str()));
                (!handler_half.is_empty())
                    .then(|| (slot.id.clone(), serde_json::Value::Object(handler_half)))
            })
            .collect()
    }

    /// Whether any **handler's** slot carries a saved config — the question
    /// `overlay_configs.is_empty()` used to ask, and the radar slot never
    /// counted towards it because it did not exist.
    pub fn has_slot_configs(&self) -> bool {
        self.layers
            .iter()
            .any(|slot| !slot.config.is_null() && slot.id != known::RADAR)
    }

    /// Take a whole layer stack — order, flags and configs together. The one
    /// operation layer-link sync needs, and the reason it cannot lose a half:
    /// there are no longer three halves to keep in step.
    pub fn adopt_layers(&mut self, layers: &[LayerSlot]) {
        self.layers = layers.to_vec();
    }

    /// Overwrite the **handler-owned** part of this pane's slots with the
    /// registry's fresh serialize, KEEPING every slot whose id no handler
    /// serves: an unknown id's saved state (a newer build's layer) must ride
    /// through verbatim, or the next autosave makes the loss permanent.
    ///
    /// The radar slot's config has two owners: the handler's members are
    /// adopted like any other slot's, and [`RADAR_SLOT_PANE_KEYS`] — this
    /// pane's site, product, tilt and live-chunk switch — are left alone. A
    /// plain overwrite would erase the pane's whole selection on the next
    /// layer toggle.
    pub fn adopt_handler_state(
        &mut self,
        registry: &rustdar_overlays::render::overlay_state::OverlayRegistry,
    ) {
        let configs = registry.save_pane_configs();
        let enabled = registry.save_enabled_map();
        for slot in &mut self.layers {
            if registry.handler_by_id(&slot.id).is_none() {
                continue;
            }
            if let Some(&on) = enabled.get(&slot.id) {
                slot.enabled = on;
            }
            let Some(fresh) = configs.get(&slot.id) else {
                continue;
            };
            if slot.id != known::RADAR {
                slot.config = fresh.clone();
                continue;
            }
            // The radar slot has two owners. The handler's members land
            // beside the pane's, never over them.
            let mut merged = match slot.config.take() {
                serde_json::Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            if let serde_json::Value::Object(fresh) = fresh {
                for (key, value) in fresh {
                    if RADAR_SLOT_PANE_KEYS.contains(&key.as_str()) {
                        continue;
                    }
                    merged.insert(key.clone(), value.clone());
                }
            }
            slot.config = serde_json::Value::Object(merged);
        }
        // A registered handler this pane has no slot for at all: it joins the
        // stack, exactly as the old `extend` gave it a map entry.
        let missing: Vec<(LayerId, u32, bool)> = registry
            .handlers()
            .filter(|h| !self.layers.iter().any(|slot| slot.id == h.id()))
            .map(|h| (h.id(), h.draw_order_weight(), h.is_enabled()))
            .collect();
        if !missing.is_empty() {
            let weights: HashMap<LayerId, u32> = registry
                .handlers()
                .map(|h| (h.id(), h.draw_order_weight()))
                .collect();
            self.insert_missing_slots(&missing, &|id| weights.get(id).copied());
            for (id, _, _) in &missing {
                if let (Some(slot), Some(fresh)) = (self.slot_mut(id), configs.get(id)) {
                    slot.config = fresh.clone();
                }
            }
        }
    }

    /// The live-chunk switch this pane's radar slot carries, or `None` for a
    /// slot that has never been given one (a fresh pane, or a file written
    /// before the switch fanned out).
    pub fn radar_live_chunks(&self) -> Option<bool> {
        self.slot(&known::RADAR)?
            .config
            .get("live_chunks")?
            .as_bool()
    }

    /// Write the live-chunk switch into this pane's radar slot.
    pub fn set_radar_live_chunks(&mut self, enabled: bool) {
        let Some(slot) = self.slot_mut(&known::RADAR) else {
            return;
        };
        let mut map = match slot.config.take() {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        map.insert("live_chunks".to_string(), serde_json::Value::Bool(enabled));
        slot.config = serde_json::Value::Object(map);
    }

    pub fn overlay_cache(&self, id: &LayerId) -> Option<&OverlayTextureCache> {
        self.overlay_textures.get(id)
    }

    pub fn overlay_cache_mut(&mut self, id: &LayerId) -> &mut OverlayTextureCache {
        self.overlay_textures.entry(id.clone()).or_default()
    }

    /// Whether `kind`'s texture may be let go, judged against the slot list
    /// that decides it — the single definition of that question.
    pub fn overlay_texture_releasable(layers: &[LayerSlot], id: &LayerId) -> bool {
        // The same fallback [`Self::is_overlay_enabled`] applies: an id with no
        // slot is not drawn, so its texture is releasable.
        *id != known::RADAR && !layers.iter().any(|slot| slot.id == *id && slot.enabled)
    }

    pub fn overlay_texture_is_releasable(&self, id: &LayerId) -> bool {
        Self::overlay_texture_releasable(&self.layers, id)
    }

    /// Let go of the GPU texture of every overlay this pane no longer draws.
    pub fn release_disabled_overlay_textures(&mut self) {
        // Two fields of one struct, borrowed disjointly, which is the whole
        // reason the predicate above is an associated function over the list.
        let layers = &self.layers;
        for (id, cache) in &mut self.overlay_textures {
            if Self::overlay_texture_releasable(layers, id) {
                cache.clear();
            }
        }
    }

    pub fn is_holding_raster(&self) -> bool {
        self.overlay_textures
            .values()
            .any(OverlayTextureCache::is_holding)
    }

    pub fn held_raster_id(&self) -> Option<egui::TextureId> {
        Some(self.overlay_cache(&known::RADAR)?.held_texture()?.id())
    }

    /// Let go of every raster that is still arriving, without showing any.
    pub fn release_held_raster(&mut self) {
        for cache in self.overlay_textures.values_mut() {
            cache.release_hold();
        }
    }

    /// Show the raster this pane is holding, if every texel of it has landed.
    pub fn promote_held_raster(&mut self, delivered: impl Fn(egui::TextureId) -> bool) -> bool {
        let cache = self.overlay_cache_mut(&known::RADAR);
        let Some(held) = cache.take_held_if_delivered(delivered) else {
            return false;
        };
        cache.show(held.data);
        self.data_time = held.data_time;
        true
    }

    /// Show every non-radar overlay picture this pane is holding whose pixels
    /// have all landed.
    pub fn promote_held_overlays(&mut self, delivered: impl Fn(egui::TextureId) -> bool) {
        for (id, cache) in &mut self.overlay_textures {
            if *id == known::RADAR {
                continue;
            }
            if let Some(held) = cache.take_held_if_delivered(&delivered) {
                cache.show(held.data);
            }
        }
    }

    /// Put a freshly placed raster on this pane — now, or when it is whole.
    pub fn place_radar_raster(
        &mut self,
        data: crate::overlay_cache::OverlayTextureData,
        data_time: Option<chrono::NaiveDateTime>,
        already_whole: bool,
    ) {
        let cache = self.overlay_cache_mut(&known::RADAR);
        if already_whole || cache.current().is_none() {
            cache.show(data);
            self.data_time = data_time;
        } else {
            cache.hold(data, data_time);
        }
    }

    pub fn get_rendering_params(&self) -> Option<(RadarProduct, f32)> {
        let elevations = self
            .scan_info
            .as_ref()?
            .product_elevations
            .get(&self.selected_product)?;
        let snapped = elevations
            .iter()
            .min_by(|a, b| {
                ((**a - self.selected_elevation).abs())
                    .total_cmp(&((**b - self.selected_elevation).abs()))
            })
            .copied()
            .unwrap_or(self.selected_elevation);
        Some((self.selected_product, snapped))
    }
}

impl Default for PaneState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PaneLayout {
    /// Number of active panes (1-6 desktop, 1-4 mobile).
    pub pane_count: usize,
    /// Grid configuration. Each element is the number of columns in that row.
    /// e.g., [2, 2] = 2×2 grid, [2, 1] = 2 top + 1 bottom.
    grid: Vec<usize>,
    /// Height ratio for each row (each >= MIN_RATIO, all sum to 1.0).
    row_ratios: Vec<f32>,
    /// Width ratios for columns in each row (each row's ratios sum to 1.0).
    col_ratios: Vec<Vec<f32>>,
}

const MIN_RATIO: f32 = 0.15;
const DIVIDER_HALF_WIDTH: f32 = 4.0;

/// Height/width ratio at which the color scale bars *take up* the horizontal
/// (bottom-edge) orientation, having been vertical.
const COLOR_SCALE_HORIZONTAL_ENTER: f32 = 1.35;
/// Height/width ratio at which they *give it up* again.
const COLOR_SCALE_HORIZONTAL_EXIT: f32 = 1.05;
/// Ratio used for the very first decision, when there is no previous
/// orientation to keep. Sits in the middle of the band.
const COLOR_SCALE_SEED_RATIO: f32 = 1.2;

/// The color scale bars' orientation for the whole map panel, remembered across
/// frames so it has hysteresis instead of a bare threshold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorScaleOrientation {
    /// `None` until the first usable panel rect has been seen.
    horizontal: Option<bool>,
}

impl ColorScaleOrientation {
    pub fn resolve(&mut self, panel_rect: egui::Rect) -> bool {
        let (w, h) = (panel_rect.width(), panel_rect.height());
        // A degenerate or not-yet-laid-out panel must not seed the memory with
        // a decision that then sticks through the hysteresis band.
        if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
            return self.horizontal.unwrap_or(false);
        }

        let ratio = h / w;
        let horizontal = match self.horizontal {
            None => ratio > COLOR_SCALE_SEED_RATIO,
            // Already horizontal: keep it until the panel is clearly not portrait.
            Some(true) => ratio > COLOR_SCALE_HORIZONTAL_EXIT,
            // Already vertical: take it up only when the panel is clearly portrait.
            Some(false) => ratio > COLOR_SCALE_HORIZONTAL_ENTER,
        };
        self.horizontal = Some(horizontal);
        horizontal
    }
}

impl Default for PaneLayout {
    fn default() -> Self {
        Self::for_count(1)
    }
}

impl PaneLayout {
    /// Create a layout for the given pane count, clamped to
    /// `1..=`[`MAX_PANES_DESKTOP`].
    pub fn for_count(count: usize) -> Self {
        let count = count.clamp(1, MAX_PANES_DESKTOP);
        let grid = match count {
            1 => vec![1],
            2 => vec![2],
            3 => vec![2, 1],
            4 => vec![2, 2],
            5 => vec![3, 2],
            6 => vec![3, 3],
            // Unreachable after the clamp above. Left as a total match rather
            // than a panic: a layout is not worth crashing over, and the clamp
            // is what makes this arm dead.
            _ => vec![1],
        };
        let num_rows = grid.len();
        let row_ratios = vec![1.0 / num_rows as f32; num_rows];
        let col_ratios = grid
            .iter()
            .map(|&cols| vec![1.0 / cols as f32; cols])
            .collect();
        Self {
            pane_count: count,
            grid,
            row_ratios,
            col_ratios,
        }
    }

    pub fn grid(&self) -> &[usize] {
        &self.grid
    }

    pub fn pane_rect(&self, pane_idx: usize, total_rect: egui::Rect) -> egui::Rect {
        let mut row_y = total_rect.top();
        let mut idx = 0;
        for (row_idx, &cols) in self.grid.iter().enumerate() {
            let row_height = total_rect.height() * self.row_ratios[row_idx];
            if pane_idx < idx + cols {
                let col_in_row = pane_idx - idx;
                let col_x: f32 = self.col_ratios[row_idx][..col_in_row].iter().sum();
                let col_width = total_rect.width() * self.col_ratios[row_idx][col_in_row];
                let min_x = total_rect.left() + total_rect.width() * col_x;
                return egui::Rect::from_min_size(
                    egui::pos2(min_x, row_y),
                    egui::vec2(col_width, row_height),
                );
            }
            row_y += row_height;
            idx += cols;
        }
        // Fallback — shouldn't happen with valid index
        total_rect
    }

    /// Handle draggable dividers between panes. Call AFTER rendering pane maps
    /// so divider interactions take priority over map panning in the overlap zone.
    pub fn handle_dividers(&mut self, ui: &mut egui::Ui, total_rect: egui::Rect) {
        if self.pane_count <= 1 {
            return;
        }

        let mut y = total_rect.top();
        for row_idx in 0..self.grid.len().saturating_sub(1) {
            y += total_rect.height() * self.row_ratios[row_idx];
            let divider_rect = egui::Rect::from_min_max(
                egui::pos2(total_rect.left(), y - DIVIDER_HALF_WIDTH),
                egui::pos2(total_rect.right(), y + DIVIDER_HALF_WIDTH),
            );
            let id = egui::Id::new(("h_div", row_idx));
            drag_divider(
                ui,
                divider_rect,
                id,
                &mut self.row_ratios,
                row_idx,
                total_rect.height(),
                true,
            );
        }

        let mut row_y = total_rect.top();
        for (row_idx, &cols) in self.grid.iter().enumerate() {
            let row_height = total_rect.height() * self.row_ratios[row_idx];
            let mut col_x = total_rect.left();
            for col_idx in 0..cols.saturating_sub(1) {
                col_x += total_rect.width() * self.col_ratios[row_idx][col_idx];
                let divider_rect = egui::Rect::from_min_max(
                    egui::pos2(col_x - DIVIDER_HALF_WIDTH, row_y),
                    egui::pos2(col_x + DIVIDER_HALF_WIDTH, row_y + row_height),
                );
                let id = egui::Id::new(("v_div", row_idx, col_idx));
                drag_divider(
                    ui,
                    divider_rect,
                    id,
                    &mut self.col_ratios[row_idx],
                    col_idx,
                    total_rect.width(),
                    false,
                );
            }
            row_y += row_height;
        }
    }
}

/// Shared divider drag logic: interact, apply ratio delta, set cursor.
/// `use_y_axis = true` for horizontal dividers (row splits), `false` for vertical (column splits).
fn drag_divider(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: egui::Id,
    ratios: &mut [f32],
    idx: usize,
    total_extent: f32,
    use_y_axis: bool,
) {
    let response = ui.interact(rect, id, egui::Sense::drag());
    if response.dragged() {
        let delta = if use_y_axis {
            response.drag_delta().y
        } else {
            response.drag_delta().x
        };
        let ratio_delta = delta / total_extent;
        let new_a = ratios[idx] + ratio_delta;
        let new_b = ratios[idx + 1] - ratio_delta;
        if new_a >= MIN_RATIO && new_b >= MIN_RATIO {
            ratios[idx] = new_a;
            ratios[idx + 1] = new_b;
        }
    }
    if response.hovered() || response.dragged() {
        let cursor = if use_y_axis {
            egui::CursorIcon::ResizeVertical
        } else {
            egui::CursorIcon::ResizeHorizontal
        };
        ui.ctx().set_cursor_icon(cursor);
    }
}

#[cfg(test)]
mod render_params_tests;

/// The section loop's identity and the plan-view/section collision it closes.
#[cfg(test)]
mod section_loop_tests;

#[cfg(test)]
mod tests;
