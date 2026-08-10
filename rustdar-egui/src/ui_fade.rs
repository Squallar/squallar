//! The UI fade (plan §1.8, §3.6): one confirmed click on the bare map of the
//! already-active pane hides every floating surface; the next one brings them
//! back.
//!
//! # The trigger is the click nothing else wanted
//!
//! The pane loop resolves every map click through one pipeline
//! (`ui_input::MapPointerFrame`), so by the time a position reaches
//! `overlay_click_pos` it is already a *click* — egui and the touch pipeline
//! both discard a drag — and already off every floating layer
//! (`filter_dialog_blocked`). The loop then adds the rest of the sentence: the
//! pane must be the active one, and active from **before** the press —
//! `Gui::press_switched_pane` records a press that activated its pane, so the
//! first click on an inactive pane only activates (§1.8) — the click must
//! land on a *map* pane's rect, no armed drag may own the gesture (the armed
//! resolvers clear the click themselves; a section-handle press is excluded
//! beside them), no dialog may be up (an open feature popup means the click
//! is "close that", and the catalog and Set Time dialogs outrank the
//! gesture), and no feature or site icon may have consumed the click
//! (`click_consumed`). What survives all of that is recorded as
//! [`Gui::fade_candidate`] and resolved here, after the pending appliers —
//! the same deferral every other loop-recorded intent gets.
//!
//! # Fading closes; unfading reopens nothing
//!
//! Fade ON closes the stack, the inspector, the menu (both the phone flag and
//! the ☰ popup's mirror), the catalog and every sheet page **for real** —
//! state, not paint — so nothing is ever "open but invisible" (§1.8). Fade
//! OFF only clears the flag: the chrome that was unconditionally on screen
//! (timeline, status bar, pills, phone bottom bar) returns, and the panels
//! stay closed until asked for. [`Gui::enforce_fade_invariants`] pins the
//! closed-while-faded half at the top of every frame — and repairs a breach
//! by unfading, never by re-closing (see its note).
//!
//! # The top bar never fades, and unfades before acting
//!
//! The bar is docked — fading it would leave a blank strip, not map — so it
//! stays, whole and interactive. Any press on it while faded clears the fade
//! *first* ([`Gui::clear_fade_on_top_bar_press`]), so nothing a bar control
//! opens can open invisibly: the guard is spatial — every handler the bar
//! will ever grow lives inside the panel's rect — rather than a first line
//! each handler has to remember. A map drag whose release lands over the bar
//! trips the guard too, and that is benign by direction: this guard can only
//! ever *unfade* — restore chrome, never hide it — so its worst false
//! positive is a UI the next tap can dismiss again. The keyboard can reach a
//! bar control with no pointer event at all (egui's Tab-focus plus Enter);
//! that route is spatially invisible here and is caught one frame later by
//! the invariant's unfade repair instead.
//!
//! # A consumed click while faded unfades
//!
//! The map stays fully interactive while faded — that is the point — so a
//! click can still hit a warning polygon or a site icon. The feature's
//! details must not open into an invisible UI, and a site switch is the user
//! *working*, not hiding; either way the consumed click clears the fade
//! before the frame draws what it opened. This is the §1.8 refinement that
//! keeps the invariant honest without eating the click.
//!
//! # The error surface outranks the gesture
//!
//! A deliberate refinement of §1.8's "fade all floating chrome": the error
//! toast stays visible while faded. An error the user hid *by accident* — the
//! fade is one tap — is an error unseen, and on this app's subject matter an
//! unseen "live feed failed" is the most expensive pixel on the screen. On
//! Compact the toast already floats on its own; the wide widths normally
//! carry the error inside the status bar, so while that bar is faded the same
//! toast presentation carries it instead (`Gui::ui` makes that call).
//!
//! # Animations, and why tests never see them
//!
//! Every open/close and the fade itself animate through
//! [`egui::Context::animate_bool_with_time`] over [`anim_time`] — but under
//! `cfg(test)` that time is **zero**, which egui's animation manager resolves
//! to an instant snap to the target. The harness drives discrete frames and
//! asserts on rects the same frame a state flips; a real animation would put
//! every one of those assertions inside a transition window. Production alone
//! renders the transitions, and a transitioning remnant is always
//! non-interactive (`Ui::disable`), so the animation can never make a closed
//! surface catch a click.

/// How long the fade and every surface open/close animates, in seconds —
/// §3.3's ~0.15–0.22 s band. Zero under test: see the module note.
pub(super) fn anim_time() -> f32 {
    if cfg!(test) { 0.0 } else { 0.18 }
}

/// Dim a transitioning surface: paint at `opacity`, and while any transition
/// is in flight (`opacity < 1`) take the widgets out of interaction — a
/// half-faded timeline must not catch the click meant for the map under it.
pub(super) fn dim(ui: &mut egui::Ui, opacity: f32) {
    if opacity < 1.0 {
        ui.multiply_opacity(opacity);
        ui.disable();
    }
}

