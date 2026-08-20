//! The phone shell's bottom cluster: the bottom bar, and the bottom sheet
//! every panel and dialog presents in below the Compact breakpoint.

use crate::actions::GuiAction;

use super::shell::SurfaceSlot;
use super::{InspectorSelection, ui_menu};

const TOAST_INSET: f32 = 8.0;

/// The sheet's clearance from the map's *top* edge; it sits flush on the bar.
const SHEET_TOP_GAP: f32 = 8.0;

const BAR_ITEM_PADDING: egui::Vec2 = egui::vec2(10.0, 4.0);
const BAR_ITEM_GAP: f32 = 2.0;

/// The sheet's two snap heights, as fractions of the map (plan §1.13).
const SHEET_HALF_FRACTION: f32 = 0.5;
const SHEET_FULL_FRACTION: f32 = 0.93;

/// A release below this fraction of the Half height dismisses the sheet.
const SHEET_DISMISS_FRACTION: f32 = 0.75;

/// The floor a mid-drag sheet cannot shrink under: handle and title stay on screen.
const MIN_SHEET_HEIGHT: f32 = 96.0;

/// The Layers page's own floor: handle + title + segments + separator +
/// margins, with slack.
const LAYERS_MIN_SHEET_HEIGHT: f32 = 148.0;

const HANDLE_HEIGHT: f32 = 16.0;
const HANDLE_BAR: egui::Vec2 = egui::vec2(40.0, 4.0);

const BODY_INSET: f32 = 8.0;

/// What the catalog body's own header costs above its scroll in the sheet.
const CATALOG_HEADER_ALLOWANCE: f32 = 48.0;

/// The scrim's fill — the same alpha egui's `Modal` dims its backdrop with.
const SCRIM_COLOR: egui::Color32 = egui::Color32::from_black_alpha(100);

/// The bottom bar's four page items, icon above label. The demo's `▤` and
/// `▦` have no glyph in egui's bundled fonts (see `ui_glyphs.rs`).
const MENU_ITEM: (&str, &str) = ("\u{2630}", "Menu");
const LAYERS_ITEM: (&str, &str) = ("\u{25a3}", "Layers");
const PANE_ITEM: (&str, &str) = ("\u{229e}", "Pane");
const APP_ITEM: (&str, &str) = ("\u{2699}", "App");

const CLOSE_LABEL: &str = "\u{d7}";

struct BarItemDraw {
    response: egui::Response,
    #[cfg_attr(not(test), allow(dead_code))]
    icon: egui::Rect,
    #[cfg_attr(not(test), allow(dead_code))]
    label: egui::Rect,
}

/// A bar item: icon above label, one click target (plan §1.13). Painted
/// rather than a `selectable_label`, which lays its text on one line.
fn bar_item(ui: &mut egui::Ui, selected: bool, (icon, label): (&str, &str)) -> BarItemDraw {
    let icon_font = egui::TextStyle::Button.resolve(ui.style());
    let label_font = egui::TextStyle::Small.resolve(ui.style());
    let icon_galley =
        ui.painter()
            .layout_no_wrap(icon.to_owned(), icon_font, egui::Color32::PLACEHOLDER);
    let label_galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), label_font, egui::Color32::PLACEHOLDER);
    let size = egui::vec2(
        icon_galley.size().x.max(label_galley.size().x) + 2.0 * BAR_ITEM_PADDING.x,
        icon_galley.size().y + BAR_ITEM_GAP + label_galley.size().y + 2.0 * BAR_ITEM_PADDING.y,
    );
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() || response.has_focus() {
        let mut visuals = visuals;
        if selected {
            visuals.weak_bg_fill = ui.visuals().selection.bg_fill;
        }
        ui.painter().rect(
            rect,
            visuals.corner_radius,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }

    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.center().x - icon_galley.size().x / 2.0,
            rect.top() + BAR_ITEM_PADDING.y,
        ),
        icon_galley.size(),
    );
    let label_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.center().x - label_galley.size().x / 2.0,
            icon_rect.bottom() + BAR_ITEM_GAP,
        ),
        label_galley.size(),
    );
    ui.painter()
        .galley(icon_rect.min, icon_galley, visuals.text_color());
    ui.painter()
        .galley(label_rect.min, label_galley, visuals.text_color());

    BarItemDraw {
        response,
        icon: icon_rect,
        label: label_rect,
    }
}

