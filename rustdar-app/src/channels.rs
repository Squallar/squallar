use chrono::NaiveDateTime;
use nexrad_model::data::Scan;
use rustdar_egui::pane::RenderTarget;
use rustdar_geo::GeoBounds;
use rustdar_overlays::render::overlay_state::SourceEvent;
use rustdar_overlays::render::rasterize::HitMap;
use rustdar_radar::level3::Level3Product;
use rustdar_radar::types::RadarProduct;
use rustdar_source::id::LayerId;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

pub struct ScanData {
    pub scan: Scan,
    /// What the volume's cuts declared their Nyquist velocities to be.
    ///
    /// Carried beside the `Scan` because the model type has no field for it — see
    /// [`rustdar_radar::nyquist`].
    pub declared_nyquist: rustdar_radar::nyquist::DeclaredNyquist,
    pub site: String,
    pub timestamp: NaiveDateTime,
}

/// **Who asked for a scan.**
///
/// Carried from `spawn_fetch` through [`ScanResponse`] to the decode landing,
/// because delivery has to know: an archive volume is not a broadcast. A pane
/// that scrubbed owns the volume it scrubbed to, and writing it onto a
/// same-site pane parked at its own moment is the defect `UNLINK_NOTE`
/// promises against.
///
/// It is also the key half of the fetch generation. `is_scan_stale` used to
/// mean "a newer request for this *site* superseded yours", which cancelled a
/// same-site sibling's in-flight fetch: A scrubs at generation 5, B scrubs at
/// 6, and A's reply is thrown away. Per requester, only A's own next request
/// supersedes A — while a genuine re-request still does, which keying on the
/// timestamp instead would have destroyed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FetchRequester {
    /// Nobody in particular: the archive auto-poll, a site switch, the refetch
    /// a retired chunk feed falls back to. Every pane on the site takes it,
    /// and the site-wide generation is what supersedes it.
    Site,
    /// One pane navigated, and this volume is that pane's and its time
    /// group's.
    Pane(usize),
}

pub struct ScanResponse {
    pub generation: u64,
    pub site: String,
    /// Which pane this volume was fetched for, and so which panes it may be
    /// delivered to. See [`FetchRequester`].
    pub requester: FetchRequester,
    pub result: Result<ScanData, String>,
    pub is_auto_poll: bool,
}

/// What a render produced: the texture, the half-width it was projected at,
/// and the per-pixel value grid a hover reads.
pub struct RenderedImage {
    /// Already in egui's pixel layout, and already an `Arc`, because the frame
    /// thread must neither convert it nor copy it.
    ///
    /// The per-pixel unmultiply is 16 MiB at the base size and 64 MiB long range;
    /// on the frame thread it measured 6.4 ms at 2048² and 31.0 ms at 4096² (7950X,
    /// release, medians of 11 runs of a real KDMX 0.5° cut) against a 16.7 ms
    /// budget. `offload::execute` premultiplies inside the job instead.
    pub image: Arc<egui::ColorImage>,
    /// [`rustdar_radar::types::plan_view_extent_km`] of the sweep's reach, and so
    /// the only thing that says where these pixels sit on the ground.
    pub max_range_km: f64,
    /// The gates behind these pixels, for the readout under the pointer — see
    /// [`rustdar_radar::hover::HoverSource`]. The same numbers at the resolution
    /// the radar took them, about a fortieth of a `side²` `f32` raster grid.
    pub hover: Arc<rustdar_radar::hover::HoverSource>,
    /// Where the drawn sweep's cut declared its velocity folds, m/s, or `None` for
    /// a raster no single cut is behind.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer these pixels were classified against came from, or
    /// `None` for a raster that classified nothing. Not recoverable at the far end.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where the storm motion vector these pixels were shifted by came from, or
    /// `None` for a raster that shifted nothing.
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
}

pub struct RenderResponse {
    /// `None` where the renderer found nothing to draw.
    ///
    /// A render that answers nothing still has to report back: `render_in_flight`
    /// is cleared on receipt of this message and nowhere else outside
    /// `reset_panes*`. An abandoned render sends nothing at all — the send is
    /// gated on `results_wanted`.
    pub rendered: Option<RenderedImage>,
    pub product: RadarProduct,
    pub elevation: f32,
    pub generation: u64,
    pub pane_idx: usize,
    /// `Some(site)` marks a speculative result: an adjacent-tilt pre-render no
    /// pane asked for, so `pane_idx` is a sentinel the receiver never reads on this
    /// arm. Its whole delivery is an insert into the RenderCache.
    pub speculative_for: Option<String>,
}

