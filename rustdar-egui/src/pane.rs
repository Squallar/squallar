use std::collections::HashMap;
use rustdar_overlays::render::overlay_state::OverlayKind;
use crate::overlay_cache::OverlayTextureCache;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::{RadarProduct, ScanInfo};
use chrono::NaiveDateTime;
use std::sync::Arc;
use walkers::MapMemory;

const DEFAULT_PANE_ZOOM: f64 = 4.0;

/// Identifies a pane in the multi-pane layout.
pub type PaneId = usize;

/// Holds the radar image texture and its associated metadata.
#[derive(Clone)]
pub struct RadarImageData {
    pub texture: egui::TextureHandle,
    pub lat: f64,
    pub lon: f64,
    pub max_range_km: f64,
    pub value_data: Arc<Vec<f32>>,
}

/// A single rendered frame in a radar loop.
pub struct LoopFrame {
    /// UTC timestamp of this scan.
    pub timestamp: NaiveDateTime,
    /// Rendered texture, `None` if not yet rendered or evicted.
    pub texture: Option<RadarImageData>,
    /// True while a background render is in progress for this frame.
    pub render_in_flight: bool,
    /// True once a render for this frame has been attempted and produced nothing
    /// (no matching sweep for the selected product/elevation, or the render itself
    /// failed). Terminal for this frame's current scan data: the dispatcher stops
    /// retrying it, and it no longer holds up loop readiness. Without this, an
    /// unrenderable frame would either be re-spawned every frame forever or wedge
    /// the loop in `Rendering` permanently.
    pub render_failed: bool,
}

/// Tolerance for comparing two selected elevation angles. Shared with the render
/// dispatcher, which uses it when deciding whether two panes' selections are the
/// same and whether a queued render already covers a frame.
pub const ELEVATION_TOLERANCE: f32 = 0.01;

/// Every input `render_radar_to_image` is given *except the scan itself*: the radar
/// site whose coordinates set the projection, and the product/elevation selection
/// that picks the sweep out of that scan.
///
/// This is the render target key. It is stored on `LoopPlaybackState::rendered_for`,
/// stamped onto every dispatched render, and compared on arrival so a result
/// produced for one target is never painted onto frames keyed to another.
///
/// It is deliberately *not* a claim that `(timestamp, target)` identifies an image.
/// The scan a frame renders comes from `LoopDownloadManager`'s cache, which is keyed
/// on timestamp alone with no site in it, and `append_scan_to_active_loops` appends a
/// polled scan to every active loop without checking whose site it came from — so a
/// loop can be handed a scan from another site and will render it with its own
/// coordinates and stamp its own target on the result. This key cannot detect that;
/// it derives from the loop, not from the scan. Fixing it means keying the scan cache
/// on `(site, timestamp)`, which is tracked separately.
///
/// `site` is the site the loop's *geometry* was captured for — the same lookup that
/// produced `LoopPlaybackState::site_lat`/`site_lon`, which is what
/// `render_radar_to_image` actually projects with. It is deliberately not the pane's
/// live `site` field: the two can drift (a pane's site is re-synced from the active
/// pane without rebuilding its loop), and it is the geometry the image depends on.
///
/// No `PartialEq` on purpose — `elevation` is an `f32` carried straight from a combo
/// box, so `==` would be the wrong comparison. Use [`RenderTarget::matches`].
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
        Self { site: site.into(), product, elevation }
    }

    /// Whether this target names the same image as `site`/`product`/`elevation`.
    /// Site and product are exact; elevation is compared within
    /// `ELEVATION_TOLERANCE`, since the selection is an `f32` that round-trips
    /// through the UI and the scan's own sweep angles.
    ///
    /// Takes the parts loose so a caller that already holds them — notably
    /// `retarget_renders`, which runs for every looping pane every frame — can ask
    /// without allocating a `RenderTarget` just to throw it away.
    pub fn matches_parts(&self, site: &str, product: RadarProduct, elevation: f32) -> bool {
        self.site == site
            && self.product == product
            && (self.elevation - elevation).abs() <= ELEVATION_TOLERANCE
    }

    /// Whether two targets name the same image.
    pub fn matches(&self, other: &RenderTarget) -> bool {
        self.matches_parts(&other.site, other.product, other.elevation)
    }
}

/// The state phases for a radar loop playback instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopPhase {
    /// Loop mode is disabled (single-frame mode).
    Inactive,
    /// Loop is enabled and waiting for the scan listing to complete.
    FetchingScanList,
    /// Scans listed and downloads/renders started, waiting to reach render budget.
    Rendering,
    /// Sufficient frames have rendered to allow playback, but playing is not started.
    Ready,
    /// Loop is actively playing/animating forward through frames.
    Playing,
    /// User paused the active loop (has enough rendered frames).
    Paused,
}

/// Per-pane loop playback state.
///
/// Always present on every pane. In single-frame mode (`phase == LoopPhase::Inactive`),
/// `frames` holds at most one entry — the current static radar image. When the
/// user enables loop mode, the phase transitions and multiple historical frames
/// are fetched and rendered.
pub struct LoopPlaybackState {
    /// The current phase of the loop playback lifecycle.
    pub phase: LoopPhase,
    /// Index of the currently displayed frame in `frames`.
    pub current_frame: usize,
    /// Ordered list of frames (oldest-first).
    pub frames: Vec<LoopFrame>,
    /// Lookback duration in seconds that was requested.
    pub lookback_secs: u64,
    /// Instant of the last frame advance (for animation timing).
    pub last_advance: Option<std::time::Instant>,
    /// NEXRAD site code the loop's geometry belongs to, captured at loop creation
    /// from the same lookup as `site_lat`/`site_lon`. Every frame in this loop is
    /// rendered and positioned with those coordinates, so this — not the pane's
    /// live `site` field — is the site half of the render target.
    pub site: String,
    /// Radar site latitude, captured at loop creation for rendering.
    pub site_lat: f64,
    /// Radar site longitude, captured at loop creation for rendering.
    pub site_lon: f64,
    /// The [`RenderTarget`] every frame's render state was produced for, or `None`
    /// before the first dispatch. The user can change the pane's product or
    /// elevation at any time, and both pieces of per-frame render state are
    /// judgements about that selection — a `texture` shows that product, and a
    /// `render_failed` flag means "this scan carries no sweep for that product".
    /// When the selection moves, both are stale; see `retarget_renders`.
    ///
    /// The site rides along because it is a render input like any other, and every
    /// path that hands one loop's image to another pane has to check it. A loop is
    /// rebuilt from scratch when the pane changes site, so this half never moves
    /// under a live loop — it exists so that results and sibling textures carrying
    /// another site's geometry are rejected by construction rather than by luck.
    ///
    /// It does *not* make a frame's image fully determined: the scan is still looked
    /// up by timestamp alone, with no site in the key. See [`RenderTarget`].
    pub rendered_for: Option<RenderTarget>,
}

