use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::types::{EARTH_RADIUS_KM, IMAGE_SIZE, RadarProduct};
use rustdar_units::UserPreferences;

#[path = "ui_map_pane.rs"]
mod pane_render;

impl super::Gui {
    pub(super) fn render_map(
        &mut self,
        ui: &mut egui::Ui,
        excluded_rects: &[egui::Rect],
    ) -> Vec<GuiAction> {
        use walkers::{Map, Position};

        let mut actions = Vec::new();
        let ctx = ui.ctx().clone();

        // What the map was *handed*, so a test can check the chrome's rects
        // actually arrive here. They reach every click handler from
        // `PaneRenderCtx::excluded_rects` below.
        #[cfg(test)]
        {
            self.last_map_excluded_rects = excluded_rects.to_vec();
        }

        // Detect current theme from egui context
        let is_dark_theme = ctx.global_style().visuals.dark_mode;

        // Initialize tiles via MapTileState
        self.map_tiles.ensure_base_tiles(is_dark_theme, &ctx);
        // Visible panes only (`Gui::panes`): a pane remembered from a wider
        // split must not keep label-tile fetching alive.
        let any_city_labels = self
            .panes()
            .iter()
            .any(|p| p.is_overlay_enabled(OverlayKind::CityLabels));
        if any_city_labels {
            self.map_tiles.ensure_label_tiles(is_dark_theme, &ctx);
        }

        // Take tiles out of self so they can be reborrowed per-pane in the loop.
        let mut tiles_owned = self.map_tiles.take_base_tiles();
        let mut label_tiles = if any_city_labels {
            self.map_tiles.take_label_tiles()
        } else {
            None
        };

        // The visible slice's bound, not the layout's raw count: the loop below
        // indexes `self.panes[pane_idx]` directly, and `Gui::panes` documents
        // why slicing at `pane_layout.pane_count` alone could outrun the vector.
        let pane_count = self.visible_pane_count();
        // Resolved once for the frame, before the pane loop: every pane must
        // agree about what is pointing at the screen.
        let modality = self.layout.modality;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let panel_rect = ui.max_rect();
                #[cfg(test)]
                {
                    self.last_map_panel_rect = panel_rect;
                }

                // One color-scale orientation for the whole grid, resolved from
                // the panel (not from each pane's rect) so every pane on screen
                // agrees and dragging a divider cannot flip the bars. See
                // `ColorScaleOrientation`.
                let horizontal_color_scale = self.color_scale_orientation.resolve(panel_rect);

                self.detect_active_pane_click(ui.ctx(), panel_rect);

                // Snapshot viewport state before rendering for sync detection
                let (pre_zooms, pre_positions): (Vec<f64>, Vec<Option<Position>>) =
                    if self.viewport_sync && pane_count > 1 {
                        self.panes
                            .iter()
                            .take(pane_count)
                            .map(|p| (p.map_memory.zoom(), p.map_memory.detached()))
                            .unzip()
                    } else {
                        (vec![], vec![])
                    };

                let pointer_available = self.dismiss_overlay_popups(ui.ctx());

                // Rects of floating chrome drawn over the map (the hamburger).
                // Clicks there must not become overlay polygon hit-tests.
                //
                // Supplied by the chrome that drew them rather than rebuilt
                // here from a second copy of its position constants — the two
                // copies could disagree silently, leaving a dead zone at the
                // old position and a live one under the button.

                for pane_idx in 0..pane_count {
                    let pane_rect = self.pane_layout.pane_rect(pane_idx, panel_rect);
                    let is_active = pane_idx == self.active_pane;

                    let mut pane = std::mem::take(&mut self.panes[pane_idx]);

                    // Determine the map center
                    let center = if let Some(scan_info) = &pane.scan_info {
                        Position::new(scan_info.site.lon, scan_info.site.lat)
                    } else {
                        Position::new(-98.5795, 39.8283) // Geographic center of contiguous USA
                    };

                    // Clone user location and heading for use in closure
                    let user_location = self.user_fix.as_ref().map(|f| (f.latitude, f.longitude));
                    let user_heading = self.gps_config.heading_source.effective_heading(
                        self.user_heading,
                        self.user_fix.as_ref().and_then(|f| f.heading_deg),
                        self.user_fix.as_ref().and_then(|f| f.speed_mps),
                    );
                    let user_fix = self.user_fix.clone();

                    // Take map_memory out so Map::new borrows it independently
                    // of the pane fields used in the render closure.
                    let mut map_memory = std::mem::take(&mut pane.map_memory);

                    // Resolve this pane's pointer state for the frame. Which
                    // pipeline runs is a *runtime* decision, taken once per
                    // frame by `LayoutCtx` and enforced by `InteractionState`:
                    // - Mouse: egui's built-in click detection (instant)
                    // - Touch: the gesture pipeline for the active pane
                    //   (deferred single-tap so double-tap-to-zoom doesn't open
                    //   popups, plus zoom-drag and long-press)
                    //
                    // Both paths run the click position through the canonical
                    // dialog-blocking gate (`ui_input::filter_dialog_blocked`),
                    // which discards clicks landing on a floating dialog or
                    // popup window. All handlers that receive overlay_click_pos
                    // from PaneRenderCtx automatically inherit this protection.
                    //
                    // CONVENTION: New map click handlers MUST use overlay_click_pos from
                    // PaneRenderCtx — never read raw click events via ctx.input() for
                    // map-level interactions, as that bypasses dialog blocking.
                    let pointer = if is_active {
                        self.interaction
                            .resolve_active(&ctx, modality, &mut map_memory, pane_rect)
                    } else {
                        self.interaction.resolve_inactive(&ctx, modality)
                    };

                    let overlay_click_pos = pointer.overlay_click_pos;
                    let suppress_pan = pointer.suppress_pan;

                    // From the same locals that feed `PaneRenderCtx` and
                    // `drag_pan_buttons` below: after the gate, after
                    // `overlay_click_pos` is read out. See `PanePointerProbe`.
                    #[cfg(test)]
                    self.last_pane_pointers
                        .push(crate::ui_input::PanePointerProbe {
                            pane_idx,
                            is_active,
                            modality,
                            frame: crate::ui_input::MapPointerFrame {
                                overlay_click_pos,
                                long_press_pos: pointer.long_press_pos,
                                suppress_pan,
                            },
                        });

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
                            // `zoom_with_ctrl(false)` is what puts us on walkers'
                            // raw-scroll zoom path, and walkers 0.55 changed that
                            // path's frame-time multiplier from
                            // `stable_dt.max(predicted_dt * 1.5)` to
                            // `stable_dt.clamp(predicted_dt * 0.5, predicted_dt * 2.0)`.
                            // At a steady frame rate that is a uniform x0.667 on the
                            // scroll-zoom step (60Hz: 0.025 -> 0.01667, so a wheel
                            // notch that gave ~1.31x now gives ~1.21x); on a hitched
                            // frame the old form grew unbounded and the new one is
                            // capped, which is the bug being fixed.
                            //
                            // `Map::zoom_speed` (default 2.0) can compensate the
                            // magnitude, but it is not an exact undo: it scales the
                            // combined zoom delta, so pinch and double-click zoom
                            // move with it. Left at the default deliberately.
                            .zoom_with_ctrl(false)
                            .panning(false)
                            .drag_pan_buttons(if suppress_pan {
                                egui::DragPanButtons::empty()
                            } else {
                                egui::DragPanButtons::PRIMARY
                            })
                            .show(&mut child_ui, |ui, _response, projector, memory| {
                                let zoom = memory.zoom();

                                let mut render_ctx = pane_render::PaneRenderCtx {
                                    pane_idx,
                                    pane: &mut pane,
                                    overlays: &mut self.overlays,
                                    user_location,
                                    user_heading,
                                    user_fix: user_fix.clone(),
                                    label_tiles: &mut label_tiles,
                                    actions: &mut actions,
                                    pane_rect,
                                    horizontal_color_scale,
                                    pointer_available,
                                    excluded_rects: excluded_rects.to_vec(),
                                    long_press_pos: pointer.long_press_pos,
                                    overlay_click_pos,
                                    preferences: &self.preferences,
                                };

                                pane_render::render_pane_map_content(
                                    ui,
                                    projector,
                                    zoom,
                                    &mut render_ctx,
                                );
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
                    let divider_layer =
                        egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pane_dividers"));
                    let mut divider_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(panel_rect)
                            .layer_id(divider_layer),
                    );
                    self.pane_layout
                        .handle_dividers(&mut divider_ui, panel_rect);
                }

                // Sync viewports: propagate the interacted pane's viewport to all others
                self.sync_viewports(&pre_zooms, &pre_positions);
            });

