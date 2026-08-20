use super::map_overlays::draw_tile_layer;
use crate::actions::GuiAction;
use rustdar_radar::hover::{HoverSource, Reading};
use rustdar_radar::types::{RadarProduct, RenderView};
use rustdar_source::id::known;
use rustdar_units::UserPreferences;

#[path = "ui_map_pane.rs"]
mod pane_render;

#[path = "ui_section_pane.rs"]
pub(crate) mod section_render;

#[path = "ui_volume_alpha.rs"]
pub(crate) mod volume_alpha_editor;

/// What a cross-section pane says while it has nothing to show.
pub(crate) const CROSS_SECTION_EMPTY_STATE: &str =
    "Draw a line on a map pane to cut a cross-section";

/// What a 3D pane says while it has nothing to show.
pub(crate) const VOLUME_EMPTY_STATE: &str = "3D volume view unavailable";

/// The header over the 3D pane's sidebar block. Icon, two spaces, name — the
/// same shape as [`super::SECTION_SIDEBAR_HEADER`] and the overlay rows'
/// labels, which is what keeps the block reading as part of the one panel.
pub(crate) const VOLUME_SIDEBAR_HEADER: &str = "\u{26f6}  3D view";

/// The Map floor checkbox's label.
pub(crate) const MAP_FLOOR_LABEL: &str = "Map floor";

/// What the sidebar says when the Map floor checkbox cannot produce anything.
pub(crate) const MAP_FLOOR_INERT_NOTE: &str =
    "No floor yet - nothing is being drawn to stand it under:";

/// The headline of a pane's empty-state reason: everything up to the first
/// line break, trimmed.
fn reason_headline(reason: &str) -> &str {
    reason.lines().next().unwrap_or(reason).trim()
}

impl super::Gui {
    /// Draw every visible pane, whatever kind each one is.
    pub(super) fn render_panes(
        &mut self,
        ui: &mut egui::Ui,
        excluded_rects: &[egui::Rect],
    ) -> Vec<GuiAction> {
        use walkers::{Map, Position};

        let mut actions = Vec::new();
        let ctx = ui.ctx().clone();

        #[cfg(test)]
        {
            self.probes.last_map_excluded_rects = excluded_rects.to_vec();
        }

        let is_dark_theme = ctx.global_style().visuals.dark_mode;

        self.map_tiles.ensure_base_tiles(is_dark_theme, &ctx);
        let any_city_labels = self
            .panes()
            .iter()
            .any(|p| p.draws_ground() && p.is_overlay_enabled(&known::CITY_LABELS));
        if any_city_labels {
            self.map_tiles.ensure_label_tiles(is_dark_theme, &ctx);
        }

        let mut tiles_owned = self.map_tiles.take_base_tiles();
        let mut label_tiles = if any_city_labels {
            self.map_tiles.take_label_tiles()
        } else {
            None
        };

        let pane_count = self.visible_pane_count();
        let modality = self.layout.modality;
        let tile_zoom_biases: Vec<u8> = (0..pane_count)
            .map(|idx| self.tile_zoom_bias_for_pane(idx))
            .collect();

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let panel_rect = ui.max_rect();
                #[cfg(test)]
                {
                    self.probes.last_map_panel_rect = panel_rect;
                }

                let horizontal_color_scale = self.color_scale_orientation.resolve(panel_rect);

                let color_scale_floor = panel_rect.bottom() - self.phone_bar_height;

                self.detect_active_pane_click(ui.ctx(), panel_rect);

                let (pre_zooms, pre_positions): (Vec<f64>, Vec<Option<Position>>) =
                    if pane_count > 1 {
                        self.panes
                            .iter()
                            .take(pane_count)
                            .map(|p| (p.map_memory.zoom(), p.map_memory.detached()))
                            .unzip()
                    } else {
                        (vec![], vec![])
                    };

                let pointer_available = self.dismiss_overlay_popups(ui.ctx());

                let mut click_consumed = false;
                let mut fade_candidate = false;

                let floors: Vec<bool> = self.panes[..pane_count]
                    .iter()
                    .map(|pane| pane.volume().is_some_and(|volume| !volume.hide_floor))
                    .collect();
                self.map_pane_geo
                    .retain(|&idx, _| floors.get(idx).copied().unwrap_or(false));

                self.volume_empty_states.clear();

                let (floor_strips, mirror_size_points) = floor_strip_plan(
                    ui.ctx().viewport_rect(),
                    &(0..pane_count)
                        .map(|idx| floors[idx].then(|| self.pane_layout.pane_rect(idx, panel_rect)))
                        .collect::<Vec<_>>(),
                );
                self.mirror_size_points = mirror_size_points;

                for pane_idx in 0..pane_count {
                    let pane_rect = self.pane_layout.pane_rect(pane_idx, panel_rect);
                    let is_active = pane_idx == self.active_pane;

                    let mut pane = std::mem::take(&mut self.panes[pane_idx]);

                    let center = if let Some(scan_info) = &pane.scan_info {
                        Position::new(scan_info.site.lon, scan_info.site.lat)
                    } else if let Some(site) = rustdar_radar::sites::get_radar_site(pane.site()) {
                        Position::new(site.lon, site.lat)
                    } else {
                        Position::new(-98.5795, 39.8283) // Geographic center of contiguous USA
                    };

                    let user_location = self.user_fix.as_ref().map(|f| (f.point.lat, f.point.lon));
                    let user_heading = self.heading_source.effective_heading(
                        self.user_heading,
                        self.user_fix.as_ref().and_then(|f| f.heading_deg),
                        self.user_fix.as_ref().and_then(|f| f.speed_mps),
                    );
                    let user_fix = self.user_fix.clone();

                    let mut map_memory = std::mem::take(&mut pane.map_memory);

                    let armed_draw = (self.section_draw_armed() || self.region_pick_armed())
                        && is_active
                        && pane.is_map();
                    let (pointer, gesture) = if armed_draw {
                        let armed = self.interaction.resolve_armed(&ctx, modality);
                        (armed.pointer(), Some(armed.gesture()))
                    } else if is_active {
                        (
                            self.interaction.resolve_active(
                                &ctx,
                                modality,
                                self.layout.width == crate::ui_layout::WidthClass::Compact,
                                &mut map_memory,
                                pane_rect,
                            ),
                            None,
                        )
                    } else {
                        (self.interaction.resolve_inactive(&ctx, modality), None)
                    };

                    let overlay_click_pos = pointer.overlay_click_pos;
                    if overlay_click_pos.is_some() {
                        self.pill_revealed = None;
                    }
                    let section_editing = self
                        .section_edit_drag
                        .as_ref()
                        .is_some_and(|d| d.map_pane == pane_idx);
                    let handle_press = !armed_draw
                        && !section_editing
                        && self.section_handle_pressed(&ctx, pane_idx);
                    let suppress_pan = pointer.suppress_pan || section_editing || handle_press;

                    if is_active
                        && pointer_available
                        && pane.is_map()
                        && !section_editing
                        && !handle_press
                        && self.fade_gesture_allowed()
                        && overlay_click_pos.is_some_and(|pos| pane_rect.contains(pos))
                    {
                        fade_candidate = true;
                    }

                    #[cfg(test)]
                    self.probes
                        .last_pane_pointers
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

                    let mut child_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(pane_rect)
                            .id_salt(("pane_map", pane_idx)),
                    );
                    child_ui.set_clip_rect(pane_rect);

                    match pane.render_view() {
                        RenderView::PlanView => {
                            self.record_pane_content(pane_idx, RenderView::PlanView, pane_rect);
                            let tile_zoom_bias =
                                tile_zoom_biases.get(pane_idx).copied().unwrap_or(0);
                            if let Some(tiles) = tiles_owned.as_mut() {
                                let ctx = child_ui.ctx().clone();
                                crate::ui_region::steady_wheel(&ctx, || {
                                    Map::new(None, &mut map_memory, center)
                                        .zoom_with_ctrl(false)
                                        .panning(false)
                                        .drag_pan_buttons(if suppress_pan {
                                            egui::DragPanButtons::empty()
                                        } else {
                                            egui::DragPanButtons::PRIMARY
                                        })
                                        .show(&mut child_ui, |ui, _response, projector, memory| {
                                            let zoom = memory.zoom();

                                            draw_tile_layer(
                                                ui,
                                                projector,
                                                zoom,
                                                tiles,
                                                tile_zoom_bias,
                                            );

                                            if let Some(gesture) = gesture {
                                                if self.section_draw_armed() {
                                                    self.track_section_draw(
                                                        pane_idx, gesture, projector,
                                                    );
                                                } else if self.region_pick_armed() {
                                                    self.track_region_pick(
                                                        pane_idx, gesture, projector,
                                                    );
                                                }
                                            }

                                            let mut render_ctx = pane_render::PaneRenderCtx {
                                                pane_idx,
                                                pane: &mut pane,
                                                overlays: &mut self.overlays,
                                                user_location,
                                                user_heading,
                                                user_fix: user_fix.clone(),
                                                label_tiles: &mut label_tiles,
                                                tile_zoom_bias,
                                                actions: &mut actions,
                                                pane_rect,
                                                surfaces: pane_render::PaneSurfaces::GroundAndGlass,
                                                horizontal_color_scale,
                                                color_scale_floor,
                                                pointer_available,
                                                excluded_rects: excluded_rects.to_vec(),
                                                long_press_pos: pointer.long_press_pos,
                                                overlay_click_pos,
                                                click_consumed: &mut click_consumed,
                                                preferences: &self.preferences,
                                                #[cfg(test)]
                                                paint_order: Vec::new(),
                                            };

                                            pane_render::render_pane_map_content(
                                                ui,
                                                projector,
                                                zoom,
                                                &mut render_ctx,
                                            );

                                            #[cfg(test)]
                                            self.probes.last_paint_order.push((
                                                pane_idx,
                                                std::mem::take(&mut render_ctx.paint_order),
                                            ));

                                            self.track_section_edit(
                                                ui,
                                                projector,
                                                pane_idx,
                                                pane_rect,
                                                excluded_rects,
                                            );

                                            self.draw_section_tracks(
                                                ui, projector, pane_idx, pane_rect,
                                            );

                                            self.draw_region_boxes(ui, projector, pane_idx);
                                        });
                                });
                            }
                        }
                        RenderView::CrossSection => {
                            self.record_pane_content(pane_idx, RenderView::CrossSection, pane_rect);
                            let top_clearance =
                                crate::ui::pills::pill_row_clearance(child_ui.ctx(), pane_idx);
                            section_render::render_cross_section(
                                &mut child_ui,
                                &mut pane,
                                pane_rect,
                                top_clearance,
                                horizontal_color_scale,
                                &self.preferences,
                            );
                            pane_render::render_color_scale(
                                child_ui.painter(),
                                pane_render::clear_of_bottom_chrome(pane_rect, color_scale_floor),
                                horizontal_color_scale,
                                &pane,
                                &self.preferences,
                            );
                        }
                        RenderView::Volume => {
                            self.record_pane_content(pane_idx, RenderView::Volume, pane_rect);
                            let volume_response = child_ui.interact(
                                pane_rect,
                                child_ui.id().with(("volume_orbit", pane_idx)),
                                egui::Sense::click_and_drag(),
                            );
                            let painter = self.volume_painter().cloned();
                            let floor_frame = floor_frame_for(
                                &pane,
                                pane_idx,
                                painter.as_deref(),
                                center,
                                floor_strips.get(pane_idx).copied().flatten(),
                                &map_memory,
                            );
                            let source_geo = self.draw_floor_strip(
                                &ctx,
                                pane_idx,
                                floor_strips.get(pane_idx).copied().flatten(),
                                FloorStripCtx {
                                    pane: &mut pane,
                                    map_memory: floor_frame.memory,
                                    center: floor_frame.centre,
                                    tiles: tiles_owned.as_mut(),
                                    label_tiles: &mut label_tiles,
                                    tile_zoom_bias: tile_zoom_biases
                                        .get(pane_idx)
                                        .copied()
                                        .unwrap_or(0),
                                    horizontal_color_scale,
                                    color_scale_floor,
                                    user_location,
                                    user_heading,
                                    user_fix: user_fix.clone(),
                                    actions: &mut actions,
                                },
                            );
                            let current_stamp = self.current_volume_for(pane.site());
                            let chrome = self.chrome_fade();
                            let chrome_rect = pane_render::color_scale_free_rect(
                                child_ui.painter(),
                                pane_render::clear_of_bottom_chrome(pane_rect, color_scale_floor),
                                horizontal_color_scale,
                                pane_idx,
                                &pane,
                                &self.overlays,
                                &self.preferences,
                            );
                            let outcome = render_volume_pane(
                                &mut child_ui,
                                pane_rect,
                                chrome_rect,
                                pane_idx,
                                &mut pane,
                                &volume_response,
                                suppress_pan,
                                painter.as_deref(),
                                current_stamp,
                                chrome,
                                source_geo,
                                mirror_size_points,
                                &mut actions,
                                &mut self.volume_alpha,
                                &self.volume_iso,
                                #[cfg(test)]
                                &mut self.probes.last_alpha_buttons,
                            );
                            if let Some(why) = outcome.clone() {
                                self.volume_empty_states.insert(pane_idx, why);
                            }
                            #[cfg(test)]
                            self.probes
                                .last_volume_arms
                                .push(VolumeArmProbe { pane_idx, outcome });
                            #[cfg(not(test))]
                            let _ = outcome;

                            self.draw_volume_glass(
                                &child_ui,
                                pane_idx,
                                pane_render::clear_of_bottom_chrome(pane_rect, color_scale_floor),
                                horizontal_color_scale,
                                &pane,
                            );
                        }
                    }

                    if is_active && pane.is_map() {
                        let armed = if self.section_draw_armed() {
                            Some((SECTION_ARM_HINT, SECTION_TRACK_COLOR))
                        } else if self.region_pick_armed() {
                            Some((REGION_ARM_HINT, crate::ui_region::REGION_ARM_COLOR))
                        } else {
                            None
                        };
                        if let Some((text, color)) = armed {
                            paint_armed_hint_chip(&ctx, pane_idx, pane_rect, text, color);
                        }
                    }

                    pane.map_memory = map_memory;
                    self.panes[pane_idx] = pane;

                    if pane_count > 1 {
                        let painted = draw_pane_border(ui, pane_rect, is_active);
                        #[cfg(test)]
                        self.probes
                            .last_pane_borders
                            .push((pane_idx, painted, is_active));
                        #[cfg(not(test))]
                        let _ = painted;
                    }
                } // end pane loop

