use crate::constants::{
    DEFAULT_LOOP_SPEED_FPS, MAX_LOOP_SECTION_CUTS_PER_FRAME, MAX_LOOP_SPEED_FPS,
    MAX_LOOP_VOLUME_BUILDS_PER_FRAME, MIN_LOOP_SPEED_FPS,
};
use crate::loop_downloads::{
    FramePlan, L3FrameState, LoopFrameData, PendingDownloads, PendingL3Pairings,
};
use crate::loop_pool::{LoopAllocation, LoopDemand, LoopFrameModel};
use crate::render_dispatch::CachedPaneRender;
use egui_wgpu::wgpu;
use rustdar_egui::actions::GuiAction;
use rustdar_egui::pane::{BroadcastSweep, ELEVATION_TOLERANCE, RenderTarget};
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

/// The plan-view rasters one pass has already put on the GPU, so the second
/// pane showing one of them is handed the *texture* rather than a second copy
/// of the picture.
///
/// # What this is worth
///
/// Two panes on one site, one product and one tilt are looking at one buffer,
/// and that is by design rather than by coincidence: [`RenderCache`] shares an
/// `Arc<egui::ColorImage>` across them, and every call site here `Arc::clone`s
/// it. But each pane then ran its own `Context::load_texture`, which is a
/// distinct `egui::TextureId` and so a distinct `queue.write_texture` of the
/// whole raster, plus 16 MiB of duplicate VRAM held for as long as the pane
/// shows it.
///
/// Measured here against a 16.7 ms frame budget, uploading through
/// `egui_wgpu::Renderer` on an RTX 3090, median of eleven, three interleaved
/// rounds — one pane, then the same volume across two and four:
///
/// | panes | before | after |
/// |---|---|---|
/// | 1 | 8.44 ms | 8.46 ms |
/// | 2 | 17.27 ms | 8.44 ms |
/// | 4 | 35.67 ms | 8.45 ms |
///
/// So a four-pane split paid ~27 ms of extra frame thread per volume for three
/// textures whose contents were provably the same object. The resume path
/// measured the same shape (35.24 ms → 8.54 ms at four panes), and the
/// single-pane row is the control: it must not move, and does not.
///
/// # The VRAM saving is per volume, not instantaneous
///
/// This is scoped to a **pass**, so panes that arrive one at a time — a split
/// added between volumes, each pane served by its own `dispatch_pane_renders`
/// hit — still hold a texture each until the next volume lands, when one drain
/// serves all of them and they collapse onto one. The 48 MiB a four-pane split
/// stops holding is therefore the steady state, reached within one volume,
/// rather than something that takes effect the moment a pane is added. That is
/// the path that matters — the cost being removed was per volume, forever —
/// but it is not the same claim as "never more than one texture".
///
/// The overlay path has always done it the other way — one upload, the handle
/// cloned into each pane's `OverlayTextureData` — and so has the loop path
/// (`rendered_image` takes a `&TextureHandle`). This is the plan view catching
/// up with its two siblings. Nothing reads a per-pane `TextureId`:
/// `overlay_cache::draw_overlay_texture` and `ui_map_pane::render_radar_overlay`
/// both paint with `Color32::WHITE` and a full-rect uv, so the id is an argument
/// to `painter.image` and never an identity.
///
/// # Why the `Arc` is held and not just its address
///
/// Identity here is "the same buffer", which is exactly what `Arc::ptr_eq`
/// asks — the audit's own finding was that the bytes are the same *object* and
/// not merely equal. Keeping only the address would make that true by luck: an
/// entry could be dropped mid-pass and the next allocation land on it, and this
/// would then hand back a texture of somebody else's pixels. Holding the `Arc`
/// makes the address unreusable for as long as it is a key. It costs one
/// refcount per *distinct* raster in the pass, against buffers the render cache
/// is holding anyway.
///
/// Scoped to a pass rather than to the app, because a texture kept alive here
/// is VRAM and nothing else: the panes that want one are already holding their
/// own clones by the time this is dropped. The app used to hold replaced
/// handles a frame longer than that as well, in an `old_textures` vector; see
/// the note in `App::apply_render_to_pane` for why nothing needs to.
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

        // Ensure pane_render vec matches gui pane count
        self.render.ensure_pane_count(self.gui.pane_count());
        // And the other thing keyed by pane index that a layout change strands:
        // a hidden 3D pane's voxel grid and offscreen. Here rather than in the
        // pane loop because this is a point at which every pane is in the
        // vector — `Gui::ui` below opens the frame's first `mem::take` window —
        // and ahead of the dispatchers so the budget they are measured against
        // is not being spent on panes nobody can see. See
        // `App::release_hidden_pane_volumes`.
        self.release_hidden_pane_volumes();

        // The frame's egui context, resolved once. The two passes below that
        // upload a plan-view texture are handed it rather than each reaching
        // through `self.state` for a copy of their own: one `unwrap` on the
        // renderer per frame instead of three, and it is what lets both of them
        // be driven by a test against a bare `egui::Context`, which is all
        // `Context::load_texture` has ever needed.
        let ctx = self.state.as_ref().unwrap().egui_renderer.context().clone();

        // Before the pollers, which is before `Gui::ui` builds the paint list: a
        // raster whose last band landed on the previous frame goes on screen in
        // *this* frame's paint list rather than the next one. See the callee.
        self.promote_uploaded_rasters();

        self.poll_render_results(&ctx);
        self.poll_section_results(&ctx);
        self.poll_level3_results();
        self.poll_site_catalogue();
        self.poll_overlay_render_results(&ctx);
        self.poll_loop_scan_list_results();
        self.poll_loop_scan_download_results();
        self.poll_loop_l3_list_results();
        self.poll_loop_l3_fetch_results();
        self.poll_loop_render_results(&ctx);
        self.poll_loop_section_results(&ctx);
        self.advance_loop_playback();
        self.dispatch_pane_renders(&ctx);
        self.dispatch_section_renders();
        self.dispatch_loop_renders();
        // After the dispatch, which is the only thing that grows the store,
        // and before the GUI pass that paints from it — so a grid that has
        // just been evicted is never one a callback is about to march. The
        // hard bound on resident voxel grids; see
        // `VolumeStore::enforce_budget`.
        //
        // What it is held to is the loop pool's own answer — one share per
        // *distinct* 3D loop, which is what stops two panes on one volume being
        // charged twice — floored at `Budgets::volume_loop_bytes` so a
        // session with no 3D loop at all still has room for the live grids the
        // store holds for ordinary 3D panes. A bound of zero there would evict
        // a live volume every frame and rebuild it every frame.
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

        // Last, so this frame is laid out over everything applied above.
        let gui_action = self.gui.ui(&ctx);

        (size_in_pixels, gui_action)
    }

    /// Poll for completed background render results and upload textures.
    fn poll_render_results(&mut self, ctx: &egui::Context) {
        // One upload per raster for the whole drain, not per pane served from
        // it. The origin pane and every sibling below hold the *same*
        // `Arc<ColorImage>` — that is what the broadcast hands them — and this
        // is what stops each of them turning it into 16 MiB of its own VRAM.
        let mut uploads = PlanViewUploads::default();
        while let Ok(rr) = self.channels.render_receiver.try_recv() {
            if rr.pane_idx < self.render.pane_render.len() {
                // Unconditionally, and before every gate below: a result that
                // is stale, or for a pane that has since stopped drawing a plan
                // view, still means this render is over. The key it was holding
                // goes with the flag — a sibling waiting on a render that has
                // already answered would wait for ever.
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
            // The pane keeps whatever it was showing, which is what a missing
            // tilt should look like.
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

            // Cache the render output for sharing with other panes on the same site
            let origin_site = self
                .gui
                .pane(origin_pane)
                .map(|p| p.site.clone())
                .unwrap_or_default();
            // `RenderView::PlanView` because this is the plan-view path and
            // only the plan-view path: `dispatch_pane_renders` starts no render
            // for a non-map pane, and `CachedRenderOutput` is a square
            // plan-view raster by construction. The axis exists so a section
            // cached later cannot be handed to this consumer — see
            // `RenderCacheKey`.
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

            // Apply to the originating pane — unless it stopped being a map
            // while this render was in flight. `dispatch_pane_renders` no longer
            // starts one for a non-map pane, but a conversion after dispatch is
            // a live race, and the result would land as a plan-view texture on
            // a pane that draws none. `render_in_flight` was already cleared
            // above, and `last_rendered` stays unset, so converting back
            // re-dispatches.
            if !self.gui.pane_has_no_plan_view(origin_pane) {
                self.apply_render_to_pane(ctx, origin_pane, &render_result, &mut uploads);
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
                        self.apply_render_to_pane(ctx, other_idx, &render_result, &mut uploads);
                    }
                }
            }
        }
    }

    /// Apply a rendered radar image to a specific pane (upload texture to overlay cache).
    ///
    /// `uploads` is the pass's record of what is already on the GPU. A pane
    /// served from a raster another pane in this same pass was served from gets
    /// that pane's [`egui::TextureHandle`], not a second upload of it — see
    /// [`PlanViewUploads`].
    fn apply_render_to_pane(
        &mut self,
        ctx: &egui::Context,
        pane_idx: usize,
        render: &crate::render_dispatch::CachedPaneRender,
        uploads: &mut PlanViewUploads,
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

        // Whether the picture being applied is the picture already on this
        // pane — the *same buffer*, not a buffer that compares equal.
        //
        // `cached_render.image` is the `Arc` this pane's **newest** texture was
        // uploaded from — the one it is showing, or the one it is holding while
        // the pixels arrive — and the two are only ever written together: here,
        // and in `restore_cached_render`. Everything that invalidates one clears
        // the other or clears the cache outright (`clear_graphics_state`,
        // `dispatch_pane_renders`' no-scan arm, `reset_panes*`), so a `true`
        // here means these pixels are the ones already on their way to the GPU
        // or already there.
        //
        // # The case this exists for
        //
        // The four products `RadarProduct::tilt_independent_plan_view` names
        // draw the same picture at every tilt, and `render_cache_key` collapses
        // them onto `NO_ELEVATION_SLOT` for exactly that reason. So a tilt click
        // on one of those panes is a cache **hit** — and `needs_render` is still
        // true, because it compares the raw elevation — which put the whole
        // 16 MiB back on the GPU to redraw a picture that was already on it.
        // Only `RadarTextureMeta::elevation` genuinely had to move, and it still
        // does: the rest of this function runs unchanged, so the pane is
        // re-described even though it is not re-uploaded. That matters rather
        // than being tidy — `PaneState::stale_image_on_screen` reads that field
        // and nothing else, so a pane whose upload was skipped without the
        // restamp would disown its own correct picture for good.
        //
        // Stated as buffer identity rather than as the tilt predicate because
        // the predicate is one instance of it. Any cache hit that hands a pane
        // back the raster it is already showing is the same waste, and this
        // needs no second list to stay in step with the first.
        let already_on_screen = self
            .render
            .pane_render
            .get(pane_idx)
            .and_then(|prs| prs.cached_render.as_ref())
            .is_some_and(|cached| Arc::ptr_eq(&cached.image, &render.image));

        // Let go of the old radar overlay texture — unless it is the one about
        // to go back, in which case it is kept rather than retired and
        // re-uploaded.
        //
        // # Why the replaced handle is simply dropped
        //
        // There used to be an `App::old_textures` holding pen here: every
        // replaced handle was pushed onto it and the vector cleared at the top
        // of the *next* frame, "to let the GPU finish using them before we drop
        // them". That was written into the app during a wholesale rewrite
        // (8800bf20) rather than in answer to any crash, and every layer below
        // it already guarantees what it was defending. It cost a standing second
        // copy of every replaced overlay texture for a whole extra frame, which
        // on web is the single largest allocation this application makes:
        // measured in Chromium on hardware WebGL2, dropping it took standing
        // overlay residency after a zoom from 395.6 MiB to 257.0 MiB.
        //
        // 1. **Nothing in this frame draws it.** Each of the five sites the
        //    vector used to serve runs before `Gui::ui` builds the frame's paint
        //    list — four in the poller/dispatcher block of `setup_egui_frame`,
        //    and `restore_cached_render` earlier still, under
        //    `ensure_rendering_state`, which `handle_redraw` calls ahead of
        //    `setup_egui_frame`. "Before `Gui::ui`, inside `handle_redraw`" is
        //    the claim that covers all five; "inside `setup_egui_frame`" does
        //    not. The handle being let go here is already out of the pane's
        //    overlay cache, so no shape in the frame under construction names
        //    it, and the frame that did draw it was submitted at least one
        //    `queue.submit()` ago.
        //
        // 2. **egui already defers the free by a frame's tail.** Dropping the
        //    handle only reaches `epaint::TextureManager::free`, which pushes
        //    the id onto `delta.free`; the delta is taken at `end_pass` and
        //    `Renderer::free_texture` is not called until after
        //    `queue.submit()` in `present_frame`. Ids are never recycled —
        //    `TextureManager::alloc` bumps a monotonic `next_id` — so a freed
        //    id cannot collide with a live one.
        //
        // 3. **wgpu defers the raw delete past the submission using it.**
        //    `free_texture` calls `wgpu::Texture::destroy()`, which is the wgpu
        //    API and not a raw `glDeleteTextures`. wgpu-core's
        //    `Texture::destroy` snatches the raw handle out (so no *new*
        //    command can name it) and hands it to
        //    `LifetimeTracker::schedule_resource_destruction(temp,
        //    last_submit_index)`, which parks it in the still-active
        //    submission's `temp_resources`. Those are dropped — and the raw
        //    image actually destroyed — only in `triage_submissions`, once the
        //    GPU has signalled past that index. When
        //    `get_texture_latest_submission_index` answers `None` the temp is
        //    dropped on the spot, which is safe for the reason it is `None`:
        //    no active submission names the texture. That code is in
        //    wgpu-**core**, so it is identical for the Vulkan backend used
        //    natively and the GL/WebGL2 backend the wasm build pins via
        //    `Backends::GL`; there is no browser-WebGPU path here to reason
        //    about separately.
        //
        // And the case that settles it, because it is this application's own and
        // has been true all along: **map tiles have always done the far more
        // dangerous thing without harm.** `tile_source::Tiles` keeps its tiles
        // in a 256-entry `LruCache`, and `Tiles::at` — "called once per visible
        // tile per frame by walkers' flood fill" — calls
        // `receive_one_fetched_tile`, whose `cache.put` evicts at capacity and
        // drops a `walkers::Tile::Raster(TextureHandle)` *in the middle of
        // `Gui::ui`*, where a shape added earlier in the same pass can already
        // name the id. `old_textures` never covered that path, and panning a map
        // does it continuously without artifact. This change therefore makes
        // overlay textures exactly as safe as map tiles have always been, and
        // strictly safer: every site above lets go before the pass opens, where
        // tile eviction lets go inside it.
        //
        // In short, an in-flight submission holding this texture is exactly the
        // case wgpu is tracking submission indices in order to handle. An extra
        // application-level frame of deferral buys nothing and doubles
        // residency. If a future crash ever seems to want it back, measure
        // first: the bug will be one of the assumptions above breaking, and the
        // fix belongs wherever it broke — and whatever it is, it would have been
        // breaking tile eviction for longer.
        let Some(pane) = self.gui.pane_mut(pane_idx) else {
            return;
        };
        let cache = pane.overlay_cache_mut(OverlayKind::Radar);
        // The pane's own handle for these exact pixels, if it has one, and
        // whether that handle is **whole**.
        //
        // Two slots to look in, and the order matters: `cached_render.image`
        // describes whichever of the two is newer, and a hold is only ever newer
        // than the picture it will replace. A raster still arriving is reused
        // *and kept held* — re-describing it here does not put another texel on
        // the wire, so a second `hold` for the same id will be promoted by the
        // same completion the first was waiting for. One already on screen is
        // reused and shown at once, which is what stops a tilt click on a
        // tilt-independent product blanking a pane for the length of an upload
        // that is not going to happen.
        let retained = already_on_screen
            .then(|| match cache.held_texture() {
                Some(arriving) => Some((arriving.clone(), false)),
                None => cache.current().map(|old| (old.texture.clone(), true)),
            })
            .flatten();

        // The picture's own dimensions, not a constant: a sweep reaching past
        // the floor is a wider raster, and the texture, the overlay entry and
        // the hover all have to agree about which. `plan_view_image` already
        // refused anything that is not a size this build makes, so there is
        // nothing left here to validate.
        let side = render.image.width();
        let (texture, whole) = match retained {
            // The pane's own handle, preferred over anything `uploads` may hold
            // for the same raster. Not a lifetime question — see the note above
            // on why a replaced handle can just be dropped — but a churn one:
            // taking the memo's copy would free this one and retain that one to
            // no purpose, when the pane is already holding the texture it is
            // about to be told to draw.
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

        // Store in overlay cache with radar metadata. The bounds come from the
        // render's own `max_range_km` — the half-width it projected at — so
        // the texture is placed on exactly the ground its gates were painted
        // onto, whether that is a TDWR Doppler cut's 88.8 km or the same
        // radar's 417 km long-range reflectivity.
        let bounds = ImageBounds::from_radar_site(lat, lon, render.max_range_km);
        let geo_bounds = GeoBounds {
            min_lat: bounds.min_lat,
            max_lat: bounds.max_lat,
            min_lon: bounds.min_lon,
            max_lon: bounds.max_lon,
        };
        let pane = self.gui.pane_mut(pane_idx).unwrap();
        // Dropping this call is silent: the pane simply keeps whatever time it
        // was last stamped with, which reads as a current image of another
        // volume. The lookup inside the callee is the dispatcher's own tests'
        // business; that this function *makes the call* is `stamping_tests`
        // below. It travels with the picture rather than being written now —
        // see `RenderDispatcher::data_time_for_render`.
        let data_time = self.render.data_time_for_render(pane, render);
        let placed = OverlayTextureData {
            texture,
            geo_bounds,
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
                // Where the classification behind these pixels stood, on the
                // same terms as the fold limit: it describes *this* image, and
                // the object it came from belongs to a volume that will have
                // rolled by the time anything could look it up again.
                melting_layer_source: render.melting_layer_source,
                // And where the shift behind them came from, on exactly the
                // same terms: it describes *this* image, and the `N0S` it was
                // read from belongs to a volume that will have rolled by the
                // time anything could look it up again.
                storm_motion: render.storm_motion,
                // What these pixels are, travelling with them. Whichever
                // datasource produced them: this is the one assignment behind
                // `PaneState::stale_image_on_screen`, so a Level II and a
                // Level III image are described identically and neither can
                // stay on screen unlabelled after the selection moves.
                product: render.product,
                elevation: render.elevation,
            }),
            hit_map: None,
        };

        // **The swap, or the promise of one.**
        //
        // A texture the pane already had, whole, goes up now: there is no
        // upload to wait for and waiting on one that will never come is a pane
        // that never changes again. Everything else is *held* — the picture the
        // pane is showing stays on screen, entire, with its own ground and its
        // own caption, until `promote_uploaded_rasters` finds that the last band
        // has landed and swaps the lot in one step.
        //
        // # What ends a hold, and why it always ends
        //
        // Four things, and between them they cover every way a raster can fail
        // to arrive:
        //
        // 1. **Its own delivery.** Bounded by construction: `BandPlan::of`
        //    guarantees at least one row of progress per band, `DECLINE_PATIENCE`
        //    bounds a ring that will not hand a slot over, and
        //    `Gui::any_raster_held` keeps `handle_redraw` asking for the frames
        //    those bands move on.
        // 2. **A newer render.** The `hold` below replaces one still arriving —
        //    a site switch, a product change or a tilt click mid-upload lands
        //    here, and the superseded handle drops on the spot.
        // 3. **The cache being cleared.** `OverlayTextureCache::clear` takes both
        //    slots, so a pane that loses its scan, or the whole graphics state,
        //    is not holding a picture it has decided not to show.
        // 4. **A renderer rebuild.** `restore_cached_render` releases every hold
        //    before it re-uploads, because an id from a dead `egui::Context` is
        //    the one id `is_delivered` will answer `false` about for ever.
        //
        // A render that never arrives at all — a failed decode, a cancelled job,
        // a tilt with no sweep — reaches none of this: `poll_render_results`
        // returns before `apply_render_to_pane` and the pane keeps what it had,
        // exactly as it did before any of this existed.
        //
        // The one raster that is not held is the one with nothing to hold *for*;
        // see `PaneState::place_radar_raster`.
        pane.place_radar_raster(placed, data_time, whole);

        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].last_rendered =
                Some((render.product, render.elevation));
        }
    }

    /// Show every held raster whose last band has landed.
    ///
    /// # Where this runs, and why it is the first thing in the frame
    ///
    /// At the top of [`Self::setup_egui_frame`], ahead of the pollers that stage
    /// new holds and well ahead of `Gui::ui`. A raster whose bands finished on
    /// the previous frame's `end_pass_and_upload` is therefore on screen in the
    /// paint list of the very next frame — the swap costs one frame and never
    /// two, and a hold staged by a poller a few lines below is not asked about
    /// on the frame it was staged, when the answer could only be no.
    ///
    /// # Why it asks rather than being told
    ///
    /// `TextureUploads` could have handed out a list of ids that finished this
    /// frame, and that list would have to be consumed exactly once by a frame
    /// that is guaranteed to happen. This asks instead, every frame, and a frame
    /// that does not run costs nothing — the same level-triggered reasoning
    /// `OverlayTextureCache` gives for having no sequence number.
    ///
    /// With no renderer there is nothing to ask, and holds simply stand: a
    /// headless `App` has no GPU, and the frame that builds one goes on to
    /// release every hold in `restore_cached_render` anyway.
    fn promote_uploaded_rasters(&mut self) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let renderer = &state.egui_renderer;
        self.gui
            .promote_held_rasters(|id| renderer.is_delivered(id));
    }

    /// Promote every held raster, as the frame after the last band lands does.
    ///
    /// A headless `App` has no renderer to ask, so `promote_uploaded_rasters`
    /// above returns before it can promote anything. This says "they all
    /// landed", which is the only thing a test with no GPU can honestly say —
    /// what a band costs and how many frames it takes belong to
    /// `texture_upload`'s own tests and to `tests/raster_upload_gpu.rs`, and
    /// what a pane *shows* once they have landed belongs here.
    #[cfg(test)]
    pub(super) fn deliver_held_rasters(&mut self) {
        self.gui.promote_held_rasters(|_| true);
    }

    /// Take the launch's one catalogue refresh and write it to the cache.
    ///
    /// **It is normally not applied to the live table**, and that is the whole
    /// design: the table was resolved from the cached catalogue before the
    /// first frame, and applying a fresh one here would add a map marker, add a
    /// site-list row and shift a section's height datum under a user who is
    /// already looking at them. The refresh is for the *next* launch. See
    /// [`crate::site_catalogue`].
    ///
    /// # The launch that cannot wait
    ///
    /// A launch with no cached catalogue knows no network. Since the
    /// compiled-in table was deleted the binary carries nothing either, so
    /// "apply it next launch" means a run whose site list is only the radars
    /// this install happens to have decoded — an empty list on a first run, and
    /// one or two rows for anyone else. The following run would be fine and
    /// this one would look broken.
    ///
    /// It is not only the first run, and reading it that way is what shipped
    /// the bug: a returning user has a stored config and a learned row, and on
    /// the web — where the reload that would fix it is a relaunch nobody thinks
    /// to perform — that reads as a network with one radar in it.
    ///
    /// So when [`App::catalogue_pending`] says nothing had yet told this binary
    /// which radars exist, this resolves the catalogue immediately; and when
    /// [`App::site_hint_pending`] says the launch brought no site of its own,
    /// it then runs the timezone hint against it. Neither step can violate the
    /// rule the next-launch policy exists to protect — see the two functions
    /// below for why, one each.
    ///
    /// Each is spent once, however long the session runs, and each is spent by
    /// its own step. They were one flag and one function until a returning web
    /// user's saved site suppressed the catalogue outright; the two questions
    /// are independent and are now asked independently.
    ///
    /// Drains rather than taking one, like every sibling poller, even though
    /// exactly one message can ever arrive: a poller that leaves a message in
    /// the queue is a poller that needs another frame to come, and the frame
    /// after a startup fetch is not guaranteed.
    fn poll_site_catalogue(&mut self) {
        while let Ok(response) = self.channels.site_catalogue_receiver.try_recv() {
            // A failed fetch is silent by design — offline is not an error
            // state here, it is a launch that runs on the cache. `catalogue`
            // has already logged the reason at `debug`.
            //
            // On a fresh install with no network it is also the end of the road
            // for this session: the site list holds only what this install has
            // decoded and says so, which is the honest reading of "nothing has
            // told this binary which radars exist". Both flags stay set, so the
            // next launch that reaches the network still fills it in.
            let Some(fetched) = response.catalogue else {
                continue;
            };
            let store = self.platform.config_store();
            crate::site_catalogue::store_if_changed(
                store.as_deref(),
                &self.site_catalogue,
                &fetched,
            );
            // Held whether or not the write took, and deliberately not folded
            // into the `store_if_changed` branch it used to sit in. Site data
            // blocked, a sandboxed iframe, and a full `localStorage` all answer
            // `false` there, and treating that as "keep the empty one" discarded
            // a catalogue that had already arrived — leaving the session with no
            // radars *and* the flag spent, on precisely the platforms least able
            // to spare it. Persistence is how the *next* launch benefits; this
            // one benefits from the value in hand.
            self.site_catalogue = fetched;
            if self.catalogue_pending {
                self.adopt_the_first_catalogue();
            }
            // Asked separately rather than nested inside the adopt above: a
            // catalogue that places nothing leaves the table empty, and a launch
            // with no site of its own still needs the hint run against whatever
            // did arrive.
            if self.site_hint_pending {
                self.open_on_the_timezones_radar();
            }
        }
    }

    /// Put the first catalogue this install ever fetched into the live table.
    ///
    /// # It cannot move anything a user is looking at
    ///
    /// Which is the rule the next-launch policy exists to protect, and the
    /// reason this one exception is safe. Reached only while
    /// [`App::catalogue_pending`] holds — no catalogue has been in the table —
    /// so every row present came from a position learned off a volume this
    /// install decoded. [`rustdar_radar::sites::SiteFix::Learned`] outranks
    /// `Network`, and `sites::extended` settles rank before it builds a row, so
    /// a fetched position is never *applied* to a row a learned one claims,
    /// let alone allowed to overwrite it. The first catalogue can only add
    /// rows: no marker moves, no label changes, no height datum shifts.
    ///
    /// Spends [`App::catalogue_pending`], so every later catalogue takes the
    /// ordinary next-launch path.
    fn adopt_the_first_catalogue(&mut self) {
        self.catalogue_pending = false;
        self.gui.set_catalogue_pending(false);
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
    }

    /// Open on the radar nearest this device's timezone.
    ///
    /// Reached only while [`App::site_hint_pending`] holds — this launch had no
    /// stored configuration naming a site and no table to resolve one against —
    /// so there is no site the user chose for this to overrule. That is the
    /// whole of its licence to change the open pane, and why it is gated on a
    /// different question from the catalogue apply above: a returning user has
    /// a site, and a catalogue landing mid-session must never move them off it.
    ///
    /// Spends [`App::site_hint_pending`] on the way in, whether or not the hint
    /// finds anything, so a catalogue that arrives without a usable timezone
    /// cannot leave this armed for a later one.
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
        if self.gui.pane(0).is_some_and(|pane| pane.site == site) {
            return;
        }
        log::info!("opening on {site}, nearest to timezone {zone}");
        // Through the action a click on the site picker raises, for the reason
        // spelled out in `upgrade_provisional_site`: assigning `pane.site` is
        // only the visible third of a site change, and the part that matters
        // here is that this also spawns the fetch. `set_initial_site` is right
        // only before the event loop, where the app's own first fetch reads the
        // site it leaves behind — and this runs inside a frame.
        self.handle_gui_action(
            crate::app::GuiAction::SwitchRadarSite {
                site: site.to_string(),
                pane_idx: self.gui.active_pane_idx(),
            },
            None,
        );
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
                    "Env heights moved for {}: dropped the renders that read them",
                    sounding.site
                );
            }
        }
        while let Ok(ml) = self.channels.melting_layer_receiver.try_recv() {
            if self.render.is_fetch_stale(&ml.site, ml.generation) {
                continue;
            }
            // A site that published no object for this volume caches nothing
            // and is not retried: `fetch_product_for_volume` already opened
            // every candidate in the pairing window, so "not found" is an
            // answer about the volume rather than a failure to reach the
            // bucket. The classification falls to the next rung and the pane
            // says which one — the gap is visible, not silent.
            let Some(bytes) = ml.object else {
                continue;
            };
            log::info!(
                "Melting layer cached for {} (volume {}, {} bytes)",
                ml.site,
                ml.volume_start,
                bytes.len()
            );
            // Through the setter, so a classification pane already drawn
            // against the fleet default is redrawn against the measured layer
            // rather than waiting for the volume to roll — the same reason the
            // sounding above goes through `set_env_heights`.
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
            // A site that published no `N0S` for this volume, or one whose PDB
            // carried no vector, caches nothing and is not retried — the same
            // reading of "not found" the melting layer above takes. SRV falls
            // to the next rung of `rustdar_radar::srv::storm_motion` and the
            // pane says which one.
            //
            // **`Some((0.0, 0.0))` does not land here.** It is a vector the
            // RPG applied — SCIT tracked no cells and the field went
            // unshifted — so it passes this gate like any other reading and is
            // cached. Only `None`, which is the absence of an object rather
            // than a zero in one, stops.
            let Some((speed_kt, direction_deg)) = sm.motion else {
                continue;
            };
            log::info!(
                "Storm motion cached for {} (volume {}): {speed_kt:.1} kt from {direction_deg:.1}°",
                sm.site,
                sm.volume_start,
            );
            // Through the setter, so an SRV pane already drawn against a
            // Bunkers right-mover is redrawn against the RPG's own vector
            // rather than waiting for the volume to roll — the same reason the
            // two above go through theirs.
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
    ///
    /// Handed the frame's context rather than reaching through `self.state` for
    /// one, for the reason the resolution site gives: an `AppState` is a wgpu
    /// device, a surface and a window, and `Context::load_texture` needs none of
    /// them. Taking it as a parameter is what lets this poller be driven against
    /// a bare `egui::Context`, as its plan-view and loop siblings already are —
    /// and this is the poller with the largest buffer passing through it.
    fn poll_overlay_render_results(&mut self, ctx: &egui::Context) {
        use rustdar_egui::overlay_cache::OverlayTextureData;

        while let Ok(resp) = self.channels.overlay_render_receiver.try_recv() {
            // Load texture once, then clone handle to all target panes.
            //
            // The pixels arrive already converted — see
            // `OverlayRenderResponse::image` — so nothing here walks the buffer.
            // The upload takes the `Arc` itself (`ImageData: From<Arc<ColorImage>>`),
            // so it is not copied on the way to the GPU either.
            self.texture_counter += 1;
            // The picture's own dimensions rather than a pair carried beside it:
            // the rasterizer was handed a width and a height and answered with
            // exactly that many pixels, and a second copy of the numbers is a
            // second thing that can disagree with the texture being placed.
            let [width, height] = resp.image.size;
            let (width, height) = (width as u32, height as u32);
            let tex_name = format!("overlay_{}", self.texture_counter);
            let texture = ctx.load_texture(
                tex_name,
                Arc::clone(&resp.image),
                egui::TextureOptions::LINEAR,
            );

            for &pane_idx in &resp.pane_indices {
                let Some(pane) = self.gui.pane_mut(pane_idx) else {
                    continue;
                };

                let cache = pane.overlay_cache_mut(resp.overlay_kind);

                cache.render_in_flight = false;

                // Every result is stored, and the staleness question is asked
                // next frame instead. `resp.generation` is a content token, not
                // a sequence number (`ui_map_pane::overlay_cache_token`), so
                // there is no order to compare it in — and none is needed:
                // `needs_rerender` re-asks whether the stored token, zoom and
                // bounds still describe what the pane wants, so a late result
                // is superseded by the same test that asked for it. See
                // `OverlayTextureCache` for the counter that used to be
                // compared here and why it had to go rather than be renamed.

                // The assignment below is what retires the texture this pane was
                // showing: the replaced `OverlayTextureData` drops with it, and
                // that is the whole of the cleanup. See the note in
                // `App::apply_render_to_pane` for why no frame of deferral is
                // needed — this is the path it costs the most on, because these
                // are the full-viewport overlay rasters.
                cache.show(OverlayTextureData {
                    texture: texture.clone(),
                    geo_bounds: resp.geo_bounds,
                    data_generation: resp.generation,
                    render_zoom: resp.zoom,
                    width,
                    height,
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
        // Commit on release. A `DragValue` produces a value every frame, and
        // the invalidation below is not cheap: it evicts every storm-relative
        // grid and section. Applied per drag frame that is ~210 ms of re-cut
        // for a cross-section and, for a 3D loop, the whole resident set —
        // fourteen grids and ~1.9 s of resample, thrown away and restarted on
        // the next frame, so a loop would never finish building while a finger
        // was on the widget. Holding the commit makes the cost proportional to
        // the edit rather than to how long it took. See
        // `Gui::storm_motion_mid_edit` for why this is a widget-state question
        // rather than a timeout.
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
        // Shared across the pane loop for the reason `poll_render_results`
        // shares it: two panes that convert to the same `(site, product, view,
        // elevation)` in one pass are two cache hits on one entry, and so two
        // `Arc::clone`s of one buffer.
        let mut uploads = PlanViewUploads::default();
        for pane_idx in 0..self.gui.pane_count() {
            // Ahead of the rendering-params branch, not inside it. A pane with
            // no plan view still has a product and an elevation selected —
            // they are flat fields — so it would take the `if` arm and buy a
            // full-size plan-view image plus an equally large `f32` value
            // grid, per pane per selection change, that nothing draws. Under the `else` arm it would instead have its radar
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

                if needs_render && !prs.render_in_flight() {
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
                    //
                    // The cache above only answers for renders that have come
                    // *back*, so on the frame a volume lands it misses for
                    // every pane at once — and this pass then started one
                    // render per pane of one sweep, each preceded by its own
                    // `RenderInput::extract` on this thread. Measured over four
                    // sites and three products at 70–175 ms of extra CPU and
                    // ~15,000 extra minor faults per extra pane, per volume;
                    // the faults because `rustdar_radar::render`'s pools hold
                    // one buffer each, so concurrent duplicates can only be
                    // served once and the rest fault a fresh 16 MiB texture,
                    // 16 MiB grid and 32 MiB cell array back in.
                    //
                    // Nothing is deferred by skipping. `poll_render_results`
                    // already broadcasts one result to every pane wanting that
                    // site, product and tilt — the pass `PlanViewUploads` was
                    // built around — so these panes are waiting on precisely
                    // the result they would have been handed anyway, and the
                    // one they *would* have started would have been discarded
                    // by the same broadcast a moment later.
                    if self
                        .render
                        .plan_view_in_flight(&pane_site, product, elevation)
                    {
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
                    } else if let Some((data, declared)) = self.scan_data.get(scan_info.site.name) {
                        // Cloned out of the map before the dispatcher is
                        // borrowed mutably; both are refcounts.
                        let (data, declared) = (Arc::clone(data), Arc::clone(declared));
                        self.render.spawn_level2_render(
                            pane_idx,
                            &params,
                            &pane_site,
                            data,
                            &declared,
                            // Which volume this render is *of*, so the
                            // dispatcher can decide whether the melting-layer
                            // object it holds belongs to it. Off the pane's own
                            // `scan_info`, which is the same field
                            // `spawn_level3_fetches` paired that object
                            // against — one statement of "the volume on this
                            // pane", read at both ends.
                            scan_info.timestamp,
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
                    cache.clear();
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
                .is_some_and(|p| p.render_in_flight())
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
                .map(|(scan, declared, _)| (Arc::clone(scan), Arc::clone(declared)));
            let overlay = self.chunk_feeds.snapshot(site.as_str());

            // The two refusals that have to be *named* rather than left as a
            // blank pane. Checked here, before any budget is taken, because both
            // are properties of the volume and the product rather than of the
            // cut — dispatching would burn a render slot to be told the same
            // thing, and on wasm there is only one slot.
            if let Some(reason) = section_source_refusal(
                base.as_ref().map(|(scan, _)| scan.as_ref()),
                overlay.as_ref().map(|live| live.scan.as_ref()),
            ) {
                // All three reasons resolve themselves — the mid-flight
                // pattern arrives with the next volume start, the first sealed
                // sweep is minutes away at most, the first download is already
                // in flight — so the key is *not* written: the pane will ask
                // again, and get an answer.
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
                state.render_finished();
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
            // Assigning retires the cut this pane was showing; see the note in
            // `App::apply_render_to_pane`.
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
    ///
    /// # The last full-size unmultiply on the frame thread is gone from here
    ///
    /// This ran on the frame thread on **both** targets and was the one raster
    /// walk that did — 8 MiB per cut natively — because the pane retains the
    /// `CrossSection` rather than a converted copy, so converting before the
    /// send used to mean carrying both for the life of the session. That trade
    /// is not the one that was taken: `offload::execute` premultiplies the
    /// section's own raster inside the job, so the cut the pane retains is
    /// already in egui's convention and this is a length assertion and a copy.
    /// The resume path below re-uploads from the same retained bytes and gets
    /// the same picture, which is what makes one conversion enough.
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

        // Every raster still arriving is let go of first, on **every** pane and
        // whether or not this goes on to restore one.
        //
        // This runs when a new `AppState` has just been built, so every
        // `TextureHandle` in the application belongs to an `egui::Context` that
        // no longer exists. The upload path that would have finished those
        // uploads went with it, and a fresh `TextureUploads` answers
        // `is_delivered` with `false` about an id it has never seen — correctly,
        // because those texels are on no GPU. So a hold left here is the one
        // hold nothing would ever end: the pane would keep a dead handle on
        // screen and `any_raster_held` would keep the event loop at refresh rate
        // for the rest of the session, asking a question whose answer cannot
        // change.
        //
        // On every pane, not only the ones restored below, because the panes
        // this skips are exactly the ones nothing else would come back for: a
        // pane with no `cached_render` and a pane that has stopped being a map
        // both fall out of the loop before they reach a `show`.
        self.gui.release_held_rasters();

        // Section panes first, and through their own loop: the one below is
        // bounded by `pane_render.len()` and skips every pane with no plan
        // view, which is every section pane there is.
        self.restore_section_textures(ctx);

        // Panes sharing a raster shared it before the context died too:
        // `apply_render_to_pane` stores `Arc::clone(&render.image)` into each
        // pane's `cached_render`, so a resume with four panes on one site is
        // four copies of one buffer and must be one upload. This is the path
        // where paying four would be worst — a resume on Android is the frame
        // with the least budget there is.
        let mut uploads = PlanViewUploads::default();

        for pane_idx in 0..self.render.pane_render.len().min(self.gui.pane_count()) {
            // `dispatch_pane_renders` deliberately *keeps* `cached_render` on a
            // converted pane, so that converting back to a map is instant. That
            // makes this the one place the kept copy could still be uploaded: every
            // suspend, resume and surface loss would re-create a full-size
            // plan-view texture in the Radar overlay cache of a pane that draws
            // no map.
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

            // The cached extent, not a fresh one: these are the pixels the
            // render produced, so they belong on the ground that render
            // projected them onto. A resume that rebuilt the bounds from
            // anything else would put a restored long-range image back at the
            // wrong size, which reads as a pane that moved while suspended.
            let bounds = ImageBounds::from_radar_site(lat, lon, max_range_km);
            let geo_bounds = GeoBounds {
                min_lat: bounds.min_lat,
                max_lat: bounds.max_lat,
                min_lon: bounds.min_lon,
                max_lon: bounds.max_lon,
            };
            if let Some(pane) = self.gui.pane_mut(pane_idx) {
                let cache = pane.overlay_cache_mut(OverlayKind::Radar);
                // Showing retires whatever the pane was showing; see the note
                // in `App::apply_render_to_pane`.
                cache.show(OverlayTextureData {
                    texture,
                    geo_bounds,
                    data_generation: 0,
                    render_zoom: 0,
                    width: side as u32,
                    height: side as u32,
                    radar_meta: Some(RadarTextureMeta {
                        hover: Arc::clone(&cached.hover),
                        lat,
                        lon,
                        max_range_km,
                        // The restored image depicts what the cached render did,
                        // so it is described the same way. A resume that put the
                        // pixels back without this would leave a pane that had
                        // been switched while suspended showing the old product
                        // with nothing saying so — and, for the fold limit,
                        // annotating one cut's velocity with another cut's PRF.
                        nyquist_ms,
                        // And, for the melting layer, restoring a fleet-default
                        // classification with no qualification on it at all —
                        // the picture this whole path exists to stop being
                        // indistinguishable from a measured one.
                        melting_layer_source,
                        // And, for the storm motion, restoring a right-mover
                        // prediction with nothing saying so — a field shifted
                        // by a vector the RPG never applied, indistinguishable
                        // from the reference product.
                        storm_motion,
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
        //
        // An empty guest list does not merely skip the pass — it gives the
        // texture back. The mirror is frame-sized and singular (up to 64 MiB on
        // desktop, 16 MiB on web and mobile;
        // `constants::VOLUME_MIRROR_BYTES_MAX`), and `release_pane` cannot free
        // it because no single pane owns it. This is the one place that knows
        // whether *anybody* still wants a floor, so it is the place that
        // answers: closing the last 3D pane must not hold the frame's worth of
        // colour for the rest of the session.
        let mirror_rects = self.gui.mirror_source_rects();
        // How much the 3D panes on this frame are stretching the ground they
        // sample, folded into the rung the mirror is drawn at. Taken even when
        // nothing wants a mirror, so the demand cannot survive into a frame it
        // was not measured on.
        let demand = self
            .volume_painter
            .as_ref()
            .and_then(|painter| painter.take_floor_demand());
        let mirror_target = if mirror_rects.is_empty() {
            if let Some(resources) = state
                .egui_renderer
                .callback_resources_mut()
                .get_mut::<crate::volume::bridge::VolumeResources>()
            {
                resources.release_mirror();
            }
            None
        } else {
            let points = state.egui_renderer.context().pixels_per_point();
            // Sized in **points**, from the UI rather than from the surface:
            // the mirror is no longer the frame. Each 3D pane draws its own map
            // into a strip below the frame's bottom edge, and the mirror has to
            // reach down to the lowest of them — the frame's own pixel size
            // covers none of that, and using it would leave every floor
            // sampling the top of a texture whose content is underneath.
            let size_in_points = self.gui.mirror_size_points();
            let plan = self.mirror_rungs.observe(
                demand,
                [size_in_points.x, size_in_points.y],
                points,
                crate::egui_renderer::MirrorLimits::for_device(
                    state.device.limits().max_texture_dimension_2d,
                    self.budgets.mirror_bytes,
                ),
            );
            let format = state.egui_renderer.attachment_config().color_format;
            let device = state.device.clone();
            state
                .egui_renderer
                .callback_resources_mut()
                .get_mut::<crate::volume::bridge::VolumeResources>()
                .map(|resources| {
                    (
                        resources.ensure_mirror(&device, plan.size_in_pixels, format),
                        plan,
                    )
                })
        };
        // Next frame's tiles, from the rung this frame's mirror was actually
        // sized to — the ordering `MirrorRungs::tile_zoom_bias` documents. A
        // rung with no matching tile bias buys interpolation rather than
        // detail, and a bias with no rung buys four times the fetches for
        // nothing, so the two are set from the same plan or not at all.
        self.gui
            .set_floor_tile_zoom_bias(self.mirror_rungs.tile_zoom_bias());
        let mirror =
            mirror_target
                .as_ref()
                .map(|(view, plan)| crate::egui_renderer::MirrorRequest {
                    view,
                    size_in_pixels: plan.size_in_pixels,
                    pixels_per_point: plan.pixels_per_point,
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
                    let volume_on_screen =
                        self.gui.panes().iter().any(|pane| {
                            pane.render_view() == rustdar_radar::types::RenderView::Volume
                        });
                    if volume_on_screen {
                        let losses = crate::volume::degrade::note_surface_loss_with_volume();
                        log::warn!(
                            "wgpu surface lost with a 3D volume on screen ({losses} so far)"
                        );
                    }

                    // A lost surface is the one signal a browser gives that a
                    // GPU allocation was too large — WebGL2 answers exhaustion
                    // by destroying the context, not by failing a call — and it
                    // is the only memory evidence any target here produces at
                    // all. Counted against the loop pool whether or not a
                    // volume was on screen, because the loops are the largest
                    // thing this application allocates. The pool lives on `App`
                    // precisely so it survives the `self.state = None` below,
                    // and so does the profile the rest of the budgets are
                    // resolved from — `install_volume_bridge` re-resolves off
                    // it when the surface comes back, so a rung surrendered
                    // here is a rung the recovered renderer is built at.
                    self.back_off_budgets();

                    // Surface is irrecoverably lost (e.g. display changed on a
                    // foldable). Drop the entire rendering state so the next
                    // handle_redraw() lazily recreates it with a fresh surface.
                    // Keep cached_render so the radar image can be restored
                    // instantly.
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
        let allocation = self.loop_allocation();
        let budgets = self.budgets;
        while let Ok(resp) = self.channels.loop_scan_list_receiver.try_recv() {
            let Some(pane) = self.gui.pane_mut(resp.pane_idx) else {
                continue;
            };
            // Whether this listing is still wanted, and what it makes of the frame
            // list, is decided in one place — including refusing a listing for a
            // site the pane's loop has since moved off.
            let product = pane.selected_product;
            let Some(plan) = accept_scan_listing(
                allocation,
                &budgets,
                &mut pane.loop_state,
                &resp.site,
                resp.scans,
            ) else {
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
    /// Broadcasts rendered textures from a layer-linked origin to the
    /// layer-linked sibling panes that need the same frame (matching
    /// product+elevation+timestamp).
    fn poll_loop_render_results(&mut self, ctx: &egui::Context) {
        while let Ok(mut rr) = self.channels.loop_render_receiver.try_recv() {
            let origin_pane = rr.pane_idx;
            // Resolved before the pane is borrowed, and off the *response*
            // rather than off the pane — see `frame_gates`.
            let gates = frame_gates(&self.loop_mgr, &rr);

            let Some(pane) = self.gui.pane_mut(origin_pane) else {
                continue;
            };

            // Vetting the result, retiring a failed render and placing the image are
            // one step over one resolved frame — see `accept_render_result`. The
            // texture is uploaded from inside it, so a result this pane has
            // retargeted away from costs no GPU memory.
            let counter = &mut self.texture_counter;
            let Some(texture) =
                accept_render_result(&mut pane.loop_state, &mut rr, gates, |color_image| {
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
            //
            // Texture sharing happens inside the layer-linked group (M11):
            // a linked origin donates to linked siblings, an unlinked pane
            // neither donates nor receives — the same two-ended gate as
            // `propagate_layer_sync`, so the render pipeline and the state
            // convergence describe one group.
            if self.gui.pane_layer_linked(origin_pane) {
                for sibling_idx in 0..self.gui.pane_count() {
                    if sibling_idx == origin_pane
                        || self.gui.pane_has_no_plan_view(sibling_idx)
                        || !self.gui.pane_layer_linked(sibling_idx)
                    {
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
        let allocation = self.loop_allocation();
        let budgets = self.budgets;
        let mut abandoned = Vec::new();
        for pidx in 0..self.gui.pane_count() {
            let loop_mgr = &self.loop_mgr;
            let Some(p) = self.gui.pane_mut(pidx) else {
                continue;
            };
            let budget = loop_render_budget(allocation, &p.loop_state, &budgets);
            if settle_loop_phase(loop_mgr, pidx, &mut p.loop_state, budget) {
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
    /// immediately, and an unlinked not-ready pane holds nobody back.
    ///
    /// # Why a pane that cannot loop is not merely skipped but must be
    ///
    /// The sync rule below is "hold every looping pane until all of them are
    /// ready", and a pane whose frames nothing renders can never become ready —
    /// `dispatch_loop_renders` neither fills its frames nor marks them failed. So
    /// one such pane in `not_ready_panes`, with Sync Layers on, stops **every
    /// other looping pane's** loop from ever starting. The symptom is in the other panes, which
    /// is what makes it the worst of these: a deadlock introduced by the very
    /// filter that protects the render path.
    ///
    /// `PaneState::set_kind` clears a converted pane's loop, so the state should
    /// be unreachable. This is here anyway, because the cost of being wrong is
    /// every loop on screen rather than one pane's, and because the field is
    /// public. Pinned by
    /// `a_pane_that_cannot_loop_cannot_hold_another_panes_loop_back`, whose
    /// blocked pane is an unaimed cross-section rather than a plan-view-less
    /// one — the property is the same, the pane kind that shows it changed.
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
                    if ls.frames[candidate].image.is_some() {
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
    /// Both loops below skip panes that cannot loop
    /// ([`Gui::pane_cannot_loop`](rustdar_egui::Gui::pane_cannot_loop)) — today
    /// the 3D volume, whose picture is raymarched from the eye and so cannot be
    /// cached per frame. There is nothing to dispatch for such a pane and
    /// nothing to clone into one, and the first loop's replan would otherwise
    /// start a download queue for a pane nobody is drawing. `loop_sync_targets`
    /// keeps it out of the enable action in the first place; this is the other
    /// half, for the pane that was converted while its loop was already running.
    ///
    /// The predicate is deliberately **not** `pane_has_no_plan_view`, which is
    /// what it was while a loop frame could only be a plan-view tilt. A
    /// cross-section pane has no plan view *and* loops, and the second pass
    /// below branches on the loop's own
    /// [`view`](rustdar_egui::pane::LoopPlaybackState::view) to decide which
    /// kind of picture each of its frames wants.
    ///
    /// The first pass also finishes the teardown `PaneState::set_kind` starts.
    /// That setter clears a converted pane's `loop_state`, which is the half a
    /// pane can do for itself; the other half is this pane's queue inside
    /// `LoopDownloadManager`, which is keyed by index and which a `PaneState`
    /// cannot reach. Doing it here rather than at the conversion covers every
    /// route to a non-map pane — the menu, a restored config, a later auto-create
    /// — and it is idempotent, so running it once a frame costs a hash lookup.
    /// What the panes are asking the loop pool for, this frame.
    ///
    /// **Counted in loops, not panes.** The two raster kinds never share a
    /// cached frame between panes, so each looping pane is one loop. A 3D loop's
    /// frames are resident grids in one application-wide `VolumeStore` keyed by
    /// target, so two 3D panes on the same site, product and region are one
    /// resident set, one build and one upload — and therefore **one** loop with
    /// **one** share. Charging that twice is the double-count
    /// `the_3d_set_is_not_double_counted_across_two_panes` exists to catch, and
    /// it would under-serve the one loop kind that cannot re-render its way out
    /// of being short.
    ///
    /// The key deliberately mirrors `VolumeTarget`'s own equality minus the
    /// per-frame timestamp: site, product and the loop's `VolumeLoopKey`, which
    /// is the region and the storm motion. Two panes at different zoom levels
    /// produce different regions and so are correctly two sets.
    ///
    /// Safe to read `pane` here despite `render_view`'s `mem::take` caveat: every
    /// caller runs outside the egui pass.
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
            let ls = &pane.loop_state;
            if !ls.is_active() {
                continue;
            }
            let already = if ls.view == rustdar_radar::types::RenderView::Volume {
                let Some(product) = loop_product(ls) else {
                    continue;
                };
                let key = (ls.site.clone(), product, ls.volume_key().cloned());
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
    ///
    /// Folded once per frame from `update_loop_state`, and read — never
    /// recomputed — everywhere else, so that the dispatcher, the texture
    /// eviction and the readiness check cannot disagree about how many frames a
    /// loop is entitled to. That is `render_set_indices`' invariant, one level
    /// up.
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
    ///
    /// The behavioural half of the sizing, and on every target that can report
    /// nothing it is the *only* half. Written to the config store at the moment
    /// of the decision rather than left to `autosave_config`'s 3 s timer,
    /// because a session that has just lost its rendering surface may not get
    /// three more seconds — and because a browser's answer to GPU memory
    /// exhaustion is exactly this event.
    ///
    /// # Two halves, because the two resources step differently
    ///
    /// * **The pool halves.** A continuous quantity divided among the loops
    ///   that want one, so halving is a real intermediate answer and a machine
    ///   one step too ambitious does not lose its whole loop over one event.
    ///   This is `LoopPool::back_off`, unchanged, and it is the ladder's loop
    ///   history rung — which is why `budget::demote` deliberately does not
    ///   own one.
    /// * **Everything else takes one rung.** `budget::demote` walks the
    ///   ordered ladder: lighting, then the offscreen, then the grid, then the
    ///   raster side. Discrete knobs with named stops, so there is no halving
    ///   to do — there is a next rung or there is the floor.
    ///
    /// # The count is a ladder position, not a failure count
    ///
    /// It stops rising the moment nothing moves, so a device that keeps losing
    /// its surface at the floor writes the same number rather than an
    /// ever-growing one. Below the floor the answer is not a smaller budget:
    /// `volume::degrade` retires the 3D view after two such losses, which is a
    /// different mechanism and the real bottom of this ladder.
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
            crate::loop_pool::remember(self.platform.config_store().as_deref(), bytes);
        }

        let memo = self
            .device_profile
            .memo
            .get_or_insert_with(Default::default);
        let stepped = memo.steps_back.saturating_add(1);
        memo.steps_back = stepped;
        let resolved = crate::budget::resolve(&self.device_profile);
        // Compared with the count itself held equal, because the count is a
        // field of what is being compared: `steps_back` always differs after an
        // increment, and what is being asked is whether *the budgets* moved.
        let same_but_for_the_count = crate::budget::Budgets {
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
        crate::budget::remember_steps(self.platform.config_store().as_deref(), stepped);
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
        // the seamless-swap rule that keeps the old grid through a rebuild is
        // right for one grid and is a peak of two full sets for fourteen.
        // Collected rather than acted on inline, because the store is borrowed
        // from `self` while the pane is.
        let mut release_volume_sets: Vec<usize> = Vec::new();
        // Panes whose loop is no longer active and whose download queue is
        // therefore serving nobody. Collected for the same borrow reason.
        let mut retire_queues: Vec<usize> = Vec::new();
        // Read once, outside the pane loop, so every section loop retargeted in
        // this pass is keyed to the same vector the cuts it is about to dispatch
        // will be derived with — the rule `SectionInputKey::of` states, applied
        // across panes instead of within one dispatch.
        let motion_override = self.render.storm_motion_override_kt();
        for pane_idx in 0..self.gui.pane_count() {
            if self.gui.pane_cannot_loop(pane_idx) {
                // The host-side half of the loop teardown. Without it the pane's
                // queue outlives its loop and goes on spending the *shared*
                // download budget on volumes nobody will draw, starving the live
                // panes beside it.
                self.loop_mgr.remove_pending(pane_idx);
                continue;
            }
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            let product = pane.selected_product;
            let elevation = pane.selected_elevation;
            // The section half of the key, for a section loop: the line the
            // frames are cut along and the vector they are derived with. `None`
            // for a plan-view loop, and `None` for a section pane that has lost
            // its line — which counts as a change and discards the frames, the
            // safe direction.
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
                    // Reading it here is what keeps a loop's frames resampled
                    // over the same ground the live pane is showing, and reading
                    // the *stored* field rather than a per-frame measurement is
                    // what stops a gesture rekeying the loop: a zoom now moves
                    // the eye, so fourteen grids are no longer thrown away for
                    // one scroll.
                    v.region,
                    (product == rustdar_radar::types::RadarProduct::StormRelativeVelocity)
                        .then_some(motion_override)
                        .flatten(),
                    self.render.srv_fallback(),
                )
            });
            let ls = &mut pane.loop_state;
            if !ls.is_active() {
                // The host-side teardown, and the reason it is here rather
                // than only in the branch above: with every pane kind able to
                // loop, "cannot loop" no longer covers the pane whose loop was
                // *torn down* — `PaneState::set_kind` clears `loop_state` on
                // any kind change and cannot reach `loop_mgr`, which is keyed
                // by pane index. Left behind, that queue goes on spending the
                // shared download budget on volumes nobody will draw.
                //
                // Also correct for a loop simply switched off, which is what
                // makes it a property of the state rather than of the route
                // that reached it.
                retire_queues.push(pane_idx);
                continue;
            }
            if ls.frames.is_empty() {
                continue;
            }

            // One key per loop kind, and the kind decides which. `None` for a
            // plan-view loop; for the other two, `None` also stands for "this
            // pane has lost the thing its frames were pictures of", which
            // counts as a change and discards them — the safe direction.
            let view_key = match ls.view {
                rustdar_radar::types::RenderView::CrossSection => {
                    section_key.map(rustdar_egui::pane::LoopViewKey::Section)
                }
                rustdar_radar::types::RenderView::Volume => {
                    volume_key.map(rustdar_egui::pane::LoopViewKey::Volume)
                }
                rustdar_radar::types::RenderView::PlanView => None,
            };

            // The pane's product/elevation combo boxes write straight through, so
            // pick the change up here: every texture depicts the old product and
            // every render_failed flag judged the old product. Invalidating leaves
            // nothing to evict. The section half moves for the same kind of
            // reason and is discarded by the same call — a redrawn line or an
            // edited storm motion vector makes every frame a picture of
            // something else.
            if ls.retarget_renders_keyed(product, elevation, view_key) {
                // The 3D loop's frames are not textures to be dropped but
                // grids in a shared store, and the retarget has just cleared
                // the only record of which ones this pane was holding. Release
                // them here, *before* the pass below builds anything for the
                // new key: `retain_set(&[])`, then rebuild. See
                // `VolumeStore::retain_set` for the peak that avoids.
                if ls.view == rustdar_radar::types::RenderView::Volume {
                    release_volume_sets.push(pane_idx);
                }
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
            ls.evict_textures_outside_render_set(loop_render_budget(allocation, ls, &budgets));
        }
        for pane_idx in retire_queues {
            self.loop_mgr.remove_pending(pane_idx);
            // A torn-down 3D loop's grids go with its queue. Without this the
            // resident set outlives the loop that asked for it, and 512 MiB
            // stays allocated for a pane that is showing a live volume.
            //
            // Asked before it is done, because the answer is also what says
            // whether this pane's `rendered_for` is a lie. While a 3D loop
            // runs, the pane paints the playhead's frame and stops asking for
            // the live volume, so `rendered_for` freezes at whatever it named
            // when the loop started — and the grid it names has just been let
            // go of. Left alone, the level-triggered `PrepareVolume` would
            // never fire again and the pane would read "Building…" for ever.
            // Clearing it is only correct *here*, for a pane that really was a
            // set holder: doing it unconditionally would clear a live 3D
            // pane's key every frame and rebuild its grid every frame with it.
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
        // release, then build.
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
        // Recorded so they stop being retried and stop holding up readiness.
        let mut to_mark_failed: Vec<(usize, usize)> = Vec::new();

        let pane_count = self.gui.pane_count();

        // Cross-section cuts to dispatch, and the running count that paces them.
        // The cap is across *panes*, not per pane, because what it protects is
        // the frame thread — see `MAX_LOOP_SECTION_CUTS_PER_FRAME`.
        let mut to_cut: Vec<LoopSectionRequest> = Vec::new();

        // Voxel grids to make resident. Planned for every frame of every 3D
        // loop, not just the ones a build has to be dispatched for: most
        // passes find the grid already in the store and only have to name it
        // on the frame, which costs a lookup. See the pacing below, which
        // counts *dispatches* rather than entries here.
        let mut to_build: Vec<LoopVolumeRequest> = Vec::new();

        for pane_idx in 0..pane_count {
            if self.gui.pane_cannot_loop(pane_idx) {
                continue;
            }
            // Texture sharing — donor clones below, and the queued-render
            // dedup that leans on the response-path broadcast — happens
            // inside the layer-linked group (M11), the same two-ended gate
            // the broadcast itself applies: an unlinked pane renders its own
            // frames and nobody counts on serving it.
            let linked = self.gui.pane_layer_linked(pane_idx);
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
            //
            // A 3D loop's budget is its own, and it is the *whole* frame list
            // rather than a window inside it: a resident grid re-entered costs
            // ~140 ms to rebuild against a 200 ms playback interval, so the
            // walking window the other two kinds use does not close here. The
            // count is `LoopAllocation::volume_frames`, which is what
            // `frames_for` answers on this view.
            let indices = ls.render_set_indices(loop_render_budget(allocation, ls, &budgets));

            // A 3D loop's frames are resident grids rather than rasters, so it
            // plans separately and against a different budget. Same branch
            // discipline as the section arm below: on the *loop's* view, not
            // on the pane's kind.
            if ls.view == rustdar_radar::types::RenderView::Volume {
                let Some(key) = ls.volume_key().cloned() else {
                    // Unreachable while the pane is a volume pane — the pass
                    // above always builds a key for one — and the honest
                    // answer if a future caller reaches it is the section
                    // arm's: retire the frames rather than sit in `Rendering`
                    // for the session.
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
                        product: target.product,
                        region: key.region,
                    };
                    // **Every frame is planned, every pass, whatever state it
                    // is in** — including one already resident and already
                    // named. The plan is not a work list, it is the statement
                    // `retain_set` makes below, and that statement detaches
                    // the holder from everything it does not name. A frame
                    // dropped from the plan for having landed is therefore a
                    // frame whose grid is handed back on the very next pass,
                    // and the set is eaten from the front as fast as it is
                    // built: what survives is the last grid stated, which
                    // `lookup_for_pane`'s same-scope fallback then paints
                    // under every other frame's caption. That is the "the loop
                    // sort of plays and then shows the current time" report,
                    // and the skip that caused it read perfectly reasonably as
                    // an optimisation.
                    //
                    // The cost of planning a landed frame is a `share_held`
                    // probe and a `lookup` — see `make_volume_frames_resident`,
                    // whose pacing counts *dispatches* precisely so that the
                    // naming half can run for every frame of every pass.
                    to_build.push(LoopVolumeRequest {
                        pane_idx,
                        frame_idx: idx,
                        target: volume_target,
                        // A retired frame is still planned for the same
                        // reason, so that the store goes on holding the
                        // refusal it was retired by. What it must never do is
                        // buy another extraction: the answer is a property of
                        // the volume, so retrying is a walk that fails
                        // identically for ever.
                        retired: frame.render_failed,
                    });
                }
                continue;
            }

            // A section loop wants a different picture per frame and identifies
            // it with different things, so it plans separately. The branch is on
            // the *loop's* view rather than on the pane's kind because that is
            // the value every acceptance test downstream compares against, and
            // two ways of asking one question is how they come to disagree.
            if ls.view == rustdar_radar::types::RenderView::CrossSection {
                let Some(key) = ls.section_key().cloned() else {
                    // A section loop with no line has nothing to cut and its
                    // volumes download perfectly well, so nothing would ever
                    // settle the batch and the loop would sit in `Rendering` for
                    // the session. `PaneState::can_loop` keeps this unreachable
                    // by refusing to start such a loop at all; retiring the
                    // frames here is the second line, and it routes into the
                    // existing "no frame can be rendered" path that switches the
                    // loop off with a warning.
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
                    // A frame already cut from *this* ladder is done. One cut
                    // from a different one is stale and re-cut: the newest
                    // frame's volume is re-cached under the same
                    // `(site, timestamp)` key as more of it seals, so a section
                    // cut from a two-rung ladder can otherwise stand at the head
                    // of a loop while the real volume grows to fourteen. This is
                    // the same fingerprint, and the same reasoning, as
                    // `SectionTarget::ladder` on the live pane — reused rather
                    // than a second notion of section staleness.
                    if frame
                        .image
                        .as_ref()
                        .and_then(rustdar_egui::pane::LoopFrameImage::section)
                        .is_some_and(|cut| cut.ladder == ladder)
                    {
                        continue;
                    }

                    // Take a sibling's raster instead of cutting, on the same
                    // terms the plan-view path donates on: same target, same key,
                    // and — standing in for the snapped sweep a section has no
                    // equivalent of — the same ladder. Including about the
                    // group: donors come from the layer-linked panes, for a
                    // layer-linked receiver.
                    if linked
                        && let Some((src_pane, src_frame)) = find_section_donor(
                            (0..pane_count)
                                .filter(|&i| self.gui.pane_layer_linked(i))
                                .filter_map(|i| self.gui.pane(i).map(|p| (i, &p.loop_state))),
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

                // Take a sibling's texture instead of rendering, but only from a loop
                // keyed to the same target. Same test the response-path broadcast
                // applies, so the two cannot disagree about who may serve this frame
                // — including about the group: donors come from the layer-linked
                // panes, for a layer-linked receiver.
                if linked {
                    let donor = find_donor(
                        (0..pane_count)
                            .filter(|&i| self.gui.pane_layer_linked(i))
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
                        // Deduplicate: if another *linked* pane already queued a
                        // render for the same target and timestamp, skip — the
                        // broadcast in poll_loop_render_results will deliver the
                        // texture to this pane. The queuing pane must be linked
                        // too, or the broadcast this lean relies on never runs.
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
                let Some(image) = sframe.image.clone() else {
                    continue;
                };
                image
            };
            let Some(dest) = self.gui.pane_mut(req.dest_pane) else {
                continue;
            };
            if let Some(dframe) = dest.loop_state.frames.get_mut(req.dest_frame) {
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

        // Cut the queued sections. Same three rules as the loop above: the
        // budget is re-read before every spawn because it is shared with static
        // renders, the frame is marked in flight only if a job was actually
        // started, and missing data is a skipped frame the next pass retries.
        for req in to_cut {
            if self.render.renders_in_flight.load(Ordering::Relaxed)
                >= self.render.concurrent_renders()
            {
                break;
            }
            let Some(LoopFrameData::Volume(scan, declared)) =
                frame_data(&self.loop_mgr, &req.target, req.timestamp)
            else {
                // A section is cut from a volume. A loop whose product reads
                // Level III objects instead reaches here with `Products`, and
                // there is no vertical structure in one to slice — the frame is
                // retired, and `settle_loop_phase` switches a loop whose frames
                // are all retired off.
                if let Some(pane) = self.gui.pane_mut(req.pane_idx)
                    && let Some(frame) = pane.loop_state.frames.get_mut(req.frame_idx)
                {
                    frame.render_failed = true;
                }
                continue;
            };
            let (pane_idx, frame_idx) = (req.pane_idx, req.frame_idx);
            match self.spawn_loop_section_render(req, scan, declared) {
                crate::render_dispatch::SectionDispatch::Dispatched => {
                    if let Some(pane) = self.gui.pane_mut(pane_idx)
                        && let Some(frame) = pane.loop_state.frames.get_mut(frame_idx)
                    {
                        frame.render_in_flight = true;
                    }
                }
                // Nothing was taken and nothing is wrong: ask again next frame.
                crate::render_dispatch::SectionDispatch::Busy => {}
                // This volume carries no field to cut under this product. Retire
                // the frame so the dispatcher stops retrying it and readiness
                // stops waiting on it — the same answer `FrameSweep::Unrenderable`
                // gets on the plan-view path.
                crate::render_dispatch::SectionDispatch::NoPayload => {
                    if let Some(pane) = self.gui.pane_mut(pane_idx)
                        && let Some(frame) = pane.loop_state.frames.get_mut(frame_idx)
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
    ///
    /// Split out of [`Self::dispatch_loop_renders`], which is long enough, and
    /// because this is the half with the two rules worth reading together:
    ///
    /// * **The pacing counts dispatches, not frames.** A pass over a settled
    ///   fourteen-frame loop finds every grid already in the store and spends
    ///   fourteen hash-free linear lookups; only a *miss* costs the
    ///   `extract_volume_parts` walk on the frame thread, and at most
    ///   [`MAX_LOOP_VOLUME_BUILDS_PER_FRAME`] of those are paid per frame. The
    ///   cheap `share_held` probe ahead of the budget check is what separates
    ///   the two, and without it the cap would stop the *naming* as well and a
    ///   loop would take a frame per frame to notice grids it already had.
    /// * **The resident set is stated, not inferred.** After the pass, every
    ///   3D loop tells the store the whole list it holds — which drops the
    ///   grids of frames that have scrolled out of the loop, and the pane's own
    ///   live grid on the frame its loop takes over. That is
    ///   [`crate::volume::bridge::VolumeStore::retain_set`], and it is what
    ///   makes "the frame list is the resident set" a property rather than a
    ///   hope.
    ///
    /// The two rules meet at one obligation on the *caller*: `to_build` must be
    /// the loop's **whole** frame list on every pass, not the frames that still
    /// need work. `retain_set` detaches the holder from everything the list
    /// does not name, so a frame left out of the plan for having already landed
    /// is a frame whose grid is handed straight back — the set eaten from the
    /// front as fast as it is built, which is what the loop-that-snaps-back
    /// report turned out to be. The pacing above is what makes that affordable:
    /// planning a landed frame costs a probe and a lookup, never an extraction.
    fn make_volume_frames_resident(&mut self, to_build: Vec<LoopVolumeRequest>) {
        use crate::volume::bridge::{Hold, VolumeEntry};

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
                    // Ruled out, and the store no longer remembers why. Left
                    // alone rather than re-extracted: every reason a build is
                    // refused is a property of the volume, so a retry is a
                    // multi-millisecond walk that fails identically for ever.
                    continue;
                }
                if dispatched >= MAX_LOOP_VOLUME_BUILDS_PER_FRAME {
                    // Out of frame-thread budget for this pass. Left alone,
                    // not retired: the next pass asks again, and the pane goes
                    // on marching whatever has already landed.
                    continue;
                }
                match self.prepare_volume(req.pane_idx, &req.target, Hold::Set) {
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
            let Some(frame) = pane.loop_state.frames.get_mut(req.frame_idx) else {
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
                // Terminal for this frame's scan: this volume carries no field
                // to resample under this product, or the site is unknown. The
                // same answer `FrameSweep::Unrenderable` gets on the plan-view
                // path, and it is what lets readiness stop waiting.
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
    ///
    /// The section counterpart of [`Self::poll_loop_render_results`], with the
    /// same two steps in the same order: vet-and-place through
    /// [`accept_section_result`], then — inside the layer-linked group — offer
    /// the finished raster to every sibling section loop cut for the same
    /// thing. The same two-ended gate as the plan-view broadcast (M11): a
    /// linked origin donates to linked siblings, an unlinked pane neither
    /// donates nor receives.
    fn poll_loop_section_results(&mut self, ctx: &egui::Context) {
        while let Ok(mut sr) = self.channels.loop_section_receiver.try_recv() {
            let origin_pane = sr.pane_idx;
            let Some(pane) = self.gui.pane_mut(origin_pane) else {
                continue;
            };

            let counter = &mut self.texture_counter;
            let Some(placed) =
                accept_section_result(&mut pane.loop_state, &mut sr, |color_image| {
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
                // The receiver's own half of the ladder comparison, resolved from
                // the receiver's own scan and never filled in from the reply —
                // the same discipline `own_sweep` enforces on the plan-view side,
                // where taking the sender's value would compare it to itself.
                let own_ladder = match frame_section(&self.loop_mgr, &sr.target, sr.timestamp) {
                    FrameSection::At(ladder) => Some(ladder),
                    FrameSection::Unrenderable | FrameSection::Pending => None,
                };
                let Some(sibling) = self.gui.pane_mut(sibling_idx) else {
                    continue;
                };
                let Some(sframe) = sibling.loop_state.frame_accepting_section_broadcast_mut(
                    sr.timestamp,
                    &sr.target,
                    &sr.key,
                    sr.ladder,
                    own_ladder,
                ) else {
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
///
/// A pure function of the two holders so the decision is testable without a
/// live chunk feed. The distinctions between its three answers are the
/// load-bearing part, and all three are mid-flight or cold-start states that
/// clear themselves — each needs its own sentence:
///
/// * an overlay carrying sealed sweeps but **no pattern** is the mid-flight
///   join before the VCP message: `chunks.rs` stands in an empty coverage
///   pattern until it lands, and `current::resolve` correctly refuses to key a
///   flight by another flight's table;
/// * an overlay carrying a **pattern with nothing sealed yet** is the same join
///   one step later, and it is the one that used to have no name at all — see
///   below;
/// * nothing at all is the cold-start download.
///
/// # The merge that resolves and is still empty
///
/// `current::resolve` answers "can these two volumes be keyed onto one ladder",
/// not "is there anything on it". A feed that joins after a volume start but
/// before the first seal, with no archive base yet, gives it a pattern it can
/// key and zero sweeps to key — so it succeeds, returning a merged volume with
/// an empty sweep list, and this function used to read that as "a section can
/// be cut".
///
/// Nothing downstream could recover. `ladder_fingerprint` answers `None` over
/// no sweeps so the staleness key was `0`; the extraction found no moment and
/// `spawn_section_render` returned [`SectionDispatch::NoPayload`], which named
/// the state `ProductMissingFromVolume` — *"this volume carries no Reflectivity
/// to cut"*. That is a true sentence about the wrong thing: the volume carries
/// no anything, this has nothing to do with the product, and switching moment
/// (which is what the message invites) changes nothing. Meanwhile the dispatch
/// logged "no volume payload" once per key, and the pane stood blank for the
/// rest of the volume's first tilt — up to ~30 s.
///
/// Refusing here fixes both halves at once, and that is why it is here rather
/// than in the dispatch's `NoPayload` arm: the pane gets a sentence about
/// *waiting*, and the render path is never entered, so there is nothing left to
/// log. The key is deliberately not written by the caller for any of these
/// three, because all three are answered by the next thing that arrives.
///
/// [`SectionDispatch::NoPayload`]: crate::render_dispatch::SectionDispatch::NoPayload
fn section_source_refusal(
    base: Option<&nexrad_model::data::Scan>,
    overlay: Option<&nexrad_model::data::Scan>,
) -> Option<rustdar_egui::pane::SectionUnavailable> {
    // The declared Nyquist tables are irrelevant to *whether* a merge resolves
    // — the admission rule reads cut angles and elevation numbers only — so
    // this asks the question with empty ones rather than threading tables into
    // a pure predicate about coverage.
    if let Some(current) =
        rustdar_radar::current::resolve(base.map(Into::into), overlay.map(Into::into))
    {
        // Asked of the **merged** sweep list rather than of either source's,
        // because that list is what the cut would be taken from: a base whose
        // every sweep the admission rule dropped on a VCP change resolves to
        // exactly the same nothing as a flight that has sealed nothing yet, and
        // the pane owes the same sentence for both.
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
    allocation: LoopAllocation,
    budgets: &crate::budget::Budgets,
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

    // The site's own cadence, read off the listing *before* the sampling below
    // throws scans away. Once sampled there is no way back to it, and it is what
    // the timeline caption needs to tell "every scan" from "one in five".
    ls.scan_step_secs = median_step_secs(
        &scans
            .iter()
            .map(|(timestamp, _id)| *timestamp)
            .collect::<Vec<_>>(),
    );

    // Cap the downloads by evenly sampling the listing. A 3D loop's cap is its
    // *resident* one and is far lower, because for that kind the frame list and
    // the resident set are one thing — see `loop_frames_held`.
    //
    // The kept/given ratio is the loop's own answer to "am I showing every
    // scan", and it is recorded here because here is the only place it exists:
    // one line further down the listing is gone and nothing left behind can
    // reconstruct it. See `LoopPlaybackState::listing_sampled`.
    let held = loop_frames_held(allocation, ls, budgets);
    let total = scans.len();
    let sample = rustdar_egui::pane::listing_sample_indices(total, held);
    ls.listing_sampled = Some(sample.is_some());
    let scans = match sample {
        Some(indices) => {
            log::info!("Loop: sampled {total} down to {held} frames for {site}");
            indices.into_iter().map(|i| scans[i].clone()).collect()
        }
        None => scans,
    };

    ls.phase = rustdar_egui::pane::LoopPhase::Rendering;
    // Oldest-first, matching the scan listing order.
    ls.frames = scans
        .iter()
        .map(|(ts, _id)| rustdar_egui::pane::LoopFrame {
            timestamp: *ts,
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    if !ls.frames.is_empty() {
        ls.current_frame = ls.frames.len() - 1; // start at newest
    }

    Some(FramePlan::new(site.to_string(), scans))
}

/// The median gap between consecutive scan times, in whole seconds.
///
/// `None` for a run too short to have a gap. Zero and negative gaps are
/// dropped rather than clamped: a listing is oldest-first and strictly
/// increasing, so a non-positive gap is a duplicate key or an out-of-order one,
/// and averaging it in would pull the cadence toward a value no radar ran at.
///
/// Median rather than mean because a window can straddle a VCP change — a real
/// event, not a corner case: across the six TDWR and four WSR-88D sites measured
/// for 2026-08-11, every site but TDFW alternated VCPs during the day. A window
/// holding a 259 s run and a 517 s run has no meaningful mean.
///
/// Takes bare timestamps rather than a listing because there are two runs it has
/// to answer for and they are not the same type: the listing
/// `accept_scan_listing` was handed, and the frame list `append_polled_frame`
/// re-measures as a loop that holds every scan follows the site forward. One
/// median for both, so the figure a loop starts with and the figure it keeps
/// cannot be two different statistics.
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
    if ls.frames.iter().any(|f| f.image.is_some()) {
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
    gates: Option<rustdar_radar::hover::SweepGates>,
) -> rustdar_egui::pane::RadarImageData {
    rustdar_egui::pane::RadarImageData {
        texture: texture.clone(),
        lat: rr.site_lat,
        lon: rr.site_lon,
        max_range_km: rr.max_range_km,
        nyquist_ms: rr.nyquist_ms,
        melting_layer_source: rr.melting_layer_source,
        storm_motion: rr.storm_motion,
        // **This is what makes a hover work under a loop.** The field carries
        // this frame's wedges and no numbers — `deliver` stripped them — and
        // `gates` is an `Arc` on the volume it was drawn from, which the loop's
        // download cache is holding anyway. The clone is 5.8 KiB of geometry
        // per receiving pane, not 5.03 MiB of values.
        //
        // A `None` here is a frame whose volume has gone or whose product is
        // computed rather than measured, and the readout says so rather than
        // going blank — see `rustdar_radar::hover::Reading::NotResident`.
        hover: Arc::new(rustdar_radar::hover::HoverSource::from_volume(
            rr.polar.clone(),
            gates,
        )),
    }
}

/// The sweep a finished loop render was drawn from, for reading its numbers
/// back out.
///
/// Keyed on the *render's* site and timestamp rather than on any pane's, for
/// the reason [`crate::channels::LoopRenderResponse::site_lat`] gives: a
/// sibling pane takes this image through the broadcast, and a receiver that
/// looked the volume up under its own loop's site would be answering for a
/// picture it did not draw.
///
/// The elevation is `snapped` and not `target.elevation` — the angle the image
/// actually depicts, which is what the renderer resolved a sweep at. A frame
/// read at the *selected* angle would be reading a different cut from the one
/// on the glass wherever a scan's tilts do not line up with the selection, and
/// a loop steps through scans that do not agree about that.
fn frame_gates(
    loop_mgr: &crate::loop_downloads::LoopDownloadManager,
    rr: &crate::channels::LoopRenderResponse,
) -> Option<rustdar_radar::hover::SweepGates> {
    let (scan, _) = loop_mgr.get_cached(&rr.target.site, &rr.timestamp)?;
    rustdar_radar::hover::SweepGates::new(Arc::clone(scan), rr.target.product, rr.snapped)
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
///
/// Same shape and same contract: one resolved frame, vetted and filled in one
/// step, with `upload` run only once both checks have passed so a refused reply
/// costs no GPU texture. The vet is
/// [`LoopPlaybackState::frame_awaiting_section_result_mut`], which tests the
/// loop's view as well as both halves of the key — a plan-view loop and a
/// section loop on one site, product and elevation produce `RenderTarget`s that
/// `matches` calls equal, and without the view test one would place the other's
/// raster.
///
/// `sr` is taken by `&mut` so the raster and the tilt list can be `take`n rather
/// than the whole reply moved, leaving `&sr` available to the broadcast below —
/// the same reason [`accept_render_result`] does it.
fn accept_section_result(
    ls: &mut rustdar_egui::pane::LoopPlaybackState,
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
    if let Some(volume) = resp.scan {
        loop_mgr.cache_scan(&resp.site, resp.timestamp, volume);
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
    let Some((scan, _)) = loop_mgr.get_cached(&target.site, &timestamp) else {
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

/// What one frame's own volume makes of a section loop's line.
///
/// The section counterpart of [`FrameSweep`], and it answers the same three
/// questions — renderable, gap, or waiting — because every caller downstream
/// needs exactly those three. What it carries in the renderable arm differs:
/// a plan view carries the sweep its scan snapped the selection to, and a
/// section, which cuts across every tilt, carries the fingerprint of the ladder
/// it would cut along.
enum FrameSection {
    /// The ladder fingerprint this frame would be cut from.
    At(u64),
    /// The volume is here and carries nothing to cut under this product.
    /// Terminal.
    Unrenderable,
    /// The volume has not arrived yet.
    Pending,
}

/// The ladder frame `timestamp` of a section loop keyed to `target` would be cut
/// from.
///
/// [`rustdar_radar::sampler::ladder_fingerprint`] over the frame's **own** cached
/// volume, which is the same function the live section pane's staleness key uses
/// over the merged current volume — one notion of "which cut is each rung made
/// of", asked of two different volumes. Walks sweep metadata only, so it costs
/// nothing to ask once per frame per dispatch pass.
///
/// `target.site` is where the loop's geometry came from and so the only site
/// whose data may be cut with it, exactly as in [`frame_data`].
fn frame_section(
    loop_mgr: &crate::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> FrameSection {
    let Some((scan, _)) = loop_mgr.get_cached(&target.site, &timestamp) else {
        return FrameSection::Pending;
    };
    let sweeps: Vec<&nexrad_model::data::Sweep> = scan.sweeps().iter().collect();
    match rustdar_radar::sampler::ladder_fingerprint(
        scan.coverage_pattern(),
        &sweeps,
        target.product,
    ) {
        Some(ladder) => FrameSection::At(ladder),
        None => FrameSection::Unrenderable,
    }
}

/// The allocation an idle application has: the whole pool at this target's
/// floor, undivided.
///
/// The tests below are written against the numbers this target ships, and the
/// pool reproduces them exactly for a single loop at the floor — that is the
/// continuity property `one_loop_at_the_floor_gets_the_whole_span_budget`
/// pins. So a test that is about dispatch rather than about division takes
/// this and reads unchanged; a test that is about division builds its own.
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
///
/// The same resolution `App::with_instance` performs, so a test and the
/// application it stands in for spend the same figures.
#[cfg(test)]
pub(crate) fn test_budgets() -> crate::budget::Budgets {
    crate::budget::resolve(&crate::budget::DeviceProfile::for_target())
}

/// Frames this loop may keep **textured**, which is the term that bounds memory.
///
/// Two bounds and both are real:
///
/// * `LoopAllocation::frames_for` — the loop pool's share, which is what a
///   *second* pane opening a loop takes back;
/// * `Budgets::frames_for_span` — `constants::LOOP_SPAN_BUDGET_SECS` converted
///   at this loop's own site cadence, which is what a *slower radar* takes
///   back. A TDWR volume is 360 s and a WSR-88D precip volume 259 s, so the
///   same two hours is 21 frames at one site and 28 at the other, and paying
///   the higher figure everywhere is how a loop came to mean three different
///   amounts of weather depending on which radar it was pointed at.
///
/// The pool's share is not itself cadence-aware and must not become so: it is
/// one application-wide plan over panes whose sites differ, and a per-site term
/// inside it would make one pane's radar decide another pane's frame count.
/// Taking the minimum here is where the two questions meet, once, per loop.
///
/// Both bounds are at or above `MIN_LOOP_FRAMES_PER_PANE`, so their minimum is
/// too — a loop that is not a loop is not reachable from here.
///
/// # The append path reads this one append behind, deliberately
///
/// `app_fetch::append_polled_frame` is handed `held` by its caller, then
/// re-measures `scan_step_secs` over the new frame list, then caps. So on the
/// append where a VCP change first moves the cadence, the cap applied is the
/// one the *previous* cadence bought, and the new figure binds on the next
/// append. That ordering is the append path's own and is right: it re-measures
/// off the full scan list rather than off a list already cut to a cap derived
/// from the stale figure.
///
/// It costs at most one frame, for at most one volume period, and only on a 3D
/// loop — the raster kinds' held count does not read the cadence at all. It
/// cannot cost memory in any case: the pool's share is the other half of the
/// minimum above and does not move, so a lagging cadence can only leave the
/// list a frame longer than the *span* wanted, never a frame longer than the
/// bytes allow.
fn loop_render_budget(
    allocation: LoopAllocation,
    ls: &rustdar_egui::pane::LoopPlaybackState,
    budgets: &crate::budget::Budgets,
) -> usize {
    allocation
        .frames_for(ls.view)
        .min(budgets.frames_for_span(ls.scan_step_secs))
}

/// Frames a loop of this view **holds**.
///
/// `Budgets::loop_frames_held` for the two raster kinds, which hold more than they
/// texture and re-render as the playhead walks — a held frame is scan data and
/// a timestamp, not a texture, so it is not what the loop pool is spent on and
/// it does not shrink when a pane arrives. It is deliberately **not** held to
/// the span budget either: the frame list is what the loop shows over the
/// user's whole lookback, and a lookback wider than the span budget is answered
/// by sampling, which `LoopPlaybackState::listing_sampled` records and the
/// caption reports. Capping the list at the span budget would throw away detail
/// that costs no textures — and it would do it silently, because the caption
/// reports the *sampler's* answer and a shorter list is not a sample.
///
/// A 3D loop's frames are resident grids and re-entering one costs ~140 ms
/// against a 200 ms playback interval, so its list *is* its resident set: both
/// are [`loop_render_budget`]'s answer, so a 3D loop's list follows the span
/// budget where a raster loop's does not — because for that kind the list is
/// the thing that costs memory.
///
/// Exhaustive, like every other classification by view in this workspace.
pub(super) fn loop_frames_held(
    allocation: LoopAllocation,
    ls: &rustdar_egui::pane::LoopPlaybackState,
    budgets: &crate::budget::Budgets,
) -> usize {
    match ls.view {
        rustdar_radar::types::RenderView::Volume => loop_render_budget(allocation, ls, budgets),
        rustdar_radar::types::RenderView::PlanView
        | rustdar_radar::types::RenderView::CrossSection => budgets.loop_frames_held,
    }
}

/// A 3D loop frame the dispatcher intends to make resident.
///
/// Deliberately smaller than [`LoopSectionRequest`], and that is the shape of
/// the feature: a section frame carries a raster keyed by a line, a ladder and
/// a pair of site coordinates, while a volume frame carries nothing but the
/// [`rustdar_egui::pane::VolumeTarget`] the grid is built from — which already
/// holds the site, the volume time, the product and the region. The frame index
/// is here because the answer has to be written back to a frame, and the pane
/// index because the store refcounts by holder.
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
///
/// Also what `App::spawn_loop_section_render` is handed, whole: every field is
/// part of one decision this dispatcher made, and passing them loose would put
/// two `f64`s and a `u64` next to each other in a call signature.
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
///
/// [`find_donor`] for sections. `target`, `key` and `wanted_ladder` are the
/// **receiver's**, for the reason that one gives: asking each candidate about
/// its own key would compare it to itself.
fn find_section_donor<'a>(
    loops: impl IntoIterator<Item = (usize, &'a rustdar_egui::pane::LoopPlaybackState)>,
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
///
/// [`render_already_queued`] for sections, and it weighs exactly what
/// [`LoopPlaybackState::frame_accepting_section_broadcast`] weighs — suppression
/// is a promise of acceptance, so a term in one and not the other is a frame
/// served by neither. The ladder is not a term because two loops sharing one
/// `(site, timestamp)` cache entry share one volume and so one ladder, which is
/// the same argument [`LoopPlaybackState::frame_donatable_to`] makes about the
/// snapped sweep.
fn section_already_queued<'a>(
    mut queued: impl Iterator<Item = &'a LoopSectionRequest>,
    timestamp: chrono::NaiveDateTime,
    target: &RenderTarget,
    key: &rustdar_egui::pane::SectionLoopKey,
) -> bool {
    queued.any(|r| r.timestamp == timestamp && r.target.matches(target) && &r.key == key)
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
fn render_already_queued<'a>(
    mut queued: impl Iterator<Item = &'a LoopRenderRequest>,
    timestamp: chrono::NaiveDateTime,
    target: &RenderTarget,
    snapped: f32,
) -> bool {
    queued.any(|r| {
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
/// Whether every dispatch path tells the renderer where the sweep it asked for
/// folds — the plan view, the section, and the loop.
#[path = "app_render/declared_nyquist_dispatch_tests.rs"]
#[cfg(test)]
mod declared_nyquist_dispatch_tests;

#[path = "app_render/frame_build_order_tests.rs"]
#[cfg(test)]
mod frame_build_order_tests;

/// Where the per-pixel unmultiply is allowed to run, and where it is not.
///
/// The binding rule about heavy work on the frame thread, checked at the places
/// that kept breaking it: a full-size `ColorImage::from_rgba_unmultiplied` is
/// 6.66 ms for a 2048² radar raster and ~47 ms for a desktop-sized overlay,
/// against a 16.7 ms budget, and both are ordinary per-volume events rather than
/// rare ones. The radar half of it has since stopped being a question of which
/// poller converts: `offload::execute` premultiplies inside the job, so there is
/// no unmultiply for a poller to take. The overlay half still converts in the
/// closure that drew it, because its producer is not a job.
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

/// What `poll_level3_results` does with a channel holding more than one answer.
///
/// Built on `stamping_tests`' fixtures: an `App` with one pane on a real radar,
/// and the smallest Level III object the pipeline will accept.
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
/// quietly buys a full-size plan-view image plus an equally large `f32` value
/// grid, uploads a texture, and hands it to a pane that draws none.
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

/// A pane keeps the picture it has until the next one is whole.
///
/// The app-level half of the hold: which raster is held and which goes up as it
/// arrives, what the second copy costs and when it is given back, what a
/// renderer rebuild does to a hold, and the two positional facts — the swap
/// happens before the frame is laid out, and a hold keeps the loop awake — that
/// nothing has a type to carry.
#[path = "app_render/raster_hold_tests.rs"]
#[cfg(test)]
mod raster_hold_tests;

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

/// One sweep is one *render*, however many panes are looking at it — the
/// sibling of `radar_texture_sharing_tests`, one step earlier in the same path.
///
/// That module counts uploads; this one counts the jobs that produce the buffer
/// being uploaded. `dispatch_pane_renders` walks every pane in one pass and the
/// render cache is only written on the way back, so on the frame a volume lands
/// the cache misses for all of them at once — and each pane used to start its
/// own render of one sweep, preceded by its own `RenderInput::extract` on the
/// frame thread.
#[path = "app_render/one_render_per_sweep_tests.rs"]
#[cfg(test)]
mod one_render_per_sweep_tests;