/// Per-pane state: each pane independently selects a radar product,
/// elevation, layer toggles, and maintains its own map viewport.
pub struct PaneState {
    /// NEXRAD site code this pane is viewing (e.g. "KTLX").
    pub site: String,
    /// Product/elevation metadata for this pane's site.
    pub scan_info: Option<ScanInfo>,
    pub selected_product: RadarProduct,
    pub selected_elevation: f32,
    /// Whether this pane is viewing the latest (live) data.
    pub viewing_live: bool,
    /// Time navigation step size in seconds (0 = single scan mode).
    pub time_step_secs: i64,
    pub hover_value: Option<String>,
    /// Hover tooltip text from overlay handlers (e.g. model data CIN value).
    pub overlay_hover_value: Option<String>,
    pub last_hover_pos: Option<egui::Pos2>,
    pub map_memory: MapMemory,
    /// Per-overlay-type texture caches (background-rendered), keyed by `OverlayKind`.
    /// Only texture overlay kinds (SPC, NWS, discussions) have cache entries.
    pub overlay_textures: HashMap<OverlayKind, OverlayTextureCache>,
    /// Per-pane draw order (bottom to top). Controls the visual stacking of all
    /// map layers. Persisted across sessions.
    pub draw_order: Vec<OverlayKind>,
    /// Per-pane overlay enabled state (master visibility for each overlay kind).
    /// When `sync_layers` is on, this is propagated from the active pane to all others.
    pub enabled_overlays: HashMap<OverlayKind, bool>,
    /// Per-pane overlay handler config snapshots (serialized handler state per kind).
    /// Swapped into/out of the global OverlayRegistry around access points so each
    /// pane can independently configure overlay sub-controls (categories, day, etc.).
    pub overlay_configs: HashMap<OverlayKind, serde_json::Value>,
    /// Radar display state. Always present; in single-frame mode holds at most
    /// one frame (the current static radar image). In multi-frame mode holds
    /// the full animated loop.
    pub loop_state: LoopPlaybackState,
    /// Which site is currently being loaded for this pane (transient loading indicator).
    pub loading_site: Option<String>,
    /// Generation counter for RadarSites texture invalidation.
    /// Bumped when site, loading_site, or theme changes.
    pub radar_sites_render_gen: u64,
}

impl Default for LoopPlaybackState {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopPlaybackState {
    /// Create a default single-frame (non-loop) state.
    ///
    /// The site fields are placeholders: the state is `Inactive` with no frames, so
    /// nothing is ever rendered or accepted against them.
    pub fn new() -> Self {
        Self {
            phase: LoopPhase::Inactive,
            current_frame: 0,
            frames: Vec::new(),
            lookback_secs: 0,
            last_advance: None,
            site: String::new(),
            site_lat: 0.0,
            site_lon: 0.0,
            rendered_for: None,
        }
    }

    /// Create a new initialized loop state starting the fetch phase.
    ///
    /// Takes the whole [`RadarSite`] rather than a code and a pair of coordinates:
    /// the code is what the render target is compared on and the coordinates are what
    /// frames are actually projected with, so they have to describe the same site. As
    /// separate parameters a caller could pass the pane's site code alongside another
    /// site's coordinates, and every later comparison would be exact and wrong.
    pub fn new_for_loop(lookback_secs: u64, site: &RadarSite) -> Self {
        Self {
            phase: LoopPhase::FetchingScanList,
            current_frame: 0,
            frames: Vec::new(),
            lookback_secs,
            last_advance: None,
            site: site.name.to_string(),
            site_lat: site.lat,
            site_lon: site.lon,
            rendered_for: None,
        }
    }

