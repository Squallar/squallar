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

/// The in-flight volume as a consumer sees it: the sealed sweeps, and what
/// their cuts declared their Nyquist velocities to be.
///
/// The two travel together because `nexrad_model::data::Scan` cannot hold the
/// second — see [`rustdar_radar::nyquist`] — and every consumer that resolves
/// the merged current volume needs both or guards differently from the thread
/// that extracted its payload. `Arc` on each half because the assembler hands
/// its snapshot out by refcount and the bridge below clones per frame.
#[derive(Clone)]
pub struct LiveVolume {
    pub scan: std::sync::Arc<nexrad_model::data::Scan>,
    pub declared: std::sync::Arc<rustdar_radar::nyquist::DeclaredNyquist>,
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
    /// Paired with the declared Nyquist table the `Scan` cannot carry, because
    /// the bridge has to serve the *same* pair the poller would: a bridged
    /// frame that dropped the table would put the section worker on estimated
    /// fold limits for the ~0.1–1 s of every round, and back on declared ones
    /// after — a guard that changes its mind at the poll cadence.
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
            // The bridge copy dies with the flight. [`Self::snapshot`]
            // already answers `None` for a retired feed; this is the other
            // half, so the kept-snapshot bridge cannot serve the frozen
            // volume across the retirement from any path either.
            feed.last_snapshot = None;
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
    ///
    /// `None` once the feed is retired, whatever the assembler still holds.
    /// The flight is dead and the archive path owns the site — but the kept
    /// poller keeps its assembler, and the partial volume it froze on must
    /// not go on standing over a base the archive polls keep rolling
    /// forward: overlay sweeps supersede base cuts by list order, not by
    /// time, so a dead flight's low tilts would be served under a caption
    /// whose newest time reads the newer base.
    pub fn snapshot(&mut self, site: &str) -> Option<LiveVolume> {
        let feed = self.feeds.get_mut(site)?;
        if feed.retired.is_some() {
            return None;
        }
        match feed.poller.as_mut() {
            Some(poller) => {
                // The table is read before `snapshot` takes the poller
                // mutably, and both describe the same assembler state.
                let declared = poller
                    .declared_nyquist()
                    .cloned()
                    .map(std::sync::Arc::new)
                    .unwrap_or_default();
                let snapshot = poller.snapshot().map(|scan| LiveVolume { scan, declared });
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

    /// Put a feed mid-round with `scan` in hand: the poller away and the
    /// bridge serving — the shape every frame of a live round sees. For tests
    /// that need a serving overlay without a network to assemble one.
    #[cfg(test)]
    pub(crate) fn force_serving(
        &mut self,
        site: &str,
        scan: std::sync::Arc<nexrad_model::data::Scan>,
    ) {
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

#[cfg(test)]
mod due_tests;

#[cfg(test)]
mod freshness_tests;

#[cfg(test)]
mod status_tests;

#[cfg(test)]
mod tests;
