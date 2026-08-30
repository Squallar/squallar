use crate::actions::GuiAction;
use squallar_overlays::render::overlay_state::PaneRef;
use squallar_radar::hover::{HoverSource, Reading};
use squallar_radar::types::RenderView;
use squallar_source::id::known;
use squallar_source::product::FieldId;
use squallar_units::UserPreferences;

#[path = "ui_map_pane.rs"]
pub(crate) mod pane_render;

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

/// The accurate-sun toggle's label. Phrased as what it turns ON, like every
/// other checkbox here, even though the pane stores the negative.
pub(crate) const SUN_LIGHT_LABEL: &str = "Sunlight";

/// What the sun control says when the pane asked for the sun and the arithmetic
/// refused the volume's own timestamp.
///
/// It names what the picture IS rather than apologising: the readable light is
/// a whole correct picture, not a degraded one, and the reader can act on this
/// by scrubbing to another volume or by leaving the box unticked.
pub(crate) const SUN_UNPLACEABLE_NOTE: &str = "The sun cannot be placed for this volume's site and time, so the pane is under the \
     readable light.";

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

        let pane_count = self.visible_pane_count();
        // The base slot follows the BasemapTiles layer exactly as the terrain
        // slot follows Terrain: built only while a visible pane draws it,
        // released the frame none does. The per-source-layer choices are read
        // off the layer's declared control surface — the sanctioned door to a
        // handler's own fields — and a changed set rebuilds the source inside
        // `ensure_base_tiles`.
        let basemap_on = self.panes[..pane_count]
            .iter()
            .any(|pane| pane.is_overlay_enabled(&known::BASEMAP_TILES));
        if basemap_on {
            let disabled = crate::basemap_layer::disabled_from_controls(
                &self
                    .overlays
                    .controls(&known::BASEMAP_TILES, &PaneRef::bare(0)),
            );
            self.map_tiles
                .ensure_base_tiles(is_dark_theme, &disabled, &ctx);
        } else {
            self.map_tiles.release_base_tiles();
        }
        let mut tiles_owned = self.map_tiles.take_base_tiles();
        // The terrain slot follows the layer, not the frame: built only while
        // a visible pane draws it, released the frame none does. A disabled
        // layer must cost zero network, and a source that exists is a source
        // whose IO task can be asked for tiles.
        let terrain_on = self.panes[..pane_count]
            .iter()
            .any(|pane| pane.is_overlay_enabled(&known::TERRAIN));
        if terrain_on {
            self.map_tiles.ensure_terrain_tiles(&ctx);
        } else {
            self.map_tiles.release_terrain_tiles();
        }
        let mut terrain_owned = self.map_tiles.take_terrain_tiles();
        let modality = self.layout.modality;
        // One read for every pane this frame draws: the figure is the
        // device's, not the pane's.
        let overlay_render_limit = self.concurrent_renders;
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
                    let color_scale_floor = self.color_scale_floor(
                        ui,
                        panel_rect,
                        pane_rect,
                        pane_idx,
                        horizontal_color_scale,
                    );

                    let mut pane = std::mem::take(&mut self.panes[pane_idx]);

                    let center = if let Some(scan_info) = &pane.scan_info {
                        Position::new(scan_info.site.lon, scan_info.site.lat)
                    } else if let Some(site) = squallar_radar::sites::get_radar_site(pane.site()) {
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

                    let armed_draw = (self.section_draw_armed()
                        || self.region_pick_armed()
                        || self.download_pick_armed())
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
                            // No tile gate: the Map widget carries the layer
                            // walk, gestures and every overlay, so it runs
                            // whether or not a base source exists this frame
                            // (the BasemapTiles arm inside the walk is what
                            // draws the ground, when the pane has it on).
                            {
                                Map::new(None, &mut map_memory, center)
                                    .zoom_with_ctrl(false)
                                    .panning(false)
                                    // This app's frames are 4 ms idle and
                                    // hundreds of ms mid-raster, so walkers'
                                    // frame-time multiplier would make a
                                    // notch zoom ~75x further during a slow
                                    // one. Asking walkers for a nominal
                                    // frame time replaces a correction this
                                    // crate used to apply by mutating
                                    // egui's `InputState` around the widget.
                                    .wheel_zoom_scales_with_frame_time(false)
                                    .drag_pan_buttons(if suppress_pan {
                                        egui::DragPanButtons::empty()
                                    } else {
                                        egui::DragPanButtons::PRIMARY
                                    })
                                    .show(&mut child_ui, |ui, _response, projector, memory| {
                                        let zoom = memory.zoom();

                                        if let Some(gesture) = gesture {
                                            if self.section_draw_armed() {
                                                self.track_section_draw(
                                                    pane_idx, gesture, projector,
                                                );
                                            } else if self.region_pick_armed() {
                                                self.track_region_pick(
                                                    pane_idx, gesture, projector,
                                                );
                                            } else if self.download_pick_armed() {
                                                self.track_download_pick(
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
                                            basemap_labels: Vec::new(),
                                            basemap_tiles: tiles_owned.as_mut(),
                                            terrain_tiles: terrain_owned.as_mut(),
                                            tile_zoom_bias,
                                            overlay_render_limit,
                                            actions: &mut actions,
                                            pane_rect,
                                            surfaces: pane_render::PaneSurfaces::GroundAndGlass,
                                            // Not a lookup: the 3D ground is
                                            // drawn by the volume arm, which
                                            // this arm is not, and the type
                                            // offers a plan view no other
                                            // answer.
                                            draws_3d_ground: pane_render::GroundIsMesh::PLAN_VIEW,
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
                            // The hydrate every caller runs before asking a
                            // handler about a pane, and this arm is about to
                            // ask twelve of them. It is also where the pane
                            // publishes its own selection into the slot the
                            // layer that owns it reads back, so without it the
                            // walk below would gate on the selection this pane
                            // had when its config file was opened.
                            pane.hydrate_layer_states(&self.overlays, pane_idx);
                            let volume_ask = pane.volume_ask(&self.overlays, pane_idx);
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
                                    terrain: terrain_owned.as_mut(),
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
                            let current_stamp = crate::radar_layer::current_volume_for(
                                self.liveness(),
                                pane.site(),
                            );
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
                                &volume_ask,
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
                        } else if self.download_pick_armed() {
                            Some((
                                crate::ui_download_area::DOWNLOAD_ARM_HINT,
                                crate::ui_download_area::DOWNLOAD_ARM_COLOR,
                            ))
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
                        let marks = PaneBorderMarks {
                            is_active,
                            group: self.panes[pane_idx].group,
                            partial: self.panes[pane_idx].partial_member(),
                        };
                        let painted = draw_pane_border(ui, pane_rect, marks);
                        #[cfg(test)]
                        self.probes
                            .last_pane_borders
                            .push((pane_idx, painted, marks));
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

                // The basemap credit, drawn once per *panel* and deliberately
                // outside the pane loop above: four panes must not produce four
                // copies. Two independent obligations want it on the map --
                // ODbL for the OpenStreetMap data, and OpenMapTiles' CC-BY,
                // which asks for visible credit in the corner of the map.
                //
                // An `Area` rather than a bare painter call because
                // `detect_active_pane_click` already ignores a press landing on
                // any layer above `Order::Background`, so the link becomes
                // click-safe without pushing a rect into `excluded_rects`.
                // **The credit is the drawn source's own**, read off
                // `Tiles::attribution` rather than a second const beside it —
                // a hardcoded string here is how the painted credit comes to
                // name a provider the client never contacts. A frame with no
                // base source falls back to the archive's credit, or — when
                // the archive has been found unreachable this session — to the
                // line that says so: the honest degraded state has to be on
                // the glass, not only in the log.
                let base_credit = tiles_owned.as_ref().map_or(
                    walkers::sources::Attribution {
                        text: if self.map_tiles.base_archive_is_unreachable() {
                            crate::tiles::UNREACHABLE_ATTRIBUTION_TEXT
                        } else {
                            crate::tiles::ARCHIVE_ATTRIBUTION_TEXT
                        },
                        url: crate::tiles::ATTRIBUTION_URL,
                        logo_light: None,
                        logo_dark: None,
                    },
                    walkers::Tiles::attribution,
                );
                // The Terrain layer's credit joins the same notice — still
                // one per panel — while the terrain slot holds a source. The
                // slot is not a second source of truth for "is Terrain on":
                // it is built and released at the top of this function off
                // the one sanctioned read (a visible pane drawing the layer),
                // plus the health latch — so `terrain_owned` is `Some`
                // exactly while Copernicus pixels can reach the glass. A
                // layer switched off, or one whose archive is dead so the
                // layer draws nothing, keeps its credit off the glass too: an
                // idle credit is clutter that dilutes the required ones, the
                // same rule `UNREACHABLE_ATTRIBUTION_TEXT` follows in owing
                // no copyright sign. The two archives are separate hosts, so
                // every base state — the source's own credit, the fallback,
                // the unreachable line — composes with the terrain credit
                // independently. The text is the drawn source's own, read off
                // `Tiles::attribution` exactly as the base's is; one
                // hyperlink still, and it keeps the base credit's target —
                // ODbL wants its notice reachable, and the Copernicus
                // obligation is the visible words.
                let credit_text = match terrain_owned.as_ref().map(walkers::Tiles::attribution) {
                    Some(terrain_credit) => std::borrow::Cow::Owned(format!(
                        "{} \u{b7} {}",
                        base_credit.text, terrain_credit.text
                    )),
                    None => std::borrow::Cow::Borrowed(base_credit.text),
                };
                self.draw_basemap_attribution(
                    ui,
                    panel_rect,
                    horizontal_color_scale,
                    pane_count,
                    &credit_text,
                    base_credit.url,
                );

                self.sync_viewports(&pre_zooms, &pre_positions);
            });

        self.map_tiles.restore_base_tiles(tiles_owned);
        self.map_tiles.restore_terrain_tiles(terrain_owned);

        actions
    }

    /// The lowest `y` this pane's colour scale may draw to.
    ///
    /// Two things push it up, and they are different in kind. The phone
    /// shell's bottom bar is **docked**: it is panel-wide, its height is known
    /// before the panes lay out, and a pane whose bottom is nowhere near it is
    /// unaffected because [`pane_render::clear_of_bottom_chrome`] no-ops when
    /// the floor is already below the pane. The **floating** chrome — the
    /// status bar and the timeline — is the part this function adds, and it is
    /// a bug fix: without it the scale draws *under* those surfaces and is
    /// simply not on screen. Measured at 402x874 the expanded transport spans
    /// `[0,772]-[402,825]` and swallowed the bar at `[16,789]-[386,809]`, its
    /// tick labels and its `dBZ` title whole; at 1400x900 the vertical scale's
    /// bottom labels sat inside the status bar at `[8,860]-[1394,892]`.
    ///
    /// **Only chrome that overlaps the columns this pane's scale occupies
    /// counts**, and the two orientations occupy different ones:
    ///
    /// * A **horizontal** scale is a band across the pane's whole width, so
    ///   any overlap at all hides part of it.
    /// * A **vertical** scale stands in the gutter along the pane's right
    ///   edge, so the span is [`pane_render::color_scale_gutter`] wide — the
    ///   painter's own answer, tick labels and unit title included, not the
    ///   bar alone. Testing the pane's literal right edge instead does not
    ///   work: the status bar is inset 6pt from the panel, so it would read as
    ///   missing a scale it covers outright. That was the first cut here, and
    ///   it left the desktop defect entirely unfixed.
    ///
    /// Either way a chip over one end of the bar takes that end's tick labels
    /// with it, so the whole scale lifts rather than being cropped.
    ///
    /// The gutter is measured against the **unclipped** pane rect, which is
    /// not circular: of everything `color_scale_gutter` reads, only its
    /// "pane too small for a bar" bail looks at the rect at all, and a pane
    /// that draws no bar has nothing for this floor to protect.
    ///
    /// The timeline form is chosen by `timeline_collapsed` rather than by
    /// probing both ids: egui keeps an `Area`'s last rect in memory after it
    /// stops being shown, so the form that is *not* up would answer with a
    /// stale rect and the scale would dodge a bar that is not there.
    ///
    /// There is no layout loop in reading the timeline's rect back: neither it
    /// nor the status bar positions itself off any colour-scale rect.
    fn color_scale_floor(
        &self,
        ui: &egui::Ui,
        panel_rect: egui::Rect,
        pane_rect: egui::Rect,
        pane_idx: usize,
        horizontal_color_scale: bool,
    ) -> f32 {
        let scale_left = if horizontal_color_scale {
            pane_rect.left()
        } else {
            pane_rect.right()
                - pane_render::color_scale_gutter(
                    ui.painter(),
                    pane_rect,
                    horizontal_color_scale,
                    pane_idx,
                    &self.panes[pane_idx],
                    &self.overlays,
                    &self.preferences,
                )
        };
        let timeline = egui::Id::new(if self.timeline_collapsed {
            "timeline_chip"
        } else {
            "timeline"
        });
        self.statusbar_rect
            .into_iter()
            .chain(ui.ctx().memory(|memory| memory.area_rect(timeline)))
            // `Rect::NOTHING` is inverted, so a surface that did not draw
            // fails this test rather than needing a case of its own.
            .filter(|bar| bar.max.x > scale_left && bar.min.x < pane_rect.right())
            .map(|bar| bar.top())
            .fold(panel_rect.bottom() - self.phone_bar_height, f32::min)
    }

    /// Paint the basemap credit in the map panel's bottom-right corner.
    ///
    /// Called once per panel. The corner is contested three ways and the credit
    /// gives way to all three, because it is the one surface there that may not
    /// be covered — an obscured notice satisfies neither ODbL nor CC-BY:
    ///
    /// * **The colour scale.** Its gutter owns the right edge of a landscape
    ///   pane and the bottom strip of a portrait one, tick labels included.
    ///   [`pane_render::color_scale_free_rect`] is the same answer every other
    ///   floating chrome positions against, so the credit asks it rather than
    ///   re-deriving the bar's arithmetic. It is a *per-pane* rect, so the
    ///   question is put to the pane holding the panel's bottom-right corner —
    ///   only that pane's legend can reach the credit.
    /// * **The floating status bar,** which spans nearly the panel's whole
    ///   width. It is drawn in `render_shell`, before the pane loop, so
    ///   `statusbar_rect` is this frame's.
    /// * **The timeline** — expanded transport, or the collapsed chip, which
    ///   anchors to this very corner. Both draw *after* the pane loop, so their
    ///   rects come from the layout memory the last frame left, exactly as
    ///   `pill_row_clearance` and `render_timeline_chip` read theirs.
    ///
    /// The lift test is whether a bar's span overlaps the credit's **own
    /// span**, `[left, right]`. It was the credit's right edge alone until the
    /// 2026-08-27 sweep, and that is precisely the defect that sweep found:
    /// the expanded transport is a fixed ~880pt-wide *centred* area, so on a
    /// panel wide enough to leave the corner free but not wide enough to clear
    /// the whole notice, the transport's right edge lands **inside** the
    /// credit's box. An edge test sees no collision there and does not lift,
    /// and the notice prints on the transport. Measured at 1200x874 the credit
    /// spanned `[994,1137]` against a transport ending at `1040` — 46pt of
    /// overlap the edge test could not see; at 800x874 with two panes it
    /// missed by 4pt. The whole 600-1000 medium band failed this way, in every
    /// pane count, whenever the transport was expanded.
    ///
    /// The span costs nothing in exactness: [`attribution_span`] lays the text
    /// out from the font rather than reading the last frame's rect, so it is
    /// as true on the first frame as the edge test was.
    ///
    /// A bar that stops short of the notice still does not lift it, which is
    /// what keeps the M8.1 behaviour the chip wants — a status bar collapsed to
    /// its restore button leaves the corner open map.
    ///
    /// **A portrait pane places the credit the other way up.** There the
    /// colour scale is a horizontal bar along the pane's bottom, and giving
    /// way to it upwards puts the notice *over* the bar, in the middle of the
    /// map. The user asked for it under the bar instead, so the horizontal arm
    /// hangs the credit off the top of
    /// [`pane_render::color_scale_under_rect`] — the bar's own pane-edge
    /// margin — with an `Align2::RIGHT_TOP` pivot, which puts it below the bar
    /// whatever height the notice lays out at rather than by arithmetic that
    /// would have to guess.
    ///
    /// That arm does **not** lift over bottom chrome, and it cannot: lifting
    /// is exactly what put the notice over the bar. Measured at 402x874 the
    /// cost is real and is not this function's to fix — the expanded transport
    /// spans `[0,772]-[402,825]` and so already covers the whole colour-scale
    /// strip, the bar at `[16,789]-[386,809]` and its `dBZ` title included.
    /// Anything under that bar is under the transport too. The credit is
    /// visible there only once the scale itself is, which is a question about
    /// `color_scale_floor`, not about where the credit hangs.
    fn draw_basemap_attribution(
        &mut self,
        ui: &egui::Ui,
        panel_rect: egui::Rect,
        horizontal_color_scale: bool,
        pane_count: usize,
        credit_text: &str,
        credit_url: &str,
    ) {
        let ctx = ui.ctx();

        let corner = panel_rect.max - egui::vec2(1.0, 1.0);
        let corner_idx = (0..pane_count)
            .find(|&idx| self.pane_layout.pane_rect(idx, panel_rect).contains(corner))
            .unwrap_or(0);
        let corner_rect = self.pane_layout.pane_rect(corner_idx, panel_rect);
        let chrome = pane_render::clear_of_bottom_chrome(
            corner_rect,
            self.color_scale_floor(
                ui,
                panel_rect,
                corner_rect,
                corner_idx,
                horizontal_color_scale,
            ),
        );
        let free = pane_render::color_scale_free_rect(
            ui.painter(),
            chrome,
            horizontal_color_scale,
            corner_idx,
            &self.panes[corner_idx],
            &self.overlays,
            &self.preferences,
        );

        let right = free.right() - ATTRIBUTION_INSET;
        let left = right - attribution_span(ui.painter(), credit_text);
        let (pivot, y) = match pane_render::color_scale_under_rect(chrome, horizontal_color_scale) {
            // Portrait: under the bar, hung off the top of the bar's own
            // margin so the notice's laid-out height cannot push it back over
            // the bar.
            Some(under) => (egui::Align2::RIGHT_TOP, under.top()),
            None => {
                // Which timeline form to ask about, by the flag rather than by
                // probing both ids: egui keeps an `Area`'s last rect in memory
                // after it stops being shown, so the form that is *not* up
                // would answer with a stale rect and the credit would dodge a
                // bar that is not there.
                let timeline = egui::Id::new(if self.timeline_collapsed {
                    "timeline_chip"
                } else {
                    "timeline"
                });
                let bottom = self
                    .statusbar_rect
                    .into_iter()
                    .chain(ctx.memory(|memory| memory.area_rect(timeline)))
                    .filter(|bar| bar.min.x <= right && left <= bar.max.x)
                    .map(|bar| bar.top() - ATTRIBUTION_INSET)
                    .fold(free.bottom() - ATTRIBUTION_INSET, f32::min)
                    // A panel whose whole bottom is chrome still keeps the
                    // notice on screen rather than lifting it off the top edge.
                    .max(free.top() + ATTRIBUTION_INSET + ATTRIBUTION_TEXT_SIZE);
                (egui::Align2::RIGHT_BOTTOM, bottom)
            }
        };

        let area = egui::Area::new(egui::Id::new("basemap_attribution"))
            .order(egui::Order::Middle)
            .pivot(pivot)
            .fixed_pos(egui::pos2(right, y))
            .show(ctx, |ui| {
                // A backdrop rather than a bare link: the notice is painted
                // straight onto radar over basemap, and a hyperlink-blue line
                // on a bright cell is not a legible notice. See
                // `super::shell::notice_frame` for why this one frame is not
                // `chrome_frame`.
                //
                // The words are `text_color`, not the stock `hyperlink_color`
                // a `Link` would paint itself in, and that is the *backdrop's*
                // doing: measured against `window_fill`, the link blue reads
                // 7.08:1 in dark but 2.77:1 in light, under the 4.5:1 a body
                // line needs. `text_color` is 5.12:1 and 7.59:1. Putting an
                // opaque surface behind the notice is what fixes the colour it
                // had to be legible against, so the colour moves with it. It
                // is still a link: pointer cursor, and a hover underline in
                // `hyperlink_color`.
                let words = ui.visuals().text_color();
                super::shell::notice_frame(ui.visuals()).show(ui, |ui| {
                    ui.hyperlink_to(
                        egui::RichText::new(credit_text)
                            .size(ATTRIBUTION_TEXT_SIZE)
                            .color(words),
                        credit_url,
                    );
                });
            });

        #[cfg(test)]
        self.probes.last_attribution.push(area.response.rect);
        #[cfg(not(test))]
        let _ = area;
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
            squallar_geo::GeoPoint {
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
            squallar_geo::GeoPoint {
                lat: position.y(),
                lon: position.x(),
            }
        };

        match gesture {
            ArmedDragGesture::Idle => {}
            ArmedDragGesture::Anchored(pos) => {
                self.region_drag =
                    crate::ui_region::RegionDrag::begin(pane_idx, ground(pos), voxel_pick_bounds());
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
                // The drag answers with a raw centre and half-width; making a
                // *resampler region* of it is this arm's business, with the
                // voxel bounds this arm owns.
                match drag.commit().and_then(|(centre, half_width_km)| {
                    crate::pane::VolumeRegion::new(
                        centre,
                        squallar_radar::voxel::HalfExtentKm::square(half_width_km),
                    )
                }) {
                    Some(region) => {
                        self.pending_region = Some((pane_idx, region));
                        self.set_region_pick_armed(false);
                    }
                    None => log::debug!(
                        "3D region drag was {:.1} km across, below the resampler's \
                         {:.0} km minimum; discarded",
                        2.0 * drag.half_width_km(),
                        2.0 * squallar_radar::voxel::MIN_HALF_WIDTH_KM,
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

    /// Advance the armed offline-download pick by one frame's gesture.
    ///
    /// [`Self::track_region_pick`]'s twin over the **same** drag, and the
    /// whole of the difference is the two things a drag was decoupled from its
    /// consumer to allow: the bounds handed to `begin` are this arm's, so a
    /// town under the resampler's 10 km floor commits, and what the committed
    /// centre-and-extent becomes is a picked box rather than a resampler
    /// region.
    fn track_download_pick(
        &mut self,
        pane_idx: usize,
        gesture: crate::ui_input::ArmedDragGesture,
        projector: &walkers::Projector,
    ) {
        use crate::ui_input::ArmedDragGesture;

        let ground = |pos: egui::Pos2| {
            let position = projector.unproject(egui::vec2(pos.x, pos.y));
            squallar_geo::GeoPoint {
                lat: position.y(),
                lon: position.x(),
            }
        };

        match gesture {
            ArmedDragGesture::Idle => {}
            ArmedDragGesture::Anchored(pos) => {
                self.download_drag = crate::ui_region::RegionDrag::begin(
                    pane_idx,
                    ground(pos),
                    crate::ui_download_area::download_pick_bounds(),
                );
            }
            ArmedDragGesture::Dragging(pos) => {
                if let Some(drag) = self
                    .download_drag
                    .as_mut()
                    .filter(|drag| drag.pane_idx() == pane_idx)
                {
                    drag.extend_to(ground(pos));
                }
            }
            ArmedDragGesture::Released(pos) => {
                let Some(mut drag) = self.download_drag.take() else {
                    return;
                };
                if drag.pane_idx() != pane_idx {
                    return;
                }
                drag.extend_to(ground(pos));
                match drag.commit().and_then(|(centre, half_width_km)| {
                    crate::ui_download_area::PickedBox::new(centre, half_width_km)
                }) {
                    Some(picked) => {
                        self.download_pick = Some(picked);
                        self.set_download_pick_armed(false);
                    }
                    None => log::debug!(
                        "the offline-area drag was {:.2} km across, under the {:.0} km a \
                         deliberate box starts at; discarded",
                        2.0 * drag.half_width_km(),
                        2.0 * crate::ui_download_area::MIN_DOWNLOAD_HALF_WIDTH_KM,
                    ),
                }
            }
            ArmedDragGesture::Cancelled => {
                if self
                    .download_drag
                    .is_some_and(|drag| drag.pane_idx() == pane_idx)
                {
                    self.download_drag = None;
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
            |p: squallar_geo::GeoPoint| projector.project(walkers::lat_lon(p.lat, p.lon)).to_pos2();
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
            squallar_geo::GeoPoint {
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
            |p: squallar_geo::GeoPoint| projector.project(walkers::lat_lon(p.lat, p.lon)).to_pos2();

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
                squallar_radar::voxel::HalfExtentKm::square(half_width_km),
            )
        {
            paint_region_box(painter, rect, crate::ui_region::REGION_ARM_COLOR, true);
            if let Some(text) = region_hint_text(half_width_km, cells_across) {
                paint_region_hint(painter, rect, &text, crate::ui_region::REGION_ARM_COLOR);
            }
        }

        self.draw_download_box(ui, projector, pane_idx);

        #[cfg(test)]
        self.probes.last_region_boxes.extend(painted);
    }

    /// The offline-download box: the one being dragged on this pane, and the
    /// committed one, each with the chip that describes it.
    ///
    /// The committed box draws on **every** map pane rather than only the one
    /// it was picked on: a downloaded area is a fact about the device rather
    /// than about a window, so a second map over the same ground shows it too.
    fn draw_download_box(
        &self,
        ui: &egui::Ui,
        projector: &walkers::Projector,
        pane_idx: crate::pane::PaneId,
    ) {
        let painter = ui.painter();
        let color = crate::ui_download_area::DOWNLOAD_ARM_COLOR;

        if let Some(picked) = self.download_pick
            && let Some(rect) = region_screen_rect(projector, picked.centre, picked.half_extent())
        {
            paint_region_box(painter, rect, color, false);
            paint_region_hint(painter, rect, &self.download_hint_text(picked), color);
        }

        if let Some((centre, half_width_km)) = self.download_preview(pane_idx)
            && let Some(rect) = region_screen_rect(
                projector,
                centre,
                squallar_radar::voxel::HalfExtentKm::square(half_width_km),
            )
        {
            paint_region_box(painter, rect, color, true);
            // Mid-drag the sentence is the width alone. A size figure is exact
            // or it is nothing, and measuring one per frame of a drag would
            // spend an archive read on a box the pointer has already left.
            paint_region_hint(
                painter,
                rect,
                &format!("{:.0} km", 2.0 * half_width_km),
                color,
            );
        }
    }

    /// The committed download box's chip: its width, its detail level, and the
    /// exact size that level costs once it has been measured.
    ///
    /// **The same widget as the 3D pick's `km/cell` chip with a different
    /// sentence** — the box drawn is the box described, in both arms.
    fn download_hint_text(&self, picked: crate::ui_download_area::PickedBox) -> String {
        let level = self.download_detail;
        format!(
            "{:.0} km - {} - {}",
            picked.across_km(),
            level.label(),
            self.download_size.size_label(level),
        )
    }

    /// Keep the size figure moving.
    ///
    /// **The only frame-path work the selection UI does**, and it is
    /// bookkeeping:
    /// [`AreaSizeProbe::poll`](crate::ui_download_area::AreaSizeProbe::poll)
    /// reads a `OnceLock` and may hand a task to the IO runtime. No archive is
    /// opened here and no byte is summed here.
    ///
    /// **Publishing a finished run is not here.** `settle_offline_download`
    /// rides the per-frame drive rather than any screen, because a download
    /// completes whether or not anyone is watching, and a second publish path
    /// would be a second chance to disagree about what finished.
    pub(super) fn pump_download_area(&mut self, ctx: &egui::Context) {
        self.download_size.set_box(self.download_pick);
        self.download_size
            .set_terrain(self.download_wants_terrain());
        // The same switch `go_offline_for_tests` throws for the tile slots, for
        // the same reason: a unit test's Gui must open no range source against
        // the production archive. Always live outside a harness.
        let live_archive = !self.map_tiles.is_offline();
        self.download_size.poll(
            ctx,
            || {
                live_archive
                    .then(crate::tiles::archive_range_source)
                    .and_then(Result::ok)
            },
            || {
                live_archive
                    .then(crate::tiles::terrain_range_source)
                    .and_then(Result::ok)
            },
        );
    }

    /// Whether a download started now would fetch the terrain hillshade.
    ///
    /// The user's explicit choice if they made one, and otherwise **whatever
    /// the Base Map inspector's "Terrain shading" switch says right now** — so
    /// an area holds what the map on the glass is showing rather than a
    /// default nobody picked. Read across the visible panes exactly as
    /// `ensure_terrain_tiles` reads it, so the checkbox and the layer cannot
    /// come to two answers about whether shading is on.
    pub(super) fn download_wants_terrain(&self) -> bool {
        self.download_terrain.unwrap_or_else(|| {
            self.panes[..self.visible_pane_count()]
                .iter()
                .any(|pane| pane.is_overlay_enabled(&known::TERRAIN))
        })
    }

    /// The picked box's level list: three depths, each with the exact size it
    /// costs, and the one action that spends it.
    pub(super) fn render_download_area(&mut self, ctx: &egui::Context, map_rect: egui::Rect) {
        use crate::ui_download_area::{DETAIL_LEVELS, quota_shortfall, shortfall_action_label};

        let Some(picked) = self.download_pick else {
            return;
        };
        let sizes = self.download_size.sizes();
        let spec_id = self
            .download_size
            .area_spec(self.download_detail)
            .map(|spec| spec.area_id);
        let free = crate::ui_download_area::free_space(self.download_quota);
        let short = quota_shortfall(&sizes, self.download_detail, free);
        // The one in-flight run the app has, whichever surface started it, so
        // this panel and the Downloaded areas screen show the same download
        // rather than two views that could disagree.
        let progress = self
            .active_download
            .as_ref()
            .filter(|active| Some(&active.spec.area_id) == spec_id.as_ref())
            .map(crate::basemap_areas::ActiveDownload::progress);

        let mut start = false;
        let mut clear = false;
        let mut cancel = false;
        let mut choose = None;
        let terrain = self.download_wants_terrain();
        let mut terrain_choice = None;

        egui::Area::new(egui::Id::new("download_area_panel"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_TOP)
            .fixed_pos(map_rect.left_top() + egui::vec2(PANEL_MARGIN, PANEL_MARGIN))
            .show(ctx, |ui| {
                super::shell::chrome_frame(&ctx.global_style()).show(ui, |ui| {
                    ui.set_width(DOWNLOAD_PANEL_WIDTH);
                    ui.label(egui::RichText::new(DOWNLOAD_PANEL_TITLE).strong());
                    ui.label(format!("{:.0} km across", picked.across_km()));
                    ui.separator();
                    ui.label(DETAIL_LEVEL_HEADING);
                    for level in DETAIL_LEVELS {
                        let size = self.download_size.size_label(level);
                        let row = ui.selectable_label(
                            level == self.download_detail,
                            format!("{}  -  {size}", level.label()),
                        );
                        if row.clicked() {
                            choose = Some(level);
                        }
                    }

                    // The hillshade is a second archive and a second cost, so
                    // it is asked for here rather than assumed: every size in
                    // the list above is the figure for the archives this box is
                    // ticked for, and ticking it moves them all.
                    let mut wants_terrain = terrain;
                    if ui
                        .checkbox(&mut wants_terrain, TERRAIN_INCLUDE_LABEL)
                        .changed()
                    {
                        terrain_choice = Some(wants_terrain);
                    }

                    if let Some(short) = short {
                        ui.separator();
                        ui.label(crate::ui_download_area::shortfall_line(short));
                        if let Some(alternative) = short.alternative
                            && ui.button(shortfall_action_label(alternative)).clicked()
                        {
                            choose = Some(alternative);
                        }
                    }

                    ui.separator();
                    match progress {
                        Some(progress) => {
                            // The same block the Downloaded areas screen
                            // draws: one in-flight run, one shape, so the two
                            // views cannot come to two answers about it.
                            crate::ui_download_area::render_download_progress(ui, progress);
                            if ui.button(DOWNLOAD_CANCEL_LABEL).clicked() {
                                cancel = true;
                            }
                        }
                        None => {
                            ui.horizontal(|ui| {
                                // Refused while the level's figure is not in
                                // hand: starting a download whose size we
                                // cannot yet state is starting one the user
                                // did not agree to.
                                let ready = self.download_size.size(self.download_detail).is_some();
                                if ui
                                    .add_enabled(ready, egui::Button::new(DOWNLOAD_START_LABEL))
                                    .clicked()
                                {
                                    start = true;
                                }
                                if ui.button(DOWNLOAD_DISMISS_LABEL).clicked() {
                                    clear = true;
                                }
                            });
                        }
                    }
                });
            });

        if let Some(level) = choose {
            self.download_detail = level;
        }
        if let Some(wants) = terrain_choice {
            // Latched the moment it is touched: from here the checkbox is the
            // user's answer and no longer the switch's.
            self.download_terrain = Some(wants);
        }
        if clear {
            self.clear_download_pick();
        }
        if cancel {
            // Dropping the engine is the whole cancel protocol; the segments
            // already written stay, and a later start completes the
            // difference. The box stays picked - cancelling a download is not
            // un-choosing the ground.
            self.active_download = None;
        }
        if start && let Some(spec) = self.download_size.area_spec(self.download_detail) {
            // `start_area_download` and nothing else, so this button, Resume
            // and Update all reach the engine the same way and there is no
            // second opinion about where segments live.
            let terrain = self.download_wants_terrain();
            self.start_area_download(spec, terrain, ctx);
        }
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
            terrain,
            tile_zoom_bias,
            horizontal_color_scale,
            color_scale_floor,
            user_location,
            user_heading,
            user_fix,
            actions,
        } = floor;
        use walkers::Map;

        let overlay_render_limit = self.concurrent_renders;

        // `tiles` stays an `Option`: the strip is the pane's projector and
        // floor for every ground layer, not just the base tiles, so a released
        // base slot (BasemapTiles off everywhere) must not take the whole
        // floor with it.
        let strip = strip?;
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
        // Read before the closure takes `pane`, and read from the same
        // function the renderer's own `heights` comes from -- see
        // `pane_ground_heights`. One expression, and the only binding of this
        // name in the module: a second one composing the answer out of
        // something else is what `GroundIsMesh` and the source pin in
        // `ui_map_pane/floor_strip_shading_tests.rs` exist to refuse.
        let draws_3d_ground = pane_render::GroundIsMesh::from_height_field(
            pane_ground_heights(pane, pane_idx).as_deref(),
        );

        Map::new(None, map_memory, center)
            .zoom_with_ctrl(false)
            .panning(false)
            // The strip is a pure projector, not a second thing to zoom. Its
            // `MapMemory` is an owned copy (`FloorStripCtx::map_memory`) that is
            // dropped at the end of this function, so a zoom gesture here writes
            // to a throwaway — and it would read the same wheel the plan view
            // does, at a different rate, because only the plan view asks for a
            // nominal frame time.
            .zoom_gesture(false)
            .drag_pan_buttons(egui::DragPanButtons::empty())
            .show(&mut strip_ui, |ui, _response, projector, memory| {
                let zoom = memory.zoom();

                self.map_pane_geo
                    .insert(pane_idx, map_pane_geo_from(projector, strip));

                let mut render_ctx = pane_render::PaneRenderCtx {
                    pane_idx,
                    pane,
                    overlays: &mut self.overlays,
                    user_location,
                    user_heading,
                    user_fix,
                    basemap_labels: Vec::new(),
                    basemap_tiles: tiles,
                    terrain_tiles: terrain,
                    tile_zoom_bias,
                    overlay_render_limit,
                    actions,
                    pane_rect: strip,
                    surfaces: pane_render::PaneSurfaces::GroundOnly,
                    draws_3d_ground,
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
                &on_screen,
                elevation,
            );
        }
    }
}

/// The 3D pick's drag bounds: what the voxel resampler will build. The drag
/// gesture no longer knows them — this arm owns them, which is what lets a
/// different arm hand the same gesture different bounds.
fn voxel_pick_bounds() -> crate::ui_region::DragBoundsKm {
    crate::ui_region::DragBoundsKm {
        min_half_width_km: squallar_radar::voxel::MIN_HALF_WIDTH_KM,
        max_half_width_km: squallar_radar::voxel::MAX_HALF_WIDTH_KM,
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
    /// The terrain slot's source, when the frame holds one — the floor strip
    /// draws the same ground the plan view does, hillshade included.
    terrain: Option<&'a mut crate::tile_source::HttpsTiles>,
    tile_zoom_bias: u8,
    horizontal_color_scale: bool,
    /// See [`pane_render::PaneRenderCtx::color_scale_floor`]. Carried through
    /// the floor strip only so the `PaneRenderCtx` it builds is complete; the
    /// strip itself is `GroundOnly` and paints no legend.
    color_scale_floor: f32,
    user_location: Option<(f64, f64)>,
    user_heading: Option<f32>,
    user_fix: Option<squallar_location::Fix>,
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
                squallar_radar::voxel::HalfExtentKm {
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
    current_stamp: Option<crate::radar_layer::CurrentVolumeStamp>,
    chrome: Option<f32>,
    source_geo: Option<crate::volume_view::MapPaneGeo>,
    mirror_size_points: egui::Vec2,
    actions: &mut Vec<GuiAction>,
    alpha_curves: &mut crate::volume_alpha::AlphaCurves,
    iso_thresholds: &crate::volume_iso::IsoThresholds,
    volume_ask: &Result<crate::pane::VolumeAsk, String>,
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
        volume_ask,
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

/// The height field pane `pane_idx`'s ground is drawn as, or `None` while it
/// has none and the pane draws the flat map floor at box `z = 0`.
///
/// **The one answer to "does this pane draw 3D ground", and it has two
/// readers.** `volume_pane_outcome` puts it on the `VolumeFrameState` the
/// renderer decides the mesh from, and `draw_floor_strip` asks whether the
/// strip must suppress the hillshade
/// ([`PaneRenderCtx::draws_3d_ground`](pane_render::PaneRenderCtx::draws_3d_ground)).
/// Those two must never disagree -- a strip that dropped the hillshade for a
/// pane drawing the flat lid would lose shading the lid has no other source
/// of, and a strip that kept it for a pane drawing the mesh would shade that
/// mesh twice -- so they read one function rather than two beliefs.
///
/// **Owed, and named here rather than left as a shrug.** B3 built the whole
/// path a field travels -- archive tiles, the offload row, the resample, the
/// `R16Uint` upload, the mesh, the drape -- and proved it end to end against a
/// fixture archive. What it did not build is the scheduler that decides when a
/// pane asks for one, because the archive A2 would fetch from is not
/// published: `HEIGHT_ARCHIVE_URL` still carries `UNPUBLISHED-GENERATION`, so
/// a wired request would 404 on every tile and the pane would draw exactly
/// this `None` anyway. **So it answers `None` for every pane today**, which is
/// why nothing that gates on it can be proven by driving the app: the
/// two-directional evidence for the suppression is at
/// `render_pane_map_content`'s own seam, where both answers are constructible
/// (`ui_map_pane/floor_strip_shading_tests.rs`).
///
/// Whoever wires the scheduler fills in this body, and both readers flip
/// together because there is only the one.
fn pane_ground_heights(
    _pane: &crate::pane::PaneState,
    _pane_idx: usize,
) -> Option<std::sync::Arc<crate::volume_view::GroundHeightField>> {
    None
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
    current_stamp: Option<crate::radar_layer::CurrentVolumeStamp>,
    source_geo: Option<crate::volume_view::MapPaneGeo>,
    mirror_size_points: egui::Vec2,
    actions: &mut Vec<GuiAction>,
    alpha_curves: &crate::volume_alpha::AlphaCurves,
    iso_thresholds: &crate::volume_iso::IsoThresholds,
    // Which of this pane's layers will build the grid and of which field, or
    // the sentence to paint instead. Resolved by `PaneState::volume_ask` one
    // level up, where the layer registry is in scope.
    volume_ask: &Result<crate::pane::VolumeAsk, String>,
) -> VolumeOutcome {
    use crate::pane::OrbitDelta;
    use crate::volume_view::{VolumeFrameState, VolumePaint};

    let Some((camera_before, view_mode, region)) =
        pane.volume().map(|v| (v.camera, v.view_mode, v.region))
    else {
        return VolumeOutcome::empty_state(VOLUME_EMPTY_STATE.to_owned());
    };
    // The site the box falls back to when no region is picked — the same
    // fallback `volume_job_context` resolves on the app side, so the floor the
    // pane frames and names is the floor the resampler will build.
    let site_geo =
        squallar_radar::sites::get_radar_site(pane.site()).map(|site| squallar_geo::GeoPoint {
            lat: site.lat,
            lon: site.lon,
        });
    let base_km_msl = crate::pane::volume_base_km_msl(region, site_geo);
    let mut box_size_km = crate::pane::box_size_km_for_base(region, base_km_msl);
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
    let loop_grid = pane.active_volume_frame().cloned();
    // Which volume this pane is about, live or navigated. Answered by the
    // pane so the arrival path (WO-M14c) reads the same answer from the same
    // place rather than re-deriving it a frame earlier.
    let stamp = pane.volume_stamp(current_stamp);

    let Some(volume) = pane.volume_mut() else {
        return VolumeOutcome::empty_state(VOLUME_EMPTY_STATE.to_owned());
    };
    volume.camera.nudge(delta);
    let camera = volume.camera;
    let floor = !volume.hide_floor;
    let accurate_light = volume.sun_lighting;
    // Cleared here and set again below only if this arm gets as far as
    // building a frame, so a pane that ends up drawing an empty state does not
    // leave its control claiming a light for a picture that is not there.
    volume.shown_light = None;

    let Some(painter) = painter else {
        return VolumeOutcome::empty_state(VOLUME_EMPTY_STATE.to_owned());
    };
    let Some((volume_stamp, base_started)) = stamp else {
        return VolumeOutcome::empty_state(format!(
            "Downloading the first {site_code} volume...\n\nThe 3D view builds the moment it \
             lands, then updates tilt by tilt as new sweeps arrive.",
        ));
    };
    // **The gate is a layer's own answer, not this pane's guess.** The walk
    // that produced it asked every enabled slot in turn whether it has a 3D
    // half and whether that half builds the field the slot itself says it is
    // showing; nothing here matched on an id, and nothing here read the derive
    // table. A pane with no qualifying slot gets the sentence the walk wrote,
    // which names the layer and the field.
    let ask = match volume_ask {
        Ok(ask) => ask,
        Err(why) => return VolumeOutcome::empty_state(why.clone()),
    };
    let product = ask.field.clone();

    let target = match loop_grid {
        Some(grid) => grid.target,
        None => pane.volume_target_for(&product, volume_stamp),
    };
    let collected = target.volume.collected;
    // **The one light both surfaces get**, off the volume's own collection
    // time rather than the pane's playhead: `TimeMode::as_of` is `None` on a
    // live pane by design, which is the commonest pane there is, and the
    // volume's collection time is what the picture actually depicts whether
    // the pane is live or scrubbed.
    let light = crate::volume_view::volume_light(
        accurate_light,
        crate::pane::volume_box_anchor(region, site_geo),
        crate::volume_view::unix_seconds_of(collected),
    );
    // **Recorded, so the control reports the picture rather than deriving a
    // second answer.** The two derivations diverged in two routine states -
    // see `VolumePane::shown_light`.
    if let Some(volume) = pane.volume_mut() {
        volume.shown_light = Some(light);
    }
    // **The level trigger, and the whole of it.** `volume_build_due` holds
    // all three refusals — not in Volume mode, playing a 3D loop, already
    // rendered for this target — and the arrival path asks the same function
    // at install time, so an eager build quiesces this arm rather than racing
    // it.
    if pane.volume_build_due(&target) {
        actions.push(GuiAction::PrepareVolume {
            pane_idx,
            layer: ask.layer.clone(),
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
        // WO-M14a: keyed by the pane's OWN field id. WO-E9c wrote this as a
        // product -> id bridge and E9e re-typed the selection to an id, which
        // left a FieldId -> spec -> FieldId round trip whose only effect was
        // to substitute the default field's key for an unregistered one.
        alpha: alpha_curves.get(&product),
        view_mode,
        light,
        iso_threshold: iso_thresholds.get(&product),
        heights: pane_ground_heights(pane, pane_idx),
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
            let drawn = CaptionBox {
                half: squallar_radar::voxel::HalfExtentKm {
                    east_km: 0.5 * f64::from(drawn_box_km[0]),
                    north_km: 0.5 * f64::from(drawn_box_km[1]),
                },
                base_km_msl: floor_of_drawn_box(drawn_box_km),
            };
            paint_volume_caption(
                ui,
                pane_rect,
                crate::ui::pills::pill_row_clearance(ui.ctx(), pane_idx),
                &volume_caption(
                    &site_code,
                    collected,
                    base_started,
                    drawn,
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
            // The slider's travel and the meaning of its number are the
            // field's own registered facts now, read once. The `_ => 0.0..=1.0`
            // wildcard that used to answer for a field with no stated domain is
            // gone: a field with no vertical extent never reaches this arm, and
            // gets the refusal plate above instead.
            let facts = crate::field_facts::facts(&product);
            let (prefix, suffix) = facts.domain_label_ends;
            let (domain_start, domain_end) = facts.value_domain;
            let mut threshold = iso_thresholds.get(&facts.id);
            ui.horizontal(|ui| {
                ui.label(format!("{prefix}:"));
                let response = ui.add(
                    egui::Slider::new(&mut threshold, domain_start..=domain_end)
                        .suffix(suffix)
                        .fixed_decimals(if domain_end <= 4.0 { 2 } else { 0 }),
                );
                if response.changed() {
                    iso_thresholds.set(&facts.id, threshold);
                }
                response.on_hover_text(format!(
                    "Where {}'s surface sits. Per product - every 3D pane showing this \
                     product shares it.",
                    facts.name,
                ));
            });
            if alpha_curves.is_edited(&facts.id) {
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

        let mut sunlight = volume.sun_lighting;
        if ui
            .checkbox(&mut sunlight, SUN_LIGHT_LABEL)
            .on_hover_text(
                "Lights the ground and the storm above it by the real sun, from where the box \
                 is on Earth and when the volume was collected - warm and low near sunrise \
                 and sunset, cool and dim through twilight, and down to a night floor the \
                 terrain still reads by. One light, so the two surfaces always agree. Turn it \
                 off for the fixed studio light, which is dimmer nowhere and is the readable \
                 choice at night.",
            )
            .changed()
        {
            volume.sun_lighting = sunlight;
        }
        // **What the picture is actually under, read off the frame that drew
        // it.** The arithmetic refuses a timestamp it cannot honour rather
        // than answering a plausible night, and the pane then draws under the
        // readable light; saying so here is what stops that being a silent
        // substitution.
        //
        // `shown_light` and not a second derivation: the 3D arm records the
        // light it actually sent the painter, so this reports the picture on
        // the glass. Deriving it again here read a different instant - the
        // target INSTALLED rather than the one asked for - which diverges
        // through every loop and every rebuild.
        match volume.shown_light {
            Some(crate::volume_view::VolumeLight::Sun(sun)) => {
                ui.label(
                    egui::RichText::new(format!(
                        "Sun {:.1}\u{b0} {} the horizon.",
                        sun.elevation_deg.abs(),
                        if sun.elevation_deg >= 0.0 {
                            "above"
                        } else {
                            "below"
                        },
                    ))
                    .small()
                    .weak(),
                );
            }
            Some(crate::volume_view::VolumeLight::Headlight) if sunlight => {
                ui.label(egui::RichText::new(SUN_UNPLACEABLE_NOTE).small().weak());
            }
            Some(crate::volume_view::VolumeLight::Headlight) | None => {}
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

/// **The floor a drawn box stands on, km MSL** — its fixed top less its own
/// vertical span.
///
/// The caption has to describe **one** box. While a stand-in grid is up the
/// drawn box is the one that was BUILT and the pane's region is the one that
/// was PICKED, so taking the horizontal from the drawn box and re-deriving the
/// floor from the live region would caption a stale width beside a fresh
/// height — the same lying readout this arm exists to stop.
///
/// Two facts license reading the floor back out of the span rather than asking
/// the painter for it. `DrawnBox` never rescales the vertical: a cropped box
/// carries `z_km_msl: settled.z_km_msl`, the held grid's own. And
/// `request_for` writes `DEFAULT_TOP_KM_MSL` on every request it shapes, which
/// `the_box_top_and_the_value_plane_are_decided_on_this_side_of_the_seam`
/// pins. So the top is known and the span is measured, and the floor is the
/// difference.
fn floor_of_drawn_box(box_km: [f32; 3]) -> f64 {
    squallar_radar::voxel::DEFAULT_TOP_KM_MSL - f64::from(box_km[2])
}

/// The box a caption describes: how far it reaches either side on the ground,
/// and where its bottom face sits.
///
/// One parameter rather than two because the caption's vertical line is a
/// *statement about this box*, and a floor that follows the ground can only be
/// carried alongside the extent that chose it. Both halves come off the same
/// pick.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CaptionBox {
    half: squallar_radar::voxel::HalfExtentKm,
    /// Where the bottom face sits, km MSL — `crate::pane::volume_base_km_msl`
    /// of the same pick the resampler is given.
    base_km_msl: f64,
}

/// What the pane says about the picture it is showing, one line per fact.
fn volume_caption(
    site: &str,
    newest: chrono::NaiveDateTime,
    base_started: Option<chrono::NaiveDateTime>,
    drawn: CaptionBox,
    camera: crate::pane::OrbitCamera,
    showing: crate::volume_view::Showing,
    cells: Option<usize>,
) -> Vec<String> {
    let half = drawn.half;
    let mut lines = vec![format!(
        "{site} volume - newest data {}Z",
        newest.format("%H:%M")
    )];

    match base_started {
        Some(base) => lines.push(format!("base volume {}Z", base.format("%H:%M"))),
        None => lines.push("no complete volume yet - showing the tilts flown so far".to_owned()),
    }

    // **The floor is read, not assumed.** This line said "0" for every box on
    // Earth while the resampler's floor followed the ground, which for a
    // feature whose whole reason is being accurate about a slice of the earth
    // is the one thing it may not do.
    let base = drawn.base_km_msl * KFT_PER_KM;
    let top = squallar_radar::voxel::DEFAULT_TOP_KM_MSL * KFT_PER_KM;
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

/// The basemap credit's gap from whatever bounds it: the colour scale's free
/// rect on both axes, and the top edge of any bottom chrome it had to lift
/// over. The status bar's own `BAR_INSET` is the same 8pt, so a credit resting
/// on the bar reads as one gap rather than two different ones.
const ATTRIBUTION_INSET: f32 = 8.0;

/// Point size for the credit. Small on purpose: it is a legal notice that has
/// to be legible, not a label competing with the map.
const ATTRIBUTION_TEXT_SIZE: f32 = 10.0;

/// How wide the credit will lay out, `notice_frame`'s side margins included.
///
/// Measured from the font here rather than read back off the last frame's
/// `Area` response, because the placement that needs it runs *before* the area
/// is shown: on the first frame the response rect is still `Rect::NOTHING`,
/// and a lift test reading it would put the notice on the very chrome it is
/// supposed to clear, then correct itself a frame later as a visible jump.
///
/// The frame's own padding comes from [`super::shell::NOTICE_MARGIN_X`], not a
/// second spelling of it, so the two cannot drift.
fn attribution_span(measure: &egui::Painter, credit: &str) -> f32 {
    let text = measure
        .layout_no_wrap(
            credit.to_owned(),
            egui::FontId::proportional(ATTRIBUTION_TEXT_SIZE),
            egui::Color32::WHITE,
        )
        .rect
        .width();
    text + 2.0 * f32::from(super::shell::NOTICE_MARGIN_X)
}

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
    centre: squallar_geo::GeoPoint,
    half: squallar_radar::voxel::HalfExtentKm,
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

/// The download panel's inset from the map's top-left corner, points.
const PANEL_MARGIN: f32 = 12.0;

/// How wide the download panel draws. Wide enough for the longest level name
/// beside a `310 MB`, narrow enough to leave the map readable behind it at
/// phone width.
const DOWNLOAD_PANEL_WIDTH: f32 = 260.0;

/// The download panel's title.
pub(crate) const DOWNLOAD_PANEL_TITLE: &str = "Make available offline";

/// The heading over the three depths.
pub(crate) const DETAIL_LEVEL_HEADING: &str = "Detail level";

/// The hillshade checkbox's label — the Base Map inspector's own words for the
/// same thing, so the switch on the map and the box in this panel read as one
/// feature rather than two.
pub(crate) const TERRAIN_INCLUDE_LABEL: &str = "Terrain shading";

/// The panel's three buttons.
pub(crate) const DOWNLOAD_START_LABEL: &str = "Download";
/// See [`DOWNLOAD_START_LABEL`].
pub(crate) const DOWNLOAD_CANCEL_LABEL: &str = "Cancel download";
/// See [`DOWNLOAD_START_LABEL`].
pub(crate) const DOWNLOAD_DISMISS_LABEL: &str = "Clear box";

/// One box's own sentence, over its top edge, on a chip in the arm's colour.
///
/// The 3D pick spends it on `"{across} km - {km}/cell"`; the download arm on
/// the box's width, its detail level and the exact size that costs. One
/// widget, two sentences — which is the whole of the difference between the
/// two arms' chips.
fn paint_region_hint(painter: &egui::Painter, rect: egui::Rect, text: &str, color: egui::Color32) {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.0),
        egui::Color32::from_rgb(20, 20, 20),
    );
    let origin = egui::pos2(rect.left(), rect.top() - galley.size().y - 4.0);
    painter.rect_filled(
        egui::Rect::from_min_size(origin, galley.size()).expand(3.0),
        2.0,
        color,
    );
    painter.galley(origin, galley, egui::Color32::PLACEHOLDER);
}

/// The hint's text for a drag standing at `half_width_km`, or `None` for a box
/// that cannot be described.
fn region_hint_text(half_width_km: f64, cells: Option<usize>) -> Option<String> {
    let region = crate::pane::VolumeRegion::new(
        squallar_geo::GeoPoint { lat: 0.0, lon: 0.0 },
        squallar_radar::voxel::HalfExtentKm::square(half_width_km),
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
    project: impl Fn(squallar_geo::GeoPoint) -> egui::Pos2,
) -> Vec<egui::Pos2> {
    let a = (line.a().lat, line.a().lon);
    let b = (line.b().lat, line.b().lon);
    (0..=SECTION_TRACK_SAMPLES)
        .map(|i| {
            let t = i as f64 / SECTION_TRACK_SAMPLES as f64;
            let (lat, lon) = squallar_geo::great_circle_point(a, b, t);
            project(squallar_geo::GeoPoint { lat, lon })
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

/// The group accent bar's thickness, logical pixels.
const GROUP_BAR_HEIGHT: f32 = 3.0;

/// One dash of a partial member's accent bar, and the gap after it.
const GROUP_DASH: f32 = 9.0;
const GROUP_DASH_GAP: f32 = 6.0;

/// The group tab's box, at the accent bar's right end.
const GROUP_TAB_WIDTH: f32 = 17.0;
const GROUP_TAB_HEIGHT: f32 = 14.0;
const GROUP_TAB_FONT: f32 = 10.0;

/// **What a pane's border has to say about its links**, beside whether it is
/// the active one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneBorderMarks {
    pub is_active: bool,
    /// The group this pane belongs to, or `None` for a pane in no group —
    /// which paints no accent at all, because "with nobody" is the absence of
    /// a group and not a seventh colour.
    pub group: Option<crate::pane::GroupId>,
    /// In a group, opted out of at least one dimension. Marked by **breaking
    /// the accent bar into dashes**, not by a second colour: a hue difference
    /// is exactly what a theme, a projector or a colour-blind reader can
    /// collapse, and a solid line against a broken one survives all three.
    pub partial: bool,
}

/// Draw a border around a pane rect: the stroke says whether this is the
/// active pane, and the accent along its top edge says which link group it
/// belongs to. Returns the painted stroke's bounds, for the M8 containment
/// pin.
///
/// **The two channels are deliberately separate.** Active/inactive keeps the
/// blue-2px / grey-1px vocabulary it has always had, so nothing about
/// selection changes; the group is an addition beside it, drawn inside the
/// stroke and above the pill row's 8px inset, where nothing else paints.
///
/// The accent is drawn on its own dark backing rather than in a theme colour,
/// because it sits on map tiles: the background under it has nothing to do
/// with the app theme, so the same pixels are correct in both. See
/// [`crate::pane::GroupId::accent`].
fn draw_pane_border(
    ui: &mut egui::Ui,
    pane_rect: egui::Rect,
    marks: PaneBorderMarks,
) -> egui::Rect {
    let border_color = if marks.is_active {
        egui::Color32::from_rgb(60, 140, 255)
    } else {
        egui::Color32::from_rgba_unmultiplied(128, 128, 128, 100)
    };
    let stroke_width = if marks.is_active { 2.0 } else { 1.0 };
    let kind = egui::StrokeKind::Inside;
    ui.painter().rect_stroke(
        pane_rect,
        0.0,
        egui::Stroke::new(stroke_width, border_color),
        kind,
    );
    if let Some(group) = marks.group {
        draw_group_accent(ui.painter(), pane_rect, stroke_width, group, marks.partial);
    }
    match kind {
        egui::StrokeKind::Inside => pane_rect,
        egui::StrokeKind::Middle => pane_rect.expand(stroke_width / 2.0),
        egui::StrokeKind::Outside => pane_rect.expand(stroke_width),
    }
}

/// The group half of [`draw_pane_border`]: the accent bar across the top edge
/// — solid for a full member, dashed for a partial one — and the lettered tab
/// at its right end, which is what names the group for a reader who cannot
/// tell the hues apart.
fn draw_group_accent(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    inset: f32,
    group: crate::pane::GroupId,
    partial: bool,
) {
    let accent = group.accent();
    // The backing: one dark pixel of margin under everything the accent
    // paints, so a pale hue over a pale tile still has an edge.
    let backing = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 150);
    let top = pane_rect.top() + inset;
    let left = pane_rect.left() + inset;
    let right = pane_rect.right() - inset;
    if right - left < GROUP_TAB_WIDTH + 2.0 {
        return;
    }

    let bar = egui::Rect::from_min_max(
        egui::pos2(left, top),
        egui::pos2(right, top + GROUP_BAR_HEIGHT),
    );
    painter.rect_filled(bar.expand(1.0), 0.0, backing);
    if partial {
        let mut x = bar.left();
        while x < bar.right() {
            let end = (x + GROUP_DASH).min(bar.right());
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x, bar.top()), egui::pos2(end, bar.bottom())),
                0.0,
                accent,
            );
            x = end + GROUP_DASH_GAP;
        }
    } else {
        painter.rect_filled(bar, 0.0, accent);
    }

    // The tab hangs off the bar's right end: the pill row starts 8px in from
    // the pane's left, and the colour scale's title sits ~16px down its right
    // edge, so this band is the one place on a pane nothing else claims.
    let tab = egui::Rect::from_min_max(
        egui::pos2(right - GROUP_TAB_WIDTH, bar.bottom()),
        egui::pos2(right, bar.bottom() + GROUP_TAB_HEIGHT),
    );
    painter.rect_filled(tab.expand(1.0), 2.0, backing);
    painter.rect_filled(tab, 2.0, accent);
    painter.text(
        tab.center(),
        egui::Align2::CENTER_CENTER,
        group.letter(),
        egui::FontId::proportional(GROUP_TAB_FONT),
        group.accent_ink(),
    );
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
    product: &FieldId,
    prefs: &UserPreferences,
) -> String {
    let (azimuth, distance_km) = squallar_geo::site_bearing_range_km(
        input.site_lat,
        input.site_lon,
        input.hover_lat,
        input.hover_lon,
    );

    let value_str = match hover.read(azimuth, distance_km) {
        Reading::Value(value) => format!(
            "| {}",
            crate::field_facts::format_value(product, value, prefs)
        ),
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

#[cfg(test)]
#[path = "ui_map/download_pick_tests.rs"]
mod download_pick_tests;
