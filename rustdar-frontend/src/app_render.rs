use crate::constants::{
    DEFAULT_LOOP_SPEED_FPS, MAX_CONCURRENT_LOOP_DOWNLOADS, MAX_CONCURRENT_RENDERS, MAX_LOOP_FRAMES,
    MAX_LOOP_RENDER_BUDGET, MAX_LOOP_SPEED_FPS, MIN_LOOP_SPEED_FPS,
};
use crate::loop_downloads::{
    FramePlan, L3FrameState, LoopFrameData, PendingDownloads, PendingL3Pairings,
};
use crate::render_dispatch::CachedPaneRender;
use egui_wgpu::wgpu;
use rustdar_egui::actions::GuiAction;
use rustdar_egui::pane::{BroadcastSweep, ELEVATION_TOLERANCE, RenderTarget};
use rustdar_radar::types::IMAGE_SIZE;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

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

/// How long one loop frame is held on screen, for a stored playback speed.
///
/// The clamp is here rather than at the slider because this is the last point
/// before the value becomes a `Duration`, and `Duration::from_secs_f32` panics
/// on a negative, an infinity or a NaN — while `1.0 / 0.0` is an infinity, so a
/// stored zero panics too. The slider that normally writes `loop_speed_fps`
/// bounds an *edit*; a config load assigns the stored number as it stands. See
/// [`MIN_LOOP_SPEED_FPS`].
///
/// NaN is handled before the clamp, not by it: `f32::clamp` propagates NaN
/// rather than replacing it, so clamping alone would leave the panic in place
/// for the one input that reaches it by arithmetic rather than by editing.
fn loop_interval(fps: f32) -> std::time::Duration {
    let fps = if fps.is_finite() {
        fps.clamp(MIN_LOOP_SPEED_FPS, MAX_LOOP_SPEED_FPS)
    } else {
        DEFAULT_LOOP_SPEED_FPS
    };
    std::time::Duration::from_secs_f32(1.0 / fps)
}

impl super::App {
    /// Set up and run the egui UI pass.
    ///
    /// Returns the surface size in pixels and any GUI actions triggered. Only
    /// the size is returned: the scale the frame is laid out at is handed to
    /// egui here and read back off the context when the pass ends, so there is
    /// no second copy of it to drift.
    ///
    /// The scale handed to egui is the surface-to-window ratio, which matters on
    /// web, where the canvas backing store can differ from its CSS size. There is
    /// no second, application-level factor beside it: `AppState` used to carry a
    /// `scale_factor` that was initialised to 1.0 and never written, so the
    /// product it took part in was always just this ratio.
    ///
    /// OS display scaling is *not* included: egui-winit puts it on the raw input
    /// and egui applies it itself.
    ///
    /// # Why the pollers run before `Gui::ui`
    ///
    /// Everything they apply — a finished radar image, an overlay raster, a
    /// loop frame — is state the UI reads while it lays the frame out. Applied
    /// after the layout it misses the frame that was being built, and nothing
    /// asks for another one: the re-arm at the end of `handle_redraw` fires only
    /// for a render still in flight, for auto-poll, or for an active loop. So
    /// the *last* result of a batch, with auto-poll off, sat applied but
    /// unpresented until something unrelated — a mouse move — repainted.
    ///
    /// Polling first costs nothing. A poller needs `&mut self` and an
    /// `egui::Context`, and `Context::load_texture` neither needs a pass to be
    /// open nor cares that one is. The dispatchers move with them: they read
    /// the selection the *previous* frame left, which is what they did anyway
    /// for every frame the UI did not change it.
    pub(super) fn setup_egui_frame(&mut self) -> ([u32; 2], Vec<GuiAction>) {
        // Before the pass, because the cache it writes is read by everything
        // that rasterizes off-frame — see `App::resolve_theme`.
        let use_dark_theme = self.resolve_theme();

        // Open egui's pass and apply the theme.
        // Scoped so `state` is dropped before we call &mut self methods below.
        let size_in_pixels = {
            let state = self.state.as_mut().unwrap();
            let window = self.window.as_ref().unwrap();

            let window_size = window.inner_size();
            // The CSS-size-to-backing-store ratio, and nothing else.
            // `window.scale_factor()` is deliberately not folded in: egui
            // already has it from the raw input and multiplies it back on, using
            // the value for the pass being started rather than the one it
            // happened to hold beforehand.
            let zoom_factor = state.surface_config.width as f32 / window_size.width.max(1) as f32;

            // Start egui frame
            state.egui_renderer.begin_frame(window, zoom_factor);

            state.egui_renderer.apply_theme(use_dark_theme);

            [state.surface_config.width, state.surface_config.height]
        };

        // Clean up old textures from previous frame
        // This allows the GPU to finish using them before we drop them
        self.old_textures.clear();

        // Ensure pane_render vec matches gui pane count
        self.render.ensure_pane_count(self.gui.pane_count());

        // The frame's egui context, resolved once. The two passes below that
        // upload a plan-view texture are handed it rather than each reaching
        // through `self.state` for a copy of their own: one `unwrap` on the
        // renderer per frame instead of three, and it is what lets both of them
        // be driven by a test against a bare `egui::Context`, which is all
        // `Context::load_texture` has ever needed.
        let ctx = self.state.as_ref().unwrap().egui_renderer.context().clone();

        self.poll_render_results(&ctx);
        self.poll_section_results(&ctx);
        self.poll_level3_results();
        self.poll_overlay_render_results();
        self.poll_loop_scan_list_results();
        self.poll_loop_scan_download_results();
        self.poll_loop_l3_list_results();
        self.poll_loop_l3_fetch_results();
        self.poll_loop_render_results(&ctx);
        self.advance_loop_playback();
        self.dispatch_pane_renders(&ctx);
        self.dispatch_section_renders();
        self.dispatch_loop_renders();
        self.update_loop_readiness();

        // Last, so this frame is laid out over everything applied above.
        let gui_action = self.gui.ui(&ctx);

        (size_in_pixels, gui_action)
    }

