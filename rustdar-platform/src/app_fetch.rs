use chrono::NaiveDateTime;
use chrono::TimeZone;
use std::sync::atomic::Ordering;
use winit::event_loop::ActiveEventLoop;
use rustdar_egui::actions::GuiAction;
use crate::channels::{ScanResponse, ScanData, Level3Response, OverlayRenderResponse};
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayKind};
use crate::constants::MAX_CONCURRENT_RENDERS;
use crate::render_dispatch::RenderGuard;
use rustdar_radar::types::{IMAGE_SIZE, RadarProduct};

/// Parameters for a background overlay rasterization request.
pub(super) struct OverlayRenderRequest {
    /// The pane's viewport, *before* overdraw is applied.
    pub geo_bounds: rustdar_overlays::types::GeoBounds,
    /// Pixel dimensions and the overdraw fraction they were sized for, already
    /// reconciled with the adapter's `max_texture_dimension_2d` by
    /// `rustdar_egui::overlay_cache::plan_overlay_texture`.
    pub texture: rustdar_egui::overlay_cache::OverlayTexturePlan,
    pub data_generation: u64,
    pub zoom: i32,
}
use rustdar_radar::scan;
use std::future::Future;
use std::sync::mpsc::Sender;

impl super::App {

    /// Spawn an async task on the Tokio runtime that sends its result through
    /// a channel and requests a redraw when complete.
    ///
    /// The caller builds the full future (including error handling and result
    /// construction). This helper handles cloning the window handle, spawning
    /// on the runtime, sending the result, and calling `notify_redraw()`.
    fn spawn_async_task<T: Send + 'static>(
        &self,
        sender: Sender<T>,
        future: impl Future<Output = T> + Send + 'static,
    ) {
        let window = self.window.clone();
        self.tokio_runtime.spawn(async move {
            let result = future.await;
            let _ = sender.send(result);
            super::notify_redraw(&window);
        });
    }

    /// Spawn an async radar data fetch on the background runtime.
    /// Handles generation tracking, result sending, and redraw requests.
    pub fn spawn_fetch(&mut self, site: String, timestamp: NaiveDateTime) {
        let generation = self.render.next_fetch_generation(&site);
        self.spawn_async_task(self.channels.scan_sender.clone(), async move {
            log::info!("Fetching {} @ {} UTC", site, timestamp);
            let msg = match scan::get_scan(&site, timestamp).await {
                Ok(data) => {
                    log::info!("Fetched scan: {} @ {}", site, timestamp);
                    Ok(ScanData { scan: data, site: site.clone(), timestamp })
                }
                Err(e) => {
                    let err = format!("Failed to fetch radar scan: {:?}", e);
                    log::error!("{}", err);
                    Err(err)
                }
            };
            ScanResponse { generation, site, result: msg, is_auto_poll: false }
        });
    }

    /// Spawn Level III product fetches for all supported Level III products.
    /// Called after a Level II scan loads so the products are available
    /// alongside the base moments.
    pub(super) fn spawn_level3_fetches(&self, site: &str) {
        let generation = self.render.fetch_generations.get(site).copied().unwrap_or(0);
        for l3_product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
            let Some(dirs) = l3_product.tgftp_dirs() else { continue };
            for &dir in dirs {
                let site = site.to_string();
                let dir_str = dir.to_string();
                let product = *l3_product;
                self.spawn_async_task(self.channels.level3_sender.clone(), async move {
                    log::info!("Fetching TGFTP {} for {}", dir_str, site);
                    let result = match scan::get_tgftp_product(&site, &dir_str).await {
                        Ok(msg) => {
                            log::info!("Fetched TGFTP {} for {}", dir_str, site);
                            Ok(msg)
                        }
                        Err(e) => {
                            log::warn!("TGFTP {} fetch failed: {}", dir_str, e);
                            Err(format!("{e}"))
                        }
                    };
                    Level3Response { generation, product, tilt_code: dir_str, site, result }
                });
            }
        }
    }

    fn local_to_utc(timestamp: NaiveDateTime) -> NaiveDateTime {
        let local_dt = chrono::Local
            .from_local_datetime(&timestamp)
            .latest()
            .unwrap_or_else(chrono::Local::now);
        local_dt.with_timezone(&chrono::Utc).naive_utc()
    }

    pub(super) fn handle_gui_action(&mut self, action: GuiAction, event_loop: Option<&ActiveEventLoop>) {
        match action {
            GuiAction::FetchRadarScan(_)
            | GuiAction::CheckForNewScans(_)
            | GuiAction::SwitchRadarSite { .. } => self.handle_radar_action(action),
            GuiAction::Exit => {
                self.request_exit(event_loop);
            }
            GuiAction::FetchOverlay { .. }
            | GuiAction::RefreshOverlay { .. } => self.handle_overlay_action(action),
            GuiAction::RenderOverlay { .. } => {
                // Handled in process_gui_actions() with deduplication
                unreachable!("RenderOverlay should be intercepted by process_gui_actions");
            }
            GuiAction::EnableLoop { pane_idx, lookback_secs } => {
                self.handle_enable_loop(pane_idx, lookback_secs);
            }
            GuiAction::DisableLoop { pane_idx } => {
                self.handle_disable_loop(pane_idx);
            }
            GuiAction::ToggleLoopPlayback { pane_idx } => {
                if let Some(pane) = self.gui.pane_mut(pane_idx) {
                    let ls = &mut pane.loop_state;
                    match ls.phase {
                        rustdar_egui::pane::LoopPhase::Playing => {
                            ls.phase = rustdar_egui::pane::LoopPhase::Paused;
                        }
                        rustdar_egui::pane::LoopPhase::Ready | rustdar_egui::pane::LoopPhase::Paused => {
                            ls.phase = rustdar_egui::pane::LoopPhase::Playing;
                            ls.last_advance = Some(std::time::Instant::now());
                        }
                        _ => {}
                    }
                }
            }
            GuiAction::StepLoopFrame { pane_idx, forward } => {
                if let Some(pane) = self.gui.pane_mut(pane_idx) {
                    let ls = &mut pane.loop_state;
                    if !ls.frames.is_empty() {
                        if forward {
                            ls.current_frame = (ls.current_frame + 1) % ls.frames.len();
                        } else if ls.current_frame == 0 {
                            ls.current_frame = ls.frames.len() - 1;
                        } else {
                            ls.current_frame -= 1;
                        }
                    }
                }
            }
            GuiAction::SeekLoopFrame { pane_idx, frame_index } => {
                if let Some(pane) = self.gui.pane_mut(pane_idx) {
                    let ls = &mut pane.loop_state;
                    if frame_index < ls.frames.len() {
                        ls.current_frame = frame_index;
                    }
                }
            }
            GuiAction::NavigateTime { pane_idx, step_secs } => {
                self.handle_navigate_time(pane_idx, step_secs);
            }
            GuiAction::NavigateOneScan { pane_idx, forward } => {
                self.handle_navigate_one_scan(pane_idx, forward);
            }
            GuiAction::JumpToLive { pane_idx } => {
                self.handle_jump_to_live(pane_idx);
            }
            GuiAction::StartGps { config } => {
                self.platform.start_gps(&config);
            }
            GuiAction::StopGps => {
                self.platform.stop_gps();
            }
        }
    }

    /// Handle radar data fetch/switch actions.
    fn handle_radar_action(&mut self, action: GuiAction) {
        match action {
            GuiAction::FetchRadarScan(radar_config) => {
                log::info!(
                    "Fetch radar scan requested: {} @ {} (local)",
                    radar_config.site,
                    radar_config.timestamp
                );
                let utc_timestamp = Self::local_to_utc(radar_config.timestamp);
                self.spawn_fetch(radar_config.site, utc_timestamp);
            }
            GuiAction::CheckForNewScans(radar_config) => {
                log::info!(
                    "Check for new scans: {} @ {} (local)",
                    radar_config.site,
                    radar_config.timestamp
                );

                let utc_timestamp = Self::local_to_utc(radar_config.timestamp);
                let current_scan_timestamp = self.gui.get_scan_info().map(|info| info.timestamp);

                let generation = self.render.next_fetch_generation(&radar_config.site);
                let site = radar_config.site.clone();
                let window = self.window.clone();
                let sender = self.channels.scan_sender.clone();

                // Not using spawn_async_task: conditional send (only on new data)
                self.tokio_runtime.spawn(async move {
                    match scan::check_and_fetch_latest(&site, &utc_timestamp.date(), current_scan_timestamp).await {
                        Ok(Some((data, timestamp))) => {
                            let _ = sender.send(crate::channels::ScanResponse {
                                generation,
                                site: site.clone(),
                                result: Ok(crate::channels::ScanData { scan: data, site, timestamp }),
                                is_auto_poll: true,
                            });
                        }
                        Ok(None) => { /* already latest or no data */ }
                        Err(e) => {
                            log::error!("Failed to check for new scans: {:?}", e);
                        }
                    }
                    crate::app::notify_redraw(&window);
                });
            }
            GuiAction::SwitchRadarSite { site, pane_idx } => {
                log::info!("Switch radar site requested: pane {} -> {}", pane_idx, site);
                
                let mut new_config = self.gui.get_radar_config().clone();
                new_config.site = site.clone();
                self.gui.set_radar_config(new_config.clone());

                if self.gui.is_sync_layers() {
                    // Sync ON: update all panes to the new site
                    for idx in 0..self.gui.pane_count() {
                        if let Some(pane) = self.gui.pane_mut(idx) {
                            pane.loading_site = Some(site.clone());
                            pane.site = site.clone();
                            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
                            pane.loop_state = rustdar_egui::pane::LoopPlaybackState::new();
                        }
                    }
                } else {
                    // Sync OFF: only update the target pane
                    if let Some(pane) = self.gui.pane_mut(pane_idx) {
                        pane.loading_site = Some(site.clone());
                        pane.site = site.clone();
                        pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
                        pane.loop_state = rustdar_egui::pane::LoopPlaybackState::new();
                    }
                }
                self.loop_mgr.clear_all();
                
                let utc_timestamp = Self::local_to_utc(new_config.timestamp);
                self.spawn_fetch(site, utc_timestamp);
            }
            _ => unreachable!(),
        }
    }

    /// Handle overlay fetch/refresh actions for all overlay kinds.
    fn handle_overlay_action(&mut self, action: GuiAction) {
        match action {
            GuiAction::FetchOverlay { kind, pane_idx } | GuiAction::RefreshOverlay { kind, pane_idx } => {
                self.fetch_overlay(kind, pane_idx);
            }
            _ => unreachable!(),
        }
    }

    /// Fetch overlay data for the given kind, resolving parameters from current state.
    fn fetch_overlay(&mut self, kind: OverlayKind, pane_idx: usize) {
        use rustdar_overlays::render::overlay_state::FetchConfig;

        // Load the requesting pane's config so create_fetch_tasks reads the
        // correct per-pane settings (e.g. selected model parameter, SPC day).
        let pane_configs = self.gui.pane(pane_idx)
            .map(|p| p.overlay_configs.clone())
            .unwrap_or_default();
        if !pane_configs.is_empty() {
            self.gui.overlays.load_pane_configs(&pane_configs);
        }

        let config = FetchConfig {
            client: self.http_client.clone(),
            zone_cache_dir: self.platform.zone_cache_dir().map(|p| p.to_path_buf()),
        };

        let tasks = self.gui.overlays.create_fetch_tasks(kind, &config);
        if tasks.is_empty() {
            return;
        }

        log::info!("Fetching overlay data for {:?} ({} task(s))", kind, tasks.len());
        self.gui.overlays.set_fetching(kind, true);

        for task in tasks {
            let task_kind = task.kind;
            self.spawn_async_task(self.channels.overlay_fetch_sender.clone(), async move {
                let data = task.future.await;
                OverlayFetchResult { kind: task_kind, data }
            });
        }
    }

    /// Spawn a background thread to rasterize overlay polygons via tiny-skia.
    pub(super) fn spawn_overlay_render(
        &mut self,
        pane_indices: Vec<usize>,
        kind: OverlayKind,
        req: OverlayRenderRequest,
    ) {
        use rustdar_overlays::render::rasterize;
        use rustdar_egui::overlay_cache::ZOOM_QUANTIZATION_FACTOR;

        let OverlayRenderRequest { geo_bounds, texture, data_generation, zoom } = req;
        let (width, height) = (texture.width, texture.height);

        if width == 0 || height == 0 {
            return;
        }

        if self.gui.overlays.render_mode(kind) != Some(rustdar_overlays::render::overlay_state::RenderMode::Texture) {
            log::warn!("spawn_overlay_render called with non-texture kind: {:?}", kind);
            return;
        }

        // Mark in-flight on the appropriate texture cache for all target panes
        for &pidx in &pane_indices {
            if let Some(pane) = self.gui.pane_mut(pidx) {
                pane.overlay_cache_mut(kind).render_in_flight = true;
            }
        }

        // The plan answers this itself. There is no fraction to pass, so the one
        // substitution that would break the cache — `OVERDRAW_FRACTION` in place of
        // what the adapter actually allowed — cannot be written here.
        let render_bounds = texture.coverage(&geo_bounds);

        // Use the first target pane for data extraction (all synced panes share config).
        // Clone the pane's overlay config before mutating the registry.
        let first_pane_idx = pane_indices[0];
        let pane_configs = {
            let Some(target_pane) = self.gui.pane(first_pane_idx) else { return };
            target_pane.overlay_configs.clone()
        };
        if !pane_configs.is_empty() {
            self.gui.overlays.load_pane_configs(&pane_configs);
        }

        let sender = self.channels.overlay_render_sender.clone();
        let window = self.window.clone();

        // Clone the data needed for the render closure
        match kind {
            // Handler-backed texture overlays: use prepare_rasterize
            OverlayKind::SpcOutlook
            | OverlayKind::SpcDiscussions
            | OverlayKind::NwsAlerts
            | OverlayKind::StormReports
            | OverlayKind::Lightning
            | OverlayKind::ModelData => {
                let rctx = rustdar_overlays::render::overlay_state::RasterizeContext {
                    is_dark: self.cached_dark_theme.unwrap_or(false),
                    zoom: zoom as f64 / ZOOM_QUANTIZATION_FACTOR,
                };
                let Some(rasterize_fn) = self.gui.overlays.prepare_rasterize(kind, &rctx) else {
                    // Nothing to render — clear in-flight
                    for &pidx in &pane_indices {
                        if let Some(pane) = self.gui.pane_mut(pidx) {
                            pane.overlay_cache_mut(kind).render_in_flight = false;
                        }
                    }
                    return;
                };
                std::thread::Builder::new()
                    .name("overlay-render".into())
                    .spawn(move || {
                    let output = rasterize_fn(&render_bounds, width, height);
                    let _ = sender.send(OverlayRenderResponse {
                        image_data: output.rgba,
                        width,
                        height,
                        geo_bounds: render_bounds,
                        overlay_kind: kind,
                        generation: data_generation,
                        pane_indices,
                        zoom,
                        hit_map: output.hit_map,
                    });
                    super::notify_redraw(&window);
                }).expect("failed to spawn overlay-render thread");
            }
            OverlayKind::RadarSites => {
                let Some(target_pane) = self.gui.pane(first_pane_idx) else { return };
                let target_site = target_pane.site.clone();
                let target_loading = target_pane.loading_site.clone();
                let is_dark = self.cached_dark_theme.unwrap_or(false);
                let actual_zoom = zoom as f64 / ZOOM_QUANTIZATION_FACTOR;
                let sites: Vec<rasterize::RadarSiteInfo> = rustdar_radar::sites::RADARS.iter().map(|s| {
                    rasterize::RadarSiteInfo {
                        name: s.name.to_string(),
                        lat: s.lat,
                        lon: s.lon,
                        is_current: s.name == target_site,
                        is_loading: target_loading.as_deref() == Some(s.name),
                    }
                }).collect();
                std::thread::Builder::new()
                    .name("sites-render".into())
                    .spawn(move || {
                    let image_data = rasterize::rasterize_radar_sites(
                        &sites,
                        &render_bounds,
                        width,
                        height,
                        actual_zoom,
                        is_dark,
                    );
                    let _ = sender.send(OverlayRenderResponse {
                        image_data,
                        width,
                        height,
                        geo_bounds: render_bounds,
                        overlay_kind: kind,
                        generation: data_generation,
                        pane_indices,
                        zoom,
                        hit_map: None,
                    });
                    super::notify_redraw(&window);
                }).expect("failed to spawn sites-render thread");
            }
            // Non-texture overlay kinds are never dispatched for background rendering.
            OverlayKind::Radar | OverlayKind::CityLabels
            | OverlayKind::UserLocation | OverlayKind::Metar
            | OverlayKind::ColorScale  => {
                log::warn!("spawn_overlay_render called with non-texture kind: {:?}", kind);
            }
        }
    }

    /// Enable radar loop for a pane: initializes loop state and spawns
    /// an async task to list available scans in the lookback window.
    ///
    /// Everything except the spawn lives in [`begin_loop_for_pane`], so which pane
    /// the loop is read from is one decision, made and tested in one place.
    fn handle_enable_loop(&mut self, pane_idx: usize, lookback_secs: u64) {
        let Some(request) = begin_loop_for_pane(self.gui.panes_mut(), &mut self.loop_mgr, pane_idx, lookback_secs)
        else {
            return;
        };
        let LoopScanRequest { site, start, end } = request;

        self.spawn_async_task(self.channels.loop_scan_list_sender.clone(), async move {
            match scan::list_scans_for_range(&site, start, end).await {
                Ok(scans) => {
                    log::info!(
                        "Loop: found {} {} scans in range for pane {}",
                        scans.len(), site, pane_idx
                    );
                    crate::channels::LoopScanListResponse { pane_idx, site, scans }
                }
                Err(e) => {
                    log::error!("Loop scan listing failed for {}: {:?}", site, e);
                    // Send empty list so UI can show error state
                    crate::channels::LoopScanListResponse { pane_idx, site, scans: Vec::new() }
                }
            }
        });
    }

    /// Disable radar loop for a pane: resets to single-frame mode.
    fn handle_disable_loop(&mut self, pane_idx: usize) {
        if let Some(pane) = self.gui.pane_mut(pane_idx) {
            pane.loop_state = rustdar_egui::pane::LoopPlaybackState::new();
        }
        self.loop_mgr.remove_pending(pane_idx);
        // Clear last_rendered so dispatch_pane_renders will re-apply the
        // cached static render (or spawn a fresh one) on the next frame.
        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].last_rendered = None;
        }
        // Global scan cache and download tracking are left intact for other panes.
        // Stale entries are cleaned up lazily when no pane references them.
    }

    /// Navigate by a relative time step (seconds). Positive = forward, negative = backward.
    fn handle_navigate_time(&mut self, pane_idx: usize, step_secs: i64) {
        let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else { return };
        let site = scan_info.site.name.to_string();
        let current_utc = scan_info.timestamp;

        let target = current_utc + chrono::Duration::seconds(step_secs);
        let now_utc = chrono::Utc::now().naive_utc();

        // Cap forward navigation to now; if capped, we're "live" again
        let (target, is_live) = if step_secs > 0 && target >= now_utc {
            (now_utc, true)
        } else {
            (target, false)
        };

        self.gui.set_viewing_live_for_pane(pane_idx, is_live);
        self.manual_nav_pending = true;

        // Update the UI config timestamp (local time for display)
        let local_ts = chrono::TimeZone::from_utc_datetime(&chrono::Local, &target).naive_local();
        let mut config = self.gui.get_radar_config().clone();
        config.timestamp = local_ts;
        self.gui.set_radar_config(config);
        self.gui.set_fetching(true);

        self.spawn_fetch(site, target);
    }

    /// Navigate to the next or previous adjacent scan on AWS.
    fn handle_navigate_one_scan(&mut self, pane_idx: usize, forward: bool) {
        let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else { return };
        let site = scan_info.site.name.to_string();
        let current_utc = scan_info.timestamp;

        self.manual_nav_pending = true;
        self.gui.set_fetching(true);

        let generation = self.render.next_fetch_generation(&site);

        self.spawn_async_task(self.channels.scan_sender.clone(), async move {
            match scan::get_adjacent_scan(&site, current_utc, forward).await {
                Ok((data, timestamp)) => {
                    crate::channels::ScanResponse {
                        generation,
                        site: site.clone(),
                        result: Ok(crate::channels::ScanData { scan: data, site, timestamp }),
                        is_auto_poll: false,
                    }
                }
                Err(e) => {
                    let err = format!("Failed to find adjacent scan: {:?}", e);
                    log::error!("{}", err);
                    crate::channels::ScanResponse {
                        generation,
                        site,
                        result: Err(err),
                        is_auto_poll: false,
                    }
                }
            }
        });
    }

    /// Jump back to live mode: apply any cached auto-poll scan, or fetch latest.
    fn handle_jump_to_live(&mut self, pane_idx: usize) {
        self.gui.set_viewing_live_for_pane(pane_idx, true);
        self.manual_nav_pending = true;

        // Get the pane's site to check for cached scan
        let pane_site = self.gui.pane(pane_idx).map(|p| p.site.clone()).unwrap_or_default();

        if let Some((scan_arc, scan_info, timestamp)) = self.latest_cached_scans.remove(&pane_site) {
            log::info!("JumpToLive: using cached scan for {} @ {}", pane_site, timestamp);
            self.scan_data.insert(pane_site.clone(), scan_arc);

            let local_ts = chrono::TimeZone::from_utc_datetime(&chrono::Local, &timestamp).naive_local();
            let mut config = self.gui.get_radar_config().clone();
            config.timestamp = local_ts;
            self.gui.set_radar_config(config);
            self.gui.set_scan_info_for_site(&pane_site, scan_info);
            self.gui.clear_loading_site_for_site(&pane_site);
            self.render.reset_panes_for_site(&pane_site, &self.gui);
            self.spawn_level3_fetches(&pane_site);

            self.manual_nav_pending = false;
            self.reinit_active_loops();
            return;
        }

        // No cached scan for this site — fetch latest
        let now = chrono::Local::now().naive_local();
        let mut config = self.gui.get_radar_config().clone();
        config.timestamp = now;
        self.gui.set_radar_config(config);
        self.gui.set_fetching(true);

        let utc_timestamp = Self::local_to_utc(now);
        self.spawn_fetch(pane_site, utc_timestamp);
    }

    /// Spawn a download task for a single loop frame scan.
    ///
    /// `site` is the site the requesting pane's loop is on, and is echoed on the
    /// response: it is half the key the scan is cached and looked up under, so it
    /// has to travel with the scan rather than being re-read from the pane, whose
    /// loop may be rebuilt for another site before this lands.
    pub(super) fn spawn_loop_scan_download(
        &self,
        pane_idx: usize,
        site: String,
        timestamp: NaiveDateTime,
        identifier: nexrad_data::aws::archive::Identifier,
    ) {
        self.spawn_async_task(self.channels.loop_scan_download_sender.clone(), async move {
            let scan = match scan::download_scan(identifier).await {
                Ok(scan_data) => Some(std::sync::Arc::new(scan_data)),
                Err(e) => {
                    log::error!(
                        "Loop scan download failed for pane {} ({} @ {}): {:?}",
                        pane_idx, site, timestamp, e
                    );
                    None
                }
            };
            crate::channels::LoopScanDownloadResponse {
                pane_idx,
                site,
                timestamp,
                scan,
            }
        });
    }

    /// Spawn a background render thread for a single loop frame.
    ///
    /// Returns `true` if a render thread was spawned. `false` means the shared
    /// concurrency budget was exhausted and nothing was started — the caller must
    /// not mark the frame as in flight, since no response will arrive to clear it.
    ///
    /// `target` is the pane's current render target (`LoopPlaybackState::rendered_for`):
    /// the loop's site plus the *selected* product and elevation, as opposed to
    /// `params.elevation`, which is snapped to a sweep in this frame's own scan, and
    /// `params.lat`/`params.lon`, which are that same site's coordinates. It is stamped
    /// on the response so a result can be rejected if the pane retargets — or the loop
    /// is rebuilt for another site — while the render runs.
    pub(super) fn spawn_loop_frame_render(
        &self,
        pane_idx: usize,
        timestamp: NaiveDateTime,
        scan_data: std::sync::Arc<nexrad_model::data::Scan>,
        params: crate::render_dispatch::RenderParams,
        target: rustdar_egui::pane::RenderTarget,
    ) -> bool {
        // Check concurrent render limit (the counter is shared with static pane renders)
        let current = self.render.renders_in_flight.load(Ordering::Relaxed);
        if current >= MAX_CONCURRENT_RENDERS {
            return false;
        }
        self.render.renders_in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(std::sync::Arc::clone(&self.render.renders_in_flight));

        // Both the render call and the response's `snapped` read `params`, and
        // nothing here re-derives either from `target`. `params.elevation` is the
        // sweep this frame's own scan carries; `target.elevation` is the selection
        // that was asked for. `LoopRenderRequest::render_params` makes that choice
        // once, under test — this only forwards it, so the two cannot disagree
        // about what the image depicts.
        let crate::render_dispatch::RenderParams { product, elevation: snapped, lat, lon } = params;
        let sender = self.channels.loop_render_sender.clone();
        let window = self.window.clone();
        std::thread::Builder::new()
            .name("loop-render".into())
            .spawn(move || {
            let _guard = guard;
            // A failed render still has to be sent, so render_in_flight gets cleared.
            let rendered =
                rustdar_radar::render::render_radar_to_image(&scan_data, snapped, product, lat, lon);
            let (image, max_range_km) = match rendered {
                Some((rgba, range, _values)) => {
                    // Convert here rather than on the main thread, and let `rgba` drop
                    // at the end of this scope so only one of the two buffers is ever
                    // in the channel. `values` is dropped outright: loop frames store
                    // an empty value grid, so shipping 16 MiB of hover data per frame
                    // only to discard it on arrival is pure waste.
                    match loop_frame_image(&rgba) {
                        Some(image) => (Some(image), range),
                        None => {
                            log::error!(
                                "Loop render for pane {pane_idx} produced {} bytes, expected {}",
                                rgba.len(),
                                IMAGE_SIZE * IMAGE_SIZE * 4
                            );
                            (None, 0.0)
                        }
                    }
                }
                None => (None, 0.0),
            };
            // One send site for both outcomes, so `snapped` cannot come to differ
            // between them. It describes the render that was dispatched — the sweep
            // `render_params` resolved — and stays true of a response carrying no
            // image, which is what makes it safe to set outside the match.
            let _ = sender.send(crate::channels::LoopRenderResponse {
                pane_idx,
                timestamp,
                target,
                snapped,
                image,
                max_range_km,
            });
            super::notify_redraw(&window);
        }).expect("failed to spawn loop-render thread");
        true
    }

    /// Append a freshly-polled scan to any active loops, evicting frames past
    /// the lookback window.
    ///
    /// `site` is the site the *scan* came from, not any pane's — it decides both
    /// which cache entry the scan becomes and which loops may take a frame for it.
    pub(super) fn append_scan_to_active_loops(
        &mut self,
        site: &str,
        timestamp: chrono::NaiveDateTime,
        scan: std::sync::Arc<nexrad_model::data::Scan>,
    ) {
        // Store in the shared cache under this scan's own site, for every loop on
        // that site to use.
        self.loop_mgr.cache_scan(site, timestamp, scan);

        append_polled_frame_to_loops(self.gui.panes_mut(), site, timestamp);
    }

    /// Re-initialize radar loops on all panes that have an active loop.
    /// Called after a manual time navigation to rebase loops around the new scan time.
    pub(super) fn reinit_active_loops(&mut self) {
        let mut to_reinit = Vec::new();
        for pane_idx in 0..self.gui.pane_count() {
            if let Some(pane) = self.gui.pane_mut(pane_idx)
                && pane.loop_state.is_active() {
                    to_reinit.push((pane_idx, pane.loop_state.lookback_secs));
                }
        }
        for (pane_idx, lookback_secs) in to_reinit {
            self.handle_enable_loop(pane_idx, lookback_secs);
        }
    }
}

