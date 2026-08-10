//! The layer stack: one row per layer of the active pane, in draw order.
//!
//! One body for every shell. On Expanded it floats at the map's top-left,
//! open by default; below the sidebar breakpoint the same body is the
//! slide-over drawer, closed until the top bar's Layers toggle opens it —
//! `Gui::layers_panel_visible` is the one definition of "open", exactly as it
//! was for the panel this replaces. The area and scroll ids are the old
//! panel's (`layers_panel`, `layers_scroll`) on purpose: egui's memory of the
//! surface — its scroll offset above all — belongs to *the place the layers
//! live*, not to which milestone's renderer is drawing it.
//!
//! The rows walk the active pane's `draw_order` **reversed**, so the top row
//! is drawn last — over everything — which is the reading the header's
//! tooltip teaches. The ▲▼ buttons swap neighbours in that same `draw_order`,
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

use super::{InspectorSelection, PaneState};

/// Width of the stack, in both its sidebar and drawer forms.
///
/// One value, not two, because the panel keeps one egui id: a per-form width
/// would make it jump when the window crossed the breakpoint, for no reason a
/// user could see.
pub(super) const STACK_WIDTH: f32 = 240.0;

/// The stack's inset from the map's top-left corner.
const STACK_INSET: f32 = 8.0;

/// What the stack leaves clear above the map's bottom edge: room for the
/// status bar and the timeline transport floating there (plan §1.3).
const STACK_BOTTOM_CLEARANCE: f32 = 88.0;

/// What the header row and its separator cost above the scroll body — charged
/// against the body's ceiling so the whole panel, header included, stays out
/// of the bottom clearance band.
const HEADER_ALLOWANCE: f32 = 40.0;

/// The collapse button's glyph: the panel slides out to the left.
const COLLAPSE_LABEL: &str = "\u{27e8}";

/// One row the stack actually drew, as it was drawn. Reported by the
/// renderer, never rebuilt by a test — see `ui_menu::DrawnMenuLeaf` for the
/// pattern.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StackRowProbe {
    /// The layer this row is for.
    pub kind: OverlayKind,
    /// The row's click target — the name (and status) block that selects the
    /// layer in the inspector.
    pub rect: egui::Rect,
    /// The 👁 visibility eye.
    pub eye: egui::Rect,
    /// The enabled state the eye was drawn showing.
    pub eye_on: bool,
    /// The ▲ reorder button, and whether it was enabled.
    pub up: (egui::Rect, bool),
    /// The ▼ reorder button, and whether it was enabled.
    pub down: (egui::Rect, bool),
    /// The status line under the name, when the handler offered one.
    pub status_line: Option<String>,
    /// Whether the row was drawn as the inspector's current selection.
    pub selected: bool,
}

/// What the stack drew last frame.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StackProbe {
    /// The floating area's whole rect, off its own response.
    pub rect: egui::Rect,
    /// The header title — the interim route to Pane properties.
    pub header: egui::Rect,
    /// The ⟨ collapse button.
    pub collapse: egui::Rect,
    /// Whether the stack was on screen this frame.
    pub open: bool,
    /// The rows, top row first — draw order reversed.
    pub rows: Vec<StackRowProbe>,
}

#[cfg(test)]
impl Default for StackProbe {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            header: egui::Rect::NOTHING,
            collapse: egui::Rect::NOTHING,
            open: false,
            rows: Vec::new(),
        }
    }
}

