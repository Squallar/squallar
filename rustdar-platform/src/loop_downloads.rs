use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// Manages loop radar download state: scan cache, in-flight tracking,
/// and per-pane pending download queues. Grouping these together prevents
/// partial updates that could leave the fields in an inconsistent state.
pub struct LoopDownloadManager {
    /// Downloaded scan data cache for loop frames, keyed by timestamp (shared across panes).
    scan_cache: HashMap<chrono::NaiveDateTime, Arc<nexrad_model::data::Scan>>,
    /// Timestamps currently being downloaded (to avoid duplicate downloads across panes).
    in_flight_set: HashSet<chrono::NaiveDateTime>,
    /// Pending loop scan downloads per pane, waiting to be dispatched (throttled).
    pending_downloads: HashMap<usize, VecDeque<(chrono::NaiveDateTime, nexrad_data::aws::archive::Identifier)>>,
    /// Number of loop scan downloads currently in flight (global, not per-pane).
    in_flight_count: usize,
}

impl Default for LoopDownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopDownloadManager {
    pub fn new() -> Self {
        Self {
            scan_cache: HashMap::new(),
            in_flight_set: HashSet::new(),
            pending_downloads: HashMap::new(),
            in_flight_count: 0,
        }
    }

    /// Number of download slots remaining before hitting the concurrency cap.
    pub fn available_slots(&self, max_concurrent: usize) -> usize {
        max_concurrent.saturating_sub(self.in_flight_count)
    }

    /// Whether a scan for the given timestamp is already cached.
    pub fn is_cached(&self, ts: &chrono::NaiveDateTime) -> bool {
        self.scan_cache.contains_key(ts)
    }

    /// Whether a download for the given timestamp is currently in flight.
    pub fn is_in_flight(&self, ts: &chrono::NaiveDateTime) -> bool {
        self.in_flight_set.contains(ts)
    }

    /// Get a cached scan by timestamp.
    pub fn get_cached(&self, ts: &chrono::NaiveDateTime) -> Option<&Arc<nexrad_model::data::Scan>> {
        self.scan_cache.get(ts)
    }

    /// Store a downloaded scan in the cache.
    pub fn cache_scan(&mut self, ts: chrono::NaiveDateTime, scan: Arc<nexrad_model::data::Scan>) {
        self.scan_cache.insert(ts, scan);
    }

    /// Mark a timestamp as currently being downloaded.
    pub fn mark_in_flight(&mut self, ts: chrono::NaiveDateTime) {
        self.in_flight_set.insert(ts);
    }

    /// Remove a timestamp from the in-flight set (download completed or failed).
    pub fn complete_download(&mut self, ts: &chrono::NaiveDateTime) {
        self.in_flight_set.remove(ts);
    }

    /// Decrement the in-flight counter by the number of completed downloads.
    pub fn complete_batch(&mut self, count: usize) {
        self.in_flight_count = self.in_flight_count.saturating_sub(count);
    }

    /// Increment the in-flight counter after spawning new downloads.
    pub fn add_spawned(&mut self, count: usize) {
        self.in_flight_count += count;
    }

    /// Set the pending download queue for a pane.
    pub fn insert_pending(
        &mut self,
        pane: usize,
        scans: VecDeque<(chrono::NaiveDateTime, nexrad_data::aws::archive::Identifier)>,
    ) {
        self.pending_downloads.insert(pane, scans);
    }

    /// Remove a pane's pending download queue.
    pub fn remove_pending(&mut self, pane: usize) {
        self.pending_downloads.remove(&pane);
    }

    /// Get mutable access to a pane's pending download queue.
    pub fn pending_mut(
        &mut self,
        pane: usize,
    ) -> Option<&mut VecDeque<(chrono::NaiveDateTime, nexrad_data::aws::archive::Identifier)>> {
        self.pending_downloads.get_mut(&pane)
    }

    /// Extract the pending queue completely. Call `insert_pending` to return it later.
    pub fn extract_pending(
        &mut self,
        pane: usize,
    ) -> Option<VecDeque<(chrono::NaiveDateTime, nexrad_data::aws::archive::Identifier)>> {
        self.pending_downloads.remove(&pane)
    }

    /// Collect all pane indices that have pending download entries.
    pub fn pending_pane_indices(&self) -> Vec<usize> {
        self.pending_downloads.keys().copied().collect()
    }

    /// Whether all pending downloads for a pane have been dispatched.
    pub fn is_pane_done(&self, pane: usize) -> bool {
        self.pending_downloads.get(&pane).is_none_or(|p| p.is_empty())
    }

    /// Reset all loop download state. Used on site switch to avoid stale data.
    pub fn clear_all(&mut self) {
        self.scan_cache.clear();
        self.in_flight_set.clear();
        self.pending_downloads.clear();
        self.in_flight_count = 0;
    }
}
