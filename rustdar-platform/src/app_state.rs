use egui_wgpu::wgpu;
use winit::window::Window;

use crate::egui_renderer;

/// Minimum window dimension (width or height) in pixels
const MIN_SIZE: u32 = 1;

/// Selects the best surface format from available capabilities
fn select_surface_format(capabilities: &wgpu::SurfaceCapabilities) -> wgpu::TextureFormat {
    if capabilities.formats.is_empty() {
        // Fallback to a common format
        wgpu::TextureFormat::Rgba8UnormSrgb
    } else {
        capabilities
            .formats
            .iter()
            .find(|&&format| format == wgpu::TextureFormat::Bgra8Unorm)
            .copied()
            .unwrap_or(capabilities.formats[0])
    }
}

pub struct AppState {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub surface: wgpu::Surface<'static>,
    pub scale_factor: f32,
    pub egui_renderer: egui_renderer::EguiRenderer,
    max_texture_dimension: u32,
}

impl AppState {
    pub async fn new(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        window: &Window,
        width: u32,
        height: u32,
    ) -> Self {
        let power_pref = wgpu::PowerPreference::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power_pref,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .expect("Failed to find an appropriate adapter");

        let features = wgpu::Features::empty();
        // Use downlevel limits to ensure WebGL2 compatibility
        // This disables compute shaders and other advanced features not needed for 2D rendering
        let limits = wgpu::Limits::downlevel_webgl2_defaults();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: features,
                required_limits: limits,
                memory_hints: Default::default(),
                trace: Default::default(),
            })
            .await
            .expect("Failed to create device");

        let swapchain_capabilities = surface.get_capabilities(&adapter);
        let swapchain_format = select_surface_format(&swapchain_capabilities);

        // Get the maximum texture dimension supported by the device
        // For WebGL2, this is typically 2048
        let max_texture_dimension = device.limits().max_texture_dimension_2d;

        // Clamp dimensions to device limits - wgpu requires this for surface configuration
        let (width, height) = {
            let width = width.clamp(MIN_SIZE, max_texture_dimension);
            let height = height.clamp(MIN_SIZE, max_texture_dimension);
            (width, height)
        };

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: swapchain_format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 0,
            alpha_mode: swapchain_capabilities.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &surface_config);

        let egui_renderer =
            egui_renderer::EguiRenderer::new(&device, surface_config.format, None, 1, window);

        let scale_factor = 1.0;

        Self {
            device,
            queue,
            surface,
            surface_config,
            egui_renderer,
            scale_factor,
            max_texture_dimension,
        }
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
        // Clamp dimensions to device limits (required by wgpu surface configuration)
        let width = width.clamp(MIN_SIZE, self.max_texture_dimension);
        let height = height.clamp(MIN_SIZE, self.max_texture_dimension);

        if width != self.surface_config.width || height != self.surface_config.height {
            log::debug!(
                "Resizing surface to {}x{} (clamped to max {})",
                width,
                height,
                self.max_texture_dimension
            );
            self.surface_config.width = width;
            self.surface_config.height = height;
            self.surface.configure(&self.device, &self.surface_config);
        }
    }
}
