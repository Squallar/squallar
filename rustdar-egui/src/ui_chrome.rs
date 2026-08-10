//! The chrome pass: the docked top bar, then the floating surfaces around the
//! full-bleed map.
//!
//! # One docked panel; everything else floats
//!
//! The Synthesis design's full-bleed rule is that the map fills everything
//! under the top bar and all other chrome floats over it, inside its bounds.
//! So exactly one `Panel` claims space here — the top bar (`ui_topbar.rs`) —
//! and what is left of the root `Ui` **is** the map's `CentralPanel`, edge to
//! edge. That remainder is captured once, as [`ChromeOutput::map_rect`], and
//! every floating surface positions itself from it: the status bar along the
//! bottom inset (`ui_statusbar.rs`), the layers panel at top-left (below),
//! and the timeline transport the frame draws later, after the pane loop
//! (`ui_timeline.rs`).
//!
//! The floating surfaces are `egui::Area`s above `Order::Background`, which is
//! what keeps the map from reacting underneath them: every map click resolver
//! runs through `filter_dialog_blocked`/`is_pos_blocked`, whose layer check
//! drops a position covered by any layer above Background — no excluded-rect
//! plumbing required.
//!
//! # Ids do not depend on the breakpoint
//!
//! Every area, panel and combo-box id prefix uses one constant id regardless
//! of which presentation is on screen. egui keys widget memory — combo state,
//! scroll offsets, panel sizes — on those ids, so keying any of them on the
//! layout would silently reset the user's UI state every time the window
//! crossed a breakpoint. The two pre-rebuild files had exactly that hazard
//! latent in them: `"d_"`/`"m_"` control prefixes and
//! `layers_panel`/`mobile_layers_panel` could never collide only because the
//! two files were never compiled together.
//!
//! The full-bleed flip also *strengthened* the positional-id story. egui's
//! `Ui::new_child` computes `unique_id = stable_id.with(parent's
//! next_auto_id_salt)` (`egui-0.35.0/src/ui.rs:255`), so the root `Ui`'s
//! auto-id counter folds into every panel's registered id — which is why a
//! panel that appears or vanishes with the width would re-key everything shown
//! after it. With the status bar and layers panel now floating `Area`s (whose
//! ids are their own roots, not children of the root `Ui`), the top bar is the
//! only thing that advances that counter at all, and it is drawn at every
//! width. `crossing_a_breakpoint_re_keys_nothing` pins the whole claim, and
//! `crossing_a_breakpoint_does_not_move_any_widget_id` pins the stored-state
//! half of it.

use crate::actions::GuiAction;

/// Width of the layers panel, in both its persistent and drawer forms.
///
/// One value, not two, because the panel keeps one egui id: a per-form width
/// would make the panel jump when the window crossed the breakpoint, for no
/// reason a user could see.
const LAYERS_PANEL_WIDTH: f32 = 240.0;

/// Width of combo boxes inside the layers panel.
const COMBO_BOX_WIDTH: f32 = 150.0;

/// Id prefix for every widget in the layers panel.
///
/// Deliberately one constant and not a per-layout string: see the module note.
const LAYER_CONTROL_ID_PREFIX: &str = "layers_";

/// The layers panel's inset from the map's top-left corner.
const LAYERS_PANEL_INSET: f32 = 8.0;

/// What the panel leaves clear above the map's bottom edge: room for the
/// status bar and the timeline transport floating there (plan §1.3).
const LAYERS_PANEL_BOTTOM_CLEARANCE: f32 = 88.0;

/// What the chrome produced this frame.
pub(super) struct ChromeOutput {
    pub actions: Vec<GuiAction>,
    /// Screen rects of floating chrome drawn *over* the map, which map click
    /// handling must not treat as map clicks.
    ///
    /// This is an **output** of the chrome rather than something the map
    /// reconstructs — only the code that draws a floating thing knows where it
    /// is. Empty in practice since the hamburger went: everything left either
    /// claims panel space or is an egui layer above `Background`, which the
    /// layer half of `is_pos_blocked` catches with no plumbing. The mechanism
    /// stays because painted-in-pane chrome has no layer to be caught by, and
    /// the next thing painted over a pane will need it again.
    pub excluded_rects: Vec<egui::Rect>,
    /// What the top bar left of the content rect — the rect the map's
    /// `CentralPanel` will fill, captured here so the floating surfaces (and
    /// the timeline, drawn after the pane loop) can position against it
    /// without re-deriving the top bar's height.
    pub map_rect: egui::Rect,
}