/// Result from a background cross-section cut.
///
/// Carries the [`SectionTarget`](rustdar_egui::pane::SectionTarget) rather than a
/// pane index: a section takes far longer to produce than the user takes to draw
/// another line over it.
pub struct SectionResponse {
    pub pane_idx: usize,
    pub generation: u64,
    pub target: rustdar_egui::pane::SectionTarget,
    /// `None` where the cut answered nothing. Sent either way, for the reason
    /// [`RenderResponse::rendered`] is: this message clears `render_in_flight`.
    pub section: Option<Box<rustdar_radar::xsect::CrossSection>>,
}

/// Result from a background voxel build. Carries the
/// [`VolumeTarget`](rustdar_egui::pane::VolumeTarget) and no pane index: the
/// store refcounts grids by target, and a stale target is dropped.
pub struct VoxelResponse {
    pub target: rustdar_egui::pane::VolumeTarget,
    pub grid: Option<Box<rustdar_radar::voxel::VolumeGrid>>,
}

/// Result from a Level III object fetch. Names the AWIPS code and no product:
/// one poll fetches each code once and every product that reads it is served
/// from the same object — [`RadarProduct::level3_readers`].
pub struct Level3Response {
    pub generation: u64,
    /// AWIPS product ID this object is, e.g. `"EET"` — the cache key alongside site.
    pub code: String,
    pub site: String,
    /// The decoded product and the stamp of the object it came from — what lets
    /// the UI tell this scan's product from a previous-day fallback, up to ~48 h old.
    pub result: Result<Level3Product, String>,
}

pub struct OverlayRenderResponse {
    /// Already in egui's pixel layout, and already an `Arc`, for the reason
    /// [`RenderedImage::image`] gives — and at the larger of the two sizes: an
    /// overlay texture is planned against the viewport plus overdraw, so it scales
    /// with the window. At `OVERDRAW_FRACTION` 1.0 a 1920×1080 pane planned
    /// 5760×3240, 71.2 MiB of `Color32`, whose unmultiply measured 45.5 ms best /
    /// 47.6 ms median on a 7950X — two fifths of it first-touch paging, since both
    /// buffers are above glibc's `DEFAULT_MMAP_THRESHOLD_MAX` and so are their own
    /// `mmap`/`munmap`.
    ///
    /// `None` is a render that failed, and it must still be sent: this message is
    /// the only thing that clears the named panes' `render_in_flight` marks.
    pub image: Option<Arc<egui::ColorImage>>,
    pub geo_bounds: GeoBounds,
    /// Which layer this raster is for, carried back to find each pane's cache.
    pub overlay_kind: LayerId,
    pub generation: u64,
    pub pane_indices: Vec<usize>,
    pub zoom: i32,
    pub hit_map: Option<HitMap>,
    /// **Which loop frame asked for this raster**, or `None` for the pane's
    /// live picture.
    ///
    /// Echoed straight back off `RasterizeContext::frame`, because a loop
    /// dispatches several rasters of one layer at once and every other field
    /// here is identical across them: the layer, the pane, the geometry and
    /// the zoom are all the same, and without this there is nothing on the
    /// message that says which of the frames it belongs to.
    ///
    /// Filed by **stamp** and never by index — see
    /// [`LayerTimeState::frame_at_stamp_mut`]. A render is in the air for
    /// frames at a time and the list can be re-sampled underneath it.
    ///
    /// [`LayerTimeState::frame_at_stamp_mut`]: rustdar_egui::pane::LayerTimeState::frame_at_stamp_mut
    pub frame: Option<rustdar_source::time::FrameStamp>,
}

pub struct LoopScanDownloadResponse {
    /// NEXRAD site this scan was downloaded from. Half of the cache key, and the
    /// site of the listing the identifier came from, not the pane's current site.
    pub site: String,
    pub timestamp: NaiveDateTime,
    /// The decoded volume and its declared Nyquist velocities, or `None` if the
    /// download failed — the pair for the reason [`ScanData::declared_nyquist`] gives.
    pub scan: Option<(Arc<Scan>, Arc<rustdar_radar::nyquist::DeclaredNyquist>)>,
}

