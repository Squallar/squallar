//! The click-registry gate: one UiSweep loop in the headless harness, with
//! the events flowing through the same normalize-then-pass pipeline the
//! renderer uses, and every scheduled target counted from the registry.
//!
//! RED on the unmodified baseline by construction — no registry exists there
//! — and non-vacuous past that: the counts are exact per target, and the
//! mid-loop reads assert the toggles really flipped on the glass, not merely
//! that events were emitted.
//!
//! **What that was NOT, until 2026-08-31.** Every count here was taken against
//! the rows the panel had DRAWN, so the gate asked "did the sweep click
//! everything it found" and never "did it find everything there is". On a
//! layout that draws fewer controls it read 100% while exercising less of the
//! app, and nothing on the row said so — the same checker-and-checked-from-one-
//! belief shape as the three raster holes closed the same day.
//! [`InputHarness::stack_inventory`] is the denominator the registry cannot
//! supply, and [`SCHEDULED_TARGETS`] is the same thing for the four controls
//! the script drives by name.

use super::*;
use crate::gesture_player::{GesturePlayer, LOOP_SECONDS, ui_sweep};

/// **The sweep's non-eye targets, named by the script rather than found on the
/// glass.** One row per control `GesturePlayer::ui_sweep` schedules, with the
/// press/release pairs one loop owes it.
///
/// Derived from the agenda, not from the registry: this is what the scene is
/// supposed to drive, so "registered" and "delivered" can both be measured
/// against it. `SLIDER_PREFIX` is a prefix because sliders register per
/// control and the sweep drags the first in id order.
const SCHEDULED_TARGETS: &[(&str, u32)] = &[
    (ui_sweep::LAYERS_TOGGLE, 2),
    (ui_sweep::INSPECTOR_TOGGLE, 1),
    (ui_sweep::INSPECTOR_CLOSE, 1),
    (ui_sweep::SLIDER_PREFIX, 1),
];

/// A phone. Below `COMPACT_MAX_WIDTH`, so `WidthClass::Compact`.
const PHONE_SCREEN: egui::Vec2 = egui::vec2(412.0, 915.0);

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

    // **The denominator first, and it does not come from the registry.** What
    // the pane HOLDS an eye for, computed off the pane and the overlay
    // registry; then the rows that really drew, held to it. A layout that
    // drew fewer rows than the pane holds is a scene doing less work, and
    // before this assertion existed it read as a full pass.
    let inventory = h.stack_inventory();
    assert!(
        !inventory.is_empty(),
        "floor: the pane holds no layers at all, so the equality below is \
         between two empty lists and the sweep has nothing to prove on"
    );
    let drew: Vec<LayerId> = h.stack().rows.iter().map(|row| row.kind.clone()).collect();
    assert_eq!(
        drew,
        inventory,
        "the layers panel drew {} of the {} layers this pane holds. The sweep \
         can only target what registered, and what registers is what drew, so \
         every count below would still read as a full pass over a scene that \
         exercised {} fewer controls. Missing: {:?}",
        drew.len(),
        inventory.len(),
        inventory.len() - drew.len(),
        inventory
            .iter()
            .filter(|id| !drew.contains(id))
            .collect::<Vec<_>>(),
    );

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

    // **Every scheduled target really got its pairs** — checked against the
    // script's own list rather than against the registry, so a control that
    // stopped drawing (and therefore stopped registering, and therefore
    // stopped being targeted) fails here instead of vanishing from both sides
    // of the comparison at once.
    let unmet: Vec<&str> = SCHEDULED_TARGETS
        .iter()
        .filter(|(id, owed)| {
            delivered
                .iter()
                .filter(|(got, _)| got.as_str() == *id || got.starts_with(id))
                .map(|(_, pairs)| *pairs)
                .sum::<u32>()
                != *owed
        })
        .map(|(id, _)| *id)
        .collect();
    assert!(
        unmet.is_empty(),
        "the sweep's agenda names {} controls and {unmet:?} were never driven \
         on this layout",
        SCHEDULED_TARGETS.len(),
    );
}

