//! The [`Gui`]'s per-frame drive, split out of `ui.rs`: `ui()` and the per-frame
//! drivers it dispatches — the auto-polls, the initial zoom claim, the pending
//! appliers, the time dialog and the dismiss chain.
use super::*;

impl Gui {
    /// Create the UI using egui.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();

        if !self.settings_visible() {
            self.storm_motion_editing = false;
        }

        self.check_auto_polls(&mut actions);

        self.layout = LayoutCtx::resolve(ctx, &mut self.modality, self.safe_area_insets);
        #[cfg(test)]
        {
            self.probes.widget_id_probes.clear();
            self.probes.last_menu_leaves.clear();
            self.probes.last_pane_pointers.clear();
            self.probes.last_pane_content.clear();
            self.probes.last_volume_arms.clear();
            self.probes.last_pane_borders.clear();
            self.probes.last_section_tracks.clear();
            self.probes.last_region_boxes.clear();
            self.probes.last_alpha_buttons.clear();
            self.probes.last_paint_order.clear();
            self.probes.last_pane_options.clear();
            self.probes.last_dropdowns.clear();
            self.probes.last_control_items.clear();
            self.probes.last_settings_rows.clear();
            self.probes.last_popup_triggered.clear();
            self.probes.last_popup_handled.clear();
            self.probes.last_stack = StackProbe::default();
            self.probes.last_inspector = InspectorProbe::default();
            self.probes.last_catalog = CatalogProbe::default();
            self.probes.last_pills.clear();
            self.probes.last_pill_popover = None;
            self.probes.control_render_passes = 0;
            self.probes.last_bottom_bar = BottomBarProbe::default();
            self.probes.last_sheet = SheetProbe::default();
            self.probes.last_error_toast = None;
        }

        if self.layout.width != crate::ui_layout::WidthClass::Compact {
            self.menu_open = false;
        }

        self.enforce_fade_invariants(ctx);

        let mut root_ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("rustdar_root"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(self.layout.content_rect),
        );

        let shell = self.render_shell(&mut root_ui);
        actions.extend(shell.actions);

        if let Some(action) = self.render_time_dialog(ctx) {
            actions.push(action);
        }

        actions.extend(self.render_panes(&mut root_ui, &shell.excluded_rects));

        self.apply_pending_pane_view(&mut actions);
        self.apply_pending_section_line();
        self.apply_pending_region();
        self.apply_pending_section_edit();

        self.apply_fade_toggle(ctx);

        self.render_pane_pills(ctx, shell.map_rect, &mut actions);

        let phone_bar_top = (self.layout.width == crate::ui_layout::WidthClass::Compact)
            .then(|| self.render_bottom_bar(ctx, shell.map_rect));
        self.phone_bar_height =
            phone_bar_top.map_or(0.0, |top| (shell.map_rect.bottom() - top).max(0.0));

        self.render_timeline(ctx, shell.map_rect, phone_bar_top, &mut actions);

        if let Some(bar_top) = phone_bar_top {
            self.render_phone_error_toast(ctx, shell.map_rect, true);
            self.render_phone_sheet(ctx, shell.map_rect, bar_top, &mut actions);
        } else {
            self.render_phone_error_toast(ctx, shell.map_rect, self.ui_faded);
        }

        self.render_overlay_popup(ctx);

        self.render_catalog(ctx, &mut actions);

