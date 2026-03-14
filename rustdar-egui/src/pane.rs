use rustdar_overlays::render::layers::LayerManager;
use crate::overlay_cache::OverlayLayerCache;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct};
use rustdar_radar::types::{ImageBounds, RadarProduct, ScanInfo};
use std::collections::HashMap;
use std::sync::Arc;
use walkers::MapMemory;

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

/// Per-pane state: each pane independently selects a radar product,
/// elevation, layer toggles, and maintains its own map viewport.
pub struct PaneState {
    pub selected_product: RadarProduct,
    pub selected_elevation: f32,
    pub radar_image: Option<RadarImageData>,
    pub cached_image_bounds: Option<ImageBounds>,
    pub hover_value: Option<String>,
    pub last_hover_pos: Option<egui::Pos2>,
    pub layers: LayerManager,
    pub map_memory: MapMemory,
    // Per-pane overlay projection caches (viewport-dependent).
    pub spc_overlay_caches: HashMap<(OutlookDay, OutlookProduct), OverlayLayerCache>,
    pub nws_overlay_cache: OverlayLayerCache,
    pub spc_md_overlay_cache: OverlayLayerCache,
}

impl PaneState {
    pub fn new() -> Self {
        let mut map_memory = MapMemory::default();
        let _ = map_memory.set_zoom(4.0);
        Self {
            selected_product: RadarProduct::Reflectivity,
            selected_elevation: 0.0,
            radar_image: None,
            cached_image_bounds: None,
            hover_value: None,
            last_hover_pos: None,
            layers: LayerManager::new(),
            map_memory,
            spc_overlay_caches: HashMap::new(),
            nws_overlay_cache: OverlayLayerCache::new(),
            spc_md_overlay_cache: OverlayLayerCache::new(),
        }
    }

    /// Get rendering params for this pane (product + closest elevation).
    pub fn get_rendering_params(
        &self,
        scan_info: Option<&ScanInfo>,
    ) -> Option<(RadarProduct, f32)> {
        scan_info.and_then(|si| {
            si.product_elevations
                .get(&self.selected_product)
                .and_then(|elevations| {
                    elevations
                        .iter()
                        .min_by(|a, b| {
                            ((**a - self.selected_elevation).abs())
                                .partial_cmp(&((**b - self.selected_elevation).abs()))
                                .unwrap()
                        })
                        .copied()
                })
                .map(|elev_angle| (self.selected_product, elev_angle))
        })
    }

    /// Set the radar image to display on the map.
    pub fn set_radar_image(
        &mut self,
        texture: egui::TextureHandle,
        lat: f64,
        lon: f64,
        max_range_km: f64,
        value_data: Vec<f32>,
    ) {
        self.radar_image = Some(RadarImageData {
            texture,
            lat,
            lon,
            max_range_km,
            value_data: Arc::new(value_data),
        });
        self.cached_image_bounds = Some(ImageBounds::from_radar_site(lat, lon));
    }

    /// Clear the radar image.
    pub fn clear_radar_image(&mut self) {
        self.radar_image = None;
        self.cached_image_bounds = None;
    }

    /// Take the radar image, removing it from this pane.
    pub fn take_radar_image(&mut self) -> Option<RadarImageData> {
        self.radar_image.take()
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
