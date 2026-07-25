use std::collections::HashMap;
use rustdar_overlays::render::overlay_state::OverlayKind;
use crate::overlay_cache::OverlayTextureCache;
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

/// Tolerance for comparing two selected elevation angles, matching the snapping
/// tolerance the render dispatcher uses when picking a sweep for a selection.
const ELEVATION_TOLERANCE: f32 = 0.01;

/// Everything besides a frame's timestamp that determines the image a loop frame
/// renders to: the radar site whose coordinates set the projection, and the
/// product/elevation selection that picks the sweep.
///
/// This is the render target key. It is stored on `LoopPlaybackState::rendered_for`,
/// stamped onto every dispatched render, and compared on arrival so a result
/// produced for one target is never painted onto frames keyed to another.
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

    /// Whether two targets name the same image. Site and product are exact;
    /// elevation is compared within `ELEVATION_TOLERANCE`, since the selection is
    /// an `f32` that round-trips through the UI and the scan's own sweep angles.
    pub fn matches(&self, other: &RenderTarget) -> bool {
        self.site == other.site
            && self.product == other.product
            && (self.elevation - other.elevation).abs() <= ELEVATION_TOLERANCE
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
    /// The site rides along so the target is the *whole* key a frame's image is
    /// determined by. A loop is rebuilt from scratch when the pane changes site, so
    /// this half never moves under a live loop — it exists to make results and
    /// sibling broadcasts carrying another site's geometry impossible to apply,
    /// rather than merely improbable.
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
    /// `site`, `site_lat` and `site_lon` must come from the same site lookup: the
    /// coordinates are what frames are rendered with, and the code is what identifies
    /// them in the render target.
    pub fn new_for_loop(
        lookback_secs: u64,
        site: impl Into<String>,
        site_lat: f64,
        site_lon: f64,
    ) -> Self {
        Self {
            phase: LoopPhase::FetchingScanList,
            current_frame: 0,
            frames: Vec::new(),
            lookback_secs,
            last_advance: None,
            site: site.into(),
            site_lat,
            site_lon,
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
    ///   that *is* in flight. Since a loop frame's image is fully determined by
    ///   (timestamp, site, product, elevation), comparing the target is exact — a late
    ///   result that still matches the current target is byte-identical to the pending
    ///   one and safe to apply.
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
        if !self.is_rendered_for(target) {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.render_in_flight)
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
        let target = RenderTarget::new(self.site.clone(), product, elevation);
        if self.is_rendered_for(&target) {
            return false;
        }

        // Nothing to discard before the first dispatch — frames start blank.
        let had_previous_target = self.rendered_for.is_some();
        self.rendered_for = Some(target);
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

    fn loop_with_frames(count: usize, current_frame: usize) -> LoopPlaybackState {
        loop_for_site(SITE, count, current_frame)
    }

    fn loop_for_site(site: &str, count: usize, current_frame: usize) -> LoopPlaybackState {
        let mut state = LoopPlaybackState::new_for_loop(3600, site, 35.0, -97.0);
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
        let mut rebuilt = loop_for_site("KOUN", 3, 0);
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
        let mut sibling = loop_for_site("KOUN", 3, 0);
        sibling.retarget_renders(RadarProduct::Reflectivity, 0.5);

        assert!(
            !sibling.is_rendered_for(&target(SITE, RadarProduct::Reflectivity, 0.5)),
            "same product and elevation, different geometry"
        );
        assert!(sibling.is_rendered_for(&target("KOUN", RadarProduct::Reflectivity, 0.5)));
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