    /// True if the loop is active (`new_for_loop` was called; single frame mode uses `Inactive`).
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, LoopPhase::Inactive)
    }

    /// True if actively playing back frames.
    pub fn is_playing(&self) -> bool {
        matches!(self.phase, LoopPhase::Playing)
    }

    /// True if enough frames have rendered for playback to be enabled.
    pub fn is_render_ready(&self) -> bool {
        matches!(self.phase, LoopPhase::Ready | LoopPhase::Playing | LoopPhase::Paused)
    }

    /// True during the initial scan list fetch.
    pub fn is_fetching(&self) -> bool {
        matches!(self.phase, LoopPhase::FetchingScanList)
    }

    /// True if playback was previously started (could be paused or playing).
    pub fn has_playback_started(&self) -> bool {
        matches!(self.phase, LoopPhase::Playing | LoopPhase::Paused)
    }

    /// True if the frames' render state is keyed to exactly this target.
    pub fn is_rendered_for(&self, target: &RenderTarget) -> bool {
        self.rendered_for.as_ref().is_some_and(|t| t.matches(target))
    }

    /// The index of the frame a finished render for `timestamp`, produced for
    /// `target`, must be written to — or `None` if the result has to be dropped.
    ///
    /// Two independent ways a result goes stale, and both must be checked:
    ///
    /// - The pane retargeted while the render ran, so the image depicts a site,
    ///   product or elevation the frames are no longer keyed to. Checking "is the
    ///   frame still marked in flight?" cannot catch this: `retarget_renders` clears
    ///   the mark, but the very same dispatch pass re-spawns the frame for the new
    ///   target and marks it again, so the older render's result arrives to a frame
    ///   that *is* in flight. Comparing the target catches it, and a late result that
    ///   still matches the current target is safe to apply: the target fixes every
    ///   render input except the scan, and the scan for a given `(site, timestamp)`
    ///   does not change under a live loop, so the pending render would produce the
    ///   same image. That qualifier is load-bearing rather than pedantic — the cache is
    ///   keyed on timestamp alone and `cache_scan` inserts unconditionally, so a second
    ///   site's scan at a colliding timestamp overwrites the first. See
    ///   [`RenderTarget`] for why this key cannot see that.
    /// - The frame is not expecting a result at all: the frame list was rebuilt, the
    ///   graphics state was cleared, or a sibling pane already supplied the texture.
    ///
    /// Returns the *index* rather than a yes/no so the caller cannot look the frame up
    /// a second time and land somewhere else. Timestamps are unique across a frame
    /// list today, but only incidentally — a predicate answering "is some frame with
    /// this timestamp in flight?" paired with a caller fetching "the frame with this
    /// timestamp" is two lookups that are free to disagree, and the frame the
    /// predicate cleared would then stay marked in flight forever.
    pub fn frame_awaiting_render_result(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<usize> {
        if !self.is_active() || !self.is_rendered_for(target) {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.render_in_flight)
    }

    /// [`Self::frame_awaiting_render_result`] as a mutable borrow of the frame itself.
    ///
    /// This is what callers use. Handing back the frame rather than its index leaves
    /// nothing for a caller to re-derive: the borrow of `self` is live for as long as
    /// the frame is held, so "look the frame up again by timestamp" is not expressible
    /// at the call site. The index form stays public so the choice can be asserted
    /// directly in tests.
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
    ///
    /// Two panes showing the same site at the same product and elevation render
    /// byte-identical images, so one render can serve both. The site is what makes
    /// that true, and it is not implied by the panes agreeing on product and
    /// elevation: `propagate_layer_sync` converges `PaneState::site` across panes but
    /// never rebuilds their loops, so two panes can agree on every visible control
    /// while their loops still carry different geometry. Handing an image across that
    /// gap positions it at coordinates it was not projected for.
    ///
    /// Only untextured frames qualify — a frame that already has an image is not
    /// improved by an identical one, and overwriting it would churn texture handles.
    pub fn frame_accepting_broadcast(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<usize> {
        if !self.is_active() || !self.is_rendered_for(target) {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.texture.is_none())
    }

    /// [`Self::frame_accepting_broadcast`] as a mutable borrow of the frame itself,
    /// for the same reason as [`Self::frame_awaiting_render_result_mut`].
    pub fn frame_accepting_broadcast_mut(
        &mut self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<&mut LoopFrame> {
        let idx = self.frame_accepting_broadcast(timestamp, target)?;
        Some(&mut self.frames[idx])
    }

    /// The index of a frame this loop can hand to a pane keyed to `target`, letting
    /// that pane skip a render it would otherwise dispatch.
    ///
    /// The mirror of [`Self::frame_accepting_broadcast`] — the dispatcher looks for a
    /// donor *before* rendering, the response path pushes to receivers *after* — and
    /// it must apply the same test, including the site. If the two disagree the
    /// dispatcher suppresses a pane's own render on the promise of a broadcast the
    /// response path then refuses, and the frame is served by neither.
    pub fn frame_donatable_to(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<usize> {
        if !self.is_active() || !self.is_rendered_for(target) {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.texture.is_some())
    }

    /// Point the loop's frame renders at `product`/`elevation`, discarding every
    /// frame's render state if that differs from what the frames were last rendered
    /// for. Returns `true` if frames were invalidated.
    ///
    /// Both pieces of per-frame render state are only meaningful relative to a
    /// selection: a `texture` depicts one product at one elevation, and a
    /// `render_failed` flag records that the frame's scan carries no sweep for that
    /// product. The user can change either at any time from the pane's combo boxes,
    /// which write straight through to the pane. Without this, a frame retired under
    /// a product that only some scans carry would stay blank forever after switching
    /// to a product every scan has — and readiness counts retired frames as settled,
    /// so playback would animate with permanent holes.
    ///
    /// In-flight renders are un-marked as well, since nothing is owed to a frame whose
    /// target moved. That alone does *not* make their results stale — the same dispatch
    /// pass re-spawns and re-marks the frame — so rejecting them is
    /// `frame_awaiting_render_result`'s job, via the target stamped on the response.
    ///
    /// Only the product and elevation are parameters: the target's site is the loop's
    /// own `site`, which is fixed for the life of a `LoopPlaybackState`. A pane that
    /// changes site gets a whole new loop state rather than a retarget.
    pub fn retarget_renders(&mut self, product: RadarProduct, elevation: f32) -> bool {
        // Runs for every looping pane every frame, and almost always finds no change,
        // so ask before building a target rather than allocating one to throw away.
        if self
            .rendered_for
            .as_ref()
            .is_some_and(|t| t.matches_parts(&self.site, product, elevation))
        {
            return false;
        }

        // Nothing to discard before the first dispatch — frames start blank.
        let had_previous_target = self.rendered_for.is_some();
        self.rendered_for = Some(RenderTarget::new(self.site.clone(), product, elevation));
        if !had_previous_target {
            return false;
        }

        for frame in &mut self.frames {
            frame.texture = None;
            frame.render_in_flight = false;
            frame.render_failed = false;
        }
        true
    }

    /// Drop textures outside the intended render set once more than `budget` frames
    /// are textured, capping loop memory.
    ///
    /// Deliberately shares `render_set_indices` with the dispatcher and the readiness
    /// check: an eviction rule that disagreed with the dispatcher could drop the
    /// texture of a frame that is about to be re-rendered, churning renders forever.
    pub fn evict_textures_outside_render_set(&mut self, budget: usize) {
        let textured = self.frames.iter().filter(|f| f.texture.is_some()).count();
        if textured <= budget {
            return;
        }
        let keep = self.render_set_indices(budget);
        for (idx, frame) in self.frames.iter_mut().enumerate() {
            if !keep.contains(&idx) {
                frame.texture = None;
            }
        }
    }

    /// Indices of the frames the renderer intends to have textured: up to `budget`
    /// frames, walking outward from the playhead (forward first, then backward).
    ///
    /// This is the "intended render set". The dispatcher spawns renders for exactly
    /// these frames, and readiness waits for exactly these frames, so both must use
    /// this function — if they disagree, readiness can fire over frames that were
    /// never rendered. `budget` is clamped to the frame count.
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
    ///
    /// This is deliberately *not* "nothing is in flight right now". The concurrent
    /// render budget is shared with static pane renders, so a batch of loop frames
    /// can be starved: only some spawn, those finish, and for an instant nothing is
    /// in flight even though most of the set is still blank. Treating that as ready
    /// makes playback animate mostly-empty frames. A frame is settled only if it has
    /// a texture, or nothing is going to produce one for it (no render in flight, and
    /// either it has been ruled out via `render_failed` or its scan has not
    /// downloaded yet — the latter is gated separately by the download check).
    ///
    /// `scan_available` reports whether the frame's scan data has been downloaded;
    /// that cache lives outside the pane, so the caller supplies it.
    pub fn render_set_settled(
        &self,
        budget: usize,
        scan_available: impl Fn(&LoopFrame) -> bool,
    ) -> bool {
        self.render_set_indices(budget).into_iter().all(|idx| {
            let frame = &self.frames[idx];
            frame.texture.is_some()
                || (!frame.render_in_flight && (frame.render_failed || !scan_available(frame)))
        })
    }
}

impl PaneState {
    pub fn new() -> Self {
        Self::with_site("KTLX".to_string())
    }

    /// Create a new pane viewing the given site.
    pub fn with_site(site: String) -> Self {
        let mut map_memory = MapMemory::default();
        let _ = map_memory.set_zoom(DEFAULT_PANE_ZOOM);
        Self {
            site,
            scan_info: None,
            selected_product: RadarProduct::Reflectivity,
            selected_elevation: 0.0,
            viewing_live: true,
            time_step_secs: 600,
            hover_value: None,
            overlay_hover_value: None,
            last_hover_pos: None,
            map_memory,
            overlay_textures: OverlayKind::all()
                .iter()
                .map(|&k| (k, OverlayTextureCache::new()))
                .collect(),
            draw_order: OverlayKind::default_draw_order(),
            enabled_overlays: HashMap::new(),
            overlay_configs: HashMap::new(),
            loop_state: LoopPlaybackState::new(),
            loading_site: None,
            radar_sites_render_gen: 0,
        }
    }

    /// The currently active radar image (from loop frame or static render).
    pub fn active_image(&self) -> Option<&RadarImageData> {
        self.loop_state.frames
            .get(self.loop_state.current_frame)
            .and_then(|f| f.texture.as_ref())
    }

    /// Whether this overlay is enabled for this pane.
    ///
    /// Falls back to `false` if the kind has no entry (uninitialised pane).
    pub fn is_overlay_enabled(&self, kind: OverlayKind) -> bool {
        self.enabled_overlays.get(&kind).copied().unwrap_or(false)
    }

    /// Set the per-pane enabled state for a given overlay kind.
    pub fn set_overlay_enabled(&mut self, kind: OverlayKind, enabled: bool) {
        self.enabled_overlays.insert(kind, enabled);
    }

    /// Get the overlay texture cache for a given kind (read-only).
    pub fn overlay_cache(&self, kind: OverlayKind) -> Option<&OverlayTextureCache> {
        self.overlay_textures.get(&kind)
    }

    /// Get the overlay texture cache for a given kind, inserting a default if absent.
    pub fn overlay_cache_mut(&mut self, kind: OverlayKind) -> &mut OverlayTextureCache {
        self.overlay_textures.entry(kind).or_default()
    }

    /// Get rendering params for this pane (product + closest elevation).
    pub fn get_rendering_params(&self) -> Option<(RadarProduct, f32)> {
        self.scan_info.as_ref().and_then(|si| {
            si.product_elevations
                .get(&self.selected_product)
                .and_then(|elevations| {
                    elevations
                        .iter()
                        .min_by(|a, b| {
                            ((**a - self.selected_elevation).abs())
                                .total_cmp(&((**b - self.selected_elevation).abs()))
                        })
                        .copied()
                })
                .map(|elev_angle| (self.selected_product, elev_angle))
        })
    }

}

impl Default for PaneState {
    fn default() -> Self {
        Self::new()
    }
}

/// Maximum number of panes on desktop.
pub const MAX_PANES_DESKTOP: usize = 6;

/// Maximum number of panes on mobile.
pub const MAX_PANES_MOBILE: usize = 4;

/// Defines how panes are arranged in a grid layout.
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
///
/// The gap between this and [`COLOR_SCALE_HORIZONTAL_ENTER`] is the whole point:
/// a single threshold — whatever its value — is a point the layout can be parked
/// on or dragged across, and 1.2 sat 4% away from a 16:10 laptop's two-pane
/// split and landed exactly on a 4:3 five-pane one. A ratio inside this band
/// changes nothing at all; only leaving it flips the bars.
const COLOR_SCALE_HORIZONTAL_EXIT: f32 = 1.05;
/// Ratio used for the very first decision, when there is no previous
/// orientation to keep. Sits in the middle of the band.
const COLOR_SCALE_SEED_RATIO: f32 = 1.2;

/// The color scale bars' orientation for the whole map panel, remembered across
/// frames so it has hysteresis instead of a bare threshold.
///
/// # Why the panel and not each pane
///
/// The orientation used to be decided per pane, from the pane's own rect. That
/// is a defensible reading of "the bar should span the pane's shorter axis", but
/// it has two failures a threshold cannot fix:
///
/// * **Mixed orientations on one screen.** A three-pane `[2, 1]` grid on a
///   portrait phone gives two tall panes (h/w ≈ 2.0) and one wide one
///   (h/w ≈ 1.0), so the same screen showed two bottom bars and one right-hand
///   bar. No threshold helps: the panes genuinely disagree.
/// * **Divider drags.** Dragging a divider changes pane rects continuously, so
///   any per-pane threshold is something the user can scrub back and forth
///   across, hopping the bars mid-drag.
///
/// Keying on the panel — the rect the whole grid is laid out in — fixes both
/// outright. Every pane on a screen agrees by construction, and the panel rect
/// does not move when a divider is dragged, so dragging cannot flip anything at
/// all. What is left is window resizes and device rotation, which is what the
/// hysteresis band above is for.
///
/// The single-pane case, which is the overwhelmingly common one on every
/// platform, is unchanged: there the panel *is* the pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorScaleOrientation {
    /// `None` until the first usable panel rect has been seen.
    horizontal: Option<bool>,
}

impl ColorScaleOrientation {
    /// Resolve the orientation for this frame's `panel_rect`, remembering it.
    ///
    /// Returns `true` for horizontal bars along the bottom edge, `false` for
    /// vertical bars along the right edge. Call once per frame, before the pane
    /// loop, and pass the result to every pane.
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
    /// Create a layout for the given pane count.
    pub fn for_count(count: usize) -> Self {
        let grid = match count {
            1 => vec![1],
            2 => vec![2],
            3 => vec![2, 1],
            4 => vec![2, 2],
            5 => vec![3, 2],
            6 => vec![3, 3],
            _ => vec![1],
        };
        let num_rows = grid.len();
        let row_ratios = vec![1.0 / num_rows as f32; num_rows];
        let col_ratios = grid.iter().map(|&cols| vec![1.0 / cols as f32; cols]).collect();
        Self {
            pane_count: count,
            grid,
            row_ratios,
            col_ratios,
        }
    }

    /// Get the grid configuration.
    pub fn grid(&self) -> &[usize] {
        &self.grid
    }

    /// Compute the rect for the pane at the given index within the given total rect.
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

        // Horizontal dividers (between rows)
        let mut y = total_rect.top();
        for row_idx in 0..self.grid.len().saturating_sub(1) {
            y += total_rect.height() * self.row_ratios[row_idx];
            let divider_rect = egui::Rect::from_min_max(
                egui::pos2(total_rect.left(), y - DIVIDER_HALF_WIDTH),
                egui::pos2(total_rect.right(), y + DIVIDER_HALF_WIDTH),
            );
            let id = egui::Id::new(("h_div", row_idx));
            drag_divider(ui, divider_rect, id, &mut self.row_ratios, row_idx, total_rect.height(), true);
        }

        // Vertical dividers (between columns in each row)
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
                drag_divider(ui, divider_rect, id, &mut self.col_ratios[row_idx], col_idx, total_rect.width(), false);
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
        let delta = if use_y_axis { response.drag_delta().y } else { response.drag_delta().x };
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
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A panel `w` by `h` logical pixels.
    fn panel(w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))
    }

    /// The orientation follows the panel's shape: landscape windows get the
    /// vertical (right-edge) bar, portrait ones the horizontal (bottom) bar.
    #[test]
    fn color_scale_orientation_follows_the_panel_shape() {
        // Every landscape desktop/laptop aspect, and a landscape phone.
        for (w, h) in [(1920.0, 1080.0), (1920.0, 1200.0), (1280.0, 1024.0), (2340.0, 1080.0)] {
            assert!(
                !ColorScaleOrientation::default().resolve(panel(w, h)),
                "{w}x{h} is landscape: the bar belongs on the right edge"
            );
        }
        // Phone and tablet portrait.
        for (w, h) in [(1080.0, 2340.0), (1200.0, 1920.0), (1200.0, 1600.0)] {
            assert!(
                ColorScaleOrientation::default().resolve(panel(w, h)),
                "{w}x{h} is portrait: the bar belongs along the bottom"
            );
        }
    }

    /// The decision is sticky inside the band, which is what makes it
    /// hysteresis rather than a threshold: a panel resized back and forth
    /// across the middle of the band never flips.
    #[test]
    fn color_scale_orientation_is_sticky_inside_the_band() {
        // Seeded landscape, then resized to well inside the band (h/w = 1.25,
        // the ratio a 16:10 laptop's two-pane split used to sit at).
        let mut from_landscape = ColorScaleOrientation::default();
        assert!(!from_landscape.resolve(panel(1920.0, 1080.0)));
        assert!(!from_landscape.resolve(panel(960.0, 1200.0)), "1.25 is inside the band");
        assert!(!from_landscape.resolve(panel(1000.0, 1200.0)), "1.20, exactly the old threshold");
        assert!(!from_landscape.resolve(panel(1000.0, 1100.0)), "1.10, still inside");

        // Seeded portrait, walked through the identical ratios: it keeps the
        // *other* answer. Same input, different history — that is hysteresis.
        let mut from_portrait = ColorScaleOrientation::default();
        assert!(from_portrait.resolve(panel(1080.0, 2340.0)));
        assert!(from_portrait.resolve(panel(960.0, 1200.0)));
        assert!(from_portrait.resolve(panel(1000.0, 1200.0)));
        assert!(from_portrait.resolve(panel(1000.0, 1100.0)));

        // Only leaving the band flips it, in either direction.
        assert!(from_landscape.resolve(panel(1000.0, 1400.0)), "1.40 is clearly portrait");
        assert!(!from_portrait.resolve(panel(1000.0, 1000.0)), "1.00 is clearly not portrait");

        // …and the flip is *recorded*, not just returned. If the memory froze
        // at the seed, the band would be one-sided: the same in-band ratio
        // would keep answering with the original orientation, and the bars
        // would snap back the moment the resize came home.
        assert!(
            from_landscape.resolve(panel(1000.0, 1200.0)),
            "having flipped to horizontal, 1.20 must now keep it"
        );
        assert!(
            !from_portrait.resolve(panel(1000.0, 1200.0)),
            "having flipped to vertical, the same 1.20 must keep that instead"
        );
    }

    /// The seed ratio sits in the middle of the band, and both of its edges
    /// matter: a first panel at 1.12 (a 16:9 laptop's two-pane split) is
    /// vertical, one at 1.25 (16:10) is horizontal. Seeding at either band edge
    /// instead would move one of them.
    #[test]
    fn the_first_panel_is_seeded_from_the_middle_of_the_band() {
        assert!(
            !ColorScaleOrientation::default().resolve(panel(1000.0, 1120.0)),
            "1.12 is below the seed ratio"
        );
        assert!(
            ColorScaleOrientation::default().resolve(panel(1000.0, 1250.0)),
            "1.25 is above it"
        );
    }

    /// A panel that has not been laid out yet must not seed the memory.
    ///
    /// Both degenerate rects give a NaN ratio, which compares false against
    /// everything — so without the guard they quietly record "vertical", and
    /// the first *real* panel is then judged against the band's far edge
    /// instead of the seed ratio. The panel below is deliberately inside the
    /// band, where that difference shows.
    #[test]
    fn color_scale_orientation_ignores_a_degenerate_panel() {
        for degenerate in [egui::Rect::ZERO, egui::Rect::NOTHING] {
            let mut orientation = ColorScaleOrientation::default();
            assert!(!orientation.resolve(degenerate));
            assert!(
                orientation.resolve(panel(960.0, 1200.0)),
                "the first real panel must still be free to seed, even at 1.25 \
                 where only the seed ratio (not the band edge) says portrait"
            );

            // A degenerate rect arriving *later* — a collapsed or hidden panel
            // mid-session — must hand back what is remembered, not a default.
            // Answering `false` there would flip every bar for a frame.
            assert!(
                orientation.resolve(degenerate),
                "a degenerate panel must report the remembered orientation"
            );
            assert!(orientation.resolve(panel(960.0, 1200.0)), "and not have disturbed it");
        }
    }

    fn ts(minute: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, minute, 0)
            .unwrap()
    }

    /// A 1x1 texture handle. `egui::Context` allocates textures through its own
    /// texture manager, so this needs no window, GPU, or renderer.
    fn dummy_texture(ctx: &egui::Context) -> RadarImageData {
        let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
        RadarImageData {
            texture: ctx.load_texture("test", image, egui::TextureOptions::NEAREST),
            lat: 0.0,
            lon: 0.0,
            max_range_km: 100.0,
            value_data: Arc::new(Vec::new()),
        }
    }

    /// The site every test loop is built for, unless it is explicitly given another.
    const SITE: &str = "KTLX";

    /// A site value with the code and coordinates agreeing, as the real table has it.
    fn site(name: &'static str, lat: f64, lon: f64) -> RadarSite {
        RadarSite { name, lat, lon, elev: None }
    }

    fn loop_with_frames(count: usize, current_frame: usize) -> LoopPlaybackState {
        loop_for_site(&site(SITE, 35.0, -97.0), count, current_frame)
    }

    fn loop_for_site(site: &RadarSite, count: usize, current_frame: usize) -> LoopPlaybackState {
        let mut state = LoopPlaybackState::new_for_loop(3600, site);
        state.phase = LoopPhase::Rendering;
        state.frames = (0..count)
            .map(|i| LoopFrame {
                timestamp: ts(i as u32),
                texture: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        state.current_frame = current_frame;
        state
    }

    /// Every frame's scan has downloaded.
    fn all_scans_available(_: &LoopFrame) -> bool {
        true
    }

    /// The target a render result carries, as stamped by `spawn_loop_frame_render`.
    fn target(site: &str, product: RadarProduct, elevation: f32) -> RenderTarget {
        RenderTarget::new(site, product, elevation)
    }

    #[test]
    fn render_set_walks_outward_from_playhead() {
        let state = loop_with_frames(8, 0);
        // Forward first, then backward (wrapping), alternating.
        assert_eq!(state.render_set_indices(5), vec![0, 1, 7, 2, 6]);
    }

    #[test]
    fn render_set_is_capped_and_deduplicated() {
        let state = loop_with_frames(4, 2);
        let indices = state.render_set_indices(12);
        assert_eq!(indices.len(), 4, "cannot exceed the frame count");
        assert_eq!(
            indices.iter().copied().collect::<HashSet<_>>(),
            (0..4).collect::<HashSet<_>>(),
            "every frame covered exactly once"
        );

        assert!(state.render_set_indices(0).is_empty());
        assert!(loop_with_frames(0, 0).render_set_indices(6).is_empty());
    }

    /// Regression: the render budget is shared with static pane renders, so a loop
    /// batch can be starved — only some frames spawn, they finish, and for a moment
    /// nothing is in flight while most of the set is still blank. The old predicate
    /// ("no frame is in flight") called that ready and animated blank frames.
    #[test]
    fn starved_frames_block_readiness() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(4, 0);
        // One frame rendered; the rest never got a slot, so nothing is in flight.
        state.frames[0].texture = Some(dummy_texture(&ctx));

        assert!(
            !state.frames.iter().any(|f| f.render_in_flight),
            "precondition: the old 'nothing in flight' predicate would pass here"
        );
        assert!(
            !state.render_set_settled(12, all_scans_available),
            "frames that are pending but not yet spawned must block readiness"
        );
    }

    #[test]
    fn fully_rendered_batch_is_settled() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(4, 0);
        for frame in &mut state.frames {
            frame.texture = Some(dummy_texture(&ctx));
        }
        assert!(state.render_set_settled(12, all_scans_available));
    }

    #[test]
    fn in_flight_frames_block_readiness() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(3, 0);
        state.frames[0].texture = Some(dummy_texture(&ctx));
        state.frames[1].texture = Some(dummy_texture(&ctx));
        state.frames[2].render_in_flight = true;
        assert!(!state.render_set_settled(12, all_scans_available));
    }

    /// A frame whose scan has not downloaded cannot be rendered yet, so it must not
    /// block readiness — download progress is gated separately by the pending queue.
    #[test]
    fn undownloaded_frames_do_not_block_readiness() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(3, 0);
        state.frames[0].texture = Some(dummy_texture(&ctx));
        let downloaded = state.frames[0].timestamp;
        assert!(state.render_set_settled(12, |f| f.timestamp == downloaded));
    }

    /// A frame that has been ruled out (render attempted and produced nothing) must
    /// not block readiness forever, or the loop would wedge in `Rendering`.
    #[test]
    fn failed_frames_do_not_block_readiness() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(3, 0);
        state.frames[0].texture = Some(dummy_texture(&ctx));
        state.frames[1].render_failed = true;
        state.frames[2].render_failed = true;
        assert!(state.render_set_settled(12, all_scans_available));
    }

    /// Nothing has been rendered before the first dispatch, so adopting a target is
    /// not an invalidation.
    #[test]
    fn retarget_is_a_noop_before_the_first_dispatch() {
        let mut state = loop_with_frames(3, 0);
        assert!(state.rendered_for.is_none());
        assert!(!state.retarget_renders(RadarProduct::Reflectivity, 0.5));
        let adopted = state.rendered_for.as_ref().expect("target adopted");
        assert!(adopted.matches(&target(SITE, RadarProduct::Reflectivity, 0.5)));
    }

    #[test]
    fn retarget_keeps_frames_when_the_selection_is_unchanged() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Reflectivity, 0.5);
        state.frames[0].texture = Some(dummy_texture(&ctx));

        assert!(!state.retarget_renders(RadarProduct::Reflectivity, 0.5));
        assert!(state.frames[0].texture.is_some());
        // Elevation jitter below the tolerance used elsewhere is not a change.
        assert!(!state.retarget_renders(RadarProduct::Reflectivity, 0.505));
        assert!(state.frames[0].texture.is_some());
    }

    /// `texture` and `render_failed` are both judgements about one product at one
    /// elevation, and the pane's combo boxes can change that at any time. A frame
    /// retired under a product only some scans carry must come back when the user
    /// switches to a product every scan carries — otherwise it stays blank forever
    /// while readiness counts it as settled, and playback animates with holes.
    #[test]
    fn retarget_discards_frame_state_that_judged_the_old_product() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(4, 0);
        state.retarget_renders(RadarProduct::Velocity, 0.5);
        state.frames[0].texture = Some(dummy_texture(&ctx));
        // Retired because their scans carry no Velocity sweep. Readiness counts
        // retired frames as settled (see `failed_frames_do_not_block_readiness`),
        // so left alone these would animate as permanent holes under any product.
        state.frames[1].render_failed = true;
        state.frames[2].render_failed = true;
        // Still rendering Velocity when the user switches away.
        state.frames[3].render_in_flight = true;

        assert!(state.retarget_renders(RadarProduct::Reflectivity, 0.5));
        assert!(state.frames.iter().all(|f| f.texture.is_none()));
        assert!(state.frames.iter().all(|f| !f.render_failed));
        // In-flight renders are un-marked so their old-product results are rejected
        // on arrival rather than painted onto a retargeted frame.
        assert!(state.frames.iter().all(|f| !f.render_in_flight));

        // And the loop must render the whole set again before it can be Ready.
        assert!(!state.render_set_settled(12, all_scans_available));
    }

    #[test]
    fn retarget_reacts_to_an_elevation_change() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Reflectivity, 0.5);
        state.frames[0].texture = Some(dummy_texture(&ctx));

        assert!(state.retarget_renders(RadarProduct::Reflectivity, 1.5));
        assert!(state.frames[0].texture.is_none());
        let retargeted = state.rendered_for.as_ref().expect("target adopted");
        assert!(retargeted.matches(&target(SITE, RadarProduct::Reflectivity, 1.5)));
    }

    /// The render target is the *whole* key a frame's image is determined by, and the
    /// site is half the geometry: `render_radar_to_image` projects around the site's
    /// coordinates, so the same scan at the same product and elevation is a different
    /// image per site. Without the site in the key, "a loop frame's image is fully
    /// determined by (timestamp, product, elevation)" is simply false, and the target
    /// comparison stops being exact.
    #[test]
    fn a_result_rendered_for_another_site_is_rejected() {
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Reflectivity, 0.5);
        let frame_ts = state.frames[0].timestamp;
        state.frames[0].render_in_flight = true;

        assert_eq!(
            state.frame_awaiting_render_result(
                frame_ts,
                &target(SITE, RadarProduct::Reflectivity, 0.5)
            ),
            Some(0),
            "the loop's own site is accepted"
        );
        assert_eq!(
            state.frame_awaiting_render_result(
                frame_ts,
                &target("KOUN", RadarProduct::Reflectivity, 0.5)
            ),
            None,
            "an image projected around another site's coordinates must be rejected"
        );
    }

    /// The site-change path. Switching site tears the loop down and builds a new one
    /// (`LoopPlaybackState::new()` then `new_for_loop`), which is what closes this
    /// today — but only incidentally: once the new loop has listed its scans, adopted
    /// the same product/elevation and re-marked a frame in flight, an old render still
    /// running for the previous site would be accepted on nothing but a timestamp
    /// match. Two sites' volume times colliding to the second is unlikely, not
    /// impossible, and the frame-list contents are not ours to guarantee.
    #[test]
    fn a_rebuilt_loop_rejects_the_previous_sites_in_flight_result() {
        let mut old = loop_with_frames(3, 0);
        old.retarget_renders(RadarProduct::Reflectivity, 0.5);
        let frame_ts = old.frames[0].timestamp;
        old.frames[0].render_in_flight = true;
        let in_flight_target = old.rendered_for.clone().expect("dispatched target");

        // User switches site: the loop is rebuilt for the new site and reaches the
        // same state — same timestamp, same selection, frame dispatched again.
        let mut rebuilt = loop_for_site(&site("KOUN", 35.2, -97.5), 3, 0);
        rebuilt.retarget_renders(RadarProduct::Reflectivity, 0.5);
        rebuilt.frames[0].render_in_flight = true;

        assert_eq!(
            rebuilt.frames[0].timestamp, frame_ts,
            "precondition: the rebuilt loop lists a frame at the same timestamp"
        );
        assert_eq!(
            rebuilt.frame_awaiting_render_result(frame_ts, &in_flight_target),
            None,
            "the old site's render must not be painted onto the new site's frame"
        );
        assert_eq!(
            rebuilt.frame_awaiting_render_result(
                frame_ts,
                &target("KOUN", RadarProduct::Reflectivity, 0.5)
            ),
            Some(0),
            "the new site's own render is still accepted"
        );
    }

    /// The sibling broadcast hands one pane's finished texture to every other pane
    /// keyed to the same target, positioning it with the *receiving* pane's
    /// `site_lat`/`site_lon`. A pane whose loop geometry is another site would draw
    /// the image in the wrong place, so the site has to be part of that match too.
    #[test]
    fn a_sibling_on_another_site_does_not_accept_the_broadcast() {
        let mut sibling = loop_for_site(&site("KOUN", 35.2, -97.5), 3, 0);
        sibling.retarget_renders(RadarProduct::Reflectivity, 0.5);

        assert!(
            !sibling.is_rendered_for(&target(SITE, RadarProduct::Reflectivity, 0.5)),
            "same product and elevation, different geometry"
        );
        assert!(sibling.is_rendered_for(&target("KOUN", RadarProduct::Reflectivity, 0.5)));
    }

    /// The render target is compared on the site *code* while frames are projected
    /// with the site *coordinates*, so the two must come from one site value. If they
    /// could disagree every later comparison would be exact and wrong.
    #[test]
    fn a_loop_takes_its_code_and_its_coordinates_from_one_site() {
        let koun = site("KOUN", 35.23, -97.46);
        let state = LoopPlaybackState::new_for_loop(3600, &koun);

        assert_eq!(state.site, koun.name);
        assert_eq!(state.site_lat, koun.lat);
        assert_eq!(state.site_lon, koun.lon);
    }

    /// The dispatcher's donor search is a second, independent way one pane's image
    /// reaches another — it runs *before* rendering and suppresses the receiving
    /// pane's own render. It has to apply the same site test as the broadcast.
    #[test]
    fn a_donor_on_another_site_is_not_offered() {
        let ctx = egui::Context::default();
        let mut donor = loop_with_frames(3, 0);
        donor.retarget_renders(RadarProduct::Reflectivity, 0.5);
        donor.frames[0].texture = Some(dummy_texture(&ctx));
        let frame_ts = donor.frames[0].timestamp;

        assert_eq!(
            donor.frame_donatable_to(frame_ts, &target(SITE, RadarProduct::Reflectivity, 0.5)),
            Some(0),
            "a pane on the same target may take this texture"
        );
        assert_eq!(
            donor.frame_donatable_to(frame_ts, &target("KOUN", RadarProduct::Reflectivity, 0.5)),
            None,
            "a pane whose loop is on another site must render its own"
        );
    }

    /// The dispatcher suppresses a pane's own render on the promise that the queued
    /// render's result will be broadcast to it. If the donor test and the broadcast
    /// test disagree, that promise is broken and the frame is served by neither —
    /// blank forever, while readiness waits on it. They must agree frame for frame.
    #[test]
    fn donor_and_broadcast_agree_on_who_may_serve_a_frame() {
        let ctx = egui::Context::default();
        let mut donor = loop_with_frames(3, 0);
        donor.retarget_renders(RadarProduct::Reflectivity, 0.5);
        donor.frames[1].texture = Some(dummy_texture(&ctx));
        let frame_ts = donor.frames[1].timestamp;

        let same_site = loop_with_frames(3, 0);
        let mut same_site = same_site;
        same_site.retarget_renders(RadarProduct::Reflectivity, 0.5);

        let mut other_site = loop_for_site(&site("KOUN", 35.2, -97.5), 3, 0);
        other_site.retarget_renders(RadarProduct::Reflectivity, 0.5);

        for (label, receiver) in [("same site", &same_site), ("other site", &other_site)] {
            let offered = donor
                .frame_donatable_to(frame_ts, receiver.rendered_for.as_ref().unwrap())
                .is_some();
            let accepted = receiver
                .frame_accepting_broadcast(frame_ts, donor.rendered_for.as_ref().unwrap())
                .is_some();
            assert_eq!(
                offered, accepted,
                "{label}: donor offered={offered} but broadcast accepted={accepted}"
            );
        }

        // And the same-site pair really does transfer, so the agreement is not the
        // trivial "both always refuse".
        assert!(
            same_site
                .frame_accepting_broadcast(frame_ts, donor.rendered_for.as_ref().unwrap())
                .is_some()
        );
    }

    /// The donor mirror of `a_textured_frame_does_not_accept_a_broadcast`, and the
    /// guard is load-bearing in a way that does not announce itself: offering an
    /// untextured frame makes the dispatcher queue a clone and skip its own render,
    /// the clone then finds no texture to copy, and the frame ends up untextured, not
    /// in flight and not failed — which `render_set_settled` scores as unsettled, so
    /// the loop never reaches `Ready`. It cannot self-correct either, because a donor
    /// frame outside the donor's own render set is never rendered, so the empty offer
    /// repeats every pass. Exactly the "served by neither" failure the paired donor
    /// and acceptance tests exist to prevent.
    #[test]
    fn an_untextured_frame_is_not_donatable() {
        let ctx = egui::Context::default();
        let mut donor = loop_with_frames(3, 0);
        donor.retarget_renders(RadarProduct::Reflectivity, 0.5);
        let current = target(SITE, RadarProduct::Reflectivity, 0.5);
        let frame_ts = donor.frames[0].timestamp;

        assert_eq!(
            donor.frame_donatable_to(frame_ts, &current),
            None,
            "a blank frame has nothing to give"
        );
        // Being mid-render is not having an image either.
        donor.frames[0].render_in_flight = true;
        assert_eq!(donor.frame_donatable_to(frame_ts, &current), None);

        donor.frames[0].render_in_flight = false;
        donor.frames[0].texture = Some(dummy_texture(&ctx));
        assert_eq!(donor.frame_donatable_to(frame_ts, &current), Some(0));
    }

    /// A frame that already has an image gains nothing from an identical one, and
    /// overwriting it churns texture handles.
    #[test]
    fn a_textured_frame_does_not_accept_a_broadcast() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Reflectivity, 0.5);
        let current = target(SITE, RadarProduct::Reflectivity, 0.5);
        let frame_ts = state.frames[0].timestamp;

        assert_eq!(state.frame_accepting_broadcast(frame_ts, &current), Some(0));
        state.frames[0].texture = Some(dummy_texture(&ctx));
        assert_eq!(state.frame_accepting_broadcast(frame_ts, &current), None);
    }

    /// Single-frame mode keeps a `LoopPlaybackState` around with stale placeholder
    /// site fields. Nothing may be applied to it through any path.
    #[test]
    fn an_inactive_loop_takes_nothing_from_any_path() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Reflectivity, 0.5);
        let current = target(SITE, RadarProduct::Reflectivity, 0.5);
        let frame_ts = state.frames[0].timestamp;
        state.frames[0].render_in_flight = true;
        state.frames[1].texture = Some(dummy_texture(&ctx));
        let textured_ts = state.frames[1].timestamp;

        // Precondition: everything is accepted while the loop is active.
        assert!(state.frame_awaiting_render_result(frame_ts, &current).is_some());
        assert!(state.frame_donatable_to(textured_ts, &current).is_some());

        state.phase = LoopPhase::Inactive;

        assert_eq!(state.frame_awaiting_render_result(frame_ts, &current), None);
        assert_eq!(state.frame_accepting_broadcast(frame_ts, &current), None);
        assert_eq!(state.frame_donatable_to(textured_ts, &current), None);
    }

    /// The `&mut` forms are what the response path uses; they must resolve to the
    /// same frame the index forms name.
    #[test]
    fn the_mutable_accessors_hand_back_the_frame_that_was_chosen() {
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Reflectivity, 0.5);
        let current = target(SITE, RadarProduct::Reflectivity, 0.5);

        let shared = state.frames[0].timestamp;
        state.frames[2].timestamp = shared;
        state.frames[2].render_in_flight = true;

        let expected = state.frame_awaiting_render_result(shared, &current);
        assert_eq!(expected, Some(2));

        let frame = state
            .frame_awaiting_render_result_mut(shared, &current)
            .expect("frame handed back");
        frame.render_in_flight = false;
        // The mark was cleared on frame 2, not on the other frame with this timestamp.
        assert!(!state.frames[2].render_in_flight);
        assert_eq!(state.frame_awaiting_render_result(shared, &current), None);
    }

    /// The broadcast half of the same property. This is the accessor the response path
    /// actually calls, and duplicate timestamps are no more structurally prevented for
    /// it than for the render-result accessor.
    #[test]
    fn the_broadcast_accessor_hands_back_the_frame_that_was_chosen() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Reflectivity, 0.5);
        let current = target(SITE, RadarProduct::Reflectivity, 0.5);

        // Two frames at one timestamp, the first already textured — so the frame that
        // may take a broadcast is the *second*, not the one a plain lookup would reach.
        let shared = state.frames[0].timestamp;
        state.frames[2].timestamp = shared;
        state.frames[0].texture = Some(dummy_texture(&ctx));

        assert_eq!(
            state.frames.iter().position(|f| f.timestamp == shared),
            Some(0),
            "precondition: a timestamp-only lookup lands on the textured frame"
        );
        assert_eq!(state.frame_accepting_broadcast(shared, &current), Some(2));

        let frame = state
            .frame_accepting_broadcast_mut(shared, &current)
            .expect("frame handed back");
        frame.texture = Some(dummy_texture(&ctx));
        assert!(state.frames[2].texture.is_some(), "frame 2 received the texture");
        assert_eq!(
            state.frame_accepting_broadcast(shared, &current),
            None,
            "and nothing at this timestamp wants another"
        );
    }

    /// Elevation is still compared with tolerance, and the site exactly.
    #[test]
    fn target_matching_tolerates_elevation_jitter_only() {
        let base = target(SITE, RadarProduct::Reflectivity, 0.5);
        assert!(base.matches(&target(SITE, RadarProduct::Reflectivity, 0.505)));
        assert!(!base.matches(&target(SITE, RadarProduct::Reflectivity, 1.5)));
        assert!(!base.matches(&target(SITE, RadarProduct::Velocity, 0.5)));
        assert!(!base.matches(&target("KOUN", RadarProduct::Reflectivity, 0.5)));
    }

    /// Item 2: the accept check and the write must resolve to the same frame. The old
    /// shape asked "is *some* frame with this timestamp in flight?" and left the caller
    /// to fetch "the frame with this timestamp" — two lookups free to disagree, which
    /// would clear one frame and leave the dispatched one marked in flight forever.
    /// Returning the index makes disagreement unrepresentable.
    #[test]
    fn the_accepted_frame_is_the_one_that_is_in_flight() {
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Reflectivity, 0.5);

        // Two frames sharing a timestamp. Deduplication upstream makes this
        // unreachable today; nothing in this type enforces it.
        let shared = state.frames[0].timestamp;
        state.frames[2].timestamp = shared;
        state.frames[2].render_in_flight = true;

        assert_eq!(
            state.frames.iter().position(|f| f.timestamp == shared),
            Some(0),
            "precondition: a timestamp-only lookup lands on the wrong frame"
        );
        assert_eq!(
            state.frame_awaiting_render_result(
                shared,
                &target(SITE, RadarProduct::Reflectivity, 0.5)
            ),
            Some(2),
            "the result must be written to the frame that was actually dispatched"
        );
    }

    /// Eviction must keep exactly the render set. A rule that disagreed with the
    /// dispatcher would drop textures for frames about to be re-rendered.
    #[test]
    fn eviction_keeps_exactly_the_render_set() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(10, 4);
        for frame in &mut state.frames {
            frame.texture = Some(dummy_texture(&ctx));
        }

        state.evict_textures_outside_render_set(3);

        let textured: HashSet<usize> = state
            .frames
            .iter()
            .enumerate()
            .filter(|(_, f)| f.texture.is_some())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            textured,
            state.render_set_indices(3).into_iter().collect::<HashSet<_>>()
        );
        assert!(state.render_set_settled(3, all_scans_available));
    }

    /// The defect the in-flight mark alone cannot catch. `retarget_renders` un-marks
    /// the frame, but the *same* dispatch pass re-spawns it for the new target and
    /// marks it again — so when the older render finishes first (it started seconds
    /// earlier on the same workload) it arrives at a frame that is genuinely in
    /// flight. Only the target stamped on the result identifies it as stale. Left
    /// unchecked the frame keeps the previous product's image forever: the dispatcher
    /// skips textured frames, readiness counts it settled, and the newer result is
    /// then dropped because the frame is no longer marked.
    #[test]
    fn stale_result_is_rejected_after_the_frame_is_respawned() {
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Velocity, 0.5);
        let frame_ts = state.frames[0].timestamp;
        state.frames[0].render_in_flight = true; // render dispatched for Velocity

        // User switches product; the same dispatch pass re-spawns and re-marks.
        assert!(state.retarget_renders(RadarProduct::Reflectivity, 0.5));
        state.frames[0].render_in_flight = true;

        assert!(
            state.frames[0].render_in_flight,
            "precondition: an in-flight-only guard would accept the stale result here"
        );
        assert_eq!(
            state.frame_awaiting_render_result(frame_ts, &target(SITE, RadarProduct::Velocity, 0.5)),
            None,
            "a result for the abandoned target must be rejected"
        );
        assert_eq!(
            state.frame_awaiting_render_result(
                frame_ts,
                &target(SITE, RadarProduct::Reflectivity, 0.5)
            ),
            Some(0),
            "the re-dispatched render for the current target is still accepted"
        );
    }

    #[test]
    fn results_for_frames_not_awaiting_one_are_rejected() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(RadarProduct::Reflectivity, 0.5);
        let frame_ts = state.frames[0].timestamp;

        let current = target(SITE, RadarProduct::Reflectivity, 0.5);

        // Never dispatched, or already satisfied by a sibling pane's broadcast.
        assert_eq!(state.frame_awaiting_render_result(frame_ts, &current), None);
        state.frames[0].texture = Some(dummy_texture(&ctx));
        assert_eq!(state.frame_awaiting_render_result(frame_ts, &current), None);

        // A timestamp that is not in the frame list at all (list rebuilt since dispatch).
        state.frames[1].render_in_flight = true;
        assert_eq!(state.frame_awaiting_render_result(ts(59), &current), None);
    }

    /// Eviction now keeps only render-set members, where the previous rule kept the
    /// `budget` closest *textured* frames regardless of membership. Out-of-set
    /// textures are frames the dispatcher will never refresh, so this is deliberate;
    /// the visible effect is that scrubbing back to one blanks until it re-renders.
    #[test]
    fn eviction_drops_textured_frames_outside_the_render_set() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(10, 0);
        for idx in [2, 3, 4, 5] {
            state.frames[idx].texture = Some(dummy_texture(&ctx));
        }
        assert_eq!(state.render_set_indices(3), vec![0, 1, 9]);

        state.evict_textures_outside_render_set(3);

        assert!(
            state.frames.iter().all(|f| f.texture.is_none()),
            "none of the textured frames were in the render set"
        );
    }

    #[test]
    fn eviction_is_a_noop_within_budget() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(10, 0);
        // Textured, but deliberately far from the playhead and outside the render set.
        state.frames[5].texture = Some(dummy_texture(&ctx));
        state.frames[6].texture = Some(dummy_texture(&ctx));

        state.evict_textures_outside_render_set(3);

        assert!(state.frames[5].texture.is_some());
        assert!(state.frames[6].texture.is_some());
    }

    /// Frames outside the budgeted window around the playhead are never rendered,
    /// so they must not hold up readiness either.
    #[test]
    fn frames_outside_the_render_set_do_not_block_readiness() {
        let ctx = egui::Context::default();
        let mut state = loop_with_frames(10, 0);
        for &idx in &state.render_set_indices(3) {
            state.frames[idx].texture = Some(dummy_texture(&ctx));
        }
        assert!(state.render_set_settled(3, all_scans_available));
        assert!(
            !state.render_set_settled(10, all_scans_available),
            "widening the budget pulls blank frames back into the set"
        );
    }
}
