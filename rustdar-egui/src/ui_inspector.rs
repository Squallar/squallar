//! The inspector: one panel, three bodies — a layer's options, the active
//! pane's properties, or the app's settings.
//!
//! One body set for every shell. On Expanded it floats at the map's top-right,
//! mirroring the stack on the left; below the sidebar breakpoint the same
//! panel is a right slide-over. Closed by default at every width — the top
//! bar's ⚙ toggle, a stack row click and the menu's Settings… entry are the
//! ways in, and `dismiss_top_layer` is the keyboard's way out.
//!
//! The crumb row names what the body is about — `Pane 2 › NWS Alerts`,
//! `Pane 2 › Properties`, `App › Settings` — and the body arm is dispatched on
//! [`InspectorSelection`]. Each arm writes its own literal into the probe's
//! `mode`, the `PaneContentProbe` pattern: a mis-wired arm cannot fake having
//! run.
//!
//! # One body per selection, one id scope per body
//!
//! The three bodies allocate very different widget counts, and they share one
//! `Ui`. Each body renders inside its own explicit-id scope, so switching the
//! selection cannot re-key the widgets of the body being switched *to* — the
//! same reasoning as the kind scope inside the Pane-properties body, written
//! out on [`render_pane_props_body`](super::Gui::render_pane_props_body).
//!
//! The panel is sized from the map explicitly per frame, not via
//! `Area::default_size` — the same §5.9 fix as the stack; see `ui_stack.rs`'s
//! module note for the mechanism.

use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::OverlayKind;

use super::{InspectorSelection, PaneState, map};

/// Width of the inspector, in both its floating and slide-over forms — one
/// value for the same one-id reason as [`super::ui_stack::STACK_WIDTH`].
const INSPECTOR_WIDTH: f32 = 300.0;

/// The inspector's inset from the map's top-right corner.
const INSPECTOR_INSET: f32 = 8.0;

/// What the inspector leaves clear above the map's bottom edge — the same
/// band the stack leaves (plan §1.4: same vertical insets).
const INSPECTOR_BOTTOM_CLEARANCE: f32 = 88.0;

/// What the crumb row and its separator cost above the scroll body.
const HEADER_ALLOWANCE: f32 = 40.0;

/// The collapse button's glyph: the panel slides out to the right.
const COLLAPSE_LABEL: &str = "\u{27e9}";

/// The deselect button's glyph — back to App › Settings.
const DESELECT_LABEL: &str = "\u{2715}";

/// Width of combo boxes inside the inspector — the layers panel's old value,
/// kept with the `layers_` salts so the combos' stored state moved intact.
const COMBO_BOX_WIDTH: f32 = 150.0;

/// Id prefix for the product/tilt combos. Deliberately the prefix the layers
/// panel always used: the widget state egui keyed on `layers_product_sel`
/// belongs to the control, not to which panel is hosting it.
const LAYER_CONTROL_ID_PREFIX: &str = "layers_";

/// What the inspector drew last frame, as it was drawn.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InspectorProbe {
    /// The floating area's whole rect, off its own response.
    pub rect: egui::Rect,
    /// The crumb row's text, e.g. `Pane 2 › Properties`.
    pub crumb: String,
    /// The `✕` deselect button — [`egui::Rect::NOTHING`] on the App ›
    /// Settings body, which has nothing to deselect.
    pub deselect: egui::Rect,
    /// The `⟩` collapse button.
    pub collapse: egui::Rect,
    /// Whether the inspector was on screen this frame.
    pub open: bool,
    /// Which body arm actually drew, written by that arm as a literal — the
    /// [`super::PaneContentProbe`] pattern, so a mis-wired arm cannot fake it.
    pub mode: Option<InspectorSelection>,
    /// The layer body's "Show <layer>" master toggle, and the state it was
    /// drawn showing.
    pub master: Option<(egui::Rect, bool)>,
}

#[cfg(test)]
impl Default for InspectorProbe {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            crumb: String::new(),
            deselect: egui::Rect::NOTHING,
            collapse: egui::Rect::NOTHING,
            open: false,
            mode: None,
            master: None,
        }
    }
}

