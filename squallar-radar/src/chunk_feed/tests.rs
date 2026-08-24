use super::*;

fn outcome() -> Result<PollOutcome, String> {
    Ok(PollOutcome {
        ingested: 1,
        ..Default::default()
    })
}

#[test]
fn a_round_in_flight_does_not_take_the_snapshot_with_it() {
    let volume = stub_volume();
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KICT");
    mgr.feeds.get_mut("KICT").expect("ensured").last_snapshot = Some(LiveVolume {
        scan: std::sync::Arc::clone(&volume),
        declared: Default::default(),
    });

    mgr.force_due("KICT");
    let poller = mgr.take_for_round("KICT").expect("the poller leaves");
    let held = mgr
        .snapshot("KICT")
        .expect("the volume vanished for the duration of the round");
    assert!(
        std::sync::Arc::ptr_eq(&held.scan, &volume),
        "the bridge must serve the very volume the last frame resolved",
    );

    mgr.finish_round("KICT", poller, &empty());
    assert!(
        mgr.snapshot("KICT").is_none(),
        "a poller home with no volume yet answers None, and a bridge that \
             never refreshes would overrule it with the stale copy",
    );

    mgr.force_due("KICT");
    let poller = mgr.take_for_round("KICT").expect("the next round leaves");
    assert!(
        mgr.snapshot("KICT").is_none(),
        "the poller-home refresh never reached the bridge, so the round \
             serves a volume no frame has resolved since",
    );
    mgr.finish_round("KICT", poller, &empty());
}

fn empty() -> Result<PollOutcome, String> {
    Ok(PollOutcome::default())
}

fn stub_volume() -> std::sync::Arc<nexrad_model::data::Scan> {
    use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};
    std::sync::Arc::new(Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            Vec::new(),
        ),
        Vec::new(),
    ))
}

#[test]
fn a_retired_feed_serves_no_snapshot() {
    let volume = stub_volume();
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KICT");
    mgr.feeds.get_mut("KICT").expect("ensured").last_snapshot = Some(LiveVolume {
        scan: std::sync::Arc::clone(&volume),
        declared: Default::default(),
    });
    mgr.force_due("KICT");
    let _poller = mgr.take_for_round("KICT").expect("the poller leaves");
    assert!(
        mgr.snapshot("KICT").is_some(),
        "precondition: the feed is serving the volume its flight assembled",
    );

    mgr.force_retire_at("KICT", std::time::Duration::from_secs(1));
    assert!(
        mgr.snapshot("KICT").is_none(),
        "a retired feed kept serving its frozen partial volume, so every \
             consumer merges a dead flight's low tilts over a rolling base",
    );
}

#[test]
fn retirement_drops_the_bridge_copy_and_recovery_starts_fresh() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KICT");
    mgr.feeds.get_mut("KICT").expect("ensured").last_snapshot = Some(LiveVolume {
        scan: stub_volume(),
        declared: Default::default(),
    });

    mgr.force_stall("KICT");
    let poller = take(&mut mgr, "KICT");
    assert_eq!(
        mgr.finish_round("KICT", poller, &empty()),
        Some(Retirement::Stalled),
        "precondition: this is the real retirement path",
    );
    assert!(
        mgr.feeds
            .get("KICT")
            .expect("still present")
            .last_snapshot
            .is_none(),
        "retirement left the bridge copy in hand; the retired gate is then \
             the only thing between it and every consumer",
    );
    assert!(mgr.snapshot("KICT").is_none());

    mgr.force_retire_at("KICT", RETRY_AFTER + std::time::Duration::from_secs(1));
    mgr.ensure("KICT");
    assert!(mgr.is_feeding("KICT"), "the retry window has passed");
    assert!(
        mgr.snapshot("KICT").is_none(),
        "a fresh flight has assembled nothing yet; anything else is the \
             dead flight's volume back from the grave",
    );
    mgr.force_due("KICT");
    assert!(
        mgr.take_for_round("KICT").is_some(),
        "recovery must resume rounds, so the fresh flight's overlay can \
             merge again",
    );
}

