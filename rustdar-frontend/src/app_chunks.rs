//! Driving the real-time chunk feed, and applying what it returns.
//!
//! The rounds are dispatched and drained from the frame loop rather than from a
//! self-scheduling task: this crate builds for wasm, where there is no
//! `tokio::time` and a detached loop could not be cancelled by the UI. The
//! cadence lives in [`rustdar_radar::chunks::POLL_INTERVAL`] and is enforced by
//! [`crate::chunk_feed::ChunkFeedManager`], not here.

use std::sync::Arc;

use rustdar_radar::types::ScanInfo;

use crate::channels::ChunkResponse;
use crate::chunk_feed::Retirement;
use crate::chunk_notify::{ChunkAvailable, Feed, Notified};

impl super::App {
    /// Start or stop feeds so the set matches the sites panes are watching
    /// live, and dispatch a round for any that is due.
    ///
    /// Called once a frame. Cheap when nothing is due: the manager's own
    /// interval check is the gate, and every site is normally in the middle of
    /// one.
    pub(super) fn drive_chunk_feeds(&mut self) {
        let enabled = self.gui.live_chunks_enabled();
        let live = self.gui.live_sites();
        // Published every frame, including when the feed is off or retired, so
        // the status bar never shows a stale claim about the transport.
        let showing = self
            .gui
            .get_rendering_params_for_pane(self.gui.active_pane_idx())
            .map(|(_, elevation)| (self.gui.active_pane().site.clone(), elevation));
        let mut status = self.chunk_feeds.status(
            &live,
            enabled,
            showing.as_ref().map(|(s, e)| (s.as_str(), *e)),
        );
        status.pushed = status.feeding
            && showing
                .as_ref()
                .is_some_and(|(site, _)| self.chunk_notify.chunk_link_open(site));
        self.gui.set_chunk_status(status);
        // Ahead of the `enabled` gate on purpose, for two reasons. Archive
        // pushes are worth having precisely when the chunk feed is off — they
        // are what takes the path that is then carrying the site from "up to a
        // minute late" to "as soon as it is published". And reconnection runs
        // from here, so returning early would mean a socket that dropped while
        // the setting was briefly off is never retried after it comes back.
        self.drive_chunk_notifications(&live);
        if !enabled {
            // The feeds go with the setting, not merely the rounds. Kept, the
            // map's last assemblers would go on serving their frozen partial
            // overlays to every consumer of the merged current volume — none
            // of which gates on this setting — and each one holds tens of
            // megabytes of dead volume besides. A no-op every frame after the
            // first: the map is already empty.
            self.chunk_feeds.retain_live(&[]);
            return;
        }
        // Narrower than `evict_unshown_scans`: a feed has no reader once no pane
        // is live on its site. See `ChunkFeedManager::retain_live`.
        self.chunk_feeds.retain_live(&live);

        for site in live {
            self.chunk_feeds.ensure(&site);
            let selection = self.cut_selection_for(&site);
            self.chunk_feeds.set_selection(&site, selection);
            let Some(mut poller) = self.chunk_feeds.take_for_round(&site) else {
                continue;
            };
            // Inherited, never bumped. A five-second tick that superseded a
            // manual navigation would make the scan drain's stale arm take that
            // navigation's spinner down early.
            let generation = self.render.fetch_generation_for(&site);
            let sender = self.channels.chunk_sender.clone();
            let window = self.window.clone();
            self.spawn_detached(async move {
                let result = rustdar_radar::scan::poll_chunks(&mut poller)
                    .await
                    .map_err(|e| format!("{e:?}"));
                let _ = sender.send(ChunkResponse {
                    generation,
                    site,
                    poller,
                    result,
                });
                crate::app::notify_redraw(&window);
            });
        }
    }

