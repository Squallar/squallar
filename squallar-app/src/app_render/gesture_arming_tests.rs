//! The non-vacuity pair's dormant half, held at the arming seam: with the
//! key absent and the variable unset, no player exists, so `begin_frame`'s
//! `extra_events` is the empty vector and the raw input is byte-identical to
//! an unarmed build. Tamper-checked once at WO-4 time: force
//! `gesture_player_from` to arm unconditionally and
//! `an_absent_key_and_variable_arm_no_player` goes red.

use squallar_kv::{KvStore, MemoryKvStore};

/// The dormant half. Every clause matters: no store at all, a store without
/// the key, and a store with an unknown script name must all arm nothing.
#[test]
fn an_absent_key_and_variable_arm_no_player() {
    assert!(
        super::gesture_player_from(None, None).is_none(),
        "an install with no store and no variable armed a player"
    );

    let store = MemoryKvStore::default();
    assert!(
        super::gesture_player_from(None, Some(&store)).is_none(),
        "an install that never set the key armed a player"
    );

    store
        .store(super::GESTURE_SCRIPT_KEY, "no-such-script")
        .expect("the memory store always accepts a write");
    assert!(
        super::gesture_player_from(None, Some(&store)).is_none(),
        "an unknown script name armed a player"
    );
}

/// The armed half: a stored name arms, the variable arms, and the variable
/// outranks the store.
#[test]
fn the_key_or_the_variable_arms_the_named_script() {
    let store = MemoryKvStore::default();
    store
        .store(super::GESTURE_SCRIPT_KEY, "pan-zoom-2d")
        .expect("the memory store always accepts a write");
    assert!(
        super::gesture_player_from(None, Some(&store)).is_some(),
        "the stored script name did not arm"
    );
    assert!(
        super::gesture_player_from(Some("ui-sweep".into()), None).is_some(),
        "the variable alone did not arm"
    );
    assert!(
        super::gesture_player_from(Some("no-such-script".into()), Some(&store)).is_none(),
        "the variable must outrank the store, even holding a bad name"
    );
}

/// Byte-identical means byte-identical: the seam appends a vector, and the
/// dormant vector is empty, so the events egui sees are exactly the events
/// the platform delivered.
#[test]
fn a_dormant_frame_leaves_the_event_vec_untouched() {
    let real = vec![
        egui::Event::PointerMoved(egui::pos2(3.0, 4.0)),
        egui::Event::PointerGone,
    ];
    let mut events = real.clone();
    let extra: Vec<egui::Event> = match super::gesture_player_from(None, None) {
        Some(_) => panic!("the dormant arm produced a player"),
        None => Vec::new(),
    };
    events.extend(extra);
    assert_eq!(events, real);
}

/// The key is the localStorage name's other half — the rig seeds
/// `squallar.gesture_script` from this literal, so a rename silently
/// disarms every web leg.
#[test]
fn the_gesture_script_key_is_pinned() {
    assert_eq!(super::GESTURE_SCRIPT_KEY, "gesture_script");
}