        actions
    }

    /// Check timers and emit fetch actions for auto-polling radar scans, NWS
    /// alerts, and SPC discussions.
    pub(super) fn check_auto_polls(&mut self, actions: &mut Vec<GuiAction>) {
        if !self.auto_poll.initial_fetch_done && !self.radar.fetching {
            self.radar.fetching = true;
            self.auto_poll.initial_fetch_done = true;
            self.auto_poll.record_fetch();
            actions.push(GuiAction::FetchRadarScan(self.active_pane_fetch_config()));
        }

        if self.is_any_pane_live() && self.auto_poll.should_poll() && !self.radar.fetching {
            let now = chrono::Local::now().naive_local();
            let current_scan_time = now
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now);

            let mut seen_sites: Vec<&str> = Vec::with_capacity(self.pane_layout.pane_count);
            for pane in self.panes.iter().take(self.pane_layout.pane_count) {
                if pane.viewing_live && !seen_sites.contains(&pane.site()) {
                    seen_sites.push(pane.site());
                    let config = RadarConfig {
                        site: pane.site().to_string(),
                        timestamp: current_scan_time,
                    };
                    actions.push(GuiAction::CheckForNewScans(config));
                }
            }

            self.auto_poll.record_fetch();
        }

        let poll_ids: Vec<rustdar_source::id::LayerId> =
            self.overlays.handlers().map(|h| h.id()).collect();
        for kind in poll_ids {
            if self
                .overlays
                .auto_fetch_delay(&kind)
                .is_some_and(|d| d.is_zero())
                && let Some(pane_idx) = self.first_pane_with_overlay_enabled(&kind)
            {
                actions.push(GuiAction::FetchOverlay { kind, pane_idx });
            }
        }
    }

    /// Zoom to the radar on the first scan of a session and never again, so a later
    /// load does not throw away the user's navigation.
    pub(super) fn claim_initial_zoom(&mut self) {
        if !self.initial_zoom_set {
            for pane in &mut self.panes {
                let _ = pane.map_memory.set_zoom(DEFAULT_INITIAL_ZOOM);
            }
            self.initial_zoom_set = true;
        }
    }

    /// Close the topmost thing the user has open, and say whether there was one.
    pub fn dismiss_top_layer(&mut self) -> bool {
        if self.section_edit_drag.is_some() {
            self.section_edit_drag = None;
            return true;
        }
        if self.ui_faded {
            self.ui_faded = false;
            return true;
        }
        if self.menu_popup_open {
            self.menu_popup_open = false;
            self.menu_popup_close_requested = true;
            return true;
        }
        if self.layout.width == crate::ui_layout::WidthClass::Compact {
            if let Some(page) = self.top_sheet_page() {
                match page {
                    sheet::SheetPage::Feature => {
                        self.overlays.selected_overlays.clear();
                        self.overlays.selected_overlay_page = 0;
                    }
                    sheet::SheetPage::Time => self.time_dialog.show = false,
                    sheet::SheetPage::Catalog => self.catalog_open = false,
                    sheet::SheetPage::Menu => self.menu_open = false,
                    sheet::SheetPage::Inspector => {
                        self.insp_open = false;
                        self.inspector_sel = InspectorSelection::AppSettings;
                    }
                    sheet::SheetPage::Layers => self.drawer_open = false,
                }
                return true;
            }
        } else {
            if self.catalog_open {
                self.catalog_open = false;
                return true;
            }
            if !self.overlays.selected_overlays.is_empty() {
                self.overlays.selected_overlays.clear();
                self.overlays.selected_overlay_page = 0;
                return true;
            }
            if self.time_dialog.show {
                self.time_dialog.show = false;
                return true;
            }
            if self.menu_open {
                self.menu_open = false;
                return true;
            }
            if self.insp_open {
                self.insp_open = false;
                self.inspector_sel = InspectorSelection::AppSettings;
                return true;
            }
            if self.drawer_open {
                self.drawer_open = false;
                return true;
            }
        }
        if self.section_draw_armed {
            self.set_section_draw_armed(false);
            return true;
        }
        if self.region_pick_armed {
            self.set_region_pick_armed(false);
            return true;
        }
        false
    }

    pub(super) fn render_time_dialog(&mut self, ctx: &Context) -> Option<GuiAction> {
        if !self.time_dialog.show || self.layout.width == crate::ui_layout::WidthClass::Compact {
            return None;
        }

        let mut action = None;
        egui::Window::new("Set Time")
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(self.layout.dialog_center())
            .show(ctx, |ui| {
                action = self.render_time_dialog_body(ui);
            });
        action
    }

    /// Apply the view change the frame asked for, if any.
    pub(super) fn apply_pending_pane_view(&mut self, actions: &mut Vec<GuiAction>) {
        let Some((pane_idx, view)) = self.pending_pane_view.take() else {
            return;
        };
        match self.panes.get_mut(pane_idx) {
            Some(pane) => {
                if pane.render_view() == rustdar_radar::types::RenderView::Volume
                    && view != rustdar_radar::types::RenderView::Volume
                {
                    actions.push(GuiAction::ReleaseVolume { pane_idx });
                }
                pane.set_view(view);
            }
            None => log::warn!("pane {pane_idx} is gone; not switching it to {view:?}"),
        }
    }

    /// Give the line this frame drew to a pane, converting or creating one if need
    /// be.
    pub(super) fn apply_pending_section_line(&mut self) {
        let Some((source, line)) = self.pending_section_line.take() else {
            return;
        };

        let (source_product, source_site, source_scan) = match self.panes.get(source) {
            Some(pane) => (
                pane.selected_product(),
                pane.site().to_string(),
                pane.scan_info.clone(),
            ),
            None => {
                log::warn!("pane {source} drew a section line and is already gone");
                return;
            }
        };

        let target = self
            .section_pane_sourced_from(source)
            .or_else(|| self.grown_pane())
            .or_else(|| self.lowest_section_pane())
            .or_else(|| self.highest_pane_other_than(source))
            .unwrap_or(source);

        let Some(pane) = self.panes.get_mut(target) else {
            log::warn!("no pane could hold the section drawn on pane {source}");
            return;
        };
        pane.set_kind(crate::pane::PaneKind::CrossSection);
        pane.set_selected_product(source_product);
        pane.set_site(source_site);
        pane.scan_info = source_scan;
        if let Some(section) = pane.cross_section_mut() {
            section.line = Some(line);
            section.source_pane = Some(source);
            section.section = None;
            section.texture = None;
            section.unavailable = None;
            section.rendered_for = None;
        }
        self.active_pane = target;
    }

    /// Give the region this frame dragged to a pane, converting or creating one if
    /// need be.
    pub(super) fn apply_pending_region(&mut self) {
        let Some((source, region)) = self.pending_region.take() else {
            return;
        };

        let (source_product, source_site, source_scan) = match self.panes.get(source) {
            Some(pane) => (
                pane.selected_product(),
                pane.site().to_string(),
                pane.scan_info.clone(),
            ),
            None => {
                log::warn!("pane {source} picked a 3D region and is already gone");
                return;
            }
        };

        let target = self
            .volume_pane_sourced_from(source)
            .or_else(|| self.grown_pane())
            .or_else(|| self.lowest_volume_pane())
            .or_else(|| self.highest_pane_other_than(source))
            .unwrap_or(source);

        let Some(pane) = self.panes.get_mut(target) else {
            log::warn!("no pane could hold the region picked on pane {source}");
            return;
        };
        pane.set_kind(crate::pane::PaneKind::Map);
        pane.set_map_render(crate::pane::MapRender::Volume);
        pane.set_selected_product(source_product);
        pane.set_site(source_site);
        pane.scan_info = source_scan;
        if let Some(volume) = pane.volume_mut() {
            volume.region = Some(region);
            volume.source_pane = Some(source);
            volume.rendered_for = None;
        }
        self.active_pane = target;
    }

    /// Write a dropped handle's line onto the section pane it belongs to.
    pub(super) fn apply_pending_section_edit(&mut self) {
        let Some((pane_idx, line)) = self.pending_section_edit.take() else {
            return;
        };
        let Some(section) = self
            .panes
            .get_mut(pane_idx)
            .and_then(|p| p.cross_section_mut())
        else {
            log::warn!("pane {pane_idx} is no longer a section pane; dropping the edited line");
            return;
        };
        section.line = Some(line);
    }
}
