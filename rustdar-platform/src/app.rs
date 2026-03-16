use egui_wgpu::wgpu;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
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
use crate::render_dispatch::RenderDispatcher;
use rustdar_egui::{
    Gui,
    actions::GuiAction,
};
use rustdar_radar::types::ScanInfo;

#[path = "app_fetch.rs"]
mod fetch;

#[path = "app_render.rs"]
mod render;

/// Request a redraw if a window handle is available.
/// Used by async tasks and event handlers that hold an `Option<WindowRef>`.
pub(crate) fn notify_redraw(window: &Option<WindowRef>) {
    if let Some(w) = window {
        // Background threads may outlive the event loop on exit.
        // request_redraw() panics on X11 when the loop is closed,
        // so we catch and ignore that.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            w.request_redraw();
        }));
    }
}

pub struct App {
    instance: wgpu::Instance,
    state: Option<app_state::AppState>,
    window: Option<WindowRef>,
    gui: Gui,
    scan_data: std::collections::HashMap<String, Arc<nexrad_model::data::Scan>>,
    input: InputHandler,
    channels: ChannelHub,
    render: RenderDispatcher,
    platform: Box<dyn PlatformBridge>,
    // Counter to generate unique texture names
    texture_counter: u32,
    // Old textures to clean up after the next frame
    old_textures: Vec<egui::TextureHandle>,
    // Cache the detected theme to avoid calling detection every frame
    cached_dark_theme: Option<bool>,
    // Flag for deferred exit when event_loop isn't available during redraw
    exit_requested: bool,
    // Shared Tokio runtime for all async network requests
    tokio_runtime: tokio::runtime::Runtime,
    // Shared HTTP client for overlay data fetches (SPC, etc.)
    http_client: reqwest::Client,
    // Downloaded scan data cache for loop frames, keyed by timestamp (shared across panes).
    loop_scan_cache: std::collections::HashMap<chrono::NaiveDateTime, Arc<nexrad_model::data::Scan>>,
    // Timestamps currently being downloaded (to avoid duplicate downloads across panes).
    loop_downloads_in_flight_set: std::collections::HashSet<chrono::NaiveDateTime>,
    // Pending loop scan downloads per pane, waiting to be dispatched (throttled).
    loop_pending_downloads: std::collections::HashMap<usize, Vec<(chrono::NaiveDateTime, nexrad_data::aws::archive::Identifier)>>,
    // Number of loop scan downloads currently in flight (global, not per-pane).
    loop_downloads_in_flight: usize,
    // Shared counter for background render threads in flight (loop + static renders).
    renders_in_flight: Arc<AtomicUsize>,
    // Cached latest scan from auto-poll while viewing historic data.
    latest_cached_scan: Option<(Arc<nexrad_model::data::Scan>, ScanInfo, String, chrono::NaiveDateTime)>,
    // Set when a manual time navigation fetch is pending; triggers loop reinit after scan loads.
    manual_nav_pending: bool,
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
        let renders_in_flight = Arc::new(AtomicUsize::new(0));
        let render = RenderDispatcher::new(Arc::clone(&renders_in_flight));
        let platform = Box::new(platform::create_platform());

        let tokio_runtime = tokio::runtime::Runtime::new()
            .expect("Failed to create Tokio runtime");

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("rustdar/1.0 (https://github.com/USA-RedDragon/rustdar)")
            .build()
            .unwrap_or_default();

        let mut gui = Gui::new();
        if let Some(config_dir) = platform.config_dir() {
            gui.load_ui_config(config_dir);
        }

        Self {
            instance,
            state: None,
            window: None,
            gui,
            scan_data: std::collections::HashMap::new(),
            input,
            channels,
            render,
            platform,
            texture_counter: 0,
            old_textures: Vec::new(),
            cached_dark_theme: None,
            exit_requested: false,
            http_client,
            tokio_runtime,
            loop_scan_cache: std::collections::HashMap::new(),
            loop_downloads_in_flight_set: std::collections::HashSet::new(),
            loop_pending_downloads: std::collections::HashMap::new(),
            loop_downloads_in_flight: 0,
            renders_in_flight: Arc::new(AtomicUsize::new(0)),
            latest_cached_scan: None,
            manual_nav_pending: false,
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
        self.input.clear_frame_state();
        self.poll_platform_state();
        self.poll_data_channels();

        // Skip rendering when minimized
        if let Some(window) = self.window.as_ref()
            && let Some(min) = window.is_minimized()
            && min
        {
            log::debug!("Window is minimized");
            return;
        }

        self.ensure_rendering_state();
        if self.state.is_none() || self.window.is_none() {
            return;
        }

        let (screen_descriptor, gui_actions) = self.setup_egui_frame();
        self.present_frame(screen_descriptor);
        self.process_gui_actions(gui_actions);

        // Request redraw only when there is pending background work or auto-poll is active
        if self.render.any_render_in_flight() || self.gui.is_auto_poll_active() || self.gui.any_loop_active() {
            notify_redraw(&self.window);
        }
    }

