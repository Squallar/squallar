use super::*;

/// What a push notification does: the chunk exists now, so the remainder of
/// the poll interval is latency for nothing.
#[test]
fn marking_a_site_due_lets_it_poll_before_the_interval() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let poller = mgr.take_for_round("KTLX").expect("the first round is due");
    mgr.finish_round("KTLX", poller, &Ok(PollOutcome::default()));

    assert!(
        mgr.take_for_round("KTLX").is_none(),
        "precondition: the interval has not elapsed"
    );
    mgr.mark_due("KTLX");
    assert!(
        mgr.take_for_round("KTLX").is_some(),
        "a notification did not bring the next round forward"
    );
}

/// It must not start a second concurrent round — a burst of notifications
/// for one volume would otherwise dispatch one round per message.
#[test]
fn marking_a_site_due_does_not_interrupt_a_round_in_flight() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let poller = mgr.take_for_round("KTLX").expect("the first round is due");

    for _ in 0..5 {
        mgr.mark_due("KTLX");
        assert!(
            mgr.take_for_round("KTLX").is_none(),
            "a notification dispatched a round while one was still in flight"
        );
    }
    mgr.finish_round("KTLX", poller, &Ok(PollOutcome::default()));
}

/// A notification for a site with no feed is inert rather than a panic —
/// the socket can outlive the feed by a frame.
#[test]
fn marking_an_unknown_site_due_is_inert() {
    let mut mgr = ChunkFeedManager::new();
    mgr.mark_due("KTLX");
    assert!(!mgr.is_feeding("KTLX"));
}

/// A retired site stays retired: notifications are an accelerator for the
/// polling feed, not a way around its failure handling.
#[test]
fn a_notification_does_not_revive_a_retired_feed() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.force_retire_at("KTLX", std::time::Duration::from_secs(1));
    mgr.mark_due("KTLX");
    assert!(mgr.take_for_round("KTLX").is_none());
}
