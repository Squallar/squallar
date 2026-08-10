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
    /// The attachments [`EguiRenderer::draw`]'s render pass has. Recorded at
    /// construction because there is nowhere else to read them back from — see
    /// [`AttachmentConfig`].
    attachment_config: AttachmentConfig,
}

/// The attachment layout of the egui render pass.
///
/// A `wgpu::RenderPipeline` has to declare the colour format, depth-stencil
/// state and sample count of the pass it will be used in; a mismatch is a
/// validation error at `create_render_pipeline`, and `create_render_pipeline`
/// does not return `Result`. So anything building a pipeline that draws into
/// egui's own pass has to be told these three, and `egui_wgpu::Renderer` exposes
/// none of them — hence recording them on the way past.
///
/// **The volume raymarch is not the consumer.** It renders into an offscreen
/// `Rgba8Unorm` target of its own, so it is bound by that target's format rather
/// than by this. The consumer is the **blit quad** that composites that target
/// into egui's pass, which is the one pipeline that genuinely has to match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachmentConfig {
    /// The colour attachment's format — the swapchain's, in practice.
    ///
    /// Note this is deliberately *not* always non-sRGB:
    /// `app_state::select_surface_format` only prefers a non-sRGB format on
    /// wasm32, and natively falls back to `capabilities.formats[0]`. Anything
    /// that has to match egui's gamma convention must key off
    /// `TextureFormat::is_srgb` on this value rather than assume either way.
    pub color_format: TextureFormat,
    /// The depth-stencil attachment's format, or `None` when the pass has none.
    /// `EguiRenderer::draw` attaches no depth buffer today.
    pub depth_format: Option<TextureFormat>,
    /// Samples per pixel in the pass. 1 today, i.e. MSAA off.
    pub msaa_samples: u32,
}

/// An egui pass that has been ended, tessellated and uploaded.
///
/// Holding one is proof that [`EguiRenderer::end_pass_and_upload`] already ran,
/// which is the ordering guarantee the frame path depends on.
pub struct PreparedFrame {
    tris: Vec<egui::ClippedPrimitive>,
    /// The descriptor this geometry was built for. Carried with the geometry so
    /// the draw cannot be clipped at a different scale than it was laid out at.
    screen_descriptor: ScreenDescriptor,
    /// Textures egui retired this frame.
    textures_to_free: Vec<egui::TextureId>,
    /// Command buffers egui collected from this frame's paint callbacks.
    ///
    /// `egui_wgpu::Renderer::update_buffers` returns whatever every
    /// [`egui_wgpu::CallbackTrait::prepare`] and `finish_prepare` handed back
    /// (`egui-wgpu-0.35.0/src/renderer.rs:1050-1075`), and that return is *not*
    /// `#[must_use]`. This field exists because dropping it — which this code did
    /// until the fix — means a callback recording into its own command buffers
    /// renders nothing at all, with no validation error and no warning anywhere.
    ///
    /// Drained by [`PreparedFrame::submit`].
    user_command_buffers: Vec<wgpu::CommandBuffer>,
}

/// Order a frame's command buffers the way egui-wgpu documents.
///
/// The callbacks' buffers go first and egui's own last. This is not cosmetic:
/// a callback's `prepare` exists to produce the resources its `paint` then reads
/// inside egui's render pass, so submitting egui's buffer first would run the
/// paint against whatever the callback's target held on the *previous* frame.
///
/// Generic over the buffer type purely so the ordering can be unit-tested
/// without a GPU — the order is the one thing here a refactor can quietly
/// invert. It matches `egui_wgpu`'s own painter, which submits
/// `chain(user_cmd_bufs, [encoded])` (`egui-wgpu-0.35.0/src/winit.rs:733`).
fn submission_order<T>(callbacks: Vec<T>, egui: T) -> Vec<T> {
    let mut ordered = callbacks;
    ordered.push(egui);
    ordered
}