/// Which page the sheet is showing — derived, never stored: a reading of the
/// shared open-surface flags, in a fixed priority order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SheetPage {
    Feature,
    Time,
    Catalog,
    Menu,
    Inspector,
    Layers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SheetExtent {
    Half,
    Full,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BottomBarProbe {
    pub rect: egui::Rect,
    pub menu: (egui::Rect, bool),
    pub layers: (egui::Rect, bool),
    pub pane: (egui::Rect, bool),
    pub app: (egui::Rect, bool),
    pub icon_label: [(egui::Rect, egui::Rect); 4],
    /// The Live/timestamp chip, and whether the inline transport was
    /// expanded when it drew. [`egui::Rect::NOTHING`] when the bar had no room.
    /// room for it — the chip hides rather than overlap the items.
    pub live_chip: (egui::Rect, bool),
}

#[cfg(test)]
impl Default for BottomBarProbe {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            menu: (egui::Rect::NOTHING, false),
            layers: (egui::Rect::NOTHING, false),
            pane: (egui::Rect::NOTHING, false),
            app: (egui::Rect::NOTHING, false),
            icon_label: [(egui::Rect::NOTHING, egui::Rect::NOTHING); 4],
            live_chip: (egui::Rect::NOTHING, false),
        }
    }
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SheetProbe {
    pub rect: egui::Rect,
    pub page: Option<SheetPage>,
    pub title: String,
    pub close: egui::Rect,
    pub handle: egui::Rect,
    pub extent: SheetExtent,
}

#[cfg(test)]
impl Default for SheetProbe {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            page: None,
            title: String::new(),
            close: egui::Rect::NOTHING,
            handle: egui::Rect::NOTHING,
            extent: SheetExtent::Half,
        }
    }
}

/// What the phone error toast drew last frame — `None` while no toast was on
/// screen.
/// bar's slot instead.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ErrorToastProbe {
    pub rect: egui::Rect,
    pub close: egui::Rect,
}

impl super::Gui {
    pub(crate) fn top_sheet_page(&self) -> Option<SheetPage> {
        if !self.overlays.selected_overlays.is_empty() {
            return Some(SheetPage::Feature);
        }
        if self.time_dialog.show {
            return Some(SheetPage::Time);
        }
        if self.catalog_open {
            return Some(SheetPage::Catalog);
        }
        if self.menu_open {
            return Some(SheetPage::Menu);
        }
        if self.insp_open {
            return Some(SheetPage::Inspector);
        }
        if self.drawer_open {
            return Some(SheetPage::Layers);
        }
        None
    }

    /// Clear every page flag — what the × and a drag-down dismissal mean:
    /// the whole sheet goes, not one page.
    pub(super) fn clear_sheet_pages(&mut self) {
        self.overlays.selected_overlays.clear();
        self.overlays.selected_overlay_page = 0;
        self.time_dialog.show = false;
        self.catalog_open = false;
        self.menu_open = false;
        self.insp_open = false;
        self.inspector_sel = InspectorSelection::AppSettings;
        self.drawer_open = false;
    }

    fn clear_sheet_dialogs(&mut self) {
        self.overlays.selected_overlays.clear();
        self.overlays.selected_overlay_page = 0;
        self.time_dialog.show = false;
        self.catalog_open = false;
    }

    /// A bar item's switch step: the dialogs above and every *other* bar page
    /// yield, so the bar's four pages are mutually exclusive. The inspector's
    /// selection is left alone: a switch is not a dismissal.
    fn switch_to_bar_page(&mut self) {
        self.clear_sheet_dialogs();
        self.menu_open = false;
        self.insp_open = false;
        self.drawer_open = false;
    }

