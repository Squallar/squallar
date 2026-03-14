use crate::actions::GuiAction;
use crate::pane::RadarImageData;
use crate::tiles::MapTileState;
use egui::Context;
use rustdar_radar::types::{RadarProduct, IMAGE_SIZE};

#[path = "ui_map_pane.rs"]
mod pane_render;

impl super::Gui {
    pub(super) fn render_map(&mut self, ctx: &Context) -> Vec<GuiAction> {
        use walkers::{Map, Position};

        let mut actions = Vec::new();

        // Detect current theme from egui context
        let is_dark_theme = ctx.style().visuals.dark_mode;

        // Initialize tiles via MapTileState
        self.map_tiles.ensure_base_tiles(is_dark_theme, ctx);
        let any_city_labels = MapTileState::any_city_labels(&self.panes);
        if any_city_labels {
            self.map_tiles.ensure_label_tiles(is_dark_theme, ctx);
        }

        // Take tiles out of self so they can be reborrowed per-pane in the loop.
        let mut tiles_owned = self.map_tiles.take_base_tiles();
        let mut label_tiles = if any_city_labels {
            self.map_tiles.take_label_tiles()
        } else {
            None
        };

        let pane_count = self.pane_layout.pane_count;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let panel_rect = ui.max_rect();

                self.detect_active_pane_click(ui.ctx(), panel_rect);

                // Snapshot viewport state before rendering for sync detection
                let (pre_zooms, pre_positions): (Vec<f64>, Vec<Option<Position>>) =
                    if self.viewport_sync && pane_count > 1 {
                        self.panes.iter().take(pane_count)
                            .map(|p| (p.map_memory.zoom(), p.map_memory.detached()))
                            .unzip()
                    } else {
                        (vec![], vec![])
                    };

                let pointer_available = self.dismiss_overlay_popups(ui.ctx());

                // Collect rects for floating UI elements that overlay the map
                // (e.g., hamburger button on Android). Clicks in these areas
                // must not trigger overlay polygon hit-tests.
                let excluded_rects: Vec<egui::Rect> = {
                    #[cfg(target_os = "android")]
                    {
                        let mut rects = Vec::new();
                        if !self.mobile.show_menu {
                            let top_inset = self.safe_area_insets.0;
                            rects.push(egui::Rect::from_min_size(
                                egui::pos2(12.0, 48.0 + top_inset),
                                egui::vec2(48.0, 48.0),
                            ));
                        }
                        rects
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        Vec::new()
                    }
                };

                for pane_idx in 0..pane_count {
                    let pane_rect = self.pane_layout.pane_rect(pane_idx, panel_rect);
                    let is_active = pane_idx == self.active_pane;

                    let mut pane = std::mem::take(&mut self.panes[pane_idx]);

                    // Determine the map center
                    let center = if let Some(scan_info) = &self.radar.scan_info {
                        Position::new(scan_info.site.lon, scan_info.site.lat)
                    } else {
                        Position::new(-98.5795, 39.8283) // Geographic center of contiguous USA
                    };

                    // Clone radar image data for use in closure
                    let radar_image = pane.radar_image.clone();

                    // Clone user location for use in closure
                    let user_location = self.user_location;

                    // Take map_memory out so Map::new borrows it independently
                    // of the pane fields used in the render closure.
                    let mut map_memory = std::mem::take(&mut pane.map_memory);

                    // On Android, process double-tap-drag zoom only for the active pane
                    #[cfg(target_os = "android")]
                    if is_active {
                        self.mobile.double_tap_detector.update(ctx, &mut map_memory);
                    }

                    #[cfg(target_os = "android")]
                    let is_zoom_dragging = if is_active {
                        self.mobile.double_tap_detector.is_zooming()
                    } else {
                        false
                    };
                    #[cfg(not(target_os = "android"))]
                    let is_zoom_dragging = false;

                    // On Android, detect long-press for radar value tooltip
                    #[cfg(target_os = "android")]
                    let long_press_pos = if is_active && !is_zoom_dragging {
                        self.mobile.long_press_detector.update(ctx)
                    } else {
                        None
                    };

                    #[cfg(target_os = "android")]
                    let suppress_pan = is_zoom_dragging || long_press_pos.is_some();
                    #[cfg(not(target_os = "android"))]
                    let suppress_pan = is_zoom_dragging;

                    // Create a child UI constrained to this pane's rect
                    let mut child_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(pane_rect)
                            .id_salt(("pane_map", pane_idx)),
                    );
                    child_ui.set_clip_rect(pane_rect);

                    if let Some(tiles) = tiles_owned.as_mut() {
                        Map::new(None, &mut map_memory, center)
                        .with_layer(tiles, 1.0)
                        .zoom_with_ctrl(false)
                        .panning(false)
                        .drag_pan_buttons(if suppress_pan {
                            egui::DragPanButtons::empty()
                        } else {
                            egui::DragPanButtons::PRIMARY
                        })
                        .show(&mut child_ui, |ui, projector, memory| {
                            let zoom = memory.zoom();

                            let mut render_ctx = pane_render::PaneRenderCtx {
                                pane_idx,
                                pane: &mut pane,
                                overlays: &mut self.overlays,
                                radar_image: &radar_image,
                                user_location,
                                label_tiles: &mut label_tiles,
                                actions: &mut actions,
                                pane_rect,
                                pointer_available,
                                is_dark_theme,
                                scan_info_site_name: self.radar.scan_info.as_ref().map(|i| i.site.name),
                                loading_site: &mut self.radar.loading_site,
                                excluded_rects: excluded_rects.clone(),
                                is_zoom_dragging,
                                #[cfg(target_os = "android")]
                                long_press_pos,
                            };

                            pane_render::render_pane_map_content(ui, projector, zoom, &mut render_ctx);
                        });
                    }

                    // Restore map_memory and pane
                    pane.map_memory = map_memory;
                    self.panes[pane_idx] = pane;

                    if pane_count > 1 {
                        draw_pane_border(ui, pane_rect, is_active);
                    }
                } // end pane loop

