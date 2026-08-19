use crate::level3::Level3Product;
use crate::types::RadarProduct;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// Which Level III object a loop frame wants: the site whose bucket keys it comes
/// from, the AWIPS code, and the **volume start** the frame names.
///
/// The volume start, not the key's own timestamp. A key names when the RPG
/// *published* an object; a frame names the volume it draws, and the two differ by
/// however long generation took — plus, under SAILS, by however many intermediate
/// republications of the same volume there were. Keying the cache on the volume is
/// what makes "this frame's object" a question with one answer.
pub type L3FrameKey = (String, String, chrono::NaiveDateTime);

/// What a loop frame has to render, once its data has arrived.
///
/// The two arms are the two datasources, and they are the *only* place the
/// distinction is drawn in the loop: everything downstream — the render budget, the
/// target key, the sibling broadcast, the readiness rules — treats a frame the same
/// either way.
pub enum LoopFrameData {
    /// A decoded Level II volume and what its cuts declared their Nyquist
    /// velocities to be; the renderer picks its sweep out of the first and
    /// folds that sweep's velocity around the second.
    ///
    /// The table is not derivable from the `Scan` — `nexrad_model`'s radial
    /// has no field for it ([`crate::nyquist`]) — so it travels, or
    /// the frame dealiases on an estimate while the still frame beside it
    /// dealiases on the RDA's own number.
    Volume(
        Arc<nexrad_model::data::Scan>,
        Arc<crate::nyquist::DeclaredNyquist>,
    ),
    /// The Level III objects of this frame's volume, one per AWIPS code in
    /// [`RadarProduct::level3_products`] order.
    ///
    /// A `Vec` rather than one object because a product may be derived from
    /// several: VIL density is `DVL ÷ EET`, two codes paired to one volume. The
    /// pairing, the caching and the "is this frame ready" rule are all already
    /// per-code, so such a product needs no new plumbing here — only a render job
    /// that reads more than the first entry.
    Products(Vec<Arc<Level3Product>>),
}

/// One downloaded volume as the loop caches it: the sweeps, and what their cuts
/// declared their Nyquist velocities to be.
///
/// Named rather than spelt out at the cache, its two accessors and every caller
/// that holds one: two `Arc`s of unrelated types are exactly the pair that gets
/// transposed with no type error to catch it.
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
///
/// Scans and download marks are keyed by `(site, timestamp)`, never by timestamp
/// alone. Panes run independent loops on independent sites, and two sites' volume
/// times land on the same second often enough — a timestamp-only key let one site's
/// scan overwrite another's, and the loop that then looked it up rendered another
/// radar's data around its own coordinates. Nothing downstream can catch that: the
/// render target key is derived from the loop, so the result looks entirely
/// consistent. The site has to be in the key.
///
/// The Level III half follows the same rule for the same reason, with the AWIPS
/// code alongside: see [`L3FrameKey`].
pub struct LoopDownloadManager {
    /// Downloaded scan data cache for loop frames, keyed by site then timestamp
    /// (shared across every pane looping that site).
    ///
    /// One entry is `(volume, what its cuts declared)` — see
    /// [`LoopFrameData::Volume`] for why the second half cannot be recovered
    /// from the first.
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
    ///
    /// Listing is a round-trip per UTC day, and a loop pairs tens of volumes
    /// against the same code; re-listing per frame would spend tens of requests
    /// to answer one question. An empty list is a real answer — the site served
    /// nothing — and is cached as such, which is what lets every frame resolve to
    /// a gap and the loop retire cleanly instead of waiting forever.
    l3_keys: HashMap<(String, String), Arc<Vec<String>>>,
    /// `(site, code)` listings under way, so two panes looping one site do not
    /// both list it.
    l3_keys_in_flight: HashSet<(String, String)>,
    /// The object paired to each frame's volume, or `None` where the site
    /// generated none.
    ///
    /// `None` is cached deliberately: a gap that was not remembered would be
    /// re-paired — up to `PAIRING_CANDIDATES` object fetches — on every dispatch
    /// pass for the life of the loop.
    l3_cache: HashMap<L3FrameKey, Option<Arc<Level3Product>>>,
    /// Pairings under way, the Level III counterpart of
    /// [`in_flight_set`](Self::in_flight_set).
    l3_in_flight: HashSet<L3FrameKey>,
    /// Number of loop downloads currently in flight (global, not per-pane, and
    /// shared by the Level II and Level III paths so the network concurrency cap
    /// means one thing).
    in_flight_count: usize,
}

