use egui_wgpu::{ScreenDescriptor, wgpu};
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::KeyCode;
use winit::window::{Window, WindowId};

#[cfg(target_os = "android")]
use rustdar_android_theme as android_theme;

use crate::WindowRef;
use crate::app_state;
use crate::constants::*;
use crate::input::InputHandler;
use chrono::TimeZone;
use rustdar_egui::{
    Gui,
    actions::GuiAction,
};
use rustdar_radar::render::{
    RadarProduct,
    ScanInfo,
    render_radar_to_image,
};
use rustdar_radar::scan;
use rustdar_radar::sites::get_radar_site;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use std::collections::HashMap;

use chrono::NaiveDateTime;
use nexrad_model::data::Scan;
use std::sync::mpsc::{Receiver, Sender};

type ScanResult = (u64, Result<(Scan, String, NaiveDateTime), String>);

/// Result from background radar rendering: (image_data, max_range_km, value_data, product, elevation, generation)
type RenderResult = (Vec<u8>, f64, Vec<f32>, RadarProduct, f32, u64);

/// Result from a background SPC outlook fetch
type OutlookResult = (OutlookDay, OutlookProduct, Result<SpcOutlook, String>);

pub struct App {
    instance: wgpu::Instance,
    state: Option<app_state::AppState>,
    window: Option<WindowRef>,
    gui: Gui,
    scan_data: Option<Arc<nexrad_model::data::Scan>>,
    input: InputHandler,
    scan_receiver: Receiver<ScanResult>,
    scan_sender: Sender<ScanResult>,
    // Channel for background radar render results
    render_result_receiver: Receiver<RenderResult>,
    render_result_sender: Sender<RenderResult>,
    // True while a background render is in progress
    render_in_flight: bool,
    // Track last rendered radar parameters to detect changes
    last_rendered: Option<(RadarProduct, f32)>,
    // Cached raw RGBA + metadata from the last successful render so we can
    // re-upload the texture instantly after suspend/resume without re-rendering.
    // Fields: (rgba_data, max_range_km, value_data, product, elevation)
    cached_render: Option<(Vec<u8>, f64, Vec<f32>, RadarProduct, f32)>,
    // Generation counter to discard stale render results after site/scan changes
    render_generation: u64,
    // Generation counter to discard stale fetch results from older requests
    fetch_generation: u64,
    // Counter to generate unique texture names
    texture_counter: u32,
    // Old textures to clean up after the next frame
    old_textures: Vec<egui::TextureHandle>,
    // Cache the detected theme to avoid calling detection every frame
    cached_dark_theme: Option<bool>,
    // Track the last applied visuals theme to skip redundant set_visuals calls
    applied_visuals_dark: Option<bool>,
    // Channel to receive theme change notifications (Android only;
    // desktop platforms use WindowEvent::ThemeChanged instead)
    #[cfg(target_os = "android")]
    theme_receiver: std::sync::mpsc::Receiver<bool>,
    // Flag for deferred exit when event_loop isn't available during redraw
    exit_requested: bool,
    // Shared Tokio runtime for all async network requests
    tokio_runtime: tokio::runtime::Runtime,
    // Shared HTTP client for overlay data fetches (SPC, etc.)
    http_client: reqwest::Client,
    // Channel for SPC outlook fetch results
    outlook_receiver: Receiver<OutlookResult>,
    outlook_sender: Sender<OutlookResult>,
    // Channel to receive GPS location updates (Android only)
    #[cfg(target_os = "android")]
    location_receiver: Option<std::sync::mpsc::Receiver<(f64, f64)>>,
    // Optional callback for back-button behavior (e.g. moveTaskToBack on Android).
    // When None, back button exits the app.
    back_handler: Option<fn()>,
    // Optional callback to query system bar insets (returns logical top, bottom, left, right).
    // Called during resumed() when the window is ready.
    #[cfg(target_os = "android")]
    insets_querier: Option<fn() -> (f32, f32, f32, f32)>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let instance = egui_wgpu::wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let input = InputHandler::new();
        let (scan_sender, scan_receiver) = std::sync::mpsc::channel();
        let (render_result_sender, render_result_receiver) = std::sync::mpsc::channel();
        let (outlook_sender, outlook_receiver) = std::sync::mpsc::channel();

