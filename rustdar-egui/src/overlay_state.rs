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
