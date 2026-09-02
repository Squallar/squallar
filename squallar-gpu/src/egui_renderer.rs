use egui::Context;
use egui_wgpu::Renderer;
use egui_wgpu::wgpu::{CommandEncoder, Device, Queue, StoreOp, TextureFormat, TextureView};
use egui_winit::State;

use egui_wgpu::{ScreenDescriptor, wgpu};
use winit::event::WindowEvent;
use winit::window::Window;

pub struct EguiRenderer {
    state: State,
    renderer: Renderer,
    applied_visuals_dark: Option<bool>,
    /// The attachments [`EguiRenderer::draw`]'s render pass has.
    attachment_config: AttachmentConfig,
    /// Where egui's texture deltas actually cross PCIe.
    uploads: texture_upload::TextureUploads,
    /// What ending each pass cost, phase by phase.
    pass_costs: pass_costs::PassCostLedger,
    /// Which route this renderer's geometry took. The write side is inside
    /// `egui_wgpu::Renderer`; this is the read side of the same counters.
    geometry_staging: geometry_staging::GeometryStagingLedger,
    /// Whether this frame's raw input carried interaction. Written by
    /// [`Self::begin_frame`]; see [`Self::frame_had_interaction`].
    frame_interacted: bool,
    /// The GPU pass probe, present only where [`Self::install_gpu_probe`] was
    /// called and the device can time a pass. `None` on every ordinary
    /// install, and every use is behind the `Option` — an uninstalled probe
    /// submits zero query operations.
    probe: Option<crate::gpu_probe::GpuPassProbe>,
}

/// The attachment layout of the egui render pass. A pipeline drawing into it
/// must declare these three, and `egui_wgpu::Renderer` exposes none of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachmentConfig {
    /// The swapchain's format. May be sRGB or not, so anything matching egui's
    /// gamma convention must key off `TextureFormat::is_srgb` on this value.
    pub color_format: TextureFormat,
    /// The depth-stencil attachment's format, or `None` when the pass has none.
    pub depth_format: Option<TextureFormat>,
    /// Samples per pixel in the pass. 1 today, i.e. MSAA off.
    pub msaa_samples: u32,
}

/// Whether egui's fragment shader dithers its output down to eight bits.
///
/// `egui_wgpu::RendererOptions` defaults this to `true`, and `Renderer::new`
/// below asks for it **by name** rather than through `..Default::default()`,
/// because a second pipeline draws into the same pass and has to make the same
/// choice: [`crate::tile_mesh::TileMeshStore`] reads this constant, and an
/// undithered fill beside a dithered gradient is a seam in the map.
pub const EGUI_DITHERING: bool = true;

/// How large the mirror is drawn, and how often that is allowed to change.
mod mirror;

pub use mirror::{
    MIRROR_RUNG_DWELL_FRAMES, MIRROR_RUNG_HYSTERESIS, MIRROR_SCALE_MAX, MirrorLimits, MirrorPlan,
    MirrorRungs, mirror_plan, mirror_size_for, wanted_scale_for,
};
pub use squallar_device_profile::constants::MIRROR_MAX_SIDE;

/// What ending each egui pass costs the frame thread, phase by phase.
pub mod pass_costs;

/// How a texture delta gets onto the GPU without the frame paying for it.
pub mod texture_upload;

/// How a frame's vertices and indices get there without the frame paying for
/// them either.
pub mod geometry_staging;

/// What the mirror pass is asked to copy, and where to put it. The 3D view's
/// map floor is the pane's own map render, drawn into an off-screen strip below
/// the frame and copied here for the raymarch to sample.
pub struct MirrorRequest<'a> {
    /// The colour attachment to draw into. Must have the same sRGB-ness as the
    /// swapchain, whose format picked egui's fragment entry point at
    /// `Renderer::new`.
    pub view: &'a wgpu::TextureView,
    /// The mirror's size in texels and the scale to draw at, from
    /// [`mirror_size_for`].
    pub size_in_pixels: [u32; 2],
    /// See [`mirror_size_for`].
    pub pixels_per_point: f32,
    /// The strips, in points, whose primitives are copied — all below the
    /// frame's bottom edge. Outside them stays transparent: "no ground here".
    pub source_rects: &'a [egui::Rect],
}

