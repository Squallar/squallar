//! **The frame's retirement drain, asserted on what it MOVED.**
//!
//! A drain that runs and finds nothing is indistinguishable, from the outside,
//! from a working seam — and that is the exact shape the first design of this
//! took: a drain hung off the fetch-delivery path would have found the memos'
//! parked rows empty every frame, because those retire at dispatch. So every
//! assertion here is on a non-zero count, never on "the call happened".

use crate::input_harness::InputHarness;
use squallar_overlays::render::overlay_state::{OverlayFetchResult, OverlayRegistry, PaneRef};

/// One alert, with enough text and geometry that a generation of them is a
/// real allocation rather than a token.
fn an_alert(id: &str) -> squallar_overlays::nws::alert::NwsAlert {
    let ring: Vec<(f64, f64)> = (0..64)
        .map(|i| {
            let t = i as f64 * std::f64::consts::TAU / 64.0;
            (35.0 + 0.3 * t.sin(), -97.0 + 0.3 * t.cos())
        })
        .collect();
    squallar_overlays::nws::alert::NwsAlert {
        id: id.to_string(),
        event: "Tornado Warning".to_string(),
        category: squallar_overlays::nws::alert::AlertCategory::Warning,
        severity: squallar_overlays::nws::alert::AlertSeverity::Extreme,
        urgency: squallar_overlays::nws::alert::AlertUrgency::Immediate,
        certainty: squallar_overlays::nws::alert::AlertCertainty::Observed,
        headline: Some("A tornado warning is in effect".to_string()),
        description: "x".repeat(1200),
        instruction: Some("y".repeat(400)),
        area_desc: "Cleveland, OK".to_string(),
        sender_name: "NWS Norman OK".to_string(),
        effective: "2026-05-20T19:56:00-05:00".to_string(),
        expires: "2026-05-20T20:30:00-05:00".to_string(),
        onset: None,
        ends: None,
        valid_from: None,
        valid_until: None,
        affected_zones: Vec::new(),
        features: std::sync::Arc::new(vec![squallar_overlays::types::OverlayFeature::new(
            vec![vec![ring]],
            [255, 0, 0, 80],
            [255, 0, 0, 255],
            "Tornado Warning".to_string(),
            String::new(),
            squallar_overlays::types::HatchPattern::None,
        )]),
    }
}

fn ingest(h: &mut InputHarness, ids: &[&str]) {
    h.gui_mut().overlays.apply_fetch_result(
        OverlayFetchResult {
            kind: squallar_source::id::known::NWS_ALERTS,
            data: OverlayRegistry::nws_alerts_payload(ids.iter().map(|id| an_alert(id)).collect()),
        },
        &PaneRef::bare(0),
    );
}

/// **A generation replaced by the next one leaves the frame thread.**
///
/// The first round installs; the second round retires the first, and the
/// frame that follows carries it out. The assertion is the count the drain
/// moved, which is zero on a tree where `install` freed inline.
#[test]
fn the_frames_drain_carries_out_a_replaced_generation() {
    let mut h = InputHarness::new();
    h.gui_mut()
        .enable_overlay_for_test(&squallar_source::id::known::NWS_ALERTS);
    h.warm_up();

    ingest(&mut h, &["urn:one", "urn:two"]);
    h.frame();
    // The first install replaced an empty default, which is a real park of an
    // empty list — so the interesting claim is the SECOND one, where the
    // parked generation carries two alerts' text and geometry.
    let before = h.last_retired();
    assert!(
        before <= 1,
        "one install parks at most one generation, moved {before}",
    );

    ingest(&mut h, &["urn:three"]);
    h.frame();
    assert_eq!(
        h.last_retired(),
        1,
        "the replaced generation must reach the drain; it moved {} payloads",
        h.last_retired(),
    );

    // And the slot is empty afterwards: a drain that handed the same batch
    // out twice would be handing out a dangling promise to free it.
    h.frame();
    assert_eq!(
        h.last_retired(),
        0,
        "a drained slot must stay drained until something else retires",
    );
}

/// The drain is on the **frame**, so it keeps running with no fetch traffic at
/// all — and finds nothing, which is the honest answer and not a failure.
#[test]
fn a_quiet_frame_drains_nothing_and_says_so() {
    let mut h = InputHarness::new();
    h.warm_up();
    h.frame();
    assert_eq!(h.last_retired(), 0);
}

// ── The layer-off drop, and the split that hid it ────────────────────────

use crate::Gui;
use squallar_source::id::LayerId;

const ALERTS: LayerId = squallar_source::id::known::NWS_ALERTS;

/// The production per-pane write — the same call a stack row's eye makes.
fn set_pane_layer(gui: &mut Gui, idx: usize, on: bool) {
    let mut pane = std::mem::take(gui.pane_mut(idx).expect("the pane exists"));
    Gui::write_pane_overlay(&mut gui.overlays, idx, &mut pane, &ALERTS, on);
    *gui.pane_mut(idx).expect("the pane exists") = pane;
}

