use egui_wgpu::wgpu;
use winit::window::Window;

use crate::egui_renderer;

/// Minimum window dimension (width or height) in pixels
const MIN_SIZE: u32 = 1;

/// Selects the best surface format from available capabilities
fn select_surface_format(capabilities: &wgpu::SurfaceCapabilities) -> wgpu::TextureFormat {
    if capabilities.formats.is_empty() {
        // Fallback to a common format
        return wgpu::TextureFormat::Rgba8UnormSrgb;
    }
    // WebGL2 presents the canvas through a plain, non-sRGB default framebuffer.
    // Configuring an sRGB swapchain on top of that makes the browser apply the
    // transfer function a second time over the one egui has already baked into
    // its vertex colours; the failure is washed-out output, not a validation
    // error, so nothing reports it. Native has a real sRGB-capable swapchain and
    // keeps the existing preference untouched.
    #[cfg(target_arch = "wasm32")]
    if let Some(&format) = capabilities.formats.iter().find(|f| !f.is_srgb()) {
        return format;
    }
    capabilities
        .formats
        .iter()
        .find(|&&format| format == wgpu::TextureFormat::Bgra8Unorm)
        .copied()
        .unwrap_or(capabilities.formats[0])
}

/// The limit set to request from the adapter.
///
/// Native asks for the adapter's real limits so desktop GPUs can use textures
/// far larger than any portable floor. WebGL2 cannot express most of wgpu's
/// limit set at all, so requesting the adapter's limits verbatim there fails the
/// device request outright. The web arm starts from the WebGL2 downlevel
/// defaults and lifts *only* the resolution back to what the adapter actually
/// reports — `max_texture_dimension_2d` is the one limit the overlay planner
/// reads, and pinning it to the 2048 spec floor would cost resolution on every
/// browser that offers more.
#[cfg(not(target_arch = "wasm32"))]
fn device_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    adapter.limits()
}

/// See the native variant above.
#[cfg(target_arch = "wasm32")]
fn device_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits())
}

/// How the surface presents.
///
/// `Fifo` is the only present mode WebGL2 actually has — the browser paces
/// presentation through `requestAnimationFrame` and wgpu's other modes have
/// nothing to map onto. Naming it explicitly keeps the web build off
/// `AutoVsync`'s negotiation, which has no meaningful choice to make here.
#[cfg(not(target_arch = "wasm32"))]
const PRESENT_MODE: wgpu::PresentMode = wgpu::PresentMode::AutoVsync;
/// See the native variant above.
#[cfg(target_arch = "wasm32")]
const PRESENT_MODE: wgpu::PresentMode = wgpu::PresentMode::Fifo;

pub struct AppState {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub surface: wgpu::Surface<'static>,
    pub egui_renderer: egui_renderer::EguiRenderer,
    max_surface_dimension: u32,
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
        // Native takes the adapter's actual limits so it is not held to a
        // portable floor; the web arm reconciles them with what WebGL2 can
        // express. See `device_limits`.
        let limits = device_limits(&adapter);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: features,
                required_limits: limits,
                memory_hints: Default::default(),
                experimental_features: Default::default(),
                trace: Default::default(),
            })
            .await
            .expect("Failed to create device");

        let swapchain_capabilities = surface.get_capabilities(&adapter);
        let swapchain_format = select_surface_format(&swapchain_capabilities);

        // Get the maximum texture dimension - wgpu requires surface dimensions to respect this
        let max_surface_dimension = device.limits().max_texture_dimension_2d;

        // Clamp surface dimensions to the device's texture dimension limit
        let width = width.clamp(MIN_SIZE, max_surface_dimension);
        let height = height.clamp(MIN_SIZE, max_surface_dimension);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: swapchain_format,
            width,
            height,
            present_mode: PRESENT_MODE,
            desired_maximum_frame_latency: 2,
            alpha_mode: swapchain_capabilities.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &surface_config);

        let egui_renderer =
            egui_renderer::EguiRenderer::new(&device, surface_config.format, None, 1, window);

        Self {
            device,
            queue,
            surface,
            surface_config,
            egui_renderer,
            max_surface_dimension,
        }
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
        // Clamp to device's maximum texture dimension (required by wgpu)
        let width = width.clamp(MIN_SIZE, self.max_surface_dimension);
        let height = height.clamp(MIN_SIZE, self.max_surface_dimension);

        if width != self.surface_config.width || height != self.surface_config.height {
            log::debug!("Resizing surface to {}x{}", width, height);
            self.surface_config.width = width;
            self.surface_config.height = height;
            self.surface.configure(&self.device, &self.surface_config);
        }
    }
}
