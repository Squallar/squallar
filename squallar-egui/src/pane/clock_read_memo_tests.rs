//! **The clock read adds no resolver work to the layer walk.**
//!
//! [`PaneState::time_mode`] is read once per layer per pane per frame — by
//! `PaneView::layer`, which hands every handler its depicted instant, and by the
//! cache token's as-of term. On a live pane whose clock is a running loop's
//! playhead the read must ask whether the transport still runs, and asking it
//! through the slot stack's resolver, between two per-layer lookups, would
//! replace the resolver's one-entry memo on every layer: every lookup after it a
//! scan. The read asks through a validated position hint instead.
//!
//! Measured with the resolver's own always-on counters (`slot_ledger`: lookups,
//! fingerprints scanned, full compares), thread-local, so no other test's walk
//! reaches these figures. **The control is the same walk on a live pane over a
//! live clock**, which never asks the transport anything: the loop's pane must
//! cost exactly what it costs.

use super::layer_stack::slot_ledger;
use super::*;
use squallar_source::id::known;

fn walked_layers() -> [LayerId; 6] {
    [
        known::RADAR,
        known::NWS_ALERTS,
        known::SPC_DISCUSSIONS,
        known::MRMS,
        known::GMGSI,
        known::LIGHTNING,
    ]
}

fn pane_with_layers() -> PaneState {
    let mut pane = PaneState::with_site("KTLX".to_string());
    for id in walked_layers() {
        pane.set_overlay_enabled(id, true);
    }
    pane
}

/// The per-layer reads the frame's walk makes of one pane — the handler's view,
/// which carries the depicted instant, then the slot — three frames of it.
fn walk(pane: &PaneState) -> (u64, u64, u64) {
    slot_ledger::reset();
    for _ in 0..3 {
        let view = pane.view(0);
        for id in walked_layers() {
            std::hint::black_box(view.layer(&id).as_of);
            std::hint::black_box(pane.slot(&id).is_some());
        }
    }
    slot_ledger::read()
}

#[test]
fn a_live_loops_clock_read_adds_no_resolver_work_to_the_layer_walk() {
    let at = chrono::NaiveDate::from_ymd_opt(2026, 9, 12)
        .and_then(|d| d.and_hms_opt(12, 0, 0))
        .expect("a real instant");

    let live = pane_with_layers();
    let mut looping = pane_with_layers();
    looping.begin_or_continue_loop();
    *looping.transport_state_mut() =
        LayerTimeState::begin(3600, RenderView::PlanView, Box::new(()));
    looping.set_time_mode(TimeMode::AsOf(at));
    assert!(looping.viewing_live(), "premise: the pane follows live");
    assert_eq!(
        looping.time_mode(),
        TimeMode::AsOf(at),
        "premise: its clock is the running loop's playhead, so every read asks \
         whether the transport still runs"
    );

    // Warm both: the first walk builds the resolver's fingerprints.
    let _ = walk(&live);
    let _ = walk(&looping);
    let control = walk(&live);
    let with_loop = walk(&looping);

    assert!(
        control.0 > 0,
        "floor: the walk resolved slots at all, or equality proves nothing"
    );
    assert_eq!(
        with_loop, control,
        "(lookups, fingerprints scanned, compares): the loop's clock read added \
         resolver work to the layer walk — the transport question is going \
         through the one-entry memo the per-layer lookups depend on",
    );
}
