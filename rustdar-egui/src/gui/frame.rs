//! The [`Gui`]'s per-frame drive, split out of `ui.rs` at WO-E1: `ui()`
//! and the per-frame drivers it dispatches — the auto-polls, the initial
//! zoom claim, the pending appliers, the time dialog and the dismiss
//! chain. Bodies are verbatim moves.
use super::*;

impl Gui {
    /// Create the UI using egui.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();

        // The second writer of `storm_motion_editing`, and the reason a latch
        // here cannot stick: the rows that clear it only run while the
        // settings body is drawn, so a panel closed mid-drag would leave the
        // commit deferred for ever. Cleared *before* the body draws, so a body
        // that does draw this frame still gets the last word.
        if !self.settings_visible() {
            self.storm_motion_editing = false;
        }

        self.check_auto_polls(&mut actions);

        // Resolve the frame's layout exactly once, before anything draws. Every
        // responsive decision below reads `self.layout`; nothing recomputes a
        // width or a modality of its own.
        self.layout = LayoutCtx::resolve(ctx, &mut self.modality, self.safe_area_insets);
        #[cfg(test)]
        {
            self.probes.widget_id_probes.clear();
            self.probes.last_menu_leaves.clear();
            self.probes.last_pane_pointers.clear();
            // Cleared beside the pointer probes, and for the same reason: both
            // are per-pane records of one frame's pane loop, so a leftover entry
            // would report an arm that did not run this frame.
            self.probes.last_pane_content.clear();
            // Same reason as the line above: a per-frame record of the pane
            // loop, so a leftover entry would report a 3D arm that did not run.
            self.probes.last_volume_arms.clear();
            // Per-frame paint records of the pane loop, on the same terms:
            // the borders, the section tracks, the region boxes and the Volume
            // Alpha corner buttons are all re-painted (or legitimately absent)
            // each frame.
            self.probes.last_pane_borders.clear();
            self.probes.last_section_tracks.clear();
            self.probes.last_region_boxes.clear();
            self.probes.last_alpha_buttons.clear();
            self.probes.last_paint_order.clear();
            // Cleared like the rest: the picker redraws from the top bar every
            // frame, and appending over a stale list would report every button
            // twice.
            self.probes.last_pane_options.clear();
            // The handler dropdowns only exist while the layers panel is on
            // screen, so a stale entry would report widgets that are not there.
            self.probes.last_dropdowns.clear();
            // And its generalisation, for the same reason.
            self.probes.last_control_items.clear();
            // Likewise: the settings rows only exist while the window is open.
            self.probes.last_settings_rows.clear();
            // Per-frame records of the popup's action handling; a leftover
            // entry would report a button press that did not happen this frame.
            self.probes.last_popup_triggered.clear();
            self.probes.last_popup_handled.clear();
            // Per-frame records of the stack and inspector; a stale probe
            // would report a panel that is no longer on screen. Reset rather
            // than cleared, like the timeline's — `open: false` is a report,
            // not an absence.
            self.probes.last_stack = StackProbe::default();
            self.probes.last_inspector = InspectorProbe::default();
            self.probes.last_catalog = CatalogProbe::default();
            // Per-frame records of the pill rows and their popover; a stale
            // entry would report a row for a pane no longer on screen.
            self.probes.last_pills.clear();
            self.probes.last_pill_popover = None;
            // The double-render guard's counter; see the field.
            self.probes.control_render_passes = 0;
            // Per-frame records of the phone shell's bottom cluster; reset
            // like the stack's — `page: None` is a report, not an absence.
            self.probes.last_bottom_bar = BottomBarProbe::default();
            self.probes.last_sheet = SheetProbe::default();
            // And of its error toast — `None` is "no toast drew".
            self.probes.last_error_toast = None;
        }

        // The sheet's Menu page is Compact chrome; on the wider widths the ☰
        // Popup owns the menu with its own egui-managed state. Clearing the
        // flag whenever the width says so is what keeps a resize with the
        // page open from stranding a flag no surface renders — which
        // `dismiss_top_layer` would then consume a back press against,
        // invisibly.
        if self.layout.width != crate::ui_layout::WidthClass::Compact {
            self.menu_open = false;
        }

