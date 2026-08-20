//! The [`Gui`]'s overlay-registry access and pane-config swap sites, split out of
//! `ui.rs` — the whole enclosing fn of each.
use super::*;
use rustdar_source::handler::PaneMut;
use rustdar_source::handler::PaneRef;
use rustdar_source::id::LayerId;

/// How a radar archive round finished. Two arms because the ladder treats
/// them oppositely: a delivery wipes it, a failure files against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoundOutcome<'a> {
    Delivered,
    /// …carrying what went wrong, because the layer's options panel prints it.
    Failed(&'a str),
}

impl Gui {
    /// Render **one** handler's controls — the only place handler [`ControlItem`]s
    /// render, hosted by the inspector's layer body.
    pub(super) fn render_overlay_controls_one(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        kind: &LayerId,
        actions: &mut Vec<GuiAction>,
    ) {
        #[cfg(test)]
        {
            self.probes.control_render_passes += 1;
        }

        pane.hydrate_layer_states(&self.overlays, self.active_pane);

        let mut updates: Vec<(LayerId, ControlUpdate)> = Vec::new();
        let mut probe = ControlProbe::default();

        let controls = {
            let view = pane.view(self.active_pane);
            self.overlays.controls(kind, &view.layer(kind))
        };
        for item in controls.iter().filter(|item| !is_master_control(item)) {
            render_control_item(ui, kind, item, &mut updates, &mut probe);
        }

        #[cfg(test)]
        {
            self.probes
                .last_dropdowns
                .extend(probe.drawn.iter().cloned());
            self.probes
                .last_control_items
                .extend(probe.items.iter().cloned());
        }
        #[cfg(not(test))]
        let _ = probe;

        let active_pane = self.active_pane;
        let visible = self.pane_layout.pane_count;
        for (kind, update) in updates {
            // The other panes' state for the same layer. A control edit that
            // moves what the LAYER is asking for — the outlook's day and
            // product set — has to weigh this pane's new selection against
            // the ones it is not editing, or it takes the layer off a ledger
            // another pane's selection is still on. The edited pane is not in
            // here: `self.panes[active_pane]` is the `mem::take`n placeholder
            // while `pane` is out, and its own half is `state` below.
            let peers: Vec<&dyn std::any::Any> = self
                .panes
                .iter()
                .take(visible)
                .filter_map(|p| p.slot(&kind))
                .filter_map(|slot| slot.state.as_deref())
                .map(|s| s as &dyn std::any::Any)
                .collect();
            // The REAL slot state, not `None`: an edit that landed in a
            // scratch context would be silently dropped, and the control
            // would read as applied.
            let mut pane_ctx = PaneMut {
                pane_idx: active_pane,
                state: pane
                    .slot_mut(&kind)
                    .and_then(|slot| slot.state.as_deref_mut())
                    .map(|s| s as &mut dyn std::any::Any),
                peers: &peers,
            };
            let effect = self.overlays.apply_control(&kind, &update, &mut pane_ctx);
            if matches!(effect, ControlEffect::Fetch) {
                push_user_overlay_fetch(&mut self.overlays, actions, kind, active_pane);
            }
            // The live-chunk switch's second half, and the refresh button's
            // whole answer. Both are radar-shaped and both are asked of the
            // radar glue, which decides for itself whether this edit was one
            // of its own. The edited pane is the `mem::take`n one the caller
            // holds, so it is chained in: the vector's slot for it is a
            // placeholder until the inspector puts it back.
            crate::radar_layer::fan_out_live_chunks(
                self.panes.iter_mut().chain(std::iter::once(&mut *pane)),
                &update,
            );
            if crate::radar_layer::refresh_requested(&update) {
                // **`pane`, not `self.active_pane()`.** While this body
                // renders, the pane vector's slot for the active pane is the
                // `mem::take`n placeholder the inspector's host left behind,
                // and its site is the default one — so the shared config's
                // site is substituted from the pane the caller is holding,
                // exactly as the settings row this button replaced did.
                actions.push(GuiAction::FetchRadarScan(RadarConfig {
                    site: pane.site().to_string(),
                    timestamp: self.time_dialog.timestamp,
                }));
            }
        }

        pane.adopt_handler_state(&self.overlays);
    }

