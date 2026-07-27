use egui_wgpu::wgpu;
use std::collections::HashMap;
use std::sync::Arc;
use winit::application::ApplicationHandler;
#[cfg(not(target_arch = "wasm32"))]
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::WindowRef;
use crate::app_state;
use crate::channels::ChannelHub;
// Only the default window size is used here, and only by the native arm of
// `create_window` — the web build takes its size from the canvas. A glob import
// would go unused on wasm32 and warn.
#[cfg(not(target_arch = "wasm32"))]
use crate::constants::{RENDER_HEIGHT, RENDER_WIDTH};
use crate::input::InputHandler;
use crate::loop_downloads::LoopDownloadManager;
use crate::platform::PlatformBridge;
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

/// Which wgpu backends this build will consider.
///
/// Native keeps reading `WGPU_BACKEND` from the environment. The browser has no
/// environment to read, and the choice there is not open: this build targets
/// WebGL2, so WebGPU has to be *excluded* rather than merely deprioritised.
/// Left in, wgpu would select it wherever it exists — which is Chrome but not
/// Firefox — and the two browsers would then run different, separately-broken
/// rendering paths off the same binary.
#[cfg(not(target_arch = "wasm32"))]
fn instance_descriptor() -> wgpu::InstanceDescriptor {
    wgpu::InstanceDescriptor::new_without_display_handle_from_env()
}

/// See the native variant above.
#[cfg(target_arch = "wasm32")]
fn instance_descriptor() -> wgpu::InstanceDescriptor {
    wgpu::InstanceDescriptor {
        backends: wgpu::Backends::GL,
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    }
}

/// Fails the build when this crate's two `wgpu` paths are different copies; the
/// notes below say why that matters, and `tests/wgpu_guard.rs` keeps this from
/// being edited into something vacuous.
///
/// Scope is this crate only — a second wgpu reached by another member is
/// invisible here, and to any Rust check. Nothing covers that today.
const _: () = {
    /// The `wgpu` entry in this crate's `Cargo.toml`.
    type OurWgpu = ::wgpu::Instance;
    /// The copy egui-wgpu links and renders through.
    type EguiWgpu = egui_wgpu::wgpu::Instance;

    #[diagnostic::on_unimplemented(
        message = "egui-wgpu links a different copy of `wgpu` than this crate configures",
        label = "this is egui-wgpu's `wgpu`, and it is not this crate's `wgpu`",
        note = "the backend features in rustdar-frontend/Cargo.toml apply to this crate's \
                copy, but rendering goes through egui-wgpu's; split, they configure nothing.",
        note = "egui-wgpu pins a wgpu major, so wgpu cannot move alone: bump egui, \
                egui-wgpu, egui-winit, walkers and wgpu together, and expect walkers to \
                gate it — it pins an exact egui minor. `cargo tree -i wgpu` lists the \
                copies that are in the graph now."
    )]
    trait IsOurWgpu {}

    impl IsOurWgpu for OurWgpu {}

    fn assert_is_our_wgpu<T: IsOurWgpu>() {}

    let _: fn() = assert_is_our_wgpu::<EguiWgpu>;
};

/// Check at compile time that the manifest's backend selection survived.
///
/// `Instance::enabled_backend_features` is a `const fn` over wgpu's own cfg
/// aliases, so this is the real compiled-in set, not a restatement of it.
/// Deliberately written `::wgpu::` rather than the `egui_wgpu::wgpu` re-export
/// imported above: this and the guard above are the only places that name the
/// *direct* dependency.
///
/// Two failures it turns into build errors.
///
/// **The `wgpu` entry in `Cargo.toml` going away.** It carries this crate's
/// entire per-target backend selection and nothing imports it — every `wgpu::`
/// path here comes through `egui_wgpu::wgpu`, which is what keeps a single wgpu
/// in the graph. That makes the entry look dead to `cargo machete`, to
/// `cargo udeps`, and to anyone tidying the manifest. Deleting it still
/// compiles: wgpu falls back to the `std` + `wgsl` egui-wgpu asks for, with no
/// backend at all, and the app dies at `request_adapter` instead. Naming the
/// crate here also makes the dependency genuinely used, so those tools stop
/// reporting it.
///
/// **`webgpu` coming back.** Features are additive across the graph, so any
/// dependency that turns on `wgpu/default` re-enables it regardless of what this
/// crate asks for — which is how the duplicate-bindings failure got in. A build
/// that has drifted back onto WebGPU now says so here rather than in a browser.
const _: () = {
    let enabled = ::wgpu::Instance::enabled_backend_features();

    assert!(
        !enabled.contains(::wgpu::Backends::BROWSER_WEBGPU),
        "wgpu's `webgpu` feature is enabled. This build targets WebGL2 because \
         Firefox has no stable WebGPU; something re-enabled `wgpu/default`."
    );

    // Only reachable when `web` is on and `webgl` is not. Dropping `webgl` on its
    // own never gets here: it implies `wgpu/web`, which gates `wgpu::web_sys`, so
    // egui-wgpu stops compiling first with E0433 and this crate is never built.
    #[cfg(target_arch = "wasm32")]
    assert!(
        enabled.contains(::wgpu::Backends::GL),
        "no WebGL2 backend compiled in — wgpu's `webgl` feature is off. Note \
         that `gles` does not cover the browser. See the wasm32 target section \
         of this crate's Cargo.toml."
    );

    #[cfg(not(target_arch = "wasm32"))]
    assert!(
        !enabled.is_empty(),
        "no native wgpu backend compiled in. See the per-target wgpu feature \
         sections of this crate's Cargo.toml."
    );
};

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

/// What one press of Escape or the back button resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackPress {
    /// A layer closed. The app stays, and nothing else about it changes.
    Dismissed,
    /// Nothing was open and the platform took the press — Android minimises.
    PlatformHandled,
    /// Nothing was open and nothing took it: leave.
    Exit,
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
    /// Native only. The browser supplies its own executor, so the web build
    /// spawns via `wasm_bindgen_futures` instead — see `App::spawn_detached`.
    #[cfg(not(target_arch = "wasm32"))]
    tokio_runtime: tokio::runtime::Runtime,
    /// Web only. Set while the async adapter/device request is in flight.
    ///
    /// Native resolves that request inside `ensure_rendering_state` and never
    /// needs to remember anything across frames; the browser forbids blocking,
    /// so the renderer arrives on a later frame and something has to hold the
    /// receiver until it does.
    #[cfg(target_arch = "wasm32")]
    pending_state: Option<std::sync::mpsc::Receiver<app_state::AppState>>,
    // Shared HTTP client for overlay data fetches (SPC, etc.)
    http_client: reqwest::Client,
    // Grouped loop download state: scan cache, in-flight tracking, and pending queues.
    loop_mgr: LoopDownloadManager,
    // Cached latest scan per site from auto-poll while panes on that site view historic data.
    latest_cached_scans: HashMap<String, (Arc<nexrad_model::data::Scan>, ScanInfo, chrono::NaiveDateTime)>,
    // Set when a manual time navigation fetch is pending; triggers loop reinit after scan loads.
    manual_nav_pending: bool,
    /// The map extent most recently asked for on screen.
    ///
    /// Fed to `FetchConfig::viewport` so overlays that fetch per-region data
    /// can scope their requests. `None` until the first frame that draws an
    /// overlay; `metar::networks::DEFAULT_VIEWPORT` covers that window.
    last_viewport: Option<rustdar_overlays::types::GeoBounds>,
}

