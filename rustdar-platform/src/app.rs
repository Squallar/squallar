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
use crate::channels::ChannelHub;
use crate::constants::*;
use crate::input::InputHandler;
use crate::platform::{self, PlatformBridge};
use chrono::TimeZone;
use rustdar_egui::{
    Gui,
    actions::GuiAction,
};
use rustdar_radar::render::{render_radar_to_image, render_level3_radial_to_image};
use rustdar_radar::types::{RadarProduct, ScanInfo};
use rustdar_radar::scan;

use chrono::NaiveDateTime;

#[path = "app_fetch.rs"]
mod fetch;


/// Per-pane render tracking state.
struct PaneRenderState {
    /// True while a background render is in progress for this pane.
    render_in_flight: bool,
    /// Last rendered radar parameters to detect changes.
    last_rendered: Option<(RadarProduct, f32)>,
    /// Cached raw RGBA + metadata from the last successful render so we can
    /// re-upload the texture instantly after suspend/resume without re-rendering.
    cached_render: Option<(Vec<u8>, f64, Vec<f32>, RadarProduct, f32)>,
}

impl PaneRenderState {
    fn new() -> Self {
        Self {
            render_in_flight: false,
            last_rendered: None,
            cached_render: None,
        }
    }
}


pub struct App {
    instance: wgpu::Instance,
    state: Option<app_state::AppState>,
    window: Option<WindowRef>,
    gui: Gui,
    scan_data: Option<Arc<nexrad_model::data::Scan>>,
    input: InputHandler,
    channels: ChannelHub,
    platform: Box<dyn PlatformBridge>,
    scan_receiver: Receiver<ScanResult>,
    scan_sender: Sender<ScanResult>,
    // Channel for background radar render results
    render_result_receiver: Receiver<RenderResult>,
    render_result_sender: Sender<RenderResult>,
    // Per-pane render tracking (indexed by pane index)
    pane_render: Vec<PaneRenderState>,
    // Decoded Level III product data, keyed by (RadarProduct, tilt_code)
    level3_data: HashMap<(RadarProduct, String), Arc<Level3Message>>,
    // Channel for Level III fetch results
    level3_receiver: Receiver<Level3Result>,
    level3_sender: Sender<Level3Result>,
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
    // Flag for deferred exit when event_loop isn't available during redraw
    exit_requested: bool,
    // Shared Tokio runtime for all async network requests
    tokio_runtime: tokio::runtime::Runtime,
    // Shared HTTP client for overlay data fetches (SPC, etc.)
    http_client: reqwest::Client,
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
        let channels = ChannelHub::new();
        let platform = Box::new(platform::create_platform());

        let tokio_runtime = tokio::runtime::Runtime::new()
            .expect("Failed to create Tokio runtime");

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("rustdar/1.0 (https://github.com/USA-RedDragon/rustdar)")
            .build()
            .unwrap_or_default();

        let mut gui = Gui::new();
        if let Some(config_dir) = Gui::default_config_dir() {
            gui.load_ui_config(&config_dir);
        }

