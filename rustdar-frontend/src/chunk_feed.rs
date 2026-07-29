//! Per-site real-time chunk feeds, and the rules for retiring one.
//!
//! Modelled on [`crate::loop_downloads::LoopDownloadManager`]: a plain state
//! container owned by `App`, with no network of its own. `App` drives the
//! rounds; this decides which sites still want one and when to give up on a
//! feed and let the archive path take over.

use std::collections::HashMap;

use rustdar_radar::chunks::{ChunkPoller, PollOutcome, VolumeIndex};

/// Consecutive failed rounds before a site falls back to the archive.
///
/// An *empty* round is not a failure — no new chunk is the ordinary state
/// between cuts and across the gap between volumes, and counting it would
/// retire a feed that is working perfectly.
pub const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// How long a feed may make no progress at all before it is retired.
///
/// Longer than any inter-cut or inter-volume gap in any VCP — the slowest
/// clear-air patterns take about ten minutes for a whole volume and still
/// deliver a chunk every few tens of seconds — so two minutes of complete
/// silence means the site or the feed is down rather than merely quiet.
pub const STALL: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a retired site waits before chunks are tried again. A CORS blip or
/// a brief outage should not cost the rest of the session.
pub const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(600);

/// Why a site stopped using the chunk feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retirement {
    /// Repeated hard failures — network, CORS, S3, a listing that would not parse.
    Errors,
    /// Rounds kept succeeding but nothing ever arrived.
    Stalled,
}

/// One site's feed.
pub struct SiteFeed {
    /// `None` only while a round is in flight — the poller travels with the
    /// request and comes back on the response, because it owns the assembled
    /// volume and a detached task cannot borrow it out of `App`.
    poller: Option<Box<ChunkPoller>>,
    in_flight: bool,
    consecutive_errors: u32,
    last_progress: web_time::Instant,
    last_poll: Option<web_time::Instant>,
    /// The volume index this site last worked on, so a feed rebuilt after a
    /// site switch and back can skip the ~10-request discovery search.
    last_volume: Option<VolumeIndex>,
    retired: Option<(Retirement, web_time::Instant)>,
}

impl SiteFeed {
    fn new(site: &str, resume_from: Option<VolumeIndex>) -> Self {
        let poller = match resume_from {
            Some(volume) => rustdar_radar::scan::resume_chunk_poller(site, volume),
            None => rustdar_radar::scan::chunk_poller(site),
        };
        Self {
            poller: Some(Box::new(poller)),
            in_flight: false,
            consecutive_errors: 0,
            last_progress: web_time::Instant::now(),
            last_poll: None,
            last_volume: resume_from,
            retired: None,
        }
    }

    /// Whether this site should dispatch a round now.
    ///
    /// One round per site at a time, deliberately **not** interlocked on the
    /// global `RadarState::fetching`. That flag drives the status-bar spinner and
    /// gates the archive poll; a five-second cadence on it would strobe the bar
    /// and suppress the very fallback this feed may need.
    fn should_poll(&self, now: web_time::Instant) -> bool {
        if self.in_flight || self.retired.is_some() || self.poller.is_none() {
            return false;
        }
        let Some(poller) = &self.poller else {
            return false;
        };
        match self.last_poll {
            None => true,
            Some(last) => now.duration_since(last) >= poller.suggested_interval(),
        }
    }
}

/// Every site being fed from the real-time bucket.
#[derive(Default)]
pub struct ChunkFeedManager {
    feeds: HashMap<String, SiteFeed>,
}