impl App {
    /// Build the application around a caller-supplied platform bridge.
    ///
    /// The bridge is injected rather than constructed here so that this type
    /// stays free of any per-OS code: the concrete [`PlatformBridge`] impls
    /// live alongside their entry points, and only the entry point knows which
    /// one to build. Without that inversion the app layer and the platform
    /// layer would have to depend on each other.
    pub fn new(platform: Box<dyn PlatformBridge>) -> Self {
        Self::with_instance(egui_wgpu::wgpu::Instance::new(instance_descriptor()), platform)
    }

    /// Everything [`new`](Self::new) does once the wgpu instance exists.
    ///
    /// Split off so a test can supply an instance with no backends selected.
    /// `Instance::new(instance_descriptor())` opens the Vulkan and GL loaders
    /// and enumerates adapters — measured at ~72 ms per call on this machine,
    /// against ~1 µs for an empty one — and nothing an `App` does without a
    /// window ever asks it for a surface. The split is here rather than at the
    /// field so that everything else `new` wires up, `set_supports_exit` and
    /// the initial config load included, is on the tested side of it.
    fn with_instance(instance: wgpu::Instance, platform: Box<dyn PlatformBridge>) -> Self {
        let input = InputHandler::new();
        let channels = ChannelHub::new();
        // Owns the single shared render-budget counter used by both the loop and
        // static pane render paths (see `RenderDispatcher::renders_in_flight`).
        let render = RenderDispatcher::new();

        #[cfg(not(target_arch = "wasm32"))]
        let tokio_runtime = tokio::runtime::Runtime::new()
            .expect("Failed to create Tokio runtime");

        // Goes through `rustdar_radar::tls` rather than `reqwest::Client::builder`
        // directly: that is what installs the rustls crypto provider (no provider
        // is compiled in) and sets `https_only`. See `rustdar_radar::tls`.
        let http_client = rustdar_radar::tls::client(
            rustdar_radar::tls::USER_AGENT,
            std::time::Duration::from_secs(30),
        )
        .build()
        .expect("Failed to build HTTP client");

        let mut gui = Gui::new();
        gui.set_supports_exit(platform.supports_exit());
        if let Some(store) = platform.config_store() {
            gui.load_ui_config(store.as_ref());
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
            #[cfg(not(target_arch = "wasm32"))]
            tokio_runtime,
            #[cfg(target_arch = "wasm32")]
            pending_state: None,
            loop_mgr: LoopDownloadManager::new(),
            latest_cached_scans: HashMap::new(),
            manual_nav_pending: false,
            last_viewport: None,
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
        // A rotation moves the cutout and the navigation bar to other edges,
        // and it reaches the app as a resize — not as a resume. Queried once in
        // `resumed` and never again, the insets would describe the orientation
        // the app happened to start in for the rest of the session, and the map
        // would keep an exclusion band down the wrong side of the screen.
        //
        // A resize is also the only signal available that a *layout* has
        // happened, which is what `getRootWindowInsets` needs before it has
        // anything but the previous frame's numbers to return; see
        // `rustdar_android::get_system_insets`.
        //
        // Only on a real size. Android cannot distinguish a failed read from a
        // genuine zero -- `get_system_insets` collapses every JNI failure,
        // including a null `getRootWindowInsets()` before the first layout, to
        // all-zero -- so querying at 0x0 replaces good insets with bad ones.
        if width > 0 && height > 0 {
            self.refresh_safe_area_insets();
        }
        if width > 0
            && height > 0
            && let Some(state) = self.state.as_mut()
        {
            log::info!("Window resized to {}x{}", width, height);
            state.resize_surface(width, height);
        }
    }

    /// Ask the platform what the system bars are covering and hand it to the UI.
    ///
    /// A bridge with nothing to say answers `None` and the last value stands
    /// rather than being zeroed: desktop has no system bars, and on iOS
    /// egui-winit fills `RawInput::safe_area_insets` itself, so writing zeros
    /// here would be this code overriding the platform's own answer with a
    /// worse one.
    ///
    /// Android is the only platform that answers `Some`, and it answers
    /// all-zero for a failed read as readily as for a real one, so callers
    /// must not ask unless a layout has actually happened.
    fn refresh_safe_area_insets(&mut self) {
        if let Some((top, bottom, left, right)) = self.platform.query_insets() {
            self.gui.set_safe_area_insets(top, bottom, left, right);
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

        // Skip rendering a window with no area.
        //
        // On web this is the *normal* state of the first frame or two, not an
        // edge case: winit's web backend serves `inner_size()` from a cell that
        // starts at zero and is written only when the ResizeObserver it installs
        // on the canvas first fires, which is after the initial redraw.
        //
        // Rendering anyway does not fail cleanly. The surface gets configured at
        // one pixel, egui lays the UI out inside a degenerate rect, and the map
        // code then unprojects that rect into latitudes far outside the world —
        // `draw_label_tiles_overlay` turns those into a tile index of `u32::MAX`
        // and panics on the `+ 1`. On wasm a panic is unrecoverable, so the app
        // dies on frame one and the resize that would have fixed everything
        // never arrives.
        if let Some(window) = self.window.as_ref() {
            let size = window.inner_size();
            if size.width == 0 || size.height == 0 {
                log::debug!("Window has zero area ({}x{}); skipping frame", size.width, size.height);
                return;
            }
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

    /// Poll for platform-specific theme, GPS fix, and compass heading changes.
    fn poll_platform_state(&mut self) {
        if let Some(new_theme) = self.platform.poll_theme()
            && self.cached_dark_theme != Some(new_theme) {
                self.cached_dark_theme = Some(new_theme);
                self.gui.bump_all_radar_sites_gen();
                notify_redraw(&self.window);
            }
        if let Some(fix) = self.platform.poll_gps_fix() {
            self.gui.set_gps_fix(fix);
        }
        if let Some(heading) = self.platform.poll_heading() {
            self.gui.set_user_heading(heading);
        }
    }

    /// Lazily initialize wgpu rendering state on first redraw after window creation.
    #[cfg(not(target_arch = "wasm32"))]
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

    /// See the native variant above.
    ///
    /// The browser cannot block on a future. `pollster::block_on` here would
    /// spin forever rather than deadlock loudly: the executor that resolves an
    /// adapter request *is* the event loop being blocked, so the future it is
    /// waiting on can never be polled. The request is therefore spawned and its
    /// result collected on a later frame, which is the whole reason this arm is
    /// a state machine and the native one is a straight line.
    #[cfg(target_arch = "wasm32")]
    fn ensure_rendering_state(&mut self) {
        // A request already in flight: collect it if it has landed.
        if let Some(rx) = self.pending_state.as_ref() {
            match rx.try_recv() {
                Ok(state) => {
                    self.pending_state = None;
                    self.state = Some(state);
                    self.restore_cached_render();
                }
                // Still running — nothing to do until the redraw it will post.
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                // The task was dropped without sending. Clearing the slot lets a
                // later frame retry instead of wedging forever on a dead
                // receiver, which is what leaving it in place would do.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending_state = None,
            }
            return;
        }

        if self.state.is_some() {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };

        let size = window.inner_size();
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_state = Some(rx);

        let instance = self.instance.clone();
        let redraw_target = self.window.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let state = Self::initialize_rendering_state(
                &instance,
                &window,
                size.width.max(1),
                size.height.max(1),
            )
            .await;
            let _ = tx.send(state);
            // The frame that kicked this off returned without a renderer, and
            // under `ControlFlow::Wait` nothing schedules another frame on its
            // own. Without this redraw the app would sit on a blank canvas
            // holding a perfectly good `AppState` it never collects.
            notify_redraw(&redraw_target);
        });
    }

    /// Process all GUI actions emitted during this frame.
    fn process_gui_actions(&mut self, actions: Vec<GuiAction>) {
        use rustdar_overlays::render::overlay_state::OverlayKind;

        // Separate overlay render actions for deduplication
        let mut overlay_renders: Vec<(usize, OverlayKind, fetch::OverlayRenderRequest)> = Vec::new();

        for action in actions {
            if let GuiAction::RenderOverlay { pane_idx, overlay_kind, geo_bounds, texture, data_generation, zoom } = action {
                // The unexpanded viewport, which is what a region-scoped fetch
                // wants — the renderer's overdraw margin is a rasterization
                // concern and would over-fetch if it leaked into the request.
                self.last_viewport = Some(geo_bounds);
                overlay_renders.push((pane_idx, overlay_kind, fetch::OverlayRenderRequest {
                    geo_bounds, texture, data_generation, zoom,
                }));
            } else {
                log::debug!("GUI action received: {}", action);
                self.handle_gui_action(action, None);
            }
        }

        if !overlay_renders.is_empty() {
            let should_group = self.gui.is_viewport_sync() && self.gui.is_sync_layers();
            let grouped = deduplicate_overlay_renders(overlay_renders, should_group);
            for (pane_indices, kind, req) in grouped {
                if should_group {
                    log::debug!("Spawning overlay render for {:?} targeting {} panes", kind, pane_indices.len());
                }
                self.spawn_overlay_render(pane_indices, kind, req);
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

                        // When auto-poll delivers a new scan, check if any pane
                        // on this site is viewing live. If all panes on this site
                        // are historic, cache silently for JumpToLive.
                        let any_pane_live_for_site = scan_resp.is_auto_poll && {
                            let count = self.gui.pane_count();
                            (0..count).any(|i| {
                                self.gui.pane(i).is_some_and(|p| p.site == site && p.viewing_live)
                            })
                        };
                        if scan_resp.is_auto_poll && !any_pane_live_for_site {
                            log::info!("Auto-poll: caching scan (historic mode) @ {}", timestamp);
                            self.append_scan_to_active_loops(&site, timestamp, Arc::clone(&scan_arc));
                            self.latest_cached_scans.insert(site, (scan_arc, scan_info, timestamp));
                        } else {
                            log::info!("Received scan data from background thread");
                            self.scan_data.insert(site.clone(), Arc::clone(&scan_arc));
                            self.gui.set_scan_info_for_site(&site, scan_info);
                            self.gui.clear_loading_site_for_site(&site);
                            self.render.reset_panes_for_site(&site, &self.gui);
                            self.spawn_level3_fetches(&site);

                            // Append the new scan to any active loops on this site
                            self.append_scan_to_active_loops(&site, timestamp, Arc::clone(&scan_arc));

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
                        self.gui.clear_loading_site_for_site(&scan_resp.site);
                    }
                }
            }
        }

        // Check for received overlay fetch results (unified channel)
        while let Ok(result) = self.channels.overlay_fetch_receiver.try_recv() {
            self.gui.overlays.apply_fetch_result(result);
        }
    }

    /// Request application exit - handles both GUI and keyboard exit requests
    fn request_exit(&mut self, event_loop: Option<&ActiveEventLoop>) {
        // Persist UI config before exiting
        if let Some(store) = self.platform.config_store() {
            self.gui.save_ui_config(store.as_ref());
        }
        if !self.platform.supports_exit() {
            // The config save above still ran, which is the part that matters.
            log::debug!("exit requested; ignored (this platform has no quit)");
            return;
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
        if let Some(store) = self.platform.config_store() {
            self.gui.load_ui_config(store.as_ref());
        }
    }

    // The three below are forwards to trait methods that are *not* gated: the
    // bridge declares them for every platform with a no-op default, and only
    // Android and the web override any of them. Gating the forwards on
    // `target_os = "android"` therefore bought nothing and cost twice: the web
    // entry point had to reach past `App` and call the trait method on its own
    // bridge before boxing it, and a host build — which is every build the
    // tests run in — compiled none of this, so nothing here could be exercised
    // anywhere. `set_theme_detector` beside them was never gated at all.
    //
    // `set_safe_area_insets` used to sit here too and is gone. It pushed insets
    // straight at the UI, and no entry point has called it since Android
    // switched to injecting a querier; the live route is `set_insets_querier`
    // -> `query_insets` -> `refresh_safe_area_insets`.

    /// Set a receiver for GPS fix updates. Android and the web send fixes this
    /// way; desktop reads a serial port instead, through `start_gps`.
    pub fn set_gps_fix_receiver(&mut self, receiver: std::sync::mpsc::Receiver<rustdar_gps::GpsFix>) {
        self.platform.set_gps_fix_receiver(receiver);
    }

    /// Set a receiver for compass heading updates (Android only).
    pub fn set_heading_receiver(&mut self, receiver: std::sync::mpsc::Receiver<f32>) {
        self.platform.set_heading_receiver(receiver);
    }

    /// Set a callback that queries system bar insets (Android only).
    pub fn set_insets_querier(&mut self, querier: fn() -> (f32, f32, f32, f32)) {
        self.platform.set_insets_querier(querier);
    }

    /// Set a callback that reads the OS dark-theme preference (Android only).
    pub fn set_theme_detector(&mut self, detector: fn() -> bool) {
        self.platform.set_theme_detector(detector);
    }

    /// Set a callback that takes a back press delivered outside the input
    /// queue (Android's `OnBackInvokedDispatcher`; see
    /// [`PlatformBridge::poll_back_press`]).
    pub fn set_back_press_taker(&mut self, taker: fn() -> bool) {
        self.platform.set_back_press_taker(taker);
    }

    fn handle_input_events(&mut self, event_loop: &ActiveEventLoop) {
        // Both keys mean the same thing — back out of the thing I am in — so
        // both take the same route. They used to differ only in that back gave
        // the platform first refusal, and on Android a handler is always
        // installed, so back never reached any of the decisions below it.
        // Taken, not read: this runs on every keyboard press, not once a frame.
        if self.input.take_back_out_press() {
            self.back_out(event_loop);
        }
    }

    /// One press of Escape or the back button.
    ///
    /// Three callers, one body: `handle_input_events` for Escape and for
    /// `KEYCODE_BACK` off the input queue, and `about_to_wait` for a press
    /// Android's `OnBackInvokedDispatcher` delivered instead. Anything that
    /// makes a route to `resolve_back_press` its own is the bug this shape
    /// exists to prevent — the predictive-back callback used to be exactly
    /// that, minimising on its own with no route into Rust at all.
    fn back_out(&mut self, event_loop: &ActiveEventLoop) {
        match Self::resolve_back_press(&mut self.gui, self.platform.as_ref()) {
            // Nothing else consumed the press, so nothing else will schedule
            // the frame that shows the layer gone.
            BackPress::Dismissed => notify_redraw(&self.window),
            BackPress::PlatformHandled => {}
            BackPress::Exit => self.request_exit(Some(event_loop)),
        }
    }

    /// Resolve one press of Escape or back.
    ///
    /// The single decision for every route in: Escape, `KEYCODE_BACK` off the
    /// input queue, and Android's `OnBackInvokedDispatcher`. The last of those
    /// is a Java callback that could perfectly well minimise for itself, and
    /// deliberately does not — see `BackHandler.java`.
    ///
    /// The UI gets first refusal and the platform is asked only about a press
    /// it did not want. That order is the whole fix: on Android a back handler
    /// is always installed, so [`PlatformBridge::handle_back`] reports every
    /// press consumed, and asking it first meant nothing after it was ever
    /// asked at all — one press with the drawer open minimised the app.
    ///
    /// Takes the two collaborators rather than `&mut self` so the decision can
    /// be exercised without an event loop or a GPU.
    fn resolve_back_press(gui: &mut Gui, platform: &dyn PlatformBridge) -> BackPress {
        if gui.dismiss_top_layer() {
            return BackPress::Dismissed;
        }
        if platform.handle_back() {
            return BackPress::PlatformHandled;
        }
        BackPress::Exit
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        // The bridge gets to amend the attributes because the web backend has to
        // bind its canvas here and nowhere else. See `PlatformBridge::window_attributes`.
        let attributes = self
            .platform
            .window_attributes(Window::default_attributes().with_title("Rustdar"));
        let window = event_loop.create_window(attributes).unwrap();

        let window = Arc::new(window);
        // Native opens at a fixed default size. On web the canvas already has
        // whatever size the page's layout gave it, and overriding that with a
        // 1920x1080 backing store would both ignore the layout and, at a
        // devicePixelRatio above 1, ask for a surface past WebGL2's texture
        // ceiling — which is a validation error, not a clamp.
        #[cfg(not(target_arch = "wasm32"))]
        let _ = window.request_inner_size(PhysicalSize::new(RENDER_WIDTH, RENDER_HEIGHT));
        self.window = Some(window.clone());

        // Rendering state is initialized lazily in handle_redraw().
        // This keeps resumed() fast on Android, preventing ANRs during
        // configuration changes (e.g. folding/unfolding the device).
        window.request_redraw();
    }
}

/// Deduplicate overlay render requests.
///
/// When `should_group` is true (viewport sync + layer sync both on), groups requests
/// by `(overlay_kind, zoom, data_generation, width, height)` and merges pane indices
/// so one render serves multiple panes. When false, each request passes through as-is.
///
/// The overdraw fraction is deliberately absent from the key. It is a function of the
/// pane's size and the one adapter limit, so two requests that already agree on width
/// and height cannot disagree about it — keying on it would only add a field that is
/// always equal when the rest are.
fn deduplicate_overlay_renders(
    overlay_renders: Vec<(usize, rustdar_overlays::render::overlay_state::OverlayKind, fetch::OverlayRenderRequest)>,
    should_group: bool,
) -> Vec<(Vec<usize>, rustdar_overlays::render::overlay_state::OverlayKind, fetch::OverlayRenderRequest)> {
    use rustdar_overlays::render::overlay_state::OverlayKind;

    if !should_group {
        return overlay_renders
            .into_iter()
            .map(|(pane_idx, kind, req)| (vec![pane_idx], kind, req))
            .collect();
    }

    struct GroupedRender {
        kind: OverlayKind,
        req: fetch::OverlayRenderRequest,
        pane_indices: Vec<usize>,
    }

    let mut grouped: HashMap<(OverlayKind, i32, u64, u32, u32), GroupedRender> = HashMap::new();

    for (pane_idx, kind, req) in overlay_renders {
        let key = (kind, req.zoom, req.data_generation, req.texture.width, req.texture.height);
        grouped.entry(key)
            .and_modify(|g| {
                if !g.pane_indices.contains(&pane_idx) {
                    g.pane_indices.push(pane_idx);
                }
            })
            .or_insert_with(|| GroupedRender {
                kind,
                req,
                pane_indices: vec![pane_idx],
            });
    }

    grouped.into_values().map(|g| (g.pane_indices, g.kind, g.req)).collect()
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        log::info!("App resumed");
        self.create_window(event_loop);

        // Query system bar insets now that the window is ready. Not the only
        // query — see `handle_resized`, which catches the orientation changes
        // that never come back through here.
        self.refresh_safe_area_insets();
    }

    /// Pick up a back press the platform delivered outside the input queue.
    ///
    /// Android's predictive-back dispatcher hands the press to a Java callback
    /// on the UI thread, which parks it and wakes this loop with
    /// `EventLoopProxy::send_event` — the flag alone would not do, because
    /// winit's Android backend drops a bare wake unless the loop is running
    /// *and* a redraw or user event is already outstanding. (Which also means a
    /// press that arrives while the app is paused waits for the resume; the
    /// dispatcher does not deliver one there anyway.) Everywhere else
    /// `poll_back_press` is the trait's `false` default and this costs one load
    /// per iteration.
    ///
    /// Here rather than in `user_event` so the press is spent on the iteration
    /// it arrived in even if the wake coalesced with a real event, and so the
    /// funnel does not depend on *which* winit callback the wake surfaces as.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.platform.poll_back_press() {
            self.back_out(event_loop);
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        log::info!("App suspended — clearing graphics state");
        // Save config on suspend — on Android this is the only reliable save
        // point before the system may kill the process.
        if let Some(store) = self.platform.config_store() {
            self.gui.save_ui_config(store.as_ref());
        }
        self.old_textures.clear();
        self.render.clear_last_rendered();
        self.texture_counter = 0;
        self.gui.clear_graphics_state();        // Keep cached_render intact so we can re-upload the texture
        // immediately on resume without re-rendering.        // Clear both window and state so resumed() creates fresh ones.
        // Leaving state alive would keep a wgpu surface referencing the destroyed window.
        self.window = None;
        self.state = None;
        // An init in flight targets the window just dropped. Leaving the
        // receiver in place would let `ensure_rendering_state` collect an
        // `AppState` holding a surface for a destroyed window and treat it as
        // current, which is worse than starting the request over.
        #[cfg(target_arch = "wasm32")]
        {
            self.pending_state = None;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform_double::TestBridge;
    use rustdar_egui::config_store::MemoryConfigStore;
    use rustdar_egui::overlay_cache::OverlayTexturePlan;
    use rustdar_overlays::render::overlay_state::OverlayKind;
    use rustdar_overlays::types::GeoBounds;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn bounds() -> GeoBounds {
        GeoBounds { min_lat: 30.0, max_lat: 40.0, min_lon: -100.0, max_lon: -90.0 }
    }

    /// A request as `process_gui_actions` builds one: unexpanded viewport bounds
    /// plus a texture plan.
    fn req(w: u32, h: u32, overdraw: f32, data_gen: u64, zoom: i32) -> fetch::OverlayRenderRequest {
        fetch::OverlayRenderRequest {
            geo_bounds: bounds(),
            texture: OverlayTexturePlan { width: w, height: h, overdraw },
            data_generation: data_gen,
            zoom,
        }
    }

    fn entry(pane: usize, kind: OverlayKind) -> (usize, OverlayKind, fetch::OverlayRenderRequest) {
        (pane, kind, req(800, 600, 1.0, 1, 10))
    }

    #[test]
    fn test_dedup_empty() {
        let result = deduplicate_overlay_renders(vec![], true);
        assert!(result.is_empty());
        let result = deduplicate_overlay_renders(vec![], false);
        assert!(result.is_empty());
    }

    #[test]
    fn test_dedup_single_render() {
        let result = deduplicate_overlay_renders(vec![entry(0, OverlayKind::Radar)], true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, vec![0]);
        assert_eq!(result[0].1, OverlayKind::Radar);
        assert_eq!(result[0].2.texture.width, 800);

        let result = deduplicate_overlay_renders(vec![entry(0, OverlayKind::Radar)], false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, vec![0]);
    }

    #[test]
    fn test_dedup_no_grouping() {
        let input = vec![
            entry(0, OverlayKind::Radar),
            entry(1, OverlayKind::Radar),
            entry(2, OverlayKind::NwsAlerts),
        ];

        let result = deduplicate_overlay_renders(input, false);
        assert_eq!(result.len(), 3);
        for e in &result {
            assert_eq!(e.0.len(), 1);
        }
    }

    #[test]
    fn test_dedup_groups_same_key() {
        let input = vec![entry(0, OverlayKind::Radar), entry(1, OverlayKind::Radar)];

        let result = deduplicate_overlay_renders(input, true);
        assert_eq!(result.len(), 1);
        let mut panes = result[0].0.clone();
        panes.sort();
        assert_eq!(panes, vec![0, 1]);
        assert_eq!(result[0].1, OverlayKind::Radar);
    }

    #[test]
    fn test_dedup_different_keys() {
        let input = vec![entry(0, OverlayKind::Radar), entry(1, OverlayKind::NwsAlerts)];

        let result = deduplicate_overlay_renders(input, true);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_dedup_duplicate_pane_idx() {
        let input = vec![entry(0, OverlayKind::Radar), entry(0, OverlayKind::Radar)];

        let result = deduplicate_overlay_renders(input, true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, vec![0]);
    }

    /// Panes of different sizes must not share one render: the survivor's plan would
    /// be applied to a pane it was not sized for. Width is part of the key, and the
    /// overdraw that travels with it has to survive grouping intact.
    #[test]
    fn test_dedup_keeps_differently_sized_panes_apart() {
        let input = vec![
            (0, OverlayKind::Radar, req(2048, 600, 0.28, 1, 10)),
            (1, OverlayKind::Radar, req(2400, 600, 1.0, 1, 10)),
        ];

        let mut result = deduplicate_overlay_renders(input, true);
        assert_eq!(result.len(), 2, "different texture widths are different renders");
        result.sort_by_key(|e| e.2.texture.width);
        assert_eq!(result[0].2.texture.width, 2048);
        assert_eq!(result[0].2.texture.overdraw, 0.28, "the clamped plan's overdraw survived grouping");
        assert_eq!(result[1].2.texture.overdraw, 1.0);
    }

    /// A bridge that consumes every back press, as Android's does: it installs
    /// a handler at startup and `handle_back` reports `true` from then on.
    fn minimising_bridge() -> TestBridge {
        let mut bridge = TestBridge::android();
        // Deliberately not `record_back_press`: that one's flag belongs to
        // `the_injected_callbacks_reach_the_bridge` alone. Tests run in
        // parallel, and a second writer could set it while that test is
        // asserting — which would only ever make it pass, which is worse.
        bridge.set_back_handler(|| {});
        bridge
    }

    /// Back with something open closes it; only a second press, with nothing
    /// open, minimises.
    ///
    /// The bug is an *ordering* one, which is why the platform here consumes
    /// everything: `handle_back` used to be asked first, and on Android a
    /// handler is always installed, so it always said yes — the UI was never
    /// consulted and one press with the drawer open went straight to minimise.
    ///
    /// Opens the settings window rather than the drawer only because
    /// `show_settings` is the dismissible state this crate can reach.
    /// `dismiss_top_layer`'s own coverage of the drawer, and of the one-layer-
    /// per-press rule, is in `rustdar-egui`'s `ui_menu` tests.
    #[test]
    fn back_closes_what_is_open_before_it_minimises() {
        let mut gui = Gui::new();
        let platform = minimising_bridge();
        gui.show_settings = true;

        assert_eq!(
            App::resolve_back_press(&mut gui, &platform),
            BackPress::Dismissed,
            "the first press left the app with a window still open"
        );
        assert!(!gui.show_settings, "the window is still open");

        assert_eq!(
            App::resolve_back_press(&mut gui, &platform),
            BackPress::PlatformHandled,
            "with nothing open, back must reach the platform and minimise"
        );
    }

    /// The two tests above exercise the decision; nothing can exercise the
    /// call that reaches it, because `handle_input_events` takes an
    /// `ActiveEventLoop` and winit will not hand one out except from inside a
    /// running loop. Reading the source is the only handle, as it is for
    /// `egui_renderer`'s `begin_frame`.
    fn fn_body(name: &str) -> &'static str {
        let (_, rest) = include_str!("app.rs")
            .split_once(name)
            .unwrap_or_else(|| panic!("{name} is no longer a method here"));
        rest.split_once("\n    }")
            .map(|(body, _)| body)
            .unwrap_or_else(|| panic!("{name} has no recognisable body"))
    }

    /// A press has to actually reach the funnel.
    ///
    /// Both keys go through one call and one route, so this is the whole
    /// wiring: drop either and Escape and back do nothing at all, with the
    /// decision tests still green because they call `resolve_back_press`
    /// directly.
    ///
    /// `take_back_out_press` rather than a plain read is part of the claim —
    /// `handle_input_events` runs on every keyboard press, so a non-consuming
    /// read spends one press on two layers.
    #[test]
    fn every_back_out_press_reaches_the_funnel_exactly_once() {
        let body = fn_body("fn handle_input_events(");
        for call in ["take_back_out_press(", "self.back_out("] {
            assert!(
                body.contains(call),
                "handle_input_events no longer calls {call}, so Escape and the \
                 back button reach nothing: {body}"
            );
        }
    }

    /// A dismissal has to schedule the frame that shows it.
    ///
    /// Nothing else consumed the press, so nothing else requests a redraw: drop
    /// this and the drawer stays on screen until something unrelated repaints.
    /// `WindowRef` cannot be built without a window, so the source is again the
    /// only handle.
    #[test]
    fn a_dismissal_asks_for_the_frame_that_shows_it() {
        let body = fn_body("fn back_out(");
        let dismissed = body
            .find("BackPress::Dismissed")
            .expect("back_out no longer handles a dismissal");
        let arm_end = body[dismissed..]
            .find('\n')
            .map(|i| dismissed + i)
            .unwrap_or(body.len());
        assert!(
            body[dismissed..arm_end].contains("notify_redraw("),
            "the Dismissed arm does not request a redraw: {}",
            &body[dismissed..arm_end]
        );
    }

    // ── The second delivery route: Android's predictive back ────────────
    //
    // `OnBackInvokedDispatcher` does not go through the input queue, so none of
    // the pins above see it. It also does not go through this process's main
    // thread: the press lands on a Java callback, which parks it and wakes the
    // loop, and `about_to_wait` collects it. What has to hold is that it ends
    // in the *same* `resolve_back_press` — which the decision tests above
    // already cover once a press gets there.

    /// The Java half of the route, so a rename on either side is a build
    /// failure rather than an `UnsatisfiedLinkError` on a device.
    const BACK_HANDLER_JAVA: &str = include_str!(
        "../../rustdar-android/android/app/src/main/java/com/rustdar/BackHandler.java"
    );

    /// The Rust half. `rustdar-android` is `#![cfg(target_os = "android")]`, so
    /// it compiles to nothing on a host and can hold no test of its own; this
    /// crate owns the funnel both halves are about, so the pins live here.
    const ANDROID_ENTRY: &str = include_str!("../../rustdar-android/src/lib.rs");

    /// `src` with its Java comments removed.
    ///
    /// The pins below are about the order two calls happen in, and the prose
    /// around them necessarily names both — the first draft failed on its own
    /// javadoc. Deliberately naive: it would mangle a `//` inside a string
    /// literal, and there is none in this file.
    fn java_code(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut rest = src;
        while let Some(slash) = rest.find('/') {
            let (kept, tail) = rest.split_at(slash);
            out.push_str(kept);
            if let Some(body) = tail.strip_prefix("/*") {
                rest = body.split_once("*/").map_or("", |(_, after)| after);
            } else if let Some(body) = tail.strip_prefix("//") {
                rest = body.split_once('\n').map_or("", |(_, after)| after);
            } else {
                // A lone '/' opens nothing. Keep it and move past it.
                out.push('/');
                rest = &tail[1..];
            }
        }
        out.push_str(rest);
        out
    }

    /// A press delivered outside the input queue has to reach the same funnel,
    /// and only when there *is* one.
    ///
    /// `about_to_wait` takes an `ActiveEventLoop`, so this is a source probe for
    /// the same reason `handle_input_events` is. Three claims, and the third is
    /// the one a substring pair missed: without `poll_back_press` the press is
    /// never collected; without `self.back_out` it is collected and thrown
    /// away; and with the poll demoted out of the condition — `let _ =
    /// self.platform.poll_back_press(); self.back_out(event_loop);` — this runs
    /// on *every* iteration of the loop and the UI dismantles itself. So the
    /// call is pinned as the `if`, not merely as present.
    #[test]
    fn a_back_press_from_the_platform_reaches_the_funnel_too() {
        let body = fn_body("fn about_to_wait(");
        assert!(
            body.contains("if self.platform.poll_back_press() {"),
            "the platform back press is no longer what gates the funnel, so \
             about_to_wait either drops it or backs out on every iteration: \
             {body}"
        );
        assert!(
            body.contains("self.back_out("),
            "about_to_wait collects the press and does nothing with it: {body}"
        );
    }

    /// The two ends of the JNI hop must agree on one name.
    ///
    /// It is resolved by string at runtime and by nothing at build time, so a
    /// rename on either side compiles, links, ships, and then throws
    /// `UnsatisfiedLinkError` on the first back press — where the Java
    /// fallback catches it and minimises, which is indistinguishable from the
    /// bug this route exists to remove.
    #[test]
    fn the_java_callback_calls_the_symbol_rust_exports() {
        let java = java_code(BACK_HANDLER_JAVA);
        assert!(
            java.contains("package com.rustdar;")
                && java.contains("class BackHandler")
                && java.contains("native boolean nativeBackPressed()"),
            "the Java side no longer declares com.rustdar.BackHandler.nativeBackPressed",
        );
        assert!(
            ANDROID_ENTRY.contains("fn Java_com_rustdar_BackHandler_nativeBackPressed("),
            "nothing exports the symbol BackHandler.nativeBackPressed() binds to",
        );
    }

    /// Offsets of every *call* to `name`, skipping the line that declares it.
    ///
    /// The declaration and the call are spelled the same, and an earlier draft
    /// of the pin below matched the first of either. A review moved
    /// `private static native boolean nativeBackPressed();` above the method and
    /// rewrote the body to minimise first and ask second — the regression the
    /// pin is named for — and it passed, because the declaration was now the
    /// first match. A `native` keyword on the line is what tells them apart.
    fn call_sites(java: &str, name: &str) -> Vec<usize> {
        java.match_indices(name)
            .map(|(at, _)| at)
            .filter(|at| {
                let line = java[..*at].rfind('\n').map_or(0, |nl| nl + 1);
                !java[line..*at].contains("native ")
            })
            .collect()
    }

    /// The bomb this route was built to defuse.
    ///
    /// The callback used to be `() -> activity.moveTaskToBack(true)`: no route
    /// into Rust at all, inert only because the manifest has not opted in and
    /// targetSdk is 34. Raising targetSdk opts the app in, and back would have
    /// gone straight back to minimising on the first press with the drawer
    /// open — no test failing, nothing logged.
    ///
    /// So: every minimise in this class must come after the class has asked
    /// Rust. The one `moveTaskToBack` left is the fallback for a press with no
    /// event loop to route to, and it sits after the call that asks.
    ///
    /// Deliberately ordered across the whole class rather than within one
    /// method: a minimise hoisted into a helper *defined earlier in the file*
    /// would fail this even if it still ran after the call. That is the safe
    /// direction to be wrong in, and the class is sixty lines of code.
    #[test]
    fn the_predictive_back_callback_asks_rust_before_it_minimises() {
        let java = java_code(BACK_HANDLER_JAVA);
        assert!(
            java.contains("registerOnBackInvokedCallback"),
            "BackHandler no longer registers a callback",
        );

        let asks = *call_sites(&java, "nativeBackPressed(")
            .first()
            .expect("BackHandler declares the native funnel but never calls it");

        for minimises in call_sites(&java, "moveTaskToBack(") {
            assert!(
                minimises > asks,
                "BackHandler minimises before it asks Rust, so one press with \
                 the drawer open minimises the app",
            );
        }
        assert!(
            java.matches("moveTaskToBack(").count() <= 1,
            "a second minimise appeared in BackHandler; the one this class is \
             allowed is the fallback for a press with no event loop to route to",
        );
    }

    /// Set by `one_press` below. A `fn` pointer closes over nothing, which is
    /// the constraint the real taker is under too — it reads a `static` a JNI
    /// entry point on the UI thread wrote.
    static PARKED_BACK_PRESS: AtomicBool = AtomicBool::new(false);

    fn one_press() -> bool {
        PARKED_BACK_PRESS.swap(false, Ordering::Relaxed)
    }

    /// The taker has to reach the bridge, and it has to *consume*.
    ///
    /// `about_to_wait` runs every loop iteration, so a non-consuming read would
    /// spend one gesture on every layer the UI has open — the drawer, the
    /// settings window and the time dialog would all vanish together, and then
    /// the app would minimise.
    #[test]
    fn a_parked_back_press_is_collected_once() {
        let mut app = headless(TestBridge::android());
        PARKED_BACK_PRESS.store(true, Ordering::Relaxed);
        assert!(
            !app.platform.poll_back_press(),
            "precondition: nothing injected yet, so there is nothing to collect",
        );

        app.set_back_press_taker(one_press);

        assert!(
            app.platform.poll_back_press(),
            "the parked press never reached the bridge",
        );
        assert!(
            !app.platform.poll_back_press(),
            "the press was not consumed, so it fires again on the next iteration",
        );
    }

    /// No bridge may invent a press. `about_to_wait` runs on every iteration of
    /// every platform's loop, so a bridge answering `true` on its own would
    /// close a layer per iteration and then minimise, for a gesture nobody
    /// made. Desktop and iOS never get a taker at all; Android has none until
    /// `android_main` injects one.
    #[test]
    fn no_bridge_invents_a_back_press() {
        for (name, mut bridge) in [
            ("desktop", TestBridge::desktop()),
            ("ios", TestBridge::ios()),
            ("android, before android_main injects the taker", TestBridge::android()),
        ] {
            assert!(
                !bridge.poll_back_press(),
                "{name} reported a back press nobody delivered",
            );
        }
    }

    /// The same press on a platform with no back handler: Escape on the desktop
    /// and the browser's back. Nothing open means quit, and quitting must stay
    /// reachable — a dismissal that reported itself with nothing open would
    /// make the app unquittable.
    #[test]
    fn escape_with_nothing_open_still_exits() {
        let mut gui = Gui::new();
        let platform = TestBridge::desktop();
        gui.show_settings = true;

        assert_eq!(
            App::resolve_back_press(&mut gui, &platform),
            BackPress::Dismissed,
            "escape must close the window rather than quit, same as back"
        );
        assert_eq!(App::resolve_back_press(&mut gui, &platform), BackPress::Exit);
    }

    // ── Driving a whole `App` ───────────────────────────────────────────
    //
    // Everything below builds one. Two things used to make that impossible and
    // only one of them was real: `App::new` builds a `wgpu::Instance` and a
    // Tokio runtime, and it needs a `PlatformBridge`. The bridge is now
    // `platform_double::TestBridge`; the instance is built with no backends,
    // which is the whole of `with_instance`'s reason to exist. A texture upload
    // was also blamed and is not an obstacle at all — a bare `egui::Context`
    // uploads perfectly well with no renderer behind it, which is what
    // `app_render`'s tests rely on.

    /// An `App` with no GPU behind it, wired the way `App::new` wires one.
    pub(super) fn headless(platform: TestBridge) -> App {
        App::with_instance(
            egui_wgpu::wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::empty(),
                ..instance_descriptor()
            }),
            Box::new(platform),
        )
    }

