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

    /// Apply the storm motion override the settings panel holds, and if it
    /// moved, invalidate everything derived with the old vector.
    ///
    /// Returns whether the vector changed. A method rather than a block inside
    /// [`Self::dispatch_pane_renders`] because it is the whole edit path — the
    /// widget's own state in, three invalidations out — and the only way to
    /// test it end to end is to be able to call it. `dispatch_pane_renders`
    /// takes an `egui::Context` and does eleven other things.
    fn apply_storm_motion_override(&mut self) -> bool {
        // Editing the vector changes nothing else about a pane, so the derived
        // storm-relative tilts have to be invalidated explicitly.
        let storm_motion = self.gui.storm_motion_override.sample();
        if !self.render.set_storm_motion_override(storm_motion) {
            return false;
        }
        // The vertical views' counterpart of the plan-view invalidation the
        // setter just did: an SRV grid or section is derived *with* the
        // vector, but the vector is not part of the target that keys it —
        // without this, an override edit leaves every SRV volume and section
        // painting the old vector's field until the next volume.
        //
        // Clearing a section pane's staleness key is necessary and was never
        // sufficient. The dispatcher's own payload cache is keyed separately,
        // and until the vector joined that key too a cleared staleness key
        // simply re-dispatched the *same payload* — see
        // `render_dispatch::SectionInputKey::storm_motion`.
        self.volume_store
            .evict_product(rustdar_radar::types::RadarProduct::StormRelativeVelocity);
        for pane_idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            if pane.selected_product != rustdar_radar::types::RadarProduct::StormRelativeVelocity {
                continue;
            }
            if let Some(volume) = pane.volume_mut() {
                volume.rendered_for = None;
            }
            if let Some(section) = pane.cross_section_mut() {
                section.rendered_for = None;
            }
        }
        true
    }

    /// Check all panes for needed background renders and spawn render threads.
    fn dispatch_pane_renders(&mut self, ctx: &egui::Context) {
        self.apply_storm_motion_override();
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

            // The current merged volume — the base plus every sealed sweep —
            // and **not** `scan_data`, whose mid-volume content is the growing
            // snapshot alone: cutting from that is what made a section's
            // ladder start one rung tall after every roll. The section reads
            // the same resolve the target's fingerprint and the 3D build read,
            // so all three describe one volume.
            let base = self
                .base_scans
                .get(site.as_str())
                .map(|(scan, _)| Arc::clone(scan));
            let overlay = self.chunk_feeds.snapshot(site.as_str());

            // The two refusals that have to be *named* rather than left as a
            // blank pane. Checked here, before any budget is taken, because both
            // are properties of the volume and the product rather than of the
            // cut — dispatching would burn a render slot to be told the same
            // thing, and on wasm there is only one slot.
            if let Some(reason) = section_source_refusal(base.as_deref(), overlay.as_deref()) {
                // Both reasons resolve themselves — the mid-flight pattern
                // arrives with the next volume start, the first download is
                // already in flight — so the key is *not* written: the pane
                // will ask again, and get an answer.
                self.mark_section_unavailable(pane_idx, reason);
                continue;
            }
            // `volume_slot`, not `samplable`: the derived products (SRV,
            // NROT, KDP) slice through the worker-side derivation layer
            // (`rustdar_radar::derive`), so only the products with no
            // per-tilt field at all — the hybrid classification, the column
            // integrals, the precipitation rate — are refused here.
            if rustdar_radar::derive::volume_slot(target.product).is_none() {
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

            // The extraction, deferred: it walks the merged volume's ~15 MB of
            // gate bytes on this thread, so the dispatcher only runs it when
            // its payload cache misses — the closure owns the `Arc`s and
            // resolves the same merge the refusal check above cleared.
            let product = target.product;
            // Captured before the closure: the user's storm motion vector,
            // for the worker-side SRV derivation. The extraction keeps it
            // only on an SRV payload.
            let motion = self.render.storm_motion_override_kt();
            let extract = move || {
                let current = rustdar_radar::current::resolve(base.as_deref(), overlay.as_deref())?;
                rustdar_radar::render_input::RenderInput::extract_volume_parts(
                    current.pattern(),
                    current.sweeps(),
                    product,
                    lat,
                    lon,
                    motion,
                )
            };
            match self.render.spawn_section_render(
                pane_idx,
                &target,
                extract,
                self.channels.section_sender.clone(),
                self.window.clone(),
            ) {
                // Nothing taken, nothing said: the budget frees up on its own
                // and the pane asks again next frame.
                crate::render_dispatch::SectionDispatch::Busy => {}
                crate::render_dispatch::SectionDispatch::NoPayload => {
                    // This volume carries nothing to cut under this product.
                    // The key **is** written, and that is the fix: without a
                    // name for this state it was indistinguishable from a full
                    // budget, so the pane re-asked every frame and painted
                    // "Cutting the cross-section…" for as long as the volume
                    // stood. The key carries the volume stamp and the ladder,
                    // so the next volume asks again on its own.
                    self.mark_section_unavailable(
                        pane_idx,
                        rustdar_egui::pane::SectionUnavailable::ProductMissingFromVolume(
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
                }
                crate::render_dispatch::SectionDispatch::Dispatched => {
                    if let Some(section) = self
                        .gui
                        .pane_mut(pane_idx)
                        .and_then(|p| p.cross_section_mut())
                    {
                        // Written on **dispatch**, not on arrival. A cut that
                        // answers nothing would otherwise never write it, and
                        // the pane would re-dispatch the same failing cut on
                        // every frame for as long as the volume stood — a busy
                        // loop whose only symptom is a warm machine.
                        // `poll_section_results` matches the reply against this
                        // key, so a superseded cut still cannot land.
                        section.rendered_for = Some(target);
                        section.unavailable = None;
                    }
                }
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
        let Some(collected) = pane.scan_info.as_ref().map(|s| s.timestamp) else {
            self.mark_section_unavailable(
                pane_idx,
                rustdar_egui::pane::SectionUnavailable::AwaitingVolume,
            );
            return None;
        };
        // The ladder fingerprint, resolved over the same merged volume the cut
        // will be extracted from — **not** off the pane's
        // `ScanInfo::product_elevations`. See `SectionTarget::ladder`: the
        // pane's angle set is merged rather than replaced as chunks land, so
        // after one complete volume it already holds the whole VCP and never
        // moves again — which would freeze the key exactly the way the volume
        // timestamp does, one volume later. An unresolvable ladder keys zero
        // rather than refusing: the dispatch below has its own arm for that,
        // and this one is about naming the key.
        let ladder = self
            .current_ladder_fingerprint(site.as_str(), product)
            .unwrap_or(0);
        Some(rustdar_egui::pane::SectionTarget {
            volume: rustdar_egui::pane::VolumeStamp { site, collected },
            product,
            line,
            ladder,
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

            let texture = self.upload_section_raster(ctx, &cut);

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

    /// Upload a cut's raster and hand back the handle. The **one** place a
    /// section becomes a texture.
    ///
    /// Two callers — the arrival path above and the resume path below — and
    /// they share this rather than each doing their own `load_texture` because
    /// the options are an honesty decision that has to hold on both. NEAREST,
    /// and it is not a performance choice: a section's rows are the tilt
    /// ladder's rungs stretched to fill the gaps between them, and bilinear
    /// filtering would blend those edges into a smooth gradient and paint
    /// exactly the impression the pane's caption exists to refuse — that the
    /// vertical structure was measured continuously. The blockiness is the
    /// data. A resume that quietly re-uploaded the same pixels `LINEAR` would
    /// look like nothing at all had changed.
    fn upload_section_raster(
        &mut self,
        ctx: &egui::Context,
        cut: &rustdar_radar::xsect::CrossSection,
    ) -> egui::TextureHandle {
        self.texture_counter += 1;
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [
                rustdar_radar::xsect::SECTION_WIDTH,
                rustdar_radar::xsect::SECTION_HEIGHT,
            ],
            cut.image(),
        );
        ctx.load_texture(
            format!("cross_section_{}", self.texture_counter),
            color_image,
            egui::TextureOptions::NEAREST,
        )
    }

    /// Put every section pane's raster back on the GPU, from the
    /// [`CrossSection`](rustdar_radar::xsect::CrossSection) the pane still
    /// holds.
    ///
    /// # This is the whole reason the cut is retained across a release
    ///
    /// `PaneContent::release_textures` drops the handle and keeps the cut, and
    /// without this function that keeping bought nothing: a section pane came
    /// back from a suspend, a display change or a wgpu surface loss with
    /// `texture: None`, `section: Some(..)` and `rendered_for: Some(target)`,
    /// which paints "Cutting the cross-section…" while
    /// `dispatch_section_renders` short-circuits on the matching key and never
    /// asks again. The hover readout is gone with it, because
    /// `render_cross_section` returns before it. On the live feed the next
    /// volume rescued the pane within a scan; on an archived or paused volume
    /// nothing ever did — the "waiting that will never end" the section module
    /// names as the worst state a pane can be in.
    ///
    /// # Why re-upload rather than re-cut
    ///
    /// Clearing `rendered_for` here instead would make the dispatcher ask
    /// again, and that answer is worse three ways. It is a 15.6 MB volume walk
    /// plus an 8–13 ms raster for a picture already in memory, paid on resume,
    /// which on Android is the moment with the least budget. It needs the
    /// *volume*, which may have been evicted while the app was away — turning a
    /// recoverable state into `AwaitingVolume` forever. And it is slow enough
    /// to be seen, where this is on screen the frame the context comes back.
    ///
    /// # Why re-uploading cannot show a stale picture
    ///
    /// Because the key is kept too. This restores exactly the picture that was
    /// on the glass when the context died, still described by the
    /// `rendered_for` it was cut for. If the pane's target has moved on since —
    /// a new volume, a different moment, a redrawn line — `dispatch_section_renders`
    /// compares against that same key on the next frame, disagrees, and cuts a
    /// fresh one over the top. The restore never *extends* the life of a stale
    /// section; it only stops one blinking out.
    fn restore_section_textures(&mut self, ctx: &egui::Context) {
        // Every *remembered* pane, not every visible one, because
        // `clear_graphics_state` released every remembered pane. A section pane
        // the user has split away from comes back to a live context otherwise
        // holding a released texture, with its `rendered_for` still satisfied —
        // the same stuck pane, reached by splitting up instead of by suspending.
        for pane_idx in 0..self.gui.remembered_pane_count() {
            let Some(cut) = self
                .gui
                .pane(pane_idx)
                .and_then(|pane| pane.cross_section())
                // A pane that still has its handle was not released, so
                // re-uploading would leak the live one it is drawing with.
                .filter(|section| section.texture.is_none())
                .and_then(|section| section.section.clone())
            else {
                continue;
            };
            let texture = self.upload_section_raster(ctx, &cut);
            if let Some(section) = self
                .gui
                .pane_mut(pane_idx)
                .and_then(|p| p.cross_section_mut())
            {
                section.texture = Some(texture);
            }
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

        // Section panes first, and through their own loop: the one below is
        // bounded by `pane_render.len()` and skips every pane with no plan
        // view, which is every section pane there is.
        self.restore_section_textures(ctx);

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

    /// Returns how soon egui asked to be painted again — the frame's
    /// `repaint_delay`, which `handle_redraw` turns into an immediate
    /// redraw or a scheduled wake (the second user test's animation fix;
    /// see `PreparedFrame::repaint_delay`). Returned from every exit,
    /// the skipped-surface ones included: the pass ended either way, and
    /// an animation must not stall because one frame lost its surface.
    pub(super) fn present_frame(&mut self, size_in_pixels: [u32; 2]) -> std::time::Duration {
        let state = self.state.as_mut().unwrap();
        let window = self.window.as_ref().unwrap();

        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        // The pane mirror: which 2D panes some 3D pane is standing on, and the
        // target their render is copied into. Empty when nothing wants a floor,
        // and then the whole pass is skipped rather than clearing a texture
        // nobody reads.
        //
        // The format is the **swapchain's**, deliberately: `egui_wgpu` chose its
        // fragment entry point from that format once, at `Renderer::new`, and
        // the same pipeline draws the mirror. A mirror whose sRGB-ness
        // disagreed would be a floor slightly too dark or too light, with no
        // validation error to notice it by. `AttachmentConfig` is where that
        // format is recorded.
        let mirror_rects = self.gui.mirror_source_rects();
        let mirror_target = (!mirror_rects.is_empty())
            .then(|| {
                let points = state.egui_renderer.context().pixels_per_point();
                let (size, scale) = crate::egui_renderer::mirror_size_for(size_in_pixels, points);
                let format = state.egui_renderer.attachment_config().color_format;
                let device = state.device.clone();
                state
                    .egui_renderer
                    .callback_resources_mut()
                    .get_mut::<crate::volume::bridge::VolumeResources>()
                    .map(|resources| (resources.ensure_mirror(&device, size, format), size, scale))
            })
            .flatten();
        let mirror =
            mirror_target
                .as_ref()
                .map(|(view, size, scale)| crate::egui_renderer::MirrorRequest {
                    view,
                    size_in_pixels: *size,
                    pixels_per_point: *scale,
                    source_rects: &mirror_rects,
                });

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
                    mirror,
                )
            },
            |finished| Self::get_surface_texture(&state.surface, finished),
        );
        let repaint_delay = frame.repaint_delay();

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
                return repaint_delay;
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
        repaint_delay
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

/// Why no section can be cut from what the app holds for a site, or `None`
/// when one can.
///
/// A pure function of the two holders so the decision is testable without a
/// live chunk feed. The distinction between its two answers is the load-bearing
/// part: an overlay carrying sealed sweeps but no pattern is the mid-flight
/// join — `chunks.rs` stands in an empty coverage pattern until the VCP
/// message lands, and `current::resolve` correctly refuses to key a flight by
/// another flight's table — while nothing at all is the cold-start download.
/// Both clear themselves, and each needs its own sentence.
fn section_source_refusal(
    base: Option<&nexrad_model::data::Scan>,
    overlay: Option<&nexrad_model::data::Scan>,
) -> Option<rustdar_egui::pane::SectionUnavailable> {
    if rustdar_radar::current::resolve(base, overlay).is_some() {
        return None;
    }
    if overlay.is_some_and(|scan| !scan.sweeps().is_empty()) {
        return Some(rustdar_egui::pane::SectionUnavailable::AwaitingCoveragePattern);
    }
    Some(rustdar_egui::pane::SectionUnavailable::AwaitingVolume)
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
            "Loop: sampled {} down to {} frames for {}",
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

/// The order one frame is assembled in.
///
/// `setup_egui_frame` unwraps an `AppState`, which is a wgpu device, a surface
/// and a window — none of which exist here — so the sequence can only be read
/// off the source, the same handle `handle_input_events` and `begin_frame` are
/// pinned by.
#[path = "app_render/frame_build_order_tests.rs"]
#[cfg(test)]
mod frame_build_order_tests;

#[path = "app_render/frame_order_tests.rs"]
#[cfg(test)]
mod frame_order_tests;

/// What `poll_level3_results` does with a channel holding more than one answer.
///
/// Built on `stamping_tests`' fixtures: an `App` with one pane on a real radar,
/// and the smallest Level III object the pipeline will accept.
#[path = "app_render/level3_poll_tests.rs"]
#[cfg(test)]
mod level3_poll_tests;

#[path = "app_render/loop_dispatch_tests.rs"]
#[cfg(test)]
mod loop_dispatch_tests;

/// What the loop timer does with a playback speed no slider could have set.
#[path = "app_render/loop_interval_tests.rs"]
#[cfg(test)]
mod loop_interval_tests;

/// The Level III half of the loop: pairing a bucket object to each frame's volume,
/// what a gap does, and what happens when a pane retargets across the datasource
/// line mid-loop.
///
/// Nothing here touches the network. The pairing itself is
/// `rustdar_radar::level3`'s, tested against synthetic keys and PDBs there; what
/// these tests pin is the frontend's half — which frames get queued, what a
/// resolved-to-nothing frame does to playback, and that a Level III frame reaches
/// the render dispatcher through exactly the path a Level II one does.
#[path = "app_render/loop_level3_tests.rs"]
#[cfg(test)]
mod loop_level3_tests;

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
#[path = "app_render/pane_kind_render_filter_tests.rs"]
#[cfg(test)]
mod pane_kind_render_filter_tests;

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
#[path = "app_render/restore_describes_its_image_tests.rs"]
#[cfg(test)]
mod restore_describes_its_image_tests;

/// What a section pane is told when it cannot be cut, and when the picture on
/// screen has stopped being the truth.
///
/// The two refusals here are the ones a user meets without doing anything
/// wrong, and the whole point of separating them is that they are *unlike*: one
/// resolves itself on the next volume and the other never will. A pane that
/// showed the same blank for both would make the recoverable one look broken and
/// the permanent one look like it was still loading.
#[path = "app_render/section_dispatch_tests.rs"]
#[cfg(test)]
mod section_dispatch_tests;

/// What `poll_level3_results` does with sounding responses: the same drain and
/// fetch-generation gate as everything else on it, plus the keep-on-failure
/// rule that makes the TTL retry loop safe.
#[path = "app_render/sounding_poll_tests.rs"]
#[cfg(test)]
mod sounding_poll_tests;

/// What `apply_render_to_pane` does with a finished image beyond placing it.
///
/// Reached by building an `App` — see `app::tests::headless` — with the
/// platform double standing in for the OS and a bare `egui::Context` for the
/// renderer. The upload is genuinely done here: `Context::load_texture` needs no
/// device, no surface and no window, so the only thing that ever blocked this
/// was `App::new`'s wgpu instance.
#[path = "app_render/stamping_tests.rs"]
#[cfg(test)]
mod stamping_tests;
