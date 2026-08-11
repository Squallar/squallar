//! The every-option parity walk: on every width class, every option the models
//! offer must be reachable and *drawn* through the real chrome.
//!
//! This is the migration's fixed star. The inventories are derived from the
//! models — [`Gui::menu_model_leaf_labels`], each
//! [`OVERLAY_CONTROL_ORDER`] handler's [`ControlItem`] tree,
//! [`SETTINGS_ROWS`] — never written out here, so a newly added option joins
//! the audit by construction and a dropped one fails it. The drawn half comes
//! from the probes the renderers write ([`DrawnControlItem`],
//! [`crate::ui::DrawnMenuLeaf`], [`crate::ui::DrawnSettingsRow`]); the walk
//! never reconstructs what "should" have been painted.
//!
//! Each width class is walked through the chrome a user of that width gets:
//! the top bar's Layers toggle opens the layer stack — sidebar on Expanded,
//! drawer elsewhere — each stack row opens its layer's options in the
//! inspector, the ☰ button opens the one menu dropdown every width shares,
//! its Settings… entry opens the inspector's App › Settings body, and the
//! stack's `+ Add layer` opens the catalog — the modal above 600 pt, the
//! sheet's Catalog page below it — whose every tile the walk scrolls on
//! screen. The 3D pane's hand-written rows are the one literal inventory —
//! see [`walk_volume_body`] for why they cannot be derived.
//! `ScrollArea`s lay content out beyond the viewport, so an item is only
//! counted once its probe's rect centre is inside the screen —
//! [`InputHarness::scroll_until`] does the scrolling a user would.
//!
//! A handler's heading and its master `enabled` toggle are excluded from the
//! control inventory through [`is_master_control`] — the renderer's own
//! predicate — because the inspector expresses them as the crumb and the
//! "Show <layer>" toggle; the walk asserts that toggle drew instead.
//!
//! Assertion failures name the missing label *and* the width class, because
//! "reachable on desktop" and "reachable on a phone" are separate claims and
//! the whole point here is which one broke.
//!
//! [`Gui::menu_model_leaf_labels`]: crate::Gui
//! [`ControlItem`]: rustdar_overlays::render::controls::ControlItem

use crate::input_harness::InputHarness;
use crate::ui::{
    CatalogGroup, DrawnControlItem, DrawnControlKind, OVERLAY_CONTROL_ORDER, SETTINGS_ROWS,
    builtin_presets, is_master_control,
};
use crate::ui_layout::WidthClass;
use rustdar_overlays::hrrr::ModelParameter;
use rustdar_overlays::render::controls::ControlItem;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::types::RadarProduct;

/// One wheel step of the walk's scrolling. Small enough that nothing can jump
/// clean across the shortest screen under test between two frames.
const SCROLL_STEP: egui::Vec2 = egui::vec2(0.0, -160.0);

/// How many scroll steps the walk spends looking for one item before calling
/// it unreachable. Generous: the drawer's full content is a few thousand
/// points tall at most.
const MAX_SCROLL_STEPS: usize = 120;

/// Where the walk points the wheel to scroll the inspector: inside its own
/// area, from egui's authority on where that is.
fn inspector_scroll_pos(h: &InputHarness) -> egui::Pos2 {
    h.inspector_rect()
        .expect("the inspector must be on screen to be scrolled")
        .center()
}

/// Whether an item's probe was recorded with its centre on screen — the walk's
/// definition of "drawn": laid out somewhere a user could actually see it.
fn control_on_screen(
    h: &InputHarness,
    handler: OverlayKind,
    kind: DrawnControlKind,
    label: &str,
) -> bool {
    h.control_items().iter().any(|item| {
        matches(item, handler, kind, label) && h.screen_rect().contains(item.rect.center())
    })
}

/// Whether the probe exists at all, on screen or off — how the walk tells "not
/// yet scrolled to" from "inside a collapsed section, not drawn at all".
fn control_recorded(
    h: &InputHarness,
    handler: OverlayKind,
    kind: DrawnControlKind,
    label: &str,
) -> bool {
    h.control_items()
        .iter()
        .any(|item| matches(item, handler, kind, label))
}