    /// A loop speed no default produces, so finding it can only mean the stored
    /// config was read.
    const STORED_FPS: f32 = 9.25;

    /// Write a config the way the app writes one, rather than by hand: a
    /// literal blob would stop matching the format the moment it changed and
    /// would then be testing nothing.
    fn seed_config(store: &MemoryConfigStore, fps: f32) {
        let mut gui = Gui::new();
        gui.loop_speed_fps = fps;
        gui.save_ui_config(store);
    }

    /// What a bridge's store holds, read back through the same parser the app
    /// loads with.
    fn stored_fps(store: &MemoryConfigStore) -> f32 {
        let mut reloaded = Gui::new();
        reloaded.load_ui_config(store);
        reloaded.loop_speed_fps
    }

    /// Set by the back handler the app installs, so a test can see it *ran*
    /// rather than merely being held somewhere.
    static BACK_PRESS_REACHED_THE_HANDLER: AtomicBool = AtomicBool::new(false);

    fn record_back_press() {
        BACK_PRESS_REACHED_THE_HANDLER.store(true, Ordering::Relaxed);
    }

    fn always_dark() -> bool {
        true
    }

    /// The app opens showing what the last session left, and it can only get
    /// that from the bridge — this crate has no idea where config lives.
    #[test]
    fn the_app_opens_with_the_config_its_platform_kept() {
        let bridge = TestBridge::desktop();
        seed_config(&bridge.store(), STORED_FPS);

        let app = headless(bridge);

        assert_eq!(
            app.gui.loop_speed_fps, STORED_FPS,
            "the stored config never reached the UI, so every session starts \
             on defaults",
        );
    }