/// A pane's undispatched loop downloads, with the site they belong to.
///
/// The site travels *with* the queue rather than being read back off the pane when
/// a download is dispatched. A scan listing is requested asynchronously and cannot
/// be cancelled, so a listing for the site a pane's loop used to be on can land
/// after the loop has been rebuilt for another one. Re-deriving the site at
/// dispatch time labelled those files with whatever site the pane had reached,
/// cached one radar's scan under another's key, and — because the download filter
/// then treats that key as satisfied — discarded the real scans that would have
/// corrected it. Only a site switch, which then emptied the manager, recovered
/// from that.
pub struct PendingDownloads {
    /// The site the listing was made for. Every identifier in `queue` is one of
    /// this site's files, and the scan each becomes is cached under it.
    pub site: String,
    /// Scans still to download, oldest-first.
    pub queue: VecDeque<(chrono::NaiveDateTime, crate::archive::Identifier)>,
}

/// A pane's undispatched Level III pairings, with the site they belong to.
///
/// The site travels with the queue for exactly the reason it does on
/// [`PendingDownloads`]: a pairing is an uncancellable network round-trip, and the
/// pane's loop can be rebuilt for another site while it runs. The object it
/// produces is cached under the site named here, never under whatever site the
/// pane has reached by the time it lands.
pub struct PendingL3Pairings {
    /// The site whose bucket keys every entry below is paired against.
    pub site: String,
    /// The product these pairings are for.
    ///
    /// The product rather than a bare list of codes, because it answers both
    /// halves of a pairing — which AWIPS codes to list, and which object of a
    /// matched volume to take ([`RadarProduct::level3_volume_pick`]) — so the two
    /// cannot come from different places and disagree.
    pub product: RadarProduct,
    /// `(volume start, AWIPS code)` still to pair, oldest volume first.
    pub queue: VecDeque<(chrono::NaiveDateTime, String)>,
}

/// Every volume a pane's loop frames name, kept so the download queues can be
/// re-derived without re-listing the archive.
///
/// A loop's frame list is built from one Level II archive listing and does not
/// change as the user switches product. What *does* change is which bytes each
/// frame needs: a Level II product wants the ~10 MB volume, a Level III product
/// wants a few hundred kilobytes of bucket object and no volume at all. Keeping
/// the plan means switching between them costs no listing — and means a Level III
/// loop never downloads the volumes it would not read.
pub struct FramePlan {
    /// The site the listing was made for; every identifier below is one of its
    /// files, and every pairing derived from this plan is against its keys.
    pub site: String,
    /// Volume start and archive file per frame, oldest-first.
    pub frames: Vec<(chrono::NaiveDateTime, crate::archive::Identifier)>,
    /// The product the queues were last derived for. Compared, not assumed:
    /// re-deriving on every dispatch pass would rebuild both queues every frame
    /// of the UI, and re-deriving never would leave a retargeted pane waiting on
    /// data nothing was fetching.
    planned_for: Option<RadarProduct>,
}

