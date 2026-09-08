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
    /// **Which surface this pane's loop frames can be drawn on.**
    ///
    /// Carried from the dispatcher rather than decided here, for the reason
    /// [`crate::jobs::PlanSurface`] gives: whether a fan can be drawn at all is
    /// a property of the machine the pane is on, and this crate cannot see it.
    pub surface: crate::jobs::PlanSurface,
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
    ///
    /// **Presence here means "renderable now", and that is what
    /// [`Self::is_cached`] answers.** A frame whose decoded half has been
    /// evicted keeps its [`archive_cache`](Self::archive_cache) entry and is
    /// absent from this map, so every existing reader of `is_cached` stays
    /// correct without knowing the archive half exists.
    scan_cache: HashMap<String, HashMap<chrono::NaiveDateTime, CachedVolume>>,
    /// **The compressed archive each loop-downloaded frame was decoded
    /// from**, keyed exactly as [`scan_cache`](Self::scan_cache) is.
    ///
    /// Held so that re-obtaining a volume the decoded cache has evicted is a
    /// DECODE rather than a network round trip. Measured over 39 archive
    /// volumes: a decoded volume is 33.7-82.7 MiB and the archive it came
    /// from is 1.0-16.1 MiB, a median ratio of 17.1x, so holding the
    /// compressed form instead costs a median 5.8 % of the decoded one.
    ///
    /// **Two paths file here, and the chunk feed is not one of them.** The
    /// loop's own downloads file the archive they fetched; the archive drain's
    /// three arrivals — the pane fetch, the auto-poll and the adjacent-volume
    /// nudge — file the archive their decode came out of **where
    /// [`Self::is_looping`] answers for the site**, because otherwise the
    /// volume they hand [`Self::cache_scan`] is one
    /// [`Self::evict_decoded_except`] can never drop. What has none is the
    /// chunk feed's assembled volume, which was built from chunks and was
    /// never one archive object; so an absent entry means "no archive was ever
    /// kept", never "the archive was lost".
    ///
    /// **No two-clock problem, on either path.** Every key here was filed in
    /// the same call, out of the same timestamp, as the `scan_cache` entry it
    /// belongs to — the loop's download under the S3 address it fetched at,
    /// the drain's under the address the arrival carried — so the two halves
    /// of a frame cannot end up under two addresses.
    archive_cache: HashMap<String, HashMap<chrono::NaiveDateTime, Arc<Vec<u8>>>>,
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
    /// frame — and a volume was measured at **48.88 MiB median live heap,
    /// 74.63 MiB max** over 208 real archive volumes, so on a 1 GiB wasm page
    /// heap this is a figure that decides whether a scene fits.
    ///
    /// Those are the corrected figures. The 46.1–46.8 MiB median / 58.3 MiB
    /// max this doc used to quote came from an instrument whose measurement
    /// window took its baseline **after** the compressed archive buffer had
    /// been read, and that buffer is freed inside the window — so every
    /// volume was discounted by its own compressed size (0.34–17.96 MiB,
    /// median 5.56). Under-stated, in the direction that costs the process.
    scan_bytes_cached: usize,
    /// **The largest volume this session has actually decoded from each
    /// site**, in the bytes `crate::scan_size::scan_bytes` prices them at.
    ///
    /// The measured half of [`Self::site_scan_reserve_bytes`]. Keyed by site
    /// and not by (site, stamp) on purpose: what it answers is "how big do
    /// this radar's volumes get", which is a property of the site's VCP and
    /// its weather, and a per-stamp figure would only ever describe a volume
    /// that has already arrived and so needs no reserve.
    ///
    /// It never falls. An eviction frees the bytes but does not unlearn the
    /// fact that this site produced them, and a reserve that forgot its own
    /// evidence would re-under-reserve the next time the same weather came
    /// back.
    site_scan_peak: std::collections::HashMap<String, usize>,
    /// The reserve's bootstrap, handed in by the application because the
    /// figure is the budget crate's (`LOOP_SCAN_RESERVE_BYTES`) and that
    /// crate depends on this one, not the other way about. Zero until set,
    /// which makes the reserve the site's own peak alone — the honest answer
    /// for a caller that has not said what its bootstrap is.
    scan_reserve_bootstrap: usize,
    /// **Volumes that arrived larger than the reserve in force for their
    /// site**, and by how much in total.
    ///
    /// The field's own report that the reserve is wrong. The figure this
    /// bootstraps from was called a maximum for months and turned out to be
    /// a 70.7th percentile, exceeded by 61 of 208 volumes — discovered by
    /// assembling a third corpus, which is a thing nobody does twice. These
    /// two counters are what make the next such discovery arrive from the
    /// field instead.
    scan_over_arrivals: u64,
    scan_over_arrival_bytes: u64,
    /// The price of each cached volume, so [`Self::retain_scans`] subtracts
    /// what it removes instead of re-walking it. Keyed exactly as
    /// [`scan_cache`](Self::scan_cache) is addressed, and every mutation of
    /// one is a mutation of the other.
    scan_prices: HashMap<(String, chrono::NaiveDateTime), usize>,
    /// **What [`archive_cache`](Self::archive_cache) is holding, in host
    /// bytes**: each buffer's length and one allocator block for it.
    ///
    /// A running total for [`Self::cached_scan_bytes`]'s reason, and a
    /// SEPARATE total from it on purpose. Decoded volumes and compressed
    /// archives are different orders of magnitude and are evicted by
    /// different policies; summing them into one figure would let a cache
    /// holding 25 archives and no volumes read like one holding half a
    /// volume, and the census family that reports it could not say which.
    archive_bytes_cached: usize,
    /// **Decodes dispatched and not yet landed, with the bytes each was
    /// reserved at** — the site's reserve at dispatch. Keyed as the caches
    /// are; a key here is also in `in_flight_set`, never the reverse.
    ///
    /// This is what makes the decoded ceiling an ADMISSION bound: a volume
    /// being decoded is not yet in `scan_bytes_cached`, so a pump that read
    /// only that total would dispatch a decode per pass until the first one
    /// landed and then find itself over the ceiling by every decode it had
    /// started. Reserved at dispatch, released at arrival.
    decodes_in_flight: HashMap<(String, chrono::NaiveDateTime), usize>,
    /// Running sum of [`decodes_in_flight`](Self::decodes_in_flight)'s values.
    decode_reserved_bytes: usize,
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
            archive_cache: HashMap::new(),
            archive_bytes_cached: 0,
            decodes_in_flight: HashMap::new(),
            decode_reserved_bytes: 0,
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
            site_scan_peak: std::collections::HashMap::new(),
            scan_reserve_bootstrap: 0,
            scan_over_arrivals: 0,
            scan_over_arrival_bytes: 0,
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
        // **Reconcile the reserve against the truth, here, at the one moment
        // the size is known.** Read the reserve that was in force *before*
        // this arrival taught the site anything, so a volume is scored
        // against the figure that was actually used to admit it — calibrating
        // first and then comparing would make every arrival its own reserve
        // and the counter could never fire.
        let reserved = self.site_scan_reserve_bytes(site);
        if price > reserved {
            self.scan_over_arrivals = self.scan_over_arrivals.saturating_add(1);
            self.scan_over_arrival_bytes = self
                .scan_over_arrival_bytes
                .saturating_add((price - reserved) as u64);
            log::debug!(
                "{site}: a volume arrived at {} MiB against a {} MiB reserve",
                price / (1024 * 1024),
                reserved / (1024 * 1024),
            );
        }
        let peak = self.site_scan_peak.entry(site.to_string()).or_insert(0);
        *peak = (*peak).max(price);
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
    /// [`crate::scan_size::scan_bytes`] — the vectors at capacity and one
    /// allocator block per allocation, no longer a floor. O(1): the total is
    /// maintained at the two places the cache changes.
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

    /// **What one archive buffer costs this process**: its bytes and the one
    /// allocator block holding them.
    fn archive_price(archive: &Arc<Vec<u8>>) -> usize {
        archive
            .len()
            .saturating_add(crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD)
    }

    /// **Keep the compressed archive a loop frame was decoded from**, so the
    /// decoded half can be evicted and rebuilt without the network.
    ///
    /// Takes the `Arc` the job funnel already moved by pointer
    /// (`jobs::DecodeJob::archive`), so retaining it is a refcount and never
    /// a copy of 1.0-16.1 MiB.
    ///
    /// **The buffer is shared with the decode job that is reading it** for as
    /// long as that job runs, so during a decode these bytes are named twice
    /// across this figure and the job's. After the reply lands this map is
    /// the only owner.
    pub fn cache_archive(&mut self, site: &str, ts: chrono::NaiveDateTime, archive: Arc<Vec<u8>>) {
        let price = Self::archive_price(&archive);
        let previous = self
            .archive_cache
            .entry(site.to_string())
            .or_default()
            .insert(ts, archive);
        if let Some(was) = previous {
            self.archive_bytes_cached = self
                .archive_bytes_cached
                .saturating_sub(Self::archive_price(&was));
        }
        self.archive_bytes_cached = self.archive_bytes_cached.saturating_add(price);
    }

    /// Whether the compressed archive for this frame is held, whatever the
    /// decoded half is doing.
    pub fn has_archive(&self, site: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.archive_cache
            .get(site)
            .is_some_and(|archives| archives.contains_key(ts))
    }

    /// The archive for this frame, as a pointer to hand the decode job.
    pub fn archive_for(&self, site: &str, ts: &chrono::NaiveDateTime) -> Option<Arc<Vec<u8>>> {
        self.archive_cache.get(site)?.get(ts).map(Arc::clone)
    }

    /// **Host bytes the compressed archives are holding.** O(1).
    ///
    /// Deliberately not folded into [`Self::cached_scan_bytes`]; see
    /// [`archive_bytes_cached`](Self::archive_bytes_cached).
    pub fn cached_archive_bytes(&self) -> usize {
        self.archive_bytes_cached
    }

    /// How many archives this site is holding. A probe, for the reason the
    /// probes above it are.
    pub fn cached_archive_count(&self, site: &str) -> usize {
        self.archive_cache
            .get(site)
            .map_or(0, |archives| archives.len())
    }

    /// **The third state of a loop frame: its bytes are here and its moments
    /// are not.**
    ///
    /// The download pump asks this after [`Self::is_cached`] and
    /// [`Self::is_in_flight`] have both said no. `true` means dispatch a
    /// decode of [`Self::archive_for`]; `false` with the other two false
    /// means dispatch a download.
    ///
    /// In-flight is part of the question because one `in_flight_set` covers
    /// both errands: a frame being decoded is in flight exactly as a frame
    /// being downloaded is, so the network concurrency cap keeps meaning one
    /// thing and a second decode cannot be dispatched for a frame already
    /// being decoded.
    pub fn needs_decode(&self, site: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.has_archive(site, ts) && !self.is_cached(site, ts) && !self.is_in_flight(site, ts)
    }

    /// **Evict the DECODED half of every frame that fails `keep`, holding on
    /// to the archive.** Hands the removed volumes back owned, for the
    /// deferred-drop path.
    ///
    /// This is the residency policy, and it is a different question from
    /// [`Self::retain_scans`]'s: that one asks whether the loop still wants
    /// the frame AT ALL and drops both halves, this one asks whether the
    /// frame needs its moments RIGHT NOW.
    ///
    /// Only two things read a decoded volume: a first render, and a retarget
    /// re-render. Playback reads neither — the texture already exists, and
    /// since `hover::SweepGates` stopped pinning the volume the readout holds
    /// its own sweep. So a frame that is textured and not about to be
    /// retargeted needs no moments, and the caller's `keep` is expected to be
    /// "has no texture yet, or is the playhead or within its lookahead".
    ///
    /// A frame evicted here comes back through the decode arm of the pump
    /// ([`Self::needs_decode`]), off the archive, at no network cost.
    /// **A volume with no archive behind it is never evicted here**, whatever
    /// `keep` says. This policy's whole premise is that eviction costs a
    /// decode; for a volume the archive drain or the chunk feed filed there
    /// is nothing to decode from, so evicting it would silently convert a
    /// residency policy into a re-download policy. Those volumes leave only
    /// through [`Self::retain_scans`], which is the caller saying the frame
    /// is not wanted at all.
    pub fn evict_decoded_except(
        &mut self,
        keep: impl Fn(&str, &chrono::NaiveDateTime, &nexrad_model::data::Scan) -> bool,
    ) -> Vec<CachedVolume> {
        // Disjoint field borrows: the archive map is read inside a closure
        // that is mutating the scan map, which `self.` spelling cannot express.
        let Self {
            scan_cache,
            archive_cache,
            scan_prices,
            scan_bytes_cached,
            ..
        } = self;
        let mut removed = Vec::new();
        let mut gone: Vec<(String, chrono::NaiveDateTime)> = Vec::new();
        scan_cache.retain(|site, scans| {
            removed.extend(
                scans
                    .extract_if(|ts, (scan, _)| {
                        let rebuildable = archive_cache
                            .get(site.as_str())
                            .is_some_and(|archives| archives.contains_key(ts));
                        rebuildable && !keep(site.as_str(), ts, scan)
                    })
                    .map(|(ts, volume)| {
                        gone.push((site.clone(), ts));
                        volume
                    }),
            );
            !scans.is_empty()
        });
        for key in gone {
            if let Some(price) = scan_prices.remove(&key) {
                *scan_bytes_cached = scan_bytes_cached.saturating_sub(price);
            }
        }
        removed
    }

    /// **Drop every archive whose `(site, timestamp)` fails `keep`.**
    ///
    /// Asked with the address alone and not with the volume, unlike
    /// [`Self::retain_scans`]: every archive here was filed under the address
    /// the loop downloaded it at, so the address IS its identity and the
    /// two-clock hazard that predicate exists for cannot arise.
    ///
    /// The caller passes the frame-list predicate — "does a live loop frame
    /// name this moment" — and not the narrower one that decides decoded
    /// residency. A site whose loop has retargeted to Level III keeps its
    /// archives while its frames stand, so retargeting back is a decode
    /// rather than the re-download it is today.
    pub fn retain_archives(&mut self, keep: impl Fn(&str, &chrono::NaiveDateTime) -> bool) {
        let mut freed = 0usize;
        self.archive_cache.retain(|site, archives| {
            archives.retain(|ts, archive| {
                let kept = keep(site.as_str(), ts);
                if !kept {
                    freed = freed.saturating_add(Self::archive_price(archive));
                }
                kept
            });
            !archives.is_empty()
        });
        self.archive_bytes_cached = self.archive_bytes_cached.saturating_sub(freed);
    }

    /// **Hold the archives inside a BYTE ceiling, evicting the ones furthest
    /// from the playhead first.** Returns the bytes freed.
    ///
    /// # Why bytes and not a frame count
    ///
    /// Measured over 39 archive volumes: the compressed form is 1,023,254 B
    /// at the smallest and 16,895,202 B at the largest, a **16.5x swing**,
    /// while the frame count a loop holds does not move at all. A ceiling
    /// expressed as frames and sized for the median is 2.9x over budget on
    /// the largest — `25 * 16,895,202` is 402.8 MiB, which is over the whole
    /// process target on its own, before a single sweep is held. So the
    /// bound has to be the quantity that actually varies.
    ///
    /// `rank` is asked per held archive and orders eviction: **higher is
    /// evicted first**, so a caller passes distance from the playhead. Frames
    /// nobody names should already have gone through
    /// [`Self::retain_archives`]; this is the second bound, for a loop whose
    /// every frame is legitimately named and whose archives still do not fit.
    ///
    /// An evicted archive costs a re-download if its frame is wanted again,
    /// which is exactly what a frame costs today — the policy degrades to
    /// current behaviour rather than to something worse.
    ///
    /// O(held) and only when over the ceiling; held is a few dozen.
    pub fn evict_archives_to_ceiling(
        &mut self,
        ceiling: usize,
        rank: impl Fn(&str, &chrono::NaiveDateTime) -> u64,
    ) -> usize {
        if self.archive_bytes_cached <= ceiling {
            return 0;
        }
        let mut held: Vec<(u64, String, chrono::NaiveDateTime, usize)> = Vec::new();
        // A plain walk and not an iterator chain: `rank` is borrowed by every
        // entry, and a nested `map` would have to move it.
        for (site, archives) in &self.archive_cache {
            for (ts, archive) in archives {
                held.push((
                    rank(site.as_str(), ts),
                    site.clone(),
                    *ts,
                    Self::archive_price(archive),
                ));
            }
        }
        // Furthest first, and the stamp breaks a tie so two archives at equal
        // distance evict in a fixed order rather than in map order.
        held.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)));
        let mut freed = 0usize;
        for (_, site, ts, price) in held {
            if self.archive_bytes_cached <= ceiling {
                break;
            }
            if let Some(archives) = self.archive_cache.get_mut(&site)
                && archives.remove(&ts).is_some()
            {
                self.archive_bytes_cached = self.archive_bytes_cached.saturating_sub(price);
                freed = freed.saturating_add(price);
                if archives.is_empty() {
                    self.archive_cache.remove(&site);
                }
            }
        }
        freed
    }

    /// **Tell this cache what a volume is presumed to cost** before anything
    /// has been measured — the budget crate's `LOOP_SCAN_RESERVE_BYTES`.
    ///
    /// Handed in rather than named here because the constant lives in the
    /// crate that depends on this one. Setting it is what turns
    /// [`Self::site_scan_reserve_bytes`] from "the largest volume seen" into
    /// the floor-and-evidence figure it is meant to be.
    pub fn set_scan_reserve_bootstrap(&mut self, bytes: usize) {
        self.scan_reserve_bootstrap = bytes;
    }

    /// **What one not-yet-arrived volume from `site` should be reserved at**:
    /// `max(bootstrap, that site's resident maximum)`.
    ///
    /// The bootstrap is a corpus percentile — a statement about radars in
    /// general. The site's own peak is a statement about *this* radar, made
    /// by this process, and **a site that has already handed this process a
    /// volume larger than the bootstrap is evidence no corpus percentile
    /// outranks**. So the two compose as a maximum and not as a replacement:
    /// the bootstrap is the whole reserve for the first frame at any site,
    /// which is exactly the case that has no measurement yet, and the
    /// evidence takes over the moment there is some.
    ///
    /// **It only ever rises**, and that is deliberate. A reserve that fell
    /// back toward the bootstrap when a big volume was evicted would let the
    /// same site under-reserve repeatedly, which is the failure this replaces
    /// rather than a different one.
    pub fn site_scan_reserve_bytes(&self, site: &str) -> usize {
        self.scan_reserve_bootstrap
            .max(self.site_scan_peak.get(site).copied().unwrap_or(0))
    }

    /// **Volumes that arrived larger than the reserve in force for them**,
    /// and the bytes by which they overshot.
    ///
    /// Always on, and reported whether or not anything gates on it: a reserve
    /// is a claim about the world, and this is the only figure that can
    /// falsify it from the field.
    pub fn scan_over_arrivals(&self) -> (u64, u64) {
        (self.scan_over_arrivals, self.scan_over_arrival_bytes)
    }

    /// **What a loop already holds decoded, at its measured size**: of the
    /// frames it names (`stamps`, its frame list), the ones this cache holds
    /// for `site`, as `(bytes, frames)` — the sum of the prices
    /// [`Self::cache_scan`] gave them on arrival, and how many there were.
    ///
    /// The reconciliation half of the budget model's scan term
    /// (`squallar_device_profile::fit::NeedTerms::loop_scans_host`): a
    /// resident frame costs what it was measured at, a frame still to come
    /// costs the reserve, so a bound is never charged for a measured thing
    /// and a live allocation is never priced under its size.
    ///
    /// **Per named frame, not per site**, and that is the whole reason this
    /// is a walk rather than a running total beside
    /// [`Self::cached_scan_bytes`]. A per-site total would answer a different
    /// question: the site's cache can hold a volume no live frame names any
    /// more — every pass until `App::evict_unneeded_loop_scans` runs — and
    /// two panes looping one site at different lookbacks name different
    /// subsets of it, so a total would over-price the narrower loop by
    /// exactly the frames it is not showing. It also could not say how many
    /// of the loop's frames are still to come, which is the count the reserve
    /// multiplies. A total maintained at the mutation sites would be cheaper
    /// per read and would answer neither.
    ///
    /// One map probe per named stamp, and one key allocation for the whole
    /// walk: the price map is keyed `(String, NaiveDateTime)` and a borrowed
    /// key cannot be built for it, so the four-letter site name is allocated
    /// once and the stamp rewritten per probe. Nothing is decoded and no
    /// radial is walked — safe on the frame thread, where the loop walk asks
    /// it once per looping pane.
    pub fn cached_scan_bytes_for(
        &self,
        site: &str,
        stamps: impl IntoIterator<Item = chrono::NaiveDateTime>,
    ) -> (usize, usize) {
        let mut key = (site.to_string(), chrono::NaiveDateTime::default());
        stamps.into_iter().fold((0, 0), |(bytes, frames), at| {
            key.1 = at;
            match self.scan_prices.get(&key) {
                Some(price) => (bytes.saturating_add(*price), frames + 1),
                None => (bytes, frames),
            }
        })
    }

    /// **Host bytes the loop's paired Level III objects are holding** — the
    /// product buffers. O(1), for [`Self::cached_scan_bytes`]'s reason.
    pub fn cached_l3_bytes(&self) -> usize {
        self.l3_bytes_cached
    }

    /// Take out every cached volume that fails `keep`, and hand the removed
    /// values back **owned**.
    ///
    /// `keep` is asked with the entry's **address** — the `(site, timestamp)`
    /// it was filed under — **and the volume itself**, because the two do not
    /// name one clock. A volume a loop downloaded is filed under its S3 key's
    /// second; one the archive drain filed is under the second it was fetched
    /// by; one the chunk feed filed is under its own first radial. A caller
    /// asking an IDENTITY question — "is this the volume a pane is parked on"
    /// — must answer it off the volume (`crate::types::volume_collected_at`),
    /// never off the address, or an archive-fetched volume a pane is parked on
    /// reads as unwanted and is evicted from under it.
    pub fn retain_scans(
        &mut self,
        keep: impl Fn(&str, &chrono::NaiveDateTime, &nexrad_model::data::Scan) -> bool,
    ) -> Vec<CachedVolume> {
        let mut removed = Vec::new();
        let mut gone: Vec<(String, chrono::NaiveDateTime)> = Vec::new();
        self.scan_cache.retain(|site, scans| {
            removed.extend(
                scans
                    .extract_if(|ts, (scan, _)| !keep(site.as_str(), ts, scan))
                    .map(|(ts, volume)| {
                        gone.push((site.clone(), ts));
                        volume
                    }),
            );
            !scans.is_empty()
        });
        // The prices go by exactly the keys that left, so the total falls by
        // what left rather than by a second walk of the volumes now removed.
        for key in gone {
            if let Some(price) = self.scan_prices.remove(&key) {
                self.scan_bytes_cached = self.scan_bytes_cached.saturating_sub(price);
            }
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
    /// **Bytes the decoded cache is committed to**: what it holds plus what
    /// every decode in flight was reserved at. The figure the decoded
    /// ceiling is enforced against, at the pump and at download arrival.
    pub fn decoded_committed_bytes(&self) -> usize {
        self.scan_bytes_cached
            .saturating_add(self.decode_reserved_bytes)
    }

    /// **Whether one more decode of a `site` volume fits under `ceiling`**,
    /// pricing the not-yet-decoded volume at the site's reserve — the same
    /// figure the budget model prices a pending frame at, so admission and
    /// the model cannot disagree about what a decode will cost.
    ///
    /// `true` when nothing is committed yet even if one volume alone exceeds
    /// the ceiling: a ceiling that admitted nothing would be a loop that never
    /// renders, which is not the failure it exists to prevent.
    pub fn decoded_room_for(&self, site: &str, ceiling: usize) -> bool {
        let committed = self.decoded_committed_bytes();
        committed == 0 || committed.saturating_add(self.site_scan_reserve_bytes(site)) <= ceiling
    }

    /// Mark a DECODE in flight: the shared in-flight mark, plus a reservation
    /// of the site's reserve against the decoded ceiling.
    pub fn mark_decode_in_flight(&mut self, site: &str, ts: chrono::NaiveDateTime) {
        let reserve = self.site_scan_reserve_bytes(site);
        self.mark_in_flight(site, ts);
        if self
            .decodes_in_flight
            .insert((site.to_string(), ts), reserve)
            .is_none()
        {
            self.decode_reserved_bytes = self.decode_reserved_bytes.saturating_add(reserve);
        }
    }

    /// **Every moment of `pane`'s site whose bytes are held and whose moments
    /// are not** — the frames the pump should decode once the decoded ceiling
    /// has room. Catches both a frame the residency policy evicted and one
    /// whose decode was deferred at arrival, neither of which is in any
    /// download queue any more.
    ///
    /// **The plan's own frames first, in plan order, then the site's other
    /// archived moments oldest-first.** The pump takes these until it runs out
    /// of slots, so the order is a priority and the listed frames are the ones
    /// a re-plan would also queue.
    ///
    /// **Asked of the archives and not of the plan alone**, because a plan is
    /// built from a bucket listing and the archive drain files moments from
    /// outside it: the pane fetch, the auto-poll and the adjacent-volume nudge
    /// each put a volume and its compressed bytes in this cache without any
    /// listing having named the moment. Restricting the walk to the plan would
    /// make those arrivals evictable by
    /// [`Self::evict_decoded_except`] and restorable by nothing until the next
    /// listing arrived. The archive half is what bounds this: `retain_archives`
    /// keeps only moments a live loop frame names, so a moment offered here is
    /// one something still wants.
    pub fn frames_needing_decode(&self, pane: usize) -> Vec<(String, chrono::NaiveDateTime)> {
        let Some(plan) = self.plans.get(&pane) else {
            return Vec::new();
        };
        let mut wanted: Vec<chrono::NaiveDateTime> = plan
            .frames
            .iter()
            .copied()
            .filter(|ts| self.needs_decode(&plan.site, ts))
            .collect();
        if let Some(archives) = self.archive_cache.get(plan.site.as_str()) {
            let mut unlisted: Vec<chrono::NaiveDateTime> = archives
                .keys()
                .copied()
                .filter(|ts| !plan.frames.contains(ts) && self.needs_decode(&plan.site, ts))
                .collect();
            unlisted.sort_unstable();
            wanted.extend(unlisted);
        }
        wanted
            .into_iter()
            .map(|ts| (plan.site.clone(), ts))
            .collect()
    }

    pub fn complete_download(&mut self, site: &str, ts: &chrono::NaiveDateTime) {
        if let Some(reserve) = self.decodes_in_flight.remove(&(site.to_string(), *ts)) {
            self.decode_reserved_bytes = self.decode_reserved_bytes.saturating_sub(reserve);
        }
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

    /// **Whether any pane is looping `site`** — asked of the plans, which are
    /// what a running loop leaves here and what
    /// [`Self::remove_pending`] takes away when one stops.
    ///
    /// The question a caller filing an arrival's compressed bytes needs
    /// answered: an archive earns its keep only where
    /// [`Self::evict_decoded_except`] can trade it for a decoded volume, and
    /// that pass keeps the volume a pane is parked at whatever else it does.
    /// So on a site nothing loops the compressed half would be held for a
    /// swap that cannot happen.
    ///
    /// **A proxy, and loose in the safe direction.** A plan outlives the frame
    /// list it was built from until the pane's queues are removed, so this can
    /// answer `true` for a loop that has just ended — which costs one archive
    /// the next `retain_archives` drops, not a decoded volume.
    ///
    /// O(panes with a plan), which is at most the pane count.
    pub fn is_looping(&self, site: &str) -> bool {
        self.plans.values().any(|plan| plan.site == site)
    }

    /// Record what volumes a pane's loop frames name, replacing any previous
    /// plan and the queues derived from it.
    pub fn set_plan(&mut self, pane: usize, plan: FramePlan) {
        self.pending_downloads.remove(&pane);
        self.pending_l3.remove(&pane);
        self.plans.insert(pane, plan);
    }

    /// **Whether the plan filed for `pane` still describes the loop it serves**
    /// — the same site, the same stamps, in the same order.
    ///
    /// **The way back.** A plan is a derivation of a loop's frame list and of
    /// nothing else — `the_frame_list_and_the_frame_plan_describe_the_same_scans`
    /// pins the two as equal frame for frame where the listing lands — but it
    /// was only ever *re-derived* on an event: `resample_frames` reporting that
    /// the list had changed. A queue retired by [`Self::remove_pending`]
    /// therefore had no way back at all. The list it was derived from had not
    /// changed, so nothing re-asked for it, and a loop whose queue was retired
    /// sat holding frames it would never refetch — which is why nothing could
    /// afford to retire one.
    ///
    /// Asked as a **state** and not as an event, it composes: false for a plan
    /// that was retired, false for one the frame list has moved out from
    /// under, and true on every pass in between. A caller that re-derives on
    /// `false` re-derives exactly when the derivation is stale and is a no-op
    /// the rest of the time, whatever the reason the two came apart.
    ///
    /// No allocation, and it stops at the first disagreement: a site compare
    /// and a few dozen `NaiveDateTime` compares.
    pub fn plan_describes(
        &self,
        pane: usize,
        site: &str,
        frames: impl IntoIterator<Item = chrono::NaiveDateTime>,
    ) -> bool {
        self.plans
            .get(&pane)
            .is_some_and(|plan| plan.site == site && plan.frames.iter().copied().eq(frames))
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
                        surface: ctx.surface,
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

    pub(super) fn ts(minute: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, minute, 0)
            .unwrap()
    }

    /// A distinct cached volume. The contents do not matter — every assertion
    /// here is about *which* `Arc` comes back out, compared by pointer — and
    /// nothing here reads the declarations, so the fixture declares nothing.
    pub(super) fn volume() -> CachedVolume {
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
    pub(super) fn priced_volume() -> CachedVolume {
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

        mgr.retain_scans(|_, at, _| *at == ts(0));
        assert_eq!(mgr.cached_scan_bytes(), one, "eviction did not subtract");
        mgr.retain_scans(|_, _, _| false);
        assert_eq!(mgr.cached_scan_bytes(), 0, "an emptied cache still priced");
        mgr.retain_l3(|_, _| false);
        assert_eq!(mgr.cached_l3_bytes(), 0);
    }

    /// **The reconciliation figure is the named frames this cache holds, at
    /// their arrival prices, and nothing else.** Two of three named stamps
    /// held: their two prices and a count of two. A volume the site holds
    /// that the loop does not name is not in it — the case a per-site total
    /// would get wrong — and neither is another site's volume at a named
    /// stamp. An empty frame list is `(0, 0)`, and so is a site the cache has
    /// never seen; the count is what the reserve is NOT charged for, so a
    /// figure that drifted up would under-price a loop rather than over-.
    #[test]
    fn cached_scan_bytes_for_prices_the_named_frames_the_cache_holds() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        mgr.cache_scan("KTLX", ts(5), priced_volume());
        mgr.cache_scan("KTLX", ts(30), priced_volume());
        mgr.cache_scan("KFWS", ts(10), priced_volume());
        let one = mgr.cached_scan_price("KTLX", &ts(0)).expect("filed");
        assert!(one > 0, "the fixture must price at something");

        let named = [ts(0), ts(5), ts(10)];
        assert_eq!(
            mgr.cached_scan_bytes_for("KTLX", named),
            (2 * one, 2),
            "two of the three named frames are held, at their prices — the \
             volume at ts(30) is the site's and is not named",
        );
        assert_eq!(
            mgr.cached_scan_bytes_for("KFWS", named),
            (one, 1),
            "the other site holds one of the named stamps",
        );
        assert_eq!(mgr.cached_scan_bytes_for("KTLX", []), (0, 0));
        assert_eq!(mgr.cached_scan_bytes_for("KDDC", named), (0, 0));

        // Every KTLX volume named: the site's whole cache, which is the
        // running total less the other site's one.
        assert_eq!(
            mgr.cached_scan_bytes_for("KTLX", [ts(0), ts(5), ts(30)]),
            (mgr.cached_scan_bytes() - one, 3),
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

        let removed = mgr.retain_scans(|_, stamp, _| *stamp == ts(1));

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

        let removed = mgr.retain_scans(|site, _, _| site == "KTLX");

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

        let removed = mgr.retain_scans(|site, stamp, _| site == "KOUN" && *stamp == ts(1));

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

        let removed = mgr.retain_scans(|_, _, _| false);

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
    /// **The bootstrap is a floor, not the whole answer.** Before any volume
    /// has arrived from a site there is nothing to know about it, so the
    /// reserve is the corpus figure the application handed down.
    #[test]
    fn a_site_with_no_history_reserves_the_bootstrap() {
        let mut mgr = LoopDownloadManager::new();
        assert_eq!(
            mgr.site_scan_reserve_bytes("KTLX"),
            0,
            "with no bootstrap set the reserve can only be the site's own peak",
        );
        mgr.set_scan_reserve_bootstrap(80 * 1024 * 1024);
        assert_eq!(mgr.site_scan_reserve_bytes("KTLX"), 80 * 1024 * 1024);
        assert_eq!(
            mgr.site_scan_reserve_bytes("KOUN"),
            80 * 1024 * 1024,
            "a site nobody has fetched from is not special",
        );
    }

    /// **A site that has handed this process a volume larger than the
    /// bootstrap is evidence no corpus percentile outranks.** The reserve
    /// rises to it, and only for that site.
    #[test]
    fn a_site_reserve_rises_to_the_largest_volume_that_site_produced() {
        let mut mgr = LoopDownloadManager::new();
        // A bootstrap below the fixture's own size, so the arrival is the
        // larger of the two and the calibration has something to do.
        mgr.set_scan_reserve_bootstrap(1);
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        let measured = mgr.cached_scan_price("KTLX", &ts(0)).expect("priced");

        assert_eq!(
            mgr.site_scan_reserve_bytes("KTLX"),
            measured,
            "the reserve did not learn from the volume that arrived",
        );
        assert_eq!(
            mgr.site_scan_reserve_bytes("KOUN"),
            1,
            "one site's evidence moved another site's reserve",
        );
    }

    /// The reserve never falls: an eviction frees the bytes but does not
    /// unlearn that the site produced them. A reserve that forgot its own
    /// evidence would under-reserve the next time the same weather came back.
    #[test]
    fn a_site_reserve_does_not_fall_when_the_volume_is_evicted() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_scan_reserve_bootstrap(1);
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        let learned = mgr.site_scan_reserve_bytes("KTLX");
        assert!(learned > 1);

        let dropped = mgr.retain_scans(|_, _, _| false);
        assert_eq!(dropped.len(), 1, "the fixture was not evicted");
        assert_eq!(
            mgr.cached_scan_bytes(),
            0,
            "the fixture did not actually leave the cache",
        );
        assert_eq!(
            mgr.site_scan_reserve_bytes("KTLX"),
            learned,
            "the reserve unlearned its own evidence on eviction",
        );
    }

    /// **The field's own report that a reserve is wrong.** A volume larger
    /// than the reserve in force is counted with its shortfall — the figure
    /// this bootstraps from was called a maximum and was in fact a 70.7th
    /// percentile, found by assembling a third corpus, which is a thing
    /// nobody does twice.
    #[test]
    fn a_volume_over_its_reserve_is_counted_with_its_shortfall() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_scan_reserve_bootstrap(1);
        assert_eq!(mgr.scan_over_arrivals(), (0, 0));

        mgr.cache_scan("KTLX", ts(0), priced_volume());
        let measured = mgr.cached_scan_price("KTLX", &ts(0)).expect("priced");
        let (count, bytes) = mgr.scan_over_arrivals();
        assert_eq!(
            count, 1,
            "the arrival over a 1-byte reserve was not counted"
        );
        assert_eq!(bytes, (measured - 1) as u64);

        // The next volume of the same size is no longer a surprise: the site
        // has calibrated, so an arrival AT the reserve is not over it. A
        // counter that fired here would report every reserve wrong forever,
        // including the volume the reserve was sized from.
        mgr.cache_scan("KTLX", ts(1), priced_volume());
        assert_eq!(
            mgr.scan_over_arrivals(),
            (1, (measured - 1) as u64),
            "an arrival at exactly the calibrated reserve was counted as over",
        );
    }

    /// The healthy arm: with the shipped bootstrap above the fixture's size,
    /// nothing is ever counted. A counter that fires on ordinary work cannot
    /// be read as evidence of anything.
    #[test]
    fn a_volume_under_its_reserve_is_counted_nowhere() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_scan_reserve_bootstrap(80 * 1024 * 1024);
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        mgr.cache_scan("KTLX", ts(1), priced_volume());
        mgr.cache_scan("KOUN", ts(0), priced_volume());

        assert_eq!(mgr.scan_over_arrivals(), (0, 0));
        assert_eq!(
            mgr.site_scan_reserve_bytes("KTLX"),
            80 * 1024 * 1024,
            "evidence smaller than the bootstrap lowered the reserve",
        );
    }
}

/// **The compressed-archive half of the loop cache**, and the two eviction
/// policies over it.
///
/// # Every test here names the tamper that must turn it red
///
/// Part B shipped a claim in its commit body that no test in its file could
/// falsify — every fixture happened to make the defect invisible — and it was
/// caught only by tampering the assertion after writing it. So each test below
/// records, in its own doc, the one-line change to production code that must
/// make it fail. **A test whose tamper has not been run is not yet a gate**,
/// and none of these tampers has been run at the time of writing.
#[cfg(test)]
mod archive_tests {
    use super::tests::{priced_volume, ts, volume};
    use super::*;

    /// An archive buffer of exactly `bytes` bytes.
    fn archive(bytes: usize) -> Arc<Vec<u8>> {
        Arc::new(vec![7u8; bytes])
    }

    /// **A volume with no archive behind it can starve the decode pump, and
    /// the archive is what un-starves it.**
    ///
    /// `decoded_room_for` is what admits a decode, at the pump and at download
    /// arrival, and `evict_decoded_except` is the only thing that brings the
    /// committed figure back down — but it refuses a volume with nothing to
    /// rebuild from. So a cache filled with archive-less volumes a live loop
    /// still names is over the ceiling with no way back under, and every frame
    /// the loop downloads for itself waits compressed and undrawn. That is the
    /// shape the archive drain's arrivals used to have: they were filed by
    /// `App::append_scan_to_active_loops` with their compressed half thrown
    /// away.
    ///
    /// The ceiling is a parameter here, so this asks the real predicate at a
    /// size the fixture can reach rather than pinning a device constant.
    ///
    /// TAMPER: make `evict_decoded_except` ignore its `rebuildable` guard —
    /// the archive-less half of this test goes green, which is the whole
    /// distinction it draws.
    #[test]
    fn archive_less_volumes_hold_the_decoded_ceiling_shut() {
        let one = crate::scan_size::scan_bytes(&priced_volume().0);
        let ceiling = one * 2;

        let mut with = LoopDownloadManager::new();
        let mut without = LoopDownloadManager::new();
        for minute in [0, 2, 4] {
            with.cache_scan("KTLX", ts(minute), priced_volume());
            with.cache_archive("KTLX", ts(minute), archive(1024));
            without.cache_scan("KTLX", ts(minute), priced_volume());
        }

        assert!(
            !with.decoded_room_for("KTLX", ceiling) && !without.decoded_room_for("KTLX", ceiling),
            "precondition: both caches are over the ceiling, so the pump \
             admits no decode from either",
        );

        with.evict_decoded_except(|_, _, _| false);
        without.evict_decoded_except(|_, _, _| false);

        assert!(
            with.decoded_room_for("KTLX", ceiling),
            "the residency pass traded the moments for the archives and the \
             pump can dispatch again",
        );
        assert!(
            !without.decoded_room_for("KTLX", ceiling),
            "and with nothing to rebuild from the same pass frees nothing, so \
             the ceiling stays shut for as long as the loop names the frames",
        );
        assert_eq!(
            without.cached_scan_count("KTLX"),
            3,
            "not one of them left: this is a policy refusing to evict, not an \
             eviction that failed",
        );
    }

    /// **A moment no listing named is still offered back to the decode arm**,
    /// after the plan's own frames and oldest-first.
    ///
    /// The archive drain files volumes from outside the loop's download path,
    /// so their moments are in no plan. Before those arrivals carried their
    /// compressed bytes this far they could never be evicted and the gap did
    /// not matter; now that `evict_decoded_except` can drop them, a walk over
    /// the plan alone would give the moments away with nothing able to hand
    /// them back until the next listing arrived.
    ///
    /// TAMPER: restrict the walk to `plan.frames` again, or drop the
    /// `!plan.frames.contains(ts)` guard. Both fail every run.
    ///
    /// **The ordering conjunct is gated probabilistically and nothing can make
    /// it otherwise**: the source is a `HashMap`, so dropping `sort_unstable`
    /// fails only on the runs whose iteration order is not already ascending.
    /// Measured at two unlisted moments it came back green on 3 of 5 runs —
    /// a coin flip, which is no gate at all. Six of them put a false green at
    /// one permutation in 720, and they are filed in scrambled order so the
    /// insertion sequence is not the answer either.
    #[test]
    fn the_decode_walk_offers_an_archived_moment_no_plan_names() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(0, FramePlan::new("KTLX".to_string(), vec![ts(4)]));
        // The plan's own frame, held in both forms and then evicted: the
        // ordinary loop-download case.
        mgr.cache_scan("KTLX", ts(4), volume());
        mgr.cache_archive("KTLX", ts(4), archive(1024));
        // Six drain arrivals in scrambled order — see the note on the
        // ordering conjunct above.
        for minute in [11, 5, 13, 7, 3, 9] {
            mgr.cache_scan("KTLX", ts(minute), volume());
            mgr.cache_archive("KTLX", ts(minute), archive(1024));
        }
        // A fourth site's arrival, which this pane's plan must not name.
        mgr.cache_scan("KOUN", ts(7), volume());
        mgr.cache_archive("KOUN", ts(7), archive(1024));

        assert!(
            mgr.frames_needing_decode(0).is_empty(),
            "precondition: nothing needs a decode while every volume's \
             moments are here",
        );

        mgr.evict_decoded_except(|_, _, _| false);

        assert_eq!(
            mgr.frames_needing_decode(0),
            vec![
                ("KTLX".to_string(), ts(4)),
                ("KTLX".to_string(), ts(3)),
                ("KTLX".to_string(), ts(5)),
                ("KTLX".to_string(), ts(7)),
                ("KTLX".to_string(), ts(9)),
                ("KTLX".to_string(), ts(11)),
                ("KTLX".to_string(), ts(13)),
            ],
            "the plan's frame first because a re-plan would queue it too, \
             then the unlisted arrivals oldest-first, and no other site's",
        );
    }

    /// The measured extremes of the 39-volume corpus, so a ceiling test is run
    /// against the size that actually breaks a frame-count bound rather than
    /// against the median that hides it.
    const CORPUS_MIN_ARCHIVE: usize = 1_023_254;
    const CORPUS_MAX_ARCHIVE: usize = 16_895_202;

    /// **`is_cached` keeps meaning "renderable now".**
    ///
    /// Every existing caller reads it to decide whether a frame can be drawn or
    /// has to be fetched, and a compressed-only entry can do neither — its
    /// moments are gone. If this ever answers `true` for an archive-only frame,
    /// the render path will ask for a volume that is not there.
    ///
    /// TAMPER: make `is_cached` consult `archive_cache` as well as `scan_cache`.
    #[test]
    fn an_archive_alone_is_not_a_cached_scan() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_archive("KTLX", ts(0), archive(4096));

        assert!(
            mgr.has_archive("KTLX", &ts(0)),
            "precondition: the archive was filed"
        );
        assert!(
            !mgr.is_cached("KTLX", &ts(0)),
            "an archive-only frame reported itself renderable"
        );
        assert!(
            mgr.get_cached("KTLX", &ts(0)).is_none(),
            "an archive-only frame handed out a volume"
        );
        assert_eq!(
            mgr.cached_scan_count("KTLX"),
            0,
            "an archive was counted as a decoded volume"
        );
    }

    /// **The third planner state is exactly "bytes here, moments not, nothing
    /// in flight".**
    ///
    /// This is the predicate the download pump branches on. Each of the three
    /// conjuncts is asserted separately, because a predicate that ignored one
    /// of them would still pass a test that only checked the happy case: an
    /// ignored `is_cached` re-decodes a frame that is already renderable, and
    /// an ignored `is_in_flight` dispatches a second decode for a frame already
    /// being decoded.
    ///
    /// TAMPER: drop any one of the three conjuncts from `needs_decode`.
    #[test]
    fn needs_decode_is_true_only_with_bytes_and_no_moments_and_no_errand() {
        let mut mgr = LoopDownloadManager::new();

        assert!(
            !mgr.needs_decode("KTLX", &ts(0)),
            "a frame with nothing at all wants a download, not a decode"
        );

        mgr.cache_archive("KTLX", ts(0), archive(4096));
        assert!(
            mgr.needs_decode("KTLX", &ts(0)),
            "bytes held and moments absent is the decode state"
        );

        mgr.mark_in_flight("KTLX", ts(0));
        assert!(
            !mgr.needs_decode("KTLX", &ts(0)),
            "a frame already on an errand must not be dispatched again"
        );
        mgr.complete_download("KTLX", &ts(0));

        mgr.cache_scan("KTLX", ts(0), volume());
        assert!(
            !mgr.needs_decode("KTLX", &ts(0)),
            "a renderable frame does not need decoding"
        );
    }

    /// **Evicting the moments keeps the bytes, and the two totals move
    /// independently.**
    ///
    /// The whole design rests on this: the decoded total falls by a volume, the
    /// archive total does not move, and the frame is left in the state the pump
    /// will rebuild from.
    ///
    /// TAMPER: make `evict_decoded_except` drop the archive entry too.
    #[test]
    fn evicting_the_decoded_half_leaves_the_archive_and_its_bytes() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_archive("KTLX", ts(0), archive(CORPUS_MIN_ARCHIVE));
        mgr.cache_scan("KTLX", ts(0), priced_volume());

        let decoded_before = mgr.cached_scan_bytes();
        let archives_before = mgr.cached_archive_bytes();
        assert!(decoded_before > 0, "precondition: the volume was priced");
        assert!(archives_before > 0, "precondition: the archive was priced");

        let dropped = mgr.evict_decoded_except(|_, _, _| false);

        assert_eq!(dropped.len(), 1, "the volume was not handed back owned");
        assert_eq!(
            mgr.cached_scan_bytes(),
            0,
            "the decoded total did not fall by the evicted volume"
        );
        assert_eq!(
            mgr.cached_archive_bytes(),
            archives_before,
            "evicting moments moved the archive total"
        );
        assert!(
            mgr.has_archive("KTLX", &ts(0)),
            "the archive went with the volume, so the frame now costs a download"
        );
        assert!(
            mgr.needs_decode("KTLX", &ts(0)),
            "the evicted frame did not land in the decode state"
        );
    }

    /// **A volume with no archive behind it is never evicted by the residency
    /// policy.**
    ///
    /// The archive drain and the chunk feed file decoded volumes with no
    /// compressed bytes. For those, eviction is not "costs a decode" but "costs
    /// a re-download", which silently converts a residency policy into a
    /// bandwidth policy. This is the conjunct most likely to be dropped by
    /// someone simplifying the closure, and the one whose loss is invisible
    /// until a user on a slow link scrubs a loop.
    ///
    /// TAMPER: remove the `rebuildable &&` guard in `evict_decoded_except`.
    #[test]
    fn a_volume_with_no_archive_survives_the_residency_policy() {
        let mut mgr = LoopDownloadManager::new();
        // Filed the way the chunk feed and the archive drain file: no archive.
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        let priced = mgr.cached_scan_bytes();

        let dropped = mgr.evict_decoded_except(|_, _, _| false);

        assert!(
            dropped.is_empty(),
            "a volume with nothing to rebuild it from was evicted"
        );
        assert!(
            mgr.is_cached("KTLX", &ts(0)),
            "and it is gone from the cache"
        );
        assert_eq!(
            mgr.cached_scan_bytes(),
            priced,
            "its bytes left the total while the volume stayed, or the reverse"
        );
    }

    /// **`retain_archives` drops by the frame predicate and subtracts exactly
    /// what left.**
    ///
    /// TAMPER: make `retain_archives` return without touching
    /// `archive_bytes_cached`.
    #[test]
    fn dropping_archives_subtracts_their_bytes() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_archive("KTLX", ts(0), archive(CORPUS_MIN_ARCHIVE));
        mgr.cache_archive("KTLX", ts(1), archive(CORPUS_MAX_ARCHIVE));
        mgr.cache_archive("KOUN", ts(0), archive(CORPUS_MIN_ARCHIVE));
        let all = mgr.cached_archive_bytes();

        mgr.retain_archives(|site, at| site == "KTLX" && *at == ts(0));

        assert!(mgr.has_archive("KTLX", &ts(0)), "the kept archive left");
        assert!(
            !mgr.has_archive("KTLX", &ts(1)),
            "an unwanted archive stayed"
        );
        assert!(
            !mgr.has_archive("KOUN", &ts(0)),
            "another site's archive stayed"
        );
        assert_eq!(
            mgr.cached_archive_bytes(),
            CORPUS_MIN_ARCHIVE + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD,
            "the total is not what the one surviving archive holds"
        );
        assert!(
            mgr.cached_archive_bytes() < all,
            "the total did not fall at all"
        );

        mgr.retain_archives(|_, _| false);
        assert_eq!(
            mgr.cached_archive_bytes(),
            0,
            "an emptied cache still priced"
        );
    }

    /// **Re-filing an archive under a key already held does not double count.**
    ///
    /// The re-decode path files the same buffer back, so this runs on every
    /// rebuild rather than being an edge case.
    ///
    /// TAMPER: drop the `if let Some(was)` subtraction in `cache_archive`.
    #[test]
    fn refiling_one_archive_prices_it_once() {
        let mut mgr = LoopDownloadManager::new();
        let bytes = archive(CORPUS_MIN_ARCHIVE);
        mgr.cache_archive("KTLX", ts(0), Arc::clone(&bytes));
        let once = mgr.cached_archive_bytes();

        mgr.cache_archive("KTLX", ts(0), bytes);

        assert_eq!(
            mgr.cached_archive_bytes(),
            once,
            "re-filing the same buffer charged for it twice"
        );
        assert_eq!(mgr.cached_archive_count("KTLX"), 1);
    }

    /// **The archive ceiling is held in BYTES, and the test uses the corpus
    /// MAXIMUM rather than its median.**
    ///
    /// This is the test the design exists for. A frame-count bound sized for a
    /// median archive passes on median inputs and is 2.9x over budget on the
    /// largest: 25 frames at 16,895,202 B is 402.8 MiB, past the whole process
    /// target on its own. So the fixture is 25 of the largest, and the
    /// assertion is against the ceiling, not against a frame count.
    ///
    /// TAMPER: change `evict_archives_to_ceiling` to stop after a fixed number
    /// of evictions rather than when under the ceiling.
    #[test]
    fn the_archive_ceiling_binds_on_maximum_sized_archives() {
        const CEILING: usize = 96 * 1024 * 1024;
        let mut mgr = LoopDownloadManager::new();
        for minute in 0..25u32 {
            mgr.cache_archive("KTLX", ts(minute), archive(CORPUS_MAX_ARCHIVE));
        }
        let before = mgr.cached_archive_bytes();
        assert!(
            before > CEILING,
            "precondition: 25 maximum archives ({before} B) must exceed the ceiling"
        );

        // Furthest from the playhead first: minute 0 is the playhead.
        let freed = mgr.evict_archives_to_ceiling(CEILING, |_, at| {
            (at.and_utc().timestamp() - ts(0).and_utc().timestamp()) as u64
        });

        assert!(freed > 0, "nothing was evicted");
        assert!(
            mgr.cached_archive_bytes() <= CEILING,
            "the cache is still {} B, over the {CEILING} B ceiling",
            mgr.cached_archive_bytes()
        );
        assert!(
            mgr.has_archive("KTLX", &ts(0)),
            "the playhead's own archive was evicted before further ones"
        );
        assert!(
            !mgr.has_archive("KTLX", &ts(24)),
            "the furthest archive survived while the ceiling was exceeded"
        );
    }

    /// **Under the ceiling, nothing is evicted and nothing is walked.**
    ///
    /// The counterpart the ceiling test cannot give: a policy that always
    /// evicted something would pass the test above and quietly shrink a cache
    /// that fits.
    ///
    /// TAMPER: remove the `if self.archive_bytes_cached <= ceiling { break; }`
    /// guard from the eviction loop. Removing the EARLY RETURN alone does not
    /// turn this red — the in-loop guard still stops it on the first pass —
    /// which the tamper run showed and which is why the guard is named here
    /// rather than the return.
    #[test]
    fn a_cache_inside_the_ceiling_is_left_alone() {
        const CEILING: usize = 96 * 1024 * 1024;
        let mut mgr = LoopDownloadManager::new();
        for minute in 0..25u32 {
            mgr.cache_archive("KTLX", ts(minute), archive(CORPUS_MIN_ARCHIVE));
        }
        let before = mgr.cached_archive_bytes();
        assert!(before <= CEILING, "precondition: 25 minimum archives fit");

        let freed = mgr.evict_archives_to_ceiling(CEILING, |_, _| 0);

        assert_eq!(freed, 0, "a cache that fits was trimmed anyway");
        assert_eq!(mgr.cached_archive_bytes(), before);
        assert_eq!(mgr.cached_archive_count("KTLX"), 25);
    }

    /// **The decoded ceiling is enforced against what is COMMITTED — held plus
    /// reserved in flight — not against what has landed.**
    ///
    /// A pump that read only `cached_scan_bytes` would dispatch one decode per
    /// pass until the first landed, then find itself over the ceiling by every
    /// decode it had started. On the reproduced wasm freeze that is fourteen
    /// volumes admitted at once out of a retarget. The reservation is what
    /// makes the ceiling an admission bound.
    ///
    /// Nothing here consults the admission door or a grant: on that
    /// reproduction the door admitted every time on an over-stated `room`, so
    /// a grant is not evidence that anything fits.
    ///
    /// TAMPER: make `decoded_committed_bytes` return `scan_bytes_cached` alone.
    #[test]
    fn the_decoded_ceiling_counts_decodes_in_flight() {
        let mut mgr = LoopDownloadManager::new();
        const RESERVE: usize = 50 * 1024 * 1024;
        mgr.set_scan_reserve_bootstrap(RESERVE);
        // Room for two reserves and not three.
        let ceiling = 2 * RESERVE + RESERVE / 2;
        for minute in 0..4u32 {
            mgr.cache_archive("KTLX", ts(minute), archive(4096));
        }

        assert!(
            mgr.decoded_room_for("KTLX", ceiling),
            "an empty cache has room"
        );
        mgr.mark_decode_in_flight("KTLX", ts(0));
        assert_eq!(
            mgr.decoded_committed_bytes(),
            RESERVE,
            "one decode reserved"
        );
        assert!(mgr.decoded_room_for("KTLX", ceiling), "room for a second");
        mgr.mark_decode_in_flight("KTLX", ts(1));
        assert_eq!(mgr.decoded_committed_bytes(), 2 * RESERVE);
        assert!(
            !mgr.decoded_room_for("KTLX", ceiling),
            "a third decode was admitted with two in flight and no room for it"
        );
        // Marking the same frame twice reserves once.
        mgr.mark_decode_in_flight("KTLX", ts(1));
        assert_eq!(
            mgr.decoded_committed_bytes(),
            2 * RESERVE,
            "a re-mark double-reserved"
        );

        // Landing releases the reservation and the volume takes its place at
        // its measured size, which is what the ceiling then sees.
        mgr.complete_download("KTLX", &ts(0));
        assert_eq!(
            mgr.decoded_committed_bytes(),
            RESERVE,
            "arrival did not release"
        );
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        assert_eq!(
            mgr.decoded_committed_bytes(),
            RESERVE + mgr.cached_scan_bytes(),
            "the landed volume is not what the ceiling now sees"
        );
        assert!(
            mgr.decoded_room_for("KTLX", ceiling),
            "with one reserve and one small volume there is room again"
        );
    }

    /// **A ceiling smaller than one volume still admits the first one.**
    ///
    /// The bound exists to stop a loop killing the page, not to stop it
    /// rendering. A ceiling that admitted nothing on a site whose reserve
    /// exceeds it would be a loop that never draws, silently.
    ///
    /// TAMPER: drop the `committed == 0 ||` arm from `decoded_room_for`.
    #[test]
    fn an_empty_decoded_set_admits_one_volume_whatever_the_ceiling() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_scan_reserve_bootstrap(80 * 1024 * 1024);
        assert!(
            mgr.decoded_room_for("KTLX", 1),
            "a 1-byte ceiling refused the first volume, so the loop never renders"
        );
        mgr.mark_decode_in_flight("KTLX", ts(0));
        assert!(
            !mgr.decoded_room_for("KTLX", 1),
            "and it admits only the one"
        );
    }

    /// **The plan walk finds both kinds of frame that need decoding and are in
    /// no queue**: one the residency pass evicted, and one whose decode was
    /// deferred at arrival. Neither is in a download queue any more, so a pump
    /// that only drained its queue would never decode either.
    ///
    /// TAMPER: make `frames_needing_decode` read the pending queue instead of
    /// the plan.
    #[test]
    fn the_plan_walk_finds_evicted_and_deferred_frames() {
        let mut mgr = LoopDownloadManager::new();
        mgr.set_plan(
            7,
            FramePlan::new("KTLX".to_string(), vec![ts(0), ts(1), ts(2), ts(3)]),
        );
        // ts(0): decoded and textured -> evicted by the residency pass.
        mgr.cache_archive("KTLX", ts(0), archive(4096));
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        mgr.evict_decoded_except(|_, _, _| false);
        // ts(1): arrived with the decoded set full -> filed compressed only.
        mgr.cache_archive("KTLX", ts(1), archive(4096));
        // ts(2): renderable, nothing to do.
        mgr.cache_archive("KTLX", ts(2), archive(4096));
        mgr.cache_scan("KTLX", ts(2), priced_volume());
        // ts(3): never downloaded; a download's job, not a decode's.

        let found = mgr.frames_needing_decode(7);
        assert_eq!(
            found,
            vec![("KTLX".to_string(), ts(0)), ("KTLX".to_string(), ts(1))],
            "the plan walk did not name exactly the evicted and the deferred frame, in plan order"
        );
        // And a frame already on its errand is not named twice.
        mgr.mark_decode_in_flight("KTLX", ts(0));
        assert_eq!(
            mgr.frames_needing_decode(7),
            vec![("KTLX".to_string(), ts(1))]
        );
        assert!(
            mgr.frames_needing_decode(8).is_empty(),
            "an unknown pane has no plan"
        );
    }

    /// **The frame build never decodes.**
    ///
    /// `frame_render_job` is called synchronously from `App::spawn_loop_frame_render`
    /// on the frame thread. For a frame whose moments have been evicted it must
    /// answer `None` — the same "nothing to draw" it gives a frame whose
    /// download has not landed — and leave the rebuild to the pump. A version
    /// that decoded here would put 30-80 MiB of work on the frame thread, and
    /// "it runs rarely" is not an exception to that rule.
    ///
    /// TAMPER: make `frame_render_job` fall back to `archive_for` and decode.
    #[test]
    fn the_frame_build_answers_none_rather_than_decoding() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_archive("KTLX", ts(0), archive(CORPUS_MIN_ARCHIVE));

        let ctx = LoopRenderContext {
            product: RadarProduct::Reflectivity,
            elevation: 0.5,
            lat: 35.3333,
            lon: -97.2778,
            storm_motion: None,
            env_heights: None,
            srv_fallback: crate::srv::SrvFallback::default(),
            melting_layer: None,
            rpg_storm_motion: None,
            surface: crate::jobs::PlanSurface::Raster,
        };

        assert!(
            mgr.frame_render_job("KTLX", &ts(0), &ctx).is_none(),
            "the frame build produced a job for a frame whose moments are absent"
        );
        assert!(
            mgr.frame_data("KTLX", RadarProduct::Reflectivity, &ts(0))
                .is_none(),
            "an archive-only frame reported its data as arrived"
        );
        // And the archive is still there for the pump to rebuild from.
        assert!(mgr.needs_decode("KTLX", &ts(0)));
    }
}