/// Convert a renderer RGBA buffer into egui's pixel layout, or `None` if it is not
/// the `IMAGE_SIZE²` image the renderer is supposed to produce.
///
/// The length check is not defensive padding. `ColorImage::from_rgba_unmultiplied`
/// asserts on a mismatch, and this now runs on the render worker rather than the
/// main thread: a panic there kills only that thread, so no `LoopRenderResponse`
/// would ever arrive, `render_in_flight` would never clear, and the frame would stay
/// blank and be skipped for the life of the loop. Returning `None` routes a
/// malformed buffer down the same path as "no matching sweep", which the dispatcher
/// already knows how to retire.
fn loop_frame_image(rgba: &[u8]) -> Option<egui::ColorImage> {
    if rgba.len() != IMAGE_SIZE * IMAGE_SIZE * 4 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([IMAGE_SIZE, IMAGE_SIZE], rgba))
}

/// The scan listing a freshly-built loop needs, and the site it must be requested
/// for.
///
/// One struct rather than three loose values because they have to describe a single
/// pane's single site: `site` is the code the listing is requested with *and* the
/// code the loop's geometry was captured under, and `start`/`end` are that site's
/// own scan time walked back by the lookback. Any of them coming from elsewhere
/// gives a loop that lists one radar's files and draws them at another's coordinates.
pub(super) struct LoopScanRequest {
    site: String,
    start: NaiveDateTime,
    end: NaiveDateTime,
}

