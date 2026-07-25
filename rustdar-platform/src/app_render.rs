use egui_wgpu::{ScreenDescriptor, wgpu};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use rustdar_egui::actions::GuiAction;
use rustdar_egui::pane::{ELEVATION_TOLERANCE, RenderTarget};
use rustdar_radar::types::IMAGE_SIZE;
use crate::constants::{MAX_CONCURRENT_RENDERS, MAX_LOOP_RENDER_BUDGET, MAX_CONCURRENT_LOOP_DOWNLOADS, MAX_LOOP_FRAMES};
use crate::render_dispatch::CachedPaneRender;

impl super::App {
    /// Create screen descriptor and setup egui frame.
    /// Returns the screen descriptor and any GUI actions triggered.
    ///
    /// This calculates the proper scaling factors accounting for:
    /// - OS display scaling (window.scale_factor())
    /// - Application scale factor (state.scale_factor)
    pub(super) fn setup_egui_frame(&mut self) -> (ScreenDescriptor, Vec<GuiAction>) {
        // Build screen descriptor, apply theme, and run the egui UI pass.
        // Scoped so `state` is dropped before we call &mut self methods below.
        let (screen_descriptor, gui_action) = {
            let state = self.state.as_mut().unwrap();
            let window = self.window.as_ref().unwrap();

            // Calculate screen descriptor
            let window_size = window.inner_size();
            let css_to_canvas_scale_x =
                state.surface_config.width as f32 / window_size.width.max(1) as f32;
            let pixels_per_point =
                window.scale_factor() as f32 * state.scale_factor * css_to_canvas_scale_x;

            let screen_descriptor = ScreenDescriptor {
                size_in_pixels: [state.surface_config.width, state.surface_config.height],
                pixels_per_point,
            };

            // Start egui frame
            state.egui_renderer.begin_frame(window);

            // Set theme based on OS preference
            let use_dark_theme = match window.theme() {
                Some(theme) => matches!(theme, winit::window::Theme::Dark),
                None => match self.cached_dark_theme {
                    Some(cached) => cached,
                    None => {
                        let detected = self.platform.detect_dark_theme();
                        self.cached_dark_theme = Some(detected);
                        detected
                    }
                },
            };
            state.egui_renderer.apply_theme(use_dark_theme);

            let gui_action = self.gui.ui(state.egui_renderer.context());

            (screen_descriptor, gui_action)
        };

        // Clean up old textures from previous frame
        // This allows the GPU to finish using them before we drop them
        self.old_textures.clear();

        // Ensure pane_render vec matches gui pane count
        self.render.ensure_pane_count(self.gui.pane_count());

        self.poll_render_results();
        self.poll_level3_results();
        self.poll_overlay_render_results();
        self.poll_loop_scan_list_results();
        self.poll_loop_scan_download_results();
        self.poll_loop_render_results();
        self.advance_loop_playback();
        self.dispatch_pane_renders();
        self.dispatch_loop_renders();
        self.update_loop_readiness();

        (screen_descriptor, gui_action)
    }

    /// Poll for completed background render results and upload textures.
    fn poll_render_results(&mut self) {
        let ctx = self.state.as_ref().unwrap().egui_renderer.context().clone();
        while let Ok(rr) = self.channels.render_receiver.try_recv() {
            if rr.pane_idx < self.render.pane_render.len() {
                self.render.pane_render[rr.pane_idx].render_in_flight = false;
            }

            if self.render.is_render_stale(rr.generation) {
                log::debug!("Discarding stale render result (gen {} < current {})", rr.generation, self.render.render_generation);
                continue;
            }

            if rr.pane_idx >= self.gui.pane_count()
                || self.gui.get_rendering_params_for_pane(rr.pane_idx).is_none()
            {
                continue;
            }

            // Extract fields to avoid borrow issues
            let origin_pane = rr.pane_idx;
            let render_result = crate::render_dispatch::CachedPaneRender {
                image_data: rr.image_data,
                max_range_km: rr.max_range_km,
                value_data: rr.value_data,
                product: rr.product,
                elevation: rr.elevation,
            };

            // Cache the render output for sharing with other panes on the same site
            let origin_site = self.gui.pane(origin_pane).map(|p| p.site.clone()).unwrap_or_default();
            self.render.cache_render(&origin_site, render_result.product, render_result.elevation, crate::render_dispatch::CachedRenderOutput {
                image_data: Arc::clone(&render_result.image_data),
                max_range_km: render_result.max_range_km,
                value_data: Arc::clone(&render_result.value_data),
            });

            // Apply to the originating pane
            self.apply_render_to_pane(&ctx, origin_pane, &render_result);

            // Broadcast to sibling panes that need the same site+product+elevation
            let pane_count = self.gui.pane_count();
            for other_idx in 0..pane_count {
                if other_idx == origin_pane {
                    continue;
                }
                let matches_site = self.gui.pane(other_idx).is_some_and(|p| p.site == origin_site);
                if !matches_site {
                    continue;
                }
                let Some((other_product, other_elevation)) = self.gui.get_rendering_params_for_pane(other_idx) else {
                    continue;
                };
                if other_product == render_result.product && (other_elevation - render_result.elevation).abs() <= ELEVATION_TOLERANCE {
                    let needs = other_idx < self.render.pane_render.len()
                        && self.render.pane_render[other_idx]
                            .last_rendered
                            .map(|(lp, le)| lp != other_product || (le - other_elevation).abs() > ELEVATION_TOLERANCE)
                            .unwrap_or(true);
                    if needs {
                        self.apply_render_to_pane(&ctx, other_idx, &render_result);
                    }
                }
            }
        }
    }

    /// Apply a rendered radar image to a specific pane (upload texture to overlay cache).
    fn apply_render_to_pane(
        &mut self,
        ctx: &egui::Context,
        pane_idx: usize,
        render: &crate::render_dispatch::CachedPaneRender,
    ) {
        use rustdar_egui::overlay_cache::{OverlayTextureData, RadarTextureMeta};
        use rustdar_overlays::render::overlay_state::OverlayKind;
        use rustdar_overlays::types::GeoBounds;
        use rustdar_radar::types::ImageBounds;

        // Extract site coordinates before mutable borrow
        let (lat, lon) = {
            let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                return;
            };
            (scan_info.site.lat, scan_info.site.lon)
        };

