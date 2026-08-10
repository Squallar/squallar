//! The layer stack: one row per layer of the active pane, in draw order.
//!
//! One body for every shell. On Expanded it floats at the map's top-left,
//! open by default; on Medium the same body is the slide-over drawer, closed
//! until the top bar's Layers toggle opens it; on Compact it is the phone
//! sheet's Layers page, in the slot `ui_sheet.rs` hands over —
//! `Gui::layers_panel_visible` is the one definition of "open", exactly as it
//! was for the panel this replaces. The area and scroll ids are the old
//! panel's (`layers_panel`, `layers_scroll`) on purpose, at every one of the
//! three hosts: egui's memory of the surface — its scroll offset above all —
//! belongs to *the place the layers live*, not to which milestone's renderer
//! or which width's host is drawing it.
//!
//! The rows walk the active pane's `draw_order` **reversed**, so the top row
//! is drawn last — over everything — which is the reading the header's
//! tooltip teaches. The ⏶⏷ buttons swap neighbours in that same `draw_order`,
//! which closes a long-standing gap: the order has been persisted per pane
//! since multi-pane landed, and this is the first UI that can change it.
//!
//! # The panel is sized from the map, every frame
//!
//! Not `Area::default_size`: egui applies that only while the stored
//! `AreaState` size is `None`, so after frame 1 the committed size becomes
//! the sizing-pass ceiling — and a `ScrollArea` fills exactly what it is
//! offered, so a shrink-then-grow of the window left the old panel stuck at
//! its smallest-ever height (the §5.9 carried finding). The ceiling here is
//! explicit per frame instead: the scroll body's `max_height` *and*
//! `min_scrolled_height` are both the height the map currently affords, so
//! an overflowing list is exactly that tall whatever stale size the area
//! state remembers, and a short list still shrink-wraps.

use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::OverlayKind;

use super::shell::SurfaceSlot;
use super::{InspectorSelection, PaneState};

/// Width of the stack, in both its sidebar and drawer forms.
///
/// One value, not two, because the panel keeps one egui id: a per-form width
/// would make it jump when the window crossed the breakpoint, for no reason a
/// user could see. (The phone sheet's Layers page is the exception with a
/// reason: its slot is the sheet's own width, and the sheet is a different
/// surface, not this panel at a third width.)
pub(super) const STACK_WIDTH: f32 = 240.0;

/// The stack's inset from the map's top-left corner.
pub(super) const STACK_INSET: f32 = 8.0;

/// What the stack leaves clear above the map's bottom edge: room for the
/// status bar and the timeline transport floating there (plan §1.3).
pub(super) const STACK_BOTTOM_CLEARANCE: f32 = 88.0;

/// What the header row and its separator cost above the scroll body — charged
/// against the body's ceiling so the whole panel, header included, stays out
/// of the bottom clearance band.
const HEADER_ALLOWANCE: f32 = 40.0;

/// The collapse button's glyph: the panel slides out to the left. `‹` rather
/// than the demo's `⟨`, which egui's bundled fonts do not carry (see
/// `ui_glyphs.rs`).
const COLLAPSE_LABEL: &str = "\u{2039}";

/// The Add-layer buttons' label — one button above the rows and one below
/// (plan §1.3), both opening the catalog: the list can be taller than the
/// panel, and "add" is wanted at whichever end the scroll left the user.
const ADD_LAYER_LABEL: &str = "+ Add layer";

/// The non-map body's route to where the pane's real controls live (plan
/// §1.4): a pane with no map has no layer rows, and a panel that were only
/// the explanatory caption read as broken — this button is the body's one
/// action, and it opens the inspector on Pane properties.
const PANE_PROPS_BUTTON_LABEL: &str = "Pane properties...";

/// A row's minimum height — the whole row is the click target (the M8
/// full-row fix), so it lays out at a comfortable hit height even when the
/// handler offers no status line under the name.
const MIN_ROW_HEIGHT: f32 = 28.0;