/// Build `pane_idx`'s loop state and return the scan listing it now needs, or
/// `None` if that pane has no scan loaded to anchor a loop on.
///
/// This is everything enabling a loop does apart from the spawn: it indexes the
/// panes itself, so "which pane" is decided and tested here rather than at an
/// untestable call site. The active pane is deliberately never consulted —
/// `reinit_active_loops` runs this for every looping pane in turn, and a loop that
/// took the active pane's site would show that radar under its own pane's label.
///
/// The pane's `scan_info` can lag its `site` field briefly after a site switch,
/// while the new site's scan is still loading. The loop built in that window is
/// stale but not wrong: its code, its coordinates and its listing all come from the
/// one `RadarSite` in that `scan_info`, and the next scan to land re-runs this.
fn begin_loop_for_pane(
    panes: &mut [rustdar_egui::pane::PaneState],
    loop_mgr: &mut crate::loop_downloads::LoopDownloadManager,
    pane_idx: usize,
    lookback_secs: u64,
) -> Option<LoopScanRequest> {
    let scan_info = panes.get(pane_idx)?.scan_info.as_ref()?;
    // The whole site value, so the loop's render-target code and the coordinates
    // it projects with cannot come from different sites.
    let radar_site = scan_info.site.clone();
    // The loop ends at this pane's current scan, not at wall clock, so it covers
    // where the pane is actually looking.
    let end = scan_info.timestamp;

    // Drop the previous listing's undispatched downloads; they were queued for the
    // loop this call is replacing. The scan cache is global and deliberately kept.
    loop_mgr.remove_pending(pane_idx);

    panes[pane_idx].loop_state =
        rustdar_egui::pane::LoopPlaybackState::new_for_loop(lookback_secs, &radar_site);

    Some(LoopScanRequest {
        site: radar_site.name.to_string(),
        start: end - chrono::Duration::seconds(lookback_secs as i64),
        end,
    })
}

