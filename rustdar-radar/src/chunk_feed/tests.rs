use super::*;

fn outcome() -> Result<PollOutcome, String> {
    Ok(PollOutcome {
        ingested: 1,
        ..Default::default()
    })
}

/// **The volume must not vanish for the duration of every round.** The
/// poller travels with the round, and before the bridge existed
/// `snapshot` answered `None` for the ~0.1–1 s of every ~5 s poll — so
/// everything resolved through `current::resolve` flapped between the
/// merged volume and the base alone at the poll cadence. Measured live:
/// 65 voxel rebuilds in 5.5 minutes against ~20 sealed sweeps, and the
/// section re-cut key moving per round.
///
/// The tail matters as much as the bridge: when the poller comes home
/// with no volume yet (a fresh feed, pre-first-chunk), the live answer is
/// `None` and the bridge must not overrule it with the stale copy. The
/// poller-home `snapshot` is the bridge's **only writer** — the planted
/// copy is stale residue it must overwrite — so the round dispatched
/// after it is what proves the refresh happened: a bridge that never
/// refreshes serves the residue there, a volume no frame has resolved
/// since.
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
    // The poller-home call: the production refresh takes the live answer
    // (`None` — this poller never ingested a chunk) into the bridge.
    assert!(
        mgr.snapshot("KICT").is_none(),
        "a poller home with no volume yet answers None, and a bridge that \
             never refreshes would overrule it with the stale copy",
    );

    // The next round serves the bridge state that call established.
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

/// The smallest `Scan` the bridge can hold — for fixtures that need a
/// volume in hand without a network to assemble one.
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

/// **The dead flight's frozen volume dies with the feed.** A retired
/// poller keeps its assembler, so without the retired gate `snapshot`
/// goes on serving the partial volume the flight froze on — while the
/// archive polls roll `base_scans` forward underneath, and overlay
/// sweeps supersede base cuts by list order rather than by time. Every
/// consumer of the merged current volume then serves a dead flight's low
/// tilts under a caption whose newest time reads the newer base.
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

/// Retirement along the real path — a stalled feed's round comes home
/// empty — takes the bridge copy with it, so nothing can serve the frozen
/// volume across the retirement. And recovery after the window is a
/// genuinely fresh flight: feeding again, rounds resuming, with nothing
/// left of the dead flight to serve while the new one assembles.
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

    // Recovery after the window is a fresh flight, not the dead one
    // revived.
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

/// Take a round, skipping the real interval — these tests are about the
/// retirement rules, not the clock.
fn take(mgr: &mut ChunkFeedManager, site: &str) -> Box<ChunkPoller> {
    mgr.force_due(site);
    mgr.take_for_round(site).expect("a round was available")
}

/// The first round is available immediately; a second is not, because the
/// poller is out.
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

/// The mutation this kills: counting an empty round as a failure. No new
/// chunk is the ordinary state between cuts, so a feed that is working
/// perfectly would retire after fifteen seconds of it.
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

/// Three consecutive hard failures retire the site; two do not.
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

/// And a success in between clears the count, so intermittent failures never
/// accumulate into a retirement.
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

/// Rounds succeeding but delivering nothing for two minutes is a dead feed,
/// which no error count would ever catch.
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

/// A retirement is not permanent — a CORS blip should not cost the session —
/// but it does not lift early either.
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

/// The feed of a site nothing is watching live holds tens of megabytes of
/// accumulated volume and has no reader.
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

/// A round in flight for a site that was dropped meanwhile must not
/// resurrect it, and must not panic.
#[test]
fn a_round_landing_after_its_site_was_dropped_is_discarded() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let poller = take(&mut mgr, "KTLX");
    mgr.retain_live(&[]);
    assert_eq!(mgr.finish_round("KTLX", poller, &outcome()), None);
    assert_eq!(mgr.feed_count(), 0);
}