        // The fade's frame-top pass: while faded nothing may be open — a
        // surface found open means the user acted through a route the
        // pointer guards cannot see, and the repair is to unfade — and the
        // frame's shared chrome opacity resolves here, once. See `ui_fade.rs`.
        self.enforce_fade_invariants(ctx);

        // Create a root Ui to host the panels. Since egui 0.35 the Context-taking
        // `Panel::show` is gone and panels are Ui-scoped only, so this root Ui is
        // the only way in.
        //
        // The root rect is the *content* rect, so every `Panel` nested inside it
        // is inset from the system bars and the notch for free. That is what
        // replaced the hand-rolled `add_space(top_inset)` calls the mobile UI
        // used to carry at each panel's top edge.
        let mut root_ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("rustdar_root"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(self.layout.content_rect),
        );

        // The shell first: the docked top bar claims its space, the floating
        // surfaces — status bar, layer stack, inspector — position themselves
        // in what it left, and that remainder — `shell.map_rect` — is the
        // map's. See `ui_shell.rs`.
        let shell = self.render_shell(&mut root_ui);
        actions.extend(shell.actions);

        if let Some(action) = self.render_time_dialog(ctx) {
            actions.push(action);
        }

        actions.extend(self.render_panes(&mut root_ui, &shell.excluded_rects));

        // After the pane loop, and therefore after every `mem::take` window in
        // the frame has closed. See the `pending_pane_view` field for why
        // converting a pane cannot be a direct write from the dispatcher that
        // asked for it.
        self.apply_pending_pane_view(&mut actions);
        // Same window, and one thing more: this can grow `pane_count`, which
        // moves `pane_rect` for every pane. Inside the loop that would leave the
        // panes drawn after it hit-tested against rects they are no longer in.
        self.apply_pending_section_line();
        // The other modal drag's applier, on the same footing: it grows the
        // layout through the same `grown_pane`, so it must be outside the loop
        // for the same reason. Never both in one frame — the two modes are held
        // mutually exclusive by their setters and share one detector — so the
        // order between these two is unobservable, and they are adjacent so
        // that the growth argument is read once for both.
        self.apply_pending_region();
        // After the modal-draw applier, so if both somehow fired in one frame
        // the dropped edit — the later write — would win. The case is
        // unreachable: an armed draw makes the handles inert, and beginning a
        // handle drag requires no mode to be armed, so the two cannot both
        // have a gesture to commit. This one can never grow the layout, so it
        // takes no part in the ordering argument below.
        self.apply_pending_section_edit();
        // After the kind conversion, so a region that lands on a pane the same
        // frame converted it finds a 3D pane rather than the map it used to be.
        //
        // # Two appliers, and why their order is not a design decision
        //
        // Both of these can grow the layout, and running two growths in one frame
        // would be a case neither was written for: the second one's target rule
        // would run against a layout the first had already changed, and in a full
        // layout each rule's last resort is *the same pane* — so the second would
        // convert the pane the first had just filled, and the user would see one
        // of two completed gestures produce nothing.
        //
        // It cannot happen, and the reason is upstream of here: the two modes are
        // mutually exclusive (see [`Self::set_section_draw_armed`]), only an armed
        // mode can record a pending, and each pending is recorded and consumed
        // inside a single frame. So at most one of these two lines does anything
        // on any frame. The exclusivity that argument rests on is pinned by
        // `arming_the_section_draw_clears_a_handle_drag_in_flight`, which is
        // where the invariant lives — arming one mode clears the other's
        // gesture, so two pendings cannot both be recorded. Be aware that the
        // conclusion *about this call order* is not itself under test: the test
        // that used to be cited here drove both toggles, and went with the
        // render-mode split without a successor.

        // The fade toggle, after the appliers like every other loop-recorded
        // intent: it needs the pane loop's final consumption verdict, and the
        // surfaces drawn below read the state it settles. See `ui_fade.rs`.
        self.apply_fade_toggle(ctx);

        // The pill rows, after the pane loop and the appliers: outside every
        // `mem::take` window, so a popover pick writes real panes, and after
        // the kind appliers so a row states the kind its pane ended the
        // frame as. See `ui_pills.rs`.
        self.render_pane_pills(ctx, shell.map_rect, &mut actions);