/// Append a frame for a scan polled from `site` at `timestamp` to every active
/// loop that is on that site.
///
/// The site test is the point. A polled scan is cached under `(site, timestamp)`
/// and looked up that way at render time, so a loop on another site handed this
/// frame resolves the lookup to *its* site's scan or to nothing at all — and
/// before the cache carried a site, it resolved to this scan and drew it around
/// the other site's coordinates, which is data from one radar under another
/// radar's label. Loops on other sites get their own frames from their own polls.
fn append_polled_frame_to_loops(
    panes: &mut [rustdar_egui::pane::PaneState],
    site: &str,
    timestamp: chrono::NaiveDateTime,
) {
    for (pane_idx, pane) in panes.iter_mut().enumerate() {
        if append_polled_frame(&mut pane.loop_state, site, timestamp) {
            log::info!(
                "Appended {} scan {} to loop on pane {} ({} frames)",
                site,
                timestamp,
                pane_idx,
                pane.loop_state.frames.len()
            );
        }
    }
}

/// Add a frame at `timestamp` to `ls` if the loop is active, is on `site`, and does
/// not already have that frame. Returns whether a frame was added.
///
/// Evicting past the lookback window is part of the same step: the window is
/// measured from the newest frame, so it can only be applied once the new frame is
/// in place.
fn append_polled_frame(
    ls: &mut rustdar_egui::pane::LoopPlaybackState,
    site: &str,
    timestamp: chrono::NaiveDateTime,
) -> bool {
    use rustdar_egui::pane::LoopFrame;

    if !ls.is_active() {
        return false;
    }
    // `LoopPlaybackState::site` is the loop's *geometry* site, captured when the
    // loop was built — not the pane's live `site` field, which is re-synced across
    // panes without rebuilding their loops.
    if ls.site != site {
        return false;
    }
    // Skip if this timestamp already exists
    if ls.frames.iter().any(|f| f.timestamp == timestamp) {
        return false;
    }

    // Insert in sorted order
    let insert_pos = ls.frames.partition_point(|f| f.timestamp < timestamp);
    ls.frames.insert(insert_pos, LoopFrame {
        timestamp,
        texture: None,
        render_in_flight: false,
        render_failed: false,
    });

    // Evict frames outside the lookback window
    let lookback = chrono::Duration::seconds(ls.lookback_secs as i64);
    if let Some(newest) = ls.frames.last().map(|f| f.timestamp) {
        let cutoff = newest - lookback;
        ls.frames.retain(|f| f.timestamp >= cutoff);
        // Adjust current_frame if the playhead fell off the end
        if ls.current_frame >= ls.frames.len() {
            ls.current_frame = ls.frames.len().saturating_sub(1);
        }
    }

    true
}

