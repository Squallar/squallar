//! **Layer-stack curation, end to end through the chrome.**
//!
//! The type-level rule lives in `pane/layer_stack/tests.rs`; this suite is the
//! user's half — the trash can, the catalog's add, and the one property the
//! whole design turns on: **a layer the user removed does not come back.**
//!
//! Its own file rather than a wing of `input_harness/tests.rs`, which is
//! thirteen thousand lines and is where a suite goes to be unfindable.

use squallar_kv::KvStore;
use squallar_source::id::{LayerId, known};

use crate::Gui;
use crate::input_harness::InputHarness;

/// A layer that ships **enabled**, so a fresh pane really holds it and the
/// reconcile really wants to put it back — which is what makes the
/// removal-persists test below able to fail. A default-off layer would stay
/// absent for a reason that has nothing to do with the tombstone.
const REMOVABLE: LayerId = known::CITY_LABELS;

fn store_with(json: &str) -> squallar_kv::MemoryKvStore {
    let store = squallar_kv::MemoryKvStore::default();
    store
        .store(crate::UI_CONFIG_KEY, json)
        .expect("the memory store accepts a write");
    store
}

/// The stack a fresh pane comes up with.
fn fresh_stack(h: &InputHarness) -> Vec<LayerId> {
    h.stack().rows.iter().map(|row| row.kind.clone()).collect()
}

/// **The trash can removes the layer from the pane, and does not merely hide
/// it.**
///
/// The distinction is the whole feature: hiding is what the eye beside it
/// already did.
#[test]
fn the_trash_can_takes_the_layer_out_of_the_stack_rather_than_hiding_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_layers();
    let row = h.stack_row(&REMOVABLE).expect("a fresh pane holds it");
    assert!(row.eye_on, "precondition: and holds it shown");
    assert!(
        row.remove_enabled,
        "precondition: an ordinary layer's can is live",
    );

    h.mouse_click(row.remove.center());
    h.warm_up();

    assert!(
        h.stack_row(&REMOVABLE).is_none(),
        "the row is still drawn: the can hid the layer instead of removing it",
    );
    assert!(
        !fresh_stack(&h).contains(&REMOVABLE),
        "the layer is still in the pane's draw order",
    );
    assert!(
        !h.overlay_enabled_on(0, &REMOVABLE),
        "a removed layer must answer the draw gate with no, structurally",
    );
}

/// **The removal survives a save and a reload** — and it survives the very
/// reconcile that used to re-complete the stack, which is the thing that made
/// removal impossible before.
///
/// This is the item's own regressor. Deleting the `removed_layers` half of
/// either the save or the load path, or restoring
/// `insert_missing_slots`'s old unconditional fill, turns it red.
#[test]
fn a_removed_layer_stays_removed_across_a_reload() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_layers();
    let before = fresh_stack(&h);
    assert!(
        before.contains(&REMOVABLE),
        "precondition: the pane starts holding it, so the reload below has \
         something to hand back",
    );

    let row = h.stack_row(&REMOVABLE).expect("the row is drawn");
    h.mouse_click(row.remove.center());
    h.warm_up();

    let saved = h.gui_mut().ui_config_json().expect("the config serializes");
    assert!(
        saved.contains("removed_layers"),
        "the save wrote no tombstone, so the reload can only be guessing",
    );

    let mut reopened = Gui::new();
    assert!(
        reopened.load_ui_config(&store_with(&saved)),
        "the saved config must reload",
    );
    let after: Vec<LayerId> = reopened.pane(0).expect("pane 0").draw_order_vec();
    assert!(
        !after.contains(&REMOVABLE),
        "the removed layer came back on reopen - reopen is not 1:1 for a user \
         who curated: {after:?}",
    );
    // The non-triviality floor: the rest of the stack came back, so "absent"
    // is a statement about this layer and not about a load that dropped
    // everything.
    for id in before.iter().filter(|id| **id != REMOVABLE) {
        assert!(
            after.contains(id),
            "{id:?} was lost on reopen too - the reload dropped the stack \
             rather than honouring one removal",
        );
    }
}

