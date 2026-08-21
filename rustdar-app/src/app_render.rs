use super::frame_pump::PumpPhase;
use crate::loop_pool::{LoopAllocation, LoopDemand, LoopFrameModel};
use crate::render_dispatch::CachedPaneRender;
use egui_wgpu::wgpu;
use rustdar_device_profile::constants::{
    DEFAULT_LOOP_SPEED_FPS, MAX_LOOP_SECTION_CUTS_PER_FRAME, MAX_LOOP_SPEED_FPS,
    MAX_LOOP_VOLUME_BUILDS_PER_FRAME, MIN_LOOP_SPEED_FPS,
};
use rustdar_egui::actions::GuiAction;
use rustdar_egui::pane::{BroadcastSweep, ELEVATION_TOLERANCE, RenderTarget};
use rustdar_egui::radar_layer;
use rustdar_radar::loop_downloads::{FramePlan, L3FrameState, PendingDownloads, PendingL3Pairings};
// Test-only since WO-M12d: production loop dispatch holds a frame's payload
// only as radar's own described job. What still names the arms is the suites'
// own inspection of `frame_data` below.
#[cfg(test)]
use rustdar_radar::loop_downloads::LoopFrameData;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// What a speculative dispatch needs from the delivered result's own pane
/// — copied OUT of `poll_render_results`' one origin-pane read,
/// because the borrow cannot span the apply calls and the hook must not
/// re-read state its caller already read.
struct SpeculationInputs {
    volume_start: chrono::NaiveDateTime,
    lat: f64,
    lon: f64,
    /// The name the volume is stored under — `ScanInfo::site` is the table's
    /// row, so the name is the table's own `&'static str`.
    scan_site: &'static str,
    /// The pane's tilt ladder for the delivered product, if it has one.
    ladder: Option<Vec<f32>>,
}

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
fn loop_interval(fps: f32) -> std::time::Duration {
    let fps = if fps.is_finite() {
        fps.clamp(MIN_LOOP_SPEED_FPS, MAX_LOOP_SPEED_FPS)
    } else {
        DEFAULT_LOOP_SPEED_FPS
    };
    std::time::Duration::from_secs_f32(1.0 / fps)
}

/// The plan-view rasters one pass has already put on the GPU, so the second
/// pane showing one of them is handed the *texture* rather than a second copy
/// of the picture.
#[derive(Default)]
pub(super) struct PlanViewUploads {
    uploaded: Vec<(Arc<egui::ColorImage>, egui::TextureHandle)>,
}

impl PlanViewUploads {
    /// The texture holding `image`, running `upload` only if this pass has not
    /// uploaded that exact buffer already.
    fn handle(
        &mut self,
        image: &Arc<egui::ColorImage>,
        upload: impl FnOnce() -> egui::TextureHandle,
    ) -> egui::TextureHandle {
        if let Some((_, texture)) = self
            .uploaded
            .iter()
            .find(|(seen, _)| Arc::ptr_eq(seen, image))
        {
            return texture.clone();
        }
        let texture = upload();
        self.uploaded.push((Arc::clone(image), texture.clone()));
        texture
    }
}

impl super::App {
    /// Set up and run the egui UI pass.
    pub(super) fn setup_egui_frame(&mut self) -> ([u32; 2], Vec<GuiAction>) {
        // Before the pass, because the cache it writes is read by everything
        // that rasterizes off-frame — see `App::resolve_theme`.
        let use_dark_theme = self.resolve_theme();

        // Open egui's pass and apply the theme.
        let size_in_pixels = {
            let state = self.state.as_mut().unwrap();
            let window = self.window.as_ref().unwrap();

            let window_size = window.inner_size();
            // The CSS-size-to-backing-store ratio, and nothing else.
            let zoom_factor = state.surface_config.width as f32 / window_size.width.max(1) as f32;

            // Start egui frame
            state.egui_renderer.begin_frame(window, zoom_factor);

            state.egui_renderer.apply_theme(use_dark_theme);

            [state.surface_config.width, state.surface_config.height]
        };

        // Ensure pane_render vec matches gui pane count
        self.render.ensure_pane_count(self.gui.pane_count());
        // And the other thing keyed by pane index that a layout change strands:
        self.release_hidden_pane_volumes();

        let ctx = self.state.as_ref().unwrap().egui_renderer.context().clone();

        // Before the pollers, which is before `Gui::ui` builds the paint list: a
        // raster whose last band landed on the previous frame goes on screen in
        // *this* frame's paint list rather than the next one. See the callee.
        self.promote_uploaded_rasters();

        self.run_frame_pump(PumpPhase::Apply, Some(&ctx));
        self.run_frame_pump(PumpPhase::Advance, Some(&ctx));
        self.run_frame_pump(PumpPhase::Dispatch, Some(&ctx));
        let volume_budget = self
            .loop_allocation()
            .volume_reserve_bytes()
            .max(self.budgets.volume_loop_bytes());
        let evicted = self.volume_store.enforce_budget(volume_budget);
        if evicted > 0 {
            log::info!(
                "3D volume view: evicted {evicted} resident grid(s) to fit the {} MiB budget",
                volume_budget / (1024 * 1024),
            );
        }
        self.update_loop_readiness();

        // The frame's facts, composed after every drain above so each one
        // reflects this frame's arrivals, and applied in one call so the UI
        // can never see half a frame's worth.
        self.push_frame_inputs();

        // Last, so this frame is laid out over everything applied above.
        let gui_action = self.gui.ui(&ctx);

        (size_in_pixels, gui_action)
    }

    /// Compose this frame's [`rustdar_egui::shell_api::FrameInputs`] from the
    /// state the App owns and apply it — the one place the snapshot-shaped
    /// facts cross the Gui↔App seam.
    pub(super) fn push_frame_inputs(&mut self) {
        self.gui
            .apply_frame_inputs(rustdar_egui::shell_api::FrameInputs {
                safe_area_insets: self.safe_area_insets,
                supports_exit: self.supports_exit,
                loop_frame_budget: self.loop_frame_budget,
                location_settings_available: self.location_settings_available,
                // Read off the gate each frame; the gate is the owner and
                // `poll_platform_state` already redraws on a change.
                location: (self.location.permission(), self.location.active()),
                // The arrival instant travels with the fix — see the field.
                gps: self.user_gps.clone(),
                user_heading: self.user_heading,
                catalogue_pending: self.catalogue_pending,
                liveness: &self.liveness,
                floor_tile_zoom_bias: self.mirror_rungs.tile_zoom_bias(),
            });
    }

