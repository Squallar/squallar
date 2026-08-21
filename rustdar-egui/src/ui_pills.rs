//! Per-pane pill rows and their popovers — the in-pane pane controls
//! (plan §1.7, §3.5), and the shared picker bodies behind them.

use crate::actions::GuiAction;
use crate::pane::PaneId;
use crate::ui_layout::PointerModality;
use rustdar_radar::sites::RadarNetwork;
use rustdar_radar::types::RenderView;
use rustdar_source::product::FieldId;

/// The row's idle opacity; the reveal animates between this and 1.0.
const PILL_IDLE_OPACITY: f32 = 0.35;

/// The row's inset from its pane's top-left corner, both axes.
const PILL_INSET: f32 = 8.0;

/// What a pane's own top-left content must leave clear for the pill row.
pub(crate) const PILL_ROW_CLEARANCE: f32 = 40.0;

/// The clearance pane `idx`'s own top-left content must keep this frame.
pub(crate) fn pill_row_clearance(ctx: &egui::Context, idx: PaneId) -> f32 {
    ctx.memory(|m| m.area_rect(egui::Id::new(("pane_pills", idx))))
        .map(|rect| rect.height() + PILL_INSET + 6.0)
        .map_or(PILL_ROW_CLEARANCE, |h| h.max(PILL_ROW_CLEARANCE))
}

/// The site popover's minimum width — room for the search field and the
/// `XXXX - TDWR` rows without wrapping.
const SITE_POPOVER_WIDTH: f32 = 220.0;

/// The link popover's width ceiling, so [`UNLINK_NOTE`] wraps instead of
/// stretching the popup across the pane.
const LINK_POPOVER_WIDTH: f32 = 260.0;

/// The site list's height, in the inspector body and the popover alike.
const SITE_LIST_HEIGHT: f32 = 150.0;

/// The Sync pill's text while all three of this pane's links are on.
const SYNC_PILL_LINKED: &str = "Sync";

/// The Sync pill's text while **any** of this pane's three links is off.
const SYNC_PILL_UNLINKED: &str = "\u{2297} Sync";

/// The Sync popover's three toggles.
pub(super) const SYNC_VIEWPORT_OPTION: &str = "Sync viewport";
pub(super) const SYNC_LAYERS_OPTION: &str = "Sync layers";
pub(super) const SYNC_TIME_OPTION: &str = "Sync time";

/// The two action rows under the toggles — the ways home. The first copies
/// this pane's viewport everywhere and touches no link; the second is that
/// copy plus every link on every visible pane turned back on.
pub(super) const SYNC_MATCH_ALL: &str = "Match all panes to this view";
pub(super) const SYNC_RELINK_ALL: &str = "Re-link all here";

/// The sync section's five row labels in draw order.
#[cfg(test)]
pub(crate) const SYNC_SECTION_LABELS: [&str; 5] = [
    SYNC_VIEWPORT_OPTION,
    SYNC_LAYERS_OPTION,
    SYNC_TIME_OPTION,
    SYNC_MATCH_ALL,
    SYNC_RELINK_ALL,
];

/// What unlinking time really does — the section's caption. Shared time
/// navigation and the loop leave the pane alone, but scan delivery is
/// per-site, so an unlinked pane still watching live still follows new scans.
pub(crate) const UNLINK_NOTE: &str = "Off leaves this pane out of shared time \
    navigation and the loop. Parked in the archive it holds its moment; \
    still live, it still follows new scans.";

/// The viewport toggle's hover: off means this pane pans and zooms alone,
/// and the group moves without it.
const VIEWPORT_LINK_NOTE: &str = "Off lets this pane pan and zoom alone; \
    the other linked panes keep moving together.";

/// What stands in the viewport toggle's place on a pane that does not share a
/// viewport ([`crate::pane::PaneState::shares_viewport`]).
pub(crate) const NO_VIEWPORT_LINK_NOTE: &str = "Viewport sync is for map \
    panes. This pane aims its own view; its setting is kept for when it \
    shows the map again.";

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

/// The three pictures a pane can show, as the pickers offer them.
const PANE_VIEW_OPTIONS: [(RenderView, &str); 3] = [
    (RenderView::PlanView, "Map"),
    (RenderView::Volume, "3D Volume"),
    (RenderView::CrossSection, "Cross-section"),
];

/// Which pill of a row something is — the popover ids salt on this, and the
/// probes name pills by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PillKind {
    /// The pane number. Activates; no popover.
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

/// **Shut every popover pane `idx` owns.** Called by `Gui::close_pane` for the
/// closed slot and every slot above it: these ids are salted on the pane
/// index, so an open popover left behind at index 3 reopens on whichever pane
/// renumbering puts at 3.
pub(super) fn close_pane_popovers(ctx: &egui::Context, idx: PaneId) {
    for pill in POPOVER_PILLS {
        egui::Popup::close_id(ctx, pill_popup_id(idx, pill));
    }
}