impl FramePlan {
    /// A plan for a fresh listing, with nothing derived from it yet.
    ///
    /// The site and the frames are taken together for the reason they travel
    /// together everywhere else in the loop: every identifier is one of that
    /// site's files, and everything derived from the plan — a volume download, a
    /// bucket pairing — is filed under it. Built by whoever accepted the listing,
    /// so the site cannot be re-read from a pane whose loop has moved on.
    pub fn new(
        site: String,
        frames: Vec<(chrono::NaiveDateTime, crate::archive::Identifier)>,
    ) -> Self {
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
    /// keeps derivation-memo entries for (WO-E4.8). A 3D loop derives NROT or
    /// SRV per frame it revisits; pruning the memo against the plan-view
    /// stores alone would drop and re-run those derivations once a frame.
    pub fn cached_scans(&self) -> impl Iterator<Item = &nexrad_model::data::Scan> {
        self.scan_cache
            .values()
            .flat_map(|scans| scans.values().map(|(scan, _)| scan.as_ref()))
    }

    // ------------------------------------------------------------------
    // Test probes. `#[cfg(test)]` until the WO-RF2n fold; unconditional
    // since, because their consumers — the ten loop-pin suites — live
    // app-side in `rustdar-app`, across a crate boundary `cfg(test)`
    // cannot reach. Read-only counts and containment checks, nothing more.
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
    ///
    /// A cached `None` occupies a key and answers a dispatch gate exactly as a
    /// present object does, so a count that skipped them would report a cache
    /// the sweep had not touched as empty.
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
    ///
    /// Separate from [`cached_scan_count`](Self::cached_scan_count) because that
    /// answers `0` for a site whose inner map is empty *and* for one that was
    /// pruned, and the pruning is the thing under test.
    pub fn has_cached_site(&self, site: &str) -> bool {
        self.scan_cache.contains_key(site)
    }

    /// Store a downloaded volume in the cache under the site it was downloaded
    /// for, with what its cuts declared.
    pub fn cache_scan(&mut self, site: &str, ts: chrono::NaiveDateTime, volume: CachedVolume) {
        self.scan_cache
            .entry(site.to_string())
            .or_default()
            .insert(ts, volume);
    }

    /// Take out every cached volume whose `(site, timestamp)` fails `keep`, and
    /// hand the removed values back **owned**.
    ///
    /// # Why it returns them rather than dropping them
    ///
    /// The mirror of `RenderCache::retain` (`rustdar-app`'s
    /// `render_dispatch` module) in shape and its opposite in destination.
    /// `retain` frees in place, and in place is the frame thread: an entry here
    /// is a whole decoded volume, 47–69 MiB across thousands of per-radial
    /// buffers, and returning them is what lets the caller hand them to
    /// `rustdar_worker::offload::discard_each` instead.
    /// Same reasoning, and the same helper's reasoning, as `rustdar-app`'s
    /// `app::evicted`.
    ///
    /// The Level III cache is bounded by [`retain_l3`](Self::retain_l3), which
    /// mirrors this in shape and is handed the very same predicate.
    ///
    /// # Until this, nothing evicted an entry
    ///
    /// A site switch emptied the map wholesale — every site's entries, not just
    /// the departing pane's, which is why that call is gone too — and nothing
    /// else ever removed one, while
    /// [`cache_scan`](Self::cache_scan) is written on every auto-poll and every
    /// completed live volume. A pane parked on a live radar therefore
    /// accumulated one decoded volume per scan for the life of the process —
    /// 0.4–1 GB an hour, outside every byte budget in the workspace, because
    /// the loop pool's budget counts *texture* bytes and these are CPU-side.
    ///
    /// # The predicate is `(site, timestamp)`, both halves
    ///
    /// For the reason the cache is keyed that way (see this type's own doc):
    /// two sites' volume times collide often enough, and a rule that answered
    /// on the timestamp alone would evict one radar's entry because another
    /// radar had stopped naming that second.
    ///
    /// An emptied site's inner map goes with its last entry. Left behind it is
    /// a `String` key per radar a session ever looped, which is small — and is
    /// also the difference between "this site holds nothing" and "this site is
    /// not in the map", a distinction no caller should have to know does not
    /// matter.
    ///
    /// # It does not touch the in-flight marks, deliberately
    ///
    /// [`in_flight_set`](Self::in_flight_set) and
    /// [`in_flight_count`](Self::in_flight_count) mirror network operations
    /// that are already under way and cannot be recalled. Clearing a mark here
    /// would let the same file be requested twice; decrementing the count would
    /// raise the concurrency cap above what is actually running, and the count
    /// is `saturating_sub`bed on completion, so it would wedge low and starve
    /// dispatch for the rest of the session. A download in flight for an entry
    /// this pass evicted simply lands and is cached again — the loop that
    /// wanted it is the only thing that would have asked.
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
        removed
    }