    /// The floating bottom bar (plan §1.13). Returns the bar's top edge, so
    /// the inline timeline and the sheet can sit above it.
    pub(super) fn render_bottom_bar(&mut self, ctx: &egui::Context, map_rect: egui::Rect) -> f32 {
        #[cfg(test)]
        let mut probe = BottomBarProbe::default();

        // The fade (§1.8): the phone bottom cluster fades with the rest of
        // the floating chrome. Fully faded it does not render at all.
        let Some(fade) = self.chrome_fade() else {
            #[cfg(test)]
            {
                self.probes.last_bottom_bar = probe;
            }
            return map_rect.bottom();
        };

        let page = self.top_sheet_page();
        let mut frame = super::shell::chrome_frame(&ctx.global_style());
        frame.corner_radius.sw = 0;
        frame.corner_radius.se = 0;
        frame.outer_margin = egui::Margin::ZERO;
        let inner_width = map_rect.width() - frame.inner_margin.sum().x - 2.0 * frame.stroke.width;

        let area = egui::Area::new(egui::Id::new("bottom_bar"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(map_rect.left_bottom())
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    super::fade::dim(ui, fade);
                    ui.set_width(inner_width);
                    ui.horizontal(|ui| {
                        let on = page == Some(SheetPage::Menu);
                        let item = bar_item(ui, on, MENU_ITEM);
                        #[cfg(test)]
                        {
                            probe.menu = (item.response.rect, on);
                            probe.icon_label[0] = (item.icon, item.label);
                        }
                        if item.response.clicked() {
                            if on {
                                self.clear_sheet_pages();
                            } else {
                                self.switch_to_bar_page();
                                self.menu_open = true;
                            }
                        }

                        let on = page == Some(SheetPage::Layers);
                        let item = bar_item(ui, on, LAYERS_ITEM);
                        #[cfg(test)]
                        {
                            probe.layers = (item.response.rect, on);
                            probe.icon_label[1] = (item.icon, item.label);
                        }
                        if item.response.clicked() {
                            if on {
                                self.clear_sheet_pages();
                            } else {
                                self.switch_to_bar_page();
                                self.drawer_open = true;
                            }
                        }

                        let on = page == Some(SheetPage::Inspector)
                            && self.inspector_sel == InspectorSelection::PaneProps;
                        let item = bar_item(ui, on, PANE_ITEM);
                        #[cfg(test)]
                        {
                            probe.pane = (item.response.rect, on);
                            probe.icon_label[2] = (item.icon, item.label);
                        }
                        if item.response.clicked() {
                            if on {
                                self.clear_sheet_pages();
                            } else {
                                self.switch_to_bar_page();
                                self.select_pane_props();
                            }
                        }

                        let on = page == Some(SheetPage::Inspector)
                            && self.inspector_sel == InspectorSelection::AppSettings;
                        let item = bar_item(ui, on, APP_ITEM);
                        #[cfg(test)]
                        {
                            probe.app = (item.response.rect, on);
                            probe.icon_label[3] = (item.icon, item.label);
                        }
                        if item.response.clicked() {
                            if on {
                                self.clear_sheet_pages();
                            } else {
                                self.switch_to_bar_page();
                                self.open_settings();
                            }
                        }

                        let expanded = !self.timeline_collapsed;
                        let live = self.panes[self.active_pane].viewing_live;
                        let chip_text = if live {
                            "\u{23fa} Live".to_owned()
                        } else {
                            format!("\u{23f1} {}", self.active_time_label())
                        };
                        let chip_font = egui::TextStyle::Button.resolve(ui.style());
                        let chip_width = ui
                            .painter()
                            .layout_no_wrap(
                                chip_text.clone(),
                                chip_font,
                                egui::Color32::PLACEHOLDER,
                            )
                            .size()
                            .x
                            + 2.0 * ui.spacing().button_padding.x
                            + ui.spacing().item_spacing.x;
                        if ui.available_width() >= chip_width {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let chip = ui
                                        .selectable_label(expanded, chip_text)
                                        .on_hover_text("Show or hide the timeline");
                                    #[cfg(test)]
                                    {
                                        probe.live_chip = (chip.rect, expanded);
                                    }
                                    if chip.clicked() {
                                        self.timeline_collapsed = expanded;
                                    }
                                },
                            );
                        }
                    });
                });
            });

        let top = area.response.rect.top();
        #[cfg(test)]
        {
            probe.rect = area.response.rect;
            self.probes.last_bottom_bar = probe;
        }
        #[cfg(not(test))]
        let _ = area;
        top
    }

    /// The sheet: scrim, frame, handle, title row, and whichever page the
    /// projection says is on top.
    pub(super) fn render_phone_sheet(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        bar_top: f32,
        actions: &mut Vec<GuiAction>,
    ) {
        // The rise and fall (§3.3): the open state animates, and the fall
        // keeps rendering the page the flags just closed — remembered in
        // `sheet_last_page`, dead to input — until the slide is off screen.
        let open = self.top_sheet_page();
        let open_factor = ctx.animate_bool_with_time(
            egui::Id::new("sheet_open"),
            open.is_some(),
            super::fade::anim_time(),
        );
        if open.is_some() {
            self.sheet_last_page = open;
        }
        let falling = open.is_none();
        let Some(page) = open.or(if open_factor > 0.0 {
            self.sheet_last_page
        } else {
            None
        }) else {
            self.sheet_drag = None;
            return;
        };
        let Some(fade) = self.chrome_fade() else {
            self.sheet_drag = None;
            return;
        };
        if falling {
            self.sheet_drag = None;
        }

        // Flush on the bar — the gap died with the bar's insets.
        let sheet_bottom = bar_top;
        // The Layers page's floor covers its segments header too — a sheet
        // that cannot cut the pane controls off however far it is dragged.
        let min_height = if page == SheetPage::Layers {
            LAYERS_MIN_SHEET_HEIGHT
        } else {
            MIN_SHEET_HEIGHT
        };
        let avail = (sheet_bottom - map_rect.top() - SHEET_TOP_GAP).max(min_height);
        let half = (map_rect.height() * SHEET_HALF_FRACTION).min(avail);
        let full = (map_rect.height() * SHEET_FULL_FRACTION).min(avail);
        // The catalog is a full-height page (plan §1.10): a half sheet would
        // be a search box with two tiles under it.
        let extent = if page == SheetPage::Catalog {
            SheetExtent::Full
        } else {
            self.sheet_extent
        };
        let base = match extent {
            SheetExtent::Half => half,
            SheetExtent::Full => full,
        };
        let height =
            (base - self.sheet_drag.unwrap_or(0.0)).clamp(min_height, full.max(min_height));
        let sheet_rect = egui::Rect::from_min_max(
            egui::pos2(map_rect.left(), sheet_bottom - height),
            egui::pos2(map_rect.right(), sheet_bottom),
        );
        // The slide itself: at factor zero the surface has travelled its own
        // height down past the bottom edge; in between the whole cluster
        // follows this one rect.
        let sheet_rect = sheet_rect.translate(egui::vec2(0.0, (1.0 - open_factor) * height));

        // The scrim, over what the sheet leaves uncovered above it — not
        // over the bottom bar, which is the way between pages (§1.13). The
        // cluster is sealed: no bare-map sliver for a tap to slip through.
        // Dismissal on Compact stays projection-first (`dismiss_top_layer`).
        let scrim_rect =
            egui::Rect::from_min_max(map_rect.min, egui::pos2(map_rect.right(), sheet_rect.top()));
        // A falling scrim thins with the slide and takes no clicks: the
        // pages are already closed, and a dead scrim eating the tap that
        // dismissed it would read as a stuck UI.
        let scrim_color = SCRIM_COLOR.gamma_multiply(open_factor * fade);
        let scrim_sense = if falling {
            egui::Sense::hover()
        } else {
            egui::Sense::click()
        };
        let scrim = egui::Area::new(egui::Id::new("sheet_scrim"))
            .order(egui::Order::Foreground)
            .fixed_pos(scrim_rect.min)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(scrim_rect.size(), scrim_sense);
                ui.painter().rect_filled(rect, 0.0, scrim_color);
                if response.clicked() {
                    // The backdrop half of the dismissal contract (§1.9):
                    // one layer per click, through the same chain a key
                    // press walks.
                    self.dismiss_top_layer();
                }
            });
        let scrim_layer = scrim.response.layer_id;

        #[cfg(test)]
        let mut probe = SheetProbe {
            page: Some(page),
            extent,
            ..SheetProbe::default()
        };

        // Where the Layers/Inspector page body goes, decided while the sheet
        // frame lays its header out — consumed after the area closes, where
        // the take window may open.
        let mut body_slot: Option<SurfaceSlot> = None;

        let title: String = match page {
            SheetPage::Feature => self
                .feature_page_heading()
                .map(|(title, _)| title)
                .unwrap_or_default(),
            SheetPage::Time => "Set Time".to_owned(),
            SheetPage::Catalog => "Add layer".to_owned(),
            SheetPage::Menu => "Menu".to_owned(),
            SheetPage::Inspector => "Inspector".to_owned(),
            SheetPage::Layers => "Layers".to_owned(),
        };
        #[cfg(test)]
        {
            probe.title = title.clone();
        }

        let mut frame = super::shell::chrome_frame(&ctx.global_style());
        frame.corner_radius = egui::CornerRadius {
            nw: 12,
            ne: 12,
            sw: 0,
            se: 0,
        };
        // Everything the frame adds around the content: margins *and* the
        // stroke, which egui 0.35 lays outside the inner margin — without it
        // the sheet overhangs the map by a stroke width each side.
        let margin = frame.inner_margin.sum()
            + frame.outer_margin.sum()
            + egui::vec2(2.0, 2.0) * frame.stroke.width;

        let area = egui::Area::new(egui::Id::new("phone_sheet"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_TOP)
            .fixed_pos(sheet_rect.min)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    super::fade::dim(ui, fade);
                    if falling {
                        // The fall is a slide, not a dim — but the falling
                        // remnant is already closed in state, so its widgets
                        // are dead whatever the slide still shows.
                        ui.disable();
                    }
                    ui.set_width(sheet_rect.width() - margin.x);
                    ui.set_min_height(height - margin.y);

                    let (handle_rect, handle) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), HANDLE_HEIGHT),
                        egui::Sense::drag(),
                    );
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(handle_rect.center(), HANDLE_BAR),
                        2.0,
                        ui.visuals().weak_text_color(),
                    );
                    #[cfg(test)]
                    {
                        probe.handle = handle_rect;
                    }
                    if handle.dragged() {
                        *self.sheet_drag.get_or_insert(0.0) += handle.drag_delta().y;
                    }
                    if handle.drag_stopped() {
                        let released = base - self.sheet_drag.take().unwrap_or(0.0);
                        self.sheet_snap(released, half, full, page == SheetPage::Catalog);
                    } else if !handle.dragged() && self.sheet_drag.is_some() {
                        // A cancelled touch reports no release, ever — the
                        // scrubber's own rule: the cancel behaves like a
                        // cancel, and the sheet springs back to its snap.
                        self.sheet_drag = None;
                    }

                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let close = ui.button(CLOSE_LABEL).on_hover_text("Close the sheet");
                            #[cfg(test)]
                            {
                                probe.close = close.rect;
                            }
                            if close.clicked() {
                                self.clear_sheet_pages();
                            }
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let text = egui::RichText::new(title.as_str()).strong();
                                    let text = match page {
                                        SheetPage::Feature => match self.feature_page_heading() {
                                            Some((_, accent)) => text.color(accent),
                                            None => text,
                                        },
                                        _ => text,
                                    };
                                    ui.add(egui::Label::new(text).truncate());
                                },
                            );
                        });
                    });

                    // The phone-only Panes/Pane header (plan §1.3): the
                    // phone top bar has no segments, so the Layers page
                    // carries them — the top bar's own renderer.
                    if page == SheetPage::Layers {
                        ui.horizontal(|ui| self.render_pane_segments(ui, true));
                    }
                    ui.separator();

                    let body_top = ui.cursor().top();
                    let body_max = (sheet_rect.bottom() - margin.y / 2.0 - body_top).max(0.0);
                    match page {
                        SheetPage::Layers | SheetPage::Inspector => {
                            body_slot = Some(SurfaceSlot {
                                pos: egui::pos2(sheet_rect.left() + BODY_INSET, body_top),
                                pivot: egui::Align2::LEFT_TOP,
                                width: sheet_rect.width() - 2.0 * BODY_INSET,
                                avail_height: body_max,
                                sheet: true,
                                opacity: fade,
                                interactive: !falling,
                            });
                        }
                        SheetPage::Menu => {
                            self.render_sheet_menu(ui, body_max, actions);
                        }
                        SheetPage::Catalog => {
                            #[cfg(test)]
                            let mut catalog_probe = super::CatalogProbe {
                                open: true,
                                ..super::CatalogProbe::default()
                            };
                            self.render_catalog_body(
                                ui,
                                (body_max - CATALOG_HEADER_ALLOWANCE).max(120.0),
                                actions,
                                #[cfg(test)]
                                &mut catalog_probe,
                            );
                            #[cfg(test)]
                            {
                                catalog_probe.rect = ui.min_rect();
                                self.probes.last_catalog = catalog_probe;
                            }
                        }
                        SheetPage::Time => {
                            if let Some(action) = self.render_time_dialog_body(ui) {
                                actions.push(action);
                            }
                        }
                        SheetPage::Feature => {
                            // A deliberate exception to the id doctrine, with
                            // `catalog_search`/`catalog_scroll` (see
                            // `render_catalog_body`): the salt is stable, but
                            // the parent layer differs either side of 600 pt, so
                            // egui-side scroll state does not carry across it.
                            egui::ScrollArea::vertical()
                                .scroll_source(super::shell::panel_scroll_source())
                                .id_salt("sheet_feature_scroll")
                                .max_height(body_max)
                                .show(ui, |ui| {
                                    self.render_feature_page_body(ui);
                                });
                        }
                    }
                });
            });
        // The sheet stacks directly over the scrim, wherever the sticky
        // area order has the scrim — see the module note on layering.
        let sheet_layer = area.response.layer_id;
        ctx.set_sublayer(scrim_layer, sheet_layer);

        #[cfg(test)]
        {
            probe.rect = area.response.rect;
            self.probes.last_sheet = probe;
        }
        #[cfg(not(test))]
        let _ = area;

        // The Layers/Inspector body, through the same take window the shell
        // opens on the wider widths — see `ui_shell.rs` for the discipline.
        if let Some(slot) = body_slot {
            let mut pane = std::mem::take(&mut self.panes[self.active_pane]);
            if pane.has_slot_configs() {
                self.overlays.load_pane_configs(&pane.slot_config_map());
            }
            let body_id = match page {
                SheetPage::Layers => {
                    let statuses = self.stack_row_statuses(&pane);
                    self.render_stack(ctx, slot, &mut pane, &statuses, actions);
                    "layers_panel"
                }
                _ => {
                    self.render_inspector(ctx, slot, &mut pane, actions);
                    "inspector_panel"
                }
            };
            self.panes[self.active_pane] = pane;
            self.propagate_layer_sync();
            // And the body directly over the sheet — the third link of the
            // scrim → sheet → body chain.
            ctx.set_sublayer(
                sheet_layer,
                egui::LayerId::new(egui::Order::Foreground, egui::Id::new(body_id)),
            );
        }
    }

    /// The Menu page: the whole menu model as the drawer list — woken from
    /// its dormancy for exactly this host. Commands close the sheet (the
    /// thing they opened is what the user wants to see); toggles keep it up,
    /// except the armed draw, which closes it for the drag that follows.
    fn render_sheet_menu(
        &mut self,
        ui: &mut egui::Ui,
        body_max: f32,
        actions: &mut Vec<GuiAction>,
    ) {
        let model = self.menu_model();
        let menu_frame = egui::ScrollArea::vertical()
            .scroll_source(super::shell::panel_scroll_source())
            .id_salt("sheet_menu_scroll")
            .max_height(body_max)
            .show(ui, |ui| ui_menu::render_menu_drawer(ui, &model))
            .inner;
        #[cfg(test)]
        self.probes
            .last_menu_leaves
            .extend(menu_frame.drawn.iter().copied());

        let mut close = false;
        for event in menu_frame.events {
            close |= matches!(event, ui_menu::MenuEvent::Invoked(_))
                || matches!(
                    event,
                    ui_menu::MenuEvent::Toggled(
                        ui_menu::MenuToggle::DrawCrossSection | ui_menu::MenuToggle::PickRegion,
                        true,
                    )
                );
            self.apply_menu_event(event, actions);
        }
        if close {
            self.menu_open = false;
        }
    }

    /// Snap the released sheet to Full, Half, or gone — the release decides
    /// what the drag meant; a "gone" release falls through the sheet's own
    /// close animation like every other dismissal.
    fn sheet_snap(&mut self, released_height: f32, half: f32, full: f32, forced_full: bool) {
        if released_height < half * SHEET_DISMISS_FRACTION {
            self.clear_sheet_pages();
        } else if forced_full {
            // Not a dismissal, and nothing else to decide: the page ignores
            // the snap, so the snap keeps the user's last real choice.
        } else if released_height > (half + full) / 2.0 {
            self.sheet_extent = SheetExtent::Full;
        } else {
            self.sheet_extent = SheetExtent::Half;
        }
    }

    /// The phone's error surface: a small banner under the top bar, with the
    /// status bar's own dismissable body. The phone shell has no status bar
    /// to host the error slot.
    pub(super) fn render_phone_error_toast(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        carries: bool,
    ) {
        let present = carries && self.radar.error_message.is_some();
        let factor = ctx.animate_bool_with_time(
            egui::Id::new("error_toast_open"),
            present,
            super::fade::anim_time(),
        );
        if present {
            self.toast_last_error = self.radar.error_message.clone();
        }
        if factor <= 0.0 {
            // Fully off screen: forget the remnant so a much later fall can
            // never resurrect a long-dismissed message.
            self.toast_last_error = None;
            return;
        }
        // What the banner shows: the live message, or the fall's remembered
        // one — the remnant copy is what `render_error_display` mutates.
        // dismiss, so the real state is only ever written back from its live copy.
        let mut shown = if present {
            self.radar.error_message.clone()
        } else {
            self.toast_last_error.clone()
        };
        // `Order::Tooltip`, so the toast reads over the sheet cluster: the
        // scrim, sheet and hosted bodies are all `Order::Foreground`, and an
        // error banner under a scrim is an error the user can neither see
        // nor dismiss. A higher order rather than a splice into the chain,
        // which only exists while a page is open, and rather than a per-frame
        // `move_to_top` at Foreground, which would pin the toast above the
        // combo popups the sheet bodies open.
        let area = egui::Area::new(egui::Id::new("phone_error_toast"))
            .order(egui::Order::Tooltip)
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(egui::pos2(
                map_rect.center().x,
                map_rect.top() + TOAST_INSET,
            ))
            .show(ctx, |ui| {
                super::shell::chrome_frame(&ctx.global_style())
                    .show(ui, |ui| {
                        super::fade::dim(ui, factor);
                        ui.horizontal(|ui| super::statusbar::render_error_display(ui, &mut shown))
                            .inner
                    })
                    .inner
            });
        if present {
            self.radar.error_message = shown;
        }
        #[cfg(test)]
        {
            self.probes.last_error_toast = area.inner.map(|close| ErrorToastProbe {
                rect: area.response.rect,
                close,
            });
        }
        #[cfg(not(test))]
        let _ = area;
    }
}
