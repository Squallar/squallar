//! The UI fade: one confirmed click on the bare map of the already-active
//! pane hides every floating surface; the next one brings them back. Fading
//! closes surfaces in state, not paint; unfading only clears the flag.

/// How long the fade and every surface open/close animates, in seconds —
/// §3.3's ~0.15–0.22 s band. Zero under test.
pub(super) fn anim_time() -> f32 {
    if cfg!(test) { 0.0 } else { 0.18 }
}

/// Dim a transitioning surface: paint at `opacity`, and while any transition
/// is in flight (`opacity < 1`) take the widgets out of interaction.
pub(super) fn dim(ui: &mut egui::Ui, opacity: f32) {
    if opacity < 1.0 {
        ui.multiply_opacity(opacity);
        ui.disable();
    }
}

impl super::Gui {
    /// The floating chrome's visibility this frame: `None` once the fade-out
    /// has completed — the surface must not render at all, which is what
    /// makes it input-transparent — otherwise the opacity to draw at.
    pub(super) fn chrome_fade(&self) -> Option<f32> {
        (self.fade_factor > 0.0).then_some(self.fade_factor)
    }

    /// Frame-top guard: while faded, nothing may be open. A surface open
    /// anyway means the user acted through a route no pointer guard can see
    /// (Tab-focus plus Enter; pane-borne chrome outside the bar's rect). The
    /// repair is to **unfade**, not to re-close. Also resolves this frame's
    /// [`Gui::fade_factor`] — one animation read per frame, shared by all.
    pub(super) fn enforce_fade_invariants(&mut self, ctx: &egui::Context) {
        if self.ui_faded {
            let open = self.layers_panel_visible()
                || self.insp_open
                || self.menu_open
                || self.menu_popup_open
                || self.catalog_open
                || self.time_dialog.show
                || !self.overlays.selected_overlays.is_empty()
                || self.pill_revealed.is_some()
                || self
                    .panes
                    .iter()
                    .take(self.pane_layout.pane_count)
                    .any(|pane| pane.volume().is_some_and(|v| v.alpha_editor_open));
            if open {
                self.ui_faded = false;
            }
        }
        self.fade_factor =
            ctx.animate_bool_with_time(egui::Id::new("ui_fade"), !self.ui_faded, anim_time());
    }

    /// Resolve the pane loop's fade verdict — called from
    /// [`Gui::ui`](super::Gui::ui) after the pending appliers, once the
    /// loop's consumption flag is final.
    pub(super) fn apply_fade_toggle(&mut self, ctx: &egui::Context) {
        let mut flipped = false;
        if self.ui_faded && self.click_consumed_frame {
            self.ui_faded = false;
            flipped = true;
        }
        if std::mem::take(&mut self.fade_candidate) {
            if self.ui_faded {
                self.ui_faded = false;
            } else {
                self.ui_faded = true;
                self.fade_close_all();
            }
            flipped = true;
        }
        if flipped {
            self.fade_factor =
                ctx.animate_bool_with_time(egui::Id::new("ui_fade"), !self.ui_faded, anim_time());
        }
    }

    /// The fade's close half: every openable surface, for real — state, not paint.
    pub(super) fn fade_close_all(&mut self) {
        self.clear_sheet_pages();
        self.stack_open = Some(false);
        self.pill_revealed = None;
        if self.menu_popup_open {
            self.menu_popup_open = false;
            self.menu_popup_close_requested = true;
        }
        for pane in self.panes.iter_mut().take(self.pane_layout.pane_count) {
            if let Some(volume) = pane.volume_mut() {
                volume.alpha_editor_open = false;
            }
        }
    }

    /// The unfade-before-acting choke point (§3.6): a primary press or
    /// release inside the top bar's rect while faded clears the fade before
    /// the frame draws the floating chrome. Spatial on purpose: every
    /// top-bar handler lives inside this one rect.
    pub(super) fn clear_fade_on_top_bar_press(
        &mut self,
        ctx: &egui::Context,
        bar_rect: egui::Rect,
    ) {
        if !self.ui_faded {
            return;
        }
        let pressed_in_bar = ctx.input(|i| {
            (i.pointer.primary_pressed() || i.pointer.primary_released())
                && i.pointer
                    .interact_pos()
                    .is_some_and(|pos| bar_rect.contains(pos))
        });
        if pressed_in_bar {
            self.ui_faded = false;
            self.fade_factor =
                ctx.animate_bool_with_time(egui::Id::new("ui_fade"), true, anim_time());
        }
    }

    /// Whether a click in the pane loop can qualify as a fade gesture, for
    /// the parts the loop does not already know: the press must not be the
    /// one that activated the pane, must not have landed with a popup open
    /// (recorded at press time — the popup is gone by the confirm frame),
    /// and no dialog may outrank it. Panel surfaces deliberately do not
    /// block; closing them is the fade's own job.
    pub(super) fn fade_gesture_allowed(&self) -> bool {
        !self.press_switched_pane
            && !self.press_popup_open
            && !self.time_dialog.show
            && !self.catalog_open
    }

    /// Whether the UI is faded, for the harness.
    #[cfg(test)]
    pub(crate) fn ui_faded_for_test(&self) -> bool {
        self.ui_faded
    }
}
