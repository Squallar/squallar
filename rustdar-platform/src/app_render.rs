use egui_wgpu::{ScreenDescriptor, wgpu};
use std::sync::Arc;
use rustdar_egui::actions::GuiAction;

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
                state.surface_config.width as f32 / window_size.width as f32;
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
        self.dispatch_pane_renders();

        (screen_descriptor, gui_action)
    }

    /// Poll for completed background render results and upload textures.
    fn poll_render_results(&mut self) {
        let ctx = self.state.as_ref().unwrap().egui_renderer.context();
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

            // Save the old texture to be cleaned up after this frame completes
            if let Some(old_img) = self.gui.take_radar_image_for_pane(rr.pane_idx) {
                self.old_textures.push(old_img.texture);
            }

            self.texture_counter += 1;
            let color_image =
                egui::ColorImage::from_rgba_unmultiplied([1800, 1800], &rr.image_data);
            let texture_name = format!("radar_image_{}", self.texture_counter);
            let texture = ctx.load_texture(
                texture_name,
                color_image,
                egui::TextureOptions::NEAREST,
            );

            // Cache the raw image data for fast restore after suspend/resume
            self.render.pane_render[rr.pane_idx].cached_render = Some((
                rr.image_data,
                rr.max_range_km,
                rr.value_data.clone(),
                rr.product,
                rr.elevation,
            ));

            if let Some(scan_info) = self.gui.get_scan_info() {
                self.gui.set_radar_image_for_pane(
                    rr.pane_idx,
                    texture,
                    scan_info.site.lat,
                    scan_info.site.lon,
                    rr.max_range_km,
                    rr.value_data,
                );
            }

            self.render.pane_render[rr.pane_idx].last_rendered = Some((rr.product, rr.elevation));
        }
    }

    /// Poll for completed Level III fetch results and update scan info.
    fn poll_level3_results(&mut self) {
        let Ok(l3_resp) = self.channels.level3_receiver.try_recv() else {
            return;
        };

        if self.render.is_fetch_stale(l3_resp.generation) {
            log::debug!("Discarding stale Level III result (gen {} < current {})", l3_resp.generation, self.render.fetch_generation);
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
        self.render.level3_data.insert((l3_resp.product, l3_resp.tilt_code), Arc::new(message));

        // Trigger a re-render for any pane viewing this product
        for (idx, prs) in self.render.pane_render.iter_mut().enumerate() {
            if self.gui.get_rendering_params_for_pane(idx).map(|(p, _)| p) == Some(l3_resp.product) {
                prs.last_rendered = None;
            }
        }

        // Add Level III products to the scan info's available list
        let Some(scan_info) = self.gui.get_scan_info() else {
            return;
        };
        let mut info = scan_info.clone();
        if !info.available_products.contains(&l3_resp.product) {
            info.available_products.push(l3_resp.product);
            info.available_products.sort_by_key(|p| p.sort_order());
            info.status = format!(
                "Loaded {} products: {}",
                info.available_products.len(),
                info.available_products.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")
            );
        }
        // Register the actual elevation angle from the PDB
        let elevations = info.product_elevations.entry(l3_resp.product).or_default();
        let rounded_elev = (elevation * 10.0).round() / 10.0;
        if !elevations.iter().any(|e| (e - rounded_elev).abs() < 0.05) {
            elevations.push(rounded_elev);
            elevations.sort_by(|a, b| a.partial_cmp(b).unwrap());
        }
        self.gui.set_scan_info(info);
    }

    /// Poll for completed overlay rasterization results and upload textures.
    fn poll_overlay_render_results(&mut self) {
        use rustdar_egui::overlay_cache::OverlayTextureData;
        use crate::channels::OverlayType;

        let ctx = self.state.as_ref().unwrap().egui_renderer.context();
        while let Ok(resp) = self.channels.overlay_render_receiver.try_recv() {
            let Some(pane) = self.gui.pane_mut(resp.pane_idx) else {
                continue;
            };

            let cache = match resp.overlay_type {
                OverlayType::SpcOutlook(..) => &mut pane.spc_overlay_texture,
                OverlayType::SpcDiscussions => &mut pane.spc_md_texture,
                OverlayType::NwsAlerts => &mut pane.nws_alert_texture,
            };

            cache.render_in_flight = false;

            // Discard stale results
            if resp.generation < cache.render_generation {
                continue;
            }

            self.texture_counter += 1;
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [resp.width as usize, resp.height as usize],
                &resp.image_data,
            );
            let tex_name = format!("overlay_{}_{}", resp.pane_idx, self.texture_counter);

            // Save old texture for deferred cleanup
            if let Some(old) = cache.current.take() {
                self.old_textures.push(old.texture);
            }

            let texture = ctx.load_texture(tex_name, color_image, egui::TextureOptions::LINEAR);
            cache.current = Some(OverlayTextureData {
                texture,
                geo_bounds: resp.geo_bounds,
                data_generation: resp.generation,
                render_zoom: resp.zoom,
                width: resp.width,
                height: resp.height,
            });
        }
    }

    /// Check all panes for needed background renders and spawn render threads.
    fn dispatch_pane_renders(&mut self) {
        for pane_idx in 0..self.gui.pane_count() {
            if let Some((product, elevation)) = self.gui.get_rendering_params_for_pane(pane_idx) {
                let prs = &self.render.pane_render[pane_idx];
                let needs_render = prs
                    .last_rendered
                    .map(|(last_prod, last_elev)| {
                        last_prod != product || (last_elev - elevation).abs() > 0.01
                    })
                    .unwrap_or(true);

                if needs_render && !prs.render_in_flight {
                    if product.is_level3() {
                        if let Some(scan_info) = self.gui.get_scan_info() {
                            self.render.try_spawn_level3_render(
                                pane_idx,
                                product,
                                elevation,
                                scan_info.site.lat,
                                scan_info.site.lon,
                                self.channels.render_sender.clone(),
                                self.window.clone(),
                            );
                        }
                    } else if let Some(data) = &self.scan_data {
                        if let Some(scan_info) = self.gui.get_scan_info() {
                            self.render.spawn_level2_render(
                                pane_idx,
                                product,
                                elevation,
                                scan_info.site.lat,
                                scan_info.site.lon,
                                Arc::clone(data),
                                self.channels.render_sender.clone(),
                                self.window.clone(),
                            );
                        }
                    }
                }
            } else if pane_idx < self.render.pane_render.len() {
                // No rendering params for this pane, clear its image
                self.gui.clear_radar_image_for_pane(pane_idx);
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
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let Some(scan_info) = self.gui.get_scan_info().cloned() else {
            return;
        };

        for pane_idx in 0..self.render.pane_render.len().min(self.gui.pane_count()) {
            let Some((ref image_data, max_range_km, ref value_data, product, elevation)) =
                self.render.pane_render[pane_idx].cached_render
            else {
                continue;
            };

            log::info!(
                "Restoring cached radar image for pane {} ({:?} at {:.1}°) from memory",
                pane_idx,
                product,
                elevation
            );

            self.texture_counter += 1;
            let ctx = state.egui_renderer.context();
            let color_image = egui::ColorImage::from_rgba_unmultiplied([1800, 1800], image_data);
            let texture_name = format!("radar_image_{}", self.texture_counter);
            let texture = ctx.load_texture(texture_name, color_image, egui::TextureOptions::NEAREST);

            self.gui.set_radar_image_for_pane(
                pane_idx,
                texture,
                scan_info.site.lat,
                scan_info.site.lon,
                max_range_km,
                value_data.clone(),
            );
            self.render.pane_render[pane_idx].last_rendered = Some((product, elevation));
        }
    }

    /// Try to acquire the next surface texture for rendering.
    /// Returns `None` if the surface is temporarily unavailable (e.g. during
    /// a display change).  Returns `Err(true)` via the second element when
    /// the surface is *lost* and the caller must recreate rendering state.
    fn get_surface_texture(surface: &wgpu::Surface) -> (Option<wgpu::SurfaceTexture>, bool) {
        match surface.get_current_texture() {
            Ok(texture) => (Some(texture), false),
            Err(wgpu::SurfaceError::Outdated) => {
                log::warn!("wgpu surface outdated, skipping frame");
                (None, false)
            }
            Err(wgpu::SurfaceError::Lost) => {
                log::warn!("wgpu surface lost (display change?), will recreate state");
                (None, true)
            }
            Err(err) => {
                log::error!("Surface error: {:?}", err);
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
}