fn matches(
    item: &DrawnControlItem,
    handler: OverlayKind,
    kind: DrawnControlKind,
    label: &str,
) -> bool {
    item.handler == Some(handler) && item.kind == kind && item.label == label
}

/// Scroll the inspector until `label` is drawn on screen, and fail naming it
/// if it never is.
fn assert_control_reachable(
    h: &mut InputHarness,
    width: WidthClass,
    handler: OverlayKind,
    kind: DrawnControlKind,
    label: &str,
) {
    let pos = inspector_scroll_pos(h);
    let found = h.scroll_until(pos, SCROLL_STEP, MAX_SCROLL_STEPS, |h| {
        control_on_screen(h, handler, kind, label)
    });
    assert!(
        found,
        "{handler:?} control {label:?} ({kind:?}) was never drawn on screen \
         on {width:?} — the model offers it but the chrome never showed it"
    );
}

/// The drawn shape each model item must appear as. `None` for a separator,
/// which draws nothing nameable.
fn drawn_kind(item: &ControlItem) -> Option<DrawnControlKind> {
    Some(match item {
        ControlItem::Toggle { .. } => DrawnControlKind::Checkbox,
        ControlItem::Heading { .. } => DrawnControlKind::Heading,
        ControlItem::InfoText { .. } => DrawnControlKind::InfoText,
        ControlItem::Dropdown { .. } => DrawnControlKind::Dropdown,
        ControlItem::Slider { .. } => DrawnControlKind::Slider,
        ControlItem::Section { .. } => DrawnControlKind::Section,
        ControlItem::ButtonRow { .. } | ControlItem::Separator => return None,
    })
}

/// Walk one handler's item list, depth first, asserting each drawable item.
///
/// A collapsed section's children record no probe at all, so when a section
/// header is on screen but its first child was never recorded, the walk opens
/// it the way a user does — a click on the header — before descending.
fn assert_control_tree(
    h: &mut InputHarness,
    width: WidthClass,
    handler: OverlayKind,
    items: &[ControlItem],
) {
    for item in items.iter().filter(|item| !is_master_control(item)) {
        match item {
            ControlItem::ButtonRow { buttons } => {
                for button in buttons {
                    assert_control_reachable(
                        h,
                        width,
                        handler,
                        DrawnControlKind::Button,
                        &button.label,
                    );
                }
            }
            ControlItem::Section {
                label,
                items: children,
                ..
            } => {
                assert_control_reachable(h, width, handler, DrawnControlKind::Section, label);
                let first_child = children
                    .iter()
                    .find_map(|child| drawn_kind(child).map(|kind| (kind, control_label(child))));
                if let Some((kind, child_label)) = first_child
                    && !control_recorded(h, handler, kind, child_label)
                {
                    // Collapsed: open it as a user would and let the children
                    // record themselves.
                    let header = h
                        .control_items()
                        .into_iter()
                        .find(|drawn| matches(drawn, handler, DrawnControlKind::Section, label))
                        .expect("the section was just asserted drawn");
                    h.mouse_click(header.rect.center());
                    h.warm_up();
                }
                assert_control_tree(h, width, handler, children);
            }
            ControlItem::Separator => {}
            _ => {
                if let Some(kind) = drawn_kind(item) {
                    assert_control_reachable(h, width, handler, kind, control_label(item));
                }
            }
        }
    }
}

/// The label a non-container item is asserted under.
fn control_label(item: &ControlItem) -> &str {
    match item {
        ControlItem::Toggle { label, .. }
        | ControlItem::Dropdown { label, .. }
        | ControlItem::Slider { label, .. }
        | ControlItem::Section { label, .. } => label,
        ControlItem::Heading { text } | ControlItem::InfoText { text } => text,
        ControlItem::ButtonRow { .. } | ControlItem::Separator => "",
    }
}

