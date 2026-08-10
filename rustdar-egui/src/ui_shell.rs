//! The shell pass: the docked top bar, then the floating surfaces around the
//! full-bleed map.
//!
//! # One docked panel; everything else floats
//!
//! The Synthesis design's full-bleed rule is that the map fills everything
//! under the top bar and all other chrome floats over it, inside its bounds.
//! So exactly one `Panel` claims space here — the top bar (`ui_topbar.rs`) —
//! and what is left of the root `Ui` **is** the map's `CentralPanel`, edge to
//! edge. That remainder is captured once, as [`ShellOutput::map_rect`], and
//! every floating surface positions itself from it: the status bar along the
//! bottom inset (`ui_statusbar.rs`), the layer stack at top-left
//! (`ui_stack.rs`), the inspector at top-right (`ui_inspector.rs`), and the
//! timeline transport the frame draws later, after the pane loop
//! (`ui_timeline.rs`).
//!
//! The floating surfaces are `egui::Area`s above `Order::Background`, which is
//! what keeps the map from reacting underneath them: every map click resolver
//! runs through `filter_dialog_blocked`/`is_pos_blocked`, whose layer check
//! drops a position covered by any layer above Background — no excluded-rect
//! plumbing required.
//!
//! Below the Compact breakpoint the corner-floating pass stands down: the
//! same flags present as pages of the phone sheet, drawn late in the frame by
//! `ui_sheet.rs` through the same body renderers and the same take window —
//! the shell keeps only the top bar there, and the status bar keeps nothing
//! (its short scan summary lives in the phone top bar).
//!
//! # One take window for the stack and the inspector
//!
//! Both panels are about the active pane — the stack walks its `draw_order`
//! and flips its layers, the inspector edits its product, kind and per-layer
//! configs — and several of the renderers they host need `&mut PaneState`
//! beside `&mut self`. So the pass holds the active pane out of the vector
//! with `std::mem::take`, **once, around both panels**: everything either
//! panel needs of the pane reads and writes the taken value, and nothing
//! inside the window may read `self.panes[..]`, whose active slot holds a
//! default placeholder until the restore. The menu model is built before any
//! take (by the top bar), the stack's row status lines are built from the
//! taken pane against a registry demonstrably loaded with its configs, and
//! [`Gui::write_pane_overlay`] is how the eye and Show toggles write both
//! halves without touching the vector. The restore is followed by
//! `propagate_layer_sync`, so a reorder, a toggle or an edited config fans
//! out to the synced panes the same frame.
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
//! after it. With the status bar, stack and inspector all floating `Area`s
//! (whose ids are their own roots, not children of the root `Ui`), the top bar
//! is the only thing that advances that counter at all, and it is drawn at
//! every width. `crossing_a_breakpoint_re_keys_nothing` pins the whole claim,
//! and `crossing_a_breakpoint_does_not_move_any_widget_id` pins the
//! stored-state half of it.

use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::OverlayKind;

use super::{InspectorSelection, PaneState};

/// Where a hosted surface goes this frame: the placement the caller decides
/// so the body renderers never key anything on the width.
///
/// Two callers build these — the shell's floating pass here, and the phone
/// sheet (`ui_sheet.rs`), which is exactly why the type exists: the stack and
/// inspector keep one `Area` id and one internal structure whoever is
/// positioning them, and the only thing that changes hands is this geometry.
pub(super) struct SurfaceSlot {
    /// Where the area's pivot corner goes.
    pub pos: egui::Pos2,
    pub pivot: egui::Align2,
    /// The surface's content width.
    pub width: f32,
    /// Space for the whole surface, header included — each renderer charges
    /// its own header allowance against it before capping its scroll body.
    pub avail_height: f32,
    /// Hosted inside the phone sheet: `Order::Foreground` (above the scrim)
    /// and frameless (the sheet's own frame is the background). The frame
    /// choice is id-neutral — `Frame::show` creates one child `Ui` either
    /// way — which is what keeps the breakpoint id contract intact across
    /// the host switch.
    pub sheet: bool,
    /// The opacity the host wants the surface painted at — `1.0` at rest,
    /// less during a fade or a host's own transition. Anything below `1.0`
    /// also disables the contents (`fade::dim`): a transitioning surface
    /// must never catch a click.
    pub opacity: f32,
    /// Whether the surface may interact at all this frame — `false` for a
    /// closing remnant the host is still animating out: its state says
    /// closed, so its widgets must already be dead whatever the opacity
    /// still shows.
    pub interactive: bool,
}