    /// iOS cannot quit, and the menu must not offer to. The flag is pushed in
    /// from here because `rustdar-egui` cannot see a bridge; what it then does
    /// with it — dropping the Exit entry — is covered there.
    #[test]
    fn the_ui_is_told_whether_this_platform_can_quit() {
        assert!(
            !headless(TestBridge::ios()).gui.supports_exit(),
            "iOS would draw an Exit button that does nothing",
        );
        assert!(
            headless(TestBridge::desktop()).gui.supports_exit(),
            "the desktop menu lost its Exit entry",
        );
    }

    /// Android learns its data directory only after startup, so the load in
    /// `App::new` had nothing to read and the second one is the only one that
    /// ever runs there.
    ///
    /// Also the strongest available statement that the directory *reached the
    /// bridge*: the double, like Android's, has no store to hand out until it
    /// has been told where one lives, so a dropped forward leaves the UI on
    /// defaults just as a dropped load does.
    #[test]
    fn learning_where_config_lives_loads_it() {
        let bridge = TestBridge::android();
        seed_config(&bridge.store(), STORED_FPS);

        let mut app = headless(bridge);
        assert_eq!(
            app.gui.loop_speed_fps, 5.0,
            "precondition: nowhere to load from yet",
        );

        app.set_config_dir(std::path::PathBuf::from("/data/user/0/rustdar"));

        assert_eq!(
            app.gui.loop_speed_fps, STORED_FPS,
            "the config directory arrived and nothing was read from it",
        );
    }

