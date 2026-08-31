use crate::actions::{GuiAction, RadarConfig};
use squallar_overlays::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, ControlValue,
};

const DEFAULT_INITIAL_ZOOM: f64 = 7.0;

use crate::pane::{ColorScaleOrientation, PaneId, PaneLayout, PaneState};
use crate::tiles::MapTileState;
use crate::ui_layout::{LayoutCtx, ModalityLatch};
use chrono::Timelike;
use egui::Context;
use squallar_overlays::fetch_policy::FetchHealth;
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_radar::types::ScanInfo;
use squallar_source::id::{LayerId, known};
use squallar_source::product::FieldId;
use squallar_units::UserPreferences;
use std::collections::HashMap;

#[path = "ui_shell.rs"]
mod shell;
#[path = "ui_stack.rs"]
mod ui_stack;
#[cfg(test)]
pub(crate) use ui_stack::{ADD_LAYER_LABEL, StackProbe, StackRowProbe};
#[path = "ui_inspector.rs"]
mod ui_inspector;
#[cfg(test)]
pub(crate) use ui_inspector::InspectorProbe;
#[path = "ui_config.rs"]
pub(crate) mod config;
#[path = "ui_map_overlays.rs"]
mod map_overlays;
#[path = "ui_popups.rs"]
mod popups;
#[path = "ui_menu.rs"]
mod ui_menu;
#[cfg(test)]
pub(crate) use ui_menu::DrawnMenuLeaf;
#[cfg(test)]
#[cfg(test)]
pub(crate) use ui_menu::VOLUME_PANE_LABEL;
#[cfg(test)]
pub(crate) use ui_menu::{DRAW_CROSS_SECTION_LABEL, PICK_REGION_LABEL};
#[path = "ui_map.rs"]
pub(crate) mod map;
#[cfg(test)]
pub(crate) use map::VOLUME_SIDEBAR_HEADER;
#[cfg(test)]
pub(crate) use map::VolumeArmProbe;
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
/// The archive rail's geometry, for the tests that drive it through the real
/// widget: where the travel begins and ends, and where `now` sits on it.
#[cfg(test)]
pub(crate) use timeline::{NOW_SPLIT, slider_end_inset, slider_travel_px};
#[path = "ui_topbar.rs"]
mod topbar;
/// The top bar's layout floor — margins plus one interact row.
#[cfg(test)]
pub(crate) use topbar::MIN_BAR_HEIGHT;
#[path = "ui_pills.rs"]
mod pills;
/// What a pane's own top-left content leaves clear for its pill row — read
/// by the section pane's layout and the 3D pane's caption.
#[cfg(test)]
pub(crate) use pills::PILL_ROW_CLEARANCE;
/// The sync section's row labels, for the parity walk.
#[cfg(test)]
pub(crate) use pills::SYNC_SECTION_LABELS;
#[cfg(test)]
pub(crate) use pills::{PillKind, PillPopoverProbe, PillRowProbe};
#[path = "ui_fade.rs"]
mod fade;
#[path = "ui_sheet.rs"]
mod sheet;
pub(crate) use sheet::SheetExtent;
#[cfg(test)]
pub(crate) use sheet::SheetPage;
#[cfg(test)]
pub(crate) use sheet::{BottomBarProbe, ErrorToastProbe, SheetProbe};
#[path = "ui_catalog.rs"]
mod catalog;
/// The preset shape, re-used by the config writer.
pub(crate) use catalog::PresetConfig;
#[cfg(test)]
pub(crate) use catalog::PresetPane;
/// The compiled-in presets, for the parity walk.
#[cfg(test)]
pub(crate) use catalog::builtin_presets;
#[cfg(test)]
pub(crate) use catalog::{CatalogGroup, CatalogProbe, CatalogTileProbe};

/// The sentence the settings pane puts under a refusal.
#[cfg(test)]
pub(crate) use settings::LOCATION_DENIED_NOTE;
/// The settings window's row table and its drawn-row probe, for the parity walk.
#[cfg(test)]
pub(crate) use settings::{DrawnSettingsRow, SETTINGS_ROWS};

#[path = "ui_diagnostics.rs"]
mod diagnostics;
#[path = "ui_offline_areas.rs"]
mod offline_areas;
#[path = "gui/probes.rs"]
mod probes;
pub(crate) use probes::ControlProbe;
#[path = "gui/state.rs"]
mod state;
pub use state::{Gui, StormMotionOverride};
#[path = "gui/frame.rs"]
mod frame;
#[path = "gui/layer_glue.rs"]
mod layer_glue;
pub(crate) use layer_glue::RoundOutcome;
#[path = "gui/sync.rs"]
mod sync;

use crate::ui_input::InteractionState;

/// What [`Gui::mirror_source_rects`] answers: the strips the mirror pass
/// would copy, and whether this pass's primitives actually carry them.
///
/// Both fields are private on purpose — the shell reads, and can neither
/// invent a strip nor overrule the strip cache's repainted/held verdict.
/// A forged `repainted: true` over a skipped pass renders the mirror from
/// primitives with no strips in them and blanks every 3D floor; a forged
/// `false` over a repainted pass freezes the floors stale.
pub struct MirrorSources {
    rects: Vec<egui::Rect>,
    repainted: bool,
}

impl MirrorSources {
    /// The strips, in points, all below the frame's bottom edge.
    pub fn rects(&self) -> &[egui::Rect] {
        &self.rects
    }

    /// Whether the pass that just ended painted the strips — the mirror must
    /// be re-rendered exactly when this is true (and the rects are
    /// non-empty).
    pub fn repainted(&self) -> bool {
        self.repainted
    }
}

/// One pane-count button the picker drew, as it was drawn.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaneOptionProbe {
    pub count: usize,
    pub selected: bool,
    /// Whether the button could be clicked. The top bar draws every count up
    /// to the absolute maximum and disables the ones past this width's offer.
    pub enabled: bool,
    pub rect: egui::Rect,
}

/// One split-orientation button the picker drew, as it was drawn. Empty
/// whenever the picker drew none — one pane has no split to orient.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SplitOptionProbe {
    pub orientation: crate::pane::SplitOrientation,
    pub selected: bool,
    pub rect: egui::Rect,
}

/// What the top bar drew: the rects a test drives it by, and the state each
/// toggle was showing. Reported by the renderer, never rebuilt by a test.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TopBarProbe {
    pub rect: egui::Rect,
    pub menu_button: egui::Rect,
    pub layers_toggle: (egui::Rect, bool),
    pub pane_count_max: usize,
    pub section_arm: (egui::Rect, bool),
    pub region_arm: (egui::Rect, bool),
    /// The offline-download arm toggle: where it drew, and whether it is lit.
    pub offline_arm: (egui::Rect, bool),
    pub inspector_toggle: (egui::Rect, bool),
    /// The phone bar's scan summary chip text, verbatim.
    pub scan_text: String,
    /// The phone bar's ◧ collapse/restore button.
    pub collapse: egui::Rect,
    /// Whether the phone bar hosted the hover readout this frame.
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
            offline_arm: (egui::Rect::NOTHING, false),
            inspector_toggle: (egui::Rect::NOTHING, false),
            scan_text: String::new(),
            collapse: egui::Rect::NOTHING,
            hover: false,
        }
    }
}

/// Which render arm ran for one pane, recorded **inside the arm itself**.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaneContentProbe {
    pub pane_idx: usize,
    pub view: squallar_radar::types::RenderView,
    pub rect: egui::Rect,
}

/// What the status bar drew, rather than the flags that decided it.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StatusBarProbe {
    /// The scan summary text, verbatim — long or short form.
    pub scan_text: String,
    pub product_age_text: Option<String>,
    /// The auto-poll chip's rect and text, when one was drawn.
    pub poll_chip: Option<(egui::Rect, String)>,
    /// The refresh button's rect — always drawn.
    pub refresh: egui::Rect,
    /// The ◧ collapse button's rect — the restore button while collapsed.
    pub collapse: egui::Rect,
    pub collapsed: bool,
    pub hover: bool,
    /// The rect the floating bar actually claimed, straight off its own response.
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InspectorSelection {
    AppSettings,
    PaneProps,
    Layer(LayerId),
}

/// Time editing dialog state.
pub(super) struct TimeDialogState {
    /// **The time on display**, and the value the two strings below are a
    /// view of. It moved here from the radar config at WO-E8d because this is
    /// what edits it: the dialog's OK parses the strings into it, Cancel and
    /// "Use Current Time" reformat the strings from it, and the shell pushes
    /// it in whenever a navigation lands on a different scan.
    pub timestamp: chrono::NaiveDateTime,
    pub date_string: String,
    pub time_string: String,
    pub show: bool,
}

impl TimeDialogState {
    /// Take a new selected time **and re-render both strings from it**, which
    /// is the one place that pairing happens — a timestamp written without
    /// them leaves the dialog showing the previous time.
    pub fn select(&mut self, timestamp: chrono::NaiveDateTime) {
        self.timestamp = timestamp;
        self.date_string = timestamp.format("%Y-%m-%d").to_string();
        self.time_string = timestamp.format("%H:%M:%S").to_string();
    }
}

/// Where an in-flight cross-section draw started.
struct SectionAnchor {
    pane_idx: PaneId,
    ground: squallar_geo::GeoPoint,
    screen: egui::Pos2,
    current: egui::Pos2,
}