    /// [`Self::write_pane_overlay`] plus the enable-fetch rule, in one place for
    /// its three callers — the stack's eye, the inspector's Show toggle and the
    /// catalog's tiles.
    pub(super) fn set_pane_overlay_with_fetch(
        &mut self,
        pane: &mut PaneState,
        pane_idx: usize,
        kind: &LayerId,
        on: bool,
        actions: &mut Vec<GuiAction>,
    ) {
        Self::write_pane_overlay(&mut self.overlays, pane_idx, pane, kind, on);
        let stale = self
            .overlays
            .fetch_health(kind)
            .is_some_and(FetchHealth::is_unhealthy);
        if on
            && (!self
                .overlays
                .has_data(kind, &pane.layer_ref(pane_idx, kind))
                || stale)
            && !self.overlays.is_fetching(kind)
        {
            push_user_overlay_fetch(&mut self.overlays, actions, kind.clone(), pane_idx);
        }
    }

    /// **Give every pane a slot for every registered layer.** A saved stack
    /// names only the layers the writing build had, so the ones this build
    /// serves and the file never mentioned join here — each at the position
    /// its `draw_order_weight` puts it in, which is the reconcile the flat
    /// `draw_order` used to get on its own.
    ///
    /// The joined slot takes the **layer's own default**, and nothing else:
    /// it used to take `is_enabled` off the registry, and to copy the
    /// registry's whole serialize into a pane that had saved nothing. Both
    /// read one pane's state and wrote it into every other — which is what
    /// the config swap was, and it dies with it. A slot with a `null` config
    /// gets its state from `create_pane_state(slot.enabled)` at the hydrate
    /// below, which is the same answer without the borrowed opinion.
    pub fn initialize_pane_enabled(&mut self) {
        let mut wanted: Vec<(LayerId, u32, bool)> = self
            .overlays
            .handlers()
            .map(|h| (h.id(), h.draw_order_weight(), h.default_enabled()))
            .collect();
        // Weight order, so each insertion lands among slots that are already
        // in it — the same walk `reconcile_draw_order` made.
        wanted.sort_by_key(|&(_, weight, _)| weight);
        let weights: std::collections::HashMap<LayerId, u32> = wanted
            .iter()
            .map(|(id, weight, _)| (id.clone(), *weight))
            .collect();
        for pane in &mut self.panes {
            pane.insert_missing_slots(&wanted, &|id| weights.get(id).copied());
        }
        // Every pane that has just gained slots also needs the state those
        // slots stand for — this is the one place every pane-making site
        // already goes through.
        let Self {
            panes, overlays, ..
        } = self;
        for (idx, pane) in panes.iter_mut().enumerate() {
            pane.hydrate_layer_states(overlays, idx);
        }
    }

    /// Run the hydrate every caller runs before asking a handler about a
    /// pane. It is where the pane publishes its radar selection into the slot
    /// a handler reads it from, so a test that moves a selection and then asks
    /// a handler goes through here exactly as the frame does.
    #[cfg(test)]
    pub(crate) fn hydrate_pane_layer_states_for_test(&mut self, idx: usize) {
        let Self {
            overlays, panes, ..
        } = self;
        panes[idx].hydrate_layer_states(overlays, idx);
    }

    /// Set one pane's overlay state, writing the config as well as the enabled map
    /// — `render_overlay_controls_one` reloads the handlers from the config every
    /// frame it runs, so a write to `enabled_overlays` alone is undone.
    #[cfg(test)]
    pub(crate) fn set_overlay_on_pane_for_test(&mut self, idx: usize, kind: &LayerId, on: bool) {
        let Self {
            overlays, panes, ..
        } = self;
        let pane = &mut panes[idx];
        pane.hydrate_layer_states(overlays, idx);
        pane.set_layer_enabled(overlays, idx, kind, on);
        pane.adopt_handler_state(overlays);
    }

