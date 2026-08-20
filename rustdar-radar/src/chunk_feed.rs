//! Per-site real-time chunk feeds, and the rules for retiring one.

use std::collections::HashMap;

use crate::chunks::{ChunkPoller, PollOutcome, VolumeIndex};

/// Consecutive failed rounds before a site falls back to the archive.
pub const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// How long a feed may make no progress at all before it is retired.
pub const STALL: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a retired site waits before chunks are tried again.
pub const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(600);

/// How fresh the tilt on screen is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TiltFreshness {
    /// The elevation the active pane is rendering — the snapped angle.
    pub elevation: f32,
    /// Seconds since the radar collected the newest radial in that sweep.
    pub data_age_secs: u64,
}

/// What the real-time chunk feed is doing for the pane on screen.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChunkFeedStatus {
    /// Some live site is being fed from the real-time bucket.
    pub feeding: bool,
    /// A live site had its feed retired and fell back to the archive.
    pub retired: bool,
    /// The feed's own poll cadence, in seconds.
    pub interval_secs: u64,
    /// A push-notification socket is open, so chunks are fetched on arrival
    /// rather than on the next tick.
    pub pushed: bool,
    /// The active pane's tilt, once the feed has delivered it at least once.
    pub tilt: Option<TiltFreshness>,
}

/// Why a site stopped using the chunk feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retirement {
    /// Repeated hard failures — network, CORS, S3, a listing that would not parse.
    Errors,
    /// Rounds kept succeeding but nothing ever arrived.
    Stalled,
}

/// The in-flight volume as a consumer sees it: the sealed sweeps, and what
/// their cuts declared their Nyquist velocities to be.
#[derive(Clone)]
pub struct LiveVolume {
    pub scan: std::sync::Arc<nexrad_model::data::Scan>,
    pub declared: std::sync::Arc<crate::nyquist::DeclaredNyquist>,
}

/// One site's feed.
pub struct SiteFeed {
    /// `None` only while a round is in flight: the poller travels with the
    /// request and comes back on the response.
    poller: Option<Box<ChunkPoller>>,
    /// The last snapshot the poller handed out, bridging the window the
    /// poller is away on a round.
    last_snapshot: Option<LiveVolume>,
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
            Some(volume) => crate::scan::resume_chunk_poller(site, volume),
            None => crate::scan::chunk_poller(site),
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

    /// How long until this feed next wants a frame, or `None` when it does not
    /// want one at all.
    fn next_round_delay(&self, now: web_time::Instant) -> Option<std::time::Duration> {
        if self.in_flight {
            return None;
        }
        if let Some((_, at)) = self.retired {
            return Some(RETRY_AFTER.saturating_sub(now.duration_since(at)));
        }
        let poller = self.poller.as_ref()?;
        let Some(last) = self.last_poll else {
            return Some(std::time::Duration::ZERO);
        };
        Some(
            poller
                .suggested_interval()
                .saturating_sub(now.duration_since(last)),
        )
    }
}

/// Elevation in tenths of a degree, so two angles that round to the same tilt
/// share a key — the same rounding `render_dispatch` and `ScanInfo` use.
fn elevation_tenths(elevation: f32) -> i32 {
    (elevation * 10.0).round() as i32
}