/// An egui pass that has been ended, tessellated and uploaded. Holding one is
/// proof that [`EguiRenderer::end_pass_and_upload`] already ran.
pub struct PreparedFrame {
    tris: Vec<egui::ClippedPrimitive>,
    /// The descriptor this geometry was built for. Carried with the geometry so
    /// the draw cannot be clipped at a different scale than it was laid out at.
    screen_descriptor: ScreenDescriptor,
    textures_to_free: Vec<egui::TextureId>,
    /// The root viewport's `repaint_delay`: `ZERO` while an animation is in
    /// flight, a finite delay for a timed request, `MAX` when nothing asked.
    /// Honoured by the app side's `repaint_action`; the loop runs on
    /// `ControlFlow::Wait`, so dropping it strands the animation.
    repaint_delay: std::time::Duration,
    /// Command buffers egui collected from this frame's paint callbacks
    /// (`egui-wgpu-0.35.0/src/renderer.rs:1050-1075`). `update_buffers`' return
    /// is *not* `#[must_use]`, and dropping it makes a callback render nothing
    /// at all, with no validation error. Drained by [`PreparedFrame::submit`].
    user_command_buffers: Vec<wgpu::CommandBuffer>,
    /// Where this pass crossed its own phase boundaries, for a caller that
    /// brackets a wider span than this function and needs to cut it. See
    /// [`pass_costs::PassPhaseStamps`], which says why the stamps travel
    /// rather than the durations [`pass_costs::PassCosts`] already keeps.
    phase_stamps: pass_costs::PassPhaseStamps,
}

/// Whole microseconds from `a` to `b`, for the pass-cost stamps. Saturates
/// rather than overflowing; `web_time::Instant` is std-backed off the web.
fn micros_between(a: web_time::Instant, b: web_time::Instant) -> u64 {
    b.duration_since(a).as_micros().min(u128::from(u64::MAX)) as u64
}

/// Callbacks' command buffers first, egui's own last: a callback's `prepare`
/// produces what its `paint` reads inside egui's pass. Matches `egui_wgpu`'s
/// `chain(user_cmd_bufs, [encoded])` (`egui-wgpu-0.35.0/src/winit.rs:733`).
fn submission_order<T>(callbacks: Vec<T>, egui: T) -> Vec<T> {
    let mut ordered = callbacks;
    ordered.push(egui);
    ordered
}

impl PreparedFrame {
    pub fn textures_to_free(&self) -> &[egui::TextureId] {
        &self.textures_to_free
    }

    pub fn repaint_delay(&self) -> std::time::Duration {
        self.repaint_delay
    }

    /// Where the pass that built this frame crossed its phase boundaries.
    /// See [`pass_costs::PassPhaseStamps`] for the six spans these five
    /// stamps cut, and why the app's frame ledger needs the instants rather
    /// than the durations the renderer already totals for itself.
    pub fn phase_stamps(&self) -> pass_costs::PassPhaseStamps {
        self.phase_stamps
    }

    /// Submit every command buffer this frame recorded, egui's included. Takes
    /// the encoder by value so egui's `finish` and the callbacks' submit cannot
    /// be separated. Safe on a frame that never acquired a surface.
    pub fn submit(&mut self, queue: &Queue, encoder: CommandEncoder) {
        let callbacks = std::mem::take(&mut self.user_command_buffers);
        queue.submit(submission_order(callbacks, encoder.finish()));
    }
}

