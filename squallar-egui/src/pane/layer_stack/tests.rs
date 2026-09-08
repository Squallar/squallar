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

/// **A removal files the layer's opacity on its tombstone** - the promise
/// `config` already makes, for the same reason: an accidental click costs a
/// click, not a slider position. The re-add half, which this does not reach,
/// is `layer_curation_tests::re_adding_a_removed_layer_restores_its_opacity`,
/// driven through the pane where the re-add actually happens.
#[test]
fn a_removal_files_the_layers_opacity_on_its_tombstone() {
    let mut stack = LayerStack::default();
    let mut lightning = slot(known::LIGHTNING);
    lightning.opacity = Some(0.4);
    stack.push(lightning);
    stack.push(slot(known::METAR));

    stack.take_out(&known::LIGHTNING).expect("it was held");
    assert_eq!(stack.saved_opacity_of_removed(&known::LIGHTNING), Some(0.4));
    assert_eq!(
        stack.saved_opacity_of_removed(&known::METAR),
        None,
        "a layer that was never removed has no saved opacity",
    );
    stack.take_out(&known::METAR).expect("it was held");
    assert_eq!(
        stack.saved_opacity_of_removed(&known::METAR),
        None,
        "a layer removed at its default is re-added at its default, not at a \
         number the tombstone made up",
    );
}

/// Opacity is part of what a slot IS: two slots differing only in it are
/// unequal, and a clone carries it - the layer-link sync copies whole stacks
/// between panes every frame, and a copy that dropped it would reset the
/// slider on the next frame.
#[test]
fn two_slots_differing_only_in_opacity_are_unequal_and_a_clone_carries_it() {
    let a = slot(known::METAR);
    let mut b = slot(known::METAR);
    assert_eq!(a, b, "precondition: identical apart from what is set below");
    b.opacity = Some(0.5);
    assert_ne!(a, b, "opacity is not part of slot equality");
    let copy = b.clone();
    assert_eq!(copy.opacity, Some(0.5), "the clone dropped the opacity");
    assert_eq!(copy, b);
}

// ── The position index, door by door ──────────────────────────────────────
//
// The index answers `position_of`, and a STALE one is a wrong answer rather
// than a slow one: it names a slot that has moved, or misses a slot that has
// arrived. The type's defence is structural — `slots` and its index live
// behind `guarded::Slots`, whose only mutable door drops the index on the way
// in — but "structural" is a claim, and these are what hold it to the claim.
//
// One test per door that can move a slot, each asking the question the index
// answers rather than the one the `Vec` answers. The tamper they were all
// shown red under is `guarded::Slots::as_mut` not clearing `fresh`: that one
// edit is the whole of the invalidation, so every door below reads red
// together under it, which is the point — no door has its own remembering to
// do.

/// The whole index in one property: after any door, `position_of` and a plain
/// scan of the same stack agree about every id, present or absent.
fn index_agrees_with_a_scan(stack: &LayerStack, ids: &[LayerId]) {
    for id in ids {
        let scanned = stack.iter().position(|held| held.id == *id);
        assert_eq!(
            stack.position_of(id),
            scanned,
            "the index puts {id:?} at {:?} and a scan of the same stack puts \
             it at {scanned:?}",
            stack.position_of(id),
        );
    }
}

/// Every id these tests ask about — the three they place plus one they never
/// do, so a miss is exercised as well as a hit.
fn probed_ids() -> Vec<LayerId> {
    vec![
        known::METAR,
        known::NWS_ALERTS,
        known::LIGHTNING,
        known::GMGSI,
    ]
}

/// **`push`** — a slot arriving on top after the index has been built.
#[test]
fn the_index_survives_a_push() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    // Build the index BEFORE the mutation: a door that invalidates nothing is
    // indistinguishable from one that does unless something was there to lose.
    assert_eq!(stack.position_of(&known::METAR), Some(0));
    stack.push(slot(known::NWS_ALERTS));
    index_agrees_with_a_scan(&stack, &probed_ids());
    assert_eq!(stack.position_of(&known::NWS_ALERTS), Some(1));
}

/// **`insert`** — the door that moves every slot above it down one.
#[test]
fn the_index_survives_an_insert_below_a_held_slot() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    stack.push(slot(known::LIGHTNING));
    assert_eq!(stack.position_of(&known::LIGHTNING), Some(1));
    stack.insert(0, slot(known::NWS_ALERTS));
    index_agrees_with_a_scan(&stack, &probed_ids());
    assert_eq!(
        stack.position_of(&known::LIGHTNING),
        Some(2),
        "an insert underneath pushed the held slot up and the index did not \
         follow it",
    );
}

/// **`take_out`** — the door that removes, and therefore the one where a stale
/// index can name a position the list no longer has.
#[test]
fn the_index_survives_a_take_out() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    stack.push(slot(known::NWS_ALERTS));
    stack.push(slot(known::LIGHTNING));
    assert_eq!(stack.position_of(&known::LIGHTNING), Some(2));
    assert!(stack.take_out(&known::METAR).is_some());
    index_agrees_with_a_scan(&stack, &probed_ids());
    assert_eq!(stack.position_of(&known::METAR), None);
    assert_eq!(stack.position_of(&known::LIGHTNING), Some(1));
}

