use egui_wgpu::wgpu;
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
// Only the default window size is used here, and only by the native arm of
// `create_window` — the web build takes its size from the canvas. A glob import
// would go unused on wasm32 and warn.
#[cfg(not(target_arch = "wasm32"))]
use crate::constants::{RENDER_HEIGHT, RENDER_WIDTH};
use crate::input::InputHandler;
use crate::location_permission::LocationGate;
use crate::loop_downloads::LoopDownloadManager;
use crate::platform::{PlatformBridge, RedrawWaker};
use crate::render_dispatch::RenderDispatcher;
use rustdar_egui::{Gui, actions::GuiAction};
use rustdar_radar::types::ScanInfo;

#[path = "app_fetch.rs"]
mod fetch;

#[path = "app_render.rs"]
mod render;

#[path = "app_chunks.rs"]
mod chunks;

/// Whether this build is the browser build. See `app_state::WEB`, which is the
/// same value for the same reason: a `cfg!` forks a function both of whose arms
/// still compile, so a host `cargo test` can call either one.
const WEB: bool = cfg!(target_arch = "wasm32");

/// Which wgpu backends this build will consider.
///
/// Native keeps reading `WGPU_BACKEND` from the environment. The browser has no
/// environment to read, and the choice there is not open: this build targets
/// WebGL2, so WebGPU has to be *excluded* rather than merely deprioritised.
/// Left in, wgpu would select it wherever it exists — which is Chrome but not
/// Firefox — and the two browsers would then run different, separately-broken
/// rendering paths off the same binary.
fn instance_descriptor() -> wgpu::InstanceDescriptor {
    backends_for(
        WEB,
        wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
    )
}

/// The backend choice itself, parameterised so both arms run from one binary.
///
/// The `const _` below asserts the GL *feature* is compiled in. It does not
/// assert that this function asks for it, and the difference is not academic:
/// delete the `backends` line and that assertion still passes, the wasm
/// `cargo check` still exits 0, and every browser silently reverts to
/// `Backends::all()` — which is the Chrome-on-WebGPU / Firefox-on-WebGL2 split
/// the doc above says this exists to prevent.
/// `the_browser_build_asks_for_webgl2_and_refuses_webgpu` asserts the ask.
///
/// `base` is a parameter and not `new_without_display_handle_from_env()` read
/// inline, for a reason measured rather than assumed: with the environment as
/// the only possible base, "the browser arm restricts something" could only be
/// asserted against whatever `WGPU_BACKEND` happened to say. A developer or CI
/// runner with `WGPU_BACKEND=gl` exported could then delete the `backends` line
/// and watch the gate stay green, because `Backends::GL` is what the default
/// already was. Taking the base in lets the test supply one that is *not* GL,
/// so the restriction is checked rather than coincided with.
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
                gate it - it pins an exact egui minor. `cargo tree -i wgpu` lists the \
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
        "no WebGL2 backend compiled in - wgpu's `webgl` feature is off. Note \
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
    /// The decoded Level II volume each pane's static render draws from, by site.
    ///
    /// # Retention
    ///
    /// One entry is a whole decoded volume — tens of megabytes — so this is held
    /// to the sites that are on screen: [`evict_unshown_scans`] runs once a frame
    /// and drops every site no pane names. Nothing else ever removes an entry, so
    /// without that pass a session's every visited radar stayed resident for the
    /// life of the process, which on a handheld is an OOM rather than a leak.
    ///
    /// A site counts as named by a pane's live `site` *or* by the site of the
    /// `scan_info` it is currently drawing, and both are needed: a switch moves
    /// `pane.site` at once, while `dispatch_pane_renders` goes on looking the
    /// volume up under `scan_info.site.name` until the new one lands. Evicting on
    /// the live site alone pulls the scan out from under a pane still rendering
    /// from it.
    ///
    /// Loop frames are not in here. They have their own cache and their own
    /// bound — see `LoopDownloadManager` and `MAX_LOOP_FRAMES`.
    ///
    /// [`evict_unshown_scans`]: Self::evict_unshown_scans
    scan_data: std::collections::HashMap<String, Arc<nexrad_model::data::Scan>>,
    /// The most recent **complete** volume for each site, with the time its
    /// first radial was collected — the base of the current merged volume
    /// ([`rustdar_radar::current::resolve`]) that sections, the 3D view and
    /// every other whole-volume reader stand on.
    ///
    /// # Why this is separate from `scan_data`
    ///
    /// `scan_data` holds whichever volume the plan view is drawing, and
    /// mid-volume that is the live snapshot: sealed sweeps only, one rung tall
    /// after every roll. The property this map holds is **completeness** — a
    /// volume that carries every cut its flight sealed, so a ladder built over
    /// it reaches the top of what was flown. Two writers satisfy it:
    ///
    /// * the archive path, whose volumes are published only once every cut is
    ///   finished — all three of its branches write here, because the two that
    ///   decline to *display* a volume still received a real, complete one;
    /// * `app_chunks`' completed branch, exactly when the closed volume is
    ///   `whole_volume_complete` — the live feed's own statement of the same
    ///   property, and what keeps the base rolling forward without another
    ///   archive download for as long as the feed runs.
    ///
    /// This used to be `archive_scans`, archive-written only, and the one
    /// thing a 3D pane could build from — a rule made when completeness seemed
    /// to require the archive's provenance. It does not: a closed chunk volume
    /// that sealed every cut is the same volume the archive will publish
    /// minutes later, and holding the 3D view to the slower copy held it a
    /// full volume behind the pane beside it. What the rule protected against
    /// — building from a *partial* volume — is still protected, by the
    /// `whole_volume_complete` gate on the live writer.
    ///
    /// This follows *arrival*, not maximum timestamp, and that is deliberate:
    /// scrubbing the plan view back through archive time carries the
    /// whole-volume panes with it. While a site is viewed live its feed's
    /// closed volumes land here too; while it is viewed historic they divert
    /// to `latest_cached_scans`, so a scrubbed base stays scrubbed.
    ///
    /// Bounded by the same `evict_unshown_scans` pass as `scan_data`, and
    /// often sharing an allocation with it: at a volume boundary both maps
    /// hold the same `Arc` until the next sweep seals.
    base_scans: HashMap<String, (Arc<nexrad_model::data::Scan>, chrono::NaiveDateTime)>,
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
    /// Per-site real-time chunk feeds. Empty until a live site starts one.
    chunk_feeds: crate::chunk_feed::ChunkFeedManager,
    /// Push notification of new chunks. Purely an early wake-up for the feeds
    /// above; see `chunk_notify`.
    chunk_notify: crate::chunk_notify::ChunkNotifier,
    // Cached latest scan per site from auto-poll while panes on that site view historic data.
    latest_cached_scans: HashMap<
        String,
        (
            Arc<nexrad_model::data::Scan>,
            ScanInfo,
            chrono::NaiveDateTime,
        ),
    >,
    // Set when a manual time navigation fetch is pending; triggers loop reinit after scan loads.
    manual_nav_pending: bool,
    /// The map extent most recently asked for on screen.
    ///
    /// Fed to `FetchConfig::viewport` so overlays that fetch per-region data
    /// can scope their requests. `None` until the first frame that draws an
    /// overlay; `metar::networks::DEFAULT_VIEWPORT` covers that window.
    last_viewport: Option<rustdar_overlays::types::GeoBounds>,
    autosave: AutosaveState,
    /// When egui next wants a frame, from a timed repaint request
    /// (`request_repaint_after` — a cursor blink, a tooltip delay). `None`
    /// while nothing is scheduled. Written by [`App::handle_redraw`] from the
    /// frame's own `repaint_delay` ([`repaint_action`]), spent in
    /// [`App::about_to_wait`], which also folds the remaining wait into the
    /// loop's control flow so a parked loop actually wakes for it. Zero-delay
    /// requests — animations — never land here: they ask for the redraw on
    /// the spot.
    egui_repaint_at: Option<web_time::Instant>,
    /// Whether the current site was guessed from the timezone rather than chosen.
    ///
    /// A guessed site is the one thing a location fix is allowed to overwrite.
    /// It is cleared the moment the guess is replaced — by a fix or by the user —
    /// so a site the user has actually settled on is never moved out from under
    /// them, however far they later travel.
    site_is_provisional: bool,
    /// The voxel grids 3D panes are holding, refcounted by the volume they were
    /// built from.
    ///
    /// Deliberately **not** in `AppState`: a surface loss destroys that struct,
    /// and rebuilding an 8 MiB grid that took 100 ms to resample — from a scan
    /// that is still in hand and has not changed — is work with nothing to show
    /// for it. What does die with the device is the *upload*, which lives in
    /// egui's callback resources instead.
    ///
    /// `Arc` because the painter handed to the `Gui` reads it during the UI
    /// pass, while this side writes it from the action handler.
    volume_store: std::sync::Arc<crate::volume::bridge::VolumeStore>,
    /// How many times [`extract_current_volume`] has run — the call-count
    /// seam for the tests that pin *when* the frame thread pays for the
    /// merged-volume walk, the same property the section path carries in its
    /// extraction closure.
    ///
    /// [`extract_current_volume`]: App::extract_current_volume
    #[cfg(test)]
    pub(crate) volume_extractions: std::cell::Cell<u32>,
    /// How a thread that is not this one asks for a frame.
    ///
    /// Filled in [`create_window`](Self::create_window) and emptied in
    /// [`suspended`](App::suspended), so it tracks [`window`](Self::window)
    /// exactly — see [`RedrawWaker`] for why a slot rather than a snapshot, and
    /// why the emptying is the load-bearing half.
    redraw_waker: RedrawWaker,
    /// The only thing in this application that can raise a location permission
    /// prompt. See [`crate::location_permission`].
    location: LocationGate,
}

