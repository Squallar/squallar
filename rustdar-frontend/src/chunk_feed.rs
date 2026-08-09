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
    /// The last snapshot the poller handed out, bridging the window the
    /// poller is away on a round.
    ///
    /// Without it, [`ChunkFeedManager::snapshot`] answered `None` for the
    /// ~0.1–1 s of every ~5 s round — and everything resolved through
    /// `current::resolve` flapped between the merged volume and the base
    /// alone at the poll cadence. Measured live before the fix: 65 voxel
    /// rebuilds in 5.5 minutes against ~20 sealed sweeps, every extra one a
    /// full worker resample of a picture that had not changed, and the
    /// section re-cut key moving per *round* rather than per rung change —
    /// exactly the waste its fingerprint exists to prevent. An `Arc` clone
    /// of the assembler's own cached snapshot, so the bridge costs a
    /// refcount, not a copy.
    last_snapshot: Option<std::sync::Arc<nexrad_model::data::Scan>>,
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
            last_snapshot: None,
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

/// Elevation in tenths of a degree, so two angles that round to the same tilt
/// share a key — the same rounding `render_dispatch` and `ScanInfo` use.
fn elevation_tenths(elevation: f32) -> i32 {
    (elevation * 10.0).round() as i32
}

/// When a tilt was last delivered, and how old its data was at that moment.
///
/// Recorded on apply rather than recomputed each frame: the age now is that
/// number plus the wall clock since, which is exact and O(1). Rescanning the
/// sweep's radials for their newest timestamp every frame would be hundreds of
/// iterations per frame for a value that only changes when a cut lands.
#[derive(Debug, Clone, Copy)]
struct Delivered {
    age_at_apply: std::time::Duration,
    at: web_time::Instant,
}

/// Every site being fed from the real-time bucket.
#[derive(Default)]
pub struct ChunkFeedManager {
    feeds: HashMap<String, SiteFeed>,
    /// Keyed by site and elevation in tenths of a degree, matching
    /// `render_dispatch`'s cache key.
    delivered: HashMap<(String, i32), Delivered>,
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

    /// Make a site due for a round immediately, skipping the interval.
    ///
    /// What a push notification does: the chunk exists *now*, so waiting out the
    /// remainder of the poll interval is latency for nothing. Everything else
    /// about the round is unchanged, which is why a notifier that goes away costs
    /// nothing — the timer is still there underneath.
    ///
    /// Does not disturb a round already in flight; `should_poll` still refuses
    /// while the poller is out.
    pub fn mark_due(&mut self, site: &str) {
        if let Some(feed) = self.feeds.get_mut(site) {
            feed.last_poll = None;
        }
    }

    /// Tell a site's feed which cuts to download.
    ///
    /// Applied to the poller, so it survives a volume roll. Ignored while a
    /// round is in flight — the poller is out — and picked up on the next one,
    /// which is a frame's delay at worst.
    pub fn set_selection(&mut self, site: &str, selection: rustdar_radar::chunks::CutSelection) {
        if let Some(feed) = self.feeds.get_mut(site)
            && let Some(poller) = feed.poller.as_mut()
        {
            poller.set_selection(selection);
        }
    }

    /// Take the poller regardless of the interval, for a notification-driven
    /// fetch.
    ///
    /// Still refuses while a round is in flight — that is the part that matters,
    /// since a burst of notifications for one volume would otherwise start a
    /// fetch per message. The interval is skipped because a notification means
    /// the object exists *now*, which is the whole point.
    pub fn take_now(&mut self, site: &str) -> Option<Box<ChunkPoller>> {
        let feed = self.feeds.get_mut(site)?;
        if feed.in_flight || feed.retired.is_some() {
            return None;
        }
        feed.last_poll = Some(web_time::Instant::now());
        feed.in_flight = true;
        feed.poller.take()
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
    pub fn status(
        &self,
        live_sites: &[String],
        enabled: bool,
        showing: Option<(&str, f32)>,
    ) -> rustdar_egui::ChunkFeedStatus {
        let mut status = rustdar_egui::ChunkFeedStatus {
            interval_secs: rustdar_radar::chunks::POLL_INTERVAL.as_secs(),
            ..Default::default()
        };
        if !enabled {
            return status;
        }
        if let Some((site, elevation)) = showing {
            status.tilt = self.freshness(site, elevation);
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
            }
        }
        status
    }

    /// Note that a tilt was just delivered, with the age of its newest radial.
    pub fn record_delivery(&mut self, site: &str, elevation: f32, age: std::time::Duration) {
        self.delivered.insert(
            (site.to_string(), elevation_tenths(elevation)),
            Delivered {
                age_at_apply: age,
                at: web_time::Instant::now(),
            },
        );
    }

    /// How stale the tilt on screen is now, if the feed has ever delivered it.
    pub fn freshness(&self, site: &str, elevation: f32) -> Option<rustdar_egui::TiltFreshness> {
        let d = self
            .delivered
            .get(&(site.to_string(), elevation_tenths(elevation)))?;
        Some(rustdar_egui::TiltFreshness {
            elevation,
            data_age_secs: (d.age_at_apply + d.at.elapsed()).as_secs(),
        })
    }

    /// The volume so far for a site, complete sweeps only.
    pub fn snapshot(&mut self, site: &str) -> Option<std::sync::Arc<nexrad_model::data::Scan>> {
        let feed = self.feeds.get_mut(site)?;
        match feed.poller.as_mut() {
            Some(poller) => {
                let snapshot = poller.snapshot();
                // Refreshed here — the one place the poller's answer passes —
                // so the bridge below can only ever serve what some frame
                // already saw.
                feed.last_snapshot.clone_from(&snapshot);
                snapshot
            }
            // The poller is away on a round. Serve the volume as it stood
            // when the round left: a round only adds, so this is the same
            // data the previous frame resolved — see
            // [`SiteFeed::last_snapshot`] for what answering `None` here did
            // to every consumer of the merged volume.
            None => feed.last_snapshot.clone(),
        }
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
        self.delivered
            .retain(|(site, _), _| live_sites.iter().any(|s| s == site));
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
    /// `None` and the bridge must not overrule it with the stale copy.
    #[test]
    fn a_round_in_flight_does_not_take_the_snapshot_with_it() {
        use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};
        let volume = std::sync::Arc::new(Scan::new(
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
        ));

        let mut mgr = ChunkFeedManager::new();
        mgr.ensure("KICT");
        mgr.feeds.get_mut("KICT").expect("ensured").last_snapshot =
            Some(std::sync::Arc::clone(&volume));

        mgr.force_due("KICT");
        let poller = mgr.take_for_round("KICT").expect("the poller leaves");
        let held = mgr
            .snapshot("KICT")
            .expect("the volume vanished for the duration of the round");
        assert!(
            std::sync::Arc::ptr_eq(&held, &volume),
            "the bridge must serve the very volume the last frame resolved",
        );

        mgr.finish_round("KICT", poller, &empty());
        assert!(
            mgr.snapshot("KICT").is_none(),
            "a poller home with no volume yet answers None, and a bridge that \
             never refreshes would overrule it with the stale copy",
        );
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
}

#[cfg(test)]
mod freshness_tests {
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
}

#[cfg(test)]
mod due_tests {
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
}