/// Mesh vertices, mesh indices and the bytes they occupy, over the primitives
/// about to be handed to `update_buffers`.
///
/// The same fold `egui_wgpu::Renderer::update_buffers` opens with
/// (`egui-wgpu-0.35.0/src/renderer.rs:941-958`) and the same arithmetic it
/// sizes its staging buffers with, so the byte figure is what actually crosses
/// into the staging buffer rather than an estimate of it.
/// `Primitive::Callback` carries no mesh and contributes nothing.
fn staged_geometry(tris: &[egui::ClippedPrimitive]) -> (u64, u64, u64) {
    use egui::epaint::Primitive;

    let (vertices, indices) =
        tris.iter()
            .fold((0u64, 0u64), |acc, clipped| match &clipped.primitive {
                Primitive::Mesh(mesh) => (
                    acc.0 + mesh.vertices.len() as u64,
                    acc.1 + mesh.indices.len() as u64,
                ),
                Primitive::Callback(_) => acc,
            });
    let bytes =
        vertices * size_of::<egui::epaint::Vertex>() as u64 + indices * size_of::<u32>() as u64;
    (vertices, indices, bytes)
}

/// A primitive's clip rect, narrowed to whichever source rect it belongs to;
/// `Rect::ZERO` for none — a zero-size scissor, which `Renderer::render` skips
/// while still advancing its buffer iterators. First match, not the union: the
/// source strips are disjoint.
fn clamp_to_sources(clip: egui::Rect, sources: &[egui::Rect]) -> egui::Rect {
    sources
        .iter()
        .find(|source| source.intersects(clip))
        .map_or(egui::Rect::ZERO, |source| clip.intersect(*source))
}

/// Make `ctx.request_repaint()` reach the event loop: it only sets a flag the
/// next `begin_pass` reads, and a loop on `ControlFlow::Wait` never gets there.
///
/// **Only a zero delay wakes.** A timed request is already carried out of the
/// frame by `FullOutput`'s `repaint_delay` and scheduled by the app side;
/// honouring it here too would turn it into a redraw per frame, forever.
///
/// **The wake is carried out off egui's context lock, and that is the whole
/// reason [`repaint_handoff`] exists.** See it for the deadlock.
pub(crate) fn install_repaint_wake(ctx: &Context, wake: impl Fn() + Send + Sync + 'static) {
    let post = repaint_handoff(wake);
    ctx.set_request_repaint_callback(move |info| {
        if info.delay.is_zero() {
            post();
        }
    });
}