    /// What this site's feed needs to download: **everything, always.**
    ///
    /// This used to narrow the feed to the tilts on screen, with `All` forced
    /// by whole-volume products, whole-volume pane kinds, and active loops —
    /// three exceptions whose omission each produced a plausible, wrong
    /// picture with no error to notice. The narrowing is superseded by the
    /// current merged volume: a live site's whole point is now that the app
    /// *always* holds a full and current copy of its data, so that a
    /// cross-section or a 3D pane opened at any moment cuts instantly from
    /// `base_scans` plus every sealed sweep, and so that each closed volume is
    /// `whole_volume_complete` and rolls the base forward without another
    /// archive download. A narrowed feed breaks both halves of that promise:
    /// the overlay would carry only the shown tilts, and no closed volume
    /// would ever be whole, so the base would age from the moment the first
    /// archive fetch landed — `CheckForNewScans` is skipped for any chunk-fed
    /// site, so nothing else would refresh it.
    ///
    /// The cost this buys back is one full volume per volume period —
    /// measured against KTLX, chunks for a complete super-resolution volume
    /// run 10–25 MB per 4–7 minutes — which is the price of the product
    /// working the way the reference display does.
    ///
    /// What the narrowing protected is still protected, one layer down:
    /// `App::apply_chunk_outcome` refuses to cache or base a volume that is
    /// not whole. That guard is now the belt for a rule this function makes
    /// true by construction, exactly as before — it must never be the thing
    /// that fires.
    ///
    /// [`CutSelection::Tilts`](rustdar_radar::chunks::CutSelection::Tilts)
    /// itself stays in `rustdar-radar`, tested and working: the decision
    /// retired here is the *frontend's*, and a future caller with a genuine
    /// bandwidth ceiling (a metered mobile build, say) has the mechanism and
    /// this history to weigh against it.
    fn cut_selection_for(&self, _site: &str) -> rustdar_radar::chunks::CutSelection {
        rustdar_radar::chunks::CutSelection::All
    }

    /// Keep the notification subscriptions matched to the live sites, and turn
    /// anything they said into an early round.
    ///
    /// A notification never carries data — only "a chunk exists". It marks the
    /// site due and the ordinary poller does the rest, which is what makes the
    /// service optional: with it, latency is bounded by the fetch; without it,
    /// by the five-second timer that is still running underneath.
    fn drive_chunk_notifications(&mut self, live: &[String]) {
        if !self.gui.chunk_notifications_enabled() {
            // Drop every socket rather than merely ignoring them, so turning the
            // setting off actually stops the connections.
            self.chunk_notify.sync_sites(&[], &[], "", || {});
            return;
        }
        // Chunk pushes only mean anything while the live feed is running, since
        // all they do is bring its next round forward. Archive pushes stand on
        // their own and are kept either way.
        let chunks = self.gui.live_chunks_enabled();
        let feeds: &[Feed] = if chunks { &Feed::ALL } else { &[Feed::Archive] };
        let endpoint = self.gui.notifier_endpoint().to_string();
        let window = self.window.clone();
        self.chunk_notify
            .sync_sites(live, feeds, &endpoint, move || {
                // From the socket's own thread: without this the frame loop can sleep
                // through the very notification that was supposed to wake it.
                crate::app::notify_redraw(&window);
            });

        for notified in self.chunk_notify.drain() {
            // Nothing should arrive on a feed that was not subscribed, but a
            // chunk notification acted on with the feed off would build an
            // assembler nothing will ever drain.
            if !chunks && matches!(notified, Notified::Chunk(_)) {
                continue;
            }
            match notified {
                // The message named the object, so fetch it outright — no
                // listing, no discovery, no rollover probe.
                Notified::Chunk(ChunkAvailable::Identified(id)) => self.fetch_notified_chunk(id),
                // It only said something landed. Bring the site's next round
                // forward and let the poller work out what is new.
                Notified::Chunk(ChunkAvailable::Site(site)) => self.chunk_feeds.mark_due(&site),
                // A completed volume was published. Routed through the ordinary
                // auto-poll action rather than fetched here, which is what keeps
                // one description of "is this volume worth taking": it skips
                // sites the chunk feed is already serving, inherits the
                // generation bookkeeping, and lands in the scan drain behind the
                // guard that refuses an archive volume older than the live feed.
                //
                // This is what takes the fallback path — and every historic pane
                // and loop — from up to a minute late to as soon as it is
                // published.
                Notified::Archive { site } => self.check_archive_for(&site),
            }
        }
    }

    /// Ask the archive for this site's newest volume, exactly as the 60-second
    /// timer would have.
    fn check_archive_for(&mut self, site: &str) {
        if !self.gui.live_sites().iter().any(|s| s == site) {
            return;
        }
        let now = chrono::Local::now().naive_local();
        self.handle_gui_action(
            rustdar_egui::actions::GuiAction::CheckForNewScans(
                rustdar_egui::actions::RadarConfig {
                    site: site.to_string(),
                    timestamp: now,
                },
            ),
            None,
        );
    }