    /// The save has to happen before the platform gets to refuse the exit.
    ///
    /// On iOS the refusal is unconditional, so a `supports_exit` check hoisted
    /// above the save would mean that platform never persists anything on quit
    /// at all — and it would look completely fine on every other platform.
    #[test]
    fn a_platform_that_cannot_quit_still_saves_on_the_way_out() {
        let bridge = TestBridge::ios();
        let store = bridge.store();
        let mut app = headless(bridge);
        app.gui.loop_speed_fps = STORED_FPS;

        app.request_exit(None);

        assert_eq!(
            stored_fps(&store),
            STORED_FPS,
            "nothing was persisted; on iOS this is the only exit path there is",
        );
        assert!(
            !app.exit_requested,
            "iOS has no quit, so nothing may be scheduled on the next event",
        );
    }

    /// An exit asked for during a redraw has no event loop to hand, so it is
    /// deferred rather than dropped.
    #[test]
    fn an_exit_with_no_event_loop_is_deferred_to_the_next_event() {
        let mut app = headless(TestBridge::desktop());
        assert!(!app.exit_requested, "precondition");

        app.request_exit(None);

        assert!(
            app.exit_requested,
            "the request was swallowed and the app never quits",
        );
    }

    /// The menu's Exit is one of the four ways out and goes through the same
    /// gate as the rest: it saves, and it respects a platform that cannot quit.
    ///
    /// The other three — `CloseRequested`, Escape and the Android back button —
    /// all reach `request_exit` holding an `ActiveEventLoop`, which winit will
    /// not hand out except from inside a running loop. Their routes are pinned
    /// by the source probes above and below; only this one can be driven.
    #[test]
    fn the_menus_exit_goes_through_the_same_gate() {
        let mut app = headless(TestBridge::desktop());
        app.handle_gui_action(GuiAction::Exit, None);
        assert!(
            app.exit_requested,
            "Exit from the menu no longer reaches request_exit",
        );

        let bridge = TestBridge::ios();
        let store = bridge.store();
        let mut app = headless(bridge);
        app.gui.loop_speed_fps = STORED_FPS;

        app.handle_gui_action(GuiAction::Exit, None);

        assert!(!app.exit_requested, "iOS took the exit path anyway");
        assert_eq!(
            stored_fps(&store),
            STORED_FPS,
            "the menu's Exit skipped the config save",
        );
    }

