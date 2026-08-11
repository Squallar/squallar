//! Per-pane pill rows and their popovers — the in-pane pane controls
//! (plan §1.7, §3.5), and the shared picker bodies behind them.
//!
//! One `egui::Area` per visible pane, top-left, wrapping to the pane's width:
//! pane number · site · product code · tilt (map panes only) · sync ·
//! kind. Every pill click makes its pane the active pane first; the pills
//! with something to choose then open an [`egui::Popup`] anchored under
//! themselves, closing on a click outside — the M1 dropdown's own pattern.
//!
//! # The row idles translucent, and opacity is cosmetic
//!
//! The row draws at [`PILL_IDLE_OPACITY`] until something says the user is
//! looking at it: the "Pin pane controls" setting, an open popover, a mouse
//! pointer over the pane, or — on touch, where there is no hover — a first
//! tap on the dim row, which reveals it and is deliberately swallowed (no
//! pill acts on it). Opacity is `Ui::set_opacity`, which changes painting and
//! nothing else, so a dim row still hit-tests: that is what makes the
//! hover- and tap-reveal possible at all. The reveal clears when a map click
//! switches the active pane or a confirmed map tap lands anywhere (both in
//! `ui_map.rs`, where those gestures are resolved).
//!
//! # Why the pills are `Area`s, not painted chrome
//!
//! An `Area` above `Order::Background` is caught by the same layer check
//! every map click resolver already runs (`filter_dialog_blocked`,
//! `is_pos_blocked`), so a click on a pill can never also be a map click —
//! no `excluded_rects` plumbing, no frame-ordering knot from reporting rects
//! before `render_panes` (plan §3.3's resolution). The pass runs from
//! [`Gui::ui`](super::Gui::ui) **after** the pane loop and the pending
//! appliers, outside every `mem::take` window, which is what lets a popover
//! selection write `self.panes[idx]` directly.
//!
//! # The panels stack above the rows
//!
//! Same-order egui areas paint in registration order, *and* egui auto-tops
//! every area on its debut frame (`!visible_last_frame`) — so a debuting
//! pill row lands above the layers panel and the inspector: at startup,
//! where the pills register after the shell's panels, and again whenever
//! the pane count grows mid-session (the top bar's Panes segment, a preset
//! apply, a drawn section line or region). The pills pass therefore tracks
//! which rows the last frame drew, and every debut re-arms a raise of
//! whichever panels are open (`move_to_top`, deferred past the debut frame;
//! see the call site), while a panel opened later is appended above the
//! pills by egui itself. After that, egui's ordinary click-to-front governs
//! the overlap, exactly as it does between two windows.
//!
//! # Shared pickers: parity by construction
//!
//! The list bodies here — [`site_list_ui`], [`product_list_ui`],
//! [`tilt_list_ui`], [`kind_list_ui`] — are the same functions the
//! inspector's Pane-properties body renders its site list and its
//! product/tilt combo contents from, and both kind pickers choose through
//! [`Gui::pick_pane_kind`](super::Gui::pick_pane_kind). The popovers and the
//! inspector cannot offer different inventories, because there is only one
//! inventory to render.

use crate::actions::GuiAction;
use crate::pane::{PaneId, PaneKind};
use crate::ui_layout::PointerModality;
use rustdar_radar::types::RadarProduct;

/// The row's idle opacity. The reveal animates between this and 1.0
/// (`Gui::render_pill_row`'s `animate_bool_with_time`; instant under test —
/// `ui_fade::anim_time`).
const PILL_IDLE_OPACITY: f32 = 0.35;

/// The row's inset from its pane's top-left corner, both axes.
const PILL_INSET: f32 = 8.0;

/// What a pane's own top-left content must leave clear for the pill row:
/// the inset, one row of buttons, and a gap. The section pane's caption
/// (with its clickable ⓘ) and the 3D pane's caption both start below this —
/// content under the row would be covered, and the ⓘ in particular would be
/// unclickable, since the row is an egui layer above the pane.
///
/// One row's worth — the floor. A narrow pane wraps its pill row to two or
/// more lines (`horizontal_wrapped`), so the honest clearance is measured:
/// see [`pill_row_clearance`], which every caption site reads instead of
/// this constant directly (the second user test's pane-6 collision).
pub(crate) const PILL_ROW_CLEARANCE: f32 = 40.0;

/// The clearance pane `idx`'s own top-left content must keep this frame:
/// the pill row's *measured* height — its area rect as of the last frame,
/// which egui remembers — plus the inset and a gap, floored at the one-row
/// [`PILL_ROW_CLEARANCE`]. One frame stale by construction (the pane draws
/// before the pills pass), which for a wrap decision is harmless: a pane's
/// width changes reflow both within a frame of each other.
pub(crate) fn pill_row_clearance(ctx: &egui::Context, idx: PaneId) -> f32 {
    ctx.memory(|m| m.area_rect(egui::Id::new(("pane_pills", idx))))
        .map(|rect| rect.height() + PILL_INSET + 6.0)
        .map_or(PILL_ROW_CLEARANCE, |h| h.max(PILL_ROW_CLEARANCE))
}