/// The Level III bucket keys a loop's pairings will be ranked against: one
/// listing per `(site, AWIPS code)` covering the UTC days its window touches.
/// Carries both because a listing is an uncancellable round-trip and the pane's
/// loop can be rebuilt while it is in the air.
pub struct LoopL3ListResponse {
    pub pane_idx: usize,
    pub site: String,
    pub code: String,
    /// Every key across the listed days, unordered; ranking per frame is
    /// [`rustdar_radar::level3::candidates_near`]'s job. An empty list is cached
    /// as a real answer, so the loop retires rather than waiting.
    pub keys: Vec<String>,
}

/// The Level III object paired to one loop frame's volume. `product` is `None`
/// when the site generated no object for that volume: an ordinary gap, cached.
pub struct LoopL3FetchResponse {
    pub pane_idx: usize,
    /// Site the object was paired against, carried from the pairing rather than
    /// re-read from the pane on arrival.
    pub site: String,
    pub code: String,
    /// The frame's volume start — what the pairing matched the object's PDB
    /// against, not the object's own key timestamp.
    pub timestamp: chrono::NaiveDateTime,
    pub product: Option<Arc<Level3Product>>,
}

pub struct LoopRenderResponse {
    pub pane_idx: usize,
    pub timestamp: NaiveDateTime,
    /// The render target this render was dispatched for — the pane's selected
    /// product and elevation, not the snapped angle.
    pub target: RenderTarget,
    /// The sweep angle the image actually depicts: `target.elevation` snapped to a
    /// sweep this frame's own scan carries. A pane taking this image via the
    /// sibling broadcast must check it. Set on the failure path too.
    pub snapped: f32,
    /// The site coordinates the image was projected around, straight off
    /// `LoopRenderRequest::render_params`. The image travels with them because
    /// sibling panes take it via the broadcast.
    pub site_lat: f64,
    /// See [`Self::site_lat`].
    pub site_lon: f64,
    /// The finished image, already in egui's pixel layout, or `None` when the scan
    /// carried no matching sweep.
    ///
    /// Converted before the send so the RGBA buffer and its `Color32` copy — 16
    /// MiB apiece at 2048² — never coexist in the channel. The receiver `take`s
    /// it, so the rest of the response stays borrowable for `broadcast_sweep`.
    pub image: Option<egui::ColorImage>,
    /// The half-width this frame was projected at, km. Per frame, not per loop:
    /// each sweep reaches as far as it reaches.
    pub max_range_km: f64,
    /// Where this frame's cut declared its velocity folds, m/s. Per frame: the
    /// RDA reselects PRFs between volumes.
    pub nyquist_ms: Option<f64>,
    /// Where this frame's melting layer came from, or `None` for a frame that
    /// classified nothing. Per frame: a loop pairs one N0M object per volume.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where this frame's storm motion vector came from, or `None` for a frame
    /// that shifted nothing. Per frame: a loop pairs one `N0S` per volume.
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
    /// Where this frame's gates are, with no numbers behind them: 5.8 KiB of
    /// wedges and a gate spacing against 5.03 MiB of values — see
    /// [`rustdar_radar::hover::SweepGates`]. Set on the failure path too, empty.
    pub polar: rustdar_radar::render::polar::PolarField,
}

/// Result from cutting a single cross-section loop frame.
///
/// A separate type from [`LoopRenderResponse`] because the two identify their
/// pictures differently: a plan-view frame by the sweep its scan snapped to, a
/// section frame by the line, the storm motion vector and the tilt ladder.
pub struct LoopSectionResponse {
    pub pane_idx: usize,
    pub timestamp: NaiveDateTime,
    /// The site/product half of the key this cut was dispatched for.
    pub target: RenderTarget,
    /// The line/storm-motion half. Both halves must match.
    pub key: rustdar_egui::pane::SectionLoopKey,
    /// The fingerprint of the tilt ladder this raster was cut from, from
    /// `rustdar_radar::sampler::ladder_fingerprint`. Set on the failure path too.
    pub ladder: u64,
    /// The finished raster, already in egui's pixel layout, or `None` when this
    /// volume carried nothing to cut.
    pub image: Option<egui::ColorImage>,
    /// The height and distance scales the raster was cut against, and where the
    /// ladder's rungs are. `None` exactly when [`Self::image`] is.
    pub axes: Option<rustdar_radar::xsect::SectionAxes>,
    pub tilt_elevations_deg: Vec<f64>,
    /// When each of those rungs was flown, ms since the Unix epoch, in the same
    /// order — `rustdar_radar::xsect::CrossSection::tilt_collected_ms`.
    pub tilt_collected_ms: Vec<i64>,
}