/// **The sweep drives strictly less of the app on a phone, and this is the
/// number that says so.**
///
/// The user's report, verbatim: *"the firefox on android tests didn't toggle a
/// bunch of layers in its test, I think the rigging is a little bit broken for
/// the mobile view."* It is not the rigging. It is the layout, and the rig was
/// unable to see it.
///
/// **MEASURED MECHANISM.** `render_top_bar` computes
/// `let model = (!compact).then(|| self.menu_model())` and registers
/// [`ui_sweep::LAYERS_TOGGLE`] and [`ui_sweep::INSPECTOR_TOGGLE`] **inside**
/// `if let Some(model) = &model`. On `WidthClass::Compact` that branch never
/// runs, so neither control registers a rect. The sweep therefore never opens
/// the layers panel and never opens the inspector — and because
/// [`ui_sweep::INSPECTOR_CLOSE`] and the slider are drawn *by* the inspector,
/// those two cannot register either. Four of the sweep's controls, gone,
/// silently.
///
/// **AND THE OLD GATE COULD NOT SEE ANY OF IT**, because it counted presses
/// against the registry: no rect registered means no press scheduled means no
/// press missing. 100%, over a scene doing a fraction of the work. Every
/// Android scene-D figure on the scoreboard (p99 22.6 / 45.3 / 26.9 ms) is
/// against this smaller control set, and nothing on those rows says so. The
/// same applies to any narrow desktop window; nobody has to be on a phone.
///
/// **THIS TEST PINS THE GAP, IT DOES NOT BLESS IT.** The list below is a
/// measurement of today's mobile UI. If it goes red because a control started
/// registering on a phone, that is the fix landing: shorten the list, and
/// re-state whether scene D is comparable across viewports — it is not while
/// this list is non-empty.
#[test]
fn a_phone_layout_presents_none_of_the_sweeps_named_controls() {
    let mut h = InputHarness::with_screen(PHONE_SCREEN);
    h.warm_up();
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "floor: {PHONE_SCREEN:?} did not bucket as a phone, so this test is \
         measuring the desktop layout under a mobile name",
    );

    let mut player = GesturePlayer::from_name("ui-sweep").expect("a known script name");
    let mut t = 0.0;
    run_until(&mut h, &mut player, &mut t, LOOP_SECONDS - 1e-6);

    let delivered = player.pairs_delivered();
    let driven: Vec<&str> = SCHEDULED_TARGETS
        .iter()
        .filter(|(id, _)| {
            delivered
                .keys()
                .any(|got| got.as_str() == *id || got.starts_with(id))
        })
        .map(|(id, _)| *id)
        .collect();

    assert!(
        driven.is_empty(),
        "the phone layout now presents {driven:?} to the sweep. That is the \
         mobile-UI gap closing, which is good — shorten this list, and say \
         explicitly whether scene D is comparable across viewports now. It is \
         not while any of the sweep's named controls is unreachable on a \
         phone.",
    );

    // The floor, and it is what stops the emptiness above being free: the same
    // player, the same script and the same loop DO drive all four at a desktop
    // width. Without this, a `pairs_delivered` that always returned nothing —
    // or a `run_until` that ran no frames — would satisfy the assertion above
    // for reasons that have nothing to do with the layout.
    let mut wide = InputHarness::new();
    wide.open_layer_in_inspector(&known::LIGHTNING);
    wide.close_inspector();
    wide.open_layers();
    let mut player = GesturePlayer::from_name("ui-sweep").expect("a known script name");
    let mut t = 0.0;
    run_until(&mut wide, &mut player, &mut t, LOOP_SECONDS - 1e-6);
    let wide_delivered = player.pairs_delivered();
    let wide_driven: Vec<&str> = SCHEDULED_TARGETS
        .iter()
        .filter(|(id, _)| {
            wide_delivered
                .keys()
                .any(|got| got.as_str() == *id || got.starts_with(id))
        })
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        wide_driven.len(),
        SCHEDULED_TARGETS.len(),
        "floor: the desktop layout drove {wide_driven:?} of the sweep's \
         {} named controls, so the phone reading above is not a statement \
         about the phone",
        SCHEDULED_TARGETS.len(),
    );

    // The eyes are the other half, and they come apart the same way: a phone
    // registers a row only while the Layers page is up, and the sweep has no
    // way to raise it.
    let phone_eyes = delivered
        .keys()
        .filter(|id| id.starts_with(ui_sweep::EYE_PREFIX))
        .count();
    let wide_eyes = wide_delivered
        .keys()
        .filter(|id| id.starts_with(ui_sweep::EYE_PREFIX))
        .count();
    assert!(
        phone_eyes < wide_eyes,
        "the phone drove {phone_eyes} layer eyes and the desktop {wide_eyes}. \
         If those are equal the mobile gap has closed and this whole test \
         should be replaced by the equality",
    );
}