/// The site popover's minimum width — room for the search field and the
/// `XXXX · TDWR` rows without wrapping.
const SITE_POPOVER_WIDTH: f32 = 220.0;

/// The link popover's width ceiling, so [`UNLINK_NOTE`] wraps instead of
/// stretching the popup across the pane.
const LINK_POPOVER_WIDTH: f32 = 260.0;

/// The site list's height, in the inspector body and the popover alike:
/// enough rows to scan, small enough that what is under the list stays in
/// reach without scrolling past 200 sites.
const SITE_LIST_HEIGHT: f32 = 150.0;

/// The Sync pill's text while all three of this pane's links are on. Plain
/// text — the old `⛓` rendered like a DNA helix (the second user test), and
/// the demo's `🔗` has no glyph in egui's bundled fonts (see `ui_glyphs.rs`).
const SYNC_PILL_LINKED: &str = "Sync";

/// The Sync pill's text while **any** of this pane's three links is off —
/// the `⊗` marks a pane sitting out part of the group at a glance, and the
/// pill's hover names which parts (see [`sync_pill_hover`]).
const SYNC_PILL_UNLINKED: &str = "\u{2297} Sync";

/// The Sync popover's three toggles — all honestly per-pane since M11:
/// this pane's own [`crate::pane::PaneState::viewport_link`], `layer_link`
/// and `time_link`, under one label set the inspector's Pane-properties sync
/// section shares by rendering the same [`sync_section_ui`].
pub(super) const SYNC_VIEWPORT_OPTION: &str = "Sync viewport";
pub(super) const SYNC_LAYERS_OPTION: &str = "Sync layers";
pub(super) const SYNC_TIME_OPTION: &str = "Sync time";

/// The two action rows under the toggles — the ways home. The first copies
/// this pane's viewport everywhere and touches no link; the second is that
/// copy plus every link on every visible pane turned back on.
pub(super) const SYNC_MATCH_ALL: &str = "Match all panes to this view";
pub(super) const SYNC_RELINK_ALL: &str = "Re-link all here";

/// The sync section's five row labels in draw order — the parity walk's
/// inventory for the one section [`sync_section_ui`] renders at both routes.
#[cfg(test)]
pub(crate) const SYNC_SECTION_LABELS: [&str; 5] = [
    SYNC_VIEWPORT_OPTION,
    SYNC_LAYERS_OPTION,
    SYNC_TIME_OPTION,
    SYNC_MATCH_ALL,
    SYNC_RELINK_ALL,
];

/// What unlinking time really does — one sentence for the section's caption,
/// rendered by [`sync_section_ui`] in the popover and the inspector alike so
/// the two routes cannot describe the setting differently. Careful about
/// "frozen": shared time navigation and the loop leave the pane alone, and a
/// pane parked in the archive therefore holds its moment — but scan delivery
/// is per-site, so an unlinked pane still watching live still follows new
/// scans.
pub(crate) const UNLINK_NOTE: &str = "Off leaves this pane out of shared time \
    navigation and the loop. Parked in the archive it holds its moment; \
    still live, it still follows new scans.";

/// The viewport toggle's hover — brief, because the behaviour is the
/// symmetric one the label implies: off means this pane pans and zooms
/// alone, and the group moves without it.
const VIEWPORT_LINK_NOTE: &str = "Off lets this pane pan and zoom alone; \
    the other linked panes keep moving together.";

/// The layers toggle's hover — what "layers" covers here, so off is not
/// mistaken for the eye toggles alone.
const LAYER_LINK_NOTE: &str = "Off keeps this pane's site, product, tilt and \
    layers its own; linked panes keep converging without it.";

/// The match-all action's hover: the copy, and the promise that it is only
/// the copy.
const MATCH_ALL_NOTE: &str = "Copy this pane's zoom and centre to every map \
    pane. Links stay as they are.";

/// The re-link action's hover: the same copy, plus the three links turned
/// back on everywhere.
const RELINK_ALL_NOTE: &str = "Copy this view to every map pane and turn \
    viewport, layer and time sync back on for every pane.";

/// The three pane kinds as the pickers offer them — the inspector's
/// segmented row and the kind popover render this one table.
const PANE_KIND_OPTIONS: [(PaneKind, &str); 3] = [
    (PaneKind::Map, "Map"),
    (PaneKind::Volume, "3D Volume"),
    (PaneKind::CrossSection, "Cross-section"),
];

/// Which pill of a row something is — the popover ids salt on this, and the
/// probes name pills by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PillKind {
    /// The pane number. Activates; no popover — so production only ever
    /// constructs this variant in the probes, which are `cfg(test)`.
    #[cfg_attr(not(test), allow(dead_code))]
    Number,
    /// The site code → search + full site list.
    Site,
    /// The product code → the scan's product list.
    Product,
    /// The tilt → the product's elevation list. Map panes only.
    Tilt,
    /// The time-link glyph → follow / unlink pair.
    Link,
    /// The kind label → Map / 3D Volume / Cross-section.
    Kind,
}