/// The representative handler the walk runs with its layer **hidden**: the
/// richest gated-history tree (dropdown, slider, level toggles, refresh),
/// so its leg proves reachability does not depend on visibility — the eye
/// hides pixels, never options (the M9.1 user report). Explicitly disabled
/// rather than trusting the handler's default, so a default flip cannot
/// silently retire the coverage; the overlays crate's controls-parity test
/// pins the model half of the same rule for all twelve kinds.
const HIDDEN_WALK_HANDLER: OverlayKind = OverlayKind::Lightning;

/// Every handler's every control, reachable through its stack row and the
/// inspector's layer body.
fn walk_layer_controls(h: &mut InputHarness, width: WidthClass) {
    h.set_overlay_on_pane(0, HIDDEN_WALK_HANDLER, false);
    for &handler in OVERLAY_CONTROL_ORDER {
        if handler == HIDDEN_WALK_HANDLER {
            assert!(
                !h.overlay_enabled_on(0, handler),
                "precondition: {handler:?} walks its leg hidden, and nothing \
                 on the way to its row may have re-enabled it"
            );
        }
        let model = h.control_item_model(handler);
        assert!(
            !model.is_empty(),
            "{handler:?} offers no controls at all on {width:?} — the \
             inventory itself is broken, not the chrome"
        );
        // The user's route: the stack row. Handles the drawer, the scroll to
        // the row, and asserts the layer body's own arm drew. The master
        // controls the tree filter excludes render as the crumb and the
        // stack row's 👁 eye — the layer's one on/off since the Show
        // toggle's de-dup (contract 86) — and the helper asserts the eye on
        // the very row it clicks, so excluding them from the inventory
        // cannot quietly orphan a layer's on/off.
        h.open_layer_in_inspector(handler);
        assert_control_tree(h, width, handler, &model);
    }
    // Leave nothing open over the next phase's clicks.
    h.close_inspector();
}

/// Every menu leaf, drawn inside the viewport by the one ☰ dropdown.
fn walk_menu(h: &mut InputHarness, width: WidthClass) {
    let labels = h.menu_leaf_labels();
    let groups = h.menu_groups();
    let grouped: Vec<&'static str> = groups
        .iter()
        .flat_map(|(_, leaves)| leaves.iter().copied())
        .collect();
    assert_eq!(
        grouped, labels,
        "a menu entry sits outside every submenu on {width:?}; the popup \
         renders it, but the model's own grouping has stopped covering it"
    );

    h.open_menu();
    for label in labels {
        let visible = |h: &InputHarness| {
            h.menu_leaf(label)
                .is_some_and(|leaf| h.screen_rect().contains(leaf.rect.center()))
        };
        // The sheet's Menu page scrolls where the dropdown does not — work
        // the list like a user before calling a leaf unreachable.
        if !visible(h) && width == WidthClass::Compact {
            let pos = h
                .sheet_rect()
                .expect("the Menu page is open, so the sheet has a rect")
                .center();
            h.scroll_until(pos, SCROLL_STEP, MAX_SCROLL_STEPS, visible);
        }
        let leaf = h
            .menu_leaf(label)
            .unwrap_or_else(|| panic!("menu leaf {label:?} was never drawn on {width:?}"));
        assert!(
            h.screen_rect().contains(leaf.rect.center()),
            "menu leaf {label:?} was drawn at {:?}, outside the {width:?} \
             viewport {:?}",
            leaf.rect,
            h.screen_rect()
        );
    }
    // Closed again so the next phase's clicks cannot land on the popup.
    h.close_menu();
}

/// The Set Time dialog, reachable through the menu, with both fields drawn.
fn walk_time_dialog(h: &mut InputHarness, width: WidthClass) {
    h.open_menu();
    let leaf = h
        .menu_leaf("Time...")
        .unwrap_or_else(|| panic!("the menu did not draw Time... on {width:?}"));
    h.mouse_click(leaf.rect.center());
    h.warm_up();

    let screen = h.screen_rect();
    for needle in ["Select Time", "Date:", "Time:"] {
        assert!(
            h.text_painted_in(screen, needle),
            "the time dialog never painted {needle:?} on {width:?}"
        );
    }

    // Close it the user's way, so the next phase's menu clicks cannot land on
    // the dialog instead.
    let cancel = h
        .painted_text_rects()
        .into_iter()
        .find(|(_, text)| text == "Cancel")
        .unwrap_or_else(|| panic!("the time dialog has no Cancel on {width:?}"))
        .0;
    h.mouse_click(cancel.center());
    h.warm_up();
    assert!(
        !h.text_painted_in(screen, "Select Time"),
        "Cancel did not close the time dialog on {width:?}"
    );
}