/// One round of a site's real-time chunk feed.
///
/// The poller travels back on this channel rather than being borrowed across the
/// await: it owns the assembled volume, and the fetch is on a detached task.
pub struct ChunkResponse {
    /// The site's fetch generation at dispatch — inherited from the Level II
    /// fetch, never bumped, so a tick cannot supersede a manual navigation.
    pub generation: u64,
    pub site: String,
    pub poller: Box<rustdar_radar::chunks::ChunkPoller>,
    pub result: Result<rustdar_radar::chunks::PollOutcome, String>,
}

/// The environmental 0 °C / −20 °C heights over a site, from Open-Meteo —
/// fetched when a scan loads, TTL-gated by
/// [`rustdar_radar::sounding::ENV_HEIGHTS_TTL`].
pub struct SoundingResponse {
    pub generation: u64,
    pub site: String,
    /// `None` when the fetch or parse failed; the receiver keeps what it holds.
    pub heights: Option<rustdar_radar::sounding::EnvHeights>,
}

/// The RPG's own Melting Layer object (Level III 166, AWIPS `N0M`) for one
/// site's currently-loaded volume.
///
/// A melting-layer object is a fact about one volume, so the volume it names
/// travels with it and the accessor refuses to apply it to any other.
pub struct MeltingLayerResponse {
    pub generation: u64,
    pub site: String,
    /// The Level II volume start the object was paired against, and the only
    /// volume it may be applied to.
    pub volume_start: NaiveDateTime,
    /// The object's bytes, or `None` when the site generated no `N0M` for that
    /// volume — an ordinary gap; `rustdar_radar::hca::resolve_melting_layer` falls
    /// to its next rung.
    pub object: Option<Arc<Vec<u8>>>,
}

/// One site's RPG storm motion vector for one volume, already decoded.
///
/// [`MeltingLayerResponse`]'s sibling, with the same volume discipline. Carries
/// no bytes: an `N0S` yields two scalars out of a PDB the pairing step already
/// decoded.
pub struct StormMotionResponse {
    pub generation: u64,
    pub site: String,
    /// The Level II volume start this vector may be applied to.
    pub volume_start: NaiveDateTime,
    /// `(speed_kt, direction_from_deg)` as the PDB stated them, or `None` when the
    /// site generated no `N0S` or its PDB carried no vector. `Some((0.0, 0.0))` is
    /// a reading, not a gap: SCIT tracked no cells and the RPG painted unshifted.
    pub motion: Option<(f32, f32)>,
}

/// The network site catalogue, once the launch's one refresh has landed.
///
/// Written to the cache, not applied — see [`crate::site_catalogue`].
pub struct SiteCatalogueResponse {
    /// `None` when the fetch failed. The receiver keeps the cache it already had.
    pub catalogue: Option<rustdar_radar::catalogue::SiteCatalogue>,
}

