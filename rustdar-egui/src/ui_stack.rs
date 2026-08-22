//! The layer stack: one row per layer of the active pane, in draw order.

use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::STATUS_MARK;
use rustdar_source::id::LayerId;

use super::shell::SurfaceSlot;
use super::{InspectorSelection, PaneState};

/// Width of the stack, in both its sidebar and drawer forms.
pub(super) const STACK_WIDTH: f32 = 240.0;

/// The stack's inset from the map's top-left corner.
pub(super) const STACK_INSET: f32 = 8.0;

/// What the stack leaves clear above the map's bottom edge: room for the
/// status bar and the timeline transport floating there.
pub(super) const STACK_BOTTOM_CLEARANCE: f32 = 88.0;

/// What the header row and its separator cost above the scroll body — charged
/// against the body's ceiling so the whole panel, header included, stays out
/// of the bottom clearance band.
const HEADER_ALLOWANCE: f32 = 40.0;

/// The collapse button's glyph: the panel slides out to the left. `‹` rather
/// than the demo's `⟨`, which egui's bundled fonts do not carry (see
/// `ui_glyphs.rs`).
const COLLAPSE_LABEL: &str = "\u{2039}";

/// The catalog buttons' label — one above the rows and one below, both opening
/// the same catalog: the list can be taller than the panel, and the way in is
/// wanted at whichever end the scroll left the user.
///
/// **It says "add" again, and now it is true.** W21 renamed it to "show"
/// because at the time nothing was ever added: every pane held a slot for every
/// registered layer for as long as it existed, so the rows below were the whole
/// inventory and the catalog could only turn one of them on. The rename was the
/// honest description of a broken shape, not a fix for it. The stack is a
/// curated list now (`pane/layer_stack.rs`), the catalog's tiles really do
/// insert a row, and the label goes back to naming what the button does.
pub(crate) const ADD_LAYER_LABEL: &str = "+ Add layer";

/// The per-row remove control's glyph: a wastebasket, U+1F5D1.
///
/// **Probed, not assumed** (`ui_glyphs.rs` and the coverage test that walks
/// it): egui's bundled proportional family carries this one, and carries
/// *none* of U+2716, U+2715, U+2296, U+232B or U+2326 — the ASCII-adjacent
/// marks a reader reaches for first are exactly the ones that would have drawn
/// a tofu box. U+00D7 does exist there and is deliberately not used: it is the
/// app's close/dismiss glyph and nothing else, and a destructive control
/// wearing the dismissal glyph is the defect the catalog's preset delete was
/// already written to avoid.
const REMOVE_LABEL: &str = "\u{1f5d1}";

/// The layer-less body's route to where the pane's real controls live (plan
/// the plan): a pane that draws no map layers has no rows, and a panel that were
/// only the explanatory caption read as broken — this button is the body's one
/// action, and it opens the inspector on Pane properties.
const PANE_PROPS_BUTTON_LABEL: &str = "Pane properties...";

/// A row's minimum height — the whole row is the click target (the M8
/// full-row fix), so it lays out at a comfortable hit height even when the
/// handler offers no status line under the name.
const MIN_ROW_HEIGHT: f32 = 28.0;

/// The drag grip's hit width. Full row height; the painted dots are smaller.
const GRIP_WIDTH: f32 = 18.0;

/// The painted grip: two columns of three dots, this radius each.
const GRIP_DOT_RADIUS: f32 = 1.2;
/// Spacing between grip dot centres, both axes.
const GRIP_DOT_SPACING: f32 = 5.0;

/// The phone Layers page's helper caption — the demo's "same
/// stack as desktop" one-liner, in this app's own words. Sheet host only:
/// on the wider widths the panel *is* visibly the desktop's, and the line
/// would restate the screen.
const SHEET_HELPER_CAPTION: &str = "The same layer stack as on a desktop: \
    rows select a layer, \u{1f441} hides it, \u{1f5d1} takes it out of this \
    pane, dragging the grip sets what draws over what.";

