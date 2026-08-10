use chrono::NaiveDateTime;
use nexrad_model::data::Scan;
use rustdar_egui::pane::RenderTarget;
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayKind};
use rustdar_overlays::render::rasterize::HitMap;
use rustdar_overlays::types::GeoBounds;
use rustdar_radar::archive::Identifier;
use rustdar_radar::level3::Level3Product;
use rustdar_radar::types::RadarProduct;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

/// Successful scan data returned from a background fetch.
pub struct ScanData {
    pub scan: Scan,
    /// What the volume's cuts declared their Nyquist velocities to be.
    ///
    /// Carried beside the `Scan` rather than in it because the model type has
    /// no field for it — see [`rustdar_radar::nyquist`] — and dropping it here
    /// would leave the section worker estimating velocity fold limits that the
    /// archive stated outright, with no symptom to notice.
    pub declared_nyquist: rustdar_radar::nyquist::DeclaredNyquist,
    pub site: String,
    pub timestamp: NaiveDateTime,
}

/// Result from a background radar scan fetch, with generation tracking.
pub struct ScanResponse {
    pub generation: u64,
    /// Site this fetch was for (needed for per-site generation checking).
    pub site: String,
    pub result: Result<ScanData, String>,
    /// True when this result originated from an auto-poll check (not manual navigation).
    pub is_auto_poll: bool,
}

/// What a render produced: the RGBA texture, the range it was projected at, and
/// the per-pixel value grid a hover reads.
pub struct RenderedImage {
    pub image_data: Arc<Vec<u8>>,
    pub max_range_km: f64,
    pub value_data: Arc<Vec<f32>>,
}

/// Result from a background radar render thread.
pub struct RenderResponse {
    /// `None` where the renderer found nothing to draw.
    ///
    /// A render that answers nothing still has to report back. `pane_render`'s
    /// `render_in_flight` is cleared on receipt of this message and nowhere else
    /// outside `reset_panes*`, and `dispatch_pane_renders` refuses to dispatch
    /// while it is set — so a render that stayed silent would leave its pane
    /// unable to ask for another one until something reset it.
    ///
    /// The ordinary source of a `None` is `Job::renders_nothing`: a pane parked
    /// on a tilt the volume does not carry. Rare against an archive volume,
    /// which holds every cut it will ever have; routine against a volume still
    /// being assembled from the real-time chunk feed, where an upper tilt has
    /// simply not been scanned yet. That change in frequency is what makes the
    /// report mandatory rather than tidy.
    ///
    /// An *abandoned* render still sends nothing at all — the send is gated on
    /// `results_wanted`, so a superseded render cannot clear the flag belonging
    /// to the render that replaced it.
    pub rendered: Option<RenderedImage>,
    pub product: RadarProduct,
    pub elevation: f32,
    pub generation: u64,
    pub pane_idx: usize,
}

/// Result from a background cross-section cut.
///
/// Carries the [`SectionTarget`](rustdar_egui::pane::SectionTarget) it was cut
/// for rather than a bare pane index, and that is what matches a result to a
/// pane. A section takes an order of magnitude longer to produce than the user
/// takes to draw another line over it, so "the pane this belongs to" and "the
/// pane that is still waiting for this" are different questions — and answering
/// only the first would let a section of the previous line arrive after the
/// current one and sit there looking authoritative.
pub struct SectionResponse {
    pub pane_idx: usize,
    pub generation: u64,
    /// What was asked for: which volume, which moment, which line.
    pub target: rustdar_egui::pane::SectionTarget,
    /// `None` where the cut answered nothing.
    ///
    /// Sent either way, for the reason [`RenderResponse::rendered`] is: this
    /// message is what clears `render_in_flight`, and a pane that never hears
    /// back stops asking.
    pub section: Option<Box<rustdar_radar::xsect::CrossSection>>,
}

/// Result from a background voxel build.
///
/// Carries the [`VolumeTarget`](rustdar_egui::pane::VolumeTarget) it was built
/// for and no pane index at all: the store refcounts grids **by target**, so
/// the result belongs to every pane attached to that target's `Building`
/// entry, and `VolumeStore::complete` is what resolves them. A stale target —
/// superseded by a newer sealed sweep while the build was in flight — finds no
/// `Building` entry and is dropped, which is the dedupe working, not a leak.
pub struct VoxelResponse {
    /// What was asked for: which site, which stamp, which moment, which region.
    pub target: rustdar_egui::pane::VolumeTarget,
    /// `None` where the resample answered nothing.
    pub grid: Option<Box<rustdar_radar::voxel::VoxelGrid>>,
}

