use egui_wgpu::{ScreenDescriptor, wgpu};
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::KeyCode;
use winit::window::{Window, WindowId};

use crate::WindowRef;
use crate::app_state;
use crate::constants::*;
use crate::input::InputHandler;
use crate::radar_renderer;
use chrono::TimeZone;
use rustdar_egui::{
    Gui,
    actions::{GuiAction, RadarProduct, ScanInfo},
};
use rustdar_radar::scan;
use rustdar_radar::sites::get_radar_site;
use std::collections::HashMap;

use chrono::NaiveDateTime;
use nexrad::model::DataFile;
use std::sync::mpsc::{Receiver, Sender};

type ScanResult = Result<(DataFile, String, NaiveDateTime), String>;

pub struct App {
    instance: wgpu::Instance,
    state: Option<app_state::AppState>,
    window: Option<WindowRef>,
    gui: Gui,
    scan_data: Option<nexrad::model::DataFile>,
    input: InputHandler,
    scan_receiver: Receiver<ScanResult>,
    scan_sender: Sender<ScanResult>,
    // Track last rendered radar parameters to detect changes
    last_rendered: Option<(RadarProduct, f32)>,
    // Counter to generate unique texture names
    texture_counter: u32,
    // Old textures to clean up after the next frame
    old_textures: Vec<egui::TextureHandle>,
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