    /// Poll for completed background render results and upload textures.
    fn poll_render_results(&mut self, ctx: &egui::Context) {
        let mut uploads = PlanViewUploads::default();
        while let Ok(rr) = self.channels.render_receiver.try_recv() {
            // A speculative result before any pane bookkeeping:
            if let Some(site) = rr.speculative_for {
                self.render.speculative_finished();
                if !self.render.is_render_stale(rr.generation)
                    && let Some(rendered) = rr.rendered
                {
                    self.render.cache_render(
                        &site,
                        rr.product,
                        rustdar_radar::types::RenderView::PlanView,
                        rr.elevation,
                        crate::render_dispatch::CachedRenderOutput {
                            image: rendered.image,
                            max_range_km: rendered.max_range_km,
                            hover: rendered.hover,
                            nyquist_ms: rendered.nyquist_ms,
                            melting_layer_source: rendered.melting_layer_source,
                            storm_motion: rendered.storm_motion,
                        },
                    );
                }
                continue;
            }
            if rr.pane_idx < self.render.pane_render.len() {
                self.render.pane_render[rr.pane_idx].render_finished();
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
            let Some(rendered) = rr.rendered else {
                continue;
            };

            // Extract fields to avoid borrow issues
            let origin_pane = rr.pane_idx;
            let render_result = crate::render_dispatch::CachedPaneRender {
                image: rendered.image,
                max_range_km: rendered.max_range_km,
                hover: rendered.hover,
                product: rr.product,
                elevation: rr.elevation,
                nyquist_ms: rendered.nyquist_ms,
                melting_layer_source: rendered.melting_layer_source,
                storm_motion: rendered.storm_motion,
            };

            // Cache the render output for sharing with other panes on the same site.
            let (origin_site, origin_draws_plan, speculate_from) = self
                .gui
                .pane(origin_pane)
                .map(|p| {
                    let inputs = p
                        .is_map()
                        .then_some(p.scan_info.as_ref())
                        .flatten()
                        .map(|si| SpeculationInputs {
                            volume_start: si.timestamp,
                            lat: si.site.lat,
                            lon: si.site.lon,
                            scan_site: si.site.name,
                            ladder: si.product_elevations.get(&rr.product).cloned(),
                        });
                    (p.site().to_string(), p.is_map(), inputs)
                })
                .unwrap_or_default();
            self.render.cache_render(
                &origin_site,
                render_result.product,
                rustdar_radar::types::RenderView::PlanView,
                render_result.elevation,
                crate::render_dispatch::CachedRenderOutput {
                    image: Arc::clone(&render_result.image),
                    max_range_km: render_result.max_range_km,
                    hover: Arc::clone(&render_result.hover),
                    nyquist_ms: render_result.nyquist_ms,
                    melting_layer_source: render_result.melting_layer_source,
                    storm_motion: render_result.storm_motion,
                },
            );

            if origin_draws_plan {
                self.apply_render_to_pane(ctx, origin_pane, &render_result, &mut uploads);
            }

            // Broadcast to sibling panes that need the same site+product+elevation.
            let pane_count = self.gui.pane_count();
            for other_idx in 0..pane_count {
                if other_idx == origin_pane {
                    continue;
                }
                let Some(other) = self.gui.pane(other_idx) else {
                    continue;
                };
                if !other.is_map() || other.site() != origin_site {
                    continue;
                }
                let Some((other_product, other_elevation)) = other
                    .get_rendering_params()
                    .and_then(|(id, e)| Some((rustdar_radar::fields::product_for(&id)?, e)))
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
                        self.apply_render_to_pane(ctx, other_idx, &render_result, &mut uploads);
                    }
                }
            }

            if let Some(inputs) = speculate_from {
                self.maybe_spawn_speculative_render(
                    &origin_site,
                    render_result.product,
                    render_result.elevation,
                    inputs,
                );
            }
        }
    }

    /// Dispatch ONE adjacent-tilt pre-render after a delivered static plan
    /// view, when ALL of: not wasm and the budget is wide
    /// ([`crate::render_dispatch::speculative_render_allowed`] — desktop 6 /
    /// mobile 3 qualify, wasm's 1 never, AF8); **no interactive render in
    /// flight** (both the pane flags and the shared thread counter read
    fn maybe_spawn_speculative_render(
        &mut self,
        site: &str,
        product: rustdar_radar::types::RadarProduct,
        delivered_elevation: f32,
        inputs: SpeculationInputs,
    ) {
        if !crate::render_dispatch::speculative_render_allowed(
            super::WEB,
            self.render.concurrent_renders(),
        ) {
            return;
        }
        if self.render.speculative_in_flight()
            || self.render.any_render_in_flight()
            || self
                .render
                .renders_in_flight
                .load(std::sync::atomic::Ordering::Relaxed)
                != 0
        {
            return;
        }
        // Level III panes render from fetched objects, not from the volume —
        // there is no Level II job to speculate.
        if product.is_level3() {
            return;
        }
        let Some(ladder) = inputs.ladder else {
            return;
        };
        let above = ladder
            .iter()
            .copied()
            .filter(|e| *e > delivered_elevation + ELEVATION_TOLERANCE)
            .min_by(|a, b| a.total_cmp(b));
        let below = ladder
            .iter()
            .copied()
            .filter(|e| *e < delivered_elevation - ELEVATION_TOLERANCE)
            .max_by(|a, b| a.total_cmp(b));
        let Some(target) = above.or(below) else {
            return;
        };
        // Already resident: the goal state — nothing to pre-render.
        if self
            .render
            .get_cached_render(
                site,
                product,
                rustdar_radar::types::RenderView::PlanView,
                target,
            )
            .is_some()
        {
            return;
        }
        let Some((data, declared)) = self.volumes.still_for(inputs.scan_site) else {
            return;
        };
        self.render.spawn_speculative_render(
            site,
            product,
            target,
            inputs.volume_start,
            inputs.lat,
            inputs.lon,
            data,
            &declared,
            self.channels.render_sender.clone(),
            self.window.clone(),
        );
    }

    /// Apply a rendered radar image to a specific pane (upload texture to overlay cache).
    fn apply_render_to_pane(
        &mut self,
        ctx: &egui::Context,
        pane_idx: usize,
        render: &crate::render_dispatch::CachedPaneRender,
        uploads: &mut PlanViewUploads,
    ) {
        use rustdar_egui::overlay_cache::{OverlayTextureData, RadarTextureMeta};
        use rustdar_geo::PlacedRaster;
        use rustdar_radar::types::ImageBounds;

        // Extract site coordinates before mutable borrow
        let (lat, lon) = {
            let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                return;
            };
            (scan_info.site.lat, scan_info.site.lon)
        };

        // Whether the picture being applied is the picture already on this
        // pane — the *same buffer*, not a buffer that compares equal.
        let already_on_screen = self
            .render
            .pane_render
            .get(pane_idx)
            .and_then(|prs| prs.cached_render.as_ref())
            .is_some_and(|cached| Arc::ptr_eq(&cached.image, &render.image));

        // Let go of the old radar overlay texture — unless it is the one about
        // to go back, in which case it is kept rather than retired and
        // re-uploaded.
        let Some(pane) = self.gui.pane_mut(pane_idx) else {
            return;
        };
        let cache = pane.overlay_cache_mut(&rustdar_source::id::known::RADAR);
        // The pane's own handle for these exact pixels, if it has one, and
        // whether that handle is **whole**.
        let retained = already_on_screen
            .then(|| match cache.held_texture() {
                Some(arriving) => Some((arriving.clone(), false)),
                None => cache.current().map(|old| (old.texture.clone(), true)),
            })
            .flatten();

        let side = render.image.width();
        let (texture, whole) = match retained {
            // The pane's own handle, preferred over anything `uploads` may hold
            // for the same raster. Not a lifetime question — see the note above
            // on why a replaced handle can just be dropped — but a churn one:
            Some(pair) => pair,
            None => {
                let counter = &mut self.texture_counter;
                let texture = uploads.handle(&render.image, || {
                    *counter += 1;
                    ctx.load_texture(
                        format!("radar_image_{counter}"),
                        Arc::clone(&render.image),
                        egui::TextureOptions::NEAREST,
                    )
                });
                // A texture minted this frame — by this call or by an earlier
                // pane in the same drain — has handed egui pixels that
                // `end_pass` has not seen yet, let alone moved. Never whole.
                (texture, false)
            }
        };

        // Cache the pixels for fast restore after suspend/resume
        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].cached_render = Some(CachedPaneRender {
                image: Arc::clone(&render.image),
                max_range_km: render.max_range_km,
                hover: Arc::clone(&render.hover),
                product: render.product,
                elevation: render.elevation,
                nyquist_ms: render.nyquist_ms,
                melting_layer_source: render.melting_layer_source,
                storm_motion: render.storm_motion,
            });
        }

        let bounds = ImageBounds::from_radar_site(lat, lon, render.max_range_km);
        let placed_raster: PlacedRaster = bounds.into();
        let pane = self.gui.pane_mut(pane_idx).unwrap();
        let data_time = self.render.data_time_for_render(pane, render);
        let placed = OverlayTextureData {
            texture,
            placed: placed_raster,
            data_generation: 0,
            render_zoom: 0,
            width: side as u32,
            height: side as u32,
            radar_meta: Some(RadarTextureMeta {
                hover: Arc::clone(&render.hover),
                lat,
                lon,
                max_range_km: render.max_range_km,
                nyquist_ms: render.nyquist_ms,
                melting_layer_source: render.melting_layer_source,
                storm_motion: render.storm_motion,
                product: crate::render_key::field_id_of(render.product),
                elevation: render.elevation,
            }),
            hit_map: None,
        };

        // **The swap, or the promise of one.**
        pane.place_radar_raster(placed, data_time, whole);

        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].last_rendered =
                Some((render.product, render.elevation));
        }
    }

    /// Show every held raster whose last band has landed.
    fn promote_uploaded_rasters(&mut self) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let renderer = &state.egui_renderer;
        self.gui
            .promote_held_rasters(|id| renderer.is_delivered(id));
    }

    /// Promote every held raster, as the frame after the last band lands does.
    #[cfg(test)]
    pub(super) fn deliver_held_rasters(&mut self) {
        self.gui.promote_held_rasters(|_| true);
    }

    /// Take the launch's one catalogue refresh and write it to the cache.
    fn poll_site_catalogue(&mut self) {
        while let Ok(response) = self.channels.site_catalogue_receiver.try_recv() {
            // A failed fetch is silent by design — offline is not an error
            // state here, it is a launch that runs on the cache. `catalogue`
            // has already logged the reason at `debug`.
            let Some(fetched) = response.catalogue else {
                continue;
            };
            let store = self.platform.kv();
            crate::site_catalogue::store_if_changed(
                store.as_deref(),
                &self.site_catalogue,
                &fetched,
            );
            self.site_catalogue = fetched;
            if self.catalogue_pending {
                self.adopt_the_first_catalogue();
            }
            if self.site_hint_pending {
                self.open_on_the_timezones_radar();
            }
        }
    }

    /// Put the first catalogue this install ever fetched into the live table.
    fn adopt_the_first_catalogue(&mut self) {
        // The picker learns the list is whole through the per-frame compose.
        self.catalogue_pending = false;
        let table = rustdar_radar::sites::resolve(
            self.site_positions
                .fixes()
                .chain(self.site_catalogue.fixes()),
        );
        log::info!(
            "first catalogue applied in-session: {} radars placed, {} listed \
             without a position",
            table.rows().len(),
            table.unplaced().len(),
        );
        // The site layer draws from its own copy of the table, so a catalogue
        // that places radars mid-session has to be handed over again or the
        // map keeps drawing the list this install booted with.
        self.gui.publish_radar_sites();
        // ...and the volumes already on screen were named against the table
        // this call just replaced. One decoded before its radar was in it
        // carries UNKNOWN, and `dispatch_pane_renders` looks the volume up
        // under that name in a still store keyed by the site -- so the render
        // was skipped, silently, and nothing else revisits it. This arrival is
        // what un-skips it, which is a re-trigger and not a retry.
        let replaced = self.gui.place_shown_volumes_against_the_table();
        if replaced > 0 {
            log::info!(
                "the catalogue placed {replaced} radar(s) whose volume was \
                 already in hand; those panes can be drawn after all",
            );
        }
    }

    /// Open on the radar nearest this device's timezone.
    fn open_on_the_timezones_radar(&mut self) {
        self.site_hint_pending = false;
        // The hint is run here rather than remembered from startup, because at
        // startup it had nothing to resolve against and chose nothing.
        let Some(zone) = self.platform.iana_timezone() else {
            return;
        };
        let Some(site) = crate::location_hint::site_for_timezone(&zone) else {
            return;
        };
        // Still a guess either way, so a later location fix may refine it.
        self.site_is_provisional = true;
        if self.gui.pane(0).is_some_and(|pane| pane.site() == site) {
            return;
        }
        log::info!("opening on {site}, nearest to timezone {zone}");
        self.handle_gui_action(
            crate::app::GuiAction::SwitchRadarSite {
                site: site.to_string(),
                pane_idx: self.gui.active_pane_idx(),
            },
            None,
        );
    }

    /// Poll for completed Level III fetch results and update scan info.
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
            if self
                .render
                .set_env_heights(&sounding.site, heights, &self.gui)
            {
                log::info!(
                    "Env heights moved for {}: dropped the renders that read them",
                    sounding.site
                );
            }
        }
        while let Ok(ml) = self.channels.melting_layer_receiver.try_recv() {
            if self.render.is_fetch_stale(&ml.site, ml.generation) {
                continue;
            }
            let Some(bytes) = ml.object else {
                continue;
            };
            log::info!(
                "Melting layer cached for {} (volume {}, {} bytes)",
                ml.site,
                ml.volume_start,
                bytes.len()
            );
            if self.render.set_melting_layer(
                &ml.site,
                crate::render_dispatch::MeltingLayerObject {
                    volume_start: ml.volume_start,
                    bytes,
                },
                &self.gui,
            ) {
                log::info!(
                    "Melting layer moved for {}: dropped the classification renders",
                    ml.site
                );
            }
        }
        while let Ok(sm) = self.channels.storm_motion_receiver.try_recv() {
            if self.render.is_fetch_stale(&sm.site, sm.generation) {
                continue;
            }
            let Some((speed_kt, direction_deg)) = sm.motion else {
                continue;
            };
            log::info!(
                "Storm motion cached for {} (volume {}): {speed_kt:.1} kt from {direction_deg:.1}°",
                sm.site,
                sm.volume_start,
            );
            if self.render.set_storm_motion(
                &sm.site,
                crate::render_dispatch::StormMotionObject {
                    volume_start: sm.volume_start,
                    motion: (speed_kt, direction_deg),
                },
                &self.gui,
            ) {
                log::info!(
                    "Storm motion moved for {}: dropped the storm-relative renders",
                    sm.site
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

            let readers = rustdar_radar::types::RadarProduct::level3_readers(&l3_resp.code);
            let elevation = fetched.message.pdb.elevation_angle();
            // The age is logged, not just carried: `latest_key` falls back to the
            // previous UTC day, so a site down since yesterday delivers a product
            // up to ~48 h old and this is currently the only place that says so.
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
                let pane_matches_site =
                    self.gui.pane(idx).is_some_and(|p| p.site() == l3_resp.site);
                if pane_matches_site
                    && self
                        .gui
                        .get_rendering_params_for_pane(idx)
                        .and_then(|(id, _)| crate::render_key::radar_field(&id))
                        .is_some_and(|p| readers.contains(&p))
                {
                    prs.last_rendered = None;
                }
            }

            // Add Level III products to the scan info for panes on this site
            for pane_idx in 0..self.gui.pane_count() {
                let pane_site = self
                    .gui
                    .pane(pane_idx)
                    .map(|p| p.site().to_string())
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
                    self.gui
                        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane {
                            pane_idx,
                            info,
                        });
                }
            }
        }
    }

    /// Poll for completed overlay rasterization results and upload textures.
    fn poll_overlay_render_results(&mut self, ctx: &egui::Context) {
        use rustdar_egui::overlay_cache::OverlayTextureData;

        while let Ok(mut resp) = self.channels.overlay_render_receiver.try_recv() {
            let id = resp.overlay_kind.clone();

            // Narrow the result to the panes that still draw this layer, and do
            // it **before the upload**.
            let gui = &mut self.gui;
            resp.pane_indices.retain(|&pane_idx| {
                let Some(pane) = gui.pane_mut(pane_idx) else {
                    return false;
                };
                let wanted = !pane.overlay_texture_is_releasable(&id);
                pane.overlay_cache_mut(&id).render_in_flight = false;
                wanted
            });
            if resp.pane_indices.is_empty() {
                continue;
            }

            let Some(image) = resp.image else {
                continue;
            };

            // Load texture once, then clone handle to all target panes.
            self.texture_counter += 1;
            // The picture's own dimensions rather than a pair carried beside it:
            let [width, height] = image.size;
            let (width, height) = (width as u32, height as u32);
            let tex_name = format!("overlay_{}", self.texture_counter);
            let texture =
                ctx.load_texture(tex_name, Arc::clone(&image), egui::TextureOptions::LINEAR);

            // Every pane still named here wants the picture: the retain above is
            // what decided that, and it also cleared every in-flight mark.
            for &pane_idx in &resp.pane_indices {
                let Some(pane) = self.gui.pane_mut(pane_idx) else {
                    continue;
                };

                let cache = pane.overlay_cache_mut(&id);

                let data = OverlayTextureData {
                    texture: texture.clone(),
                    placed: rustdar_geo::PlacedRaster::of(resp.geo_bounds),
                    data_generation: resp.generation,
                    render_zoom: resp.zoom,
                    width,
                    height,
                    radar_meta: None,
                    hit_map: resp.hit_map.clone(),
                };
                if cache.current().is_none() {
                    cache.show(data);
                } else {
                    cache.hold(data, None);
                }
            }
        }
    }

    /// Apply the storm motion override the settings panel holds, and if it
    /// moved, invalidate everything derived with the old vector.
    fn apply_storm_motion_override(&mut self) -> bool {
        if self.gui.storm_motion_mid_edit() {
            return false;
        }
        // Editing the vector changes nothing else about a pane, so the derived
        // storm-relative tilts have to be invalidated explicitly.
        let storm_motion = self.gui.storm_motion_override.sample();
        if !self
            .render
            .set_storm_motion_choice(storm_motion, self.gui.srv_fallback)
        {
            return false;
        }
        self.volume_store
            .evict_product(&rustdar_radar::fields::known::STORM_RELATIVE_VELOCITY);
        for pane_idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            if pane.selected_product() != rustdar_radar::fields::known::STORM_RELATIVE_VELOCITY {
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

    /// Move `RenderInput::extract` to volume arrival for `site`:
    pub(super) fn refresh_extract_cache_for_site(&mut self, site: &str) {
        self.render.retain_extracts(|key| key.site != site);
        for pane_idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane(pane_idx) else {
                continue;
            };
            if !pane.is_map() || pane.site() != site {
                continue;
            }
            let Some((product, elevation)) = pane
                .get_rendering_params()
                .and_then(|(id, e)| Some((rustdar_radar::fields::product_for(&id)?, e)))
            else {
                continue;
            };
            // Level III renders from fetched objects, not from the volume —
            // there is no extraction to move.
            if product.is_level3() {
                continue;
            }
            let Some(scan_info) = pane.scan_info.as_ref() else {
                continue;
            };
            // The same stores and the same names the dispatch reads: the key
            // is the pane's site, the volume is looked up under the
            // scan_info's, and the coordinates are the scan_info's.
            let Some((data, _declared)) = self.volumes.still_for(scan_info.site.name) else {
                continue;
            };
            let (lat, lon) = (scan_info.site.lat, scan_info.site.lon);
            let (key, storm_motion, env_heights) =
                self.render
                    .extract_tuple_for(site, scan_info.timestamp, product, elevation);
            #[cfg(not(target_arch = "wasm32"))]
            {
                let sender = self.render.extract_sender();
                let window = self.window.clone();
                self.tokio_runtime.spawn_blocking(move || {
                    if let Some(input) = rustdar_radar::render_input::RenderInput::extract(
                        &data,
                        elevation,
                        product,
                        lat,
                        lon,
                        storm_motion,
                        env_heights,
                    ) {
                        let _ = sender.send((key, std::sync::Arc::new(input)));
                        // Wake the pump so the Apply row drains this before
                        // the next dispatch rather than on some later event.
                        crate::app::notify_redraw(&window);
                    }
                });
            }
            #[cfg(target_arch = "wasm32")]
            {
                if let Some(input) = rustdar_radar::render_input::RenderInput::extract(
                    &data,
                    elevation,
                    product,
                    lat,
                    lon,
                    storm_motion,
                    env_heights,
                ) {
                    self.render
                        .populate_extract(key, std::sync::Arc::new(input));
                }
            }
        }
    }

    /// Check all panes for needed background renders and spawn render threads.
    fn dispatch_pane_renders(&mut self, ctx: &egui::Context) {
        self.apply_storm_motion_override();
        let mut uploads = PlanViewUploads::default();
        for pane_idx in 0..self.gui.pane_count() {
            if self.gui.pane_has_no_plan_view(pane_idx) {
                continue;
            }
            if let Some((product, elevation)) = self
                .gui
                .get_rendering_params_for_pane(pane_idx)
                .and_then(|(id, e)| Some((rustdar_radar::fields::product_for(&id)?, e)))
            {
                let prs = &self.render.pane_render[pane_idx];
                let needs_render = prs
                    .last_rendered
                    .map(|(last_prod, last_elev)| {
                        last_prod != product || (last_elev - elevation).abs() > ELEVATION_TOLERANCE
                    })
                    .unwrap_or(true);

                if needs_render && !prs.render_in_flight() {
                    let Some(pane) = self.gui.pane(pane_idx) else {
                        continue;
                    };
                    let pane_site = pane.site().to_string();

                    if let Some(cached) = self.render.get_cached_render(
                        &pane_site,
                        product,
                        rustdar_radar::types::RenderView::PlanView,
                        elevation,
                    ) {
                        let render_result = crate::render_dispatch::CachedPaneRender {
                            image: Arc::clone(&cached.image),
                            max_range_km: cached.max_range_km,
                            hover: Arc::clone(&cached.hover),
                            product,
                            elevation,
                            nyquist_ms: cached.nyquist_ms,
                            melting_layer_source: cached.melting_layer_source,
                            storm_motion: cached.storm_motion,
                        };
                        log::info!(
                            "Reusing cached render for pane {}: {:?} at {:.1}°",
                            pane_idx,
                            product,
                            elevation
                        );
                        self.apply_render_to_pane(ctx, pane_idx, &render_result, &mut uploads);
                        continue;
                    }

                    // A sibling pane is already having this exact picture made.
                    if self
                        .render
                        .plan_view_in_flight(&pane_site, product, elevation)
                    {
                        continue;
                    }

                    let Some(scan_info) = pane.scan_info.as_ref() else {
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
                    } else if let Some((data, declared)) =
                        self.volumes.still_for(scan_info.site.name)
                    {
                        // Handed back as refcounts, so the dispatcher below can be
                        // borrowed mutably in the same statement.
                        self.render.spawn_level2_render(
                            pane_idx,
                            &params,
                            &pane_site,
                            data,
                            &declared,
                            scan_info.timestamp,
                            self.channels.render_sender.clone(),
                            self.window.clone(),
                        );
                    }
                }
            } else if pane_idx < self.render.pane_render.len() {
                // Only clear the radar texture if no scan data is loaded for this pane.
                let has_scan = self
                    .gui
                    .pane(pane_idx)
                    .is_some_and(|p| p.scan_info.is_some());
                if !has_scan && let Some(pane) = self.gui.pane_mut(pane_idx) {
                    let cache = pane.overlay_cache_mut(&rustdar_source::id::known::RADAR);
                    cache.clear();
                }
                self.render.pane_render[pane_idx].last_rendered = None;
            }
        }
    }

    /// Cut a fresh cross-section for every section pane whose picture no longer
    /// matches what it is aimed at.
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
                .is_some_and(|p| p.render_in_flight())
            {
                continue;
            }

            let site = target.volume.site.clone();
            let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                continue;
            };
            let (lat, lon) = (scan_info.site.lat, scan_info.site.lon);

            let base = self.volumes.base_for(site.as_str());
            let overlay = self.chunk_feeds.snapshot(site.as_str());

            if let Some(reason) = section_source_refusal(
                base.as_ref().map(|(scan, _)| scan.as_ref()),
                overlay.as_ref().map(|live| live.scan.as_ref()),
            ) {
                self.mark_section_unavailable(pane_idx, reason);
                continue;
            }
            if crate::render_key::radar_field(&target.product)
                .and_then(rustdar_radar::derive::volume_slot)
                .is_none()
            {
                // Permanent for this product, so the key *is* written: nothing
                // about this volume will make a column integral sliceable, and
                // re-asking every frame would be a busy loop with no output.
                self.mark_section_unavailable(
                    pane_idx,
                    rustdar_egui::pane::SectionUnavailable::ProductHasNoVerticalStructure(
                        target.product.clone(),
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

            // The extraction is radar's own and keyed by radar's field; the
            // target names it by id, and the arm above already refused an id
            // with no vertical slot.
            let Some(product) = crate::render_key::radar_field(&target.product) else {
                continue;
            };
            // Captured before the closure: the user's storm motion vector,
            // for the worker-side SRV derivation. The extraction keeps it
            // only on an SRV payload.
            let motion = self.render.storm_motion_override_kt();
            // Read here, on the frame thread, for the reason `motion` above it
            // is: the closure runs later, and a rung read inside it could be a
            // different one from the rung the key was built with.
            let fallback = self.render.srv_fallback();
            let extract = move || {
                let current = rustdar_radar::current::resolve(
                    base.as_ref().map(|(scan, declared)| {
                        rustdar_radar::nyquist::Volume::new(scan, declared)
                    }),
                    overlay.as_ref().map(|live| {
                        rustdar_radar::nyquist::Volume::new(&live.scan, &live.declared)
                    }),
                )?;
                rustdar_radar::render_input::RenderInput::extract_volume_parts(
                    current.pattern(),
                    current.sweeps(),
                    product,
                    lat,
                    lon,
                    motion,
                )
                // The same stamp `App::extract_current_volume` applies, and for
                // the same reason: without it this payload's worker estimates
                // the velocity fold limits the merge just declared.
                .map(|input| {
                    input
                        .with_declared_nyquist(current.declared_nyquist())
                        .with_srv_fallback(fallback)
                })
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
                    self.mark_section_unavailable(
                        pane_idx,
                        rustdar_egui::pane::SectionUnavailable::ProductMissingFromVolume(
                            target.product.clone(),
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
                        section.rendered_for = Some(target);
                        section.unavailable = None;
                    }
                }
            }
        }
    }

    /// What pane `pane_idx` would have to cut to be showing the truth, or `None`
    /// if it is not a section pane, has no line, or has no volume yet.
    fn section_target_for_pane(
        &mut self,
        pane_idx: usize,
    ) -> Option<rustdar_egui::pane::SectionTarget> {
        let pane = self.gui.pane(pane_idx)?;
        let section = pane.cross_section()?;
        let line = section.line?;
        let product = rustdar_radar::fields::product_for(&pane.selected_product())?;
        let site = pane.site().to_string();
        let Some(collected) = pane.scan_info.as_ref().map(|s| s.timestamp) else {
            self.mark_section_unavailable(
                pane_idx,
                rustdar_egui::pane::SectionUnavailable::AwaitingVolume,
            );
            return None;
        };
        let ladder = self
            .current_ladder_fingerprint(site.as_str(), product)
            .unwrap_or(0);
        Some(rustdar_egui::pane::SectionTarget {
            volume: rustdar_egui::pane::VolumeStamp { site, collected },
            product: crate::render_key::field_id_of(product),
            line,
            ladder,
        })
    }

    /// Record why a section pane has no picture, leaving whatever it is showing
    /// alone.
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
                state.render_finished();
            }

            if self.render.is_render_stale(sr.generation) {
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
            // Assigning retires the cut this pane was showing; see the note in
            // `App::apply_render_to_pane`.
            section_state.texture = Some(texture);
            section_state.section = Some(Arc::from(cut));
            section_state.unavailable = None;
        }
    }

    /// Upload a cut's raster and hand back the handle. The **one** place a
    /// section becomes a texture.
    fn upload_section_raster(
        &mut self,
        ctx: &egui::Context,
        cut: &rustdar_radar::xsect::CrossSection,
    ) -> egui::TextureHandle {
        self.texture_counter += 1;
        let color_image = egui::ColorImage::from_rgba_premultiplied(
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
    fn restore_section_textures(&mut self, ctx: &egui::Context) {
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
    pub(super) fn restore_cached_render(&mut self, ctx: &egui::Context) {
        use rustdar_egui::overlay_cache::{OverlayTextureData, RadarTextureMeta};
        use rustdar_geo::PlacedRaster;
        use rustdar_radar::types::ImageBounds;

        // Every raster still arriving is let go of first, on **every** pane and
        // whether or not this goes on to restore one.
        self.gui.release_held_rasters();

        // Section panes first, and through their own loop: the one below is
        // bounded by `pane_render.len()` and skips every pane with no plan
        // view, which is every section pane there is.
        self.restore_section_textures(ctx);

        // Panes sharing a raster shared it before the context died too:
        let mut uploads = PlanViewUploads::default();

        for pane_idx in 0..self.render.pane_render.len().min(self.gui.pane_count()) {
            if self.gui.pane_has_no_plan_view(pane_idx) {
                continue;
            }
            let Some(ref cached) = self.render.pane_render[pane_idx].cached_render else {
                continue;
            };
            let max_range_km = cached.max_range_km;
            let product = cached.product;
            let elevation = cached.elevation;
            let nyquist_ms = cached.nyquist_ms;
            let melting_layer_source = cached.melting_layer_source;
            let storm_motion = cached.storm_motion;

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

            let side = cached.image.width();
            let image = Arc::clone(&cached.image);
            let texture = {
                let counter = &mut self.texture_counter;
                uploads.handle(&image, || {
                    *counter += 1;
                    ctx.load_texture(
                        format!("radar_image_{counter}"),
                        Arc::clone(&image),
                        egui::TextureOptions::NEAREST,
                    )
                })
            };

            let bounds = ImageBounds::from_radar_site(lat, lon, max_range_km);
            let placed: PlacedRaster = bounds.into();
            if let Some(pane) = self.gui.pane_mut(pane_idx) {
                let cache = pane.overlay_cache_mut(&rustdar_source::id::known::RADAR);
                // Showing retires whatever the pane was showing; see the note
                // in `App::apply_render_to_pane`.
                cache.show(OverlayTextureData {
                    texture,
                    placed,
                    data_generation: 0,
                    render_zoom: 0,
                    width: side as u32,
                    height: side as u32,
                    radar_meta: Some(RadarTextureMeta {
                        hover: Arc::clone(&cached.hover),
                        lat,
                        lon,
                        max_range_km,
                        nyquist_ms,
                        melting_layer_source,
                        storm_motion,
                        product: crate::render_key::field_id_of(product),
                        elevation,
                    }),
                    hit_map: None,
                });
            }
            self.render.pane_render[pane_idx].last_rendered = Some((product, elevation));
        }
    }

    /// Try to acquire the next surface texture for rendering.
    fn get_surface_texture(
        surface: &wgpu::Surface,
        _finished: &rustdar_gpu::egui_renderer::PreparedFrame,
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
    pub(super) fn present_frame(&mut self, size_in_pixels: [u32; 2]) -> std::time::Duration {
        let state = self.state.as_mut().unwrap();
        let window = self.window.as_ref().unwrap();

        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        let mirror_rects = self.gui.mirror_source_rects();
        let demand = self
            .volume_painter
            .as_ref()
            .and_then(|painter| painter.take_floor_demand());
        let mirror_target = if mirror_rects.is_empty() {
            if let Some(resources) = state
                .egui_renderer
                .callback_resources_mut()
                .get_mut::<rustdar_volumetric::bridge::VolumeResources>()
            {
                resources.release_mirror();
            }
            None
        } else {
            let points = state.egui_renderer.context().pixels_per_point();
            // Sized in **points**, from the UI rather than from the surface:
            let size_in_points = self.gui.mirror_size_points();
            let plan = self.mirror_rungs.observe(
                demand,
                [size_in_points.x, size_in_points.y],
                points,
                rustdar_gpu::egui_renderer::MirrorLimits::for_device(
                    state.device.limits().max_texture_dimension_2d,
                    self.budgets.mirror_bytes,
                ),
            );
            let format = state.egui_renderer.attachment_config().color_format;
            let device = state.device.clone();
            state
                .egui_renderer
                .callback_resources_mut()
                .get_mut::<rustdar_volumetric::bridge::VolumeResources>()
                .map(|resources| {
                    (
                        resources.ensure_mirror(&device, plan.size_in_pixels, format),
                        plan,
                    )
                })
        };
        let mirror =
            mirror_target
                .as_ref()
                .map(|(view, plan)| rustdar_gpu::egui_renderer::MirrorRequest {
                    view,
                    size_in_pixels: plan.size_in_pixels,
                    pixels_per_point: plan.pixels_per_point,
                    source_rects: &mirror_rects,
                });

        // Finish egui's pass and upload its textures, THEN ask for a surface.
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
                frame.submit(&state.queue, encoder);
                state.egui_renderer.free_textures(frame.textures_to_free());

                if matches!(status, SurfaceStatus::Lost) {
                    let volume_on_screen =
                        self.gui.panes().iter().any(|pane| {
                            pane.render_view() == rustdar_radar::types::RenderView::Volume
                        });
                    if volume_on_screen {
                        let losses = rustdar_volumetric::degrade::note_surface_loss_with_volume();
                        log::warn!(
                            "wgpu surface lost with a 3D volume on screen ({losses} so far)"
                        );
                    }

                    self.back_off_budgets();

                    // Surface is irrecoverably lost (e.g. display changed on a
                    // foldable). Drop the entire rendering state so the next
                    // handle_redraw() lazily recreates it with a fresh surface.
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

    /// **Build the loops that were waiting on a listing that has now
    /// landed.** Populates each pane's frame list and kicks off its downloads
    /// (throttled).
    ///
    /// The frames come from the layer, not from the arrival: the listing is
    /// filed by `apply_frame_listing` under the site it was listed for, and
    /// this reads it back through `list_frames` scoped to each pane's own
    /// site. A pane on another site therefore sees nothing of it, which is
    /// what keeps two sites' stamps out of one list.
    pub(super) fn accept_loop_scan_listings(&mut self) {
        let arrived = std::mem::take(&mut self.loop_listings_arrived);
        if arrived.is_empty() {
            return;
        }
        let allocation = self.loop_allocation();
        let budgets = self.budgets;
        let config = self.fetch_config();
        for (site, range) in arrived {
            let span_secs = (range.1 - range.0).num_seconds();
            let mut built: Vec<(usize, FramePlan, rustdar_radar::types::RadarProduct)> = Vec::new();
            {
                let (panes, overlays) = self.gui.panes_and_overlays_mut();
                for (pane_idx, pane) in panes.iter_mut().enumerate() {
                    // Only a pane still waiting for a listing over this very
                    // window: two panes looping one site with two spans ask
                    // two questions, and neither may be answered with the
                    // other's.
                    if !pane.loop_state().is_active()
                        || pane.loop_state().phase
                            != rustdar_egui::pane::LoopPhase::FetchingScanList
                        || pane.loop_state().span_secs as i64 != span_secs
                    {
                        continue;
                    }
                    pane.hydrate_layer_states(overlays, pane_idx);
                    let Some(product) =
                        rustdar_radar::fields::product_for(&pane.selected_product())
                    else {
                        continue;
                    };
                    // The whole-pane cap divides across the layers this pane is
                    // animating, and it is counted HERE — where the budget is
                    // consumed — not pushed down with it.
                    let animating = pane.animating_layers().count();
                    let frames: Vec<chrono::NaiveDateTime> = {
                        let view = pane.view(pane_idx);
                        let pane_ref = view.layer(&rustdar_source::id::known::RADAR);
                        overlays
                            .list_frames(
                                &rustdar_source::id::known::RADAR,
                                &config,
                                &pane_ref,
                                range,
                            )
                            .frames
                            .iter()
                            .map(|frame| frame.valid)
                            .collect()
                    };
                    // Whether this listing is still wanted, and what it makes of the
                    // frame list, is decided in one place — including refusing a
                    // listing for a site the pane's loop has since moved off.
                    let Some(plan) = accept_scan_listing(
                        allocation,
                        &budgets,
                        pane.loop_state_mut(),
                        &site,
                        frames,
                        animating,
                    ) else {
                        continue;
                    };
                    built.push((pane_idx, plan, product));
                }
            }
            for (pane_idx, plan, product) in built {
                log::info!(
                    "Loop: populated {} {} frames for pane {}",
                    plan.frames.len(),
                    plan.site,
                    pane_idx
                );
                // Store the frame plan — with the site it was listed for — then derive
                // the queue for whichever datasource this pane's product reads and
                // dispatch the first batch.
                self.loop_mgr.set_plan(pane_idx, plan);
                self.loop_mgr.plan_downloads_for(pane_idx, product);
                self.dispatch_pending_loop_downloads(pane_idx);
                self.dispatch_pending_loop_l3_pairings(pane_idx);
            }
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
    fn dispatch_pending_loop_l3_pairings(&mut self, pane_idx: usize) {
        let Some(PendingL3Pairings {
            site,
            product,
            queue,
        }) = self.loop_mgr.extract_pending_l3(pane_idx)
        else {
            return;
        };
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

        let slots = self
            .loop_mgr
            .available_slots(self.budgets.concurrent_loop_downloads);
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
        let slots = self
            .loop_mgr
            .available_slots(self.budgets.concurrent_loop_downloads);
        if slots == 0 {
            return;
        }

        // We need to look up cached/in_flight state while modifying the pending
        // queue, and both live in loop_mgr, so the queue is extracted completely,
        // processed, and put back.
        let Some(PendingDownloads { site, mut queue }) = self.loop_mgr.extract_pending(pane_idx)
        else {
            return;
        };

        // Filter out timestamps already cached or in flight for this site
        let mut batch = Vec::new();
        while !queue.is_empty() && batch.len() < slots {
            let ts = *queue.front().unwrap();
            if self.loop_mgr.is_cached(&site, &ts) || self.loop_mgr.is_in_flight(&site, &ts) {
                // Already have or fetching this scan — remove from pending
                queue.pop_front();
            } else {
                batch.push(queue.pop_front().unwrap());
            }
        }

        // **The layer resolves each volume to an archive object**; nothing
        // here holds one. A stamp it cannot resolve is a volume no listing of
        // this pane's site named, so it is dropped rather than retried
        // forever — the frame retires the way an unrenderable one does.
        let config = self.fetch_config();
        let listed = site.clone();
        let tasks: Vec<(chrono::NaiveDateTime, rustdar_source::handler::FetchTask)> = self
            .with_layer_pane(
                pane_idx,
                &rustdar_source::id::known::RADAR,
                |overlays, pane_ref| {
                    batch
                        .into_iter()
                        .filter_map(|ts| {
                            let stamp = rustdar_source::time::FrameStamp {
                                valid: ts,
                                run: None,
                            };
                            let task = overlays.fetch_frame(
                                &rustdar_source::id::known::RADAR,
                                &config,
                                pane_ref,
                                &stamp,
                            );
                            if task.is_none() {
                                log::warn!(
                                    "Loop: no {listed} archive object is listed for {ts}; \
                                     that frame cannot be fetched",
                                );
                            }
                            task.map(|task| (ts, task))
                        })
                        .collect()
                },
            )
            .unwrap_or_default();

        let spawned = tasks.len();

        for (ts, task) in tasks {
            self.loop_mgr.mark_in_flight(&site, ts);
            self.spawn_frame_fetch_task(
                rustdar_source::time::FrameStamp {
                    valid: ts,
                    run: None,
                },
                task,
            );
        }

        // Put the queue back, still carrying its own site
        self.loop_mgr
            .insert_pending(pane_idx, PendingDownloads { site, queue });

        if spawned > 0 {
            self.loop_mgr.add_spawned(spawned);
        }
    }

    /// Poll for completed loop frame render results and upload textures.
    fn poll_loop_render_results(&mut self, ctx: &egui::Context) {
        while let Ok(mut rr) = self.channels.loop_render_receiver.try_recv() {
            let origin_pane = rr.pane_idx;
            // Resolved before the pane is borrowed, and off the *response*
            // rather than off the pane — see `frame_gates`.
            let gates = frame_gates(&self.loop_mgr, &rr);

            let Some(pane) = self.gui.pane_mut(origin_pane) else {
                continue;
            };

            let counter = &mut self.texture_counter;
            let Some(texture) =
                accept_render_result(pane.loop_state_mut(), &mut rr, gates, |color_image| {
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
            if self.gui.pane_layer_linked(origin_pane) {
                for sibling_idx in 0..self.gui.pane_count() {
                    if sibling_idx == origin_pane
                        || self.gui.pane_has_no_plan_view(sibling_idx)
                        || !self.gui.pane_layer_linked(sibling_idx)
                    {
                        continue;
                    }
                    let Some(sibling_loop) = self.gui.pane(sibling_idx).map(|p| p.loop_state())
                    else {
                        continue;
                    };
                    if !sibling_loop.is_rendered_for(&rr.target) {
                        continue;
                    }
                    let sweep = broadcast_sweep(&self.loop_mgr, sibling_loop, &rr);

                    let Some(sibling) = self.gui.pane_mut(sibling_idx) else {
                        continue;
                    };
                    let Some(sframe) = sibling.loop_state_mut().frame_accepting_broadcast_mut(
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
                    sframe.image = Some(rustdar_egui::pane::LoopFrameImage::PlanView(
                        rendered_image(&rr, &texture, frame_gates(&self.loop_mgr, &rr)),
                    ));
                }
            }
        }
    }

    /// Promote loops from `Rendering` to `Ready` once every frame they intend to
    /// render has settled — or off entirely when none of them can be rendered at
    /// all — then start playback for the panes that are ready.
    pub(super) fn update_loop_readiness(&mut self) {
        let allocation = self.loop_allocation();
        let budgets = self.budgets;
        let mut abandoned = Vec::new();
        for pidx in 0..self.gui.pane_count() {
            let loop_mgr = &self.loop_mgr;
            let Some(p) = self.gui.pane_mut(pidx) else {
                continue;
            };
            let budget = loop_render_budget(allocation, p.loop_state(), &budgets);
            if settle_loop_phase(loop_mgr, pidx, p.loop_state_mut(), budget) {
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

        // Synchronized playback start: the time-linked looping panes wait
        // for each other; an unlinked loop starts on its own readiness.
        self.sync_loop_playback_start();
    }

    /// Start loop playback for panes that are ready, holding the
    /// time-linked ones together (M11: `PaneState::time_link` is the gate —
    /// loop start synchronisation is a shared-time behaviour, so it follows
    /// the time link, not the layer link). A linked ready pane waits while
    /// any linked looping pane is not ready; an unlinked ready pane starts
    fn sync_loop_playback_start(&mut self) {
        let pane_count = self.gui.pane_count();
        let multi = pane_count > 1;

        // Collect readiness status for all panes with active loops
        let mut ready_panes: Vec<usize> = Vec::new();
        let mut not_ready_panes: Vec<usize> = Vec::new();
        for idx in 0..pane_count {
            if self.gui.pane_cannot_loop(idx) {
                continue;
            }
            let Some(pane) = self.gui.pane(idx) else {
                continue;
            };
            let ls = pane.loop_state();
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

        // The linked group starts as one: a time-linked ready pane waits
        // while any time-linked looping pane is still catching up. Unlinked
        // panes sit outside both halves of that sentence.
        let hold_linked = multi
            && not_ready_panes
                .iter()
                .any(|&idx| self.gui.pane_time_linked(idx));

        // Start the startable panes with the same instant and frame position
        let now = web_time::Instant::now();
        for idx in ready_panes {
            if hold_linked && self.gui.pane_time_linked(idx) {
                continue;
            }
            let pane = self.gui.pane_mut(idx).unwrap();
            let ls = pane.loop_state_mut();
            ls.phase = rustdar_egui::pane::LoopPhase::Playing;
            ls.last_advance = Some(now);
            // Align all panes to the last frame so they start from the same
            // position — said as a clock rather than as an index: `Live` is
            // "the newest there is", which is the same frame on every pane.
            pane.set_time_mode(rustdar_egui::pane::TimeMode::Live);
        }
    }

    /// Advance loop playback for all panes with active playing loops.
    fn advance_loop_playback(&mut self) {
        let now = web_time::Instant::now();

        for pane_idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            // The pane's own posture, which every pane carries the same copy
            // of — see `Gui::set_loop_speed_fps`.
            let interval = loop_interval(pane.time.speed_fps);
            let mode = pane.time.mode;
            // The pane's time-primary layer — the topmost one animating — is
            // whose stamps the clock walks. On a radar pane that is radar,
            // which sits above the model in the draw order.
            let Some(id) = pane.clock_layer().cloned() else {
                continue;
            };
            let ls = pane.time_state_mut(&id);
            if !ls.is_active() || !ls.is_playing() || ls.frames.is_empty() {
                continue;
            }

            let should_advance = ls
                .last_advance
                .map(|last| now.duration_since(last) >= interval)
                .unwrap_or(true);

            if should_advance {
                ls.last_advance = Some(now);
                // Skip to the next frame that has a rendered texture, and move
                // the pane's CLOCK onto that frame's stamp rather than the
                // playhead onto its index: the playhead is derived from the
                // clock, and every other layer on the pane rides the same one.
                let num_frames = ls.frames.len();
                let from = ls.frame_at(mode);
                let landed = (1..=num_frames)
                    .map(|offset| (from + offset) % num_frames)
                    .find(|&candidate| ls.frames[candidate].image.is_some())
                    .map(|candidate| ls.frames[candidate].timestamp);
                if let Some(stamp) = landed {
                    pane.set_time_mode(rustdar_egui::pane::TimeMode::AsOf(stamp));
                }
            }
        }
    }

    /// Dispatch renders for loop frames around the playhead that have
    /// downloaded scan data but no rendered texture yet.
    fn loop_demand(&self) -> LoopDemand {
        let mut demand = LoopDemand::default();
        let mut seen: Vec<(
            String,
            rustdar_radar::types::RadarProduct,
            Option<rustdar_egui::pane::VolumeLoopKey>,
        )> = Vec::new();
        for pane_idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane(pane_idx) else {
                continue;
            };
            let ls = pane.loop_state();
            if !ls.is_active() {
                continue;
            }
            let already = if ls.view == rustdar_radar::types::RenderView::Volume {
                let Some(product) = loop_product(ls) else {
                    continue;
                };
                let key = (
                    radar_layer::site(ls).to_string(),
                    product,
                    ls.volume_key().cloned(),
                );
                let seen_before = seen.contains(&key);
                if !seen_before {
                    seen.push(key);
                }
                seen_before
            } else {
                false
            };
            demand.add(ls.view, already);
        }
        demand
    }

    /// The division of the pool in force, after the dwell and the dead band
    /// have had their say.
    pub(super) fn observe_loop_demand(&mut self) -> LoopAllocation {
        let demand = self.loop_demand();
        self.loop_pool_state.observe(
            self.loop_pool,
            LoopFrameModel::from_budgets(&self.budgets),
            demand,
        )
    }

    /// The allocation in force. See [`Self::observe_loop_demand`].
    pub(super) fn loop_allocation(&self) -> LoopAllocation {
        self.loop_pool_state.allocation()
    }

    /// Step the whole budget set down after the device refused, and remember it.
    pub(super) fn back_off_budgets(&mut self) {
        // The bracket the *resolved* budgets carry, not `for_target`'s: the two
        // are the same figures today because no bracket promotes the pool, and
        // reading the resolved one is what keeps them the same when one does.
        if self
            .loop_pool
            .back_off(crate::loop_pool::LoopPoolLimits::from_budgets(
                &self.budgets,
            ))
        {
            let bytes = self.loop_pool.bytes();
            log::warn!(
                "Loop pool: backed off to {} MiB after a lost surface",
                bytes / (1024 * 1024),
            );
            if let Some(memo) = self.device_profile.memo.as_mut() {
                memo.loop_pool_bytes = Some(bytes);
            }
            crate::loop_pool::remember(self.platform.kv().as_deref(), bytes);
        }

        let memo = self
            .device_profile
            .memo
            .get_or_insert_with(Default::default);
        let stepped = memo.steps_back.saturating_add(1);
        memo.steps_back = stepped;
        let resolved = rustdar_device_profile::budget::resolve(&self.device_profile);
        // Compared with the count itself held equal, because the count is a
        // field of what is being compared: `steps_back` always differs after an
        // increment, and what is being asked is whether *the budgets* moved.
        let same_but_for_the_count = rustdar_device_profile::budget::Budgets {
            steps_back: self.budgets.steps_back,
            ..resolved
        };
        if same_but_for_the_count == self.budgets {
            // Every rung this ladder owns is already at its stop. Roll the count
            // back rather than persisting a number that describes nothing, so
            // the memo stays a position on the ladder.
            if let Some(memo) = self.device_profile.memo.as_mut() {
                memo.steps_back = stepped.saturating_sub(1);
            }
            return;
        }
        log::warn!(
            "Budgets: stepped down to rung {stepped} after a lost surface: {:?} 3D quality \
             ceiling, {} MiB of offscreen, {:?} grid cells",
            resolved.quality_ceiling,
            resolved.offscreen_bytes / (1024 * 1024),
            resolved.grid_cells,
        );
        self.budgets = resolved;
        crate::budget_memo::remember_steps(self.platform.kv().as_deref(), stepped);
    }

    fn dispatch_loop_renders(&mut self) {
        let allocation = self.observe_loop_demand();
        let budgets = self.budgets;
        // Panes whose product moved to another datasource, so the frames now need
        // bytes nothing is fetching. Collected here and acted on below, because
        // re-deriving a queue needs `loop_mgr` while the pane is borrowed.
        let mut replan: Vec<(usize, rustdar_radar::types::RadarProduct)> = Vec::new();
        // Panes whose 3D loop must let go of every grid it holds **before**
        // anything is built for the new key. See `VolumeStore::retain_set`:
        let mut release_volume_sets: Vec<usize> = Vec::new();
        // Panes whose loop is no longer active and whose download queue is
        // therefore serving nobody. Collected for the same borrow reason.
        let mut retire_queues: Vec<usize> = Vec::new();
        let motion_override = self.render.storm_motion_override_kt();
        for pane_idx in 0..self.gui.pane_count() {
            if self.gui.pane_cannot_loop(pane_idx) {
                self.loop_mgr.remove_pending(pane_idx);
                continue;
            }
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            let Some(product) = rustdar_radar::fields::product_for(&pane.selected_product()) else {
                continue;
            };
            let elevation = pane.selected_elevation();
            let section_key = pane.cross_section().and_then(|s| s.line).map(|line| {
                rustdar_egui::pane::SectionLoopKey::new(
                    line,
                    (product == rustdar_radar::types::RadarProduct::StormRelativeVelocity)
                        .then_some(motion_override)
                        .flatten(),
                    self.render.srv_fallback(),
                )
            });
            // The volume half of the key, for a 3D loop: the ground the frames
            // are resampled over and the vector they are derived with. See
            // `VolumeLoopKey`.
            let volume_key = pane.volume().map(|v| {
                rustdar_egui::pane::VolumeLoopKey::new(
                    // The pane's stored region — see `VolumePane::region`.
                    v.region,
                    (product == rustdar_radar::types::RadarProduct::StormRelativeVelocity)
                        .then_some(motion_override)
                        .flatten(),
                    self.render.srv_fallback(),
                )
            });
            let ls = pane.loop_state_mut();
            if !ls.is_active() {
                retire_queues.push(pane_idx);
                continue;
            }
            if ls.frames.is_empty() {
                continue;
            }

            let view_key = match ls.view {
                rustdar_radar::types::RenderView::CrossSection => {
                    section_key.map(rustdar_egui::pane::LoopViewKey::Section)
                }
                rustdar_radar::types::RenderView::Volume => {
                    volume_key.map(rustdar_egui::pane::LoopViewKey::Volume)
                }
                rustdar_radar::types::RenderView::PlanView => None,
            };

            if ls.retarget_renders_keyed(
                &crate::render_key::field_id_of(product),
                elevation,
                view_key,
            ) {
                if ls.view == rustdar_radar::types::RenderView::Volume {
                    release_volume_sets.push(pane_idx);
                }
                log::debug!(
                    "Loop: pane {} retargeted to {:?} at {:.1}°, re-rendering all frames",
                    pane_idx,
                    product,
                    elevation
                );
                replan.push((pane_idx, product));
                continue;
            }

            // Evict textures from frames far from the playhead to cap memory usage.
            ls.evict_textures_outside_render_set(loop_render_budget(allocation, ls, &budgets));
        }
        for pane_idx in retire_queues {
            self.loop_mgr.remove_pending(pane_idx);
            // A torn-down 3D loop's grids go with its queue. Without this the
            // resident set outlives the loop that asked for it, and 512 MiB
            // stays allocated for a pane that is showing a live volume.
            if self.volume_store.holds_set(pane_idx) {
                self.volume_store.release_set(pane_idx);
                if let Some(pane) = self.gui.pane_mut(pane_idx)
                    && let Some(volume) = pane.volume_mut()
                {
                    volume.rendered_for = None;
                }
            }
        }
        // Ahead of every dispatch below, which is the whole point of the rule:
        for pane_idx in release_volume_sets {
            let dropped = self.volume_store.release_set(pane_idx);
            log::debug!(
                "3D loop: pane {pane_idx} retargeted, released its resident set ({dropped} grids \
                 freed)",
            );
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
        let mut to_mark_failed: Vec<(usize, usize)> = Vec::new();

        let pane_count = self.gui.pane_count();

        // Cross-section cuts to dispatch, and the running count that paces them.
        let mut to_cut: Vec<LoopSectionRequest> = Vec::new();

        let mut to_build: Vec<LoopVolumeRequest> = Vec::new();

        for pane_idx in 0..pane_count {
            if self.gui.pane_cannot_loop(pane_idx) {
                continue;
            }
            let linked = self.gui.pane_layer_linked(pane_idx);
            let Some(pane) = self.gui.pane(pane_idx) else {
                continue;
            };
            let ls = pane.loop_state();
            if !ls.is_active() || ls.frames.is_empty() {
                continue;
            }

            let site_lat = radar_layer::coords(ls).0;
            let site_lon = radar_layer::coords(ls).1;

            // Set by `retarget_renders` in the loop above for every active, non-empty
            // loop. Carried through the plan so the dedup, the donor search and the
            // dispatch stamp all read the one value instead of re-deriving it.
            let Some(target) = ls.rendered_for.clone() else {
                continue;
            };

            // The intended render set — shared with the readiness check so the two
            // cannot drift apart (see `LayerTimeState::render_set_settled`).
            let indices = ls.render_set_indices(loop_render_budget(allocation, ls, &budgets));

            if ls.view == rustdar_radar::types::RenderView::Volume {
                let Some(key) = ls.volume_key().cloned() else {
                    for &idx in &indices {
                        to_mark_failed.push((pane_idx, idx));
                    }
                    continue;
                };
                for &idx in &indices {
                    let frame = &ls.frames[idx];
                    let volume_target = rustdar_egui::pane::VolumeTarget {
                        volume: rustdar_egui::pane::VolumeStamp {
                            site: target.site.clone(),
                            collected: frame.timestamp,
                        },
                        product: target.product.clone(),
                        region: key.region,
                    };
                    to_build.push(LoopVolumeRequest {
                        pane_idx,
                        frame_idx: idx,
                        target: volume_target,
                        retired: frame.render_failed,
                    });
                }
                continue;
            }

            if ls.view == rustdar_radar::types::RenderView::CrossSection {
                let Some(key) = ls.section_key().cloned() else {
                    for &idx in &indices {
                        to_mark_failed.push((pane_idx, idx));
                    }
                    continue;
                };
                for &idx in &indices {
                    let frame = &ls.frames[idx];
                    if frame.render_in_flight || frame.render_failed {
                        continue;
                    }
                    // The ladder this frame's own scan resolves *now*. Both the
                    // staleness test and the cut are keyed on it, so they cannot
                    // disagree about which ladder the picture is of.
                    let ladder = match frame_section(&self.loop_mgr, &target, frame.timestamp) {
                        FrameSection::At(ladder) => ladder,
                        FrameSection::Unrenderable => {
                            to_mark_failed.push((pane_idx, idx));
                            continue;
                        }
                        FrameSection::Pending => continue,
                    };
                    if frame
                        .image
                        .as_ref()
                        .and_then(rustdar_egui::pane::LoopFrameImage::section)
                        .is_some_and(|cut| cut.ladder == ladder)
                    {
                        continue;
                    }

                    if linked
                        && let Some((src_pane, src_frame)) = find_section_donor(
                            (0..pane_count)
                                .filter(|&i| self.gui.pane_layer_linked(i))
                                .filter_map(|i| self.gui.pane(i).map(|p| (i, p.loop_state()))),
                            pane_idx,
                            frame.timestamp,
                            &target,
                            &key,
                            ladder,
                        )
                    {
                        to_clone.push(LoopCloneRequest {
                            dest_pane: pane_idx,
                            dest_frame: idx,
                            src_pane,
                            src_frame,
                        });
                        continue;
                    }

                    if to_cut.len() >= MAX_LOOP_SECTION_CUTS_PER_FRAME {
                        // Out of frame-thread budget for this pass. Left alone,
                        // not retired: the next pass asks again, and the pane
                        // goes on showing whatever has already landed.
                        break;
                    }
                    // The queuing pane must be linked too, or the section
                    // broadcast this lean relies on never runs — the same
                    // linked-queuer filter as `render_already_queued`'s.
                    if linked
                        && section_already_queued(
                            to_cut
                                .iter()
                                .filter(|r| self.gui.pane_layer_linked(r.pane_idx)),
                            frame.timestamp,
                            &target,
                            &key,
                        )
                    {
                        continue;
                    }
                    to_cut.push(LoopSectionRequest {
                        pane_idx,
                        frame_idx: idx,
                        timestamp: frame.timestamp,
                        target: target.clone(),
                        key: key.clone(),
                        ladder,
                        site_lat,
                        site_lon,
                    });
                }
                continue;
            }

            for &idx in &indices {
                let frame = &ls.frames[idx];
                if frame.image.is_some() || frame.render_in_flight || frame.render_failed {
                    continue;
                }

                if linked {
                    let donor = find_donor(
                        (0..pane_count)
                            .filter(|&i| self.gui.pane_layer_linked(i))
                            .filter_map(|i| self.gui.pane(i).map(|p| (i, p.loop_state()))),
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
                        if linked
                            && render_already_queued(
                                to_render
                                    .iter()
                                    .filter(|r| self.gui.pane_layer_linked(r.pane_idx)),
                                frame.timestamp,
                                &target,
                                snapped,
                            )
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
                && let Some(frame) = pane.loop_state_mut().frames.get_mut(frame_idx)
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
                let Some(sframe) = src.loop_state().frames.get(req.src_frame) else {
                    continue;
                };
                let Some(image) = sframe.image.clone() else {
                    continue;
                };
                image
            };
            let Some(dest) = self.gui.pane_mut(req.dest_pane) else {
                continue;
            };
            if let Some(dframe) = dest.loop_state_mut().frames.get_mut(req.dest_frame) {
                dframe.image = Some(cloned);
            }
        }

        // Now spawn renders and mark the frames in flight, respecting concurrent limit
        for req in to_render {
            // Check concurrent render limit before each spawn (shared with static pane renders)
            let current = self.render.renders_in_flight.load(Ordering::Relaxed);
            if current >= self.render.concurrent_renders() {
                break;
            }

            // Asked here rather than inside the spawn because "the data has not
            // arrived" is not a failed render: this frame is skipped and asked
            // again next pass, and nothing about it is marked.
            let Some(req_product) = crate::render_key::radar_field(&req.target.product) else {
                continue;
            };
            if !self
                .loop_mgr
                .frame_data_arrived(&req.target.site, req_product, &req.timestamp)
            {
                continue;
            }

            let spawned = self.spawn_loop_frame_render(
                req.pane_idx,
                req.timestamp,
                req.render_params(),
                req.target,
            );

            if spawned && let Some(pane) = self.gui.pane_mut(req.pane_idx) {
                pane.loop_state_mut().frames[req.frame_idx].render_in_flight = true;
            }
        }

        for req in to_cut {
            if self.render.renders_in_flight.load(Ordering::Relaxed)
                >= self.render.concurrent_renders()
            {
                break;
            }
            let Some((scan, declared)) = crate::render_key::radar_field(&req.target.product)
                .and_then(|p| {
                    self.loop_mgr
                        .frame_volume(&req.target.site, p, &req.timestamp)
                })
            else {
                if let Some(pane) = self.gui.pane_mut(req.pane_idx)
                    && let Some(frame) = pane.loop_state_mut().frames.get_mut(req.frame_idx)
                {
                    frame.render_failed = true;
                }
                continue;
            };
            let (pane_idx, frame_idx) = (req.pane_idx, req.frame_idx);
            match self.spawn_loop_section_render(req, scan, declared) {
                crate::render_dispatch::SectionDispatch::Dispatched => {
                    if let Some(pane) = self.gui.pane_mut(pane_idx)
                        && let Some(frame) = pane.loop_state_mut().frames.get_mut(frame_idx)
                    {
                        frame.render_in_flight = true;
                    }
                }
                // Nothing was taken and nothing is wrong: ask again next frame.
                crate::render_dispatch::SectionDispatch::Busy => {}
                crate::render_dispatch::SectionDispatch::NoPayload => {
                    if let Some(pane) = self.gui.pane_mut(pane_idx)
                        && let Some(frame) = pane.loop_state_mut().frames.get_mut(frame_idx)
                    {
                        frame.render_failed = true;
                    }
                }
            }
        }

        self.make_volume_frames_resident(to_build);
    }

    /// Make each planned 3D loop frame's grid resident, and name it on the
    /// frame once it is.
    fn make_volume_frames_resident(&mut self, to_build: Vec<LoopVolumeRequest>) {
        use rustdar_volumetric::bridge::{Hold, VolumeEntry};

        let mut dispatched = 0usize;
        // Every target still wanted, per pane, gathered as the pass goes so
        // the statement below is exactly what this pass decided rather than a
        // second walk free to disagree with it.
        let mut held: std::collections::BTreeMap<usize, Vec<rustdar_egui::pane::VolumeTarget>> =
            std::collections::BTreeMap::new();

        for req in to_build {
            held.entry(req.pane_idx)
                .or_default()
                .push(req.target.clone());
            // Cheap: already built, building, or refused. Costs a lookup and
            // an attach, and is deliberately outside the pacing budget.
            let known = self
                .volume_store
                .share_held(req.pane_idx, &req.target, Hold::Set);
            if !known {
                if req.retired {
                    continue;
                }
                if dispatched >= MAX_LOOP_VOLUME_BUILDS_PER_FRAME {
                    // Out of frame-thread budget for this pass. Left alone,
                    // not retired: the next pass asks again, and the pane goes
                    // on marching whatever has already landed.
                    continue;
                }
                // **A remainder, named rather than papered over.** This whole
                // pass is radar's loop — the view is a `rustdar_radar` enum,
                // the anchor is a radar geometry and the frames are radar
                // scans — so the layer to ask is not in doubt and is not
                // derived from anything generic. The pane's own 3D walk is
                // what will name it when the loop path itself goes
                // source-agnostic; that is not WO-M14b-2's.
                match self.prepare_volume(
                    req.pane_idx,
                    &req.target,
                    Hold::Set,
                    &rustdar_source::id::known::RADAR,
                ) {
                    // A build was started, or a refusal was decided. Either
                    // way the store now answers for this target.
                    crate::app::VolumePrepare::Served => dispatched += 1,
                    // The scan has not downloaded yet, or the render budget is
                    // full. Nothing was spent; the next pass asks again.
                    crate::app::VolumePrepare::Waiting | crate::app::VolumePrepare::Busy => {
                        continue;
                    }
                }
            }
            let Some(found) = self.volume_store.lookup(&req.target) else {
                continue;
            };
            let Some(pane) = self.gui.pane_mut(req.pane_idx) else {
                continue;
            };
            let Some(frame) = pane.loop_state_mut().frames.get_mut(req.frame_idx) else {
                continue;
            };
            match found.entry {
                // Resident. The frame names it, which is what makes the
                // playhead able to march it.
                VolumeEntry::Ready(_) => {
                    frame.render_in_flight = false;
                    frame.image = Some(rustdar_egui::pane::LoopFrameImage::Volume(
                        rustdar_egui::pane::VolumeFrameGrid {
                            id: found.id,
                            target: req.target.clone(),
                        },
                    ));
                }
                VolumeEntry::Building => frame.render_in_flight = true,
                VolumeEntry::Refused(_) => {
                    frame.render_in_flight = false;
                    frame.render_failed = true;
                }
            }
        }

        for (pane_idx, targets) in held {
            self.volume_store.retain_set(pane_idx, &targets);
        }
    }

    /// Poll for finished cross-section loop cuts and upload their rasters.
    fn poll_loop_section_results(&mut self, ctx: &egui::Context) {
        while let Ok(mut sr) = self.channels.loop_section_receiver.try_recv() {
            let origin_pane = sr.pane_idx;
            let Some(pane) = self.gui.pane_mut(origin_pane) else {
                continue;
            };

            let counter = &mut self.texture_counter;
            let Some(placed) =
                accept_section_result(pane.loop_state_mut(), &mut sr, |color_image| {
                    *counter += 1;
                    ctx.load_texture(
                        format!("loop_section_{counter}"),
                        color_image,
                        egui::TextureOptions::NEAREST,
                    )
                })
            else {
                continue;
            };

            if !self.gui.pane_layer_linked(origin_pane) {
                continue;
            }
            for sibling_idx in 0..self.gui.pane_count() {
                if sibling_idx == origin_pane
                    || self.gui.pane_cannot_loop(sibling_idx)
                    || !self.gui.pane_layer_linked(sibling_idx)
                {
                    continue;
                }
                let own_ladder = match frame_section(&self.loop_mgr, &sr.target, sr.timestamp) {
                    FrameSection::At(ladder) => Some(ladder),
                    FrameSection::Unrenderable | FrameSection::Pending => None,
                };
                let Some(sibling) = self.gui.pane_mut(sibling_idx) else {
                    continue;
                };
                let Some(sframe) = sibling
                    .loop_state_mut()
                    .frame_accepting_section_broadcast_mut(
                        sr.timestamp,
                        &sr.target,
                        &sr.key,
                        sr.ladder,
                        own_ladder,
                    )
                else {
                    continue;
                };
                // Its own cut, if any, is now redundant: same key, same ladder,
                // same volume means the same raster, so its reply is dropped on
                // arrival by the target check.
                sframe.render_in_flight = false;
                sframe.image = Some(rustdar_egui::pane::LoopFrameImage::Section(placed.clone()));
            }
        }
    }
}

/// Why no section can be cut from what the app holds for a site, or `None`
/// when one can.
fn section_source_refusal(
    base: Option<&nexrad_model::data::Scan>,
    overlay: Option<&nexrad_model::data::Scan>,
) -> Option<rustdar_egui::pane::SectionUnavailable> {
    if let Some(current) =
        rustdar_radar::current::resolve(base.map(Into::into), overlay.map(Into::into))
    {
        return current
            .sweeps()
            .is_empty()
            .then_some(rustdar_egui::pane::SectionUnavailable::AwaitingFirstSweep);
    }
    if overlay.is_some_and(|scan| !scan.sweeps().is_empty()) {
        return Some(rustdar_egui::pane::SectionUnavailable::AwaitingCoveragePattern);
    }
    Some(rustdar_egui::pane::SectionUnavailable::AwaitingVolume)
}

/// Take a scan listing for `site` into `ls`'s frame list, returning the downloads
/// it now owes.
fn accept_scan_listing(
    allocation: LoopAllocation,
    budgets: &rustdar_device_profile::budget::Budgets,
    ls: &mut rustdar_egui::pane::LayerTimeState,
    site: &str,
    scans: Vec<chrono::NaiveDateTime>,
    animating: usize,
) -> Option<FramePlan> {
    if !ls.is_active() || radar_layer::site(ls) != site {
        return None;
    }

    if scans.is_empty() {
        log::warn!("Loop: no {site} scans in the requested window; leaving loop mode");
        *ls = rustdar_egui::pane::LayerTimeState::new();
        return None;
    }

    // The site's own cadence, read off the listing *before* the sampling below
    // throws scans away. Once sampled there is no way back to it, and it is what
    // the timeline caption needs to tell "every scan" from "one in five".
    ls.cadence_secs = median_step_secs(&scans);

    // Cap the downloads by evenly sampling the listing. A 3D loop's cap is its
    // *resident* one and is far lower, because for that kind the frame list and
    // the resident set are one thing — see `loop_frames_held`.
    let held = layer_share(loop_frames_held(allocation, ls, budgets), animating);
    let total = scans.len();
    let sample = rustdar_egui::pane::listing_sample_indices(total, held);
    ls.sampled = Some(sample.is_some());
    let scans = match sample {
        Some(indices) => {
            log::info!("Loop: sampled {total} down to {held} frames for {site}");
            indices.into_iter().map(|i| scans[i]).collect()
        }
        None => scans,
    };

    ls.phase = rustdar_egui::pane::LoopPhase::Rendering;
    // Oldest-first, matching the scan listing order.
    ls.frames = scans
        .iter()
        .map(|ts| rustdar_egui::pane::LoopFrame {
            timestamp: *ts,
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    // A freshly built loop is parked on its newest frame; the pane's own
    // clock takes over at the next settle.
    ls.settle_playhead(rustdar_egui::pane::TimeMode::Live);

    Some(FramePlan::new(site.to_string(), scans))
}

/// The median gap between consecutive scan times, in whole seconds.
pub(super) fn median_step_secs(times: &[chrono::NaiveDateTime]) -> Option<u32> {
    let mut gaps: Vec<i64> = times
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).num_seconds())
        .filter(|secs| *secs > 0)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_unstable();
    u32::try_from(gaps[gaps.len() / 2]).ok()
}

/// Move a loop that is still `Rendering` on to whatever its frames have settled
/// into, returning `true` if the loop was switched off.
fn settle_loop_phase(
    loop_mgr: &rustdar_radar::loop_downloads::LoopDownloadManager,
    pane_idx: usize,
    ls: &mut rustdar_egui::pane::LayerTimeState,
    budget: usize,
) -> bool {
    if !ls.is_active() || ls.is_render_ready() || ls.frames.is_empty() {
        return false;
    }
    // `is_pane_done` means "dispatched", not "arrived" — see below.
    if !loop_batch_settled(loop_mgr, ls, budget) || !loop_mgr.is_pane_done(pane_idx) {
        return false;
    }
    if ls.frames.iter().any(|f| f.image.is_some()) {
        ls.phase = rustdar_egui::pane::LoopPhase::Ready;
        return false;
    }
    if let Some(product) = loop_product(ls)
        && ls
            .frames
            .iter()
            .any(|f| loop_mgr.frame_data_in_flight(radar_layer::site(ls), product, &f.timestamp))
    {
        return false;
    }
    log::warn!("Loop: no frame on pane {pane_idx} could be rendered; leaving loop mode");
    *ls = rustdar_egui::pane::LayerTimeState::new();
    true
}

/// The frame image a finished loop render describes.
fn rendered_image(
    rr: &crate::channels::LoopRenderResponse,
    texture: &egui::TextureHandle,
    gates: Option<rustdar_radar::hover::SweepGates>,
) -> rustdar_egui::pane::RadarImageData {
    rustdar_egui::pane::RadarImageData {
        texture: texture.clone(),
        lat: rr.site_lat,
        lon: rr.site_lon,
        max_range_km: rr.max_range_km,
        placed: rustdar_radar::types::ImageBounds::from_radar_site(
            rr.site_lat,
            rr.site_lon,
            rr.max_range_km,
        )
        .into(),
        nyquist_ms: rr.nyquist_ms,
        melting_layer_source: rr.melting_layer_source,
        storm_motion: rr.storm_motion,
        hover: Arc::new(rustdar_radar::hover::HoverSource::from_volume(
            rr.polar.clone(),
            gates,
        )),
    }
}

/// The sweep a finished loop render was drawn from, for reading its numbers
/// back out.
fn frame_gates(
    loop_mgr: &rustdar_radar::loop_downloads::LoopDownloadManager,
    rr: &crate::channels::LoopRenderResponse,
) -> Option<rustdar_radar::hover::SweepGates> {
    let (scan, _) = loop_mgr.get_cached(&rr.target.site, &rr.timestamp)?;
    let product = crate::render_key::radar_field(&rr.target.product)?;
    rustdar_radar::hover::SweepGates::new(Arc::clone(scan), product, rr.snapped)
}

/// Place a finished loop render on the frame of `ls` that asked for it, returning
/// the texture that was uploaded so the caller can offer it to sibling panes.
fn accept_render_result(
    ls: &mut rustdar_egui::pane::LayerTimeState,
    rr: &mut crate::channels::LoopRenderResponse,
    gates: Option<rustdar_radar::hover::SweepGates>,
    upload: impl FnOnce(egui::ColorImage) -> egui::TextureHandle,
) -> Option<egui::TextureHandle> {
    let frame = ls.frame_awaiting_render_result_mut(rr.timestamp, &rr.target)?;
    frame.render_in_flight = false;

    let Some(color_image) = rr.image.take() else {
        frame.render_failed = true;
        return None;
    };

    let texture = upload(color_image);
    frame.image = Some(rustdar_egui::pane::LoopFrameImage::PlanView(
        rendered_image(rr, &texture, gates),
    ));
    Some(texture)
}

/// [`accept_render_result`] for a finished cross-section cut.
fn accept_section_result(
    ls: &mut rustdar_egui::pane::LayerTimeState,
    sr: &mut crate::channels::LoopSectionResponse,
    upload: impl FnOnce(egui::ColorImage) -> egui::TextureHandle,
) -> Option<rustdar_egui::pane::SectionImageData> {
    let frame = ls.frame_awaiting_section_result_mut(sr.timestamp, &sr.target, &sr.key)?;
    frame.render_in_flight = false;

    // The axes travel with the raster and are `None` exactly when it is, so a
    // reply carrying one without the other is a bug upstream rather than a
    // frame to draw with the previous frame's scales.
    let (Some(color_image), Some(axes)) = (sr.image.take(), sr.axes) else {
        frame.render_failed = true;
        return None;
    };

    let image = rustdar_egui::pane::SectionImageData {
        texture: upload(color_image),
        axes,
        tilt_elevations_deg: std::mem::take(&mut sr.tilt_elevations_deg),
        tilt_collected_ms: std::mem::take(&mut sr.tilt_collected_ms),
        ladder: sr.ladder,
    };
    frame.image = Some(rustdar_egui::pane::LoopFrameImage::Section(image.clone()));
    Some(image)
}

/// Record a finished download: clear its in-flight mark and cache the scan.
fn apply_completed_download(
    loop_mgr: &mut rustdar_radar::loop_downloads::LoopDownloadManager,
    resp: crate::channels::LoopScanDownloadResponse,
) {
    loop_mgr.complete_download(&resp.site, &resp.timestamp);
    // Skip failures — the mark is cleared either way so the frame can be retried.
    if let Some(volume) = resp.scan {
        loop_mgr.cache_scan(&resp.site, resp.timestamp, volume);
    }
}

/// Every UTC day the pairing windows of `queue`'s volumes touch, deduplicated.
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
/// Test-only since WO-M12d: the dispatch path asks radar for the *described job*
/// a frame's data makes and never holds the arms themselves. What the suites
/// below still pin through here is the keying — that a frame's data is looked up
/// under its own target's site and its own target's product.
#[cfg(test)]
fn frame_data(
    loop_mgr: &rustdar_radar::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> Option<rustdar_radar::loop_downloads::LoopFrameData> {
    crate::render_key::radar_field(&target.product)
        .and_then(|p| loop_mgr.frame_data(&target.site, p, &timestamp))
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
fn frame_sweep(
    loop_mgr: &rustdar_radar::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> FrameSweep {
    let Some(product) = crate::render_key::radar_field(&target.product) else {
        return FrameSweep::Unrenderable;
    };
    if product.is_level3() {
        return match loop_mgr.l3_frame_state(&target.site, product, &timestamp) {
            L3FrameState::Pending => FrameSweep::Pending,
            L3FrameState::Absent => FrameSweep::Unrenderable,
            L3FrameState::Ready => {
                match loop_mgr
                    .l3_frame_products(&target.site, product, &timestamp)
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
    let Some((scan, _)) = loop_mgr.get_cached(&target.site, &timestamp) else {
        return FrameSweep::Pending;
    };
    match rustdar_radar::render::find_closest_elevation(scan, product, target.elevation) {
        Some(snapped) => FrameSweep::At(snapped),
        None => FrameSweep::Unrenderable,
    }
}

/// The sweep `ls`'s own data for `timestamp` resolves `product`/`elevation` to, or
/// `None` if it has none or that data carries nothing for the product.
fn own_sweep(
    loop_mgr: &rustdar_radar::loop_downloads::LoopDownloadManager,
    ls: &rustdar_egui::pane::LayerTimeState,
    timestamp: chrono::NaiveDateTime,
    product: rustdar_source::product::FieldId,
    elevation: f32,
) -> Option<f32> {
    // Resolved through the same function the dispatcher plans with, against the
    // receiver's own site: a second rule for "which sweep does this frame show"
    match frame_sweep(
        loop_mgr,
        &RenderTarget::new(radar_layer::site(ls).to_string(), &product, elevation),
        timestamp,
    ) {
        FrameSweep::At(sweep) => Some(sweep),
        FrameSweep::Unrenderable | FrameSweep::Pending => None,
    }
}

/// The sweep pair for offering `rr`'s finished image to the loop `ls`.
fn broadcast_sweep(
    loop_mgr: &rustdar_radar::loop_downloads::LoopDownloadManager,
    ls: &rustdar_egui::pane::LayerTimeState,
    rr: &crate::channels::LoopRenderResponse,
) -> BroadcastSweep {
    BroadcastSweep {
        rendered: rr.snapped,
        own: own_sweep(
            loop_mgr,
            ls,
            rr.timestamp,
            rr.target.product.clone(),
            rr.target.elevation,
        ),
    }
}

/// The product a loop's frames are keyed to, or `None` before the first dispatch.
fn loop_product(
    ls: &rustdar_egui::pane::LayerTimeState,
) -> Option<rustdar_radar::types::RadarProduct> {
    ls.rendered_for
        .as_ref()
        .and_then(|t| crate::render_key::radar_field(&t.product))
}

/// Whether every frame `ls` intends to render has settled, given what has arrived.
fn loop_batch_settled(
    loop_mgr: &rustdar_radar::loop_downloads::LoopDownloadManager,
    ls: &rustdar_egui::pane::LayerTimeState,
    budget: usize,
) -> bool {
    let Some(product) = loop_product(ls) else {
        // Nothing dispatched yet, so nothing has settled.
        return false;
    };
    // Not merely "nothing in flight this instant": the render budget is shared with
    // static pane renders, so part of a batch can be starved and not yet spawned.
    ls.render_set_settled(budget, |f| {
        loop_mgr.frame_data_settled(radar_layer::site(ls), product, &f.timestamp)
    })
}

/// What one frame's own volume makes of a section loop's line.
enum FrameSection {
    /// The ladder fingerprint this frame would be cut from.
    At(u64),
    /// The volume is here and carries nothing to cut under this product.
    Unrenderable,
    /// The volume has not arrived yet.
    Pending,
}

/// The ladder frame `timestamp` of a section loop keyed to `target` would be cut
/// from.
fn frame_section(
    loop_mgr: &rustdar_radar::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> FrameSection {
    let Some((scan, _)) = loop_mgr.get_cached(&target.site, &timestamp) else {
        return FrameSection::Pending;
    };
    let sweeps: Vec<&nexrad_model::data::Sweep> = scan.sweeps().iter().collect();
    let Some(product) = crate::render_key::radar_field(&target.product) else {
        return FrameSection::Unrenderable;
    };
    match rustdar_radar::sampler::ladder_fingerprint(scan.coverage_pattern(), &sweeps, product) {
        Some(ladder) => FrameSection::At(ladder),
        None => FrameSection::Unrenderable,
    }
}

/// The allocation an idle application has: the whole pool at this target's
/// floor, undivided.
#[cfg(test)]
pub(crate) fn test_loop_allocation() -> LoopAllocation {
    let budgets = test_budgets();
    let limits = crate::loop_pool::LoopPoolLimits::from_budgets(&budgets);
    crate::loop_pool::LoopPool::new(limits.floor, limits).plan(
        LoopFrameModel::from_budgets(&budgets),
        LoopDemand::default(),
    )
}

/// This build's own budgets, for the tests that take them as an argument.
#[cfg(test)]
pub(crate) fn test_budgets() -> rustdar_device_profile::budget::Budgets {
    rustdar_device_profile::budget::resolve(
        &rustdar_device_profile::budget::DeviceProfile::for_target(),
    )
}

/// Frames this loop may keep **textured**, which is the term that bounds memory.
fn loop_render_budget(
    allocation: LoopAllocation,
    ls: &rustdar_egui::pane::LayerTimeState,
    budgets: &rustdar_device_profile::budget::Budgets,
) -> usize {
    allocation
        .frames_for(ls.view)
        .min(budgets.frames_for_span(ls.cadence_secs))
}

/// Frames a loop of this view **holds**, before the pane's own layers divide
/// it — see [`layer_share`].
pub(super) fn loop_frames_held(
    allocation: LoopAllocation,
    ls: &rustdar_egui::pane::LayerTimeState,
    budgets: &rustdar_device_profile::budget::Budgets,
) -> usize {
    match ls.view {
        rustdar_radar::types::RenderView::Volume => loop_render_budget(allocation, ls, budgets),
        rustdar_radar::types::RenderView::PlanView
        | rustdar_radar::types::RenderView::CrossSection => budgets.loop_frames_held,
    }
}

/// **One animating layer's share of the pane's frame budget.**
///
/// The cap is a whole-pane number — it is a texture-memory allowance, and a
/// pane animating radar and a model field at once spends it twice. So it
/// divides, with a **floor of two frames per layer**: one frame is a still
/// picture, and a layer that cannot hold two cannot animate at all, so the
/// floor is where the budget stops being divisible rather than a cushion.
///
/// **A pane animating one layer gets the budget untouched.** Not
/// `budget / 1` clamped — the one-layer case returns before the floor is
/// applied, so a view whose own allowance is legitimately below two (a 3D
/// loop on a small pool) is not silently raised to two by a division that did
/// not happen. That is what makes this a no-op on every pane in the build
/// today, and it is pinned as one.
pub(super) fn layer_share(budget: usize, animating: usize) -> usize {
    if animating <= 1 {
        return budget;
    }
    (budget / animating).max(2)
}

/// [`accept_scan_listing`] under a name the sibling test modules can reach —
/// the function itself is private to this module and stays that way.
#[cfg(test)]
pub(crate) fn accept_scan_listing_for_test(
    allocation: LoopAllocation,
    budgets: &rustdar_device_profile::budget::Budgets,
    ls: &mut rustdar_egui::pane::LayerTimeState,
    site: &str,
    scans: Vec<chrono::NaiveDateTime>,
    animating: usize,
) -> Option<FramePlan> {
    accept_scan_listing(allocation, budgets, ls, site, scans, animating)
}

/// A 3D loop frame the dispatcher intends to make resident.
pub(crate) struct LoopVolumeRequest {
    pub pane_idx: usize,
    pub frame_idx: usize,
    pub target: rustdar_egui::pane::VolumeTarget,
    /// This frame has already been ruled out. It is planned anyway so the
    /// resident set the dispatcher states names the whole frame list, and it
    /// is never dispatched for.
    pub retired: bool,
}

/// A cross-section loop frame the dispatcher intends to cut.
pub(crate) struct LoopSectionRequest {
    pub(crate) pane_idx: usize,
    pub(crate) frame_idx: usize,
    pub(crate) timestamp: chrono::NaiveDateTime,
    /// The site/product half of the key this cut is for.
    pub(crate) target: RenderTarget,
    /// The line/storm-motion half.
    pub(crate) key: rustdar_egui::pane::SectionLoopKey,
    /// The ladder this frame's own volume resolves, resolved once during
    /// planning and carried through so the staleness test, the donor search and
    /// the dispatch stamp all read the one value.
    pub(crate) ladder: u64,
    pub(crate) site_lat: f64,
    pub(crate) site_lon: f64,
}

/// A section frame another pane's loop can donate to `receiver`, as
/// `(pane, frame)`.
fn find_section_donor<'a>(
    loops: impl IntoIterator<Item = (usize, &'a rustdar_egui::pane::LayerTimeState)>,
    receiver: usize,
    timestamp: chrono::NaiveDateTime,
    target: &RenderTarget,
    key: &rustdar_egui::pane::SectionLoopKey,
    wanted_ladder: u64,
) -> Option<(usize, usize)> {
    loops
        .into_iter()
        .filter(|&(idx, _)| idx != receiver)
        .find_map(|(idx, ls)| {
            Some((
                idx,
                ls.section_frame_donatable_to(timestamp, target, key, wanted_ladder)?,
            ))
        })
}

/// Whether a cut for this frame and key is already queued in this dispatch pass.
fn section_already_queued<'a>(
    mut queued: impl Iterator<Item = &'a LoopSectionRequest>,
    timestamp: chrono::NaiveDateTime,
    target: &RenderTarget,
    key: &rustdar_egui::pane::SectionLoopKey,
) -> bool {
    // A cut's picture is a function of the line, the volume and the storm
    // motion; the tilt is not an input to it, and `CrossSection` is what says so.
    queued.any(|r| {
        r.timestamp == timestamp
            && r.target
                .matches(target, rustdar_radar::types::RenderView::CrossSection)
            && &r.key == key
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
    fn render_params(&self) -> crate::render_dispatch::RenderParams {
        crate::render_dispatch::RenderParams {
            product: crate::render_key::radar_field(&self.target.product)
                .expect("a loop render request names a field the radar layer registers"),
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
fn find_donor<'a>(
    loops: impl IntoIterator<Item = (usize, &'a rustdar_egui::pane::LayerTimeState)>,
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
fn render_already_queued<'a>(
    mut queued: impl Iterator<Item = &'a LoopRenderRequest>,
    timestamp: chrono::NaiveDateTime,
    target: &RenderTarget,
    snapped: f32,
) -> bool {
    // A `LoopRenderRequest` carries the site coordinates it builds `RenderParams`
    // from, so it is a plan view by construction. The snapped term stays: it is
    // sweep *agreement*, not identity.
    queued.any(|r| {
        r.timestamp == timestamp
            && r.target
                .matches(target, rustdar_radar::types::RenderView::PlanView)
            && (r.snapped - snapped).abs() <= ELEVATION_TOLERANCE
    })
}

/// The order one frame is assembled in.
#[path = "app_render/declared_nyquist_dispatch_tests.rs"]
#[cfg(test)]
mod declared_nyquist_dispatch_tests;

#[path = "app_render/frame_build_order_tests.rs"]
#[cfg(test)]
mod frame_build_order_tests;

/// Where the per-pixel unmultiply is allowed to run, and where it is not.
#[path = "app_render/frame_thread_conversion_tests.rs"]
#[cfg(test)]
mod frame_thread_conversion_tests;

/// What the overlay poller puts on the GPU, read back from egui's own texture
/// delta rather than inferred.
#[path = "app_render/overlay_upload_tests.rs"]
#[cfg(test)]
mod overlay_upload_tests;

/// One sweep is one texture, however many panes are showing it — counted the
/// same way, off the delta, because the cost being removed is the upload and
/// not the picture.
#[path = "app_render/radar_texture_sharing_tests.rs"]
#[cfg(test)]
mod radar_texture_sharing_tests;

#[path = "app_render/frame_order_tests.rs"]
#[cfg(test)]
mod frame_order_tests;

/// The renderer pins that stayed behind — each scrapes a file this
/// crate owns (`present_frame`, the one `EguiRenderer::new` call, the wake).
#[path = "app_render/egui_frame_pin_tests.rs"]
#[cfg(test)]
mod egui_frame_pin_tests;

/// What `poll_level3_results` does with a channel holding more than one answer.
#[path = "app_render/level3_poll_tests.rs"]
#[cfg(test)]
mod level3_poll_tests;

/// The launch that has never seen a radar: what a first catalogue does, and
/// what every later one must not.
#[path = "app_render/first_launch_tests.rs"]
#[cfg(test)]
mod first_launch_tests;

#[path = "app_render/loop_dispatch_tests.rs"]
#[cfg(test)]
mod loop_dispatch_tests;

/// The cross-section loop's dispatch, placement and frame-thread pacing.
#[path = "app_render/loop_section_tests.rs"]
#[cfg(test)]
mod loop_section_tests;

/// The 3D loop's dispatch: what becomes resident, what the resident set is
/// bounded by, and what a region change releases before it rebuilds.
#[path = "app_render/loop_volume_tests.rs"]
#[cfg(test)]
mod loop_volume_tests;

/// What a 3D pane the layout stopped showing gives back, and what the release
/// beside it must not touch.
#[path = "app_render/hidden_pane_volume_tests.rs"]
#[cfg(test)]
mod hidden_pane_volume_tests;

/// What the loop timer does with a playback speed no slider could have set.
#[path = "app_render/loop_interval_tests.rs"]
#[cfg(test)]
mod loop_interval_tests;

#[path = "app_render/layer_share_tests.rs"]
#[cfg(test)]
mod layer_share_tests;

/// The Level III half of the loop: pairing a bucket object to each frame's volume,
/// what a gap does, and what happens when a pane retargets across the datasource
/// line mid-loop.
#[path = "app_render/loop_level3_tests.rs"]
#[cfg(test)]
mod loop_level3_tests;

/// What bounds the loop's two data caches: the decoded volumes — the fourth
/// holder of whole `Arc<Scan>`s — and the paired Level III objects beside them.
#[path = "app_render/loop_scan_cache_tests.rs"]
#[cfg(test)]
mod loop_scan_cache_tests;

/// The plan-view render pipeline against a pane that has no plan view.
#[path = "app_render/pane_kind_render_filter_tests.rs"]
#[cfg(test)]
mod pane_kind_render_filter_tests;

/// A restored image describes itself too.
#[path = "app_render/restore_describes_its_image_tests.rs"]
#[cfg(test)]
mod restore_describes_its_image_tests;

/// What a section pane is told when it cannot be cut, and when the picture on
/// screen has stopped being the truth.
#[path = "app_render/section_dispatch_tests.rs"]
#[cfg(test)]
mod section_dispatch_tests;

/// What `poll_level3_results` does with sounding responses: the same drain and
/// fetch-generation gate as everything else on it, plus the keep-on-failure
/// rule that makes the TTL retry loop safe.
#[path = "app_render/sounding_poll_tests.rs"]
#[cfg(test)]
mod sounding_poll_tests;

/// A pane keeps the picture it has until the next one is whole.
#[path = "app_render/raster_hold_tests.rs"]
#[cfg(test)]
mod raster_hold_tests;

/// What `apply_render_to_pane` does with a finished image beyond placing it.
#[path = "app_render/stamping_tests.rs"]
#[cfg(test)]
mod stamping_tests;

/// One sweep is one *render*, however many panes are looking at it — the
/// sibling of `radar_texture_sharing_tests`, one step earlier in the same path.
#[path = "app_render/one_render_per_sweep_tests.rs"]
#[cfg(test)]
mod one_render_per_sweep_tests;

/// The arrival-time extraction cache: a volume's arrival performs
/// the plan-view `RenderInput::extract` walks off-thread for the panes
/// showing the site, and the dispatch serves them from the cache — zero
/// frame-thread extraction on a hit, today's inline walk on a miss.
#[path = "app_render/extract_cache_tests.rs"]
#[cfg(test)]
mod extract_cache_tests;

/// The adjacent-tilt pre-render: one speculative render after a
/// delivered plan view, into the existing RenderCache, gated off wasm and
/// small budgets — never marking a pane, never more than one at a time.
#[path = "app_render/speculative_render_tests.rs"]
#[cfg(test)]
mod speculative_render_tests;

/// The frames between a dispatched overlay render and the layer being switched
/// off — where a result that cannot be recalled lands on a pane that no longer
/// wants it.
#[path = "app_render/overlay_disable_race_tests.rs"]
#[cfg(test)]
mod overlay_disable_race_tests;

/// The overlay half of the hold: a pane keeps the layer picture it has —
/// alerts, outlooks — until the next one's pixels have all landed, the swap
/// leaves the radar caption alone, and a renderer rebuild releases what it can
/// never deliver.
#[path = "app_render/overlay_hold_tests.rs"]
#[cfg(test)]
mod overlay_hold_tests;

// ---- FRAME_PUMP wrappers ----

pub(super) fn pump_poll_render_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_render_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_poll_section_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_section_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_poll_level3_results(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.poll_level3_results();
}

pub(super) fn pump_poll_site_catalogue(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.poll_site_catalogue();
}

pub(super) fn pump_poll_overlay_render_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_overlay_render_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_accept_loop_scan_listings(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.accept_loop_scan_listings();
}

pub(super) fn pump_poll_loop_scan_download_results(
    app: &mut super::App,
    _ctx: Option<&egui::Context>,
) {
    app.poll_loop_scan_download_results();
}

pub(super) fn pump_poll_loop_l3_list_results(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.poll_loop_l3_list_results();
}

pub(super) fn pump_poll_loop_l3_fetch_results(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.poll_loop_l3_fetch_results();
}

pub(super) fn pump_poll_loop_render_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_loop_render_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_poll_loop_section_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_loop_section_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_advance_loop_playback(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.advance_loop_playback();
}

pub(super) fn pump_poll_extract_results(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.render.poll_extract_results();
}

pub(super) fn pump_dispatch_pane_renders(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.dispatch_pane_renders(ctx.expect("Dispatch rows run from setup_egui_frame"));
}

pub(super) fn pump_dispatch_section_renders(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.dispatch_section_renders();
}

pub(super) fn pump_dispatch_loop_renders(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.dispatch_loop_renders();
}
