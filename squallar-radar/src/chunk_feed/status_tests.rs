use super::*;

fn live(sites: &[&str]) -> Vec<String> {
    sites.iter().map(|s| (*s).to_string()).collect()
}

/// With the setting off, nothing claims the low-latency path is running.
#[test]
fn the_status_says_nothing_when_the_feed_is_disabled() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let status = mgr.status(&live(&["KTLX"]), false, None);
    assert!(!status.feeding);
    assert!(!status.retired);
}

/// A site with no feed at all reads as plain auto-poll, not as a failure.
#[test]
fn a_site_that_never_had_a_feed_is_not_reported_as_retired() {
    let mgr = ChunkFeedManager::new();
    let status = mgr.status(&live(&["KTLX"]), true, None);
    assert!(!status.feeding);
    assert!(
        !status.retired,
        "a site that was never fed would read as a downgrade that never happened"
    );
}

/// A running feed reports its own cadence, so the label cannot drift from
/// the interval the poller is actually using — including the longer one it
/// backs off to when the radar is between cuts.
#[test]
fn a_running_feed_reports_its_own_interval() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let status = mgr.status(&live(&["KTLX"]), true, None);
    assert!(status.feeding);
    assert_eq!(status.interval_secs, crate::chunks::POLL_INTERVAL.as_secs());
}

/// A retirement is a silent drop from seconds of latency to minutes, so it
/// has to reach the status bar.
#[test]
fn a_retired_feed_is_reported_so_the_downgrade_is_visible() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.force_retire_at("KTLX", std::time::Duration::from_secs(1));
    let status = mgr.status(&live(&["KTLX"]), true, None);
    assert!(status.retired);
    assert!(!status.feeding);
}

/// With two sites on screen and one fed, the bar says the feed is working —
/// the pane showing it is genuinely seconds behind the radar.
#[test]
fn one_fed_site_among_several_still_reads_as_feeding() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.ensure("KOUN");
    mgr.force_retire_at("KOUN", std::time::Duration::from_secs(1));
    let status = mgr.status(&live(&["KTLX", "KOUN"]), true, None);
    assert!(status.feeding);
    assert!(status.retired);
}
