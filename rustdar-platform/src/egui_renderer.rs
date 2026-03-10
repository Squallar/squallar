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
        }
    }

    pub fn handle_input(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.state.on_window_event(window, event);
        response.repaint
    }

    fn set_pixels_per_point(&mut self, v: f32) {
        self.context().set_pixels_per_point(v);
    }

    pub fn begin_frame(&mut self, window: &Window) {
        let raw_input = self.state.take_egui_input(window);
        self.state.egui_ctx().begin_pass(raw_input);
    }

    pub fn end_frame_and_draw(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        window: &Window,
        window_surface_view: &TextureView,
        screen_descriptor: ScreenDescriptor,
    ) -> Vec<egui::TextureId> {
        self.set_pixels_per_point(screen_descriptor.pixels_per_point);

        let full_output = self.state.egui_ctx().end_pass();

        // Handle platform output more carefully to avoid animation loops
        let platform_output = full_output.platform_output;

        self.state.handle_platform_output(window, platform_output);

        // Always render - the change detection was causing panels to blink
        let tris = self
            .state
            .egui_ctx()
            .tessellate(full_output.shapes, self.state.egui_ctx().pixels_per_point());
        for (id, image_delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(device, queue, *id, image_delta);
        }
        self.renderer
            .update_buffers(device, queue, encoder, &tris, &screen_descriptor);

        let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: window_surface_view,
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
        });

        self.renderer
            .render(&mut rpass.forget_lifetime(), &tris, &screen_descriptor);

        // Return textures to free — caller must free AFTER queue.submit()
        // to avoid destroying GPU resources still referenced by the
        // recorded render pass.
        full_output.textures_delta.free
    }

    /// Free textures that are no longer needed.  Call after `queue.submit()`.
    pub fn free_textures(&mut self, ids: &[egui::TextureId]) {
        for id in ids {
            self.renderer.free_texture(id);
        }
    }
}
