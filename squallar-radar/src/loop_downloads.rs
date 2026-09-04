use crate::level3::Level3Product;
use crate::types::RadarProduct;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// Which Level III object a loop frame wants: the site whose bucket keys it comes
/// from, the AWIPS code, and the **volume start** the frame names.
pub type L3FrameKey = (String, String, chrono::NaiveDateTime);

/// What a loop frame has to render, once its data has arrived.
pub enum LoopFrameData {
    /// A decoded Level II volume and what its cuts declared their Nyquist
    /// velocities to be; the renderer picks its sweep out of the first and
    /// folds that sweep's velocity around the second.
    Volume(
        Arc<nexrad_model::data::Scan>,
        Arc<crate::nyquist::DeclaredNyquist>,
    ),
    /// The Level III objects of this frame's volume, one per AWIPS code in
    /// [`RadarProduct::level3_products`] order.
    Products(Vec<Arc<Level3Product>>),
}

/// What a loop frame's render needs that the frame's own data does not carry:
/// the pane's selection, and the four per-render rungs the dispatcher above
/// holds for the site.
///
/// Every field is read by exactly one arm of
/// [`frame_render_job`](LoopDownloadManager::frame_render_job) or by both; the
/// dispatcher fills it in without knowing which arm will read what.
pub struct LoopRenderContext {
    /// The product the pane's loop is rendering.
    pub product: RadarProduct,
    /// The sweep angle the pane's elevation selection snapped to.
    pub elevation: f32,
    pub lat: f64,
    pub lon: f64,
    /// The storm motion override, where a lower rung does not apply.
    pub storm_motion: Option<(f32, f32)>,
    /// The site's `(0 °C, −20 °C)` pair in km MSL, for products that read them.
    pub env_heights: Option<(f64, f64)>,
    /// Which derived rung a Level II render's payload carries.
    pub srv_fallback: crate::srv::SrvFallback,
    /// The `N0M` object **this frame's own volume** may classify against.
    pub melting_layer: Option<Arc<Vec<u8>>>,
    /// The RPG's storm motion vector for **this frame's own volume**.
    pub rpg_storm_motion: Option<(f32, f32)>,
}

/// **Whether `site`'s decoded Level II volumes are still needed by anything
/// that loops** — the one predicate both retention and loop admission ask.
///
/// `live_loops` names every loop running right now as `(site, product)`, with
/// `None` for a loop that has not dispatched yet and so has not said what it
/// renders.
///
/// A loop's frames come from one of two sources, and
/// [`LoopDownloadManager::plan_downloads_for`] already decides which by asking
/// [`RadarProduct::level3_products`]: a product naming AWIPS codes queues
/// Level III pairings and downloads no volume at all, and a product naming
/// none downloads the Level II volume every one of its frames is derived
/// from. This asks that same question of a whole site, so retention cannot
/// disagree with the download planner about what a site's volumes are for.
///
/// One loop is enough: two panes share one site's cache, so a Level II loop
/// on `site` keeps its volumes however many Level III loops sit beside it.
///
/// **`true` for a site whose loop has not said what it renders.** A loop that
/// has not dispatched cannot be shown to need nothing, and the safe direction
/// is the one that holds bytes rather than the one that costs a re-download.
///
/// **This is the peer-facing handle.** The loop admission door prices a loop
/// on exactly this distinction — full decoded frames where this answers
/// `true`, textures plus whatever single volume a pane is parked at where it
/// answers `false` — and the table it publishes reads this function rather
/// than a second copy of the rule that would be free to drift from it.
///
/// O(`live_loops`), which holds one entry per pane with a running loop.
pub fn site_needs_decoded_source(site: &str, live_loops: &[(&str, Option<RadarProduct>)]) -> bool {
    live_loops.iter().any(|&(loop_site, product)| {
        loop_site == site && product.is_none_or(|p| p.level3_products().is_none())
    })
}

/// One downloaded volume as the loop caches it: the sweeps, and what their cuts
/// declared their Nyquist velocities to be.
pub type CachedVolume = (
    Arc<nexrad_model::data::Scan>,
    Arc<crate::nyquist::DeclaredNyquist>,
);

/// Whether a frame's Level III objects have arrived.
#[derive(Debug, PartialEq, Eq)]
pub enum L3FrameState {
    /// Every code the product needs is paired to this frame's volume.
    Ready,
    /// At least one code was paired and the site generated no object for this
    /// volume. A gap — normal, terminal, and not an error: nothing will ever
    /// render this frame, so it is retired the way an unrenderable Level II frame
    /// is.
    Absent,
    /// At least one code has not been paired yet.
    Pending,
}

/// Manages loop radar download state: scan cache, in-flight tracking,
/// and per-pane pending download queues. Grouping these together prevents
/// partial updates that could leave the fields in an inconsistent state.
pub struct LoopDownloadManager {
    /// Downloaded scan data cache for loop frames, keyed by site then timestamp
    /// (shared across every pane looping that site).
    scan_cache: HashMap<String, HashMap<chrono::NaiveDateTime, CachedVolume>>,
    /// Scans currently being downloaded, keyed by site then timestamp (to avoid
    /// duplicate downloads across panes looping the same site).
    in_flight_set: HashMap<String, HashSet<chrono::NaiveDateTime>>,
    /// Pending loop scan downloads per pane, waiting to be dispatched (throttled).
    pending_downloads: HashMap<usize, PendingDownloads>,
    /// Pending Level III pairings per pane, the counterpart of
    /// [`pending_downloads`](Self::pending_downloads).
    pending_l3: HashMap<usize, PendingL3Pairings>,
    /// Every frame's volume for each pane's loop, so the download queues can be
    /// re-derived when the pane retargets across the Level II / Level III line
    /// without re-listing the archive. See [`FramePlan`].
    plans: HashMap<usize, FramePlan>,
    /// The bucket keys serving one `(site, AWIPS code)` over the days a loop's
    /// window touches, listed **once** and then ranked per frame.
    l3_keys: HashMap<(String, String), Arc<Vec<String>>>,
    /// `(site, code)` listings under way, so two panes looping one site do not
    /// both list it.
    l3_keys_in_flight: HashSet<(String, String)>,
    /// The object paired to each frame's volume, or `None` where the site
    /// generated none.
    l3_cache: HashMap<L3FrameKey, Option<Arc<Level3Product>>>,
    /// Pairings under way, the Level III counterpart of
    /// [`in_flight_set`](Self::in_flight_set).
    l3_in_flight: HashSet<L3FrameKey>,
    /// Number of loop downloads currently in flight (global, not per-pane, and
    /// shared by the Level II and Level III paths so the network concurrency cap
    /// means one thing).
    in_flight_count: usize,
    /// **What [`scan_cache`](Self::scan_cache) is holding, in host bytes**, by
    /// [`crate::scan_size::scan_bytes`].
    ///
    /// A running total rather than a walk, because the answer is wanted every
    /// telemetry tick and the walk is over every radial of every cached
    /// volume: each volume is priced ONCE where it is filed, and the price is
    /// kept beside it so eviction is a subtraction. The cache is bounded by
    /// **frame count and nothing else** — one decoded volume per named loop
    /// frame — and a volume was measured at 46.1-46.8 MiB median, 58.3 MiB
    /// max over 208 real archive volumes, so on a 1 GiB wasm page heap this
    /// is a figure that decides whether a scene fits.
    scan_bytes_cached: usize,
    /// The price of each cached volume, so [`Self::retain_scans`] subtracts
    /// what it removes instead of re-walking it. Keyed exactly as
    /// [`scan_cache`](Self::scan_cache) is addressed, and every mutation of
    /// one is a mutation of the other.
    scan_prices: HashMap<(String, chrono::NaiveDateTime), usize>,
    /// **What [`l3_cache`](Self::l3_cache) is holding, in host bytes** — each
    /// product's `bytes` buffer, which is nearly all of it. O(1) to price, so
    /// there is no price map beside it.
    l3_bytes_cached: usize,
}