    /// Poll for completed background render results and upload textures.
    fn poll_render_results(&mut self, ctx: &egui::Context) {
        while let Ok(rr) = self.channels.render_receiver.try_recv() {
            if rr.pane_idx < self.render.pane_render.len() {
                self.render.pane_render[rr.pane_idx].render_in_flight = false;
            }

            if self.render.is_render_stale(rr.generation) {
                log::debug!(
                    "Discarding stale render result (gen {} < current {})",
                    rr.generation,
                    self.render.render_generation
                );
                continue;
            }

            if rr.pane_idx >= self.gui.pane_count()
                || self
                    .gui
                    .get_rendering_params_for_pane(rr.pane_idx)
                    .is_none()
            {
                continue;
            }

            // A render that found no sweep has already done its one job above by
            // clearing `render_in_flight`; there is nothing to cache or draw.
            // The pane keeps whatever it was showing, which is what a missing
            // tilt should look like.
            let Some(rendered) = rr.rendered else {
                continue;
            };

            // Extract fields to avoid borrow issues
            let origin_pane = rr.pane_idx;
            let render_result = crate::render_dispatch::CachedPaneRender {
                image_data: rendered.image_data,
                max_range_km: rendered.max_range_km,
                value_data: rendered.value_data,
                product: rr.product,
                elevation: rr.elevation,
            };

            // Cache the render output for sharing with other panes on the same site
            let origin_site = self
                .gui
                .pane(origin_pane)
                .map(|p| p.site.clone())
                .unwrap_or_default();
            // `RenderView::PlanView` because this is the plan-view path and
            // only the plan-view path: `dispatch_pane_renders` starts no render
            // for a non-map pane, and `CachedRenderOutput` is an `IMAGE_SIZE`
            // square raster by construction. The axis exists so a section
            // cached later cannot be handed to this consumer — see
            // `RenderCacheKey`.
            self.render.cache_render(
                &origin_site,
                render_result.product,
                rustdar_radar::types::RenderView::PlanView,
                render_result.elevation,
                crate::render_dispatch::CachedRenderOutput {
                    image_data: Arc::clone(&render_result.image_data),
                    max_range_km: render_result.max_range_km,
                    value_data: Arc::clone(&render_result.value_data),
                },
            );

            // Apply to the originating pane — unless it stopped being a map
            // while this render was in flight. `dispatch_pane_renders` no longer
            // starts one for a non-map pane, but a conversion after dispatch is
            // a live race, and the result would land as a plan-view texture on
            // a pane that draws none. `render_in_flight` was already cleared
            // above, and `last_rendered` stays unset, so converting back
            // re-dispatches.
            if !self.gui.pane_has_no_plan_view(origin_pane) {
                self.apply_render_to_pane(ctx, origin_pane, &render_result);
            }

            // Broadcast to sibling panes that need the same site+product+elevation.
            //
            // The test is on site, product and elevation with **no view term**,
            // because nothing renders anything but a plan view yet: every
            // `RenderResponse` in the channel is a plan-view raster, so the
            // receiving pane's kind is the whole of the question. When a section
            // render exists it will also have to be keyed on the *result's* view
            // — a pane and a result can both be sections and still disagree
            // about which — and that arrives with `RenderCacheKey`'s view axis in
            // WP-G. Until then a view term here would compare a constant against
            // a constant.
            let pane_count = self.gui.pane_count();
            for other_idx in 0..pane_count {
                if other_idx == origin_pane {
                    continue;
                }
                if self.gui.pane_has_no_plan_view(other_idx) {
                    continue;
                }
                let matches_site = self
                    .gui
                    .pane(other_idx)
                    .is_some_and(|p| p.site == origin_site);
                if !matches_site {
                    continue;
                }
                let Some((other_product, other_elevation)) =
                    self.gui.get_rendering_params_for_pane(other_idx)
                else {
                    continue;
                };
                if other_product == render_result.product
                    && (other_elevation - render_result.elevation).abs() <= ELEVATION_TOLERANCE
                {
                    let needs = other_idx < self.render.pane_render.len()
                        && self.render.pane_render[other_idx]
                            .last_rendered
                            .map(|(lp, le)| {
                                lp != other_product
                                    || (le - other_elevation).abs() > ELEVATION_TOLERANCE
                            })
                            .unwrap_or(true);
                    if needs {
                        self.apply_render_to_pane(ctx, other_idx, &render_result);
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
        let texture = ctx.load_texture(texture_name, color_image, egui::TextureOptions::NEAREST);

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
        // Dropping this call is silent: the pane simply keeps whatever time it
        // was last stamped with, which reads as a current image of another
        // volume. The lookup and the assignment inside the callee are the
        // dispatcher's own tests' business; that this function *makes the call*
        // is `stamping_tests` below.
        self.render.stamp_pane_with_data_time(pane, render);
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
                // What these pixels are, travelling with them. Whichever
                // datasource produced them: this is the one assignment behind
                // `PaneState::stale_image_on_screen`, so a Level II and a
                // Level III image are described identically and neither can
                // stay on screen unlabelled after the selection moves.
                product: render.product,
                elevation: render.elevation,
            }),
            hit_map: None,
        });

        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].last_rendered =
                Some((render.product, render.elevation));
        }
    }

    /// Poll for completed Level III fetch results and update scan info.
    ///
    /// Drains, like every sibling poller. One Level II scan spawns a fetch per
    /// distinct AWIPS code, all landing within a few hundred milliseconds of each
    /// other, so taking one per frame turned the product picker into a list that
    /// fills in one entry per redraw, and stalled outright on the frame where no
    /// redraw follows.
    fn poll_level3_results(&mut self) {
        while let Ok(sounding) = self.channels.sounding_receiver.try_recv() {
            if self
                .render
                .is_fetch_stale(&sounding.site, sounding.generation)
            {
                continue;
            }
            // A failed fetch keeps the previous entry: a stale environment
            // beats none, and the TTL gate in `spawn_level3_fetches` retries
            // on the next poll precisely because nothing fresh landed here.
            let Some(heights) = sounding.heights else {
                log::warn!("Sounding fetch failed for {}", sounding.site);
                continue;
            };
            log::info!(
                "Env heights cached for {}: 0C {:.2} km, -20C {:.2} km MSL",
                sounding.site,
                heights.h0c_km_msl,
                heights.hm20c_km_msl
            );
            // Through the setter so hail panes drawn against the old pair —
            // including the "no pair yet, drew nothing" state a pane sits in
            // when it was selected before the first sounding landed — are
            // redrawn against the new one.
            if self
                .render
                .set_env_heights(&sounding.site, heights, &self.gui)
            {
                log::info!(
                    "Env heights moved for {}: hail renders dropped",
                    sounding.site
                );
            }
        }
        while let Ok(l3_resp) = self.channels.level3_receiver.try_recv() {
            if self
                .render
                .is_fetch_stale(&l3_resp.site, l3_resp.generation)
            {
                log::debug!(
                    "Discarding stale Level III result for {} (gen {})",
                    l3_resp.site,
                    l3_resp.generation
                );
                continue;
            }

            let fetched = match l3_resp.result {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("Level III {} fetch failed: {}", l3_resp.code, e);
                    continue;
                }
            };

            // Every product this object feeds. One object serves several — `DVL`
            // is VIL's field and VIL density's numerator — and the fetch names
            // only the code, so the products are derived here rather than
            // travelling with the response. Each of them gets the redraw and the
            // picker entry it would have got from its own fetch.
            let readers = rustdar_radar::types::RadarProduct::level3_readers(&l3_resp.code);
            let elevation = fetched.message.pdb.elevation_angle();
            // The age is logged, not just carried: `latest_key` falls back to the
            // previous UTC day, so a site down since yesterday delivers a product
            // up to ~48 h old and this is currently the only place that says so.
            // Surfacing it in the pane is what remains — see `ProductStamp`.
            log::info!(
                "Level III {} fetched successfully for {:?} (elevation={:.1}°, key={}, age={:?} min)",
                l3_resp.code,
                readers.iter().map(|p| p.name()).collect::<Vec<_>>(),
                elevation,
                fetched.stamp.key,
                fetched
                    .age(chrono::Utc::now().naive_utc())
                    .map(|a| a.num_minutes()),
            );
            self.render
                .cache_level3(l3_resp.code.clone(), l3_resp.site.clone(), fetched);

            // Trigger a re-render for panes on the same site showing anything this
            // object feeds.
            for (idx, prs) in self.render.pane_render.iter_mut().enumerate() {
                let pane_matches_site = self.gui.pane(idx).is_some_and(|p| p.site == l3_resp.site);
                if pane_matches_site
                    && self
                        .gui
                        .get_rendering_params_for_pane(idx)
                        .is_some_and(|(p, _)| readers.contains(&p))
                {
                    prs.last_rendered = None;
                }
            }

            // Add Level III products to the scan info for panes on this site
            for pane_idx in 0..self.gui.pane_count() {
                let pane_site = self
                    .gui
                    .pane(pane_idx)
                    .map(|p| p.site.clone())
                    .unwrap_or_default();
                if pane_site != l3_resp.site {
                    continue;
                }
                let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                    continue;
                };
                let mut info = scan_info.clone();
                let mut changed = false;
                for &product in &readers {
                    if !info.available_products.contains(&product) {
                        info.available_products.push(product);
                        info.available_products.sort_by_key(|p| p.sort_order());
                        info.status = format!(
                            "Loaded {} products: {}",
                            info.available_products.len(),
                            info.available_products
                                .iter()
                                .map(|p| p.name())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        changed = true;
                    }
                    // Register the actual elevation angle from the PDB.
                    let elevations = info.product_elevations.entry(product).or_default();
                    let rounded_elev = (elevation * 10.0).round() / 10.0;
                    if !elevations.iter().any(|e| (e - rounded_elev).abs() < 0.05) {
                        elevations.push(rounded_elev);
                        elevations.sort_by(|a, b| a.total_cmp(b));
                        changed = true;
                    }
                }
                if changed {
                    self.gui.set_scan_info_for_pane(pane_idx, info);
                }
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
    fn dispatch_pane_renders(&mut self, ctx: &egui::Context) {
        // Editing the vector changes nothing else about a pane, so the derived
        // storm-relative tilts have to be invalidated explicitly.
        let storm_motion = self.gui.storm_motion_override.sample();
        self.render.set_storm_motion_override(storm_motion);
        for pane_idx in 0..self.gui.pane_count() {
            // Ahead of the rendering-params branch, not inside it. A pane with
            // no plan view still has a product and an elevation selected —
            // they are flat fields — so it would take the `if` arm and buy a
            // full `IMAGE_SIZE` x `IMAGE_SIZE` RGBA image plus an equally large
            // `f32` value grid, per pane per selection change, that nothing
            // draws. Under the `else` arm it would instead have its radar
            // texture torn down, which is a wasted upload on the way back.
            // Skipping outright leaves whatever it had as a map pane in place,
            // so converting back to a map is instant and needs no re-render.
            if self.gui.pane_has_no_plan_view(pane_idx) {
                continue;
            }
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
                    let pane_site = self
                        .gui
                        .pane(pane_idx)
                        .map(|p| p.site.clone())
                        .unwrap_or_default();

                    // Check if another pane already rendered this site+product+elevation
                    // Plan view, and only plan view — see the matching
                    // `cache_render` above. A pane of another kind never
                    // reaches here.
                    if let Some(cached) = self.render.get_cached_render(
                        &pane_site,
                        product,
                        rustdar_radar::types::RenderView::PlanView,
                        elevation,
                    ) {
                        let render_result = crate::render_dispatch::CachedPaneRender {
                            image_data: Arc::clone(&cached.image_data),
                            max_range_km: cached.max_range_km,
                            value_data: Arc::clone(&cached.value_data),
                            product,
                            elevation,
                        };
                        log::info!(
                            "Reusing cached render for pane {}: {:?} at {:.1}°",
                            pane_idx,
                            product,
                            elevation
                        );
                        self.apply_render_to_pane(ctx, pane_idx, &render_result);
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
                        // The override reaches the render through
                        // `set_storm_motion_override` above, not as an argument
                        // here — one source for both the invalidation and the
                        // field that gets drawn.
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
                            &pane_site,
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
                let has_scan = self
                    .gui
                    .pane(pane_idx)
                    .is_some_and(|p| p.scan_info.is_some());
                if !has_scan && let Some(pane) = self.gui.pane_mut(pane_idx) {
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

    /// Cut a fresh cross-section for every section pane whose picture no longer
    /// matches what it is aimed at.
    ///
    /// # Staleness needs no help from any reset path
    ///
    /// The comparison is against a whole
    /// [`SectionTarget`](rustdar_egui::pane::SectionTarget) — site, volume time,
    /// moment and line — so *every* way a section can go stale is one
    /// comparison. A new volume for the site changes the time; a site switch
    /// changes the site; the product picker changes the moment; a redrawn line
    /// changes the line. No `reset_panes_for_*` arm has to remember section
    /// panes, which is exactly the kind of thing that gets remembered for one of
    /// the two reset paths and not the other.
    ///
    /// # Why a poll rather than an action fired on commit
    ///
    /// Only three of those four inputs are user gestures. The fourth — a new
    /// volume arriving — is not something the UI does, so an action pushed when
    /// a line is committed would cut the section once and then leave it showing
    /// a storm that had moved on, live, indefinitely. A poll against the target
    /// covers all four with one rule.
    ///
    /// It costs nothing per frame: the key is written when the job is
    /// *dispatched*, so a matching key is the ordinary state and the loop below
    /// falls straight through it.
    fn dispatch_section_renders(&mut self) {
        for pane_idx in 0..self.gui.pane_count() {
            let Some(target) = self.section_target_for_pane(pane_idx) else {
                continue;
            };
            let Some(pane) = self.gui.pane(pane_idx) else {
                continue;
            };
            let Some(section) = pane.cross_section() else {
                continue;
            };
            if section.rendered_for.as_ref() == Some(&target) {
                continue;
            }
            if self
                .render
                .pane_render
                .get(pane_idx)
                .is_some_and(|p| p.render_in_flight)
            {
                continue;
            }

            let site = target.volume.site.clone();
            let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                continue;
            };
            let (lat, lon) = (scan_info.site.lat, scan_info.site.lon);
            let Some(data) = self.scan_data.get(site.as_str()).map(Arc::clone) else {
                continue;
            };

            // The two refusals that have to be *named* rather than left as a
            // blank pane. Checked here, before any budget is taken, because both
            // are properties of the volume and the product rather than of the
            // cut — dispatching would burn a render slot to be told the same
            // thing, and on wasm there is only one slot.
            if data.coverage_pattern().elevation_cuts().is_empty() {
                // The live chunk feed's mid-flight state: `chunks.rs` stands in
                // an empty coverage pattern until the VCP message lands, and
                // `VolumeSampler::new` refuses it rather than inventing a ladder
                // out of the sweeps' own elevation numbers. It resolves itself
                // on the next volume, so the key is *not* written — the pane
                // will ask again, and get an answer.
                self.mark_section_unavailable(
                    pane_idx,
                    rustdar_egui::pane::SectionUnavailable::AwaitingCoveragePattern,
                );
                continue;
            }
            if rustdar_radar::sampler::samplable(target.product).is_none() {
                // Permanent for this product, so the key *is* written: nothing
                // about this volume will make a column integral sliceable, and
                // re-asking every frame would be a busy loop with no output.
                self.mark_section_unavailable(
                    pane_idx,
                    rustdar_egui::pane::SectionUnavailable::ProductHasNoVerticalStructure(
                        target.product,
                    ),
                );
                if let Some(section) = self
                    .gui
                    .pane_mut(pane_idx)
                    .and_then(|p| p.cross_section_mut())
                {
                    section.rendered_for = Some(target);
                }
                continue;
            }

            if self.render.spawn_section_render(
                pane_idx,
                &target,
                &data,
                lat,
                lon,
                self.channels.section_sender.clone(),
                self.window.clone(),
            ) && let Some(section) = self
                .gui
                .pane_mut(pane_idx)
                .and_then(|p| p.cross_section_mut())
            {
                // Written on **dispatch**, not on arrival. A cut that answers
                // nothing would otherwise never write it, and the pane would
                // re-dispatch the same failing cut on every frame for as long as
                // the volume stood — a busy loop whose only symptom is a warm
                // machine. `poll_section_results` matches the reply against this
                // key, so a superseded cut still cannot land.
                section.rendered_for = Some(target);
                section.unavailable = None;
            }
        }
    }

    /// What pane `pane_idx` would have to cut to be showing the truth, or `None`
    /// if it is not a section pane, has no line, or has no volume yet.
    ///
    /// The "no volume yet" arm is where a pane gets told it is waiting: that is
    /// the ordinary state at startup and after a site switch, and a section pane
    /// showing nothing with no explanation is indistinguishable from one that is
    /// broken.
    fn section_target_for_pane(
        &mut self,
        pane_idx: usize,
    ) -> Option<rustdar_egui::pane::SectionTarget> {
        let pane = self.gui.pane(pane_idx)?;
        let section = pane.cross_section()?;
        let line = section.line?;
        let product = pane.selected_product;
        let site = pane.site.clone();
        let (collected, tilts) = match pane.scan_info.as_ref() {
            // The tilt count is read from the *same* place the tilt ladder is
            // drawn from, so the key moves when the ladder does. See
            // `SectionTarget::tilts`: on the live chunk feed `timestamp` is the
            // first sweep's, and so is frozen for the whole volume while the
            // ladder grows from one rung to fourteen underneath it.
            Some(scan_info) => (
                scan_info.timestamp,
                scan_info
                    .product_elevations
                    .get(&product)
                    .map_or(0, Vec::len),
            ),
            None => {
                self.mark_section_unavailable(
                    pane_idx,
                    rustdar_egui::pane::SectionUnavailable::AwaitingVolume,
                );
                return None;
            }
        };
        Some(rustdar_egui::pane::SectionTarget {
            volume: rustdar_egui::pane::VolumeStamp { site, collected },
            product,
            line,
            tilts,
        })
    }

    /// Record why a section pane has no picture, leaving whatever it is showing
    /// alone.
    ///
    /// The picture is deliberately **not** cleared. A section of the previous
    /// volume is stale rather than wrong, it is labelled with its own volume
    /// time in the pane's caption, and blanking the pane every time the live
    /// feed rejoins mid-scan would make the feature flicker for a reason the
    /// user cannot act on.
    fn mark_section_unavailable(
        &mut self,
        pane_idx: usize,
        reason: rustdar_egui::pane::SectionUnavailable,
    ) {
        if let Some(section) = self
            .gui
            .pane_mut(pane_idx)
            .and_then(|p| p.cross_section_mut())
        {
            section.unavailable = Some(reason);
        }
    }

    /// Take delivery of finished cross-sections and upload their rasters.
    fn poll_section_results(&mut self, ctx: &egui::Context) {
        while let Ok(sr) = self.channels.section_receiver.try_recv() {
            if let Some(state) = self.render.pane_render.get_mut(sr.pane_idx) {
                state.render_in_flight = false;
            }

            if self.render.is_render_stale(sr.generation) {
                // The key was written on dispatch, so leaving it would tell the
                // dispatcher this cut had been answered when it had been thrown
                // away — and nothing else would ever ask again. Cleared, so the
                // pane re-dispatches against whatever it is aimed at now.
                if let Some(section) = self
                    .gui
                    .pane_mut(sr.pane_idx)
                    .and_then(|p| p.cross_section_mut())
                {
                    section.rendered_for = None;
                }
                continue;
            }

            let Some(section_state) = self
                .gui
                .pane_mut(sr.pane_idx)
                .and_then(|p| p.cross_section_mut())
            else {
                continue;
            };
            // The pane has been re-aimed, converted or re-sited while this cut
            // was in the air. Dropped without touching the key: whatever the
            // pane is waiting for now is still on its way.
            if section_state.rendered_for.as_ref() != Some(&sr.target) {
                continue;
            }

            let Some(cut) = sr.section else {
                section_state.unavailable =
                    Some(rustdar_egui::pane::SectionUnavailable::RenderFailed);
                continue;
            };

            self.texture_counter += 1;
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [
                    rustdar_radar::xsect::SECTION_WIDTH,
                    rustdar_radar::xsect::SECTION_HEIGHT,
                ],
                cut.image(),
            );
            // NEAREST, and it is an honesty decision rather than a performance
            // one. A section's rows are the tilt ladder's rungs stretched to
            // fill the gaps between them; bilinear filtering would blend those
            // edges into a smooth gradient and paint exactly the impression the
            // pane's caption exists to refuse — that the vertical structure was
            // measured continuously. The blockiness is the data.
            let texture = ctx.load_texture(
                format!("cross_section_{}", self.texture_counter),
                color_image,
                egui::TextureOptions::NEAREST,
            );

            let Some(section_state) = self
                .gui
                .pane_mut(sr.pane_idx)
                .and_then(|p| p.cross_section_mut())
            else {
                continue;
            };
            if let Some(old) = section_state.texture.take() {
                self.old_textures.push(old);
            }
            section_state.texture = Some(texture);
            section_state.section = Some(Arc::from(cut));
            section_state.unavailable = None;
        }
    }

    /// Restore the radar image from cached raw RGBA data.
    ///
    /// Called after wgpu state is recreated (suspend/resume or surface loss) to
    /// avoid a multi-second background re-render.  Re-uploads the cached pixel
    /// data as a new GPU texture instantly.
    /// The egui context is a parameter for the same reason it is on
    /// `poll_render_results` and `dispatch_pane_renders`: the caller has it, one
    /// `unwrap` on the renderer per frame beats three, and it is what lets this be
    /// driven headlessly against a bare `Context` — which `Context::load_texture`
    /// is all this needs. Reaching through `self.state` here made the pane-kind
    /// filter above untestable: the whole function returned early with no
    /// renderer, so a test could not tell a skipped pane from a skipped call.
    pub(super) fn restore_cached_render(&mut self, ctx: &egui::Context) {
        use rustdar_egui::overlay_cache::{OverlayTextureData, RadarTextureMeta};
        use rustdar_overlays::render::overlay_state::OverlayKind;
        use rustdar_overlays::types::GeoBounds;
        use rustdar_radar::types::ImageBounds;

        for pane_idx in 0..self.render.pane_render.len().min(self.gui.pane_count()) {
            // `dispatch_pane_renders` deliberately *keeps* `cached_render` on a
            // converted pane, so that converting back to a map is instant. That
            // makes this the one place the kept copy could still be uploaded: every
            // suspend, resume and surface loss would re-create a full
            // `IMAGE_SIZE` x `IMAGE_SIZE` RGBA texture in the Radar overlay cache of
            // a pane that draws no map.
            if self.gui.pane_has_no_plan_view(pane_idx) {
                continue;
            }
            let Some(ref cached) = self.render.pane_render[pane_idx].cached_render else {
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
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [IMAGE_SIZE, IMAGE_SIZE],
                &cached.image_data,
            );
            let texture_name = format!("radar_image_{}", self.texture_counter);
            let texture =
                ctx.load_texture(texture_name, color_image, egui::TextureOptions::NEAREST);

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
                        // The restored image depicts what the cached render did,
                        // so it is described the same way. A resume that put the
                        // pixels back without this would leave a pane that had
                        // been switched while suspended showing the old product
                        // with nothing saying so.
                        product,
                        elevation,
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
        let (mut frame, status) = finish_then_acquire(
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
                frame.submit(&state.queue, encoder);
                state.egui_renderer.free_textures(frame.textures_to_free());

                if matches!(status, SurfaceStatus::Lost) {
                    // A loss with a volume on screen is the one the 3D view has
                    // to answer for, and it is counted BEFORE `self.state` is
                    // dropped — because dropping it is exactly why the counter
                    // cannot live in `AppState`. A WebGL2 context loss arrives
                    // here, rebuilds the state, and would reset any counter kept
                    // inside it; the volume would then be rebuilt, crash the
                    // context again, and loop forever. `volume::degrade`'s
                    // counter is a module-level `static` for that reason, and
                    // after two such losses the view is permanently unavailable.
                    //
                    // Safe to read `panes()` here despite its `mem::take`
                    // caveat: `present_frame` runs after the egui pass has
                    // ended, never inside it.
                    let volume_on_screen = self
                        .gui
                        .panes()
                        .iter()
                        .any(|pane| pane.kind() == rustdar_egui::pane::PaneKind::Volume);
                    if volume_on_screen {
                        let losses = crate::volume::degrade::note_surface_loss_with_volume();
                        log::warn!(
                            "wgpu surface lost with a 3D volume on screen ({losses} so far)"
                        );
                    }

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

        state
            .egui_renderer
            .draw(&mut encoder, &surface_view, &frame);

        frame.submit(&state.queue, encoder);
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
            let product = pane.selected_product;
            let Some(plan) = accept_scan_listing(&mut pane.loop_state, &resp.site, resp.scans)
            else {
                continue;
            };
            log::info!(
                "Loop: populated {} {} frames for pane {}",
                plan.frames.len(),
                plan.site,
                resp.pane_idx
            );

            // Store the frame plan — with the site it was listed for — then derive
            // the queue for whichever datasource this pane's product reads and
            // dispatch the first batch.
            self.loop_mgr.set_plan(resp.pane_idx, plan);
            self.loop_mgr.plan_downloads_for(resp.pane_idx, product);
            self.dispatch_pending_loop_downloads(resp.pane_idx);
            self.dispatch_pending_loop_l3_pairings(resp.pane_idx);
        }
    }

    /// Poll for finished Level III key listings. Each one unblocks every frame
    /// pairing that was waiting on it.
    fn poll_loop_l3_list_results(&mut self) {
        let mut listed = false;
        while let Ok(resp) = self.channels.loop_l3_list_receiver.try_recv() {
            // Cached under the site and code it was *listed* for, never under
            // whatever the requesting pane has since become — the keys belong to
            // the listing, and every pane looping that site shares them.
            self.loop_mgr
                .cache_l3_keys(&resp.site, &resp.code, resp.keys);
            listed = true;
        }
        if !listed {
            return;
        }
        // Every pane, not just the requester: two panes looping one site wait on
        // one listing, and the second would otherwise sit until something else
        // happened to re-dispatch it.
        for pane_idx in self.loop_mgr.pending_l3_pane_indices() {
            self.dispatch_pending_loop_l3_pairings(pane_idx);
        }
    }

    /// Poll for finished Level III frame pairings. A `None` result is cached as
    /// the answer — the site generated no object for that volume — so the frame is
    /// retired once instead of being re-paired every pass.
    fn poll_loop_l3_fetch_results(&mut self) {
        let mut completed_count = 0usize;
        while let Ok(resp) = self.channels.loop_l3_fetch_receiver.try_recv() {
            self.loop_mgr
                .cache_l3_product(&resp.site, &resp.code, resp.timestamp, resp.product);
            completed_count += 1;
        }
        if completed_count > 0 {
            // The same counter the Level II downloads decrement: one network
            // concurrency budget for the loop, whichever datasource it reads.
            self.loop_mgr.complete_batch(completed_count);
            self.dispatch_freed_loop_slots();
        }
    }

    /// Offer the slots a finished batch released to every pane that still owes
    /// downloads, on **both** datasources.
    ///
    /// The budget is one counter, so a pane looping a Level II product and a pane
    /// looping a Level III one compete for it — and each datasource's completion
    /// drain is the only thing that ever frees a slot. A drain that re-dispatched
    /// only its own kind starves the other: once the budget is full of volume
    /// downloads, nothing re-triggers the pairing queue until a pairing completes,
    /// and no pairing was ever spawned. The pane sits in `Rendering` with its
    /// queue intact and nothing running.
    fn dispatch_freed_loop_slots(&mut self) {
        for pane_idx in self.loop_mgr.pending_pane_indices() {
            self.dispatch_pending_loop_downloads(pane_idx);
        }
        for pane_idx in self.loop_mgr.pending_l3_pane_indices() {
            self.dispatch_pending_loop_l3_pairings(pane_idx);
        }
    }

    /// Dispatch pending Level III frame pairings up to the concurrency limit,
    /// listing the keys they will be ranked against first.
    ///
    /// The shape mirrors [`dispatch_pending_loop_downloads`](Self::dispatch_pending_loop_downloads)
    /// deliberately: the queue is extracted whole so the site travels with it,
    /// entries already resolved or in flight are dropped, a batch up to the
    /// remaining slots is spawned, and the rest goes back.
    ///
    /// Entries whose key listing has not landed are **kept**, not dropped: the
    /// listing is what they need, and `poll_loop_l3_list_results` re-dispatches
    /// them when it arrives. That is also why the queue's emptiness is a safe
    /// answer to "has this pane dispatched everything it owes" — see
    /// `is_pane_done`.
    fn dispatch_pending_loop_l3_pairings(&mut self, pane_idx: usize) {
        let Some(PendingL3Pairings {
            site,
            product,
            queue,
        }) = self.loop_mgr.extract_pending_l3(pane_idx)
        else {
            return;
        };
        // The pick is the product's, not the frame's or the pane's: DPR's
        // intermediates are partial accumulations, so its loop takes each
        // volume's last object while the once-per-volume products take the
        // nearest one. Read from the queue's own product, which cannot have
        // retargeted under it the way the pane can.
        //
        // The pairing cache below is keyed per `(site, code, volume)` and shared
        // by every product that reads the code, so two readers of one code have
        // to agree on this — `every_shared_level3_code_agrees_on_its_volume_pick`
        // in `rustdar_radar::level3` is what holds them to it.
        //
        // `plan_downloads_for` only ever builds this queue for a product that
        // names codes, so the `None` arm is unreachable. It puts the queue back
        // rather than dropping it: an early return that quietly emptied a queue
        // would make `is_pane_done` report a pane as finished with work still
        // owed, which is how a loop gets abandoned mid-fetch.
        let Some(pick) = product.level3_volume_pick() else {
            self.loop_mgr.insert_pending_l3(
                pane_idx,
                PendingL3Pairings {
                    site,
                    product,
                    queue,
                },
            );
            return;
        };

        // One listing per (site, code), shared by every pane looping that site.
        // The days come from the loop's own frames rather than from wall clock:
        // a loop parked on yesterday's data must list yesterday's prefix.
        let days = pairing_days_for_frames(&queue);
        for code in product.level3_products().into_iter().flatten() {
            if self.loop_mgr.claim_l3_listing(&site, code) {
                self.spawn_loop_l3_listing(
                    pane_idx,
                    site.clone(),
                    (*code).to_string(),
                    days.clone(),
                );
            }
        }

        let slots = self.loop_mgr.available_slots(MAX_CONCURRENT_LOOP_DOWNLOADS);
        let mut batch = Vec::new();
        let mut retained = VecDeque::with_capacity(queue.len());
        for (ts, code) in queue {
            if self.loop_mgr.l3_is_resolved(&site, &code, &ts)
                || self.loop_mgr.l3_is_in_flight(&site, &code, &ts)
            {
                // Answered, or being answered — nothing owed either way.
                continue;
            }
            let Some(keys) = self.loop_mgr.l3_keys(&site, &code) else {
                // Waiting on the listing above.
                retained.push_back((ts, code));
                continue;
            };
            if batch.len() >= slots {
                retained.push_back((ts, code));
                continue;
            }
            batch.push((ts, code, Arc::clone(keys)));
        }

        let spawned = batch.len();
        for (ts, code, keys) in batch {
            self.loop_mgr.mark_l3_in_flight(&site, &code, ts);
            self.spawn_loop_l3_pairing(pane_idx, site.clone(), code, ts, keys, pick);
        }

        self.loop_mgr.insert_pending_l3(
            pane_idx,
            PendingL3Pairings {
                site,
                product,
                queue: retained,
            },
        );

        if spawned > 0 {
            self.loop_mgr.add_spawned(spawned);
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
            // Both datasources: the concurrency budget is shared, so the slots this
            // batch released belong to whoever is owed work. See
            // `dispatch_freed_loop_slots`.
            self.dispatch_freed_loop_slots();
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
        self.loop_mgr
            .insert_pending(pane_idx, PendingDownloads { site, queue });

        if spawned > 0 {
            self.loop_mgr.add_spawned(spawned);
        }
    }

    /// Poll for completed loop frame render results and upload textures.
    /// When sync_layers is on, broadcasts rendered textures to sibling panes
    /// that need the same frame (matching product+elevation+timestamp).
    fn poll_loop_render_results(&mut self, ctx: &egui::Context) {
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

            // Broadcast to sibling panes with matching product+elevation+timestamp.
            //
            // The same kind filter as the static broadcast in
            // `poll_render_results`, and it has to be here too: a loop frame is a
            // plan-view raster, so handing one to a pane that draws none buys a GPU
            // texture per frame for nothing. `set_kind` clears a converted pane's
            // loop, so `is_rendered_for` below would refuse it anyway — this is
            // the cheap, explicit refusal rather than one that depends on a
            // teardown elsewhere having happened first.
            if self.gui.is_sync_layers() {
                for sibling_idx in 0..self.gui.pane_count() {
                    if sibling_idx == origin_pane || self.gui.pane_has_no_plan_view(sibling_idx) {
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

                    let Some(sibling) = self.gui.pane_mut(sibling_idx) else {
                        continue;
                    };
                    // Hand the image only to panes whose frames are keyed to exactly
                    // what it depicts, site and sweep included. Matching against the
                    // response rather than the origin pane's live selection keeps a
                    // retarget on either side from planting an image the receiving pane
                    // will never correct. The decision — and the frame it resolves to —
                    // lives in `LoopPlaybackState` so it stays in step with the donor
                    // test the dispatcher applies before suppressing a pane's own render.
                    let Some(sframe) = sibling.loop_state.frame_accepting_broadcast_mut(
                        rr.timestamp,
                        &rr.target,
                        sweep,
                    ) else {
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
    /// render has settled — or off entirely when none of them can be rendered at
    /// all — then start playback for the panes that are ready.
    ///
    /// Runs once per frame after dispatch rather than inside the render-response
    /// drain. Several things that settle a batch never produce a render response —
    /// a frame retired as unrenderable, a texture cloned from a sibling pane, the
    /// render set shifting as the playhead moves — so a loop can be complete with
    /// nothing left to receive. A second pane whose frames are all satisfied by
    /// sibling clones spawns no renders at all, and would never be promoted.
    ///
    /// The phase decision itself is [`settle_loop_phase`]; what is left here is the
    /// state that lives outside the pane, which a loop being switched off has to
    /// release.
    pub(super) fn update_loop_readiness(&mut self) {
        let mut abandoned = Vec::new();
        for pidx in 0..self.gui.pane_count() {
            let loop_mgr = &self.loop_mgr;
            let Some(p) = self.gui.pane_mut(pidx) else {
                continue;
            };
            if settle_loop_phase(loop_mgr, pidx, &mut p.loop_state, MAX_LOOP_RENDER_BUDGET) {
                abandoned.push(pidx);
            }
        }
        for pidx in abandoned {
            // The same release `handle_disable_loop` does: the pane is back to
            // single-frame mode, and clearing `last_rendered` is what makes
            // `dispatch_pane_renders` put its static image back.
            self.loop_mgr.remove_pending(pidx);
            if pidx < self.render.pane_render.len() {
                self.render.pane_render[pidx].last_rendered = None;
            }
        }

        // Synchronized playback start: when sync_layers is on, wait for ALL
        // looping panes to be render_ready before starting any of them.
        self.sync_loop_playback_start();
    }

    /// Start loop playback for panes that are ready, synchronizing when sync_layers is on.
    ///
    /// # Why a pane with no plan view is not merely skipped but must be
    ///
    /// The sync rule below is "hold every looping pane until all of them are
    /// ready", and a pane whose frames nothing renders can never become ready —
    /// `dispatch_loop_renders` neither fills its frames nor marks them failed. So
    /// one such pane in `not_ready_panes`, with Sync Layers on, stops **every map
    /// pane's** loop from ever starting. The symptom is in the other panes, which
    /// is what makes it the worst of these: a deadlock introduced by the very
    /// filter that protects the render path.
    ///
    /// `PaneState::set_kind` clears a converted pane's loop, so the state should
    /// be unreachable. This is here anyway, because the cost of being wrong is
    /// every loop on screen rather than one pane's, and because the field is
    /// public. Pinned by
    /// `a_pane_with_no_plan_view_cannot_hold_another_panes_loop_back`.
    fn sync_loop_playback_start(&mut self) {
        let pane_count = self.gui.pane_count();
        let sync = self.gui.is_sync_layers() && pane_count > 1;

        // Collect readiness status for all panes with active loops
        let mut ready_panes: Vec<usize> = Vec::new();
        let mut not_ready_panes: Vec<usize> = Vec::new();
        for idx in 0..pane_count {
            if self.gui.pane_has_no_plan_view(idx) {
                continue;
            }
            let Some(pane) = self.gui.pane(idx) else {
                continue;
            };
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
        let interval = loop_interval(self.gui.loop_speed_fps);

        for pane_idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            let ls = &mut pane.loop_state;
            if !ls.is_active() || !ls.is_playing() || ls.frames.is_empty() {
                continue;
            }

            let should_advance = ls
                .last_advance
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
    ///
    /// Both loops below skip panes with no plan view
    /// ([`Gui::pane_has_no_plan_view`](rustdar_egui::Gui::pane_has_no_plan_view)).
    /// A loop frame *is* a rendered plan-view tilt, so there is nothing to
    /// dispatch for a section or a volume pane and nothing to clone into one —
    /// and the first loop's replan would otherwise start a download queue for a
    /// pane nobody is drawing. `loop_sync_targets` keeps such a pane out of the
    /// enable action in the first place; this is the other half, for the pane
    /// that was converted while its loop was already running.
    ///
    /// The first pass also finishes the teardown `PaneState::set_kind` starts.
    /// That setter clears a converted pane's `loop_state`, which is the half a
    /// pane can do for itself; the other half is this pane's queue inside
    /// `LoopDownloadManager`, which is keyed by index and which a `PaneState`
    /// cannot reach. Doing it here rather than at the conversion covers every
    /// route to a non-map pane — the menu, a restored config, a later auto-create
    /// — and it is idempotent, so running it once a frame costs a hash lookup.
    fn dispatch_loop_renders(&mut self) {
        // Panes whose product moved to another datasource, so the frames now need
        // bytes nothing is fetching. Collected here and acted on below, because
        // re-deriving a queue needs `loop_mgr` while the pane is borrowed.
        let mut replan: Vec<(usize, rustdar_radar::types::RadarProduct)> = Vec::new();
        for pane_idx in 0..self.gui.pane_count() {
            if self.gui.pane_has_no_plan_view(pane_idx) {
                // The host-side half of the loop teardown. Without it the pane's
                // queue outlives its loop and goes on spending the *shared*
                // download budget on volumes nobody will draw, starving the live
                // map panes beside it.
                self.loop_mgr.remove_pending(pane_idx);
                continue;
            }
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
                    pane_idx,
                    product,
                    elevation
                );
                // The retarget may have crossed the Level II / Level III line, in
                // which case every frame now needs bytes the old queue was not
                // fetching. `plan_downloads_for` is a no-op when the product has
                // not actually moved, so this is safe to ask unconditionally.
                replan.push((pane_idx, product));
                continue;
            }

            // Evict textures from frames far from the playhead to cap memory usage.
            ls.evict_textures_outside_render_set(MAX_LOOP_RENDER_BUDGET);
        }
        for (pane_idx, product) in replan {
            if self.loop_mgr.plan_downloads_for(pane_idx, product) {
                log::info!(
                    "Loop: pane {pane_idx} now reads {} for its frames",
                    if product.is_level3() {
                        "Level III objects"
                    } else {
                        "Level II volumes"
                    },
                );
                self.dispatch_pending_loop_downloads(pane_idx);
                self.dispatch_pending_loop_l3_pairings(pane_idx);
            }
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
            if self.gui.pane_has_no_plan_view(pane_idx) {
                continue;
            }
            let Some(pane) = self.gui.pane(pane_idx) else {
                continue;
            };
            let ls = &pane.loop_state;
            if !ls.is_active() || ls.frames.is_empty() {
                continue;
            }

            let site_lat = ls.site_lat;
            let site_lon = ls.site_lon;

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
                        (0..pane_count)
                            .filter_map(|i| self.gui.pane(i).map(|p| (i, &p.loop_state))),
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

                // The sweep this frame's own data resolves the selection to, or
                // why it cannot be rendered. One question for both datasources —
                // see `frame_sweep`.
                match frame_sweep(&self.loop_mgr, &target, frame.timestamp) {
                    FrameSweep::At(snapped) => {
                        // Deduplicate: if another pane already queued a render for the
                        // same target and timestamp, skip — the broadcast in
                        // poll_loop_render_results will deliver the texture to this pane.
                        if sync
                            && render_already_queued(&to_render, frame.timestamp, &target, snapped)
                        {
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
                    // Nothing will ever render this frame — the volume carries no
                    // sweep for the product, or the site generated no object for
                    // this volume. Retire it so the dispatcher stops retrying and
                    // readiness stops waiting; playback then steps over it, which
                    // is what a gap has always looked like.
                    FrameSweep::Unrenderable => to_mark_failed.push((pane_idx, idx)),
                    // Its data has not arrived yet. Left alone; the next pass asks
                    // again.
                    FrameSweep::Pending => {}
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
                let Some(src) = self.gui.pane(req.src_pane) else {
                    continue;
                };
                let Some(sframe) = src.loop_state.frames.get(req.src_frame) else {
                    continue;
                };
                let Some(tex) = sframe.texture.clone() else {
                    continue;
                };
                tex
            };
            let Some(dest) = self.gui.pane_mut(req.dest_pane) else {
                continue;
            };
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
            // an entry, but missing data is a skipped frame the next pass retries,
            // not something to bring the process down over.
            let Some(data) = frame_data(&self.loop_mgr, &req.target, req.timestamp) else {
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
                data,
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
/// it now owes.
///
/// `None` means there is nothing to download, for one of two reasons:
/// - This loop is not the one that asked for the listing (see below), and is left
///   exactly as it was.
/// - The listing is empty — the site served nothing for the window, or the request
///   failed and `handle_enable_loop` sent an empty list in its place. There is no
///   loop to be had, so the loop is switched off and the pane returns to its static
///   image. The alternative is what this used to do: advance to `Rendering` with
///   zero frames, where `update_loop_readiness` skips it (no frames),
///   `any_loop_active` reads false (nothing in flight) and nothing retries — a
///   pane stuck reading "rendering" for the rest of the session.
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
/// The frame list and the returned plan are built from one sampled set on purpose:
/// they are the two halves of the same decision, and a frame with no planned
/// download never settles.
///
/// The plan is returned rather than a download queue because *what* each frame
/// needs depends on the pane's product, which can change without re-listing: a
/// Level II product wants each frame's archive volume, a Level III product wants
/// the bucket objects of the same volumes and not the volumes at all. The frame
/// list — the loop's timeline — is the same either way, which is what keeps a
/// mixed set of panes animating in step. See
/// [`crate::loop_downloads::LoopDownloadManager::plan_downloads_for`].
fn accept_scan_listing(
    ls: &mut rustdar_egui::pane::LoopPlaybackState,
    site: &str,
    scans: Vec<(chrono::NaiveDateTime, rustdar_radar::archive::Identifier)>,
) -> Option<FramePlan> {
    if !ls.is_active() || ls.site != site {
        return None;
    }

    if scans.is_empty() {
        log::warn!("Loop: no {site} scans in the requested window; leaving loop mode");
        *ls = rustdar_egui::pane::LoopPlaybackState::new();
        return None;
    }

    // Cap the downloads at MAX_LOOP_FRAMES by evenly sampling the listing.
    let scans = if scans.len() > MAX_LOOP_FRAMES {
        let total = scans.len();
        let sampled: Vec<_> = (0..MAX_LOOP_FRAMES)
            .map(|i| scans[i * (total - 1) / (MAX_LOOP_FRAMES - 1).max(1)].clone())
            .collect();
        log::info!(
            "Loop: sampled {} → {} frames for {}",
            total,
            MAX_LOOP_FRAMES,
            site
        );
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

    Some(FramePlan::new(site.to_string(), scans))
}

/// Move a loop that is still `Rendering` on to whatever its frames have settled
/// into, returning `true` if the loop was switched off.
///
/// Three outcomes, and the third is the one that used to be missing:
/// - Nothing has settled yet: left alone.
/// - Something rendered: promoted to `Ready`, and playback starts.
/// - Nothing rendered and nothing ever will: switched off. Every frame has been
///   ruled out — retired as `render_failed` because its scan carries no sweep for
///   the selected product, or left with no scan at all because its download
///   failed — and no listing, download or render is outstanding to change that.
///   Left in `Rendering` such a loop is a dead end: readiness needs a rendered
///   frame to promote it, `any_loop_active` reads false so nothing even repaints,
///   and the pane draws its loop frames instead of its static image — which means
///   it draws nothing at all.
///
/// Switching off rather than promoting to `Ready` is deliberate: a `Ready` loop
/// with no textures starts "playing", asks for a repaint every frame, and shows an
/// empty pane. Off, the pane goes back to its static radar image, which is what
/// the user had before enabling the loop.
///
/// The caller's half of switching off is in `update_loop_readiness`; both
/// download bookkeeping and the settled/finished distinction are resolved here so
/// the decision is one testable unit rather than three booleans assembled at an
/// untestable call site.
fn settle_loop_phase(
    loop_mgr: &crate::loop_downloads::LoopDownloadManager,
    pane_idx: usize,
    ls: &mut rustdar_egui::pane::LoopPlaybackState,
    budget: usize,
) -> bool {
    if !ls.is_active() || ls.is_render_ready() || ls.frames.is_empty() {
        return false;
    }
    // `is_pane_done` means "dispatched", not "arrived" — see below.
    if !loop_batch_settled(loop_mgr, ls, budget) || !loop_mgr.is_pane_done(pane_idx) {
        return false;
    }
    if ls.frames.iter().any(|f| f.texture.is_some()) {
        ls.phase = rustdar_egui::pane::LoopPhase::Ready;
        return false;
    }
    // A frame whose data is still arriving is "settled" as far as rendering goes —
    // nothing is in flight for it *yet* — so the download half has to be asked
    // separately before concluding that nothing will ever render. Otherwise every
    // loop is abandoned on the pass right after its last batch is dispatched.
    //
    // Asked about the loop's own product, so a Level III loop's pairings hold it
    // open the way a Level II loop's volume downloads do.
    if let Some(product) = loop_product(ls)
        && ls
            .frames
            .iter()
            .any(|f| loop_mgr.frame_data_in_flight(&ls.site, product, &f.timestamp))
    {
        return false;
    }
    log::warn!("Loop: no frame on pane {pane_idx} could be rendered; leaving loop mode");
    *ls = rustdar_egui::pane::LoopPlaybackState::new();
    true
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

/// Every UTC day the pairing windows of `queue`'s volumes touch, deduplicated.
///
/// Derived from the frames rather than from wall clock. A loop can be parked on
/// historic data — `handle_navigate_time` then `reinit_active_loops` rebuilds it
/// around whatever scan the pane is showing — and listing today's prefix for a
/// loop over yesterday's volumes finds nothing, which is indistinguishable from
/// "the site served no objects" and would retire every frame as a gap.
///
/// One listing per day is a round-trip, so the set is kept minimal: a loop inside
/// one UTC day yields two days (the day and the one before, per
/// [`rustdar_radar::level3::pairing_days`]), a loop spanning midnight three.
fn pairing_days_for_frames(
    queue: &VecDeque<(chrono::NaiveDateTime, String)>,
) -> Vec<chrono::NaiveDate> {
    let mut days: Vec<chrono::NaiveDate> = Vec::new();
    for (ts, _) in queue {
        for day in rustdar_radar::level3::pairing_days(*ts) {
            if !days.contains(&day) {
                days.push(day);
            }
        }
    }
    days
}

/// The data a loop keyed to `target` renders for `timestamp`: the Level II volume,
/// or every Level III object of that volume, whichever `target.product` reads.
///
/// `target.site` is where the loop's geometry came from, so it is also the only
/// site whose data may be projected with it. The pane's live `site` field is not a
/// substitute — it is re-synced across panes without rebuilding their loops — and
/// it is not in scope here.
fn frame_data(
    loop_mgr: &crate::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> Option<LoopFrameData> {
    loop_mgr.frame_data(&target.site, target.product, &timestamp)
}

/// What one frame's own data makes of the pane's elevation selection.
enum FrameSweep {
    /// The sweep the frame will be rendered at.
    At(f32),
    /// The data is here and carries nothing for this product: the volume has no
    /// such sweep, or the site generated no object for this volume. Terminal.
    Unrenderable,
    /// The data has not arrived yet.
    Pending,
}

/// The sweep frame `timestamp` of a loop keyed to `target` would be rendered at.
///
/// One function for both datasources, because the *distinction* the loop draws is
/// not "which datasource" but "renderable, gap, or waiting" — and every caller
/// downstream needs exactly those three.
///
/// * A Level II frame snaps the selection to the nearest sweep its own volume
///   carries. Two volumes can snap one selection differently, which is why this is
///   per frame rather than per loop.
/// * A Level III frame is one object per code, already chosen: the sweep it depicts
///   is the object's own PDB elevation angle. That is the honest answer — it is
///   what the image shows — and it makes the sibling broadcast's sweep comparison
///   mean something, since two panes resolving the same `(site, code, volume)`
///   share one cache entry and therefore one angle.
fn frame_sweep(
    loop_mgr: &crate::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> FrameSweep {
    if target.product.is_level3() {
        return match loop_mgr.l3_frame_state(&target.site, target.product, &timestamp) {
            L3FrameState::Pending => FrameSweep::Pending,
            L3FrameState::Absent => FrameSweep::Unrenderable,
            L3FrameState::Ready => {
                match loop_mgr
                    .l3_frame_products(&target.site, target.product, &timestamp)
                    .as_deref()
                    .and_then(<[_]>::first)
                {
                    Some(first) => FrameSweep::At(first.message.pdb.elevation_angle()),
                    // `Ready` promised every code, so this is unreachable; a
                    // retired frame is still the right answer for a product that
                    // names no codes at all.
                    None => FrameSweep::Unrenderable,
                }
            }
        };
    }
    let Some(scan) = loop_mgr.get_cached(&target.site, &timestamp) else {
        return FrameSweep::Pending;
    };
    match rustdar_radar::render::find_closest_elevation(scan, target.product, target.elevation) {
        Some(snapped) => FrameSweep::At(snapped),
        None => FrameSweep::Unrenderable,
    }
}

/// The sweep `ls`'s own data for `timestamp` resolves `product`/`elevation` to, or
/// `None` if it has none or that data carries nothing for the product.
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
///   sender resolved its data from moments ago, so it is present.
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
    // Resolved through the same function the dispatcher plans with, against the
    // receiver's own site: a second rule for "which sweep does this frame show"
    // would be free to disagree with the one that produced `rr.snapped`.
    match frame_sweep(
        loop_mgr,
        &RenderTarget::new(ls.site.clone(), product, elevation),
        timestamp,
    ) {
        FrameSweep::At(sweep) => Some(sweep),
        FrameSweep::Unrenderable | FrameSweep::Pending => None,
    }
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
        own: own_sweep(
            loop_mgr,
            ls,
            rr.timestamp,
            rr.target.product,
            rr.target.elevation,
        ),
    }
}

/// The product a loop's frames are keyed to, or `None` before the first dispatch.
///
/// Read off `rendered_for` rather than off the pane. The two diverge for exactly
/// one dispatch pass after a retarget, and every question below — has this frame's
/// data arrived, is something fetching it — is about the frames as they stand, not
/// about the selection they are on their way to.
fn loop_product(
    ls: &rustdar_egui::pane::LoopPlaybackState,
) -> Option<rustdar_radar::types::RadarProduct> {
    ls.rendered_for.as_ref().map(|t| t.product)
}

/// Whether every frame `ls` intends to render has settled, given what has arrived.
///
/// The "has it arrived" question is asked about the loop's own site *and its own
/// product*. Site-blind, another site's scan at the same timestamp counts as this
/// frame's data. Product-blind, a Level III loop's frames would be judged against
/// a Level II volume cache nothing is filling, so no batch would ever settle and
/// the loop would sit in `Rendering` for the session.
fn loop_batch_settled(
    loop_mgr: &crate::loop_downloads::LoopDownloadManager,
    ls: &rustdar_egui::pane::LoopPlaybackState,
    budget: usize,
) -> bool {
    let Some(product) = loop_product(ls) else {
        // Nothing dispatched yet, so nothing has settled.
        return false;
    };
    // Not merely "nothing in flight this instant": the render budget is shared with
    // static pane renders, so part of a batch can be starved and not yet spawned.
    ls.render_set_settled(budget, |f| {
        loop_mgr.frame_data_settled(&ls.site, product, &f.timestamp)
    })
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
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
    };
    use rustdar_egui::pane::{LoopFrame, LoopPhase, LoopPlaybackState};
    use rustdar_radar::archive::Identifier;
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
    pub(super) fn scan_with_sweeps(elevations: &[f32]) -> Arc<Scan> {
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
                    Some(MomentData::from_fixed_point(
                        1,
                        0,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![0],
                    )),
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
                212,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
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
            &RadarSite {
                name: site,
                lat: 35.0,
                lon: -97.0,
                elev: None,
            },
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

    fn queued(
        target: RenderTarget,
        timestamp: chrono::NaiveDateTime,
        snapped: f32,
    ) -> LoopRenderRequest {
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
        assert!(render_already_queued(
            &q,
            ts(0),
            &target("KTLX", 0.505),
            0.48
        ));
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
        assert!(!render_already_queued(
            &q,
            ts(1),
            &target("KTLX", 0.5),
            0.48
        ));
        // Same target, but the two scans resolved the selection to different sweeps,
        // so the images differ.
        assert!(!render_already_queued(&q, ts(0), &target("KTLX", 0.5), 1.5));
        assert!(!render_already_queued(
            &[],
            ts(0),
            &target("KTLX", 0.5),
            0.48
        ));
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
                    BroadcastSweep {
                        rendered: 0.48,
                        own: Some(own),
                    },
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
        let plan = accept_scan_listing(&mut koun, "KOUN", live).expect("its own listing");
        assert_eq!(
            plan.site, "KOUN",
            "the plan carries the site it was listed for"
        );
        assert_eq!(plan.frames.len(), 1);
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

    /// The wedge. A failed listing is delivered as an empty list, and so is a
    /// window the site served nothing for. Advancing to `Rendering` with no frames
    /// is a state nothing leaves: readiness skips loops with no frames,
    /// `any_loop_active` reads false so the app stops repainting, nothing retries,
    /// and the pane draws its (nonexistent) loop frames instead of its static
    /// image for the rest of the session.
    #[test]
    fn an_empty_listing_switches_the_loop_off() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        ls.phase = LoopPhase::FetchingScanList;

        assert!(
            accept_scan_listing(&mut ls, "KTLX", Vec::new()).is_none(),
            "there is nothing to download"
        );
        assert!(
            !ls.is_active(),
            "the pane must fall back to its static image, not sit in Rendering"
        );
        assert!(ls.frames.is_empty());
    }

    /// A loop in `Rendering` whose frames have all been ruled out — every scan
    /// carries no sweep for the selected product — is the same dead end reached
    /// from the other side: readiness needs a rendered frame to promote it, and
    /// there will never be one.
    #[test]
    fn a_loop_no_frame_of_which_can_render_is_switched_off() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        for frame in &mut ls.frames {
            frame.render_failed = true;
        }
        let mgr = LoopDownloadManager::new();

        assert!(
            settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET),
            "the caller has to release this pane's loop state"
        );
        assert!(!ls.is_active());
    }

    /// …but not while its scans are still arriving. A frame with no scan yet is
    /// "settled" as far as rendering goes — nothing is in flight for it *yet* — so
    /// a check that only asked the render side would abandon every loop on the
    /// pass right after its last download batch was dispatched.
    #[test]
    fn a_loop_still_waiting_on_its_scans_is_left_alone() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        let mut mgr = LoopDownloadManager::new();
        mgr.mark_in_flight("KTLX", ts(0));

        assert!(!settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET));
        assert_eq!(ls.phase, LoopPhase::Rendering, "still working");

        // Undispatched downloads hold it open too.
        let mut mgr = LoopDownloadManager::new();
        mgr.insert_pending(
            0,
            PendingDownloads {
                site: "KTLX".to_string(),
                queue: [(ts(1), identifier("KTLX20240101_000100_V06"))]
                    .into_iter()
                    .collect(),
            },
        );
        assert!(!settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET));
        assert_eq!(ls.phase, LoopPhase::Rendering);
    }

    /// One rendered frame is still enough to play, whatever became of the rest.
    #[test]
    fn a_loop_with_something_to_show_is_promoted_rather_than_abandoned() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[1]);
        ls.frames[0].render_failed = true;
        ls.frames[2].render_failed = true;
        let mgr = LoopDownloadManager::new();

        assert!(!settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET));
        assert_eq!(ls.phase, LoopPhase::Ready);
    }

    /// The frame list and the frame *plan* are the two halves of one decision:
    /// every frame must be in the plan, or nothing ever fetches its data, it never
    /// settles and the loop hangs in `Rendering`. That has to survive the sampling
    /// that caps long listings.
    ///
    /// The plan, not a download queue: which bytes a frame needs depends on the
    /// pane's product, and switching between a Level II and a Level III product
    /// re-derives the queue from this same plan rather than re-listing. So the
    /// agreement being pinned is frames-to-plan; `plan_downloads_for` is what turns
    /// the plan into one queue or the other, and
    /// `a_level3_loop_queues_a_pairing_per_frame_and_no_volume_downloads` pins that
    /// half.
    ///
    /// Taking the listing also has to *advance* the phase. This is the one fixture
    /// that starts where a real loop starts — `FetchingScanList`, set by
    /// `new_for_loop` and left there until its listing lands — so a missing advance
    /// reads as a loop still fetching rather than as a value already in place.
    /// Left in `FetchingScanList`, `is_fetching()` never goes false: the pane keeps
    /// its "fetching" label and keeps asking for continuous repaints forever.
    #[test]
    fn the_frame_list_and_the_frame_plan_describe_the_same_scans() {
        let ctx = egui::Context::default();
        let mut ls = loop_on(&ctx, "KTLX", &[]);
        ls.phase = LoopPhase::FetchingScanList;
        assert!(
            ls.is_fetching(),
            "precondition: a loop awaiting its listing"
        );

        let scans: Vec<_> = (0..(MAX_LOOP_FRAMES as u32 + 40))
            .map(|i| (ts(i), identifier(&format!("KTLX2024010{}_V06", i))))
            .collect();

        let plan = accept_scan_listing(&mut ls, "KTLX", scans).expect("accepted");

        assert_eq!(plan.frames.len(), MAX_LOOP_FRAMES, "capped");
        assert_eq!(
            ls.frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
            plan.frames.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            "the sampled set is the frame list, frame for frame"
        );
        assert_eq!(
            ls.current_frame,
            ls.frames.len() - 1,
            "playback starts at the newest"
        );
        assert_eq!(ls.phase, LoopPhase::Rendering);
        assert!(
            !ls.is_fetching(),
            "and the loop has stopped reading as fetching"
        );
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

        assert_ne!(
            rr.site_lat, ls.site_lat,
            "precondition: the two sources differ"
        );
        assert_ne!(rr.site_lon, ls.site_lon);

        let texture = accept_render_result(&mut ls, &mut rr, |_| dummy_texture(&ctx))
            .expect("the loop is awaiting this result");

        let image = ls.frames[1].texture.as_ref().expect("the frame was filled");
        assert_eq!(
            image.lat, rr.site_lat,
            "the latitude the image was projected around"
        );
        assert_eq!(image.lon, rr.site_lon);
        assert_eq!(image.max_range_km, rr.max_range_km);
        assert!(
            !ls.frames[1].render_in_flight,
            "and the frame is no longer in flight"
        );

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

        assert!(
            placed.is_none(),
            "a result for another elevation is not this loop's"
        );
        assert_eq!(uploads, 0, "and nothing was uploaded for it");
        assert!(ls.frames[1].texture.is_none());
        assert!(
            stale.image.is_some(),
            "and its pixels were not taken off the response"
        );
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

        apply_completed_download(
            &mut mgr,
            crate::channels::LoopScanDownloadResponse {
                pane_idx: 0,
                site: "KTLX".to_string(),
                timestamp: ts(0),
                scan: Some(Arc::clone(&scan)),
            },
        );

        assert!(Arc::ptr_eq(
            mgr.get_cached("KTLX", &ts(0)).expect("cached"),
            &scan
        ));
        assert!(mgr.get_cached("KOUN", &ts(0)).is_none());
        assert!(!mgr.is_in_flight("KTLX", &ts(0)), "and its mark is cleared");
    }

    /// A failed download still clears the mark, or the timestamp is never retried.
    #[test]
    fn a_failed_download_clears_its_mark_and_caches_nothing() {
        let mut mgr = LoopDownloadManager::new();
        mgr.mark_in_flight("KTLX", ts(0));

        apply_completed_download(
            &mut mgr,
            crate::channels::LoopScanDownloadResponse {
                pane_idx: 0,
                site: "KTLX".to_string(),
                timestamp: ts(0),
                scan: None,
            },
        );

        assert!(!mgr.is_in_flight("KTLX", &ts(0)));
        assert!(!mgr.is_cached("KTLX", &ts(0)));
    }

    /// The data a frame renders is named by the target it is rendered for, because
    /// that is where the geometry came from.
    #[test]
    fn a_frames_data_is_looked_up_under_its_targets_site() {
        let mut mgr = LoopDownloadManager::new();
        let ktlx = scan_with_sweeps(&[0.5]);
        mgr.cache_scan("KTLX", ts(0), Arc::clone(&ktlx));

        let found = frame_data(&mgr, &target("KTLX", 0.5), ts(0)).expect("KTLX's own scan");
        match found {
            LoopFrameData::Volume(scan) => assert!(Arc::ptr_eq(&scan, &ktlx)),
            LoopFrameData::Products(_) => panic!("reflectivity is a Level II product"),
        }
        assert!(
            frame_data(&mgr, &target("KOUN", 0.5), ts(0)).is_none(),
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

        assert_eq!(
            sweep.rendered, 1.4,
            "the tilt the image depicts — not the 0.5 selection"
        );
        assert_eq!(
            sweep.own,
            Some(0.5),
            "what this loop's own scan resolves that selection to"
        );
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

        let (_finished, status) = finish_then_acquire(|| ctx.end_pass(), |_| SurfaceStatus::Lost);

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

/// What `apply_render_to_pane` does with a finished image beyond placing it.
///
/// Reached by building an `App` — see `app::tests::headless` — with the
/// platform double standing in for the OS and a bare `egui::Context` for the
/// renderer. The upload is genuinely done here: `Context::load_texture` needs no
/// device, no surface and no window, so the only thing that ever blocked this
/// was `App::new`'s wgpu instance.
#[cfg(test)]
mod stamping_tests {
    use super::*;
    use crate::platform_double::TestBridge;
    use nexrad_level3::model::{Level3Message, MessageHeader, ProductDescriptionBlock};
    use rustdar_radar::level3::{Level3Product, ProductStamp};
    use rustdar_radar::types::{RadarProduct, ScanInfo};

    /// A radar whose Level III objects the pane below is showing.
    pub(super) const SITE: &str = "KMPX";
    /// The product carried through — any Level III product will do, and
    /// storm-relative velocity no longer is one.
    const PRODUCT: RadarProduct = RadarProduct::EchoTops;

    /// The smallest Level III object `nearest_tilt` will consider: it reads
    /// the elevation off the PDB and nothing else.
    pub(super) fn tilt(elevation_tenths: i16, key: &str) -> Level3Product {
        Level3Product {
            message: Level3Message {
                header: MessageHeader {
                    message_code: 135,
                    date_of_message: 20661,
                    time_of_message: 7108,
                    message_length: 0,
                    source_id: 0,
                    destination_id: 0,
                    number_of_blocks: 3,
                },
                pdb: ProductDescriptionBlock {
                    block_divider: -1,
                    latitude: 44.849,
                    longitude: -93.565,
                    height: 1000,
                    product_code: 135,
                    operational_mode: 2,
                    vcp: 212,
                    sequence_number: 0,
                    volume_scan_number: 39,
                    volume_scan_date: 20661,
                    volume_scan_time: 7108,
                    generation_date: 20661,
                    generation_time: 7108,
                    product_specific_1: 0,
                    product_specific_2: 0,
                    elevation_number: 1,
                    product_specific_3: elevation_tenths,
                    thresholds: [0u16; 16],
                    product_specific_47_53: [0i16; 7],
                    version: 0,
                    spot_blank: 0,
                    symbology_offset: 60,
                    graphic_offset: 0,
                    tabular_offset: 0,
                },
                symbology: None,
            },
            stamp: ProductStamp::from_key(key),
            // No render in these tests, so nothing decodes them.
            bytes: std::sync::Arc::new(Vec::new()),
        }
    }

    /// A finished render, as `poll_render_results` builds one. The pixels are
    /// blank but full size: `ColorImage::from_rgba_unmultiplied` checks the
    /// buffer against the dimensions it is given.
    fn finished(product: RadarProduct, elevation: f32) -> CachedPaneRender {
        CachedPaneRender {
            image_data: Arc::new(vec![0u8; IMAGE_SIZE * IMAGE_SIZE * 4]),
            max_range_km: 230.0,
            value_data: Arc::new(Vec::new()),
            product,
            elevation,
        }
    }

    /// The volume the fixture pane has loaded, deliberately **not** the time in
    /// the Level III key below: a pane stamped with the wrong one of the two is
    /// then a wrong value rather than a coincidence.
    fn volume_time() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
            .unwrap()
            .and_hms_opt(1, 48, 0)
            .unwrap()
    }

    /// The time `MPX_EET_2026_07_26_01_55_52` carries — seven minutes after the
    /// volume, which is what a bucket object that lagged a volume looks like.
    fn object_time() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
            .unwrap()
            .and_hms_opt(1, 55, 52)
            .unwrap()
    }

    /// An `App` with one pane on [`SITE`], far enough along that
    /// `apply_render_to_pane` will not bail out of it: the pane needs scan info
    /// for the site coordinates and the dispatcher needs a slot for the pane.
    pub(super) fn app_showing_site() -> crate::app::App {
        let mut app = crate::app::tests::headless(TestBridge::desktop());
        let site = rustdar_radar::sites::get_radar_site(SITE)
            .expect("KMPX is a real radar")
            .clone();
        app.gui.pane_mut(0).unwrap().site = SITE.to_string();
        app.gui.set_scan_info_for_pane(
            0,
            ScanInfo {
                site,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![PRODUCT],
                product_elevations: std::collections::HashMap::new(),
                status: String::new(),
            },
        );
        app.render.ensure_pane_count(1);
        app
    }

    /// Placing an image also dates it, with the time of the data *behind that
    /// image*.
    ///
    /// `latest_key` falls back to the previous UTC day, so a site that went down
    /// yesterday serves an object most of a day old while the Level II scan line
    /// beside it looks current. The data line is the only thing that says so, and
    /// nothing between the render arriving and the pane being drawn would notice
    /// this call going missing — the pane would simply keep the time it last had.
    #[test]
    fn a_placed_render_dates_the_pane_it_lands_on() {
        let ctx = egui::Context::default();
        let mut app = app_showing_site();
        app.render.cache_level3(
            "EET".to_string(),
            SITE.to_string(),
            tilt(5, "MPX_EET_2026_07_26_01_55_52"),
        );

        app.apply_render_to_pane(&ctx, 0, &finished(PRODUCT, 0.5));

        assert_eq!(
            app.gui.pane(0).unwrap().data_time,
            Some(object_time()),
            "a Level III pane must report its own object's time, not the volume's",
        );

        // …and the image really did land, so the assertion above is about a
        // frame the user would be looking at rather than an early return.
        let pane = app.gui.pane_mut(0).unwrap();
        assert!(
            pane.overlay_cache_mut(rustdar_overlays::render::overlay_state::OverlayKind::Radar)
                .current
                .is_some(),
            "precondition: no texture was placed at all",
        );
    }

    /// Switching datasource replaces the time rather than leaving the old one.
    ///
    /// The assignment is unconditional for this reason: leaving the Level III
    /// object's time in place would caption a field derived from the volume with
    /// the age of one it has nothing to do with. And the replacement is the
    /// volume's own time, not nothing — a product whose age line disappears is a
    /// product the user can identify as coming from somewhere else, which is the
    /// asymmetry this line no longer has.
    #[test]
    fn switching_datasource_redates_the_pane_rather_than_undating_it() {
        let ctx = egui::Context::default();
        let mut app = app_showing_site();
        app.render.cache_level3(
            "EET".to_string(),
            SITE.to_string(),
            tilt(5, "MPX_EET_2026_07_26_01_55_52"),
        );

        app.apply_render_to_pane(&ctx, 0, &finished(PRODUCT, 0.5));
        assert_eq!(
            app.gui.pane(0).unwrap().data_time,
            Some(object_time()),
            "precondition: dated from the bucket object",
        );

        app.apply_render_to_pane(&ctx, 0, &finished(RadarProduct::Reflectivity, 0.5));

        assert_eq!(
            app.gui.pane(0).unwrap().data_time,
            Some(volume_time()),
            "a volume-derived product reports the volume's time — the same line, \
             filled in the same way",
        );
    }

    /// Placing an image also records **what it depicts**, so a pane can tell when
    /// its pixels are not the selection its labels describe.
    ///
    /// Written into the texture's own `RadarTextureMeta`, which is what makes
    /// `PaneState::stale_image_on_screen` impossible to leave behind: the pair is
    /// placed together and dropped together. Nothing between the render arriving
    /// and the pane being drawn would notice this assignment going missing — the
    /// pane would simply never report a mismatch, and would go on captioning one
    /// product's image with another's name, which is the defect.
    ///
    /// Both datasources, in both directions, from the one call: the product on the
    /// render is the only thing that differs, so a Level II and a Level III image
    /// cannot be described differently. This is also the contract
    /// `InputHarness::place_radar_image` imitates.
    #[test]
    fn a_placed_render_describes_what_it_depicts() {
        let ctx = egui::Context::default();
        let mut app = app_showing_site();
        assert!(
            PRODUCT.is_level3() && !RadarProduct::Reflectivity.is_level3(),
            "one product from each datasource",
        );

        // A Level III image under a Level II selection.
        app.gui.pane_mut(0).unwrap().selected_product = RadarProduct::Reflectivity;
        app.apply_render_to_pane(&ctx, 0, &finished(PRODUCT, 0.5));
        assert_eq!(
            app.gui.pane(0).unwrap().stale_image_on_screen(),
            Some((PRODUCT, 0.5)),
            "the placed image's own product and sweep, reported so the pane can \
             say the label is ahead of the pixels",
        );

        // The matching render lands: nothing to report.
        app.gui.pane_mut(0).unwrap().selected_product = PRODUCT;
        assert_eq!(
            app.gui.pane(0).unwrap().stale_image_on_screen(),
            None,
            "the image is the selection now",
        );

        // And the other way round — a Level II image under a Level III selection,
        // through the same call.
        app.apply_render_to_pane(&ctx, 0, &finished(RadarProduct::Reflectivity, 0.5));
        assert_eq!(
            app.gui.pane(0).unwrap().stale_image_on_screen(),
            Some((RadarProduct::Reflectivity, 0.5)),
        );
    }
}

/// A restored image describes itself too.
///
/// `restore_cached_render` is the one path that puts a radar texture on screen
/// without going through `apply_render_to_pane`: after suspend/resume or surface
/// loss it re-uploads the cached pixels rather than re-rendering, and so builds
/// its own [`rustdar_egui::overlay_cache::RadarTextureMeta`]. A pane switched
/// while the app was away would otherwise come back showing the old product with
/// nothing saying so — the exact state the pending notice exists for, reached by
/// the one route around it.
///
/// Read off the source for the reason `frame_build_order_tests` gives: the
/// function unwraps an `AppState`, which is a wgpu device, a surface and a window,
/// none of which a headless `App` has, so it returns before its first statement.
#[cfg(test)]
mod restore_describes_its_image_tests {
    /// The body of `restore_cached_render`.
    fn restore_body() -> &'static str {
        let (_, rest) = include_str!("app_render.rs")
            .split_once("pub(super) fn restore_cached_render(")
            .expect("restore_cached_render is no longer a method here");
        rest.split_once("\n    }")
            .map(|(body, _)| body)
            .expect("restore_cached_render has no recognisable body")
    }

    #[test]
    fn a_restored_image_still_says_what_it_depicts() {
        let body = restore_body();
        let meta = body
            .find("RadarTextureMeta {")
            .expect("restore_cached_render no longer describes the texture it places");
        let fields = &body[meta..];
        for field in ["product,", "elevation,"] {
            assert!(
                fields.contains(field),
                "a restored image carries no `{field}`, so a pane switched while \
                 suspended comes back showing the old product with nothing saying \
                 so; `stale_image_on_screen` reads this metadata and nothing else",
            );
        }
        // The values come from the *cached render*, not from the pane's live
        // selection — which is the whole distinction the notice rests on.
        for source in [
            "let product = cached.product;",
            "let elevation = cached.elevation;",
        ] {
            assert!(
                body.contains(source),
                "`{source}` is gone: the restored image would be described by \
                 whatever the pane has selected rather than by what it depicts",
            );
        }
    }
}

/// What the loop timer does with a playback speed no slider could have set.
#[cfg(test)]
mod loop_interval_tests {
    use super::loop_interval;
    use crate::constants::{DEFAULT_LOOP_SPEED_FPS, MAX_LOOP_SPEED_FPS, MIN_LOOP_SPEED_FPS};

    /// A stored speed the UI cannot produce must not take the app down.
    ///
    /// Every one of these panics `Duration::from_secs_f32` — zero and the
    /// negatives through the reciprocal, the rest directly — and it panics in
    /// `advance_loop_playback`, which runs on every frame. There is no getting
    /// out of that: the frame that would let the user fix the slider is the
    /// frame that dies. The values are all reachable, because the save-side
    /// guard checks only `is_finite` and the load assigns whatever it finds.
    #[test]
    fn a_speed_no_slider_could_have_set_still_yields_a_frame_interval() {
        for fps in [0.0, -1.0, -0.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let interval = loop_interval(fps);
            assert!(
                interval.as_secs_f32().is_finite() && !interval.is_zero(),
                "{fps} produced {interval:?}",
            );
        }
    }

    /// And the speeds the slider *can* set are honoured exactly.
    ///
    /// A clamp that quietly rounded every speed to one value would satisfy the
    /// test above and make the setting inert.
    #[test]
    fn a_speed_the_slider_can_set_is_used_as_it_stands() {
        assert_eq!(loop_interval(5.0).as_secs_f32(), 0.2);
        assert_eq!(
            loop_interval(MIN_LOOP_SPEED_FPS).as_secs_f32(),
            1.0 / MIN_LOOP_SPEED_FPS,
        );
        assert_eq!(
            loop_interval(MAX_LOOP_SPEED_FPS).as_secs_f32(),
            1.0 / MAX_LOOP_SPEED_FPS,
        );
        assert_eq!(
            loop_interval(f32::NAN),
            loop_interval(DEFAULT_LOOP_SPEED_FPS),
            "a value that is not a number falls back to the UI's own default",
        );
    }
}

/// The Level III half of the loop: pairing a bucket object to each frame's volume,
/// what a gap does, and what happens when a pane retargets across the datasource
/// line mid-loop.
///
/// Nothing here touches the network. The pairing itself is
/// `rustdar_radar::level3`'s, tested against synthetic keys and PDBs there; what
/// these tests pin is the frontend's half — which frames get queued, what a
/// resolved-to-nothing frame does to playback, and that a Level III frame reaches
/// the render dispatcher through exactly the path a Level II one does.
#[cfg(test)]
mod loop_level3_tests {
    use super::*;
    use crate::loop_downloads::{L3FrameState, LoopDownloadManager};
    use nexrad_level3::model::{Level3Message, MessageHeader, ProductDescriptionBlock};
    use rustdar_egui::pane::{LoopFrame, LoopPhase, LoopPlaybackState};
    use rustdar_radar::archive::Identifier;
    use rustdar_radar::level3::{Level3Product, ProductStamp};
    use rustdar_radar::sites::RadarSite;
    use rustdar_radar::types::RadarProduct;

    const SITE: &str = "KTLX";
    /// Echo tops: one AWIPS code (`EET`), and the product whose loop this
    /// exercises. Its `level3_products()` is read rather than the literal, so a
    /// change to the mapping cannot leave these tests pairing a code the app no
    /// longer fetches.
    const L3: RadarProduct = RadarProduct::EchoTops;
    const L2: RadarProduct = RadarProduct::Reflectivity;

    fn ts(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            + chrono::Duration::minutes(minute as i64)
    }

    fn codes(product: RadarProduct) -> &'static [&'static str] {
        product
            .level3_products()
            .expect("a Level III product names its codes")
    }

    /// A frame plan for `n` volumes one minute apart, as `accept_scan_listing`
    /// builds one.
    fn plan(n: u32) -> crate::loop_downloads::FramePlan {
        crate::loop_downloads::FramePlan::new(
            SITE.to_string(),
            (0..n)
                .map(|i| {
                    (
                        ts(i),
                        Identifier::new(format!("KTLX20240101_00{i:02}00_V06")),
                    )
                })
                .collect(),
        )
    }

    /// A decoded object whose PDB reports `elevation_tenths / 10` degrees. Only
    /// the fields the loop reads carry anything — no symbology, since nothing here
    /// renders.
    fn object(elevation_tenths: i16) -> Arc<Level3Product> {
        let pdb = ProductDescriptionBlock {
            block_divider: -1,
            latitude: 35.33,
            longitude: -97.27,
            height: 1200,
            product_code: 135,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 1,
            volume_scan_date: 19723,
            volume_scan_time: 0,
            generation_date: 19723,
            generation_time: 90,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number: 1,
            product_specific_3: elevation_tenths,
            thresholds: [0; 16],
            product_specific_47_53: [0; 7],
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        };
        Arc::new(Level3Product {
            message: Level3Message {
                header: MessageHeader {
                    message_code: 135,
                    date_of_message: 19723,
                    time_of_message: 90,
                    message_length: 0,
                    source_id: 0,
                    destination_id: 0,
                    number_of_blocks: 3,
                },
                pdb,
                symbology: None,
            },
            stamp: ProductStamp::from_key("TLX_EET_2024_01_01_00_01_30"),
            bytes: Arc::new(Vec::new()),
        })
    }

    /// A loop on [`SITE`] with `n` frames, retargeted to `product`.
    fn loop_for(product: RadarProduct, n: u32) -> LoopPlaybackState {
        let mut ls = LoopPlaybackState::new_for_loop(
            3600,
            &RadarSite {
                name: SITE,
                lat: 35.33,
                lon: -97.27,
                elev: None,
            },
        );
        ls.phase = LoopPhase::Rendering;
        ls.frames = (0..n)
            .map(|i| LoopFrame {
                timestamp: ts(i),
                texture: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        ls.retarget_renders(product, 0.5);
        ls
    }

    /// The core of the feature. A Level III loop's frames are the *same* volume
    /// timeline a Level II loop's are — which is what keeps a mixed set of panes
    /// animating in step, since they share one clock — but what each frame needs
    /// downloaded is a bucket object per AWIPS code, not the ~10 MB archive volume.
    ///
    /// Both halves are asserted. Queuing the pairings without dropping the volume
    /// queue would work, animate correctly, and quietly spend a volume download per
    /// frame on bytes no render reads.
    #[test]
    fn a_level3_loop_queues_a_pairing_per_frame_and_no_volume_downloads() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan(4));

        assert!(mgr.plan_downloads_for(0, L3), "the first plan is a change");

        let pending = mgr
            .extract_pending_l3(0)
            .expect("a Level III product owes pairings");
        assert_eq!(pending.site, SITE, "the site travels with the queue");
        assert_eq!(pending.product, L3);
        assert_eq!(
            pending.queue.len(),
            4 * codes(L3).len(),
            "one pairing per frame per AWIPS code",
        );
        assert_eq!(
            pending.queue.front().map(|(t, c)| (*t, c.clone())),
            Some((ts(0), codes(L3)[0].to_string())),
            "oldest volume first, as the frame list is ordered",
        );
        assert!(
            mgr.extract_pending(0).is_none(),
            "a Level III loop must not download the volumes it never reads",
        );
    }

    /// A Level II loop is the mirror image: volumes queued, no pairings.
    #[test]
    fn a_level2_loop_queues_its_volumes_and_no_pairings() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan(3));
        assert!(mgr.plan_downloads_for(0, L2));

        let pending = mgr.extract_pending(0).expect("volumes are owed");
        assert_eq!(pending.site, SITE);
        assert_eq!(pending.queue.len(), 3);
        assert!(mgr.extract_pending_l3(0).is_none());
    }

    /// Switching product mid-loop must re-derive the queues, in both directions.
    /// The frame list does not change — the loop's timeline is the volumes either
    /// way — so without this the frames would sit waiting on data nothing is
    /// fetching, and `settle_loop_phase` would abandon the loop.
    #[test]
    fn retargeting_across_the_datasource_line_requeues_the_frames() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan(2));

        assert!(mgr.plan_downloads_for(0, L2));
        assert!(
            mgr.plan_downloads_for(0, L3),
            "moving to Level III is a change",
        );
        assert!(
            mgr.extract_pending(0).is_none(),
            "the volume queue went with the old product",
        );
        let l3 = mgr.extract_pending_l3(0).expect("pairings queued");
        assert_eq!(l3.queue.len(), 2 * codes(L3).len());
        mgr.insert_pending_l3(0, l3);

        assert!(mgr.plan_downloads_for(0, L2), "and back again");
        assert_eq!(
            mgr.extract_pending(0).map(|p| p.queue.len()),
            Some(2),
            "the volumes are queued from the same plan, with no re-listing",
        );
        assert!(mgr.extract_pending_l3(0).is_none());
    }

    /// An unchanged product must not re-derive anything. `dispatch_loop_renders`
    /// asks on every retarget, and an elevation change is a retarget — rebuilding
    /// both queues every time the user nudges a tilt would re-queue every frame
    /// that had already been fetched.
    #[test]
    fn an_unchanged_product_requeues_nothing() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan(2));
        assert!(mgr.plan_downloads_for(0, L3));
        assert!(
            !mgr.plan_downloads_for(0, L3),
            "the same product is not a change",
        );
        // And a pane with no plan has nothing to derive from.
        assert!(!mgr.plan_downloads_for(7, L3));
    }

    /// The three answers a frame's Level III data can have, and the one that only
    /// exists because gaps are normal: a volume the site generated no object for is
    /// **Absent**, cached as such, and never asked about again.
    ///
    /// The `Absent` case is what a re-pairing loop would otherwise cost: up to
    /// `PAIRING_CANDIDATES` object fetches per dispatch pass, forever.
    #[test]
    fn a_frames_level3_state_distinguishes_ready_absent_and_pending() {
        let mut mgr = LoopDownloadManager::new();
        let code = codes(L3)[0];

        assert_eq!(
            mgr.l3_frame_state(SITE, L3, &ts(0)),
            L3FrameState::Pending,
            "nothing paired yet",
        );
        assert!(!mgr.l3_is_resolved(SITE, code, &ts(0)));

        mgr.cache_l3_product(SITE, code, ts(0), Some(object(0)));
        assert_eq!(mgr.l3_frame_state(SITE, L3, &ts(0)), L3FrameState::Ready);
        assert!(mgr.l3_is_resolved(SITE, code, &ts(0)));

        mgr.cache_l3_product(SITE, code, ts(1), None);
        assert_eq!(
            mgr.l3_frame_state(SITE, L3, &ts(1)),
            L3FrameState::Absent,
            "the site generated no object for that volume",
        );
        assert!(
            mgr.l3_is_resolved(SITE, code, &ts(1)),
            "a gap is an answer, so nothing re-pairs it",
        );
    }

    /// Another site's objects are never this frame's, even at the same volume time
    /// — the same rule the volume cache follows, and for the same reason: two
    /// sites' volume starts land on the same second often enough, and an image
    /// drawn from one radar's object at another's coordinates looks entirely
    /// consistent.
    #[test]
    fn a_paired_object_is_never_taken_from_another_site() {
        let mut mgr = LoopDownloadManager::new();
        let code = codes(L3)[0];
        mgr.cache_l3_product(SITE, code, ts(0), Some(object(0)));

        assert_eq!(mgr.l3_frame_state(SITE, L3, &ts(0)), L3FrameState::Ready);
        assert_eq!(
            mgr.l3_frame_state("KOUN", L3, &ts(0)),
            L3FrameState::Pending,
            "KOUN has paired nothing",
        );
        assert!(mgr.l3_frame_products("KOUN", L3, &ts(0)).is_none());
    }

    /// A product needs *every* one of its AWIPS codes before a frame is ready, and
    /// is a gap as soon as any one of them is missing.
    ///
    /// Every Level III product rustdar draws today names one code, so for them this
    /// reduces to the single-code case above. It is written over
    /// `level3_products()` rather than over a literal because that is about to stop
    /// being true: VIL density is being rebuilt as `DVL ÷ EET`, two codes paired to
    /// one volume, and the moment it lands this test carries the all-or-nothing
    /// rule without being touched.
    #[test]
    fn a_frame_needs_every_one_of_its_products_codes() {
        for product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
            let all = codes(*product);
            let mut mgr = LoopDownloadManager::new();
            // All but the last code paired.
            for code in &all[..all.len() - 1] {
                mgr.cache_l3_product(SITE, code, ts(0), Some(object(0)));
            }
            assert_eq!(
                mgr.l3_frame_state(SITE, *product, &ts(0)),
                L3FrameState::Pending,
                "{} was ready without {}",
                product.name(),
                all[all.len() - 1],
            );
            assert!(
                mgr.l3_frame_products(SITE, *product, &ts(0)).is_none(),
                "{} must not render against a missing input",
                product.name(),
            );

            mgr.cache_l3_product(SITE, all[all.len() - 1], ts(0), Some(object(0)));
            assert_eq!(
                mgr.l3_frame_state(SITE, *product, &ts(0)),
                L3FrameState::Ready,
            );
            assert_eq!(
                mgr.l3_frame_products(SITE, *product, &ts(0))
                    .map(|p| p.len()),
                Some(all.len()),
                "{} renders from all of its codes, in order",
                product.name(),
            );
        }
    }

    /// The sweep a Level III frame is rendered at is its **object's own** PDB
    /// elevation, not the pane's selection.
    ///
    /// That is what the image actually depicts, and it is what makes the sibling
    /// broadcast's sweep comparison mean anything: two panes resolving the same
    /// `(site, code, volume)` share one cache entry and so one angle, while a
    /// comparison against the selection would agree for every object regardless of
    /// which cut it is. The fixture's object sits at 1.4° against a 0.5° selection
    /// so the two cannot be confused.
    #[test]
    fn a_level3_frames_sweep_is_its_objects_own_elevation() {
        let mut mgr = LoopDownloadManager::new();
        let code = codes(L3)[0];
        let tgt = RenderTarget::new(SITE, L3, 0.5);

        assert!(
            matches!(frame_sweep(&mgr, &tgt, ts(0)), FrameSweep::Pending),
            "nothing paired yet, so the frame waits rather than being retired",
        );

        mgr.cache_l3_product(SITE, code, ts(0), Some(object(14)));
        match frame_sweep(&mgr, &tgt, ts(0)) {
            FrameSweep::At(sweep) => assert_eq!(sweep, 1.4),
            other => panic!("expected a renderable frame, got {:?}", DebugSweep(other)),
        }
    }

    /// A gap retires its frame, exactly as a Level II volume carrying no sweep for
    /// the product does — and by the same route, so playback steps over it instead
    /// of flashing an empty pane or raising an error.
    #[test]
    fn a_gap_makes_its_frame_unrenderable_rather_than_pending() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_l3_product(SITE, codes(L3)[0], ts(0), None);
        assert!(matches!(
            frame_sweep(&mgr, &RenderTarget::new(SITE, L3, 0.5), ts(0)),
            FrameSweep::Unrenderable
        ));
    }

    /// A frame's render data is resolved from the product on its own target, so a
    /// Level III frame gets objects and a Level II frame gets a volume with no
    /// caller deciding which.
    #[test]
    fn frame_data_follows_the_targets_own_product() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_l3_product(SITE, codes(L3)[0], ts(0), Some(object(0)));

        match frame_data(&mgr, &RenderTarget::new(SITE, L3, 0.5), ts(0)) {
            Some(LoopFrameData::Products(objects)) => assert_eq!(objects.len(), codes(L3).len()),
            _ => panic!("a Level III target must resolve to its objects"),
        }
        assert!(
            frame_data(&mgr, &RenderTarget::new(SITE, L2, 0.5), ts(0)).is_none(),
            "a Level II target reads the volume cache, which holds nothing here",
        );
    }

    /// Readiness has to be asked about the loop's own *product*, not only its site,
    /// and this is the failure it prevents.
    ///
    /// `render_set_settled` reads "this frame has no data yet" as settled — nothing
    /// is owed to a frame with nothing to render — and leaves the arriving half to
    /// the download check. So a Level III loop judged against the **volume** cache,
    /// which nothing fills for it, reads as fully settled the moment its pairings
    /// are dispatched: no frame has a texture, nothing is in flight, and
    /// `settle_loop_phase` concludes nothing will ever render and switches the loop
    /// off. The pane silently falls back to its static image, which is precisely
    /// "an L3 product that does not loop".
    ///
    /// Asked about the product, a paired frame *is* data-available, so the batch
    /// stays unsettled until its render lands.
    #[test]
    fn a_level3_loops_batch_settles_on_its_pairings_not_on_volumes() {
        let mut ls = loop_for(L3, 3);
        let mut mgr = LoopDownloadManager::new();
        let code = codes(L3)[0];

        // Every frame's object is paired; none has rendered.
        for i in 0..3 {
            mgr.cache_l3_product(SITE, code, ts(i), Some(object(0)));
        }
        assert!(
            !loop_batch_settled(&mgr, &ls, MAX_LOOP_RENDER_BUDGET),
            "three renderable frames and no textures: renders are owed",
        );
        // The contrast, spelled out: the same batch judged the old way — against
        // the volume cache — reads as settled, which is the abandonment above.
        assert!(
            ls.render_set_settled(MAX_LOOP_RENDER_BUDGET, |f| mgr
                .is_cached(SITE, &f.timestamp)),
            "precondition: a volume-cache check settles this batch, and the loop \
             would then be switched off with everything it needs in hand",
        );
        assert!(
            !settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET),
            "so the loop must be left in Rendering, waiting on its renders",
        );
        assert_eq!(ls.phase, LoopPhase::Rendering);

        // Caching the volumes changes nothing either way: this loop never reads them.
        for i in 0..3 {
            mgr.cache_scan(SITE, ts(i), volume());
        }
        assert!(!loop_batch_settled(&mgr, &ls, MAX_LOOP_RENDER_BUDGET));

        // One rendered, one gap, one rendered: the batch settles and the loop is
        // promoted rather than abandoned — the gap is not held against it.
        ls.frames[0].texture = Some(image());
        ls.frames[2].texture = Some(image());
        mgr.cache_l3_product(SITE, code, ts(1), None);
        ls.frames[1].render_failed = true;
        assert!(loop_batch_settled(&mgr, &ls, MAX_LOOP_RENDER_BUDGET));
        assert!(!settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET));
        assert_eq!(ls.phase, LoopPhase::Ready);
    }

    /// A pairing in flight holds the loop open, the way a volume download does.
    /// Without it, a Level III loop is abandoned on the pass right after its first
    /// batch is dispatched: no frame has a texture yet, nothing is *rendering*, and
    /// the only thing outstanding is on the other datasource's in-flight set.
    #[test]
    fn a_pairing_in_flight_keeps_the_loop_from_being_abandoned() {
        let mut ls = loop_for(L3, 3);
        let mut mgr = LoopDownloadManager::new();
        mgr.mark_l3_in_flight(SITE, codes(L3)[0], ts(0));

        assert!(!settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET));
        assert_eq!(ls.phase, LoopPhase::Rendering, "still working");

        // Undispatched pairings hold it open too — the queue, not just the marks.
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan(3));
        mgr.plan_downloads_for(0, L3);
        assert!(!mgr.is_pane_done(0), "pairings are still owed");
        assert!(!settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET));
        assert_eq!(ls.phase, LoopPhase::Rendering);
    }

    /// A loop every one of whose frames is a gap is switched off, so the pane falls
    /// back to its static image rather than animating nothing. The same dead end a
    /// Level II loop with no renderable frame reaches, by the same route.
    #[test]
    fn a_level3_loop_that_is_all_gaps_is_switched_off() {
        let mut ls = loop_for(L3, 3);
        let mut mgr = LoopDownloadManager::new();
        for i in 0..3 {
            mgr.cache_l3_product(SITE, codes(L3)[0], ts(i), None);
            ls.frames[i as usize].render_failed = true;
        }

        assert!(
            settle_loop_phase(&mgr, 0, &mut ls, MAX_LOOP_RENDER_BUDGET),
            "the caller has to release this pane's loop state",
        );
        assert!(!ls.is_active());
    }

    /// A pane whose loop has never dispatched has no product to judge its frames
    /// by, so nothing is settled — rather than everything being, which would
    /// promote a loop with no frames rendered.
    #[test]
    fn a_loop_before_its_first_dispatch_has_settled_nothing() {
        let mut ls = loop_for(L3, 2);
        ls.rendered_for = None;
        let mgr = LoopDownloadManager::new();
        assert!(!loop_batch_settled(&mgr, &ls, MAX_LOOP_RENDER_BUDGET));
    }

    /// The days a Level III listing covers come from the loop's own frames, not
    /// from wall clock: a loop rebuilt around a historic scan pairs against
    /// yesterday's prefix, and listing today's would find nothing — which is
    /// indistinguishable from "the site served no objects" and would retire every
    /// frame as a gap.
    #[test]
    fn the_listed_days_come_from_the_frames_and_span_midnight() {
        let code = codes(L3)[0].to_string();
        let jan2 = chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
        let jan1 = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let dec31 = chrono::NaiveDate::from_ymd_opt(2023, 12, 31).unwrap();

        // Frames inside one UTC day: that day plus the one before, since an
        // object for an early volume can sit under the previous prefix.
        let same_day: VecDeque<_> = [(ts(10), code.clone()), (ts(20), code.clone())]
            .into_iter()
            .collect();
        assert_eq!(pairing_days_for_frames(&same_day), vec![jan1, dec31]);

        // A window that crosses 00Z lists all three, each once.
        let across: VecDeque<_> = [
            (ts(23 * 60 + 50), code.clone()),
            (ts(24 * 60 + 5), code.clone()),
        ]
        .into_iter()
        .collect();
        assert_eq!(pairing_days_for_frames(&across), vec![jan1, dec31, jan2]);

        assert!(
            pairing_days_for_frames(&VecDeque::new()).is_empty(),
            "nothing left to pair, nothing to list",
        );
    }

    /// A key listing is claimed once. Two panes looping one site want the same
    /// keys, and the listing is the expensive half of a pairing — a round-trip per
    /// UTC day, against a few hundred kilobytes of object per pairing.
    #[test]
    fn a_key_listing_is_claimed_once_and_shared() {
        let mut mgr = LoopDownloadManager::new();
        let code = codes(L3)[0];

        assert!(mgr.claim_l3_listing(SITE, code), "the first caller owes it");
        assert!(
            !mgr.claim_l3_listing(SITE, code),
            "the second waits on the first",
        );
        assert!(
            mgr.claim_l3_listing("KOUN", code),
            "another site is another listing",
        );
        assert!(mgr.l3_keys(SITE, code).is_none(), "not landed yet");

        mgr.cache_l3_keys(SITE, code, vec!["TLX_EET_2024_01_01_00_01_30".to_string()]);
        assert_eq!(mgr.l3_keys(SITE, code).map(|k| k.len()), Some(1));
        assert!(
            !mgr.claim_l3_listing(SITE, code),
            "and is not listed a second time once cached",
        );
    }

    /// An empty listing is an answer, not a failure to record. Discarded, the
    /// pairings would wait on a listing that already happened and the loop would
    /// hang in `Rendering`; cached, every frame pairs to a gap and the loop retires
    /// to the pane's static image.
    #[test]
    fn an_empty_key_listing_is_cached_as_the_answer() {
        let mut mgr = LoopDownloadManager::new();
        let code = codes(L3)[0];
        assert!(mgr.claim_l3_listing(SITE, code));
        mgr.cache_l3_keys(SITE, code, Vec::new());

        assert_eq!(
            mgr.l3_keys(SITE, code).map(|k| k.len()),
            Some(0),
            "an empty list is stored, so the pairings can proceed and find nothing",
        );
        assert!(!mgr.claim_l3_listing(SITE, code));
    }

    /// Switching site drops every trace of the Level III half too. A pairing left
    /// behind would land against a loop that no longer exists, and a key listing
    /// left behind would be re-used for a site it was never made for — which
    /// `clear_all`'s whole job is to prevent.
    #[test]
    fn clear_all_empties_the_level3_state_as_well() {
        let mut mgr = LoopDownloadManager::new();
        let code = codes(L3)[0];
        mgr.set_plan(0, plan(2));
        mgr.plan_downloads_for(0, L3);
        mgr.cache_l3_keys(SITE, code, vec!["TLX_EET_2024_01_01_00_01_30".to_string()]);
        mgr.cache_l3_product(SITE, code, ts(0), Some(object(0)));
        mgr.mark_l3_in_flight(SITE, code, ts(1));
        assert!(!mgr.is_pane_done(0), "precondition: pairings are owed");

        mgr.clear_all();

        assert!(mgr.is_pane_done(0));
        assert!(mgr.pending_l3_pane_indices().is_empty());
        assert!(mgr.l3_keys(SITE, code).is_none());
        assert!(!mgr.l3_is_resolved(SITE, code, &ts(0)));
        assert!(!mgr.l3_is_in_flight(SITE, code, &ts(1)));
        // And the plan is gone, so nothing can re-derive a queue from the site the
        // pane has just left.
        assert!(!mgr.plan_downloads_for(0, L3));
    }

    /// The two queues are reported by two separate index lists, which is why a
    /// completion drain has to iterate both.
    ///
    /// One concurrency budget serves them, and each drain is the only thing that
    /// frees a slot. A drain that re-dispatched only its own kind starves the other:
    /// with the budget full of volume downloads nothing re-triggers the pairing
    /// queue, because no pairing was ever spawned to complete. That is what
    /// `dispatch_freed_loop_slots` exists to prevent, and this pins the shape it
    /// depends on — neither list can stand in for the other.
    #[test]
    fn the_two_queues_are_reported_separately_so_both_must_be_dispatched() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan(2));
        mgr.plan_downloads_for(0, L2);
        mgr.set_plan(1, plan(2));
        mgr.plan_downloads_for(1, L3);

        assert_eq!(mgr.pending_pane_indices(), vec![0], "pane 0 owes volumes");
        assert_eq!(
            mgr.pending_l3_pane_indices(),
            vec![1],
            "pane 1 owes pairings, and iterating the volume list alone never \
             reaches it",
        );
        assert!(!mgr.is_pane_done(0));
        assert!(!mgr.is_pane_done(1));
    }

    /// Switching the loop off releases both queues and the plan behind them.
    #[test]
    fn removing_a_panes_pending_work_takes_both_queues_and_the_plan() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan(2));
        mgr.plan_downloads_for(0, L3);
        assert!(!mgr.is_pane_done(0));

        mgr.remove_pending(0);

        assert!(mgr.is_pane_done(0));
        assert!(
            !mgr.plan_downloads_for(0, L2),
            "the plan went with the queues, so nothing refills from it",
        );
    }

    /// `FrameSweep` is not `Debug` in production — nothing logs it — so the
    /// panic message above wraps it.
    struct DebugSweep(FrameSweep);

    impl std::fmt::Debug for DebugSweep {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self.0 {
                FrameSweep::At(a) => write!(f, "At({a})"),
                FrameSweep::Unrenderable => write!(f, "Unrenderable"),
                FrameSweep::Pending => write!(f, "Pending"),
            }
        }
    }

    /// A Level II volume with one reflectivity sweep, so the volume cache holds
    /// something real when a test needs to prove it is *not* being read.
    fn volume() -> Arc<nexrad_model::data::Scan> {
        use nexrad_model::data::{
            MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
        };
        let radial = Radial::new(
            0,
            0,
            0.0,
            1.0,
            RadialStatus::ElevationStart,
            1,
            0.5,
            Some(MomentData::from_fixed_point(
                1,
                0,
                250,
                8,
                2.0,
                66.0,
                vec![0],
            )),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        Arc::new(Scan::new(
            VolumeCoveragePattern::new(
                212,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                Vec::new(),
            ),
            vec![Sweep::new(1, vec![radial])],
        ))
    }

    /// A 1x1 texture standing in for a rendered frame. Nothing here reads pixels.
    fn image() -> rustdar_egui::pane::RadarImageData {
        let ctx = egui::Context::default();
        rustdar_egui::pane::RadarImageData {
            texture: ctx.load_texture(
                "test",
                egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
                egui::TextureOptions::NEAREST,
            ),
            lat: 35.33,
            lon: -97.27,
            max_range_km: 100.0,
            value_data: Arc::new(Vec::new()),
        }
    }
}

