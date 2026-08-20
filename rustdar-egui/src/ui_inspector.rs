//! The inspector: one panel, three bodies — a layer's options, the active pane's
//! properties, or the app's settings.

use crate::actions::GuiAction;
use rustdar_source::id::{LayerId, known};

use super::shell::SurfaceSlot;
use super::{InspectorSelection, PaneState, map};

/// Width of the inspector, in both its floating and slide-over forms — one value
/// for the same one-id reason as [`super::ui_stack::STACK_WIDTH`].
pub(super) const INSPECTOR_WIDTH: f32 = 300.0;

/// The inspector's inset from the map's top-right corner.
pub(super) const INSPECTOR_INSET: f32 = 8.0;

/// What the inspector leaves clear above the map's bottom edge — the same band the
/// stack leaves (plan §1.4: same vertical insets).
pub(super) const INSPECTOR_BOTTOM_CLEARANCE: f32 = 88.0;

/// What the crumb row and its separator cost above the scroll body.
const HEADER_ALLOWANCE: f32 = 40.0;

/// The collapse button's glyph: the panel slides out to the right.
const COLLAPSE_LABEL: &str = "\u{203a}";

/// The deselect button's glyph — back to App › Settings.
const DESELECT_LABEL: &str = "\u{d7}";

/// Width of combo boxes inside the inspector — the layers panel's old value, kept
/// with the `layers_` salts so the combos' stored state moved intact.
const COMBO_BOX_WIDTH: f32 = 150.0;

/// Id prefix for the product/tilt combos.
const LAYER_CONTROL_ID_PREFIX: &str = "layers_";

/// What the inspector drew last frame, as it was drawn.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InspectorProbe {
    /// The floating area's whole rect, off its own response.
    pub rect: egui::Rect,
    /// The crumb row's text, e.g. `Pane 2 › Properties`.
    pub crumb: String,
    /// The `×` deselect button — [`egui::Rect::NOTHING`] on the App › Settings
    /// body, which has nothing to deselect.
    pub deselect: egui::Rect,
    /// The `›` collapse button.
    pub collapse: egui::Rect,
    /// Whether the inspector was on screen this frame.
    pub open: bool,
    /// Which body arm actually drew, written by that arm as a literal — the
    /// [`super::PaneContentProbe`] pattern, so a mis-wired arm cannot fake it.
    pub mode: Option<InspectorSelection>,
    /// The pane-props body's site search field.
    pub site_search: egui::Rect,
    /// The site rows the pane-props body drew, filtered as drawn: the site code,
    /// where the row landed, and whether it was highlighted as the pane's current
    /// site.
    pub site_rows: Vec<(String, egui::Rect, bool)>,
    /// The site list's count caption, verbatim.
    pub site_caption: String,
    /// The sync section's rows as drawn — the three link checkboxes with the state
    /// they were handed and the two action rows with `false`
    /// (`pills::sync_section_ui`'s outcome, verbatim).
    pub sync_rows: Vec<(String, egui::Rect, bool)>,
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
            site_search: egui::Rect::NOTHING,
            site_rows: Vec::new(),
            site_caption: String::new(),
            sync_rows: Vec::new(),
        }
    }
}