/// The popover pills, in row order — what the reveal check asks "is one of
/// this pane's popovers open?" over.
const POPOVER_PILLS: [PillKind; 5] = [
    PillKind::Site,
    PillKind::Product,
    PillKind::Tilt,
    PillKind::Link,
    PillKind::Kind,
];

/// The popup id for pane `idx`'s `pill` popover. Salted on the pane index
/// and the pill — never on the width, per the id contract.
fn pill_popup_id(idx: PaneId, pill: PillKind) -> egui::Id {
    egui::Id::new(("pill_popup", idx, pill))
}

/// One pane's pill row, as it was drawn — reported by the renderer, never
/// rebuilt by a test; see `ui_menu::DrawnMenuLeaf` for the pattern.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PillRowProbe {
    pub pane_idx: usize,
    /// The area's whole rect, off its own response.
    pub rect: egui::Rect,
    /// Every pill drawn, in row order: which, the text it showed, and where
    /// it landed so a test can click it.
    pub pills: Vec<(PillKind, String, egui::Rect)>,
    /// Whether the row drew at full opacity this frame — the reveal
    /// decision as the renderer took it.
    pub full_opacity: bool,
}

/// The pill popover the last frame drew, if one was open.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PillPopoverProbe {
    pub pane_idx: usize,
    pub pill: PillKind,
    /// The popup's whole rect — what "anchored to its pill" is asserted on.
    pub rect: egui::Rect,
    /// The search field, on the site popover only.
    pub search: Option<egui::Rect>,
    /// The option rows drawn: label, rect, and whether the row read as the
    /// current selection.
    pub rows: Vec<(String, egui::Rect, bool)>,
}

/// What one shared picker pass produced: the option the user picked, if any,
/// and — for the probes — the rows as they were drawn.
pub(super) struct PickOutcome<T> {
    pub picked: Option<T>,
    #[cfg(test)]
    pub rows: Vec<(String, egui::Rect, bool)>,
}

impl<T> Default for PickOutcome<T> {
    fn default() -> Self {
        Self {
            picked: None,
            #[cfg(test)]
            rows: Vec::new(),
        }
    }
}

impl<T> PickOutcome<T> {
    /// Record one drawn row and fold its click into the outcome. Clicking
    /// the current selection means nothing, as it does on the map's icons.
    fn row(&mut self, ui: &mut egui::Ui, label: &str, selected: bool, value: T) {
        let row = ui.selectable_label(selected, label);
        #[cfg(test)]
        self.rows.push((label.to_owned(), row.rect, selected));
        if row.clicked() && !selected {
            self.picked = Some(value);
        }
    }
}

/// [`PickOutcome`] plus the site list's count caption, which the inspector's
/// probe records verbatim.
pub(super) struct SiteListOutcome {
    pub picked: Option<String>,
    #[cfg(test)]
    pub rows: Vec<(String, egui::Rect, bool)>,
    #[cfg(test)]
    pub caption: String,
}

/// The filterable site list over the full compiled-in table: count caption,
/// then a scrolling list with the current site highlighted and TDWRs marked.
/// Returns the site a click picked — always a site other than `current`.
///
/// **The** site list: the inspector's Pane-properties body and the site
/// pill's popover both render this, which is what keeps the two routes one
/// inventory (module note).
pub(super) fn site_list_ui(ui: &mut egui::Ui, query: &str, current: &str) -> SiteListOutcome {
    use rustdar_radar::sites::RADARS;

    // The codes are the table's names; uppercased so a lowercase query
    // still finds them.
    let query = query.trim().to_uppercase();
    let shown: Vec<&rustdar_radar::sites::RadarSite> = RADARS
        .iter()
        .filter(|site| query.is_empty() || site.name.contains(query.as_str()))
        .collect();

    // Computed from the table, not restated: the split is the caption's
    // claim, and a hardcoded count would outlive an edit.
    let total = RADARS.len();
    let tdwr = RADARS.iter().filter(|site| site.is_tdwr()).count();
    let caption = format!(
        "{} shown - {} sites ({} NEXRAD + {} TDWR)",
        shown.len(),
        total,
        total - tdwr,
        tdwr
    );
    ui.label(egui::RichText::new(caption.as_str()).small().weak());

    let mut outcome = SiteListOutcome {
        picked: None,
        #[cfg(test)]
        rows: Vec::new(),
        #[cfg(test)]
        caption,
    };
    #[cfg(not(test))]
    let _ = caption;

    egui::ScrollArea::vertical()
        .scroll_source(super::shell::panel_scroll_source())
        .id_salt("site_list")
        .max_height(SITE_LIST_HEIGHT)
        .show(ui, |ui| {
            for site in shown {
                let is_current = current == site.name;
                // TDWRs are marked in the row: they are pickable — the map
                // icons allow them too — but the Level II archive has
                // nothing for them, and the caption's split deserves to be
                // visible per row.
                let label = if site.is_tdwr() {
                    format!("{} - TDWR", site.name)
                } else {
                    site.name.to_owned()
                };
                let row = ui.selectable_label(is_current, label.as_str());
                #[cfg(test)]
                outcome
                    .rows
                    .push((site.name.to_owned(), row.rect, is_current));
                if row.clicked() && !is_current {
                    outcome.picked = Some(site.name.to_owned());
                }
            }
        });
    outcome
}