/// **The re-add restores what the layer held**, so an accidental removal costs
/// a click rather than a re-configuration.
///
/// The setting moved is found through the **handler's own declared controls**
/// rather than by naming a field: any layer offering a slider is a layer whose
/// settings this proves, and no arm here knows which one it got.
#[test]
fn re_adding_a_removed_layer_restores_its_saved_settings() {
    use squallar_source::controls::{ControlItem, ControlUpdate, ControlValue};

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_layers();
    // Ships disabled, so this leg also proves the tombstone carries settings
    // for a layer the pane only holds because the user asked for it.
    let subject = known::LIGHTNING;
    h.add_layer_from_catalog(&subject);

    // Move one of the layer's own settings off its current value, through the
    // same door the inspector's control uses.
    let (id, moved) = h
        .gui()
        .control_item_model_for_test(&subject)
        .iter()
        .find_map(|item| match item {
            ControlItem::Slider {
                id,
                min,
                max,
                value,
                ..
            } => Some((
                *id,
                if *value > (min + max) / 2.0 {
                    *min
                } else {
                    *max
                },
            )),
            _ => None,
        })
        .expect("the subject layer declares a slider to move");
    h.gui_mut().apply_control_on_pane_for_test(
        0,
        &subject,
        &ControlUpdate {
            id,
            value: ControlValue::Float(moved),
        },
    );
    let reading = |h: &InputHarness| -> Option<f64> {
        h.gui()
            .control_item_model_for_test(&subject)
            .iter()
            .find_map(|item| match item {
                ControlItem::Slider { id: got, value, .. } if *got == id => Some(*value),
                _ => None,
            })
    };
    assert_eq!(
        reading(&h),
        Some(moved),
        "precondition: the edit must have landed, or \"restored\" below is a \
         statement about a default",
    );

    let row = h.stack_row(&subject).expect("the row is drawn");
    h.mouse_click(row.remove.center());
    h.warm_up();
    assert!(h.stack_row(&subject).is_none(), "precondition: it left");

    h.add_layer_from_catalog(&subject);

    assert_eq!(
        reading(&h),
        Some(moved),
        "the re-add reset {subject:?} instead of restoring what it left with",
    );
}

/// **A layer the pane structurally owns has a visible, explained refusal —
/// never an absent control and never a live one that does nothing.**
#[test]
fn the_radar_layers_can_is_drawn_disabled_with_a_reason() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_layers();
    let row = h.stack_row(&known::RADAR).expect("a map pane holds radar");
    assert_ne!(
        row.remove,
        egui::Rect::NOTHING,
        "the radar row drew no remove control at all - an absent affordance \
         reads as an oversight",
    );
    assert!(
        !row.remove_enabled,
        "the radar row's can is live: clicking it would either do nothing or \
         delete the pane's own site, product and tilt",
    );

    h.mouse_click(row.remove.center());
    h.warm_up();
    assert!(
        h.stack_row(&known::RADAR).is_some(),
        "the disabled can removed radar anyway",
    );
    assert!(
        h.gui_mut()
            .pane(0)
            .expect("pane 0")
            .layer_removal_refusal(&known::RADAR)
            .is_some_and(|reason| !reason.is_empty()),
        "the refusal carries no sentence to show the user",
    );
}

