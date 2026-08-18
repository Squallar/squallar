//! The [`Gui`]'s overlay-registry access and pane-config swap sites,
//! split out of `ui.rs` at WO-E1 — the whole enclosing fn of each.
//! Bodies are verbatim moves.
use super::*;

impl Gui {
    /// Render **one** handler's controls — the only place handler
    /// [`ControlItem`]s render, hosted by the inspector's layer body.
    ///
    /// The round trip is the old 12-kind loop's, for one kind: load the
    /// active pane's config snapshot into the handlers, render the tree,
    /// apply updates, honour Fetch effects, then save the (possibly mutated)
    /// handler state back to the pane. This is what makes every sub-control
    /// (categories, day, products, etc.) per-pane when Sync Layers is off —
    /// and why there must be exactly one such pass per frame: each pass ends
    /// by overwriting the pane's configs with the handlers' state, so a
    /// second pass would save over the first's writes with whatever it had
    /// loaded before them. The `control_render_passes` counter holds the
    /// suite to that.
    ///
    /// The handler's own [`is_master_control`] items — its heading and its
    /// master `enabled` toggle — are skipped: the inspector's crumb names the
    /// layer and its "Show <layer>" toggle is the master, so rendering the
    /// handler's copies would put two of each on screen with only one wired
    /// to [`Self::select_layer`]'s discipline. The parity walk excludes them
    /// through the same predicate, so the two cannot drift.
    pub(super) fn render_overlay_controls_one(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        kind: OverlayKind,
        actions: &mut Vec<GuiAction>,
    ) {
        #[cfg(test)]
        {
            self.probes.control_render_passes += 1;
        }

        // Load this pane's config snapshot into the handlers.
        if !pane.overlay_configs.is_empty() {
            self.overlays.load_pane_configs(&pane.overlay_configs);
        }

        let ctx = self.active_pane_control_context();

        // Render controls and collect updates.
        let mut updates: Vec<(OverlayKind, ControlUpdate)> = Vec::new();
        let mut probe = ControlProbe::default();

        let controls = self.overlays.controls(kind, &ctx);
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

        // Apply updates and handle effects.
        let mut pane_ctx = PaneControlContextMut {
            pane_idx: self.active_pane,
            pane_state: None,
        };

        let active_pane = self.active_pane;
        for (kind, update) in updates {
            let effect = self.overlays.apply_control(kind, &update, &mut pane_ctx);
            if matches!(effect, ControlEffect::Fetch) {
                // Refresh, and every option change that implies one. A user
                // pressing Refresh on a layer that has been backing off — or
                // one given up on entirely — must be answered now.
                push_user_overlay_fetch(&mut self.overlays, actions, kind, active_pane);
            }
        }

        // Save the (possibly mutated) handler state back to the pane.
        pane.overlay_configs = self.overlays.save_pane_configs();
        pane.enabled_overlays = self.overlays.save_enabled_map();
    }

    /// [`Self::write_pane_overlay`] plus the enable-fetch rule, in one place
    /// for its three callers — the stack's eye, the inspector's Show toggle
    /// and the catalog's tiles.
    ///
    /// The rule: a layer turned on with **nothing to draw, or nothing worth
    /// trusting**, fetches now rather than waiting out an auto-poll interval —
    /// the same effect its own sub-toggles ask for, and the only route for a
    /// layer (SPC outlooks) that never auto-polls. `pane` is the caller's —
    /// taken or not — and `pane_idx` is the index the fetch is attributed to,
    /// because two of the callers hold the pane out of the vector where
    /// `active_pane` cannot be assumed to be it (the preset applier walks every
    /// pane).
    ///
    /// The health half of that condition is the fix for the recovery that did
    /// not recover. The guard was `!has_data(kind)` alone, and `has_data` is
    /// `!data.is_empty()` — so a layer that had worked, then started failing,
    /// was *holding* data and therefore did not re-ask. Toggling it off and on
    /// did nothing at all, which is the one case where the user is most likely
    /// to try it: an alerts layer painting a warning set that stopped updating
    /// an hour ago looks exactly like one that is current. "Off and on again"
    /// has to mean something, and now it means "re-ask".
    ///
    /// What the original guard was for still holds: a layer with fresh, healthy
    /// data does not spend a request on being switched on. That is what keeps a
    /// preset that enables eight layers on four panes from becoming thirty-two
    /// requests, and it is why this is not simply the guard deleted.
    ///
    /// One fetch per kind per frame: a second enable of the same kind in the
    /// same batch (a preset enabling it on every pane) finds the first's
    /// action already queued and does not queue another — the handlers are
    /// global, so one fetch serves every pane.
    pub(super) fn set_pane_overlay_with_fetch(
        &mut self,
        pane: &mut PaneState,
        pane_idx: usize,
        kind: OverlayKind,
        on: bool,
        actions: &mut Vec<GuiAction>,
    ) {
        Self::write_pane_overlay(&mut self.overlays, pane, kind, on);
        let stale = self
            .overlays
            .fetch_health(kind)
            .is_some_and(FetchHealth::is_unhealthy);
        if on && (!self.overlays.has_data(kind) || stale) && !self.overlays.is_fetching(kind) {
            // Switching a layer on is a user action, so it clears whatever the
            // ladder had accumulated — see `push_user_overlay_fetch`.
            push_user_overlay_fetch(&mut self.overlays, actions, kind, pane_idx);
        }
    }