/// One pane's pill row, as it was drawn — reported by the renderer.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PillRowProbe {
    pub pane_idx: usize,
    /// The area's whole rect, off its own response.
    pub rect: egui::Rect,
    /// Every pill drawn, in row order: which, the text it showed, and where
    /// it landed so a test can click it.
    pub pills: Vec<(PillKind, String, egui::Rect)>,
    /// Whether the row drew at full opacity this frame.
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
    /// Every row the *site* popover painted, sections included and tagged.
    /// Empty on every other pill: only the site list has sections.
    pub site_rows: Vec<SiteRowProbe>,
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
    /// Record one drawn row and fold its click into the outcome.
    fn row(&mut self, ui: &mut egui::Ui, label: &str, selected: bool, value: T) {
        let row = ui.selectable_label(selected, label);
        #[cfg(test)]
        self.rows.push((label.to_owned(), row.rect, selected));
        if row.clicked() && !selected {
            self.picked = Some(value);
        }
    }
}

/// Which block of the site list a row was drawn in — how a probe tells a
/// shortcut row from the network group that carries the same radar again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SiteSection {
    Favorites,
    InUse,
    Nearby,
    /// A row in one of the two network groups. Only the probes ever ask which
    /// block a row came from, and only the group rows are tagged there.
    #[cfg_attr(not(test), allow(dead_code))]
    Network(RadarNetwork),
}

/// One drawn site row, as the probes see it.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SiteRowProbe {
    pub section: SiteSection,
    /// The bare identifier the row picks — never the decorated label.
    pub site: String,
    /// The text the row actually drew, decorations and all.
    pub label: String,
    /// The right-aligned note, where the row had one: the pane number, or the
    /// distance in the user's own unit. `None` in the network groups.
    pub note: Option<String>,
    pub rect: egui::Rect,
    pub is_current: bool,
    /// The star's own rect, which sits *inside* [`Self::rect`].
    pub star: egui::Rect,
    pub starred: bool,
}

/// The shortcut sections drawn above the network groups, and everything they
/// need to draw.
///
/// [`site_list_ui`] is a free function with no `&Gui` — deliberately, so the
/// one picker body cannot grow a second opinion about the app. What the `Gui`
/// knows therefore arrives here as data, assembled by
/// [`super::Gui::site_sections`] on both routes.
///
/// **Every field empty is the ordinary case** — one pane, nothing starred, no
/// location fix — and draws no sections at all.
#[derive(Default)]
pub(super) struct SiteSections {
    /// The user's starred sites, in the order they starred them.
    pub favorites: Vec<String>,
    /// What every *other* visible pane is showing: the site, and the pane
    /// index showing it. The pane number is drawn on the row, so a repeat
    /// reads as "pane 3 is here" rather than as an unexplained duplicate.
    pub in_use: Vec<(String, PaneId)>,
    /// The sites closest to the user's own position, nearest first, with the
    /// distance in kilometres. **Empty when there is no fix**, and an empty
    /// section is not drawn — which is how "this app does not know where you
    /// are" reads here. A caption saying so is a notice nobody can act on.
    pub nearby: Vec<(&'static str, f64)>,
    /// The unit [`Self::nearby`]'s distances are shown in.
    pub distance: rustdar_units::DistanceUnit,
}

/// [`PickOutcome`] plus the site list's count caption, which the inspector's
/// probe records verbatim, and the two things only the caller can act on: a
/// star that was clicked, and a click on the row that is already current.
pub(super) struct SiteListOutcome {
    pub picked: Option<String>,
    /// A star was clicked; the caller flips this identifier's favourite bit.
    pub toggled_favorite: Option<String>,
    /// **The inventory**: one entry per radar the filter kept, in the network
    /// groups' own order. Not the rows on screen — the sections above repeat
    /// some of these, and [`Self::drawn`] is what counts paint.
    #[cfg(test)]
    pub rows: Vec<(String, egui::Rect, bool)>,
    /// **The paint**: every row drawn, sections included, each tagged with the
    /// block it was drawn in.
    #[cfg(test)]
    pub drawn: Vec<SiteRowProbe>,
    #[cfg(test)]
    pub caption: String,
}

/// The Favorites section's heading.
pub(crate) const FAVORITES_HEADING: &str = "Favorites";

/// The in-use section's heading — what the *other* panes are showing.
pub(crate) const IN_USE_HEADING: &str = "In other panes";

/// The nearby section's heading.
pub(crate) const NEARBY_HEADING: &str = "Nearby";

/// How many of the closest sites the Nearby section offers. Enough to cover
/// the case the section exists for — the radar wanted is rarely the single
/// closest, since the closest is often a TDWR or a neighbour with the weather
/// on its far side — and few enough that the section stays a shortcut rather
/// than a second list.
const NEARBY_SITE_COUNT: usize = 5;

/// The star's column width inside a site row.
const SITE_STAR_WIDTH: f32 = 22.0;

/// How well one identifier answers the query — the sort key, best first.
///
/// The list was an **unranked filter**: `contains` over the identifier, in
/// table order. With place names in the predicate that is no longer good
/// enough, because typing an identifier would bury the exact radar under every
/// place whose name happens to carry the same letters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum MatchRank {
    /// The query *is* the identifier.
    ExactId,
    /// The identifier begins with the query.
    IdPrefix,
    /// The identifier carries the query somewhere inside it — how the
    /// three-letter short form (`TLX` for `KTLX`) reaches its radar, which is
    /// why this rung survives the ranking rather than folding into the one
    /// above it.
    IdSubstring,
    /// The place name carries the query.
    Place,
}