/// The product list a scan offers, current one highlighted. Rendered by the
/// inspector's product combo body and the product pill's popover alike.
pub(super) fn product_list_ui(
    ui: &mut egui::Ui,
    options: &[RadarProduct],
    current: RadarProduct,
) -> PickOutcome<RadarProduct> {
    let mut outcome = PickOutcome::default();
    for &product in options {
        outcome.row(ui, product.name(), product == current, product);
    }
    outcome
}

/// The tilt list a product offers, current one highlighted — the same exact
/// equality the combo's `selectable_value` used, so the two routes agree
/// about which row reads selected.
pub(super) fn tilt_list_ui(
    ui: &mut egui::Ui,
    elevations: &[f32],
    current: f32,
) -> PickOutcome<f32> {
    let mut outcome = PickOutcome::default();
    for &angle in elevations {
        outcome.row(ui, &format!("{:.1}\u{b0}", angle), angle == current, angle);
    }
    outcome
}

/// The three pane kinds, current one highlighted — [`PANE_KIND_OPTIONS`]
/// rendered wherever the caller's layout puts the rows (the inspector lays
/// them in a horizontal run, the popover stacks them).
pub(super) fn kind_list_ui(ui: &mut egui::Ui, current: PaneKind) -> PickOutcome<PaneKind> {
    let mut outcome = PickOutcome::default();
    for (kind, label) in PANE_KIND_OPTIONS {
        outcome.row(ui, label, kind == current, kind);
    }
    outcome
}

/// What one pass of [`sync_section_ui`] produced: which action row was
/// clicked, whether the layer link was just turned **on** (the popover
/// converges immediately on that instead of waiting for the next panel
/// pass), and — for the probes — the five rows as drawn, checkbox rows with
/// the state they were handed (the `DrawnMenuLeaf` discipline) and action
/// rows with `false`.
#[derive(Default)]
pub(crate) struct SyncSectionOutcome {
    pub layer_relinked: bool,
    pub match_all: bool,
    pub relink_all: bool,
    #[cfg(test)]
    pub rows: Vec<(String, egui::Rect, bool)>,
}

/// **The** per-pane sync section (M11): the three link checkboxes — all
/// honestly per-pane, writing `pane`'s own fields — and the two action rows.
///
/// The Sync pill's popover and the inspector's Pane-properties sync section
/// both render this one function, which is what keeps the two routes one
/// wording and one behaviour by construction (the module note's parity
/// pattern). It only writes the pane it is handed — the pill popover hands
/// the pane it `mem::take`s, the inspector the pane its whole pass holds —
/// and reports the action clicks for the caller to apply through
/// [`super::Gui::apply_sync_outcome`], where touching the *other* panes is
/// safe.
pub(super) fn sync_section_ui(
    ui: &mut egui::Ui,
    pane: &mut crate::pane::PaneState,
) -> SyncSectionOutcome {
    let mut outcome = SyncSectionOutcome::default();
    #[cfg(test)]
    let push = |rows: &mut Vec<(String, egui::Rect, bool)>,
                label: &str,
                rect: egui::Rect,
                was: bool| rows.push((label.to_owned(), rect, was));

    #[cfg(test)]
    let was = pane.viewport_link;
    let row = ui
        .checkbox(&mut pane.viewport_link, SYNC_VIEWPORT_OPTION)
        .on_hover_text(VIEWPORT_LINK_NOTE);
    #[cfg(test)]
    push(&mut outcome.rows, SYNC_VIEWPORT_OPTION, row.rect, was);
    #[cfg(not(test))]
    let _ = row;

    #[cfg(test)]
    let was = pane.layer_link;
    let row = ui
        .checkbox(&mut pane.layer_link, SYNC_LAYERS_OPTION)
        .on_hover_text(LAYER_LINK_NOTE);
    if row.changed() && pane.layer_link {
        // Rejoining the group converges now, not on the next frame that
        // happens to run a panel pass — the old global toggle's own rule,
        // kept per pane.
        outcome.layer_relinked = true;
    }
    #[cfg(test)]
    push(&mut outcome.rows, SYNC_LAYERS_OPTION, row.rect, was);

    #[cfg(test)]
    let was = pane.time_link;
    let row = ui
        .checkbox(&mut pane.time_link, SYNC_TIME_OPTION)
        .on_hover_text(UNLINK_NOTE);
    #[cfg(test)]
    push(&mut outcome.rows, SYNC_TIME_OPTION, row.rect, was);
    #[cfg(not(test))]
    let _ = row;
    // The honesty sentence rides on screen with the choice, both routes.
    ui.label(egui::RichText::new(UNLINK_NOTE).small().weak());

    ui.separator();
    let row = ui.button(SYNC_MATCH_ALL).on_hover_text(MATCH_ALL_NOTE);
    if row.clicked() {
        outcome.match_all = true;
    }
    #[cfg(test)]
    push(&mut outcome.rows, SYNC_MATCH_ALL, row.rect, false);
    let row = ui.button(SYNC_RELINK_ALL).on_hover_text(RELINK_ALL_NOTE);
    if row.clicked() {
        outcome.relink_all = true;
    }
    #[cfg(test)]
    push(&mut outcome.rows, SYNC_RELINK_ALL, row.rect, false);

    outcome
}

