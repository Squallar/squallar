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

/// How large the mirror is drawn, and how often that is allowed to change.
mod mirror;

pub use mirror::{
    MIRROR_RUNG_DWELL_FRAMES, MIRROR_RUNG_HYSTERESIS, MIRROR_SCALE_MAX, MirrorLimits, MirrorPlan,
    MirrorRungs, mirror_plan, mirror_size_for, wanted_scale_for,
};
pub use rustdar_device_profile::constants::MIRROR_MAX_SIDE;

/// How a texture delta gets onto the GPU without the frame paying for it.
pub mod texture_upload;

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

    /// Submit every command buffer this frame recorded, egui's included. Takes
    /// the encoder by value so egui's `finish` and the callbacks' submit cannot
    /// be separated. Safe on a frame that never acquired a surface.
    pub fn submit(&mut self, queue: &Queue, encoder: CommandEncoder) {
        let callbacks = std::mem::take(&mut self.user_command_buffers);
        queue.submit(submission_order(callbacks, encoder.finish()));
    }
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
pub(crate) fn install_repaint_wake(ctx: &Context, wake: impl Fn() + Send + Sync + 'static) {
    ctx.set_request_repaint_callback(move |info| {
        if info.delay.is_zero() {
            wake();
        }
    });
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
        let egui_renderer = Renderer::new(
            device,
            output_color_format,
            egui_wgpu::RendererOptions {
                depth_stencil_format: output_depth_format,
                msaa_samples,
                ..Default::default()
            },
        );

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
        }
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
    pub fn begin_frame(&mut self, window: &Window, zoom_factor: f32) {
        self.context().set_zoom_factor(zoom_factor);
        let mut raw_input = self.state.take_egui_input(window);
        // Before `begin_pass`: egui buckets touches by device as it folds the
        // events in.
        rustdar_egui::normalize_touch_devices(&mut raw_input);
        // Web only: native reports one line per notch, which egui's native
        // `line_scroll_speed` already scales correctly.
        #[cfg(target_arch = "wasm32")]
        rustdar_egui::normalize_wheel_units(&mut raw_input, zoom_factor);
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

        let mut tris = self
            .state
            .egui_ctx()
            .tessellate(full_output.shapes, pixels_per_point);

        // Not `Renderer::update_texture` in a loop: that is up to 59 ms of
        // blocking host stores on this thread at the raster ceiling.
        let uploading = self.uploads.apply(
            device,
            queue,
            &mut self.renderer,
            &full_output.textures_delta.set,
        );
        // Before the `update_buffers` below, not after — see `render_mirror`.
        if let Some(request) = mirror {
            self.render_mirror(device, queue, &mut tris, &request);
        }
        // `update_buffers` also dispatches every callback's `prepare` and
        // returns their command buffers; they must reach the submit.
        let user_command_buffers =
            self.renderer
                .update_buffers(device, queue, encoder, &tris, &screen_descriptor);

        PreparedFrame {
            tris,
            screen_descriptor,
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
                timestamp_writes: None,
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
            timestamp_writes: None,
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
