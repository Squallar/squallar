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
            let visuals = if use_dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            };
            self.context().set_visuals(visuals);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::submission_order;
    #[cfg(not(target_arch = "wasm32"))]
    use super::{PreparedFrame, Renderer, ScreenDescriptor, TextureFormat, wgpu};

    /// A named function's body, read out of a source file this crate ships.
    ///
    /// `end_pass_and_upload` and `present_frame` both need a real `Window`, a
    /// wgpu device and a swapchain, so no host test can run either. Reading the
    /// source is the only handle there is — the same technique the `begin_frame`
    /// assertions below already rely on.
    fn body_of(source: &'static str, signature: &str) -> &'static str {
        source
            .split_once(signature)
            .and_then(|(_, rest)| rest.split_once("\n    }"))
            .map(|(body, _)| body)
            .unwrap_or_else(|| panic!("`{signature}` is no longer a method there"))
    }

    /// The callbacks' command buffers must precede egui's own.
    ///
    /// A callback's `prepare` records the work its `paint` then reads inside
    /// egui's render pass. Submitting egui's buffer first would paint against
    /// whatever the callback's target held on the previous frame — plausible
    /// output, one frame stale, and no error anywhere. `chain` is a one-token
    /// edit away from being reversed, so pin the order itself.
    #[test]
    fn the_callbacks_command_buffers_are_submitted_before_eguis() {
        assert_eq!(
            submission_order(vec!["callback 0", "callback 1"], "egui"),
            vec!["callback 0", "callback 1", "egui"],
        );
    }

    /// With no callbacks, egui's buffer is still submitted, and alone.
    ///
    /// This is every frame rustdar draws today, so it is the case that must not
    /// regress while the volume view is being built.
    #[test]
    fn a_frame_with_no_callbacks_still_submits_eguis_own_buffer() {
        assert_eq!(submission_order(Vec::new(), "egui"), vec!["egui"]);
    }

    /// `update_buffers`' return must be bound and carried, not dropped.
    ///
    /// This is a real defect that shipped: `egui_wgpu::Renderer::update_buffers`
    /// returns the `Vec<wgpu::CommandBuffer>` it gathered from every
    /// `CallbackTrait::prepare` and `finish_prepare`, the return is not
    /// `#[must_use]`, and this function discarded it. Nothing warned, and nothing
    /// could fail — until a callback exists, at which point its work is silently
    /// never submitted and it renders nothing.
    ///
    /// There is no callback in the crate yet, so no behavioural test can see the
    /// regression; the assertion is that the value is bound and reaches the
    /// returned frame.
    #[test]
    fn end_pass_and_upload_carries_the_callback_command_buffers() {
        let body = body_of(
            include_str!("egui_renderer.rs"),
            "pub fn end_pass_and_upload(",
        );
        let call = body
            .find("update_buffers(")
            .expect("end_pass_and_upload no longer calls update_buffers");

        // The whole statement the call sits in — from the previous statement
        // boundary to its own `;`. Not the line: rustfmt is free to wrap the
        // binding onto a line of its own, and it does.
        let statement_start = body[..call].rfind(';').map_or(0, |semi| semi + 1);
        let statement = body[statement_start..]
            .split_once(';')
            .map(|(head, _)| head)
            .expect("the update_buffers call is not a statement");
        assert!(
            statement.contains("let user_command_buffers"),
            "update_buffers' returned command buffers are discarded again. Any \
             CallbackTrait::prepare that records into them then renders nothing, \
             silently — the return is not #[must_use]. Found: {statement:?}"
        );

        assert!(
            body.contains("user_command_buffers,"),
            "end_pass_and_upload binds the callback command buffers but does not \
             put them on the PreparedFrame it returns, so they are dropped one \
             line later instead of at the call"
        );
    }

    /// The frame path must submit through [`super::PreparedFrame::submit`].
    ///
    /// `submit` takes the encoder by value, so it is impossible to submit egui's
    /// buffer *through it* without the callbacks' — but that only closes the door
    /// on the type level for callers that use it. A caller can still write
    /// `queue.submit(Some(encoder.finish()))` itself, which is exactly the
    /// pre-fix code and compiles clean. There are two submit sites (the frame
    /// that acquired a surface and the frame that did not) and both matter: a
    /// callback that recorded work for a frame nobody draws still has to be
    /// flushed rather than leaked.
    #[test]
    fn the_frame_path_submits_only_through_prepared_frame() {
        let body = body_of(
            include_str!("app_render.rs"),
            "pub(super) fn present_frame(",
        );

        let submits = body.matches("frame.submit(").count();
        assert_eq!(
            submits, 2,
            "present_frame should submit through PreparedFrame::submit exactly \
             twice — once for the frame that got a surface and once for the \
             frame that did not — found {submits}"
        );
        assert!(
            !body.contains("encoder.finish()"),
            "present_frame finishes the encoder itself instead of handing it to \
             PreparedFrame::submit, which skips the paint callbacks' command \
             buffers entirely"
        );
    }

    /// `attachment_config` must report the pass, not a guess at it.
    ///
    /// `EguiRenderer::new` needs a real `Window`, so no host test can call the
    /// accessor. What it can catch is the mutation that matters: each field
    /// hard-coded to what `AppState` happens to pass today rather than taken from
    /// the parameter. That compiles, reads plausibly, and reports a pass layout
    /// that is right until the first caller passes something else — at which
    /// point a consumer builds a pipeline for the wrong pass and
    /// `create_render_pipeline` has no `Result` to say so in.
    #[test]
    fn attachment_config_is_built_from_new_s_own_parameters() {
        let body = body_of(include_str!("egui_renderer.rs"), "    pub fn new(");
        for (field, parameter) in [
            ("color_format", "output_color_format"),
            ("depth_format", "output_depth_format"),
            ("msaa_samples", "msaa_samples"),
        ] {
            // Field-init shorthand where the two names coincide, which is what
            // clippy asks for and what `msaa_samples` therefore has to be.
            let written = format!("{field}: {parameter}");
            let shorthand = format!("{field},");
            assert!(
                body.contains(&written) || (field == parameter && body.contains(&shorthand)),
                "AttachmentConfig::{field} is not initialised from `new`'s \
                 `{parameter}` parameter, so `attachment_config()` describes \
                 something other than the pass egui was configured for"
            );
        }
    }

    /// The pass `draw` opens must be the pass `attachment_config` describes.
    ///
    /// `draw` hard-codes `depth_stencil_attachment: None` and
    /// `resolve_target: None`, while `new` accepts *any* depth format and sample
    /// count and forwards them to egui's own pipeline. Those two are already one
    /// call-site edit away from disagreeing, and the failure mode is a pipeline
    /// that declares depth (or MSAA) for a pass that has neither: a validation
    /// error at draw time, from a `create_render_pipeline` that returns no
    /// `Result`. Publishing `attachment_config()` makes the disagreement
    /// reachable by anything building a pipeline, so pin both halves.
    #[test]
    fn the_pass_draw_opens_matches_what_attachment_config_promises() {
        let draw = body_of(include_str!("egui_renderer.rs"), "    pub fn draw(");
        assert!(
            draw.contains("depth_stencil_attachment: None"),
            "draw now attaches a depth buffer, so `AttachmentConfig::depth_format` \
             must stop being able to disagree with it"
        );
        assert!(
            draw.contains("resolve_target: None"),
            "draw now resolves MSAA, so a single-sampled `msaa_samples` no longer \
             describes this pass"
        );

        // The only production construction, and what makes the two consistent.
        let state = include_str!("app_state.rs");
        let call = state
            .split_once("EguiRenderer::new(")
            .map(|(_, rest)| rest)
            .and_then(|rest| rest.split_once(')'))
            .map(|(args, _)| args)
            .expect("app_state no longer constructs an EguiRenderer");
        assert!(
            call.contains("None") && call.contains(", 1,"),
            "app_state constructs the EguiRenderer with `{call}` — a depth format \
             or a sample count that `draw`'s render pass does not provide, so \
             egui's own pipeline no longer matches its own pass"
        );
    }

    /// A callback's own command buffer reaches the queue, on a real device.
    ///
    /// The end-to-end version of the defect above, and the only test that can
    /// distinguish "recorded" from "executed": the callback's `prepare` copies a
    /// sentinel between two buffers using a command buffer of its own, and the
    /// sentinel is only readable back if that buffer was submitted. Before the
    /// fix, `update_buffers`' return was dropped and this read zeroes.
    ///
    /// Deliberately does *not* cover the wiring inside `end_pass_and_upload` and
    /// `present_frame` — both need a real `Window` and a swapchain. That half is
    /// what `end_pass_and_upload_carries_the_callback_command_buffers` and
    /// `the_frame_path_submits_only_through_prepared_frame` pin.
    ///
    /// Needs a real adapter, so it is ignored by default. CI has no GPU:
    ///
    /// ```text
    /// cargo test -p rustdar-frontend --lib \
    ///     egui_renderer::tests::a_paint_callbacks_own_command_buffer_reaches_the_queue \
    ///     -- --ignored --exact --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_paint_callbacks_own_command_buffer_reaches_the_queue() {
        /// Anything but zero, so a buffer that was never written is telling.
        const SENTINEL: u32 = 0xC0FF_EE01;

        /// Copies [`SENTINEL`] from `source` into `landing` — in a command buffer
        /// of its own, which is the mechanism under test. Recording into the
        /// `egui_encoder` argument instead would pass even with the defect
        /// present, because that encoder was always submitted.
        struct SentinelCallback {
            source: wgpu::Buffer,
            landing: wgpu::Buffer,
        }

        impl egui_wgpu::CallbackTrait for SentinelCallback {
            fn prepare(
                &self,
                device: &wgpu::Device,
                _queue: &wgpu::Queue,
                _screen_descriptor: &ScreenDescriptor,
                _egui_encoder: &mut wgpu::CommandEncoder,
                _resources: &mut egui_wgpu::CallbackResources,
            ) -> Vec<wgpu::CommandBuffer> {
                let mut own = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("rustdar.volume.test.sentinel"),
                });
                own.copy_buffer_to_buffer(&self.source, 0, &self.landing, 0, 4);
                vec![own.finish()]
            }

            fn paint(
                &self,
                _info: egui::epaint::PaintCallbackInfo,
                _pass: &mut wgpu::RenderPass<'static>,
                _resources: &egui_wgpu::CallbackResources,
            ) {
                // Nothing to draw: this test never records egui's render pass.
            }
        }

        // Same constructor the app uses, so `WGPU_BACKEND` selects the backend
        // here too.
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("no wgpu adapter; this test is ignored by default for that reason");
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))
            .expect("could not create a device on an adapter that was found");

        let buffer = |label: &str, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 4,
                usage,
                mapped_at_creation: false,
            })
        };
        let source = buffer(
            "sentinel source",
            wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        );
        let landing = buffer(
            "sentinel landing",
            wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        );
        let readback = buffer(
            "sentinel readback",
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        queue.write_buffer(&source, 0, &SENTINEL.to_le_bytes());

        let mut renderer = Renderer::new(
            &device,
            TextureFormat::Rgba8Unorm,
            egui_wgpu::RendererOptions::default(),
        );
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [64, 64],
            pixels_per_point: 1.0,
        };
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(64.0, 64.0));
        let tris = vec![egui::ClippedPrimitive {
            clip_rect: rect,
            primitive: egui::epaint::Primitive::Callback(egui_wgpu::Callback::new_paint_callback(
                rect,
                SentinelCallback {
                    source,
                    landing: landing.clone(),
                },
            )),
        }];

        // The two production lines this test can reach: capture, then submit.
        let mut encoder = device.create_command_encoder(&Default::default());
        let user_command_buffers =
            renderer.update_buffers(&device, &queue, &mut encoder, &tris, &screen_descriptor);
        assert_eq!(
            user_command_buffers.len(),
            1,
            "egui did not gather the callback's command buffer at all, so this \
             test cannot say anything about submission"
        );
        let mut frame = PreparedFrame {
            tris,
            screen_descriptor,
            textures_to_free: Vec::new(),
            user_command_buffers,
        };
        frame.submit(&queue, encoder);

        let mut readback_encoder = device.create_command_encoder(&Default::default());
        readback_encoder.copy_buffer_to_buffer(&landing, 0, &readback, 0, 4);
        queue.submit(Some(readback_encoder.finish()));
        readback.slice(..).map_async(wgpu::MapMode::Read, |r| {
            r.expect("mapping the readback buffer failed");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("polling the device failed");

        let mapped = readback.slice(..).get_mapped_range();
        let landed = u32::from_le_bytes(
            <[u8; 4]>::try_from(&mapped[..4]).expect("the readback buffer is 4 bytes"),
        );
        assert_eq!(
            landed, SENTINEL,
            "the callback's command buffer never executed. egui returns it from \
             update_buffers and that return is not #[must_use], so dropping it \
             leaves a callback rendering nothing with no error anywhere."
        );
    }

    /// `begin_frame`'s body, read out of this file's own source.
    ///
    /// `begin_frame` needs a real `Window` and a wgpu device, so no unit test
    /// can run it; the input harness models what the rewrites do but cannot
    /// observe that this function calls them. Reading the source is the only
    /// handle there is.
    fn begin_frame_body() -> &'static str {
        body_of(include_str!("egui_renderer.rs"), "pub fn begin_frame(")
    }

    /// Both input rewrites must precede `begin_pass`, and only this file says so.
    ///
    /// Moving either call below `begin_pass` broke nothing in the suite while
    /// breaking pinch and wheel zoom in the browser — egui folds the events in
    /// during `begin_pass`, so a later rewrite is a frame too late and never
    /// reaches that frame's gestures.
    #[test]
    fn the_input_rewrites_run_before_begin_pass() {
        let body = begin_frame_body();
        let begin_pass = body
            .find("begin_pass(")
            .expect("begin_frame no longer starts a pass");

        for call in ["normalize_touch_devices(", "normalize_wheel_units("] {
            let at = body
                .find(call)
                .unwrap_or_else(|| panic!("begin_frame no longer calls {call}"));
            assert!(
                at < begin_pass,
                "{call} runs after begin_pass, so egui has already bucketed \
                 this frame's events and the rewrite lands a frame late"
            );
        }
    }

    /// The wheel rewrite must be *reachable*, and reachable on the web only.
    ///
    /// Order is not the only way to switch a call off, and the assertion above
    /// sees none of the others: pointing the `cfg` at another arch makes the
    /// rewrite dead on every target — the fix silently reverted, Firefox back to
    /// a 2.5x slow wheel — while deleting the attribute runs it natively, where
    /// winit already reports one line per notch and 20px a line against egui's
    /// native `line_scroll_speed` of 40.0 nearly halves the desktop wheel. Both
    /// leave the call exactly where it is, before `begin_pass`. So pin the
    /// guard, not just the position.
    #[test]
    fn the_wheel_rewrite_is_gated_on_wasm32_and_nothing_else() {
        let body = begin_frame_body();
        let at = body
            .find("normalize_wheel_units(")
            .expect("begin_frame no longer calls normalize_wheel_units");

        // Back up to the start of the call's own line, so the search lands on
        // the attribute above it rather than on the call's indentation.
        let line_start = body[..at].rfind('\n').map_or(0, |nl| nl + 1);
        let guard = body[..line_start]
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .expect("nothing at all precedes the wheel rewrite");

        assert_eq!(
            guard, r#"#[cfg(target_arch = "wasm32")]"#,
            "the wheel rewrite must sit directly under that cfg and no other \
             guard; found {guard:?}"
        );
    }
}