/// Result from a Level III object fetch.
///
/// Names the AWIPS **code** and no product. One poll fetches each code once and
/// every product that reads it is served from the same object, so a `product`
/// field here would be one of several right answers — and whichever it named
/// would be the only pane redrawn and the only picker entry filled in. The
/// readers are derived on arrival instead: [`RadarProduct::level3_readers`].
pub struct Level3Response {
    pub generation: u64,
    /// AWIPS product ID this object is, e.g. `"EET"` — the cache key alongside
    /// the site, and what the readers are looked up by.
    pub code: String,
    pub site: String,
    /// The decoded product *and* the stamp of the object it came from.
    ///
    /// Carrying the stamp is what lets the UI distinguish a product from this
    /// scan from one `level3::latest_key`'s previous-day fallback found — up to
    /// ~48 h old — the same way `HrrrGridData::ref_time` distinguishes a 0–1 h
    /// forecast from an analysis.
    pub result: Result<Level3Product, String>,
}

/// Result from a background overlay rasterization thread.
pub struct OverlayRenderResponse {
    pub image_data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub geo_bounds: GeoBounds,
    pub overlay_kind: OverlayKind,
    pub generation: u64,
    pub pane_indices: Vec<usize>,
    pub zoom: i32,
    pub hit_map: Option<HitMap>,
}

/// Result from listing available scans for a loop time range.
pub struct LoopScanListResponse {
    pub pane_idx: usize,
    /// NEXRAD site the listing was requested for. Every `Identifier` below is one
    /// of this site's files.
    ///
    /// A listing is a network round-trip that cannot be cancelled, and a pane's
    /// loop can be torn down and rebuilt for another site while it is in the air —
    /// by a site switch, or by any of the routine rebuilds (`reinit_active_loops`,
    /// the lookback slider). Without this the receiver could not tell a live
    /// listing from one belonging to a loop that no longer exists, and would take
    /// one site's file list as another site's frames.
    pub site: String,
    /// Timestamps and identifiers for scans in the requested range (oldest-first).
    pub scans: Vec<(NaiveDateTime, Identifier)>,
}

/// Result from downloading a single scan for a loop frame.
pub struct LoopScanDownloadResponse {
    pub pane_idx: usize,
    /// NEXRAD site this scan was downloaded from. Half of the cache key.
    ///
    /// It is the site of the *listing the identifier came from*, carried through
    /// `PendingDownloads` and echoed here — not the site the requesting pane's loop
    /// happened to be on when the download was dispatched, and not re-read from the
    /// pane on arrival. Both of those can have moved on: the pane's loop is rebuilt
    /// on a site switch, and identifiers outlive the loop that listed them.
    pub site: String,
    /// UTC timestamp of the downloaded scan.
    pub timestamp: NaiveDateTime,
    /// The decoded scan data, or `None` if the download failed.
    pub scan: Option<Arc<Scan>>,
}

/// The Level III bucket keys a loop's pairings will be ranked against: one
/// listing per `(site, AWIPS code)` covering the UTC days its window touches.
///
/// The Level III counterpart of [`LoopScanListResponse`], and it carries the site
/// and the code for the same reason that one carries the site: a listing is an
/// uncancellable round-trip, the pane's loop can be rebuilt for another site or
/// retargeted to another product while it is in the air, and the keys are useless
/// — worse, misleading — filed under anything but what they were listed for.
pub struct LoopL3ListResponse {
    pub pane_idx: usize,
    /// Site the listing was made for. Every key below is one of its objects.
    pub site: String,
    /// AWIPS product ID the listing was made for, e.g. `"EET"`.
    pub code: String,
    /// Every key across the listed days, unordered. Ranking per frame is
    /// [`rustdar_radar::level3::candidates_near`]'s job.
    ///
    /// An empty list is a real answer — the site served no objects for this
    /// product — and is cached as one, so every frame resolves to a gap and the
    /// loop retires rather than waiting on a listing that already happened.
    pub keys: Vec<String>,
}