/// Turn `wake` into something safe to call from inside egui's repaint
/// callback.
///
/// **The invariant: nothing egui's repaint callback runs may block on another
/// thread.** egui invokes that callback with the `Context`'s *write* lock
/// held — `egui-0.35.0/src/context.rs:161-167`, reached from
/// `Context::request_repaint_of` -> `self.write(..)`, and again at `:115-121`
/// from `begin_pass`. For as long as the callback runs, every other thread is
/// locked out of the context, the frame thread included.
///
/// That made a hard deadlock out of a call that reads like a post. winit's
/// `Window::request_redraw` (`winit-0.30.13/src/window.rs:600`) goes through
/// `maybe_queue_on_main`, and the macOS copy of that
/// (`src/platform_impl/macos/window.rs:40-51`) says in its own comment that it
/// deliberately does *not* queue — it is `MainThreadBound::get_on_main` ->
/// `objc2_foundation::run_on_main` -> `dispatch::Queue::main().exec_sync`,
/// which blocks the caller until the **main thread** services the queue. So a
/// tile decode calling `ctx.request_repaint()` took egui's write lock, then
/// waited for a main thread that was inside `App::handle_redraw` waiting for
/// that same lock at the first `ctx.content_rect()` of `Gui::ui`. Neither ever
/// moved again and the app stopped rendering entirely, in 2-35 s, on every
/// overlay-heavy scene. The Linux backends pass `maybe_queue_on_main` straight
/// through (`src/platform_impl/linux/mod.rs:307-313`), which is why only macOS
/// ever showed it.
///
/// The fix is not to stop asking for redraws off-frame — the tile, basemap and
/// area workers all legitimately do, and a dropped wake is a
/// `ControlFlow::Wait` loop that sleeps through its own data arriving. The fix
/// is that the asking no longer happens under the lock.
#[cfg(not(target_arch = "wasm32"))]
fn repaint_handoff(wake: impl Fn() + Send + Sync + 'static) -> impl Fn() + Send + Sync + 'static {
    let (posted, wakes) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("squallar-redraw-wake".to_owned())
        .spawn(move || {
            // Ends when the context — and with it the callback holding the
            // sender — is dropped.
            while wakes.recv().is_ok() {
                // Coalesce a burst into one platform call: the loop only needs
                // telling once that a frame is due, and on macOS each call is a
                // round trip to the main thread.
                while wakes.try_recv().is_ok() {}
                wake();
            }
        })
        // Louder than the alternatives. Falling back to calling `wake` inline
        // would put the deadlock back, and swallowing it would leave a loop on
        // `ControlFlow::Wait` that never draws another off-frame arrival —
        // which is the same stopped app, without the stack that explains it.
        .expect("the redraw wake thread is what lets an off-frame repaint reach the event loop");
    move || {
        // Non-blocking: an unbounded `Sender` never waits on the receiver, so
        // egui's write lock is released as soon as this returns.
        let _ = posted.send(());
    }
}

/// One thread, so there is nobody to block on and nothing to hand off.
///
/// The web build's wake is a `requestAnimationFrame`-shaped post, and wasm has
/// no equivalent of the macOS main-thread rendezvous described on
/// [`repaint_handoff`]'s native arm.
#[cfg(target_arch = "wasm32")]
fn repaint_handoff(wake: impl Fn() + Send + Sync + 'static) -> impl Fn() + Send + Sync + 'static {
    wake
}

/// Whether one frame's raw input carried a hand on the app: at least one
/// pointer move, pointer button, touch, wheel or zoom event. This is the whole
/// definition of an *interact* frame everywhere the frame telemetry buckets by
/// it — keyboard, focus and IME events are deliberately not in it, because the
/// bar being held is "the picture answers the hand", and a keystroke does not
/// move the map.
fn input_carries_interaction(events: &[egui::Event]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            egui::Event::PointerMoved(_)
                | egui::Event::PointerButton { .. }
                | egui::Event::Touch { .. }
                | egui::Event::MouseWheel { .. }
                | egui::Event::Zoom(_)
        )
    })
}

impl EguiRenderer {
    pub fn context(&self) -> &Context {
        self.state.egui_ctx()
    }

    pub fn new(
        device: &Device,
        output_color_format: TextureFormat,
        output_depth_format: Option<TextureFormat>,
        msaa_samples: u32,
        window: &crate::WindowRef,
        // The app side's redraw request; this crate cannot name the event loop.
        wake: impl Fn() + Send + Sync + 'static,
    ) -> EguiRenderer {
        let egui_context = Context::default();
        install_repaint_wake(&egui_context, wake);

        let max_texture_side = device.limits().max_texture_dimension_2d as usize;

        let egui_state = egui_winit::State::new(
            egui_context,
            egui::viewport::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(max_texture_side),
        );
        let mut egui_renderer = Renderer::new(
            device,
            output_color_format,
            egui_wgpu::RendererOptions {
                depth_stencil_format: output_depth_format,
                msaa_samples,
                dithering: EGUI_DITHERING,
                ..Default::default()
            },
        );

        // Without this the frame's vertices and indices are pushed across PCIe
        // by this thread at 2.15 GB/s; with it the copy engine pulls them at
        // an order of magnitude more. See [`geometry_staging`]. A device
        // without the ring feature -- all of the web -- gets no stager and
        // egui's own route, unchanged.
        let geometry_ledger = geometry_staging::GeometryStagingLedger::default();
        if geometry_staging::available(device) {
            egui_renderer.set_geometry_stager(Box::new(geometry_staging::GeometryStaging::new(
                &geometry_ledger,
            )));
        }

        EguiRenderer {
            state: egui_state,
            renderer: egui_renderer,
            applied_visuals_dark: None,
            // The three values `Renderer::new` was given; it offers no way back.
            attachment_config: AttachmentConfig {
                color_format: output_color_format,
                depth_format: output_depth_format,
                msaa_samples,
            },
            uploads: texture_upload::TextureUploads::new(device),
            pass_costs: pass_costs::PassCostLedger::default(),
            geometry_staging: geometry_ledger,
            frame_interacted: false,
            probe: None,
        }
    }

