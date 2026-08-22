use egui_wgpu::wgpu;
use rustdar_egui::radar_layer;
use std::collections::HashMap;
use std::sync::Arc;
use winit::application::ApplicationHandler;
#[cfg(not(target_arch = "wasm32"))]
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::{Window, WindowId};

use crate::WindowRef;
use crate::app_state;
use crate::channels::ChannelHub;
use crate::input::InputHandler;
use crate::platform::{PlatformBridge, RedrawWaker};
use crate::render_dispatch::RenderDispatcher;
#[cfg(not(target_arch = "wasm32"))]
use rustdar_device_profile::constants::{RENDER_HEIGHT, RENDER_WIDTH};
use rustdar_egui::shell_api::GuiEvent;
use rustdar_egui::{Gui, actions::GuiAction};
use rustdar_location::LocationFacade;
use rustdar_radar::loop_downloads::LoopDownloadManager;
use rustdar_radar::site_position::SitePositionSource;
use rustdar_radar::types::ScanInfo;
use rustdar_source::id::LayerId;

#[path = "app_fetch.rs"]
// `pub(crate)` for one type: `render_dispatch` holds the last
// `OverlayRenderRequest` per (pane, layer) — see `last_overlay_dispatch`.
pub(crate) mod fetch;

#[path = "app_render.rs"]
mod render;

#[path = "app_chunks.rs"]
mod chunks;

#[path = "frame_pump.rs"]
mod frame_pump;

/// Whether this build is the browser build.
const WEB: bool = cfg!(target_arch = "wasm32");

/// Which wgpu backends this build will consider.
fn instance_descriptor() -> wgpu::InstanceDescriptor {
    backends_for(
        WEB,
        wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
    )
}

/// The backend choice itself, parameterised so both arms run from one binary.
fn backends_for(web: bool, base: wgpu::InstanceDescriptor) -> wgpu::InstanceDescriptor {
    if web {
        wgpu::InstanceDescriptor {
            backends: wgpu::Backends::GL,
            ..base
        }
    } else {
        base
    }
}

/// Request a redraw if a window handle is available.
pub(crate) fn notify_redraw(window: &Option<WindowRef>) {
    if let Some(w) = window {
        // Background threads may outlive the event loop on exit.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            w.request_redraw();
        }));
    }
}

/// What one press of Escape or the back button resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackPress {
    /// A layer closed.
    Dismissed,
    /// Nothing was open and the platform took the press — Android minimises.
    PlatformHandled,
    /// Nothing was open, nothing took it, and this platform says an unhandled
    /// back leaves — see [`PlatformBridge::exits_on_unhandled_back`].
    Exit,
    /// Nothing was open, nothing took it, and this platform does not quit on a
    /// back press: the press does nothing at all. Desktop and web.
    Ignored,
}

pub struct App {
    instance: wgpu::Instance,
    state: Option<app_state::AppState>,
    window: Option<WindowRef>,
    gui: Gui,
    /// Whether this platform can quit, answered once at startup —
    /// `PlatformBridge::supports_exit` is a property of the build.
    supports_exit: bool,
    /// This build's loop frame cap (`budgets.loop_frames_held`), so the timeline's caption
    /// states the platform's real budget — the budget lives in this crate and the UI crate
    /// cannot see it.
    loop_frame_budget: usize,
    /// Whether this platform has a location settings page to offer, answered once at
    /// startup: the permission changes, the platform does not.
    location_settings_available: bool,
    /// Safe-area insets in logical pixels, from the last successful
    /// [`PlatformBridge::query_insets`] — see `refresh_safe_area_insets` for when that is
    /// allowed to be asked.
    safe_area_insets: (f32, f32, f32, f32),
    /// The user's GPS fix and when this app heard it.
    user_gps: Option<(rustdar_location::Fix, web_time::Instant)>,
    /// Compass heading in degrees, once a platform has delivered one.
    user_heading: Option<f32>,
    /// **What the radar layer says it is doing** — the chunk feed's status and
    /// each site's current-volume stamp, in the layer's own type.
    ///
    /// Both halves are recomputed every frame (`drive_chunk_feeds`,
    /// `publish_base_volumes`) so the status bar never shows a stale claim,
    /// but the seam's entry is rebuilt only when this **changes**: see
    /// [`Self::republish_liveness`].
    radar_liveness: rustdar_egui::radar_layer::RadarLiveness,
    /// The seam's own value, one entry per layer that publishes one. Rebuilt
    /// on change, re-stated every frame.
    liveness: Vec<rustdar_source::liveness::SourceLiveness>,
    /// The same painter [`Gui`] was handed, kept so the frame path can take its floor-
    /// magnification demand.
    volume_painter: Option<Arc<rustdar_volumetric::bridge::BridgeVolumePainter>>,
    /// The rung the pane mirror is drawn at, and the hysteresis that governs when it may
    /// move.
    mirror_rungs: rustdar_gpu::egui_renderer::MirrorRungs,
    /// Every per-target number this build spends, resolved once from a
    /// [`rustdar_device_profile::budget::DeviceProfile`] and threaded from here.
    budgets: rustdar_device_profile::budget::Budgets,
    /// Everything known about the machine, and the only input [`Self::budgets`] has.
    device_profile: rustdar_device_profile::budget::DeviceProfile,
    /// The application's whole loop allowance, and the hysteresis that governs how it is
    /// divided.
    loop_pool: crate::loop_pool::LoopPool,
    /// See [`Self::loop_pool`].
    loop_pool_state: crate::loop_pool::LoopPoolState,
    /// Whether [`Self::loop_pool`] is already the answer for this machine.
    loop_pool_sized: bool,
    /// The site-keyed decoded volumes: what each pane's static render draws, and
    /// each site's merge base. One owner, asked by name rather than indexed —
    /// see [`crate::volume_inventory`].
    pub(crate) volumes: crate::volume_inventory::VolumeInventory,
    input: InputHandler,
    channels: ChannelHub,
    render: RenderDispatcher,
    platform: Box<dyn PlatformBridge>,
    texture_counter: u32,
    /// A rendering state has been built and the rasters it dropped have not
    /// been put back yet. Set where the state is created, spent inside the
    /// frame — see `App::setup_egui_frame`, and
    /// [`restore_cached_render`](Self::restore_cached_render) for why the
    /// restore cannot run at the moment the state is built.
    restore_pending: bool,
    cached_dark_theme: Option<bool>,
    /// The last predictive-back claim pushed to the platform, so the push is
    /// edge-triggered. `false` at construction because nothing is open on the
    /// first frame — which is also what the platform assumes until told
    /// otherwise, so the two start in agreement.
    back_claimed: bool,
    exit_requested: bool,
    /// Native only.
    #[cfg(not(target_arch = "wasm32"))]
    tokio_runtime: tokio::runtime::Runtime,
    /// Web only.
    #[cfg(target_arch = "wasm32")]
    pending_state: Option<std::sync::mpsc::Receiver<app_state::AppState>>,
    http_client: reqwest::Client,
    loop_mgr: LoopDownloadManager,
    /// **Frame listings that landed this frame**, one entry per arrival.
    ///
    /// The arrival is drained in `Ingest` and the loops waiting on it are
    /// built in `Apply` — the phase the listing channel's own drain ran in —
    /// so re-pointing the supply did not move when a listing becomes a plan.
    loop_listings_arrived: Vec<render::LoopListingArrival>,
    /// **What each pane's clock has been asking for that its loop cannot
    /// answer**, so that a drag through many instants is one question and a
    /// pane parked on a hole asks once rather than every frame. See
    /// [`crate::loop_refill`].
    loop_refill: crate::loop_refill::LoopRefillWatch,
    /// Per-site real-time chunk feeds.
    chunk_feeds: rustdar_radar::chunk_feed::ChunkFeedManager,
    /// Push notification of new chunks.
    chunk_notify: rustdar_radar::chunk_notify::ChunkNotifier,
    /// `(volume, what its cuts declared, its product inventory, when it was collected)`.
    latest_cached_scans: HashMap<
        String,
        (
            Arc<nexrad_model::data::Scan>,
            Arc<rustdar_radar::nyquist::DeclaredNyquist>,
            ScanInfo,
            chrono::NaiveDateTime,
        ),
    >,
    manual_nav_pending: bool,
    /// The map extent most recently asked for on screen.
    last_viewport: Option<rustdar_geo::GeoBounds>,
    autosave: AutosaveState,
    /// When egui next wants a frame, from a timed repaint request (`request_repaint_after`
    /// — a cursor blink, a tooltip delay).
    egui_repaint_at: Option<web_time::Instant>,
    /// When an auto-poll timer next needs a frame, or `None` while none of them do.
    auto_poll_at: Option<web_time::Instant>,
    /// Whether the current site was guessed from the timezone rather than chosen.
    site_is_provisional: bool,
    /// Whether the live table has never had a network catalogue in it, and is still waiting
    /// for the fetch that would put one there.
    catalogue_pending: bool,
    /// Whether this launch had no site of its own to open on, and is still waiting for a
    /// catalogue to run the timezone hint against.
    site_hint_pending: bool,
    /// The voxel grids 3D panes are holding, refcounted by the volume they were built from.
    volume_store: std::sync::Arc<rustdar_volumetric::bridge::VolumeStore>,
    #[cfg(test)]
    pub(crate) volume_extractions: std::cell::Cell<u32>,
    /// How a thread that is not this one asks for a frame.
    redraw_waker: RedrawWaker,
    location: LocationFacade,
    /// Where earlier volumes said their radars are.
    site_positions: crate::site_positions::SitePositions,
    /// The network catalogue this install last cached: which radars exist, and where the
    /// published record puts them.
    site_catalogue: rustdar_radar::catalogue::SiteCatalogue,
}

