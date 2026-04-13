use chrono::NaiveDateTime;
use chrono::TimeZone;
use std::sync::atomic::Ordering;
use winit::event_loop::ActiveEventLoop;
use rustdar_egui::actions::GuiAction;
use crate::channels::{ScanResponse, ScanData, Level3Response, OverlayRenderResponse};
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayKind};
use crate::constants::MAX_CONCURRENT_RENDERS;
use crate::render_dispatch::RenderGuard;
use rustdar_radar::types::RadarProduct;

/// Parameters for a background overlay rasterization request.
pub(super) struct OverlayRenderRequest {
    pub geo_bounds: rustdar_overlays::types::GeoBounds,
    pub width: u32,
    pub height: u32,
    pub data_generation: u64,
    pub zoom: i32,
}
use rustdar_radar::scan;

impl super::App {

    /// Spawn an async radar data fetch on the background runtime.
    /// Handles generation tracking, result sending, and redraw requests.
    pub fn spawn_fetch(&mut self, site: String, timestamp: NaiveDateTime) {
        let generation = self.render.next_fetch_generation(&site);
        let window = self.window.clone();
        let sender = self.channels.scan_sender.clone();
        self.tokio_runtime.spawn(async move {
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
            let _ = sender.send(ScanResponse { generation, site, result: msg, is_auto_poll: false });
            super::notify_redraw(&window);
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
                let sender = self.channels.level3_sender.clone();
                let window = self.window.clone();
                self.tokio_runtime.spawn(async move {
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
                    let _ = sender.send(Level3Response { generation, product, tilt_code: dir_str, site, result });
                    super::notify_redraw(&window);
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
            GuiAction::FetchOverlay(_)
            | GuiAction::RefreshOverlay(_) => self.handle_overlay_action(action),
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
                    if ls.playing {
                        // Always allow pause
                        ls.playing = false;
                    } else if ls.render_ready {
                        ls.playing = true;
                        ls.last_advance = Some(std::time::Instant::now());
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
                self.loop_pending_downloads.clear();
                self.loop_scan_cache.clear();
                
                let utc_timestamp = Self::local_to_utc(new_config.timestamp);
                self.spawn_fetch(site, utc_timestamp);
            }
            _ => unreachable!(),
        }
    }

    /// Handle overlay fetch/refresh actions for all overlay kinds.
    fn handle_overlay_action(&mut self, action: GuiAction) {
        match action {
            GuiAction::FetchOverlay(kind) | GuiAction::RefreshOverlay(kind) => {
                self.fetch_overlay(kind);
            }
            _ => unreachable!(),
        }
    }

    /// Fetch overlay data for the given kind, resolving parameters from current state.
    fn fetch_overlay(&mut self, kind: OverlayKind) {
        use rustdar_overlays::render::overlay_state::FetchConfig;

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
            let sender = self.channels.overlay_fetch_sender.clone();
            let window = self.window.clone();
            let task_kind = task.kind;
            self.tokio_runtime.spawn(async move {
                let data = task.future.await;
                let _ = sender.send(OverlayFetchResult { kind: task_kind, data });
                super::notify_redraw(&window);
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
        use rustdar_egui::overlay_cache::OVERDRAW_FRACTION;

        let OverlayRenderRequest { geo_bounds, width, height, data_generation, zoom } = req;

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

        // Expand geo_bounds by overdraw fraction, clamped to valid Mercator range
        let lat_range = geo_bounds.max_lat - geo_bounds.min_lat;
        let lon_range = geo_bounds.max_lon - geo_bounds.min_lon;
        let overdraw = OVERDRAW_FRACTION as f64;
        let render_bounds = rustdar_overlays::types::GeoBounds {
            min_lat: (geo_bounds.min_lat - lat_range * overdraw).max(-85.05),
            max_lat: (geo_bounds.max_lat + lat_range * overdraw).min(85.05),
            min_lon: geo_bounds.min_lon - lon_range * overdraw,
            max_lon: geo_bounds.max_lon + lon_range * overdraw,
        };

        // Use the first target pane for data extraction (all synced panes share config)
        let first_pane_idx = pane_indices[0];
        let Some(target_pane) = self.gui.pane(first_pane_idx) else { return };

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
                    zoom: zoom as f64 / 32.0,
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
                std::thread::spawn(move || {
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
                });
            }
            OverlayKind::RadarSites => {
                let target_site = target_pane.site.clone();
                let target_loading = target_pane.loading_site.clone();
                let is_dark = self.cached_dark_theme.unwrap_or(false);
                let actual_zoom = zoom as f64 / 32.0;
                let sites: Vec<rasterize::RadarSiteInfo> = rustdar_radar::sites::RADARS.iter().map(|s| {
                    rasterize::RadarSiteInfo {
                        name: s.name.to_string(),
                        lat: s.lat,
                        lon: s.lon,
                        is_current: s.name == target_site,
                        is_loading: target_loading.as_deref() == Some(s.name),
                    }
                }).collect();
                std::thread::spawn(move || {
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
                });
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
    fn handle_enable_loop(&mut self, pane_idx: usize, lookback_secs: u64) {
        let Some(scan_info) = self.gui.get_scan_info() else { return };
        let site = scan_info.site.name.to_string();
        let site_lat = scan_info.site.lat;
        let site_lon = scan_info.site.lon;
        let scan_timestamp = scan_info.timestamp;

        // Clear pending downloads for this pane (global cache is kept for sharing)
        self.loop_pending_downloads.remove(&pane_idx);

        // Initialize loop state on the pane
        if let Some(pane) = self.gui.pane_mut(pane_idx) {
            pane.loop_state = rustdar_egui::pane::LoopPlaybackState {
                multi_frame: true,
                playing: false,
                current_frame: 0,
                frames: Vec::new(),
                lookback_secs,
                fetching: true,
                render_ready: false,
                playback_started: false,
                last_advance: None,
                site_lat,
                site_lon,
            };
        }

        // Use the current scan's timestamp as the loop end time (not wall clock)
        let end = scan_timestamp;
        let start = end - chrono::Duration::seconds(lookback_secs as i64);

        let sender = self.channels.loop_scan_list_sender.clone();
        let window = self.window.clone();
        self.tokio_runtime.spawn(async move {
            match scan::list_scans_for_range(&site, start, end).await {
                Ok(scans) => {
                    log::info!("Loop: found {} scans in range for pane {}", scans.len(), pane_idx);
                    let _ = sender.send(crate::channels::LoopScanListResponse {
                        pane_idx,
                        scans,
                    });
                }
                Err(e) => {
                    log::error!("Loop scan listing failed: {:?}", e);
                    // Send empty list so UI can show error state
                    let _ = sender.send(crate::channels::LoopScanListResponse {
                        pane_idx,
                        scans: Vec::new(),
                    });
                }
            }
            super::notify_redraw(&window);
        });
    }

    /// Disable radar loop for a pane: resets to single-frame mode.
    fn handle_disable_loop(&mut self, pane_idx: usize) {
        if let Some(pane) = self.gui.pane_mut(pane_idx) {
            pane.loop_state = rustdar_egui::pane::LoopPlaybackState::new();
        }
        self.loop_pending_downloads.remove(&pane_idx);
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
        let sender = self.channels.scan_sender.clone();
        let window = self.window.clone();

        self.tokio_runtime.spawn(async move {
            match scan::get_adjacent_scan(&site, current_utc, forward).await {
                Ok((data, timestamp)) => {
                    // If navigating forward and the result is the same as current, treat as live
                    let _ = sender.send(crate::channels::ScanResponse {
                        generation,
                        site: site.clone(),
                        result: Ok(crate::channels::ScanData { scan: data, site, timestamp }),
                        is_auto_poll: false,
                    });
                }
                Err(e) => {
                    let err = format!("Failed to find adjacent scan: {:?}", e);
                    log::error!("{}", err);
                    let _ = sender.send(crate::channels::ScanResponse {
                        generation,
                        site,
                        result: Err(err),
                        is_auto_poll: false,
                    });
                }
            }
            super::notify_redraw(&window);
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
    pub(super) fn spawn_loop_scan_download(
        &self,
        pane_idx: usize,
        timestamp: NaiveDateTime,
        identifier: nexrad_data::aws::archive::Identifier,
    ) {
        let sender = self.channels.loop_scan_download_sender.clone();
        let window = self.window.clone();
        self.tokio_runtime.spawn(async move {
            let scan = match scan::download_scan(identifier).await {
                Ok(scan_data) => Some(std::sync::Arc::new(scan_data)),
                Err(e) => {
                    log::error!("Loop scan download failed for pane {} @ {}: {:?}", pane_idx, timestamp, e);
                    None
                }
            };
            let _ = sender.send(crate::channels::LoopScanDownloadResponse {
                pane_idx,
                timestamp,
                scan,
            });
            super::notify_redraw(&window);
        });
    }

    /// Spawn a background render thread for a single loop frame.
    pub(super) fn spawn_loop_frame_render(
        &self,
        pane_idx: usize,
        timestamp: NaiveDateTime,
        scan_data: std::sync::Arc<nexrad_model::data::Scan>,
        params: &crate::render_dispatch::RenderParams,
    ) {
        // Check concurrent render limit (shared with static pane renders)
        let current = self.renders_in_flight.load(Ordering::Relaxed);
        if current >= MAX_CONCURRENT_RENDERS {
            return;
        }
        self.renders_in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(std::sync::Arc::clone(&self.renders_in_flight));

        let product = params.product;
        let elevation = params.elevation;
        let lat = params.lat;
        let lon = params.lon;
        let sender = self.channels.loop_render_sender.clone();
        let window = self.window.clone();
        std::thread::spawn(move || {
            let _guard = guard;
            match rustdar_radar::render::render_radar_to_image(&scan_data, elevation, product, lat, lon)
            {
                Some((image, range, values)) => {
                    let _ = sender.send(crate::channels::LoopRenderResponse {
                        pane_idx,
                        timestamp,
                        image_data: image,
                        max_range_km: range,
                        value_data: values,
                    });
                }
                None => {
                    // Send an empty response so render_in_flight gets cleared
                    let _ = sender.send(crate::channels::LoopRenderResponse {
                        pane_idx,
                        timestamp,
                        image_data: Vec::new(),
                        max_range_km: 0.0,
                        value_data: Vec::new(),
                    });
                }
            }
            super::notify_redraw(&window);
        });
    }

    /// Append a freshly-polled scan to any active loops, evicting frames past
    /// the lookback window.
    pub(super) fn append_scan_to_active_loops(
        &mut self,
        timestamp: chrono::NaiveDateTime,
        scan: std::sync::Arc<nexrad_model::data::Scan>,
    ) {
        use rustdar_egui::pane::LoopFrame;

        // Store in global cache for all panes to use
        self.loop_scan_cache.insert(timestamp, std::sync::Arc::clone(&scan));

        for pane_idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            let ls = &mut pane.loop_state;
            if !ls.multi_frame {
                continue;
            }

            // Skip if this timestamp already exists
            if ls.frames.iter().any(|f| f.timestamp == timestamp) {
                continue;
            }

            // Insert in sorted order
            let insert_pos = ls.frames.partition_point(|f| f.timestamp < timestamp);
            ls.frames.insert(insert_pos, LoopFrame {
                timestamp,
                texture: None,
                render_in_flight: false,
            });

            // Evict frames outside the lookback window
            let lookback = chrono::Duration::seconds(ls.lookback_secs as i64);
            if let Some(newest) = ls.frames.last().map(|f| f.timestamp) {
                let cutoff = newest - lookback;
                let old_len = ls.frames.len();
                ls.frames.retain(|f| f.timestamp >= cutoff);
                let removed = old_len - ls.frames.len();
                if removed > 0 {
                    // Adjust current_frame if needed
                    if ls.current_frame >= ls.frames.len() {
                        ls.current_frame = ls.frames.len().saturating_sub(1);
                    }
                }
            }

            log::info!(
                "Appended scan {} to loop on pane {} ({} frames)",
                timestamp,
                pane_idx,
                ls.frames.len()
            );
        }
    }

    /// Re-initialize radar loops on all panes that have an active loop.
    /// Called after a manual time navigation to rebase loops around the new scan time.
    pub(super) fn reinit_active_loops(&mut self) {
        let mut to_reinit = Vec::new();
        for pane_idx in 0..self.gui.pane_count() {
            if let Some(pane) = self.gui.pane_mut(pane_idx)
                && pane.loop_state.multi_frame {
                    to_reinit.push((pane_idx, pane.loop_state.lookback_secs));
                }
        }
        for (pane_idx, lookback_secs) in to_reinit {
            self.handle_enable_loop(pane_idx, lookback_secs);
        }
    }
}