impl super::Gui {
    /// The floating chrome's visibility this frame: `None` once the fade-out
    /// has completed — the surface must not render at all, which is what
    /// makes it input-transparent — otherwise the opacity to draw at, `1.0`
    /// meaning fully present. Callers pass anything below `1.0` through
    /// [`dim`].
    pub(super) fn chrome_fade(&self) -> Option<f32> {
        (self.fade_factor > 0.0).then_some(self.fade_factor)
    }

    /// Frame-top guard: while faded, nothing may be open — the fade *closed*
    /// everything. A surface found open anyway therefore means the user
    /// *acted*, through a route no pointer guard can see: egui's Tab-focus
    /// plus Enter activates a bar control with no pointer event for the
    /// spatial guard, and pane-borne chrome like the Volume Alpha button
    /// lives outside the bar's rect altogether. The repair is to **unfade**,
    /// not to re-close — re-closing would punish the action (the surface
    /// opens and silently vanishes, so the control reads as dead, to exactly
    /// the keyboard user who cannot see why), where unfading is the same
    /// §3.6 unfade-before-acting answer every pointer route gives, arriving
    /// one frame late: the opened surface stays open, and the chrome comes
    /// back around it. No assert — an open surface here is a legitimate
    /// input path, not a bug.
    ///
    /// Also resolves this frame's [`Gui::fade_factor`] — one animation read
    /// per frame, which every surface then shares, so the whole frame agrees
    /// how faded it is.
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
    ///
    /// A consumed click while faded unfades instead (module note): the
    /// feature dialog it opened, or the site switch it asked for, belongs in
    /// a working UI. The factor is refreshed after any flip so the surfaces
    /// drawn later this same frame — pills, timeline, bottom bar, sheet —
    /// already agree with the new state.
    pub(super) fn apply_fade_toggle(&mut self, ctx: &egui::Context) {
        let mut flipped = false;
        if self.ui_faded && self.click_consumed_frame {
            self.ui_faded = false;
            flipped = true;
        }
        if std::mem::take(&mut self.fade_candidate) {
            if self.ui_faded {
                // Fade OFF reopens nothing: the panels the fade closed stay
                // closed until asked for (§1.8).
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

    /// The fade's close half: every openable surface, for real. The sheet's
    /// own "close everything" already covers the page flags — the feature
    /// selection, the Set Time dialog, the catalog, the phone menu, the
    /// inspector (selection reset included) and the drawer — and the
    /// surfaces it does not know about close beside it: the Expanded sidebar
    /// (an explicit `Some(false)`, so the shell default cannot reopen it), a
    /// touch-revealed pill row, the ☰ dropdown through the same
    /// mirror-and-request pair `dismiss_top_layer` uses, and every visible
    /// 3D pane's Volume Alpha editor — floating chrome like the rest of them
    /// (§1.8), so it fades in state like the rest of them, and the unfade
    /// reopens it no more than it reopens the panels.
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
    /// release inside the top bar's rect while faded clears the fade, before
    /// the frame draws the floating chrome — so whatever the press goes on to
    /// do (open the menu, toggle the inspector, switch a pane) happens into a
    /// visible UI, the same frame.
    ///
    /// Spatial on purpose: every top-bar handler — the wide run's, the phone
    /// run's arms and ◧, the ☰ and anything the bar grows later — lives
    /// inside this one rect, so covering the rect provably covers them all,
    /// where a first-line call in each handler is a rule waiting to be
    /// forgotten. The press is enough (the release is checked too, for a
    /// same-frame click): unfading on the press means the click's release
    /// frame already runs against a restoring UI.
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
    /// the parts the loop does not already know from its own locals: the
    /// press must not be the one that activated the pane (§1.8's "first click
    /// on an inactive pane only activates"), it must not have landed with a
    /// popup open (egui closes a popover, dropdown or combo on the click
    /// outside it — that click is the dismissal, recorded at press time
    /// because the popup is gone by the confirm frame), and no dialog may
    /// outrank the gesture — the Set Time dialog and the catalog here, an
    /// open feature through the loop's own `pointer_available` (a click with
    /// a feature popup up means "close that", not "hide my UI"). The panel
    /// surfaces — stack, inspector, menu, the sheet's panel pages —
    /// deliberately do *not* block: a click reaching the map beside them is
    /// exactly the "give me the map" the fade is for, and closing them is
    /// the fade's own job.
    ///
    /// The dialog checks look redundant with the layer filter — the catalog
    /// modal's backdrop and the sheet's scrim already swallow most clicks —
    /// and are kept anyway: the Set Time window covers only its own rect, so
    /// "a dialog is up" cannot be read off the layer the click landed on.
    /// (The map slivers the scrim used to leave beside the bottom bar died
    /// with the full-bleed flush cluster — contract 61b — but the check's
    /// reasoning never rested on them.)
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
