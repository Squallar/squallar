//! The [`Gui`]'s per-frame drive, split out of `ui.rs`: `ui()` and the per-frame
//! drivers it dispatches — the auto-polls, the initial zoom claim, the pending
//! appliers, the time dialog and the dismiss chain.
use super::*;
use crate::shell_api::UiPhaseStamps;

impl Gui {
    /// Create the UI using egui.
    ///
    /// **The shell calls [`Gui::ui_phased`], not this.** This spelling drops
    /// the frame's phase stamps, and with them the only decomposition of the
    /// `ui` frame segment that exists; it is here for the harnesses and tests
    /// that drive a frame without a ledger to file them in.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        self.ui_phased(ctx).0
    }

    /// [`Gui::ui`], with the five instants at which it crossed its own phase
    /// boundaries.
    ///
    /// The stamps are taken unconditionally — five clock reads on a call that
    /// lays out a whole frame — so there is no armed/unarmed spelling of this
    /// function to disagree about, and no frame where the instrument is off.
    /// What the caller does with them is the caller's business; see
    /// [`crate::shell_api::UiPhaseStamps`] for why they are instants.
    pub fn ui_phased(&mut self, ctx: &egui::Context) -> (Vec<GuiAction>, UiPhaseStamps) {
        let mut actions = Vec::new();

        if !self.settings_visible() {
            self.storm_motion_editing = false;
        }

        // Before anything draws: the site layer holds a *copy* of the table,
        // and this is where it hears that the table moved.
        self.republish_radar_sites_if_the_table_moved();

        self.check_auto_polls(&mut actions);

        // **What the gridded layers are holding, published where the
        // registry lives.** The figure belongs to the heap census
        // (`crate::heap_census`), which the allocation-error hook reads at a
        // refusal; it is published from here rather than from the shell's
        // telemetry tick because the registry is the UI layer's and reaching
        // across for it would grow the app layer's coupling for a counter.
        //
        // Cheap by construction: a fold over the registered handlers, each
        // answering from a byte field or a walk of the one to four grid
        // entries its budget allows. No grid contents are touched.
        crate::heap_census::set_overlay_grid_bytes(self.overlays.resident_source_bytes());
        // **The item half of the same question**, and two families rather
        // than one because they answer different things: what a layer has
        // INSTALLED, and what it has RETIRED and not yet handed to the
        // discard seam. Both are levels the source layer maintains at the
        // install and the park, so each of these is a load rather than a
        // walk of a six-figure flash list.
        crate::heap_census::set_overlay_item_bytes(
            squallar_overlays::render::overlay_state::installed_item_bytes(),
        );
        crate::heap_census::set_overlay_parked_bytes(
            squallar_overlays::render::overlay_state::parked_item_bytes(),
        );
        // The tile mesh store publishes its own level into the mesh ledger
        // every sweep; the census carries the same figure so one line names
        // every family. GPU bytes, and the census keeps them out of its page
        // total for that reason.
        crate::heap_census::set_tile_mesh_bytes(
            crate::tile_mesh::ledger::totals().mesh_resident_bytes,
        );

        // A download finishes whether or not its screen is open, and the
        // record it publishes is what makes the area exist to the rest of the
        // app - so the publish rides the frame, not the screen.
        self.settle_offline_download();
        let polled = web_time::Instant::now();

        self.layout = LayoutCtx::resolve(ctx, &mut self.modality, self.safe_area_insets);
        self.settle_pane_layout();
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
            self.probes.last_split_options.clear();
            self.probes.last_dropdowns.clear();
            self.probes.last_control_items.clear();
            self.probes.last_settings_rows.clear();
            self.probes.last_popup_triggered.clear();
            self.probes.last_popup_handled.clear();
            self.probes.last_attribution.clear();
            self.probes.last_stack = StackProbe::default();
            self.probes.last_inspector = InspectorProbe::default();
            self.probes.last_catalog = CatalogProbe::default();
            self.probes.last_pills.clear();
            self.probes.last_pill_popover = None;
            self.probes.control_render_passes = 0;
            self.probes.last_bottom_bar = BottomBarProbe::default();
            self.probes.last_sheet = SheetProbe::default();
            self.probes.last_error_toast = None;
            self.probes.last_diagnostics_rows.clear();
        }

        if self.layout.width != crate::ui_layout::WidthClass::Compact {
            self.menu_open = false;
        }

        self.expire_site_query(ctx);

        self.enforce_fade_invariants(ctx);

        let mut root_ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("squallar_root"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(self.layout.content_rect),
        );
        let laid_out = web_time::Instant::now();

        let shell = self.render_shell_phased(&mut root_ui);
        actions.extend(shell.out.actions);
        let shell_done = web_time::Instant::now();

        if let Some(action) = self.render_time_dialog(ctx) {
            actions.push(action);
        }
        let dialog_done = web_time::Instant::now();

        actions.extend(self.render_panes(&mut root_ui, &shell.out.excluded_rects));
        let panes = web_time::Instant::now();

        self.apply_pending_pane_view(&mut actions);
        self.apply_pending_section_line();
        self.apply_pending_region();
        self.apply_pending_section_edit();

        self.apply_fade_toggle(ctx);
        let applied = web_time::Instant::now();

        self.render_pane_pills(ctx, shell.out.map_rect, &mut actions);

        let phone_bar_top = (self.layout.width == crate::ui_layout::WidthClass::Compact)
            .then(|| self.render_bottom_bar(ctx, shell.out.map_rect));
        self.phone_bar_height =
            phone_bar_top.map_or(0.0, |top| (shell.out.map_rect.bottom() - top).max(0.0));

        self.render_timeline(ctx, shell.out.map_rect, phone_bar_top, &mut actions);

        if let Some(bar_top) = phone_bar_top {
            self.render_phone_error_toast(ctx, shell.out.map_rect, true);
            self.render_phone_sheet(ctx, shell.out.map_rect, bar_top, &mut actions);
        } else {
            self.render_phone_error_toast(ctx, shell.out.map_rect, self.ui_faded);
        }

        self.pump_download_area(ctx);
        self.render_download_area(ctx, shell.out.map_rect);

        self.render_overlay_popup(ctx);

        self.render_catalog(ctx, &mut actions);

        self.render_diagnostics_panel(ctx);

        self.apply_pending_pane_close(ctx, &mut actions);

        (
            actions,
            UiPhaseStamps {
                polled,
                laid_out,
                topbar: shell.topbar,
                statusbar: shell.statusbar,
                shell: shell_done,
                dialog: dialog_done,
                panes,
                applied,
            },
        )
    }

    /// **Last thing in the frame, and both halves of that matter.**
    ///
    /// After the last surface has drawn, so every `mem::take`n pane is back in
    /// the vector — removing a slot with one still out would restore it into
    /// the wrong one. And with the whole frame's action list in hand, which is
    /// what [`Gui::close_pane`] filters: an action queued earlier this frame
    /// for a pane at or above the closed one is addressed to a pane that no
    /// longer sits there.
    pub(super) fn apply_pending_pane_close(
        &mut self,
        ctx: &egui::Context,
        actions: &mut Vec<GuiAction>,
    ) {
        let Some(idx) = self.pending_pane_close.take() else {
            return;
        };
        if !self.close_pane(ctx, idx, actions) {
            log::warn!("pane {idx} cannot be closed; leaving the layout alone");
        }
    }

    /// Check timers and emit fetch actions for auto-polling radar scans, NWS
    /// alerts, and SPC discussions.
    pub(super) fn check_auto_polls(&mut self, actions: &mut Vec<GuiAction>) {
        // The radar layer answers the same question every other polling layer
        // answers — "may an automatic round start now?" — through the one gate
        // `auto_fetch_delay` is: the poll clock and the failure ladder, taken
        // together, in the layer's own answer rather than in a timer struct
        // beside it.
        let radar_due =
            crate::radar_layer::archive_poll_delay(&self.overlays).is_some_and(|d| d.is_zero());
        // **The session's first fetch is NOT gated on the poll being on.** It
        // never was: switching auto-poll off has always meant "stop checking
        // for newer volumes", never "show nothing at all this session". Folding
        // this arm into `auto_fetch_delay` — which answers `None` for a layer
        // that declares no interval — would quietly make it the second thing.
        let never_asked = !crate::radar_layer::archive_poll_started(&self.overlays);

        if never_asked && !self.fetching() {
            // The tracked round: the shell drains its answer, so the flag
            // comes back down on delivery or on error.
            self.set_radar_round_in_flight(true);
            actions.push(GuiAction::FetchRadarScan(self.active_pane_fetch_config()));
        } else if radar_due && self.is_any_pane_live() && !self.fetching() {
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

            // **The clock is stamped by the ask, and the round ends in the
            // same breath.** This check is answered only when there IS
            // something newer (`fetch_latest_if_newer`), so nothing would ever
            // bring an in-flight flag back down — and a clock that waited for
            // a delivery would leave the layer due again on the very next
            // frame. The rising edge is what stamps it; the falling edge is
            // the round ending, which for an unanswered check is the same
            // instant.
            self.set_radar_round_in_flight(true);
            self.set_radar_round_in_flight(false);
        }

        let poll_ids: Vec<squallar_source::id::LayerId> =
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

    /// The pane whose Volume Alpha editor a back press would close, or `None`
    /// when no visible pane has one open.
    ///
    /// The active pane first, because that is the one the user is working in;
    /// otherwise the lowest-numbered pane holding one. The fallback is not
    /// decoration — the editor is a floating window per pane, the fade already
    /// treats *every* pane's as an open surface (`ui_fade.rs`), and a window on
    /// screen that Escape cannot reach is the defect this arm exists to fix.
    ///
    /// One function, read by both halves of the paired truth below, so the two
    /// cannot drift on this arm at all.
    fn alpha_editor_pane(&self) -> Option<usize> {
        let open = |idx: usize| {
            self.panes
                .get(idx)
                .and_then(PaneState::volume)
                .is_some_and(|volume| volume.alpha_editor_open)
        };
        let visible = self.pane_layout.pane_count;
        if self.active_pane < visible && open(self.active_pane) {
            return Some(self.active_pane);
        }
        (0..visible).find(|&idx| open(idx))
    }

    /// Close the topmost thing the user has open, and say whether there was one.
    ///
    /// Paired with [`back_would_dismiss`](Self::back_would_dismiss), which
    /// answers the same question without doing it. The two walk the same
    /// priority chain and MUST agree on every UI state: Android publishes the
    /// predicate's answer to the platform *before* a back press arrives (the
    /// dispatcher takes no answer afterwards), so a pair that has drifted is a
    /// claim that lies — either a press swallowed with nothing to close, or the
    /// app finished out from under an open sheet. The paired-truth test in the
    /// input harness walks a matrix of UI states asserting the two agree;
    /// change one of these and you change both.
    ///
    /// Android is not opted into that dispatcher today, so the published claim
    /// is read by nothing (`BackHandler.kt` carries the measured reason). The
    /// pairing obligation stands regardless: it is what makes the claim safe to
    /// switch on, and a drift introduced while nobody is looking is exactly the
    /// bug that would surface on the day it is.
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
                    sheet::SheetPage::Inspector => self.insp_open = false,
                    sheet::SheetPage::Layers => self.drawer_open = false,
                }
                return true;
            }
            // Below the sheet, not above it: the sheet, its scrim and its
            // hosted bodies are `Order::Foreground` and the editor is a plain
            // `egui::Window`, so while a page is up the editor is *under* it
            // and cannot be the top layer.
            if let Some(idx) = self.alpha_editor_pane() {
                self.close_alpha_editor(idx);
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
            // Beside the feature popup: both are transient surfaces the user
            // summoned onto the map, and both yield to the modal above them.
            if let Some(idx) = self.alpha_editor_pane() {
                self.close_alpha_editor(idx);
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
        if self.download_pick_armed {
            self.set_download_pick_armed(false);
            return true;
        }
        // Under the arm, because backing out of an armed drag is what the user
        // means first; a committed box is the next layer down.
        if self.download_pick.is_some() {
            self.clear_download_pick();
            return true;
        }
        false
    }

    /// Shut one pane's Volume Alpha editor — the act half of the arm
    /// [`alpha_editor_pane`](Self::alpha_editor_pane) chooses.
    fn close_alpha_editor(&mut self, idx: usize) {
        if let Some(volume) = self.panes[idx].volume_mut() {
            volume.alpha_editor_open = false;
        }
    }

    /// Whether [`dismiss_top_layer`](Self::dismiss_top_layer) would close
    /// something — the same chain, read-only.
    ///
    /// A pure predicate on purpose: it is called every frame on Android to keep
    /// the predictive-back claim truthful, and a query with a side effect there
    /// would close layers nobody pressed back on. See the pairing note on
    /// `dismiss_top_layer`.
    pub fn back_would_dismiss(&self) -> bool {
        if self.section_edit_drag.is_some() {
            return true;
        }
        if self.ui_faded {
            return true;
        }
        if self.menu_popup_open {
            return true;
        }
        if self.layout.width == crate::ui_layout::WidthClass::Compact {
            if self.top_sheet_page().is_some() {
                return true;
            }
            if self.alpha_editor_pane().is_some() {
                return true;
            }
        } else {
            if self.catalog_open {
                return true;
            }
            if !self.overlays.selected_overlays.is_empty() {
                return true;
            }
            if self.alpha_editor_pane().is_some() {
                return true;
            }
            if self.time_dialog.show {
                return true;
            }
            if self.menu_open {
                return true;
            }
            if self.insp_open {
                return true;
            }
            if self.drawer_open {
                return true;
            }
        }
        if self.section_draw_armed {
            return true;
        }
        if self.region_pick_armed {
            return true;
        }
        if self.download_pick_armed || self.download_pick.is_some() {
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

    /// **Re-ask the pane grid for the room this frame actually has**, and take
    /// up any divider positions a config file brought in.
    ///
    /// Runs immediately after [`LayoutCtx::resolve`], because this is the
    /// first moment in a session at which the real width class is known: a
    /// config load happens before any frame, so the layout it built was built
    /// against the default width. The restored ratios are a one-shot for
    /// exactly that reason — they are validated against whatever grid the real
    /// width produces, and refused rather than stretched if it is a different
    /// one.
    pub(super) fn settle_pane_layout(&mut self) {
        self.pane_layout
            .reflow(self.layout.width, self.split_orientation);
        if let Some((rows, cols)) = self.restored_ratios.take()
            && !self.pane_layout.adopt_ratios(&rows, &cols)
        {
            log::debug!(
                "saved pane dividers do not describe this window's {:?} grid; \
                 using the defaults for it",
                self.pane_layout.grid(),
            );
        }
    }

    /// Apply the view change the frame asked for, if any.
    pub(super) fn apply_pending_pane_view(&mut self, actions: &mut Vec<GuiAction>) {
        let Some((pane_idx, view)) = self.pending_pane_view.take() else {
            return;
        };
        match self.panes.get_mut(pane_idx) {
            Some(pane) => {
                if pane.render_view() == squallar_radar::types::RenderView::Volume
                    && view != squallar_radar::types::RenderView::Volume
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
