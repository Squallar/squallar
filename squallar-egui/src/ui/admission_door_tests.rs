//! **The admission doors, driven through the real chrome.**
//!
//! Every door here is private to `crate::ui` or below, which is why these
//! tests live under it rather than beside the ledger they exercise
//! (`crate::admission::tests` holds that half).
//!
//! **Both arms, everywhere.** Each refusal fixture has an admit fixture next
//! to it that resembles it — the same act, one fact changed — because
//! over-firing is the worse direction: a door that turns away scenes the
//! machine can hold is a door a user switches off, and then nothing is gated.
//!
//! **WO-G is advisory**, so what moves is the verdict and the ledger's own
//! counts, never the scene. `refused` is asserted at zero throughout: that is
//! the whole claim of this land.

use crate::admission::{AdmissionCosts, LayerGrid, PaneAdmission};
use crate::input_harness::InputHarness;
use squallar_device_profile::admit::{Increment, LoopFrames, Spare};
use squallar_source::id::{LayerId, known};

const MIB: u64 = 1024 * 1024;

/// A cost table with `spare` on both pools, `per_layer` for every pane's
/// picture and the same for one more pane. Generation 1, so the doors are
/// live — a `0` table admits everything by construction.
fn costs(spare_bytes: u64, per_layer: u64, panes: usize) -> AdmissionCosts {
    AdmissionCosts {
        generation: 1,
        spare: Spare {
            gpu_bytes: Some(spare_bytes),
            host_bytes: Some(spare_bytes),
            joint_bytes: None,
        },
        panes: vec![
            PaneAdmission {
                show_layer: Increment::host(per_layer),
                ..PaneAdmission::default()
            };
            panes.max(1)
        ],
        new_pane: Increment::host(per_layer),
        layer_grids: Vec::new(),
        frames: LoopFrames {
            budget_span_secs: 60 * 60,
            render_budget: 30,
        },
    }
}

/// A texture layer every build registers and that no pane ships showing, so
/// switching it on is a real transition.
fn a_layer() -> LayerId {
    known::NWS_ALERTS
}

// ── The layer door ────────────────────────────────────────────────────────

/// **`write_pane_overlay`, both arms.** The same eye-click against a pool
/// with room and against one without. The layer goes on either way — WO-G
/// refuses nothing — and the verdict is what differs.
#[test]
fn the_layer_door_reads_both_arms_and_turns_nothing_away() {
    for (spare, want_refusal) in [(1024 * MIB, false), (0, true)] {
        let mut h = InputHarness::new();
        let id = a_layer();
        h.gui_mut().set_overlay_on_pane_for_test(0, &id, false);
        h.set_admission(costs(spare, 64 * MIB, 1));

        let before = h.gui().admission().counts();
        h.gui_mut().set_overlay_on_pane_for_test(0, &id, true);
        // The real door, on the taken pane, exactly as the stack's eye does.
        let mut pane = std::mem::take(&mut h.gui_mut().panes[0]);
        pane.set_overlay_enabled(id.clone(), false);
        let gui = h.gui_mut();
        super::Gui::write_pane_overlay(
            &mut gui.overlays,
            &mut gui.admission,
            0,
            &mut pane,
            &id,
            true,
        );
        gui.panes[0] = pane;
        let moved = h.gui().admission().counts().since(before);

        assert!(
            h.gui().pane(0).is_some_and(|p| p.is_overlay_enabled(&id)),
            "WO-G refuses nothing: the layer is on whatever the verdict",
        );
        assert_eq!(moved.refused, 0, "WO-G turns nothing away");
        if want_refusal {
            assert!(
                moved.would_refuse > 0,
                "64 MiB of picture against no spare must register a \
                 would-refuse; the door moved {moved:?}",
            );
        } else {
            assert_eq!(
                moved.would_refuse, 0,
                "64 MiB of picture against 1 GiB of spare must not register a \
                 would-refuse; the door moved {moved:?}",
            );
        }
    }
}

