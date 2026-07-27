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
}

impl PreparedFrame {
    /// Textures egui retired this frame, to be freed once the GPU is done.
    pub fn textures_to_free(&self) -> &[egui::TextureId] {
        &self.textures_to_free
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
        }
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
        self.renderer
            .update_buffers(device, queue, encoder, &tris, &screen_descriptor);

        PreparedFrame {
            tris,
            screen_descriptor,
            // Freed by the caller AFTER queue.submit(), to avoid destroying GPU
            // resources still referenced by the recorded render pass.
            textures_to_free: full_output.textures_delta.free,
        }
    }

    /// Record the render pass for an already-prepared frame.
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
    /// `begin_frame`'s body, read out of this file's own source.
    ///
    /// `begin_frame` needs a real `Window` and a wgpu device, so no unit test
    /// can run it; the input harness models what the rewrites do but cannot
    /// observe that this function calls them. Reading the source is the only
    /// handle there is.
    fn begin_frame_body() -> &'static str {
        include_str!("egui_renderer.rs")
            .split_once("pub fn begin_frame(")
            .and_then(|(_, rest)| rest.split_once("\n    }"))
            .map(|(body, _)| body)
            .expect("begin_frame is no longer a method here")
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