        Self {
            instance,
            state: None,
            window: None,
            gui,
            scan_data: None,
            input,
            channels,
            platform,
            pane_render: vec![PaneRenderState::new()],
            level3_data: HashMap::new(),
            level3_receiver,
            level3_sender,
            render_generation: 0,
            fetch_generation: 0,
            texture_counter: 0,
            old_textures: Vec::new(),
            cached_dark_theme: None,
            applied_visuals_dark: None,
            exit_requested: false,
            http_client,
            outlook_sender,
            outlook_receiver,
            alert_sender,
            alert_receiver,
            discussion_sender,
            discussion_receiver,
            tokio_runtime,
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

        // Poll for platform-specific theme and location changes
        if let Some(new_theme) = self.platform.poll_theme() {
            if self.cached_dark_theme != Some(new_theme) {
                self.cached_dark_theme = Some(new_theme);
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
        }
        if let Some((lat, lon)) = self.platform.poll_location() {
            self.gui.set_user_location(lat, lon);
        }

        self.poll_data_channels();

        // Attempt to handle minimizing window
        if let Some(window) = self.window.as_ref()
            && let Some(min) = window.is_minimized()
            && min
        {
            log::debug!("Window is minimized");
            return;
        }

        // Lazily initialize rendering state on first redraw after window creation.
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

        // Handle GUI actions
        for action in gui_actions {
            log::info!("GUI action received: {}", action);
            self.handle_gui_action(action, None);
        }

        // Request redraw only when there is pending background work or auto-poll is active
        if self.pane_render.iter().any(|prs| prs.render_in_flight) || self.gui.is_auto_poll_active() {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    /// Poll all data channels for completed async results (scan, overlays).
    fn poll_data_channels(&mut self) {
        // Check for received scan data (with generation check)
        if let Ok((generation, result)) = self.channels.scan_receiver.try_recv() {
            if generation < self.fetch_generation {
                log::info!("Discarding stale scan result (gen {} < current {})", generation, self.fetch_generation);
            } else {
                match result {
                    Ok((data, site, timestamp)) => {
                        log::info!("Received scan data from background thread");
                        let scan_info = ScanInfo::from_scan(&data, &site, timestamp);
                        self.scan_data = Some(Arc::new(data));
                        self.gui.set_scan_info(scan_info);
                        self.gui.set_loading_site(None);
                        for prs in &mut self.pane_render {
                            prs.last_rendered = None;
                            prs.cached_render = None;
                            prs.render_in_flight = false;
                        }
                        self.render_generation += 1;
                        self.level3_data.clear();
                        self.spawn_level3_fetches(&site, timestamp);
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
            while let Ok((day, product, result)) = self.channels.outlook_receiver.try_recv() {
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

        // Check for received NWS alerts data
        if let Ok(result) = self.channels.alert_receiver.try_recv() {
            match result {
                Ok(alerts) => {
                    log::info!("Received {} NWS alerts", alerts.len());
                    self.gui.set_nws_alerts(alerts);
                }
                Err(e) => {
                    log::error!("NWS alerts fetch failed: {}", e);
                }
            }
            self.gui.set_nws_fetching(false);
        }

        // Check for received SPC Mesoscale Discussion data
        if let Ok(result) = self.channels.discussion_receiver.try_recv() {
            match result {
                Ok(discussions) => {
                    log::info!("Received {} SPC Mesoscale Discussions", discussions.len());
                    self.gui.set_spc_discussions(discussions);
                }
                Err(e) => {
                    log::error!("SPC MD fetch failed: {}", e);
                }
            }
            self.gui.set_spc_md_fetching(false);
        }
    }

    /// Create screen descriptor and setup egui frame.
    /// Returns the screen descriptor and any GUI actions triggered.
    ///
    /// This calculates the proper scaling factors accounting for:
    /// - OS display scaling (window.scale_factor())
    /// - Application scale factor (state.scale_factor)
    fn setup_egui_frame(&mut self) -> (ScreenDescriptor, Vec<GuiAction>) {
        // Build screen descriptor, apply theme, and run the egui UI pass.
        // Scoped so `state` is dropped before we call &mut self methods below.
        let (screen_descriptor, gui_action) = {
            let state = self.state.as_mut().unwrap();
            let window = self.window.as_ref().unwrap();

            // Calculate screen descriptor
            let window_size = window.inner_size();
            let css_to_canvas_scale_x =
                state.surface_config.width as f32 / window_size.width as f32;
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
                            let detected = self.platform.detect_dark_theme();
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

            (screen_descriptor, gui_action)
        };

        // Clean up old textures from previous frame
        // This allows the GPU to finish using them before we drop them
        self.old_textures.clear();

        // Ensure pane_render vec matches gui pane count
        while self.pane_render.len() < self.gui.pane_count() {
            self.pane_render.push(PaneRenderState::new());
        }

        self.poll_render_results();
        self.poll_level3_results();
        self.dispatch_pane_renders();

        (screen_descriptor, gui_action)
    }

    /// Poll for completed background render results and upload textures.
    fn poll_render_results(&mut self) {
        let ctx = self.state.as_ref().unwrap().egui_renderer.context();
        while let Ok((image_data, max_range_km, value_data, product, elevation, generation, pane_idx)) =
            self.channels.render_receiver.try_recv()
        {
            if pane_idx < self.pane_render.len() {
                self.pane_render[pane_idx].render_in_flight = false;
            }

            if generation < self.render_generation {
                log::info!("Discarding stale render result (gen {} < current {})", generation, self.render_generation);
            } else if pane_idx < self.gui.pane_count()
                && self.gui.get_rendering_params_for_pane(pane_idx).is_some()
            {
                // Save the old texture to be cleaned up after this frame completes
                if let Some(old_img) = self.gui.take_radar_image_for_pane(pane_idx) {
                    self.old_textures.push(old_img.texture);
                }

                self.texture_counter += 1;
                let color_image =
                    egui::ColorImage::from_rgba_unmultiplied([1800, 1800], &image_data);
                let texture_name = format!("radar_image_{}", self.texture_counter);
                let texture = ctx.load_texture(
                    texture_name,
                    color_image,
                    egui::TextureOptions::NEAREST,
                );

                // Cache the raw image data for fast restore after suspend/resume
                self.pane_render[pane_idx].cached_render = Some((
                    image_data,
                    max_range_km,
                    value_data.clone(),
                    product,
                    elevation,
                ));

                if let Some(scan_info) = self.gui.get_scan_info() {
                    self.gui.set_radar_image_for_pane(
                        pane_idx,
                        texture,
                        scan_info.site.lat,
                        scan_info.site.lon,
                        max_range_km,
                        value_data,
                    );
                }

                self.pane_render[pane_idx].last_rendered = Some((product, elevation));
            }
        }
    }

    /// Poll for completed Level III fetch results and update scan info.
    fn poll_level3_results(&mut self) {
        if let Ok((generation, product, tilt_code, result)) = self.channels.level3_receiver.try_recv() {
            if generation < self.fetch_generation {
                log::info!("Discarding stale Level III result (gen {} < current {})", generation, self.fetch_generation);
            } else {
                match result {
                    Ok(message) => {
                        let elevation = message.pdb.elevation_angle();
                        log::info!("Level III {:?} {} fetched successfully (elevation={:.1}°)", product, tilt_code, elevation);
                        self.level3_data.insert((product, tilt_code), Arc::new(message));
                        // Trigger a re-render for any pane viewing this product
                        for (idx, prs) in self.pane_render.iter_mut().enumerate() {
                            if self.gui.get_rendering_params_for_pane(idx).map(|(p, _)| p) == Some(product) {
                                prs.last_rendered = None;
                            }
                        }
                        // Add Level III products to the scan info's available list
                        if let Some(scan_info) = self.gui.get_scan_info() {
                            let mut info = scan_info.clone();
                            if !info.available_products.contains(&product) {
                                info.available_products.push(product);
                                info.available_products.sort_by_key(|p| p.sort_order());
                                info.status = format!(
                                    "Loaded {} products: {}",
                                    info.available_products.len(),
                                    info.available_products.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")
                                );
                            }
                            // Register the actual elevation angle from the PDB
                            let elevations = info.product_elevations.entry(product).or_default();
                            let rounded_elev = (elevation * 10.0).round() / 10.0;
                            if !elevations.iter().any(|e| (e - rounded_elev).abs() < 0.05) {
                                elevations.push(rounded_elev);
                                elevations.sort_by(|a, b| a.partial_cmp(b).unwrap());
                            }
                            self.gui.set_scan_info(info);
                        }
                    }
                    Err(e) => {
                        log::warn!("Level III {:?} fetch failed: {}", product, e);
                    }
                }
            }
        }
    }

    /// Check all panes for needed background renders and spawn render threads.
    fn dispatch_pane_renders(&mut self) {
        for pane_idx in 0..self.gui.pane_count() {
            if let Some((product, elevation)) = self.gui.get_rendering_params_for_pane(pane_idx) {
                let prs = &self.pane_render[pane_idx];
                let needs_render = prs
                    .last_rendered
                    .map(|(last_prod, last_elev)| {
                        last_prod != product || (last_elev - elevation).abs() > 0.01
                    })
                    .unwrap_or(true);

                if needs_render && !prs.render_in_flight {
                    if product.is_level3() {
                        // Level III render path — find the tilt with the closest elevation
                        let best_l3 = self.level3_data.iter()
                            .filter(|((p, _), _)| *p == product)
                            .min_by(|(_, a), (_, b)| {
                                let da = (a.pdb.elevation_angle() - elevation).abs();
                                let db = (b.pdb.elevation_angle() - elevation).abs();
                                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .map(|(_, msg)| Arc::clone(msg));
                        if let Some(l3_msg) = best_l3 {
                            if let Some(scan_info) = self.gui.get_scan_info() {
                                log::info!("Spawning Level III render for pane {}: {:?}", pane_idx, product);
                                let l3_msg = Arc::clone(&l3_msg);
                                let lat = scan_info.site.lat;
                                let lon = scan_info.site.lon;
                                let sender = self.render_result_sender.clone();
                                let generation = self.render_generation;
                                let window = self.window.clone();

                                std::thread::spawn(move || {
                                    log::debug!(
                                        "L3 {:?}: pdb product_code={}, thresholds={:?}, ps47_53={:?}",
                                        product, l3_msg.pdb.product_code,
                                        l3_msg.pdb.thresholds, l3_msg.pdb.product_specific_47_53
                                    );
                                    // Extract radial packet from symbology
                                    let radial_packet = l3_msg.symbology.as_ref().and_then(|sym| {
                                        log::debug!("L3 {:?}: symbology has {} layers", product, sym.layers.len());
                                        for (li, layer) in sym.layers.iter().enumerate() {
                                            log::debug!("L3 {:?}: layer {} has {} packets", product, li, layer.packets.len());
                                            for (pi, pkt) in layer.packets.iter().enumerate() {
                                                match pkt {
                                                    nexrad_level3::model::DataPacket::DigitalRadial(rp) => {
                                                        log::debug!(
                                                            "L3 {:?}: layer[{}].packet[{}] = DigitalRadial: radials={}, bins={}, scale_factor={}, is_legacy={}, first_range_bin={}",
                                                            product, li, pi, rp.radials.len(), rp.num_range_bins, rp.scale_factor, rp.is_legacy, rp.first_range_bin
                                                        );
                                                        if let Some(r0) = rp.radials.first() {
                                                            let non_zero: usize = r0.gate_values.iter().filter(|&&v| v > 1).count();
                                                            let max_val = r0.gate_values.iter().copied().max().unwrap_or(0);
                                                            log::debug!(
                                                                "L3 {:?}: first radial: start_angle={}, delta={}, gates={}, non_zero(>1)={}, max_gate_val={}, first_10={:?}",
                                                                product, r0.start_angle, r0.angle_delta, r0.gate_values.len(), non_zero, max_val,
                                                                &r0.gate_values[..r0.gate_values.len().min(10)]
                                                            );
                                                        }
                                                    }
                                                    nexrad_level3::model::DataPacket::Raster(_) => {
                                                        log::debug!("L3 {:?}: layer[{}].packet[{}] = Raster", product, li, pi);
                                                    }
                                                }
                                            }
                                        }
                                        sym.layers.iter().find_map(|layer| {
                                            layer.packets.iter().find_map(|pkt| {
                                                if let nexrad_level3::model::DataPacket::DigitalRadial(rp) = pkt {
                                                    Some(rp)
                                                } else {
                                                    None
                                                }
                                            })
                                        })
                                    });
                                    if radial_packet.is_none() {
                                        log::warn!("L3 {:?}: no radial packet found in symbology!", product);
                                    }
                                    if let Some(rp) = radial_packet {
                                        let scale = l3_msg.pdb.data_scale();
                                        let offset = l3_msg.pdb.data_offset();
                                        let vil_lut = l3_msg.pdb.build_vil_lut();
                                        let legacy_lut;
                                        let lut: Option<&[f32]> = if vil_lut.is_some() {
                                            vil_lut.as_deref()
                                        } else if rp.is_legacy {
                                            legacy_lut = l3_msg.pdb.decode_legacy_thresholds();
                                            Some(legacy_lut.as_slice())
                                        } else {
                                            None
                                        };
                                        log::debug!(
                                            "L3 {:?}: rendering with scale={}, offset={}, legacy={}, lut_len={:?}, gate_interval_km={}, first_gate_range_km={}",
                                            product, scale, offset, rp.is_legacy, lut.map(|l| l.len()), rp.gate_interval_km(), rp.first_gate_range_km()
                                        );
                                        if let Some((image, range, values)) =
                                            render_level3_radial_to_image(rp, product, lat, lon, scale, offset, lut)
                                        {
                                            let _ = sender.send((image, range, values, product, elevation, generation, pane_idx));
                                        } else {
                                            log::warn!("L3 {:?}: render_level3_radial_to_image returned None", product);
                                        }
                                    }
                                    if let Some(window) = window {
                                        window.request_redraw();
                                    }
                                });
                                self.pane_render[pane_idx].render_in_flight = true;
                            }
                        }
                    } else if let Some(data) = &self.scan_data {
                        if let Some(scan_info) = self.gui.get_scan_info() {
                            log::info!("Spawning background render for pane {}: {:?} at {:.1}°", pane_idx, product, elevation);
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
                                    let _ = sender.send((image, range, values, product, elevation, generation, pane_idx));
                                }
                                if let Some(window) = window {
                                    window.request_redraw();
                                }
                            });
                            self.pane_render[pane_idx].render_in_flight = true;
                        }
                    }
                }
            } else if pane_idx < self.pane_render.len() {
                // No rendering params for this pane, clear its image
                self.gui.clear_radar_image_for_pane(pane_idx);
                self.pane_render[pane_idx].last_rendered = None;
            }
        }
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
        let Some(scan_info) = self.gui.get_scan_info().cloned() else {
            return;
        };

        for pane_idx in 0..self.pane_render.len().min(self.gui.pane_count()) {
            let Some((ref image_data, max_range_km, ref value_data, product, elevation)) =
                self.pane_render[pane_idx].cached_render
            else {
                continue;
            };

            log::info!(
                "Restoring cached radar image for pane {} ({:?} at {:.1}°) from memory",
                pane_idx,
                product,
                elevation
            );

            self.texture_counter += 1;
            let ctx = state.egui_renderer.context();
            let color_image = egui::ColorImage::from_rgba_unmultiplied([1800, 1800], image_data);
            let texture_name = format!("radar_image_{}", self.texture_counter);
            let texture = ctx.load_texture(texture_name, color_image, egui::TextureOptions::NEAREST);

            self.gui.set_radar_image_for_pane(
                pane_idx,
                texture,
                scan_info.site.lat,
                scan_info.site.lon,
                max_range_km,
                value_data.clone(),
            );
            self.pane_render[pane_idx].last_rendered = Some((product, elevation));
        }
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
            for prs in &mut self.pane_render {
                prs.last_rendered = None;
            }
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
        let textures_to_free = state.egui_renderer.end_frame_and_draw(
            &state.device,
            &state.queue,
            &mut encoder,
            window,
            &surface_view,
            screen_descriptor,
        );

        state.queue.submit(Some(encoder.finish()));
        state.egui_renderer.free_textures(&textures_to_free);
        surface_texture.present();
    }

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
                let sender = self.channels.outlook_sender.clone();
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
            GuiAction::FetchNwsAlerts | GuiAction::RefreshNwsAlerts => {
                log::info!("Fetching NWS active alerts");
                self.gui.set_nws_fetching(true);
                let client = self.http_client.clone();
                let sender = self.channels.alert_sender.clone();
                let window = self.window.clone();
                let zone_cache = self.platform.zone_cache_dir().map(|p| p.to_path_buf());
                self.tokio_runtime.spawn(async move {
                    let result =
                        rustdar_overlays::nws::fetch::fetch_active_alerts(
                            &client,
                            zone_cache.as_deref(),
                        )
                            .await
                            .map_err(|e| format!("{e}"));
                    let _ = sender.send(result);
                    if let Some(w) = window {
                        w.request_redraw();
                    }
                });
            }
            GuiAction::FetchSpcDiscussions | GuiAction::RefreshSpcDiscussions => {
                log::info!("Fetching SPC Mesoscale Discussions");
                self.gui.set_spc_md_fetching(true);
                let client = self.http_client.clone();
                let sender = self.channels.discussion_sender.clone();
                let window = self.window.clone();
                self.tokio_runtime.spawn(async move {
                    let result =
                        rustdar_overlays::spc::fetch::fetch_active_discussions(&client)
                            .await
                            .map_err(|e| format!("{e}"));
                    let _ = sender.send(result);
                    if let Some(w) = window {
                        w.request_redraw();
                    }
                });
            }
        }
    }

    /// Request application exit - handles both GUI and keyboard exit requests
    fn request_exit(&mut self, event_loop: Option<&ActiveEventLoop>) {
        // Persist UI config before exiting
        if let Some(config_dir) = Gui::default_config_dir() {
            self.gui.save_ui_config(&config_dir);
        }
        if let Some(event_loop) = event_loop {
            log::info!("Exiting application");
            event_loop.exit();
            if self.platform.needs_process_exit() {
                std::process::exit(0);
            }
        } else {
            // Defer exit until the next event where event_loop is available
            self.exit_requested = true;
        }
    }

    /// Set a callback to handle the back button (e.g. moveTaskToBack on Android).
    pub fn set_back_handler(&mut self, handler: fn()) {
        self.platform.set_back_handler(handler);
    }

    /// Override the zone geometry cache directory.
    pub fn set_zone_cache_dir(&mut self, dir: std::path::PathBuf) {
        self.platform.set_zone_cache_dir(dir);
    }

    /// Set a receiver for GPS location updates (Android only).
    #[cfg(target_os = "android")]
    pub fn set_location_receiver(&mut self, receiver: std::sync::mpsc::Receiver<(f64, f64)>) {
        use crate::platform::AndroidPlatform;
        if let Some(android) = self.platform.as_any_mut().downcast_mut::<AndroidPlatform>() {
            android.set_location_receiver(receiver);
        }
    }

    /// Set safe area insets in logical pixels (top, bottom, left, right).
    #[cfg(target_os = "android")]
    pub fn set_safe_area_insets(&mut self, top: f32, bottom: f32, left: f32, right: f32) {
        self.gui.set_safe_area_insets(top, bottom, left, right);
    }

    /// Set a callback that queries system bar insets.
    #[cfg(target_os = "android")]
    pub fn set_insets_querier(&mut self, querier: fn() -> (f32, f32, f32, f32)) {
        use crate::platform::AndroidPlatform;
        if let Some(android) = self.platform.as_any_mut().downcast_mut::<AndroidPlatform>() {
            android.set_insets_querier(querier);
        }
    }

    fn handle_input_events(&mut self, event_loop: &ActiveEventLoop) {
        if self.input.key_pressed(KeyCode::Escape) {
            self.request_exit(Some(event_loop));
        }

        if self.input.back_pressed() {
            if !self.platform.handle_back() {
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
        if let Some((top, bottom, left, right)) = self.platform.query_insets() {
            self.gui.set_safe_area_insets(top, bottom, left, right);
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        log::info!("App suspended — clearing graphics state");
        self.old_textures.clear();
        for prs in &mut self.pane_render {
            prs.last_rendered = None;
        }
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