    /// Install the GPU pass probe, and say whether one was installed. The
    /// caller gates on the install having asked for frame telemetry; this
    /// answers `false` on a device without `TIMESTAMP_QUERY` (every WebGL2
    /// leg), which is the app's cue for the honest absence line. A clone of
    /// the probe's handle goes into the callback resources so the volume
    /// callback's `prepare` can bracket the passes it encodes.
    pub fn install_gpu_probe(&mut self, device: &Device, queue: &Queue) -> bool {
        let Some(probe) = crate::gpu_probe::GpuPassProbe::new(device, queue) else {
            return false;
        };
        self.renderer.callback_resources.insert(probe.handle());
        self.probe = Some(probe);
        true
    }

    /// Close the probe's frame: totals drained, claimed brackets resolved
    /// into `encoder`. Call after the last pass of the frame is recorded,
    /// before the submit. A no-op without a probe.
    pub fn probe_end_frame(&mut self, encoder: &mut CommandEncoder) {
        if let Some(probe) = self.probe.as_mut() {
            probe.end_frame(encoder);
        }
    }

    /// Harvest the probe's ring. Call after the frame's `queue.submit`;
    /// never blocks. A no-op without a probe.
    pub fn probe_collect(&mut self) {
        if let Some(probe) = self.probe.as_mut() {
            probe.collect();
        }
    }

    /// What the probe has measured, or `None` where none is installed — the
    /// app's telemetry line keys presence off exactly this.
    pub fn gpu_pass_report(&self) -> Option<crate::gpu_probe::GpuPassReport> {
        self.probe
            .as_ref()
            .map(crate::gpu_probe::GpuPassProbe::report)
    }

    /// Whether the frame the last [`Self::begin_frame`] opened carried
    /// interaction in its raw input — see [`input_carries_interaction`] for
    /// the exact event set. Read after the pass, when the app buckets the
    /// frame's timing into its interact or idle family.
    pub fn frame_had_interaction(&self) -> bool {
        self.frame_interacted
    }

    /// The attachments [`Self::draw`]'s render pass has. See [`AttachmentConfig`].
    pub fn attachment_config(&self) -> AttachmentConfig {
        self.attachment_config
    }

    /// egui's per-type store for resources a paint callback needs across frames
    /// — the only such channel, since `prepare` and `paint` take `&self`. Keyed
    /// by type, not by pane: one type is one slot for the whole application, so
    /// anything per-instance carries its own map inside that slot.
    pub fn callback_resources_mut(&mut self) -> &mut egui_wgpu::CallbackResources {
        &mut self.renderer.callback_resources
    }

    pub fn handle_input(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.state.on_window_event(window, event);
        response.repaint
    }

    /// Start an egui pass. The zoom is applied *before* `begin_pass`: egui
    /// consumes a pending zoom change at the start of a pass.
    ///
    /// `zoom_factor` excludes the window's DPI — egui multiplies it by the
    /// native pixels-per-point on the raw input. A finished pixels_per_point
    /// gets divided back out by the scale egui currently holds, which
    /// overshoots on the frame a monitor's DPI changes.
    ///
    /// `extra_events` is the gesture player's injection seam: appended ahead
    /// of the normalizers, so a scripted Line-unit wheel notch or per-finger
    /// touch takes the same rewrite path a real one would, and ahead of the
    /// interaction scan, so a scripted frame tags *interact* with no special
    /// case. Empty on every unarmed install, which leaves the raw input
    /// byte-identical.
    pub fn begin_frame(
        &mut self,
        window: &Window,
        zoom_factor: f32,
        extra_events: Vec<egui::Event>,
    ) {
        self.context().set_zoom_factor(zoom_factor);
        let mut raw_input = self.state.take_egui_input(window);
        raw_input.events.extend(extra_events);
        // Before `begin_pass`: egui buckets touches by device as it folds the
        // events in.
        squallar_egui::normalize_touch_devices(&mut raw_input);
        // Web only: native reports one line per notch, which egui's native
        // `line_scroll_speed` already scales correctly.
        #[cfg(target_arch = "wasm32")]
        squallar_egui::normalize_wheel_units(&mut raw_input, zoom_factor);
        // After the rewrites, so an event they re-spell is judged in the form
        // egui will fold in.
        self.frame_interacted = input_carries_interaction(&raw_input.events);
        self.state.egui_ctx().begin_pass(raw_input);
    }