/// The Level III object paired to one loop frame's volume.
///
/// `product` is `None` when the site generated no object for that volume: an
/// ordinary gap, not a failure. It is cached as the answer, so the frame is
/// retired once rather than re-paired on every dispatch pass.
pub struct LoopL3FetchResponse {
    pub pane_idx: usize,
    /// Site the object was paired against — half of the cache key, carried from
    /// the pairing rather than re-read from the pane on arrival, exactly as
    /// [`LoopScanDownloadResponse::site`] is.
    pub site: String,
    /// AWIPS product ID this object is, the second part of the cache key.
    pub code: String,
    /// The frame's **volume start** — what the pairing matched the object's PDB
    /// against, and the third part of the cache key. Not the object's own key
    /// timestamp, which is when the RPG published it.
    pub timestamp: chrono::NaiveDateTime,
    pub product: Option<Arc<Level3Product>>,
}

/// Result from rendering a single loop frame.
pub struct LoopRenderResponse {
    pub pane_idx: usize,
    pub timestamp: NaiveDateTime,
    /// The render target this render was dispatched for: the loop's site plus the
    /// pane's *selected* product and elevation — not the per-scan snapped angle the
    /// image was actually rendered at. Compared against
    /// `LoopPlaybackState::rendered_for` on arrival to reject results whose target the
    /// pane has since moved away from.
    pub target: RenderTarget,
    /// The sweep angle the image actually depicts: `target.elevation` snapped to a
    /// sweep this frame's own scan carries. Unlike the target, this is a property
    /// of the scan as well as the selection, so a pane taking this image via the
    /// sibling broadcast has to check it against what *its* scan resolves the same
    /// selection to — see `LoopPlaybackState::frame_accepting_broadcast`.
    ///
    /// Set unconditionally, on the failure path too: it describes the render that was
    /// *dispatched*, and there is only one send site to set it from.
    pub snapped: f32,
    /// The site coordinates the image was projected around — the ones the renderer
    /// was handed, straight off `LoopRenderRequest::render_params`.
    ///
    /// Carried rather than looked back up. The receiving loop's own
    /// `site_lat`/`site_lon` are the obvious substitute and are a reconstruction:
    /// they are only equal to these because a site change rebuilds the loop and
    /// clears `rendered_for`, so the target check rejects the result first. That
    /// coupling lives in another type and is invisible at the point of use, and it
    /// has to hold for sibling panes taking this image via the broadcast too. The
    /// image describes one pair of coordinates; it travels with them.
    pub site_lat: f64,
    /// See [`Self::site_lat`].
    pub site_lon: f64,
    /// The finished image, already in egui's pixel layout, or `None` when the scan
    /// carried no matching sweep and there is nothing to show.
    ///
    /// Deliberately not the renderer's `Vec<u8>`. Converting before the send means
    /// the RGBA buffer and its `Color32` copy — `IMAGE_SIZE² × 4` bytes each,
    /// 16 MiB apiece at 2048² — never coexist in the channel; the receiver holds
    /// exactly one buffer and moves it straight into `Context::load_texture`. The
    /// transient pair is bounded by `MAX_CONCURRENT_RENDERS`.
    ///
    /// Natively that conversion is on the render thread, off the frame-pacing
    /// path entirely. In the browser, where the rasterization runs in a Web
    /// Worker that cannot build an `egui::ColorImage` and could not post one if
    /// it did, it happens in `spawn_loop_frame_render`'s `deliver` — on the main
    /// thread, but as a reinterpretation of 4 MiB rather than a rasterization,
    /// and against a 1024² frame rather than 2048².
    ///
    /// `None` replaces the previous empty-`Vec` sentinel; the meaning is unchanged.
    /// The receiver `take`s it rather than moving it out, so the rest of the response
    /// stays borrowable for `broadcast_sweep`.
    pub image: Option<egui::ColorImage>,
    pub max_range_km: f64,
}