                self.click_consumed_frame = click_consumed;
                self.fade_candidate = fade_candidate && !click_consumed;

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

                self.sync_viewports(&pre_zooms, &pre_positions);
            });

        self.map_tiles.restore_base_tiles(tiles_owned);
        if any_city_labels {
            self.map_tiles.restore_label_tiles(label_tiles);
        }

        actions
    }

    /// Advance the armed cross-section draw by one frame's gesture.
    fn track_section_draw(
        &mut self,
        pane_idx: usize,
        gesture: crate::ui_input::ArmedDragGesture,
        projector: &walkers::Projector,
    ) {
        use crate::ui_input::{ArmedDragGesture, MIN_SECTION_DRAG_PT};

        let ground = |pos: egui::Pos2| {
            let position = projector.unproject(egui::vec2(pos.x, pos.y));
            rustdar_geo::GeoPoint {
                lat: position.y(),
                lon: position.x(),
            }
        };

        match gesture {
            ArmedDragGesture::Idle => {}
            ArmedDragGesture::Anchored(pos) => {
                self.section_anchor = Some(super::SectionAnchor {
                    pane_idx,
                    ground: ground(pos),
                    screen: pos,
                    current: pos,
                });
            }
            ArmedDragGesture::Dragging(pos) => {
                if let Some(anchor) = self.section_anchor.as_mut()
                    && anchor.pane_idx == pane_idx
                {
                    anchor.current = pos;
                }
            }
            ArmedDragGesture::Released(pos) => {
                let Some(anchor) = self.section_anchor.take() else {
                    return;
                };
                if anchor.pane_idx != pane_idx {
                    return;
                }
                if (pos - anchor.screen).length() < MIN_SECTION_DRAG_PT {
                    return;
                }
                let Some(line) = crate::pane::SectionLine::new(anchor.ground, ground(pos)) else {
                    log::warn!("a drawn section line was not a line; discarding it");
                    return;
                };
                self.pending_section_line = Some((pane_idx, line));
                self.set_section_draw_armed(false);
            }
            ArmedDragGesture::Cancelled => {
                if self
                    .section_anchor
                    .as_ref()
                    .is_some_and(|a| a.pane_idx == pane_idx)
                {
                    self.section_anchor = None;
                }
            }
        }
    }

    /// Advance the armed 3D region pick by one frame's gesture.
    fn track_region_pick(
        &mut self,
        pane_idx: usize,
        gesture: crate::ui_input::ArmedDragGesture,
        projector: &walkers::Projector,
    ) {
        use crate::ui_input::ArmedDragGesture;

        let ground = |pos: egui::Pos2| {
            let position = projector.unproject(egui::vec2(pos.x, pos.y));
            rustdar_geo::GeoPoint {
                lat: position.y(),
                lon: position.x(),
            }
        };

        match gesture {
            ArmedDragGesture::Idle => {}
            ArmedDragGesture::Anchored(pos) => {
                self.region_drag = crate::ui_region::RegionDrag::begin(pane_idx, ground(pos));
            }
            ArmedDragGesture::Dragging(pos) => {
                if let Some(drag) = self
                    .region_drag
                    .as_mut()
                    .filter(|drag| drag.pane_idx() == pane_idx)
                {
                    drag.extend_to(ground(pos));
                }
            }
            ArmedDragGesture::Released(pos) => {
                let Some(mut drag) = self.region_drag.take() else {
                    return;
                };
                if drag.pane_idx() != pane_idx {
                    return;
                }
                drag.extend_to(ground(pos));
                match drag.commit() {
                    Some(region) => {
                        self.pending_region = Some((pane_idx, region));
                        self.set_region_pick_armed(false);
                    }
                    None => log::debug!(
                        "3D region drag was {:.1} km across, below the resampler's \
                         {:.0} km minimum; discarded",
                        2.0 * drag.half_width_km(),
                        2.0 * rustdar_radar::voxel::MIN_HALF_WIDTH_KM,
                    ),
                }
            }
            ArmedDragGesture::Cancelled => {
                if self
                    .region_drag
                    .is_some_and(|drag| drag.pane_idx() == pane_idx)
                {
                    self.region_drag = None;
                }
            }
        }
    }

    /// Cells along a voxel grid's horizontal axes on **this device**, or `None`
    /// if no 3D pane has built one yet.
    fn volume_cells_across(&self) -> Option<usize> {
        let painter = self.volume_painter()?;
        (0..self.visible_pane_count()).find_map(|idx| {
            let target = self.panes[idx].volume()?.rendered_for.as_ref()?;
            painter.grid_cells_across(idx, target)
        })
    }

    /// Whether this frame's press landed on a section handle recorded last
    /// frame — the press-frame half of the pan-suppression rule; see the call
    /// site in `render_panes`.
    fn section_handle_pressed(&self, ctx: &egui::Context, pane_idx: usize) -> bool {
        let Some(pos) = ctx.input(|i| {
            if i.pointer.primary_pressed() {
                i.pointer.interact_pos()
            } else {
                None
            }
        }) else {
            return false;
        };
        self.section_handles
            .iter()
            .any(|zone| zone.map_pane == pane_idx && zone.grab_at(pos).is_some())
    }

    /// Advance the unarmed endpoint drag on this pane by one frame, and record
    /// where this pane's handles are for the next frame's press test.
    fn track_section_edit(
        &mut self,
        ui: &egui::Ui,
        projector: &walkers::Projector,
        pane_idx: usize,
        pane_rect: egui::Rect,
        excluded_rects: &[egui::Rect],
    ) {
        use crate::ui_section_edit::{SectionEditDrag, SectionGrabZone};

        self.section_handles.retain(|z| z.map_pane != pane_idx);
        if self.section_draw_armed {
            return;
        }

        let project =
            |p: rustdar_geo::GeoPoint| projector.project(walkers::lat_lon(p.lat, p.lon)).to_pos2();
        let lines: Vec<(usize, crate::pane::SectionLine)> = self
            .panes()
            .iter()
            .enumerate()
            .filter_map(|(idx, other)| {
                let section = other.cross_section()?;
                if section.source_pane != Some(pane_idx) {
                    return None;
                }
                Some((idx, section.line?))
            })
            .collect();
        let zones: Vec<(SectionGrabZone, crate::pane::SectionLine)> = lines
            .into_iter()
            .map(|(section_pane, line)| {
                let track = great_circle_track(line, project);
                (
                    SectionGrabZone {
                        map_pane: pane_idx,
                        section_pane,
                        a_px: track[0],
                        b_px: track[track.len() - 1],
                        track,
                    },
                    line,
                )
            })
            .collect();
        self.section_handles
            .extend(zones.iter().map(|(zone, _)| zone.clone()));

        let (pressed, down, released, pos, shift) = ui.ctx().input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.primary_released(),
                i.pointer.interact_pos(),
                i.modifiers.shift,
            )
        });
        let ground = |p: egui::Pos2| {
            let position = projector.unproject(egui::vec2(p.x, p.y));
            rustdar_geo::GeoPoint {
                lat: position.y(),
                lon: position.x(),
            }
        };

        if pressed
            && self.section_edit_drag.is_none()
            && let Some(pos) = pos
            && pane_rect.contains(pos)
            && !super::map_overlays::is_pos_blocked(ui.ctx(), pos, pane_rect, excluded_rects)
        {
            for (zone, line) in &zones {
                if let Some(grab) = zone.grab_at(pos) {
                    self.section_edit_drag = Some(SectionEditDrag::begin(
                        pane_idx,
                        zone.section_pane,
                        grab,
                        *line,
                        pos,
                        ground(pos),
                        shift,
                    ));
                    break;
                }
            }
        }

        let Some(drag) = self
            .section_edit_drag
            .as_mut()
            .filter(|d| d.map_pane == pane_idx)
        else {
            return;
        };

        if (down || released)
            && let Some(pos) = pos
            && drag.pointer_moved(pos)
        {
            drag.drag_to(pos, ground(pos));
        }

        if released {
            let drag = self
                .section_edit_drag
                .take()
                .expect("filtered Some above; nothing between takes it");
            if let Some(line) = drag.commit() {
                self.pending_section_edit = Some((drag.section_pane, line));
            }
        } else if !down {
            self.section_edit_drag = None;
        }
    }

    /// Draw the rubber band of an in-flight draw and the ground track of every
    /// section cut from this map.
    fn draw_section_tracks(
        &mut self,
        ui: &egui::Ui,
        projector: &walkers::Projector,
        pane_idx: usize,
        pane_rect: egui::Rect,
    ) {
        let painter = ui.painter();
        let project =
            |p: rustdar_geo::GeoPoint| projector.project(walkers::lat_lon(p.lat, p.lon)).to_pos2();

        #[cfg(test)]
        let mut painted: Vec<(usize, usize, egui::Pos2, egui::Pos2)> = Vec::new();

        for (idx, other) in self.panes().iter().enumerate() {
            let Some(section) = other.cross_section() else {
                continue;
            };
            if section.source_pane != Some(pane_idx) {
                continue;
            }
            let Some(committed) = section.line else {
                continue;
            };
            let editing = self
                .section_edit_drag
                .filter(|d| d.map_pane == pane_idx && d.section_pane == idx);
            let dropped = self
                .pending_section_edit
                .filter(|&(pane, _)| pane == idx)
                .map(|(_, line)| line);
            let line = editing
                .map(|d| d.preview())
                .or(dropped)
                .unwrap_or(committed);
            let track = great_circle_track(line, project);
            paint_section_track(painter, &track, pane_rect);
            #[cfg(test)]
            if let (Some(&a), Some(&b)) = (track.first(), track.last()) {
                painted.push((pane_idx, idx, a, b));
            }
            paint_section_handles(painter, &track, pane_rect, editing.map(|d| d.grab));
        }

        if let Some((from, to)) = self.section_rubber_band(pane_idx) {
            paint_section_track(painter, &[from, to], pane_rect);
        }

        #[cfg(test)]
        self.probes.last_section_tracks.extend(painted);
    }

    /// Draw the region boxes that belong to pane `pane_idx`: every committed
    /// one picked on this map, and the one being dragged on it right now.
    fn draw_region_boxes(
        &mut self,
        ui: &egui::Ui,
        projector: &walkers::Projector,
        pane_idx: usize,
    ) {
        let painter = ui.painter();
        let cells_across = self.volume_cells_across();

        #[cfg(test)]
        let mut painted: Vec<(usize, usize, egui::Rect)> = Vec::new();

        for (idx, other) in self.panes().iter().enumerate() {
            let Some(volume) = other.volume() else {
                continue;
            };
            if volume.source_pane != Some(pane_idx) {
                continue;
            }
            let Some(region) = volume.region else {
                continue;
            };
            let Some(rect) =
                region_screen_rect(projector, region.centre(), region.half_extent_km())
            else {
                continue;
            };
            paint_region_box(painter, rect, REGION_COMMITTED_COLOR, false);
            #[cfg(test)]
            painted.push((pane_idx, idx, rect));
            #[cfg(not(test))]
            let _ = idx;
        }

        if let Some((centre, half_width_km)) = self.region_preview(pane_idx)
            && let Some(rect) = region_screen_rect(
                projector,
                centre,
                rustdar_radar::voxel::HalfExtentKm::square(half_width_km),
            )
        {
            paint_region_box(painter, rect, crate::ui_region::REGION_ARM_COLOR, true);
            paint_region_hint(painter, rect, half_width_km, cells_across);
        }

        #[cfg(test)]
        self.probes.last_region_boxes.extend(painted);
    }

    /// Detect which pane was clicked and make it the active pane.
    fn detect_active_pane_click(&mut self, ctx: &egui::Context, panel_rect: egui::Rect) {
        let Some(pos) = ctx.input(|i| {
            if i.pointer.primary_pressed() {
                i.pointer.interact_pos()
            } else {
                None
            }
        }) else {
            return;
        };
        self.press_switched_pane = false;
        self.press_popup_open = egui::Popup::is_any_open(ctx);
        if ctx
            .layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
        {
            return;
        }
        let pane_count = self.visible_pane_count();
        if pane_count <= 1 {
            return;
        }
        for idx in 0..pane_count {
            let rect = self.pane_layout.pane_rect(idx, panel_rect);
            if rect.contains(pos) && idx != self.active_pane {
                self.active_pane = idx;
                self.press_switched_pane = true;
                self.pill_revealed = None;
                break;
            }
        }
    }

    /// Dismiss overlay popups when clicking outside them.
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

    /// Draw one 3D pane's own map into its off-screen strip, and answer with
    /// the affine it drew through.
    #[allow(clippy::too_many_lines)]
    fn draw_floor_strip(
        &mut self,
        ctx: &egui::Context,
        pane_idx: usize,
        strip: Option<egui::Rect>,
        floor: FloorStripCtx<'_>,
    ) -> Option<crate::volume_view::MapPaneGeo> {
        let FloorStripCtx {
            pane,
            map_memory,
            center,
            tiles,
            label_tiles,
            tile_zoom_bias,
            horizontal_color_scale,
            color_scale_floor,
            user_location,
            user_heading,
            user_fix,
            actions,
        } = floor;
        use walkers::Map;

        let (strip, tiles) = (strip?, tiles?);
        let mut map_memory = map_memory;
        let map_memory = &mut map_memory;

        let layer = egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new(("volume_floor_strip", pane_idx)),
        );
        let mut strip_ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new(("volume_floor_strip_ui", pane_idx)),
            egui::UiBuilder::new().layer_id(layer).max_rect(strip),
        );
        strip_ui.set_clip_rect(strip);

        let mut strip_click_consumed = false;

        Map::new(None, map_memory, center)
            .zoom_with_ctrl(false)
            .panning(false)
            .drag_pan_buttons(egui::DragPanButtons::empty())
            .show(&mut strip_ui, |ui, _response, projector, memory| {
                let zoom = memory.zoom();
                draw_tile_layer(ui, projector, zoom, tiles, tile_zoom_bias);

                self.map_pane_geo
                    .insert(pane_idx, map_pane_geo_from(projector, strip));

                let mut render_ctx = pane_render::PaneRenderCtx {
                    pane_idx,
                    pane,
                    overlays: &mut self.overlays,
                    user_location,
                    user_heading,
                    user_fix,
                    label_tiles,
                    tile_zoom_bias,
                    actions,
                    pane_rect: strip,
                    surfaces: pane_render::PaneSurfaces::GroundOnly,
                    horizontal_color_scale,
                    color_scale_floor,
                    pointer_available: false,
                    excluded_rects: Vec::new(),
                    long_press_pos: None,
                    overlay_click_pos: None,
                    click_consumed: &mut strip_click_consumed,
                    preferences: &self.preferences,
                    #[cfg(test)]
                    paint_order: Vec::new(),
                };
                pane_render::render_pane_map_content(ui, projector, zoom, &mut render_ctx);
            });

        self.map_pane_geo.get(&pane_idx).copied()
    }

    /// Draw a 3D pane's **glass**: the half of its map content that is chrome
    /// rather than geography, in ordinary 2D on the pane's own rect.
    fn draw_volume_glass(
        &self,
        ui: &egui::Ui,
        pane_idx: usize,
        pane_rect: egui::Rect,
        horizontal_color_scale: bool,
        pane: &crate::pane::PaneState,
    ) {
        let painter = ui.painter().with_clip_rect(pane_rect);
        if pane.is_overlay_enabled(&known::COLOR_SCALE) {
            pane_render::render_color_scales(
                &painter,
                pane_rect,
                horizontal_color_scale,
                pane_idx,
                pane,
                &self.overlays,
                &self.preferences,
            );
        }
        if pane.is_overlay_enabled(&known::RADAR)
            && let Some((on_screen, elevation)) = pane.stale_image_on_screen()
        {
            pane_render::draw_pending_render_notice(
                &painter,
                pane_rect,
                crate::ui::pills::pill_row_clearance(ui.ctx(), pane_idx),
                on_screen,
                elevation,
            );
        }
    }
}