/// Bookkeeping for the periodic config write.
struct AutosaveState {
    /// When the config was last examined for changes.
    last_check: Option<web_time::Instant>,
    /// The JSON most recently written, so an unchanged config costs a serialization and a
    /// string compare rather than a storage write.
    last_written: Option<String>,
    /// Whether any event has arrived that could have changed the config.
    touched: bool,
}

/// How often the config is examined for changes.
const AUTOSAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// The shortest sleep [`App::auto_poll_delay`] will ask for.
const MIN_WAKE: std::time::Duration = std::time::Duration::from_secs(1);

/// What a frame's egui repaint request means for the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepaintAction {
    /// Ask for the next frame immediately — an animation is mid-flight.
    Now,
    /// Wake and repaint after this long — a timed request (cursor blink).
    After(std::time::Duration),
    /// Nothing asked; the loop may park until something happens.
    Idle,
}

/// The ceiling past which a "timed" repaint request is read as "never": egui reports
/// `Duration::MAX` when nothing asked, and anything on the scale of a minute or more is
/// indistinguishable from idle for a loop every real input wakes anyway.
const MAX_SCHEDULED_REPAINT: std::time::Duration = std::time::Duration::from_secs(60);

/// Classify a frame's `repaint_delay` (see `PreparedFrame::repaint_delay`).
pub(crate) fn repaint_action(delay: std::time::Duration) -> RepaintAction {
    if delay == std::time::Duration::ZERO {
        RepaintAction::Now
    } else if delay <= MAX_SCHEDULED_REPAINT {
        RepaintAction::After(delay)
    } else {
        RepaintAction::Idle
    }
}

/// What one pass of [`App::prepare_volume`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VolumePrepare {
    /// The store holds an entry for this target — built, building or refused — with the
    /// caller attached to it.
    Served,
    /// The data this target names has not arrived.
    Waiting,
    /// The concurrent render budget is full.
    Busy,
}

/// **The handover a volume ask becomes**: what ground to resample, what budget
/// to spend on it, and the payload the frontend has already extracted.
///
/// This end resolves the two things only it can — the box's centre and reach,
/// which come off a picked region or fall back to the site — and hands over
/// the rest as numbers. **What is deliberately NOT decided here** is the grid
/// shape, the vertical extent and which moment: those are the answering
/// layer's, and since WO-M14b-2 they are stated on its side of the seam.
fn volume_job_context(
    target: &rustdar_egui::pane::VolumeTarget,
    site_lat: f64,
    site_lon: f64,
    cells: [u32; 3],
    max_axis: u32,
    payload: Box<dyn std::any::Any + Send>,
) -> rustdar_source::volume::VolumeJobContext {
    let (centre, half_extent_km) = match target.region {
        Some(region) => {
            let extent = region.half_extent_km();
            (region.centre(), Some((extent.east_km, extent.north_km)))
        }
        None => (
            rustdar_geo::GeoPoint {
                lat: site_lat,
                lon: site_lon,
            },
            None,
        ),
    };
    rustdar_source::volume::VolumeJobContext {
        payload,
        field: target.product.clone(),
        centre,
        half_extent_km,
        cells,
        max_axis,
    }
}

/// Point a fresh `Gui` at the radar nearest this device's timezone.
fn apply_location_hint(gui: &mut Gui, platform: &dyn PlatformBridge) -> bool {
    let Some(zone) = platform.iana_timezone() else {
        log::debug!("no timezone available; keeping the default site");
        return false;
    };
    let Some(site) = crate::location_hint::site_for_timezone(&zone) else {
        log::debug!("timezone {zone} maps to no radar; keeping the default site");
        return false;
    };
    log::info!("first run: opening on {site}, nearest to timezone {zone}");
    gui.set_initial_site(site);
    true
}

/// Take out of `map` every value whose key `doomed` names, and hand them back **owned**.
pub(crate) fn evicted<V>(
    map: &mut HashMap<String, V>,
    doomed: &impl Fn(&String) -> bool,
) -> Vec<V> {
    map.extract_if(|site, _| doomed(site))
        .map(|(_, value)| value)
        .collect()
}

impl App {
    /// Build the application around a caller-supplied platform bridge.
    pub fn new(platform: Box<dyn PlatformBridge>, location: LocationFacade) -> Self {
        Self::with_instance(
            egui_wgpu::wgpu::Instance::new(instance_descriptor()),
            platform,
            location,
        )
    }

    /// Everything [`new`](Self::new) does once the wgpu instance exists.
    fn with_instance(
        instance: wgpu::Instance,
        platform: Box<dyn PlatformBridge>,
        location: LocationFacade,
    ) -> Self {
        let input = InputHandler::new();
        let channels = ChannelHub::new();
        let mut device_profile = rustdar_device_profile::budget::DeviceProfile::for_target();
        device_profile.memo = Some(rustdar_device_profile::budget::BudgetMemo {
            loop_pool_bytes: None,
            steps_back: crate::budget_memo::remembered_steps(platform.kv().as_deref()).unwrap_or(0),
        });
        let budgets = rustdar_device_profile::budget::resolve(&device_profile);
        let render = RenderDispatcher::with_budgets(&budgets);

        #[cfg(not(target_arch = "wasm32"))]
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");

        // Goes through `rustdar_radar::tls` rather than `reqwest::Client::builder`
        // directly: that is what installs the rustls crypto provider (no provider is
        // compiled in) and sets `https_only`.
        let http_client = rustdar_radar::tls::client(
            rustdar_radar::tls::USER_AGENT,
            std::time::Duration::from_secs(30),
        )
        .build()
        .expect("Failed to build HTTP client");

        let mut gui = Gui::new();
        let supports_exit = platform.supports_exit();
        let loop_frame_budget = budgets.loop_frames_held;
        let location_settings_available = location.settings_available();
        let restored = platform
            .kv()
            .is_some_and(|store| gui.load_ui_config(store.as_ref()));
        let site_is_provisional = !restored && apply_location_hint(&mut gui, platform.as_ref());
        let site_positions = crate::site_positions::SitePositions::load(platform.kv().as_deref());
        let site_catalogue = crate::site_catalogue::load(platform.kv().as_deref());
        let table =
            rustdar_radar::sites::resolve(site_positions.fixes().chain(site_catalogue.fixes()));
        let catalogue_pending = site_catalogue.is_empty();
        let site_hint_pending = !restored && table.rows().is_empty();
        if catalogue_pending {
            log::info!(
                "no radars are known yet; the site list holds only what this \
                 install has decoded until the catalogue fetch lands",
            );
        }

        let loop_pool_limits = crate::loop_pool::LoopPoolLimits::from_budgets(&budgets);
        let loop_pool_memo =
            crate::loop_pool::remembered(platform.kv().as_deref(), loop_pool_limits);
        let loop_pool = crate::loop_pool::LoopPool::new(
            loop_pool_memo.unwrap_or(loop_pool_limits.floor),
            loop_pool_limits,
        );
        if let Some(memo) = device_profile.memo.as_mut() {
            memo.loop_pool_bytes = loop_pool_memo;
        }

        let mut app = Self {
            instance,
            state: None,
            window: None,
            gui,
            supports_exit,
            loop_frame_budget,
            location_settings_available,
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            user_gps: None,
            user_heading: None,
            radar_liveness: rustdar_egui::radar_layer::RadarLiveness::default(),
            liveness: Vec::new(),
            volume_painter: None,
            mirror_rungs: rustdar_gpu::egui_renderer::MirrorRungs::default(),
            budgets,
            device_profile,
            loop_pool,
            loop_pool_state: crate::loop_pool::LoopPoolState::new(
                loop_pool,
                crate::loop_pool::LoopFrameModel::from_budgets(&budgets),
            ),
            loop_pool_sized: loop_pool_memo.is_some(),
            volumes: crate::volume_inventory::VolumeInventory::default(),
            input,
            channels,
            render,
            platform,
            texture_counter: 0,
            restore_pending: false,
            cached_dark_theme: None,
            back_claimed: false,
            exit_requested: false,
            autosave: AutosaveState {
                last_check: None,
                last_written: None,
                touched: false,
            },
            egui_repaint_at: None,
            auto_poll_at: None,
            site_is_provisional,
            catalogue_pending,
            site_hint_pending,
            volume_store: std::sync::Arc::new(rustdar_volumetric::bridge::VolumeStore::new()),
            #[cfg(test)]
            volume_extractions: std::cell::Cell::new(0),
            http_client,
            #[cfg(not(target_arch = "wasm32"))]
            tokio_runtime,
            #[cfg(target_arch = "wasm32")]
            pending_state: None,
            loop_mgr: LoopDownloadManager::new(),
            loop_listings_arrived: Vec::new(),
            loop_refill: Default::default(),
            chunk_feeds: rustdar_radar::chunk_feed::ChunkFeedManager::new(),
            chunk_notify: rustdar_radar::chunk_notify::ChunkNotifier::new(),
            latest_cached_scans: HashMap::new(),
            manual_nav_pending: false,
            last_viewport: None,
            redraw_waker: RedrawWaker::new(),
            // The gate inside is inert until the first `poll_platform_state`, which is
            // inside the first frame — deliberately after `set_config_dir`, so it finds the
            // memo Android only learns the path to during `android_main`.
            location,
            site_positions,
            site_catalogue,
        };

        app.spawn_site_catalogue_refresh();

        app.platform.set_redraw_waker(app.redraw_waker.clone());
        let location_wake = app.redraw_waker.clone();
        app.location
            .set_wake(std::sync::Arc::new(move || location_wake.wake()));
        app.push_frame_inputs();
        app
    }

    /// A handle an entry point can give its own sensor threads.
    pub fn redraw_waker(&self) -> RedrawWaker {
        self.redraw_waker.clone()
    }

    /// Create surface and initialize AppState for a given window and dimensions.
    async fn initialize_rendering_state(
        instance: &wgpu::Instance,
        budgets: rustdar_device_profile::budget::Budgets,
        window: &WindowRef,
        width: u32,
        height: u32,
    ) -> app_state::AppState {
        let surface = instance
            .create_surface(window.clone())
            .expect("Failed to create surface!");

        app_state::AppState::new(instance, &budgets, surface, window, width, height).await
    }

