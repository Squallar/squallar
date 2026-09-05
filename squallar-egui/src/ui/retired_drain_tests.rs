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
