use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// Manages loop radar download state: scan cache, in-flight tracking,
/// and per-pane pending download queues. Grouping these together prevents
/// partial updates that could leave the fields in an inconsistent state.
///
/// Scans and download marks are keyed by `(site, timestamp)`, never by timestamp
/// alone. Panes run independent loops on independent sites, and two sites' volume
/// times land on the same second often enough — a timestamp-only key let one site's
/// scan overwrite another's, and the loop that then looked it up rendered another
/// radar's data around its own coordinates. Nothing downstream can catch that: the
/// render target key is derived from the loop, so the result looks entirely
/// consistent. The site has to be in the key.
pub struct LoopDownloadManager {
    /// Downloaded scan data cache for loop frames, keyed by site then timestamp
    /// (shared across every pane looping that site).
    scan_cache: HashMap<String, HashMap<chrono::NaiveDateTime, Arc<nexrad_model::data::Scan>>>,
    /// Scans currently being downloaded, keyed by site then timestamp (to avoid
    /// duplicate downloads across panes looping the same site).
    in_flight_set: HashMap<String, HashSet<chrono::NaiveDateTime>>,
    /// Pending loop scan downloads per pane, waiting to be dispatched (throttled).
    pending_downloads: HashMap<usize, VecDeque<(chrono::NaiveDateTime, nexrad_data::aws::archive::Identifier)>>,
    /// Number of loop scan downloads currently in flight (global, not per-pane).
    in_flight_count: usize,
}

impl LoopDownloadManager {
    pub fn new() -> Self {
        Self {
            scan_cache: HashMap::new(),
            in_flight_set: HashMap::new(),
            pending_downloads: HashMap::new(),
            in_flight_count: 0,
        }
    }

    /// Number of download slots remaining before hitting the concurrency cap.
    pub fn available_slots(&self, max_concurrent: usize) -> usize {
        max_concurrent.saturating_sub(self.in_flight_count)
    }

    /// Whether this site's scan for the given timestamp is already cached.
    pub fn is_cached(&self, site: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.scan_cache.get(site).is_some_and(|scans| scans.contains_key(ts))
    }

    /// Whether a download of this site's scan for the given timestamp is in flight.
    pub fn is_in_flight(&self, site: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.in_flight_set.get(site).is_some_and(|tss| tss.contains(ts))
    }

    /// Get a cached scan by site and timestamp.
    pub fn get_cached(
        &self,
        site: &str,
        ts: &chrono::NaiveDateTime,
    ) -> Option<&Arc<nexrad_model::data::Scan>> {
        self.scan_cache.get(site)?.get(ts)
    }

    /// Store a downloaded scan in the cache under the site it was downloaded for.
    pub fn cache_scan(
        &mut self,
        site: &str,
        ts: chrono::NaiveDateTime,
        scan: Arc<nexrad_model::data::Scan>,
    ) {
        self.scan_cache.entry(site.to_string()).or_default().insert(ts, scan);
    }

    /// Mark a site's timestamp as currently being downloaded.
    pub fn mark_in_flight(&mut self, site: &str, ts: chrono::NaiveDateTime) {
        self.in_flight_set.entry(site.to_string()).or_default().insert(ts);
    }

    /// Remove a site's timestamp from the in-flight set (download completed or failed).
    pub fn complete_download(&mut self, site: &str, ts: &chrono::NaiveDateTime) {
        if let Some(tss) = self.in_flight_set.get_mut(site) {
            tss.remove(ts);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};

    fn ts(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, minute, 0)
            .unwrap()
    }

    /// A distinct scan value. The contents do not matter — every assertion here is
    /// about *which* `Arc` comes back out, compared by pointer.
    fn scan() -> Arc<Scan> {
        Arc::new(Scan::new(
            VolumeCoveragePattern::new(
                212,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                Vec::new(),
            ),
            Vec::new(),
        ))
    }

    /// The defect. Two panes loop two sites; their volume times collide on a
    /// second, which is uncommon but in no way prevented. With a timestamp-only
    /// key the second insert replaced the first, and the loop that lost the race
    /// rendered the other radar's scan around its own site's coordinates.
    #[test]
    fn one_sites_scan_does_not_displace_another_at_the_same_timestamp() {
        let mut mgr = LoopDownloadManager::new();
        let ktlx = scan();
        let koun = scan();

        mgr.cache_scan("KTLX", ts(0), Arc::clone(&ktlx));
        mgr.cache_scan("KOUN", ts(0), Arc::clone(&koun));

        assert!(
            Arc::ptr_eq(mgr.get_cached("KTLX", &ts(0)).expect("KTLX cached"), &ktlx),
            "KTLX's loop must still get KTLX's scan"
        );
        assert!(
            Arc::ptr_eq(mgr.get_cached("KOUN", &ts(0)).expect("KOUN cached"), &koun),
            "and KOUN's loop KOUN's"
        );
    }

    /// The download filter reads the same key. Without the site, one site's cached
    /// scan made another site's pending download look satisfied, so its frame was
    /// dropped from the queue and never downloaded.
    #[test]
    fn a_cached_scan_for_one_site_does_not_satisfy_another() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), scan());

        assert!(mgr.is_cached("KTLX", &ts(0)));
        assert!(!mgr.is_cached("KOUN", &ts(0)), "KOUN has not downloaded this scan");
        assert!(!mgr.is_cached("KTLX", &ts(1)), "nor KTLX another timestamp");
        assert!(mgr.get_cached("KOUN", &ts(0)).is_none());
    }

    /// The in-flight set is the same hazard one step earlier: a download in flight
    /// for one site must not suppress another site's download of the same
    /// timestamp, or that pane's frame is never fetched and its loop never settles.
    #[test]
    fn a_download_in_flight_for_one_site_does_not_suppress_another() {
        let mut mgr = LoopDownloadManager::new();
        mgr.mark_in_flight("KTLX", ts(0));

        assert!(mgr.is_in_flight("KTLX", &ts(0)));
        assert!(!mgr.is_in_flight("KOUN", &ts(0)));

        // And completing one site's download leaves the other's mark alone.
        mgr.mark_in_flight("KOUN", ts(0));
        mgr.complete_download("KTLX", &ts(0));
        assert!(!mgr.is_in_flight("KTLX", &ts(0)));
        assert!(mgr.is_in_flight("KOUN", &ts(0)), "KOUN is still downloading");
    }

    /// Re-downloading the same site's timestamp replaces the entry, which is what
    /// makes a re-listed loop pick up a completed volume over a partial one.
    #[test]
    fn the_same_site_and_timestamp_is_still_replaced() {
        let mut mgr = LoopDownloadManager::new();
        let first = scan();
        let second = scan();
        mgr.cache_scan("KTLX", ts(0), Arc::clone(&first));
        mgr.cache_scan("KTLX", ts(0), Arc::clone(&second));

        assert!(Arc::ptr_eq(mgr.get_cached("KTLX", &ts(0)).unwrap(), &second));
    }

    /// A site switch drops every site's cached data, not just the one switched away
    /// from — the loops are all rebuilt.
    #[test]
    fn clear_all_empties_every_sites_state() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), scan());
        mgr.cache_scan("KOUN", ts(0), scan());
        mgr.mark_in_flight("KTLX", ts(1));
        mgr.add_spawned(2);

        mgr.clear_all();

        assert!(!mgr.is_cached("KTLX", &ts(0)));
        assert!(!mgr.is_cached("KOUN", &ts(0)));
        assert!(!mgr.is_in_flight("KTLX", &ts(1)));
        assert_eq!(mgr.available_slots(4), 4);
    }
}