    /// The [`ControlItem`] tree `kind`'s handler is currently offering — the
    /// *model* behind the [`DrawnControlItem`]s, asked of the handler rather than
    /// of the renderer, exactly as [`Self::dropdown_model_for_test`] asks for one
    /// dropdown.
    #[cfg(test)]
    pub(crate) fn control_item_model_for_test(&self, kind: &LayerId) -> Vec<ControlItem> {
        let view = self.panes[self.active_pane].view(self.active_pane);
        self.overlays.controls(kind, &view.layer(kind))
    }

    /// The `(options, selected)` a handler is currently offering under `label` —
    /// the *model* behind a [`DrawnDropdown`], asked of the handler rather than of
    /// the renderer.
    #[cfg(test)]
    pub(crate) fn dropdown_model_for_test(
        &self,
        label: &str,
    ) -> Option<(Vec<(String, String)>, String)> {
        let view = self.panes[self.active_pane].view(self.active_pane);
        fn find(items: &[ControlItem], label: &str) -> Option<(Vec<(String, String)>, String)> {
            for item in items {
                match item {
                    ControlItem::Dropdown {
                        label: l,
                        options,
                        selected,
                        ..
                    } if l == label => {
                        return Some((options.clone(), selected.clone()));
                    }
                    ControlItem::Section { items, .. } => {
                        if let Some(found) = find(items, label) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        // The live registry, same as the walk: a handler with a dropdown that
        // no list mentions is still a handler this helper can reach.
        let registered: Vec<rustdar_source::id::LayerId> =
            self.overlays.handlers().map(|h| h.id()).collect();
        registered
            .iter()
            .find_map(|kind| find(&self.overlays.controls(kind, &view.layer(kind)), label))
    }

    /// Turn a texture overlay on for every pane, as ticking its layer toggle does.
    #[cfg(test)]
    pub(crate) fn enable_overlay_for_test(&mut self, kind: &LayerId) {
        let Self {
            overlays, panes, ..
        } = self;
        overlays.set_enabled(kind, true, &mut PaneMut::bare(0));
        for (idx, pane) in panes.iter_mut().enumerate() {
            // Through the pane, not through the registry: a converted handler
            // keeps "on" in the pane's own state, and a write to the registry
            // alone is one `adopt_handler_state` away from being undone.
            pane.hydrate_layer_states(overlays, idx);
            pane.set_layer_enabled(overlays, idx, kind, true);
            pane.adopt_handler_state(overlays);
        }
    }

    /// Drive one control edit on ONE pane through the same construction
    /// `render_overlay_controls_one` uses — the REAL slot state, and the other
    /// panes' state as peers.
    ///
    /// Exists so a test can diverge two panes the way a user does, without an
    /// `egui::Ui`. A test that built a `PaneMut` of its own would be free to
    /// pass `None` and prove nothing, which is the silent-partial-success
    /// shape this whole order is about.
    #[cfg(test)]
    pub(crate) fn apply_control_on_pane_for_test(
        &mut self,
        idx: usize,
        kind: &LayerId,
        update: &ControlUpdate,
    ) -> ControlEffect {
        let visible = self.pane_layout.pane_count;
        let mut pane = std::mem::take(&mut self.panes[idx]);
        pane.hydrate_layer_states(&self.overlays, idx);
        let peers: Vec<&dyn std::any::Any> = self
            .panes
            .iter()
            .take(visible)
            .filter_map(|p| p.slot(kind))
            .filter_map(|slot| slot.state.as_deref())
            .map(|s| s as &dyn std::any::Any)
            .collect();
        let mut pane_ctx = PaneMut {
            pane_idx: idx,
            state: pane
                .slot_mut(kind)
                .and_then(|slot| slot.state.as_deref_mut())
                .map(|s| s as &mut dyn std::any::Any),
            peers: &peers,
        };
        let effect = self.overlays.apply_control(kind, update, &mut pane_ctx);
        pane.adopt_handler_state(&self.overlays);
        self.panes[idx] = pane;
        effect
    }

    /// Re-run every pane's write-back against the registry as it stands **now**
    /// — the step a layer toggle, a control edit and the autosave all end
    /// with. Exists so a test can put the registry's own copy at odds with the
    /// panes and prove which one the saved bytes came from.
    #[cfg(test)]
    pub(crate) fn readopt_panes_for_test(&mut self) {
        let Self {
            overlays, panes, ..
        } = self;
        for pane in panes.iter_mut() {
            pane.adopt_handler_state(overlays);
        }
    }

    /// **Hand the site table to the layer that draws it.**
    ///
    /// The table lives in `rustdar-radar`, which `rustdar-overlays` must not
    /// name (WO-M3's edge cut), so the shell that already reads it for the
    /// per-frame site labels installs the rows through the ordinary arrival
    /// door. Re-run whenever the table moves — a catalogue landing
    /// mid-session places radars this call did not have — which is why the
    /// App calls it again from `adopt_the_first_catalogue`.
    pub fn publish_radar_sites(&mut self) {
        use rustdar_overlays::render::handlers::sites::{RadarSitesFetchResult, SiteRow};
        let rows: Vec<SiteRow> = rustdar_radar::sites::radars()
            .iter()
            .map(|site| SiteRow {
                name: site.name.to_string(),
                lat: site.lat,
                lon: site.lon,
            })
            .collect();
        self.deliver_overlay_fetch(
            rustdar_overlays::render::overlay_state::OverlayFetchResult {
                kind: rustdar_source::id::known::RADAR_SITES,
                data: Box::new(RadarSitesFetchResult(rows)),
            },
        );
    }

    /// **Re-place every pane's volume against the site table as it stands.**
    ///
    /// Answers how many moved. A volume decoded before its radar was in the
    /// table is named
    /// [`UNKNOWN_SITE_NAME`](rustdar_radar::sites::UNKNOWN_SITE_NAME), and
    /// everything that looks a volume up by that name then misses — on the
    /// dispatch path that means the volume is fetched, decoded, and never
    /// rasterised. Run beside [`Self::publish_radar_sites`] whenever the table
    /// moves: the pictures already on screen were named against the old one.
    pub fn place_shown_volumes_against_the_table(&mut self) -> usize {
        let mut replaced = 0;
        for pane in self.panes_mut() {
            let site = pane.site().to_string();
            if let Some(info) = pane.scan_info.as_mut()
                && info.place_against_the_table(&site)
            {
                replaced += 1;
            }
        }
        replaced
    }

    /// **Take delivery of one overlay fetch round.**
    ///
    /// The arrival names a layer and no pane, so the handler is handed a
    /// [`PaneRef::across`]: no `state`, and every visible pane's in `peers`.
    /// What a handler asks of it — is this product still asked for, which
    /// grid must the shared cache not evict — is a question about every pane
    /// at once, and the answer is the union. Visible panes only, the same set
    /// [`Self::any_pane_has_overlay_enabled`] polls on behalf of.
    pub fn deliver_overlay_fetch(
        &mut self,
        result: rustdar_overlays::render::overlay_state::OverlayFetchResult,
    ) {
        let id = result.kind.clone();
        self.across_panes(&id, |overlays, pane| {
            overlays.apply_fetch_result(result, pane);
        });
    }

    /// **A frame listing arriving for one layer**, with the scope the layer
    /// captured when it asked.
    ///
    /// Through the same door as [`Self::deliver_overlay_fetch`], but **what
    /// scopes it is the `scope`, not the pane**: the union's config is null
    /// by construction, so a handler that read a site out of the `PaneRef`
    /// would read nothing, and two panes on two sites would file one
    /// another's frames. The union is still what is handed over, for the
    /// question the other two arrivals ask of it.
    pub fn deliver_frame_listing(
        &mut self,
        id: &rustdar_source::id::LayerId,
        listing: rustdar_source::time::FrameListing,
        scope: rustdar_overlays::render::overlay_state::FetchPayload,
    ) {
        self.across_panes(id, |overlays, pane| {
            overlays.apply_frames(id, listing, scope, pane);
        });
    }

    /// **One frame's data arriving for one layer.**
    pub fn deliver_frame(
        &mut self,
        id: &rustdar_source::id::LayerId,
        stamp: rustdar_source::time::FrameStamp,
        data: rustdar_overlays::render::overlay_state::FetchPayload,
    ) {
        self.across_panes(id, |overlays, pane| {
            overlays.apply_frame(id, stamp, data, pane);
        });
    }

    /// **The one construction of the arrival-path pane view**, so the three
    /// deliveries above cannot build three different unions.
    ///
    /// A pane whose slots have never been hydrated carries no state at all,
    /// and would silently drop out of the union — so the union is taken over
    /// hydrated panes, not over whatever happens to be ready.
    fn across_panes<R>(
        &mut self,
        id: &rustdar_source::id::LayerId,
        f: impl FnOnce(&mut rustdar_overlays::render::overlay_state::OverlayRegistry, &PaneRef<'_>) -> R,
    ) -> R {
        let visible = self.pane_layout.pane_count;
        let Self {
            overlays, panes, ..
        } = self;
        for (idx, pane) in panes.iter_mut().enumerate().take(visible) {
            pane.hydrate_layer_states(overlays, idx);
        }
        let peers: Vec<&dyn std::any::Any> = panes
            .iter()
            .take(visible)
            .filter_map(|pane| pane.slot(id))
            .filter_map(|slot| slot.state.as_deref())
            .map(|state| state as &dyn std::any::Any)
            .collect();
        f(overlays, &PaneRef::across(&peers))
    }

    /// **The [`ControlItem`] tree `kind`'s handler is offering the active
    /// pane** — a layer's declared surface, read rather than drawn.
    ///
    /// This is the only door `rustdar-egui` and the shell have to a handler's
    /// own fields: the registry hands out `&dyn SourceHandler` and `as_any` is
    /// refused, so a switch that lives inside a handler is read back off the
    /// control it declares for it.
    pub fn layer_controls(&self, kind: &LayerId) -> Vec<ControlItem> {
        let view = self.panes[self.active_pane].view(self.active_pane);
        self.overlays.controls(kind, &view.layer(kind))
    }

    /// **The [`ControlItem`] tree `kind`'s handler offers with no pane at
    /// all** — the layer's own answer, with every per-pane override out of the
    /// way. [`PaneRef::bare`]'s config is null by construction, so a control
    /// whose value is "this pane's copy, else the global" answers with the
    /// global.
    pub fn layer_default_controls(&self, kind: &LayerId) -> Vec<ControlItem> {
        self.overlays
            .controls(kind, &PaneRef::bare(self.active_pane))
    }

    /// **Apply one control edit to a layer from outside its inspector body** —
    /// the ☰ menu's leaves and the shell's own callers, which hold a switch's
    /// new value and nothing else.
    ///
    /// Generic by construction: it names a [`LayerId`] and a
    /// [`ControlUpdate`], never a field. It replaces the three `Gui::set_*`
    /// methods WO-E8b deleted, and it is not a rename of them — those wrote
    /// `Gui` fields that no longer exist, and this writes the handler's own
    /// through the same door the inspector uses.
    ///
    /// **A bare pane view, and deliberately.** Every switch reached this way is
    /// a layer-global one, so there is no per-pane state for the edit to land
    /// in, and handing it a real pane view would drag `adopt_handler_state` in
    /// behind it and re-derive every OTHER layer's flag as a side effect of
    /// toggling this one. The inspector's copy of the same control goes through
    /// the full pane construction and writes the same field.
    ///
    /// The radar glue is given every edit afterwards because one of them —
    /// the live-chunk switch — has a second half no handler can write: see
    /// [`crate::radar_layer::fan_out_live_chunks`].
    pub fn apply_layer_control(&mut self, kind: &LayerId, update: &ControlUpdate) {
        let mut pane = PaneMut::bare(self.active_pane);
        self.overlays.apply_control(kind, update, &mut pane);
        crate::radar_layer::fan_out_live_chunks(self.panes.iter_mut(), update);
    }

    /// Switch the archive poll on or off — the one write behind the ☰ menu
    /// leaf and (from the inspector) the layer's own control row, so neither
    /// can disagree with the other about the switch's state.
    pub(super) fn set_auto_poll_enabled(&mut self, on: bool) {
        let id = crate::radar_layer::POLL_LAYER;
        let update = crate::radar_layer::auto_poll_update(on);
        self.apply_layer_control(&id, &update);
    }

    /// **What the radar layer's round ended as**, which is the only thing the
    /// shell knows about it that the layer cannot see for itself.
    ///
    /// The archive fetch is dispatched by the shell and answered on a channel
    /// the shell drains, so success and failure reach the layer here rather
    /// than through `apply_fetch_result` — the arrival carries a decoded
    /// volume, not this layer's own payload.
    pub(super) fn end_radar_round(&mut self, outcome: RoundOutcome<'_>) {
        let id = crate::radar_layer::POLL_LAYER;
        // Both doors end the round themselves, which is why neither arm
        // clears the in-flight flag beside them.
        self.across_panes(&id, |overlays, pane| {
            match outcome {
                // The ladder resets and the layer returns to its interval.
                RoundOutcome::Delivered => overlays.record_fetch_success(&id, pane),
                // …and files against it, which is what spaces a failing origin
                // out instead of asking it again on the next frame.
                //
                // **Transient, always.** What reaches the shell is a message,
                // not a status code, so nothing here can honestly call a round
                // a refusal — which means radar walks the 2-4-8…-interval
                // ladder and never reaches the `Broken` floor. Sniffing the
                // string for one would be a guess wearing a classification's
                // clothes; giving radar a real one means the fetch path
                // reporting a classified `FetchError`, which is app-side.
                RoundOutcome::Failed(message) => overlays.record_fetch_failure(
                    &id,
                    &rustdar_source::fetch_policy::FetchError::transient(message),
                    pane,
                ),
            }
        });
    }

    /// Mark the radar layer's round in flight, or not. The rising edge stamps
    /// the layer's poll clock — see `RadarSource::set_fetching`.
    pub(super) fn set_radar_round_in_flight(&mut self, in_flight: bool) {
        let id = crate::radar_layer::POLL_LAYER;
        self.across_panes(&id, |overlays, pane| {
            overlays.set_fetching(&id, in_flight, pane);
        });
    }

    /// When one layer's auto-refresh is next due, on the same terms as
    /// [`Self::auto_poll_delay`].
    pub(super) fn overlay_poll_delay(&self, kind: &LayerId) -> Option<std::time::Duration> {
        if !self.some_pane_could_use(kind) {
            return None;
        }
        self.overlays.auto_fetch_delay(kind)
    }

    /// **Whether some pane on screen could use this layer's next round.**
    ///
    /// For every layer but one that is "the layer is on in some pane". The
    /// radar layer is the exception, and the predicate is not
    /// interchangeable: a pane scrubbed to an archive time still has radar
    /// **enabled**, so the enabled test would have it poll for live data
    /// nothing on screen is asking for. What it asks instead is whether any
    /// pane is viewing live.
    ///
    /// That question is **presentation state** — it is about panes, and the
    /// handler contract has never carried pane posture ([`PaneRef`] reaches
    /// `auto_fetch_delay` nowhere; it takes no pane at all). So the term stays
    /// here rather than moving into radar's own answer, by orchestrator ruling
    /// (26). Whether every polling layer should be suppressed while all panes
    /// are scrubbed is a real question and a different one: the eleven overlay
    /// layers poll regardless of scrub posture today, and changing that is a
    /// behaviour change no order has asked for.
    fn some_pane_could_use(&self, kind: &LayerId) -> bool {
        if *kind == crate::radar_layer::POLL_LAYER {
            return self.is_any_pane_live();
        }
        self.any_pane_has_overlay_enabled(kind)
    }
}