/// One round of a site's real-time chunk feed.
///
/// Deliberately **not** a variant of [`ScanResponse`]. That type's drain bakes in
/// five behaviours that all belong to a fetch someone is waiting on and are all
/// wrong every few seconds: it takes the global `fetching` spinner down, clears
/// the pane's `loading_site`, routes an error through `set_error` (which doubles
/// the *archive* poll's backoff), stashes into `latest_cached_scans` on the
/// historic branch, and compensates for a stale discard. A `is_chunk: bool`
/// beside `is_auto_poll` would put four states in one drain, three of them
/// unreachable.
///
/// The poller travels *back* on this channel rather than being borrowed across
/// the await: it owns the assembled volume, and the fetch happens on a detached
/// task that cannot hold a reference into `App`.
pub struct ChunkResponse {
    /// The site's fetch generation at dispatch — inherited from the Level II
    /// fetch, never bumped. Bumping would let a five-second tick supersede a
    /// manual navigation, and the scan drain's stale arm would then take that
    /// navigation's spinner down early.
    pub generation: u64,
    pub site: String,
    /// The poller, handed back so the next round resumes from it.
    pub poller: Box<rustdar_radar::chunks::ChunkPoller>,
    pub result: Result<rustdar_radar::chunks::PollOutcome, String>,
}

/// The environmental 0 °C / −20 °C heights over a site, from Open-Meteo —
/// fetched when a scan loads, but TTL-gated (see
/// [`rustdar_radar::sounding::ENV_HEIGHTS_TTL`]) rather than refetched every
/// poll. Staged for the hail products, which will read them off
/// `RenderDispatcher::env_heights`.
pub struct SoundingResponse {
    pub generation: u64,
    pub site: String,
    /// `None` when the fetch or parse failed. The receiver keeps whatever it
    /// already holds for the site — a stale environment beats none, and the
    /// TTL gate retries on the next poll.
    pub heights: Option<rustdar_radar::sounding::EnvHeights>,
}

/// Centralized channel hub for all async communication between the App and
/// background tasks (network fetches, radar rendering, etc.).
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
    pub overlay_fetch_sender: Sender<OverlayFetchResult>,
    pub overlay_fetch_receiver: Receiver<OverlayFetchResult>,
    pub overlay_render_sender: Sender<OverlayRenderResponse>,
    pub overlay_render_receiver: Receiver<OverlayRenderResponse>,
    pub loop_scan_list_sender: Sender<LoopScanListResponse>,
    pub loop_scan_list_receiver: Receiver<LoopScanListResponse>,
    pub loop_scan_download_sender: Sender<LoopScanDownloadResponse>,
    pub loop_scan_download_receiver: Receiver<LoopScanDownloadResponse>,
    pub loop_l3_list_sender: Sender<LoopL3ListResponse>,
    pub loop_l3_list_receiver: Receiver<LoopL3ListResponse>,
    pub loop_l3_fetch_sender: Sender<LoopL3FetchResponse>,
    pub loop_l3_fetch_receiver: Receiver<LoopL3FetchResponse>,
    pub loop_render_sender: Sender<LoopRenderResponse>,
    pub loop_render_receiver: Receiver<LoopRenderResponse>,
    pub chunk_sender: Sender<ChunkResponse>,
    pub chunk_receiver: Receiver<ChunkResponse>,
    pub sounding_sender: Sender<SoundingResponse>,
    pub sounding_receiver: Receiver<SoundingResponse>,
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
        let (loop_scan_list_sender, loop_scan_list_receiver) = std::sync::mpsc::channel();
        let (loop_scan_download_sender, loop_scan_download_receiver) = std::sync::mpsc::channel();
        let (loop_l3_list_sender, loop_l3_list_receiver) = std::sync::mpsc::channel();
        let (loop_l3_fetch_sender, loop_l3_fetch_receiver) = std::sync::mpsc::channel();
        let (loop_render_sender, loop_render_receiver) = std::sync::mpsc::channel();
        let (sounding_sender, sounding_receiver) = std::sync::mpsc::channel();
        let (chunk_sender, chunk_receiver) = std::sync::mpsc::channel();

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
            loop_scan_list_sender,
            loop_scan_list_receiver,
            loop_scan_download_sender,
            loop_scan_download_receiver,
            loop_l3_list_sender,
            loop_l3_list_receiver,
            loop_l3_fetch_sender,
            loop_l3_fetch_receiver,
            loop_render_sender,
            loop_render_receiver,
            chunk_sender,
            chunk_receiver,
            sounding_sender,
            sounding_receiver,
        }
    }
}