/// Everything [`Gui::draw_floor_strip`] needs from the pane loop that is not
/// already on the `Gui`.
struct FloorStripCtx<'a> {
    pane: &'a mut crate::pane::PaneState,
    /// The viewport the strip is drawn through, **owned**.
    map_memory: walkers::MapMemory,
    /// What the strip is centred on: the box's centre, which is the region's for
    /// a picked one and the **site** otherwise — the point `build_voxels`
    /// centres an unstated box on.
    center: walkers::Position,
    /// `None` when the frame has no tile source at all, which is the one way
    /// the floor can be missing that is not about the pane.
    tiles: Option<&'a mut crate::tile_source::HttpsTiles>,
    label_tiles: &'a mut Option<crate::tile_source::HttpsTiles>,
    tile_zoom_bias: u8,
    horizontal_color_scale: bool,
    /// See [`pane_render::PaneRenderCtx::color_scale_floor`]. Carried through
    /// the floor strip only so the `PaneRenderCtx` it builds is complete; the
    /// strip itself is `GroundOnly` and paints no legend.
    color_scale_floor: f32,
    user_location: Option<(f64, f64)>,
    user_heading: Option<f32>,
    user_fix: Option<rustdar_location::Fix>,
    actions: &'a mut Vec<GuiAction>,
}

