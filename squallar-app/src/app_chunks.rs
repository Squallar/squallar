//! Driving the real-time chunk feed, and applying what it returns.
//!
//! Rounds are dispatched and drained from the frame loop, not a self-scheduling
//! task: this crate builds for wasm, where there is no `tokio::time`. Cadence is
//! [`squallar_radar::chunk_feed::ChunkFeedManager`]'s.

use std::sync::Arc;

use crate::channels::{ChunkResponse, FetchRequester};
use squallar_radar::chunk_feed::Retirement;
use squallar_radar::chunk_notify::{ChunkAvailable, Feed, Notified};

impl super::App {
    /// Start or stop feeds so the set matches the sites panes are watching live,
    /// and dispatch a round for any that is due. Called once a frame.
    pub(super) fn drive_chunk_feeds(&mut self) {
        let enabled = squallar_egui::radar_layer::live_chunks_enabled(&self.gui);
        let live = self.gui.live_sites();
        // Published every frame, so the status bar never shows a stale claim.
        let showing = self
            .gui
            .get_rendering_params_for_pane(self.gui.active_pane_idx())
            .map(|(_, elevation)| (self.gui.active_pane().site().to_string(), elevation));
        let mut status = self.chunk_feeds.status(
            &live,
            enabled,
            showing.as_ref().map(|(s, e)| (s.as_str(), *e)),
        );
        status.pushed = status.feeding
            && showing
                .as_ref()
                .is_some_and(|(site, _)| self.chunk_notify.chunk_link_open(site));
        if self.radar_liveness.chunk_status != status {
            self.radar_liveness.chunk_status = status;
            self.republish_liveness();
        }
        // Ahead of the `enabled` gate on purpose: archive pushes matter most when
        // the chunk feed is off, and reconnection runs from here.
        self.drive_chunk_notifications(&live);
        if !enabled {
            // The feeds go with the setting, not merely the rounds: kept, their
            // assemblers serve frozen partial overlays and hold dead volumes.
            squallar_worker::offload::discard_each(
                "retired-feed",
                self.chunk_feeds.retain_live(&[]),
            );
            return;
        }
        // Narrower than `evict_unshown_scans`: a feed has no reader once no pane
        // is live on its site. This drain gets their frees off the frame thread.
        squallar_worker::offload::discard_each("retired-feed", self.chunk_feeds.retain_live(&live));

        for site in live {
            self.chunk_feeds.ensure(&site);
            let selection = self.cut_selection_for(&site);
            self.chunk_feeds.set_selection(&site, selection);
            let Some(mut poller) = self.chunk_feeds.take_for_round(&site) else {
                continue;
            };
            // Inherited, never bumped: a tick that superseded a manual navigation
            // would take that navigation's spinner down early.
            let generation = self.render.fetch_generation_for(&site);
            let sender = self.channels.chunk_sender.clone();
            let window = self.window.clone();
            self.spawn_detached(async move {
                let result = squallar_radar::scan::poll_chunks(&mut poller)
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

    /// What this site's feed needs to download: everything, always. Policy in
    /// [`squallar_radar::chunk_feed::cut_selection_for`].
    fn cut_selection_for(&self, site: &str) -> squallar_radar::chunks::CutSelection {
        squallar_radar::chunk_feed::cut_selection_for(site)
    }

    /// Keep the notification subscriptions matched to the live sites, and turn
    /// anything they said into an early round. A notification never carries data
    /// — it marks the site due and the ordinary poller does the rest.
    fn drive_chunk_notifications(&mut self, live: &[String]) {
        if !squallar_egui::radar_layer::chunk_notifications_enabled(&self.gui) {
            // Drop every socket rather than ignoring them, so the setting off
            // actually stops the connections.
            self.chunk_notify.sync_sites(&[], &[], "", || {});
            return;
        }
        // Chunk pushes only matter while the live feed runs; archive pushes stand
        // on their own.
        let chunks = squallar_egui::radar_layer::live_chunks_enabled(&self.gui);
        let feeds: &[Feed] = if chunks { &Feed::ALL } else { &[Feed::Archive] };
        let endpoint = squallar_egui::radar_layer::notifier_endpoint(&self.gui);
        let window = self.window.clone();
        self.chunk_notify
            .sync_sites(live, feeds, &endpoint, move || {
                // From the socket's own thread: else the frame loop can sleep
                // through the very notification that was supposed to wake it.
                crate::app::notify_redraw(&window);
            });

        for notified in self.chunk_notify.drain() {
            // A chunk notification acted on with the feed off would build an
            // assembler nothing will ever drain.
            if !chunks && matches!(notified, Notified::Chunk(_)) {
                continue;
            }
            match notified {
                Notified::Chunk(ChunkAvailable::Identified(id)) => self.fetch_notified_chunk(id),
                Notified::Chunk(ChunkAvailable::Site(site)) => self.chunk_feeds.mark_due(&site),
                // Through the ordinary auto-poll action, which keeps one
                // description of "is this volume worth taking".
                Notified::Archive { site } => self.check_archive_for(&site),
            }
        }
    }

    /// Ask the archive for this site's newest volume, as the 60-second timer would.
    fn check_archive_for(&mut self, site: &str) {
        if !self.gui.live_sites().iter().any(|s| s == site) {
            return;
        }
        let now = chrono::Local::now().naive_local();
        self.handle_gui_action(
            squallar_egui::actions::GuiAction::CheckForNewScans(
                squallar_egui::actions::RadarConfig {
                    site: site.to_string(),
                    timestamp: now,
                },
            ),
            None,
        );
    }

    /// Fetch one notified chunk, borrowing the site's poller for the round, so a
    /// burst of notifications for one volume cannot start concurrent fetches.
    fn fetch_notified_chunk(&mut self, id: squallar_radar::chunks::ChunkId) {
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
            let result = squallar_radar::scan::fetch_notified_chunk(&mut poller, &id)
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
    /// A round that rolled describes two volumes: the one that closed and the
    /// one now being assembled. When the closed one completed, that is the one
    /// applied, from its own `ClosedVolume::scan` — never from the feed's live
    /// snapshot, which by then is the new volume with no complete cut in it.
    /// The round's own `sealed_elevations` belong to the new volume.
    ///
    /// `volume_complete` means every cut the selection asked for sealed; what
    /// is stored for later readers gates on `whole_volume_complete`.
    fn apply_chunk_outcome(&mut self, site: &str, outcome: &squallar_radar::chunks::PollOutcome) {
        let completed = outcome
            .closed
            .as_ref()
            .filter(|closed| closed.progress.volume_complete)
            .and_then(|closed| closed.scan.as_ref().map(|scan| (closed, scan)));
        // The declared Nyquist travels with the volume on both branches: every
        // consumer below reads velocity and would otherwise fold around estimates.
        let (scan, declared, sealed) = match completed {
            Some((closed, scan)) => (
                Arc::clone(scan),
                Arc::new(closed.declared_nyquist.clone()),
                closed.progress.sealed_elevations.as_slice(),
            ),
            None => {
                // Cost, not safety: `ScanInfo::from_scan` walks every radial of
                // every sweep and most rounds seal nothing.
                if outcome.sealed_elevations.is_empty() {
                    // Except the round the coverage pattern arrives on: until it
                    // lands `current::resolve` refuses the volume, so every plan
                    // pane on this site drew without it.
                    if outcome.learned_coverage_pattern {
                        self.render.reset_panes_for_site(site, &self.gui);
                    }
                    return;
                }
                let Some(live) = self.chunk_feeds.snapshot(site) else {
                    return;
                };
                (
                    live.scan,
                    live.declared,
                    outcome.sealed_elevations.as_slice(),
                )
            }
        };
        if scan.sweeps().is_empty() {
            return;
        }

        // The volume's own start, from its first radial — stable across the whole
        // volume. Same helper as the archive drain, so positions cannot diverge.
        let requested = self.gui.selected_timestamp();
        let info = self.scan_info_learning_position(&scan, site, requested);
        let timestamp = info.timestamp;

        // Mirrors the archive drain: a site no pane is watching live keeps its
        // data for `JumpToLive` and its loops.
        if !self.any_pane_live_for_site(site) {
            self.latest_cached_scans
                .insert(site.to_string(), (scan, declared, info, timestamp));
            return;
        }

        // Keyed by the volume's own collected-at -- `timestamp` above, which is
        // exactly what the `ScanInfo` published below carries onto every pane on
        // the site, so the key installed is the key the render reads back with.
        let forced = self.volumes.install_still(
            site.to_string(),
            timestamp,
            (Arc::clone(&scan), Arc::clone(&declared)),
        );
        squallar_worker::offload::discard_each("capped-still", forced);

        if let Some((closed, _)) = completed {
            // A whole closed volume is the same volume the archive will publish
            // minutes from now, so it becomes the site's merge base immediately.
            if closed.progress.whole_volume_complete {
                // Without the closed volume's declarations the worker's velocity
                // fold guard is back on estimates.
                self.volumes.install_base(
                    site.to_string(),
                    (Arc::clone(&scan), Arc::clone(&declared), timestamp),
                );
            }
            self.gui
                .apply(squallar_egui::shell_api::GuiEvent::LiveScanInfoForSite {
                    site: site.to_owned(),
                    info,
                });
            self.gui.clear_loading_site_for_site(site);
            // A volume boundary, so every pane on the site is stale. It is also
            // the reset that drops `level3_data` and `render_cache`.
            self.render.reset_panes_for_site(site, &self.gui);
            self.spawn_level3_fetches(site);
            self.chunk_feeds.record_tilt_freshness(site, &scan, sealed);
            // Here and not mid-volume: `append_polled_frame` dedupes by timestamp
            // and a `LoopFrame` has no "the scan got better" transition. The cache
            // behind this call is read product-blind and never re-downloaded, so
            // it takes `whole_volume_complete`.
            if closed.progress.whole_volume_complete {
                self.append_scan_to_active_loops(site, timestamp, scan, declared);
            } else {
                log::debug!(
                    "{site}: volume complete on the {} cut(s) the feed asked for but \
                     not whole, so it is not cached for the loops",
                    closed.progress.sealed_elevations.len()
                );
            }
        } else {
            self.gui
                .apply(squallar_egui::shell_api::GuiEvent::ChunkScanInfo {
                    site: site.to_owned(),
                    info,
                });
            self.gui.clear_loading_site_for_site(site);
            self.chunk_feeds.record_tilt_freshness(site, &scan, sealed);
            let hit = self
                .render
                .reset_panes_for_tilts(site, &self.gui, &outcome.sealed_angles);
            log::debug!(
                "{site}: cuts {:?} complete, {hit} pane(s) re-rendering",
                outcome.sealed_elevations
            );
        }
        // Absent on both paths: a `RadarConfig` event would drag the time picker
        // along, and `manual_nav_pending` would re-list the lookback per round.

        // Rebuild the shown panes' extraction payloads off the new snapshot, so
        // dispatch never takes a stale hit.
        self.refresh_extract_cache_for_site(site);
    }

    /// Hand a site back to the archive path.
    ///
    /// The fetch is unconditional rather than a `CheckForNewScans`: that check
    /// compares against `scan_info.timestamp`, which this feed has already
    /// advanced to the in-progress volume. It also does not go through
    /// `set_error`, which would reset the archive poll's backoff.
    fn fall_back_to_archive(&mut self, site: &str, reason: Retirement) {
        log::warn!("{site}: chunk feed retired ({reason:?}); refetching from the archive");
        let timestamp = Self::local_to_utc(self.gui.selected_timestamp());
        // The feed served the whole site and its replacement does too: no pane
        // asked for this, so every pane on the site takes it.
        self.spawn_fetch(site.to_string(), timestamp, FetchRequester::Site);
    }

    pub(super) fn any_pane_live_for_site(&self, site: &str) -> bool {
        (0..self.gui.pane_count()).any(|i| {
            self.gui
                .pane(i)
                .is_some_and(|p| p.site() == site && p.viewing_live)
        })
    }

    /// Whether the chunk feed is serving this site, so the 60 s archive check is
    /// redundant.
    pub(super) fn chunks_are_feeding(&self, site: &str) -> bool {
        squallar_egui::radar_layer::live_chunks_enabled(&self.gui)
            && self.chunk_feeds.is_feeding(site)
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

/// Who the live feed's volumes are for: every pane on the site that is
/// following live, and no one else.
#[path = "app_chunks/live_follow_tests.rs"]
#[cfg(test)]
mod live_follow_tests;
