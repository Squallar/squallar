//! The click-registry gate: one UiSweep loop in the headless harness, with
//! the events flowing through the same normalize-then-pass pipeline the
//! renderer uses, and every scheduled target counted from the registry.
//!
//! RED on the unmodified baseline by construction — no registry exists there
//! — and non-vacuous past that: the counts are exact per target, and the
//! mid-loop reads assert the toggles really flipped on the glass, not merely
//! that events were emitted.

use super::*;
use crate::gesture_player::{GesturePlayer, LOOP_SECONDS, ui_sweep};

/// Drive `player` and `h` together up to elapsed time `until`, at 60 fps.
fn run_until(h: &mut InputHarness, player: &mut GesturePlayer, t: &mut f64, until: f64) {
    let dt = 1.0 / 60.0;
    while *t < until {
        *t += dt;
        let events = player.events_for_frame(*t, h.screen_rect());
        h.inject_events(events);
        h.frame_after(dt);
    }
}

#[test]
fn one_sweep_loop_clicks_every_registered_target_exactly_as_scheduled() {
    let mut h = InputHarness::new();
    // A layer whose inspector body draws a slider (GLM's time window),
    // selected and closed again: the sweep's inspector leg then opens onto a
    // body with a draggable control.
    h.open_layer_in_inspector(&known::LIGHTNING);
    h.close_inspector();
    h.open_layers();

    // What the stack really shows going in, per row: the sweep must invert
    // every one of these and then put every one back.
    let before: Vec<(LayerId, bool)> = h
        .stack()
        .rows
        .iter()
        .map(|row| (row.kind.clone(), row.eye_on))
        .collect();
    assert!(
        !before.is_empty(),
        "no stack rows drew; the sweep would have nothing to prove on"
    );
    assert!(
        before.len() <= 12,
        "more rows than the sweep's eye slots; this scene must fit one pass"
    );

    let mut player = GesturePlayer::from_name("ui-sweep").expect("a known script name");
    let mut t = 0.0;

    // Quiet phase #1: the off-pass is done, and every eye has flipped —
    // read back off the Gui, not off the emitted events.
    run_until(&mut h, &mut player, &mut t, 4.5);
    for (kind, was_on) in &before {
        assert_eq!(
            h.overlay_enabled(kind),
            !was_on,
            "{kind:?} did not flip in the off-pass"
        );
    }

    // Quiet phase #2: the on-pass has put every eye back.
    run_until(&mut h, &mut player, &mut t, 10.0);
    for (kind, was_on) in &before {
        assert_eq!(
            h.overlay_enabled(kind),
            *was_on,
            "{kind:?} did not flip back in the on-pass"
        );
    }

    // The rest of the loop: panel close/open, inspector open, slider drag,
    // inspector closed by its own button.
    run_until(&mut h, &mut player, &mut t, LOOP_SECONDS - 1e-6);
    assert!(
        h.layers_panel_on_screen(),
        "the sweep left the layers panel closed"
    );
    assert!(
        !h.inspector().open,
        "the sweep left the inspector open — its close button never landed"
    );

    // The registry's count: exactly the scheduled press/release pairs, per
    // target. Two per toggle-shaped target (off and on), one per one-shot.
    let delivered = player.pairs_delivered();
    for (kind, _) in &before {
        let id = format!("{}{}", ui_sweep::EYE_PREFIX, kind.as_str());
        assert_eq!(
            delivered.get(&id),
            Some(&2),
            "{id}: expected the off-pass and on-pass pair"
        );
    }
    assert_eq!(delivered.get(ui_sweep::LAYERS_TOGGLE), Some(&2));
    assert_eq!(delivered.get(ui_sweep::INSPECTOR_TOGGLE), Some(&1));
    assert_eq!(delivered.get(ui_sweep::INSPECTOR_CLOSE), Some(&1));
    let slider_pairs: u32 = delivered
        .iter()
        .filter(|(id, _)| id.starts_with(ui_sweep::SLIDER_PREFIX))
        .map(|(_, pairs)| *pairs)
        .sum();
    assert_eq!(
        slider_pairs, 1,
        "the slider leg delivered {slider_pairs} drags instead of one"
    );
    // Everything delivered was scheduled: no target got clicked twice by
    // accident of the agenda.
    let known_ids = delivered
        .keys()
        .filter(|id| {
            !id.starts_with(ui_sweep::EYE_PREFIX)
                && !id.starts_with(ui_sweep::SLIDER_PREFIX)
                && id.as_str() != ui_sweep::LAYERS_TOGGLE
                && id.as_str() != ui_sweep::INSPECTOR_TOGGLE
                && id.as_str() != ui_sweep::INSPECTOR_CLOSE
        })
        .count();
    assert_eq!(known_ids, 0, "pairs were delivered to unscheduled targets");
}
