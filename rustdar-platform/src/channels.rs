use chrono::NaiveDateTime;
use nexrad_data::aws::archive::Identifier;
use nexrad_level3::model::Level3Message;
use nexrad_model::data::Scan;
use rustdar_egui::pane::RenderTarget;
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayKind};
use rustdar_overlays::render::rasterize::HitMap;
use rustdar_overlays::types::GeoBounds;
use rustdar_radar::types::RadarProduct;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

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
    pub result: Result<Level3Message, String>,
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
    /// Timestamps and identifiers for scans in the requested range (oldest-first).
    pub scans: Vec<(NaiveDateTime, Identifier)>,
}

/// Result from downloading a single scan for a loop frame.
pub struct LoopScanDownloadResponse {
    pub pane_idx: usize,
    /// NEXRAD site this scan was downloaded for, captured when the download was
    /// spawned. Half of the cache key, and carried on the response rather than
    /// re-read from the pane: the pane's loop can be rebuilt for another site
    /// while the download runs, and the scan still belongs to the site it came
    /// from.
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
    pub snapped: f32,
    pub image_data: Vec<u8>,
    pub max_range_km: f64,
    pub value_data: Vec<f32>,
}

/// Centralized channel hub for all async communication between the App and
/// background tasks (network fetches, radar rendering, etc.).
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
        }
    }
}