/// **Hiding a layer is free, and showing one already shown is free.** The
/// admit fixture that resembles the refuse arm above: the same door, the same
/// empty pool, an act that is not a transition. Without this a repainting
/// frame would ask on every pass.
#[test]
fn the_layer_door_charges_only_for_a_transition() {
    let mut h = InputHarness::new();
    let id = a_layer();
    h.gui_mut().set_overlay_on_pane_for_test(0, &id, true);
    h.set_admission(costs(0, 64 * MIB, 1));

    let before = h.gui().admission().counts();
    for on in [true, false, false] {
        let mut pane = std::mem::take(&mut h.gui_mut().panes[0]);
        pane.set_overlay_enabled(id.clone(), true);
        let gui = h.gui_mut();
        super::Gui::write_pane_overlay(
            &mut gui.overlays,
            &mut gui.admission,
            0,
            &mut pane,
            &id,
            on,
        );
        gui.panes[0] = pane;
    }
    let moved = h.gui().admission().counts().since(before);
    assert_eq!(
        moved.asked, 0,
        "a layer already shown, and a layer being hidden, must ask for \
         nothing even on an empty pool; the door moved {moved:?}",
    );
}

/// **A layer whose grid is already resident is charged for the picture
/// alone.** The refuse arm is the same layer with nothing holding its grid:
/// one fact apart, and the arithmetic is the whole difference.
#[test]
fn a_resident_grid_is_not_charged_at_the_door() {
    for (resident, want_refusal) in [(true, false), (false, true)] {
        let mut h = InputHarness::new();
        let id = a_layer();
        h.gui_mut().set_overlay_on_pane_for_test(0, &id, false);
        // Room for the picture and not for the grid.
        let mut table = costs(8 * MIB, 4 * MIB, 1);
        table.layer_grids = vec![LayerGrid {
            id: id.clone(),
            grid_bytes: 64 * MIB,
            resident,
        }];
        h.set_admission(table);

        let before = h.gui().admission().counts();
        h.gui_mut().add_layer_on_pane_for_test(0, &id);
        let mut pane = std::mem::take(&mut h.gui_mut().panes[0]);
        pane.set_overlay_enabled(id.clone(), false);
        let gui = h.gui_mut();
        super::Gui::write_pane_overlay(
            &mut gui.overlays,
            &mut gui.admission,
            0,
            &mut pane,
            &id,
            true,
        );
        gui.panes[0] = pane;
        let moved = h.gui().admission().counts().since(before);

        assert_eq!(
            moved.would_refuse > 0,
            want_refusal,
            "grid resident = {resident}: the door moved {moved:?}",
        );
    }
}

// ── The pane door ─────────────────────────────────────────────────────────

/// **`set_pane_count`, both arms**, and the one door for a pane: `grown_pane`
/// comes through it, so nothing is charged twice.
#[test]
fn the_pane_door_reads_both_arms() {
    for (spare, want_refusal) in [(1024 * MIB, false), (0, true)] {
        let mut h = InputHarness::new();
        h.set_admission(costs(spare, 128 * MIB, 4));
        let before = h.gui().admission().counts();
        assert!(h.gui_mut().set_pane_count(2));
        let moved = h.gui().admission().counts().since(before);

        assert_eq!(
            h.gui().pane_count(),
            2,
            "WO-G refuses nothing: the pane opened whatever the verdict",
        );
        assert_eq!(moved.refused, 0);
        assert_eq!(
            moved.would_refuse > 0,
            want_refusal,
            "spare {spare}: the door moved {moved:?}",
        );
    }
}

/// **A count that does not grow asks for nothing.** The pane door's admit
/// fixture: the same call, the same empty pool, no growth. A frame that
/// re-states the layout would otherwise ask on every pass.
#[test]
fn the_pane_door_charges_only_for_growth() {
    let mut h = InputHarness::new();
    h.set_admission(costs(0, 128 * MIB, 4));
    assert!(h.gui_mut().set_pane_count(3));

    let before = h.gui().admission().counts();
    assert!(h.gui_mut().set_pane_count(3));
    assert!(h.gui_mut().set_pane_count(2));
    let moved = h.gui().admission().counts().since(before);
    assert_eq!(
        moved.asked, 0,
        "re-stating a count and shrinking one must ask for nothing; the door \
         moved {moved:?}",
    );
}

// ── The default-layer door ────────────────────────────────────────────────

