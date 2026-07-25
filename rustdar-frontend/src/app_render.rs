use egui_wgpu::wgpu;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use rustdar_egui::actions::GuiAction;
use rustdar_egui::pane::{BroadcastSweep, ELEVATION_TOLERANCE, RenderTarget};
use rustdar_radar::types::IMAGE_SIZE;
use crate::constants::{MAX_CONCURRENT_RENDERS, MAX_LOOP_RENDER_BUDGET, MAX_CONCURRENT_LOOP_DOWNLOADS, MAX_LOOP_FRAMES};
use crate::loop_downloads::PendingDownloads;
use crate::render_dispatch::CachedPaneRender;

/// What the swapchain had for us this frame.
pub(crate) enum SurfaceStatus {
    /// A texture to draw into.
    Ready(wgpu::SurfaceTexture),
    /// Nothing available right now; skip presenting but keep the state.
    Skip,
    /// The surface is gone and the whole rendering state must be rebuilt.
    Lost,
}

/// Finish this frame's egui pass, then ask the swapchain for somewhere to draw.
///
/// It has to be this way round because `Context::end_pass` is the call that pops
/// egui's viewport stack and hands over the frame's texture deltas. Acquiring
/// first and bailing out on failure — which is what this code used to do —
/// leaves the pass open for good: `begin_pass` pushes onto that stack every
/// frame and nothing ever pops it, so egui stops believing it is on the
/// outermost viewport and silently drops pending zoom/scale changes from then
/// on.
///
/// Uploading before acquiring matters for a second reason. egui emits each
/// font-atlas region exactly once — a full allocation, then per-glyph partial
/// updates — so once a delta has been handed over it is gone. Anything that
/// takes the deltas and then returns without applying them desyncs egui's
/// renderer permanently.
///
/// # Why `acquire` is handed the finished pass
///
/// It does not need it. The `&P` is a token: it makes the finished pass an
/// *input* to acquisition, so the ordering is enforced by data flow rather than
/// by statement order.
///
/// Returning `(P, SurfaceStatus)` is not enough on its own. It forces this
/// function to call `finish_pass`, but it says nothing about a caller that
/// acquires a surface on its own before calling this at all — which is exactly
/// the bug being fixed, and it re-compiles clean under the weaker signature.
/// [`super::App::get_surface_texture`] therefore takes a `&PreparedFrame` it
/// never reads, so acquiring without having finished the pass is not a mistake
/// anyone can make quietly: it fails to compile.
pub(crate) fn finish_then_acquire<P>(
    finish_pass: impl FnOnce() -> P,
    acquire: impl FnOnce(&P) -> SurfaceStatus,
) -> (P, SurfaceStatus) {
    let prepared = finish_pass();
    // `acquire` cannot be hoisted above this line: it needs `prepared`.
    let status = acquire(&prepared);
    (prepared, status)
}

