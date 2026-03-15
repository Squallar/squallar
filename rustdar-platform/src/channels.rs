use chrono::NaiveDateTime;
use nexrad_data::aws::archive::Identifier;
use nexrad_level3::model::Level3Message;
use nexrad_model::data::Scan;
use rustdar_overlays::nws::alert::NwsAlert;
use rustdar_overlays::spc::discussion::SpcDiscussion;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
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
    pub result: Result<Level3Message, String>,
}

/// Result from a background SPC outlook fetch.
pub struct OutlookResponse {
    pub day: OutlookDay,
    pub product: OutlookProduct,
    pub result: Result<SpcOutlook, String>,
}

/// Result from a background NWS alerts fetch.
pub type AlertResult = Result<Vec<NwsAlert>, String>;

/// Result from a background SPC Mesoscale Discussion fetch.
pub type DiscussionResult = Result<Vec<SpcDiscussion>, String>;

/// Which overlay type an overlay render result belongs to.
#[derive(Debug, Clone)]
pub enum OverlayType {
    SpcOutlook(OutlookDay, OutlookProduct),
    SpcDiscussions,
    NwsAlerts,
}

/// Result from a background overlay rasterization thread.
pub struct OverlayRenderResponse {
    pub image_data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub geo_bounds: GeoBounds,
    pub overlay_type: OverlayType,
    pub generation: u64,
    pub pane_indices: Vec<usize>,
    pub zoom: i32,
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
    /// UTC timestamp of the downloaded scan.
    pub timestamp: NaiveDateTime,
    /// The decoded scan data, or `None` if the download failed.
    pub scan: Option<Arc<Scan>>,
}

/// Result from rendering a single loop frame.
pub struct LoopRenderResponse {
    pub pane_idx: usize,
    pub timestamp: NaiveDateTime,
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
    pub outlook_sender: Sender<OutlookResponse>,
    pub outlook_receiver: Receiver<OutlookResponse>,
    pub alert_sender: Sender<AlertResult>,
    pub alert_receiver: Receiver<AlertResult>,
    pub discussion_sender: Sender<DiscussionResult>,
    pub discussion_receiver: Receiver<DiscussionResult>,
    pub overlay_render_sender: Sender<OverlayRenderResponse>,
    pub overlay_render_receiver: Receiver<OverlayRenderResponse>,
    pub loop_scan_list_sender: Sender<LoopScanListResponse>,
    pub loop_scan_list_receiver: Receiver<LoopScanListResponse>,
    pub loop_scan_download_sender: Sender<LoopScanDownloadResponse>,
    pub loop_scan_download_receiver: Receiver<LoopScanDownloadResponse>,
    pub loop_render_sender: Sender<LoopRenderResponse>,
    pub loop_render_receiver: Receiver<LoopRenderResponse>,
}

impl ChannelHub {
    pub fn new() -> Self {
        let (scan_sender, scan_receiver) = std::sync::mpsc::channel();
        let (render_sender, render_receiver) = std::sync::mpsc::channel();
        let (level3_sender, level3_receiver) = std::sync::mpsc::channel();
        let (outlook_sender, outlook_receiver) = std::sync::mpsc::channel();
        let (alert_sender, alert_receiver) = std::sync::mpsc::channel();
        let (discussion_sender, discussion_receiver) = std::sync::mpsc::channel();
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
            outlook_sender,
            outlook_receiver,
            alert_sender,
            alert_receiver,
            discussion_sender,
            discussion_receiver,
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
