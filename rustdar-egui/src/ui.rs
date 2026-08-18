use crate::actions::{GuiAction, RadarConfig};
use rustdar_overlays::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};

const DEFAULT_INITIAL_ZOOM: f64 = 7.0;

use crate::pane::{ColorScaleOrientation, PaneId, PaneLayout, PaneState};
use crate::tiles::MapTileState;
use crate::ui_layout::{LayoutCtx, ModalityLatch};
use chrono::{NaiveDateTime, Timelike};
use egui::Context;
use rustdar_overlays::fetch_policy::FetchHealth;
use rustdar_overlays::render::overlay_state::{OverlayKind, OverlayRegistry};
use rustdar_radar::types::{RadarProduct, ScanInfo};
use rustdar_units::UserPreferences;
use std::collections::HashMap;

#[path = "ui_shell.rs"]
mod shell;
#[path = "ui_stack.rs"]
mod ui_stack;
/// What the stack drew last frame, for the input harness.
#[cfg(test)]
pub(crate) use ui_stack::{StackProbe, StackRowProbe};
#[path = "ui_inspector.rs"]
mod ui_inspector;
/// What the inspector drew last frame, for the input harness.
#[cfg(test)]
pub(crate) use ui_inspector::InspectorProbe;
#[path = "ui_config.rs"]
mod config;
#[path = "ui_map_overlays.rs"]
mod map_overlays;
#[path = "ui_popups.rs"]
mod popups;
#[path = "ui_menu.rs"]
mod ui_menu;
/// What the menu presentations actually drew last frame, for the input harness.
#[cfg(test)]
pub(crate) use ui_menu::DrawnMenuLeaf;
/// The region-drag arming toggle's label, for the same reason — and for one
/// more: the tests that prove the two armed drags are mutually exclusive have to
/// look both entries up by name in the same menu.
#[cfg(test)]
/// The 3D-pane toggle's label, for the input harness — so the tests that look
/// the entry up by name cannot go on passing after it is renamed.
#[cfg(test)]
pub(crate) use ui_menu::VOLUME_PANE_LABEL;
/// The cross-section arming toggle's label, for the same reason.
#[cfg(test)]
pub(crate) use ui_menu::{DRAW_CROSS_SECTION_LABEL, PICK_REGION_LABEL};
#[path = "ui_map.rs"]
pub(crate) mod map;
/// The 3D block's sidebar header, for the input harness — so the test that
/// pins the sidebar's shared structure names the header the panel really
/// draws rather than keeping its own copy of it.
#[cfg(test)]
pub(crate) use map::VOLUME_SIDEBAR_HEADER;
/// Re-exported so the input harness can name it: `map` is private to this
/// module, and the probe is the only thing outside it that has to be.
#[cfg(test)]
pub(crate) use map::VolumeArmProbe;
/// The copy the two non-map pane arms paint, for the input harness — so a test
/// can require the text to have been painted inside a given pane's rect without
/// keeping its own copy of the sentence. Same arrangement as [`DrawnMenuLeaf`].
#[cfg(test)]
pub(crate) use map::{CROSS_SECTION_EMPTY_STATE, VOLUME_EMPTY_STATE};
#[path = "ui_settings.rs"]
mod settings;
#[path = "ui_statusbar.rs"]
mod statusbar;
#[path = "ui_timeline.rs"]
mod timeline;
#[cfg(test)]
pub(crate) use timeline::TimelineProbe;
#[path = "ui_topbar.rs"]
mod topbar;
/// The top bar's layout floor — margins plus one interact row — for the M8
/// breathing-room pin.
#[cfg(test)]
pub(crate) use topbar::MIN_BAR_HEIGHT;
#[path = "ui_pills.rs"]
mod pills;
/// What a pane's own top-left content leaves clear for its pill row — read
/// by the section pane's layout and the 3D pane's caption.
#[cfg(test)]
pub(crate) use pills::PILL_ROW_CLEARANCE;
/// The sync section's row labels, for the parity walk — the model half of
/// its Pane-properties sync leg; the drawn half is `InspectorProbe`'s
/// `sync_rows`.
#[cfg(test)]
pub(crate) use pills::SYNC_SECTION_LABELS;
/// What the pill rows and their popovers drew last frame, for the input
/// harness.
#[cfg(test)]
pub(crate) use pills::{PillKind, PillPopoverProbe, PillRowProbe};
#[path = "ui_fade.rs"]
mod fade;
#[path = "ui_sheet.rs"]
mod sheet;
/// The sheet's snap extent — `Gui` holds one as session state.
pub(crate) use sheet::SheetExtent;
/// The sheet-page projection, for the input harness — production code names
/// it through `sheet::` directly.
#[cfg(test)]
pub(crate) use sheet::SheetPage;
/// What the bottom bar, the sheet and the error toast drew last frame, for
/// the input harness.
#[cfg(test)]
pub(crate) use sheet::{BottomBarProbe, ErrorToastProbe, SheetProbe};
#[path = "ui_catalog.rs"]
mod catalog;
/// The preset shape, re-used by the config writer.
pub(crate) use catalog::PresetConfig;
/// The compiled-in presets, for the parity walk — the catalog leg's Presets
/// inventory is the table the renderer draws, not a restated name list.
#[cfg(test)]
pub(crate) use catalog::builtin_presets;
/// What the catalog drew last frame, for the input harness.
#[cfg(test)]
pub(crate) use catalog::{CatalogGroup, CatalogProbe, CatalogTileProbe};

/// The sentence the settings pane puts under a refusal, for the same reason and
/// on the same terms as the two empty states above: where a refusal is undone
/// is `cfg`'d per platform, so a harness test that spelled it out would only
/// ever pin whichever row ran it.
#[cfg(test)]
pub(crate) use settings::LOCATION_DENIED_NOTE;
/// The settings window's row table and its drawn-row probe, for the parity
/// walk — the inventory it asserts is the table the renderer iterates, so a
/// row cannot be dropped from one without the other noticing.
#[cfg(test)]
pub(crate) use settings::{DrawnSettingsRow, SETTINGS_ROWS};

// The Gui shell's own split (WO-E1): the struct and its state types in
// `state`, the test-only frame probes in `probes`, the per-frame drive in
// `frame`, the pane-link fan-outs in `sync`, and the registry/config swap
// sites in `layer_glue`. Everything else about the shell still lives here.
#[path = "gui/probes.rs"]
mod probes;
pub(crate) use probes::ControlProbe;
#[path = "gui/state.rs"]
mod state;
pub(super) use state::AutoPollState;
pub use state::{ChunkFeedStatus, CurrentVolumeStamp, Gui, StormMotionOverride, TiltFreshness};
#[path = "gui/frame.rs"]
mod frame;
#[path = "gui/layer_glue.rs"]
mod layer_glue;
#[path = "gui/sync.rs"]
mod sync;

use crate::ui_input::InteractionState;

/// One pane-count button the picker drew, as it was drawn. See
/// [`ui_menu::DrawnMenuLeaf`] for the same shape and the reason for it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaneOptionProbe {
    pub count: usize,
    pub selected: bool,
    /// Whether the button could be clicked. The top bar draws every count up
    /// to the absolute maximum and disables the ones past this width's offer,
    /// so "the picker narrows on a phone" is now a claim about this flag.
    pub enabled: bool,
    pub rect: egui::Rect,
}

/// What the top bar drew: the rects a test drives it by, and the state each
/// toggle was showing. Reported by the renderer, never rebuilt by a test —
/// see [`ui_menu::DrawnMenuLeaf`] for the pattern.
///
/// The phone-only fields (`scan_text`, `collapse`, `hover`) stay at their
/// defaults on the wider widths, exactly as the desktop-only fields (the ☰
/// button, the Layers and Inspector toggles) stay at theirs on Compact —
/// which fields are live *is* the report of which bar drew.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TopBarProbe {
    /// The rect the docked panel claimed, straight off its own response.
    pub rect: egui::Rect,
    /// The ☰ button that opens the whole-menu dropdown.
    pub menu_button: egui::Rect,
    /// The Layers toggle, and whether it read as open.
    pub layers_toggle: (egui::Rect, bool),
    /// The largest pane count offered *enabled* at this width.
    pub pane_count_max: usize,
    /// The X-sec arm toggle, and whether it read as armed.
    pub section_arm: (egui::Rect, bool),
    /// The Region arm toggle, and whether it read as armed.
    pub region_arm: (egui::Rect, bool),
    /// The ⚙ Inspector toggle, and whether it read as open.
    pub inspector_toggle: (egui::Rect, bool),
    /// The phone bar's scan summary chip text, verbatim — the short form
    /// the compact status bar used to carry.
    pub scan_text: String,
    /// The phone bar's ◧ collapse/restore button — the status bar's own
    /// collapse state, applied to this bar on Compact (contract 75).
    pub collapse: egui::Rect,
    /// Whether the phone bar hosted the hover readout this frame (contract
    /// 25: the readout follows the modality, not the width).
    pub hover: bool,
}

#[cfg(test)]
impl Default for TopBarProbe {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            menu_button: egui::Rect::NOTHING,
            layers_toggle: (egui::Rect::NOTHING, false),
            pane_count_max: 0,
            section_arm: (egui::Rect::NOTHING, false),
            region_arm: (egui::Rect::NOTHING, false),
            inspector_toggle: (egui::Rect::NOTHING, false),
            scan_text: String::new(),
            collapse: egui::Rect::NOTHING,
            hover: false,
        }
    }
}

/// Which render arm ran for one pane, recorded **inside the arm itself**.
///
/// The point is the asymmetry. `panes[i].kind()` is the *input* to
/// `render_panes`' single kind branch, so a test reading it back proves nothing
/// about the branch: a mis-wired arm, or an arm reading the kind off the
/// `mem::take`n slot instead of the taken value, agrees with it perfectly. Each
/// arm writes its own view as a literal, so what this reports is the arm that
/// actually drew — the one thing a wrong branch cannot fake.
///
/// The rect comes along because "which arm ran" and "where it drew" are the two
/// halves of the same claim: an arm that painted the right thing into another
/// pane's rect is still wrong.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaneContentProbe {
    pub pane_idx: usize,
    /// The view the arm that ran is *for*, written by that arm.
    ///
    /// A `RenderView` rather than a `PaneKind`, because the branch it reports
    /// on is a three-way one and two of the three are the same pane kind: a
    /// probe in the vocabulary of kinds could not tell a plan-view arm from a
    /// volume arm, which is exactly the mis-wiring it exists to catch.
    pub view: rustdar_radar::types::RenderView,
    pub rect: egui::Rect,
}

/// What the status bar drew, rather than the flags that decided it.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StatusBarProbe {
    /// The scan summary text, verbatim — long or short form.
    pub scan_text: String,
    /// The Level III product age line, when one was drawn.
    pub product_age_text: Option<String>,
    /// The auto-poll chip's rect and text, when one was drawn. The chip
    /// replaced the checkbox with the full-bleed flip; the toggle itself
    /// lives in the ☰ menu.
    pub poll_chip: Option<(egui::Rect, String)>,
    /// The refresh button's rect — always drawn, so a test can click the real
    /// button rather than restating its position.
    pub refresh: egui::Rect,
    /// The ◧ collapse button's rect — the restore button while collapsed.
    pub collapse: egui::Rect,
    /// Whether the bar was collapsed to its restore button this frame.
    pub collapsed: bool,
    /// Whether the hover readout was drawn.
    pub hover: bool,
    /// The rect the floating bar actually claimed, straight off its own
    /// response — not the bottom slice of the screen worked out a second time.
    pub rect: egui::Rect,
}

#[cfg(test)]
impl Default for StatusBarProbe {
    fn default() -> Self {
        Self {
            scan_text: String::new(),
            product_age_text: None,
            poll_chip: None,
            refresh: egui::Rect::NOTHING,
            collapse: egui::Rect::NOTHING,
            collapsed: false,
            hover: false,
            rect: egui::Rect::NOTHING,
        }
    }
}

/// What the inspector's body is about: the app's settings, the active pane's
/// own properties, or one layer's options.
///
/// One selection for every width — the session state the crumb row renders
/// and the three body arms dispatch on. `AppSettings` is the default and the
/// state `✕` deselect returns to: it is the one body that is never about the
/// active pane, so it is the one that can never be wrong about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InspectorSelection {
    /// The app's own settings — units, location, GPS, storm motion.
    AppSettings,
    /// The active pane's properties: kind, product, tilt, sync, and the
    /// kind-specific block.
    PaneProps,
    /// One layer's options, hosted by `render_overlay_controls_one`.
    Layer(OverlayKind),
}

/// Radar fetch lifecycle state.
pub(super) struct RadarState {
    pub config: RadarConfig,
    pub fetching: bool,
    pub error_message: Option<String>,
}

/// Time editing dialog state.
pub(super) struct TimeDialogState {
    pub date_string: String,
    pub time_string: String,
    pub show: bool,
}

