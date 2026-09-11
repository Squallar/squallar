//! **Archive delivery is addressed, not broadcast.**
//!
//! `UNLINK_NOTE` promises that a pane parked in the archive "holds its
//! moment". These are the pins that make that true of the pixels as well as
//! of the prose: an archive volume one pane asked for reaches that pane and
//! the panes that share its time, and nobody else.

use super::App;
use super::tests::{empty_scan, two_pane_app};
use crate::channels::{FetchRequester, ScanData};

const SITE: &str = "KTLX";

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

/// Two same-site panes, both parked in the archive at `at(0)`, pane 1
/// unlinked in time.
fn two_parked_panes() -> App {
    let mut app = two_pane_app(SITE, SITE);
    for idx in 0..2 {
        let pane = app.gui.pane_mut(idx).expect("the fixture built two panes");
        pane.viewing_live = false;
        pane.scan_info = Some(squallar_radar::types::ScanInfo::from_scan(
            &empty_scan(),
            SITE,
            at(0),
            None,
        ));
    }
    app.gui.pane_mut(1).expect("pane 1").time_link = false;
    app
}

fn shown_at(app: &App, idx: usize) -> chrono::NaiveDateTime {
    app.gui
        .pane(idx)
        .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp))
        .unwrap_or_else(|| panic!("pane {idx} must still hold scan info"))
}

/// Ask for an archive volume on `pane_idx`'s behalf, exactly as
/// `handle_navigate_time` does, and deliver the reply collected at
/// `collected`. The generation goes through the production counter so the
/// staleness rule is the real one.
fn scrub(app: &mut App, pane_idx: usize, collected: chrono::NaiveDateTime) {
    let requester = FetchRequester::Pane(pane_idx);
    let generation = app.render.next_scan_generation(SITE, requester);
    deliver(app, requester, generation, collected);
    poll_frames(app, 1);
}

fn deliver(
    app: &App,
    requester: FetchRequester,
    generation: u64,
    collected: chrono::NaiveDateTime,
) {
    app.channels
        .scan_sender
        .send(crate::channels::ScanResponse {
            generation,
            site: SITE.to_string(),
            requester,
            result: Ok(ScanData {
                scan: empty_scan(),
                declared_nyquist: Default::default(),
                site: SITE.to_string(),
                timestamp: collected,
                archive: Some(std::sync::Arc::new(vec![0u8; 64])),
            }),
            is_auto_poll: false,
        })
        .expect("the channel is open");
}

/// Run `frames` frames of arrival ingest.
///
/// **One `poll_data_channels` is one FRAME, not one queue flush.** Its ingest
/// is bounded by `INGEST_BUDGET_PER_FRAME` — 1 ms on desktop — and after the
/// first arrival a frame that has spent it leaves the rest queued for the next
/// one. A fixture that sends two replies and pumps once is therefore asserting
/// against whichever prefix that single frame happened to reach, which is a
/// clock and not a rule: measured here, one arrival costs 66 µs at p50 on an
/// idle box, so the assertion holds by a 15x margin until the box is busy and
/// then does not. Every arrival is taken by some frame, and each call takes at
/// least one, so `frames` calls take `frames` arrivals.
fn poll_frames(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.poll_data_channels();
    }
}

/// Assert a reply was observed at all before asserting WHICH one won.
///
/// `assert_eq!(shown, at(12))` fails identically in shape for two unrelated
/// defects — `at(6)`, the abandoned reply winning a supersession race, and
/// `at(0)`, the moment `two_parked_panes` parked the pane at, which means no
/// reply landed at all. One assertion, two mechanisms, and a bare timestamp
/// either way. This splits them so the failure names the one it caught.
fn assert_a_reply_landed(app: &App, idx: usize) {
    assert_ne!(
        shown_at(app, idx),
        at(0),
        "pane {idx} still holds the moment the fixture parked it at, so no \
         reply was observed at all — this is a delivery that never landed, \
         not a supersession that picked the wrong winner",
    );
}

/// **The payoff pin.** Pane 0 scrubs; the time-unlinked sibling on the same
/// site keeps the moment it was parked at.
#[test]
fn an_unlinked_pane_holds_its_moment_while_a_same_site_sibling_scrubs() {
    let mut app = two_parked_panes();
    assert!(
        !app.gui.pane(1).expect("pane 1").time_link,
        "precondition: pane 1 must be out of the shared time group, or the \
         gate below is asserting nothing",
    );

    scrub(&mut app, 0, at(6));

    assert_eq!(
        shown_at(&app, 0),
        at(6),
        "the pane that asked never got what it asked for, so nothing below is \
         about the gate",
    );
    assert_eq!(
        shown_at(&app, 1),
        at(0),
        "the unlinked pane was dragged to the sibling's moment. `UNLINK_NOTE` \
         says it holds its own, and the render reads exactly this timestamp to \
         look its volume up.",
    );
}

/// The other half of the same rule: a pane that **is** in the requester's time
/// group still takes the delivery.
#[test]
fn a_time_linked_sibling_still_takes_the_archive_delivery() {
    let mut app = two_parked_panes();
    app.gui.pane_mut(1).expect("pane 1").time_link = true;

    scrub(&mut app, 0, at(6));

    assert_eq!(
        shown_at(&app, 1),
        at(6),
        "gating the delivery on the requester must not cost the linked group \
         its shared clock",
    );
}