    /// End the egui pass, tessellate it, and upload everything the GPU needs.
    ///
    /// **Must run before the swapchain is touched, and unconditionally.**
    /// `end_pass` pops egui's viewport stack and hands over the texture deltas;
    /// returning early on a failed acquire leaves the pass open and strands
    /// them. Only queue writes happen here.
    pub fn end_pass_and_upload(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        window: &Window,
        size_in_pixels: [u32; 2],
        mirror: Option<MirrorRequest<'_>>,
    ) -> PreparedFrame {
        // Before `end_pass`, so the pass close and the platform-output handoff
        // are inside a measured span rather than ahead of the first stamp.
        // They were: `stamp_tessellate` used to be this function's first clock
        // read, which left everything above it invisible to every figure.
        let stamp_entry = web_time::Instant::now();
        let full_output = self.state.egui_ctx().end_pass();

        // Taken before the output is dismembered.
        let repaint_delay = full_output
            .viewport_output
            .get(&egui::viewport::ViewportId::ROOT)
            .map(|out| out.repaint_delay)
            .unwrap_or(std::time::Duration::MAX);

        self.state
            .handle_platform_output(window, full_output.platform_output);

        // From the context, not a cached scale factor: the geometry and the
        // descriptor that clips it cannot disagree.
        let pixels_per_point = self.state.egui_ctx().pixels_per_point();
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels,
            pixels_per_point,
        };

        let stamp_tessellate = web_time::Instant::now();
        let mut tris = self
            .state
            .egui_ctx()
            .tessellate(full_output.shapes, pixels_per_point);

        // Not `Renderer::update_texture` in a loop: that is up to 59 ms of
        // blocking host stores on this thread at the raster ceiling.
        let stamp_upload = web_time::Instant::now();
        let uploading = self.uploads.apply(
            device,
            queue,
            &mut self.renderer,
            &full_output.textures_delta.set,
        );
        let stamp_upload_done = web_time::Instant::now();
        // Before the `update_buffers` below, not after — see `render_mirror`.
        let mut mirror_us = 0u64;
        if let Some(request) = mirror {
            let stamp_mirror = web_time::Instant::now();
            self.render_mirror(device, queue, &mut tris, &request);
            mirror_us = micros_between(stamp_mirror, web_time::Instant::now());
        }
        // **Before `stamp_buffers`, and that is the whole point of the
        // placement.** This walk is the instrument for the phase the stamp
        // below opens; counted inside it, the byte total would inflate the
        // very microseconds it is divided by, and a before/after on
        // `buffers_and_callbacks_us` would be comparing two different clocks.
        let (vertices, indices, bytes) = staged_geometry(&tris);
        self.pass_costs.note_staged(vertices, indices, bytes);
        // `update_buffers` also dispatches every callback's `prepare` and
        // returns their command buffers; they must reach the submit.
        let stamp_buffers = web_time::Instant::now();
        let user_command_buffers =
            self.renderer
                .update_buffers(device, queue, encoder, &tris, &screen_descriptor);
        self.pass_costs.note(
            micros_between(stamp_tessellate, stamp_upload),
            micros_between(stamp_upload, stamp_upload_done),
            mirror_us,
            micros_between(stamp_buffers, web_time::Instant::now()),
        );