/// Queue an overlay fetch the **user** asked for, and clear the layer's retry
/// ladder on the way past.
pub(crate) fn push_user_overlay_fetch(
    overlays: &mut OverlayRegistry,
    actions: &mut Vec<GuiAction>,
    kind: LayerId,
    pane_idx: usize,
) {
    overlays.clear_retry(&kind);
    if !actions
        .iter()
        .any(|a| matches!(a, GuiAction::FetchOverlay { kind: k, .. } if *k == kind))
    {
        actions.push(GuiAction::FetchOverlay { kind, pane_idx });
    }
}

/// The label the open list puts against `value`, or the raw value for one the
/// handler did not offer.
fn dropdown_option_label<'a>(options: &'a [(String, String)], value: &'a str) -> &'a str {
    options
        .iter()
        .find(|(v, _)| v == value)
        .map_or(value, |(_, display)| display.as_str())
}

/// One dropdown a control tree actually drew: the collapsed box's text and rect.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnDropdown {
    pub id: &'static str,
    pub label: String,
    pub selected_text: String,
    pub rect: egui::Rect,
}

/// The widget shape a [`ControlItem`] rendered as.
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
    TextField,
}

/// One control a handler's tree actually drew — the generalisation of
/// [`DrawnDropdown`] to every [`ControlItem`] shape.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnControlItem {
    /// The handler whose tree this item came from; `None` for a control drawn
    /// outside any handler's tree.
    pub handler: Option<LayerId>,
    pub label: String,
    pub kind: DrawnControlKind,
    pub rect: egui::Rect,
}

/// The line a **cross-section** pane's sidebar shows where a map pane's layer
/// list would be.
pub(crate) const NON_MAP_LAYERS_NOTE: &str = "A cross-section has no map to draw layers on.";

/// The header over the section pane's sidebar block. The icon is the top bar's
/// own X-sec diagonal (`∕`): the demo's `✂` has no glyph in egui's bundled fonts.
pub(crate) const SECTION_SIDEBAR_HEADER: &str = "\u{2215}  Cross-section";

/// The identity line every pane kind's sidebar opens with: whose data this
/// pane shows and what the pane is, e.g. `KTLX · 3D volume`.
fn render_pane_identity(ui: &mut egui::Ui, pane: &PaneState) {
    let kind = match pane.render_view() {
        squallar_radar::types::RenderView::PlanView => "Map",
        squallar_radar::types::RenderView::CrossSection => "Cross-section",
        squallar_radar::types::RenderView::Volume => "3D volume",
    };
    ui.label(egui::RichText::new(format!("{} - {}", pane.site(), kind)).strong());
}