        // The phone shell's bottom bar, before the timeline so the inline
        // transport can position itself above the bar it just drew. Only on
        // Compact — the wider widths keep the floating bottom-centred
        // transport and no bar.
        let phone_bar_top = (self.layout.width == crate::ui_layout::WidthClass::Compact)
            .then(|| self.render_bottom_bar(ctx, shell.map_rect));
        // Measured here and read by *next* frame's pane loop, which is the only
        // order available: the loop paints the colour-scale legend into the
        // map's own layer, and this bar is drawn opaque over that layer
        // afterwards. Written on every frame including the ones that draw no
        // bar, so a resize out of Compact — or the chrome fading out, which
        // returns the map's own bottom edge and therefore a height of zero —
        // hands the edge straight back to the legend instead of stranding an
        // inset nothing is covering. See the field.
        self.phone_bar_height =
            phone_bar_top.map_or(0.0, |top| (shell.map_rect.bottom() - top).max(0.0));

        // The timeline transport, after the pane loop and the appliers: every
        // `mem::take` window in the frame has closed, so it reads and writes
        // `self.panes[self.active_pane]` directly — the real pane, not a
        // placeholder. See `ui_timeline.rs`.
        self.render_timeline(ctx, shell.map_rect, phone_bar_top, &mut actions);

        // The sheet, above everything the phone shell floats: the Layers and
        // Inspector pages open their take window in here, so it must run
        // after the pane loop and the appliers on the same terms as the
        // shell's own pass — and the Catalog page's apply paths take panes
        // themselves, so no window may already be open. See `ui_sheet.rs`.
        if let Some(bar_top) = phone_bar_top {
            self.render_phone_error_toast(ctx, shell.map_rect, true);
            self.render_phone_sheet(ctx, shell.map_rect, bar_top, &mut actions);
        } else {
            // The error surface outranks the fade (the deliberate §1.8
            // refinement in `ui_fade.rs`): the wide widths normally carry the
            // error inside the status bar, which is faded — so while faded
            // the phone's own toast presentation carries it instead. Called
            // unconditionally so its rise and fall animate through the
            // fade/unfade handoff; unfaded it presents nothing.
            self.render_phone_error_toast(ctx, shell.map_rect, self.ui_faded);
        }

        // Floating windows last, so they layer above the chrome and the map.
        // (Settings are no longer a window of their own: they are the
        // inspector's App › Settings body, drawn by the shell above.) On
        // Compact both return without drawing — the sheet pages above are
        // their presentation there (plan §1.9).
        self.render_overlay_popup(ctx);

        // The Add-layer catalog, after the feature popup so it stacks above
        // one left open — matching `dismiss_top_layer`, which closes the
        // catalog first. Also after the appliers on its own account: applying
        // a preset writes pane kinds directly and can grow the pane count,
        // both of which are only safe once every take window has closed.
        self.render_catalog(ctx, &mut actions);

        // Ensure the handler state reflects the active pane's config at frame
        // end, so any deferred actions (FetchOverlay, etc.) processed after the
        // frame use the correct per-pane state.
        let active = &self.panes[self.active_pane];
        if !active.overlay_configs.is_empty() {
            let configs = active.overlay_configs.clone();
            self.overlays.load_pane_configs(&configs);
        }