impl super::Gui {
    /// The inspector, in the slot its host chose — the map's top-right corner from
    /// the shell, the sheet's body from the phone shell.
    pub(super) fn render_inspector(
        &mut self,
        ctx: &egui::Context,
        slot: SurfaceSlot,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
        let max_body_height = (slot.avail_height - HEADER_ALLOWANCE).max(0.0);

        #[cfg(test)]
        let mut probe = InspectorProbe {
            open: true,
            ..InspectorProbe::default()
        };

        let frame = if slot.sheet {
            egui::Frame::NONE
        } else {
            super::shell::chrome_frame(&ctx.global_style())
        };
        let order = if slot.sheet {
            egui::Order::Foreground
        } else {
            egui::Order::Middle
        };
        let area = egui::Area::new(egui::Id::new("inspector_panel"))
            .order(order)
            .pivot(slot.pivot)
            .fixed_pos(slot.pos)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    super::fade::dim(ui, slot.opacity);
                    if !slot.interactive {
                        ui.disable();
                    }
                    ui.set_width(slot.width);
                    self.render_inspector_crumb(
                        ui,
                        slot.sheet,
                        #[cfg(test)]
                        &mut probe,
                    );
                    ui.separator();

                    let mut body = egui::ScrollArea::vertical()
                        .scroll_source(super::shell::panel_scroll_source())
                        .id_salt("inspector_scroll")
                        .max_height(max_body_height)
                        .min_scrolled_height(max_body_height);
                    if std::mem::take(&mut self.insp_scroll_reset) {
                        body = body.vertical_scroll_offset(0.0);
                    }
                    let scroll = body.show(ui, |ui| {
                        let scope = egui::UiBuilder::new()
                            .id(ui.id().with(match self.inspector_sel {
                                InspectorSelection::AppSettings => "body_settings",
                                InspectorSelection::PaneProps => "body_pane",
                                InspectorSelection::Layer(_) => "body_layer",
                            }))
                            .layout(egui::Layout::top_down_justified(egui::Align::LEFT));
                        ui.scope_builder(scope, |ui| match self.inspector_sel.clone() {
                            InspectorSelection::AppSettings => {
                                #[cfg(test)]
                                {
                                    probe.mode = Some(InspectorSelection::AppSettings);
                                }
                                self.render_settings_body(ui, pane, actions);
                            }
                            InspectorSelection::PaneProps => {
                                #[cfg(test)]
                                {
                                    probe.mode = Some(InspectorSelection::PaneProps);
                                }
                                self.render_pane_props_body(
                                    ui,
                                    pane,
                                    actions,
                                    #[cfg(test)]
                                    &mut probe,
                                );
                            }
                            InspectorSelection::Layer(kind) => {
                                #[cfg(test)]
                                {
                                    probe.mode = Some(InspectorSelection::Layer(kind.clone()));
                                }
                                self.render_layer_body(ui, pane, &kind, actions);
                            }
                        });
                    });

                    #[cfg(test)]
                    self.probes
                        .widget_id_probes
                        .push(("inspector_scroll", scroll.id));
                    #[cfg(not(test))]
                    let _ = scroll;
                });
            });

        #[cfg(test)]
        {
            probe.rect = area.response.rect;
            self.probes.last_inspector = probe;
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The crumb row: where the body's subject is named, and where it is changed.
    fn render_inspector_crumb(
        &mut self,
        ui: &mut egui::Ui,
        sheet: bool,
        #[cfg(test)] probe: &mut InspectorProbe,
    ) {
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !sheet {
                    let collapse = ui
                        .button(COLLAPSE_LABEL)
                        .on_hover_text("Collapse the inspector");
                    #[cfg(test)]
                    {
                        probe.collapse = collapse.rect;
                    }
                    if collapse.clicked() {
                        self.insp_open = false;
                    }
                }

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
                    let tail: String = match self.inspector_sel.clone() {
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
                            let name = self.overlays.display_name(&kind).to_owned();
                            ui.add(egui::Label::new(name.as_str()).truncate());
                            name
                        }
                    };
                    #[cfg(test)]
                    {
                        probe.crumb = match self.inspector_sel {
                            InspectorSelection::AppSettings => "App \u{203a} Settings".to_owned(),
                            _ => format!("{pane_label} \u{203a} {tail}"),
                        };
                    }
                    #[cfg(not(test))]
                    let _ = tail;
                });
            });
        });
    }

    /// The layer body: the handler's own controls, through the one host they have.
    fn render_layer_body(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        kind: &LayerId,
        actions: &mut Vec<GuiAction>,
    ) {
        if *kind == known::RADAR {
            if let Some(status) = super::shell::radar_row_status(pane) {
                ui.label(status);
                ui.add_space(4.0);
            }
            ui.label(
                egui::RichText::new(
                    "Product and tilt are pane properties - set them from the \
                     pane's pills or in Pane properties.",
                )
                .small()
                .weak(),
            );
            ui.add_space(6.0);
            if ui.button("Pane properties...").clicked() {
                self.select_pane_props();
            }
            return;
        }

        self.render_overlay_controls_one(ui, pane, kind, actions);
    }

    /// The Pane-properties body: what the pane is, what it shows, and how it runs
    /// with its siblings.
    fn render_pane_props_body(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut InspectorProbe,
    ) {
        super::render_pane_identity(ui, pane);
        ui.add_space(4.0);

        let current = pane.render_view();
        let picked = ui
            .horizontal(|ui| super::pills::kind_list_ui(ui, current).picked)
            .inner;
        if let Some(view) = picked {
            let line_absent = pane.cross_section().and_then(|s| s.line).is_none();
            self.pick_pane_kind(self.active_pane, view, line_absent);
        }
        ui.add_space(4.0);

        self.render_site_search(
            ui,
            pane,
            actions,
            #[cfg(test)]
            probe,
        );
        ui.add_space(4.0);

        self.render_radar_controls(ui, pane, COMBO_BOX_WIDTH, LAYER_CONTROL_ID_PREFIX);

        if self.pane_layout.pane_count > 1 {
            ui.add_space(6.0);
            ui.separator();
            let outcome = super::pills::sync_section_ui(ui, pane);
            #[cfg(test)]
            {
                probe.sync_rows = outcome.rows.clone();
            }
            self.apply_sync_outcome(&outcome, pane, self.active_pane);
        }

        let kind_scope = egui::UiBuilder::new().id(ui.id().with("pane_kind_controls"));
        ui.scope_builder(kind_scope, |ui| match pane.render_view() {
            rustdar_radar::types::RenderView::PlanView => {}
            rustdar_radar::types::RenderView::CrossSection => {
                self.render_section_controls(ui, pane);
            }
            rustdar_radar::types::RenderView::Volume => {
                let drawing_nothing = self.volume_empty_states.get(&self.active_pane).cloned();
                map::render_volume_controls(
                    ui,
                    pane,
                    &mut self.volume_iso,
                    &self.volume_alpha,
                    drawing_nothing.as_deref(),
                );
            }
        });
    }

    /// The radar-site search: a filter box over the full compiled-in table and a
    /// scrolling list of what survives it, the pane's current site highlighted
    /// (plan §1.4 — the first *list* route to a site; the map's clickable icons
    /// were the only picker before this).
    fn render_site_search(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut InspectorProbe,
    ) {
        let scope = egui::UiBuilder::new().id(ui.id().with("site_search"));
        ui.scope_builder(scope, |ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.site_query)
                    .id_salt("site_query")
                    .hint_text("Search radar sites"),
            );
            #[cfg(test)]
            {
                probe.site_search = search.rect;
            }
            #[cfg(not(test))]
            let _ = search;

            let outcome = super::pills::site_list_ui(
                ui,
                &self.site_query,
                &pane.site,
                self.catalogue_pending,
            );
            #[cfg(test)]
            {
                probe.site_caption = outcome.caption.clone();
                probe.site_rows = outcome.rows.clone();
            }
            if let Some(picked) = outcome.picked {
                pane.loading_site = Some(picked.clone());
                pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
                actions.push(GuiAction::SwitchRadarSite {
                    site: picked,
                    pane_idx: self.active_pane,
                });
            }
        });
    }
}
