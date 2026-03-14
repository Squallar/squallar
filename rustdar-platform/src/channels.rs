use chrono::NaiveDateTime;
use nexrad_level3::model::Level3Message;
use nexrad_model::data::Scan;
use rustdar_overlays::nws::alert::NwsAlert;
use rustdar_overlays::spc::discussion::SpcDiscussion;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use rustdar_radar::types::RadarProduct;
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
    pub result: Result<ScanData, String>,
}

/// Result from a background radar render thread.
pub struct RenderResponse {
    pub image_data: Vec<u8>,
    pub max_range_km: f64,
    pub value_data: Vec<f32>,
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
}

impl ChannelHub {
    pub fn new() -> Self {
        let (scan_sender, scan_receiver) = std::sync::mpsc::channel();
        let (render_sender, render_receiver) = std::sync::mpsc::channel();
        let (level3_sender, level3_receiver) = std::sync::mpsc::channel();
        let (outlook_sender, outlook_receiver) = std::sync::mpsc::channel();
        let (alert_sender, alert_receiver) = std::sync::mpsc::channel();
        let (discussion_sender, discussion_receiver) = std::sync::mpsc::channel();

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
        }
    }
}