        actions
    }

    /// The read-side context handlers are asked for their controls with, aimed
    /// at the active pane.
    ///
    /// One constructor for the renderer and the test accessors alike, so the
    /// model a test asks a handler for is built exactly as the renderer builds
    /// it — the two diverging is how an inventory drifts from the glass.
    pub(super) fn active_pane_control_context(&self) -> PaneControlContext<'_> {
        PaneControlContext {
            pane_idx: self.active_pane,
            pane_state: None,
        }
    }

    /// Check timers and emit fetch actions for auto-polling radar scans,
    /// NWS alerts, and SPC discussions.
    pub(super) fn check_auto_polls(&mut self, actions: &mut Vec<GuiAction>) {
        // Auto-fetch on first load
        if !self.auto_poll.initial_fetch_done && !self.radar.fetching {
            self.radar.fetching = true;
            self.auto_poll.initial_fetch_done = true;
            self.auto_poll.record_fetch();
            actions.push(GuiAction::FetchRadarScan(self.active_pane_fetch_config()));
        }

        // Poll for new scans at the current poll interval (only when any pane is viewing live)
        if self.is_any_pane_live() && self.auto_poll.should_poll() && !self.radar.fetching {
            // Check for new files without downloading — emit one check per unique live site
            let now = chrono::Local::now().naive_local();
            let current_scan_time = now
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now);

            let mut seen_sites: Vec<&str> = Vec::with_capacity(self.pane_layout.pane_count);
            for pane in self.panes.iter().take(self.pane_layout.pane_count) {
                if pane.viewing_live && !seen_sites.contains(&pane.site.as_str()) {
                    seen_sites.push(&pane.site);
                    let config = RadarConfig {
                        site: pane.site.clone(),
                        timestamp: current_scan_time,
                    };
                    actions.push(GuiAction::CheckForNewScans(config));
                }
            }

            // Reset timer to avoid spamming checks
            self.auto_poll.record_fetch();
        }

        // Auto-refresh overlay data when a layer is on screen and its own gate
        // says a fetch may start. The gate is `OverlayHandler::auto_fetch_delay`
        // and nothing here second-guesses it: it folds the poll clock, the fetch
        // in flight, and the retry ladder into one duration, and this is the
        // only place that reads it as "now".
        //
        // It used to be spelled out here as `fetch_time.is_none_or(elapsed >=
        // interval)`. `fetch_time` is stamped only on success, so a failing
        // layer answered "due" on every frame — 3089 SPC MD requests in 105 s
        // in the browser. See `rustdar_overlays::fetch_policy`.
        for &kind in OverlayKind::all() {
            if self
                .overlays
                .auto_fetch_delay(kind)
                .is_some_and(|d| d.is_zero())
                && let Some(pane_idx) = self.first_pane_with_overlay_enabled(kind)
            {
                actions.push(GuiAction::FetchOverlay { kind, pane_idx });
            }
        }
    }

    /// Zoom to the radar on the first scan of a session and never again, so a
    /// later load does not throw away the user's navigation.
    ///
    /// Factored out of [`Gui::apply`](Self::apply)'s `ScanInfoForSite` arm
    /// because the `ChunkScanInfo` arm shares this one behaviour and none of
    /// the others — and with chunks feeding live mode, the first data of a
    /// session can arrive through either.
    ///
    /// # `load_ui_config` is the other writer of the latch
    ///
    /// This predates viewport persistence, when "the first scan of a session"
    /// really did mean "the first data a default `Gui` ever saw". It no longer
    /// does: a restored config sets every pane's zoom *before* any scan arrives,
    /// so `Gui::load_ui_config` claims the latch itself when it restored one —
    /// otherwise the first scan seconds later overwrites the user's zoom and the
    /// next autosave persists the overwrite.
    ///
    /// What is left is the two cases where nothing was restored and a pane is
    /// still sitting at the roughly continental `DEFAULT_PANE_ZOOM`: a first run
    /// with no config, and a config written before the viewport was persisted.
    /// Those are the reason this is still here.
    pub(super) fn claim_initial_zoom(&mut self) {
        if !self.initial_zoom_set {
            for pane in &mut self.panes {
                let _ = pane.map_memory.set_zoom(DEFAULT_INITIAL_ZOOM);
            }
            self.initial_zoom_set = true;
        }
    }

    /// Close the topmost thing the user has open, and say whether there was
    /// one.
    ///
    /// What Escape and Android's back both mean: back out of the thing I am
    /// in. Only when this returns `false` is the press a request to leave the
    /// app — which is why a stray press with something open used to cost a
    /// whole relaunch on a phone, back going straight to minimise.
    ///
    /// Ordered topmost first — whatever is painted over everything else is
    /// what a press is aimed at — and exactly one layer closes per press.
    /// The full order (contract 65): the in-flight handle drag → the fade →
    /// the ☰ dropdown → catalog → feature → time → menu → inspector → the
    /// stack's drawer form → the armed drags.
    ///
    /// Not derived from the order `ui` calls them in, which is shell (stack
    /// and inspector included), then time dialog, then popup. The popup is
    /// `Order::Foreground`, so egui stacks it above the `Order::Middle`
    /// panels whatever the call order, and the time dialog sits between. This
    /// order is asserted rather than computed; see
    /// `a_back_press_closes_one_open_layer_at_a_time`.
    ///
    /// Below the Compact breakpoint the asserted chain gives way to the
    /// sheet's projection: every page flag presents as one sheet there, so
    /// the press pops exactly the page [`Gui::top_sheet_page`] reports on
    /// top, and only the non-page layers (the in-flight drag, the armed
    /// modes) keep their fixed places around it. See
    /// `a_back_press_walks_the_phone_sheet_pages_top_down`.
    ///
    /// Deliberately not reachable from `request_exit`: the window's close
    /// button and the menu's Exit item are unambiguous, and dismissing a dialog
    /// instead of honouring them would strand the user — the Exit item lives
    /// *inside* the ☰ dropdown this function closes first.
    pub fn dismiss_top_layer(&mut self) -> bool {
        // First, above everything painted: a handle drag in flight owns the
        // pointer right now, which makes it the most immediate thing a "back
        // out" gesture can be aimed at. Cancelling restores the line the drag
        // started from — the preview was never written anywhere.
        if self.section_edit_drag.is_some() {
            self.section_edit_drag = None;
            return true;
        }
        // The fade, next: while faded the invariant holds nothing else open
        // (`enforce_fade_invariants`), so a back press can only mean "restore
        // my UI" — the same reading every top-bar interaction gives it
        // (§3.6's unfade-before-acting), and consistent with the chain's
        // rule: the press is aimed at the most immediate state the user is
        // in. Only the handle drag outranks it, because a drag in flight can
        // exist *while* faded — the map stays interactive — and it owns the
        // pointer right now. The armed modes cannot coexist with the fade
        // (arming routes unfade first; an armed click never fades), so their
        // place below is never contested.
        if self.ui_faded {
            self.ui_faded = false;
            return true;
        }
        // The ☰ dropdown, above every dialog: it is `Order::Foreground` and
        // opened last, and it is the head of the plan's Esc chain (§3.4).
        //
        // egui's `Popup` closes itself on the Escape *it* sees, but that
        // covers one of this function's three routes. The frontend resolves
        // the same Escape press here independently, and without this layer
        // that resolution fell through to whatever sat beneath the popup —
        // two layers on one press. Android's back is worse: a logical event
        // that never enters egui's queue at all, so the popup would have
        // stayed open over a drawer this function closed behind it. Consuming
        // the press here and letting `render_top_bar_run` honour the request
        // makes all three routes close the popup, and the popup only — the
        // Escape egui also saw closes it twice over, idempotently.
        if self.menu_popup_open {
            self.menu_popup_open = false;
            self.menu_popup_close_requested = true;
            return true;
        }
        // On Compact every page flag presents as the sheet, so dismissal
        // reads the same projection the renderer does: pop exactly the page
        // `top_sheet_page` says is visibly on top. The fixed chain below
        // cannot serve here, because flags can stack out of its order —
        // flags set on a wider width and carried through a resize (the bar's
        // own pages are exclusive since contract 64's revision, but a
        // resize is not the bar) — and the chain would then pop a layer the
        // projection never shows, consuming a press invisibly. One rule
        // either side of the breakpoint: dismissal pops what is painted on
        // top.
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
                        // The same reset the wide arm below makes: a
                        // dismissal is a "back out", and what was backed out
                        // of should not lie in wait for the next open.
                        self.insp_open = false;
                        self.inspector_sel = InspectorSelection::AppSettings;
                    }
                    sheet::SheetPage::Layers => self.drawer_open = false,
                }
                return true;
            }
        } else {
            // The catalog, above the feature and time dialogs (plan §3.4 as
            // amended): it is the modal opened last when it is open at all,
            // and the frame draws it above a feature popup left open for the
            // same reason.
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
            // The phone sheet's Menu page has no presentation up here, and
            // `Gui::ui` clears its flag on every wider frame — this arm only
            // covers a press landing between a resize and the frame that
            // normalises it.
            if self.menu_open {
                self.menu_open = false;
                return true;
            }
            // The inspector, below the dialogs: it is a side panel, not a
            // modal, so anything modal over the map outranks it. Closing
            // resets the selection to App › Settings (plan §3.4) — a
            // dismissal is a "back out", and what the user backed out of
            // should not lie in wait for the next open.
            if self.insp_open {
                self.insp_open = false;
                self.inspector_sel = InspectorSelection::AppSettings;
                return true;
            }
            // The stack, in its drawer form only — the presentation that
            // covers the map. The Expanded sidebar is deliberately not a
            // dismissal target: it is open by default, and an Escape with
            // nothing else open closing it would put the sidebar between
            // every desktop user and "Escape means leave".
            if self.drawer_open {
                self.drawer_open = false;
                return true;
            }
        }
        // Last, below every painted layer, because an armed drag is a *mode*
        // rather than something on screen: whatever is drawn over the map is
        // what a press is aimed at, and the ☰ dropdown in particular is one of
        // the two places the mode is armed from.
        //
        // Being here at all is what makes an armed drag cancellable by the two
        // gestures that mean "back out" everywhere else — and on Android it is
        // what stops the back button from exiting the app while a mode is on,
        // which is the reading of a back press least likely to be what was meant.
        //
        if self.section_draw_armed {
            self.set_section_draw_armed(false);
            return true;
        }
        // The second armed mode, on the same footing and for the same reasons.
        // Never both — the setters hold them exclusive — so the order between
        // these two arms is unobservable, and they are adjacent so that a third
        // mode is added here rather than somewhere it would be missed.
        if self.region_pick_armed {
            self.set_region_pick_armed(false);
            return true;
        }
        false
    }

    pub(super) fn render_time_dialog(&mut self, ctx: &Context) -> Option<GuiAction> {
        // On Compact the sheet's Time page is the presentation (plan §1.9) —
        // the phone never draws this window.
        if !self.time_dialog.show || self.layout.width == crate::ui_layout::WidthClass::Compact {
            return None;
        }

        let mut action = None;
        egui::Window::new("Set Time")
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            // Centred in the content rect, not the viewport: on a device
            // with a notch or a nav bar those differ, and centring on the
            // viewport puts the dialog partly underneath them.
            .default_pos(self.layout.dialog_center())
            .show(ctx, |ui| {
                action = self.render_time_dialog_body(ui);
            });
        action
    }

    /// Apply the view change the frame asked for, if any.
    ///
    /// Called from [`Self::ui`] after the pane loop, where every pane is back in
    /// the vector. Changing what a pane draws keeps everything about what it is
    /// looking *at* — see `PaneState::set_view` — so there is nothing else to
    /// carry across.
    pub(super) fn apply_pending_pane_view(&mut self, actions: &mut Vec<GuiAction>) {
        let Some((pane_idx, view)) = self.pending_pane_view.take() else {
            return;
        };
        match self.panes.get_mut(pane_idx) {
            Some(pane) => {
                // Before the change, because after it the pane no longer
                // remembers it was drawing a volume. A voxel grid is 1–8 MiB of
                // host memory plus a GPU texture, refcounted by the volume it was
                // built from, and this is the only moment a pane can stop
                // needing one without anything else noticing: the pane is still
                // on screen, still on the same site, still live. Nothing else in
                // the frame is going to come back and ask.
                //
                // Keyed on the render *view* rather than the pane kind, because
                // leaving 3D for the plan view no longer changes the kind — and
                // a pane that quietly kept an 8 MiB grid it had stopped drawing
                // is exactly the leak this call exists to close.
                if pane.render_view() == rustdar_radar::types::RenderView::Volume
                    && view != rustdar_radar::types::RenderView::Volume
                {
                    actions.push(GuiAction::ReleaseVolume { pane_idx });
                }
                pane.set_view(view);
            }
            // A pane the layout no longer holds, which a pane-count change in the
            // same frame can produce. Dropped rather than clamped to another
            // index: changing a pane the user did not point at is worse than
            // changing none.
            None => log::warn!("pane {pane_idx} is gone; not switching it to {view:?}"),
        }
    }

    /// Give the line this frame drew to a pane, converting or creating one if
    /// need be.
    ///
    /// Called from [`Self::ui`] after the pane loop, where every pane is back in
    /// the vector and growing the count can no longer desynchronise a rect from
    /// the click that was hit-tested against it.
    ///
    /// # The target rule is total
    ///
    /// A drawn line always lands somewhere. Four steps, in order, and the order
    /// is the whole design:
    ///
    /// 1. **A section pane already sourced from this map.** Drawing a second
    ///    line on a map the user has already sectioned means "cut *there*
    ///    instead", not "give me another section pane" — otherwise three lines
    ///    fill the screen with panes nobody asked for.
    /// 2. **Grow the layout.** A section beside the map it was cut from is the
    ///    picture the feature is for, and it costs the user nothing they had.
    /// 3. **The lowest-indexed section pane.** The layout is full; re-aiming an
    ///    existing section is the cheapest thing that can still answer.
    /// 4. **The highest-indexed pane that is not the one drawn on.** Converting
    ///    a map is a real loss, so it is last — but it is *there*, because the
    ///    alternative is a drag that silently does nothing. The pane drawn on is
    ///    excluded because taking away the map under the line, while other panes
    ///    exist to take instead, is the one conversion that is certainly wrong.
    /// 5. **The pane drawn on.** Reachable only in a one-pane layout that cannot
    ///    grow — a phone in portrait — and right there: on a screen with room
    ///    for one thing, asking for a section is asking to look at a section.
    ///    The pane's site, product and viewport all survive the conversion, so
    ///    turning the checkbox back off restores the map it was.
    pub(super) fn apply_pending_section_line(&mut self) {
        let Some((source, line)) = self.pending_section_line.take() else {
            return;
        };

        // Whatever the source map is looking at, so a line drawn on a
        // reflectivity map cuts reflectivity. A product with no vertical
        // structure is carried across too, rather than quietly swapped: the
        // pane says which product it cannot slice and offers the picker to
        // change it, where a silent substitution would leave the user reading a
        // moment they did not ask for.
        let (source_product, source_site, source_scan) = match self.panes.get(source) {
            Some(pane) => (
                pane.selected_product,
                pane.site.clone(),
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
            // Total by construction: `highest_pane_other_than` only answers
            // `None` in a one-pane layout, and in one the source *is* the only
            // pane there is. A drawn line is never silently dropped.
            .unwrap_or(source);

        let Some(pane) = self.panes.get_mut(target) else {
            log::warn!("no pane could hold the section drawn on pane {source}");
            return;
        };
        pane.set_kind(crate::pane::PaneKind::CrossSection);
        pane.selected_product = source_product;
        pane.site = source_site;
        pane.scan_info = source_scan;
        if let Some(section) = pane.cross_section_mut() {
            section.line = Some(line);
            section.source_pane = Some(source);
            // The picture on screen is of the old line. Cleared rather than
            // left to the staleness comparison, because a section pane whose
            // texture outlives its line shows a cut through ground the user is
            // no longer pointing at, for as long as the re-cut takes.
            section.section = None;
            section.texture = None;
            section.unavailable = None;
            section.rendered_for = None;
        }
        self.active_pane = target;
    }

    /// Give the region this frame dragged to a pane, converting or creating one
    /// if need be.
    ///
    /// Called from [`Self::ui`] after the pane loop, beside
    /// [`Self::apply_pending_section_line`] and for its reason: this can grow
    /// the pane count, and a mid-loop growth moves the rects of every pane the
    /// loop has not reached yet away from the ones `detect_active_pane_click`
    /// hit-tested at the top of this same frame.
    ///
    /// # The target rule is total, and it is the section rule's shape
    ///
    /// A dragged region always lands somewhere. Five steps, in order:
    ///
    /// 1. **A 3D pane already sourced from this map.** A second drag on a map
    ///    the user has already aimed a 3D view from means "look *there*
    ///    instead", not "give me another 3D view" — otherwise adjusting a box
    ///    three times fills the screen with panes nobody asked for. This is the
    ///    common case after the first drag, and it is why
    ///    [`VolumePane::source_pane`](crate::pane::VolumePane::source_pane)
    ///    exists.
    /// 2. **Grow the layout.** A 3D view beside the map it was picked from is
    ///    the picture the feature is for, and it costs the user nothing they
    ///    had — in particular it does not cost them the map they just dragged
    ///    on, which they will want again to adjust the box.
    /// 3. **The lowest-indexed 3D pane**, whatever map aimed it. The layout is
    ///    full; re-aiming an existing 3D view is the cheapest thing that can
    ///    still answer, and it beats converting because converting destroys a
    ///    pane the user set up.
    /// 4. **The highest-indexed pane that is not the one dragged on.** A real
    ///    loss, so it is last but one — and it is *there*, because the
    ///    alternative is a drag that silently does nothing, which is
    ///    indistinguishable from one the app failed to receive. The pane
    ///    dragged on is excluded because taking the map out from under the box
    ///    the user just drew, while other panes exist to spend, is the one
    ///    conversion that is certainly wrong.
    /// 5. **The pane dragged on.** Reachable only in a one-pane layout that
    ///    cannot grow — a phone in portrait — and right there: on a screen with
    ///    room for one thing, asking for a 3D region is asking to look at it.
    ///    Nothing is lost that a plan view cannot restore, because 3D is a
    ///    *render mode* of the same map pane: the site, the product, the
    ///    viewport and the plan view itself all survive, and the mode toggle is
    ///    the way back.
    ///
    /// # Why the target takes the source map's site and moment
    ///
    /// The region names **ground**, and a 3D pane left on another site would
    /// resample its own radar over it — a box drawn on an Oklahoma map filled
    /// with a Florida volume, registered to the wrong place and captioned as if
    /// it were right. So the site, the product and the scan follow the region,
    /// exactly as they follow a section line.
    pub(super) fn apply_pending_region(&mut self) {
        let Some((source, region)) = self.pending_region.take() else {
            return;
        };

        let (source_product, source_site, source_scan) = match self.panes.get(source) {
            Some(pane) => (
                pane.selected_product,
                pane.site.clone(),
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
            // Total by construction: `highest_pane_other_than` only answers
            // `None` in a one-pane layout, and in one the source *is* the only
            // pane there is. A dragged region is never silently dropped.
            .unwrap_or(source);

        let Some(pane) = self.panes.get_mut(target) else {
            log::warn!("no pane could hold the region picked on pane {source}");
            return;
        };
        // A cross-section pane has no render mode, so the kind comes first.
        // `set_kind` keeps everything about what the pane is looking at, and
        // `set_map_render` does nothing at all for a pane already in 3D — which
        // is step 1's whole case, and is what keeps a re-aim from throwing away
        // the camera the user spent a while aiming.
        pane.set_kind(crate::pane::PaneKind::Map);
        pane.set_map_render(crate::pane::MapRender::Volume);
        pane.selected_product = source_product;
        pane.site = source_site;
        pane.scan_info = source_scan;
        if let Some(volume) = pane.volume_mut() {
            volume.region = Some(region);
            volume.source_pane = Some(source);
            // The picture on screen is of the old box, which a fresh drag can
            // put across the state from. Blanked rather than left to the
            // staleness key — which would notice, `region` being part of
            // `VolumeTarget` — because a volume of somewhere else entirely is
            // wrong for as long as it stands, and the rebuild is not instant.
            // This is `apply_pending_section_line`'s judgement, and the
            // handle-drop's opposite one does not apply: a picked region is a
            // new aim, not the repeating step of walking a box through a storm.
            volume.rendered_for = None;
        }
        self.active_pane = target;
    }

    /// Write a dropped handle's line onto the section pane it belongs to.
    ///
    /// Called from [`Self::ui`] after the pane loop, where every pane is back
    /// in the vector. The write is the line and **nothing else** — no target
    /// rule (the drop already names its pane), no growth, and deliberately no
    /// clearing of the picture on screen:
    ///
    /// # Why the old picture stands until the new cut lands
    ///
    /// [`Self::apply_pending_section_line`] blanks the pane, because a freshly
    /// drawn line can be across the state from the old one and a picture of
    /// somewhere else entirely is wrong for as long as it stands. A handle
    /// drop is an *adjustment*: the new line overlaps the old one's ground,
    /// the user's eyes are on the track they just moved, and this drop is the
    /// repeating step of walking a line through a storm — blanking to
    /// "Cutting the cross-section…" on every drop would strobe the pane
    /// exactly when the user is using it most. The stale picture stands for
    /// the fraction of a second the re-cut takes, the same way a section of
    /// the previous *volume* stands while its successor is cut, and the
    /// staleness key — which carries the line — is what notices and re-cuts
    /// without any help from here.
    pub(super) fn apply_pending_section_edit(&mut self) {
        let Some((pane_idx, line)) = self.pending_section_edit.take() else {
            return;
        };
        let Some(section) = self
            .panes
            .get_mut(pane_idx)
            .and_then(|p| p.cross_section_mut())
        else {
            // A pane-count change or a conversion in the same frame. Dropped
            // rather than retargeted: re-aiming a pane the user did not drag
            // on is worse than losing an adjustment they can repeat.
            log::warn!("pane {pane_idx} is no longer a section pane; dropping the edited line");
            return;
        };
        section.line = Some(line);
    }
}
