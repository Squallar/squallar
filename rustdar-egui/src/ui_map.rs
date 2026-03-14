use crate::actions::GuiAction;
use crate::layers::LayerKind;
use crate::pane::RadarImageData;
use crate::tiles::MapTileState;
use egui::Context;
use rustdar_radar::types::{ImageBounds, RadarProduct, IMAGE_SIZE, MAX_RANGE_KM};
use rustdar_radar::sites::RADARS;

use super::map_overlays::{OverlayDrawContext, draw_label_tiles_overlay};

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

                // Activate pane on primary press (before rendering so drag/pan
                // happens on the newly-active pane in the same frame).
                if pane_count > 1 {
                    if let Some(pos) = ui.ctx().input(|i| {
                        if i.pointer.primary_pressed() {
                            i.pointer.interact_pos()
                        } else {
                            None
                        }
                    }) {
                        for idx in 0..pane_count {
                            let rect = self.pane_layout.pane_rect(idx, panel_rect);
                            if rect.contains(pos) && idx != self.active_pane {
                                self.active_pane = idx;
                                break;
                            }
                        }
                    }
                }

                // Snapshot viewport state before rendering for sync detection
                let (pre_zooms, pre_positions): (Vec<f64>, Vec<Option<Position>>) =
                    if self.viewport_sync && pane_count > 1 {
                        self.panes.iter().take(pane_count)
                            .map(|p| (p.map_memory.zoom(), p.map_memory.detached()))
                            .unzip()
                    } else {
                        (vec![], vec![])
                    };

                // Dismiss overlay popups when clicking outside them (once, not per-pane)
                let pointer_available = self.overlays.selected_alert.is_none()
                    && self.overlays.selected_md.is_none();
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
                            self.overlays.selected_alert = None;
                            self.overlays.selected_md = None;
                        }
                    }
                }

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

                    let show_city_labels = pane.layers.is_enabled(LayerKind::CityLabels);

                    // On Android, process double-tap-drag zoom only for the active pane
                    #[cfg(target_os = "android")]
                    if is_active {
                        self.mobile.double_tap_detector.update(ctx, &mut pane.map_memory);
                    }

                    #[cfg(target_os = "android")]
                    let is_zoom_dragging = if is_active {
                        self.mobile.double_tap_detector.is_zooming()
                    } else {
                        false
                    };
                    #[cfg(not(target_os = "android"))]
                    let is_zoom_dragging = false;

                    // Create a child UI constrained to this pane's rect
                    let mut child_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(pane_rect)
                            .id_salt(("pane_map", pane_idx)),
                    );
                    child_ui.set_clip_rect(pane_rect);

                    if let Some(tiles) = tiles_owned.as_mut() {
                        Map::new(None, &mut pane.map_memory, center)
                        .with_layer(tiles, 1.0)
                        .zoom_with_ctrl(false)
                        .panning(false)
                        .drag_pan_buttons(if is_zoom_dragging {
                            egui::DragPanButtons::empty()
                        } else {
                            egui::DragPanButtons::PRIMARY
                        })
                        .show(&mut child_ui, |ui, projector, memory| {
                            let zoom = memory.zoom();

                            // Draw SPC outlook polygons (below radar)
                            let overlay_ctx = OverlayDrawContext::new(
                                ui,
                                projector,
                                zoom,
                                self.map_tiles.current_theme_is_dark,
                                pointer_available,
                            );
                            overlay_ctx.draw_spc_overlays(
                                &pane.layers,
                                &self.overlays.spc_outlooks.data,
                                &mut pane.spc_overlay_caches,
                                &self.overlays.spc_data_generation,
                            );

                            // Overlay radar data if available
                            if pane.layers.is_enabled(LayerKind::Radar) {
                            if let Some(ref img) =
                                radar_image
                            {
                                let bounds = pane.cached_image_bounds
                                    .unwrap_or_else(|| ImageBounds::from_radar_site(img.lat, img.lon));

                                let nw = projector.project(walkers::lat_lon(bounds.max_lat, bounds.min_lon)).to_pos2();
                                let se = projector.project(walkers::lat_lon(bounds.min_lat, bounds.max_lon)).to_pos2();
                                let rect = egui::Rect::from_two_pos(nw, se);

                                // Hover: only compute for the pane the cursor is in
                                if let Some(hover_pos) = ui.ctx().pointer_hover_pos() {
                                    if pane_rect.contains(hover_pos) {
                                    let pos_changed = pane.last_hover_pos
                                        .map(|last| (last - hover_pos).length() > 0.5)
                                        .unwrap_or(true);
                                    pane.last_hover_pos = Some(hover_pos);

                                    if pos_changed {
                                        let screen_vec = egui::vec2(hover_pos.x, hover_pos.y);
                                        let map_pos = projector.unproject(screen_vec);
                                        let hover_lat = map_pos.y();
                                        let hover_lon = map_pos.x();

                                        pane.hover_value = Some(compute_hover_info(
                                            img,
                                            hover_lat,
                                            hover_lon,
                                            hover_pos,
                                            rect,
                                            pane.selected_product,
                                        ));
                                    }
                                    } else {
                                        // Cursor not in this pane
                                        pane.last_hover_pos = None;
                                        pane.hover_value = None;
                                    }
                                } else {
                                    pane.last_hover_pos = None;
                                    pane.hover_value = None;
                                }

                                // Draw the radar image overlay
                                ui.painter().image(
                                    img.texture.id(),
                                    rect,
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );

                                // Draw a light grey circle showing the radar range
                                let radar_center = projector.project(walkers::lat_lon(img.lat, img.lon)).to_pos2();
                                let north_edge = projector.project(
                                    walkers::lat_lon(img.lat + MAX_RANGE_KM / 111.32, img.lon)
                                ).to_pos2();
                                let range_radius_pixels = (radar_center.y - north_edge.y).abs();
                                ui.painter().circle_stroke(
                                    radar_center,
                                    range_radius_pixels,
                                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(150, 150, 150, 80)),
                                );
                            }
                            } // end if radar layer enabled

                            // Draw SPC Mesoscale Discussion polygons
                            let clicked_md = overlay_ctx.draw_spc_discussions(
                                &pane.layers,
                                &self.overlays.spc_discussions.data,
                                &mut pane.spc_md_overlay_cache,
                                self.overlays.spc_discussions.data_generation,
                            );
                            if let Some(idx) = clicked_md {
                                self.overlays.selected_md = Some(idx);
                            }

                            // Draw NWS alert polygons
                            let clicked_alert = overlay_ctx.draw_nws_alerts(
                                &pane.layers,
                                &self.overlays.nws_alerts.data,
                                &self.overlays.hidden_alerts,
                                &mut pane.nws_overlay_cache,
                                self.overlays.nws_alerts.data_generation,
                            );
                            if let Some(idx) = clicked_alert {
                                self.overlays.selected_alert = Some(idx);
                            }

                            // Draw label-only tiles on top of the radar overlay
                            if show_city_labels {
                                if let Some(ref mut ltiles) = label_tiles {
                                    draw_label_tiles_overlay(ui, projector, memory.zoom(), ltiles);
                                }
                            }

                            // Draw radar site icons
                            if pane.layers.is_enabled(LayerKind::RadarSites) {
                                for radar_site in &RADARS {
                                    let site_position = walkers::lat_lon(radar_site.lat, radar_site.lon);
                                    let site_screen = projector.project(site_position).to_pos2();

                                    let screen_rect = ui.max_rect();
                                    if !screen_rect.expand(100.0).contains(site_screen) {
                                        continue;
                                    }

                                    let zoom = memory.zoom() as f32;
                                    let icon_size = (10.0 + zoom * 2.0).clamp(8.0, 24.0);

                                    let is_current_site = self.radar.scan_info.as_ref()
                                        .map(|info| info.site.name == radar_site.name)
                                        .unwrap_or(false);

                                    let is_loading = self.radar.loading_site.as_ref()
                                        .map(|loading| loading == radar_site.name)
                                        .unwrap_or(false);

                                    let icon_color = if is_loading {
                                        egui::Color32::from_rgb(160, 32, 240)
                                    } else if is_current_site {
                                        egui::Color32::from_rgb(255, 100, 100)
                                    } else {
                                        egui::Color32::from_rgb(100, 150, 255)
                                    };

                                    let icon_rect = egui::Rect::from_center_size(
                                        site_screen,
                                        egui::vec2(icon_size, icon_size)
                                    );

                                    let response = ui.allocate_rect(icon_rect, egui::Sense::click());

                                    if response.clicked() {
                                        self.radar.loading_site = Some(radar_site.name.to_string());
                                        actions.push(GuiAction::SwitchRadarSite(radar_site.name.to_string()));
                                    }

                                    ui.painter().circle_filled(site_screen, icon_size / 2.0, icon_color);

                                    ui.painter().circle_stroke(
                                        site_screen,
                                        icon_size / 2.0,
                                        egui::Stroke::new(1.5, egui::Color32::WHITE)
                                    );

                                    let text_color = if is_dark_theme {
                                        egui::Color32::WHITE
                                    } else {
                                        egui::Color32::BLACK
                                    };
                                    let font_size = (icon_size * 0.6).clamp(8.0, 12.0);

                                    let text_pos = egui::pos2(
                                        site_screen.x,
                                        site_screen.y + icon_size / 2.0 + 3.0,
                                    );

                                    ui.painter().text(
                                        text_pos,
                                        egui::Align2::CENTER_TOP,
                                        radar_site.name,
                                        egui::FontId::monospace(font_size),
                                        text_color,
                                    );

                                    if response.hovered() {
                                        let elev_str = match radar_site.elev {
                                            Some(e) => format!("{} ft", e),
                                            None => "N/A".to_string(),
                                        };
                                        let tooltip_text = format!("{}\nLat: {:.3}°, Lon: {:.3}°\nElev: {}",
                                            radar_site.name, radar_site.lat, radar_site.lon, elev_str);
                                        response.on_hover_text(tooltip_text);
                                    }
                                }
                            }

                            // Draw user location indicator (blue dot)
                            if let Some((user_lat, user_lon)) = user_location {
                                let user_screen = projector
                                    .project(walkers::lat_lon(user_lat, user_lon))
                                    .to_pos2();

                                let screen_rect = ui.max_rect();
                                if screen_rect.expand(50.0).contains(user_screen) {
                                    ui.painter().circle_filled(
                                        user_screen,
                                        14.0,
                                        egui::Color32::from_rgba_unmultiplied(30, 130, 255, 40),
                                    );
                                    ui.painter().circle_stroke(
                                        user_screen,
                                        7.0,
                                        egui::Stroke::new(2.5, egui::Color32::WHITE),
                                    );
                                    ui.painter().circle_filled(
                                        user_screen,
                                        7.0,
                                        egui::Color32::from_rgb(30, 130, 255),
                                    );
                                }
                            }
                        });
                    }

                    // Restore pane
                    self.panes[pane_idx] = pane;

                    // Draw pane border when multi-pane
                    if pane_count > 1 {
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