/// Paint a pane's empty state: one line of centred, muted text and nothing
/// else.
struct FloorFrame {
    centre: walkers::Position,
    memory: walkers::MapMemory,
}

/// Frame the floor strip on the box the pane resamples.
fn floor_frame_for(
    pane: &crate::pane::PaneState,
    pane_idx: usize,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    site: walkers::Position,
    strip: Option<egui::Rect>,
    pane_memory: &walkers::MapMemory,
) -> FloorFrame {
    let fallback = || FloorFrame {
        centre: site,
        memory: pane_memory.clone(),
    };
    let Some(volume) = pane.volume() else {
        return fallback();
    };
    let (centre, half) = match volume.region {
        Some(region) => (
            walkers::Position::new(region.centre().lon, region.centre().lat),
            region.half_extent_km(),
        ),
        None => {
            let Some(built) = volume
                .rendered_for
                .as_ref()
                .zip(painter)
                .and_then(|(target, painter)| painter.box_size_km(pane_idx, target))
            else {
                return fallback();
            };
            (
                site,
                rustdar_radar::voxel::HalfExtentKm {
                    east_km: 0.5 * f64::from(built[0]),
                    north_km: 0.5 * f64::from(built[1]),
                },
            )
        }
    };
    let framed = strip.and_then(|strip| crate::ui_region::viewport_for_region(strip, centre, half));
    match framed {
        Some(memory) => FloorFrame { centre, memory },
        None => fallback(),
    }
}