#[cfg(test)]
mod loop_pane_tests {
    use super::*;
    use crate::loop_downloads::LoopDownloadManager;
    use nexrad_data::aws::archive::Identifier;
    use rustdar_egui::pane::{LoopPlaybackState, PaneState};
    use rustdar_radar::sites::RadarSite;
    use rustdar_radar::types::{RadarProduct, ScanInfo};

    fn ts(minute: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, minute, 0)
            .unwrap()
    }

    fn identifier(name: &str) -> Identifier {
        Identifier::new(name.to_string())
    }

    fn site(name: &'static str, lat: f64, lon: f64) -> RadarSite {
        RadarSite { name, lat, lon, elev: None }
    }

    /// A pane showing `site`'s scan at `timestamp`.
    fn pane_showing(site: RadarSite, timestamp: NaiveDateTime) -> PaneState {
        let mut pane = PaneState::with_site(site.name.to_string());
        pane.scan_info = Some(ScanInfo {
            site,
            timestamp,
            vcp_number: 212,
            available_products: vec![RadarProduct::Reflectivity],
            product_elevations: std::collections::HashMap::new(),
            status: String::new(),
        });
        pane
    }

    /// A pane with an active loop on `site`, holding frames at the given minutes.
    fn pane_looping_on(site: RadarSite, lookback_secs: u64, frames: &[u32]) -> PaneState {
        let mut pane = PaneState::with_site(site.name.to_string());
        pane.loop_state = LoopPlaybackState::new_for_loop(lookback_secs, &site);
        for &minute in frames {
            append_polled_frame(&mut pane.loop_state, site.name, ts(minute));
        }
        pane
    }

    fn frame_times(pane: &PaneState) -> Vec<NaiveDateTime> {
        pane.loop_state.frames.iter().map(|f| f.timestamp).collect()
    }

    /// The defect: `handle_enable_loop` read the *active* pane's scan info and
    /// `reinit_active_loops` then applied it to every looping pane, so a pane on
    /// another site silently showed the active pane's radar under its own label.
    #[test]
    fn a_loop_is_built_from_its_own_panes_scan_not_the_active_panes() {
        // Pane 0 is the active one in every real call path that reaches here.
        let mut panes = [
            pane_showing(site("KTLX", 35.33, -97.27), ts(10)),
            pane_showing(site("KOUN", 35.23, -97.46), ts(25)),
        ];
        let mut mgr = LoopDownloadManager::new();

        let req = begin_loop_for_pane(&mut panes, &mut mgr, 1, 600).expect("pane 1 has a scan");

        assert_eq!(req.site, "KOUN", "the listing must be requested for pane 1's site");
        assert_eq!(
            req.end,
            ts(25),
            "and end at pane 1's own scan time, not the active pane's"
        );
        assert_eq!(req.start, ts(15), "walked back by the lookback");

        // The loop state is built from the same site value the listing names, so the
        // code it is compared on and the coordinates it projects with agree.
        let ls = &panes[1].loop_state;
        assert_eq!(ls.site, "KOUN");
        assert_eq!(ls.site_lat, 35.23);
        assert_eq!(ls.site_lon, -97.46);
        assert!(ls.is_fetching(), "and it is waiting for that listing");

        // The pane that was *not* asked for is untouched, so nothing here is
        // incidentally right because both panes were written.
        assert!(!panes[0].loop_state.is_active());

        // Pane 0 reads as itself when it is the one asked for.
        let req = begin_loop_for_pane(&mut panes, &mut mgr, 0, 600).expect("pane 0 has a scan");
        assert_eq!(req.site, "KTLX");
        assert_eq!(req.end, ts(10));
    }

    /// A pane with nothing loaded yet has no loop parameters, and must not borrow
    /// another pane's — nor leave a loop half-built.
    #[test]
    fn a_pane_with_no_scan_yields_no_loop() {
        let mut panes = [
            pane_showing(site("KTLX", 35.33, -97.27), ts(10)),
            pane_showing(site("KOUN", 35.23, -97.46), ts(25)),
        ];
        panes[1].scan_info = None;
        let mut mgr = LoopDownloadManager::new();

        assert!(begin_loop_for_pane(&mut panes, &mut mgr, 1, 600).is_none());
        assert!(!panes[1].loop_state.is_active(), "no loop was started");
        assert!(
            begin_loop_for_pane(&mut panes, &mut mgr, 7, 600).is_none(),
            "and neither does a pane that does not exist"
        );
    }

    /// Enabling a loop drops the previous listing's undispatched downloads: they
    /// were queued for the loop this call is replacing, and on a site switch they
    /// are another radar's files.
    #[test]
    fn beginning_a_loop_clears_the_panes_pending_downloads() {
        let mut panes = [pane_showing(site("KTLX", 35.33, -97.27), ts(10))];
        let mut mgr = LoopDownloadManager::new();
        mgr.insert_pending(0, crate::loop_downloads::PendingDownloads {
            site: "KOUN".to_string(),
            queue: [(ts(5), identifier("KOUN20240101_000500_V06"))].into_iter().collect(),
        });
        assert!(!mgr.is_pane_done(0), "precondition: pane 0 has work queued");

        begin_loop_for_pane(&mut panes, &mut mgr, 0, 600).expect("pane 0 has a scan");

        assert!(mgr.is_pane_done(0), "the previous loop's downloads are gone");
    }

    /// The defect this half of the site fix exists for. Auto-poll delivers one
    /// site's scan; a loop on a different site used to take a frame for it, then
    /// render that scan around its own coordinates.
    #[test]
    fn a_polled_scan_only_reaches_loops_on_its_own_site() {
        let ktlx = site("KTLX", 35.33, -97.27);
        let koun = site("KOUN", 35.23, -97.46);
        let mut panes = [
            pane_looping_on(ktlx, 3600, &[0, 5]),
            pane_looping_on(koun, 3600, &[0, 5]),
        ];

        append_polled_frame_to_loops(&mut panes, "KTLX", ts(10));

        assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(5), ts(10)]);
        assert_eq!(
            frame_times(&panes[1]),
            vec![ts(0), ts(5)],
            "a KOUN loop must not take a frame for a KTLX scan"
        );
    }

    /// The loop's own site is the geometry site captured when it was built. A pane
    /// whose live `site` field has been re-synced without its loop being rebuilt
    /// must still be judged on the loop's site, or the frame lands in a loop that
    /// projects it somewhere else.
    #[test]
    fn the_loops_site_decides_not_the_panes_live_site() {
        let koun = site("KOUN", 35.23, -97.46);
        let mut panes = [pane_looping_on(koun, 3600, &[0])];
        // `propagate_layer_sync` converges the pane's site without rebuilding loops.
        panes[0].site = "KTLX".to_string();

        append_polled_frame_to_loops(&mut panes, "KTLX", ts(10));
        assert_eq!(frame_times(&panes[0]), vec![ts(0)], "the loop is still a KOUN loop");

        append_polled_frame_to_loops(&mut panes, "KOUN", ts(10));
        assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(10)]);
    }

    /// Single-frame mode keeps a `LoopPlaybackState` around whose `site` is an
    /// empty placeholder. A poll must not turn that into a frame list.
    #[test]
    fn an_inactive_loop_takes_no_frames() {
        let mut panes = [PaneState::with_site("KTLX".to_string())];
        assert_eq!(panes[0].loop_state.site, "", "precondition: placeholder site");

        append_polled_frame_to_loops(&mut panes, "KTLX", ts(10));
        append_polled_frame_to_loops(&mut panes, "", ts(11));

        assert!(panes[0].loop_state.frames.is_empty());
    }

    #[test]
    fn a_polled_frame_is_inserted_in_time_order_and_never_twice() {
        let ktlx = site("KTLX", 35.33, -97.27);
        let mut panes = [pane_looping_on(ktlx, 3600, &[0, 10])];

        // Out-of-order arrival still lands between its neighbours.
        append_polled_frame_to_loops(&mut panes, "KTLX", ts(5));
        assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(5), ts(10)]);

        append_polled_frame_to_loops(&mut panes, "KTLX", ts(5));
        assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(5), ts(10)], "no duplicate frame");
    }

    /// Frames older than the lookback window are dropped as new ones arrive.
    #[test]
    fn appending_evicts_past_the_lookback_window() {
        let ktlx = site("KTLX", 35.33, -97.27);
        // 10 minutes of lookback, frames every 5 minutes.
        let mut panes = [pane_looping_on(ktlx, 600, &[0, 5, 10])];

        append_polled_frame_to_loops(&mut panes, "KTLX", ts(15));

        assert_eq!(
            frame_times(&panes[0]),
            vec![ts(5), ts(10), ts(15)],
            "the frame older than the window is evicted"
        );
    }

    /// The playhead has to come back inside the list when eviction shortens it.
    ///
    /// A poll gap wider than the lookback — the site was down, the app was asleep,
    /// the machine was suspended — evicts the whole window at once. Left alone,
    /// `current_frame` points past the end, `PaneState::displayed_frame` resolves it
    /// with `.get()` and finds nothing, and the pane renders blank. A paused loop
    /// never advances, so it stays blank.
    #[test]
    fn eviction_pulls_the_playhead_back_inside_the_list() {
        let ktlx = site("KTLX", 35.33, -97.27);
        let mut panes = [pane_looping_on(ktlx, 600, &[0, 5, 10])];
        panes[0].loop_state.current_frame = 2;

        // 15 minutes on from the newest frame, with a 10 minute window: everything
        // that was there is now older than the cutoff.
        append_polled_frame_to_loops(&mut panes, "KTLX", ts(25));

        assert_eq!(frame_times(&panes[0]), vec![ts(25)], "precondition: only the new frame survives");
        assert_eq!(
            panes[0].loop_state.current_frame, 0,
            "the playhead must land on a frame that exists"
        );
        assert!(
            panes[0].loop_state.frames.get(panes[0].loop_state.current_frame).is_some(),
            "and resolve to one, which is what the pane renders through"
        );
    }
}

