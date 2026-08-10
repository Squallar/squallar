//! The inspector: one panel, three bodies — a layer's options, the active
//! pane's properties, or the app's settings.
//!
//! One body set for every shell. On Expanded it floats at the map's top-right,
//! mirroring the stack on the left; on Medium the same panel is a right
//! slide-over; on Compact it is the phone sheet's Inspector page, in the slot
//! `ui_sheet.rs` hands over. Closed by default at every width — the top
//! bar's ⚙ toggle where the bar carries one, the bottom bar's Pane and App
//! items on the phone, a stack row click and the menu's Settings… entry are
//! the ways in, and `dismiss_top_layer` is the keyboard's way out.
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

use super::shell::SurfaceSlot;
use super::{InspectorSelection, PaneState, map};

/// Width of the inspector, in both its floating and slide-over forms — one
/// value for the same one-id reason as [`super::ui_stack::STACK_WIDTH`].
pub(super) const INSPECTOR_WIDTH: f32 = 300.0;

/// The inspector's inset from the map's top-right corner.
pub(super) const INSPECTOR_INSET: f32 = 8.0;

/// What the inspector leaves clear above the map's bottom edge — the same
/// band the stack leaves (plan §1.4: same vertical insets).
pub(super) const INSPECTOR_BOTTOM_CLEARANCE: f32 = 88.0;

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
    /// The pane-props body's site search field.
    pub site_search: egui::Rect,
    /// The site rows the pane-props body drew, filtered as drawn: the site
    /// code, where the row landed, and whether it was highlighted as the
    /// pane's current site.
    pub site_rows: Vec<(String, egui::Rect, bool)>,
    /// The site list's count caption, verbatim.
    pub site_caption: String,
    /// The sync section's "Follows shared time" checkbox and the state it was
    /// drawn showing — `None` on a single-pane layout, which has no shared
    /// time to follow.
    pub time_link: Option<(egui::Rect, bool)>,
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
            site_search: egui::Rect::NOTHING,
            site_rows: Vec::new(),
            site_caption: String::new(),
            time_link: None,
        }
    }
}

impl super::Gui {
    /// The inspector, in the slot its host chose — the map's top-right
    /// corner from the shell, the sheet's body from the phone shell.
    ///
    /// `pane` is the active pane, `mem::take`n by the caller for the whole
    /// stack+inspector pass — nothing in here reads `self.panes[..]`.
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

        // Same swap as the stack's, for the same id reason — see
        // `SurfaceSlot`.
        let frame = if slot.sheet {
            egui::Frame::NONE
        } else {
            egui::Frame::window(&ctx.global_style())
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
                    ui.set_width(slot.width);
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
                            // The `Pane N` segment opens Pane properties.
                            // The pills are the primary route to the pane's
                            // properties now; this stays as the in-crumb way
                            // to the body that shows them all at once.
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
            // Both halves on the taken pane, plus the enable-fetch rule —
            // the one helper this toggle, the stack's eye and the catalog's
            // tiles share.
            let idx = self.active_pane;
            self.set_pane_overlay_with_fetch(pane, idx, kind, on, actions);
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
    fn render_pane_props_body(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut InspectorProbe,
    ) {
        super::render_pane_identity(ui, pane);
        ui.add_space(4.0);

        // The kind segmented control — the same three targets the ☰ menu and
        // the armed drags reach, in one row. The shared picker body
        // (`ui_pills::kind_list_ui`) and the shared chooser
        // (`Gui::pick_pane_kind`) are the kind pill popover's own, so the
        // two routes offer and mean the same things by construction. Through
        // `request_pane_kind` inside the chooser, **not** `set_kind`: this
        // runs inside the shell's take window, where a direct write lands on
        // the placeholder in the vector and is silently discarded (see
        // `pending_pane_kind`).
        let current = pane.kind();
        let picked = ui
            .horizontal(|ui| super::pills::kind_list_ui(ui, current).picked)
            .inner;
        if let Some(kind) = picked {
            let line_absent = pane.cross_section().and_then(|s| s.line).is_none();
            self.pick_pane_kind(self.active_pane, kind, line_absent);
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
            // Per-pane, unlike the two layout-wide toggles above: off means
            // this pane is *frozen* — left out of the loop fan-out and of
            // `propagate_layer_sync`'s time pair. Written on the taken pane,
            // where every write in this body lands.
            #[cfg(test)]
            let link_was = pane.time_link;
            let link = ui
                .checkbox(&mut pane.time_link, "\u{1f517}  Follows shared time")
                // The pill popover's own sentence, shared so the two routes
                // cannot describe unlinking differently — and careful about
                // "frozen"; see `ui_pills::UNLINK_NOTE`.
                .on_hover_text(super::pills::UNLINK_NOTE);
            #[cfg(test)]
            {
                // The state the checkbox was handed, not the one the click
                // produced — the `DrawnMenuLeaf` discipline.
                probe.time_link = Some((link.rect, link_was));
            }
            #[cfg(not(test))]
            let _ = link;
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

    /// The radar-site search: a filter box over the full compiled-in table
    /// and a scrolling list of what survives it, the pane's current site
    /// highlighted (plan §1.4 — the first *list* route to a site; the map's
    /// clickable icons were the only picker before this).
    ///
    /// The list itself is the shared [`super::pills::site_list_ui`] — the
    /// site pill popover renders the same function over the same
    /// `site_query`, so the two routes are one inventory by construction. A
    /// row click emits the same [`GuiAction::SwitchRadarSite`] the map icon
    /// emits, with the same in-flight marker on the pane, so the routes
    /// cannot mean different things either.
    fn render_site_search(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut InspectorProbe,
    ) {
        // One explicit-id child scope for the whole block: the row count
        // below follows the filter, so without it every widget drawn after
        // this block would re-key each time the query changed — the same
        // device, for the same reason, as the kind scope at the body's
        // bottom.
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

            let outcome = super::pills::site_list_ui(ui, &self.site_query, &pane.site);
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
