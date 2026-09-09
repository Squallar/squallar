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
    /// **What the assembler priced this volume at when it handed it over**,
    /// carried rather than re-walked.
    ///
    /// The bridge copy outlives the assembler's own reference — that is what
    /// it is for — so a caller pricing it later cannot ask the assembler, and
    /// `crate::scan_size::scan_bytes` here would be a walk of every radial on
    /// a telemetry tick. Zero only where the poller had no snapshot to price.
    pub bytes: u64,
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
    /// **Bridge copies this manager stopped holding, waiting to be freed
    /// somewhere other than here.**
    ///
    /// [`Self::snapshot`] runs on the frame thread, several times a frame.
    /// When the poller has rebuilt since the last one it hands back a
    /// different `Arc`, and the copy being replaced is by then the volume's
    /// last owner — the assembler let go of it during the rebuild. Dropping it
    /// in place freed a whole decoded volume (47–69 MiB across thousands of
    /// per-radial buffers) on the frame thread.
    ///
    /// It comes out through [`Self::take_superseded`] instead, whose caller
    /// frees it off the frame. A queue and not a call because this crate
    /// cannot reach `squallar_worker::offload`: the worker crate depends on
    /// this one.
    superseded: Vec<LiveVolume>,
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
        let dying = retirement.and_then(|reason| {
            log::warn!("{site}: retiring the chunk feed ({reason:?}); falling back to the archive");
            feed.retired = Some((reason, now));
            // The bridge copy dies with the flight — off the frame thread,
            // like every other one this manager lets go of.
            feed.last_snapshot.take()
        });
        self.superseded.extend(dying);
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
    ///
    /// **The bridge copy this replaces is set aside, not dropped** — see
    /// [`Self::take_superseded`].
    pub fn snapshot(&mut self, site: &str) -> Option<LiveVolume> {
        let (snapshot, superseded) = {
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
                    let scan = poller.snapshot();
                    // Read AFTER the snapshot, because a rebuild is exactly when
                    // the price moves: `VolumeAssembler::snapshot` sets
                    // `cached_bytes` on the way out, and a figure taken before it
                    // would describe the volume this one replaced.
                    let bytes = poller.cached_allocation().map_or(0, |(_, bytes)| bytes);
                    let snapshot = scan.map(|scan| LiveVolume {
                        scan,
                        declared,
                        bytes,
                    });
                    // Refreshed here, the one place the poller's answer passes.
                    // `replace` rather than `clone_from`: what was here comes
                    // back out so the free lands off the frame thread.
                    let superseded = std::mem::replace(&mut feed.last_snapshot, snapshot.clone());
                    (snapshot, superseded)
                }
                // The poller is away on a round.
                None => (feed.last_snapshot.clone(), None),
            }
        };
        // Only a copy of a DIFFERENT volume is worth carrying out: the warm
        // case hands back the same `Arc` every frame, and queueing that would
        // be a refcount decrement filed as an eviction.
        if let Some(previous) = superseded {
            match &snapshot {
                Some(now) if std::sync::Arc::ptr_eq(&now.scan, &previous.scan) => {}
                _ => self.superseded.push(previous),
            }
        }
        snapshot
    }

    /// **The bridge copies this manager has stopped holding**, for a caller
    /// that can free them away from the frame thread.
    ///
    /// Drain it every frame: until it is drained the manager is still holding
    /// the volumes, which is the one thing a bridge copy set aside must not
    /// become — a leak wearing an eviction's clothes.
    pub fn take_superseded(&mut self) -> Vec<LiveVolume> {
        std::mem::take(&mut self.superseded)
    }

    /// **Every decoded volume the live feeds hold, as allocation and price**,
    /// so a caller can fold `chunk feed` into a union by pointer instead of
    /// adding a family the census documents as overlapping.
    ///
    /// Two rows per site at most and usually one allocation between them: the
    /// assembler's `cached`, and the bridge copy the poller left behind. At
    /// rest those are the same `Arc` and a pointer union collapses them; they
    /// diverge exactly while a round is in flight, and while it is the
    /// assembler travels with the request and only the bridge row is here at
    /// all. Both are yielded and neither is de-duplicated — collapsing is the
    /// caller's, which is the only place that can also see the still store
    /// and the loop cache.
    ///
    /// **Plus whatever [`Self::take_superseded`] has not been asked for yet**,
    /// which is a bridge copy this manager is still the owner of. It is
    /// normally empty — the drain runs every frame — but a row missing while
    /// the bytes are resident is the one direction this instrument may not
    /// fail in, so it is yielded on the same terms as the others.
    ///
    /// **No walk and no rebuild.** Every price is one the assembler already
    /// paid; see [`crate::chunks::VolumeAssembler::cached_allocation`] for why
    /// this is not spelled as a `snapshot()`. What it does NOT reach is the
    /// parked queue — [`Self::parked_volumes`] carries that, and says why.
    pub fn held_allocations(
        &self,
    ) -> impl Iterator<Item = (*const nexrad_model::data::Scan, u64)> + '_ {
        self.feeds
            .values()
            .flat_map(|feed| {
                feed.poller
                    .as_ref()
                    .and_then(|poller| poller.cached_allocation())
                    .into_iter()
                    .chain(
                        feed.last_snapshot
                            .as_ref()
                            .map(|live| (std::sync::Arc::as_ptr(&live.scan), live.bytes)),
                    )
            })
            .chain(
                self.superseded
                    .iter()
                    .map(|live| (std::sync::Arc::as_ptr(&live.scan), live.bytes)),
            )
    }

    /// **What every site's parked queue holds**, summed as a count and a byte
    /// level. See [`crate::chunks::ChunkPoller::parked_volumes`] for why these
    /// are not allocations and must be printed beside a union rather than
    /// folded into it.
    pub fn parked_volumes(&self) -> (usize, u64) {
        self.feeds
            .values()
            .filter_map(|feed| feed.poller.as_ref())
            .fold((0usize, 0u64), |(count, bytes), poller| {
                let (n, b) = poller.parked_volumes();
                (count.saturating_add(n), bytes.saturating_add(b))
            })
    }

    /// How many sites are being fed. The denominator every per-site radar
    /// figure is read against, and not the pane count: two panes on one site
    /// share one feed.
    pub fn feed_count(&self) -> usize {
        self.feeds.len()
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
            let bytes = crate::scan_size::scan_bytes(&scan) as u64;
            feed.last_snapshot = Some(LiveVolume {
                scan,
                declared: Default::default(),
                bytes,
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