/// The order one frame is assembled in.
///
/// `setup_egui_frame` unwraps an `AppState`, which is a wgpu device, a surface
/// and a window — none of which exist here — so the sequence can only be read
/// off the source, the same handle `handle_input_events` and `begin_frame` are
/// pinned by.
#[cfg(test)]
mod frame_build_order_tests {
    /// The body of `setup_egui_frame`.
    fn setup_body() -> &'static str {
        let (_, rest) = include_str!("app_render.rs")
            .split_once("fn setup_egui_frame(")
            .expect("setup_egui_frame is no longer a method here");
        rest.split_once("\n    }")
            .map(|(body, _)| body)
            .expect("setup_egui_frame has no recognisable body")
    }

    /// Nothing a poller applies may land after the frame has been laid out.
    ///
    /// A result applied afterwards misses the frame it was applied to, and
    /// nothing schedules the one that would show it: the re-arm at the end of
    /// `handle_redraw` covers a render still in flight, auto-poll and an active
    /// loop, and the last result of a batch is none of those. With auto-poll off
    /// it sat there, applied and unpresented, until a mouse move repainted.
    #[test]
    fn every_poller_runs_before_the_frame_is_laid_out() {
        let body = setup_body();
        let laid_out = body
            .find("self.gui.ui(")
            .expect("setup_egui_frame no longer lays out a frame");

        for poller in [
            "self.poll_render_results(",
            "self.poll_level3_results(",
            "self.poll_overlay_render_results(",
            "self.poll_loop_scan_list_results(",
            "self.poll_loop_scan_download_results(",
            // The Level III loop's two stages, listed here for the same reason
            // as the Level II pair: a pairing that lands after layout is a frame
            // that stays blank until something unrelated repaints.
            "self.poll_loop_l3_list_results(",
            "self.poll_loop_l3_fetch_results(",
            "self.poll_loop_render_results(",
            // A section is the slowest thing this app produces, so it is the
            // one most likely to be the last result of a batch — the exact case
            // the re-arm at the end of `handle_redraw` does not cover.
            "self.poll_section_results(",
        ] {
            let at = body
                .find(poller)
                .unwrap_or_else(|| panic!("{poller} is no longer called from setup_egui_frame"));
            assert!(
                at < laid_out,
                "{poller} applies its results after the frame has been laid \
                 out, so the last of a batch is not on screen until something \
                 unrelated repaints",
            );
        }
    }
}