/// Channel hub for async communication between the App and background tasks.
///
/// # The eventing posture (written and verified at WO-M13b)
///
/// **A source has ONE arrival path, and it is not a channel of its own.**
/// Everything a source produces comes back on `overlay_fetch_*` as a
/// [`SourceEvent`]: one **kind-agnostic** channel carrying `Data`, `Frames`
/// and `FrameReady` for every layer, each arrival naming its `LayerId` in the
/// payload rather than in the channel it came down. It is drained by the one
/// `FRAME_PUMP` row `poll_overlay_fetch_results`, **once per frame**. The two
/// halves of that sentence are held differently, and which is which is worth
/// knowing: *one row* is pinned by
/// `every_hub_receiver_is_drained_by_exactly_one_row`; *once per frame* is
/// measured — `poll_data_channels` runs the pump's `Ingest` phase, and
/// `handle_redraw` is its only production caller (WO-M13b) — and only partly
/// pinned, by `the_chunk_drain_runs_before_the_frame_is_laid_out`, which holds
/// that `poll_data_channels` runs `Ingest` and where in the frame it sits, not
/// that it is called exactly once. Since WO-M13a a `Data` arrival also
/// carries the **raster-now obligation**: the same pass that installs the data
/// re-asks the panes already showing that layer, so an overlay raster is
/// redrawn when its data arrives rather than when the next draw loop notices
/// it went stale.
///
/// **A new source NEVER adds a channel — it registers a handler.** Its
/// arrivals ride `overlay_fetch_*`; its rasters ride `overlay_render_*`, which
/// is keyed by `LayerId` and carries the finished raster for the seven layers
/// the job funnel builds; its 3D builds ride `voxel_*`, whose job the layer
/// shapes for itself through `VolumeCapable` (WO-M14b-2). Those three pairs are
/// seams rather than plumbing, and the count is the evidence: the hub has gone
/// **18 pairs at WO-E3 → 17 since WO-M12b** and has never once gone up — the
/// three whole sources added since (SPC Fire Weather, MRMS, GMGSI) needed no
/// row here.
/// `the_channel_hub_only_ever_shrinks` refuses any pair whose base name is not
/// already on its pinned list, which is what makes this paragraph a test and
/// not an intention.
///
/// **The other fourteen pairs are radar's own stage-plumbing, and they only
/// ever shrink.** Ten are ingest stages (`scan`, `chunk`, `level3`,
/// `sounding`, `melting_layer`, `storm_motion`, `site_catalogue`,
/// `loop_scan_download`, `loop_l3_list`, `loop_l3_fetch`); four are raster
/// replies radar does **not** send through `overlay_render_*` (`render`,
/// `section`, `loop_render`, `loop_section`) — so "radar's bespoke half" is its
/// render path as well as its ingest, and saying only "ingest" understates it.
/// They exist because amendment M-H scopes `RadarSource` out of
/// `create_fetch_tasks`: the unified fetch seam — a client, the sources and one
/// optional viewport — cannot express radar's per-pane multi-stage ingest, so
/// that stays bespoke behind `RadarSource`. **The post-campaign per-pane fetch
/// seam and the LiveFeeds/chunk-transport fold are what shrink these**, not a
/// tidying pass here: retiring a pair is legal and is the only legal
/// direction.
///
/// **Per-type channels were KEPT deliberately; one channel is not the tidier
/// answer.** A `Receiver` is FIFO only within itself, so what orders two
/// arrivals of *different* types is the pump's row order — pinned by
/// `the_pump_rows_are_in_the_pinned_order` — and, within a row owning several
/// receivers, the order that row drains them in (`poll_level3_results` owns
/// four). Collapsing the fourteen into one channel would replace that with
/// whatever order the background tasks happened to finish in, so a Level III
/// object could be applied before the volume it was paired against. The
/// ordering guarantee lives in the row table, which is why the row table is
/// what is pinned.
///
/// **The generic streaming-push verb stays DEFERRED.** Restated verbatim from
/// the plan's post-campaign register: *"Generic streaming-push verb (deferred
/// until a second real stream exists)."* Radar's chunk feed is the one real
/// stream in the tree; a verb generalised from a single implementor is a
/// guess, and this fence stands until a second one exists.
pub struct ChannelHub {
    pub scan_sender: Sender<ScanResponse>,
    pub scan_receiver: Receiver<ScanResponse>,
    pub render_sender: Sender<RenderResponse>,
    pub render_receiver: Receiver<RenderResponse>,
    pub section_sender: Sender<SectionResponse>,
    pub section_receiver: Receiver<SectionResponse>,
    pub voxel_sender: Sender<VoxelResponse>,
    pub voxel_receiver: Receiver<VoxelResponse>,
    pub level3_sender: Sender<Level3Response>,
    pub level3_receiver: Receiver<Level3Response>,
    /// **The one arrival path a source has** — see the posture on
    /// [`ChannelHub`]. Widened at WO-M11 from `OverlayFetchResult` to
    /// [`SourceEvent`]: one channel and one drain, so a new arrival shape is a
    /// compile error at the `match` rather than a second channel nobody polls.
    ///
    /// All three arms have producers as of WO-M12b — `Frames` from a
    /// handler's frame-list task and `FrameReady` from its frame fetch — so the
    /// "dark until WO-E7/WO-M12" caveat this doc used to carry is retired
    /// rather than repeated.
    pub overlay_fetch_sender: Sender<SourceEvent>,
    pub overlay_fetch_receiver: Receiver<SourceEvent>,
    pub overlay_render_sender: Sender<OverlayRenderResponse>,
    pub overlay_render_receiver: Receiver<OverlayRenderResponse>,
    pub loop_scan_download_sender: Sender<LoopScanDownloadResponse>,
    pub loop_scan_download_receiver: Receiver<LoopScanDownloadResponse>,
    pub loop_l3_list_sender: Sender<LoopL3ListResponse>,
    pub loop_l3_list_receiver: Receiver<LoopL3ListResponse>,
    pub loop_l3_fetch_sender: Sender<LoopL3FetchResponse>,
    pub loop_l3_fetch_receiver: Receiver<LoopL3FetchResponse>,
    pub loop_render_sender: Sender<LoopRenderResponse>,
    pub loop_render_receiver: Receiver<LoopRenderResponse>,
    pub loop_section_sender: Sender<LoopSectionResponse>,
    pub loop_section_receiver: Receiver<LoopSectionResponse>,
    pub chunk_sender: Sender<ChunkResponse>,
    pub chunk_receiver: Receiver<ChunkResponse>,
    pub sounding_sender: Sender<SoundingResponse>,
    pub sounding_receiver: Receiver<SoundingResponse>,
    pub melting_layer_sender: Sender<MeltingLayerResponse>,
    pub melting_layer_receiver: Receiver<MeltingLayerResponse>,
    pub storm_motion_sender: Sender<StormMotionResponse>,
    pub storm_motion_receiver: Receiver<StormMotionResponse>,
    pub site_catalogue_sender: Sender<SiteCatalogueResponse>,
    pub site_catalogue_receiver: Receiver<SiteCatalogueResponse>,
}

