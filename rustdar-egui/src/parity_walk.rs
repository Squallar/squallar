//! The every-option parity walk: on every width class, every option the models
//! offer must be reachable and *drawn* through the real chrome.

use crate::input_harness::InputHarness;
use crate::ui::{
    CatalogGroup, DrawnControlItem, DrawnControlKind, SETTINGS_ROWS, builtin_presets,
    is_master_control,
};
use crate::ui_layout::WidthClass;
use rustdar_overlays::render::controls::ControlItem;
use rustdar_source::id::{LayerId, known};

/// One wheel step of the walk's scrolling. Small enough that nothing can jump
/// clean across the shortest screen under test between two frames.
const SCROLL_STEP: egui::Vec2 = egui::vec2(0.0, -160.0);

/// How many scroll steps the walk spends looking for one item before calling
/// it unreachable.
const MAX_SCROLL_STEPS: usize = 120;

fn inspector_scroll_pos(h: &InputHarness) -> egui::Pos2 {
    h.inspector_rect()
        .expect("the inspector must be on screen to be scrolled")
        .center()
}

/// Whether an item's probe was recorded with its centre on screen — the walk's
/// definition of "drawn": laid out somewhere a user could actually see it.
fn control_on_screen(
    h: &InputHarness,
    handler: &LayerId,
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
    handler: &LayerId,
    kind: DrawnControlKind,
    label: &str,
) -> bool {
    h.control_items()
        .iter()
        .any(|item| matches(item, handler, kind, label))
}

fn matches(
    item: &DrawnControlItem,
    handler: &LayerId,
    kind: DrawnControlKind,
    label: &str,
) -> bool {
    item.handler.as_ref() == Some(handler) && item.kind == kind && item.label == label
}