impl super::Gui {
    /// The inspector, floating at the map's top-right.
    ///
    /// `pane` is the active pane, `mem::take`n by the caller for the whole
    /// stack+inspector pass — nothing in here reads `self.panes[..]`.
    pub(super) fn render_inspector(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
        let max_body_height = (map_rect.height()
            - INSPECTOR_INSET
            - INSPECTOR_BOTTOM_CLEARANCE
            - HEADER_ALLOWANCE)
            .max(0.0);

        #[cfg(test)]
        let mut probe = InspectorProbe {
            open: true,
            ..InspectorProbe::default()
        };

        let area = egui::Area::new(egui::Id::new("inspector_panel"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(map_rect.right_top() + egui::vec2(-INSPECTOR_INSET, INSPECTOR_INSET))
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.global_style()).show(ui, |ui| {
                    ui.set_width(INSPECTOR_WIDTH);
                    self.render_inspector_crumb(
                        ui,
                        #[cfg(test)]
                        &mut probe,
                    );
                    ui.separator();

                    let mut body = egui::ScrollArea::vertical()
                        .id_salt("inspector_scroll")
                        .max_height(max_body_height)
                        .min_scrolled_height(max_body_height);
                    // A selection change starts its body at the top: the
                    // offset is the panel's memory, and a deep settings
                    // scroll carried into a fresh layer's options would open
                    // them somewhere in the middle.
                    if std::mem::take(&mut self.insp_scroll_reset) {
                        body = body.vertical_scroll_offset(0.0);
                    }
                    let scroll = body.show(ui, |ui| {
                            // One explicit id scope per body — see the module
                            // note. `UiBuilder::id` rather than `id_salt` for
                            // the reason the status bar's `status_error` scope
                            // records: the explicit form takes the parent's
                            // auto-id counter out of the children entirely.
                            let scope = egui::UiBuilder::new()
                                .id(ui.id().with(match self.inspector_sel {
                                    InspectorSelection::AppSettings => "body_settings",
                                    InspectorSelection::PaneProps => "body_pane",
                                    InspectorSelection::Layer(_) => "body_layer",
                                }))
                                .layout(egui::Layout::top_down_justified(egui::Align::LEFT));
                            ui.scope_builder(scope, |ui| match self.inspector_sel {
                                InspectorSelection::AppSettings => {
                                    #[cfg(test)]
                                    {
                                        probe.mode = Some(InspectorSelection::AppSettings);
                                    }
                                    self.render_settings_body(ui, actions);
                                }
                                InspectorSelection::PaneProps => {
                                    #[cfg(test)]
                                    {
                                        probe.mode = Some(InspectorSelection::PaneProps);
                                    }
                                    self.render_pane_props_body(ui, pane);
                                }
                                InspectorSelection::Layer(kind) => {
                                    #[cfg(test)]
                                    {
                                        probe.mode = Some(InspectorSelection::Layer(kind));
                                    }
                                    self.render_layer_body(
                                        ui,
                                        pane,
                                        kind,
                                        actions,
                                        #[cfg(test)]
                                        &mut probe,
                                    );
                                }
                            });
                        });

                    #[cfg(test)]
                    self.widget_id_probes.push(("inspector_scroll", scroll.id));
                    #[cfg(not(test))]
                    let _ = scroll;
                });
            });

