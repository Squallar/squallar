use egui_wgpu::wgpu;

use rustdar_gpu::egui_renderer;

/// Minimum window dimension (width or height) in pixels
const MIN_SIZE: u32 = 1;

// The device-request policy — the `WEB` fork, the surface-format and limit
// choices, the present mode, and `request_device` itself — moved to
// `rustdar_gpu::device` at WO-RG. What stays here is winit-coupled
// (`request_adapter` needs the surface) or volumetric wiring (the probe and
// the error latch, which the gpu crate must not name — since WO-RV they are
// `rustdar_volumetric`'s, called from here).

pub struct AppState {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub surface: wgpu::Surface<'static>,
    pub egui_renderer: egui_renderer::EguiRenderer,
    /// The adapter the device came from, which used to be dropped here.
    ///
    /// It answers questions the device cannot: `get_texture_format_features` for
    /// any format the app might later want, and `get_capabilities` for a surface
    /// that is reconfigured after the fact. Both are needed by the 3D volume
    /// view, and re-requesting an adapter to ask is not equivalent — a second
    /// `request_adapter` may legitimately return a *different* one.
    pub adapter: wgpu::Adapter,
    /// What [`rustdar_volumetric::probe`] concluded about this device, before
    /// anything was created on it.
    ///
    /// Read it through [`rustdar_volumetric::support`] rather than directly:
    /// failures recorded since the probe ran — a rejected resource, a twice-lost
    /// surface — outrank it, and they deliberately live outside this struct
    /// because a lost surface destroys this struct. See `rustdar_volumetric::degrade`.
    pub volume_support: rustdar_volumetric::VolumeSupport,
    /// The largest side a static plan-view raster may have on this device.
    ///
    /// **A size and not a flag.** It used to be a `bool` —
    /// `max_texture_dimension_2d >= 4096` — which is the only question the
    /// whole raster path ever asked the device, and it threw the answer away:
    /// whether the adapter said 4096 or 32768, the raster was 4096. This box
    /// says 32768. [`rustdar_device_profile::budget::Budgets::raster_side_for_adapter`] is what turns
    /// the reading into a side, and why it does not simply believe it.
    ///
    /// Read off the device once, here, rather than probed per render: it is a
    /// static property of the adapter, and a render that learned it the hard
    /// way would learn it by failing to create a texture, which leaves a blank
    /// pane behind an error the latch swallows.
    ///
    /// A device that reports the GLES floor of 2048 still gets a correct
    /// picture rather than nothing, and a coarser one rather than a narrower
    /// one: the extent is the data's whatever the ceiling, so such a device
    /// draws a Doppler cut's ±300.11 km at 3.4121 px/km against the calibrated
    /// 4.4522. `rustdar_radar::types::raster_side_px` is where that trade is
    /// argued and measured. The dispatch sites carry this number into
    /// the request envelope's `side_ceiling_px` (`offload::JobRequest::geometry`).
    pub raster_side_ceiling_px: usize,
    max_surface_dimension: u32,
}

impl AppState {
    pub async fn new(
        instance: &wgpu::Instance,
        // The resolved budgets, so the long-range gate below compares the
        // device against the figure the rest of the application is spending
        // rather than against a `cfg` constant read here.
        budgets: &rustdar_device_profile::budget::Budgets,
        surface: wgpu::Surface<'static>,
        // The `Arc`, not a bare `&Window`: the wake closure built below keeps
        // a handle so egui's own repaint requests can reach the event loop —
        // see `rustdar_gpu::egui_renderer::install_repaint_wake`.
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

        // One feature, asked for only where the adapter already has it — the
        // caller computes the mask so the staging-ring coupling stays out of
        // the device fn. See `rustdar_gpu::device::request_device` for the
        // full argument (the DMA staging route, the "footgun" caveat, and why
        // WebGL2 takes `Features::empty()`).
        let features = adapter.features() & rustdar_gpu::staging_ring::STAGING_RING_FEATURE;
        let (device, queue) = rustdar_gpu::device::request_device(&adapter, features).await;

        // Before a single volume resource exists, and before anything can fail
        // asynchronously: purely limits the device already reports and format
        // features the adapter already knows. Note `device_limits` is untouched
        // and the one feature above is not consulted here — an uncompressed 3D
        // texture needs no feature, the staging ring is a route to the same
        // texture rather than a condition on having one, and the web arm's
        // `using_resolution` already lifts `max_texture_dimension_3d`.
        let volume_support = rustdar_volumetric::probe(&adapter, &device.limits());
        if let Some(why) = volume_support.reason() {
            log::info!("3D volume view unavailable: {why}");
        }
        // Installed unconditionally, including when the probe already said no:
        // the handler's other job is to keep wgpu's panicking default from
        // taking a browser tab down over an error a release build could survive.
        // Read the trade in `rustdar_volumetric::install_error_latch` before
        // moving this.
        rustdar_volumetric::install_error_latch(&device);

        let swapchain_capabilities = surface.get_capabilities(&adapter);
        let swapchain_format = rustdar_gpu::device::select_surface_format(&swapchain_capabilities);

        // Get the maximum texture dimension - wgpu requires surface dimensions to respect this
        let max_surface_dimension = device.limits().max_texture_dimension_2d;
        // The same figure, asked the question worth asking: how large a plan
        // view may this machine hold? Not "is 4096 allowed" — that reading is
        // what let a device offering 32768 draw the same 4096 as one offering
        // exactly 4096. `raster_side_for_adapter` is where the halving and the
        // no-regression floor are argued.
        let raster_side_ceiling_px = budgets.raster_side_for_adapter(max_surface_dimension);
        log::info!(
            "plan views may reach {raster_side_ceiling_px} px: this device reports \
             {max_surface_dimension} px 2D textures, and a raster is bounded by the \
             smaller of half that, the {} px ceiling this build was measured to, and \
             what the sweep's own gates carry",
            budgets.raster_side_ceiling_px,
        );

        // Clamp surface dimensions to the device's texture dimension limit
        let width = width.clamp(MIN_SIZE, max_surface_dimension);
        let height = height.clamp(MIN_SIZE, max_surface_dimension);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: swapchain_format,
            width,
            height,
            present_mode: rustdar_gpu::device::PRESENT_MODE,
            desired_maximum_frame_latency: 2,
            alpha_mode: swapchain_capabilities.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &surface_config);

        // See `rustdar_gpu::egui_renderer::install_repaint_wake`. The window
        // is held by the closure for as long as the context lives, which is
        // exactly as long as the renderer that owns it — `suspended` drops
        // both together.
        //
        // Named, not inline: the pass/attachment pin scrapes this call's
        // argument list up to its first `)`, and an inline closure's `)`
        // would truncate the scrape.
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

// The one test that lived here — the_web_limits_this_app_requests_clear_the
// _volume_probes_floor — moved into rustdar-volumetric's lib tests at WO-RV:
// it composes `rustdar_gpu::device::device_limits` with
// `rustdar_volumetric::limits_shortfall`, and that crate sees both.
