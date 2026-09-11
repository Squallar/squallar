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

/// **One decoded volume as a census reads it** — see
/// [`LoopDownloadManager::decoded_entries`].
///
/// Borrowed rather than owned: every field is a figure the cache already
/// holds, and a census that cloned the volume out to ask about it would be
/// holding the very allocation it is trying to describe.
pub struct DecodedEntry<'a> {
    /// The site this volume is filed under.
    pub site: &'a str,
    /// The ADDRESS it is filed at, which is not
    /// [`crate::types::volume_collected_at`] — the two are equal on 0 of the
    /// 171 local Archive II volumes.
    pub timestamp: chrono::NaiveDateTime,
    pub scan: &'a Arc<nexrad_model::data::Scan>,
    /// [`crate::scan_size::scan_bytes`], taken at arrival.
    pub bytes: usize,
    /// Whether an archive stands behind it, which is exactly whether either
    /// decoded eviction path may take it at all.
    pub has_archive: bool,
    /// Whether one ever did. `has_archive == false && ever_had_archive` is a
    /// volume whose way back was taken by the archive ceiling: the S3 object
    /// exists, so re-obtaining it is a download. `!ever_had_archive` is a
    /// chunk-feed volume, which nothing can re-obtain until the bucket has it.
    pub ever_had_archive: bool,
}

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
/// The most outstanding ceiling evictions [`LoopDownloadManager`] remembers at
/// once, so an instrument built for a memory campaign is not itself unbounded.
/// One entry is a site string and a stamp; 4096 of them is a few hundred KiB at
/// the very worst, against a realistic occupancy of a few dozen — the set
/// self-prunes the moment a volume is filed again. Past it the set stops
/// growing and `ceiling_churn`'s `saturated` says the return count has become a
/// lower bound.
const CEILING_OUTSTANDING_CAP: usize = 4096;

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
    /// **Where an archive goes instead of being dropped**, or `None` for a
    /// target that has nowhere to put one.
    ///
    /// `None` is the whole of the web target's behaviour here: a `cfg`
    /// selecting a value, never a fork inside a function body. Every path
    /// below is written once and reads `None` as "there is no off-heap", which
    /// is exactly today's behaviour.
    spill: Option<Box<dyn crate::archive_spill::ArchiveSpill>>,
    /// What [`spilled`](Self::spilled) may hold, in bytes on the medium.
    ///
    /// A byte ceiling that moves its overflow somewhere that also runs out is
    /// a leak with a longer fuse, so the spill has its own bound and the
    /// overflow of THAT is a plain drop — today's behaviour — rather than a
    /// second eviction policy to get wrong.
    spill_ceiling: usize,
    /// **The keys of every archive whose bytes are off the heap, with the
    /// length each was written at.**
    ///
    /// This map is the reason the change is small. Every question this type is
    /// asked about an archive is a **key-presence** question — the two decoded
    /// eviction policies both refuse a volume with nothing behind it by
    /// testing `contains_key`, never by touching bytes — so the keys stay
    /// here, in memory, and only a withdrawal of the bytes pays for I/O.
    /// Nothing on a frame path reads a disk.
    spilled: HashMap<String, HashMap<chrono::NaiveDateTime, usize>>,
    /// What [`spilled`](Self::spilled) is holding on the medium, in bytes.
    /// **Not host bytes** and never added to a census level: these are the
    /// bytes that left the heap.
    spill_bytes: usize,
    /// Archives written off-heap instead of dropped. A running total.
    ///
    /// **A cut needs a counter that says it fired.** A ~94 MiB cut on this
    /// campaign delivered exactly zero because its precondition never held on
    /// the arm it ran on, and nothing noticed for a day. This row does not
    /// exist in a tree without the spill, so a base binary prints no such row
    /// rather than a zero that reads like a measurement.
    archives_spilled: u64,
    /// Times the spill was at its ceiling and the archive was dropped instead
    /// — the disk bound biting, and the one state in which this behaves
    /// exactly as a tree with no spill.
    spill_refused_full: u64,
    /// Times the store itself failed: no room, no permission, a site string
    /// that will not go in a path. Distinct from a refusal, which is policy.
    spill_store_failed: u64,
    /// Bytes handed back off the medium, and the withdrawals that made them.
    ///
    /// `AtomicU64` because the two withdrawal paths ([`Self::archive_for`] and
    /// [`Self::archive_for_identity`]) are `&self` — they are asked by the
    /// pump while other fields are borrowed — and a counter is not worth
    /// making them `&mut`.
    spill_restores: std::sync::atomic::AtomicU64,
    /// Withdrawals that found the key here and nothing on the medium. **Zero
    /// on a sound tree**: the key and the file are written and removed
    /// together, so a miss is this type disagreeing with the disk.
    spill_restore_misses: std::sync::atomic::AtomicU64,
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
    /// **What the decoded byte ceiling's eviction pass has actually done, and
    /// how much of it it had to UNDO** — the counters behind
    /// [`Self::ceiling_churn`].
    ///
    /// # Why the cycle is the quantity and the event is not
    ///
    /// `evict_decoded_to_ceiling` frees bytes by throwing away a decoded
    /// volume and keeping its archive, so the cost of being wrong is a decode
    /// (6.0 / 10.7 / 40.9 ms measured) and never a network round trip. That
    /// makes a *single* eviction of a volume nothing ever wants again very
    /// nearly free — it is the policy working. What is NOT free is evicting a
    /// volume the loop immediately needs back: the ceiling then buys its bytes
    /// with a decode, and a ceiling set low enough to do that continuously
    /// buys them over and over. A count of evictions cannot tell those two
    /// apart, and they are the difference between a cut and a regression.
    ///
    /// So `returns` is the figure that matters: an eviction this pass made
    /// that had to be undone. `evictions - returns` is the clean half.
    ///
    /// # `asked` and `over`, for the reason every fires-counter needs them
    ///
    /// A pass that is never called and a pass called on a cache that is
    /// already under the ceiling both evict nothing, and a byte figure reads 0
    /// for both. `asked` counts the calls and `over` the calls that found work,
    /// so "armed and idle" is a positive reading and not an absence — the
    /// distinction a ~94 MiB cut on this campaign lacked when its counter read
    /// 0 B on all 530 ticks.
    ceiling_asks: u64,
    ceiling_over: u64,
    ceiling_evictions: u64,
    ceiling_evicted_bytes: u64,
    ceiling_returns: u64,
    ceiling_returned_bytes: u64,
    /// Keys this pass evicted that have not been filed again. **Self-pruning**:
    /// an entry leaves the moment [`Self::cache_scan`] files its key, which is
    /// the event it exists to detect, so it holds only evictions still
    /// outstanding rather than a history of them.
    ///
    /// Capped at [`CEILING_OUTSTANDING_CAP`] because an instrument on a memory
    /// campaign may not itself be unbounded. Past the cap it stops growing and
    /// `ceiling_returns` becomes a LOWER BOUND, which `saturated` says out
    /// loud rather than letting the figure quietly under-report.
    ceiling_outstanding: std::collections::HashSet<(String, chrono::NaiveDateTime)>,
    ceiling_saturated: bool,
    /// **One physical volume decoded twice, arriving at a second address** —
    /// the counters behind [`Self::identity_duplication`].
    ///
    /// A volume has an address (the `(site, timestamp)` it is filed under) and
    /// an identity ([`crate::types::volume_collected_at`]), and the two are
    /// equal on 0 of the 171 local Archive II volumes. So the chunk feed's
    /// assembled volume and the S3 archive of the SAME sweep are filed under
    /// two addresses, and nothing in this cache asks whether it is already
    /// holding what has just been handed to it.
    ///
    /// Counted at the seam and never sampled on a tick: a duplicate that the
    /// residency pass evicts between two 2 s readings is a real duplicate the
    /// level would report as zero.
    identity_dup: IdentityDuplication,
    /// The price of each cached volume, so [`Self::retain_scans`] subtracts
    /// what it removes instead of re-walking it. Keyed exactly as
    /// [`scan_cache`](Self::scan_cache) is addressed, and every mutation of
    /// one is a mutation of the other.
    scan_prices: HashMap<(String, chrono::NaiveDateTime), usize>,
    /// **Addresses an archive has ever been filed for**, for as long as their
    /// decoded half is here.
    ///
    /// The one thing `archive_cache` cannot answer once its entry has gone:
    /// whether a resident decoded volume with no way back **never had one** —
    /// a chunk-feed arrival, whose S3 object this process has never held — or
    /// **lost it** to [`Self::evict_archives_to_ceiling`], in which case the
    /// object exists and re-obtaining the volume is a download rather than
    /// data that cannot be had at all. Those are two different prices for the
    /// same eviction and a census that could not tell them apart would report
    /// a fidelity cost where the cost is bandwidth.
    ///
    /// A key lives exactly as long as the decoded entry it describes: filed in
    /// [`Self::cache_archive`], dropped wherever the volume's price row is.
    /// Two `String`s and a stamp per resident volume, against a volume that
    /// runs to tens of MiB.
    archives_ever: std::collections::HashSet<(String, chrono::NaiveDateTime)>,
    /// **What the archive at each address DECODES TO**, as the volume's own
    /// first-radial moment — learned from the decoded half while it was here
    /// and kept for as long as the archive is.
    ///
    /// # The two clocks, and why an address alone loses an archive
    ///
    /// A volume has an *address*, the `(site, timestamp)` this cache filed it
    /// under, and an *identity*, [`crate::types::volume_collected_at`]. They
    /// are different instants: an archive arrival is filed under the second
    /// its S3 key names, and over the 171 local Archive II volumes the two are
    /// equal on **0 of 171**, a median 437 ms apart.
    ///
    /// [`Self::retain_scans`] already asks both, because a pane's `scan_info`
    /// may carry either. [`Self::retain_archives`] could not: its key is the
    /// address and the identity lived only on the decoded volume, so an
    /// archive the drain filed for a moment a pane is parked at by IDENTITY
    /// matched nothing and was swept on the very next residency pass —
    /// measured on the real drain, one pass after arrival, on a looping site
    /// where the compressed bytes had already been paid for.
    ///
    /// What that cost is not a wasted download but a volume that can never be
    /// traded: `evict_decoded_except` refuses to evict a volume with no
    /// archive behind it, so the arrival was left resident and un-evictable at
    /// 33.7-82.7 MiB for the life of the process, holding the decoded ceiling
    /// shut against the loop's own frames.
    ///
    /// Filed in [`Self::cache_scan`], which is the one place a volume and its
    /// address are both in hand, and cleaned wherever an archive is removed.
    /// It outlives the DECODED half deliberately — that is the whole point,
    /// since the question is asked exactly when the moments are gone.
    archive_identity: HashMap<(String, chrono::NaiveDateTime), chrono::NaiveDateTime>,
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
    /// **How many times the byte ceiling was told to keep an archive it would
    /// otherwise have evicted** — see [`Self::evict_archives_to_ceiling`]'s
    /// `pinned`.
    ///
    /// A running total, incremented once per pinned archive per pass that was
    /// actually over the ceiling, and never a level: what it exists to say is
    /// whether the pin has ever fired on a running app. The mechanism it
    /// guards has no other witness — a stranded base and a base that was never
    /// released read identically from every other counter — and two counters
    /// on this exact path have shipped reading zero.
    archives_pinned_to_ceiling: u64,
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