        PreparedFrame {
            tris,
            screen_descriptor,
            phase_stamps: pass_costs::PassPhaseStamps {
                entry: stamp_entry,
                tessellate: stamp_tessellate,
                upload: stamp_upload,
                upload_done: stamp_upload_done,
                buffers: stamp_buffers,
            },
            // Freed by the caller AFTER queue.submit(), to avoid destroying GPU
            // resources still referenced by the recorded render pass.
            textures_to_free: full_output.textures_delta.free,
            user_command_buffers,
            // Bands still to move need another frame to move them on: the loop
            // runs on `ControlFlow::Wait`.
            repaint_delay: if uploading {
                std::time::Duration::ZERO
            } else {
                repaint_delay
            },
        }
    }

    /// Draw the floor strips' own geometry into the mirror, and get it onto the
    /// GPU before anything samples it.
    ///
    /// `update_buffers` stages the frame's index/vertex buffers and *then*
    /// dispatches every callback's `prepare` (`renderer.rs:1049-1074`) — one of
    /// which is the raymarch, which samples the mirror. Hence two calls with a
    /// submit between them:
    ///
    /// ```text
    /// update_buffers(filtered, scratch encoder) -> mirror pass -> queue.submit
    ///     -> update_buffers(whole frame, main encoder) -> draw
    /// ```
    ///
    /// That submit is load-bearing: `queue.write_buffer` data lands at the
    /// *next* submit, so without one the second `update_buffers` overwrites the
    /// staging belt before the mirror pass runs.
    ///
    /// Filtering: `render` advances the slice iterators even when it skips a
    /// zero-size scissor (`renderer.rs:516-527`), so a primitive is dropped by
    /// clamping its `clip_rect`. Callbacks are swapped for empty meshes —
    /// `render` ignores `Primitive::Callback` but `update_buffers` does not, and
    /// would run every `prepare` twice.
    fn render_mirror(
        &mut self,
        device: &Device,
        queue: &Queue,
        tris: &mut [egui::ClippedPrimitive],
        request: &MirrorRequest<'_>,
    ) {
        use egui::epaint::Primitive;

        // The mirror half of the floor ledger: one per pass encoded, counted
        // at the thing itself so a skipped frame cannot be miscounted at the
        // seam that decided to skip it.
        squallar_egui::floor_ledger::note_mirror_render();

        // Everything the window below changes, to be put back after it.
        let saved: Vec<(egui::Rect, Option<Primitive>)> = tris
            .iter_mut()
            .map(|primitive| {
                let clip = primitive.clip_rect;
                primitive.clip_rect = clamp_to_sources(clip, request.source_rects);
                let swapped = matches!(primitive.primitive, Primitive::Callback(_)).then(|| {
                    std::mem::replace(
                        &mut primitive.primitive,
                        Primitive::Mesh(egui::epaint::Mesh::default()),
                    )
                });
                (clip, swapped)
            })
            .collect();

        let descriptor = ScreenDescriptor {
            size_in_pixels: request.size_in_pixels,
            pixels_per_point: request.pixels_per_point,
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("egui pane mirror"),
        });
        // Counted here as well as at the frame's own call: this stages the same
        // geometry a second time, and a byte total that missed it would divide
        // into a `buffers` figure that did not. See `pass_costs::StagedGeometry`.
        let (vertices, indices, bytes) = staged_geometry(tris);
        self.pass_costs.note_staged(vertices, indices, bytes);
        // Empty in practice, but carried to the submit: dropping a callback's
        // command buffers is a silent no-render.
        let user_command_buffers =
            self.renderer
                .update_buffers(device, queue, &mut encoder, tris, &descriptor);

        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: request.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Transparent, not black: the shader reads zero alpha
                        // as "the pane's map is not showing this ground".
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                // The stamps land in this pass's own submit; the probe's
                // resolve reads them from the frame's later one, which is
                // legal — a resolve reads whatever the slot last held.
                timestamp_writes: self
                    .probe
                    .as_ref()
                    .and_then(|probe| probe.pass_timestamps(crate::gpu_probe::ProbedPass::Mirror)),
                label: Some("egui pane mirror pass"),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer
                .render(&mut pass.forget_lifetime(), tris, &descriptor);
        }

        queue.submit(submission_order(user_command_buffers, encoder.finish()));

        for (primitive, (clip, swapped)) in tris.iter_mut().zip(saved) {
            primitive.clip_rect = clip;
            if let Some(original) = swapped {
                primitive.primitive = original;
            }
        }
    }

    /// Record the render pass for an already-prepared frame. It has no depth
    /// attachment and no resolve target, which makes [`Self::attachment_config`]
    /// honest only while `new` is called with `None` depth and one sample.
    pub fn draw(
        &mut self,
        encoder: &mut CommandEncoder,
        view: &TextureView,
        frame: &PreparedFrame,
    ) {
        let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: egui_wgpu::wgpu::Operations {
                    load: egui_wgpu::wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: self
                .probe
                .as_ref()
                .and_then(|probe| probe.pass_timestamps(crate::gpu_probe::ProbedPass::Main)),
            label: Some("egui main render pass"),
            occlusion_query_set: None,
            multiview_mask: None,
        });

        self.renderer.render(
            &mut rpass.forget_lifetime(),
            &frame.tris,
            &frame.screen_descriptor,
        );
    }

    /// Whether every texel egui handed over for `id` has reached the GPU.
    /// What this renderer's texture uploads have moved, when anything has moved
    /// since the last time this was asked. See
    /// [`texture_upload::UploadTotals`], which also says what its denominator
    /// is — **every** texture delta egui hands this renderer, not only the
    /// overlay rasters.
    pub fn upload_totals_if_moved(&mut self) -> Option<texture_upload::UploadTotals> {
        self.uploads.totals_if_moved()
    }

    /// What this renderer's texture uploads have moved, asked unconditionally.
    pub fn upload_totals(&self) -> texture_upload::UploadTotals {
        self.uploads.totals()
    }

    /// The geometry this renderer has handed to egui's buffer staging, asked
    /// unconditionally. See [`pass_costs::StagedGeometry`], which also says
    /// what its denominator is — **every `update_buffers` call**, which a
    /// mirror pass makes twice in one pass.
    pub fn staged_geometry(&self) -> pass_costs::StagedGeometry {
        self.pass_costs.staged()
    }

    /// Which route that geometry took, asked unconditionally. See
    /// [`geometry_staging::GeometryStagingTotals`] for the denominator and for
    /// why a zero here is readable.
    pub fn geometry_staging_totals(&self) -> geometry_staging::GeometryStagingTotals {
        self.geometry_staging.totals()
    }

    /// What ending passes has cost this renderer's frame thread, asked
    /// unconditionally. See [`pass_costs::PassCosts`], which also says what
    /// its denominator is — **every pass ended**, presented or not.
    pub fn pass_costs(&self) -> pass_costs::PassCosts {
        self.pass_costs.totals()
    }

    /// [`Self::pass_costs`], but only when a pass has ended since the last
    /// time this was asked — the same contract as
    /// [`Self::upload_totals_if_moved`].
    pub fn pass_costs_if_moved(&mut self) -> Option<pass_costs::PassCosts> {
        self.pass_costs.totals_if_moved()
    }

    /// See [`texture_upload::TextureUploads::is_delivered`].
    pub fn is_delivered(&self, id: egui::TextureId) -> bool {
        self.uploads.is_delivered(id)
    }

    /// Free textures that are no longer needed.  Call after `queue.submit()`.
    pub fn free_textures(&mut self, ids: &[egui::TextureId]) {
        for id in ids {
            self.renderer.free_texture(id);
        }
        self.uploads.free(ids);
    }

    pub fn apply_theme(&mut self, use_dark: bool) {
        if self.applied_visuals_dark != Some(use_dark) {
            self.applied_visuals_dark = Some(use_dark);
            apply_theme_to_context(self.context(), use_dark);
        }
    }
}

/// The theme as one context-level application: the palette, plus the style
/// rules that must hold under both palettes. `all_styles_mut` writes into both
/// per-theme styles so a visuals flip cannot resurrect the old rule.
pub fn apply_theme_to_context(ctx: &egui::Context, use_dark: bool) {
    let visuals = if use_dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| style.interaction.selectable_labels = false);
}

#[cfg(test)]
mod tests;