    fn handle_resized(&mut self, width: u32, height: u32) {
        // A rotation moves the cutout and the navigation bar to other edges, and it reaches
        // the app as a resize — not as a resume.
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
    fn refresh_safe_area_insets(&mut self) {
        if let Some((top, bottom, left, right)) = self.platform.query_insets() {
            self.safe_area_insets = (top, bottom, left, right);
        }
    }

    fn handle_redraw(&mut self) {
        self.input.clear_frame_state();
        self.poll_platform_state();
        self.poll_data_channels();
        self.evict_unshown_scans();
        rustdar_worker::offload::drain_deferred_drops(
            rustdar_device_profile::constants::DEFERRED_DROP_BUDGET_PER_FRAME,
        );
        // Ahead of the minimized and zero-area early returns below: a window that is
        // minimized or still sizing is exactly one whose session might be about to end, and
        // skipping the save there is how the last change gets lost.
        self.autosave_config(false);

        if let Some(window) = self.window.as_ref()
            && let Some(min) = window.is_minimized()
            && min
        {
            log::debug!("Window is minimized");
            return;
        }

        if let Some(window) = self.window.as_ref() {
            let size = window.inner_size();
            if size.width == 0 || size.height == 0 {
                log::debug!(
                    "Window has zero area ({}x{}); skipping frame",
                    size.width,
                    size.height
                );
                return;
            }
        }

        self.ensure_rendering_state();
        if self.state.is_none() || self.window.is_none() {
            return;
        }

        let (screen_descriptor, gui_actions) = self.setup_egui_frame();
        let repaint_delay = self.present_frame(screen_descriptor);
        self.process_gui_actions(gui_actions);
        self.push_back_claim();

        if self.render.any_render_in_flight()
            || self.gui.any_loop_active()
            || self.gui.any_raster_held()
            // A restore that deferred itself has to be brought back by
            // something; nothing else in this list speaks for it.
            || self.restore_pending
            || self.chunk_feeds.any_in_flight()
            || self.chunk_notify.handshake_pending()
            || rustdar_worker::offload::has_deferred_drops()
        {
            notify_redraw(&self.window);
        }

        self.auto_poll_at = self
            .auto_poll_delay()
            .map(|delay| web_time::Instant::now() + delay);

        match repaint_action(repaint_delay) {
            RepaintAction::Now => {
                self.egui_repaint_at = None;
                notify_redraw(&self.window);
            }
            RepaintAction::After(delay) => {
                self.egui_repaint_at = Some(web_time::Instant::now() + delay);
            }
            RepaintAction::Idle => {
                self.egui_repaint_at = None;
            }
        }
    }

    /// Take a theme reading, and say whether it changed anything.
    fn adopt_theme(&mut self, dark: bool) -> bool {
        if self.cached_dark_theme == Some(dark) {
            return false;
        }
        self.cached_dark_theme = Some(dark);
        self.gui.bump_all_radar_sites_gen();
        true
    }

    /// What this frame draws in, adopted into the cache on the way past.
    fn resolve_theme(&mut self) -> bool {
        let dark = match self.window.as_ref().and_then(|w| w.theme()) {
            Some(theme) => matches!(theme, winit::window::Theme::Dark),
            None => match self.cached_dark_theme {
                Some(cached) => cached,
                None => self.platform.detect_dark_theme(),
            },
        };
        self.adopt_theme(dark);
        dark
    }

    /// Poll for platform-specific theme, location, GPS fix, and compass heading changes.
    fn poll_platform_state(&mut self) {
        if let Some(new_theme) = self.platform.poll_theme()
            && self.adopt_theme(new_theme)
        {
            notify_redraw(&self.window);
        }
        // Ahead of the fix poll, and that ordering is the point: this is what starts
        // delivery in the first place, so on the frame after a grant lands the fix it
        // produces is drained in the same pass rather than the next one.
        let platform = &self.platform;
        let step = self
            .location
            .step(&|| platform.kv(), self.gui.settings_visible());
        if step.changed {
            notify_redraw(&self.window);
        }
        if step.revoked && !self.location.serial_active() {
            self.user_gps = None;
        }
        if let Some(fix) = self.location.poll_fix() {
            self.upgrade_provisional_site(&fix);
            // Stamped once, at arrival — the instant travels with the fix.
            self.user_gps = Some((fix, web_time::Instant::now()));
        }
        if let Some(heading) = self.platform.poll_heading() {
            self.user_heading = Some(heading);
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
                    self.budgets,
                    window,
                    size.width.max(1),
                    size.height.max(1),
                ))
            });
            if let Some(state) = new_state {
                self.render
                    .set_raster_side_ceiling_px(state.raster_side_ceiling_px);
                self.state = Some(state);
                // Armed here, run from inside the frame. See the field.
                self.restore_pending = true;
                self.install_volume_bridge();
            }
        }
    }

    /// See the native variant above.
    #[cfg(target_arch = "wasm32")]
    fn ensure_rendering_state(&mut self) {
        if let Some(rx) = self.pending_state.as_ref() {
            match rx.try_recv() {
                Ok(state) => {
                    self.pending_state = None;
                    self.render
                        .set_raster_side_ceiling_px(state.raster_side_ceiling_px);
                    self.state = Some(state);
                    // Armed here, run from inside the frame. See the field.
                    self.restore_pending = true;
                    self.install_volume_bridge();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
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
        let budgets = self.budgets;
        let redraw_target = self.window.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let state = Self::initialize_rendering_state(
                &instance,
                budgets,
                &window,
                size.width.max(1),
                size.height.max(1),
            )
            .await;
            let _ = tx.send(state);
            // The frame that kicked this off returned without a renderer, and under
            // `ControlFlow::Wait` nothing schedules another frame on its own.
            notify_redraw(&redraw_target);
        });
    }

    /// Build the volume pipelines on the device that has just appeared and hand the `Gui`
    /// something that can draw a 3D pane.
    fn update_device_profile(&mut self, class: rustdar_device_profile::quality::DeviceClass) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let limits = state.device.limits();
        self.device_profile.class = class;
        self.device_profile.adapter = rustdar_device_profile::budget::AdapterCeilings {
            max_texture_dimension_2d: limits.max_texture_dimension_2d,
            max_texture_dimension_3d: limits.max_texture_dimension_3d,
        };
        let resolved = rustdar_device_profile::budget::resolve(&self.device_profile);
        if resolved != self.budgets {
            log::info!(
                "Budgets: {:?} on a {class:?} adapter reporting {} px 2D and {} px 3D textures: \
                 {:?} grid cells, {} MiB of offscreen, {} MiB of 3D texture",
                resolved.promotion,
                limits.max_texture_dimension_2d,
                limits.max_texture_dimension_3d,
                resolved.grid_cells,
                resolved.offscreen_bytes / (1024 * 1024),
                resolved.volume_texture_bytes / (1024 * 1024),
            );
        }
        self.budgets = resolved;
    }

    fn install_volume_bridge(&mut self) {
        use rustdar_device_profile::quality;

        // Read before the `&mut` borrow below, because the pool it also decides lives on
        // `App` rather than on `AppState` — see `Self::loop_pool`.
        let Some(class) = self.state.as_ref().map(|state| {
            rustdar_gpu::device::device_class_of(state.adapter.get_info().device_type)
        }) else {
            return;
        };

        self.update_device_profile(class);

        // The same signal, spent a second time on a different question, and the *only* one
        // there is for capacity: wgpu 29.0.4 reports no memory on any backend.
        if !self.loop_pool_sized {
            self.loop_pool_sized = true;
            self.loop_pool = crate::loop_pool::LoopPool::for_promotion(
                self.budgets.promotion,
                None,
                crate::loop_pool::LoopPoolLimits::from_budgets(&self.budgets),
            );
            log::info!(
                "Loop pool: {} MiB for a {class:?} adapter at {:?}",
                self.loop_pool.bytes() / (1024 * 1024),
                self.budgets.promotion,
            );
        }

        let Some(state) = self.state.as_mut() else {
            return;
        };

        let quality = quality::select(class, self.budgets.quality_ceiling);

        // Nothing is built on a device that cannot render a volume — the pipelines would
        // compile a shader against limits already known to be short, and
        // `create_render_pipeline` has no `Result` to notice it in.
        if rustdar_volumetric::support(&state.volume_support).is_supported() {
            log::info!(
                "3D volume view: {quality:?} on {:?}",
                state.adapter.get_info().device_type
            );
            let resources = rustdar_volumetric::bridge::VolumeResources::new(
                &state.device,
                state.egui_renderer.attachment_config(),
                &state.queue,
            );
            state
                .egui_renderer
                .callback_resources_mut()
                .insert(resources);
        }

        let painter = std::sync::Arc::new(rustdar_volumetric::bridge::BridgeVolumePainter::new(
            self.volume_store.clone(),
            quality,
            self.budgets.offscreen_bytes,
            state.volume_support.clone(),
        ));
        self.volume_painter = Some(painter.clone());
        self.gui.apply(GuiEvent::VolumePainter(Some(painter)));
    }

    /// Dispatch the voxel build a 3D pane asked for, unless the volume is already in hand
    /// or in flight.
    fn volume_grid_axis_limit(&self) -> u32 {
        self.state.as_ref().map_or(
            rustdar_device_profile::constants::WEBGL2_MAX_TEXTURE_DIMENSION_3D,
            |state| state.device.limits().max_texture_dimension_3d,
        )
    }

    /// **Resolve the layer the ask names, then ask that layer for a volume.**
    ///
    /// The pane walked its own stack and sent the layer it landed on; this end
    /// resolves that name to a handler and asks the handler for its 3D half.
    /// Nothing here matches on an id, which is the point: a second 3D source
    /// arrives as an `impl` on its own handler and this function does not
    /// change.
    ///
    /// **Re-asked rather than trusted.** The walk ran on the pane, the ask
    /// crossed the action channel, and the pane can have been re-stacked or
    /// the layer switched off in between. A layer with no 3D half — or one
    /// that does not build the field the target names — is refused *into the
    /// store*, in a sentence fit for the middle of a pane, and the refusal
    /// counts as served so the level-trigger quiesces instead of re-asking
    /// every frame.
    fn handle_prepare_volume(
        &mut self,
        pane_idx: usize,
        layer: &rustdar_source::id::LayerId,
        target: rustdar_egui::pane::VolumeTarget,
    ) {
        if let Some(why) = self.volume_layer_refusal(layer, &target) {
            self.volume_store.insert_held(
                pane_idx,
                target.clone(),
                rustdar_volumetric::bridge::VolumeEntry::Refused(why),
                rustdar_volumetric::bridge::Hold::Single,
            );
            self.mark_volume_rendered(pane_idx, &target);
            return;
        }
        if self.prepare_volume(
            pane_idx,
            &target,
            rustdar_volumetric::bridge::Hold::Single,
            layer,
        ) == VolumePrepare::Served
        {
            self.mark_volume_rendered(pane_idx, &target);
        }
    }

    /// The body of [`Self::handle_prepare_volume`], shared with the 3D loop's dispatcher.
    ///
    /// `layer` is who to ask for the job once the payload exists. It is a
    /// parameter rather than something re-derived here because the two callers
    /// know it for different reasons and neither can be made to guess: the
    /// action carries the layer the pane's own walk landed on, and the loop
    /// pass is radar's loop end to end.
    fn prepare_volume(
        &mut self,
        pane_idx: usize,
        target: &rustdar_egui::pane::VolumeTarget,
        hold: rustdar_volumetric::bridge::Hold,
        layer: &rustdar_source::id::LayerId,
    ) -> VolumePrepare {
        use rustdar_volumetric::bridge::VolumeEntry;

        if self.volume_store.share_held(pane_idx, target, hold) {
            return VolumePrepare::Served;
        }

        let live = self
            .current_volume_stamp(&target.volume.site)
            .is_some_and(|stamp| stamp.newest == target.volume.collected);
        let navigated = !live
            && self
                .volumes
                .base_is_from(&target.volume.site, target.volume.collected);
        if !live
            && !navigated
            && !self
                .loop_mgr
                .is_cached(&target.volume.site, &target.volume.collected)
        {
            return VolumePrepare::Waiting;
        }
        let Some(site) = rustdar_radar::sites::get_radar_site(&target.volume.site) else {
            self.volume_store.insert_held(
                pane_idx,
                target.clone(),
                VolumeEntry::Refused(format!(
                    "{} is not a radar site this build knows the position of.",
                    target.volume.site,
                )),
                hold,
            );
            return VolumePrepare::Served;
        };

        if !self.render.render_slot_free() {
            return VolumePrepare::Busy;
        }

        // The resample is radar's own and keyed by radar's field; the target
        // names it by id.
        let Some(product) = crate::render_key::radar_field(&target.product) else {
            self.volume_store.insert_held(
                pane_idx,
                target.clone(),
                VolumeEntry::Refused(format!(
                    "This build does not know the field {}.",
                    target.product.as_str(),
                )),
                hold,
            );
            return VolumePrepare::Served;
        };

        let started = web_time::Instant::now();
        let extracted = if live {
            self.extract_current_volume(&target.volume.site, product)
        } else if navigated {
            self.extract_base_volume(&target.volume.site, product)
        } else {
            self.extract_loop_volume(&target.volume.site, target.volume.collected, product)
        };
        let Some(input) = extracted else {
            self.volume_store.insert_held(
                pane_idx,
                target.clone(),
                VolumeEntry::Refused(format!(
                    "This volume carries no {} to resample for 3D.\n\n({} at {} UTC)",
                    rustdar_radar::fields::spec(product).name,
                    target.volume.site,
                    target.volume.collected,
                )),
                hold,
            );
            return VolumePrepare::Served;
        };
        log::info!(
            "3D volume view: extracted the {} {} payload in {} ms on the frame thread",
            target.volume.site,
            match (live, navigated) {
                (true, _) => "live",
                (_, true) => "navigated",
                _ => "loop-frame",
            },
            started.elapsed().as_millis(),
        );

        let ctx = volume_job_context(
            target,
            site.lat,
            site.lon,
            self.budgets.grid_cells,
            self.volume_grid_axis_limit(),
            Box::new(input),
        );
        let Some(job) = self.volume_job(layer, ctx) else {
            self.volume_store.insert_held(
                pane_idx,
                target.clone(),
                VolumeEntry::Refused(format!(
                    "{} could not shape a 3D build of {} from this volume.",
                    layer.as_str(),
                    target.product.as_str(),
                )),
                hold,
            );
            return VolumePrepare::Served;
        };
        let spawned = self.render.spawn_voxel_build(
            target,
            job,
            self.channels.voxel_sender.clone(),
            self.window.clone(),
        );
        if !spawned {
            return VolumePrepare::Busy;
        }
        self.volume_store.begin_build_held(pane_idx, target, hold);
        VolumePrepare::Served
    }

    /// The payload for one of a 3D loop's **past** volumes, out of the scans the loop has
    /// already downloaded.
    fn extract_loop_volume(
        &mut self,
        site: &str,
        collected: chrono::NaiveDateTime,
        product: rustdar_radar::types::RadarProduct,
    ) -> Option<rustdar_radar::render_input::RenderInput> {
        #[cfg(test)]
        self.volume_extractions
            .set(self.volume_extractions.get() + 1);
        let radar = rustdar_radar::sites::get_radar_site(site)?;
        let (scan, declared) = self.loop_mgr.get_cached(site, &collected)?;
        let (scan, declared) = (Arc::clone(scan), Arc::clone(declared));
        let sweeps: Vec<&nexrad_model::data::Sweep> = scan.sweeps().iter().collect();
        rustdar_radar::render_input::RenderInput::extract_volume_parts(
            scan.coverage_pattern(),
            &sweeps,
            product,
            radar.lat,
            radar.lon,
            self.render.storm_motion_override_kt(),
        )
        .map(|input| {
            input
                .with_declared_nyquist(&declared)
                .with_srv_fallback(self.render.srv_fallback())
        })
    }

    /// Take delivery of finished voxel builds.
    fn poll_voxel_results(&mut self) {
        use rustdar_volumetric::bridge::VolumeEntry;

        while let Ok(vr) = self.channels.voxel_receiver.try_recv() {
            let ready_grid = vr.grid.map(|grid| std::sync::Arc::new(*grid));
            let entry = match &ready_grid {
                Some(grid) => VolumeEntry::Ready(std::sync::Arc::clone(grid)),
                None => VolumeEntry::Refused(format!(
                    "This volume could not be resampled for 3D.\n\n({} at {} UTC)",
                    vr.target.volume.site, vr.target.volume.collected,
                )),
            };
            if !self.volume_store.complete(&vr.target, entry) {
                log::debug!(
                    "3D volume view: dropping a build for {} at {} that nothing is waiting for",
                    vr.target.volume.site,
                    vr.target.volume.collected,
                );
                continue;
            }
            log::info!(
                "3D volume view: the store holds {} volume(s), {} MiB",
                self.volume_store.live_ids().len(),
                self.volume_store.memory_bytes() / (1024 * 1024),
            );
        }
    }

    /// The current merged volume's whole-volume payload for `site` and `product`, extracted
    /// on this thread.
    fn extract_current_volume(
        &mut self,
        site: &str,
        product: rustdar_radar::types::RadarProduct,
    ) -> Option<rustdar_radar::render_input::RenderInput> {
        self.extract_site_volume(site, product, true)
    }

    /// The **base** volume's whole-volume payload for `site` and `product` — the same walk,
    /// over the base holder alone.
    fn extract_base_volume(
        &mut self,
        site: &str,
        product: rustdar_radar::types::RadarProduct,
    ) -> Option<rustdar_radar::render_input::RenderInput> {
        self.extract_site_volume(site, product, false)
    }

    /// The body of the two above: resolve the site's volume — with or without the live
    /// overlay merged in — and walk the product's moment out of it.
    fn extract_site_volume(
        &mut self,
        site: &str,
        product: rustdar_radar::types::RadarProduct,
        merge_live: bool,
    ) -> Option<rustdar_radar::render_input::RenderInput> {
        #[cfg(test)]
        self.volume_extractions
            .set(self.volume_extractions.get() + 1);
        let radar = rustdar_radar::sites::get_radar_site(site)?;
        let base = self.volumes.base_for(site);
        let overlay = merge_live
            .then(|| self.chunk_feeds.snapshot(site))
            .flatten();
        let current = rustdar_radar::current::resolve(
            base.as_ref()
                .map(|(scan, declared)| rustdar_radar::nyquist::Volume::new(scan, declared)),
            overlay
                .as_ref()
                .map(|live| rustdar_radar::nyquist::Volume::new(&live.scan, &live.declared)),
        )?;
        rustdar_radar::render_input::RenderInput::extract_volume_parts(
            current.pattern(),
            current.sweeps(),
            product,
            radar.lat,
            radar.lon,
            // The user's storm motion vector, for the worker-side SRV derivation; the
            // extraction keeps it only on an SRV payload.
            self.render.storm_motion_override_kt(),
        )
        .map(|input| {
            input
                .with_declared_nyquist(current.declared_nyquist())
                .with_srv_fallback(self.render.srv_fallback())
        })
    }

    /// The re-cut key for `site`'s current merged volume under `product` —
    /// [`rustdar_radar::sampler::ladder_fingerprint`] over the same resolve the section
    /// payload is extracted from, so the key and the cut cannot describe different volumes.
    pub(crate) fn current_ladder_fingerprint(
        &mut self,
        site: &str,
        product: rustdar_radar::types::RadarProduct,
    ) -> Option<u64> {
        let base = self.volumes.base_for(site);
        let overlay = self.chunk_feeds.snapshot(site);
        rustdar_radar::current::resolve(
            base.as_ref()
                .map(|(scan, declared)| rustdar_radar::nyquist::Volume::new(scan, declared)),
            overlay
                .as_ref()
                .map(|live| rustdar_radar::nyquist::Volume::new(&live.scan, &live.declared)),
        )?
        .ladder_fingerprint(product)
    }

    /// The stamp of `site`'s current merged volume: the newest data time (its identity,
    /// advanced by every sealed sweep) and the base volume's start where one contributes.
    fn current_volume_stamp(&mut self, site: &str) -> Option<rustdar_egui::CurrentVolumeStamp> {
        let base = self.volumes.base_with_time(site);
        let overlay = self.chunk_feeds.snapshot(site);
        let current = rustdar_radar::current::resolve(
            base.as_ref()
                .map(|(scan, declared, _)| rustdar_radar::nyquist::Volume::new(scan, declared)),
            overlay
                .as_ref()
                .map(|live| rustdar_radar::nyquist::Volume::new(&live.scan, &live.declared)),
        )?;
        let newest = current.newest_data_time()?;
        let base_started = (current.base_sweeps() > 0)
            .then(|| base.as_ref().map(|(_, _, collected)| *collected))
            .flatten();
        Some(rustdar_egui::CurrentVolumeStamp {
            newest,
            base_started,
        })
    }

    /// This pane is holding nothing, on the host **and** on the GPU.
    fn handle_release_volume(&mut self, pane_idx: usize) {
        self.volume_store.release(pane_idx);
        let live = self.volume_store.live_ids();
        if let Some(state) = self.state.as_mut()
            && let Some(resources) = state
                .egui_renderer
                .callback_resources_mut()
                .get_mut::<rustdar_volumetric::bridge::VolumeResources>()
        {
            resources.release_pane(pane_idx, &live);
        }
    }

    /// Give back what the layout's hidden panes are still holding — the other way a 3D pane
    /// stops needing its volume.
    pub(super) fn release_hidden_pane_volumes(&mut self) {
        // The panes the layout is showing, from the same slice `render_panes` draws — not
        // the raw `pane_count`, which may outrun the vector.
        let visible = self.gui.panes().len();
        for pane_idx in self.volume_store.hidden_holders(visible) {
            self.handle_release_volume(pane_idx);
            if let Some(pane) = self.gui.pane_mut(pane_idx)
                && let Some(volume) = pane.volume_mut()
            {
                volume.rendered_for = None;
            }
            log::debug!("3D volume view: pane {pane_idx} is hidden; released what it held");
        }
    }

    /// Record that this pane's 3D view is now about `target`, so it stops asking.
    fn mark_volume_rendered(&mut self, pane_idx: usize, target: &rustdar_egui::pane::VolumeTarget) {
        if let Some(pane) = self.gui.pane_mut(pane_idx)
            && let Some(volume) = pane.volume_mut()
        {
            volume.rendered_for = Some(target.clone());
        }
    }

    /// Process all GUI actions emitted during this frame.
    fn process_gui_actions(&mut self, actions: Vec<GuiAction>) {
        let mut overlay_renders: Vec<(usize, LayerId, fetch::OverlayRenderRequest)> = Vec::new();

        for action in actions {
            if let GuiAction::RenderOverlay {
                pane_idx,
                overlay_kind,
                geo_bounds,
                texture,
                data_generation,
                zoom,
            } = action
            {
                // The unexpanded viewport, which is what a region-scoped fetch wants — the
                // renderer's overdraw margin is a rasterization concern and would over-
                // fetch if it leaked into the request.
                self.last_viewport = Some(geo_bounds);
                overlay_renders.push((
                    pane_idx,
                    overlay_kind,
                    fetch::OverlayRenderRequest {
                        geo_bounds,
                        texture,
                        data_generation,
                        zoom,
                    },
                ));
            } else {
                log::debug!("GUI action received: {}", action);
                self.handle_gui_action(action, None);
            }
        }

        self.dispatch_overlay_renders(overlay_renders);
    }

    /// **The one way an overlay raster is asked for**, whichever path noticed
    /// it was needed.
    ///
    /// The draw loop's `needs_rerender` pass reaches it through
    /// [`Self::process_gui_actions`]; a data arrival reaches it through
    /// [`fetch::App::arrived_overlay_asks`]. Both get the same grouping, the
    /// same dedupe and the same `spawn_overlay_render` — which is what owns
    /// the `render_in_flight` marks, and is why neither path may call
    /// `offload_job` on its own: an unmarked dispatch is dispatched again on
    /// the next frame.
    fn dispatch_overlay_renders(
        &mut self,
        overlay_renders: Vec<(usize, LayerId, fetch::OverlayRenderRequest)>,
    ) {
        if overlay_renders.is_empty() {
            return;
        }
        let should_group = self.gui.overlay_renders_groupable();
        let grouped = deduplicate_overlay_renders(overlay_renders, should_group);
        for (pane_indices, id, req) in grouped {
            if should_group {
                log::debug!(
                    "Spawning overlay render for {} targeting {} panes",
                    id.as_str(),
                    pane_indices.len()
                );
            }
            // `None`: this is the pane's live raster, whichever path
            // noticed it was stale. A loop frame's dispatch is
            // `dispatch_overlay_loop_renders`, and it names its frame.
            self.spawn_overlay_render(pane_indices, id, req, None);
        }
    }

    /// Drain the archive scan channel and apply every queued volume.
    fn poll_scan_results(&mut self) {
        while let Ok(scan_resp) = self.channels.scan_receiver.try_recv() {
            if self
                .render
                .is_scan_stale(&scan_resp.site, scan_resp.requester, scan_resp.generation)
            {
                log::debug!(
                    "Discarding stale scan result for {} (gen {})",
                    scan_resp.site,
                    scan_resp.generation
                );
                self.gui.apply(GuiEvent::Fetching(false));
                self.gui.clear_loading_site_for_site(&scan_resp.site);
            } else {
                let requester = scan_resp.requester;
                match scan_resp.result {
                    Ok(scan_data) => {
                        // The archive path is the only one that can *learn*: a downloaded
                        // volume carries the Volume Data Block the chunk feed's reassembled
                        // `Scan` has no room for.
                        let scan_info = self.scan_info_learning_position(
                            &scan_data.scan,
                            &scan_data.site,
                            scan_data.timestamp,
                        );
                        let site = scan_data.site;
                        let timestamp = scan_data.timestamp;
                        let scan_arc = Arc::new(scan_data.scan);
                        // What the archive declared each cut's Nyquist velocity to be, held
                        // beside the volume for as long as it is the merge base.
                        let declared_nyquist = Arc::new(scan_data.declared_nyquist);

                        let feed_is_ahead = self.chunks_are_feeding(&site)
                            && self.any_pane_live_for_site(&site)
                            && !(self.manual_nav_pending && !scan_resp.is_auto_poll)
                            && fetch::latest_scan_time_for_site(self.gui.panes(), &site)
                                .is_some_and(|shown| timestamp <= shown);

                        let advances_the_base =
                            self.volumes.base_advances_to(&site, scan_info.timestamp);
                        if advances_the_base || !feed_is_ahead {
                            self.volumes.install_base(
                                site.clone(),
                                (
                                    Arc::clone(&scan_arc),
                                    Arc::clone(&declared_nyquist),
                                    scan_info.timestamp,
                                ),
                            );
                        }

                        let any_pane_live_for_site = scan_resp.is_auto_poll && {
                            let count = self.gui.pane_count();
                            (0..count).any(|i| {
                                self.gui
                                    .pane(i)
                                    .is_some_and(|p| p.site() == site && p.viewing_live)
                            })
                        };

                        if scan_resp.is_auto_poll && !any_pane_live_for_site {
                            log::info!("Auto-poll: caching scan (historic mode) @ {}", timestamp);
                            self.append_scan_to_active_loops(
                                &site,
                                timestamp,
                                Arc::clone(&scan_arc),
                                Arc::clone(&declared_nyquist),
                            );
                            self.latest_cached_scans
                                .insert(site, (scan_arc, declared_nyquist, scan_info, timestamp));
                        } else if feed_is_ahead {
                            log::info!(
                                "Keeping the real-time volume for {site}: the archive's \
                                 latest is {timestamp}, which is not newer"
                            );
                            self.append_scan_to_active_loops(
                                &site,
                                timestamp,
                                scan_arc,
                                declared_nyquist,
                            );
                            self.gui.apply(GuiEvent::Fetching(false));
                            self.gui.clear_loading_site_for_site(&site);
                        } else {
                            log::info!("Received scan data from background thread");
                            // Keyed by the volume's own collected-at, which is
                            // what the `ScanInfoForSite` below writes onto every
                            // pane on the site — so the key installed is the key
                            // the pane's render reads back with.
                            let forced = self.volumes.install_still(
                                site.clone(),
                                scan_info.timestamp,
                                (Arc::clone(&scan_arc), Arc::clone(&declared_nyquist)),
                            );
                            rustdar_worker::offload::discard_each("capped-still", forced);
                            self.gui.apply(fetch::scan_info_delivery(
                                site.clone(),
                                requester,
                                scan_info,
                            ));
                            self.gui.clear_loading_site_for_site(&site);
                            self.render.reset_panes_for_site(&site, &self.gui);
                            self.spawn_level3_fetches(&site);
                            self.refresh_extract_cache_for_site(&site);

                            self.append_scan_to_active_loops(
                                &site,
                                timestamp,
                                Arc::clone(&scan_arc),
                                Arc::clone(&declared_nyquist),
                            );

                            if self.manual_nav_pending {
                                self.manual_nav_pending = false;
                                self.reinit_active_loops();
                            }

                            log::info!("Scan data loaded and UI updated");
                        }
                    }
                    Err(error_msg) => {
                        log::error!("Received error from background thread: {}", error_msg);
                        self.gui.apply(GuiEvent::Error(error_msg));
                        self.gui.clear_loading_site_for_site(&scan_resp.site);
                    }
                }
            }
        }
    }

    /// Poll all data channels for completed async results (scan, overlays).
    fn poll_data_channels(&mut self) {
        self.run_frame_pump(frame_pump::PumpPhase::Ingest, None);
    }

    /// Drain the unified overlay fetch channel — **every** arrival a source
    /// produces, in one `match`.
    fn poll_overlay_fetch_results(&mut self) {
        use rustdar_overlays::render::overlay_state::SourceEvent;
        // The two radar-shaped outcomes are collected rather than acted on
        // in place: the decode needs the whole `App`, and `gui` is bound once
        // for the whole drain. The decode is offloaded either way, so nothing
        // observable turns on where in this pass it is dispatched.
        let mut listed: Vec<render::LoopListingArrival> = Vec::new();
        let mut archives: Vec<rustdar_radar::source::RadarFrameFetch> = Vec::new();
        // The layers this drain installed data for, deduplicated: two rounds of
        // the same layer in one pass are one re-ask, not two.
        let mut arrived: Vec<rustdar_source::id::LayerId> = Vec::new();
        // Bound once for the whole drain, not per arrival.
        let gui = &mut self.gui;
        while let Ok(event) = self.channels.overlay_fetch_receiver.try_recv() {
            // Not "the pane the fetch was for": the arrival carries a layer
            // id and no pane, and what the handler needs of it is the whole
            // layer's — every pane's selection, unioned. `Gui` owns the panes
            // and builds that view.
            match event {
                SourceEvent::Data(result) => {
                    // **The raster-now obligation `Data` carries** (WO-M13a):
                    // which layer just changed, so the panes already showing
                    // it can be re-asked below without waiting for a frame to
                    // notice.
                    let id = result.kind.clone();
                    gui.deliver_overlay_fetch(result);
                    if !arrived.contains(&id) {
                        arrived.push(id);
                    }
                }
                SourceEvent::Frames { id, listing, scope } => {
                    // **What the listing was about is in the scope**, which is
                    // the layer's own type: the generic halves — the stamps,
                    // the `PaneRef` union — name no site by contract, and this
                    // crate is radar's own frontend. The site is read here and
                    // the scope is handed on whole.
                    //
                    // **Every layer's listing is recorded, radar's carrying
                    // the extra half.** The loop builder needs the window to
                    // find the panes that asked for it, and that window is
                    // generic; the site is what radar's own arm additionally
                    // matches on, so it rides along as `Some` for radar and
                    // is absent for everyone else. A radar listing under a
                    // scope that is not radar's is still recorded for nobody,
                    // exactly as before.
                    let radar = scope.downcast_ref::<rustdar_radar::source::RadarListing>();
                    if id == rustdar_source::id::known::RADAR {
                        if let Some(radar) = radar {
                            listed.push(render::LoopListingArrival {
                                layer: id.clone(),
                                site: Some(radar.site.clone()),
                                range: radar.range,
                            });
                        }
                    } else {
                        listed.push(render::LoopListingArrival {
                            layer: id.clone(),
                            site: None,
                            range: listing.range,
                        });
                    }
                    gui.deliver_frame_listing(&id, listing, scope);
                }
                SourceEvent::FrameReady { id, stamp, data } => {
                    // Radar's frames are held by the loop cache this crate
                    // owns, so its bytes are taken below and decoded through
                    // the funnel; every other layer's go to the handler.
                    if id == rustdar_source::id::known::RADAR {
                        match data.downcast::<rustdar_radar::source::RadarFrameFetch>() {
                            Ok(fetch) => archives.push(*fetch),
                            Err(data) => gui.deliver_frame(&id, stamp, data),
                        }
                    } else {
                        gui.deliver_frame(&id, stamp, data);
                    }
                }
            }
        }
        self.loop_listings_arrived.extend(listed);
        for fetch in archives {
            self.take_loop_frame_archive(fetch);
        }
        // **The arrival half of the render trigger.** The draw loop's
        // `needs_rerender` pass is untouched and still runs every frame — it
        // remains the only discoverer for a first render, a resize, a pan and a
        // zoom settle, and it is still where the settle clock is recorded.
        //
        // Said precisely, because the loose version is false: this does not
        // add a *raster*. On the frame data arrives, the pass would have seen
        // the same moved token and pushed its own action; it now finds
        // either the mark set or — if the raster already landed — the cache's
        // own token caught up, and declines on guards it has always had.
        // What moved is who initiated the one raster and when —
        // this runs in `Ingest`, ahead of the paint-list build and the
        // present that a draw-time action waits behind.
        let asks = self.arrived_overlay_asks(&arrived);
        self.dispatch_overlay_renders(asks);
    }

    /// Tell the UI each site's current-volume stamp — what a whole-volume pane may build
    /// from, and how fresh it is.
    ///
    /// Returns the sites whose stamp **moved on this frame**: a volume that
    /// installed, or one whose newest sweep just sealed. That is the arrival
    /// moment WO-M14c dispatches a 3D build at, and it is derivable only here,
    /// where the previous stamp is still in hand to compare against.
    fn publish_base_volumes(&mut self) -> HashMap<String, rustdar_egui::CurrentVolumeStamp> {
        let mut sites: Vec<String> = self.volumes.sites_with_base().map(str::to_owned).collect();
        for site in self.gui.live_sites() {
            if !sites.contains(&site) {
                sites.push(site);
            }
        }
        let mut stamps = HashMap::new();
        for site in sites {
            if let Some(stamp) = self.current_volume_stamp(&site) {
                stamps.insert(site, stamp);
            }
        }
        if self.radar_liveness.current_volumes == stamps {
            return HashMap::new();
        }
        let arrived: HashMap<String, rustdar_egui::CurrentVolumeStamp> = stamps
            .iter()
            .filter(|(site, stamp)| self.radar_liveness.current_volumes.get(*site) != Some(*stamp))
            .map(|(site, stamp)| (site.clone(), *stamp))
            .collect();
        self.radar_liveness.current_volumes = stamps;
        self.republish_liveness();
        arrived
    }

    /// **Build the volume a 3D pane is waiting for on the frame it arrives**,
    /// instead of on the frame after the draw loop notices (WO-M14c).
    ///
    /// The identical call the draw-time level-trigger makes — same refusal
    /// path, same budget gate inside [`Self::prepare_volume`], same
    /// `mark_volume_rendered` bookkeeping, which is what makes the trigger
    /// quiesce rather than ask again. When the budget gate turns the eager ask
    /// away (on wasm the render budget is 1, so a busy slot does exactly that)
    /// nothing is marked and the draw-time trigger picks it up as before: the
    /// eager ask **losing** is the fallback working, not a fault.
    ///
    /// Which panes qualify is [`Self::arrived_volume_asks`]', and its doc is
    /// where the boundary is written down.
    fn dispatch_arrived_volumes(
        &mut self,
        arrived: &HashMap<String, rustdar_egui::CurrentVolumeStamp>,
    ) {
        for (pane_idx, layer, target) in self.arrived_volume_asks(arrived) {
            log::debug!(
                "3D volume view: pane {pane_idx} asked {} for {} at {} UTC as it arrived, \
                 ahead of the draw loop",
                layer.as_str(),
                target.product.as_str(),
                target.volume.collected,
            );
            self.handle_prepare_volume(pane_idx, &layer, target);
        }
    }

    /// **Rebuild the liveness seam's entry for the radar layer.**
    ///
    /// Called only where a half of it actually moved. The payload sits behind
    /// an `Arc` that every frame re-states, so a per-frame rebuild would be a
    /// per-frame allocation and a per-frame map clone for a value that
    /// changes on the order of once a scan.
    fn republish_liveness(&mut self) {
        let entry = rustdar_egui::radar_layer::liveness_entry(self.radar_liveness.clone());
        match self
            .liveness
            .iter_mut()
            .find(|existing| existing.id == entry.id)
        {
            Some(slot) => *slot = entry,
            None => self.liveness.push(entry),
        }
    }

    /// Drop the decoded volumes no pane is showing.
    fn evict_unshown_scans(&mut self) {
        // One query, not two: this runs on every frame, and the second call was
        // asking the same question of the same unchanged Gui.
        let pane_count = self.gui.pane_count();
        let mut shown: Vec<&str> = Vec::with_capacity(pane_count * 2);
        // What a pane is actually parked at, exactly. The still store is keyed
        // by moment as well as site, so "this site is on screen" is no longer a
        // fine enough question to retain by: two panes on one site at two
        // moments hold two volumes, and the one nobody is parked at has to go.
        let mut parked: Vec<(&str, chrono::NaiveDateTime)> = Vec::with_capacity(pane_count);
        for idx in 0..pane_count {
            let Some(pane) = self.gui.pane(idx) else {
                continue;
            };
            shown.push(pane.site());
            if let Some(info) = pane.scan_info.as_ref() {
                shown.push(info.site.name);
                parked.push((info.site.name, info.timestamp));
            }
        }
        // place is this frame: an entry is a whole decoded volume, a measured 46 MiB
        // median and 58.3 MiB worst case (`volume_inventory::MAX_RESIDENT_STILL_VOLUMES`)
        // across thousands of per-radial buffers, and the walk that returns them is the
        // frame-thread cost `offload::discard` exists to move.
        let unshown = |site: &String| !shown.iter().any(|shown| *shown == site);
        // A pane on a site whose `scan_info` has not caught up to the newest
        // volume there keeps that newest one — the site-keyed retention this
        // replaces, narrowed from "every moment for the site" to exactly one.
        // Resolved here rather than inside `wanted`, which cannot ask the store
        // a question while the store is being retained.
        for &site in &shown {
            if let Some(at) = self.volumes.newest_still_for(site)
                && !parked.iter().any(|&(s, t)| s == site && t == at)
            {
                parked.push((site, at));
            }
        }
        let wanted = |site: &str, at: chrono::NaiveDateTime| {
            parked.iter().any(|&(s, t)| s == site && t == at)
        };
        let evicted_stills = self.volumes.retain_still(&wanted);
        rustdar_worker::offload::discard_each("evicted-scan", evicted_stills);
        rustdar_worker::offload::discard_each(
            "evicted-base-volume",
            self.volumes.evict_base(&unshown),
        );
        rustdar_worker::offload::discard_each(
            "evicted-cached-volume",
            evicted(&mut self.latest_cached_scans, &unshown),
        );
        self.render.retain_extracts(|key| !unshown(&key.site));
        self.evict_unneeded_loop_scans();
        rustdar_radar::derive::retain_volumes(
            self.volumes
                .resident()
                .chain(
                    self.latest_cached_scans
                        .values()
                        .map(|(scan, _, _, _)| scan.as_ref()),
                )
                .chain(self.loop_mgr.cached_scans()),
        );
    }

    /// Drop the loop caches' data no live loop frame names — the decoded Level II volumes
    /// and the paired Level III objects alike.
    fn evict_unneeded_loop_scans(&mut self) {
        let mut needed: HashMap<&str, std::collections::HashSet<chrono::NaiveDateTime>> =
            HashMap::new();
        let mut settling: std::collections::HashSet<&str> = std::collections::HashSet::new();
        // One instant for the whole sweep, so two panes fetching the same listing cannot be
        // judged against two different clocks.
        let now = web_time::Instant::now();
        for idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane(idx) else {
                continue;
            };
            if let Some(info) = pane.scan_info.as_ref() {
                needed
                    .entry(info.site.name)
                    .or_default()
                    .insert(info.timestamp);
            }
            let ls = &pane.loop_state();
            if !ls.is_active() {
                continue;
            }
            if ls.listing_wait(now).is_some_and(|waited| {
                waited < rustdar_device_profile::constants::LOOP_LISTING_GRACE
            }) {
                settling.insert(radar_layer::site(ls));
            }
            let frames = needed.entry(radar_layer::site(ls)).or_default();
            for frame in &ls.frames {
                frames.insert(frame.timestamp);
            }
        }
        let keep = |site: &str, ts: &chrono::NaiveDateTime| {
            settling.contains(site) || needed.get(site).is_some_and(|frames| frames.contains(ts))
        };
        self.loop_mgr.retain_plan_frames(keep);
        rustdar_worker::offload::discard_each(
            "evicted-loop-volume",
            self.loop_mgr.retain_scans(keep),
        );
        rustdar_worker::offload::discard_each("evicted-loop-object", self.loop_mgr.retain_l3(keep));
        let keep_site = |site: &str| settling.contains(site) || needed.contains_key(site);
        rustdar_worker::offload::discard_each(
            "evicted-loop-l3-listing",
            self.loop_mgr.retain_l3_keys(keep_site),
        );
    }

    /// Persist the config if it has changed and the interval has elapsed.
    fn autosave_config(&mut self, force: bool) {
        let now = web_time::Instant::now();
        if !force
            && let Some(last) = self.autosave.last_check
            && now.duration_since(last) < AUTOSAVE_INTERVAL
        {
            return;
        }
        self.autosave.last_check = Some(now);
        self.autosave.touched = false;

        let Some(json) = self.gui.ui_config_json() else {
            return;
        };
        if self.autosave.last_written.as_deref() == Some(json.as_str()) {
            return;
        }
        let Some(store) = self.platform.kv() else {
            return;
        };
        match store.store(rustdar_egui::UI_CONFIG_KEY, &json) {
            // For a backend that queues, this says "accepted", not "written" — a write that
            // then fails is reported where it failed.
            Ok(()) => self.autosave.last_written = Some(json),
            Err(e) => log::warn!("config autosave failed: {e}"),
        }
    }

    /// Build a volume's [`ScanInfo`], and remember anything it taught about where its radar
    /// is.
    fn scan_info_learning_position(
        &mut self,
        scan: &nexrad_model::data::Scan,
        site: &str,
        requested_timestamp: chrono::NaiveDateTime,
    ) -> ScanInfo {
        let mut info = ScanInfo::from_scan(
            scan,
            site,
            requested_timestamp,
            self.site_positions.get(site),
        );
        if info.site_source == SitePositionSource::Volume
            && let Some(position) = info.site_position
        {
            let store = self.platform.kv();
            let learned = self.site_positions.learn(store.as_deref(), site, position);
            // `store` borrows `self.platform`; the resolve below wants two other fields and
            // the bump wants `self.gui`.
            drop(store);
            if learned {
                rustdar_radar::sites::resolve(
                    self.site_positions
                        .fixes()
                        .chain(self.site_catalogue.fixes()),
                );
                self.gui.bump_all_radar_sites_gen();
            }
        }
        // The info above was built against the table as it stood *before* the line
        // that just taught it where this radar is. On an install with no cached
        // catalogue that table was empty, so the volume that supplies the position
        // is the one whose info cannot name its own radar -- and `UNKNOWN` is not a
        // key the still store holds, so the picture is never made.
        info.place_against_the_table(site);
        info
    }

    /// Replace a timezone-guessed site with the one nearest an actual fix.
    fn upgrade_provisional_site(&mut self, fix: &rustdar_location::Fix) {
        if !self.site_is_provisional {
            return;
        }
        if !fix.fix_quality.can_relocate() {
            return;
        }
        if !rustdar_location::fix_is_accurate_enough_to_relocate(fix.accuracy_m) {
            log::debug!(
                "ignoring a {:.0} km fix for the opening site; the timezone \
                 guess it would replace is better than that",
                fix.accuracy_m.unwrap_or_default() / 1000.0
            );
            return;
        }
        let Some((site, dist)) =
            rustdar_radar::sites::nearest_wsr88d_site(fix.point.lat, fix.point.lon)
        else {
            return;
        };
        self.site_is_provisional = false;
        // The pane asked has to be the pane moved. They were both pane 0 while there was
        // one pane; with two, asking pane 0 about a switch aimed at the active pane skips
        // the move and spends the upgrade anyway.
        let pane_idx = self.gui.active_pane_idx();
        if self
            .gui
            .pane(pane_idx)
            .is_some_and(|p| p.site() == site.name)
        {
            return;
        }
        log::info!(
            "location fix refines the opening site to {} ({dist:.0} km)",
            site.name
        );
        self.handle_gui_action(
            GuiAction::SwitchRadarSite {
                site: site.name.to_string(),
                pane_idx,
            },
            None,
        );
    }

    /// Arrange for one more look if a change might still be unsaved.
    fn schedule_wakeup(&self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(self.wakeup_control_flow());
    }

    /// The state the loop should be left in, given what the autosave still owes, when egui
    /// next wants a timed repaint, and when auto-poll next needs a frame — whichever comes
    /// first.
    fn wakeup_control_flow(&self) -> ControlFlow {
        let now = web_time::Instant::now();
        let until = |at: Option<web_time::Instant>| at.map(|at| at.saturating_duration_since(now));
        let delay = [
            self.autosave_delay(),
            until(self.egui_repaint_at),
            until(self.auto_poll_at),
        ]
        .into_iter()
        .flatten()
        .min();
        match delay {
            // `wait_duration` rather than `WaitUntil`: winit's `Instant` is `std::time`'s
            // natively and `web_time`'s on wasm, so no single instant value typechecks for
            // both targets.
            Some(delay) => ControlFlow::wait_duration(delay),
            None => ControlFlow::Wait,
        }
    }

    /// How long the loop may sleep before the autosave next needs a look, or `None` when
    /// nothing is owed and it may sleep until something happens.
    fn autosave_delay(&self) -> Option<std::time::Duration> {
        if !self.autosave.touched {
            return None;
        }
        let deadline = self
            .autosave
            .last_check
            .map(|last| last + AUTOSAVE_INTERVAL)
            .unwrap_or_else(web_time::Instant::now);
        Some(deadline.saturating_duration_since(web_time::Instant::now()))
    }

    /// How long the loop may sleep before one of the app's timers next needs a **frame**,
    /// or `None` when none of them do and it may sleep until something happens.
    fn auto_poll_delay(&self) -> Option<std::time::Duration> {
        [
            self.gui.auto_poll_delay(),
            self.gui.status_tick_delay(),
            self.chunk_feeds.next_round_delay(),
            self.chunk_notify.next_retry_delay(),
        ]
        .into_iter()
        .flatten()
        .min()
        .map(|delay| if delay.is_zero() { MIN_WAKE } else { delay })
    }

    fn request_exit(&mut self, event_loop: Option<&ActiveEventLoop>) {
        if let Some(store) = self.platform.kv() {
            self.gui.save_ui_config(store.as_ref());
        }
        if !self.platform.supports_exit() {
            log::debug!("exit requested; ignored (this platform has no quit)");
            return;
        }
        if let Some(event_loop) = event_loop {
            self.exit_now(event_loop);
        } else {
            self.exit_requested = true;
        }
    }

    /// Leave, now: the half of [`request_exit`](Self::request_exit) that needs an event
    /// loop.
    fn exit_now(&self, event_loop: &ActiveEventLoop) {
        log::info!("Exiting application");
        if let Some(store) = self.platform.kv() {
            self.gui.save_ui_config(store.as_ref());
        }
        event_loop.exit();
        if self.platform.needs_process_exit() {
            std::process::exit(0);
        }
    }

    /// Set a callback to handle the back button (e.g.
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
        // Load config now — on Android this is called after App::new(), so the initial load
        // in new() had no config dir yet.
        if let Some(store) = self.platform.kv() {
            self.site_positions = crate::site_positions::SitePositions::load(Some(store.as_ref()));
            self.site_catalogue = crate::site_catalogue::load(Some(store.as_ref()));
            let table = rustdar_radar::sites::resolve(
                self.site_positions
                    .fixes()
                    .chain(self.site_catalogue.fixes()),
            );
            self.catalogue_pending = self.site_catalogue.is_empty();
            if self.gui.load_ui_config(store.as_ref()) {
                self.site_is_provisional = false;
                self.site_hint_pending = false;
            } else {
                self.site_hint_pending = table.rows().is_empty();
                if !self.site_is_provisional {
                    self.site_is_provisional =
                        apply_location_hint(&mut self.gui, self.platform.as_ref());
                }
            }
        }
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

    /// Set a callback that takes a back press delivered outside the input queue (Android's
    /// `OnBackInvokedDispatcher`; see [`PlatformBridge::poll_back_press`]).
    pub fn set_back_press_taker(&mut self, taker: fn() -> bool) {
        self.platform.set_back_press_taker(taker);
    }

    /// Set the sink this app publishes its predictive-back claim to (Android only;
    /// see [`PlatformBridge::set_back_claimed`]).
    pub fn set_back_claim_reporter(&mut self, reporter: fn(bool)) {
        self.platform.set_back_claim_reporter(reporter);
    }

    /// Set what [`suspended`](Self::suspended) asks to tell a finish from a
    /// backgrounding (Android only).
    pub fn set_terminal_suspend_probe(&mut self, probe: fn() -> bool) {
        self.platform.set_terminal_suspend_probe(probe);
    }

    /// Whether egui is going to want this key press for itself.
    fn ui_is_taking_keys(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.egui_renderer.context().egui_wants_keyboard_input())
    }

    fn handle_input_events(&mut self, event_loop: &ActiveEventLoop) {
        if self.input.take_back_out_press() && !self.ui_is_taking_keys() {
            self.back_out(event_loop);
        }
    }

    /// Tell the platform whether the next back press has something to close.
    ///
    /// Registering and unregistering truthfully at every sheet transition IS the
    /// whole obligation the Android predictive-back route places on this side:
    /// the dispatcher decides before the gesture whether the app or the system
    /// owns it, and takes no answer afterwards. So this runs at the end of every
    /// frame, once the frame's actions have been applied and the Gui's state is
    /// final.
    ///
    /// On change only. The far end is a JNI static call, and a per-frame hop at
    /// 120 Hz is the cost this shape exists to avoid.
    ///
    /// The claim is not consulted by anything yet: Android ships opted OUT of
    /// the predictive-back dispatcher, on a measurement recorded in the app's
    /// manifest, so back still arrives as a key. What this keeps alive is the
    /// truthfulness, which is the part that cannot be added later in a hurry.
    fn push_back_claim(&mut self) {
        let claimed = self.gui.back_would_dismiss();
        if claimed != self.back_claimed {
            self.back_claimed = claimed;
            self.platform.set_back_claimed(claimed);
        }
    }

    /// One press of Escape or the back button.
    fn back_out(&mut self, event_loop: &ActiveEventLoop) {
        match Self::resolve_back_press(&mut self.gui, self.platform.as_ref()) {
            BackPress::Dismissed => notify_redraw(&self.window),
            BackPress::PlatformHandled | BackPress::Ignored => {}
            BackPress::Exit => self.request_exit(Some(event_loop)),
        }
    }

    /// Resolve one press of Escape or back.
    ///
    /// The last line is the platform's to answer, not this function's: with
    /// nothing open and nothing taking the press, quitting is a platform
    /// property, asked through the bridge rather than forked on a `cfg` here.
    fn resolve_back_press(gui: &mut Gui, platform: &dyn PlatformBridge) -> BackPress {
        if gui.dismiss_top_layer() {
            return BackPress::Dismissed;
        }
        if platform.handle_back() {
            return BackPress::PlatformHandled;
        }
        if platform.exits_on_unhandled_back() {
            return BackPress::Exit;
        }
        BackPress::Ignored
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        // The bridge gets to amend the attributes because the web backend has to bind its
        // canvas here and nowhere else.
        let attributes = self
            .platform
            .window_attributes(Window::default_attributes().with_title("Rustdar"));
        let window = event_loop.create_window(attributes).unwrap();

        let window = Arc::new(window);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = window.request_inner_size(PhysicalSize::new(RENDER_WIDTH, RENDER_HEIGHT));
        self.window = Some(window.clone());

        let held = Some(window.clone());
        self.redraw_waker.install(move || notify_redraw(&held));

        window.request_redraw();
    }
}