/// One row the stack actually drew, as it was drawn. Reported by the
/// renderer, never rebuilt by a test — see `ui_menu::DrawnMenuLeaf` for the
/// pattern.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StackRowProbe {
    /// The layer this row is for.
    pub kind: LayerId,
    /// The row's click target — the **whole row**, full panel width (the M8
    /// full-row fix): clicking anywhere on it that is not one of the buttons
    /// below selects the layer in the inspector.
    pub rect: egui::Rect,
    /// The 👁 visibility eye.
    pub eye: egui::Rect,
    /// The enabled state the eye was drawn showing.
    pub eye_on: bool,
    /// The 🗑 remove control — drawn on every row, so the answer to "can this
    /// layer be removed" is visible rather than inferred from an absence.
    pub remove: egui::Rect,
    /// Whether the remove control was drawn live. `false` is a structural
    /// layer's greyed can, whose hover says why (see
    /// [`PaneState::layer_removal_refusal`](crate::pane::PaneState::layer_removal_refusal)).
    pub remove_enabled: bool,
    /// The drag grip — the reorder affordance, and the only part of the row
    /// that senses a drag (a swipe on the body scrolls).
    pub handle: egui::Rect,
    /// The name-and-status text block, as laid out — what the row-centering
    /// pin measures against the row rect.
    pub name: egui::Rect,
    /// The status line under the name, when the handler offered one.
    pub status_line: Option<String>,
    /// Whether the row was drawn as the inspector's current selection.
    pub selected: bool,
    /// The trailing `›` chevron — drawn on the drawer and sheet hosts only
    ///, so `None` on the desktop sidebar.
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
    /// a pane that draws no map layers, which has no rows to add to.
    pub add_top: egui::Rect,
    /// The `+ Add layer` button below the rows, on the same terms.
    pub add_bottom: egui::Rect,
    /// The rows, top row first — draw order reversed.
    pub rows: Vec<StackRowProbe>,
    /// The layer-less body's caption — [`egui::Rect::NOTHING`] on a pane that
    /// draws the layers, whose body is the rows above.
    pub non_map_note: egui::Rect,
    /// The layer-less body's `Pane properties...` button, on the same terms.
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
    pub(super) fn render_stack(
        &mut self,
        ctx: &egui::Context,
        slot: SurfaceSlot,
        pane: &mut PaneState,
        statuses: &[(LayerId, Option<String>)],
        actions: &mut Vec<GuiAction>,
    ) {
        let is_drawer = !self.layout.width.has_persistent_sidebar();
        // The sheet host draws no header of its own here — the sheet's title
        // row is the single header — so the
        // whole slot is the body's.
        let max_body_height = if slot.sheet {
            slot.avail_height.max(0.0)
        } else {
            (slot.avail_height - HEADER_ALLOWANCE).max(0.0)
        };

        // `Pane N (SITE)` reads off the taken pane — the live one.
        let title = format!("Layers - Pane {} ({})", self.active_pane + 1, pane.site());

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
            super::shell::chrome_frame(&ctx.global_style())
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
                    // closes the page (the plan's no-back-buttons rule; M7's
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
                    // id. `min_scrolled_height` is the the plan fix — see the
                    // module note.
                    let scroll = egui::ScrollArea::vertical()
                        .scroll_source(super::shell::panel_scroll_source())
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
                    self.probes
                        .widget_id_probes
                        .push(("layers_scroll", scroll.id));
                    #[cfg(not(test))]
                    let _ = scroll;
                });
            });

        #[cfg(test)]
        {
            probe.rect = area.response.rect;
            self.probes.last_stack = probe;
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The scroll body: one row per layer for a pane that draws the map layers
    /// ([`PaneState::draws_map_layers`] — the plan view and the 3D pane), the
    /// explained absence for one that does not. `sheet` is the phone-sheet
    /// host, which alone appends the helper caption under the rows.
    #[allow(clippy::too_many_arguments)]
    fn render_stack_rows(
        &mut self,
        ui: &mut egui::Ui,
        is_drawer: bool,
        sheet: bool,
        pane: &mut PaneState,
        statuses: &[(LayerId, Option<String>)],
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut StackProbe,
    ) {
        // Every row is a layer this pane's own render draws and gates on its
        // own `is_overlay_enabled`, so the body is the rows for every pane that
        // draws them — a 3D pane included, whose ground kinds go onto its floor
        // and whose colour scale goes onto its glass
        // (`PaneState::draws_map_layers`). A cross-section draws none of them
        // and is the one kind left with nothing to list: no rows, and no
        // catalog buttons, because the catalog shows map layers and this pane
        // has no map to show them on. What its body has instead (the M8 fix — a
        // bare one-liner read as a broken panel): the explained absence as a
        // padded caption, and the one action that *does* apply — the pane's own
        // properties, where a section pane's real controls live.
        if !pane.draws_map_layers() {
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

        // Top row = drawn last: display row `i` is `draw_order[len - 1 - i]`.
        let order: Vec<LayerId> = pane.draw_order().rev().cloned().collect();
        let mut row_rects: Vec<egui::Rect> = Vec::with_capacity(order.len());
        let mut drag_released = false;
        // Deferred to after the walk, and it has to be: the rows the loop
        // reports feed `resolve_stack_drag`, which pairs `row_rects` with
        // `order` positionally, and a list that lost an entry halfway through
        // would land a drag on the wrong layer.
        let mut removing: Option<LayerId> = None;

        for kind in order.iter() {
            // Keyed on the layer, not the position, so a row's widget state
            // travels with it when it is reordered. `as_str` because the id
            // itself is the stable identity.
            ui.push_id(kind.as_str(), |ui| {
                let enabled = pane.is_overlay_enabled(kind);
                let selected =
                    self.insp_open && self.inspector_sel == InspectorSelection::Layer(kind.clone());
                let name = self.overlays.display_name(kind).to_owned();
                let status = statuses
                    .iter()
                    .find(|(k, _)| k == kind)
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
                row_rects.push(row_rect);
                // The catalog's other half: applying a tile selects the layer
                // in the inspector, and this brings its row into view, so the
                // two surfaces visibly name the same thing. One-shot — a
                // standing target would fight every scroll the user makes.
                if self.stack_scroll_to.as_ref() == Some(kind) {
                    self.stack_scroll_to = None;
                    row.scroll_to_me(Some(egui::Align::Center));
                }
                let lifting = self.stack_drag.as_ref() == Some(kind);

                // Hover and selection read as the whole row, in the stock
                // theme's own selectable visuals — painted first, so the
                // content draws over the highlight. The hover read is
                // `contains_pointer`, not `hovered`: the eye and the grip
                // sit on top of this rect and take `hovered` with them,
                // blinking the highlight off as the pointer crosses. The
                // union read is for the highlight only — clicks keep egui's
                // later-registration precedence untouched.
                let hovered = row.contains_pointer();
                if selected || hovered || row.has_focus() {
                    let mut visuals = if hovered {
                        ui.style().visuals.widgets.hovered
                    } else {
                        ui.style().interact_selectable(&row, selected)
                    };
                    if selected {
                        // `interact_selectable`'s own override, re-applied on
                        // the hovered branch so selection paints one fill
                        // wherever the pointer is inside the row.
                        visuals.weak_bg_fill = ui.visuals().selection.bg_fill;
                    }
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
                // The lift: the source row dims while its ghost follows the
                // pointer (painted after the loop).
                if lifting {
                    ui.multiply_opacity(0.4);
                }

                // The drag grip — the only part of the row that senses a
                // drag, so a swipe anywhere else on the row still scrolls
                // the list on touch. The dots are painted: no glyph egui's
                // bundled fonts carry draws a grip (`ui_glyphs.rs`).
                let (handle_rect, handle) =
                    ui.allocate_exact_size(egui::vec2(GRIP_WIDTH, row_height), egui::Sense::drag());
                let grip_color = if handle.hovered() || lifting {
                    ui.visuals().strong_text_color()
                } else {
                    ui.visuals().weak_text_color()
                };
                for col in 0..2 {
                    for dot in 0..3 {
                        let offset = egui::vec2(
                            (col as f32 - 0.5) * GRIP_DOT_SPACING,
                            (dot as f32 - 1.0) * GRIP_DOT_SPACING,
                        );
                        ui.painter().circle_filled(
                            handle_rect.center() + offset,
                            GRIP_DOT_RADIUS,
                            grip_color,
                        );
                    }
                }
                let handle = handle
                    .on_hover_cursor(egui::CursorIcon::Grab)
                    .on_hover_text(format!("Drag to reorder {name}"));
                if handle.drag_started() {
                    self.stack_drag = Some(kind.clone());
                }
                if lifting {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    if handle.drag_stopped() {
                        drag_released = true;
                    }
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

                // The 🗑 remove control. The eye beside it hides the layer;
                // this takes it out of the pane's stack, which is a different
                // act and is why the two are not one control with a long
                // press. **Drawn on every row, live or not**: a layer the pane
                // cannot give up gets a greyed can whose hover says why, rather
                // than no control at all — an absent affordance reads as an
                // oversight, and one that silently does nothing is worse than
                // both.
                let refusal = pane.layer_removal_refusal(kind);
                let remove = ui
                    .add_enabled(
                        refusal.is_none(),
                        egui::Button::new(egui::RichText::new(REMOVE_LABEL).small().color(
                            if refusal.is_none() {
                                ui.visuals().weak_text_color()
                            } else {
                                ui.visuals().widgets.noninteractive.fg_stroke.color
                            },
                        ))
                        .frame(false)
                        .min_size(egui::vec2(20.0, 0.0)),
                    )
                    .on_hover_text(format!("Remove {name} from this pane"))
                    .on_disabled_hover_text(refusal.unwrap_or_default());
                if remove.clicked() {
                    removing = Some(kind.clone());
                }

                // A trailing `›` on the drawer and sheet hosts:
                // there a row click *pushes* the inspector over this list,
                // and the chevron says so. The desktop sidebar, where the
                // inspector opens beside the stack, carries none.
                #[cfg(test)]
                let mut chevron_rect = None;
                #[cfg(test)]
                let mut name_rect = egui::Rect::NOTHING;
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
                    let block = ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let text_height = ui.text_style_height(&egui::TextStyle::Body)
                            + status
                                .as_ref()
                                .map_or(0.0, |_| ui.text_style_height(&egui::TextStyle::Small));
                        ui.add_space(((row_height - text_height) / 2.0).max(0.0));
                        let name_text = if enabled {
                            egui::RichText::new(name.as_str())
                        } else {
                            egui::RichText::new(name.as_str()).weak()
                        };
                        let name_label =
                            ui.add(egui::Label::new(name_text).selectable(false).truncate());
                        let mut text_rect = name_label.rect;
                        if let Some(line) = &status {
                            // A line that opens with the fault mark is not a
                            // count, and `.weak()` is the theme's own way of
                            // saying "this is a detail" — the same dim grey
                            // `3 shown - W/Wa` sits in. A layer that stopped
                            // updating, or is drawing 85 of 297 warnings, gets
                            // the warning colour instead: same size, same
                            // place, same rect, legible as a fault.
                            let text = egui::RichText::new(line.as_str()).small();
                            let text = if line.starts_with(STATUS_MARK) {
                                text.color(ui.visuals().warn_fg_color)
                            } else {
                                text.weak()
                            };
                            let status_label =
                                ui.add(egui::Label::new(text).selectable(false).truncate());
                            text_rect = text_rect.union(status_label.rect);
                        }
                        text_rect
                    });
                    #[cfg(test)]
                    {
                        name_rect = block.inner;
                    }
                    #[cfg(not(test))]
                    let _ = block;
                });

                #[cfg(test)]
                probe.rows.push(StackRowProbe {
                    kind: kind.clone(),
                    rect: row_rect,
                    eye: eye.rect,
                    eye_on: enabled,
                    remove: remove.rect,
                    remove_enabled: refusal.is_none(),
                    handle: handle.rect,
                    name: name_rect,
                    status_line: status.clone(),
                    selected,
                    chevron: chevron_rect,
                });

                if row.clicked() {
                    // The inspector opens over or beside this list per host;
                    // the list stays open beneath either way — the M3-era
                    // rule that closed the Compact drawer died with the
                    // slide-over it served.
                    self.select_layer(kind.clone());
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

        // The phone page's one-line orientation: the sheet is
        // the only host where "this is the desktop's panel" is not visibly
        // true, so it is the only host that says it.
        if sheet {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(SHEET_HELPER_CAPTION).small().weak());
        }

        self.resolve_stack_drag(ui, &order, &row_rects, drag_released, pane);

        if let Some(kind) = removing {
            self.remove_layer_from_pane(pane, &kind);
        }
    }

    /// **Take a layer out of the active pane's stack**, and leave nothing
    /// pointing at it.
    ///
    /// The pane's own [`PaneState::remove_layer`] is what drops the slot and
    /// releases the textures; what it cannot see is the chrome still pointing
    /// at the layer — the inspector's selection, which would otherwise render a
    /// body for a layer this pane no longer holds, and the two one-shot targets
    /// (`stack_scroll_to`, `stack_drag`) that name a row by id.
    ///
    /// **No sync fan-out here, deliberately**, and on the same terms as the eye
    /// beside it: this runs against the `mem::take`n pane the host is holding,
    /// so `propagate_pane_sync` would read the vector's placeholder. The frame
    /// that puts the pane back is what carries the edit to the linked group,
    /// exactly as it does for a visibility toggle.
    ///
    /// The inspector is **redirected, not closed**: `select_pane_props` would
    /// force the panel open on a host where the user had it shut, so the
    /// selection moves and the open/closed posture is left exactly as it was.
    pub(super) fn remove_layer_from_pane(&mut self, pane: &mut PaneState, kind: &LayerId) {
        if !pane.remove_layer(kind) {
            return;
        }
        if self.inspector_sel == InspectorSelection::Layer(kind.clone()) {
            self.inspector_sel = InspectorSelection::PaneProps;
        }
        if self.stack_scroll_to.as_ref() == Some(kind) {
            self.stack_scroll_to = None;
        }
        if self.stack_drag.as_ref() == Some(kind) {
            self.stack_drag = None;
        }
    }

    /// Advance or land the grip drag, once the frame's row rects are known.
    fn resolve_stack_drag(
        &mut self,
        ui: &egui::Ui,
        order: &[LayerId],
        row_rects: &[egui::Rect],
        released: bool,
        pane: &mut PaneState,
    ) {
        let Some(dragged) = self.stack_drag.clone() else {
            return;
        };
        let Some(from) = order.iter().position(|kind| *kind == dragged) else {
            // The lifted layer left the list (a sync rewrote the order
            // mid-drag); nothing to land the drag on.
            self.stack_drag = None;
            return;
        };
        // `interact_pos`, not `latest_pos`: egui-winit ends a touch with
        // `PointerButton{up}` **and** `PointerGone` in one frame's batch (the
        // harness's event-fidelity table), and `PointerGone` clears
        // `latest_pos` — read that here and every touch drag springs back on
        // the very frame it should land. `interact_pos` survives the frame it
        // went gone on (egui clears it on the next pass), so the release still
        // knows where the finger was; a pointer that *stays* gone — mouse
        // out the window, cancelled touch — reads `None` here a frame later
        // and cancels just the same.
        let Some(pointer) = ui.ctx().pointer_interact_pos() else {
            self.stack_drag = None;
            return;
        };

        // The slot: how many row centres the pointer is below. Slot `i`
        // means "above display row i"; slot `n` is below the last row.
        let slot = row_rects
            .iter()
            .filter(|rect| rect.center().y < pointer.y)
            .count();

        if released {
            self.stack_drag = None;
            let mut display: Vec<LayerId> = order.to_vec();
            display.remove(from);
            let insert_at = if slot > from { slot - 1 } else { slot }.min(display.len());
            display.insert(insert_at, dragged);
            let reordered: Vec<LayerId> = display.into_iter().rev().collect();
            pane.set_draw_order(&reordered);
            return;
        }

        // A cancelled gesture reports no release, ever — the sheet handle's
        // own rule. Nothing being dragged means the gesture died: spring back.
        if !ui.ctx().input(|i| i.pointer.any_down()) {
            self.stack_drag = None;
            return;
        }

        // The insertion line, at the slot's boundary.
        if let (Some(first), Some(last)) = (row_rects.first(), row_rects.last()) {
            let y = match row_rects.get(slot) {
                Some(rect) => rect.top(),
                None => last.bottom(),
            };
            ui.painter().hline(
                first.x_range(),
                y,
                egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
            );
        }

        // The ghost: the lifted row's name on a plate, following the pointer.
        let ghost_layer =
            egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("stack_drag_ghost"));
        let painter = ui.ctx().layer_painter(ghost_layer);
        let name = self.overlays.display_name(&dragged).to_owned();
        let galley = painter.layout_no_wrap(
            name,
            egui::TextStyle::Body.resolve(ui.style()),
            ui.visuals().strong_text_color(),
        );
        let pad = egui::vec2(8.0, 4.0);
        let plate = egui::Rect::from_min_size(
            pointer + egui::vec2(12.0, -galley.size().y / 2.0) - pad,
            galley.size() + pad * 2.0,
        );
        painter.rect(
            plate,
            4.0,
            ui.visuals().extreme_bg_color.gamma_multiply(0.9),
            egui::Stroke::new(1.0, ui.visuals().selection.bg_fill),
            egui::StrokeKind::Inside,
        );
        painter.galley(plate.min + pad, galley, egui::Color32::PLACEHOLDER);
    }
}
