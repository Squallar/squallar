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
        let status = self.chunk_feeds.status(&live, enabled);
        self.gui.set_chunk_status(status);
        if !enabled {
            return;
        }
        // Narrower than `evict_unshown_scans`: a feed has no reader once no pane
        // is live on its site. See `ChunkFeedManager::retain_live`.
        self.chunk_feeds.retain_live(&live);

        for site in live {
            self.chunk_feeds.ensure(&site);
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
    fn apply_chunk_outcome(&mut self, site: &str, outcome: &rustdar_radar::chunks::PollOutcome) {
        let volume_complete = outcome
            .closed
            .as_ref()
            .is_some_and(|closed| closed.volume_complete);
        if outcome.sealed_elevations.is_empty() && !volume_complete {
            return;
        }

        let Some(scan) = self.chunk_feeds.snapshot(site) else {
            return;
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

        if volume_complete {
            // The volume is now exactly what the archive would have published,
            // so the steady state matches it — including the Level III refetch
            // that re-registers the tilts a merge preserved mid-volume.
            self.gui.set_scan_info_for_site(site, info);
            self.gui.clear_loading_site_for_site(site);
            self.render.reset_panes_for_site(site, &self.gui);
            self.spawn_level3_fetches(site);
            // Only now: `append_polled_frame` dedupes by timestamp and a
            // `LoopFrame` has no "the scan got better" transition, so a frame
            // appended mid-volume would freeze on a one-cut volume forever.
            self.append_scan_to_active_loops(site, timestamp, scan);
        } else {
            self.gui.apply_chunk_scan_info(site, info);
            self.gui.clear_loading_site_for_site(site);
            let have_winds = self.render.vwp_levels.contains_key(site);
            let hit = self.render.reset_panes_for_tilts(
                site,
                &self.gui,
                &outcome.sealed_angles,
                have_winds,
            );
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

#[cfg(test)]
mod tests {
    /// The chunk drain and driver must run inside `poll_data_channels`, which
    /// `handle_redraw` calls before `evict_unshown_scans` and before
    /// `setup_egui_frame` lays the frame out.
    ///
    /// A source probe because no type system expresses it: the drain is what
    /// makes a newly assembled volume the one `dispatch_pane_renders` reads, and
    /// `evict_unshown_scans` would drop a volume stored after it ran. The
    /// sibling guarantee for the pollers inside `setup_egui_frame` is pinned by
    /// `app_render::tests::every_poller_runs_before_the_frame_is_laid_out`; this
    /// is the half that lives outside it.
    #[test]
    fn the_chunk_drain_runs_before_the_frame_is_laid_out() {
        let source = include_str!("app.rs");
        let body = |name: &str| {
            let start = source
                .find(name)
                .unwrap_or_else(|| panic!("{name} is gone from app.rs"));
            let rest = &source[start..];
            let open = rest.find('{').expect("a body");
            let mut depth = 0usize;
            for (i, c) in rest[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return rest[open..open + i].to_string();
                        }
                    }
                    _ => {}
                }
            }
            panic!("unbalanced braces in {name}");
        };

        let poll = body("fn poll_data_channels(");
        let drain = poll.find("self.poll_chunk_results(").expect(
            "the chunk drain left poll_data_channels, so a volume it \
                     assembles can be evicted before anything draws it",
        );
        let drive = poll
            .find("self.drive_chunk_feeds(")
            .expect("the chunk driver left poll_data_channels");
        assert!(
            drain < drive,
            "a round is dispatched before the finished one is applied, so every \
             volume waits an extra frame"
        );

        let redraw = body("fn handle_redraw(");
        let at = |needle: &str| {
            redraw
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} is gone from handle_redraw"))
        };
        assert!(
            at("self.poll_data_channels(") < at("self.evict_unshown_scans("),
            "a volume the chunk drain stores is evicted in the same frame"
        );
        assert!(
            at("self.poll_data_channels(") < at("self.setup_egui_frame("),
            "the frame is laid out before the chunk drain has applied anything"
        );
    }
}