    /// A fix and a heading are separate readings from separate sensors and must
    /// stay that way: the map draws the dot from one and rotates it by the
    /// other.
    ///
    /// Both arrive over channels the app installs on the bridge, which is how
    /// Android and the browser deliver them. Nothing here could be reached at
    /// all until those two setters stopped being `#[cfg(target_os = "android")]`.
    ///
    /// Driven through `handle_redraw` rather than `poll_platform_state`
    /// directly. Nothing else polls the bridge, so calling the poller by hand
    /// would leave the one line that schedules it — in the frame loop — free to
    /// be deleted. With no window, `handle_redraw` polls and then returns
    /// before it needs a renderer.
    #[test]
    fn the_platforms_sensors_reach_the_map() {
        let mut app = headless(TestBridge::android());
        let (fix_tx, fix_rx) = std::sync::mpsc::channel();
        let (heading_tx, heading_rx) = std::sync::mpsc::channel();
        app.set_gps_fix_receiver(fix_rx);
        app.set_heading_receiver(heading_rx);

        fix_tx
            .send(rustdar_gps::GpsFix::from_lat_lon(35.3331, -97.2778))
            .unwrap();
        heading_tx.send(214.5).unwrap();

        app.handle_redraw();

        let fix = app.gui.gps_fix().expect("no position reached the UI");
        assert_eq!((fix.latitude, fix.longitude), (35.3331, -97.2778));
        assert_eq!(
            app.gui.user_heading(),
            Some(214.5),
            "no compass reading reached the UI — note the fix carries no \
             heading of its own, so this cannot have come from it",
        );
    }