        Self {
            instance,
            state: None,
            window: None,
            gui: Gui::new(),
            scan_data: None,
            input,
            scan_receiver,
            scan_sender,
            last_rendered: None,
            texture_counter: 0,
            old_textures: Vec::new(),
        }
    }

    /// Load scan data from the fetched radar data
    fn load_scan_data(
        &mut self,
        data: nexrad::model::DataFile,
        site: &str,
        _requested_timestamp: chrono::NaiveDateTime,
    ) -> ScanInfo {
        // Build a map of products to their available elevation angles
        let mut product_elevations: HashMap<RadarProduct, Vec<f32>> = HashMap::new();

        for radials in data.elevation_scans().values() {
            if let Some(first_radial) = radials.first() {
                let elev_angle = first_radial.header().elev();

                // Check which products have data at this elevation
                if first_radial.reflectivity_data().is_some() {
                    product_elevations
                        .entry(RadarProduct::Reflectivity)
                        .or_default()
                        .push(elev_angle);
                }
                if first_radial.velocity_data().is_some() {
                    product_elevations
                        .entry(RadarProduct::Velocity)
                        .or_default()
                        .push(elev_angle);
                }
                if first_radial.sw_data().is_some() {
                    product_elevations
                        .entry(RadarProduct::SpectrumWidth)
                        .or_default()
                        .push(elev_angle);
                }
                if first_radial.zdr_data().is_some() {
                    product_elevations
                        .entry(RadarProduct::DifferentialReflectivity)
                        .or_default()
                        .push(elev_angle);
                }
                if first_radial.rho_data().is_some() {
                    product_elevations
                        .entry(RadarProduct::CorrelationCoefficient)
                        .or_default()
                        .push(elev_angle);
                }
                if first_radial.phi_data().is_some() {
                    product_elevations
                        .entry(RadarProduct::DifferentialPhase)
                        .or_default()
                        .push(elev_angle);
                }
                if first_radial.cfp_data().is_some() {
                    product_elevations
                        .entry(RadarProduct::ClutterFilterPower)
                        .or_default()
                        .push(elev_angle);
                }
            }
        }

        // Sort and deduplicate elevation angles for each product
        let product_elevations_sorted: HashMap<RadarProduct, Vec<f32>> = product_elevations
            .into_iter()
            .map(|(product, mut angles)| {
                angles.sort_by(|a, b| a.partial_cmp(b).unwrap());
                angles.dedup();
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
            RadarProduct::ClutterFilterPower => 6,
        });

        // Extract actual timestamp from the volume header
        let volume_header = data.volume_header();
        let file_date = volume_header.file_date(); // Days since January 1, 1970 (day 1 = Jan 1, 1970)
        let file_time = volume_header.file_time(); // Milliseconds since midnight (UTC)

        // Convert days since January 1, 1970 to NaiveDate
        // Note: The NEXRAD format uses day 1 = Jan 1, 1970, so we subtract 1
        let unix_epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        let scan_date = unix_epoch + chrono::Duration::days((file_date - 1) as i64);

        // Convert milliseconds since midnight to time
        let total_seconds = file_time / 1000;
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;
        let millis = file_time % 1000;

        let scan_time = chrono::NaiveTime::from_hms_milli_opt(hours, minutes, seconds, millis)
            .unwrap_or_else(|| chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap());

        // This timestamp is in UTC
        let actual_timestamp = chrono::NaiveDateTime::new(scan_date, scan_time);

        self.scan_data = Some(data);

        // Get radar site info, or create a default with unknown location
        let radar_site = get_radar_site(site).unwrap_or_else(|| rustdar_radar::sites::RadarSite {
            name: Box::leak(site.to_string().into_boxed_str()),
            lat: 0.0,
            lon: 0.0,
            elev: 0,
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

    async fn set_window(&mut self, window: Window) {
        let window = Arc::new(window);

        let _ = window.request_inner_size(PhysicalSize::new(RENDER_WIDTH, RENDER_HEIGHT));
        let (initial_width, initial_height) = (RENDER_WIDTH, RENDER_HEIGHT);

        let state = Self::initialize_rendering_state(
            &self.instance,
            &window,
            initial_width,
            initial_height,
        )
        .await;

        self.window.get_or_insert(window);
        self.state.get_or_insert(state);
    }

    fn handle_resized(&mut self, width: u32, height: u32) {
        if width > 0
            && height > 0
            && let Some(state) = self.state.as_mut()
        {
            state.resize_surface(width, height);
        }
    }

    fn handle_redraw(&mut self) {
        // Clear per-frame input state at the start of each frame
        self.input.clear_frame_state();

        // Check for received scan data
        if let Ok(result) = self.scan_receiver.try_recv() {
            match result {
                Ok((data, site, timestamp)) => {
                    log::info!("📥 Received scan data from background thread");
                    let scan_info = self.load_scan_data(data, &site, timestamp);
                    self.gui.set_scan_info(scan_info);
                    log::info!("✅ Scan data loaded and UI updated");
                }
                Err(error_msg) => {
                    log::error!("📥 Received error from background thread: {}", error_msg);
                    self.gui.set_error(error_msg);
                }
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

        // Request another redraw for continuous rendering
        if let Some(window) = &self.window {
            window.request_redraw();
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

        // Set dark theme
        state
            .egui_renderer
            .context()
            .set_visuals(egui::Visuals::dark());

        let gui_action = self.gui.ui(state.egui_renderer.context());

        // Clean up old textures from previous frame
        // This allows the GPU to finish using them before we drop them
        self.old_textures.clear();

        // Check if we need to render radar data
        if let Some((product, elevation)) = self.gui.get_rendering_params() {
            // Check if parameters changed since last render
            let needs_render = self
                .last_rendered
                .map(|(last_prod, last_elev)| {
                    last_prod != product || (last_elev - elevation).abs() > 0.01
                })
                .unwrap_or(true);

            if needs_render {
                log::info!("Rendering radar: {:?} at {:.1}°", product, elevation);

                // Render the radar data to an image
                let radar_render_result = if let Some(data) = &self.scan_data {
                    radar_renderer::render_radar_to_image(data, elevation, product)
                } else {
                    None
                };

                if let Some((image_data, max_range_km, value_data)) = radar_render_result {
                    // Save the old texture to be cleaned up after this frame completes
                    if let Some((old_texture, _, _, _, _)) = self.gui.take_radar_image() {
                        self.old_textures.push(old_texture);
                    }

                    // Increment texture counter to get unique name
                    self.texture_counter += 1;

                    // Create an egui texture from the image data
                    let ctx = state.egui_renderer.context();

                    let color_image =
                        egui::ColorImage::from_rgba_unmultiplied([1800, 1800], &image_data);

                    // Use a unique texture name to avoid texture conflicts
                    let texture_name = format!("radar_image_{}", self.texture_counter);
                    let texture = ctx.load_texture(
                        texture_name,
                        color_image,
                        egui::TextureOptions::NEAREST, // Use nearest-neighbor filtering for crisp radar data
                    );

                    // Get radar site location from scan info
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
                } else {
                    log::warn!("Failed to render radar data");
                }
            }
        } else {
            // No rendering params available, clear any existing image
            self.gui.clear_radar_image();
            self.last_rendered = None;
        }

        (screen_descriptor, gui_action)
    }

    /// Get the current surface texture, handling common errors.
    /// Returns None if the surface is outdated or encounters an error.
    fn get_surface_texture(surface: &wgpu::Surface) -> Option<wgpu::SurfaceTexture> {
        match surface.get_current_texture() {
            Ok(texture) => Some(texture),
            Err(wgpu::SurfaceError::Outdated) => {
                log::warn!("wgpu surface outdated");
                None
            }
            Err(err) => {
                log::error!("Surface error: {:?}", err);
                None
            }
        }
    }

    fn present_frame(&mut self, screen_descriptor: ScreenDescriptor) {
        let state = self.state.as_mut().unwrap();
        let window = self.window.as_ref().unwrap();

        let Some(surface_texture) = Self::get_surface_texture(&state.surface) else {
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

    fn handle_gui_action(&mut self, action: GuiAction, event_loop: Option<&ActiveEventLoop>) {
        match action {
            GuiAction::FetchRadarScan(radar_config) => {
                log::info!(
                    "🚀 Fetch radar scan requested: {} @ {} (local)",
                    radar_config.site,
                    radar_config.timestamp
                );

                // Convert local timestamp to UTC for S3 search
                let local_dt = chrono::Local
                    .from_local_datetime(&radar_config.timestamp)
                    .single()
                    .unwrap_or_else(chrono::Local::now);
                // Convert to UTC (this actually converts the time, not just strips timezone)
                let utc_dt = local_dt.with_timezone(&chrono::Utc);
                let utc_timestamp = utc_dt.naive_utc();

                log::info!(
                    "Converted to UTC: {} @ {} UTC",
                    radar_config.site,
                    utc_timestamp
                );

                // Spawn async task to fetch radar data
                let site = radar_config.site.clone();
                let timestamp = utc_timestamp;
                let window = self.window.clone();
                let sender = self.scan_sender.clone();

                {
                    log::info!("Spawning background thread for radar fetch...");
                    // For native platforms, spawn a Tokio runtime in a new thread
                    std::thread::spawn(move || {
                        log::info!("Background thread started, creating Tokio runtime...");
                        let rt =
                            tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
                        log::info!("Starting fetch for {} @ {} UTC", site, timestamp.date());
                        let result = rt.block_on(scan::get_scan(&site, timestamp));
                        match result {
                            Ok(data) => {
                                log::info!(
                                    "✅ Successfully fetched radar scan from {} @ {}",
                                    site,
                                    timestamp
                                );
                                // Send the data back to the main thread
                                if let Err(e) = sender.send(Ok((data, site.clone(), timestamp))) {
                                    log::error!(
                                        "❌ Failed to send scan data to main thread: {:?}",
                                        e
                                    );
                                }
                            }
                            Err(e) => {
                                let error_msg = format!("Failed to fetch radar scan: {:?}", e);
                                log::error!("❌ {}", error_msg);
                                // Send error to main thread
                                let _ = sender.send(Err(error_msg));
                            }
                        }
                        // Request redraw to update UI
                        if let Some(window) = window {
                            log::info!("Requesting window redraw...");
                            window.request_redraw();
                        }
                    });
                    log::info!("Background thread spawned");
                }
            }
            GuiAction::CheckForNewScans(radar_config) => {
                log::info!(
                    "🔍 Check for new scans requested: {} @ {} (local)",
                    radar_config.site,
                    radar_config.timestamp
                );

                // Convert local timestamp to UTC for S3 search
                let local_dt = chrono::Local
                    .from_local_datetime(&radar_config.timestamp)
                    .single()
                    .unwrap_or_else(chrono::Local::now);
                let utc_dt = local_dt.with_timezone(&chrono::Utc);
                let utc_timestamp = utc_dt.naive_utc();

                // Get current scan timestamp for comparison
                let current_scan_timestamp = self.gui.get_scan_info().map(|info| info.timestamp);

                // Spawn async task to check for new radar files
                let site = radar_config.site.clone();
                let timestamp = utc_timestamp;
                let window = self.window.clone();

                {
                    let scan_sender = self.scan_sender.clone();
                    std::thread::spawn(move || {
                        let rt =
                            tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
                        let result = rt.block_on(scan::check_latest_scan(&site, &timestamp.date()));
                        match result {
                            Ok(Some(latest_timestamp)) => {
                                log::info!(
                                    "📊 Latest scan available: {} @ {}",
                                    site,
                                    latest_timestamp
                                );
                                // Check if we need to fetch this scan
                                let should_fetch =
                                    if let Some(current_timestamp) = current_scan_timestamp {
                                        latest_timestamp > current_timestamp
                                    } else {
                                        true // No scan data yet, fetch
                                    };

                                if should_fetch {
                                    log::info!("🔄 Fetching newer scan...");
                                    let fetch_result =
                                        rt.block_on(scan::get_scan(&site, latest_timestamp));
                                    match fetch_result {
                                        Ok(data) => {
                                            if let Err(e) = scan_sender.send(Ok((
                                                data,
                                                site.clone(),
                                                latest_timestamp,
                                            ))) {
                                                log::error!(
                                                    "❌ Failed to send scan data to main thread: {:?}",
                                                    e
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            let error_msg =
                                                format!("Failed to fetch radar scan: {:?}", e);
                                            log::error!("❌ {}", error_msg);
                                            let _ = scan_sender.send(Err(error_msg));
                                        }
                                    }
                                } else {
                                    log::info!("📊 Already have latest scan");
                                }
                            }
                            Ok(None) => {
                                log::info!(
                                    "📊 No scans found for {} on {}",
                                    site,
                                    timestamp.date()
                                );
                            }
                            Err(e) => {
                                log::error!("❌ Failed to check for new scans: {:?}", e);
                            }
                        }
                        // Request redraw to update UI
                        if let Some(window) = window {
                            window.request_redraw();
                        }
                    });
                }
            }
            GuiAction::SetScanInfo(scan_info) => {
                log::info!(
                    "Setting scan info: {} @ {}",
                    scan_info.site.name,
                    scan_info.timestamp
                );
                self.gui.set_scan_info(scan_info);
            }
            GuiAction::Exit => {
                self.request_exit(event_loop);
            }
        }
    }

    /// Request application exit - handles both GUI and keyboard exit requests
    fn request_exit(&self, event_loop: Option<&ActiveEventLoop>) {
        if let Some(event_loop) = event_loop {
            log::info!("Exiting application");
            event_loop.exit();
        } else {
            // Fallback for cases where event_loop isn't available
            std::process::exit(0);
        }
    }

    fn handle_input_events(&mut self, event_loop: &ActiveEventLoop) {
        if self.input.key_pressed(KeyCode::Escape) {
            self.request_exit(Some(event_loop));
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop
            .create_window(Window::default_attributes())
            .unwrap();

        pollster::block_on(self.set_window(window));

        // Request initial redraw to start the rendering loop
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.create_window(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // Update input handler
        let winit_event: winit::event::Event<()> = winit::event::Event::WindowEvent {
            window_id,
            event: event.clone(),
        };
        if self.input.process_event(&winit_event) {
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
            }
            WindowEvent::Resized(new_size) => {
                self.handle_resized(new_size.width, new_size.height);
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
