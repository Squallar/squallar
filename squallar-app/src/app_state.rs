use egui_wgpu::wgpu;

use squallar_gpu::egui_renderer;

/// Minimum window dimension (width or height) in pixels
const MIN_SIZE: u32 = 1;

pub struct AppState {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub surface: wgpu::Surface<'static>,
    pub egui_renderer: egui_renderer::EguiRenderer,
    pub adapter: wgpu::Adapter,
    /// What [`squallar_volumetric::probe`] concluded about this device, before anything was
    /// created on it.
    pub volume_support: squallar_volumetric::VolumeSupport,
    /// The largest side a static plan-view raster may have on this device.
    pub raster_side_ceiling_px: usize,
    max_surface_dimension: u32,
}

impl AppState {
    pub async fn new(
        instance: &wgpu::Instance,
        budgets: &squallar_device_profile::budget::Budgets,
        surface: wgpu::Surface<'static>,
        // The `Arc`, not a bare `&Window`: the wake closure built below keeps a handle so
        // egui's own repaint requests can reach the event loop — see
        // `squallar_gpu::egui_renderer::install_repaint_wake`.
        window: &crate::WindowRef,
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

        // **Which backend answered**, said out loud once. On the browser this is
        // the only place the WebGPU/WebGL2 outcome is observable: the choice is
        // made inside `create_instance` by a `requestAdapter()` nothing else
        // sees, and the two APIs render the same picture at different ceilings.
        // The rig reads this line to say which one it measured.
        let info = adapter.get_info();
        log::info!(
            "wgpu selected the {:?} backend: {} ({:?}), driver {} {}",
            info.backend,
            info.name,
            info.device_type,
            info.driver,
            info.driver_info,
        );

        // One feature, asked for only where the adapter already has it — the caller
        // computes the mask so the staging-ring coupling stays out of the device fn.
        let features = adapter.features() & squallar_gpu::staging_ring::STAGING_RING_FEATURE;
        let (device, queue) = squallar_gpu::device::request_device(&adapter, features).await;

        let volume_support = squallar_volumetric::probe(&adapter, &device.limits());
        if let Some(why) = volume_support.reason() {
            log::info!("3D volume view unavailable: {why}");
        }
        // Installed unconditionally, including when the probe already said no: the
        // handler's other job is to keep wgpu's panicking default from taking a browser tab
        // down over an error a release build could survive.
        squallar_volumetric::install_error_latch(&device);

        let swapchain_capabilities = surface.get_capabilities(&adapter);
        let swapchain_format = squallar_gpu::device::select_surface_format(&swapchain_capabilities);

        // `device.limits()` and not `adapter.limits()`: what the device got is
        // what was REQUESTED, and on WebGPU those differ by default — its
        // `maxTextureDimension2D` default is 8192 where a real adapter offers
        // far more. `squallar_gpu::device::device_limits` asks for the adapter's
        // own resolution rather than accepting the default, so these agree; the
        // adapter's figure is logged beside it so a regression to the default
        // shows up as two different numbers rather than as a quiet halving.
        let max_surface_dimension = device.limits().max_texture_dimension_2d;
        let raster_side_ceiling_px = budgets.raster_side_for_adapter(max_surface_dimension);
        log::info!(
            "plan views may reach {raster_side_ceiling_px} px: this device reports \
             {max_surface_dimension} px 2D textures (the {} px adapter offered), and a \
             raster is bounded by the smaller of half that, the {} px ceiling this build \
             was measured to, and what the sweep's own gates carry",
            adapter.limits().max_texture_dimension_2d,
            budgets.raster_side_ceiling_px,
        );

        let width = width.clamp(MIN_SIZE, max_surface_dimension);
        let height = height.clamp(MIN_SIZE, max_surface_dimension);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: swapchain_format,
            width,
            height,
            present_mode: squallar_gpu::device::PRESENT_MODE,
            desired_maximum_frame_latency: 2,
            alpha_mode: swapchain_capabilities.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &surface_config);

        // See `squallar_gpu::egui_renderer::install_repaint_wake`.
        let wake = {
            let held = Some(window.clone());
            move || crate::app::notify_redraw(&held)
        };
        let egui_renderer =
            egui_renderer::EguiRenderer::new(&device, surface_config.format, None, 1, window, wake);

        Self {
            device,
            queue,
            surface,
            surface_config,
            egui_renderer,
            adapter,
            volume_support,
            raster_side_ceiling_px,
            max_surface_dimension,
        }
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
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
