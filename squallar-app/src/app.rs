use egui_wgpu::wgpu;
use squallar_egui::radar_layer;
use squallar_source::id::known;
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
use squallar_device_profile::constants::{RENDER_HEIGHT, RENDER_WIDTH};
use squallar_egui::shell_api::GuiEvent;
use squallar_egui::{Gui, actions::GuiAction};
use squallar_location::LocationFacade;
use squallar_radar::loop_downloads::LoopDownloadManager;
use squallar_radar::site_position::SitePositionSource;
use squallar_radar::types::ScanInfo;
use squallar_source::id::LayerId;

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
///
/// The browser asks for **both** browser APIs. Which of the two it ends up on
/// is not decided here — see [`create_instance`], where it has to be decided,
/// and why asking for both is not the same as getting the better one.
fn backends_for(web: bool, base: wgpu::InstanceDescriptor) -> wgpu::InstanceDescriptor {
    if web {
        wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU.union(wgpu::Backends::GL),
            ..base
        }
    } else {
        base
    }
}

/// The wgpu instance this build renders through.
///
/// Async, and it has to be: **WebGPU support cannot be decided synchronously.**
/// `Instance::new` binds the WebGPU context whenever `navigator.gpu` merely
/// *exists*, and a browser that exposes that object can still answer
/// `requestAdapter()` with null. Chromium 151 does exactly that on its default
/// path, measured 2026-08-22 on both rig arms — headless on SwiftShader and
/// headed on an RTX 3090 — so this is the ordinary case and not a corner of it.
/// Once the instance is built there is no second
/// chance, because `create_surface` on the WebGPU backend calls
/// `canvas.getContext("webgpu")`, and a canvas that has answered one
/// `getContext` never answers another with a different id. A single instance
/// widened to `BROWSER_WEBGPU | GL` would therefore not fall back at all; it
/// would find no adapter, on precisely the browsers WebGL2 still serves.
///
/// wgpu's own detecting constructor is what resolves it: it issues the adapter
/// request first and drops `BROWSER_WEBGPU` from the mask when it comes back
/// empty, leaving the WebGL2 half of what [`backends_for`] asked for. On
/// Firefox/Linux — which governs here, and where WebGPU is still unshipped —
/// that is every run, and what renders is the WebGL2 path this build already
/// had.
///
/// On every native target the probe is `false` at compile time (wgpu's
/// `cfg(webgpu)` alias is wasm32-only), so this is `Instance::new` with one
/// extra `await`, and `WGPU_BACKEND` still decides.
async fn create_instance() -> wgpu::Instance {
    egui_wgpu::wgpu::util::new_instance_with_webgpu_detection(instance_descriptor()).await
}