/// What `poll_level3_results` does with a channel holding more than one answer.
///
/// Built on `stamping_tests`' fixtures: an `App` with one pane on a real radar,
/// and the smallest Level III object the pipeline will accept.
#[cfg(test)]
mod level3_poll_tests {
    use super::stamping_tests::{SITE, app_showing_site, tilt};
    use rustdar_radar::types::RadarProduct;

    /// A finished fetch of one AWIPS object, as `spawn_level3_fetches` produces
    /// one.
    ///
    /// Generation 0 is what a site nothing has re-fetched carries, so nothing
    /// here is discarded as stale. The object's contents are the same whichever
    /// code is named: what a response is *of* is decided by the code beside it,
    /// and which products that feeds is derived on arrival.
    fn landed(code: &str) -> crate::channels::Level3Response {
        crate::channels::Level3Response {
            generation: 0,
            code: code.to_string(),
            site: SITE.to_string(),
            result: Ok(tilt(5, "MPX_EET_2026_07_26_01_55_52")),
        }
    }

    /// Every Level III result queued for a frame is taken in it.
    ///
    /// One Level II scan spawns a fetch per distinct AWIPS code and they land in
    /// a burst. Taking one per frame filled the product picker an entry per
    /// redraw, and stopped filling it at all on the frame after which nothing
    /// schedules another: `handle_redraw` re-arms only for a render in flight,
    /// auto-poll, or an active loop, and a pane sitting on a finished scan is
    /// none of those.
    #[test]
    fn every_queued_level3_result_is_taken_in_the_frame_it_arrives_in() {
        let mut app = app_showing_site();
        for resp in [landed("DVL"), landed("DPR")] {
            app.channels.level3_sender.send(resp).unwrap();
        }

        app.poll_level3_results();

        let products = app
            .gui
            .get_scan_info_for_pane(0)
            .expect("the pane still has its scan info")
            .available_products
            .clone();
        for product in [
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::PrecipitationRate,
        ] {
            assert!(
                products.contains(&product),
                "{product:?} never reached the picker, so the rest of the burst \
                 is still sitting in the channel: {products:?}",
            );
        }
        assert!(
            app.channels.level3_receiver.try_recv().is_err(),
            "the frame ended with a Level III result still queued",
        );
    }