fn take(mgr: &mut ChunkFeedManager, site: &str) -> Box<ChunkPoller> {
    mgr.force_due(site);
    mgr.take_for_round(site).expect("a round was available")
}

#[test]
fn one_round_per_site_is_in_flight_at_a_time() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let poller = take(&mut mgr, "KTLX");
    mgr.force_due("KTLX");
    assert!(
        mgr.take_for_round("KTLX").is_none(),
        "a second round was dispatched while the first was still in the air, \
             so the interval is the only thing serialising rounds"
    );
    assert!(mgr.any_in_flight());
    mgr.finish_round("KTLX", poller, &outcome());
    assert!(!mgr.any_in_flight());
}

#[test]
fn an_empty_round_is_not_an_error() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    for _ in 0..10 {
        let poller = take(&mut mgr, "KTLX");
        assert_eq!(mgr.finish_round("KTLX", poller, &empty()), None);
    }
    assert!(mgr.is_feeding("KTLX"));
}

#[test]
fn three_consecutive_errors_retire_a_site_and_two_do_not() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let err = Err("boom".to_string());

    for _ in 0..2 {
        let poller = take(&mut mgr, "KTLX");
        assert_eq!(mgr.finish_round("KTLX", poller, &err), None);
    }
    assert!(mgr.is_feeding("KTLX"), "two failures is not enough");

    let poller = take(&mut mgr, "KTLX");
    assert_eq!(
        mgr.finish_round("KTLX", poller, &err),
        Some(Retirement::Errors)
    );
    assert!(!mgr.is_feeding("KTLX"));
    mgr.force_due("KTLX");
    assert!(
        mgr.take_for_round("KTLX").is_none(),
        "a retired site kept polling"
    );
}

#[test]
fn a_successful_round_clears_the_error_count() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let err = Err("boom".to_string());
    for _ in 0..2 {
        let poller = take(&mut mgr, "KTLX");
        mgr.finish_round("KTLX", poller, &err);
    }
    let poller = take(&mut mgr, "KTLX");
    mgr.finish_round("KTLX", poller, &outcome());
    for _ in 0..2 {
        let poller = take(&mut mgr, "KTLX");
        assert_eq!(mgr.finish_round("KTLX", poller, &err), None);
    }
    assert!(mgr.is_feeding("KTLX"));
}

#[test]
fn a_feed_that_makes_no_progress_retires() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.force_stall("KTLX");
    let poller = take(&mut mgr, "KTLX");
    assert_eq!(
        mgr.finish_round("KTLX", poller, &empty()),
        Some(Retirement::Stalled)
    );
}

#[test]
fn a_retired_site_is_retried_only_after_the_window() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.force_retire_at("KTLX", std::time::Duration::from_secs(60));
    mgr.ensure("KTLX");
    assert!(!mgr.is_feeding("KTLX"), "the retry window has not passed");

    mgr.force_retire_at("KTLX", RETRY_AFTER + std::time::Duration::from_secs(1));
    mgr.ensure("KTLX");
    assert!(mgr.is_feeding("KTLX"));
}

#[test]
fn feeds_for_sites_no_pane_watches_are_dropped() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.ensure("KOUN");
    assert_eq!(mgr.feed_count(), 2);
    mgr.retain_live(&["KTLX".to_string()]);
    assert_eq!(mgr.feed_count(), 1);
    assert!(mgr.is_feeding("KTLX"));
    assert!(!mgr.is_feeding("KOUN"));
}

#[test]
fn a_round_landing_after_its_site_was_dropped_is_discarded() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let poller = take(&mut mgr, "KTLX");
    mgr.retain_live(&[]);
    assert_eq!(mgr.finish_round("KTLX", poller, &outcome()), None);
    assert_eq!(mgr.feed_count(), 0);
}