/// Where an in-flight cross-section draw started.
///
/// # `ground` is the endpoint; `screen` is only the gesture
///
/// The two are not redundant and they answer different questions.
///
/// `ground` is what the finished line is built from, and it is converted from
/// the pointer **inside `Map::show` on the press frame**, where the projector
/// is in hand. A pixel denotes different ground after any viewport change, and
/// an armed draw suppresses panning but *not* zooming — walkers reads the wheel
/// itself — so a pixel anchor held across a mid-drag zoom would silently re-aim
/// the line's near end while the far end tracked the finger. The user would get
/// a section of somewhere they never pointed at, with a perfectly convincing
/// picture of it.
///
/// `screen` is the anchor's position *as a gesture*, and it is the right
/// coordinate for exactly one question: did the finger travel far enough to mean
/// a line rather than a tap ([`MIN_SECTION_DRAG_PT`]). That is a question about
/// the hand, not about the ground, and re-deriving it from `ground` each frame
/// would make the threshold depend on the zoom level.
///
/// [`MIN_SECTION_DRAG_PT`]: crate::ui_input::MIN_SECTION_DRAG_PT
struct SectionAnchor {
    /// The map pane the draw started on.
    pane_idx: PaneId,
    /// Where it started, on the ground.
    ground: crate::pane::GeoPoint,
    /// Where it started, on screen.
    screen: egui::Pos2,
    /// Where the pointer is now, on screen. The far end of the rubber band.
    current: egui::Pos2,
}

/// Queue an overlay fetch the **user** asked for, and clear the layer's retry
/// ladder on the way past.
///
/// The one door every user-driven overlay fetch goes through: the Refresh
/// button, switching a layer on, and every option change that implies a refetch
/// (outlook day and product, model parameter). The automatic poll in
/// [`Gui::check_auto_polls`] pushes its action directly and deliberately does
/// **not** come through here.
///
/// That split is what makes "a user action is never made to wait out a backoff"
/// structural rather than remembered: the ladder is consulted only by
/// `auto_fetch_delay`, and the only way to queue a user fetch is a call that has
/// already cleared it — including a layer recorded as permanently broken, which
/// no automatic poll will ever retry. See [`rustdar_overlays::fetch_policy`].
///
/// Still deduplicated per frame: the handlers are global, so one fetch serves
/// every pane that asked.
///
/// Note that overlays are not site-scoped — they are national products, and
/// changing the radar site does not queue an overlay fetch at all. The user
/// actions that reach a layer's fetch path are exactly the ones above.
pub(crate) fn push_user_overlay_fetch(
    overlays: &mut OverlayRegistry,
    actions: &mut Vec<GuiAction>,
    kind: OverlayKind,
    pane_idx: usize,
) {
    overlays.clear_retry(kind);
    if !actions
        .iter()
        .any(|a| matches!(a, GuiAction::FetchOverlay { kind: k, .. } if *k == kind))
    {
        actions.push(GuiAction::FetchOverlay { kind, pane_idx });
    }
}

/// Every handler with controls, in the audit's canonical order.
///
/// Production no longer iterates this: the stack's rows walk the active
/// pane's own `draw_order`, and the inspector renders one selected handler.
/// It remains the parity walk's inventory — the list of handlers whose every
/// control must be reachable — which is why it is test-only now rather than
/// deleted: a handler dropped from it would silently leave the audit.
#[cfg(test)]
pub(crate) const OVERLAY_CONTROL_ORDER: &[OverlayKind] = &[
    OverlayKind::Radar,
    OverlayKind::ModelData,
    OverlayKind::SpcOutlook,
    OverlayKind::SpcDiscussions,
    OverlayKind::NwsAlerts,
    OverlayKind::StormReports,
    OverlayKind::Lightning,
    OverlayKind::Metar,
    OverlayKind::CityLabels,
    OverlayKind::RadarSites,
    OverlayKind::UserLocation,
    OverlayKind::ColorScale,
];

/// The label the open list puts against `value`, or the raw value for one the
/// handler did not offer.
///
/// The single source of the text for a [`ControlItem::Dropdown`]: both the
/// collapsed box and the list read it, which is the whole point of it existing.
fn dropdown_option_label<'a>(options: &'a [(String, String)], value: &'a str) -> &'a str {
    options
        .iter()
        .find(|(v, _)| v == value)
        .map_or(value, |(_, display)| display.as_str())
}

/// One dropdown a control tree actually drew: the text the *collapsed* box
/// showed, and where it landed so a test can open it for real.
///
/// Reported by the renderer, like [`ui_menu::DrawnMenuLeaf`], rather than
/// rebuilt by a test from the [`ControlItem`] — a test that reformatted the
/// model itself would agree with a renderer that had stopped doing so.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnDropdown {
    pub id: &'static str,
    pub label: String,
    pub selected_text: String,
    pub rect: egui::Rect,
}

/// The widget shape a [`ControlItem`] rendered as.
///
/// Coarser than the model on purpose — a `ButtonRow` records one entry per
/// button, a `Separator` records nothing — because what a test needs is to name
/// the thing it expects on screen, not to reconstruct the tree.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrawnControlKind {
    Checkbox,
    Slider,
    Button,
    InfoText,
    Heading,
    Dropdown,
    Section,
}

/// One control a handler's tree actually drew — the generalisation of
/// [`DrawnDropdown`] to every [`ControlItem`] shape: which handler's pass drew
/// it, what it read as, and where it landed so a test can scroll to it and
/// click it.
///
/// Reported by the renderer, like [`ui_menu::DrawnMenuLeaf`], rather than
/// rebuilt by a test from the [`ControlItem`] — a test that walked the model
/// itself would agree with a renderer that had stopped drawing part of it.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnControlItem {
    /// The handler whose tree this item came from. `Some` for every item the
    /// current renderer draws; an `Option` so a control drawn outside any
    /// handler's tree can share the probe when one exists.
    pub handler: Option<OverlayKind>,
    pub label: String,
    pub kind: DrawnControlKind,
    pub rect: egui::Rect,
}

/// The line a **cross-section** pane's sidebar shows where a map pane's layer
/// list would be.
///
/// The panel is titled "Layers", so for a pane whose kind has none the honest
/// presentations are a tree of disabled ghosts or an explained absence. The
/// convention here is the absence: every entry in the tree is a layer drawn
/// through a projector, a section pane has none anywhere in its frame, and a
/// dozen disabled rows would bury the controls that do apply under ones that
/// never can. One line keeps the void from reading as a broken panel.
///
/// A 3D pane no longer sees this. It draws the map layers — its ground kinds
/// onto its floor and its colour scale onto its glass, each gated on its own
/// `is_overlay_enabled` — so it gets the rows
/// ([`PaneState::draws_map_layers`]). The wording says "cross-section" rather
/// than "map panes" because the old sentence became false the day the 3D view
/// grew a floor: it claimed the layers did not apply, while the floor was
/// quietly honouring a set the panel gave no way to reach.
pub(crate) const NON_MAP_LAYERS_NOTE: &str = "A cross-section has no map to draw layers on.";

/// The header over the section pane's sidebar block. Icon, two spaces, name —
/// the same shape as the loop transport's and the overlay rows' labels. The
/// icon is the top bar's own X-sec diagonal (`∕`): the demo's `✂` has no
/// glyph in egui's bundled fonts (see `ui_glyphs.rs`), and sharing the arm
/// toggle's glyph teaches which mode draws this pane's line.
pub(crate) const SECTION_SIDEBAR_HEADER: &str = "\u{2215}  Cross-section";

/// The identity line every pane kind's sidebar opens with: whose data this
/// pane shows and what the pane is, e.g. `KTLX · 3D volume`.
///
/// One function called before the kind branch rather than a line inside each
/// arm, so the three kinds keep one style and cannot drift into three
/// headers. For a map pane it is close to redundant — the panel under it is
/// full of self-describing map content — and that redundancy is the point:
/// the same line in the same place is what makes a converted pane's shorter
/// panel read as *this* panel showing fewer controls.
///
/// Reads only the `pane` it is handed: for the whole of the panel's pass the
/// active slot in `self.panes` holds a `mem::take` placeholder that reads as
/// a map pane on the default site.
fn render_pane_identity(ui: &mut egui::Ui, pane: &PaneState) {
    let kind = match pane.render_view() {
        rustdar_radar::types::RenderView::PlanView => "Map",
        rustdar_radar::types::RenderView::CrossSection => "Cross-section",
        rustdar_radar::types::RenderView::Volume => "3D volume",
    };
    ui.label(egui::RichText::new(format!("{} - {}", pane.site, kind)).strong());
}

/// Render a single declarative [`ControlItem`] into the UI, collecting any
/// resulting [`ControlUpdate`]s into `updates`.
fn render_control_item(
    ui: &mut egui::Ui,
    kind: OverlayKind,
    item: &ControlItem,
    updates: &mut Vec<(OverlayKind, ControlUpdate)>,
    probe: &mut ControlProbe,
) {
    match item {
        ControlItem::Toggle { id, label, enabled } => {
            let mut value = *enabled;
            let response = ui.checkbox(&mut value, label.as_str());
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::Checkbox, label, response.rect);
            if response.changed() {
                updates.push((
                    kind,
                    ControlUpdate {
                        id,
                        value: ControlValue::Bool(value),
                    },
                ));
            }
        }
        ControlItem::Heading { text } => {
            let response = ui.label(text.as_str());
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::Heading, text, response.rect);
            #[cfg(not(test))]
            let _ = response;
        }
        ControlItem::InfoText { text } => {
            let response = ui.label(egui::RichText::new(text.as_str()).small().weak());
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::InfoText, text, response.rect);
            #[cfg(not(test))]
            let _ = response;
        }
        ControlItem::ButtonRow { buttons } => {
            let any_highlighted = buttons.iter().any(|b| b.highlight);
            ui.horizontal_wrapped(|ui| {
                for btn in buttons {
                    let response = if any_highlighted {
                        ui.add_enabled(
                            btn.enabled,
                            egui::Button::new(btn.label.as_str()).selected(btn.highlight),
                        )
                    } else {
                        ui.add_enabled(btn.enabled, egui::Button::new(btn.label.as_str()))
                    };
                    #[cfg(test)]
                    probe.record_item(kind, DrawnControlKind::Button, &btn.label, response.rect);
                    if response.clicked() {
                        updates.push((
                            kind,
                            ControlUpdate {
                                id: btn.id,
                                value: ControlValue::Action,
                            },
                        ));
                    }
                }
            });
        }
        ControlItem::Separator => {
            ui.separator();
        }
        ControlItem::Dropdown {
            id,
            label,
            options,
            selected,
        } => {
            let mut sel = selected.clone();
            let original = sel.clone();
            ui.horizontal(|ui| {
                ui.label(label.as_str());
                // One formatter for both halves. `selected_text` used to be the
                // raw option *value*, so the collapsed box read `sbcin` and
                // `both` while the list it opened said "Surface-Based CIN" and
                // "Both".
                let shown = dropdown_option_label(options, &sel).to_owned();
                let combo = egui::ComboBox::from_id_salt(format!("{kind:?}_{id}"))
                    .selected_text(shown.as_str())
                    .show_ui(ui, |ui| {
                        for (value, display) in options {
                            ui.selectable_value(&mut sel, value.clone(), display.as_str());
                        }
                    });
                probe.record_dropdown(id, label, &shown, combo.response.rect);
                #[cfg(test)]
                probe.record_item(kind, DrawnControlKind::Dropdown, label, combo.response.rect);
            });
            if sel != original {
                updates.push((
                    kind,
                    ControlUpdate {
                        id,
                        value: ControlValue::String(sel),
                    },
                ));
            }
        }
        ControlItem::Slider {
            id,
            label,
            min,
            max,
            value,
            logarithmic,
            ..
        } => {
            let mut val = *value;
            let original = val;
            let row = ui.horizontal(|ui| {
                ui.label(label.as_str());
                let mut slider = egui::Slider::new(&mut val, *min..=*max);
                if *logarithmic {
                    slider = slider.logarithmic(true);
                }
                ui.add(slider);
            });
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::Slider, label, row.response.rect);
            #[cfg(not(test))]
            let _ = row;
            if (val - original).abs() > f64::EPSILON {
                updates.push((
                    kind,
                    ControlUpdate {
                        id,
                        value: ControlValue::Float(val),
                    },
                ));
            }
        }
        ControlItem::Section {
            label,
            collapsible,
            expanded,
            items,
        } => {
            if *collapsible {
                let collapsing = egui::CollapsingHeader::new(label.as_str())
                    .default_open(*expanded)
                    .show(ui, |ui| {
                        for child in items {
                            render_control_item(ui, kind, child, updates, probe);
                        }
                    });
                // The header's own rect, so a test can open a collapsed
                // section the way a user does — the children record
                // themselves only on a frame the body actually drew.
                #[cfg(test)]
                probe.record_item(
                    kind,
                    DrawnControlKind::Section,
                    label,
                    collapsing.header_response.rect,
                );
                #[cfg(not(test))]
                let _ = collapsing;
            } else {
                let group = ui.group(|ui| {
                    ui.label(egui::RichText::new(label.as_str()).strong());
                    for child in items {
                        render_control_item(ui, kind, child, updates, probe);
                    }
                });
                #[cfg(test)]
                probe.record_item(kind, DrawnControlKind::Section, label, group.response.rect);
                #[cfg(not(test))]
                let _ = group;
            }
        }
    }
}

