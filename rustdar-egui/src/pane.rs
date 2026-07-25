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
    /// Radar site latitude, captured at loop creation for rendering.
    pub site_lat: f64,
    /// Radar site longitude, captured at loop creation for rendering.
    pub site_lon: f64,
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
    pub fn new() -> Self {
        Self {
            phase: LoopPhase::Inactive,
            current_frame: 0,
            frames: Vec::new(),
            lookback_secs: 0,
            last_advance: None,
            site_lat: 0.0,
            site_lon: 0.0,
        }
    }

    /// Create a new initialized loop state starting the fetch phase.
    pub fn new_for_loop(lookback_secs: u64, site_lat: f64, site_lon: f64) -> Self {
        Self {
            phase: LoopPhase::FetchingScanList,
            current_frame: 0,
            frames: Vec::new(),
            lookback_secs,
            last_advance: None,
            site_lat,
            site_lon,
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

    fn loop_with_frames(count: usize, current_frame: usize) -> LoopPlaybackState {
        let mut state = LoopPlaybackState::new_for_loop(3600, 35.0, -97.0);
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