/// Request a redraw if a window handle is available.
///
/// Every ask for a frame in this app arrives here, from about thirty sites and
/// a dozen threads, which is what makes it the one place the platform call can
/// be made safe for all of them at once. What that means, and why a thread that
/// is not the loop's own never makes that call itself, is on
/// [`crate::platform::ask_for_a_frame`].
pub(crate) fn notify_redraw(window: &Option<WindowRef>) {
    if let Some(w) = window {
        crate::platform::ask_for_a_frame(w);
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

/// **Every pane restored parked on an instant**, as `(index, site, instant)`.
///
/// Radar's predicate matches the one a manual step uses: a pane that draws no
/// radar is not asking for a volume, and fetching one it would not paint is
/// speculation.
fn parked_panes(gui: &Gui) -> Vec<(usize, String, chrono::NaiveDateTime)> {
    gui.panes()
        .iter()
        .enumerate()
        .filter_map(|(idx, pane)| {
            let instant = pane.time.mode.as_of()?;
            if !pane.is_overlay_enabled(&squallar_source::id::known::RADAR) {
                return None;
            }
            let site = pane.site().to_string();
            (!site.is_empty()).then_some((idx, site, instant))
        })
        .collect()
}

/// **Every pane restored wanting a loop**, as `(index, request)`.
///
/// The sibling of [`parked_panes`], and collected the same way: read off the
/// panes once at startup, acted on when the reload is hydrated. The request
/// is read rather than taken: the pane's copy is what a config save made
/// while the arm is still waiting for its first scan writes back, and
/// `handle_enable_loop` is what consumes it when the wish resolves — so
/// nothing acts on it twice.
/// The lookback is read here, beside the panes, because it is the same
/// persisted setting the timeline's own Enable-loop action carries — a restored
/// loop must span what the user left the slider on, not a fresh default.
fn looping_panes(gui: &Gui) -> Vec<(usize, squallar_egui::pane::LoopArm, u64)> {
    let lookback = gui.loop_lookback_secs;
    gui.panes()
        .iter()
        .enumerate()
        .filter_map(|(idx, pane)| pane.loop_arm_pending.map(|arm| (idx, arm, lookback)))
        .collect()
}

pub struct App {
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
    user_gps: Option<(squallar_location::Fix, web_time::Instant)>,
    /// Compass heading in degrees, once a platform has delivered one.
    user_heading: Option<f32>,
    /// **What the radar layer says it is doing** — the chunk feed's status and
    /// each site's current-volume stamp, in the layer's own type.
    ///
    /// Both halves are recomputed every frame (`drive_chunk_feeds`,
    /// `publish_base_volumes`) so the status bar never shows a stale claim,
    /// but the seam's entry is rebuilt only when this **changes**: see
    /// [`Self::republish_liveness`].
    radar_liveness: squallar_egui::radar_layer::RadarLiveness,
    /// The seam's own value, one entry per layer that publishes one. Rebuilt
    /// on change, re-stated every frame.
    liveness: Vec<squallar_source::liveness::SourceLiveness>,
    /// The same painter [`Gui`] was handed, kept so the frame path can take its floor-
    /// magnification demand.
    volume_painter: Option<Arc<squallar_volumetric::bridge::BridgeVolumePainter>>,
    /// The rung the pane mirror is drawn at, and the hysteresis that governs when it may
    /// move.
    mirror_rungs: squallar_gpu::egui_renderer::MirrorRungs,
    /// The plan the mirror texture was last actually sized and rendered to.
    /// Compared against every frame's observed plan: on a held (clean-skip)
    /// frame the two disagreeing means a realloc is owed, and the realloc is
    /// deferred behind [`Self::mirror_plan_stamp`] so it lands on a frame
    /// whose primitives carry every strip.
    mirror_plan_applied: Option<squallar_gpu::egui_renderer::MirrorPlan>,
    /// Bumped when a plan change is deferred off a held frame; travels to the
    /// Gui in `FrameInputs`, where it forces the strip repaint the realloc
    /// needs.
    mirror_plan_stamp: u64,
    /// Every per-target number this build spends, resolved once from a
    /// [`squallar_device_profile::budget::DeviceProfile`] and threaded from here.
    budgets: squallar_device_profile::budget::Budgets,
    /// Everything known about the machine, and the only input [`Self::budgets`] has.
    device_profile: squallar_device_profile::budget::DeviceProfile,
    /// The application's whole loop allowance, and the hysteresis that governs how it is
    /// divided.
    loop_pool: crate::loop_pool::LoopPool,
    /// See [`Self::loop_pool`].
    loop_pool_state: crate::loop_pool::LoopPoolState,
    /// Whether [`Self::loop_pool`] is already the answer for this machine.
    loop_pool_sized: bool,
    /// **What the loops were holding at the last pool observation** — the pane
    /// half of [`crate::loop_telemetry`]'s reading, counted on
    /// `App::loop_demand`'s existing walk rather than on one of its own, and
    /// parked here for `report_frame_telemetry` to pick up. A LEVEL, not a
    /// total: it is overwritten every frame.
    loop_counts: crate::loop_telemetry::LoopState,
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
    /// Whether this install says the raster running totals out loud — see
    /// [`render::raster_telemetry_is_loud`]. Read once, at
    /// construction, because a per-frame reporter must not touch the config
    /// store.
    raster_telemetry_loud: bool,
    /// When [`App::report_raster_telemetry`] last wrote a line, or `None`
    /// before the first one. The running totals are a periodic readout and
    /// this is its clock; see that function.
    raster_telemetry_said: Option<web_time::Instant>,
    /// What each frame cost this thread — see [`crate::frame_ledger`].
    frame_ledger: crate::frame_ledger::FrameLedger,
    /// Whether this install says the frame timing lines out loud — see
    /// [`render::frame_telemetry_is_loud`]. Read once, at construction, for
    /// the same reason [`Self::raster_telemetry_loud`] is.
    frame_telemetry_loud: bool,
    /// When [`App::report_frame_telemetry`] last wrote its lines; the same
    /// periodic-readout clock shape as [`Self::raster_telemetry_said`].
    frame_telemetry_said: Option<web_time::Instant>,
    /// The `gpu passes:` sentence as last composed for the diagnostics
    /// overlay — the same line the telemetry family prints. `None` where no
    /// probe is installed; the overlay shows its own absence text there.
    gpu_passes_panel_line: Option<String>,
    /// The probe's collected-frame count [`Self::gpu_passes_panel_line`] was
    /// composed at, so the sentence is rebuilt only when a figure can have
    /// moved rather than allocated every frame.
    gpu_passes_panel_frames: Option<u64>,
    /// The scripted-input player, armed at construction by the
    /// `gesture_script` key or the `SQUALLAR_GESTURE_SCRIPT` variable — see
    /// [`render::gesture_player_from`]. `None` on every shipping install,
    /// and everything it costs is behind the `Option`.
    gesture_player: Option<squallar_egui::gesture_player::GesturePlayer>,
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
    chunk_feeds: squallar_radar::chunk_feed::ChunkFeedManager,
    /// Push notification of new chunks.
    chunk_notify: squallar_radar::chunk_notify::ChunkNotifier,
    /// `(volume, what its cuts declared, its product inventory, when it was collected)`.
    latest_cached_scans: HashMap<
        String,
        (
            Arc<nexrad_model::data::Scan>,
            Arc<squallar_radar::nyquist::DeclaredNyquist>,
            ScanInfo,
            chrono::NaiveDateTime,
        ),
    >,
    manual_nav_pending: bool,
    /// **Panes restored parked on an instant, whose data has not been asked
    /// for yet.** Drained once, on the first redraw.
    ///
    /// Persisting the clock is not persisting the picture. A pane reloaded with
    /// `as_of` set has its playhead in 2013 and its map painted with whatever
    /// the live poll just delivered, because the archive fetch is driven by a
    /// scrub and a reload is not one. Reopening parked-but-live is worse than
    /// reopening live: the transport says one thing and the map shows another.
    ///
    /// Collected at construction rather than dispatched there because
    /// `spawn_fetch` needs the built `App`.
    parked_fetch_pending: Vec<(usize, String, chrono::NaiveDateTime)>,
    /// Panes restored with a loop armed, drained by `hydrate_parked_panes`.
    /// An entry whose transport has no scan yet to anchor on is re-parked by
    /// `handle_enable_loop` and drained again on a later redraw.
    loop_arm_pending: Vec<(usize, squallar_egui::pane::LoopArm, u64)>,
    /// The map extent most recently asked for on screen.
    last_viewport: Option<squallar_geo::GeoBounds>,
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
    volume_store: std::sync::Arc<squallar_volumetric::bridge::VolumeStore>,
    #[cfg(test)]
    pub(crate) volume_extractions: std::cell::Cell<u32>,
    /// How a thread that is not this one asks for a frame.
    redraw_waker: RedrawWaker,
    location: LocationFacade,
    /// Where earlier volumes said their radars are.
    site_positions: crate::site_positions::SitePositions,
    /// The network catalogue this install last cached: which radars exist, and where the
    /// published record puts them.
    site_catalogue: squallar_radar::catalogue::SiteCatalogue,
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
    target: &squallar_egui::pane::VolumeTarget,
    site_lat: f64,
    site_lon: f64,
    cells: [u32; 3],
    max_axis: u32,
    payload: Box<dyn std::any::Any + Send>,
) -> squallar_source::volume::VolumeJobContext {
    let (centre, half_extent_km) = match target.region {
        Some(region) => {
            let extent = region.half_extent_km();
            (region.centre(), Some((extent.east_km, extent.north_km)))
        }
        None => (
            squallar_geo::GeoPoint {
                lat: site_lat,
                lon: site_lon,
            },
            None,
        ),
    };
    squallar_source::volume::VolumeJobContext {
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
    ///
    /// No wgpu instance is built here. The browser's cannot be — deciding
    /// between its two rendering APIs takes an `await`, see [`create_instance`]
    /// — and once one target has to defer it, both do: the instance is built
    /// where the surface is, in
    /// [`initialize_rendering_state`](Self::initialize_rendering_state).
    pub fn new(platform: Box<dyn PlatformBridge>, location: LocationFacade) -> Self {
        let input = InputHandler::new();
        let channels = ChannelHub::new();
        let mut device_profile = squallar_device_profile::budget::DeviceProfile::for_target();
        device_profile.memo = Some(squallar_device_profile::budget::BudgetMemo {
            loop_pool_bytes: None,
            steps_back: crate::budget_memo::remembered_steps(platform.kv().as_deref()).unwrap_or(0),
        });
        let budgets = squallar_device_profile::budget::resolve(&device_profile);
        let render = RenderDispatcher::with_budgets(&budgets);

        #[cfg(not(target_arch = "wasm32"))]
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");

        // Goes through `squallar_radar::tls` rather than `reqwest::Client::builder`
        // directly: that is what installs the rustls crypto provider (no provider is
        // compiled in) and sets `https_only`.
        let http_client = squallar_radar::tls::client(
            squallar_radar::tls::USER_AGENT,
            std::time::Duration::from_secs(30),
        )
        .build()
        .expect("Failed to build HTTP client");

        // The archive block cache learns its home the way squallar-egui
        // learns everything platform-shaped: once, at construction, never
        // through a Gui setter. Android answers `None` here and installs via
        // `Self::set_basemap_cache_dir` when `android_main` learns the path.
        if let Some(dir) = platform.basemap_cache_dir() {
            squallar_egui::tiles::install_basemap_cache_dir(dir.to_path_buf());
        }

        // The offline download store rides in the same way, but as a `Gui`
        // field rather than a process global: its one consumer is the Gui's
        // own download engine, and a per-instance copy is what keeps "built
        // without one" testable. Construction is the whole route — there is
        // deliberately no setter — so a bridge that will ever answer `Some`
        // (Android included) is populated before this line runs.
        let mut gui =
            Gui::new().with_basemap_dir(platform.basemap_dir().map(std::path::Path::to_path_buf));
        let supports_exit = platform.supports_exit();
        let loop_frame_budget = budgets.loop_frames_held;
        let location_settings_available = location.settings_available();
        let restored = platform
            .kv()
            .is_some_and(|store| gui.load_ui_config(store.as_ref()));
        let site_is_provisional = !restored && apply_location_hint(&mut gui, platform.as_ref());
        // Before `gui` moves into the struct literal below.
        let parked_fetch_pending = parked_panes(&gui);
        let loop_arm_pending = looping_panes(&gui);
        let site_positions = crate::site_positions::SitePositions::load(platform.kv().as_deref());
        let site_catalogue = crate::site_catalogue::load(platform.kv().as_deref());
        let table =
            squallar_radar::sites::resolve(site_positions.fixes().chain(site_catalogue.fixes()));
        let catalogue_pending = site_catalogue.is_empty();
        let site_hint_pending = !restored && table.rows().is_empty();
        if catalogue_pending {
            log::info!(
                "no radars are known yet; the site list holds only what this \
                 install has decoded until the catalogue fetch lands",
            );
        }

        // Read here and not at the report, because the report runs once a
        // frame and a config read is not a per-frame cost.
        let raster_telemetry_loud = render::raster_telemetry_is_loud(platform.kv().as_deref());
        let frame_telemetry_loud = render::frame_telemetry_is_loud(platform.kv().as_deref());
        let gesture_player = render::gesture_player_from(
            std::env::var("SQUALLAR_GESTURE_SCRIPT").ok(),
            platform.kv().as_deref(),
        );

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
            state: None,
            window: None,
            gui,
            supports_exit,
            loop_frame_budget,
            location_settings_available,
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            user_gps: None,
            user_heading: None,
            radar_liveness: squallar_egui::radar_layer::RadarLiveness::default(),
            liveness: Vec::new(),
            volume_painter: None,
            mirror_rungs: squallar_gpu::egui_renderer::MirrorRungs::default(),
            mirror_plan_applied: None,
            mirror_plan_stamp: 0,
            budgets,
            device_profile,
            loop_pool,
            loop_pool_state: crate::loop_pool::LoopPoolState::new(
                loop_pool,
                crate::loop_pool::LoopFrameModel::from_budgets(&budgets),
            ),
            loop_pool_sized: loop_pool_memo.is_some(),
            loop_counts: crate::loop_telemetry::LoopState::default(),
            volumes: crate::volume_inventory::VolumeInventory::default(),
            input,
            channels,
            render,
            platform,
            texture_counter: 0,
            restore_pending: false,
            cached_dark_theme: None,
            raster_telemetry_loud,
            raster_telemetry_said: None,
            frame_ledger: crate::frame_ledger::FrameLedger::default(),
            frame_telemetry_loud,
            frame_telemetry_said: None,
            gpu_passes_panel_line: None,
            gpu_passes_panel_frames: None,
            gesture_player,
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
            volume_store: std::sync::Arc::new(squallar_volumetric::bridge::VolumeStore::new()),
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
            chunk_feeds: squallar_radar::chunk_feed::ChunkFeedManager::new(),
            chunk_notify: squallar_radar::chunk_notify::ChunkNotifier::new(),
            latest_cached_scans: HashMap::new(),
            manual_nav_pending: false,
            parked_fetch_pending,
            loop_arm_pending,
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

    /// Create the instance and surface and initialize AppState for a given
    /// window and dimensions.
    ///
    /// The instance is built here rather than at construction because on the
    /// browser the choice between WebGPU and WebGL2 is an `await` — and it has
    /// to be made before this function touches the canvas, since the surface is
    /// what binds one of the two to it for good. [`create_instance`] carries
    /// the mechanism.
    async fn initialize_rendering_state(
        budgets: squallar_device_profile::budget::Budgets,
        window: &WindowRef,
        width: u32,
        height: u32,
    ) -> app_state::AppState {
        let instance = create_instance().await;
        let surface = instance
            .create_surface(window.clone())
            .expect("Failed to create surface!");

        app_state::AppState::new(&instance, &budgets, surface, window, width, height).await
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
        self.frame_ledger.mark_frame_start();
        self.input.clear_frame_state();
        self.poll_platform_state();
        self.poll_data_channels();
        self.evict_unshown_scans();
        squallar_worker::offload::drain_deferred_drops(
            squallar_device_profile::constants::DEFERRED_DROP_BUDGET_PER_FRAME,
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
            || squallar_worker::offload::has_deferred_drops()
            // An armed gesture player is a hand that never lifts: its next
            // frame's events exist only if a next frame comes.
            || self.gesture_player.is_some()
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

        // Close the frame's timing sample, bucketed by the renderer's own
        // reading of this frame's input, and say the periodic lines if due.
        let interacted = self
            .state
            .as_ref()
            .is_some_and(|state| state.egui_renderer.frame_had_interaction());
        self.frame_ledger.finalize(interacted);
        self.report_frame_telemetry();
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
    ///
    /// **An explicit choice outranks the window and the platform both.** A user
    /// who picked Light or Dark picked it for this application; letting the
    /// compositor's `ThemeChanged` overrule that would make the setting look
    /// broken every time their desktop switched. `System` is the default and
    /// keeps the behaviour this had before the setting existed.
    fn resolve_theme(&mut self) -> bool {
        // ONE EXPRESSION, NO EARLY RETURN. Every arm falls through to the
        // `adopt_theme` below, which is what keeps the resolved theme and the
        // cache the site labels re-rasterise from in step;
        // `the_desktop_theme_routes_record_what_they_read` pins that shape by
        // reading this body, and an arm that answered on its own would satisfy
        // the invariant here while quietly inviting the next one not to.
        let dark = match self.gui.theme.is_dark() {
            Some(chosen) => chosen,
            None => match self.window.as_ref().and_then(|w| w.theme()) {
                Some(theme) => matches!(theme, winit::window::Theme::Dark),
                None => match self.cached_dark_theme {
                    Some(cached) => cached,
                    None => self.platform.detect_dark_theme(),
                },
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

        let budgets = self.budgets;
        let redraw_target = self.window.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let state = Self::initialize_rendering_state(
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
    fn update_device_profile(&mut self, class: squallar_device_profile::quality::DeviceClass) {
        // Scoped, and deliberately: the adapter's report is read out here and
        // the borrow released, so the re-derived raster ceiling below can be
        // written back into the same `AppState`.
        let Some(limits) = self.state.as_ref().map(|state| state.device.limits()) else {
            return;
        };
        self.device_profile.class = class;
        self.device_profile.adapter = squallar_device_profile::budget::AdapterCeilings {
            max_texture_dimension_2d: limits.max_texture_dimension_2d,
            max_texture_dimension_3d: limits.max_texture_dimension_3d,
        };
        let resolved = squallar_device_profile::budget::resolve(&self.device_profile);
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

        // **The raster ceiling is re-derived here, not only in `AppState::new`.**
        // `AppState` computed it from the budgets this build resolved before it
        // had met an adapter — `Promotion::Floor` on every target, because
        // `DeviceProfile::for_target` carries the WebGL2 guarantee. The adapter
        // has now answered, and on a browser that is the only thing that ever
        // separates a workstation GPU from a blocklisted driver. Without this
        // the web bracket's promotion would resolve correctly and reach
        // nothing: the dispatcher would keep offering the floor for the whole
        // life of the process.
        let reported = limits.max_texture_dimension_2d;
        let promoted_side = resolved.raster_side_for_adapter(reported);
        if let Some(state) = self.state.as_mut()
            && state.raster_side_ceiling_px != promoted_side
        {
            log::info!(
                "plan views may now reach {promoted_side} px, up from {}: a {class:?} adapter \
                 reporting {reported} px 2D textures resolved to {:?}",
                state.raster_side_ceiling_px,
                resolved.promotion,
            );
            state.raster_side_ceiling_px = promoted_side;
            self.render.set_raster_side_ceiling_px(promoted_side);
        }
    }

    fn install_volume_bridge(&mut self) {
        use squallar_device_profile::quality;

        // Read before the `&mut` borrow below, because the pool it also decides lives on
        // `App` rather than on `AppState` — see `Self::loop_pool`.
        let Some(class) = self.state.as_ref().map(|state| {
            squallar_gpu::device::device_class_of(state.adapter.get_info().device_type)
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

        // The GPU pass probe, only where this install asked for the frame
        // timing lines: an install that never set the key pays zero query
        // submissions, not merely a cheap path. `install_gpu_probe` answers
        // `false` on a device without TIMESTAMP_QUERY — every WebGL2 leg —
        // and `report_frame_telemetry` prints the honest absence line there.
        if self.frame_telemetry_loud
            && state
                .egui_renderer
                .install_gpu_probe(&state.device, &state.queue)
        {
            log::info!("gpu pass probe installed: timestamp queries bracket the frame's passes");
        }

        let quality = quality::select(class, self.budgets.quality_ceiling);

        // Nothing is built on a device that cannot render a volume — the pipelines would
        // compile a shader against limits already known to be short, and
        // `create_render_pipeline` has no `Result` to notice it in.
        if squallar_volumetric::support(&state.volume_support).is_supported() {
            log::info!(
                "3D volume view: {quality:?} on {:?}",
                state.adapter.get_info().device_type
            );
            let resources = squallar_volumetric::bridge::VolumeResources::new(
                &state.device,
                state.egui_renderer.attachment_config(),
                &state.queue,
            );
            state
                .egui_renderer
                .callback_resources_mut()
                .insert(resources);
        }

        let painter = std::sync::Arc::new(squallar_volumetric::bridge::BridgeVolumePainter::new(
            self.volume_store.clone(),
            quality,
            self.budgets.offscreen_bytes,
            state.volume_support.clone(),
        ));
        self.volume_painter = Some(painter.clone());

        // The ground-tile store, on every device: one pipeline over ordinary
        // vertex and index buffers, with no limit a shipped adapter can fall
        // short of. Installed before the painter that draws through it, so no
        // frame can dispatch a ground callback into an empty slot.
        let attachments = state.egui_renderer.attachment_config();
        let ground = squallar_gpu::tile_mesh::TileMeshStore::new(
            &state.device,
            attachments,
            squallar_gpu::egui_renderer::EGUI_DITHERING,
        );
        state.egui_renderer.callback_resources_mut().insert(ground);

        // Both of the renderer's painters, published at one seam.
        for installed in [
            GuiEvent::VolumePainter(Some(painter)),
            GuiEvent::TileMeshPainter(Some(std::sync::Arc::new(
                squallar_gpu::tile_mesh::TileMeshBridge,
            ))),
        ] {
            self.gui.apply(installed);
        }
    }

    /// Dispatch the voxel build a 3D pane asked for, unless the volume is already in hand
    /// or in flight.
    fn volume_grid_axis_limit(&self) -> u32 {
        self.state.as_ref().map_or(
            squallar_device_profile::constants::WEBGL2_MAX_TEXTURE_DIMENSION_3D,
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
        layer: &squallar_source::id::LayerId,
        target: squallar_egui::pane::VolumeTarget,
    ) {
        if let Some(why) = self.volume_layer_refusal(layer, &target) {
            self.volume_store.insert_held(
                pane_idx,
                target.clone(),
                squallar_volumetric::bridge::VolumeEntry::Refused(why),
                squallar_volumetric::bridge::Hold::Single,
            );
            self.mark_volume_rendered(pane_idx, &target);
            return;
        }
        if self.prepare_volume(
            pane_idx,
            &target,
            squallar_volumetric::bridge::Hold::Single,
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
        target: &squallar_egui::pane::VolumeTarget,
        hold: squallar_volumetric::bridge::Hold,
        layer: &squallar_source::id::LayerId,
    ) -> VolumePrepare {
        use squallar_volumetric::bridge::VolumeEntry;

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
        let Some(site) = squallar_radar::sites::get_radar_site(&target.volume.site) else {
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
                    squallar_radar::fields::spec(product).name,
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
        product: squallar_radar::types::RadarProduct,
    ) -> Option<squallar_radar::render_input::RenderInput> {
        #[cfg(test)]
        self.volume_extractions
            .set(self.volume_extractions.get() + 1);
        let radar = squallar_radar::sites::get_radar_site(site)?;
        let (scan, declared) = self.loop_mgr.get_cached(site, &collected)?;
        let (scan, declared) = (Arc::clone(scan), Arc::clone(declared));
        let sweeps: Vec<&nexrad_model::data::Sweep> = scan.sweeps().iter().collect();
        squallar_radar::render_input::RenderInput::extract_volume_parts(
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
        use squallar_volumetric::bridge::VolumeEntry;

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
        product: squallar_radar::types::RadarProduct,
    ) -> Option<squallar_radar::render_input::RenderInput> {
        self.extract_site_volume(site, product, true)
    }

    /// The **base** volume's whole-volume payload for `site` and `product` — the same walk,
    /// over the base holder alone.
    fn extract_base_volume(
        &mut self,
        site: &str,
        product: squallar_radar::types::RadarProduct,
    ) -> Option<squallar_radar::render_input::RenderInput> {
        self.extract_site_volume(site, product, false)
    }

    /// The body of the two above: resolve the site's volume — with or without the live
    /// overlay merged in — and walk the product's moment out of it.
    fn extract_site_volume(
        &mut self,
        site: &str,
        product: squallar_radar::types::RadarProduct,
        merge_live: bool,
    ) -> Option<squallar_radar::render_input::RenderInput> {
        #[cfg(test)]
        self.volume_extractions
            .set(self.volume_extractions.get() + 1);
        let radar = squallar_radar::sites::get_radar_site(site)?;
        let base = self.volumes.base_for(site);
        let overlay = merge_live
            .then(|| self.chunk_feeds.snapshot(site))
            .flatten();
        let current = squallar_radar::current::resolve(
            base.as_ref()
                .map(|(scan, declared)| squallar_radar::nyquist::Volume::new(scan, declared)),
            overlay
                .as_ref()
                .map(|live| squallar_radar::nyquist::Volume::new(&live.scan, &live.declared)),
        )?;
        squallar_radar::render_input::RenderInput::extract_volume_parts(
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
    /// [`squallar_radar::sampler::ladder_fingerprint`] over the same resolve the section
    /// payload is extracted from, so the key and the cut cannot describe different volumes.
    pub(crate) fn current_ladder_fingerprint(
        &mut self,
        site: &str,
        product: squallar_radar::types::RadarProduct,
    ) -> Option<u64> {
        let base = self.volumes.base_for(site);
        let overlay = self.chunk_feeds.snapshot(site);
        squallar_radar::current::resolve(
            base.as_ref()
                .map(|(scan, declared)| squallar_radar::nyquist::Volume::new(scan, declared)),
            overlay
                .as_ref()
                .map(|live| squallar_radar::nyquist::Volume::new(&live.scan, &live.declared)),
        )?
        .ladder_fingerprint(product)
    }

    /// The stamp of `site`'s current merged volume: the newest data time (its identity,
    /// advanced by every sealed sweep) and the base volume's start where one contributes.
    fn current_volume_stamp(&mut self, site: &str) -> Option<squallar_egui::CurrentVolumeStamp> {
        let base = self.volumes.base_with_time(site);
        let overlay = self.chunk_feeds.snapshot(site);
        let current = squallar_radar::current::resolve(
            base.as_ref()
                .map(|(scan, declared, _)| squallar_radar::nyquist::Volume::new(scan, declared)),
            overlay
                .as_ref()
                .map(|live| squallar_radar::nyquist::Volume::new(&live.scan, &live.declared)),
        )?;
        let newest = current.newest_data_time()?;
        let base_started = (current.base_sweeps() > 0)
            .then(|| base.as_ref().map(|(_, _, collected)| *collected))
            .flatten();
        Some(squallar_egui::CurrentVolumeStamp {
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
                .get_mut::<squallar_volumetric::bridge::VolumeResources>()
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
    fn mark_volume_rendered(
        &mut self,
        pane_idx: usize,
        target: &squallar_egui::pane::VolumeTarget,
    ) {
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
    /// the in-flight marks, and is why neither path may call `offload_job` on
    /// its own: an unmarked dispatch is dispatched again on the next frame, and
    /// its answer is refused as stale when it lands.
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
                self.gui.finish_loading(&scan_resp.site);
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
                            self.gui.finish_loading(&site);
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
                            squallar_worker::offload::discard_each(
                                "capped-still",
                                crate::volume_inventory::volume_drop_parts(forced),
                            );
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
        use squallar_overlays::render::overlay_state::SourceEvent;
        // The two radar-shaped outcomes are collected rather than acted on
        // in place: the decode needs the whole `App`, and `gui` is bound once
        // for the whole drain. The decode is offloaded either way, so nothing
        // observable turns on where in this pass it is dispatched.
        let mut listed: Vec<render::LoopListingArrival> = Vec::new();
        let mut archives: Vec<squallar_radar::source::RadarFrameFetch> = Vec::new();
        // The layers this drain installed data for, deduplicated: two rounds of
        // the same layer in one pass are one re-ask, not two.
        let mut arrived: Vec<squallar_source::id::LayerId> = Vec::new();
        // The loop-frame fetches this drain took delivery of, collected for
        // the same reason `listed` is: `gui` is bound for the whole drain and
        // the mark lives on the dispatcher. Cleared below whether or not the
        // fetch carried a granule — see `clear_loop_frame_fetch`.
        let mut answered: Vec<(squallar_source::id::LayerId, chrono::NaiveDateTime)> = Vec::new();
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
                    let radar = scope.downcast_ref::<squallar_radar::source::RadarListing>();
                    if id == squallar_source::id::known::RADAR {
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
                    answered.push((id.clone(), stamp.valid));
                    // Radar's frames are held by the loop cache this crate
                    // owns, so its bytes are taken below and decoded through
                    // the funnel; every other layer's go to the handler.
                    if id == squallar_source::id::known::RADAR {
                        match data.downcast::<squallar_radar::source::RadarFrameFetch>() {
                            Ok(fetch) => archives.push(*fetch),
                            Err(data) => gui.deliver_frame(&id, stamp, data),
                        }
                    } else {
                        gui.deliver_frame(&id, stamp, data);
                    }
                }
            }
        }
        for (id, valid) in answered {
            self.render.clear_loop_frame_fetch(&id, valid);
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
    fn publish_base_volumes(&mut self) -> HashMap<String, squallar_egui::CurrentVolumeStamp> {
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
        let arrived: HashMap<String, squallar_egui::CurrentVolumeStamp> = stamps
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
        arrived: &HashMap<String, squallar_egui::CurrentVolumeStamp>,
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
        let entry = squallar_egui::radar_layer::liveness_entry(self.radar_liveness.clone());
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
        // Each volume crosses to the drop queue split at its sweep seam
        // (`volume_drop_parts`), so a wasm drain turn frees a sweep, never a
        // whole volume.
        let evicted_stills = self.volumes.retain_still(&wanted);
        squallar_worker::offload::discard_each(
            "evicted-scan",
            crate::volume_inventory::volume_drop_parts(evicted_stills),
        );
        let evicted_bases = self.volumes.evict_base(&unshown);
        squallar_worker::offload::discard_each(
            "evicted-base-volume",
            crate::volume_inventory::volume_drop_parts(
                evicted_bases
                    .into_iter()
                    .map(|(scan, nyquist, _)| (scan, nyquist)),
            ),
        );
        let evicted_cached = evicted(&mut self.latest_cached_scans, &unshown);
        squallar_worker::offload::discard_each(
            "evicted-cached-volume",
            crate::volume_inventory::volume_drop_parts(
                evicted_cached
                    .into_iter()
                    .map(|(scan, nyquist, _, _)| (scan, nyquist)),
            ),
        );
        self.render.retain_extracts(|key| !unshown(&key.site));
        self.evict_unneeded_loop_scans();
        squallar_radar::derive::retain_volumes(
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
        // **The visible slice, walked once rather than indexed.** `panes()`
        // yields `..min(pane_count, panes.len())` and the index loop this
        // replaced visited `0..pane_count` and found `None` past the end, so
        // the set is identical — WI-0's proof, applied again. The index was
        // never read inside the body. One reach where there were two, and the
        // walk exists to read loop state, which is what makes it WO-T3.7's to
        // shed.
        for pane in self.gui.panes() {
            if let Some(info) = pane.scan_info.as_ref() {
                needed
                    .entry(info.site.name)
                    .or_default()
                    .insert(info.timestamp);
            }
            // **Radar-addressed, and it stays that way** (WO-T3.7).
            // Everything this walk retains is radar's: `retain_plan_frames`,
            // `retain_scans`, `retain_l3` and `retain_l3_keys` are keyed by
            // NEXRAD site, and the site comes from `radar_layer::site(ls)` —
            // the geometry anchor only a radar timeline carries. A satellite or
            // model timeline answers `""` there, so a transport-addressed read
            // would file every loop's frames under the empty site and evict the
            // real one's volumes on the next sweep.
            let ls = &pane.time_state(&known::RADAR);
            if !ls.is_active() {
                continue;
            }
            if ls.listing_wait(now).is_some_and(|waited| {
                waited < squallar_device_profile::constants::LOOP_LISTING_GRACE
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
        squallar_worker::offload::discard_each(
            "evicted-loop-volume",
            crate::volume_inventory::volume_drop_parts(self.loop_mgr.retain_scans(keep)),
        );
        squallar_worker::offload::discard_each(
            "evicted-loop-object",
            self.loop_mgr.retain_l3(keep),
        );
        let keep_site = |site: &str| settling.contains(site) || needed.contains_key(site);
        squallar_worker::offload::discard_each(
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
        match store.store(squallar_egui::UI_CONFIG_KEY, &json) {
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
                squallar_radar::sites::resolve(
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
    fn upgrade_provisional_site(&mut self, fix: &squallar_location::Fix) {
        if !self.site_is_provisional {
            return;
        }
        if !fix.fix_quality.can_relocate() {
            return;
        }
        if !squallar_location::fix_is_accurate_enough_to_relocate(fix.accuracy_m) {
            log::debug!(
                "ignoring a {:.0} km fix for the opening site; the timezone \
                 guess it would replace is better than that",
                fix.accuracy_m.unwrap_or_default() / 1000.0
            );
            return;
        }
        let Some((site, dist)) =
            squallar_radar::sites::nearest_wsr88d_site(fix.point.lat, fix.point.lon)
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

    /// Override the archive block cache directory. The Android path: the
    /// platform learns its cache home only after startup, so the install
    /// that desktop and iOS perform inside `App::new` happens here instead.
    pub fn set_basemap_cache_dir(&mut self, dir: std::path::PathBuf) {
        squallar_egui::tiles::install_basemap_cache_dir(dir.clone());
        self.platform.set_basemap_cache_dir(dir);
    }

    /// Override the UI config directory and load config from it.
    pub fn set_config_dir(&mut self, dir: std::path::PathBuf) {
        self.platform.set_config_dir(dir);
        // Load config now — on Android this is called after App::new(), so the initial load
        // in new() had no config dir yet.
        if let Some(store) = self.platform.kv() {
            self.site_positions = crate::site_positions::SitePositions::load(Some(store.as_ref()));
            self.site_catalogue = crate::site_catalogue::load(Some(store.as_ref()));
            let table = squallar_radar::sites::resolve(
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
        // Winit calls `resumed` only from the loop thread, so this is where that
        // thread can be named — and naming it is what lets every other thread's
        // ask for a frame be routed off the blocking platform call. See
        // `platform::ask_for_a_frame`.
        crate::platform::record_loop_thread();

        // The bridge gets to amend the attributes because the web backend has to bind its
        // canvas here and nowhere else.
        let attributes = self
            .platform
            .window_attributes(Window::default_attributes().with_title("Squallar"));
        let window = event_loop.create_window(attributes).unwrap();

        let window = Arc::new(window);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = window.request_inner_size(PhysicalSize::new(RENDER_WIDTH, RENDER_HEIGHT));
        self.window = Some(window.clone());

        let held = Some(window.clone());
        self.redraw_waker.install(move || notify_redraw(&held));

        // Through the funnel like every other ask, so that the platform call
        // keeps exactly one spelling in this crate and a future one added by
        // hand fails the build rather than the app.
        notify_redraw(&self.window);
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
        // `squallar/src/android/entry.rs`'s `EventLoop::build`. Ending the
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
                self.hydrate_parked_panes();
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