/// Degrees of yaw per point of horizontal drag.
const ORBIT_YAW_DEG_PER_POINT: f32 = 0.4;
/// Degrees of pitch per point of vertical drag. Shallower than the yaw rate
/// because the usable pitch range is 178° against yaw's unbounded turn, so the
/// same rate would run into the clamp within a third of a pane.
const ORBIT_PITCH_DEG_PER_POINT: f32 = 0.25;

/// Fingers a touch drag must have to pan a 3D pane.
const TOUCH_PAN_FINGERS: usize = 2;

/// What the 3D arm did with one pane on one frame.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VolumeArmProbe {
    pub(crate) pane_idx: usize,
    pub(crate) outcome: Option<String>,
}

/// Draw one 3D pane: take its gesture, ask for its grid, and either push a
/// paint callback or say why there is not one.
#[allow(clippy::too_many_arguments)]
fn render_volume_pane(
    ui: &mut egui::Ui,
    pane_rect: egui::Rect,
    chrome_rect: egui::Rect,
    pane_idx: usize,
    pane: &mut crate::pane::PaneState,
    response: &egui::Response,
    suppress_drag: bool,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    current_stamp: Option<crate::ui::CurrentVolumeStamp>,
    chrome: Option<f32>,
    source_geo: Option<crate::volume_view::MapPaneGeo>,
    mirror_size_points: egui::Vec2,
    actions: &mut Vec<GuiAction>,
    alpha_curves: &mut crate::volume_alpha::AlphaCurves,
    iso_thresholds: &crate::volume_iso::IsoThresholds,
    #[cfg(test)] alpha_buttons: &mut Vec<(usize, egui::Rect)>,
) -> Option<String> {
    let outcome = volume_pane_outcome(
        ui,
        pane_rect,
        pane_idx,
        pane,
        response,
        suppress_drag,
        painter,
        current_stamp,
        source_geo,
        mirror_size_points,
        actions,
        alpha_curves,
        iso_thresholds,
    );
    if let Some(why) = outcome.empty.as_deref() {
        paint_pane_empty_state(ui, pane_rect, why);
    }
    volume_alpha_editor::editor_ui(
        ui,
        pane_rect,
        chrome_rect,
        pane_idx,
        pane,
        painter,
        outcome.target.as_ref(),
        chrome,
        alpha_curves,
        #[cfg(test)]
        alpha_buttons,
    );
    outcome.empty
}

/// What the 3D arm resolved for one pane on one frame: the empty-state reason
/// if there was one, and the target it aimed at if it got far enough to name
/// one. The target is what the Volume Alpha editor looks the palette up by —
/// re-deriving it there would be a second copy of the stamp-and-region logic
/// that could drift from this one.
struct VolumeOutcome {
    empty: Option<String>,
    target: Option<crate::pane::VolumeTarget>,
}

impl VolumeOutcome {
    fn empty_state(why: String) -> Self {
        Self {
            empty: Some(why),
            target: None,
        }
    }
}

/// The 3D arm's decision, with the painting left to its caller so that every
/// path out of it is a `return` of a reason rather than a `return` plus a call
/// somebody can forget to make.
#[allow(clippy::too_many_arguments)]
fn volume_pane_outcome(
    ui: &mut egui::Ui,
    pane_rect: egui::Rect,
    pane_idx: usize,
    pane: &mut crate::pane::PaneState,
    response: &egui::Response,
    suppress_drag: bool,
    painter: Option<&dyn crate::volume_view::VolumePainter>,
    current_stamp: Option<crate::ui::CurrentVolumeStamp>,
    source_geo: Option<crate::volume_view::MapPaneGeo>,
    mirror_size_points: egui::Vec2,
    actions: &mut Vec<GuiAction>,
    alpha_curves: &crate::volume_alpha::AlphaCurves,
    iso_thresholds: &crate::volume_iso::IsoThresholds,
) -> VolumeOutcome {
    use crate::pane::{OrbitDelta, VolumeStamp, VolumeTarget};
    use crate::volume_view::{VolumeFrameState, VolumePaint};

    let Some((camera_before, view_mode, region)) =
        pane.volume().map(|v| (v.camera, v.view_mode, v.region))
    else {
        return VolumeOutcome::empty_state(VOLUME_EMPTY_STATE.to_owned());
    };
    let mut box_size_km = crate::pane::box_size_km(region);
    if let Some(built) = pane
        .volume()
        .and_then(|v| v.rendered_for.as_ref())
        .zip(painter)
        .and_then(|(target, painter)| painter.box_size_km(pane_idx, target))
    {
        box_size_km = built;
    }

    let mut delta = OrbitDelta::default();
    if !suppress_drag && response.dragged_by(egui::PointerButton::Primary) {
        let drag = response.drag_delta();
        delta.yaw_deg = drag.x * ORBIT_YAW_DEG_PER_POINT;
        delta.pitch_deg = drag.y * ORBIT_PITCH_DEG_PER_POINT;
    }

    let touch = ui.ctx().multi_touch();
    let pan_drag = match touch {
        _ if suppress_drag => None,
        Some(touch) if touch.num_touches >= TOUCH_PAN_FINGERS => {
            delta.yaw_deg = 0.0;
            delta.pitch_deg = 0.0;
            Some([touch.translation_delta.x, touch.translation_delta.y])
        }
        _ if response.dragged_by(egui::PointerButton::Secondary) => {
            let drag = response.drag_delta();
            Some([drag.x, drag.y])
        }
        _ => None,
    };
    if let Some(drag) = pan_drag
        && let Some(pan) =
            crate::volume_view::pan_for_drag(camera_before, box_size_km, pane_rect.height(), drag)
    {
        delta.pan = pan;
    }

    delta.zoom_factor = crate::ui_region::zoom_camera(ui.ctx(), response);

    let site_code = pane.site().to_string();
    let product = pane.selected_product();
    let loop_grid = pane.active_volume_frame().cloned();
    let navigated = (!pane.viewing_live)
        .then(|| pane.scan_info.as_ref().map(|info| info.timestamp))
        .flatten();
    let stamp = current_stamp.map(|stamp| match navigated {
        Some(collected) => (
            VolumeStamp {
                site: site_code.clone(),
                collected,
            },
            Some(collected),
        ),
        None => (
            VolumeStamp {
                site: site_code.clone(),
                collected: stamp.newest,
            },
            stamp.base_started,
        ),
    });

    let Some(volume) = pane.volume_mut() else {
        return VolumeOutcome::empty_state(VOLUME_EMPTY_STATE.to_owned());
    };
    volume.camera.nudge(delta);
    let camera = volume.camera;
    let floor = !volume.hide_floor;
    let already_rendered = volume.rendered_for.clone();

    let Some(painter) = painter else {
        return VolumeOutcome::empty_state(VOLUME_EMPTY_STATE.to_owned());
    };
    let Some((volume_stamp, base_started)) = stamp else {
        return VolumeOutcome::empty_state(format!(
            "Downloading the first {site_code} volume...\n\nThe 3D view builds the moment it \
             lands, then updates tilt by tilt as new sweeps arrive.",
        ));
    };
    if rustdar_radar::derive::volume_slot(product).is_none() {
        return VolumeOutcome::empty_state(format!(
            "{} has no vertical structure to render in 3D - pick a moment the radar measures \
             or derives tilt by tilt",
            product.name(),
        ));
    }

    let live_target = VolumeTarget {
        volume: volume_stamp,
        product,
        region,
    };
    let (target, from_loop) = match loop_grid {
        Some(grid) => (grid.target, true),
        None => (live_target, false),
    };
    let collected = target.volume.collected;
    if !from_loop && already_rendered.as_ref() != Some(&target) {
        actions.push(GuiAction::PrepareVolume {
            pane_idx,
            target: target.clone(),
        });
    }

    let pixels_per_point = ui.ctx().pixels_per_point();
    let size_px = [
        (pane_rect.width() * pixels_per_point).round().max(1.0) as u32,
        (pane_rect.height() * pixels_per_point).round().max(1.0) as u32,
    ];

    let empty = match painter.paint(&VolumeFrameState {
        pane_idx,
        target: target.clone(),
        camera,
        size_px,
        pixels_per_point,
        floor,
        source: source_geo,
        mirror_size_points: [mirror_size_points.x, mirror_size_points.y],
        alpha: alpha_curves.get(product),
        view_mode,
        iso_threshold: iso_thresholds.get(product),
    }) {
        VolumePaint::Callback { payload, showing } => {
            ui.painter()
                .add(egui::Shape::Callback(egui::epaint::PaintCallback {
                    rect: pane_rect,
                    callback: payload,
                }));
            let drawn_box_km = painter
                .box_size_km(pane_idx, &target)
                .unwrap_or(box_size_km);
            let half = rustdar_radar::voxel::HalfExtentKm {
                east_km: 0.5 * f64::from(drawn_box_km[0]),
                north_km: 0.5 * f64::from(drawn_box_km[1]),
            };
            paint_volume_caption(
                ui,
                pane_rect,
                crate::ui::pills::pill_row_clearance(ui.ctx(), pane_idx),
                &volume_caption(
                    &site_code,
                    collected,
                    base_started,
                    half,
                    camera,
                    showing,
                    painter.grid_cells_across(pane_idx, &target),
                ),
            );
            None
        }
        VolumePaint::Empty(why) => Some(why),
    };
    VolumeOutcome {
        empty,
        target: Some(target),
    }
}

