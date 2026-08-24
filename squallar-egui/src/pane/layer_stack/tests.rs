//! The curation rule, at the level of the type that owns it: what a stack
//! admits, what a tombstone outranks, and what a removal keeps.

use super::*;
use squallar_source::id::known;

fn slot(id: LayerId) -> LayerSlot {
    LayerSlot::new(id, true)
}

/// A stack that holds a layer does not take a second copy of it, whatever the
/// layer ships as — `admits` is the whole membership rule and it is asked
/// before every join.
#[test]
fn a_stack_does_not_admit_a_layer_it_already_holds() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    assert!(!stack.admits(&known::METAR, true));
    assert!(!stack.admits(&known::METAR, false));
}

/// **The `default_enabled` gate**: a layer that ships on joins a stack that
/// has never heard of it; one that ships off waits to be asked for.
///
/// This is the rule that stops the panel's length from being a function of how
/// many source crates the build links.
#[test]
fn an_unheld_layer_joins_only_if_it_ships_enabled() {
    let stack = LayerStack::default();
    assert!(
        stack.admits(&known::NWS_ALERTS, true),
        "a layer that ships on belongs on a fresh pane",
    );
    assert!(
        !stack.admits(&known::LIGHTNING, false),
        "a layer that ships off waits in the catalogue - a stack that took \
         every registered layer is the projection this type exists to end",
    );
}

/// **A tombstone outranks a default.** The whole point of writing removals
/// down: without this, "registered, ships on, and this pane has no slot for
/// it" is indistinguishable from a layer that has just been registered, and
/// the reconcile hands a removed layer back on the next frame.
#[test]
fn a_removed_layer_is_not_readmitted_even_though_it_ships_enabled() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::NWS_ALERTS));
    assert!(stack.take_out(&known::NWS_ALERTS).is_some());
    assert!(stack.is_removed(&known::NWS_ALERTS));
    assert!(
        !stack.admits(&known::NWS_ALERTS, true),
        "the reconcile would undo every removal of a default-on layer",
    );
}

/// Removal keeps what the layer held, and a re-add hands it back — so an
/// accidental click costs a click, not a re-configuration.
#[test]
fn a_removal_keeps_the_layers_configuration_for_the_re_add() {
    let mut stack = LayerStack::default();
    let mut lightning = slot(known::LIGHTNING);
    lightning.config = serde_json::json!({"time_window_secs": 900.0});
    stack.push(lightning);

    stack.take_out(&known::LIGHTNING).expect("it was held");
    assert_eq!(
        stack.saved_config_of_removed(&known::LIGHTNING),
        serde_json::json!({"time_window_secs": 900.0}),
    );
    assert_eq!(
        stack.saved_config_of_removed(&known::METAR),
        serde_json::Value::Null,
        "a layer that was never removed has nothing saved, and says so as \
         Null rather than as an empty object",
    );
}

/// Putting a layer back clears its tombstone — through every door, because a
/// layer that is visibly in the list must not also be recorded as removed.
#[test]
fn every_door_that_puts_a_slot_back_clears_its_tombstone() {
    /// One way a slot gets back into a stack, named for the failure message.
    type Door = (&'static str, fn(&mut LayerStack));

    let doors: [Door; 3] = [
        ("push", |s| s.push(slot(known::METAR))),
        ("insert", |s| s.insert(0, slot(known::METAR))),
        ("set_slots", |s| s.set_slots(vec![slot(known::METAR)])),
    ];
    for (name, put_back) in doors {
        let mut stack = LayerStack::default();
        stack.push(slot(known::METAR));
        stack.take_out(&known::METAR);
        assert!(stack.is_removed(&known::METAR), "{name}: precondition");
        put_back(&mut stack);
        assert!(
            !stack.is_removed(&known::METAR),
            "{name} left a tombstone on a layer that is in the list",
        );
        assert!(stack.holds(&known::METAR), "{name} did not put it back");
    }
}

/// A reorder is a permutation, not an un-removal: `take_slots` leaves the
/// tombstones alone so a drag cannot resurrect a layer that is not in the list
/// being permuted.
#[test]
fn taking_the_slots_out_for_a_reorder_leaves_the_tombstones() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    stack.push(slot(known::CITY_LABELS));
    stack.take_out(&known::METAR);

    let mut slots = stack.take_slots();
    slots.reverse();
    stack.set_slots(slots);

    assert!(
        stack.is_removed(&known::METAR),
        "a reorder of the remaining rows dropped an unrelated removal",
    );
    assert_eq!(stack.len(), 1);
}

/// The layer-link copy carries the tombstones. A copy that brought only the
/// slots would leave the destination pane's next reconcile free to hand back
/// every layer the group just removed.
#[test]
fn adopting_another_panes_stack_carries_its_removals() {
    let mut src = LayerStack::default();
    src.push(slot(known::NWS_ALERTS));
    src.push(slot(known::CITY_LABELS));
    src.take_out(&known::NWS_ALERTS);

    let mut dst = LayerStack::default();
    dst.push(slot(known::NWS_ALERTS));
    dst.adopt(&src);

    assert!(!dst.holds(&known::NWS_ALERTS));
    assert!(dst.is_removed(&known::NWS_ALERTS));
    assert!(
        !dst.admits(&known::NWS_ALERTS, true),
        "the destination would re-grow the layer the group removed",
    );
}