    /// A theme change has to invalidate the site labels, and only a *change*
    /// may.
    ///
    /// The labels are raster textures baked in the theme's colours, so they are
    /// stale the moment it flips. But Android's theme poller re-sends its
    /// reading every two seconds whether or not it moved — see
    /// `spawn_state_poller` — so an unguarded bump would re-rasterise every
    /// label on every pane twice a second, forever.
    #[test]
    fn a_theme_change_invalidates_the_site_labels_exactly_once() {
        let mut bridge = TestBridge::android();
        let theme = bridge.theme_channel();
        let mut app = headless(bridge);
        let before = app.gui.pane(0).unwrap().radar_sites_render_gen;

        theme.send(true).unwrap();
        app.handle_redraw();

        assert_eq!(app.cached_dark_theme, Some(true), "the change was not taken");
        let after = app.gui.pane(0).unwrap().radar_sites_render_gen;
        assert_eq!(
            after,
            before.wrapping_add(1),
            "the site labels still carry the old theme's colours",
        );

        theme.send(true).unwrap();
        app.handle_redraw();

        assert_eq!(
            app.gui.pane(0).unwrap().radar_sites_render_gen,
            after,
            "a repeated reading re-rasterised every label; the poller sends \
             one of these every two seconds",
        );
    }

    /// Where the injected querier says the system bars are. A `fn` pointer
    /// closes over nothing, which is the constraint Android's real querier is
    /// under too — it reaches the framework through a process-wide `JavaVM`.
    static ROTATED: AtomicBool = AtomicBool::new(false);