impl super::App {
    /// Set up and run the egui UI pass.
    ///
    /// Returns the surface size in pixels and any GUI actions triggered. Only
    /// the size is returned: the scale the frame is laid out at is handed to
    /// egui here and read back off the context when the pass ends, so there is
    /// no second copy of it to drift.
    ///
    /// The scale handed to egui accounts for:
    /// - Application scale factor (state.scale_factor)
    /// - Surface-to-window ratio (matters on web, where the canvas backing
    ///   store can differ from the CSS size)
    ///
    /// OS display scaling is *not* included: egui-winit puts it on the raw input
    /// and egui applies it itself.
    pub(super) fn setup_egui_frame(&mut self) -> ([u32; 2], Vec<GuiAction>) {
        // Apply theme and run the egui UI pass.
        // Scoped so `state` is dropped before we call &mut self methods below.
        let (size_in_pixels, gui_action) = {
            let state = self.state.as_mut().unwrap();
            let window = self.window.as_ref().unwrap();

            let window_size = window.inner_size();
            let css_to_canvas_scale_x =
                state.surface_config.width as f32 / window_size.width.max(1) as f32;
            // The application's own scaling only. `window.scale_factor()` is
            // deliberately not folded in: egui already has it from the raw input
            // and multiplies it back on, using the value for the pass being
            // started rather than the one it happened to hold beforehand.
            let zoom_factor = state.scale_factor * css_to_canvas_scale_x;

            let size_in_pixels = [state.surface_config.width, state.surface_config.height];

            // Start egui frame
            state.egui_renderer.begin_frame(window, zoom_factor);

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

            (size_in_pixels, gui_action)
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

        (size_in_pixels, gui_action)
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
    ///
    /// `_finished` is never read. It is required so that acquiring a surface is
    /// impossible without already holding this frame's finished egui pass —
    /// see [`finish_then_acquire`], whose ordering this is half of. Dropping the
    /// parameter would make the pre-fix bug (acquire first, return early, leave
    /// the pass open) compile cleanly again.
    fn get_surface_texture(
        surface: &wgpu::Surface,
        _finished: &crate::egui_renderer::PreparedFrame,
    ) -> SurfaceStatus {
        match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => SurfaceStatus::Ready(texture),
            wgpu::CurrentSurfaceTexture::Outdated => {
                log::warn!("wgpu surface outdated, skipping frame");
                SurfaceStatus::Skip
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                log::warn!("wgpu surface lost (display change?), will recreate state");
                SurfaceStatus::Lost
            }
            _ => {
                log::error!("Surface error");
                SurfaceStatus::Skip
            }
        }
    }

    pub(super) fn present_frame(&mut self, size_in_pixels: [u32; 2]) {
        let state = self.state.as_mut().unwrap();
        let window = self.window.as_ref().unwrap();

        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        // Finish egui's pass and upload its textures, THEN ask for a surface.
        // The order is enforced by data flow, not by the order of these lines:
        // acquisition takes the finished pass as an argument. See the helper.
        let (frame, status) = finish_then_acquire(
            || {
                state.egui_renderer.end_pass_and_upload(
                    &state.device,
                    &state.queue,
                    &mut encoder,
                    window,
                    size_in_pixels,
                )
            },
            |finished| Self::get_surface_texture(&state.surface, finished),
        );

        let surface_texture = match status {
            SurfaceStatus::Ready(texture) => texture,
            SurfaceStatus::Skip | SurfaceStatus::Lost => {
                // Nothing to draw into, but the uploads recorded above still have
                // to land: egui already handed over these deltas and will never
                // re-send them. Submitting the encoder flushes them, and the
                // retired textures are safe to free because nothing painted with
                // them this frame.
                state.queue.submit(Some(encoder.finish()));
                state
                    .egui_renderer
                    .free_textures(frame.textures_to_free());

                if matches!(status, SurfaceStatus::Lost) {
                    // Surface is irrecoverably lost (e.g. display changed on a
                    // foldable). Drop the entire rendering state so the next
                    // handle_redraw() lazily recreates it with a fresh surface.
                    // Keep cached_render so the radar image can be restored
                    // instantly.
                    self.old_textures.clear();
                    self.render.clear_last_rendered();
                    self.gui.clear_graphics_state();
                    self.state = None;
                }
                return;
            }
        };

        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        state.egui_renderer.draw(&mut encoder, &surface_view, &frame);

        state.queue.submit(Some(encoder.finish()));
        state.egui_renderer.free_textures(frame.textures_to_free());
        surface_texture.present();
    }

    /// Poll for loop scan listing results. Populates the pane's frame list
    /// and kicks off downloads for each scan (throttled).
    fn poll_loop_scan_list_results(&mut self) {
        while let Ok(resp) = self.channels.loop_scan_list_receiver.try_recv() {
            let Some(pane) = self.gui.pane_mut(resp.pane_idx) else {
                continue;
            };
            // Whether this listing is still wanted, and what it makes of the frame
            // list, is decided in one place — including refusing a listing for a
            // site the pane's loop has since moved off.
            let Some(pending) = accept_scan_listing(&mut pane.loop_state, &resp.site, resp.scans)
            else {
                continue;
            };
            log::info!(
                "Loop: populated {} {} frames for pane {}",
                pending.queue.len(), pending.site, resp.pane_idx
            );

            // Store the scans as pending downloads — with the site they were listed
            // for — and dispatch the first batch.
            self.loop_mgr.insert_pending(resp.pane_idx, pending);
            self.dispatch_pending_loop_downloads(resp.pane_idx);
        }
    }

    /// Poll for completed loop scan downloads. When a scan arrives, store it
    /// in the global scan cache and dispatch next pending downloads.
    fn poll_loop_scan_download_results(&mut self) {
        let mut completed_count = 0usize;
        while let Ok(resp) = self.channels.loop_scan_download_receiver.try_recv() {
            apply_completed_download(&mut self.loop_mgr, resp);
            completed_count += 1;
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

        // We need to look up cached/in_flight state while modifying the pending
        // queue, and both live in loop_mgr, so the queue is extracted completely,
        // processed, and put back.
        //
        // The site comes out with it. Every cache and in-flight question below is
        // asked about the site these identifiers were *listed* for — the site their
        // scans will be cached under and looked up under at render time. Re-reading
        // it off the pane would label a stale listing's files with whatever site the
        // pane's loop has since become.
        let Some(PendingDownloads { site, mut queue }) = self.loop_mgr.extract_pending(pane_idx)
        else {
            return;
        };

        // Filter out timestamps already cached or in flight for this site
        let mut batch = Vec::new();
        while !queue.is_empty() && batch.len() < slots {
            let (ts, _) = queue.front().unwrap();
            if self.loop_mgr.is_cached(&site, ts) || self.loop_mgr.is_in_flight(&site, ts) {
                // Already have or fetching this scan — remove from pending
                queue.pop_front();
            } else {
                batch.push(queue.pop_front().unwrap());
            }
        }

        let spawned = batch.len();

        for (ts, id) in batch {
            self.loop_mgr.mark_in_flight(&site, ts);
            self.spawn_loop_scan_download(pane_idx, site.clone(), ts, id);
        }

        // Put the queue back, still carrying its own site
        self.loop_mgr.insert_pending(pane_idx, PendingDownloads { site, queue });

        if spawned > 0 {
            self.loop_mgr.add_spawned(spawned);
        }
    }

    /// Poll for completed loop frame render results and upload textures.
    /// When sync_layers is on, broadcasts rendered textures to sibling panes
    /// that need the same frame (matching product+elevation+timestamp).
    fn poll_loop_render_results(&mut self) {
        let ctx = self.state.as_ref().unwrap().egui_renderer.context();
        while let Ok(mut rr) = self.channels.loop_render_receiver.try_recv() {
            let origin_pane = rr.pane_idx;

            let Some(pane) = self.gui.pane_mut(origin_pane) else {
                continue;
            };

            // Vetting the result, retiring a failed render and placing the image are
            // one step over one resolved frame — see `accept_render_result`. The
            // texture is uploaded from inside it, so a result this pane has
            // retargeted away from costs no GPU memory.
            let counter = &mut self.texture_counter;
            let Some(texture) =
                accept_render_result(&mut pane.loop_state, &mut rr, |color_image| {
                    *counter += 1;
                    // `color_image` is the only copy of this frame's pixels on this
                    // thread — the renderer's RGBA buffer was dropped on the worker —
                    // and it is moved into the texture manager here rather than copied.
                    ctx.load_texture(
                        format!("loop_frame_{counter}"),
                        color_image,
                        egui::TextureOptions::NEAREST,
                    )
                })
            else {
                continue;
            };

            // Broadcast to sibling panes with matching product+elevation+timestamp
            if self.gui.is_sync_layers() {
                for sibling_idx in 0..self.gui.pane_count() {
                    if sibling_idx == origin_pane {
                        continue;
                    }
                    let Some(sibling_loop) = self.gui.pane(sibling_idx).map(|p| &p.loop_state)
                    else {
                        continue;
                    };
                    // Cheap refusal first. This is the same predicate
                    // `frame_accepting_broadcast` applies as the authority below, not a
                    // second opinion — it just skips resolving a sweep for the many
                    // siblings that cannot take the image anyway.
                    if !sibling_loop.is_rendered_for(&rr.target) {
                        continue;
                    }
                    let sweep = broadcast_sweep(&self.loop_mgr, sibling_loop, &rr);

                    let Some(sibling) = self.gui.pane_mut(sibling_idx) else { continue };
                    // Hand the image only to panes whose frames are keyed to exactly
                    // what it depicts, site and sweep included. Matching against the
                    // response rather than the origin pane's live selection keeps a
                    // retarget on either side from planting an image the receiving pane
                    // will never correct. The decision — and the frame it resolves to —
                    // lives in `LoopPlaybackState` so it stays in step with the donor
                    // test the dispatcher applies before suppressing a pane's own render.
                    let Some(sframe) = sibling
                        .loop_state
                        .frame_accepting_broadcast_mut(rr.timestamp, &rr.target, sweep)
                    else {
                        continue;
                    };
                    // If the sibling had its own render running for this frame it is now
                    // redundant: same target and timestamp means the same image, so its
                    // result is simply dropped when it arrives.
                    sframe.render_in_flight = false;
                    // The same response the origin frame was filled from, so every
                    // pane holding this texture agrees about what it depicts and
                    // where it sits. The receiver's own `site_lat`/`site_lon` are
                    // never consulted here — see `LoopRenderResponse::site_lat`.
                    sframe.texture = Some(rendered_image(&rr, &texture));
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
            let batch_settled = loop_batch_settled(loop_mgr, pls, MAX_LOOP_RENDER_BUDGET);
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
        let now = web_time::Instant::now();
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
        let now = web_time::Instant::now();
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

                if let Some(scan) = frame_scan(&self.loop_mgr, &target, frame.timestamp) {
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

            // The same cache entry the plan resolved above, named the same way: by
            // the target this render is for. Nothing between then and here removes
            // an entry, but a missing scan is a skipped frame the next pass retries,
            // not something to bring the process down over.
            let Some(scan_arc) = frame_scan(&self.loop_mgr, &req.target, req.timestamp).cloned()
            else {
                continue;
            };

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
                req.render_params(),
                req.target,
            );

            if spawned && let Some(pane) = self.gui.pane_mut(req.pane_idx) {
                pane.loop_state.frames[req.frame_idx].render_in_flight = true;
            }
        }
    }
}

/// Take a scan listing for `site` into `ls`'s frame list, returning the downloads
/// it now owes — or `None` if this loop is not the one that asked for it.
///
/// A listing is an uncancellable network round-trip, and a pane's loop is rebuilt
/// out from under it routinely: by a site switch, by `reinit_active_loops` after a
/// time navigation, by every settle of the lookback slider. So a listing can arrive
/// for a loop that no longer exists, and "does this pane still have *a* loop" cannot
/// tell that apart from a live one. Comparing the site can: a listing for the site
/// the loop was on before a switch names files that are not this loop's, and taking
/// them would put another radar's timestamps in the frame list and another radar's
/// identifiers in the download queue — where, labelled with this loop's site, they
/// would be cached as this site's scans and rendered with its geometry.
///
/// Stale listings for the *same* site name that site's own files, and are still
/// taken, as the last word. Not quite free, though: one requested before a lookback
/// *shrink* covers a wider span than the loop now asks for, so taking it leaves a
/// frame list — and a correspondingly oversized download queue — transiently wider
/// than the current `lookback_secs`. That self-corrects at the next poll, whose
/// eviction measures the window from the newest frame against the loop's current
/// `lookback_secs`. Closing the gap properly needs a generation counter, which is
/// not worth carrying for a few extra frames that expire on their own.
///
/// The frame list and the returned queue are built from one sampled set on purpose:
/// they are the two halves of the same plan, and a frame with no queued download
/// never settles.
fn accept_scan_listing(
    ls: &mut rustdar_egui::pane::LoopPlaybackState,
    site: &str,
    scans: Vec<(chrono::NaiveDateTime, rustdar_radar::archive::Identifier)>,
) -> Option<PendingDownloads> {
    if !ls.is_active() || ls.site != site {
        return None;
    }

    // Cap the downloads at MAX_LOOP_FRAMES by evenly sampling the listing.
    let scans = if scans.len() > MAX_LOOP_FRAMES {
        let total = scans.len();
        let sampled: Vec<_> = (0..MAX_LOOP_FRAMES)
            .map(|i| scans[i * (total - 1) / (MAX_LOOP_FRAMES - 1).max(1)].clone())
            .collect();
        log::info!("Loop: sampled {} → {} frames for {}", total, MAX_LOOP_FRAMES, site);
        sampled
    } else {
        scans
    };

    ls.phase = rustdar_egui::pane::LoopPhase::Rendering;
    // Oldest-first, matching the scan listing order.
    ls.frames = scans
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

    Some(PendingDownloads { site: site.to_string(), queue: VecDeque::from(scans) })
}

/// The frame image a finished loop render describes.
///
/// Every field comes off the response. The coordinates in particular are the ones
/// the renderer was handed, so this describes the image for whoever ends up holding
/// it — the pane that asked for it and every sibling the broadcast hands it to —
/// rather than being re-derived once per receiver from state that merely happens to
/// agree. See [`crate::channels::LoopRenderResponse::site_lat`].
fn rendered_image(
    rr: &crate::channels::LoopRenderResponse,
    texture: &egui::TextureHandle,
) -> rustdar_egui::pane::RadarImageData {
    rustdar_egui::pane::RadarImageData {
        texture: texture.clone(),
        lat: rr.site_lat,
        lon: rr.site_lon,
        max_range_km: rr.max_range_km,
        value_data: Arc::new(Vec::new()),
    }
}

/// Place a finished loop render on the frame of `ls` that asked for it, returning
/// the texture that was uploaded so the caller can offer it to sibling panes.
///
/// `None` means nothing was placed, for one of two reasons:
/// - The result is not one this loop is still expecting — rendered for a site,
///   product or elevation it has since retargeted away from, or aimed at a frame
///   that is not awaiting one. Applying either paints an image the dispatcher then
///   treats as done, so the frame never corrects itself.
/// - The render failed — no image, meaning the scan carried no matching sweep. The
///   frame is retired so the dispatcher stops retrying it and readiness stops
///   waiting on it.
///
/// The frame is resolved once, in the same pass that vets the result, and held: the
/// vet and the placement cannot end up describing different frames. `upload` is
/// handed the pixels and runs only after both checks have passed, so a refused
/// result costs no GPU texture.
///
/// `rr` is taken by `&mut` so the image can be `take`n rather than moved out of the
/// response. That is deliberate and load-bearing at the call site: the sibling
/// broadcast below hands the *whole response* to `broadcast_sweep`, because the
/// receiver's half of the sweep comparison must be resolved from the receiver's own
/// scan and never filled in from a loose `f32`. Partially moving `rr` here would
/// make `&rr` unavailable there and invite exactly that inlining.
fn accept_render_result(
    ls: &mut rustdar_egui::pane::LoopPlaybackState,
    rr: &mut crate::channels::LoopRenderResponse,
    upload: impl FnOnce(egui::ColorImage) -> egui::TextureHandle,
) -> Option<egui::TextureHandle> {
    let frame = ls.frame_awaiting_render_result_mut(rr.timestamp, &rr.target)?;
    frame.render_in_flight = false;

    let Some(color_image) = rr.image.take() else {
        frame.render_failed = true;
        return None;
    };

    let texture = upload(color_image);
    frame.texture = Some(rendered_image(rr, &texture));
    Some(texture)
}

/// Record a finished download: clear its in-flight mark and cache the scan.
///
/// Takes the whole response so the site can only come from the download itself.
/// The requesting pane is deliberately out of scope here — it is the one thing in
/// reach that looks like an answer and is not one, since its loop can have been
/// rebuilt for another site while this download ran.
fn apply_completed_download(
    loop_mgr: &mut crate::loop_downloads::LoopDownloadManager,
    resp: crate::channels::LoopScanDownloadResponse,
) {
    loop_mgr.complete_download(&resp.site, &resp.timestamp);
    // Skip failures — the mark is cleared either way so the frame can be retried.
    if let Some(scan) = resp.scan {
        loop_mgr.cache_scan(&resp.site, resp.timestamp, scan);
    }
}

/// The scan a loop keyed to `target` renders for `timestamp`.
///
/// `target.site` is where the loop's geometry came from, so it is also the only
/// site whose scan may be projected with it. The pane's live `site` field is not a
/// substitute — it is re-synced across panes without rebuilding their loops — and
/// it is not in scope here.
fn frame_scan<'a>(
    loop_mgr: &'a crate::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> Option<&'a Arc<nexrad_model::data::Scan>> {
    loop_mgr.get_cached(&target.site, &timestamp)
}

/// The sweep `ls`'s own scan for `timestamp` snaps `product`/`elevation` to, or
/// `None` if it has no such scan or that scan carries no sweep for the product.
///
/// This is the receiver's half of a broadcast check, so it must be answerable
/// *without* the sender's result: the site comes from `ls`, and the selection is
/// passed loose rather than as a `RenderTarget` so the sender's site is not even in
/// reach. Handed the sender's own snapped angle instead, the comparison would
/// compare a value to itself and agree unconditionally.
///
/// Returning `None` refuses the broadcast, and never strands a frame — a chain
/// worth stating because it is not local:
/// - A sibling on another site is already refused by `is_rendered_for`, so `None`
///   there changes nothing.
/// - A same-site sibling shares this exact cache entry with the sender, which the
///   sender resolved a scan from moments ago, so it is present.
/// - If a re-download replaced that entry with one carrying no sweep for the
///   product, the sibling's own dispatch retires the frame (`render_failed`) rather
///   than waiting on a broadcast.
/// - The one thing that empties the cache under a live loop is `clear_all`, reached
///   only from `SwitchRadarSite`, which deactivates every affected loop in the same
///   pass. **A second caller of `clear_all` would break that**, and would have to
///   re-check this.
fn own_sweep(
    loop_mgr: &crate::loop_downloads::LoopDownloadManager,
    ls: &rustdar_egui::pane::LoopPlaybackState,
    timestamp: chrono::NaiveDateTime,
    product: rustdar_radar::types::RadarProduct,
    elevation: f32,
) -> Option<f32> {
    let scan = loop_mgr.get_cached(&ls.site, &timestamp)?;
    rustdar_radar::render::find_closest_elevation(scan, product, elevation)
}

/// The sweep pair for offering `rr`'s finished image to the loop `ls`.
///
/// Both halves are assembled here rather than at the call site so the receiver's
/// half cannot be filled in from the response. `rr.snapped` is the sender's answer
/// and is already the other half of the comparison; using it for `own` as well
/// would make [`BroadcastSweep::agrees`] compare a value to itself and accept
/// unconditionally — the sweep term would still be there, still be read, and mean
/// nothing.
fn broadcast_sweep(
    loop_mgr: &crate::loop_downloads::LoopDownloadManager,
    ls: &rustdar_egui::pane::LoopPlaybackState,
    rr: &crate::channels::LoopRenderResponse,
) -> BroadcastSweep {
    BroadcastSweep {
        rendered: rr.snapped,
        own: own_sweep(loop_mgr, ls, rr.timestamp, rr.target.product, rr.target.elevation),
    }
}

/// Whether every frame `ls` intends to render has settled, given what has
/// downloaded.
///
/// The "has it downloaded" question is asked about the loop's own site. Answered
/// site-blind, another site's scan at the same timestamp counts as this frame's
/// data, and the loop is promoted to `Ready` over frames that will never render.
fn loop_batch_settled(
    loop_mgr: &crate::loop_downloads::LoopDownloadManager,
    ls: &rustdar_egui::pane::LoopPlaybackState,
    budget: usize,
) -> bool {
    // Not merely "nothing in flight this instant": the render budget is shared with
    // static pane renders, so part of a batch can be starved and not yet spawned.
    ls.render_set_settled(budget, |f| loop_mgr.is_cached(&ls.site, &f.timestamp))
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
/// `snapped` is compared as well, and `frame_accepting_broadcast` compares it too — via
/// [`rustdar_egui::pane::BroadcastSweep`] — so both halves of the promise weigh the same
/// thing. They must stay that way. The sweep is not implied by the target: the target
/// carries the *selected* elevation, and each scan snaps that to whatever sweep it
/// carries. If acceptance stopped checking it, a suppressed pane could be handed a
/// differently-snapped image, have its own in-flight render dropped as redundant, and
/// keep the wrong sweep permanently.
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
    use crate::loop_downloads::LoopDownloadManager;
    use rustdar_radar::archive::Identifier;
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
    };
    use rustdar_egui::pane::{LoopFrame, LoopPhase, LoopPlaybackState};
    use rustdar_radar::sites::RadarSite;
    use rustdar_radar::types::RadarProduct;

    /// `minute` minutes past midnight, and so still ordered past the hour — long
    /// listings run to hundreds of scans.
    fn ts(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            + chrono::Duration::minutes(minute as i64)
    }

    fn target(site: &str, elevation: f32) -> RenderTarget {
        RenderTarget::new(site, RadarProduct::Reflectivity, elevation)
    }

    fn identifier(name: &str) -> Identifier {
        Identifier::new(name.to_string())
    }

    /// A scan whose only sweeps sit at `elevations`, each carrying reflectivity.
    ///
    /// Real data, not a stand-in: `find_closest_elevation` walks the sweeps and asks
    /// each radial for the product's moment, so a scan without one answers `None` for
    /// every selection and the sweep tests would pass vacuously.
    fn scan_with_sweeps(elevations: &[f32]) -> Arc<Scan> {
        let sweeps = elevations
            .iter()
            .enumerate()
            .map(|(i, &elevation)| {
                let radial = Radial::new(
                    0,
                    0,
                    0.0,
                    1.0,
                    RadialStatus::ElevationStart,
                    i as u8 + 1,
                    elevation,
                    Some(MomentData::from_fixed_point(1, 0, 250, 8, 2.0, 66.0, vec![0])),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                Sweep::new(i as u8 + 1, vec![radial])
            })
            .collect();
        Arc::new(Scan::new(
            VolumeCoveragePattern::new(
                212, 0, 0.5, PulseWidth::Short, false, 0, false, 0, false, false, 0, false, false,
                Vec::new(),
            ),
            sweeps,
        ))
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

    /// A successful render result for `timestamp` at `target`.
    ///
    /// The coordinates are KTLX's real ones, which `loop_on` deliberately does *not*
    /// use — its loops are built at a round 35.0/-97.0. Anything that placed an
    /// image from a loop's own geometry rather than from the response would produce
    /// those round numbers instead.
    fn response(
        timestamp: chrono::NaiveDateTime,
        target: RenderTarget,
    ) -> crate::channels::LoopRenderResponse {
        crate::channels::LoopRenderResponse {
            pane_idx: 0,
            timestamp,
            target,
            snapped: 0.5,
            site_lat: 35.33,
            site_lon: -97.27,
            // `Some`, not `None`: a response carrying no image is retired as
            // `render_failed`, so `None` is the *failure* fixture and has to be
            // asked for deliberately. The pixels never matter here — every seam
            // under test reads the metadata — so a 1x1 image stands in for a frame.
            image: Some(egui::ColorImage::filled([1, 1], egui::Color32::WHITE)),
            max_range_km: 100.0,
        }
    }

    fn dummy_texture(ctx: &egui::Context) -> egui::TextureHandle {
        let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
        ctx.load_texture("test", image, egui::TextureOptions::NEAREST)
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

    /// The coupling this file's `render_already_queued` docs describe, tested where
    /// both halves are in scope. Suppressing a pane's render is a promise that the
    /// queued render's result will be handed to it, so the two must agree for every
    /// sweep — including when the receiver's own scan snaps the selection somewhere
    /// else. A sweep-blind acceptance breaks it in the dangerous direction: not
    /// suppressed (so the pane renders its own) yet accepted (so that render is
    /// dropped as redundant and an image of the wrong tilt stays put).
    #[test]
    fn suppression_and_acceptance_weigh_the_same_sweep() {
        let ctx = egui::Context::default();
        let receiver = loop_on(&ctx, "KTLX", &[]);
        let want = receiver.rendered_for.clone().expect("target adopted");
        // A sibling's render of the 0.48° sweep, queued this pass.
        let q = vec![queued(target("KTLX", 0.5), ts(0), 0.48)];

        for own in [0.48, 0.485, 1.4] {
            let suppressed = render_already_queued(&q, ts(0), &want, own);
            let accepted = receiver
                .frame_accepting_broadcast(
                    ts(0),
                    &want,
                    BroadcastSweep { rendered: 0.48, own: Some(own) },
                )
                .is_some();
            assert_eq!(
                suppressed, accepted,
                "own sweep {own}: suppressed={suppressed} but accepted={accepted}"
            );
        }

        // Not the trivial agreement of "both always refuse".
        assert!(render_already_queued(&q, ts(0), &want, 0.48));
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

    /// The blocking defect. A scan listing cannot be cancelled, so one requested
    /// before a site switch lands after the loop has been rebuilt for the new site.
    /// Taking it puts the old radar's timestamps in the frame list and the old
    /// radar's identifiers in the download queue — which are then labelled with the
    /// *new* site, cached under it, and rendered with its geometry. Nothing
    /// downstream can see it, and because the download filter treats that key as
    /// satisfied, the real scans that would correct it are discarded on arrival.
    #[test]
    fn a_listing_for_the_site_the_loop_left_is_refused() {
        let ctx = egui::Context::default();
        let mut koun = loop_on(&ctx, "KOUN", &[]);
        koun.frames.clear();
        let stale = vec![(ts(0), identifier("KTLX20240101_000000_V06"))];

        assert!(
            accept_scan_listing(&mut koun, "KTLX", stale).is_none(),
            "a KTLX listing is not this KOUN loop's frame list"
        );
        assert!(koun.frames.is_empty(), "and left no frames behind");

        // The loop's own listing is taken.
        let live = vec![(ts(0), identifier("KOUN20240101_000000_V06"))];
        let pending = accept_scan_listing(&mut koun, "KOUN", live).expect("its own listing");
        assert_eq!(pending.site, "KOUN", "the queue carries the site it was listed for");
        assert_eq!(pending.queue.len(), 1);
        assert_eq!(koun.frames.len(), 1);
    }

    /// A listing that arrives after the loop was switched off has nothing to fill.
    #[test]
    fn a_listing_for_an_inactive_loop_is_refused() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        ls.phase = LoopPhase::Inactive;

        let scans = vec![(ts(0), identifier("KTLX20240101_000000_V06"))];
        assert!(accept_scan_listing(&mut ls, "KTLX", scans).is_none());
    }

    /// The frame list and the download queue are the two halves of one plan: every
    /// frame must have a download queued, or it never settles and the loop hangs in
    /// `Rendering`. That has to survive the sampling that caps long listings.
    ///
    /// Taking the listing also has to *advance* the phase. This is the one fixture
    /// that starts where a real loop starts — `FetchingScanList`, set by
    /// `new_for_loop` and left there until its listing lands — so a missing advance
    /// reads as a loop still fetching rather than as a value already in place.
    /// Left in `FetchingScanList`, `is_fetching()` never goes false: the pane keeps
    /// its "fetching" label and keeps asking for continuous repaints forever.
    #[test]
    fn the_frame_list_and_the_download_queue_describe_the_same_scans() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        ls.phase = LoopPhase::FetchingScanList;
        assert!(ls.is_fetching(), "precondition: a loop awaiting its listing");

        let scans: Vec<_> = (0..(MAX_LOOP_FRAMES as u32 + 40))
            .map(|i| (ts(i), identifier(&format!("KTLX2024010{}_V06", i))))
            .collect();

        let pending = accept_scan_listing(&mut ls, "KTLX", scans).expect("accepted");

        assert_eq!(pending.queue.len(), MAX_LOOP_FRAMES, "capped");
        assert_eq!(
            ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
            pending.queue.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            "the sampled set is the frame list, frame for frame"
        );
        assert_eq!(ls.current_frame, ls.frames.len() - 1, "playback starts at the newest");
        assert_eq!(ls.phase, LoopPhase::Rendering);
        assert!(!ls.is_fetching(), "and the loop has stopped reading as fetching");
    }

    /// The cap has to *sample* the window, not truncate it. Taking the first
    /// `MAX_LOOP_FRAMES` or the last `MAX_LOOP_FRAMES` satisfies the cap and the
    /// frames-vs-queue agreement above equally well, and gives a loop that animates
    /// only the oldest or the newest slice of the lookback the user asked for —
    /// which plays smoothly and looks entirely correct.
    #[test]
    fn a_long_listing_is_sampled_evenly_across_its_whole_span() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        // Several times the cap, and not a multiple of it, so no exact stride
        // exists and the endpoints still have to be deliberate.
        let total = MAX_LOOP_FRAMES * 3 + 7;
        let scans: Vec<_> = (0..total as u32)
            .map(|i| (ts(i), identifier(&format!("KTLX2024010{}_V06", i))))
            .collect();

        accept_scan_listing(&mut ls, "KTLX", scans).expect("accepted");

        // `ts` is one minute per listing position, so a frame's minute *is* the
        // position it was sampled from, and the gaps below are index strides.
        let picked: Vec<i64> = ls
            .frames
            .iter()
            .map(|f| (f.timestamp - ts(0)).num_minutes())
            .collect();

        assert_eq!(picked.len(), MAX_LOOP_FRAMES);
        assert_eq!(picked[0], 0, "the oldest scan in the window is kept");
        assert_eq!(
            picked[MAX_LOOP_FRAMES - 1],
            total as i64 - 1,
            "and the newest, or the loop stops short of the scan the pane is showing"
        );

        let strides: Vec<i64> = picked.windows(2).map(|w| w[1] - w[0]).collect();
        let min = *strides.iter().min().expect("more than one frame");
        let max = *strides.iter().max().unwrap();
        assert!(min > 0, "strictly increasing, so no scan is sampled twice");
        assert!(
            max - min <= 1,
            "strides ran {min}..={max}; the sample must be evenly spaced, or the \
             loop covers only part of its own lookback window"
        );
    }

    /// The coordinates an image is placed at come off the response — the ones the
    /// renderer was actually handed — never off the loop receiving it.
    ///
    /// In production the two agree, but only via a coupling that lives in another
    /// type: `site_lat`/`site_lon` move only in `new_for_loop`, which also clears
    /// `rendered_for`, so a site change makes the target check reject the result
    /// before any coordinate is read. That is an argument, not a guarantee, it is
    /// invisible at the point of use, and it has to be re-made for every sibling
    /// pane the broadcast hands the same texture to. Carrying the values retires the
    /// argument; this test retires the way back to it.
    #[test]
    fn a_rendered_frame_is_placed_where_the_render_actually_drew_it() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        ls.frames[1].render_in_flight = true;
        let mut rr = response(ts(1), ls.rendered_for.clone().expect("target adopted"));

        assert_ne!(rr.site_lat, ls.site_lat, "precondition: the two sources differ");
        assert_ne!(rr.site_lon, ls.site_lon);

        let texture = accept_render_result(&mut ls, &mut rr, |_| dummy_texture(&ctx))
            .expect("the loop is awaiting this result");

        let image = ls.frames[1].texture.as_ref().expect("the frame was filled");
        assert_eq!(image.lat, rr.site_lat, "the latitude the image was projected around");
        assert_eq!(image.lon, rr.site_lon);
        assert_eq!(image.max_range_km, rr.max_range_km);
        assert!(!ls.frames[1].render_in_flight, "and the frame is no longer in flight");

        // The same image, described identically, is what the broadcast hands on — so
        // a sibling taking it is told where it was drawn rather than assuming.
        let broadcast = rendered_image(&rr, &texture);
        assert_eq!((broadcast.lat, broadcast.lon), (image.lat, image.lon));
    }

    /// A result the loop has retargeted away from is refused, and refusing it must
    /// cost nothing: the upload is the expensive half and must not run for an image
    /// that is about to be dropped.
    #[test]
    fn a_refused_result_is_never_uploaded() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        // In flight, so only the target can be what refuses this.
        ls.frames[1].render_in_flight = true;
        let mut stale = response(ts(1), target("KTLX", 2.4));

        let mut uploads = 0;
        let placed = accept_render_result(&mut ls, &mut stale, |_| {
            uploads += 1;
            dummy_texture(&ctx)
        });

        assert!(placed.is_none(), "a result for another elevation is not this loop's");
        assert_eq!(uploads, 0, "and nothing was uploaded for it");
        assert!(ls.frames[1].texture.is_none());
        assert!(stale.image.is_some(), "and its pixels were not taken off the response");
    }

    /// No image means the render found no matching sweep. The frame is retired
    /// rather than left in flight, or the dispatcher retries it forever and readiness
    /// never stops waiting on it.
    #[test]
    fn a_failed_render_retires_its_frame_without_a_texture() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        ls.frames[1].render_in_flight = true;
        let mut failed = crate::channels::LoopRenderResponse {
            image: None,
            ..response(ts(1), ls.rendered_for.clone().expect("target adopted"))
        };

        let mut uploads = 0;
        let placed = accept_render_result(&mut ls, &mut failed, |_| {
            uploads += 1;
            dummy_texture(&ctx)
        });

        assert!(placed.is_none());
        assert_eq!(uploads, 0, "a failed render uploads nothing");
        assert!(ls.frames[1].render_failed, "the frame is retired");
        assert!(!ls.frames[1].render_in_flight, "and released");
        assert!(ls.frames[1].texture.is_none());
    }

    /// A finished download is filed under the site it was fetched from, which the
    /// response carries. The requesting pane is not consulted — its loop may have
    /// been rebuilt for another site while the download ran, and filing under that
    /// site is exactly the corruption this key exists to prevent.
    #[test]
    fn a_download_is_cached_under_the_site_it_came_from() {
        let mut mgr = LoopDownloadManager::new();
        let scan = scan_with_sweeps(&[0.5]);
        mgr.mark_in_flight("KTLX", ts(0));

        apply_completed_download(&mut mgr, crate::channels::LoopScanDownloadResponse {
            pane_idx: 0,
            site: "KTLX".to_string(),
            timestamp: ts(0),
            scan: Some(Arc::clone(&scan)),
        });

        assert!(Arc::ptr_eq(mgr.get_cached("KTLX", &ts(0)).expect("cached"), &scan));
        assert!(mgr.get_cached("KOUN", &ts(0)).is_none());
        assert!(!mgr.is_in_flight("KTLX", &ts(0)), "and its mark is cleared");
    }

    /// A failed download still clears the mark, or the timestamp is never retried.
    #[test]
    fn a_failed_download_clears_its_mark_and_caches_nothing() {
        let mut mgr = LoopDownloadManager::new();
        mgr.mark_in_flight("KTLX", ts(0));

        apply_completed_download(&mut mgr, crate::channels::LoopScanDownloadResponse {
            pane_idx: 0,
            site: "KTLX".to_string(),
            timestamp: ts(0),
            scan: None,
        });

        assert!(!mgr.is_in_flight("KTLX", &ts(0)));
        assert!(!mgr.is_cached("KTLX", &ts(0)));
    }

    /// The scan a frame renders is named by the target it is rendered for, because
    /// that is where the geometry came from.
    #[test]
    fn a_frames_scan_is_looked_up_under_its_targets_site() {
        let mut mgr = LoopDownloadManager::new();
        let ktlx = scan_with_sweeps(&[0.5]);
        mgr.cache_scan("KTLX", ts(0), Arc::clone(&ktlx));

        let found = frame_scan(&mgr, &target("KTLX", 0.5), ts(0)).expect("KTLX's own scan");
        assert!(Arc::ptr_eq(found, &ktlx));
        assert!(
            frame_scan(&mgr, &target("KOUN", 0.5), ts(0)).is_none(),
            "a KOUN loop must not render KTLX's scan"
        );
    }

    /// The sharpest half of the broadcast check: the receiver's sweep has to be
    /// resolved from the receiver's *own* scan. Answered with the sender's snapped
    /// angle it would compare a value to itself, agree unconditionally, and the
    /// sweep term would be decorative.
    #[test]
    fn the_receivers_sweep_comes_from_the_receivers_own_scan() {
        let ctx = egui::Context::default();
        let mut mgr = LoopDownloadManager::new();
        // One timestamp, two sites, two different sweep sets — which is the whole
        // reason two loops can disagree about what a selection resolves to.
        mgr.cache_scan("KTLX", ts(0), scan_with_sweeps(&[0.5, 1.5]));
        mgr.cache_scan("KOUN", ts(0), scan_with_sweeps(&[1.4]));

        let ktlx = loop_on(&ctx, "KTLX", &[]);
        let koun = loop_on(&ctx, "KOUN", &[]);

        assert_eq!(
            own_sweep(&mgr, &ktlx, ts(0), RadarProduct::Reflectivity, 0.5),
            Some(0.5),
            "KTLX's scan carries the selected sweep"
        );
        assert_eq!(
            own_sweep(&mgr, &koun, ts(0), RadarProduct::Reflectivity, 0.5),
            Some(1.4),
            "KOUN's own scan snaps the same selection somewhere else"
        );
    }

    /// And the pair the response path actually builds. Both halves are pinned to
    /// values nothing else in the call could supply:
    ///
    /// - `rendered` is the *snapped* sweep off the response, never the selection the
    ///   target carries. Here the two are 1.4 and 0.5, so a `rendered` filled in from
    ///   `target.elevation` reads as the wrong tilt rather than as the same number.
    /// - `own` is resolved from the receiver's own scan against the *selection*. Fed
    ///   the sender's snapped angle instead it would agree with itself, and the sweep
    ///   test would pass for every image regardless of the tilt it depicts. KOUN's
    ///   scan carries both angles precisely so that substitution changes the answer.
    #[test]
    fn a_broadcast_sweep_pairs_the_senders_image_with_the_receivers_own_scan() {
        let ctx = egui::Context::default();
        let mut mgr = LoopDownloadManager::new();
        // One timestamp, two sites. KOUN's volume carries the selected 0.5° sweep and
        // a 1.4°; KTLX's is a partial volume whose only reflectivity sweep is the
        // 1.4°, so the same 0.5° selection snaps to a different tilt on each.
        mgr.cache_scan("KOUN", ts(0), scan_with_sweeps(&[0.5, 1.4]));
        mgr.cache_scan("KTLX", ts(0), scan_with_sweeps(&[1.4]));
        let koun = loop_on(&ctx, "KOUN", &[]);

        // A finished render of the 1.4° sweep, for a 0.5° selection. The target's
        // site is not read here on purpose — `own_sweep` looks the scan up under the
        // *receiving* loop's site, which is the whole point — so one response can be
        // offered to both loops below.
        //
        // It carries an image (`response`'s default, and see the note there): a
        // response with none is retired as `render_failed` before the broadcast loop
        // is reached, so a `None` fixture would put `broadcast_sweep` in a state the
        // response path never hands it.
        let rr = crate::channels::LoopRenderResponse {
            snapped: 1.4,
            ..response(ts(0), target("KOUN", 0.5))
        };

        let sweep = broadcast_sweep(&mgr, &koun, &rr);

        assert_eq!(sweep.rendered, 1.4, "the tilt the image depicts — not the 0.5 selection");
        assert_eq!(sweep.own, Some(0.5), "what this loop's own scan resolves that selection to");
        assert!(!sweep.agrees(), "so the image must not be handed over");

        // Same call, a receiver whose scan does snap where the image was rendered.
        let ktlx = loop_on(&ctx, "KTLX", &[]);
        let sweep = broadcast_sweep(&mgr, &ktlx, &rr);
        assert_eq!(sweep.own, Some(1.4));
        assert!(sweep.agrees(), "and this one takes it");
    }

    /// No scan, or no sweep for the product, means the receiver cannot check the
    /// image — which refuses the broadcast rather than accepting on faith.
    #[test]
    fn a_receiver_with_nothing_to_compare_reports_no_sweep() {
        let ctx = egui::Context::default();
        let mut mgr = LoopDownloadManager::new();
        let ktlx = loop_on(&ctx, "KTLX", &[]);

        assert_eq!(
            own_sweep(&mgr, &ktlx, ts(0), RadarProduct::Reflectivity, 0.5),
            None,
            "nothing downloaded for this frame yet"
        );

        mgr.cache_scan("KTLX", ts(0), scan_with_sweeps(&[0.5]));
        assert_eq!(
            own_sweep(&mgr, &ktlx, ts(0), RadarProduct::Velocity, 0.5),
            None,
            "the scan carries no sweep for this product"
        );
    }

    /// Readiness asks "has this frame's scan downloaded" about the loop's own site.
    /// Site-blind, another radar's scan at the same timestamp answers yes, and the
    /// loop is promoted over frames that will never render.
    #[test]
    fn readiness_counts_only_this_loops_own_downloads() {
        let ctx = egui::Context::default();
        let mut mgr = LoopDownloadManager::new();
        let koun = loop_on(&ctx, "KOUN", &[]);
        // Every frame blank, and only *KTLX* scans downloaded.
        for i in 0..3 {
            mgr.cache_scan("KTLX", ts(i), scan_with_sweeps(&[0.5]));
        }

        assert!(
            loop_batch_settled(&mgr, &koun, MAX_LOOP_RENDER_BUDGET),
            "precondition: with no scan of its own, a blank frame is not waiting on a render"
        );

        // Now KOUN's own scans arrive: the same blank frames become renders that
        // are owed, and readiness must wait for them.
        for i in 0..3 {
            mgr.cache_scan("KOUN", ts(i), scan_with_sweeps(&[0.5]));
        }
        assert!(
            !loop_batch_settled(&mgr, &koun, MAX_LOOP_RENDER_BUDGET),
            "downloaded but unrendered frames must hold the loop out of Ready"
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

#[cfg(test)]
mod frame_order_tests {
    use super::{SurfaceStatus, finish_then_acquire};

    /// Drive one frame whose surface acquisition fails.
    ///
    /// `status` ignores the finished pass it is handed; production uses that
    /// argument to make acquiring without one a compile error.
    fn skipped_frame(ctx: &egui::Context, status: fn(&egui::FullOutput) -> SurfaceStatus) {
        ctx.begin_pass(egui::RawInput::default());
        let (_finished, _status) = finish_then_acquire(|| ctx.end_pass(), status);
    }

    /// A frame that cannot acquire a surface must still end egui's pass.
    ///
    /// `cumulative_pass_nr` is incremented by `Context::end_pass` and by nothing
    /// else, so it counts passes that actually completed. That is what tells
    /// "the pass ended and then the frame was abandoned" apart from "the frame
    /// was abandoned with the pass still open" — and only the second one leaks.
    #[test]
    fn a_lost_surface_still_ends_the_egui_pass() {
        let ctx = egui::Context::default();

        ctx.begin_pass(egui::RawInput::default());
        assert_eq!(ctx.cumulative_pass_nr(), 0, "pass is open, not yet ended");

        let (_finished, status) =
            finish_then_acquire(|| ctx.end_pass(), |_| SurfaceStatus::Lost);

        assert!(matches!(status, SurfaceStatus::Lost));
        assert_eq!(
            ctx.cumulative_pass_nr(),
            1,
            "the pass must be ended even though the surface was lost"
        );
    }

    /// Repeated surface failures must not accumulate open passes.
    #[test]
    fn every_skipped_frame_completes_its_pass() {
        let ctx = egui::Context::default();
        const FRAMES: u64 = 5;

        for _ in 0..FRAMES {
            skipped_frame(&ctx, |_| SurfaceStatus::Skip);
        }

        assert_eq!(
            ctx.cumulative_pass_nr(),
            FRAMES,
            "each skipped frame should have completed exactly one pass"
        );
    }

    /// The user-visible half of the leak.
    ///
    /// egui only consumes a pending zoom/scale change when it believes it is on
    /// the outermost viewport, and it stops believing that the moment one pass
    /// is left open — `begin_pass` pushes onto the viewport stack and only
    /// `end_pass` pops it. So a window moved to a different-DPI monitor after
    /// any skipped frame would never rescale again.
    ///
    /// This asserts on a value the production path actually reads back:
    /// `end_pass_and_upload` tessellates at `ctx.pixels_per_point()`.
    #[test]
    fn scale_changes_still_apply_after_frames_the_surface_refused() {
        let ctx = egui::Context::default();

        for _ in 0..3 {
            skipped_frame(&ctx, |_| SurfaceStatus::Skip);
        }

        ctx.set_pixels_per_point(2.0);
        ctx.begin_pass(egui::RawInput::default());
        let applied = ctx.pixels_per_point();
        let _ = ctx.end_pass();

        assert_eq!(
            applied, 2.0,
            "a scale set after skipped frames must still reach the next pass"
        );
    }
}
