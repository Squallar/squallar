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

    /// What this site's feed needs to download.
    ///
    /// The tilts its panes actually render — **unless** anything on the site
    /// shows a product [`rustdar_radar::types::RadarProduct::reads_whole_volume`]
    /// names, or *is* a pane whose kind
    /// ([`PaneKind::consumes_whole_volume`](rustdar_egui::pane::PaneKind::consumes_whole_volume))
    /// reads the whole ladder, in which case every cut is needed and the answer
    /// is `All`.
    ///
    /// Those exceptions are the whole safety of selective download, and neither
    /// is optional: every such product walks only the tilts *present* —
    /// `compute_echo_tops` clamps each column to the topmost one, a wind profile
    /// fits whatever velocity tilts it is handed — so all of them would read a
    /// volume that skipped cuts as a complete short one and produce a plausible,
    /// wrong answer with no error and no NaN to notice.
    ///
    /// Neither predicate is restated here. The product one was, once, and the
    /// restatement omitted storm-relative velocity: SRV panes narrowed their
    /// site's feed to one tilt while SRV went on fitting its dealias seed and
    /// its default Bunkers vector from the volume's velocity tilts, whichever
    /// of them had happened to be downloaded.
    ///
    /// # Two questions, and both have to be asked
    ///
    /// The product question is "does this field integrate the column?"; the
    /// pane-kind question is "does this view slice vertically?". A reflectivity
    /// cross-section answers *no* to the first — it is one moment, the same
    /// moment the plan view rasterizes — and *yes* to the second. So a copy of
    /// this loop that asked only about the product would narrow the site's feed
    /// to the section pane's nominal tilt and then let the section be
    /// interpolated between whichever cuts happened to arrive. That is the worst
    /// failure mode in the feature: a partial volume does not fail and does not
    /// produce a NaN, it produces a smooth, plausible, wrong slice, and it looks
    /// *better* than the truth because the gaps are bridged.
    ///
    /// The kind check is deliberately **above** the rendering-params guard below.
    /// A whole-volume pane with no resolvable params — no scan info yet, or a
    /// product whose elevations have not landed — contributes nothing under that
    /// guard, so a sibling map pane on the same site would be left to narrow the
    /// feed on its own and the section would be cut from what the *map* asked
    /// for. That window is the whole time between converting a pane and its
    /// volume arriving, which is exactly when the first section is cut.
    ///
    /// A pane with no resolvable render params and an ordinary map kind still
    /// contributes nothing rather than forcing `All`: it is showing nothing, so
    /// it needs nothing.
    fn cut_selection_for(&self, site: &str) -> rustdar_radar::chunks::CutSelection {
        use rustdar_radar::chunks::CutSelection;

        let mut tilts: Vec<f32> = Vec::new();
        for idx in 0..self.gui.pane_count() {
            if self.gui.pane(idx).is_none_or(|p| p.site != site) {
                continue;
            }
            // The pane-kind half of the whole-volume question. See this
            // function's own documentation for why it is asked here rather than
            // after the params guard, and why answering only the product half
            // silently mis-cuts a section.
            if self.gui.pane_consumes_whole_volume(idx) {
                return CutSelection::All;
            }
            let Some((product, elevation)) = self.gui.get_rendering_params_for_pane(idx) else {
                continue;
            };
            // Ahead of the Level III check, which is safe only because the two
            // sets are disjoint — no Level III product reads a Level II tilt, so
            // `reads_whole_volume` is false for every one of them. Were a product
            // ever both, this order would silently decide for it, and `All` is
            // the answer that would still be correct.
            if product.reads_whole_volume() {
                return CutSelection::All;
            }
            // Level III panes draw from `level3_data` and say nothing about which
            // Level II cuts are needed.
            if product.is_level3() {
                continue;
            }
            if !tilts.iter().any(|t| (t - elevation).abs() < 0.05) {
                tilts.push(elevation);
            }
        }
        // Nothing renderable on this site yet — take everything until something
        // says otherwise, so a site that has only just loaded is never starved.
        if tilts.is_empty() {
            return CutSelection::All;
        }
        CutSelection::Tilts(tilts)
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
            // Every pane on the site, whatever its product, and deliberately not
            // a narrower reset of the whole-volume readers alone. `closed` is set
            // by `ChunkPoller` at the instant it rolls the assembler, so this is a
            // volume *boundary*: every pane here is showing an image built from
            // the volume that just ended. The `if`/`else` also means this round's
            // own `sealed_elevations` never reach `reset_panes_for_tilts`, so this
            // is what stands in for them. And it is the reset that drops the
            // site's `level3_data` and `render_cache`, which the refetch below
            // needs — a pane-only reset would leave the previous volume's objects
            // and images to be handed straight back.
            self.render.reset_panes_for_site(site, &self.gui);
            self.spawn_level3_fetches(site);
            self.record_tilt_freshness(site, &scan, &outcome.sealed_elevations);
            // Only now: `append_polled_frame` dedupes by timestamp and a
            // `LoopFrame` has no "the scan got better" transition, so a frame
            // appended mid-volume would freeze on a one-cut volume forever.
            self.append_scan_to_active_loops(site, timestamp, scan);
        } else {
            self.gui.apply_chunk_scan_info(site, info);
            self.gui.clear_loading_site_for_site(site);
            self.record_tilt_freshness(site, &scan, &outcome.sealed_elevations);
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

    /// Reconnection must not be conditional on anything else being busy.
    ///
    /// Two source probes, because both halves are positional and neither has a
    /// type that could carry the requirement. `sync_sites` is the only thing that
    /// reopens a dropped socket and it only runs on a frame, so the frame has to
    /// keep coming while a reconnect is owed — and the notification driver has to
    /// sit ahead of the `enabled` gate, or turning the chunk feed off would
    /// strand the socket rather than narrowing it to the archive feed.
    #[test]
    fn a_down_socket_is_retried_regardless_of_other_activity() {
        let redraw = include_str!("app.rs");
        let arm = redraw
            .find("fn handle_redraw(")
            .map(|i| &redraw[i..])
            .expect("handle_redraw is gone from app.rs");
        assert!(
            arm.find("self.chunk_notify.reconnect_pending()")
                .is_some_and(|at| at
                    < arm
                        .find("notify_redraw(&self.window)")
                        .unwrap_or(usize::MAX)),
            "the re-arm dropped its reconnect term, so a socket that goes down \
             with auto-poll off is never retried"
        );

        let chunks = include_str!("app_chunks.rs");
        let drive = chunks
            .find("fn drive_chunk_feeds(")
            .map(|i| &chunks[i..])
            .expect("drive_chunk_feeds is gone");
        let notify = drive
            .find("self.drive_chunk_notifications(")
            .expect("the notification driver left drive_chunk_feeds");
        let gate = drive
            .find("if !enabled {")
            .expect("the enabled gate left drive_chunk_feeds");
        assert!(
            notify < gate,
            "notifications are driven behind the live-chunk gate, so turning the \
             feed off drops the archive socket and stops reconnecting"
        );
    }
}

