use super::*;

fn live(sites: &[&str]) -> Vec<String> {
    sites.iter().map(|s| (*s).to_string()).collect()
}

/// The label is about the tilt on screen, so a tilt the feed has never
/// delivered reports nothing rather than borrowing another tilt's age —
/// which would claim the upper tilt was seconds old when it is a volume
/// behind.
#[test]
fn a_tilt_the_feed_has_not_delivered_has_no_age() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.record_delivery("KTLX", 0.5, std::time::Duration::from_secs(4));

    assert!(
        mgr.status(&live(&["KTLX"]), true, Some(("KTLX", 4.0)))
            .tilt
            .is_none(),
        "the 4.0° pane borrowed the 0.5° cut's freshness"
    );
    let shown = mgr
        .status(&live(&["KTLX"]), true, Some(("KTLX", 0.5)))
        .tilt
        .expect("the delivered tilt reports an age");
    assert_eq!(shown.elevation, 0.5);
    assert!(
        shown.data_age_secs >= 4,
        "the age must include how old the data already was when it arrived, \
             not just the wall clock since"
    );
}

/// Angles that round to the same tenth are the same tilt — the rounding
/// `ScanInfo` and the render cache already use — so a sweep whose radials
/// report 0.54° answers for a pane snapped to 0.5°.
#[test]
fn a_tilt_is_matched_on_the_rounded_angle() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.record_delivery("KTLX", 0.54, std::time::Duration::from_secs(1));
    assert!(
        mgr.status(&live(&["KTLX"]), true, Some(("KTLX", 0.5)))
            .tilt
            .is_some(),
        "a tilt reported at its achieved angle did not match the snapped one"
    );
}

/// One site's freshness never answers for another's.
#[test]
fn freshness_is_per_site() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.record_delivery("KTLX", 0.5, std::time::Duration::from_secs(1));
    assert!(mgr.freshness("KOUN", 0.5).is_none());
}

/// Freshness for a site nothing watches goes with the feed.
#[test]
fn dropping_a_feed_drops_its_recorded_freshness() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.record_delivery("KTLX", 0.5, std::time::Duration::from_secs(1));
    mgr.retain_live(&[]);
    assert!(mgr.freshness("KTLX", 0.5).is_none());
}