    /// Drop from every frame plan, and from every undispatched volume queue,
    /// the entries whose `(site, timestamp)` fails `keep`.
    ///
    /// # Called with the *same* predicate as [`retain_scans`](Self::retain_scans)
    ///
    /// That is the whole point, and the invariant is worth stating as one
    /// sentence: **nothing the sweep would evict stays queued.** The download
    /// filter in `dispatch_pending_loop_downloads` skips a queued timestamp when
    /// `is_cached` says the volume is already in hand, so the queue and the
    /// cache have to agree about which timestamps still matter. Sweeping one
    /// without the other is what turns a bounded cache into a download loop.
    ///
    /// # The plan is where the churn would repeat
    ///
    /// [`FramePlan::frames`] is the *original listing*, and `append_polled_frame`
    /// never prunes it as the window walks forward — it prunes
    /// `LoopPlaybackState::frames`, which is a different list. While the cache
    /// was unbounded that divergence was invisible: a retired frame's volume
    /// stayed resident for ever, so `is_cached` filtered its queue entry and
    /// nothing re-downloaded it.
    ///
    /// With the cache swept it is no longer invisible. Watch a live loop until
    /// its window has fully turned over, then switch product: the retarget
    /// re-asks [`plan_downloads_for`](Self::plan_downloads_for), which re-derives
    /// the queue from the stale plan, and up to `MAX_LOOP_FRAMES` volumes of
    /// ~10 MB are downloaded, cached, and evicted by the very next sweep — while
    /// holding the shared `concurrent_loop_downloads` slots the live frames are
    /// waiting on. It repeats on every product switch. That is precisely the
    /// refetch churn the retention design refuses a byte-LRU for, arriving
    /// through the one reader an enumeration of the *cache's* readers does not
    /// list, because it reads the queue rather than the cache.
    ///
    /// # What it deliberately leaves alone
    ///
    /// [`pending_l3`](Self::pending_l3) is not pruned here. A Level III loop
    /// downloads no volumes at all — its frames resolve through
    /// [`l3_cache`](Self::l3_cache), which this call does not touch — so judging
    /// its pairings by a volume-cache predicate would be a category error, not a
    /// missing case. They are swept by [`retain_l3`](Self::retain_l3) instead,
    /// against the cache they *do* resolve through and with the same predicate
    /// object, so the invariant above holds on both datasources.
    ///
    /// The in-flight marks are untouched for the reason [`retain_scans`] gives.
    pub fn retain_plan_frames(&mut self, keep: impl Fn(&str, &chrono::NaiveDateTime) -> bool) {
        for plan in self.plans.values_mut() {
            plan.frames.retain(|(ts, _)| keep(plan.site.as_str(), ts));
        }
        for pending in self.pending_downloads.values_mut() {
            pending
                .queue
                .retain(|(ts, _)| keep(pending.site.as_str(), ts));
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
    ///
    /// "Remove pending" has to mean *all* of it. A pane switching its loop off, or
    /// having it rebuilt, owes nothing on either datasource; leaving the Level III
    /// queue behind would keep pairing objects for a loop that no longer exists,
    /// and leaving the plan behind would let the next `plan_downloads_for` refill
    /// from a listing the new loop never asked for.
    pub fn remove_pending(&mut self, pane: usize) {
        self.pending_downloads.remove(&pane);
        self.pending_l3.remove(&pane);
        self.plans.remove(&pane);
    }

    // ── Level III frames ──────────────────────────────────────────────────

    /// Record what volumes a pane's loop frames name, replacing any previous
    /// plan and the queues derived from it.
    ///
    /// Nothing is queued yet: what the frames need depends on the pane's product,
    /// which [`plan_downloads_for`](Self::plan_downloads_for) answers.
    pub fn set_plan(&mut self, pane: usize, plan: FramePlan) {
        self.pending_downloads.remove(&pane);
        self.pending_l3.remove(&pane);
        self.plans.insert(pane, plan);
    }

    /// Derive this pane's download queues for `product`, returning whether
    /// anything changed.
    ///
    /// This is the one place the two datasources part company in the download
    /// path, and it is a *data-path* branch: a Level II frame needs its archive
    /// volume, a Level III frame needs the bucket objects of the same volume and
    /// not the volume itself. Both produce a queue, both drain through the same
    /// concurrency budget, and both settle a frame the same way.
    ///
    /// Only the queue for the datasource in use is populated. A Level III loop
    /// that also downloaded its volumes would spend ~10 MB a frame on bytes no
    /// render reads.
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
                    .flat_map(|(ts, _)| codes.iter().map(move |code| (*ts, (*code).to_string())))
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
                let queue = plan.frames.iter().cloned().collect();
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
    ///
    /// `false` means it is already listed or already being listed. Two panes
    /// looping the same site want the same keys, and a listing is the expensive
    /// half of a pairing.
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
    ///
    /// # Why the question is coarser than the other two sweeps'
    ///
    /// Because the key is. A listing is `(site, AWIPS code)` with **no volume in
    /// it** — the days' worth of keys a site's objects are *ranked* against, not
    /// one frame's answer — so "does any frame still name this volume" is a
    /// question it cannot be asked. What it can be asked is whether anything
    /// still needs the site at all, and the caller derives that from the very
    /// same two sets the volume and object predicates are built from, so the
    /// three cannot come to disagree about which sites are live.
    ///
    /// # Until this, a site switch was the only thing that removed one
    ///
    /// `clear_all` cleared this map when a pane left a radar, which is exactly
    /// the call that had to go: it emptied every *other* site's state along with
    /// the departing one's. Nothing else ever removed an entry, so without this
    /// a session would keep one listing per `(site, code)` it ever looped for
    /// its whole life — a few hundred keys apiece, small beside the volumes and
    /// the objects, and unbounded on the one axis a session can walk.
    ///
    /// It is also the entry with the longest reach when it is wrong.
    /// [`claim_l3_listing`](Self::claim_l3_listing) refuses a second listing for
    /// a `(site, code)` this map already holds, and the days a listing covers
    /// come from the frames that asked for it — so a listing kept past the loop
    /// that made it is re-used for a window it does not cover, and every frame
    /// outside it resolves to a gap that is indistinguishable from the site
    /// having served nothing.
    ///
    /// # It does not touch [`l3_keys_in_flight`](Self::l3_keys_in_flight)
    ///
    /// That is an in-flight mark and is left alone for the reason
    /// [`retain_scans`](Self::retain_scans) gives about the others: clearing it
    /// would let the same days be listed twice. A listing already on the wire
    /// lands and is cached, and this sweep collects it on a later pass if
    /// nothing wants it by then.
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
        self.l3_cache.insert(key, product);
    }

    /// Take out every paired Level III object whose `(site, volume start)` fails
    /// `keep` and hand the removed products back **owned**, then drop the
    /// undispatched pairings the same predicate refuses.
    ///
    /// # The Level III half of the same leak
    ///
    /// [`l3_cache`](Self::l3_cache) has the shape
    /// [`scan_cache`](Self::scan_cache) had before
    /// [`retain_scans`](Self::retain_scans): one entry per frame per AWIPS code,
    /// written by [`cache_l3_product`](Self::cache_l3_product), and removed by
    /// nothing at all — a site switch emptied the map wholesale, and no other
    /// path ever took an entry out. A value is a [`Level3Product`], which
    /// carries the decoded `message` **and** the `bytes` it was decoded from —
    /// kept because a `Level3Message` has no wire form and the browser's
    /// rasterization worker re-decodes the bytes itself — so every re-listing
    /// (a product switch, a time navigation, `reinit_active_loops`) leaves its
    /// whole previous window resident, per code, for the life of the process. A
    /// few hundred kilobytes an entry rather than a volume's ~10 MB, which is
    /// why it is the sibling leak and not the first one found; it is unbounded
    /// on the same axis.
    ///
    /// Dropping `message` and re-decoding from `bytes` on demand would halve an
    /// entry and is a different question from *which* entries are held. It is
    /// not asked here.
    ///
    /// # The predicate is `(site, volume start)`, and the AWIPS code is deliberately not asked about
    ///
    /// The keys carry three parts and the rule judges two. That is not an
    /// oversight in any of four respects:
    ///
    /// * **The code axis is a compile-time constant and the volume axis is
    ///   not.** [`RadarProduct::level3_products`] is a fixed table naming four
    ///   distinct codes across the whole workspace, so ignoring the code costs
    ///   at most four entries per retained volume. What grows without bound is
    ///   the volume, which is exactly what this sweep bounds.
    /// * **A product switch does not move the frames, so it must not move the
    ///   retention set.** Both frame lists — [`FramePlan::frames`] and
    ///   `LoopPlaybackState::frames` — come from a Level II archive listing, and
    ///   `retarget_renders_keyed` re-renders without re-listing. Judged on the
    ///   code, every switch would evict the objects of frames still in the
    ///   window and the switch back would re-pair them, at up to
    ///   `PAIRING_CANDIDATES` object fetches per frame per code. That is the
    ///   refetch churn the volume design refuses a byte-LRU for, arriving on the
    ///   other datasource.
    /// * **These entries are shared between products by construction.** No key
    ///   here mentions a product, which is what lets one pane looping VIL and
    ///   another looping VIL density pair each volume's `DVL` exactly once —
    ///   pinned by `one_pairing_serves_every_product_that_reads_the_code`. A
    ///   retention set derived from a loop's *current* product would evict the
    ///   other loop's objects, and there is no per-pane cache to fall back on.
    /// * **The gaps are the cheapest entries and the most expensive to lose.** A
    ///   `None` is cached as the answer "this site generated no object for this
    ///   volume", so a frame is retired once instead of being re-paired on every
    ///   dispatch pass for the life of the loop. It is removed here with the
    ///   rest — its key is what the gate reads — and contributes no payload to
    ///   hand over, because there is nothing in it.
    ///
    /// # The undispatched pairings go with the cache, by the same predicate
    ///
    /// The invariant [`retain_plan_frames`](Self::retain_plan_frames) states —
    /// **nothing the sweep would evict stays queued** — on this datasource.
    /// `dispatch_pending_loop_l3_pairings` drops a queue entry when
    /// [`l3_is_resolved`](Self::l3_is_resolved) says it is already answered,
    /// which is the Level III counterpart of the `is_cached` filter, so sweeping
    /// the cache without the queue turns every retired frame back into a live
    /// pairing — and a pairing is up to `PAIRING_CANDIDATES` object fetches
    /// holding the shared `concurrent_loop_downloads` slots the live frames wait
    /// on.
    ///
    /// This is why `retain_plan_frames` leaves [`pending_l3`](Self::pending_l3)
    /// alone rather than sweeping it there: nothing has changed about that call
    /// being unable to answer for this cache, only that this one now can.
    ///
    /// The **plans** are not swept twice. `FramePlan` is one frame list per pane
    /// whichever datasource the pane's product reads, and `retain_plan_frames`
    /// already sweeps it with this very predicate.
    ///
    /// # It does not touch the in-flight marks
    ///
    /// [`l3_in_flight`](Self::l3_in_flight) mirrors pairings already on the wire,
    /// for the reason and with the consequence [`retain_scans`](Self::retain_scans)
    /// states: clearing a mark would let the same object be fetched twice, and
    /// `in_flight_count` is `saturating_sub`bed on completion, so moving it here
    /// would wedge the concurrency cap low for the session. A pairing in flight
    /// for an entry this pass evicted simply lands and is cached again.
    pub fn retain_l3(
        &mut self,
        keep: impl Fn(&str, &chrono::NaiveDateTime) -> bool,
    ) -> Vec<Arc<Level3Product>> {
        let removed: Vec<Arc<Level3Product>> = self
            .l3_cache
            .extract_if(|(site, _, ts), _| !keep(site.as_str(), ts))
            // A gap's key goes with the rest and its value is nothing to hand
            // over. Dropped here rather than unwrapped, because a cached `None`
            // is an ordinary answer and not a missing entry.
            .filter_map(|(_, product)| product)
            .collect();
        for pending in self.pending_l3.values_mut() {
            pending
                .queue
                .retain(|(ts, _)| keep(pending.site.as_str(), ts));
        }
        removed
    }

    /// Whether frame `ts` of `product`'s loop on `site` has every object it
    /// needs, is missing one for good, or is still waiting.
    ///
    /// Asked per AWIPS code, so a product derived from several — VIL density's
    /// `DVL ÷ EET` — is ready only when all of them are and is a gap as soon as
    /// any one of them is.
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
    ///
    /// All-or-nothing on purpose: a two-input product handed one input would
    /// render a ratio against a missing denominator.
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
    ///
    /// The one lookup both render paths go through, so "which datasource does
    /// this frame draw from" is answered in one place from the product alone.
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

    /// Whether frame `ts`'s data question has been *answered* — the volume is
    /// cached, or every Level III object has been paired, gaps included.
    ///
    /// This is what loop readiness asks. A gap counts as settled: the frame will
    /// never render, which is a decision, not a wait.
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
    ///
    /// Handing back the site with the queue is the point: a caller cannot dispatch
    /// this pane's downloads without also holding the site they were listed for.
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
    ///
    /// Both queues, because a Level III loop's frames are owed through the other
    /// one. Asking only about volumes would report a loop whose pairings have not
    /// started as "done", and `settle_loop_phase` would abandon it on the pass
    /// right after its frame list was built.
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
    use crate::archive::Identifier;
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

    /// **A pane leaving a radar takes only its own pending work.**
    ///
    /// The successor to `clear_all_empties_every_sites_state`, extended rather
    /// than deleted, and inverted where its premise was the defect. That pin was
    /// on `clear_all`, which `SwitchRadarSite` called whenever *any* pane left a
    /// radar and which emptied both shared caches, every pane's queues and every
    /// frame plan — so a second pane looping a different site silently lost its
    /// loop. Its queue half is kept here, aimed at `remove_pending`, which is
    /// what the switch calls now: no entry of the departing pane's is left
    /// behind to be dispatched for a radar it is no longer on. Its cache half is
    /// the complement it lacked, and the assertions below say so directly.
    ///
    /// The concurrency assertion is inverted rather than dropped, and that is
    /// not bookkeeping either: `clear_all` reset `in_flight_count` to zero while
    /// downloads were still on the wire, which raised the effective cap above
    /// what was running — exactly what `retain_scans` refuses to do and explains
    /// at length. Nothing moves that counter here.
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
                    queue: [(ts(2), Identifier::new(format!("{site}20240101_000200_V06")))]
                        .into_iter()
                        .collect(),
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
        // collect. A pane-scoped teardown that reached them would take another
        // site's loop with it, which is the defect this shape replaces.
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
    ///
    /// The whole reason it is not a `retain`: the caller is the frame thread,
    /// an entry is 47–69 MiB across thousands of per-radial buffers, and
    /// returning the values is what lets `App::evict_unneeded_loop_scans` pass
    /// them to `offload::discard_each`. Compared by pointer, so this cannot be
    /// satisfied by handing back some other volume of the right shape.
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
    ///
    /// A rule that saw only the timestamp would evict one radar's entry because
    /// another radar had stopped naming that second — the same collision the
    /// cache is keyed on the site to avoid.
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
    ///
    /// Otherwise a session's every looped radar leaves a `String` key behind —
    /// small, but also the difference between "this site holds nothing" and
    /// "this site is not in the map", which no caller should have to know does
    /// not matter.
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
    ///
    /// They mirror network operations already under way and uncancellable.
    /// Clearing a mark would let the same file be requested twice; the count is
    /// `saturating_sub`bed on completion, so decrementing it here would wedge it
    /// low and starve dispatch — the concurrency cap would report free slots
    /// that do not exist and then never recover them.
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
            minutes
                .iter()
                .map(|&minute| {
                    (
                        ts(minute),
                        Identifier::new(format!("{site}20240101_00{minute:02}00_V06")),
                    )
                })
                .collect(),
        )
    }

    /// `retain_plan_frames` drops the plan entries the cache predicate would
    /// evict — which is what keeps the download filter and the sweep agreeing.
    ///
    /// `FramePlan::frames` is the original listing and nothing prunes it as a
    /// live window walks forward, so without this a re-plan queues downloads
    /// for volumes the very next sweep throws away.
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
    ///
    /// Such a loop downloads no volumes at all — its frames resolve through
    /// `l3_cache`, which this call does not touch — so judging its pairings by a
    /// volume-cache answer would be a category error rather than a missing case.
    /// `retain_l3` sweeps them against that cache instead, with the same
    /// predicate object; the pin is on the *split*, not on the pairings being
    /// unswept.
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

    /// **The loop pairs each object once, however many products read it.**
    ///
    /// The counterpart of the static poll's de-duplication, and it comes free:
    /// every key here is `(site, code[, volume])` and never mentions a product, so
    /// one pane looping VIL and another looping VIL density over the same volumes
    /// pair that volume's `DVL` between them exactly once — a pairing being up to
    /// `PAIRING_CANDIDATES` object fetches, which is the expensive thing to do
    /// twice.
    ///
    /// Asserted through the three predicates `dispatch_pending_loop_l3_pairings`
    /// actually gates on — the listing claim, the resolved check and the in-flight
    /// check — because a product creeping into any one of those keys is what would
    /// reintroduce the duplicate.
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
        // for its denominator, and is ready only once EET lands too. Both read the
        // same DVL entry — `l3_frame_products` hands the very same `Arc` to each.
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
    ///
    /// The same reason `retain_scans` is not a `retain`: the caller is the frame
    /// thread and a value here carries a decoded `Level3Message` *and* the bytes
    /// it was decoded from. Compared by pointer, so it cannot be satisfied by
    /// handing back some other object of the right shape.
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

    /// **The AWIPS code is deliberately not in the question.**
    ///
    /// The key has three parts and the rule judges two, so a loop that switches
    /// product keeps the objects of every frame still in its window — including
    /// the codes the new product does not read, which is the point: the frames
    /// did not move, only the codes wanted did, and re-pairing them costs up to
    /// `PAIRING_CANDIDATES` object fetches apiece. The code axis is a
    /// compile-time table of four entries; the volume axis is the unbounded one.
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
    ///
    /// Two sites' volume times land on the same second often enough — the
    /// reason the key carries the site at all — and a rule that answered on the
    /// volume alone would evict one radar's object because another radar had
    /// stopped naming that second.
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
    ///
    /// `dispatch_pending_loop_l3_pairings` drops a queue entry that
    /// `l3_is_resolved` calls answered, so a queue left holding entries the
    /// cache no longer answers for re-pairs every one of them, at up to
    /// `PAIRING_CANDIDATES` fetches apiece, holding the shared download slots
    /// the live frames are waiting on.
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

    /// **The key listings are swept by site, and only by site.**
    ///
    /// A listing has no volume in its key — it is the days' worth of bucket keys
    /// a site's objects are ranked against — so the only question it can be
    /// asked is whether anything still needs the site. Nothing removed one
    /// before: the site switch's wholesale clear did it, and that call is what
    /// had to go.
    ///
    /// `l3_keys_in_flight` is left alone, so a listing already on the wire is
    /// not requested a second time.
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
}