    /// **One landed object offers every product it feeds.**
    ///
    /// The picker is filled from the object's readers, not from the product a
    /// fetch was spawned "for", because there is no longer one such product: the
    /// single `DVL` fetch a poll issues is VIL's whole field *and* VIL density's
    /// numerator. Keying this off one product would leave the other permanently
    /// absent from the picker — selectable never, whatever landed.
    #[test]
    fn a_landed_object_offers_every_product_it_feeds() {
        let mut app = app_showing_site();
        app.channels.level3_sender.send(landed("DVL")).unwrap();
        app.poll_level3_results();

        let info = app
            .gui
            .get_scan_info_for_pane(0)
            .expect("the pane still has its scan info")
            .clone();
        for product in [
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::VilDensity,
        ] {
            assert!(
                info.available_products.contains(&product),
                "{product:?} reads DVL but never reached the picker: {:?}",
                info.available_products,
            );
            assert_eq!(
                info.product_elevations.get(&product).map(|e| e.as_slice()),
                Some(&[0.5f32][..]),
                "{product:?} must get the angle off the object's own PDB",
            );
        }
        // Echo tops is listed by the fixture's scan info, as `from_scan` lists
        // every Level III product the moment a volume loads — but it does not read
        // `DVL`, so this landing must not fill its angle in. That is the half of
        // the dispatch a code-keyed fetch could get wrong in the other direction:
        // an object credited to every Level III product rather than to its readers.
        assert_eq!(
            info.product_elevations.get(&RadarProduct::EchoTops),
            None,
            "a DVL object dated echo tops, which reads EET",
        );
    }