/// The 3D pane's own controls: how far the vertical is stretched, and a way back
/// to the view it started at.
pub(crate) fn render_volume_controls(
    ui: &mut egui::Ui,
    pane: &mut crate::pane::PaneState,
    iso_thresholds: &mut crate::volume_iso::IsoThresholds,
    alpha_curves: &crate::volume_alpha::AlphaCurves,
    drawing_nothing: Option<&str>,
) {
    let product = pane.selected_product();
    let Some(volume) = pane.volume_mut() else {
        return;
    };
    ui.add_space(6.0);
    ui.separator();
    ui.label(VOLUME_SIDEBAR_HEADER);

    ui.indent("volume_controls", |ui| {
        let mut exaggeration = volume.camera.vertical_exaggeration();
        ui.horizontal(|ui| {
            ui.label("Vertical:");
            let response = ui.add(
                egui::Slider::new(
                    &mut exaggeration,
                    crate::pane::MIN_VERTICAL_EXAGGERATION..=crate::pane::MAX_VERTICAL_EXAGGERATION,
                )
                .suffix("\u{d7}")
                .fixed_decimals(1),
            );
            if response.changed() {
                volume.camera.set_vertical_exaggeration(exaggeration);
            }
            response.on_hover_text(
                "Stretches the box vertically so storm structure is legible. Heights the pane \
                 reports stay in real kft MSL at every setting.",
            );
        });

        let mut standoff = volume.camera.eye_distance();
        ui.horizontal(|ui| {
            ui.label("Distance:");
            let response = ui.add(
                egui::Slider::new(
                    &mut standoff,
                    crate::pane::MIN_EYE_DISTANCE..=crate::pane::MAX_EYE_DISTANCE,
                )
                .logarithmic(true)
                .suffix("\u{d7}")
                .fixed_decimals(2),
            );
            if response.changed() {
                volume.camera.set_eye_distance(standoff);
            }
            response.on_hover_text(
                "How far back the eye sits, in box framing radii - the framing. Under 1 puts \
                 the eye inside the box. Scroll and pinch zoom the ground instead, the same \
                 way they do on a flat pane.",
            );
        });

        ui.horizontal(|ui| {
            ui.label("Mode:");
            ui.radio_value(
                &mut volume.view_mode,
                crate::pane::VolumeViewMode::LitVolume,
                "Lit volume",
            )
            .on_hover_text("The translucent accumulation: cloud shaped by the product's transparency profile and your Volume Alpha curve.");
            ui.radio_value(
                &mut volume.view_mode,
                crate::pane::VolumeViewMode::Isosurface,
                "Isosurface",
            )
            .on_hover_text("One opaque, lit surface at the threshold below - the shell of everything at or beyond it.");
        });
        if volume.view_mode == crate::pane::VolumeViewMode::Isosurface {
            let (prefix, suffix) = crate::volume_iso::slider_labels(product);
            let mut threshold = iso_thresholds.get(product);
            ui.horizontal(|ui| {
                ui.label(format!("{prefix}:"));
                let response = ui.add(
                    egui::Slider::new(&mut threshold, crate::volume_iso::slider_range(product))
                        .suffix(suffix)
                        .fixed_decimals(if *crate::volume_iso::slider_range(product).end() <= 4.0 {
                            2
                        } else {
                            0
                        }),
                );
                if response.changed() {
                    iso_thresholds.set(product, threshold);
                }
                response.on_hover_text(format!(
                    "Where {}'s surface sits. Per product - every 3D pane showing this \
                     product shares it.",
                    product.name(),
                ));
            });
            if alpha_curves.is_edited(product) {
                ui.label(
                    egui::RichText::new(
                        "The isosurface reads the data itself; your Volume Alpha curve \
                         applies to the lit volume only.",
                    )
                    .small()
                    .weak(),
                );
            }
        }

        let mut show_floor = !volume.hide_floor;
        if ui
            .checkbox(&mut show_floor, MAP_FLOOR_LABEL)
            .on_hover_text(
                "Draws the ground under the volume: the basemap, SPC outlooks, the base \
                 reflectivity as the 2D map shows it, the range ring, mesoscale discussion, \
                 warning and watch polygons, and city labels, registered to the box. \
                 Warnings and discussions refresh on the floor as they issue and expire. \
                 Which of them go down there is this pane's own layer set, in the Layers \
                 panel - the volume above the floor is not one of them. It is drawn by \
                 the volume's own render, so it appears when the volume does and not before.",
            )
            .changed()
        {
            volume.hide_floor = !show_floor;
        }
        if let Some(why) = drawing_nothing {
            ui.label(
                egui::RichText::new(format!("{MAP_FLOOR_INERT_NOTE} {}", reason_headline(why)))
                    .small()
                    .weak(),
            );
        }

        if let Some(region) = volume.region {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Region: {}",
                    axes(
                        2.0 * region.half_east_km(),
                        2.0 * region.half_north_km(),
                        0
                    ),
                ));
                ui.label("km");
                if ui
                    .button(WHOLE_RING_LABEL)
                    .on_hover_text(
                        "Drops the picked region and goes back to the volume's own data reach - \
                         the whole ring, at whatever range the scan in hand carries. The camera \
                         is left exactly as it is. Picking a region is the only way to spend the \
                         grid's cells on less ground, so this is the way back to the coarser, \
                         wider picture.",
                    )
                    .clicked()
                {
                    volume.region = None;
                    volume.source_pane = None;
                }
            });
        }

        if ui
            .button("Reset view")
            .on_hover_text("Back to the default angle, zoom, centre and region.")
            .clicked()
        {
            reset_volume_view(volume);
        }
    });
}

/// The label on the control that drops a picked region.
pub(crate) const WHOLE_RING_LABEL: &str = "Whole ring";

/// Put a 3D pane back to the view it opened at.
pub(crate) fn reset_volume_view(volume: &mut crate::pane::VolumePane) {
    volume.camera = crate::pane::OrbitCamera::default();
    volume.region = None;
    volume.source_pane = None;
}

/// Where each 3D pane draws its own map so that **only the mirror can see it**,
/// and how much of egui's coordinate space the mirror must therefore cover.
fn floor_strip_plan(
    screen: egui::Rect,
    wanted: &[Option<egui::Rect>],
) -> (Vec<Option<egui::Rect>>, egui::Vec2) {
    let frame = screen.max.to_vec2();
    let Some(top) = wanted
        .iter()
        .flatten()
        .map(|rect| rect.min.y)
        .reduce(f32::min)
    else {
        return (vec![None; wanted.len()], frame);
    };
    let offset = egui::vec2(0.0, (screen.max.y - top).max(0.0));
    let strips: Vec<Option<egui::Rect>> = wanted
        .iter()
        .map(|rect| rect.map(|rect| rect.translate(offset)))
        .collect();
    let size = strips.iter().flatten().fold(frame, |size, strip| {
        egui::vec2(size.x.max(strip.max.x), size.y.max(strip.max.y))
    });
    (strips, size)
}