#[cfg(test)]
mod selection_tests {
    use super::super::App;
    use super::super::tests::{headless, two_pane_app};
    use crate::platform_double::TestBridge;
    use rustdar_radar::chunks::CutSelection;
    use rustdar_radar::types::{RadarProduct, ScanInfo};

    /// One live pane on KTLX showing `product`, snapping within `available`.
    fn app_showing(product: RadarProduct, selected: f32, available: &[f32]) -> App {
        let mut app = headless(TestBridge::desktop());
        show(&mut app, product, selected, available);
        app
    }

    /// Re-point the pane an existing app already has, so a per-product sweep
    /// does not stand a `wgpu` instance up once per variant.
    fn show(app: &mut App, product: RadarProduct, selected: f32, available: &[f32]) {
        let pane = app.gui.pane_mut(0).unwrap();
        pane.site = "KTLX".to_string();
        pane.viewing_live = true;
        pane.selected_product = product;
        pane.selected_elevation = selected;
        let mut product_elevations = std::collections::HashMap::new();
        product_elevations.insert(product, available.to_vec());
        pane.scan_info = Some(ScanInfo {
            site: rustdar_radar::sites::RadarSite {
                name: "KTLX",
                lat: 35.3,
                lon: -97.3,
                elev: None,
            },
            timestamp: chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            vcp_number: 212,
            available_products: vec![product],
            product_elevations,
            status: String::new(),
        });
    }

    /// The ordinary case, and where the traffic saving comes from.
    #[test]
    fn a_single_tilt_pane_asks_for_only_its_tilt() {
        let app = app_showing(RadarProduct::Reflectivity, 0.5, &[0.5, 1.5, 4.0]);
        assert_eq!(
            app.cut_selection_for("KTLX"),
            CutSelection::Tilts(vec![0.5])
        );
    }