/// When a tilt was last delivered, and how old its data was at that moment.
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

    /// How long until some feed next wants a round, or `None` when none of
    /// them will without something else happening first.
    pub fn next_round_delay(&self) -> Option<std::time::Duration> {
        let now = web_time::Instant::now();
        self.feeds
            .values()
            .filter_map(|feed| feed.next_round_delay(now))
            .min()
    }

    /// Whether this site is currently fed by chunks.
    pub fn is_feeding(&self, site: &str) -> bool {
        self.feeds
            .get(site)
            .is_some_and(|f| f.retired.is_none() && f.poller.is_some())
    }

    /// Start a feed for a site, or clear a retirement whose retry window has passed.
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
    pub fn mark_due(&mut self, site: &str) {
        if let Some(feed) = self.feeds.get_mut(site) {
            feed.last_poll = None;
        }
    }

    /// Tell a site's feed which cuts to download.
    pub fn set_selection(&mut self, site: &str, selection: crate::chunks::CutSelection) {
        if let Some(feed) = self.feeds.get_mut(site)
            && let Some(poller) = feed.poller.as_mut()
        {
            poller.set_selection(selection);
        }
    }

    /// Take the poller regardless of the interval, for a notification-driven
    /// fetch.
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
            // The bridge copy dies with the flight.
            feed.last_snapshot = None;
        }
        retirement
    }

    /// A one-line summary of what the feed is doing across the sites on screen,
    /// for the status bar.
    pub fn status(
        &self,
        live_sites: &[String],
        enabled: bool,
        showing: Option<(&str, f32)>,
    ) -> ChunkFeedStatus {
        let mut status = ChunkFeedStatus {
            interval_secs: crate::chunks::POLL_INTERVAL.as_secs(),
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

    /// Stamp each freshly delivered cut with the age of its newest radial.
    pub fn record_tilt_freshness(
        &mut self,
        site: &str,
        scan: &nexrad_model::data::Scan,
        sealed: &[u8],
    ) {
        let now = chrono::Utc::now();
        for elevation_number in sealed {
            let Some(sweep) = scan
                .sweeps()
                .iter()
                .find(|s| s.elevation_number() == *elevation_number)
            else {
                continue;
            };
            let Some(angle) = sweep.elevation_angle_degrees() else {
                continue;
            };
            let newest = sweep
                .radials()
                .iter()
                .map(|r| r.collection_timestamp())
                .max()
                .and_then(chrono::DateTime::from_timestamp_millis);
            let age = newest
                .map(|t| (now - t).to_std().unwrap_or_default())
                .unwrap_or_default();
            self.record_delivery(site, angle, age);
        }
    }

    /// How stale the tilt on screen is now, if the feed has ever delivered it.
    pub fn freshness(&self, site: &str, elevation: f32) -> Option<TiltFreshness> {
        let d = self
            .delivered
            .get(&(site.to_string(), elevation_tenths(elevation)))?;
        Some(TiltFreshness {
            elevation,
            data_age_secs: (d.age_at_apply + d.at.elapsed()).as_secs(),
        })
    }

    /// The volume so far for a site, complete sweeps only.
    pub fn snapshot(&mut self, site: &str) -> Option<LiveVolume> {
        let feed = self.feeds.get_mut(site)?;
        if feed.retired.is_some() {
            return None;
        }
        match feed.poller.as_mut() {
            Some(poller) => {
                let declared = poller
                    .declared_nyquist()
                    .cloned()
                    .map(std::sync::Arc::new)
                    .unwrap_or_default();
                let snapshot = poller.snapshot().map(|scan| LiveVolume { scan, declared });
                // Refreshed here, the one place the poller's answer passes.
                feed.last_snapshot.clone_from(&snapshot);
                snapshot
            }
            // The poller is away on a round.
            None => feed.last_snapshot.clone(),
        }
    }

    /// Drop the feeds of sites nothing is watching live.
    pub fn retain_live(&mut self, live_sites: &[String]) -> Vec<SiteFeed> {
        let unshown = |site: &String| !live_sites.iter().any(|s| s == site);
        // `extract_if` is `retain`'s inverse: the doomed values come back owned.
        let evicted: Vec<SiteFeed> = self
            .feeds
            .extract_if(|site, _| unshown(site))
            .map(|(_, feed)| feed)
            .collect();
        self.delivered
            .retain(|(site, _), _| live_sites.iter().any(|s| s == site));
        evicted
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

    /// Also compiled under the `test-support` feature, for a dependent's tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn force_retire_at(&mut self, site: &str, ago: std::time::Duration) {
        if let Some(feed) = self.feeds.get_mut(site) {
            feed.retired = Some((Retirement::Errors, web_time::Instant::now() - ago));
        }
    }

    /// Put a feed mid-round with `scan` in hand: the poller away, bridge serving.
    #[cfg(any(test, feature = "test-support"))]
    pub fn force_serving(&mut self, site: &str, scan: std::sync::Arc<nexrad_model::data::Scan>) {
        if let Some(feed) = self.feeds.get_mut(site) {
            feed.last_snapshot = Some(LiveVolume {
                scan,
                declared: Default::default(),
            });
            feed.poller = None;
            feed.in_flight = true;
        }
    }
}

/// What a site's feed needs to download: **everything, always.**
pub fn cut_selection_for(_site: &str) -> crate::chunks::CutSelection {
    crate::chunks::CutSelection::All
}

#[cfg(test)]
mod due_tests;

#[cfg(test)]
mod freshness_tests;

#[cfg(test)]
mod status_tests;

#[cfg(test)]
mod tests;