/// Reduce a live `walkers::Projector` to the four numbers a 3D pane's map
/// floor is reprojected through. See [`crate::volume_view::MapPaneGeo`].
fn map_pane_geo_from(
    projector: &walkers::Projector,
    rect: egui::Rect,
) -> crate::volume_view::MapPaneGeo {
    use crate::volume_view::{MapPaneGeo, mercator_y_of_lat};

    let centre = projector.unproject(rect.center().to_vec2());
    let (anchor_lat, anchor_lon) = (centre.y(), centre.x());
    let anchor = projector
        .project(walkers::lat_lon(anchor_lat, anchor_lon))
        .to_pos2();

    let lon_step: f64 = if anchor_lon > 0.0 { -1.0 } else { 1.0 };
    let lat_step: f64 = if anchor_lat > 0.0 { -1.0 } else { 1.0 };

    let east = projector
        .project(walkers::lat_lon(anchor_lat, anchor_lon + lon_step))
        .to_pos2();
    let north = projector
        .project(walkers::lat_lon(anchor_lat + lat_step, anchor_lon))
        .to_pos2();

    let d_merc = mercator_y_of_lat(anchor_lat + lat_step) - mercator_y_of_lat(anchor_lat);
    MapPaneGeo {
        rect,
        anchor_lat,
        anchor_lon,
        anchor,
        points_per_degree_lon: f64::from(east.x - anchor.x) / lon_step,
        points_per_mercator_y: f64::from(north.y - anchor.y) / d_merc,
    }
}

/// Kilofeet per kilometre. The vertical readout is in kft MSL because that is
/// what a forecaster reads a storm top in, and because it is the unit the rest of
/// this application already uses for heights.
const KFT_PER_KM: f64 = 3.280_84;

/// What the pane says about the picture it is showing, one line per fact.
fn volume_caption(
    site: &str,
    newest: chrono::NaiveDateTime,
    base_started: Option<chrono::NaiveDateTime>,
    half: rustdar_radar::voxel::HalfExtentKm,
    camera: crate::pane::OrbitCamera,
    showing: crate::volume_view::Showing,
    cells: Option<usize>,
) -> Vec<String> {
    let mut lines = vec![format!(
        "{site} volume - newest data {}Z",
        newest.format("%H:%M")
    )];

    match base_started {
        Some(base) => lines.push(format!("base volume {}Z", base.format("%H:%M"))),
        None => lines.push("no complete volume yet - showing the tilts flown so far".to_owned()),
    }

    let base = rustdar_radar::voxel::DEFAULT_BASE_KM_MSL * KFT_PER_KM;
    let top = rustdar_radar::voxel::DEFAULT_TOP_KM_MSL * KFT_PER_KM;
    lines.push(format!(
        "{base:.0}-{top:.0} kft MSL - vertical exaggeration {:.1}×",
        camera.vertical_exaggeration(),
    ));

    let across = axes(2.0 * half.east_km, 2.0 * half.north_km, 0);
    match (
        showing.stale.then_some(showing.cell_km).flatten(),
        cells.and_then(|cells| {
            crate::pane::resolution_km(half.east_km, cells)
                .zip(crate::pane::resolution_km(half.north_km, cells))
        }),
    ) {
        (Some((shown_e, shown_n)), _) if showing.partial => lines.push(format!(
            "{across} km box - {} km/cell over the middle, filling in",
            axes(shown_e.into(), shown_n.into(), 2),
        )),
        (Some((shown_e, shown_n)), Some((east, north))) => lines.push(format!(
            "{across} km box - {} km/cell, sharpening to {}",
            axes(shown_e.into(), shown_n.into(), 2),
            axes(east, north, 2),
        )),
        (Some((shown_e, shown_n)), None) => lines.push(format!(
            "{across} km box - {} km/cell, sharpening",
            axes(shown_e.into(), shown_n.into(), 2),
        )),
        (None, Some((east, north))) => lines.push(format!(
            "{across} km box - {} km/cell",
            axes(east, north, 2)
        )),
        (None, None) => lines.push(format!("{across} km box")),
    }
    lines
}

/// `east` alone when the two axes print the same at `decimals`, `east × north`
/// otherwise.
fn axes(east: f64, north: f64, decimals: usize) -> String {
    let (east, north) = (format!("{east:.decimals$}"), format!("{north:.decimals$}"));
    if east == north {
        east
    } else {
        format!("{east} × {north}")
    }
}

/// Inset of the caption from the pane's top-left corner, points.
const CAPTION_MARGIN: f32 = 8.0;

/// Draw the caption in the pane's top-left corner, over the volume.
fn paint_volume_caption(
    ui: &egui::Ui,
    pane_rect: egui::Rect,
    top_clearance: f32,
    lines: &[String],
) {
    if lines.is_empty() {
        return;
    }
    let galley = ui.painter().layout(
        lines.join("\n"),
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(235, 235, 235),
        pane_rect.width() - 2.0 * CAPTION_MARGIN,
    );
    let origin = pane_rect.left_top() + egui::vec2(CAPTION_MARGIN, top_clearance);
    ui.painter().rect_filled(
        egui::Rect::from_min_size(origin, galley.size()).expand(4.0),
        3.0,
        egui::Color32::from_black_alpha(160),
    );
    ui.painter()
        .galley(origin, galley, egui::Color32::PLACEHOLDER);
}

/// Fraction of a pane's width an empty-state message is laid out across.
const EMPTY_STATE_WIDTH_FRACTION: f32 = 0.8;

/// Paint a centred, **wrapped** explanation in the middle of a pane.
fn paint_pane_empty_state(ui: &mut egui::Ui, pane_rect: egui::Rect, text: &str) {
    let galley = ui.painter().layout(
        text.to_owned(),
        egui::FontId::proportional(14.0),
        ui.visuals().weak_text_color(),
        pane_rect.width() * EMPTY_STATE_WIDTH_FRACTION,
    );
    let size = galley.size();
    let top_left = pane_rect.center() - 0.5 * size;
    ui.painter()
        .galley(top_left, galley, ui.visuals().weak_text_color());
}

/// The colour a section's ground track and its end caps are drawn in.
const SECTION_TRACK_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 214, 10);

/// What the armed cross-section draw's hint chip says.
pub(crate) const SECTION_ARM_HINT: &str = "Drag A-B to draw cross-section";

/// What the armed 3D region pick's hint chip says.
pub(crate) const REGION_ARM_HINT: &str = "Drag a square to pick the 3D region";

/// The colour a **committed** region box is drawn in on its source map.
const REGION_COMMITTED_COLOR: egui::Color32 = egui::Color32::from_rgb(120, 200, 255);

/// Stroke width of a region box, points.
const REGION_STROKE: f32 = 1.5;

/// How much of the armed box's fill shows through, as a multiple of its
/// colour's alpha.
const REGION_FILL_ALPHA: f32 = 0.12;

/// One region box's screen rect, or `None` for a box with no rectangle to draw.
fn region_screen_rect(
    projector: &walkers::Projector,
    centre: rustdar_geo::GeoPoint,
    half: rustdar_radar::voxel::HalfExtentKm,
) -> Option<egui::Rect> {
    if !(half.is_finite() && half.east_km > 0.0 && half.north_km > 0.0) {
        return None;
    }
    let (nw, se) = crate::ui_region::corners_for(centre, half)?;
    Some(crate::overlay_cache::geo_corner_rect(
        projector,
        (nw.lat, nw.lon),
        (se.lat, se.lon),
    ))
}

/// Paint one region box: an outline, and for the drag a translucent fill.
fn paint_region_box(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32, filled: bool) {
    if filled {
        painter.rect_filled(rect, 0.0, color.gamma_multiply(REGION_FILL_ALPHA));
    }
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(REGION_STROKE, color),
        egui::StrokeKind::Middle,
    );
}