    /// The guard the whole feature's safety rests on. `compute_echo_tops` clamps
    /// every column to the topmost tilt present, so a volume that skipped cuts
    /// would give it a plausible, low, wrong answer with nothing to notice.
    #[test]
    fn a_volumetric_pane_forces_the_whole_volume() {
        let app = app_showing(RadarProduct::EchoTopsInterpolated, 0.5, &[0.5]);
        assert_eq!(app.cut_selection_for("KTLX"), CutSelection::All);
    }

    /// NROT fits its wind profile from every velocity tilt — the only wind
    /// source since the NVW fetch left — so it is volume-wide for the same
    /// reason.
    #[test]
    fn an_nrot_pane_forces_the_whole_volume() {
        let app = app_showing(RadarProduct::NormalizedRotation, 0.5, &[0.5]);
        assert_eq!(app.cut_selection_for("KTLX"), CutSelection::All);
    }

    /// And the one the divergence actually bit. SRV's dealias seeding wants that
    /// same profile, and the profile is also where its default Bunkers vector
    /// comes from, so narrowing the feed under it fitted both from whichever
    /// velocity tilts happened to have been downloaded.
    #[test]
    fn an_srv_pane_forces_the_whole_volume() {
        let app = app_showing(RadarProduct::StormRelativeVelocity, 0.5, &[0.5]);
        assert_eq!(app.cut_selection_for("KTLX"), CutSelection::All);
    }

    /// Every product there is, against the one predicate that decides this.
    ///
    /// The bug this replaces was a second copy of that predicate living here and
    /// disagreeing with the first, so what is checked is the *behaviour* of the
    /// selection against [`RadarProduct::reads_whole_volume`]: a third copy, or a
    /// special case bolted onto the loop, fails this whichever product it omits.
    ///
    /// A tilt list is safe for exactly one kind of pane: a Level II product that
    /// reads the single sweep `find_sweep` picks. A volume integral needs every
    /// cut, and a Level III pane says nothing about which Level II cuts are
    /// needed, so neither of those may narrow the feed.
    #[test]
    fn only_a_one_sweep_level_two_pane_narrows_the_feed() {
        let mut app = headless(TestBridge::desktop());
        for &product in RadarProduct::all() {
            show(&mut app, product, 0.5, &[0.5]);
            let selection = app.cut_selection_for("KTLX");
            assert_eq!(
                matches!(&selection, CutSelection::Tilts(_)),
                !product.reads_whole_volume() && !product.is_level3(),
                "{product:?}: the feed was asked for {selection:?}",
            );
        }
    }

    // Not tested here: that one volumetric pane among several outweighs the
    // rest. It needs a multi-pane `Gui`, and `set_pane_count_for_test` is
    // `#[cfg(test)]` inside `rustdar-egui` so it does not exist for this crate.
    // The single-pane tests above cover both branches of the decision, and the
    // loop returns `All` on the first volumetric pane it meets.

    /// The other half of the whole-volume question: the pane's **kind**.
    ///
    /// This is the single place a live cross-section can go quietly wrong. The
    /// product half above asks "does this field integrate the column?" and a
    /// reflectivity section answers *no* — it is one moment, the same moment the
    /// plan view rasterizes. The pane-kind half asks "does this view slice
    /// vertically?", and there the answer is yes. Ask only the product and the
    /// site's feed narrows to the section pane's nominal tilt, after which the
    /// section is interpolated between whichever cuts happened to arrive: no
    /// error, no NaN, and a smooth plausible layer where there is no data at all.
    /// It looks *better* than the truth, which is what makes it the worst failure
    /// mode in the feature.
    ///
    /// Driven with a product that is deliberately `Tilts`-worthy on its own —
    /// asserted as a precondition — so the `All` below can only have come from
    /// the kind.
    #[test]
    fn a_whole_volume_pane_kind_forces_the_whole_volume() {
        use rustdar_egui::pane::PaneKind;

        for kind in [PaneKind::CrossSection, PaneKind::Volume] {
            let mut app = app_showing(RadarProduct::Reflectivity, 0.5, &[0.5, 1.5, 4.0]);
            assert_eq!(
                app.cut_selection_for("KTLX"),
                CutSelection::Tilts(vec![0.5]),
                "precondition: as a map pane this selection narrows the feed, so \
                 the kind is the only thing that can widen it below"
            );

            app.gui.pane_mut(0).unwrap().set_kind(kind);

            assert_eq!(
                app.cut_selection_for("KTLX"),
                CutSelection::All,
                "{kind:?}: the feed was narrowed under a pane that reads every cut"
            );
        }
    }