                // Handle divider dragging on a foreground layer so they
                // take priority over map panning in the overlap zone.
                if pane_count > 1 {
                    let divider_layer = egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("pane_dividers"),
                    );
                    let mut divider_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(panel_rect)
                            .layer_id(divider_layer),
                    );
                    self.pane_layout.handle_dividers(&mut divider_ui, panel_rect);
                }

                // Sync viewports: propagate the interacted pane's viewport to all others
                self.sync_viewports(pane_count, &pre_zooms, &pre_positions);
            });

        // Restore tiles and label tiles
        self.map_tiles.restore_base_tiles(tiles_owned);
        if any_city_labels {
            self.map_tiles.restore_label_tiles(label_tiles);
        }

        actions
    }

    /// Detect which pane was clicked and make it the active pane.
    fn detect_active_pane_click(&mut self, ctx: &Context, panel_rect: egui::Rect) {
        if self.pane_layout.pane_count <= 1 {
            return;
        }
        if let Some(pos) = ctx.input(|i| {
            if i.pointer.primary_pressed() {
                i.pointer.interact_pos()
            } else {
                None
            }
        }) {
            for idx in 0..self.pane_layout.pane_count {
                let rect = self.pane_layout.pane_rect(idx, panel_rect);
                if rect.contains(pos) && idx != self.active_pane {
                    self.active_pane = idx;
                    break;
                }
            }
        }
    }

    /// Dismiss overlay popups when clicking outside them.
    /// Returns `true` when no popup is open (pointer is available for map interaction).
    fn dismiss_overlay_popups(&mut self, ctx: &Context) -> bool {
        let pointer_available = self.overlays.selected_overlays.is_empty();
        if !pointer_available {
            let click_pos = ctx.input(|i| {
                if i.pointer.any_click() {
                    i.pointer.interact_pos()
                } else {
                    None
                }
            });
            if let Some(pos) = click_pos {
                let on_popup = ctx.layer_id_at(pos)
                    .is_some_and(|l| l.order > egui::Order::Background);
                if !on_popup {
                    self.overlays.selected_overlays.clear();
                    self.overlays.selected_overlay_page = 0;
                }
            }
        }
        pointer_available
    }
}

/// Draw a border around a pane rect, highlighted when active.
fn draw_pane_border(ui: &mut egui::Ui, pane_rect: egui::Rect, is_active: bool) {
    let border_color = if is_active {
        egui::Color32::from_rgb(60, 140, 255)
    } else {
        egui::Color32::from_rgba_unmultiplied(128, 128, 128, 100)
    };
    let stroke_width = if is_active { 2.0 } else { 1.0 };
    ui.painter().rect_stroke(
        pane_rect,
        0.0,
        egui::Stroke::new(stroke_width, border_color),
        egui::StrokeKind::Outside,
    );
}

/// Compute the hover information string for a cursor position over the radar image.
pub(super) fn compute_hover_info(
    img: &RadarImageData,
    hover_lat: f64,
    hover_lon: f64,
    hover_pos: egui::Pos2,
    rect: egui::Rect,
    product: RadarProduct,
) -> String {
    let lat1 = img.lat.to_radians();
    let lon1 = img.lon.to_radians();
    let lat2 = hover_lat.to_radians();
    let lon2 = hover_lon.to_radians();
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    let distance_km = 6371.0 * c;

    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    let azimuth = (y.atan2(x).to_degrees() + 360.0) % 360.0;

    let mut value_str = String::new();
    let frac_x = (hover_pos.x - rect.left()) / rect.width();
    let frac_y = (hover_pos.y - rect.top()) / rect.height();
    let px = (frac_x * IMAGE_SIZE as f32) as i32;
    let py = (frac_y * IMAGE_SIZE as f32) as i32;

    if px >= 0 && px < IMAGE_SIZE as i32 && py >= 0 && py < IMAGE_SIZE as i32 {
        let pixel_idx = py as usize * IMAGE_SIZE + px as usize;
        if pixel_idx < img.value_data.len() {
            let value = img.value_data[pixel_idx];
            if !value.is_nan() {
                value_str = product.format_value(value);
            }
        }
    }

    format!(
        "Lat: {:.4}°, Lon: {:.4}° | Range: {:.1}km, Az: {:.1}° {}",
        hover_lat, hover_lon, distance_km, azimuth, value_str
    )
}