impl Default for ChannelHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelHub {
    pub fn new() -> Self {
        let (scan_sender, scan_receiver) = std::sync::mpsc::channel();
        let (render_sender, render_receiver) = std::sync::mpsc::channel();
        let (section_sender, section_receiver) = std::sync::mpsc::channel();
        let (voxel_sender, voxel_receiver) = std::sync::mpsc::channel();
        let (level3_sender, level3_receiver) = std::sync::mpsc::channel();
        let (overlay_fetch_sender, overlay_fetch_receiver) = std::sync::mpsc::channel();
        let (overlay_render_sender, overlay_render_receiver) = std::sync::mpsc::channel();
        let (loop_scan_download_sender, loop_scan_download_receiver) = std::sync::mpsc::channel();
        let (loop_l3_list_sender, loop_l3_list_receiver) = std::sync::mpsc::channel();
        let (loop_l3_fetch_sender, loop_l3_fetch_receiver) = std::sync::mpsc::channel();
        let (loop_render_sender, loop_render_receiver) = std::sync::mpsc::channel();
        let (loop_section_sender, loop_section_receiver) = std::sync::mpsc::channel();
        let (sounding_sender, sounding_receiver) = std::sync::mpsc::channel();
        let (melting_layer_sender, melting_layer_receiver) = std::sync::mpsc::channel();
        let (storm_motion_sender, storm_motion_receiver) = std::sync::mpsc::channel();
        let (chunk_sender, chunk_receiver) = std::sync::mpsc::channel();
        let (site_catalogue_sender, site_catalogue_receiver) = std::sync::mpsc::channel();

        Self {
            scan_sender,
            scan_receiver,
            render_sender,
            render_receiver,
            section_sender,
            section_receiver,
            voxel_sender,
            voxel_receiver,
            level3_sender,
            level3_receiver,
            overlay_fetch_sender,
            overlay_fetch_receiver,
            overlay_render_sender,
            overlay_render_receiver,
            loop_scan_download_sender,
            loop_scan_download_receiver,
            loop_l3_list_sender,
            loop_l3_list_receiver,
            loop_l3_fetch_sender,
            loop_l3_fetch_receiver,
            loop_render_sender,
            loop_render_receiver,
            loop_section_sender,
            loop_section_receiver,
            chunk_sender,
            chunk_receiver,
            sounding_sender,
            sounding_receiver,
            melting_layer_sender,
            melting_layer_receiver,
            storm_motion_sender,
            storm_motion_receiver,
            site_catalogue_sender,
            site_catalogue_receiver,
        }
    }
}