/// Render a single declarative [`ControlItem`] into the UI, collecting any
/// resulting [`ControlUpdate`]s into `updates`.
fn render_control_item(
    ui: &mut egui::Ui,
    kind: &LayerId,
    item: &ControlItem,
    updates: &mut Vec<(LayerId, ControlUpdate)>,
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
                    kind.clone(),
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
                            kind.clone(),
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
        ControlItem::TextField {
            id,
            label,
            value,
            hint,
        } => {
            let mut text = value.clone();
            ui.label(label.as_str());
            let response = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .id_salt(format!("{kind:?}_{id}"))
                    .font(egui::TextStyle::Monospace)
                    .hint_text(hint.as_str()),
            );
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::TextField, label, response.rect);
            // One update per edit, exactly as a dropdown reports a new
            // selection: the handler holds the text, and the box is redrawn
            // from it next frame.
            if response.changed() {
                updates.push((
                    kind.clone(),
                    ControlUpdate {
                        id,
                        value: ControlValue::String(text),
                    },
                ));
            }
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
                    kind.clone(),
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
                ui.add(slider)
            });
            // A UiSweep target: the sweep drags the first registered slider
            // out and back. The inner response is the rail itself, so the
            // scripted press lands on the slider and not the label.
            if crate::gesture_player::click_registry::collecting() {
                crate::gesture_player::click_registry::register(
                    &format!(
                        "{}{kind:?}_{id}",
                        crate::gesture_player::ui_sweep::SLIDER_PREFIX
                    ),
                    row.inner.rect,
                );
            }
            #[cfg(test)]
            probe.record_item(kind, DrawnControlKind::Slider, label, row.response.rect);
            if (val - original).abs() > f64::EPSILON {
                updates.push((
                    kind.clone(),
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
/// crumb and the "Show <layer>" toggle instead of the handler's copies.
pub(crate) fn is_master_control(item: &ControlItem) -> bool {
    matches!(
        item,
        ControlItem::Heading { .. } | ControlItem::Toggle { id: "enabled", .. }
    )
}

impl Gui {
    /// The config a radar fetch on the active pane's behalf must use: the
    /// time on display, against the **active pane's own site**. There is no
    /// other site it could use: a pane owns its site, and no app-wide one
    /// exists to fall back to.
    pub(super) fn active_pane_fetch_config(&self) -> RadarConfig {
        RadarConfig {
            site: self.active_pane().site().to_string(),
            timestamp: self.time_dialog.timestamp,
        }
    }

    /// Apply one event-shaped push from the App.
    pub fn apply(&mut self, event: crate::shell_api::GuiEvent) {
        use crate::shell_api::GuiEvent;
        match event {
            GuiEvent::ScanInfoForSite { site, info } => {
                let mut any_pane_took_it = false;
                for pane in &mut self.panes {
                    if pane.site() == site {
                        pane.scan_info = Some(info.clone());
                        any_pane_took_it = true;
                    }
                }
                self.end_radar_round(RoundOutcome::Delivered);
                // Only a scan someone is actually looking at is a reason to zoom to
                // radar scale. A volume for a site no pane is on — a fetch that landed
                // after the pane switched away — must not spend the one-shot latch.
                if any_pane_took_it {
                    self.claim_initial_zoom();
                }
            }
            // The live feed's counterpart to the arm above: the same site-wide
            // sweep, refusing the panes that are not following live. A pane
            // parked in the archive keeps its moment whichever subsystem
            // delivered the site's newest volume.
            GuiEvent::LiveScanInfoForSite { site, info } => {
                let mut any_pane_took_it = false;
                for pane in &mut self.panes {
                    if pane.site() == site && pane.viewing_live {
                        pane.scan_info = Some(info.clone());
                        any_pane_took_it = true;
                    }
                }
                self.end_radar_round(RoundOutcome::Delivered);
                if any_pane_took_it {
                    self.claim_initial_zoom();
                }
            }
            // The addressed counterpart to the arm above. Same writes, a
            // narrower audience: the requester's time group rather than the
            // whole site. Splitting them is the point — an archive volume one
            // pane scrubbed to must not overwrite a same-site pane parked at
            // its own moment, and a live volume must still reach every pane
            // on the site.
            GuiEvent::ScanInfoForTimeGroup {
                site,
                requester,
                info,
            } => {
                let mut any_pane_took_it = false;
                for idx in self.time_sync_targets_for(requester) {
                    if let Some(pane) = self.panes.get_mut(idx)
                        && pane.site() == site
                    {
                        pane.scan_info = Some(info.clone());
                        any_pane_took_it = true;
                    }
                }
                self.end_radar_round(RoundOutcome::Delivered);
                // Same one-shot latch, same reason as `ScanInfoForSite`.
                if any_pane_took_it {
                    self.claim_initial_zoom();
                }
            }
            // Apply scan info for a volume still being assembled from the real-time
            // chunk feed.
            GuiEvent::ChunkScanInfo { site, info: fresh } => {
                let mut any_pane_took_it = false;
                for pane in &mut self.panes {
                    // Same audience as `LiveScanInfoForSite`, and for the same
                    // reason: this is the same feed, a volume earlier. The
                    // merge below moves `timestamp`, so an ungated pass drags
                    // a parked pane forward exactly as the closed-volume path
                    // did.
                    if pane.site() != site || !pane.viewing_live {
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
                // Same guard as `ScanInfoForSite`, for the same reason: the chunk
                // feed keeps delivering a site's volume for a round or two after
                // the last pane on it switched away.
                if any_pane_took_it {
                    self.claim_initial_zoom();
                }
            }
            GuiEvent::ScanInfoForPane { pane_idx, info } => {
                if let Some(pane) = self.panes.get_mut(pane_idx) {
                    pane.scan_info = Some(info);
                }
            }
            GuiEvent::Fetching(fetching) => {
                // The layer's own in-flight flag, and since WO-E8d the only
                // one: `auto_fetch_delay` refuses to schedule a second round
                // on top of the one already in the air, and the rising edge
                // stamps the layer's poll clock.
                self.set_radar_round_in_flight(fetching);
            }
            // Set an error message. The spinner comes down and the archive
            // backoff advances with it — an error ends the wait it belonged to.
            GuiEvent::Error(error) => {
                // `end_radar_round` drops the in-flight flag itself, which is
                // why nothing clears a spinner beside it -- and it files the
                // message against the layer's own retry ledger, which is where
                // the banner reads it from. There is no second copy to keep.
                self.end_radar_round(RoundOutcome::Failed(&error));
            }
            GuiEvent::SelectedTime(timestamp) => self.time_dialog.select(timestamp),
            GuiEvent::ViewingLiveForPane { pane_idx, live } => {
                if let Some(pane) = self.panes.get_mut(pane_idx) {
                    pane.viewing_live = live;
                }
            }
            GuiEvent::PaneTimeSelected {
                pane_idx,
                instant,
                live,
            } => {
                if let Some(pane) = self.panes.get_mut(pane_idx) {
                    pane.viewing_live = live;
                    // **The half the step buttons never had.** `set_time_mode`
                    // settles every layer's playhead onto the new clock, which
                    // is the whole of what makes a pane holding no radar scan
                    // move at all.
                    pane.set_time_mode(if live {
                        crate::pane::TimeMode::Live
                    } else {
                        crate::pane::TimeMode::AsOf(instant)
                    });
                }
                // The dialog is app-wide and shows local time; `instant` is
                // UTC. One conversion, here, so the clock and the strings
                // cannot name different moments.
                self.time_dialog.select(
                    chrono::TimeZone::from_utc_datetime(&chrono::Local, &instant).naive_local(),
                );
            }
            GuiEvent::VolumePainter(painter) => {
                self.volume_painter = painter;
            }
            GuiEvent::TileMeshPainter(painter) => {
                self.tile_mesh_painter = painter;
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
        self.concurrent_renders = inputs.concurrent_renders;
        self.location_settings_available = inputs.location_settings_available;
        let (permission, active) = inputs.location;
        self.location_permission = permission;
        self.location_active = active;
        // The instant travels WITH the fix: `user_fix_at` answers "when did
        // this app last hear anything", stamped once at arrival (see the
        // field). Re-stamping it per frame would hold the settings pane's
        // staleness question at zero forever. `None` clears both halves.
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
        // Cloned, not borrowed: each entry is an id plus an `Arc`, so a frame
        // that publishes an unchanged status re-states the same allocation.
        self.liveness = inputs.liveness.to_vec();
        self.floor_tile_zoom_bias = inputs.floor_tile_zoom_bias;
        self.floor_strips
            .note_mirror_plan_stamp(inputs.mirror_plan_stamp);
        // Copies histograms only on the frame that closes a 2 s window, and
        // only while the overlay is showing; every other frame this is a
        // handful of compares.
        self.diagnostics.observe(
            self.diagnostics_panel,
            inputs.frame_diagnostics.as_ref(),
            web_time::Instant::now(),
        );
    }

    // **The chunk feed's three switches used to live here** — `live_chunks`,
    // `chunk_notifications` and `notifier_endpoint`, with a public setter each.
    // WO-E8b moved the fields into `RadarSource`, where the layer that uses
    // them can answer for them; the readers are
    // `crate::radar_layer::{live_chunks_enabled, chunk_notifications_enabled,
    // notifier_endpoint}` and the one write door is
    // [`Gui::apply_layer_control`], which names a layer and an update rather
    // than a field.

    /// **What every layer says it is doing**, as the App last stated it.
    ///
    /// The generic half of the liveness seam (WO-E8c): a caller that wants a
    /// particular layer's answer asks that layer's own glue to read it — see
    /// [`crate::radar_layer::chunk_status`] and
    /// [`crate::radar_layer::current_volume_for`].
    pub fn liveness(&self) -> &[squallar_source::liveness::SourceLiveness] {
        &self.liveness
    }

    /// The distinct sites some pane is watching live — the unit the chunk feed
    /// and the archive auto-poll both work in.
    pub fn live_sites(&self) -> Vec<String> {
        let mut sites: Vec<String> = Vec::new();
        for pane in self.panes.iter().take(self.pane_layout.pane_count) {
            if pane.viewing_live && !sites.iter().any(|s| s.as_str() == pane.site()) {
                sites.push(pane.site().to_string());
            }
        }
        sites
    }

    /// Whether a fetch someone is waiting on is in flight — asked of the
    /// radar layer, which is the only thing that knows.
    pub fn fetching(&self) -> bool {
        crate::radar_layer::archive_fetching(&self.overlays)
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
                self.time_dialog.select(chrono::Local::now().naive_local());
            }

            ui.add_space(15.0);

            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    let datetime_str = format!(
                        "{} {}",
                        self.time_dialog.date_string, self.time_dialog.time_string
                    );
                    if let Ok(timestamp) =
                        chrono::NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%d %H:%M:%S")
                    {
                        self.time_dialog.timestamp = timestamp;
                        if let Some(pane) = self.panes.get_mut(self.active_pane) {
                            pane.viewing_live = false;
                        }
                        // **The active pane's own site.** There is no
                        // app-wide site to fetch against any more: a pane
                        // carries its own, and the dialog belongs to the pane
                        // in front of the user. This arm used to fetch the
                        // persisted global instead, which is the opposite
                        // answer whenever a pane had been switched away from
                        // it -- see `the_time_dialogs_ok_fetches_the_active_panes_site`.
                        action = Some(GuiAction::FetchRadarScan(self.active_pane_fetch_config()));
                    }
                    self.time_dialog.show = false;
                }

                if ui.button("Cancel").clicked() {
                    // Both strings back to the time still selected, so a
                    // half-typed edit does not survive the dialog.
                    let selected = self.time_dialog.timestamp;
                    self.time_dialog.select(selected);
                    self.time_dialog.show = false;
                }
            });
        });
        action
    }

    pub(super) fn layers_panel_visible(&self) -> bool {
        if self.layout.width.has_persistent_sidebar() {
            self.stack_open.unwrap_or(true)
        } else {
            self.drawer_open
        }
    }

    /// The cross-section pane's own sidebar block: what the pane is cutting
    /// along, in the same header-then-indent shape as every other block in the
    /// panel.
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
                    let (_, km) = squallar_geo::site_bearing_range_km(
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
    pub(super) fn render_radar_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        combo_width: f32,
        id_prefix: &str,
    ) {
        // The Radar overlay toggle governs whether the *map* draws the radar
        // image over its tiles, which is not a question a pane with no map has.
        // Gated on it, a section or a volume pane converted while the toggle was
        // off would have no way to choose a product at all.
        if pane.is_map() && !pane.is_overlay_enabled(&known::RADAR) {
            return;
        }
        // A whole-volume pane has no tilt to pick: it reads the entire ladder,
        // which is what `RenderView::reads_whole_volume` means, so every entry in
        // the combo would select the same picture. `selected_elevation` stays on
        // the pane, inert, so going back to the plan view restores its tilt.
        let offer_tilt = !pane.render_view().reads_whole_volume();
        #[cfg(test)]
        let probes = &mut self.probes.widget_id_probes;
        {
            ui.indent(format!("{id_prefix}radar_controls"), |ui| {
                if pane.scan_info.is_none() {
                    ui.label("No scan loaded");
                    return;
                }
                {
                    let prev_product = pane.selected_product();
                    // **The restructure WO-E6a handed forward.** `scan_info`
                    // is an immutable borrow of ONE field of `pane`, and it
                    // used to be held across the product write and on into
                    // the tilt lookup below — so field-level borrow splitting
                    // was the only thing that made the write legal, and a
                    // `&mut self` setter beside it was E0502. The borrow now
                    // ends with the combo that needs it: the picked product
                    // comes out as a value, the write happens with no borrow
                    // outstanding, and the tilt block takes its own fresh
                    // borrow. Both writes go through the setters, and the
                    // fields they used to reach are private.
                    let (picked_product, product_combo_id) = {
                        let scan_info = pane.scan_info.as_ref().expect("presence checked above");
                        let product_combo =
                            egui::ComboBox::from_id_salt(format!("{id_prefix}product_sel"))
                                .selected_text(crate::field_facts::name(&pane.selected_product()))
                                .width(combo_width)
                                .show_ui(ui, |ui| {
                                    // The scan lists what it offers in the
                                    // radar layer's own terms; the combo names
                                    // fields by id.
                                    let options: Vec<_> = scan_info
                                        .available_products
                                        .iter()
                                        .map(|p| squallar_radar::fields::spec(*p).id.clone())
                                        .collect();
                                    pills::product_list_ui(ui, &options, &pane.selected_product())
                                        .picked
                                });
                        (product_combo.inner.flatten(), product_combo.response.id)
                    };
                    if let Some(picked) = picked_product {
                        pane.set_selected_product(picked);
                    }
                    #[cfg(test)]
                    probes.push(("product_sel", product_combo_id));
                    #[cfg(not(test))]
                    let _ = product_combo_id;
                    if prev_product != pane.selected_product() {
                        pane.set_selected_elevation(0.0);
                    }

                    // The tilt picker is drawn for every listed product, including
                    // one whose angles have not arrived yet.
                    let scan_info = pane.scan_info.as_ref().expect("presence checked above");
                    if let Some(elevations) = offer_tilt
                        .then(|| {
                            let product =
                                squallar_radar::fields::product_for(&pane.selected_product())?;
                            scan_info.product_elevations.get(&product)
                        })
                        .flatten()
                    {
                        let selected_angle = elevations
                            .iter()
                            .min_by(|a, b| {
                                ((**a - pane.selected_elevation()).abs())
                                    .total_cmp(&((**b - pane.selected_elevation()).abs()))
                            })
                            .copied()
                            .unwrap_or(pane.selected_elevation());

                        let combo = egui::ComboBox::from_id_salt(format!("{id_prefix}elev_sel"))
                            .selected_text(format!("{:.1}\u{b0}", selected_angle))
                            .width(combo_width);
                        let elev_combo = if elevations.is_empty() {
                            let scope = ui.add_enabled_ui(false, |ui| combo.show_ui(ui, |_| {}));
                            let id = scope.inner.response.id;
                            scope
                                .response
                                .on_hover_text("Waiting for this product's data");
                            id
                        } else {
                            let shown = combo.show_ui(ui, |ui| {
                                pills::tilt_list_ui(ui, elevations, pane.selected_elevation())
                                    .picked
                            });
                            if let Some(Some(angle)) = shown.inner {
                                pane.set_selected_elevation(angle);
                            }
                            shown.response.id
                        };
                        #[cfg(test)]
                        probes.push(("elev_sel", elev_combo));
                        #[cfg(not(test))]
                        let _ = elev_combo;
                    }
                }
            });
        }
    }

    /// Turn an overlay on or off for `pane` — **both halves**.
    pub(super) fn write_pane_overlay(
        overlays: &mut OverlayRegistry,
        pane_idx: usize,
        pane: &mut PaneState,
        kind: &LayerId,
        on: bool,
    ) {
        pane.hydrate_layer_states(overlays, pane_idx);
        // The pane's own state is where "on" lives — for every handler, since
        // WO-M10c. There is no second half to keep in step.
        pane.set_layer_enabled(overlays, pane_idx, kind, on);
        pane.adopt_handler_state(overlays);
        pane.release_disabled_overlay_textures();
        // The enabled set just moved, so which layer the transport addresses
        // may have moved with it — see `PaneState::refresh_transport`.
        pane.refresh_transport(overlays);
    }

    fn set_active_pane_overlay(&mut self, kind: &LayerId, on: bool) {
        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);
        Self::write_pane_overlay(&mut self.overlays, self.active_pane, &mut pane, kind, on);
        self.panes[self.active_pane] = pane;
    }

    /// Select `kind`'s options in the inspector and make sure it is open —
    /// what a stack row click means (plan §3.8).
    pub(super) fn select_layer(&mut self, kind: LayerId) {
        self.insp_scroll_reset = self.inspector_sel != InspectorSelection::Layer(kind.clone());
        self.inspector_sel = InspectorSelection::Layer(kind);
        self.insp_open = true;
    }

    /// Select the active pane's properties in the inspector and make sure it
    /// is open — the stack header's click, and the inspector crumb's `Pane N`
    /// segment.
    pub(super) fn select_pane_props(&mut self) {
        self.insp_scroll_reset = self.inspector_sel != InspectorSelection::PaneProps;
        self.inspector_sel = InspectorSelection::PaneProps;
        self.insp_open = true;
    }

    /// Open the inspector on the App › Settings body — what the menu's
    /// Settings… entry does, and where the crumb's `Pane N` segment goes from
    /// the pane-properties body.
    pub fn open_settings(&mut self) {
        self.insp_scroll_reset = self.inspector_sel != InspectorSelection::AppSettings;
        self.inspector_sel = InspectorSelection::AppSettings;
        self.insp_open = true;
    }

    /// Whether the settings body is on screen: the inspector is open and
    /// showing App › Settings.
    pub fn settings_visible(&self) -> bool {
        self.insp_open && self.inspector_sel == InspectorSelection::AppSettings
    }

    pub fn any_pane_has_overlay_enabled(&self, kind: &LayerId) -> bool {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .any(|p| p.draws_ground() && p.is_overlay_enabled(kind))
    }

    pub fn first_pane_with_overlay_enabled(&self, kind: &LayerId) -> Option<usize> {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .position(|p| p.draws_ground() && p.is_overlay_enabled(kind))
    }

    pub fn active_pane(&self) -> &PaneState {
        &self.panes[self.active_pane]
    }

    pub fn active_pane_idx(&self) -> usize {
        self.active_pane
    }

    pub fn active_pane_mut(&mut self) -> &mut PaneState {
        &mut self.panes[self.active_pane]
    }

    pub fn panes(&self) -> &[PaneState] {
        &self.panes[..self.visible_pane_count()]
    }

    /// The off-screen strips this frame's 3D panes drew their own maps into,
    /// as rects in **points** — plus whether this pass actually painted them.
    ///
    /// The `repainted` half is what the strip cache decided
    /// (`map::FloorStrips`), and the app must obey it both ways: rendering
    /// the mirror from a pass that skipped painting would clear every floor
    /// to transparent, and keeping the mirror across a pass that repainted
    /// would freeze it stale. The field is private so the answer cannot be
    /// composed anywhere but here.
    pub fn mirror_source_rects(&self) -> MirrorSources {
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
            if !rects.contains(&geo.rect) {
                rects.push(geo.rect);
            }
        }
        MirrorSources {
            rects,
            repainted: self.floor_strips.painted_this_pass(),
        }
    }

    /// How much of egui's coordinate space the mirror has to cover this frame,
    /// in **points**: the frame itself, plus however far below it the 3D panes'
    /// off-screen map strips reach.
    pub fn mirror_size_points(&self) -> egui::Vec2 {
        self.mirror_size_points
    }

    /// The tile zoom bias for one pane: the frame's bias if this pane is
    /// drawing a floor strip, zero otherwise.
    ///
    /// Gated on the **styled** cache bound only, deliberately: the
    /// parsed-geometry cache (`tile_source::PARSED_TILE_CACHE_ENTRIES`) is an
    /// economy cache — a bias whose working set overruns it costs a refetch
    /// on the next restyle, never a frame — so admitting it here would trade
    /// a frame guarantee against an economy one. See that constant's docs.
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

    /// How many tiles every floor strip together would keep resident at `bias`
    /// — worst case over the whole zoom range, because a bias the frame can
    /// only afford at a whole zoom is one it cannot afford.
    ///
    /// **One tile pyramid per strip, and no longer a per-pane count.** The
    /// figure used to be `1 + city labels are on`, because the label raster was
    /// a second source fetching a second pyramid over the same ground. The
    /// vector basemap draws its labels out of the tile it already has, so there
    /// is one pyramid whatever the pane has switched on, and this term stopped
    /// depending on any layer's state.
    ///
    /// **The Terrain layer does not re-open that question**, and the reason is
    /// worth spelling since it looks like the labels story come back: terrain
    /// *is* a second pyramid over the same ground, but it is a second
    /// **source** with its own cache at its own bound — and
    /// `tile_source::TERRAIN_TILE_CACHE_ENTRIES` equals `TILE_CACHE_ENTRIES`
    /// precisely so this per-source comparison stays the right one for both.
    /// Each source's working set at `bias` is the figure below unchanged; a
    /// `layers: 2` here would gate the basemap's cache on the *sum* of two
    /// caches neither of which holds it.
    fn floor_tile_working_set(&self, bias: u8) -> usize {
        self.panes()
            .iter()
            .enumerate()
            .filter(|(idx, _)| self.is_floor_source(*idx))
            .map(|(idx, _)| {
                let rect = self
                    .map_pane_geo
                    .get(&idx)
                    .map_or(egui::Rect::ZERO, |geo| geo.rect);
                crate::tiles::tiles_resident_for(rect, bias, 1)
            })
            .sum()
    }

    pub fn remembered_pane_count(&self) -> usize {
        self.panes.len()
    }

    /// **The panes and the registry at once**, disjointly borrowed.
    ///
    /// A caller that has to hand a handler its pane needs both halves live:
    /// the registry to ask, and the pane to ask *about*. Reaching for
    /// `gui.overlays` and `gui.panes_mut()` in turn cannot express that, and
    /// the config swap is what used to stand in for it.
    pub fn panes_and_overlays_mut(&mut self) -> (&mut [PaneState], &mut OverlayRegistry) {
        (&mut self.panes, &mut self.overlays)
    }

    pub fn panes_mut(&mut self) -> &mut [PaneState] {
        let count = self.visible_pane_count();
        &mut self.panes[..count]
    }

    /// [`Self::panes_mut`] with the registry beside it, for a caller that has
    /// to ask a handler about the pane it is walking.
    ///
    /// **Not [`Self::panes_and_overlays_mut`]**, and the difference is the
    /// whole reason this exists: that one hands out *every* pane, which is
    /// what the arrival path wants — a listing lands for a pane whether or
    /// not the layout is currently showing it. A per-frame walk wants the
    /// visible slice, and reaching for the wider door would silently widen
    /// the walk.
    pub fn visible_panes_and_overlays_mut(&mut self) -> (&mut [PaneState], &mut OverlayRegistry) {
        let count = self.pane_layout.pane_count.min(self.panes.len());
        (&mut self.panes[..count], &mut self.overlays)
    }

    /// `pane_count` clamped to what the vector actually holds. The two are kept in
    /// step by every path that changes the layout, but slicing past the end would
    /// panic, and no pane update is worth a crash.
    fn visible_pane_count(&self) -> usize {
        self.pane_layout.pane_count.min(self.panes.len())
    }

    pub fn pane(&self, idx: usize) -> Option<&PaneState> {
        self.panes.get(idx)
    }

    pub fn pane_mut(&mut self, idx: usize) -> Option<&mut PaneState> {
        self.panes.get_mut(idx)
    }

    pub(crate) fn request_pane_view(
        &mut self,
        pane_idx: PaneId,
        view: squallar_radar::types::RenderView,
    ) {
        self.pending_pane_view = Some((pane_idx, view));
    }

    /// Grow or shrink the layout to `count` panes, seeding any new ones, and
    /// report whether the layout actually reached that count.
    fn set_pane_count(&mut self, count: usize) -> bool {
        let active_site = self.panes[self.active_pane].site().to_string();
        let active_scan_info = self.panes[self.active_pane].scan_info.clone();
        while self.panes.len() < count {
            let mut new_pane = PaneState::with_site(active_site.clone());
            new_pane.scan_info = active_scan_info.clone();
            self.panes.push(new_pane);
        }
        // A pane born here has empty overlay maps, and `is_overlay_enabled` reads
        // a missing entry as *off*. Seed it from the handlers, which hold the
        // active pane's state, the same way startup does.
        self.initialize_pane_enabled();
        self.pane_layout = PaneLayout::for_count(count, self.layout.width, self.split_orientation);
        if self.active_pane >= self.pane_layout.pane_count {
            self.active_pane = 0;
        }
        self.pane_layout.pane_count == count
    }

    /// Ask for pane `idx` to be closed. Applied at the end of the frame — see
    /// [`Gui::apply_pending_pane_close`] — because the active pane is
    /// `mem::take`n out of the vector while the inspector and the layers panel
    /// draw, and removing a slot underneath that would restore the taken pane
    /// into the wrong one.
    pub(crate) fn request_pane_close(&mut self, idx: PaneId) {
        self.pending_pane_close = Some(idx);
    }

    /// **Close one specific pane, and let go of everything keyed on the
    /// indices that just moved.**
    ///
    /// `PaneId` is a slot position, so closing pane *n* renumbers every pane
    /// above it. Stable ids decoupled from position are the real fix; this is
    /// the pragmatic one — renumber, and drop the in-flight work for `idx` and
    /// above rather than try to follow it.
    ///
    /// What that covers, and why each one is here:
    ///
    /// * **`actions`** — every action this frame already queued naming a pane
    ///   at or above `idx`. They were addressed to panes that no longer sit
    ///   there. Matched through [`GuiAction::pane_idx`], which is exhaustive
    ///   so a future variant cannot escape it.
    /// * **The app's per-pane stores**, through two actions, because they are
    ///   the app's and this crate cannot reach them:
    ///   [`GuiAction::ReleaseVolume`] for every *old* index from `idx` up, for
    ///   the volume store's index-keyed refcount and the GPU resources it
    ///   names; and [`GuiAction::PaneClosed`] for the positional render state
    ///   and the `(pane, layer)`-keyed overlay dispatch records.
    /// * **`map_pane_geo` and `volume_empty_states`**, both `HashMap<usize,_>`
    ///   over pane indices. Rebuilt every frame, cleared here anyway so no
    ///   reader between now and the next `render_panes` sees the wrong pane's
    ///   answer.
    /// * **Each moved pane's in-flight marks**, abandoned whole. The dispatch
    ///   that set one is recorded under the pane's *old* index, so its raster
    ///   is not this pane's; without abandoning the mark the pane would never
    ///   ask again. And the raster still on its way is now refused on arrival
    ///   rather than filed to whichever pane took the index — see
    ///   [`crate::overlay_cache::RendersInFlight::retire`].
    /// * **Each moved pane's `rendered_for`.** It is what the level-triggered
    ///   [`GuiAction::PrepareVolume`] dedupes on, and the volume it names has
    ///   just been released above.
    /// * **The pending appliers** that carry a `PaneId`, and the pill row's
    ///   reveal state plus its popovers, whose egui `Id`s are salted on the
    ///   pane index — a popover left open on old pane 3 would reopen on
    ///   whichever pane lands at 3.
    ///
    /// Returns whether a pane was closed. The last pane is never closed: a
    /// window with no pane has nothing to show.
    pub(crate) fn close_pane(
        &mut self,
        ctx: &egui::Context,
        idx: PaneId,
        actions: &mut Vec<GuiAction>,
    ) -> bool {
        let old_count = self.visible_pane_count();
        if old_count <= 1 || idx >= old_count || idx >= self.panes.len() {
            return false;
        }

        actions.retain(|action| action.pane_idx().is_none_or(|at| at < idx));
        for stale in idx..old_count {
            actions.push(GuiAction::ReleaseVolume { pane_idx: stale });
        }
        actions.push(GuiAction::PaneClosed { pane_idx: idx });

        self.panes.remove(idx);

        self.map_pane_geo.clear();
        self.volume_empty_states.clear();
        self.pill_revealed = None;
        for stale in idx..old_count {
            pills::close_pane_popovers(ctx, stale);
        }
        if self.pending_pane_view.is_some_and(|(at, _)| at >= idx) {
            self.pending_pane_view = None;
        }
        if self
            .pending_section_line
            .as_ref()
            .is_some_and(|(at, _)| *at >= idx)
        {
            self.pending_section_line = None;
        }
        if self
            .pending_region
            .as_ref()
            .is_some_and(|(at, _)| *at >= idx)
        {
            self.pending_region = None;
        }
        if self
            .pending_section_edit
            .as_ref()
            .is_some_and(|(at, _)| *at >= idx)
        {
            self.pending_section_edit = None;
        }

        for pane in self.panes.iter_mut().skip(idx) {
            for cache in pane.overlay_textures.values_mut() {
                cache.renders.abandon_all();
            }
            if let Some(volume) = pane.volume_mut() {
                volume.rendered_for = None;
            }
        }

        let new_count = old_count - 1;
        // **The neighbour, not pane 0.** Closing the pane you are looking at
        // moves you to the one that slid into its place — or, if it was the
        // last, to the one before it. Closing any other pane leaves you on the
        // pane you were on, under its new number.
        self.active_pane = match self.active_pane {
            at if at < idx => at,
            at if at == idx => idx.min(new_count - 1),
            at => at - 1,
        };
        self.pane_layout =
            PaneLayout::for_count(new_count, self.layout.width, self.split_orientation);
        true
    }

    /// **Take the user's split preference and re-lay the grid to it now.**
    ///
    /// Applied here rather than waiting for the next frame's
    /// `settle_pane_layout`, so that the click and the new arrangement are the
    /// same frame; the settle then finds nothing to do.
    pub(crate) fn set_split_orientation(&mut self, orientation: crate::pane::SplitOrientation) {
        if self.split_orientation == orientation {
            return;
        }
        self.split_orientation = orientation;
        self.pane_layout.reflow(self.layout.width, orientation);
    }

    #[cfg(test)]
    pub(crate) fn section_edit_drag_for_test(
        &self,
    ) -> Option<crate::ui_section_edit::SectionEditDrag> {
        self.section_edit_drag
    }

    pub fn section_draw_armed(&self) -> bool {
        self.section_draw_armed
    }

    pub(crate) fn set_section_draw_armed(&mut self, armed: bool) {
        self.section_draw_armed = armed;
        if armed {
            // An endpoint drag cannot share a map with an armed draw, and the
            // mode was asked for last.
            self.section_edit_drag = None;
            self.region_pick_armed = false;
            self.region_drag = None;
            self.set_download_pick_armed(false);
        } else {
            self.section_anchor = None;
        }
    }

    pub fn region_pick_armed(&self) -> bool {
        self.region_pick_armed
    }

    pub(crate) fn set_region_pick_armed(&mut self, armed: bool) {
        self.region_pick_armed = armed;
        if armed {
            self.section_edit_drag = None;
            self.section_draw_armed = false;
            self.section_anchor = None;
            self.set_download_pick_armed(false);
        } else {
            self.region_drag = None;
        }
    }

    /// Whether the offline download pick is armed.
    pub fn download_pick_armed(&self) -> bool {
        self.download_pick_armed
    }

    /// Arm or disarm the offline download pick.
    ///
    /// The 3D pick's setter with the box in place of the volume: the three
    /// modal drags are mutually exclusive, and disarming drops the half-drawn
    /// box rather than leaving it to be finished by a pan.
    pub(crate) fn set_download_pick_armed(&mut self, armed: bool) {
        self.download_pick_armed = armed;
        if armed {
            self.section_edit_drag = None;
            self.section_draw_armed = false;
            self.section_anchor = None;
            self.region_pick_armed = false;
            self.region_drag = None;
        } else {
            self.download_drag = None;
        }
    }

    /// Forget the picked box and the level list it drew.
    ///
    /// **Neither a running download nor a finished area's record is touched.**
    /// The engine's run belongs to `active_download`, which the Downloaded
    /// areas screen also watches, and an area whose bytes are on disk does not
    /// stop existing because the box that picked it was cleared.
    pub(crate) fn clear_download_pick(&mut self) {
        self.download_pick = None;
        self.download_size.set_box(None);
    }

    /// What the origin's storage has, for the quota arithmetic in
    /// [`crate::ui_download_area`].
    ///
    /// **The production wire is OWED, and it is owed by the web store**, not
    /// by this method. The route's client exists and is tested
    /// ([`HttpSegmentStore::quota`](crate::basemap_download::HttpSegmentStore::quota)),
    /// but the only platform with a quota to report is web, and
    /// `tiles::offline_store`'s wasm32 arm still answers `None` because the
    /// service worker's store is not reachable from the Rust side yet. Until
    /// it is, nothing outside a test writes this field, and the level list's
    /// quota arithmetic is reachable only from the suite that pins it.
    ///
    /// Stated here rather than papered over with a `pub` seam nobody calls:
    /// a public setter on `Gui` is a build failure under
    /// `arch_ratchets::the_gui_setter_surface_never_grows`, whose ceiling is 0
    /// and may only fall, and inventing a differently-spelled one to carry a
    /// value no shipped caller supplies would be the re-spelling that rule
    /// forbids by name.
    #[cfg(test)]
    pub(crate) fn set_download_quota(
        &mut self,
        quota: Option<crate::basemap_download::OfflineQuota>,
    ) {
        self.download_quota = quota;
    }

    /// The committed download box, if one was picked.
    #[cfg(test)]
    pub(crate) fn download_pick(&self) -> Option<crate::ui_download_area::PickedBox> {
        self.download_pick
    }

    /// Which detail level the level list has selected.
    #[cfg(test)]
    pub(crate) fn download_detail(&self) -> crate::ui_download_area::DetailLevel {
        self.download_detail
    }

    /// State the archive ceiling stored depths are named against — the tests'
    /// seam; see
    /// [`AreaSizeProbe::seed_ceiling`](crate::ui_download_area::AreaSizeProbe::seed_ceiling).
    #[cfg(test)]
    pub(crate) fn seed_archive_ceiling(&mut self, ceiling: u8) {
        self.download_size.seed_ceiling(ceiling);
    }

    /// Hand the size probe a ceiling and one level's figure — the tests' seam;
    /// see [`AreaSizeProbe::seed`](crate::ui_download_area::AreaSizeProbe::seed).
    #[cfg(test)]
    pub(crate) fn seed_download_size(
        &mut self,
        ceiling: u8,
        level: crate::ui_download_area::DetailLevel,
        bytes: u64,
    ) {
        self.download_size.seed(ceiling, level, bytes);
    }

    /// [`Self::seed_download_size`] for the hillshade half — the terrain
    /// archive's own figure, which the combined one is this plus the basemap's.
    #[cfg(test)]
    pub(crate) fn seed_download_terrain_size(
        &mut self,
        ceiling: u8,
        level: crate::ui_download_area::DetailLevel,
        bytes: u64,
    ) {
        self.download_size.seed_archive(
            ceiling,
            level,
            crate::basemap_download::AreaArchive::Terrain,
            bytes,
        );
    }

    /// Whether the download panel's checkbox is ticked right now — the
    /// user's explicit choice if they made one, otherwise the terrain switch.
    #[cfg(test)]
    pub(crate) fn download_terrain_wanted(&self) -> bool {
        self.download_wants_terrain()
    }

    pub(crate) fn region_preview(&self, pane_idx: PaneId) -> Option<(squallar_geo::GeoPoint, f64)> {
        self.region_drag
            .filter(|drag| drag.pane_idx() == pane_idx)
            .map(|drag| (drag.centre(), drag.half_width_km()))
    }

    /// The download box being dragged on `pane_idx` right now, if one is.
    pub(crate) fn download_preview(
        &self,
        pane_idx: PaneId,
    ) -> Option<(squallar_geo::GeoPoint, f64)> {
        self.download_drag
            .filter(|drag| drag.pane_idx() == pane_idx)
            .map(|drag| (drag.centre(), drag.half_width_km()))
    }

    pub(crate) fn section_rubber_band(&self, pane_idx: PaneId) -> Option<(egui::Pos2, egui::Pos2)> {
        let anchor = self.section_anchor.as_ref()?;
        (anchor.pane_idx == pane_idx).then_some((anchor.screen, anchor.current))
    }

    fn volume_pane_sourced_from(&self, source: PaneId) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| {
            self.panes[idx]
                .volume()
                .is_some_and(|v| v.source_pane == Some(source))
        })
    }

    fn lowest_volume_pane(&self) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| self.panes[idx].volume().is_some())
    }

    fn section_pane_sourced_from(&self, source: PaneId) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| {
            self.panes[idx]
                .cross_section()
                .is_some_and(|s| s.source_pane == Some(source))
        })
    }

    fn grown_pane(&mut self) -> Option<PaneId> {
        let wanted = self.pane_layout.pane_count + 1;
        if wanted > self.layout.width.max_panes() {
            return None;
        }
        self.set_pane_count(wanted).then(|| wanted - 1)
    }

    fn lowest_section_pane(&self) -> Option<PaneId> {
        (0..self.visible_pane_count()).find(|&idx| self.panes[idx].cross_section().is_some())
    }

    fn highest_pane_other_than(&self, source: PaneId) -> Option<PaneId> {
        (0..self.visible_pane_count())
            .rev()
            .find(|&idx| idx != source)
    }

    #[cfg(test)]
    pub(crate) fn pending_pane_view_for_test(
        &self,
    ) -> Option<(PaneId, squallar_radar::types::RenderView)> {
        self.pending_pane_view
    }

    #[cfg(test)]
    pub(crate) fn volume_arms_for_test(&self) -> &[VolumeArmProbe] {
        &self.probes.last_volume_arms
    }

    #[cfg(test)]
    pub(crate) fn pane_borders_for_test(
        &self,
    ) -> &[(usize, egui::Rect, crate::ui::map::PaneBorderMarks)] {
        &self.probes.last_pane_borders
    }

    #[cfg(test)]
    pub(crate) fn section_tracks_for_test(&self) -> &[(usize, usize, egui::Pos2, egui::Pos2)] {
        &self.probes.last_section_tracks
    }

    #[cfg(test)]
    pub(crate) fn region_boxes_for_test(&self) -> &[(usize, usize, egui::Rect)] {
        &self.probes.last_region_boxes
    }

    #[cfg(test)]
    pub(crate) fn paint_order_for_test(&self, idx: usize) -> Vec<(LayerId, egui::LayerId)> {
        self.probes
            .last_paint_order
            .iter()
            .find(|(pane, _)| *pane == idx)
            .map(|(_, order)| order.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn alpha_buttons_for_test(&self) -> &[(usize, egui::Rect)] {
        &self.probes.last_alpha_buttons
    }

    pub fn pane_has_no_plan_view(&self, idx: PaneId) -> bool {
        self.pane(idx).is_some_and(|pane| !pane.is_map())
    }

    pub fn pane_cannot_loop(&self, idx: PaneId) -> bool {
        self.pane(idx).is_some_and(|pane| !pane.can_loop())
    }

    /// Whether the storm motion vector is being edited *right now*, so that a
    /// consumer which spends real work on a change can wait for the release.
    pub fn storm_motion_mid_edit(&self) -> bool {
        self.storm_motion_editing
    }

    /// Whether pane `idx` needs every cut of its site's volume rather than the
    /// one tilt it has selected, because of *what kind of pane it is*.
    pub fn pane_consumes_whole_volume(&self, idx: PaneId) -> bool {
        self.pane(idx)
            .is_some_and(|pane| pane.render_view().reads_whole_volume())
    }

    pub fn get_rendering_params_for_pane(&self, pane_idx: PaneId) -> Option<(FieldId, f32)> {
        self.panes
            .get(pane_idx)
            .and_then(|p| p.get_rendering_params())
    }

    pub fn pane_count(&self) -> usize {
        self.pane_layout.pane_count
    }

    #[cfg(test)]
    pub(crate) fn set_pane_count_for_test(&mut self, count: usize) {
        while self.panes.len() < count {
            self.panes.push(PaneState::new());
        }
        self.pane_layout = PaneLayout::for_count(count, self.layout.width, self.split_orientation);
        if self.active_pane >= count {
            self.active_pane = 0;
        }
    }

    /// Put this `Gui` at a width class without running a frame — what a config
    /// test needs, since `load_ui_config` runs before the first `ui()` and
    /// reads whatever the layout last resolved to.
    #[cfg(test)]
    pub(crate) fn set_width_class_for_test(&mut self, width: crate::ui_layout::WidthClass) {
        self.layout.width = width;
    }

    #[cfg(test)]
    pub(crate) fn pane_layout_for_test(&self) -> &PaneLayout {
        &self.pane_layout
    }

    #[cfg(test)]
    pub(crate) fn pane_layout_mut_for_test(&mut self) -> &mut PaneLayout {
        &mut self.pane_layout
    }

    #[cfg(test)]
    pub(crate) fn split_orientation_for_test(&self) -> crate::pane::SplitOrientation {
        self.split_orientation
    }

    #[cfg(test)]
    pub(crate) fn map_panel_rect_for_test(&self) -> egui::Rect {
        self.probes.last_map_panel_rect
    }

    /// Every basemap credit the last frame drew. One per *panel* is correct;
    /// more than one means it slipped into the pane loop.
    #[cfg(test)]
    pub(crate) fn attribution_rects_for_test(&self) -> &[egui::Rect] {
        &self.probes.last_attribution
    }

    /// See [`crate::tiles::MapTileState::latch_base_unreachable_for_test`].
    #[cfg(test)]
    pub(crate) fn latch_base_unreachable_for_test(&mut self) {
        self.map_tiles.latch_base_unreachable_for_test();
    }

    /// See [`crate::tiles::MapTileState::fail_reads_for_test`].
    #[cfg(test)]
    pub(crate) fn fail_reads_for_test(&mut self, base: bool) -> bool {
        self.map_tiles.fail_reads_for_test(base)
    }

    #[cfg(test)]
    pub(crate) fn widget_id_probes(&self) -> &[(&'static str, egui::Id)] {
        &self.probes.widget_id_probes
    }

    #[cfg(test)]
    pub(crate) fn menu_leaves_for_test(&self) -> &[ui_menu::DrawnMenuLeaf] {
        &self.probes.last_menu_leaves
    }

    #[cfg(test)]
    pub(crate) fn pane_pointers_for_test(&self) -> &[crate::ui_input::PanePointerProbe] {
        &self.probes.last_pane_pointers
    }

    #[cfg(test)]
    pub(crate) fn pane_content_for_test(&self) -> &[PaneContentProbe] {
        &self.probes.last_pane_content
    }

    #[inline]
    pub(super) fn record_pane_content(
        &mut self,
        _pane_idx: usize,
        _view: squallar_radar::types::RenderView,
        _rect: egui::Rect,
    ) {
        #[cfg(test)]
        self.probes.last_pane_content.push(PaneContentProbe {
            pane_idx: _pane_idx,
            view: _view,
            rect: _rect,
        });
    }

    #[cfg(test)]
    pub(crate) fn pane_options_for_test(&self) -> &[PaneOptionProbe] {
        &self.probes.last_pane_options
    }

    #[cfg(test)]
    pub(crate) fn split_options_for_test(&self) -> &[SplitOptionProbe] {
        &self.probes.last_split_options
    }

    #[cfg(test)]
    pub(crate) fn set_active_pane_for_test(&mut self, idx: PaneId) {
        assert!(
            idx < self.pane_layout.pane_count,
            "no pane {idx} to activate"
        );
        self.active_pane = idx;
    }

    #[cfg(test)]
    pub(crate) fn map_excluded_rects_for_test(&self) -> &[egui::Rect] {
        &self.probes.last_map_excluded_rects
    }

    #[cfg(test)]
    pub(crate) fn status_bar_for_test(&self) -> &StatusBarProbe {
        &self.probes.last_status_bar
    }

    #[cfg(test)]
    pub(crate) fn timeline_for_test(&self) -> &TimelineProbe {
        &self.probes.last_timeline
    }

    #[cfg(test)]
    pub(crate) fn top_bar_for_test(&self) -> &TopBarProbe {
        &self.probes.last_top_bar
    }

    #[cfg(test)]
    pub(crate) fn bottom_bar_for_test(&self) -> &BottomBarProbe {
        &self.probes.last_bottom_bar
    }

    #[cfg(test)]
    pub(crate) fn sheet_for_test(&self) -> &SheetProbe {
        &self.probes.last_sheet
    }

    #[cfg(test)]
    pub(crate) fn error_toast_for_test(&self) -> Option<ErrorToastProbe> {
        self.probes.last_error_toast
    }

    #[cfg(test)]
    pub(crate) fn set_sheet_menu_open_for_test(&mut self, open: bool) {
        self.menu_open = open;
    }

    #[cfg(test)]
    pub(crate) fn stack_for_test(&self) -> &StackProbe {
        &self.probes.last_stack
    }

    #[cfg(test)]
    pub(crate) fn inspector_for_test(&self) -> &InspectorProbe {
        &self.probes.last_inspector
    }

    /// What the inspector would open on next — the field, not the paint.
    ///
    /// The probe reports the body that *drew*, which needs the panel open. The
    /// Compact hosts offer no route that reopens the inspector without also
    /// asserting a body (every bottom-bar item and every stack row names one),
    /// so the pin that a close leaves the selection alone has nothing else to
    /// read there.
    #[cfg(test)]
    pub(crate) fn inspector_selection_for_test(&self) -> &InspectorSelection {
        &self.inspector_sel
    }

    #[cfg(test)]
    pub(crate) fn catalog_for_test(&self) -> &CatalogProbe {
        &self.probes.last_catalog
    }

    #[cfg(test)]
    pub(crate) fn pill_rows_for_test(&self) -> &[pills::PillRowProbe] {
        &self.probes.last_pills
    }

    #[cfg(test)]
    pub(crate) fn pill_popover_for_test(&self) -> Option<&pills::PillPopoverProbe> {
        self.probes.last_pill_popover.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn click_consumed_for_test(&self) -> bool {
        self.click_consumed_frame
    }

    /// Install a preset directly, so a test can build one this build's own
    /// save path cannot produce — in particular one naming a field no
    /// registered source offers, which is exactly the case the open-id
    /// preserve rule exists for.
    #[cfg(test)]
    pub(crate) fn push_preset_for_test(&mut self, preset: PresetConfig) {
        self.presets.push(preset);
    }

    #[cfg(test)]
    pub(crate) fn presets_for_test(&self) -> &[PresetConfig] {
        &self.presets
    }

    #[cfg(test)]
    pub(crate) fn control_render_passes_for_test(&self) -> u32 {
        self.probes.control_render_passes
    }

    /// Open or close the Set Time dialog directly, for fixtures.
    #[cfg(test)]
    pub(crate) fn set_time_dialog_open_for_test(&mut self, open: bool) {
        self.time_dialog.show = open;
    }

    /// Open or close the layer catalog directly, for fixtures.
    #[cfg(test)]
    pub(crate) fn set_catalog_open_for_test(&mut self, open: bool) {
        self.catalog_open = open;
    }

    #[cfg(test)]
    pub(crate) fn active_pane_index_for_test(&self) -> PaneId {
        self.active_pane
    }

    /// Set every pane's layer link at once, for tests that need panes to disagree.
    #[cfg(test)]
    pub(crate) fn set_layer_links_for_test(&mut self, on: bool) {
        for pane in &mut self.panes {
            pane.layer_link = on;
        }
    }

    /// Whether every pane's layer link is on.
    #[cfg(test)]
    pub(crate) fn all_layer_linked_for_test(&self) -> bool {
        self.panes.iter().all(|pane| pane.layer_link)
    }

    /// Open or close the layers drawer, as the top bar's Layers toggle does.
    #[cfg(test)]
    pub(crate) fn set_drawer_open(&mut self, open: bool) {
        self.drawer_open = open;
    }

    #[cfg(test)]
    pub(crate) fn dropdowns_for_test(&self) -> &[DrawnDropdown] {
        &self.probes.last_dropdowns
    }

    #[cfg(test)]
    pub(crate) fn control_items_for_test(&self) -> &[DrawnControlItem] {
        &self.probes.last_control_items
    }

    #[cfg(test)]
    pub(crate) fn settings_rows_for_test(&self) -> &[settings::DrawnSettingsRow] {
        &self.probes.last_settings_rows
    }

    /// What the last frame's detail popup did with its action buttons.
    #[cfg(test)]
    pub(crate) fn popup_actions_for_test(&self) -> (Vec<usize>, Vec<usize>) {
        (
            self.probes.last_popup_triggered.clone(),
            self.probes.last_popup_handled.clone(),
        )
    }

    #[cfg(test)]
    pub(crate) fn layout_for_test(&self) -> LayoutCtx {
        self.layout
    }

    #[cfg(test)]
    pub(crate) fn pane_rects_for_test(&self) -> Vec<egui::Rect> {
        let panel = self.probes.last_map_panel_rect;
        (0..self.visible_pane_count())
            .map(|idx| self.pane_layout.pane_rect(idx, panel))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn claim_pane_count_for_test(&mut self, count: usize) {
        self.pane_layout = PaneLayout::for_count(count, self.layout.width, self.split_orientation);
    }

    /// Whether pane `idx`'s layer state belongs to the linked group. Out of
    /// bounds answers linked: the default every real pane starts with.
    pub fn pane_layer_linked(&self, idx: usize) -> bool {
        self.panes.get(idx).is_none_or(|pane| pane.layer_link)
    }

    /// **Whether pane `idx` draws any layer that comes in stamped frames** —
    /// the question a one-frame step is only meaningful for, asked of the
    /// layers themselves rather than by knowing which layer is radar.
    ///
    /// The registry half of the pair whose other half is
    /// [`PaneState::clock_layer`]: this one answers for a pane that is
    /// animating nothing, which is exactly the pane the step picker has to
    /// decide about.
    ///
    /// Presence is [`PaneState::topmost_frame_series_layer`] having an answer
    /// — the same walk the loop transport picks its layer with, so the step
    /// picker and the transport cannot disagree about which panes have frames.
    ///
    /// [`PaneState::clock_layer`]: crate::pane::PaneState::clock_layer
    /// [`PaneState::topmost_frame_series_layer`]: crate::pane::PaneState::topmost_frame_series_layer
    pub fn pane_has_frame_series_layer(&self, idx: usize) -> bool {
        self.panes
            .get(idx)
            .is_some_and(|pane| pane.topmost_frame_series_layer(&self.overlays).is_some())
    }

    /// Whether pane `idx` follows shared time — the loop playback
    /// synchroniser's per-pane gate.
    pub fn pane_time_linked(&self, idx: usize) -> bool {
        self.panes.get(idx).is_none_or(|pane| pane.time_link)
    }

    /// The panes a layer-wide change on pane `src` reaches: the visible
    /// layer-linked panes **in `src`'s own group** when `src` is itself
    /// linked, or `src` alone when it is not. A pane in no group reaches
    /// nobody, whatever its flag says.
    pub fn layer_sync_targets(&self, src: usize) -> Vec<usize> {
        let count = self.visible_pane_count();
        if count > 1 && self.pane_layer_linked(src) {
            (0..count)
                .filter(|&idx| idx == src || self.panes_layer_linked(src, idx))
                .collect()
        } else {
            vec![src]
        }
    }

    /// Whether one overlay render may serve several panes: every visible pane
    /// that draws ground is viewport-linked *and* layer-linked, **and they are
    /// all in one link group**. One pane out of any of the three and nothing
    /// is grouped — the dedup key carries no geo bounds and a shared texture
    /// would land on a pane whose map is elsewhere, which two groups
    /// guarantee it is.
    pub fn overlay_renders_groupable(&self) -> bool {
        let mut ground =
            (0..self.visible_pane_count()).filter(|&idx| self.panes[idx].draws_ground());
        let Some(first) = ground.next() else {
            return true;
        };
        let linked = |idx: usize| {
            let pane = &self.panes[idx];
            pane.viewport_link && pane.layer_link && pane.group.is_some()
        };
        linked(first) && ground.all(|idx| linked(idx) && self.panes_share_group(first, idx))
    }

    /// **The time on display**, which every navigation the shell drives reads
    /// before it writes a new one.
    pub fn selected_timestamp(&self) -> chrono::NaiveDateTime {
        self.time_dialog.timestamp
    }

    /// Take down the site-switch spinner on every pane showing `site`.
    ///
    /// **Edge-triggered, and it has to be.** The cache token for
    /// [`known::RADAR_SITES`] is `radar_sites_render_gen` and nothing else, so
    /// a bump here is a full-size site raster dispatched, rasterized, uploaded
    /// and promoted. This is called on **every sealed cut of the live chunk
    /// feed** (`App::apply_chunk_outcome`, both arms) as well as on every scan
    /// result and every failed fetch — a cadence of seconds, all day, on a
    /// pane whose spinner has been down since the switch completed. Bumping
    /// unconditionally spent one whole picture per call for a picture that was
    /// byte-identical.
    ///
    /// Measured on the Tier-2 rig before this guard, one leg of ~40 s on a
    /// live feed: `overlay/sites` ran **13 to 16 times** per leg against
    /// `overlay/alerts`' 2 to 3, on a scene where the site table never moved.
    ///
    /// The same discipline the theme path already keeps — see
    /// `a_theme_change_invalidates_the_site_labels_exactly_once` — and the
    /// same one `scan_info_learning_position` keeps for a volume restating a
    /// position it already taught.
    pub fn clear_loading_site_for_site(&mut self, site: &str) {
        for pane in &mut self.panes {
            // `take`, so the clear and the test of whether there was anything
            // to clear are one statement and cannot disagree.
            if pane.site() == site && pane.loading_site.take().is_some() {
                pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            }
        }
    }

    /// **A fetch for `site` is over**: stop the global spinner and clear that
    /// site's per-pane loading mark.
    ///
    /// The two always happen together and always in this order — a round that
    /// cleared one without the other left either a spinner nothing was feeding
    /// or a pane still marked loading after its scan had landed. Expressed once
    /// so a caller cannot do half of it, which also spends one reach across the
    /// app/UI seam where it used to spend two.
    pub fn finish_loading(&mut self, site: &str) {
        self.apply(crate::shell_api::GuiEvent::Fetching(false));
        self.clear_loading_site_for_site(site);
    }

    pub fn bump_all_radar_sites_gen(&mut self) {
        for pane in &mut self.panes {
            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
        }
    }

    pub fn safe_area_insets(&self) -> (f32, f32, f32, f32) {
        self.safe_area_insets
    }

    /// See [`FrameInputs::supports_exit`](crate::shell_api::FrameInputs::supports_exit).
    pub fn supports_exit(&self) -> bool {
        self.supports_exit
    }

    /// See [`FrameInputs::gps`](crate::shell_api::FrameInputs::gps).
    pub fn gps_fix(&self) -> Option<&squallar_location::Fix> {
        self.user_fix.as_ref()
    }

    /// See [`FrameInputs::location`](crate::shell_api::FrameInputs::location).
    pub fn location_permission(&self) -> squallar_location::LocationPermission {
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

    pub fn is_viewing_live(&self) -> bool {
        self.panes
            .get(self.active_pane)
            .is_some_and(|p| p.viewing_live)
    }

    pub fn is_any_pane_live(&self) -> bool {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .any(|p| p.viewing_live)
    }

    pub fn get_scan_info(&self) -> Option<&ScanInfo> {
        self.panes
            .get(self.active_pane)
            .and_then(|p| p.scan_info.as_ref())
    }

    pub fn get_scan_info_for_pane(&self, pane_idx: usize) -> Option<&ScanInfo> {
        self.panes.get(pane_idx).and_then(|p| p.scan_info.as_ref())
    }

    /// How long the event loop may sleep before some auto-poll timer next
    /// needs a **frame**, or `None` when nothing is polling and it may sleep
    /// until something happens.
    pub fn auto_poll_delay(&self) -> Option<std::time::Duration> {
        // Every layer through one term, radar included: its own gate is
        // inside `overlay_poll_delay`, which is where the difference between
        // "enabled somewhere" and "live somewhere" is written down.
        self.overlays
            .handlers()
            .filter_map(|h| self.overlay_poll_delay(&h.id()))
            .min()
    }

    /// How long until the status bar's own text would read differently, or
    /// `None` when nothing on screen is restating the clock.
    pub fn status_tick_delay(&self) -> Option<std::time::Duration> {
        self.status_bar_tick
    }

    /// Whether any pane **on screen** has a loop that is playing or has
    /// in-flight work.
    pub fn any_loop_active(&self) -> bool {
        self.panes
            .iter()
            .take(self.pane_layout.pane_count)
            .any(|p| {
                let ls = p.transport_state();
                ls.is_active()
                    && (ls.is_playing()
                        || ls.is_fetching()
                        || ls.frames.iter().any(|f| f.render_in_flight))
            })
    }

    /// Whether any pane is waiting on a raster's pixels to finish arriving.
    pub fn any_raster_held(&self) -> bool {
        self.panes.iter().any(PaneState::is_holding_raster)
    }

    /// Show every held raster whose pixels have all landed — the radar raster
    /// and every layer texture alike.
    pub fn promote_held_rasters(&mut self, delivered: impl Fn(egui::TextureId) -> bool) {
        for pane in &mut self.panes {
            pane.promote_held_raster(&delivered);
            pane.promote_held_overlays(&delivered);
        }
    }

    /// Let go of every raster still arriving, without showing any of them.
    pub fn release_held_rasters(&mut self) {
        for pane in &mut self.panes {
            pane.release_held_raster();
        }
    }

    pub fn clear_graphics_state(&mut self) {
        for pane in &mut self.panes {
            pane.loading_site = None;
            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            // **Every timeline the pane is animating, not radar's slot**
            // (WO-T3.5). A non-radar frame's picture is
            // `LoopFrameImage::Overlay`, holding a `TextureHandle` minted by
            // the device that is going away; reaching only the radar slot left
            // every other animating layer's frames pointing at handles the
            // dead device owned. The generic walk over `overlay_textures`
            // below has always been right about the live cache — this is the
            // same statement about the frames.
            for slot in pane.animating_layers_mut() {
                for frame in &mut slot.time.frames {
                    frame.image = None;
                    frame.render_in_flight = false;
                }
            }
            for cache in pane.overlay_textures.values_mut() {
                cache.clear();
                cache.renders.abandon_all();
            }
            // And whatever the pane's *kind* holds — today, a section pane's
            // raster. This is the only place a pane-held handle is released when
            // the egui context dies. Every arm deliberately keeps enough to put
            // its picture *back* — see `PaneContent::release_textures`.
            pane.content.release_textures();
        }
        self.map_tiles.clear();
        // The mirror texture died with the device: whatever the strip cache
        // committed describes pixels that no longer exist, so the next frame
        // repaints every strip whatever the keys say.
        self.floor_strips.force_repaint();
        // The painter holds wgpu handles made by the device that is going away,
        // and every one of them — pipelines, the offscreen targets, the uploaded
        // grid — is invalid the moment it does. Dropping the whole painter is
        // the release: the frontend installs a fresh one when the renderer comes
        // back, and until then every 3D pane says so.
        self.volume_painter = None;
        // Same rule, same reason: the tile-mesh store's buffers, pipeline and
        // bind groups all belong to the device that is going away, and the
        // ground falls back to CPU placement until a fresh painter arrives.
        self.tile_mesh_painter = None;
    }

    pub(crate) fn volume_painter(
        &self,
    ) -> Option<&std::sync::Arc<dyn crate::volume_view::VolumePainter>> {
        self.volume_painter.as_ref()
    }
}

#[cfg(test)]
mod chunk_scan_info_tests;

#[cfg(test)]
mod link_group_tests;

#[cfg(test)]
mod pane_slice_tests;

#[cfg(test)]
mod storm_motion_override_tests;

#[cfg(test)]
mod wake_schedule_tests;

/// The layers surfaced through another layer's inspector, and what happens to
/// one whose work the pane's own render has taken over.
#[cfg(test)]
mod surfaced_control_tests;

#[cfg(test)]
mod overlay_retry_tests;

#[cfg(test)]
mod overlay_texture_release_tests;

/// Which panes each scan-info event is addressed to.
#[cfg(test)]
mod scan_info_audience_tests;