/// Bookkeeping for the periodic config write.
///
/// # Why this exists
///
/// Configuration used to be written from exactly two places: [`request_exit`]
/// and [`suspended`]. Both are real save points on a desktop or a phone, and
/// neither one happens in a browser. Closing a tab, navigating away, or having
/// the tab discarded under memory pressure runs no Rust at all — so the web
/// build persisted nothing unless the user went out of their way to pick Exit
/// from the menu, and a session's site, layout and viewport died with the tab.
///
/// A `beforeunload` handler would be the obvious browser-shaped fix, and it is
/// the wrong one: it is not delivered for a discarded tab or a killed process,
/// it needs a path from a JS callback back into `App`'s state, and it is a
/// mechanism only one of the three platforms has. Writing periodically instead
/// costs one serialization every few seconds and is correct under every way a
/// session can end, including the ones that run no shutdown code — a killed
/// process, an OOM, a crash. The existing exit and suspend saves stay: they make
/// the *last* few seconds durable on the platforms that do get notice.
///
/// [`request_exit`]: App::request_exit
/// [`suspended`]: App::suspended
struct AutosaveState {
    /// When the config was last examined for changes.
    last_check: Option<web_time::Instant>,
    /// The JSON most recently written, so an unchanged config costs a
    /// serialization and a string compare rather than a storage write.
    ///
    /// Comparing serialized output is what lets this work without a dirty flag
    /// threaded through every mutation in the UI. A flag would be cheaper and
    /// would be wrong the first time someone adds a setting and forgets to set
    /// it; this cannot drift out of sync with what is actually persisted.
    last_written: Option<String>,
    /// Whether any event has arrived that could have changed the config.
    ///
    /// Deliberately coarse — it is set by *any* window event, most of which
    /// change nothing. Its only job is to distinguish "this session has seen
    /// activity" from "this tab has been sitting untouched", so that
    /// [`schedule_wakeup`] does not keep an idle app awake. A false
    /// positive costs one serialization; the string compare then finds nothing
    /// to write.
    ///
    /// [`schedule_wakeup`]: App::schedule_wakeup
    touched: bool,
}

/// How often the config is examined for changes.
///
/// The cost of a check is one `serde_json` serialization of a small struct. The
/// cost of setting this too high is losing that many seconds of work when a tab
/// is closed. Three seconds keeps a pan-and-zoom durable at human timescales
/// while staying far below the rate at which anything here is edited.
const AUTOSAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// What a frame's egui repaint request means for the loop. See
/// [`repaint_action`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepaintAction {
    /// Ask for the next frame immediately — an animation is mid-flight.
    Now,
    /// Wake and repaint after this long — a timed request (cursor blink).
    After(std::time::Duration),
    /// Nothing asked; the loop may park until something happens.
    Idle,
}

/// The ceiling past which a "timed" repaint request is read as "never":
/// egui reports `Duration::MAX` when nothing asked, and anything on the
/// scale of a minute or more is indistinguishable from idle for a loop
/// every real input wakes anyway.
const MAX_SCHEDULED_REPAINT: std::time::Duration = std::time::Duration::from_secs(60);

/// Classify a frame's `repaint_delay` (see `PreparedFrame::repaint_delay`).
///
/// The root cause of the second user test's "panel close shudders, then
/// vanishes": the app runs on `ControlFlow::Wait` and *nothing read this
/// value*, so `egui::Context::animate_bool_with_time` — which requests a
/// zero-delay repaint on every frame it interpolates — animated only on the
/// frames the user's own input events produced. A click's press, release
/// and stray move made ~3 frames; the slide then froze mid-travel until the
/// next input repainted whatever end state it had reached.
///
/// Zero means "now": `handle_redraw` requests the redraw on the spot, so an
/// animation renders at the display's own cadence until egui stops asking.
/// A finite delay schedules a wake instead — requesting an immediate frame
/// for a 500 ms cursor blink would busy-loop the app at frame rate, since
/// the blink re-requests itself on every paint. `Duration::MAX` (and
/// anything past [`MAX_SCHEDULED_REPAINT`]) is idle.
pub(crate) fn repaint_action(delay: std::time::Duration) -> RepaintAction {
    if delay == std::time::Duration::ZERO {
        RepaintAction::Now
    } else if delay <= MAX_SCHEDULED_REPAINT {
        RepaintAction::After(delay)
    } else {
        RepaintAction::Idle
    }
}

/// Half the east–west and north–south extent of the box a 3D pane resamples,
/// kilometres.
///
/// **The full 230 km surveillance range**, so a pane with no picked region
/// shows the whole scan. This began life at 80 km — resolution is bought with
/// half-width (80 km is 0.63 km per cell against 1.80 at the full range), and
/// past ~150 km the lowest tilt is already above 3 km AGL, so the outer box is
/// mostly cone the radar cannot see into. Both arguments are real and both
/// lost to what the crop looked like: echo running past 80 km — most of a
/// scan, on a squall-line day — simply vanished from the 3D picture before the
/// edge of the plan view beside it, which reads as a resample gone wrong
/// rather than as a curated default.
///
/// The resolution trade now belongs to the user: the region drag exists
/// precisely to spend the same cells over less ground, one deliberate commit
/// at a time (a rebuild is **150–200 ms** on the frame thread here, which is
/// why the box never tracks the viewport), and the pane's caption prints the
/// km-per-cell either way. The flatness objection — 460 x 460 x 18 km is a
/// 25.6:1 pancake at true proportions — is answered by the default vertical
/// exaggeration, which is stated on screen beside the true heights.
///
/// This value is `rustdar_egui::pane::DEFAULT_HALF_WIDTH_KM` read back, not a
/// second copy: the pane computes its own camera arithmetic against the box it
/// believes it has, and the two disagreeing would show up as a pan that drifts
/// against the picture. That constant is in turn the resampler's own
/// `MAX_HALF_WIDTH_KM`, so `build_voxels` honours it un-clamped.
const VOLUME_HALF_WIDTH_KM: f64 = rustdar_egui::pane::DEFAULT_HALF_WIDTH_KM;

/// What to resample for `target`, over the region it names or the default box
/// about the site.
///
/// Split out of `handle_prepare_volume` so the one decision in it that can be
/// silently wrong is testable without an `App`, a GPU or a decoded volume: which
/// ground gets sampled. Both failure modes are quiet — a region ignored resamples
/// the default box and looks like a region that was never committed, and a region
/// applied to the wrong axis resamples real ground the user did not pick — and
/// neither shows up as an error anywhere.
///
/// `site_lat`/`site_lon` are still needed when a region is present:
/// `build_voxels` reports its `x`/`y` ranges relative to the **site** whatever
/// the box is centred on.
fn voxel_request_for(
    target: &rustdar_egui::pane::VolumeTarget,
    site_lat: f64,
    site_lon: f64,
) -> rustdar_radar::voxel::VoxelRequest {
    // The picked region, or the default box about the site. Both halves come
    // straight off the target, which is what makes the grid and the pane's own
    // resolution readout describe the same box.
    let (centre, half_width_km) = match target.region {
        Some(region) => (
            (region.centre().lat, region.centre().lon),
            region.half_width_km(),
        ),
        None => ((site_lat, site_lon), VOLUME_HALF_WIDTH_KM),
    };
    rustdar_radar::voxel::VoxelRequest {
        centre,
        half_width_km,
        // The vertical extent is a separate axis from the horizontal region and
        // is deliberately not part of the pick: this decides what is sampled over
        // the ground, while the pane's exaggeration knob changes only how the
        // result is drawn. Conflating them would make a region drag silently
        // re-cut the column as well.
        base_km_msl: rustdar_radar::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: rustdar_radar::voxel::DEFAULT_TOP_KM_MSL,
        product: target.product,
        shape: rustdar_radar::voxel::default_shape(),
        // The raymarch reads indices only. The value plane is four times larger
        // and exists for a hover readout, which a 3D pane does not have yet.
        values_wanted: false,
    }
}