    /// Poll for platform-specific theme and location changes.
    fn poll_platform_state(&mut self) {
        if let Some(new_theme) = self.platform.poll_theme() {
            if self.cached_dark_theme != Some(new_theme) {
                self.cached_dark_theme = Some(new_theme);
                notify_redraw(&self.window);
            }
        }
        if let Some((lat, lon)) = self.platform.poll_location() {
            self.gui.set_user_location(lat, lon);
        }
    }

    /// Lazily initialize wgpu rendering state on first redraw after window creation.
    fn ensure_rendering_state(&mut self) {
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
    }

    /// Process all GUI actions emitted during this frame.
    fn process_gui_actions(&mut self, actions: Vec<GuiAction>) {
        use rustdar_egui::actions::OverlayRenderKind;

        // Separate overlay render actions for deduplication
        let mut overlay_renders: Vec<(usize, OverlayRenderKind, rustdar_overlays::types::GeoBounds, u32, u32, u64, i32)> = Vec::new();

        for action in actions {
            if let GuiAction::RenderOverlay { pane_idx, overlay_kind, geo_bounds, width, height, data_generation, zoom } = action {
                overlay_renders.push((pane_idx, overlay_kind, geo_bounds, width, height, data_generation, zoom));
            } else {
                log::debug!("GUI action received: {}", action);
                self.handle_gui_action(action, None);
            }
        }

        // Deduplicate overlay renders: when viewport_sync is on, group by
        // (overlay_kind, zoom, data_generation, width, height) and spawn one render for all panes
        if !overlay_renders.is_empty() {
            if self.gui.is_viewport_sync() {
                let mut grouped: std::collections::HashMap<
                    (std::mem::Discriminant<OverlayRenderKind>, i32, u64, u32, u32),
                    (OverlayRenderKind, rustdar_overlays::types::GeoBounds, u32, u32, u64, i32, Vec<usize>),
                > = std::collections::HashMap::new();

                for (pane_idx, kind, bounds, w, h, data_gen, zoom) in overlay_renders {
                    let key = (std::mem::discriminant(&kind), zoom, data_gen, w, h);
                    grouped.entry(key)
                        .and_modify(|(_k, _b, _w, _h, _g, _z, panes)| {
                            if !panes.contains(&pane_idx) {
                                panes.push(pane_idx);
                            }
                        })
                        .or_insert_with(|| (kind.clone(), bounds, w, h, data_gen, zoom, vec![pane_idx]));
                }

                for (_key, (kind, bounds, w, h, data_gen, zoom, pane_indices)) in grouped {
                    log::debug!("Spawning overlay render for {:?} targeting {} panes", kind, pane_indices.len());
                    self.spawn_overlay_render(pane_indices, kind, bounds, w, h, data_gen, zoom);
                }
            } else {
                for (pane_idx, kind, bounds, w, h, data_gen, zoom) in overlay_renders {
                    self.spawn_overlay_render(vec![pane_idx], kind, bounds, w, h, data_gen, zoom);
                }
            }
        }
    }