    /// The kind is asked **before** the rendering-params guard, not after it.
    ///
    /// A whole-volume pane whose params do not resolve — no scan info yet, or a
    /// product whose elevations have not landed — falls straight through that
    /// guard contributing nothing. Behind the guard, a sibling map pane on the
    /// same site is then left to narrow the feed on its own and the section gets
    /// cut from whatever the *map* asked for. The window is the whole time
    /// between converting a pane and its volume arriving, which is exactly when
    /// the first section is cut.
    ///
    /// It needs **two** panes to bite, and that is why. With the section pane
    /// alone, the tilt list ends up empty and the nothing-renderable fallback at
    /// the bottom returns `All` anyway — so a single-pane version of this test
    /// passes with the check on either side of the guard and proves nothing. It
    /// takes a sibling supplying a real tilt for the ordering to be observable,
    /// which is also precisely the arrangement a user is in.
    #[test]
    fn a_whole_volume_pane_with_no_scan_yet_still_forces_the_whole_volume() {
        use rustdar_egui::pane::PaneKind;

        let mut app = two_pane_app("KTLX", "KTLX");
        // Pane 0: an ordinary map pane on KTLX with a resolvable tilt.
        show(&mut app, RadarProduct::Reflectivity, 0.5, &[0.5, 1.5]);
        // Pane 1: a section on the same site, still waiting for its scan.
        app.gui
            .pane_mut(1)
            .unwrap()
            .set_kind(PaneKind::CrossSection);
        assert!(
            app.gui.get_rendering_params_for_pane(1).is_none(),
            "precondition: with no scan info the params guard is what this pane \
             would otherwise hit"
        );
        assert!(
            app.gui.get_rendering_params_for_pane(0).is_some(),
            "precondition: the sibling must supply a tilt, or the empty-list \
             fallback returns All whichever side of the guard the check is on"
        );

        assert_eq!(app.cut_selection_for("KTLX"), CutSelection::All);
    }

    /// A section pane on one site does not widen another site's feed.
    ///
    /// The kind check sits inside the per-site loop, *after* the site test.
    /// Hoisted out of it — or written as "any pane anywhere is a section" — every
    /// site in a multi-site layout would start downloading every cut the moment
    /// one pane anywhere became a section, and the traffic saving the whole
    /// selective feed exists for would be gone.
    ///
    /// Two panes, built through `Gui::load_ui_config`. That is the only public
    /// route to a multi-pane `Gui` from this crate —
    /// `Gui::set_pane_count_for_test` is `#[cfg(test)]` inside `rustdar-egui`, so
    /// it does not exist here — and it is why the older tests in this module
    /// could only cover the single-pane branches.
    #[test]
    fn a_whole_volume_pane_widens_only_its_own_site() {
        use rustdar_egui::pane::PaneKind;

        let mut app = two_pane_app("KTLX", "KOUN");
        show(&mut app, RadarProduct::Reflectivity, 0.5, &[0.5, 1.5]);
        app.gui
            .pane_mut(1)
            .unwrap()
            .set_kind(PaneKind::CrossSection);
        assert_eq!(
            app.cut_selection_for("KTLX"),
            CutSelection::Tilts(vec![0.5]),
            "the section pane on KOUN widened KTLX's feed"
        );

        // The counterweight: the section pane's *own* site does widen, so the
        // assertion above is about the site term rather than about a check that
        // never fires.
        assert_eq!(app.cut_selection_for("KOUN"), CutSelection::All);
    }

    /// A site with nothing renderable takes everything rather than starving.
    #[test]
    fn a_site_with_nothing_to_render_asks_for_everything() {
        let app = headless(TestBridge::desktop());
        assert_eq!(app.cut_selection_for("KTLX"), CutSelection::All);
    }

    /// Another site's panes never narrow this one's feed.
    #[test]
    fn the_selection_is_per_site() {
        let app = app_showing(RadarProduct::Reflectivity, 0.5, &[0.5, 4.0]);
        assert_eq!(app.cut_selection_for("KOUN"), CutSelection::All);
    }
}