        // Setup theme monitoring (Android only — desktop uses WindowEvent::ThemeChanged)
        #[cfg(target_os = "android")]
        let theme_receiver = {
            let (theme_sender, theme_receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                Self::monitor_theme_changes(theme_sender);
            });
            theme_receiver
        };

        let tokio_runtime = tokio::runtime::Runtime::new()
            .expect("Failed to create Tokio runtime");

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            instance,
            state: None,
            window: None,
            gui: Gui::new(),
            scan_data: None,
            input,
            scan_receiver,
            scan_sender,
            render_result_receiver,
            render_result_sender,
            render_in_flight: false,
            last_rendered: None,
            cached_render: None,
            render_generation: 0,
            fetch_generation: 0,
            texture_counter: 0,
            old_textures: Vec::new(),
            cached_dark_theme: None,
            applied_visuals_dark: None,
            #[cfg(target_os = "android")]
            theme_receiver,
            exit_requested: false,
            http_client,
            outlook_sender,
            outlook_receiver,
            tokio_runtime,
            #[cfg(target_os = "android")]
            location_receiver: None,
            back_handler: None,
            #[cfg(target_os = "android")]
            insets_querier: None,
        }
    }

    /// Detect system dark theme using the proper libraries for each platform
    fn detect_system_dark_theme() -> bool {
        #[cfg(target_os = "android")]
        {
            android_theme::detect_dark_theme()
        }
        
        #[cfg(not(target_os = "android"))]
        {
            // Use dark-light crate for desktop platforms (Windows, macOS, Linux, BSDs)
            match dark_light::detect() {
                Ok(dark_light::Mode::Dark) => true,
                Ok(dark_light::Mode::Light) => false,
                Ok(dark_light::Mode::Unspecified) => false, // Default to light when unspecified
                Err(_) => false, // Default to light on error
            }
        }
    }

    /// Monitor theme changes in a background thread (Android only)
    #[cfg(target_os = "android")]
    fn monitor_theme_changes(sender: std::sync::mpsc::Sender<bool>) {
        // Initial detection
        let mut last_theme = Self::detect_system_dark_theme();
        let _ = sender.send(last_theme);
        
        // Poll for changes every 2 seconds
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let current_theme = Self::detect_system_dark_theme();
            if current_theme != last_theme {
                last_theme = current_theme;
                if sender.send(current_theme).is_err() {
                    // Channel closed, exit the thread
                    break;
                }
            }
        }
    }

    /// Load scan data from the fetched radar data
    fn load_scan_data(
        &mut self,
        data: nexrad_model::data::Scan,
        site: &str,
        _requested_timestamp: chrono::NaiveDateTime,
    ) -> ScanInfo {
        let vcp_number = data.coverage_pattern_number().number();
        let num_sweeps = data.sweeps().len();
        log::info!("VCP: {}, {} sweeps", vcp_number, num_sweeps);

        // Build a map of products to their available elevation angles
        let mut product_elevations: HashMap<RadarProduct, Vec<f32>> = HashMap::new();

        for (i, sweep) in data.sweeps().iter().enumerate() {
            if let Some(first_radial) = sweep.radials().first() {
                let raw_angle = first_radial.elevation_angle_degrees();
                // Round to 1 decimal place so SAILS/MRLE repeat scans and
                // split-cuts at the same nominal angle collapse to one entry.
                let elev_angle = (raw_angle * 10.0).round() / 10.0;

                // Check which products have data at this elevation
                let mut products_found: Vec<&str> = Vec::new();
                for product in RadarProduct::all() {
                    if product.get_moment(first_radial).is_some() {
                        products_found.push(product.code());
                        product_elevations
                            .entry(*product)
                            .or_default()
                            .push(elev_angle);
                    }
                }
                log::info!(
                    "  Sweep {:2}: raw={:.2}° rounded={:.1}° radials={} products=[{}]",
                    i, raw_angle, elev_angle, sweep.radials().len(),
                    products_found.join(", ")
                );
            } else {
                log::warn!("  Sweep {:2}: no radials!", i);
            }
        }

        // Sort and deduplicate elevation angles for each product
        let product_elevations_sorted: HashMap<RadarProduct, Vec<f32>> = product_elevations
            .into_iter()
            .map(|(product, mut angles)| {
                angles.sort_by(|a, b| a.partial_cmp(b).unwrap());
                angles.dedup();
                log::info!(
                    "  {} → {} unique elevations: {:?}",
                    product.code(), angles.len(), angles
                );
                (product, angles)
            })
            .collect();

        // Get list of available products, sorted by priority (Reflectivity first)
        let mut available_products: Vec<RadarProduct> =
            product_elevations_sorted.keys().copied().collect();
        available_products.sort_by_key(|p| match p {
            RadarProduct::Reflectivity => 0,
            RadarProduct::Velocity => 1,
            RadarProduct::SpectrumWidth => 2,
            RadarProduct::DifferentialReflectivity => 3,
            RadarProduct::CorrelationCoefficient => 4,
            RadarProduct::DifferentialPhase => 5,
        });

        // Extract actual timestamp from the first radial's collection timestamp
        // In the new API, we get timestamps from individual radials
        let actual_timestamp = if let Some(first_sweep) = data.sweeps().first() {
            if let Some(first_radial) = first_sweep.radials().first() {
                let timestamp_ms = first_radial.collection_timestamp();
                // Convert from milliseconds since epoch to NaiveDateTime
                chrono::DateTime::from_timestamp_millis(timestamp_ms)
                    .map(|dt| dt.naive_utc())
                    .unwrap_or(_requested_timestamp)
            } else {
                _requested_timestamp
            }
        } else {
            _requested_timestamp
        };

        self.scan_data = Some(Arc::new(data));

        // Get radar site info, or create a default with unknown location.
        // Use a static string to avoid leaking memory via Box::leak.
        let radar_site = get_radar_site(site).unwrap_or_else(|| {
            log::warn!("Unknown radar site '{}', using fallback location", site);
            rustdar_radar::sites::RadarSite {
                name: "UNKNOWN",
                lat: 0.0,
                lon: 0.0,
                elev: 0,
            }
        });

        let status = format!(
            "Loaded {} products: {}",
            available_products.len(),
            available_products
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", ")
        );

        ScanInfo {
            site: radar_site,
            timestamp: actual_timestamp,
            vcp_number,
            available_products,
            product_elevations: product_elevations_sorted,
            status,
        }
    }

    /// Create surface and initialize AppState for a given window and dimensions.
    async fn initialize_rendering_state(
        instance: &wgpu::Instance,
        window: &WindowRef,
        width: u32,
        height: u32,
    ) -> app_state::AppState {
        let surface = instance
            .create_surface(window.clone())
            .expect("Failed to create surface!");

        app_state::AppState::new(instance, surface, window, width, height).await
    }

    fn handle_resized(&mut self, width: u32, height: u32) {
        if width > 0
            && height > 0
            && let Some(state) = self.state.as_mut()
        {
            log::info!("Window resized to {}x{}", width, height);
            state.resize_surface(width, height);
        }
    }

    fn handle_redraw(&mut self) {
        // Clear per-frame input state at the start of each frame
        self.input.clear_frame_state();

        // Check for theme changes from background thread (Android only)
        #[cfg(target_os = "android")]
        while let Ok(new_theme) = self.theme_receiver.try_recv() {
            if self.cached_dark_theme != Some(new_theme) {
                self.cached_dark_theme = Some(new_theme);
                // Theme changed, request redraw
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
        }

        // Check for GPS location updates (Android only)
        #[cfg(target_os = "android")]
        if let Some(ref receiver) = self.location_receiver {
            // Drain all pending updates, keep only the latest
            let mut latest = None;
            while let Ok(loc) = receiver.try_recv() {
                latest = Some(loc);
            }
            if let Some((lat, lon)) = latest {
                self.gui.set_user_location(lat, lon);
            }
        }

        // Check for received scan data (with generation check to discard stale results)
        if let Ok((generation, result)) = self.scan_receiver.try_recv() {
            if generation < self.fetch_generation {
                log::info!("Discarding stale scan result (gen {} < current {})", generation, self.fetch_generation);
            } else {
                match result {
                    Ok((data, site, timestamp)) => {
                        log::info!("Received scan data from background thread");
                        let scan_info = self.load_scan_data(data, &site, timestamp);
                        self.gui.set_scan_info(scan_info);
                        self.gui.set_loading_site(None);
                        // Invalidate last render so new data triggers a re-render
                        self.last_rendered = None;
                        self.cached_render = None;
                        self.render_in_flight = false;
                        // Increment render generation so any in-flight renders are discarded
                        self.render_generation += 1;
                        log::info!("Scan data loaded and UI updated");
                    }
                    Err(error_msg) => {
                        log::error!("Received error from background thread: {}", error_msg);
                        self.gui.set_error(error_msg);
                        self.gui.set_loading_site(None);
                    }
                }
            }
        }

        // Check for received SPC outlook data
        {
            let mut any_received = false;
            while let Ok((day, product, result)) = self.outlook_receiver.try_recv() {
                any_received = true;
                match result {
                    Ok(outlook) => {
                        log::info!("Received SPC outlook: {:?} {:?}", day, product);
                        self.gui.set_spc_outlook(day, product, outlook);
                    }
                    Err(e) => {
                        log::error!("SPC outlook fetch failed ({:?} {:?}): {}", day, product, e);
                    }
                }
            }
            if any_received {
                self.gui.set_spc_fetching(false);
            }
        }

        // Attempt to handle minimizing window
        if let Some(window) = self.window.as_ref()
            && let Some(min) = window.is_minimized()
            && min
        {
            log::debug!("Window is minimized");
            return;
        }

        // Check if we have the necessary resources
        // Lazily initialize rendering state on first redraw after window creation.
        // This keeps resumed() non-blocking, preventing ANR on Android foldable devices
        // during configuration changes (fold/unfold).
        if self.state.is_none() && self.window.is_some() {
            let new_state = self.window.as_ref().map(|window| {
                let size = window.inner_size();
                pollster::block_on(Self::initialize_rendering_state(
                    &self.instance,
                    window,
                    size.width.max(1),
                    size.height.max(1),
                ))
            });
            if let Some(state) = new_state {
                self.state = Some(state);

                // Restore the radar texture from cache if available.
                // This avoids a multi-second re-render after suspend/resume.
                self.restore_cached_render();
            }
        }

        if self.state.is_none() || self.window.is_none() {
            return;
        }

        // Setup egui and get GUI actions
        let (screen_descriptor, gui_actions) = self.setup_egui_frame();

        // Present the frame
        self.present_frame(screen_descriptor);

        // Handle GUI actions - no event_loop access in redraw, so exit via std::process::exit
        for action in gui_actions {
            log::info!("GUI action received: {}", action);
            self.handle_gui_action(action, None);
        }

        // Request redraw only when there is pending background work or auto-poll is active
        if self.render_in_flight || self.gui.is_auto_poll_active() {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    /// Create screen descriptor and setup egui frame.
    /// Returns the screen descriptor and any GUI actions triggered.
    ///
    /// This calculates the proper scaling factors accounting for:
    /// - OS display scaling (window.scale_factor())
    /// - Application scale factor (state.scale_factor)
    fn setup_egui_frame(&mut self) -> (ScreenDescriptor, Vec<GuiAction>) {
        let state = self.state.as_mut().unwrap();
        let window = self.window.as_ref().unwrap();

        // Calculate screen descriptor
        let window_size = window.inner_size();
        let css_to_canvas_scale_x = state.surface_config.width as f32 / window_size.width as f32;
        let pixels_per_point =
            window.scale_factor() as f32 * state.scale_factor * css_to_canvas_scale_x;

        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [state.surface_config.width, state.surface_config.height],
            pixels_per_point,
        };

        // Start egui frame
        state.egui_renderer.begin_frame(window);

        // Set theme based on OS preference
        let detected_theme = window.theme();
        let use_dark_theme = match detected_theme {
            Some(theme) => {
                // winit successfully detected the theme
                matches!(theme, winit::window::Theme::Dark)
            },
            None => {
                // winit couldn't detect theme - use cached value if available
                // Otherwise detect using platform-specific methods and cache it
                match self.cached_dark_theme {
                    Some(cached) => cached,
                    None => {
                        let detected = Self::detect_system_dark_theme();
                        self.cached_dark_theme = Some(detected);
                        detected
                    }
                }
            }
        };
        
        // Only update egui visuals when the theme actually changes
        if self.applied_visuals_dark != Some(use_dark_theme) {
            self.applied_visuals_dark = Some(use_dark_theme);
            let visuals = if use_dark_theme {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            };
            state
                .egui_renderer
                .context()
                .set_visuals(visuals);
        }

        let gui_action = self.gui.ui(state.egui_renderer.context());

        // Clean up old textures from previous frame
        // This allows the GPU to finish using them before we drop them
        self.old_textures.clear();

        // Poll for completed background render results (with generation check)
        if let Ok((image_data, max_range_km, value_data, product, elevation, generation)) =
            self.render_result_receiver.try_recv()
        {
            self.render_in_flight = false;

            if generation < self.render_generation {
                log::info!("Discarding stale render result (gen {} < current {})", generation, self.render_generation);
            } else if self.gui.get_rendering_params().is_some() {
                // Save the old texture to be cleaned up after this frame completes
                if let Some((old_texture, _, _, _, _)) = self.gui.take_radar_image() {
                    self.old_textures.push(old_texture);
                }

                self.texture_counter += 1;
                let ctx = state.egui_renderer.context();
                let color_image =
                    egui::ColorImage::from_rgba_unmultiplied([1800, 1800], &image_data);
                let texture_name = format!("radar_image_{}", self.texture_counter);
                let texture = ctx.load_texture(
                    texture_name,
                    color_image,
                    egui::TextureOptions::NEAREST,
                );

                // Cache the raw image data for fast restore after suspend/resume
                self.cached_render = Some((
                    image_data,
                    max_range_km,
                    value_data.clone(),
                    product,
                    elevation,
                ));

                if let Some(scan_info) = self.gui.get_scan_info() {
                    self.gui.set_radar_image(
                        texture,
                        scan_info.site.lat,
                        scan_info.site.lon,
                        max_range_km,
                        value_data,
                    );
                }

                self.last_rendered = Some((product, elevation));
            }
        }

        // Check if we need to spawn a new background render
        if let Some((product, elevation)) = self.gui.get_rendering_params() {
            let needs_render = self
                .last_rendered
                .map(|(last_prod, last_elev)| {
                    last_prod != product || (last_elev - elevation).abs() > 0.01
                })
                .unwrap_or(true);

            if needs_render && !self.render_in_flight {
                if let Some(data) = &self.scan_data {
                    if let Some(scan_info) = self.gui.get_scan_info() {
                        log::info!("Spawning background render: {:?} at {:.1}°", product, elevation);
                        let data = Arc::clone(data);
                        let lat = scan_info.site.lat;
                        let lon = scan_info.site.lon;
                        let sender = self.render_result_sender.clone();
                        let generation = self.render_generation;
                        let window = self.window.clone();

                        std::thread::spawn(move || {
                            if let Some((image, range, values)) =
                                render_radar_to_image(&data, elevation, product, lat, lon)
                            {
                                let _ = sender.send((image, range, values, product, elevation, generation));
                            }
                            // Wake the event loop so the result is picked up promptly
                            if let Some(window) = window {
                                window.request_redraw();
                            }
                        });
                        self.render_in_flight = true;
                    }
                }
            }
        } else {
            // No rendering params available, clear any existing image
            self.gui.clear_radar_image();
            self.last_rendered = None;
        }

        (screen_descriptor, gui_action)
    }

    /// Restore the radar image from cached raw RGBA data.
    ///
    /// Called after wgpu state is recreated (suspend/resume or surface loss) to
    /// avoid a multi-second background re-render.  Re-uploads the cached pixel
    /// data as a new GPU texture instantly.
    fn restore_cached_render(&mut self) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let Some((ref image_data, max_range_km, ref value_data, product, elevation)) =
            self.cached_render
        else {
            return;
        };
        let Some(scan_info) = self.gui.get_scan_info() else {
            return;
        };

        log::info!(
            "Restoring cached radar image ({:?} at {:.1}°) from memory",
            product,
            elevation
        );

        self.texture_counter += 1;
        let ctx = state.egui_renderer.context();
        let color_image = egui::ColorImage::from_rgba_unmultiplied([1800, 1800], image_data);
        let texture_name = format!("radar_image_{}", self.texture_counter);
        let texture = ctx.load_texture(texture_name, color_image, egui::TextureOptions::NEAREST);

        self.gui.set_radar_image(
            texture,
            scan_info.site.lat,
            scan_info.site.lon,
            max_range_km,
            value_data.clone(),
        );
        self.last_rendered = Some((product, elevation));
    }

    /// Get the current surface texture, handling common errors.
    /// Try to acquire the next surface texture for rendering.
    /// Returns `None` if the surface is temporarily unavailable (e.g. during
    /// a display change).  Returns `Err(true)` via the second element when
    /// the surface is *lost* and the caller must recreate rendering state.
    fn get_surface_texture(surface: &wgpu::Surface) -> (Option<wgpu::SurfaceTexture>, bool) {
        match surface.get_current_texture() {
            Ok(texture) => (Some(texture), false),
            Err(wgpu::SurfaceError::Outdated) => {
                log::warn!("wgpu surface outdated, skipping frame");
                (None, false)
            }
            Err(wgpu::SurfaceError::Lost) => {
                log::warn!("wgpu surface lost (display change?), will recreate state");
                (None, true)
            }
            Err(err) => {
                log::error!("Surface error: {:?}", err);
                (None, false)
            }
        }
    }

    fn present_frame(&mut self, screen_descriptor: ScreenDescriptor) {
        let state = self.state.as_mut().unwrap();
        let window = self.window.as_ref().unwrap();

        let (surface_texture, surface_lost) = Self::get_surface_texture(&state.surface);
        if surface_lost {
            // Surface is irrecoverably lost (e.g. display changed on a foldable).
            // Drop the entire rendering state so the next handle_redraw() lazily
            // recreates it with a fresh surface.  Keep cached_render so the radar
            // image can be restored instantly.
            self.old_textures.clear();
            self.last_rendered = None;
            self.gui.clear_graphics_state();
            self.state = None;
            self.applied_visuals_dark = None;
            return;
        }
        let Some(surface_texture) = surface_texture else {
            return;
        };

        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        // Render egui
        state.egui_renderer.end_frame_and_draw(
            &state.device,
            &state.queue,
            &mut encoder,
            window,
            &surface_view,
            screen_descriptor,
        );

        state.queue.submit(Some(encoder.finish()));
        surface_texture.present();
    }

    /// Convert a local NaiveDateTime to a UTC NaiveDateTime.
    /// Uses `.latest()` to pick the later valid interpretation during DST gaps,
    /// which is less surprising than silently falling back to the current time.
    fn local_to_utc(timestamp: NaiveDateTime) -> NaiveDateTime {
        let local_dt = chrono::Local
            .from_local_datetime(&timestamp)
            .latest()
            .unwrap_or_else(chrono::Local::now);
        local_dt.with_timezone(&chrono::Utc).naive_utc()
    }

    fn handle_gui_action(&mut self, action: GuiAction, event_loop: Option<&ActiveEventLoop>) {
        match action {
            GuiAction::FetchRadarScan(radar_config) => {
                log::info!(
                    "Fetch radar scan requested: {} @ {} (local)",
                    radar_config.site,
                    radar_config.timestamp
                );
                let utc_timestamp = Self::local_to_utc(radar_config.timestamp);
                self.spawn_fetch(radar_config.site, utc_timestamp);
            }
            GuiAction::CheckForNewScans(radar_config) => {
                log::info!(
                    "Check for new scans: {} @ {} (local)",
                    radar_config.site,
                    radar_config.timestamp
                );

                let utc_timestamp = Self::local_to_utc(radar_config.timestamp);
                let current_scan_timestamp = self.gui.get_scan_info().map(|info| info.timestamp);

                self.fetch_generation += 1;
                let generation = self.fetch_generation;
                let site = radar_config.site.clone();
                let window = self.window.clone();
                let sender = self.scan_sender.clone();

                self.tokio_runtime.spawn(async move {
                    match scan::check_and_fetch_latest(&site, &utc_timestamp.date(), current_scan_timestamp).await {
                        Ok(Some((data, timestamp))) => {
                            let _ = sender.send((generation, Ok((data, site, timestamp))));
                        }
                        Ok(None) => { /* already latest or no data */ }
                        Err(e) => {
                            log::error!("Failed to check for new scans: {:?}", e);
                        }
                    }
                    if let Some(w) = window { w.request_redraw(); }
                });
            }
            GuiAction::SetScanInfo(scan_info) => {
                log::info!(
                    "Setting scan info: {} @ {}",
                    scan_info.site.name,
                    scan_info.timestamp
                );
                self.gui.set_scan_info(scan_info);
            }
            GuiAction::SwitchRadarSite(site) => {
                log::info!("Switch radar site requested: {}", site);
                
                let mut new_config = self.gui.get_radar_config().clone();
                new_config.site = site.clone();
                self.gui.set_radar_config(new_config.clone());
                self.gui.set_loading_site(Some(site.clone()));
                
                let utc_timestamp = Self::local_to_utc(new_config.timestamp);
                self.spawn_fetch(site, utc_timestamp);
            }
            GuiAction::Exit => {
                self.request_exit(event_loop);
            }
            GuiAction::FetchSpcOutlook { day, products } => {
                log::info!("Fetching SPC outlooks for {:?}: {:?}", day, products);
                self.gui.set_spc_fetching(true);
                let client = self.http_client.clone();
                let sender = self.outlook_sender.clone();
                let window = self.window.clone();
                for product in products {
                    let client = client.clone();
                    let sender = sender.clone();
                    let window = window.clone();
                    self.tokio_runtime.spawn(async move {
                        let result =
                            rustdar_overlays::spc::fetch::fetch_outlook(&client, day, product)
                                .await
                                .map_err(|e| format!("{e}"));
                        let _ = sender.send((day, product, result));
                        if let Some(w) = window {
                            w.request_redraw();
                        }
                    });
                }
            }
            GuiAction::RefreshSpcOutlooks => {
                let day = self.gui.layers().spc_day;
                let products = self.gui.layers().enabled_spc_products();
                if !products.is_empty() {
                    self.handle_gui_action(
                        GuiAction::FetchSpcOutlook { day, products },
                        event_loop,
                    );
                }
            }
        }
    }

    /// Spawn an async radar data fetch on the background runtime.
    /// Handles generation tracking, result sending, and redraw requests.
    fn spawn_fetch(&mut self, site: String, timestamp: NaiveDateTime) {
        self.fetch_generation += 1;
        let generation = self.fetch_generation;
        let window = self.window.clone();
        let sender = self.scan_sender.clone();
        self.tokio_runtime.spawn(async move {
            log::info!("Fetching {} @ {} UTC", site, timestamp);
            let msg = match scan::get_scan(&site, timestamp).await {
                Ok(data) => {
                    log::info!("Fetched scan: {} @ {}", site, timestamp);
                    Ok((data, site, timestamp))
                }
                Err(e) => {
                    let err = format!("Failed to fetch radar scan: {:?}", e);
                    log::error!("{}", err);
                    Err(err)
                }
            };
            let _ = sender.send((generation, msg));
            if let Some(w) = window { w.request_redraw(); }
        });
    }

    /// Request application exit - handles both GUI and keyboard exit requests
    fn request_exit(&mut self, event_loop: Option<&ActiveEventLoop>) {
        if let Some(event_loop) = event_loop {
            log::info!("Exiting application");
            event_loop.exit();
            // On Android, event_loop.exit() just stops the event loop but
            // leaves the NativeActivity window visible as a grey screen.
            // Terminate the process to fully close the application.
            #[cfg(target_os = "android")]
            std::process::exit(0);
        } else {
            // Defer exit until the next event where event_loop is available
            self.exit_requested = true;
        }
    }

    /// Set a callback to handle the back button (e.g. moveTaskToBack on Android).
    /// When set, pressing back invokes this instead of exiting.
    pub fn set_back_handler(&mut self, handler: fn()) {
        self.back_handler = Some(handler);
    }

    /// Set a receiver for GPS location updates (Android only).
    /// Called from the Android entry point after starting a location polling thread.
    #[cfg(target_os = "android")]
    pub fn set_location_receiver(&mut self, receiver: std::sync::mpsc::Receiver<(f64, f64)>) {
        self.location_receiver = Some(receiver);
    }

    /// Set safe area insets in logical pixels (top, bottom, left, right).
    /// Called from the Android entry point to avoid drawing under system bars.
    #[cfg(target_os = "android")]
    pub fn set_safe_area_insets(&mut self, top: f32, bottom: f32, left: f32, right: f32) {
        self.gui.set_safe_area_insets(top, bottom, left, right);
    }

    /// Set a callback that queries system bar insets.
    /// Called lazily during resumed() when the window is available.
    #[cfg(target_os = "android")]
    pub fn set_insets_querier(&mut self, querier: fn() -> (f32, f32, f32, f32)) {
        self.insets_querier = Some(querier);
    }

    fn handle_input_events(&mut self, event_loop: &ActiveEventLoop) {
        if self.input.key_pressed(KeyCode::Escape) {
            self.request_exit(Some(event_loop));
        }

        if self.input.back_pressed() {
            if let Some(handler) = self.back_handler {
                handler();
            } else {
                self.request_exit(Some(event_loop));
            }
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop
            .create_window(Window::default_attributes())
            .unwrap();

        let window = Arc::new(window);
        let _ = window.request_inner_size(PhysicalSize::new(RENDER_WIDTH, RENDER_HEIGHT));
        self.window = Some(window.clone());

        // Rendering state is initialized lazily in handle_redraw().
        // This keeps resumed() fast on Android, preventing ANRs during
        // configuration changes (e.g. folding/unfolding the device).
        window.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        log::info!("App resumed");
        self.create_window(event_loop);

        // Query system bar insets now that the window is ready.
        // On config changes (fold/unfold) the insets may differ.
        #[cfg(target_os = "android")]
        if let Some(querier) = self.insets_querier {
            let (top, bottom, left, right) = querier();
            self.gui.set_safe_area_insets(top, bottom, left, right);
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        log::info!("App suspended — clearing graphics state");
        self.old_textures.clear();
        self.last_rendered = None;
        self.texture_counter = 0;
        self.gui.clear_graphics_state();        // Keep cached_render intact so we can re-upload the texture
        // immediately on resume without re-rendering.        // Clear both window and state so resumed() creates fresh ones.
        // Leaving state alive would keep a wgpu surface referencing the destroyed window.
        self.window = None;
        self.state = None;
        self.applied_visuals_dark = None;
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // Update input handler — pass &WindowEvent directly (no clone needed)
        if self.input.process_event(&event) {
            self.handle_input_events(event_loop);
        }

        // Let egui process the event, but only if state exists
        let mut needs_repaint = false;
        if let (Some(state), Some(window)) = (self.state.as_mut(), self.window.as_ref()) {
            needs_repaint = state.egui_renderer.handle_input(window, &event);
        }

        match event {
            WindowEvent::CloseRequested => {
                self.request_exit(Some(event_loop));
            }
            WindowEvent::RedrawRequested => {
                self.handle_redraw();
                // Check for deferred exit (set during redraw when event_loop is unavailable)
                if self.exit_requested {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(new_size) => {
                self.handle_resized(new_size.width, new_size.height);
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::ThemeChanged(_theme) => {
                // Theme changed, clear cache so we re-detect on next frame
                self.cached_dark_theme = None;
                // Request redraw to update UI
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            _ => {
                // For other events, request redraw only if egui needs it
                if needs_repaint && let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
        }
    }
}
