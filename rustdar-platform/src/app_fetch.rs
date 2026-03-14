use chrono::NaiveDateTime;
use chrono::TimeZone;
use winit::event_loop::ActiveEventLoop;
use rustdar_egui::actions::{GuiAction, OverlayRenderKind};
use crate::channels::{ScanResponse, ScanData, Level3Response, OutlookResponse, OverlayType, OverlayRenderResponse};
use rustdar_radar::types::RadarProduct;
use rustdar_radar::scan;

impl super::App {

    /// Spawn an async radar data fetch on the background runtime.
    /// Handles generation tracking, result sending, and redraw requests.
    pub fn spawn_fetch(&mut self, site: String, timestamp: NaiveDateTime) {
        let generation = self.render.next_fetch_generation();
        let window = self.window.clone();
        let sender = self.channels.scan_sender.clone();
        self.tokio_runtime.spawn(async move {
            log::info!("Fetching {} @ {} UTC", site, timestamp);
            let msg = match scan::get_scan(&site, timestamp).await {
                Ok(data) => {
                    log::info!("Fetched scan: {} @ {}", site, timestamp);
                    Ok(ScanData { scan: data, site, timestamp })
                }
                Err(e) => {
                    let err = format!("Failed to fetch radar scan: {:?}", e);
                    log::error!("{}", err);
                    Err(err)
                }
            };
            let _ = sender.send(ScanResponse { generation, result: msg });
            super::notify_redraw(&window);
        });
    }