        // Clean up old radar overlay texture
        let Some(pane) = self.gui.pane_mut(pane_idx) else {
            return;
        };
        let cache = pane.overlay_cache_mut(OverlayKind::Radar);
        if let Some(old) = cache.current.take() {
            self.old_textures.push(old.texture);
        }

        self.texture_counter += 1;
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([IMAGE_SIZE, IMAGE_SIZE], &render.image_data);
        let texture_name = format!("radar_image_{}", self.texture_counter);
        let texture = ctx.load_texture(
            texture_name,
            color_image,
            egui::TextureOptions::NEAREST,
        );

        // Cache the raw image data for fast restore after suspend/resume
        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].cached_render = Some(CachedPaneRender {
                image_data: Arc::clone(&render.image_data),
                max_range_km: render.max_range_km,
                value_data: Arc::clone(&render.value_data),
                product: render.product,
                elevation: render.elevation,
            });
        }

        // Store in overlay cache with radar metadata
        let bounds = ImageBounds::from_radar_site(lat, lon);
        let geo_bounds = GeoBounds {
            min_lat: bounds.min_lat,
            max_lat: bounds.max_lat,
            min_lon: bounds.min_lon,
            max_lon: bounds.max_lon,
        };
        let pane = self.gui.pane_mut(pane_idx).unwrap();
        let cache = pane.overlay_cache_mut(OverlayKind::Radar);
        cache.current = Some(OverlayTextureData {
            texture,
            geo_bounds,
            data_generation: 0,
            render_zoom: 0,
            width: IMAGE_SIZE as u32,
            height: IMAGE_SIZE as u32,
            radar_meta: Some(RadarTextureMeta {
                value_data: Arc::clone(&render.value_data),
                lat,
                lon,
                max_range_km: render.max_range_km,
            }),
            hit_map: None,
        });

        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].last_rendered = Some((render.product, render.elevation));
        }
    }

    /// Poll for completed Level III fetch results and update scan info.
    fn poll_level3_results(&mut self) {
        let Ok(l3_resp) = self.channels.level3_receiver.try_recv() else {
            return;
        };

        if self.render.is_fetch_stale(&l3_resp.site, l3_resp.generation) {
            log::debug!("Discarding stale Level III result for {} (gen {})", l3_resp.site, l3_resp.generation);
            return;
        }

        let message = match l3_resp.result {
            Ok(msg) => msg,
            Err(e) => {
                log::warn!("Level III {:?} fetch failed: {}", l3_resp.product, e);
                return;
            }
        };

        let elevation = message.pdb.elevation_angle();
        log::info!("Level III {:?} {} fetched successfully (elevation={:.1}°)", l3_resp.product, l3_resp.tilt_code, elevation);
        self.render.level3_data.insert((l3_resp.product, l3_resp.tilt_code.clone(), l3_resp.site.clone()), Arc::new(message));

        // Trigger a re-render for panes on the same site viewing this product
        for (idx, prs) in self.render.pane_render.iter_mut().enumerate() {
            let pane_matches_site = self.gui.pane(idx).is_some_and(|p| p.site == l3_resp.site);
            if pane_matches_site && self.gui.get_rendering_params_for_pane(idx).map(|(p, _)| p) == Some(l3_resp.product) {
                prs.last_rendered = None;
            }
        }

        // Add Level III products to the scan info for panes on this site
        for pane_idx in 0..self.gui.pane_count() {
            let pane_site = self.gui.pane(pane_idx).map(|p| p.site.clone()).unwrap_or_default();
            if pane_site != l3_resp.site {
                continue;
            }
            let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                continue;
            };
            let mut info = scan_info.clone();
            let mut changed = false;
            if !info.available_products.contains(&l3_resp.product) {
                info.available_products.push(l3_resp.product);
                info.available_products.sort_by_key(|p| p.sort_order());
                info.status = format!(
                    "Loaded {} products: {}",
                    info.available_products.len(),
                    info.available_products.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")
                );
                changed = true;
            }
            // Register the actual elevation angle from the PDB
            let elevations = info.product_elevations.entry(l3_resp.product).or_default();
            let rounded_elev = (elevation * 10.0).round() / 10.0;
            if !elevations.iter().any(|e| (e - rounded_elev).abs() < 0.05) {
                elevations.push(rounded_elev);
                elevations.sort_by(|a, b| a.total_cmp(b));
                changed = true;
            }
            if changed {
                self.gui.set_scan_info_for_pane(pane_idx, info);
            }
        }
    }

    /// Poll for completed overlay rasterization results and upload textures.
    fn poll_overlay_render_results(&mut self) {
        use rustdar_egui::overlay_cache::OverlayTextureData;

        let ctx = self.state.as_ref().unwrap().egui_renderer.context();
        while let Ok(resp) = self.channels.overlay_render_receiver.try_recv() {
            // Load texture once, then clone handle to all target panes
            self.texture_counter += 1;
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [resp.width as usize, resp.height as usize],
                &resp.image_data,
            );
            let tex_name = format!("overlay_{}", self.texture_counter);
            let texture = ctx.load_texture(tex_name, color_image, egui::TextureOptions::LINEAR);

            for &pane_idx in &resp.pane_indices {
                let Some(pane) = self.gui.pane_mut(pane_idx) else {
                    continue;
                };

                let cache = pane.overlay_cache_mut(resp.overlay_kind);

                cache.render_in_flight = false;

                // Discard stale results
                if resp.generation < cache.render_generation {
                    continue;
                }

                // Save old texture for deferred cleanup
                if let Some(old) = cache.current.take() {
                    self.old_textures.push(old.texture);
                }

                cache.current = Some(OverlayTextureData {
                    texture: texture.clone(),
                    geo_bounds: resp.geo_bounds,
                    data_generation: resp.generation,
                    render_zoom: resp.zoom,
                    width: resp.width,
                    height: resp.height,
                    radar_meta: None,
                    hit_map: resp.hit_map.clone(),
                });
            }
        }
    }

    /// Check all panes for needed background renders and spawn render threads.
    fn dispatch_pane_renders(&mut self) {
        let ctx = self.state.as_ref().unwrap().egui_renderer.context().clone();
        for pane_idx in 0..self.gui.pane_count() {
            if let Some((product, elevation)) = self.gui.get_rendering_params_for_pane(pane_idx) {
                let prs = &self.render.pane_render[pane_idx];
                let needs_render = prs
                    .last_rendered
                    .map(|(last_prod, last_elev)| {
                        last_prod != product || (last_elev - elevation).abs() > ELEVATION_TOLERANCE
                    })
                    .unwrap_or(true);

                if needs_render && !prs.render_in_flight {
                    // Get the pane's site for cache lookups
                    let pane_site = self.gui.pane(pane_idx).map(|p| p.site.clone()).unwrap_or_default();

                    // Check if another pane already rendered this site+product+elevation
                    if let Some(cached) = self.render.get_cached_render(&pane_site, product, elevation) {
                        let render_result = crate::render_dispatch::CachedPaneRender {
                            image_data: Arc::clone(&cached.image_data),
                            max_range_km: cached.max_range_km,
                            value_data: Arc::clone(&cached.value_data),
                            product,
                            elevation,
                        };
                        log::info!("Reusing cached render for pane {}: {:?} at {:.1}°", pane_idx, product, elevation);
                        self.apply_render_to_pane(&ctx, pane_idx, &render_result);
                        continue;
                    }

                    let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                        continue;
                    };

                    let params = crate::render_dispatch::RenderParams {
                        product,
                        elevation,
                        lat: scan_info.site.lat,
                        lon: scan_info.site.lon,
                    };

                    if product.is_level3() {
                        self.render.try_spawn_level3_render(
                            pane_idx,
                            &params,
                            &pane_site,
                            self.channels.render_sender.clone(),
                            self.window.clone(),
                        );
                    } else if let Some(data) = self.scan_data.get(scan_info.site.name) {
                        self.render.spawn_level2_render(
                            pane_idx,
                            &params,
                            Arc::clone(data),
                            self.channels.render_sender.clone(),
                            self.window.clone(),
                        );
                    }
                }
            } else if pane_idx < self.render.pane_render.len() {
                // Only clear the radar texture if no scan data is loaded for this pane.
                // When scan_info exists but get_rendering_params returns None, the pane
                // is a Level III product waiting for elevation data — keep the old texture
                // visible until the new render replaces it.
                let has_scan = self.gui.pane(pane_idx).is_some_and(|p| p.scan_info.is_some());
                if !has_scan
                    && let Some(pane) = self.gui.pane_mut(pane_idx) {
                        let cache = pane.overlay_cache_mut(
                            rustdar_overlays::render::overlay_state::OverlayKind::Radar,
                        );
                        if let Some(old) = cache.current.take() {
                            self.old_textures.push(old.texture);
                        }
                    }
                self.render.pane_render[pane_idx].last_rendered = None;
            }
        }
    }

    /// Restore the radar image from cached raw RGBA data.
    ///
    /// Called after wgpu state is recreated (suspend/resume or surface loss) to
    /// avoid a multi-second background re-render.  Re-uploads the cached pixel
    /// data as a new GPU texture instantly.
    pub(super) fn restore_cached_render(&mut self) {
        use rustdar_egui::overlay_cache::{OverlayTextureData, RadarTextureMeta};
        use rustdar_overlays::render::overlay_state::OverlayKind;
        use rustdar_overlays::types::GeoBounds;
        use rustdar_radar::types::ImageBounds;

        let Some(state) = self.state.as_ref() else {
            return;
        };

        for pane_idx in 0..self.render.pane_render.len().min(self.gui.pane_count()) {
            let Some(ref cached) =
                self.render.pane_render[pane_idx].cached_render
            else {
                continue;
            };
            let max_range_km = cached.max_range_km;
            let product = cached.product;
            let elevation = cached.elevation;

            let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                continue;
            };
            let lat = scan_info.site.lat;
            let lon = scan_info.site.lon;

            log::info!(
                "Restoring cached radar image for pane {} ({:?} at {:.1}°) from memory",
                pane_idx,
                product,
                elevation
            );

            self.texture_counter += 1;
            let ctx = state.egui_renderer.context();
            let color_image = egui::ColorImage::from_rgba_unmultiplied([IMAGE_SIZE, IMAGE_SIZE], &cached.image_data);
            let texture_name = format!("radar_image_{}", self.texture_counter);
            let texture = ctx.load_texture(texture_name, color_image, egui::TextureOptions::NEAREST);

            let bounds = ImageBounds::from_radar_site(lat, lon);
            let geo_bounds = GeoBounds {
                min_lat: bounds.min_lat,
                max_lat: bounds.max_lat,
                min_lon: bounds.min_lon,
                max_lon: bounds.max_lon,
            };
            if let Some(pane) = self.gui.pane_mut(pane_idx) {
                let cache = pane.overlay_cache_mut(OverlayKind::Radar);
                if let Some(old) = cache.current.take() {
                    self.old_textures.push(old.texture);
                }
                cache.current = Some(OverlayTextureData {
                    texture,
                    geo_bounds,
                    data_generation: 0,
                    render_zoom: 0,
                    width: IMAGE_SIZE as u32,
                    height: IMAGE_SIZE as u32,
                    radar_meta: Some(RadarTextureMeta {
                        value_data: Arc::clone(&cached.value_data),
                        lat,
                        lon,
                        max_range_km,
                    }),
                    hit_map: None,
                });
            }
            self.render.pane_render[pane_idx].last_rendered = Some((product, elevation));
        }
    }

    /// Try to acquire the next surface texture for rendering.
    /// Returns `None` if the surface is temporarily unavailable (e.g. during
    /// a display change).  Returns `Err(true)` via the second element when
    /// the surface is *lost* and the caller must recreate rendering state.
    fn get_surface_texture(surface: &wgpu::Surface) -> (Option<wgpu::SurfaceTexture>, bool) {
        match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => (Some(texture), false),
            wgpu::CurrentSurfaceTexture::Outdated => {
                log::warn!("wgpu surface outdated, skipping frame");
                (None, false)
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                log::warn!("wgpu surface lost (display change?), will recreate state");
                (None, true)
            }
            _ => {
                log::error!("Surface error");
                (None, false)
            }
        }
    }

    pub(super) fn present_frame(&mut self, screen_descriptor: ScreenDescriptor) {
        let state = self.state.as_mut().unwrap();
        let window = self.window.as_ref().unwrap();

        let (surface_texture, surface_lost) = Self::get_surface_texture(&state.surface);
        if surface_lost {
            // Surface is irrecoverably lost (e.g. display changed on a foldable).
            // Drop the entire rendering state so the next handle_redraw() lazily
            // recreates it with a fresh surface.  Keep cached_render so the radar
            // image can be restored instantly.
            self.old_textures.clear();
            self.render.clear_last_rendered();
            self.gui.clear_graphics_state();
            self.state = None;
            return;
        }
        let Some(surface_texture) = surface_texture else {
            return;
        };

        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        // Render egui
        let textures_to_free = state.egui_renderer.end_frame_and_draw(
            &state.device,
            &state.queue,
            &mut encoder,
            window,
            &surface_view,
            screen_descriptor,
        );

        state.queue.submit(Some(encoder.finish()));
        state.egui_renderer.free_textures(&textures_to_free);
        surface_texture.present();
    }

    /// Poll for loop scan listing results. Populates the pane's frame list
    /// and kicks off downloads for each scan (throttled).
    fn poll_loop_scan_list_results(&mut self) {
        while let Ok(resp) = self.channels.loop_scan_list_receiver.try_recv() {
            let Some(pane) = self.gui.pane_mut(resp.pane_idx) else {
                continue;
            };
            let ls = &mut pane.loop_state;
            if !ls.is_active() {
                continue;
            }
            ls.phase = rustdar_egui::pane::LoopPhase::Rendering;

            // Populate frames (oldest-first, matching scan listing order)
            ls.frames = resp
                .scans
                .iter()
                .map(|(ts, _id)| rustdar_egui::pane::LoopFrame {
                    timestamp: *ts,
                    texture: None,
                    render_in_flight: false,
                    render_failed: false,
                })
                .collect();

            if !ls.frames.is_empty() {
                ls.current_frame = ls.frames.len() - 1; // start at newest
            }

            log::info!("Loop: populated {} frames for pane {}", resp.scans.len(), resp.pane_idx);

            // Cap pending downloads to MAX_LOOP_FRAMES by evenly sampling
            let mut scans = resp.scans;
            if scans.len() > MAX_LOOP_FRAMES {
                let total = scans.len();
                let sampled: Vec<_> = (0..MAX_LOOP_FRAMES)
                    .map(|i| {
                        let idx = i * (total - 1) / (MAX_LOOP_FRAMES - 1).max(1);
                        scans[idx].clone()
                    })
                    .collect();
                // Update the frames list to match the sampled set
                ls.frames = sampled.iter().map(|(ts, _)| rustdar_egui::pane::LoopFrame {
                    timestamp: *ts,
                    texture: None,
                    render_in_flight: false,
                    render_failed: false,
                }).collect();
                if !ls.frames.is_empty() {
                    ls.current_frame = ls.frames.len() - 1;
                }
                log::info!("Loop: sampled {} → {} frames for pane {}", total, MAX_LOOP_FRAMES, resp.pane_idx);
                scans = sampled;
            }

            // Store all scans as pending downloads and dispatch the first batch
            self.loop_mgr.insert_pending(resp.pane_idx, VecDeque::from(scans));
            self.dispatch_pending_loop_downloads(resp.pane_idx);
        }
    }

    /// Poll for completed loop scan downloads. When a scan arrives, store it
    /// in the global scan cache and dispatch next pending downloads.
    fn poll_loop_scan_download_results(&mut self) {
        let mut completed_count = 0usize;
        while let Ok(resp) = self.channels.loop_scan_download_receiver.try_recv() {
            self.loop_mgr.complete_download(&resp.timestamp);
            completed_count += 1;

            // Cache the downloaded scan globally (skip failures)
            if let Some(scan) = resp.scan {
                self.loop_mgr.cache_scan(resp.timestamp, scan);
            }
        }
        if completed_count > 0 {
            self.loop_mgr.complete_batch(completed_count);
            // Dispatch next pending downloads for all panes that have pending work
            let pane_indices = self.loop_mgr.pending_pane_indices();
            for pane_idx in pane_indices {
                self.dispatch_pending_loop_downloads(pane_idx);
            }
        }
    }

    /// Dispatch pending loop scan downloads up to the concurrency limit.
    fn dispatch_pending_loop_downloads(&mut self, pane_idx: usize) {
        let slots = self.loop_mgr.available_slots(MAX_CONCURRENT_LOOP_DOWNLOADS);
        if slots == 0 {
            return;
        }

        // We need to look up cached/in_flight state while modifying pending queue.
        // pending_downloads is part of loop_mgr, so we can't iterate via loop_mgr.pending_mut
        // while also calling loop_mgr.is_cached(). We extract the queue completely, Process it, and put it back.
        let mut pending = if let Some(queue) = self.loop_mgr.extract_pending(pane_idx) {
            queue
        } else {
            return;
        };

        // Filter out timestamps already cached or in flight
        let mut batch = Vec::new();
        while !pending.is_empty() && batch.len() < slots {
            let (ts, _) = pending.front().unwrap();
            if self.loop_mgr.is_cached(ts) || self.loop_mgr.is_in_flight(ts) {
                // Already have or fetching this timestamp — remove from pending
                pending.pop_front();
            } else {
                batch.push(pending.pop_front().unwrap());
            }
        }
        
        // Put the queue back
        self.loop_mgr.insert_pending(pane_idx, pending);
        
        let spawned = batch.len();

        for (ts, id) in batch {
            self.loop_mgr.mark_in_flight(ts);
            self.spawn_loop_scan_download(pane_idx, ts, id);
        }

        if spawned > 0 {
            self.loop_mgr.add_spawned(spawned);
        }
    }

    /// Poll for completed loop frame render results and upload textures.
    /// When sync_layers is on, broadcasts rendered textures to sibling panes
    /// that need the same frame (matching product+elevation+timestamp).
    fn poll_loop_render_results(&mut self) {
        let ctx = self.state.as_ref().unwrap().egui_renderer.context();
        while let Ok(rr) = self.channels.loop_render_receiver.try_recv() {
            let origin_pane = rr.pane_idx;

            let Some(pane) = self.gui.pane_mut(origin_pane) else {
                continue;
            };
            let ls = &mut pane.loop_state;

            // The coordinates the image was projected around. Read before the frame is
            // borrowed, and carried to every pane that ends up with this texture — the
            // target's site is checked on each hand-off, so this describes the image
            // for all of them rather than being re-guessed per receiver.
            let lat = ls.site_lat;
            let lon = ls.site_lon;

            // Drop results the pane is no longer expecting — rendered for a site,
            // product or elevation it has since retargeted away from, or aimed at a
            // frame that is not awaiting one. Applying either paints an image the
            // dispatcher then treats as done, so the frame never corrects itself.
            //
            // Resolves the frame in the same pass that vets the result, and hands back
            // the frame rather than its index so there is nothing here to re-derive.
            let Some(frame) = ls.frame_awaiting_render_result_mut(rr.timestamp, &rr.target) else {
                continue;
            };
            frame.render_in_flight = false;

            // Empty image_data means the render failed (no matching sweep). Mark the
            // frame so the dispatcher stops retrying it and readiness stops waiting on it.
            if rr.image_data.is_empty() {
                frame.render_failed = true;
                continue;
            }

            self.texture_counter += 1;
            let color_image =
                egui::ColorImage::from_rgba_unmultiplied([IMAGE_SIZE, IMAGE_SIZE], &rr.image_data);
            let texture_name = format!("loop_frame_{}", self.texture_counter);
            let texture = ctx.load_texture(
                texture_name,
                color_image,
                egui::TextureOptions::NEAREST,
            );

            frame.texture = Some(rustdar_egui::pane::RadarImageData {
                texture: texture.clone(),
                lat,
                lon,
                max_range_km: rr.max_range_km,
                value_data: Arc::new(Vec::new()),
            });

            // Broadcast to sibling panes with matching product+elevation+timestamp
            if self.gui.is_sync_layers() {
                for sibling_idx in 0..self.gui.pane_count() {
                    if sibling_idx == origin_pane {
                        continue;
                    }
                    let Some(sibling) = self.gui.pane_mut(sibling_idx) else { continue };
                    // Hand the image only to panes whose frames are keyed to exactly
                    // what it depicts, site included. Matching against the response
                    // rather than the origin pane's live selection keeps a retarget on
                    // either side from planting an image the receiving pane will never
                    // correct. The decision — and the frame it resolves to — lives in
                    // `LoopPlaybackState` so it stays in step with the donor test the
                    // dispatcher applies before suppressing a pane's own render.
                    let Some(sframe) = sibling
                        .loop_state
                        .frame_accepting_broadcast_mut(rr.timestamp, &rr.target)
                    else {
                        continue;
                    };
                    // If the sibling had its own render running for this frame it is now
                    // redundant: same target and timestamp means the same image, so its
                    // result is simply dropped when it arrives.
                    sframe.render_in_flight = false;
                    sframe.texture = Some(rustdar_egui::pane::RadarImageData {
                        texture: texture.clone(),
                        // The origin's coordinates — the ones this image was actually
                        // projected around. Equal to the receiver's, since the target
                        // match above pins both loops to the same site; stating the
                        // provenance keeps that a consequence rather than a coincidence.
                        lat,
                        lon,
                        max_range_km: rr.max_range_km,
                        value_data: Arc::new(Vec::new()),
                    });
                }
            }
        }
    }

    /// Promote loops from `Rendering` to `Ready` once every frame they intend to
    /// render has settled, then start playback for the panes that are ready.
    ///
    /// Runs once per frame after dispatch rather than inside the render-response
    /// drain. Several things that settle a batch never produce a render response —
    /// a frame retired as unrenderable, a texture cloned from a sibling pane, the
    /// render set shifting as the playhead moves — so a loop can be complete with
    /// nothing left to receive. A second pane whose frames are all satisfied by
    /// sibling clones spawns no renders at all, and would never be promoted.
    pub(super) fn update_loop_readiness(&mut self) {
        for pidx in 0..self.gui.pane_count() {
            let loop_mgr = &self.loop_mgr;
            let pane_downloads_done = loop_mgr.is_pane_done(pidx);
            let Some(p) = self.gui.pane_mut(pidx) else { continue };
            let pls = &mut p.loop_state;
            if !pls.is_active() || pls.is_render_ready() || pls.frames.is_empty() {
                continue;
            }
            let any_rendered = pls.frames.iter().any(|f| f.texture.is_some());
            // Every frame we intend to render must be settled — not merely "nothing
            // in flight this instant". The render budget is shared with static pane
            // renders, so part of the batch can be starved and not yet spawned; that
            // must keep the loop out of Ready instead of animating blank frames.
            let batch_settled = pls.render_set_settled(MAX_LOOP_RENDER_BUDGET, |f| {
                loop_mgr.is_cached(&f.timestamp)
            });
            if any_rendered && batch_settled && pane_downloads_done {
                pls.phase = rustdar_egui::pane::LoopPhase::Ready;
            }
        }

        // Synchronized playback start: when sync_layers is on, wait for ALL
        // looping panes to be render_ready before starting any of them.
        self.sync_loop_playback_start();
    }

    /// Start loop playback for panes that are ready, synchronizing when sync_layers is on.
    fn sync_loop_playback_start(&mut self) {
        let pane_count = self.gui.pane_count();
        let sync = self.gui.is_sync_layers() && pane_count > 1;

        // Collect readiness status for all panes with active loops
        let mut ready_panes: Vec<usize> = Vec::new();
        let mut not_ready_panes: Vec<usize> = Vec::new();
        for idx in 0..pane_count {
            let Some(pane) = self.gui.pane(idx) else { continue };
            let ls = &pane.loop_state;
            if !ls.is_active() {
                continue;
            }
            if ls.has_playback_started() {
                continue; // Already started (may be paused by user)
            }
            if ls.is_render_ready() {
                ready_panes.push(idx);
            } else {
                not_ready_panes.push(idx);
            }
        }

        if ready_panes.is_empty() {
            return;
        }

        // When syncing, only start if ALL looping panes are ready
        if sync && !not_ready_panes.is_empty() {
            return;
        }

        // Start all ready panes with the same instant and frame position
        let now = std::time::Instant::now();
        for idx in ready_panes {
            let pane = self.gui.pane_mut(idx).unwrap();
            let ls = &mut pane.loop_state;
            ls.phase = rustdar_egui::pane::LoopPhase::Playing;
            ls.last_advance = Some(now);
            // Align all panes to the last frame so they start from the same position
            if !ls.frames.is_empty() {
                ls.current_frame = ls.frames.len() - 1;
            }
        }
    }

    /// Advance loop playback for all panes with active playing loops.
    fn advance_loop_playback(&mut self) {
        let now = std::time::Instant::now();
        let interval = std::time::Duration::from_secs_f32(1.0 / self.gui.loop_speed_fps);

        for pane_idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            let ls = &mut pane.loop_state;
            if !ls.is_active() || !ls.is_playing() || ls.frames.is_empty() {
                continue;
            }

            let should_advance = ls.last_advance
                .map(|last| now.duration_since(last) >= interval)
                .unwrap_or(true);

            if should_advance {
                ls.last_advance = Some(now);
                // Skip to the next frame that has a rendered texture
                let num_frames = ls.frames.len();
                for offset in 1..=num_frames {
                    let candidate = (ls.current_frame + offset) % num_frames;
                    if ls.frames[candidate].texture.is_some() {
                        ls.current_frame = candidate;
                        break;
                    }
                }
            }
        }
    }

    /// Dispatch renders for loop frames around the playhead that have
    /// downloaded scan data but no rendered texture yet.
    fn dispatch_loop_renders(&mut self) {
        for pane_idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            let product = pane.selected_product;
            let elevation = pane.selected_elevation;
            let ls = &mut pane.loop_state;
            if !ls.is_active() || ls.frames.is_empty() {
                continue;
            }

            // The pane's product/elevation combo boxes write straight through, so
            // pick the change up here: every texture depicts the old product and
            // every render_failed flag judged the old product. Invalidating leaves
            // nothing to evict.
            if ls.retarget_renders(product, elevation) {
                log::debug!(
                    "Loop: pane {} retargeted to {:?} at {:.1}°, re-rendering all frames",
                    pane_idx, product, elevation
                );
                continue;
            }

            // Evict textures from frames far from the playhead to cap memory usage.
            ls.evict_textures_outside_render_set(MAX_LOOP_RENDER_BUDGET);
        }

        // Renders to spawn. `target` is the pane's render target (site + selected
        // product/elevation); `snapped` is that selection resolved to a sweep angle
        // present in this frame's own scan, which is what the renderer is given.
        let mut to_render: Vec<LoopRenderRequest> = Vec::new();
        // Frames that can be satisfied by cloning a sibling's texture. Both frame
        // indices are resolved here and used as-is below — re-finding either by
        // timestamp would be a second lookup free to disagree with this one.
        let mut to_clone: Vec<LoopCloneRequest> = Vec::new();
        // Frames whose scan carries no sweep for the selected product: (pane_idx, frame_idx).
        // Recorded so they stop being retried and stop holding up readiness.
        let mut to_mark_failed: Vec<(usize, usize)> = Vec::new();

        let sync = self.gui.is_sync_layers();
        let pane_count = self.gui.pane_count();

        for pane_idx in 0..pane_count {
            let Some(pane) = self.gui.pane(pane_idx) else {
                continue;
            };
            let ls = &pane.loop_state;
            if !ls.is_active() || ls.frames.is_empty() {
                continue;
            }

            let site_lat = ls.site_lat;
            let site_lon = ls.site_lon;
            let product = pane.selected_product;
            let elevation = pane.selected_elevation;

            // Set by `retarget_renders` in the loop above for every active, non-empty
            // loop. Carried through the plan so the dedup, the donor search and the
            // dispatch stamp all read the one value instead of re-deriving it.
            let Some(target) = ls.rendered_for.clone() else {
                continue;
            };

            // The intended render set — shared with the readiness check so the two
            // cannot drift apart (see `LoopPlaybackState::render_set_settled`).
            let indices = ls.render_set_indices(MAX_LOOP_RENDER_BUDGET);

            for &idx in &indices {
                let frame = &ls.frames[idx];
                if frame.texture.is_some() || frame.render_in_flight || frame.render_failed {
                    continue;
                }

                // Take a sibling's texture instead of rendering, but only from a loop
                // keyed to the same target. Same test the response-path broadcast
                // applies, so the two cannot disagree about who may serve this frame.
                if sync {
                    let donor = find_donor(
                        (0..pane_count).filter_map(|i| self.gui.pane(i).map(|p| (i, &p.loop_state))),
                        pane_idx,
                        frame.timestamp,
                        &target,
                    );
                    if let Some((src_pane, src_frame)) = donor {
                        to_clone.push(LoopCloneRequest {
                            dest_pane: pane_idx,
                            dest_frame: idx,
                            src_pane,
                            src_frame,
                        });
                        continue;
                    }
                }

                if let Some(scan) = self.loop_mgr.get_cached(&frame.timestamp) {
                    // Snap elevation to closest available in this particular scan
                    let Some(snapped) = rustdar_radar::render::find_closest_elevation(scan, product, elevation) else {
                        // This scan has no sweep carrying the selected product at all.
                        // Nothing will ever render it, so retire the frame.
                        to_mark_failed.push((pane_idx, idx));
                        continue;
                    };
                    // Deduplicate: if another pane already queued a render for the same
                    // target and timestamp, skip — the broadcast in
                    // poll_loop_render_results will deliver the texture to this pane.
                    if sync && render_already_queued(&to_render, frame.timestamp, &target, snapped) {
                        continue;
                    }
                    to_render.push(LoopRenderRequest {
                        pane_idx,
                        frame_idx: idx,
                        timestamp: frame.timestamp,
                        target: target.clone(),
                        snapped,
                        site_lat,
                        site_lon,
                    });
                }
            }
        }

        // Retire frames that cannot be rendered at the selected product/elevation
        for (pane_idx, frame_idx) in to_mark_failed {
            if let Some(pane) = self.gui.pane_mut(pane_idx)
                && let Some(frame) = pane.loop_state.frames.get_mut(frame_idx)
            {
                frame.render_failed = true;
            }
        }

        // Apply cloned textures from sibling panes (no render needed). Both indices
        // were resolved during planning; nothing since has reordered either frame list
        // (`to_mark_failed` only sets a flag), so they are used directly.
        for req in to_clone {
            let cloned = {
                let Some(src) = self.gui.pane(req.src_pane) else { continue };
                let Some(sframe) = src.loop_state.frames.get(req.src_frame) else { continue };
                let Some(tex) = sframe.texture.clone() else { continue };
                tex
            };
            let Some(dest) = self.gui.pane_mut(req.dest_pane) else { continue };
            if let Some(dframe) = dest.loop_state.frames.get_mut(req.dest_frame) {
                dframe.texture = Some(cloned);
            }
        }

        // Now spawn renders and mark the frames in flight, respecting concurrent limit
        for req in to_render {
            // Check concurrent render limit before each spawn (shared with static pane renders)
            let current = self.render.renders_in_flight.load(Ordering::Relaxed);
            if current >= MAX_CONCURRENT_RENDERS {
                break;
            }

            let scan_arc = Arc::clone(self.loop_mgr.get_cached(&req.timestamp).unwrap());

            // Only mark the frame in flight if a thread was actually spawned. If the
            // spawn is refused (budget taken between the check above and the one inside),
            // no LoopRenderResponse will ever arrive to clear the flag, and the frame
            // would stay blank and be skipped forever.
            //
            // `req.target` is the target the frame state was keyed to when this request
            // was planned, and is stamped on the response so a result that outlives a
            // retarget is recognised as stale on arrival.
            let spawned = self.spawn_loop_frame_render(
                req.pane_idx,
                req.timestamp,
                scan_arc,
                &req.render_params(),
                req.target,
            );

            if spawned && let Some(pane) = self.gui.pane_mut(req.pane_idx) {
                pane.loop_state.frames[req.frame_idx].render_in_flight = true;
            }
        }
    }
}