    /// Fetch one notified chunk, borrowing the site's poller for the round.
    ///
    /// Goes through the same take/finish bookkeeping as a polled round, so a
    /// burst of notifications for one volume cannot start several concurrent
    /// fetches and the retirement rules still see every failure.
    fn fetch_notified_chunk(&mut self, id: rustdar_radar::chunks::ChunkId) {
        let site = id.site().to_string();
        self.chunk_feeds.ensure(&site);
        let Some(mut poller) = self.chunk_feeds.take_now(&site) else {
            // A round is already in flight; its listing will pick this chunk up.
            return;
        };
        let generation = self.render.fetch_generation_for(&site);
        let sender = self.channels.chunk_sender.clone();
        let window = self.window.clone();
        self.spawn_detached(async move {
            let result = rustdar_radar::scan::fetch_notified_chunk(&mut poller, &id)
                .await
                .map_err(|e| format!("{e:?}"));
            let _ = sender.send(ChunkResponse {
                generation,
                site,
                poller,
                result,
            });
            crate::app::notify_redraw(&window);
        });
    }

    /// Drain finished rounds and apply them.
    pub(super) fn poll_chunk_results(&mut self) {
        while let Ok(resp) = self.channels.chunk_receiver.try_recv() {
            let ChunkResponse {
                generation,
                site,
                poller,
                result,
            } = resp;

            let retirement = self.chunk_feeds.finish_round(&site, poller, &result);

            // A site switch or a manual navigation has moved on; whatever this
            // round assembled belongs to a volume nothing is showing.
            if self.render.is_fetch_stale(&site, generation) {
                continue;
            }

            match &result {
                Err(e) => log::debug!("{site}: chunk round failed: {e}"),
                Ok(outcome) => self.apply_chunk_outcome(&site, outcome),
            }

            if let Some(reason) = retirement {
                self.fall_back_to_archive(&site, reason);
            }
        }
    }