/// Where `name` ranks against an already-trimmed, already-uppercased `query`,
/// or `None` if it does not match at all.
///
/// An empty query matches everything at one rank, so the sort is the identity
/// there and the table's own order survives.
fn match_rank(name: &str, query: &str) -> Option<MatchRank> {
    if query.is_empty() {
        return Some(MatchRank::IdSubstring);
    }
    if name == query {
        return Some(MatchRank::ExactId);
    }
    if name.starts_with(query) {
        return Some(MatchRank::IdPrefix);
    }
    if name.contains(query) {
        return Some(MatchRank::IdSubstring);
    }
    // The place is free text from the station record — one field, no separable
    // state — so it is matched whole: `"Charleston,SC"` and `"Western
    // Arkansas"` are names, not a name and a state to be split apart.
    rustdar_radar::sites::table()
        .place(name)
        .filter(|place| place.to_uppercase().contains(query))
        .map(|_| MatchRank::Place)
}

/// How one site row reads: the identifier, the place if this process has been
/// told one, and the markers that say a pick lands somewhere unexpected.
fn site_row_label(name: &str, network: RadarNetwork, is_placed: bool) -> String {
    let mut label = name.to_owned();
    // `None` is the ordinary state on a fresh install, and a dangling
    // `"KMKX - "` is worse than a bare `"KMKX"`.
    if let Some(place) = rustdar_radar::sites::table().place(name) {
        label.push_str(" - ");
        label.push_str(place);
    }
    // TDWRs stay marked as well as grouped: a pick lands on a different
    // instrument (single-pol, ~89 km of Doppler range around one airport, none
    // of the Level III products this app fetches), and the marker is what says
    // so on the row itself rather than only above it.
    match (network, is_placed) {
        (RadarNetwork::Tdwr, true) => label.push_str(" - TDWR"),
        (RadarNetwork::Tdwr, false) => label.push_str(" - TDWR, position unknown"),
        (RadarNetwork::Wsr88d, false) => label.push_str(" - position unknown"),
        (RadarNetwork::Wsr88d, true) => {}
    }
    label
}

