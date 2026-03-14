use chrono::NaiveDateTime;
use nexrad_level3::model::Level3Message;
use nexrad_model::data::Scan;
use rustdar_overlays::nws::alert::NwsAlert;
use rustdar_overlays::spc::discussion::SpcDiscussion;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use rustdar_radar::types::RadarProduct;
use std::sync::mpsc::{Receiver, Sender};

pub type ScanResult = (u64, Result<(Scan, String, NaiveDateTime), String>);

/// Result from background radar rendering: (image_data, max_range_km, value_data, product, elevation, generation, pane_idx)
pub type RenderResult = (Vec<u8>, f64, Vec<f32>, RadarProduct, f32, u64, usize);

/// Result from a Level III product fetch: (generation, product, tilt_code, result)
pub type Level3Result = (u64, RadarProduct, String, Result<Level3Message, String>);

/// Result from a background SPC outlook fetch
pub type OutlookResult = (OutlookDay, OutlookProduct, Result<SpcOutlook, String>);

/// Result from a background NWS alerts fetch
pub type AlertResult = Result<Vec<NwsAlert>, String>;

/// Result from a background SPC Mesoscale Discussion fetch
pub type DiscussionResult = Result<Vec<SpcDiscussion>, String>;

/// Centralized channel hub for all async communication between the App and
/// background tasks (network fetches, radar rendering, etc.).
pub struct ChannelHub {
    pub scan_sender: Sender<ScanResult>,
    pub scan_receiver: Receiver<ScanResult>,
    pub render_sender: Sender<RenderResult>,
    pub render_receiver: Receiver<RenderResult>,
    pub level3_sender: Sender<Level3Result>,
    pub level3_receiver: Receiver<Level3Result>,
    pub outlook_sender: Sender<OutlookResult>,
    pub outlook_receiver: Receiver<OutlookResult>,
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