/// **How often this cache was handed a volume it was already holding under
/// another address**, and what the second copy cost.
///
/// # Why an address is not an identity
///
/// [`LoopDownloadManager::cache_scan`] files a volume under the
/// `(site, timestamp)` its arrival names. The chunk feed's assembled volume
/// is addressed by the moment the feed closed it; the S3 archive of the same
/// sweep is addressed by the second its object key names. Over the 171 local
/// Archive II volumes those two instants are equal on **0**, a median 437 ms
/// apart, so the same physical volume filed by both routes never collides by
/// luck and this cache holds it twice.
///
/// # Every field's denominator
///
/// * `files` — every [`LoopDownloadManager::cache_scan`] call whose volume
///   states an identity at all. The denominator for all of the below, and
///   the figure that says whether the mechanism was reachable on this leg:
///   a zero here means nothing was cached, not that nothing was duplicated.
/// * `archiveless_files` — of the volumes filed, the ones that arrived with
///   no compressed half. On this tree that is the chunk feed's assembled
///   whole volumes and nothing else, so it is the **reachability
///   denominator**: a duplicate needs one of these resident before an archive
///   of the same sweep arrives, and a `dup 0` beside an `archiveless_files 0`
///   says the mechanism never had a chance rather than that it has none.
/// * `duplicates` — of those, files landing on a DIFFERENT address whose
///   volume this cache is still holding at the same identity.
/// * `same_allocation` — duplicates whose arriving `Arc` **is** the resident
///   one, pointer-equal. One allocation under two keys: a merge over these
///   frees nothing at all, and the count exists so the two readings can never
///   be confused for each other. This campaign has read `sole 0` on three
///   cuts whose bytes looked certain, and this is the field that tells the
///   two apart before a line of the cut is written.
/// * `twin_archiveless` — duplicates whose resident twin has no archive
///   behind it, so [`LoopDownloadManager::evict_decoded_except`] refuses to
///   evict it. These are the chunk feed's assembled volumes.
///
/// # Two prices, because there are two merges and they free different bytes
///
/// * `arrival_bytes` / `arrival_sole_bytes` — the arriving copy's price, and
///   the part of it on arrivals **no other holder names**. Declining to file
///   one frees `arrival_sole_bytes`; the rest is a refcount.
/// * `twin_bytes` / `twin_sole_bytes` — the RESIDENT copy's price, and the
///   part of it this cache holds alone. Evicting the superseded twin frees
///   `twin_sole_bytes`.
///
/// The two are counted separately and are never summed: they are the same
/// physical volume, and only one of the two copies can be dropped.
///
/// # What the merge does, and what it is worth
///
/// * `merges` — resident twins handed the arriving volume's compressed half
///   by [`LoopDownloadManager::share_archive_with_twin`], which is what turns
///   an un-evictable chunk-fed volume into one the residency pass may trade.
/// * `merge_bytes` — those twins' decoded prices, summed. An UPPER BOUND on
///   what the merge frees and never a claim that it was freed: the residency
///   pass still has to want the volume gone, and a twin some other store also
///   names frees a refcount when it goes. `twin_sole_bytes` above is the part
///   that was already sole AT THE SEAM, and it is measured 0 on the real
///   arrival — the chunk feed's own volume is the site's merge base at the
///   moment its archive lands, and only stops being one when the base
///   advances.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdentityDuplication {
    pub files: u64,
    pub archiveless_files: u64,
    pub duplicates: u64,
    pub arrival_bytes: u64,
    pub arrival_sole_bytes: u64,
    pub twin_bytes: u64,
    pub twin_sole_bytes: u64,
    pub same_allocation: u64,
    pub twin_archiveless: u64,
    pub merges: u64,
    pub merge_bytes: u64,
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
            spill: None,
            spill_ceiling: 0,
            spilled: HashMap::new(),
            spill_bytes: 0,
            archives_spilled: 0,
            spill_refused_full: 0,
            spill_store_failed: 0,
            spill_restores: std::sync::atomic::AtomicU64::new(0),
            spill_restore_misses: std::sync::atomic::AtomicU64::new(0),
            archive_bytes_cached: 0,
            archives_pinned_to_ceiling: 0,
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
            ceiling_asks: 0,
            ceiling_over: 0,
            ceiling_evictions: 0,
            ceiling_evicted_bytes: 0,
            ceiling_returns: 0,
            ceiling_returned_bytes: 0,
            ceiling_outstanding: std::collections::HashSet::new(),
            ceiling_saturated: false,
            identity_dup: IdentityDuplication::default(),
            scan_prices: HashMap::new(),
            archives_ever: std::collections::HashSet::new(),
            archive_identity: HashMap::new(),
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
        // **A volume the decoded ceiling threw away, arriving back.** The
        // pass bought its bytes with a decode and this is the decode being
        // paid for, so it is counted here rather than inferred from a later
        // eviction: `evictions - returns` is then the half that cost nothing.
        if self.ceiling_outstanding.remove(&(site.to_string(), ts)) {
            self.ceiling_returns = self.ceiling_returns.saturating_add(1);
            self.ceiling_returned_bytes = self.ceiling_returned_bytes.saturating_add(price as u64);
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
        // **The identity, learned here because here is the one place both
        // halves are in hand.** Kept past the volume's own eviction: see
        // `archive_identity`.
        if let Some(collected) = crate::types::volume_collected_at(&volume.0) {
            // Asked before the insert, so the walk below cannot find the
            // arrival itself, and before `archive_identity` learns this
            // address for the same reason.
            self.note_identity_duplication(site, ts, &volume.0, price, collected);
            self.archive_identity
                .insert((site.to_string(), ts), collected);
        }
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
        self.archives_ever.insert((site.to_string(), ts));
        // A heap copy supersedes an off-heap one, or the two would both be
        // counted and the medium would keep bytes nothing will ever read.
        self.drop_spilled(site, &ts);
    }

    /// **Give this manager somewhere to put an archive instead of dropping
    /// it**, bounded by `ceiling` bytes on the medium.
    ///
    /// Not a constructor argument: every existing caller and every existing
    /// test builds a manager with no spill and keeps today's behaviour, so the
    /// off-heap path is opt-in at the one place that owns a directory.
    pub fn set_spill(
        &mut self,
        spill: Box<dyn crate::archive_spill::ArchiveSpill>,
        ceiling: usize,
    ) {
        self.spill = Some(spill);
        self.spill_ceiling = ceiling;
    }

    /// Whether the compressed archive for this frame is held, whatever the
    /// decoded half is doing.
    ///
    /// **On the heap or off it.** This is the one predicate every way-back
    /// question in this type goes through — both decoded-eviction refusals,
    /// the decode pump, the twin diagnostics — precisely so that moving bytes
    /// off the heap cannot make one of them disagree with another about
    /// whether a volume can still be rebuilt. It is two `HashMap` lookups and
    /// touches no medium.
    pub fn has_archive(&self, site: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.archive_cache
            .get(site)
            .is_some_and(|archives| archives.contains_key(ts))
            || self.is_spilled(site, ts)
    }

    /// Whether this manager has anywhere to put an archive at all.
    ///
    /// Read by the telemetry row, which must be able to tell "armed and did
    /// not fire" from "no spill on this target" — a counter that cannot
    /// distinguish those is how a cut reads as delivered when its precondition
    /// never held.
    pub fn has_spill(&self) -> bool {
        self.spill.is_some()
    }

    /// Whether this frame's archive is the off-heap one. A key lookup.
    pub fn is_spilled(&self, site: &str, ts: &chrono::NaiveDateTime) -> bool {
        self.spilled
            .get(site)
            .is_some_and(|keys| keys.contains_key(ts))
    }

    /// Bytes the spill is holding on its medium. **Not host bytes**, and never
    /// added to a census level — these are the bytes that left the heap.
    pub fn spilled_bytes(&self) -> usize {
        self.spill_bytes
    }

    /// How many archives are off-heap right now.
    pub fn spilled_count(&self) -> usize {
        self.spilled.values().map(HashMap::len).sum()
    }

    /// **Did the spill fire, and did anything come back out of it**:
    /// `(spilled, refused_full, store_failed, restored, restore_misses)`.
    /// Running totals; a line of their own and never a census level.
    pub fn spill_counts(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.archives_spilled,
            self.spill_refused_full,
            self.spill_store_failed,
            self.spill_restores
                .load(std::sync::atomic::Ordering::Relaxed),
            self.spill_restore_misses
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// The bytes back off the medium, as a fresh allocation for the decode job.
    ///
    /// **Not re-inserted into [`archive_cache`](Self::archive_cache)**: the
    /// point of the spill is that these bytes are not on the heap, and the job
    /// funnel moves the `Arc` by pointer and drops it when the decode lands.
    /// Re-filing it here would undo the cut on the next pass.
    fn withdraw_spilled(&self, site: &str, ts: &chrono::NaiveDateTime) -> Option<Arc<Vec<u8>>> {
        use std::sync::atomic::Ordering::Relaxed;
        if !self.is_spilled(site, ts) {
            return None;
        }
        match self.spill.as_ref()?.load(site, *ts) {
            Some(bytes) => {
                self.spill_restores.fetch_add(1, Relaxed);
                Some(Arc::new(bytes))
            }
            None => {
                // The key is here and the medium is not. Counted rather than
                // silently degraded: it means this type and the disk disagree.
                self.spill_restore_misses.fetch_add(1, Relaxed);
                None
            }
        }
    }

    /// Forget one spilled archive, on the medium and in the index together.
    fn drop_spilled(&mut self, site: &str, ts: &chrono::NaiveDateTime) {
        let Some(keys) = self.spilled.get_mut(site) else {
            return;
        };
        if let Some(len) = keys.remove(ts) {
            self.spill_bytes = self.spill_bytes.saturating_sub(len);
            if let Some(spill) = self.spill.as_ref() {
                spill.delete(site, *ts);
            }
        }
        if keys.is_empty() {
            self.spilled.remove(site);
        }
    }

    /// The archive for this frame, as a pointer to hand the decode job.
    ///
    /// The heap first, then the medium. **This is one of the two places that
    /// pays for the spill** — and it is a path that already precedes a bzip2
    /// decode, measured on the production path at 4.7 ms to 41.3 ms
    /// single-threaded over the 208-volume corpus, against a cold read of
    /// 1.23 ms to 55.18 ms over the same volumes. The read is the LARGER half
    /// at the top of that range: a restore is I/O-bound. See
    /// `archive_spill`, which carries the table and the retraction of the
    /// 19.3-915.8 ms decode figures this comment used to quote.
    pub fn archive_for(&self, site: &str, ts: &chrono::NaiveDateTime) -> Option<Arc<Vec<u8>>> {
        if let Some(held) = self.archive_cache.get(site).and_then(|a| a.get(ts)) {
            return Some(Arc::clone(held));
        }
        self.withdraw_spilled(site, ts)
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
            spilled,
            scan_prices,
            scan_bytes_cached,
            archives_ever,
            ..
        } = self;
        let mut removed = Vec::new();
        let mut gone: Vec<(String, chrono::NaiveDateTime)> = Vec::new();
        scan_cache.retain(|site, scans| {
            removed.extend(
                scans
                    .extract_if(|ts, (scan, _)| {
                        // On the heap or off it: a spilled archive is a way
                        // back, so the volume in front of it stays evictable.
                        // Spelled out rather than through `has_archive`
                        // because `self` is destructured here for the disjoint
                        // borrow the closure needs.
                        let rebuildable = archive_cache
                            .get(site.as_str())
                            .is_some_and(|archives| archives.contains_key(ts))
                            || spilled
                                .get(site.as_str())
                                .is_some_and(|keys| keys.contains_key(ts));
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
            archives_ever.remove(&key);
        }
        removed
    }

    /// **Hold the DECODED volumes inside a BYTE ceiling**, evicting the ones
    /// furthest from a playhead first and keeping their archives. Hands the
    /// removed volumes back owned, for the deferred-drop path.
    ///
    /// # The gap this closes
    ///
    /// `LOOP_DECODED_CEILING_BYTES` was an **admission** gate and nothing
    /// else: `decoded_room_for` refuses a new decode over the ceiling, and no
    /// path took the cache back down to it once it was over. It gets over in
    /// the first place because [`Self::cache_scan`] is ungated — every archive
    /// drain arrival (the pane fetch, the auto-poll, the adjacent-volume
    /// nudge) and every chunk-feed volume is filed here with no admission
    /// check at all, which is right, because those volumes are on screen. The
    /// consequence was not: a cache pushed past the ceiling by arrivals it
    /// cannot refuse stayed past it, and the loop's own frames — which *can*
    /// be traded for their archives at a median 5.8 % of the decoded cost —
    /// were never asked to make the room.
    ///
    /// The archive half has had this bound since `d5b2dbe4e`
    /// ([`Self::evict_archives_to_ceiling`]); this is its twin, and the
    /// asymmetry between them was the defect.
    ///
    /// # What it will not evict
    ///
    /// Two hold-backs, and each is asked of the **entry**, never of the set:
    ///
    /// * **A volume with no archive behind it**, [`Self::evict_decoded_except`]'s
    ///   premise and for its reason — this policy's cost is a decode, and for
    ///   a volume with nothing to decode from it would silently become a
    ///   re-download policy.
    /// * **A volume `pinned` says something is drawing right now.** The
    ///   caller answers that off the volume's own clocks, because a pane's
    ///   `scan_info` may carry either the address or the identity. A pinned
    ///   volume is skipped whatever the ceiling says: the ceiling may never
    ///   take a picture off the glass.
    ///
    /// So the ceiling is a **target and not a guarantee**, exactly as the
    /// archive twin's is. A cache that is entirely pinned stays over it, and
    /// that is the correct failure — the alternative is a blank pane.
    ///
    /// `rank` is asked per held volume and orders eviction: **higher is
    /// evicted first**, so a caller passes distance from the nearest
    /// playhead. An evicted volume comes back through the decode arm of the
    /// pump ([`Self::frames_needing_decode`]) at no network cost, which is why
    /// the cost of being wrong here is a wait and never a gap.
    ///
    /// O(held) and only when over the ceiling; held is a few dozen.
    pub fn evict_decoded_to_ceiling(
        &mut self,
        ceiling: usize,
        rank: impl Fn(&str, &chrono::NaiveDateTime) -> u64,
        pinned: impl Fn(&str, &chrono::NaiveDateTime, &nexrad_model::data::Scan) -> bool,
    ) -> Vec<CachedVolume> {
        // **Asked**, counted before the early return so a pass that found
        // nothing to do is a positive reading rather than the same 0 a pass
        // that never ran prints.
        self.ceiling_asks = self.ceiling_asks.saturating_add(1);
        if self.scan_bytes_cached <= ceiling {
            return Vec::new();
        }
        self.ceiling_over = self.ceiling_over.saturating_add(1);
        // Candidates only: an entry with no archive or one something is
        // drawing is not a candidate at all, so it is never ranked and can
        // never be reached by the walk below however far over the ceiling
        // this cache is.
        let mut held: Vec<(u64, String, chrono::NaiveDateTime, usize)> = Vec::new();
        for (site, scans) in &self.scan_cache {
            for (ts, (scan, _)) in scans {
                // The way-back predicate, on the heap or off it: this is the
                // second policy that refuses a volume with nothing behind it.
                if !self.has_archive(site.as_str(), ts) {
                    continue;
                }
                if pinned(site.as_str(), ts, scan) {
                    continue;
                }
                let price = self
                    .scan_prices
                    .get(&(site.clone(), *ts))
                    .copied()
                    .unwrap_or(0);
                held.push((rank(site.as_str(), ts), site.clone(), *ts, price));
            }
        }
        // Furthest first, and the stamp breaks a tie so two volumes at equal
        // distance evict in a fixed order rather than in map order.
        held.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)));
        let mut removed = Vec::new();
        for (_, site, ts, price) in held {
            if self.scan_bytes_cached <= ceiling {
                break;
            }
            let Some(scans) = self.scan_cache.get_mut(&site) else {
                continue;
            };
            let Some(volume) = scans.remove(&ts) else {
                continue;
            };
            if scans.is_empty() {
                self.scan_cache.remove(&site);
            }
            // The price row leaves with the volume, so a later re-decode
            // files a fresh one rather than double-charging this cache.
            self.scan_prices.remove(&(site.clone(), ts));
            self.archives_ever.remove(&(site.clone(), ts));
            self.scan_bytes_cached = self.scan_bytes_cached.saturating_sub(price);
            self.ceiling_evictions = self.ceiling_evictions.saturating_add(1);
            self.ceiling_evicted_bytes = self.ceiling_evicted_bytes.saturating_add(price as u64);
            // **Outstanding until it comes back**, which is what makes the
            // return detectable at all. Past the cap the set stops growing and
            // says so, rather than becoming an unbounded history.
            if self.ceiling_outstanding.len() < CEILING_OUTSTANDING_CAP {
                self.ceiling_outstanding.insert((site, ts));
            } else {
                self.ceiling_saturated = true;
            }
            removed.push(volume);
        }
        removed
    }

    /// **Every decoded volume this cache holds, as the census reads it**: where
    /// it is filed, its allocation, what it was priced at, and whether an
    /// archive stands behind it.
    ///
    /// The one question this answers that [`Self::cached_scan_allocations`]
    /// cannot is `has_archive`, which is the whole of whether
    /// [`Self::evict_decoded_except`] and [`Self::evict_decoded_to_ceiling`]
    /// are even allowed to consider the entry. It hands back the volume by
    /// reference so a caller's own residency predicate — the very closure it
    /// passed to the eviction — can be asked of the entries that survived it.
    ///
    /// No walk of any radial: the price is the one taken at arrival and the
    /// archive question is a map lookup per entry, over a cache bounded by
    /// frame count.
    pub fn decoded_entries(&self) -> impl Iterator<Item = DecodedEntry<'_>> + '_ {
        self.scan_cache.iter().flat_map(move |(site, scans)| {
            scans.iter().map(move |(ts, (scan, _))| DecodedEntry {
                site: site.as_str(),
                timestamp: *ts,
                scan,
                bytes: self
                    .scan_prices
                    .get(&(site.clone(), *ts))
                    .copied()
                    .unwrap_or(0),
                has_archive: self.has_archive(site.as_str(), ts),
                ever_had_archive: self.archives_ever.contains(&(site.clone(), *ts)),
            })
        })
    }

    /// **Every decoded volume this cache holds, as its allocation and what it
    /// was priced at** — for a caller measuring how much of `loop scans`
    /// another family names too.
    ///
    /// Pointers and not volumes: the question is identity, no gate is read,
    /// and a caller that walked the radials again would be paying
    /// [`crate::scan_size::scan_bytes`] a second time for a figure this cache
    /// already took at arrival.
    pub fn cached_scan_allocations(
        &self,
    ) -> impl Iterator<Item = (*const nexrad_model::data::Scan, usize)> + '_ {
        self.scan_cache.iter().flat_map(move |(site, scans)| {
            scans.iter().map(move |(ts, (scan, _))| {
                (
                    Arc::as_ptr(scan),
                    self.scan_prices
                        .get(&(site.clone(), *ts))
                        .copied()
                        .unwrap_or(0),
                )
            })
        })
    }

    /// **The compressed bytes a volume with this IDENTITY was decoded from**,
    /// found through the address the cache filed it under.
    ///
    /// The reverse of what `archive_identity` is keyed by, and the direction a
    /// restore asks in: a holder that keeps a volume by its own first radial —
    /// `VolumeInventory`'s merge base does — knows the identity and not the
    /// address, and the two are equal on 0 of the 171 local Archive II
    /// volumes. Without this a released base could not find its own bytes.
    ///
    /// Returns the ADDRESS as well as the bytes: every other entry point into
    /// this cache is keyed by the address, so a caller holding only the
    /// archive could not ask whether the decode it wants is already held or
    /// already in flight.
    ///
    /// A linear walk of the index, which is one entry per held archive: a few
    /// dozen, and this runs where a restore is dispatched rather than on a
    /// frame.
    pub fn archive_for_identity(
        &self,
        site: &str,
        collected: chrono::NaiveDateTime,
    ) -> Option<(chrono::NaiveDateTime, Arc<Vec<u8>>)> {
        // **Every address for this identity is tried, not the first one.**
        // One physical volume can be filed at two addresses — the chunk
        // feed's own clock and the second an S3 key names, equal on 0 of the
        // 171 local Archive II volumes — and only one of them may be holding
        // the compressed half. A `find` that stopped at the first match and
        // then asked the archive map answered `None` for a volume this cache
        // was holding the bytes of, with which of the two came out of the map
        // first deciding it.
        // Through `archive_for`, so an address whose bytes are off the heap
        // is a way back exactly as a heap-held one is.
        self.archive_identity
            .iter()
            .filter(|((held_site, _), identity)| {
                held_site.as_str() == site && **identity == collected
            })
            .find_map(|((_, at), _)| self.archive_for(site, at).map(|archive| (*at, archive)))
    }

    /// **Every address this cache has learned decodes to `collected`, split
    /// by what still stands behind it**: `(learned, with_archive,
    /// with_decoded)`.
    ///
    /// The diagnostic beside [`Self::archive_for_identity`], which answers
    /// "is there a way back" with one address or a `None`. That `None` has
    /// three causes a byte figure cannot separate — no address for this
    /// identity was ever learned, one was and its compressed half has since
    /// gone, or one holds its compressed half and the search did not reach
    /// it. Reads nothing this type does not already hold and mutates
    /// nothing.
    ///
    /// A linear walk of the identity index, one entry per filed address, and
    /// it runs where the withdrawal above runs rather than on a frame.
    pub fn identity_way_backs(
        &self,
        site: &str,
        collected: chrono::NaiveDateTime,
    ) -> (usize, usize, usize) {
        let mut learned = 0usize;
        let mut with_archive = 0usize;
        let mut with_decoded = 0usize;
        for ((held_site, held_ts), identity) in &self.archive_identity {
            if held_site.as_str() != site || *identity != collected {
                continue;
            }
            learned = learned.saturating_add(1);
            if self.has_archive(site, held_ts) {
                with_archive = with_archive.saturating_add(1);
                if self
                    .scan_cache
                    .get(site)
                    .is_some_and(|scans| scans.contains_key(held_ts))
                {
                    with_decoded = with_decoded.saturating_add(1);
                }
            }
        }
        (learned, with_archive, with_decoded)
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
    pub fn retain_archives(
        &mut self,
        keep: impl Fn(&str, &chrono::NaiveDateTime, Option<chrono::NaiveDateTime>) -> bool,
    ) {
        // **The spill follows the frame list down as well as up.** An archive
        // the loop has stopped naming leaves the medium on the same pass and
        // by the same predicate that drops the heap-held ones — this, and the
        // refusal to spill past the spill's own ceiling, are what bound the
        // disk over a long session without a second eviction policy.
        let unwanted: Vec<(String, chrono::NaiveDateTime)> = self
            .spilled
            .iter()
            .flat_map(|(site, keys)| keys.keys().map(|ts| (site.clone(), *ts)))
            .filter(|(site, ts)| {
                let collected = self.archive_identity.get(&(site.clone(), *ts)).copied();
                !keep(site.as_str(), ts, collected)
            })
            .collect();
        for (site, ts) in unwanted {
            self.drop_spilled(&site, &ts);
            self.archive_identity.remove(&(site.clone(), ts));
            if !self
                .scan_cache
                .get(site.as_str())
                .is_some_and(|scans| scans.contains_key(&ts))
            {
                self.archives_ever.remove(&(site, ts));
            }
        }

        let Self {
            archive_cache,
            archive_identity,
            archive_bytes_cached,
            scan_cache,
            archives_ever,
            ..
        } = self;
        let mut freed = 0usize;
        let mut gone: Vec<(String, chrono::NaiveDateTime)> = Vec::new();
        archive_cache.retain(|site, archives| {
            archives.retain(|ts, archive| {
                // **Resolved HERE and not by the caller.** The map is this
                // type's, and a caller that wanted to ask it would have to
                // borrow the manager immutably while this method holds it
                // mutably. Resolving it inside is also what keeps the two
                // predicates honest: `retain_scans` asks the volume for its
                // identity, this asks the index, and neither caller has to
                // know which.
                let collected = archive_identity.get(&(site.clone(), *ts)).copied();
                let kept = keep(site.as_str(), ts, collected);
                if !kept {
                    freed = freed.saturating_add(Self::archive_price(archive));
                    gone.push((site.clone(), *ts));
                }
                kept
            });
            !archives.is_empty()
        });
        for key in gone {
            archive_identity.remove(&key);
            // **The memory of having had one outlives the archive, but only
            // while the volume it describes is here.** With both halves gone
            // the key answers a question nobody can ask, and keeping it would
            // let this set grow with every stamp a long session ever listed.
            if !scan_cache
                .get(key.0.as_str())
                .is_some_and(|scans| scans.contains_key(&key.1))
            {
                archives_ever.remove(&key);
            }
        }
        *archive_bytes_cached = archive_bytes_cached.saturating_sub(freed);
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
    /// # `pinned`, and why it is not optional
    ///
    /// **For one archive it is not a re-download, it is nothing at all.** A
    /// merge base whose gates `App::release_unneeded_base_gates` has withdrawn
    /// has exactly one way back — the compressed bytes held here — and
    /// `App::ensure_base_whole` has nowhere else to ask. Evicting that archive
    /// does not cost the frame a download; it leaves a base that can never be
    /// made whole again, and a cross-section or 3D pane that asks for its
    /// gates waits forever.
    ///
    /// Worse, on the plainest scene there is that archive is the FIRST one this
    /// pass reaches. Where no live loop frame names the released base's volume
    /// — every site that is not looping, which is REST1's own shape — the
    /// caller's distance rank has no entry for it, answers `u64::MAX`, and the
    /// sort puts it at the head of the eviction order ahead of every named
    /// frame. A looping site whose newest frame IS the base is ranked
    /// ordinarily and is not the first to go, so the hazard is not universal;
    /// it is also not a corner, because the unlooped case is the common one and
    /// it is the worst-ranked case rather than the best.
    ///
    /// It is a candidate FILTER and not a stop, mirroring
    /// [`Self::evict_decoded_to_ceiling`]'s: a pinned archive is never ranked,
    /// so it can never be reached however far over the ceiling this cache is,
    /// and the pass goes on evicting whatever else it can. A cache that is over
    /// the ceiling in pinned archives alone stays over it, which is the correct
    /// direction — the alternative is a stranded volume.
    ///
    /// O(held) and only when over the ceiling; held is a few dozen.
    /// **Write one archive to the medium and file its key**, or say it did not
    /// happen.
    ///
    /// `false` for three distinct reasons, counted apart because they are not
    /// the same fact: there is no spill on this target, the spill is at its
    /// ceiling (policy — the disk bound biting), or the store itself failed
    /// (no room, no permission, a site string that will not go in a path). The
    /// caller drops the archive on any of them, which is what a tree with no
    /// spill does on every one.
    fn spill_archive(
        &mut self,
        site: &str,
        ts: chrono::NaiveDateTime,
        archive: &Arc<Vec<u8>>,
    ) -> bool {
        if self.spill.is_none() {
            return false;
        }
        let len = archive.len();
        // The bound is checked before the write, so the medium never exceeds
        // the ceiling even briefly.
        if self.spill_bytes.saturating_add(len) > self.spill_ceiling {
            self.spill_refused_full = self.spill_refused_full.saturating_add(1);
            return false;
        }
        let stored = self
            .spill
            .as_ref()
            .is_some_and(|spill| spill.store(site, ts, archive.as_slice()));
        if !stored {
            self.spill_store_failed = self.spill_store_failed.saturating_add(1);
            return false;
        }
        self.spilled
            .entry(site.to_string())
            .or_default()
            .insert(ts, len);
        self.spill_bytes = self.spill_bytes.saturating_add(len);
        self.archives_spilled = self.archives_spilled.saturating_add(1);
        true
    }

    pub fn evict_archives_to_ceiling(
        &mut self,
        ceiling: usize,
        rank: impl Fn(&str, &chrono::NaiveDateTime) -> u64,
        pinned: impl Fn(&str, &chrono::NaiveDateTime, Option<chrono::NaiveDateTime>) -> bool,
    ) -> usize {
        if self.archive_bytes_cached <= ceiling {
            return 0;
        }
        let mut held: Vec<(u64, String, chrono::NaiveDateTime, usize)> = Vec::new();
        // A plain walk and not an iterator chain: `rank` is borrowed by every
        // entry, and a nested `map` would have to move it.
        for (site, archives) in &self.archive_cache {
            for (ts, archive) in archives {
                // The identity is resolved HERE, for
                // [`Self::retain_archives`]' reason: the index is this type's
                // and a caller that wanted to ask it would have to borrow the
                // manager immutably while this method holds it mutably. A
                // released base is keyed by its volume's own identity and the
                // archive by the address the listing gave it, and on a
                // chunk-fed volume those are not the same instant.
                let collected = self.archive_identity.get(&(site.clone(), *ts)).copied();
                if pinned(site.as_str(), ts, collected) {
                    self.archives_pinned_to_ceiling =
                        self.archives_pinned_to_ceiling.saturating_add(1);
                    continue;
                }
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
            let mut now_empty = false;
            let removed = match self.archive_cache.get_mut(&site) {
                Some(archives) => {
                    let taken = archives.remove(&ts);
                    now_empty = archives.is_empty();
                    taken
                }
                None => None,
            };
            let Some(archive) = removed else {
                continue;
            };
            // The heap bytes are gone either way — that is the cut, and it is
            // the figure `squallar_alloc::live_bytes` moves on.
            self.archive_bytes_cached = self.archive_bytes_cached.saturating_sub(price);
            freed = freed.saturating_add(price);
            if now_empty {
                self.archive_cache.remove(&site);
            }
            // **Off the heap rather than gone.** A spilled archive is still a
            // way back, so the decoded volume it stands behind — a median
            // 15.5x larger — stays evictable instead of being stranded
            // un-evictable, which is what dropping it does. The archive
            // identity therefore STAYS: it is the index the way back is found
            // through, and only a real drop may take it.
            if self.spill_archive(&site, ts, &archive) {
                continue;
            }
            // Nowhere to put it, or no room there: today's behaviour exactly.
            // The index is the archive's, not the volume's, so it leaves
            // with the archive and never with the moments.
            self.archive_identity.remove(&(site.clone(), ts));
            if !self
                .scan_cache
                .get(site.as_str())
                .is_some_and(|scans| scans.contains_key(&ts))
            {
                self.archives_ever.remove(&(site.clone(), ts));
            }
        }
        freed
    }

    /// **How many archives the byte ceiling has been told to keep** because a
    /// released merge base's only way back runs through them. A running total;
    /// see the field.
    pub fn archives_pinned_to_ceiling(&self) -> u64 {
        self.archives_pinned_to_ceiling
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

    /// **Score one arrival against what this cache is already holding at the
    /// same identity**, for [`Self::identity_duplication`]. Called from
    /// [`Self::cache_scan`] before the arrival is filed.
    ///
    /// `holders` is read off the arriving `Arc` while this cache still holds
    /// none of it, so `1` is "the caller has let go and the cache is about to
    /// become the only reference" — the state in which declining to file the
    /// volume frees its whole price rather than a refcount.
    ///
    /// A linear walk of `archive_identity`, which is one entry per address
    /// this cache has filed: a few dozen, once per decode.
    fn note_identity_duplication(
        &mut self,
        site: &str,
        ts: chrono::NaiveDateTime,
        arriving: &Arc<nexrad_model::data::Scan>,
        price: usize,
        collected: chrono::NaiveDateTime,
    ) {
        self.identity_dup.files = self.identity_dup.files.saturating_add(1);
        let twin = self
            .archive_identity
            .iter()
            .find(|((held_site, held_ts), identity)| {
                held_site.as_str() == site
                    && *held_ts != ts
                    && **identity == collected
                    && self
                        .scan_cache
                        .get(site)
                        .is_some_and(|scans| scans.contains_key(held_ts))
            });
        let Some(((_, twin_ts), _)) = twin else {
            return;
        };
        let twin_ts = *twin_ts;
        // Pointer and holder count off the RESIDENT copy: what evicting the
        // superseded twin would free is its own sole-ness, not the arrival's.
        let (resident, twin_holders) = self
            .scan_cache
            .get(site)
            .and_then(|scans| scans.get(&twin_ts))
            .map_or((None, 0), |(scan, _)| {
                (Some(Arc::as_ptr(scan)), Arc::strong_count(scan))
            });
        let twin_price = self
            .scan_prices
            .get(&(site.to_string(), twin_ts))
            .copied()
            .unwrap_or(0);
        let arrival_holders = Arc::strong_count(arriving);
        // Read before the counters are borrowed mutably, and once for both
        // readers below: one evaluation of the way-back predicate, one
        // spelling of it.
        let twin_has_archive = self.has_archive(site, &twin_ts);
        let dup = &mut self.identity_dup;
        dup.duplicates = dup.duplicates.saturating_add(1);
        dup.arrival_bytes = dup.arrival_bytes.saturating_add(price as u64);
        dup.twin_bytes = dup.twin_bytes.saturating_add(twin_price as u64);
        if resident == Some(Arc::as_ptr(arriving)) {
            dup.same_allocation = dup.same_allocation.saturating_add(1);
        } else {
            // The arrival: this cache holds none of it yet, so `1` is the
            // caller having let go.
            if arrival_holders == 1 {
                dup.arrival_sole_bytes = dup.arrival_sole_bytes.saturating_add(price as u64);
            }
            // The twin: this cache is already one of its holders, so `1` is
            // the cache alone.
            if twin_holders == 1 {
                dup.twin_sole_bytes = dup.twin_sole_bytes.saturating_add(twin_price as u64);
            }
        }
        if !twin_has_archive {
            dup.twin_archiveless = dup.twin_archiveless.saturating_add(1);
        }
        log::debug!(
            "{site}: a volume collected at {collected} arrived at {ts} while this cache \
             already holds it at {twin_ts} (arrival {} B/{} holder(s), twin {} B/{} holder(s), \
             twin archive {})",
            price,
            arrival_holders,
            twin_price,
            twin_holders,
            if twin_has_archive { "held" } else { "absent" },
        );
    }

    /// **Give a resident copy of this same physical volume the compressed half
    /// it never had**, from the archive that has just arrived for it under the
    /// other clock. Returns whether a twin was found.
    ///
    /// # Why there is a twin at all
    ///
    /// A volume's ADDRESS is the `(site, timestamp)` it was filed under and
    /// its IDENTITY is [`crate::types::volume_collected_at`]. The chunk feed
    /// closes a whole volume and files it at one; the S3 object for the same
    /// sweep is filed at the second its key names. Measured on a real
    /// six-site leg: KINX's 21:07:05.736 volume arrived again at 21:07:05,
    /// 736 ms apart, **pointer-distinct** — two decodes of one sweep, 58.1
    /// MiB each. Over the 171 local Archive II volumes the two clocks are
    /// equal on 0, so this never collides by luck.
    ///
    /// # Why the archive and not the volume
    ///
    /// The arriving decoded half cannot simply be dropped: the loop frame the
    /// listing appended names the ARRIVAL's address, and a frame whose volume
    /// is not cached is one the pump downloads again. What can be moved at no
    /// cost is the compressed half, which is an `Arc` clone and not a copy of
    /// 1.0-16.1 MiB. With it the twin becomes `rebuildable`, and
    /// [`Self::evict_decoded_except`] — which refuses a volume with nothing to
    /// decode from, and for the chunk feed's volumes therefore refuses
    /// forever — may trade it on the next residency pass.
    ///
    /// **The archive is priced under both addresses** while both stand. One
    /// allocation, two rows: `archive_bytes_cached` is a retention bound and
    /// over-counting inside it can only evict earlier, never later, and
    /// [`Self::evict_archives_to_ceiling`] degrades to a re-download, which is
    /// what a frame costs today. Under-counting could hold the ceiling open
    /// on bytes nobody had budgeted, which is the direction that cannot be
    /// allowed.
    ///
    /// Skips a twin that already has an archive: it is already tradeable, and
    /// re-filing would replace a live buffer for nothing.
    ///
    /// A linear walk of the identity index — one entry per filed address, a
    /// few dozen — once per arrival that carries an archive.
    pub fn share_archive_with_twin(
        &mut self,
        site: &str,
        ts: chrono::NaiveDateTime,
        archive: &Arc<Vec<u8>>,
    ) -> bool {
        let Some(collected) = self.archive_identity.get(&(site.to_string(), ts)).copied() else {
            return false;
        };
        let twin = self
            .archive_identity
            .iter()
            .find(|((held_site, held_ts), identity)| {
                held_site.as_str() == site
                    && *held_ts != ts
                    && **identity == collected
                    && self
                        .scan_cache
                        .get(site)
                        .is_some_and(|scans| scans.contains_key(held_ts))
                    && !self.has_archive(site, held_ts)
            })
            .map(|((_, held_ts), _)| *held_ts);
        let Some(twin_ts) = twin else {
            return false;
        };
        let price = self
            .scan_prices
            .get(&(site.to_string(), twin_ts))
            .copied()
            .unwrap_or(0);
        self.identity_dup.merges = self.identity_dup.merges.saturating_add(1);
        self.identity_dup.merge_bytes = self.identity_dup.merge_bytes.saturating_add(price as u64);
        log::debug!(
            "{site}: the volume at {twin_ts} is the one that arrived at {ts}, so it takes \
             that archive and becomes evictable ({price} B decoded)"
        );
        self.cache_archive(site, twin_ts, Arc::clone(archive));
        true
    }

    /// **Note a volume filed with no compressed half**, the reachability
    /// denominator for [`Self::identity_duplication`]. Called by the app
    /// beside [`Self::cache_scan`], because whether an arrival brought an
    /// archive is the caller's fact and never this cache's.
    pub fn note_archiveless_file(&mut self) {
        self.identity_dup.archiveless_files = self.identity_dup.archiveless_files.saturating_add(1);
    }

    /// **One physical volume decoded twice**, as counters taken at the seam.
    ///
    /// Always on, and reported whether or not anything gates on it.
    pub fn identity_duplication(&self) -> IdentityDuplication {
        self.identity_dup
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

    /// **What the decoded ceiling's eviction pass has done and had to undo**,
    /// as running totals for the life of the process — never a level, which is
    /// why they are reported on a line of their own and never added to
    /// `LoopDecodedCensus`.
    ///
    /// `(asked, over, evictions, evicted_bytes, returns, returned_bytes,
    /// outstanding, saturated)`.
    ///
    /// Reading it: `asked` with `over` at 0 is the ceiling armed and never
    /// reached — the state a byte figure cannot distinguish from a pass that
    /// was never wired. `returns` is the thrash: evictions this pass had to
    /// buy back with a decode. `evictions - returns` is the half that cost
    /// nothing, which is the policy working as designed. A `returns` that
    /// tracks `evictions` is a ceiling set below what the scene actually
    /// needs resident, and it is a FRAME-TIME cost rather than a memory one.
    ///
    /// `saturated` means `outstanding` hit its cap and `returns` is a lower
    /// bound from that point on.
    pub fn ceiling_churn(&self) -> (u64, u64, u64, u64, u64, u64, usize, bool) {
        (
            self.ceiling_asks,
            self.ceiling_over,
            self.ceiling_evictions,
            self.ceiling_evicted_bytes,
            self.ceiling_returns,
            self.ceiling_returned_bytes,
            self.ceiling_outstanding.len(),
            self.ceiling_saturated,
        )
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
            self.archives_ever.remove(&key);
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
        // Both halves of the held set: an archive off the heap is decodable
        // exactly as a heap-held one is, and `needs_decode` already says so.
        let mut unlisted: Vec<chrono::NaiveDateTime> = self
            .archive_cache
            .get(plan.site.as_str())
            .into_iter()
            .flat_map(|archives| archives.keys().copied())
            .chain(
                self.spilled
                    .get(plan.site.as_str())
                    .into_iter()
                    .flat_map(|keys| keys.keys().copied()),
            )
            .filter(|ts| !plan.frames.contains(ts) && self.needs_decode(&plan.site, ts))
            .collect();
        unlisted.sort_unstable();
        // The two maps are disjoint by construction (`cache_archive` drops the
        // spilled copy), so this is insurance and not a policy.
        unlisted.dedup();
        wanted.extend(unlisted);
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

    /// **One physical volume, two addresses, TWO ALLOCATIONS** — the reading
    /// the whole duplicate-volume question turns on.
    ///
    /// `priced_volume` stamps every radial with one collection timestamp, so
    /// two of them are one identity at two addresses: exactly the shape the
    /// chunk feed and the S3 archive produce for one sweep, whose two clocks
    /// are equal on 0 of the 171 local volumes.
    ///
    /// The third file is the OTHER reading, and it is asserted in the same
    /// test on purpose: the same `Arc` under two keys is one allocation, a
    /// merge over it would free a refcount and no memory, and three cuts in
    /// this campaign have been priced against exactly that confusion. If the
    /// two ever collapsed into one counter this test would still pass on
    /// `duplicates` alone, so it reads `same_allocation` and `sole_bytes`
    /// separately.
    ///
    /// TAMPER: make `note_identity_duplication` compare `held_ts == ts`
    /// instead of `!=`, or drop the `scan_cache` residency conjunct, and the
    /// counts move.
    #[test]
    fn one_volume_at_two_addresses_is_two_allocations_and_the_second_is_sole() {
        let mut mgr = LoopDownloadManager::new();
        assert_eq!(mgr.identity_duplication(), IdentityDuplication::default());

        mgr.cache_scan("KTLX", ts(0), priced_volume());
        let first = mgr.identity_duplication();
        assert_eq!(first.files, 1, "the volume states an identity");
        assert_eq!(first.duplicates, 0, "nothing was held to duplicate");

        // The same physical volume, arriving at the address its archive names.
        mgr.cache_scan("KTLX", ts(1), priced_volume());
        let dup = mgr.identity_duplication();
        assert_eq!(dup.files, 2);
        assert_eq!(dup.duplicates, 1, "the second file is the same identity");
        assert_eq!(
            dup.same_allocation, 0,
            "two decodes are two allocations, and a pointer compare says so"
        );
        assert!(
            dup.arrival_sole_bytes > 0,
            "nothing else holds the arriving volume, so declining to file it \
             frees its whole price and not a refcount"
        );
        assert_eq!(
            dup.arrival_sole_bytes, dup.arrival_bytes,
            "the fixture has no other holder"
        );
        assert_eq!(
            dup.twin_sole_bytes, dup.twin_bytes,
            "this cache is the resident twin's only holder, so evicting the \
             superseded copy frees its whole price"
        );
        assert!(dup.twin_bytes > 0, "the twin priced at nothing");
        assert_eq!(
            dup.twin_archiveless, 1,
            "the resident twin has no archive, so `evict_decoded_except` \
             cannot drop it either"
        );

        // The OTHER reading: one allocation filed under two keys.
        let shared = priced_volume();
        mgr.cache_scan("KINX", ts(0), shared.clone());
        mgr.cache_scan("KINX", ts(1), shared);
        let both = mgr.identity_duplication();
        assert_eq!(both.duplicates, 2, "the second KINX file duplicates too");
        assert_eq!(
            both.same_allocation, 1,
            "pointer-equal, so a merge over it frees nothing"
        );
        assert_eq!(
            (both.arrival_sole_bytes, both.twin_sole_bytes),
            (dup.arrival_sole_bytes, dup.twin_sole_bytes),
            "a pointer-equal duplicate adds no freeable bytes on either side"
        );
    }

    /// **A way back is found through whichever address holds it, not through
    /// whichever comes out of the map first.**
    ///
    /// One physical volume can be filed at two addresses — the chunk feed's
    /// own clock and the second an S3 key names — and only one of them may
    /// hold the compressed half. `archive_for_identity` used to take the
    /// first identity match and then ask the archive map about that one
    /// address, so which of the two the `HashMap` yielded first decided
    /// whether a base had a way back at all. Deterministic within a process
    /// and a coin flip between them, which is the shape of a defect that
    /// hides for a whole leg.
    ///
    /// TAMPER: put the `?` back on the first match — spell it `find(..)?`
    /// and then look the archive up — and this goes red about half the time,
    /// which is why the fixture files the archive-less address FIRST and
    /// asserts the address that comes back rather than only that one did.
    #[test]
    fn a_way_back_is_found_past_an_address_that_shares_the_identity_and_holds_nothing() {
        let mut mgr = LoopDownloadManager::new();
        let archive: Arc<Vec<u8>> = Arc::new(vec![3u8; 2048]);
        let collected = crate::types::volume_collected_at(&priced_volume().0)
            .expect("the fixture states an identity");

        mgr.cache_scan("KTLX", ts(0), priced_volume());
        mgr.cache_scan("KTLX", ts(1), priced_volume());
        mgr.cache_archive("KTLX", ts(1), Arc::clone(&archive));

        let (at, found) = mgr
            .archive_for_identity("KTLX", collected)
            .expect("the cache is holding this identity's bytes at one of two addresses");
        assert_eq!(
            at,
            ts(1),
            "the address handed back is not the one holding the archive, so \
             the restore would decode from nothing",
        );
        assert!(
            Arc::ptr_eq(&found, &archive),
            "a different buffer came back"
        );

        // And the control: with no archive anywhere, both addresses still
        // answer nothing.
        let mut bare = LoopDownloadManager::new();
        bare.cache_scan("KTLX", ts(0), priced_volume());
        bare.cache_scan("KTLX", ts(1), priced_volume());
        assert!(
            bare.archive_for_identity("KTLX", collected).is_none(),
            "a way back was invented for a volume nothing is holding bytes for",
        );
    }

    /// **The merge hands a chunk-fed volume a way back, and the residency
    /// pass can then take it** — the whole point of the cut, asserted through
    /// `evict_decoded_except` rather than through `has_archive` alone, because
    /// an archive filed somewhere the evictor does not look would satisfy the
    /// weaker assertion and buy nothing.
    ///
    /// The control is the second half: a twin that ALREADY has an archive is
    /// not re-filed, and a site with no twin at all fires nothing. Without
    /// them a `share_archive_with_twin` that returned `true` unconditionally
    /// would pass on the fires counter.
    ///
    /// TAMPER: drop the `!archive_cache.contains_key` conjunct and the
    /// already-archived control fires; drop the `*held_ts != ts` conjunct and
    /// the no-twin control fires.
    #[test]
    fn the_merge_gives_a_chunk_fed_twin_a_way_back_and_the_evictor_takes_it() {
        let mut mgr = LoopDownloadManager::new();
        let archive: Arc<Vec<u8>> = Arc::new(vec![7u8; 4096]);

        // The chunk feed's volume, with no compressed half, and the archive of
        // the same sweep arriving under the other clock.
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        mgr.cache_scan("KTLX", ts(1), priced_volume());

        // Precondition: the residency pass cannot take either of them.
        assert!(
            mgr.evict_decoded_except(|_, _, _| false).is_empty(),
            "precondition: a volume with no archive is never evicted"
        );

        assert!(
            mgr.share_archive_with_twin("KTLX", ts(1), &archive),
            "the twin at ts(0) is the same physical volume and has no archive"
        );
        let fired = mgr.identity_duplication();
        assert_eq!(
            fired.merges, 1,
            "the fires counter did not record the merge"
        );
        assert!(fired.merge_bytes > 0, "the merged twin priced at nothing");
        assert!(mgr.has_archive("KTLX", &ts(0)), "the twin took the archive");

        // The reading the cut is FOR: the evictor may now trade it.
        let taken = mgr.evict_decoded_except(|_, at, _| *at != ts(0));
        assert_eq!(taken.len(), 1, "the merged twin is still un-evictable");
        assert!(!mgr.is_cached("KTLX", &ts(0)), "the twin is gone");

        // Control: a twin that already has an archive is not re-filed.
        mgr.cache_scan("KINX", ts(0), priced_volume());
        mgr.cache_archive("KINX", ts(0), Arc::clone(&archive));
        mgr.cache_scan("KINX", ts(1), priced_volume());
        assert!(
            !mgr.share_archive_with_twin("KINX", ts(1), &archive),
            "a twin that can already be rebuilt was merged again"
        );

        // Control: a site holding one volume has no twin to merge with.
        mgr.cache_scan("KVNX", ts(0), priced_volume());
        assert!(
            !mgr.share_archive_with_twin("KVNX", ts(0), &archive),
            "a volume was merged with itself"
        );
        assert_eq!(
            mgr.identity_duplication().merges,
            1,
            "a control fired the counter"
        );
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

    /// **The identity index outlives the decoded half it was learned from**,
    /// which is the only reason it is any use.
    ///
    /// `retain_archives` is asked exactly when an archive is all that is left,
    /// so an index cleaned alongside the moments would answer `None` at the
    /// one moment it is consulted and the two-clock repair would be a no-op
    /// that reads as present. The fixture therefore **evicts the decoded half
    /// first** and only then asks: an arrangement that queried the index while
    /// the volume was still resident could not tell a surviving index from one
    /// that dies with the volume.
    ///
    /// It is cleaned with the ARCHIVE, and the second half of this test is
    /// what says so — an index that grew without bound would be a leak of its
    /// own, small per entry and unbounded in time.
    ///
    /// TAMPER: clear `archive_identity` in `evict_decoded_except`, or stop
    /// filling it in `cache_scan`, and the first assertion fails; stop
    /// clearing it in `retain_archives` and the last one does.
    #[test]
    fn the_identity_index_outlives_the_volume_and_dies_with_the_archive() {
        let volume = priced_volume();
        let identity = crate::types::volume_collected_at(&volume.0)
            .expect("fixture: the volume has no clocked radial, so there is no identity to index");
        assert_ne!(
            identity,
            ts(0),
            "fixture: the identity equals the address, so an address-only \
             predicate answers correctly and nothing here is about two clocks",
        );

        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), volume);
        mgr.cache_archive("KTLX", ts(0), archive(1024));

        // The moments go, the compressed bytes stay: the state the index
        // exists to survive.
        drop(mgr.evict_decoded_except(|_, _, _| false));
        assert!(
            mgr.get_cached("KTLX", &ts(0)).is_none(),
            "precondition: the decoded half is gone, which is when this index \
             is consulted",
        );

        // Asked ONLY by identity — the address is deliberately refused, so a
        // predicate that still resolved by address would fail here.
        mgr.retain_archives(|_, _, collected| collected == Some(identity));
        assert!(
            mgr.has_archive("KTLX", &ts(0)),
            "the index did not survive the volume, so an archive whose frame \
             is named by identity is swept the moment its moments are traded \
             away — which is the trade undoing itself",
        );

        // And it leaves with the archive rather than accumulating.
        mgr.retain_archives(|_, _, _| false);
        assert!(!mgr.has_archive("KTLX", &ts(0)));
        mgr.retain_archives(|_, _, collected| {
            assert_eq!(
                collected, None,
                "the index outlived the archive it belongs to, so it grows \
                 for the life of the process",
            );
            true
        });
    }

    /// **The decoded ceiling reclaims, and it reclaims the right entry** —
    /// furthest from a playhead first, and never one that is drawing or one
    /// with nothing to rebuild from.
    ///
    /// # What an input must carry for the defect to appear
    ///
    /// Three things at once, and a fixture missing any one of them is
    /// structurally unable to reach this defect class however green it runs:
    ///
    /// 1. **The cache must be OVER the ceiling.** Only reachable because
    ///    `ceiling` is a parameter — a policy that read
    ///    `LOOP_DECODED_CEILING_BYTES` inside itself could not be driven into
    ///    its own working range by any fixture that fits in a test process,
    ///    and every assertion about it would be about the branch not taken.
    /// 2. **At least one evictable candidate**: archived, unpinned, and far
    ///    enough out that the ranking has something to order. Without it the
    ///    walk returns empty and the hold-backs below are unfalsifiable, since
    ///    "nothing was evicted" would be the right answer anyway.
    /// 3. **At least one of EACH protected class, resident at the same
    ///    time** — one pinned volume and one archive-less volume. A fixture
    ///    holding only candidates proves the eviction and nothing about what
    ///    the eviction refuses.
    ///
    /// Each entry is asserted against **what that entry needs**, by
    /// `Arc::strong_count`, and never against a total: a cache that shed the
    /// right number of bytes by evicting the pinned volume and keeping a far
    /// one would satisfy any figure-shaped assertion and be the exact defect
    /// this exists to catch.
    ///
    /// `Arc::strong_count` and not `cached_scan_count`: a store row is not a
    /// byte, and the whole campaign this lands in turns on the difference —
    /// the same allocation is held by the still inventory and by this cache,
    /// so a count falling to zero here proves nothing about the heap. What
    /// proves it is the refcount of the allocation itself, taken after the
    /// returned volumes are dropped.
    ///
    /// TAMPER: drop the `pinned` skip and the parked volume is evicted from
    /// under its pane; drop the `archived` skip and the archive-less volume
    /// goes with nothing able to hand it back; reverse the sort and the
    /// nearest frame to the playhead is the one that goes.
    #[test]
    fn the_ceiling_counts_the_eviction_it_bought_back_apart_from_the_one_it_did_not() {
        let one = crate::scan_size::scan_bytes(&priced_volume().0);
        // Room for three of the five, so exactly two go.
        let ceiling = one * 3;
        let mut mgr = LoopDownloadManager::new();
        for minute in [0, 2, 4, 6, 8] {
            mgr.cache_scan("KTLX", ts(minute), priced_volume());
            mgr.cache_archive("KTLX", ts(minute), archive(1024));
        }

        // **Armed and idle, first**, because it is the reading a byte figure
        // cannot give: a pass that ran against a cache already under its
        // ceiling evicts nothing and must still be visible as having run.
        let under = mgr.evict_decoded_to_ceiling(usize::MAX, |_, _| 0, |_, _, _| false);
        assert!(under.is_empty());
        let (asked, over, evicted, ..) = mgr.ceiling_churn();
        assert_eq!(
            (asked, over, evicted),
            (1, 0, 0),
            "asked but never over: the ceiling is armed and was not reached, \
             which is not the same world as a pass nobody wired",
        );

        // Furthest stamp first, so minutes 8 and 6 are the two that go.
        let rank = |ts: &chrono::NaiveDateTime| {
            use chrono::Timelike;
            u64::from(ts.and_utc().minute())
        };
        let removed = mgr.evict_decoded_to_ceiling(ceiling, |_, ts| rank(ts), |_, _, _| false);
        assert_eq!(removed.len(), 2, "fixture: exactly two volumes go");
        drop(removed);
        let (asked, over, evicted, evicted_bytes, returned, returned_bytes, outstanding, saturated) =
            mgr.ceiling_churn();
        assert_eq!((asked, over, evicted), (2, 1, 2));
        assert_eq!(evicted_bytes, (one * 2) as u64);
        assert_eq!(
            (returned, returned_bytes),
            (0, 0),
            "nothing has come back yet, so the whole eviction is still the \
             free half",
        );
        assert_eq!(outstanding, 2);
        assert!(!saturated);

        // **One of the two comes back**, which is the decode the ceiling
        // bought its bytes with. The other never does.
        mgr.cache_scan("KTLX", ts(8), priced_volume());
        let (_, _, evicted, _, returned, returned_bytes, outstanding, _) = mgr.ceiling_churn();
        assert_eq!(
            (returned, returned_bytes),
            (1, one as u64),
            "the volume the ceiling threw away and the loop needed back is \
             the cycle, and it is counted where the decode is PAID rather \
             than inferred from a later eviction",
        );
        assert_eq!(
            evicted - returned,
            1,
            "and the eviction nothing wanted again stays in the free half: a \
             count of evictions alone cannot tell these two apart, which is \
             the whole reason this counter is a cycle and not an event",
        );
        assert_eq!(outstanding, 1, "the one that never came back is still owed");

        // A volume that was never ceiling-evicted must not count as a return.
        mgr.cache_scan("KTLX", ts(2), priced_volume());
        let (_, _, _, _, returned, _, _, _) = mgr.ceiling_churn();
        assert_eq!(
            returned, 1,
            "a re-file of a volume this pass never evicted is not a buy-back",
        );
    }

    #[test]
    fn the_decoded_ceiling_evicts_the_furthest_and_refuses_the_protected() {
        use chrono::Timelike;
        let one = crate::scan_size::scan_bytes(&priced_volume().0);
        // Room for three of the five, so the walk must both evict and stop:
        // a policy that emptied every candidate would also satisfy "under the
        // ceiling" and is what the `ts(4)` assertion below rules out.
        let ceiling = one * 3;

        let mut mgr = LoopDownloadManager::new();
        let mut held: Vec<(u32, Arc<nexrad_model::data::Scan>)> = Vec::new();
        for minute in [0, 2, 4, 6, 8] {
            let volume = priced_volume();
            held.push((minute, Arc::clone(&volume.0)));
            mgr.cache_scan("KTLX", ts(minute), volume);
        }
        // Every one but ts(2): that one is the archive-less class.
        for minute in [0, 4, 6, 8] {
            mgr.cache_archive("KTLX", ts(minute), archive(1024));
        }
        let at = |minute: u32| -> &Arc<nexrad_model::data::Scan> {
            &held
                .iter()
                .find(|(m, _)| *m == minute)
                .expect("the fixture filed this minute")
                .1
        };

        assert_eq!(
            mgr.cached_scan_bytes(),
            one * 5,
            "precondition: five distinct volumes at one price, so the \
             arithmetic below is about which of them went",
        );
        assert!(
            mgr.cached_scan_bytes() > ceiling,
            "precondition: the cache is over the ceiling, which is the only \
             state this policy acts in",
        );
        for (minute, volume) in &held {
            assert_eq!(
                Arc::strong_count(volume),
                2,
                "precondition: minute {minute} is held by the cache and by \
                 this test and by nothing else",
            );
        }

        // **The rank DESCENDS with the stamp, so the two protected entries
        // are the two the walk would reach FIRST.** That is the whole design
        // of this fixture and not an incidental ordering: with the protected
        // entries ranked nearest, a walk that had lost its hold-backs would
        // still stop at the ceiling before reaching them, and every assertion
        // about what this policy refuses would be about a branch the fixture
        // cannot enter. Ranked furthest, dropping either guard evicts a
        // protected volume on the first or second step.
        let rank = |minute: u32| 100u64 - u64::from(minute) * 5;
        let removed = mgr.evict_decoded_to_ceiling(
            ceiling,
            |_, ts| rank(ts.and_utc().minute()),
            |_, ts, _| ts.and_utc().minute() == 0,
        );
        drop(removed);

        assert_eq!(
            Arc::strong_count(at(4)),
            1,
            "the furthest evictable frame is archived and unpinned and was \
             not evicted",
        );
        assert_eq!(
            Arc::strong_count(at(6)),
            1,
            "the second-furthest evictable frame was needed to reach the \
             ceiling and stayed",
        );
        assert_eq!(
            Arc::strong_count(at(8)),
            2,
            "the ceiling was already met and the nearest frame was evicted \
             anyway, so the walk empties rather than reclaiming",
        );
        assert_eq!(
            Arc::strong_count(at(2)),
            2,
            "the volume with no archive behind it was evicted, which turns a \
             residency policy into a re-download policy",
        );
        assert_eq!(
            Arc::strong_count(at(0)),
            2,
            "the volume a pane is parked on was evicted by a byte ceiling, \
             which is a blank pane",
        );
        assert_eq!(
            mgr.cached_scan_bytes(),
            ceiling,
            "the running total did not follow the volumes out",
        );
        for minute in [4, 6] {
            assert!(
                mgr.has_archive("KTLX", &ts(minute)),
                "minute {minute} lost its archive with its moments, so the \
                 trade this policy is built on cannot be made back",
            );
        }
    }

    /// **Under the ceiling the policy is inert.** The control for the test
    /// above: a walk that evicted whatever `rank` put last would satisfy
    /// every assertion there and would be shedding volumes a cache with room
    /// for them is entitled to keep.
    ///
    /// TAMPER: delete the early return **and** the eviction loop's own
    /// `break`. Either alone is covered by the other — they are two spellings
    /// of one predicate, the first there to skip building and ranking the
    /// candidate list at all on a sweep with nothing to do — so naming only
    /// one would be a tamper this gate cannot see, which is a claim about the
    /// gate and not about the code. Removing both fails on the first volume.
    #[test]
    fn the_decoded_ceiling_evicts_nothing_while_the_cache_fits() {
        let one = crate::scan_size::scan_bytes(&priced_volume().0);
        let mut mgr = LoopDownloadManager::new();
        let mut held = Vec::new();
        for minute in [0, 2] {
            let volume = priced_volume();
            held.push(Arc::clone(&volume.0));
            mgr.cache_scan("KTLX", ts(minute), volume);
            mgr.cache_archive("KTLX", ts(minute), archive(1024));
        }
        assert!(
            mgr.cached_scan_bytes() <= one * 2,
            "precondition: the fixture is inside the ceiling it is given",
        );

        let removed = mgr.evict_decoded_to_ceiling(one * 2, |_, _| u64::MAX, |_, _, _| false);

        assert!(removed.is_empty(), "a cache that fits shed volumes anyway");
        for volume in &held {
            assert_eq!(
                Arc::strong_count(volume),
                2,
                "a volume left the cache while it was under its ceiling",
            );
        }
    }

    /// **A cache whose every volume is protected stays over the ceiling**, and
    /// that is the correct failure.
    ///
    /// The ceiling is a target, not a guarantee, for the same reason the
    /// archive twin's is: the alternative to holding a pinned volume past a
    /// byte budget is taking a picture off the glass. Gated rather than
    /// stated, because "it would never happen" is what the archive-less
    /// arrivals were said to be.
    ///
    /// TAMPER: make either hold-back conditional on the ceiling and this goes
    /// red on the class that loses its exemption.
    #[test]
    fn the_decoded_ceiling_never_takes_a_drawing_volume_off_the_glass() {
        use chrono::Timelike;
        let one = crate::scan_size::scan_bytes(&priced_volume().0);
        let mut mgr = LoopDownloadManager::new();
        let mut held = Vec::new();
        for minute in [0, 2, 4] {
            let volume = priced_volume();
            held.push(Arc::clone(&volume.0));
            mgr.cache_scan("KTLX", ts(minute), volume);
        }
        // **One entry per class, and each protected by exactly ONE guard**,
        // so a tamper on either is reachable. ts(0) and ts(2) are archived
        // and pinned, so only `pinned` stands between them and a ceiling of
        // zero; ts(4) is unpinned with no archive, so only the archive rule
        // stands between it and the same ceiling. Pinning all three would
        // have made the archive rule unfalsifiable here.
        for minute in [0, 2] {
            mgr.cache_archive("KTLX", ts(minute), archive(1024));
        }

        let removed = mgr.evict_decoded_to_ceiling(
            0,
            |_, _| u64::MAX,
            |_, ts, _| matches!(ts.and_utc().minute(), 0 | 2),
        );

        assert!(
            removed.is_empty(),
            "a ceiling of zero evicted a volume every hold-back protects",
        );
        assert_eq!(
            mgr.cached_scan_bytes(),
            one * 3,
            "the cache is expected to sit over its ceiling here; a figure \
             that fell means something was taken",
        );
        for volume in &held {
            assert_eq!(Arc::strong_count(volume), 2);
        }
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
    /// **What "no archive" means, told apart from "never had one"** — the
    /// two prices of the same eviction, and the memory that keeps them apart
    /// lives exactly as long as the volume it describes.
    ///
    /// A resident volume whose archive the ceiling took still answers
    /// `ever_had_archive`, because the object is in the bucket and getting it
    /// back is a download. Once BOTH halves are gone the key goes too, so this
    /// set cannot grow with every stamp a long session lists.
    ///
    /// TAMPER: drop the `archives_ever.insert` in `cache_archive` and the
    /// first assertion reads false; drop either cleanup and the last reads 1.
    #[test]
    fn a_volume_that_lost_its_archive_is_told_apart_from_one_that_never_had_one() {
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        mgr.cache_scan("KTLX", ts(1), priced_volume());
        mgr.cache_archive("KTLX", ts(0), Arc::new(vec![0u8; 64]));

        mgr.retain_archives(|_, _, _| false);

        let ever: Vec<(chrono::NaiveDateTime, bool)> = mgr
            .decoded_entries()
            .map(|e| (e.timestamp, e.ever_had_archive))
            .collect();
        assert!(
            ever.contains(&(ts(0), true)),
            "the volume whose way back the sweep took reads as never having \
             had one, which prices a download as lost data: {ever:?}",
        );
        assert!(
            ever.contains(&(ts(1), false)),
            "the volume nothing ever filed an archive for reads as having had \
             one: {ever:?}",
        );

        // Both halves gone: the question dies with them.
        let keep = ts(1);
        mgr.retain_scans(|_, at, _| *at == keep);
        mgr.retain_archives(|_, _, _| false);

        assert_eq!(
            mgr.archives_ever.len(),
            0,
            "a key outlived both halves of the frame it describes, so this set \
             grows with every stamp the session ever listed",
        );

        // And the archive that never had a decoded half beside it — the shape
        // the decoded evictions' own cleanup can never reach — through both
        // paths that drop an archive.
        mgr.cache_archive("KTLX", ts(2), Arc::new(vec![0u8; 64]));
        mgr.retain_archives(|_, _, _| false);
        assert_eq!(
            mgr.archives_ever.len(),
            0,
            "an archive with no volume beside it left its key behind",
        );
        mgr.cache_archive("KTLX", ts(3), Arc::new(vec![0u8; 64]));
        mgr.evict_archives_to_ceiling(0, |_, _| 0, |_, _, _| false);
        assert_eq!(
            mgr.archives_ever.len(),
            0,
            "the byte ceiling drops an archive and leaves its key behind",
        );
    }

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

        mgr.retain_archives(|site, at, _| site == "KTLX" && *at == ts(0));

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

        mgr.retain_archives(|_, _, _| false);
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
        let freed = mgr.evict_archives_to_ceiling(
            CEILING,
            |_, at| (at.and_utc().timestamp() - ts(0).and_utc().timestamp()) as u64,
            |_, _, _| false,
        );

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

    /// **Without a pin the byte ceiling takes a released base's way back
    /// FIRST**, ahead of every loop frame it is holding — the defect, forced
    /// rather than argued.
    ///
    /// The scene is the one `App::release_unneeded_base_gates` leaves behind:
    /// a volume whose decoded half has been withdrawn and whose compressed half
    /// is the only thing that can bring it back, beside a loop holding 25
    /// maximum-sized archives that put the cache over its ceiling. No live
    /// frame names the released base's address — the pane holding it is not
    /// looping — so the caller's distance rank answers `u64::MAX` for it and the
    /// sort puts it at the head of the queue, ahead of all 25.
    ///
    /// This is `pinned` answering `false`, which is what the parameter's
    /// absence was until 2026-09-10: the archive goes, `ensure_base_whole` has
    /// nothing to decode from, and a cross-section or 3D pane on that site
    /// waits for gates that can never arrive.
    #[test]
    fn without_a_pin_the_ceiling_takes_a_released_bases_way_back_before_any_loop_frame() {
        const CEILING: usize = 96 * 1024 * 1024;
        let released_at = ts(50);
        let mut mgr = LoopDownloadManager::new();

        // The released base: its identity is learned from the volume, the
        // volume is then withdrawn, and the archive is all that is left.
        mgr.cache_archive("KTLX", released_at, archive(CORPUS_MIN_ARCHIVE));
        mgr.cache_scan("KTLX", released_at, priced_volume());
        drop(mgr.evict_decoded_except(|site, at, _| !(site == "KTLX" && *at == released_at)));
        assert!(
            mgr.has_archive("KTLX", &released_at),
            "precondition: the release keeps the archive it released against",
        );

        // The loop, whose frames are all nearer a playhead than the released
        // base, which no frame names at all.
        for minute in 0..25u32 {
            mgr.cache_archive("KTLX", ts(minute), archive(CORPUS_MAX_ARCHIVE));
        }
        assert!(
            mgr.cached_archive_bytes() > CEILING,
            "precondition: the cache must be over the ceiling for the pass to run",
        );

        let freed = mgr.evict_archives_to_ceiling(
            CEILING,
            |_, at| {
                if *at == released_at {
                    u64::MAX
                } else {
                    (at.and_utc().timestamp() - ts(0).and_utc().timestamp()) as u64
                }
            },
            |_, _, _| false,
        );

        assert!(freed > 0, "nothing was evicted, so nothing is being shown");
        assert!(
            !mgr.has_archive("KTLX", &released_at),
            "the scene the pin exists for did not arise: the released base's way \
             back survived an unpinned ceiling, so this test cannot show what \
             pinning it buys",
        );
        assert_eq!(
            mgr.archives_pinned_to_ceiling(),
            0,
            "nothing was pinned and the counter moved anyway",
        );
    }

    /// **The pin keeps it, and keeps only it.**
    ///
    /// The same scene with the predicate the application passes. The way back
    /// survives, the ceiling is still enforced against everything else, and the
    /// counter says the pin fired — which is the only witness there is: a
    /// stranded base and a base that was never released read identically from
    /// every other instrument.
    ///
    /// TAMPER: return `false` from the pin and this is the test above.
    #[test]
    fn the_ceiling_will_not_take_a_released_bases_way_back() {
        const CEILING: usize = 96 * 1024 * 1024;
        let released_at = ts(50);
        let mut mgr = LoopDownloadManager::new();

        mgr.cache_archive("KTLX", released_at, archive(CORPUS_MIN_ARCHIVE));
        mgr.cache_scan("KTLX", released_at, priced_volume());
        let identity = crate::types::volume_collected_at(&priced_volume().0)
            .expect("the fixture volume has a first radial");
        drop(mgr.evict_decoded_except(|site, at, _| !(site == "KTLX" && *at == released_at)));
        for minute in 0..25u32 {
            mgr.cache_archive("KTLX", ts(minute), archive(CORPUS_MAX_ARCHIVE));
        }
        let before = mgr.cached_archive_bytes();
        assert!(before > CEILING, "precondition: over the ceiling");

        // Asked by IDENTITY, which is what a merge base is keyed by, and not
        // by the address the archive is filed at — the two are not the same
        // instant on a chunk-fed volume.
        let freed = mgr.evict_archives_to_ceiling(
            CEILING,
            |_, at| {
                if *at == released_at {
                    u64::MAX
                } else {
                    (at.and_utc().timestamp() - ts(0).and_utc().timestamp()) as u64
                }
            },
            |site, _, collected| site == "KTLX" && collected == Some(identity),
        );

        assert!(freed > 0, "the pass evicted nothing at all");
        assert!(
            mgr.has_archive("KTLX", &released_at),
            "the byte ceiling took the released base's only way back",
        );
        assert!(
            mgr.cached_archive_bytes() <= CEILING,
            "the pin stopped the pass rather than filtering it: {} B left against \
             a {CEILING} B ceiling",
            mgr.cached_archive_bytes(),
        );
        assert_eq!(
            mgr.archives_pinned_to_ceiling(),
            1,
            "the counter that is the pin's only witness did not move",
        );
    }

    /// **A pinned archive is not consulted at all while the cache fits.**
    ///
    /// The counter is a running total over passes that were over the ceiling,
    /// and a pass that early-returns must not charge it — otherwise the figure
    /// says "the pin saved a way back" on a leg where nothing was ever at risk.
    #[test]
    fn a_cache_inside_the_ceiling_does_not_charge_the_pin() {
        const CEILING: usize = 96 * 1024 * 1024;
        let mut mgr = LoopDownloadManager::new();
        mgr.cache_archive("KTLX", ts(0), archive(CORPUS_MIN_ARCHIVE));
        assert_eq!(
            mgr.evict_archives_to_ceiling(CEILING, |_, _| 0, |_, _, _| true),
            0,
        );
        assert_eq!(
            mgr.archives_pinned_to_ceiling(),
            0,
            "a pass that never ran charged the pin",
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

        let freed = mgr.evict_archives_to_ceiling(CEILING, |_, _| 0, |_, _, _| false);

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

    // ---- the off-heap spill ----------------------------------------------

    /// An [`ArchiveSpill`](crate::archive_spill::ArchiveSpill) in memory, so a
    /// manager test never touches a disk. The `FsArchiveSpill` has its own
    /// suite; what these tests are about is the manager's bookkeeping.
    /// The blobs a [`SpillDouble`] is holding, keyed as the caches key them.
    type SpilledBlobs = HashMap<(String, chrono::NaiveDateTime), Vec<u8>>;

    #[derive(Clone, Default)]
    struct SpillProbe(Arc<std::sync::Mutex<SpilledBlobs>>);

    impl SpillProbe {
        fn len(&self) -> usize {
            self.0.lock().expect("the probe is not poisoned").len()
        }
    }

    struct SpillDouble {
        probe: SpillProbe,
        refuse: bool,
    }

    impl crate::archive_spill::ArchiveSpill for SpillDouble {
        fn store(&self, site: &str, ts: chrono::NaiveDateTime, bytes: &[u8]) -> bool {
            if self.refuse {
                return false;
            }
            self.probe
                .0
                .lock()
                .expect("the probe is not poisoned")
                .insert((site.to_string(), ts), bytes.to_vec());
            true
        }

        fn load(&self, site: &str, ts: chrono::NaiveDateTime) -> Option<Vec<u8>> {
            self.probe
                .0
                .lock()
                .expect("the probe is not poisoned")
                .get(&(site.to_string(), ts))
                .cloned()
        }

        fn delete(&self, site: &str, ts: chrono::NaiveDateTime) {
            self.probe
                .0
                .lock()
                .expect("the probe is not poisoned")
                .remove(&(site.to_string(), ts));
        }
    }

    /// A manager holding three minimum-corpus archives for one site, the
    /// oldest of which also has its decoded half, and an archive ceiling one
    /// byte under what it is holding so exactly the ranked-first archive must
    /// go. `spill` gives it somewhere to put it.
    fn over_ceiling(spill: Option<SpillProbe>, refuse: bool) -> (LoopDownloadManager, usize) {
        let mut mgr = LoopDownloadManager::new();
        if let Some(probe) = spill {
            mgr.set_spill(Box::new(SpillDouble { probe, refuse }), 64 * 1024 * 1024);
        }
        // The decoded half of the frame whose archive is about to leave: it is
        // the 15.5x-larger half, and whether it stays evictable is the point.
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        for minute in 0..3u32 {
            mgr.cache_archive("KTLX", ts(minute), archive(CORPUS_MIN_ARCHIVE));
        }
        let held = mgr.cached_archive_bytes();
        (mgr, held)
    }

    /// Rank `ts(0)` furthest from the playhead, so it is the one the ceiling
    /// reaches first.
    fn oldest_first(_: &str, at: &chrono::NaiveDateTime) -> u64 {
        if *at == ts(0) { u64::MAX } else { 0 }
    }

    /// **The whole cut, and the reason it is not simply a smaller ceiling: the
    /// heap bytes leave AND the way back survives, so the decoded volume in
    /// front of the archive stays evictable instead of being stranded.**
    ///
    /// Both arms in one test because the contrast is the claim. With nowhere to
    /// put the archive — today's tree — the ceiling drops it and
    /// `evict_decoded_except` then refuses the volume for ever: the archive was
    /// a median 6.45 % of it, so 1 part is reclaimed and 15.5 parts are
    /// stranded un-evictable. With a spill, the same pass frees the same heap
    /// bytes and the volume is still reclaimable.
    ///
    /// TAMPER: make `spill_archive` return `false` unconditionally and the
    /// spill arm becomes the control arm — `has_archive` goes false and the
    /// decoded volume is refused.
    #[test]
    fn an_archive_the_ceiling_takes_goes_off_heap_and_is_still_a_way_back() {
        // --- the control: no spill, which is this tree before the change ---
        let (mut bare, held) = over_ceiling(None, false);
        let freed = bare.evict_archives_to_ceiling(held - 1, oldest_first, |_, _, _| false);
        assert!(freed > 0, "the fixture did not go over the ceiling");
        assert!(
            !bare.has_archive("KTLX", &ts(0)),
            "the control arm kept the archive, so it is not the control",
        );
        assert_eq!(
            bare.evict_decoded_except(|_, _, _| false).len(),
            0,
            "the decoded volume was evicted with no archive behind it, which \
             would make the residency policy a re-download policy",
        );
        assert_eq!(bare.spill_counts().0, 0, "a manager with no spill spilled");

        // --- the change: somewhere to put it ---
        let probe = SpillProbe::default();
        let (mut mgr, held) = over_ceiling(Some(probe.clone()), false);
        let freed = mgr.evict_archives_to_ceiling(held - 1, oldest_first, |_, _, _| false);

        assert_eq!(
            freed,
            CORPUS_MIN_ARCHIVE + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD,
            "the heap did not give back the archive's bytes, which is the cut",
        );
        assert_eq!(
            mgr.cached_archive_bytes(),
            held - freed,
            "the host-byte ledger disagrees with what it says it freed",
        );
        assert_eq!(
            mgr.cached_archive_count("KTLX"),
            2,
            "the archive is still on the heap",
        );

        // The way back survived.
        assert!(
            mgr.has_archive("KTLX", &ts(0)),
            "the way back went with the heap bytes, so the volume in front of \
             it is now stranded un-evictable — the one outcome this change \
             exists to avoid",
        );
        assert!(mgr.is_spilled("KTLX", &ts(0)));
        assert_eq!(probe.len(), 1, "the bytes never reached the medium");
        assert_eq!(
            mgr.spilled_bytes(),
            CORPUS_MIN_ARCHIVE,
            "the medium's ledger is not the bytes written to it",
        );

        // And therefore the 15.5x-larger half is reclaimable.
        assert_eq!(
            mgr.evict_decoded_except(|_, _, _| false).len(),
            1,
            "the decoded volume was refused though its archive is one read \
             away, so the spill bought nothing",
        );
    }

    /// **The fires-counter pair, both directions.** A cut whose counter cannot
    /// distinguish "did not fire" from "fired and did nothing" is how a
    /// ~94 MiB cut on this campaign delivered exactly zero for a day.
    #[test]
    fn the_spill_counters_say_which_way_the_bytes_went() {
        let probe = SpillProbe::default();
        let (mut mgr, held) = over_ceiling(Some(probe.clone()), false);
        assert_eq!(
            mgr.spill_counts(),
            (0, 0, 0, 0, 0),
            "a manager that has evicted nothing has already counted something",
        );

        mgr.evict_archives_to_ceiling(held - 1, oldest_first, |_, _, _| false);
        let (spilled, refused, failed, restored, misses) = mgr.spill_counts();
        assert_eq!(
            (spilled, refused, failed, restored, misses),
            (1, 0, 0, 0, 0),
            "one archive went off-heap and nothing has been asked back yet",
        );

        // The withdrawal direction.
        let back = mgr
            .archive_for("KTLX", &ts(0))
            .expect("the archive is one read away");
        assert_eq!(
            back.len(),
            CORPUS_MIN_ARCHIVE,
            "the bytes that came back are not the ones that went out",
        );
        assert!(
            back.iter().all(|b| *b == 7),
            "the archive came back corrupted, so a decode would fail on it",
        );
        let (_, _, _, restored, misses) = mgr.spill_counts();
        assert_eq!((restored, misses), (1, 0), "the restore was not counted");
    }

    /// **A released base's way back runs through the medium too.** This is the
    /// other withdrawal, and it is the one whose absence leaves a cross-section
    /// pane waiting for ever.
    #[test]
    fn a_released_base_finds_its_way_back_through_the_medium() {
        let probe = SpillProbe::default();
        let mut mgr = LoopDownloadManager::new();
        mgr.set_spill(
            Box::new(SpillDouble {
                probe,
                refuse: false,
            }),
            64 * 1024 * 1024,
        );
        let collected = crate::types::volume_collected_at(&priced_volume().0)
            .expect("the fixture states an identity");

        mgr.cache_scan("KTLX", ts(0), priced_volume());
        mgr.cache_archive("KTLX", ts(0), archive(CORPUS_MIN_ARCHIVE));
        let held = mgr.cached_archive_bytes();
        assert!(
            mgr.archive_for_identity("KTLX", collected).is_some(),
            "the fixture did not reach the state this test is about",
        );

        mgr.evict_archives_to_ceiling(held - 1, oldest_first, |_, _, _| false);
        assert_eq!(mgr.cached_archive_count("KTLX"), 0, "still on the heap");

        let (at, found) = mgr
            .archive_for_identity("KTLX", collected)
            .expect("the medium is holding this identity's bytes");
        assert_eq!(at, ts(0), "the wrong address came back");
        assert_eq!(found.len(), CORPUS_MIN_ARCHIVE);

        // And the diagnostic beside it agrees, rather than reporting the
        // archive as gone.
        let (learned, with_archive, _) = mgr.identity_way_backs("KTLX", collected);
        assert_eq!(
            (learned, with_archive),
            (1, 1),
            "the way-back diagnostic reports an off-heap archive as gone, so a \
             real defect would be read as this change's doing",
        );
    }

    /// **The disk has a ceiling too, and past it this behaves exactly as a
    /// tree with no spill.** A byte ceiling that moves its overflow to a medium
    /// which also runs out is a leak with a longer fuse.
    #[test]
    fn past_the_spills_own_ceiling_the_archive_is_dropped_and_the_refusal_counted() {
        let probe = SpillProbe::default();
        let mut mgr = LoopDownloadManager::new();
        // Room for nothing: the first archive offered is already too big.
        mgr.set_spill(
            Box::new(SpillDouble {
                probe: probe.clone(),
                refuse: false,
            }),
            1024,
        );
        mgr.cache_scan("KTLX", ts(0), priced_volume());
        for minute in 0..3u32 {
            mgr.cache_archive("KTLX", ts(minute), archive(CORPUS_MIN_ARCHIVE));
        }
        let held = mgr.cached_archive_bytes();

        let freed = mgr.evict_archives_to_ceiling(held - 1, oldest_first, |_, _, _| false);

        assert!(freed > 0, "nothing was evicted, so nothing was offered");
        assert_eq!(probe.len(), 0, "the medium took bytes past its ceiling");
        assert_eq!(mgr.spilled_bytes(), 0);
        assert!(
            !mgr.has_archive("KTLX", &ts(0)),
            "the archive is reported held though it reached neither the heap \
             nor the medium",
        );
        let (spilled, refused, failed, _, _) = mgr.spill_counts();
        assert_eq!(
            (spilled, refused, failed),
            (0, 1, 0),
            "the refusal is not distinguishable from a store that failed",
        );
    }

    /// A store that fails is not a policy refusal, and the two are counted
    /// apart. Either way the archive is dropped, as a tree with no spill does.
    #[test]
    fn a_store_that_fails_is_counted_apart_from_a_full_medium() {
        let probe = SpillProbe::default();
        let (mut mgr, held) = over_ceiling(Some(probe.clone()), true);
        mgr.evict_archives_to_ceiling(held - 1, oldest_first, |_, _, _| false);

        assert_eq!(probe.len(), 0);
        assert!(!mgr.has_archive("KTLX", &ts(0)));
        let (spilled, refused, failed, _, _) = mgr.spill_counts();
        assert_eq!(
            (spilled, refused, failed),
            (0, 0, 1),
            "a failed store reads as a full medium, which would send the next \
             reader to the wrong ceiling",
        );
    }

    /// **The spill follows the frame list DOWN as well as up**, which with the
    /// ceiling refusal is what bounds the medium over a long session.
    #[test]
    fn a_frame_the_loop_stops_naming_leaves_the_medium_too() {
        let probe = SpillProbe::default();
        let (mut mgr, held) = over_ceiling(Some(probe.clone()), false);
        mgr.evict_archives_to_ceiling(held - 1, oldest_first, |_, _, _| false);
        assert_eq!(probe.len(), 1, "the fixture did not reach a spilled state");
        assert_eq!(mgr.spilled_count(), 1);

        // The loop stops naming ts(0).
        mgr.retain_archives(|_, at, _| *at != ts(0));

        assert_eq!(
            probe.len(),
            0,
            "a frame the loop no longer names kept its file on the medium; \
             nothing else would ever collect it and the disk grows with every \
             stamp a long session lists",
        );
        assert_eq!(mgr.spilled_bytes(), 0, "the medium's ledger did not follow");
        assert_eq!(mgr.spilled_count(), 0);
        assert!(!mgr.has_archive("KTLX", &ts(0)));
    }

    /// A heap copy supersedes an off-heap one, or both would be counted and the
    /// medium would keep bytes nothing can ever read.
    #[test]
    fn a_heap_copy_supersedes_the_spilled_one() {
        let probe = SpillProbe::default();
        let (mut mgr, held) = over_ceiling(Some(probe.clone()), false);
        mgr.evict_archives_to_ceiling(held - 1, oldest_first, |_, _, _| false);
        assert!(mgr.is_spilled("KTLX", &ts(0)), "the fixture did not arise");

        mgr.cache_archive("KTLX", ts(0), archive(CORPUS_MIN_ARCHIVE));

        assert!(
            !mgr.is_spilled("KTLX", &ts(0)),
            "the spilled copy outlived the heap copy that replaced it",
        );
        assert_eq!(probe.len(), 0, "the medium kept an unreadable duplicate");
        assert_eq!(mgr.spilled_bytes(), 0);
        assert!(mgr.has_archive("KTLX", &ts(0)));
    }

    /// A key here with nothing on the medium is this type disagreeing with the
    /// disk, and it is counted rather than silently degraded.
    #[test]
    fn a_key_the_medium_has_lost_is_counted_as_a_miss() {
        let probe = SpillProbe::default();
        let (mut mgr, held) = over_ceiling(Some(probe.clone()), false);
        mgr.evict_archives_to_ceiling(held - 1, oldest_first, |_, _, _| false);
        assert_eq!(probe.len(), 1, "the fixture did not arise");

        // Something else took the file.
        probe.0.lock().expect("not poisoned").clear();

        assert!(
            mgr.archive_for("KTLX", &ts(0)).is_none(),
            "bytes were invented for a key the medium has lost",
        );
        let (_, _, _, restored, misses) = mgr.spill_counts();
        assert_eq!(
            (restored, misses),
            (0, 1),
            "a lost file read as a successful restore",
        );
    }
}