fn layer_holds_data(h: &mut InputHarness) -> bool {
    h.gui_mut().overlays.has_data(&ALERTS, &PaneRef::bare(0))
}

/// **The over-firing direction, which is the dangerous one.**
///
/// A layer another pane genuinely still draws must keep its data. Dropping it
/// there blanks a live pane, which is far worse than the memory it saves — so
/// this is asserted before the drop is, and over several frames, because the
/// sweep runs on every one of them and only has to be wrong once.
///
/// The sibling is UNLINKED, which is what makes it a real second opinion: a
/// linked one adopts the off-switch inside the same frame (see the test
/// below), so it would not be holding the layer on for long enough to
/// disagree.
#[test]
fn a_layer_another_pane_still_draws_keeps_its_data() {
    let mut h = InputHarness::new();
    h.set_pane_count(2);
    h.gui_mut().enable_overlay_for_test(&ALERTS);
    h.gui_mut().pane_mut(1).expect("a second pane").layer_link = false;
    h.warm_up();
    ingest(&mut h, &["urn:one", "urn:two"]);
    h.frame();
    assert!(layer_holds_data(&mut h), "premise: the round landed");
    assert!(
        h.gui_mut()
            .pane(1)
            .expect("a second pane")
            .is_overlay_enabled(&ALERTS),
        "premise: the sibling draws the layer",
    );

    set_pane_layer(h.gui_mut(), 0, false);
    for frame in 0..5 {
        h.frame();
        assert!(
            h.gui_mut()
                .pane(1)
                .expect("a second pane")
                .is_overlay_enabled(&ALERTS),
            "frame {frame}: the unlinked sibling must keep its own switch",
        );
        assert!(
            layer_holds_data(&mut h),
            "frame {frame}: a pane still draws this layer and its data was \
             dropped; that blanks a live layer",
        );
    }
}

/// **The drop lands once the layer-link fan-out has reached the sibling —
/// which happens inside the same frame as the click, with nothing asking
/// again.**
///
/// This is the trap the whole placement is for. At the moment of the click
/// the predicate answers "a sibling still draws it" and a click-time check
/// correctly declines; the off-switch is then copied onto that sibling
/// wholesale by the frame's own fan-out, and there is no second click to
/// re-ask on. Asking at the END of every frame is what closes it, and both
/// halves are asserted here: the answer before the frame, and the data after
/// it.
#[test]
fn the_drop_lands_after_the_layer_link_fan_out_reaches_the_sibling() {
    let mut h = InputHarness::new();
    h.set_pane_count(2);
    h.gui_mut().enable_overlay_for_test(&ALERTS);
    h.warm_up();
    ingest(&mut h, &["urn:one", "urn:two"]);
    h.frame();
    assert!(layer_holds_data(&mut h));

    set_pane_layer(h.gui_mut(), 0, false);
    assert!(
        h.gui_mut().any_pane_has_overlay_enabled(&ALERTS),
        "at the click the linked sibling still draws it, so a check made HERE \
         would decline — this is the state the old hole was in",
    );

    h.frame();
    assert!(
        !h.gui_mut().any_pane_has_overlay_enabled(&ALERTS),
        "the fan-out reached the sibling during the frame",
    );
    assert!(
        !layer_holds_data(&mut h),
        "no pane draws this layer any more and its round is still resident",
    );
    assert!(
        h.last_retired() >= 1,
        "the released round must reach the discard seam, not be freed here",
    );
}

/// **Switching it back on re-populates it**, which is what makes the drop
/// affordable: interaction is realtime, data may lag.
#[test]
fn switching_the_layer_back_on_asks_for_the_round_again() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&ALERTS);
    h.warm_up();
    ingest(&mut h, &["urn:one"]);
    h.frame();

    set_pane_layer(h.gui_mut(), 0, false);
    h.frame();
    assert!(!layer_holds_data(&mut h), "premise: the round was dropped");

    // The poll clock went with the data, so the layer reads as due rather
    // than waiting out an interval against a round that is gone.
    assert!(
        h.gui_mut()
            .overlays
            .auto_fetch_delay(&ALERTS)
            .is_none_or(|d| d.is_zero()),
        "a layer whose data was dropped must be due for a round, not parked \
         behind the interval its last one stamped",
    );

    // And the way back through the toggle asks for a round of its own.
    set_pane_layer(h.gui_mut(), 0, true);
    h.frames_for(3, 0.05);
    assert!(
        h.gui_mut().overlays.is_fetching(&ALERTS)
            || h.last_actions()
                .iter()
                .any(|a| matches!(a, crate::actions::GuiAction::FetchOverlay { kind, .. } if *kind == ALERTS)),
        "switching the layer back on must ask the origin again, or it stays \
         silently empty",
    );
}