    /// The de-duplication against the live bucket: one poll, one request per
    /// object.
    ///
    /// `spawn_level3_fetches` sends exactly one `Level3Response` per fetch it
    /// spawns — success *and* failure, so a site that served nothing still
    /// answers — which makes the responses a count of the requests that were
    /// really issued. Before this, `DVL` and `EET` each arrived twice: once for
    /// the single-field product and once for VIL density.
    ///
    /// Run with:
    ///   cargo test -p rustdar-frontend --lib -- --ignored --nocapture live_a_poll
    #[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
    #[test]
    fn live_a_poll_fetches_each_object_once() {
        let want = RadarProduct::level3_codes_for(RadarProduct::all());
        let app = app_showing_site();
        app.spawn_level3_fetches(SITE);

        let mut codes: Vec<String> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while codes.len() < want.len() && std::time::Instant::now() < deadline {
            while let Ok(resp) = app.channels.level3_receiver.try_recv() {
                println!("fetched {} for {}", resp.code, resp.site);
                codes.push(resp.code);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // A duplicate would land alongside its twin, not minutes later, but give
        // the slower of a pair time to arrive before declaring there was none.
        std::thread::sleep(std::time::Duration::from_secs(10));
        while let Ok(resp) = app.channels.level3_receiver.try_recv() {
            println!("fetched {} for {} (late)", resp.code, resp.site);
            codes.push(resp.code);
        }

        codes.sort();
        assert_eq!(
            codes,
            want,
            "one request per distinct object, once each — {} requests for {} \
             objects means the poll is still walking the per-product table",
            codes.len(),
            want.len(),
        );
    }
}

/// What `poll_level3_results` does with sounding responses: the same drain and
/// fetch-generation gate as everything else on it, plus the keep-on-failure
/// rule that makes the TTL retry loop safe.
#[cfg(test)]
mod sounding_poll_tests {
    use super::stamping_tests::{SITE, app_showing_site};
    use rustdar_radar::sounding::EnvHeights;

    fn heights(h0c_km_msl: f64) -> EnvHeights {
        EnvHeights {
            h0c_km_msl,
            hm20c_km_msl: h0c_km_msl + 3.2,
            fetched_at: chrono::Utc::now(),
        }
    }

    /// As the sounding spawn in `spawn_level3_fetches` produces one.
    /// Generation 0 is what a site nothing has re-fetched carries.
    fn landed(generation: u64, heights: Option<EnvHeights>) -> crate::channels::SoundingResponse {
        crate::channels::SoundingResponse {
            generation,
            site: SITE.to_string(),
            heights,
        }
    }

    /// A landed sounding is stored per site, and a failed refetch keeps the
    /// previous entry rather than clearing it: stale environmental heights
    /// beat none, and it is precisely the entry *staying stale* that makes
    /// the TTL gate retry on the next poll.
    #[test]
    fn a_failed_refetch_keeps_the_previous_heights() {
        let mut app = app_showing_site();
        app.channels
            .sounding_sender
            .send(landed(0, Some(heights(4.2))))
            .unwrap();
        app.poll_level3_results();
        assert_eq!(
            app.render.env_heights.get(SITE).map(|h| h.h0c_km_msl),
            Some(4.2),
            "the landed sounding never reached env_heights",
        );

        app.channels.sounding_sender.send(landed(0, None)).unwrap();
        app.poll_level3_results();
        assert_eq!(
            app.render.env_heights.get(SITE).map(|h| h.h0c_km_msl),
            Some(4.2),
            "a failed refetch cleared the stored heights instead of keeping them",
        );
    }

    /// The per-site fetch-generation gate covers soundings too: a result from
    /// a superseded fetch must not land.
    #[test]
    fn a_superseded_sounding_result_is_discarded() {
        let mut app = app_showing_site();
        let superseded = app.render.next_fetch_generation(SITE);
        app.render.next_fetch_generation(SITE);

        app.channels
            .sounding_sender
            .send(landed(superseded, Some(heights(9.9))))
            .unwrap();
        app.poll_level3_results();

        assert!(
            !app.render.env_heights.contains_key(SITE),
            "a sounding from a superseded fetch generation was stored",
        );
    }
}

/// The plan-view render pipeline against a pane that has no plan view.
///
/// Four production loops dispatch, cache or broadcast a full-size plan-view
/// raster, and every one of them reads a pane's `selected_product` and
/// `selected_elevation` — flat fields a section or a volume pane carries exactly
/// as a map pane does. So none of them *fails* on a non-map pane. Each one
/// quietly buys an `IMAGE_SIZE` x `IMAGE_SIZE` RGBA image plus an equally large
/// `f32` value grid, uploads a texture, and hands it to a pane that draws none.
///
/// The four have to agree with each other as well as with reality, which is why
/// they share one predicate ([`Gui::pane_has_no_plan_view`]): a pane that is
/// dispatched to but never broadcast to, or broadcast to but never dispatched,
/// is a pane wedged with `render_in_flight` set for the life of the session.
///
/// [`Gui::pane_has_no_plan_view`]: rustdar_egui::Gui::pane_has_no_plan_view
#[cfg(test)]
mod pane_kind_render_filter_tests {
    use super::*;
    use crate::app::tests::{empty_scan, headless, two_pane_app};
    use crate::loop_downloads::LoopDownloadManager;
    use crate::platform_double::TestBridge;
    use rustdar_egui::pane::{LoopFrame, LoopPhase, LoopPlaybackState, PaneKind};
    use rustdar_overlays::render::overlay_state::OverlayKind;
    use rustdar_radar::sites::RadarSite;
    use rustdar_radar::types::RadarProduct;

    const SITE: &str = "KTLX";
    const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
    const TILT: f32 = 0.5;

    fn volume_time() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 29)
            .unwrap()
            .and_hms_opt(18, 30, 0)
            .unwrap()
    }