/// Point a fresh `Gui` at the radar nearest this device's timezone.
///
/// Returns whether a site was actually chosen. `false` means the platform had no
/// timezone or the timezone is not one we map, and the compiled-in default
/// stands — see [`crate::location_hint`].
///
/// Called only when nothing was restored from storage. That is the whole
/// precedence rule: a stored site is the user's, and this never touches it.
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

/// How coarse a fix may be and still be allowed to spend the provisional site.
///
/// **Deliberately enormous, and the number is measured rather than guessed.**
/// The instinct is to demand a tight fix here, and it is exactly backwards: the
/// thing this replaces is the IANA timezone guess, whose population-weighted
/// mean error is **605 km** and which opens 61% of sampled US metro population
/// on a radar that physically cannot see their weather. A portal IP lookup —
/// the coarsest source rustdar will ever read — measures **25 km**, and
/// displacing every sample point by that much changed the chosen site in only
/// **5.5%** of probes, by a median of 17 km. WSR-88D sites sit ~200 km apart;
/// this job simply does not need precision.
///
/// So the gate exists to reject the absurd, not to hold a standard. 150 km is
/// roughly where a fix stops beating the hint it would replace. Set it tight
/// and the single largest win in the feature is silently switched off.
const MAX_RELOCATION_ACCURACY_M: f64 = 150_000.0;