        #[cfg(test)]
        {
            probe.rect = area.response.rect;
            self.last_inspector = probe;
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The crumb row: where the body's subject is named, and where it is
    /// changed.
    fn render_inspector_crumb(
        &mut self,
        ui: &mut egui::Ui,
        #[cfg(test)] probe: &mut InspectorProbe,
    ) {
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let collapse = ui
                    .button(COLLAPSE_LABEL)
                    .on_hover_text("Collapse the inspector");
                #[cfg(test)]
                {
                    probe.collapse = collapse.rect;
                }
                if collapse.clicked() {
                    // A collapse is not a deselection: the selection stays,
                    // so reopening returns to it. Escape resets — see
                    // `dismiss_top_layer`.
                    self.insp_open = false;
                }

                // Nothing to deselect on App › Settings — it is what
                // deselecting returns to.
                if self.inspector_sel != InspectorSelection::AppSettings {
                    let deselect = ui
                        .button(DESELECT_LABEL)
                        .on_hover_text("Back to App \u{203a} Settings");
                    #[cfg(test)]
                    {
                        probe.deselect = deselect.rect;
                    }
                    if deselect.clicked() {
                        self.inspector_sel = InspectorSelection::AppSettings;
                        self.insp_scroll_reset = true;
                    }
                }

                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let pane_label = format!("Pane {}", self.active_pane + 1);
                    let tail: String = match self.inspector_sel {
                        InspectorSelection::AppSettings => {
                            ui.label(egui::RichText::new("App").strong());
                            ui.label("\u{203a}");
                            ui.label("Settings");
                            "Settings".to_owned()
                        }
                        InspectorSelection::PaneProps => {
                            let _ = ui.selectable_label(
                                true,
                                egui::RichText::new(pane_label.as_str()).strong(),
                            );
                            ui.label("\u{203a}");
                            ui.label("Properties");
                            "Properties".to_owned()
                        }
                        InspectorSelection::Layer(kind) => {
                            // The `Pane N` segment is the interim route to
                            // Pane properties — M5's pills take this over.
                            let seg = ui
                                .selectable_label(
                                    false,
                                    egui::RichText::new(pane_label.as_str()).strong(),
                                )
                                .on_hover_text("This pane's properties");
                            if seg.clicked() {
                                self.select_pane_props();
                            }
                            ui.label("\u{203a}");
                            let name = self.overlays.display_name(kind).to_owned();
                            ui.add(egui::Label::new(name.as_str()).truncate());
                            name
                        }
                    };
                    #[cfg(test)]
                    {
                        probe.crumb = match self.inspector_sel {
                            InspectorSelection::AppSettings => {
                                "App \u{203a} Settings".to_owned()
                            }
                            _ => format!("{pane_label} \u{203a} {tail}"),
                        };
                    }
                    #[cfg(not(test))]
                    let _ = tail;
                });
            });
        });
    }

    /// The layer body: the master "Show <layer>" toggle, then the handler's
    /// own controls through the one host they have.
    fn render_layer_body(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        kind: OverlayKind,
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut InspectorProbe,
    ) {
        let name = self.overlays.display_name(kind).to_owned();
        let mut on = pane.is_overlay_enabled(kind);
        #[cfg(test)]
        let was_on = on;
        let master = ui.checkbox(&mut on, format!("Show {name}"));
        #[cfg(test)]
        {
            // The state the checkbox was *handed*, not the one the click
            // produced — the same discipline as `DrawnMenuLeaf`.
            probe.master = Some((master.rect, was_on));
        }
        if master.changed() {
            // Both halves, on the taken pane — see `write_pane_overlay`.
            Self::write_pane_overlay(&mut self.overlays, pane, kind, on);
            // The same enable-fetch rule as the stack's eye.
            if on && !self.overlays.has_data(kind) && !self.overlays.is_fetching(kind) {
                actions.push(GuiAction::FetchOverlay {
                    kind,
                    pane_idx: self.active_pane,
                });
            }
        }
        ui.add_space(4.0);
        ui.separator();

        self.render_overlay_controls_one(ui, pane, kind, actions);
    }

    /// The Pane-properties body: what the pane is, what it shows, and how it
    /// runs with its siblings.
    ///
    /// # The kind-specific block goes in one child scope
    ///
    /// The block at the bottom depends on what the pane *is* — a volume pane
    /// draws the 3D knobs, a section pane its A–B readout, a map pane nothing
    /// — so it sits inside a single `scope_builder` with an explicit
    /// [`egui::UiBuilder::id`]. `Ui::new_child` folds the parent's
    /// `next_auto_id_salt` into every child's registered id, so whatever this
    /// body ever grows below the block would come back under new ids the
    /// moment a pane was converted; the explicit id takes this scope's
    /// children out of that entirely. Defence, not a fix for a live
    /// difference — the block is currently last — on the same terms the old
    /// layers panel recorded at length.
    fn render_pane_props_body(&mut self, ui: &mut egui::Ui, pane: &mut PaneState) {
        super::render_pane_identity(ui, pane);
        ui.add_space(4.0);

        // The kind segmented control — the same three targets the ☰ menu and
        // the armed drags reach, in one row. Through `request_pane_kind`,
        // **not** `set_kind`: this runs inside the shell's take window, where
        // a direct write lands on the placeholder in the vector and is
        // silently discarded (see `pending_pane_kind`).
        let current = pane.kind();
        ui.horizontal(|ui| {
            for (kind, label) in [
                (crate::pane::PaneKind::Map, "Map"),
                (crate::pane::PaneKind::Volume, "3D Volume"),
                (crate::pane::PaneKind::CrossSection, "Cross-section"),
            ] {
                let selected = current == kind;
                if ui.selectable_label(selected, label).clicked() && !selected {
                    self.request_pane_kind(self.active_pane, kind);
                    // A section pane with no line is a pane waiting to be
                    // aimed, so choosing the kind arms the draw — the same
                    // gesture the menu's "Draw cross-section" entry arms,
                    // saving the trip back to the menu.
                    if kind == crate::pane::PaneKind::CrossSection
                        && pane.cross_section().and_then(|s| s.line).is_none()
                    {
                        self.set_section_draw_armed(true);
                    }
                }
            }
        });
        ui.add_space(4.0);

        self.render_radar_controls(ui, pane, COMBO_BOX_WIDTH, LAYER_CONTROL_ID_PREFIX);

        // --- Sync ---
        //
        // Both settings are properties of the *layout* rather than of the
        // active pane, and they stay meaningful with a non-map pane on
        // screen — `sync_layers` still converges site, product and time
        // across every pane, and `sync_viewports` still holds the map panes
        // together while leaving this one alone. Only offered when there is
        // more than one pane to hold together.
        if self.pane_layout.pane_count > 1 {
            ui.add_space(6.0);
            ui.separator();
            ui.checkbox(&mut self.viewport_sync, "\u{1f517}  Sync Viewports");
            ui.checkbox(&mut self.sync_layers, "\u{1f517}  Sync Layers");
        }

        // The kind-specific block, last, in its one scope — see the method
        // note.
        let kind_scope = egui::UiBuilder::new().id(ui.id().with("pane_kind_controls"));
        ui.scope_builder(kind_scope, |ui| match pane.kind() {
            crate::pane::PaneKind::Map => {}
            crate::pane::PaneKind::CrossSection => {
                self.render_section_controls(ui, pane);
            }
            crate::pane::PaneKind::Volume => {
                map::render_volume_controls(ui, pane, &mut self.volume_iso, &self.volume_alpha);
            }
        });
    }
}