    /// Initialize per-pane `enabled_overlays` from the current handler states.
    ///
    /// Called after `new()`, after `load_ui_config()` (backward compatibility
    /// for configs without per-pane maps), and when the pane-count picker
    /// grows the vector — anywhere a pane could otherwise be left with an
    /// empty map that `is_overlay_enabled` reads as everything-off.
    pub fn initialize_pane_enabled(&mut self) {
        let defaults = self.overlays.build_enabled_map();
        let default_configs = self.overlays.save_pane_configs();
        for pane in &mut self.panes {
            for (&kind, &enabled) in &defaults {
                pane.enabled_overlays.entry(kind).or_insert(enabled);
            }
            // Seed overlay configs from handler defaults for panes with empty configs.
            if pane.overlay_configs.is_empty() {
                pane.overlay_configs = default_configs.clone();
            }
        }
    }

    /// Set one pane's overlay state, writing the config as well as the enabled
    /// map — `render_overlay_controls_one` reloads the handlers from the config
    /// every frame it runs, so a write to `enabled_overlays` alone is undone.
    #[cfg(test)]
    pub(crate) fn set_overlay_on_pane_for_test(&mut self, idx: usize, kind: OverlayKind, on: bool) {
        let configs = self.panes[idx].overlay_configs.clone();
        if !configs.is_empty() {
            self.overlays.load_pane_configs(&configs);
        }
        self.overlays.set_enabled(kind, on);
        let configs = self.overlays.save_pane_configs();
        let enabled = self.overlays.save_enabled_map();
        let pane = &mut self.panes[idx];
        pane.overlay_configs = configs;
        pane.enabled_overlays = enabled;
    }

    /// The [`ControlItem`] tree `kind`'s handler is currently offering — the
    /// *model* behind the [`DrawnControlItem`]s, asked of the handler rather
    /// than of the renderer, exactly as [`Self::dropdown_model_for_test`] asks
    /// for one dropdown.
    #[cfg(test)]
    pub(crate) fn control_item_model_for_test(&self, kind: OverlayKind) -> Vec<ControlItem> {
        let ctx = self.active_pane_control_context();
        self.overlays.controls(kind, &ctx)
    }

    /// The `(options, selected)` a handler is currently offering under `label`
    /// — the *model* behind a [`DrawnDropdown`], asked of the handler rather
    /// than of the renderer.
    #[cfg(test)]
    pub(crate) fn dropdown_model_for_test(
        &self,
        label: &str,
    ) -> Option<(Vec<(String, String)>, String)> {
        let ctx = self.active_pane_control_context();
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
        OVERLAY_CONTROL_ORDER
            .iter()
            .find_map(|&kind| find(&self.overlays.controls(kind, &ctx), label))
    }

    /// Turn a texture overlay on for every pane, as ticking its layer toggle does.
    ///
    /// The handler's own state has to be written back into each pane's
    /// `overlay_configs`, not just into `enabled_overlays`: every frame reloads the
    /// registry from the pane's configs and then saves the enabled map back out, so
    /// a pane whose config still says "off" turns itself off again on the next frame.
    #[cfg(test)]
    pub(crate) fn enable_overlay_for_test(&mut self, kind: OverlayKind) {
        self.overlays.set_enabled(kind, true);
        let configs = self.overlays.save_pane_configs();
        let enabled = self.overlays.save_enabled_map();
        for pane in &mut self.panes {
            pane.overlay_configs = configs.clone();
            pane.enabled_overlays = enabled.clone();
        }
    }

    /// When one overlay's auto-refresh is next due, on the same terms as
    /// [`Self::auto_poll_delay`]. `None` when this layer does not auto-poll,
    /// when no pane on screen can draw it, or when its fetch is already in
    /// flight.
    pub(super) fn overlay_poll_delay(&self, kind: OverlayKind) -> Option<std::time::Duration> {
        if !self.any_pane_has_overlay_enabled(kind) {
            return None;
        }
        // The same reading `check_auto_polls` fires on, not a second derivation
        // of it. These were two spellings of one rule — one in whole seconds,
        // one in durations — and a wake spent on a frame that polls nothing is
        // the busy loop with extra steps.
        self.overlays.auto_fetch_delay(kind)
    }
}