/// The phone Layers page's helper caption (plan §1.3) — the demo's "same
/// stack as desktop" one-liner, in this app's own words. Sheet host only:
/// on the wider widths the panel *is* visibly the desktop's, and the line
/// would restate the screen.
const SHEET_HELPER_CAPTION: &str = "The same layer stack as on a desktop: \
    rows select a layer, \u{1f441} hides it, \u{23f6}\u{23f7} set what draws \
    over what.";

/// One row the stack actually drew, as it was drawn. Reported by the
/// renderer, never rebuilt by a test — see `ui_menu::DrawnMenuLeaf` for the
/// pattern.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StackRowProbe {
    /// The layer this row is for.
    pub kind: OverlayKind,
    /// The row's click target — the **whole row**, full panel width (the M8
    /// full-row fix): clicking anywhere on it that is not one of the buttons
    /// below selects the layer in the inspector.
    pub rect: egui::Rect,
    /// The 👁 visibility eye.
    pub eye: egui::Rect,
    /// The enabled state the eye was drawn showing.
    pub eye_on: bool,
    /// The ⏶ reorder button, and whether it was enabled.
    pub up: (egui::Rect, bool),
    /// The ⏷ reorder button, and whether it was enabled.
    pub down: (egui::Rect, bool),
    /// The status line under the name, when the handler offered one.
    pub status_line: Option<String>,
    /// Whether the row was drawn as the inspector's current selection.
    pub selected: bool,
    /// The trailing `›` chevron — drawn on the drawer and sheet hosts only
    /// (plan §1.3), so `None` on the desktop sidebar.
    pub chevron: Option<egui::Rect>,
}

/// What the stack drew last frame.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StackProbe {
    /// The floating area's whole rect, off its own response.
    pub rect: egui::Rect,
    /// The header title — a secondary route to Pane properties (the pills
    /// are the primary one).
    pub header: egui::Rect,
    /// The ‹ collapse button.
    pub collapse: egui::Rect,
    /// Whether the stack was on screen this frame.
    pub open: bool,
    /// The `+ Add layer` button above the rows — [`egui::Rect::NOTHING`] for
    /// a pane with no map, which has no rows to add to.
    pub add_top: egui::Rect,
    /// The `+ Add layer` button below the rows, on the same terms.
    pub add_bottom: egui::Rect,
    /// The rows, top row first — draw order reversed.
    pub rows: Vec<StackRowProbe>,
    /// The non-map body's caption — [`egui::Rect::NOTHING`] on a map pane,
    /// whose body is the rows above.
    pub non_map_note: egui::Rect,
    /// The non-map body's `Pane properties...` button, on the same terms.
    pub props_button: egui::Rect,
}

#[cfg(test)]
impl Default for StackProbe {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            header: egui::Rect::NOTHING,
            collapse: egui::Rect::NOTHING,
            open: false,
            add_top: egui::Rect::NOTHING,
            add_bottom: egui::Rect::NOTHING,
            rows: Vec::new(),
            non_map_note: egui::Rect::NOTHING,
            props_button: egui::Rect::NOTHING,
        }
    }
}