/// A pane's undispatched loop downloads, with the site they belong to.
pub struct PendingDownloads {
    /// The site the listing was made for. Every volume in `queue` is one of
    /// this site's, and the scan each becomes is cached under it.
    pub site: String,
    /// Volume starts still to download, oldest-first. The **archive object**
    /// each one is stays with the layer that listed it — nothing here holds an
    /// identifier, so nothing here can download from the wrong site's bucket.
    pub queue: VecDeque<chrono::NaiveDateTime>,
}

/// A pane's undispatched Level III pairings, with the site they belong to.
pub struct PendingL3Pairings {
    /// The site whose bucket keys every entry below is paired against.
    pub site: String,
    /// The product these pairings are for.
    pub product: RadarProduct,
    /// `(volume start, AWIPS code)` still to pair, oldest volume first.
    pub queue: VecDeque<(chrono::NaiveDateTime, String)>,
}

/// Every volume a pane's loop frames name, kept so the download queues can be
/// re-derived without re-listing the archive.
pub struct FramePlan {
    /// The site the listing was made for; every volume below is one of its
    /// own, and every pairing derived from this plan is against its keys.
    pub site: String,
    /// Volume start per frame, oldest-first.
    pub frames: Vec<chrono::NaiveDateTime>,
    /// The product the queues were last derived for. Compared, not assumed:
    /// re-deriving on every dispatch pass would rebuild both queues every frame
    /// of the UI, and re-deriving never would leave a retargeted pane waiting on
    /// data nothing was fetching.
    planned_for: Option<RadarProduct>,
}

