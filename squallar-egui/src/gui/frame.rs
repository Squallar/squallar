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

    /// **What the overlay layers retired this frame**, for a caller that
    /// drives a frame without the app's discard seam.
    ///
    /// [`Gui::ui`] drops the batch, which frees it right here — the frame
    /// thread — and that is exactly what the seam exists to avoid. It is
    /// tolerable in the harnesses and tests that spelling is for, and the
    /// production caller is [`Gui::ui_phased`]'s third return.
    pub fn take_retired_overlay_data(&self) -> Vec<Box<dyn std::any::Any + Send>> {
        self.overlays.take_retired()
    }

    /// [`Gui::ui`], with the fourteen instants at which it crossed its own
    /// phase boundaries.
    ///
    /// The stamps are taken unconditionally — **six** clock reads here, two
    /// more inside `render_shell_phased` and six inside
    /// `render_stack_and_inspector` (one on a frame that draws no panel), on
    /// a call that lays out a whole frame — so there is no armed/unarmed
    /// spelling of this function to disagree about, and no frame where the
    /// instrument is off. What the caller does with them is the caller's
    /// business; see [`crate::shell_api::UiPhaseStamps`] for why they are
    /// instants.
    ///
    /// The count read "five" from before the topbar/statusbar pair landed.
    /// Recounted here rather than adjusted, on `frame_ledger`'s rule: its
    /// figures have drifted every time a split landed and a reader who sizes
    /// anything off a stale count writes against the wrong shape.
    pub fn ui_phased(
        &mut self,
        ctx: &egui::Context,
    ) -> (
        Vec<GuiAction>,
        UiPhaseStamps,
        Vec<Box<dyn std::any::Any + Send>>,
    ) {
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

        // **The glyph atlas, the one host texture no family named.** epaint
        // holds it for the life of the `Context`, `min(max_texture_side,
        // 16384)` wide, doubling its height on demand and evicting nothing;
        // `TextureAtlas::max_height` is the width, so it can reach the width
        // squared before `Fonts::begin_pass` recycles it. Until this line
        // every byte of it landed in the census residual, and `gpu textures`
        // named only the device copy.
        //
        // Published from here for the reason the overlay families above are:
        // the `Context` is the UI layer's, and reaching across for it would
        // grow the app layer's coupling for a counter.
        //
        // One `Context::fonts` per frame. That is a write lock on the
        // context, and it is the same lock `walkers::AtlasStamp::read`
        // already takes once per pane per frame from `paint_labels`; this
        // adds one more of it, before anything paints, where nothing is
        // contending for it. No walk: `font_image_size` is two `usize`s off
        // the image header.
        crate::heap_census::set_font_atlas_bytes(ctx.fonts(|f| {
            let [w, h] = f.font_image_size();
            (w as u64) * (h as u64) * 4
        }));

        // **The denominator the census never had.** Every family above says
        // who is holding the heap; none of them says how big the heap is, and
        // on native there is no `byteLength` to take a residual against - so
        // the line printed `residual unknown` and a measured scene sat
        // 1,441 MiB above what the census could name with nothing saying so.
        //
        // Idempotent, and NOT a reading: after the first frame this is one
        // atomic load. Both `/proc` readings are taken on the sampler's own
        // thread precisely because the cheap one is 11 us and the expensive
        // one is 3.3 ms, and neither belongs on a frame.
        crate::heap_census::spawn_process_sampler(
            crate::heap_census::PROCESS_SAMPLE_PERIOD,
            crate::heap_census::PROCESS_WALK_EVERY,
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
            self.probes.last_download_area_boxes.clear();
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

        // The seventh `ui` cut, and its own seven sub-cuts beside it: the
        // call runs a pane loop, so those are nanosecond sums it accumulates
        // rather than instants this function could stamp.
        let (pane_actions, panes_cuts) = self.render_panes(&mut root_ui, &shell.out.excluded_rects);
        actions.extend(pane_actions);
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

        // **Before the drain, so what it lets go of leaves on this frame.**
        self.release_data_of_layers_no_pane_draws();

        // **Last, and per-frame.** A layer's state retires at delivery and
        // its memos retire at dispatch — two different passes — so this is
        // the only place that sees both. The batch is RETURNED rather than
        // discarded here: this crate does not depend on the worker, and the
        // app files it against the pool's free lane.
        let retired = self.overlays.take_retired();

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
                // The stack's own six, taken inside `render_shell_phased` and
                // carried here rather than returned beside the tuple: the
                // App's call site keeps its arity and gains no new reach.
                stack: shell.stack,
                // And the panes' own seven, on the same terms and for the
                // same reason — nanosecond sums, not stamps, because the cut
                // they open is a loop.
                panes_cuts,
            },
            retired,
        )
    }

    /// **Let go of the source data of every layer no pane draws.**
    ///
    /// Asked once a frame, at the END of one, and both of those are the
    /// point.
    ///
    /// A **per-frame** question rather than a per-click one, because the
    /// answer changes after the click that provoked it. The ordinary
    /// sequence with a split open is: the user hides a layer on the active
    /// pane; a click-time check correctly declines, because a linked sibling
    /// still draws it; and the off-switch is then propagated to that sibling
    /// wholesale by `propagate_layer_state`, with nothing asking again. The
    /// texture release already carries a line inside that fan-out for exactly
    /// this reason. Asking every frame needs no such line and covers every
    /// other route a pane can stop drawing a layer — a pane closed, a slot
    /// curated out, a config loaded.
    ///
    /// At the **end** of the frame, because that is where every `mem::take`n
    /// pane is back in the vector: a mid-frame walk would read a pane that is
    /// currently out as drawing nothing, and drop the data it is drawing
    /// with.
    ///
    /// [`Gui::any_pane_has_overlay_enabled`] is the predicate, and it is the
    /// same one the poll gate hangs off (`some_pane_could_use`) — one
    /// definition of "does any pane still show this", not a second that can
    /// drift from it.
    ///
    /// **A layer with a 3D half is excluded**, and this is the over-firing
    /// guard rather than an optimisation. That predicate asks
    /// `PaneState::draws_ground`, which is `false` for a Volume pane with its
    /// floor hidden and for a cross-section — so a pane rendering a gridded
    /// layer in 3D reads as not drawing it, and a drop would blank a live
    /// layer. None of the layers that implement `release_data` has a 3D half
    /// today; the guard is what keeps that true if one gains it.
    pub(crate) fn release_data_of_layers_no_pane_draws(&mut self) {
        if self.pane_layout.pane_count == 0 {
            return;
        }
        let idle: Vec<squallar_source::id::LayerId> = self
            .overlays
            .handlers()
            .filter(|handler| handler.volume().is_none())
            .map(|handler| handler.id())
            .filter(|id| !self.any_pane_has_overlay_enabled(id))
            .collect();
        for id in idle {
            if let Some(handler) = self.overlays.get_handler_mut(&id) {
                handler.release_data();
            }
        }
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

        // **And only for a pane that needs the data.** The first fetch takes
        // the *active* pane's config, so the active pane is the one asked:
        // with its radar layer off, this arm holds, `never_asked` stays true,
        // and the very next frame after the layer comes on — or after the user
        // moves to a pane that draws radar — it fires. Nothing is lost by
        // waiting, and what it saves is a 6.9 MB volume download and its 103.3
        // MB decode on a scene showing no radar (`FLOOR.f1`, 2026-09-07).
        let active_wants_radar = self.active_pane().needs_radar_data();

        if never_asked && active_wants_radar && !self.fetching() {
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

            // **`live_sites` rather than a walk of its own.** This loop used
            // to re-spell that function — same `viewing_live` filter, same
            // first-seen dedupe, same pane order — and the copy is what let
            // the cadence keep pulling the archive for a site whose radar
            // layer had been switched off after `live_sites` learned to ask.
            // One spelling, so the chunk feed, the notification sockets and
            // this cadence cannot disagree about which sites are wanted.
            for site in self.live_sites() {
                let config = RadarConfig {
                    site,
                    timestamp: current_scan_time,
                };
                actions.push(GuiAction::CheckForNewScans(config));
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

        // **Every distinct ask on screen, not the first pane's.** A layer's
        // round is shaped by the pane it is built against, so a poll that named
        // one pane refreshed one pane's selection and left every other pane
        // drawing the answer it happened to arrive with — for the life of the
        // session, with the layer's clock, health and status line all reading
        // fresh. The radar arm above has fanned out over its own distinct asks
        // (`seen_sites`) all along; this is the same shape for the layers whose
        // ask is a selection rather than a site.
        //
        // **The registry answers which ones are due; this used to ask it one
        // id at a time.** The old spelling collected every handler's id and
        // then handed each back to `auto_fetch_delay`, which resolved it by
        // scanning the id vector it had just been read out of — one linear
        // scan per registered layer, every frame, to arrive back at the
        // handler the iterator had already been standing on. The registry
        // reads the delay beside the id instead, so the ordinary frame — on
        // which nothing is due — pays no lookup, no probe and no allocation
        // at all, rather than eighteen lookups and a hundred and seventy-one
        // comparisons for an empty answer.
        for kind in self.overlays.ids_due_for_auto_fetch() {
            for pane_idx in self.panes_owed_a_round(&kind) {
                actions.push(GuiAction::FetchOverlay {
                    kind: kind.clone(),
                    pane_idx,
                });
            }
        }
    }

    /// **One pane index per distinct ask the panes drawing `kind` would make**,
    /// in pane order — who a due round is started on behalf of.
    ///
    /// Asked **per frame**, and that is what covers the layer-link fan-out. A
    /// pane's selection is propagated to its linked siblings wholesale, inside
    /// the same frame as the click that provoked it, so a check made at the
    /// click reads the state before the propagation and nothing asks again.
    /// Asking here needs no hook inside `propagate_layer_state` — the same
    /// reason [`Gui::release_data_of_layers_no_pane_draws`] is a per-frame
    /// question rather than a per-click one.
    ///
    /// **Deduplicated on what a round actually asks for, and on nothing else.**
    /// Every `create_fetch_tasks` that reads its pane at all reads exactly two
    /// things: this layer's own per-pane state, through the handler's own view
    /// of [`PaneRef::state`]; and the instant and window the pane depicts,
    /// which `fetch_config_for_layer` narrows `FetchConfig::as_of` and the
    /// depicted span by. None of them reads the pane's site, its index or its
    /// sibling slots. So the key is those two members and no more — a wider key
    /// turns two panes showing one product into two identical downloads, and a
    /// narrower one collapses two panes scrubbed to different hours into a
    /// single round, which is this very defect on the other axis.
    fn panes_owed_a_round(&mut self, kind: &squallar_source::id::LayerId) -> Vec<usize> {
        let wanting = self.panes_with_overlay_enabled(kind);
        // The ordinary case, and it costs exactly what naming one pane cost:
        // no hydrate, no serialize, no key.
        if wanting.len() < 2 {
            return wanting;
        }
        let Self {
            overlays, panes, ..
        } = self;
        // **Only a layer whose picture is a function of the depicted instant
        // carries a window in its key**, and this is `as_of_for_layer`'s own
        // predicate verbatim — the one that decides whether the fetch context
        // is narrowed to the pane's clock at all, and the same one the
        // depicted span and frames are held back by. A `TimeAxis::Live` layer
        // keeps the wall clock however far a pane is scrubbed, so two panes
        // parked at different hours ask the national feed for the very same
        // bytes; splitting them would buy a second identical download for a
        // difference the request never carries.
        let depicted_matters = overlays.handlers().any(|handler| {
            handler.id() == *kind
                && matches!(
                    handler.time_axis(),
                    squallar_source::time::TimeAxis::EventLifetime
                        | squallar_source::time::TimeAxis::FrameSeries { .. }
                )
        });
        let mut asks: Vec<RoundAsk> = Vec::with_capacity(wanting.len());
        let mut owed: Vec<usize> = Vec::with_capacity(wanting.len());
        for idx in wanting {
            // An unhydrated pane carries no state at all and would answer for
            // the layer's defaults — the precondition `App::with_layer_pane`
            // states before it hands a handler a pane view, and the same one
            // `Gui::across_panes` states before it builds the arrival union.
            panes[idx].hydrate_layer_states(overlays, idx);
            let ask = round_ask(&panes[idx], overlays, kind, depicted_matters);
            if asks.contains(&ask) {
                continue;
            }
            asks.push(ask);
            owed.push(idx);
        }
        owed
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

/// **What one pane's round of a layer would ask for**, as far as anything
/// outside the handler can see it — see [`Gui::panes_owed_a_round`] for why it
/// is these two members and no others.
type RoundAsk = (serde_json::Value, Option<DepictedWindow>);

/// **The window a scrubbed pane depicts**: the instant, the width its listing
/// was asked over, and the stops its clock can land on. `None` on a live pane,
/// where all three of the app-side narrowings fall back to the wall clock and
/// two live panes therefore ask the same thing — and `None` again for a layer
/// whose time axis means those narrowings never fire, however far its panes
/// are scrubbed apart.
type DepictedWindow = (chrono::NaiveDateTime, u64, Vec<chrono::NaiveDateTime>);

/// [`RoundAsk`] for one pane.
///
/// The selection is taken as **the handler's own serialization** of the pane's
/// state, which is the description of a selection that already exists — the one
/// the pane persists — rather than a second one invented here that could go on
/// reading equal after a handler gained a field. `slot.config` is the same
/// bytes only *after* `adopt_handler_state` has run: on a pane that has merely
/// been hydrated it is still `null`, so keying on it would read two identical
/// panes as two different asks.
fn round_ask(
    pane: &crate::pane::PaneState,
    overlays: &squallar_overlays::render::overlay_state::OverlayRegistry,
    kind: &squallar_source::id::LayerId,
    depicted_matters: bool,
) -> RoundAsk {
    let selection = pane
        .slot(kind)
        .and_then(|slot| slot.state.as_deref())
        .map_or(serde_json::Value::Null, |state| {
            overlays.serialize_pane_state(kind, state as &dyn std::any::Any)
        });
    let depicted = pane
        .time
        .mode
        .as_of()
        .filter(|_| depicted_matters)
        .map(|instant| {
            let timeline = pane.transport_state();
            // The width the listing was actually asked over while a loop is armed,
            // and the Lookback slider when one is not — `depicted_stops`' own two
            // arms, which is what the fetch's depicted span is derived from.
            let span = if timeline.is_active() {
                timeline.span_secs
            } else {
                pane.time.span_secs
            };
            let stops: Vec<chrono::NaiveDateTime> = timeline
                .frames
                .iter()
                .map(|frame| frame.timestamp)
                .collect();
            (instant, span, stops)
        });
    (selection, depicted)
}