/// Whether `item` is one of a handler's *master* controls — its heading, or
/// its whole-layer `enabled` toggle — which the inspector expresses as the
/// crumb and the "Show <layer>" toggle instead of rendering the handler's
/// copies.
///
/// One predicate, two callers: `render_overlay_controls_one` skips these and
/// the parity walk excludes them from its inventory. A copy in either place
/// would let the renderer and the audit disagree about what "every control"
/// means.
pub(crate) fn is_master_control(item: &ControlItem) -> bool {
    matches!(
        item,
        ControlItem::Heading { .. } | ControlItem::Toggle { id: "enabled", .. }
    )
}

impl Gui {
    /// The config a radar fetch on the active pane's behalf must use: the
    /// shared `radar.config` with the active pane's site substituted in.
    ///
    /// `config.site` is a *global* last-switched site — the frontend's
    /// `SwitchRadarSite` writes it even when layer sync is off — so with
    /// per-pane sites it can name a site the active pane is not viewing.
    /// Both Refresh entry points (status bar and menu) and the initial
    /// auto-fetch route through here rather than cloning the config verbatim,
    /// so they cannot drift apart.
    pub(super) fn active_pane_fetch_config(&self) -> RadarConfig {
        let mut config = self.radar.config.clone();
        config.site = self.active_pane().site.clone();
        config
    }

    /// Apply one event-shaped push from the App (WO-E2's seam).
    ///
    /// Each arm holds the body of the setter it replaced, compound effects
    /// included, moved verbatim — the spinner/backoff/zoom-latch couplings are
    /// load-bearing and documented on the arms. Events are applied at the call
    /// site's existing control-flow position, exactly where the setter call
    /// sat.
    pub fn apply(&mut self, event: crate::shell_api::GuiEvent) {
        use crate::shell_api::GuiEvent;
        match event {
            // Update the scan info for all panes viewing the given site.
            GuiEvent::ScanInfoForSite { site, info } => {
                let mut any_pane_took_it = false;
                for pane in &mut self.panes {
                    if pane.site == site {
                        pane.scan_info = Some(info.clone());
                        any_pane_took_it = true;
                    }
                }
                self.radar.fetching = false;
                self.auto_poll.on_success();
                // Only a scan someone is actually looking at is a reason to zoom to
                // radar scale. A volume for a site no pane is on — a fetch that landed
                // after the pane switched away — must not spend the one-shot latch,
                // let alone move every other pane's map.
                if any_pane_took_it {
                    self.claim_initial_zoom();
                }
            }
            // Apply scan info for a volume still being assembled from the real-time
            // chunk feed.
            //
            // Two differences from `ScanInfoForSite`, both deliberate.
            //
            // **It does not take the spinner down or reset the archive backoff.** Those
            // belong to a fetch someone is waiting on; this happens on its own every
            // few seconds. Clearing `fetching` here would cancel the spinner of a manual
            // Refresh still in flight and unblock the auto-poll queued behind it, and
            // `auto_poll.on_success()` would undo exactly the retreat the archive
            // fallback depends on.
            //
            // **It merges the product and elevation lists rather than replacing them.**
            // A partial volume knows only the cuts that have completed, so replacing
            // would shrink the tilt picker every few seconds and let it regrow — and
            // `PaneState::get_rendering_params` snaps to the nearest *listed* angle, so
            // every pane would walk up the VCP once per volume. It would also wipe the
            // Level III products and elevations that `poll_level3_results` accumulates
            // into `ScanInfo` in place, freezing every L3 pane until the volume closed.
            // The union keeps both and still gains a tilt the moment one first appears.
            //
            // At volume completion the caller uses `ScanInfoForSite` with a
            // plain `from_scan` instead, so the steady state after every volume is
            // exactly what the archive path produces — which is what makes a fallback
            // invisible.
            GuiEvent::ChunkScanInfo { site, info: fresh } => {
                let mut any_pane_took_it = false;
                for pane in &mut self.panes {
                    if pane.site != site {
                        continue;
                    }
                    any_pane_took_it = true;
                    let merged = match pane.scan_info.take() {
                        None => fresh.clone(),
                        Some(mut existing) => {
                            existing.timestamp = fresh.timestamp;
                            existing.vcp_number = fresh.vcp_number;
                            existing.status = fresh.status.clone();
                            for product in &fresh.available_products {
                                if !existing.available_products.contains(product) {
                                    existing.available_products.push(*product);
                                }
                            }
                            existing.available_products.sort_by_key(|p| p.sort_order());
                            for (product, angles) in &fresh.product_elevations {
                                let known =
                                    existing.product_elevations.entry(*product).or_default();
                                for angle in angles {
                                    if !known.iter().any(|k| (k - angle).abs() < 0.05) {
                                        known.push(*angle);
                                    }
                                }
                                known.sort_by(|a, b| a.total_cmp(b));
                            }
                            existing
                        }
                    };
                    pane.scan_info = Some(merged);
                }
                // Same guard as `ScanInfoForSite`, for the same reason: the
                // chunk feed keeps delivering a site's volume for a round or two after
                // the last pane on it switched away, and that data nobody is looking at
                // must not spend the one-shot latch.
                if any_pane_took_it {
                    self.claim_initial_zoom();
                }
            }
            // Update the scan info for a specific pane.
            GuiEvent::ScanInfoForPane { pane_idx, info } => {
                if let Some(pane) = self.panes.get_mut(pane_idx) {
                    pane.scan_info = Some(info);
                }
            }
            // Set fetching status.
            GuiEvent::Fetching(fetching) => {
                self.radar.fetching = fetching;
            }
            // Set an error message. The spinner comes down and the archive
            // backoff advances with it — an error ends the wait it belonged to.
            GuiEvent::Error(error) => {
                self.radar.error_message = Some(error);
                self.radar.fetching = false;
                self.auto_poll.on_error();
            }
            // Set the radar config, keeping the Set Time dialog's strings in
            // sync with it.
            GuiEvent::RadarConfig(config) => {
                let date = config.timestamp.format("%Y-%m-%d").to_string();
                let time = config.timestamp.format("%H:%M:%S").to_string();
                self.radar.config = config;
                self.time_dialog.date_string = date;
                self.time_dialog.time_string = time;
            }
            // Set live/historic viewing mode for a specific pane.
            GuiEvent::ViewingLiveForPane { pane_idx, live } => {
                if let Some(pane) = self.panes.get_mut(pane_idx) {
                    pane.viewing_live = live;
                }
            }
            // Install what can draw 3D panes, or take it away.
            //
            // Sent by the frontend when a renderer is created and, with `None`,
            // when one is lost. Every 3D pane on screen picks the change up on
            // the next frame with no other bookkeeping, because the painter is
            // consulted afresh inside each pane's arm rather than cached
            // anywhere.
            GuiEvent::VolumePainter(painter) => {
                self.volume_painter = painter;
            }
        }
    }

    /// Apply one frame's facts, composed by the App from state it already
    /// owns, once per frame immediately before [`Gui::ui`]. Plain stores —
    /// every compound, event-shaped effect goes through [`Gui::apply`].
    pub fn apply_frame_inputs(&mut self, inputs: crate::shell_api::FrameInputs<'_>) {
        self.safe_area_insets = inputs.safe_area_insets;
        self.supports_exit = inputs.supports_exit;
        self.loop_frame_budget = inputs.loop_frame_budget;
        self.location_settings_available = inputs.location_settings_available;
        let (permission, active) = inputs.location;
        self.location_permission = permission;
        self.location_active = active;
        // The instant travels WITH the fix: `user_fix_at` answers "when did
        // this app last hear anything", stamped once at arrival (see the
        // field). Re-stamping it per frame would hold the settings pane's
        // staleness question at zero forever. `None` clears both halves —
        // consent for the position on screen has gone away, and the last
        // position delivered under the old permission must go with it.
        match inputs.gps {
            Some((fix, at)) => {
                self.user_fix = Some(fix);
                self.user_fix_at = Some(at);
            }
            None => {
                self.user_fix = None;
                self.user_fix_at = None;
            }
        }
        self.user_heading = inputs.user_heading;
        self.catalogue_pending = inputs.catalogue_pending;
        self.chunk_status = inputs.chunk_status;
        self.current_volumes = inputs.current_volumes.clone();
        self.floor_tile_zoom_bias = inputs.floor_tile_zoom_bias;
    }

    /// Whether live panes should be fed from the real-time chunk bucket.
    ///
    /// Persisted as `UiConfig::live_chunks`, default on. Turning it off leaves
    /// live mode on the archive path, which is the same code that serves the
    /// time picker and history — so the fallback is never a separate,
    /// less-exercised route.
    pub fn live_chunks_enabled(&self) -> bool {
        self.live_chunks
    }

    /// Set by the settings UI and by the config load.
    pub fn set_live_chunks(&mut self, enabled: bool) {
        self.live_chunks = enabled;
    }

    /// Whether to subscribe to the push-notification service.
    ///
    /// Purely an accelerator: it makes a chunk fetch start the moment the chunk
    /// exists rather than on the next five-second tick. Turning it off, or
    /// failing to reach the service, leaves the polling feed running exactly as
    /// it is — which is why it can default on without making a third-party
    /// deployment load-bearing.
    pub fn chunk_notifications_enabled(&self) -> bool {
        self.chunk_notifications
    }

    pub fn set_chunk_notifications(&mut self, enabled: bool) {
        self.chunk_notifications = enabled;
    }

    /// Where the notifier service lives.
    ///
    /// Settable because it is one person's deployment rather than a NOAA
    /// endpoint: a user behind a network that cannot reach it, or one running
    /// their own, needs to be able to point elsewhere. An empty value falls back
    /// to the default rather than disabling the feature, so a cleared box is not
    /// a silent off switch.
    pub fn notifier_endpoint(&self) -> &str {
        if self.notifier_endpoint.trim().is_empty() {
            crate::DEFAULT_NOTIFIER_ENDPOINT
        } else {
            self.notifier_endpoint.trim()
        }
    }

    pub fn set_notifier_endpoint(&mut self, endpoint: impl Into<String>) {
        self.notifier_endpoint = endpoint.into();
    }

    pub fn chunk_status(&self) -> &ChunkFeedStatus {
        &self.chunk_status
    }

    /// The stamp of `site`'s current volume, if this build holds one at all.
    ///
    /// `None` is an ordinary state and the reason a 3D pane says it is
    /// waiting: it is what a site looks like before its first volume — archive
    /// fetch or first sealed sweeps — has arrived.
    pub fn current_volume_for(&self, site: &str) -> Option<CurrentVolumeStamp> {
        self.current_volumes.get(site).copied()
    }

    /// The distinct sites some pane is watching live — the unit the chunk feed
    /// and the archive auto-poll both work in.
    pub fn live_sites(&self) -> Vec<String> {
        let mut sites: Vec<String> = Vec::new();
        for pane in self.panes.iter().take(self.pane_layout.pane_count) {
            if pane.viewing_live && !sites.iter().any(|s| s == &pane.site) {
                sites.push(pane.site.clone());
            }
        }
        sites
    }

    /// Whether a fetch someone is waiting on is in flight.
    ///
    /// Global rather than per-site, and it gates `check_auto_polls` — so any
    /// path that raises it has to lower it on every exit.
    pub fn fetching(&self) -> bool {
        self.radar.fetching
    }

