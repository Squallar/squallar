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
use crate::texture_manager::TextureManager;
use crate::world;
use chrono::TimeZone;
use rustdar_egui::{Gui, actions::GuiAction};

#[cfg(target_arch = "wasm32")]
use crate::wasm_canvas;
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use winit::platform::web::WindowAttributesExtWebSys;

use chrono::NaiveDateTime;
use nexrad::model::DataFile;
use std::sync::mpsc::{Receiver, Sender};

type ScanResult = Result<(DataFile, String, NaiveDateTime), String>;

pub struct App {
    instance: wgpu::Instance,
    state: Option<app_state::AppState>,
    window: Option<WindowRef>,
    #[cfg(target_arch = "wasm32")]
    shared_state: Rc<RefCell<Option<app_state::AppState>>>,
    gui: Gui,
    world: world::World,
    input: InputHandler,
    texture_manager: TextureManager,
    scan_receiver: Receiver<ScanResult>,
    scan_sender: Sender<ScanResult>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let instance = egui_wgpu::wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let world = world::World::new();
        let input = InputHandler::new();
        let (scan_sender, scan_receiver) = std::sync::mpsc::channel();

        Self {
            instance,
            state: None,
            window: None,
            #[cfg(target_arch = "wasm32")]
            shared_state: Rc::new(RefCell::new(None)),
            gui: Gui::new(),
            world,
            input,
            texture_manager: TextureManager::new(),
            scan_receiver,
            scan_sender,
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

    #[cfg(not(target_arch = "wasm32"))]
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

        // Trigger a resize event now that the WASM state is ready
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                if let Ok(event) = web_sys::Event::new("resize") {
                    let _ = window.dispatch_event(&event);
                    log::debug!("WASM state ready - triggered resize event");
                }
            }
        }
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
                    let scan_info = self.world.load_scan_data(data, &site, timestamp);
                    self.gui.set_scan_info(scan_info);
                    log::info!("✅ Scan data loaded into world and UI updated");
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

        // For WASM, check if async initialization is complete
        #[cfg(target_arch = "wasm32")]
        self.prepare_wasm_state();

        // Check if we have the necessary resources
        if self.state.is_none() || self.window.is_none() {
            return;
        }

        // Update world
        self.world.update();

        // Setup egui and get GUI actions
        let (screen_descriptor, gui_actions) = self.setup_egui_frame();

        // Render world to texture
        self.render_world_to_texture();

        // Render world texture in egui panel
        self.render_world_panel();

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

    #[cfg(target_arch = "wasm32")]
    fn prepare_wasm_state(&mut self) {
        if self.state.is_none() {
            // Check if the shared state has been initialized and extract it
            let state_to_set = if let Ok(mut shared_state) = self.shared_state.try_borrow_mut() {
                shared_state.take()
            } else {
                None
            };

            if let Some(state) = state_to_set {
                self.state = Some(state);

                // Manually call handle_resized to sync surface size with viewport
                let (width, height) = wasm_canvas::get_viewport_dimensions();
                self.handle_resized(width, height);
            }
        }
    }

    /// Create screen descriptor and setup egui frame.
    /// Returns the screen descriptor and any GUI actions triggered.
    ///
    /// This calculates the proper scaling factors accounting for:
    /// - OS display scaling (window.scale_factor())
    /// - Application scale factor (state.scale_factor)
    /// - CSS-to-canvas scaling for WASM (css_to_canvas_scale_x)
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

        (screen_descriptor, gui_action)
    }

    fn render_world_to_texture(&mut self) {
        let state = self.state.as_mut().unwrap();

        // Render world screen to fixed-size framebuffer
        self.texture_manager.framebuffer_mut().fill(0);
        self.world.draw(self.texture_manager.framebuffer_mut());

        // Always update texture (frame detection not yet implemented)
        self.texture_manager
            .update_texture(state.egui_renderer.context());
    }

    fn render_world_panel(&self) {
        let state = self.state.as_ref().unwrap();

        // Only render world screen if we have a texture
        if let Some(texture_handle) = self.texture_manager.texture_handle() {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::TRANSPARENT))
                .show(state.egui_renderer.context(), |ui| {
                    // Get available space and calculate scale to fit
                    let available_rect = ui.available_rect_before_wrap();

                    let texture_width = RENDER_WIDTH as f32;
                    let texture_height = RENDER_HEIGHT as f32;

                    // Scale to fit in available space while keeping aspect ratio
                    let scale_x = available_rect.width() / texture_width;
                    let scale_y = available_rect.height() / texture_height;
                    let final_scale = scale_x.min(scale_y);

                    let final_size =
                        egui::Vec2::new(texture_width * final_scale, texture_height * final_scale);

                    ui.with_layout(
                        egui::Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            ui.add_sized(
                                final_size,
                                egui::Image::new(texture_handle).fit_to_exact_size(final_size),
                            );
                        },
                    );
                });
        }
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
                    .unwrap_or_else(|| chrono::Local::now());
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

                #[cfg(not(target_arch = "wasm32"))]
                {
                    log::info!("Spawning background thread for radar fetch...");
                    // For native platforms, spawn a Tokio runtime in a new thread
                    std::thread::spawn(move || {
                        log::info!("Background thread started, creating Tokio runtime...");
                        let rt =
                            tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
                        log::info!("Starting fetch for {} @ {} UTC", site, timestamp.date());
                        let result = rt.block_on(rustdar_radar::get_scan(&site, timestamp));
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

                #[cfg(target_arch = "wasm32")]
                {
                    wasm_bindgen_futures::spawn_local(async move {
                        let result = rustdar_radar::get_scan(&site, timestamp).await;
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
                            window.request_redraw();
                        }
                    });
                }
            }
            GuiAction::SetScanInfo(scan_info) => {
                log::info!(
                    "Setting scan info: {} @ {}",
                    scan_info.site,
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

    /// Create a window for native platforms (non-WASM)
    #[cfg(not(target_arch = "wasm32"))]
    fn create_window_native(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop
            .create_window(Window::default_attributes())
            .unwrap();

        pollster::block_on(self.set_window(window));

        // Request initial redraw to start the rendering loop
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Create a window for WASM platform
    #[cfg(target_arch = "wasm32")]
    fn create_window_wasm(&mut self, event_loop: &ActiveEventLoop) {
        // Cache web_sys::window() for reuse
        let web_window = web_sys::window().expect("Failed to get web_sys::window");

        // Get the existing canvas element
        let canvas = web_window
            .document()
            .and_then(|doc| doc.get_element_by_id("app-canvas"))
            .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("Failed to find canvas element with id 'app-canvas'");

        // Get viewport size
        let (width, height) = wasm_canvas::get_viewport_dimensions();

        // Create window with explicit inner size to match viewport
        let window_attributes = Window::default_attributes()
            .with_canvas(Some(canvas))
            .with_inner_size(PhysicalSize::new(width, height));

        let window = event_loop.create_window(window_attributes).unwrap();
        let window = Arc::new(window);

        // Set canvas size to match viewport (accounting for device pixel ratio)
        wasm_canvas::resize_canvas_to_viewport(&window);
        window.request_redraw();

        self.window = Some(window.clone());

        // Set up window resize handler
        self.setup_wasm_resize_handler(&window);

        // Start async initialization
        self.start_wasm_async_init(window);
    }

    /// Set up resize event handler for WASM
    #[cfg(target_arch = "wasm32")]
    fn setup_wasm_resize_handler(&self, window: &WindowRef) {
        let web_window = web_sys::window().expect("Failed to get web_sys::window");
        let window_clone = Rc::new(window.clone());

        let closure = wasm_bindgen::closure::Closure::wrap(Box::new({
            let window = window_clone.clone();
            move |_e: web_sys::Event| {
                wasm_canvas::resize_canvas_to_viewport(&window);
            }
        }) as Box<dyn FnMut(_)>);

        web_window
            .add_event_listener_with_callback("resize", closure.as_ref().unchecked_ref())
            .unwrap();
        closure.forget();
    }

    /// Start async initialization for WASM
    #[cfg(target_arch = "wasm32")]
    fn start_wasm_async_init(&mut self, window: WindowRef) {
        let instance = self.instance.clone();
        let shared_state = self.shared_state.clone();

        wasm_bindgen_futures::spawn_local(async move {
            // Get viewport size (accounting for device pixel ratio)
            let (width, height) = wasm_canvas::get_viewport_dimensions();

            let state = App::initialize_rendering_state(&instance, &window, width, height).await;

            *shared_state.borrow_mut() = Some(state);

            // Request initial render
            window.request_redraw();
        });
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_arch = "wasm32")]
        self.create_window_wasm(event_loop);

        #[cfg(not(target_arch = "wasm32"))]
        self.create_window_native(event_loop);
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
