//! The [`Gui`]'s overlay-registry access and pane-config swap sites, split out of
//! `ui.rs` — the whole enclosing fn of each.
use super::*;
use rustdar_source::id::LayerId;

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

        if !pane.overlay_configs.is_empty() {
            self.overlays.load_pane_configs(&pane.overlay_configs);
        }

        let ctx = self.active_pane_control_context();

        let mut updates: Vec<(LayerId, ControlUpdate)> = Vec::new();
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

        let mut pane_ctx = PaneControlContextMut {
            pane_idx: self.active_pane,
            pane_state: None,
        };

        let active_pane = self.active_pane;
        for (kind, update) in updates {
            let effect = self.overlays.apply_control(&kind, &update, &mut pane_ctx);
            if matches!(effect, ControlEffect::Fetch) {
                push_user_overlay_fetch(&mut self.overlays, actions, kind, active_pane);
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
        Self::write_pane_overlay(&mut self.overlays, pane, kind, on);
        let stale = self
            .overlays
            .fetch_health(kind)
            .is_some_and(FetchHealth::is_unhealthy);
        if on && (!self.overlays.has_data(kind) || stale) && !self.overlays.is_fetching(kind) {
            push_user_overlay_fetch(&mut self.overlays, actions, kind.clone(), pane_idx);
        }
    }

    /// Initialize per-pane `enabled_overlays` from the current handler states.
    pub fn initialize_pane_enabled(&mut self) {
        let defaults = self.overlays.build_enabled_map();
        let default_configs = self.overlays.save_pane_configs();
        for pane in &mut self.panes {
            for (kind, &enabled) in &defaults {
                pane.enabled_overlays.entry(kind.clone()).or_insert(enabled);
            }
            if pane.overlay_configs.is_empty() {
                pane.overlay_configs = default_configs.clone();
            }
        }
    }

    /// Set one pane's overlay state, writing the config as well as the enabled map
    /// — `render_overlay_controls_one` reloads the handlers from the config every
    /// frame it runs, so a write to `enabled_overlays` alone is undone.
    #[cfg(test)]
    pub(crate) fn set_overlay_on_pane_for_test(&mut self, idx: usize, kind: &LayerId, on: bool) {
        let configs = self.panes[idx].overlay_configs.clone();
        if !configs.is_empty() {
            self.overlays.load_pane_configs(&configs);
        }
        self.overlays.set_enabled(kind, on);
        let pane = &mut self.panes[idx];
        pane.adopt_handler_state(&self.overlays);
    }

    /// The [`ControlItem`] tree `kind`'s handler is currently offering — the
    /// *model* behind the [`DrawnControlItem`]s, asked of the handler rather than
    /// of the renderer, exactly as [`Self::dropdown_model_for_test`] asks for one
    /// dropdown.
    #[cfg(test)]
    pub(crate) fn control_item_model_for_test(&self, kind: &LayerId) -> Vec<ControlItem> {
        let ctx = self.active_pane_control_context();
        self.overlays.controls(kind, &ctx)
    }

    /// The `(options, selected)` a handler is currently offering under `label` —
    /// the *model* behind a [`DrawnDropdown`], asked of the handler rather than of
    /// the renderer.
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
            .find_map(|kind| find(&self.overlays.controls(kind, &ctx), label))
    }

    /// Turn a texture overlay on for every pane, as ticking its layer toggle does.
    #[cfg(test)]
    pub(crate) fn enable_overlay_for_test(&mut self, kind: &LayerId) {
        self.overlays.set_enabled(kind, true);
        for pane in &mut self.panes {
            pane.adopt_handler_state(&self.overlays);
        }
    }

    /// When one overlay's auto-refresh is next due, on the same terms as
    /// [`Self::auto_poll_delay`].
    pub(super) fn overlay_poll_delay(&self, kind: &LayerId) -> Option<std::time::Duration> {
        if !self.any_pane_has_overlay_enabled(kind) {
            return None;
        }
        self.overlays.auto_fetch_delay(kind)
    }
}