/// What the shell produced this frame.
pub(super) struct ShellOutput {
    pub actions: Vec<GuiAction>,
    /// Screen rects of floating chrome drawn *over* the map, which map click
    /// handling must not treat as map clicks.
    ///
    /// This is an **output** of the shell rather than something the map
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
    pub(super) fn render_shell(&mut self, ui: &mut egui::Ui) -> ShellOutput {
        let mut actions = Vec::new();

        self.render_top_bar(ui, &mut actions);

        // Everything the top bar did not claim is the map's — the full-bleed
        // rect every floating surface positions itself in.
        let map_rect = ui.available_rect_before_wrap();

        self.render_status_bar(ui.ctx(), map_rect, &mut actions);

        self.render_stack_and_inspector(ui.ctx(), map_rect, &mut actions);

        ShellOutput {
            actions,
            excluded_rects: Vec::new(),
            map_rect,
        }
    }

    /// The stack and the inspector, around the one take window — see the
    /// module note for the discipline this pass keeps.
    fn render_stack_and_inspector(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        actions: &mut Vec<GuiAction>,
    ) {
        // A Layer selection describes a map layer, and a pane with no map has
        // none — the stack shows it no rows to have selected one from. Snap
        // to the pane's own properties, which is what the inspector can still
        // truthfully say about it. Before the take, off the live pane — and
        // at every width: the sheet pass below the breakpoint relies on this
        // having run.
        if matches!(self.inspector_sel, InspectorSelection::Layer(_))
            && !self.panes[self.active_pane].is_map()
        {
            self.inspector_sel = InspectorSelection::PaneProps;
        }

        // Below the breakpoint the same flags present as sheet pages — the
        // sheet pass late in the frame hosts the same bodies through the same
        // take window (`ui_sheet.rs`); nothing floats at the map's corners.
        if self.layout.width == crate::ui_layout::WidthClass::Compact {
            return;
        }

        // The fade rule is total (§1.8): the fade closes both panels for
        // real, so this gate is moot in the steady state — it exists for the
        // fade-out transition, whose closing remnants dim with the rest of
        // the chrome, and as the stated rule should anything ever render
        // here while faded.
        let Some(fade) = self.chrome_fade() else {
            return;
        };

        // The slide animations (§3.3): each panel's open flag drives a
        // factor, and a closing panel renders as a non-interactive remnant
        // sliding off its own edge until the factor reaches zero. Under
        // `cfg(test)` the time is zero and the factors snap — see
        // `ui_fade::anim_time`.
        let stack_open = self.layers_panel_visible();
        let insp_open = self.insp_open;
        let stack_slide = ctx.animate_bool_with_time(
            egui::Id::new("stack_slide"),
            stack_open,
            super::fade::anim_time(),
        );
        let insp_slide = ctx.animate_bool_with_time(
            egui::Id::new("inspector_slide"),
            insp_open,
            super::fade::anim_time(),
        );
        if stack_slide <= 0.0 && insp_slide <= 0.0 {
            return;
        }

        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);

        // The registry must hold *this* pane's configs before anything asks a
        // handler about itself — the status lines below, and the layer body's
        // round trip, both read handler state as "the active pane's". The
        // frame-end reload in `Gui::ui` usually guarantees it already, but an
        // active-pane switch earlier this same frame (the top bar's segments)
        // would leave the previous pane's configs loaded.
        if !pane.overlay_configs.is_empty() {
            self.overlays.load_pane_configs(&pane.overlay_configs);
        }

        let statuses: Vec<(OverlayKind, Option<String>)> = if stack_slide > 0.0 {
            self.stack_row_statuses(&pane)
        } else {
            Vec::new()
        };