impl FramePlan {
    /// A plan for a fresh listing, with nothing derived from it yet.
    pub fn new(site: String, frames: Vec<chrono::NaiveDateTime>) -> Self {
        Self {
            site,
            frames,
            planned_for: None,
        }
    }
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
            in_flight_set: HashMap::new(),
            pending_downloads: HashMap::new(),
            pending_l3: HashMap::new(),
            plans: HashMap::new(),
            l3_keys: HashMap::new(),
            l3_keys_in_flight: HashSet::new(),
            l3_cache: HashMap::new(),
            l3_in_flight: HashSet::new(),
            in_flight_count: 0,
            scan_bytes_cached: 0,
            scan_prices: HashMap::new(),
            l3_bytes_cached: 0,
        }
    }

    /// Number of download slots remaining before hitting the concurrency cap.
    pub fn available_slots(&self, max_concurrent: usize) -> usize {
        max_concurrent.saturating_sub(self.in_flight_count)
    }

    /// Whether this site's scan for the given timestamp is already cached.
    pub fn is_cached(&self, site: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.scan_cache
            .get(site)
            .is_some_and(|scans| scans.contains_key(ts))
    }

    /// Whether a download of this site's scan for the given timestamp is in flight.
    pub fn is_in_flight(&self, site: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.in_flight_set
            .get(site)
            .is_some_and(|tss| tss.contains(ts))
    }

    /// Get a cached volume and its declarations by site and timestamp.
    pub fn get_cached(&self, site: &str, ts: &chrono::NaiveDateTime) -> Option<&CachedVolume> {
        self.scan_cache.get(site)?.get(ts)
    }

    /// Every cached loop volume, any site, any timestamp — the loop cache's
    /// contribution to the set of live volumes [`crate::derive::retain_volumes`]
    /// keeps derivation-memo entries for.
    pub fn cached_scans(&self) -> impl Iterator<Item = &nexrad_model::data::Scan> {
        self.scan_cache
            .values()
            .flat_map(|scans| scans.values().map(|(scan, _)| scan.as_ref()))
    }

    // ------------------------------------------------------------------
    // Test probes, unconditional because their consumers live app-side in
    // `squallar-app`, across a crate boundary `cfg(test)` cannot reach.
    // ------------------------------------------------------------------

    /// How many volumes this site is holding.
    pub fn cached_scan_count(&self, site: &str) -> usize {
        self.scan_cache.get(site).map_or(0, |scans| scans.len())
    }

    /// How many frames this pane's plan still names.
    pub fn plan_frame_count(&self, pane: usize) -> usize {
        self.plans.get(&pane).map_or(0, |plan| plan.frames.len())
    }

    /// How many volume downloads this pane still has queued and undispatched.
    pub fn pending_queue_count(&self, pane: usize) -> usize {
        self.pending_downloads
            .get(&pane)
            .map_or(0, |pending| pending.queue.len())
    }

    /// How many Level III objects this site is holding, **gaps included**.
    pub fn cached_l3_count(&self, site: &str) -> usize {
        self.l3_cache
            .keys()
            .filter(|(cached, _, _)| cached == site)
            .count()
    }

    /// How many pairings this pane still has queued and undispatched.
    pub fn pending_l3_queue_count(&self, pane: usize) -> usize {
        self.pending_l3
            .get(&pane)
            .map_or(0, |pending| pending.queue.len())
    }

    /// Whether the map has an entry for this site at all.
    pub fn has_cached_site(&self, site: &str) -> bool {
        self.scan_cache.contains_key(site)
    }

    /// Store a downloaded volume in the cache under the site it was downloaded
    /// for, with what its cuts declared.
    ///
    /// The volume is priced here and nowhere else. That is one walk of its
    /// radials per arrival, on the path a decode has just finished — the one
    /// moment the volume's bytes are already the expensive thing that
    /// happened — rather than a walk per reading of
    /// [`Self::cached_scan_bytes`], which is asked for every telemetry tick.
    pub fn cache_scan(&mut self, site: &str, ts: chrono::NaiveDateTime, volume: CachedVolume) {
        let price = crate::scan_size::scan_bytes(&volume.0);
        // A re-file under a key already held replaces the volume, so its old
        // price leaves with it; `insert` returning the old price is what says
        // whether there was one.
        if let Some(was) = self.scan_prices.insert((site.to_string(), ts), price) {
            self.scan_bytes_cached = self.scan_bytes_cached.saturating_sub(was);
        }
        self.scan_bytes_cached = self.scan_bytes_cached.saturating_add(price);
        self.scan_cache
            .entry(site.to_string())
            .or_default()
            .insert(ts, volume);
    }

    /// **Host bytes the loop's decoded volumes are holding**, by
    /// [`crate::scan_size::scan_bytes`] — a floor, since the allocator's own
    /// overhead is not reachable from a slice. O(1): the total is maintained
    /// at the two places the cache changes.
    ///
    /// It is what emptying this cache would free **if nothing else held the
    /// same volumes**, and something else usually does: the still inventory
    /// and the derivation memo hold `Arc`s of the same `Scan`s. Summing this
    /// with theirs gives an upper bound on the joint footprint, not a
    /// partition of it.
    pub fn cached_scan_bytes(&self) -> usize {
        self.scan_bytes_cached
    }

    /// **What one cached volume was priced at**, the figure
    /// [`Self::cache_scan`] computed at arrival. `None` where this cache is
    /// not holding that volume.
    ///
    /// It exists so a holder that outlives the cache entry — a loop frame's
    /// `HoverSource`, which `Arc::clone`s the volume out of here — can carry
    /// the figure instead of walking the radials again on the frame thread.
    /// O(1): one map lookup, and the `String` its key wants is a four-letter
    /// site name.
    pub fn cached_scan_price(&self, site: &str, ts: &chrono::NaiveDateTime) -> Option<usize> {
        self.scan_prices.get(&(site.to_string(), *ts)).copied()
    }

    /// **Host bytes the loop's paired Level III objects are holding** — the
    /// product buffers. O(1), for [`Self::cached_scan_bytes`]'s reason.
    pub fn cached_l3_bytes(&self) -> usize {
        self.l3_bytes_cached
    }

    /// Take out every cached volume whose `(site, timestamp)` fails `keep`, and
    /// hand the removed values back **owned**.
    pub fn retain_scans(
        &mut self,
        keep: impl Fn(&str, &chrono::NaiveDateTime) -> bool,
    ) -> Vec<CachedVolume> {
        let mut removed = Vec::new();
        self.scan_cache.retain(|site, scans| {
            removed.extend(
                scans
                    .extract_if(|ts, _| !keep(site.as_str(), ts))
                    .map(|(_, volume)| volume),
            );
            !scans.is_empty()
        });
        // The prices go by the same predicate, so the total falls by exactly
        // what left rather than by a second walk of the volumes now removed.
        for (_, price) in self
            .scan_prices
            .extract_if(|(site, ts), _| !keep(site.as_str(), ts))
        {
            self.scan_bytes_cached = self.scan_bytes_cached.saturating_sub(price);
        }
        removed
    }

    /// Drop from every frame plan, and from every undispatched volume queue,
    /// the entries whose `(site, timestamp)` fails `keep`.
    pub fn retain_plan_frames(&mut self, keep: impl Fn(&str, &chrono::NaiveDateTime) -> bool) {
        for plan in self.plans.values_mut() {
            plan.frames.retain(|ts| keep(plan.site.as_str(), ts));
        }
        for pending in self.pending_downloads.values_mut() {
            pending.queue.retain(|ts| keep(pending.site.as_str(), ts));
        }
    }

    /// Mark a site's timestamp as currently being downloaded.
    pub fn mark_in_flight(&mut self, site: &str, ts: chrono::NaiveDateTime) {
        self.in_flight_set
            .entry(site.to_string())
            .or_default()
            .insert(ts);
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

    /// Set the pending download queue for a pane, with the site it was listed for.
    pub fn insert_pending(&mut self, pane: usize, pending: PendingDownloads) {
        self.pending_downloads.insert(pane, pending);
    }

    /// Remove a pane's pending download queue — both halves, and the plan they
    /// were derived from.
    pub fn remove_pending(&mut self, pane: usize) {
        self.pending_downloads.remove(&pane);
        self.pending_l3.remove(&pane);
        self.plans.remove(&pane);
    }

    /// Record what volumes a pane's loop frames name, replacing any previous
    /// plan and the queues derived from it.
    pub fn set_plan(&mut self, pane: usize, plan: FramePlan) {
        self.pending_downloads.remove(&pane);
        self.pending_l3.remove(&pane);
        self.plans.insert(pane, plan);
    }

    /// Derive this pane's download queues for `product`, returning whether
    /// anything changed.
    pub fn plan_downloads_for(&mut self, pane: usize, product: RadarProduct) -> bool {
        let Some(plan) = self.plans.get_mut(&pane) else {
            return false;
        };
        if plan.planned_for == Some(product) {
            return false;
        }
        plan.planned_for = Some(product);
        let site = plan.site.clone();
        match product.level3_products() {
            Some(codes) => {
                self.pending_downloads.remove(&pane);
                let queue = plan
                    .frames
                    .iter()
                    .flat_map(|ts| codes.iter().map(move |code| (*ts, (*code).to_string())))
                    .collect();
                self.pending_l3.insert(
                    pane,
                    PendingL3Pairings {
                        site,
                        product,
                        queue,
                    },
                );
            }
            None => {
                self.pending_l3.remove(&pane);
                let queue = plan.frames.iter().copied().collect();
                self.pending_downloads
                    .insert(pane, PendingDownloads { site, queue });
            }
        }
        true
    }

    /// Extract a pane's pending pairings completely, mirroring
    /// [`extract_pending`](Self::extract_pending) — the site and the codes come
    /// out with the queue, so a caller cannot dispatch one pane's pairings while
    /// naming another's site.
    pub fn extract_pending_l3(&mut self, pane: usize) -> Option<PendingL3Pairings> {
        self.pending_l3.remove(&pane)
    }

    /// Return a queue taken by [`extract_pending_l3`](Self::extract_pending_l3).
    pub fn insert_pending_l3(&mut self, pane: usize, pending: PendingL3Pairings) {
        self.pending_l3.insert(pane, pending);
    }

    /// Claim the key listing for `(site, code)`, returning whether the caller now
    /// owes one.
    pub fn claim_l3_listing(&mut self, site: &str, code: &str) -> bool {
        let key = (site.to_string(), code.to_string());
        if self.l3_keys.contains_key(&key) || self.l3_keys_in_flight.contains(&key) {
            return false;
        }
        self.l3_keys_in_flight.insert(key);
        true
    }

    /// Record a finished key listing. An empty list is stored, not discarded: it
    /// is the answer "this site served no objects", which every frame then
    /// resolves to a gap.
    pub fn cache_l3_keys(&mut self, site: &str, code: &str, keys: Vec<String>) {
        let key = (site.to_string(), code.to_string());
        self.l3_keys_in_flight.remove(&key);
        self.l3_keys.insert(key, Arc::new(keys));
    }

    /// The cached key listing for `(site, code)`, or `None` if it has not landed.
    pub fn l3_keys(&self, site: &str, code: &str) -> Option<&Arc<Vec<String>>> {
        self.l3_keys.get(&(site.to_string(), code.to_string()))
    }

    /// Drop the bucket-key listings of every site `keep_site` refuses, and hand
    /// them back owned.
    pub fn retain_l3_keys(&mut self, keep_site: impl Fn(&str) -> bool) -> Vec<Arc<Vec<String>>> {
        self.l3_keys
            .extract_if(|(site, _), _| !keep_site(site.as_str()))
            .map(|(_, keys)| keys)
            .collect()
    }

    fn l3_key(site: &str, code: &str, ts: &chrono::NaiveDateTime) -> L3FrameKey {
        (site.to_string(), code.to_string(), *ts)
    }

    /// Whether this frame's object for `code` has been paired — including having
    /// been paired to nothing.
    pub fn l3_is_resolved(&self, site: &str, code: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.l3_cache.contains_key(&Self::l3_key(site, code, ts))
    }

    /// Whether a pairing for this frame's object is under way.
    pub fn l3_is_in_flight(&self, site: &str, code: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.l3_in_flight.contains(&Self::l3_key(site, code, ts))
    }

    /// Mark a pairing as under way.
    pub fn mark_l3_in_flight(&mut self, site: &str, code: &str, ts: chrono::NaiveDateTime) {
        self.l3_in_flight.insert(Self::l3_key(site, code, &ts));
    }

    /// Record a finished pairing: clear the in-flight mark and store the result,
    /// `None` included.
    pub fn cache_l3_product(
        &mut self,
        site: &str,
        code: &str,
        ts: chrono::NaiveDateTime,
        product: Option<Arc<Level3Product>>,
    ) {
        let key = Self::l3_key(site, code, &ts);
        self.l3_in_flight.remove(&key);
        let price = product.as_ref().map_or(0, |p| p.bytes.len());
        if let Some(Some(was)) = self.l3_cache.insert(key, product) {
            self.l3_bytes_cached = self.l3_bytes_cached.saturating_sub(was.bytes.len());
        }
        self.l3_bytes_cached = self.l3_bytes_cached.saturating_add(price);
    }

    /// Take out every paired Level III object whose `(site, volume start)` fails
    /// `keep` and hand the removed products back **owned**, then drop the
    /// undispatched pairings the same predicate refuses.
    pub fn retain_l3(
        &mut self,
        keep: impl Fn(&str, &chrono::NaiveDateTime) -> bool,
    ) -> Vec<Arc<Level3Product>> {
        let removed: Vec<Arc<Level3Product>> = self
            .l3_cache
            .extract_if(|(site, _, ts), _| !keep(site.as_str(), ts))
            // A gap's key goes with the rest and its value is nothing to hand
            // over.
            .filter_map(|(_, product)| product)
            .collect();
        for product in &removed {
            self.l3_bytes_cached = self.l3_bytes_cached.saturating_sub(product.bytes.len());
        }
        for pending in self.pending_l3.values_mut() {
            pending
                .queue
                .retain(|(ts, _)| keep(pending.site.as_str(), ts));
        }
        removed
    }

    /// Whether frame `ts` of `product`'s loop on `site` has every object it
    /// needs, is missing one for good, or is still waiting.
    pub fn l3_frame_state(
        &self,
        site: &str,
        product: RadarProduct,
        ts: &chrono::NaiveDateTime,
    ) -> L3FrameState {
        let Some(codes) = product.level3_products() else {
            return L3FrameState::Absent;
        };
        let mut pending = false;
        for code in codes {
            match self.l3_cache.get(&Self::l3_key(site, code, ts)) {
                Some(Some(_)) => {}
                // Paired to nothing: terminal, and it decides the frame outright
                // — no later code can supply the missing input.
                Some(None) => return L3FrameState::Absent,
                None => pending = true,
            }
        }
        if pending {
            L3FrameState::Pending
        } else {
            L3FrameState::Ready
        }
    }

    /// The objects frame `ts` renders, in [`RadarProduct::level3_products`] order,
    /// or `None` unless every one of them is present.
    pub fn l3_frame_products(
        &self,
        site: &str,
        product: RadarProduct,
        ts: &chrono::NaiveDateTime,
    ) -> Option<Vec<Arc<Level3Product>>> {
        let codes = product.level3_products()?;
        codes
            .iter()
            .map(|code| {
                self.l3_cache
                    .get(&Self::l3_key(site, code, ts))?
                    .as_ref()
                    .map(Arc::clone)
            })
            .collect()
    }

    /// Everything frame `ts` of `product`'s loop on `site` needs to render, or
    /// `None` if it has not all arrived.
    pub fn frame_data(
        &self,
        site: &str,
        product: RadarProduct,
        ts: &chrono::NaiveDateTime,
    ) -> Option<LoopFrameData> {
        if product.is_level3() {
            return self
                .l3_frame_products(site, product, ts)
                .map(LoopFrameData::Products);
        }
        self.get_cached(site, ts)
            .map(|(scan, declared)| LoopFrameData::Volume(Arc::clone(scan), Arc::clone(declared)))
    }

    /// Whether frame `ts` of `product`'s loop on `site` has everything it needs
    /// to render — [`frame_data`](Self::frame_data)'s own question, asked without
    /// building the answer's arms.
    ///
    /// Delegates rather than re-deciding: a second copy of "which cache does this
    /// product read" is exactly the kind that drifts from the first.
    pub fn frame_data_arrived(
        &self,
        site: &str,
        product: RadarProduct,
        ts: &chrono::NaiveDateTime,
    ) -> bool {
        self.frame_data(site, product, ts).is_some()
    }

    /// The Level II volume frame `ts` of `product`'s loop on `site` renders.
    ///
    /// `None` where [`frame_data`](Self::frame_data) is `None`, and also where it
    /// answers the Level III arm: a loop whose product reads objects has no
    /// volume to hand out, whatever this site's volume cache happens to hold.
    pub fn frame_volume(
        &self,
        site: &str,
        product: RadarProduct,
        ts: &chrono::NaiveDateTime,
    ) -> Option<CachedVolume> {
        match self.frame_data(site, product, ts)? {
            LoopFrameData::Volume(scan, declared) => Some((scan, declared)),
            LoopFrameData::Products(_) => None,
        }
    }

    /// The described render job frame `ts` of `product`'s loop on `site` runs,
    /// with its concrete input type erased.
    ///
    /// This is the whole of what a loop frame's *closed arms* decide: which job
    /// input a frame's own data makes, and what of `ctx` each arm reads. The
    /// dispatcher above holds the answer without naming either arm — the codec
    /// row that owns the input type is what runs it (`crate::jobs::JOB_CODECS`).
    ///
    /// `None` is "there is nothing to draw", not "the data has not arrived":
    /// callers ask [`frame_data`](Self::frame_data) for the latter. The two
    /// `None` sources here are a volume with no such sweep and a Level III frame
    /// whose object list is empty.
    pub fn frame_render_job(
        &self,
        site: &str,
        ts: &chrono::NaiveDateTime,
        ctx: &LoopRenderContext,
    ) -> Option<squallar_source::job::DescribedJob> {
        match self.frame_data(site, ctx.product, ts)? {
            // The scan is reduced to the one sweep this frame draws before the
            // job is dispatched.
            LoopFrameData::Volume(scan_data, declared) => {
                let input = crate::render_input::RenderInput::extract(
                    &scan_data,
                    ctx.elevation,
                    ctx.product,
                    ctx.lat,
                    ctx.lon,
                    ctx.storm_motion,
                    ctx.env_heights,
                )?;
                Some(squallar_source::job::DescribedJob::new(
                    crate::jobs::RadarPlanJob {
                        // The same stamp the still frame takes, off this frame's
                        // own volume.
                        input: Box::new(
                            input
                                .with_declared_nyquist(&declared)
                                .with_srv_fallback(ctx.srv_fallback)
                                .with_melting_layer_product(ctx.melting_layer.clone())
                                .with_rpg_storm_motion(ctx.rpg_storm_motion),
                        ),
                        // Loop frames store an empty value grid.
                        values_wanted: false,
                    },
                ))
            }
            // The object's *bytes*, exactly as the static Level III pane render
            // dispatches them (`try_spawn_level3_render`).
            LoopFrameData::Products(products) => {
                let first = products.first()?;
                Some(squallar_source::job::DescribedJob::new(
                    crate::jobs::Level3Job {
                        bytes: Arc::clone(&first.bytes),
                        product: ctx.product,
                        radar_lat: ctx.lat,
                        radar_lon: ctx.lon,
                    },
                ))
            }
        }
    }

    /// Whether frame `ts`'s data question has been *answered* — the volume is
    /// cached, or every Level III object has been paired, gaps included.
    pub fn frame_data_settled(
        &self,
        site: &str,
        product: RadarProduct,
        ts: &chrono::NaiveDateTime,
    ) -> bool {
        if product.is_level3() {
            return self.l3_frame_state(site, product, ts) != L3FrameState::Pending;
        }
        self.is_cached(site, ts)
    }

    /// Whether a download or pairing for frame `ts` is under way.
    pub fn frame_data_in_flight(
        &self,
        site: &str,
        product: RadarProduct,
        ts: &chrono::NaiveDateTime,
    ) -> bool {
        match product.level3_products() {
            Some(codes) => codes
                .iter()
                .any(|code| self.l3_is_in_flight(site, code, ts)),
            None => self.is_in_flight(site, ts),
        }
    }

    /// Extract the pending queue completely. Call `insert_pending` to return it later.
    pub fn extract_pending(&mut self, pane: usize) -> Option<PendingDownloads> {
        self.pending_downloads.remove(&pane)
    }

    /// Collect all pane indices that have pending download entries.
    pub fn pending_pane_indices(&self) -> Vec<usize> {
        self.pending_downloads.keys().copied().collect()
    }

    /// Collect all pane indices that have pending Level III pairings.
    pub fn pending_l3_pane_indices(&self) -> Vec<usize> {
        self.pending_l3.keys().copied().collect()
    }

    /// Whether every download a pane owes — volume or object — has been
    /// dispatched.
    pub fn is_pane_done(&self, pane: usize) -> bool {
        self.pending_downloads
            .get(&pane)
            .is_none_or(|p| p.queue.is_empty())
            && self
                .pending_l3
                .get(&pane)
                .is_none_or(|p| p.queue.is_empty())
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

    /// A distinct cached volume. The contents do not matter — every assertion
    /// here is about *which* `Arc` comes back out, compared by pointer — and
    /// nothing here reads the declarations, so the fixture declares nothing.
    fn volume() -> CachedVolume {
        (scan(), Arc::default())
    }

    /// A distinct scan value.
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
        let ktlx = volume();
        let koun = volume();

        mgr.cache_scan("KTLX", ts(0), ktlx.clone());
        mgr.cache_scan("KOUN", ts(0), koun.clone());

        assert!(
            Arc::ptr_eq(
                &mgr.get_cached("KTLX", &ts(0)).expect("KTLX cached").0,
                &ktlx.0
            ),
            "KTLX's loop must still get KTLX's scan"
        );
        assert!(
            Arc::ptr_eq(
                &mgr.get_cached("KOUN", &ts(0)).expect("KOUN cached").0,
                &koun.0
            ),
            "and KOUN's loop KOUN's"
        );
    }

    /// The download filter reads the same key. Without the site, one site's cached
    /// scan made another site's pending download look satisfied, so its frame was
    /// dropped from the queue and never downloaded.
    #[test]
    fn a_cached_scan_for_one_site_does_not_satisfy_another() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), volume());

        assert!(mgr.is_cached("KTLX", &ts(0)));
        assert!(
            !mgr.is_cached("KOUN", &ts(0)),
            "KOUN has not downloaded this scan"
        );
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
        assert!(
            mgr.is_in_flight("KOUN", &ts(0)),
            "KOUN is still downloading"
        );
    }

    /// A cached volume that actually carries gates, so it prices at something.
    /// The shared `volume()` fixture declares no sweeps on purpose — every
    /// other test here compares `Arc` pointers and never reads a gate — and a
    /// volume of no sweeps correctly prices at zero, which is exactly the
    /// value the byte assertions below could not tell from a broken total.
    fn priced_volume() -> CachedVolume {
        use nexrad_model::data::{MomentData, Radial, RadialStatus, Sweep};

        let radials = (0..8)
            .map(|i| {
                Radial::new(
                    1_700_000_000_000,
                    i,
                    f32::from(i),
                    0.5,
                    RadialStatus::IntermediateRadialData,
                    1,
                    0.5,
                    Some(MomentData::from_fixed_point(
                        400,
                        2125,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![3u8; 400],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        (
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
                vec![Sweep::new(1, radials)],
            )),
            Arc::default(),
        )
    }

    /// **The byte totals track what the caches hold**, over a file, a
    /// replacement and an eviction.
    ///
    /// The totals exist because the caches are bounded by frame count and by
    /// nothing else, so the only way to know what a loop is holding is to
    /// keep the figure. A replacement that added instead of replacing, or an
    /// eviction that forgot to subtract, would leave a monotonic number that
    /// looked like a leak in the very instrument built to find one.
    #[test]
    fn the_cached_byte_totals_track_the_caches() {
        let mut mgr = LoopDownloadManager::new();
        assert_eq!(mgr.cached_scan_bytes(), 0);
        assert_eq!(mgr.cached_l3_bytes(), 0);

        mgr.cache_scan("KTLX", ts(0), priced_volume());
        let one = mgr.cached_scan_bytes();
        assert!(one > 0, "a filed volume priced at nothing");

        mgr.cache_scan("KTLX", ts(1), priced_volume());
        assert_eq!(mgr.cached_scan_bytes(), 2 * one, "the second volume");

        // A replacement under a held key swaps the price, it does not add one.
        mgr.cache_scan("KTLX", ts(1), priced_volume());
        assert_eq!(
            mgr.cached_scan_bytes(),
            2 * one,
            "re-filing a held key double-counted it"
        );

        let l3_product = l3();
        mgr.cache_l3_product("KTLX", "EET", ts(0), Some(l3_product.clone()));
        assert_eq!(mgr.cached_l3_bytes(), l3_product.bytes.len());
        mgr.cache_l3_product("KTLX", "NMD", ts(0), None);
        assert_eq!(
            mgr.cached_l3_bytes(),
            l3_product.bytes.len(),
            "a gap paired to nothing was charged for bytes it has not got"
        );

        mgr.retain_scans(|_, at| *at == ts(0));
        assert_eq!(mgr.cached_scan_bytes(), one, "eviction did not subtract");
        mgr.retain_scans(|_, _| false);
        assert_eq!(mgr.cached_scan_bytes(), 0, "an emptied cache still priced");
        mgr.retain_l3(|_, _| false);
        assert_eq!(mgr.cached_l3_bytes(), 0);
    }

    /// Re-downloading the same site's timestamp replaces the entry, which is what
    /// makes a re-listed loop pick up a completed volume over a partial one.
    #[test]
    fn the_same_site_and_timestamp_is_still_replaced() {
        let mut mgr = LoopDownloadManager::new();
        let first = volume();
        let second = volume();
        mgr.cache_scan("KTLX", ts(0), first.clone());
        mgr.cache_scan("KTLX", ts(0), second.clone());

        assert!(Arc::ptr_eq(
            &mgr.get_cached("KTLX", &ts(0)).unwrap().0,
            &second.0
        ));
    }

    #[test]
    fn a_departing_pane_takes_only_its_own_pending_work() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), volume());
        mgr.cache_scan("KOUN", ts(0), volume());
        mgr.cache_l3_product("KOUN", "EET", ts(0), Some(l3()));
        mgr.mark_in_flight("KTLX", ts(1));
        for (pane, site) in [(0usize, "KTLX"), (1, "KOUN")] {
            mgr.insert_pending(
                pane,
                PendingDownloads {
                    site: site.to_string(),
                    queue: [ts(2)].into_iter().collect(),
                },
            );
        }
        mgr.add_spawned(2);
        assert!(
            !mgr.is_pane_done(0) && !mgr.is_pane_done(1),
            "precondition: both panes have a download queued"
        );

        mgr.remove_pending(0);

        assert!(
            mgr.is_pane_done(0),
            "the departing pane is still owed a download"
        );
        assert_eq!(
            mgr.pending_pane_indices(),
            vec![1],
            "either the departing pane left a queue entry to be dispatched for \
             the radar it is no longer on, or the bystander's went with it"
        );
        // The caches are keyed by site, not by pane, and are the sweep's to
        // collect.
        assert!(
            mgr.is_cached("KTLX", &ts(0)) && mgr.is_cached("KOUN", &ts(0)),
            "a per-pane teardown emptied the shared volume cache"
        );
        assert!(
            mgr.l3_is_resolved("KOUN", "EET", &ts(0)),
            "a per-pane teardown emptied the shared Level III cache"
        );
        assert!(
            mgr.is_in_flight("KTLX", &ts(1)),
            "a download already on the wire lost its mark, so the same file is \
             requested a second time"
        );
        assert_eq!(
            mgr.available_slots(4),
            2,
            "the concurrency counter moved, so the cap no longer counts what is \
             actually running"
        );
    }

    /// `retain_scans` hands the evicted volumes back rather than freeing them.
    #[test]
    fn retain_scans_returns_the_volumes_it_removed() {
        let mut mgr = LoopDownloadManager::new();
        let doomed = volume();
        let kept = volume();
        mgr.cache_scan("KTLX", ts(0), doomed.clone());
        mgr.cache_scan("KTLX", ts(1), kept.clone());

        let removed = mgr.retain_scans(|_, stamp| *stamp == ts(1));

        assert_eq!(removed.len(), 1, "one entry failed the predicate");
        assert!(
            Arc::ptr_eq(&removed[0].0, &doomed.0),
            "the value handed back is not the one that was evicted, so the \
             caller cannot hand the evicted volume over",
        );
        assert!(
            Arc::ptr_eq(&mgr.get_cached("KTLX", &ts(1)).expect("kept").0, &kept.0),
            "the surviving entry was replaced",
        );
    }

    /// Both halves of the key reach the predicate.
    #[test]
    fn retain_scans_judges_the_site_as_well_as_the_timestamp() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), volume());
        mgr.cache_scan("KOUN", ts(0), volume());

        let removed = mgr.retain_scans(|site, _| site == "KTLX");

        assert_eq!(removed.len(), 1);
        assert!(mgr.is_cached("KTLX", &ts(0)));
        assert!(
            !mgr.is_cached("KOUN", &ts(0)),
            "KOUN's entry survived a predicate that named only KTLX",
        );
    }

    /// A site that loses its last entry loses its inner map too.
    #[test]
    fn retain_scans_prunes_a_site_it_emptied() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), volume());
        mgr.cache_scan("KOUN", ts(0), volume());
        mgr.cache_scan("KOUN", ts(1), volume());

        let removed = mgr.retain_scans(|site, stamp| site == "KOUN" && *stamp == ts(1));

        assert_eq!(removed.len(), 2);
        assert!(
            !mgr.has_cached_site("KTLX"),
            "the emptied site's inner map was left behind",
        );
        assert!(
            mgr.has_cached_site("KOUN"),
            "a site that still holds an entry was pruned",
        );
        assert_eq!(mgr.cached_scan_count("KOUN"), 1);
    }

    /// The in-flight marks are not the sweep's to touch.
    #[test]
    fn retain_scans_leaves_the_in_flight_marks_alone() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), volume());
        mgr.mark_in_flight("KTLX", ts(5));
        mgr.mark_in_flight("KOUN", ts(5));
        mgr.add_spawned(2);

        let removed = mgr.retain_scans(|_, _| false);

        assert_eq!(removed.len(), 1, "precondition: the sweep did evict");
        assert!(
            mgr.is_in_flight("KTLX", &ts(5)),
            "a download already on the wire lost its mark, so the same file is \
             requested a second time",
        );
        assert!(mgr.is_in_flight("KOUN", &ts(5)));
        assert_eq!(
            mgr.available_slots(4),
            2,
            "the concurrency cap moved, so it no longer counts what is running",
        );
    }

    /// A plan naming `minutes`, as `accept_scan_listing` builds one.
    fn plan_for(site: &str, minutes: &[u32]) -> FramePlan {
        FramePlan::new(
            site.to_string(),
            minutes.iter().map(|&minute| ts(minute)).collect(),
        )
    }

    /// `retain_plan_frames` drops the plan entries the cache predicate would
    /// evict — which is what keeps the download filter and the sweep agreeing.
    #[test]
    fn retain_plan_frames_drops_what_the_cache_predicate_would_evict() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan_for("KTLX", &[0, 2, 4]));

        mgr.retain_plan_frames(|site, stamp| site == "KTLX" && *stamp >= ts(4));

        assert_eq!(
            mgr.plan_frame_count(0),
            1,
            "the plan still names frames nothing will draw, so the next \
             re-derivation queues their downloads",
        );
        // And the re-derivation really does read the pruned plan.
        assert!(mgr.plan_downloads_for(0, RadarProduct::Reflectivity));
        assert_eq!(mgr.pending_queue_count(0), 1);
    }

    /// The site half is judged too, and one pane's plan is not pruned by
    /// another site's predicate.
    #[test]
    fn retain_plan_frames_judges_each_plans_own_site() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan_for("KTLX", &[0, 2]));
        mgr.set_plan(1, plan_for("KOUN", &[0, 2]));

        mgr.retain_plan_frames(|site, _| site == "KOUN");

        assert_eq!(
            mgr.plan_frame_count(0),
            0,
            "KTLX's plan survived a predicate that names only KOUN",
        );
        assert_eq!(
            mgr.plan_frame_count(1),
            2,
            "KOUN's plan was pruned by KTLX's answer",
        );
    }

    /// An undispatched queue is swept by the same predicate as the plan it came
    /// from, so a queue derived before the window moved cannot outlive it.
    #[test]
    fn retain_plan_frames_sweeps_the_undispatched_queue_too() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan_for("KTLX", &[0, 2, 4]));
        assert!(mgr.plan_downloads_for(0, RadarProduct::Reflectivity));
        assert_eq!(
            mgr.pending_queue_count(0),
            3,
            "precondition: the queue was derived before the window moved",
        );

        mgr.retain_plan_frames(|_, stamp| *stamp >= ts(4));

        assert_eq!(
            mgr.pending_queue_count(0),
            1,
            "an already-derived queue kept entries the sweep will evict, so \
             they are dispatched and thrown away",
        );
    }

    /// A Level III loop's pairings are **not** the volume predicate's business.
    #[test]
    fn retain_plan_frames_leaves_level3_pairings_alone() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan_for("KTLX", &[0, 2, 4]));
        assert!(mgr.plan_downloads_for(0, RadarProduct::EchoTops));
        let queued = mgr
            .extract_pending_l3(0)
            .expect("a Level III product queues pairings rather than volumes");
        let before = queued.queue.len();
        assert!(
            before > 0,
            "precondition: there are pairings to leave alone"
        );
        mgr.insert_pending_l3(0, queued);

        mgr.retain_plan_frames(|_, _| false);

        let after = mgr
            .extract_pending_l3(0)
            .expect("the pairings are still there")
            .queue
            .len();
        assert_eq!(
            after, before,
            "a volume-cache predicate pruned Level III pairings, which read a \
             cache it does not sweep",
        );
    }

    /// An object, or the answer that there is none.
    fn l3() -> Arc<Level3Product> {
        Arc::new(Level3Product {
            message: nexrad_level3::model::Level3Message {
                header: nexrad_level3::model::MessageHeader {
                    message_code: 134,
                    date_of_message: 20661,
                    time_of_message: 7108,
                    message_length: 0,
                    source_id: 0,
                    destination_id: 0,
                    number_of_blocks: 3,
                },
                pdb: nexrad_level3::model::ProductDescriptionBlock {
                    block_divider: -1,
                    latitude: 35.333,
                    longitude: -97.278,
                    height: 1200,
                    product_code: 134,
                    operational_mode: 2,
                    vcp: 212,
                    sequence_number: 0,
                    volume_scan_number: 39,
                    volume_scan_date: 20661,
                    volume_scan_time: 7108,
                    generation_date: 20661,
                    generation_time: 7108,
                    product_specific_1: 0,
                    product_specific_2: 0,
                    elevation_number: 0,
                    product_specific_3: 0,
                    thresholds: [0u16; 16],
                    product_specific_47_53: [0i16; 7],
                    version: 0,
                    spot_blank: 0,
                    symbology_offset: 60,
                    graphic_offset: 0,
                    tabular_offset: 0,
                },
                symbology: None,
            },
            stamp: crate::level3::ProductStamp::from_key("TLX_DVL_2024_01_01_00_00_30"),
            bytes: Arc::new(Vec::new()),
        })
    }

    #[test]
    fn one_pairing_serves_every_product_that_reads_the_code() {
        let mut mgr = LoopDownloadManager::new();

        // The listing is claimed once for the site and code, so the pane that
        // asks second inherits it rather than listing the days again.
        assert!(mgr.claim_l3_listing("KTLX", "DVL"));
        assert!(
            !mgr.claim_l3_listing("KTLX", "DVL"),
            "a second reader of DVL must not list the same days again",
        );
        assert!(
            mgr.claim_l3_listing("KTLX", "EET"),
            "a different code is a different listing",
        );

        // A pairing in flight for DVL suppresses every other reader's.
        mgr.mark_l3_in_flight("KTLX", "DVL", ts(0));
        assert!(mgr.l3_is_in_flight("KTLX", "DVL", &ts(0)));
        assert!(!mgr.l3_is_resolved("KTLX", "DVL", &ts(0)));

        // And once it lands, both readers see it settled from the one entry.
        mgr.cache_l3_product("KTLX", "DVL", ts(0), Some(l3()));
        assert!(!mgr.l3_is_in_flight("KTLX", "DVL", &ts(0)));
        assert!(mgr.l3_is_resolved("KTLX", "DVL", &ts(0)));

        // VIL's frame is ready off that object alone; VIL density's still waits
        // for its denominator, and is ready only once EET lands too.
        assert_eq!(
            mgr.l3_frame_state("KTLX", RadarProduct::VerticallyIntegratedLiquid, &ts(0)),
            L3FrameState::Ready,
        );
        assert_eq!(
            mgr.l3_frame_state("KTLX", RadarProduct::VilDensity, &ts(0)),
            L3FrameState::Pending,
            "the denominator has not been paired",
        );
        mgr.cache_l3_product("KTLX", "EET", ts(0), Some(l3()));
        assert_eq!(
            mgr.l3_frame_state("KTLX", RadarProduct::VilDensity, &ts(0)),
            L3FrameState::Ready,
        );

        let vil = mgr
            .l3_frame_products("KTLX", RadarProduct::VerticallyIntegratedLiquid, &ts(0))
            .expect("VIL's frame is ready");
        let vild = mgr
            .l3_frame_products("KTLX", RadarProduct::VilDensity, &ts(0))
            .expect("VIL density's frame is ready");
        assert_eq!(vil.len(), 1);
        assert_eq!(vild.len(), 2, "numerator then denominator");
        assert!(
            Arc::ptr_eq(&vil[0], &vild[0]),
            "the two loops rendered different DVL objects, so the volume was \
             paired twice",
        );

        // Nothing here was ever keyed by product: another volume is still
        // unanswered for both.
        assert!(!mgr.l3_is_resolved("KTLX", "DVL", &ts(1)));
        assert!(!mgr.l3_is_resolved("KOUN", "DVL", &ts(0)));
    }

    /// `retain_l3` hands the evicted objects back rather than freeing them, and
    /// a cached gap goes with them without pretending to be one.
    #[test]
    fn retain_l3_returns_the_products_it_removed() {
        let mut mgr = LoopDownloadManager::new();
        let doomed = l3();
        let kept = l3();
        mgr.cache_l3_product("KTLX", "EET", ts(0), Some(doomed.clone()));
        mgr.cache_l3_product("KTLX", "EET", ts(1), Some(kept.clone()));
        // A gap at the doomed volume: its *key* is what the dispatch gate
        // reads, so it has to go, and there is nothing in it to hand over.
        mgr.cache_l3_product("KTLX", "DVL", ts(0), None);
        assert_eq!(
            mgr.cached_l3_count("KTLX"),
            3,
            "precondition: the cache holds something for the sweep to remove",
        );

        let removed = mgr.retain_l3(|_, stamp| *stamp == ts(1));

        assert_eq!(removed.len(), 1, "one object failed the predicate");
        assert!(
            Arc::ptr_eq(&removed[0], &doomed),
            "the value handed back is not the one that was evicted, so the \
             caller cannot hand the evicted object over",
        );
        assert!(
            !mgr.l3_is_resolved("KTLX", "DVL", &ts(0)),
            "the gap's key outlived the sweep, so the pairing gate goes on \
             answering \"already resolved\" for a volume nothing holds",
        );
        assert!(
            mgr.l3_is_resolved("KTLX", "EET", &ts(1)),
            "the surviving entry was removed",
        );
        assert_eq!(mgr.cached_l3_count("KTLX"), 1);
    }

    #[test]
    fn retain_l3_ignores_the_awips_code() {
        let mut mgr = LoopDownloadManager::new();
        for code in ["DVL", "EET"] {
            mgr.cache_l3_product("KTLX", code, ts(0), Some(l3()));
            mgr.cache_l3_product("KTLX", code, ts(9), Some(l3()));
        }
        assert_eq!(
            mgr.cached_l3_count("KTLX"),
            4,
            "precondition: two codes over two volumes",
        );

        // The window still names ts(0) and has retired ts(9). Which product the
        // pane is on does not enter: the predicate has nowhere to put it.
        let removed = mgr.retain_l3(|_, stamp| *stamp == ts(0));

        assert_eq!(removed.len(), 2, "both codes of the retired volume went");
        for code in ["DVL", "EET"] {
            assert!(
                mgr.l3_is_resolved("KTLX", code, &ts(0)),
                "{code}: an object of a frame still in the window was evicted, \
                 so a switch to a product reading it re-pairs a volume the loop \
                 never stopped naming",
            );
            assert!(
                !mgr.l3_is_resolved("KTLX", code, &ts(9)),
                "{code}: the object of a retired frame survived, which is the \
                 leak this sweep exists to close",
            );
        }
    }

    /// Both halves of what the rule *does* judge reach it.
    #[test]
    fn retain_l3_judges_the_site_as_well_as_the_volume() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_l3_product("KTLX", "EET", ts(0), Some(l3()));
        mgr.cache_l3_product("KOUN", "EET", ts(0), Some(l3()));

        let removed = mgr.retain_l3(|site, _| site == "KTLX");

        assert_eq!(removed.len(), 1);
        assert!(mgr.l3_is_resolved("KTLX", "EET", &ts(0)));
        assert!(
            !mgr.l3_is_resolved("KOUN", "EET", &ts(0)),
            "KOUN's object survived a predicate that named only KTLX",
        );
    }

    /// The undispatched pairings are swept by the same predicate as the cache
    /// they resolve through — the Level III half of the invariant
    /// `retain_plan_frames` states.
    #[test]
    fn retain_l3_sweeps_the_undispatched_pairings_too() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, plan_for("KTLX", &[0, 2, 4]));
        assert!(mgr.plan_downloads_for(0, RadarProduct::VilDensity));
        assert_eq!(
            mgr.pending_l3_queue_count(0),
            6,
            "precondition: three frames, two codes apiece, derived before the \
             window moved",
        );

        mgr.retain_l3(|_, stamp| *stamp >= ts(4));

        assert_eq!(
            mgr.pending_l3_queue_count(0),
            2,
            "an already-derived pairing queue kept the entries the sweep just \
             evicted, so each retired frame is paired again and thrown away by \
             the next sweep",
        );
    }

    #[test]
    fn retain_l3_keys_drops_a_site_nothing_needs_and_keeps_the_rest() {
        let mut mgr = LoopDownloadManager::new();
        for (site, code) in [("KTLX", "EET"), ("KTLX", "DVL"), ("KOUN", "EET")] {
            assert!(mgr.claim_l3_listing(site, code));
            mgr.cache_l3_keys(
                site,
                code,
                vec![format!("{site}_{code}_2024_01_01_00_00_30")],
            );
        }
        assert!(mgr.claim_l3_listing("KLZK", "EET"), "and one still listing");
        assert!(
            mgr.l3_keys("KTLX", "EET").is_some() && mgr.l3_keys("KOUN", "EET").is_some(),
            "precondition: three listings have landed",
        );

        let removed = mgr.retain_l3_keys(|site| site == "KOUN");

        assert_eq!(removed.len(), 2, "both of KTLX's codes went");
        assert!(
            mgr.l3_keys("KTLX", "EET").is_none() && mgr.l3_keys("KTLX", "DVL").is_none(),
            "a departed site's listings outlived it, and `claim_l3_listing` \
             will now refuse to re-list them for a window they do not cover",
        );
        assert!(
            mgr.l3_keys("KOUN", "EET").is_some(),
            "a site something still needs lost its listing",
        );
        assert!(
            !mgr.claim_l3_listing("KLZK", "EET"),
            "a listing already on the wire lost its mark, so the same days are \
             listed a second time",
        );
        // And a site the sweep emptied really can be listed again.
        assert!(mgr.claim_l3_listing("KTLX", "EET"));
    }

    /// The in-flight marks are not the sweep's to touch, for the reason
    /// `retain_scans_leaves_the_in_flight_marks_alone` gives on the other
    /// datasource: one shared concurrency counter, `saturating_sub`bed on
    /// completion, so moving it here wedges it low for the session.
    #[test]
    fn retain_l3_leaves_the_in_flight_marks_alone() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_l3_product("KTLX", "EET", ts(0), Some(l3()));
        mgr.mark_l3_in_flight("KTLX", "EET", ts(5));
        mgr.add_spawned(1);

        let removed = mgr.retain_l3(|_, _| false);

        assert_eq!(removed.len(), 1, "precondition: the sweep did evict");
        assert!(
            mgr.l3_is_in_flight("KTLX", "EET", &ts(5)),
            "a pairing already on the wire lost its mark, so the same object is \
             fetched a second time",
        );
        assert_eq!(
            mgr.available_slots(4),
            3,
            "the concurrency cap moved, so it no longer counts what is running",
        );
    }

    /// **`site_needs_decoded_source` answers for the site it was asked
    /// about, and for the product each loop on it renders.**
    ///
    /// Every arm matters to a caller: the Level II arm is what keeps a
    /// playing loop's volumes, the Level III arm is the whole saving, the
    /// undispatched arm is the safe direction, and the "another site's loop"
    /// arm is what stops one site's Level II loop from paying for every other
    /// site in the cache.
    #[test]
    fn a_site_needs_its_decoded_source_only_where_a_level_ii_loop_reads_it() {
        use crate::types::RadarProduct;

        // A Level II product: its frames are derived from the decoded volume.
        assert!(
            site_needs_decoded_source("KTLX", &[("KTLX", Some(RadarProduct::Reflectivity))]),
            "a playing Level II loop stopped keeping its own volumes, which              re-downloads the whole window on every sweep",
        );
        // A Level III product: the frames are objects, and the volumes are
        // dead weight — 47.99 MiB of it per frame, measured.
        assert!(
            !site_needs_decoded_source("KTLX", &[("KTLX", Some(RadarProduct::PrecipitationRate))]),
            "a Level III loop still claims the decoded volumes nothing on this              site derives from",
        );
        // Not yet dispatched: nothing has said what it renders.
        assert!(
            site_needs_decoded_source("KTLX", &[("KTLX", None)]),
            "a loop before its first dispatch was read as needing nothing, so              its window is evicted one frame before it is asked for",
        );
        // Another site's Level II loop says nothing about this one.
        assert!(
            !site_needs_decoded_source("KTLX", &[("KOUN", Some(RadarProduct::Reflectivity))]),
            "one site's Level II loop kept a different site's volumes",
        );
        // Two loops on one site share one cache: the Level II one wins.
        assert!(
            site_needs_decoded_source(
                "KTLX",
                &[
                    ("KTLX", Some(RadarProduct::PrecipitationRate)),
                    ("KTLX", Some(RadarProduct::Velocity)),
                ]
            ),
            "a second pane's Level II loop on the same site lost its volumes to              the first pane's Level III one; the cache is shared, so this is a              black frame on a playing loop",
        );
        // No loop at all: nothing that loops needs it.
        assert!(
            !site_needs_decoded_source("KTLX", &[]),
            "a site with no loop running claimed its volumes anyway",
        );
    }
}