/// Whether a fix reporting this accuracy may choose the opening site.
///
/// `None` passes. Every NMEA source reports no accuracy at all — the sentences
/// carry HDOP, a dimensionless geometry factor, and no way to turn it into
/// metres — and the serial path has been trusted since before this field
/// existed. Treating absence as failure would disable the serial dongle's own
/// upgrade, which is the one source here that is *more* accurate than the
/// threshold, not less.
fn fix_is_accurate_enough_to_relocate(accuracy_m: Option<f64>) -> bool {
    // `is_none_or`, so a NaN accuracy — which no producer should emit and which
    // compares false against everything — is rejected rather than admitted.
    accuracy_m.is_none_or(|m| m <= MAX_RELOCATION_ACCURACY_M)
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
        Self::with_instance(
            egui_wgpu::wgpu::Instance::new(instance_descriptor()),
            platform,
        )
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
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");

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
        // This build's loop frame cap, so the timeline's caption states the
        // platform's real budget — the constant lives in this crate and the
        // UI crate cannot see it.
        gui.set_loop_frame_budget(crate::constants::MAX_LOOP_FRAMES);
        // Once, here, and not at the gate's cadence: whether a platform has a
        // location settings page is a property of the build. The permission it
        // sits beside changes; this does not.
        gui.set_location_settings_available(platform.location_settings_available());
        let restored = platform
            .config_store()
            .is_some_and(|store| gui.load_ui_config(store.as_ref()));
        // Android has no config dir yet at this point and loads later in
        // `set_config_dir`, so `restored` is false there even for a returning
        // user. That is handled where the real load happens, not here.
        let site_is_provisional = !restored && apply_location_hint(&mut gui, platform.as_ref());

        let mut app = Self {
            instance,
            state: None,
            window: None,
            gui,
            scan_data: std::collections::HashMap::new(),
            base_scans: HashMap::new(),
            input,
            channels,
            render,
            platform,
            texture_counter: 0,
            old_textures: Vec::new(),
            cached_dark_theme: None,
            exit_requested: false,
            // `last_written` starts empty rather than seeded from the config
            // just loaded. The two differ whenever this build added a field, and
            // that first corrective write is exactly what should happen.
            autosave: AutosaveState {
                last_check: None,
                last_written: None,
                touched: false,
            },
            egui_repaint_at: None,
            site_is_provisional,
            volume_store: std::sync::Arc::new(crate::volume::bridge::VolumeStore::new()),
            #[cfg(test)]
            volume_extractions: std::cell::Cell::new(0),
            http_client,
            #[cfg(not(target_arch = "wasm32"))]
            tokio_runtime,
            #[cfg(target_arch = "wasm32")]
            pending_state: None,
            loop_mgr: LoopDownloadManager::new(),
            chunk_feeds: crate::chunk_feed::ChunkFeedManager::new(),
            chunk_notify: crate::chunk_notify::ChunkNotifier::new(),
            latest_cached_scans: HashMap::new(),
            manual_nav_pending: false,
            last_viewport: None,
            redraw_waker: RedrawWaker::new(),
            // Inert until the first `poll_platform_state`, which is inside the
            // first frame — deliberately after `set_config_dir`, so the gate
            // finds the memo Android only learns the path to during
            // `android_main`.
            location: LocationGate::new(),
        };

        // Here, and not later, because "later" does not exist for two of the
        // bridge's producers: Android starts its theme poller from
        // `set_theme_detector`, which `android_main` calls before `run_app`, and
        // `DesktopPlatform::start_gps` needs the waker already in hand when a
        // menu toggle reaches it. Both are before any window, which is the
        // situation `RedrawWaker`'s slot exists for.
        app.platform.set_redraw_waker(app.redraw_waker.clone());
        app
    }

    /// A handle an entry point can give its own sensor threads.
    ///
    /// The bridge gets one directly (see [`PlatformBridge::set_redraw_waker`]).
    /// This is for the producers that are not the bridge's: `android_main`'s
    /// location and compass threads, and the browser's `watchPosition` watch —
    /// all three of which are started by an entry point that owns the `App` but
    /// has no window either.
    pub fn redraw_waker(&self) -> RedrawWaker {
        self.redraw_waker.clone()
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
    ///
    /// # Known gap: insets can change without a resize
    ///
    /// Switching between gesture and 3-button navigation, and the system bars
    /// showing or hiding under `Theme.DeviceDefault.NoActionBar.Fullscreen`,
    /// move the insets without changing the window size. Android reports both
    /// as `MainEvent::InsetsChanged` and winit discards it outright —
    /// `winit-0.30.13/src/platform_impl/android/mod.rs:294` logs
    /// `"TODO: handle Android InsetsChanged notification"` and forwards no
    /// event — so this function's two call sites, `resumed` and
    /// `handle_resized`, are the only signal the app has, and stale insets
    /// stand until the next resize. Re-check that line when winit is bumped; an
    /// `InsetsChanged` forwarded upstream is the fix.
    fn refresh_safe_area_insets(&mut self) {
        if let Some((top, bottom, left, right)) = self.platform.query_insets() {
            self.gui.set_safe_area_insets(top, bottom, left, right);
        }
    }

    fn handle_redraw(&mut self) {
        self.input.clear_frame_state();
        self.poll_platform_state();
        self.poll_data_channels();
        self.evict_unshown_scans();
        // Ahead of the minimized and zero-area early returns below: a window
        // that is minimized or still sizing is exactly one whose session might
        // be about to end, and skipping the save there is how the last change
        // gets lost.
        self.autosave_config(false);

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

        // Request redraw only when there is pending background work or auto-poll is active
        if self.render.any_render_in_flight()
            || self.gui.is_auto_poll_active()
            || self.gui.any_loop_active()
            || self.chunk_feeds.any_in_flight()
            // A down socket reconnects from `sync_sites`, which only runs on a
            // frame. Without this term the retry would depend on something else
            // happening to keep the loop awake, so turning auto-poll off with the
            // notifier unreachable would strand it permanently.
            || self.chunk_notify.reconnect_pending()
        {
            notify_redraw(&self.window);
        }

        // egui's own repaint request — the animation fix (see
        // `repaint_action`): an immediate ask repaints now, a timed one
        // schedules a wake `about_to_wait` spends, and idle clears any
        // stale schedule so a parked loop stays parked.
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
    ///
    /// Every source goes through here: Android's poll thread, winit's
    /// `ThemeChanged`, and the per-frame read of `window.theme()` that the
    /// desktops answer — see [`resolve_theme`](Self::resolve_theme). One
    /// funnel because the cache is not a memo: `cached_dark_theme` is what
    /// overlay rasterization reads (`RasterizeContext::is_dark`, and the
    /// `is_dark` handed to `rasterize_radar_sites`), and those run off-frame
    /// with no window to ask. A source that writes the theme somewhere else,
    /// or not at all, leaves them rasterizing light under a dark UI.
    ///
    /// Only a *change* invalidates. The site labels are raster textures baked
    /// in the theme's colours, so they are stale the moment it flips — but
    /// Android's poller re-sends its reading every two seconds whether or not
    /// it moved (see `spawn_state_poller`), so an unguarded bump would
    /// re-rasterise every label on every pane twice a second, forever.
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
    /// winit answers `window.theme()` on Windows and macOS and that answer is
    /// authoritative, so it is taken first — and *recorded*, which is the half
    /// that used to be missing. Desktop's [`PlatformBridge::poll_theme`] is
    /// hardwired `None`, so on those platforms nothing else ever writes the
    /// cache and everything reading it off-frame saw `None` forever.
    ///
    /// The bridge is asked only where winit has no answer — X11 and Android —
    /// and only once: the read is a JNI call there, and the poll thread keeps
    /// the cache current from then on.
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

    /// Poll for platform-specific theme, location, GPS fix, and compass heading
    /// changes.
    fn poll_platform_state(&mut self) {
        if let Some(new_theme) = self.platform.poll_theme()
            && self.adopt_theme(new_theme)
        {
            notify_redraw(&self.window);
        }
        // Ahead of the fix poll, and that ordering is the point: this is what
        // starts delivery in the first place, so on the frame after a grant
        // lands the fix it produces is drained in the same pass rather than the
        // next one.
        let step = self
            .location
            .step(self.platform.as_mut(), self.gui.settings_visible());
        if step.changed {
            self.gui
                .set_location_state(self.location.permission(), self.location.active());
            notify_redraw(&self.window);
        }
        // Consent for the position on screen has gone away. The serial reader
        // is deliberately exempt: a dongle the user plugged in is not covered
        // by this permission and its dot must survive a location denial.
        if step.revoked && !self.platform.gps_active() {
            self.gui.clear_gps_fix();
        }
        if let Some(fix) = self.platform.poll_gps_fix() {
            self.upgrade_provisional_site(&fix);
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
                let ctx = state.egui_renderer.context().clone();
                self.state = Some(state);
                self.restore_cached_render(&ctx);
                self.install_volume_bridge();
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
                    let ctx = state.egui_renderer.context().clone();
                    self.state = Some(state);
                    self.restore_cached_render(&ctx);
                    self.install_volume_bridge();
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

    /// Build the volume pipelines on the device that has just appeared and hand
    /// the `Gui` something that can draw a 3D pane.
    ///
    /// Called from both arms of `ensure_rendering_state`, which is every place a
    /// renderer comes into existence — first start, resume from suspend, and
    /// recovery from a lost surface. The matching teardown is
    /// `Gui::clear_graphics_state`, which drops the painter; there is no third
    /// place either half can be forgotten.
    ///
    /// The painter is installed **even when the probe said no**, because it is
    /// what tells the pane *why*. A pane with no painter falls back to a generic
    /// "unavailable", which is the one message that helps nobody.
    fn install_volume_bridge(&mut self) {
        use crate::volume::quality;

        let Some(state) = self.state.as_mut() else {
            return;
        };

        // The one production call site of `quality::select`, and the reason its
        // `Virtual`/`Unknown` arms matter: that is what a browser reports for
        // every adapter it exposes, so the web build takes them on every device.
        let quality = quality::select(
            quality::DeviceClass::from_device_type(state.adapter.get_info().device_type),
            quality::PLATFORM_CEILING,
        );

        // Nothing is built on a device that cannot render a volume — the
        // pipelines would compile a shader against limits already known to be
        // short, and `create_render_pipeline` has no `Result` to notice it in.
        if crate::volume::support(&state.volume_support).is_supported() {
            log::info!(
                "3D volume view: {quality:?} on {:?}",
                state.adapter.get_info().device_type
            );
            let resources = crate::volume::bridge::VolumeResources::new(
                &state.device,
                state.egui_renderer.attachment_config(),
                &state.queue,
            );
            state
                .egui_renderer
                .callback_resources_mut()
                .insert(resources);
        }

        self.gui.set_volume_painter(Some(std::sync::Arc::new(
            crate::volume::bridge::BridgeVolumePainter::new(
                self.volume_store.clone(),
                quality,
                state.volume_support.clone(),
            ),
        )));
    }

    /// Dispatch the voxel build a 3D pane asked for, unless the volume is
    /// already in hand or in flight.
    ///
    /// The build runs through [`crate::offload::offload_job`] — a thread
    /// natively, the render worker on wasm — never on the frame thread. It
    /// used to run here synchronously at a measured 150–200 ms per volume,
    /// which was tolerable when the source was an archive volume that changed
    /// every few minutes; on the merged substrate a rebuild lands with *every
    /// sealed sweep*, and a 150 ms hitch each 15–25 s is exactly the frame
    /// stall the user asked to be rid of.
    ///
    /// What stays on this thread is the extraction — the walk that copies the
    /// product's moment out of the merged volume into the payload — which is
    /// the same per-seal cost the section path already pays, and is logged so
    /// the claim stays measured.
    ///
    /// # The dedupe, now that the build is asynchronous
    ///
    /// `PrepareVolume` is level-triggered: the pane re-asks every frame until
    /// its `rendered_for` matches. The synchronous build's dedupe was the
    /// result existing before the next frame; the worker path's is the
    /// `VolumeEntry::Building` placeholder [`VolumeStore::begin_build`] opens
    /// at dispatch — the next frame, and any second pane, finds it through
    /// `share` and attaches instead of dispatching again. The placeholder is
    /// also what recognises a stale reply: a build superseded by a newer
    /// sealed sweep finds its entry gone and is dropped in `complete`.
    fn handle_prepare_volume(&mut self, pane_idx: usize, target: rustdar_egui::pane::VolumeTarget) {
        use crate::volume::bridge::VolumeEntry;

        // Built, building, or refused — attach and share rather than repeat.
        if self.volume_store.share(pane_idx, &target) {
            self.mark_volume_rendered(pane_idx, &target);
            return;
        }

        // The pane asks for the volume the App published; both sides compute
        // the stamp through `current::resolve` over the same holders. A
        // mismatch means a sweep sealed between publish and here — the pane
        // re-asks next frame with the new stamp, and building the superseded
        // one would be a whole resample for a picture already out of date.
        let Some(stamp) = self.current_volume_stamp(&target.volume.site) else {
            // No volume at all yet. Deliberately no entry: the pane goes on
            // asking, and the first frame after data lands builds it.
            return;
        };
        if stamp.newest != target.volume.collected {
            return;
        }
        let Some(site) = rustdar_radar::sites::get_radar_site(&target.volume.site) else {
            self.volume_store.insert(
                pane_idx,
                target.clone(),
                VolumeEntry::Refused(format!(
                    "{} is not a radar site this build knows the position of.",
                    target.volume.site,
                )),
            );
            self.mark_volume_rendered(pane_idx, &target);
            return;
        };

        // The budget gate, **before** the extraction — the same shape
        // `spawn_section_render` has built in. The extraction below is the
        // full merged-volume walk and copy, multi-ms on the frame thread, and
        // `PrepareVolume` is level-triggered: refused *after* extracting, the
        // walk repeats every frame until a slot frees — on wasm, where the
        // budget is 1, that is a per-frame multi-ms stall behind any
        // in-flight render. `spawn_voxel_build`'s own check stays as the
        // belt; this one is what decides whether the walk is paid at all.
        if !self.render.render_slot_free() {
            return;
        }

        let started = web_time::Instant::now();
        let Some(input) = self.extract_current_volume(&target.volume.site, target.product) else {
            // The merged volume carries this moment nowhere — the same answer
            // `build_voxels` would give, decided before paying for a dispatch.
            self.volume_store.insert(
                pane_idx,
                target.clone(),
                VolumeEntry::Refused(format!(
                    "This volume carries no {} to resample for 3D.\n\n({} at {} UTC)",
                    target.product.name(),
                    target.volume.site,
                    target.volume.collected,
                )),
            );
            self.mark_volume_rendered(pane_idx, &target);
            return;
        };
        log::info!(
            "3D volume view: extracted the {} payload in {} ms on the frame thread",
            target.volume.site,
            started.elapsed().as_millis(),
        );

        let request = voxel_request_for(&target, site.lat, site.lon);
        let spawned = self.render.spawn_voxel_build(
            &target,
            input,
            request,
            self.channels.voxel_sender.clone(),
            self.window.clone(),
        );
        if !spawned {
            // Budget full. Nothing dispatched and nothing marked: the
            // level-triggered pane asks again next frame.
            return;
        }
        self.volume_store.begin_build(pane_idx, &target);
        self.mark_volume_rendered(pane_idx, &target);
    }

    /// Take delivery of finished voxel builds.
    ///
    /// The result is resolved by **target**, not by pane: `complete` swaps the
    /// store's `Building` entry for the grid and every attached pane starts
    /// painting it on this very frame — the seamless half of the swap whose
    /// other half is `lookup_for_pane` keeping the old grid on screen while
    /// the build ran.
    fn poll_voxel_results(&mut self) {
        use crate::volume::bridge::VolumeEntry;

        while let Ok(vr) = self.channels.voxel_receiver.try_recv() {
            let ready_grid = vr.grid.map(|grid| std::sync::Arc::new(*grid));
            let entry = match &ready_grid {
                Some(grid) => VolumeEntry::Ready(std::sync::Arc::clone(grid)),
                // `build_voxels` has already logged which invariant it refused
                // on; the message is for the centre of the pane.
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
            // After the swap, so it counts what is now held. One grid is up to
            // 8 MiB and the bound is one per 3D pane plus one in flight, which
            // is the sort of figure that should be readable in a log — see
            // `VolumeStore::memory_bytes`.
            log::info!(
                "3D volume view: the store holds {} volume(s), {} MiB",
                self.volume_store.live_ids().len(),
                self.volume_store.memory_bytes() / (1024 * 1024),
            );
        }
    }

    /// The current merged volume's whole-volume payload for `site` and
    /// `product`, extracted on this thread.
    ///
    /// One resolver call — `current::resolve` over the base and the live
    /// snapshot — so this cannot disagree with the stamp the App publishes.
    fn extract_current_volume(
        &mut self,
        site: &str,
        product: rustdar_radar::types::RadarProduct,
    ) -> Option<rustdar_radar::render_input::RenderInput> {
        #[cfg(test)]
        self.volume_extractions
            .set(self.volume_extractions.get() + 1);
        let radar = rustdar_radar::sites::get_radar_site(site)?;
        let base = self.base_scans.get(site).map(|(scan, _)| Arc::clone(scan));
        let overlay = self.chunk_feeds.snapshot(site);
        let current = rustdar_radar::current::resolve(base.as_deref(), overlay.as_deref())?;
        rustdar_radar::render_input::RenderInput::extract_volume_parts(
            current.pattern(),
            current.sweeps(),
            product,
            radar.lat,
            radar.lon,
            // The user's storm motion vector, for the worker-side SRV
            // derivation; the extraction keeps it only on an SRV payload.
            self.render.storm_motion_override_kt(),
        )
    }

    /// The re-cut key for `site`'s current merged volume under `product` —
    /// [`rustdar_radar::sampler::ladder_fingerprint`] over the same resolve
    /// the section payload is extracted from, so the key and the cut cannot
    /// describe different volumes.
    pub(crate) fn current_ladder_fingerprint(
        &mut self,
        site: &str,
        product: rustdar_radar::types::RadarProduct,
    ) -> Option<u64> {
        let base = self.base_scans.get(site).map(|(scan, _)| Arc::clone(scan));
        let overlay = self.chunk_feeds.snapshot(site);
        rustdar_radar::current::resolve(base.as_deref(), overlay.as_deref())?
            .ladder_fingerprint(product)
    }

    /// The stamp of `site`'s current merged volume: the newest data time (its
    /// identity, advanced by every sealed sweep) and the base volume's start
    /// where one contributes.
    ///
    /// `None` while the site has no volume at all — no base and no sealed
    /// sweeps — which is the cold-start window the panes caption as
    /// downloading.
    fn current_volume_stamp(&mut self, site: &str) -> Option<rustdar_egui::CurrentVolumeStamp> {
        let base = self
            .base_scans
            .get(site)
            .map(|(scan, collected)| (Arc::clone(scan), *collected));
        let overlay = self.chunk_feeds.snapshot(site);
        let current = rustdar_radar::current::resolve(
            base.as_ref().map(|(scan, _)| scan.as_ref()),
            overlay.as_deref(),
        )?;
        let newest = current.newest_data_time()?;
        // The base is named only where it contributes: after a VCP change the
        // merge honestly drops it, and a caption naming it anyway would claim
        // tilts the ladder no longer carries.
        let base_started = (current.base_sweeps() > 0)
            .then(|| base.as_ref().map(|(_, collected)| *collected))
            .flatten();
        Some(rustdar_egui::CurrentVolumeStamp {
            newest,
            base_started,
        })
    }

    /// This pane is holding nothing, on the host **and** on the GPU.
    ///
    /// The GPU half is the part that is easy to leave out and impossible to see:
    /// a pane-sized `Rgba8Unorm` offscreen is ~3 MiB at 900², and the voxel
    /// texture behind it is up to 8 MiB. Dropping the store entry alone would
    /// leave both alive inside egui's callback resources until the renderer
    /// itself was rebuilt.
    fn handle_release_volume(&mut self, pane_idx: usize) {
        self.volume_store.release(pane_idx);
        let live = self.volume_store.live_ids();
        if let Some(state) = self.state.as_mut()
            && let Some(resources) = state
                .egui_renderer
                .callback_resources_mut()
                .get_mut::<crate::volume::bridge::VolumeResources>()
        {
            resources.release_pane(pane_idx, &live);
        }
    }

    /// Record that this pane's 3D view is now about `target`, so it stops
    /// asking. Set for a refusal as well as for a grid: a volume that cannot be
    /// resampled must not be re-attempted every frame at 100 ms a go.
    fn mark_volume_rendered(&mut self, pane_idx: usize, target: &rustdar_egui::pane::VolumeTarget) {
        if let Some(pane) = self.gui.pane_mut(pane_idx)
            && let Some(volume) = pane.volume_mut()
        {
            volume.rendered_for = Some(target.clone());
        }
    }

    /// Process all GUI actions emitted during this frame.
    fn process_gui_actions(&mut self, actions: Vec<GuiAction>) {
        use rustdar_overlays::render::overlay_state::OverlayKind;

        // Separate overlay render actions for deduplication
        let mut overlay_renders: Vec<(usize, OverlayKind, fetch::OverlayRenderRequest)> =
            Vec::new();

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
                // The unexpanded viewport, which is what a region-scoped fetch
                // wants — the renderer's overdraw margin is a rasterization
                // concern and would over-fetch if it leaked into the request.
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

        if !overlay_renders.is_empty() {
            let should_group = self.gui.is_viewport_sync() && self.gui.is_sync_layers();
            let grouped = deduplicate_overlay_renders(overlay_renders, should_group);
            for (pane_indices, kind, req) in grouped {
                if should_group {
                    log::debug!(
                        "Spawning overlay render for {:?} targeting {} panes",
                        kind,
                        pane_indices.len()
                    );
                }
                self.spawn_overlay_render(pane_indices, kind, req);
            }
        }
    }

    /// Poll all data channels for completed async results (scan, overlays).
    fn poll_data_channels(&mut self) {
        // Every queued scan result, not one per frame (with generation check).
        //
        // Responses arrive in batches — auto-poll sends one `CheckForNewScans`
        // per live site, and two quick navigations queue two — while winit
        // coalesces the redraws they each ask for into a single
        // `RedrawRequested`. Taking one per frame therefore strands the rest:
        // the end-of-frame re-arm in `handle_redraw` only fires for a render in
        // flight, auto-poll, or an active loop, so a queued response can sit
        // there until some unrelated OS event wakes the loop.
        while let Ok(scan_resp) = self.channels.scan_receiver.try_recv() {
            if self
                .render
                .is_fetch_stale(&scan_resp.site, scan_resp.generation)
            {
                log::debug!(
                    "Discarding stale scan result for {} (gen {})",
                    scan_resp.site,
                    scan_resp.generation
                );
                // Throwing the result away still ends the wait it belonged to.
                // Nothing else does: the fetch that superseded this one is
                // typically the auto-poll check, and `check_and_fetch_latest`
                // sends no response at all when there is no newer volume — so a
                // spinner left up here stays up until some later volume happens
                // to land, and a `fetching` flag left set blocks the very poll
                // that would have cleared it (`check_auto_polls` refuses to poll
                // while it is true). `SwitchRadarSite` raises a `loading_site`
                // and sets no `fetching` flag at all, so that gate does not
                // protect this path either — a switch superseded by one auto-poll
                // check is the case this was found on.
                //
                // The cost is the other order: a newer fetch that raised a
                // spinner of its own before this landed has it taken down early.
                // That is a frame or two of understatement against a wait
                // indicator nothing ever takes down, and the newer result still
                // arrives and repaints the pane. The flag is global rather than
                // per-site, which is the same coarseness `set_error` has on the
                // error arm below.
                self.gui.set_fetching(false);
                self.gui.clear_loading_site_for_site(&scan_resp.site);
            } else {
                match scan_resp.result {
                    Ok(scan_data) => {
                        let scan_info = ScanInfo::from_scan(
                            &scan_data.scan,
                            &scan_data.site,
                            scan_data.timestamp,
                        );
                        let site = scan_data.site;
                        let timestamp = scan_data.timestamp;
                        let scan_arc = Arc::new(scan_data.scan);

                        // An archive volume for a site the chunk feed has already
                        // moved past.
                        //
                        // The archive publishes a volume only once every cut is
                        // finished, so what it returns while a feed is running is
                        // by construction the volume *before* the one being
                        // assembled. Applying it walks the display backwards by a
                        // whole volume — which is what a user pressing Refresh
                        // least expects, and how this was found.
                        //
                        // The volume is still real and still complete, so it goes
                        // to the loops and the cache; only the live display is
                        // left alone. Once the feed retires `chunks_are_feeding`
                        // goes false and the archive is applied unconditionally,
                        // which is the whole point of the fallback.
                        //
                        // Two states outrank the guard, and both are the same
                        // statement: this response is not a "latest" the feed
                        // has already beaten, it is a *destination*.
                        //
                        // A pending **manual navigation** (Back, Forward, the
                        // scrubber, an adjacent-scan step — everything that
                        // sets `manual_nav_pending`) asked for an archive
                        // moment on purpose. The guard reading that answer as
                        // "stale" made the whole timeline transport inert on
                        // any chunk-fed site whose feed had not retired by the
                        // time the fetch landed — every navigation fetched,
                        // arrived, and was thrown away here, which is the M10
                        // "time controls do nothing" report. Auto-poll results
                        // are exempt from the exemption: they really are
                        // "latest" claims, whatever else is pending.
                        //
                        // And a site with **no live pane** has no live display
                        // for the guard to protect: whatever asked for this
                        // volume (the Set Time dialog parks its pane before
                        // fetching) meant to look at it. This also closes the
                        // retire race — `drive_chunk_feeds` drops a parked
                        // site's feed a frame later, and a response draining
                        // on exactly the click's frame used to catch the feed
                        // still nominally up.
                        let feed_is_ahead = self.chunks_are_feeding(&site)
                            && self.any_pane_live_for_site(&site)
                            && !(self.manual_nav_pending && !scan_resp.is_auto_poll)
                            && fetch::latest_scan_time_for_site(self.gui.panes(), &site)
                                .is_some_and(|shown| timestamp <= shown);

                        // Every path out of this arm holds a complete archive
                        // volume, including the two below that decline to put it
                        // on screen — so the 3D pane is offered it here, above
                        // the branch, rather than in the one branch that also
                        // happens to update the plan view. A site being watched
                        // live takes `feed_is_ahead` on every poll, and recording
                        // this inside the `else` would leave a 3D pane on that
                        // site waiting forever for a volume the app already had.
                        //
                        // **`scan_info.timestamp`, not `scan_data.timestamp`**,
                        // and the difference is seconds with a visible
                        // consequence. The first is when the volume's first
                        // radial was collected; the second is the archive's own
                        // key for the object. `PaneState::scan_info` carries the
                        // former, and a 3D pane compares the two to decide
                        // whether to name a second volume the app holds for the
                        // site — so recording the archive key here would make
                        // that line appear on *every* ordinary archive view,
                        // where it is not merely wrong but actively harmful: a
                        // warning that is always on is one the reader learns to
                        // ignore, and it is the same line that has to be
                        // believed in live mode.
                        //
                        // Guarded against walking *backwards* while the feed is
                        // ahead: the feed's whole closed volumes roll the base
                        // forward at volume end, up to ~7 minutes before the
                        // archive publishes the same volume, so in that window a
                        // manual Refresh returns the volume *before* the one
                        // already based — and an unconditional insert would put
                        // the older ladder back under every whole-volume
                        // consumer. Deliberately **not** a plain only-if-newer:
                        // with no feed ahead the insert stays unconditional, so a
                        // historic navigation still re-bases the substrate on
                        // the volume shown — a section pane stamps its target
                        // with the pane's own time while cutting from
                        // `base_scans`, and a base pinned newer than the display
                        // would cut newer data under the navigated caption.
                        let advances_the_base = self
                            .base_scans
                            .get(&site)
                            .is_none_or(|(_, held)| scan_info.timestamp > *held);
                        if advances_the_base || !feed_is_ahead {
                            self.base_scans
                                .insert(site.clone(), (Arc::clone(&scan_arc), scan_info.timestamp));
                        }

                        // When auto-poll delivers a new scan, check if any pane
                        // on this site is viewing live. If all panes on this site
                        // are historic, cache silently for JumpToLive.
                        let any_pane_live_for_site = scan_resp.is_auto_poll && {
                            let count = self.gui.pane_count();
                            (0..count).any(|i| {
                                self.gui
                                    .pane(i)
                                    .is_some_and(|p| p.site == site && p.viewing_live)
                            })
                        };

                        if scan_resp.is_auto_poll && !any_pane_live_for_site {
                            log::info!("Auto-poll: caching scan (historic mode) @ {}", timestamp);
                            self.append_scan_to_active_loops(
                                &site,
                                timestamp,
                                Arc::clone(&scan_arc),
                            );
                            self.latest_cached_scans
                                .insert(site, (scan_arc, scan_info, timestamp));
                        } else if feed_is_ahead {
                            log::info!(
                                "Keeping the real-time volume for {site}: the archive's \
                                 latest is {timestamp}, which is not newer"
                            );
                            self.append_scan_to_active_loops(&site, timestamp, scan_arc);
                            // The wait this fetch belonged to still has to end.
                            // A Refresh raises `fetching`, and `check_auto_polls`
                            // refuses to poll while it is set — so skipping
                            // `set_scan_info_for_site` without this leaves the
                            // spinner up and the archive poll wedged behind it.
                            self.gui.set_fetching(false);
                            self.gui.clear_loading_site_for_site(&site);
                        } else {
                            log::info!("Received scan data from background thread");
                            self.scan_data.insert(site.clone(), Arc::clone(&scan_arc));
                            self.gui.set_scan_info_for_site(&site, scan_info);
                            self.gui.clear_loading_site_for_site(&site);
                            self.render.reset_panes_for_site(&site, &self.gui);
                            self.spawn_level3_fetches(&site);

                            // Append the new scan to any active loops on this site
                            self.append_scan_to_active_loops(
                                &site,
                                timestamp,
                                Arc::clone(&scan_arc),
                            );

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

        // Real-time chunks, drained beside the scan results and for the same
        // reason: this is where a new volume becomes the one the panes draw
        // from, and it has to happen before `evict_unshown_scans` and before the
        // frame is laid out.
        self.poll_chunk_results();
        self.drive_chunk_feeds();

        // Finished voxel builds land before the stamps are published, so a
        // build and its announcement cannot straddle a frame.
        self.poll_voxel_results();

        // After both drains, so the UI is told about a volume on the frame it
        // arrived rather than the frame after. A 3D pane reads this to decide
        // which volume to ask for, so a frame's delay here is a frame's delay on
        // every build.
        self.publish_base_volumes();

        // Check for received overlay fetch results (unified channel)
        while let Ok(result) = self.channels.overlay_fetch_receiver.try_recv() {
            self.gui.overlays.apply_fetch_result(result);
        }
    }

    /// Tell the UI each site's current-volume stamp — what a whole-volume pane
    /// may build from, and how fresh it is.
    ///
    /// Stamps only — the `Scan`s themselves stay here, because `rustdar-egui`
    /// has no business holding a decoded volume and the pane only needs to
    /// *name* the one it wants. `handle_prepare_volume` resolves the same
    /// stamp back through `current_volume_stamp`, and the two agree because
    /// both are one `current::resolve` over the same holders.
    ///
    /// Covers the union of sites with a base and sites with a live feed: a
    /// site whose first volume is still filling has sealed sweeps and no base,
    /// and a historic site has a base and no feed — both are buildable and
    /// both publish.
    fn publish_base_volumes(&mut self) {
        let mut sites: Vec<String> = self.base_scans.keys().cloned().collect();
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
        self.gui.set_current_volumes(stamps);
    }

    /// Drop the decoded volumes no pane is showing.
    ///
    /// The retention rule, and why it is the *union* of two site fields rather
    /// than either one, is written down at [`scan_data`](Self::scan_data).
    ///
    /// Once a frame rather than at the inserts: there are two of those and one
    /// of them (`handle_jump_to_live`) is nowhere near this, so a sweep is the
    /// only form that cannot be half-wired. It costs a walk of a map that is
    /// never longer than the pane count plus whatever one frame's switches left
    /// behind.
    ///
    /// **The absence of a `pane_has_no_plan_view` filter here is deliberate**, and
    /// the one place in this file where adding one would be the bug. Every other
    /// all-panes loop that names that predicate is asking "should this pane be
    /// *given* a plan-view raster"; this one asks "is anyone still reading this
    /// volume", and a section or a 3D pane reads the whole volume rather than one
    /// tilt of it — so skipping it would free the very data it is sampling, on the
    /// next frame, for ever. Pinned by
    /// `a_whole_volume_pane_keeps_the_volume_it_is_sampling`.
    fn evict_unshown_scans(&mut self) {
        let mut shown: Vec<&str> = Vec::with_capacity(self.gui.pane_count() * 2);
        for idx in 0..self.gui.pane_count() {
            let Some(pane) = self.gui.pane(idx) else {
                continue;
            };
            shown.push(pane.site.as_str());
            if let Some(info) = pane.scan_info.as_ref() {
                shown.push(info.site.name);
            }
        }
        self.scan_data
            .retain(|site, _| shown.iter().any(|shown| *shown == site));
        // The same bound, for the same reason: an entry is a whole decoded
        // volume, and a session that visits ten sites would otherwise keep all
        // ten resident. `shown` already covers a pane's live site *and* the site
        // of the scan it is drawing, which is what stops a switch evicting the
        // volume a 3D pane is still building from.
        self.base_scans
            .retain(|site, _| shown.iter().any(|shown| *shown == site));
        // And the third holder of whole volumes, which used to be exempt and
        // simply grew: an entry is written for every site whose panes are all
        // historic when its feed delivers, it was removed only by
        // `handle_jump_to_live` for that one site, and nothing else ever
        // touched it — so a session that toured sites in historic mode kept
        // every one of their latest volumes for the life of the process. The
        // entry exists to serve `JumpToLive`, which is a per-pane action, so a
        // site no pane shows cannot be jumped to and holds nothing here.
        self.latest_cached_scans
            .retain(|site, _| shown.iter().any(|shown| *shown == site));
    }

    /// Persist the config if it has changed and the interval has elapsed.
    ///
    /// Cheap enough to call every frame: until [`AUTOSAVE_INTERVAL`] is up this
    /// is a subtraction, and after it a serialization and a string compare. Only
    /// a genuine change reaches the store.
    ///
    /// See [`AutosaveState`] for why the config is written on a timer rather
    /// than at shutdown.
    fn autosave_config(&mut self, force: bool) {
        let now = web_time::Instant::now();
        if !force
            && let Some(last) = self.autosave.last_check
            && now.duration_since(last) < AUTOSAVE_INTERVAL
        {
            return;
        }
        self.autosave.last_check = Some(now);
        // The check is happening now, so activity up to this point is accounted
        // for. Anything arriving after re-arms it and earns another wake-up.
        self.autosave.touched = false;

        let Some(json) = self.gui.ui_config_json() else {
            return;
        };
        if self.autosave.last_written.as_deref() == Some(json.as_str()) {
            return;
        }
        let Some(store) = self.platform.config_store() else {
            return;
        };
        match store.store(rustdar_egui::config_store::UI_CONFIG_KEY, &json) {
            Ok(()) => self.autosave.last_written = Some(json),
            // Not fatal and not retried on a shorter timer: the next tick tries
            // again anyway, and a full `localStorage` would otherwise log once
            // per frame forever.
            Err(e) => log::warn!("config autosave failed: {e}"),
        }
    }

    /// Replace a timezone-guessed site with the one nearest an actual fix.
    ///
    /// This is the silent upgrade the timezone guess exists to be replaced by:
    /// the guess resolves a *region* in time for the first paint, and a real fix
    /// — which arrives only where the user has already granted location, so no
    /// prompt is involved — resolves the actual radar a moment later.
    ///
    /// Does nothing once the site is no longer provisional, which is the
    /// precedence rule the whole feature turns on. A user who has chosen a site,
    /// or whose site came back from storage, keeps it: someone in Dallas
    /// watching a storm over Kansas must not be yanked home by a fix arriving
    /// late.
    fn upgrade_provisional_site(&mut self, fix: &rustdar_gps::GpsFix) {
        if !self.site_is_provisional {
            return;
        }
        // Not every fix is a statement about where the user is. `FixQuality::None`
        // means the receiver's fix flag is clear, so its coordinates are stale;
        // `Manual` is a position somebody typed into the dongle and `Simulation`
        // is a canned track — both live on the serial path, both perfectly
        // well-formed, and neither one a place. See `FixQuality::can_relocate`,
        // which is named for *this* question rather than for whether there are
        // coordinates in the struct.
        //
        // (The map itself never reads `fix_quality` at all — `ui_map.rs` draws
        // the dot from latitude and longitude alone — so this gate is about the
        // site choice and nothing else. An earlier version of this comment
        // claimed the opposite, and named a variant that does not exist.)
        if !fix.fix_quality.can_relocate() {
            return;
        }
        if !fix_is_accurate_enough_to_relocate(fix.accuracy_m) {
            log::debug!(
                "ignoring a {:.0} km fix for the opening site; the timezone \
                 guess it would replace is better than that",
                fix.accuracy_m.unwrap_or_default() / 1000.0
            );
            return;
        }
        let Some((site, dist)) =
            rustdar_radar::sites::nearest_wsr88d_site(fix.latitude, fix.longitude)
        else {
            return;
        };
        // Spent either way. A fix that confirms the guess must still stop the
        // site being provisional, or every later fix re-runs this.
        self.site_is_provisional = false;
        if self.gui.pane(0).is_some_and(|p| p.site == site.name) {
            return;
        }
        log::info!(
            "location fix refines the opening site to {} ({dist:.0} km)",
            site.name
        );
        // Through the same action a click on the site picker raises, not through
        // `set_initial_site`. Assigning `pane.site` is only the visible third of
        // a site change: `SwitchRadarSite` also raises `loading_site`, resets the
        // loop, clears the download manager and — the part that actually matters
        // here — spawns the fetch. Setting the field alone left the pane naming a
        // site whose volume nobody had asked for, so `scan_info` stayed `None`
        // and the map fell back to its no-data centre, which is in Kansas.
        //
        // `set_initial_site` is still right for the startup guess, where this
        // runs before the event loop and the app's own first fetch reads the
        // site it leaves behind.
        self.handle_gui_action(
            GuiAction::SwitchRadarSite {
                site: site.name.to_string(),
                pane_idx: self.gui.active_pane_idx(),
            },
            None,
        );
    }

    /// Arrange for one more look if a change might still be unsaved.
    ///
    /// The app runs on `ControlFlow::Wait` and wakes only when something
    /// happens, which leaves a gap the interval alone cannot close: the user
    /// pans the map, the pan's final frame lands less than
    /// [`AUTOSAVE_INTERVAL`] after the previous check, nothing else happens,
    /// and the app sleeps forever holding an unwritten change. Asking for a
    /// wake-up once the interval is up gives that change an iteration to be
    /// saved on — an iteration, not a frame, because the timer that grants it
    /// does not produce one (see [`about_to_wait`]).
    ///
    /// Only when something has actually happened. Scheduling unconditionally
    /// would turn an idle tab into a 0.33 Hz poll for no benefit, which is the
    /// cost `ControlFlow::Wait` was chosen to avoid in the first place.
    ///
    /// [`about_to_wait`]: App::about_to_wait
    fn schedule_wakeup(&self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(self.wakeup_control_flow());
    }

    /// The state the loop should be left in, given what the autosave still
    /// owes and when egui next wants a timed repaint — whichever comes
    /// first.
    ///
    /// Split out of [`schedule_wakeup`] so the whole decision is
    /// reachable from a test: an `ActiveEventLoop` cannot be had outside a
    /// running winit loop, so a function that takes one can only ever be read
    /// as source.
    ///
    /// [`ControlFlow::Wait`] — the loop's resting state, set once at startup by
    /// each platform's entry point — is the answer whenever nothing is owed,
    /// and returning it is load-bearing rather than tidy. `set_control_flow` is
    /// sticky, and a `WaitUntil` is compared against the clock afresh every
    /// iteration: one left behind after its deadline passes makes every
    /// subsequent iteration compute a zero timeout, wake immediately, and find
    /// the same expired deadline. Measured on X11 at winit 0.30.13, that is
    /// ~164,000 iterations per second on a full core, forever.
    fn wakeup_control_flow(&self) -> ControlFlow {
        let egui_delay = self
            .egui_repaint_at
            .map(|at| at.saturating_duration_since(web_time::Instant::now()));
        let delay = match (self.autosave_delay(), egui_delay) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        match delay {
            // `wait_duration` rather than `WaitUntil`: winit's `Instant` is
            // `std::time`'s natively and `web_time`'s on wasm, so no single
            // instant value typechecks for both targets. A duration does, and
            // the helper also degrades an overflowing deadline to a plain
            // `Wait`.
            Some(delay) => ControlFlow::wait_duration(delay),
            None => ControlFlow::Wait,
        }
    }

    /// How long the loop may sleep before the autosave next needs a look, or
    /// `None` when nothing is owed and it may sleep until something happens.
    ///
    /// A duration rather than the deadline it was computed from, for the same
    /// reason [`wakeup_control_flow`] builds its `WaitUntil` out of one: an
    /// instant here would be `std::time`'s on three platforms and `web_time`'s
    /// on the fourth, and a test naming either could only be written for half
    /// the targets it is meant to hold for.
    ///
    /// [`wakeup_control_flow`]: App::wakeup_control_flow
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
            self.exit_now(event_loop);
        } else {
            // Defer exit until the next event where event_loop is available
            self.exit_requested = true;
        }
    }

    /// Leave, now: the half of [`request_exit`](Self::request_exit) that needs
    /// an event loop.
    ///
    /// Split out so the deferred replay in `window_event` can take exactly this
    /// half and no more — the config save happened when the flag was set, and
    /// running it again on the way out would write the file twice.
    ///
    /// `process::exit` is not redundant beside `event_loop.exit()`. On Android
    /// the loop never unwinds, so nothing after `exit()` ever runs and the
    /// process stays alive; that is also the platform where the menu's Exit is
    /// the primary way out, and the menu is processed during a redraw with no
    /// event loop to hand out. So the deferred route is exactly the one that
    /// must not lose this.
    fn exit_now(&self, event_loop: &ActiveEventLoop) {
        log::info!("Exiting application");
        event_loop.exit();
        if self.platform.needs_process_exit() {
            std::process::exit(0);
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
            if self.gui.load_ui_config(store.as_ref()) {
                // A returning user on Android reaches the timezone guess before
                // their stored site is readable, so the guess has to be undone
                // here rather than merely not applied.
                self.site_is_provisional = false;
            } else if !self.site_is_provisional {
                // Still a first run, and `App::new` had no bridge answer to work
                // with. This is the first chance to place them.
                self.site_is_provisional =
                    apply_location_hint(&mut self.gui, self.platform.as_ref());
            }
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
    pub fn set_gps_fix_receiver(
        &mut self,
        receiver: std::sync::mpsc::Receiver<rustdar_gps::GpsFix>,
    ) {
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

    /// Install the four location calls (Android only; see
    /// [`PlatformBridge::set_location_hooks`]).
    ///
    /// The entry point installs these, not `App`, for the reason every other
    /// setter here exists: they are JNI calls that live in `rustdar-android`,
    /// which depends on this crate and can never be called from it. Handing
    /// them over before `run_app` is what closes the window in which
    /// `AndroidPlatform` answers `Unavailable` for want of them — a terminal
    /// state the gate would stop polling out of.
    pub fn set_location_hooks(&mut self, hooks: crate::platform::LocationHooks) {
        self.platform.set_location_hooks(hooks);
    }

    /// Set a callback that takes a back press delivered outside the input
    /// queue (Android's `OnBackInvokedDispatcher`; see
    /// [`PlatformBridge::poll_back_press`]).
    pub fn set_back_press_taker(&mut self, taker: fn() -> bool) {
        self.platform.set_back_press_taker(taker);
    }

    /// Whether egui is going to want this key press for itself.
    ///
    /// `egui_wants_keyboard_input` is true whenever *any* widget holds focus,
    /// not only a text field, and that is the right question: Escape is how egui
    /// surrenders focus, whatever kind of widget has it. Read off the context
    /// the last frame left, which is the answer egui will give for this press
    /// too — focus moves only inside a pass, and no pass has run since.
    ///
    /// `false` with no renderer yet. Nothing can be focused before the first
    /// frame, so a press then is the app's to spend.
    ///
    /// Only the raw-key route asks. `about_to_wait` collects a press Android's
    /// `OnBackInvokedDispatcher` delivered, and nothing in egui is competing for
    /// that one — it never entered the keyboard queue, and on Android it is the
    /// route back actually arrives by.
    fn ui_is_taking_keys(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.egui_renderer.context().egui_wants_keyboard_input())
    }

    fn handle_input_events(&mut self, event_loop: &ActiveEventLoop) {
        // Both keys mean the same thing — back out of the thing I am in — so
        // both take the same route. They used to differ only in that back gave
        // the platform first refusal, and on Android a handler is always
        // installed, so back never reached any of the decisions below it.
        // Taken, not read: this runs on every keyboard press, not once a frame.
        //
        // Taken *before* the focus test, and deliberately: a press left latched
        // because the UI wanted it is spent by the next key of any kind, which
        // is the same double dismissal one keystroke later. `InputHandler` reads
        // the raw `WindowEvent` and is never told what egui consumed, so this is
        // the only place the two can be reconciled — without it, Escape in a
        // text field unfocuses the field *and* closes the layer behind it.
        if self.input.take_back_out_press() && !self.ui_is_taking_keys() {
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

        // Every sensor thread's waker is a clone of one slot, and this is where
        // the slot learns what a wake means. Written beside the assignment above
        // because the two must not drift: a producer holding a waker that points
        // at a *previous* window would be asking a destroyed surface for frames.
        let held = Some(window.clone());
        self.redraw_waker.install(move || notify_redraw(&held));

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
    overlay_renders: Vec<(
        usize,
        rustdar_overlays::render::overlay_state::OverlayKind,
        fetch::OverlayRenderRequest,
    )>,
    should_group: bool,
) -> Vec<(
    Vec<usize>,
    rustdar_overlays::render::overlay_state::OverlayKind,
    fetch::OverlayRenderRequest,
)> {
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
        let key = (
            kind,
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
                kind,
                req,
                pane_indices: vec![pane_idx],
            });
    }

    grouped
        .into_values()
        .map(|g| (g.pane_indices, g.kind, g.req))
        .collect()
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        log::info!("App resumed");
        self.create_window(event_loop);

        // Query system bar insets now that the window is ready. Not the only
        // query — see `handle_resized`, which catches the orientation changes
        // that never come back through here.
        self.refresh_safe_area_insets();

        // A location permission can be changed in system settings while the app
        // is in the background, and in a settled state the gate has stopped
        // polling for it entirely — so this is the one moment a revocation made
        // outside the app is noticed at all.
        self.location.resumed();
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
        // A due timed repaint (`request_repaint_after` — see
        // `repaint_action`) is spent as a redraw request: this is the one
        // callback the wake-up timer reaches, and the frame itself must go
        // through `RedrawRequested` like every other frame.
        if self
            .egui_repaint_at
            .is_some_and(|at| web_time::Instant::now() >= at)
        {
            self.egui_repaint_at = None;
            notify_redraw(&self.window);
        }
        // The save the wake-up below is scheduled for, spent here rather than on
        // a frame. A `WaitUntil` deadline expiring dispatches `new_events` and
        // then this — and nothing else. It never delivers `RedrawRequested`, so
        // `handle_redraw`, the app's only other route into `autosave_config`, is
        // unreachable from the one timer that exists to reach it.
        //
        // Directly rather than by asking for a redraw: the check is a
        // subtraction until the interval is up and a serialization plus a string
        // compare after it, against a whole frame — egui pass, texture sampling,
        // present — to write a few hundred bytes of JSON on a timer whose entire
        // premise is that the app is otherwise asleep.
        self.autosave_config(false);
        self.schedule_wakeup(event_loop);
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        log::info!("App suspended - clearing graphics state");
        // Save config on suspend — on Android this is the only reliable save
        // point before the system may kill the process.
        if let Some(store) = self.platform.config_store() {
            self.gui.save_ui_config(store.as_ref());
        }
        self.old_textures.clear();
        self.render.clear_last_rendered();
        self.texture_counter = 0;
        self.gui.clear_graphics_state(); // Keep cached_render intact so we can re-upload the texture
        // immediately on resume without re-rendering.        // Clear both window and state so resumed() creates fresh ones.
        // Leaving state alive would keep a wgpu surface referencing the destroyed window.
        self.window = None;
        self.state = None;
        // The third holder of that window, and the only one this thread does
        // not own outright: five sensor threads have a clone of the waker. A
        // slot left filled would leave every one of them holding an
        // `Arc<Window>` whose `ANativeWindow` is gone — surviving a suspend is
        // the bug, not the virtue. `resumed` refills it through
        // `create_window`; until then a wake is a no-op, which is the right
        // answer for an app with nothing on screen.
        self.redraw_waker.detach();
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
                // Spend a deferred exit (set during redraw, where there was no
                // event loop to hand out) through the same door an immediate one
                // uses — `process::exit` included. Taken rather than read: the
                // config save already ran when the flag was set.
                if std::mem::take(&mut self.exit_requested) {
                    self.exit_now(event_loop);
                }
            }
            WindowEvent::Resized(new_size) => {
                self.handle_resized(new_size.width, new_size.height);
                notify_redraw(&self.window);
            }
            WindowEvent::ThemeChanged(theme) => {
                // winit hands the new theme over, so take it rather than
                // clearing the cache and hoping something re-detects: on the
                // desktops the bridge's `poll_theme` never answers, so an
                // emptied cache is one that stays empty for every off-frame
                // reader — which is what overlay rasterization is.
                if self.adopt_theme(matches!(theme, winit::window::Theme::Dark)) {
                    notify_redraw(&self.window);
                }
            }
            _ => {
                // Everything the user does to change the config — clicking,
                // dragging, typing, panning — arrives here, so this is where the
                // autosave learns it has something to look at. Deliberately not
                // set for the named arms above: a redraw is the frame the
                // autosave wake-up itself asks for, and re-arming from it would
                // never let an idle app sleep.
                self.autosave.touched = true;
                // For other events, request redraw only if egui needs it
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
mod tests;