/// A loop frame render the dispatcher intends to spawn.
struct LoopRenderRequest {
    pane_idx: usize,
    frame_idx: usize,
    timestamp: chrono::NaiveDateTime,
    /// The pane's render target: site plus *selected* product and elevation. What the
    /// result is keyed on — never what the renderer is given. See `render_params`.
    target: RenderTarget,
    /// `target.elevation` resolved to a sweep angle this frame's own scan carries.
    snapped: f32,
    site_lat: f64,
    site_lon: f64,
}

impl LoopRenderRequest {
    /// The inputs the renderer is handed.
    ///
    /// `elevation` is the *snapped* sweep angle, never `target.elevation`. The two are
    /// adjacent and both plausible, so the choice is made here once and asserted in
    /// tests rather than re-made at the call site. They are not interchangeable:
    /// `find_closest_elevation` returns the nearest sweep in this frame's own scan,
    /// which can sit arbitrarily far from the selection, while `find_sweep` only
    /// matches within 0.05°. Passing the selection would return `None` for every frame
    /// whose nearest sweep is further away than that — an empty response, and a frame
    /// retired as unrenderable that renders perfectly well.
    fn render_params(&self) -> crate::render_dispatch::RenderParams {
        crate::render_dispatch::RenderParams {
            product: self.target.product,
            elevation: self.snapped,
            lat: self.site_lat,
            lon: self.site_lon,
        }
    }
}

