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
//! tooltip teaches. Reordering is a drag on the row's grip: the ⏶⏷ buttons
//! this replaces were too small on a desktop and unusable on touch (the
//! second user test). The grip is a painted 2×3-dot affordance — no carried
//! glyph draws one; see `ui_glyphs.rs` — sensing the drag alone, so a swipe
//! on the row body still scrolls the list on touch. The drag lifts the row
//! (the source dims, a ghost follows the pointer), an insertion line names
//! the slot, and the release permutes the same persisted `draw_order` the
//! buttons used to swap, through the same `propagate_layer_sync` fan-out.
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

/// The drag grip's hit width. Full row height; the painted dots are smaller.
const GRIP_WIDTH: f32 = 18.0;

/// The painted grip: two columns of three dots, this radius each.
const GRIP_DOT_RADIUS: f32 = 1.2;
/// Spacing between grip dot centres, both axes.
const GRIP_DOT_SPACING: f32 = 5.0;

/// The phone Layers page's helper caption (plan §1.3) — the demo's "same
/// stack as desktop" one-liner, in this app's own words. Sheet host only:
/// on the wider widths the panel *is* visibly the desktop's, and the line
/// would restate the screen.
const SHEET_HELPER_CAPTION: &str = "The same layer stack as on a desktop: \
    rows select a layer, \u{1f441} hides it, dragging the grip sets what \
    draws over what.";

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

        // Top row = drawn last: display row `i` is `draw_order[len - 1 - i]`.
        // A grip drag in flight is resolved after the loop, once every row's
        // rect for this frame is known — the insertion slot is a function of
        // the pointer against all of them, and the release permutes then.
        let order: Vec<OverlayKind> = pane.draw_order.iter().rev().copied().collect();
        let mut row_rects: Vec<egui::Rect> = Vec::with_capacity(order.len());
        let mut drag_released = false;

        for &kind in order.iter() {
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
                row_rects.push(row_rect);
                let lifting = self.stack_drag == Some(kind);

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
                    self.stack_drag = Some(kind);
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
                    // The selection highlight is the row's, painted above.
                    // A nested top-down child flows from the *top* of the
                    // row whatever the parent's cross-align says (the
                    // second user test's sits-high finding), so the block
                    // centres itself: half the row's slack above the text.
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
                            let status_label = ui.add(
                                egui::Label::new(egui::RichText::new(line.as_str()).small().weak())
                                    .selectable(false)
                                    .truncate(),
                            );
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
                    kind,
                    rect: row_rect,
                    eye: eye.rect,
                    eye_on: enabled,
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

        self.resolve_stack_drag(ui, &order, &row_rects, drag_released, pane);
    }

    /// Advance or land the grip drag, once the frame's row rects are known.
    ///
    /// While the drag flies: an insertion line at the slot the pointer names,
    /// and a ghost of the lifted row following the pointer on the tooltip
    /// order (over every panel, like any drag preview). On release the
    /// display list is permuted — remove the lifted row, insert at the slot —
    /// and written back **reversed** as the pane's `draw_order`, the same
    /// persisted field the old ⏶⏷ pair swapped; the caller's post-restore
    /// `propagate_layer_sync` fans it out to the synced panes.
    fn resolve_stack_drag(
        &mut self,
        ui: &egui::Ui,
        order: &[OverlayKind],
        row_rects: &[egui::Rect],
        released: bool,
        pane: &mut PaneState,
    ) {
        let Some(dragged) = self.stack_drag else {
            return;
        };
        let Some(from) = order.iter().position(|&kind| kind == dragged) else {
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
            let mut display: Vec<OverlayKind> = order.to_vec();
            display.remove(from);
            let insert_at = if slot > from { slot - 1 } else { slot }.min(display.len());
            display.insert(insert_at, dragged);
            pane.draw_order = display.into_iter().rev().collect();
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
        let name = self.overlays.display_name(dragged).to_owned();
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