    /// The Set Time dialog's body, host-free: the window above wraps it on
    /// the wider widths and the phone sheet's Time page hosts it verbatim,
    /// so the two presentations cannot drift.
    pub(super) fn render_time_dialog_body(&mut self, ui: &mut egui::Ui) -> Option<GuiAction> {
        let mut action = None;
        ui.vertical_centered(|ui| {
            ui.heading("Select Time");
            ui.add_space(10.0);

            ui.label("Date:");
            ui.text_edit_singleline(&mut self.time_dialog.date_string);

            ui.add_space(5.0);

            ui.label("Time:");
            ui.text_edit_singleline(&mut self.time_dialog.time_string);

            ui.add_space(10.0);

            if ui.button("Use Current Time").clicked() {
                self.radar.config.timestamp = chrono::Local::now().naive_local();
                self.time_dialog.date_string =
                    self.radar.config.timestamp.format("%Y-%m-%d").to_string();
                self.time_dialog.time_string =
                    self.radar.config.timestamp.format("%H:%M:%S").to_string();
            }

            ui.add_space(15.0);

            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    // Try to parse the date and time strings
                    let datetime_str = format!(
                        "{} {}",
                        self.time_dialog.date_string, self.time_dialog.time_string
                    );
                    if let Ok(timestamp) =
                        chrono::NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%d %H:%M:%S")
                    {
                        self.radar.config.timestamp = timestamp;
                        if let Some(pane) = self.panes.get_mut(self.active_pane) {
                            pane.viewing_live = false;
                        }
                        action = Some(GuiAction::FetchRadarScan(self.radar.config.clone()));
                    }
                    self.time_dialog.show = false;
                }

                if ui.button("Cancel").clicked() {
                    // Restore the original strings from the current config
                    self.time_dialog.date_string =
                        self.radar.config.timestamp.format("%Y-%m-%d").to_string();
                    self.time_dialog.time_string =
                        self.radar.config.timestamp.format("%H:%M:%S").to_string();
                    self.time_dialog.show = false;
                }
            });
        });
        action
    }

    /// Whether the layers panel is on screen this frame, in either form.
    ///
    /// One question with two answers by width: on Expanded the panel is the
    /// sidebar, open unless [`Self::stack_open`] says otherwise; elsewhere it
    /// is the drawer, closed unless opened. The top bar's Layers toggle reads
    /// and writes through this split, so it is the one definition of "open".
    pub(super) fn layers_panel_visible(&self) -> bool {
        if self.layout.width.has_persistent_sidebar() {
            self.stack_open.unwrap_or(true)
        } else {
            self.drawer_open
        }
    }

    /// The cross-section pane's own sidebar block: what the pane is cutting
    /// along, in the same header-then-indent shape as every other block in the
    /// panel — the loop transport, the 3D view's knobs — so a section pane's
    /// sidebar reads as the normal panel showing this pane's controls rather
    /// than as a stub with most of the panel missing.
    ///
    /// It states rather than steers: a line is aimed by drawing it on a map,
    /// and a sidebar editor for its endpoints would be a second, worse way to
    /// do the same thing. The hint names the real control by its own menu
    /// label, so renaming the menu entry cannot strand the hint pointing at a
    /// control that no longer exists.
    ///
    /// Reads only the `pane` it is handed, never `self.panes` — the caller
    /// holds the active pane out of the vector for the whole panel pass.
    fn render_section_controls(&self, ui: &mut egui::Ui, pane: &PaneState) {
        ui.add_space(6.0);
        ui.separator();
        ui.label(SECTION_SIDEBAR_HEADER);
        ui.indent("section_controls", |ui| {
            match pane.cross_section().and_then(|section| section.line) {
                Some(line) => {
                    // The ends are named A and B because that is what the map
                    // paints at them; the length is the same haversine the
                    // hover readout uses rather than a second copy of it.
                    // ASCII prose throughout — the M8 glyph rules in
                    // `ui_glyphs.rs`.
                    let (_, km) = rustdar_radar::beam::site_bearing_range_km(
                        line.a().lat,
                        line.a().lon,
                        line.b().lat,
                        line.b().lon,
                    );
                    let unit = self.preferences.distance;
                    ui.label(format!(
                        "A - B: {:.0} {}",
                        unit.convert_from_km(km),
                        unit.suffix()
                    ));
                }
                None => {
                    ui.label("No line drawn yet");
                }
            }
            ui.label(
                egui::RichText::new(format!(
                    "Aim it: turn on \"{}\" and drag across a map.",
                    ui_menu::DRAW_CROSS_SECTION_LABEL
                ))
                .small()
                .weak(),
            );
        });
    }

    /// Render the radar product picker, and the tilt picker where a tilt means
    /// anything.
    ///
    /// Hosted by the inspector's Pane-properties body — the only caller since
    /// the stack/inspector split — but kept here beside the identity line and
    /// the section block it shares the panel with. The combo salts keep the
    /// `layers_` prefix they have always had, so the widget state egui stores
    /// under them survived the move.
    pub(super) fn render_radar_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        combo_width: f32,
        id_prefix: &str,
    ) {
        // The Radar overlay toggle governs whether the *map* draws the radar
        // image over its tiles, which is not a question a pane with no map has.
        // Gated on it, a section or a volume pane converted while the toggle
        // happened to be off would have no way to choose a product at all — a
        // control that is simply absent, for a reason nothing on screen explains.
        if pane.is_map() && !pane.is_overlay_enabled(OverlayKind::Radar) {
            return;
        }
        // A whole-volume pane has no tilt to pick: it reads the entire ladder,
        // which is what `RenderView::reads_whole_volume` means, so every entry in
        // the combo would select the same picture. `selected_elevation` stays on
        // the pane, inert, so going back to the plan view restores the tilt it
        // had.
        let offer_tilt = !pane.render_view().reads_whole_volume();
        // Reported the way `time_step_sel` is, and for the same reason: a test
        // rebuilding these ids from the same format strings could agree with a
        // panel that drew neither control. *Which* of the two appear is how a test
        // sees the product picker survive a conversion while the tilt picker does
        // not.
        #[cfg(test)]
        let probes = &mut self.probes.widget_id_probes;
        {
            ui.indent(format!("{id_prefix}radar_controls"), |ui| {
                if let Some(scan_info) = &pane.scan_info {
                    let prev_product = pane.selected_product;
                    // The combo's body is the shared product list — the same
                    // function the product pill's popover renders — so the
                    // two routes offer one inventory by construction
                    // (`ui_pills.rs`'s module note).
                    let product_combo =
                        egui::ComboBox::from_id_salt(format!("{id_prefix}product_sel"))
                            .selected_text(pane.selected_product.name())
                            .width(combo_width)
                            .show_ui(ui, |ui| {
                                pills::product_list_ui(
                                    ui,
                                    &scan_info.available_products,
                                    pane.selected_product,
                                )
                                .picked
                            });
                    if let Some(Some(picked)) = product_combo.inner {
                        pane.selected_product = picked;
                    }
                    #[cfg(test)]
                    probes.push(("product_sel", product_combo.response.id));
                    if prev_product != pane.selected_product {
                        pane.selected_elevation = 0.0;
                    }

                    // The tilt picker is drawn for every listed product, including
                    // one whose angles have not arrived yet.
                    //
                    // Skipping it while the list was empty made the control vanish
                    // and the panel reflow around it — for a Level III product on
                    // first selection, and again on every archive poll, which
                    // rebuilds `ScanInfo` from the volume alone and so drops the
                    // angles `poll_level3_results` had filled in. Present but
                    // unpopulated is the honest state: the product is selected, the
                    // selection stands (`get_rendering_params` leaves it unsnapped),
                    // and there is nothing to choose between yet.
                    if let Some(elevations) = offer_tilt
                        .then(|| scan_info.product_elevations.get(&pane.selected_product))
                        .flatten()
                    {
                        let selected_angle = elevations
                            .iter()
                            .min_by(|a, b| {
                                ((**a - pane.selected_elevation).abs())
                                    .total_cmp(&((**b - pane.selected_elevation).abs()))
                            })
                            .copied()
                            .unwrap_or(pane.selected_elevation);

                        let combo = egui::ComboBox::from_id_salt(format!("{id_prefix}elev_sel"))
                            .selected_text(format!("{:.1}\u{b0}", selected_angle))
                            .width(combo_width);
                        let elev_combo = if elevations.is_empty() {
                            // Nothing to pick from, so the control is inert rather
                            // than an empty menu that opens onto nothing.
                            let scope = ui.add_enabled_ui(false, |ui| combo.show_ui(ui, |_| {}));
                            let id = scope.inner.response.id;
                            scope
                                .response
                                .on_hover_text("Waiting for this product's data");
                            id
                        } else {
                            // The shared tilt list — the tilt pill popover's
                            // own body — for the same one-inventory reason
                            // as the product combo above.
                            let shown = combo.show_ui(ui, |ui| {
                                pills::tilt_list_ui(ui, elevations, pane.selected_elevation).picked
                            });
                            if let Some(Some(angle)) = shown.inner {
                                pane.selected_elevation = angle;
                            }
                            shown.response.id
                        };
                        // Both branches, so the probe reports the control existing
                        // rather than the elevation list happening to be populated.
                        #[cfg(test)]
                        probes.push(("elev_sel", elev_combo));
                        #[cfg(not(test))]
                        let _ = elev_combo;
                    }
                } else {
                    ui.label("No scan loaded");
                }
            });
        }
    }

    /// Turn an overlay on or off for `pane` — **both halves**, which is the
    /// whole discipline.
    ///
    /// `render_overlay_controls_one` reloads the handlers from
    /// `overlay_configs` every frame and saves the enabled map back over
    /// `enabled_overlays`, so a change that never reached the config is
    /// undone on the next frame. Everything that flips a layer goes through
    /// here: [`Self::set_active_pane_overlay`] for callers outside a take
    /// window, and the stack's eye / the inspector's Show-toggle directly,
    /// with the pane they hold `mem::take`n out of the vector — where
    /// indexing `self.panes` would write the placeholder and lose the click.
    ///
    /// An associated function rather than a method so it can borrow the
    /// registry while the caller holds the taken pane.
    ///
    /// # The texture goes with the toggle
    ///
    /// Switching a layer off used to be a `bool` and nothing else. The
    /// handlers' own `set_enabled` is a no-op for most kinds
    /// (`OverlayHandler::set_enabled`'s default body is empty), and the pane's
    /// [`overlay_textures`](PaneState::overlay_textures) map was released only
    /// by `Gui::clear_graphics_state` — i.e. on a lost GPU context. A pane that
    /// is on screen and drawing a map recovered anyway, because the viewport
    /// loop in `ui_map_pane` clears a disabled kind's cache on the next frame it
    /// paints; the panes that never reach that loop did not. A pane hidden by a
    /// split going from four to one, or converted to a cross-section, is not
    /// drawn by it at all and kept a full-size RGBA texture per kind, per pane,
    /// for the rest of the session.
    ///
    /// So the release happens *here*, where the decision is made, rather than
    /// being left to whether the pane happens to be painted afterwards. See
    /// [`PaneState::release_disabled_overlay_textures`] for what it lets go and
    /// [`PaneState::overlay_texture_releasable`] for why the radar raster is not
    /// part of it.
    ///
    /// **This does not make a hidden pane cheap.** What it recovers is the kinds
    /// a user *switched off* — on any pane, whether or not that pane is being
    /// painted. A hidden pane whose five layers are all still on keeps five
    /// pane-sized textures exactly as before, and on the 4→1 split above that is
    /// the larger share of what is resident. Releasing a pane's live layers when
    /// it leaves the layout is a separate rule with a separate trigger, and a
    /// re-render on every re-split to pay for it; it is not this.
    pub(super) fn write_pane_overlay(
        overlays: &mut OverlayRegistry,
        pane: &mut PaneState,
        kind: OverlayKind,
        on: bool,
    ) {
        if !pane.overlay_configs.is_empty() {
            overlays.load_pane_configs(&pane.overlay_configs);
        }
        overlays.set_enabled(kind, on);
        pane.overlay_configs = overlays.save_pane_configs();
        pane.enabled_overlays = overlays.save_enabled_map();
        // After the map is saved, not before: the reconciliation reads it.
        pane.release_disabled_overlay_textures();
    }

    /// [`Self::write_pane_overlay`] aimed at the active pane, for callers
    /// outside every `mem::take` window — the menu dispatcher, today.
    fn set_active_pane_overlay(&mut self, kind: OverlayKind, on: bool) {
        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);
        Self::write_pane_overlay(&mut self.overlays, &mut pane, kind, on);
        self.panes[self.active_pane] = pane;
    }

    /// Select `kind`'s options in the inspector and make sure it is open —
    /// what a stack row click means (plan §3.8).
    pub(super) fn select_layer(&mut self, kind: OverlayKind) {
        self.insp_scroll_reset = self.inspector_sel != InspectorSelection::Layer(kind);
        self.inspector_sel = InspectorSelection::Layer(kind);
        self.insp_open = true;
    }

    /// Select the active pane's properties in the inspector and make sure it
    /// is open — the stack header's click, and the inspector crumb's `Pane N`
    /// segment. The pills are the primary pane-properties route now (each
    /// pill *is* one of the properties; the pane-number pill activates, the
    /// kind pill converts) — these two stay as harmless secondary ways into
    /// the inspector body that shows them all at once.
    pub(super) fn select_pane_props(&mut self) {
        self.insp_scroll_reset = self.inspector_sel != InspectorSelection::PaneProps;
        self.inspector_sel = InspectorSelection::PaneProps;
        self.insp_open = true;
    }

    /// Open the inspector on the App › Settings body — what the menu's
    /// Settings… entry does, and the state a `✕` deselect returns to.
    pub fn open_settings(&mut self) {
        self.insp_scroll_reset = self.inspector_sel != InspectorSelection::AppSettings;
        self.inspector_sel = InspectorSelection::AppSettings;
        self.insp_open = true;
    }

    /// Whether the settings body is on screen: the inspector is open and
    /// showing App › Settings.
    ///
    /// The successor to the old `show_settings` field, and the one thing the
    /// frontend still reads: the location-permission gate polls faster while
    /// this is true, so the pane that renders the permission is looking at a
    /// fresh copy of it.
    pub fn settings_visible(&self) -> bool {
        self.insp_open && self.inspector_sel == InspectorSelection::AppSettings
    }

    /// Returns `true` if any pane has the given overlay kind enabled.
    ///
    /// Used for auto-poll decisions: we should fetch data for an overlay
    /// if at least one pane wants to display it.
    ///
    /// # Why a pane with nowhere to draw does not count, while keeping its toggles
    ///
    /// This and [`Self::first_pane_with_overlay_enabled`] ask "is this overlay
    /// being *drawn* anywhere?", and every overlay is a layer over map tiles,
    /// geo-positioned against a projector a pane may not have. So a pane with no
    /// ground anywhere in its frame must not keep an overlay's auto-poll timer
    /// running, or be the pane a `FetchOverlay` is attributed to.
    ///
    /// [`PaneState::draws_ground`], **not** `is_map`, and the difference is a
    /// live bug rather than a nicety. A 3D pane's floor is its own map: it runs
    /// the same `Map::show` and the same `render_pane_map_content` a plan view
    /// does, so it draws these very layers and emits its own `RenderOverlay` for
    /// each of them at its own bounds. Filtered on `is_map`, a **lone** 3D pane
    /// answered no here — so nothing polled, the alerts and discussions on its
    /// floor never refreshed as they issued and expired, and the Map floor
    /// checkbox's hover text promised in as many words that they would.
    ///
    /// A pane whose floor is switched off still answers no, which is the half
    /// that was right: there is nothing on screen to refresh.
    ///
    /// Its `enabled_overlays` is deliberately left alone rather than cleared,
    /// which is the same choice `set_kind` makes about the viewport and the tilt:
    /// it is the user's remembered answer to "which layers do I want", and it
    /// becomes meaningful again the instant the pane is converted back. Filtering
    /// the readers keeps both properties; clearing the record would lose one.
    ///
    /// Both are called from `check_auto_polls`, at the very top of [`Self::ui`]
    /// before any pane is `mem::take`n, so reading the view through `self.panes`
    /// is safe here — see [`PaneContent`](crate::pane::PaneContent)'s module docs
    /// for why that is worth checking rather than assuming.
    pub fn any_pane_has_overlay_enabled(&self, kind: OverlayKind) -> bool {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .any(|p| p.draws_ground() && p.is_overlay_enabled(kind))
    }

    /// Returns the index of the first pane that has the given overlay kind enabled,
    /// or `None` if no pane has it enabled.
    ///
    /// Panes with no ground to draw it on are skipped; see
    /// [`Self::any_pane_has_overlay_enabled`].
    pub fn first_pane_with_overlay_enabled(&self, kind: OverlayKind) -> Option<usize> {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .position(|p| p.draws_ground() && p.is_overlay_enabled(kind))
    }

    /// Get the active pane (immutable).
    pub fn active_pane(&self) -> &PaneState {
        &self.panes[self.active_pane]
    }

    /// Index of the active pane, for the `GuiAction`s that address one by index.
    pub fn active_pane_idx(&self) -> usize {
        self.active_pane
    }

    /// Get the active pane (mutable).
    pub fn active_pane_mut(&mut self) -> &mut PaneState {
        &mut self.panes[self.active_pane]
    }

    /// Every pane the layout is currently showing, in pane-index order.
    ///
    /// Splitting to fewer panes leaves the extra `PaneState`s in the vector so a
    /// re-split remembers them, and they are neither drawn nor updated — so the
    /// slice stops at `pane_count`, and code that acts on "all panes" must go
    /// through here rather than iterating `panes` directly.
    ///
    /// One caveat, shared with [`Self::pane`] and [`Self::pane_mut`]: while the
    /// settings panel is drawing, the active pane is held out of the vector by
    /// `mem::take` and its slot is a default `PaneState`. Nothing that reaches these
    /// accessors runs in that window — the loop and scan paths run either side of
    /// the egui pass, never inside it — but a future caller inside the UI pass would
    /// read a blank pane rather than the live one.
    pub fn panes(&self) -> &[PaneState] {
        &self.panes[..self.visible_pane_count()]
    }

    /// The off-screen strips this frame's 3D panes drew their own maps into,
    /// as rects in **points**.
    ///
    /// This is the mirror pass's guest list. A 3D pane's map floor is its own
    /// `Map::show`, drawn a second time into a rect *below the frame* — see
    /// `ui_map`'s `floor_strip_for` — and the mirror pass copies exactly these
    /// rects, so the sidebar, the top bar, the panes' chrome and the volumes
    /// themselves never land in it.
    ///
    /// Empty means there is nothing to mirror, and the frontend skips the pass
    /// entirely rather than clearing a texture nobody reads.
    ///
    /// The floor toggle is re-read from the live pane here rather than inferred
    /// from the presence of a recorded affine. `render_panes` already prunes
    /// the entry when a pane stops wanting a floor, so this is belt and braces
    /// — but this is a `pub` reader the frontend calls at a point in the frame
    /// of its own choosing, and copying a hidden pane's map is not a failure
    /// anyone would think to look for in a guest list.
    pub fn mirror_source_rects(&self) -> Vec<egui::Rect> {
        let mut rects: Vec<egui::Rect> = Vec::new();
        for (idx, pane) in self.panes().iter().enumerate() {
            let Some(volume) = pane.volume() else {
                continue;
            };
            if volume.hide_floor {
                continue;
            }
            let Some(geo) = self.map_pane_geo.get(&idx) else {
                continue;
            };
            // Strips are disjoint by construction — they are the pane rects
            // under one uniform translation — so this can only dedupe an
            // entry that has not been re-recorded yet. Compared by value
            // rather than by index because the rect is what the pass uses.
            if !rects.contains(&geo.rect) {
                rects.push(geo.rect);
            }
        }
        rects
    }

    /// How much of egui's coordinate space the mirror has to cover this frame,
    /// in **points**: the frame itself, plus however far below it the 3D panes'
    /// off-screen map strips reach.
    ///
    /// The frame is included even though nothing in it is ever copied — after
    /// the strips landed, every rect in [`Self::mirror_source_rects`] is below
    /// the frame's bottom edge. It has to be: `egui_wgpu::Renderer::render`
    /// hardcodes `set_viewport(0, 0, size_in_pixels)` and its vertex shader
    /// divides by `screen_size_in_points`, so the attachment always spans
    /// egui's space from the origin. A texture that started at the strips would
    /// need an origin egui has no way to be told about.
    ///
    /// That is the whole of what the strip design costs: a frame's worth of
    /// texels that are cleared and never drawn into, bounding the mirror at
    /// twice the frame. `mirror_size_points_for` keeps it as far under that
    /// bound as the layout allows.
    ///
    /// `Vec2::ZERO` before the first frame has laid a pane grid out, which
    /// cannot coincide with a real answer — the frame is always in it.
    pub fn mirror_size_points(&self) -> egui::Vec2 {
        self.mirror_size_points
    }

    /// The tile zoom bias for one pane: the frame's bias if this pane is
    /// drawing a floor strip, zero otherwise.
    ///
    /// Per-pane rather than global on purpose. The extra detail is only ever
    /// looked at through a floor, so a map pane with no 3D view of its own
    /// would pay four times the fetches — against the one
    /// `tile_source::TILE_CACHE_ENTRIES` LRU every pane shares — for a picture
    /// the user is already seeing at its native scale.
    pub(crate) fn tile_zoom_bias_for_pane(&self, pane_idx: usize) -> u8 {
        if self.floor_tile_zoom_bias == 0 || !self.is_floor_source(pane_idx) {
            return 0;
        }
        if self.floor_tile_working_set(self.floor_tile_zoom_bias)
            > crate::tile_source::TILE_CACHE_ENTRIES.get()
        {
            return 0;
        }
        self.floor_tile_zoom_bias
    }

    /// Whether this pane draws a map into a floor strip — a 3D pane with the
    /// floor showing.
    fn is_floor_source(&self, pane_idx: usize) -> bool {
        self.panes()
            .get(pane_idx)
            .and_then(PaneState::volume)
            .is_some_and(|volume| !volume.hide_floor)
    }

    /// How many tiles every floor strip together would keep resident at `bias`,
    /// across the raster layers each of them draws.
    ///
    /// Summed over panes rather than checked per pane because
    /// `tile_source::TILE_CACHE_ENTRIES` is **one** LRU for the whole
    /// application: two strips that each fit it individually still evict each
    /// other. This is what stops a bias being taken on a frame where it would
    /// thrash — a large window, or a split with two 3D views in it.
    fn floor_tile_working_set(&self, bias: u8) -> usize {
        self.panes()
            .iter()
            .enumerate()
            .filter(|(idx, _)| self.is_floor_source(*idx))
            .map(|(idx, pane)| {
                // The basemap always, the label tiles only when the layer is on.
                let layers = 1 + usize::from(pane.is_overlay_enabled(OverlayKind::CityLabels));
                let rect = self
                    .map_pane_geo
                    .get(&idx)
                    .map_or(egui::Rect::ZERO, |geo| geo.rect);
                crate::tiles::tiles_resident_for(rect, bias, layers)
            })
            .sum()
    }

    /// How many panes are *remembered*, including the ones the current split is
    /// not showing.
    ///
    /// Almost every caller wants [`panes`](Self::panes) instead: a hidden pane
    /// is not on screen, does not want a render dispatched for it and does not
    /// take part in any sync. The exception is the GPU-handle lifecycle.
    /// [`clear_graphics_state`](Self::clear_graphics_state) deliberately reaches
    /// every remembered pane — a handle belonging to a pane the user split away
    /// from is just as invalid once the context is gone — so whatever puts those
    /// handles *back* has to reach exactly as far, or a pane split away and
    /// split back to comes up holding a released texture and no way to ask for
    /// another.
    pub fn remembered_pane_count(&self) -> usize {
        self.panes.len()
    }

    /// [`Self::panes`] for the paths that update pane state (loop frames, scan
    /// info), with the same bound.
    pub fn panes_mut(&mut self) -> &mut [PaneState] {
        let count = self.visible_pane_count();
        &mut self.panes[..count]
    }

    /// `pane_count` clamped to what the vector actually holds. The two are kept in
    /// step by every path that changes the layout, but slicing past the end would
    /// panic, and no pane update is worth a crash.
    fn visible_pane_count(&self) -> usize {
        self.pane_layout.pane_count.min(self.panes.len())
    }

    /// Get a specific pane by index (immutable), or `None` if out of bounds.
    pub fn pane(&self, idx: usize) -> Option<&PaneState> {
        self.panes.get(idx)
    }

    /// Get a specific pane by index (mutable), or `None` if out of bounds.
    pub fn pane_mut(&mut self, idx: usize) -> Option<&mut PaneState> {
        self.panes.get_mut(idx)
    }

    /// Ask for pane `pane_idx` to show `view`, taking effect at the end of the
    /// frame.
    ///
    /// **The only route by which the UI may change what a pane draws.**
    /// `PaneState::set_kind` and `PaneState::set_map_render` are the mechanisms
    /// and stay reachable for the config loader and for test fixtures; nothing
    /// drawing a frame calls them, because two UI paths hold the pane they would
    /// write out of the vector as a `mem::take` placeholder about to be thrown
    /// away. The menu dispatcher, as it happens, is *not* inside either window
    /// today — a direct write from it would work — so this is one rule for both
    /// dispatch and the writers inside `render_panes`' take, rather than a fix
    /// for a live bug on this path. The
    /// [`pending_pane_view`](Self::pending_pane_view) field lays out which is
    /// which.
    ///
    /// Out-of-range indices are recorded and dropped on application rather than
    /// refused here, so a caller inside the UI pass never has to know whether the
    /// vector currently holds the pane it is drawing.
    pub(crate) fn request_pane_view(
        &mut self,
        pane_idx: PaneId,
        view: rustdar_radar::types::RenderView,
    ) {
        self.pending_pane_view = Some((pane_idx, view));
    }

    /// Grow or shrink the layout to `count` panes, seeding any new ones, and
    /// report whether the layout actually reached that count.
    ///
    /// **The one writer of the pane count.** Factored out of the pane picker
    /// rather than left inline because the picker is no longer the only thing
    /// that changes it: a region drag on a layout with room in it opens a 3D pane
    /// beside the map, and a section line does the same for a cross-section.
    /// Three copies of this would be three places to remember
    /// [`Self::initialize_pane_enabled`], and forgetting it in one of them
    /// produces a pane that draws no overlays at all — Radar included — which
    /// reads as a broken pane rather than as a missing seed. It is not a compile
    /// error and not a panic; it is a blank pane, from one missing call.
    ///
    /// **The caller must have put any `mem::take`n pane back first.** This indexes
    /// `self.panes` directly, and a taken pane's slot holds a default map pane
    /// whose site a new pane would then be seeded from.
    ///
    /// Returns `false` when the layout could not reach `count` —
    /// `PaneLayout::for_count` clamps, so asking for more than it allows leaves
    /// the count where it was rather than producing panes no rect is drawn for.
    /// The active-pane bound is checked against the **clamped** count for the same
    /// reason: comparing against the requested one would leave `active_pane`
    /// pointing past the end of a layout that refused to grow.
    fn set_pane_count(&mut self, count: usize) -> bool {
        let active_site = self.panes[self.active_pane].site.clone();
        let active_scan_info = self.panes[self.active_pane].scan_info.clone();
        while self.panes.len() < count {
            let mut new_pane = PaneState::with_site(active_site.clone());
            new_pane.scan_info = active_scan_info.clone();
            self.panes.push(new_pane);
        }
        // A pane born here has empty overlay maps, and `is_overlay_enabled` reads
        // a missing entry as *off* — so with layer sync disabled it would draw no
        // overlays at all, Radar included. Seed it from the handlers, which hold
        // the active pane's state (reloaded at the end of every frame in
        // `Gui::ui`), the same way startup does.
        self.initialize_pane_enabled();
        self.pane_layout = PaneLayout::for_count(count);
        if self.active_pane >= self.pane_layout.pane_count {
            self.active_pane = 0;
        }
        self.pane_layout.pane_count == count
    }

    /// The in-flight section handle drag, if any — the state both armed-drag
    /// setters must clear, and the tests' way of watching them do it.
    #[cfg(test)]
    pub(crate) fn section_edit_drag_for_test(
        &self,
    ) -> Option<crate::ui_section_edit::SectionEditDrag> {
        self.section_edit_drag
    }

    /// Whether the cross-section draw is armed.
    pub fn section_draw_armed(&self) -> bool {
        self.section_draw_armed
    }

    /// Arm or disarm the cross-section draw.
    ///
    /// Disarming drops any half-drawn line: the anchor means nothing once the
    /// mode it belongs to is off, and leaving it would make re-arming resume a
    /// drag the user abandoned minutes ago.
    ///
    /// Arming disarms the **region pick**, and that exclusion is the reason
    /// both modes go through a setter rather than writing the flag: one press
    /// on one map pane cannot be both a line and a box, and the two share one
    /// detector, so a frame with both armed would hand one gesture to two
    /// interpreters. The mode asked for last wins, which is the only reading a
    /// user can predict from a control they just operated.
    pub(crate) fn set_section_draw_armed(&mut self, armed: bool) {
        self.section_draw_armed = armed;
        if armed {
            // An endpoint drag cannot share a map with an armed draw, and the
            // mode was asked for last.
            self.section_edit_drag = None;
            self.region_pick_armed = false;
            self.region_drag = None;
        } else {
            self.section_anchor = None;
        }
    }

    /// Whether the 3D region pick is armed.
    pub fn region_pick_armed(&self) -> bool {
        self.region_pick_armed
    }

    /// Arm or disarm the 3D region pick.
    ///
    /// The mirror of [`Self::set_section_draw_armed`], and deliberately the
    /// same shape: disarming drops a half-dragged box for the reason that one
    /// drops a half-drawn line, and arming clears the section mode and any
    /// section drag in flight, because one press cannot be two gestures.
    pub(crate) fn set_region_pick_armed(&mut self, armed: bool) {
        self.region_pick_armed = armed;
        if armed {
            self.section_edit_drag = None;
            self.section_draw_armed = false;
            self.section_anchor = None;
        } else {
            self.region_drag = None;
        }
    }

    /// The box being dragged on pane `pane_idx` right now — its centre and its
    /// half-width in kilometres — or `None`.
    ///
    /// Geographic, unlike [`Self::section_rubber_band`]'s pixels, and the
    /// difference is real rather than an inconsistency. A rubber band is a
    /// preview of a *gesture* and should track the finger exactly through a
    /// mid-drag wheel zoom. A region box is a preview of *ground*: the drag's
    /// centre was fixed on the press frame and its half-width is measured in
    /// kilometres, so drawing it through the projector is what makes it stay
    /// over the same ground when the map moves under it — which is what the
    /// committed box will do.
    pub(crate) fn region_preview(&self, pane_idx: PaneId) -> Option<(crate::pane::GeoPoint, f64)> {
        self.region_drag
            .filter(|drag| drag.pane_idx() == pane_idx)
            .map(|drag| (drag.centre(), drag.half_width_km()))
    }

    /// The rubber band to draw on pane `pane_idx`, in screen points, or `None`.
    ///
    /// Both endpoints are pixels rather than ground, deliberately: this is a
    /// preview of a gesture in progress, and it should track the finger exactly
    /// even on the frame a wheel-zoom has moved the map under it. The *stored*
    /// anchor is geographic — see [`SectionAnchor`] — and it is that one the
    /// committed line is built from.
    pub(crate) fn section_rubber_band(&self, pane_idx: PaneId) -> Option<(egui::Pos2, egui::Pos2)> {
        let anchor = self.section_anchor.as_ref()?;
        (anchor.pane_idx == pane_idx).then_some((anchor.screen, anchor.current))
    }

    /// The first 3D pane whose region was dragged on `source`.
    fn volume_pane_sourced_from(&self, source: PaneId) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| {
            self.panes[idx]
                .volume()
                .is_some_and(|v| v.source_pane == Some(source))
        })
    }

    /// The lowest-indexed pane currently drawing its volume, whatever aimed it.
    ///
    /// [`PaneState::volume`](crate::pane::PaneState::volume) rather than
    /// [`PaneState::map`](crate::pane::PaneState::map), so this answers only for
    /// a pane *actually in* the 3D render mode. A plan-view pane keeps a camera
    /// and a region across the switch, and treating one as a 3D pane here would
    /// silently flip a map the user is reading.
    fn lowest_volume_pane(&self) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| self.panes[idx].volume().is_some())
    }

    /// The first section pane whose line was drawn on `source`.
    fn section_pane_sourced_from(&self, source: PaneId) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| {
            self.panes[idx]
                .cross_section()
                .is_some_and(|s| s.source_pane == Some(source))
        })
    }

    /// A new pane at the end of the layout, or `None` if the layout is full.
    ///
    /// Shared by both target rules — a section line's and a 3D region's —
    /// because "is there room for one more, and if so which index is it" is a
    /// question about the layout and not about what will fill the pane.
    fn grown_pane(&mut self) -> Option<PaneId> {
        let wanted = self.pane_layout.pane_count + 1;
        if wanted > self.layout.width.max_panes() {
            return None;
        }
        self.set_pane_count(wanted).then(|| wanted - 1)
    }

    /// The lowest-indexed section pane, whatever it was aimed at.
    fn lowest_section_pane(&self) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| self.panes[idx].cross_section().is_some())
    }

    /// The highest-indexed visible pane that is not `source`.
    fn highest_pane_other_than(&self, source: PaneId) -> Option<PaneId> {
        (0..self.visible_pane_count())
            .rev()
            .find(|&idx| idx != source)
    }

    /// The view change this frame recorded and has not applied yet.
    ///
    /// Read by `ui_menu`'s dispatcher fingerprint, which has to be able to see
    /// that the toggle's arm did something: recording the request *is* what that
    /// arm does, and applying it is a separate step with its own test. Nothing in
    /// production reads it — the applier takes the field directly.
    #[cfg(test)]
    pub(crate) fn pending_pane_view_for_test(
        &self,
    ) -> Option<(PaneId, rustdar_radar::types::RenderView)> {
        self.pending_pane_view
    }

    /// What the 3D arm decided for each volume pane on the last frame.
    #[cfg(test)]
    pub(crate) fn volume_arms_for_test(&self) -> &[VolumeArmProbe] {
        &self.probes.last_volume_arms
    }

    /// The pane borders the last frame painted: pane index, the stroke's
    /// painted bounds, and whether it was the active highlight.
    #[cfg(test)]
    pub(crate) fn pane_borders_for_test(&self) -> &[(usize, egui::Rect, bool)] {
        &self.probes.last_pane_borders
    }

    /// The section tracks the last frame painted: map pane, section pane,
    /// and the painted A and B endpoints.
    #[cfg(test)]
    pub(crate) fn section_tracks_for_test(&self) -> &[(usize, usize, egui::Pos2, egui::Pos2)] {
        &self.probes.last_section_tracks
    }

    /// The committed region boxes the last frame painted: map pane, 3D pane,
    /// and the painted rect.
    #[cfg(test)]
    pub(crate) fn region_boxes_for_test(&self) -> &[(usize, usize, egui::Rect)] {
        &self.probes.last_region_boxes
    }

    /// Pane `idx`'s dispatched kinds in paint order, with the layer each
    /// painted into — the draw-order pin's read side.
    #[cfg(test)]
    pub(crate) fn paint_order_for_test(&self, idx: usize) -> Vec<(OverlayKind, egui::LayerId)> {
        self.probes
            .last_paint_order
            .iter()
            .find(|(pane, _)| *pane == idx)
            .map(|(_, order)| order.clone())
            .unwrap_or_default()
    }

    /// The Volume Alpha corner buttons the last frame drew, per pane.
    #[cfg(test)]
    pub(crate) fn alpha_buttons_for_test(&self) -> &[(usize, egui::Rect)] {
        &self.probes.last_alpha_buttons
    }

    /// Whether pane `idx` is a pane the **plan-view** pipeline must skip: it
    /// exists, and it is not a map.
    ///
    /// One predicate for the seven frontend loops that dispatch, cache, broadcast
    /// or gate on a plan-view raster: `dispatch_pane_renders`, the sibling
    /// broadcast in `poll_render_results`, both halves of `dispatch_loop_renders`,
    /// the loop-frame broadcast in `poll_loop_render_results`,
    /// `restore_cached_render`, and `sync_loop_playback_start`. Named once because
    /// they have to agree: a pane that is dispatched to but not broadcast to, or
    /// broadcast to but never dispatched, is a pane wedged with
    /// `render_in_flight` set forever — and one counted as a loop participant
    /// while nothing renders its frames holds every *other* pane's loop back.
    ///
    /// Written in the negative on purpose. An index past the end answers
    /// `false` — "not a pane to skip" — which leaves out-of-range handling
    /// exactly where each caller already had it, rather than folding a second,
    /// different question into this one. `dispatch_pane_renders` in particular
    /// iterates the layout's raw `pane_count`, which can outrun the vector, and
    /// its own `else` branch is what deals with that.
    ///
    /// The `mem::take` caveat on [`Self::pane`] applies in full: during the UI
    /// pass a taken pane reads as a map. Every caller of this runs from the
    /// frontend's frame loop, outside the egui pass, which is what makes it
    /// safe — see [`PaneContent`](crate::pane::PaneContent)'s module docs.
    pub fn pane_has_no_plan_view(&self, idx: PaneId) -> bool {
        self.pane(idx).is_some_and(|pane| !pane.is_map())
    }

    /// Whether pane `idx` is a pane the **loop** machinery must skip: it exists,
    /// and its kind has no picture a loop can hold ([`PaneKind::can_loop`]).
    ///
    /// The sibling of [`Self::pane_has_no_plan_view`], and the distinction
    /// between them is the whole reason both exist. That one asks "does this
    /// pane draw a square raster of one tilt?" and gates the
    /// plan-view dispatch, the static sibling broadcast and the suspend/resume
    /// restore. This one asks "can a sequence of this pane's pictures be
    /// animated?" and gates the loop dispatch, the loop-frame broadcast, the
    /// readiness settle and the playback start. A cross-section pane answers
    /// *yes* to the first question's negation and *no* to this one's: it has no
    /// plan view and it can loop, and collapsing the two would either stop it
    /// looping or hand it a plan-view raster.
    ///
    /// Written in the negative, and an index past the end answers `false`, for
    /// exactly the reasons the sibling gives — each caller keeps its own
    /// out-of-range handling rather than having a second question folded in.
    ///
    /// The `mem::take` caveat on [`Self::pane`] applies in full.
    pub fn pane_cannot_loop(&self, idx: PaneId) -> bool {
        self.pane(idx).is_some_and(|pane| !pane.can_loop())
    }

    /// Whether the storm motion vector is being edited *right now*, so that a
    /// consumer which spends real work on a change can wait for the release.
    ///
    /// # Commit on release, and why this control needs it when the others do
    /// not
    ///
    /// Every other setting that invalidates a render is a click: a product, a
    /// tilt, a checkbox. This one is a `DragValue`, and a drag produces a new
    /// value *every frame*. `App::apply_storm_motion_override` answers a change
    /// by evicting every storm-relative grid and section, so a two-second drag
    /// used to evict and rebuild them sixty times over — 210 ms of re-cut per
    /// drag frame for a cross-section, and for a 3D loop the whole resident
    /// set: fourteen grids, ~2 s of resample, discarded and restarted on the
    /// next frame, for ever, so the loop would never finish building while a
    /// finger was on the widget.
    ///
    /// Holding the commit until the drag ends makes the cost proportional to
    /// the *edit* rather than to how long it took: one eviction and one
    /// rebuild, whatever route the number took to get there. The picture on
    /// screen goes on showing the previous vector until then, which is the
    /// honest state — it is what the data was derived with — and the widget
    /// shows the new number, so nothing claims otherwise.
    ///
    /// Deliberately not "the value has stopped changing for N frames": a
    /// timeout would fire mid-drag on a slow frame and would make the commit a
    /// function of frame rate.
    pub fn storm_motion_mid_edit(&self) -> bool {
        self.storm_motion_editing
    }

    /// Whether pane `idx` needs every cut of its site's volume rather than the
    /// one tilt it has selected, because of *what kind of pane it is*.
    ///
    /// The view-side half of the whole-volume safety property;
    /// [`RadarProduct::reads_whole_volume`] is the data-side half, and
    /// `App::cut_selection_for` has to honour both. An index past the end needs
    /// nothing.
    pub fn pane_consumes_whole_volume(&self, idx: PaneId) -> bool {
        self.pane(idx)
            .is_some_and(|pane| pane.render_view().reads_whole_volume())
    }

    /// Get the rendering params for a specific pane.
    pub fn get_rendering_params_for_pane(&self, pane_idx: PaneId) -> Option<(RadarProduct, f32)> {
        self.panes
            .get(pane_idx)
            .and_then(|p| p.get_rendering_params())
    }

    /// Number of active panes.
    pub fn pane_count(&self) -> usize {
        self.pane_layout.pane_count
    }

    /// Split the map into `count` panes, as the settings UI's pane picker does.
    #[cfg(test)]
    pub(crate) fn set_pane_count_for_test(&mut self, count: usize) {
        while self.panes.len() < count {
            self.panes.push(PaneState::new());
        }
        self.pane_layout = PaneLayout::for_count(count);
        if self.active_pane >= count {
            self.active_pane = 0;
        }
    }

    /// The rect the pane grid was laid out in on the last frame.
    #[cfg(test)]
    pub(crate) fn map_panel_rect_for_test(&self) -> egui::Rect {
        self.probes.last_map_panel_rect
    }

    /// The egui `Id`s the last frame's layers panel resolved.
    #[cfg(test)]
    pub(crate) fn widget_id_probes(&self) -> &[(&'static str, egui::Id)] {
        &self.probes.widget_id_probes
    }

    /// Every menu leaf the last frame actually drew, as the renderer reported
    /// it — see [`ui_menu::DrawnMenuLeaf`].
    #[cfg(test)]
    pub(crate) fn menu_leaves_for_test(&self) -> &[ui_menu::DrawnMenuLeaf] {
        &self.probes.last_menu_leaves
    }

    /// The pointer state `render_panes` resolved for each pane last frame.
    #[cfg(test)]
    pub(crate) fn pane_pointers_for_test(&self) -> &[crate::ui_input::PanePointerProbe] {
        &self.probes.last_pane_pointers
    }

    /// Which render arm ran for each pane last frame. See [`PaneContentProbe`].
    #[cfg(test)]
    pub(crate) fn pane_content_for_test(&self) -> &[PaneContentProbe] {
        &self.probes.last_pane_content
    }

    /// Whether a label-tile source has been created, which is the observable half
    /// of "is this app fetching the city-label tile pyramid?".
    ///
    /// `MapTileState::ensure_label_tiles` only ever *creates* the source, so this
    /// answering `false` after a frame means no fetch was ever started.
    #[cfg(test)]
    pub(crate) fn label_tiles_made_for_test(&self) -> bool {
        self.map_tiles.label_tiles_light.is_some() || self.map_tiles.label_tiles_dark.is_some()
    }

    /// Record that the arm for `kind` drew pane `pane_idx` into `rect`.
    ///
    /// Called from inside each arm of `render_panes`' render branch, with the
    /// view written out as a literal there rather than passed down from the
    /// branch's subject — that literal is the whole reason the probe can catch a
    /// mis-wired arm. A no-op outside tests, like `ControlProbe::record_dropdown`.
    #[inline]
    pub(super) fn record_pane_content(
        &mut self,
        _pane_idx: usize,
        _view: rustdar_radar::types::RenderView,
        _rect: egui::Rect,
    ) {
        #[cfg(test)]
        self.probes.last_pane_content.push(PaneContentProbe {
            pane_idx: _pane_idx,
            view: _view,
            rect: _rect,
        });
    }

    /// The pane-count buttons the picker drew on the last frame.
    #[cfg(test)]
    pub(crate) fn pane_options_for_test(&self) -> &[PaneOptionProbe] {
        &self.probes.last_pane_options
    }

    /// The excluded rects `render_panes` was handed on the last frame.
    #[cfg(test)]
    pub(crate) fn map_excluded_rects_for_test(&self) -> &[egui::Rect] {
        &self.probes.last_map_excluded_rects
    }

    /// What the last frame's status bar drew.
    #[cfg(test)]
    pub(crate) fn status_bar_for_test(&self) -> &StatusBarProbe {
        &self.probes.last_status_bar
    }

    /// What the last frame's timeline transport drew.
    #[cfg(test)]
    pub(crate) fn timeline_for_test(&self) -> &TimelineProbe {
        &self.probes.last_timeline
    }

    /// What the last frame's top bar drew.
    #[cfg(test)]
    pub(crate) fn top_bar_for_test(&self) -> &TopBarProbe {
        &self.probes.last_top_bar
    }

    /// What the last frame's bottom bar drew.
    #[cfg(test)]
    pub(crate) fn bottom_bar_for_test(&self) -> &BottomBarProbe {
        &self.probes.last_bottom_bar
    }

    /// What the last frame's phone sheet drew.
    #[cfg(test)]
    pub(crate) fn sheet_for_test(&self) -> &SheetProbe {
        &self.probes.last_sheet
    }

    /// What the last frame's phone error toast drew, if it drew.
    #[cfg(test)]
    pub(crate) fn error_toast_for_test(&self) -> Option<ErrorToastProbe> {
        self.probes.last_error_toast
    }

    /// Open or close the sheet's Menu page directly, for the chain tests
    /// that build the full page stack without walking the bottom bar.
    #[cfg(test)]
    pub(crate) fn set_sheet_menu_open_for_test(&mut self, open: bool) {
        self.menu_open = open;
    }

    /// What the last frame's layer stack drew.
    #[cfg(test)]
    pub(crate) fn stack_for_test(&self) -> &StackProbe {
        &self.probes.last_stack
    }

    /// What the last frame's inspector drew.
    #[cfg(test)]
    pub(crate) fn inspector_for_test(&self) -> &InspectorProbe {
        &self.probes.last_inspector
    }

    /// What the last frame's Add-layer catalog drew.
    #[cfg(test)]
    pub(crate) fn catalog_for_test(&self) -> &CatalogProbe {
        &self.probes.last_catalog
    }

    /// What the last frame's pill rows drew, in pane order.
    #[cfg(test)]
    pub(crate) fn pill_rows_for_test(&self) -> &[pills::PillRowProbe] {
        &self.probes.last_pills
    }

    /// The pill popover the last frame drew, if one was open.
    #[cfg(test)]
    pub(crate) fn pill_popover_for_test(&self) -> Option<&pills::PillPopoverProbe> {
        self.probes.last_pill_popover.as_ref()
    }

    /// Whether some feature consumed the last frame's map click — see the
    /// `click_consumed_frame` field.
    #[cfg(test)]
    pub(crate) fn click_consumed_for_test(&self) -> bool {
        self.click_consumed_frame
    }

    /// The user's saved presets, as the catalog holds them.
    #[cfg(test)]
    pub(crate) fn presets_for_test(&self) -> &[PresetConfig] {
        &self.presets
    }

    /// How many handler-control passes the last frame ran. The harness holds
    /// this to at most one after every frame — see the field.
    #[cfg(test)]
    pub(crate) fn control_render_passes_for_test(&self) -> u32 {
        self.probes.control_render_passes
    }

    /// Open or close the Set Time dialog directly, for fixtures that need a
    /// centred floating dialog over the map — the settings window used to be
    /// the convenient one, and it is a side panel now.
    #[cfg(test)]
    pub(crate) fn set_time_dialog_open_for_test(&mut self, open: bool) {
        self.time_dialog.show = open;
    }

    /// Open or close the Add-layer catalog directly, for fixtures stacking
    /// layers the UI routes cannot stack — the Esc-chain walk opens it under
    /// a feature popup and a time dialog, whose windows would swallow the
    /// clicks the UI route needs.
    #[cfg(test)]
    pub(crate) fn set_catalog_open_for_test(&mut self, open: bool) {
        self.catalog_open = open;
    }

    /// Which pane is currently active.
    #[cfg(test)]
    pub(crate) fn active_pane_index_for_test(&self) -> PaneId {
        self.active_pane
    }

    /// Set every pane's layer link at once — the harness's one-call stand-in
    /// for the retired `sync_layers` global, for tests that need panes able
    /// to disagree (off) or the default convergence (on).
    #[cfg(test)]
    pub(crate) fn set_layer_links_for_test(&mut self, on: bool) {
        for pane in &mut self.panes {
            pane.layer_link = on;
        }
    }

    /// Whether every pane's layer link is on — the default-state precondition
    /// the sync contracts assert before driving the fan-out.
    #[cfg(test)]
    pub(crate) fn all_layer_linked_for_test(&self) -> bool {
        self.panes.iter().all(|pane| pane.layer_link)
    }

    /// Open or close the layers drawer, as the top bar's Layers toggle does
    /// below the sidebar breakpoint.
    #[cfg(test)]
    pub(crate) fn set_drawer_open(&mut self, open: bool) {
        self.drawer_open = open;
    }

    /// Every handler dropdown the last frame drew. See [`DrawnDropdown`].
    #[cfg(test)]
    pub(crate) fn dropdowns_for_test(&self) -> &[DrawnDropdown] {
        &self.probes.last_dropdowns
    }

    /// Every control item the last frame drew, whatever its shape. See
    /// [`DrawnControlItem`].
    #[cfg(test)]
    pub(crate) fn control_items_for_test(&self) -> &[DrawnControlItem] {
        &self.probes.last_control_items
    }

    /// Every settings row the last frame drew. See
    /// [`settings::DrawnSettingsRow`].
    #[cfg(test)]
    pub(crate) fn settings_rows_for_test(&self) -> &[settings::DrawnSettingsRow] {
        &self.probes.last_settings_rows
    }

    /// What the last frame's detail popup did with its action buttons:
    /// `(triggered, handled)` indices. See the note on the handling in
    /// `ui_popups.rs` for why the second must hold at most one entry.
    #[cfg(test)]
    pub(crate) fn popup_actions_for_test(&self) -> (Vec<usize>, Vec<usize>) {
        (
            self.probes.last_popup_triggered.clone(),
            self.probes.last_popup_handled.clone(),
        )
    }

    /// This frame's resolved layout, for tests asserting on the breakpoint.
    #[cfg(test)]
    pub(crate) fn layout_for_test(&self) -> LayoutCtx {
        self.layout
    }

    /// The pane rects the layout produces inside the map panel, as
    /// `render_panes` computes them.
    ///
    /// "As `render_panes` computes them" is the whole contract, so the bound is
    /// [`Self::visible_pane_count`] like the real loop's: with the raw count a
    /// test would be handed rects for panes no frame ever drew, and any test that
    /// clicked one would be asserting about a pane the app does not have.
    #[cfg(test)]
    pub(crate) fn pane_rects_for_test(&self) -> Vec<egui::Rect> {
        let panel = self.probes.last_map_panel_rect;
        (0..self.visible_pane_count())
            .map(|idx| self.pane_layout.pane_rect(idx, panel))
            .collect()
    }

    /// Claim `count` panes in the layout **without** growing the pane vector.
    ///
    /// The skew `visible_pane_count` exists for, built on purpose. No production
    /// writer can reach it — see `detect_active_pane_click` — so a test that wants
    /// it has to say so, which is also what keeps the difference between "clamped
    /// by a caller" and "clamped by the type" visible.
    #[cfg(test)]
    pub(crate) fn claim_pane_count_for_test(&mut self, count: usize) {
        self.pane_layout = PaneLayout::for_count(count);
    }

    /// Whether pane `idx`'s layer state belongs to the linked group — the
    /// per-pane successor to the retired `is_sync_layers` global, read by the
    /// frontend's loop texture sharing (broadcast and donor clones happen
    /// inside the linked group, never across an unlinked pane's boundary).
    /// Out of bounds answers linked: the default every real pane starts with.
    pub fn pane_layer_linked(&self, idx: usize) -> bool {
        self.panes.get(idx).is_none_or(|pane| pane.layer_link)
    }

    /// Whether pane `idx` follows shared time — the loop playback
    /// synchroniser's per-pane gate (`sync_loop_playback_start` holds the
    /// time-linked loops together and lets an unlinked loop start alone).
    pub fn pane_time_linked(&self, idx: usize) -> bool {
        self.panes.get(idx).is_none_or(|pane| pane.time_link)
    }

    /// The panes a layer-wide change on pane `src` reaches: the visible
    /// layer-linked panes when `src` is itself linked, or `src` alone when it
    /// is not — `propagate_layer_sync`'s two-ended gate, exported for the
    /// frontend's site switch, which writes the move itself.
    pub fn layer_sync_targets(&self, src: usize) -> Vec<usize> {
        let count = self.visible_pane_count();
        if count > 1 && self.pane_layer_linked(src) {
            (0..count)
                .filter(|&idx| idx == src || self.pane_layer_linked(idx))
                .collect()
        } else {
            vec![src]
        }
    }

    /// Whether one overlay render may serve several panes: every visible pane
    /// that draws ground is viewport-linked *and* layer-linked, so their
    /// viewports and layer stacks are one by construction. The per-pane
    /// successor to the old "viewport sync and layer sync both on" grouping
    /// gate — one pane out of either group and nothing is grouped, because the
    /// dedup key carries no geo bounds and a shared texture would land on a
    /// pane whose map is somewhere else.
    ///
    /// The exemption is [`PaneState::draws_ground`] rather than `is_map`
    /// because the panes being excused have to be the ones that never receive
    /// one of these textures. A 3D pane does receive them — its floor asks for
    /// them, at its own viewport's bounds — so an unlinked 3D pane excused here
    /// is exactly the pane a shared texture would land on wrongly.
    pub fn overlay_renders_groupable(&self) -> bool {
        (0..self.visible_pane_count()).all(|idx| {
            let pane = &self.panes[idx];
            !pane.draws_ground() || (pane.viewport_link && pane.layer_link)
        })
    }

    /// Get the current radar config
    pub fn get_radar_config(&self) -> &RadarConfig {
        &self.radar.config
    }

    /// Clear loading_site on all panes viewing the given site.
    pub fn clear_loading_site_for_site(&mut self, site: &str) {
        for pane in &mut self.panes {
            if pane.site == site {
                pane.loading_site = None;
                pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            }
        }
    }

    /// Bump the RadarSites texture generation on all panes (e.g. on theme change).
    pub fn bump_all_radar_sites_gen(&mut self) {
        for pane in &mut self.panes {
            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
        }
    }

    /// The insets currently in force, in the same order the frame composes
    /// them in.
    ///
    /// This and the getters below it are the read half of the frame-input
    /// facts ([`FrameInputs`](crate::shell_api::FrameInputs)), and they exist
    /// for one reason: every one of these values is pushed in from the host
    /// through a platform bridge this crate cannot see, and the frontend's
    /// tests need somewhere to observe that the hand-off happened at all.
    /// What the UI then *does* with them is covered here, against the drawn
    /// chrome (see `input_harness`), never against these.
    pub fn safe_area_insets(&self) -> (f32, f32, f32, f32) {
        self.safe_area_insets
    }

    /// See [`FrameInputs::supports_exit`](crate::shell_api::FrameInputs::supports_exit).
    pub fn supports_exit(&self) -> bool {
        self.supports_exit
    }

    /// See [`FrameInputs::gps`](crate::shell_api::FrameInputs::gps).
    pub fn gps_fix(&self) -> Option<&rustdar_gps::GpsFix> {
        self.user_fix.as_ref()
    }

    /// See [`FrameInputs::location`](crate::shell_api::FrameInputs::location).
    pub fn location_permission(&self) -> rustdar_gps::LocationPermission {
        self.location_permission
    }

    /// See [`FrameInputs::location`](crate::shell_api::FrameInputs::location).
    pub fn location_active(&self) -> bool {
        self.location_active
    }

    /// See [`FrameInputs::location_settings_available`](crate::shell_api::FrameInputs::location_settings_available).
    pub fn location_settings_available(&self) -> bool {
        self.location_settings_available
    }

    /// See [`FrameInputs::catalogue_pending`](crate::shell_api::FrameInputs::catalogue_pending).
    pub fn catalogue_pending(&self) -> bool {
        self.catalogue_pending
    }

    /// See [`FrameInputs::user_heading`](crate::shell_api::FrameInputs::user_heading).
    pub fn user_heading(&self) -> Option<f32> {
        self.user_heading
    }

    /// Whether the active pane is showing the most recent (live) scan.
    pub fn is_viewing_live(&self) -> bool {
        self.panes
            .get(self.active_pane)
            .is_some_and(|p| p.viewing_live)
    }

    /// Whether any pane is viewing live (for auto-poll gating).
    pub fn is_any_pane_live(&self) -> bool {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .any(|p| p.viewing_live)
    }

    /// Get the scan info for the active pane.
    pub fn get_scan_info(&self) -> Option<&ScanInfo> {
        self.panes
            .get(self.active_pane)
            .and_then(|p| p.scan_info.as_ref())
    }

    /// Get the scan info for a specific pane.
    pub fn get_scan_info_for_pane(&self, pane_idx: usize) -> Option<&ScanInfo> {
        self.panes.get(pane_idx).and_then(|p| p.scan_info.as_ref())
    }

    /// How long the event loop may sleep before some auto-poll timer next
    /// needs a **frame**, or `None` when nothing is polling and it may sleep
    /// until something happens.
    ///
    /// This replaced an `is_auto_poll_active` predicate, and the shape is the
    /// whole point. That predicate answered "is a timer running" — `enabled &&
    /// initial_fetch_done`, plus any enabled layer with an interval — and its
    /// one caller re-armed an unconditional redraw with it at the end of every
    /// frame. `initial_fetch_done` goes true on frame one and is never
    /// cleared, so from the first frame of the default configuration the
    /// answer was permanently yes: request redraw, draw, request redraw, at
    /// display refresh rate for the life of the process, to service a poll
    /// that fires once a minute.
    ///
    /// A duration instead, folded into the loop's `ControlFlow` — so the app
    /// sleeps until the poll is actually due. Every term is gated on the poll
    /// *firing*, not merely on a timer existing, because a wake granted for a
    /// frame that polls nothing is the busy loop back again:
    ///
    /// * the radar poll needs a live pane and no fetch in flight, which is
    ///   exactly what [`check_auto_polls`](Self::check_auto_polls) requires;
    /// * an overlay needs a pane that can draw it and no fetch in flight.
    ///
    /// Zero means "due now, and the next frame will take it". The frame that
    /// just ran normally consumes what it was due, so this reads zero only
    /// where something refused it — `create_fetch_tasks` declining to build a
    /// task leaves an overlay permanently due — and the caller is responsible
    /// for not turning that into a zero-length sleep re-armed every iteration.
    pub fn auto_poll_delay(&self) -> Option<std::time::Duration> {
        let radar = if self.is_any_pane_live() && !self.radar.fetching {
            self.auto_poll.poll_delay()
        } else {
            None
        };
        let overlays = OverlayKind::all()
            .iter()
            .filter_map(|&kind| self.overlay_poll_delay(kind))
            .min();
        [radar, overlays].into_iter().flatten().min()
    }

    /// How long until the status bar's own text would read differently, or
    /// `None` when nothing on screen is restating the clock.
    ///
    /// Kept apart from [`Self::auto_poll_delay`] because it is a *display*
    /// obligation rather than a poll: the chip counts down whether or not the
    /// poll it counts towards can fire, and it stops mattering the instant the
    /// bar is off screen. Both end up in the same wake, but conflating them
    /// would let the bar's presence decide when data is fetched.
    ///
    /// This is last frame's reading rather than a fresh derivation of it —
    /// see [`status_bar_tick`](Self::status_bar_tick). What the bar drew is a
    /// fact; re-deriving it would mean a second copy of the width class, the
    /// chrome fade, the collapse and the chip's own three-valued state, which
    /// is four places to fall out of step with the one that draws. Nothing
    /// changes any of them without an event, and an event produces a frame
    /// that writes it again.
    pub fn status_tick_delay(&self) -> Option<std::time::Duration> {
        self.status_bar_tick
    }

    /// Whether any pane **on screen** has a loop that is playing or has
    /// in-flight work.
    ///
    /// Bounded by the layout's count like its siblings
    /// [`is_any_pane_live`](Self::is_any_pane_live) and
    /// [`any_pane_has_overlay_enabled`](Self::any_pane_has_overlay_enabled),
    /// and for a sharper reason than tidiness: splitting to fewer panes leaves
    /// the extra `PaneState`s in the vector, and `advance_loop_playback` walks
    /// `0..pane_count`. So a loop playing on a pane that is then hidden is one
    /// nothing advances and nothing can stop — it answered yes here for the
    /// life of the process, holding the event loop at loop frame rate for an
    /// animation that never moves, with its frame textures beyond the reach of
    /// eviction.
    pub fn any_loop_active(&self) -> bool {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .any(|p| {
                let ls = &p.loop_state;
                ls.is_active()
                    && (ls.is_playing()
                        || ls.is_fetching()
                        || ls.frames.iter().any(|f| f.render_in_flight))
            })
    }

    /// Whether any pane is waiting on a raster's pixels to finish arriving.
    ///
    /// A term in `App::handle_redraw`'s re-arm, beside the renders and the loops
    /// and for the same reason: **it finishes**. A hold ends when its last band
    /// lands, when a newer render replaces it, when the pane's radar cache is
    /// cleared, or when a renderer rebuild releases it — and until then the loop
    /// owes a frame, because a hold with nothing waking the loop is a pane
    /// showing the previous sweep until some unrelated input happens by.
    ///
    /// Over **every** pane rather than the layout's count, unlike
    /// [`any_loop_active`](Self::any_loop_active) above. The two differ because
    /// the failures differ: a loop on a hidden pane is unstoppable work, where a
    /// hold on a hidden pane is a bounded upload that finishes and then stops
    /// answering yes. Counting it costs at most the frames that upload takes,
    /// and *not* counting it would leave a pane split back into view showing the
    /// sweep before last.
    pub fn any_raster_held(&self) -> bool {
        self.panes.iter().any(PaneState::is_holding_raster)
    }

    /// Show every held raster whose pixels have all landed — the radar raster
    /// and every layer texture alike.
    ///
    /// One pass, over every pane for the reason above. `delivered` is
    /// `EguiRenderer::is_delivered`, asked once per held id — panes served from
    /// one raster hold clones of one handle (`PlanViewUploads`, and the overlay
    /// poller clones one handle across the panes a result names), so a split
    /// of four on one site swaps on one answer.
    pub fn promote_held_rasters(&mut self, delivered: impl Fn(egui::TextureId) -> bool) {
        for pane in &mut self.panes {
            pane.promote_held_raster(&delivered);
            pane.promote_held_overlays(&delivered);
        }
    }

    /// Let go of every raster still arriving, without showing any of them.
    ///
    /// See [`PaneState::release_held_raster`]: the ids belong to a context that
    /// no longer exists, so nothing will ever say they arrived.
    pub fn release_held_rasters(&mut self) {
        for pane in &mut self.panes {
            pane.release_held_raster();
        }
    }

    pub fn clear_graphics_state(&mut self) {
        for pane in &mut self.panes {
            pane.loading_site = None;
            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            // Clear loop frame textures so they get re-rendered on resume.
            // The frame list and scan cache survive, so dispatch_loop_renders()
            // will re-upload textures automatically.
            for frame in &mut pane.loop_state.frames {
                frame.image = None;
                frame.render_in_flight = false;
            }
            // Clear overlay texture caches — handles become invalid when the
            // egui context is destroyed. needs_rerender() will trigger fresh
            // background renders.
            for cache in pane.overlay_textures.values_mut() {
                cache.clear();
                cache.render_in_flight = false;
            }
            // And whatever the pane's *kind* holds — today, a section pane's
            // raster. This is the only place a pane-held handle is released when
            // the egui context dies. Note that every arm deliberately keeps
            // enough to put its picture *back*: the frontend's
            // `restore_section_textures` re-uploads a section from the
            // `CrossSection` this leaves behind, exactly as the loop above
            // relies on `dispatch_loop_renders` re-uploading a loop frame. See
            // `PaneContent::release_textures`.
            pane.content.release_textures();
        }
        self.map_tiles.clear();
        // The painter holds wgpu handles made by the device that is going away,
        // and every one of them — pipelines, the offscreen targets, the uploaded
        // grid — is invalid the moment it does. Dropping the whole painter is
        // the release: the frontend installs a fresh one when the renderer comes
        // back, and until then every 3D pane says so instead of drawing with a
        // dangling handle. This is the surface-loss and suspend/resume half of
        // `ReleaseVolume`.
        self.volume_painter = None;
    }

    /// Whatever can draw 3D panes this frame.
    pub(crate) fn volume_painter(
        &self,
    ) -> Option<&std::sync::Arc<dyn crate::volume_view::VolumePainter>> {
        self.volume_painter.as_ref()
    }
}

#[cfg(test)]
mod chunk_scan_info_tests;

#[cfg(test)]
mod pane_slice_tests;

#[cfg(test)]
mod storm_motion_override_tests;

#[cfg(test)]
mod wake_schedule_tests;

#[cfg(test)]
mod overlay_retry_tests;

#[cfg(test)]
mod overlay_texture_release_tests;