impl super::Gui {
    /// Draw all the chrome around the map: the one docked panel first, then
    /// the floating surfaces positioned in what it left.
    pub(super) fn render_chrome(&mut self, ui: &mut egui::Ui) -> ChromeOutput {
        let mut actions = Vec::new();

        self.render_top_bar(ui, &mut actions);

        // Everything the top bar did not claim is the map's — the full-bleed
        // rect every floating surface positions itself in.
        let map_rect = ui.available_rect_before_wrap();

        self.render_status_bar(ui.ctx(), map_rect, &mut actions);

        // Persistent-by-default sidebar on Expanded, drawer elsewhere; the top
        // bar's Layers toggle is the one way in and out on every width.
        if self.layers_panel_visible() {
            self.render_layers_panel(ui.ctx(), map_rect, &mut actions);
        }

        ChromeOutput {
            actions,
            excluded_rects: Vec::new(),
            map_rect,
        }
    }

    /// The layers panel, floating at the map's top-left, in whichever of its
    /// two forms this width calls for.
    ///
    /// The body is identical either way; only the header differs, because the
    /// drawer covers the map and wants a close button where the user already
    /// is — the sidebar's way out is the top bar's Layers toggle.
    ///
    /// Since the full-bleed flip this is an `egui::Area` over the map rather
    /// than a docked `Panel::left`: opening and closing it no longer resizes
    /// the map, and its clicks are kept off the map by the layer check, like
    /// every other floating surface (see the module note).
    fn render_layers_panel(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        actions: &mut Vec<GuiAction>,
    ) {
        let is_drawer = !self.layout.width.has_persistent_sidebar();

        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);

        // The panel stops well short of the map's bottom edge so the status
        // bar and the timeline keep a clear band to float in.
        let max_body_height =
            (map_rect.height() - LAYERS_PANEL_INSET - LAYERS_PANEL_BOTTOM_CLEARANCE).max(0.0);

        egui::Area::new(egui::Id::new("layers_panel"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::LEFT_TOP)
            // The sizing-pass ceiling. Without this an area's first pass caps
            // its `Ui` at `Spacing::default_area_size` (400pt tall), the
            // scroll area inside fills exactly that, and the committed size
            // becomes a fixed point the panel never grows out of — the
            // `max_height` below is what actually bounds the body.
            .default_size(map_rect.size())
            .fixed_pos(map_rect.left_top() + egui::vec2(LAYERS_PANEL_INSET, LAYERS_PANEL_INSET))
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.global_style()).show(ui, |ui| {
                    ui.set_width(LAYERS_PANEL_WIDTH);
                    ui.horizontal(|ui| {
                        ui.heading("Layers");
                        if is_drawer {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("\u{2715}").clicked() {
                                        self.drawer_open = false;
                                    }
                                },
                            );
                        }
                    });
                    ui.separator();

                    // An explicit salt rather than egui's positional auto-id.
                    //
                    // This is defensive, not a fix for a live bug: the two
                    // header forms happen to allocate the same number of ids
                    // today (the drawer's close button is nested inside the
                    // `horizontal`, so it does not advance this Ui's counter),
                    // and the breakpoint test confirms the auto-id would
                    // currently be stable too. The salt makes that independent
                    // of *how many widgets precede it*, which is what an
                    // unrelated edit to the header would otherwise silently
                    // change — costing the user their scroll position on every
                    // resize, with nothing to point at.
                    let scroll = egui::ScrollArea::vertical()
                        .id_salt("layers_scroll")
                        .max_height(max_body_height)
                        .show(ui, |ui| {
                            self.render_layer_controls(
                                ui,
                                &mut pane,
                                COMBO_BOX_WIDTH,
                                LAYER_CONTROL_ID_PREFIX,
                                actions,
                            );
                        });

                    // Report the id egui really used, rather than reconstructing
                    // it: the test that pins id stability across a breakpoint has
                    // to be reading the same id the scroll state is stored under,
                    // or it proves nothing about that state surviving.
                    #[cfg(test)]
                    self.widget_id_probes.push(("layers_scroll", scroll.id));
                    #[cfg(not(test))]
                    let _ = scroll;
                });
            });

        self.panes[self.active_pane] = pane;
        // After the restore, so the source it copies from is the real pane rather
        // than the `mem::take` placeholder. It deliberately does **not** copy
        // `content`: a pane's kind is how this pane presents the shared subject,
        // not part of the subject, and propagating it would convert every sibling
        // the moment one pane became a 3D view — from a setting called "Sync
        // Layers". The reasoning is written out on `propagate_layer_sync` itself.
        self.propagate_layer_sync();
    }
}