/// One site row: a full-width click target with a favourite star sitting on
/// top of its right edge, and an optional right-aligned note.
///
/// **Not one widget any more.** A `selectable_label` cannot carry a second
/// button, so this follows `ui_stack`'s eye-and-grip row: the row rect is
/// allocated with its own click sense *first*, and the star is drawn after,
/// inside it — egui resolves an overlap to the later registration, so the star
/// keeps its own click while the rest of the row keeps the row's.
///
/// The highlight reads `contains_pointer`, **not** `hovered`: the star sits on
/// top of this rect and takes `hovered` with it, so a `hovered` read would
/// blink the row's highlight off as the pointer crossed the star.
fn site_row_ui(
    ui: &mut egui::Ui,
    label: &str,
    note: Option<&str>,
    is_current: bool,
    starred: bool,
) -> (egui::Response, egui::Response) {
    let padding = ui.spacing().button_padding;
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 2.0 * padding.y;
    let (row_rect, row) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_height),
        egui::Sense::click(),
    );

    let hovered = row.contains_pointer();
    if is_current || hovered || row.has_focus() {
        let mut visuals = if hovered {
            ui.style().visuals.widgets.hovered
        } else {
            ui.style().interact_selectable(&row, is_current)
        };
        if is_current {
            // `interact_selectable`'s own override, re-applied on the hovered
            // branch so the current site paints one fill wherever inside the
            // row the pointer is.
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
    let text_color = ui
        .style()
        .interact_selectable(&row, is_current)
        .text_color();

    let star_rect = egui::Rect::from_min_max(
        egui::pos2(row_rect.right() - SITE_STAR_WIDTH, row_rect.top()),
        row_rect.max,
    );
    let body = row_rect
        .with_max_x(star_rect.left())
        .shrink2(egui::vec2(padding.x, 0.0));

    // The note is measured and given its own column rather than appended to
    // the label: a truncating label eats its own tail first, and the tail is
    // exactly the part — the distance, the pane number — the note exists for.
    let small = egui::TextStyle::Small.resolve(ui.style());
    let note_width = note.map_or(0.0, |note| {
        ui.painter()
            .layout_no_wrap(note.to_owned(), small.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    });
    let text_rect = if note_width > 0.0 {
        body.with_max_x((body.right() - note_width - padding.x).max(body.left()))
    } else {
        body
    };

    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(text_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    text_ui.add(
        egui::Label::new(egui::RichText::new(label).color(text_color))
            .selectable(false)
            .truncate(),
    );

    if let Some(note) = note.filter(|_| note_width > 0.0) {
        let mut note_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(body.with_min_x(body.right() - note_width))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        note_ui.add(
            egui::Label::new(egui::RichText::new(note).small().weak())
                .selectable(false)
                .extend(),
        );
    }

    let mut star_ui = ui.new_child(egui::UiBuilder::new().max_rect(star_rect).layout(
        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
    ));
    let star_text = if starred {
        egui::RichText::new("\u{2605}")
    } else {
        egui::RichText::new("\u{2606}").weak()
    };
    let star = star_ui
        .add(egui::Button::new(star_text).frame(false))
        .on_hover_text(if starred {
            "Remove from favorites"
        } else {
            "Add to favorites"
        });

    (row, star)
}

/// One section heading. A heading never appears alone: every caller draws it
/// on the first row it actually has, so an empty section is absent rather than
/// present and bare.
fn section_heading(ui: &mut egui::Ui, heading: &str) {
    ui.label(egui::RichText::new(heading).small().weak());
}

/// Fold one drawn row's two clicks into the outcome. The star wins over the
/// row it sits inside, which is what makes the pair one row and two meanings.
fn fold_site_row(
    row: &egui::Response,
    star: &egui::Response,
    name: &str,
    is_current: bool,
    outcome: &mut SiteListOutcome,
) {
    if star.clicked() {
        outcome.toggled_favorite = Some(name.to_owned());
    } else if row.clicked() && !is_current {
        outcome.picked = Some(name.to_owned());
    }
}

/// The three shortcut sections, above the network groups.
///
/// Each is filtered by the same query the groups are — a search that narrowed
/// the list below while the shortcuts went on showing everything would read as
/// a broken filter — and each is omitted entirely, heading and all, when
/// nothing survives.
fn draw_site_sections(
    ui: &mut egui::Ui,
    query: &str,
    current: &str,
    sections: &SiteSections,
    outcome: &mut SiteListOutcome,
) {
    let favorites = sections.favorites.clone();
    let mut draw = |ui: &mut egui::Ui,
                    section: SiteSection,
                    heading: &str,
                    entries: Vec<(String, Option<String>)>| {
        let mut drew_heading = false;
        for (name, note) in entries {
            if match_rank(&name, query).is_none() {
                continue;
            }
            if !drew_heading {
                section_heading(ui, heading);
                drew_heading = true;
            }
            let is_current = current == name;
            // A shortcut may name a radar this process cannot place — a stale
            // favourite, or one the catalogue has not reached yet. It is still
            // pickable: the data is fetched by identifier, and the volume then
            // places it.
            let placed = rustdar_radar::sites::get_radar_site(&name).is_some();
            let label = site_row_label(&name, RadarNetwork::of_id(&name), placed);
            let starred = favorites.contains(&name);
            let (row, star) = site_row_ui(ui, &label, note.as_deref(), is_current, starred);
            #[cfg(test)]
            outcome.drawn.push(SiteRowProbe {
                section,
                site: name.clone(),
                label,
                note: note.clone(),
                rect: row.rect,
                is_current,
                star: star.rect,
                starred,
            });
            #[cfg(not(test))]
            let _ = (section, label);
            fold_site_row(&row, &star, &name, is_current, outcome);
        }
    };

    draw(
        ui,
        SiteSection::Favorites,
        FAVORITES_HEADING,
        sections
            .favorites
            .iter()
            .map(|site| (site.clone(), None))
            .collect(),
    );
    draw(
        ui,
        SiteSection::InUse,
        IN_USE_HEADING,
        sections
            .in_use
            .iter()
            .map(|(site, idx)| (site.clone(), Some(format!("pane {}", idx + 1))))
            .collect(),
    );
    draw(
        ui,
        SiteSection::Nearby,
        NEARBY_HEADING,
        sections
            .nearby
            .iter()
            .map(|(site, km)| {
                (
                    (*site).to_owned(),
                    Some(format!(
                        "{:.0} {}",
                        sections.distance.convert_from_km(*km),
                        sections.distance.suffix()
                    )),
                )
            })
            .collect(),
    );
}

/// The filterable list of every radar this process knows of: the count
/// caption, then a scrolling list — the shortcut sections first, then the two
/// network groups — with the current site highlighted, TDWRs marked and radars
/// with no known position marked too.
///
/// **A site may appear in a section and in its network group.** The sections
/// are shortcuts, not a partition.
pub(super) fn site_list_ui(
    ui: &mut egui::Ui,
    query: &str,
    current: &str,
    catalogue_pending: bool,
    sections: &SiteSections,
) -> SiteListOutcome {
    let table = rustdar_radar::sites::table();
    let radars = table.rows();
    let unplaced = table.unplaced();

    let query = query.trim().to_uppercase();
    // The row carries the network as DATA. `is_tdwr_id` survives at exactly one
    // site — the unplaced members, which have no row to ask — and nothing here
    // re-derives the classification from a prefix.
    let mut shown: Vec<(&'static str, RadarNetwork, bool, MatchRank)> = radars
        .iter()
        .filter_map(|site| {
            match_rank(site.name, &query).map(|rank| (site.name, site.network, true, rank))
        })
        .chain(unplaced.iter().filter_map(|name| {
            match_rank(name, &query).map(|rank| (*name, RadarNetwork::of_id(name), false, rank))
        }))
        .collect();
    // Stable, so an empty query — every row at one rank — leaves the table's
    // own order exactly where it was.
    shown.sort_by_key(|(_, _, _, rank)| *rank);

    let total = radars.len() + unplaced.len();
    let tdwr = shown_total_tdwrs(radars, unplaced);
    // **The count is of radars, not of rows.** The sections above repeat some
    // of what the groups list, so a bare "N shown" would disagree with what a
    // reader can count on screen. Naming the denominator — `of {total} radars`
    // — is what makes the numerator legible as the same unit.
    let caption = if total == 0 {
        "Finding radars...".to_owned()
    } else if catalogue_pending {
        // No total, deliberately: how big the network is, is a claim nothing
        // has made yet.
        format!("{} radars so far - still finding the network", shown.len())
    } else if unplaced.is_empty() {
        format!(
            "{} of {} radars - {} NEXRAD + {} TDWR",
            shown.len(),
            total,
            total - tdwr,
            tdwr
        )
    } else {
        format!(
            "{} of {} radars - {} NEXRAD + {} TDWR, {} unplaced",
            shown.len(),
            total,
            total - tdwr,
            tdwr,
            unplaced.len(),
        )
    };
    ui.label(egui::RichText::new(caption.as_str()).small().weak());

    let mut outcome = SiteListOutcome {
        picked: None,
        toggled_favorite: None,
        #[cfg(test)]
        rows: Vec::new(),
        #[cfg(test)]
        drawn: Vec::new(),
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
            draw_site_sections(ui, &query, current, sections, &mut outcome);

            // Two groups, WSR-88D first. The split is presentation only: the
            // search above spans both, every row stays pickable, and what a
            // pick persists is still the bare ICAO, so a reopen is unchanged.
            // A heading is drawn only over a group that has rows -- an empty
            // section is not an expressed option, it is furniture.
            let groups = [
                // "NEXRAD" rather than "WSR-88D" because the caption three
                // lines above already spells it that way, and one widget must
                // not use two words for one network.
                (RadarNetwork::Wsr88d, "NEXRAD"),
                (RadarNetwork::Tdwr, "TDWR"),
            ];
            let labelled = groups
                .iter()
                .filter(|(network, _)| shown.iter().any(|(_, n, ..)| n == network))
                .count()
                > 1;
            for (network, heading) in groups {
                let mut drew_heading = false;
                for (name, row_network, is_placed, _) in
                    shown.iter().filter(|(_, n, ..)| *n == network)
                {
                    if labelled && !drew_heading {
                        section_heading(ui, heading);
                        drew_heading = true;
                    }
                    let name = *name;
                    let is_current = current == name;
                    let label = site_row_label(name, *row_network, *is_placed);
                    let starred = sections.favorites.iter().any(|s| s == name);
                    let (row, star) = site_row_ui(ui, &label, None, is_current, starred);
                    #[cfg(test)]
                    {
                        outcome.rows.push((name.to_owned(), row.rect, is_current));
                        outcome.drawn.push(SiteRowProbe {
                            section: SiteSection::Network(*row_network),
                            site: name.to_owned(),
                            label,
                            note: None,
                            rect: row.rect,
                            is_current,
                            star: star.rect,
                            starred,
                        });
                    }
                    #[cfg(not(test))]
                    let _ = label;
                    fold_site_row(&row, &star, name, is_current, &mut outcome);
                }
            }
        });
    outcome
}

/// How many of the two inventories are TDWRs.
fn shown_total_tdwrs(
    radars: &[rustdar_radar::sites::RadarSite],
    unplaced: &[&'static str],
) -> usize {
    radars
        .iter()
        .filter(|site| site.network == RadarNetwork::Tdwr)
        .count()
        + unplaced
            .iter()
            .filter(|name| RadarNetwork::of_id(name) == RadarNetwork::Tdwr)
            .count()
}

/// The product list a scan offers, current one highlighted. Rendered by the
/// inspector's product combo body and the product pill's popover alike.
pub(super) fn product_list_ui(
    ui: &mut egui::Ui,
    options: &[FieldId],
    current: &FieldId,
) -> PickOutcome<FieldId> {
    let mut outcome = PickOutcome::default();
    for product in options {
        outcome.row(
            ui,
            crate::field_facts::name(product),
            product == current,
            product.clone(),
        );
    }
    outcome
}

/// The tilt list a product offers, current one highlighted — the same exact
/// equality the combo's `selectable_value` used.
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

/// The three pane views, current one highlighted — [`PANE_VIEW_OPTIONS`].
pub(super) fn kind_list_ui(ui: &mut egui::Ui, current: RenderView) -> PickOutcome<RenderView> {
    let mut outcome = PickOutcome::default();
    for (view, label) in PANE_VIEW_OPTIONS {
        outcome.row(ui, label, view == current, view);
    }
    outcome
}

/// What one pass of [`sync_section_ui`] produced: which action row was
/// clicked, whether the layer link was just turned **on**, and — for the
/// probes — the rows as drawn.
#[derive(Default)]
pub(crate) struct SyncSectionOutcome {
    pub layer_relinked: bool,
    pub match_all: bool,
    pub relink_all: bool,
    #[cfg(test)]
    pub rows: Vec<(String, egui::Rect, bool)>,
}

/// **The** per-pane sync section: the three link checkboxes — all
/// honestly per-pane, writing `pane`'s own fields — and the two action rows.
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

    if pane.shares_viewport() {
        #[cfg(test)]
        let was = pane.viewport_link;
        let row = ui
            .checkbox(&mut pane.viewport_link, SYNC_VIEWPORT_OPTION)
            .on_hover_text(VIEWPORT_LINK_NOTE);
        #[cfg(test)]
        push(&mut outcome.rows, SYNC_VIEWPORT_OPTION, row.rect, was);
        #[cfg(not(test))]
        let _ = row;
    } else {
        ui.label(egui::RichText::new(NO_VIEWPORT_LINK_NOTE).small().weak());
    }

    #[cfg(test)]
    let was = pane.layer_link;
    let row = ui
        .checkbox(&mut pane.layer_link, SYNC_LAYERS_OPTION)
        .on_hover_text(LAYER_LINK_NOTE);
    if row.changed() && pane.layer_link {
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

/// The Sync pill's hover: plain "Sync options" while every link this pane
/// offers is on, and the unlinked dimensions named while any is off.
fn sync_pill_hover(
    shares_viewport: bool,
    viewport_link: bool,
    layer_link: bool,
    time_link: bool,
) -> String {
    let mut off = Vec::new();
    if shares_viewport && !viewport_link {
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
    /// The shortcut sections pane `idx`'s site picker offers.
    ///
    /// Assembled here rather than inside [`site_list_ui`] because that body is
    /// a free function with no `&Gui`: what the app knows crosses as data, and
    /// the two routes cannot drift into two answers.
    /// `current` is passed rather than read off `self.panes[idx]`: the
    /// inspector route runs with the active pane `mem::take`n out of the
    /// vector, so the slot holds a placeholder that would answer for the wrong
    /// site.
    pub(super) fn site_sections(&self, idx: PaneId, current: &str) -> SiteSections {
        // Deliberately NOT `Gui::live_sites`, which filters to panes watching
        // live — the right predicate for the chunk feed, the wrong one here. A
        // pane parked in the archive is still a pane showing a site.
        let mut in_use: Vec<(String, PaneId)> = Vec::new();
        for (other, pane) in self
            .panes
            .iter()
            .enumerate()
            .take(self.pane_layout.pane_count)
        {
            let site = pane.site();
            if other == idx || site == current || in_use.iter().any(|(seen, _)| seen == site) {
                continue;
            }
            in_use.push((site.to_owned(), other));
        }

        // No fix, no section: `user_fix` absent is the whole condition, and
        // the empty vector is what makes the section vanish rather than draw
        // an apology the reader cannot act on.
        let nearby = self.user_fix.as_ref().map_or_else(Vec::new, |fix| {
            rustdar_radar::sites::nearest_sites(fix.point.lat, fix.point.lon, NEARBY_SITE_COUNT)
                .into_iter()
                .map(|(site, km)| (site.name, km))
                .collect()
        });

        SiteSections {
            favorites: self.favorite_sites.clone(),
            in_use,
            nearby,
            distance: self.preferences.distance,
        }
    }

    /// Star or unstar `site`. App-wide: a favourite belongs to the person, not
    /// to the pane whose picker the star was clicked in.
    pub(super) fn toggle_favorite_site(&mut self, site: &str) {
        match self.favorite_sites.iter().position(|s| s == site) {
            Some(at) => {
                self.favorite_sites.remove(at);
            }
            // Appended, so the section reads in the order the user built it.
            None => self.favorite_sites.push(site.to_owned()),
        }
    }

    /// Ask for pane `idx` to show `view` the pickers' way: through the
    /// deferred applier, arming the cross-section draw when the pane has no
    /// line to show yet.
    pub(super) fn pick_pane_kind(&mut self, idx: PaneId, view: RenderView, line_absent: bool) {
        self.request_pane_view(idx, view);
        if view == RenderView::CrossSection && line_absent {
            self.set_section_draw_armed(true);
        }
    }

    /// Draw every visible pane's pill row. Called from
    /// [`Gui::ui`](super::Gui::ui) after the pane loop and the pending
    /// appliers — outside every `mem::take` window, so the popovers' writes
    /// land on real panes.
    pub(super) fn render_pane_pills(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        actions: &mut Vec<GuiAction>,
    ) {
        let Some(chrome) = self.chrome_fade() else {
            self.pills_drawn_last_frame = 0;
            return;
        };
        let pane_count = self.visible_pane_count();
        for idx in 0..pane_count {
            let pane_rect = self.pane_layout.pane_rect(idx, map_rect);
            self.render_pill_row(ctx, idx, pane_rect, chrome, actions);
        }

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
        let (site, kind, product, shares_viewport, links, line_absent, tilt, products, elevations) = {
            let pane = &self.panes[idx];
            let (_, tilt) = pane
                .get_rendering_params()
                .unwrap_or((pane.selected_product(), pane.selected_elevation()));
            (
                pane.site().to_string(),
                pane.render_view(),
                pane.selected_product(),
                pane.shares_viewport(),
                (pane.viewport_link, pane.layer_link, pane.time_link),
                pane.cross_section().and_then(|s| s.line).is_none(),
                tilt,
                pane.scan_info.as_ref().map(|info| {
                    // The scan lists what it offers in the radar layer's own
                    // terms; the picker names fields by id.
                    info.available_products
                        .iter()
                        .map(|p| rustdar_radar::fields::spec(*p).id.clone())
                        .collect::<Vec<_>>()
                }),
                pane.scan_info
                    .as_ref()
                    .and_then(|info| {
                        let product = rustdar_radar::fields::product_for(&pane.selected_product())?;
                        info.product_elevations.get(&product)
                    })
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        let is_map = kind == RenderView::PlanView;
        let offer_link = self.pane_layout.pane_count > 1;

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
                ui.set_opacity(row_opacity);
                if chrome < 1.0 {
                    ui.disable();
                }
                ui.set_max_width((pane_rect.width() - 2.0 * PILL_INSET).max(40.0));
                ui.horizontal_wrapped(|ui| {
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

                    let code = crate::field_facts::code(&product).to_uppercase();
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
                        self.product_pill_popover(&pill, idx, products.as_deref(), &product);
                    }

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

                    if offer_link {
                        let (viewport_link, layer_link, time_link) = links;
                        let all_linked =
                            (!shares_viewport || viewport_link) && layer_link && time_link;
                        let label = if all_linked {
                            SYNC_PILL_LINKED
                        } else {
                            SYNC_PILL_UNLINKED
                        };
                        let pill = ui.button(label).on_hover_text(sync_pill_hover(
                            shares_viewport,
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

                    let label = match kind {
                        RenderView::PlanView => "Map",
                        RenderView::Volume => "3D Volume",
                        RenderView::CrossSection => "X-section",
                    };
                    let pill = ui.button(label).on_hover_text("Pane view");
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
            self.probes.last_pills.push(probe);
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The site popover: search field over the one site list.
    fn site_pill_popover(
        &mut self,
        pill: &egui::Response,
        idx: PaneId,
        current: &str,
        actions: &mut Vec<GuiAction>,
    ) {
        let sections = self.site_sections(idx, current);
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
                let outcome = site_list_ui(ui, &query, current, self.catalogue_pending, &sections);
                #[cfg(test)]
                {
                    self.probes.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Site,
                        rect: egui::Rect::NOTHING,
                        search: Some(search.rect),
                        rows: outcome.rows.clone(),
                        site_rows: outcome.drawn.clone(),
                    });
                }
                #[cfg(not(test))]
                let _ = search;
                if let Some(site) = outcome.toggled_favorite {
                    self.toggle_favorite_site(&site);
                }
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

    /// The product popover: the scan's own product list.
    fn product_pill_popover(
        &mut self,
        pill: &egui::Response,
        idx: PaneId,
        options: Option<&[FieldId]>,
        current: &FieldId,
    ) {
        let shown = egui::Popup::menu(pill)
            .id(pill_popup_id(idx, PillKind::Product))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let outcome = match options {
                    Some(options) => product_list_ui(ui, options, current),
                    None => {
                        ui.label("No scan loaded");
                        PickOutcome::default()
                    }
                };
                #[cfg(test)]
                {
                    self.probes.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Product,
                        rect: egui::Rect::NOTHING,
                        search: None,
                        rows: outcome.rows.clone(),
                        site_rows: Vec::new(),
                    });
                }
                if let Some(picked) = outcome.picked {
                    self.active_pane = idx;
                    let pane = &mut self.panes[idx];
                    if pane.selected_product() != picked {
                        pane.set_selected_product(picked);
                        pane.set_selected_elevation(0.0);
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
                let current = self.panes[idx].selected_elevation();
                let outcome = if elevations.is_empty() {
                    ui.label("Waiting for this product's data");
                    PickOutcome::default()
                } else {
                    tilt_list_ui(ui, elevations, current)
                };
                #[cfg(test)]
                {
                    self.probes.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Tilt,
                        rect: egui::Rect::NOTHING,
                        search: None,
                        rows: outcome.rows.clone(),
                        site_rows: Vec::new(),
                    });
                }
                if let Some(angle) = outcome.picked {
                    self.active_pane = idx;
                    self.panes[idx].set_selected_elevation(angle);
                    self.propagate_layer_sync();
                    ui.close_kind(egui::UiKind::Menu);
                }
            });
        self.record_popover_rect(&shown);
    }

    /// The Sync popover: [`sync_section_ui`] over this pane. Checkboxes
    /// keep the popover up.
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
                    self.propagate_layer_sync();
                }
                if outcome.match_all || outcome.relink_all {
                    ui.close_kind(egui::UiKind::Menu);
                }

                #[cfg(test)]
                {
                    self.probes.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Link,
                        rect: egui::Rect::NOTHING,
                        search: None,
                        rows: outcome.rows,
                        site_rows: Vec::new(),
                    });
                }
            });
        self.record_popover_rect(&shown);
    }

    /// The kind popover: the three kinds through [`Gui::pick_pane_kind`],
    /// deferred.
    fn kind_pill_popover(
        &mut self,
        pill: &egui::Response,
        idx: PaneId,
        current: RenderView,
        line_absent: bool,
    ) {
        let shown = egui::Popup::menu(pill)
            .id(pill_popup_id(idx, PillKind::Kind))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let outcome = kind_list_ui(ui, current);
                #[cfg(test)]
                {
                    self.probes.last_pill_popover = Some(PillPopoverProbe {
                        pane_idx: idx,
                        pill: PillKind::Kind,
                        rect: egui::Rect::NOTHING,
                        search: None,
                        rows: outcome.rows.clone(),
                        site_rows: Vec::new(),
                    });
                }
                if let Some(view) = outcome.picked {
                    self.active_pane = idx;
                    self.pick_pane_kind(idx, view, line_absent);
                    ui.close_kind(egui::UiKind::Menu);
                }
            });
        self.record_popover_rect(&shown);
    }

    /// Fill in the rect of the popover probe the closure just recorded.
    fn record_popover_rect(&mut self, _shown: &Option<egui::InnerResponse<()>>) {
        #[cfg(test)]
        if let Some(inner) = _shown
            && let Some(probe) = self.probes.last_pill_popover.as_mut()
        {
            probe.rect = inner.response.rect;
        }
    }
}

#[cfg(test)]
#[path = "ui_pills/site_picker_tests.rs"]
mod site_picker_tests;

#[cfg(test)]
mod clearance_tests {
    use super::{PILL_ROW_CLEARANCE, pill_row_clearance};

    /// The clearance follows the row's measured height: a wrapped two-line
    /// row widens it past the one-row floor, a missing row falls back to
    /// the floor.
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
