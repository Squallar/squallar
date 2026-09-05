use super::frame_pump::PumpPhase;
use crate::loop_pool::{
    GRID_BYTES, LoopAllocation, LoopDemand, LoopFrameModel, LoopKey, LoopKind, LoopNeed, LoopPool,
    loop_ceiling_frames,
};
use crate::render_dispatch::CachedPaneRender;
use egui_wgpu::wgpu;
use squallar_device_profile::constants::{
    DEFAULT_LOOP_SPEED_FPS, ECONOMY_FRACTION, LOOP_POOL_DWELL_FRAMES,
    MAX_LOOP_SECTION_CUTS_PER_FRAME, MAX_LOOP_SPEED_FPS, MAX_LOOP_VOLUME_BUILDS_PER_FRAME,
    MAX_OVERLAY_LOOP_RENDERS_PER_PASS, MIN_LOOP_SPEED_FPS,
};
use squallar_device_profile::fit::{economy_allowance, fit, fit_holds, floor_need, need};
use squallar_device_profile::scene::{OverlayGridNeed, Scene};
use squallar_egui::actions::GuiAction;
use squallar_egui::pane::{BroadcastSweep, ELEVATION_TOLERANCE, RenderTarget};
use squallar_egui::radar_layer;
use squallar_radar::loop_downloads::{
    FramePlan, L3FrameState, PendingDownloads, PendingL3Pairings,
};
use squallar_source::id::known;
// Test-only since WO-M12d: production loop dispatch holds a frame's payload
// only as radar's own described job. What still names the arms is the suites'
// own inspection of `frame_data` below.
#[cfg(test)]
use squallar_radar::loop_downloads::LoopFrameData;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set once `fit` has handed back an answer that does not hold
/// ([`App::fit_scene`]); never cleared, because the arithmetic that broke does
/// not heal within a process, and read where the pool is sized so the loops
/// are held at the floor from then on. Process-global rather than a field:
/// it is a defect flag, and a second `App` in the same process has the same
/// arithmetic.
static FIT_INVARIANT_BROKEN: AtomicBool = AtomicBool::new(false);

// **Why every loop read in this file is `time_state(&known::RADAR)`, spelled
// out rather than hidden behind an accessor (WO-T3.7).**
//
// The render funnel below is radar's own vocabulary from end to end, and the
// reads are radar-addressed because their payloads are:
//
// * a plan-view render is identified by `RenderTarget` — site, product and
//   elevation — and `retarget_renders` drops every frame texture the moment
//   that triple moves. No non-radar layer has a `RenderTarget`;
// * `LoopFrameImage::view()` answers `None` for the `Overlay` arm precisely so
//   a radar render cannot land in an overlay frame, and the dedup, the donor
//   search and the dispatch stamp all key off `ls.rendered_for`;
// * the section and volume arms key off `SectionLoopKey` / `VolumeLoopKey`,
//   both of which are cut out of a decoded NEXRAD volume;
// * `radar_layer::coords(ls)` reads the geometry anchor only a radar timeline
//   carries — a satellite or model timeline answers the origin.
//
// A transport-addressed read here would offer radar's texture to whatever layer
// happened to own the controls. The overlay loop's own dispatch is
// `dispatch_overlay_loop_renders`, which is layer-addressed, and the two are
// deliberately separate funnels. The residency and settle forks are the third
// case: they exclude radar because radar's storage lives above the handler
// until WO-M12d lands.

/// What a speculative dispatch needs from the delivered result's own pane
/// — copied OUT of `poll_render_results`' one origin-pane read,
/// because the borrow cannot span the apply calls and the hook must not
/// re-read state its caller already read.
struct SpeculationInputs {
    volume_start: chrono::NaiveDateTime,
    lat: f64,
    lon: f64,
    /// The name the volume is stored under — `ScanInfo::site` is the table's
    /// row, so the name is the table's own `&'static str`.
    scan_site: &'static str,
    /// The pane's tilt ladder for the delivered product, if it has one.
    ladder: Option<Vec<f32>>,
}

/// What the swapchain had for us this frame.
pub(crate) enum SurfaceStatus {
    /// A texture to draw into.
    Ready(wgpu::SurfaceTexture),
    /// Nothing available right now; skip presenting but keep the state.
    Skip,
    /// The surface is gone and the whole rendering state must be rebuilt.
    Lost,
}

/// Finish this frame's egui pass, then ask the swapchain for somewhere to draw.
pub(crate) fn finish_then_acquire<P>(
    finish_pass: impl FnOnce() -> P,
    acquire: impl FnOnce(&P) -> SurfaceStatus,
) -> (P, SurfaceStatus) {
    let prepared = finish_pass();
    // `acquire` cannot be hoisted above this line: it needs `prepared`.
    let status = acquire(&prepared);
    (prepared, status)
}

/// How long one loop frame is held on screen, for a stored playback speed.
fn loop_interval(fps: f32) -> std::time::Duration {
    let fps = if fps.is_finite() {
        fps.clamp(MIN_LOOP_SPEED_FPS, MAX_LOOP_SPEED_FPS)
    } else {
        DEFAULT_LOOP_SPEED_FPS
    };
    std::time::Duration::from_secs_f32(1.0 / fps)
}

/// **Where a starting loop parks its clock**, or `None` for "the newest frame
/// there is" — see `App::sync_loop_playback_start`, its one caller.
///
/// # Why the answer is not always the last frame
///
/// [`squallar_source::time::TimeAxis::FrameSeries`] under
/// [`squallar_egui::pane::TimeMode::Live`] is `frames.len() - 1`. For radar,
/// and for every other rail whose stamps are all history, that frame is *now*
/// and starting there is exactly right. For a rail that declares
/// `extends_future` the same frame is its **horizon** — the HRRR CONUS one is
/// 48 h out — so `Live` starts the loop on a picture of the day after tomorrow
/// and leaves the pane's clock there.
///
/// So a forward-reaching rail parks on the frame `FrameSeries` names at the
/// wall clock: the latest one valid at or before `now`, read through
/// [`squallar_egui::pane::LayerTimeState::qualifying_frame_at`] so this cannot
/// become a second spelling of that contract. A list lying entirely ahead of
/// the wall clock — a run whose analysis hour has already been evicted —
/// qualifies nothing, and then the earliest frame held is the nearest thing to
/// now there is.
///
/// # Radar keeps `TimeMode::Live`, and that is not the same as `len() - 1`
///
/// The tempting simplification is to park every loop on an index and drop the
/// `Live` arm. It is wrong: `Live` is a **pane clock mode**, not a frame
/// number, and three things read the mode rather than the playhead —
/// `as_of_term` (`ui_map_pane.rs`) has a `Live` fast path returning `0`, so an
/// `AsOf` clock mints a fresh raster token for every `EventLifetime` layer on
/// the pane; `TimeMode::as_of` answers `None` under `Live`, which is what the
/// per-layer `as_of` fallback keys on; and `settle_playheads` under `Live`
/// puts **each** layer on its own newest frame, where `AsOf(t)` puts every
/// layer on the latest frame at or before one layer's `t`. Parking radar on
/// its last frame would change all three.
///
/// # The declaration is read off the registry, never off the id
///
/// Whether stamps later than the wall clock are expected is the layer's own
/// answer, the same way `arm_layer_loop` asks it. A `match` on the layer
/// id here would be a second authority to keep in step.
fn loop_start_frame(
    pane: &squallar_egui::pane::PaneState,
    overlays: &squallar_overlays::render::overlay_state::OverlayRegistry,
    now: chrono::NaiveDateTime,
) -> Option<usize> {
    let extends_future = overlays
        .handler_by_id(pane.transport_layer())
        .is_some_and(|handler| {
            matches!(
                handler.time_axis(),
                squallar_source::time::TimeAxis::FrameSeries {
                    extends_future: true,
                    ..
                }
            )
        });
    if !extends_future {
        return None;
    }
    let ls = pane.transport_state();
    Some(
        ls.qualifying_frame_at(squallar_egui::pane::TimeMode::AsOf(now))
            .unwrap_or(0),
    )
}

/// The plan-view rasters one pass has already put on the GPU, so the second
/// pane showing one of them is handed the *texture* rather than a second copy
/// of the picture.
#[derive(Default)]
pub(super) struct PlanViewUploads {
    uploaded: Vec<(Arc<egui::ColorImage>, egui::TextureHandle)>,
}

impl PlanViewUploads {
    /// The texture holding `image`, running `upload` only if this pass has not
    /// uploaded that exact buffer already.
    fn handle(
        &mut self,
        image: &Arc<egui::ColorImage>,
        upload: impl FnOnce() -> egui::TextureHandle,
    ) -> egui::TextureHandle {
        if let Some((_, texture)) = self
            .uploaded
            .iter()
            .find(|(seen, _)| Arc::ptr_eq(seen, image))
        {
            return texture.clone();
        }
        let texture = upload();
        self.uploaded.push((Arc::clone(image), texture.clone()));
        texture
    }
}

/// How often the two running-total lines are said, at most.
///
/// **Chosen against the instrument that has to hear them, not against taste.**
/// `.github/browser-rig/drive.py`'s `wait_overlay_rasters` polls the console
/// ring every 2 s for up to `--expect-timeout` (180 s in `run_tier2.sh`) and
/// takes the newest reading, so anything well under that window costs the rig
/// nothing. Two seconds also keeps a native log to 30 readings a minute —
/// a reading being the pair — where a per-frame report writes 7200.
const RASTER_TELEMETRY_PERIOD: std::time::Duration = std::time::Duration::from_secs(2);

/// The config key an install sets to hear the running totals at `info`.
///
/// On the browser this is the `localStorage` entry `squallar.raster_telemetry`
/// — see `squallar_web::kv::storage_key` for the prefix — which is how the
/// Tier-2 rig turns them on: it seeds the key beside the layer seed in
/// `run_tier2.sh`, and `the_rig_seeds_the_key_that_makes_the_lines_loud` is
/// what stops the two drifting apart.
pub(crate) const RASTER_TELEMETRY_KEY: &str = "raster_telemetry";

/// Whether this install says the raster running totals at `info` rather than
/// `debug`.
///
/// Anything other than a stored `"1"` — including no store at all — is `false`.
/// A truthier parse would make the absent case and the "someone typed
/// something" case behave differently for no benefit; there is one value that
/// turns it on.
pub(crate) fn raster_telemetry_is_loud(store: Option<&dyn squallar_kv::KvStore>) -> bool {
    store.is_some_and(|kv| kv.load(RASTER_TELEMETRY_KEY).as_deref() == Some("1"))
}

/// The config key an install sets to hear the frame timing lines at `info`.
///
/// A separate switch from [`RASTER_TELEMETRY_KEY`], same mechanism: the two
/// instruments answer different questions and a rig leg seeds only the ones
/// it reads. On the browser this is the `localStorage` entry
/// `squallar.frame_telemetry` — `squallar_web::kv::storage_key` is the
/// prefix.
pub(crate) const FRAME_TELEMETRY_KEY: &str = "frame_telemetry";

/// Whether this install says the frame timing lines at `info` rather than
/// `debug`. Same contract as [`raster_telemetry_is_loud`]: only a stored
/// `"1"` turns it on.
pub(crate) fn frame_telemetry_is_loud(store: Option<&dyn squallar_kv::KvStore>) -> bool {
    store.is_some_and(|kv| kv.load(FRAME_TELEMETRY_KEY).as_deref() == Some("1"))
}

/// The config key that arms the gesture player: a script name from
/// `squallar_egui::gesture_player`. On the browser this is the `localStorage`
/// entry `squallar.gesture_script` — `squallar_web::kv::storage_key` is the
/// prefix — so the rig can seed it beside `frame_telemetry`.
pub(crate) const GESTURE_SCRIPT_KEY: &str = "gesture_script";

/// Arm the gesture player, or `None` — the shipping state. `env` is the
/// `SQUALLAR_GESTURE_SCRIPT` variable's value, taken as a parameter so this
/// stays a pure function of its inputs; it outranks the stored key, and the
/// read compiles identically on every target (the variable simply never
/// exists on the web).
pub(crate) fn gesture_player_from(
    env: Option<String>,
    store: Option<&dyn squallar_kv::KvStore>,
) -> Option<squallar_egui::gesture_player::GesturePlayer> {
    let name = env.or_else(|| store.and_then(|kv| kv.load(GESTURE_SCRIPT_KEY)))?;
    squallar_egui::gesture_player::GesturePlayer::from_name(&name)
}

/// Whether a reading is due, given when the last one was said.
///
/// A separate function because the alternative is a test that waits on a wall
/// clock: this takes both instants and is asked, not timed.
fn telemetry_is_due(said: Option<web_time::Instant>, now: web_time::Instant) -> bool {
    said.is_none_or(|last| now.duration_since(last) >= RASTER_TELEMETRY_PERIOD)
}

/// Write one telemetry line at the level this install asked for.
/// What the heap itself says about host spare, beside the model's own figure.
///
/// `wall` is `(declared maximum, current byteLength)` for an instance that has
/// one — a browser page — and `None` for a native process, which has no
/// declared wall and no linear memory to read. That is the arm selector, and
/// it is a runtime reading rather than a `cfg`: the same binary answers both
/// ways depending on what its bridge reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostSpareInputs {
    /// `(page_max_bytes, page_bytes)`, where a wall was declared.
    pub wall: Option<(u64, u64)>,
    /// What this instance's allocator has handed out and not been handed back
    /// (`squallar_alloc::live_bytes`); `None` where nothing installed the
    /// counter, which is every binary but the two that declare it.
    pub live_bytes: Option<u64>,
    /// The headroom the page-heap watermark reserves under the wall, which
    /// sets where the act line falls.
    pub headroom_bytes: u64,
}

/// **Host spare: the model's figure, bounded by what the heap actually says.**
///
/// `need()` has no term for loop scans, the extract cache, egui's own buffers
/// or the deferred-drop queue, so `allowance - need` can read hundreds of MiB
/// of model spare on a heap that is nearly full — which is exactly the reading
/// that would admit one more layer into a trap. **The heap measurement bounds
/// the model, never the other way round**, and the one figure that bounds it
/// and can also FALL is what this instance's allocator is holding.
///
/// Two arms, by whether the instance has a declared wall:
///
/// * **A walled instance** (a browser page) is bounded by `max - byteLength`,
///   which cannot fall, and by `act_line - live_bytes`, which can. The act
///   line and not the wall, because the watermark acts below the wall: spare
///   that reaches past it is spare the governor takes back on the next
///   reading.
/// * **An unwalled one** (native) is bounded by `allowance - live_bytes`.
///   There is no second reading to take, and the allowance is the pool.
///
/// The result is the minimum over every bound that is present, so an absent
/// one can only leave the answer where it was — never widen it. With no heap
/// reading and no counter at all the model's own figure stands, which is what
/// every build did before the counter existed.
pub(crate) fn host_spare_bytes(model_spare: u64, allowance: u64, heap: HostSpareInputs) -> u64 {
    let wall_room = heap.wall.map(|(max, used)| max.saturating_sub(used));
    let act_room = heap.wall.zip(heap.live_bytes).map(|((max, _), live)| {
        squallar_device_profile::linear_memory::act_line(max, heap.headroom_bytes)
            .saturating_sub(live)
    });
    // Native's own live bound, and only where there is no wall, so a page is
    // never bounded twice by the same allocator figure.
    let allowance_room = heap
        .live_bytes
        .filter(|_| heap.wall.is_none())
        .map(|live| allowance.saturating_sub(live));
    [Some(model_spare), wall_room, act_room, allowance_room]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(model_spare)
}

fn say_telemetry(loud: bool, line: &str) {
    if loud {
        log::info!("{line}");
    } else {
        log::debug!("{line}");
    }
}

/// The `overlay rasters:` running-total line.
///
/// **A free function, and built as a `String`, so that a test can read it.**
/// The Tier-2 rig scrapes this sentence out of the browser's console with a
/// regex, and the two are in different languages in different directories: an
/// extra space here is not a compile error, is not a test failure anywhere
/// else, and turns the rig's whole overlay reading into `null` — which reads
/// as "the path never ran". `the_rig_reads_the_lines_the_app_actually_writes`
/// is what stops that, and it can only exist because this is a value rather
/// than an argument to `log::info!`.
///
/// Never called on a tick that moved nothing since the last one; see
/// [`super::App::report_raster_telemetry`].
fn overlay_raster_line(t: &squallar_egui::overlay_cache::ledger::Totals) -> String {
    format!(
        "overlay rasters: {} dispatched, {} arrived, {} pictures of {} B, \
         {} inked, {} shown, {} promoted, {} dropped, {} superseded, \
         {} cancelled",
        t.dispatched,
        t.arrived,
        t.pictures,
        t.picture_bytes,
        t.inked,
        t.shown,
        t.promoted,
        t.dropped,
        t.superseded,
        t.cancelled,
    )
}

/// The `texture uploads:` running-total line. See [`overlay_raster_line`] for
/// why this is a value.
///
/// Two of the figures overlap by design and are never added: `whole` is a
/// routing subset of `blocking` (a whole delta goes through
/// `Renderer::update_texture`, which is a blocking `write_texture` on the
/// frame's own queue). The GPU total is `staged + blocking` — those two are
/// the disjoint pair. `blocking` is classified by the path the bytes took,
/// never by the 8 MiB band straddle; see `UploadTotals::blocking_bytes`.
fn texture_upload_line(u: &squallar_gpu::egui_renderer::texture_upload::UploadTotals) -> String {
    format!(
        "texture uploads: {} deltas, {} B to the GPU, {} B whole, \
         {} bands, {} B staged, {} B blocking",
        u.deltas,
        u.bytes(),
        u.whole_bytes,
        u.bands,
        u.staged_bytes,
        u.blocking_bytes,
    )
}

/// The `floor strips:` running-total line. See [`overlay_raster_line`] for why
/// this is a value. Two denominators, never added:
/// [`squallar_egui::floor_ledger`]'s strip paints are per pane per painted
/// frame, its mirror renders per mirror pass encoded. Orbit frames over a
/// resolved floor move neither — which is the reading that proves the strip
/// cache is skipping.
///
/// The last three say **why** a paint happened, and they overlap both each
/// other and the first figure — see the ledger's module doc. `key moves` is
/// the repaint rate the content asked for, and a `paints` figure far above it
/// is a floor repainting for a reason that is not its content.
fn floor_strip_line(t: &squallar_egui::floor_ledger::Totals) -> String {
    format!(
        "floor strips: {} paints, {} mirror renders, {} key moves, \
         {} on a stable key, {} incomplete",
        t.strip_paints, t.mirror_renders, t.key_moves, t.paints_on_stable_key, t.incomplete_paints,
    )
}

/// The `ground tiles:` running-total line. See [`overlay_raster_line`] for why
/// this is a value.
///
/// **Six denominators, none of them added.** `placed` counts fill vertices the
/// frame thread copied and `stroke pts` counts stroke points it copied — two
/// halves of one tile's ground phase, reported apart because they reach the
/// GPU through different vertex formats and a display change can put the
/// second back on the CPU while the first stays off it. `labels` is the
/// anchors the pass deferred, and is what makes a zero in either of the first
/// two readable: all three zero is a tile pass that never ran. `draws` and
/// `stroke draws` are paint callbacks pushed, per run per tile per frame, and
/// are the floors under those two zeros — a stroke run covers a span of
/// consecutive paths, so its count is far below the shape count and the two
/// draw figures are not comparable to each other either. `uploads` counts
/// buffer writes, once per tile lifetime rather than once per frame, and
/// `resident` is a **level** — the only figure here that goes down.
///
/// `stroke draws` is **appended** rather than placed beside `draws` because
/// the browser rig parses this line by an unanchored regex
/// (`.github/browser-rig/drive.py`); a field added at the end leaves every
/// existing capture where it was.
fn ground_tile_line(t: &squallar_egui::tile_mesh::ledger::Totals) -> String {
    format!(
        "ground tiles: {} placed, {} stroke pts, {} labels, {} draws, \
         {} uploads of {} B, {} evicted, {} B resident, {} unrendered, \
         {} stroke draws",
        t.mesh_vertices_placed,
        t.path_points_placed,
        t.label_anchors_placed,
        t.mesh_draws,
        t.mesh_uploads,
        t.mesh_upload_bytes,
        t.mesh_evictions,
        t.mesh_resident_bytes,
        t.mesh_store_missing,
        t.stroke_draws,
    )
}

/// The `tile cache (<role>):` running-total line, one per cache role that
/// has recorded anything. See [`overlay_raster_line`] for why this is a
/// value.
///
/// Denominator: **one event at a tile source's cache** — see
/// `squallar_egui::tile_source::cache_ledger`, which states the ten counters
/// and the three levels and why none is added to another. It is the reading
/// the `ground tiles:` line above cannot give: that line's `uploads` and
/// `evicted` are the GPU store's, keyed on a minted identity, so they cannot
/// say whether an upload was a tile's first sight, a restyle, or the same tile
/// fetched again after the cache dropped it. `refetch after eviction` is the
/// subset of `asks` that answers that; `duplicate` and `orphan` are the two
/// shapes of a body fetched for nothing. `entries`, `B resident` and `parsed`
/// are levels and go down; `B` figures are the lower bound the slot can price
/// today. `snap` is a level too, `1` while the tile-sharpness rung holds the
/// role's source at the whole zoom below the fractional one
/// (`squallar_egui::tile_source::snap`), else `0`.
///
/// Never compared to `ground tiles: uploads` by subtraction: a put is a cache
/// slot and an upload is a mesh buffer write, and a put with no fills uploads
/// nothing.
fn tile_cache_line(
    role: squallar_egui::tile_source::cache_ledger::CacheRole,
    t: &squallar_egui::tile_source::cache_ledger::Totals,
) -> String {
    format!(
        "tile cache ({}): {} asks, {} restyle asks, {} refetch after eviction, \
         {} puts first, {} restyle, {} duplicate, {} orphan, {} evicted pending, \
         {} evicted resident of {} B, {} entries, {} B resident, {} parsed, snap {}",
        role.label(),
        t.requests,
        t.restyle_asks,
        t.refetch_after_eviction,
        t.puts_first,
        t.puts_restyle,
        t.puts_duplicate,
        t.puts_orphan,
        t.evicted_pending,
        t.evicted_resident,
        t.evicted_bytes,
        t.resident_entries,
        t.resident_bytes,
        t.parsed_entries,
        t.snapped,
    )
}

/// The `basemap tiles:` running-total line. See [`overlay_raster_line`] for
/// why this is a value rather than an argument to `log::info!` — and this one
/// most of all, because the Tier-2 rig **gates** on it
/// (`--expect-basemap-tiles`) rather than merely printing it.
///
/// A third denominator, added to neither raster figure. Every number here
/// counts one archive tile *body* that decoded; see
/// [`squallar_egui::basemap_ledger`] for what that excludes. `vector` is the
/// self-hosted basemap's MVT and is the floor — it went to zero for the whole
/// life of a shipped build while every other Tier-2 assertion stayed green,
/// which is the reading this line exists to make. `raster` is the terrain
/// hillshade, legitimately zero with terrain off. `sniffed` is an archive that
/// declared no `tile_type`, which no archive this app opens does, so any
/// non-zero reading there is itself the finding.
fn basemap_tile_line(t: &squallar_egui::basemap_ledger::Totals) -> String {
    format!(
        "basemap tiles: {} vector, {} raster, {} sniffed",
        t.vector_tiles, t.raster_tiles, t.sniffed_tiles,
    )
}

/// What the shell does with the mirror on one frame — the two old states plus
/// the strip cache's third.
///
/// Reading it off `Gui::mirror_source_rects`'s verdict
/// and nothing else is the correctness rule: rendering over a held pass
/// blanks every floor (the pass's primitives carry no strips), and holding
/// over a repainted pass freezes them stale.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MirrorFrame {
    /// No floor strips on screen: release the mirror texture.
    Release,
    /// The strips repainted this pass: size to the observed plan and render.
    Render,
    /// The strips were held clean: keep the texture, render nothing.
    Hold,
}

/// The one place the verdict is decided. A free function over the sources'
/// two facts so the arm choice is unit-testable without a device; the facts
/// themselves still come only off `MirrorSources`, whose fields the Gui owns.
fn mirror_frame_action(rects_empty: bool, repainted: bool) -> MirrorFrame {
    if rects_empty {
        MirrorFrame::Release
    } else if repainted {
        MirrorFrame::Render
    } else {
        MirrorFrame::Hold
    }
}

/// The third state exists and the two old ones survive: rendering over a
/// held pass would blank every floor (its primitives carry no strips), and
/// releasing on a held pass would leave the raymarch sampling a destroyed
/// texture — `Hold` is reachable ONLY as keep-but-don't-render.
#[cfg(test)]
mod mirror_frame_tests {
    use super::{MirrorFrame, mirror_frame_action};

    #[test]
    fn a_held_pass_keeps_the_texture_and_renders_nothing() {
        assert_eq!(mirror_frame_action(false, false), MirrorFrame::Hold);
    }

    #[test]
    fn a_repainted_pass_renders() {
        assert_eq!(mirror_frame_action(false, true), MirrorFrame::Render);
    }

    #[test]
    fn no_strips_releases_whatever_the_verdict_says() {
        // An empty guest list releases even on a "repainted" pass: with no
        // floor on screen there is nothing to hold the texture for.
        assert_eq!(mirror_frame_action(true, false), MirrorFrame::Release);
        assert_eq!(mirror_frame_action(true, true), MirrorFrame::Release);
    }
}

// ── The frame timing lines ──
//
// Values, not `log!` arguments, for the same seam reason as the raster lines
// above: the browser rig reads these sentences back out of the console with
// regexes, and `frame_telemetry_line_tests` pins each one. Every figure is a
// whole microsecond and every percentile is a conservative bin upper edge —
// see `squallar_device_profile::hist`. **No figure in any of these lines ever
// gates CI.**

/// A histogram percentile for a line: whole microseconds, `none` on an empty
/// histogram, `over` when the ranked sample sits in the over-64 ms clamp,
/// whose upper edge does not exist.
fn pctl_us(h: &squallar_device_profile::hist::Hist, q: f64) -> String {
    match h.percentile_upper_micros(q) {
        None => "none".to_owned(),
        Some(u32::MAX) => "over".to_owned(),
        Some(us) => us.to_string(),
    }
}