    /// Spawn Level III product fetches for all supported Level III products.
    /// Called after a Level II scan loads so the products are available
    /// alongside the base moments.
    pub(super) fn spawn_level3_fetches(&self, site: &str) {
        let generation = self.render.fetch_generation;
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
                    let _ = sender.send(Level3Response { generation, product, tilt_code: dir_str, result });
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
            | GuiAction::SwitchRadarSite(_) => self.handle_radar_action(action),
            GuiAction::Exit => {
                self.request_exit(event_loop);
            }
            GuiAction::FetchSpcOutlook { .. }
            | GuiAction::RefreshSpcOutlooks
            | GuiAction::FetchNwsAlerts
            | GuiAction::RefreshNwsAlerts
            | GuiAction::FetchSpcDiscussions
            | GuiAction::RefreshSpcDiscussions => self.handle_overlay_action(action),
            GuiAction::RenderOverlay { pane_idx, overlay_kind, geo_bounds, width, height, data_generation, zoom } => {
                self.spawn_overlay_render(pane_idx, overlay_kind, geo_bounds, width, height, data_generation, zoom);
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

                let generation = self.render.next_fetch_generation();
                let site = radar_config.site.clone();
                let window = self.window.clone();
                let sender = self.channels.scan_sender.clone();

                self.tokio_runtime.spawn(async move {
                    match scan::check_and_fetch_latest(&site, &utc_timestamp.date(), current_scan_timestamp).await {
                        Ok(Some((data, timestamp))) => {
                            let _ = sender.send(crate::channels::ScanResponse {
                                generation,
                                result: Ok(crate::channels::ScanData { scan: data, site, timestamp }),
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
            GuiAction::SwitchRadarSite(site) => {
                log::info!("Switch radar site requested: {}", site);
                
                let mut new_config = self.gui.get_radar_config().clone();
                new_config.site = site.clone();
                self.gui.set_radar_config(new_config.clone());
                self.gui.set_loading_site(Some(site.clone()));
                
                let utc_timestamp = Self::local_to_utc(new_config.timestamp);
                self.spawn_fetch(site, utc_timestamp);
            }
            _ => unreachable!(),
        }
    }

    /// Spawn an async overlay fetch task. Sends the future's result through
    /// the given channel and requests a redraw when complete.
    fn spawn_overlay_fetch<T: Send + 'static>(
        &self,
        sender: std::sync::mpsc::Sender<T>,
        future: impl std::future::Future<Output = T> + Send + 'static,
    ) {
        let window = self.window.clone();
        self.tokio_runtime.spawn(async move {
            let result = future.await;
            let _ = sender.send(result);
            super::notify_redraw(&window);
        });
    }

    /// Handle overlay fetch/refresh actions (SPC outlooks, NWS alerts, SPC discussions).
    fn handle_overlay_action(&mut self, action: GuiAction) {
        match action {
            GuiAction::FetchSpcOutlook { day, products } => {
                log::info!("Fetching SPC outlooks for {:?}: {:?}", day, products);
                self.gui.overlays.set_spc_fetching(true);
                let client = self.http_client.clone();
                let sender = self.channels.outlook_sender.clone();
                for product in products {
                    let client = client.clone();
                    self.spawn_overlay_fetch(sender.clone(), async move {
                        let result =
                            rustdar_overlays::spc::fetch::fetch_outlook(&client, day, product)
                                .await
                                .map_err(|e| format!("{e}"));
                        OutlookResponse { day, product, result }
                    });
                }
            }
            GuiAction::RefreshSpcOutlooks => {
                let day = self.gui.active_pane().layers.spc_day;
                let products = self.gui.active_pane().layers.enabled_spc_products();
                if !products.is_empty() {
                    self.handle_overlay_action(
                        GuiAction::FetchSpcOutlook { day, products },
                    );
                }
            }
            GuiAction::FetchNwsAlerts | GuiAction::RefreshNwsAlerts => {
                log::info!("Fetching NWS active alerts");
                self.gui.overlays.set_nws_fetching(true);
                let client = self.http_client.clone();
                let zone_cache = self.platform.zone_cache_dir().map(|p| p.to_path_buf());
                self.spawn_overlay_fetch(self.channels.alert_sender.clone(), async move {
                    rustdar_overlays::nws::fetch::fetch_active_alerts(
                        &client,
                        zone_cache.as_deref(),
                    )
                        .await
                        .map_err(|e| format!("{e}"))
                });
            }
            GuiAction::FetchSpcDiscussions | GuiAction::RefreshSpcDiscussions => {
                log::info!("Fetching SPC Mesoscale Discussions");
                self.gui.overlays.set_spc_md_fetching(true);
                let client = self.http_client.clone();
                self.spawn_overlay_fetch(self.channels.discussion_sender.clone(), async move {
                    rustdar_overlays::spc::fetch::fetch_active_discussions(&client)
                        .await
                        .map_err(|e| format!("{e}"))
                });
            }
            _ => unreachable!(),
        }
    }

    /// Spawn a background thread to rasterize overlay polygons via tiny-skia.
    fn spawn_overlay_render(
        &mut self,
        pane_idx: usize,
        kind: OverlayRenderKind,
        geo_bounds: rustdar_overlays::types::GeoBounds,
        width: u32,
        height: u32,
        data_generation: u64,
        zoom: i32,
    ) {
        use rustdar_overlays::render::rasterize;
        use rustdar_egui::overlay_cache::OVERDRAW_FRACTION;

        if width == 0 || height == 0 {
            return;
        }

        // Mark in-flight on the appropriate texture cache
        if let Some(cache) = self.pane_overlay_cache_mut(pane_idx, &kind) {
            cache.render_in_flight = true;
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

        let overlay_type = match kind {
            OverlayRenderKind::SpcOutlook => OverlayType::SpcOutlook(
                self.gui.active_pane().layers.spc_day,
                // The texture combines all enabled products — just use Categorical as tag.
                rustdar_overlays::spc::outlook::OutlookProduct::Categorical,
            ),
            OverlayRenderKind::SpcDiscussions => OverlayType::SpcDiscussions,
            OverlayRenderKind::NwsAlerts => OverlayType::NwsAlerts,
        };

        let sender = self.channels.overlay_render_sender.clone();
        let window = self.window.clone();

        // Clone the data needed for the render closure
        match kind {
            OverlayRenderKind::SpcOutlook => {
                let layers = &self.gui.active_pane().layers;
                let day = layers.spc_day;
                let mut features = Vec::new();
                for lk in layers.spc_layers_for_day() {
                    if !layers.is_enabled(lk) {
                        continue;
                    }
                    let Some(product) = lk.to_outlook_product() else { continue };
                    if let Some(outlook) = self.gui.overlays.spc_outlooks.data.get(&(day, product)) {
                        features.extend(outlook.features.iter().cloned());
                    }
                }
                let is_dark = self.cached_dark_theme.unwrap_or(false);
                std::thread::spawn(move || {
                    let hatch_color = if is_dark {
                        [200, 200, 200, 180]
                    } else {
                        [60, 60, 60, 180]
                    };
                    let image_data = rasterize::rasterize_spc_outlooks(
                        &features,
                        &render_bounds,
                        width,
                        height,
                        hatch_color,
                    );
                    let _ = sender.send(OverlayRenderResponse {
                        image_data,
                        width,
                        height,
                        geo_bounds: render_bounds,
                        overlay_type,
                        generation: data_generation,
                        pane_idx,
                        zoom,
                    });
                    super::notify_redraw(&window);
                });
            }
            OverlayRenderKind::SpcDiscussions => {
                let discussions: Vec<_> = self.gui.overlays.spc_discussions.data.clone();
                std::thread::spawn(move || {
                    let image_data = rasterize::rasterize_spc_discussions(
                        &discussions,
                        &render_bounds,
                        width,
                        height,
                    );
                    let _ = sender.send(OverlayRenderResponse {
                        image_data,
                        width,
                        height,
                        geo_bounds: render_bounds,
                        overlay_type,
                        generation: data_generation,
                        pane_idx,
                        zoom,
                    });
                    super::notify_redraw(&window);
                });
            }
            OverlayRenderKind::NwsAlerts => {
                let alerts: Vec<_> = self.gui.overlays.nws_alerts.data.clone();
                let enabled_categories = self.gui.active_pane().layers.enabled_nws_categories();
                let hidden_alerts = self.gui.overlays.hidden_alerts.clone();
                std::thread::spawn(move || {
                    let image_data = rasterize::rasterize_nws_alerts(
                        &alerts,
                        &enabled_categories,
                        &hidden_alerts,
                        &render_bounds,
                        width,
                        height,
                    );
                    let _ = sender.send(OverlayRenderResponse {
                        image_data,
                        width,
                        height,
                        geo_bounds: render_bounds,
                        overlay_type,
                        generation: data_generation,
                        pane_idx,
                        zoom,
                    });
                    super::notify_redraw(&window);
                });
            }
        }
    }

    /// Get a mutable reference to the appropriate overlay texture cache for a pane.
    fn pane_overlay_cache_mut(
        &mut self,
        pane_idx: usize,
        kind: &OverlayRenderKind,
    ) -> Option<&mut rustdar_egui::overlay_cache::OverlayTextureCache> {
        let pane = self.gui.pane_mut(pane_idx)?;
        Some(match kind {
            OverlayRenderKind::SpcOutlook => &mut pane.spc_overlay_texture,
            OverlayRenderKind::SpcDiscussions => &mut pane.spc_md_texture,
            OverlayRenderKind::NwsAlerts => &mut pane.nws_alert_texture,
        })
    }
}