        if stack_slide > 0.0 {
            // Sliding out to the left: the whole panel's travel is its width
            // plus both insets, so at factor zero nothing of it remains on
            // the map.
            let travel =
                (1.0 - stack_slide) * (super::ui_stack::STACK_WIDTH + 2.0 * super::ui_stack::STACK_INSET);
            let slot = SurfaceSlot {
                pos: map_rect.left_top()
                    + egui::vec2(super::ui_stack::STACK_INSET - travel, super::ui_stack::STACK_INSET),
                pivot: egui::Align2::LEFT_TOP,
                width: super::ui_stack::STACK_WIDTH,
                avail_height: map_rect.height()
                    - super::ui_stack::STACK_INSET
                    - super::ui_stack::STACK_BOTTOM_CLEARANCE,
                sheet: false,
                opacity: fade,
                interactive: stack_open,
            };
            self.render_stack(ctx, slot, &mut pane, &statuses, actions);
        }
        if insp_slide > 0.0 {
            let travel = (1.0 - insp_slide)
                * (super::ui_inspector::INSPECTOR_WIDTH + 2.0 * super::ui_inspector::INSPECTOR_INSET);
            let slot = SurfaceSlot {
                pos: map_rect.right_top()
                    + egui::vec2(
                        -super::ui_inspector::INSPECTOR_INSET + travel,
                        super::ui_inspector::INSPECTOR_INSET,
                    ),
                pivot: egui::Align2::RIGHT_TOP,
                width: super::ui_inspector::INSPECTOR_WIDTH,
                avail_height: map_rect.height()
                    - super::ui_inspector::INSPECTOR_INSET
                    - super::ui_inspector::INSPECTOR_BOTTOM_CLEARANCE,
                sheet: false,
                opacity: fade,
                interactive: insp_open,
            };
            self.render_inspector(ctx, slot, &mut pane, actions);
        }

        self.panes[self.active_pane] = pane;
        // After the restore, so the source it copies from is the real pane
        // rather than the `mem::take` placeholder. It deliberately does
        // **not** copy `content`: a pane's kind is how this pane presents the
        // shared subject, not part of the subject, and propagating it would
        // convert every sibling the moment one pane became a 3D view — from a
        // setting called "Sync Layers". The reasoning is written out on
        // `propagate_layer_sync` itself.
        self.propagate_layer_sync();
    }

    /// The stack rows' status lines, one per layer in the pane's own order —
    /// empty for a pane with no map, which has no rows to carry them.
    ///
    /// Radar's is the exception with a reason: the product and tilt are pane
    /// state — the radar handler holds only the layer toggle — so the line
    /// is read off the taken pane rather than asked of the registry. The
    /// caller must have loaded the pane's configs into the registry first;
    /// both the shell pass above and the sheet pass do.
    pub(super) fn stack_row_statuses(
        &self,
        pane: &PaneState,
    ) -> Vec<(OverlayKind, Option<String>)> {
        if !pane.is_map() {
            return Vec::new();
        }
        pane.draw_order
            .iter()
            .map(|&kind| {
                let line = if kind == OverlayKind::Radar {
                    radar_row_status(pane)
                } else {
                    self.overlays.status_line(kind)
                };
                (kind, line)
            })
            .collect()
    }
}

/// The Radar row's status line: what picture this pane's radar layer is —
/// product code and tilt, e.g. `REF · 0.5°`.
///
/// The tilt is the *snapped* angle where a scan is loaded — the one the pane
/// is actually rendering — falling back to the raw selection before any scan
/// arrives. `None` while the layer is hidden: the dimmed row carries no line,
/// like every other layer's. `code()` is the fetch path's lowercase spelling,
/// uppercased here because the row is a display, not a URL.
fn radar_row_status(pane: &PaneState) -> Option<String> {
    if !pane.is_overlay_enabled(OverlayKind::Radar) {
        return None;
    }
    let (product, tilt) = pane
        .get_rendering_params()
        .unwrap_or((pane.selected_product, pane.selected_elevation));
    Some(format!(
        "{} \u{b7} {tilt:.1}\u{b0}",
        product.code().to_uppercase()
    ))
}
