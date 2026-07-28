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

/// Result from a background radar render thread.
pub struct RenderResponse {
    pub image_data: Arc<Vec<u8>>,
    pub max_range_km: f64,
    pub value_data: Arc<Vec<f32>>,
    pub product: RadarProduct,
    pub elevation: f32,
    pub generation: u64,
    pub pane_idx: usize,
}

/// Result from a Level III product fetch.
pub struct Level3Response {
    pub generation: u64,
    pub product: RadarProduct,
    pub tilt_code: String,
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
    /// Deliberately not the renderer's `Vec<u8>`. Converting on the render thread
    /// means the RGBA buffer and its `Color32` copy — `IMAGE_SIZE² × 4` bytes each,
    /// 16 MiB apiece at 2048² — never coexist on the main thread, which then holds
    /// exactly one buffer and moves it straight into `Context::load_texture`. The
    /// worker's own transient pair is bounded by `MAX_CONCURRENT_RENDERS` and is off
    /// the frame-pacing path.
    ///
    /// `None` replaces the previous empty-`Vec` sentinel; the meaning is unchanged.
    /// The receiver `take`s it rather than moving it out, so the rest of the response
    /// stays borrowable for `broadcast_sweep`.
    pub image: Option<egui::ColorImage>,
    pub max_range_km: f64,
}

/// Centralized channel hub for all async communication between the App and
/// background tasks (network fetches, radar rendering, etc.).
/// The latest VAD Wind Profile levels for a site — (height km, u, v) —
/// fetched alongside the Level III products; NROT renders pass them to
/// `render_radar_to_image_with_winds`.
pub struct VwpResponse {
    pub generation: u64,
    pub site: String,
    pub levels: Vec<(f64, f64, f64)>,
}

pub struct ChannelHub {
    pub scan_sender: Sender<ScanResponse>,
    pub scan_receiver: Receiver<ScanResponse>,
    pub render_sender: Sender<RenderResponse>,
    pub render_receiver: Receiver<RenderResponse>,
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
    pub loop_render_sender: Sender<LoopRenderResponse>,
    pub loop_render_receiver: Receiver<LoopRenderResponse>,
    pub vwp_sender: Sender<VwpResponse>,
    pub vwp_receiver: Receiver<VwpResponse>,
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
        let (level3_sender, level3_receiver) = std::sync::mpsc::channel();
        let (overlay_fetch_sender, overlay_fetch_receiver) = std::sync::mpsc::channel();
        let (overlay_render_sender, overlay_render_receiver) = std::sync::mpsc::channel();
        let (loop_scan_list_sender, loop_scan_list_receiver) = std::sync::mpsc::channel();
        let (loop_scan_download_sender, loop_scan_download_receiver) = std::sync::mpsc::channel();
        let (loop_render_sender, loop_render_receiver) = std::sync::mpsc::channel();
        let (vwp_sender, vwp_receiver) = std::sync::mpsc::channel();

        Self {
            scan_sender,
            scan_receiver,
            render_sender,
            render_receiver,
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
            loop_render_sender,
            loop_render_receiver,
            vwp_sender,
            vwp_receiver,
        }
    }
}