    /// Poll all data channels for completed async results (scan, overlays).
    fn poll_data_channels(&mut self) {
        // Check for received scan data (with generation check)
        if let Ok(scan_resp) = self.channels.scan_receiver.try_recv() {
            if self.render.is_fetch_stale(&scan_resp.site, scan_resp.generation) {
                log::debug!("Discarding stale scan result for {} (gen {})", scan_resp.site, scan_resp.generation);
            } else {
                match scan_resp.result {
                    Ok(scan_data) => {
                        let scan_info = ScanInfo::from_scan(&scan_data.scan, &scan_data.site, scan_data.timestamp);
                        let site = scan_data.site;
                        let timestamp = scan_data.timestamp;
                        let scan_arc = Arc::new(scan_data.scan);

                        // When auto-poll delivers a new scan while viewing historic data,
                        // cache it silently for JumpToLive and loop growing.
                        if scan_resp.is_auto_poll && !self.gui.is_viewing_live() {
                            log::info!("Auto-poll: caching scan (historic mode) @ {}", timestamp);
                            self.append_scan_to_active_loops(timestamp, Arc::clone(&scan_arc));
                            self.latest_cached_scan = Some((scan_arc, scan_info, site, timestamp));
                        } else {
                            log::info!("Received scan data from background thread");
                            self.scan_data.insert(site.clone(), Arc::clone(&scan_arc));
                            self.gui.set_scan_info_for_site(&site, scan_info);
                            self.gui.set_loading_site(None);
                            self.render.reset_panes_for_site(&site, &self.gui);
                            self.spawn_level3_fetches(&site);

                            // Append the new scan to any active loops
                            self.append_scan_to_active_loops(timestamp, Arc::clone(&scan_arc));

                            // If this was a manual navigation, reinitialize active loops
                            if self.manual_nav_pending {
                                self.manual_nav_pending = false;
                                self.reinit_active_loops();
                            }

                            log::info!("Scan data loaded and UI updated");
                        }
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
            while let Ok(outlook_resp) = self.channels.outlook_receiver.try_recv() {
                any_received = true;
                match outlook_resp.result {
                    Ok(outlook) => {
                        log::info!("Received SPC outlook: {:?} {:?}", outlook_resp.day, outlook_resp.product);
                        self.gui.overlays.set_spc_outlook(outlook_resp.day, outlook_resp.product, outlook);
                    }
                    Err(e) => {
                        log::error!("SPC outlook fetch failed ({:?} {:?}): {}", outlook_resp.day, outlook_resp.product, e);
                    }
                }
            }
            if any_received {
                self.gui.overlays.set_spc_fetching(false);
            }
        }

        // Check for received NWS alerts data
        if let Ok(result) = self.channels.alert_receiver.try_recv() {
            match result {
                Ok(alerts) => {
                    log::info!("Received {} NWS alerts", alerts.len());
                    self.gui.overlays.set_nws_alerts(alerts);
                }
                Err(e) => {
                    log::error!("NWS alerts fetch failed: {}", e);
                }
            }
            self.gui.overlays.set_nws_fetching(false);
        }

        // Check for received SPC Mesoscale Discussion data
        if let Ok(result) = self.channels.discussion_receiver.try_recv() {
            match result {
                Ok(discussions) => {
                    log::info!("Received {} SPC Mesoscale Discussions", discussions.len());
                    self.gui.overlays.set_spc_discussions(discussions);
                }
                Err(e) => {
                    log::error!("SPC MD fetch failed: {}", e);
                }
            }
            self.gui.overlays.set_spc_md_fetching(false);
        }
    }

    /// Request application exit - handles both GUI and keyboard exit requests
    fn request_exit(&mut self, event_loop: Option<&ActiveEventLoop>) {
        // Persist UI config before exiting
        if let Some(config_dir) = self.platform.config_dir() {
            self.gui.save_ui_config(config_dir);
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

    /// Override the UI config directory and load config from it.
    pub fn set_config_dir(&mut self, dir: std::path::PathBuf) {
        self.platform.set_config_dir(dir);
        // Load config now — on Android this is called after App::new(),
        // so the initial load in new() had no config dir yet.
        if let Some(config_dir) = self.platform.config_dir() {
            self.gui.load_ui_config(config_dir);
        }
    }

    /// Set a receiver for GPS location updates (Android only).
    #[cfg(target_os = "android")]
    pub fn set_location_receiver(&mut self, receiver: std::sync::mpsc::Receiver<(f64, f64)>) {
        self.platform.set_location_receiver(receiver);
    }

    /// Set safe area insets in logical pixels (top, bottom, left, right).
    #[cfg(target_os = "android")]
    pub fn set_safe_area_insets(&mut self, top: f32, bottom: f32, left: f32, right: f32) {
        self.gui.set_safe_area_insets(top, bottom, left, right);
    }

    /// Set a callback that queries system bar insets.
    #[cfg(target_os = "android")]
    pub fn set_insets_querier(&mut self, querier: fn() -> (f32, f32, f32, f32)) {
        self.platform.set_insets_querier(querier);
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
        // Save config on suspend — on Android this is the only reliable save
        // point before the system may kill the process.
        if let Some(config_dir) = self.platform.config_dir() {
            self.gui.save_ui_config(config_dir);
        }
        self.old_textures.clear();
        self.render.clear_last_rendered();
        self.texture_counter = 0;
        self.gui.clear_graphics_state();        // Keep cached_render intact so we can re-upload the texture
        // immediately on resume without re-rendering.        // Clear both window and state so resumed() creates fresh ones.
        // Leaving state alive would keep a wgpu surface referencing the destroyed window.
        self.window = None;
        self.state = None;
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
                notify_redraw(&self.window);
            }
            WindowEvent::ThemeChanged(_theme) => {
                // Theme changed, clear cache so we re-detect on next frame
                self.cached_dark_theme = None;
                notify_redraw(&self.window);
            }
            _ => {
                // For other events, request redraw only if egui needs it
                if needs_repaint {
                    notify_redraw(&self.window);
                }
            }
        }
    }
}