impl super::Gui {
    /// The stack, in the slot its host chose — the map's top-left corner
    /// from the shell, the sheet's body from the phone shell.
    ///
    /// `pane` is the active pane, `mem::take`n by the caller for the whole
    /// stack+inspector pass — nothing in here reads `self.panes[..]`, whose
    /// active slot holds a default placeholder until the caller restores it.
    /// `statuses` is built by the caller from the *taken* pane — the live
    /// one — against a registry it has demonstrably loaded with that pane's
    /// configs (see `ui_shell.rs`).
    pub(super) fn render_stack(
        &mut self,
        ctx: &egui::Context,
        slot: SurfaceSlot,
        pane: &mut PaneState,
        statuses: &[(OverlayKind, Option<String>)],
        actions: &mut Vec<GuiAction>,
    ) {
        let is_drawer = !self.layout.width.has_persistent_sidebar();
        // The sheet host draws no header of its own here — the sheet's title
        // row is the single header (plan §1.13 as polished in M7) — so the
        // whole slot is the body's.
        let max_body_height = if slot.sheet {
            slot.avail_height.max(0.0)
        } else {
            (slot.avail_height - HEADER_ALLOWANCE).max(0.0)
        };

        // `Pane N (SITE)` reads off the taken pane — the live one.
        let title = format!("Layers - Pane {} ({})", self.active_pane + 1, pane.site);

        #[cfg(test)]
        let mut probe = StackProbe {
            open: true,
            ..StackProbe::default()
        };

        // The sheet host swaps the frame and the order, never the id: the
        // area — and every id chain hanging off it — is the same surface at
        // every width (see `SurfaceSlot`).
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
        let area = egui::Area::new(egui::Id::new("layers_panel"))
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
                    // The sheet host draws no header row: the sheet's title
                    // row is the single header there (title + ×), and the ‹
                    // collapse would shadow the back-chain that already
                    // closes the page (§1.13's no-back-buttons rule; M7's
                    // sheet-header polish). The wider hosts keep both.
                    if !slot.sheet {
                        ui.horizontal(|ui| {
                            // Right-to-left so the collapse button owns the
                            // right edge and the title truncates in what is
                            // left.
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let collapse = ui
                                        .button(COLLAPSE_LABEL)
                                        .on_hover_text("Collapse the layer stack");
                                    #[cfg(test)]
                                    {
                                        probe.collapse = collapse.rect;
                                    }
                                    if collapse.clicked() {
                                        // The same split the top bar's Layers
                                        // toggle writes through: an explicit
                                        // choice over the Expanded default,
                                        // the drawer flag elsewhere.
                                        if self.layout.width.has_persistent_sidebar() {
                                            self.stack_open = Some(false);
                                        } else {
                                            self.drawer_open = false;
                                        }
                                    }

                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            let header = ui
                                                .add(
                                                    egui::Label::new(
                                                        egui::RichText::new(title.as_str())
                                                            .strong(),
                                                    )
                                                    .truncate()
                                                    .sense(egui::Sense::click()),
                                                )
                                                .on_hover_text("Layer order: top = drawn last");
                                            #[cfg(test)]
                                            {
                                                probe.header = header.rect;
                                            }
                                            // A route to Pane properties, for
                                            // every pane kind — the header
                                            // names the pane, so clicking it
                                            // selects the pane. The pills are
                                            // the primary route now; this
                                            // stays as the panel's own way in.
                                            if header.clicked() {
                                                self.select_pane_props();
                                            }
                                        },
                                    );
                                },
                            );
                        });
                        ui.separator();
                    }

                    // An explicit salt rather than egui's positional auto-id:
                    // the scroll offset must survive edits to the header, and
                    // the breakpoint tests read the offset back through this
                    // id. `min_scrolled_height` is the §5.9 fix — see the
                    // module note.
                    let scroll = egui::ScrollArea::vertical()
                        .id_salt("layers_scroll")
                        .max_height(max_body_height)
                        .min_scrolled_height(max_body_height)
                        .show(ui, |ui| {
                            self.render_stack_rows(
                                ui,
                                is_drawer,
                                slot.sheet,
                                pane,
                                statuses,
                                actions,
                                #[cfg(test)]
                                &mut probe,
                            );
                        });

                    // Report the id egui really used, rather than
                    // reconstructing it — the breakpoint tests must be
                    // reading the same id the scroll state is stored under.
                    #[cfg(test)]
                    self.widget_id_probes.push(("layers_scroll", scroll.id));
                    #[cfg(not(test))]
                    let _ = scroll;
                });
            });

        #[cfg(test)]
        {
            probe.rect = area.response.rect;
            self.last_stack = probe;
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The scroll body: one row per layer for a map pane, the explained
    /// absence for a pane with no map. `sheet` is the phone-sheet host,
    /// which alone appends the helper caption under the rows (plan §1.3).
    #[allow(clippy::too_many_arguments)]
    fn render_stack_rows(
        &mut self,
        ui: &mut egui::Ui,
        is_drawer: bool,
        sheet: bool,
        pane: &mut PaneState,
        statuses: &[(OverlayKind, Option<String>)],
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut StackProbe,
    ) {
        // Every row is a layer drawn over map tiles, so a pane with no map
        // has no rows — the same omission-plus-one-line convention the old
        // panel used, for the same reason: a dozen disabled rows would bury
        // the fact that nothing here can apply. No Add-layer buttons either:
        // the catalog adds map layers, and this pane has no map to add to.
        // What the body has instead (the M8 fix — a bare one-liner read as a
        // broken panel): the explained absence as a padded caption, and the
        // one action that *does* apply — the pane's own properties, where a
        // 3D or section pane's real controls live.
        if !pane.is_map() {
            ui.add_space(6.0);
            let note = ui.label(
                egui::RichText::new(super::NON_MAP_LAYERS_NOTE)
                    .small()
                    .weak(),
            );
            ui.add_space(6.0);
            let props = ui.button(PANE_PROPS_BUTTON_LABEL);
            #[cfg(test)]
            {
                probe.non_map_note = note.rect;
                probe.props_button = props.rect;
            }
            #[cfg(not(test))]
            let _ = note;
            if props.clicked() {
                self.select_pane_props();
            }
            return;
        }

        let add_top = ui.button(ADD_LAYER_LABEL);
        #[cfg(test)]
        {
            probe.add_top = add_top.rect;
        }
        if add_top.clicked() {
            self.catalog_open = true;
        }

        // Top row = drawn last. The swap is recorded and applied after the
        // loop so the walk iterates a consistent order; display row `i` is
        // `draw_order[len - 1 - i]`.
        let order: Vec<OverlayKind> = pane.draw_order.iter().rev().copied().collect();
        let last = order.len().saturating_sub(1);
        let mut swap: Option<(usize, usize)> = None;

        for (row_idx, &kind) in order.iter().enumerate() {
            // Keyed on the layer, not the position, so a row's widget state
            // travels with it when it is reordered.
            ui.push_id(kind, |ui| {
                let enabled = pane.is_overlay_enabled(kind);
                let selected =
                    self.insp_open && self.inspector_sel == InspectorSelection::Layer(kind);
                let name = self.overlays.display_name(kind).to_owned();
                let status = statuses
                    .iter()
                    .find(|(k, _)| *k == kind)
                    .and_then(|(_, line)| line.clone());

                // The whole row is the click target (the M8 full-row fix):
                // the full panel width at a comfortable height, allocated
                // with its own click sense **before** the row's buttons —
                // egui resolves an overlap to the later registration, so the
                // reorder pair and the eye, drawn after inside this rect,
                // keep their own clicks by sitting on top. Sized from the
                // real text styles so a themed font cannot clip the block.
                let row_height = (ui.text_style_height(&egui::TextStyle::Body)
                    + status
                        .as_ref()
                        .map_or(0.0, |_| ui.text_style_height(&egui::TextStyle::Small))
                    + 6.0)
                    .max(MIN_ROW_HEIGHT);
                let (row_rect, row) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_height),
                    egui::Sense::click(),
                );

                // Hover and selection read as the whole row, in the stock
                // theme's own selectable visuals — painted first, so the
                // content draws over the highlight.
                if selected || row.hovered() || row.has_focus() {
                    let visuals = ui.style().interact_selectable(&row, selected);
                    ui.painter().rect(
                        row_rect,
                        visuals.corner_radius,
                        visuals.weak_bg_fill,
                        visuals.bg_stroke,
                        egui::StrokeKind::Inside,
                    );
                }

                let mut row_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(row_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                let ui = &mut row_ui;

                // The reorder pair, disabled at the ends.
                let (up, down) = ui
                    .vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        // Sized to their small text, not the interact
                        // height: two default-height buttons would outgrow
                        // the row they sit in.
                        ui.spacing_mut().interact_size.y = 0.0;
                        let up = ui.add_enabled(
                            row_idx > 0,
                            egui::Button::new(egui::RichText::new("\u{23f6}").small()).frame(false),
                        );
                        let down = ui.add_enabled(
                            row_idx < last,
                            egui::Button::new(egui::RichText::new("\u{23f7}").small()).frame(false),
                        );
                        (up, down)
                    })
                    .inner;
                // Display row up = drawn later = towards the *end* of
                // `draw_order`.
                let n = order.len();
                if up.clicked() {
                    swap = Some((n - 1 - row_idx, n - row_idx));
                }
                if down.clicked() {
                    swap = Some((n - 2 - row_idx, n - 1 - row_idx));
                }

                // The 👁 eye. Both halves through `write_pane_overlay`,
                // on the *taken* pane — `set_active_pane_overlay` would
                // write the placeholder in the vector.
                let eye_text = if enabled {
                    egui::RichText::new("\u{1f441}")
                } else {
                    egui::RichText::new("-").weak()
                };
                let eye = ui
                    .add(
                        egui::Button::new(eye_text)
                            .frame(false)
                            .min_size(egui::vec2(20.0, 0.0)),
                    )
                    .on_hover_text(if enabled {
                        format!("Hide {name}")
                    } else {
                        format!("Show {name}")
                    });
                if eye.clicked() {
                    // Both halves plus the enable-fetch rule, through the
                    // one helper the inspector's Show toggle and the
                    // catalog's tiles share.
                    let idx = self.active_pane;
                    self.set_pane_overlay_with_fetch(pane, idx, kind, !enabled, actions);
                }

                // A trailing `›` on the drawer and sheet hosts (plan §1.3):
                // there a row click *pushes* the inspector over this list,
                // and the chevron says so. The desktop sidebar, where the
                // inspector opens beside the stack, carries none.
                // Right-to-left so the chevron owns the edge and the name
                // block takes what is left — the header's own device. The
                // labels are explicitly non-selectable and carry no sense of
                // their own: a click on the text *is* a click on the row.
                #[cfg(test)]
                let mut chevron_rect = None;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if is_drawer {
                        let chevron = ui.add(
                            egui::Label::new(egui::RichText::new("\u{203a}").weak())
                                .selectable(false),
                        );
                        #[cfg(test)]
                        {
                            chevron_rect = Some(chevron.rect);
                        }
                        #[cfg(not(test))]
                        let _ = chevron;
                    }

                    // The name and status block. Hidden layers render
                    // dimmed — weak text is the stock theme's own dimming.
                    // The selection highlight is the row's, painted above.
                    ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let name_text = if enabled {
                            egui::RichText::new(name.as_str())
                        } else {
                            egui::RichText::new(name.as_str()).weak()
                        };
                        ui.add(egui::Label::new(name_text).selectable(false).truncate());
                        if let Some(line) = &status {
                            ui.add(
                                egui::Label::new(egui::RichText::new(line.as_str()).small().weak())
                                    .selectable(false)
                                    .truncate(),
                            );
                        }
                    });
                });

                #[cfg(test)]
                probe.rows.push(StackRowProbe {
                    kind,
                    rect: row_rect,
                    eye: eye.rect,
                    eye_on: enabled,
                    up: (up.rect, row_idx > 0),
                    down: (down.rect, row_idx < last),
                    status_line: status.clone(),
                    selected,
                    chevron: chevron_rect,
                });

                if row.clicked() {
                    // The inspector opens over or beside this list per host;
                    // the list stays open beneath either way — the M3-era
                    // rule that closed the Compact drawer died with the
                    // slide-over it served.
                    self.select_layer(kind);
                }
            });
        }

        let add_bottom = ui.button(ADD_LAYER_LABEL);
        #[cfg(test)]
        {
            probe.add_bottom = add_bottom.rect;
        }
        if add_bottom.clicked() {
            self.catalog_open = true;
        }

        // The phone page's one-line orientation (plan §1.3): the sheet is
        // the only host where "this is the desktop's panel" is not visibly
        // true, so it is the only host that says it.
        if sheet {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(SHEET_HELPER_CAPTION).small().weak());
        }

        if let Some((a, b)) = swap {
            pane.draw_order.swap(a, b);
            // The persisted order changed; the caller's post-restore
            // `propagate_layer_sync` fans it out to the synced panes.
        }
    }
}