    /// A one-pane app on [`SITE`] with scan info, which is what
    /// `apply_render_to_pane` reads the site coordinates out of before it will
    /// place anything at all.
    fn app_on_site() -> crate::app::App {
        let mut app = headless(TestBridge::desktop());
        point_at_site(&mut app, 0);
        app.render.ensure_pane_count(1);
        app
    }

    fn point_at_site(app: &mut crate::app::App, pane_idx: usize) {
        let site = rustdar_radar::sites::get_radar_site(SITE)
            .expect("KTLX is a real radar")
            .clone();
        let mut product_elevations = std::collections::HashMap::new();
        product_elevations.insert(PRODUCT, vec![TILT]);
        let pane = app.gui.pane_mut(pane_idx).expect("pane exists");
        pane.site = SITE.to_string();
        pane.selected_product = PRODUCT;
        pane.selected_elevation = TILT;
        app.gui.set_scan_info_for_pane(
            pane_idx,
            rustdar_radar::types::ScanInfo {
                site,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![PRODUCT],
                product_elevations,
                status: String::new(),
            },
        );
    }

    /// Finished pixels, full size: `ColorImage::from_rgba_unmultiplied` checks
    /// the buffer against the dimensions it is handed, in a bare `assert_eq!`
    /// that is live in release and on the main thread.
    fn finished_pixels() -> Arc<Vec<u8>> {
        Arc::new(vec![0u8; IMAGE_SIZE * IMAGE_SIZE * 4])
    }

    fn cached_output() -> crate::render_dispatch::CachedRenderOutput {
        crate::render_dispatch::CachedRenderOutput {
            image_data: finished_pixels(),
            max_range_km: 230.0,
            value_data: Arc::new(Vec::new()),
        }
    }

    /// Whether pane `pane_idx` is holding a radar texture.
    ///
    /// The observable throughout this module: it is what `apply_render_to_pane`
    /// exists to produce, and the only thing that tells a pane which was served
    /// from one which was skipped.
    fn holds_radar_texture(app: &mut crate::app::App, pane_idx: usize) -> bool {
        app.gui
            .pane_mut(pane_idx)
            .expect("pane exists")
            .overlay_cache_mut(OverlayKind::Radar)
            .current
            .is_some()
    }

    /// A finished render landing on the channel, as a render thread posts one,
    /// and then drained by the poller.
    ///
    /// The bare `egui::Context` is the whole renderer these paths need —
    /// `Context::load_texture` wants no device, no surface and no window — which
    /// is what `stamping_tests` already relies on and why the frame's context is
    /// a parameter of the poller rather than something it reaches through
    /// `self.state` for.
    fn deliver(app: &mut crate::app::App, pane_idx: usize) {
        app.channels
            .render_sender
            .send(crate::channels::RenderResponse {
                rendered: Some(crate::channels::RenderedImage {
                    image_data: finished_pixels(),
                    max_range_km: 230.0,
                    value_data: Arc::new(Vec::new()),
                }),
                product: PRODUCT,
                elevation: TILT,
                generation: app.render.render_generation,
                pane_idx,
            })
            .expect("the receiver lives on the App");
        app.poll_render_results(&egui::Context::default());
    }

    /// `dispatch_pane_renders` skips a pane with no plan view, and skips it
    /// *before* the rendering-params branch.
    ///
    /// Driven through the render cache rather than through a spawned render, so
    /// neither a thread nor a decoded volume is needed: a cache hit is one of the
    /// two ways the `if` arm places an image, and reaching it at all proves the
    /// pane got past the guard. The map case is asserted in the same run, so this
    /// cannot be satisfied by a dispatcher that skips every pane.
    #[test]
    fn the_dispatcher_skips_a_pane_with_no_plan_view() {
        for kind in [PaneKind::CrossSection, PaneKind::Volume] {
            let mut app = app_on_site();
            app.render.cache_render(
                SITE,
                PRODUCT,
                rustdar_radar::types::RenderView::PlanView,
                TILT,
                cached_output(),
            );

            app.dispatch_pane_renders(&egui::Context::default());
            assert!(
                holds_radar_texture(&mut app, 0),
                "precondition: a map pane must take the cached render, or the \
                 assertion below is about a path nothing reaches"
            );
            assert_eq!(
                app.render.pane_render[0].last_rendered,
                Some((PRODUCT, TILT)),
                "precondition: the map pane's dispatch must have been recorded"
            );

            let mut app = app_on_site();
            app.render.cache_render(
                SITE,
                PRODUCT,
                rustdar_radar::types::RenderView::PlanView,
                TILT,
                cached_output(),
            );
            app.gui.pane_mut(0).unwrap().set_kind(kind);

            app.dispatch_pane_renders(&egui::Context::default());

            assert!(
                !holds_radar_texture(&mut app, 0),
                "{kind:?}: a full-size plan-view image was uploaded to a pane \
                 that draws none"
            );
            assert_eq!(
                app.render.pane_render[0].last_rendered, None,
                "{kind:?}: the dispatcher recorded a render for a pane it must \
                 not have served"
            );
        }
    }

    /// The sibling broadcast skips a pane with no plan view.
    ///
    /// It accepts on site + product + elevation with **no view term**, and all
    /// three match for a section pane sitting beside the map it was cut from —
    /// which is the ordinary arrangement rather than a corner case. Unfiltered,
    /// the section pane is handed the map's raster on the first render either of
    /// them triggers.
    ///
    /// Pane 1 is asserted to take the broadcast while it is still a map, so what
    /// is observed below is the filter and not a sibling that never qualified.
    #[test]
    fn the_sibling_broadcast_skips_a_pane_with_no_plan_view() {
        for kind in [PaneKind::CrossSection, PaneKind::Volume] {
            let mut app = two_pane_app(SITE, SITE);
            point_at_site(&mut app, 0);
            point_at_site(&mut app, 1);

            deliver(&mut app, 0);
            assert!(
                holds_radar_texture(&mut app, 1),
                "precondition: a map sibling on the same site, product and tilt \
                 must take the broadcast, or nothing below is being filtered"
            );

            let mut app = two_pane_app(SITE, SITE);
            point_at_site(&mut app, 0);
            point_at_site(&mut app, 1);
            app.gui.pane_mut(1).unwrap().set_kind(kind);

            deliver(&mut app, 0);

            assert!(
                holds_radar_texture(&mut app, 0),
                "{kind:?}: precondition: the origin pane is still a map and must \
                 have been served"
            );
            assert!(
                !holds_radar_texture(&mut app, 1),
                "{kind:?}: the broadcast handed a plan-view raster to a pane that \
                 draws none"
            );
        }
    }

    /// A render already in flight when its pane is converted is not placed on it.
    ///
    /// `dispatch_pane_renders` no longer starts one, but conversion happens on a
    /// frame and a render takes many, so the window is real rather than
    /// theoretical. The result still clears `render_in_flight` — that is its
    /// other job, and dropping it would wedge the pane forever — and
    /// `last_rendered` stays unset, so converting back to a map re-dispatches
    /// rather than showing nothing.
    #[test]
    fn a_render_in_flight_across_a_conversion_is_not_placed() {
        let mut app = app_on_site();
        app.render.pane_render[0].render_in_flight = true;
        app.gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);

        deliver(&mut app, 0);