#[cfg(test)]
mod loop_frame_image_tests {
    use super::*;

    /// A well-formed buffer converts, and keeps the dimensions the rest of the loop
    /// machinery assumes.
    #[test]
    fn a_full_size_buffer_converts() {
        let rgba = vec![0u8; IMAGE_SIZE * IMAGE_SIZE * 4];
        let image = loop_frame_image(&rgba).expect("a correctly sized buffer must convert");
        assert_eq!(image.size, [IMAGE_SIZE, IMAGE_SIZE]);
        assert_eq!(image.pixels.len(), IMAGE_SIZE * IMAGE_SIZE);
    }

    /// The reason the guard exists: on the worker thread the assert inside
    /// `from_rgba_unmultiplied` would kill the thread silently, no response would be
    /// sent, and the frame would sit `render_in_flight` forever.
    #[test]
    fn a_malformed_buffer_is_rejected_rather_than_panicking() {
        let short = IMAGE_SIZE * IMAGE_SIZE * 4 - 4;
        let long = IMAGE_SIZE * IMAGE_SIZE * 4 + 4;
        assert!(loop_frame_image(&vec![0u8; short]).is_none(), "short buffer");
        assert!(loop_frame_image(&vec![0u8; long]).is_none(), "long buffer");
        assert!(loop_frame_image(&[]).is_none(), "empty buffer");
    }

    /// Pixel values survive the conversion — a frame that converted to transparent
    /// black would render as nothing and look exactly like a frame that never rendered.
    #[test]
    fn pixel_values_survive_the_conversion() {
        let mut rgba = vec![0u8; IMAGE_SIZE * IMAGE_SIZE * 4];
        rgba[0..4].copy_from_slice(&[10, 20, 30, 255]);
        let image = loop_frame_image(&rgba).unwrap();
        assert_eq!(image.pixels[0], egui::Color32::from_rgba_unmultiplied(10, 20, 30, 255));
        assert_ne!(image.pixels[0], egui::Color32::TRANSPARENT);
    }
}
