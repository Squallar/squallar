use std::collections::{HashMap, HashSet};
use crate::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use crate::spc::discussion::SpcDiscussion;
use crate::nws::alert::NwsAlert;

/// Generic wrapper for overlay data that follows the fetch-cache-generation pattern.
///
/// Each overlay type (SPC outlooks, NWS alerts, SPC discussions) has the same
/// lifecycle: data is fetched asynchronously, cached locally, and invalidated
/// via a generation counter when new data arrives.  This struct captures that
/// shared pattern, reducing scattered fields on `Gui`.
pub struct OverlayState<T> {
    pub data: T,
    pub fetch_time: Option<std::time::Instant>,
    pub fetching: bool,
    pub data_generation: u64,
}

impl<T: Default> Default for OverlayState<T> {
    fn default() -> Self {
        Self {
            data: T::default(),
            fetch_time: None,
            fetching: false,
            data_generation: 0,
        }
    }
}

impl<T: Default> OverlayState<T> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T> OverlayState<T> {
    /// Replace the data and bump the generation counter.
    pub fn set_data(&mut self, data: T) {
        self.data = data;
        self.fetch_time = Some(std::time::Instant::now());
        self.data_generation = self.data_generation.wrapping_add(1);
    }

    /// Whether a refresh is due (no data yet, or `interval` has elapsed since last fetch).
    pub fn needs_refresh(&self, interval_secs: u64) -> bool {
        self.fetch_time
            .map_or(true, |t| t.elapsed().as_secs() >= interval_secs)
    }
}

/// All shared overlay state: SPC outlooks, NWS alerts, SPC discussions,
/// and their selection/hidden state.
pub struct OverlayData {
    pub spc_outlooks: OverlayState<HashMap<(OutlookDay, OutlookProduct), SpcOutlook>>,
    /// Per-product SPC data generation (keyed separately for cache invalidation).
    pub spc_data_generation: HashMap<(OutlookDay, OutlookProduct), u64>,
    pub nws_alerts: OverlayState<Vec<NwsAlert>>,
    /// Index of the currently selected alert for detail popup.
    pub selected_alert: Option<usize>,
    /// Alert IDs hidden by the user (not rendered on the map).
    pub hidden_alerts: HashSet<String>,
    pub spc_discussions: OverlayState<Vec<SpcDiscussion>>,
    /// Index of the currently selected MD for detail popup.
    pub selected_md: Option<usize>,
}

impl Default for OverlayData {
    fn default() -> Self {
        Self {
            spc_outlooks: OverlayState::new(),
            spc_data_generation: HashMap::new(),
            nws_alerts: OverlayState::new(),
            selected_alert: None,
            hidden_alerts: HashSet::new(),
            spc_discussions: OverlayState::new(),
            selected_md: None,
        }
    }
}

impl OverlayData {
    /// Store a fetched SPC outlook in the cache.
    pub fn set_spc_outlook(&mut self, day: OutlookDay, product: OutlookProduct, outlook: SpcOutlook) {
        self.spc_outlooks.data.insert((day, product), outlook);
        self.spc_outlooks.fetch_time = Some(std::time::Instant::now());
        let generation = self.spc_data_generation.entry((day, product)).or_insert(0);
        *generation = generation.wrapping_add(1);
    }

    /// Set whether an SPC fetch is currently in progress.
    pub fn set_spc_fetching(&mut self, fetching: bool) {
        self.spc_outlooks.fetching = fetching;
    }

    /// Store fetched NWS alerts, replacing the previous set.
    pub fn set_nws_alerts(&mut self, alerts: Vec<NwsAlert>) {
        let current_ids: HashSet<String> = alerts.iter().map(|a| a.id.clone()).collect();
        self.hidden_alerts.retain(|id| current_ids.contains(id));
        self.nws_alerts.set_data(alerts);
    }

    /// Set whether an NWS alerts fetch is currently in progress.
    pub fn set_nws_fetching(&mut self, fetching: bool) {
        self.nws_alerts.fetching = fetching;
    }

    /// Store fetched SPC Mesoscale Discussions, replacing the previous set.
    pub fn set_spc_discussions(&mut self, discussions: Vec<SpcDiscussion>) {
        self.spc_discussions.set_data(discussions);
    }

    /// Set whether an SPC MD fetch is currently in progress.
    pub fn set_spc_md_fetching(&mut self, fetching: bool) {
        self.spc_discussions.fetching = fetching;
    }

    /// Combined SPC data generation — sum of all per-product generations.
    /// Used as a single change-detection value for the unified SPC texture.
    pub fn combined_spc_data_generation(&self) -> u64 {
        self.spc_data_generation.values().sum()
    }
}