/// **Every registered layer reaches the catalog, curation or not.**
///
/// The other half of the new-source rule: whether a layer joins a pane's stack
/// is a product decision (`default_enabled`), but *appearing in the catalog* is
/// unconditional — that is what "adding a source is one crate's work" rests on,
/// and a curated stack must not be able to hide a layer from the one surface
/// that lists what the build can draw.
#[test]
fn every_registered_layer_is_offered_by_the_catalog_even_after_a_removal() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let registered: Vec<LayerId> = h.gui_mut().overlays.handlers().map(|h| h.id()).collect();
    assert!(
        registered.len() >= 10,
        "non-triviality floor: this build registers {} layers",
        registered.len(),
    );

    // Curate the pane down as far as it will go, so the catalog is being asked
    // about layers the active pane does not hold.
    for id in &registered {
        h.gui_mut().pane_mut(0).expect("pane 0").remove_layer(id);
    }
    h.warm_up();

    h.open_catalog();
    for id in &registered {
        let name = h.overlay_display_name(id).to_owned();
        assert!(
            h.catalog_tile(crate::ui::CatalogGroup::Layers, &name)
                .is_some(),
            "{id:?} ({name:?}) is registered but the catalog offers no tile \
             for it - a curated pane can hide a layer from the inventory",
        );
    }
}

/// **The catalog's tile really inserts a row**, at the layer's own draw-order
/// weight rather than on top of the stack.
#[test]
fn a_catalog_tile_adds_a_row_at_the_layers_own_draw_order_weight() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_layers();
    // Ships disabled, so a curated stack has never held it: the tile has
    // something to add rather than something to switch on.
    let added = known::LIGHTNING;
    assert!(
        h.stack_row(&added).is_none(),
        "precondition: a fresh pane does not hold a layer that ships off - \
         without this the tile below is only a visibility toggle and \"add\" \
         is unproven",
    );

    h.add_layer_from_catalog(&added);

    let order = h.gui_mut().pane(0).expect("pane 0").draw_order_vec();
    let at = order
        .iter()
        .position(|id| *id == added)
        .expect("the tile added it");
    assert!(
        at > 0 && at < order.len() - 1,
        "the added layer landed at an end of the stack ({at} of {}); the \
         weight-ordered insert put it nowhere in particular: {order:?}",
        order.len(),
    );
    assert!(
        h.overlay_enabled_on(0, &added),
        "a layer added from the catalog must be shown - adding something \
         invisible is a click that did nothing",
    );
}

/// **A fresh pane's stack is the layers that ship enabled, and it is shorter
/// than the build's catalogue.**
///
/// The panel-length claim, stated as a property rather than as a count: this is
/// what stops the stack from growing a row every time a source crate is added.
#[test]
fn a_fresh_panes_stack_is_shorter_than_the_registry() {
    let h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let registered = h.gui().overlays.handlers().count();
    let held = h.gui().pane(0).expect("pane 0").draw_order_vec();
    assert!(
        held.len() < registered,
        "a fresh pane holds all {registered} registered layers - the stack is \
         still a projection of the build's catalogue: {held:?}",
    );
    assert!(
        !held.is_empty(),
        "a fresh pane holds nothing at all, which is not curation either",
    );
    for id in &held {
        assert!(
            h.gui()
                .overlays
                .handler_by_id(id)
                .is_some_and(|handler| handler.default_enabled()),
            "{id:?} is in a fresh pane's stack but does not ship enabled",
        );
    }
}

/// **An existing config loads to exactly the stack it names** — the
/// compatibility claim. A user who has never curated must see what they saw
/// before, so a file that lists every layer comes back with every layer, and
/// the new key's absence reads as "nothing removed" rather than as
/// "everything removed".
#[test]
fn a_config_written_before_curation_loads_to_the_stack_it_names() {
    let mut before = Gui::new();
    // Every registered layer in the pane, which is what every file written by
    // a build before curation carries.
    let every: Vec<LayerId> = before.overlays.handlers().map(|h| h.id()).collect();
    for id in &every {
        before.add_layer_on_pane_for_test(0, id);
    }
    let json = before.ui_config_json().expect("serializes");
    assert!(
        !json.contains("removed_layers"),
        "a pane that removed nothing must write no tombstone key at all, or \
         every existing config file changes shape on first save",
    );

    let mut after = Gui::new();
    assert!(after.load_ui_config(&store_with(&json)), "it must reload");
    let held = after.pane(0).expect("pane 0").draw_order_vec();
    for id in &every {
        assert!(
            held.contains(id),
            "{id:?} was in the file's stack and is not in the loaded one: a \
             build that never curated lost rows on upgrade",
        );
    }
}