        assert!(!holds_radar_texture(&mut app, 0));
        assert!(
            !app.render.pane_render[0].render_in_flight,
            "the in-flight flag was not cleared, so this pane could never ask \
             for another render as long as it lived"
        );
        assert_eq!(app.render.pane_render[0].last_rendered, None);
    }

    /// A loop on [`SITE`] with one frame per timestamp, keyed to
    /// [`PRODUCT`] at [`TILT`].
    fn active_loop(timestamps: &[chrono::NaiveDateTime]) -> LoopPlaybackState {
        let mut ls = LoopPlaybackState::new_for_loop(
            3600,
            &RadarSite {
                name: SITE,
                lat: 35.33,
                lon: -97.27,
                elev: None,
            },
        );
        ls.phase = LoopPhase::Rendering;
        ls.frames = timestamps
            .iter()
            .map(|&timestamp| LoopFrame {
                timestamp,
                texture: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        // Takes the target and reports `false`: there was nothing to discard, so
        // there is nothing for the caller to react to. What matters here is that
        // `rendered_for` is now set, which is what the dispatcher reads.
        ls.retarget_renders(PRODUCT, TILT);
        assert!(
            ls.rendered_for.is_some(),
            "precondition: a fresh loop must take its first target"
        );
        ls
    }

    /// `dispatch_loop_renders`' **first** pass skips a pane with no plan view.
    ///
    /// That pass's job is to notice the pane's product moving and re-key the whole
    /// frame list to it, which also queues a fresh download plan — for a pane
    /// nobody draws, a download queue serving nobody. So the observable is
    /// `rendered_for`: it must move for a map pane and must not move for a
    /// non-map one.
    #[test]
    fn the_first_loop_dispatch_pass_skips_a_pane_with_no_plan_view() {
        let moved_to = RadarProduct::Velocity;
        assert!(
            !moved_to.is_level3() && !PRODUCT.is_level3(),
            "precondition: both products must be Level II, or the replan the \
             retarget triggers starts a download this test does not serve"
        );

        for (kind, expected) in [
            (PaneKind::Map, Some((moved_to, 0.0))),
            (PaneKind::CrossSection, Some((PRODUCT, TILT))),
            (PaneKind::Volume, Some((PRODUCT, TILT))),
        ] {
            let mut app = app_on_site();
            {
                let pane = app.gui.pane_mut(0).unwrap();
                // Converted *first*, because `set_kind` tears a loop down — the
                // root fix for the stuck-loop family. Planting the loop afterwards
                // is what leaves the state this filter is about, and it is
                // reachable: `loop_state` is a public field, and the setter is
                // not the only route to a non-map pane.
                pane.set_kind(kind);
                pane.loop_state = active_loop(&[volume_time()]);
                pane.selected_product = moved_to;
                pane.selected_elevation = 0.0;
            }

            app.dispatch_loop_renders();

            let keyed = app
                .gui
                .pane(0)
                .unwrap()
                .loop_state
                .rendered_for
                .as_ref()
                .map(|target| (target.product, target.elevation));
            assert_eq!(
                keyed, expected,
                "{kind:?}: the loop's render target moved for a pane whose frames \
                 nobody draws — or failed to move for one whose frames are drawn"
            );
        }
    }

    /// `dispatch_loop_renders`' **second** pass skips a pane with no plan view.
    ///
    /// That pass is the one which plans renders and clones siblings' textures.
    /// The observable is `render_failed`, which it sets on a frame whose own
    /// volume carries no sweep for the selected product: a scan with no sweeps at
    /// all makes `find_closest_elevation` answer `None`, so a map pane's frame is
    /// retired and a non-map pane's frame is never examined. No render thread and
    /// no real volume are involved.
    #[test]
    fn the_second_loop_dispatch_pass_skips_a_pane_with_no_plan_view() {
        for (kind, expected_failed) in [
            (PaneKind::Map, true),
            (PaneKind::CrossSection, false),
            (PaneKind::Volume, false),
        ] {
            let mut app = app_on_site();
            app.loop_mgr = LoopDownloadManager::new();
            // A volume that is present, so the frame is not `Pending`, and
            // carries nothing for the product, so it is `Unrenderable`.
            app.loop_mgr
                .cache_scan(SITE, volume_time(), Arc::new(empty_scan()));
            {
                let pane = app.gui.pane_mut(0).unwrap();
                // Converted first; see the note in the test above.
                pane.set_kind(kind);
                pane.loop_state = active_loop(&[volume_time()]);
            }

            app.dispatch_loop_renders();

            assert_eq!(
                app.gui.pane(0).unwrap().loop_state.frames[0].render_failed,
                expected_failed,
                "{kind:?}: the second dispatch pass judged a frame belonging to a \
                 pane it must not have looked at — or skipped one it must have"
            );
        }
    }

    /// A pane with no plan view cannot hold another pane's loop back.
    ///
    /// The worst of these, because the symptom is in the *other* panes and the
    /// cause is the filter that protects the render path.
    /// `sync_loop_playback_start`'s rule is "hold every looping pane until all of
    /// them are ready", and a pane whose frames nothing renders can never become
    /// ready — `dispatch_loop_renders` neither fills its frames nor marks them
    /// failed. So one such pane, with Sync Layers on, stops every map pane's loop
    /// from ever starting: a deadlock, silently, in panes the user did not touch.
    ///
    /// The blocked pane is given a real textured frame so it *is* render-ready and
    /// would start on its own; the only thing that can stop it is the sync rule.
    #[test]
    fn a_pane_with_no_plan_view_cannot_hold_another_panes_loop_back() {
        use rustdar_egui::pane::LoopPhase;

        let mut app = two_pane_app(SITE, SITE);
        point_at_site(&mut app, 0);
        point_at_site(&mut app, 1);
        assert!(
            app.gui.is_sync_layers(),
            "precondition: sync must be on — it is the config default, and it is              what makes one pane able to hold another back"
        );

        // Pane 0: a map pane whose loop is ready to play.
        {
            let ls = &mut app.gui.pane_mut(0).unwrap().loop_state;
            *ls = active_loop(&[volume_time()]);
            ls.phase = LoopPhase::Ready;
        }
        assert!(
            app.gui.pane(0).unwrap().loop_state.is_render_ready(),
            "precondition: the map pane's loop must be ready, or nothing can be \
             observed being held back"
        );

        // Pane 1: converted, and then given an active loop whose frames nothing
        // will ever render — the state `set_kind` clears but a public field can
        // still reach.
        {
            let pane = app.gui.pane_mut(1).unwrap();
            pane.set_kind(PaneKind::Volume);
            pane.loop_state = active_loop(&[volume_time()]);
        }
        assert!(
            !app.gui.pane(1).unwrap().loop_state.is_render_ready(),
            "precondition: the converted pane must be un-ready, which is the \
             whole hazard"
        );

        app.sync_loop_playback_start();

        assert_eq!(
            app.gui.pane(0).unwrap().loop_state.phase,
            LoopPhase::Playing,
            "the map pane's loop never started: a pane nothing renders frames for \
             was counted as a looping pane that had not caught up yet, so with \
             sync on every loop on screen waits for ever"
        );
    }

    /// The loop-frame broadcast skips a pane with no plan view.
    ///
    /// The fifth of these broadcasts and the direct sibling of the static one:
    /// a loop frame is a plan-view raster, so handing one to a pane that draws
    /// none buys a GPU texture per frame for nothing.
    ///
    /// Driven by planting the same target on both panes and delivering one
    /// finished frame, with the map case asserted in the same run so the filter is
    /// what is observed rather than a sibling that never qualified.
    #[test]
    fn the_loop_frame_broadcast_skips_a_pane_with_no_plan_view() {
        let textured = |app: &mut crate::app::App, idx: usize| {
            app.gui.pane(idx).unwrap().loop_state.frames[0]
                .texture
                .is_some()
        };

        for kind in [None, Some(PaneKind::CrossSection), Some(PaneKind::Volume)] {
            let mut app = two_pane_app(SITE, SITE);
            point_at_site(&mut app, 0);
            point_at_site(&mut app, 1);
            assert!(
                app.gui.is_sync_layers(),
                "precondition: sync is on by default"
            );
            app.loop_mgr = LoopDownloadManager::new();
            // A volume that really carries the tilt, reusing the fixture the loop
            // dispatch tests already build: `broadcast_sweep` resolves the
            // *sibling's* own scan and refuses an image whose angle its data does
            // not have, so an empty volume would refuse the broadcast for a reason
            // that has nothing to do with pane kinds.
            app.loop_mgr.cache_scan(
                SITE,
                volume_time(),
                super::loop_dispatch_tests::scan_with_sweeps(&[TILT]),
            );
            for idx in 0..2 {
                let ls = &mut app.gui.pane_mut(idx).unwrap().loop_state;
                *ls = active_loop(&[volume_time()]);
                // A result is only accepted for a frame that is *awaiting* one —
                // see `frame_awaiting_render_result_mut` — which is the state
                // `dispatch_loop_renders` leaves behind when it spawns.
                ls.frames[0].render_in_flight = true;
            }
            if let Some(kind) = kind {
                // Converted, then re-given the loop: `set_kind` tears one down.
                let pane = app.gui.pane_mut(1).unwrap();
                pane.set_kind(kind);
                pane.loop_state = active_loop(&[volume_time()]);
                pane.loop_state.frames[0].render_in_flight = true;
            }

            let target = app
                .gui
                .pane(0)
                .unwrap()
                .loop_state
                .rendered_for
                .clone()
                .expect("the fixture loop is keyed");
            app.channels
                .loop_render_sender
                .send(crate::channels::LoopRenderResponse {
                    pane_idx: 0,
                    timestamp: volume_time(),
                    target,
                    snapped: TILT,
                    site_lat: 35.33,
                    site_lon: -97.27,
                    image: Some(egui::ColorImage::from_rgba_unmultiplied(
                        [IMAGE_SIZE, IMAGE_SIZE],
                        &finished_pixels(),
                    )),
                    max_range_km: 230.0,
                })
                .expect("the receiver lives on the App");
            app.poll_loop_render_results(&egui::Context::default());

            assert!(
                textured(&mut app, 0),
                "{kind:?}: precondition: the originating pane must take its own frame"
            );
            match kind {
                None => assert!(
                    textured(&mut app, 1),
                    "precondition: a map sibling keyed to the same target must take \
                     the broadcast, or nothing below is being filtered"
                ),
                Some(kind) => assert!(
                    !textured(&mut app, 1),
                    "{kind:?}: a loop frame was uploaded to a pane that draws none"
                ),
            }
        }
    }

    /// `restore_cached_render` skips a pane with no plan view.
    ///
    /// `dispatch_pane_renders` deliberately *keeps* `cached_render` on a converted
    /// pane so that converting back to a map is instant, which makes this the one
    /// place the kept copy could still be uploaded — on every suspend, resume and
    /// surface loss, a full-size RGBA texture into the Radar overlay cache of a
    /// pane that draws no map.
    #[test]
    fn the_cached_render_restore_skips_a_pane_with_no_plan_view() {
        for kind in [PaneKind::CrossSection, PaneKind::Volume] {
            let mut app = app_on_site();
            app.render.cache_render(
                SITE,
                PRODUCT,
                rustdar_radar::types::RenderView::PlanView,
                TILT,
                cached_output(),
            );
            app.dispatch_pane_renders(&egui::Context::default());
            assert!(
                app.render.pane_render[0].cached_render.is_some(),
                "precondition: the pane must be holding a cached render to restore"
            );

            // The state a conversion leaves: the cached pixels are kept on purpose.
            app.gui.pane_mut(0).unwrap().set_kind(kind);
            app.gui
                .pane_mut(0)
                .unwrap()
                .overlay_cache_mut(OverlayKind::Radar)
                .current = None;

            app.restore_cached_render(&egui::Context::default());

            assert!(
                !holds_radar_texture(&mut app, 0),
                "{kind:?}: a resume re-uploaded a full-size plan-view texture to a \
                 pane that draws none"
            );
            assert!(
                app.render.pane_render[0].cached_render.is_some(),
                "{kind:?}: the cached pixels must survive, or converting back to a \
                 map costs a fresh render rather than an upload"
            );
        }
    }

    /// Converting a pane tears its loop down, on both sides of the seam.
    ///
    /// The root fix for the stuck-loop family, which was eight consumers with one
    /// cause: a loop left running on a pane nothing renders frames for holds
    /// `loop_mgr` state, keeps the event loop waking at loop frame rate, reads
    /// "Rendering n/m" for ever with no transport drawn to cancel it, and goes on
    /// spending the *shared* download budget on volumes nobody will draw.
    ///
    /// `PaneState::set_kind` does the pane-local half. The other half — this
    /// pane's queue inside `LoopDownloadManager`, which is keyed by index and
    /// which a `PaneState` cannot reach — is done by `dispatch_loop_renders`, so
    /// that it also covers a pane that reached a non-map kind by a route that
    /// never called the setter.
    #[test]
    fn converting_a_pane_tears_its_loop_down_on_both_sides() {
        for kind in [PaneKind::CrossSection, PaneKind::Volume] {
            let mut app = app_on_site();
            app.gui.pane_mut(0).unwrap().loop_state = active_loop(&[volume_time()]);
            app.loop_mgr = LoopDownloadManager::new();
            app.loop_mgr.set_plan(
                0,
                crate::loop_downloads::FramePlan::new(
                    SITE.to_string(),
                    vec![(
                        volume_time(),
                        rustdar_radar::archive::Identifier::new("a-volume".to_string()),
                    )],
                ),
            );
            app.loop_mgr.plan_downloads_for(0, PRODUCT);
            assert!(
                app.loop_mgr.pending_pane_indices().contains(&0),
                "precondition: the pane must own a download queue to be relieved of"
            );
            assert!(app.gui.pane(0).unwrap().loop_state.is_active());

            app.gui.pane_mut(0).unwrap().set_kind(kind);

            assert!(
                !app.gui.pane(0).unwrap().loop_state.is_active(),
                "{kind:?}: the loop survived the conversion, so it will read \
                 \"Rendering\" for ever with no transport drawn to cancel it"
            );
            // The host-side half, applied by the frame pass rather than by the
            // setter, because a `PaneState` cannot see `loop_mgr`.
            app.dispatch_loop_renders();
            assert!(
                !app.loop_mgr.pending_pane_indices().contains(&0),
                "{kind:?}: the download queue outlived the loop, so it goes on \
                 spending the shared budget on volumes nobody will draw"
            );
        }
    }

    /// `App::evict_unshown_scans` needs **no** kind filter, and this is the pin
    /// on that.
    ///
    /// It is the one all-panes loop where excluding a non-map pane would be the
    /// bug. It retains a decoded volume if any pane names its site, through
    /// `pane.site` and `pane.scan_info.site` — both flat fields on every pane
    /// whatever its kind. A section pane samples the whole volume, so it needs it
    /// alive *more* than a map pane does, and dropping it under one is a
    /// use-after-evict-shaped fault in the pass whose entire job is knowing what
    /// is on screen. This is why `PaneContent` is one field on a flat
    /// `PaneState` rather than an `enum PaneState`.
    #[test]
    fn a_whole_volume_pane_keeps_the_volume_it_is_sampling() {
        for kind in [PaneKind::CrossSection, PaneKind::Volume] {
            let mut app = app_on_site();
            app.gui.pane_mut(0).unwrap().set_kind(kind);
            app.scan_data
                .insert(SITE.to_string(), Arc::new(empty_scan()));
            app.scan_data
                .insert("KOUN".to_string(), Arc::new(empty_scan()));

            app.evict_unshown_scans();

            assert!(
                app.scan_data.contains_key(SITE),
                "{kind:?}: the volume this pane is cutting from was evicted"
            );
            assert!(
                !app.scan_data.contains_key("KOUN"),
                "precondition: eviction must still be happening at all, or the \
                 assertion above holds for a pass that dropped nothing"
            );
        }
    }
}

/// What a section pane is told when it cannot be cut, and when the picture on
/// screen has stopped being the truth.
///
/// The two refusals here are the ones a user meets without doing anything
/// wrong, and the whole point of separating them is that they are *unlike*: one
/// resolves itself on the next volume and the other never will. A pane that
/// showed the same blank for both would make the recoverable one look broken and
/// the permanent one look like it was still loading.
#[cfg(test)]
mod section_dispatch_tests {
    use super::*;
    use crate::platform_double::TestBridge;
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
    };
    use rustdar_egui::pane::{GeoPoint, PaneKind, SectionLine, SectionUnavailable};
    use rustdar_radar::types::{RadarProduct, ScanInfo};

    const SITE: &str = "KTLX";

    fn volume_time() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
            .unwrap()
            .and_hms_opt(18, 30, 0)
            .unwrap()
    }

    fn line() -> SectionLine {
        SectionLine::new(
            GeoPoint {
                lat: 35.0,
                lon: -97.8,
            },
            GeoPoint {
                lat: 35.6,
                lon: -96.9,
            },
        )
        .expect("a fixture line must be finite and have two distinct ends")
    }

    /// One elevation cut, so the coverage pattern is a real tilt ladder rather
    /// than the empty placeholder.
    fn one_cut() -> ElevationCut {
        ElevationCut::new(
            0.5,
            ChannelConfiguration::ConstantPhase,
            WaveformType::CS,
            0.0,
            false,
            false,
            false,
            false,
            0,
            0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0,
            false,
            0,
            false,
            true,
        )
    }

    /// A one-sweep reflectivity volume. `cuts` empty is exactly what
    /// `chunks::placeholder_coverage_pattern(0)` produces — the shape a volume
    /// joined mid-scan has until its VCP message lands.
    fn volume(cuts: Vec<ElevationCut>) -> Arc<Scan> {
        let radial = Radial::new(
            0,
            0,
            0.0,
            1.0,
            RadialStatus::ElevationStart,
            1,
            0.5,
            Some(MomentData::from_fixed_point(
                1,
                0,
                250,
                8,
                2.0,
                66.0,
                vec![32],
            )),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        Arc::new(Scan::new(
            VolumeCoveragePattern::new(
                if cuts.is_empty() { 0 } else { 212 },
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                cuts,
            ),
            vec![Sweep::new(1, vec![radial])],
        ))
    }

    /// An `App` with one section pane aimed along [`line`], on a site whose
    /// volume is `scan`.
    fn app_with_section(product: RadarProduct, scan: Arc<Scan>) -> crate::app::App {
        let mut app = crate::app::tests::headless(TestBridge::desktop());
        let site = rustdar_radar::sites::get_radar_site(SITE)
            .expect("KTLX is a real radar")
            .clone();
        {
            let pane = app.gui.pane_mut(0).unwrap();
            pane.site = SITE.to_owned();
            pane.selected_product = product;
            pane.set_kind(PaneKind::CrossSection);
            pane.cross_section_mut().unwrap().line = Some(line());
        }
        app.gui.set_scan_info_for_pane(
            0,
            ScanInfo {
                site,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![product],
                product_elevations: std::collections::HashMap::new(),
                status: String::new(),
            },
        );
        app.render.ensure_pane_count(1);
        app.scan_data.insert(SITE.to_owned(), scan);
        app
    }

    fn state(app: &crate::app::App) -> &rustdar_egui::pane::CrossSectionPane {
        app.gui
            .pane(0)
            .unwrap()
            .cross_section()
            .expect("pane 0 is a section pane")
    }

    /// A volume joined mid-scan says so, and **keeps asking**.
    ///
    /// `chunks.rs` stands in an empty coverage pattern until the VCP message
    /// lands, and `VolumeSampler::new` refuses that rather than inventing a
    /// ladder out of the sweeps' own elevation numbers — correctly, but the
    /// result is a blank pane in ordinary live use.
    ///
    /// Leaving the staleness key unwritten is the load-bearing half: the state
    /// resolves itself on the next volume, so the pane has to be still asking
    /// when it does. Writing the key here would make a transient condition
    /// permanent for the life of the pane.
    #[test]
    fn a_volume_with_no_coverage_pattern_says_so_and_keeps_asking() {
        let mut app = app_with_section(RadarProduct::Reflectivity, volume(Vec::new()));

        app.dispatch_section_renders();

        assert_eq!(
            state(&app).unavailable,
            Some(SectionUnavailable::AwaitingCoveragePattern),
            "a mid-scan join is a blank pane with no explanation"
        );
        assert_eq!(
            state(&app).rendered_for,
            None,
            "the key was written for a condition that clears itself, so the pane \
             will never ask again and never show a section"
        );
        assert!(
            !app.render.pane_render[0].render_in_flight,
            "a render slot was spent to be told what the volume already said"
        );

        // The message names the cause and says it clears itself, which is the
        // whole reason it is not folded into a generic "no data".
        let message = SectionUnavailable::AwaitingCoveragePattern.message();
        assert!(message.contains("mid-scan"), "{message}");
        assert!(message.contains("next volume"), "{message}");
    }

    /// A product with no vertical structure says so, and **stops** asking.
    ///
    /// The mirror of the test above, and the pair is the point: nothing about
    /// this volume or the next will make a column integral sliceable, so
    /// re-asking every frame is a busy loop with no output and no symptom but a
    /// warm machine.
    #[test]
    fn a_product_with_no_vertical_structure_says_so_and_stops_asking() {
        let mut app = app_with_section(RadarProduct::EchoTops, volume(vec![one_cut()]));

        app.dispatch_section_renders();

        assert_eq!(
            state(&app).unavailable,
            Some(SectionUnavailable::ProductHasNoVerticalStructure(
                RadarProduct::EchoTops
            )),
        );
        assert!(
            state(&app).rendered_for.is_some(),
            "nothing will ever make this product sliceable, so leaving the key \
             unwritten re-dispatches the same refusal on every frame"
        );
        assert!(!app.render.pane_render[0].render_in_flight);

        // Named, so the message can say which product and what to do instead.
        let message =
            SectionUnavailable::ProductHasNoVerticalStructure(RadarProduct::EchoTops).message();
        assert!(message.contains(RadarProduct::EchoTops.name()), "{message}");
    }

    /// A pane with no volume yet is waiting, not broken.
    #[test]
    fn a_section_with_no_volume_is_told_it_is_waiting() {
        let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
        app.gui.pane_mut(0).unwrap().scan_info = None;

        app.dispatch_section_renders();
        assert_eq!(
            state(&app).unavailable,
            Some(SectionUnavailable::AwaitingVolume)
        );
        assert_eq!(state(&app).rendered_for, None);
    }

    /// A cut of the right shape and no content, for the receive path.
    fn blank_cut() -> Box<rustdar_radar::xsect::CrossSection> {
        use rustdar_radar::sampler::SampleStatus;
        use rustdar_radar::xsect::{CrossSection, SECTION_HEIGHT, SECTION_WIDTH, SectionAxes};
        let pixels = SECTION_WIDTH * SECTION_HEIGHT;
        Box::new(
            CrossSection::from_parts(
                vec![0u8; pixels * 4],
                vec![f32::NAN; pixels],
                vec![SampleStatus::NoCoverage.wire_code(); pixels],
                SectionAxes {
                    length_km: 100.0,
                    base_km_msl: 0.4,
                    top_km_msl: 20.4,
                    near_ground_range_km: 10.0,
                    far_ground_range_km: 110.0,
                    coverage_ground_range_km: 0.0,
                    cone_of_silence_km: 0.0,
                    tilt_count: 1,
                    widest_tilt_gap_deg: 0.0,
                },
            )
            .expect("a full-size, all-NoCoverage section is well formed"),
        )
    }

    /// A cut lands on the pane that asked for it, and clears its in-flight flag.
    #[test]
    fn a_finished_cut_lands_on_the_pane_that_asked_for_it() {
        let ctx = egui::Context::default();
        let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
        let target = app.section_target_for_pane(0).expect("aimed with a volume");
        app.gui
            .pane_mut(0)
            .unwrap()
            .cross_section_mut()
            .unwrap()
            .rendered_for = Some(target.clone());
        app.render.pane_render[0].render_in_flight = true;

        app.channels
            .section_sender
            .send(crate::channels::SectionResponse {
                pane_idx: 0,
                generation: app.render.render_generation,
                target,
                section: Some(blank_cut()),
            })
            .expect("the receiver is alive");
        app.poll_section_results(&ctx);

        assert!(
            state(&app).section.is_some(),
            "the cut never reached the pane"
        );
        assert!(
            state(&app).texture.is_some(),
            "the raster was never uploaded"
        );
        assert_eq!(state(&app).unavailable, None);
        assert!(
            !app.render.pane_render[0].render_in_flight,
            "a pane that never hears back stops asking for another cut"
        );
    }

    /// A cut for a line the pane is no longer aimed along is dropped, and the
    /// key is left alone.
    ///
    /// A section takes an order of magnitude longer to produce than the user
    /// takes to draw another line over it, so this is ordinary rather than
    /// exotic — and the failure is the worst kind: a section of the *previous*
    /// line, on screen, captioned with the current volume, looking authoritative.
    #[test]
    fn a_cut_for_a_line_the_pane_has_left_behind_is_dropped() {
        let ctx = egui::Context::default();
        let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
        let superseded = app.section_target_for_pane(0).expect("aimed with a volume");

        // The pane moves on: a new line, and the key that goes with it.
        app.gui
            .pane_mut(0)
            .unwrap()
            .cross_section_mut()
            .unwrap()
            .line = SectionLine::new(
            GeoPoint {
                lat: 35.0,
                lon: -97.8,
            },
            GeoPoint {
                lat: 36.4,
                lon: -95.9,
            },
        );
        let current = app.section_target_for_pane(0).expect("still aimed");
        assert_ne!(
            current, superseded,
            "precondition: the pane really moved on"
        );
        app.gui
            .pane_mut(0)
            .unwrap()
            .cross_section_mut()
            .unwrap()
            .rendered_for = Some(current.clone());

        app.channels
            .section_sender
            .send(crate::channels::SectionResponse {
                pane_idx: 0,
                generation: app.render.render_generation,
                target: superseded,
                section: Some(blank_cut()),
            })
            .expect("the receiver is alive");
        app.poll_section_results(&ctx);

        assert!(
            state(&app).section.is_none(),
            "a cut of the line the user has already replaced is on screen"
        );
        assert_eq!(
            state(&app).rendered_for,
            Some(current),
            "the superseded cut took the key with it, so the cut still in flight \
             will be dropped too and the pane will wait for ever"
        );
    }

    /// A cut answering nothing says so, rather than leaving the pane looking as
    /// though it were still working.
    #[test]
    fn a_cut_that_answered_nothing_says_it_failed() {
        let ctx = egui::Context::default();
        let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
        let target = app.section_target_for_pane(0).expect("aimed with a volume");
        app.gui
            .pane_mut(0)
            .unwrap()
            .cross_section_mut()
            .unwrap()
            .rendered_for = Some(target.clone());

        app.channels
            .section_sender
            .send(crate::channels::SectionResponse {
                pane_idx: 0,
                generation: app.render.render_generation,
                target,
                section: None,
            })
            .expect("the receiver is alive");
        app.poll_section_results(&ctx);

        assert_eq!(
            state(&app).unavailable,
            Some(SectionUnavailable::RenderFailed),
            "a pane that will never get a picture must not look like one that is \
             about to"
        );
    }

    /// A result from a superseded *generation* is dropped **and clears the key**.
    ///
    /// The opposite of the case above, and the asymmetry is the point. There the
    /// pane has already asked for something else, so its key belongs to a cut
    /// still in flight. Here the pane is still waiting and the answer has been
    /// thrown away, so leaving the key would tell the dispatcher this cut had
    /// been answered — and nothing else would ever ask again.
    #[test]
    fn a_result_from_a_dead_generation_puts_the_pane_back_to_asking() {
        let ctx = egui::Context::default();
        let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
        let target = app.section_target_for_pane(0).expect("aimed with a volume");
        app.gui
            .pane_mut(0)
            .unwrap()
            .cross_section_mut()
            .unwrap()
            .rendered_for = Some(target.clone());
        let stale = app.render.render_generation;
        app.render.render_generation += 1;

        app.channels
            .section_sender
            .send(crate::channels::SectionResponse {
                pane_idx: 0,
                generation: stale,
                target,
                section: Some(blank_cut()),
            })
            .expect("the receiver is alive");
        app.poll_section_results(&ctx);

        assert!(state(&app).section.is_none(), "a stale cut was drawn");
        assert_eq!(
            state(&app).rendered_for,
            None,
            "the key outlived the answer that was thrown away, so the pane will \
             never ask again and never show a section"
        );
    }

    /// A new volume for the site makes the section on screen stale **by the
    /// same comparison** that notices a moved endpoint or a changed moment.
    ///
    /// This is what buys the absence of a `reset_panes_for_*` arm for section
    /// panes — the kind of thing that gets remembered for one of the two reset
    /// paths and not the other. Asserted on the key itself, because the key is
    /// what the dispatch decides on.
    #[test]
    fn a_new_volume_makes_the_section_on_screen_stale_with_no_reset_arm() {
        let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));

        let before = app
            .section_target_for_pane(0)
            .expect("the pane is aimed and has a volume");

        // Nothing but the volume time moves.
        if let Some(info) = app.gui.pane_mut(0).unwrap().scan_info.as_mut() {
            info.timestamp = volume_time() + chrono::Duration::minutes(6);
        }
        let after = app.section_target_for_pane(0).expect("still aimed");
        assert_ne!(before, after, "a new volume did not make the key move");

        // The product picker moves it too, so the one comparison really does
        // cover every input rather than only the one it was written for.
        app.gui.pane_mut(0).unwrap().selected_product = RadarProduct::Velocity;
        assert_ne!(app.section_target_for_pane(0), Some(after));

        // And so does the line, which is the input the interaction produces.
        app.gui
            .pane_mut(0)
            .unwrap()
            .cross_section_mut()
            .unwrap()
            .line = SectionLine::new(
            GeoPoint {
                lat: 35.0,
                lon: -97.8,
            },
            GeoPoint {
                lat: 36.0,
                lon: -96.0,
            },
        );
        let moved = app.section_target_for_pane(0).expect("still aimed");
        assert_ne!(moved.line, before.line);
    }

    /// A live volume that is still filling re-cuts as it fills, **even though
    /// its timestamp never moves**.
    ///
    /// This is the configuration the feature actually ships in — live chunks are
    /// on by default — and it is the one the volume-time key does not cover.
    /// `ScanInfo::timestamp` is the *first* sweep's first radial, and on the
    /// chunk feed `sweeps[0]` is fixed for the whole volume, so the stamp is a
    /// constant for five to six minutes while the ladder goes from one rung to
    /// nine. Observed live before the fix: a map pane full of echo, a section
    /// pane empty, and a caption reading `1 tilts` for six minutes.
    ///
    /// The precondition is the assertion that matters. `before.volume` and
    /// `after.volume` are asserted **equal** — if a future change makes the
    /// volume stamp move on the live feed this test starts failing on its
    /// premise rather than quietly passing for the wrong reason.
    #[test]
    fn a_live_volume_that_is_still_filling_re_cuts_as_it_fills() {
        let mut app = app_with_section(RadarProduct::Reflectivity, volume(vec![one_cut()]));
        let grow_to = |app: &mut crate::app::App, angles: Vec<f32>| {
            if let Some(info) = app.gui.pane_mut(0).unwrap().scan_info.as_mut() {
                info.product_elevations
                    .insert(RadarProduct::Reflectivity, angles);
            }
        };

        grow_to(&mut app, vec![0.5]);
        let before = app
            .section_target_for_pane(0)
            .expect("the pane is aimed and has a volume");
        assert_eq!(before.tilts, 1);

        // Exactly what `Gui::apply_chunk_scan_info` does on the next few chunks:
        // it merges new angles in and leaves `timestamp` where it was.
        grow_to(&mut app, vec![0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4]);
        let after = app.section_target_for_pane(0).expect("still aimed");

        assert_eq!(
            before.volume, after.volume,
            "precondition: the live feed's volume stamp really is frozen, so it \
             cannot be what notices the volume growing"
        );
        assert_eq!(after.tilts, 9);
        assert_ne!(
            before, after,
            "eight more cuts arrived and the pane went on showing a one-rung section"
        );

        // And the pane really re-dispatches on it: with the one-rung key stored,
        // the nine-rung target no longer matches and the short-circuit at the
        // top of `dispatch_section_renders` falls through.
        app.gui
            .pane_mut(0)
            .unwrap()
            .cross_section_mut()
            .unwrap()
            .rendered_for = Some(before);
        app.dispatch_section_renders();
        assert_eq!(
            state(&app).rendered_for.as_ref().map(|t| t.tilts),
            Some(9),
            "the dispatcher short-circuited on a key cut from one ninth of the volume"
        );
    }
}