/// The Sync pill's hover: plain "Sync options" while every link is on, and
/// the unlinked dimensions named while any is off — the pill's `⊗` says
/// *something* is out; this says what.
fn sync_pill_hover(viewport_link: bool, layer_link: bool, time_link: bool) -> String {
    let mut off = Vec::new();
    if !viewport_link {
        off.push("viewport");
    }
    if !layer_link {
        off.push("layers");
    }
    if !time_link {
        off.push("time");
    }
    if off.is_empty() {
        "Sync options".to_owned()
    } else {
        format!("Sync options - unlinked: {}", off.join(", "))
    }
}

impl super::Gui {
    /// Ask for pane `idx` to become `kind` the pickers' way: through the
    /// deferred applier, arming the cross-section draw when the pane has no
    /// line to show yet — the same gesture the menu's "Draw cross-section"
    /// entry arms, saving the trip back to the menu.
    ///
    /// One function for the inspector's segmented row and the kind popover,
    /// so the two routes cannot drift about what choosing a kind means.
    pub(super) fn pick_pane_kind(&mut self, idx: PaneId, kind: PaneKind, line_absent: bool) {
        self.request_pane_kind(idx, kind);
        if kind == PaneKind::CrossSection && line_absent {
            self.set_section_draw_armed(true);
        }
    }

    /// Draw every visible pane's pill row. Called from
    /// [`Gui::ui`](super::Gui::ui) after the pane loop and the pending
    /// appliers — outside every `mem::take` window, so the popovers' writes
    /// land on real panes (module note). `map_rect` is the shell's
    /// full-bleed map rect, the same rect `render_panes` laid the grid out
    /// in, so `pane_rect` answers with the rects the panes were drawn at.
    pub(super) fn render_pane_pills(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        actions: &mut Vec<GuiAction>,
    ) {
        // The fade (§1.8): fully faded rows do not render — the absence is
        // the input transparency — and a transition renders dimmed and dead.
        let Some(chrome) = self.chrome_fade() else {
            // The bookkeeping below still owes the panels a raise when the
            // rows return, so the drawn count records the truth: nothing.
            self.pills_drawn_last_frame = 0;
            return;
        };
        let pane_count = self.visible_pane_count();
        for idx in 0..pane_count {
            let pane_rect = self.pane_layout.pane_rect(idx, map_rect);
            self.render_pill_row(ctx, idx, pane_rect, chrome, actions);
        }

        // The deferred panel raise — see the module note. egui auto-tops
        // every debuting area, so any rows' debut frame — startup, and any
        // mid-session pane growth — lands the new rows above whichever
        // panels are open. Deferred one frame past the debut: egui puts
        // every *new* area into its wants-on-top set, and the end-of-pass
        // re-sort is stable — so a raise requested on the debut frame ties
        // with the just-registered rows and loses to their later position.
        // On the next pass the rows are no longer new and the request
        // sticks; egui's ordinary click-to-front governs the overlap from
        // there, exactly as it does between two windows.
        // Off-Compact only: below the breakpoint the panels are sheet-hosted
        // at `Order::Foreground` (the layering note in `ui_sheet.rs`), and
        // raising the Middle ids here would register phantom layers no frame
        // draws.
        if std::mem::take(&mut self.pills_raise_pending)
            && self.layout.width != crate::ui_layout::WidthClass::Compact
        {
            if self.layers_panel_visible() {
                ctx.move_to_top(egui::LayerId::new(
                    egui::Order::Middle,
                    egui::Id::new("layers_panel"),
                ));
            }
            if self.insp_open {
                ctx.move_to_top(egui::LayerId::new(
                    egui::Order::Middle,
                    egui::Id::new("inspector_panel"),
                ));
            }
        }
        // The rows' areas are keyed on contiguous `0..pane_count`, so last
        // frame's count is the set of rows it drew, and a count past it
        // means a debut — which owes the panels a raise on the next pass.
        if pane_count > self.pills_drawn_last_frame {
            self.pills_raise_pending = true;
        }
        self.pills_drawn_last_frame = pane_count;
    }