/// A loop frame that a sibling pane's already-rendered texture can satisfy.
struct LoopCloneRequest {
    dest_pane: usize,
    dest_frame: usize,
    src_pane: usize,
    src_frame: usize,
}

/// The `(pane, frame)` that can serve `timestamp` for a pane keyed to `target`
/// without a new render, or `None` if nobody can.
///
/// `target` is the *receiver's* — the one pane whose frame is being filled — and it is
/// the only one in scope here on purpose. Every candidate is asked about that same
/// target. Asking a candidate about its own `rendered_for` instead would compare it to
/// itself and always agree, which is precisely how a loop on one site comes to donate
/// to a loop on another; taking one target for all candidates makes that mis-wiring
/// unrepresentable rather than merely wrong.
///
/// `receiver` is skipped: a pane cannot serve itself, and the frame being filled is by
/// definition untextured.
fn find_donor<'a>(
    loops: impl IntoIterator<Item = (usize, &'a rustdar_egui::pane::LoopPlaybackState)>,
    receiver: usize,
    timestamp: chrono::NaiveDateTime,
    target: &RenderTarget,
) -> Option<(usize, usize)> {
    loops
        .into_iter()
        .filter(|&(idx, _)| idx != receiver)
        .find_map(|(idx, ls)| Some((idx, ls.frame_donatable_to(timestamp, target)?)))
}