/// **`initialize_pane_enabled` is a batch and it settles.** The first call on
/// a fresh Gui mints every default-on layer's slot and asks once; the second
/// finds every pane holding them and asks for nothing.
#[test]
fn the_default_layer_door_asks_once_and_then_settles() {
    let mut h = InputHarness::new();
    h.set_admission(costs(0, 32 * MIB, 1));

    let before = h.gui().admission().counts();
    h.gui_mut().initialize_pane_enabled();
    let settled = h.gui().admission().counts().since(before);
    assert_eq!(
        settled.asked, 0,
        "a Gui whose panes already hold every default layer must ask for \
         nothing; the door moved {settled:?}",
    );
}

// ── The preset door ───────────────────────────────────────────────────────

/// **A preset is one ask, whole.** Four panes and a set of layers reach the
/// ledger exactly once — a per-toggle refusal is what would leave half a
/// preset applied, and the count is the property that forbids it.
#[test]
fn a_preset_reaches_the_ledger_exactly_once() {
    let mut h = InputHarness::new();
    h.set_admission(costs(u64::MAX, 8 * MIB, 6));
    let preset = super::catalog::PresetConfig {
        name: "wide".to_string(),
        pane_count: 4,
        panes: Vec::new(),
        overlays: vec![a_layer(), known::METAR].into(),
    };
    let before = h.gui().admission().counts();
    let mut actions = Vec::new();
    h.gui_mut().apply_preset_for_test(&preset, &mut actions);
    let moved = h.gui().admission().counts().since(before);
    assert_eq!(
        moved.asked, 1,
        "a preset must be one ask however many panes and layers it names; \
         the door moved {moved:?}",
    );
}

/// **A preset onto a scene at capacity is refused whole, and one onto a scene
/// with room is admitted whole.** The two arms, one fact apart.
#[test]
fn a_preset_reads_both_arms_as_one_verdict() {
    for (spare, want_refusal) in [(u64::MAX, false), (0, true)] {
        let mut h = InputHarness::new();
        h.set_admission(costs(spare, 64 * MIB, 6));
        let preset = super::catalog::PresetConfig {
            name: "wide".to_string(),
            pane_count: 4,
            panes: Vec::new(),
            overlays: vec![a_layer(), known::METAR].into(),
        };
        let before = h.gui().admission().counts();
        let mut actions = Vec::new();
        h.gui_mut().apply_preset_for_test(&preset, &mut actions);
        let moved = h.gui().admission().counts().since(before);

        assert_eq!(
            h.gui().pane_count(),
            4,
            "WO-G applies the preset either way"
        );
        assert_eq!(moved.refused, 0);
        assert_eq!(
            moved.would_refuse > 0,
            want_refusal,
            "spare {spare}: the preset door moved {moved:?}",
        );
    }
}

// ── The span door ─────────────────────────────────────────────────────────

/// **The span door charges the panes that are looping and nobody else.** A
/// scene with no loop running is the admit fixture: the same slider, the same
/// empty pool, nothing to add.
#[test]
fn the_span_door_charges_nothing_when_no_pane_is_looping() {
    let mut h = InputHarness::new();
    h.set_admission(costs(0, 0, 1));
    let before = h.gui().admission().counts();
    h.gui_mut().set_loop_span_secs(4 * 60 * 60);
    let moved = h.gui().admission().counts().since(before);
    assert_eq!(
        moved.asked, 0,
        "widening the window with nothing looping adds no frames; the door \
         moved {moved:?}",
    );
}

/// **And it charges the frames a wider window adds when one is.** Both arms
/// of the same slider: a pane looping at a cadence, a spare that holds the
/// delta and one that does not.
#[test]
fn the_span_door_charges_the_frames_a_wider_window_adds() {
    for (spare, want_refusal) in [(1024 * MIB, false), (0, true)] {
        let mut h = InputHarness::new();
        let mut table = costs(spare, 0, 1);
        table.panes = vec![PaneAdmission {
            looping: true,
            loop_frame: Increment {
                gpu_bytes: 8 * MIB,
                host_bytes: 0,
            },
            loop_frames_now: 2,
            cadence_secs: Some(300),
            ..PaneAdmission::default()
        }];
        h.set_admission(table);

        let before = h.gui().admission().counts();
        h.gui_mut().set_loop_span_secs(60 * 60);
        let moved = h.gui().admission().counts().since(before);
        assert!(
            moved.asked > 0,
            "a wider window over a looping pane must ask; the door moved \
             {moved:?}",
        );
        assert_eq!(moved.refused, 0);
        assert_eq!(
            moved.would_refuse > 0,
            want_refusal,
            "spare {spare}: the span door moved {moved:?}",
        );
    }
}