    /// One pane's pill row and whichever of its popovers is open. `chrome`
    /// is the frame's fade opacity (`Gui::chrome_fade`).
    fn render_pill_row(
        &mut self,
        ctx: &egui::Context,
        idx: PaneId,
        pane_rect: egui::Rect,
        chrome: f32,
        actions: &mut Vec<GuiAction>,
    ) {
        // Everything the row states, read before any closure borrows self.
        let (site, kind, product, links, line_absent, tilt, products, elevations) = {
            let pane = &self.panes[idx];
            let (_, tilt) = pane
                .get_rendering_params()
                .unwrap_or((pane.selected_product, pane.selected_elevation));
            (
                pane.site.clone(),
                pane.kind(),
                pane.selected_product,
                (pane.viewport_link, pane.layer_link, pane.time_link),
                pane.cross_section().and_then(|s| s.line).is_none(),
                tilt,
                pane.scan_info
                    .as_ref()
                    .map(|info| info.available_products.clone()),
                pane.scan_info
                    .as_ref()
                    .and_then(|info| info.product_elevations.get(&pane.selected_product))
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        let is_map = kind == PaneKind::Map;
        // The same gate the inspector's checkbox keeps: with one pane there
        // is no shared time to follow, so the pill would be an option the
        // inspector does not express.
        let offer_link = self.pane_layout.pane_count > 1;

        // The reveal decision, taken before the row draws so the whole row
        // agrees: pinned, a popover open, a mouse pointer over the pane, or
        // a touch reveal already granted. On touch a tap on a still-dim row
        // is swallowed into a reveal (module note).
        let popover_open = POPOVER_PILLS
            .iter()
            .any(|&pill| egui::Popup::is_id_open(ctx, pill_popup_id(idx, pill)));
        let hover_over_pane = ctx
            .pointer_latest_pos()
            .is_some_and(|pos| pane_rect.contains(pos));
        let full = self.pin_pane_controls
            || popover_open
            || match self.layout.modality {
                PointerModality::Mouse => hover_over_pane,
                PointerModality::Touch => self.pill_revealed == Some(idx),
            };
        let swallow = self.layout.modality == PointerModality::Touch && !full;

        #[cfg(test)]
        let mut probe = PillRowProbe {
            pane_idx: idx,
            rect: egui::Rect::NOTHING,
            pills: Vec::new(),
            full_opacity: full,
        };

        // The reveal animates between idle and full opacity (§3.3) — the
        // instant switch it replaces read as a flash. Per-pane id, zero-time
        // under test (`ui_fade::anim_time`).
        let reveal = ctx.animate_bool_with_time(
            egui::Id::new(("pill_reveal", idx)),
            full,
            super::fade::anim_time(),
        );
        let row_opacity = egui::lerp(PILL_IDLE_OPACITY..=1.0, reveal) * chrome;

        let area = egui::Area::new(egui::Id::new(("pane_pills", idx)))
            .order(egui::Order::Middle)
            .fixed_pos(pane_rect.min + egui::vec2(PILL_INSET, PILL_INSET))
            .show(ctx, |ui| {
                // Painting only: a dim row still hit-tests, which is what
                // the hover- and tap-reveal stand on. The fade is the
                // exception (`fade::dim`): a fading row is also dying.
                ui.set_opacity(row_opacity);
                if chrome < 1.0 {
                    ui.disable();
                }
                ui.set_max_width((pane_rect.width() - 2.0 * PILL_INSET).max(40.0));
                ui.horizontal_wrapped(|ui| {
                    // Every pill click below goes through one rule: a tap on
                    // a dim row on touch only reveals it; otherwise the
                    // click activates this pane first, and the pill's own
                    // popover (attached only when not swallowing) does the
                    // rest.

                    // -- pane number: activate, nothing more --
                    let number = ui
                        .button(format!("{}", idx + 1))
                        .on_hover_text("Make this the active pane");
                    #[cfg(test)]
                    probe
                        .pills
                        .push((PillKind::Number, format!("{}", idx + 1), number.rect));
                    if number.clicked() {
                        if swallow {
                            self.pill_revealed = Some(idx);
                        } else {
                            self.active_pane = idx;
                        }
                    }

                    // -- site --
                    let pill = ui.button(site.as_str()).on_hover_text("Radar site");
                    #[cfg(test)]
                    probe.pills.push((PillKind::Site, site.clone(), pill.rect));
                    if pill.clicked() {
                        if swallow {
                            self.pill_revealed = Some(idx);
                        } else {
                            self.active_pane = idx;
                        }
                    }
                    if !swallow {
                        self.site_pill_popover(&pill, idx, &site, actions);
                    }

                    // -- product --
                    let code = product.code().to_uppercase();
                    let pill = ui.button(code.as_str()).on_hover_text("Radar product");
                    #[cfg(test)]
                    probe.pills.push((PillKind::Product, code, pill.rect));
                    if pill.clicked() {
                        if swallow {
                            self.pill_revealed = Some(idx);
                        } else {
                            self.active_pane = idx;
                        }
                    }
                    if !swallow {
                        self.product_pill_popover(&pill, idx, products.as_deref(), product);
                    }

                    // -- tilt, on map panes only: a whole-volume kind reads
                    // the entire ladder, and a section slices it itself --
                    if is_map {
                        let label = format!("{:.1}\u{b0}", tilt);
                        let pill = ui.button(label.as_str()).on_hover_text("Tilt");
                        #[cfg(test)]
                        probe.pills.push((PillKind::Tilt, label, pill.rect));
                        if pill.clicked() {
                            if swallow {
                                self.pill_revealed = Some(idx);
                            } else {
                                self.active_pane = idx;
                            }
                        }
                        if !swallow {
                            self.tilt_pill_popover(&pill, idx, &elevations);
                        }
                    }

                    // -- sync --
                    if offer_link {
                        // Text, with the ⊗ marking a pane with **any** of
                        // its three links off distinctly (the second user
                        // test: the old ⛓ read as a DNA helix). The hover
                        // names which dimensions are out.
                        let (viewport_link, layer_link, time_link) = links;
                        let all_linked = viewport_link && layer_link && time_link;
                        let label = if all_linked {
                            SYNC_PILL_LINKED
                        } else {
                            SYNC_PILL_UNLINKED
                        };
                        let pill = ui.button(label).on_hover_text(sync_pill_hover(
                            viewport_link,
                            layer_link,
                            time_link,
                        ));
                        #[cfg(test)]
                        probe
                            .pills
                            .push((PillKind::Link, label.to_owned(), pill.rect));
                        if pill.clicked() {
                            if swallow {
                                self.pill_revealed = Some(idx);
                            } else {
                                self.active_pane = idx;
                            }
                        }
                        if !swallow {
                            self.sync_pill_popover(&pill, idx);
                        }
                    }

                    // -- kind --
                    let label = match kind {
                        PaneKind::Map => "Map",
                        PaneKind::Volume => "3D Volume",
                        PaneKind::CrossSection => "X-section",
                    };
                    let pill = ui.button(label).on_hover_text("Pane kind");
                    #[cfg(test)]
                    probe
                        .pills
                        .push((PillKind::Kind, label.to_owned(), pill.rect));
                    if pill.clicked() {
                        if swallow {
                            self.pill_revealed = Some(idx);
                        } else {
                            self.active_pane = idx;
                        }
                    }
                    if !swallow {
                        self.kind_pill_popover(&pill, idx, kind, line_absent);
                    }
                });
            });

        #[cfg(test)]
        {
            probe.rect = area.response.rect;
            self.last_pills.push(probe);
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The site popover: search field over the one site list. A pick means
    /// exactly what the map icon and the inspector row mean — the same
    /// `SwitchRadarSite`, with the same in-flight marker on the pane.
    fn site_pill_popover(
        &mut self,
        pill: &egui::Response,
        idx: PaneId,
        current: &str,
        actions: &mut Vec<GuiAction>,
    ) {
        let shown = egui::Popup::menu(pill)
            .id(pill_popup_id(idx, PillKind::Site))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(SITE_POPOVER_WIDTH);
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.site_query)
                        .id_salt("pill_site_query")
                        .hint_text("Search radar sites"),
                );
                let query = self.site_query.clone();
                let outcome = site_list_ui(ui, &query, current);
                #[cfg(test)]
                {
                    self.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Site,
                        rect: egui::Rect::NOTHING,
                        search: Some(search.rect),
                        rows: outcome.rows.clone(),
                    });
                }
                #[cfg(not(test))]
                let _ = search;
                if let Some(picked) = outcome.picked {
                    self.active_pane = idx;
                    let pane = &mut self.panes[idx];
                    pane.loading_site = Some(picked.clone());
                    pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
                    actions.push(GuiAction::SwitchRadarSite {
                        site: picked,
                        pane_idx: idx,
                    });
                    ui.close_kind(egui::UiKind::Menu);
                }
            });
        self.record_popover_rect(&shown);
    }

    /// The product popover: the scan's own product list — the very list the
    /// inspector's combo renders — with a pick written straight to the pane
    /// (this pass runs outside every take window).
    fn product_pill_popover(
        &mut self,
        pill: &egui::Response,
        idx: PaneId,
        options: Option<&[RadarProduct]>,
        current: RadarProduct,
    ) {
        let shown = egui::Popup::menu(pill)
            .id(pill_popup_id(idx, PillKind::Product))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let outcome = match options {
                    Some(options) => product_list_ui(ui, options, current),
                    None => {
                        // The inspector's own wording for the same state.
                        ui.label("No scan loaded");
                        PickOutcome::default()
                    }
                };
                #[cfg(test)]
                {
                    self.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Product,
                        rect: egui::Rect::NOTHING,
                        search: None,
                        rows: outcome.rows.clone(),
                    });
                }
                if let Some(picked) = outcome.picked {
                    self.active_pane = idx;
                    let pane = &mut self.panes[idx];
                    if pane.selected_product != picked {
                        pane.selected_product = picked;
                        // The tilt belonged to the old product — the same
                        // reset the inspector's combo makes.
                        pane.selected_elevation = 0.0;
                    }
                    self.propagate_layer_sync();
                    ui.close_kind(egui::UiKind::Menu);
                }
            });
        self.record_popover_rect(&shown);
    }

    /// The tilt popover: the selected product's elevation list, exactly the
    /// combo's.
    fn tilt_pill_popover(&mut self, pill: &egui::Response, idx: PaneId, elevations: &[f32]) {
        let shown = egui::Popup::menu(pill)
            .id(pill_popup_id(idx, PillKind::Tilt))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let current = self.panes[idx].selected_elevation;
                let outcome = if elevations.is_empty() {
                    // The combo's hover wording for its inert state.
                    ui.label("Waiting for this product's data");
                    PickOutcome::default()
                } else {
                    tilt_list_ui(ui, elevations, current)
                };
                #[cfg(test)]
                {
                    self.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Tilt,
                        rect: egui::Rect::NOTHING,
                        search: None,
                        rows: outcome.rows.clone(),
                    });
                }
                if let Some(angle) = outcome.picked {
                    self.active_pane = idx;
                    self.panes[idx].selected_elevation = angle;
                    self.propagate_layer_sync();
                    ui.close_kind(egui::UiKind::Menu);
                }
            });
        self.record_popover_rect(&shown);
    }

    /// The Sync popover: the shared per-pane sync section —
    /// [`sync_section_ui`], the same function the inspector's Pane-properties
    /// sync section renders — over this pane. Three checkboxes writing this
    /// pane's own links, and the two action rows applied through
    /// [`super::Gui::apply_sync_outcome`]. Checkboxes keep the popover up:
    /// flipping two of them must not be two opens (the ☰ dropdown's own
    /// reasoning); the action rows close it — they are commands, and their
    /// effect is on the whole grid behind the popup.
    ///
    /// The pane is `mem::take`n for the section's duration so the shared
    /// body has one signature at both call sites; this pass runs outside
    /// every other take window (module note), so the slot is free to take.
    fn sync_pill_popover(&mut self, pill: &egui::Response, idx: PaneId) {
        let shown = egui::Popup::menu(pill)
            .id(pill_popup_id(idx, PillKind::Link))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_max_width(LINK_POPOVER_WIDTH);
                let mut pane = std::mem::take(&mut self.panes[idx]);
                let outcome = sync_section_ui(ui, &mut pane);
                self.apply_sync_outcome(&outcome, &mut pane, idx);
                self.panes[idx] = pane;
                if outcome.layer_relinked || outcome.relink_all {
                    // Converge now, not on the next frame that happens to
                    // run a panel pass — after the pane is back in its slot,
                    // so the pass reads the real active pane.
                    self.propagate_layer_sync();
                }
                if outcome.match_all || outcome.relink_all {
                    ui.close_kind(egui::UiKind::Menu);
                }

                #[cfg(test)]
                {
                    self.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Link,
                        rect: egui::Rect::NOTHING,
                        search: None,
                        rows: outcome.rows,
                    });
                }
            });
        self.record_popover_rect(&shown);
    }

    /// The kind popover: the three kinds through [`Gui::pick_pane_kind`] —
    /// deferred conversion, and the unaimed cross-section arms the draw,
    /// matching the inspector's segmented row by construction.
    fn kind_pill_popover(
        &mut self,
        pill: &egui::Response,
        idx: PaneId,
        current: PaneKind,
        line_absent: bool,
    ) {
        let shown = egui::Popup::menu(pill)
            .id(pill_popup_id(idx, PillKind::Kind))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let outcome = kind_list_ui(ui, current);
                #[cfg(test)]
                {
                    self.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Kind,
                        rect: egui::Rect::NOTHING,
                        search: None,
                        rows: outcome.rows.clone(),
                    });
                }
                if let Some(kind) = outcome.picked {
                    self.active_pane = idx;
                    self.pick_pane_kind(idx, kind, line_absent);
                    ui.close_kind(egui::UiKind::Menu);
                }
            });
        self.record_popover_rect(&shown);
    }

    /// Fill in the rect of the popover probe the closure just recorded —
    /// the closure cannot see the popup's own response, and the rect is the
    /// "anchored to its pill" half of the probe.
    fn record_popover_rect(&mut self, _shown: &Option<egui::InnerResponse<()>>) {
        #[cfg(test)]
        if let Some(inner) = _shown
            && let Some(probe) = self.last_pill_popover.as_mut()
        {
            probe.rect = inner.response.rect;
        }
    }
}