    /// Apply one round's completions.
    ///
    /// # Which volume a round is about
    ///
    /// A round that rolled describes two: the one that closed and the one now
    /// being assembled. When the closed one *completed*, that is the one applied,
    /// from its own `ClosedVolume::scan` — never from the feed's live snapshot,
    /// which by then is the new volume with no complete cut in it at all.
    ///
    /// Reading the live snapshot here was a staleness bug on every whole-volume
    /// product. `ChunkPoller::roll` sets `closed` in the same statement that
    /// replaces the assembler `snapshot` reads, so the guard below fired on the
    /// empty new volume and the entire `volume_complete` branch — the site reset,
    /// the Level III refetch, the loop append — never ran on a healthy feed. A
    /// pane on echo tops, NROT, SRV, HCA or either hail product rendered once and
    /// then stayed frozen until the user changed something.
    ///
    /// It was also a *correctness* bug in the minority case it did run in. After
    /// an error backoff the probe round can find the new volume already carrying
    /// a sealed cut, so the snapshot was not empty and a whole-volume product was
    /// handed a one- or two-cut volume — the failure `reads_whole_volume` exists
    /// to prevent, and one that produces a plausible wrong answer rather than an
    /// error. Taking the closed volume's own scan makes that unreachable: the
    /// branch is gated on `progress.volume_complete` and reads the scan that flag
    /// describes.
    ///
    /// The round's *own* `sealed_elevations` belong to the new volume, so they are
    /// not used on that path — `reset_panes_for_site` covers every pane on the
    /// site, including the tilt panes those cuts would have refreshed, and the
    /// freshness stamps come from the closed volume's cuts against the closed
    /// volume's radials. (Not a repair of anything: before the closed volume
    /// travelled out, `scan` and `sealed` both described the volume being
    /// assembled and so agreed. The pairing changes *because* `scan` changed.)
    /// Applying both volumes in one round is not an option: `scan_data` holds one
    /// volume per site, and a partial one there is exactly what the paragraph above
    /// is about.
    ///
    /// # `volume_complete` is not "whole", and what is stored takes the strict gate
    ///
    /// `volume_complete` means every cut *the selection asked for* sealed. The
    /// selection is now always `All` ([`Self::cut_selection_for`]), so the two
    /// predicates coincide in practice — but the distinction stays load-bearing
    /// for everything a volume outlives: the loop cache is read product-blind
    /// later, and `base_scans` puts a ladder under every whole-volume consumer
    /// for the whole next volume. Both writers therefore gate on
    /// `whole_volume_complete`, the statement about the *data*, so that a
    /// regression in the selection — or a volume genuinely missing a cut to
    /// chunk loss — degrades to "the base ages one volume" rather than to a
    /// plausible, short ladder nothing would notice.
    fn apply_chunk_outcome(&mut self, site: &str, outcome: &rustdar_radar::chunks::PollOutcome) {
        // The flag and the scan are read together rather than one gating the other:
        // `ChunkPoller::roll` builds the scan exactly when the volume completed, so
        // a change to either end of that contract lands here as "nothing to apply"
        // instead of as a volume nothing checked.
        let completed = outcome
            .closed
            .as_ref()
            .filter(|closed| closed.progress.volume_complete)
            .and_then(|closed| closed.scan.as_ref().map(|scan| (closed, scan)));
        let (scan, sealed) = match completed {
            Some((closed, scan)) => (
                Arc::clone(scan),
                closed.progress.sealed_elevations.as_slice(),
            ),
            None => {
                // Cost, not safety — nothing below is wrong on a round that
                // sealed nothing, it is just work for no change. `ScanInfo::from_scan`
                // walks every radial of every sweep and `reset_panes_for_tilts`
                // sweeps the render cache, and most rounds seal nothing.
                if outcome.sealed_elevations.is_empty() {
                    return;
                }
                let Some(live) = self.chunk_feeds.snapshot(site) else {
                    return;
                };
                (live.scan, outcome.sealed_elevations.as_slice())
            }
        };
        if scan.sweeps().is_empty() {
            return;
        }

        // The volume's own start, from its first radial — stable across the
        // whole volume, so it does not walk while cuts land.
        let info = ScanInfo::from_scan(&scan, site, self.gui.get_radar_config().timestamp);
        let timestamp = info.timestamp;

        // Mirrors the archive drain: a site no pane is watching live keeps its
        // data for `JumpToLive` and its loops, and must not have `scan_info`
        // moved under it.
        if !self.any_pane_live_for_site(site) {
            self.latest_cached_scans
                .insert(site.to_string(), (scan, info, timestamp));
            return;
        }

        self.scan_data.insert(site.to_string(), Arc::clone(&scan));

        if let Some((closed, _)) = completed {
            // A whole closed volume is the same volume the archive will
            // publish minutes from now, so it becomes the site's merge base
            // immediately — this is what keeps sections and the 3D view
            // standing on a complete volume across every roll without another
            // archive download. Gated on `whole_volume_complete`, the same
            // strictness the loop append below carries and for the same
            // reason: the base outlives this round, and a base missing cuts
            // would put a plausible, short ladder under every consumer.
            if closed.progress.whole_volume_complete {
                // The closed volume's own declarations travel with it: this
                // base is what every section and 3D payload is cut from until
                // the next volume closes, and a base without them puts the
                // worker's velocity fold guard back on estimates.
                self.base_scans.insert(
                    site.to_string(),
                    (
                        Arc::clone(&scan),
                        Arc::new(closed.declared_nyquist.clone()),
                        timestamp,
                    ),
                );
            }
            // The volume is now exactly what the archive would have published,
            // so the steady state matches it — including the Level III refetch
            // that re-registers the tilts a merge preserved mid-volume.
            self.gui.set_scan_info_for_site(site, info);
            self.gui.clear_loading_site_for_site(site);
            // Every pane on the site, whatever its product, and deliberately not
            // a narrower reset of the whole-volume readers alone. This is a volume
            // *boundary*: every pane here is showing an image built from the
            // volume before the one just installed, so all of them are stale, not
            // only the whole-volume readers. It also stands in for this round's
            // own `sealed_elevations`, which belong to the *new* volume and so
            // never reach `reset_panes_for_tilts`. And it is the reset that drops
            // the site's `level3_data` and `render_cache`, which the refetch below
            // needs — a pane-only reset would leave the previous volume's objects
            // and images to be handed straight back.
            self.render.reset_panes_for_site(site, &self.gui);
            self.spawn_level3_fetches(site);
            self.record_tilt_freshness(site, &scan, sealed);
            // Two conditions, and both are about permanence.
            //
            // *Here and not mid-volume*, because `append_polled_frame` dedupes by
            // timestamp and a `LoopFrame` has no "the scan got better" transition,
            // so a frame appended for a volume still being assembled would freeze
            // on however many cuts it had at that moment.
            //
            // *`whole_volume_complete`, not `volume_complete`*, because this is the
            // one place a volume outlives the selection that produced it. The cache
            // behind this call is read product-blind and never re-downloaded, so a
            // volume narrowed to the tilts a Reflectivity loop wanted would be
            // handed to echo tops the moment that pane changed product.
            //
            // **This is a guard, not a policy, and it must not be the thing that
            // fires.** Skipping it skips the frame append too — the call is both —
            // and nothing else backfills a frame: the 60 s archive check is skipped
            // for any chunk-fed site, and only enabling a loop lists a window. A site
            // whose feed was narrow while looping would therefore gain no frames at
            // all and age indefinitely. What keeps that from happening is
            // `cut_selection_for`, which answers `All` for any site with an active
            // loop, so a looping site's volumes are whole and this passes. The guard
            // stays as the thing that makes a non-whole volume in the cache
            // unreachable rather than merely unlikely.
            if closed.progress.whole_volume_complete {
                self.append_scan_to_active_loops(site, timestamp, scan);
            } else {
                log::debug!(
                    "{site}: volume complete on the {} cut(s) the feed asked for but \
                     not whole, so it is not cached for the loops",
                    closed.progress.sealed_elevations.len()
                );
            }
        } else {
            self.gui.apply_chunk_scan_info(site, info);
            self.gui.clear_loading_site_for_site(site);
            self.record_tilt_freshness(site, &scan, sealed);
            let hit = self
                .render
                .reset_panes_for_tilts(site, &self.gui, &outcome.sealed_angles);
            log::debug!(
                "{site}: cuts {:?} complete, {hit} pane(s) re-rendering",
                outcome.sealed_elevations
            );
        }
        // Deliberately absent on both paths: `set_radar_config`, which belongs
        // to user navigation and would drag the time picker along every few
        // seconds, and `manual_nav_pending`, which would trigger
        // `reinit_active_loops` and re-list the whole lookback window per round.
    }