/// Whether `queued` already covers a render for `timestamp` at `target`.
///
/// Suppressing a pane's own render here is a promise that the queued render's result
/// will be broadcast to it, so this must test exactly what
/// `LoopPlaybackState::frame_accepting_broadcast` tests — the whole target, site
/// included. A site-blind check suppresses the render of a pane the broadcast will
/// then refuse, and the frame is served by neither path.
///
/// `snapped` is compared as well, which is strictly stronger than what
/// `frame_accepting_broadcast` tests — it does not look at the sweep at all. That
/// asymmetry is safe only in this direction: suppression implies acceptance, never the
/// reverse. Two loops on the same target cannot currently resolve the selection to
/// different sweeps, because the scan cache is keyed on timestamp alone and hands both
/// of them the same `Arc<Scan>`. If that key ever gains a site — the fix this file's
/// `RenderTarget` docs point at — the hole is the *broadcast*, not this: it would hand
/// over a differently-snapped image, drop the receiver's own in-flight render as
/// redundant, and leave the wrong sweep in place permanently. Whoever re-keys the
/// cache has to give `frame_accepting_broadcast` a sweep comparison in the same change.
fn render_already_queued(
    queued: &[LoopRenderRequest],
    timestamp: chrono::NaiveDateTime,
    target: &RenderTarget,
    snapped: f32,
) -> bool {
    queued.iter().any(|r| {
        r.timestamp == timestamp
            && r.target.matches(target)
            && (r.snapped - snapped).abs() <= ELEVATION_TOLERANCE
    })
}