#[cfg(test)]
mod clearance_tests {
    use super::{PILL_ROW_CLEARANCE, pill_row_clearance};

    /// The clearance follows the row's measured height: a wrapped two-line
    /// row widens it past the one-row floor, a missing row falls back to
    /// the floor. This is the mechanism behind M9-18's collision fix — the
    /// fixed constant put the section caption under a wrapped row's second
    /// line on narrow panes.
    #[test]
    fn a_wrapped_pill_row_widens_the_clearance_and_absence_floors_it() {
        let ctx = egui::Context::default();
        for _ in 0..2 {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            });
            egui::Area::new(egui::Id::new(("pane_pills", 0usize)))
                .fixed_pos(egui::pos2(8.0, 8.0))
                .show(&ctx, |ui| {
                    // A two-line row's worth of height.
                    ui.allocate_exact_size(egui::vec2(200.0, 56.0), egui::Sense::hover());
                });
            let _ = ctx.end_pass();
        }
        let measured = pill_row_clearance(&ctx, 0);
        assert!(
            measured >= 56.0 + super::PILL_INSET,
            "a 56pt row must widen the clearance past the floor, got {measured}"
        );
        assert_eq!(
            pill_row_clearance(&ctx, 1),
            PILL_ROW_CLEARANCE,
            "a pane with no measured row keeps the one-row floor"
        );
    }
}