/// Every settings row, reachable through the menu's Settings... entry — the
/// inspector's App › Settings body, whose scroll the walk works like a user.
fn walk_settings(h: &mut InputHarness, width: WidthClass) {
    h.open_settings();
    for &row in SETTINGS_ROWS {
        if !cfg!(feature = "gps-serial") && row.starts_with("gps.") {
            // The table lists the rows unconditionally; this build compiled
            // the widgets out, so there is nothing to have drawn.
            continue;
        }
        let pos = inspector_scroll_pos(h);
        let found = h.scroll_until(pos, SCROLL_STEP, MAX_SCROLL_STEPS, |h| {
            h.settings_row(row)
                .is_some_and(|drawn| h.screen_rect().contains(drawn.rect.center()))
        });
        assert!(
            found,
            "settings row {row:?} was never drawn on screen on {width:?}"
        );
    }
}

/// Every catalog entry, drawn and reachable through each shell's own route —
/// the modal above 600 pt, the sheet's Catalog page below it (M7's leg).
///
/// The inventories are the models the renderer itself draws from: the
/// compiled-in presets table, `OverlayKind::all()` under the registry's
/// display names, `RadarProduct::all()` and `ModelParameter::all()` — never a
/// restated name list, so a new product or parameter joins the audit by
/// construction. User presets are deliberately absent: a fresh session has
/// none, and their tiles share the built-ins' code path. The user-preset
/// tiles and the Save tile are instead covered by the dedicated preset suite
/// (`a_saved_preset_appears_applies_and_deletes` and its neighbours), once
/// rather than per width — they render inside the same host this walk just
/// scrolled end to end, so a second per-width pass would re-walk the same
/// code for no new claim.
fn walk_catalog(h: &mut InputHarness, width: WidthClass) {
    let mut inventory: Vec<(CatalogGroup, String)> = Vec::new();
    for preset in builtin_presets() {
        inventory.push((CatalogGroup::Presets, preset.name));
    }
    for &kind in OverlayKind::all() {
        inventory.push((
            CatalogGroup::Overlays,
            h.overlay_display_name(kind).to_owned(),
        ));
    }
    for product in RadarProduct::all() {
        inventory.push((CatalogGroup::Products, product.name().to_owned()));
    }
    for param in ModelParameter::all() {
        inventory.push((CatalogGroup::Hrrr, param.display_name().to_owned()));
    }

    h.open_catalog();
    // Where the wheel points: the sheet on the phone, the modal elsewhere —
    // each host's own scrolling surface.
    let scroll_pos = if width == WidthClass::Compact {
        h.sheet_rect()
            .expect("the Catalog page is open, so the sheet has a rect")
            .center()
    } else {
        h.catalog().rect.center()
    };
    for (group, label) in inventory {
        let found = h.scroll_until(scroll_pos, SCROLL_STEP, MAX_SCROLL_STEPS, |h| {
            h.catalog_tile(group, &label)
                .is_some_and(|tile| h.screen_rect().contains(tile.rect.center()))
        });
        assert!(
            found,
            "catalog tile {label:?} ({group:?}) was never drawn on screen on \
             {width:?} — the model offers it but the catalog never showed it"
        );
    }
    // Closed again so the next phase's clicks cannot land on the host.
    assert!(
        h.gui_mut().dismiss_top_layer(),
        "the catalog was open, so a back press must close it"
    );
    h.warm_up();
}