    fn cutout() -> (f32, f32, f32, f32) {
        if ROTATED.load(Ordering::Relaxed) {
            (0.0, 0.0, 96.0, 0.0)
        } else {
            (96.0, 0.0, 0.0, 0.0)
        }
    }

    /// Turning the device sideways moves the cutout to another edge, and the
    /// app has to ask again.
    ///
    /// It arrives as a resize, not as a resume, so insets queried once at
    /// startup describe the orientation the app happened to open in for the
    /// rest of the session — reserving a strip along the top while the notch is
    /// down the left. The resize is also the signal that a layout has happened,
    /// which is what `getRootWindowInsets` needs before it has anything current
    /// to return.
    #[test]
    fn a_rotation_re_queries_the_insets_rather_than_keeping_the_old_edge() {
        ROTATED.store(false, Ordering::Relaxed);
        let mut app = headless(TestBridge::android());
        app.set_insets_querier(cutout);

        // What `resumed` does once the window exists.
        app.refresh_safe_area_insets();
        assert_eq!(
            app.gui.safe_area_insets(),
            (96.0, 0.0, 0.0, 0.0),
            "precondition: portrait puts the cutout along the top",
        );

        ROTATED.store(true, Ordering::Relaxed);
        app.handle_resized(2400, 1080);

        assert_eq!(
            app.gui.safe_area_insets(),
            (0.0, 0.0, 96.0, 0.0),
            "the device rotated and the app is still holding a strip clear at \
             the top while the cutout eats the left edge",
        );
    }

    /// Both query sites have to stay wired. The behavioural test above drives
    /// `handle_resized`; `resumed` takes an `ActiveEventLoop` and cannot be
    /// called, so its half is read off the source, as `back_out`'s is.
    #[test]
    fn both_inset_queries_are_still_wired() {
        for f in ["fn resumed(", "fn handle_resized("] {
            assert!(
                fn_body(f).contains("refresh_safe_area_insets("),
                "{f} no longer asks the platform for insets",
            );
        }
    }

    /// The window's own close button is the fourth exit trigger and the last
    /// one with no other handle on it: `window_event` takes an
    /// `ActiveEventLoop`, so the arm can only be read.
    ///
    /// What it must reach is `request_exit` and not `event_loop.exit()` — the
    /// config save and the `supports_exit` refusal both live inside it, and a
    /// direct exit here would skip both while looking perfectly correct.
    #[test]
    fn closing_the_window_goes_through_request_exit() {
        let body = fn_body("fn window_event(");
        let arm = body
            .find("WindowEvent::CloseRequested")
            .expect("window_event no longer handles CloseRequested");
        let arm_end = body[arm..]
            .find("WindowEvent::RedrawRequested")
            .map(|i| arm + i)
            .unwrap_or(body.len());
        assert!(
            body[arm..arm_end].contains("self.request_exit("),
            "the close button bypasses request_exit, so it saves no config and \
             ignores a platform that cannot quit: {}",
            &body[arm..arm_end],
        );
    }

    /// Two things the app hands the bridge that it can only get back by asking.
    ///
    /// The theme read is Android's only source — NativeActivity never emits
    /// `ThemeChanged` — and the back handler is what makes back minimise there
    /// instead of quitting. Both are `fn` pointers because the JNI they end in
    /// lives in a crate the bridge cannot depend on.
    #[test]
    fn the_injected_callbacks_reach_the_bridge() {
        let mut app = headless(TestBridge::android());
        assert!(
            !app.platform.detect_dark_theme(),
            "precondition: nothing injected yet",
        );

        app.set_theme_detector(always_dark);
        assert!(
            app.platform.detect_dark_theme(),
            "the theme read never arrived, and Android has no other one",
        );

        BACK_PRESS_REACHED_THE_HANDLER.store(false, Ordering::Relaxed);
        assert_eq!(
            App::resolve_back_press(&mut app.gui, app.platform.as_ref()),
            BackPress::Exit,
            "precondition: with no handler installed, back quits",
        );

        app.set_back_handler(record_back_press);
        assert_eq!(
            App::resolve_back_press(&mut app.gui, app.platform.as_ref()),
            BackPress::PlatformHandled,
        );
        assert!(
            BACK_PRESS_REACHED_THE_HANDLER.load(Ordering::Relaxed),
            "the handler was installed but never run, so back reports the app \
             minimised and nothing minimises",
        );
    }

    /// The reader is started on the port the *action* names.
    ///
    /// The settings pane edits a config and emits it with the action; the
    /// bridge is the only thing that ever sees it, and opening the wrong serial
    /// port is indistinguishable from a missing one at this level. So the
    /// double keeps what it was handed — the one place in this suite where a
    /// recorded argument is the only observable there is.
    #[test]
    fn starting_gps_hands_the_bridge_the_config_the_action_carried() {
        let bridge = TestBridge::desktop();
        let started = bridge.gps_record();
        let mut app = headless(bridge);

        app.handle_gui_action(
            GuiAction::StartGps {
                config: rustdar_gps::GpsConfig {
                    port_path: Some("/dev/ttyPROBE".to_string()),
                    baud_rate: 38400,
                    ..Default::default()
                },
            },
            None,
        );

        assert!(app.platform.gps_active(), "the reader was never started");
        {
            let record = started.borrow();
            let config = record.as_ref().expect("start_gps was not reached");
            assert_eq!(
                config.port_path.as_deref(),
                Some("/dev/ttyPROBE"),
                "the reader opened a different port than the action asked for",
            );
            assert_eq!(config.baud_rate, 38400);
        }

        app.handle_gui_action(GuiAction::StopGps, None);
        assert!(
            !app.platform.gps_active(),
            "the reader kept the serial port open after being told to stop",
        );
    }
}