impl PreparedFrame {
    /// Textures egui retired this frame, to be freed once the GPU is done.
    pub fn textures_to_free(&self) -> &[egui::TextureId] {
        &self.textures_to_free
    }

    /// Submit every command buffer this frame recorded, egui's included.
    ///
    /// Takes the encoder **by value** so that finishing egui's own commands and
    /// submitting the callbacks' cannot be separated: there is no way to reach
    /// `encoder.finish()` through this type without also handing over
    /// [`Self::user_command_buffers`]. That is the shape of the guarantee, and
    /// `the_frame_path_submits_only_through_prepared_frame` is what keeps the
    /// caller from routing round it.
    ///
    /// Safe to call on the frame that never acquired a surface, too: egui's
    /// uploads still have to land, and a callback that recorded work for a frame
    /// nobody draws still has to be flushed rather than leaked.
    pub fn submit(&mut self, queue: &Queue, encoder: CommandEncoder) {
        let callbacks = std::mem::take(&mut self.user_command_buffers);
        queue.submit(submission_order(callbacks, encoder.finish()));
    }
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
        window: &Window,
    ) -> EguiRenderer {
        let egui_context = Context::default();

        // Query the device's actual texture size limit
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
            // The same three values `Renderer::new` was just given, kept because
            // it offers no way to ask for them back.
            attachment_config: AttachmentConfig {
                color_format: output_color_format,
                depth_format: output_depth_format,
                msaa_samples,
            },
        }
    }

    /// The attachments [`Self::draw`]'s render pass has. See [`AttachmentConfig`].
    pub fn attachment_config(&self) -> AttachmentConfig {
        self.attachment_config
    }

    /// egui's per-type store for resources a paint callback needs across frames.
    ///
    /// `egui_wgpu::Renderer::callback_resources` is `pub`
    /// (`egui-wgpu-0.35.0/src/renderer.rs:259`) but [`Self::renderer`] is not, so
    /// this accessor is the only way to reach it — and it is the *only* channel
    /// there is, because `CallbackTrait::prepare` and `paint` both take `&self`
    /// and so cannot own mutable state of their own.
    ///
    /// `_mut` even though [`Self::draw`] takes `&self`: `update_buffers` already
    /// hands callbacks a `&mut CallbackResources`, so nothing here is made more
    /// mutable than it already was.
    ///
    /// A caveat worth knowing before inserting: `CallbackResources` is a
    /// `TypeMap` keyed by type, not by pane or by callback. One inserted type is
    /// one slot for the whole application, so anything that needs to be
    /// per-instance has to carry its own map inside that slot.
    pub fn callback_resources_mut(&mut self) -> &mut egui_wgpu::CallbackResources {
        &mut self.renderer.callback_resources
    }

    pub fn handle_input(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.state.on_window_event(window, event);
        response.repaint
    }

    /// Start an egui pass.
    ///
    /// Applied *before* `begin_pass`, not after. egui consumes a pending zoom
    /// change at the start of a pass, so setting it afterwards — as this used to
    /// — would not take effect until the next frame, leaving that frame's
    /// geometry a scale behind. Setting it here makes
    /// `Context::pixels_per_point()` authoritative for the pass that follows,
    /// which is what the tessellation and the screen descriptor are both taken
    /// from.
    ///
    /// `zoom_factor` is the application's own scaling only — it deliberately
    /// excludes the window's DPI. egui multiplies it by the native
    /// pixels-per-point carried on the raw input, which egui-winit keeps in step
    /// with the window. Passing a finished pixels_per_point instead would make
    /// egui divide it back out by the native scale it *currently* holds, and on
    /// the one frame a monitor's DPI changes that is still the old value, so the
    /// result overshoots by the ratio of the two before self-correcting the
    /// frame after.
    pub fn begin_frame(&mut self, window: &Window, zoom_factor: f32) {
        self.context().set_zoom_factor(zoom_factor);
        let mut raw_input = self.state.take_egui_input(window);
        // Before `begin_pass`: egui buckets touches by device as it folds the
        // events in, so a later rewrite would be a frame too late.
        rustdar_egui::normalize_touch_devices(&mut raw_input);
        // Web only: native reports one line per notch, which egui's native
        // `line_scroll_speed` already scales correctly.
        #[cfg(target_arch = "wasm32")]
        rustdar_egui::normalize_wheel_units(&mut raw_input, zoom_factor);
        self.state.egui_ctx().begin_pass(raw_input);
    }

    /// End the egui pass, tessellate it, and upload everything the GPU needs.
    ///
    /// **This must run before the swapchain is touched, and unconditionally.**
    /// `Context::end_pass` is what pops egui's viewport stack and hands over the
    /// frame's texture deltas — including font-atlas growth, which egui emits
    /// exactly once per region. A frame that returns early because the surface
    /// could not be acquired leaves the pass open (every later frame then nests
    /// one level deeper, and egui stops applying zoom changes because it no
    /// longer believes it is on the outermost viewport) and strands those
    /// uploads.
    ///
    /// Only queue writes happen here, so none of it depends on having a render
    /// target. See `app::render::finish_then_acquire` for the ordering.
    pub fn end_pass_and_upload(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        window: &Window,
        size_in_pixels: [u32; 2],
    ) -> PreparedFrame {
        let full_output = self.state.egui_ctx().end_pass();

        // Handle platform output more carefully to avoid animation loops
        self.state
            .handle_platform_output(window, full_output.platform_output);

        // Taken from the context rather than from a cached scale factor so the
        // geometry and the descriptor that clips it cannot disagree: this is the
        // value the pass was actually laid out at.
        let pixels_per_point = self.state.egui_ctx().pixels_per_point();
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels,
            pixels_per_point,
        };

        // Always render - the change detection was causing panels to blink
        let tris = self
            .state
            .egui_ctx()
            .tessellate(full_output.shapes, pixels_per_point);

        for (id, image_delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(device, queue, *id, image_delta);
        }
        // `update_buffers` also dispatches every paint callback's `prepare` and
        // `finish_prepare`, and returns the command buffers they produced. The
        // return must be carried to the submit — see `user_command_buffers`.
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
        }
    }

    /// Record the render pass for an already-prepared frame.
    ///
    /// Note the pass this opens has **no depth attachment and no resolve
    /// target**, which is what makes [`Self::attachment_config`] honest only
    /// while `new` is called with `None` depth and one sample. Both halves are
    /// pinned by `the_pass_draw_opens_matches_what_attachment_config_promises` —
    /// a pipeline built from a depth format this pass does not attach fails
    /// validation at draw time, and `create_render_pipeline` returns no `Result`
    /// to notice it in.
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

    /// Free textures that are no longer needed.  Call after `queue.submit()`.
    pub fn free_textures(&mut self, ids: &[egui::TextureId]) {
        for id in ids {
            self.renderer.free_texture(id);
        }
    }

    /// Apply dark/light theme only when it actually changes.
    pub fn apply_theme(&mut self, use_dark: bool) {
        if self.applied_visuals_dark != Some(use_dark) {
            self.applied_visuals_dark = Some(use_dark);
            apply_theme_to_context(self.context(), use_dark);
        }
    }
}

/// The theme as one context-level application: the palette, plus the style
/// rules that must hold under both palettes.
///
/// `selectable_labels` goes off here because rustdar's labels are readouts,
/// not documents: a map drag that ends over the chrome left label text
/// highlighted as though selected (the M8 first-run finding), and nothing in
/// the app wants label text selected — `TextEdit` fields keep their own
/// selection regardless of this flag. `all_styles_mut` writes the rule into
/// both of egui's per-theme styles, so a later visuals flip cannot resurrect
/// it.
///
/// A free function over the `Context` rather than a renderer method so a host
/// test can drive it against a bare context — the renderer itself needs a
/// wgpu device no host test has.
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
