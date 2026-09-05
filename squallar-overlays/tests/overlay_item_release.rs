//! **Does a layer switched off actually give the memory back?**
//!
//! Every other assertion about the drop is a figure this workspace maintains
//! — `data_bytes`, the census family, a retired count — and each of them
//! could be right while the allocator still held the bytes. This binary
//! installs the counting global allocator and watches `live_bytes` instead,
//! so the claim is the allocator's own.
//!
//! **What the instrument does not see.** `squallar_alloc` counts REQUESTED
//! sizes: the allocator's own per-block overhead, its reserve and its
//! fragmentation are all outside the figure. So a release that shows up here
//! as N bytes returned gave back at least N; the residual between this and a
//! resident-set reading is unaccounted for rather than absent, and the two
//! are never subtracted from each other.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use squallar_overlays::render::overlay_state::{
    OverlayFetchResult, OverlayRegistry, PaneRef, installed_item_bytes,
};

/// The same installation the shipped binaries make, so the figure below is
/// the real global allocator's and not a stand-in's.
#[global_allocator]
static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;

/// Enough alerts, with enough text and geometry each, that the round is far
/// larger than anything else this binary allocates between the two readings.
const ALERTS: usize = 400;

fn an_alert(index: usize) -> squallar_overlays::nws::alert::NwsAlert {
    let ring: Vec<(f64, f64)> = (0..256)
        .map(|i| {
            let t = i as f64 * std::f64::consts::TAU / 256.0;
            (35.0 + 0.4 * t.sin(), -97.0 + 0.4 * t.cos())
        })
        .collect();
    squallar_overlays::nws::alert::NwsAlert {
        id: format!("urn:oid:2.49.0.1.840.0.{index:08}"),
        event: "Tornado Warning".to_string(),
        category: squallar_overlays::nws::alert::AlertCategory::Warning,
        severity: squallar_overlays::nws::alert::AlertSeverity::Extreme,
        urgency: squallar_overlays::nws::alert::AlertUrgency::Immediate,
        certainty: squallar_overlays::nws::alert::AlertCertainty::Observed,
        headline: Some("A tornado warning is in effect".repeat(4)),
        description: "x".repeat(2048),
        instruction: Some("y".repeat(1024)),
        area_desc: "Cleveland, Oklahoma, McClain".to_string(),
        sender_name: "NWS Norman OK".to_string(),
        effective: "2026-05-20T19:56:00-05:00".to_string(),
        expires: "2026-05-20T20:30:00-05:00".to_string(),
        onset: None,
        ends: None,
        valid_from: None,
        valid_until: None,
        affected_zones: Vec::new(),
        features: Arc::new(vec![squallar_overlays::types::OverlayFeature::new(
            vec![vec![ring]],
            [255, 0, 0, 80],
            [255, 0, 0, 255],
            "Tornado Warning".to_string(),
            String::new(),
            squallar_overlays::types::HatchPattern::None,
        )]),
    }
}

/// **The released round leaves the allocator**, watched at the allocator.
///
/// One test rather than several: the counters are process-global and the
/// harness runs tests on several threads, so a second test moving the same
/// statics would race this one's arithmetic.
#[test]
fn a_released_round_gives_its_bytes_back_to_the_allocator() {
    let mut registry = OverlayRegistry::default();
    let id = squallar_source::id::known::NWS_ALERTS;

    let quiet = squallar_alloc::live_bytes().expect("this binary installed the counter");
    registry.apply_fetch_result(
        OverlayFetchResult {
            kind: id.clone(),
            data: OverlayRegistry::nws_alerts_payload((0..ALERTS).map(an_alert).collect()),
        },
        &PaneRef::across(&[]),
    );
    let priced = installed_item_bytes();
    let held = squallar_alloc::live_bytes().expect("still counting");
    assert!(
        held > quiet,
        "the round must have raised live bytes: {quiet} -> {held}",
    );
    assert!(
        priced > 0,
        "and the census family must have priced it: {priced} B",
    );

    // Switch it off everywhere. Nothing this test built is a pane, so no pane
    // draws the layer and this is exactly the production predicate's answer.
    let handler = registry
        .get_handler_mut(&id)
        .expect("the alerts layer is registered");
    assert!(handler.release_data(), "there was a round to release");
    let retired = handler.take_retired();
    assert_eq!(retired.len(), 1, "the round is parked, not freed inline");
    assert!(
        squallar_alloc::live_bytes().expect("still counting") >= held,
        "a PARKED round is still resident; the seam defers the free, it does \
         not perform it",
    );

    // The app hands the batch to the discard pool; here, dropping it is the
    // same free.
    drop(retired);
    let after = squallar_alloc::live_bytes().expect("still counting");
    assert!(
        after < held,
        "the released round never reached the allocator: live bytes went \
         {quiet} -> {held} -> {after}",
    );
    // The released bytes are the census family's own figure, so the two
    // instruments have to agree on the direction and the order of magnitude.
    // `priced` counts requested sizes and so does the allocator, but the
    // allocator also carries everything else this binary did between the
    // readings, so the claim is a bound and not an equality.
    let returned = held - after;
    eprintln!(
        "overlay item release: {ALERTS} alerts, census priced {priced} B, \
         live bytes {quiet} -> {held} -> {after} ({returned} B returned)",
    );
    // **Two instruments, agreeing.** The census prices what the layer
    // installed; the allocator counts what it granted and took back. Both
    // count REQUESTED sizes, so they share a blind spot — per-block allocator
    // overhead is in neither — but they are otherwise independent, and on
    // this fixture they agree to the byte (3 343 600 B each).
    assert!(
        returned >= priced,
        "the allocator gave back {returned} B where the census family priced \
         the round at {priced} B; one of the two is wrong",
    );
    assert_eq!(
        installed_item_bytes(),
        0,
        "and the family must report the layer as holding nothing",
    );
}