impl super::Gui {
    /// The stack, floating at the map's top-left in whichever of its two
    /// forms this width calls for.
    ///
    /// `pane` is the active pane, `mem::take`n by the caller for the whole
    /// stack+inspector pass — nothing in here reads `self.panes[..]`, whose
    /// active slot holds a default placeholder until the caller restores it.
    /// `statuses` is built by the caller *before* the take, while the
    /// registry demonstrably holds this pane's configs.
    pub(super) fn render_stack(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        pane: &mut PaneState,
        statuses: &[(OverlayKind, Option<String>)],
        actions: &mut Vec<GuiAction>,
    ) {
        let is_drawer = !self.layout.width.has_persistent_sidebar();
        let max_body_height =
            (map_rect.height() - STACK_INSET - STACK_BOTTOM_CLEARANCE - HEADER_ALLOWANCE).max(0.0);

        // `Pane N (SITE)` reads off the taken pane — the live one.
        let title = format!(
            "Layers \u{2014} Pane {} ({})",
            self.active_pane + 1,
            pane.site
        );

        #[cfg(test)]
        let mut probe = StackProbe {
            open: true,
            ..StackProbe::default()
        };

        let area = egui::Area::new(egui::Id::new("layers_panel"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::LEFT_TOP)
            .fixed_pos(map_rect.left_top() + egui::vec2(STACK_INSET, STACK_INSET))
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.global_style()).show(ui, |ui| {
                    ui.set_width(STACK_WIDTH);
                    ui.horizontal(|ui| {
                        // Right-to-left so the collapse button owns the right
                        // edge and the title truncates in what is left.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let collapse = ui
                                .button(COLLAPSE_LABEL)
                                .on_hover_text("Collapse the layer stack");
                            #[cfg(test)]
                            {
                                probe.collapse = collapse.rect;
                            }
                            if collapse.clicked() {
                                // The same split the top bar's Layers toggle
                                // writes through: an explicit choice over the
                                // Expanded default, the drawer flag elsewhere.
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
                                                egui::RichText::new(title.as_str()).strong(),
                                            )
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                        )
                                        .on_hover_text("Layer order: top = drawn last");
                                    #[cfg(test)]
                                    {
                                        probe.header = header.rect;
                                    }
                                    // The interim route to Pane properties,
                                    // for every pane kind — the header names
                                    // the pane, so clicking it selects the
                                    // pane. M5's pills take this over.
                                    if header.clicked() {
                                        self.select_pane_props();
                                    }
                                },
                            );
                        });
                    });
                    ui.separator();

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
    /// absence for a pane with no map.
    fn render_stack_rows(
        &mut self,
        ui: &mut egui::Ui,
        is_drawer: bool,
        pane: &mut PaneState,
        statuses: &[(OverlayKind, Option<String>)],
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut StackProbe,
    ) {
        // Every row is a layer drawn over map tiles, so a pane with no map
        // has no rows — the same omission-plus-one-line convention the old
        // panel used, for the same reason: a dozen disabled rows would bury
        // the fact that nothing here can apply.
        if !pane.is_map() {
            super::render_non_map_layers_note(ui);
            return;
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

                ui.horizontal(|ui| {
                    // The reorder pair, disabled at the ends.
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let up = ui.add_enabled(
                            row_idx > 0,
                            egui::Button::new(egui::RichText::new("\u{25b2}").small())
                                .frame(false),
                        );
                        let down = ui.add_enabled(
                            row_idx < last,
                            egui::Button::new(egui::RichText::new("\u{25bc}").small())
                                .frame(false),
                        );
                        #[cfg(test)]
                        {
                            probe.rows.push(StackRowProbe {
                                kind,
                                rect: egui::Rect::NOTHING,
                                eye: egui::Rect::NOTHING,
                                eye_on: enabled,
                                up: (up.rect, row_idx > 0),
                                down: (down.rect, row_idx < last),
                                status_line: status.clone(),
                                selected,
                            });
                        }
                        // Display row up = drawn later = towards the *end*
                        // of `draw_order`.
                        let n = order.len();
                        if up.clicked() {
                            swap = Some((n - 1 - row_idx, n - row_idx));
                        }
                        if down.clicked() {
                            swap = Some((n - 2 - row_idx, n - 1 - row_idx));
                        }
                    });

                    // The 👁 eye. Both halves through `write_pane_overlay`,
                    // on the *taken* pane — `set_active_pane_overlay` would
                    // write the placeholder in the vector.
                    let eye_text = if enabled {
                        egui::RichText::new("\u{1f441}")
                    } else {
                        egui::RichText::new("\u{2013}").weak()
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
                    #[cfg(test)]
                    if let Some(row) = probe.rows.last_mut() {
                        row.eye = eye.rect;
                    }
                    if eye.clicked() {
                        Self::write_pane_overlay(&mut self.overlays, pane, kind, !enabled);
                        // A layer turned on with nothing to draw yet fetches
                        // now rather than waiting out an auto-poll interval —
                        // the same effect its own sub-toggles ask for, and
                        // the only route for a layer (SPC outlooks) that
                        // never auto-polls.
                        if !enabled
                            && !self.overlays.has_data(kind)
                            && !self.overlays.is_fetching(kind)
                        {
                            actions.push(GuiAction::FetchOverlay {
                                kind,
                                pane_idx: self.active_pane,
                            });
                        }
                    }

                    // The name and status block: the row's click target.
                    // Hidden layers render dimmed — weak text is the stock
                    // theme's own dimming.
                    ui.with_layout(
                        egui::Layout::top_down_justified(egui::Align::LEFT),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let name_text = if enabled {
                                egui::RichText::new(name.as_str())
                            } else {
                                egui::RichText::new(name.as_str()).weak()
                            };
                            let select = ui.selectable_label(selected, name_text);
                            let mut target = select.rect;
                            if let Some(line) = &status {
                                let drawn =
                                    ui.label(egui::RichText::new(line.as_str()).small().weak());
                                target = target.union(drawn.rect);
                            }
                            #[cfg(test)]
                            if let Some(row) = probe.rows.last_mut() {
                                row.rect = target;
                            }
                            #[cfg(not(test))]
                            let _ = target;
                            if select.clicked() {
                                self.select_layer(kind);
                                // Transitional, until M6's sheet: on the one
                                // width where the right slide-over lands on
                                // top of the drawer, the drawer yields — the
                                // options the user just asked for must not
                                // open underneath it.
                                if is_drawer
                                    && self.layout.width == crate::ui_layout::WidthClass::Compact
                                {
                                    self.drawer_open = false;
                                }
                            }
                        },
                    );
                });
            });
        }

        if let Some((a, b)) = swap {
            pane.draw_order.swap(a, b);
            // The persisted order changed; the caller's post-restore
            // `propagate_layer_sync` fans it out to the synced panes.
        }
    }
}