/// The width and per-cell resolution of the box being dragged, over its top
/// edge.
fn paint_region_hint(
    painter: &egui::Painter,
    rect: egui::Rect,
    half_width_km: f64,
    cells: Option<usize>,
) {
    let Some(text) = region_hint_text(half_width_km, cells) else {
        return;
    };
    let galley = painter.layout_no_wrap(
        text,
        egui::FontId::proportional(12.0),
        egui::Color32::from_rgb(20, 20, 20),
    );
    let origin = egui::pos2(rect.left(), rect.top() - galley.size().y - 4.0);
    painter.rect_filled(
        egui::Rect::from_min_size(origin, galley.size()).expand(3.0),
        2.0,
        crate::ui_region::REGION_ARM_COLOR,
    );
    painter.galley(origin, galley, egui::Color32::PLACEHOLDER);
}

/// The hint's text for a drag standing at `half_width_km`, or `None` for a box
/// that cannot be described.
fn region_hint_text(half_width_km: f64, cells: Option<usize>) -> Option<String> {
    let region = crate::pane::VolumeRegion::new(
        rustdar_geo::GeoPoint { lat: 0.0, lon: 0.0 },
        rustdar_radar::voxel::HalfExtentKm::square(half_width_km),
    )?;
    let across = 2.0 * region.half_east_km();
    match cells.and_then(|cells| region.resolution_km(cells)) {
        Some((km, _)) => Some(format!("{across:.0} km - {km:.2} km/cell")),
        None => Some(format!("{across:.0} km")),
    }
}

/// Padding between the hint chip's text and its dashed border, each axis.
const ARMED_HINT_PADDING: egui::Vec2 = egui::vec2(12.0, 8.0);

/// Dash and gap of the chip's border, points.
const ARMED_HINT_DASH: f32 = 6.0;
const ARMED_HINT_GAP: f32 = 4.0;

/// Paint the armed-tool hint chip: a centred, non-interactive dashed-border
/// chip naming the drag the armed mode is waiting for.
fn paint_armed_hint_chip(
    ctx: &egui::Context,
    pane_idx: usize,
    pane_rect: egui::Rect,
    text: &str,
    color: egui::Color32,
) {
    let layer = egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new(("armed_hint_chip", pane_idx)),
    );
    let painter = ctx.layer_painter(layer).with_clip_rect(pane_rect);
    let wrap_width = (pane_rect.width() - 2.0 * ARMED_HINT_PADDING.x - 2.0 * 16.0).max(40.0);
    let galley = painter.layout(
        text.to_owned(),
        egui::FontId::proportional(13.0),
        color,
        wrap_width,
    );
    let rect =
        egui::Rect::from_center_size(pane_rect.center(), galley.size() + 2.0 * ARMED_HINT_PADDING);
    painter.rect_filled(
        rect,
        4.0,
        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140),
    );
    let stroke = egui::Stroke::new(1.0, color);
    for (a, b) in [
        (rect.left_top(), rect.right_top()),
        (rect.right_top(), rect.right_bottom()),
        (rect.right_bottom(), rect.left_bottom()),
        (rect.left_bottom(), rect.left_top()),
    ] {
        painter.extend(egui::Shape::dashed_line(
            &[a, b],
            stroke,
            ARMED_HINT_DASH,
            ARMED_HINT_GAP,
        ));
    }
    painter.galley(
        rect.min + ARMED_HINT_PADDING,
        galley,
        egui::Color32::PLACEHOLDER,
    );
}

/// Segments a committed ground track is drawn with.
const SECTION_TRACK_SAMPLES: usize = 32;

/// The screen polyline of the great circle a section is cut along.
fn great_circle_track(
    line: crate::pane::SectionLine,
    project: impl Fn(rustdar_geo::GeoPoint) -> egui::Pos2,
) -> Vec<egui::Pos2> {
    let a = (line.a().lat, line.a().lon);
    let b = (line.b().lat, line.b().lon);
    (0..=SECTION_TRACK_SAMPLES)
        .map(|i| {
            let t = i as f64 / SECTION_TRACK_SAMPLES as f64;
            let (lat, lon) = rustdar_geo::great_circle_point(a, b, t);
            project(rustdar_geo::GeoPoint { lat, lon })
        })
        .collect()
}

/// Paint one section ground track: a polyline with a cap at each end.
fn paint_section_track(painter: &egui::Painter, points: &[egui::Pos2], pane_rect: egui::Rect) {
    let (Some(&from), Some(&to)) = (points.first(), points.last()) else {
        return;
    };
    let painter = painter.with_clip_rect(pane_rect);
    painter.add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(4.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140)),
    ));
    painter.add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(2.0, SECTION_TRACK_COLOR),
    ));
    for (pos, label) in [(from, "A"), (to, "B")] {
        painter.circle_filled(pos, 4.0, SECTION_TRACK_COLOR);
        painter.circle_stroke(pos, 4.0, egui::Stroke::new(1.0, egui::Color32::BLACK));
        painter.text(
            pos + egui::vec2(0.0, -12.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(11.0),
            SECTION_TRACK_COLOR,
        );
    }
}

/// Paint the two grab handles over a track's end caps.
fn paint_section_handles(
    painter: &egui::Painter,
    points: &[egui::Pos2],
    pane_rect: egui::Rect,
    active: Option<crate::ui_section_edit::SectionGrab>,
) {
    use crate::ui_section_edit::SectionGrab;
    let (Some(&a), Some(&b)) = (points.first(), points.last()) else {
        return;
    };
    let painter = painter.with_clip_rect(pane_rect);
    for (pos, grab) in [(a, SectionGrab::A), (b, SectionGrab::B)] {
        let grabbed = active == Some(grab);
        let ring = if grabbed { 9.0 } else { 7.0 };
        painter.circle_stroke(
            pos,
            ring,
            egui::Stroke::new(3.5, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140)),
        );
        painter.circle_stroke(
            pos,
            ring,
            egui::Stroke::new(if grabbed { 2.5 } else { 1.5 }, SECTION_TRACK_COLOR),
        );
    }
}

/// Draw a border around a pane rect, highlighted when active. Returns the
/// painted stroke's bounds, for the M8 containment pin.
fn draw_pane_border(ui: &mut egui::Ui, pane_rect: egui::Rect, is_active: bool) -> egui::Rect {
    let border_color = if is_active {
        egui::Color32::from_rgb(60, 140, 255)
    } else {
        egui::Color32::from_rgba_unmultiplied(128, 128, 128, 100)
    };
    let stroke_width = if is_active { 2.0 } else { 1.0 };
    let kind = egui::StrokeKind::Inside;
    ui.painter().rect_stroke(
        pane_rect,
        0.0,
        egui::Stroke::new(stroke_width, border_color),
        kind,
    );
    match kind {
        egui::StrokeKind::Inside => pane_rect,
        egui::StrokeKind::Middle => pane_rect.expand(stroke_width / 2.0),
        egui::StrokeKind::Outside => pane_rect.expand(stroke_width),
    }
}

/// What a hover needs to know about where it is, in the world rather than on
/// the glass.
pub(super) struct HoverInput {
    pub site_lat: f64,
    pub site_lon: f64,
    pub hover_lat: f64,
    pub hover_lon: f64,
}

/// What the readout says where the picture has numbers behind it that this
/// process is not holding.
const NOT_RESIDENT: &str = "| no value held for this frame";

/// Compute hover info string from the picture's own gates and site coordinates.
pub(super) fn compute_hover_info_raw(
    hover: &HoverSource,
    input: &HoverInput,
    product: RadarProduct,
    prefs: &UserPreferences,
) -> String {
    let (azimuth, distance_km) = rustdar_geo::site_bearing_range_km(
        input.site_lat,
        input.site_lon,
        input.hover_lat,
        input.hover_lon,
    );

    let value_str = match hover.read(azimuth, distance_km) {
        Reading::Value(value) => format!("| {}", product.format_value(value, prefs)),
        Reading::Unpainted => String::new(),
        Reading::NotResident => NOT_RESIDENT.to_string(),
    };

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

#[path = "ui_map/tests.rs"]
#[cfg(test)]
mod tests;

#[path = "ui_map/volume_arm_tests.rs"]
#[cfg(test)]
mod volume_arm_tests;

#[path = "ui_map/region_pick_tests.rs"]
#[cfg(test)]
mod region_pick_tests;