impl ChunkFeedManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sites with a round in flight, for the redraw re-arm.
    pub fn any_in_flight(&self) -> bool {
        self.feeds.values().any(|f| f.in_flight)
    }

    /// Whether this site is currently fed by chunks — the test
    /// `check_auto_polls` uses to decide between a chunk round and the 60 s
    /// archive check.
    pub fn is_feeding(&self, site: &str) -> bool {
        self.feeds
            .get(site)
            .is_some_and(|f| f.retired.is_none() && f.poller.is_some())
    }

    /// Start a feed for a site, or clear a retirement whose retry window has
    /// passed. Idempotent.
    pub fn ensure(&mut self, site: &str) {
        let now = web_time::Instant::now();
        match self.feeds.get_mut(site) {
            None => {
                self.feeds
                    .insert(site.to_string(), SiteFeed::new(site, None));
            }
            Some(feed) => {
                if let Some((_, at)) = feed.retired
                    && now.duration_since(at) >= RETRY_AFTER
                {
                    let resume = feed.last_volume;
                    *feed = SiteFeed::new(site, resume);
                }
            }
        }
    }

    /// Take the poller for a round, if this site wants one now.
    ///
    /// Hands ownership out; [`Self::finish_round`] must put it back or the site
    /// stops feeding.
    pub fn take_for_round(&mut self, site: &str) -> Option<Box<ChunkPoller>> {
        let now = web_time::Instant::now();
        let feed = self.feeds.get_mut(site)?;
        if !feed.should_poll(now) {
            return None;
        }
        feed.last_poll = Some(now);
        feed.in_flight = true;
        feed.poller.take()
    }

    /// Put the poller back and fold in what the round did.
    ///
    /// Returns a retirement when this round exhausted the site's patience, so
    /// the caller can hand the site back to the archive path.
    pub fn finish_round(
        &mut self,
        site: &str,
        poller: Box<ChunkPoller>,
        result: &Result<PollOutcome, String>,
    ) -> Option<Retirement> {
        let now = web_time::Instant::now();
        let Some(feed) = self.feeds.get_mut(site) else {
            // The site was dropped while the round was in the air; the poller
            // goes with it.
            return None;
        };
        feed.last_volume = poller.volume();
        feed.poller = Some(poller);
        feed.in_flight = false;

        match result {
            Ok(outcome) => {
                feed.consecutive_errors = 0;
                if outcome.ingested > 0 || outcome.rolled_to.is_some() {
                    feed.last_progress = now;
                }
            }
            Err(_) => feed.consecutive_errors += 1,
        }

        let retirement = if feed.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
            Some(Retirement::Errors)
        } else if now.duration_since(feed.last_progress) >= STALL {
            Some(Retirement::Stalled)
        } else {
            None
        };
        if let Some(reason) = retirement {
            log::warn!("{site}: retiring the chunk feed ({reason:?}); falling back to the archive");
            feed.retired = Some((reason, now));
        }
        retirement
    }

    /// A one-line summary of what the feed is doing across the sites on screen,
    /// for the status bar.
    ///
    /// `retired` is reported only for a site that *was* being fed and is not
    /// any more, so a site that never had a feed reads as plain auto-poll rather
    /// than as a failure.
    pub fn status(&self, live_sites: &[String], enabled: bool) -> rustdar_egui::ChunkFeedStatus {
        let mut status = rustdar_egui::ChunkFeedStatus {
            interval_secs: rustdar_radar::chunks::POLL_INTERVAL.as_secs(),
            ..Default::default()
        };
        if !enabled {
            return status;
        }
        for site in live_sites {
            let Some(feed) = self.feeds.get(site) else {
                continue;
            };
            if feed.retired.is_some() {
                status.retired = true;
                continue;
            }
            status.feeding = true;
            if let Some(poller) = &feed.poller {
                status.interval_secs = poller.suggested_interval().as_secs();
                status.cuts_this_volume = poller
                    .progress()
                    .map(|p| p.sealed_elevations.len())
                    .unwrap_or(0);
            }
        }
        status
    }

    /// The volume so far for a site, complete sweeps only.
    pub fn snapshot(&mut self, site: &str) -> Option<std::sync::Arc<nexrad_model::data::Scan>> {
        self.feeds
            .get_mut(site)?
            .poller
            .as_mut()
            .and_then(|p| p.snapshot())
    }

    /// Drop the feeds of sites nothing is watching live.
    ///
    /// Narrower than `evict_unshown_scans` on purpose. That pass retains the
    /// union of `pane.site` and `pane.scan_info.site.name`, keeping a volume
    /// alive under the name a switching pane's `scan_info` still carries because
    /// `dispatch_pane_renders` looks it up there. A *feed* has no such reader:
    /// the moment no pane is live on the site, nothing wants another chunk and
    /// the tens of megabytes of accumulated volume it holds are dead. The
    /// retained set is exactly the set `check_auto_polls` will ask for a round
    /// for.
    ///
    /// A round in flight for a dropped site is not a leak — the poller travels
    /// on the response and is dropped by [`Self::finish_round`] when it finds no
    /// feed to put it back into.
    pub fn retain_live(&mut self, live_sites: &[String]) {
        self.feeds
            .retain(|site, _| live_sites.iter().any(|s| s == site));
    }

    #[cfg(test)]
    pub(crate) fn feed_count(&self) -> usize {
        self.feeds.len()
    }

    /// Make a site due for a round now, so a test can run several without
    /// waiting out the real five-second interval.
    #[cfg(test)]
    pub(crate) fn force_due(&mut self, site: &str) {
        if let Some(feed) = self.feeds.get_mut(site) {
            feed.last_poll = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn force_stall(&mut self, site: &str) {
        if let Some(feed) = self.feeds.get_mut(site) {
            feed.last_progress =
                web_time::Instant::now() - STALL - std::time::Duration::from_secs(1);
        }
    }

    #[cfg(test)]
    pub(crate) fn force_retire_at(&mut self, site: &str, ago: std::time::Duration) {
        if let Some(feed) = self.feeds.get_mut(site) {
            feed.retired = Some((Retirement::Errors, web_time::Instant::now() - ago));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome() -> Result<PollOutcome, String> {
        Ok(PollOutcome {
            ingested: 1,
            ..Default::default()
        })
    }

    fn empty() -> Result<PollOutcome, String> {
        Ok(PollOutcome::default())
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
}

#[cfg(test)]
mod status_tests {
    use super::*;

    fn live(sites: &[&str]) -> Vec<String> {
        sites.iter().map(|s| (*s).to_string()).collect()
    }

    /// With the setting off, nothing claims the low-latency path is running.
    #[test]
    fn the_status_says_nothing_when_the_feed_is_disabled() {
        let mut mgr = ChunkFeedManager::new();
        mgr.ensure("KTLX");
        let status = mgr.status(&live(&["KTLX"]), false);
        assert!(!status.feeding);
        assert!(!status.retired);
    }

    /// A site with no feed at all reads as plain auto-poll, not as a failure.
    #[test]
    fn a_site_that_never_had_a_feed_is_not_reported_as_retired() {
        let mgr = ChunkFeedManager::new();
        let status = mgr.status(&live(&["KTLX"]), true);
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
        let status = mgr.status(&live(&["KTLX"]), true);
        assert!(status.feeding);
        assert_eq!(
            status.interval_secs,
            rustdar_radar::chunks::POLL_INTERVAL.as_secs()
        );
    }

    /// A retirement is a silent drop from seconds of latency to minutes, so it
    /// has to reach the status bar.
    #[test]
    fn a_retired_feed_is_reported_so_the_downgrade_is_visible() {
        let mut mgr = ChunkFeedManager::new();
        mgr.ensure("KTLX");
        mgr.force_retire_at("KTLX", std::time::Duration::from_secs(1));
        let status = mgr.status(&live(&["KTLX"]), true);
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
        let status = mgr.status(&live(&["KTLX", "KOUN"]), true);
        assert!(status.feeding);
        assert!(status.retired);
    }
}