/// Scroll the inspector until `label` is drawn on screen, and fail naming it
/// if it never is.
fn assert_control_reachable(
    h: &mut InputHarness,
    width: WidthClass,
    handler: &LayerId,
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

fn drawn_kind(item: &ControlItem) -> Option<DrawnControlKind> {
    Some(match item {
        ControlItem::Toggle { .. } => DrawnControlKind::Checkbox,
        ControlItem::Heading { .. } => DrawnControlKind::Heading,
        ControlItem::InfoText { .. } => DrawnControlKind::InfoText,
        ControlItem::Dropdown { .. } => DrawnControlKind::Dropdown,
        ControlItem::Slider { .. } => DrawnControlKind::Slider,
        ControlItem::Section { .. } => DrawnControlKind::Section,
        ControlItem::TextField { .. } => DrawnControlKind::TextField,
        ControlItem::ButtonRow { .. } | ControlItem::Separator => return None,
    })
}

/// Walk one handler's item list, depth first, asserting each drawable item.
fn assert_control_tree(
    h: &mut InputHarness,
    width: WidthClass,
    handler: &LayerId,
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

fn control_label(item: &ControlItem) -> &str {
    match item {
        ControlItem::Toggle { label, .. }
        | ControlItem::Dropdown { label, .. }
        | ControlItem::Slider { label, .. }
        | ControlItem::Section { label, .. }
        | ControlItem::TextField { label, .. } => label,
        ControlItem::Heading { text } | ControlItem::InfoText { text } => text,
        ControlItem::ButtonRow { .. } | ControlItem::Separator => "",
    }
}

/// The representative handler the walk runs with its layer **hidden**: the
/// richest gated-history tree (dropdown, slider, level toggles, refresh),
/// so its leg proves reachability does not depend on visibility. Explicitly
/// disabled rather than trusting the handler's default, so a default flip
/// cannot silently retire the coverage.
const HIDDEN_WALK_HANDLER: LayerId = known::LIGHTNING;

/// Every handler's every control, reachable through its stack row and the
/// inspector's layer body.
fn walk_layer_controls(h: &mut InputHarness, width: WidthClass) {
    // **The LIVE registry, in registry order.** There is no second list a
    // handler can be dropped from: a layer that registers is a layer this walk
    // covers, which is what makes "registered but never audited" unspellable.
    let registered: Vec<LayerId> = h.gui().overlays.handlers().map(|h| h.id()).collect();
    // **The floor on THIS leg's own list**, and it is not decoration: a walk is
    // a PARITY, so a handler missing from the list it iterates leaves nothing
    // to look for and nothing to fail on. Filtering one id out of this vector
    // was tampered and came back GREEN before this assertion existed. The
    // cross-check is the hand-kept `REGISTERED_LAYER_COUNT`, never a second
    // read of the registry.
    assert_eq!(
        registered.len(),
        crate::sources::REGISTERED_LAYER_COUNT,
        "the control walk is about to cover {} handlers on {width:?}, but this \
         build registers {} — every handler it does not iterate is a handler \
         whose every option goes unaudited, in silence: {registered:?}",
        registered.len(),
        crate::sources::REGISTERED_LAYER_COUNT,
    );
    assert!(
        registered.contains(&HIDDEN_WALK_HANDLER),
        "the hidden-leg handler {HIDDEN_WALK_HANDLER:?} is not registered in \
         this build — a rename would otherwise retire that leg in silence, \
         leaving every handler walked shown and none walked hidden",
    );
    // **Two steps, since the stack became a curated list.** The hidden leg
    // needs a layer that is IN the pane's stack and switched OFF; the handler
    // it uses ships disabled, so a fresh pane does not hold it at all and
    // "switch it off" is now a no-op on a layer with no row. Adding it first is
    // what makes the off meaningful — and it has to be the pane API rather than
    // `open_layer_in_inspector`'s catalogue route, because that route turns the
    // layer ON, which is the one thing this leg must not be.
    h.add_layer_to_pane(0, &HIDDEN_WALK_HANDLER);
    h.set_overlay_on_pane(0, &HIDDEN_WALK_HANDLER, false);
    for handler in &registered {
        if *handler == HIDDEN_WALK_HANDLER {
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
        // controls the tree filter excludes render as the crumb and the stack
        // row's 👁 eye, and the helper asserts the eye on the very row it clicks.
        h.open_layer_in_inspector(handler);
        assert_control_tree(h, width, handler, &model);
    }
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
fn walk_catalog(h: &mut InputHarness, width: WidthClass) {
    let mut inventory: Vec<(CatalogGroup, String)> = Vec::new();
    for preset in builtin_presets() {
        inventory.push((CatalogGroup::Presets, preset.name));
    }
    for kind in crate::sources::default_draw_order() {
        inventory.push((
            CatalogGroup::Layers,
            h.overlay_display_name(&kind).to_owned(),
        ));
    }
    // **The fields, derived from the live registry** — the same list the
    // catalogue renders from, so a source that registers a new group of fields
    // is walked without an edit here. The floor below is what keeps that from
    // being a walk of the registry against itself.
    for (_, spec) in h.gui().overlays.fields() {
        inventory.push((CatalogGroup::Fields(spec.group), spec.name.to_owned()));
    }

    // **The anti-shrink floor**, on the one inventory leg that is *derived*.
    // `default_draw_order` reads the composed registry, so a composition that
    // quietly lost a source crate would hand this walk a shorter list and the
    // walk would check that many fewer tiles and still pass.
    //
    // The cross-check is `sources::REGISTERED_LAYER_COUNT`, a **hand-kept
    // literal** — deliberately not a second read of the registry, which would
    // compare the registry against itself and could not fail. See that
    // constant's own doc; it is the reason this assertion is worth writing.
    let layers_in_inventory = inventory
        .iter()
        .filter(|(group, _)| matches!(group, CatalogGroup::Layers))
        .count();
    assert_eq!(
        layers_in_inventory,
        crate::sources::REGISTERED_LAYER_COUNT,
        "the catalog leg's layer inventory is {layers_in_inventory} on \
         {width:?} but this build registers {} layers — the walk would check \
         that many fewer tiles and still pass",
        crate::sources::REGISTERED_LAYER_COUNT,
    );

    // **The field floor, and it is new at WO-E9d land 2 because the exposure is
    // new.** Before this land the field legs were enumerated from
    // the two source enums' own `all()` lists — enums in other crates, and
    // therefore already an independent second spelling of what the
    // catalogue drew. Deriving the inventory from `fields()` removed that
    // independence in exactly the way ruling (30) described for layers: the
    // walk and the thing it walks would read one list, and a registry that
    // quietly dropped a field would be met by a catalogue that quietly dropped
    // the same tile. `REGISTERED_FIELD_COUNT` is the hand-kept literal that
    // restores it.
    let fields_in_inventory = inventory
        .iter()
        .filter(|(group, _)| matches!(group, CatalogGroup::Fields(_)))
        .count();
    assert_eq!(
        fields_in_inventory,
        crate::sources::REGISTERED_FIELD_COUNT,
        "the catalog leg's field inventory is {fields_in_inventory} on \
         {width:?} but this build registers {} fields — the walk would check \
         that many fewer tiles and still pass",
        crate::sources::REGISTERED_FIELD_COUNT,
    );

    h.open_catalog();
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
    assert!(
        h.gui_mut().dismiss_top_layer(),
        "the catalog was open, so a back press must close it"
    );
    h.warm_up();
}

/// The 3D pane's own rows in the Pane-properties body, reachable at every
/// width through that width's own route to the body.
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
/// inspector's own inventory through the shared pickers, already audited;
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

/// The Pane-properties sync section (M11): on a split layout, the five
/// per-pane rows — three links, two actions — reachable through the
/// inspector route at every width, against the one inventory
/// `pills::sync_section_ui` renders ([`crate::ui::SYNC_SECTION_LABELS`]).
/// What the walk owes is that the rows are *on screen* per width.
fn walk_sync_section(h: &mut InputHarness, width: WidthClass) {
    h.set_pane_count(2);
    if width == WidthClass::Compact {
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
    for needle in crate::ui::SYNC_SECTION_LABELS {
        let found = h.scroll_until(scroll_pos, SCROLL_STEP, MAX_SCROLL_STEPS, |h| {
            h.inspector()
                .sync_rows
                .iter()
                .any(|(label, rect, _)| label == needle && h.screen_rect().contains(rect.center()))
        });
        assert!(
            found,
            "sync row {needle:?} was never drawn on screen on {width:?} — \
             the section offers it but the inspector route never showed it"
        );
    }
    h.close_inspector();
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
    // Splits the layout to two panes — the sync section only exists with a
    // group to sync — so it runs after the single-pane legs above.
    walk_sync_section(&mut h, expect);
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