/// A histogram's raw counts for a line: 42 comma-separated totals — the
/// under-62.5 µs clamp, the 40 geometric bins in edge order, the at-or-over
/// 64 ms clamp. Cumulative from boot; a windowed reading is the difference of
/// two of these.
fn hist_counts(h: &squallar_device_profile::hist::Hist) -> String {
    h.counts()
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// The `frame service (interact):` line, histogram embedded.
///
/// Denominator: presented frames whose egui raw input carried at least one
/// pointer/touch/wheel/zoom event. Service is the frame thread's own work —
/// the whole redraw minus the swapchain acquire (the vsync block). Cumulative
/// from boot.
fn frame_service_interact_line(h: &squallar_device_profile::hist::Hist) -> String {
    format!(
        "frame service (interact): n={}, p50={} us, p90={} us, p99={} us, hist={}",
        h.total(),
        pctl_us(h, 0.50),
        pctl_us(h, 0.90),
        pctl_us(h, 0.99),
        hist_counts(h),
    )
}

/// The `frame service (idle):` line, histogram embedded.
///
/// Denominator: presented frames whose egui raw input carried none of the
/// interaction events — the floor under the interact family, and the figure
/// that prices this instrument's own overhead. Cumulative from boot.
///
/// The histogram is what makes the family window-diffable, and the windowed
/// idle family is where the **settle burst** shows: WO-8 moved the
/// post-gesture re-raster out of the interact window, so its cost lands on
/// input-free frames inside a gesture window's quiet phases. A scoreboard
/// that read only the interact diff would have moved that cost somewhere no
/// figure could see it.
fn frame_service_idle_line(h: &squallar_device_profile::hist::Hist) -> String {
    format!(
        "frame service (idle): n={}, p50={} us, p90={} us, p99={} us, hist={}",
        h.total(),
        pctl_us(h, 0.50),
        pctl_us(h, 0.90),
        pctl_us(h, 0.99),
        hist_counts(h),
    )
}

/// The `frame segments:` line — where an interact frame's service goes.
///
/// Denominator: interact frames only (see [`frame_service_interact_line`]),
/// p99 per segment. The acquire is reported beside them and is **not** a
/// service segment: it is the vsync block, already excluded from service, and
/// adding it to the six segments does not produce any figure this instrument
/// quotes.
fn frame_segments_line(
    s: &crate::frame_ledger::SegmentHists,
    acquire: &squallar_device_profile::hist::Hist,
) -> String {
    format!(
        "frame segments (interact, p99 us): pre={}, pump={}, ui={}, \
         prepare={}, finish={}, post={}; acquire n={}, p50={} us, p99={} us",
        pctl_us(&s.pre, 0.99),
        pctl_us(&s.pump, 0.99),
        pctl_us(&s.ui, 0.99),
        pctl_us(&s.prepare, 0.99),
        pctl_us(&s.finish, 0.99),
        pctl_us(&s.post, 0.99),
        acquire.total(),
        pctl_us(acquire, 0.50),
        pctl_us(acquire, 0.99),
    )
}

/// One named histogram, whole: its count, its **exact** running sum, three
/// conservative percentiles and its 42 raw bins.
///
/// # Why the sum and not the mean
///
/// The mean is what a reader wants and the sum is what a *window* needs. `n`
/// and `sum` both subtract between two readings, so a windowed mean is
/// `(sum_b - sum_a) / (n_b - n_a)` and is **exact**; a mean printed directly
/// would be a cumulative-from-boot average that cannot be un-mixed from the
/// window inside it. Same division of labour the `frame prep costs:` line
/// already uses — "cost per pass is the figure divided by the pass count of
/// the same reading".
///
/// # Why this shape exists, when `frame segments` already reports the segments
///
/// Because a percentile does not subtract and a histogram does. The
/// `frame segments` line answers p99 **cumulative from boot**, so the only
/// question it can answer is "since boot, boot frames and the first volume
/// build included" — and the question anyone actually has about a gesture is
/// "what did this segment cost *during the gesture*". No windowed per-segment
/// figure was obtainable from the shipping instrument at all; a lane that
/// needed one had to build a private one on a branch, and the figures it
/// produced named segments that do not exist here and had to be retracted.
///
/// `hist=` is what closes that: two readings of this line, subtracted bin by
/// bin (`Hist::diff`, which `.github/browser-rig/drive.py` implements as
/// `hist_diff`), are the window. The mean is carried beside the bins because
/// the bins are four per octave — one bin apart is anywhere from 0% to 19%,
/// and every true ratio between 1.68x and 2.38x prints as exactly `2.00x`. A
/// windowed mean is exact and a windowed percentile is not.
///
/// The `frame segments` line is deliberately left exactly as it was: it is
/// pinned by three things that move together, and this is additive.
fn named_hist_line(prefix: &str, name: &str, h: &squallar_device_profile::hist::Hist) -> String {
    format!(
        "{prefix} ({name}): n={}, sum={} us, p50={} us, p90={} us, p99={} us, hist={}",
        h.total(),
        h.sum_micros(),
        pctl_us(h, 0.50),
        pctl_us(h, 0.90),
        pctl_us(h, 0.99),
        hist_counts(h),
    )
}

/// The six `frame segment (<name>):` lines — the windowable spelling of
/// [`frame_segments_line`].
///
/// Denominator: interact frames only, the same as `frame segments`, and the
/// six are **contiguous cuts of one frame's service**, so their sum telescopes
/// to it. The acquire is not among them and is not a service segment; it stays
/// on the `frame segments` line where it already is.
///
/// All six are emitted every tick, `n=0` included — the same choice
/// [`super::App::report_frame_telemetry`] already makes for the interact
/// family, and for the same reason: "nobody touched the window" has to be
/// readable as a figure rather than as an absence.
fn frame_segment_lines(s: &crate::frame_ledger::SegmentHists) -> [String; 6] {
    [
        named_hist_line("frame segment", "pre", &s.pre),
        named_hist_line("frame segment", "pump", &s.pump),
        named_hist_line("frame segment", "ui", &s.ui),
        named_hist_line("frame segment", "prepare", &s.prepare),
        named_hist_line("frame segment", "finish", &s.finish),
        named_hist_line("frame segment", "post", &s.post),
    ]
}

/// The six `frame prepare (<name>):` lines — the `prepare` segment, opened up.
///
/// Denominator: **exactly `frame segment (prepare)`'s** — presented interact
/// frames — and the six are contiguous cuts of that one span, so their sums
/// telescope to its sum. That equality is what makes this a decomposition
/// rather than a seventh segment: `frame prepare (*)` is never added to
/// `frame segment (prepare)`, it *is* it.
///
/// **Never added to `frame prep costs:` either.** That line counts every egui
/// pass ended — idle frames and frames that never presented included — so it
/// has strictly more samples in it than there are frames here. The two time
/// overlapping work over different frame sets; only these six share a
/// denominator with a frame segment, which is the whole reason they exist.
///
/// Emitted every tick, `n=0` included, on [`frame_segment_lines`]' terms: none
/// of the six is ever structurally absent, and a `mirror` cut of zero on a
/// 2D-only install is a figure, not an absence.
fn frame_prepare_lines(p: &crate::frame_ledger::PrepareHists) -> [String; 6] {
    [
        named_hist_line("frame prepare", "plan", &p.plan),
        named_hist_line("frame prepare", "end-pass", &p.end_pass),
        named_hist_line("frame prepare", "tessellate", &p.tessellate),
        named_hist_line("frame prepare", "upload", &p.upload),
        named_hist_line("frame prepare", "mirror", &p.mirror),
        named_hist_line("frame prepare", "buffers", &p.buffers),
    ]
}

/// The nine `frame ui (<name>):` lines — the `ui` segment, opened up.
///
/// Denominator: **exactly `frame segment (ui)`'s** — presented interact
/// frames — and the six are contiguous cuts of that one span, so their sums
/// telescope to its sum. That equality is what makes this a decomposition
/// rather than a seventh segment: `frame ui (*)` is never added to
/// `frame segment (ui)`, it *is* it.
///
/// A sibling of [`frame_prepare_lines`] and independent of it: that one cuts
/// `prepare`, this one cuts `ui`, and the two share only the formatter. Both
/// are decompositions of their own segment and neither is ever added to the
/// other.
///
/// Emitted every tick, `n=0` included, on [`frame_segment_lines`]' terms:
/// none of the six is ever structurally absent, and a `chrome` cut of zero on
/// a desktop layout with no phone bar is a figure, not an absence.
fn frame_ui_lines(u: &crate::frame_ledger::UiHists) -> [String; 9] {
    [
        named_hist_line("frame ui", "poll", &u.poll),
        named_hist_line("frame ui", "layout", &u.layout),
        named_hist_line("frame ui", "topbar", &u.topbar),
        named_hist_line("frame ui", "statusbar", &u.statusbar),
        named_hist_line("frame ui", "stack", &u.stack),
        named_hist_line("frame ui", "dialog", &u.dialog),
        named_hist_line("frame ui", "panes", &u.panes),
        named_hist_line("frame ui", "apply", &u.apply),
        named_hist_line("frame ui", "chrome", &u.chrome),
    ]
}

/// The eight `frame pump (<name>):` lines — the `pump` segment, opened up.
///
/// Same denominator as `frame segment (pump)` — presented interact frames —
/// and a DECOMPOSITION of that one span rather than a ninth segment:
/// `frame pump (*)` is never added to `frame segment (pump)`.
///
/// `frame pump (dispatch)` is **not** `frame post (dispatch)`: this one is the
/// `Dispatch` pump walk before the paint list is built, that one is
/// `dispatch_overlay_renders` after the present. Different spans, different
/// parents, never summed.
fn frame_pump_lines(p: &crate::frame_ledger::PumpHists) -> [String; 8] {
    [
        named_hist_line("frame pump", "begin", &p.begin),
        named_hist_line("frame pump", "restore", &p.restore),
        named_hist_line("frame pump", "promote", &p.promote),
        named_hist_line("frame pump", "raster", &p.raster),
        named_hist_line("frame pump", "apply", &p.apply),
        named_hist_line("frame pump", "advance", &p.advance),
        named_hist_line("frame pump", "dispatch", &p.dispatch),
        named_hist_line("frame pump", "settle", &p.settle),
    ]
}

/// The seven `frame post (<name>):` lines — the `post` segment, opened up.
///
/// Denominator: **exactly `frame segment (post)`'s** — presented interact
/// frames — and the seven are contiguous cuts of that one span, so their sums
/// telescope to its sum. That equality is what makes this a decomposition
/// rather than a seventh segment: `frame post (*)` is never added to
/// `frame segment (post)`, it *is* it.
///
/// Emitted every tick, `n=0` included, on [`frame_segment_lines`]' terms.
/// Five of the seven are structurally near-empty on every arm measured, and
/// that is the reading, not an absence: `post`'s cost is one occasional event
/// and the split exists to name which cut carries it.
fn frame_post_lines(p: &crate::frame_ledger::PostHists) -> [String; 7] {
    [
        named_hist_line("frame post", "handle", &p.handle),
        named_hist_line("frame post", "dispatch", &p.dispatch),
        named_hist_line("frame post", "back", &p.back),
        named_hist_line("frame post", "wake", &p.wake),
        named_hist_line("frame post", "poll", &p.poll),
        named_hist_line("frame post", "repaint", &p.repaint),
        named_hist_line("frame post", "close", &p.close),
    ]
}

/// The nine `frame finish (<name>):` lines — the `finish` segment, opened up.
///
/// # Denominator, and it is the one thing to read before any figure here
///
/// **Every presented frame, interact and idle alike** — which is NOT the
/// denominator of `frame segment (finish)` beside it, nor of any other split
/// family this file prints. All four of those record inside `finalize`'s
/// `if interacted` arm; this one records outside it, on purpose, because the
/// frames on which `finish` is a p99 contributor are idle ones. See
/// `frame_ledger::FinishHists`.
///
/// So the eight named cuts are **never added to `frame segment (finish)`**
/// and are not a decomposition of it: they decompose the same span over a
/// strictly larger frame set, and `n` here is larger than `n` there by the
/// idle frames. `frame finish (whole)` is the parent on this family's own
/// terms — the eight telescope to it exactly — so every share this family
/// supports is computed without borrowing another family's count.
///
/// Emitted every tick, `n=0` included, on [`frame_segment_lines`]' terms.
fn frame_finish_lines(f: &crate::frame_ledger::FinishHists) -> [String; 9] {
    [
        named_hist_line("frame finish", "file", &f.file),
        named_hist_line("frame finish", "view", &f.view),
        named_hist_line("frame finish", "draw", &f.draw),
        named_hist_line("frame finish", "resolve", &f.resolve),
        named_hist_line("frame finish", "submit", &f.submit),
        named_hist_line("frame finish", "collect", &f.collect),
        named_hist_line("frame finish", "free", &f.free),
        named_hist_line("frame finish", "present", &f.present),
        named_hist_line("frame finish", "whole", &f.whole),
    ]
}

/// The seven `frame dispatch (<name>):` lines — `frame post (dispatch)`,
/// opened up.
///
/// Denominator: **exactly `frame post (dispatch)`'s**, narrowed to the frames
/// whose tail dispatched at all — six named cuts plus a residual, contiguous
/// within that one span, so their sums telescope to it. `frame dispatch (*)`
/// is never added to `frame post (*)` and never to `frame segment (post)`:
/// each is the one below it, opened up, and adding any pair double-counts.
///
/// **`n` is smaller here than on `frame post (dispatch)`, and that is a
/// figure.** A frame whose tail dispatched nothing has no dispatch to
/// decompose and contributes no sample, where the parent cut records a
/// near-zero span on every interact frame. So `n` on these seven is the count
/// of dispatching frames in the window — the denominator the 84% reading
/// needs and the parent line cannot give.
///
/// Emitted every tick, `n=0` included, on [`frame_segment_lines`]' terms: a
/// window in which nothing dispatched is a reading, not an absence.
///
/// **Read the verdict off `sum`, not the percentiles.** `Hist` is four bins
/// per octave, so every percentile it answers is quantized to a bin edge and
/// any true ratio between 1.68x and 2.38x prints as exactly 2.00x; `sum` is
/// carried exactly beside the bins and differences exactly. The reading this
/// split exists to produce — which cut holds what share of `dispatch` — is a
/// ratio of sums for that reason.
fn frame_dispatch_lines(d: &crate::frame_ledger::DispatchHists) -> [String; 7] {
    [
        named_hist_line("frame dispatch", "dedupe", &d.dedupe),
        named_hist_line("frame dispatch", "marks", &d.marks),
        named_hist_line("frame dispatch", "hydrate", &d.hydrate),
        named_hist_line("frame dispatch", "prepare", &d.prepare),
        named_hist_line("frame dispatch", "hitmap", &d.hitmap),
        named_hist_line("frame dispatch", "offload", &d.offload),
        named_hist_line("frame dispatch", "residual", &d.residual),
    ]
}

/// The `frame worst:` line — the anatomy of ONE frame, not a distribution.
///
/// Denominator: **every presented frame of the last telemetry period**, both
/// families, which is not any other frame line's denominator here. The six
/// segment figures are that single frame's microseconds and they sum to its
/// service exactly, because they are the same six the ledger telescoped to it.
///
/// **Never added to `frame segments` or to `frame segment (*)`.** Those are
/// percentiles over interact frames; this is one frame, and it is usually not
/// one of theirs — the frames that pay for a click carry no pointer event, so
/// they are filed idle and no segment or cut histogram in this file ever sees
/// them. That gap is the reason this line exists: scene D's verdict is `p99`
/// AND `max`, and until this line a `max` could be located in a bin but never
/// opened up. The one previous attempt to open one up by inference — CARD-D's
/// reading of a 53.8 ms scene-D max as `ui` — was measured wrong afterwards.
///
/// `family=` is reported, never filtered on: "this scene's worst frame is
/// always an idle one" is a finding, and hiding it behind a filter is how the
/// last one stayed hidden.
///
/// A period in which nothing presented prints the absence rather than a zero
/// frame, on [`frame_segment_lines`]' terms inverted: a zero here would be a
/// frame that cost nothing, which is a different claim from no frame at all.
fn frame_worst_line(
    worst: Option<crate::frame_ledger::WorstFrame>,
    since_boot: Option<crate::frame_ledger::WorstFrame>,
) -> String {
    // The since-boot maximum's own anatomy rides the same line, so a boot-time
    // spike reads as `boot: idle, prepare=<nearly all>` instead of as a bare
    // number nobody can attribute. `since_boot=<service> us` keeps its place
    // so a reader matching the prefix is unchanged.
    let boot = match since_boot {
        None => "boot: none".to_string(),
        Some(b) => {
            let [pre, pump, ui, prepare, finish, post] = b.segments;
            format!(
                "boot: {}, pre={} us, pump={} us, ui={} us, prepare={} us, finish={} us, post={} us",
                if b.interact { "interact" } else { "idle" },
                pre,
                pump,
                ui,
                prepare,
                finish,
                post,
            )
        }
    };
    let since_boot_us = since_boot.map_or(0, |b| b.service);
    let Some(w) = worst else {
        return format!(
            "frame worst: no frame presented this period, since_boot={since_boot_us} us, {boot}"
        );
    };
    let [pre, pump, ui, prepare, finish, post] = w.segments;
    format!(
        "frame worst: service={} us, family={}, since_boot={} us, pre={} us, pump={} us, \
         ui={} us, prepare={} us, finish={} us, post={} us, {boot}",
        w.service,
        if w.interact { "interact" } else { "idle" },
        since_boot_us,
        pre,
        pump,
        ui,
        prepare,
        finish,
        post,
    )
}

/// The `tile take (<family>):` lines — what one tile take costs the thread
/// that performs it.
///
/// Denominator: **one completion moved off a source's tile channel and handled
/// to completion** — see `squallar_egui::tile_source::take_ledger`, which
/// states it in full along with why the five families are never added to each
/// other, and never to `overlay rasters`, `texture uploads`, `basemap tiles`
/// or any frame segment.
///
/// This is the figure `PUMP_TIME_BUDGET`'s own doc has to reason about in
/// prose — "a multi-millisecond tessellation" — because the budget is checked
/// **between** takes and never during one, so a frame pays that budget plus
/// one whole unbounded take and nothing measured the second half.
///
/// Unlike the frame segments, a family is emitted **only when it has samples**.
/// A family with `n=0` has no window to diff and nothing to say, and three of
/// the five are structurally empty on any given arm (`put` is the native arm's
/// only family and never appears on the browser; `sniffed` and `restyle` need
/// a plain-HTTP source and a theme flip respectively). Six frame-segment lines
/// every tick is a floor worth paying; five permanently-zero tile lines beside
/// them is console the reader has to step over, and the console ring the rig
/// scrapes holds 1200 entries and evicts.
fn tile_take_lines(t: &squallar_egui::tile_source::take_ledger::Totals) -> Vec<String> {
    squallar_egui::tile_source::take_ledger::FAMILIES
        .into_iter()
        .filter(|&kind| t.family(kind).total() > 0)
        .map(|kind| named_hist_line("tile take", kind.label(), t.family(kind)))
        .collect()
}

/// The `tile phase (parse|style):` lines — one vector take, opened up.
///
/// Denominator: **one vector body decoded**, one sample per phase — see
/// `squallar_egui::tile_source::take_ledger::PhaseTotals`. A *decomposition*
/// of `tile take (vector)`, never a sixth take family and never added to one:
/// the two phases sum to a vector take minus its cache put. A restyle records
/// a `style` sample and no `parse` sample, so the two `n`s legitimately differ.
///
/// Why the split is worth its two clock reads: the halves have **different
/// resumability**. `parse` walks the tile's source layers, at most sixteen of
/// them, and its finest unit is one layer; `style` walks the style's layers
/// taking a lazy iterator of features, and its finest unit is one feature, of
/// which a dense tile has thousands. Which half carries the cost is what
/// decides how finely a take could be cut — so this line is the evidence under
/// any claim about banding, rather than a number for its own sake.
///
/// Emitted only for phases with samples, on [`tile_take_lines`]' terms.
fn tile_phase_lines(t: &squallar_egui::tile_source::take_ledger::PhaseTotals) -> Vec<String> {
    squallar_egui::tile_source::take_ledger::PHASES
        .into_iter()
        .filter(|&phase| t.phase(phase).total() > 0)
        .map(|phase| named_hist_line("tile phase", phase.label(), t.phase(phase)))
        .collect()
}

/// The `tile bodies:` line — where this process's vector tile bodies were
/// paid for.
///
/// Denominator: **one vector tile body disposed of**, which is the
/// denominator the two `tile phase` lines above should be read against.
/// Running totals, never added to a take family and never to a duration.
///
/// **Emitted unconditionally, unlike every family line above it**, and that
/// asymmetry is the reason this line exists. A take family with no samples is
/// not printed, which is right for a family — but once the browser's pump
/// offloads, `tile phase (parse)` and `(style)` fall towards `n = 0` because
/// the work genuinely left the frame thread, and a reader cannot tell that
/// from a line that was never collected. A count has nothing to diff against
/// and no reason to go quiet, so `0 offloaded, 0 inline` is a reading rather
/// than a silence.
fn tile_disposition_line(d: &squallar_egui::tile_source::take_ledger::Disposition) -> String {
    format!(
        "tile bodies: {} offloaded, {} decoded on the frame thread",
        d.offloaded, d.inline,
    )
}

/// The `frame prep costs:` running-total line.
///
/// Denominator: every egui pass this renderer ended, presented or not — see
/// [`squallar_gpu::egui_renderer::pass_costs::PassCosts`], whose fields these
/// are. Cumulative microsecond totals, not percentiles: cost per pass is the
/// figure divided by the pass count of the same reading.
fn prep_costs_line(c: &squallar_gpu::egui_renderer::pass_costs::PassCosts) -> String {
    format!(
        "frame prep costs: {} passes, {} us tessellate, {} us upload apply, \
         {} us mirror, {} us buffers and callbacks",
        c.passes, c.tessellate_us, c.upload_apply_us, c.mirror_us, c.buffers_and_callbacks_us,
    )
}

/// The `frame prep geometry:` running-total line — the byte side of the
/// `buffers` phase on the line above.
///
/// **A different denominator from `frame prep costs:`, and the two are never
/// divided across.** `stagings` is every `update_buffers` call, which a pass
/// that renders a pane mirror makes twice; `passes` on the line above is
/// every pass ended. See
/// [`squallar_gpu::egui_renderer::pass_costs::StagedGeometry`], whose fields
/// these are.
///
/// **What it is for.** `B staged` over the same reading's `us buffers and
/// callbacks` **plus its `us mirror`** is the effective bandwidth of the
/// staging copy — the one question that path has never been able to answer,
/// because every other figure on it is a clock and a clock cannot tell a
/// large copy from a slow one. Both sides are running totals, so a windowed
/// rate is a subtraction. The mirror term is in the divisor because the
/// mirror's staging is timed into `us mirror`, not into `us buffers and
/// callbacks`; with the mirror off it is zero and the rate is exact, and with
/// it on the divisor over-states the time, so the rate is a lower bound and
/// can never make a slow copy read fast.
///
/// `vertices` and `indices` are the staging identity: the same picture staged
/// by a different route must read the same two counts, so a route that is
/// quicker because it stages less cannot pass as quicker.
///
/// **Which route, and a third denominator.** The trailing pair is
/// [`squallar_gpu::egui_renderer::geometry_staging::GeometryStagingTotals`]:
/// stagings that went through cached host memory, and stagings that fell back
/// to the mapping `wgpu` places in the card's BAR window. They sum to
/// `stagings` minus the calls whose picture was entirely paint callbacks, so
/// they are **not** subtracted from the count above. Both zero is a build with
/// no ring — all of the web; `0 through the ring` with a non-zero
/// `declined` is a ring that is installed and refusing, which costs what the
/// BAR costs and is invisible in every other figure on this line.
fn prep_geometry_line(
    g: &squallar_gpu::egui_renderer::pass_costs::StagedGeometry,
    r: &squallar_gpu::egui_renderer::geometry_staging::GeometryStagingTotals,
) -> String {
    format!(
        "frame prep geometry: {} stagings, {} vertices, {} indices, {} B staged, \
         {} through the ring, {} declined",
        g.calls, g.vertices, g.indices, g.bytes, r.staged, r.declined,
    )
}

/// The `command stream:` line — what the main pass's walk of the primitive
/// list recorded into the encoder.
///
/// **Not a `frame <name>` line, deliberately.** That prefix is this app's
/// namespace for the timing families the rig buckets and
/// `every_frame_line_family_the_app_writes_has_a_named_rig_probe` enumerates;
/// every one of them is a clock. This is a count, and it sits with the other
/// always-on counter lines — `overlay rasters:`, `texture uploads:`,
/// `ground tiles:` — which name themselves.
///
/// **Two readings on this one line and they are never added.** The `last`
/// group is the most recent walk alone — what "a scene-D frame records N
/// commands" means. The `per walk` group is the running totals divided by
/// their own walk count, which is every main pass drawn since boot and so
/// spans every layout the session has been in. A window is a subtraction of
/// two `total` readings, not a reading of the mean.
///
/// Denominator: **every [`squallar_gpu::egui_renderer::EguiRenderer::draw`]**
/// — the frame's main pass alone. The pane-mirror pass walks the same
/// primitives a second time and is *not* counted here, unlike
/// `frame prep geometry:`'s stagings, which are.
///
/// **Counts, not clocks, and that is the point.** The frame tail is 93%
/// `queue.submit`, which on the GL backend replays this recorded stream as
/// real GL calls on the frame thread. A count of it is deterministic where a
/// timing of it is not: the same scene records the same number under any
/// load, so a reduction is provable on a box this campaign's 340 us noise
/// floor makes untimeable.
///
/// **The GL multiplier is not in `calls`.** `wgpu-core` drops the bind-group
/// repeats; `wgpu-hal`'s GL backend turns each draw's vertex bind into one
/// command where `VERTEX_BUFFER_LAYOUT` holds and into one per vertex
/// attribute — three, for egui's vertex — where it does not, which is every
/// WebGL2 leg. A `calls` figure quoted without its backend is two different
/// numbers.
fn command_stream_line(
    last: &squallar_gpu::egui_renderer::command_stream::CommandStream,
    total: &squallar_gpu::egui_renderer::command_stream::CommandStream,
) -> String {
    let per = |n: u64| n.checked_div(total.walks).unwrap_or(0);
    format!(
        "command stream: last {} primitives ({} mesh, {} callback, {} skipped), \
         {} draws, {} calls; splits {} clip, {} texture, {} callback, {} mergeable; \
         {} resets, {} scissors ({} repeat), {} bind groups ({} repeat), {} buffer binds; \
         per walk {} primitives, {} calls over {} walks",
        last.primitives,
        last.meshes,
        last.callbacks,
        last.skipped,
        last.draws,
        last.calls,
        last.split_clip,
        last.split_texture,
        last.split_callback,
        last.split_none,
        last.resets,
        last.scissor_sets,
        last.scissor_repeats,
        last.bind_group_sets,
        last.bind_group_repeats,
        last.buffer_binds,
        per(total.primitives),
        per(total.calls),
        total.walks,
    )
}

/// The `gpu passes:` line — what each bracketed pass family costs the GPU.
///
/// Three denominators share this line and are never added: each family's
/// `n=` is every pass of that kind ENCODED (six volume panes are six raymarch
/// passes); its percentiles are over one bracketed pass per frame in which
/// the family ran; the trailing `frames` is resolves collected — the floor
/// under every percentile. GPU time, a different clock from every `frame *`
/// line above, and **never added to service**. Cumulative from boot.
fn gpu_passes_line(r: &squallar_gpu::gpu_probe::GpuPassReport) -> String {
    use squallar_gpu::gpu_probe::ProbedPass;
    let family = |pass: ProbedPass| {
        format!(
            "n={}, p50={} us, p99={} us",
            r.passes(pass),
            pctl_us(r.hist(pass), 0.50),
            pctl_us(r.hist(pass), 0.99),
        )
    };
    format!(
        "gpu passes: raymarch {}; ground {}; mirror {}; main {}; {} frames",
        family(ProbedPass::Raymarch),
        family(ProbedPass::Ground),
        family(ProbedPass::Mirror),
        family(ProbedPass::Main),
        r.frames,
    )
}

/// What a keyed-loud install hears instead of [`gpu_passes_line`] on an
/// adapter that cannot time a pass — every WebGL2 leg, always. An absence
/// stated as one, never an extrapolated figure; the rig greps this sentence
/// verbatim, so it is pinned beside the others.
const GPU_PASSES_UNAVAILABLE_LINE: &str = "gpu passes: unavailable (adapter lacks TIMESTAMP_QUERY)";

/// The `frame cadence:` line, histogram embedded.
///
/// Denominator: the interval between consecutive **presented** frames'
/// starts — redraw to redraw, both input families. The co-criterion beside
/// service: on a leg whose GPU passes cannot be timed, a limping cadence is
/// what betrays a GPU-bound frame whose CPU service reads innocent. **Never
/// added to service** — the two share no denominator. Cumulative from boot.
fn frame_cadence_line(h: &squallar_device_profile::hist::Hist) -> String {
    format!(
        "frame cadence: n={}, p50={} us, p99={} us, hist={}",
        h.total(),
        pctl_us(h, 0.50),
        pctl_us(h, 0.99),
        hist_counts(h),
    )
}

impl super::App {
    /// Set up and run the egui UI pass.
    pub(super) fn setup_egui_frame(&mut self) -> ([u32; 2], Vec<GuiAction>) {
        self.frame_ledger.mark_setup_entry();
        // Before the pass, because the cache it writes is read by everything
        // that rasterizes off-frame — see `App::resolve_theme`.
        let use_dark_theme = self.resolve_theme();

        // Open egui's pass and apply the theme.
        let size_in_pixels = {
            let state = self.state.as_mut().unwrap();
            let window = self.window.as_ref().unwrap();

            let window_size = window.inner_size();
            // The CSS-size-to-backing-store ratio, and nothing else.
            let zoom_factor = state.surface_config.width as f32 / window_size.width.max(1) as f32;

            // The gesture player's frame, empty on every unarmed install.
            // Computed against the window in egui points, since that is the
            // space the events land in.
            let extra_events = match self.gesture_player.as_mut() {
                Some(player) => {
                    let ppp = state
                        .egui_renderer
                        .context()
                        .pixels_per_point()
                        .max(f32::EPSILON);
                    let screen = egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(
                            window_size.width as f32 / ppp,
                            window_size.height as f32 / ppp,
                        ),
                    );
                    let now_secs = player.elapsed_secs();
                    player.events_for_frame(now_secs, screen)
                }
                None => Vec::new(),
            };

            // Start egui frame
            state
                .egui_renderer
                .begin_frame(window, zoom_factor, extra_events);

            state.egui_renderer.apply_theme(use_dark_theme);

            [state.surface_config.width, state.surface_config.height]
        };

        // Ensure pane_render vec matches gui pane count
        self.render.ensure_pane_count(self.gui.pane_count());
        // And the other thing keyed by pane index that a layout change strands:
        self.release_hidden_pane_volumes();

        let ctx = self.state.as_ref().unwrap().egui_renderer.context().clone();
        let pump_began = web_time::Instant::now();

        // First use of the context this frame, and it has to be: `begin_frame`
        // above is the moment egui is told this device's real texture limit,
        // and the restore is an upload. See `App::restore_cached_render`.
        if self.restore_pending {
            self.restore_cached_render(&ctx);
        }
        let pump_restored = web_time::Instant::now();

        // Before the pollers, which is before `Gui::ui` builds the paint list: a
        // raster whose last band landed on the previous frame goes on screen in
        // *this* frame's paint list rather than the next one. See the callee.
        self.promote_uploaded_rasters();
        let pump_promoted = web_time::Instant::now();
        // After the promote, so a picture that reached the screen this frame is
        // in the line this frame writes rather than the next one's.
        self.report_raster_telemetry();
        let pump_rastered = web_time::Instant::now();

        self.run_frame_pump(PumpPhase::Apply, Some(&ctx));
        let pump_applied = web_time::Instant::now();
        self.run_frame_pump(PumpPhase::Advance, Some(&ctx));
        let pump_advanced = web_time::Instant::now();
        self.run_frame_pump(PumpPhase::Dispatch, Some(&ctx));
        let pump_dispatched = web_time::Instant::now();
        let volume_budget = self
            .loop_allocation()
            .volume_reserve_bytes()
            .max(self.budgets.volume_loop_bytes());
        let evicted = self.volume_store.enforce_budget(volume_budget);
        if evicted > 0 {
            log::info!(
                "3D volume view: evicted {evicted} resident grid(s) to fit the {} MiB budget",
                volume_budget / (1024 * 1024),
            );
        }
        self.update_loop_readiness();

        // The frame's facts, composed after every drain above so each one
        // reflects this frame's arrivals, and applied in one call so the UI
        // can never see half a frame's worth.
        self.push_frame_inputs();

        // Every boundary this call crossed, handed over before `ui_start` is
        // stamped: the eight cuts telescope to `pump` exactly, and the right
        // boundary is the mark on the next line. See `frame_ledger::PumpHists`.
        self.frame_ledger
            .record_pump_phases(crate::frame_ledger::PumpPhaseStamps {
                began: pump_began,
                restored: pump_restored,
                promoted: pump_promoted,
                rastered: pump_rastered,
                applied: pump_applied,
                advanced: pump_advanced,
                dispatched: pump_dispatched,
            });
        // Last, so this frame is laid out over everything applied above.
        self.frame_ledger.mark_ui_start();
        let (gui_action, ui_phases, retired) = self.gui.ui_phased(&ctx);
        self.frame_ledger.mark_ui_end();
        // After `mark_ui_end`, because the stamps only cut anything when
        // paired with this frame's `ui_start`/`ui_end`. See
        // `frame_ledger::UiHists`.
        self.frame_ledger.record_ui_phases(ui_phases);
        // **Carried out on the frame's existing return, not fetched by a new
        // reach.** The overlay registry is the UI layer's, and the ceiling on
        // this file's reaches into it may only fall
        // (`gui_seam_ratchet_tests`), so the batch travels back with the
        // actions rather than through a second call.
        Self::discard_retired_overlay_data(retired);

        (size_in_pixels, gui_action)
    }

    /// **Free what the overlay layers retired this frame, away from here.**
    ///
    /// Each payload is one layer's replaced generation or one parked paint
    /// input: a list whose free is one free per item, and the lightning
    /// layer's is six figures of them. `discard_each` files them one at a
    /// time, so a drain turn frees one layer's batch rather than every
    /// layer's at once.
    ///
    /// Answers how many payloads went, so a test can say the seam moved
    /// something rather than only that it ran.
    pub(super) fn discard_retired_overlay_data(
        retired: Vec<Box<dyn std::any::Any + Send>>,
    ) -> usize {
        let moved = retired.len();
        squallar_worker::offload::discard_each("retired-overlay-data", retired);
        moved
    }

    /// Compose this frame's [`squallar_egui::shell_api::FrameInputs`] from the
    /// state the App owns and apply it — the one place the snapshot-shaped
    /// facts cross the Gui↔App seam.
    pub(super) fn push_frame_inputs(&mut self) {
        // The overlay's gpu row: the sentence [`gpu_passes_line`] prints,
        // rebuilt only when the probe has collected another frame. Where no
        // probe is installed this stays `None` and the overlay shows its own
        // absence text.
        if let Some(report) = self
            .state
            .as_ref()
            .and_then(|state| state.egui_renderer.gpu_pass_report())
            && self.gpu_passes_panel_frames != Some(report.frames)
        {
            self.gpu_passes_panel_frames = Some(report.frames);
            self.gpu_passes_panel_line = Some(gpu_passes_line(&report));
        }
        self.gui
            .apply_frame_inputs(squallar_egui::shell_api::FrameInputs {
                safe_area_insets: self.safe_area_insets,
                supports_exit: self.supports_exit,
                loop_frame_budget: self.loop_frame_budget,
                // Off the dispatcher, which resolved it from `Budgets` at
                // startup and is what every other background render on this
                // device is admitted against.
                concurrent_renders: self.render.concurrent_renders(),
                // What the tile caches may hold, as the last loop walk
                // priced it against this session's capacity — see
                // `Self::observe_loop_demand`.
                tile_cache: self.tile_cache_budget,
                // The overlay-oversampling rung in force, as the fraction the
                // planner takes: the ladder's answer for this session's
                // capacity, host axis included.
                overlay_overdraw: squallar_egui::overlay_cache::overdraw_for_oversample(
                    self.budgets.overlay_oversample_percent,
                ),
                location_settings_available: self.location_settings_available,
                // Read off the gate each frame; the gate is the owner and
                // `poll_platform_state` already redraws on a change.
                location: (self.location.permission(), self.location.active()),
                // The arrival instant travels with the fix — see the field.
                gps: self.user_gps.clone(),
                user_heading: self.user_heading,
                catalogue_pending: self.catalogue_pending,
                liveness: &self.liveness,
                floor_tile_zoom_bias: self.mirror_rungs.tile_zoom_bias(),
                mirror_plan_stamp: self.mirror_plan_stamp,
                frame_diagnostics: Some(squallar_egui::shell_api::FrameDiagnostics {
                    gpu_passes: self.gpu_passes_panel_line.as_deref(),
                    budget_state: self.budget_state_panel_line.as_deref(),
                    ..self.frame_ledger.diagnostics()
                }),
                // As the last telemetry tick composed it — see
                // `Self::compose_budget_readout`. Borrowed, and the Gui
                // copies it only when its generation moved, so re-stating it
                // every frame costs a pointer here and a `u64` compare there.
                budget_readout: Some(&self.budget_readout),
            });
    }

    /// Poll for completed background render results and upload textures.
    fn poll_render_results(&mut self, ctx: &egui::Context) {
        let mut uploads = PlanViewUploads::default();
        while let Ok(rr) = self.channels.render_receiver.try_recv() {
            // A speculative result before any pane bookkeeping:
            if let Some(site) = rr.speculative_for {
                self.render.speculative_finished();
                if !self.render.is_render_stale(rr.generation)
                    && let Some(rendered) = rr.rendered
                {
                    self.render.cache_render(
                        &site,
                        rr.product,
                        squallar_radar::types::RenderView::PlanView,
                        rr.elevation,
                        crate::render_dispatch::CachedRenderOutput {
                            image: rendered.image,
                            max_range_km: rendered.max_range_km,
                            hover: rendered.hover,
                            nyquist_ms: rendered.nyquist_ms,
                            melting_layer_source: rendered.melting_layer_source,
                            storm_motion: rendered.storm_motion,
                        },
                    );
                }
                continue;
            }
            if rr.pane_idx < self.render.pane_render.len() {
                self.render.pane_render[rr.pane_idx].render_finished();
            }

            if self.render.is_render_stale(rr.generation) {
                log::debug!(
                    "Discarding stale render result (gen {} < current {})",
                    rr.generation,
                    self.render.render_generation
                );
                continue;
            }

            if rr.pane_idx >= self.gui.pane_count()
                || self
                    .gui
                    .get_rendering_params_for_pane(rr.pane_idx)
                    .is_none()
            {
                continue;
            }

            // A render that found no sweep has already done its one job above by
            // clearing `render_in_flight`; there is nothing to cache or draw.
            let Some(rendered) = rr.rendered else {
                continue;
            };

            // Extract fields to avoid borrow issues
            let origin_pane = rr.pane_idx;
            let render_result = crate::render_dispatch::CachedPaneRender {
                image: rendered.image,
                max_range_km: rendered.max_range_km,
                hover: rendered.hover,
                product: rr.product,
                elevation: rr.elevation,
                nyquist_ms: rendered.nyquist_ms,
                melting_layer_source: rendered.melting_layer_source,
                storm_motion: rendered.storm_motion,
            };

            // Cache the render output for sharing with other panes on the same site.
            let (origin_site, origin_draws_plan, speculate_from) = self
                .gui
                .pane(origin_pane)
                .map(|p| {
                    let inputs = p
                        .is_map()
                        .then_some(p.scan_info.as_ref())
                        .flatten()
                        .map(|si| SpeculationInputs {
                            volume_start: si.timestamp,
                            lat: si.site.lat,
                            lon: si.site.lon,
                            scan_site: si.site.name,
                            ladder: si.product_elevations.get(&rr.product).cloned(),
                        });
                    (p.site().to_string(), p.is_map(), inputs)
                })
                .unwrap_or_default();
            self.render.cache_render(
                &origin_site,
                render_result.product,
                squallar_radar::types::RenderView::PlanView,
                render_result.elevation,
                crate::render_dispatch::CachedRenderOutput {
                    image: Arc::clone(&render_result.image),
                    max_range_km: render_result.max_range_km,
                    hover: Arc::clone(&render_result.hover),
                    nyquist_ms: render_result.nyquist_ms,
                    melting_layer_source: render_result.melting_layer_source,
                    storm_motion: render_result.storm_motion,
                },
            );

            if origin_draws_plan {
                self.apply_render_to_pane(ctx, origin_pane, &render_result, &mut uploads);
            }

            // Broadcast to sibling panes that need the same site+product+elevation.
            let pane_count = self.gui.pane_count();
            for other_idx in 0..pane_count {
                if other_idx == origin_pane {
                    continue;
                }
                let Some(other) = self.gui.pane(other_idx) else {
                    continue;
                };
                if !other.is_map() || other.site() != origin_site {
                    continue;
                }
                let Some((other_product, other_elevation)) = other
                    .get_rendering_params()
                    .and_then(|(id, e)| Some((squallar_radar::fields::product_for(&id)?, e)))
                else {
                    continue;
                };
                if other_product == render_result.product
                    && (other_elevation - render_result.elevation).abs() <= ELEVATION_TOLERANCE
                {
                    let needs = other_idx < self.render.pane_render.len()
                        && self.render.pane_render[other_idx]
                            .last_rendered
                            .map(|(lp, le)| {
                                lp != other_product
                                    || (le - other_elevation).abs() > ELEVATION_TOLERANCE
                            })
                            .unwrap_or(true);
                    if needs {
                        self.apply_render_to_pane(ctx, other_idx, &render_result, &mut uploads);
                    }
                }
            }

            if let Some(inputs) = speculate_from {
                self.maybe_spawn_speculative_render(
                    &origin_site,
                    render_result.product,
                    render_result.elevation,
                    inputs,
                );
            }
        }
    }

    /// Dispatch ONE adjacent-tilt pre-render after a delivered static plan
    /// view, when ALL of: not wasm and the budget is wide
    /// ([`crate::render_dispatch::speculative_render_allowed`] — desktop 6 /
    /// mobile 3 qualify, wasm's 1 never, AF8); **no interactive render in
    /// flight** (both the pane flags and the shared thread counter read
    fn maybe_spawn_speculative_render(
        &mut self,
        site: &str,
        product: squallar_radar::types::RadarProduct,
        delivered_elevation: f32,
        inputs: SpeculationInputs,
    ) {
        if !crate::render_dispatch::speculative_render_allowed(
            super::WEB,
            self.render.concurrent_renders(),
        ) {
            return;
        }
        if self.render.speculative_in_flight()
            || self.render.any_render_in_flight()
            || self
                .render
                .renders_in_flight
                .load(std::sync::atomic::Ordering::Relaxed)
                != 0
        {
            return;
        }
        // Level III panes render from fetched objects, not from the volume —
        // there is no Level II job to speculate.
        if product.is_level3() {
            return;
        }
        let Some(ladder) = inputs.ladder else {
            return;
        };
        let above = ladder
            .iter()
            .copied()
            .filter(|e| *e > delivered_elevation + ELEVATION_TOLERANCE)
            .min_by(|a, b| a.total_cmp(b));
        let below = ladder
            .iter()
            .copied()
            .filter(|e| *e < delivered_elevation - ELEVATION_TOLERANCE)
            .max_by(|a, b| a.total_cmp(b));
        let Some(target) = above.or(below) else {
            return;
        };
        // Already resident: the goal state — nothing to pre-render.
        if self
            .render
            .get_cached_render(
                site,
                product,
                squallar_radar::types::RenderView::PlanView,
                target,
            )
            .is_some()
        {
            return;
        }
        let Some((data, declared)) = self
            .volumes
            .still_for(inputs.scan_site, inputs.volume_start)
        else {
            return;
        };
        self.render.spawn_speculative_render(
            site,
            product,
            target,
            inputs.volume_start,
            inputs.lat,
            inputs.lon,
            data,
            &declared,
            self.channels.render_sender.clone(),
            self.window.clone(),
        );
    }

    /// Apply a rendered radar image to a specific pane (upload texture to overlay cache).
    fn apply_render_to_pane(
        &mut self,
        ctx: &egui::Context,
        pane_idx: usize,
        render: &crate::render_dispatch::CachedPaneRender,
        uploads: &mut PlanViewUploads,
    ) {
        use squallar_egui::overlay_cache::{OverlayTextureData, RadarTextureMeta};
        use squallar_geo::PlacedRaster;
        use squallar_radar::types::ImageBounds;

        // Extract site coordinates before mutable borrow
        let (lat, lon) = {
            let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                return;
            };
            (scan_info.site.lat, scan_info.site.lon)
        };

        // Whether the picture being applied is the picture already on this
        // pane — the *same buffer*, not a buffer that compares equal.
        let already_on_screen = self
            .render
            .pane_render
            .get(pane_idx)
            .and_then(|prs| prs.cached_render.as_ref())
            .is_some_and(|cached| Arc::ptr_eq(&cached.image, &render.image));

        // Let go of the old radar overlay texture — unless it is the one about
        // to go back, in which case it is kept rather than retired and
        // re-uploaded.
        let Some(pane) = self.gui.pane_mut(pane_idx) else {
            return;
        };
        let cache = pane.overlay_cache_mut(&squallar_source::id::known::RADAR);
        // The pane's own handle for these exact pixels, if it has one, and
        // whether that handle is **whole**.
        let retained = already_on_screen
            .then(|| match cache.held_texture() {
                Some(arriving) => Some((arriving.clone(), false)),
                None => cache.current().map(|old| (old.texture.clone(), true)),
            })
            .flatten();

        let side = render.image.width();
        let (texture, whole) = match retained {
            // The pane's own handle, preferred over anything `uploads` may hold
            // for the same raster. Not a lifetime question — see the note above
            // on why a replaced handle can just be dropped — but a churn one:
            Some(pair) => pair,
            None => {
                let counter = &mut self.texture_counter;
                let texture = uploads.handle(&render.image, || {
                    *counter += 1;
                    ctx.load_texture(
                        format!("radar_image_{counter}"),
                        Arc::clone(&render.image),
                        egui::TextureOptions::NEAREST,
                    )
                });
                // A texture minted this frame — by this call or by an earlier
                // pane in the same drain — has handed egui pixels that
                // `end_pass` has not seen yet, let alone moved. Never whole.
                (texture, false)
            }
        };

        // Cache the pixels for fast restore after suspend/resume
        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].cached_render = Some(CachedPaneRender {
                image: Arc::clone(&render.image),
                max_range_km: render.max_range_km,
                hover: Arc::clone(&render.hover),
                product: render.product,
                elevation: render.elevation,
                nyquist_ms: render.nyquist_ms,
                melting_layer_source: render.melting_layer_source,
                storm_motion: render.storm_motion,
            });
        }

        let bounds = ImageBounds::from_radar_site(lat, lon, render.max_range_km);
        let placed_raster: PlacedRaster = bounds.into();
        let pane = self.gui.pane_mut(pane_idx).unwrap();
        let data_time = self.render.data_time_for_render(pane, render);
        let placed = OverlayTextureData {
            texture,
            placed: placed_raster,
            data_generation: 0,
            render_zoom: 0,
            width: side as u32,
            height: side as u32,
            radar_meta: Some(RadarTextureMeta {
                hover: Arc::clone(&render.hover),
                lat,
                lon,
                max_range_km: render.max_range_km,
                nyquist_ms: render.nyquist_ms,
                melting_layer_source: render.melting_layer_source,
                storm_motion: render.storm_motion,
                product: crate::render_key::field_id_of(render.product),
                elevation: render.elevation,
            }),
            hit_map: None,
        };

        // **The swap, or the promise of one.**
        pane.place_radar_raster(placed, data_time, whole);

        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].last_rendered =
                Some((render.product, render.elevation));
        }
    }

    /// Show every held raster whose last band has landed.
    fn promote_uploaded_rasters(&mut self) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let renderer = &state.egui_renderer;
        self.gui
            .promote_held_rasters(|id| renderer.is_delivered(id));
    }

    /// **Say what the raster pipeline has spent, periodically, on a tick where
    /// it spent something since the last one.**
    ///
    /// Four lines, never one, because they have four different denominators
    /// and adding any of them would describe neither term:
    ///
    /// * `overlay rasters:` is the whole-picture overlay dispatch — the ten
    ///   layer kinds [`App::spawn_overlay_render`] rasterizes. Radar's own
    ///   pipeline and the loop frames are not in it. See
    ///   [`squallar_egui::overlay_cache::ledger`].
    /// * `texture uploads:` is what this device was then made to move, for
    ///   **every** texture egui holds — the font atlas, the basemap tiles and
    ///   radar included. See
    ///   [`squallar_gpu::egui_renderer::texture_upload::UploadTotals`].
    /// * `floor strips:` is the 3D floor path — strips painted per pane per
    ///   frame, mirror passes encoded per frame. See
    ///   [`squallar_egui::floor_ledger`].
    /// * `basemap tiles:` is archive tile **bodies decoded**, split by the
    ///   archive header's declared kind. A vector decode uploads no texture at
    ///   all and is in none of the three above; a raster decode is one egui
    ///   texture and so is a *subset* of `texture uploads`, never a term to
    ///   add to it. See [`squallar_egui::basemap_ledger`].
    ///
    /// Running totals rather than per-event deltas, so **one** line is the
    /// whole answer: the browser's console is a ring that evicts, and a reader
    /// that had to sum it would be summing whatever survived. That is the same
    /// reason `squallar_web`'s `worker_port` reports the transport this way.
    ///
    /// # A running total is a periodic readout, never a per-frame event
    ///
    /// Two things bound it, and they answer two different questions.
    ///
    /// **How often**: at most one round of lines per
    /// [`RASTER_TELEMETRY_PERIOD`], however busy the pipeline is. The
    /// alternative — a line on every frame the pipeline moved — is 120 INFO
    /// lines a second under any activity at all, saying a number that has
    /// grown by one. The trailing edge is free rather than tracked: the
    /// ledgers' own `*_if_moved` compare against the last **reported**
    /// figure, not the last frame's, so the tick after the pipeline stops
    /// still carries everything the suppressed frames added.
    ///
    /// **How loud**: `debug` on an ordinary install, and `info` only where
    /// [`raster_telemetry_is_loud`] says this install asked for them. A user
    /// who never asked for the pipeline's accounts never reads one.
    ///
    /// `info` for the instrument is load-bearing on the browser target, where
    /// `console_log` is initialised at `Level::Info` and a `debug!` line is
    /// invisible to the Tier-2 rig. That is why the switch moves the *level*
    /// and not the sentence: `.github/browser-rig/drive.py` scrapes three of
    /// these lines out of the console ring, and `raster_telemetry_line_tests`
    /// pins each scraped sentence against the rig's own regex for it, plus the
    /// key the rig has to seed to hear any of them.
    ///
    /// # What this costs the frame
    ///
    /// One monotonic clock read, and on all but one frame in
    /// [`RASTER_TELEMETRY_PERIOD`] nothing else — the ledgers are not even
    /// loaded. On the frame that is due and moved nothing: nine relaxed loads
    /// and a failed compare-exchange for the overlay ledger, and two or three
    /// relaxed loads plus one `u64` add and one compare for each of the other
    /// three, and no formatting, because every `*_if_moved` call answers
    /// `None`. Nothing here takes a lock. The
    /// increments themselves are one `fetch_add` each, at sites that run per
    /// dispatched raster, per arriving raster and per moved band — never in
    /// the per-pane-per-layer walks, which are the hot ones.
    fn report_raster_telemetry(&mut self) {
        let now = web_time::Instant::now();
        if !telemetry_is_due(self.raster_telemetry_said, now) {
            return;
        }
        let rasters = squallar_egui::overlay_cache::ledger::totals_if_moved();
        let uploads = self
            .state
            .as_mut()
            .and_then(|state| state.egui_renderer.upload_totals_if_moved());
        let strips = squallar_egui::floor_ledger::totals_if_moved();
        let ground = squallar_egui::tile_mesh::ledger::totals_if_moved();
        let basemap = squallar_egui::basemap_ledger::totals_if_moved();
        if rasters.is_none()
            && uploads.is_none()
            && strips.is_none()
            && ground.is_none()
            && basemap.is_none()
        {
            return;
        }
        // Stamped only where a line really went out, so a quiet pipeline does
        // not push the next reading a period further away every frame.
        self.raster_telemetry_said = Some(now);
        let loud = self.raster_telemetry_loud;
        if let Some(t) = rasters {
            say_telemetry(loud, &overlay_raster_line(&t));
        }
        if let Some(u) = uploads {
            say_telemetry(loud, &texture_upload_line(&u));
        }
        if let Some(s) = strips {
            say_telemetry(loud, &floor_strip_line(&s));
        }
        if let Some(g) = ground {
            say_telemetry(loud, &ground_tile_line(&g));
        }
        // Beside the ground line it explains, once per role that moved since
        // its last reading. Read here rather than in the early-return above:
        // a cache event on a drawing pane always moves the ground line too.
        for role in squallar_egui::tile_source::cache_ledger::ROLES {
            if let Some(t) = squallar_egui::tile_source::cache_ledger::totals_if_moved(role) {
                say_telemetry(loud, &tile_cache_line(role, &t));
            }
        }
        if let Some(b) = basemap {
            say_telemetry(loud, &basemap_tile_line(&b));
        }
    }

    /// Say the five frame timing lines, at most once per
    /// [`RASTER_TELEMETRY_PERIOD`] — the frame instrument shares the raster
    /// one's cadence deliberately, so one 2 s console poll hears both.
    ///
    /// Unlike the raster report there is no `*_if_moved` arm: this runs at
    /// the end of a frame, so the ledger has always moved since the last due
    /// tick, and printing every family every tick — the interact one at
    /// `n=0` included — is what lets a reader see "nobody touched the
    /// window" as a figure rather than as an absence.
    ///
    /// **How loud**: `debug` on an ordinary install, `info` where
    /// [`frame_telemetry_is_loud`] says this install asked — the same
    /// level-not-sentence switch as the raster lines, for the same
    /// browser-console reason.
    pub(super) fn report_frame_telemetry(&mut self) {
        let now = web_time::Instant::now();
        if !telemetry_is_due(self.frame_telemetry_said, now) {
            return;
        }
        self.frame_telemetry_said = Some(now);
        let loud = self.frame_telemetry_loud;
        // Taken before the shared borrow below, and taken rather than read:
        // the latch is cleared by this call, which is what scopes the figure
        // to one telemetry period. See `frame_ledger::WorstFrame`.
        let worst = self.frame_ledger.take_worst();
        let ledger = &self.frame_ledger;
        say_telemetry(
            loud,
            &frame_service_interact_line(ledger.service_interact()),
        );
        say_telemetry(loud, &frame_service_idle_line(ledger.service_idle()));
        say_telemetry(
            loud,
            &frame_segments_line(ledger.segments(), ledger.acquire()),
        );
        // The windowable spelling of the line above: same six segments, same
        // denominator, with the bins that make a gesture window a subtraction
        // rather than a percentile of the whole run.
        for line in frame_segment_lines(ledger.segments()) {
            say_telemetry(loud, &line);
        }
        // The `prepare` segment above, opened up at the seams the code has.
        // Same denominator, six contiguous cuts of that one span — a
        // decomposition of the line above, never a seventh segment beside it.
        for line in frame_prepare_lines(ledger.prepare_phases()) {
            say_telemetry(loud, &line);
        }
        // The `ui` segment above, opened up at the seams `Gui::ui` has. Same
        // denominator, six contiguous cuts of that one span — a decomposition
        // of the line above, never a seventh segment beside it.
        for line in frame_ui_lines(ledger.ui_phases()) {
            say_telemetry(loud, &line);
        }
        // The `pump` segment above, opened up at the seams `setup_egui_frame`
        // has. Same denominator, eight contiguous cuts of that one span — a
        // decomposition of the line above, never a ninth segment beside it.
        for line in frame_pump_lines(ledger.pump_phases()) {
            say_telemetry(loud, &line);
        }
        // The `post` segment above, opened up at the seams `handle_redraw`'s
        // tail has. Same denominator, six contiguous cuts of that one span — a
        // decomposition of the line above, never a seventh segment beside it.
        for line in frame_post_lines(ledger.post_phases()) {
            say_telemetry(loud, &line);
        }
        // The `finish` segment above, opened up at the seams `present_frame`
        // has — and on a WIDER denominator than every family above it: every
        // presented frame, not the interact ones alone. Its own parent rides
        // on `frame finish (whole)`; never add these to `frame segment
        // (finish)`. See `frame_finish_lines`.
        for line in frame_finish_lines(ledger.finish_phases()) {
            say_telemetry(loud, &line);
        }
        // One level below the seven above: which of the things the overlay
        // dispatch inlines is the one that costs. See `frame_dispatch_lines`.
        for line in frame_dispatch_lines(ledger.dispatch_cuts()) {
            say_telemetry(loud, &line);
        }
        // One frame, not a distribution: where the most expensive presented
        // frame of this period actually spent its time. Not added to any line
        // above — see `frame_worst_line`.
        say_telemetry(
            loud,
            &frame_worst_line(worst, ledger.worst_frame_since_boot()),
        );
        // What one tile take cost, per family. Read unconditionally rather
        // than through an `_if_moved` arm, for the same reason the frame
        // families are: this runs at the end of a frame and the window a
        // reader brackets has to have a reading at each end of it.
        for line in tile_take_lines(&squallar_egui::tile_source::take_ledger::totals()) {
            say_telemetry(loud, &line);
        }
        // One vector take opened up. Its two phases decompose `tile take
        // (vector)` and are never added to it or to each other's take family.
        for line in tile_phase_lines(&squallar_egui::tile_source::take_ledger::phase_totals()) {
            say_telemetry(loud, &line);
        }
        // Unconditional, where the two blocks above are filtered: this is what
        // makes a phase line's absence readable. See `tile_disposition_line`.
        say_telemetry(
            loud,
            &tile_disposition_line(&squallar_egui::tile_source::take_ledger::disposition()),
        );
        if let Some(state) = self.state.as_ref() {
            say_telemetry(loud, &prep_costs_line(&state.egui_renderer.pass_costs()));
            // Immediately after the clocks it divides into, and never added to
            // them: a byte total and a microsecond total on two denominators.
            say_telemetry(
                loud,
                &prep_geometry_line(
                    &state.egui_renderer.staged_geometry(),
                    &state.egui_renderer.geometry_staging_totals(),
                ),
            );
            // The command stream the pass tail replays. A count, beside the
            // clocks above and never added to them.
            say_telemetry(
                loud,
                &command_stream_line(
                    &state.egui_renderer.command_stream_last(),
                    &state.egui_renderer.command_stream(),
                ),
            );
            // The GPU pass family: real figures where a probe is installed,
            // the verbatim absence where this install asked (the probe is
            // built only for keyed-loud installs) and the adapter cannot
            // answer — every WebGL2 leg. A not-loud install has no probe by
            // choice and hears neither line: the absence sentence names a
            // missing FEATURE, and saying it beside a capable adapter would
            // be false prose.
            match state.egui_renderer.gpu_pass_report() {
                Some(report) => say_telemetry(loud, &gpu_passes_line(&report)),
                None if loud
                    && !state
                        .device
                        .features()
                        .contains(wgpu::Features::TIMESTAMP_QUERY) =>
                {
                    say_telemetry(loud, GPU_PASSES_UNAVAILABLE_LINE);
                }
                None => {}
            }
        }
        say_telemetry(loud, &frame_cadence_line(ledger.cadence()));
        say_telemetry(
            loud,
            &crate::loop_telemetry::loop_state_line(&self.loop_state()),
        );
        // The browser probe's figure, if it has landed since the last tick,
        // folded before the line below is composed so the tick it arrives on
        // prints `cap N 2` beside it. Nothing natively, nothing on WebGL2.
        self.poll_gpu_probe();
        // What the machine told the budget system, beside what the loops
        // hold — see [`crate::budget_telemetry`]. Composed once per tick and
        // parked for the diagnostics overlay's row on the way to the log, so
        // the panel and a captured log carry the same sentence. One heap
        // reading serves both the line and the watermark below it.
        let linear = self.platform.linear_memory();
        self.page_heap_reading = linear.or(self.page_heap_reading);
        // **The host pool, re-read rather than remembered.** What the OS will
        // give this process moves with every other program on the machine, so
        // a figure taken once at construction would be the high-water mark
        // this reading exists to replace. Read here, on the tick, and never on
        // the frame thread: on Linux and Android it is a `/proc/meminfo` read.
        // A reader that stops answering leaves the field unread rather than
        // stale, the way `adopt_gpu_capacity` does with the card's.
        self.device_profile.host_pool_bytes = crate::app::host_pool_reading(self.platform.as_ref());
        // **The readout is composed here, by the one thing that reads it**,
        // and after the heap reading above so its host spare is held to this
        // tick's room rather than the last one's. This is the whole of its
        // cadence: the frame path composes nothing
        // ([`Self::compose_budget_readout`]). The scene comes back so the
        // pane count below reads it instead of walking again.
        let scene = self.refresh_budget_readout();
        say_telemetry(
            loud,
            self.budget_state_panel_line
                .insert(crate::budget_telemetry::budget_state_line(
                    &self.budgets,
                    &self.device_profile,
                    linear,
                    self.loop_pool.bytes(),
                    self.loop_pool_state.allocation().balloon_bytes(),
                    &self.capacity(),
                    self.gpu_probe,
                    self.linear_memory_watch,
                    &self.budget_readout,
                    squallar_alloc::live_bytes(),
                )),
        );
        // What this frame's panes' overlay pictures are sized at, so a
        // harness reads the figure instead of modelling it — see
        // `crate::budget_telemetry::overlay_pictures_line`. Its own line, and
        // never appended to the one above: the two have different
        // denominators, and `budget state:` is scraped by a regex whose
        // groups are positional.
        say_telemetry(
            loud,
            &crate::budget_telemetry::overlay_pictures_line(
                &self.render.overlay_picture_sizes(scene.panes.len()),
                self.render.resident_overlay_pictures(),
                self.budgets.overlay_oversample_percent,
            ),
        );
        // **What is holding this instance's heap, family by family.** Its own
        // line, never appended to `budget state:` — that one is scraped by a
        // positional regex, and these figures have a different denominator
        // from every field on it: they are LEVELS of bytes resident now,
        // where `pool` and `ceiling` are allowances and `overlay rasters:`
        // are running totals. The census is also read by the
        // allocation-error hook, which prints the same line at the instant a
        // request is refused; this tick is the slow copy, and on a scene that
        // climbs hundreds of MB between two ticks the hook's is the one to
        // believe.
        self.publish_heap_census();
        say_telemetry(
            loud,
            &squallar_egui::heap_census::line(
                &squallar_egui::heap_census::census(),
                linear.map(|heap| heap.page_bytes),
                "page",
            ),
        );
        // The wasm heap watermarks, on the same tick. The bridge answers the
        // platform question: a native bridge reads no heap and neither arm
        // is entered there. The two instances are judged apart — each has
        // the same ceiling of its own, the two are never added, and only
        // the page's has levers — and the page's is also judged after every
        // picture arrival and every frame's tile puts (`observe_page_heap`),
        // so this tick is the worker's one reading and the page's slowest.
        if let Some(heap) = linear {
            self.observe_page_heap(heap.page_bytes, heap.page_max_bytes);
            if let Some(worker) = heap.worker_bytes {
                self.observe_worker_heap(worker, heap.worker_max_bytes);
            }
        }
    }

    /// **Publish every heap-census family this layer owns.**
    ///
    /// The census (`squallar_egui::heap_census`) is a set of always-on byte
    /// levels the allocation-error hook can read without allocating, so
    /// every publisher has to be a figure its owner already maintains. Each
    /// call here is a field read or a fold over a store bounded at a couple
    /// of dozen entries; nothing walks a volume, a raster or a grid. The
    /// families the UI layer owns publish themselves (the gridded overlays
    /// and the tile meshes from `Gui::ui_phased`, the tile caches from their
    /// own ledger, the pending uploads from the renderer's sweep), and the
    /// web layer publishes the loan book.
    ///
    /// **The radar families overlap and the census says so.** The loop
    /// cache, the still inventory, the derivation memo and the stored loop
    /// frames' hover sources hold `Arc`s of the same `Scan`s, so each reports
    /// what emptying it alone would free and their sum is an upper bound, not
    /// a partition.
    fn publish_heap_census(&mut self) {
        use squallar_egui::heap_census as census;

        census::set_loop_scan_bytes(self.loop_mgr.cached_scan_bytes() as u64);
        census::set_loop_l3_bytes(self.loop_mgr.cached_l3_bytes() as u64);
        census::set_still_scan_bytes(self.volumes.resident_scan_bytes() as u64);
        census::set_derive_memo_bytes(squallar_radar::derive::memo_bytes() as u64);
        census::set_render_cache_bytes(self.render.render_cache.resident_bytes() as u64);
        census::set_overlay_picture_bytes(self.render.resident_overlay_pictures().1);
        census::set_loop_frame_bytes(self.loop_frames.resident_host_bytes());
        census::set_loop_frame_scan_bytes(self.loop_frames.pinned_volume_bytes());
        census::set_volume_store_bytes(self.volume_store.memory_bytes() as u64);
        // Evicted and not yet freed: what `offload::discard` is holding for
        // the frame-paced drain. One `Cell` read; the queue prices itself at
        // enqueue and de-prices at drop.
        census::set_deferred_drop_bytes(squallar_worker::offload::deferred_drop_bytes());
    }

    /// **Judge one reading of the page's linear memory** against the line
    /// the scene sets — the wall less the next picture batch, never past
    /// the percentage line (`squallar_device_profile::linear_memory::act_line`)
    /// — and act on it: the host levers through [`Self::on_pressure`]. Called
    /// where the page allocates, after the overlay arrivals of a frame and
    /// after its tile puts, and on the telemetry tick; every call is one
    /// `byteLength` read and a few compares, and an action is bounded by
    /// what `on_pressure` does. The watch's re-fire step keeps a heap that
    /// has acted once from acting on every frame.
    ///
    /// `max` is **this instance's own wall**, carried in on the reading
    /// (`platform::LinearMemory::page_max_bytes`) rather than read off a
    /// constant: the page chooses its memory's maximum per device before the
    /// module is instantiated, so on a handheld the warn and act lines fall at
    /// 75 % and 87 % of a smaller number. A `max` of 0 is "nobody said", and
    /// `linear_memory_verdict` spells that `Quiet`.
    pub(super) fn observe_page_heap(&mut self, used: u64, max: u64) {
        use squallar_device_profile::linear_memory::LinearMemoryVerdict;

        match self
            .linear_memory_watch
            .observe(used, max, self.host_headroom_bytes)
        {
            LinearMemoryVerdict::Quiet => {}
            LinearMemoryVerdict::Warn => {
                log::info!("{}", crate::pressure::linear_memory_line(used, max));
            }
            LinearMemoryVerdict::Act => {
                self.on_pressure(crate::pressure::Pressure::LinearMemory { used, max });
            }
        }
    }

    /// **Judge one reading of the rasterization worker's linear memory**, by
    /// the percentage line alone: no scene of this application is priced on
    /// that heap and no lever reaches it, so an action there evicts economy
    /// and lowers no presumption ([`crate::pressure::Pressure::WorkerMemory`]).
    ///
    /// `max` is the **worker's** wall and not the page's. The two instances
    /// are chosen separately and on a handheld they differ: the worker holds
    /// jobs in flight rather than caches, so it is given less
    /// (`squallar-web/heap.js`). It arrives with the reading, from the
    /// worker's own hello.
    pub(super) fn observe_worker_heap(&mut self, used: u64, max: u64) {
        use squallar_device_profile::linear_memory::LinearMemoryVerdict;

        match self.worker_memory_watch.observe(used, max, 0) {
            LinearMemoryVerdict::Quiet => {}
            LinearMemoryVerdict::Warn => {
                log::info!("worker {}", crate::pressure::linear_memory_line(used, max));
            }
            LinearMemoryVerdict::Act => {
                self.on_pressure(crate::pressure::Pressure::WorkerMemory { used, max });
            }
        }
    }

    /// **Sample the page heap where a frame just allocated**: after the
    /// overlay arrivals — each one a picture copied into this heap — and
    /// after the Gui pass, which is where the tile pump puts its entries.
    /// Nothing on a bridge that reads no heap.
    pub(super) fn sample_page_heap(&mut self) {
        if let Some(heap) = self.platform.linear_memory() {
            self.page_heap_reading = Some(heap);
            self.observe_page_heap(heap.page_bytes, heap.page_max_bytes);
        }
    }

    /// One reading of what this application's loops hold — see
    /// [`crate::loop_telemetry`], which owns the counting and the sentence.
    ///
    /// **The pane walk is not here**, and that is deliberate rather than
    /// awkward: this file's reaches into the `Gui` sit on a permanent ceiling
    /// (`gui_seam_ratchet_tests`) that may only fall, so a new reading may not
    /// buy itself two new reaches. [`Self::loop_demand`] already walks every
    /// pane once a frame for the pool's sake; it counts these on the same walk
    /// and parks the result, and this assembles the halves that are the App's
    /// own — the allocation in force, the resolved budgets and the pool.
    fn loop_state(&self) -> crate::loop_telemetry::LoopState {
        let allocation = self.loop_allocation();
        crate::loop_telemetry::LoopState {
            allowed_plan: allocation.plan_view_frames,
            allowed_section: allocation.section_frames,
            allowed_volume: allocation.volume_frames,
            allowed_overlay: allocation.overlay_frames,
            share_bytes: allocation.share_bytes,
            cap: self.budgets.loop_render_budget,
            held: self.budgets.loop_frames_held,
            floor_bytes: self.budgets.loop_pool_floor_bytes,
            ceiling_bytes: self.budgets.loop_pool_ceiling_bytes,
            pool_bytes: self.loop_pool.bytes(),
            ..self.loop_counts
        }
    }

    /// Promote every held raster, as the frame after the last band lands does.
    #[cfg(test)]
    pub(super) fn deliver_held_rasters(&mut self) {
        self.gui.promote_held_rasters(|_| true);
    }

    /// Take the launch's one catalogue refresh and write it to the cache.
    fn poll_site_catalogue(&mut self) {
        while let Ok(response) = self.channels.site_catalogue_receiver.try_recv() {
            // A failed fetch is silent by design — offline is not an error
            // state here, it is a launch that runs on the cache. `catalogue`
            // has already logged the reason at `debug`.
            let Some(fetched) = response.catalogue else {
                continue;
            };
            let store = self.platform.kv();
            crate::site_catalogue::store_if_changed(
                store.as_deref(),
                &self.site_catalogue,
                &fetched,
            );
            self.site_catalogue = fetched;
            if self.catalogue_pending {
                self.adopt_the_first_catalogue();
            }
            if self.site_hint_pending {
                self.open_on_the_timezones_radar();
            }
        }
    }

    /// Put the first catalogue this install ever fetched into the live table.
    fn adopt_the_first_catalogue(&mut self) {
        // The picker learns the list is whole through the per-frame compose.
        self.catalogue_pending = false;
        let table = squallar_radar::sites::resolve(
            self.site_positions
                .fixes()
                .chain(self.site_catalogue.fixes()),
        );
        log::info!(
            "first catalogue applied in-session: {} radars placed, {} listed \
             without a position",
            table.rows().len(),
            table.unplaced().len(),
        );
        // The site layer draws from its own copy of the table, so a catalogue
        // that places radars mid-session has to be handed over again or the
        // map keeps drawing the list this install booted with.
        self.gui.publish_radar_sites();
        // ...and the volumes already on screen were named against the table
        // this call just replaced. One decoded before its radar was in it
        // carries UNKNOWN, and `dispatch_pane_renders` looks the volume up
        // under that name in a still store keyed by the site -- so the render
        // was skipped, silently, and nothing else revisits it. This arrival is
        // what un-skips it, which is a re-trigger and not a retry.
        let replaced = self.gui.place_shown_volumes_against_the_table();
        if replaced > 0 {
            log::info!(
                "the catalogue placed {replaced} radar(s) whose volume was \
                 already in hand; those panes can be drawn after all",
            );
        }
    }

    /// Open on the radar nearest this device's timezone.
    fn open_on_the_timezones_radar(&mut self) {
        self.site_hint_pending = false;
        // The hint is run here rather than remembered from startup, because at
        // startup it had nothing to resolve against and chose nothing.
        let Some(zone) = self.platform.iana_timezone() else {
            return;
        };
        let Some(site) = crate::location_hint::site_for_timezone(&zone) else {
            return;
        };
        // Still a guess either way, so a later location fix may refine it.
        self.site_is_provisional = true;
        // The pane asked has to be the pane opened: a hint that checks pane 0 and switches
        // the active pane skips the move whenever those differ, and the hint is already
        // spent by then.
        let pane_idx = self.gui.active_pane_idx();
        if self
            .gui
            .pane(pane_idx)
            .is_some_and(|pane| pane.site() == site)
        {
            return;
        }
        log::info!("opening on {site}, nearest to timezone {zone}");
        self.handle_gui_action(
            crate::app::GuiAction::SwitchRadarSite {
                site: site.to_string(),
                pane_idx,
            },
            None,
        );
    }

    /// Poll for completed Level III fetch results and update scan info.
    fn poll_level3_results(&mut self) {
        while let Ok(sounding) = self.channels.sounding_receiver.try_recv() {
            if self
                .render
                .is_fetch_stale(&sounding.site, sounding.generation)
            {
                continue;
            }
            // A failed fetch keeps the previous entry: a stale environment
            // beats none, and the TTL gate in `spawn_level3_fetches` retries
            // on the next poll precisely because nothing fresh landed here.
            let Some(heights) = sounding.heights else {
                log::warn!("Sounding fetch failed for {}", sounding.site);
                continue;
            };
            log::info!(
                "Env heights cached for {}: 0C {:.2} km, -20C {:.2} km MSL",
                sounding.site,
                heights.h0c_km_msl,
                heights.hm20c_km_msl
            );
            if self
                .render
                .set_env_heights(&sounding.site, heights, &self.gui)
            {
                log::info!(
                    "Env heights moved for {}: dropped the renders that read them",
                    sounding.site
                );
            }
        }
        while let Ok(ml) = self.channels.melting_layer_receiver.try_recv() {
            if self.render.is_fetch_stale(&ml.site, ml.generation) {
                continue;
            }
            let Some(bytes) = ml.object else {
                continue;
            };
            log::info!(
                "Melting layer cached for {} (volume {}, {} bytes)",
                ml.site,
                ml.volume_start,
                bytes.len()
            );
            if self.render.set_melting_layer(
                &ml.site,
                crate::render_dispatch::MeltingLayerObject {
                    volume_start: ml.volume_start,
                    bytes,
                },
                &self.gui,
            ) {
                log::info!(
                    "Melting layer moved for {}: dropped the classification renders",
                    ml.site
                );
            }
        }
        while let Ok(sm) = self.channels.storm_motion_receiver.try_recv() {
            if self.render.is_fetch_stale(&sm.site, sm.generation) {
                continue;
            }
            let Some((speed_kt, direction_deg)) = sm.motion else {
                continue;
            };
            log::info!(
                "Storm motion cached for {} (volume {}): {speed_kt:.1} kt from {direction_deg:.1}°",
                sm.site,
                sm.volume_start,
            );
            if self.render.set_storm_motion(
                &sm.site,
                crate::render_dispatch::StormMotionObject {
                    volume_start: sm.volume_start,
                    motion: (speed_kt, direction_deg),
                },
                &self.gui,
            ) {
                log::info!(
                    "Storm motion moved for {}: dropped the storm-relative renders",
                    sm.site
                );
            }
        }
        while let Ok(l3_resp) = self.channels.level3_receiver.try_recv() {
            if self
                .render
                .is_fetch_stale(&l3_resp.site, l3_resp.generation)
            {
                log::debug!(
                    "Discarding stale Level III result for {} (gen {})",
                    l3_resp.site,
                    l3_resp.generation
                );
                continue;
            }

            let fetched = match l3_resp.result {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("Level III {} fetch failed: {}", l3_resp.code, e);
                    continue;
                }
            };

            let readers = squallar_radar::types::RadarProduct::level3_readers(&l3_resp.code);
            let elevation = fetched.message.pdb.elevation_angle();
            // The age is logged, not just carried: `latest_key` falls back to the
            // previous UTC day, so a site down since yesterday delivers a product
            // up to ~48 h old and this is currently the only place that says so.
            log::info!(
                "Level III {} fetched successfully for {:?} (elevation={:.1}°, key={}, age={:?} min)",
                l3_resp.code,
                readers.iter().map(|p| p.name()).collect::<Vec<_>>(),
                elevation,
                fetched.stamp.key,
                fetched
                    .age(chrono::Utc::now().naive_utc())
                    .map(|a| a.num_minutes()),
            );
            self.render
                .cache_level3(l3_resp.code.clone(), l3_resp.site.clone(), fetched);

            // Trigger a re-render for panes on the same site showing anything this
            // object feeds.
            for (idx, prs) in self.render.pane_render.iter_mut().enumerate() {
                let pane_matches_site =
                    self.gui.pane(idx).is_some_and(|p| p.site() == l3_resp.site);
                if pane_matches_site
                    && self
                        .gui
                        .get_rendering_params_for_pane(idx)
                        .and_then(|(id, _)| crate::render_key::radar_field(&id))
                        .is_some_and(|p| readers.contains(&p))
                {
                    prs.last_rendered = None;
                }
            }

            // Add Level III products to the scan info for panes on this site
            for pane_idx in 0..self.gui.pane_count() {
                let pane_site = self
                    .gui
                    .pane(pane_idx)
                    .map(|p| p.site().to_string())
                    .unwrap_or_default();
                if pane_site != l3_resp.site {
                    continue;
                }
                let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                    continue;
                };
                let mut info = scan_info.clone();
                let mut changed = false;
                for &product in &readers {
                    if !info.available_products.contains(&product) {
                        info.available_products.push(product);
                        info.available_products.sort_by_key(|p| p.sort_order());
                        info.status = format!(
                            "Loaded {} products: {}",
                            info.available_products.len(),
                            info.available_products
                                .iter()
                                .map(|p| p.name())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        changed = true;
                    }
                    // Register the actual elevation angle from the PDB.
                    let elevations = info.product_elevations.entry(product).or_default();
                    let rounded_elev = (elevation * 10.0).round() / 10.0;
                    if !elevations.iter().any(|e| (e - rounded_elev).abs() < 0.05) {
                        elevations.push(rounded_elev);
                        elevations.sort_by(|a, b| a.total_cmp(b));
                        changed = true;
                    }
                }
                if changed {
                    self.gui
                        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
                            pane_idx,
                            info,
                        });
                }
            }
        }
    }

    /// Poll for completed overlay rasterization results and upload textures.
    fn poll_overlay_render_results(&mut self, ctx: &egui::Context) {
        use squallar_egui::overlay_cache::OverlayTextureData;

        let mut arrived = 0usize;
        while let Ok(mut resp) = self.channels.overlay_render_receiver.try_recv() {
            arrived += 1;
            let id = resp.overlay_kind.clone();

            // **One loop frame's raster, filed on the frame that asked** — the
            // whole of the WI-6b arrival, taken before the live path's
            // bookkeeping because none of it applies: the in-flight mark this
            // answers is the frame's, not the pane's, and this raster is not a
            // candidate for the pane's overlay cache at all.
            if let Some(stamp) = resp.frame {
                self.file_overlay_loop_frame(ctx, &id, stamp, resp);
                continue;
            }

            // **Counted here and not at the top of the loop**, so that
            // `ledger::Totals::arrivals_balance` is an identity rather than a
            // hope: a loop frame takes the arm above and is neither a picture
            // in a pane's overlay cache nor a drop from one, so counting it as
            // an arrival would make the balance false for a reason that is not
            // a defect. Everything past this line ends in exactly one of
            // `note_picture` or `note_dropped`.
            squallar_egui::overlay_cache::ledger::note_arrived();

            // The dispatch this answers, rebuilt from the two terms the
            // response echoes — see `RenderTicket`. A pane whose cache is no
            // longer waiting for *this* raster does not get it.
            let ticket =
                squallar_egui::overlay_cache::RenderTicket::whole(resp.generation, resp.geo_bounds);

            // Narrow the result to the panes that still draw this layer, and do
            // it **before the upload**.
            let gui = &mut self.gui;
            resp.pane_indices.retain(|&pane_idx| {
                let Some(pane) = gui.pane_mut(pane_idx) else {
                    return false;
                };
                let wanted = !pane.overlay_texture_is_releasable(&id);
                // **Retired for every pane, kept for some.** The mark is this
                // pane's bookkeeping and has to come off whether or not the
                // picture is wanted; `still_asked` is the separate question of
                // whether the cache is waiting for this very dispatch. A
                // `false` there is a raster for a viewport the pane has moved
                // past, or one whose mark was abandoned while it flew: it is
                // dropped here, before the upload below, rather than held.
                let still_asked = pane.overlay_cache_mut(&id).renders.retire(&ticket);
                wanted && still_asked
            });
            if resp.pane_indices.is_empty() {
                // Rasterized and thrown away: every pane had moved past this
                // dispatch or stopped drawing the layer. The bytes never reach
                // the GPU, which is what makes this worth counting separately
                // from the ones that do.
                squallar_egui::overlay_cache::ledger::note_dropped();
                continue;
            }

            let Some(answer) = resp.picture.take() else {
                // A render that produced no picture. Still an arrival, and
                // still not a picture, so it is a drop for the balance's
                // purposes.
                squallar_egui::overlay_cache::ledger::note_dropped();
                continue;
            };

            // **The two answers part here, and only one of them costs a
            // texture.** `Blank` is a raster that painted nothing: no buffer
            // was built for it on the offload thread, so there is nothing to
            // upload and nothing to clone to the panes below — what crosses is
            // the shape it would have had. It is still a picture for the
            // ledger's balance, and it carries no bytes, which is the whole of
            // the saving stated as a figure.
            let painted = match &answer {
                crate::channels::OverlayPicture::Painted(image) => {
                    // Load texture once, then clone handle to all target panes.
                    self.texture_counter += 1;
                    let tex_name = format!("overlay_{}", self.texture_counter);
                    let texture =
                        ctx.load_texture(tex_name, Arc::clone(image), egui::TextureOptions::LINEAR);
                    // **Once per response, never once per pane.** This one
                    // handle is cloned to every pane below, so the pixels
                    // cross once however many panes asked; counting inside
                    // that loop would multiply the byte figure by the pane
                    // count. `as_raw()` is the picture's own buffer, so this
                    // is the real size rather than `w * h * 4` restated.
                    squallar_egui::overlay_cache::ledger::note_picture(
                        image.as_raw().len() as u64,
                        true,
                    );
                    Some(texture)
                }
                crate::channels::OverlayPicture::Blank { .. } => {
                    // Zero bytes, and it is not an estimate: no `ColorImage`
                    // was allocated, no texture was minted and nothing was
                    // uploaded. `pictures - inked` is still the blank count.
                    squallar_egui::overlay_cache::ledger::note_picture(0, false);
                    None
                }
            };

            // The picture's own dimensions rather than a pair carried beside
            // it — and the blank's are the plan's, recorded by the deliver
            // that decided not to build it.
            let (width, height) = match &answer {
                crate::channels::OverlayPicture::Painted(image) => {
                    let [width, height] = image.size;
                    (width as u32, height as u32)
                }
                crate::channels::OverlayPicture::Blank { width, height } => (*width, *height),
            };

            // Every pane still named here wants the answer: the retain above is
            // what decided that, and it also cleared every in-flight mark.
            for &pane_idx in &resp.pane_indices {
                let Some(pane) = self.gui.pane_mut(pane_idx) else {
                    continue;
                };

                let cache = pane.overlay_cache_mut(&id);
                let shape = squallar_egui::overlay_cache::PictureShape {
                    placed: squallar_geo::PlacedRaster::of(resp.geo_bounds),
                    data_generation: resp.generation,
                    render_zoom: resp.zoom,
                    width,
                    height,
                };

                let Some(texture) = &painted else {
                    // **The clear**, and it lands whatever the pane was
                    // drawing: a blank that only applied to an empty cache
                    // would leave the ink of a layer whose data has gone away
                    // on the glass for ever. Counted as a supersede on the
                    // same terms `hold` is, because it throws away the same
                    // started upload.
                    if cache.is_holding() {
                        squallar_egui::overlay_cache::ledger::note_superseded();
                    }
                    cache.show_blank(shape);
                    continue;
                };

                let data = OverlayTextureData {
                    texture: texture.clone(),
                    placed: shape.placed,
                    data_generation: shape.data_generation,
                    render_zoom: shape.render_zoom,
                    width,
                    height,
                    radar_meta: None,
                    hit_map: resp.hit_map.clone(),
                };
                if cache.current().is_none() {
                    squallar_egui::overlay_cache::ledger::note_shown();
                    cache.show(data);
                } else {
                    // A hold replaces rather than queues, so handing one to a
                    // cache that is already holding throws away an upload that
                    // had started. Asked before the call because `hold` is what
                    // clears the answer.
                    if cache.is_holding() {
                        squallar_egui::overlay_cache::ledger::note_superseded();
                    }
                    cache.hold(data, None);
                }
            }
        }
        // Every arrival above was a picture copied into this heap; one
        // reading now, where it grew, rather than on the tick two seconds
        // hence. Nothing natively.
        if arrived > 0 {
            self.sample_page_heap();
        }
    }

    /// **File one loop frame's finished raster** (WI-6b).
    ///
    /// **Found by stamp, never by the index it was dispatched at.** A render is
    /// in the air for frames at a time, and the list underneath it can be
    /// re-listed or re-sampled (`cap_frames`) while it flies; the index would
    /// then name a different instant. Two frames never share a timestamp, so
    /// the stamp is the identity that survives — see
    /// `LayerTimeState::frame_at_stamp_mut`.
    ///
    /// **One upload per response, and that is the picture's identity.** Two
    /// frames at two stamps are two responses carrying two `ColorImage`s and
    /// therefore two distinct `TextureHandle`s. Nothing here can hand two
    /// frames the same handle, which is the last thing
    /// `a_forecast_loops_frames_each_get_their_own_picture_end_to_end` asserts.
    ///
    /// A response with no picture retires the frame the same way radar's does:
    /// `render_failed` is terminal, and it is what lets `render_set_settled`
    /// promote a loop out of `Rendering` instead of hanging there for a frame
    /// that will never arrive.
    fn file_overlay_loop_frame(
        &mut self,
        ctx: &egui::Context,
        id: &squallar_source::id::LayerId,
        stamp: squallar_source::time::FrameStamp,
        resp: crate::channels::OverlayRenderResponse,
    ) {
        let uploaded = match resp.picture {
            Some(crate::channels::OverlayPicture::Painted(image)) => {
                self.texture_counter += 1;
                let [width, height] = image.size;
                let name = format!("overlay_loop_{}", self.texture_counter);
                let texture = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
                Some((texture, width as u32, height as u32))
            }
            // **`Blank` cannot reach here** and is not silently equated with a
            // failure: `overlay_job_deliver` builds a picture for every loop
            // frame however empty it is, because a frame has no state for "draws
            // nothing" — see the note there. If that ever changes, a frame with
            // no picture is terminal, which is the answer a picture-less
            // response has always had.
            Some(crate::channels::OverlayPicture::Blank { .. }) | None => None,
        };
        for pane_idx in resp.pane_indices {
            let Some(pane) = self.gui.pane_mut(pane_idx) else {
                continue;
            };
            let Some(frame) = pane.time_state_mut(id).frame_at_stamp_mut(stamp.valid) else {
                // The loop was rebuilt or torn down while this flew. Nothing to
                // file it to, and nothing to un-mark either.
                continue;
            };
            frame.render_in_flight = false;
            match &uploaded {
                Some((texture, width, height)) => {
                    frame.image = Some(squallar_egui::pane::LoopFrameImage::Overlay(
                        squallar_egui::overlay_cache::OverlayTextureData {
                            texture: texture.clone(),
                            placed: squallar_geo::PlacedRaster::of(resp.geo_bounds),
                            data_generation: resp.generation,
                            render_zoom: resp.zoom,
                            width: *width,
                            height: *height,
                            // Radar's hover payload and radar's hit map: an
                            // overlay loop frame is a picture and nothing else.
                            // Hovers are answered by the live layer state,
                            // which is not what a frame holds.
                            radar_meta: None,
                            hit_map: None,
                        },
                    ));
                }
                None => frame.render_failed = true,
            }
        }
    }

    /// Apply the storm motion override the settings panel holds, and if it
    /// moved, invalidate everything derived with the old vector.
    fn apply_storm_motion_override(&mut self) -> bool {
        if self.gui.storm_motion_mid_edit() {
            return false;
        }
        // Editing the vector changes nothing else about a pane, so the derived
        // storm-relative tilts have to be invalidated explicitly.
        let storm_motion = self.gui.storm_motion_override.sample();
        if !self
            .render
            .set_storm_motion_choice(storm_motion, self.gui.srv_fallback)
        {
            return false;
        }
        self.volume_store
            .evict_product(&squallar_radar::fields::known::STORM_RELATIVE_VELOCITY);
        for pane in self.gui.panes_mut() {
            if pane.selected_product() != squallar_radar::fields::known::STORM_RELATIVE_VELOCITY {
                continue;
            }
            if let Some(volume) = pane.volume_mut() {
                volume.rendered_for = None;
            }
            if let Some(section) = pane.cross_section_mut() {
                section.rendered_for = None;
            }
        }
        true
    }

    /// Move `RenderInput::extract` to volume arrival for `site`:
    pub(super) fn refresh_extract_cache_for_site(&mut self, site: &str) {
        self.render.retain_extracts(|key| key.site != site);
        for pane in self.gui.panes() {
            if !pane.is_map() || pane.site() != site {
                continue;
            }
            let Some((product, elevation)) = pane
                .get_rendering_params()
                .and_then(|(id, e)| Some((squallar_radar::fields::product_for(&id)?, e)))
            else {
                continue;
            };
            // Level III renders from fetched objects, not from the volume —
            // there is no extraction to move.
            if product.is_level3() {
                continue;
            }
            let Some(scan_info) = pane.scan_info.as_ref() else {
                continue;
            };
            // The same stores and the same names the dispatch reads: the
            // extract key is the pane's site, the volume is looked up under
            // the scan_info's site *and its moment*, and the coordinates are
            // the scan_info's.
            let Some((data, _declared)) = self
                .volumes
                .still_for(scan_info.site.name, scan_info.timestamp)
            else {
                continue;
            };
            let (lat, lon) = (scan_info.site.lat, scan_info.site.lon);
            let (key, storm_motion, env_heights) =
                self.render
                    .extract_tuple_for(site, scan_info.timestamp, product, elevation);
            #[cfg(not(target_arch = "wasm32"))]
            {
                let sender = self.render.extract_sender();
                let window = self.window.clone();
                self.tokio_runtime.spawn_blocking(move || {
                    if let Some(input) = squallar_radar::render_input::RenderInput::extract(
                        &data,
                        elevation,
                        product,
                        lat,
                        lon,
                        storm_motion,
                        env_heights,
                    ) {
                        let _ = sender.send((key, std::sync::Arc::new(input)));
                        // Wake the pump so the Apply row drains this before
                        // the next dispatch rather than on some later event.
                        crate::app::notify_redraw(&window);
                    }
                });
            }
            #[cfg(target_arch = "wasm32")]
            {
                if let Some(input) = squallar_radar::render_input::RenderInput::extract(
                    &data,
                    elevation,
                    product,
                    lat,
                    lon,
                    storm_motion,
                    env_heights,
                ) {
                    self.render
                        .populate_extract(key, std::sync::Arc::new(input));
                }
            }
        }
    }

    /// Check all panes for needed background renders and spawn render threads.
    fn dispatch_pane_renders(&mut self, ctx: &egui::Context) {
        self.apply_storm_motion_override();
        let mut uploads = PlanViewUploads::default();
        for pane_idx in 0..self.gui.pane_count() {
            if self.gui.pane_has_no_plan_view(pane_idx) {
                continue;
            }
            if let Some((product, elevation)) = self
                .gui
                .get_rendering_params_for_pane(pane_idx)
                .and_then(|(id, e)| Some((squallar_radar::fields::product_for(&id)?, e)))
            {
                let prs = &self.render.pane_render[pane_idx];
                let needs_render = prs
                    .last_rendered
                    .map(|(last_prod, last_elev)| {
                        last_prod != product || (last_elev - elevation).abs() > ELEVATION_TOLERANCE
                    })
                    .unwrap_or(true);

                if needs_render && !prs.render_in_flight() {
                    let Some(pane) = self.gui.pane(pane_idx) else {
                        continue;
                    };
                    let pane_site = pane.site().to_string();

                    if let Some(cached) = self.render.get_cached_render(
                        &pane_site,
                        product,
                        squallar_radar::types::RenderView::PlanView,
                        elevation,
                    ) {
                        let render_result = crate::render_dispatch::CachedPaneRender {
                            image: Arc::clone(&cached.image),
                            max_range_km: cached.max_range_km,
                            hover: Arc::clone(&cached.hover),
                            product,
                            elevation,
                            nyquist_ms: cached.nyquist_ms,
                            melting_layer_source: cached.melting_layer_source,
                            storm_motion: cached.storm_motion,
                        };
                        log::info!(
                            "Reusing cached render for pane {}: {:?} at {:.1}°",
                            pane_idx,
                            product,
                            elevation
                        );
                        self.apply_render_to_pane(ctx, pane_idx, &render_result, &mut uploads);
                        continue;
                    }

                    // A sibling pane is already having this exact picture made.
                    if self
                        .render
                        .plan_view_in_flight(&pane_site, product, elevation)
                    {
                        continue;
                    }

                    let Some(scan_info) = pane.scan_info.as_ref() else {
                        continue;
                    };

                    let params = crate::render_dispatch::RenderParams {
                        product,
                        elevation,
                        lat: scan_info.site.lat,
                        lon: scan_info.site.lon,
                    };

                    if product.is_level3() {
                        self.render.try_spawn_level3_render(
                            pane_idx,
                            &params,
                            &pane_site,
                            self.channels.render_sender.clone(),
                            self.window.clone(),
                        );
                    } else if let Some((data, declared)) = self
                        .volumes
                        .still_for(scan_info.site.name, scan_info.timestamp)
                    {
                        // Handed back as refcounts, so the dispatcher below can be
                        // borrowed mutably in the same statement.
                        self.render.spawn_level2_render(
                            pane_idx,
                            &params,
                            &pane_site,
                            data,
                            &declared,
                            scan_info.timestamp,
                            self.channels.render_sender.clone(),
                            self.window.clone(),
                        );
                    }
                }
            } else if pane_idx < self.render.pane_render.len() {
                // Only clear the radar texture if no scan data is loaded for this pane.
                let has_scan = self
                    .gui
                    .pane(pane_idx)
                    .is_some_and(|p| p.scan_info.is_some());
                if !has_scan && let Some(pane) = self.gui.pane_mut(pane_idx) {
                    let cache = pane.overlay_cache_mut(&squallar_source::id::known::RADAR);
                    cache.clear();
                }
                self.render.pane_render[pane_idx].last_rendered = None;
            }
        }
    }

    /// Cut a fresh cross-section for every section pane whose picture no longer
    /// matches what it is aimed at.
    fn dispatch_section_renders(&mut self) {
        for pane_idx in 0..self.gui.pane_count() {
            let Some(target) = self.section_target_for_pane(pane_idx) else {
                continue;
            };
            let Some(pane) = self.gui.pane(pane_idx) else {
                continue;
            };
            let Some(section) = pane.cross_section() else {
                continue;
            };
            if section.rendered_for.as_ref() == Some(&target) {
                continue;
            }
            if self
                .render
                .pane_render
                .get(pane_idx)
                .is_some_and(|p| p.render_in_flight())
            {
                continue;
            }

            let site = target.volume.site.clone();
            let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                continue;
            };
            let (lat, lon) = (scan_info.site.lat, scan_info.site.lon);

            let base = self.volumes.base_for(site.as_str());
            let overlay = self.chunk_feeds.snapshot(site.as_str());

            if let Some(reason) = section_source_refusal(
                base.as_ref().map(|(scan, _)| scan.as_ref()),
                overlay.as_ref().map(|live| live.scan.as_ref()),
            ) {
                self.mark_section_unavailable(pane_idx, reason);
                continue;
            }
            if crate::render_key::radar_field(&target.product)
                .and_then(squallar_radar::derive::volume_slot)
                .is_none()
            {
                // Permanent for this product, so the key *is* written: nothing
                // about this volume will make a column integral sliceable, and
                // re-asking every frame would be a busy loop with no output.
                self.mark_section_unavailable(
                    pane_idx,
                    squallar_egui::pane::SectionUnavailable::ProductHasNoVerticalStructure(
                        target.product.clone(),
                    ),
                );
                if let Some(section) = self
                    .gui
                    .pane_mut(pane_idx)
                    .and_then(|p| p.cross_section_mut())
                {
                    section.rendered_for = Some(target);
                }
                continue;
            }

            // The extraction is radar's own and keyed by radar's field; the
            // target names it by id, and the arm above already refused an id
            // with no vertical slot.
            let Some(product) = crate::render_key::radar_field(&target.product) else {
                continue;
            };
            // Captured before the closure: the user's storm motion vector,
            // for the worker-side SRV derivation. The extraction keeps it
            // only on an SRV payload.
            let motion = self.render.storm_motion_override_kt();
            // Read here, on the frame thread, for the reason `motion` above it
            // is: the closure runs later, and a rung read inside it could be a
            // different one from the rung the key was built with.
            let fallback = self.render.srv_fallback();
            let extract = move || {
                let current = squallar_radar::current::resolve(
                    base.as_ref().map(|(scan, declared)| {
                        squallar_radar::nyquist::Volume::new(scan, declared)
                    }),
                    overlay.as_ref().map(|live| {
                        squallar_radar::nyquist::Volume::new(&live.scan, &live.declared)
                    }),
                )?;
                squallar_radar::render_input::RenderInput::extract_volume_parts(
                    current.pattern(),
                    current.sweeps(),
                    product,
                    lat,
                    lon,
                    motion,
                )
                // The same stamp `App::extract_current_volume` applies, and for
                // the same reason: without it this payload's worker estimates
                // the velocity fold limits the merge just declared.
                .map(|input| {
                    input
                        .with_declared_nyquist(current.declared_nyquist())
                        .with_srv_fallback(fallback)
                })
            };
            match self.render.spawn_section_render(
                pane_idx,
                &target,
                extract,
                self.channels.section_sender.clone(),
                self.window.clone(),
            ) {
                // Nothing taken, nothing said: the budget frees up on its own
                // and the pane asks again next frame.
                crate::render_dispatch::SectionDispatch::Busy => {}
                crate::render_dispatch::SectionDispatch::NoPayload => {
                    // This volume carries nothing to cut under this product.
                    self.mark_section_unavailable(
                        pane_idx,
                        squallar_egui::pane::SectionUnavailable::ProductMissingFromVolume(
                            target.product.clone(),
                        ),
                    );
                    if let Some(section) = self
                        .gui
                        .pane_mut(pane_idx)
                        .and_then(|p| p.cross_section_mut())
                    {
                        section.rendered_for = Some(target);
                    }
                }
                crate::render_dispatch::SectionDispatch::Dispatched => {
                    if let Some(section) = self
                        .gui
                        .pane_mut(pane_idx)
                        .and_then(|p| p.cross_section_mut())
                    {
                        section.rendered_for = Some(target);
                        section.unavailable = None;
                    }
                }
            }
        }
    }

    /// What pane `pane_idx` would have to cut to be showing the truth, or `None`
    /// if it is not a section pane, has no line, or has no volume yet.
    fn section_target_for_pane(
        &mut self,
        pane_idx: usize,
    ) -> Option<squallar_egui::pane::SectionTarget> {
        let pane = self.gui.pane(pane_idx)?;
        let section = pane.cross_section()?;
        let line = section.line?;
        let product = squallar_radar::fields::product_for(&pane.selected_product())?;
        let site = pane.site().to_string();
        let Some(collected) = pane.scan_info.as_ref().map(|s| s.timestamp) else {
            self.mark_section_unavailable(
                pane_idx,
                squallar_egui::pane::SectionUnavailable::AwaitingVolume,
            );
            return None;
        };
        let ladder = self
            .current_ladder_fingerprint(site.as_str(), product)
            .unwrap_or(0);
        Some(squallar_egui::pane::SectionTarget {
            volume: squallar_egui::pane::VolumeStamp { site, collected },
            product: crate::render_key::field_id_of(product),
            line,
            ladder,
        })
    }

    /// Record why a section pane has no picture, leaving whatever it is showing
    /// alone.
    fn mark_section_unavailable(
        &mut self,
        pane_idx: usize,
        reason: squallar_egui::pane::SectionUnavailable,
    ) {
        if let Some(section) = self
            .gui
            .pane_mut(pane_idx)
            .and_then(|p| p.cross_section_mut())
        {
            section.unavailable = Some(reason);
        }
    }

    /// Take delivery of finished cross-sections and upload their rasters.
    fn poll_section_results(&mut self, ctx: &egui::Context) {
        while let Ok(sr) = self.channels.section_receiver.try_recv() {
            if let Some(state) = self.render.pane_render.get_mut(sr.pane_idx) {
                state.render_finished();
            }

            if self.render.is_render_stale(sr.generation) {
                if let Some(section) = self
                    .gui
                    .pane_mut(sr.pane_idx)
                    .and_then(|p| p.cross_section_mut())
                {
                    section.rendered_for = None;
                }
                continue;
            }

            let Some(section_state) = self
                .gui
                .pane_mut(sr.pane_idx)
                .and_then(|p| p.cross_section_mut())
            else {
                continue;
            };
            // The pane has been re-aimed, converted or re-sited while this cut
            // was in the air. Dropped without touching the key: whatever the
            // pane is waiting for now is still on its way.
            if section_state.rendered_for.as_ref() != Some(&sr.target) {
                continue;
            }

            let Some(cut) = sr.section else {
                section_state.unavailable =
                    Some(squallar_egui::pane::SectionUnavailable::RenderFailed);
                continue;
            };

            let texture = self.upload_section_raster(ctx, &cut);

            let Some(section_state) = self
                .gui
                .pane_mut(sr.pane_idx)
                .and_then(|p| p.cross_section_mut())
            else {
                continue;
            };
            // Assigning retires the cut this pane was showing; see the note in
            // `App::apply_render_to_pane`.
            section_state.texture = Some(texture);
            section_state.section = Some(Arc::from(cut));
            section_state.unavailable = None;
        }
    }

    /// Upload a cut's raster and hand back the handle. The **one** place a
    /// section becomes a texture.
    fn upload_section_raster(
        &mut self,
        ctx: &egui::Context,
        cut: &squallar_radar::xsect::CrossSection,
    ) -> egui::TextureHandle {
        self.texture_counter += 1;
        let color_image = egui::ColorImage::from_rgba_premultiplied(
            [
                squallar_radar::xsect::SECTION_WIDTH,
                squallar_radar::xsect::SECTION_HEIGHT,
            ],
            cut.image(),
        );
        ctx.load_texture(
            format!("cross_section_{}", self.texture_counter),
            color_image,
            egui::TextureOptions::NEAREST,
        )
    }

    /// Put every section pane's raster back on the GPU, from the
    /// [`CrossSection`](squallar_radar::xsect::CrossSection) the pane still
    /// holds.
    fn restore_section_textures(&mut self, ctx: &egui::Context) {
        for pane_idx in 0..self.gui.remembered_pane_count() {
            let Some(cut) = self
                .gui
                .pane(pane_idx)
                .and_then(|pane| pane.cross_section())
                // A pane that still has its handle was not released, so
                // re-uploading would leak the live one it is drawing with.
                .filter(|section| section.texture.is_none())
                .and_then(|section| section.section.clone())
            else {
                continue;
            };
            let texture = self.upload_section_raster(ctx, &cut);
            if let Some(section) = self
                .gui
                .pane_mut(pane_idx)
                .and_then(|p| p.cross_section_mut())
            {
                section.texture = Some(texture);
            }
        }
    }

    /// The largest side this restore would hand egui.
    ///
    /// Plan views only, and that is the whole set rather than a shortcut: a
    /// cross-section is `SECTION_WIDTH` by half of it — 2048 native, 1024 on
    /// wasm — and 2048 is the *smallest* limit any `egui::Context` reports,
    /// the `InputState::default` one. A section can therefore never be the
    /// raster that does not fit. Pinned by
    /// `a_cross_section_can_never_be_the_raster_that_does_not_fit`, which goes
    /// red if a section ever grows past that floor and this has to widen.
    fn widest_raster_to_restore(&self) -> usize {
        self.render
            .pane_render
            .iter()
            .filter_map(|prs| prs.cached_render.as_ref())
            .map(|cached| cached.image.width().max(cached.image.height()))
            .max()
            .unwrap_or(0)
    }

    /// Restore the radar image from cached raw RGBA data.
    ///
    /// **The context must already have run a pass.** `egui::Context::load_texture`
    /// checks the picture it is handed against `InputState::max_texture_side`,
    /// and a context that has not begun a pass carries the 2048 that
    /// `InputState::default` carries — not what the adapter reports, which on
    /// the API-34 x86_64 emulator is 32768 and on any mobile device is at least
    /// twice the 4096 a long-range plan view reaches. Handing it a 4096 px
    /// raster there trips a `debug_assert!` that takes the winit loop, and with
    /// it the Activity, down; measured 3 of 3 on `main@178ab361` and again 3 of
    /// 3 on `main@5dbe9339`, 2026-08-21, API-34 x86_64 emulator.
    /// egui learns the real number from the `RawInput` `begin_frame` hands it,
    /// so the restore runs from inside the frame. The guard below is that
    /// sentence checked rather than assumed: it defers rather than clamping,
    /// because a halved raster is a visible change to the picture the user had.
    pub(super) fn restore_cached_render(&mut self, ctx: &egui::Context) {
        use squallar_egui::overlay_cache::{OverlayTextureData, RadarTextureMeta};
        use squallar_geo::PlacedRaster;
        use squallar_radar::types::ImageBounds;

        // Ahead of every mutation below, so a deferral is a whole one. The
        // flag is this function's own: it clears it by doing the work and
        // raises it by declining to, so no caller has to know the rule.
        let widest = self.widest_raster_to_restore();
        let admitted = ctx.input(|i| i.max_texture_side);
        if widest > admitted {
            // Said rather than passed over in silence: this frame put nothing
            // back, and the two numbers are what tells a frame that ran too
            // early from an adapter that really is this small.
            log::info!(
                "restore deferred: a {widest} px raster against a context \
                 admitting {admitted} px"
            );
            self.restore_pending = true;
            return;
        }
        self.restore_pending = false;

        // Every raster still arriving is let go of first, on **every** pane and
        // whether or not this goes on to restore one.
        self.gui.release_held_rasters();

        // Section panes first, and through their own loop: the one below is
        // bounded by `pane_render.len()` and skips every pane with no plan
        // view, which is every section pane there is.
        self.restore_section_textures(ctx);

        // Panes sharing a raster shared it before the context died too:
        let mut uploads = PlanViewUploads::default();

        for pane_idx in 0..self.render.pane_render.len().min(self.gui.pane_count()) {
            if self.gui.pane_has_no_plan_view(pane_idx) {
                continue;
            }
            let Some(ref cached) = self.render.pane_render[pane_idx].cached_render else {
                continue;
            };
            let max_range_km = cached.max_range_km;
            let product = cached.product;
            let elevation = cached.elevation;
            let nyquist_ms = cached.nyquist_ms;
            let melting_layer_source = cached.melting_layer_source;
            let storm_motion = cached.storm_motion;

            let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
                continue;
            };
            let lat = scan_info.site.lat;
            let lon = scan_info.site.lon;

            log::info!(
                "Restoring cached radar image for pane {} ({:?} at {:.1}°) from memory",
                pane_idx,
                product,
                elevation
            );

            let side = cached.image.width();
            let image = Arc::clone(&cached.image);
            let texture = {
                let counter = &mut self.texture_counter;
                uploads.handle(&image, || {
                    *counter += 1;
                    ctx.load_texture(
                        format!("radar_image_{counter}"),
                        Arc::clone(&image),
                        egui::TextureOptions::NEAREST,
                    )
                })
            };

            let bounds = ImageBounds::from_radar_site(lat, lon, max_range_km);
            let placed: PlacedRaster = bounds.into();
            if let Some(pane) = self.gui.pane_mut(pane_idx) {
                let cache = pane.overlay_cache_mut(&squallar_source::id::known::RADAR);
                // Showing retires whatever the pane was showing; see the note
                // in `App::apply_render_to_pane`.
                cache.show(OverlayTextureData {
                    texture,
                    placed,
                    data_generation: 0,
                    render_zoom: 0,
                    width: side as u32,
                    height: side as u32,
                    radar_meta: Some(RadarTextureMeta {
                        hover: Arc::clone(&cached.hover),
                        lat,
                        lon,
                        max_range_km,
                        nyquist_ms,
                        melting_layer_source,
                        storm_motion,
                        product: crate::render_key::field_id_of(product),
                        elevation,
                    }),
                    hit_map: None,
                });
            }
            self.render.pane_render[pane_idx].last_rendered = Some((product, elevation));
        }
    }

    /// Try to acquire the next surface texture for rendering.
    fn get_surface_texture(
        surface: &wgpu::Surface,
        _finished: &squallar_gpu::egui_renderer::PreparedFrame,
    ) -> SurfaceStatus {
        match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => SurfaceStatus::Ready(texture),
            wgpu::CurrentSurfaceTexture::Outdated => {
                log::warn!("wgpu surface outdated, skipping frame");
                SurfaceStatus::Skip
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                log::warn!("wgpu surface lost (display change?), will recreate state");
                SurfaceStatus::Lost
            }
            _ => {
                log::error!("Surface error");
                SurfaceStatus::Skip
            }
        }
    }

    /// Returns how soon egui asked to be painted again — the frame's
    /// `repaint_delay`, which `handle_redraw` turns into an immediate
    /// redraw or a scheduled wake (the second user test's animation fix;
    /// see `PreparedFrame::repaint_delay`). Returned from every exit,
    /// the skipped-surface ones included: the pass ended either way, and
    pub(super) fn present_frame(&mut self, size_in_pixels: [u32; 2]) -> std::time::Duration {
        let state = self.state.as_mut().unwrap();
        let window = self.window.as_ref().unwrap();

        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        let mirror_sources = self.gui.mirror_source_rects();
        let demand = self
            .volume_painter
            .as_ref()
            .and_then(|painter| painter.take_floor_demand());
        // Whether the deferral arm below owes the app another frame.
        let mut mirror_frame_owed = false;
        let mirror_target = match mirror_frame_action(
            mirror_sources.rects().is_empty(),
            mirror_sources.repainted(),
        ) {
            MirrorFrame::Release => {
                if let Some(resources) = state
                    .egui_renderer
                    .callback_resources_mut()
                    .get_mut::<squallar_volumetric::bridge::VolumeResources>()
                {
                    resources.release_mirror();
                }
                self.mirror_plan_applied = None;
                None
            }
            verdict => {
                let points = state.egui_renderer.context().pixels_per_point();
                // Sized in **points**, from the UI rather than from the surface:
                let size_in_points = self.gui.mirror_size_points();
                // Observed on EVERY frame with strips on screen, held frames
                // included: the dwell counter is what turns a dolly into a
                // rung flip, and a counter that paused while the strip was
                // clean would be a rung that never flips mid-orbit.
                let plan = self.mirror_rungs.observe(
                    demand,
                    [size_in_points.x, size_in_points.y],
                    points,
                    squallar_gpu::egui_renderer::MirrorLimits::for_device(
                        state.device.limits().max_texture_dimension_2d,
                        self.budgets.mirror_bytes,
                    ),
                );
                if verdict == MirrorFrame::Render {
                    let format = state.egui_renderer.attachment_config().color_format;
                    let device = state.device.clone();
                    self.mirror_plan_applied = Some(plan);
                    state
                        .egui_renderer
                        .callback_resources_mut()
                        .get_mut::<squallar_volumetric::bridge::VolumeResources>()
                        .map(|resources| {
                            (
                                resources.ensure_mirror(&device, plan.size_in_pixels, format),
                                plan,
                            )
                        })
                } else {
                    // The third state: keep-but-don't-render. The floors keep
                    // sampling last frame's mirror, so the texture is neither
                    // released (the `Release` arm's job) nor resized — a
                    // realloc destroys the picture mid-sample. A changed plan
                    // is deferred behind the stamp instead: the Gui repaints
                    // every strip next frame, and the realloc lands on
                    // primitives that carry them.
                    if self
                        .mirror_plan_applied
                        .is_some_and(|applied| applied != plan)
                    {
                        self.mirror_plan_stamp = self.mirror_plan_stamp.wrapping_add(1);
                        mirror_frame_owed = true;
                    }
                    None
                }
            }
        };
        let mirror =
            mirror_target
                .as_ref()
                .map(|(view, plan)| squallar_gpu::egui_renderer::MirrorRequest {
                    view,
                    size_in_pixels: plan.size_in_pixels,
                    pixels_per_point: plan.pixels_per_point,
                    source_rects: mirror_sources.rects(),
                });

        // Finish egui's pass and upload its textures, THEN ask for a surface.
        // The acquire span is stamped through a closure-local `Cell` so the
        // ledger borrow stays disjoint from the closures' captures.
        let acquire_span = std::cell::Cell::new(None);
        let (mut frame, status) = finish_then_acquire(
            || {
                state.egui_renderer.end_pass_and_upload(
                    &state.device,
                    &state.queue,
                    &mut encoder,
                    window,
                    size_in_pixels,
                    mirror,
                )
            },
            |finished| {
                let acquire_start = web_time::Instant::now();
                let acquired = Self::get_surface_texture(&state.surface, finished);
                acquire_span.set(Some((acquire_start, web_time::Instant::now())));
                acquired
            },
        );
        // Before the acquire span is filed, because the two are read together:
        // the phase stamps only cut anything when paired with this frame's
        // `ui_end` and acquire start. See `frame_ledger::PrepareHists`.
        self.frame_ledger
            .record_prepare_phases(frame.phase_stamps());
        if let Some((acquire_start, acquire_end)) = acquire_span.get() {
            self.frame_ledger.record_acquire(acquire_start, acquire_end);
        }
        // A deferred mirror-plan change owes the app one more frame: the loop
        // runs on `ControlFlow::Wait`, and the stamp only reaches the Gui at
        // the top of the next frame.
        let repaint_delay = if mirror_frame_owed {
            std::time::Duration::ZERO
        } else {
            frame.repaint_delay()
        };

        let surface_texture = match status {
            SurfaceStatus::Ready(texture) => texture,
            SurfaceStatus::Skip | SurfaceStatus::Lost => {
                self.frame_ledger.mark_skipped();
                // The mirror and raymarch passes this frame encoded are still
                // in this submit, so their brackets resolve here too — a
                // skipped surface is not a skipped measurement.
                state.egui_renderer.probe_end_frame(&mut encoder);
                frame.submit(&state.queue, encoder);
                state.egui_renderer.probe_collect();
                state.egui_renderer.free_textures(frame.textures_to_free());

                if matches!(status, SurfaceStatus::Lost) {
                    let volume_on_screen = self.gui.panes().iter().any(|pane| {
                        pane.render_view() == squallar_radar::types::RenderView::Volume
                    });
                    if volume_on_screen {
                        let losses = squallar_volumetric::degrade::note_surface_loss_with_volume();
                        log::warn!(
                            "wgpu surface lost with a 3D volume on screen ({losses} so far)"
                        );
                    }

                    self.on_pressure(crate::pressure::Pressure::SurfaceLost);

                    // Surface is irrecoverably lost (e.g. display changed on a
                    // foldable). Drop the entire rendering state so the next
                    // handle_redraw() lazily recreates it with a fresh surface.
                    self.render.clear_last_rendered();
                    drop(self.loop_frames.clear());
                    self.gui.clear_graphics_state();
                    // The mirror texture died with the device; the strip
                    // cache was force-flagged by `clear_graphics_state`, and
                    // the applied plan must not claim otherwise.
                    self.mirror_plan_applied = None;
                    self.state = None;
                }
                self.frame_ledger.mark_present_return();
                return repaint_delay;
            }
        };

        // The eight stamps of the `finish` split. Seven clock reads on every
        // presented frame, and unlike its four sibling splits this one is
        // recorded for idle frames too — see `frame_ledger::FinishHists` for
        // the measurement that made the wider denominator the point of it.
        // The skipped/lost arm above takes none: `finalize` discards that
        // frame outright, so a stamp there could never become a sample.
        let filed = web_time::Instant::now();
        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let viewed = web_time::Instant::now();

        state
            .egui_renderer
            .draw(&mut encoder, &surface_view, &frame);
        let drawn = web_time::Instant::now();

        // After the last pass of the frame, before its submit: the probe's
        // claimed brackets resolve in the same command stream that wrote
        // them. The collect after the submit never blocks — see the probe.
        state.egui_renderer.probe_end_frame(&mut encoder);
        let resolved = web_time::Instant::now();
        frame.submit(&state.queue, encoder);
        let submitted = web_time::Instant::now();
        state.egui_renderer.probe_collect();
        let collected = web_time::Instant::now();
        state.egui_renderer.free_textures(frame.textures_to_free());
        let freed = web_time::Instant::now();
        surface_texture.present();
        self.frame_ledger.mark_present_return();
        self.frame_ledger
            .record_finish_phases(crate::frame_ledger::FinishPhaseStamps {
                filed,
                viewed,
                drawn,
                resolved,
                submitted,
                collected,
                freed,
            });
        repaint_delay
    }

    /// **Build the loops that were waiting on a listing that has now
    /// landed.** Populates each pane's frame list and kicks off its downloads
    /// (throttled).
    ///
    /// The frames come from the layer, not from the arrival: the listing is
    /// filed by `apply_frame_listing` under the scope it was listed for, and
    /// this reads it back through `list_frames` scoped to each pane's own
    /// selection. A pane on another site therefore sees nothing of a radar
    /// listing, which is what keeps two sites' stamps out of one list.
    ///
    /// **Two arms, and the arrival says which** (WI-5). Radar's builds a
    /// `FramePlan` and hands it to the download manager, which owns its
    /// volumes and its Level III pairings. Every other layer owns its own
    /// residency, so its arm asks the layer for one task per frame it means
    /// to hold and puts them on the wire; the answers land through
    /// `apply_frame` on the arrival path that already exists.
    ///
    /// The two arms share the frame list itself - [`build_loop_frames`] - so
    /// they cannot sample, order or park differently.
    pub(super) fn accept_loop_scan_listings(&mut self) {
        let arrived = std::mem::take(&mut self.loop_listings_arrived);
        if arrived.is_empty() {
            return;
        }
        let allocation = self.loop_allocation();
        let budgets = self.budgets;
        let config = self.fetch_config();
        for LoopListingArrival { layer, site, range } in arrived {
            let mut built: Vec<(usize, FramePlan, squallar_radar::types::RadarProduct)> =
                Vec::new();
            let mut owed: Vec<(usize, Vec<squallar_source::time::FrameStamp>)> = Vec::new();
            {
                let (panes, overlays) = self.gui.panes_and_overlays_mut();
                for (pane_idx, pane) in panes.iter_mut().enumerate() {
                    // Only a pane still waiting for a listing over this very
                    // window, **on the layer the listing is for** — the
                    // window itself, not its width. Two panes looping one
                    // layer with equal spans and different anchors (a live
                    // enable beside a deep-scrub refill) ask two questions,
                    // and answering either with the other's listing presents
                    // one era's frames as the other — a confidently wrong
                    // picture, not a missing one. Every producer echoes the
                    // range it was dispatched with verbatim, so the recorded
                    // ask matches its own answer exactly and nobody else's.
                    let waiting = {
                        let ls = pane.time_state(&layer);
                        ls.is_active()
                            && ls.phase == squallar_egui::pane::LoopPhase::FetchingScanList
                            && ls.asked_range == Some(range)
                    };
                    if !waiting {
                        continue;
                    }
                    pane.hydrate_layer_states(overlays, pane_idx);
                    // The whole-pane cap divides across the layers this pane is
                    // animating, and it is counted HERE - where the budget is
                    // consumed - not pushed down with it.
                    let animating = pane.animating_layers().count();
                    // **The listing whole, not just its instants.**
                    // `FrameStamp::run` is what tells two runs' grids for the
                    // same valid time apart, and the model layer's
                    // `frame_target` — the resolver behind both `fetch_frame`
                    // and the frame-addressed `prepare_job` — answers `None`
                    // without it. A `LoopFrame` carries only a timestamp, so
                    // the run is carried in the stamps `build_loop_frames`
                    // hands back, and the listing itself stays on the
                    // timeline for the re-sample a changed allocation asks.
                    let listing: squallar_source::time::FrameListing = {
                        let view = pane.view(pane_idx);
                        let pane_ref = view.layer(&layer);
                        overlays.list_frames(&layer, &config, &pane_ref, range)
                    };
                    let Some(site) = site.as_deref() else {
                        // -- Every layer but radar --------------------------
                        // The cap is a byte division rather than radar's
                        // count, because an overlay frame is not a radar
                        // frame's size.
                        let frame_bytes = overlay_frame_bytes(pane, &layer, &budgets);
                        let ls = pane.time_state_mut(&layer);
                        let Some(stamps) = build_loop_frames(ls, listing, |_| {
                            layer_share(&allocation, pane_idx, None, frame_bytes, animating)
                        }) else {
                            log::warn!(
                                "Loop: {} listed no frames in the requested window for \
                                 pane {pane_idx}; leaving loop mode",
                                layer.as_str(),
                            );
                            *ls = squallar_egui::pane::LayerTimeState::new();
                            continue;
                        };
                        log::info!(
                            "Loop: populated {} {} frames for pane {pane_idx}",
                            stamps.len(),
                            layer.as_str(),
                        );
                        // The whole frame list, in render-set order: it was
                        // already capped to what the pane's byte share buys,
                        // so every frame it holds is a frame it means to make
                        // resident, and the order is the playhead outward.
                        let budget = ls.frames.len();
                        owed.push((
                            pane_idx,
                            ls.render_set_indices(budget)
                                .into_iter()
                                .map(|idx| {
                                    let valid = ls.frames[idx].timestamp;
                                    // The stamp the LAYER named, carried back
                                    // whole — see `listing` above. The
                                    // reconstruction is the fallback for a
                                    // layer whose sampling dropped the row,
                                    // which cannot happen while the frames come
                                    // from `stamps` itself.
                                    stamps.iter().copied().find(|f| f.valid == valid).unwrap_or(
                                        squallar_source::time::FrameStamp { valid, run: None },
                                    )
                                })
                                .collect(),
                        ));
                        continue;
                    };
                    // -- Radar ------------------------------------------------
                    let Some(product) =
                        squallar_radar::fields::product_for(&pane.selected_product())
                    else {
                        continue;
                    };
                    // Whether this listing is still wanted, and what it makes of the
                    // frame list, is decided in one place - including refusing a
                    // listing for a site the pane's loop has since moved off.
                    let Some(plan) = accept_scan_listing(
                        &allocation,
                        pane_idx,
                        &budgets,
                        pane.time_state_mut(&known::RADAR),
                        site,
                        listing,
                        animating,
                    ) else {
                        continue;
                    };
                    built.push((pane_idx, plan, product));
                }
            }
            for (pane_idx, plan, product) in built {
                log::info!(
                    "Loop: populated {} {} frames for pane {}",
                    plan.frames.len(),
                    plan.site,
                    pane_idx
                );
                // Store the frame plan - with the site it was listed for - then derive
                // the queue for whichever datasource this pane's product reads and
                // dispatch the first batch.
                self.loop_mgr.set_plan(pane_idx, plan);
                self.loop_mgr.plan_downloads_for(pane_idx, product);
                self.dispatch_pending_loop_downloads(pane_idx);
                self.dispatch_pending_loop_l3_pairings(pane_idx);
            }
            for (pane_idx, stamps) in owed {
                self.dispatch_loop_frame_fetches(pane_idx, &layer, &config, stamps);
            }
        }
    }

    /// **Ask `layer` for every frame `pane_idx` owes, and put the tasks it
    /// answers on the wire.**
    ///
    /// The layer may decline any of them - `fetch_frame` answers `None` for a
    /// stamp it already holds, and for one no listing of its own named - and a
    /// declined frame is simply not fetched rather than being retried here.
    ///
    /// **No throttle.** Radar's queue is rate-limited by
    /// `concurrent_loop_downloads` through its download manager; this dispatches
    /// the whole render set at once, which is bounded only by the byte cap the
    /// frame list was built under.
    fn dispatch_loop_frame_fetches(
        &mut self,
        pane_idx: usize,
        layer: &squallar_source::id::LayerId,
        config: &squallar_overlays::render::overlay_state::FetchConfig,
        stamps: Vec<squallar_source::time::FrameStamp>,
    ) {
        let tasks = self
            .with_layer_pane(pane_idx, layer, |overlays, pane_ref| {
                stamps
                    .into_iter()
                    .filter_map(|stamp| {
                        overlays
                            .fetch_frame(layer, config, pane_ref, &stamp)
                            .map(|task| (stamp, task))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        log::info!(
            "Loop: {} dispatched {} frame fetches for pane {pane_idx}",
            layer.as_str(),
            tasks.len(),
        );
        for (stamp, task) in tasks {
            // Marked here as well as in `refetch_owed_loop_frames`, so the
            // every-frame pass does not put a second copy of a granule already
            // travelling on the wire.
            self.render.mark_loop_frame_fetch(layer, stamp.valid);
            self.spawn_frame_fetch_task(stamp, task);
        }
    }

    /// Poll for finished Level III key listings. Each one unblocks every frame
    /// pairing that was waiting on it.
    fn poll_loop_l3_list_results(&mut self) {
        let mut listed = false;
        while let Ok(resp) = self.channels.loop_l3_list_receiver.try_recv() {
            // Cached under the site and code it was *listed* for, never under
            // whatever the requesting pane has since become — the keys belong to
            // the listing, and every pane looping that site shares them.
            self.loop_mgr
                .cache_l3_keys(&resp.site, &resp.code, resp.keys);
            listed = true;
        }
        if !listed {
            return;
        }
        // Every pane, not just the requester: two panes looping one site wait on
        // one listing, and the second would otherwise sit until something else
        // happened to re-dispatch it.
        for pane_idx in self.loop_mgr.pending_l3_pane_indices() {
            self.dispatch_pending_loop_l3_pairings(pane_idx);
        }
    }

    /// Poll for finished Level III frame pairings. A `None` result is cached as
    /// the answer — the site generated no object for that volume — so the frame is
    /// retired once instead of being re-paired every pass.
    fn poll_loop_l3_fetch_results(&mut self) {
        let mut completed_count = 0usize;
        while let Ok(resp) = self.channels.loop_l3_fetch_receiver.try_recv() {
            self.loop_mgr
                .cache_l3_product(&resp.site, &resp.code, resp.timestamp, resp.product);
            completed_count += 1;
        }
        if completed_count > 0 {
            // The same counter the Level II downloads decrement: one network
            // concurrency budget for the loop, whichever datasource it reads.
            self.loop_mgr.complete_batch(completed_count);
            self.dispatch_freed_loop_slots();
        }
    }

    /// Offer the slots a finished batch released to every pane that still owes
    /// downloads, on **both** datasources.
    fn dispatch_freed_loop_slots(&mut self) {
        for pane_idx in self.loop_mgr.pending_pane_indices() {
            self.dispatch_pending_loop_downloads(pane_idx);
        }
        for pane_idx in self.loop_mgr.pending_l3_pane_indices() {
            self.dispatch_pending_loop_l3_pairings(pane_idx);
        }
    }

    /// Dispatch pending Level III frame pairings up to the concurrency limit,
    /// listing the keys they will be ranked against first.
    fn dispatch_pending_loop_l3_pairings(&mut self, pane_idx: usize) {
        let Some(PendingL3Pairings {
            site,
            product,
            queue,
        }) = self.loop_mgr.extract_pending_l3(pane_idx)
        else {
            return;
        };
        let Some(pick) = product.level3_volume_pick() else {
            self.loop_mgr.insert_pending_l3(
                pane_idx,
                PendingL3Pairings {
                    site,
                    product,
                    queue,
                },
            );
            return;
        };

        // One listing per (site, code), shared by every pane looping that site.
        let days = pairing_days_for_frames(&queue);
        for code in product.level3_products().into_iter().flatten() {
            if self.loop_mgr.claim_l3_listing(&site, code) {
                self.spawn_loop_l3_listing(
                    pane_idx,
                    site.clone(),
                    (*code).to_string(),
                    days.clone(),
                );
            }
        }

        let slots = self
            .loop_mgr
            .available_slots(self.budgets.concurrent_loop_downloads);
        let mut batch = Vec::new();
        let mut retained = VecDeque::with_capacity(queue.len());
        for (ts, code) in queue {
            if self.loop_mgr.l3_is_resolved(&site, &code, &ts)
                || self.loop_mgr.l3_is_in_flight(&site, &code, &ts)
            {
                // Answered, or being answered — nothing owed either way.
                continue;
            }
            let Some(keys) = self.loop_mgr.l3_keys(&site, &code) else {
                // Waiting on the listing above.
                retained.push_back((ts, code));
                continue;
            };
            if batch.len() >= slots {
                retained.push_back((ts, code));
                continue;
            }
            batch.push((ts, code, Arc::clone(keys)));
        }

        let spawned = batch.len();
        for (ts, code, keys) in batch {
            self.loop_mgr.mark_l3_in_flight(&site, &code, ts);
            self.spawn_loop_l3_pairing(pane_idx, site.clone(), code, ts, keys, pick);
        }

        self.loop_mgr.insert_pending_l3(
            pane_idx,
            PendingL3Pairings {
                site,
                product,
                queue: retained,
            },
        );

        if spawned > 0 {
            self.loop_mgr.add_spawned(spawned);
        }
    }

    /// Poll for completed loop scan downloads. When a scan arrives, store it
    /// in the global scan cache and dispatch next pending downloads.
    fn poll_loop_scan_download_results(&mut self) {
        let mut completed_count = 0usize;
        while let Ok(resp) = self.channels.loop_scan_download_receiver.try_recv() {
            apply_completed_download(&mut self.loop_mgr, resp);
            completed_count += 1;
        }
        if completed_count > 0 {
            self.loop_mgr.complete_batch(completed_count);
            // Both datasources: the concurrency budget is shared, so the slots this
            // batch released belong to whoever is owed work. See
            // `dispatch_freed_loop_slots`.
            self.dispatch_freed_loop_slots();
        }
    }

    /// Dispatch pending loop scan downloads up to the concurrency limit.
    fn dispatch_pending_loop_downloads(&mut self, pane_idx: usize) {
        let slots = self
            .loop_mgr
            .available_slots(self.budgets.concurrent_loop_downloads);
        if slots == 0 {
            return;
        }

        // We need to look up cached/in_flight state while modifying the pending
        // queue, and both live in loop_mgr, so the queue is extracted completely,
        // processed, and put back.
        let Some(PendingDownloads { site, mut queue }) = self.loop_mgr.extract_pending(pane_idx)
        else {
            return;
        };

        // Filter out timestamps already cached or in flight for this site
        let mut batch = Vec::new();
        while !queue.is_empty() && batch.len() < slots {
            let ts = *queue.front().unwrap();
            if self.loop_mgr.is_cached(&site, &ts) || self.loop_mgr.is_in_flight(&site, &ts) {
                // Already have or fetching this scan — remove from pending
                queue.pop_front();
            } else {
                batch.push(queue.pop_front().unwrap());
            }
        }

        // **The layer resolves each volume to an archive object**; nothing
        // here holds one. A stamp it cannot resolve is a volume no listing of
        // this pane's site named, so it is dropped rather than retried
        // forever — the frame retires the way an unrenderable one does.
        let config = self.fetch_config();
        let listed = site.clone();
        let tasks: Vec<(chrono::NaiveDateTime, squallar_source::handler::FetchTask)> = self
            .with_layer_pane(
                pane_idx,
                &squallar_source::id::known::RADAR,
                |overlays, pane_ref| {
                    batch
                        .into_iter()
                        .filter_map(|ts| {
                            let stamp = squallar_source::time::FrameStamp {
                                valid: ts,
                                run: None,
                            };
                            let task = overlays.fetch_frame(
                                &squallar_source::id::known::RADAR,
                                &config,
                                pane_ref,
                                &stamp,
                            );
                            if task.is_none() {
                                log::warn!(
                                    "Loop: no {listed} archive object is listed for {ts}; \
                                     that frame cannot be fetched",
                                );
                            }
                            task.map(|task| (ts, task))
                        })
                        .collect()
                },
            )
            .unwrap_or_default();

        let spawned = tasks.len();

        for (ts, task) in tasks {
            self.loop_mgr.mark_in_flight(&site, ts);
            self.spawn_frame_fetch_task(
                squallar_source::time::FrameStamp {
                    valid: ts,
                    run: None,
                },
                task,
            );
        }

        // Put the queue back, still carrying its own site
        self.loop_mgr
            .insert_pending(pane_idx, PendingDownloads { site, queue });

        if spawned > 0 {
            self.loop_mgr.add_spawned(spawned);
        }
    }

    /// Poll for completed loop frame render results and upload textures.
    fn poll_loop_render_results(&mut self, ctx: &egui::Context) {
        while let Ok(mut rr) = self.channels.loop_render_receiver.try_recv() {
            let origin_pane = rr.pane_idx;
            // Resolved before the pane is borrowed, and off the *response*
            // rather than off the pane — see `frame_gates`.
            let gates = frame_gates(&self.loop_mgr, &rr);

            let Some(pane) = self.gui.pane_mut(origin_pane) else {
                continue;
            };

            let counter = &mut self.texture_counter;
            let Some(image) = accept_render_result(
                pane.time_state_mut(&known::RADAR),
                &mut rr,
                gates,
                |color_image| {
                    *counter += 1;
                    // `color_image` is the only copy of this frame's pixels on this
                    // thread — the renderer's RGBA buffer was dropped on the worker —
                    // and it is moved into the texture manager here rather than copied.
                    ctx.load_texture(
                        format!("loop_frame_{counter}"),
                        color_image,
                        egui::TextureOptions::NEAREST,
                    )
                },
            ) else {
                continue;
            };

            // **Filed once, under what it is a picture of.** Every pane on
            // this site, product and tilt at this instant draws this one
            // texture and reads this one hover source from here on —
            // whatever the panes' link flags or groups say. The siblings
            // keyed to it take it now rather than a pass later; a pane that
            // arrives later takes it out of the store at dispatch.
            let key =
                crate::loop_frame_store::LoopFrameKey::plan_view(rr.target.clone(), rr.timestamp);
            let picture = squallar_egui::pane::LoopFrameImage::PlanView(image);
            if let Some(replaced) =
                self.loop_frames
                    .insert(key.clone(), picture.clone(), origin_pane)
            {
                crate::loop_frame_store::discard(vec![replaced]);
            }
            for sibling_idx in 0..self.gui.pane_count() {
                if sibling_idx == origin_pane || self.gui.pane_has_no_plan_view(sibling_idx) {
                    continue;
                }
                let Some(sibling_pane) = self.gui.pane(sibling_idx) else {
                    continue;
                };
                // **Radar-addressed, named.** The broadcast carries a radar
                // plan-view texture keyed by `RenderTarget`
                // (site+product+elevation), so the only frame list it can
                // land in is radar's. Reading the sibling's *transport*
                // here would offer a radar picture to a satellite timeline.
                let sibling_loop = sibling_pane.time_state(&known::RADAR);
                if !sibling_loop.is_rendered_for(&rr.target) {
                    continue;
                }
                let sweep = broadcast_sweep(&self.loop_mgr, sibling_loop, &rr);

                let Some(sibling) = self.gui.pane_mut(sibling_idx) else {
                    continue;
                };
                let Some(sframe) = sibling
                    .time_state_mut(&known::RADAR)
                    .frame_accepting_broadcast_mut(rr.timestamp, &rr.target, sweep)
                else {
                    continue;
                };
                // If the sibling had its own render running for this frame it is now
                // redundant: same target and timestamp means the same image, so its
                // result is simply dropped when it arrives.
                sframe.render_in_flight = false;
                sframe.image = Some(picture.clone());
                self.loop_frames.hold(sibling_idx, &key);
            }
        }
    }

    /// Promote loops from `Rendering` to `Ready` once every frame they intend to
    /// render has settled — or off entirely when none of them can be rendered at
    /// all — then start playback for the panes that are ready.
    pub(super) fn update_loop_readiness(&mut self) {
        let allocation = self.loop_allocation();
        let budgets = self.budgets;
        let mut abandoned = Vec::new();
        let loop_mgr = &self.loop_mgr;
        // The visible slice, exactly as `panes_mut()` gives it - the registry
        // rides along because the residency question below is the layer's to
        // answer, and a handler needs the pane it is being asked about.
        let (panes, overlays) = self.gui.visible_panes_and_overlays_mut();
        for (pidx, p) in panes.iter_mut().enumerate() {
            let mut released = false;
            // **The residency read, taken before the mutable walk.** Each
            // non-radar animating layer is asked which of its frames it is
            // holding; the answers are owned stamps, so the walk below can
            // borrow the pane mutably while still consulting them.
            //
            // **Nothing happens here on a pane animating radar alone**, which
            // is every pane in this build: no hydrate, no ask, no allocation.
            // This runs once per frame per pane, and `hydrate_layer_states`
            // is not free - it republishes the pane's radar selection into its
            // slot - so it is asked for only when there is something to ask
            // it about.
            let ids: Vec<squallar_source::id::LayerId> = p
                .animating_layers()
                .filter(|slot| slot.id != squallar_source::id::known::RADAR)
                .map(|slot| slot.id.clone())
                .collect();
            let resident: Vec<(squallar_source::id::LayerId, Vec<chrono::NaiveDateTime>)> =
                if ids.is_empty() {
                    Vec::new()
                } else {
                    p.hydrate_layer_states(overlays, pidx);
                    let view = p.view(pidx);
                    ids.into_iter()
                        .map(|id| {
                            let stamps = overlays
                                .frames_resident(&id, &view.layer(&id))
                                .into_iter()
                                .map(|stamp| stamp.valid)
                                .collect();
                            (id, stamps)
                        })
                        .collect()
                };
            // Asked of every layer that is animating, not of the radar slot by
            // name (WI-2) - pinned by
            // `the_readiness_walk_settles_every_animating_layer_not_only_radar`.
            for slot in p.animating_layers_mut() {
                let budget = loop_ready_budget(&allocation, pidx, &slot.time, &budgets);
                let settled = if slot.id == squallar_source::id::known::RADAR {
                    // Radar answers both questions out of its own bookkeeping.
                    settle_radar_loop_phase(loop_mgr, pidx, &mut slot.time, budget)
                } else {
                    // Every other layer (WI-5 supplies the residency oracle,
                    // WI-7 the loading state):
                    //
                    // *settled* is the generic reading of the render set - a
                    // frame counts once it has an image, or once the layer
                    // says it holds no data for it and nothing is being
                    // rendered. The `scan_available` half is the layer's own
                    // `frames_resident`, which is why a frame whose data HAS
                    // landed does not read as settled: it is owed a texture.
                    //
                    // **What produces that texture is
                    // `dispatch_overlay_loop_renders`** (WI-6b), which runs in
                    // `Dispatch` after this pass has read the phase: a frame
                    // whose data is resident and whose picture is missing is
                    // exactly what it asks for, carrying the stamp so
                    // `prepare_job` rasterizes that frame rather than the
                    // pane's current selection. Until that answer lands the
                    // frame is owed a texture and this reads unsettled, which
                    // is why a loop still filling stays in `Rendering` and its
                    // map goes empty rather than stale.
                    //
                    // *still arriving* is `true`, and deliberately not
                    // `false`. It is the only thing standing between a loop
                    // with nothing yet to show and `*ls = new()`, so a layer
                    // that cannot answer must not be read as "nothing more is
                    // coming" - that reading is what destroyed a loading model
                    // timeline before WI-2.
                    let held = resident
                        .iter()
                        .find(|(id, _)| *id == slot.id)
                        .map(|(_, stamps)| stamps.as_slice())
                        .unwrap_or_default();
                    settle_loop_phase(
                        pidx,
                        &mut slot.time,
                        |ls| ls.render_set_settled(budget, frames_are_resident(held)),
                        |_| true,
                    )
                };
                if settled {
                    released = true;
                }
            }
            if released {
                abandoned.push(pidx);
            }
        }
        for pidx in abandoned {
            // The same release `handle_disable_loop` does: the pane is back to
            // single-frame mode, and clearing `last_rendered` is what makes
            // `dispatch_pane_renders` put its static image back.
            self.loop_mgr.remove_pending(pidx);
            if pidx < self.render.pane_render.len() {
                self.render.pane_render[pidx].last_rendered = None;
            }
        }

        // Synchronized playback start: the time-linked looping panes wait
        // for each other; an unlinked loop starts on its own readiness.
        self.sync_loop_playback_start();
    }

    /// Start loop playback for panes that are ready, holding the
    /// time-linked ones together (M11: `PaneState::time_link` is the gate —
    /// loop start synchronisation is a shared-time behaviour, so it follows
    /// the time link, not the layer link). A linked ready pane waits while
    /// any linked looping pane is not ready; an unlinked ready pane starts
    fn sync_loop_playback_start(&mut self) {
        let pane_count = self.gui.pane_count();
        let multi = pane_count > 1;

        // One wall-clock reading for the whole pass, so two panes parking on
        // "the frame at now" cannot be anchored a tick apart.
        let wall = chrono::Utc::now().naive_utc();

        // Each ready pane, with where its loop must park when it starts:
        // `Some(index)` for a rail that reaches into the future, `None` for
        // "the newest there is". See `loop_start_frame`.
        let mut ready_panes: Vec<(usize, Option<usize>)> = Vec::new();
        let mut not_ready_panes: Vec<usize> = Vec::new();
        // The slice walk, not `0..pane_count` with a `pane(idx)` inside it:
        // proven to visit the same panes (WI-0, pinned by
        // `the_index_walk_and_the_slice_walk_visit_the_same_panes`) and one
        // fewer reach through the seam. `pane_cannot_loop(idx)` was
        // `pane(idx).is_some_and(|p| !p.can_loop())`, so an index with no pane
        // fell through it and was dropped by the very next line either way.
        //
        // **The registry rides along** rather than being reached for
        // separately: where a starting loop parks is read off the transport
        // layer's own `time_axis`, and this is the visible slice `panes()`
        // hands out with the registry beside it — the same set, one reach.
        let (panes, overlays) = self.gui.visible_panes_and_overlays_mut();
        for (idx, pane) in panes.iter().enumerate() {
            if !pane.can_loop() {
                continue;
            }
            let ls = pane.transport_state();
            if !ls.is_active() {
                continue;
            }
            if ls.has_playback_started() {
                continue; // Already started (may be paused by user)
            }
            if ls.is_render_ready() {
                ready_panes.push((idx, loop_start_frame(pane, overlays, wall)));
            } else {
                not_ready_panes.push(idx);
            }
        }

        if ready_panes.is_empty() {
            return;
        }

        // A link group starts as one: a time-linked ready pane waits while a
        // pane it shares a group with is still catching up. Unlinked panes,
        // and panes in another group, sit outside both halves of that
        // sentence — so the wait is asked per pane rather than once for the
        // whole layout.

        // Start the startable panes with the same instant and frame position
        let now = web_time::Instant::now();
        for (idx, park) in ready_panes {
            if multi
                && not_ready_panes
                    .iter()
                    .any(|&waiting| self.gui.panes_time_linked(idx, waiting))
            {
                continue;
            }
            let pane = self.gui.pane_mut(idx).unwrap();
            let ls = pane.transport_state_mut();
            ls.phase = squallar_egui::pane::LoopPhase::Playing;
            ls.last_advance = Some(now);
            match park {
                // A forecast rail's newest frame is its horizon — up to 48 h
                // ahead — so `Live` starts the loop on the wrong picture and
                // parks the pane's clock in the future.
                Some(index) => {
                    pane.park_on_transport_frame(index);
                }
                // Align all panes to the last frame so they start from the same
                // position — said as a clock rather than as an index: `Live` is
                // "the newest there is", which is the same frame on every pane.
                //
                // A PARKED PANE IS EXEMPT, and this is not a special case so
                // much as the same sentence read properly: the alignment exists
                // so panes start together, and a pane the user scrubbed to an
                // instant is already where it is supposed to start. Applied to
                // it, `Live` silently threw that instant away.
                //
                // `loop_start_frame` returns `None` for every transport that
                // does not extend into the future, which is every radar pane,
                // so this arm is the common path rather than an edge: any
                // parked radar pane whose loop armed was dragged to live data
                // the moment it armed. That is how a screenshot pinned to the
                // 2013 Moore volume came back showing this afternoon's weather.
                None if pane.time.mode.as_of().is_some() => pane.settle_playheads(),
                None => pane.set_time_mode(squallar_egui::pane::TimeMode::Live),
            }
        }
    }

    /// Advance loop playback for all panes with active playing loops.
    fn advance_loop_playback(&mut self) {
        let now = web_time::Instant::now();

        for pane in self.gui.panes_mut() {
            // The pane's own posture, which every pane carries the same copy
            // of — see `Gui::set_loop_speed_fps`.
            let interval = loop_interval(pane.time.speed_fps);
            let mode = pane.time.mode;
            // **The pane's decision, not a derivation off what happens to be
            // running.** This read `clock_layer()` — the topmost *active*
            // slot — every tick, so the layer whose stamps the clock walked
            // could change underneath a running loop: a radar loop retiring
            // mid-playback (its phase falls to `Inactive`, or another layer
            // starts animating above it) moved the tick onto a different
            // layer's frames without the transport ever having moved.
            // `transport_layer()` is what the ∞ toggle armed and what the
            // scrubber addresses, so it is the one the tick must walk.
            let ls = pane.transport_state();
            if !ls.is_active() || !ls.is_playing() || ls.frames.is_empty() {
                continue;
            }

            let should_advance = ls
                .last_advance
                .map(|last| now.duration_since(last) >= interval)
                .unwrap_or(true);

            if should_advance {
                // Skip to the next frame that has a rendered texture, and move
                // the pane's CLOCK onto that frame's stamp rather than the
                // playhead onto its index: the playhead is derived from the
                // clock, and every other layer on the pane rides the same one.
                let num_frames = ls.frames.len();
                let from = ls.frame_at(mode);
                let landed = (1..=num_frames)
                    .map(|offset| (from + offset) % num_frames)
                    .find(|&candidate| ls.frames[candidate].image.is_some())
                    .map(|candidate| ls.frames[candidate].timestamp);
                // The slot is known to exist — an absent one reads back the
                // pane's `Inactive` orphan and was dropped above — so this
                // cannot be the write that materialises an empty timeline.
                pane.transport_state_mut().last_advance = Some(now);
                if let Some(stamp) = landed {
                    pane.set_time_mode(squallar_egui::pane::TimeMode::AsOf(stamp));
                }
            }
        }
    }

    /// Dispatch renders for loop frames around the playhead that have
    /// downloaded scan data but no rendered texture yet.
    ///
    /// **Also counts what the loops are holding**, for
    /// [`crate::loop_telemetry`], **and describes the scene** the budget
    /// system prices — one [`squallar_device_profile::scene::PaneNeed`] per
    /// pane. Neither is a second concern bolted on: this is the one walk over
    /// every pane the frame already makes for the pool's sake, and the
    /// alternative is more reaches into the `Gui` in a file whose coupling
    /// ceiling is permanent and already at its measured value. Both are a
    /// handful of integer reads over the layers this walk is touching anyway.
    fn loop_demand(&self) -> LoopWalk {
        use squallar_device_profile::quality::GroundPass;
        use squallar_device_profile::scene::PaneNeed;

        let mut demand = LoopDemand::default();
        let mut counts = crate::loop_telemetry::LoopState {
            // Every pane carries the same copy of the posture — see
            // `Gui::set_loop_speed_fps` — so whichever pane the walk sees
            // first is the setting.
            advance_us: 0,
            ..crate::loop_telemetry::LoopState::default()
        };
        // The window's size, which stands in for a 3D pane the painter has
        // not fitted an offscreen for yet. The offscreen is held to the
        // offscreen budget whatever size goes in, so the stand-in over-prices
        // by at most that budget and never under-prices. `[0, 0]` before a
        // surface exists.
        let window_px = self.state.as_ref().map_or([0, 0], |state| {
            [state.surface_config.width, state.surface_config.height]
        });
        let mut scene = Scene {
            panes: Vec::new(),
            // One entry per tile source that drew last pass, read off the
            // cache ledger's levels rather than off the Gui: the working set
            // the pass measured (cells on the glass and in the ancestor net)
            // at what one resident entry has measured on average. A role
            // that has drawn nothing yet — or a source parked with its
            // layer off — wants nothing and is not listed.
            tile_sources: tile_needs(),
            mirror_px: self
                .mirror_plan_applied
                .map_or([0, 0], |plan| plan.size_in_pixels),
            // Filled at the end of the walk from `overlay_grids` below.
            overlay_grids: Vec::new(),
        };
        // Distinct loops seen so far, each with the pane that owns it: a
        // later pane on the same identity is an alias of that pane's loop.
        let mut seen: Vec<(LoopIdentity, usize)> = Vec::new();
        // **Sites whose decoded loop volumes are already counted**, each with
        // the pane counting them: the loop download cache holds one volume
        // per named frame per site whatever product or view each pane draws
        // from it, so a second pane on the site holds the first's.
        let mut scan_sites: Vec<(String, usize)> = Vec::new();
        // **Every gridded overlay layer some pane shows**, once, at the host
        // budget its handler states — the scene's shared-grid term and its
        // per-layer read-out. Once across panes, because the handler is one
        // instance for the whole application.
        let mut overlay_grids: Vec<(squallar_source::id::LayerId, u64)> = Vec::new();
        // **Every loop running right now and what it renders**, and **what
        // each pane is parked at** — the two the decoded-volume price is
        // resolved from once the walk has seen every pane, exactly as
        // `App::evict_unneeded_loop_scans` resolves the retention. A later
        // pane's Level II loop makes an earlier pane's site need volumes, so
        // neither question can be answered inside the walk.
        let mut live_loops: Vec<(&str, Option<squallar_radar::types::RadarProduct>)> = Vec::new();
        let mut parked: Vec<(&str, chrono::NaiveDateTime)> = Vec::new();
        // The site and frame list of every pane that reached the radar arm,
        // by its index in `scene.panes`, for that second pass.
        let mut scan_owners: Vec<(usize, &str, &[squallar_egui::pane::LoopFrame])> = Vec::new();
        let model = LoopFrameModel::from_budgets(&self.budgets);
        for (pane_idx, pane) in self.gui.panes().iter().enumerate() {
            if counts.advance_us == 0 {
                counts.advance_us = loop_interval(pane.time.speed_fps)
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64;
            }
            counts.count_pane(pane);
            // The one volume a site keeps even when nothing loops from it —
            // read here, on the walk, for the same reason the eviction sweep
            // reads it on its own walk.
            if let Some(info) = pane.scan_info.as_ref() {
                parked.push((info.site.name, info.timestamp));
            }
            let view = pane.render_view();
            // A 3D pane's offscreen is priced at what the painter last fitted
            // it from: the pane's own size and the ground pass it decided,
            // read off the painter this walk owns rather than off the Gui.
            // Nothing is sized from a 2D pane's figure.
            let (px, ground) = match view {
                squallar_radar::types::RenderView::Volume => self
                    .volume_painter
                    .as_ref()
                    .and_then(|painter| painter.pane_picture(pane_idx))
                    .map_or((window_px, GroundPass::Off), |p| (p.px, p.ground)),
                _ => ([0, 0], GroundPass::Off),
            };
            // **The whole-picture overlays this pane SHOWS**: every
            // texture-mode layer with a cache slot on this pane that the
            // pane's own layer stack has enabled, radar excluded — its raster
            // is its own pipeline's and the static term's. Each is one raster
            // of the pane at the budget's oversampling crossing the page
            // heap, and the glass it covers is what the last dispatch planned
            // over (`[0, 0]` before one, when the window stands in —
            // over-priced, never under).
            //
            // **A need, not an observation, and the difference is a race.**
            // `fit` asks what this scene COSTS, which must be a function of
            // the scene alone; how many pictures happen to be on the heap at
            // the instant the walk runs is a transient of the upload drain,
            // which lands one band a frame and so passes through every count
            // from one to the layer total on the way to steady state. Priced
            // from that transient, two runs of one scene took different rungs
            // — Chromium read rung 0 on one Tier-2 `huge` pass and rung 2 on
            // the next, same bundle, same box — and the user sees the
            // oversampling, so the race is visible as sharpness. The resident
            // count is the watermark's question ("what is on the heap now")
            // and stays on the telemetry line as the observation it is.
            //
            // The key set is the deterministic half: `overlay_cache_mut` is
            // called for every texture-mode handler on every pane pass
            // (`squallar_egui::ui_map_pane`, before `enabled` is consulted),
            // so once a pane has drawn, its `overlay_textures` keys ARE the
            // texture-mode roster. `enabled` is the pane's own saved slot
            // state. Neither moves with an upload.
            let overlay_pictures = pane
                .overlay_textures
                .keys()
                .filter(|id| **id != known::RADAR && pane.is_overlay_enabled(id))
                .count();
            for id in pane
                .overlay_textures
                .keys()
                .filter(|id| **id != known::RADAR && pane.is_overlay_enabled(id))
            {
                if overlay_grids.iter().any(|(counted, _)| counted == id) {
                    continue;
                }
                let bytes = squallar_overlays::render::handlers::source_grid_budget_bytes(id);
                if bytes > 0 {
                    overlay_grids.push((id.clone(), bytes));
                }
            }
            let picture_px = match self.render.overlay_pane_px(pane_idx) {
                [0, 0] => window_px,
                planned => planned,
            };
            let mut pane_need = PaneNeed {
                px,
                view,
                looping: false,
                loop_span_secs: pane.time.span_secs as usize,
                cadence_secs: None,
                overlay_frame_bytes: 0,
                volume_grids: usize::from(view == squallar_radar::types::RenderView::Volume),
                ground,
                // No production caller dispatches a `BuildingMeshJob` yet, so
                // no pane spends the prism row.
                buildings: false,
                overlay_pictures,
                picture_px,
                loop_scans_shared: false,
                loop_scans_resident_bytes: 0,
                loop_scans_resident_frames: 0,
                loop_scans_needed: true,
            };
            let ls = pane.time_state(&known::RADAR);
            if !ls.is_active() {
                // **A pane looping something other than radar is a share
                // too.** Radar's timeline is the only one the three arms below
                // can see, so before WB-7 a radar-off pane running a forecast
                // or a satellite loop asked the pool for nothing — and was
                // then handed `share_bytes` sized as though it were the only
                // loop in the application, twice over with two such panes. A
                // pane whose radar IS looping is already counted below, and
                // `layer_share` divides that one share across the layers it
                // animates rather than the pane asking twice. Its need is
                // priced at the layer's own frame — measured off the texture
                // it is drawing with — over the wider of the pane's lookback
                // and the window the layer's listing was asked over.
                if let Some(slot) = pane.animating_layers().next() {
                    let span = pane_need.loop_span_secs.max(slot.time.span_secs as usize);
                    let cadence = slot.time.cadence_secs;
                    let frame_bytes = overlay_frame_bytes(pane, &slot.id, &self.budgets);
                    demand.push(pane_loop_need(
                        pane,
                        pane_idx,
                        LoopKind::Overlay,
                        &slot.time,
                        &self.budgets,
                        &model,
                    ));
                    pane_need.looping = true;
                    pane_need.cadence_secs = cadence;
                    pane_need.loop_span_secs = span;
                    pane_need.overlay_frame_bytes = frame_bytes;
                }
                scene.panes.push(pane_need);
                continue;
            }
            live_loops.push(live_loop_row(ls));
            let identity = loop_product(ls).map(|product| LoopIdentity::of(pane, ls, product));
            let owner = match identity {
                // A 3D pane with no product yet asks for nothing.
                None if ls.view == squallar_radar::types::RenderView::Volume => {
                    scene.panes.push(pane_need);
                    continue;
                }
                None => None,
                Some(key) => match seen.iter().find(|(k, _)| *k == key) {
                    Some((_, owner)) => Some(*owner),
                    None => {
                        seen.push((key, pane_idx));
                        None
                    }
                },
            };
            pane_need.cadence_secs = ls.cadence_secs;
            match owner {
                // A second pane on a loop already counted — orbiting the
                // same volume, or showing the same picture set over the same
                // window — holds no set of its own: its frames (and, in 3D,
                // its live grid) are the first pane's, and it reads that
                // pane's grant.
                Some(owner) => {
                    demand.alias(pane_idx, owner);
                    pane_need.looping = false;
                    pane_need.volume_grids = 0;
                }
                None => {
                    demand.push(pane_loop_need(
                        pane,
                        pane_idx,
                        LoopKind::of(ls.view),
                        ls,
                        &self.budgets,
                        &model,
                    ));
                    pane_need.looping = true;
                }
            }
            // **The site's decoded volumes are counted once**, under the pane
            // with the widest lookback. A later pane on a counted site marks
            // itself shared; a later pane looping wider takes the count over
            // and marks the earlier pane instead — an alias never takes it,
            // since it prices no loop of its own. Every pane pushes exactly
            // one entry, so a pane's index is its index in `scene.panes`.
            let site = radar_layer::site(ls);
            match scan_sites.iter_mut().find(|(counted, _)| counted == site) {
                Some((_, counted)) => {
                    if pane_need.looping
                        && pane_need.loop_span_secs > scene.panes[*counted].loop_span_secs
                    {
                        let earlier = &mut scene.panes[*counted];
                        earlier.loop_scans_shared = true;
                        earlier.loop_scans_resident_bytes = 0;
                        earlier.loop_scans_resident_frames = 0;
                        *counted = pane_idx;
                    } else {
                        pane_need.loop_scans_shared = true;
                    }
                }
                None => scan_sites.push((site.to_string(), pane_idx)),
            }
            if pane_need.looping {
                scan_owners.push((pane_idx, site, &ls.frames));
            }
            scene.panes.push(pane_need);
        }
        // **What each looping pane's decoded volumes cost**, resolved now
        // that every live loop is known.
        //
        // The rule is the retention's, asked of the retention's own predicate
        // rather than of a second copy of it: on a site some live loop renders
        // Level II from, the cache holds one volume per named frame, so the
        // frames it already holds are priced at what they measured and the
        // rest at the reserve. On a site where every live loop is Level III —
        // frames derived from paired objects, reading nothing from a volume —
        // the sweep drops the volumes, so no reserve is charged for a fetch
        // that will never come and what is left is whatever a pane parked at
        // a still is keeping.
        //
        // **Resident is measured on both arms**, which is what makes the
        // grace window need no modelling of its own: a site inside
        // `LOOP_LISTING_GRACE` still holds its volumes and they are still
        // counted, and the figure falls by itself on the sweep that drops
        // them. The parked still's volume is counted once — the loop cache is
        // the denominator here, and `volume_inventory` holding the same `Arc`
        // is the census's declared overlap, not a second charge.
        for (pane_idx, site, frames) in scan_owners {
            let pane = &mut scene.panes[pane_idx];
            if pane.loop_scans_shared {
                continue;
            }
            pane.loop_scans_needed =
                squallar_radar::loop_downloads::site_needs_decoded_source(site, &live_loops);
            let (named_bytes, named_frames) = self
                .loop_mgr
                .cached_scan_bytes_for(site, frames.iter().map(|frame| frame.timestamp));
            // The parked still, where it is not already one of the frames —
            // counted in the bytes, never in the frame count, which is what
            // the reserve subtracts from.
            let (parked_bytes, _) = self.loop_mgr.cached_scan_bytes_for(
                site,
                parked.iter().filter_map(|&(at_site, at)| {
                    (at_site == site && !frames.iter().any(|frame| frame.timestamp == at))
                        .then_some(at)
                }),
            );
            pane.loop_scans_resident_bytes = (named_bytes + parked_bytes) as u64;
            pane.loop_scans_resident_frames = named_frames;
        }
        counts.shared = self.loop_frames.shared();
        scene.overlay_grids = overlay_grids
            .iter()
            .map(|(_, budget_bytes)| OverlayGridNeed {
                budget_bytes: *budget_bytes,
            })
            .collect();
        LoopWalk {
            demand,
            counts,
            scene,
            overlay_grids,
        }
    }

    /// What is on screen, in the terms the budget system prices — built on
    /// the one pane walk [`Self::loop_demand`] already makes.
    pub(super) fn scene_of(&self) -> Scene {
        self.loop_demand().scene
    }

    /// The division of the pool in force, after the dwell and the dead band
    /// have had their say. The pool itself follows the scene: what its loops
    /// need, capped by the room the rest of it leaves under this session's
    /// capacity — never the class's ceiling.
    pub(super) fn observe_loop_demand(&mut self) -> LoopAllocation {
        let LoopWalk {
            demand,
            counts,
            scene,
            // The readout's, and composed on the readout's cadence rather
            // than this one — see `Self::compose_budget_readout`.
            overlay_grids: _,
        } = self.loop_demand();
        self.loop_counts = counts;
        self.refit_to_scene(&scene);
        // The tile caches' allowance follows the same scene and the same
        // capacity: the class rung's figures on the presumed arm, the
        // economy split held inside the bracket on the measured one. Pushed
        // to the Gui with the next frame's inputs.
        self.tile_cache_budget = squallar_device_profile::fit::tile_cache_budget(
            &scene,
            &self.budgets,
            &self.device_profile.limits,
            &self.capacity(),
            GRID_BYTES,
        );
        // Under a page-heap squeeze the economies are nothing: the caches
        // keep their working set — their own floor — and hold no history.
        // The rung rides along untouched; it is the ladder's, not the
        // economy's.
        if self.tile_economy_squeezed {
            self.tile_cache_budget = squallar_device_profile::budget::TileCacheBudget {
                styled_bytes: 0,
                parsed_bytes: 0,
                terrain_bytes: 0,
                whole_zoom: self.tile_cache_budget.whole_zoom,
            };
        }
        // What the page's next picture batch will allocate, for the
        // watermark's line: priced here, on the walk that has the scene, so
        // no reading of the heap walks the panes again.
        let terms = squallar_device_profile::fit::need_terms(&scene, &self.budgets, GRID_BYTES);
        self.host_headroom_bytes = terms
            .pictures_host
            .saturating_add(terms.picture_arrival_host);
        self.loop_pool = self.pool_for_scene(&scene);
        self.loop_pool_state
            .observe(
                self.loop_pool,
                LoopFrameModel::from_budgets(&self.budgets),
                demand,
            )
            .clone()
    }

    /// **Rebuild the budget readout**, taking the walk it needs.
    ///
    /// The one caller is [`Self::report_frame_telemetry`], which is the
    /// readout's only consumer — see [`Self::compose_budget_readout`] for why
    /// the cadence is the consumer's and not the frame's. It takes one
    /// `loop_demand` walk, which that tick was already taking for
    /// `overlay_pictures_line`'s pane count, and hands the scene back so the
    /// tick spends one walk where it spent one before.
    ///
    /// The allocation is the one in force ([`Self::loop_allocation`]), not a
    /// fresh plan: planning is the frame path's, and a readout that re-planned
    /// the pool to describe it would be an instrument with a side effect.
    fn refresh_budget_readout(&mut self) -> Scene {
        let LoopWalk {
            scene,
            overlay_grids,
            ..
        } = self.loop_demand();
        let terms = squallar_device_profile::fit::need_terms(&scene, &self.budgets, GRID_BYTES);
        let allocation = self.loop_allocation();
        self.compose_budget_readout(&scene, &terms, overlay_grids, &allocation);
        scene
    }

    /// **Compose the budget readout** from a scene a walk has priced: per
    /// pane, `fit::need_terms_for_pane` beside what the pane's stores hold;
    /// per pool, the capacity in force, its allowance, the scene's need and
    /// what is spare.
    ///
    /// # Cadence, and who sets it
    ///
    /// **Called from [`Self::refresh_budget_readout`], on the telemetry tick
    /// — never on the frame path.** The one thing that reads this readout is
    /// `budget_telemetry::budget_state_line`, inside
    /// [`Self::report_frame_telemetry`], which runs at most once per
    /// [`RASTER_TELEMETRY_PERIOD`] — 2 s. **That consumer sets the cadence.**
    /// The in-frame overlay the seam was built for reads `Gui::budget_readout`
    /// and paints the same levels; neither needs a figure retaken 120 times a
    /// second to say what the line says every 2 s.
    ///
    /// **There is no scene-change trigger to hang this on, and that is why it
    /// is a cadence.** Half of what it reads is not a function of the scene at
    /// all: `VolumeStore::pane_texture_bytes` and the loop grants are levels
    /// of bytes resident *now*, moving as volumes land and are evicted, and
    /// `page_heap_reading` is re-sampled by this same tick. A gate on "the
    /// scene moved" would hold a stale figure through exactly the window the
    /// readout exists to describe, and `refit_to_scene` — the nearest thing
    /// to such a trigger — has none either: it re-runs `fit` every frame and
    /// merely *acts* less often.
    ///
    /// What it costs, on the tick and nowhere else: a few multiplications a
    /// pane, one lock of the volume store's handful of entries per 3D pane,
    /// two linear scans of the allocation per 2D pane, and the grid list
    /// moved in. The pane vector is reused, so past the first composition it
    /// allocates nothing. [`squallar_egui::shell_api::BudgetReadout::generation`]
    /// rises by one, which is the whole of what the Gui compares.
    ///
    /// **Held bytes have two sources and one denominator each.** A 3D pane's
    /// grids are in the volume store, which knows its holders — shared where
    /// another pane holds the grid too. A 2D pane's loop is its grant from
    /// the pool — shared where the plan aliased the pane to another's loop or
    /// another to its. The loop frame store's picture sharing beyond aliasing
    /// is `loop state:`'s `shared` and is not folded in.
    ///
    /// **Spare is the least of what bounds it.** The model's spare is the
    /// allowance less the need; on a page heap the heap's own room — its
    /// maximum less `byteLength`, as the frame last sampled it — bounds that
    /// from above, since a model that has no term for something resident
    /// cannot see the wall the heap can. The live-bytes bound the design
    /// names has no producer yet and joins the minimum when it does.
    fn compose_budget_readout(
        &mut self,
        scene: &Scene,
        terms: &squallar_device_profile::fit::NeedTerms,
        overlay_grids: Vec<(squallar_source::id::LayerId, u64)>,
        allocation: &LoopAllocation,
    ) {
        use squallar_device_profile::scene::CapacitySource;
        use squallar_egui::shell_api::{PaneBudget, PoolReadout};

        let cap = self.capacity();
        let need = terms.total();
        let readout = &mut self.budget_readout;
        // Bumped here and nowhere else, so the counter and the content cannot
        // disagree: every consumer that caches a copy keys off this.
        readout.generation = readout.generation.wrapping_add(1);
        readout.panes.clear();
        for (pane_idx, pane) in scene.panes.iter().enumerate() {
            let pane_terms =
                squallar_device_profile::fit::need_terms_for_pane(pane, &self.budgets, GRID_BYTES);
            let (shared_bytes, own_bytes) = match pane.view {
                squallar_radar::types::RenderView::Volume => {
                    let (shared, own) = self.volume_store.pane_texture_bytes(pane_idx);
                    (shared as u64, own as u64)
                }
                squallar_radar::types::RenderView::PlanView
                | squallar_radar::types::RenderView::CrossSection => {
                    let held = allocation
                        .grant_for_pane(pane_idx)
                        .map_or(0, |grant| grant.bytes() as u64);
                    if allocation.pane_shares_loop(pane_idx) {
                        (held, 0)
                    } else {
                        (0, held)
                    }
                }
            };
            readout.panes.push(PaneBudget {
                terms: pane_terms,
                shared_bytes,
                own_bytes,
            });
        }
        readout.terms = *terms;
        readout.gpu = PoolReadout {
            capacity_bytes: cap.gpu_bytes,
            source: cap.source,
            allowance_bytes: cap.allowance(),
            need_bytes: need.gpu_bytes,
            spare_bytes: Some(cap.allowance().saturating_sub(need.gpu_bytes)),
            requested_percent: None,
            effective_percent: None,
        };
        // See [`host_spare_bytes`]: the model's spare, bounded by what the
        // heap and this instance's allocator actually say.
        let heap = HostSpareInputs {
            wall: self
                .page_heap_reading
                .filter(|heap| heap.page_max_bytes > 0)
                .map(|heap| (heap.page_max_bytes, heap.page_bytes)),
            live_bytes: squallar_alloc::live_bytes(),
            headroom_bytes: self.host_headroom_bytes,
        };
        readout.host = cap
            .host_bytes
            .zip(cap.host_allowance())
            .map(|(host, allowance)| {
                let model_spare = allowance.saturating_sub(need.host_bytes);
                PoolReadout {
                    capacity_bytes: host,
                    // The host has no probe: a probed session's host figure is
                    // the bracket's presumption, and a measured session's is
                    // the RAM reading beside the VRAM one.
                    source: match cap.source {
                        CapacitySource::Measured => CapacitySource::Measured,
                        CapacitySource::Probed | CapacitySource::Presumed => {
                            CapacitySource::Presumed
                        }
                    },
                    allowance_bytes: allowance,
                    need_bytes: need.host_bytes,
                    spare_bytes: Some(host_spare_bytes(model_spare, allowance, heap)),
                    requested_percent: None,
                    effective_percent: None,
                }
            });
        readout.overlay_grids = overlay_grids;
    }

    /// **`fit` for `scene` against this session's capacity, its answer
    /// checked.** `fit` promises `fit_holds` — the scene's need under the
    /// allowance, or every rung at its stop — by construction on both arms,
    /// so a `false` here is a defect in the arithmetic, not a scene too large.
    /// A debug build stops on it. A release build logs it once at warn and
    /// marks [`FIT_INVARIANT_BROKEN`], which holds the loop pool at its floor
    /// from then on ([`Self::pool_for_scene`]) rather than sizing loops from
    /// budgets that were not fitted; the budgets themselves are still
    /// adopted, since the floor of the ladder is the safest answer there is.
    pub(super) fn fit_scene(&self, scene: &Scene) -> squallar_device_profile::budget::Budgets {
        let cap = self.capacity();
        let fitted = fit(scene, &self.device_profile, &cap, GRID_BYTES);
        let holds = fit_holds(
            scene,
            &fitted,
            &self.device_profile.limits,
            &cap,
            GRID_BYTES,
        );
        let needed = need(scene, &fitted, GRID_BYTES).gpu_bytes / (1024 * 1024);
        debug_assert!(
            holds,
            "fit handed back {needed} MiB of need against a {} MiB allowance with rungs \
             left to shed",
            cap.allowance() / (1024 * 1024),
        );
        if !holds && !FIT_INVARIANT_BROKEN.swap(true, Ordering::Relaxed) {
            log::warn!(
                "Budgets: fit handed back {needed} MiB of need against a {} MiB allowance \
                 with rungs left to shed, at rung {}; the loop pool is held at its floor \
                 from here on",
                cap.allowance() / (1024 * 1024),
                fitted.steps_back,
            );
        }
        fitted
    }

    /// **The pool the scene asks for** — what its loops need, capped by the
    /// room the rest leaves under this session's capacity, held inside the
    /// bracket's floor and, on the presumed arm, its ceiling — or the floor
    /// alone once [`FIT_INVARIANT_BROKEN`] has been set.
    pub(super) fn pool_for_scene(&self, scene: &Scene) -> LoopPool {
        let cap = self.capacity();
        let limits = crate::loop_pool::LoopPoolLimits::from_budgets(&self.budgets);
        if FIT_INVARIANT_BROKEN.load(Ordering::Relaxed) {
            return LoopPool::new(0, limits);
        }
        LoopPool::for_scene(scene, &self.budgets, &cap, limits)
    }

    /// **Re-fit the budgets to the scene**, adopting a different answer only
    /// once it has held for the pool's own dwell. `fit` is pure and a few
    /// multiplications per pane, so it runs on every loop walk; the dwell is
    /// what keeps a pane that flickers into existence for a frame from moving
    /// the 3D quality ceiling with it. A scene that shrinks gets its rungs back
    /// the same way — computed live from scene and capacity, no counter.
    pub(super) fn refit_to_scene(&mut self, scene: &Scene) {
        let wanted = self.fit_scene(scene);
        if wanted == self.budgets {
            self.pending_fit = None;
            return;
        }
        let held = match self.pending_fit {
            Some((pending, frames)) if pending == wanted => frames + 1,
            _ => 1,
        };
        if held < LOOP_POOL_DWELL_FRAMES {
            self.pending_fit = Some((wanted, held));
            return;
        }
        self.pending_fit = None;
        let cap = self.capacity();
        log::info!(
            "Budgets: rung {} for the scene ({} panes, {} MiB of need against {} MiB allowed \
             of {} MiB {}, {} MiB of economy allowance): {:?} 3D quality ceiling, {} MiB of \
             offscreen, {:?} grid cells, {} textured loop frames, overlay oversampling {} \
             percent against {} MiB of host need",
            wanted.steps_back,
            scene.panes.len(),
            need(scene, &wanted, GRID_BYTES).gpu_bytes / (1024 * 1024),
            cap.allowance() / (1024 * 1024),
            cap.gpu_bytes / (1024 * 1024),
            crate::budget_telemetry::capacity_source_word(cap.source),
            economy_allowance(scene, &wanted, &cap, GRID_BYTES) / (1024 * 1024),
            wanted.quality_ceiling,
            wanted.offscreen_bytes / (1024 * 1024),
            wanted.grid_cells,
            wanted.loop_render_budget,
            wanted.overlay_oversample_percent,
            need(scene, &wanted, GRID_BYTES).host_bytes / (1024 * 1024),
        );
        self.adopt_budgets(wanted);
    }

    /// The allocation in force. See [`Self::observe_loop_demand`].
    pub(super) fn loop_allocation(&self) -> LoopAllocation {
        self.loop_pool_state.allocation().clone()
    }

    /// **Answer memory pressure, within this session.** Economy first — the
    /// shared render cache, the plan-view extractions, and the loop caches'
    /// data no live frame names — then the session's capacity presumption
    /// comes down and the scene is re-fitted to it, which sheds a rung only
    /// when need alone no longer fits. Nothing is written to the store: a
    /// reopen fits the same scene to the same budgets whatever this session
    /// learned.
    ///
    /// The payloads freed here leave through `offload::discard_each`, never
    /// dropped on this thread: a shared render is `side^2 x 4` bytes twice
    /// over, and an extraction is a whole sweep's gates.
    ///
    /// Held rasters are not released here. The one call that lets them go is
    /// on the surface-lost path, which drops the whole graphics state behind
    /// this anyway; reaching it from here would grow the app's reach into the
    /// UI layer, and the tile cache's economy is not yet in this system.
    pub(super) fn on_pressure(&mut self, cause: crate::pressure::Pressure) {
        let (renders, render_bytes) = self.render.clear_render_cache();
        let extracts = self.render.clear_extract_cache();
        let (render_entries, extract_entries) = (renders.len(), extracts.len());
        squallar_worker::offload::discard_each("pressure-render-cache", renders);
        squallar_worker::offload::discard_each("pressure-extract-cache", extracts);

        // **The page heap's own levers, in order.** First the tile
        // economies, the cheapest thing on that heap to give back — the
        // styled history, the parsed geometry and the terrain rasters, all
        // to zero, the working set kept by the caches' own floor, paid down
        // one eviction per pump and never a frame (`Self::observe_loop_demand`
        // holds the allowances at nothing from here on). Counted once: a
        // second event finds them already given. Then the presumption and
        // the rung below; the loop caches last, after the re-fit, because
        // what a shorter history frees on the page is the decoded volumes
        // no live frame names, and that set is known only once the pool has
        // been re-planned.
        let tile_economy_bytes = if cause.is_page_heap() && !self.tile_economy_squeezed {
            self.tile_economy_squeezed = true;
            let held = self.tile_cache_budget;
            held.styled_bytes + held.parsed_bytes + held.terrain_bytes
        } else {
            0
        };

        // An allocation the browser refused while the WebGPU probe was holding
        // its doubling textures is the probe's doing, not a wall of this
        // session's: the textures are destroyed the moment the probe reports,
        // and a presumption lowered here would hold the probed figure down for
        // the whole session (`crate::pressure::is_the_gpu_probes_own`). The
        // economy is evicted all the same; the rung stands. A heap no lever
        // reaches — the worker's — holds the rung the same way.
        let rung = if crate::pressure::is_the_gpu_probes_own(cause, self.gpu_probe) {
            log::warn!("pressure: oom during gpu probe, presumption held");
            self.budgets.steps_back
        } else if cause.is_beyond_reach() {
            self.budgets.steps_back
        } else {
            self.refit_under_pressure(cause)
        };
        self.evict_unneeded_loop_scans();
        let reclaimed = crate::pressure::Reclaimed {
            render_entries,
            render_bytes,
            extracts: extract_entries,
            tile_economy_bytes,
            oversample_percent: self.budgets.overlay_oversample_percent,
        };
        log::warn!("{}", crate::pressure::pressure_line(cause, reclaimed, rung));
    }

    /// Take what the device's error sink counted since the last frame. Any
    /// count at all is one pressure event; how many errors one frame produced
    /// does not change the answer. Called once per frame, at the end of the
    /// present path, so a frame that also lost its surface can step twice —
    /// two causes, two rungs — which is bounded and rare rather than a defect
    /// this guards against.
    pub(super) fn absorb_gpu_pressure(&mut self) {
        if squallar_gpu::pressure::take_out_of_memory() > 0 {
            self.on_pressure(crate::pressure::Pressure::OutOfMemory);
        }
    }

    /// **Lower this session's capacity presumption and re-fit the scene to
    /// it**; the rung the ladder now stands on.
    ///
    /// **Why the allowance and not the need.** The design sketched
    /// `resident_at_event x 0.9`. Nothing in this tree measures what was
    /// resident — the profile's `vram_bytes` is capacity and the upload ledgers
    /// are running totals — and the one figure to hand, the scene's need, is
    /// the wrong stand-in: nine tenths of a need can never be fitted by that
    /// same need, so a presumption set there would shed a rung on every event,
    /// including one whose whole cause was economy the eviction has already
    /// taken. The wall is therefore taken to be at most the capacity in force,
    /// and the presumption comes down by one economy fraction: what sat above
    /// need at the event is what the eviction gave back. The scene then re-fits
    /// against it, which sheds rungs only when need alone no longer fits; when
    /// the eviction was the whole answer nothing moves and the log says so, and
    /// a second event lowers the presumption again. Never persisted: the
    /// presumption dies with the process.
    ///
    /// **The capacity figure is what comes down, not the allowance**, so the
    /// step is one economy fraction on both arms: a presumed capacity is its
    /// own allowance and the two spellings agree; a measured one allows three
    /// quarters of itself, and lowering the allowance's figure and then
    /// allowing three quarters of *that* would compound the step to 0.675 on
    /// every event.
    ///
    /// **Two walls, two presumptions.** A page-heap event lowers the host
    /// figure and nothing else: the page's watermark says nothing about the
    /// card, and a GPU rung shed for it would cost the loop its history for
    /// a byte the page never gets back. Every other cause lowers the GPU
    /// figure, as before, and leaves the host's where it stands.
    ///
    /// **The GPU decay has a floor: what the ladder's floor rung needs for
    /// this scene.** The step is geometric — seven events halve the figure,
    /// thirty leave four percent of it — and below the floor rung's need the
    /// ladder has nothing left to shed, so a presumption lowered past it
    /// buys no rung and only makes the readout lie about a wall this scene
    /// was never going to fit under. The floor is `fit::floor_need`, priced
    /// for the scene at every rung's stop and turned back into a capacity
    /// figure on this arm (`Capacity::gpu_bytes_for_allowance`), and it is
    /// never above the capacity in force: a scene whose floor need already
    /// exceeds the capacity holds the presumption rather than raising it.
    ///
    /// **Beside the high-water mark, the figure that can fall.** The host
    /// arm's `observed` is `byteLength`, which only grows; the same lines
    /// now print this instance's live bytes (`squallar_alloc::live_bytes`)
    /// beside it — logged, and not yet acted on. `0` there is an instance
    /// that never installed the counter.
    fn refit_under_pressure(&mut self, cause: crate::pressure::Pressure) -> u32 {
        const MIB: u64 = 1024 * 1024;
        let cap = self.capacity();
        let scene = self.scene_of();
        let live = squallar_alloc::live_bytes().unwrap_or(0) / MIB;
        let mut beside = format!("live {live} MiB");
        let lowered = if cause.is_page_heap() {
            // A bracket with no host figure has nothing to hold down: the
            // economy went, the rung stands, and the log says why.
            let Some(host) = cap.host_bytes else {
                log::info!(
                    "Budgets: held at rung {} after {}: no host figure on the {} bracket to \
                     presume lower",
                    self.budgets.steps_back,
                    cause.label(),
                    self.budgets.name,
                );
                return self.budgets.steps_back;
            };
            // **Lowered from the mark, not from the constant.** The scene's
            // host need is the tile working set and the picture batch, and
            // those are a minority of what a page holds: the module's own
            // statics, egui's tessellation buffers, the decoded volumes
            // behind the loop and every transfer in flight are on the same
            // heap and in no term here. So a presumption stepped down from
            // the bracket's 1 GiB stays far above a need that was never the
            // whole story, the fit finds nothing over its allowance, and the
            // ladder stands at its top rung while the heap traps — measured
            // on the Tier-2 `huge` leg of 2026-09-03, which read `steps 0`
            // and `oversample 150` at 1011 of 1024 MiB.
            //
            // The reading is the fix. It is a high-water mark on a heap that
            // only grows, so it is a floor under what this page has already
            // needed, and holding the presumption to a fraction of it prices
            // the whole heap rather than the part this crate can name. Each
            // event lowers it again from the newer, higher mark, so the
            // ladder converges instead of stalling.
            let observed = cause.page_heap_used().unwrap_or(host);
            beside = format!("page heap mark {} MiB, {beside}", observed / MIB);
            let host = host.min(observed) / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0;
            self.session_host_capacity = Some(host);
            host
        } else {
            let lowered = cap.gpu_bytes / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0;
            let floor = cap
                .gpu_bytes_for_allowance(
                    floor_need(&scene, &self.device_profile, GRID_BYTES).gpu_bytes,
                )
                .min(cap.gpu_bytes);
            let gpu = lowered.max(floor);
            self.session_capacity = Some(gpu);
            gpu
        };
        let refitted = self.fit_scene(&scene);
        let needed = need(&scene, &refitted, GRID_BYTES);
        let needed = if cause.is_page_heap() {
            needed.host_bytes
        } else {
            needed.gpu_bytes
        } / (1024 * 1024);
        if refitted == self.budgets {
            log::info!(
                "Budgets: held at rung {} after {}: the scene's {needed} MiB fits the {} MiB \
                 this session now presumes; {beside}",
                self.budgets.steps_back,
                cause.label(),
                lowered / MIB,
            );
            return self.budgets.steps_back;
        }
        log::info!(
            "Budgets: re-fitted to rung {} after {}: {needed} MiB against the {} MiB this \
             session now presumes; {:?} 3D quality ceiling, {} MiB of offscreen, {} textured \
             loop frames, {:?} grid cells, overlay oversampling {} percent; {beside}",
            refitted.steps_back,
            cause.label(),
            lowered / MIB,
            refitted.quality_ceiling,
            refitted.offscreen_bytes / (1024 * 1024),
            refitted.loop_render_budget,
            refitted.grid_cells,
            refitted.overlay_oversample_percent,
        );
        self.adopt_budgets(refitted);
        self.pending_fit = None;
        self.loop_pool = self.pool_for_scene(&scene);
        self.budgets.steps_back
    }

    /// **One raster per loop frame of a non-radar layer that has none** — the
    /// producer for [`LoopFrameImage::Overlay`] (WI-6b).
    ///
    /// [`LoopFrameImage::Overlay`]: squallar_egui::pane::LoopFrameImage::Overlay
    ///
    /// Radar's loop fills itself from decoded volumes it owns; every other
    /// animating layer's frame is a texture, and the only thing that makes one
    /// is `spawn_overlay_render`. This walks the frames that are missing one
    /// and asks for exactly those.
    ///
    /// # What bounds it, and what that costs
    ///
    /// **The byte share, re-derived every pass and applied to what is already
    /// held before anything more is asked for.** [`layer_share`] divides this
    /// pane's slice of the loop pool by what one of *this layer's* frames
    /// really costs on *this* pane, measured off the live raster
    /// (`overlay_frame_bytes`) and priced by [`LoopFrameModel`]'s `overlay` arm
    /// before one exists. At 1280x960 points and 1x that is a 1920x1440
    /// texture = **11.06 MB per frame**; at 2x it is 44.24 MB. The wasm pool
    /// floor is 56 MiB for the whole application, so on wasm at floor with one
    /// animating layer this buys **5 frames of the 1280x960 pane** and two
    /// animating layers buy 2 each — the `MIN_LOOP_FRAMES_PER_PANE` floor,
    /// which [`layer_share`] documents itself as exceeding the byte bound to
    /// honour.
    ///
    /// WI-5 already sampled the frame *list* to that figure when the listing
    /// landed, through the same call. This re-derives it because **every input
    /// moves**: the window can be resized (the texture is planned off the pane
    /// rect, so `frame_bytes` grows with it), `LoopPool` re-plans the
    /// allocation at runtime, and a second layer can start animating. So the
    /// eviction below is not belt-and-braces — it is the only thing that gives
    /// bytes back when the share shrinks under a list that was sized against a
    /// larger one.
    ///
    /// # What it will not do
    ///
    /// * **Rasterize a frame whose data is not resident.** The layer's own
    ///   `frames_resident` is the oracle, and the stamp it answers with — run
    ///   included — is what the dispatch carries. A frame whose grid has not
    ///   landed is left alone for a later pass; asking anyway would file the
    ///   pane's *current* picture into it, which is the whole defect the draw
    ///   fork exists to prevent.
    /// * **Invent a viewport.** The geometry comes off the record of this
    ///   pane's own live raster of this layer. A layer that has never
    ///   rasterized here has no record and is skipped.
    /// * **Land on the frame thread.** `spawn_overlay_render` is the same
    ///   off-thread funnel the live raster goes through. A CONUS HRRR
    ///   rasterize measured 133 ms median against a 200 ms loop interval, and
    ///   the cost is projection-bound: the output texture's size is nearly
    ///   free and the *window* is everything.
    fn dispatch_overlay_loop_renders(&mut self) {
        let allocation = self.loop_allocation();
        let budgets = self.budgets;
        // Collected before anything is dispatched: `spawn_overlay_render`
        // takes `&mut self` and the walk below holds the pane slice.
        let mut asks: Vec<(
            usize,
            squallar_source::id::LayerId,
            squallar_source::time::FrameStamp,
        )> = Vec::new();
        // Frames whose **granule** is missing, as `(pane, layer, stamp)`.
        // Filled by the same walk and spent below, after the render asks: a
        // frame that can be drawn now is worth more than one that has to
        // travel first.
        let mut owed_data: Vec<(
            usize,
            squallar_source::id::LayerId,
            squallar_source::time::FrameStamp,
        )> = Vec::new();
        {
            let (panes, overlays) = self.gui.visible_panes_and_overlays_mut();
            for (pane_idx, pane) in panes.iter_mut().enumerate() {
                // The whole-pane share divides across every layer that is
                // animating, radar included — the same denominator WI-5 built
                // the frame list under.
                let animating = pane.animating_layers().count();
                let ids: Vec<squallar_source::id::LayerId> = pane
                    .animating_layers()
                    .filter(|slot| slot.id != squallar_source::id::known::RADAR)
                    .map(|slot| slot.id.clone())
                    .collect();
                if ids.is_empty() {
                    continue;
                }
                pane.hydrate_layer_states(overlays, pane_idx);
                // Read before any timeline is borrowed mutably: a re-sampled
                // list is settled onto the pane's own clock.
                let clock = pane.time.mode;
                for id in ids {
                    if !overlays
                        .render_mode(&id)
                        .is_some_and(|mode| mode.has_texture())
                    {
                        continue;
                    }
                    let held = layer_share(
                        &allocation,
                        pane_idx,
                        None,
                        overlay_frame_bytes(pane, &id, &budgets),
                        animating,
                    );
                    // The list follows the allocation in force — denser when
                    // the pool granted this loop a balloon, sparser when a
                    // pane joined and took it back — and bytes are given back
                    // before more is asked for; see the note on the share
                    // moving under a list that is already built. A frame the
                    // re-sample adds is owed its data and its picture, and the
                    // walk below asks for both through the supply that fills
                    // every other frame.
                    {
                        let ls = pane.time_state_mut(&id);
                        if ls.resample_frames(held) {
                            ls.settle_playhead(clock);
                        }
                        ls.evict_textures_outside_render_set(held);
                    }
                    let resident = {
                        let view = pane.view(pane_idx);
                        overlays.frames_resident(&id, &view.layer(&id))
                    };
                    let ls = pane.time_state(&id);
                    if ls.frames.is_empty() {
                        continue;
                    }
                    for idx in ls.render_set_indices(held) {
                        if asks.len() >= MAX_OVERLAY_LOOP_RENDERS_PER_PASS {
                            // Out of funnel budget for this pass. Left alone,
                            // not retired: the next pass asks again, and the
                            // playhead-outward order means the frames nearest
                            // the playhead were taken first.
                            break;
                        }
                        let frame = &ls.frames[idx];
                        if frame.image.is_some() || frame.render_in_flight || frame.render_failed {
                            continue;
                        }
                        // The layer's own stamp, not one rebuilt from the
                        // frame's timestamp: `FrameStamp::run` is what
                        // separates two runs' grids for the same instant, and
                        // a `LoopFrame` carries only the instant.
                        let Some(stamp) = resident.iter().find(|s| s.valid == frame.timestamp)
                        else {
                            // **Owed its data, not its picture.** The layer is
                            // not holding this frame's granule: either it has
                            // never been fetched, or it was staged, passed
                            // through the one-granule staging area and was
                            // evicted by the next arrival before anything
                            // described a job off it. Both are the same
                            // question — ask again — and neither used to be
                            // asked at all.
                            //
                            // Asked for as the stamp the LISTING named, run
                            // and all, not one rebuilt from the instant: the
                            // layer is not holding this frame, so its own
                            // stamp is not there to take, and the listing is
                            // the answer this slot was chosen from. A model
                            // layer's `fetch_frame` declines a stamp with no
                            // run, so a bare instant would leave the frame
                            // owed for the life of the loop. The bare instant
                            // is the fallback for a listing that does not
                            // name it, which is what every observed layer's
                            // stamp is anyway.
                            let stamp = ls
                                .listing
                                .as_ref()
                                .and_then(|listing| {
                                    listing.frames.iter().find(|s| s.valid == frame.timestamp)
                                })
                                .copied()
                                .unwrap_or(squallar_source::time::FrameStamp {
                                    valid: frame.timestamp,
                                    run: None,
                                });
                            owed_data.push((pane_idx, id.clone(), stamp));
                            continue;
                        };
                        asks.push((pane_idx, id.clone(), *stamp));
                    }
                }
            }
        }
        for (pane_idx, id, stamp) in asks {
            let Some(req) = self.render.overlay_record(pane_idx, &id).cloned() else {
                continue;
            };
            log::debug!(
                "Loop: pane {pane_idx} asked {} for the frame at {}",
                id.as_str(),
                stamp.valid,
            );
            self.spawn_overlay_render(vec![pane_idx], id, req, Some(stamp));
        }
        self.refetch_owed_loop_frames(owed_data);
    }

    /// **Put the granules the loop is still missing back on the wire.**
    ///
    /// The other half of the frame supply, and the half that did not exist:
    /// `accept_loop_scan_listings` dispatched one fetch per frame the instant
    /// its listing landed and **never asked again**, while a layer that stages
    /// one granule at a time evicts each arrival as the next lands. A granule
    /// that reached the staging area at a moment its pane could not rasterize
    /// it was therefore gone for good, and its frame stayed blank for the life
    /// of the loop.
    ///
    /// Two guards, and both are load-bearing because this runs every frame:
    ///
    /// * **the pane's own geometry record.** A pane that has never rasterized
    ///   this layer cannot turn a granule into a picture — `spawn_overlay_render`
    ///   would have nothing to size the raster with, which is why the render
    ///   asks above are dropped for it. Fetching for it would be the same
    ///   bytes discarded on a loop; the ask waits until the pane can spend it.
    /// * **[`crate::render_dispatch::RenderDispatcher::loop_frame_fetch_in_flight`]**,
    ///   so a frame owed its data asks once rather than once per frame. This
    ///   half is not an optimisation. A frame stays owed for as long as its
    ///   granule is travelling, and this pass runs in every `Dispatch` phase,
    ///   so without the mark a thirteen-frame loop puts thirteen 7.4 MB GETs
    ///   on the wire *per frame of the pump* for the whole time it is loading
    ///   normally — measured at 130 asks across 10 passes by
    ///   `a_frame_owed_its_granule_is_asked_for_once_however_many_passes_run`,
    ///   which is the floor that stands under it.
    ///
    /// **And a third, for the case neither of those covers**: a fetch that
    /// *answers* and carries nothing. The mark clears on every answer, so a
    /// granule whose fetch cannot succeed was re-asked on the very next pass,
    /// and the next — 120 attempts in 3.3 seconds, measured in the field on
    /// 2026-08-31, against a condition that could not clear on its own and
    /// burning the frame time and memory the failure was about.
    /// [`crate::render_dispatch::RenderDispatcher::loop_frame_retry_due`] is
    /// the widening ladder that bounds it, and its first rung is immediate so
    /// the ordinary "staged, evicted before it could be drawn" case is
    /// unslowed.
    ///
    /// The layer still has the last word: `fetch_frame` answers `None` for a
    /// stamp no listing of its own named and for one it already holds.
    ///
    /// **The ask carries the whole stamp, run included.** The marks and the
    /// ladder are keyed on the instant alone — a pane's frames never share
    /// one — but the fetch is not: a model layer's frame is `(run, hour)`, its
    /// `fetch_frame` resolves the run off the stamp and answers `None`
    /// without one, so a re-ask built from the bare instant was declined
    /// every pass and the frame stayed owed for the life of the loop. The
    /// caller takes the stamp from the listing the frame was chosen from.
    fn refetch_owed_loop_frames(
        &mut self,
        owed: Vec<(
            usize,
            squallar_source::id::LayerId,
            squallar_source::time::FrameStamp,
        )>,
    ) {
        // Over the WHOLE owed set, before any of it is spent: a ladder is
        // dropped the moment its frame stops being owed, and doing that as the
        // walk goes would drop every rung the walk had not reached yet.
        let still_owed: std::collections::HashSet<(
            squallar_source::id::LayerId,
            chrono::NaiveDateTime,
        )> = owed
            .iter()
            .map(|(_, id, stamp)| (id.clone(), stamp.valid))
            .collect();
        self.render.retain_loop_frame_retries(&still_owed);

        let config = self.fetch_config();
        for (pane_idx, id, stamp) in owed {
            let valid = stamp.valid;
            // The two guards below come first on purpose: a pass on which the
            // pane cannot draw this layer, or on which the granule is already
            // travelling, is not a pass this frame sat out, and burning a rung
            // for it would slow the loading case the ladder is not about.
            if self.render.overlay_record(pane_idx, &id).is_none()
                || self.render.loop_frame_fetch_in_flight(&id, valid)
            {
                continue;
            }
            if !self.render.loop_frame_retry_due(&id, valid) {
                continue;
            }
            let task = self
                .with_layer_pane(pane_idx, &id, |overlays, pane_ref| {
                    overlays.fetch_frame(&id, &config, pane_ref, &stamp)
                })
                .flatten();
            let Some(task) = task else {
                continue;
            };
            log::info!(
                "Loop: re-asked {} for the granule at {valid} (pane {pane_idx})",
                id.as_str(),
            );
            self.render.mark_loop_frame_fetch(&id, valid);
            self.spawn_frame_fetch_task(stamp, task);
        }
    }

    fn dispatch_loop_renders(&mut self) {
        let allocation = self.observe_loop_demand();
        let budgets = self.budgets;
        let model = LoopFrameModel::from_budgets(&budgets);
        // Radar loops whose frame list was re-sampled to the allocation in
        // force this pass: the pane, its product, the loop's site and the list
        // as it now stands, so the download queue can be re-derived from it
        // once the panes are released.
        let mut resampled: Vec<(
            usize,
            squallar_radar::types::RadarProduct,
            String,
            Vec<chrono::NaiveDateTime>,
        )> = Vec::new();
        // Panes whose product moved to another datasource, so the frames now need
        // bytes nothing is fetching. Collected here and acted on below, because
        // re-deriving a queue needs `loop_mgr` while the pane is borrowed.
        let mut replan: Vec<(usize, squallar_radar::types::RadarProduct)> = Vec::new();
        // Panes whose 3D loop must let go of every grid it holds **before**
        // anything is built for the new key. See `VolumeStore::retain_set`:
        let mut release_volume_sets: Vec<usize> = Vec::new();
        // Panes whose loop is no longer active and whose download queue is
        // therefore serving nobody. Collected for the same borrow reason.
        let mut retire_queues: Vec<usize> = Vec::new();
        // Panes that cannot loop at all. Separate from `retire_queues` because
        // that one also releases a 3D loop's resident grid set, and a pane
        // that was never able to loop has none to release — the index walk
        // this replaced called `remove_pending` and nothing else for them.
        let mut drop_pending: Vec<usize> = Vec::new();
        let motion_override = self.render.storm_motion_override_kt();
        let srv_fallback = self.render.srv_fallback();
        // The slice walk, not `0..pane_count` with a `pane_mut(idx)` inside it:
        // proven to visit the same panes (WI-0, pinned by
        // `the_index_walk_and_the_slice_walk_visit_the_same_panes`) and two
        // fewer reaches through the seam. `pane_cannot_loop(idx)` was
        // `pane(idx).is_some_and(|p| !p.can_loop())`, so an index naming no
        // pane was false there and then dropped by `pane_mut` on the next
        // line — it never reached `remove_pending`, and it still does not.
        // Every holder of a shared frame is re-stated inside this walk, so
        // the store starts the pass holding nothing for anybody.
        self.loop_frames.begin_pass();
        let panes = self.gui.panes_mut();
        for (pane_idx, pane) in panes.iter_mut().enumerate() {
            if !pane.can_loop() {
                drop_pending.push(pane_idx);
                continue;
            }
            let Some(product) = squallar_radar::fields::product_for(&pane.selected_product())
            else {
                continue;
            };
            let elevation = pane.selected_elevation();
            let section_key = pane.cross_section().and_then(|s| s.line).map(|line| {
                squallar_egui::pane::SectionLoopKey::new(
                    line,
                    (product == squallar_radar::types::RadarProduct::StormRelativeVelocity)
                        .then_some(motion_override)
                        .flatten(),
                    srv_fallback,
                )
            });
            // The volume half of the key, for a 3D loop: the ground the frames
            // are resampled over and the vector they are derived with. See
            // `VolumeLoopKey`.
            let volume_key = pane.volume().map(|v| {
                squallar_egui::pane::VolumeLoopKey::new(
                    // The pane's stored region — see `VolumePane::region`.
                    v.region,
                    (product == squallar_radar::types::RadarProduct::StormRelativeVelocity)
                        .then_some(motion_override)
                        .flatten(),
                    srv_fallback,
                )
            });
            // Read before the timeline is borrowed mutably: the divisor the
            // pane's layers share, and the clock a re-sampled list settles to.
            let animating = pane.animating_layers().count();
            let clock = pane.time.mode;
            let ls = pane.time_state_mut(&known::RADAR);
            if !ls.is_active() {
                retire_queues.push(pane_idx);
                continue;
            }
            if ls.frames.is_empty() {
                continue;
            }

            // **The frame list follows the allocation in force**: denser
            // when the pool granted this loop a balloon, sparser when a pane
            // joined and took it back. A 2D radar loop animating alone lists
            // its whole cap and only its textured set moves below; a 3D
            // loop's list is its resident set, and a 2D loop beside another
            // animating layer is held to its bytes, so this is where a
            // balloon adds their frames and a deflation drops them. Integers
            // over a few dozen stamps, and a no-op on every pass the list
            // already matches.
            let held = layer_share(
                &allocation,
                pane_idx,
                Some(loop_frames_held(&allocation, pane_idx, ls, &budgets)),
                model.bytes_for(ls.view),
                animating,
            );
            if ls.resample_frames(held) {
                ls.settle_playhead(clock);
                resampled.push((
                    pane_idx,
                    product,
                    radar_layer::site(ls).to_string(),
                    ls.frames.iter().map(|f| f.timestamp).collect(),
                ));
            }

            let view_key = match ls.view {
                squallar_radar::types::RenderView::CrossSection => {
                    section_key.map(squallar_egui::pane::LoopViewKey::Section)
                }
                squallar_radar::types::RenderView::Volume => {
                    volume_key.map(squallar_egui::pane::LoopViewKey::Volume)
                }
                squallar_radar::types::RenderView::PlanView => None,
            };

            if ls.retarget_renders_keyed(
                &crate::render_key::field_id_of(product),
                elevation,
                view_key,
            ) {
                if ls.view == squallar_radar::types::RenderView::Volume {
                    release_volume_sets.push(pane_idx);
                }
                log::debug!(
                    "Loop: pane {} retargeted to {:?} at {:.1}°, re-rendering all frames",
                    pane_idx,
                    product,
                    elevation
                );
                replan.push((pane_idx, product));
                continue;
            }

            // Evict textures from frames far from the playhead to cap memory usage...
            let budget = loop_render_budget(&allocation, pane_idx, ls, &budgets);
            ls.evict_textures_outside_render_set(budget);
            // ...then re-state to the shared store what this pane still
            // wants of it: its render set, and whatever it is holding under
            // budget — filed, if the store has not seen it. A frame this pane
            // scrubbed away from stays filed while any other pane names it;
            // one nobody names is dropped after the walk. A 3D loop holds
            // grids in the volume store and says nothing here.
            if ls.view != squallar_radar::types::RenderView::Volume
                && let Some(target) = ls.rendered_for.as_ref()
            {
                let keep = ls.render_set_indices(budget);
                self.loop_frames.hold_frames(
                    pane_idx,
                    target,
                    ls.view,
                    ls.section_key(),
                    ls.frames
                        .iter()
                        .enumerate()
                        .filter(|(idx, frame)| frame.image.is_some() || keep.contains(idx))
                        .map(|(_, frame)| (frame.timestamp, frame.image.as_ref())),
                );
            }
        }
        crate::loop_frame_store::discard(self.loop_frames.end_pass());
        for pane_idx in drop_pending {
            self.loop_mgr.remove_pending(pane_idx);
        }
        for pane_idx in retire_queues {
            self.loop_mgr.remove_pending(pane_idx);
            // A torn-down 3D loop's grids go with its queue. Without this the
            // resident set outlives the loop that asked for it, and 512 MiB
            // stays allocated for a pane that is showing a live volume.
            if self.volume_store.holds_set(pane_idx) {
                self.volume_store.release_set(pane_idx);
                if let Some(pane) = self.gui.pane_mut(pane_idx)
                    && let Some(volume) = pane.volume_mut()
                {
                    volume.rendered_for = None;
                }
            }
        }
        // Ahead of every dispatch below, which is the whole point of the rule:
        for pane_idx in release_volume_sets {
            let dropped = self.volume_store.release_set(pane_idx);
            log::debug!(
                "3D loop: pane {pane_idx} retargeted, released its resident set ({dropped} grids \
                 freed)",
            );
        }
        // A re-sampled list names volumes the plan may not: the plan is
        // re-set from the list as it stands and the queue re-derived, so the
        // frames a balloon added are fetched — and nothing already cached is
        // fetched twice, since the download dispatch skips the cache.
        for (pane_idx, product, site, frames) in resampled {
            log::info!(
                "Loop: pane {pane_idx} re-sampled its {site} list to {} frames for the \
                 allocation in force",
                frames.len(),
            );
            self.loop_mgr
                .set_plan(pane_idx, FramePlan::new(site, frames));
            if self.loop_mgr.plan_downloads_for(pane_idx, product) {
                self.dispatch_pending_loop_downloads(pane_idx);
                self.dispatch_pending_loop_l3_pairings(pane_idx);
            }
        }
        for (pane_idx, product) in replan {
            if self.loop_mgr.plan_downloads_for(pane_idx, product) {
                log::info!(
                    "Loop: pane {pane_idx} now reads {} for its frames",
                    if product.is_level3() {
                        "Level III objects"
                    } else {
                        "Level II volumes"
                    },
                );
                self.dispatch_pending_loop_downloads(pane_idx);
                self.dispatch_pending_loop_l3_pairings(pane_idx);
            }
        }

        // Renders to spawn. `target` is the pane's render target (site + selected
        // product/elevation); `snapped` is that selection resolved to a sweep angle
        // present in this frame's own scan, which is what the renderer is given.
        let mut to_render: Vec<LoopRenderRequest> = Vec::new();
        // Frames the store already holds a picture for. The frame index is
        // resolved here and used as-is below — re-finding it by timestamp
        // would be a second lookup free to disagree with this one.
        let mut to_clone: Vec<LoopCloneRequest> = Vec::new();
        // Frames whose scan carries no sweep for the selected product: (pane_idx, frame_idx).
        let mut to_mark_failed: Vec<(usize, usize)> = Vec::new();

        let pane_count = self.gui.pane_count();

        // Cross-section cuts to dispatch, and the running count that paces them.
        let mut to_cut: Vec<LoopSectionRequest> = Vec::new();

        let mut to_build: Vec<LoopVolumeRequest> = Vec::new();

        for pane_idx in 0..pane_count {
            if self.gui.pane_cannot_loop(pane_idx) {
                continue;
            }
            let Some(pane) = self.gui.pane(pane_idx) else {
                continue;
            };
            let ls = pane.time_state(&known::RADAR);
            if !ls.is_active() || ls.frames.is_empty() {
                continue;
            }

            let site_lat = radar_layer::coords(ls).0;
            let site_lon = radar_layer::coords(ls).1;

            // Set by `retarget_renders` in the loop above for every active, non-empty
            // loop. Carried through the plan so the dedup, the donor search and the
            // dispatch stamp all read the one value instead of re-deriving it.
            let Some(target) = ls.rendered_for.clone() else {
                continue;
            };

            // The intended render set — shared with the readiness check so the two
            // cannot drift apart (see `LayerTimeState::render_set_settled`).
            let indices =
                ls.render_set_indices(loop_render_budget(&allocation, pane_idx, ls, &budgets));

            if ls.view == squallar_radar::types::RenderView::Volume {
                let Some(key) = ls.volume_key().cloned() else {
                    for &idx in &indices {
                        to_mark_failed.push((pane_idx, idx));
                    }
                    continue;
                };
                for &idx in &indices {
                    let frame = &ls.frames[idx];
                    let volume_target = squallar_egui::pane::VolumeTarget {
                        volume: squallar_egui::pane::VolumeStamp {
                            site: target.site.clone(),
                            collected: frame.timestamp,
                        },
                        product: target.product.clone(),
                        region: key.region,
                    };
                    to_build.push(LoopVolumeRequest {
                        pane_idx,
                        frame_idx: idx,
                        target: volume_target,
                        retired: frame.render_failed,
                    });
                }
                continue;
            }

            if ls.view == squallar_radar::types::RenderView::CrossSection {
                let Some(key) = ls.section_key().cloned() else {
                    for &idx in &indices {
                        to_mark_failed.push((pane_idx, idx));
                    }
                    continue;
                };
                for &idx in &indices {
                    let frame = &ls.frames[idx];
                    if frame.render_in_flight || frame.render_failed {
                        continue;
                    }
                    // The ladder this frame's own scan resolves *now*. Both the
                    // staleness test and the cut are keyed on it, so they cannot
                    // disagree about which ladder the picture is of.
                    let ladder = match frame_section(&self.loop_mgr, &target, frame.timestamp) {
                        FrameSection::At(ladder) => ladder,
                        FrameSection::Unrenderable => {
                            to_mark_failed.push((pane_idx, idx));
                            continue;
                        }
                        FrameSection::Pending => continue,
                    };
                    if frame
                        .image
                        .as_ref()
                        .and_then(squallar_egui::pane::LoopFrameImage::section)
                        .is_some_and(|cut| cut.ladder == ladder)
                    {
                        continue;
                    }

                    // One copy, drawn twice: a cut any pane has finished for
                    // this key through this ladder is taken from the store,
                    // whatever the panes' links say.
                    let filed = crate::loop_frame_store::LoopFrameKey::section(
                        target.clone(),
                        key.clone(),
                        frame.timestamp,
                    );
                    if let Some(picture) = self
                        .loop_frames
                        .get(&filed)
                        .filter(|p| p.section().is_some_and(|cut| cut.ladder == ladder))
                    {
                        to_clone.push(LoopCloneRequest {
                            dest_pane: pane_idx,
                            dest_frame: idx,
                            key: filed,
                            picture: picture.clone(),
                        });
                        continue;
                    }

                    if to_cut.len() >= MAX_LOOP_SECTION_CUTS_PER_FRAME {
                        // Out of frame-thread budget for this pass. Left alone,
                        // not retired: the next pass asks again, and the pane
                        // goes on showing whatever has already landed.
                        break;
                    }
                    // A cut already queued this pass for the same key will be
                    // filed and broadcast on arrival, so this frame leans on it.
                    if section_already_queued(to_cut.iter(), frame.timestamp, &target, &key) {
                        continue;
                    }
                    to_cut.push(LoopSectionRequest {
                        pane_idx,
                        frame_idx: idx,
                        timestamp: frame.timestamp,
                        target: target.clone(),
                        key: key.clone(),
                        ladder,
                        site_lat,
                        site_lon,
                    });
                }
                continue;
            }

            for &idx in &indices {
                let frame = &ls.frames[idx];
                if frame.image.is_some() || frame.render_in_flight || frame.render_failed {
                    continue;
                }

                // One copy, drawn twice: a picture any pane has finished for
                // this target at this instant is taken from the store,
                // whatever the panes' links say. The render below is only for
                // a picture nobody has.
                let filed = crate::loop_frame_store::LoopFrameKey::plan_view(
                    target.clone(),
                    frame.timestamp,
                );
                if let Some(picture) = self.loop_frames.get(&filed) {
                    to_clone.push(LoopCloneRequest {
                        dest_pane: pane_idx,
                        dest_frame: idx,
                        key: filed,
                        picture: picture.clone(),
                    });
                    continue;
                }

                // The sweep this frame's own data resolves the selection to, or
                // why it cannot be rendered. One question for both datasources —
                // see `frame_sweep`.
                match frame_sweep(&self.loop_mgr, &target, frame.timestamp) {
                    FrameSweep::At(snapped) => {
                        if render_already_queued(
                            to_render.iter(),
                            frame.timestamp,
                            &target,
                            snapped,
                        ) {
                            continue;
                        }
                        to_render.push(LoopRenderRequest {
                            pane_idx,
                            frame_idx: idx,
                            timestamp: frame.timestamp,
                            target: target.clone(),
                            snapped,
                            site_lat,
                            site_lon,
                        });
                    }
                    FrameSweep::Unrenderable => to_mark_failed.push((pane_idx, idx)),
                    // Its data has not arrived yet. Left alone; the next pass asks
                    // again.
                    FrameSweep::Pending => {}
                }
            }
        }

        // Retire frames that cannot be rendered at the selected product/elevation
        for (pane_idx, frame_idx) in to_mark_failed {
            if let Some(pane) = self.gui.pane_mut(pane_idx)
                && let Some(frame) = pane.time_state_mut(&known::RADAR).frames.get_mut(frame_idx)
            {
                frame.render_failed = true;
            }
        }

        // Hand out the pictures the store already had (no render needed). The
        // frame index was resolved during planning; nothing since has
        // reordered the frame list (`to_mark_failed` only sets a flag), so it
        // is used directly.
        for req in to_clone {
            let Some(dest) = self.gui.pane_mut(req.dest_pane) else {
                continue;
            };
            if let Some(dframe) = dest
                .time_state_mut(&known::RADAR)
                .frames
                .get_mut(req.dest_frame)
            {
                dframe.image = Some(req.picture);
                self.loop_frames.hold(req.dest_pane, &req.key);
            }
        }

        // Now spawn renders and mark the frames in flight, respecting concurrent limit
        for req in to_render {
            // Check concurrent render limit before each spawn (shared with static pane renders)
            let current = self.render.renders_in_flight.load(Ordering::Relaxed);
            if current >= self.render.concurrent_renders() {
                break;
            }

            // Asked here rather than inside the spawn because "the data has not
            // arrived" is not a failed render: this frame is skipped and asked
            // again next pass, and nothing about it is marked.
            let Some(req_product) = crate::render_key::radar_field(&req.target.product) else {
                continue;
            };
            if !self
                .loop_mgr
                .frame_data_arrived(&req.target.site, req_product, &req.timestamp)
            {
                continue;
            }

            let spawned = self.spawn_loop_frame_render(
                req.pane_idx,
                req.timestamp,
                req.render_params(),
                req.target,
            );

            if spawned && let Some(pane) = self.gui.pane_mut(req.pane_idx) {
                pane.time_state_mut(&known::RADAR).frames[req.frame_idx].render_in_flight = true;
            }
        }

        for req in to_cut {
            if self.render.renders_in_flight.load(Ordering::Relaxed)
                >= self.render.concurrent_renders()
            {
                break;
            }
            let Some((scan, declared)) = crate::render_key::radar_field(&req.target.product)
                .and_then(|p| {
                    self.loop_mgr
                        .frame_volume(&req.target.site, p, &req.timestamp)
                })
            else {
                if let Some(pane) = self.gui.pane_mut(req.pane_idx)
                    && let Some(frame) = pane
                        .time_state_mut(&known::RADAR)
                        .frames
                        .get_mut(req.frame_idx)
                {
                    frame.render_failed = true;
                }
                continue;
            };
            let (pane_idx, frame_idx) = (req.pane_idx, req.frame_idx);
            match self.spawn_loop_section_render(req, scan, declared) {
                crate::render_dispatch::SectionDispatch::Dispatched => {
                    if let Some(pane) = self.gui.pane_mut(pane_idx)
                        && let Some(frame) =
                            pane.time_state_mut(&known::RADAR).frames.get_mut(frame_idx)
                    {
                        frame.render_in_flight = true;
                    }
                }
                // Nothing was taken and nothing is wrong: ask again next frame.
                crate::render_dispatch::SectionDispatch::Busy => {}
                crate::render_dispatch::SectionDispatch::NoPayload => {
                    if let Some(pane) = self.gui.pane_mut(pane_idx)
                        && let Some(frame) =
                            pane.time_state_mut(&known::RADAR).frames.get_mut(frame_idx)
                    {
                        frame.render_failed = true;
                    }
                }
            }
        }

        self.make_volume_frames_resident(to_build);
    }

    /// Make each planned 3D loop frame's grid resident, and name it on the
    /// frame once it is.
    fn make_volume_frames_resident(&mut self, to_build: Vec<LoopVolumeRequest>) {
        use squallar_volumetric::bridge::{Hold, VolumeEntry};

        let mut dispatched = 0usize;
        // Every target still wanted, per pane, gathered as the pass goes so
        // the statement below is exactly what this pass decided rather than a
        // second walk free to disagree with it.
        let mut held: std::collections::BTreeMap<usize, Vec<squallar_egui::pane::VolumeTarget>> =
            std::collections::BTreeMap::new();

        for req in to_build {
            held.entry(req.pane_idx)
                .or_default()
                .push(req.target.clone());
            // Cheap: already built, building, or refused. Costs a lookup and
            // an attach, and is deliberately outside the pacing budget.
            let known = self
                .volume_store
                .share_held(req.pane_idx, &req.target, Hold::Set);
            if !known {
                if req.retired {
                    continue;
                }
                if dispatched >= MAX_LOOP_VOLUME_BUILDS_PER_FRAME {
                    // Out of frame-thread budget for this pass. Left alone,
                    // not retired: the next pass asks again, and the pane goes
                    // on marching whatever has already landed.
                    continue;
                }
                // **A remainder, named rather than papered over.** This whole
                // pass is radar's loop — the view is a `squallar_radar` enum,
                // the anchor is a radar geometry and the frames are radar
                // scans — so the layer to ask is not in doubt and is not
                // derived from anything generic. The pane's own 3D walk is
                // what will name it when the loop path itself goes
                // source-agnostic; that is not WO-M14b-2's.
                match self.prepare_volume(
                    req.pane_idx,
                    &req.target,
                    Hold::Set,
                    &squallar_source::id::known::RADAR,
                ) {
                    // A build was started, or a refusal was decided. Either
                    // way the store now answers for this target.
                    crate::app::VolumePrepare::Served => dispatched += 1,
                    // The scan has not downloaded yet, or the render budget is
                    // full. Nothing was spent; the next pass asks again.
                    crate::app::VolumePrepare::Waiting | crate::app::VolumePrepare::Busy => {
                        continue;
                    }
                }
            }
            let Some(found) = self.volume_store.lookup(&req.target) else {
                continue;
            };
            let Some(pane) = self.gui.pane_mut(req.pane_idx) else {
                continue;
            };
            let Some(frame) = pane
                .time_state_mut(&known::RADAR)
                .frames
                .get_mut(req.frame_idx)
            else {
                continue;
            };
            match found.entry {
                // Resident. The frame names it, which is what makes the
                // playhead able to march it.
                VolumeEntry::Ready(_) => {
                    frame.render_in_flight = false;
                    frame.image = Some(squallar_egui::pane::LoopFrameImage::Volume(
                        squallar_egui::pane::VolumeFrameGrid {
                            id: found.id,
                            target: req.target.clone(),
                        },
                    ));
                }
                VolumeEntry::Building => frame.render_in_flight = true,
                VolumeEntry::Refused(_) => {
                    frame.render_in_flight = false;
                    frame.render_failed = true;
                }
            }
        }

        for (pane_idx, targets) in held {
            self.volume_store.retain_set(pane_idx, &targets);
        }
    }

    /// Poll for finished cross-section loop cuts and upload their rasters.
    fn poll_loop_section_results(&mut self, ctx: &egui::Context) {
        while let Ok(mut sr) = self.channels.loop_section_receiver.try_recv() {
            let origin_pane = sr.pane_idx;
            let Some(pane) = self.gui.pane_mut(origin_pane) else {
                continue;
            };

            let counter = &mut self.texture_counter;
            let Some(placed) =
                accept_section_result(pane.time_state_mut(&known::RADAR), &mut sr, |color_image| {
                    *counter += 1;
                    ctx.load_texture(
                        format!("loop_section_{counter}"),
                        color_image,
                        egui::TextureOptions::NEAREST,
                    )
                })
            else {
                continue;
            };

            // Filed once under the cut's whole key — target, line and vector,
            // instant — for the reason the plan-view arrival files its
            // picture: every pane cutting this line through this volume
            // draws this one raster, linked or not. A re-cut against a moved
            // ladder replaces the stale one.
            let key = crate::loop_frame_store::LoopFrameKey::section(
                sr.target.clone(),
                sr.key.clone(),
                sr.timestamp,
            );
            let picture = squallar_egui::pane::LoopFrameImage::Section(placed);
            if let Some(replaced) =
                self.loop_frames
                    .insert(key.clone(), picture.clone(), origin_pane)
            {
                crate::loop_frame_store::discard(vec![replaced]);
            }
            for sibling_idx in 0..self.gui.pane_count() {
                if sibling_idx == origin_pane || self.gui.pane_cannot_loop(sibling_idx) {
                    continue;
                }
                let own_ladder = match frame_section(&self.loop_mgr, &sr.target, sr.timestamp) {
                    FrameSection::At(ladder) => Some(ladder),
                    FrameSection::Unrenderable | FrameSection::Pending => None,
                };
                let Some(sibling) = self.gui.pane_mut(sibling_idx) else {
                    continue;
                };
                let Some(sframe) = sibling
                    .time_state_mut(&known::RADAR)
                    .frame_accepting_section_broadcast_mut(
                        sr.timestamp,
                        &sr.target,
                        &sr.key,
                        sr.ladder,
                        own_ladder,
                    )
                else {
                    continue;
                };
                // Its own cut, if any, is now redundant: same key, same ladder,
                // same volume means the same raster, so its reply is dropped on
                // arrival by the target check.
                sframe.render_in_flight = false;
                sframe.image = Some(picture.clone());
                self.loop_frames.hold(sibling_idx, &key);
            }
        }
    }
}

/// Why no section can be cut from what the app holds for a site, or `None`
/// when one can.
fn section_source_refusal(
    base: Option<&nexrad_model::data::Scan>,
    overlay: Option<&nexrad_model::data::Scan>,
) -> Option<squallar_egui::pane::SectionUnavailable> {
    if let Some(current) =
        squallar_radar::current::resolve(base.map(Into::into), overlay.map(Into::into))
    {
        return current
            .sweeps()
            .is_empty()
            .then_some(squallar_egui::pane::SectionUnavailable::AwaitingFirstSweep);
    }
    if overlay.is_some_and(|scan| !scan.sweeps().is_empty()) {
        return Some(squallar_egui::pane::SectionUnavailable::AwaitingCoveragePattern);
    }
    Some(squallar_egui::pane::SectionUnavailable::AwaitingVolume)
}

/// **What one pane walk of [`App::loop_demand`] produced**: the loops' demand
/// on the pool, the telemetry's count of what they hold, the scene the
/// budget system prices, and the gridded overlays that scene's shared-grid
/// term was built from, keyed for the readout.
struct LoopWalk {
    demand: LoopDemand,
    counts: crate::loop_telemetry::LoopState,
    scene: Scene,
    overlay_grids: Vec<(squallar_source::id::LayerId, u64)>,
}

/// **What makes two panes' radar loops one loop for the pool.** Two panes
/// agreeing on every term of an arm hold one set of frames — one resident
/// grid set in the volume store, one set of pictures in the loop frame store
/// — and the pool charges it once, the second pane an alias of the first
/// ([`LoopDemand::alias`]).
#[derive(Clone, Debug, PartialEq)]
enum LoopIdentity {
    /// Site, product, and the ground and vector the grids are resampled with.
    Volume {
        site: String,
        product: squallar_radar::types::RadarProduct,
        key: Option<squallar_egui::pane::VolumeLoopKey>,
    },
    /// The picture's identity — site, product, the tilt where it selects the
    /// picture (by the render's own tenths bucket), a section's line and
    /// vector — **over the same window**: the pane's lookback and the instant
    /// it depicts. Two panes on one picture set at different lookbacks, or
    /// parked at different instants, list different frames: they share what
    /// overlaps through the loop frame store and are priced as two, since one
    /// grant would under-price the frames only one of them holds.
    Raster {
        site: String,
        product: squallar_radar::types::RadarProduct,
        elevation_tenths: Option<i32>,
        section: Option<squallar_egui::pane::SectionLoopKey>,
        span_secs: u64,
        mode: squallar_egui::pane::TimeMode,
    },
}

impl LoopIdentity {
    fn of(
        pane: &squallar_egui::pane::PaneState,
        ls: &squallar_egui::pane::LayerTimeState,
        product: squallar_radar::types::RadarProduct,
    ) -> Self {
        let site = radar_layer::site(ls).to_string();
        match ls.view {
            squallar_radar::types::RenderView::Volume => Self::Volume {
                site,
                product,
                key: ls.volume_key().cloned(),
            },
            squallar_radar::types::RenderView::PlanView
            | squallar_radar::types::RenderView::CrossSection => Self::Raster {
                site,
                product,
                elevation_tenths: ls
                    .rendered_for
                    .as_ref()
                    .filter(|_| ls.view.elevation_selects_picture(product))
                    .map(|target| squallar_egui::pane::elevation_tenths(target.elevation)),
                section: ls.section_key().cloned(),
                span_secs: pane.time.span_secs,
                mode: pane.time.mode,
            },
        }
    }
}

/// **One frame listing that landed**, and what the loop builder has to match
/// it against a pane with.
///
/// `site` is radar's own extra half — the NEXRAD site the listing was taken
/// for — and is `None` for every other layer, whose frames are addressed by
/// the layer id and the window alone.
pub(crate) struct LoopListingArrival {
    pub(crate) layer: squallar_source::id::LayerId,
    pub(crate) site: Option<String>,
    pub(crate) range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
}

/// **A listing becomes this layer's frame list**, sampled to `held` and with
/// the two recorded decisions the timeline caption reads back — and the
/// listing itself is kept on the timeline, so a changed allocation can
/// re-sample the list denser or sparser from what the source said exists.
///
/// The layer-agnostic half of [`accept_scan_listing`], and literally the same
/// code: radar calls it after its own site check and the two arms cannot
/// sample, order or park differently.
///
/// Returns the stamps that became frames, runs and all, or `None` when the
/// listing named nothing — a loop with no frames is not a loop, and the
/// caller switches it off.
///
/// **`held` is a closure, and the order is the reason.** A 3D loop's cap is
/// `frames_for_span`, which reads the layer's own `cadence_secs` — recorded
/// here, off this very listing. Taking the cap as a number would let a caller
/// compute it against the *previous* cadence (`None` on a fresh timeline,
/// which answers the whole render budget), and the 3D frame list would come
/// out at the budget instead of at the span. Pinned by
/// `a_slow_site_shortens_a_3d_loops_list_without_shortening_its_span`.
fn build_loop_frames(
    ls: &mut squallar_egui::pane::LayerTimeState,
    listing: squallar_source::time::FrameListing,
    held: impl FnOnce(&squallar_egui::pane::LayerTimeState) -> usize,
) -> Option<Vec<squallar_source::time::FrameStamp>> {
    if listing.frames.is_empty() {
        return None;
    }
    // The source's own cadence, read off the listing *before* the sampling
    // below throws stamps away. Once sampled there is no way back to it, and
    // it is what the timeline caption needs to tell "every frame" from "one in
    // five".
    let valids: Vec<chrono::NaiveDateTime> = listing.frames.iter().map(|s| s.valid).collect();
    ls.cadence_secs = median_step_secs(&valids);

    // Cap the frame list by evenly sampling the listing — endpoint-anchored,
    // so the window's two ends survive whatever the cap is.
    let held = held(ls);
    let total = listing.frames.len();
    let sample = squallar_egui::pane::listing_sample_indices(total, held);
    ls.sampled = Some(sample.is_some());
    let stamps: Vec<squallar_source::time::FrameStamp> = match sample {
        Some(indices) => indices.into_iter().map(|i| listing.frames[i]).collect(),
        None => listing.frames.clone(),
    };
    // The answer the frames were chosen from, kept whole: what a re-sample to
    // a changed allocation chooses from, denser or sparser.
    ls.listing = Some(listing);

    ls.phase = squallar_egui::pane::LoopPhase::Rendering;
    // Oldest-first, matching the listing order.
    ls.frames = stamps
        .iter()
        .map(|stamp| squallar_egui::pane::LoopFrame {
            timestamp: stamp.valid,
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    // A freshly built loop is parked on its newest frame; the pane's own
    // clock takes over at the next settle.
    ls.settle_playhead(squallar_egui::pane::TimeMode::Live);
    Some(stamps)
}

/// **Bytes one frame of `layer` costs on this pane** — measured, not scaled.
///
/// The pane is already drawing this layer at the current viewport, so the
/// texture on screen is the size a loop frame of it would be: the overlay
/// raster is planned from the pane rect and the overdraw margin, and a loop
/// frame is that same raster held instead of replaced. A 1280x960-**point**
/// pane measures 1920x1440 texels = 11.06 MB, which is why this is read
/// rather than assumed — the figure moves with the window.
///
/// Before the first raster there is no measurement to take, and the fallback
/// is **[`LoopFrameModel`]'s own `overlay` arm** — the same planner run on this
/// class's default window, which is what lets [`LoopPool`] price an overlay
/// loop it has never seen rasterized. It is not the radar loop frame's figure,
/// which was this fallback until WB-7 and is a different shape on every arm
/// (4 MiB against 9.44 MB on wasm, 16 MiB against 18.66 MB native).
///
/// [`LoopPool`]: crate::loop_pool::LoopPool
fn overlay_frame_bytes(
    pane: &squallar_egui::pane::PaneState,
    layer: &squallar_source::id::LayerId,
    budgets: &squallar_device_profile::budget::Budgets,
) -> usize {
    pane.overlay_cache(layer)
        .and_then(squallar_egui::overlay_cache::OverlayTextureCache::current)
        .map(|texture| texture.width as usize * texture.height as usize * 4)
        .filter(|bytes| *bytes > 0)
        .unwrap_or_else(|| LoopFrameModel::from_budgets(budgets).overlay)
}

/// **A non-radar layer's answer to [`settle_loop_phase`]'s `scan_available`**:
/// whether the layer is holding this frame's data.
///
/// This is the oracle WI-2 left as `|_| false`. That placeholder said "no data
/// exists for any frame", under which every frame settles the instant it is
/// created and a loop reads as finished before a byte of it has landed. The
/// layer's own `frames_resident` is the answer, and it is asked of the layer
/// rather than derived from the frame: a frame's `image` is a *texture*, and
/// what puts one there for a non-radar layer is
/// `App::dispatch_overlay_loop_renders` (WI-6b), which reads this same
/// residency answer before it asks for a raster at all.
///
/// The stamp comparison is on `valid` alone. `FrameStamp::run` is the
/// reference time a forecast frame came from — two runs can hold the same
/// valid hour — and a loop's frames are addressed by the instant they depict,
/// which is what the pane's clock names.
fn frames_are_resident(
    resident: &[chrono::NaiveDateTime],
) -> impl Fn(&squallar_egui::pane::LoopFrame) -> bool + '_ {
    move |frame| resident.contains(&frame.timestamp)
}

/// Take a scan listing for `site` into `ls`'s frame list, returning the downloads
/// it now owes.
fn accept_scan_listing(
    allocation: &LoopAllocation,
    pane_idx: usize,
    budgets: &squallar_device_profile::budget::Budgets,
    ls: &mut squallar_egui::pane::LayerTimeState,
    site: &str,
    listing: squallar_source::time::FrameListing,
    animating: usize,
) -> Option<FramePlan> {
    if !ls.is_active() || radar_layer::site(ls) != site {
        return None;
    }

    // Cap the downloads by evenly sampling the listing. A 3D loop's cap is its
    // *resident* one and is far lower, because for that kind the frame list and
    // the resident set are one thing — see `loop_frames_held`.
    let total = listing.frames.len();
    let model = LoopFrameModel::from_budgets(budgets);
    let Some(scans) = build_loop_frames(ls, listing, |ls| {
        layer_share(
            allocation,
            pane_idx,
            Some(loop_frames_held(allocation, pane_idx, ls, budgets)),
            model.bytes_for(ls.view),
            animating,
        )
    }) else {
        log::warn!("Loop: no {site} scans in the requested window; leaving loop mode");
        *ls = squallar_egui::pane::LayerTimeState::new();
        return None;
    };
    if ls.sampled == Some(true) {
        log::info!(
            "Loop: sampled {total} down to {} frames for {site}",
            scans.len()
        );
    }

    Some(FramePlan::new(
        site.to_string(),
        scans.iter().map(|stamp| stamp.valid).collect(),
    ))
}

/// The median gap between consecutive scan times, in whole seconds.
pub(super) fn median_step_secs(times: &[chrono::NaiveDateTime]) -> Option<u32> {
    let mut gaps: Vec<i64> = times
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).num_seconds())
        .filter(|secs| *secs > 0)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_unstable();
    u32::try_from(gaps[gaps.len() / 2]).ok()
}

/// Move a loop that is still `Rendering` on to whatever its frames have settled
/// into, returning `true` if the loop was switched off.
///
/// **Layer-agnostic (WI-2).** Both questions this asks about a loop's *data*
/// are the owning layer's to answer, so both arrive as closures and nothing in
/// this body names radar:
///
/// - `batch_settled` — has every frame this loop intends to render reached a
///   verdict, and has the layer finished dispatching for this pane? Radar
///   answers it from its `LoopDownloadManager`; see [`radar_batch_settled`].
/// - `still_arriving` — is any frame's data on its way? This is the gate that
///   stands between a loop with nothing to show and its own destruction.
///
/// The second one is why this could not be retargeted by moving the accessors
/// alone. It used to read a NEXRAD site straight out of `ls.anchor`, and
/// `radar_layer::site` answers `""` — not a panic — for a timeline with no
/// radar geometry. Nothing is ever in flight for `""`, so a non-radar loop
/// whose frames were still loading fell through to the `*ls = new()` below and
/// had its timeline wiped while its data was in the air. Pinned by
/// `a_model_loop_whose_frames_are_still_arriving_is_not_destroyed`.
fn settle_loop_phase(
    pane_idx: usize,
    ls: &mut squallar_egui::pane::LayerTimeState,
    batch_settled: impl Fn(&squallar_egui::pane::LayerTimeState) -> bool,
    still_arriving: impl Fn(&squallar_egui::pane::LayerTimeState) -> bool,
) -> bool {
    if !ls.is_active() || ls.is_render_ready() || ls.frames.is_empty() {
        return false;
    }
    if !batch_settled(ls) {
        return false;
    }
    if ls.frames.iter().any(|f| f.image.is_some()) {
        // A restored loop that was playing when its config was written starts
        // playing here, at the first moment "play" means anything, and the
        // request is spent as it fires — a pause afterwards stays paused.
        ls.phase = if std::mem::take(&mut ls.autoplay_on_ready) {
            squallar_egui::pane::LoopPhase::Playing
        } else {
            squallar_egui::pane::LoopPhase::Ready
        };
        return false;
    }
    if still_arriving(ls) {
        return false;
    }
    log::warn!("Loop: no frame on pane {pane_idx} could be rendered; leaving loop mode");
    *ls = squallar_egui::pane::LayerTimeState::new();
    true
}

/// Radar's answer to [`settle_loop_phase`]'s `batch_settled`: every frame it
/// means to render has a verdict **and** its downloads have all been
/// dispatched. `is_pane_done` means "dispatched", not "arrived" — the arrival
/// question is [`radar_still_arriving`].
fn radar_batch_settled(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    pane_idx: usize,
    ls: &squallar_egui::pane::LayerTimeState,
    budget: usize,
) -> bool {
    loop_batch_settled(loop_mgr, ls, budget) && loop_mgr.is_pane_done(pane_idx)
}

/// Radar's answer to [`settle_loop_phase`]'s `still_arriving`: whether any
/// frame of `ls` is waiting on a volume or a Level III pairing that is already
/// on the wire. This is the one call that reads `ls`'s NEXRAD site, and it is
/// reached only from the radar arm.
fn radar_still_arriving(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    ls: &squallar_egui::pane::LayerTimeState,
) -> bool {
    let Some(product) = loop_product(ls) else {
        return false;
    };
    ls.frames
        .iter()
        .any(|f| loop_mgr.frame_data_in_flight(radar_layer::site(ls), product, &f.timestamp))
}

/// **The radar arm of the readiness walk**, and the whole of it: this is what
/// `update_loop_readiness` calls for a radar slot, so the suites that call it
/// exercise production's own pair of closures rather than a second pair
/// written to agree with them. Two spellings of this wiring would be free to
/// drift, and the tests would keep passing while they did.
fn settle_radar_loop_phase(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    pane_idx: usize,
    ls: &mut squallar_egui::pane::LayerTimeState,
    budget: usize,
) -> bool {
    settle_loop_phase(
        pane_idx,
        ls,
        |ls| radar_batch_settled(loop_mgr, pane_idx, ls, budget),
        |ls| radar_still_arriving(loop_mgr, ls),
    )
}

/// The frame image a finished loop render describes.
fn rendered_image(
    rr: &crate::channels::LoopRenderResponse,
    texture: &egui::TextureHandle,
    gates: Option<squallar_radar::hover::SweepGates>,
) -> squallar_egui::pane::RadarImageData {
    squallar_egui::pane::RadarImageData {
        texture: texture.clone(),
        lat: rr.site_lat,
        lon: rr.site_lon,
        max_range_km: rr.max_range_km,
        placed: squallar_radar::types::ImageBounds::from_radar_site(
            rr.site_lat,
            rr.site_lon,
            rr.max_range_km,
        )
        .into(),
        nyquist_ms: rr.nyquist_ms,
        melting_layer_source: rr.melting_layer_source,
        storm_motion: rr.storm_motion,
        hover: Arc::new(squallar_radar::hover::HoverSource::from_volume(
            rr.polar.clone(),
            gates,
        )),
    }
}

/// The sweep a finished loop render was drawn from, for reading its numbers
/// back out.
fn frame_gates(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    rr: &crate::channels::LoopRenderResponse,
) -> Option<squallar_radar::hover::SweepGates> {
    let (scan, _) = loop_mgr.get_cached(&rr.target.site, &rr.timestamp)?;
    let product = crate::render_key::radar_field(&rr.target.product)?;
    // The volume's price comes from the cache that already computed it at
    // arrival, not from a second walk of the radials: this runs on the frame
    // thread, once per landed loop frame, and `scan_bytes` is O(radials).
    //
    // `get_cached` just answered for this key, and `cache_scan` files the
    // volume and its price together while `retain_scans` removes both by one
    // predicate — so the price is present whenever the volume is. The assert
    // is there because the fallback is a ZERO: if that invariant ever breaks,
    // this frame would price its pinned volume at nothing, which is the exact
    // silent undercount this whole change exists to remove.
    let priced = loop_mgr.cached_scan_price(&rr.target.site, &rr.timestamp);
    debug_assert!(
        priced.is_some(),
        "a cached volume with no price: {} at {}",
        rr.target.site,
        rr.timestamp
    );
    let scan_bytes = priced.unwrap_or(0);
    squallar_radar::hover::SweepGates::new(Arc::clone(scan), product, rr.snapped, scan_bytes)
}

/// Place a finished loop render on the frame of `ls` that asked for it, returning
/// the placed picture — texture and hover source, built once — so the caller can
/// file it and hand the same one to every sibling pane.
fn accept_render_result(
    ls: &mut squallar_egui::pane::LayerTimeState,
    rr: &mut crate::channels::LoopRenderResponse,
    gates: Option<squallar_radar::hover::SweepGates>,
    upload: impl FnOnce(egui::ColorImage) -> egui::TextureHandle,
) -> Option<squallar_egui::pane::RadarImageData> {
    let frame = ls.frame_awaiting_render_result_mut(rr.timestamp, &rr.target)?;
    frame.render_in_flight = false;

    let Some(color_image) = rr.image.take() else {
        frame.render_failed = true;
        return None;
    };

    let texture = upload(color_image);
    let image = rendered_image(rr, &texture, gates);
    frame.image = Some(squallar_egui::pane::LoopFrameImage::PlanView(image.clone()));
    Some(image)
}

/// [`accept_render_result`] for a finished cross-section cut.
fn accept_section_result(
    ls: &mut squallar_egui::pane::LayerTimeState,
    sr: &mut crate::channels::LoopSectionResponse,
    upload: impl FnOnce(egui::ColorImage) -> egui::TextureHandle,
) -> Option<squallar_egui::pane::SectionImageData> {
    let frame = ls.frame_awaiting_section_result_mut(sr.timestamp, &sr.target, &sr.key)?;
    frame.render_in_flight = false;

    // The axes travel with the raster and are `None` exactly when it is, so a
    // reply carrying one without the other is a bug upstream rather than a
    // frame to draw with the previous frame's scales.
    let (Some(color_image), Some(axes)) = (sr.image.take(), sr.axes) else {
        frame.render_failed = true;
        return None;
    };

    let image = squallar_egui::pane::SectionImageData {
        texture: upload(color_image),
        axes,
        tilt_elevations_deg: std::mem::take(&mut sr.tilt_elevations_deg),
        tilt_collected_ms: std::mem::take(&mut sr.tilt_collected_ms),
        ladder: sr.ladder,
    };
    frame.image = Some(squallar_egui::pane::LoopFrameImage::Section(image.clone()));
    Some(image)
}

/// Record a finished download: clear its in-flight mark and cache the scan.
fn apply_completed_download(
    loop_mgr: &mut squallar_radar::loop_downloads::LoopDownloadManager,
    resp: crate::channels::LoopScanDownloadResponse,
) {
    loop_mgr.complete_download(&resp.site, &resp.timestamp);
    // Skip failures — the mark is cleared either way so the frame can be retried.
    if let Some(volume) = resp.scan {
        loop_mgr.cache_scan(&resp.site, resp.timestamp, volume);
    }
}

/// Every UTC day the pairing windows of `queue`'s volumes touch, deduplicated.
fn pairing_days_for_frames(
    queue: &VecDeque<(chrono::NaiveDateTime, String)>,
) -> Vec<chrono::NaiveDate> {
    let mut days: Vec<chrono::NaiveDate> = Vec::new();
    for (ts, _) in queue {
        for day in squallar_radar::level3::pairing_days(*ts) {
            if !days.contains(&day) {
                days.push(day);
            }
        }
    }
    days
}

/// The data a loop keyed to `target` renders for `timestamp`: the Level II volume,
/// or every Level III object of that volume, whichever `target.product` reads.
///
/// Test-only since WO-M12d: the dispatch path asks radar for the *described job*
/// a frame's data makes and never holds the arms themselves. What the suites
/// below still pin through here is the keying — that a frame's data is looked up
/// under its own target's site and its own target's product.
#[cfg(test)]
fn frame_data(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> Option<squallar_radar::loop_downloads::LoopFrameData> {
    crate::render_key::radar_field(&target.product)
        .and_then(|p| loop_mgr.frame_data(&target.site, p, &timestamp))
}

/// What one frame's own data makes of the pane's elevation selection.
enum FrameSweep {
    /// The sweep the frame will be rendered at.
    At(f32),
    /// The data is here and carries nothing for this product: the volume has no
    /// such sweep, or the site generated no object for this volume. Terminal.
    Unrenderable,
    /// The data has not arrived yet.
    Pending,
}

/// The sweep frame `timestamp` of a loop keyed to `target` would be rendered at.
fn frame_sweep(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> FrameSweep {
    let Some(product) = crate::render_key::radar_field(&target.product) else {
        return FrameSweep::Unrenderable;
    };
    if product.is_level3() {
        return match loop_mgr.l3_frame_state(&target.site, product, &timestamp) {
            L3FrameState::Pending => FrameSweep::Pending,
            L3FrameState::Absent => FrameSweep::Unrenderable,
            L3FrameState::Ready => {
                match loop_mgr
                    .l3_frame_products(&target.site, product, &timestamp)
                    .as_deref()
                    .and_then(<[_]>::first)
                {
                    Some(first) => FrameSweep::At(first.message.pdb.elevation_angle()),
                    // `Ready` promised every code, so this is unreachable; a
                    // retired frame is still the right answer for a product that
                    // names no codes at all.
                    None => FrameSweep::Unrenderable,
                }
            }
        };
    }
    let Some((scan, _)) = loop_mgr.get_cached(&target.site, &timestamp) else {
        return FrameSweep::Pending;
    };
    match squallar_radar::render::find_closest_elevation(scan, product, target.elevation) {
        Some(snapped) => FrameSweep::At(snapped),
        None => FrameSweep::Unrenderable,
    }
}

/// The sweep `ls`'s own data for `timestamp` resolves `product`/`elevation` to, or
/// `None` if it has none or that data carries nothing for the product.
fn own_sweep(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    ls: &squallar_egui::pane::LayerTimeState,
    timestamp: chrono::NaiveDateTime,
    product: squallar_source::product::FieldId,
    elevation: f32,
) -> Option<f32> {
    // Resolved through the same function the dispatcher plans with, against the
    // receiver's own site: a second rule for "which sweep does this frame show"
    match frame_sweep(
        loop_mgr,
        &RenderTarget::new(radar_layer::site(ls).to_string(), &product, elevation),
        timestamp,
    ) {
        FrameSweep::At(sweep) => Some(sweep),
        FrameSweep::Unrenderable | FrameSweep::Pending => None,
    }
}

/// The sweep pair for offering `rr`'s finished image to the loop `ls`.
fn broadcast_sweep(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    ls: &squallar_egui::pane::LayerTimeState,
    rr: &crate::channels::LoopRenderResponse,
) -> BroadcastSweep {
    BroadcastSweep {
        rendered: rr.snapped,
        own: own_sweep(
            loop_mgr,
            ls,
            rr.timestamp,
            rr.target.product.clone(),
            rr.target.elevation,
        ),
    }
}

/// The product a loop's frames are keyed to, or `None` before the first dispatch.
///
/// `pub(super)` for `App::evict_unneeded_loop_scans`, which asks
/// `squallar_radar::loop_downloads::site_needs_decoded_source` what a site's
/// running loops render and must read the product through the same accessor
/// the dispatch path does.
pub(super) fn loop_product(
    ls: &squallar_egui::pane::LayerTimeState,
) -> Option<squallar_radar::types::RadarProduct> {
    ls.rendered_for
        .as_ref()
        .and_then(|t| crate::render_key::radar_field(&t.product))
}

/// **One row of the `live_loops` slice
/// [`squallar_radar::loop_downloads::site_needs_decoded_source`] reads**: the
/// site a radar timeline is anchored to and the product it renders, `None`
/// where the loop has not dispatched yet — which that predicate reads as
/// "keep", the safe direction.
///
/// Spelled once and called from both walks that build the slice — the
/// eviction sweep that retains by the predicate (`App::evict_unneeded_loop_scans`)
/// and the pane walk that prices by it ([`App::loop_demand`]) — so the two
/// cannot come to describe a different set of loops to the same function.
/// The caller has already established that `ls` is active.
pub(super) fn live_loop_row(
    ls: &squallar_egui::pane::LayerTimeState,
) -> (&str, Option<squallar_radar::types::RadarProduct>) {
    (radar_layer::site(ls), loop_product(ls))
}

/// Whether every frame `ls` intends to render has settled, given what has arrived.
fn loop_batch_settled(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    ls: &squallar_egui::pane::LayerTimeState,
    budget: usize,
) -> bool {
    let Some(product) = loop_product(ls) else {
        // Nothing dispatched yet, so nothing has settled.
        return false;
    };
    // Not merely "nothing in flight this instant": the render budget is shared with
    // static pane renders, so part of a batch can be starved and not yet spawned.
    ls.render_set_settled(budget, |f| {
        loop_mgr.frame_data_settled(radar_layer::site(ls), product, &f.timestamp)
    })
}

/// What one frame's own volume makes of a section loop's line.
enum FrameSection {
    /// The ladder fingerprint this frame would be cut from.
    At(u64),
    /// The volume is here and carries nothing to cut under this product.
    Unrenderable,
    /// The volume has not arrived yet.
    Pending,
}

/// The ladder frame `timestamp` of a section loop keyed to `target` would be cut
/// from.
fn frame_section(
    loop_mgr: &squallar_radar::loop_downloads::LoopDownloadManager,
    target: &RenderTarget,
    timestamp: chrono::NaiveDateTime,
) -> FrameSection {
    let Some((scan, _)) = loop_mgr.get_cached(&target.site, &timestamp) else {
        return FrameSection::Pending;
    };
    let sweeps: Vec<&nexrad_model::data::Sweep> = scan.sweeps().iter().collect();
    let Some(product) = crate::render_key::radar_field(&target.product) else {
        return FrameSection::Unrenderable;
    };
    match squallar_radar::sampler::ladder_fingerprint(scan.coverage_pattern(), &sweeps, product) {
        Some(ladder) => FrameSection::At(ladder),
        None => FrameSection::Unrenderable,
    }
}

/// The allocation an idle application has: the whole pool at this target's
/// floor, undivided.
#[cfg(test)]
pub(crate) fn test_loop_allocation() -> LoopAllocation {
    let budgets = test_budgets();
    let limits = crate::loop_pool::LoopPoolLimits::from_budgets(&budgets);
    crate::loop_pool::LoopPool::new(limits.floor, limits).plan(
        LoopFrameModel::from_budgets(&budgets),
        &LoopDemand::default(),
    )
}

/// The tile term of the scene: one `TileNeed` per cache role whose last whole
/// pass wanted anything, off `cache_ledger`'s levels. The cells are what the
/// pass asked for, drawn level and ancestor net; the per-tile cost is the
/// mean charge of the role's resident entries, floored at the marker's node.
fn tile_needs() -> Vec<squallar_device_profile::scene::TileNeed> {
    use squallar_egui::tile_source::cache_ledger::{ROLES, totals};

    ROLES
        .iter()
        .map(|role| totals(*role))
        .filter(|t| t.wanted_on_glass + t.wanted_net > 0)
        .map(|t| squallar_device_profile::scene::TileNeed {
            tiles_on_glass: t.wanted_on_glass as usize,
            ancestor_net: t.wanted_net as usize,
            bytes_per_tile: (t.resident_bytes / t.resident_entries.max(1))
                .max(squallar_egui::tile_source::byte_lru::MARKER_BYTES)
                as usize,
        })
        .collect()
}

/// This build's own budgets, for the tests that take them as an argument.
#[cfg(test)]
pub(crate) fn test_budgets() -> squallar_device_profile::budget::Budgets {
    squallar_device_profile::budget::resolve(
        &squallar_device_profile::budget::DeviceProfile::for_target(),
    )
}

/// Frames this loop may keep **textured**, which is the term that bounds
/// memory.
///
/// **The grant is the answer** for a pane the plan has seen: its base already
/// holds the pane's lookback to the rung's span (`Budgets::frames_for_span_of`,
/// which is what `fit` charged), and everything above it is the balloon the
/// pool granted, so clamping it to the span again here would take the balloon
/// back. Two cases still read the span clamp, and both are the plan not having
/// caught up yet: a pane the plan has not seen, held to its kind's ceiling as
/// a loop with no grant always was; and a grant planned before this loop's
/// listing said what its cadence is, held to the rung's span at that cadence
/// exactly as before — the demand carries the cadence on the next walk, and
/// the plan follows within the dwell.
pub(super) fn loop_render_budget(
    allocation: &LoopAllocation,
    pane_idx: usize,
    ls: &squallar_egui::pane::LayerTimeState,
    budgets: &squallar_device_profile::budget::Budgets,
) -> usize {
    match allocation.grant_for_pane(pane_idx) {
        Some(grant) if grant.cadence_secs.is_some() || ls.cadence_secs.is_none() => grant.frames,
        Some(grant) => grant.frames.min(budgets.frames_for_span(ls.cadence_secs)),
        None => allocation
            .frames_for(ls.view)
            .min(budgets.frames_for_span(ls.cadence_secs)),
    }
}

/// **Frames that have to be settled before a loop is ready to play**: the
/// base — what `fit` made room for, and what the loop was held to before a
/// balloon existed — never the balloon. A loop granted sixty frames over a
/// base of twenty-five starts playing when the twenty-five nearest the
/// playhead are textured, exactly when it did before, and the dispatch fills
/// the other thirty-five in behind it. Gating readiness on the whole grant
/// would trade time-to-first-playback for density, which no ruling asked for.
/// A pane the plan has not seen reads [`loop_render_budget`]'s own answer,
/// which for it is already the span-held figure.
pub(super) fn loop_ready_budget(
    allocation: &LoopAllocation,
    pane_idx: usize,
    ls: &squallar_egui::pane::LayerTimeState,
    budgets: &squallar_device_profile::budget::Budgets,
) -> usize {
    let textured = loop_render_budget(allocation, pane_idx, ls, budgets);
    match allocation.grant_for_pane(pane_idx) {
        Some(grant) => textured.min(grant.base),
        None => textured,
    }
}

/// **What one pane's animating layers ask the pool for, as one need.** The
/// pool sees a pane as one loop and [`layer_share`] divides the pane's bytes
/// equally across the layers it animates, each converting at its own price —
/// so the need has to be sized for all of them, or a second animating layer
/// halves the first's frames below what its own listing named. Its price is
/// the dearest layer's frame times the number of layers, and its frames are
/// the most any layer asks for (base and ceiling alike); every layer's equal
/// slice then buys at least its own frames, and the divider holds each to its
/// own list. One layer alone is exactly its own need. `primary` is the layer
/// the pane's clock walks — radar's timeline where radar loops, the first
/// animating layer otherwise — and its cadence is the one the grant records.
fn pane_loop_need(
    pane: &squallar_egui::pane::PaneState,
    pane_idx: usize,
    kind: LoopKind,
    primary: &squallar_egui::pane::LayerTimeState,
    budgets: &squallar_device_profile::budget::Budgets,
    model: &LoopFrameModel,
) -> LoopNeed {
    let mut layers = 0usize;
    let mut price = 0usize;
    let mut span_secs = pane.time.span_secs;
    let mut base_frames = 0usize;
    let mut max_frames = 0usize;
    for slot in pane.animating_layers() {
        layers += 1;
        let ls = &slot.time;
        let layer_price = if slot.id == squallar_source::id::known::RADAR {
            model.bytes_for(ls.view)
        } else {
            overlay_frame_bytes(pane, &slot.id, budgets)
        };
        price = price.max(layer_price);
        let span = pane.time.span_secs.max(ls.span_secs);
        span_secs = span_secs.max(span);
        let base = budgets.frames_for_span_of(span as usize, ls.cadence_secs);
        base_frames = base_frames.max(base);
        // What this layer lists, read off the answer its frames were chosen
        // from — `None` before a listing has landed, and then the base is the
        // most it can hold, because nothing says what more exists.
        let listed = ls.listing.as_ref().map(|listing| listing.frames.len());
        max_frames = max_frames.max(loop_ceiling_frames(
            listed,
            span,
            ls.cadence_secs,
            base,
            budgets.loop_frames_held,
        ));
    }
    if layers == 0 {
        // Reachable only for a pane whose primary timeline is active while
        // no slot is — a fixture's shape, not the walk's — priced as the
        // primary alone.
        layers = 1;
        price = model.price(kind);
        base_frames = budgets.frames_for_span_of(span_secs as usize, primary.cadence_secs);
        max_frames = base_frames;
    }
    LoopNeed {
        key: LoopKey { pane: pane_idx },
        kind,
        span_secs,
        cadence_secs: primary.cadence_secs,
        frame_bytes: price.saturating_mul(layers),
        base_frames,
        max_frames,
    }
}

/// Frames a loop of this view **holds**, before the pane's own layers divide
/// it — see [`layer_share`].
pub(super) fn loop_frames_held(
    allocation: &LoopAllocation,
    pane_idx: usize,
    ls: &squallar_egui::pane::LayerTimeState,
    budgets: &squallar_device_profile::budget::Budgets,
) -> usize {
    match ls.view {
        squallar_radar::types::RenderView::Volume => {
            loop_render_budget(allocation, pane_idx, ls, budgets)
        }
        squallar_radar::types::RenderView::PlanView
        | squallar_radar::types::RenderView::CrossSection => budgets.loop_frames_held,
    }
}

/// **One animating layer's share of the pane's loop allowance — divided in
/// BYTES, not in frame count.**
///
/// The pool's allowance is bytes, and a pane animating two layers spends it
/// twice. Splitting the *count* equally is right only while every animating
/// layer's frame costs the same, and they do not: a radar plan-view frame is
/// `LOOP_IMAGE_SIZE`² × 4 (4 MiB on wasm, 16 MiB native) and a model or
/// satellite frame is the pane's own raster. So each animating layer takes an
/// equal slice of the pane's bytes — its grant's frames at its grant's price,
/// or the pool's own summary for a pane the plan has not seen
/// ([`LoopAllocation::share_bytes_for`]) — and converts it at **its own**
/// price. A pane animating a 4 MiB layer beside a 16 MiB one holds them
/// **4:1**, and the two together fit the bytes — where an equal count split
/// would hold 5:1 of the bytes it was given.
///
/// `count_cap` is what this layer may hold on top of what the bytes allow:
///
/// * **`Some(n)` — radar.** `n` is [`loop_frames_held`], a *frame-list* length
///   (downloaded scans), of which only the render set is ever textured. **A
///   pane animating radar alone therefore gets `n` exactly, untouched, on
///   every arm** — the bytes have nothing to say about a list nobody is
///   texturing, and this land moves no radar-only count anywhere. It is when a
///   second layer starts animating that the pane's texture bytes become the
///   binding term and cap the list at what it can afford to show.
/// * **`None` — every other layer**, whose frame list *is* its textured set
///   (`build_loop_frames` sizes it from this, and the dispatch evicts to it).
///   Bytes are the only bound, plus the floor.
///
/// The **floor of [`MIN_LOOP_FRAMES_PER_PANE`]** applies to every divided
/// answer: one frame is a still picture, and a layer that cannot hold two
/// cannot animate at all, so the floor is where the allowance stops being
/// divisible rather than a cushion. **On a share that does not buy two frames
/// the floor wins and the byte bound is exceeded** — one frame over, by
/// construction, which is the alternative to an animation that cannot animate.
/// It is not applied to the undivided radar answer: a view whose own allowance
/// is legitimately below two is not silently raised by a division that did not
/// happen.
///
/// [`MIN_LOOP_FRAMES_PER_PANE`]: squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE
pub(super) fn layer_share(
    allocation: &LoopAllocation,
    pane_idx: usize,
    count_cap: Option<usize>,
    frame_bytes: usize,
    animating: usize,
) -> usize {
    if let Some(cap) = count_cap
        && animating <= 1
    {
        return cap;
    }
    // A frame that costs nothing is a model built wrong; the floor answers
    // rather than a division by zero.
    let by_bytes = (allocation.share_bytes_for(pane_idx) / animating.max(1))
        .checked_div(frame_bytes)
        .unwrap_or(squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE);
    by_bytes
        .min(count_cap.unwrap_or(usize::MAX))
        .max(squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE)
}

/// [`accept_scan_listing`] under a name the sibling test modules can reach —
/// the function itself is private to this module and stays that way. Takes
/// the bare instants the suites list with and wraps them as the listing radar
/// would have answered (no runs, the window read off its ends), for pane 0.
#[cfg(test)]
pub(crate) fn accept_scan_listing_for_test(
    allocation: &LoopAllocation,
    budgets: &squallar_device_profile::budget::Budgets,
    ls: &mut squallar_egui::pane::LayerTimeState,
    site: &str,
    scans: Vec<chrono::NaiveDateTime>,
    animating: usize,
) -> Option<FramePlan> {
    accept_scan_listing(
        allocation,
        0,
        budgets,
        ls,
        site,
        listing_of_for_test(scans),
        animating,
    )
}

/// A listing radar would have answered for `scans`: the instants alone, the
/// window read off its two ends, and complete.
#[cfg(test)]
pub(crate) fn listing_of_for_test(
    scans: Vec<chrono::NaiveDateTime>,
) -> squallar_source::time::FrameListing {
    let range = match (scans.first(), scans.last()) {
        (Some(first), Some(last)) => (*first, *last),
        _ => (
            chrono::NaiveDateTime::default(),
            chrono::NaiveDateTime::default(),
        ),
    };
    squallar_source::time::FrameListing {
        range,
        frames: scans
            .into_iter()
            .map(|valid| squallar_source::time::FrameStamp { valid, run: None })
            .collect(),
        complete: true,
    }
}

/// A 3D loop frame the dispatcher intends to make resident.
pub(crate) struct LoopVolumeRequest {
    pub pane_idx: usize,
    pub frame_idx: usize,
    pub target: squallar_egui::pane::VolumeTarget,
    /// This frame has already been ruled out. It is planned anyway so the
    /// resident set the dispatcher states names the whole frame list, and it
    /// is never dispatched for.
    pub retired: bool,
}

/// A cross-section loop frame the dispatcher intends to cut.
pub(crate) struct LoopSectionRequest {
    pub(crate) pane_idx: usize,
    pub(crate) frame_idx: usize,
    pub(crate) timestamp: chrono::NaiveDateTime,
    /// The site/product half of the key this cut is for.
    pub(crate) target: RenderTarget,
    /// The line/storm-motion half.
    pub(crate) key: squallar_egui::pane::SectionLoopKey,
    /// The ladder this frame's own volume resolves, resolved once during
    /// planning and carried through so the staleness test, the donor search and
    /// the dispatch stamp all read the one value.
    pub(crate) ladder: u64,
    pub(crate) site_lat: f64,
    pub(crate) site_lon: f64,
}

/// Whether a cut for this frame and key is already queued in this dispatch pass.
fn section_already_queued<'a>(
    mut queued: impl Iterator<Item = &'a LoopSectionRequest>,
    timestamp: chrono::NaiveDateTime,
    target: &RenderTarget,
    key: &squallar_egui::pane::SectionLoopKey,
) -> bool {
    // A cut's picture is a function of the line, the volume and the storm
    // motion; the tilt is not an input to it, and `CrossSection` is what says so.
    queued.any(|r| {
        r.timestamp == timestamp
            && r.target
                .matches(target, squallar_radar::types::RenderView::CrossSection)
            && &r.key == key
    })
}

/// A loop frame render the dispatcher intends to spawn.
struct LoopRenderRequest {
    pane_idx: usize,
    frame_idx: usize,
    timestamp: chrono::NaiveDateTime,
    /// The pane's render target: site plus *selected* product and elevation. What the
    /// result is keyed on — never what the renderer is given. See `render_params`.
    target: RenderTarget,
    /// `target.elevation` resolved to a sweep angle this frame's own scan carries.
    snapped: f32,
    site_lat: f64,
    site_lon: f64,
}

impl LoopRenderRequest {
    /// The inputs the renderer is handed.
    fn render_params(&self) -> crate::render_dispatch::RenderParams {
        crate::render_dispatch::RenderParams {
            product: crate::render_key::radar_field(&self.target.product)
                .expect("a loop render request names a field the radar layer registers"),
            elevation: self.snapped,
            lat: self.site_lat,
            lon: self.site_lon,
        }
    }
}

/// A loop frame the store can satisfy without a render: the picture, and the
/// key it was filed under so the receiving pane is recorded as a holder.
struct LoopCloneRequest {
    dest_pane: usize,
    dest_frame: usize,
    key: crate::loop_frame_store::LoopFrameKey,
    picture: squallar_egui::pane::LoopFrameImage,
}

/// Whether `queued` already covers a render for `timestamp` at `target`.
fn render_already_queued<'a>(
    mut queued: impl Iterator<Item = &'a LoopRenderRequest>,
    timestamp: chrono::NaiveDateTime,
    target: &RenderTarget,
    snapped: f32,
) -> bool {
    // A `LoopRenderRequest` carries the site coordinates it builds `RenderParams`
    // from, so it is a plan view by construction. The snapped term stays: it is
    // sweep *agreement*, not identity.
    queued.any(|r| {
        r.timestamp == timestamp
            && r.target
                .matches(target, squallar_radar::types::RenderView::PlanView)
            && (r.snapped - snapped).abs() <= ELEVATION_TOLERANCE
    })
}

/// The two running-total sentences, against the browser rig's own probes for
/// them — read out of `drive.py` rather than restated.
#[path = "app_render/raster_telemetry_line_tests.rs"]
#[cfg(test)]
mod raster_telemetry_line_tests;

/// The native rig seeds the keys this app reads, into the filenames its store
/// opens — one scene's path through shell, python and two rust crates.
#[path = "app_render/native_seed_pin_tests.rs"]
#[cfg(test)]
mod native_seed_pin_tests;

/// The five frame timing sentences, pinned word for word, and the key that
/// makes them loud.
#[path = "app_render/frame_telemetry_line_tests.rs"]
#[cfg(test)]
mod frame_telemetry_line_tests;

/// The JavaScript those probes are written in, checked by a JS engine: every
/// embedded block parses, and every console scrape executes without throwing
/// and hands back every family it was fed.
#[path = "app_render/rig_js_tests.rs"]
#[cfg(test)]
mod rig_js_tests;

/// The gesture player's arming seam: absent key and variable arm nothing —
/// the non-vacuity pair's dormant half — and the key is pinned.
#[path = "app_render/gesture_arming_tests.rs"]
#[cfg(test)]
mod gesture_arming_tests;

/// The order one frame is assembled in.
#[path = "app_render/declared_nyquist_dispatch_tests.rs"]
#[cfg(test)]
mod declared_nyquist_dispatch_tests;

#[path = "app_render/frame_build_order_tests.rs"]
#[cfg(test)]
mod frame_build_order_tests;

/// Where the per-pixel unmultiply is allowed to run, and where it is not.
#[path = "app_render/frame_thread_conversion_tests.rs"]
#[cfg(test)]
mod frame_thread_conversion_tests;

/// Where the frame's retired overlay payloads are actually freed.
#[path = "app_render/retired_discard_tests.rs"]
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod retired_discard_tests;

/// What the overlay poller puts on the GPU, read back from egui's own texture
/// delta rather than inferred.
#[path = "app_render/overlay_upload_tests.rs"]
#[cfg(test)]
mod overlay_upload_tests;

/// One sweep is one texture, however many panes are showing it — counted the
/// same way, off the delta, because the cost being removed is the upload and
/// not the picture.
#[path = "app_render/radar_texture_sharing_tests.rs"]
#[cfg(test)]
mod radar_texture_sharing_tests;

#[path = "app_render/frame_order_tests.rs"]
#[cfg(test)]
mod frame_order_tests;

/// The renderer pins that stayed behind — each scrapes a file this
/// crate owns (`present_frame`, the one `EguiRenderer::new` call, the wake).
#[path = "app_render/egui_frame_pin_tests.rs"]
#[cfg(test)]
mod egui_frame_pin_tests;

/// What `poll_level3_results` does with a channel holding more than one answer.
#[path = "app_render/level3_poll_tests.rs"]
#[cfg(test)]
mod level3_poll_tests;

/// The launch that has never seen a radar: what a first catalogue does, and
/// what every later one must not.
#[path = "app_render/first_launch_tests.rs"]
#[cfg(test)]
mod first_launch_tests;

#[path = "app_render/loop_dispatch_tests.rs"]
#[cfg(test)]
mod loop_dispatch_tests;

/// Frame supply and residency for a non-radar transport layer (WI-5): a
/// listing becoming a frame list, the byte cap it is sampled to, the fetches
/// it owes, and the layer's own answer to what it is holding.
#[path = "app_render/loop_supply_tests.rs"]
#[cfg(test)]
mod loop_supply_tests;

/// The producer for a non-radar loop's pictures (WI-6b): one raster per frame
/// at that frame's own stamp, filed back onto the frame that asked, bounded by
/// the pane's byte share.
#[path = "app_render/loop_overlay_render_tests.rs"]
#[cfg(test)]
mod loop_overlay_render_tests;

/// One copy of a 2D loop frame, drawn on every pane that shows it, linked or
/// not: the store, the broadcast, the eviction union and the pool's price.
#[path = "app_render/loop_frame_sharing_tests.rs"]
#[cfg(test)]
mod loop_frame_sharing_tests;

/// Playback on the transport layer: the gate, the start frame, the flip and
/// radar's unchanged tick.
#[path = "app_render/loop_playback_transport_tests.rs"]
#[cfg(test)]
mod loop_playback_transport_tests;

/// What the satellite layer puts on the glass under another layer's transport.
#[path = "app_render/satellite_loop_draw_tests.rs"]
#[cfg(test)]
mod satellite_loop_draw_tests;

/// The satellite loop (WB-11), end to end against the **real** GMGSI handler
/// rather than a double.
#[path = "app_render/gmgsi_loop_tests.rs"]
#[cfg(test)]
mod gmgsi_loop_tests;

/// The national mosaic loop (WB-10), end to end against the **real** MRMS
/// handler rather than a double — and the clock ruling, read at the pane.
#[path = "app_render/mrms_loop_tests.rs"]
#[cfg(test)]
mod mrms_loop_tests;

/// What a GLM loop puts on the glass, frame by frame.
#[path = "app_render/glm_loop_draw_tests.rs"]
#[cfg(test)]
mod glm_loop_draw_tests;

/// The cross-section loop's dispatch, placement and frame-thread pacing.
#[path = "app_render/loop_section_tests.rs"]
#[cfg(test)]
mod loop_section_tests;

/// The 3D loop's dispatch: what becomes resident, what the resident set is
/// bounded by, and what a region change releases before it rebuilds.
#[path = "app_render/loop_volume_tests.rs"]
#[cfg(test)]
mod loop_volume_tests;

/// What a 3D pane the layout stopped showing gives back, and what the release
/// beside it must not touch.
#[path = "app_render/hidden_pane_volume_tests.rs"]
#[cfg(test)]
mod hidden_pane_volume_tests;

/// What the loop timer does with a playback speed no slider could have set.
#[path = "app_render/loop_interval_tests.rs"]
#[cfg(test)]
mod loop_interval_tests;

#[path = "app_render/layer_share_tests.rs"]
#[cfg(test)]
mod layer_share_tests;

#[path = "app_render/loop_balloon_tests.rs"]
#[cfg(test)]
mod loop_balloon_tests;

/// The Level III half of the loop: pairing a bucket object to each frame's volume,
/// what a gap does, and what happens when a pane retargets across the datasource
/// line mid-loop.
#[path = "app_render/loop_level3_tests.rs"]
#[cfg(test)]
mod loop_level3_tests;

/// What bounds the loop's two data caches: the decoded volumes — the fourth
/// holder of whole `Arc<Scan>`s — and the paired Level III objects beside them.
#[path = "app_render/loop_scan_cache_tests.rs"]
#[cfg(test)]
mod loop_scan_cache_tests;

/// Which timeline the render funnel addresses (WO-T3.8): the arrival path, the
/// broadcast and the dispatch pass all file radar's payloads in radar's own
/// frame list, and go on doing so while another layer holds the transport.
#[path = "app_render/radar_timeline_addressing_tests.rs"]
#[cfg(test)]
mod radar_timeline_addressing_tests;

/// The plan-view render pipeline against a pane that has no plan view.
#[path = "app_render/pane_kind_render_filter_tests.rs"]
#[cfg(test)]
mod pane_kind_render_filter_tests;

/// The restore's one precondition: egui knows this device's texture limit.
#[path = "app_render/restore_texture_limit_tests.rs"]
#[cfg(test)]
mod restore_texture_limit_tests;

/// A restored image describes itself too.
#[path = "app_render/restore_describes_its_image_tests.rs"]
#[cfg(test)]
mod restore_describes_its_image_tests;

/// What a section pane is told when it cannot be cut, and when the picture on
/// screen has stopped being the truth.
#[path = "app_render/section_dispatch_tests.rs"]
#[cfg(test)]
mod section_dispatch_tests;

/// What `poll_level3_results` does with sounding responses: the same drain and
/// fetch-generation gate as everything else on it, plus the keep-on-failure
/// rule that makes the TTL retry loop safe.
#[path = "app_render/sounding_poll_tests.rs"]
#[cfg(test)]
mod sounding_poll_tests;

/// A pane keeps the picture it has until the next one is whole.
#[path = "app_render/raster_hold_tests.rs"]
#[cfg(test)]
mod raster_hold_tests;

/// What `apply_render_to_pane` does with a finished image beyond placing it.
#[path = "app_render/stamping_tests.rs"]
#[cfg(test)]
mod stamping_tests;

/// One sweep is one *render*, however many panes are looking at it — the
/// sibling of `radar_texture_sharing_tests`, one step earlier in the same path.
#[path = "app_render/one_render_per_sweep_tests.rs"]
#[cfg(test)]
mod one_render_per_sweep_tests;

/// The arrival-time extraction cache: a volume's arrival performs
/// the plan-view `RenderInput::extract` walks off-thread for the panes
/// showing the site, and the dispatch serves them from the cache — zero
/// frame-thread extraction on a hit, today's inline walk on a miss.
#[path = "app_render/extract_cache_tests.rs"]
#[cfg(test)]
mod extract_cache_tests;

/// The adjacent-tilt pre-render: one speculative render after a
/// delivered plan view, into the existing RenderCache, gated off wasm and
/// small budgets — never marking a pane, never more than one at a time.
#[path = "app_render/speculative_render_tests.rs"]
#[cfg(test)]
mod speculative_render_tests;

/// The frames between a dispatched overlay render and the layer being switched
/// off — where a result that cannot be recalled lands on a pane that no longer
/// wants it.
#[path = "app_render/overlay_disable_race_tests.rs"]
#[cfg(test)]
mod overlay_disable_race_tests;

/// A raster with no ink in it costs no picture and still clears the pane it
/// reaches — the 10.8%-19.1% of Tier-2 pictures that were full-size
/// transparent buffers.
#[cfg(test)]
#[path = "app_render/blank_raster_tests.rs"]
mod blank_raster_tests;

/// The overlay half of the hold: a pane keeps the layer picture it has —
/// alerts, outlooks — until the next one's pixels have all landed, the swap
/// leaves the radar caption alone, and a renderer rebuild releases what it can
/// never deliver.
#[path = "app_render/overlay_hold_tests.rs"]
#[cfg(test)]
mod overlay_hold_tests;

// ---- FRAME_PUMP wrappers ----

pub(super) fn pump_poll_render_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_render_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_poll_section_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_section_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_poll_level3_results(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.poll_level3_results();
}

pub(super) fn pump_poll_site_catalogue(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.poll_site_catalogue();
}

pub(super) fn pump_poll_overlay_render_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_overlay_render_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_accept_loop_scan_listings(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.accept_loop_scan_listings();
}

pub(super) fn pump_poll_loop_scan_download_results(
    app: &mut super::App,
    _ctx: Option<&egui::Context>,
) {
    app.poll_loop_scan_download_results();
}

pub(super) fn pump_poll_loop_l3_list_results(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.poll_loop_l3_list_results();
}

pub(super) fn pump_poll_loop_l3_fetch_results(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.poll_loop_l3_fetch_results();
}

pub(super) fn pump_poll_loop_render_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_loop_render_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_poll_loop_section_results(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.poll_loop_section_results(ctx.expect("Apply rows run from setup_egui_frame"));
}

pub(super) fn pump_advance_loop_playback(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.advance_loop_playback();
}

pub(super) fn pump_poll_extract_results(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.render.poll_extract_results();
}

pub(super) fn pump_dispatch_pane_renders(app: &mut super::App, ctx: Option<&egui::Context>) {
    app.dispatch_pane_renders(ctx.expect("Dispatch rows run from setup_egui_frame"));
}

pub(super) fn pump_dispatch_section_renders(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.dispatch_section_renders();
}

pub(super) fn pump_dispatch_loop_renders(app: &mut super::App, _ctx: Option<&egui::Context>) {
    app.dispatch_loop_renders();
}

pub(super) fn pump_dispatch_overlay_loop_renders(
    app: &mut super::App,
    _ctx: Option<&egui::Context>,
) {
    app.dispatch_overlay_loop_renders();
}

/// **Idle means idle**: a pane whose texture layers each hold a picture asks
/// for no further rasters, driven end to end through the production doors.
#[path = "app_render/idle_raster_tests.rs"]
#[cfg(test)]
mod idle_raster_tests;

/// A 3D pane is priced at the size and ground pass the painter last fitted
/// its offscreen from, not at the window's.
#[path = "app_render/pane_need_px_tests.rs"]
#[cfg(test)]
mod pane_need_px_tests;

/// Host spare is the model's figure bounded by the heap's, on both arms.
#[path = "app_render/host_spare_tests.rs"]
#[cfg(test)]
mod host_spare_tests;

/// The budget readout is composed on the telemetry tick that reads it, and
/// never on the frame path — the count, not the figures.
#[path = "app_render/budget_readout_cadence_tests.rs"]
#[cfg(test)]
mod budget_readout_cadence_tests;