/// **W28's half, and the reason the re-keyed volume store was not enough.**
///
/// Both panes scrub. The second navigation used to bump the site's one
/// generation counter and so throw the first pane's reply away as stale — the
/// store could hold two volumes for a site and the fetch still cancelled
/// itself.
#[test]
fn two_same_site_panes_can_each_have_a_fetch_in_flight() {
    let mut app = two_parked_panes();

    // A asks, then B asks, and only then do the replies land — the ordering
    // that made A's reply stale.
    let a = FetchRequester::Pane(0);
    let b = FetchRequester::Pane(1);
    let a_generation = app.render.next_scan_generation(SITE, a);
    let b_generation = app.render.next_scan_generation(SITE, b);
    assert!(
        b_generation > a_generation,
        "precondition: the site counter must still advance across requesters, \
         or the stale rule this pins is not being exercised",
    );

    deliver(&app, a, a_generation, at(6));
    deliver(&app, b, b_generation, at(12));
    poll_frames(&mut app, 2);

    assert_a_reply_landed(&app, 0);
    assert_a_reply_landed(&app, 1);
    assert_eq!(
        shown_at(&app, 0),
        at(6),
        "pane 0's reply was discarded because pane 1 asked afterwards",
    );
    assert_eq!(shown_at(&app, 1), at(12), "pane 1's own reply must land");
}

/// A pane's **own** next request still supersedes its last one. Keying the
/// counter on the timestamp instead of the requester would have lost this,
/// and with it the counter's whole purpose.
#[test]
fn a_panes_own_re_request_still_supersedes_the_one_it_abandoned() {
    let mut app = two_parked_panes();

    let a = FetchRequester::Pane(0);
    let abandoned = app.render.next_scan_generation(SITE, a);
    let current = app.render.next_scan_generation(SITE, a);

    deliver(&app, a, abandoned, at(6));
    deliver(&app, a, current, at(12));
    poll_frames(&mut app, 2);

    assert_a_reply_landed(&app, 0);
    assert_eq!(
        shown_at(&app, 0),
        at(12),
        "the abandoned reply landed after the one that superseded it, so the \
         pane shows a moment the user has already navigated away from",
    );
}

/// **The stale-index hazard W11 names, on the one store W11 could not see.**
///
/// A fetch is in flight for pane 1 when pane 1 is closed. `PaneId` is
/// positional, so the reply must not be delivered to whichever pane now holds
/// index 1 — and here it must not be delivered at all.
#[test]
fn a_fetch_in_flight_for_a_closed_pane_lands_on_nobody() {
    use squallar_egui::actions::GuiAction;

    let mut app = two_parked_panes();
    // Pane 1 is linked here on purpose: an unlinked pane would be excluded by
    // the time-group gate alone and the invalidation would not be under test.
    app.gui.pane_mut(1).expect("pane 1").time_link = true;

    let b = FetchRequester::Pane(1);
    let generation = app.render.next_scan_generation(SITE, b);

    // What `Gui::close_pane` emits once the slot is gone. Driven through the
    // action wire rather than by poking the store, so the pin covers the
    // handler as well as the invalidation.
    app.handle_gui_action(GuiAction::PaneClosed { pane_idx: 1 }, None);

    deliver(&app, b, generation, at(12));
    poll_frames(&mut app, 1);

    assert_eq!(
        shown_at(&app, 0),
        at(0),
        "a fetch spawned for a pane that has since been closed landed on the \
         pane that took its index",
    );
}

/// **An arrival the frame's budget cannot afford is DEFERRED, not dropped.**
///
/// `INGEST_BUDGET_PER_FRAME`'s contract is that what a frame cannot apply
/// "stays queued and the window is asked for another frame". The drain read
/// that budget *after* `try_recv_arrival` had already taken the message, so
/// the arrival that crossed the boundary was destroyed at the `break`: never
/// applied, never re-sent, and silent, because the only thing the drain logged
/// was for the staleness path. The pane kept the moment it was parked at and
/// no later frame could recover it — there was nothing left in the channel to
/// recover.
///
/// Driven with a deadline that is already spent rather than by loading the
/// box: the window in which a real stall does this is one arrival wide (66 µs
/// at p50, against a 1 ms budget), which is why it took a full-workspace run
/// to find and 33 later runs to not find again.
#[test]
fn an_arrival_the_budget_cannot_afford_is_deferred_rather_than_dropped() {
    let mut app = two_parked_panes();
    let a = FetchRequester::Pane(0);
    let abandoned = app.render.next_scan_generation(SITE, a);
    let current = app.render.next_scan_generation(SITE, a);

    deliver(&app, a, abandoned, at(6));
    deliver(&app, a, current, at(12));

    // A frame whose budget is spent the moment it opens: `ingest_budget_spent`
    // is `now >= deadline`. One arrival always goes through, so this frame
    // takes the abandoned reply — and discards it, correctly, as stale.
    app.ingest_deadline = Some(web_time::Instant::now());
    app.run_frame_pump(super::frame_pump::PumpPhase::Ingest, None);
    app.ingest_deadline = None;
    assert_eq!(
        shown_at(&app, 0),
        at(0),
        "the spent-budget frame applied more than the one arrival it is \
         guaranteed, so what this pins below is not the boundary",
    );

    // The next frame, with a budget of its own.
    poll_frames(&mut app, 1);
    assert_a_reply_landed(&app, 0);
    assert_eq!(
        shown_at(&app, 0),
        at(12),
        "the reply the spent frame could not afford was taken off the channel \
         and dropped instead of being left on it, so the pane holds a moment \
         the user navigated away from until something unrelated refetches",
    );
}