        // Restore tiles and label tiles
        self.map_tiles.restore_base_tiles(tiles_owned);
        if any_city_labels {
            self.map_tiles.restore_label_tiles(label_tiles);
        }

        actions
    }

    /// Detect which pane was clicked and make it the active pane.
    fn detect_active_pane_click(&mut self, ctx: &egui::Context, panel_rect: egui::Rect) {
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
            // Don't switch panes when the click lands on a floating dialog or popup.
            if ctx
                .layer_id_at(pos)
                .is_some_and(|l| l.order > egui::Order::Background)
            {
                return;
            }
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
    fn dismiss_overlay_popups(&mut self, ctx: &egui::Context) -> bool {
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
                let on_popup = ctx
                    .layer_id_at(pos)
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

/// Context for computing hover info from radar value data.
pub(super) struct HoverInput {
    pub site_lat: f64,
    pub site_lon: f64,
    pub hover_lat: f64,
    pub hover_lon: f64,
    pub hover_pos: egui::Pos2,
    pub rect: egui::Rect,
}

/// Compute hover info string from raw value data and site coordinates.
pub(super) fn compute_hover_info_raw(
    value_data: &[f32],
    input: &HoverInput,
    product: RadarProduct,
    prefs: &UserPreferences,
) -> String {
    let lat1 = input.site_lat.to_radians();
    let lon1 = input.site_lon.to_radians();
    let lat2 = input.hover_lat.to_radians();
    let lon2 = input.hover_lon.to_radians();
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    let distance_km = EARTH_RADIUS_KM * c;

    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    let azimuth = (y.atan2(x).to_degrees() + 360.0) % 360.0;

    let mut value_str = String::new();
    let frac_x = (input.hover_pos.x - input.rect.left()) / input.rect.width();
    let frac_y = (input.hover_pos.y - input.rect.top()) / input.rect.height();
    let px = (frac_x * IMAGE_SIZE as f32) as i32;
    let py = (frac_y * IMAGE_SIZE as f32) as i32;

    if px >= 0 && px < IMAGE_SIZE as i32 && py >= 0 && py < IMAGE_SIZE as i32 {
        let pixel_idx = py as usize * IMAGE_SIZE + px as usize;
        if pixel_idx < value_data.len() {
            let value = value_data[pixel_idx];
            if !value.is_nan() {
                value_str = format!("| {}", product.format_value(value, prefs));
            }
        }
    }

    let distance = prefs.distance.convert_from_km(distance_km);

    format!(
        "Lat: {:.4}\u{b0}, Lon: {:.4}\u{b0} | Range: {:.1}{}, Az: {:.1}\u{b0} {}",
        input.hover_lat,
        input.hover_lon,
        distance,
        prefs.distance.suffix(),
        azimuth,
        value_str
    )
}