/// **`take_slots` + `set_slots`** — the whole-list rewrite `set_draw_order`
/// runs, which is a permutation and therefore invisible to any check that
/// only watches the list's length.
#[test]
fn the_index_survives_a_whole_list_rewrite_that_only_permutes() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    stack.push(slot(known::NWS_ALERTS));
    assert_eq!(stack.position_of(&known::METAR), Some(0));
    let mut taken = stack.take_slots();
    taken.reverse();
    stack.set_slots(taken);
    index_agrees_with_a_scan(&stack, &probed_ids());
    assert_eq!(
        stack.position_of(&known::METAR),
        Some(1),
        "the list was reversed and the index still points where the slot used \
         to be — the length never changed, so only the ids can catch this",
    );
}

/// **`clear`** — every position the index holds becomes out of bounds at once.
#[test]
fn the_index_survives_a_clear() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    assert_eq!(stack.position_of(&known::METAR), Some(0));
    stack.clear();
    index_agrees_with_a_scan(&stack, &probed_ids());
    assert_eq!(stack.position_of(&known::METAR), None);
}

/// **`adopt`** — the layer-link sync's whole-stack copy, which replaces the
/// slots wholesale from another pane.
#[test]
fn the_index_survives_an_adopt() {
    let mut source = LayerStack::default();
    source.push(slot(known::LIGHTNING));
    source.push(slot(known::GMGSI));

    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    assert_eq!(stack.position_of(&known::METAR), Some(0));
    stack.adopt(&source);
    index_agrees_with_a_scan(&stack, &probed_ids());
    assert_eq!(stack.position_of(&known::METAR), None);
    assert_eq!(stack.position_of(&known::GMGSI), Some(1));
}

/// **`DerefMut`** — the escape hatch, and the reason the invalidation is a
/// property of the door rather than a list of call sites.
///
/// `&mut [LayerSlot]` cannot insert or remove, so a reviewer reading the
/// mutation methods alone would conclude the index is safe. It can *reorder*,
/// and it can rewrite a slot's `id` in place, and either one moves the mapping
/// this index holds. Nothing in the crate does so today; the point is that
/// nothing has to remember not to.
#[test]
fn the_index_survives_a_reorder_through_the_mutable_slice() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    stack.push(slot(known::NWS_ALERTS));
    assert_eq!(stack.position_of(&known::METAR), Some(0));
    stack.swap(0, 1);
    index_agrees_with_a_scan(&stack, &probed_ids());
    assert_eq!(stack.position_of(&known::METAR), Some(1));
}

/// The other half of the same escape hatch: an id rewritten in place, so the
/// stack holds a layer the index has never heard of and no longer holds one it
/// has.
#[test]
fn the_index_survives_an_id_rewritten_through_the_mutable_slice() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    stack.push(slot(known::NWS_ALERTS));
    // Builds the index and leaves position 0 remembered. The slot rewritten
    // below is the OTHER one, so the memo cannot answer for it and the
    // fingerprint list is the only thing standing between the question and a
    // wrong answer.
    assert_eq!(stack.position_of(&known::METAR), Some(0));
    stack[1].id = known::GMGSI;
    index_agrees_with_a_scan(&stack, &probed_ids());
    assert_eq!(stack.position_of(&known::NWS_ALERTS), None);
    assert_eq!(
        stack.position_of(&known::GMGSI),
        Some(1),
        "a layer arrived by having its id written in place, and the index has \
         never heard of it",
    );
}

/// **A fingerprint rejects but never accepts.** Two ids sharing a length and
/// their first, middle and last byte are the same fingerprint and different
/// layers, and the index must still tell them apart — which it does by
/// confirming every candidate with a full comparison before returning it.
#[test]
fn two_ids_with_the_same_fingerprint_are_still_told_apart() {
    // Same length (5), same first byte, same middle byte, same last byte.
    let a = LayerId::new("axbxc");
    let b = LayerId::new("aybxc");
    let mut stack = LayerStack::default();
    stack.push(slot(a.clone()));
    stack.push(slot(b.clone()));
    assert_eq!(stack.position_of(&a), Some(0));
    assert_eq!(stack.position_of(&b), Some(1));
    assert_eq!(stack.position_of(&LayerId::new("azbxc")), None);
}

/// **The memo is a shortcut, never an answer.** The walk asks the pane three
/// or four questions about the same layer in a row, so the resolver remembers
/// the last position it returned — and a remembered position that has stopped
/// being right must fall through to the scan rather than be believed.
#[test]
fn the_remembered_position_is_confirmed_before_it_is_believed() {
    let mut stack = LayerStack::default();
    stack.push(slot(known::METAR));
    stack.push(slot(known::NWS_ALERTS));
    // Leaves METAR remembered at 0.
    assert_eq!(stack.position_of(&known::METAR), Some(0));
    stack.swap(0, 1);
    assert_eq!(
        stack.position_of(&known::METAR),
        Some(1),
        "the memo answered from the position METAR held before the swap",
    );
}