/// The 3D pane's own rows in the Pane-properties body, reachable at every
/// width through that width's own route to the body.
///
/// Unlike the layer legs, this inventory is a literal list: the volume rows
/// are hand-written in `render_volume_controls` rather than offered by a
/// model, so there is nothing to derive them from. The restatement is the
/// point — the sidebar-order contract
/// (`kind_specific_blocks_sit_inside_the_shared_sidebar_structure`) pins
/// these rows on one Expanded screen only, and the block is the body's
/// *last*: on the Compact sheet its tail (the Map floor checkbox, the Reset
/// view button) lays out below the fold, where only the scroll this leg
/// performs proves a phone user can reach it at all. A row added to the
/// volume body joins this list by review rather than by construction; a row
/// dropped from the body fails it by name, on the width that lost it.
fn walk_volume_body(h: &mut InputHarness, width: WidthClass) {
    h.make_pane_volume(0);
    if width == WidthClass::Compact {
        // The phone's route: the bottom bar's Pane item hosts the body as
        // the sheet's Inspector page.
        let item = h.bottom_bar().pane.0;
        h.mouse_click(item.center());
        h.warm_up();
    } else {
        h.open_pane_props();
    }
    let scroll_pos = if width == WidthClass::Compact {
        h.sheet_rect()
            .expect("the Inspector page is open, so the sheet has a rect")
            .center()
    } else {
        inspector_scroll_pos(h)
    };
    for needle in ["Vertical:", "Mode:", "Map floor", "Reset view"] {
        let found = h.scroll_until(scroll_pos, SCROLL_STEP, MAX_SCROLL_STEPS, |h| {
            let host = if width == WidthClass::Compact {
                h.sheet_rect()
                    .expect("the Inspector page stays open through the scroll")
            } else {
                h.inspector_rect()
                    .expect("the inspector stays open through the scroll")
            };
            h.text_painted_in(host, needle)
        });
        assert!(
            found,
            "volume row {needle:?} was never drawn on screen on {width:?} — \
             the 3D pane offers it but its body never showed it"
        );
    }
    h.close_inspector();
}

/// Every visible pane carries its pill row (M5): presence, at every width.
/// Deliberately not a per-option leg — every option behind the pills is the
/// inspector's own inventory through the shared pickers (`ui_pills.rs`'s
/// module note), so the walk's existing inspector legs already audit them;
/// what only the pills can lose is the rows themselves.
fn walk_pills(h: &mut InputHarness, width: WidthClass) {
    let panes = h.pane_rects();
    assert!(!panes.is_empty(), "a layout always has a pane on {width:?}");
    for (idx, pane) in panes.iter().enumerate() {
        let row = h
            .pill_row(idx)
            .unwrap_or_else(|| panic!("pane {idx} drew no pill row on {width:?}"));
        assert!(
            pane.contains(row.rect.min),
            "pane {idx}'s pill row sits outside its pane on {width:?}: \
             row {:?}, pane {pane:?}",
            row.rect
        );
        assert!(
            !row.pills.is_empty(),
            "pane {idx}'s pill row drew no pills on {width:?}"
        );
    }
}

/// The whole walk for one screen: layer controls through the layers panel,
/// then the menu, the time dialog and the settings window through the ☰
/// dropdown — the same routes at every width.
fn walk_every_option(size: egui::Vec2, expect: WidthClass) {
    let mut h = InputHarness::with_screen(size);
    assert_eq!(
        h.width_class(),
        expect,
        "precondition: a {size:?} screen must land in {expect:?}"
    );

    walk_layer_controls(&mut h, expect);
    walk_menu(&mut h, expect);
    walk_time_dialog(&mut h, expect);
    walk_settings(&mut h, expect);
    walk_catalog(&mut h, expect);
    walk_pills(&mut h, expect);
    // Last, because it converts pane 0 to a 3D pane and the legs above are
    // claims about a map layout.
    walk_volume_body(&mut h, expect);
}

#[test]
fn every_option_is_reachable_on_a_compact_screen() {
    walk_every_option(egui::vec2(420.0, 1400.0), WidthClass::Compact);
}

#[test]
fn every_option_is_reachable_on_a_medium_screen() {
    walk_every_option(egui::vec2(800.0, 1200.0), WidthClass::Medium);
}

#[test]
fn every_option_is_reachable_on_an_expanded_screen() {
    walk_every_option(egui::vec2(1400.0, 900.0), WidthClass::Expanded);
}