#[cfg(test)]
mod loop_dispatch_tests {
    use super::*;
    use rustdar_egui::pane::{LoopFrame, LoopPhase, LoopPlaybackState};
    use rustdar_radar::sites::RadarSite;
    use rustdar_radar::types::RadarProduct;

    fn ts(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, minute, 0)
            .unwrap()
    }

    fn target(site: &str, elevation: f32) -> RenderTarget {
        RenderTarget::new(site, RadarProduct::Reflectivity, elevation)
    }

    /// A loop on `site` with three frames, retargeted to Reflectivity at 0.5, and
    /// with `textured` already rendered.
    fn loop_on(ctx: &egui::Context, site: &'static str, textured: &[usize]) -> LoopPlaybackState {
        let mut ls = LoopPlaybackState::new_for_loop(
            3600,
            &RadarSite { name: site, lat: 35.0, lon: -97.0, elev: None },
        );
        ls.phase = LoopPhase::Rendering;
        ls.frames = (0..3)
            .map(|i| LoopFrame {
                timestamp: ts(i),
                texture: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        ls.retarget_renders(RadarProduct::Reflectivity, 0.5);
        for &i in textured {
            let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
            ls.frames[i].texture = Some(rustdar_egui::pane::RadarImageData {
                texture: ctx.load_texture("test", image, egui::TextureOptions::NEAREST),
                lat: 35.0,
                lon: -97.0,
                max_range_km: 100.0,
                value_data: Arc::new(Vec::new()),
            });
        }
        ls
    }

    fn queued(target: RenderTarget, timestamp: chrono::NaiveDateTime, snapped: f32) -> LoopRenderRequest {
        LoopRenderRequest {
            pane_idx: 0,
            frame_idx: 0,
            timestamp,
            target,
            snapped,
            site_lat: 35.0,
            site_lon: -97.0,
        }
    }

    /// The behaviour the dedup exists for: one render serves both panes.
    #[test]
    fn a_queued_render_for_the_same_target_suppresses_a_duplicate() {
        let q = vec![queued(target("KTLX", 0.5), ts(0), 0.48)];
        assert!(render_already_queued(&q, ts(0), &target("KTLX", 0.5), 0.48));
        // Selection jitter within tolerance is the same target.
        assert!(render_already_queued(&q, ts(0), &target("KTLX", 0.505), 0.48));
    }

    /// The defect: suppressing here promises a broadcast that
    /// `frame_accepting_broadcast` refuses across sites, leaving the frame served by
    /// neither path — and pushing the pane into the site-blind clone path instead.
    #[test]
    fn a_queued_render_for_another_site_suppresses_nothing() {
        let q = vec![queued(target("KTLX", 0.5), ts(0), 0.48)];
        assert!(
            !render_already_queued(&q, ts(0), &target("KOUN", 0.5), 0.48),
            "a pane on another site must still render its own frame"
        );
    }

    #[test]
    fn a_queued_render_at_another_timestamp_or_sweep_suppresses_nothing() {
        let q = vec![queued(target("KTLX", 0.5), ts(0), 0.48)];
        assert!(!render_already_queued(&q, ts(1), &target("KTLX", 0.5), 0.48));
        // Same target, but the two scans resolved the selection to different sweeps,
        // so the images differ.
        assert!(!render_already_queued(&q, ts(0), &target("KTLX", 0.5), 1.5));
        assert!(!render_already_queued(&[], ts(0), &target("KTLX", 0.5), 0.48));
    }

    #[test]
    fn a_queued_render_for_another_product_suppresses_nothing() {
        let q = vec![queued(target("KTLX", 0.5), ts(0), 0.48)];
        let velocity = RenderTarget::new("KTLX", RadarProduct::Velocity, 0.5);
        assert!(!render_already_queued(&q, ts(0), &velocity, 0.48));
    }

    /// The wiring the donor search exists to get right: every candidate is judged
    /// against the *receiver's* target. Judging each against its own would compare it
    /// to itself, always agree, and put a KTLX image into a KOUN loop.
    #[test]
    fn a_donor_is_judged_against_the_receiving_panes_target() {
        let ctx = egui::Context::default();
        let ktlx = loop_on(&ctx, "KTLX", &[1]);
        let koun = loop_on(&ctx, "KOUN", &[]);
        let loops = [(0usize, &ktlx), (1usize, &koun)];

        // Pane 1 (KOUN) asks. Pane 0 has the frame textured, but on another site.
        assert_eq!(
            find_donor(loops, 1, ts(1), koun.rendered_for.as_ref().unwrap()),
            None,
            "a KTLX loop must not serve a KOUN loop"
        );
        // The same candidate judged against its own target would have agreed.
        assert_eq!(
            find_donor(loops, 1, ts(1), ktlx.rendered_for.as_ref().unwrap()),
            Some((0, 1)),
            "precondition: only the target argument distinguishes these"
        );
    }

    #[test]
    fn a_donor_on_the_same_target_is_found_and_never_the_receiver_itself() {
        let ctx = egui::Context::default();
        let a = loop_on(&ctx, "KTLX", &[2]);
        let b = loop_on(&ctx, "KTLX", &[]);
        let loops = [(0usize, &a), (1usize, &b)];
        let want = b.rendered_for.as_ref().unwrap();

        assert_eq!(find_donor(loops, 1, ts(2), want), Some((0, 2)));
        // Pane 0 asking for the same frame is not offered its own texture.
        assert_eq!(find_donor(loops, 0, ts(2), want), None);
        // Nobody has a frame at this timestamp textured.
        assert_eq!(find_donor(loops, 1, ts(0), want), None);
    }

    /// `target.elevation` is the pane's selection; `snapped` is the sweep this frame's
    /// scan actually carries. `find_sweep` only matches within 0.05°, so handing the
    /// renderer the selection retires every frame whose nearest sweep is further away.
    #[test]
    fn the_renderer_is_given_the_snapped_sweep_not_the_selection() {
        // A selection of 0.5 that snapped to a 1.4° sweep — well outside find_sweep's
        // 0.05° window, so the two are not interchangeable.
        let req = queued(target("KTLX", 0.5), ts(0), 1.4);
        let params = req.render_params();

        assert_eq!(params.elevation, 1.4, "the sweep the scan carries");
        assert_ne!(params.elevation, req.target.elevation);
        assert_eq!(params.product, RadarProduct::Reflectivity);
        assert_eq!(params.lat, 35.0);
        assert_eq!(params.lon, -97.0);
    }
}