    /// Stamp each freshly delivered cut with the age of its newest radial.
    ///
    /// Taken from the sweep rather than from the wall clock at arrival: what a
    /// user wants to know is how long ago the *radar* looked, and a chunk can
    /// sit in the bucket or in a retry before it gets here.
    fn record_tilt_freshness(
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
            self.chunk_feeds.record_delivery(site, angle, age);
        }
    }

    /// Hand a site back to the archive path.
    ///
    /// The fetch is unconditional rather than a `CheckForNewScans`. That check
    /// compares against `scan_info.timestamp`, which this feed has already
    /// advanced to the in-progress volume, so it would answer "nothing newer"
    /// and leave the pane on a partial volume until the radar published the
    /// *next* one.
    ///
    /// It also does not go through `set_error`: that resets the *archive* poll's
    /// backoff for a failure that was not the archive's.
    fn fall_back_to_archive(&mut self, site: &str, reason: Retirement) {
        log::warn!("{site}: chunk feed retired ({reason:?}); refetching from the archive");
        let timestamp = Self::local_to_utc(self.gui.get_radar_config().timestamp);
        self.spawn_fetch(site.to_string(), timestamp);
    }

    /// Whether any pane on this site is showing live data.
    pub(super) fn any_pane_live_for_site(&self, site: &str) -> bool {
        (0..self.gui.pane_count()).any(|i| {
            self.gui
                .pane(i)
                .is_some_and(|p| p.site == site && p.viewing_live)
        })
    }

    /// Whether the chunk feed is currently serving this site, so the 60 s
    /// archive check for it is redundant.
    pub(super) fn chunks_are_feeding(&self, site: &str) -> bool {
        self.gui.live_chunks_enabled() && self.chunk_feeds.is_feeding(site)
    }
}

#[path = "app_chunks/selection_tests.rs"]
#[cfg(test)]
mod selection_tests;

#[path = "app_chunks/tests.rs"]
#[cfg(test)]
mod tests;

#[path = "app_chunks/volume_close_tests.rs"]
#[cfg(test)]
mod volume_close_tests;