/// A **narrower** window frees frames and asks for nothing. The saturating
/// difference is what makes it free rather than negative.
#[test]
fn a_narrower_window_asks_for_nothing() {
    let mut h = InputHarness::new();
    let mut table = costs(0, 0, 1);
    table.panes = vec![PaneAdmission {
        looping: true,
        loop_frame: Increment {
            gpu_bytes: 8 * MIB,
            host_bytes: 0,
        },
        loop_frames_now: 12,
        cadence_secs: Some(300),
        ..PaneAdmission::default()
    }];
    h.set_admission(table);
    let before = h.gui().admission().counts();
    h.gui_mut().set_loop_span_secs(10 * 60);
    let moved = h.gui().admission().counts().since(before);
    assert_eq!(
        moved.asked, 0,
        "narrowing the window frees frames; the door moved {moved:?}",
    );
}

// ── Nothing the existing suite does changes ───────────────────────────────

/// **A harness with no table published asks nothing, anywhere.** This is what
/// lets the whole existing suite run through the doors untouched: `None`
/// crosses the seam, the ledger stays at generation 0, and every door is a
/// compare that returns immediately.
#[test]
fn a_session_that_never_priced_a_scene_never_asks() {
    let mut h = InputHarness::new();
    assert!(h.gui_mut().set_pane_count(4));
    h.gui_mut().set_loop_span_secs(4 * 60 * 60);
    h.gui_mut().initialize_pane_enabled();
    h.gui_mut()
        .set_overlay_on_pane_for_test(0, &a_layer(), true);
    assert_eq!(
        h.gui().admission().counts(),
        crate::admission::Totals::default(),
        "no table means no ask — the existing suite must not be gated by a \
         door it never published a spare to",
    );
}

// ── Restore is never a refusal ────────────────────────────────────────────

/// **Six panes saved, loaded under a capacity that has nothing left, saved
/// again — still six.**
///
/// The one property the whole admission system must not break. A restore that
/// admission narrowed would be persisted by the next autosave within seconds,
/// and the user would lose panes without having touched anything: a scene the
/// machine could not hold would be replaced by a smaller one they never asked
/// for and cannot get back. So `load_ui_config` is exempt — **restore is never
/// a refusal** — and this is the gate that says so.
///
/// It passes trivially under WO-G, which refuses nothing. That is the point of
/// writing it here rather than with the enforcing land: it is a control now
/// and the real gate the moment anything starts turning acts away, and a gate
/// that only appears alongside the behaviour it guards has never been seen to
/// pass on the behaviour's absence.
#[test]
fn a_six_pane_config_survives_a_restore_under_a_capacity_with_nothing_left() {
    let store = squallar_kv::MemoryKvStore::default();
    let mut saved = crate::Gui::new();
    // The real door, so the panes are seeded the way the chrome seeds them.
    assert!(saved.set_pane_count(6), "the fixture wants six panes");
    saved.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    // A session with nothing spare on either pool, published BEFORE the load
    // so every door the restore passes through sees an empty machine.
    restored.admission.adopt(&costs(0, 512 * MIB, 6));
    assert!(restored.load_ui_config(&store), "the config must load");
    assert_eq!(
        restored.pane_count(),
        6,
        "a restore under a full machine lost panes; restore is never a \
         refusal, and an autosave would make the loss permanent",
    );

    // And the second save writes back what the first one wrote.
    let again = squallar_kv::MemoryKvStore::default();
    restored.save_ui_config(&again);
    let mut round_two = crate::Gui::new();
    assert!(round_two.load_ui_config(&again));
    assert_eq!(
        round_two.pane_count(),
        6,
        "the autosave after a constrained restore narrowed the config",
    );
}