/// **A config written before the Terrain layer existed still serves it** —
/// additively, with no migration and no CONFIG_VERSION bump. Terrain ships
/// OFF, so for a default-off layer "served" means what the tree means by it:
/// the old user's stack is untouched (nothing new is drawn on their map), the
/// catalog offers the layer, adding it lands at its registry position — the
/// bottom, weight 2 — and the addition reopens 1:1. The fixture JSON is the
/// pre-Terrain shape (`root_site_v4.json`'s pane, abbreviated).
#[test]
fn terrain_appears_for_a_config_written_before_it_existed() {
    let json = r#"{
        "config_version": 4,
        "pane_count": 1,
        "active_pane": 0,
        "site": "KTLX",
        "panes": [{
            "layer_slots": [
                {"id": "Radar", "enabled": true},
                {"id": "NwsAlerts", "enabled": true}
            ]
        }]
    }"#;

    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store_with(json)),
        "the pre-Terrain config must load",
    );
    let order = gui.pane(0).expect("pane 0").draw_order_vec();
    assert!(
        !order.contains(&known::TERRAIN),
        "Terrain ships OFF, so an old config must not sprout a stack row \
         nobody asked for: {order:?}",
    );
    assert!(
        order.contains(&known::RADAR) && order.contains(&known::NWS_ALERTS),
        "non-triviality: the file's own slots came through",
    );

    // The catalog's add — the door an old user reaches the new layer through.
    assert!(
        gui.add_layer_on_pane_for_test(0, &known::TERRAIN),
        "a config that never heard of Terrain must not block adding it",
    );
    let order = gui.pane(0).expect("pane 0").draw_order_vec();
    assert_eq!(
        order.first(),
        Some(&known::TERRAIN),
        "Terrain's weight (2) puts it under everything the file named: \
         {order:?}",
    );
    assert!(
        gui.pane(0)
            .expect("pane 0")
            .is_overlay_enabled(&known::TERRAIN),
        "a layer added from the catalog is shown",
    );

    // Reopen is 1:1: the addition survives a save and a reload.
    let saved = gui.ui_config_json().expect("the config serializes");
    let mut reopened = Gui::new();
    assert!(reopened.load_ui_config(&store_with(&saved)));
    let pane = reopened.pane(0).expect("pane 0");
    assert!(
        pane.draw_order_vec().first() == Some(&known::TERRAIN)
            && pane.is_overlay_enabled(&known::TERRAIN),
        "the added Terrain row did not reopen where and how it was left",
    );
}

/// **A tombstoned Terrain stays removed** — the same contract every other
/// layer's removal carries, proven for the id that did not exist when the
/// tombstone mechanism shipped.
#[test]
fn a_tombstoned_terrain_stays_removed() {
    let json = r#"{
        "config_version": 4,
        "pane_count": 1,
        "active_pane": 0,
        "site": "KTLX",
        "panes": [{
            "layer_slots": [
                {"id": "Radar", "enabled": true}
            ],
            "removed_layers": [
                {"id": "Terrain"}
            ]
        }]
    }"#;

    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store_with(json)),
        "the config must load"
    );
    let pane = gui.pane(0).expect("pane 0");
    let order = pane.draw_order_vec();
    assert!(
        !order.contains(&known::TERRAIN),
        "the tombstone is the one thing that excludes a registered layer, \
         and the reconcile walked over it: {order:?}",
    );
    assert!(
        !pane.is_overlay_enabled(&known::TERRAIN),
        "a removed layer answers the draw gate with no, structurally",
    );
    // Non-triviality: the same load with no tombstone would have inserted it
    // (the test above), and the named slots are intact.
    assert!(order.contains(&known::RADAR));
}