/// Deduplicate overlay render requests.
fn deduplicate_overlay_renders(
    overlay_renders: Vec<(usize, LayerId, fetch::OverlayRenderRequest)>,
    should_group: bool,
) -> Vec<(Vec<usize>, LayerId, fetch::OverlayRenderRequest)> {
    if !should_group {
        return overlay_renders
            .into_iter()
            .map(|(pane_idx, id, req)| (vec![pane_idx], id, req))
            .collect();
    }

    struct GroupedRender {
        id: LayerId,
        req: fetch::OverlayRenderRequest,
        pane_indices: Vec<usize>,
    }

    let mut grouped: HashMap<(LayerId, i32, u64, u32, u32), GroupedRender> = HashMap::new();

    for (pane_idx, id, req) in overlay_renders {
        let key = (
            id.clone(),
            req.zoom,
            req.data_generation,
            req.texture.width,
            req.texture.height,
        );
        grouped
            .entry(key)
            .and_modify(|g| {
                if !g.pane_indices.contains(&pane_idx) {
                    g.pane_indices.push(pane_idx);
                }
            })
            .or_insert_with(|| GroupedRender {
                id,
                req,
                pane_indices: vec![pane_idx],
            });
    }

    grouped
        .into_values()
        .map(|g| (g.pane_indices, g.id, g.req))
        .collect()
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        log::info!("App resumed");
        self.create_window(event_loop);

        self.refresh_safe_area_insets();

        // A location permission can be changed in system settings while the app is in the
        // background, and in a settled state the gate has stopped polling for it entirely —
        // so this is the one moment a revocation made outside the app is noticed at all.
        self.location.resumed();
    }

    /// Pick up a back press the platform delivered outside the input queue.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.platform.poll_back_press() {
            self.back_out(event_loop);
        }
        if self
            .egui_repaint_at
            .is_some_and(|at| web_time::Instant::now() >= at)
        {
            self.egui_repaint_at = None;
            notify_redraw(&self.window);
        }
        if self
            .auto_poll_at
            .is_some_and(|at| web_time::Instant::now() >= at)
        {
            self.auto_poll_at = None;
            notify_redraw(&self.window);
        }
        self.autosave_config(false);
        self.schedule_wakeup(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        log::info!("App suspended - clearing graphics state");
        // Save config on suspend — on Android this is the only reliable save point before
        // the system may kill the process.
        if let Some(store) = self.platform.kv() {
            self.gui.save_ui_config(store.as_ref());
        }
        self.render.clear_last_rendered();
        self.texture_counter = 0;
        self.gui.clear_graphics_state(); // Keep cached_render intact so we can re-upload the texture
        self.window = None;
        self.state = None;
        // The third holder of that window, and the only one this thread does not own
        // outright: five sensor threads have a clone of the waker, and on Android
        // so does the predictive-back callback's parking slot.
        self.redraw_waker.detach();
        #[cfg(target_arch = "wasm32")]
        {
            self.pending_state = None;
        }
        // A suspend is usually the app going to the background, and the loop
        // keeps running. When the platform says this one is a *finish*, the
        // loop has to end here and nowhere else: Android's glue blocks the
        // Java UI thread inside `onDestroy` waiting for this thread to stop,
        // and the window is already gone by the time that runs, so `Suspended`
        // is the last moment at which ending it is still cheap.
        //
        // `exit_now`, which takes the process down with it on the platform
        // that needs it, and that is not a shortcut. Letting the loop unwind
        // and leaving the process warm was tried first, because a warm process
        // is the better reopen: it does not work. winit permits **one
        // `EventLoop` per process** (`EventLoopError::RecreationAttempt`), so
        // the second `android_main` in a surviving process panics building
        // one -- measured on the emulator, 2026-08-21, at
        // `rustdar/src/android/entry.rs`'s `EventLoop::build`. Ending the
        // process is what makes the relaunch a fresh one, and a fresh one is
        // the only kind that works.
        if self.platform.suspend_is_terminal() {
            log::info!("Activity is finishing - ending the event loop");
            self.exit_now(event_loop);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.input.process_event(&event) {
            self.handle_input_events(event_loop);
        }

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
                if std::mem::take(&mut self.exit_requested) {
                    self.exit_now(event_loop);
                }
            }
            WindowEvent::Resized(new_size) => {
                self.handle_resized(new_size.width, new_size.height);
                notify_redraw(&self.window);
            }
            WindowEvent::ThemeChanged(theme) => {
                if self.adopt_theme(matches!(theme, winit::window::Theme::Dark)) {
                    notify_redraw(&self.window);
                }
            }
            _ => {
                self.autosave.touched = true;
                if needs_repaint {
                    notify_redraw(&self.window);
                }
            }
        }
    }
}

#[cfg(test)]
mod chunk_feed_precedence_tests;

#[cfg(test)]
mod gui_action_replay_tests;

#[cfg(test)]
mod gui_seam_ratchet_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod theme_flip_tests;

/// The far end of the 3D ask: the layer it names, resolved.
#[cfg(test)]
mod volume_layer_tests;

/// The arrival path: a 3D pane's volume built on the frame it lands.
#[cfg(test)]
mod volume_arrival_tests;

/// Archive delivery is addressed to the pane that asked, not broadcast.
#[cfg(test)]
mod time_group_delivery_tests;
